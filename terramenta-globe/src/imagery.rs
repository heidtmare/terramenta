//! The imagery layer a tile is fetched from, and the asset source that fetches it.
//!
//! Two protocols are supported and they differ in where the tiling comes from.
//! WMS ([`crate::wms`]) renders an arbitrary bounding box on demand, so the
//! client chooses the grid; WMTS ([`crate::wmts`]) serves a fixed pyramid that
//! the server publishes, so the client has to adopt the server's grid. Both
//! end up addressing one square image per [`TileId`], which is all
//! [`crate::tiles`] needs to stream either of them.
//!
//! The fetching itself is handed to Bevy: this module registers an `imagery://`
//! asset source whose reader rewrites a tile path such as `0/4/9/3.jpg` into
//! whatever URL the active layer wants, and delegates to Bevy's HTTP reader.
//! That buys asynchronous loading, image decoding, GPU upload and reference
//! counting on both native and web, and it means a tile is loaded with a plain
//! `asset_server.load`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use bevy::asset::io::web::WebAssetReader;
use bevy::asset::io::{AssetReader, AssetReaderError, AssetSourceBuilder, Reader};
use bevy::asset::{AssetApp, io::PathStream};
use bevy::prelude::*;

use crate::tiles::{TileGrid, TileId};
use crate::wms::WmsConfig;
use crate::wmts::WmtsConfig;

/// The asset source scheme that [`ImageryAssetReader`] is registered under.
pub const IMAGERY_SOURCE: &str = "imagery";

/// The image encoding to request.
///
/// Which one a layer can serve is the server's business — a WMS lists its
/// formats in `GetCapabilities`, a WMTS names one per layer — but the choice
/// matters twice over here: JPEG is what a full-globe base layer wants, and
/// PNG is the only one of the two that can carry the transparency an overlay
/// or a palette-coded science layer needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
}

impl ImageFormat {
    pub fn mime(self) -> &'static str {
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

/// One configured source of imagery, whichever protocol it speaks.
#[derive(Debug, Clone)]
pub enum ImageryLayer {
    Wms(WmsConfig),
    Wmts(WmtsConfig),
}

impl ImageryLayer {
    /// Shown in the readout, so the two protocols can be told apart at a glance
    /// when the same layer is offered over both.
    pub fn protocol(&self) -> &'static str {
        match self {
            Self::Wms(_) => "WMS",
            Self::Wmts(_) => "WMTS",
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Wms(config) => &config.label,
            Self::Wmts(config) => &config.label,
        }
    }

    /// The tiling the quadtree walk has to follow for this layer.
    pub fn grid(&self) -> TileGrid {
        match self {
            Self::Wms(_) => TileGrid::GEODETIC,
            Self::Wmts(config) => config.grid,
        }
    }

    pub fn max_level(&self) -> u8 {
        match self {
            Self::Wms(config) => config.max_level,
            Self::Wmts(config) => config.max_level,
        }
    }

    pub fn tile_size(&self) -> u32 {
        match self {
            Self::Wms(config) => config.tile_size,
            Self::Wmts(config) => config.tile_size,
        }
    }

    pub fn format(&self) -> ImageFormat {
        match self {
            Self::Wms(config) => config.format,
            Self::Wmts(config) => config.format,
        }
    }

    /// The URL one tile's image is fetched from.
    fn tile_url(&self, tile: TileId) -> String {
        match self {
            Self::Wms(config) => config.get_map_url(TileGrid::GEODETIC.bounds(tile)),
            Self::Wmts(config) => config.get_tile_url(tile),
        }
    }
}

impl From<WmsConfig> for ImageryLayer {
    fn from(config: WmsConfig) -> Self {
        Self::Wms(config)
    }
}

impl From<WmtsConfig> for ImageryLayer {
    fn from(config: WmtsConfig) -> Self {
        Self::Wmts(config)
    }
}

/// Percent-encodes a parameter value.
///
/// Beyond correctness this keeps slashes and colons — `image/jpeg`,
/// `EPSG:4326` — out of the query string, which matters because the URL travels
/// to Bevy's HTTP reader as a [`Path`] and would otherwise pick up path
/// normalization on the way.
pub fn percent_encode(value: &str) -> String {
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

/// The live layer, shared between the ECS resource and the asset reader, which
/// lives outside the `World`.
pub type SharedImageryLayer = Arc<RwLock<ImageryLayer>>;

/// The currently selected layer.
#[derive(Resource)]
pub struct ImagerySettings {
    shared: SharedImageryLayer,
    /// Bumped whenever the layer changes. It is part of every tile path, so a
    /// change cannot be answered from Bevy's cache of the previous layer.
    generation: u32,
    /// Layers the user can cycle through.
    pub presets: Vec<ImageryLayer>,
    pub preset_index: usize,
    pub enabled: bool,
}

impl ImagerySettings {
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Reads one value out of the active layer.
    fn with<T>(&self, read: impl FnOnce(&ImageryLayer) -> T) -> T {
        read(&self.shared.read().expect("imagery layer lock poisoned"))
    }

    /// The protocol and name of the active layer, as the readout shows it.
    pub fn label(&self) -> String {
        self.with(|layer| format!("{} · {}", layer.protocol(), layer.label()))
    }

    /// The protocol the active layer speaks.
    pub fn protocol(&self) -> &'static str {
        self.with(ImageryLayer::protocol)
    }

    pub fn grid(&self) -> TileGrid {
        self.with(ImageryLayer::grid)
    }

    pub fn max_level(&self) -> u8 {
        self.with(ImageryLayer::max_level)
    }

    pub fn tile_size(&self) -> u32 {
        self.with(ImageryLayer::tile_size)
    }

    pub fn tile_extension(&self) -> &'static str {
        self.with(|layer| layer.format().extension())
    }

    /// Switches to the next preset layer and invalidates every loaded tile.
    pub fn cycle_preset(&mut self) {
        self.select_preset(self.preset_index + 1);
    }

    /// Switches to the previous one. Worth having: the presets span two
    /// protocols, so stepping back is how the same scene is compared across
    /// them without cycling the whole list.
    pub fn cycle_preset_back(&mut self) {
        self.select_preset(self.preset_index + self.presets.len().saturating_sub(1));
    }

    /// Switches to a preset by index, wrapping past the end of the list.
    pub fn select_preset(&mut self, index: usize) {
        if self.presets.is_empty() {
            return;
        }
        self.preset_index = index % self.presets.len();
        let next = self.presets[self.preset_index].clone();
        *self.shared.write().expect("imagery layer lock poisoned") = next;
        self.generation = self.generation.wrapping_add(1);
    }
}

pub struct ImageryPlugin {
    pub presets: Vec<ImageryLayer>,
    /// Whether tiles are streamed at startup.
    pub enabled: bool,
}

impl Plugin for ImageryPlugin {
    fn build(&self, app: &mut App) {
        let initial = self
            .presets
            .first()
            .cloned()
            .unwrap_or_else(|| ImageryLayer::Wms(WmsConfig::gibs("none", "")));
        let shared: SharedImageryLayer = Arc::new(RwLock::new(initial));

        // The reader outlives any one `World`, so it holds the layer through
        // the same handle the resource writes to.
        let reader_layer = shared.clone();
        app.register_asset_source(
            IMAGERY_SOURCE,
            AssetSourceBuilder::new(move || {
                Box::new(ImageryAssetReader::new(reader_layer.clone()))
            }),
        );

        app.insert_resource(ImagerySettings {
            shared,
            generation: 0,
            presets: self.presets.clone(),
            preset_index: 0,
            enabled: self.enabled,
        });
    }
}

/// Serves tile paths of the form `{generation}/{level}/{x}/{y}.{ext}` by
/// turning them into whatever request the active layer's protocol wants.
pub struct ImageryAssetReader {
    layer: SharedImageryLayer,
    http: WebAssetReader,
    https: WebAssetReader,
}

impl ImageryAssetReader {
    fn new(layer: SharedImageryLayer) -> Self {
        Self {
            layer,
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

impl AssetReader for ImageryAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let Some(tile) = parse_tile_path(path) else {
            return Err(AssetReaderError::NotFound(path.to_path_buf()));
        };

        // Build the URL and release the lock before awaiting, so the guard is
        // never held across a suspension point.
        let url = {
            let layer = self
                .layer
                .read()
                .map_err(|_| AssetReaderError::NotFound(path.to_path_buf()))?;
            layer.tile_url(tile)
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
        // An imagery endpoint has no companion metadata; reporting that plainly
        // is cheaper than letting a request go out and 404.
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
    fn tile_paths_round_trip() {
        let tile = TileId {
            level: 4,
            x: 9,
            y: 3,
        };
        assert_eq!(parse_tile_path(Path::new("7/4/9/3.jpg")), Some(tile));
        assert_eq!(parse_tile_path(Path::new("not-a-tile")), None);
    }

    #[test]
    fn percent_encoding_escapes_what_a_path_would_otherwise_eat() {
        assert_eq!(percent_encode("image/jpeg"), "image%2Fjpeg");
        assert_eq!(percent_encode("EPSG:4326"), "EPSG%3A4326");
        assert_eq!(percent_encode("2026-09-10"), "2026-09-10");
    }
}
