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
//! **Refreshing has to defeat two caches**, which is what the generation in
//! every asset path is for. That, and the `geojson://` source itself, is
//! [`crate::fetch`] — shared with [`crate::ephemeris`], which is the same
//! shape of layer over a different document.
//!
//! **Size is in pixels, not in kilometres.** A marker and a line are sized on
//! screen, so they stay legible from orbit and from a low pass without the
//! layer being rebuilt for either. That means the mesh holds anchors rather
//! than shapes, and the corners are spread in the vertex shader — see
//! `assets/shaders/vector.wgsl`, which is where the size is finally decided.
//!
//! **A document may style itself, one feature at a time.** GeoJSON has a
//! convention for it — [simplestyle-spec 1.1.0], read by [`crate::simplestyle`]
//! — and where a feature carries those members they override the layer's
//! colours and sizes for that feature alone. It stays three draws: a styled
//! feature's paint rides in its own vertices, and everything else in the same
//! mesh still follows the material's uniform, which is what keeps recolouring a
//! layer from having to rebuild it. See [`FeaturePaint`], and `simple_style` on
//! [`OverlayRequest`] for turning the whole business off.
//!
//! **Height is the layer's to interpret.** GeoJSON's third element is carried
//! through parsing unread (see [`crate::geo::Position`]) and turned into a
//! radius here, under the layer's [`OverlayAltitude`]: what unit it is in, and
//! whether it is honoured at all. It is measured up from the drape radii below
//! rather than from the sphere, so a position with no height, one at sea level
//! and one on a clamped layer all draw in the same place.
//!
//! [simplestyle-spec 1.1.0]: https://github.com/mapbox/simplestyle-spec/tree/master/1.1.0

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, RwLock};

use bevy::asset::io::Reader;
use bevy::asset::{AssetApp, AssetLoader, LoadContext, LoadState, RenderAssetUsages};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::color::Srgba;
use bevy::math::DVec2;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use serde::Serialize;

use crate::api::Cursor;
use crate::features::{Coords, FeatureSet};
use crate::fetch::{self, SharedUrls};
use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::{EARTH_RADIUS_KM, Position};
use crate::globe::GLOBE_RADIUS;
use crate::picking::{self, Hit, PickIndex, PickKind, Tolerance};
use crate::simplestyle::SimpleStyleInfo;
use crate::tessellate;
use crate::tiles::MAX_TILE_RADIUS;

/// The asset source scheme overlay documents are fetched over.
pub const OVERLAY_SOURCE: &str = "geojson";

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
const MAX_SEGMENT_DEGREES: f64 = 2.0;

/// The most triangles one polygon's fill may be refined into. Past it the fill
/// is drawn coarser — lifted further off the surface to compensate — rather
/// than abandoned.
const MAX_FILL_TRIANGLES: usize = 60_000;

/// A refresh period is clamped to this at the fast end. Anything quicker is a
/// request loop rather than a refresh, and no feed is worth polling that hard.
pub const MIN_REFRESH_SECONDS: f32 = 1.0;

/// How far off a shape the cursor may still be and count as on it, on top of
/// the shape's own size. A two-pixel line is otherwise a two-pixel target.
pub(crate) const PICK_SLACK_PX: f32 = 4.0;

/// What a picked feature is drawn in, and how much bigger.
///
/// Near-white, because it has to separate the picked feature from *any* layer
/// colour, and drawn behind rather than over — so a marker keeps its own colour
/// and gains a halo, rather than disappearing under the highlight.
pub(crate) const HIGHLIGHT_COLOR: Srgba = Srgba::new(1.0, 1.0, 1.0, 0.9);
const HIGHLIGHT_FILL: Srgba = Srgba::new(1.0, 1.0, 1.0, 0.3);
pub(crate) const HIGHLIGHT_GROW_PX: f32 = 7.0;

/// Every overlay is drawn on the same sphere, so the transparent pass — which
/// sorts by distance — has almost nothing to sort by, and would otherwise
/// interleave fills, lines and markers in whatever order they happened to
/// reach it. These are added to that distance to settle it: larger is nearer,
/// and nearer is drawn last.
const HIGHLIGHT_BEHIND: f32 = 1.0;

/// How far over the overlays an ephemeris is drawn.
///
/// A satellite is the one thing here that is genuinely somewhere else: it is
/// hundreds of kilometres above everything drawn on the surface, so on the rare
/// frame where the transparent pass cannot tell them apart by distance — a
/// grazing view along the limb, where a marker overhead and a coastline beyond
/// it are the same distance from the camera — the one in orbit is the one in
/// front. Clear of the highlight as well, so a picked feature's halo does not
/// come out over a satellite.
pub(crate) const EPHEMERIS_ABOVE: f32 = 8.0;

/// How far under the overlays a vector tile's geometry is drawn.
///
/// Enough to clear the highlight as well as the overlays themselves, because a
/// basemap is the thing annotations are drawn *on*: a coastline out of a vector
/// tile has to sit under the layer of earthquakes over it and under the halo
/// around the one that is picked. Same radius, so both are the same height
/// above the imagery — this settles the order, not the placement.
pub(crate) const VECTOR_TILE_BENEATH: f32 = 4.0;

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
/// One style for the whole layer — the unit an interface gives a colour to,
/// and what two feeds are told apart by. A feature may still override it for
/// itself, but only by the document saying so in the members of
/// [simplestyle-spec 1.1.0]; see [`crate::simplestyle`] and [`FeaturePaint`].
///
/// [simplestyle-spec 1.1.0]: https://github.com/mapbox/simplestyle-spec/tree/master/1.1.0
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

impl OverlayStyle {
    /// What this layer draws one kind of shape with.
    pub(crate) fn paint(&self, mode: VectorMode) -> Paint {
        match mode {
            VectorMode::Marker => Paint {
                color: self.point_color,
                size_px: self.point_size_px,
            },
            VectorMode::Line => Paint {
                color: self.line_color,
                size_px: self.line_width_px,
            },
            // A fill has no size on screen: it is as big as its ring.
            VectorMode::Fill => Paint {
                color: self.fill_color,
                size_px: 0.0,
            },
        }
    }
}

/// What one shape is drawn with: a colour, and a size on screen in device
/// pixels — a marker's diameter, or a line's width.
///
/// The pair rather than two arguments, because everything that decides either
/// of them decides both: the layer's style, a feature's own simplestyle over
/// it, and the highlight over that.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    pub color: Srgba,
    pub size_px: f32,
}

/// What a layer does with the height in its positions.
///
/// Two modes rather than KML's three: with no terrain model under the imagery,
/// a height above the ground and a height above the ellipsoid are the same
/// number, so `absolute` and `relativeToGround` would be one mode described
/// twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AltitudeMode {
    /// Heights are drawn: a position climbs above the surface by its third
    /// element, scaled. This is the default, because a feed that states a
    /// height generally means it.
    #[default]
    RelativeToSurface,
    /// Heights are ignored and everything is draped on the surface — which is
    /// what a layer wants when the third element is not a height at all. The
    /// USGS earthquake feeds put depth in kilometres there; read as metres up,
    /// a deep quake would hover, and read as what it is, it would be buried.
    ClampToSurface,
}

impl AltitudeMode {
    /// The stable name the control surface calls this by.
    pub fn id(self) -> &'static str {
        match self {
            Self::RelativeToSurface => "relativeToSurface",
            Self::ClampToSurface => "clampToSurface",
        }
    }

    /// Reads a mode back, `None` for a name that is not one. Clamping is worth
    /// spelling both ways round: it is the mode an interface names most often.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "relativeToSurface" | "relative" => Some(Self::RelativeToSurface),
            "clampToSurface" | "clampToGround" | "clamp" => Some(Self::ClampToSurface),
            _ => None,
        }
    }
}

/// How high a layer's positions are drawn.
///
/// Separate from [`OverlayStyle`] because it is placement rather than paint:
/// changing it moves geometry, so it rebuilds the layer's meshes, where a
/// restyle only swaps colours on the ones already built.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayAltitude {
    pub mode: AltitudeMode,
    /// Whether a polygon is joined to the ground by walls.
    ///
    /// A ring at a height is otherwise a lid hanging in the air with nothing
    /// under it. Extruded, every edge of every ring drops a wall to the surface,
    /// so a square at a height becomes a box standing on it — the same thing
    /// KML means by `<extrude>`.
    ///
    /// It is off by default and set per layer, which is the granularity
    /// everything else about a layer's appearance has. A layer of building
    /// footprints wants all of them extruded; a layer of airspace shelves wants
    /// none of them, because the shelves *are* the shape.
    pub extrude: bool,
    /// Metres of height per unit of the third element.
    ///
    /// RFC 7946 says that element is metres, and says it loosely enough that
    /// feeds disagree: a kilometre-based feed passes `1000`, and one that
    /// counts downward passes a negative scale. It is here rather than in the
    /// parser because only whoever chose the feed knows which it is.
    pub scale: f32,
}

impl Default for OverlayAltitude {
    fn default() -> Self {
        Self {
            mode: AltitudeMode::default(),
            scale: 1.0,
            extrude: false,
        }
    }
}

impl OverlayAltitude {
    /// A layer that draws everything on the surface, whatever its positions say.
    pub const CLAMPED: Self = Self {
        mode: AltitudeMode::ClampToSurface,
        scale: 1.0,
        extrude: false,
    };

    /// How far above the surface one position is drawn, in scene units.
    ///
    /// Never negative: below the surface is not somewhere the globe can draw,
    /// and a layer whose scale turns heights into depths is asking for the
    /// ground rather than for a hole in it. The USGS feeds are the case to
    /// think about — every one of their positions is a depth.
    fn lift(self, position: Position) -> f32 {
        if self.mode == AltitudeMode::ClampToSurface || !self.scale.is_finite() {
            return 0.0;
        }
        // In `f64`, because the height came out of the store at `f64` and the
        // scale is the only thing that has ever been stated at `f32`.
        let metres = position.altitude_m * f64::from(self.scale);
        if !metres.is_finite() {
            return 0.0;
        }
        let units = metres / (f64::from(EARTH_RADIUS_KM) * 1000.0) * f64::from(GLOBE_RADIUS);
        (units as f32).max(0.0)
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
    /// Whether the document's own [simplestyle-spec 1.1.0] members override
    /// that style, feature by feature. On unless an embedder says otherwise.
    ///
    /// [simplestyle-spec 1.1.0]: https://github.com/mapbox/simplestyle-spec/tree/master/1.1.0
    pub simple_style: bool,
    pub altitude: OverlayAltitude,
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
    /// How many of those carried simplestyle members of their own, which is
    /// what tells an interface why recolouring the layer left some of it alone.
    pub styled: usize,
    pub points: usize,
    pub lines: usize,
    pub polygons: usize,
}

/// One feature of one layer, named the way the globe can still find it after a
/// refresh has renumbered nothing and a removal has renumbered everything.
///
/// By slot rather than by layer id, because a slot is never reused: a pin held
/// across a layer being taken down and another put up under the same name
/// cannot silently come to mean the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pick {
    slot: u64,
    feature: usize,
    kind: PickKind,
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
    /// Whether the document's own simplestyle members are honoured over the
    /// style above. On by default — a document that took the trouble to say how
    /// it looks generally meant it — and off for an interface that wants every
    /// layer in a colour of its choosing whatever the feed thinks.
    simple_style: bool,
    altitude: OverlayAltitude,
    refresh_seconds: Option<f32>,
    /// Seconds since the last fetch was started, which is both the refresh
    /// countdown and how stale an interface should say the layer is.
    age_seconds: f32,
    visible: bool,
    status: OverlayStatus,
    counts: OverlayCounts,
    /// Held so the document stays loaded, and so its load state can be polled.
    handle: Option<Handle<GeoJsonAsset>>,
    /// The geometry on screen, kept rather than dropped after meshing: it is
    /// what the hit test runs against, what the highlight is rebuilt from, and
    /// where a picked feature's properties come from.
    ///
    /// Behind an `Arc` so a system holding `&mut OverlaySettings` can take a
    /// reference to one layer's document and still spawn entities for it.
    data: Option<Arc<FeatureSet>>,
    /// Where each of that document's shapes is, roughly, so the hit test can
    /// dismiss most of them without walking their vertices.
    index: Arc<PickIndex>,
    /// Geometry waiting to be meshed, from a text source or a finished fetch.
    pending: Option<FeatureSet>,
    /// Set when a fetch is due; cleared once one has been started.
    wants_fetch: bool,
    /// Set when the colours changed but the geometry did not.
    wants_restyle: bool,
    /// Set when the geometry has to be rebuilt from the document already in
    /// hand — a height setting changed, which moves vertices rather than
    /// recolouring them, and there is nothing to refetch.
    wants_remesh: bool,
    parts: Vec<OverlayPart>,
}

impl Overlay {
    /// What one feature of this layer is actually drawn with: the layer's own
    /// paint, with the document's simplestyle over it where there is one and
    /// where the layer is honouring it.
    ///
    /// The one answer to that question, so that the hit test, the highlight and
    /// the mesh cannot disagree about how big a marker is — which they would
    /// show by the halo sitting inside the thing it is meant to be around.
    fn drawn_paint(&self, document: &FeatureSet, feature: usize, mode: VectorMode) -> Paint {
        let base = self.style.paint(mode);
        if !self.simple_style {
            return base;
        }
        document
            .feature_style(feature)
            .map_or(base, |style| style.paint(mode, base))
    }

    /// The asset path this overlay's document is fetched over.
    fn asset_path(&self) -> String {
        fetch::asset_path(OVERLAY_SOURCE, self.slot, self.generation, "geojson")
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

/// Every drawn layer's geometry, by layer id, reachable from outside the
/// `World`.
///
/// This exists for one caller: [`crate::wasm::overlay_geometry`], which hands
/// an embedder a view straight onto the GeoArrow buffers and has no `World` to
/// ask. It is a second handle on the same `Arc` the layer is drawing from, so
/// publishing into it is a reference count and no copy.
///
/// Global rather than threaded through the plugin because the binding is a free
/// function that JavaScript calls whenever it likes, not a system with access
/// to resources — the same reason [`crate::api`] keeps its command queue here.
static GEOMETRY: LazyLock<RwLock<HashMap<String, Arc<FeatureSet>>>> =
    LazyLock::new(RwLock::default);

/// The geometry of one layer as it is currently drawn, or `None` for a layer
/// that is not up or has not loaded.
///
/// Only the WebAssembly binding asks; a native host holding the `App` reads the
/// resource directly.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn geometry_of(id: &str) -> Option<Arc<FeatureSet>> {
    GEOMETRY.read().ok()?.get(id).cloned()
}

/// Every overlay, and the master switch over all of them.
#[derive(Resource)]
pub struct OverlaySettings {
    /// Whether overlays are drawn at all. Off, every layer stays loaded and
    /// simply stops being drawn, so switching back is instant.
    pub enabled: bool,
    /// Whether the cursor picks features at all. Off, nothing is hovered and
    /// nothing is highlighted; a pin already set stays set.
    pub picking: bool,
    overlays: Vec<Overlay>,
    /// What the cursor is over now.
    hovered: Option<Pick>,
    /// What an embedder asked to keep, whatever the cursor does afterwards.
    /// This is what a click becomes: the globe reports what is under the
    /// pointer, and the interface decides that one of those is the selection.
    pinned: Option<Pick>,
    /// What the highlight currently draws, so it is only rebuilt when it
    /// changes rather than on every frame the cursor moves within a feature.
    highlighted: Option<Pick>,
    highlight: Vec<Entity>,
    urls: SharedUrls,
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

    /// What the highlight should be drawing: the pin if there is one, and
    /// otherwise whatever the cursor is over.
    fn highlight_target(&self) -> Option<Pick> {
        self.pinned.or(self.hovered)
    }

    /// Keeps a feature selected until told otherwise. Returns whether the layer
    /// and feature exist to be pinned.
    pub fn pin(&mut self, id: &str, feature: usize) -> bool {
        let Some(overlay) = self.overlays.iter().find(|overlay| overlay.id == id) else {
            return false;
        };
        let Some(document) = overlay.data.as_ref() else {
            return false;
        };
        let Some(kind) = picking::kind_of(document, feature) else {
            return false;
        };
        self.pinned = Some(Pick {
            slot: overlay.slot,
            feature,
            kind,
        });
        self.revision += 1;
        true
    }

    pub fn clear_pin(&mut self) {
        if self.pinned.take().is_some() {
            self.revision += 1;
        }
    }

    /// Forgets any pick that named this layer — after a refresh, because
    /// feature seven of the new document is a different earthquake, and after a
    /// removal, because there is no feature seven at all.
    fn forget_picks(&mut self, slot: u64) {
        for pick in [&mut self.hovered, &mut self.pinned] {
            if pick.is_some_and(|pick| pick.slot == slot) {
                *pick = None;
                self.revision += 1;
            }
        }
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
            simple_style: request.simple_style,
            altitude: request.altitude,
            refresh_seconds: None,
            age_seconds: 0.0,
            visible: request.visible,
            status: OverlayStatus::Loading,
            counts: OverlayCounts::default(),
            handle: None,
            data: None,
            index: Arc::default(),
            pending: None,
            // A URL is fetched on the next tick; text is already here, so it is
            // parsed now and the failure, if it is one, reported straight away.
            wants_fetch: matches!(request.source, OverlaySource::Url(_)),
            wants_restyle: false,
            wants_remesh: false,
            parts: Vec::new(),
            source: request.source,
        };
        overlay.set_refresh(request.refresh_seconds);
        if let OverlaySource::Text(text) = &overlay.source {
            match crate::geojson::parse(text) {
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
        if let Ok(mut published) = GEOMETRY.write() {
            published.remove(&overlay.id);
        }
        self.forget_picks(overlay.slot);
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
        // The highlight is sized from the layer's style, and it is rebuilt
        // rather than restyled, so it has to be made stale by hand.
        self.highlighted = None;
        self.with(id, |overlay| {
            overlay.style = style;
            overlay.wants_restyle = true;
        })
    }

    /// Sets whether the document's own simplestyle members are honoured.
    ///
    /// A remesh rather than a restyle, because where a feature's own colour
    /// goes is into its vertices — which is exactly what makes it survive a
    /// restyle. Nothing is refetched: the document already in hand is rebuilt,
    /// so a pinned feature stays pinned.
    pub fn set_simple_style(&mut self, id: &str, simple_style: bool) -> bool {
        self.highlighted = None;
        self.with(id, |overlay| {
            if overlay.simple_style != simple_style {
                overlay.simple_style = simple_style;
                overlay.wants_remesh = true;
            }
        })
    }

    /// Sets how a layer reads the heights in its positions, rebuilding its
    /// geometry where it stands — no refetch, and from the document already
    /// loaded, so a pinned feature stays pinned.
    pub fn set_altitude(&mut self, id: &str, altitude: OverlayAltitude) -> bool {
        // The highlight is built from the same geometry, and would otherwise be
        // left behind at the height the layer has just left.
        self.highlighted = None;
        self.with(id, |overlay| {
            if overlay.altitude != altitude {
                overlay.altitude = altitude;
                overlay.wants_remesh = true;
            }
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

impl VectorMode {
    /// Where this shape sits in the transparent pass, relative to the rest of
    /// the overlay. Markers over lines over fills, which is the order they have
    /// to be in for a marker on a filled country to be visible at all.
    fn depth_bias(self) -> f32 {
        match self {
            Self::Marker => 2.0,
            Self::Line => 1.0,
            Self::Fill => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, ShaderType)]
pub struct VectorUniform {
    /// Linear RGBA — the style is stated in sRGB, because that is what a colour
    /// picker hands over, and converted here.
    ///
    /// What the layer is painted with. A vertex carrying a colour of its own
    /// overrides it, one vertex at a time; see [`FeaturePaint`].
    pub color: Vec4,
    /// Marker radius, or half a line's width, in device pixels. Overridden the
    /// same way, and halved the same way when it is.
    pub size_px: f32,
    pub mode: u32,
    pub _padding: Vec2,
}

#[derive(Asset, AsBindGroup, TypePath, Clone)]
pub struct VectorMaterial {
    #[uniform(0)]
    pub uniform: VectorUniform,
    /// Added to the distance the transparent pass sorts by. Not a binding —
    /// `AsBindGroup` ignores a field it was given no attribute for.
    pub depth_bias: f32,
}

impl VectorMaterial {
    pub(crate) fn new(mode: VectorMode, paint: Paint) -> Self {
        let linear = bevy::color::LinearRgba::from(paint.color);
        Self {
            depth_bias: mode.depth_bias(),
            uniform: VectorUniform {
                color: Vec4::new(linear.red, linear.green, linear.blue, linear.alpha),
                // Half-extents: the mesh spreads each corner one unit either
                // way, so the shader wants the radius rather than the diameter.
                size_px: (paint.size_px * 0.5).max(0.1),
                mode: mode as u32,
                _padding: Vec2::ZERO,
            },
        }
    }

    /// Puts this draw behind the ordinary overlay geometry, so a highlight
    /// drawn larger reads as a halo around what it is highlighting rather than
    /// as a blob over it.
    pub(crate) fn behind(mut self) -> Self {
        self.depth_bias -= HIGHLIGHT_BEHIND;
        self
    }

    /// Puts this draw under every overlay — what a vector tile basemap is
    /// drawn with. See [`VECTOR_TILE_BENEATH`].
    pub(crate) fn beneath(mut self) -> Self {
        self.depth_bias -= VECTOR_TILE_BENEATH;
        self
    }

    /// Puts this draw over every overlay — what an ephemeris is drawn with.
    /// See [`EPHEMERIS_ABOVE`].
    pub(crate) fn above(mut self) -> Self {
        self.depth_bias += EPHEMERIS_ABOVE;
        self
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

    fn depth_bias(&self) -> f32 {
        self.depth_bias
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
pub struct GeoJsonAsset(pub FeatureSet);

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
        crate::geojson::parse(&text)
            .map(GeoJsonAsset)
            .map_err(GeoJsonLoadError::NotGeoJson)
    }

    fn extensions(&self) -> &[&str] {
        &["geojson"]
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
        let urls: SharedUrls = Arc::new(RwLock::new(HashMap::new()));

        // The reader outlives any one `World`, so it shares the URL table
        // through the same handle the resource writes to.
        app.register_asset_source(OVERLAY_SOURCE, fetch::source(urls.clone()));

        let mut settings = OverlaySettings {
            enabled: true,
            picking: true,
            overlays: Vec::new(),
            hovered: None,
            pinned: None,
            highlighted: None,
            highlight: Vec::new(),
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
                    pick_features,
                    highlight_pick,
                    orient_overlays,
                )
                    .chain()
                    .in_set(FrameSet::Apply)
                    // The cursor is cast onto the globe before any of this, and
                    // the snapshot goes out after it, so what is reported as
                    // picked is what is highlighted on the same frame.
                    .after(crate::api::track_cursor)
                    .before(crate::api::publish_state),
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
    let mut stale = Vec::new();
    for overlay in &mut settings.overlays {
        // Two ways to get here. New geometry has arrived, or the one already up
        // has to be built again at a different height — same document, so its
        // features are the same features and a pick on one still means what it
        // meant.
        let remesh = std::mem::take(&mut overlay.wants_remesh);
        let document = match overlay.pending.take() {
            Some(document) => {
                overlay.counts = OverlayCounts {
                    features: document.feature_count(),
                    styled: document.styled_features(),
                    points: document.point_count(),
                    lines: document.line_count(),
                    polygons: document.polygon_count(),
                };
                overlay.index = Arc::new(PickIndex::build(&document));
                let document = Arc::new(document);
                overlay.data = Some(document.clone());
                if let Ok(mut published) = GEOMETRY.write() {
                    published.insert(overlay.id.clone(), document.clone());
                }
                // Whatever was picked was picked in the document this one
                // replaces, and feature seven of a refreshed feed is a
                // different earthquake.
                stale.push(overlay.slot);
                document
            }
            None if remesh => match overlay.data.clone() {
                Some(document) => document,
                // Nothing loaded yet: whatever arrives will be built at the new
                // height anyway.
                None => continue,
            },
            None => continue,
        };

        for part in overlay.parts.drain(..) {
            commands.entity(part.entity).despawn();
        }

        let style = overlay.style;
        let altitude = overlay.altitude;
        // What the document is allowed to say about its own appearance. A layer
        // told to ignore it draws as if the members were not there at all —
        // which is also what makes turning the toggle back and forth a remesh
        // rather than a restyle.
        let paint = |mode| {
            if overlay.simple_style {
                FeaturePaint::Over(style.paint(mode))
            } else {
                FeaturePaint::Layer
            }
        };
        // Fills first, then lines, then markers: the radii above already put
        // them in that order, and building them in it keeps the two agreeing.
        let built = [
            (
                VectorMode::Fill,
                fill_mesh(&document, None, altitude, paint(VectorMode::Fill)),
            ),
            (
                VectorMode::Line,
                line_mesh(&document, None, true, altitude, paint(VectorMode::Line)),
            ),
            (
                VectorMode::Marker,
                marker_mesh(&document, None, altitude, paint(VectorMode::Marker)),
            ),
        ];

        for (mode, mesh) in built {
            let Some(mesh) = mesh else {
                continue;
            };
            let material = materials.add(VectorMaterial::new(mode, style.paint(mode)));
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

    for slot in stale {
        settings.forget_picks(slot);
    }
    if changed {
        settings.revision += 1;
    }
}

/// Applies a colour or size change without rebuilding any geometry — the mesh
/// holds anchors, and none of the *layer's* style is in it.
///
/// A feature that styled itself is the one thing this does not reach, and that
/// is the intended answer rather than a limitation: a document that asked for a
/// red ring asked for a red ring, and recolouring the layer around it should
/// leave it red. Everything else in the same mesh follows the uniform as it
/// always did.
fn restyle_overlays(
    mut settings: ResMut<OverlaySettings>,
    mut materials: ResMut<Assets<VectorMaterial>>,
) {
    for overlay in &mut settings.overlays {
        if !std::mem::take(&mut overlay.wants_restyle) {
            continue;
        }
        for part in &overlay.parts {
            if let Some(mut material) = materials.get_mut(&part.material) {
                *material = VectorMaterial::new(part.mode, overlay.style.paint(part.mode));
            }
        }
    }
}

/// Works out which feature the cursor is over.
///
/// Every visible layer is asked, and the best answer across all of them wins by
/// the same rule as within one — a marker over a line over a fill, and the
/// nearest of whichever kind — so that a marker in one layer is not lost under
/// a country in another.
fn pick_features(
    cursor: Res<Cursor>,
    camera: Single<(&Camera, &Transform, &Projection)>,
    frame: Res<ReferenceFrame>,
    mut settings: ResMut<OverlaySettings>,
) {
    let hovered = (settings.enabled && settings.picking)
        .then(|| cursor.ground)
        .flatten()
        .and_then(|cursor| {
            let (camera, camera_transform, projection) = *camera;
            let Projection::Perspective(perspective) = projection else {
                // "Pixels at this distance" does not mean anything under a
                // projection that has no distance in it.
                return None;
            };
            // The cursor is Earth-fixed; the camera is wherever the frame put it.
            let target = frame.earth_to_world() * cursor.to_direction() * GLOBE_RADIUS;
            let tolerance = degrees_per_pixel(
                (camera_transform.translation - target).length(),
                perspective.fov,
                camera.logical_viewport_size()?.y,
            );

            let mut best: Option<(Pick, Hit)> = None;
            for overlay in &settings.overlays {
                if !overlay.visible || overlay.status != OverlayStatus::Ready {
                    continue;
                }
                let Some(document) = overlay.data.as_ref() else {
                    continue;
                };
                let Some(hit) = overlay.index.pick(
                    document,
                    cursor,
                    Tolerance {
                        degrees_per_pixel: tolerance,
                        point_px: overlay.style.point_size_px,
                        line_px: overlay.style.line_width_px,
                        slack_px: PICK_SLACK_PX,
                        // A feature drawn at the size it asked for has to be
                        // grabbable at that size, or a large marker would have
                        // a small target and a thin one a fat one.
                        simple_style: overlay.simple_style,
                    },
                ) else {
                    continue;
                };
                if best.is_none_or(|(_, held)| hit.beats(held)) {
                    best = Some((
                        Pick {
                            slot: overlay.slot,
                            feature: hit.feature,
                            kind: hit.kind,
                        },
                        hit,
                    ));
                }
            }
            best.map(|(pick, _)| pick)
        });

    if settings.hovered != hovered {
        settings.hovered = hovered;
        settings.revision += 1;
    }
}

/// How many degrees of globe one pixel covers at a given depth, which is what
/// turns a tolerance stated in pixels into one the hit test can use.
///
/// The same arithmetic the tile streamer sizes its pyramid with, run the other
/// way: there it asks how many pixels a patch of ground covers, here how much
/// ground a pixel does.
fn degrees_per_pixel(depth: f32, fov: f32, viewport_height: f32) -> f32 {
    let focal_pixels = viewport_height / (2.0 * (fov * 0.5).tan());
    // World units per pixel at that depth, then as arc along the surface.
    (depth / focal_pixels.max(1.0e-6) / GLOBE_RADIUS).to_degrees()
}

/// Draws the picked feature again, larger and behind itself.
///
/// Rebuilt only when the pick changes, which is what keeps moving the cursor
/// within one feature from re-meshing it sixty times a second.
fn highlight_pick(
    mut commands: Commands,
    settings: ResMut<OverlaySettings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VectorMaterial>>,
    frame: Res<ReferenceFrame>,
) {
    let settings = settings.into_inner();
    let target = settings.highlight_target();
    if target == settings.highlighted {
        return;
    }

    for entity in settings.highlight.drain(..) {
        commands.entity(entity).despawn();
    }
    settings.highlighted = target;

    let Some(pick) = target else {
        return;
    };
    let Some(overlay) = settings
        .overlays
        .iter()
        .find(|overlay| overlay.slot == pick.slot)
    else {
        return;
    };
    let Some(document) = overlay.data.clone() else {
        return;
    };

    let altitude = overlay.altitude;
    // The same builders the layer itself was drawn with, told to walk past
    // every shape that is not this feature's. Cheap, because walking past a
    // shape is reading one integer out of the owner column — which is what
    // having that column separate from the coordinates buys.
    let only = Some(pick.feature as u32);
    // A halo has to be bigger than the thing it is around, so it is sized from
    // what that feature was actually drawn at — its own `stroke-width` or
    // `marker-size` where the document gave it one, and the layer's otherwise.
    // One feature, one size: nothing here needs paint per vertex.
    let grown = |mode| Paint {
        color: match mode {
            VectorMode::Fill => HIGHLIGHT_FILL,
            _ => HIGHLIGHT_COLOR,
        },
        size_px: overlay.drawn_paint(&document, pick.feature, mode).size_px + HIGHLIGHT_GROW_PX,
    };

    let built = [
        (
            VectorMode::Fill,
            fill_mesh(&document, only, altitude, FeaturePaint::Layer),
        ),
        (
            VectorMode::Line,
            line_mesh(&document, only, true, altitude, FeaturePaint::Layer),
        ),
        (
            VectorMode::Marker,
            marker_mesh(&document, only, altitude, FeaturePaint::Layer),
        ),
    ];

    for (mode, mesh) in built {
        let Some(mesh) = mesh else {
            continue;
        };
        let entity = commands
            .spawn((
                Name::new(format!("Highlight {} #{}", overlay.id, pick.feature)),
                // Named as part of the layer, so it is carried around with the
                // globe and hidden with the layer like everything else of it.
                OverlayEntity(overlay.id.clone()),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(materials.add(VectorMaterial::new(mode, grown(mode)).behind())),
                Transform::from_rotation(frame.earth_to_world()),
                NoFrustumCulling,
            ))
            .id();
        settings.highlight.push(entity);
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

/// Where a mesh's colours come from.
///
/// [`FeaturePaint::Layer`] is every shape the same, which is what the
/// material's uniform already does: the mesh carries no colour at all, and a
/// restyle swaps the layer's colours without touching a vertex. It is what a
/// vector tile, an ephemeris and a highlight are built with, and what an
/// ordinary overlay is built with too until a document turns up that styles
/// itself.
///
/// [`FeaturePaint::Over`] is such a document. Every vertex carries the paint
/// its own feature asked for, over the layer paint given here — and a vertex
/// whose feature asked for nothing carries [`INHERIT`] instead, which sends the
/// shader back to the uniform. That is the part worth keeping: a document where
/// one country is red stays one draw, still restyles the other nine hundred
/// from the uniform, and keeps the red one red while it does.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FeaturePaint {
    /// The material decides, for everything in the mesh.
    Layer,
    /// The document decides, feature by feature, over this layer paint.
    Over(Paint),
}

impl FeaturePaint {
    /// Whether the mesh has to carry paint per vertex at all. A document that
    /// styles nothing — which is most of them, and every vector tile — pays
    /// nothing for this, not even an attribute of sentinels.
    fn per_vertex(self, document: &FeatureSet) -> bool {
        matches!(self, Self::Over(_)) && document.styles_paint()
    }

    /// What one feature's shapes are drawn with, or `None` to leave it to the
    /// material — which is what a feature that styled nothing wants, so that a
    /// restyle still reaches it.
    fn of(self, document: &FeatureSet, feature: u32, mode: VectorMode) -> Option<Paint> {
        let Self::Over(base) = self else {
            return None;
        };
        let paint = document.feature_style(feature as usize)?.paint(mode, base);
        (paint != base).then_some(paint)
    }
}

/// The attributes every overlay mesh carries.
///
/// All three shapes use the same set whether or not they need all of it: the
/// vertex layout is what decides which fields of Bevy's `Vertex` struct exist,
/// and one shader serving all three has to find the same ones every time.
///
/// The paint attributes are the exception, and they are an exception on
/// purpose: they are either on every vertex of a mesh or on none of it, and the
/// shader finds them through the `VERTEX_COLORS` and `VERTEX_UVS_B` definitions
/// Bevy sets from the layout. A layer that does not need them draws through a
/// pipeline that does not have them, and its vertices stay the size they were.
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
    /// Linear RGBA per vertex, and the size in pixels beside it, for a mesh
    /// built from a document that styles itself. Empty otherwise, and then no
    /// attribute is inserted at all.
    colors: Vec<[f32; 4]>,
    sizes: Vec<[f32; 2]>,
    /// Whether the two above are being filled, decided once for the whole mesh.
    per_vertex_paint: bool,
    /// What [`Self::push`] writes into them, until it is set again. `None` is
    /// the sentinel: this vertex takes the material's word for it.
    paint: Option<Paint>,
    indices: Vec<u32>,
}

/// What a vertex that is not part of a line puts in its tangent.
const NO_TANGENT: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

/// What a vertex writes when its feature said nothing about how to paint it:
/// a negative alpha and a negative size, neither of which is a value anything
/// could otherwise mean, so the shader reads either one as "ask the uniform".
const INHERIT_COLOR: [f32; 4] = [0.0, 0.0, 0.0, -1.0];
const INHERIT_SIZE: [f32; 2] = [-1.0, 0.0];

impl MeshBuilder {
    fn new(per_vertex_paint: bool) -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            tangents: Vec::new(),
            colors: Vec::new(),
            sizes: Vec::new(),
            per_vertex_paint,
            paint: None,
            indices: Vec::new(),
        }
    }

    /// Sets what the vertices pushed from here on are painted with.
    ///
    /// Held on the builder rather than passed to [`Self::push`] because a shape
    /// is pushed from half a dozen places — a ribbon, a corner post, a wall —
    /// and all of them would only be carrying it through.
    fn paint(&mut self, paint: Option<Paint>) {
        self.paint = paint;
    }

    fn push(&mut self, direction: Vec3, radius: f32, uv: [f32; 2], tangent: [f32; 4]) {
        self.positions.push((direction * radius).to_array());
        self.normals.push(direction.to_array());
        self.uvs.push(uv);
        self.tangents.push(tangent);
        if self.per_vertex_paint {
            match self.paint {
                Some(paint) => {
                    // Linear, because that is what a vertex colour means to a
                    // shader — the sRGB the style was stated in is converted
                    // here exactly as the uniform's is.
                    let linear = bevy::color::LinearRgba::from(paint.color);
                    self.colors
                        .push([linear.red, linear.green, linear.blue, linear.alpha]);
                    self.sizes.push([paint.size_px, 0.0]);
                }
                None => {
                    self.colors.push(INHERIT_COLOR);
                    self.sizes.push(INHERIT_SIZE);
                }
            }
        }
    }

    fn next_index(&self) -> u32 {
        self.positions.len() as u32
    }

    fn finish(self) -> Option<Mesh> {
        if self.indices.is_empty() {
            return None;
        }
        let mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, self.tangents)
        .with_inserted_indices(Indices::U32(self.indices));
        if !self.per_vertex_paint {
            return Some(mesh);
        }
        Some(
            mesh.with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.colors)
                .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, self.sizes),
        )
    }
}

/// Where one position is drawn, in scene units.
///
/// Height is measured from the radius its kind is draped at rather than from
/// the globe itself, which is both simpler and more nearly true. That drape is
/// what clears the imagery, and the imagery is the ground as far as anything
/// looking at the screen is concerned — a tile stands up to eleven kilometres
/// proud of the sphere at its corners (see [`MAX_TILE_RADIUS`]), so a height
/// measured from the sphere would be swallowed whole below that, and the first
/// ten kilometres of every flight path would lie flat.
///
/// `lift` is the chord correction the mesh is using. It multiplies the height
/// as well as the base, because the sag between two vertices is a fraction of
/// their radius rather than a fixed distance.
fn radius_of(position: Position, altitude: OverlayAltitude, base: f32, lift: f32) -> f32 {
    (base + altitude.lift(position)) * lift
}

/// How far out a marker is drawn, in scene units.
///
/// A marker is a flat quad on one anchor, so there is no span across it to sag:
/// no chord correction of its own.
fn marker_radius(position: Position, altitude: OverlayAltitude) -> f32 {
    radius_of(position, altitude, MARKER_RADIUS, 1.0)
}

/// Where a marker's anchor is in world space — the point the shader spreads its
/// quad around.
///
/// Public to the crate because [`crate::ephemeris`] hit-tests against it: a
/// satellite is picked where its marker was drawn rather than where its
/// coordinate stands on the ground, and this is the one place that says where
/// that is. Two answers to that would be two answers, and the halo would sit
/// beside the thing it is meant to be around.
pub(crate) fn marker_anchor(position: Position, altitude: OverlayAltitude) -> Vec3 {
    position.to_direction() * marker_radius(position, altitude)
}

/// One quad per point, all four corners on the same anchor. The shader spreads
/// them into a disc facing the camera, and the UV says which corner is which —
/// which is also what the disc is rounded off with.
pub(crate) fn marker_mesh(
    document: &FeatureSet,
    only: Option<u32>,
    altitude: OverlayAltitude,
    paint: FeaturePaint,
) -> Option<Mesh> {
    let mut builder = MeshBuilder::new(paint.per_vertex(document));
    for (feature, point) in document.points() {
        if only.is_some_and(|wanted| wanted != feature) {
            continue;
        }
        builder.paint(paint.of(document, feature, VectorMode::Marker));
        let direction = point.to_direction();
        let radius = marker_radius(point, altitude);
        let base = builder.next_index();
        for corner in [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]] {
            builder.push(direction, radius, corner, NO_TANGENT);
        }
        builder
            .indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    builder.finish()
}

/// Every line, plus every polygon's rings — so a polygon still reads as a shape
/// when its fill is transparent, or when it was too big to triangulate.
///
/// Extruded, a ring is outlined three times over: around its own corners, again
/// around the footprint on the ground, and up the corner posts joining the two.
/// The walls [`push_walls`] builds are a flat wash of one colour with no edge
/// between the faces, because the overlay is drawn unlit — so without these the
/// only crisp thing in the drawing is the lid's outline, and a box reads as a
/// square hanging in the air. See [`push_ring_edges`].
pub(crate) fn line_mesh(
    document: &FeatureSet,
    only: Option<u32>,
    outline_rings: bool,
    altitude: OverlayAltitude,
    paint: FeaturePaint,
) -> Option<Mesh> {
    let mut builder = MeshBuilder::new(paint.per_vertex(document));
    for (feature, line) in document.lines() {
        if only.is_some_and(|wanted| wanted != feature) {
            continue;
        }
        builder.paint(paint.of(document, feature, VectorMode::Line));
        push_ribbon(&mut builder, &densify(line, false, altitude, LINE_RADIUS));
    }
    if outline_rings {
        for (feature, polygon) in document.polygons() {
            if only.is_some_and(|wanted| wanted != feature) {
                continue;
            }
            // A ring's outline is a stroke, so it takes `stroke` and
            // `stroke-width` — the same members the feature's lines take.
            builder.paint(paint.of(document, feature, VectorMode::Line));
            for ring in polygon.rings() {
                push_ribbon(&mut builder, &densify(ring, true, altitude, LINE_RADIUS));
                if altitude.extrude {
                    push_ring_edges(&mut builder, ring, altitude);
                }
            }
        }
    }
    builder.finish()
}

/// What an extruded ring is outlined with besides itself: a post at every
/// corner, dropped to the ground, and the footprint those posts stand on.
///
/// Corners rather than densified steps, which is the whole difference between
/// an edge and a fence. A post is radial, so it is straight in space however
/// far it runs and has no sag to subdivide away; the footprint is a path across
/// the globe like any other, so it is densified like any other.
///
/// A ring already on the ground has nothing to stand up from, and drawing its
/// footprint would be drawing the ring a second time over itself — so a ring
/// with no corner off the ground is left exactly as an unextruded one.
fn push_ring_edges(builder: &mut MeshBuilder, ring: Coords<'_>, altitude: OverlayAltitude) {
    let lift = chord_lift(MAX_SEGMENT_DEGREES);
    let floor = LINE_RADIUS * lift;

    let mut standing = false;
    for index in 0..ring.len() {
        let corner = ring.get(index);
        let radius = radius_of(corner, altitude, LINE_RADIUS, lift);
        // A corner on the ground is already on its own footprint.
        if radius <= floor {
            continue;
        }
        standing = true;
        push_post(builder, corner.to_direction(), floor, radius);
    }

    if standing {
        // The same ring, drawn as if it had no heights at all.
        push_ribbon(
            builder,
            &densify(ring, true, OverlayAltitude::CLAMPED, LINE_RADIUS),
        );
    }
}

/// One corner post: a ribbon straight out from the globe, from `floor` to
/// `radius`.
///
/// Not [`push_ribbon`], which works out which way a path is running from the
/// difference between neighbouring points — and the two ends of a post lie in
/// the same direction, so that difference is zero and the ribbon would come out
/// with no width. A post runs *outward*, so its own direction is what the
/// shader has to step off from.
fn push_post(builder: &mut MeshBuilder, direction: Vec3, floor: f32, radius: f32) {
    let base = builder.next_index();
    for end in [floor, radius] {
        for side in [-1.0_f32, 1.0] {
            builder.push(
                direction,
                end,
                [side, 0.0],
                [direction.x, direction.y, direction.z, side],
            );
        }
    }
    builder
        .indices
        .extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
}

/// Every polygon, filled.
///
/// The triangles ear clipping produces are exact but arbitrarily large — a
/// rectangle is two triangles whatever it spans — and a large flat triangle
/// laid across a sphere is a chord that cuts *under* it. Thirty degrees across,
/// that is two hundred kilometres beneath the ground, which is to say buried.
/// So they are refined until every edge is short, and then lifted by the sag
/// that is left.
///
/// A fill is drawn at one height across the whole polygon — the mean of its
/// outer ring — where its outline follows every corner's own. Triangulation
/// duplicates and reorders corners (see [`crate::tessellate`]), so a height per
/// corner would have to be carried through ear clipping to reach the mesh, and
/// what it would buy is a fill that folds. A lid at the average height, ringed
/// by an outline that climbs, is both simpler and easier to read.
pub(crate) fn fill_mesh(
    document: &FeatureSet,
    only: Option<u32>,
    altitude: OverlayAltitude,
    paint: FeaturePaint,
) -> Option<Mesh> {
    let mut builder = MeshBuilder::new(paint.per_vertex(document));
    for (feature, polygon) in document.polygons() {
        if only.is_some_and(|wanted| wanted != feature) {
            continue;
        }
        builder.paint(paint.of(document, feature, VectorMode::Fill));
        let (mut corners, indices) = tessellate::triangulate(polygon);
        let indices = refine(&mut corners, indices);
        if indices.is_empty() {
            continue;
        }

        let lift = chord_lift(longest_edge_degrees(&corners, &indices));
        let radius = radius_of(mean_height(polygon.outer()), altitude, FILL_RADIUS, lift);
        let base = builder.next_index();
        for corner in &corners {
            // The corner is still `f64` here, and is narrowed exactly once, on
            // its way into the vertex buffer.
            builder.push(
                crate::geo::direction(corner.y, corner.x),
                radius,
                [0.0, 0.0],
                NO_TANGENT,
            );
        }
        builder
            .indices
            .extend(indices.iter().map(|index| base + index));

        // The lid is only half of an extruded shape. Walls are part of the fill
        // rather than a draw of their own: they are the same surface seen edge
        // on, and a wall in a different colour from the lid it holds up would
        // read as two shapes rather than one solid.
        if altitude.extrude {
            for ring in polygon.rings() {
                push_walls(&mut builder, ring, altitude);
            }
        }
    }
    builder.finish()
}

/// Walls joining a ring to the ground under it: a quad per step, from each
/// point's own height down to the surface.
///
/// The ring is densified first, so a wall around anything large follows the
/// curve of the globe instead of cutting through it, and so a ring whose
/// corners are at different heights gets a wall whose top edge slopes the way
/// its outline does.
///
/// A hole gets walls too, which is what makes an extruded ring with a hole read
/// as a shape with a shaft through it rather than as a lid with a gap.
fn push_walls(builder: &mut MeshBuilder, ring: Coords<'_>, altitude: OverlayAltitude) {
    let floor = FILL_RADIUS * chord_lift(MAX_SEGMENT_DEGREES);
    let path = densify(ring, true, altitude, FILL_RADIUS);

    for step in path.windows(2) {
        let (from, to) = (step[0], step[1]);
        // A ring already on the ground has no wall to draw, and a wall of no
        // height is two degenerate triangles.
        if from.radius <= floor && to.radius <= floor {
            continue;
        }

        let base = builder.next_index();
        // Anticlockwise seen from outside, though nothing depends on it: the
        // overlay material culls no faces, because a fill's winding follows
        // whichever way its ring happened to be drawn.
        builder.push(from.direction, floor, [0.0, 0.0], NO_TANGENT);
        builder.push(to.direction, floor, [0.0, 0.0], NO_TANGENT);
        builder.push(to.direction, to.radius, [0.0, 0.0], NO_TANGENT);
        builder.push(from.direction, from.radius, [0.0, 0.0], NO_TANGENT);
        builder
            .indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
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
fn refine(corners: &mut Vec<DVec2>, mut indices: Vec<u32>) -> Vec<u32> {
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
    corners: &mut Vec<DVec2>,
    cache: &mut HashMap<(u32, u32), u32>,
    from: u32,
    to: u32,
) -> u32 {
    *cache.entry(edge(from, to)).or_insert_with(|| {
        let (from, to) = (corners[from as usize], corners[to as usize]);
        // Longitudes within one polygon are unwrapped to run continuously, so
        // the plain average is the point between them even across the
        // antimeridian — see `tessellate`.
        corners.push((from + to) * 0.5);
        corners.len() as u32 - 1
    })
}

/// How far apart two corners are, in degrees, taking the larger of the two axes.
///
/// An overestimate near the poles, where a degree of longitude is much less
/// than a degree of arc. Overestimating only refines more than it has to.
fn span_degrees(from: DVec2, to: DVec2) -> f64 {
    (from - to).abs().max_element()
}

fn longest_edge_degrees(corners: &[DVec2], indices: &[u32]) -> f64 {
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
        .fold(0.0, f64::max)
}

/// How far out to push corners so that the flat surface between them sits at or
/// above the radius it was meant to, rather than dipping below it.
///
/// The same correction `TileGrid::radius` applies to a tile patch, for the same
/// reason: a polygon inscribed in a sphere is entirely inside it, and what is
/// drawn on the globe has to be entirely outside.
fn chord_lift(span_degrees: f64) -> f32 {
    if span_degrees <= 0.0 {
        return 1.0;
    }
    (1.0 / (span_degrees * 0.5).to_radians().cos()) as f32
}

/// One vertex of a densified path: which way it lies, and how far out it is
/// drawn. The radius is per point rather than per ribbon, which is what lets a
/// line climb along its length.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    direction: Vec3,
    radius: f32,
}

/// A ribbon along a path: two vertices per point, one either side of the spine,
/// stepped off in the shader so the width is in pixels rather than kilometres.
fn push_ribbon(builder: &mut MeshBuilder, path: &[Anchor]) {
    let count = path.len();
    if count < 2 {
        return;
    }
    // A closed path comes back to where it started, and its two ends have to be
    // given each other's neighbour or a corner would show at the seam.
    let closed = path[0].direction == path[count - 1].direction && count > 2;

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
        let mut along = next.direction - previous.direction;
        if along.length_squared() < 1.0e-12 {
            along = next.direction - point.direction;
        }
        let along = along.normalize_or_zero();

        for side in [-1.0_f32, 1.0] {
            builder.push(
                point.direction,
                point.radius,
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
/// leave the surface, and returns where each point of it is drawn.
///
/// Longitude runs continuously from one corner to the next, so a step from
/// 179° E to 179° W is the two degrees it looks like on a globe rather than the
/// 358 it looks like in a table of numbers. Height is carried along the same
/// way: a segment between two corners at different heights climbs evenly across
/// however many steps it was split into, so a line runs to where it was told to
/// rather than stepping up at each corner.
fn densify(path: Coords<'_>, closed: bool, altitude: OverlayAltitude, base: f32) -> Vec<Anchor> {
    if path.is_empty() {
        return Vec::new();
    }

    // `MAX_SEGMENT_DEGREES` bounds how far apart two points of a ribbon can be,
    // so the sag between them is bounded too, and lifting by it keeps a line
    // from dipping into the imagery at mid-segment.
    let lift = chord_lift(MAX_SEGMENT_DEGREES);
    let anchor = |position: Position| Anchor {
        direction: position.to_direction(),
        radius: radius_of(position, altitude, base, lift),
    };

    let count = path.len();
    let segments = if closed { count } else { count - 1 };
    let mut out = Vec::with_capacity(count);
    let first = path.get(0);
    out.push(anchor(first));

    let mut latitude = first.lat;
    let mut longitude = first.lon;
    let mut height = first.altitude_m;
    for index in 0..segments {
        let corner = path.wrapping(index + 1);
        let target_longitude = longitude + shortest_turn(corner.lon - longitude);
        let steps = ((corner.lat - latitude)
            .abs()
            .max((target_longitude - longitude).abs())
            / MAX_SEGMENT_DEGREES)
            .ceil()
            .max(1.0) as u32;

        for step in 1..=steps {
            let fraction = f64::from(step) / f64::from(steps);
            let stepped = anchor(Position::new(
                latitude + (corner.lat - latitude) * fraction,
                longitude + (target_longitude - longitude) * fraction,
                height + (corner.altitude_m - height) * fraction,
            ));
            // A repeated coordinate would leave a ribbon segment with no
            // direction to step off.
            if out
                .last()
                .is_none_or(|last| last.direction.distance_squared(stepped.direction) > 1.0e-14)
            {
                out.push(stepped);
            }
        }
        latitude = corner.lat;
        longitude = target_longitude;
        height = corner.altitude_m;
    }

    out
}

/// A ring's mean position, which is the one height its fill is drawn at.
///
/// The coordinate is the first corner's: nothing reads it, because a fill is
/// built from the triangulated corners and only the radius comes from here, but
/// averaging longitudes across the antimeridian would be wrong in a way that
/// would matter if anything ever did.
fn mean_height(ring: Coords<'_>) -> Position {
    if ring.is_empty() {
        return Position::new(0.0, 0.0, 0.0);
    }
    let first = ring.get(0);
    let total: f64 = ring.iter().map(|position| position.altitude_m).sum();
    Position::new(first.lat, first.lon, total / ring.len() as f64)
}

/// Brings an angle in degrees into `[-180, 180)`.
fn shortest_turn(degrees: f64) -> f64 {
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
    /// How many of those styled themselves, in the members of
    /// simplestyle-spec 1.1.0. Reported so an interface can say why a colour it
    /// chose did not reach the whole layer — and offer `simpleStyle: false`,
    /// which is what makes it.
    pub styled_features: usize,
    pub points: usize,
    pub lines: usize,
    pub polygons: usize,
    pub style: OverlayStyleInfo,
    /// Whether those members are being honoured.
    pub simple_style: bool,
    pub altitude: OverlayAltitudeInfo,
}

/// How a layer is reading the heights in its positions.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OverlayAltitudeInfo {
    /// `"relativeToSurface"` or `"clampToSurface"`.
    pub mode: &'static str,
    pub scale: f32,
    pub extrude: bool,
}

impl From<&OverlayAltitude> for OverlayAltitudeInfo {
    fn from(altitude: &OverlayAltitude) -> Self {
        Self {
            mode: altitude.mode.id(),
            scale: altitude.scale,
            extrude: altitude.extrude,
        }
    }
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

/// A feature the cursor found, with everything the document said about it.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PickedFeature {
    /// The id of the layer it belongs to, which is what a command naming it
    /// again — pinning it, for instance — has to be given.
    pub layer: String,
    pub label: String,
    /// Its position in the layer's feature list.
    pub index: usize,
    /// Its GeoJSON `id` member, if it had one.
    pub id: Option<String>,
    /// `"point"`, `"line"` or `"polygon"` — which of its shapes was hit.
    pub kind: &'static str,
    /// The `properties` object, exactly as the document wrote it. What a `mag`
    /// or a `place` means is the feed's business and the interface's.
    pub properties: serde_json::Value,
    /// The simplestyle members of those properties, parsed, for a feature that
    /// carried any.
    ///
    /// Duplicated out of `properties` on purpose: the globe has already had to
    /// read them to draw the feature, and three of them — `title`,
    /// `description` and `marker-symbol` — it cannot draw at all and can only
    /// hand on. An interface wanting a label for what the cursor is over should
    /// not have to re-implement [`crate::simplestyle`] to find one.
    pub style: Option<SimpleStyleInfo>,
}

/// Fills in a pick, or `None` when the layer or feature has since gone.
pub fn describe_pick(settings: &OverlaySettings, pick: Pick) -> Option<PickedFeature> {
    let overlay = settings
        .overlays
        .iter()
        .find(|overlay| overlay.slot == pick.slot)?;
    let document = overlay.data.as_ref()?;
    if pick.feature >= document.feature_count() {
        return None;
    }
    Some(PickedFeature {
        layer: overlay.id.clone(),
        label: overlay.label.clone(),
        index: pick.feature,
        id: document.feature_id(pick.feature).map(str::to_string),
        kind: pick.kind.id(),
        // Parsed here rather than held parsed — see `crate::features`. This
        // runs once when the pick changes, not once a frame.
        properties: document.feature_properties(pick.feature),
        style: document
            .feature_style(pick.feature)
            .filter(|style| !style.is_empty())
            .map(SimpleStyleInfo::from),
    })
}

/// What the cursor is over, if anything.
pub fn hovered(settings: &OverlaySettings) -> Option<PickedFeature> {
    settings
        .hovered
        .and_then(|pick| describe_pick(settings, pick))
}

/// What has been kept selected, if anything.
pub fn pinned(settings: &OverlaySettings) -> Option<PickedFeature> {
    settings
        .pinned
        .and_then(|pick| describe_pick(settings, pick))
}

/// The two picks as the state digest compares them: cheap, `Copy`, and without
/// the properties, which can be a page of JSON that never changes.
pub fn pick_digest(settings: &OverlaySettings) -> crate::api::PickDigest {
    let key = |pick: Option<Pick>| pick.map(|pick| (pick.slot, pick.feature));
    (key(settings.hovered), key(settings.pinned))
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
            styled_features: overlay.counts.styled,
            points: overlay.counts.points,
            lines: overlay.counts.lines,
            polygons: overlay.counts.polygons,
            style: OverlayStyleInfo::from(&overlay.style),
            simple_style: overlay.simple_style,
            altitude: OverlayAltitudeInfo::from(&overlay.altitude),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::FeatureSetBuilder;

    /// Planar area of a triangulation, in square degrees.
    fn area(corners: &[DVec2], indices: &[u32]) -> f64 {
        indices
            .chunks_exact(3)
            .map(|triangle| {
                let corner = |index: u32| corners[index as usize];
                let (a, b, c) = (
                    corner(triangle[0]),
                    corner(triangle[1]),
                    corner(triangle[2]),
                );
                (b - a).perp_dot(c - a) * 0.5
            })
            .sum()
    }

    /// A set holding one point, which is a layer of one marker.
    fn point_set(point: Position) -> FeatureSet {
        let mut builder = FeatureSetBuilder::new();
        let feature = builder.feature(None, None);
        builder.push_point(feature, point);
        builder.finish()
    }

    /// A set holding one line, which is also how a path is handed to
    /// `densify` — it takes a run of the store's coordinates, not a slice.
    fn line_set(path: &[Position]) -> FeatureSet {
        let mut builder = FeatureSetBuilder::new();
        let feature = builder.feature(None, None);
        builder.push_line(feature, path.iter().copied());
        builder.finish()
    }

    /// A set holding one polygon per ring given, all naming one feature.
    fn polygon_set(rings: &[Vec<Position>]) -> FeatureSet {
        let mut builder = FeatureSetBuilder::new();
        let feature = builder.feature(None, None);
        for ring in rings {
            builder.push_polygon(feature, [ring.iter().copied()]);
        }
        builder.finish()
    }

    fn big_polygon() -> FeatureSet {
        big_polygon_at(0.0)
    }

    /// The same polygon with every corner at one height.
    fn big_polygon_at(altitude_m: f64) -> FeatureSet {
        polygon_set(&[vec![
            Position::new(30.0, -10.0, altitude_m),
            Position::new(30.0, 20.0, altitude_m),
            Position::new(50.0, 20.0, altitude_m),
            Position::new(50.0, -10.0, altitude_m),
        ]])
    }

    #[test]
    fn refining_leaves_no_edge_long_enough_to_sag_off_the_globe() {
        let set = big_polygon();
        let (mut corners, indices) = tessellate::triangulate(set.polygon(0).1);
        assert!(longest_edge_degrees(&corners, &indices) > MAX_SEGMENT_DEGREES);

        let refined = refine(&mut corners, indices);
        assert!(longest_edge_degrees(&corners, &refined) <= MAX_SEGMENT_DEGREES);
    }

    #[test]
    fn refining_covers_exactly_the_same_ground() {
        let set = big_polygon();
        let (mut corners, indices) = tessellate::triangulate(set.polygon(0).1);
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
        let mesh = fill_mesh(
            &big_polygon(),
            None,
            OverlayAltitude::default(),
            FeaturePaint::Layer,
        )
        .expect("a fill");
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
    fn a_pixel_covers_less_ground_the_closer_the_camera_is() {
        let fov = 45.0_f32.to_radians();
        // Roughly a low pass at 200 km, and the default view from 14,000.
        let close = degrees_per_pixel(0.031, fov, 1080.0);
        let far = degrees_per_pixel(2.2, fov, 1080.0);
        assert!(close < far, "{close} vs {far}");

        // A nine-pixel marker from the default view is a target a few tenths of
        // a degree across — tens of kilometres, which is what it looks like.
        let tolerance = (9.0 * 0.5 + PICK_SLACK_PX) * far;
        assert!((0.1..2.0).contains(&tolerance), "{tolerance}");
    }

    #[test]
    fn a_taller_viewport_puts_more_pixels_across_the_same_ground() {
        let fov = 45.0_f32.to_radians();
        let short = degrees_per_pixel(1.0, fov, 540.0);
        let tall = degrees_per_pixel(1.0, fov, 1080.0);
        assert!((short - tall * 2.0).abs() < 1.0e-6, "{short} vs {tall}");
    }

    /// A position on the ground, which most of these are.
    fn at(lat: f64, lon: f64) -> Position {
        Position::new(lat, lon, 0.0)
    }

    /// Scene units per metre of height, which is what a height works out to
    /// once it is on the globe.
    fn units_per_metre() -> f32 {
        GLOBE_RADIUS / (EARTH_RADIUS_KM * 1000.0)
    }

    #[test]
    fn a_long_segment_is_subdivided_onto_the_surface() {
        let path = line_set(&[at(0.0, 0.0), at(0.0, 90.0)]);
        let densified = densify(
            path.line(0).1,
            false,
            OverlayAltitude::default(),
            LINE_RADIUS,
        );
        assert!(densified.len() > 45, "{}", densified.len());
        for point in &densified {
            assert!((point.direction.length() - 1.0).abs() < 1.0e-5);
        }
    }

    #[test]
    fn a_step_over_the_antimeridian_is_taken_the_short_way() {
        // Two degrees, at one step of at most two: the ends and nothing in
        // between, rather than the 179 steps a wrap the wrong way would need.
        let path = line_set(&[at(0.0, 179.0), at(0.0, -179.0)]);
        let densified = densify(
            path.line(0).1,
            false,
            OverlayAltitude::default(),
            LINE_RADIUS,
        );
        assert_eq!(densified.len(), 2);
    }

    #[test]
    fn a_ribbon_has_two_corners_per_point_and_two_triangles_per_segment() {
        let mut builder = MeshBuilder::new(false);
        let line = line_set(&[at(0.0, 0.0), at(1.0, 0.0)]);
        let path = densify(
            line.line(0).1,
            false,
            OverlayAltitude::default(),
            LINE_RADIUS,
        );
        push_ribbon(&mut builder, &path);
        assert_eq!(builder.positions.len(), path.len() * 2);
        assert_eq!(builder.indices.len(), (path.len() - 1) * 6);
    }

    #[test]
    fn a_closed_ring_meshes_without_a_seam() {
        let closed = line_set(&[at(0.0, 0.0), at(0.0, 1.0), at(1.0, 1.0)]);
        let ring = densify(
            closed.line(0).1,
            true,
            OverlayAltitude::default(),
            LINE_RADIUS,
        );
        assert_eq!(
            ring.first().unwrap().direction,
            ring.last().unwrap().direction
        );
        let mut builder = MeshBuilder::new(false);
        push_ribbon(&mut builder, &ring);
        assert_eq!(builder.indices.len(), (ring.len() - 1) * 6);
    }

    #[test]
    fn a_marker_is_drawn_at_the_height_it_was_given() {
        let points = point_set(Position::new(0.0, 0.0, 100_000.0));
        let radius = |altitude| {
            let mesh = marker_mesh(&points, None, altitude, FeaturePaint::Layer).expect("a marker");
            let positions = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|values| values.as_float3())
                .expect("positions");
            Vec3::from_array(positions[0]).length()
        };

        // A hundred kilometres above where a clamped marker would sit.
        let lifted = radius(OverlayAltitude::default());
        let expected = MARKER_RADIUS + 100_000.0 * units_per_metre();
        assert!((lifted - expected).abs() < 1.0e-6, "{lifted}");

        // Clamped, it is back on the surface with everything else.
        assert_eq!(radius(OverlayAltitude::CLAMPED), MARKER_RADIUS);
    }

    #[test]
    fn a_scale_says_what_the_third_element_was_in() {
        let points = point_set(Position::new(0.0, 0.0, 10.0));
        let radius = |scale| {
            let mesh = marker_mesh(
                &points,
                None,
                OverlayAltitude {
                    mode: AltitudeMode::RelativeToSurface,
                    scale,
                    ..OverlayAltitude::default()
                },
                FeaturePaint::Layer,
            )
            .expect("a marker");
            let positions = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|values| values.as_float3())
                .expect("positions");
            Vec3::from_array(positions[0]).length()
        };

        // Ten of something: as metres it is ten metres, as kilometres it is ten
        // kilometres, and the difference between those is the whole point of
        // the scale.
        assert!((radius(1.0) - (MARKER_RADIUS + 10.0 * units_per_metre())).abs() < 1.0e-7);
        let kilometres = radius(1000.0);
        assert!((kilometres - (MARKER_RADIUS + 10_000.0 * units_per_metre())).abs() < 1.0e-6);
        // A feed counting downward has nothing above the surface to draw.
        assert_eq!(radius(-1000.0), MARKER_RADIUS);
    }

    #[test]
    fn a_line_climbs_evenly_between_its_corners() {
        let climbing = line_set(&[
            Position::new(0.0, 0.0, 0.0),
            Position::new(0.0, 10.0, 200_000.0),
        ]);
        let path = densify(
            climbing.line(0).1,
            false,
            OverlayAltitude::default(),
            LINE_RADIUS,
        );
        assert!(path.len() > 4, "{}", path.len());

        // Monotonic from end to end, rather than a step at the far corner.
        for pair in path.windows(2) {
            assert!(pair[1].radius >= pair[0].radius);
        }
        let climb = path.last().unwrap().radius - path[0].radius;
        assert!(
            (climb - 200_000.0 * units_per_metre() * chord_lift(MAX_SEGMENT_DEGREES)).abs()
                < 1.0e-6,
            "{climb}"
        );
    }

    #[test]
    fn a_fill_is_drawn_at_the_mean_height_of_its_ring() {
        let radius = |altitude| {
            let mesh = fill_mesh(
                &big_polygon_at(50_000.0),
                None,
                altitude,
                FeaturePaint::Layer,
            )
            .expect("a fill");
            let positions = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|values| values.as_float3())
                .expect("positions");
            Vec3::from_array(positions[0]).length()
        };

        let lifted = radius(OverlayAltitude::default());
        assert!(
            lifted > FILL_RADIUS + 49_000.0 * units_per_metre(),
            "{lifted}"
        );
        // Still clear of the imagery once clamped, which is the floor every
        // fill is held to.
        assert!(radius(OverlayAltitude::CLAMPED) > MAX_TILE_RADIUS);
    }

    #[test]
    fn rings_at_different_heights_stack_into_shelves() {
        // What a stepped airspace is made of: rings that are each flat at one
        // height, stacked. Both halves have to hold for the stack to read as a
        // volume — the outlines have to separate in space, and each fill has to
        // sit at its own ring's height rather than at some average of all of
        // them.
        let shelf = |altitude_m: f64, span: f64| {
            vec![
                Position::new(-span, -span, altitude_m),
                Position::new(-span, span, altitude_m),
                Position::new(span, span, altitude_m),
                Position::new(span, -span, altitude_m),
            ]
        };
        let floors = [0.0, 2000.0, 3000.0, 4000.0];
        let rings: Vec<Vec<Position>> = floors
            .iter()
            .enumerate()
            .map(|(step, floor)| shelf(*floor, 0.2 + step as f64 * 0.2))
            .collect();
        let shelves = polygon_set(&rings);

        let radii = |mesh: Mesh| {
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|values| values.as_float3())
                .expect("positions")
                .iter()
                .map(|position| Vec3::from_array(*position).length())
                .fold((f32::MAX, f32::MIN), |(low, high), radius| {
                    (low.min(radius), high.max(radius))
                })
        };

        // The outlines span the whole stack, floor to ceiling.
        let (low, high) = radii(
            line_mesh(
                &shelves,
                None,
                true,
                OverlayAltitude::default(),
                FeaturePaint::Layer,
            )
            .expect("lines"),
        );
        let expected = (floors.last().unwrap() - floors[0]) as f32 * units_per_metre();
        assert!((high - low - expected).abs() < 1.0e-5, "{low} to {high}");

        // And every fill lands on its own shelf: four of them, none sharing a
        // height with another.
        let mut levels = Vec::new();
        for (index, floor) in floors.iter().enumerate() {
            let one = polygon_set(std::slice::from_ref(&rings[index]));
            let (low, high) = radii(
                fill_mesh(&one, None, OverlayAltitude::default(), FeaturePaint::Layer)
                    .expect("a fill"),
            );
            // One height across the whole lid, to within the precision of a
            // direction that was built from a sine and a cosine.
            assert!(high - low < 1.0e-6, "{low} to {high}");
            assert!(
                low > FILL_RADIUS + (*floor as f32 - 1.0) * units_per_metre(),
                "{low}"
            );
            levels.push(low);
        }
        for pair in levels.windows(2) {
            assert!(pair[1] > pair[0], "{levels:?}");
        }

        // Clamped, the stack is one flat drawing again.
        let (low, high) = radii(
            line_mesh(
                &shelves,
                None,
                true,
                OverlayAltitude::CLAMPED,
                FeaturePaint::Layer,
            )
            .expect("lines"),
        );
        assert!(high - low < 1.0e-6, "{low} to {high}");
    }

    #[test]
    fn extruding_walls_a_raised_ring_down_to_the_ground() {
        // A square at a height: a lid on its own, a box once it is extruded.
        let side = 1.0;
        let top = 200_000.0;
        let box_lid = polygon_set(&[vec![
            Position::new(-side, -side, top),
            Position::new(-side, side, top),
            Position::new(side, side, top),
            Position::new(side, -side, top),
        ]]);

        let spread = |altitude| {
            let mesh = fill_mesh(&box_lid, None, altitude, FeaturePaint::Layer).expect("a fill");
            let radii: Vec<f32> = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|values| values.as_float3())
                .expect("positions")
                .iter()
                .map(|position| Vec3::from_array(*position).length())
                .collect();
            let low = radii.iter().copied().fold(f32::MAX, f32::min);
            let high = radii.iter().copied().fold(f32::MIN, f32::max);
            (low, high, radii.len())
        };

        // Unextruded, every vertex is on the lid and nothing reaches down.
        let (low, high, flat_count) = spread(OverlayAltitude::default());
        assert!(high - low < 1.0e-6, "{low} to {high}");

        let extruded = OverlayAltitude {
            extrude: true,
            ..OverlayAltitude::default()
        };
        let (low, high, walled_count) = spread(extruded);
        // The walls span from the ground to the lid, and add vertices to do it.
        assert!(walled_count > flat_count, "{walled_count} vs {flat_count}");
        assert!(
            (high - low - top as f32 * units_per_metre()).abs() < 1.0e-5,
            "{low} to {high}"
        );
        assert!(low < FILL_RADIUS * 1.001, "{low} should be on the ground");

        // A ring already on the ground has nothing to wall: extruding it is the
        // same drawing as not.
        let on_the_ground = polygon_set(&[vec![
            at(-side, -side),
            at(-side, side),
            at(side, side),
            at(side, -side),
        ]]);
        let count = |altitude| {
            fill_mesh(&on_the_ground, None, altitude, FeaturePaint::Layer)
                .expect("a fill")
                .count_vertices()
        };
        assert_eq!(count(extruded), count(OverlayAltitude::default()));
    }

    #[test]
    fn extruding_outlines_the_footprint_and_the_corner_posts() {
        // The same box as above, seen by the half of the drawing that has
        // edges: without these, the only crisp thing on screen is the lid's
        // outline and the box reads as a square in the air.
        let side = 1.0;
        let top = 200_000.0;
        let box_lid = polygon_set(&[vec![
            Position::new(-side, -side, top),
            Position::new(-side, side, top),
            Position::new(side, side, top),
            Position::new(side, -side, top),
        ]]);

        let spread = |altitude| {
            let mesh =
                line_mesh(&box_lid, None, true, altitude, FeaturePaint::Layer).expect("an outline");
            let radii: Vec<f32> = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|values| values.as_float3())
                .expect("positions")
                .iter()
                .map(|position| Vec3::from_array(*position).length())
                .collect();
            let low = radii.iter().copied().fold(f32::MAX, f32::min);
            let high = radii.iter().copied().fold(f32::MIN, f32::max);
            (low, high, radii.len())
        };

        // Unextruded, the outline is the lid's ring and nothing else: every
        // vertex of it is at the one height.
        let (low, high, lid_only) = spread(OverlayAltitude::default());
        assert!(high - low < 1.0e-6, "{low} to {high}");

        let extruded = OverlayAltitude {
            extrude: true,
            ..OverlayAltitude::default()
        };
        let (low, high, walled) = spread(extruded);
        // The lid's ring, the footprint under it, and a post at each of the
        // four corners — every one of them vertices the lid alone did not have.
        assert!(walled > lid_only * 2, "{walled} vs {lid_only}");
        assert!(
            (high - low - top as f32 * units_per_metre()).abs() < 1.0e-5,
            "{low} to {high}"
        );
        assert!(low < LINE_RADIUS * 1.001, "{low} should be on the ground");

        // A post is drawn out from the globe rather than across it, so its two
        // ends lie in the same direction — which is exactly what a ribbon built
        // from the difference between neighbouring points cannot do. The
        // tangents are what carry that, so they have to be the direction
        // itself, not the zero a collapsed ribbon would leave.
        let mesh =
            line_mesh(&box_lid, None, true, extruded, FeaturePaint::Layer).expect("an outline");
        let bevy::mesh::VertexAttributeValues::Float32x4(tangents) =
            mesh.attribute(Mesh::ATTRIBUTE_TANGENT).expect("tangents")
        else {
            panic!("a tangent is four floats");
        };
        assert!(
            tangents
                .iter()
                .all(|tangent| Vec3::from_slice(tangent).length_squared() > 1.0e-12),
            "a ribbon with no direction is a ribbon with no width"
        );

        // A ring already on the ground stands on its own footprint, so
        // extruding it is the same drawing as not.
        let on_the_ground = polygon_set(&[vec![
            at(-side, -side),
            at(-side, side),
            at(side, side),
            at(side, -side),
        ]]);
        let count = |altitude| {
            line_mesh(&on_the_ground, None, true, altitude, FeaturePaint::Layer)
                .expect("an outline")
                .count_vertices()
        };
        assert_eq!(count(extruded), count(OverlayAltitude::default()));
    }

    #[test]
    fn a_height_setting_rebuilds_the_layer_without_refetching_it() {
        let mut settings = test_settings();
        settings.add(OverlayRequest {
            id: "track".into(),
            label: String::new(),
            source: OverlaySource::Text(
                r#"{"type": "Point", "coordinates": [1, 2, 5000]}"#.to_string(),
            ),
            style: OverlayStyle::default(),
            simple_style: true,
            altitude: OverlayAltitude::default(),
            refresh_seconds: None,
            visible: true,
        });

        assert!(settings.set_altitude("track", OverlayAltitude::CLAMPED));
        let overlay = &settings.overlays[0];
        assert!(overlay.wants_remesh);
        assert!(!overlay.wants_fetch);
        assert_eq!(overlay.generation, 0);

        // Setting it to what it already is moves nothing.
        settings.set_altitude("track", OverlayAltitude::CLAMPED);
        settings.overlays[0].wants_remesh = false;
        settings.set_altitude("track", OverlayAltitude::CLAMPED);
        assert!(!settings.overlays[0].wants_remesh);
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
            simple_style: true,
            altitude: OverlayAltitude::default(),
            refresh_seconds: Some(30.0),
            visible: true,
        });
        let overlay = &settings.overlays[0];
        // The label falls back to the id, and the geometry is already waiting.
        assert_eq!(overlay.label, "local");
        assert_eq!(
            overlay.pending.as_ref().map(FeatureSet::point_count),
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
            simple_style: true,
            altitude: OverlayAltitude::default(),
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
            simple_style: true,
            altitude: OverlayAltitude::default(),
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

    // -----------------------------------------------------------------------
    // What a document says about its own appearance
    // -----------------------------------------------------------------------

    /// Two rings, of which only the first says how it wants to look.
    fn half_styled() -> FeatureSet {
        crate::geojson::parse(
            r##"{"type": "FeatureCollection", "features": [
                {"type": "Feature",
                 "properties": {"fill": "#ff0000", "fill-opacity": 1.0, "stroke-width": 8},
                 "geometry": {"type": "Polygon",
                              "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]]}},
                {"type": "Feature", "properties": {"name": "plain"},
                 "geometry": {"type": "Polygon",
                              "coordinates": [[[5, 5], [6, 5], [6, 6], [5, 6], [5, 5]]]}}
            ]}"##,
        )
        .expect("valid")
    }

    /// Every vertex colour of a mesh, or `None` where the mesh carries none.
    fn vertex_colors(mesh: &Mesh) -> Option<Vec<[f32; 4]>> {
        match mesh.attribute(Mesh::ATTRIBUTE_COLOR)? {
            bevy::mesh::VertexAttributeValues::Float32x4(values) => Some(values.clone()),
            _ => None,
        }
    }

    #[test]
    fn a_layer_whose_document_says_nothing_carries_no_paint() {
        // The whole point of the sentinel being opt-in: a feed that styles
        // nothing draws through the same pipeline it always did, with vertices
        // the size they always were.
        let plain = crate::geojson::parse(
            r#"{"type": "Polygon", "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 0]]]}"#,
        )
        .expect("valid");
        let style = OverlayStyle::default();
        let mesh = fill_mesh(
            &plain,
            None,
            OverlayAltitude::default(),
            FeaturePaint::Over(style.paint(VectorMode::Fill)),
        )
        .expect("a fill");
        assert!(vertex_colors(&mesh).is_none());
        assert!(mesh.attribute(Mesh::ATTRIBUTE_UV_1).is_none());
    }

    #[test]
    fn a_feature_that_styles_itself_carries_its_colour_and_its_neighbour_does_not() {
        let document = half_styled();
        let style = OverlayStyle::default();
        let mesh = fill_mesh(
            &document,
            None,
            OverlayAltitude::default(),
            FeaturePaint::Over(style.paint(VectorMode::Fill)),
        )
        .expect("a fill");

        let colors = vertex_colors(&mesh).expect("colours");
        let red = bevy::color::LinearRgba::from(Srgba::new(1.0, 0.0, 0.0, 1.0));
        assert!(
            colors.contains(&[red.red, red.green, red.blue, red.alpha]),
            "the styled ring should carry the colour it asked for: {colors:?}"
        );
        // And the ring that asked for nothing carries the sentinel, so a
        // restyle of the layer still reaches it.
        assert!(
            colors.contains(&INHERIT_COLOR),
            "the unstyled ring should defer to the material: {colors:?}"
        );
    }

    #[test]
    fn a_layer_told_to_ignore_the_document_carries_no_paint_either() {
        let document = half_styled();
        let mesh = fill_mesh(
            &document,
            None,
            OverlayAltitude::default(),
            FeaturePaint::Layer,
        )
        .expect("a fill");
        assert!(vertex_colors(&mesh).is_none());
    }

    #[test]
    fn a_stroke_width_reaches_the_mesh_as_a_size() {
        let document = half_styled();
        let style = OverlayStyle::default();
        let mesh = line_mesh(
            &document,
            None,
            true,
            OverlayAltitude::default(),
            FeaturePaint::Over(style.paint(VectorMode::Line)),
        )
        .expect("outlines");
        let sizes = match mesh.attribute(Mesh::ATTRIBUTE_UV_1).expect("sizes") {
            bevy::mesh::VertexAttributeValues::Float32x2(values) => values.clone(),
            other => panic!("unexpected sizes: {other:?}"),
        };
        assert!(sizes.iter().any(|size| size[0] == 8.0), "{sizes:?}");
        assert!(sizes.contains(&INHERIT_SIZE), "{sizes:?}");
    }

    /// One document holding a feature of every kind the globe draws, run end to
    /// end: parsed into the store, and all three meshes built out of it.
    ///
    /// Written out here rather than read from the reference app's sample file,
    /// because the globe knows nothing about the app around it and a test is
    /// not the place to start.
    #[test]
    fn a_document_of_every_kind_parses_and_meshes() {
        let document = crate::geojson::parse(
            r#"{
                "type": "FeatureCollection",
                "features": [
                    {"type": "Feature", "id": 1, "properties": {"name": "a point"},
                     "geometry": {"type": "Point", "coordinates": [10, 20, 1500]}},
                    {"type": "Feature", "properties": {"name": "a track"},
                     "geometry": {"type": "LineString",
                                  "coordinates": [[0, 0], [10, 5, 2000], [20, 10, 4000]]}},
                    {"type": "Feature", "properties": {"name": "an island chain"},
                     "geometry": {"type": "MultiPolygon", "coordinates": [
                        [[[30, 30], [34, 30], [34, 34], [30, 34], [30, 30]],
                         [[31, 31], [32, 31], [32, 32], [31, 32], [31, 31]]],
                        [[[40, 30], [44, 30], [44, 34], [40, 34], [40, 30]]]
                     ]}},
                    {"type": "Feature", "properties": {"name": "an airspace shelf"},
                     "geometry": {"type": "Polygon", "coordinates":
                        [[[-5, -5, 3000], [-3, -5, 3000], [-3, -3, 3000], [-5, -5, 3000]]]}},
                    {"type": "Feature", "properties": null,
                     "geometry": {"type": "GeometryCollection", "geometries": [
                        {"type": "MultiPoint", "coordinates": [[60, 10], [61, 11]]}
                     ]}}
                ]
            }"#,
        )
        .expect("valid");

        assert_eq!(document.feature_count(), 5);
        assert_eq!(document.point_count(), 3);
        assert_eq!(document.line_count(), 1);
        // The `MultiPolygon` is two polygons of one feature, plus the shelf.
        assert_eq!(document.polygon_count(), 3);

        let altitude = OverlayAltitude::default();
        assert!(marker_mesh(&document, None, altitude, FeaturePaint::Layer).is_some());
        assert!(line_mesh(&document, None, true, altitude, FeaturePaint::Layer).is_some());
        assert!(fill_mesh(&document, None, altitude, FeaturePaint::Layer).is_some());

        // Every shape names a feature that exists, which is what the hit test
        // and the highlight both depend on.
        let features = document.feature_count() as u32;
        for owners in [
            document.point_owners(),
            document.line_owners(),
            document.polygon_owners(),
        ] {
            assert!(owners.iter().all(|owner| *owner < features));
        }

        // Both islands belong to the one feature, so highlighting either is
        // highlighting the chain — and that is fewer vertices than the layer.
        let chain = document.polygon_owners()[0];
        assert_eq!(document.polygon_owners()[1], chain);
        let whole = fill_mesh(&document, None, altitude, FeaturePaint::Layer).expect("a fill");
        let one = fill_mesh(&document, Some(chain), altitude, FeaturePaint::Layer)
            .expect("one feature's fill");
        assert!(one.count_vertices() < whole.count_vertices());
    }

    fn test_settings() -> OverlaySettings {
        OverlaySettings {
            enabled: true,
            picking: true,
            overlays: Vec::new(),
            hovered: None,
            pinned: None,
            highlighted: None,
            highlight: Vec::new(),
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
            simple_style: true,
            altitude: OverlayAltitude::default(),
            refresh_seconds: None,
            age_seconds: 0.0,
            visible: true,
            status: OverlayStatus::Loading,
            counts: OverlayCounts::default(),
            handle: None,
            data: None,
            index: Arc::default(),
            pending: None,
            wants_fetch: false,
            wants_restyle: false,
            wants_remesh: false,
            parts: Vec::new(),
        }
    }
}
