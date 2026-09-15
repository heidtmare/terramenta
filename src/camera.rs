//! A globe-flavoured orbit camera: drag to spin the Earth, scroll to close in.
//!
//! The one thing that separates this from a generic orbit rig is that the
//! rotation rate scales with altitude. From far away a drag sweeps whole
//! continents; a hundred kilometres up the same drag nudges a city block, which
//! is what makes the globe feel navigable instead of twitchy.

use std::f32::consts::FRAC_PI_2;

use bevy::camera::Hdr;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::gestures::PinchGesture;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;

use crate::globe::GLOBE_RADIUS;

/// Closest approach, ~130 km above the surface.
const MIN_DISTANCE: f32 = GLOBE_RADIUS * 1.02;
/// Far enough out that the Earth is a marble.
const MAX_DISTANCE: f32 = GLOBE_RADIUS * 14.0;
const DEFAULT_DISTANCE: f32 = GLOBE_RADIUS * 3.2;
/// Radians of orbit per pixel of drag, before the altitude scaling.
const DRAG_SENSITIVITY: f32 = 0.005;
const KEY_ORBIT_SPEED: f32 = 1.2;
const AUTO_ROTATE_SPEED: f32 = 0.05;
/// Higher converges on the target faster; this is the rate of an exponential decay.
const SMOOTHING: f32 = 14.0;
const PITCH_LIMIT: f32 = FRAC_PI_2 - 0.02;

pub struct OrbitCameraPlugin;

impl Plugin for OrbitCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TouchTracker>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                ((mouse_input, keyboard_input, touch_input), apply_orbit).chain(),
            );
    }
}

/// Orbit state. The `target_*` fields are what input writes to; the plain
/// fields are what the transform is built from, and they chase the targets.
#[derive(Component)]
pub struct OrbitCamera {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target_yaw: f32,
    pub target_pitch: f32,
    pub target_distance: f32,
    pub auto_rotate: bool,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            // Start looking at the Atlantic, tilted slightly north.
            yaw: -0.5,
            pitch: 0.35,
            distance: DEFAULT_DISTANCE,
            target_yaw: -0.5,
            target_pitch: 0.35,
            target_distance: DEFAULT_DISTANCE,
            auto_rotate: false,
        }
    }
}

impl OrbitCamera {
    /// Height above the surface in Earth radii, clamped away from zero so it can
    /// scale input without collapsing at the closest zoom level.
    fn altitude_factor(&self) -> f32 {
        ((self.distance - GLOBE_RADIUS) / GLOBE_RADIUS).clamp(0.015, 1.0)
    }

    fn zoom_by(&mut self, exponent: f32) {
        self.target_distance =
            (self.target_distance * (-exponent).exp()).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    fn orbit_by(&mut self, yaw: f32, pitch: f32) {
        self.target_yaw -= yaw;
        self.target_pitch = (self.target_pitch + pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }
}

/// Remembers the previous two-finger spread so pinches can be turned into zoom.
#[derive(Resource, Default)]
struct TouchTracker {
    previous_spread: Option<f32>,
}

fn spawn_camera(mut commands: Commands) {
    let orbit = OrbitCamera::default();
    commands.spawn((
        Name::new("Orbit Camera"),
        Camera3d::default(),
        // Bloom and tonemapping both want headroom above 1.0.
        Hdr,
        Projection::from(PerspectiveProjection {
            fov: 45.0_f32.to_radians(),
            // The star field sphere has to fit inside the frustum.
            near: 0.001,
            far: 1000.0,
            ..default()
        }),
        Tonemapping::TonyMcMapface,
        // Lets city lights and the atmospheric limb bloom the way they do from orbit.
        Bloom {
            intensity: 0.18,
            ..Bloom::NATURAL
        },
        Transform::from_translation(Vec3::Z * orbit.distance),
        orbit,
    ));
}

fn mouse_input(
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut pinch: MessageReader<PinchGesture>,
    mut camera: Single<&mut OrbitCamera>,
) {
    if buttons.pressed(MouseButton::Left) && motion.delta != Vec2::ZERO {
        let scale = DRAG_SENSITIVITY * camera.altitude_factor();
        let delta = motion.delta * scale;
        camera.orbit_by(delta.x, delta.y);
        camera.auto_rotate = false;
    }

    if scroll.delta.y != 0.0 {
        // Line-based wheels report whole notches; pixel-based trackpads report
        // many small deltas, so they need very different gains.
        let gain = match scroll.unit {
            MouseScrollUnit::Line => 0.15,
            MouseScrollUnit::Pixel => 0.005,
        };
        camera.zoom_by(scroll.delta.y * gain);
    }

    let pinch_amount: f32 = pinch.read().map(|gesture| gesture.0).sum();
    if pinch_amount != 0.0 {
        camera.zoom_by(pinch_amount * 2.0);
    }
}

fn keyboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut camera: Single<&mut OrbitCamera>,
) {
    let mut orbit = Vec2::ZERO;
    if keys.pressed(KeyCode::ArrowLeft) || keys.pressed(KeyCode::KeyA) {
        orbit.x -= 1.0;
    }
    if keys.pressed(KeyCode::ArrowRight) || keys.pressed(KeyCode::KeyD) {
        orbit.x += 1.0;
    }
    if keys.pressed(KeyCode::ArrowUp) || keys.pressed(KeyCode::KeyW) {
        orbit.y += 1.0;
    }
    if keys.pressed(KeyCode::ArrowDown) || keys.pressed(KeyCode::KeyS) {
        orbit.y -= 1.0;
    }

    if orbit != Vec2::ZERO {
        let step = orbit * KEY_ORBIT_SPEED * camera.altitude_factor() * time.delta_secs();
        camera.orbit_by(-step.x, step.y);
        camera.auto_rotate = false;
    }

    let mut zoom = 0.0;
    if keys.pressed(KeyCode::Equal) || keys.pressed(KeyCode::NumpadAdd) {
        zoom += 1.0;
    }
    if keys.pressed(KeyCode::Minus) || keys.pressed(KeyCode::NumpadSubtract) {
        zoom -= 1.0;
    }
    if zoom != 0.0 {
        camera.zoom_by(zoom * time.delta_secs());
    }

    if keys.just_pressed(KeyCode::Space) {
        camera.auto_rotate = !camera.auto_rotate;
    }

    if keys.just_pressed(KeyCode::KeyR) {
        let reset = OrbitCamera::default();
        camera.target_yaw = reset.target_yaw;
        camera.target_pitch = reset.target_pitch;
        camera.target_distance = reset.target_distance;
        camera.auto_rotate = false;
    }
}

fn touch_input(
    touches: Res<Touches>,
    mut tracker: ResMut<TouchTracker>,
    mut camera: Single<&mut OrbitCamera>,
) {
    let active: Vec<_> = touches.iter().collect();

    match active.as_slice() {
        [touch] => {
            tracker.previous_spread = None;
            let delta = touch.delta();
            if delta != Vec2::ZERO {
                let scale = DRAG_SENSITIVITY * camera.altitude_factor();
                let delta = delta * scale;
                camera.orbit_by(delta.x, delta.y);
                camera.auto_rotate = false;
            }
        }
        [first, second] => {
            let spread = first.position().distance(second.position());
            if let Some(previous) = tracker.previous_spread {
                // Treat the relative change in spread as a zoom exponent so a
                // pinch feels the same regardless of how far apart the fingers start.
                if previous > 1.0 {
                    camera.zoom_by((spread / previous).ln());
                }
            }
            tracker.previous_spread = Some(spread);
            camera.auto_rotate = false;
        }
        _ => tracker.previous_spread = None,
    }
}

fn apply_orbit(time: Res<Time>, mut camera: Single<(&mut OrbitCamera, &mut Transform)>) {
    let (orbit, transform) = &mut *camera;

    if orbit.auto_rotate {
        orbit.target_yaw += AUTO_ROTATE_SPEED * time.delta_secs();
    }

    // Frame-rate independent exponential smoothing toward the input targets.
    let t = 1.0 - (-SMOOTHING * time.delta_secs()).exp();
    orbit.yaw += (orbit.target_yaw - orbit.yaw) * t;
    orbit.pitch += (orbit.target_pitch - orbit.pitch) * t;
    orbit.distance += (orbit.target_distance - orbit.distance) * t;

    let rotation = Quat::from_rotation_y(orbit.yaw) * Quat::from_rotation_x(-orbit.pitch);
    transform.rotation = rotation;
    transform.translation = rotation * (Vec3::Z * orbit.distance);
}
