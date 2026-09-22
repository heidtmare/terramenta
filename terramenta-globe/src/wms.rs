//! OGC Web Map Service client.
//!
//! WMS ([the OGC standard](https://www.ogc.org/standards/wms/)) answers
//! `GetMap` requests for an arbitrary bounding box: the server renders
//! whatever rectangle is requested. Requests are pinned to a fixed quadtree of
//! tiles — see [`crate::tiles`] — with one square image requested per tile.
//!
//! Because the grid is chosen locally, it uses the tidy option:
//! [`TileGrid::GEODETIC`], two 180° tiles at level 0, quartered at each level
//! below. [`crate::wmts`] is the reverse — the server publishes the grid and
//! the client follows it.
//!
//! Fetching is handled by the shared `imagery://` asset source in
//! [`crate::imagery`]; this module only builds URLs.

use crate::geo::GeoBounds;
use crate::imagery::ImageFormat;

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
    pub format: ImageFormat,
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
            format: ImageFormat::Jpeg,
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
            url.push_str(&crate::imagery::percent_encode(value));
        }
        url
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
}
