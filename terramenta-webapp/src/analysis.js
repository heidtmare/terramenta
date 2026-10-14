/**
 * Project `analysis`: saved plots (Reports tab) and porkchop studies (Mission
 * tab). Held here between open and save, like `timeline.js`.
 *
 * - Plot: `{id, label?, quantity, spacecraft: [id]}`, `quantity` a
 *   `reports.QUANTITIES` key.
 * - Porkchop: `{id, label?, origin, destination, departure: {start, end},
 *   arrival: {start, end}, retrograde?, steps?}`, dates `YYYY-MM-DD` (UTC).
 */

let plots = [];
let porkchops = [];
const listeners = new Set();

/** Loads `project.analysis`; does not notify. */
export function openAnalysis(project) {
  plots = structuredClone(project.analysis?.plots ?? []);
  porkchops = structuredClone(project.analysis?.porkchops ?? []);
}

/** `callback()` on any edit (not `openAnalysis`); returns an unsubscribe. */
export function subscribe(callback) {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

function notify() {
  for (const callback of listeners) callback();
}

export const savedPlots = () => plots;
export const savedPorkchops = () => porkchops;

/** Adds `plot`, replacing one with its id. */
export function addPlot(plot) {
  plots = replaced(plots, plot);
  notify();
}

export function removePlot(id) {
  plots = plots.filter((plot) => plot.id !== id);
  notify();
}

/** Adds `study`, replacing one with its id. */
export function addPorkchop(study) {
  porkchops = replaced(porkchops, study);
  notify();
}

export function removePorkchop(id) {
  porkchops = porkchops.filter((study) => study.id !== id);
  notify();
}

/** `base`, or `base-N`, unused among `"plots"` or `"porkchops"`. */
export function uniqueId(kind, base) {
  const taken = new Set((kind === "plots" ? plots : porkchops).map((entry) => entry.id));
  if (!taken.has(base)) return base;
  let n = 2;
  while (taken.has(`${base}-${n}`)) n++;
  return `${base}-${n}`;
}

/**
 * `project.analysis` for saving: spacecraft no longer in the scene dropped
 * from plots, empty plots dropped; `undefined` if nothing is left.
 */
export function analysisSection({ spacecraftIds }) {
  const section = {};
  const kept = plots
    .map((plot) => ({ ...plot, spacecraft: plot.spacecraft.filter((id) => spacecraftIds.has(id)) }))
    .filter((plot) => plot.spacecraft.length);
  if (kept.length) section.plots = structuredClone(kept);
  if (porkchops.length) section.porkchops = structuredClone(porkchops);
  return Object.keys(section).length ? section : undefined;
}

/** Reference errors in `project.analysis` (paths from `analysis`); structure already valid. */
export function analysisErrors(project, { spacecraftIds }) {
  const analysis = project.analysis;
  if (!analysis) return [];
  const errors = [];
  for (const kind of ["plots", "porkchops"]) {
    const seen = new Set();
    analysis[kind]?.forEach((entry, index) => {
      if (seen.has(entry.id)) errors.push(`${kind}[${index}].id: duplicate "${entry.id}"`);
      seen.add(entry.id);
    });
  }
  analysis.plots?.forEach((plot, index) => {
    plot.spacecraft.forEach((id, at) => {
      if (!spacecraftIds.has(id)) errors.push(`plots[${index}].spacecraft[${at}]: no spacecraft "${id}"`);
    });
  });
  analysis.porkchops?.forEach((study, index) => {
    if (study.origin === study.destination) errors.push(`porkchops[${index}]: origin and destination are the same body`);
    for (const range of ["departure", "arrival"]) {
      const { start, end } = study[range];
      if (!(Date.parse(start) < Date.parse(end))) errors.push(`porkchops[${index}].${range}: start must be before end`);
    }
  });
  return errors;
}

function replaced(list, entry) {
  const copy = structuredClone(entry);
  const index = list.findIndex((other) => other.id === entry.id);
  return index < 0 ? [...list, copy] : list.with(index, copy);
}
