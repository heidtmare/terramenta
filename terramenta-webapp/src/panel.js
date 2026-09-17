/**
 * The control panel: every feature the globe exposes, as something to click.
 *
 * The data flow only goes one way. A control sends a command and then forgets
 * about it — it never sets its own value from a click. The globe applies the
 * command, the next state snapshot comes back, and `sync` sets every control
 * from it. So the panel cannot drift out of step with the globe, and the
 * keyboard shortcuts still work with the panel open: press `P` and the pause
 * button changes with it, because both are drawn from the same snapshot.
 *
 * The one exception is a slider being dragged. A value overwritten mid-drag
 * fights the pointer, so a slider says so while it is being held and `sync`
 * leaves it alone until it is let go.
 *
 * The sections are dealt out into three tabs — the imagery under everything,
 * the data drawn over it, and the view it is all seen from. Switching tabs
 * only hides panes; nothing is torn down or rebuilt, so a control in a closed
 * tab is still bound and still current when you come back to it.
 */

import * as globe from "./globe.js";
import { el, row, section, tabs } from "./dom.js";
import { mountEphemeris } from "./ephemeris.js";
import { altitude, timeScale } from "./format.js";
import { mountOverlays } from "./overlays.js";
import { PLACES } from "./places.js";
import { button, choice, slider, toggle } from "./widgets.js";

export function mountPanel(root) {
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

  // The style is sent whole rather than a field at a time: the globe returns
  // anything left out to its default, so a colour change alone would silently
  // undo the width beside it. Declared as a function so the colour input and
  // the fill toggle can both be built around it.
  function sendVectorStyle() {
    const hex = vectorColor.value;
    globe.setVectorTileStyle({
      lineColor: hex,
      lineWidthPx: 1.4,
      pointColor: hex,
      pointSizePx: 5,
      // A basemap's fills would hide the imagery under them, so the alpha stays
      // low even when they are on — enough to tint a lake, not to hide it.
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
    el(
      "p",
      { class: "detail" },
      "Mapbox Vector Tiles, decoded and unprojected out of Web Mercator onto " +
        "the globe. Nothing above 85° is in any tile, which is why the poles " +
        "have no coastline.",
    ),
  );

  // --- GeoJSON overlays ----------------------------------------------------

  // Enough of a feature to be its own file; `bind` is handed over so its
  // controls follow the same one-way rule as everything here.
  const overlays = mountOverlays(bind);

  // --- Satellites ----------------------------------------------------------

  // The other layer built on the same GeoArrow store and the same shader, and
  // the only one whose geometry is computed rather than fetched.
  const ephemerides = mountEphemeris(bind);

  // --- Sun and clock -------------------------------------------------------

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
    "Sun & clock",
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
        ? "Earth-fixed: the ground is still and the stars turn."
        : "Inertial: the stars are still and the Earth turns beneath.";
  });

  const frame = section("Reference frame", frameChoice.node, frameDetail);

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
        // Filling the boxes from the view is what makes the coordinate of a
        // place you found by dragging recoverable.
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
    "Globe chrome",
    hudToggle.node,
    helpToggle.node,
    keyboardToggle.node,
    el(
      "p",
      { class: "detail" },
      "The globe draws its own overlay and binds its own keys. An embedder can " +
        "keep either, or switch both off and do it all itself — which is what " +
        "this panel is.",
    ),
  );

  // --- Tabs ----------------------------------------------------------------

  // Imagery is the basemap under everything; the data layers are what is drawn
  // over it, vector tiles included — they are decoded features, not pixels,
  // and belong with the other two feature layers rather than with the raster
  // imagery. What is left is how the scene is viewed rather than what is in it.
  const { strip, panes } = tabs([
    { id: "imagery", label: "Imagery", panes: [imagery] },
    { id: "data", label: "Data layers", panes: [vectorTiles, overlays, ephemerides] },
    { id: "other", label: "Other", panes: [sun, frame, camera, chrome] },
  ]);

  // The strip keeps its place while the pane below it scrolls, so the tabs are
  // still reachable from the bottom of a long one.
  root.append(strip, el("div", { class: "panel-body" }, ...panes));

  return {
    sync(state) {
      lastState = state;
      for (const control of controls) {
        if (!control.isHeld()) control.sync(state);
      }
    },
  };
}

/**
 * Splits the presets into WMS and WMTS groups.
 *
 * The same layers are offered over both protocols so they can be compared on
 * the same imagery, which only reads as deliberate if the list says which is
 * which rather than appearing to repeat itself.
 */
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
