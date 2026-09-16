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
use crate::frame::{FrameMode, FrameRealigned, FrameSet, ReferenceFrame};
use crate::geo::{LatLon, ray_sphere_intersection};
use crate::globe::GLOBE_RADIUS;
use crate::hud::HudSettings;
use crate::imagery::{ImageryLayer, ImagerySettings};
use crate::sun::{self, Sun};
use crate::tiles::TileCache;

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
}

impl Limits {
    pub fn current() -> Self {
        let (min_altitude_km, max_altitude_km) = OrbitCamera::altitude_limits_km();
        Self {
            min_altitude_km,
            max_altitude_km,
            min_time_scale: sun::MIN_TIME_SCALE,
            max_time_scale: sun::MAX_TIME_SCALE,
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

/// The discrete part of the state — the fields a control flips rather than the
/// ones that drift every frame. A change here publishes immediately.
type Digest = (
    &'static str,
    bool,
    bool,
    bool,
    usize,
    bool,
    bool,
    bool,
    u8,
    usize,
    usize,
);

fn digest(state: &GlobeState) -> Digest {
    (
        state.frame.mode,
        state.sun.paused,
        state.sun.shaded,
        state.imagery.enabled,
        state.imagery.layer_index,
        state.hud.visible,
        state.hud.help_visible,
        state.keyboard,
        state.imagery.deepest_level,
        state.imagery.visible_tiles,
        state.imagery.loading_tiles,
    )
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
            .add_systems(Update, publish_state.in_set(FrameSet::Apply));
    }
}

fn apply_commands(
    mut camera: Query<&mut OrbitCamera>,
    mut frame: ResMut<ReferenceFrame>,
    mut realigned: MessageWriter<FrameRealigned>,
    mut sun: ResMut<Sun>,
    mut imagery: ResMut<ImagerySettings>,
    mut hud: ResMut<HudSettings>,
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
    camera: Query<(&Camera, &GlobalTransform, &OrbitCamera)>,
    windows: Query<&Window>,
    frame: Res<ReferenceFrame>,
    sun: Res<Sun>,
    imagery: Res<ImagerySettings>,
    tiles: Res<TileCache>,
    hud: Res<HudSettings>,
    input: Res<GlobeInput>,
    mut stream: ResMut<StateStream>,
    mut latest: ResMut<LatestState>,
) {
    let Some((camera, camera_transform, orbit)) = camera.iter().next() else {
        return;
    };

    let cursor = windows
        .iter()
        .find_map(|window| window.cursor_position())
        .and_then(|cursor| camera.viewport_to_world(camera_transform, cursor).ok())
        .and_then(|ray| ray_sphere_intersection(ray.origin, *ray.direction, GLOBE_RADIUS))
        // The hit is in world space; the coordinate under it is Earth-fixed.
        .map(|hit| LatLon::from_direction(frame.world_to_earth() * hit));

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
        hud: HudState {
            visible: hud.visible,
            help_visible: hud.help_visible,
        },
        cursor,
        keyboard: input.keyboard,
    };

    stream.since_publish += time.delta_secs();
    let current = digest(&state);
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
