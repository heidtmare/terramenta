/**
 * The porkchop-plot module, as the rest of the app sees it.
 *
 * `terramenta-charta` compiles to a flat module of two functions — `bodies`
 * and `computePorkchopPlot` — and this is the one place that talks to it,
 * the same split `globe.js` keeps for `terramenta-globe`.
 *
 * Unlike the globe there is no running state on the other side of this: every
 * call is a pure function of its arguments, answered once and returned, not
 * streamed back on a callback. So this module has nothing to start and
 * nothing to poll — `load()` and then `computePlot()` is the whole of it.
 */

/** Where `scripts/build.sh` puts the module. */
const MODULE_URL = "../charta/terramenta_charta.js";

/** Resolved once `load()` has run. */
let wasm = null;

/** Downloads and instantiates the module. Safe to call more than once. */
export async function load() {
  if (!wasm) {
    const module = await import(MODULE_URL);
    await module.default();
    wasm = module;
  }
  return wasm;
}

function required() {
  if (!wasm) throw new Error("The porkchop module has not been loaded yet — await load() first.");
  return wasm;
}

/** The bodies a plot can be drawn between — answers without solving anything. */
export function bodies() {
  return required().bodies();
}

/**
 * Computes a porkchop plot between `originBody` and `destinationBody` (names
 * from `bodies()`), over a departure date range and an arrival date range,
 * each `{start, end, steps}` with `start`/`end` as `Date`s or Unix seconds and
 * `steps` at least 2.
 *
 * `contourLevelsKmS` are the total delta-v values, in km/s, to trace contours
 * at; leave it out for the grid alone. `retrograde` selects the long way
 * round a transfer sweeps its focus.
 *
 * Returns `null` if either body name is unknown or either range has fewer
 * than 2 steps — otherwise `{departuresUnixSeconds, arrivalsUnixSeconds,
 * cells, totalDeltaVBoundsKmS, contours}`, exactly as `terramenta-charta`'s
 * own module docs describe.
 */
export function computePlot(
  originBody,
  destinationBody,
  departure,
  arrival,
  { contourLevelsKmS = [], retrograde = false } = {},
) {
  return required().computePorkchopPlot(
    originBody,
    destinationBody,
    toUnixSeconds(departure.start),
    toUnixSeconds(departure.end),
    departure.steps,
    toUnixSeconds(arrival.start),
    toUnixSeconds(arrival.end),
    arrival.steps,
    Float64Array.from(contourLevelsKmS),
    retrograde,
  );
}

function toUnixSeconds(dateOrUnixSeconds) {
  return dateOrUnixSeconds instanceof Date ? dateOrUnixSeconds.getTime() / 1000 : dateOrUnixSeconds;
}
