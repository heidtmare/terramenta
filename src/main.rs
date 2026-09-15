//! Terramenta — a navigable 3D globe of Earth, built on Bevy and rendered
//! through WebGPU in the browser or the native backend on the desktop.

mod camera;
mod frame;
mod geo;
mod globe;
mod hud;
mod sun;
mod tiles;
mod wms;

use bevy::asset::AssetMetaCheck;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSamplerDescriptor};
use bevy::prelude::*;

use camera::OrbitCameraPlugin;
use frame::FramePlugin;
use globe::GlobePlugin;
use hud::HudPlugin;
use sun::SunPlugin;
use tiles::TilePlugin;
use wms::{WmsConfig, WmsPlugin};

fn main() {
    App::new()
        .insert_resource(ClearColor(Color::BLACK))
        // The WMS asset source has to be registered before `AssetPlugin` builds,
        // which is why this plugin goes in ahead of `DefaultPlugins`.
        .add_plugins(WmsPlugin {
            presets: imagery_layers(),
            enabled: true,
        })
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Terramenta".into(),
                        // On the web the app binds to this canvas and tracks
                        // its size; both are ignored on native.
                        canvas: Some("#terramenta".into()),
                        fit_canvas_to_parent: true,
                        // Keep the browser from scrolling the page or showing a
                        // context menu when the globe is being dragged.
                        prevent_default_event_handling: true,
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    // Every tile would otherwise be preceded by a request for a
                    // `.meta` file that a WMS endpoint will never have, doubling
                    // the traffic to serve nothing.
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
            GlobePlugin,
            OrbitCameraPlugin,
            SunPlugin,
            FramePlugin,
            TilePlugin,
            HudPlugin,
        ))
        .run();
}

/// The WMS layers offered at startup, cycled through with `L`.
///
/// All of these come from NASA's Global Imagery Browse Services, which needs no
/// API key. The dated layers are pinned to a fixed day: GIBS serves the most
/// recent imagery a day or two in arrears, so asking for "today" returns an
/// empty tile.
fn imagery_layers() -> Vec<WmsConfig> {
    vec![
        WmsConfig::gibs(
            "Blue Marble · shaded relief",
            "BlueMarble_ShadedRelief_Bathymetry",
        )
        .with_max_level(7),
        WmsConfig::gibs(
            "MODIS Terra · true colour",
            "MODIS_Terra_CorrectedReflectance_TrueColor",
        )
        .with_parameter("TIME", "2026-09-10")
        .with_max_level(8),
        WmsConfig::gibs("VIIRS · city lights", "VIIRS_CityLights_2012").with_max_level(7),
        WmsConfig::gibs(
            "MODIS Terra · land surface temperature",
            "MODIS_Terra_Land_Surface_Temp_Day",
        )
        .with_parameter("TIME", "2026-09-10")
        .with_max_level(6),
    ]
}
