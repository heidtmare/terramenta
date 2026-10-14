/**
 * Full panel: six tabs — Project, Layers (imagery, vector tiles, overlays,
 * satellites), Spacecraft, Timeline, Analysis (plots, reports, events, coverage, porkchop,
 * launch) and View (clock, frame, camera, globe chrome).
 *
 * One-way data flow: controls send commands; `sync` sets every control from
 * the next state snapshot, so the panel and keyboard shortcuts never drift.
 * Held sliders are skipped until released. Hidden panes stay mounted and
 * bound.
 */

import * as globe from "./globe.js";
import { el, row, section, tabs } from "./dom.js";
import { mountEphemeris } from "./ephemeris.js";
import { mountCoverage } from "./coverage-panel.js";
import { mountEvents } from "./events-panel.js";
import { altitude, timeScale } from "./format.js";
import { mountMission } from "./mission.js";
import { mountOverlays } from "./overlays.js";
import { PLACES } from "./places.js";
import { mountProjectSection } from "./project-panel.js";
import { mountReports } from "./reports-panel.js";
import { mountSpacecraft } from "./spacecraft-panel.js";
import { mountTimeline } from "./timeline-panel.js";
import { button, choice, slider, toggle } from "./widgets.js";

/** `tab`: the tab opened first; `signal` aborts when the project closes. */
export function mountPanel(root, { tab, signal }) {
  const limits = globe.limits();
  const layers = globe.layers();
  const vectorTileLayers = globe.vectorLayers();
  const controls = [];

  /**
   * Registers a control, how a snapshot sets it, and — for the ones that can be
   * dragged — when it is being held and should be left alone.
   */
  const bind = (node, sync, isHeld = () => false) => {
    controls.push({ sync, isHeld });
    return node;
  };

  /** The last snapshot, for the controls that act relative to where things are. */
  let lastState = null;

  /**
   * Jumps the clock relative to where it is. The globe's own command takes an
   * absolute moment, so the offset is applied against the last snapshot.
   */
  const shiftClock = (seconds) => {
    if (lastState) globe.setClock(lastState.sun.unixSeconds + seconds);
  };

  // --- Imagery -------------------------------------------------------------

  const layerSelect = el(
    "select",
    { class: "select", onchange: (event) => globe.setLayer(Number(event.target.value)) },
    ...groupLayersByProtocol(layers),
  );
  bind(layerSelect, (state) => {
    layerSelect.value = String(state.imagery.layerIndex);
  });

  const imageryToggle = toggle("Stream tiles", globe.setImageryEnabled);
  bind(imageryToggle.node, (state) => imageryToggle.set(state.imagery.enabled));

  const layerDetail = el("p", { class: "detail" }, "—");
  bind(layerDetail, (state) => {
    const layer = layers[state.imagery.layerIndex];
    layerDetail.textContent = `${layer.protocol} · ${layer.tileSize}px ${layer.format} · to level ${layer.maxLevel}`;
  });

  const imagery = section(
    "Imagery",
    row("Layer", layerSelect),
    layerDetail,
    el(
      "div",
      { class: "buttons" },
      button("Previous", globe.previousLayer),
      button("Next", globe.nextLayer),
    ),
    imageryToggle.node,
  );

  // --- Vector tiles --------------------------------------------------------

  const vectorSelect = el(
    "select",
    { class: "select", onchange: (event) => globe.setVectorTileLayer(Number(event.target.value)) },
    ...vectorTileLayers.map((layer) => el("option", { value: String(layer.index) }, layer.label)),
  );
  bind(vectorSelect, (state) => {
    vectorSelect.value = String(state.vectorTiles.layerIndex);
  });

  const vectorToggle = toggle("Stream tiles", globe.setVectorTilesEnabled);
  bind(vectorToggle.node, (state) => vectorToggle.set(state.vectorTiles.enabled));

  // Sent whole: omitted fields reset to their defaults.
  function sendVectorStyle() {
    const hex = vectorColor.value;
    globe.setVectorTileStyle({
      lineColor: hex,
      lineWidthPx: 1.4,
      pointColor: hex,
      pointSizePx: 5,
      // Low alpha: tints, never hides, the imagery.
      fillColor: `${hex}${vectorFill.input.checked ? "33" : "00"}`,
    });
  }

  const vectorColor = el("input", {
    class: "swatch",
    type: "color",
    value: "#8cd9ff",
    title: "Vector tile colour",
    oninput: sendVectorStyle,
  });
  const vectorFill = toggle("Fill rings", sendVectorStyle);
  bind(vectorFill.node, (state) => {
    vectorFill.set(!state.vectorTiles.style.fillColor.toLowerCase().endsWith("00"));
    if (document.activeElement !== vectorColor) {
      vectorColor.value = state.vectorTiles.style.lineColor.slice(0, 7).toLowerCase();
    }
  });

  const vectorDetail = el("p", { class: "detail" }, "—");
  bind(vectorDetail, (state) => {
    const tiles = state.vectorTiles;
    const sources = tiles.sourceLayers.length ? tiles.sourceLayers.join(", ") : "every source layer";
    vectorDetail.textContent = tiles.enabled
      ? `${sources} · level ${tiles.deepestLevel} of ${tiles.maxLevel} · ` +
        `${tiles.visibleTiles} drawn · ${tiles.features} features`
      : `${sources} · to level ${tiles.maxLevel}`;
  });

  const vectorTiles = section(
    "Vector tiles",
    row("Source", vectorSelect),
    vectorDetail,
    row("Colour", vectorColor),
    vectorFill.node,
    vectorToggle.node,
    el("p", { class: "detail" }, "MVT from Web Mercator; no data above 85°."),
  );

  // --- GeoJSON overlays ----------------------------------------------------

  const overlays = mountOverlays(bind, signal);

  // --- Satellites ----------------------------------------------------------

  const ephemerides = mountEphemeris(bind);

  // --- Clock ---------------------------------------------------------------

  const pauseToggle = toggle("Run the clock", (running) => globe.setSunPaused(!running));
  bind(pauseToggle.node, (state) => pauseToggle.set(!state.sun.paused));

  const shadedToggle = toggle("Shade the night side", globe.setSunShaded);
  bind(shadedToggle.node, (state) => shadedToggle.set(state.sun.shaded));

  const rate = slider({
    min: limits.minTimeScale,
    max: limits.maxTimeScale,
    format: timeScale,
    onInput: globe.setTimeScale,
  });
  bind(rate.node, (state) => rate.set(state.sun.timeScale), rate.isHeld);

  const sun = section(
    "Clock",
    pauseToggle.node,
    shadedToggle.node,
    rate.node,
    el(
      "div",
      { class: "buttons" },
      button("Now", globe.snapClockToNow),
      button("+6 h", () => shiftClock(6 * 3600)),
      button("+1 day", () => shiftClock(24 * 3600)),
    ),
  );

  // --- Reference frame -----------------------------------------------------

  const frameChoice = choice(
    [
      ["ecef", "ECEF"],
      ["eci", "ECI"],
    ],
    globe.setFrame,
  );
  bind(frameChoice.node, (state) => frameChoice.set(state.frame.mode));

  const frameDetail = el("p", { class: "detail" }, "—");
  bind(frameDetail, (state) => {
    frameDetail.textContent =
      state.frame.mode === "ecef"
        ? "Earth-fixed: the ground is still."
        : "Inertial: the stars are still.";
  });

  const frame = section("Reference frame", frameChoice.node, frameDetail);

  // --- Frame overlay -------------------------------------------------------

  const gncToggle = toggle("Draw overlay", globe.setGncEnabled);
  bind(gncToggle.node, (state) => gncToggle.set(state.gnc.enabled));

  const eciAxesToggle = toggle("Inertial triad (cyan)", globe.setGncEciAxes);
  bind(eciAxesToggle.node, (state) => eciAxesToggle.set(state.gnc.eciAxes));

  const ecefAxesToggle = toggle("Earth-fixed triad (amber)", globe.setGncEcefAxes);
  bind(ecefAxesToggle.node, (state) => ecefAxesToggle.set(state.gnc.ecefAxes));

  const siderealToggle = toggle("Sidereal angle arc", globe.setGncSidereal);
  bind(siderealToggle.node, (state) => siderealToggle.set(state.gnc.sidereal));

  const graticuleToggle = toggle("Graticule", globe.setGncGraticule);
  bind(graticuleToggle.node, (state) => graticuleToggle.set(state.gnc.graticule));

  const graticuleStep = slider({
    min: limits.minGraticuleStep,
    max: limits.maxGraticuleStep,
    format: (degrees) => `${degrees.toFixed(0)}° spacing`,
    onInput: globe.setGncGraticuleStep,
  });
  bind(
    graticuleStep.node,
    (state) => graticuleStep.set(state.gnc.graticuleStepDeg),
    graticuleStep.isHeld,
  );

  const trackToggle = toggle("Followed track, both frames", globe.setGncTrack);
  bind(trackToggle.node, (state) => trackToggle.set(state.gnc.track));

  const trackOrbits = slider({
    min: limits.minTrackOrbits,
    max: limits.maxTrackOrbits,
    format: (orbits) => `${orbits.toFixed(2)} orbits each way`,
    onInput: globe.setGncTrackOrbits,
  });
  bind(trackOrbits.node, (state) => trackOrbits.set(state.gnc.trackOrbits), trackOrbits.isHeld);

  // ECI → ECEF rotation as GMST and quaternion.
  const gncAngle = el("p", { class: "detail" }, "—");
  bind(gncAngle, (state) => {
    gncAngle.textContent =
      `GMST ${state.gnc.gmstDeg.toFixed(3)}° · ${(state.gnc.earthRateDegS * 3600).toFixed(3)}°/h`;
  });

  const gncQuaternion = el("p", { class: "detail" }, "—");
  bind(gncQuaternion, (state) => {
    const fixed = (value) => value.toFixed(4);
    gncQuaternion.textContent = `ECI → ECEF  q = (${state.gnc.quaternion.map(fixed).join(", ")})`;
  });

  const gncFocus = el("p", { class: "detail" }, "—");
  bind(gncFocus, (state) => {
    const focus = state.gnc.focus;
    gncFocus.textContent = focus
      ? `${focus.name} · ${focus.speedEciKmS.toFixed(3)} km/s ECI · ` +
        `${focus.speedEcefKmS.toFixed(3)} km/s ECEF · ${focus.altitudeKm.toFixed(0)} km`
      : "No satellite followed; click one on the globe.";
  });

  const gnc = section(
    { title: "Frame overlay", open: false },
    gncToggle.node,
    eciAxesToggle.node,
    ecefAxesToggle.node,
    siderealToggle.node,
    graticuleToggle.node,
    graticuleStep.node,
    trackToggle.node,
    trackOrbits.node,
    gncAngle,
    gncQuaternion,
    gncFocus,
  );

  // --- Camera --------------------------------------------------------------

  const altitudeSlider = slider({
    min: limits.minAltitudeKm,
    max: limits.maxAltitudeKm,
    format: altitude,
    onInput: globe.setAltitude,
  });
  bind(
    altitudeSlider.node,
    (state) => altitudeSlider.set(state.camera.altitudeKm),
    altitudeSlider.isHeld,
  );

  const latInput = el("input", { class: "coordinate", type: "number", step: "0.0001", value: "0" });
  const lonInput = el("input", { class: "coordinate", type: "number", step: "0.0001", value: "0" });
  const flyTo = () => globe.lookAt(Number(latInput.value) || 0, Number(lonInput.value) || 0);

  const camera = section(
    "Camera",
    altitudeSlider.node,
    row("Latitude", latInput),
    row("Longitude", lonInput),
    el(
      "div",
      { class: "buttons" },
      button("Fly there", flyTo),
      button("Here", () => {
        // Fills the boxes from the view.
        if (!lastState) return;
        latInput.value = lastState.camera.center.lat.toFixed(4);
        lonInput.value = lastState.camera.center.lon.toFixed(4);
      }),
      button("Reset", globe.resetView),
    ),
    el(
      "div",
      { class: "places" },
      ...PLACES.map((place) =>
        button(place.name, () => {
          globe.lookAt(place.lat, place.lon, place.altitudeKm);
          latInput.value = place.lat.toFixed(4);
          lonInput.value = place.lon.toFixed(4);
        }),
      ),
    ),
  );

  // --- The globe's own chrome ----------------------------------------------

  const hudToggle = toggle("Built-in readout", globe.setHudVisible);
  bind(hudToggle.node, (state) => hudToggle.set(state.hud.visible));

  const helpToggle = toggle("Built-in key list", globe.setHelpVisible);
  bind(helpToggle.node, (state) => helpToggle.set(state.hud.helpVisible));

  const keyboardToggle = toggle("Keyboard shortcuts", globe.setKeyboardEnabled);
  bind(keyboardToggle.node, (state) => keyboardToggle.set(state.keyboard));

  const chrome = section(
    { title: "Globe chrome", open: false },
    hudToggle.node,
    helpToggle.node,
    keyboardToggle.node,
  );

  // --- Tabs ----------------------------------------------------------------

  const { strip, panes, select } = tabs([
    { id: "project", label: "Project", panes: [mountProjectSection({ signal })] },
    { id: "layers", label: "Layers", panes: [imagery, vectorTiles, overlays, ephemerides] },
    { id: "spacecraft", label: "Spacecraft", panes: mountSpacecraft(bind, signal) },
    { id: "timeline", label: "Timeline", panes: [mountTimeline(bind)] },
    { id: "analysis", label: "Analysis", panes: [...mountReports(bind, signal), mountEvents(bind, signal), mountCoverage(bind, signal), ...mountMission(bind, signal)] },
    { id: "view", label: "View", panes: [sun, frame, gnc, camera, chrome] },
  ]);

  // Strip stays put; the body scrolls.
  root.append(strip, el("div", { class: "panel-body" }, ...panes));
  if (tab) select(tab);

  return {
    sync(state) {
      lastState = state;
      for (const control of controls) {
        if (!control.isHeld()) control.sync(state);
      }
    },
  };
}

/** Presets grouped by protocol (the same imagery is offered over WMS and WMTS). */
function groupLayersByProtocol(layers) {
  const protocols = [...new Set(layers.map((layer) => layer.protocol))];
  return protocols.map((protocol) =>
    el(
      "optgroup",
      { label: protocol },
      ...layers
        .filter((layer) => layer.protocol === protocol)
        .map((layer) => el("option", { value: String(layer.index) }, layer.label)),
    ),
  );
}
