//! A globe-flavoured orbit camera: drag to spin the Earth, scroll to close in.
//!
//! The camera orbits in world space, fixed relative to whichever reference
//! frame the scene is drawn in: the ground in ECEF, the stars in ECI, with
//! the globe turning underneath it.
//!
//! Rotation rate scales with altitude, unlike a generic orbit rig: from far
//! away a drag sweeps whole continents, while a hundred kilometres up the
//! same drag nudges a city block.
//!
//! Two rig angles sit on top of that orbit and leave the ground alone.
//! `heading` and `tilt` (ctrl + drag) swing the camera around the point it is
//! looking at, from straight down to a grazing view across the horizon,
//! without that point leaving the centre of the screen. `gaze` (shift + drag)
//! rotates the camera in place, swinging the view off the anchor without
//! moving the camera. Panning, the tile walk, and the readout are all written
//! in terms of the anchor; `heading`/`tilt` and `gaze` are the only places
//! the anchor/camera-position distinction matters.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use bevy::camera::Hdr;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::gestures::PinchGesture;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;

use crate::api::keyboard_enabled;
use crate::frame::{FrameRealigned, FrameSet};
use crate::geo::{EARTH_RADIUS_KM, LatLon};
use crate::globe::GLOBE_RADIUS;
use crate::heliocentric::HeliocentricCamera;
use crate::view::{in_globe_view, not_heliocentric_view};

/// Closest approach, ~130 km above the surface.
const MIN_DISTANCE: f32 = GLOBE_RADIUS * 1.02;
/// Far enough out that the Earth is a marble.
const MAX_DISTANCE: f32 = GLOBE_RADIUS * 14.0;
pub(crate) const DEFAULT_DISTANCE: f32 = GLOBE_RADIUS * 3.2;
/// Radians of orbit per pixel of drag, before the altitude scaling.
const DRAG_SENSITIVITY: f32 = 0.005;
/// Radians per pixel for the drags that turn the rig rather than the globe.
/// These are angles of the camera itself, so unlike panning they do not scale
/// with altitude: a quarter turn is a quarter turn from any height.
const LOOK_SENSITIVITY: f32 = 0.005;
const KEY_ORBIT_SPEED: f32 = 1.2;
/// Higher converges on the target faster; this is the rate of an exponential decay.
const SMOOTHING: f32 = 14.0;
const PITCH_LIMIT: f32 = FRAC_PI_2 - 0.02;
/// How far the camera can be laid over toward the horizon. Stopping short of a
/// right angle keeps some ground in frame at the bottom of the screen.
const TILT_LIMIT: f32 = FRAC_PI_2 - 0.09;
/// How far free look can swing off the anchor: a full look to either side, but
/// never behind, so the globe is always a drag back the way you came.
const GAZE_LIMIT: f32 = FRAC_PI_2;

pub struct OrbitCameraPlugin;

impl Plugin for OrbitCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TouchTracker>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (
                    (
                        mouse_input.run_if(in_globe_view),
                        keyboard_input
                            .run_if(keyboard_enabled)
                            .run_if(in_globe_view),
                        touch_input.run_if(in_globe_view),
                    ),
                    // Whatever the frame switch did to the globe has to reach
                    // the camera before the transform is rebuilt from it, and
                    // the finished transform is what the tile walk reads.
                    // `apply_orbit` keeps running through both legs of a
                    // heliocentric transition — see `crate::view` — since
                    // that's what actually performs the pull-back and the
                    // return; it only stops once the heliocentric camera is
                    // the one drawing the screen.
                    (follow_frame, apply_orbit.run_if(not_heliocentric_view))
                        .chain()
                        .in_set(FrameSet::Camera),
                )
                    .chain(),
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
    /// Compass bearing of the camera about the point it is looking at.
    pub heading: f32,
    /// Angle away from straight down, toward the horizon.
    pub tilt: f32,
    /// Free look, left and right of wherever the rig points.
    pub gaze_yaw: f32,
    /// Free look, up and down.
    pub gaze_pitch: f32,
    pub target_yaw: f32,
    pub target_pitch: f32,
    pub target_distance: f32,
    pub target_heading: f32,
    pub target_tilt: f32,
    pub target_gaze_yaw: f32,
    pub target_gaze_pitch: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            // Start looking at the Atlantic, tilted slightly north.
            yaw: -0.5,
            pitch: 0.35,
            distance: DEFAULT_DISTANCE,
            heading: 0.0,
            tilt: 0.0,
            gaze_yaw: 0.0,
            gaze_pitch: 0.0,
            target_yaw: -0.5,
            target_pitch: 0.35,
            target_distance: DEFAULT_DISTANCE,
            target_heading: 0.0,
            target_tilt: 0.0,
            target_gaze_yaw: 0.0,
            target_gaze_pitch: 0.0,
        }
    }
}

impl OrbitCamera {
    /// Height above the surface in Earth radii, clamped away from zero so it can
    /// scale input without collapsing at the closest zoom level.
    fn altitude_factor(&self) -> f32 {
        ((self.distance - GLOBE_RADIUS) / GLOBE_RADIUS).clamp(0.015, 1.0)
    }

    /// Distance from the camera to the point it is looking at, in kilometres,
    /// as it is being drawn rather than where it is heading. Straight down —
    /// which is to say with no tilt on the rig — that is its height above the
    /// surface, and it is the same number [`OrbitCamera::set_altitude_km`]
    /// takes back.
    pub fn altitude_km(&self) -> f32 {
        (self.distance - GLOBE_RADIUS) * EARTH_RADIUS_KM
    }

    /// The altitudes the camera can be flown to, in kilometres.
    pub fn altitude_limits_km() -> (f32, f32) {
        (
            (MIN_DISTANCE - GLOBE_RADIUS) * EARTH_RADIUS_KM,
            (MAX_DISTANCE - GLOBE_RADIUS) * EARTH_RADIUS_KM,
        )
    }

    /// Flies to a given height above the surface.
    pub fn set_altitude_km(&mut self, altitude_km: f32) {
        self.target_distance =
            (GLOBE_RADIUS + altitude_km / EARTH_RADIUS_KM).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// The coordinate the camera is looking at, in world space.
    ///
    /// Yaw and pitch are the longitude and latitude of the anchor the rig
    /// hangs from, which falls out of how the orbit transform is built — so
    /// this and [`OrbitCamera::look_at`] are a pair of conversions rather than
    /// a search. Tilting swings the camera off this point but keeps looking at
    /// it; free look is the one thing that points the view somewhere else.
    pub fn world_center(&self) -> LatLon {
        LatLon::new(self.pitch.to_degrees(), self.yaw.to_degrees())
    }

    /// Swings around to centre a world-space direction, keeping whatever
    /// heading and tilt the rig is carrying. Free look is released, because a
    /// view still turned away from the anchor would not show what was asked for.
    pub fn look_at(&mut self, direction: Vec3) {
        let target = LatLon::from_direction(direction);
        self.target_pitch = target.lat.to_radians().clamp(-PITCH_LIMIT, PITCH_LIMIT);
        // Yaw is smoothed toward rather than snapped to, so take the short way
        // round: the nearest revolution, not three turns about the pole.
        let yaw = target.lon.to_radians();
        self.target_yaw += (yaw - self.target_yaw + PI).rem_euclid(TAU) - PI;
        self.target_gaze_yaw = 0.0;
        self.target_gaze_pitch = 0.0;
    }

    /// Returns the view to where it started, rig angles included.
    pub fn reset(&mut self) {
        let start = Self::default();
        self.target_yaw = start.target_yaw;
        self.target_pitch = start.target_pitch;
        self.target_distance = start.target_distance;
        self.target_heading = start.target_heading;
        self.target_tilt = start.target_tilt;
        self.target_gaze_yaw = start.target_gaze_yaw;
        self.target_gaze_pitch = start.target_gaze_pitch;
    }

    pub fn zoom_by(&mut self, exponent: f32) {
        self.target_distance =
            (self.target_distance * (-exponent).exp()).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    pub fn orbit_by(&mut self, yaw: f32, pitch: f32) {
        self.target_yaw -= yaw;
        self.target_pitch = (self.target_pitch + pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Drags the globe under the camera by a screen-space amount, in radians of
    /// orbit per axis.
    ///
    /// This is [`OrbitCamera::orbit_by`] with the compass taken into account:
    /// once the rig has been turned, screen right is no longer east, so the
    /// delta is rotated back out of screen space before it moves the anchor.
    /// Dragging always pushes the ground the way the pointer went.
    pub fn pan_by(&mut self, screen: Vec2) {
        let delta = Vec2::from_angle(-self.target_heading).rotate(screen);
        self.orbit_by(delta.x, delta.y);
    }

    /// Swings the camera around the point it is looking at: `heading` about the
    /// local vertical, `tilt` down from overhead toward the horizon. The view
    /// stays on that point throughout.
    pub fn rotate_rig_by(&mut self, heading: f32, tilt: f32) {
        self.target_heading += heading;
        self.target_tilt = (self.target_tilt + tilt).clamp(0.0, TILT_LIMIT);
    }

    /// Turns the camera where it stands, without moving it. Unlike the rig
    /// angles this points the view off the anchor entirely, so it is bounded on
    /// both axes.
    pub fn gaze_by(&mut self, yaw: f32, pitch: f32) {
        self.target_gaze_yaw = (self.target_gaze_yaw + yaw).clamp(-GAZE_LIMIT, GAZE_LIMIT);
        self.target_gaze_pitch = (self.target_gaze_pitch + pitch).clamp(-GAZE_LIMIT, GAZE_LIMIT);
    }

    /// Where the camera stands and which way it faces, built from the smoothed
    /// angles rather than the targets.
    fn transform(&self) -> Transform {
        // The frame standing on the ground under the camera: X east, Y north,
        // Z straight up. Yaw and pitch put it at a longitude and a latitude,
        // which is why they can be read straight back off as one.
        let anchored = Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(-self.pitch);
        // Heading spins that frame about the vertical; tilt lays it over toward
        // the horizon. Both leave the origin of the frame — the point under the
        // camera — exactly where it was, which is what keeps the view on it.
        let rig = anchored * Quat::from_rotation_z(self.heading) * Quat::from_rotation_x(self.tilt);

        // The camera sits one range back along the rig from the anchor, so with
        // no tilt it is directly overhead at `distance` from the centre, and
        // with tilt it slides down the tangent and looks back up the way it came.
        let anchor = anchored * (Vec3::Z * GLOBE_RADIUS);
        let range = self.distance - GLOBE_RADIUS;
        Transform {
            translation: anchor + rig * (Vec3::Z * range),
            // Free look turns the camera in place, after everything that
            // positioned it.
            rotation: rig
                * Quat::from_rotation_y(self.gaze_yaw)
                * Quat::from_rotation_x(self.gaze_pitch),
            scale: Vec3::ONE,
        }
    }
}

/// Remembers the previous two-finger spread so pinches can be turned into zoom.
#[derive(Resource, Default)]
struct TouchTracker {
    previous_spread: Option<f32>,
}

pub(crate) fn spawn_camera(mut commands: Commands) {
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
        HeliocentricCamera::default(),
    ));
}

fn mouse_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut pinch: MessageReader<PinchGesture>,
    mut camera: Single<&mut OrbitCamera>,
) {
    if buttons.pressed(MouseButton::Left) && motion.delta != Vec2::ZERO {
        let control = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
        let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
        // All three drags read the same way round: the pointer pushes the scene,
        // so dragging down brings what is beyond the top of the screen into view
        // whether that is done by panning, laying the camera over or looking up.
        if control {
            let delta = motion.delta * LOOK_SENSITIVITY;
            camera.rotate_rig_by(delta.x, delta.y);
        } else if shift {
            let delta = motion.delta * LOOK_SENSITIVITY;
            camera.gaze_by(delta.x, delta.y);
        } else {
            let scale = DRAG_SENSITIVITY * camera.altitude_factor();
            camera.pan_by(motion.delta * scale);
        }
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
        camera.pan_by(Vec2::new(-step.x, step.y));
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

    if keys.just_pressed(KeyCode::KeyR) {
        camera.reset();
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
                camera.pan_by(delta * scale);
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
        }
        _ => tracker.previous_spread = None,
    }
}

/// Keeps the camera over the same patch of ground when the frame changes.
///
/// Switching frames turns the globe by most of a full rotation in a single
/// step. The camera is not attached to it, so without this the ground would
/// slide out from under the view: the same toggle that turns the Earth turns
/// the orbit with it. Both the smoothed yaw and the target move, so the switch
/// is instant rather than a long whip around the planet.
fn follow_frame(
    mut realigned: MessageReader<FrameRealigned>,
    mut camera: Single<&mut OrbitCamera>,
) {
    for realignment in realigned.read() {
        camera.yaw += realignment.ground_yaw;
        camera.target_yaw += realignment.ground_yaw;
    }
}

fn apply_orbit(time: Res<Time>, mut camera: Single<(&mut OrbitCamera, &mut Transform)>) {
    let (orbit, transform) = &mut *camera;

    // Frame-rate independent exponential smoothing toward the input targets.
    let t = 1.0 - (-SMOOTHING * time.delta_secs()).exp();
    orbit.yaw += (orbit.target_yaw - orbit.yaw) * t;
    orbit.pitch += (orbit.target_pitch - orbit.pitch) * t;
    orbit.distance += (orbit.target_distance - orbit.distance) * t;
    orbit.heading += (orbit.target_heading - orbit.heading) * t;
    orbit.tilt += (orbit.target_tilt - orbit.tilt) * t;
    orbit.gaze_yaw += (orbit.target_gaze_yaw - orbit.gaze_yaw) * t;
    orbit.gaze_pitch += (orbit.target_gaze_pitch - orbit.gaze_pitch) * t;

    **transform = orbit.transform();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An orbit sitting exactly on its targets, which is what the transform is
    /// built from once the smoothing has caught up.
    fn settled(yaw: f32, pitch: f32, distance: f32) -> OrbitCamera {
        OrbitCamera {
            yaw,
            pitch,
            distance,
            target_yaw: yaw,
            target_pitch: pitch,
            target_distance: distance,
            ..default()
        }
    }

    #[test]
    fn no_rig_angles_leaves_the_camera_overhead() {
        let orbit = settled(-0.5, 0.35, DEFAULT_DISTANCE);
        let transform = orbit.transform();
        let overhead = orbit.world_center().to_direction() * orbit.distance;
        assert!((transform.translation - overhead).length() < 1.0e-4);
        // Looking straight down is looking at the centre of the Earth.
        assert!((transform.forward().as_vec3() + overhead.normalize()).length() < 1.0e-4);
    }

    #[test]
    fn tilting_keeps_the_view_on_the_same_place() {
        let anchor = settled(-0.5, 0.35, DEFAULT_DISTANCE)
            .world_center()
            .to_direction()
            * GLOBE_RADIUS;
        for tilt in [0.0, 0.4, TILT_LIMIT] {
            for heading in [0.0, 1.0, -2.5] {
                let orbit = OrbitCamera {
                    heading,
                    tilt,
                    ..settled(-0.5, 0.35, DEFAULT_DISTANCE)
                };
                let transform = orbit.transform();
                let to_anchor = (anchor - transform.translation).normalize();
                assert!(
                    (transform.forward().as_vec3() - to_anchor).length() < 1.0e-4,
                    "tilt {tilt} heading {heading}"
                );
                // Laid over or not, the camera stays outside the globe.
                assert!(transform.translation.length() > GLOBE_RADIUS);
            }
        }
    }

    #[test]
    fn free_look_turns_without_moving() {
        let straight = OrbitCamera {
            tilt: 0.3,
            ..settled(1.0, -0.2, DEFAULT_DISTANCE)
        };
        let turned = OrbitCamera {
            gaze_yaw: 0.6,
            gaze_pitch: -0.3,
            ..OrbitCamera {
                tilt: 0.3,
                ..settled(1.0, -0.2, DEFAULT_DISTANCE)
            }
        };
        assert!(
            (straight.transform().translation - turned.transform().translation).length() < 1.0e-5
        );
        let angle = straight
            .transform()
            .forward()
            .angle_between(turned.transform().forward().as_vec3());
        assert!(angle > 0.5, "{angle}");
    }

    #[test]
    fn panning_follows_the_compass() {
        // With the rig turned a quarter turn, a drag that moved the ground east
        // now moves it north instead: the pointer pushes the scene the way the
        // screen is oriented, not the way the globe is.
        let mut level = settled(0.0, 0.0, DEFAULT_DISTANCE);
        level.pan_by(Vec2::new(0.1, 0.0));
        let mut turned = OrbitCamera {
            heading: FRAC_PI_2,
            target_heading: FRAC_PI_2,
            ..settled(0.0, 0.0, DEFAULT_DISTANCE)
        };
        turned.pan_by(Vec2::new(0.1, 0.0));
        assert!(
            (level.target_yaw + 0.1).abs() < 1.0e-5,
            "{}",
            level.target_yaw
        );
        assert!(turned.target_yaw.abs() < 1.0e-5, "{}", turned.target_yaw);
        assert!(
            (turned.target_pitch + 0.1).abs() < 1.0e-5,
            "{}",
            turned.target_pitch
        );
    }

    #[test]
    fn rig_angles_are_bounded_and_reset() {
        let mut orbit = OrbitCamera::default();
        orbit.rotate_rig_by(0.0, 10.0);
        orbit.gaze_by(10.0, -10.0);
        assert_eq!(orbit.target_tilt, TILT_LIMIT);
        assert_eq!(orbit.target_gaze_yaw, GAZE_LIMIT);
        assert_eq!(orbit.target_gaze_pitch, -GAZE_LIMIT);
        // Tilting the other way stops at overhead rather than passing under.
        orbit.rotate_rig_by(0.0, -10.0);
        assert_eq!(orbit.target_tilt, 0.0);

        orbit.rotate_rig_by(1.0, 0.5);
        orbit.reset();
        assert_eq!(orbit.target_heading, 0.0);
        assert_eq!(orbit.target_tilt, 0.0);
        assert_eq!(orbit.target_gaze_yaw, 0.0);
        assert_eq!(orbit.target_gaze_pitch, 0.0);
    }
}
