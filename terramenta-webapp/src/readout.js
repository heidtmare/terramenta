/** Telemetry overlay: the state snapshot, formatted (the globe's own HUD is off). */

import { el, section } from "./dom.js";
import { altitude, clock, coordinate, timeScale } from "./format.js";

/** `L2/7 · 11 drawn[ · extra]`; `—` while off. */
const tiles = (source, extra) =>
  source.enabled ? [`L${source.deepestLevel}/${source.maxLevel}`, `${source.visibleTiles} drawn`, extra].filter(Boolean).join(" · ") : "—";

/** Headline values: `[label, value, note]`. Zone moves to the note to fit the column. */
const HERO = [
  [
    "clock",
    (state) => clock(state.sun.unixSeconds).replace(" UTC", ""),
    (state) => `UTC · ${state.sun.paused ? "paused" : timeScale(state.sun.timeScale)}`,
  ],
  ["altitude", (state) => altitude(state.camera.altitudeKm), (state) => state.frame.label],
];

/** `[label, read]`; label ≤ 8 characters; `read` → null hides the row. */
const POSITION = [
  ["cursor", (state) => coordinate(state.cursor)],
  ["centre", (state) => coordinate(state.camera.center)],
  [
    "followed",
    (state) => {
      const focus = state.gnc.focus;
      if (!state.gnc.enabled || !focus) return null;
      return `${focus.name} · ${focus.speedEciKmS.toFixed(3)} km/s ECI · ${focus.speedEcefKmS.toFixed(3)} km/s ECEF`;
    },
  ],
];

const EARTH = [
  ["subsolar", (state) => coordinate(state.sun.subsolar) + (state.sun.shaded ? "" : "  (unshaded)")],
  ["GMST", (state) => `${state.gnc.gmstDeg.toFixed(3)}°`],
];

const SCENE = [
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
];

export function mountReadout(root) {
  const updates = [];

  const hero = HERO.map(([label, read, note]) => {
    const value = el("span", { class: "hero-value" }, "—");
    const sub = el("span", { class: "hero-note" });
    updates.push((state) => {
      value.textContent = read(state);
      sub.textContent = note(state);
    });
    return el("div", { class: "hero" }, el("span", { class: "hero-label" }, label), value, sub);
  });

  const list = (fields) =>
    el(
      "dl",
      { class: "readout" },
      fields.map(([label, read]) => {
        const value = el("dd", {}, "—");
        const node = el("div", { class: "readout-row" }, el("dt", {}, label), value);
        updates.push((state) => {
          const text = read(state);
          node.hidden = text === null;
          if (text !== null) value.textContent = text;
        });
        return node;
      }),
    );

  root.append(
    el("div", { class: "readout-hero" }, hero),
    list(POSITION),
    list(EARTH),
    section({ title: "Scene", open: false }, list(SCENE)),
  );

  return {
    sync(state) {
      for (const update of updates) update(state);
    },
  };
}
