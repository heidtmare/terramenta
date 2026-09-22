//! A procedurally starred sky, shared by the Earth-centered globe view and the
//! heliocentric view — same shader, same mesh helper, different radius and
//! different rotation convention for "still".
//!
//! Previously each view kept its own copy of this material and its own system
//! driving the rotation uniform. `crate::globe`'s system iterated every
//! [`StarfieldMaterial`] asset regardless of which view it belonged to, so the
//! two systems raced over the same material, producing a visible wobble. Now
//! one shared component and a single system drive it, gated per-entity by
//! which frame the entity was spawned in — there is only ever one writer per
//! material.

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, Face, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use crate::frame::ReferenceFrame;
use crate::geo::equirectangular_sphere;

pub struct StarfieldPlugin;

impl Plugin for StarfieldPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<StarfieldMaterial>::default())
            .add_systems(Update, drive_starfield);
    }
}

/// Which sky rotation a starfield entity should be driven by. The globe view
/// spins the sky backward against Earth's own rotation so the stars read as
/// fixed while the ground turns beneath them ([`ReferenceFrame::sky_rotation`]);
/// the heliocentric view has no ECEF/ECI split to speak of — everything in it
/// is already drawn in one fixed, inertial orientation — so its sky just
/// holds still.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Starfield {
    Ecef,
    Inertial,
}

#[derive(Clone, Copy, Debug, ShaderType, Default)]
pub struct StarfieldUniform {
    /// Drives the twinkle; fed from elapsed time.
    pub time: f32,
    /// How far the sky is turned about the poles, in radians.
    pub rotation: f32,
}

/// A procedurally starred sky, drawn on the inside of a very large sphere.
#[derive(Asset, AsBindGroup, TypePath, Clone, Default)]
pub struct StarfieldMaterial {
    #[uniform(0)]
    pub uniform: StarfieldUniform,
}

impl Material for StarfieldMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/starfield.wgsl".into()
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
        // We sit inside this sphere, so its front faces point away from us.
        descriptor.primitive.cull_mode = Some(Face::Front);
        if let Some(depth_stencil) = descriptor.depth_stencil.as_mut() {
            depth_stencil.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// A starfield sphere of the given radius and kind, for a caller to spawn
/// alongside whatever view-specific components (visibility toggling, and so
/// on) it needs — the globe view's own, always-visible sphere and the
/// heliocentric view's hidden-until-toggled one build on the same bundle.
pub fn bundle(
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StarfieldMaterial>,
    radius: f32,
    kind: Starfield,
) -> impl Bundle {
    (
        Name::new(match kind {
            Starfield::Ecef => "Starfield",
            Starfield::Inertial => "Heliocentric Starfield",
        }),
        kind,
        Mesh3d(meshes.add(equirectangular_sphere(radius, 48, 24))),
        MeshMaterial3d(materials.add(StarfieldMaterial::default())),
        Transform::IDENTITY,
    )
}

/// Drives every starfield's twinkle and sky rotation in one pass — the only
/// system that writes [`StarfieldUniform`], so the globe and heliocentric
/// skies can no longer race over the same material the way they used to.
fn drive_starfield(
    time: Res<Time>,
    frame: Res<ReferenceFrame>,
    mut materials: ResMut<Assets<StarfieldMaterial>>,
    starfields: Query<(&Starfield, &MeshMaterial3d<StarfieldMaterial>)>,
) {
    let elapsed = time.elapsed_secs();
    for (kind, material) in &starfields {
        let Some(mut material) = materials.get_mut(&material.0) else {
            continue;
        };
        material.uniform.time = elapsed;
        material.uniform.rotation = match kind {
            Starfield::Ecef => frame.sky_rotation(),
            Starfield::Inertial => 0.0,
        };
    }
}
