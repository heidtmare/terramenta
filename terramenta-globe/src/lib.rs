//! Terramenta — a navigable 3D globe of Earth, built on Bevy and rendered
//! through WebGPU in the browser or the native backend on the desktop.
//!
//! The crate is the globe and nothing else: it draws the Earth, streams imagery
//! onto it and lets it be flown around, and it exposes all of that through
//! [`api`] so an embedder can put its own interface on top. `terramenta-webapp`
//! in this workspace is the reference one, and [`crate::wasm`] is the binding
//! it reaches the globe through.
//!
//! There are two ways in:
//!
//! * [`run`], which starts the globe and does not return. That is what the
//!   native binary calls and what [`wasm::start`] calls in the browser.
//! * [`app`], which builds the same `App` without running it, for a host that
//!   wants to add plugins of its own first.

pub mod api;
mod camera;
mod ephemeris;
mod features;
mod fetch;
mod frame;
mod geo;
mod geojson;
mod globe;
mod gnc;
mod hud;
mod imagery;
mod moon;
mod mvt;
mod omm;
mod overlays;
mod picking;
mod placemark;
mod simplestyle;
pub mod solar;
mod sun;
mod tessellate;
mod tiles;
mod vector_tiles;
#[cfg(target_arch = "wasm32")]
pub mod wasm;
mod wms;
mod wmts;

use bevy::asset::AssetMetaCheck;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSamplerDescriptor};
use bevy::prelude::*;

use api::ApiPlugin;
use camera::OrbitCameraPlugin;
use ephemeris::{EphemerisPlugin, EphemerisRequest, EphemerisSourcePlugin};
use frame::FramePlugin;
use globe::GlobePlugin;
use gnc::GncPlugin;
use hud::HudPlugin;
use imagery::{ImageFormat, ImageryLayer, ImageryPlugin};
use moon::MoonPlugin;
use overlays::{OverlayPlugin, OverlayRequest, OverlaySourcePlugin};
use placemark::PlacemarkPlugin;
use solar::SolarSystemPlugin;
use sun::SunPlugin;
use tiles::TilePlugin;
use vector_tiles::{VectorTileLayer, VectorTilePlugin, VectorTileSourcePlugin};
use wms::WmsConfig;
use wmts::WmtsConfig;

/// Where the globe is drawn and what it loads from.
#[derive(Debug, Clone)]
pub struct GlobeConfig {
    /// The canvas to draw on, as a CSS selector. The web build attaches to it
    /// and tracks its size; native ignores it, where the window is the whole app.
    pub canvas_selector: String,
    /// Where the shaders and base textures are, relative to the page on the web
    /// and to the crate on the desktop.
    ///
    /// An embedder that keeps the module in a folder of its own has to say so
    /// here: the browser resolves this against the page, not against the module,
    /// so a globe at `globe/terramenta_globe.js` still loads its assets from
    /// `assets/` unless told otherwise.
    pub asset_path: String,
    /// GeoJSON overlays to put up at startup.
    ///
    /// Empty by default, because an overlay is the embedder's data rather than
    /// the globe's: the reference app adds its own through
    /// [`api::GlobeCommand::AddOverlay`] once the module has loaded, which is
    /// also the only route a web embedder has. This is here for a native host
    /// building the `App` itself.
    pub overlays: Vec<OverlayRequest>,
    /// Ephemeris layers to put up at startup, on the same terms: a catalogue of
    /// satellites is the embedder's data, and a web one adds its own through
    /// [`api::GlobeCommand::AddEphemeris`].
    pub ephemerides: Vec<EphemerisRequest>,
}

impl Default for GlobeConfig {
    fn default() -> Self {
        Self {
            canvas_selector: "#terramenta".into(),
            asset_path: "assets".into(),
            overlays: Vec::new(),
            ephemerides: Vec::new(),
        }
    }
}

/// Starts the globe with the default configuration. This does not return: Bevy
/// takes the thread on the desktop and the browser's event loop on the web.
pub fn run() {
    app(GlobeConfig::default()).run();
}

/// Builds the globe's `App` without running it, for a host that wants to add
/// plugins of its own first.
pub fn app(config: GlobeConfig) -> App {
    let mut app = App::new();
    app.insert_resource(ClearColor(Color::BLACK));
    app
        // Both of these register an asset source, and an asset source has to be
        // registered before `AssetPlugin` builds — which is why they go in
        // ahead of `DefaultPlugins` while the rest of each feature does not.
        .add_plugins(ImageryPlugin {
            presets: imagery_layers(),
            enabled: true,
        })
        .add_plugins(OverlaySourcePlugin {
            initial: config.overlays.clone(),
        })
        .add_plugins(EphemerisSourcePlugin {
            initial: config.ephemerides.clone(),
        })
        .add_plugins(VectorTileSourcePlugin {
            presets: vector_tile_layers(),
            enabled: true,
        })
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Terramenta".into(),
                        // On the web the app binds to this canvas and tracks
                        // its size; both are ignored on native.
                        canvas: Some(config.canvas_selector.clone()),
                        fit_canvas_to_parent: true,
                        // Keep the browser from scrolling the page or showing a
                        // context menu when the globe is being dragged.
                        prevent_default_event_handling: true,
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    file_path: config.asset_path.clone(),
                    // Every tile would otherwise be preceded by a request for a
                    // `.meta` file that an imagery endpoint will never have,
                    // doubling the traffic to serve nothing.
                    meta_check: AssetMetaCheck::Never,
                    ..default()
                })
                .set(ImagePlugin {
                    // Equirectangular imagery wraps around the globe, so the
                    // horizontal axis has to repeat rather than clamp — that is
                    // what lets the cloud layer scroll across the antimeridian
                    // without a seam. Tiles opt out of this per image.
                    default_sampler: ImageSamplerDescriptor {
                        address_mode_u: ImageAddressMode::Repeat,
                        mag_filter: ImageFilterMode::Linear,
                        min_filter: ImageFilterMode::Linear,
                        ..default()
                    },
                }),
        )
        .add_plugins((
            ApiPlugin,
            GlobePlugin,
            OrbitCameraPlugin,
            SunPlugin,
            MoonPlugin,
            FramePlugin,
            GncPlugin,
            SolarSystemPlugin,
            TilePlugin,
            VectorTilePlugin,
            OverlayPlugin,
            EphemerisPlugin,
            PlacemarkPlugin,
            HudPlugin,
        ));

    app
}

/// The imagery presets the globe starts with, in the order they cycle.
pub fn layers() -> Vec<api::LayerInfo> {
    api::describe_layers(&imagery_layers())
}

/// The vector tile presets the globe starts with, in the order they cycle.
pub fn vector_layers() -> Vec<api::VectorTileLayerInfo> {
    vector_tiles::describe_layers(&vector_tile_layers())
}

/// The vector tile sources wired up by default.
///
/// Both are keyless, which is the constraint that decides the list: almost
/// every hosted vector tile service wants a token, and a globe that draws
/// nothing until one is pasted in is a globe that looks broken. An embedder
/// with a key of its own builds the `App` itself and passes its own presets.
fn vector_tile_layers() -> Vec<VectorTileLayer> {
    vec![
        // MapLibre's demo tiles: one source layer of country polygons, cut to
        // level 5. Coarse, tiny, and the reason it is first — it draws the
        // world's borders over the imagery from the moment the globe starts.
        VectorTileLayer::new(
            "MapLibre · country boundaries",
            "https://demotiles.maplibre.org/tiles/{z}/{x}/{y}.pbf",
        )
        .with_max_level(5)
        .with_source_layers(["countries"]),
        // OpenStreetMap's own vector tiles, in the Shortbread schema. Deep
        // enough to fly down into, so the layers drawn are restricted to the
        // ones that read as lines on a globe — coastlines, water and the road
        // network — rather than the land use and labels underneath them.
        VectorTileLayer::new(
            "OpenStreetMap · Shortbread",
            "https://vector.openstreetmap.org/shortbread_v1/{z}/{x}/{y}.mvt",
        )
        .with_max_level(12)
        .with_source_layers(["ocean", "water_polygons", "streets", "boundaries"]),
    ]
}

fn imagery_layers() -> Vec<ImageryLayer> {
    vec![
        // WMTS. The tile matrix set is not a free choice — GIBS publishes each
        // layer in exactly one, and it sets how deep the pyramid goes.
        WmtsConfig::gibs(
            "Blue Marble · shaded relief",
            "BlueMarble_ShadedRelief_Bathymetry",
            "500m",
            ImageFormat::Jpeg,
        )
        .into(),
        WmtsConfig::gibs(
            "MODIS Terra · true colour",
            "MODIS_Terra_CorrectedReflectance_TrueColor",
            "250m",
            ImageFormat::Jpeg,
        )
        .with_dimension("Time", "2026-09-10")
        .into(),
        // Served as KVP rather than a REST path, which is the other encoding a
        // WMTS server may offer and the only one some of them do.
        WmtsConfig::gibs(
            "VIIRS · city lights",
            "VIIRS_CityLights_2012",
            "500m",
            ImageFormat::Jpeg,
        )
        .with_kvp("https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/wmts.cgi")
        .into(),
        // A palette-coded science layer, which GIBS serves as PNG.
        WmtsConfig::gibs(
            "MODIS Terra · land surface temperature",
            "MODIS_Terra_Land_Surface_Temp_Day",
            "1km",
            ImageFormat::Png,
        )
        .with_dimension("Time", "2026-09-10")
        .into(),
        // The same catalogue over WMS, where the grid is ours to pick and
        // `max_level` is a matter of taste rather than a hard ceiling.
        WmsConfig::gibs(
            "Blue Marble · shaded relief",
            "BlueMarble_ShadedRelief_Bathymetry",
        )
        .with_max_level(7)
        .into(),
        WmsConfig::gibs(
            "MODIS Terra · true colour",
            "MODIS_Terra_CorrectedReflectance_TrueColor",
        )
        .with_parameter("TIME", "2026-09-10")
        .with_max_level(8)
        .into(),
        WmsConfig::gibs("VIIRS · city lights", "VIIRS_CityLights_2012")
            .with_max_level(7)
            .into(),
        WmsConfig::gibs(
            "MODIS Terra · land surface temperature",
            "MODIS_Terra_Land_Surface_Temp_Day",
        )
        .with_parameter("TIME", "2026-09-10")
        .with_max_level(6)
        .into(),
    ]
}
