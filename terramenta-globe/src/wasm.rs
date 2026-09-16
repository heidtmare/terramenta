//! The JavaScript binding: [`crate::api`] as a module an embedder can import.
//!
//! Everything here is a thin adapter. Commands go in as plain arguments and are
//! queued rather than applied, so calling one before the globe has started is
//! fine — it lands on the first tick. State comes back out as ordinary
//! JavaScript objects through a callback, which is the only channel that has to
//! cross in that direction.
//!
//! The shape of those objects is `crate::api::GlobeState`; the module's own
//! documentation is `terramenta-webapp/src/globe.js`, which wraps every one of
//! these calls.

use wasm_bindgen::prelude::*;

use crate::GlobeConfig;
use crate::api::{self, GlobeCommand, GlobeState, Limits};
use crate::frame::FrameMode;
use crate::geo::LatLon;

/// Starts the globe on a canvas, by CSS selector, loading its assets from a
/// path relative to the page.
///
/// Either argument may be `null` for its default — `"#terramenta"` and
/// `"assets"`. This does not return in the usual sense: winit hands control to
/// the browser's event loop by unwinding through an exception, which the caller
/// is expected to catch and ignore. `terramenta-webapp/src/globe.js` shows what
/// that looks like.
#[wasm_bindgen]
pub fn start(canvas_selector: Option<String>, asset_path: Option<String>) {
    let defaults = GlobeConfig::default();
    crate::app(GlobeConfig {
        canvas_selector: canvas_selector.unwrap_or(defaults.canvas_selector),
        asset_path: asset_path.unwrap_or(defaults.asset_path),
    })
    .run();
}

// ---------------------------------------------------------------------------
// Catalogue
// ---------------------------------------------------------------------------

/// The imagery presets the globe offers, in the order they cycle.
///
/// Available before [`start`], so an interface can be built before the globe is.
#[wasm_bindgen]
pub fn layers() -> JsValue {
    to_js(&crate::layers())
}

/// The ranges the controls accept, for an interface building its own inputs.
#[wasm_bindgen]
pub fn limits() -> JsValue {
    to_js(&Limits::current())
}

// ---------------------------------------------------------------------------
// Camera
// ---------------------------------------------------------------------------

/// Looks straight down at a coordinate, optionally from a given height in
/// kilometres.
#[wasm_bindgen(js_name = lookAt)]
pub fn look_at(lat: f32, lon: f32, altitude_km: Option<f32>) {
    api::send(GlobeCommand::LookAt {
        coordinate: LatLon::new(lat, lon),
        altitude_km,
    });
}

#[wasm_bindgen(js_name = setAltitude)]
pub fn set_altitude(altitude_km: f32) {
    api::send(GlobeCommand::SetAltitude(altitude_km));
}

/// Turns the view by an angle in degrees, the way a drag would.
#[wasm_bindgen(js_name = orbitBy)]
pub fn orbit_by(yaw_deg: f32, pitch_deg: f32) {
    api::send(GlobeCommand::OrbitBy { yaw_deg, pitch_deg });
}

/// Zooms by an exponent: positive closes in, negative pulls back.
#[wasm_bindgen(js_name = zoomBy)]
pub fn zoom_by(exponent: f32) {
    api::send(GlobeCommand::ZoomBy(exponent));
}

#[wasm_bindgen(js_name = resetView)]
pub fn reset_view() {
    api::send(GlobeCommand::ResetView);
}

// ---------------------------------------------------------------------------
// Reference frame
// ---------------------------------------------------------------------------

/// Draws the scene in `"ecef"` or `"eci"`. An unknown name is ignored.
#[wasm_bindgen(js_name = setFrame)]
pub fn set_frame(mode: &str) {
    if let Some(mode) = FrameMode::from_id(mode) {
        api::send(GlobeCommand::SetFrame(mode));
    }
}

#[wasm_bindgen(js_name = toggleFrame)]
pub fn toggle_frame() {
    api::send(GlobeCommand::ToggleFrame);
}

// ---------------------------------------------------------------------------
// Sun and clock
// ---------------------------------------------------------------------------

#[wasm_bindgen(js_name = setSunPaused)]
pub fn set_sun_paused(paused: bool) {
    api::send(GlobeCommand::SetSunPaused(paused));
}

/// Simulated seconds per real second.
#[wasm_bindgen(js_name = setTimeScale)]
pub fn set_time_scale(scale: f32) {
    api::send(GlobeCommand::SetTimeScale(scale));
}

/// Jumps the simulated clock to a moment, in seconds since the Unix epoch —
/// which is `Date.now() / 1000`, not milliseconds.
#[wasm_bindgen(js_name = setClock)]
pub fn set_clock(unix_seconds: f64) {
    api::send(GlobeCommand::SetClock(unix_seconds));
}

#[wasm_bindgen(js_name = snapClockToNow)]
pub fn snap_clock_to_now() {
    api::send(GlobeCommand::SnapClockToNow);
}

/// Whether the sun lights the globe. Off, there is no terminator: every face is
/// lit as though the sun were overhead, which is how imagery of somewhere in
/// darkness is read.
#[wasm_bindgen(js_name = setSunShaded)]
pub fn set_sun_shaded(shaded: bool) {
    api::send(GlobeCommand::SetSunShaded(shaded));
}

// ---------------------------------------------------------------------------
// Imagery
// ---------------------------------------------------------------------------

/// Selects a preset by its index in [`layers`], wrapping past the end.
#[wasm_bindgen(js_name = setLayer)]
pub fn set_layer(index: usize) {
    api::send(GlobeCommand::SetLayer(index));
}

#[wasm_bindgen(js_name = nextLayer)]
pub fn next_layer() {
    api::send(GlobeCommand::NextLayer);
}

#[wasm_bindgen(js_name = previousLayer)]
pub fn previous_layer() {
    api::send(GlobeCommand::PreviousLayer);
}

/// Whether imagery tiles are streamed at all. Off, the globe falls back to its
/// built-in base texture.
#[wasm_bindgen(js_name = setImageryEnabled)]
pub fn set_imagery_enabled(enabled: bool) {
    api::send(GlobeCommand::SetImageryEnabled(enabled));
}

// ---------------------------------------------------------------------------
// Chrome and input
// ---------------------------------------------------------------------------

/// Whether the globe draws its own readout. An interface that draws its own
/// turns this off.
#[wasm_bindgen(js_name = setHudVisible)]
pub fn set_hud_visible(visible: bool) {
    api::send(GlobeCommand::SetHudVisible(visible));
}

/// Whether the globe draws its key list.
#[wasm_bindgen(js_name = setHelpVisible)]
pub fn set_help_visible(visible: bool) {
    api::send(GlobeCommand::SetHelpVisible(visible));
}

/// Whether the globe's own keyboard shortcuts are live. An embedder that binds
/// its own keys turns these off so the two do not both fire.
#[wasm_bindgen(js_name = setKeyboardEnabled)]
pub fn set_keyboard_enabled(enabled: bool) {
    api::send(GlobeCommand::SetKeyboardEnabled(enabled));
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

// The listener the globe pushes state to. A `thread_local` rather than a
// `static`: a `js_sys::Function` is neither `Send` nor `Sync`, and on this
// target there is only ever the one thread it could be called from anyway.
thread_local! {
    static LISTENER: std::cell::RefCell<Option<js_sys::Function>> =
        const { std::cell::RefCell::new(None) };
}

/// Registers the callback the globe reports its state to.
///
/// It fires about ten times a second, and immediately whenever something an
/// interface has a control for changes, so a toggle answers on the next frame
/// rather than on the next tick of the throttle. Only one listener is kept;
/// registering again replaces it.
#[wasm_bindgen(js_name = onState)]
pub fn on_state(callback: Option<js_sys::Function>) {
    LISTENER.with(|listener| *listener.borrow_mut() = callback);
}

/// Hands a snapshot to the registered listener, if there is one.
pub(crate) fn publish(state: &GlobeState) {
    LISTENER.with(|listener| {
        if let Some(callback) = listener.borrow().as_ref() {
            // A listener that throws is the embedder's problem, not the globe's:
            // swallowing it here keeps one bad frame from stopping the stream.
            let _ = callback.call1(&JsValue::NULL, &to_js(state));
        }
    });
}

/// Serializes through JSON, which is the one representation `serde` and
/// JavaScript both already agree on without another dependency.
fn to_js<T: serde::Serialize>(value: &T) -> JsValue {
    serde_json::to_string(value)
        .ok()
        .and_then(|json| js_sys::JSON::parse(&json).ok())
        .unwrap_or(JsValue::NULL)
}
