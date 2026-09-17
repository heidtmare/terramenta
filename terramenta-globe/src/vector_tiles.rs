//! Streams Mapbox Vector Tiles onto the globe as lines, rings and markers.
//!
//! This is [`crate::tiles`] for geometry rather than for pictures, and the two
//! halves it is built out of already existed: a quadtree walked from the camera
//! each frame, and the mesh builders [`crate::overlays`] turns a GeoJSON
//! document into screen-sized vector geometry with. What sits between them is
//! [`crate::mvt`], which decodes one tile's protobuf and unprojects it out of
//! Web Mercator back into degrees — so by the time a tile reaches this module
//! it is indistinguishable from a small GeoJSON document, and everything from
//! there on is the overlay path.
//!
//! Three things about it are worth reading before changing any of it.
//!
//! **The grid is not the imagery's grid.** Vector tiles are cut in Web
//! Mercator, where level `n` is a square `2^n` tiles on a side and the world
//! stops at ±85°; the imagery here is cut in plate carrée, where level `n` is
//! `2^(n+1)` by `2^n` and the world reaches the poles. They cannot share a
//! [`crate::tiles::TileGrid`], which is why the walk below is its own rather
//! than the one in `tiles`. [`crate::tiles::TileId`] is shared, because a level
//! with a column and a row is a level with a column and a row.
//!
//! **Decoding happens off the schedule; meshing happens on it.** A tile is
//! loaded through an asset source, so the protobuf is decoded in Bevy's asset
//! pipeline — a task thread natively, a microtask in the browser — and the
//! ECS only ever sees a finished [`VectorTileAsset`]. Turning that into
//! vertex buffers has to happen where `Assets<Mesh>` is, which is the schedule,
//! so it is rationed: [`MAX_BUILDS_PER_FRAME`] tiles a frame, because one
//! zoomed-in city tile can hold several thousand rings and ear-clipping all of
//! them at once is a visible hitch.
//!
//! **Features are drawn, not picked.** An overlay keeps its document so the
//! cursor can hit-test it; a vector tile drops everything but the meshes. A
//! screenful of tiles is tens of thousands of features against an overlay's
//! tens, and the index that makes picking cheap would cost more to build, every
//! time the camera moved, than the picking is worth on a basemap.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use bevy::asset::io::web::WebAssetReader;
use bevy::asset::io::{AssetReader, AssetReaderError, AssetSourceBuilder, PathStream, Reader};
use bevy::asset::{AssetApp, AssetLoader, LoadContext, LoadState};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::color::Srgba;
use bevy::prelude::*;
use serde::Serialize;

use crate::features::FeatureSet;
use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::{GeoBounds, LatLon};
use crate::globe::GLOBE_RADIUS;
use crate::mvt::{self, MvtError};
use crate::overlays::{OverlayAltitude, OverlayStyle, VectorMaterial, VectorMode};
use crate::tiles::TileId;

/// The asset source scheme vector tiles are fetched over.
pub const VECTOR_TILE_SOURCE: &str = "mvt";

/// The extension the tile path ends in, which is what picks the loader.
const TILE_EXTENSION: &str = "mvt";

/// The screen size a tile's geometry is considered good for, in pixels.
///
/// A vector tile has no resolution of its own — that is the point of it — so
/// there is no stretched image to notice, and the level has to be chosen from
/// how much *detail* the data holds instead. Every vector tile scheme in
/// practice generalises its geometry for a 512-pixel tile, so that is the size
/// worth matching: past it the lines are coarser than the screen can show.
const TILE_PIXELS: f32 = 512.0;

/// Split once a tile is being stretched past this multiple of that size.
/// Slightly over one, which trades a little detail for noticeably fewer
/// requests — the same bargain [`crate::tiles`] makes.
const SPLIT_FACTOR: f32 = 1.25;

/// How many requests may be outstanding, and how many may start per frame.
const MAX_TILES_IN_FLIGHT: usize = 16;
const MAX_NEW_REQUESTS_PER_FRAME: usize = 4;

/// How many tiles may be turned into meshes on one frame.
///
/// Small on purpose. Ear-clipping a city tile's worth of rings is milliseconds,
/// not microseconds, and a fast zoom lands a dozen tiles at once — meshing them
/// all on the frame they arrive is a stutter, and meshing two of them is not.
const MAX_BUILDS_PER_FRAME: usize = 2;

/// Frames a tile may go undrawn before it is dropped.
const RETIRE_AFTER_FRAMES: u64 = 240;

/// The deepest any vector tile layer may be asked to go, whatever it claims.
const MAX_LEVEL: u8 = 16;

// ---------------------------------------------------------------------------
// What a layer is
// ---------------------------------------------------------------------------

/// One source of vector tiles.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorTileLayer {
    /// What to call it in an interface.
    pub label: String,
    /// The tile URL, with `{z}`, `{x}` and `{y}` where the address goes —
    /// which is how every vector tile service in existence publishes itself,
    /// so a layer can be transcribed from a TileJSON document as it stands.
    pub url_template: String,
    /// How deep the service's pyramid goes. Asking past it earns 404s, which
    /// are slower than not asking.
    pub max_level: u8,
    /// Which of the tile's source layers to draw, or every one when empty.
    ///
    /// This is the difference between a readable basemap and a solid mat of
    /// ink: a zoom-14 tile holds roads, buildings, land use, water, labels and
    /// housenumbers, and almost nobody wants all six at once.
    pub source_layers: Vec<String>,
    /// What it is drawn in. The same style an overlay takes, because it is
    /// drawn by the same shader out of the same meshes.
    pub style: OverlayStyle,
}

impl VectorTileLayer {
    /// A layer with the defaults filled in, to be adjusted from there.
    pub fn new(label: impl Into<String>, url_template: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            url_template: url_template.into(),
            max_level: 6,
            source_layers: Vec::new(),
            // Cyan rather than the overlays' amber: a basemap has to be
            // distinguishable at a glance from the data drawn over it, and the
            // two hues that read against imagery are these.
            style: OverlayStyle {
                point_color: Srgba::new(0.55, 0.85, 1.0, 0.9),
                point_size_px: 5.0,
                line_color: Srgba::new(0.55, 0.85, 1.0, 0.85),
                line_width_px: 1.4,
                // Off by default. A basemap's job here is its lines; filling
                // every ring would hide the imagery the globe is showing.
                fill_color: Srgba::new(0.55, 0.85, 1.0, 0.0),
            },
        }
    }

    pub fn with_max_level(mut self, max_level: u8) -> Self {
        self.max_level = max_level.min(MAX_LEVEL);
        self
    }

    /// Draws only these source layers, in place of all of them.
    pub fn with_source_layers<S: Into<String>>(
        mut self,
        names: impl IntoIterator<Item = S>,
    ) -> Self {
        self.source_layers = names.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_style(mut self, style: OverlayStyle) -> Self {
        self.style = style;
        self
    }

    /// The URL one tile is fetched from.
    fn tile_url(&self, tile: TileId) -> String {
        self.url_template
            .replace("{z}", &tile.level.to_string())
            .replace("{x}", &tile.x.to_string())
            .replace("{y}", &tile.y.to_string())
    }
}

// ---------------------------------------------------------------------------
// The resource an embedder drives
// ---------------------------------------------------------------------------

/// The live layer, shared with the asset reader and loader, which live outside
/// the `World`.
type SharedVectorTileLayer = Arc<RwLock<VectorTileLayer>>;

/// Which vector tile layer is being streamed, and whether it is being streamed
/// at all.
///
/// One active layer rather than a list of them, which is the shape
/// [`crate::imagery`] has and for the same reason: a basemap is the ground
/// everything else is drawn on, and two of them at once is two ground truths.
/// Data that should sit *over* the imagery alongside other data is an overlay.
#[derive(Resource)]
pub struct VectorTileSettings {
    shared: SharedVectorTileLayer,
    /// Bumped whenever the layer or its filter changes. It is part of every
    /// tile path, so a change cannot be answered out of the cache of the one
    /// before it.
    generation: u32,
    /// Layers an interface can offer, in the order they cycle.
    pub presets: Vec<VectorTileLayer>,
    pub preset_index: usize,
    pub enabled: bool,
}

impl VectorTileSettings {
    pub fn generation(&self) -> u32 {
        self.generation
    }

    fn with<T>(&self, read: impl FnOnce(&VectorTileLayer) -> T) -> T {
        read(&self.shared.read().expect("vector tile layer lock poisoned"))
    }

    pub fn label(&self) -> String {
        self.with(|layer| layer.label.clone())
    }

    pub fn max_level(&self) -> u8 {
        self.with(|layer| layer.max_level.min(MAX_LEVEL))
    }

    pub fn style(&self) -> OverlayStyle {
        self.with(|layer| layer.style)
    }

    pub fn source_layers(&self) -> Vec<String> {
        self.with(|layer| layer.source_layers.clone())
    }

    /// Switches to a preset by index, wrapping past the end of the list, and
    /// invalidates every tile already loaded.
    pub fn select_preset(&mut self, index: usize) {
        if self.presets.is_empty() {
            return;
        }
        self.preset_index = index % self.presets.len();
        let next = self.presets[self.preset_index].clone();
        self.replace(next);
    }

    pub fn cycle_preset(&mut self) {
        self.select_preset(self.preset_index + 1);
    }

    pub fn cycle_preset_back(&mut self) {
        self.select_preset(self.preset_index + self.presets.len().saturating_sub(1));
    }

    /// Restyles the active layer.
    ///
    /// Unlike an overlay this rebuilds rather than recolours. A tile's meshes
    /// are keyed to the style they were built with — a layer whose fill was
    /// transparent did not build a fill mesh at all — so there is nothing to
    /// swap a colour on, and rebuilding a screenful of tiles is what the cache
    /// already does whenever the layer changes.
    pub fn set_style(&mut self, style: OverlayStyle) {
        let mut layer = self.with(Clone::clone);
        if layer.style == style {
            return;
        }
        layer.style = style;
        if let Some(preset) = self.presets.get_mut(self.preset_index) {
            preset.style = style;
        }
        self.replace(layer);
    }

    fn replace(&mut self, layer: VectorTileLayer) {
        *self
            .shared
            .write()
            .expect("vector tile layer lock poisoned") = layer;
        self.generation = self.generation.wrapping_add(1);
    }
}

// ---------------------------------------------------------------------------
// The tile, as an asset
// ---------------------------------------------------------------------------

/// One decoded tile: its geometry, already in degrees.
#[derive(Asset, TypePath, Debug)]
pub struct VectorTileAsset(pub FeatureSet);

#[derive(Debug)]
pub enum VectorTileLoadError {
    Io(std::io::Error),
    /// The path did not name a tile, which can only mean the asset source
    /// served something this loader was not meant to see.
    NotATile(PathBuf),
    NotAVectorTile(MvtError),
}

impl std::fmt::Display for VectorTileLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::NotATile(path) => write!(formatter, "not a tile path: {}", path.display()),
            Self::NotAVectorTile(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for VectorTileLoadError {}

/// Decodes a tile's protobuf into geometry on the globe.
///
/// Which tile it is comes from the path being loaded rather than from a
/// setting, because that is the only thing that distinguishes one request from
/// another — and it has to be known here, since the coordinates inside a tile
/// mean nothing without the square they are measured in.
#[derive(TypePath)]
struct VectorTileLoader {
    layer: SharedVectorTileLayer,
}

impl AssetLoader for VectorTileLoader {
    type Asset = VectorTileAsset;
    type Settings = ();
    type Error = VectorTileLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let path = context.path().path().to_path_buf();
        let tile = parse_tile_path(&path).ok_or(VectorTileLoadError::NotATile(path))?;

        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(VectorTileLoadError::Io)?;

        // Read and released before decoding rather than held across it: the
        // decode is the long part, and the schedule may want this lock.
        let wanted = self
            .layer
            .read()
            .map(|layer| layer.source_layers.clone())
            .unwrap_or_default();

        mvt::decode(&bytes, tile, &wanted)
            .map(VectorTileAsset)
            .map_err(VectorTileLoadError::NotAVectorTile)
    }

    fn extensions(&self) -> &[&str] {
        &[TILE_EXTENSION]
    }
}

/// Serves `{generation}/{z}/{x}/{y}.mvt` from whatever URL the active layer's
/// template makes of that address, exactly as the imagery source serves a
/// picture.
struct VectorTileAssetReader {
    layer: SharedVectorTileLayer,
    http: WebAssetReader,
    https: WebAssetReader,
}

/// Recovers the tile a path refers to. The leading generation segment only
/// exists to keep cache entries apart between layers, so it is discarded.
fn parse_tile_path(path: &Path) -> Option<TileId> {
    let mut segments = path.to_str()?.split('/');
    let _generation = segments.next()?;
    let level: u8 = segments.next()?.parse().ok()?;
    let x: u32 = segments.next()?.parse().ok()?;
    let y: u32 = segments.next()?.split('.').next()?.parse().ok()?;
    Some(TileId { level, x, y })
}

impl AssetReader for VectorTileAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let not_found = || AssetReaderError::NotFound(path.to_path_buf());
        let tile = parse_tile_path(path).ok_or_else(not_found)?;

        // Built and the lock released before awaiting, so the guard is never
        // held across a suspension point.
        let url = {
            let layer = self.layer.read().map_err(|_| not_found())?;
            layer.tile_url(tile)
        };

        // Bevy's reader prepends the scheme itself, so hand it the remainder.
        let (reader, remainder) = match url.split_once("://") {
            Some(("https", rest)) => (&self.https, rest),
            Some(("http", rest)) => (&self.http, rest),
            _ => return Err(AssetReaderError::NotFound(PathBuf::from(url))),
        };
        reader.read(Path::new(remainder)).await
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        Err::<Box<dyn Reader>, _>(AssetReaderError::NotFound(path.to_path_buf()))
    }

    async fn is_directory<'a>(&'a self, _path: &'a Path) -> Result<bool, AssetReaderError> {
        Ok(false)
    }

    async fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        Err(AssetReaderError::NotFound(path.to_path_buf()))
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileState {
    /// Asked for, not yet answered.
    Pending,
    /// Decoded, not yet turned into meshes.
    Decoded,
    /// Drawable.
    Ready,
    Failed,
}

struct TileSlot {
    handle: Handle<VectorTileAsset>,
    state: TileState,
    /// One entity per shape drawn — up to three, and frequently one, since a
    /// layer of boundaries has no fills and no markers.
    entities: Vec<Entity>,
    /// How many features the tile turned out to hold, for the readout.
    features: usize,
    last_used: u64,
}

#[derive(Resource, Default)]
pub struct VectorTileCache {
    slots: HashMap<TileId, TileSlot>,
    generation: u32,
    frame: u64,
    /// Counters for the readout.
    pub visible_tiles: usize,
    pub loading_tiles: usize,
    pub deepest_level: u8,
    pub features: usize,
}

impl VectorTileCache {
    fn is_drawable(&self, tile: TileId) -> bool {
        self.slots
            .get(&tile)
            .is_some_and(|slot| slot.state == TileState::Ready)
    }

    /// Drops every tile — what a change of layer leaves behind.
    fn clear(&mut self, commands: &mut Commands) {
        for (_, slot) in self.slots.drain() {
            for entity in slot.entities {
                commands.entity(entity).despawn();
            }
        }
        self.visible_tiles = 0;
        self.loading_tiles = 0;
        self.deepest_level = 0;
        self.features = 0;
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

struct SelectionContext {
    camera_position: Vec3,
    camera_direction: Vec3,
    camera_distance: f32,
    /// Pixels per radian at the centre of the view.
    focal_pixels: f32,
    max_level: u8,
}

impl SelectionContext {
    /// Roughly how many pixels across the tile covers.
    fn projected_pixels(&self, bounds: GeoBounds) -> f32 {
        let center = bounds.center().to_direction() * GLOBE_RADIUS;
        let extent = bounds.lat_span().to_radians() * GLOBE_RADIUS;
        let distance = (self.camera_position - center).length().max(1.0e-3);
        extent / distance * self.focal_pixels
    }

    fn should_split(&self, bounds: GeoBounds) -> bool {
        self.projected_pixels(bounds) > TILE_PIXELS * SPLIT_FACTOR
    }

    /// Whether any part of the tile can be over the horizon. The same test
    /// [`crate::tiles`] uses, on the same sphere.
    fn is_visible(&self, bounds: GeoBounds) -> bool {
        if bounds.lat_span() >= 90.0 {
            return true;
        }
        let angular_radius = bounds.lat_span().to_radians() * 0.75;
        let horizon = GLOBE_RADIUS / self.camera_distance - (1.0 - angular_radius.cos()) - 0.02;

        [
            LatLon::new(bounds.lat_min, bounds.lon_min),
            LatLon::new(bounds.lat_min, bounds.lon_max),
            LatLon::new(bounds.lat_max, bounds.lon_min),
            LatLon::new(bounds.lat_max, bounds.lon_max),
            bounds.center(),
        ]
        .iter()
        .any(|corner| corner.to_direction().dot(self.camera_direction) > horizon)
    }
}

/// Walks the Mercator quadtree, collecting what should be drawn and what should
/// be asked for.
///
/// The fallback rule is the imagery streamer's: a tile that has not arrived
/// falls back to the nearest ancestor that has, so zooming in coarsens rather
/// than blanks. Unlike imagery there is nothing underneath to show through, so
/// a branch with nothing loaded anywhere along it simply draws nothing.
fn select_tiles(
    tile: TileId,
    context: &SelectionContext,
    cache: &VectorTileCache,
    selected: &mut HashSet<TileId>,
    wanted: &mut Vec<TileId>,
) {
    let bounds = mvt::tile_bounds(tile);
    if !context.is_visible(bounds) {
        return;
    }

    if tile.level < context.max_level && context.should_split(bounds) {
        for child in tile.children() {
            select_tiles(child, context, cache, selected, wanted);
        }
        return;
    }

    let mut candidate = Some(tile);
    while let Some(current) = candidate {
        if cache.is_drawable(current) {
            selected.insert(current);
            return;
        }
        if !cache.slots.contains_key(&current) {
            wanted.push(current);
        }
        candidate = current.parent();
    }
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

/// Marks an entity drawing part of a vector tile.
#[derive(Component)]
pub struct VectorTileEntity(pub TileId);

/// The half that has to be in place before `AssetPlugin` builds: the asset
/// source tiles are fetched over, and the layer they are fetched for.
///
/// Split from [`VectorTilePlugin`] for the same reason [`crate::imagery`] is
/// split from [`crate::tiles`] — an asset source can only be registered before
/// the asset server exists, and an asset type only after.
pub struct VectorTileSourcePlugin {
    /// Layers an interface can offer, in the order they cycle.
    pub presets: Vec<VectorTileLayer>,
    /// Whether tiles are streamed at startup.
    pub enabled: bool,
}

impl Plugin for VectorTileSourcePlugin {
    fn build(&self, app: &mut App) {
        let initial = self
            .presets
            .first()
            .cloned()
            .unwrap_or_else(|| VectorTileLayer::new("none", ""));
        let shared: SharedVectorTileLayer = Arc::new(RwLock::new(initial));

        // Both the reader and the loader outlive any one `World`, so they share
        // the layer through the same handle the resource writes to.
        let reader_layer = shared.clone();
        app.register_asset_source(
            VECTOR_TILE_SOURCE,
            AssetSourceBuilder::new(move || {
                Box::new(VectorTileAssetReader {
                    layer: reader_layer.clone(),
                    http: WebAssetReader::Http,
                    https: WebAssetReader::Https,
                })
            }),
        );

        app.insert_resource(VectorTileSettings {
            shared,
            generation: 0,
            presets: self.presets.clone(),
            preset_index: 0,
            enabled: self.enabled && !self.presets.is_empty(),
        });
    }
}

/// Drawing vector tiles: the tile type, its loader, and the systems that keep
/// what is on screen in step with the camera.
pub struct VectorTilePlugin;

impl Plugin for VectorTilePlugin {
    fn build(&self, app: &mut App) {
        let shared = app.world().resource::<VectorTileSettings>().shared.clone();

        app.init_asset::<VectorTileAsset>()
            .register_asset_loader(VectorTileLoader { layer: shared })
            .init_resource::<VectorTileCache>()
            .add_systems(
                Update,
                (
                    vector_tile_controls.run_if(crate::api::keyboard_enabled),
                    stream_vector_tiles,
                    build_vector_tiles,
                    orient_vector_tiles,
                )
                    .chain()
                    .in_set(FrameSet::Apply)
                    .before(crate::api::publish_state),
            );
    }
}

fn vector_tile_controls(keys: Res<ButtonInput<KeyCode>>, mut settings: ResMut<VectorTileSettings>) {
    if keys.just_pressed(KeyCode::KeyV) {
        if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
            settings.cycle_preset();
        } else {
            settings.enabled = !settings.enabled;
        }
    }
}

/// Walks the quadtree, starts what is missing, and retires what the view has
/// stopped wanting.
fn stream_vector_tiles(
    mut commands: Commands,
    // The camera's own transform, not its global one: the global is a tick
    // behind, and a tick behind is exactly wrong on the tick the frame
    // switches, when the camera and the Earth move together.
    camera: Single<(&Camera, &Transform, &Projection)>,
    frame: Res<ReferenceFrame>,
    settings: Res<VectorTileSettings>,
    mut cache: ResMut<VectorTileCache>,
    assets: Res<AssetServer>,
    mut drawn: Query<(&VectorTileEntity, &mut Visibility)>,
) {
    cache.frame += 1;

    // A new layer invalidates every cached path, so start over.
    if cache.generation != settings.generation() {
        cache.clear(&mut commands);
        cache.generation = settings.generation();
    }

    if !settings.enabled {
        if !cache.slots.is_empty() {
            cache.clear(&mut commands);
        }
        return;
    }

    // Promote anything that finished loading since the last frame.
    for slot in cache.slots.values_mut() {
        if slot.state != TileState::Pending {
            continue;
        }
        match assets.get_load_state(&slot.handle) {
            Some(LoadState::Loaded) => slot.state = TileState::Decoded,
            Some(LoadState::Failed(_)) => slot.state = TileState::Failed,
            _ => {}
        }
    }

    let (camera_component, camera_transform, projection) = *camera;
    // The quadtree is addressed in latitude and longitude, so the walk has to
    // happen in Earth-fixed coordinates however the world is turned.
    let camera_position = frame.earth_to_world().inverse() * camera_transform.translation;
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
    // Level zero is one tile holding the whole Mercator world.
    select_tiles(
        TileId {
            level: 0,
            x: 0,
            y: 0,
        },
        &context,
        &cache,
        &mut selected,
        &mut wanted,
    );

    // Coarse tiles first: they cover the most ground per request, so they are
    // what turns an empty view into a complete one soonest.
    wanted.sort_unstable();
    wanted.dedup();

    let mut in_flight = cache
        .slots
        .values()
        .filter(|slot| slot.state == TileState::Pending)
        .count();
    let generation = cache.generation;
    let frame_number = cache.frame;

    for tile in wanted.into_iter().take(MAX_NEW_REQUESTS_PER_FRAME) {
        if in_flight >= MAX_TILES_IN_FLIGHT {
            break;
        }
        if cache.slots.contains_key(&tile) {
            continue;
        }
        let path = format!(
            "{VECTOR_TILE_SOURCE}://{generation}/{}/{}/{}.{TILE_EXTENSION}",
            tile.level, tile.x, tile.y
        );
        cache.slots.insert(
            tile,
            TileSlot {
                handle: assets.load(path),
                state: TileState::Pending,
                entities: Vec::new(),
                features: 0,
                last_used: frame_number,
            },
        );
        in_flight += 1;
    }

    // Draw what the walk chose and nothing else. A tile that has fallen out of
    // the selection is kept and hidden rather than dropped: the view swings
    // back over it constantly, and re-meshing what is already in hand is waste.
    for (tile, mut visibility) in &mut drawn {
        *visibility = if selected.contains(&tile.0) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }

    let mut visible_tiles = 0;
    let mut loading_tiles = 0;
    let mut features = 0;
    let mut deepest = 0;
    for (tile, slot) in cache.slots.iter_mut() {
        if selected.contains(tile) {
            slot.last_used = frame_number;
            deepest = deepest.max(tile.level);
            visible_tiles += 1;
            features += slot.features;
        }
        if matches!(slot.state, TileState::Pending | TileState::Decoded) {
            loading_tiles += 1;
        }
    }

    cache.visible_tiles = visible_tiles;
    cache.loading_tiles = loading_tiles;
    cache.features = features;
    cache.deepest_level = deepest;

    // Retire whatever the view has not wanted for a while. A tile still waiting
    // on its request is kept: it has no entities to cost anything, and dropping
    // it would only mean asking again.
    let retired: Vec<TileId> = cache
        .slots
        .iter()
        .filter(|(_, slot)| {
            slot.state != TileState::Pending && slot.last_used + RETIRE_AFTER_FRAMES < frame_number
        })
        .map(|(tile, _)| *tile)
        .collect();
    for tile in retired {
        if let Some(slot) = cache.slots.remove(&tile) {
            for entity in slot.entities {
                commands.entity(entity).despawn();
            }
        }
    }
}

/// Turns decoded tiles into vertex buffers, a few at a time.
///
/// This is the step the module exists for: the geometry arrives as latitude and
/// longitude, and leaves as positions on the globe in scene units, laid out for
/// the vector shader — markers as quads on one anchor, lines as ribbons that
/// step off their spine in the vertex stage, rings triangulated and lifted off
/// the sphere by the sag of their own chords. None of that is written here,
/// because an overlay needs exactly the same thing and already has it.
fn build_vector_tiles(
    mut commands: Commands,
    settings: Res<VectorTileSettings>,
    mut cache: ResMut<VectorTileCache>,
    tiles: Res<Assets<VectorTileAsset>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VectorMaterial>>,
    frame: Res<ReferenceFrame>,
) {
    if !settings.enabled {
        return;
    }

    let style = settings.style();
    let earth_to_world = frame.earth_to_world();

    let pending: Vec<TileId> = cache
        .slots
        .iter()
        .filter(|(_, slot)| slot.state == TileState::Decoded)
        .map(|(tile, _)| *tile)
        .take(MAX_BUILDS_PER_FRAME)
        .collect();

    for tile in pending {
        let Some(slot) = cache.slots.get_mut(&tile) else {
            continue;
        };
        let Some(decoded) = tiles.get(&slot.handle) else {
            // The handle outlived its asset, which can only mean the load was
            // undone underneath us; asking again is the recovery.
            slot.state = TileState::Failed;
            continue;
        };
        let document = &decoded.0;
        slot.features = document.feature_count();

        // Vector tiles are flat by construction — the format has no third
        // element — so the height rules an overlay carries have nothing to act
        // on, and everything is draped.
        let altitude = OverlayAltitude::CLAMPED;
        // Fills first, then lines, then markers, which is the order the radii
        // in `overlays` already put them in.
        // A vector tile has no styling of its own to honour: the format has
        // nowhere to put simplestyle, and a basemap is styled as a basemap
        // rather than a feature at a time. See `crate::overlays::FeaturePaint`.
        let paint = crate::overlays::FeaturePaint::Layer;
        let built = [
            (
                VectorMode::Fill,
                // A fully transparent fill is not drawn at all rather than
                // drawn invisibly: a basemap's rings can be most of its
                // geometry, and triangulating them to blend nothing would be
                // the most expensive part of the whole tile.
                (style.fill_color.alpha > 0.0)
                    .then(|| crate::overlays::fill_mesh(document, None, altitude, paint))
                    .flatten(),
            ),
            (
                VectorMode::Line,
                // No polygons here, unlike an overlay. A clipped ring's edges
                // are not all boundaries, so `mvt` puts the parts that are into
                // the line list; outlining the rings as well would draw every
                // border twice and the tile grid once. See `crate::mvt`.
                crate::overlays::line_mesh(document, None, false, altitude, paint),
            ),
            (
                VectorMode::Marker,
                crate::overlays::marker_mesh(document, None, altitude, paint),
            ),
        ];

        for (mode, mesh) in built {
            let Some(mesh) = mesh else {
                continue;
            };
            let entity = commands
                .spawn((
                    Name::new(format!(
                        "Vector tile {}/{}/{} ({mode:?})",
                        tile.level, tile.x, tile.y
                    )),
                    VectorTileEntity(tile),
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(
                        materials.add(VectorMaterial::new(mode, style.paint(mode)).beneath()),
                    ),
                    // `orient_vector_tiles` keeps this in step with the frame;
                    // the spawn value only has to be right for this frame.
                    Transform::from_rotation(earth_to_world),
                    // Markers and lines are spread in the vertex shader, so the
                    // mesh's own bounds understate what is drawn.
                    NoFrustumCulling,
                ))
                .id();
            slot.entities.push(entity);
        }

        slot.state = TileState::Ready;
    }
}

/// Carries the tiles around with the globe they sit on, and hides them all when
/// the layer is switched off.
fn orient_vector_tiles(
    frame: Res<ReferenceFrame>,
    settings: Res<VectorTileSettings>,
    mut tiles: Query<(&mut Transform, &mut Visibility), With<VectorTileEntity>>,
) {
    let earth_to_world = frame.earth_to_world();
    for (mut transform, mut visibility) in &mut tiles {
        transform.rotation = earth_to_world;
        if !settings.enabled {
            *visibility = Visibility::Hidden;
        }
    }
}

// ---------------------------------------------------------------------------
// What the state stream reports
// ---------------------------------------------------------------------------

/// The vector tile layer, as an interface sees it.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VectorTilesState {
    pub enabled: bool,
    pub layer_index: usize,
    pub label: String,
    /// How deep the active layer's pyramid goes.
    pub max_level: u8,
    /// How deep the walk actually went this frame.
    pub deepest_level: u8,
    pub visible_tiles: usize,
    pub loading_tiles: usize,
    /// How many features are drawn across every tile on screen.
    pub features: usize,
    /// Which of the tile's source layers are drawn, or empty for all of them.
    pub source_layers: Vec<String>,
    pub style: crate::overlays::OverlayStyleInfo,
}

/// One of the vector tile presets, as an embedder sees it before picking one.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VectorTileLayerInfo {
    pub index: usize,
    pub label: String,
    pub max_level: u8,
    pub source_layers: Vec<String>,
}

/// Describes the presets a globe built from this list would offer.
pub fn describe_layers(presets: &[VectorTileLayer]) -> Vec<VectorTileLayerInfo> {
    presets
        .iter()
        .enumerate()
        .map(|(index, layer)| VectorTileLayerInfo {
            index,
            label: layer.label.clone(),
            max_level: layer.max_level.min(MAX_LEVEL),
            source_layers: layer.source_layers.clone(),
        })
        .collect()
}

pub fn describe(settings: &VectorTileSettings, cache: &VectorTileCache) -> VectorTilesState {
    let style = settings.style();
    VectorTilesState {
        enabled: settings.enabled,
        layer_index: settings.preset_index,
        label: settings.label(),
        max_level: settings.max_level(),
        deepest_level: cache.deepest_level,
        visible_tiles: cache.visible_tiles,
        loading_tiles: cache.loading_tiles,
        features: cache.features,
        source_layers: settings.source_layers(),
        style: crate::overlays::OverlayStyleInfo::from(&style),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_paths_round_trip() {
        let tile = TileId {
            level: 9,
            x: 264,
            y: 171,
        };
        assert_eq!(parse_tile_path(Path::new("3/9/264/171.mvt")), Some(tile));
        assert_eq!(parse_tile_path(Path::new("not-a-tile")), None);
    }

    #[test]
    fn a_template_is_filled_in_the_way_every_service_publishes_one() {
        let layer = VectorTileLayer::new("demo", "https://example.test/{z}/{x}/{y}.pbf");
        assert_eq!(
            layer.tile_url(TileId {
                level: 4,
                x: 9,
                y: 3
            }),
            "https://example.test/4/9/3.pbf"
        );
    }

    #[test]
    fn a_layer_cannot_be_asked_to_go_deeper_than_the_grid_is_addressable() {
        let layer = VectorTileLayer::new("demo", "").with_max_level(30);
        assert_eq!(layer.max_level, MAX_LEVEL);
    }
}
