//! Terramenta — a navigable 3D globe of Earth, built on Bevy and rendered
//! through WebGPU in the browser or the native backend on the desktop.

mod camera;
mod geo;
mod globe;
mod hud;
mod sun;

use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSamplerDescriptor};
use bevy::prelude::*;

use camera::OrbitCameraPlugin;
use globe::GlobePlugin;
use hud::HudPlugin;
use sun::SunPlugin;

fn main() {
    App::new()
        .insert_resource(ClearColor(Color::BLACK))
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
                .set(ImagePlugin {
                    // Equirectangular imagery wraps around the globe, so the
                    // horizontal axis has to repeat rather than clamp — that is
                    // what lets the cloud layer scroll across the antimeridian
                    // without a seam.
                    default_sampler: ImageSamplerDescriptor {
                        address_mode_u: ImageAddressMode::Repeat,
                        mag_filter: ImageFilterMode::Linear,
                        min_filter: ImageFilterMode::Linear,
                        ..default()
                    },
                }),
        )
        .add_plugins((GlobePlugin, OrbitCameraPlugin, SunPlugin, HudPlugin))
        .run();
}
