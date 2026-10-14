/** Telemetry overlay: the state snapshot, formatted (the globe's own HUD is off). */

import { el } from "./dom.js";
import { altitude, clock, coordinate, timeScale } from "./format.js";

/** `L2/7 · 11 drawn[ · extra]`; `—` while off. */
const tiles = (source, extra) =>
  source.enabled ? [`L${source.deepestLevel}/${source.maxLevel}`, `${source.visibleTiles} drawn`, extra].filter(Boolean).join(" · ") : "—";

/** Label (≤ 8 characters, to fit the column), and what to read off a snapshot. */
const FIELDS = [
  ["cursor", (state) => coordinate(state.cursor)],
  ["centre", (state) => coordinate(state.camera.center)],
  ["altitude", (state) => altitude(state.camera.altitudeKm)],
  ["frame", (state) => state.frame.label],
  ["subsolar", (state) => coordinate(state.sun.subsolar) + (state.sun.shaded ? "" : "  (unshaded)")],
  ["clock", (state) => clock(state.sun.unixSeconds) + (state.sun.paused ? "  (paused)" : "")],
  ["rate", (state) => timeScale(state.sun.timeScale)],
  ["GMST", (state) => `${state.gnc.gmstDeg.toFixed(3)}°`],
  ["imagery", (state) => (state.imagery.enabled ? state.imagery.label : "off")],
  ["tiles", (state) => tiles(state.imagery, state.imagery.loadingTiles ? `${state.imagery.loadingTiles} loading` : "")],
  ["vectors", (state) => (state.vectorTiles.enabled ? state.vectorTiles.label : "off")],
  ["features", (state) => tiles(state.vectorTiles, `${state.vectorTiles.features} features`)],
  [
    "overlays",
    (state) =>
      state.overlays.layers.length === 0
        ? "none"
        : `${state.overlays.drawn} of ${state.overlays.layers.length} drawn` +
          (state.overlays.enabled ? "" : "  (off)"),
  ],
  [
    "objects",
    (state) => {
      const { layers, enabled } = state.ephemerides;
      if (layers.length === 0) return "none";
      const drawn = layers.reduce((total, layer) => total + layer.tracked, 0);
      const trailed = layers.reduce((total, layer) => total + (layer.trails ? layer.trailed : 0), 0);
      return `${drawn} drawn · ${trailed} trailed` + (enabled ? "" : "  (off)");
    },
  ],
  [
    "followed",
    (state) => {
      const focus = state.gnc.focus;
      if (!state.gnc.enabled || !focus) return "—";
      return `${focus.name} · ${focus.speedEciKmS.toFixed(3)} km/s ECI · ${focus.speedEcefKmS.toFixed(3)} km/s ECEF`;
    },
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
