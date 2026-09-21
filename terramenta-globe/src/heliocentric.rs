//! The heliocentric view: the Sun, Earth and Mars themselves, drawn as
//! simple lit spheres around the Solar System Barycentre, and the camera rig
//! that orbits them.
//!
//! Everything here draws through the same [`crate::solar::SolarBody`] and
//! [`crate::solar::place_solar_bodies`] machinery a spacecraft already used —
//! these are just three more entities carrying that component, scaled in
//! astronomical units instead of Earth radii once [`crate::view::ViewState`]
//! says the view is heliocentric. What's new is a mesh to look at (a
//! spacecraft draws nothing of its own) and a camera rig sized for the
//! distances between planets rather than the distance to a horizon.
//!
//! At true scale a planet is a few hundred-thousandths of an astronomical
//! unit across — an invisible dot from a vantage that can see all three
//! bodies at once. [`SUN_VISUAL_RADIUS_AU`], [`EARTH_VISUAL_RADIUS_AU`] and
//! [`MARS_VISUAL_RADIUS_AU`] are art-directed instead: sized by eye to read
//! clearly on screen, not by a formula, the same way [`crate::globe`]'s own
//! radii are tuned constants rather than derived ones.

use bevy::prelude::*;

use crate::geo::equirectangular_sphere;
use crate::globe::StarfieldMaterial;
use crate::solar::{FloatingOrigin, SolarBody, SolarSystem, floating_offset};
use crate::sun::Sun;
use crate::view::in_heliocentric_view;
use terramenta_solare::{Epoch, FrameId};

/// The Sun's visual radius, wildly exaggerated from its true ~0.00465 AU so
/// it reads as more than a point from Earth's orbit.
const SUN_VISUAL_RADIUS_AU: f32 = 0.02;
/// Earth's visual radius, exaggerated the same way, keeping roughly Earth and
/// Mars's real 1.88:1 size ratio between the two.
const EARTH_VISUAL_RADIUS_AU: f32 = 0.006;
const MARS_VISUAL_RADIUS_AU: f32 = 0.0032;
/// Inside [`crate::view`]'s far plane (20 AU) but well outside anything the
/// heliocentric camera can orbit out to ([`HELIO_MAX_DISTANCE_AU`]).
const STARFIELD_RADIUS_AU: f32 = 15.0;

/// Closest the heliocentric camera can orbit in to its anchor.
pub(crate) const HELIO_MIN_DISTANCE_AU: f32 = 0.05;
/// Furthest out — comfortably past Mars's aphelion, ~1.66 AU.
pub(crate) const HELIO_MAX_DISTANCE_AU: f32 = 6.0;
/// Where the establishing shot lands, on the cut into this view.
pub(crate) const HELIO_DEFAULT_DISTANCE_AU: f32 = 3.0;

const DRAG_SENSITIVITY: f32 = 0.005;
const KEY_ORBIT_SPEED: f32 = 1.2;
const ZOOM_SENSITIVITY: f32 = 0.15;
/// Same rate [`crate::camera::OrbitCamera`] smooths its own targets at.
const SMOOTHING: f32 = 14.0;
const PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.02;

/// Marks the three [`SolarBody`] entities this module owns — the Sun, Earth
/// and Mars meshes — so [`crate::view::toggle_body_visibility`] can show or
/// hide exactly these without touching a spacecraft's own [`SolarBody`].
#[derive(Component)]
pub(crate) struct HeliocentricVisual;

/// The heliocentric camera rig: a simple orbit, in astronomical units, around
/// whichever body it is [`HeliocentricCamera::anchor`]ed to.
///
/// Unlike [`crate::camera::OrbitCamera`] this has no heading, tilt or free
/// look — a v1 scope call, since a heliocentric establishing shot has no
/// ground to keep level against. `anchor` starts `None` because the frame it
/// should orbit (Earth's) is not known until [`crate::view::drive_view_transition`]
/// looks it up from [`SolarSystem`] at the moment this view actually becomes
/// active; it is meaningless before that.
#[derive(Component)]
pub struct HeliocentricCamera {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target_yaw: f32,
    pub target_pitch: f32,
    pub target_distance: f32,
    pub anchor: Option<FrameId>,
}

impl Default for HeliocentricCamera {
    fn default() -> Self {
        Self {
            yaw: -0.5,
            pitch: 0.35,
            distance: HELIO_DEFAULT_DISTANCE_AU,
            target_yaw: -0.5,
            target_pitch: 0.35,
            target_distance: HELIO_DEFAULT_DISTANCE_AU,
            anchor: None,
        }
    }
}

impl HeliocentricCamera {
    pub(crate) fn zoom_by(&mut self, exponent: f32) {
        self.target_distance = (self.target_distance * (-exponent).exp())
            .clamp(HELIO_MIN_DISTANCE_AU, HELIO_MAX_DISTANCE_AU);
    }

    pub(crate) fn orbit_by(&mut self, yaw: f32, pitch: f32) {
        self.target_yaw -= yaw;
        self.target_pitch = (self.target_pitch + pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Where the camera sits and which way it looks, `distance` back from
    /// `anchor` along the rig's yaw/pitch.
    fn transform(&self, anchor: Vec3) -> Transform {
        let direction =
            Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(-self.pitch) * Vec3::Z;
        Transform::from_translation(anchor + direction * self.distance)
            .looking_at(anchor, Vec3::Y)
    }
}

pub struct HeliocentricPlugin;

impl Plugin for HeliocentricPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_heliocentric_bodies)
            .add_systems(
                Update,
                (
                    heliocentric_mouse_input.run_if(in_heliocentric_view),
                    heliocentric_keyboard_input
                        .run_if(crate::api::keyboard_enabled)
                        .run_if(in_heliocentric_view),
                    apply_heliocentric_orbit.run_if(in_heliocentric_view),
                    drive_heliocentric_starfield.run_if(in_heliocentric_view),
                ),
            );
    }
}

fn spawn_heliocentric_bodies(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut starfield_materials: ResMut<Assets<StarfieldMaterial>>,
    solar_system: Res<SolarSystem>,
) {
    // A second starfield at heliocentric scale, reusing `crate::globe`'s own
    // shader and mesh helper rather than a new one: it is the same idea (a
    // procedurally starred sphere, seen from inside) at a radius that fits
    // this view's much larger distances instead of the globe's.
    commands.spawn((
        Name::new("Heliocentric Starfield"),
        HeliocentricVisual,
        Mesh3d(meshes.add(equirectangular_sphere(STARFIELD_RADIUS_AU, 48, 24))),
        MeshMaterial3d(starfield_materials.add(StarfieldMaterial::default())),
        Transform::IDENTITY,
        Visibility::Hidden,
    ));

    let sun = solar_system
        .find("Sun")
        .expect("terramenta_solare::solar_system always adds a Sun frame");
    let earth = solar_system
        .find("Earth")
        .expect("terramenta_solare::solar_system always adds an Earth frame");
    let mars = solar_system
        .find("Mars")
        .expect("terramenta_solare::solar_system always adds a Mars frame");

    commands.spawn((
        Name::new("Heliocentric Sun"),
        HeliocentricVisual,
        SolarBody(sun),
        Mesh3d(meshes.add(Sphere::new(SUN_VISUAL_RADIUS_AU))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::BLACK,
            emissive: LinearRgba::rgb(8.0, 6.4, 3.2),
            unlit: true,
            ..default()
        })),
        Transform::default(),
        Visibility::Hidden,
        PointLight {
            intensity: 3.0e7,
            range: 40.0,
            shadow_maps_enabled: false,
            ..default()
        },
    ));

    commands.spawn((
        Name::new("Heliocentric Earth"),
        HeliocentricVisual,
        SolarBody(earth),
        Mesh3d(meshes.add(Sphere::new(EARTH_VISUAL_RADIUS_AU))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.25, 0.45, 0.85),
            perceptual_roughness: 0.9,
            ..default()
        })),
        Transform::default(),
        Visibility::Hidden,
    ));

    commands.spawn((
        Name::new("Heliocentric Mars"),
        HeliocentricVisual,
        SolarBody(mars),
        Mesh3d(meshes.add(Sphere::new(MARS_VISUAL_RADIUS_AU))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.75, 0.35, 0.2),
            perceptual_roughness: 0.95,
            ..default()
        })),
        Transform::default(),
        Visibility::Hidden,
    ));
}

fn heliocentric_mouse_input(
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    scroll: Res<bevy::input::mouse::AccumulatedMouseScroll>,
    mut camera: Single<&mut HeliocentricCamera>,
) {
    if buttons.pressed(MouseButton::Left) && motion.delta != Vec2::ZERO {
        let delta = motion.delta * DRAG_SENSITIVITY;
        camera.orbit_by(delta.x, delta.y);
    }
    if scroll.delta.y != 0.0 {
        let gain = match scroll.unit {
            bevy::input::mouse::MouseScrollUnit::Line => 0.15,
            bevy::input::mouse::MouseScrollUnit::Pixel => 0.005,
        };
        camera.zoom_by(scroll.delta.y * gain);
    }
}

fn heliocentric_keyboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut camera: Single<&mut HeliocentricCamera>,
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
        let step = orbit * KEY_ORBIT_SPEED * time.delta_secs();
        camera.orbit_by(-step.x, step.y);
    }

    let mut zoom = 0.0;
    if keys.pressed(KeyCode::Equal) || keys.pressed(KeyCode::NumpadAdd) {
        zoom += 1.0;
    }
    if keys.pressed(KeyCode::Minus) || keys.pressed(KeyCode::NumpadSubtract) {
        zoom -= 1.0;
    }
    if zoom != 0.0 {
        camera.zoom_by(zoom * ZOOM_SENSITIVITY * time.delta_secs() * 10.0);
    }
}

/// Smooths the rig toward its targets and places it relative to its anchor
/// body's own current position, which [`floating_offset`] computes fresh
/// every tick the same way [`crate::solar::place_solar_bodies`] does for
/// every other [`SolarBody`] — the anchor moves around the Sun like anything
/// else in the tree, so the shot has to be rebuilt from where it is now, not
/// where it was when the view was entered.
fn apply_heliocentric_orbit(
    time: Res<Time>,
    solar_system: Res<SolarSystem>,
    origin: Res<FloatingOrigin>,
    sun: Res<Sun>,
    mut camera: Single<(&mut HeliocentricCamera, &mut Transform)>,
) {
    let (rig, transform) = &mut *camera;
    let Some(anchor) = rig.anchor else {
        return;
    };

    let t = 1.0 - (-SMOOTHING * time.delta_secs()).exp();
    rig.yaw += (rig.target_yaw - rig.yaw) * t;
    rig.pitch += (rig.target_pitch - rig.pitch) * t;
    rig.distance += (rig.target_distance - rig.distance) * t;

    let epoch = Epoch::from_unix_seconds(sun.unix_seconds);
    let anchor_position = floating_offset(
        solar_system.tree(),
        anchor,
        origin.frame,
        epoch,
        Quat::IDENTITY,
        terramenta_solare::bodies::ASTRONOMICAL_UNIT_KM,
    );

    **transform = rig.transform(anchor_position);
}

/// Keeps the heliocentric starfield's twinkle animated. Its rotation stays
/// zero rather than tracking [`crate::frame::ReferenceFrame`] the way the
/// globe's own starfield does — there is no ECEF/ECI split out here, just the
/// one fixed, ICRF-aligned orientation every heliocentric placement uses; see
/// [`crate::solar::floating_offset`].
///
/// The query narrows to this view's own starfield entity by component type
/// alone — it is the only [`HeliocentricVisual`] carrying a
/// [`StarfieldMaterial`], since the Sun, Earth and Mars meshes carry a
/// [`StandardMaterial`] instead — so this never touches
/// [`crate::globe`]'s own starfield material sharing the same underlying
/// [`Assets<StarfieldMaterial>`] collection.
fn drive_heliocentric_starfield(
    time: Res<Time>,
    starfield: Single<&MeshMaterial3d<StarfieldMaterial>, With<HeliocentricVisual>>,
    mut materials: ResMut<Assets<StarfieldMaterial>>,
) {
    if let Some(mut material) = materials.get_mut(&starfield.0) {
        material.uniform.time = time.elapsed_secs();
        material.uniform.rotation = 0.0;
    }
}
