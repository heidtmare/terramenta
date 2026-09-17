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

/** The vector tile presets, in the order they cycle: `{index, label, maxLevel, sourceLayers}`. */
export const vectorLayers = () => required().vectorLayers();

/**
 * The ranges the controls accept: `{minAltitudeKm, maxAltitudeKm, minTimeScale,
 * maxTimeScale, maxVectorTileLatitude}`.
 */
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

// --- Vector tiles ----------------------------------------------------------

/**
 * Whether Mapbox Vector Tiles are streamed at all.
 *
 * Off, every tile is dropped rather than hidden — a vector tile is cheap to ask
 * for again and its meshes are not cheap to keep — so switching back re-walks
 * the view and refetches what it needs.
 */
export const setVectorTilesEnabled = (enabled) => required().setVectorTilesEnabled(enabled);

/** Selects a preset by its index in `vectorLayers()`, wrapping past the end. */
export const setVectorTileLayer = (index) => required().setVectorTileLayer(index);

export const nextVectorTileLayer = () => required().nextVectorTileLayer();

export const previousVectorTileLayer = () => required().previousVectorTileLayer();

/**
 * Recolours the vector tile layer. Takes the same style fields an overlay does:
 * `pointColor`, `pointSizePx`, `lineColor`, `lineWidthPx`, `fillColor`.
 *
 * Unlike an overlay this rebuilds the tiles on screen rather than swapping a
 * colour on them — a tile's meshes are keyed to the style they were built with,
 * and a layer whose fill was transparent never built a fill at all. Anything
 * left out returns to its default, so send the whole style each time.
 */
export const setVectorTileStyle = (style) => required().setVectorTileStyle(style);

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
 * How a layer reads the third element of its positions: `altitudeMode` of
 * `"relativeToSurface"` or `"clampToSurface"`, `altitudeScale` as metres per
 * unit, and `extrude` to wall its polygons down to the ground. The layer is
 * rebuilt where it stands, without refetching.
 */
export const setOverlayAltitude = (id, altitude) => required().setOverlayAltitude(id, altitude);

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

// --- Ephemerides -----------------------------------------------------------

/**
 * Puts a satellite layer up, or replaces the one already under this id.
 *
 * The document is [OMM](https://public.ccsds.org/Pubs/502x0b3e1.pdf) JSON — an
 * array of orbit mean-element records, which is what every current catalogue
 * publishes and the successor to the two-line element set. The globe turns each
 * into an SGP4 propagator and evaluates them against its own simulated clock,
 * so the satellites obey `setTimeScale`, `setSunPaused` and `setClock` like
 * everything else on the globe.
 *
 * ```js
 * addEphemeris("stations", {
 *   url: "https://celestrak.org/NORAD/elements/gp.php?GROUP=stations&FORMAT=json",
 *   label: "Crewed stations",
 * });
 * addEphemeris("iss", { url, select: [25544], leadingOrbits: 1, trailingOrbits: 1 });
 * ```
 *
 * Options: `label`, `refreshSeconds` (floored at five minutes), `visible`,
 * `select` (catalogue numbers, or omit for everything the document holds),
 * `trails`, the trail window `leadingOrbits`, `trailingOrbits` and
 * `trailSamples`, and the same style fields an overlay takes — `fillColor`
 * excepted, because an ephemeris has no rings in it.
 *
 * Switch to the ECI frame to see an orbit as the closed ellipse it is; in ECEF
 * the same arc is the corkscrew a ground track is, because the Earth turns
 * underneath it.
 */
export const addEphemeris = (id, options) => required().addEphemeris(id, options);

export const removeEphemeris = (id) => required().removeEphemeris(id);

export const setEphemerisVisible = (id, visible) => required().setEphemerisVisible(id, visible);

/** Recolours a layer without repropagating it. Anything left out returns to its default. */
export const setEphemerisStyle = (id, style) => required().setEphemerisStyle(id, style);

/**
 * Replaces which objects the layer draws, by catalogue number. `null` is
 * everything the document holds, down to the budget the state stream reports as
 * `ephemerides.maxTracked`.
 */
export const setEphemerisSelection = (id, noradIds) =>
  required().setEphemerisSelection(id, noradIds ?? undefined);

/** Draws one object, or stops drawing it. */
export const selectSatellite = (id, noradId, selected) =>
  required().selectSatellite(id, noradId, selected);

/** Draws one object's orbit arc, or stops drawing it. */
export const setSatelliteTrail = (id, noradId, trail) =>
  required().setSatelliteTrail(id, noradId, trail);

/** Whether the layer draws arcs at all. Off, the arcs it has are hidden, not discarded. */
export const setEphemerisTrails = (id, trails) => required().setEphemerisTrails(id, trails);

/**
 * How far the arcs run either side of now, and how finely:
 * `{leadingOrbits, trailingOrbits, trailSamples, trailPath}`. The window is in
 * orbits rather than minutes, so half an orbit is half an orbit for the station
 * at ninety minutes and for a navigation satellite at twelve hours alike.
 *
 * `trailPath` is why an arc changes shape when the frame is switched.
 * `"track"` draws where the satellite passed over the ground, each sample
 * placed against the rotation at its own moment — a corkscrew in ECEF, and the
 * figure of eight a navigation constellation is usually drawn as. `"orbit"`
 * draws the path itself, every sample against the current rotation, which is
 * the same curve in both frames and does not move when they are switched.
 */
export const setEphemerisTrail = (id, trail) => required().setEphemerisTrail(id, trail);

/** Seconds between refetches, or `null` to stop. Floored at five minutes. */
export const setEphemerisRefresh = (id, seconds) => required().setEphemerisRefresh(id, seconds);

export const refreshEphemeris = (id) => required().refreshEphemeris(id);

/**
 * Whether ephemerides are drawn at all.
 *
 * Off, every layer stays loaded and stops being propagated — which, unlike an
 * overlay, is where the whole cost of one goes: an ephemeris is recomputed
 * every frame rather than parsed once.
 */
export const setEphemeridesEnabled = (enabled) => required().setEphemeridesEnabled(enabled);

/**
 * What one layer holds: `[{noradId, name, internationalDesignator,
 * epochUnixSeconds, periodMinutes, inclinationDeg, eccentricity, selected,
 * trail}, ...]`, or `null` for a layer that is not up or has not loaded.
 *
 * Pulled rather than streamed, because a catalogue can be twelve thousand rows
 * and the snapshot goes out ten times a second. The snapshot carries the
 * layer's `revision` instead, which changes whenever this list would — so an
 * interface pulls again when it does, and not otherwise. `ephemeris.js` is
 * where this app does that.
 *
 * `epochUnixSeconds` is on the same clock as `sun.unixSeconds`: subtract the
 * two for how stale the elements are, which is how much to trust the dot.
 */
export const ephemerisObjects = (id) => required().ephemerisObjects(id);

/**
 * A satellite layer's drawn geometry as typed arrays viewing the module's own
 * memory, exactly as `overlayGeometry` hands out an overlay's:
 * `{points: {coords, features}, lines: {coords, offsets, features}}`.
 *
 * The coordinates are in whichever frame the scene is drawn in — in ECI the
 * second component is a right ascension rather than a longitude — and the
 * height is metres above a sphere of mean Earth radius. Each feature's id is
 * the object's catalogue number, so `ephemerisObjects` is what gives a
 * coordinate here a name.
 *
 * The same rule as `overlayGeometry`, and harder: these buffers are rebuilt
 * every frame, so read them synchronously and `.slice()` anything worth keeping.
 */
export const ephemerisGeometry = (id) => required().ephemerisGeometry(id);

// --- Geometry --------------------------------------------------------------

/**
 * A layer's geometry as typed arrays viewing the module's own memory — the
 * buffers the globe is drawing from, in [GeoArrow](https://geoarrow.org)
 * layout, not a copy of them.
 *
 *     const g = globe.overlayGeometry("quakes");
 *     // The first point of the layer, in full f64 precision.
 *     const [lon, lat, height] = g.points.coords.subarray(0, 3);
 *     // Which feature it belongs to — the index `pinFeature` takes.
 *     const feature = g.points.features[0];
 *
 * `coords` is `longitude, latitude, height` repeated: degrees on WGS 84 and
 * metres above the surface, exactly as the source wrote them. `offsets` say
 * where each shape starts and ends *in coordinates*, so line `i` spans
 * `offsets[i]` to `offsets[i + 1]`; a polygon's `offsets` index into
 * `ringOffsets`, which index into `coords`, outer ring first. Rings are open —
 * the repeated closing position is gone, so a ring's last edge is the one back
 * to its first point.
 *
 * **The arrays are windows onto live memory.** Anything that allocates inside
 * the module detaches them, and refreshing or removing the layer frees what
 * they point at. So read them synchronously, and to keep the data — or to post
 * it to a worker — copy it first with `.slice()`, which returns an ordinary
 * array that owns its bytes.
 *
 * Returns `null` for a layer that is not up, or has not loaded yet.
 */
export const overlayGeometry = (id) => required().overlayGeometry(id);

// --- Picking ---------------------------------------------------------------

/**
 * Whether the cursor picks anything.
 *
 * On, every snapshot carries `overlays.hovered` — the feature under the
 * pointer, with its properties — and `ephemerides.hovered`, the satellite under
 * it, with its elements and where it is now. The globe haloes each.
 *
 * One switch for both, because they are one thing to whoever is pointing at the
 * globe. Two hit tests, though: a feature is picked where it stands on the
 * ground, and a satellite where its marker was drawn, hundreds of kilometres
 * above it.
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

/**
 * The same, for a satellite: `layer` is an ephemeris layer's id and `noradId` a
 * catalogue number, both as `ephemerides.hovered` reports them.
 *
 * By catalogue number rather than by position, like every other satellite
 * control, so a pin holds when the layer refetches and the document is
 * renumbered under it.
 */
export const pinSatellite = (layer, noradId) => required().pinSatellite(layer, noradId);

export const clearPinnedSatellite = () => required().clearPinnedSatellite();

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
 * The snapshot is `{camera, frame, sun, imagery, vectorTiles, overlays,
 * ephemerides, hud, cursor, keyboard}`; see
 * `readout.js` and `panel.js` for what is in each. Only one listener is kept,
 * so this app fans it out itself rather than registering twice.
 */
export const onState = (callback) => required().onState(callback);
