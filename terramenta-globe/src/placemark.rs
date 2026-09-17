//! Placemarks: an icon pinned to a coordinate and drawn at a fixed size on
//! screen.
//!
//! Two are up from the moment the globe starts, and both are the same thing
//! seen from the ground: the point a body is directly overhead.
//!
//! The **subsolar** point moves west at fifteen degrees an hour and up and down
//! with the season. It is where the terminator is drawn *from*, so having it
//! marked turns the lighting on the globe from something to look at into
//! something to read: the bright spot under the icon is local noon, and the
//! ring of twilight is a quarter of the planet away.
//!
//! The **sublunar** point is the moon's, and it moves differently enough to be
//! worth watching — a little over twelve degrees further west each day, and
//! wandering as far as 28° from the equator over the nineteen years its orbit's
//! nodes take to come round. How far it is from the sun's icon is the phase:
//! together is new, opposite is full, and a quarter of the planet apart is a
//! half moon.
//!
//! Neither coordinate is worked out here. They come from [`crate::sun::Sun`]
//! and [`crate::moon::Moon`], which are the only places that know — so the
//! shading, the HUD's `sun over` line and these icons can never disagree about
//! where either body is.
//!
//! Like an overlay marker, a placemark is **sized in pixels rather than in
//! kilometres**: the mesh is one quad with all four corners on the anchor, and
//! `assets/shaders/icon.wgsl` spreads them across the screen. And like
//! everything else geographic, the anchor is Earth-fixed and rotated into world
//! space by [`crate::frame::ReferenceFrame::earth_to_world`], so the icon stays
//! over its ground in either frame.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::LatLon;
use crate::globe::GLOBE_RADIUS;
use crate::moon::Moon;
use crate::sun::Sun;
use crate::tiles::MAX_TILE_RADIUS;

/// The icons, drawn at the size they were authored at.
const SUN_ICON: &str = "icons/sun32.png";
const MOON_ICON: &str = "icons/moon32.png";
const ICON_PX: f32 = 32.0;

/// How far out a placemark's anchor sits, in scene units.
///
/// Above the highest an imagery tile is ever drawn, on the same reasoning as
/// the overlay radii in [`crate::overlays`] — a tile stands proud of the sphere
/// by as much as a kilometre and a half at its corners, so anything drawn on
/// the surface itself is buried at every corner. Above an overlay marker as
/// well, because a placemark is the annotation over the annotations.
const PLACEMARK_RADIUS: f32 = MAX_TILE_RADIUS + GLOBE_RADIUS * 2.5e-4;

/// Where a placemark sits in the transparent pass.
///
/// Everything over the globe is drawn on very nearly the same sphere, so the
/// pass has almost nothing to sort by and these biases settle it instead. Above
/// an overlay marker and its highlight halo, below
/// [`crate::overlays::EPHEMERIS_ABOVE`]: a satellite really is hundreds of
/// kilometres further out, and should come out in front.
const PLACEMARK_ABOVE: f32 = 6.0;

/// An icon pinned to a point on the globe.
///
/// The coordinate is Earth-fixed. Write to it and the icon moves; the frame is
/// applied afterwards, in [`place_placemarks`].
#[derive(Component, Debug, Clone, Copy)]
pub struct Placemark {
    pub coordinate: LatLon,
}

/// Marks the placemark that follows the sun.
#[derive(Component, Debug)]
pub struct Subsolar;

/// Marks the placemark that follows the moon.
#[derive(Component, Debug)]
pub struct Sublunar;

pub struct PlacemarkPlugin;

impl Plugin for PlacemarkPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<IconMaterial>::default())
            .add_systems(Startup, spawn_placemarks)
            .add_systems(
                Update,
                // The clock has already been advanced by the time `Apply` runs,
                // so the points read here are this frame's, not last frame's —
                // and the frame rotation they are placed under has been settled
                // for this tick as well.
                (follow_the_sun, follow_the_moon, place_placemarks)
                    .chain()
                    .in_set(FrameSet::Apply),
            );
    }
}

fn spawn_placemarks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<IconMaterial>>,
    assets: Res<AssetServer>,
    sun: Res<Sun>,
    moon: Res<Moon>,
) {
    // One quad serves both, and every placemark added after them: the mesh is
    // four corners on the origin, and everything that makes one icon different
    // from another is in its material and its transform.
    let quad = meshes.add(icon_quad());

    commands.spawn((
        Name::new("Subsolar placemark"),
        Placemark {
            coordinate: sun.subsolar,
        },
        Subsolar,
        Mesh3d(quad.clone()),
        MeshMaterial3d(materials.add(IconMaterial::new(assets.load(SUN_ICON), ICON_PX))),
        Transform::from_translation(anchor(sun.subsolar)),
        // The quad is spread in the vertex shader from four corners sitting on
        // top of one another, so its own bounds are a point: left to cull
        // itself it would vanish the moment the anchor left the screen, with
        // half the icon still on it.
        NoFrustumCulling,
    ));

    commands.spawn((
        Name::new("Sublunar placemark"),
        Placemark {
            coordinate: moon.sublunar,
        },
        Sublunar,
        Mesh3d(quad),
        MeshMaterial3d(materials.add(IconMaterial::new(assets.load(MOON_ICON), ICON_PX))),
        Transform::from_translation(anchor(moon.sublunar)),
        NoFrustumCulling,
    ));
}

/// Keeps the subsolar placemark on the point the sun is overhead.
fn follow_the_sun(sun: Res<Sun>, mut placemarks: Query<&mut Placemark, With<Subsolar>>) {
    for mut placemark in &mut placemarks {
        placemark.coordinate = sun.subsolar;
    }
}

/// And the sublunar one on the point the moon is.
fn follow_the_moon(moon: Res<Moon>, mut placemarks: Query<&mut Placemark, With<Sublunar>>) {
    for mut placemark in &mut placemarks {
        placemark.coordinate = moon.sublunar;
    }
}

/// Carries every placemark's Earth-fixed coordinate into world space.
fn place_placemarks(
    frame: Res<ReferenceFrame>,
    mut placemarks: Query<(&Placemark, &mut Transform)>,
) {
    let earth_to_world = frame.earth_to_world();
    for (placemark, mut transform) in &mut placemarks {
        transform.translation = earth_to_world * anchor(placemark.coordinate);
    }
}

/// Where a coordinate's icon hangs, in the Earth-fixed frame.
fn anchor(coordinate: LatLon) -> Vec3 {
    coordinate.to_direction() * PLACEMARK_RADIUS
}

/// One quad, all four corners on the anchor. The UV says which corner this is,
/// which is both what the vertex shader spreads it by and what the fragment
/// shader reads the icon with.
fn icon_quad() -> Mesh {
    let corners = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0, 0.0, 0.0]; 4])
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4])
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, corners.to_vec())
    .with_inserted_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]))
}

// ---------------------------------------------------------------------------
// The material
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, ShaderType)]
pub struct IconUniform {
    /// Multiplied into the texture. Linear, straight alpha.
    pub tint: Vec4,
    /// Half the icon's size on screen, in device pixels: the mesh spreads each
    /// corner one unit either way, so the shader wants the half-extent.
    pub size_px: f32,
    pub _padding: Vec3,
}

#[derive(Asset, AsBindGroup, TypePath, Clone)]
pub struct IconMaterial {
    #[uniform(0)]
    pub uniform: IconUniform,
    #[texture(1)]
    #[sampler(2)]
    pub icon: Option<Handle<Image>>,
}

impl IconMaterial {
    fn new(icon: Handle<Image>, size_px: f32) -> Self {
        Self {
            uniform: IconUniform {
                tint: Vec4::ONE,
                size_px: (size_px * 0.5).max(0.1),
                _padding: Vec3::ZERO,
            },
            icon: Some(icon),
        }
    }
}

impl Material for IconMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/icon.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/icon.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn depth_bias(&self) -> f32 {
        PLACEMARK_ABOVE
    }

    fn enable_shadows() -> bool {
        false
    }

    fn enable_prepass() -> bool {
        // The prepass would draw this with the *default* vertex shader, which
        // knows nothing about spreading the quad — so the placemark would write
        // its depth as two degenerate triangles on the anchor.
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The quad's winding flips as the globe turns under it, so which way it
        // faces cannot decide whether it is drawn.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placemark_hangs_above_the_imagery() {
        // Clear of the highest a tile is ever drawn, or the icon's anchor would
        // be inside the terrain it is meant to be marking.
        const { assert!(PLACEMARK_RADIUS > MAX_TILE_RADIUS) };
        let anchor = anchor(LatLon::new(0.0, 0.0));
        assert!((anchor.length() - PLACEMARK_RADIUS).abs() < 1.0e-6);
    }

    #[test]
    fn a_placemark_sits_over_its_coordinate() {
        let coordinate = LatLon::new(23.44, -75.0);
        let placed = LatLon::from_direction(anchor(coordinate));
        assert!((placed.lat - coordinate.lat).abs() < 1.0e-3, "{placed:?}");
        assert!((placed.lon - coordinate.lon).abs() < 1.0e-3, "{placed:?}");
    }

    #[test]
    fn the_subsolar_placemark_follows_the_clock() {
        // Six hours on is a quarter of the way around the planet, westward.
        let mut sun = Sun::default();
        sun.set_clock(0.0);
        let noon = sun.subsolar.lon;
        sun.set_clock(6.0 * 3600.0);
        let separation = (noon - sun.subsolar.lon).rem_euclid(360.0);
        assert!((separation - 90.0).abs() < 1.0e-3, "{separation}");
    }
}
