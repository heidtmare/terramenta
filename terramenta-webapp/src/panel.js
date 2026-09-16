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
 */

import * as globe from "./globe.js";
import { el, row, section } from "./dom.js";
import { altitude, logScale, timeScale } from "./format.js";
import { PLACES } from "./places.js";

/** Slider positions are `0..1` at this resolution, and the range is applied on top. */
const SLIDER_STEPS = 1000;

export function mountPanel(root) {
  const limits = globe.limits();
  const layers = globe.layers();
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

  root.append(imagery, sun, frame, camera, chrome);

  return {
    sync(state) {
      lastState = state;
      for (const control of controls) {
        if (!control.isHeld()) control.sync(state);
      }
    },
  };
}

// ---------------------------------------------------------------------------
// Control widgets
// ---------------------------------------------------------------------------

function button(label, onClick) {
  return el("button", { class: "button", type: "button", onclick: onClick }, label);
}

/** A checkbox that reports its new state. */
function toggle(label, onChange) {
  const input = el("input", {
    type: "checkbox",
    checked: true,
    onchange: (event) => onChange(event.target.checked),
  });
  const node = el("label", { class: "toggle" }, input, el("span", {}, label));
  return {
    node,
    set: (value) => {
      input.checked = value;
    },
  };
}

/** A set of mutually exclusive buttons. */
function choice(options, onChange) {
  const buttons = options.map(([value, label]) =>
    el(
      "button",
      { class: "choice-option", type: "button", value, onclick: () => onChange(value) },
      label,
    ),
  );
  const node = el("div", { class: "choice" }, ...buttons);
  return {
    node,
    set: (value) => {
      for (const button of buttons) {
        button.classList.toggle("selected", button.value === value);
      }
    },
  };
}

/** A slider over a range that spans orders of magnitude, with its value shown. */
function slider({ min, max, format, onInput }) {
  // Held from the moment the thumb is grabbed until it is let go, whether by
  // pointer or by arrow key. The globe reports the altitude it is smoothing
  // toward, so without this the slider would spring back under the pointer.
  let held = false;

  const output = el("span", { class: "slider-value" }, "—");
  const input = el("input", {
    class: "slider",
    type: "range",
    min: "0",
    max: String(SLIDER_STEPS),
    value: "0",
    onpointerdown: () => (held = true),
    onpointerup: () => (held = false),
    onpointercancel: () => (held = false),
    onkeydown: () => (held = true),
    onkeyup: () => (held = false),
    onblur: () => (held = false),
    oninput: (event) => {
      const value = logScale.toValue(Number(event.target.value) / SLIDER_STEPS, min, max);
      output.textContent = format(value);
      onInput(value);
    },
  });
  const node = el("div", { class: "slider-row" }, input, output);
  return {
    node,
    isHeld: () => held,
    set: (value) => {
      const clamped = Math.min(Math.max(value, min), max);
      input.value = String(Math.round(logScale.toPosition(clamped, min, max) * SLIDER_STEPS));
      output.textContent = format(value);
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
