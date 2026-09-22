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
//! unit across — invisible from a vantage that sees all three bodies at once.
//! [`SUN_VISUAL_RADIUS_AU`], [`EARTH_VISUAL_RADIUS_AU`] and
//! [`MARS_VISUAL_RADIUS_AU`] are art-directed instead: sized by eye to read
//! clearly on screen, not by a formula, the same way [`crate::globe`]'s own
//! radii are tuned constants rather than derived ones.

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, Face, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use crate::solar::{FloatingOrigin, SolarBody, SolarSystem, floating_offset};
use crate::starfield::{self, Starfield, StarfieldMaterial};
use crate::sun::Sun;
use crate::view::in_heliocentric_view;
use terramenta_solare::{Epoch, FrameId};

/// The Sun's visual radius, wildly exaggerated from its true ~0.00465 AU so
/// it reads as more than a point from Earth's orbit.
const SUN_VISUAL_RADIUS_AU: f32 = 0.02;
/// The corona shell drawn around the Sun, sized relative to its own radius
/// the same way [`crate::globe::ATMOSPHERE_RADIUS`] is sized off the globe's.
const SUN_CORONA_RADIUS_AU: f32 = SUN_VISUAL_RADIUS_AU * 1.9;
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

/// A body the heliocentric camera can be anchored to, named the way
/// [`crate::view::ViewMode`] names a view — a stable id a keybinding or an
/// embedder picks one by, independent of [`FrameId`], which is only
/// meaningful once [`SolarSystem`] has resolved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliocentricAnchor {
    Sun,
    Earth,
    Mars,
    /// The Solar System Barycentre itself — the tree's own root, not one of
    /// the bodies hanging off it. It has no [`HeliocentricVisual`] of its own
    /// to look at, since nothing is drawn at the SSB, but orbiting it is
    /// still meaningful: unlike the Sun, which wanders a little relative to
    /// it, the SSB is the one point in the scene that never moves.
    Barycenter,
}

impl HeliocentricAnchor {
    /// The stable name the control surface names this anchor by, on the same
    /// terms as [`crate::view::ViewMode::id`].
    pub fn id(self) -> &'static str {
        match self {
            Self::Sun => "sun",
            Self::Earth => "earth",
            Self::Mars => "mars",
            Self::Barycenter => "barycenter",
        }
    }

    /// Parses [`HeliocentricAnchor::id`] back, for an anchor named by an
    /// embedder.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "sun" => Some(Self::Sun),
            "earth" => Some(Self::Earth),
            "mars" => Some(Self::Mars),
            "barycenter" => Some(Self::Barycenter),
            _ => None,
        }
    }

    /// Looks this anchor up in `solar_system` — the Sun, Earth and Mars by
    /// name, the barycentre as the tree's own root, which is always present
    /// and so never `None` the other three are, in principle, before the
    /// tree has been built.
    fn resolve(self, solar_system: &SolarSystem) -> Option<FrameId> {
        match self {
            Self::Sun => solar_system.find("Sun"),
            Self::Earth => solar_system.find("Earth"),
            Self::Mars => solar_system.find("Mars"),
            Self::Barycenter => Some(solar_system.root()),
        }
    }
}

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

    /// Re-anchors the rig on a different body, keeping its current yaw,
    /// pitch and distance — the same cut an orbit target change on
    /// [`crate::camera::OrbitCamera`] makes, just around a different point
    /// in space rather than a different point on the ground. Unresolvable —
    /// which cannot happen for any [`HeliocentricAnchor`] today, since all
    /// four always exist once [`SolarSystem`] does — leaves the rig anchored
    /// where it was.
    pub(crate) fn set_anchor(&mut self, anchor: HeliocentricAnchor, solar_system: &SolarSystem) {
        if let Some(frame) = anchor.resolve(solar_system) {
            self.anchor = Some(frame);
        }
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

// ---------------------------------------------------------------------------
// Sun materials
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, ShaderType)]
pub struct SunUniform {
    pub time: f32,
}

/// Procedural granulation and limb darkening for the Sun's surface, in place
/// of a flat emissive [`StandardMaterial`] — unlit, the same way the Sun
/// entity's old material was, since the Sun lights itself rather than
/// responding to light.
#[derive(Asset, AsBindGroup, TypePath, Clone, Default)]
pub struct SunMaterial {
    #[uniform(0)]
    pub uniform: SunUniform,
}

impl Material for SunMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/sun.wgsl".into()
    }

    fn enable_shadows() -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, ShaderType)]
pub struct SunCoronaUniform {
    pub color: Vec3,
    pub density: f32,
    pub falloff: f32,
    pub time: f32,
}

impl Default for SunCoronaUniform {
    fn default() -> Self {
        Self {
            color: Vec3::new(1.4, 0.85, 0.35),
            density: 1.1,
            falloff: 2.6,
            time: 0.0,
        }
    }
}

/// A thin additive glow shell around the Sun, on the same terms as
/// [`crate::globe::AtmosphereMaterial`] — back-face-only, no depth write, so
/// it accumulates over whatever is behind it instead of occluding it.
#[derive(Asset, AsBindGroup, TypePath, Clone, Default)]
pub struct SunCoronaMaterial {
    #[uniform(0)]
    pub uniform: SunCoronaUniform,
}

impl Material for SunCoronaMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/sun_corona.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Add
    }

    fn enable_shadows() -> bool {
        false
    }

    fn enable_prepass() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = Some(Face::Front);
        if let Some(depth_stencil) = descriptor.depth_stencil.as_mut() {
            depth_stencil.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

pub struct HeliocentricPlugin;

impl Plugin for HeliocentricPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            MaterialPlugin::<SunMaterial>::default(),
            MaterialPlugin::<SunCoronaMaterial>::default(),
        ))
        .add_systems(Startup, spawn_heliocentric_bodies)
        .add_systems(
            Update,
            (
                heliocentric_mouse_input.run_if(in_heliocentric_view),
                heliocentric_keyboard_input
                    .run_if(crate::api::keyboard_enabled)
                    .run_if(in_heliocentric_view),
                apply_heliocentric_orbit.run_if(in_heliocentric_view),
                drive_heliocentric_sun.run_if(in_heliocentric_view),
            ),
        );
    }
}

fn spawn_heliocentric_bodies(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut starfield_materials: ResMut<Assets<StarfieldMaterial>>,
    mut sun_materials: ResMut<Assets<SunMaterial>>,
    mut corona_materials: ResMut<Assets<SunCoronaMaterial>>,
    solar_system: Res<SolarSystem>,
) {
    // A second starfield at heliocentric scale, reusing `crate::starfield`'s
    // shared shader and mesh helper rather than a new one: it is the same
    // idea (a procedurally starred sphere, seen from inside) at a radius that
    // fits this view's much larger distances instead of the globe's, held
    // fixed in the inertial frame this whole view is already drawn in.
    commands.spawn((
        starfield::bundle(
            &mut meshes,
            &mut starfield_materials,
            STARFIELD_RADIUS_AU,
            Starfield::Inertial,
        ),
        HeliocentricVisual,
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
        MeshMaterial3d(sun_materials.add(SunMaterial::default())),
        Transform::default(),
        Visibility::Hidden,
        PointLight {
            intensity: 3.0e7,
            range: 40.0,
            shadow_maps_enabled: false,
            ..default()
        },
    ));

    // A separate, slightly larger shell for the corona glow — its own
    // `SolarBody(sun)` keeps it tracking the Sun's position independently of
    // the surface mesh, the same way the atmosphere shell in `crate::globe`
    // sits just outside the globe it surrounds.
    commands.spawn((
        Name::new("Heliocentric Sun Corona"),
        HeliocentricVisual,
        SolarBody(sun),
        Mesh3d(meshes.add(Sphere::new(SUN_CORONA_RADIUS_AU))),
        MeshMaterial3d(corona_materials.add(SunCoronaMaterial::default())),
        Transform::default(),
        Visibility::Hidden,
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
    solar_system: Res<SolarSystem>,
    mut camera: Single<&mut HeliocentricCamera>,
) {
    if let Some(anchor) = pressed_anchor(&keys) {
        camera.set_anchor(anchor, &solar_system);
    }

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

/// Which [`HeliocentricAnchor`], if any, was just picked by its number key —
/// `1` through `4` in the same order [`HeliocentricVisual`] draws them, Sun
/// first, plus the barycentre nothing is drawn at.
fn pressed_anchor(keys: &ButtonInput<KeyCode>) -> Option<HeliocentricAnchor> {
    if keys.just_pressed(KeyCode::Digit1) {
        Some(HeliocentricAnchor::Sun)
    } else if keys.just_pressed(KeyCode::Digit2) {
        Some(HeliocentricAnchor::Earth)
    } else if keys.just_pressed(KeyCode::Digit3) {
        Some(HeliocentricAnchor::Mars)
    } else if keys.just_pressed(KeyCode::Digit4) {
        Some(HeliocentricAnchor::Barycenter)
    } else {
        None
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

/// Animates the Sun's granulation and corona flicker. The heliocentric
/// starfield's own twinkle is driven by [`crate::starfield`] instead, shared
/// with the globe view's sky rather than kept as a separate system here.
fn drive_heliocentric_sun(
    time: Res<Time>,
    sun: Single<&MeshMaterial3d<SunMaterial>, With<HeliocentricVisual>>,
    corona: Single<&MeshMaterial3d<SunCoronaMaterial>, With<HeliocentricVisual>>,
    mut sun_materials: ResMut<Assets<SunMaterial>>,
    mut corona_materials: ResMut<Assets<SunCoronaMaterial>>,
) {
    let elapsed = time.elapsed_secs();
    if let Some(mut material) = sun_materials.get_mut(&sun.0) {
        material.uniform.time = elapsed;
    }
    if let Some(mut material) = corona_materials.get_mut(&corona.0) {
        material.uniform.time = elapsed;
    }
}
