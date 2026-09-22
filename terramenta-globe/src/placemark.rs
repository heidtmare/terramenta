//! Placemarks: an icon pinned to a coordinate and drawn at a fixed size on
//! screen.
//!
//! Two exist from startup: the **subsolar** and **sublunar** points, where
//! the sun and moon respectively are directly overhead.
//!
//! The subsolar point moves west at fifteen degrees an hour and shifts in
//! latitude with the season. It is where the terminator is drawn from, so
//! its icon marks local noon (the bright spot under it) and puts the
//! twilight ring a quarter of the planet away.
//!
//! The sublunar point moves a little over twelve degrees further west each
//! day, and wanders as far as 28° from the equator over the nineteen years
//! its orbit's nodes take to come round. Its angular distance from the sun's
//! icon is the phase: together is new, opposite is full, a quarter of the
//! planet apart is a half moon.
//!
//! Neither coordinate is computed here; both come from [`crate::sun::Sun`]
//! and [`crate::moon::Moon`], so the shading, the HUD's `sun over` line, and
//! these icons cannot disagree about where either body is.
//!
//! A placemark is sized in pixels rather than kilometres: the mesh is one
//! quad with all four corners on the anchor, and `assets/shaders/icon.wgsl`
//! spreads them across the screen. It is drawn above the anchor rather than
//! around it. The anchor is Earth-fixed and rotated into world space by
//! [`crate::frame::ReferenceFrame::earth_to_world`], so the icon stays over
//! its ground point in either frame.
//!
//! **An icon is never occluded by the ground it stands on.** A flat quad
//! held up to the camera at a point on a sphere is always partly inside that
//! sphere, since the surface curves away from the quad; from any but a
//! straight-down view, the globe would otherwise pass through the icon and
//! the depth buffer would hide part of it. Standing the icon on its anchor
//! rather than centring it fixes the common case, but not near the limb,
//! where the ground rises to meet the camera faster than the icon can be
//! raised clear of it.
//!
//! So a placemark ignores the depth buffer entirely (see
//! [`IconMaterial::specialize`]) and is always drawn whole over the scene;
//! the one thing that legitimately hides it — the planet itself — does so
//! explicitly in [`hide_over_the_horizon`]. The icon is either fully visible
//! or not drawn at all, since a partial icon would read as a different icon.
//!
//! **Picking is also in pixels**, for the same reason [`crate::ephemeris`]
//! picks its satellites that way rather than through [`crate::picking`]: an
//! icon's screen position is not its coordinate's. It stands above the
//! anchor by its own height, so the pointer is over the icon before it is
//! over the point the icon marks — more so as the view flattens. The anchor
//! is projected into the viewport and the pointer tested against the
//! rectangle the icon occupies there, matching what is drawn on screen.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::ecs::system::SystemParam;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, CompareFunction, RenderPipelineDescriptor, ShaderType,
    SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use serde::Serialize;

use crate::api::Cursor;
use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::LatLon;
use crate::globe::GLOBE_RADIUS;
use crate::moon::Moon;
use crate::overlays::PICK_SLACK_PX;
use crate::sun::Sun;
use crate::tiles::MAX_TILE_RADIUS;
use crate::view::not_departing_view;

/// How big an icon is drawn, in device pixels — the size the images were
/// authored at, so neither is resampled at rest.
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

/// How strongly the halo behind an icon burns: nothing for a placemark that is
/// neither hovered nor pinned, a suggestion for one under the pointer, and the
/// full thing for one an interface has kept.
///
/// Two levels rather than one, because they answer different questions — "this
/// is what you are pointing at" and "this is what is selected" — and an
/// interface that pins on click shows both at once while the pointer stays put.
const HOVERED_HALO: f32 = 0.45;
const PINNED_HALO: f32 = 1.0;

/// Which body a placemark follows. A placemark *is* a body's point on the
/// ground, so this is the whole of what tells one from the other: the icon it
/// is drawn with, the name it is reported under, and the coordinate it tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    Sun,
    Moon,
}

impl Body {
    /// Every placemark the globe puts up, in the order they are spawned.
    const ALL: [Self; 2] = [Self::Sun, Self::Moon];

    /// The stable name the control surface names this by, and what
    /// [`crate::api::GlobeCommand::PinPlacemark`] takes.
    pub fn id(self) -> &'static str {
        match self {
            Self::Sun => "sun",
            Self::Moon => "moon",
        }
    }

    /// Parses [`Body::id`] back, for a placemark named by an embedder.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|body| body.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sun => "Sun · subsolar point",
            Self::Moon => "Moon · sublunar point",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Sun => "icons/sun32.png",
            Self::Moon => "icons/moon32.png",
        }
    }

    /// Where this body is overhead at the moment, which is the one thing a
    /// placemark does not decide for itself.
    fn coordinate(self, sun: &Sun, moon: &Moon) -> LatLon {
        match self {
            Self::Sun => sun.subsolar,
            Self::Moon => moon.sublunar,
        }
    }
}

/// An icon pinned to a point on the globe.
///
/// The coordinate is Earth-fixed and rewritten every tick from the body's own
/// resource; the frame is applied afterwards, in [`place_placemarks`].
#[derive(Component, Debug, Clone, Copy)]
pub struct Placemark {
    pub body: Body,
    pub coordinate: LatLon,
}

/// What the cursor has found among the placemarks, and whether it is looking.
///
/// The same shape as the picks [`crate::overlays`] and [`crate::ephemeris`]
/// keep, and driven by the same switch: picking is one thing to whoever is
/// pointing at the globe, however many hit tests are behind it.
#[derive(Resource, Debug, Clone, Copy)]
pub struct PlacemarkSettings {
    /// Whether the cursor picks placemarks at all. Off, nothing is hovered and
    /// no halo is drawn; a pin already set stays set.
    pub picking: bool,
    /// What the cursor is over now.
    hovered: Option<Body>,
    /// What an embedder asked to keep, whatever the cursor does afterwards.
    /// This is what a click becomes.
    pinned: Option<Body>,
}

impl Default for PlacemarkSettings {
    fn default() -> Self {
        Self {
            picking: true,
            hovered: None,
            pinned: None,
        }
    }
}

impl PlacemarkSettings {
    /// What the halo should be drawing: the pin if there is one, and otherwise
    /// whatever the cursor is over.
    fn highlight_target(&self) -> Option<Body> {
        self.pinned.or(self.hovered)
    }

    /// Keeps one placemark selected until told otherwise. Returns whether the
    /// globe has a placemark under that name.
    pub fn pin(&mut self, id: &str) -> bool {
        let Some(body) = Body::from_id(id) else {
            return false;
        };
        self.pinned = Some(body);
        true
    }

    pub fn clear_pin(&mut self) {
        self.pinned = None;
    }
}

pub struct PlacemarkPlugin;

impl Plugin for PlacemarkPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<IconMaterial>::default())
            .init_resource::<PlacemarkSettings>()
            .add_systems(Startup, spawn_placemarks)
            .add_systems(
                Update,
                // The clock has already been advanced by the time `Apply` runs,
                // so the points read here are this frame's, not last frame's —
                // and the frame rotation they are placed under has been settled
                // for this tick as well.
                //
                // Then the chain runs downhill from where the icons ended up:
                // what is over the horizon, what the pointer is on, and what
                // that means for the halo. Each step reads the one before it
                // within the tick, because a pick a frame stale highlights the
                // placemark the pointer has just left.
                // A placemark is Earth-fixed and sized to the globe's own
                // scale (see `PLACEMARK_RADIUS`), which is meaningless once
                // `ReferenceFrame::earth_to_world` and the globe it anchors
                // to are no longer what the camera is looking at — so the
                // whole chain stands down in heliocentric view, the same as
                // `crate::tiles` and `crate::vector_tiles` do.
                (
                    track_bodies,
                    place_placemarks,
                    hide_over_the_horizon,
                    pick_placemarks,
                    highlight_placemarks,
                )
                    .chain()
                    .run_if(not_departing_view)
                    .in_set(FrameSet::Apply)
                    .after(crate::api::track_cursor),
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
    // One quad serves every placemark: the mesh is four corners on the origin,
    // and everything that makes one icon different from another is in its
    // material and its transform.
    let quad = meshes.add(icon_quad());

    for body in Body::ALL {
        let coordinate = body.coordinate(&sun, &moon);
        commands.spawn((
            Name::new(format!("{} placemark", body.id())),
            Placemark { body, coordinate },
            Mesh3d(quad.clone()),
            MeshMaterial3d(materials.add(IconMaterial::new(assets.load(body.icon()), ICON_PX))),
            Transform::from_translation(anchor(coordinate)),
            // The quad is spread in the vertex shader from four corners sitting
            // on top of one another, so its own bounds are a point: left to cull
            // itself it would vanish the moment the anchor left the screen, with
            // half the icon still on it.
            NoFrustumCulling,
        ));
    }
}

/// Keeps each placemark on the point its body is overhead.
fn track_bodies(sun: Res<Sun>, moon: Res<Moon>, mut placemarks: Query<&mut Placemark>) {
    for mut placemark in &mut placemarks {
        placemark.coordinate = placemark.body.coordinate(&sun, &moon);
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

/// Hides the placemarks whose point has gone round the back of the globe.
///
/// This is the other half of drawing an icon that ignores the depth buffer —
/// see [`IconMaterial::specialize`]. Nothing in the scene can hide a placemark
/// any more, so the planet has to hide it here instead: an icon is drawn only
/// while its anchor is on the near side of the horizon its own globe cuts.
fn hide_over_the_horizon(
    camera: Query<&GlobalTransform, With<Camera3d>>,
    mut placemarks: Query<(&Transform, &mut Visibility), With<Placemark>>,
) {
    let Some(camera) = camera.iter().next() else {
        return;
    };
    let eye = camera.translation();

    for (transform, mut visibility) in &mut placemarks {
        *visibility = if above_the_horizon(transform.translation, eye) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}

/// Works out which placemark the pointer is over.
///
/// In the viewport, against the rectangle the icon covers there, because that
/// is what is on screen — see the module docs. A placemark already hidden at
/// the horizon is not a target: there is nothing drawn to be pointing at.
fn pick_placemarks(
    cursor: Res<Cursor>,
    camera: Query<(&Camera, &GlobalTransform)>,
    placemarks: Query<(&Placemark, &Transform, &Visibility)>,
    mut settings: ResMut<PlacemarkSettings>,
) {
    let Some((camera, camera_transform)) = camera.iter().next() else {
        return;
    };

    let hovered = settings
        .picking
        .then_some(cursor.screen)
        .flatten()
        .and_then(|pointer| {
            // The icon is sized in device pixels and the pointer arrives in
            // logical ones, so the rectangle has to be stated in the pointer's
            // terms or a retina display would be picked at twice the size.
            let size = ICON_PX / camera.target_scaling_factor().unwrap_or(1.0);

            let mut best: Option<(Body, f32)> = None;
            for (placemark, transform, visibility) in &placemarks {
                if *visibility == Visibility::Hidden {
                    continue;
                }
                let Ok(anchor) = camera.world_to_viewport(camera_transform, transform.translation)
                else {
                    // Behind the camera, or off a viewport with no size. Neither
                    // is anywhere an icon was drawn.
                    continue;
                };
                let Some(distance) = distance_to_icon(pointer, anchor, size) else {
                    continue;
                };
                if best.is_none_or(|(_, held)| distance < held) {
                    best = Some((placemark.body, distance));
                }
            }
            best.map(|(body, _)| body)
        });

    if settings.hovered != hovered {
        settings.hovered = hovered;
    }
}

/// How far the pointer is from the centre of an icon, or `None` when it is
/// outside the icon altogether.
///
/// The icon stands on its anchor, so in viewport coordinates — which count
/// down the screen — it covers the square *above* that point. The slack is the
/// same one an overlay allows: a thirty-two pixel target is generous, but it is
/// a target that is also moving.
fn distance_to_icon(pointer: Vec2, anchor: Vec2, size_px: f32) -> Option<f32> {
    let half = size_px * 0.5 + PICK_SLACK_PX;
    let center = Vec2::new(anchor.x, anchor.y - size_px * 0.5);
    let offset = (pointer - center).abs();
    (offset.x <= half && offset.y <= half).then(|| pointer.distance(center))
}

/// Burns the halo behind whatever is picked, and puts out the rest.
///
/// Written into the material rather than swapped for another one: there is a
/// material per placemark already, and the halo is one number in it.
fn highlight_placemarks(
    settings: Res<PlacemarkSettings>,
    placemarks: Query<(&Placemark, &MeshMaterial3d<IconMaterial>)>,
    mut materials: ResMut<Assets<IconMaterial>>,
) {
    let target = settings.highlight_target();
    for (placemark, material) in &placemarks {
        let halo = if target == Some(placemark.body) {
            if settings.pinned == Some(placemark.body) {
                PINNED_HALO
            } else {
                HOVERED_HALO
            }
        } else {
            0.0
        };
        if let Some(mut material) = materials.get_mut(&material.0)
            && material.uniform.halo != halo
        {
            material.uniform.halo = halo;
        }
    }
}

/// Whether a point on the globe can be seen from the camera.
///
/// The horizon of a sphere of radius `r` as seen from `eye` is the plane
/// `dot(point, eye) == r * r`, so that one product answers it — nearer the eye
/// than the plane and the point is over the edge of the world.
///
/// Measured against the radius the *imagery* reaches rather than the sphere's,
/// because a tile stands proud of the sphere and is what an icon would really
/// disappear behind. The difference is a fraction of a degree of arc; it is
/// there so an icon does not linger for a frame on a horizon the terrain has
/// already crossed.
fn above_the_horizon(point: Vec3, eye: Vec3) -> bool {
    point.dot(eye) >= MAX_TILE_RADIUS * MAX_TILE_RADIUS
}

/// Where a coordinate's icon hangs, in the Earth-fixed frame.
fn anchor(coordinate: LatLon) -> Vec3 {
    coordinate.to_direction() * PLACEMARK_RADIUS
}

/// One quad, all four corners on the anchor. The UV says which corner this is,
/// which is what the vertex shader spreads it by — bottom edge on the
/// coordinate, the rest standing above it — and what the fragment shader reads
/// the icon with.
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
// What the state stream reports
// ---------------------------------------------------------------------------

/// The placemarks, as an interface sees them.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlacemarksState {
    /// Whether the cursor is picking placemarks.
    pub picking: bool,
    /// The placemark under the cursor, if there is one.
    pub hovered: Option<PickedPlacemark>,
    /// The placemark that was pinned, if one was. The globe haloes this in
    /// preference to whatever is hovered, so an interface showing one of them
    /// should prefer it too.
    pub pinned: Option<PickedPlacemark>,
}

/// A placemark the cursor found, and where it stands at this moment.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PickedPlacemark {
    /// `"sun"` or `"moon"` — what [`crate::api::GlobeCommand::PinPlacemark`]
    /// has to be given to name this one again.
    pub body: &'static str,
    pub label: &'static str,
    /// The point the body is overhead, which is where the icon is standing.
    pub coordinate: LatLon,
}

/// What the state snapshot reads the placemarks through: the picks, and the
/// moon — whose coordinate the snapshot needs and nothing else in it carries.
///
/// One parameter rather than two because a system may only take sixteen, and
/// [`crate::api::publish_state`] already spans every controllable part of the
/// globe. Grouping them here also keeps the snapshot from having to know that
/// a placemark's coordinate lives somewhere other than the placemark.
#[derive(SystemParam)]
pub struct PlacemarkPicks<'w> {
    settings: Res<'w, PlacemarkSettings>,
    moon: Res<'w, Moon>,
}

impl PlacemarkPicks<'_> {
    /// Describes both picks, with the coordinate each placemark is on now.
    pub fn describe(&self, sun: &Sun) -> PlacemarksState {
        let describe = |body: Option<Body>| {
            body.map(|body| PickedPlacemark {
                body: body.id(),
                label: body.label(),
                coordinate: body.coordinate(sun, &self.moon),
            })
        };
        PlacemarksState {
            picking: self.settings.picking,
            hovered: describe(self.settings.hovered),
            pinned: describe(self.settings.pinned),
        }
    }

    /// The two picks as the state digest compares them — which placemark,
    /// rather than where it has drifted to since the last frame.
    pub fn digest(&self) -> crate::api::PlacemarkDigest {
        (self.settings.hovered, self.settings.pinned)
    }
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
    /// How strongly the halo behind the icon burns, from nothing to
    /// [`PINNED_HALO`].
    pub halo: f32,
    pub _padding: Vec2,
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
                halo: 0.0,
                _padding: Vec2::ZERO,
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

        // And neither can the depth buffer. A placemark is a flat quad held up
        // to the camera at a point on a curved surface, so the ground it is
        // standing on rises through it from every angle but straight overhead —
        // half an icon at a grazing view, a corner of one near the limb, and
        // the line where it is cut moving as the camera does. Raising the
        // anchor cannot fix that, because the amount the surface rises across
        // the icon is unbounded as the view flattens.
        //
        // So the icon is drawn over the scene rather than into it: never
        // clipped, never half-buried, and always whole. What it gives up is
        // being hidden by anything in front of it — which is only the globe
        // itself, and [`hide_over_the_horizon`] takes that back by hiding a
        // placemark whose point has gone round the far side.
        if let Some(depth_stencil) = descriptor.depth_stencil.as_mut() {
            depth_stencil.depth_compare = Some(CompareFunction::Always);
        }
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
    fn a_point_facing_the_camera_is_drawn() {
        let eye = Vec3::Z * 4.0;
        assert!(above_the_horizon(anchor(LatLon::new(0.0, 0.0)), eye));
        // Well inside the visible cap, and well outside it.
        assert!(above_the_horizon(anchor(LatLon::new(0.0, 60.0)), eye));
        assert!(!above_the_horizon(anchor(LatLon::new(0.0, 120.0)), eye));
        assert!(!above_the_horizon(anchor(LatLon::new(0.0, 180.0)), eye));
    }

    #[test]
    fn the_horizon_closes_in_as_the_camera_drops() {
        // From low down only a small cap is in view; from far off, very nearly
        // a hemisphere. The same coordinate crosses the horizon between the two.
        let low = Vec3::Z * (GLOBE_RADIUS + 0.02);
        let high = Vec3::Z * 60.0;
        let point = anchor(LatLon::new(0.0, 80.0));
        assert!(!above_the_horizon(point, low));
        assert!(above_the_horizon(point, high));
    }

    #[test]
    fn the_icon_is_picked_where_it_is_drawn() {
        // Viewport coordinates count down the screen, so the icon covers the
        // square above its anchor.
        let anchor = Vec2::new(100.0, 200.0);
        let middle = Vec2::new(100.0, 184.0);
        assert!(distance_to_icon(middle, anchor, 32.0).is_some());
        // Its top edge, and well above it.
        assert!(distance_to_icon(Vec2::new(100.0, 170.0), anchor, 32.0).is_some());
        assert!(distance_to_icon(Vec2::new(100.0, 140.0), anchor, 32.0).is_none());
        // Below the anchor is the ground the icon stands on, not the icon.
        assert!(distance_to_icon(Vec2::new(100.0, 230.0), anchor, 32.0).is_none());
        // And off to one side.
        assert!(distance_to_icon(Vec2::new(160.0, 184.0), anchor, 32.0).is_none());
    }

    #[test]
    fn the_nearer_icon_is_the_one_picked() {
        let size = 32.0;
        let pointer = Vec2::new(100.0, 184.0);
        let near = distance_to_icon(pointer, Vec2::new(100.0, 200.0), size).expect("a hit");
        let far = distance_to_icon(pointer, Vec2::new(110.0, 205.0), size).expect("a hit");
        assert!(near < far, "{near} vs {far}");
    }

    #[test]
    fn a_pin_outlives_the_pointer_and_answers_to_its_name() {
        let mut settings = PlacemarkSettings {
            hovered: Some(Body::Sun),
            ..Default::default()
        };
        assert_eq!(settings.highlight_target(), Some(Body::Sun));

        assert!(settings.pin("moon"));
        // Pinned beats hovered, which is what an interface showing one of them
        // has to be able to rely on.
        assert_eq!(settings.highlight_target(), Some(Body::Moon));
        settings.hovered = None;
        assert_eq!(settings.highlight_target(), Some(Body::Moon));

        assert!(!settings.pin("mars"));
        settings.clear_pin();
        assert_eq!(settings.highlight_target(), None);
    }

    #[test]
    fn every_body_is_named_both_ways() {
        for body in Body::ALL {
            assert_eq!(Body::from_id(body.id()), Some(body));
        }
        assert_eq!(Body::from_id("sol"), None);
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
