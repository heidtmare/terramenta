//! OGC Web Map Tile Service client.
//!
//! WMTS ([the OGC standard](https://www.ogc.org/standards/wmts/)) is the other
//! half of the pair [`crate::wms`] starts. Where WMS renders whatever
//! rectangle it is asked for, WMTS only hands back tiles it has already cut,
//! addressed by a **tile matrix set** the server publishes: a fixed pyramid
//! of levels, each a grid of same-sized images. The server can cache every
//! tile, so responses are quick and identical for everybody, but the client
//! has to adopt the server's grid rather than choose its own, which is why a
//! [`WmtsConfig`] carries a [`TileGrid`] where a `WmsConfig` does not.
//!
//! That grid is not always the tidy one a globe would choose. NASA GIBS, the
//! service wired up here, publishes `EPSG:4326` matrix sets whose level 0 is
//! two 288°-wide tiles rather than two 180° ones, so the coarse levels hang
//! off the edge of the world and the matrix widths run 2, 3, 5, 10, 20 rather
//! than doubling. [`TileGrid`] is built to describe exactly that, and
//! [`TileGrid::clipped_bounds`] keeps the overhang from being drawn.
//!
//! Requests come in two encodings, both supported because deployed servers
//! are split between them: `RESTful`, where the capabilities document gives
//! a URL template to substitute into, and `KVP`, a `GetTile` query string in
//! the style of WMS.

use crate::geo::LatLon;
use crate::imagery::{ImageFormat, percent_encode};
use crate::tiles::{TileGrid, TileId};

/// The only version of the specification in the field.
const WMTS_VERSION: &str = "1.0.0";

/// How to phrase a `GetTile` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WmtsEncoding {
    /// RESTful: a `ResourceURL` template lifted straight out of the server's
    /// capabilities document, with `{TileMatrixSet}`, `{TileMatrix}`,
    /// `{TileRow}` and `{TileCol}` substituted per tile — plus `{Layer}`,
    /// `{Style}` and any dimension such as `{Time}`.
    Rest { template: String },
    /// Key-value pairs against a `GetTile` endpoint, for servers that do not
    /// publish a template.
    Kvp { endpoint: String },
}

/// Everything needed to address one WMTS layer.
#[derive(Debug, Clone)]
pub struct WmtsConfig {
    /// A short name for the layer, shown in the readout.
    pub label: String,
    /// The `Layer` identifier, as the capabilities document spells it.
    pub layer: String,
    /// The `Style` identifier. `default` is what almost every server calls its
    /// only style, and an empty string is not the same thing to a REST path.
    pub style: String,
    /// The `TileMatrixSet` identifier — the name of the pyramid below.
    pub tile_matrix_set: String,
    /// The tiling that matrix set defines, which the quadtree walk follows.
    pub grid: TileGrid,
    pub encoding: WmtsEncoding,
    pub format: ImageFormat,
    /// Edge length in pixels of one tile. Unlike WMS this is not a request
    /// parameter — it is a property of the matrix set, and getting it wrong
    /// only misinforms the level-of-detail decision.
    pub tile_size: u32,
    /// The deepest level the matrix set defines. Asking past it is an error
    /// from the server rather than a blurry tile, so this is a hard ceiling
    /// rather than the taste-based one a WMS layer has.
    pub max_level: u8,
    /// Dimension values, such as `("Time", "2026-09-10")`. Named, because a
    /// REST template refers to them by name and a KVP request sends them as
    /// parameters.
    pub dimensions: Vec<(String, String)>,
}

/// The `EPSG:4326` tile matrix sets NASA GIBS publishes, named for the ground
/// resolution of their deepest level, with the number of levels each defines.
///
/// A layer is served from exactly one of these, so the choice is the server's
/// and is read off the capabilities document rather than picked.
const GIBS_MATRIX_SETS: [(&str, u8); 7] = [
    ("16km", 3),
    ("2km", 6),
    ("1km", 7),
    ("500m", 8),
    ("250m", 9),
    ("31.25m", 12),
    ("15.625m", 13),
];

/// The scale denominator of level 0 of every GIBS `EPSG:4326` matrix set. They
/// share a top level and differ only in how far down they go.
const GIBS_LEVEL0_SCALE_DENOMINATOR: f64 = 223_632_905.611_487_1;

/// GIBS cuts its `EPSG:4326` imagery into 512-pixel tiles.
const GIBS_TILE_SIZE: u32 = 512;

impl WmtsConfig {
    /// A layer from NASA's Global Imagery Browse Services, which serves a large
    /// catalogue of Earth-observation layers over WMTS without requiring a key,
    /// and sends `Access-Control-Allow-Origin: *` so the browser build can
    /// reach it too.
    ///
    /// `tile_matrix_set` is the one the layer is published in — GIBS links each
    /// layer to a single set, so it is not a free choice. An unrecognised name
    /// is assumed to be eight levels deep, which [`WmtsConfig::with_max_level`]
    /// can correct.
    pub fn gibs(label: &str, layer: &str, tile_matrix_set: &str, format: ImageFormat) -> Self {
        let levels = GIBS_MATRIX_SETS
            .iter()
            .find(|(name, _)| *name == tile_matrix_set)
            .map_or(8, |(_, levels)| *levels);

        Self {
            label: label.to_string(),
            layer: layer.to_string(),
            style: "default".to_string(),
            tile_matrix_set: tile_matrix_set.to_string(),
            grid: TileGrid::from_scale_denominator(
                LatLon::new(90.0, -180.0),
                GIBS_LEVEL0_SCALE_DENOMINATOR,
                GIBS_TILE_SIZE,
            ),
            encoding: WmtsEncoding::Rest {
                template: format!(
                    "https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/{layer}/{{Style}}/{{Time}}/{{TileMatrixSet}}/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.{}",
                    // GIBS spells the JPEG extension out in full, where Bevy
                    // wants the short form on the asset path; only the URL
                    // uses this one.
                    match format {
                        ImageFormat::Jpeg => "jpeg",
                        ImageFormat::Png => "png",
                    }
                ),
            },
            format,
            tile_size: GIBS_TILE_SIZE,
            max_level: levels.saturating_sub(1),
            dimensions: Vec::new(),
        }
    }

    /// Sends this layer's requests as a `GetTile` query string instead of a
    /// REST path, against the given endpoint.
    pub fn with_kvp(mut self, endpoint: &str) -> Self {
        self.encoding = WmtsEncoding::Kvp {
            endpoint: endpoint.to_string(),
        };
        self
    }

    /// Sets a dimension value, replacing any existing one of the same name.
    pub fn with_dimension(mut self, name: &str, value: &str) -> Self {
        self.dimensions
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        self.dimensions.push((name.to_string(), value.to_string()));
        self
    }

    /// Lowers the ceiling on refinement. Raising it past what the matrix set
    /// defines only earns errors from the server.
    #[allow(
        dead_code,
        reason = "how an unrecognised matrix set is corrected; no preset needs it"
    )]
    pub fn with_max_level(mut self, max_level: u8) -> Self {
        self.max_level = max_level;
        self
    }

    /// The value a template placeholder or a KVP parameter stands for.
    ///
    /// The four tile parameters and the layer and style are matched by their
    /// standard names; anything else is looked for among the dimensions.
    fn resolve(&self, name: &str, tile: TileId) -> Option<String> {
        let value = match name.to_ascii_lowercase().as_str() {
            "tilematrixset" => self.tile_matrix_set.clone(),
            "tilematrix" => tile.level.to_string(),
            "tilerow" => tile.y.to_string(),
            "tilecol" => tile.x.to_string(),
            "layer" => self.layer.clone(),
            "style" => self.style.clone(),
            _ => self
                .dimensions
                .iter()
                .find(|(dimension, _)| dimension.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())?,
        };
        Some(value)
    }

    /// Builds the `GetTile` URL for one tile.
    pub fn get_tile_url(&self, tile: TileId) -> String {
        match &self.encoding {
            WmtsEncoding::Rest { template } => self.fill_template(template, tile),
            WmtsEncoding::Kvp { endpoint } => self.get_tile_query(endpoint, tile),
        }
    }

    /// Substitutes `{Placeholder}` occurrences in a `ResourceURL` template.
    ///
    /// A placeholder the configuration says nothing about becomes `default`,
    /// which is what a template asks for when a dimension is left unspecified —
    /// and is what makes a GIBS template with `{Time}` in it usable for a layer
    /// that has no time dimension, rather than a second template being needed.
    fn fill_template(&self, template: &str, tile: TileId) -> String {
        let mut url = String::with_capacity(template.len());
        let mut rest = template;

        while let Some(open) = rest.find('{') {
            url.push_str(&rest[..open]);
            let Some(close) = rest[open..].find('}').map(|offset| open + offset) else {
                // An unterminated brace is not a placeholder; leave it be.
                break;
            };
            let name = &rest[open + 1..close];
            let value = self.resolve(name, tile).unwrap_or_else(|| "default".into());
            url.push_str(&percent_encode(&value));
            rest = &rest[close + 1..];
        }
        url.push_str(rest);
        url
    }

    /// Builds the equivalent KVP query string.
    fn get_tile_query(&self, endpoint: &str, tile: TileId) -> String {
        let separator = if endpoint.contains('?') { '&' } else { '?' };
        let mut url = format!("{endpoint}{separator}");

        let mut parameters: Vec<(String, String)> = vec![
            ("SERVICE".into(), "WMTS".into()),
            ("VERSION".into(), WMTS_VERSION.into()),
            ("REQUEST".into(), "GetTile".into()),
            ("LAYER".into(), self.layer.clone()),
            ("STYLE".into(), self.style.clone()),
            ("FORMAT".into(), self.format.mime().into()),
            ("TILEMATRIXSET".into(), self.tile_matrix_set.clone()),
            ("TILEMATRIX".into(), tile.level.to_string()),
            ("TILEROW".into(), tile.y.to_string()),
            ("TILECOL".into(), tile.x.to_string()),
        ];
        parameters.extend(
            self.dimensions
                .iter()
                .map(|(name, value)| (name.to_ascii_uppercase(), value.clone())),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(level: u8, x: u32, y: u32) -> TileId {
        TileId { level, x, y }
    }

    #[test]
    fn gibs_matrix_sets_share_a_level_0_tile_span_of_288_degrees() {
        // The number that makes the GIBS grid unlike a plain geodetic quadtree:
        // a level-0 tile is 288° on a side, so it hangs off the world.
        let config = WmtsConfig::gibs(
            "Blue Marble",
            "BlueMarble_ShadedRelief_Bathymetry",
            "500m",
            ImageFormat::Jpeg,
        );
        assert!((config.grid.level0_span - 288.0).abs() < 1.0e-3);
        assert_eq!(config.max_level, 7);
    }

    #[test]
    fn rest_templates_substitute_every_tile_parameter() {
        let config = WmtsConfig::gibs(
            "MODIS Terra",
            "MODIS_Terra_CorrectedReflectance_TrueColor",
            "250m",
            ImageFormat::Jpeg,
        )
        .with_dimension("Time", "2026-09-10");

        assert_eq!(
            config.get_tile_url(tile(3, 4, 2)),
            "https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/\
             MODIS_Terra_CorrectedReflectance_TrueColor/default/2026-09-10/250m/3/2/4.jpeg"
        );
    }

    #[test]
    fn an_unset_dimension_falls_back_to_default() {
        // GIBS publishes one template with `{Time}` in it; a layer without a
        // time dimension has to be able to use it.
        let config = WmtsConfig::gibs(
            "Blue Marble",
            "BlueMarble_ShadedRelief_Bathymetry",
            "500m",
            ImageFormat::Jpeg,
        );
        assert_eq!(
            config.get_tile_url(tile(0, 1, 0)),
            "https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/\
             BlueMarble_ShadedRelief_Bathymetry/default/default/500m/0/0/1.jpeg"
        );
    }

    #[test]
    fn kvp_requests_carry_the_required_parameters() {
        let config = WmtsConfig::gibs(
            "VIIRS city lights",
            "VIIRS_CityLights_2012",
            "500m",
            ImageFormat::Jpeg,
        )
        .with_kvp("https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/wmts.cgi");
        let url = config.get_tile_url(tile(2, 4, 1));

        assert!(url.contains("SERVICE=WMTS"));
        assert!(url.contains("REQUEST=GetTile"));
        assert!(url.contains("VERSION=1.0.0"));
        assert!(url.contains("TILEMATRIXSET=500m"));
        assert!(url.contains("TILEMATRIX=2"));
        // Row is y and column is x; swapping them is the classic WMTS bug and
        // it produces imagery that is merely wrong rather than missing.
        assert!(url.contains("TILEROW=1"));
        assert!(url.contains("TILECOL=4"));
        assert!(url.contains("FORMAT=image%2Fjpeg"));
    }

    #[test]
    fn an_unknown_matrix_set_can_have_its_depth_corrected() {
        // Eight levels deep, so the deepest is level 7.
        let config = WmtsConfig::gibs("x", "y", "not-a-gibs-set", ImageFormat::Jpeg);
        assert_eq!(config.max_level, 7);
        assert_eq!(config.with_max_level(4).max_level, 4);
    }

    #[test]
    fn a_dimension_replaces_rather_than_repeats() {
        let config = WmtsConfig::gibs("x", "y", "250m", ImageFormat::Png)
            .with_dimension("Time", "2026-01-01")
            .with_dimension("TIME", "2026-09-10");
        assert_eq!(config.dimensions.len(), 1);
        assert!(config.get_tile_url(tile(0, 0, 0)).contains("2026-09-10"));
    }
}
