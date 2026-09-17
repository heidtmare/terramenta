/**
 * The telemetry overlay: everything the globe reports, formatted.
 *
 * The globe has a readout of its own, and this app turns it off and draws this
 * instead — not because the built-in one is lacking, but because replacing it
 * is the point. Every number here comes from the state snapshot, so this is
 * also the clearest demonstration of what that snapshot contains.
 */

import { el } from "./dom.js";
import { altitude, clock, coordinate, timeScale } from "./format.js";

/** Label, and what to pull out of a state snapshot for it. */
const FIELDS = [
  ["cursor", (state) => coordinate(state.cursor)],
  ["centre", (state) => coordinate(state.camera.center)],
  ["altitude", (state) => altitude(state.camera.altitudeKm)],
  ["frame", (state) => state.frame.label],
  ["sun over", (state) => coordinate(state.sun.subsolar) + (state.sun.shaded ? "" : "  (unshaded)")],
  ["clock", (state) => clock(state.sun.unixSeconds) + (state.sun.paused ? "  (paused)" : "")],
  ["rate", (state) => timeScale(state.sun.timeScale)],
  ["imagery", (state) => (state.imagery.enabled ? state.imagery.label : "off")],
  [
    "overlays",
    (state) =>
      state.overlays.layers.length === 0
        ? "none"
        : `${state.overlays.drawn} of ${state.overlays.layers.length} drawn` +
          (state.overlays.enabled ? "" : "  (off)"),
  ],
  [
    "satellites",
    (state) => {
      const { layers, enabled } = state.ephemerides;
      if (layers.length === 0) return "none";
      const drawn = layers.reduce((total, layer) => total + layer.tracked, 0);
      const trailed = layers.reduce((total, layer) => total + (layer.trails ? layer.trailed : 0), 0);
      return `${drawn} drawn · ${trailed} trailed` + (enabled ? "" : "  (off)");
    },
  ],
  [
    "tiles",
    (state) =>
      state.imagery.enabled
        ? `level ${state.imagery.deepestLevel} of ${state.imagery.maxLevel} · ` +
          `${state.imagery.visibleTiles} drawn · ${state.imagery.loadingTiles} loading`
        : "—",
  ],
  ["vectors", (state) => (state.vectorTiles.enabled ? state.vectorTiles.label : "off")],
  [
    "vector tiles",
    (state) =>
      state.vectorTiles.enabled
        ? `level ${state.vectorTiles.deepestLevel} of ${state.vectorTiles.maxLevel} · ` +
          `${state.vectorTiles.visibleTiles} drawn · ${state.vectorTiles.features} features`
        : "—",
  ],
];

export function mountReadout(root) {
  const values = new Map();

  const rows = FIELDS.map(([label]) => {
    const value = el("dd", {}, "—");
    values.set(label, value);
    return el("div", { class: "readout-row" }, el("dt", {}, label), value);
  });

  root.append(el("dl", { class: "readout" }, ...rows));

  return {
    sync(state) {
      for (const [label, read] of FIELDS) {
        values.get(label).textContent = read(state);
      }
    },
  };
}
