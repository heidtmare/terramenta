//! GeoJSON overlays: vector data drawn over the globe.
//!
//! An overlay is one GeoJSON document — from a URL, or handed straight over as
//! text — drawn as markers, lines and filled rings on top of whatever imagery
//! is underneath. Several can be up at once; each has its own colours, its own
//! visibility, and its own refresh period.
//!
//! Three things here are worth reading before changing any of it.
//!
//! **Where the two kinds of source part company.** A URL is the globe's
//! business: it is fetched through the `geojson://` asset source below, which
//! is the trick [`crate::imagery`] uses for tiles and buys asynchronous I/O,
//! identically on native and in the browser. Text is the embedder's — a file
//! the user picked, a document assembled in JavaScript — and the globe has
//! nowhere to fetch it from again, so auto-refresh is for URL sources. An
//! embedder refreshing a local file re-sends the text under the same id, which
//! replaces the layer in place.
//!
//! **Refreshing has to defeat two caches.** Bevy keys its asset cache by path
//! and the browser keys its own by URL, so simply asking again is answered
//! twice over from something already in hand. Both are sidestepped the same
//! way: the path carries a generation, which the asset cache sees, and from the
//! second fetch onward so does the request, which the HTTP cache sees. A layer
//! that never refreshes never gets the extra parameter, so a signed or
//! otherwise parameter-sensitive URL still works.
//!
//! **Size is in pixels, not in kilometres.** A marker and a line are sized on
//! screen, so they stay legible from orbit and from a low pass without the
//! layer being rebuilt for either. That means the mesh holds anchors rather
//! than shapes, and the corners are spread in the vertex shader — see
//! `assets/shaders/vector.wgsl`, which is where the size is finally decided.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use bevy::asset::io::web::WebAssetReader;
use bevy::asset::io::{AssetReader, AssetReaderError, AssetSourceBuilder, PathStream, Reader};
use bevy::asset::{AssetApp, AssetLoader, LoadContext, LoadState, RenderAssetUsages};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::color::Srgba;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use serde::Serialize;

use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::LatLon;
use crate::geojson::{GeoJson, Polygon};
use crate::globe::GLOBE_RADIUS;
use crate::tessellate;
use crate::tiles::MAX_TILE_RADIUS;

/// The asset source scheme overlay documents are fetched over.
pub const OVERLAY_SOURCE: &str = "geojson";

/// The query parameter a refresh adds to get past the HTTP cache. Named rather
/// than a bare `t` so a server log says where it came from.
const CACHE_BUSTER: &str = "_terramenta";

/// Overlays sit above the highest an imagery tile is ever drawn, and in this
/// order, so a marker is never lost in the fill under it.
///
/// Clearing [`MAX_TILE_RADIUS`] is not optional and not cosmetic. A tile is a
/// polygon inscribed in a sphere, pushed out far enough that its sag still
/// leaves it at or above its level's radius — so it stands *proud* of the
/// surface by as much as a kilometre and a half at its corners, and an overlay
/// drawn on the surface itself would be buried at every corner and visible only
/// in the middle of each quad. A line does not vanish; it goes dashed, which is
/// a far more confusing thing to look at.
const FILL_RADIUS: f32 = MAX_TILE_RADIUS + GLOBE_RADIUS * 1.0e-4;
const LINE_RADIUS: f32 = MAX_TILE_RADIUS + GLOBE_RADIUS * 1.5e-4;
const MARKER_RADIUS: f32 = MAX_TILE_RADIUS + GLOBE_RADIUS * 2.0e-4;

/// The furthest apart two corners of a line may be before the segment between
/// them is subdivided. A straight chord over a long span cuts under the globe
/// and disappears; this keeps a line on the surface it belongs to.
const MAX_SEGMENT_DEGREES: f32 = 2.0;

/// The most triangles one polygon's fill may be refined into. Past it the fill
/// is drawn coarser — lifted further off the surface to compensate — rather
/// than abandoned.
const MAX_FILL_TRIANGLES: usize = 60_000;

/// A refresh period is clamped to this at the fast end. Anything quicker is a
/// request loop rather than a refresh, and no feed is worth polling that hard.
pub const MIN_REFRESH_SECONDS: f32 = 1.0;

// ---------------------------------------------------------------------------
// What an overlay is
// ---------------------------------------------------------------------------

/// Where a document comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlaySource {
    /// Fetched by the globe, and the only kind that can be refreshed.
    Url(String),
    /// Handed over as text — a local file, once the embedder has read it.
    Text(String),
}

impl OverlaySource {
    /// The stable name the control surface calls this kind by.
    pub fn id(&self) -> &'static str {
        match self {
            Self::Url(_) => "url",
            Self::Text(_) => "text",
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Url(url) => Some(url),
            Self::Text(_) => None,
        }
    }
}

/// How an overlay is drawn.
///
/// One style for the whole layer. Per-feature styling would mean carrying
/// GeoJSON properties through to the mesh builder and then a draw call per
/// distinct appearance, and a layer is the unit an interface gives a colour to
/// anyway — two feeds are told apart by being two colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayStyle {
    pub point_color: Srgba,
    /// Marker diameter on screen, in device pixels.
    pub point_size_px: f32,
    pub line_color: Srgba,
    /// Line width on screen, in device pixels.
    pub line_width_px: f32,
    /// Ring fill. Its alpha is the fill's opacity, and zero turns the fill off
    /// without taking the outline with it.
    pub fill_color: Srgba,
}

impl Default for OverlayStyle {
    fn default() -> Self {
        // Amber: the one hue that is neither ocean, vegetation, desert nor
        // cloud, so it reads against imagery wherever the layer lands.
        Self {
            point_color: Srgba::new(1.0, 0.62, 0.24, 1.0),
            point_size_px: 9.0,
            line_color: Srgba::new(1.0, 0.62, 0.24, 0.95),
            line_width_px: 2.0,
            fill_color: Srgba::new(1.0, 0.62, 0.24, 0.22),
        }
    }
}

/// Everything needed to put one overlay up, as an embedder states it.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayRequest {
    /// The embedder's name for the layer. Adding a second overlay under an id
    /// already in use replaces the first, which is what makes re-sending a
    /// re-read file an update rather than a duplicate.
    pub id: String,
    /// What to call it in an interface. The id, if this is empty.
    pub label: String,
    pub source: OverlaySource,
    pub style: OverlayStyle,
    /// Seconds between refetches, or `None` to fetch once. A text source has
    /// nowhere to refetch from, so it ignores this.
    pub refresh_seconds: Option<f32>,
    pub visible: bool,
}

/// How an overlay's data is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayStatus {
    Loading,
    Ready,
    /// Unreachable, not GeoJSON, or refused by the browser for want of CORS.
    Failed(String),
}

impl OverlayStatus {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Failed(_) => "failed",
        }
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(message) => Some(message),
            _ => None,
        }
    }
}

/// How much of what an overlay turned out to hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayCounts {
    pub features: usize,
    pub points: usize,
    pub lines: usize,
    pub polygons: usize,
}

/// One drawn piece of an overlay: its markers, its lines, or its fills.
struct OverlayPart {
    entity: Entity,
    material: Handle<VectorMaterial>,
    mode: VectorMode,
}

/// One live overlay.
struct Overlay {
    id: String,
    label: String,
    source: OverlaySource,
    /// Identifies this overlay to the asset reader, which lives outside the
    /// `World` and so cannot be handed anything borrowed from it. Never reused,
    /// so a response still in flight for a removed layer cannot be mistaken for
    /// one belonging to a layer added afterwards.
    slot: u64,
    /// Bumped by every refresh, and part of the asset path, so a refetch is
    /// never answered out of the cache of the one before it.
    generation: u32,
    style: OverlayStyle,
    refresh_seconds: Option<f32>,
    /// Seconds since the last fetch was started, which is both the refresh
    /// countdown and how stale an interface should say the layer is.
    age_seconds: f32,
    visible: bool,
    status: OverlayStatus,
    counts: OverlayCounts,
    /// Held so the document stays loaded, and so its load state can be polled.
    handle: Option<Handle<GeoJsonAsset>>,
    /// Geometry waiting to be meshed, from a text source or a finished fetch.
    pending: Option<GeoJson>,
    /// Set when a fetch is due; cleared once one has been started.
    wants_fetch: bool,
    /// Set when the colours changed but the geometry did not.
    wants_restyle: bool,
    parts: Vec<OverlayPart>,
}

impl Overlay {
    /// The asset path this overlay's document is fetched over.
    fn asset_path(&self) -> String {
        format!(
            "{OVERLAY_SOURCE}://{}/{}.geojson",
            self.slot, self.generation
        )
    }

    fn set_refresh(&mut self, seconds: Option<f32>) {
        // A text source has nowhere to fetch from, so it never carries a
        // period: an embedder refreshing a local file re-reads it and re-sends
        // the text, and the globe would only be counting down to nothing.
        self.refresh_seconds = seconds
            .filter(|_| matches!(self.source, OverlaySource::Url(_)))
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .map(|seconds| seconds.max(MIN_REFRESH_SECONDS));
    }

    /// Returns whether this was a change, which is what decides if the state
    /// stream should publish immediately rather than on its next tick.
    fn set_status(&mut self, status: OverlayStatus) -> bool {
        let changed = self.status != status;
        self.status = status;
        changed
    }
}

// ---------------------------------------------------------------------------
// The resource an embedder drives
// ---------------------------------------------------------------------------

/// The URL behind each overlay slot, shared with the asset reader outside the
/// `World`.
type SharedOverlayUrls = Arc<RwLock<HashMap<u64, String>>>;

/// Every overlay, and the master switch over all of them.
#[derive(Resource)]
pub struct OverlaySettings {
    /// Whether overlays are drawn at all. Off, every layer stays loaded and
    /// simply stops being drawn, so switching back is instant.
    pub enabled: bool,
    overlays: Vec<Overlay>,
    urls: SharedOverlayUrls,
    next_slot: u64,
    /// Bumped by anything an interface has a control for, so the state stream
    /// can publish the moment one changes rather than on the next throttle tick.
    revision: u64,
    /// Entities of removed overlays, waiting for a system with `Commands`.
    retired: Vec<Entity>,
}

impl OverlaySettings {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether a layer would be drawn if it had anything to draw. The master
    /// switch is separate, so a caller has to want both.
    fn is_visible(&self, id: &str) -> bool {
        self.overlays
            .iter()
            .any(|overlay| overlay.id == id && overlay.visible)
    }

    /// How many overlays are actually on screen: loaded, visible, and the
    /// master switch on.
    pub fn drawn(&self) -> usize {
        if !self.enabled {
            return 0;
        }
        self.overlays
            .iter()
            .filter(|overlay| overlay.visible && overlay.status == OverlayStatus::Ready)
            .count()
    }

    /// Puts an overlay up, replacing any already under the same id.
    pub fn add(&mut self, request: OverlayRequest) {
        self.remove(&request.id);

        let slot = self.next_slot;
        self.next_slot += 1;
        if let OverlaySource::Url(url) = &request.source
            && let Ok(mut urls) = self.urls.write()
        {
            urls.insert(slot, url.clone());
        }

        let label = if request.label.is_empty() {
            request.id.clone()
        } else {
            request.label
        };

        let mut overlay = Overlay {
            id: request.id,
            label,
            slot,
            generation: 0,
            style: request.style,
            refresh_seconds: None,
            age_seconds: 0.0,
            visible: request.visible,
            status: OverlayStatus::Loading,
            counts: OverlayCounts::default(),
            handle: None,
            pending: None,
            // A URL is fetched on the next tick; text is already here, so it is
            // parsed now and the failure, if it is one, reported straight away.
            wants_fetch: matches!(request.source, OverlaySource::Url(_)),
            wants_restyle: false,
            parts: Vec::new(),
            source: request.source,
        };
        overlay.set_refresh(request.refresh_seconds);
        if let OverlaySource::Text(text) = &overlay.source {
            match GeoJson::parse(text) {
                Ok(parsed) => overlay.pending = Some(parsed),
                Err(error) => overlay.status = OverlayStatus::Failed(error.to_string()),
            }
        }

        self.overlays.push(overlay);
        self.revision += 1;
    }

    /// Takes an overlay down. Returns whether there was one.
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(index) = self.overlays.iter().position(|overlay| overlay.id == id) else {
            return false;
        };
        let overlay = self.overlays.remove(index);
        if let Ok(mut urls) = self.urls.write() {
            urls.remove(&overlay.slot);
        }
        self.retired
            .extend(overlay.parts.iter().map(|part| part.entity));
        self.revision += 1;
        true
    }

    pub fn set_visible(&mut self, id: &str, visible: bool) -> bool {
        self.with(id, |overlay| overlay.visible = visible)
    }

    pub fn set_style(&mut self, id: &str, style: OverlayStyle) -> bool {
        self.with(id, |overlay| {
            overlay.style = style;
            overlay.wants_restyle = true;
        })
    }

    /// Sets how often the layer refetches, or `None` to stop refreshing.
    pub fn set_refresh(&mut self, id: &str, seconds: Option<f32>) -> bool {
        self.with(id, |overlay| overlay.set_refresh(seconds))
    }

    /// Refetches now, whatever the period says. A text source has nowhere to
    /// fetch from, so this does nothing to one.
    pub fn refresh(&mut self, id: &str) -> bool {
        self.with(id, |overlay| {
            if matches!(overlay.source, OverlaySource::Url(_)) {
                overlay.wants_fetch = true;
            }
        })
    }

    fn with(&mut self, id: &str, change: impl FnOnce(&mut Overlay)) -> bool {
        let Some(overlay) = self.overlays.iter_mut().find(|overlay| overlay.id == id) else {
            return false;
        };
        change(overlay);
        self.revision += 1;
        true
    }
}

// ---------------------------------------------------------------------------
// The material
// ---------------------------------------------------------------------------

/// Which of the three shapes a draw is, which is all that separates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorMode {
    Marker = 0,
    Line = 1,
    Fill = 2,
}

#[derive(Clone, Copy, Debug, ShaderType)]
pub struct VectorUniform {
    /// Linear RGBA — the style is stated in sRGB, because that is what a colour
    /// picker hands over, and converted here.
    pub color: Vec4,
    /// Marker radius, or half a line's width, in device pixels.
    pub size_px: f32,
    pub mode: u32,
    pub _padding: Vec2,
}

#[derive(Asset, AsBindGroup, TypePath, Clone)]
pub struct VectorMaterial {
    #[uniform(0)]
    pub uniform: VectorUniform,
}

impl VectorMaterial {
    fn new(mode: VectorMode, color: Srgba, size_px: f32) -> Self {
        let linear = bevy::color::LinearRgba::from(color);
        Self {
            uniform: VectorUniform {
                color: Vec4::new(linear.red, linear.green, linear.blue, linear.alpha),
                // Half-extents: the mesh spreads each corner one unit either
                // way, so the shader wants the radius rather than the diameter.
                size_px: (size_px * 0.5).max(0.1),
                mode: mode as u32,
                _padding: Vec2::ZERO,
            },
        }
    }
}

impl Material for VectorMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/vector.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/vector.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn enable_shadows() -> bool {
        false
    }

    fn enable_prepass() -> bool {
        // The prepass would draw this geometry with the *default* vertex
        // shader, which knows nothing about spreading a marker's corners — so
        // every overlay would write its depth as a cluster of degenerate
        // triangles sitting on the surface.
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // A fill's winding follows whichever way its ring happened to be drawn,
        // and a marker's flips as the globe turns under it, so neither can be
        // culled by which way it faces.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The document, as an asset
// ---------------------------------------------------------------------------

/// A parsed GeoJSON document, so that fetching, decoding and reference counting
/// are Bevy's problem rather than this module's.
#[derive(Asset, TypePath, Debug)]
pub struct GeoJsonAsset(pub GeoJson);

#[derive(Debug)]
pub enum GeoJsonLoadError {
    Io(std::io::Error),
    NotText(std::string::FromUtf8Error),
    NotGeoJson(crate::geojson::GeoJsonError),
}

impl std::fmt::Display for GeoJsonLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::NotText(error) => write!(formatter, "not text: {error}"),
            Self::NotGeoJson(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for GeoJsonLoadError {}

#[derive(Default, TypePath)]
struct GeoJsonLoader;

impl AssetLoader for GeoJsonLoader {
    type Asset = GeoJsonAsset;
    type Settings = ();
    type Error = GeoJsonLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(GeoJsonLoadError::Io)?;
        let text = String::from_utf8(bytes).map_err(GeoJsonLoadError::NotText)?;
        GeoJson::parse(&text)
            .map(GeoJsonAsset)
            .map_err(GeoJsonLoadError::NotGeoJson)
    }

    fn extensions(&self) -> &[&str] {
        &["geojson"]
    }
}

/// Serves `{slot}/{generation}.geojson` by fetching whatever URL that slot
/// holds, exactly as the imagery source serves a tile path.
struct OverlayAssetReader {
    urls: SharedOverlayUrls,
    http: WebAssetReader,
    https: WebAssetReader,
}

/// Recovers the slot and generation a path refers to.
fn parse_overlay_path(path: &Path) -> Option<(u64, u32)> {
    let mut segments = path.to_str()?.split('/');
    let slot: u64 = segments.next()?.parse().ok()?;
    let generation: u32 = segments.next()?.split('.').next()?.parse().ok()?;
    Some((slot, generation))
}

/// Adds the cache-busting parameter a refetch needs — and nothing at all to a
/// first fetch, so a URL that cannot take an extra parameter still works for
/// every layer that is not being refreshed.
fn request_url(url: &str, generation: u32) -> String {
    if generation == 0 {
        return url.to_string();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{CACHE_BUSTER}={generation}")
}

impl AssetReader for OverlayAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let not_found = || AssetReaderError::NotFound(path.to_path_buf());
        let (slot, generation) = parse_overlay_path(path).ok_or_else(not_found)?;

        // Built and the lock released before awaiting, so the guard is never
        // held across a suspension point.
        let url = {
            let urls = self.urls.read().map_err(|_| not_found())?;
            request_url(urls.get(&slot).ok_or_else(not_found)?, generation)
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
// Plugins
// ---------------------------------------------------------------------------

/// Marks an entity drawing part of an overlay, and says which one.
#[derive(Component)]
pub struct OverlayEntity(pub String);

/// The half of the feature that has to be in place before `AssetPlugin` builds:
/// the asset source documents are fetched over, and the layers themselves.
///
/// Split from [`OverlayPlugin`] for the same reason [`crate::imagery`] is split
/// from [`crate::tiles`] — an asset source can only be registered before the
/// asset server exists, and an asset type only after.
pub struct OverlaySourcePlugin {
    /// Overlays the globe starts with. An embedder adding its own from
    /// JavaScript leaves this empty.
    pub initial: Vec<OverlayRequest>,
}

impl Plugin for OverlaySourcePlugin {
    fn build(&self, app: &mut App) {
        let urls: SharedOverlayUrls = Arc::new(RwLock::new(HashMap::new()));

        // The reader outlives any one `World`, so it shares the URL table
        // through the same handle the resource writes to.
        let reader_urls = urls.clone();
        app.register_asset_source(
            OVERLAY_SOURCE,
            AssetSourceBuilder::new(move || {
                Box::new(OverlayAssetReader {
                    urls: reader_urls.clone(),
                    http: WebAssetReader::Http,
                    https: WebAssetReader::Https,
                })
            }),
        );

        let mut settings = OverlaySettings {
            enabled: true,
            overlays: Vec::new(),
            urls,
            next_slot: 0,
            revision: 0,
            retired: Vec::new(),
        };
        for request in &self.initial {
            settings.add(request.clone());
        }
        app.insert_resource(settings);
    }
}

/// Drawing overlays: the document type, its loader, the material and the
/// systems that keep the three in step.
pub struct OverlayPlugin;

impl Plugin for OverlayPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<VectorMaterial>::default())
            .init_asset::<GeoJsonAsset>()
            .init_asset_loader::<GeoJsonLoader>()
            .add_systems(
                Update,
                (
                    age_overlays,
                    fetch_overlays,
                    poll_overlays,
                    rebuild_overlays,
                    restyle_overlays,
                    orient_overlays,
                )
                    .chain()
                    .in_set(FrameSet::Apply),
            );
    }
}

/// Ages every layer, and marks the ones whose refresh period has run out.
fn age_overlays(time: Res<Time>, mut settings: ResMut<OverlaySettings>) {
    let delta = time.delta_secs();
    for overlay in &mut settings.overlays {
        overlay.age_seconds += delta;
        if overlay
            .refresh_seconds
            .is_some_and(|period| overlay.age_seconds >= period)
        {
            overlay.wants_fetch = true;
        }
    }
}

/// Starts whatever fetches are due.
fn fetch_overlays(assets: Res<AssetServer>, mut settings: ResMut<OverlaySettings>) {
    let mut changed = false;

    for overlay in &mut settings.overlays {
        if !std::mem::take(&mut overlay.wants_fetch) {
            continue;
        }

        // Generation zero is the first fetch; every one past that gets a path
        // of its own, so neither cache can answer it with what it has.
        if overlay.handle.is_some() {
            overlay.generation = overlay.generation.wrapping_add(1);
        }
        overlay.handle = Some(assets.load(overlay.asset_path()));
        overlay.age_seconds = 0.0;
        // A refresh leaves the geometry it has on screen until the new document
        // lands, so a feed that goes down does not blank the layer.
        changed |= overlay.set_status(OverlayStatus::Loading);
    }

    if changed {
        settings.revision += 1;
    }
}

/// Promotes the fetches that finished, and records the ones that failed.
fn poll_overlays(
    assets: Res<AssetServer>,
    documents: Res<Assets<GeoJsonAsset>>,
    mut settings: ResMut<OverlaySettings>,
) {
    let mut changed = false;

    for overlay in &mut settings.overlays {
        // Anything already carrying geometry it has not drawn yet is settled;
        // so is anything that is not waiting on a fetch.
        if overlay.pending.is_some() {
            continue;
        }
        let Some(handle) = overlay.handle.as_ref() else {
            continue;
        };
        match assets.get_load_state(handle) {
            Some(LoadState::Loaded) => {
                if overlay.status == OverlayStatus::Loading
                    && let Some(document) = documents.get(handle)
                {
                    overlay.pending = Some(document.0.clone());
                }
            }
            Some(LoadState::Failed(error)) => {
                changed |= overlay.set_status(OverlayStatus::Failed(error.to_string()));
            }
            _ => {}
        }
    }

    if changed {
        settings.revision += 1;
    }
}

/// Builds meshes for anything whose geometry changed, and clears away whatever
/// it replaces.
fn rebuild_overlays(
    mut commands: Commands,
    settings: ResMut<OverlaySettings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VectorMaterial>>,
    frame: Res<ReferenceFrame>,
) {
    let settings = settings.into_inner();

    for entity in settings.retired.drain(..) {
        commands.entity(entity).despawn();
    }

    let mut changed = false;
    for overlay in &mut settings.overlays {
        let Some(document) = overlay.pending.take() else {
            continue;
        };

        for part in overlay.parts.drain(..) {
            commands.entity(part.entity).despawn();
        }

        overlay.counts = OverlayCounts {
            features: document.features,
            points: document.points.len(),
            lines: document.lines.len(),
            polygons: document.polygons.len(),
        };

        let style = overlay.style;
        // Fills first, then lines, then markers: the radii above already put
        // them in that order, and building them in it keeps the two agreeing.
        let built = [
            (
                VectorMode::Fill,
                style.fill_color,
                0.0,
                fill_mesh(&document.polygons),
            ),
            (
                VectorMode::Line,
                style.line_color,
                style.line_width_px,
                line_mesh(&document.lines, &document.polygons),
            ),
            (
                VectorMode::Marker,
                style.point_color,
                style.point_size_px,
                marker_mesh(&document.points),
            ),
        ];

        for (mode, color, size_px, mesh) in built {
            let Some(mesh) = mesh else {
                continue;
            };
            let material = materials.add(VectorMaterial::new(mode, color, size_px));
            let entity = commands
                .spawn((
                    Name::new(format!("Overlay {} ({mode:?})", overlay.id)),
                    OverlayEntity(overlay.id.clone()),
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(material.clone()),
                    // `orient_overlays` keeps this in step with the frame; the
                    // spawn value only has to be right for the frame it is
                    // spawned into.
                    Transform::from_rotation(frame.earth_to_world()),
                    // Markers and lines are spread in the vertex shader, so the
                    // mesh's own bounds understate what is drawn — and a layer
                    // of a single point has no bounds at all. Culling by them
                    // would blink the layer out at the edge of the view.
                    NoFrustumCulling,
                ))
                .id();
            overlay.parts.push(OverlayPart {
                entity,
                material,
                mode,
            });
        }

        overlay.set_status(OverlayStatus::Ready);
        overlay.wants_restyle = false;
        changed = true;
    }

    if changed {
        settings.revision += 1;
    }
}

/// Applies a colour or size change without rebuilding any geometry — the mesh
/// holds anchors, and none of the style is in it.
fn restyle_overlays(
    mut settings: ResMut<OverlaySettings>,
    mut materials: ResMut<Assets<VectorMaterial>>,
) {
    for overlay in &mut settings.overlays {
        if !std::mem::take(&mut overlay.wants_restyle) {
            continue;
        }
        for part in &overlay.parts {
            let (color, size_px) = match part.mode {
                VectorMode::Marker => (overlay.style.point_color, overlay.style.point_size_px),
                VectorMode::Line => (overlay.style.line_color, overlay.style.line_width_px),
                VectorMode::Fill => (overlay.style.fill_color, 0.0),
            };
            if let Some(mut material) = materials.get_mut(&part.material) {
                *material = VectorMaterial::new(part.mode, color, size_px);
            }
        }
    }
}

/// Carries the overlays around with the globe, and draws only what should be
/// drawn — the same job `orient_tiles` does for imagery.
fn orient_overlays(
    frame: Res<ReferenceFrame>,
    settings: Res<OverlaySettings>,
    mut parts: Query<(&OverlayEntity, &mut Transform, &mut Visibility)>,
) {
    let earth_to_world = frame.earth_to_world();
    for (overlay, mut transform, mut visibility) in &mut parts {
        transform.rotation = earth_to_world;
        *visibility = if settings.enabled && settings.is_visible(&overlay.0) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}

// ---------------------------------------------------------------------------
// Meshes
// ---------------------------------------------------------------------------

/// The attributes every overlay mesh carries.
///
/// All three shapes use the same set whether or not they need all of it: the
/// vertex layout is what decides which fields of Bevy's `Vertex` struct exist,
/// and one shader serving all three has to find the same ones every time.
struct MeshBuilder {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    /// For a marker, which corner of the quad this is; for a line, which side
    /// of the spine. Either way it is also what the fragment shader softens the
    /// edge with.
    uvs: Vec<[f32; 2]>,
    /// `xyz` is the direction a line is running in and `w` which side of it
    /// this corner steps off to. Unused by markers and fills, which still carry
    /// it so that the vertex layout does not change between them.
    tangents: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

/// What a vertex that is not part of a line puts in its tangent.
const NO_TANGENT: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

impl MeshBuilder {
    fn new() -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            tangents: Vec::new(),
            indices: Vec::new(),
        }
    }

    fn push(&mut self, direction: Vec3, radius: f32, uv: [f32; 2], tangent: [f32; 4]) {
        self.positions.push((direction * radius).to_array());
        self.normals.push(direction.to_array());
        self.uvs.push(uv);
        self.tangents.push(tangent);
    }

    fn next_index(&self) -> u32 {
        self.positions.len() as u32
    }

    fn finish(self) -> Option<Mesh> {
        if self.indices.is_empty() {
            return None;
        }
        Some(
            Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::RENDER_WORLD,
            )
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
            .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, self.tangents)
            .with_inserted_indices(Indices::U32(self.indices)),
        )
    }
}

/// One quad per point, all four corners on the same anchor. The shader spreads
/// them into a disc facing the camera, and the UV says which corner is which —
/// which is also what the disc is rounded off with.
fn marker_mesh(points: &[LatLon]) -> Option<Mesh> {
    let mut builder = MeshBuilder::new();
    for point in points {
        let direction = point.to_direction();
        let base = builder.next_index();
        for corner in [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]] {
            builder.push(direction, MARKER_RADIUS, corner, NO_TANGENT);
        }
        builder
            .indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    builder.finish()
}

/// Every line, plus every polygon's rings — so a polygon still reads as a shape
/// when its fill is transparent, or when it was too big to triangulate.
fn line_mesh(lines: &[Vec<LatLon>], polygons: &[Polygon]) -> Option<Mesh> {
    let mut builder = MeshBuilder::new();
    for line in lines {
        push_ribbon(&mut builder, &densify(line, false));
    }
    for polygon in polygons {
        for ring in &polygon.rings {
            push_ribbon(&mut builder, &densify(ring, true));
        }
    }
    builder.finish()
}

/// Every polygon, filled.
///
/// The triangles ear clipping produces are exact but arbitrarily large — a
/// rectangle is two triangles whatever it spans — and a large flat triangle
/// laid across a sphere is a chord that cuts *under* it. Thirty degrees across,
/// that is two hundred kilometres beneath the ground, which is to say buried.
/// So they are refined until every edge is short, and then lifted by the sag
/// that is left.
fn fill_mesh(polygons: &[Polygon]) -> Option<Mesh> {
    let mut builder = MeshBuilder::new();
    for polygon in polygons {
        let (mut corners, indices) = tessellate::triangulate(polygon);
        let indices = refine(&mut corners, indices);
        if indices.is_empty() {
            continue;
        }

        let radius = FILL_RADIUS * chord_lift(longest_edge_degrees(&corners, &indices));
        let base = builder.next_index();
        for corner in &corners {
            builder.push(corner.to_direction(), radius, [0.0, 0.0], NO_TANGENT);
        }
        builder
            .indices
            .extend(indices.iter().map(|index| base + index));
    }
    builder.finish()
}

/// Splits triangles until none of their edges spans far enough to sag off the
/// globe, and returns the new index list. Corners are appended to in place.
///
/// Adaptive, so a polygon that is mostly small triangles does not pay for the
/// one large one — and crack-free, which is the part that constrains the
/// method. Whether an edge splits is decided *for the edge*, over the whole
/// mesh, before any triangle is rebuilt; so the two triangles sharing an edge
/// always agree about it, and a corner can never end up hanging halfway along
/// a neighbour's unsplit edge with a hairline of background showing through.
/// Each triangle is then rebuilt from however many of its three edges were
/// marked, which is why there are eight cases below rather than one.
fn refine(corners: &mut Vec<LatLon>, mut indices: Vec<u32>) -> Vec<u32> {
    let mut midpoints: HashMap<(u32, u32), u32> = HashMap::new();

    loop {
        let mut splitting: HashSet<(u32, u32)> = HashSet::new();
        for triangle in indices.chunks_exact(3) {
            for (from, to) in [
                (triangle[0], triangle[1]),
                (triangle[1], triangle[2]),
                (triangle[2], triangle[0]),
            ] {
                let span = span_degrees(corners[from as usize], corners[to as usize]);
                if span > MAX_SEGMENT_DEGREES {
                    splitting.insert(edge(from, to));
                }
            }
        }

        // Every split adds at most two triangles, so this bounds the next pass
        // rather than discovering afterwards that it was too big.
        if splitting.is_empty() || indices.len() / 3 + splitting.len() * 2 > MAX_FILL_TRIANGLES {
            return indices;
        }

        let mut refined = Vec::with_capacity(indices.len() * 2);
        for triangle in indices.chunks_exact(3) {
            let (a, b, c) = (triangle[0], triangle[1], triangle[2]);
            let mut split = |from, to| {
                splitting
                    .contains(&edge(from, to))
                    .then(|| midpoint(corners, &mut midpoints, from, to))
            };
            let (ab, bc, ca) = (split(a, b), split(b, c), split(c, a));

            // Each arm walks the triangle the same way round, so the refinement
            // keeps the winding it was given.
            refined.extend(match (ab, bc, ca) {
                (None, None, None) => vec![a, b, c],
                (Some(p), None, None) => vec![a, p, c, p, b, c],
                (None, Some(q), None) => vec![b, q, a, q, c, a],
                (None, None, Some(r)) => vec![c, r, b, r, a, b],
                (Some(p), Some(q), None) => vec![a, p, q, p, b, q, a, q, c],
                (None, Some(q), Some(r)) => vec![b, q, r, q, c, r, b, r, a],
                (Some(p), None, Some(r)) => vec![c, r, p, r, a, p, c, p, b],
                (Some(p), Some(q), Some(r)) => vec![a, p, r, p, b, q, r, q, c, p, q, r],
            });
        }
        indices = refined;
    }
}

/// An edge, named the same way from either of the triangles that share it.
fn edge(from: u32, to: u32) -> (u32, u32) {
    (from.min(to), from.max(to))
}

/// The corner halfway along an edge, made once and shared by both sides.
fn midpoint(
    corners: &mut Vec<LatLon>,
    cache: &mut HashMap<(u32, u32), u32>,
    from: u32,
    to: u32,
) -> u32 {
    *cache.entry(edge(from, to)).or_insert_with(|| {
        let (from, to) = (corners[from as usize], corners[to as usize]);
        corners.push(LatLon::new(
            (from.lat + to.lat) * 0.5,
            // Longitudes within one polygon are unwrapped to run continuously,
            // so the plain average is the point between them even across the
            // antimeridian — see `tessellate`.
            (from.lon + to.lon) * 0.5,
        ));
        corners.len() as u32 - 1
    })
}

/// How far apart two corners are, in degrees, taking the larger of the two axes.
///
/// An overestimate near the poles, where a degree of longitude is much less
/// than a degree of arc. Overestimating only refines more than it has to.
fn span_degrees(from: LatLon, to: LatLon) -> f32 {
    (from.lat - to.lat).abs().max((from.lon - to.lon).abs())
}

fn longest_edge_degrees(corners: &[LatLon], indices: &[u32]) -> f32 {
    indices
        .chunks_exact(3)
        .flat_map(|triangle| {
            [
                (triangle[0], triangle[1]),
                (triangle[1], triangle[2]),
                (triangle[2], triangle[0]),
            ]
        })
        .map(|(from, to)| span_degrees(corners[from as usize], corners[to as usize]))
        .fold(0.0, f32::max)
}

/// How far out to push corners so that the flat surface between them sits at or
/// above the radius it was meant to, rather than dipping below it.
///
/// The same correction `TileGrid::radius` applies to a tile patch, for the same
/// reason: a polygon inscribed in a sphere is entirely inside it, and what is
/// drawn on the globe has to be entirely outside.
fn chord_lift(span_degrees: f32) -> f32 {
    if span_degrees <= 0.0 {
        return 1.0;
    }
    1.0 / (span_degrees * 0.5).to_radians().cos()
}

/// A ribbon along a path: two vertices per point, one either side of the spine,
/// stepped off in the shader so the width is in pixels rather than kilometres.
fn push_ribbon(builder: &mut MeshBuilder, path: &[Vec3]) {
    let count = path.len();
    if count < 2 {
        return;
    }
    // A closed path comes back to where it started, and its two ends have to be
    // given each other's neighbour or a corner would show at the seam.
    let closed = path[0] == path[count - 1] && count > 2;

    let base = builder.next_index();
    for (index, point) in path.iter().enumerate() {
        let previous = match index {
            0 if closed => path[count - 2],
            0 => *point,
            _ => path[index - 1],
        };
        let next = if index + 1 < count {
            path[index + 1]
        } else if closed {
            path[1]
        } else {
            *point
        };

        // Across the whole corner rather than along either of its two segments,
        // so the two sides of a bend meet instead of overlapping or gapping.
        let mut along = next - previous;
        if along.length_squared() < 1.0e-12 {
            along = next - *point;
        }
        let along = along.normalize_or_zero();

        for side in [-1.0_f32, 1.0] {
            builder.push(
                *point,
                // `densify` bounds how far apart two points of a ribbon can be,
                // so the sag between them is bounded too, and lifting by it
                // keeps a line from dipping into the imagery at mid-segment.
                LINE_RADIUS * chord_lift(MAX_SEGMENT_DEGREES),
                [side, 0.0],
                [along.x, along.y, along.z, side],
            );
        }
    }

    for segment in 0..(count as u32 - 1) {
        let (left, right) = (base + segment * 2, base + segment * 2 + 1);
        builder
            .indices
            .extend([left, right, left + 2, right, right + 2, left + 2]);
    }
}

/// Walks a path, subdividing anything long enough that a straight chord would
/// leave the surface, and returns it as unit directions.
///
/// Longitude runs continuously from one corner to the next, so a step from
/// 179° E to 179° W is the two degrees it looks like on a globe rather than the
/// 358 it looks like in a table of numbers.
fn densify(path: &[LatLon], closed: bool) -> Vec<Vec3> {
    if path.is_empty() {
        return Vec::new();
    }

    let count = path.len();
    let segments = if closed { count } else { count - 1 };
    let mut out = Vec::with_capacity(count);
    out.push(path[0].to_direction());

    let mut latitude = path[0].lat;
    let mut longitude = path[0].lon;
    for index in 0..segments {
        let corner = path[(index + 1) % count];
        let target_longitude = longitude + shortest_turn(corner.lon - longitude);
        let steps = ((corner.lat - latitude)
            .abs()
            .max((target_longitude - longitude).abs())
            / MAX_SEGMENT_DEGREES)
            .ceil()
            .max(1.0) as u32;

        for step in 1..=steps {
            let fraction = step as f32 / steps as f32;
            let direction = LatLon::new(
                latitude + (corner.lat - latitude) * fraction,
                longitude + (target_longitude - longitude) * fraction,
            )
            .to_direction();
            // A repeated coordinate would leave a ribbon segment with no
            // direction to step off.
            if out
                .last()
                .is_none_or(|last| last.distance_squared(direction) > 1.0e-14)
            {
                out.push(direction);
            }
        }
        latitude = corner.lat;
        longitude = target_longitude;
    }

    out
}

/// Brings an angle in degrees into `[-180, 180)`.
fn shortest_turn(degrees: f32) -> f32 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

// ---------------------------------------------------------------------------
// What the state stream reports
// ---------------------------------------------------------------------------

/// One overlay, as an interface sees it.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OverlayInfo {
    pub id: String,
    pub label: String,
    /// `"url"` or `"text"`.
    pub source: &'static str,
    /// Where it came from, for a URL source.
    pub url: Option<String>,
    pub visible: bool,
    /// `"loading"`, `"ready"` or `"failed"`.
    pub status: &'static str,
    pub error: Option<String>,
    /// Seconds between refetches, or `null` when it is not refreshing.
    pub refresh_seconds: Option<f32>,
    /// Seconds until the next one, so an interface can count down to it.
    pub next_refresh_seconds: Option<f32>,
    /// How long ago the document on screen was asked for.
    pub age_seconds: f32,
    pub features: usize,
    pub points: usize,
    pub lines: usize,
    pub polygons: usize,
    pub style: OverlayStyleInfo,
}

/// The style, as hex colours an interface can put straight into a colour input.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OverlayStyleInfo {
    pub point_color: String,
    pub point_size_px: f32,
    pub line_color: String,
    pub line_width_px: f32,
    pub fill_color: String,
}

impl From<&OverlayStyle> for OverlayStyleInfo {
    fn from(style: &OverlayStyle) -> Self {
        Self {
            point_color: style.point_color.to_hex(),
            point_size_px: style.point_size_px,
            line_color: style.line_color.to_hex(),
            line_width_px: style.line_width_px,
            fill_color: style.fill_color.to_hex(),
        }
    }
}

/// Describes every overlay, in the order they were added.
pub fn describe(settings: &OverlaySettings) -> Vec<OverlayInfo> {
    settings
        .overlays
        .iter()
        .map(|overlay| OverlayInfo {
            id: overlay.id.clone(),
            label: overlay.label.clone(),
            source: overlay.source.id(),
            url: overlay.source.url().map(str::to_string),
            visible: overlay.visible,
            status: overlay.status.id(),
            error: overlay.status.error().map(str::to_string),
            refresh_seconds: overlay.refresh_seconds,
            next_refresh_seconds: overlay
                .refresh_seconds
                .map(|period| (period - overlay.age_seconds).max(0.0)),
            age_seconds: overlay.age_seconds,
            features: overlay.counts.features,
            points: overlay.counts.points,
            lines: overlay.counts.lines,
            polygons: overlay.counts.polygons,
            style: OverlayStyleInfo::from(&overlay.style),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    /// Planar area of a triangulation, in square degrees.
    fn area(corners: &[LatLon], indices: &[u32]) -> f32 {
        indices
            .chunks_exact(3)
            .map(|triangle| {
                let corner = |index: u32| {
                    let point = corners[index as usize];
                    Vec2::new(point.lon, point.lat)
                };
                let (a, b, c) = (
                    corner(triangle[0]),
                    corner(triangle[1]),
                    corner(triangle[2]),
                );
                (b - a).perp_dot(c - a) * 0.5
            })
            .sum()
    }

    fn big_polygon() -> Polygon {
        Polygon {
            rings: vec![vec![
                LatLon::new(30.0, -10.0),
                LatLon::new(30.0, 20.0),
                LatLon::new(50.0, 20.0),
                LatLon::new(50.0, -10.0),
            ]],
        }
    }

    #[test]
    fn refining_leaves_no_edge_long_enough_to_sag_off_the_globe() {
        let (mut corners, indices) = tessellate::triangulate(&big_polygon());
        assert!(longest_edge_degrees(&corners, &indices) > MAX_SEGMENT_DEGREES);

        let refined = refine(&mut corners, indices);
        assert!(longest_edge_degrees(&corners, &refined) <= MAX_SEGMENT_DEGREES);
    }

    #[test]
    fn refining_covers_exactly_the_same_ground() {
        let (mut corners, indices) = tessellate::triangulate(&big_polygon());
        let before = area(&corners, &indices);
        let refined = refine(&mut corners, indices);
        // Signed, so an arm of the refinement that came out wound backwards
        // would cancel rather than hide inside the total.
        assert!(
            (area(&corners, &refined) - before).abs() < 1.0e-2,
            "{before}"
        );
        assert!((before - 600.0).abs() < 1.0e-2);
    }

    #[test]
    fn a_refined_fill_is_lifted_clear_of_the_imagery() {
        let mesh = fill_mesh(&[big_polygon()]).expect("a fill");
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("positions");
        for position in positions {
            let radius = Vec3::from_array(*position).length();
            assert!(radius > MAX_TILE_RADIUS, "{radius}");
        }
    }

    #[test]
    fn overlay_paths_round_trip() {
        assert_eq!(parse_overlay_path(Path::new("3/7.geojson")), Some((3, 7)));
        assert_eq!(parse_overlay_path(Path::new("nonsense")), None);
    }

    #[test]
    fn only_a_refetch_carries_the_cache_buster() {
        let url = "https://example.org/all_hour.geojson";
        assert_eq!(request_url(url, 0), url);
        assert_eq!(request_url(url, 2), format!("{url}?{CACHE_BUSTER}=2"));
        assert_eq!(
            request_url("https://example.org/feed?format=geojson", 1),
            format!("https://example.org/feed?format=geojson&{CACHE_BUSTER}=1")
        );
    }

    #[test]
    fn a_long_segment_is_subdivided_onto_the_surface() {
        let densified = densify(&[LatLon::new(0.0, 0.0), LatLon::new(0.0, 90.0)], false);
        assert!(densified.len() > 45, "{}", densified.len());
        for point in &densified {
            assert!((point.length() - 1.0).abs() < 1.0e-5);
        }
    }

    #[test]
    fn a_step_over_the_antimeridian_is_taken_the_short_way() {
        // Two degrees, at one step of at most two: the ends and nothing in
        // between, rather than the 179 steps a wrap the wrong way would need.
        let densified = densify(&[LatLon::new(0.0, 179.0), LatLon::new(0.0, -179.0)], false);
        assert_eq!(densified.len(), 2);
    }

    #[test]
    fn a_ribbon_has_two_corners_per_point_and_two_triangles_per_segment() {
        let mut builder = MeshBuilder::new();
        let path = densify(&[LatLon::new(0.0, 0.0), LatLon::new(1.0, 0.0)], false);
        push_ribbon(&mut builder, &path);
        assert_eq!(builder.positions.len(), path.len() * 2);
        assert_eq!(builder.indices.len(), (path.len() - 1) * 6);
    }

    #[test]
    fn a_closed_ring_meshes_without_a_seam() {
        let ring = densify(
            &[
                LatLon::new(0.0, 0.0),
                LatLon::new(0.0, 1.0),
                LatLon::new(1.0, 1.0),
            ],
            true,
        );
        assert_eq!(*ring.first().unwrap(), *ring.last().unwrap());
        let mut builder = MeshBuilder::new();
        push_ribbon(&mut builder, &ring);
        assert_eq!(builder.indices.len(), (ring.len() - 1) * 6);
    }

    #[test]
    fn a_refresh_period_cannot_be_set_to_a_request_loop() {
        let mut overlay = test_overlay();
        overlay.set_refresh(Some(0.01));
        assert_eq!(overlay.refresh_seconds, Some(MIN_REFRESH_SECONDS));
        overlay.set_refresh(Some(0.0));
        assert_eq!(overlay.refresh_seconds, None);
        overlay.set_refresh(Some(f32::NAN));
        assert_eq!(overlay.refresh_seconds, None);
        overlay.set_refresh(Some(300.0));
        assert_eq!(overlay.refresh_seconds, Some(300.0));
    }

    #[test]
    fn a_text_source_is_parsed_the_moment_it_arrives() {
        let mut settings = test_settings();
        settings.add(OverlayRequest {
            id: "local".into(),
            label: String::new(),
            source: OverlaySource::Text(r#"{"type": "Point", "coordinates": [1, 2]}"#.to_string()),
            style: OverlayStyle::default(),
            refresh_seconds: Some(30.0),
            visible: true,
        });
        let overlay = &settings.overlays[0];
        // The label falls back to the id, and the geometry is already waiting.
        assert_eq!(overlay.label, "local");
        assert_eq!(
            overlay.pending.as_ref().map(|data| data.points.len()),
            Some(1)
        );
        // Nothing to fetch, and nowhere to fetch it from, so the period asked
        // for is dropped rather than counted down to a request that would fail.
        assert!(!overlay.wants_fetch);
        assert_eq!(overlay.refresh_seconds, None);
    }

    #[test]
    fn bad_text_fails_that_layer_and_nothing_else() {
        let mut settings = test_settings();
        settings.add(OverlayRequest {
            id: "broken".into(),
            label: String::new(),
            source: OverlaySource::Text("{".to_string()),
            style: OverlayStyle::default(),
            refresh_seconds: None,
            visible: true,
        });
        assert_eq!(settings.overlays[0].status.id(), "failed");
        assert_eq!(settings.drawn(), 0);
    }

    #[test]
    fn adding_under_an_id_already_in_use_replaces_it() {
        let mut settings = test_settings();
        let request = |url: &str| OverlayRequest {
            id: "feed".into(),
            label: "Feed".into(),
            source: OverlaySource::Url(url.into()),
            style: OverlayStyle::default(),
            refresh_seconds: None,
            visible: true,
        };
        settings.add(request("https://example.org/one.geojson"));
        let first_slot = settings.overlays[0].slot;
        settings.add(request("https://example.org/two.geojson"));

        assert_eq!(settings.overlays.len(), 1);
        // A fresh slot, so a response still in flight for the first cannot be
        // taken for the second's.
        assert_ne!(settings.overlays[0].slot, first_slot);
        let urls = settings.urls.read().expect("lock");
        assert_eq!(urls.len(), 1);
    }

    fn test_settings() -> OverlaySettings {
        OverlaySettings {
            enabled: true,
            overlays: Vec::new(),
            urls: Arc::new(RwLock::new(HashMap::new())),
            next_slot: 0,
            revision: 0,
            retired: Vec::new(),
        }
    }

    fn test_overlay() -> Overlay {
        Overlay {
            id: "test".into(),
            label: "test".into(),
            source: OverlaySource::Url("https://example.org/a.geojson".into()),
            slot: 0,
            generation: 0,
            style: OverlayStyle::default(),
            refresh_seconds: None,
            age_seconds: 0.0,
            visible: true,
            status: OverlayStatus::Loading,
            counts: OverlayCounts::default(),
            handle: None,
            pending: None,
            wants_fetch: false,
            wants_restyle: false,
            parts: Vec::new(),
        }
    }
}
