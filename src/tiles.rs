//! Streams WMS imagery onto the globe as a quadtree of tiles.
//!
//! The grid is the "global geodetic" scheme that WMS implementations serve from
//! `EPSG:4326`: level 0 is two square 180°x180° tiles covering the two halves of
//! the world, and each level quarters them. Level `n` therefore holds
//! `2^(n+1) x 2^n` tiles, and because the projection is plate carrée, a tile's
//! bounding box is a plain latitude/longitude rectangle — the same mapping the
//! base globe's textures already use.
//!
//! Each frame the tree is walked from the roots. A tile is split when its
//! projected size on screen exceeds the resolution of the image behind it, and
//! kept otherwise, so detail follows the camera. Tiles that have not arrived
//! yet fall back to the nearest ancestor that has, and failing that to the base
//! globe underneath, so there is never a hole.

use std::collections::{HashMap, HashSet};

use bevy::asset::{LoadState, RenderAssetUsages};
use bevy::image::{
    ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor,
};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::{GeoBounds, LatLon};
use crate::globe::GLOBE_RADIUS;
use crate::sun::Sun;
use crate::wms::{WMS_SOURCE, WmsSettings};

/// Tiles sit fractionally above the base globe so they never fight it for
/// depth, and each level a fraction higher again so a child always wins over
/// the parent it is replacing. The total offset across every level is under a
/// kilometre of Earth radius.
const TILE_BASE_RADIUS: f32 = GLOBE_RADIUS * 1.0004;
const TILE_LEVEL_STEP: f32 = GLOBE_RADIUS * 2.0e-5;

/// The largest angle a single quad of a tile's mesh may span.
///
/// A flat quad chords across the sphere, so its middle sags below the true
/// surface by `1 - cos(step / 2)`. Bounding the step bounds that sag, which is
/// what keeps the base globe from poking through the imagery drawn over it —
/// a fixed quad count cannot, because a level-0 tile spans 180° and a level-8
/// tile spans 0.7°.
const TILE_MAX_QUAD_DEGREES: f32 = 4.0;
/// Even the smallest tiles keep enough quads to curve.
const TILE_MIN_QUADS: u32 = 4;

/// Split a tile once its image would be stretched beyond this many screen
/// pixels. Slightly above the tile size, which trades a little sharpness for
/// noticeably fewer requests.
const SPLIT_PIXELS: f32 = 320.0;

/// How many requests may be outstanding at once, and how many may be started
/// per frame. Without a ceiling a single fast zoom queues hundreds of tiles
/// that are stale by the time they arrive.
const MAX_TILES_IN_FLIGHT: usize = 24;
const MAX_NEW_REQUESTS_PER_FRAME: usize = 6;

/// Frames a tile may go unused before it is dropped.
const RETIRE_AFTER_FRAMES: u64 = 240;

// ---------------------------------------------------------------------------
// Tile addressing
// ---------------------------------------------------------------------------

/// One tile in the global-geodetic quadtree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub level: u8,
    /// Column, counted east from the antimeridian.
    pub x: u32,
    /// Row, counted south from the north pole.
    pub y: u32,
}

impl TileId {
    /// The two tiles of level 0, each a 180° square.
    pub const ROOTS: [TileId; 2] = [
        TileId {
            level: 0,
            x: 0,
            y: 0,
        },
        TileId {
            level: 0,
            x: 1,
            y: 0,
        },
    ];

    /// Degrees of latitude and longitude spanned by a tile at this level.
    pub fn span_degrees(level: u8) -> f32 {
        180.0 / (1u32 << level) as f32
    }

    pub fn bounds(self) -> GeoBounds {
        let span = Self::span_degrees(self.level);
        let lon_min = -180.0 + self.x as f32 * span;
        let lat_max = 90.0 - self.y as f32 * span;
        GeoBounds {
            lat_min: lat_max - span,
            lat_max,
            lon_min,
            lon_max: lon_min + span,
        }
    }

    pub fn children(self) -> [TileId; 4] {
        let level = self.level + 1;
        let (x, y) = (self.x * 2, self.y * 2);
        [
            TileId { level, x, y },
            TileId { level, x: x + 1, y },
            TileId { level, x, y: y + 1 },
            TileId {
                level,
                x: x + 1,
                y: y + 1,
            },
        ]
    }

    pub fn parent(self) -> Option<TileId> {
        (self.level > 0).then(|| TileId {
            level: self.level - 1,
            x: self.x / 2,
            y: self.y / 2,
        })
    }

    /// The outward direction through the middle of the tile.
    pub fn center_direction(self) -> Vec3 {
        self.bounds().center().to_direction()
    }

    /// Quads per side of this tile's mesh patch.
    fn mesh_quads(self) -> u32 {
        let span = Self::span_degrees(self.level);
        ((span / TILE_MAX_QUAD_DEGREES).ceil() as u32).max(TILE_MIN_QUADS)
    }

    /// Radius the tile's geometry is drawn at.
    ///
    /// The patch is a polygon inscribed in a sphere, so it is pushed outward by
    /// the depth of its deepest sag: that lifts the whole approximation to sit
    /// at or above the intended radius instead of dipping under it. The sag to
    /// correct for is measured across a triangle's diagonal, not a quad's edge
    /// — each quad is drawn as two triangles, and the diagonal spans `sqrt(2)`
    /// times the angle, sagging twice as far.
    fn radius(self) -> f32 {
        let span = Self::span_degrees(self.level);
        let half_diagonal =
            (span / self.mesh_quads() as f32 * 0.5 * std::f32::consts::SQRT_2).to_radians();
        (TILE_BASE_RADIUS + self.level as f32 * TILE_LEVEL_STEP) / half_diagonal.cos()
    }

    /// Longitude the tile sits at relative to the first column, which is the
    /// rotation applied to the shared mesh for its row.
    fn longitude_offset(self) -> f32 {
        (self.x as f32 * Self::span_degrees(self.level)).to_radians()
    }
}

// ---------------------------------------------------------------------------
// Material
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, ShaderType)]
pub struct TileUniform {
    pub sun_direction: Vec3,
    pub rim_strength: f32,
    pub terminator_softness: f32,
    pub _padding: Vec3,
}

impl Default for TileUniform {
    fn default() -> Self {
        Self {
            sun_direction: Vec3::X,
            // Kept deliberately in step with the corresponding constants in
            // `globe.wgsl`, so a tile and the globe beneath it are shaded alike.
            rim_strength: 0.55,
            terminator_softness: 0.12,
            _padding: Vec3::ZERO,
        }
    }
}

#[derive(Asset, AsBindGroup, TypePath, Clone)]
pub struct TileMaterial {
    #[uniform(0)]
    pub uniform: TileUniform,
    #[texture(1)]
    #[sampler(2)]
    pub imagery: Option<Handle<Image>>,
}

impl Material for TileMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/tile.wgsl".into()
    }

    fn enable_shadows() -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileState {
    Pending,
    Ready,
    Failed,
}

struct TileSlot {
    imagery: Handle<Image>,
    material: Option<Handle<TileMaterial>>,
    entity: Option<Entity>,
    state: TileState,
    last_used: u64,
}

#[derive(Resource, Default)]
pub struct TileCache {
    slots: HashMap<TileId, TileSlot>,
    /// Patch geometry, shared by every tile in a level's row. Tiles in the same
    /// row differ only by a rotation about the poles.
    meshes: HashMap<(u8, u32), Handle<Mesh>>,
    frame: u64,
    generation: u32,
    /// Counters for the readout.
    pub visible_tiles: usize,
    pub loading_tiles: usize,
    pub deepest_level: u8,
}

impl TileCache {
    fn is_requested(&self, tile: TileId) -> bool {
        self.slots.contains_key(&tile)
    }

    fn is_ready(&self, tile: TileId) -> bool {
        self.slots
            .get(&tile)
            .is_some_and(|slot| slot.state == TileState::Ready)
    }

    fn mesh_for(&mut self, tile: TileId, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.meshes
            .entry((tile.level, tile.y))
            .or_insert_with(|| meshes.add(tile_patch_mesh(tile)))
            .clone()
    }

    /// Drops every tile, used when the layer changes and the imagery behind
    /// each path is no longer what was cached.
    fn clear(&mut self, commands: &mut Commands, materials: &mut Assets<TileMaterial>) {
        for (_, slot) in self.slots.drain() {
            if let Some(entity) = slot.entity {
                commands.entity(entity).despawn();
            }
            if let Some(material) = slot.material {
                materials.remove(&material);
            }
        }
    }
}

/// Builds the mesh patch for a tile, positioned at the first column of its row.
///
/// Sharing one mesh per row is what keeps the geometry cheap: every tile at the
/// same level and latitude band is the same surface, turned about the axis.
fn tile_patch_mesh(tile: TileId) -> Mesh {
    let bounds = TileId {
        level: tile.level,
        x: 0,
        y: tile.y,
    }
    .bounds();
    let radius = tile.radius();
    let quads = tile.mesh_quads();

    let vertices = ((quads + 1) * (quads + 1)) as usize;
    let mut positions = Vec::with_capacity(vertices);
    let mut normals = Vec::with_capacity(vertices);
    let mut uvs = Vec::with_capacity(vertices);
    let mut indices = Vec::with_capacity((quads * quads * 6) as usize);

    for row in 0..=quads {
        // A WMS image is north-up, so v runs from the top of the box downward.
        let v = row as f32 / quads as f32;
        let latitude = bounds.lat_max - v * bounds.lat_span();

        for column in 0..=quads {
            let u = column as f32 / quads as f32;
            let longitude = bounds.lon_min + u * bounds.lon_span();

            let normal = LatLon::new(latitude, longitude).to_direction();
            positions.push((normal * radius).to_array());
            normals.push(normal.to_array());
            uvs.push([u, v]);
        }
    }

    let stride = quads + 1;
    for row in 0..quads {
        for column in 0..quads {
            let top_left = row * stride + column;
            let top_right = top_left + 1;
            let bottom_left = top_left + stride;
            let bottom_right = bottom_left + 1;
            indices.extend_from_slice(&[top_left, bottom_left, top_right]);
            indices.extend_from_slice(&[top_right, bottom_left, bottom_right]);
        }
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

struct SelectionContext {
    camera_position: Vec3,
    camera_direction: Vec3,
    camera_distance: f32,
    /// Pixels per radian at the centre of the view, which converts a world-space
    /// size at a given distance into a size on screen.
    focal_pixels: f32,
    max_level: u8,
}

impl SelectionContext {
    /// Roughly how many pixels across the tile covers.
    fn projected_pixels(&self, tile: TileId) -> f32 {
        let bounds = tile.bounds();
        let center = tile.center_direction() * GLOBE_RADIUS;
        let extent = bounds.lat_span().to_radians() * GLOBE_RADIUS;
        let distance = (self.camera_position - center).length().max(1.0e-3);
        extent / distance * self.focal_pixels
    }

    /// Whether any part of the tile can be over the horizon.
    fn is_visible(&self, tile: TileId) -> bool {
        // The first couple of levels are so large that a corner test is
        // meaningless — a hemisphere always has a visible part.
        if tile.level <= 1 {
            return true;
        }

        let bounds = tile.bounds();
        // A point is over the horizon when its dot product with the view
        // direction exceeds the ratio of the globe's radius to the camera's
        // distance; widen that by the tile's own angular size so a tile is not
        // culled while one edge is still in view.
        let angular_radius = bounds.lat_span().to_radians() * 0.75;
        let horizon = GLOBE_RADIUS / self.camera_distance - (1.0 - angular_radius.cos()) - 0.02;

        let corners = [
            LatLon::new(bounds.lat_min, bounds.lon_min),
            LatLon::new(bounds.lat_min, bounds.lon_max),
            LatLon::new(bounds.lat_max, bounds.lon_min),
            LatLon::new(bounds.lat_max, bounds.lon_max),
            bounds.center(),
        ];
        corners
            .iter()
            .any(|corner| corner.to_direction().dot(self.camera_direction) > horizon)
    }
}

/// Walks the quadtree, collecting what should be drawn and what should be asked for.
fn select_tiles(
    tile: TileId,
    context: &SelectionContext,
    cache: &TileCache,
    selected: &mut HashSet<TileId>,
    wanted: &mut Vec<TileId>,
) {
    if !context.is_visible(tile) {
        return;
    }

    if tile.level < context.max_level && context.projected_pixels(tile) > SPLIT_PIXELS {
        for child in tile.children() {
            select_tiles(child, context, cache, selected, wanted);
        }
        return;
    }

    // This is as deep as the view needs to go. Draw the finest imagery that has
    // actually arrived along this branch, requesting anything missing on the
    // way up; if nothing has, the base globe shows through.
    let mut candidate = Some(tile);
    while let Some(current) = candidate {
        if cache.is_ready(current) {
            selected.insert(current);
            return;
        }
        if !cache.is_requested(current) {
            wanted.push(current);
        }
        candidate = current.parent();
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Marks an entity that draws one tile, and records which one, so the tiles in
/// the world can be inspected or queried by address.
#[derive(Component)]
pub struct TileEntity(pub TileId);

pub struct TilePlugin;

impl Plugin for TilePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<TileMaterial>::default())
            .init_resource::<TileCache>()
            .add_systems(
                Update,
                (tile_controls, stream_tiles, orient_tiles, sync_tile_sun)
                    .chain()
                    .in_set(FrameSet::Apply),
            );
    }
}

fn tile_controls(keys: Res<ButtonInput<KeyCode>>, mut settings: ResMut<WmsSettings>) {
    if keys.just_pressed(KeyCode::KeyT) {
        settings.enabled = !settings.enabled;
    }
    if keys.just_pressed(KeyCode::KeyL) {
        settings.cycle_preset();
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "streaming touches the camera, the cache and three asset collections; splitting it would only move the argument list"
)]
fn stream_tiles(
    mut commands: Commands,
    // The camera's own transform, not its global one: the global is a tick
    // behind, and a tick behind is exactly wrong on the tick the frame
    // switches, when the camera and the Earth move together.
    camera: Single<(&Camera, &Transform, &Projection)>,
    frame: Res<ReferenceFrame>,
    settings: Res<WmsSettings>,
    mut cache: ResMut<TileCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<TileMaterial>>,
    mut visibilities: Query<&mut Visibility, With<TileEntity>>,
    asset_server: Res<AssetServer>,
) {
    cache.frame += 1;

    // A new layer invalidates every cached path, so start over.
    if cache.generation != settings.generation() {
        cache.clear(&mut commands, &mut materials);
        cache.generation = settings.generation();
    }

    if !settings.enabled {
        if !cache.slots.is_empty() {
            cache.clear(&mut commands, &mut materials);
        }
        cache.visible_tiles = 0;
        cache.loading_tiles = 0;
        cache.deepest_level = 0;
        return;
    }

    // Promote anything that finished loading since the last frame.
    for (_, slot) in cache.slots.iter_mut() {
        if slot.state != TileState::Pending {
            continue;
        }
        match asset_server.get_load_state(&slot.imagery) {
            Some(LoadState::Loaded) => slot.state = TileState::Ready,
            Some(LoadState::Failed(_)) => slot.state = TileState::Failed,
            _ => {}
        }
    }

    let (camera_component, camera_transform, projection) = *camera;
    // The quadtree is addressed in latitude and longitude, so the walk has to
    // happen in Earth-fixed coordinates however the world is turned.
    let earth_to_world = frame.earth_to_world();
    let camera_position = earth_to_world.inverse() * camera_transform.translation;
    let camera_distance = camera_position.length().max(GLOBE_RADIUS * 1.0001);
    let field_of_view = match projection {
        Projection::Perspective(perspective) => perspective.fov,
        _ => 45.0_f32.to_radians(),
    };
    let viewport_height = camera_component
        .logical_viewport_size()
        .map(|size| size.y)
        .unwrap_or(1080.0);

    let context = SelectionContext {
        camera_position,
        camera_direction: camera_position / camera_distance,
        camera_distance,
        focal_pixels: viewport_height / (2.0 * (field_of_view * 0.5).tan()),
        max_level: settings.max_level(),
    };

    let mut selected = HashSet::new();
    let mut wanted = Vec::new();
    for root in TileId::ROOTS {
        select_tiles(root, &context, &cache, &mut selected, &mut wanted);
    }

    // Coarse tiles first: they cover the most ground per request, so they are
    // what turns an empty view into a complete one soonest.
    wanted.sort_unstable();
    wanted.dedup();

    let mut in_flight = cache
        .slots
        .values()
        .filter(|slot| slot.state == TileState::Pending)
        .count();
    let mut issued = 0;
    let generation = cache.generation;
    let requested_at = cache.frame;
    let extension = settings.tile_extension();

    for tile in wanted {
        if issued >= MAX_NEW_REQUESTS_PER_FRAME || in_flight >= MAX_TILES_IN_FLIGHT {
            break;
        }
        if cache.slots.contains_key(&tile) {
            continue;
        }

        let path = format!(
            "{WMS_SOURCE}://{generation}/{}/{}/{}.{extension}",
            tile.level, tile.x, tile.y
        );
        // Tiles butt up against each other, so their edges must clamp; the
        // globe's own textures wrap, and that default would bleed the far edge
        // of every tile into its neighbour.
        let imagery = asset_server
            .load_builder()
            .with_settings(|settings: &mut ImageLoaderSettings| {
                settings.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::ClampToEdge,
                    address_mode_v: ImageAddressMode::ClampToEdge,
                    mag_filter: ImageFilterMode::Linear,
                    min_filter: ImageFilterMode::Linear,
                    ..default()
                });
            })
            .load(path);

        cache.slots.insert(
            tile,
            TileSlot {
                imagery,
                material: None,
                entity: None,
                state: TileState::Pending,
                last_used: requested_at,
            },
        );
        in_flight += 1;
        issued += 1;
    }

    // Give every selected tile an entity, and show only what was selected.
    let frame = cache.frame;
    let mut deepest = 0;
    for tile in &selected {
        deepest = deepest.max(tile.level);
    }

    let ready_selection: Vec<TileId> = selected
        .iter()
        .copied()
        .filter(|tile| cache.is_ready(*tile))
        .collect();

    for tile in ready_selection {
        let mesh = cache.mesh_for(tile, &mut meshes);
        let Some(slot) = cache.slots.get_mut(&tile) else {
            continue;
        };
        slot.last_used = frame;

        if slot.entity.is_none() {
            let material = materials.add(TileMaterial {
                uniform: TileUniform::default(),
                imagery: Some(slot.imagery.clone()),
            });
            let entity = commands
                .spawn((
                    Name::new(format!("Tile {}/{}/{}", tile.level, tile.x, tile.y)),
                    TileEntity(tile),
                    Mesh3d(mesh),
                    MeshMaterial3d(material.clone()),
                    // `orient_tiles` keeps this in step with the frame; the
                    // spawn value only has to be right for the frame it is
                    // spawned into.
                    Transform::from_rotation(
                        earth_to_world * Quat::from_rotation_y(tile.longitude_offset()),
                    ),
                ))
                .id();
            slot.material = Some(material);
            slot.entity = Some(entity);
        }
    }

    let mut visible_count = 0;
    let mut loading_count = 0;
    for (tile, slot) in cache.slots.iter() {
        if slot.state == TileState::Pending {
            loading_count += 1;
        }
        let Some(entity) = slot.entity else {
            continue;
        };
        let Ok(mut visibility) = visibilities.get_mut(entity) else {
            continue;
        };
        if selected.contains(tile) {
            *visibility = Visibility::Inherited;
            visible_count += 1;
        } else {
            *visibility = Visibility::Hidden;
        }
    }

    // Retire whatever the view has not wanted for a while.
    let mut retired = Vec::new();
    for (tile, slot) in cache.slots.iter() {
        if slot.last_used + RETIRE_AFTER_FRAMES < frame {
            retired.push(*tile);
        }
    }
    for tile in retired {
        if let Some(slot) = cache.slots.remove(&tile) {
            if let Some(entity) = slot.entity {
                commands.entity(entity).despawn();
            }
            if let Some(material) = slot.material {
                materials.remove(&material);
            }
        }
    }

    cache.visible_tiles = visible_count;
    cache.loading_tiles = loading_count;
    cache.deepest_level = deepest;
}

/// Carries the tiles around with the globe they sit on.
///
/// Each tile's own rotation places it in its row; the frame rotation is what
/// turns the whole Earth-fixed grid into world space, and both are rotations
/// about the poles, so they simply compose.
fn orient_tiles(frame: Res<ReferenceFrame>, mut tiles: Query<(&TileEntity, &mut Transform)>) {
    let earth_to_world = frame.earth_to_world();
    for (tile, mut transform) in &mut tiles {
        transform.rotation = earth_to_world * Quat::from_rotation_y(tile.0.longitude_offset());
    }
}

fn sync_tile_sun(
    sun: Res<Sun>,
    frame: Res<ReferenceFrame>,
    mut materials: ResMut<Assets<TileMaterial>>,
) {
    let sun_direction = frame.earth_to_world() * sun.direction_ecef;
    for (_, material) in materials.iter_mut() {
        material.uniform.sun_direction = sun_direction;
    }
}
