/**
 * The globe, as the rest of the app sees it.
 *
 * `terramenta-globe` compiles to a flat module of functions — `setLayer`,
 * `lookAt`, `onState` — and this is the one place that talks to it. Everything
 * above imports `globe` and never the wasm module, so the boundary between the
 * two stays a single file: what the globe can do is exactly what is listed
 * here, and if a call changes shape, it changes here.
 *
 * Two things are worth knowing about the module underneath.
 *
 * Commands are queued, not applied. Calling one before `start()` is fine; it
 * lands on the globe's first frame. So the interface can be built and wired up
 * before the renderer exists, which is what lets the panel render from
 * `layers()` while the wasm is still downloading.
 *
 * State arrives on a callback, not by asking. The globe pushes a snapshot about
 * ten times a second, and immediately whenever something with a control on it
 * changes. Nothing here polls.
 */

/** Where `scripts/build.sh` puts the module and its assets. */
const MODULE_URL = "../globe/terramenta_globe.js";
const ASSET_PATH = "globe/assets";

/** Resolved once `load()` has run. */
let wasm = null;

/**
 * Winit hands control to the browser's event loop by unwinding through an
 * exception. It reaches us as a throw with this in the message, and it means
 * the globe started, not that it failed.
 */
const CONTROL_FLOW_UNWIND = "Using exceptions for control flow";

/** Whether this browser can render the globe at all. */
export function isSupported() {
  return Boolean(navigator.gpu);
}

/**
 * Downloads and instantiates the module, without starting the globe.
 *
 * Split from `start()` because the catalogue is worth having early: `layers()`
 * and `limits()` answer as soon as this resolves, so the panel can be built
 * while the renderer is still warming up.
 */
export async function load() {
  if (!wasm) {
    const module = await import(MODULE_URL);
    await module.default();
    wasm = module;
  }
  return wasm;
}

/**
 * Starts rendering on a canvas.
 *
 * Resolves once the globe has taken the event loop, which it signals by
 * throwing; any other error is real and is rethrown.
 */
export async function start(canvasSelector) {
  const module = await load();
  try {
    module.start(canvasSelector, ASSET_PATH);
  } catch (error) {
    if (!`${error}`.includes(CONTROL_FLOW_UNWIND)) throw error;
  }
}

function required() {
  if (!wasm) throw new Error("The globe has not been loaded yet — await load() first.");
  return wasm;
}

// --- Catalogue -------------------------------------------------------------
// Both answer from compiled-in tables, so they work before `start()`.

/** The imagery presets, in the order they cycle: `{index, label, protocol, maxLevel, tileSize, format}`. */
export const layers = () => required().layers();

/** The ranges the controls accept: `{minAltitudeKm, maxAltitudeKm, minTimeScale, maxTimeScale}`. */
export const limits = () => required().limits();

// --- Camera ----------------------------------------------------------------

/**
 * Looks straight down at a coordinate, optionally from a height in kilometres.
 *
 * The coordinate is Earth-fixed. In ECI the ground keeps turning underneath
 * afterwards, so this points at a place rather than following it.
 */
export const lookAt = (lat, lon, altitudeKm) => required().lookAt(lat, lon, altitudeKm);

/** Height above the surface, in kilometres. */
export const setAltitude = (altitudeKm) => required().setAltitude(altitudeKm);

/** Turns the view by an angle in degrees, the way a drag would. */
export const orbitBy = (yawDeg, pitchDeg) => required().orbitBy(yawDeg, pitchDeg);

/** Zooms by an exponent: positive closes in, negative pulls back. */
export const zoomBy = (exponent) => required().zoomBy(exponent);

export const resetView = () => required().resetView();

// --- Reference frame -------------------------------------------------------

/** `"ecef"` holds the ground still; `"eci"` holds the stars still. */
export const setFrame = (mode) => required().setFrame(mode);

export const toggleFrame = () => required().toggleFrame();

// --- Sun and clock ---------------------------------------------------------

export const setSunPaused = (paused) => required().setSunPaused(paused);

/** Simulated seconds per real second. */
export const setTimeScale = (scale) => required().setTimeScale(scale);

/** Jumps the simulated clock. Seconds since the Unix epoch, so `Date.now() / 1000`. */
export const setClock = (unixSeconds) => required().setClock(unixSeconds);

export const snapClockToNow = () => required().snapClockToNow();

/**
 * Whether the sun lights the globe.
 *
 * Off, there is no terminator and no night side — every face is lit as though
 * the sun were overhead, which is how imagery of somewhere in darkness is read.
 */
export const setSunShaded = (shaded) => required().setSunShaded(shaded);

// --- Imagery ---------------------------------------------------------------

/** Selects a preset by its index in `layers()`, wrapping past the end. */
export const setLayer = (index) => required().setLayer(index);

export const nextLayer = () => required().nextLayer();

export const previousLayer = () => required().previousLayer();

/** Whether tiles are streamed at all. Off, the globe falls back to its base texture. */
export const setImageryEnabled = (enabled) => required().setImageryEnabled(enabled);

// --- GeoJSON overlays ------------------------------------------------------

/**
 * Puts a GeoJSON overlay up, or replaces the one already under this id.
 *
 * Exactly one of `url` and `text` says where the data comes from. A URL is the
 * globe's to fetch, and the only kind it can refetch; `text` is for GeoJSON
 * this app already has — a file the user picked, most of all — and re-sending
 * it under the same id is what an update looks like.
 *
 * ```js
 * addOverlay("quakes", {
 *   url: "https://earthquake.usgs.gov/.../all_hour.geojson",
 *   label: "Earthquakes, past hour",
 *   refreshSeconds: 60,
 *   pointColor: "#ff9e3d",
 * });
 * addOverlay("local", { text: await file.text() });
 * ```
 *
 * Options: `label`, `refreshSeconds`, `visible`, and the style fields
 * `pointColor`, `pointSizePx`, `lineColor`, `lineWidthPx`, `fillColor` —
 * colours as hex, with an optional alpha pair, exactly as CSS writes them.
 *
 * Returns whether the options made sense. A layer that fails to *load* still
 * returns `true`: that failure arrives on the state stream, with its reason,
 * long after the call is over.
 */
export const addOverlay = (id, options) => required().addOverlay(id, options);

export const removeOverlay = (id) => required().removeOverlay(id);

export const setOverlayVisible = (id, visible) => required().setOverlayVisible(id, visible);

/** Restyles a layer without refetching it. Anything left out returns to its default. */
export const setOverlayStyle = (id, style) => required().setOverlayStyle(id, style);

/**
 * Seconds between refetches, or `null` to stop refreshing.
 *
 * Only a layer the globe fetched can refresh. One given as text has nowhere to
 * fetch from and reports `refreshSeconds: null` whatever is asked here — see
 * `overlays.js` for what this app does about that.
 */
export const setOverlayRefresh = (id, seconds) => required().setOverlayRefresh(id, seconds);

/** Refetches now, whatever the period says. */
export const refreshOverlay = (id) => required().refreshOverlay(id);

/** Whether overlays are drawn at all. Off, every layer stays loaded. */
export const setOverlaysEnabled = (enabled) => required().setOverlaysEnabled(enabled);

// --- Picking ---------------------------------------------------------------

/**
 * Whether the cursor picks features.
 *
 * On, every snapshot carries `overlays.hovered` — the feature under the
 * pointer, with its properties — and the globe draws a halo around it.
 */
export const setPickingEnabled = (enabled) => required().setPickingEnabled(enabled);

/**
 * Keeps a feature selected, whatever the cursor does afterwards.
 *
 * This is what a click is made of. The globe reports what is under the pointer;
 * deciding that one of those is *the* selection is this app's job, and
 * `feature.js` is where it decides it.
 */
export const pinFeature = (layer, index) => required().pinFeature(layer, index);

export const clearPinnedFeature = () => required().clearPinnedFeature();

// --- The globe's own chrome ------------------------------------------------

/** Whether the globe draws its built-in readout. This app draws its own instead. */
export const setHudVisible = (visible) => required().setHudVisible(visible);

/** Whether the globe draws its built-in key list. */
export const setHelpVisible = (visible) => required().setHelpVisible(visible);

/**
 * Whether the globe's own keyboard shortcuts are live.
 *
 * This app leaves them on: they are part of what it is demonstrating, and the
 * state stream means the panel stays in step with whatever they do. An embedder
 * binding its own keys would turn them off so the two do not both fire.
 */
export const setKeyboardEnabled = (enabled) => required().setKeyboardEnabled(enabled);

// --- State -----------------------------------------------------------------

/**
 * Registers the callback the globe reports its state to.
 *
 * The snapshot is `{camera, frame, sun, imagery, overlays, hud, cursor,
 * keyboard}`; see
 * `readout.js` and `panel.js` for what is in each. Only one listener is kept,
 * so this app fans it out itself rather than registering twice.
 */
export const onState = (callback) => required().onState(callback);
