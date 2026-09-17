//! The control surface the globe is driven through from outside.
//!
//! Everything the keyboard does — and everything the on-screen readout knows —
//! is reachable from here, so an embedder can build its own interface over the
//! globe rather than around it. `terramenta-webapp` is the reference one.
//!
//! It works in two directions, and both are deliberately one-way:
//!
//! * **In**, as [`GlobeCommand`]s. They are pushed onto a queue from wherever
//!   the caller happens to be — a JavaScript event handler, another thread —
//!   and drained inside the schedule by [`apply_commands`], which is the only
//!   place that touches the `World`. Nothing is applied mid-tick, so a burst of
//!   commands from one UI interaction lands together on the next frame.
//! * **Out**, as a [`GlobeState`] snapshot. It is rebuilt from the same
//!   resources the HUD reads, published to whoever is listening, and left in
//!   [`LatestState`] for anything inside the app that wants it — which is how
//!   the HUD gets its numbers without computing them twice.
//!
//! On the web the two ends are bound to JavaScript in [`crate::wasm`]. Natively
//! the queue works just the same; only the outbound listener is web-only, since
//! a native embedder can read [`LatestState`] straight out of the `World`.

use std::sync::{LazyLock, Mutex};

use bevy::prelude::*;
use serde::Serialize;

use crate::camera::OrbitCamera;
use crate::ephemeris::{self, EphemerisSettings};
use crate::frame::{FrameRealigned, FrameSet, ReferenceFrame};
use crate::geo::ray_sphere_intersection;
use crate::globe::GLOBE_RADIUS;
use crate::hud::HudSettings;
use crate::imagery::{ImageryLayer, ImagerySettings};
use crate::overlays::{self, OverlaySettings};
use crate::sun::{self, Sun};
use crate::tiles::TileCache;
use crate::vector_tiles::{self, VectorTileCache, VectorTileSettings};

/// The types a command or a snapshot is stated in, re-exported so the control
/// surface is nameable from one place. A native embedder sending a
/// [`GlobeCommand::LookAt`] or an [`GlobeCommand::AddOverlay`] has to be able
/// to name what it is sending, and which module the type lives in is the
/// globe's own business.
pub use crate::ephemeris::{
    EphemerisLayerInfo, EphemerisRequest, EphemerisState, MAX_TRACKED, MAX_TRAILED, SatelliteInfo,
    Selection, TrailWindow, default_style as ephemeris_style,
};
// `SetFrame` takes one of these, so a native embedder has to be able to name
// it; the module it lives in is the globe's own business.
pub use crate::frame::FrameMode;
pub use crate::geo::LatLon;
pub use crate::overlays::{
    AltitudeMode, MIN_REFRESH_SECONDS, OverlayAltitude, OverlayInfo, OverlayRequest, OverlaySource,
    OverlayStyle, PickedFeature,
};
pub use crate::vector_tiles::{VectorTileLayer, VectorTileLayerInfo, VectorTilesState};

/// How often the state snapshot goes out, in seconds.
///
/// The globe runs at the display's refresh rate and most of the snapshot
/// changes on every one of those frames, so publishing each tick would spend
/// more time serializing than an interface can use. A discrete change — a layer
/// switch, the clock pausing — is published the moment it happens regardless,
/// so controls still answer instantly.
const STATE_INTERVAL_SECONDS: f32 = 0.1;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// One instruction for the globe, applied on the next tick.
#[derive(Debug, Clone, PartialEq)]
pub enum GlobeCommand {
    /// Swings the camera around to look straight down at a coordinate, and
    /// optionally to a given height above it.
    ///
    /// The coordinate is Earth-fixed. In ECI the ground turns underneath the
    /// camera afterwards, so this points at a place rather than following it.
    LookAt {
        coordinate: LatLon,
        altitude_km: Option<f32>,
    },
    /// Height above the surface, in kilometres.
    SetAltitude(f32),
    /// Turns the view by an angle, in degrees, the way a drag would.
    OrbitBy {
        yaw_deg: f32,
        pitch_deg: f32,
    },
    /// Zooms by an exponent: positive closes in, negative pulls back.
    ZoomBy(f32),
    ResetView,

    SetFrame(FrameMode),
    ToggleFrame,

    SetSunPaused(bool),
    /// Simulated seconds per real second.
    SetTimeScale(f32),
    /// Jumps the simulated clock to a moment, in seconds since the Unix epoch.
    SetClock(f64),
    SnapClockToNow,
    /// Whether the sun lights the globe at all; off is flat full daylight.
    SetSunShaded(bool),

    /// Selects an imagery preset by index, wrapping past the end of the list.
    SetLayer(usize),
    NextLayer,
    PreviousLayer,
    SetImageryEnabled(bool),

    /// Whether Mapbox Vector Tiles are streamed at all. Off, every tile is
    /// dropped; back on, the view is re-walked and refetched.
    SetVectorTilesEnabled(bool),
    /// Selects a vector tile preset by index, wrapping past the end of the list.
    SetVectorTileLayer(usize),
    NextVectorTileLayer,
    PreviousVectorTileLayer,
    /// Recolours the vector tile layer. Unlike an overlay this rebuilds the
    /// tiles on screen: a tile's meshes are keyed to the style they were built
    /// with, and a layer whose fill was transparent never built a fill at all.
    SetVectorTileStyle(OverlayStyle),

    /// Puts a GeoJSON overlay up, replacing any already under the same id —
    /// which is how a refreshed local file becomes an update rather than a
    /// second copy of the layer.
    AddOverlay(OverlayRequest),
    RemoveOverlay(String),
    SetOverlayVisible {
        id: String,
        visible: bool,
    },
    SetOverlayStyle {
        id: String,
        style: OverlayStyle,
    },
    /// Seconds between refetches, or `None` to stop refreshing. Ignored for a
    /// layer the globe did not fetch and so cannot fetch again.
    /// How a layer reads the heights in its positions. Moves its geometry, so
    /// the layer is rebuilt — from the document already loaded, not refetched.
    SetOverlayAltitude {
        id: String,
        altitude: OverlayAltitude,
    },
    SetOverlayRefresh {
        id: String,
        seconds: Option<f32>,
    },
    /// Refetches now, whatever the period says.
    RefreshOverlay(String),
    /// Whether overlays are drawn at all. Off, every layer stays loaded.
    SetOverlaysEnabled(bool),

    /// Puts an ephemeris layer up from an OMM catalogue, replacing any already
    /// under the same id.
    AddEphemeris(EphemerisRequest),
    RemoveEphemeris(String),
    SetEphemerisVisible {
        id: String,
        visible: bool,
    },
    SetEphemerisStyle {
        id: String,
        style: OverlayStyle,
    },
    /// Which objects of the catalogue are drawn. Replaces the whole selection;
    /// `Selection::All` is everything the document held, down to the budget.
    SetEphemerisSelection {
        id: String,
        selection: Selection,
    },
    /// Draws one object, or stops drawing it, by catalogue number.
    SelectSatellite {
        id: String,
        norad_id: u64,
        selected: bool,
    },
    /// Draws one object's orbit arc, or stops drawing it.
    SetSatelliteTrail {
        id: String,
        norad_id: u64,
        trail: bool,
    },
    /// Whether the layer draws arcs at all.
    SetEphemerisTrails {
        id: String,
        trails: bool,
    },
    /// How far the arcs run either side of now, and how finely they are drawn.
    SetEphemerisTrail {
        id: String,
        trail: TrailWindow,
    },
    SetEphemerisRefresh {
        id: String,
        seconds: Option<f32>,
    },
    RefreshEphemeris(String),
    /// Whether ephemerides are drawn at all. Off, every layer stays loaded and
    /// stops being propagated, which is where the cost of one goes.
    SetEphemeridesEnabled(bool),

    /// Whether the cursor picks anything at all — overlay features and
    /// satellites both. One switch, because "the cursor picks things" is one
    /// thing to whoever is using the globe.
    SetPickingEnabled(bool),
    /// Keeps a feature selected, whatever the cursor does afterwards.
    ///
    /// This is what a click becomes. The globe reports what is under the
    /// pointer; deciding that one of those is *the* selection is an interface's
    /// business, and this is how it says so.
    PinFeature {
        layer: String,
        index: usize,
    },
    ClearPinnedFeature,

    /// Keeps a satellite selected, whatever the cursor does afterwards. The
    /// same idea as [`GlobeCommand::PinFeature`], and by catalogue number
    /// rather than by position, so it survives the layer being refetched.
    PinSatellite {
        layer: String,
        norad_id: u64,
    },
    ClearPinnedSatellite,

    SetHudVisible(bool),
    SetHelpVisible(bool),
    /// Whether the globe's own keyboard shortcuts are live. An embedder that
    /// binds its own keys turns these off so the two do not both fire.
    SetKeyboardEnabled(bool),
}

/// Commands waiting to be applied.
///
/// A plain queue behind a lock rather than a Bevy message: it is written to
/// from outside the app, often before the app has even started, and it has to
/// survive that wait.
static QUEUE: LazyLock<Mutex<Vec<GlobeCommand>>> = LazyLock::new(Mutex::default);

/// Queues a command. Safe to call before the globe starts.
pub fn send(command: GlobeCommand) {
    if let Ok(mut queue) = QUEUE.lock() {
        queue.push(command);
    }
}

fn take_queued() -> Vec<GlobeCommand> {
    QUEUE
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Everything an interface needs to render the globe's condition.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GlobeState {
    pub camera: CameraState,
    pub frame: FrameState,
    pub sun: SunState,
    pub imagery: ImageryState,
    /// The Mapbox Vector Tile layer, and how much of it is on screen.
    pub vector_tiles: VectorTilesState,
    /// Every GeoJSON overlay, in the order they were added.
    pub overlays: OverlaysState,
    /// Every ephemeris layer, in the order they were added.
    pub ephemerides: EphemerisState,
    pub hud: HudState,
    /// The coordinate under the pointer, or `None` when it is off the globe.
    pub cursor: Option<LatLon>,
    /// Whether the globe's own keyboard shortcuts are live.
    pub keyboard: bool,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CameraState {
    /// The coordinate the camera hangs over, in Earth-fixed terms.
    pub center: LatLon,
    pub altitude_km: f32,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FrameState {
    /// `"ecef"` or `"eci"`.
    pub mode: &'static str,
    pub label: &'static str,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SunState {
    pub paused: bool,
    pub shaded: bool,
    pub time_scale: f32,
    /// The simulated clock, in seconds since the Unix epoch.
    pub unix_seconds: f64,
    /// The same clock as `14:32 UTC`.
    pub utc: String,
    pub subsolar: LatLon,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ImageryState {
    pub enabled: bool,
    pub layer_index: usize,
    /// Protocol and name together, as the readout shows them.
    pub label: String,
    pub protocol: &'static str,
    /// How deep the active layer's pyramid goes.
    pub max_level: u8,
    /// How deep the walk actually went this frame.
    pub deepest_level: u8,
    pub visible_tiles: usize,
    pub loading_tiles: usize,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OverlaysState {
    /// The master switch over all of them.
    pub enabled: bool,
    /// How many are on screen: loaded, visible, and the switch on.
    pub drawn: usize,
    pub layers: Vec<OverlayInfo>,
    /// Whether the cursor is picking features.
    pub picking: bool,
    /// The feature under the cursor, with its properties.
    pub hovered: Option<PickedFeature>,
    /// The feature that was pinned, if one was. The globe highlights this in
    /// preference to whatever is hovered, so an interface showing one set of
    /// properties should prefer it too.
    pub pinned: Option<PickedFeature>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HudState {
    pub visible: bool,
    pub help_visible: bool,
}

/// One of the imagery presets, as an embedder sees it before picking one.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LayerInfo {
    pub index: usize,
    pub label: String,
    pub protocol: &'static str,
    pub max_level: u8,
    pub tile_size: u32,
    /// `"jpg"` or `"png"`.
    pub format: &'static str,
}

/// The ranges the controls accept, so an interface can build its sliders from
/// the globe rather than from a guess.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub min_altitude_km: f32,
    pub max_altitude_km: f32,
    pub min_time_scale: f32,
    pub max_time_scale: f32,
    /// How far from the equator a vector tile can reach.
    ///
    /// Mercator sends the poles to infinity, so every vector tile scheme stops
    /// where the projected world is square. Nothing above this latitude is in
    /// any tile, and an interface is better off saying so than leaving someone
    /// to wonder why the Arctic has no coastline.
    pub max_vector_tile_latitude: f32,
}

impl Limits {
    pub fn current() -> Self {
        let (min_altitude_km, max_altitude_km) = OrbitCamera::altitude_limits_km();
        Self {
            min_altitude_km,
            max_altitude_km,
            min_time_scale: sun::MIN_TIME_SCALE,
            max_time_scale: sun::MAX_TIME_SCALE,
            max_vector_tile_latitude: crate::mvt::MAX_LATITUDE,
        }
    }
}

/// Describes the presets a globe built from this list would offer.
pub fn describe_layers(presets: &[ImageryLayer]) -> Vec<LayerInfo> {
    presets
        .iter()
        .enumerate()
        .map(|(index, layer)| LayerInfo {
            index,
            label: layer.label().to_string(),
            protocol: layer.protocol(),
            max_level: layer.max_level(),
            tile_size: layer.tile_size(),
            format: layer.format().extension(),
        })
        .collect()
}

/// The most recent snapshot, kept in the `World` for anything inside the app
/// that would otherwise recompute it.
#[derive(Resource, Debug, Clone, Default)]
pub struct LatestState(pub Option<GlobeState>);

/// Where the pointer is, in both of the terms the globe picks in.
///
/// Worked out once a tick and left here, because more than one thing wants it:
/// the readout shows it, and [`crate::overlays`] hit-tests against it. Two
/// ray-sphere intersections a frame is not the cost — two of them disagreeing
/// by a tick would be, because then the feature that highlights is not the one
/// the coordinate readout says you are over.
///
/// Both terms, because the globe picks in both. Anything drawn on the ground is
/// hit-tested where it stands, in degrees — see [`crate::picking`]. Anything
/// drawn hundreds of kilometres above it cannot be: a satellite's marker is
/// nowhere near its own sub-satellite point on screen, so [`crate::ephemeris`]
/// measures in pixels instead, and it needs the pointer in pixels to do it.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct Cursor {
    /// The coordinate under the pointer, in Earth-fixed terms, or `None` when
    /// it is off the globe or off the window.
    pub ground: Option<LatLon>,
    /// Where the pointer is in the window, in logical pixels, or `None` when it
    /// is outside it. Set even when the pointer is off the globe — which is
    /// exactly where a satellite over the limb is.
    pub screen: Option<Vec2>,
}

/// Casts the pointer onto the globe.
pub(crate) fn track_cursor(
    camera: Query<(&Camera, &GlobalTransform)>,
    windows: Query<&Window>,
    frame: Res<ReferenceFrame>,
    mut cursor: ResMut<Cursor>,
) {
    let Some((camera, camera_transform)) = camera.iter().next() else {
        return;
    };
    cursor.screen = windows.iter().find_map(|window| window.cursor_position());
    cursor.ground = cursor
        .screen
        .and_then(|position| camera.viewport_to_world(camera_transform, position).ok())
        .and_then(|ray| ray_sphere_intersection(ray.origin, *ray.direction, GLOBE_RADIUS))
        // The hit is in world space; the coordinate under it is Earth-fixed.
        .map(|hit| LatLon::from_direction(frame.world_to_earth() * hit));
}

/// One feature, as the digest names it: the overlay's slot and the feature's
/// index. Slots are never reused, so this stays unambiguous across a layer
/// being taken down and another put up under the same name.
type FeatureKey = (u64, usize);

/// The hovered and pinned features, in that order.
pub(crate) type PickDigest = (Option<FeatureKey>, Option<FeatureKey>);

/// One satellite, as the digest names it: the layer's slot and the catalogue
/// number. Both stable — a slot is never reused, and a catalogue number names
/// the object rather than its row.
type SatelliteKey = (u64, u64);

/// The hovered and pinned satellites, in that order.
pub(crate) type SatelliteDigest = (Option<SatelliteKey>, Option<SatelliteKey>);

/// The discrete part of the state — the fields a control flips rather than the
/// ones that drift every frame. A change here publishes immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Digest {
    frame: &'static str,
    sun_paused: bool,
    sun_shaded: bool,
    imagery_enabled: bool,
    layer_index: usize,
    vector_tiles_enabled: bool,
    vector_layer_index: usize,
    vector_deepest_level: u8,
    vector_visible_tiles: usize,
    vector_loading_tiles: usize,
    hud_visible: bool,
    help_visible: bool,
    keyboard: bool,
    deepest_level: u8,
    visible_tiles: usize,
    loading_tiles: usize,
    overlays_enabled: bool,
    picking: bool,
    /// Which feature, rather than the whole of it: the properties can be a page
    /// of JSON, and comparing them every frame to discover they have not
    /// changed would cost more than publishing does.
    hovered: Option<FeatureKey>,
    pinned: Option<FeatureKey>,
    /// And the same for satellites, so that hovering one publishes on the next
    /// frame rather than on the next tick of the throttle.
    hovered_satellite: Option<SatelliteKey>,
    pinned_satellite: Option<SatelliteKey>,
    /// Ephemerides keep a counter for the same reason overlays do, and it
    /// serves one purpose more: a change to it is what tells an interface that
    /// the object list it pulled through `ephemerisObjects` is stale.
    ephemeris_revision: u64,
    ephemerides_enabled: bool,
    /// Overlays are a list rather than a handful of fields, so they keep a
    /// counter of their own: anything an interface has a control for bumps it,
    /// and the countdown to the next refresh — which changes every tick and has
    /// no control on it — deliberately does not.
    overlay_revision: u64,
}

fn digest(
    state: &GlobeState,
    overlay_revision: u64,
    ephemeris_revision: u64,
    picks: PickDigest,
    satellites: SatelliteDigest,
) -> Digest {
    let (hovered, pinned) = picks;
    let (hovered_satellite, pinned_satellite) = satellites;
    Digest {
        frame: state.frame.mode,
        sun_paused: state.sun.paused,
        sun_shaded: state.sun.shaded,
        imagery_enabled: state.imagery.enabled,
        layer_index: state.imagery.layer_index,
        vector_tiles_enabled: state.vector_tiles.enabled,
        vector_layer_index: state.vector_tiles.layer_index,
        vector_deepest_level: state.vector_tiles.deepest_level,
        vector_visible_tiles: state.vector_tiles.visible_tiles,
        vector_loading_tiles: state.vector_tiles.loading_tiles,
        hud_visible: state.hud.visible,
        help_visible: state.hud.help_visible,
        keyboard: state.keyboard,
        deepest_level: state.imagery.deepest_level,
        visible_tiles: state.imagery.visible_tiles,
        loading_tiles: state.imagery.loading_tiles,
        overlays_enabled: state.overlays.enabled,
        picking: state.overlays.picking,
        hovered,
        pinned,
        hovered_satellite,
        pinned_satellite,
        overlay_revision,
        ephemeris_revision,
        ephemerides_enabled: state.ephemerides.enabled,
    }
}

/// Publishing state, throttled.
#[derive(Resource)]
pub(crate) struct StateStream {
    since_publish: f32,
    last_digest: Option<Digest>,
}

impl Default for StateStream {
    fn default() -> Self {
        Self {
            // Publish on the first tick rather than a tenth of a second into it,
            // so an interface has something to draw as soon as the globe starts.
            since_publish: STATE_INTERVAL_SECONDS,
            last_digest: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Input gating
// ---------------------------------------------------------------------------

/// Which of the globe's own inputs are live.
#[derive(Resource, Debug, Clone)]
pub struct GlobeInput {
    /// The keyboard shortcuts. The pointer is always live: an embedder replaces
    /// the keys, not the dragging.
    pub keyboard: bool,
}

impl Default for GlobeInput {
    fn default() -> Self {
        Self { keyboard: true }
    }
}

/// Run condition for every system that reads a key binding.
pub fn keyboard_enabled(input: Res<GlobeInput>) -> bool {
    input.keyboard
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct ApiPlugin;

impl Plugin for ApiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GlobeInput>()
            .init_resource::<Cursor>()
            .init_resource::<LatestState>()
            .init_resource::<StateStream>()
            .add_systems(
                Update,
                apply_commands
                    .in_set(FrameSet::Settle)
                    // The Earth's rotation is settled from the clock first, so a
                    // frame switch measures against where the planet is this
                    // tick; and before the key bindings, so a command and a
                    // keypress on the same tick compose instead of racing.
                    .after(crate::frame::sync_earth_rotation)
                    .before(crate::frame::frame_controls),
            )
            // The cursor is cast onto the globe first, because both the
            // snapshot and the overlay hit test are built from it, and the
            // snapshot goes out last so that it carries what the hit test found
            // on this tick rather than on the last one.
            .add_systems(
                Update,
                (track_cursor, publish_state)
                    .chain()
                    .in_set(FrameSet::Apply),
            );
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "commands reach every controllable part of the globe, so applying them necessarily borrows all of them"
)]
fn apply_commands(
    mut camera: Query<&mut OrbitCamera>,
    mut frame: ResMut<ReferenceFrame>,
    mut realigned: MessageWriter<FrameRealigned>,
    mut sun: ResMut<Sun>,
    mut imagery: ResMut<ImagerySettings>,
    mut vector_tiles: ResMut<VectorTileSettings>,
    mut hud: ResMut<HudSettings>,
    mut overlays: ResMut<OverlaySettings>,
    mut ephemerides: ResMut<EphemerisSettings>,
    mut input: ResMut<GlobeInput>,
) {
    let commands = take_queued();
    if commands.is_empty() {
        return;
    }

    let mut camera = camera.iter_mut().next();

    for command in commands {
        match command {
            GlobeCommand::LookAt {
                coordinate,
                altitude_km,
            } => {
                if let Some(camera) = camera.as_mut() {
                    // The coordinate is Earth-fixed and the camera orbits in
                    // world space, so it has to be carried across the frame.
                    camera.look_at(frame.earth_to_world() * coordinate.to_direction());
                    if let Some(altitude_km) = altitude_km {
                        camera.set_altitude_km(altitude_km);
                    }
                }
            }
            GlobeCommand::SetAltitude(altitude_km) => {
                if let Some(camera) = camera.as_mut() {
                    camera.set_altitude_km(altitude_km);
                }
            }
            GlobeCommand::OrbitBy { yaw_deg, pitch_deg } => {
                if let Some(camera) = camera.as_mut() {
                    camera.orbit_by(-yaw_deg.to_radians(), pitch_deg.to_radians());
                }
            }
            GlobeCommand::ZoomBy(exponent) => {
                if let Some(camera) = camera.as_mut() {
                    camera.zoom_by(exponent);
                }
            }
            GlobeCommand::ResetView => {
                if let Some(camera) = camera.as_mut() {
                    camera.reset();
                }
            }

            GlobeCommand::SetFrame(mode) => set_frame(&mut frame, &mut realigned, mode),
            GlobeCommand::ToggleFrame => {
                let toggled = frame.mode.toggled();
                set_frame(&mut frame, &mut realigned, toggled);
            }

            GlobeCommand::SetSunPaused(paused) => sun.paused = paused,
            GlobeCommand::SetTimeScale(scale) => sun.set_time_scale(scale),
            GlobeCommand::SetClock(unix_seconds) => {
                sun.set_clock(unix_seconds);
                // The rotation for this tick was settled from the old clock.
                frame.sync_rotation(sun.unix_seconds);
            }
            GlobeCommand::SnapClockToNow => {
                sun.snap_to_now();
                frame.sync_rotation(sun.unix_seconds);
            }
            GlobeCommand::SetSunShaded(shaded) => sun.shaded = shaded,

            GlobeCommand::SetLayer(index) => imagery.select_preset(index),
            GlobeCommand::NextLayer => imagery.cycle_preset(),
            GlobeCommand::PreviousLayer => imagery.cycle_preset_back(),
            GlobeCommand::SetImageryEnabled(enabled) => imagery.enabled = enabled,

            GlobeCommand::SetVectorTilesEnabled(enabled) => vector_tiles.enabled = enabled,
            GlobeCommand::SetVectorTileLayer(index) => vector_tiles.select_preset(index),
            GlobeCommand::NextVectorTileLayer => vector_tiles.cycle_preset(),
            GlobeCommand::PreviousVectorTileLayer => vector_tiles.cycle_preset_back(),
            GlobeCommand::SetVectorTileStyle(style) => vector_tiles.set_style(style),

            GlobeCommand::AddOverlay(request) => overlays.add(request),
            // Naming an overlay that is not up is not an error. An interface
            // can send one for a layer the user has just removed, and the layer
            // being gone is the state it was asking for anyway.
            GlobeCommand::RemoveOverlay(id) => {
                overlays.remove(&id);
            }
            GlobeCommand::SetOverlayVisible { id, visible } => {
                overlays.set_visible(&id, visible);
            }
            GlobeCommand::SetOverlayStyle { id, style } => {
                overlays.set_style(&id, style);
            }
            GlobeCommand::SetOverlayAltitude { id, altitude } => {
                overlays.set_altitude(&id, altitude);
            }
            GlobeCommand::SetOverlayRefresh { id, seconds } => {
                overlays.set_refresh(&id, seconds);
            }
            GlobeCommand::RefreshOverlay(id) => {
                overlays.refresh(&id);
            }
            GlobeCommand::SetOverlaysEnabled(enabled) => overlays.enabled = enabled,

            GlobeCommand::AddEphemeris(request) => ephemerides.add(request),
            GlobeCommand::RemoveEphemeris(id) => {
                ephemerides.remove(&id);
            }
            GlobeCommand::SetEphemerisVisible { id, visible } => {
                ephemerides.set_visible(&id, visible);
            }
            GlobeCommand::SetEphemerisStyle { id, style } => {
                ephemerides.set_style(&id, style);
            }
            GlobeCommand::SetEphemerisSelection { id, selection } => {
                ephemerides.set_selection(&id, selection);
            }
            GlobeCommand::SelectSatellite {
                id,
                norad_id,
                selected,
            } => {
                ephemerides.select(&id, norad_id, selected);
            }
            GlobeCommand::SetSatelliteTrail {
                id,
                norad_id,
                trail,
            } => {
                ephemerides.set_object_trail(&id, norad_id, trail);
            }
            GlobeCommand::SetEphemerisTrails { id, trails } => {
                ephemerides.set_trails(&id, trails);
            }
            GlobeCommand::SetEphemerisTrail { id, trail } => {
                ephemerides.set_trail(&id, trail);
            }
            GlobeCommand::SetEphemerisRefresh { id, seconds } => {
                ephemerides.set_refresh(&id, seconds);
            }
            GlobeCommand::RefreshEphemeris(id) => {
                ephemerides.refresh(&id);
            }
            GlobeCommand::SetEphemeridesEnabled(enabled) => ephemerides.enabled = enabled,

            GlobeCommand::SetPickingEnabled(enabled) => {
                overlays.picking = enabled;
                ephemerides.picking = enabled;
            }
            GlobeCommand::PinFeature { layer, index } => {
                overlays.pin(&layer, index);
            }
            GlobeCommand::ClearPinnedFeature => overlays.clear_pin(),
            GlobeCommand::PinSatellite { layer, norad_id } => {
                ephemerides.pin(&layer, norad_id);
            }
            GlobeCommand::ClearPinnedSatellite => ephemerides.clear_pin(),

            GlobeCommand::SetHudVisible(visible) => hud.visible = visible,
            GlobeCommand::SetHelpVisible(visible) => hud.help_visible = visible,
            GlobeCommand::SetKeyboardEnabled(enabled) => input.keyboard = enabled,
        }
    }
}

/// Switches frames the same way the key does, so whatever is watching the
/// ground is carried across the jump.
fn set_frame(
    frame: &mut ReferenceFrame,
    realigned: &mut MessageWriter<FrameRealigned>,
    mode: FrameMode,
) {
    if frame.mode == mode {
        return;
    }
    let before = frame.earth_yaw();
    frame.mode = mode;
    realigned.write(FrameRealigned {
        ground_yaw: frame.earth_yaw() - before,
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "the snapshot spans every controllable part of the globe, so building it necessarily reads all of them"
)]
pub(crate) fn publish_state(
    time: Res<Time>,
    camera: Query<&OrbitCamera>,
    cursor: Res<Cursor>,
    frame: Res<ReferenceFrame>,
    sun: Res<Sun>,
    imagery: Res<ImagerySettings>,
    tiles: Res<TileCache>,
    vector_settings: Res<VectorTileSettings>,
    vector_cache: Res<VectorTileCache>,
    overlays: Res<OverlaySettings>,
    ephemerides: Res<EphemerisSettings>,
    hud: Res<HudSettings>,
    input: Res<GlobeInput>,
    mut stream: ResMut<StateStream>,
    mut latest: ResMut<LatestState>,
) {
    let Some(orbit) = camera.iter().next() else {
        return;
    };

    let state = GlobeState {
        camera: CameraState {
            center: LatLon::from_direction(
                frame.world_to_earth() * orbit.world_center().to_direction(),
            ),
            altitude_km: orbit.altitude_km(),
        },
        frame: FrameState {
            mode: frame.mode.id(),
            label: frame.mode.label(),
        },
        sun: SunState {
            paused: sun.paused,
            shaded: sun.shaded,
            time_scale: sun.time_scale,
            unix_seconds: sun.unix_seconds,
            utc: sun.format_utc(),
            subsolar: sun.subsolar,
        },
        imagery: ImageryState {
            enabled: imagery.enabled,
            layer_index: imagery.preset_index,
            label: imagery.label(),
            protocol: imagery.protocol(),
            max_level: imagery.max_level(),
            deepest_level: tiles.deepest_level,
            visible_tiles: tiles.visible_tiles,
            loading_tiles: tiles.loading_tiles,
        },
        vector_tiles: vector_tiles::describe(&vector_settings, &vector_cache),
        overlays: OverlaysState {
            enabled: overlays.enabled,
            drawn: overlays.drawn(),
            layers: overlays::describe(&overlays),
            picking: overlays.picking,
            hovered: overlays::hovered(&overlays),
            pinned: overlays::pinned(&overlays),
        },
        ephemerides: ephemeris::describe(&ephemerides, sun.unix_seconds),
        hud: HudState {
            visible: hud.visible,
            help_visible: hud.help_visible,
        },
        cursor: cursor.ground,
        keyboard: input.keyboard,
    };

    stream.since_publish += time.delta_secs();
    let current = digest(
        &state,
        overlays.revision(),
        ephemerides.revision(),
        overlays::pick_digest(&overlays),
        ephemeris::pick_digest(&ephemerides),
    );
    let changed = stream.last_digest != Some(current);
    let due = stream.since_publish >= STATE_INTERVAL_SECONDS;

    // The HUD reads this every tick, so it is kept current even between
    // publishes — the throttle is on what leaves the app, not on what it knows.
    latest.0 = Some(state);

    if changed || due {
        stream.since_publish = 0.0;
        stream.last_digest = Some(current);
        #[cfg(target_arch = "wasm32")]
        if let Some(state) = latest.0.as_ref() {
            crate::wasm::publish(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_commands_are_taken_once() {
        send(GlobeCommand::ResetView);
        send(GlobeCommand::NextLayer);
        assert_eq!(
            take_queued(),
            vec![GlobeCommand::ResetView, GlobeCommand::NextLayer]
        );
        assert!(take_queued().is_empty());
    }

    #[test]
    fn frames_are_named_both_ways() {
        for mode in [FrameMode::Ecef, FrameMode::Eci] {
            assert_eq!(FrameMode::from_id(mode.id()), Some(mode));
        }
        assert_eq!(FrameMode::from_id("geodetic"), None);
    }
}
