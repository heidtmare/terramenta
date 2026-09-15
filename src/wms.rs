//! OGC Web Map Service client.
//!
//! WMS ([the OGC standard](https://www.ogc.org/standards/wms/)) answers
//! `GetMap` requests for an arbitrary bounding box, which means the server will
//! happily render whatever rectangle we ask for. To turn that into something a
//! globe can stream, this module pins requests to a fixed quadtree of tiles —
//! see [`crate::tiles`] — and asks for one square image per tile.
//!
//! The fetching itself is handed to Bevy: the module registers a `wms://` asset
//! source whose reader rewrites a tile path such as `0/4/9/3.jpg` into a full
//! `GetMap` URL and delegates to Bevy's HTTP reader. That buys asynchronous
//! loading, image decoding, GPU upload and reference counting on both native
//! and web, and it means a tile is loaded with a plain `asset_server.load`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use bevy::asset::io::web::WebAssetReader;
use bevy::asset::io::{AssetReader, AssetReaderError, AssetSourceBuilder, Reader};
use bevy::asset::{AssetApp, io::PathStream};
use bevy::prelude::*;

use crate::geo::GeoBounds;
use crate::tiles::TileId;

/// The asset source scheme that [`WmsAssetReader`] is registered under.
pub const WMS_SOURCE: &str = "wms";

/// Which revision of the specification to speak.
///
/// The two differ in more than the version number: 1.3.0 renamed `SRS` to `CRS`
/// and — the part that silently produces a world turned on its side — declared
/// that `EPSG:4326` is latitude-first, where 1.1.1 had treated it as
/// longitude-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WmsVersion {
    /// Still what a good number of deployed servers speak.
    #[allow(
        dead_code,
        reason = "selectable in a WmsConfig; exercised only by tests"
    )]
    V1_1_1,
    V1_3_0,
}

impl WmsVersion {
    fn as_str(self) -> &'static str {
        match self {
            Self::V1_1_1 => "1.1.1",
            Self::V1_3_0 => "1.3.0",
        }
    }

    /// The name of the coordinate-system parameter for this version.
    fn crs_parameter(self) -> &'static str {
        match self {
            Self::V1_1_1 => "SRS",
            Self::V1_3_0 => "CRS",
        }
    }

    /// Formats a bounding box in the axis order this version mandates.
    fn format_bbox(self, bounds: GeoBounds) -> String {
        let GeoBounds {
            lat_min,
            lat_max,
            lon_min,
            lon_max,
        } = bounds;
        match self {
            Self::V1_1_1 => format!("{lon_min},{lat_min},{lon_max},{lat_max}"),
            Self::V1_3_0 => format!("{lat_min},{lon_min},{lat_max},{lon_max}"),
        }
    }
}

/// The image encoding to request. Anything else can be reached through
/// [`WmsConfig::extra`], but these two cover what a globe wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WmsFormat {
    Jpeg,
    /// What a transparent overlay layer needs.
    #[expect(dead_code, reason = "selectable in a WmsConfig; no preset uses it")]
    Png,
}

impl WmsFormat {
    fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    /// The extension the tile path ends in. Bevy picks an image loader by
    /// extension, so this is what decides whether the bytes reach the JPEG or
    /// the PNG decoder.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }
}

/// Everything needed to address one WMS layer.
#[derive(Debug, Clone)]
pub struct WmsConfig {
    /// A short name for the layer, shown in the readout.
    pub label: String,
    /// The `GetMap` endpoint, with scheme, and without a query string.
    pub endpoint: String,
    /// The `LAYERS` parameter — comma-separated if you want several drawn together.
    pub layers: String,
    pub styles: String,
    pub version: WmsVersion,
    pub format: WmsFormat,
    pub transparent: bool,
    /// Edge length in pixels of each requested tile.
    pub tile_size: u32,
    /// How deep the quadtree may go. Level `n` splits the globe into
    /// `2^(n+1) x 2^n` tiles, so 8 is about 150 m per pixel at the equator.
    pub max_level: u8,
    /// Any further parameters, such as `TIME` for a dated layer.
    pub extra: Vec<(String, String)>,
}

impl WmsConfig {
    /// NASA's Global Imagery Browse Services, which serves a large catalogue of
    /// Earth-observation layers over WMS without requiring a key, and sends
    /// `Access-Control-Allow-Origin: *` so the browser build can reach it too.
    pub fn gibs(label: &str, layers: &str) -> Self {
        Self {
            label: label.to_string(),
            endpoint: "https://gibs.earthdata.nasa.gov/wms/epsg4326/best/wms.cgi".to_string(),
            layers: layers.to_string(),
            styles: String::new(),
            version: WmsVersion::V1_3_0,
            format: WmsFormat::Jpeg,
            transparent: false,
            tile_size: 256,
            max_level: 8,
            extra: Vec::new(),
        }
    }

    /// Adds a parameter, replacing any existing one with the same name.
    pub fn with_parameter(mut self, name: &str, value: &str) -> Self {
        let name = name.to_ascii_uppercase();
        self.extra.retain(|(existing, _)| *existing != name);
        self.extra.push((name, value.to_string()));
        self
    }

    pub fn with_max_level(mut self, max_level: u8) -> Self {
        self.max_level = max_level;
        self
    }

    /// Builds the `GetMap` URL for one tile.
    pub fn get_map_url(&self, bounds: GeoBounds) -> String {
        let separator = if self.endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        let mut url = format!("{}{separator}", self.endpoint);

        let mut parameters: Vec<(&str, String)> = vec![
            ("SERVICE", "WMS".to_string()),
            ("VERSION", self.version.as_str().to_string()),
            ("REQUEST", "GetMap".to_string()),
            ("LAYERS", self.layers.clone()),
            ("STYLES", self.styles.clone()),
            (self.version.crs_parameter(), "EPSG:4326".to_string()),
            ("BBOX", self.version.format_bbox(bounds)),
            ("WIDTH", self.tile_size.to_string()),
            ("HEIGHT", self.tile_size.to_string()),
            ("FORMAT", self.format.mime().to_string()),
            (
                "TRANSPARENT",
                if self.transparent { "TRUE" } else { "FALSE" }.to_string(),
            ),
        ];
        parameters.extend(
            self.extra
                .iter()
                .map(|(name, value)| (name.as_str(), value.clone())),
        );

        for (index, (name, value)) in parameters.iter().enumerate() {
            if index > 0 {
                url.push('&');
            }
            url.push_str(name);
            url.push('=');
            url.push_str(&percent_encode(value));
        }
        url
    }
}

/// Percent-encodes a parameter value.
///
/// Beyond correctness this keeps slashes and colons — `image/jpeg`,
/// `EPSG:4326` — out of the URL, which matters because the URL travels to
/// Bevy's HTTP reader as a [`Path`] and would otherwise pick up path
/// normalization on the way.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The live configuration, shared between the ECS resource and the asset
/// reader, which lives outside the `World`.
pub type SharedWmsConfig = Arc<RwLock<WmsConfig>>;

/// The currently selected layer.
#[derive(Resource)]
pub struct WmsSettings {
    shared: SharedWmsConfig,
    /// Bumped whenever the layer changes. It is part of every tile path, so a
    /// change cannot be answered from Bevy's cache of the previous layer.
    generation: u32,
    /// Layers the user can cycle through.
    pub presets: Vec<WmsConfig>,
    pub preset_index: usize,
    pub enabled: bool,
}

impl WmsSettings {
    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn label(&self) -> String {
        self.shared
            .read()
            .expect("WMS config lock poisoned")
            .label
            .clone()
    }

    pub fn max_level(&self) -> u8 {
        self.shared
            .read()
            .expect("WMS config lock poisoned")
            .max_level
    }

    pub fn tile_extension(&self) -> &'static str {
        self.shared
            .read()
            .expect("WMS config lock poisoned")
            .format
            .extension()
    }

    /// Switches to the next preset layer and invalidates every loaded tile.
    pub fn cycle_preset(&mut self) {
        if self.presets.is_empty() {
            return;
        }
        self.preset_index = (self.preset_index + 1) % self.presets.len();
        let next = self.presets[self.preset_index].clone();
        *self.shared.write().expect("WMS config lock poisoned") = next;
        self.generation = self.generation.wrapping_add(1);
    }
}

pub struct WmsPlugin {
    pub presets: Vec<WmsConfig>,
    /// Whether tiles are streamed at startup.
    pub enabled: bool,
}

impl Plugin for WmsPlugin {
    fn build(&self, app: &mut App) {
        let initial = self
            .presets
            .first()
            .cloned()
            .unwrap_or_else(|| WmsConfig::gibs("none", ""));
        let shared: SharedWmsConfig = Arc::new(RwLock::new(initial));

        // The reader outlives any one `World`, so it holds the configuration
        // through the same handle the resource writes to.
        let reader_config = shared.clone();
        app.register_asset_source(
            WMS_SOURCE,
            AssetSourceBuilder::new(move || Box::new(WmsAssetReader::new(reader_config.clone()))),
        );

        app.insert_resource(WmsSettings {
            shared,
            generation: 0,
            presets: self.presets.clone(),
            preset_index: 0,
            enabled: self.enabled,
        });
    }
}

/// Serves tile paths of the form `{generation}/{level}/{x}/{y}.{ext}` by
/// turning them into `GetMap` requests.
pub struct WmsAssetReader {
    config: SharedWmsConfig,
    http: WebAssetReader,
    https: WebAssetReader,
}

impl WmsAssetReader {
    fn new(config: SharedWmsConfig) -> Self {
        Self {
            config,
            http: WebAssetReader::Http,
            https: WebAssetReader::Https,
        }
    }
}

/// Recovers the tile a path refers to. The leading generation segment only
/// exists to keep cache entries apart between layers, so it is discarded.
fn parse_tile_path(path: &Path) -> Option<TileId> {
    let text = path.to_str()?;
    let mut segments = text.split('/');
    let _generation = segments.next()?;
    let level: u8 = segments.next()?.parse().ok()?;
    let x: u32 = segments.next()?.parse().ok()?;
    let y: u32 = segments.next()?.split('.').next()?.parse().ok()?;
    Some(TileId { level, x, y })
}

impl AssetReader for WmsAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let Some(tile) = parse_tile_path(path) else {
            return Err(AssetReaderError::NotFound(path.to_path_buf()));
        };

        // Build the URL and release the lock before awaiting, so the guard is
        // never held across a suspension point.
        let url = {
            let config = self
                .config
                .read()
                .map_err(|_| AssetReaderError::NotFound(path.to_path_buf()))?;
            config.get_map_url(tile.bounds())
        };

        // Bevy's reader prepends the scheme itself, so hand it the remainder.
        let (reader, remainder) = match url.split_once("://") {
            Some(("https", rest)) => (&self.https, rest),
            Some(("http", rest)) => (&self.http, rest),
            _ => return Err(AssetReaderError::NotFound(PathBuf::from(url))),
        };

        reader.read(Path::new(remainder)).await
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        // A WMS endpoint has no companion metadata; reporting that plainly is
        // cheaper than letting a request go out and 404.
        Err::<Box<dyn Reader>, _>(AssetReaderError::NotFound(path.to_path_buf()))
    }

    async fn is_directory<'a>(&'a self, _path: &'a Path) -> Result<bool, AssetReaderError> {
        Ok(false)
    }

    async fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        Err(AssetReaderError::NotFound(path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_1_3_0_orders_the_bounding_box_latitude_first() {
        let bounds = GeoBounds {
            lat_min: -90.0,
            lat_max: 0.0,
            lon_min: -180.0,
            lon_max: -90.0,
        };
        assert_eq!(WmsVersion::V1_3_0.format_bbox(bounds), "-90,-180,0,-90");
        assert_eq!(WmsVersion::V1_1_1.format_bbox(bounds), "-180,-90,-90,0");
    }

    #[test]
    fn get_map_url_carries_the_required_parameters() {
        let config = WmsConfig::gibs("Blue Marble", "BlueMarble_ShadedRelief_Bathymetry");
        let url = config.get_map_url(GeoBounds::WORLD);
        assert!(url.contains("SERVICE=WMS"));
        assert!(url.contains("REQUEST=GetMap"));
        assert!(url.contains("VERSION=1.3.0"));
        assert!(url.contains("CRS=EPSG%3A4326"));
        assert!(url.contains("FORMAT=image%2Fjpeg"));
        assert!(url.contains("WIDTH=256"));
    }

    #[test]
    fn tile_paths_round_trip() {
        let tile = TileId {
            level: 4,
            x: 9,
            y: 3,
        };
        assert_eq!(parse_tile_path(Path::new("7/4/9/3.jpg")), Some(tile));
        assert_eq!(parse_tile_path(Path::new("not-a-tile")), None);
    }
}
