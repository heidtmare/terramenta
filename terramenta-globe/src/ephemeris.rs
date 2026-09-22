//! Satellite ephemeris: current, recent, and upcoming positions.
//!
//! An ephemeris layer is one OMM catalogue — from a URL, or handed over as
//! text — propagated with SGP4 against the globe's own simulated clock and
//! drawn as a marker per object with a leading and trailing arc. Several
//! layers can be up at once, each with its own colours, selection and trail
//! window, the way [`crate::overlays`] holds several GeoJSON layers; the two
//! share the fetch plumbing in [`crate::fetch`], the GeoArrow store in
//! [`crate::features`], and the mesh builders and shader that turn a
//! coordinate into a screen-sized marker or line.
//!
//! **Positions are computed, not parsed.** [`crate::omm`] turns each record
//! into a propagator; nothing has a coordinate until this module evaluates
//! one, against [`crate::sun::Sun::unix_seconds`] — the simulated clock, not
//! the wall clock. Satellites follow the clock controls like everything else:
//! pausing, time-scaling, and jumping all apply.
//!
//! **Geometry is rebuilt every frame, not loaded once.** Other layers parse a
//! document once and redraw it until refetched; this one propagates every
//! drawn object every frame, since a satellite in low Earth orbit crosses the
//! screen in seconds at the default rate. [`MAX_TRACKED`] bounds the number of
//! markers, and [`MAX_TRAIL_POINTS`] bounds trail samples by dividing one
//! budget among however many satellites are trailed — one trailed satellite
//! gets a smooth arc, sixty get coarser ones, and the per-frame cost is the
//! same either way.
//!
//! **Frame conversion happens per sample, not as a mesh transform.** SGP4
//! works in TEME (inertial); the globe draws in whichever frame
//! [`crate::frame::ReferenceFrame`] says world space is. A marker can be
//! carried across frames by a transform, as overlays are, but an arc cannot:
//! each point belongs to a different moment, and in ECEF the Earth has turned
//! underneath between them — an orbit is a closed ellipse in ECI and a
//! corkscrew in ECEF, which is not a rigid rotation of one mesh. Every sample
//! is converted to the active frame at its own moment, so the mesh is built
//! already in world space and the entities carry no rotation; switching
//! frames rebuilds them.
//!
//! [`TrailPath`] chooses which moment each sample is drawn at. Turning each
//! sample by its own moment draws the ground track — the figure-eight a
//! navigation constellation traces, or the stationary dot a geostationary one
//! is. Turning every sample by the current moment instead draws the orbit
//! itself, frozen into Earth-fixed space as it stands now; this curve is the
//! same in both frames, so it does not move when frames switch, matching what
//! an orbit (as opposed to a track) should do. The difference between the two
//! modes, sample by sample, equals how far the Earth turned between that
//! sample's moment and now: zero at the satellite's current position, growing
//! to a quarter turn at each end of a full-orbit window for a navigation
//! satellite. In ECI the two modes are identical.
//!
//! **Coordinates are geocentric, not geodetic.** A position reduces to a
//! declination, a right ascension and a radius; the radius becomes a height
//! above a sphere of [`EARTH_RADIUS_KM`], not the WGS 84 ellipsoid — matching
//! the sphere the globe actually draws, so a satellite sits over the imagery
//! it is really over. Geocentric and geodetic latitude differ by up to about a
//! fifth of a degree, and height differs by roughly twenty kilometres at the
//! poles.
//!
//! **Picking uses screen pixels, not ground position.** [`crate::picking`]
//! normally hit-tests the ground point a layer is drawn at; a satellite marker
//! and its sub-point coincide only when the camera looks straight down.
//! Instead, a satellite is picked at its projected screen anchor, measured
//! against the pointer in pixels — the same unit the marker's size is stated
//! in. The Earth occludes: an object on the far side of an opaque globe cannot
//! be picked. Only markers are picked, not arcs, since an arc represents where
//! a satellite has been rather than the satellite itself.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, RwLock};

use bevy::asset::io::Reader;
use bevy::asset::{AssetApp, AssetLoader, LoadContext, LoadState};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::prelude::*;
use serde::Serialize;

use crate::api::Cursor;
use crate::features::{FeatureSet, FeatureSetBuilder};
use crate::fetch::{self, SharedUrls};
use crate::frame::{FrameMode, FrameSet, ReferenceFrame};
use crate::geo::{EARTH_RADIUS_KM, Position};
use crate::omm::{self, Catalogue};
use crate::overlays::{
    AltitudeMode, FeaturePaint, HIGHLIGHT_COLOR, HIGHLIGHT_GROW_PX, OverlayAltitude, OverlaySource,
    OverlayStyle, OverlayStyleInfo, PICK_SLACK_PX, VectorMaterial, VectorMode,
};
use crate::sun::Sun;
use crate::tiles::MAX_TILE_RADIUS;

/// The asset source OMM catalogues are fetched over.
pub const EPHEMERIS_SOURCE: &str = "omm";

/// The extension an ephemeris document is fetched under, which is what picks
/// the loader.
const DOCUMENT_EXTENSION: &str = "omm";

/// The most objects one layer propagates and draws a marker for.
///
/// A marker costs one SGP4 evaluation per frame, and SGP4 is a couple of
/// microseconds — so this is the number that decides whether a full catalogue
/// is a globe with satellites on it or a slideshow. Six hundred is a comfortable
/// millisecond or two a frame in the browser, and it is far more objects than
/// can be told apart on screen anyway. A selection larger than this is drawn
/// down to it in catalogue order, and the layer reports both numbers so an
/// interface can say so rather than quietly losing the rest.
pub const MAX_TRACKED: usize = 600;

/// The most objects one layer draws a trail for.
pub const MAX_TRAILED: usize = 64;

/// The total trail samples one layer may spend, divided among whatever is
/// trailed.
///
/// A budget rather than a per-satellite count, because the per-satellite count
/// is the wrong thing to hold fixed: one satellite with a smooth five-hundred
/// point orbit and sixty with a coarse sixty-point one cost the same, and both
/// are what you want at those two extremes. So the requested sample count is a
/// ceiling, the budget is the floor under it, and a layer quietly gets coarser
/// as more of it is switched on instead of getting slower.
const MAX_TRAIL_POINTS: usize = 4096;

/// The fewest samples a trail is drawn with before it is not worth drawing.
/// Below about this an orbit reads as a polygon rather than as an arc.
const MIN_TRAIL_SAMPLES: u32 = 16;

/// The most samples one trail may be asked for, however the budget falls.
const MAX_TRAIL_SAMPLES: u32 = 512;

/// How much of an orbit a trail may span, either way. One whole revolution
/// behind and one ahead is the most that means anything: past it the arc is
/// drawn over itself.
const MAX_TRAIL_ORBITS: f32 = 1.0;

/// How often trails are rebuilt, in seconds of real time.
///
/// Markers are rebuilt every frame because a satellite visibly moves every
/// frame; an arc through where it has been does not, so it is rebuilt when the
/// clock has moved a worthwhile fraction of the window it spans — at most ten
/// times a second, and at least twice, so that a paused clock still catches a
/// selection change and a clock running at a day a second still keeps up.
const MIN_TRAIL_INTERVAL_SECONDS: f32 = 0.1;
const MAX_TRAIL_INTERVAL_SECONDS: f32 = 0.5;

/// The fraction of its own window a trail may go stale by before it is redrawn.
const TRAIL_STALE_FRACTION: f64 = 1.0 / 240.0;

/// A refresh period is clamped to this at the fast end.
///
/// Five minutes, where an overlay's floor is one second, and the difference is
/// what the data is rather than what the globe can stand. A catalogue is
/// regenerated a few times a day from observations that are themselves hours
/// old; asking every minute would fetch the same document sixty times over, and
/// the public endpoints that serve it ask politely that you do not.
pub const MIN_REFRESH_SECONDS: f32 = 300.0;

/// What an ephemeris does with the heights it computes.
///
/// Never clamped and never extruded: the whole point of a satellite is that it
/// is not on the ground, and a scale of one is right because the heights are
/// this module's own metres rather than a feed's opinion of them.
const ALTITUDE: OverlayAltitude = OverlayAltitude {
    mode: AltitudeMode::RelativeToSurface,
    scale: 1.0,
    extrude: false,
};

// ---------------------------------------------------------------------------
// What a layer is
// ---------------------------------------------------------------------------

/// What an arc is a picture of.
///
/// Only ECEF can tell the two apart. In ECI world space is already inertial, so
/// there is no rotation to apply and both draw the same orbit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrailPath {
    /// Where the satellite passed over the ground: each sample placed at the
    /// longitude it was over *at its own moment*. A corkscrew in ECEF, because
    /// the planet turned underneath between one sample and the next.
    #[default]
    GroundTrack,
    /// The orbit itself, as it stands at this moment: every sample placed
    /// against the *current* rotation, so the arc is the same curve whichever
    /// frame is drawn and switching between them does not move it. It drifts
    /// westward over the ground as the clock runs, which is what an inertial
    /// ellipse does when the Earth turns under it.
    Orbit,
}

impl TrailPath {
    pub fn id(self) -> &'static str {
        match self {
            Self::GroundTrack => "track",
            Self::Orbit => "orbit",
        }
    }

    /// Parses [`TrailPath::id`] back, for a path named by an embedder.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "track" => Some(Self::GroundTrack),
            "orbit" => Some(Self::Orbit),
            _ => None,
        }
    }
}

/// How long a trail is, how finely it is drawn, and what it is a picture of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrailWindow {
    /// How far ahead the leading arc runs, in orbits.
    pub leading_orbits: f32,
    /// How far back the trailing arc runs, in orbits.
    pub trailing_orbits: f32,
    /// Samples per whole orbit, before the budget is applied.
    ///
    /// In orbits rather than in minutes because an orbit is the natural unit of
    /// the thing being drawn: half an orbit is half an orbit for the station at
    /// ninety minutes and for a navigation satellite at twelve hours, where
    /// "forty minutes" is most of one and a twentieth of the other.
    pub samples: u32,
    /// A ground track or an orbit. See [`TrailPath`].
    pub path: TrailPath,
}

impl Default for TrailWindow {
    fn default() -> Self {
        // Half an orbit either way, which is one whole revolution centred on
        // where the satellite is now — a closed ellipse in ECI, and one turn of
        // the corkscrew in ECEF.
        Self {
            leading_orbits: 0.5,
            trailing_orbits: 0.5,
            samples: 128,
            // A track rather than an orbit, because it is the one of the two
            // that ECEF alone can show: the orbit is what ECI already draws.
            path: TrailPath::GroundTrack,
        }
    }
}

impl TrailWindow {
    /// The same window with everything brought inside the ranges above.
    fn sanitized(self) -> Self {
        let orbits = |value: f32| {
            if value.is_finite() {
                value.clamp(0.0, MAX_TRAIL_ORBITS)
            } else {
                0.0
            }
        };
        Self {
            leading_orbits: orbits(self.leading_orbits),
            trailing_orbits: orbits(self.trailing_orbits),
            samples: self.samples.clamp(MIN_TRAIL_SAMPLES, MAX_TRAIL_SAMPLES),
            path: self.path,
        }
    }

    fn span_orbits(self) -> f32 {
        self.leading_orbits + self.trailing_orbits
    }
}

/// Which objects of a catalogue a layer is asked for.
///
/// Stated by catalogue number, which is the only identifier that is both stable
/// and unique — names repeat and an international designator names a launch
/// rather than a piece of one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Selection {
    /// Everything the document held, down to [`MAX_TRACKED`].
    #[default]
    All,
    /// Only these, and nothing for a number the document did not hold.
    Only(BTreeSet<u64>),
}

impl Selection {
    /// Builds a selection from whatever an embedder listed.
    pub fn only(ids: impl IntoIterator<Item = u64>) -> Self {
        Self::Only(ids.into_iter().collect())
    }

    fn wants(&self, norad_id: u64) -> bool {
        match self {
            Self::All => true,
            Self::Only(ids) => ids.contains(&norad_id),
        }
    }
}

/// Everything needed to put one ephemeris layer up, as an embedder states it.
#[derive(Debug, Clone, PartialEq)]
pub struct EphemerisRequest {
    /// The embedder's name for the layer. Adding a second under an id already
    /// in use replaces the first.
    pub id: String,
    /// What to call it in an interface. The id, if this is empty.
    pub label: String,
    /// Where the OMM JSON comes from. The same two kinds an overlay has, and
    /// for the same reasons — see [`crate::overlays`].
    pub source: OverlaySource,
    /// Marker and line colours and sizes. The fill is unused: an ephemeris has
    /// no rings in it.
    pub style: OverlayStyle,
    pub trail: TrailWindow,
    /// Whether trails are drawn at all for this layer.
    pub trails: bool,
    /// Which objects to draw, and which of those to trail. Both default to
    /// everything the document holds, down to the budgets.
    pub selection: Selection,
    /// Seconds between refetches, or `None` to fetch once.
    pub refresh_seconds: Option<f32>,
    pub visible: bool,
}

impl EphemerisRequest {
    /// A layer from a URL, with everything else defaulted.
    pub fn from_url(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: String::new(),
            source: OverlaySource::Url(url.into()),
            style: default_style(),
            trail: TrailWindow::default(),
            trails: true,
            selection: Selection::All,
            refresh_seconds: None,
            visible: true,
        }
    }
}

/// What a satellite layer is drawn in when nothing says otherwise.
///
/// Pale cyan rather than the overlays' amber, and deliberately: an ephemeris is
/// the one layer that is never on the ground, so it wants a colour that reads
/// against the *sky* and against the night side as much as against imagery.
/// Markers a little smaller than an overlay's and lines a little thinner,
/// because a constellation is many of both at once.
pub fn default_style() -> OverlayStyle {
    OverlayStyle {
        point_color: bevy::color::Srgba::new(0.55, 0.88, 1.0, 1.0),
        point_size_px: 7.0,
        line_color: bevy::color::Srgba::new(0.55, 0.88, 1.0, 0.65),
        line_width_px: 1.4,
        fill_color: bevy::color::Srgba::NONE,
    }
}

/// How a layer's data is doing. The same three states an overlay has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EphemerisStatus {
    Loading,
    Ready,
    /// Unreachable, not a catalogue, or refused by the browser for want of CORS.
    Failed(String),
}

impl EphemerisStatus {
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

/// One object of one layer, named the way the globe can still find it after a
/// refresh and after a layer has been taken down.
///
/// By slot rather than by layer id, because a slot is never reused: a pin held
/// across a layer being replaced under the same name cannot come to mean the
/// new one. And by catalogue number rather than by position in the document,
/// because a refetched catalogue is renumbered — the same satellite is a
/// different row, and every other control here names objects the same way. A
/// pin therefore survives a refresh, and is quietly nothing while the object is
/// missing from whatever the layer holds now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SatellitePick {
    slot: u64,
    norad_id: u64,
}

/// One drawn piece of a layer: its markers, its trails, or the halo around
/// whatever is picked.
#[derive(Default)]
struct Drawn {
    entity: Option<Entity>,
    mesh: Option<Handle<Mesh>>,
    material: Option<Handle<VectorMaterial>>,
    /// Whether the last build had anything in it.
    drawing: bool,
}

/// One live ephemeris layer.
struct Ephemeris {
    id: String,
    label: String,
    source: OverlaySource,
    /// Identifies this layer to the asset reader, which lives outside the
    /// `World`. Never reused.
    slot: u64,
    /// Bumped by every refresh, and part of the asset path, so a refetch is
    /// never answered out of the cache of the one before it.
    generation: u32,
    style: OverlayStyle,
    trail: TrailWindow,
    trails: bool,
    visible: bool,
    refresh_seconds: Option<f32>,
    age_seconds: f32,
    status: EphemerisStatus,
    handle: Option<Handle<OmmAsset>>,
    /// The propagators, once a document has landed.
    catalogue: Option<Arc<Catalogue>>,
    /// What an embedder asked for, kept so that a refresh re-applies it to the
    /// new document rather than resetting the layer to everything.
    requested: Selection,
    /// Whether each object of the catalogue is drawn, and whether it is
    /// trailed. Parallel to `catalogue.satellites`, because that is what makes
    /// a toggle an index rather than a search.
    selected: Vec<bool>,
    trailed: Vec<bool>,
    /// A document waiting to be adopted, from a text source or a finished fetch.
    pending: Option<Catalogue>,
    wants_fetch: bool,
    wants_restyle: bool,
    /// Set when something other than the clock moved the trails.
    wants_trails: bool,
    /// Seconds of real time since the trails were last built, and the simulated
    /// moment and frame they were built for.
    since_trails: f32,
    trail_clock: f64,
    trail_mode: FrameMode,
    markers: Drawn,
    arcs: Drawn,
    highlight: Drawn,
    /// Where every drawn object was this frame, as one point per object — the
    /// very set the markers were built from.
    ///
    /// Kept because three things want it and propagating it three times would
    /// be three answers: the markers are built from it, the hit test measures
    /// against it, and the halo is drawn on it. Behind an `Arc` so the pull-side
    /// binding shares these buffers rather than a copy of them.
    positions: Arc<FeatureSet>,
    /// Which object of the catalogue each of those points is, in the same
    /// order. A point is not a satellite until something says which one, and
    /// the set holds only the ones that could be propagated.
    drawn: Vec<usize>,
    /// The reference angle those positions were placed with, so a coordinate
    /// drawn in an inertial frame can still be reported as a place on Earth.
    drawn_angle: f64,
    /// Bumped by anything an interface has a control for, so the state stream
    /// publishes the moment one changes — and so an interface knows when the
    /// object list it pulled has gone stale.
    revision: u64,
}

impl Ephemeris {
    fn asset_path(&self) -> String {
        fetch::asset_path(
            EPHEMERIS_SOURCE,
            self.slot,
            self.generation,
            DOCUMENT_EXTENSION,
        )
    }

    fn set_refresh(&mut self, seconds: Option<f32>) {
        self.refresh_seconds = seconds
            .filter(|_| matches!(self.source, OverlaySource::Url(_)))
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .map(|seconds| seconds.max(MIN_REFRESH_SECONDS));
    }

    fn set_status(&mut self, status: EphemerisStatus) -> bool {
        let changed = self.status != status;
        self.status = status;
        changed
    }

    /// Applies the requested selection to whatever catalogue is loaded.
    ///
    /// The budgets bite here rather than at draw time, so that what an interface
    /// is told is selected is what is actually drawn. Catalogue order decides
    /// who makes the cut, which for every published group is the order the
    /// provider put them in.
    fn resolve_selection(&mut self) {
        let Some(catalogue) = self.catalogue.as_ref() else {
            self.selected.clear();
            self.trailed.clear();
            return;
        };

        self.selected = Vec::with_capacity(catalogue.len());
        self.trailed = Vec::with_capacity(catalogue.len());
        let (mut tracked, mut trailed) = (0, 0);
        for satellite in &catalogue.satellites {
            let wanted = self.requested.wants(satellite.norad_id) && tracked < MAX_TRACKED;
            tracked += usize::from(wanted);
            let trail = wanted && trailed < MAX_TRAILED;
            trailed += usize::from(trail);
            self.selected.push(wanted);
            self.trailed.push(trail);
        }
    }

    /// The objects that are drawn, as indices into the catalogue.
    fn tracked(&self) -> impl Iterator<Item = usize> + '_ {
        self.selected
            .iter()
            .enumerate()
            .filter_map(|(index, on)| on.then_some(index))
    }

    /// The objects that are trailed — which is a subset of the tracked ones,
    /// because an arc through a satellite that is not drawn is a loose thread.
    fn trailing(&self) -> impl Iterator<Item = usize> + '_ {
        self.selected
            .iter()
            .zip(&self.trailed)
            .enumerate()
            .filter_map(|(index, (drawn, trailed))| (*drawn && *trailed).then_some(index))
    }

    fn counts(&self) -> (usize, usize) {
        (self.tracked().count(), self.trailing().count())
    }

    /// Forgets where everything was, for a layer that has stopped being
    /// propagated. What a pick says then is what is true: the object is in the
    /// catalogue and nowhere on screen.
    fn clear_drawn(&mut self) {
        self.positions = Arc::default();
        self.drawn.clear();
    }

    /// Where one object was drawn this frame, by catalogue number, or `None`
    /// when it is not drawn at all — unselected, hidden, or past a
    /// divergence.
    fn drawn_position(&self, norad_id: u64) -> Option<Position> {
        let catalogue = self.catalogue.as_ref()?;
        let point = self
            .drawn
            .iter()
            .position(|index| catalogue.satellites[*index].norad_id == norad_id)?;
        Some(self.positions.point(point).1)
    }

    /// The three materials a layer draws with. Built in one place because they
    /// are made twice — once when a part is first spawned, and again whenever
    /// the style changes — and two spellings of the same material would drift.
    fn marker_material(&self) -> VectorMaterial {
        VectorMaterial::new(VectorMode::Marker, self.style.paint(VectorMode::Marker)).above()
    }

    fn arc_material(&self) -> VectorMaterial {
        VectorMaterial::new(VectorMode::Line, self.style.paint(VectorMode::Line)).above()
    }

    /// Larger than the marker and drawn behind it, so what is picked keeps its
    /// own colour and gains a halo — the same treatment an overlay's pick gets,
    /// in the same near-white, so that one globe has one way of saying "this
    /// one".
    fn highlight_material(&self) -> VectorMaterial {
        VectorMaterial::new(
            VectorMode::Marker,
            crate::overlays::Paint {
                color: HIGHLIGHT_COLOR,
                size_px: self.style.point_size_px + HIGHLIGHT_GROW_PX,
            },
        )
        .above()
        .behind()
    }
}

// ---------------------------------------------------------------------------
// The resource an embedder drives
// ---------------------------------------------------------------------------

/// One layer's catalogue and selection, reachable from outside the `World`.
///
/// This exists for [`crate::wasm::ephemeris_objects`], which hands an embedder
/// the list of what a layer holds and has no `World` to ask — the same reason
/// [`crate::overlays`] publishes its geometry. The object list is pulled rather
/// than streamed because a catalogue can be twelve thousand rows and the state
/// snapshot goes out ten times a second; what the stream carries is the
/// revision that says the pull is stale.
///
/// Nothing but that binding asks, so on a native build this and everything that
/// serves it are dead code in the literal sense and live code in every other
/// one — a native host holding the `App` reads the resource directly.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub struct PublishedLayer {
    pub catalogue: Arc<Catalogue>,
    pub selected: Vec<bool>,
    pub trailed: Vec<bool>,
}

/// One layer's drawn geometry, in the GeoArrow arrays every other layer uses.
///
/// Two sets rather than one, because they are rebuilt on different clocks:
/// `positions` is where every tracked object is *now* and is rebuilt every
/// frame; `trails` is the arcs and is rebuilt when they go stale.
///
/// As [`PublishedLayer`]: only the WebAssembly binding reads it back out.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Default)]
pub struct EphemerisGeometry {
    pub positions: Arc<FeatureSet>,
    pub trails: Arc<FeatureSet>,
}

static CATALOGUES: LazyLock<RwLock<HashMap<String, Arc<PublishedLayer>>>> =
    LazyLock::new(RwLock::default);

static GEOMETRY: LazyLock<RwLock<HashMap<String, Arc<EphemerisGeometry>>>> =
    LazyLock::new(RwLock::default);

/// What one layer holds, or `None` for a layer that is not up or has not loaded.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn catalogue_of(id: &str) -> Option<Arc<PublishedLayer>> {
    CATALOGUES.read().ok()?.get(id).cloned()
}

/// One layer's geometry as it is currently drawn.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn geometry_of(id: &str) -> Option<Arc<EphemerisGeometry>> {
    GEOMETRY.read().ok()?.get(id).cloned()
}

/// Every ephemeris layer, and the master switch over all of them.
#[derive(Resource)]
pub struct EphemerisSettings {
    /// Whether ephemerides are drawn at all. Off, every layer stays loaded and
    /// simply stops being propagated, which is also where the cost goes.
    pub enabled: bool,
    /// Whether the cursor picks satellites at all. Off, nothing is hovered and
    /// no halo is drawn; a pin already set stays set. The same switch the
    /// overlays have, and set by the same command — "the cursor picks things"
    /// is one thing to a person using the globe, not two.
    pub picking: bool,
    /// What the cursor is over now.
    hovered: Option<SatellitePick>,
    /// What an embedder asked to keep, whatever the cursor does afterwards.
    /// This is what a click becomes: the globe reports what is under the
    /// pointer, and the interface decides that one of those is the selection.
    pinned: Option<SatellitePick>,
    layers: Vec<Ephemeris>,
    urls: SharedUrls,
    next_slot: u64,
    revision: u64,
    /// Entities of removed layers, waiting for a system with `Commands`.
    retired: Vec<Entity>,
}

impl EphemerisSettings {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// How many layers are actually on screen.
    pub fn drawn(&self) -> usize {
        if !self.enabled {
            return 0;
        }
        self.layers
            .iter()
            .filter(|layer| layer.visible && layer.status == EphemerisStatus::Ready)
            .count()
    }

    /// What the halo should be drawing: the pin if there is one, and otherwise
    /// whatever the cursor is over.
    fn highlight_target(&self) -> Option<SatellitePick> {
        self.pinned.or(self.hovered)
    }

    /// Keeps one object selected until told otherwise. Returns whether the
    /// layer is up and holds it.
    pub fn pin(&mut self, id: &str, norad_id: u64) -> bool {
        let Some(layer) = self.layers.iter().find(|layer| layer.id == id) else {
            return false;
        };
        let Some(catalogue) = layer.catalogue.as_ref() else {
            return false;
        };
        if catalogue.index_of(norad_id).is_none() {
            return false;
        }
        self.pinned = Some(SatellitePick {
            slot: layer.slot,
            norad_id,
        });
        self.revision += 1;
        true
    }

    pub fn clear_pin(&mut self) {
        if self.pinned.take().is_some() {
            self.revision += 1;
        }
    }

    /// The pinned object, named the way a command names one: the layer's id and
    /// the catalogue number.
    ///
    /// [`crate::gnc`] draws one satellite in both frames at once, and the
    /// obvious satellite to draw is whichever one was just clicked on — so
    /// rather than a second selection with a second way of getting out of step
    /// with the first, it follows this one when it is given nothing else.
    pub(crate) fn pinned_object(&self) -> Option<(String, u64)> {
        let pick = self.pinned?;
        let layer = self.layers.iter().find(|layer| layer.slot == pick.slot)?;
        Some((layer.id.clone(), pick.norad_id))
    }

    /// One object's propagator, by layer and catalogue number.
    ///
    /// The `Arc` is cloned out rather than borrowed because the caller
    /// propagates while holding other resources of its own, and a borrow of
    /// this one would keep the whole settings resource locked for as long as it
    /// took — which is the same reason [`draw_ephemerides`] clones it.
    pub(crate) fn propagator(&self, layer: &str, norad_id: u64) -> Option<(Arc<Catalogue>, usize)> {
        let layer = self.layers.iter().find(|held| held.id == layer)?;
        let catalogue = layer.catalogue.as_ref()?;
        let index = catalogue.index_of(norad_id)?;
        Some((catalogue.clone(), index))
    }

    /// Forgets any pick that named this layer — after a removal, because there
    /// is no layer left to have picked anything in. A refresh deliberately does
    /// not: a pin names a catalogue number, and that is the same satellite in
    /// the new document as in the old.
    fn forget_picks(&mut self, slot: u64) {
        for pick in [&mut self.hovered, &mut self.pinned] {
            if pick.is_some_and(|pick| pick.slot == slot) {
                *pick = None;
                self.revision += 1;
            }
        }
    }

    /// Puts a layer up, replacing any already under the same id.
    pub fn add(&mut self, request: EphemerisRequest) {
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

        let mut layer = Ephemeris {
            id: request.id,
            label,
            slot,
            generation: 0,
            style: request.style,
            trail: request.trail.sanitized(),
            trails: request.trails,
            visible: request.visible,
            refresh_seconds: None,
            age_seconds: 0.0,
            status: EphemerisStatus::Loading,
            handle: None,
            catalogue: None,
            requested: request.selection,
            selected: Vec::new(),
            trailed: Vec::new(),
            pending: None,
            // A URL is fetched on the next tick; text is already here, so it is
            // parsed now and the failure, if it is one, reported straight away.
            wants_fetch: matches!(request.source, OverlaySource::Url(_)),
            wants_restyle: false,
            wants_trails: true,
            since_trails: f32::MAX,
            trail_clock: f64::NAN,
            trail_mode: FrameMode::default(),
            markers: Drawn::default(),
            arcs: Drawn::default(),
            highlight: Drawn::default(),
            positions: Arc::default(),
            drawn: Vec::new(),
            drawn_angle: 0.0,
            revision: 0,
            source: request.source,
        };
        layer.set_refresh(request.refresh_seconds);
        if let OverlaySource::Text(text) = &layer.source {
            match omm::parse(text) {
                Ok(parsed) => layer.pending = Some(parsed),
                Err(error) => layer.status = EphemerisStatus::Failed(error.to_string()),
            }
        }

        self.layers.push(layer);
        self.revision += 1;
    }

    /// Takes a layer down. Returns whether there was one.
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(index) = self.layers.iter().position(|layer| layer.id == id) else {
            return false;
        };
        let layer = self.layers.remove(index);
        if let Ok(mut published) = CATALOGUES.write() {
            published.remove(&layer.id);
        }
        if let Ok(mut published) = GEOMETRY.write() {
            published.remove(&layer.id);
        }
        if let Ok(mut urls) = self.urls.write() {
            urls.remove(&layer.slot);
        }
        self.retired.extend(
            [
                layer.markers.entity,
                layer.arcs.entity,
                layer.highlight.entity,
            ]
            .into_iter()
            .flatten(),
        );
        self.forget_picks(layer.slot);
        self.revision += 1;
        true
    }

    pub fn set_visible(&mut self, id: &str, visible: bool) -> bool {
        self.with(id, |layer| layer.visible = visible)
    }

    pub fn set_style(&mut self, id: &str, style: OverlayStyle) -> bool {
        self.with(id, |layer| {
            layer.style = style;
            layer.wants_restyle = true;
        })
    }

    /// Sets how long a layer's trails are and how finely they are drawn.
    pub fn set_trail(&mut self, id: &str, trail: TrailWindow) -> bool {
        self.with(id, |layer| {
            let trail = trail.sanitized();
            if layer.trail != trail {
                layer.trail = trail;
                layer.wants_trails = true;
            }
        })
    }

    /// Whether the layer draws trails at all.
    pub fn set_trails(&mut self, id: &str, trails: bool) -> bool {
        self.with(id, |layer| {
            if layer.trails != trails {
                layer.trails = trails;
                layer.wants_trails = true;
            }
        })
    }

    /// Replaces which objects the layer draws.
    pub fn set_selection(&mut self, id: &str, selection: Selection) -> bool {
        self.with(id, |layer| {
            if layer.requested != selection {
                layer.requested = selection;
                layer.resolve_selection();
                layer.wants_trails = true;
            }
        }) && self.publish(id)
    }

    /// Draws one object, or stops drawing it.
    ///
    /// Selecting past [`MAX_TRACKED`] does nothing rather than silently pushing
    /// another object out: which one it displaced would be arbitrary, and an
    /// interface that shows the count can say the layer is full.
    pub fn select(&mut self, id: &str, norad_id: u64, selected: bool) -> bool {
        let changed = self.with(id, |layer| {
            let Some(catalogue) = layer.catalogue.as_ref() else {
                return;
            };
            let Some(index) = catalogue.index_of(norad_id) else {
                return;
            };
            if layer.selected[index] == selected {
                return;
            }
            if selected && layer.tracked().count() >= MAX_TRACKED {
                return;
            }
            layer.selected[index] = selected;
            // Turning an object on gives it a trail if there is room for one,
            // which is what makes a freshly selected satellite look like the
            // ones beside it rather than like a bare dot.
            if selected && !layer.trailed[index] && layer.trailing().count() < MAX_TRAILED {
                layer.trailed[index] = true;
            }
            // The requested selection follows, so a refresh keeps the choice.
            layer.requested = explicit(layer);
            layer.wants_trails = true;
        });
        changed && self.publish(id)
    }

    /// Trails one object, or stops trailing it.
    pub fn set_object_trail(&mut self, id: &str, norad_id: u64, trail: bool) -> bool {
        let changed = self.with(id, |layer| {
            let Some(catalogue) = layer.catalogue.as_ref() else {
                return;
            };
            let Some(index) = catalogue.index_of(norad_id) else {
                return;
            };
            if layer.trailed[index] == trail {
                return;
            }
            if trail && layer.trailing().count() >= MAX_TRAILED {
                return;
            }
            layer.trailed[index] = trail;
            layer.wants_trails = true;
        });
        changed && self.publish(id)
    }

    /// Sets how often the layer refetches, or `None` to stop refreshing.
    pub fn set_refresh(&mut self, id: &str, seconds: Option<f32>) -> bool {
        self.with(id, |layer| layer.set_refresh(seconds))
    }

    /// Refetches now, whatever the period says.
    pub fn refresh(&mut self, id: &str) -> bool {
        self.with(id, |layer| {
            if matches!(layer.source, OverlaySource::Url(_)) {
                layer.wants_fetch = true;
            }
        })
    }

    fn with(&mut self, id: &str, change: impl FnOnce(&mut Ephemeris)) -> bool {
        let Some(layer) = self.layers.iter_mut().find(|layer| layer.id == id) else {
            return false;
        };
        change(layer);
        layer.revision += 1;
        self.revision += 1;
        true
    }

    /// Republishes one layer's catalogue and selection for the pull-side
    /// bindings. Always answers `true`, so it composes with [`Self::with`].
    fn publish(&self, id: &str) -> bool {
        let Some(layer) = self.layers.iter().find(|layer| layer.id == id) else {
            return true;
        };
        let Some(catalogue) = layer.catalogue.as_ref() else {
            return true;
        };
        if let Ok(mut published) = CATALOGUES.write() {
            published.insert(
                layer.id.clone(),
                Arc::new(PublishedLayer {
                    catalogue: catalogue.clone(),
                    selected: layer.selected.clone(),
                    trailed: layer.trailed.clone(),
                }),
            );
        }
        true
    }
}

/// The selection a layer is currently drawing, written out object by object —
/// which is what a per-object toggle has to leave behind so that a refresh
/// re-applies the same choice to the new document.
fn explicit(layer: &Ephemeris) -> Selection {
    let Some(catalogue) = layer.catalogue.as_ref() else {
        return Selection::All;
    };
    Selection::only(
        layer
            .tracked()
            .map(|index| catalogue.satellites[index].norad_id),
    )
}

// ---------------------------------------------------------------------------
// The document, as an asset
// ---------------------------------------------------------------------------

/// A parsed OMM catalogue, so that fetching and reference counting are Bevy's
/// problem rather than this module's.
#[derive(Asset, TypePath)]
pub struct OmmAsset(pub Catalogue);

#[derive(Debug)]
pub enum OmmLoadError {
    Io(std::io::Error),
    NotText(std::string::FromUtf8Error),
    NotOmm(omm::OmmError),
}

impl std::fmt::Display for OmmLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::NotText(error) => write!(formatter, "not text: {error}"),
            Self::NotOmm(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for OmmLoadError {}

#[derive(Default, TypePath)]
struct OmmLoader;

impl AssetLoader for OmmLoader {
    type Asset = OmmAsset;
    type Settings = ();
    type Error = OmmLoadError;

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
            .map_err(OmmLoadError::Io)?;
        let text = String::from_utf8(bytes).map_err(OmmLoadError::NotText)?;
        omm::parse(&text)
            .map(OmmAsset)
            .map_err(OmmLoadError::NotOmm)
    }

    fn extensions(&self) -> &[&str] {
        &[DOCUMENT_EXTENSION]
    }
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

/// Marks an entity drawing part of an ephemeris layer, and says which one.
#[derive(Component)]
pub struct EphemerisEntity(pub String);

/// The half that has to be in place before `AssetPlugin` builds: the asset
/// source catalogues are fetched over, and the layers themselves.
pub struct EphemerisSourcePlugin {
    /// Layers the globe starts with. A web embedder adds its own through
    /// `addEphemeris` once the module has loaded and leaves this empty.
    pub initial: Vec<EphemerisRequest>,
}

impl Plugin for EphemerisSourcePlugin {
    fn build(&self, app: &mut App) {
        let urls: SharedUrls = Arc::new(RwLock::new(HashMap::new()));
        app.register_asset_source(EPHEMERIS_SOURCE, fetch::source(urls.clone()));

        let mut settings = EphemerisSettings {
            enabled: true,
            picking: true,
            hovered: None,
            pinned: None,
            layers: Vec::new(),
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

/// Drawing ephemerides: the document type, its loader, and the systems that
/// keep what is on screen in step with the clock.
pub struct EphemerisPlugin;

impl Plugin for EphemerisPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<OmmAsset>()
            .init_asset_loader::<OmmLoader>()
            .add_systems(
                Update,
                (
                    ephemeris_controls.run_if(crate::api::keyboard_enabled),
                    age_ephemerides,
                    fetch_ephemerides,
                    poll_ephemerides,
                    adopt_catalogues,
                    draw_ephemerides,
                    pick_satellites,
                    highlight_satellites,
                    restyle_ephemerides,
                    show_ephemerides,
                )
                    .chain()
                    .in_set(FrameSet::Apply)
                    // The clock and the Earth's rotation are settled first —
                    // every coordinate here is a function of both — and the
                    // snapshot goes out after, so what it reports is what was
                    // drawn on the same frame. The cursor is cast before all of
                    // it, and the hit test runs on the positions this frame
                    // built, so what is picked is what is on screen rather than
                    // where everything was a frame ago.
                    .after(crate::api::track_cursor)
                    .before(crate::api::publish_state),
            );
    }
}

fn ephemeris_controls(keys: Res<ButtonInput<KeyCode>>, mut settings: ResMut<EphemerisSettings>) {
    if !keys.just_pressed(KeyCode::KeyO) {
        return;
    }
    if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
        // Every layer together, because the key is a coarse control and a globe
        // with trails on half its layers is not a state worth cycling through.
        let ids: Vec<String> = settings
            .layers
            .iter()
            .map(|layer| layer.id.clone())
            .collect();
        let on = !settings.layers.iter().any(|layer| layer.trails);
        for id in ids {
            settings.set_trails(&id, on);
        }
    } else {
        settings.enabled = !settings.enabled;
        settings.revision += 1;
    }
}

/// Ages every layer, and marks the ones whose refresh period has run out.
fn age_ephemerides(time: Res<Time>, mut settings: ResMut<EphemerisSettings>) {
    let delta = time.delta_secs();
    for layer in &mut settings.layers {
        layer.age_seconds += delta;
        layer.since_trails = (layer.since_trails + delta).min(f32::MAX / 2.0);
        if layer
            .refresh_seconds
            .is_some_and(|period| layer.age_seconds >= period)
        {
            layer.wants_fetch = true;
        }
    }
}

/// Starts whatever fetches are due.
fn fetch_ephemerides(assets: Res<AssetServer>, mut settings: ResMut<EphemerisSettings>) {
    let mut changed = false;
    for layer in &mut settings.layers {
        if !std::mem::take(&mut layer.wants_fetch) {
            continue;
        }
        if layer.handle.is_some() {
            layer.generation = layer.generation.wrapping_add(1);
        }
        layer.handle = Some(assets.load(layer.asset_path()));
        layer.age_seconds = 0.0;
        // A refresh keeps drawing the elements it has until the new ones land,
        // so a catalogue that goes down does not blank the constellation.
        changed |= layer.set_status(EphemerisStatus::Loading);
    }
    if changed {
        settings.revision += 1;
    }
}

/// Promotes the fetches that finished, and records the ones that failed.
fn poll_ephemerides(
    assets: Res<AssetServer>,
    mut documents: ResMut<Assets<OmmAsset>>,
    mut settings: ResMut<EphemerisSettings>,
) {
    let mut changed = false;
    for layer in &mut settings.layers {
        if layer.pending.is_some() {
            continue;
        }
        let Some(handle) = layer.handle.as_ref() else {
            continue;
        };
        match assets.get_load_state(handle) {
            Some(LoadState::Loaded) => {
                if layer.status == EphemerisStatus::Loading
                    // Taken rather than cloned: a `Catalogue` holds an
                    // initialised propagator per object and none of that is
                    // `Clone`, so the asset hands its contents over and is left
                    // empty. Nothing reads it again — the layer owns the
                    // propagators from here, and a refresh loads a new asset
                    // under a new path.
                    && let Some(mut document) = documents.get_mut(handle)
                {
                    layer.pending = Some(std::mem::take(&mut document.0));
                }
            }
            Some(LoadState::Failed(error)) => {
                changed |= layer.set_status(EphemerisStatus::Failed(error.to_string()));
            }
            _ => {}
        }
    }
    if changed {
        settings.revision += 1;
    }
}

/// Adopts whatever catalogues have arrived, and applies the selection to them.
fn adopt_catalogues(mut settings: ResMut<EphemerisSettings>) {
    let mut adopted = Vec::new();
    for layer in &mut settings.layers {
        let Some(catalogue) = layer.pending.take() else {
            continue;
        };
        layer.catalogue = Some(Arc::new(catalogue));
        layer.resolve_selection();
        layer.set_status(EphemerisStatus::Ready);
        layer.wants_trails = true;
        adopted.push(layer.id.clone());
    }
    if adopted.is_empty() {
        return;
    }
    for id in &adopted {
        settings.publish(id);
    }
    settings.revision += 1;
}

/// Propagates everything that is drawn, and turns it into meshes.
fn draw_ephemerides(
    mut commands: Commands,
    settings: ResMut<EphemerisSettings>,
    sun: Res<Sun>,
    frame: Res<ReferenceFrame>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VectorMaterial>>,
) {
    let settings = settings.into_inner();

    for entity in settings.retired.drain(..) {
        commands.entity(entity).despawn();
    }
    if !settings.enabled {
        for layer in &mut settings.layers {
            layer.clear_drawn();
        }
        return;
    }

    let now = sun.unix_seconds;
    let mode = frame.mode;

    for layer in &mut settings.layers {
        let Some(catalogue) = layer.catalogue.clone() else {
            continue;
        };
        if !layer.visible {
            // Hidden is not merely undrawn: propagating a constellation nobody
            // is looking at is the whole cost of this module.
            layer.clear_drawn();
            continue;
        }

        let (positions, drawn) = positions_of(layer, &catalogue, now, mode);
        let markers = crate::overlays::marker_mesh(&positions, None, ALTITUDE, FeaturePaint::Layer);
        // The material is taken before the layer is borrowed to be drawn into.
        let material = layer.marker_material();
        let positions = Arc::new(positions);
        // Kept before it is drawn: the hit test and the halo run off these, and
        // they are the same positions the mesh below was built from.
        layer.positions = positions.clone();
        layer.drawn = drawn;
        layer.drawn_angle = reference_angle(mode, now);

        put(
            &mut commands,
            &mut meshes,
            &mut materials,
            &layer.id,
            &mut layer.markers,
            "markers",
            material,
            markers,
        );

        let trails = trails_due(layer, now, mode).then(|| {
            layer.wants_trails = false;
            layer.since_trails = 0.0;
            layer.trail_clock = now;
            layer.trail_mode = mode;
            arcs_of(layer, &catalogue, now, mode)
        });
        let trails = trails.map(|trails| {
            let material = layer.arc_material();
            put(
                &mut commands,
                &mut meshes,
                &mut materials,
                &layer.id,
                &mut layer.arcs,
                "trails",
                material,
                crate::overlays::line_mesh(&trails, None, false, ALTITUDE, FeaturePaint::Layer),
            );
            Arc::new(trails)
        });

        if let Ok(mut published) = GEOMETRY.write() {
            let previous = published.get(&layer.id);
            let trails = trails.unwrap_or_else(|| {
                previous
                    .map(|geometry| geometry.trails.clone())
                    .unwrap_or_default()
            });
            published.insert(
                layer.id.clone(),
                Arc::new(EphemerisGeometry { positions, trails }),
            );
        }
    }
}

/// Whether a layer's arcs have gone stale enough to be worth redrawing.
fn trails_due(layer: &Ephemeris, now: f64, mode: FrameMode) -> bool {
    if !layer.trails {
        // Left as they were and hidden, rather than rebuilt empty: switching
        // trails back on should not have to propagate everything again.
        return false;
    }
    if layer.wants_trails || layer.trail_mode != mode || !layer.trail_clock.is_finite() {
        return true;
    }
    if layer.since_trails < MIN_TRAIL_INTERVAL_SECONDS {
        return false;
    }
    if layer.since_trails >= MAX_TRAIL_INTERVAL_SECONDS {
        return true;
    }
    // A trail is stale in proportion to the window it spans, so a long arc
    // tolerates more drift than a short one before it has to be drawn again.
    let span_minutes = f64::from(layer.trail.span_orbits()) * shortest_period(layer);
    (now - layer.trail_clock).abs() / 60.0 >= span_minutes * TRAIL_STALE_FRACTION
}

/// The shortest orbital period among the objects a layer is trailing, which is
/// the one whose arc goes stale first.
fn shortest_period(layer: &Ephemeris) -> f64 {
    let Some(catalogue) = layer.catalogue.as_ref() else {
        return 0.0;
    };
    layer
        .trailing()
        .map(|index| catalogue.satellites[index].period_minutes)
        .fold(f64::INFINITY, f64::min)
        .min(f64::MAX)
}

/// Where every tracked object is now, as one point per object — and which
/// object each of those points is.
///
/// The second half is what makes a pick a satellite rather than a dot: an
/// object that cannot be propagated is left out, so the nth point is not the
/// nth tracked object, and only the builder knows which is which.
fn positions_of(
    layer: &Ephemeris,
    catalogue: &Catalogue,
    now: f64,
    mode: FrameMode,
) -> (FeatureSet, Vec<usize>) {
    let angle = reference_angle(mode, now);
    let mut builder = FeatureSetBuilder::new();
    let mut drawn = Vec::new();
    for index in layer.tracked() {
        let satellite = &catalogue.satellites[index];
        let Some(teme) = satellite.position_teme_km(now) else {
            continue;
        };
        drawn.push(index);
        // The catalogue number as the feature id, and no properties at all:
        // this is rebuilt every frame, and serializing a name and an epoch per
        // object per frame would cost more than the propagation does. What an
        // object *is* travels the other way, through `ephemerisObjects`.
        let feature = builder.feature(Some(satellite.norad_id.to_string()), None);
        builder.push_point(feature, place(teme, angle));
    }
    (builder.finish(), drawn)
}

/// The arc through every trailed object, as one line per object.
fn arcs_of(layer: &Ephemeris, catalogue: &Catalogue, now: f64, mode: FrameMode) -> FeatureSet {
    let mut builder = FeatureSetBuilder::new();
    let window = layer.trail;
    if window.span_orbits() <= 0.0 {
        return builder.finish();
    }

    let trailing: Vec<usize> = layer.trailing().collect();
    if trailing.is_empty() {
        return builder.finish();
    }

    // The budget, shared out: one satellite gets the smoothest orbit it asked
    // for, sixty get a coarse one, and both cost the same.
    let share = (MAX_TRAIL_POINTS / trailing.len()) as u32;
    let samples = window
        .samples
        .min(share)
        .clamp(MIN_TRAIL_SAMPLES, MAX_TRAIL_SAMPLES);

    // An orbit is drawn against one rotation for the whole arc — the one the
    // marker is placed with — so the curve is the same in both frames. A track
    // is drawn against each sample's own, which is what makes it a track.
    let anchor = match window.path {
        TrailPath::Orbit => Some(reference_angle(mode, now)),
        TrailPath::GroundTrack => None,
    };

    let mut path = Vec::new();
    for index in trailing {
        let satellite = &catalogue.satellites[index];
        // Samples are stated per whole orbit, so a half-orbit window gets half
        // of them — which is what keeps the spacing along the arc the same
        // whatever the window is set to.
        let span = f64::from(window.span_orbits()) * satellite.period_minutes;
        let steps = ((f64::from(samples) * f64::from(window.span_orbits())).round() as u32).max(2);
        let start = -f64::from(window.trailing_orbits) * satellite.period_minutes;

        path.clear();
        let mut diverged = false;
        for step in 0..=steps {
            let minutes = start + span * f64::from(step) / f64::from(steps);
            let at = now + minutes * 60.0;
            let Some(teme) = satellite.position_teme_km(at) else {
                // SGP4 diverges rather than failing gracefully, and the samples
                // either side of the divergence are not on any orbit. Dropping
                // the arc is the only honest answer; the marker stays if the
                // object can still be placed at the moment that matters.
                diverged = true;
                break;
            };
            path.push(place(
                teme,
                anchor.unwrap_or_else(|| reference_angle(mode, at)),
            ));
        }
        if diverged {
            continue;
        }

        let feature = builder.feature(Some(satellite.norad_id.to_string()), None);
        builder.push_line(feature, path.iter().copied());
    }

    builder.finish()
}

/// How far the Earth is turned, in radians, for a coordinate that is about to
/// be drawn at a given moment.
///
/// This is the whole of the frame handling. In ECEF it is Greenwich's sidereal
/// angle at that moment, which takes an inertial position to a longitude on the
/// turning Earth — so an arc's samples are each unturned by their *own* moment
/// and the result is the corkscrew a ground track is. In ECI world space is
/// already inertial, so there is nothing to undo and a right ascension is a
/// world longitude.
fn reference_angle(mode: FrameMode, unix_seconds: f64) -> f64 {
    match mode {
        FrameMode::Ecef => crate::frame::sidereal_radians(unix_seconds),
        FrameMode::Eci => 0.0,
    }
}

/// Turns a TEME position in kilometres into the coordinate the globe draws.
///
/// Geocentric: declination becomes the latitude, right ascension less the
/// reference angle becomes the longitude, and the radius becomes a height above
/// a sphere of [`EARTH_RADIUS_KM`]. See the module docs for why that sphere and
/// not the ellipsoid.
fn place(teme_km: [f64; 3], reference_angle: f64) -> Position {
    let [x, y, z] = teme_km;
    let radius = (x * x + y * y + z * z).sqrt();
    if radius <= 0.0 {
        return Position::new(0.0, 0.0, 0.0);
    }
    let latitude = (z / radius).clamp(-1.0, 1.0).asin().to_degrees();
    let longitude = (y.atan2(x) - reference_angle).to_degrees();
    Position::new(
        latitude,
        (longitude + 180.0).rem_euclid(360.0) - 180.0,
        (radius - f64::from(EARTH_RADIUS_KM)) * 1000.0,
    )
}

/// Puts a freshly built mesh on screen, reusing the entity and the mesh asset
/// rather than replacing them.
///
/// This runs every frame for the markers, so the churn matters: `meshes.add`
/// would mint an asset id a frame and drop the one before it, which is a
/// render-world insert and remove sixty times a second for geometry that is the
/// same size every time. `insert` against the id already held keeps it and
/// republishes the buffer, which is the part that genuinely changed.
///
/// `insert` rather than writing through `get_mut`, and the difference is not
/// stylistic: the mesh builders make their meshes `RENDER_WORLD` only, so once
/// one has been extracted the main world holds a placeholder rather than the
/// vertices, and anything that reaches into it for an attribute panics.
/// Replacing the whole asset never looks inside.
#[expect(
    clippy::too_many_arguments,
    reason = "spawning into the world needs its commands, both asset stores and the material the part is drawn in"
)]
fn put(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<VectorMaterial>,
    id: &str,
    drawn: &mut Drawn,
    part: &str,
    material: VectorMaterial,
    mesh: Option<Mesh>,
) {
    let Some(mesh) = mesh else {
        drawn.drawing = false;
        return;
    };
    drawn.drawing = true;

    if let Some(handle) = drawn.mesh.as_ref() {
        // The only failure is an id this store has never held, which cannot
        // happen for a handle it minted a moment ago and has held since.
        let _ = meshes.insert(handle.id(), mesh);
        return;
    }

    let handle = meshes.add(mesh);
    let material = materials.add(material);
    let entity = commands
        .spawn((
            Name::new(format!("Ephemeris {id} ({part})")),
            EphemerisEntity(id.to_string()),
            Mesh3d(handle.clone()),
            MeshMaterial3d(material.clone()),
            // No rotation, ever. The geometry is built in whichever frame world
            // space is — see the module docs.
            Transform::IDENTITY,
            // Markers and lines are spread in the vertex shader, so the mesh's
            // own bounds understate what is drawn.
            NoFrustumCulling,
        ))
        .id();
    drawn.mesh = Some(handle);
    drawn.material = Some(material);
    drawn.entity = Some(entity);
}

/// Works out which satellite the cursor is over.
///
/// Measured in pixels against the marker anchors this frame drew, which is the
/// only way that agrees with what is on screen — see the module docs. Every
/// visible layer is asked and the nearest answer across all of them wins, so a
/// satellite in one layer is not lost under one in another.
///
/// The cost is a projection per drawn object per frame, which is a matrix
/// multiply beside the SGP4 evaluation that put the object there in the first
/// place. No spatial index, for the same reason: [`MAX_TRACKED`] already bounds
/// the work at six hundred, and an index would have to be rebuilt every frame
/// because every object moves every frame.
fn pick_satellites(
    cursor: Res<Cursor>,
    camera: Query<(&Camera, &GlobalTransform)>,
    mut settings: ResMut<EphemerisSettings>,
) {
    let Some((camera, camera_transform)) = camera.iter().next() else {
        return;
    };

    let hovered = (settings.enabled && settings.picking)
        .then_some(cursor.screen)
        .flatten()
        .and_then(|pointer| {
            let eye = camera_transform.translation();
            let mut best: Option<(SatellitePick, f32)> = None;

            for layer in &settings.layers {
                if !layer.visible || layer.status != EphemerisStatus::Ready {
                    continue;
                }
                let Some(catalogue) = layer.catalogue.as_ref() else {
                    continue;
                };
                // Half the marker, because it is drawn either side of its
                // anchor; plus a little, because a seven-pixel dot on a moving
                // target is otherwise a seven-pixel target.
                let reach = layer.style.point_size_px * 0.5 + PICK_SLACK_PX;

                for (point, index) in layer.drawn.iter().copied().enumerate() {
                    let anchor =
                        crate::overlays::marker_anchor(layer.positions.point(point).1, ALTITUDE);
                    if occluded(eye, anchor) {
                        continue;
                    }
                    let Ok(screen) = camera.world_to_viewport(camera_transform, anchor) else {
                        // Behind the camera, or off a viewport it has no size
                        // for. Neither is somewhere a marker was drawn.
                        continue;
                    };
                    let distance = screen.distance(pointer);
                    if distance > reach || best.is_some_and(|(_, held)| distance >= held) {
                        continue;
                    }
                    best = Some((
                        SatellitePick {
                            slot: layer.slot,
                            norad_id: catalogue.satellites[index].norad_id,
                        },
                        distance,
                    ));
                }
            }

            best.map(|(pick, _)| pick)
        });

    if settings.hovered != hovered {
        settings.hovered = hovered;
        // The layer's own revision deliberately stays put: it is what tells an
        // interface that the object list it pulled has gone stale, and hovering
        // a dot has not changed a catalogue.
        settings.revision += 1;
    }
}

/// Whether the Earth is between the camera and a point.
///
/// A satellite on the far side is drawn behind an opaque globe, so it is not
/// something the pointer can be over — and picking one would mean a halo
/// appearing around nothing, on the wrong side of the world. Measured against
/// the radius the imagery reaches rather than the sphere underneath it, because
/// the imagery is what is actually in the way.
///
/// The camera is always outside the globe, so the segment passes through it
/// exactly when its closest approach to the centre falls between the two ends
/// and is nearer than the surface.
fn occluded(eye: Vec3, target: Vec3) -> bool {
    let along = target - eye;
    let length_squared = along.length_squared();
    if length_squared <= 0.0 {
        return false;
    }
    let fraction = (-eye.dot(along) / length_squared).clamp(0.0, 1.0);
    (eye + along * fraction).length() < MAX_TILE_RADIUS
}

/// Draws the halo around whatever is picked.
///
/// Rebuilt every frame rather than when the pick changes, which is the opposite
/// of what [`crate::overlays`] does and for the opposite reason: an overlay's
/// feature stays where it is, and a satellite does not. It is one point, so
/// rebuilding it costs the same as deciding not to.
fn highlight_satellites(
    mut commands: Commands,
    settings: ResMut<EphemerisSettings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VectorMaterial>>,
) {
    let settings = settings.into_inner();
    let target = settings.highlight_target().filter(|_| settings.enabled);

    for layer in &mut settings.layers {
        let mesh = target
            .filter(|pick| pick.slot == layer.slot && layer.visible)
            .and_then(|pick| layer.drawn_position(pick.norad_id))
            .and_then(|position| {
                let mut builder = FeatureSetBuilder::new();
                let feature = builder.feature(None, None);
                builder.push_point(feature, position);
                // The same builder the markers are drawn with, so the halo is
                // spread around the same anchor by the same shader.
                crate::overlays::marker_mesh(&builder.finish(), None, ALTITUDE, FeaturePaint::Layer)
            });

        let material = layer.highlight_material();
        put(
            &mut commands,
            &mut meshes,
            &mut materials,
            &layer.id,
            &mut layer.highlight,
            "highlight",
            material,
            mesh,
        );
    }
}

/// Applies a colour or size change without rebuilding any geometry.
fn restyle_ephemerides(
    mut settings: ResMut<EphemerisSettings>,
    mut materials: ResMut<Assets<VectorMaterial>>,
) {
    for layer in &mut settings.layers {
        if !std::mem::take(&mut layer.wants_restyle) {
            continue;
        }
        let parts = [
            (&layer.markers, layer.marker_material()),
            (&layer.arcs, layer.arc_material()),
            // The halo is sized from the marker it goes around, so it restyles
            // with it rather than staying the size the old style was.
            (&layer.highlight, layer.highlight_material()),
        ];
        for (drawn, restyled) in parts {
            if let Some(handle) = drawn.material.as_ref()
                && let Some(mut material) = materials.get_mut(handle)
            {
                *material = restyled;
            }
        }
    }
}

/// Draws only what should be drawn. Each part is hidden on its own terms: the
/// trails have a switch of their own, the halo is there only while something
/// is picked, and all of it stands down while the globe camera is departing
/// for or settled in the heliocentric view — see [`crate::view::is_departing_view`]
/// — since a satellite orbit is Earth-scale geometry with nothing to read
/// against a camera an astronomical unit away. Folding that into the same
/// [`Visibility`] this already recomputes every tick, rather than gating the
/// whole system off, means a catalogue refresh mid-departure still lands
/// correctly hidden instead of leaking a freshly spawned entity in at its
/// default visibility.
fn show_ephemerides(
    settings: Res<EphemerisSettings>,
    view: Res<crate::view::ViewState>,
    mut parts: Query<(Entity, &EphemerisEntity, &mut Visibility)>,
) {
    let departing = crate::view::is_departing_view(&view);
    for (entity, part, mut visibility) in &mut parts {
        let shown = !departing
            && settings.enabled
            && settings
                .layers
                .iter()
                .find(|layer| layer.id == part.0)
                .is_some_and(|layer| {
                    layer.visible
                        && if layer.arcs.entity == Some(entity) {
                            layer.trails && layer.arcs.drawing
                        } else if layer.highlight.entity == Some(entity) {
                            layer.highlight.drawing
                        } else {
                            layer.markers.drawing
                        }
                });
        *visibility = if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}

// ---------------------------------------------------------------------------
// What the state stream reports
// ---------------------------------------------------------------------------

/// Every ephemeris layer, as an interface sees it.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EphemerisState {
    /// The master switch over all of them.
    pub enabled: bool,
    /// How many are on screen: loaded, visible, and the switch on.
    pub drawn: usize,
    pub layers: Vec<EphemerisLayerInfo>,
    /// Whether the cursor is picking satellites.
    pub picking: bool,
    /// The object under the cursor, if there is one.
    pub hovered: Option<PickedSatellite>,
    /// The object that was pinned, if one was. The globe haloes this in
    /// preference to whatever is hovered, so an interface showing one set of
    /// details should prefer it too.
    pub pinned: Option<PickedSatellite>,
    /// The budgets, so an interface can say why a selection stopped growing
    /// rather than leaving it to be discovered.
    pub max_tracked: usize,
    pub max_trailed: usize,
}

/// A satellite the cursor found, with everything the catalogue said about it
/// and where it is at this moment.
///
/// This is `SatelliteInfo` with the layer it belongs to and a position added —
/// separate rather than shared because the two travel in opposite directions
/// and at opposite rates. An object list is pulled, once, for thousands of
/// rows; a pick is pushed with every snapshot, for one.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PickedSatellite {
    /// The id of the layer it belongs to, which is what a command naming it
    /// again — pinning it, for instance — has to be given.
    pub layer: String,
    /// That layer's label, for an interface that has only this to draw from.
    pub layer_label: String,
    pub norad_id: u64,
    pub name: String,
    pub international_designator: Option<String>,
    pub epoch_unix_seconds: f64,
    /// How stale its elements are against the simulated clock, in days. SGP4 is
    /// a fit around its epoch and drifts away from it, so this is how much to
    /// trust the dot.
    pub elements_age_days: f64,
    pub period_minutes: f64,
    pub inclination_deg: f64,
    pub eccentricity: f64,
    /// Whether it is trailed, so an interface can offer the toggle against the
    /// thing it just picked.
    pub trail: bool,
    /// Where it is now, or `None` for an object that is in the catalogue but
    /// not on screen — which is what a pin outliving a selection change looks
    /// like.
    pub position: Option<SubPoint>,
}

/// Where a satellite is, as an interface reads it.
///
/// Earth-fixed whatever frame the globe is drawing in: a longitude here is a
/// place on the ground, not a right ascension, so it means the same thing as
/// the one under the cursor.
#[derive(Serialize, Debug, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct SubPoint {
    pub lat: f64,
    pub lon: f64,
    pub altitude_km: f64,
}

/// One layer, as an interface sees it.
///
/// What is *not* here is the object list: a catalogue can be twelve thousand
/// rows and this goes out ten times a second. `revision` is what says the list
/// pulled through `ephemerisObjects` has gone stale.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EphemerisLayerInfo {
    pub id: String,
    pub label: String,
    /// `"url"` or `"text"`.
    pub source: &'static str,
    pub url: Option<String>,
    pub visible: bool,
    /// `"loading"`, `"ready"` or `"failed"`.
    pub status: &'static str,
    pub error: Option<String>,
    pub refresh_seconds: Option<f32>,
    pub next_refresh_seconds: Option<f32>,
    pub age_seconds: f32,
    /// How many objects the document held, and how many of its records could
    /// not be turned into a propagator.
    pub objects: usize,
    pub rejected: usize,
    /// How many are drawn, and how many of those have an arc through them.
    pub tracked: usize,
    pub trailed: usize,
    /// Whether arcs are drawn at all.
    pub trails: bool,
    pub trail: TrailWindowInfo,
    pub style: OverlayStyleInfo,
    /// How stale the oldest drawn object's elements are against the simulated
    /// clock, in days. SGP4 is a fit around its epoch and drifts away from it,
    /// so this is how much to trust the dots.
    pub oldest_elements_days: Option<f64>,
    /// Bumped by anything an interface has a control for, and by a catalogue
    /// landing. A change here means a pulled object list is out of date.
    pub revision: u64,
}

#[derive(Serialize, Debug, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct TrailWindowInfo {
    pub leading_orbits: f32,
    pub trailing_orbits: f32,
    pub samples: u32,
    /// `"track"` or `"orbit"`.
    pub path: &'static str,
}

impl From<&TrailWindow> for TrailWindowInfo {
    fn from(trail: &TrailWindow) -> Self {
        Self {
            leading_orbits: trail.leading_orbits,
            trailing_orbits: trail.trailing_orbits,
            samples: trail.samples,
            path: trail.path.id(),
        }
    }
}

/// One object of one layer, as the pulled catalogue lists it.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SatelliteInfo {
    pub norad_id: u64,
    pub name: String,
    pub international_designator: Option<String>,
    /// The moment the elements describe, in seconds since the Unix epoch — the
    /// same clock `sun.unixSeconds` is on, so an interface subtracts the two to
    /// get how stale they are.
    pub epoch_unix_seconds: f64,
    pub period_minutes: f64,
    pub inclination_deg: f64,
    pub eccentricity: f64,
    pub selected: bool,
    pub trail: bool,
}

/// Lists everything one layer holds, with what is drawn and what is trailed.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn describe_objects(published: &PublishedLayer) -> Vec<SatelliteInfo> {
    published
        .catalogue
        .satellites
        .iter()
        .enumerate()
        .map(|(index, satellite)| SatelliteInfo {
            norad_id: satellite.norad_id,
            name: satellite.name.clone(),
            international_designator: satellite.international_designator.clone(),
            epoch_unix_seconds: satellite.epoch_unix_seconds,
            period_minutes: satellite.period_minutes,
            inclination_deg: satellite.inclination_deg,
            eccentricity: satellite.eccentricity,
            selected: published.selected.get(index).copied().unwrap_or(false),
            trail: published.trailed.get(index).copied().unwrap_or(false),
        })
        .collect()
}

/// Fills in a pick, or `None` when the layer or the object has since gone.
fn describe_pick(
    settings: &EphemerisSettings,
    pick: SatellitePick,
    unix_seconds: f64,
) -> Option<PickedSatellite> {
    let layer = settings
        .layers
        .iter()
        .find(|layer| layer.slot == pick.slot)?;
    let catalogue = layer.catalogue.as_ref()?;
    let index = catalogue.index_of(pick.norad_id)?;
    let satellite = &catalogue.satellites[index];

    Some(PickedSatellite {
        layer: layer.id.clone(),
        layer_label: layer.label.clone(),
        norad_id: satellite.norad_id,
        name: satellite.name.clone(),
        international_designator: satellite.international_designator.clone(),
        epoch_unix_seconds: satellite.epoch_unix_seconds,
        elements_age_days: satellite.age_days(unix_seconds),
        period_minutes: satellite.period_minutes,
        inclination_deg: satellite.inclination_deg,
        eccentricity: satellite.eccentricity,
        trail: layer.trailed.get(index).copied().unwrap_or(false),
        position: layer
            .drawn_position(pick.norad_id)
            .map(|position| earth_fixed(position, layer.drawn_angle, unix_seconds))
            .map(|position| SubPoint {
                lat: position.lat,
                lon: position.lon,
                altitude_km: position.altitude_m / 1000.0,
            }),
    })
}

/// What the cursor is over, if anything.
fn hovered(settings: &EphemerisSettings, unix_seconds: f64) -> Option<PickedSatellite> {
    settings
        .hovered
        .and_then(|pick| describe_pick(settings, pick, unix_seconds))
}

/// What has been kept selected, if anything.
fn pinned(settings: &EphemerisSettings, unix_seconds: f64) -> Option<PickedSatellite> {
    settings
        .pinned
        .and_then(|pick| describe_pick(settings, pick, unix_seconds))
}

/// The two picks as the state digest compares them: cheap, `Copy`, and without
/// the names and the elements, none of which change while a pick holds.
pub fn pick_digest(settings: &EphemerisSettings) -> crate::api::SatelliteDigest {
    let key = |pick: Option<SatellitePick>| pick.map(|pick| (pick.slot, pick.norad_id));
    (key(settings.hovered), key(settings.pinned))
}

/// One drawn position in Earth-fixed terms, whatever frame it was drawn in.
///
/// A drawn longitude is a right ascension less whatever the frame turned it by
/// — nothing at all in ECI, Greenwich's sidereal angle in ECEF. Putting that
/// back and taking the sidereal angle off instead gives the point on the ground
/// the satellite is over, which is what a longitude means everywhere else the
/// globe reports one.
fn earth_fixed(position: Position, drawn_angle: f64, unix_seconds: f64) -> Position {
    let turn = (drawn_angle - crate::frame::sidereal_radians(unix_seconds)).to_degrees();
    Position::new(
        position.lat,
        (position.lon + turn + 180.0).rem_euclid(360.0) - 180.0,
        position.altitude_m,
    )
}

/// Describes every layer, in the order they were added.
pub fn describe(settings: &EphemerisSettings, unix_seconds: f64) -> EphemerisState {
    EphemerisState {
        enabled: settings.enabled,
        drawn: settings.drawn(),
        picking: settings.picking,
        hovered: hovered(settings, unix_seconds),
        pinned: pinned(settings, unix_seconds),
        max_tracked: MAX_TRACKED,
        max_trailed: MAX_TRAILED,
        layers: settings
            .layers
            .iter()
            .map(|layer| {
                let (tracked, trailed) = layer.counts();
                EphemerisLayerInfo {
                    id: layer.id.clone(),
                    label: layer.label.clone(),
                    source: layer.source.id(),
                    url: layer.source.url().map(str::to_string),
                    visible: layer.visible,
                    status: layer.status.id(),
                    error: layer.status.error().map(str::to_string),
                    refresh_seconds: layer.refresh_seconds,
                    next_refresh_seconds: layer
                        .refresh_seconds
                        .map(|period| (period - layer.age_seconds).max(0.0)),
                    age_seconds: layer.age_seconds,
                    objects: layer.catalogue.as_ref().map_or(0, |held| held.len()),
                    rejected: layer.catalogue.as_ref().map_or(0, |held| held.rejected),
                    tracked,
                    trailed,
                    trails: layer.trails,
                    trail: TrailWindowInfo::from(&layer.trail),
                    style: OverlayStyleInfo::from(&layer.style),
                    oldest_elements_days: oldest_elements_days(layer, unix_seconds),
                    revision: layer.revision,
                }
            })
            .collect(),
    }
}

/// How stale the oldest drawn object's elements are, in days.
fn oldest_elements_days(layer: &Ephemeris, unix_seconds: f64) -> Option<f64> {
    let catalogue = layer.catalogue.as_ref()?;
    layer
        .tracked()
        .map(|index| catalogue.satellites[index].age_days(unix_seconds))
        .fold(None, |held: Option<f64>, age| {
            Some(held.map_or(age, |held| if age.abs() > held.abs() { age } else { held }))
        })
}

/// A settings resource holding one layer, with its catalogue already adopted —
/// the state everything but a fetch runs against.
///
/// Outside `mod tests` because [`crate::gnc`] needs one too: its drawing follows
/// a satellite out of a layer, and the only honest way to test that is against a
/// layer that really holds one. The fields are private and staying that way, so
/// this is the seam rather than making them public for a test.
#[cfg(test)]
pub(crate) fn test_settings(document: &str, id: &str) -> EphemerisSettings {
    let mut settings = EphemerisSettings {
        enabled: true,
        picking: true,
        hovered: None,
        pinned: None,
        layers: Vec::new(),
        urls: Arc::new(RwLock::new(HashMap::new())),
        next_slot: 0,
        revision: 0,
        retired: Vec::new(),
    };
    settings.add(EphemerisRequest {
        source: OverlaySource::Text(document.to_string()),
        ..EphemerisRequest::from_url(id, "")
    });
    let layer = settings.layers.last_mut().expect("a layer");
    layer.catalogue = Some(Arc::new(layer.pending.take().expect("a catalogue")));
    layer.resolve_selection();
    settings
}

/// One record, which is enough to exercise everything that is not a fetch.
#[cfg(test)]
pub(crate) const TEST_ISS: &str = r#"[{
    "OBJECT_NAME": "ISS (ZARYA)",
    "OBJECT_ID": "1998-067A",
    "EPOCH": "2026-09-16T03:25:57.950976",
    "MEAN_MOTION": 15.49133683,
    "ECCENTRICITY": 0.00049077,
    "INCLINATION": 51.631,
    "RA_OF_ASC_NODE": 209.9325,
    "ARG_OF_PERICENTER": 145.256,
    "MEAN_ANOMALY": 214.875,
    "EPHEMERIS_TYPE": 0,
    "CLASSIFICATION_TYPE": "U",
    "NORAD_CAT_ID": 25544,
    "ELEMENT_SET_NO": 999,
    "REV_AT_EPOCH": 58585,
    "BSTAR": 0.00013461457,
    "MEAN_MOTION_DOT": 7.008e-5,
    "MEAN_MOTION_DDOT": 0
}]"#;

#[cfg(test)]
mod tests {
    use super::*;

    use super::{TEST_ISS as ISS, test_settings};

    fn settings() -> EphemerisSettings {
        test_settings(ISS, "stations")
    }

    fn layer() -> Ephemeris {
        settings().layers.pop().expect("a layer")
    }

    #[test]
    fn a_text_source_is_parsed_on_the_spot() {
        let layer = layer();
        assert_eq!(layer.status, EphemerisStatus::Loading);
        assert_eq!(layer.catalogue.as_ref().expect("a catalogue").len(), 1);
        assert_eq!(layer.counts(), (1, 1));
    }

    #[test]
    fn a_position_lands_where_the_orbit_says_it_should() {
        let layer = layer();
        let catalogue = layer.catalogue.clone().expect("a catalogue");
        let now = catalogue.satellites[0].epoch_unix_seconds;

        let (set, drawn) = positions_of(&layer, &catalogue, now, FrameMode::Eci);
        assert_eq!(set.point_count(), 1);
        // And the point knows which object of the catalogue it is, which is
        // what makes it pickable rather than a dot.
        assert_eq!(drawn, vec![0]);
        let (_, position) = set.point(0);
        // The station is inclined 51.6°, so it is never further from the
        // equator than that, and it orbits around 420 km up.
        assert!(position.lat.abs() <= 51.7, "{position:?}");
        assert!(
            (350_000.0..500_000.0).contains(&position.altitude_m),
            "{position:?}"
        );
        assert_eq!(set.feature_id(0), Some("25544"));
    }

    #[test]
    fn the_two_frames_disagree_by_exactly_the_earth_rotation() {
        let layer = layer();
        let catalogue = layer.catalogue.clone().expect("a catalogue");
        let now = catalogue.satellites[0].epoch_unix_seconds;

        let inertial = positions_of(&layer, &catalogue, now, FrameMode::Eci)
            .0
            .point(0)
            .1;
        let fixed = positions_of(&layer, &catalogue, now, FrameMode::Ecef)
            .0
            .point(0)
            .1;

        // Same place, same height: only the longitude origin moved.
        assert!((inertial.lat - fixed.lat).abs() < 1.0e-9);
        assert!((inertial.altitude_m - fixed.altitude_m).abs() < 1.0e-6);

        let turn = crate::frame::sidereal_radians(now).to_degrees();
        let apart = (inertial.lon - fixed.lon - turn).rem_euclid(360.0);
        assert!(apart < 1.0e-6 || (360.0 - apart) < 1.0e-6, "{apart}");
    }

    #[test]
    fn an_inertial_trail_closes_and_an_earth_fixed_one_does_not() {
        let mut layer = layer();
        layer.trail = TrailWindow {
            leading_orbits: 1.0,
            trailing_orbits: 0.0,
            samples: 64,
            ..TrailWindow::default()
        };
        let catalogue = layer.catalogue.clone().expect("a catalogue");
        let now = catalogue.satellites[0].epoch_unix_seconds;

        let ends = |mode| {
            let set = arcs_of(&layer, &catalogue, now, mode);
            let (_, line) = set.line(0);
            (line.get(0), line.get(line.len() - 1))
        };

        // In ECI a whole revolution comes back to where it started, give or
        // take the drift between the Kozai period and SGP4's own.
        let (start, end) = ends(FrameMode::Eci);
        assert!((start.lon - end.lon).abs() < 2.0, "{start:?} {end:?}");

        // In ECEF the Earth turned about 23° under it in that time, which is
        // exactly what makes a ground track a corkscrew rather than a loop.
        let (start, end) = ends(FrameMode::Ecef);
        let drift = (start.lon - end.lon).rem_euclid(360.0);
        assert!((20.0..27.0).contains(&drift), "{drift}");
    }

    #[test]
    fn the_sample_budget_is_shared_out_rather_than_exceeded() {
        let layer = layer();
        let catalogue = layer.catalogue.clone().expect("a catalogue");
        let now = catalogue.satellites[0].epoch_unix_seconds;
        let set = arcs_of(&layer, &catalogue, now, FrameMode::Eci);
        // One satellite, half an orbit either way, 128 samples per orbit: the
        // whole window is one orbit, so it gets all of them (plus the closing
        // vertex).
        assert_eq!(set.line_count(), 1);
        assert_eq!(set.line(0).1.len(), 129);
    }

    #[test]
    fn a_trail_window_cannot_be_set_past_what_means_anything() {
        let window = TrailWindow {
            leading_orbits: 9.0,
            trailing_orbits: f32::NAN,
            samples: 1,
            ..TrailWindow::default()
        }
        .sanitized();
        assert_eq!(window.leading_orbits, MAX_TRAIL_ORBITS);
        assert_eq!(window.trailing_orbits, 0.0);
        assert_eq!(window.samples, MIN_TRAIL_SAMPLES);
    }

    #[test]
    fn a_selection_names_objects_rather_than_positions() {
        let mut layer = layer();
        layer.requested = Selection::only([99999]);
        layer.resolve_selection();
        assert_eq!(layer.counts(), (0, 0));

        layer.requested = Selection::only([25544]);
        layer.resolve_selection();
        assert_eq!(layer.counts(), (1, 1));
    }

    #[test]
    fn the_height_is_measured_from_the_sphere_the_globe_draws() {
        // A position straight up the TEME x axis at one Earth radius plus a
        // hundred kilometres is a hundred kilometres up on the equator, and at
        // zero longitude when nothing is turned.
        let position = place([f64::from(EARTH_RADIUS_KM) + 100.0, 0.0, 0.0], 0.0);
        assert!(position.lat.abs() < 1.0e-9, "{position:?}");
        assert!(position.lon.abs() < 1.0e-9, "{position:?}");
        assert!(
            (position.altitude_m - 100_000.0).abs() < 1.0e-3,
            "{position:?}"
        );
    }
    #[test]
    fn a_pin_names_an_object_rather_than_a_row() {
        let mut settings = settings();
        assert!(settings.pin("stations", 25544));
        assert_eq!(
            settings.highlight_target().map(|pick| pick.norad_id),
            Some(25544)
        );

        // Nothing else in the document, and no layer by that name.
        assert!(!settings.pin("stations", 99999));
        assert!(!settings.pin("elsewhere", 25544));
        // A refused pin leaves the one that was held.
        assert_eq!(
            settings.highlight_target().map(|pick| pick.norad_id),
            Some(25544)
        );

        settings.clear_pin();
        assert_eq!(settings.highlight_target(), None);
    }

    #[test]
    fn a_pin_outlives_a_refresh_and_not_a_removal() {
        let mut settings = settings();
        assert!(settings.pin("stations", 25544));

        // A refetch replaces the catalogue under the same slot. The pin names a
        // catalogue number, so it still means the same satellite.
        let catalogue = omm::parse(ISS).expect("a catalogue");
        let layer = settings.layers.last_mut().expect("a layer");
        layer.catalogue = Some(Arc::new(catalogue));
        layer.resolve_selection();
        assert!(settings.pinned.is_some());

        settings.remove("stations");
        assert_eq!(settings.pinned, None);
    }

    #[test]
    fn the_halo_follows_the_pin_and_the_cursor_follows_nothing() {
        let mut settings = settings();
        settings.hovered = Some(SatellitePick {
            slot: 0,
            norad_id: 25544,
        });
        // Hovered is what there is, until something is pinned over it.
        assert_eq!(settings.highlight_target(), settings.hovered);

        settings.pinned = Some(SatellitePick {
            slot: 0,
            norad_id: 99999,
        });
        assert_eq!(settings.highlight_target(), settings.pinned);
    }

    #[test]
    fn a_pick_reports_the_object_and_where_it_is_now() {
        let mut settings = settings();
        let catalogue = settings.layers[0].catalogue.clone().expect("a catalogue");
        let now = catalogue.satellites[0].epoch_unix_seconds;

        // What `draw_ephemerides` leaves behind, which is what a pick reads.
        let (positions, drawn) =
            positions_of(&settings.layers[0], &catalogue, now, FrameMode::Ecef);
        let layer = &mut settings.layers[0];
        layer.positions = Arc::new(positions);
        layer.drawn = drawn;
        layer.drawn_angle = reference_angle(FrameMode::Ecef, now);

        assert!(settings.pin("stations", 25544));
        let state = describe(&settings, now);
        let picked = state.pinned.expect("a pick");
        assert_eq!(picked.layer, "stations");
        assert_eq!(picked.norad_id, 25544);
        assert_eq!(picked.name, "ISS (ZARYA)");
        assert!(picked.trail);
        assert!(picked.elements_age_days.abs() < 1.0e-6);

        let point = picked.position.expect("a position");
        assert!(point.lat.abs() <= 51.7, "{point:?}");
        assert!((350.0..500.0).contains(&point.altitude_km), "{point:?}");
    }

    #[test]
    fn a_position_is_reported_on_the_ground_whichever_frame_it_was_drawn_in() {
        let mut settings = settings();
        let catalogue = settings.layers[0].catalogue.clone().expect("a catalogue");
        let now = catalogue.satellites[0].epoch_unix_seconds;
        assert!(settings.pin("stations", 25544));

        let mut reported = |mode| {
            let (positions, drawn) = positions_of(&settings.layers[0], &catalogue, now, mode);
            let layer = &mut settings.layers[0];
            layer.positions = Arc::new(positions);
            layer.drawn = drawn;
            layer.drawn_angle = reference_angle(mode, now);
            describe(&settings, now)
                .pinned
                .expect("a pick")
                .position
                .expect("a position")
        };

        // Drawn in an inertial frame, the longitude is a right ascension; drawn
        // Earth-fixed it is a place. Reported, they are the same place.
        let inertial = reported(FrameMode::Eci);
        let fixed = reported(FrameMode::Ecef);
        assert!((inertial.lat - fixed.lat).abs() < 1.0e-9);
        let apart = (inertial.lon - fixed.lon).abs();
        assert!(apart < 1.0e-6 || (360.0 - apart) < 1.0e-6, "{apart}");
    }

    #[test]
    fn a_pick_is_nothing_while_the_object_is_not_drawn() {
        let mut settings = settings();
        let now = settings.layers[0]
            .catalogue
            .as_ref()
            .expect("a catalogue")
            .satellites[0]
            .epoch_unix_seconds;
        assert!(settings.pin("stations", 25544));

        // Pinned, in the catalogue, and nowhere on screen: the layer knows what
        // it is without knowing where it is.
        let picked = describe(&settings, now).pinned.expect("a pick");
        assert_eq!(picked.norad_id, 25544);
        assert_eq!(picked.position.map(|point| point.lat), None);
    }

    #[test]
    fn the_far_side_of_the_globe_is_not_pickable() {
        // A camera out along +Z, and a satellite just above the surface.
        let eye = Vec3::new(0.0, 0.0, 4.0);
        assert!(!occluded(eye, Vec3::new(0.0, 0.0, 1.1)));
        // The same height, on the far side: the Earth is in the way.
        assert!(occluded(eye, Vec3::new(0.0, 0.0, -1.1)));
        // Out past the limb, which is drawn against the sky and is grabbable.
        assert!(!occluded(eye, Vec3::new(1.5, 0.0, 0.0)));
        // And a satellite between the camera and the globe never is.
        assert!(!occluded(eye, Vec3::new(0.0, 0.0, 2.0)));
    }

    #[test]
    fn a_marker_is_anchored_where_the_satellite_is_drawn() {
        // A satellite four hundred kilometres up is anchored four hundred
        // kilometres up, on the same scale the globe is a unit sphere at — not
        // on the ground under it, which is where picking in degrees would put
        // it.
        let anchor = crate::overlays::marker_anchor(Position::new(0.0, 0.0, 400_000.0), ALTITUDE);
        let height = f64::from(anchor.length()) - 1.0;
        let expected = 400.0 / f64::from(EARTH_RADIUS_KM);
        // A shade over, and only a shade: every marker is drawn off the drape
        // the imagery sits on rather than off the sphere, which is a dozen
        // kilometres against four hundred of orbit.
        assert!(height > expected, "{anchor:?}");
        assert!(
            height - expected < 20.0 / f64::from(EARTH_RADIUS_KM),
            "{anchor:?}"
        );
    }
    #[test]
    fn an_arc_stays_on_its_satellite_in_either_frame() {
        let layer = layer();
        let catalogue = layer.catalogue.clone().expect("a catalogue");
        let now = catalogue.satellites[0].epoch_unix_seconds;

        // The window is half an orbit either way, so the middle sample is the
        // moment the marker is drawn at. The two have to agree in both frames:
        // a switch rebuilds the arc into the frame it is drawn in, and if it
        // ever did not, every arc would hang the whole Earth rotation away from
        // the satellite it belongs to.
        for mode in [FrameMode::Ecef, FrameMode::Eci] {
            let positions = positions_of(&layer, &catalogue, now, mode).0;
            let marker = positions.point(0).1;
            let arcs = arcs_of(&layer, &catalogue, now, mode);
            let (_, line) = arcs.line(0);
            let middle = line.get(line.len() / 2);
            assert!((middle.lat - marker.lat).abs() < 1.0e-6, "{mode:?}");
            assert!((middle.lon - marker.lon).abs() < 1.0e-6, "{mode:?}");
        }
    }
    #[test]
    fn an_orbit_is_the_same_curve_in_both_frames_and_a_track_is_not() {
        let mut layer = layer();
        let catalogue = layer.catalogue.clone().expect("a catalogue");
        let now = catalogue.satellites[0].epoch_unix_seconds;

        // Both arcs as places on the ground: the inertial one brought back
        // through the rotation the globe is drawn with, which is what the eye
        // compares across a frame switch.
        let turn = crate::frame::sidereal_radians(now).to_degrees();
        let ends = |layer: &Ephemeris, mode| {
            let arcs = arcs_of(layer, &catalogue, now, mode);
            let (_, line) = arcs.line(0);
            let back = match mode {
                FrameMode::Eci => turn,
                FrameMode::Ecef => 0.0,
            };
            let ground = |lon: f64| (lon - back + 180.0).rem_euclid(360.0) - 180.0;
            (
                ground(line.get(0).lon),
                ground(line.get(line.len() - 1).lon),
            )
        };
        let apart = |(a, b): (f64, f64), (c, d): (f64, f64)| {
            let wrap = |value: f64| ((value + 180.0).rem_euclid(360.0) - 180.0).abs();
            wrap(a - c).max(wrap(b - d))
        };

        // A track is a different curve in each frame, by exactly the Earth's
        // rotation over half the window either way — a little under twelve
        // degrees for the station's ninety-minute orbit.
        layer.trail.path = TrailPath::GroundTrack;
        let moved = apart(ends(&layer, FrameMode::Ecef), ends(&layer, FrameMode::Eci));
        assert!((11.0..12.5).contains(&moved), "{moved}");

        // An orbit is the same curve in both, which is the whole point of it:
        // switching frames leaves it where it was.
        layer.trail.path = TrailPath::Orbit;
        let moved = apart(ends(&layer, FrameMode::Ecef), ends(&layer, FrameMode::Eci));
        assert!(moved < 1.0e-6, "{moved}");
    }

    #[test]
    fn an_orbit_arc_still_runs_through_its_satellite() {
        let mut layer = layer();
        layer.trail.path = TrailPath::Orbit;
        let catalogue = layer.catalogue.clone().expect("a catalogue");
        let now = catalogue.satellites[0].epoch_unix_seconds;

        for mode in [FrameMode::Ecef, FrameMode::Eci] {
            let positions = positions_of(&layer, &catalogue, now, mode).0;
            let marker = positions.point(0).1;
            let arcs = arcs_of(&layer, &catalogue, now, mode);
            let (_, line) = arcs.line(0);
            let middle = line.get(line.len() / 2);
            assert!((middle.lat - marker.lat).abs() < 1.0e-6, "{mode:?}");
            assert!((middle.lon - marker.lon).abs() < 1.0e-6, "{mode:?}");
        }
    }
}
