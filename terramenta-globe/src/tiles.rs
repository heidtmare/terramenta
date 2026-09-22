//! Streams imagery onto the globe as a quadtree of tiles.
//!
//! Both supported protocols cut the world into a pyramid of square images, but
//! disagree on where the cuts fall, so the grid is described by a
//! [`TileGrid`] rather than hard-coded. A WMS layer uses the tidy grid — level
//! 0 is two 180°x180° tiles covering the two halves of the world, each level
//! quartering them — because WMS renders any rectangle asked of it. A WMTS
//! layer must take whatever tile matrix set the server publishes, which is
//! often untidy: NASA GIBS starts at two 288° tiles, so its coarse levels
//! overhang the world and its matrix widths run 2, 3, 5, 10, 20 instead of
//! doubling. [`TileGrid::clipped_bounds`] reconciles the two by drawing only
//! the part of a tile that lands on the globe.
//!
//! Every grid here is plate carrée, so a tile's bounding box is a plain
//! latitude/longitude rectangle — the same mapping the base globe's textures
//! use, requiring no reprojection.
//!
//! Each frame the tree is walked from the roots. A tile is split when its
//! projected screen size exceeds the resolution of the image behind it, and
//! kept otherwise, so detail follows the camera. Tiles that have not arrived
//! yet fall back to the nearest ancestor that has, and failing that to the
//! base globe underneath, so there is never a hole.

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
use crate::imagery::{IMAGERY_SOURCE, ImagerySettings};
use crate::sun::Sun;
use crate::view::not_departing_view;

/// Tiles sit fractionally above the base globe so they never fight it for
/// depth, and each level a fraction higher again so a child always wins over
/// the parent it is replacing. The total offset across every level is under a
/// kilometre of Earth radius.
const TILE_BASE_RADIUS: f32 = GLOBE_RADIUS * 1.0004;
const TILE_LEVEL_STEP: f32 = GLOBE_RADIUS * 2.0e-5;

/// The highest [`TileGrid::radius`] ever puts a tile, over every level of every
/// grid — which is not the deepest level, because the sag correction below
/// dominates the per-level step and it is largest where a quad is widest.
///
/// It matters outside this module: anything else drawn on the surface has to
/// clear it or the imagery will bury it. `assert_no_tile_rises_above_the_stated_ceiling`
/// is what keeps this honest.
pub const MAX_TILE_RADIUS: f32 = GLOBE_RADIUS * 1.0017;

/// The largest angle a single quad of a tile's mesh may span.
///
/// A flat quad chords across the sphere, so its middle sags below the true
/// surface by `1 - cos(step / 2)`. Bounding the step bounds that sag, which is
/// what keeps the base globe from poking through the imagery drawn over it —
/// a fixed quad count cannot, because a level-0 tile spans 180° or more and a
/// level-8 tile spans well under a degree.
const TILE_MAX_QUAD_DEGREES: f32 = 4.0;
/// Even the smallest tiles keep enough quads to curve.
const TILE_MIN_QUADS: u32 = 4;

/// Split a tile once its image is being stretched past this multiple of its own
/// resolution. Slightly over one, which trades a little sharpness for
/// noticeably fewer requests.
///
/// Expressed as a ratio rather than a pixel count because tile sizes differ
/// between layers: GIBS serves 512-pixel tiles over WMTS where the WMS requests
/// here ask for 256, and a fixed threshold would over-split the larger ones
/// fourfold.
const SPLIT_FACTOR: f32 = 1.25;

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

/// The address of one tile, in the terms WMTS names them: a level, a column and
/// a row. What rectangle it stands for is a question for the [`TileGrid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub level: u8,
    /// Column, counted east from the grid's western edge.
    pub x: u32,
    /// Row, counted south from the grid's northern edge.
    pub y: u32,
}

impl TileId {
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
}

/// The tiling scheme a layer's imagery is cut into — WMTS calls this a tile
/// matrix set.
///
/// Two numbers describe every plate-carrée pyramid in practice: the north-west
/// corner tile `(0, 0)` starts at, and how many degrees one level-0 tile spans.
/// Each level halves the span, and the matrix is however many tiles of that
/// span it takes to reach the far edge of the world — which is why a grid whose
/// span does not divide the world evenly has matrix widths that do not simply
/// double, and tiles at the south and east edges that hang off it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileGrid {
    /// The north-west corner the grid is measured from.
    pub origin: LatLon,
    /// Degrees of latitude and longitude one level-0 tile spans.
    pub level0_span: f32,
}

impl Default for TileGrid {
    fn default() -> Self {
        Self::GEODETIC
    }
}

impl TileGrid {
    /// The global geodetic quadtree: two 180° tiles at level 0, quartered at
    /// every level below, so level `n` holds `2^(n+1) x 2^n` tiles and nothing
    /// ever overhangs. What a client picks when it is free to.
    pub const GEODETIC: Self = Self {
        origin: LatLon::new(90.0, -180.0),
        level0_span: 180.0,
    };

    /// Builds a grid from the numbers a WMTS capabilities document states for
    /// level 0 of a tile matrix set.
    ///
    /// WMTS fixes a level's resolution with a *scale denominator* rather than a
    /// tile span: a pixel is the denominator multiplied by a standardised
    /// 0.28 mm, and for a geographic coordinate system that length is then read
    /// as degrees through a fixed metres-per-degree. Doing that arithmetic here
    /// means a matrix set can be transcribed from its capabilities document
    /// without working the span out by hand.
    pub fn from_scale_denominator(
        origin: LatLon,
        level0_scale_denominator: f64,
        tile_size: u32,
    ) -> Self {
        /// The pixel size the standard assumes, in metres.
        const STANDARD_PIXEL_METRES: f64 = 0.000_28;
        /// The metres-per-degree the standard pins a geographic CRS to.
        const METRES_PER_DEGREE: f64 = 111_319.490_793_273_58;

        let degrees_per_pixel =
            level0_scale_denominator * STANDARD_PIXEL_METRES / METRES_PER_DEGREE;
        Self {
            origin,
            level0_span: (degrees_per_pixel * f64::from(tile_size)) as f32,
        }
    }

    /// Degrees of latitude and longitude spanned by a tile at this level.
    pub fn span(self, level: u8) -> f32 {
        self.level0_span / (1u32 << level) as f32
    }

    /// Columns and rows the matrix at this level holds — however many tiles it
    /// takes to cover the world from the origin, rounded up.
    pub fn matrix_size(self, level: u8) -> (u32, u32) {
        let span = self.span(level);
        let columns = ((GeoBounds::WORLD.lon_max - self.origin.lon) / span).ceil();
        let rows = ((self.origin.lat - GeoBounds::WORLD.lat_min) / span).ceil();
        ((columns as u32).max(1), (rows as u32).max(1))
    }

    /// The tiles of level 0, which the selection walk descends from.
    pub fn roots(self) -> Vec<TileId> {
        let (columns, rows) = self.matrix_size(0);
        (0..rows)
            .flat_map(|y| (0..columns).map(move |x| TileId { level: 0, x, y }))
            .collect()
    }

    /// The full rectangle a tile's image covers, which near the edges of an
    /// untidy grid runs off the world.
    pub fn bounds(self, tile: TileId) -> GeoBounds {
        let span = self.span(tile.level);
        let lon_min = self.origin.lon + tile.x as f32 * span;
        let lat_max = self.origin.lat - tile.y as f32 * span;
        GeoBounds {
            lat_min: lat_max - span,
            lat_max,
            lon_min,
            lon_max: lon_min + span,
        }
    }

    /// The part of that rectangle that is actually on the globe, or `None` for
    /// a tile that lies entirely off it.
    ///
    /// This is the only thing standing between a grid like GIBS's and a level-0
    /// tile being drawn 288° wide, wrapping most of the way round the planet a
    /// second time.
    pub fn clipped_bounds(self, tile: TileId) -> Option<GeoBounds> {
        self.bounds(tile).intersect(GeoBounds::WORLD)
    }

    /// Quads per side a full tile is divided into.
    fn quads_per_side(self, level: u8) -> u32 {
        ((self.span(level) / TILE_MAX_QUAD_DEGREES).ceil() as u32).max(TILE_MIN_QUADS)
    }

    /// The angle one quad spans, which is what bounds the sag of the mesh.
    ///
    /// Held the same for every tile at a level, including the clipped ones: a
    /// clipped tile gets fewer quads rather than smaller ones, so its geometry
    /// meets its neighbours' exactly and no crack opens along the join.
    fn quad_degrees(self, level: u8) -> f32 {
        self.span(level) / self.quads_per_side(level) as f32
    }

    /// Radius the tiles of a level are drawn at.
    ///
    /// The patch is a polygon inscribed in a sphere, so it is pushed outward by
    /// the depth of its deepest sag: that lifts the whole approximation to sit
    /// at or above the intended radius instead of dipping under it. The sag to
    /// correct for is measured across a triangle's diagonal, not a quad's edge
    /// — each quad is drawn as two triangles, and the diagonal spans `sqrt(2)`
    /// times the angle, sagging twice as far.
    fn radius(self, level: u8) -> f32 {
        let half_diagonal =
            (self.quad_degrees(level) * 0.5 * std::f32::consts::SQRT_2).to_radians();
        (TILE_BASE_RADIUS + level as f32 * TILE_LEVEL_STEP) / half_diagonal.cos()
    }

    /// Longitude the tile sits at relative to the first column, which is the
    /// rotation applied to the shared mesh for its row.
    fn longitude_offset(self, tile: TileId) -> f32 {
        (tile.x as f32 * self.span(tile.level)).to_radians()
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
    /// Matches [`crate::globe::GlobeUniform::sun_shading`].
    pub sun_shading: f32,
    pub _padding: Vec2,
}

impl Default for TileUniform {
    fn default() -> Self {
        Self {
            sun_direction: Vec3::X,
            // Kept deliberately in step with the corresponding constants in
            // `globe.wgsl`, so a tile and the globe beneath it are shaded alike.
            rim_strength: 0.55,
            terminator_softness: 0.12,
            sun_shading: 1.0,
            _padding: Vec2::ZERO,
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
    /// Patch geometry, shared by every tile that has the same shape. Tiles in
    /// the same row of a level differ only by a rotation about the poles — save
    /// where the grid overhangs the world and the last column is clipped
    /// narrower, which is what the longitude span in the key distinguishes.
    meshes: HashMap<MeshKey, Handle<Mesh>>,
    /// The grid the cached tiles were addressed in.
    grid: TileGrid,
    frame: u64,
    generation: u32,
    /// Counters for the readout.
    pub visible_tiles: usize,
    pub loading_tiles: usize,
    pub deepest_level: u8,
}

/// Level, row, and the drawn longitude span in raw bits — `f32` is not `Hash`,
/// and the spans being compared are computed identically rather than merely
/// closely, so their bit patterns match exactly.
type MeshKey = (u8, u32, u32);

impl TileCache {
    fn is_requested(&self, tile: TileId) -> bool {
        self.slots.contains_key(&tile)
    }

    fn is_ready(&self, tile: TileId) -> bool {
        self.slots
            .get(&tile)
            .is_some_and(|slot| slot.state == TileState::Ready)
    }

    fn mesh_for(
        &mut self,
        tile: TileId,
        drawn: GeoBounds,
        meshes: &mut Assets<Mesh>,
    ) -> Handle<Mesh> {
        let grid = self.grid;
        let lon_span = drawn.lon_span();
        self.meshes
            .entry((tile.level, tile.y, lon_span.to_bits()))
            .or_insert_with(|| meshes.add(tile_patch_mesh(grid, tile.level, tile.y, lon_span)))
            .clone()
    }

    /// Drops every tile, used when the layer changes and neither the imagery
    /// behind each path nor the grid it was addressed in is what was cached.
    fn clear(&mut self, commands: &mut Commands, materials: &mut Assets<TileMaterial>) {
        for (_, slot) in self.slots.drain() {
            if let Some(entity) = slot.entity {
                commands.entity(entity).despawn();
            }
            if let Some(material) = slot.material {
                materials.remove(&material);
            }
        }
        self.meshes.clear();
    }
}

/// Builds the mesh patch for the tiles of one shape, positioned at the first
/// column of their row.
///
/// Sharing one mesh per row is what keeps the geometry cheap: every tile at the
/// same level and latitude band is the same surface, turned about the axis.
/// `lon_span` is how much of the tile is drawn, which is the full span except
/// where the grid runs off the eastern edge of the world.
fn tile_patch_mesh(grid: TileGrid, level: u8, row: u32, lon_span: f32) -> Mesh {
    let full = grid.bounds(TileId {
        level,
        x: 0,
        y: row,
    });
    // Latitude is clipped identically for every tile in the row.
    let lat_max = full.lat_max.min(GeoBounds::WORLD.lat_max);
    let lat_min = full.lat_min.max(GeoBounds::WORLD.lat_min);
    let lat_span = lat_max - lat_min;

    let radius = grid.radius(level);
    let quad = grid.quad_degrees(level);
    let columns = ((lon_span / quad).ceil() as u32).max(1);
    let rows = ((lat_span / quad).ceil() as u32).max(1);

    let vertices = ((columns + 1) * (rows + 1)) as usize;
    let mut positions = Vec::with_capacity(vertices);
    let mut normals = Vec::with_capacity(vertices);
    let mut uvs = Vec::with_capacity(vertices);
    let mut indices = Vec::with_capacity((columns * rows * 6) as usize);

    for row_index in 0..=rows {
        let latitude = lat_max - row_index as f32 / rows as f32 * lat_span;
        // Imagery is north-up, and the texture covers the tile's *full* extent
        // whether or not all of it is drawn, so the coordinate is measured
        // against the unclipped box.
        let v = (full.lat_max - latitude) / full.lat_span();

        for column in 0..=columns {
            let longitude = full.lon_min + column as f32 / columns as f32 * lon_span;
            let u = (longitude - full.lon_min) / full.lon_span();

            let normal = LatLon::new(latitude, longitude).to_direction();
            positions.push((normal * radius).to_array());
            normals.push(normal.to_array());
            uvs.push([u, v]);
        }
    }

    let stride = columns + 1;
    for row_index in 0..rows {
        for column in 0..columns {
            let top_left = row_index * stride + column;
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
    grid: TileGrid,
    /// Edge length in pixels of the imagery behind one tile.
    tile_size: u32,
    max_level: u8,
}

impl SelectionContext {
    /// Roughly how many pixels across the drawn part of the tile covers.
    fn projected_pixels(&self, drawn: GeoBounds) -> f32 {
        let center = drawn.center().to_direction() * GLOBE_RADIUS;
        let extent = drawn.lat_span().to_radians() * GLOBE_RADIUS;
        let distance = (self.camera_position - center).length().max(1.0e-3);
        extent / distance * self.focal_pixels
    }

    /// Whether the tile's imagery is being stretched far enough to be worth
    /// replacing with its four children.
    ///
    /// Both sides of the comparison are scaled by how much of the tile is
    /// drawn: a tile clipped to a third of its height is also only carrying a
    /// third of its pixels there, so the ratio is what stays meaningful.
    fn should_split(&self, tile: TileId, drawn: GeoBounds) -> bool {
        let drawn_fraction = drawn.lat_span() / self.grid.span(tile.level);
        let image_pixels = self.tile_size as f32 * drawn_fraction;
        self.projected_pixels(drawn) > image_pixels * SPLIT_FACTOR
    }

    /// Whether any part of the tile can be over the horizon.
    fn is_visible(&self, drawn: GeoBounds) -> bool {
        // A tile spanning a quarter-turn or more always has a visible part, and
        // a corner test on one is meaningless.
        if drawn.lat_span() >= 90.0 {
            return true;
        }

        // A point is over the horizon when its dot product with the view
        // direction exceeds the ratio of the globe's radius to the camera's
        // distance; widen that by the tile's own angular size so a tile is not
        // culled while one edge is still in view.
        let angular_radius = drawn.lat_span().to_radians() * 0.75;
        let horizon = GLOBE_RADIUS / self.camera_distance - (1.0 - angular_radius.cos()) - 0.02;

        let corners = [
            LatLon::new(drawn.lat_min, drawn.lon_min),
            LatLon::new(drawn.lat_min, drawn.lon_max),
            LatLon::new(drawn.lat_max, drawn.lon_min),
            LatLon::new(drawn.lat_max, drawn.lon_max),
            drawn.center(),
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
    // A tile entirely off the world is one the grid defines but the server does
    // not hold; descending into it would only earn errors.
    let Some(drawn) = context.grid.clipped_bounds(tile) else {
        return;
    };
    if !context.is_visible(drawn) {
        return;
    }

    if tile.level < context.max_level && context.should_split(tile, drawn) {
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
                (
                    tile_controls.run_if(crate::api::keyboard_enabled),
                    // The heliocentric view draws no imagery — the globe it
                    // would drape onto is hidden and a few Earth radii across,
                    // invisible against a camera parked astronomical units
                    // away — so streaming stands down rather than walking a
                    // quadtree against a camera transform the heliocentric
                    // rig, not this one, is now driving. It stands down from
                    // the moment the departure begins, not just once it has
                    // settled, since `crate::view::drop_departure_clutter`
                    // hides the tiles right away and would otherwise lose
                    // that race with this system's own per-tick visibility.
                    (stream_tiles, orient_tiles, sync_tile_sun)
                        .chain()
                        .run_if(not_departing_view),
                )
                    .chain()
                    .in_set(FrameSet::Apply),
            );
    }
}

fn tile_controls(keys: Res<ButtonInput<KeyCode>>, mut settings: ResMut<ImagerySettings>) {
    if keys.just_pressed(KeyCode::KeyT) {
        settings.enabled = !settings.enabled;
    }
    if keys.just_pressed(KeyCode::KeyL) {
        if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
            settings.cycle_preset_back();
        } else {
            settings.cycle_preset();
        }
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
    settings: Res<ImagerySettings>,
    mut cache: ResMut<TileCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<TileMaterial>>,
    mut visibilities: Query<&mut Visibility, With<TileEntity>>,
    asset_server: Res<AssetServer>,
) {
    cache.frame += 1;
    // Cached here rather than read from behind the layer's lock on every use,
    // and by `orient_tiles` after this system has run.
    cache.grid = settings.grid();

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
        grid: cache.grid,
        tile_size: settings.tile_size(),
        max_level: settings.max_level(),
    };

    let mut selected = HashSet::new();
    let mut wanted = Vec::new();
    for root in context.grid.roots() {
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
            "{IMAGERY_SOURCE}://{generation}/{}/{}/{}.{extension}",
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
    let grid = cache.grid;
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
        let Some(drawn) = grid.clipped_bounds(tile) else {
            continue;
        };
        let mesh = cache.mesh_for(tile, drawn, &mut meshes);
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
                        earth_to_world * Quat::from_rotation_y(grid.longitude_offset(tile)),
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
fn orient_tiles(
    frame: Res<ReferenceFrame>,
    cache: Res<TileCache>,
    mut tiles: Query<(&TileEntity, &mut Transform)>,
) {
    let earth_to_world = frame.earth_to_world();
    for (tile, mut transform) in &mut tiles {
        transform.rotation =
            earth_to_world * Quat::from_rotation_y(cache.grid.longitude_offset(tile.0));
    }
}

fn sync_tile_sun(
    sun: Res<Sun>,
    frame: Res<ReferenceFrame>,
    mut materials: ResMut<Assets<TileMaterial>>,
) {
    let sun_direction = frame.earth_to_world() * sun.direction_ecef;
    let sun_shading = sun.shading();
    for (_, material) in materials.iter_mut() {
        material.uniform.sun_direction = sun_direction;
        material.uniform.sun_shading = sun_shading;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid NASA GIBS publishes its `EPSG:4326` imagery in.
    fn gibs_grid() -> TileGrid {
        TileGrid::from_scale_denominator(LatLon::new(90.0, -180.0), 223_632_905.611_487_1, 512)
    }

    #[test]
    fn assert_no_tile_rises_above_the_stated_ceiling() {
        for grid in [TileGrid::GEODETIC, gibs_grid()] {
            for level in 0..=12 {
                let radius = grid.radius(level);
                assert!(
                    radius <= MAX_TILE_RADIUS,
                    "level {level} reaches {radius}, past {MAX_TILE_RADIUS}"
                );
            }
        }
    }

    #[test]
    fn the_geodetic_grid_is_a_plain_quadtree() {
        let grid = TileGrid::GEODETIC;
        assert_eq!(grid.matrix_size(0), (2, 1));
        assert_eq!(grid.matrix_size(1), (4, 2));
        assert_eq!(grid.matrix_size(8), (512, 256));

        // Nothing in it overhangs, so clipping is a no-op.
        let tile = TileId {
            level: 0,
            x: 0,
            y: 0,
        };
        assert_eq!(grid.clipped_bounds(tile), Some(grid.bounds(tile)));
        assert_eq!(
            grid.bounds(tile),
            GeoBounds {
                lat_min: -90.0,
                lat_max: 90.0,
                lon_min: -180.0,
                lon_max: 0.0,
            }
        );
    }

    #[test]
    fn a_scale_denominator_recovers_the_gibs_tile_span() {
        assert!((gibs_grid().level0_span - 288.0).abs() < 1.0e-3);
    }

    #[test]
    fn the_gibs_matrix_widths_match_its_capabilities_document() {
        // 2, 3, 5, 10, 20, 40 across and 1, 2, 3, 5, 10, 20 down — the sizes the
        // server reports, and asking for a column past them is an error rather
        // than an empty tile.
        let grid = gibs_grid();
        let sizes: Vec<(u32, u32)> = (0..6).map(|level| grid.matrix_size(level)).collect();
        assert_eq!(
            sizes,
            vec![(2, 1), (3, 2), (5, 3), (10, 5), (20, 10), (40, 20)]
        );
    }

    #[test]
    fn overhanging_tiles_are_clipped_to_the_world() {
        let grid = gibs_grid();

        // Level 0 column 0 runs from 180° W to 108° E and from the north pole
        // to 198° S; only the northern 180° of it is real.
        let clipped = grid
            .clipped_bounds(TileId {
                level: 0,
                x: 0,
                y: 0,
            })
            .expect("the first tile is on the world");
        assert_eq!(clipped, GeoBounds::WORLD.intersect(clipped).unwrap());
        assert!((clipped.lat_min - -90.0).abs() < 1.0e-3);
        assert!((clipped.lon_max - 108.0).abs() < 1.0e-3);

        // Column 1 starts at 108° E, so only 72° of its 288° is on the world.
        let clipped = grid
            .clipped_bounds(TileId {
                level: 0,
                x: 1,
                y: 0,
            })
            .expect("the second tile is partly on the world");
        assert!((clipped.lon_span() - 72.0).abs() < 1.0e-3);

        // Level 1 has three columns; the fourth, which the quadtree walk would
        // otherwise descend into, is entirely off the world.
        assert!(
            grid.clipped_bounds(TileId {
                level: 1,
                x: 3,
                y: 0
            })
            .is_none()
        );
    }

    #[test]
    fn clipping_never_drops_a_tile_the_matrix_still_holds() {
        // The two ways of knowing whether a tile exists have to agree, because
        // only one of them is consulted during the walk.
        for grid in [TileGrid::GEODETIC, gibs_grid()] {
            for level in 0..6u8 {
                let (columns, rows) = grid.matrix_size(level);
                for x in 0..columns + 1 {
                    for y in 0..rows + 1 {
                        let inside = x < columns && y < rows;
                        let tile = TileId { level, x, y };
                        assert_eq!(
                            grid.clipped_bounds(tile).is_some(),
                            inside,
                            "level {level} tile {x},{y}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_clipped_tile_keeps_its_neighbours_quad_size() {
        // Different quad counts are fine; a different quad *angle* would put the
        // two patches at different radii and open a crack between them.
        let grid = gibs_grid();
        for level in 0..8u8 {
            assert!(grid.quad_degrees(level) <= TILE_MAX_QUAD_DEGREES + 1.0e-4);
        }
        assert_eq!(grid.radius(0), grid.radius(0));
    }

    #[test]
    fn tile_texture_coordinates_cover_the_drawn_part_of_the_image() {
        // A level-0 GIBS tile is 288° tall but only its top 180° is on the
        // globe, so the mesh must stop at v = 180/288 rather than at 1.
        let grid = gibs_grid();
        let mesh = tile_patch_mesh(grid, 0, 0, 288.0);
        let uvs = match mesh.attribute(Mesh::ATTRIBUTE_UV_0).unwrap() {
            bevy::mesh::VertexAttributeValues::Float32x2(values) => values.clone(),
            other => panic!("unexpected uv format: {other:?}"),
        };
        let max_v = uvs.iter().map(|uv| uv[1]).fold(f32::MIN, f32::max);
        let max_u = uvs.iter().map(|uv| uv[0]).fold(f32::MIN, f32::max);
        assert!((max_v - 180.0 / 288.0).abs() < 1.0e-3, "max v was {max_v}");
        assert!((max_u - 1.0).abs() < 1.0e-3, "max u was {max_u}");
    }
}
