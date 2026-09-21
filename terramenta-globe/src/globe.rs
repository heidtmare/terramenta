//! The Earth itself: the textured surface, its cloud deck, the atmospheric
//! shell around it and the star field behind it.

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, Face, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::equirectangular_sphere;
use crate::imagery::ImagerySettings;
use crate::starfield::{self, Starfield, StarfieldMaterial};
use crate::sun::Sun;

/// Radius of the globe in world units. Everything else is expressed in Earth radii.
pub const GLOBE_RADIUS: f32 = 1.0;
/// The atmosphere shell sits just above the surface, like the real one.
pub const ATMOSPHERE_RADIUS: f32 = GLOBE_RADIUS * 1.025;
/// Far enough away to read as "infinitely distant" without leaving the far plane.
const STARFIELD_RADIUS: f32 = 400.0;
/// How brightly the base globe's city lights burn on its night side.
const NIGHT_INTENSITY: f32 = 1.6;

/// Marks the entity carrying the Earth surface mesh.
#[derive(Component)]
pub struct Globe;

pub struct GlobePlugin;

impl Plugin for GlobePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            MaterialPlugin::<GlobeMaterial>::default(),
            MaterialPlugin::<AtmosphereMaterial>::default(),
        ))
        .add_systems(Startup, spawn_globe)
        .add_systems(
            Update,
            (orient_globe, drive_materials).in_set(FrameSet::Apply),
        );
    }
}

// ---------------------------------------------------------------------------
// Materials
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, ShaderType)]
pub struct GlobeUniform {
    /// Unit vector from the globe's center toward the sun, in world space.
    pub sun_direction: Vec3,
    /// Longitudinal offset of the cloud layer, in UV units, so weather drifts.
    pub cloud_offset: f32,
    /// Brightness multiplier for city lights on the night side.
    pub night_intensity: f32,
    /// How opaque the cloud deck reads at full coverage.
    pub cloud_opacity: f32,
    /// Strength of the blue scattering that bleeds over the lit limb.
    pub rim_strength: f32,
    /// Width of the day/night terminator, as a dot-product range.
    pub terminator_softness: f32,
    /// Scales every sunlight term: `1.0` is the real terminator, `0.0` floods
    /// the whole globe with daylight.
    pub sun_shading: f32,
}

impl Default for GlobeUniform {
    fn default() -> Self {
        Self {
            sun_direction: Vec3::X,
            cloud_offset: 0.0,
            night_intensity: NIGHT_INTENSITY,
            cloud_opacity: 0.85,
            rim_strength: 0.55,
            terminator_softness: 0.12,
            sun_shading: 1.0,
        }
    }
}

/// Lights the Earth from its textures directly rather than through the PBR
/// pipeline: the day/night blend, city lights and ocean specular all key off a
/// single sun direction, which is cheaper and easier to art-direct than a
/// [`DirectionalLight`] plus a [`StandardMaterial`].
#[derive(Asset, AsBindGroup, TypePath, Clone)]
pub struct GlobeMaterial {
    #[uniform(0)]
    pub uniform: GlobeUniform,
    #[texture(1)]
    #[sampler(2)]
    pub day_texture: Option<Handle<Image>>,
    #[texture(3)]
    #[sampler(4)]
    pub night_texture: Option<Handle<Image>>,
    #[texture(5)]
    #[sampler(6)]
    pub cloud_texture: Option<Handle<Image>>,
}

impl Material for GlobeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/globe.wgsl".into()
    }
}

#[derive(Clone, Copy, Debug, ShaderType)]
pub struct AtmosphereUniform {
    pub sun_direction: Vec3,
    pub density: f32,
    pub color: Vec3,
    pub falloff: f32,
    /// Matches [`GlobeUniform::sun_shading`], so the glow does not keep a
    /// terminator the surface underneath has lost.
    pub sun_shading: f32,
}

impl Default for AtmosphereUniform {
    fn default() -> Self {
        Self {
            sun_direction: Vec3::X,
            density: 1.0,
            color: Vec3::new(0.29, 0.52, 1.0),
            falloff: 3.4,
            sun_shading: 1.0,
        }
    }
}

/// A thin additive shell that fakes Rayleigh scattering at the limb.
#[derive(Asset, AsBindGroup, TypePath, Clone, Default)]
pub struct AtmosphereMaterial {
    #[uniform(0)]
    pub uniform: AtmosphereUniform,
}

impl Material for AtmosphereMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/atmosphere.wgsl".into()
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
        // The glow is strongest where the shell is seen edge-on, so draw its far
        // side and let it accumulate over whatever is already in the buffer.
        descriptor.primitive.cull_mode = Some(Face::Front);
        if let Some(depth_stencil) = descriptor.depth_stencil.as_mut() {
            depth_stencil.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

fn spawn_globe(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut globe_materials: ResMut<Assets<GlobeMaterial>>,
    mut atmosphere_materials: ResMut<Assets<AtmosphereMaterial>>,
    mut starfield_materials: ResMut<Assets<StarfieldMaterial>>,
    assets: Res<AssetServer>,
) {
    commands.spawn((
        Name::new("Earth"),
        Globe,
        Mesh3d(meshes.add(equirectangular_sphere(GLOBE_RADIUS, 192, 96))),
        MeshMaterial3d(globe_materials.add(GlobeMaterial {
            uniform: GlobeUniform::default(),
            day_texture: Some(assets.load("textures/earth_day.jpg")),
            night_texture: Some(assets.load("textures/earth_night.jpg")),
            cloud_texture: Some(assets.load("textures/earth_clouds.jpg")),
        })),
        Transform::IDENTITY,
    ));

    commands.spawn((
        Name::new("Atmosphere"),
        Mesh3d(meshes.add(equirectangular_sphere(ATMOSPHERE_RADIUS, 96, 48))),
        MeshMaterial3d(atmosphere_materials.add(AtmosphereMaterial::default())),
        Transform::IDENTITY,
    ));

    commands.spawn(starfield::bundle(
        &mut meshes,
        &mut starfield_materials,
        STARFIELD_RADIUS,
        Starfield::Ecef,
    ));
}

/// Turns the globe to face the way the active frame says it should.
///
/// In ECEF this is the identity every frame: world space is the Earth-fixed
/// frame, so the planet never moves and the sun sweeps past it instead.
fn orient_globe(frame: Res<ReferenceFrame>, mut globe: Single<&mut Transform, With<Globe>>) {
    globe.rotation = frame.earth_to_world();
}

/// Pushes the simulated sun direction and the animated offsets into the shaders.
fn drive_materials(
    time: Res<Time>,
    sun: Res<Sun>,
    frame: Res<ReferenceFrame>,
    imagery: Res<ImagerySettings>,
    mut globe_materials: ResMut<Assets<GlobeMaterial>>,
    mut atmosphere_materials: ResMut<Assets<AtmosphereMaterial>>,
) {
    let elapsed = time.elapsed_secs();
    // The shaders light everything in world space, so the Earth-fixed sun has
    // to be carried into whichever frame the scene is being drawn in.
    let sun_direction = frame.earth_to_world() * sun.direction_ecef;

    // The city lights belong to the Blue Marble night texture, and nothing in a
    // streamed layer knows about them: where imagery covers the globe they would
    // burn through a scene that has its own idea of what the ground looks like,
    // and where it has not arrived yet they would light only the gaps. So the
    // night side goes dark for as long as imagery is on.
    let night_intensity = if imagery.enabled {
        0.0
    } else {
        NIGHT_INTENSITY
    };
    let sun_shading = sun.shading();

    for (_, material) in globe_materials.iter_mut() {
        material.uniform.sun_direction = sun_direction;
        material.uniform.night_intensity = night_intensity;
        material.uniform.sun_shading = sun_shading;
        // Clouds drift a little faster than the planet turns beneath them.
        material.uniform.cloud_offset = (elapsed * 0.002).fract();
    }

    for (_, material) in atmosphere_materials.iter_mut() {
        material.uniform.sun_direction = sun_direction;
        material.uniform.sun_shading = sun_shading;
    }
}
