/**
 * Project `analysis.coverage`: access coverage studies of an H3 grid (Analysis
 * tab), held between open and save like `analysis.js`, run with
 * `globe.coverage`.
 *
 * - Study: `{id, label?, spacecraft: [id], access?, minElevationDeg?,
 *   resolution?, bounds?: [west, south, east, north], stepS?, metric?, show?}`;
 *   defaults in `COVERAGE_DEFAULTS`.
 * - `access: "sensors"`: each spacecraft's attitude profile (`attitude.js`)
 *   and its sensors with a `fov`; spacecraft without one see nothing.
 *
 * Results are kept per study until it, a spacecraft entry or a profile
 * changes; not saved. The `show` study is re-run when stale and drawn as
 * overlay `COVERAGE_LAYER` (not listed in Layers, not saved as an overlay),
 * shaded by `metric`.
 */

import { attitudeOptions, savedAttitudes } from "./attitude.js";
import { duration } from "./format.js";
import * as globe from "./globe.js";

export const COVERAGE_LAYER = "coverage";
export const COVERAGE_DEFAULTS = { access: "horizon", minElevationDeg: 0, resolution: 2, stepS: 60, metric: "fraction" };
/** Metric → label, cell value, formatter. */
export const METRICS = {
  fraction: { label: "Coverage", value: (cell) => cell.fraction, format: (value) => `${(100 * value).toFixed(1)} %` },
  maxGap: { label: "Max gap", value: (cell) => cell.maxGapS, format: (value) => duration(value) },
  meanGap: { label: "Mean revisit", value: (cell) => cell.meanGapS, format: (value) => duration(value) },
  accesses: { label: "Accesses", value: (cell) => cell.accesses, format: (value) => String(Math.round(value)) },
};
/** Low → high. */
export const RAMP = ["#30123b", "#4686fb", "#1ae4b6", "#a2fc3c", "#fabb39", "#e4460a"];
const FILL_OPACITY = 0.6;

let studies = [];
/** id → `{inputs, result | error, ms}`; `inputs` as computed. */
const results = new Map();
/** `[result, metric]` drawn, or `null`. */
let drawn = null;
const listeners = new Set();
const resultListeners = new Set();

/** Loads `project.analysis.coverage`; does not notify. Call after `globe.reset`. */
export function openCoverage(project) {
  studies = structuredClone(project.analysis?.coverage ?? []);
  results.clear();
  drawn = null;
}

/** `callback()` on any edit (not `openCoverage`); returns an unsubscribe. */
export function subscribe(callback) {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

/** `callback()` when a result changes; returns an unsubscribe. */
export function onResult(callback) {
  resultListeners.add(callback);
  return () => resultListeners.delete(callback);
}

function notify(set = listeners) {
  for (const callback of set) callback();
}

export const savedCoverage = () => studies;
export const settings = (study) => ({ ...COVERAGE_DEFAULTS, ...study });

/** Adds `study`, replacing one with its id; showing it hides the others. */
export function addCoverage(study) {
  const copy = structuredClone(study);
  const index = studies.findIndex((other) => other.id === study.id);
  studies = index < 0 ? [...studies, copy] : studies.with(index, copy);
  if (copy.show) studies = studies.map((other) => (other === copy || !other.show ? other : withoutShow(other)));
  notify();
}

export function removeCoverage(id) {
  studies = studies.filter((study) => study.id !== id);
  results.delete(id);
  notify();
}

/** Shows study `id` on the globe (hiding any other), or none for `null`. */
export function showCoverage(id) {
  studies = studies.map((study) => (study.id === id ? { ...study, show: true } : withoutShow(study)));
  notify();
}

/** Sets study `id`'s shaded metric. */
export function setMetric(id, metric) {
  studies = studies.map((study) => {
    if (study.id !== id) return study;
    const { metric: _metric, ...rest } = study;
    return metric === COVERAGE_DEFAULTS.metric ? rest : { ...rest, metric };
  });
  notify();
}

function withoutShow(study) {
  if (!study.show) return study;
  const { show: _show, ...rest } = study;
  return rest;
}

/** `base` (slugged), or `base-N`, not yet a study id. */
export function uniqueCoverageId(base) {
  const root =
    base
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "") || "coverage";
  const taken = new Set(studies.map((study) => study.id));
  if (!taken.has(root)) return root;
  let n = 2;
  while (taken.has(`${root}-${n}`)) n++;
  return `${root}-${n}`;
}

/** Studies for `analysis.coverage`: absent spacecraft dropped, empty studies dropped. */
export function coverageStudies({ spacecraftIds }) {
  return structuredClone(
    studies
      .map((study) => ({ ...study, spacecraft: study.spacecraft.filter((id) => spacecraftIds.has(id)) }))
      .filter((study) => study.spacecraft.length),
  );
}

/** Reference errors in `project.analysis.coverage` (paths from `analysis`). */
export function coverageErrors(project, { spacecraftIds }) {
  const errors = [];
  const seen = new Set();
  const list = project.analysis?.coverage ?? [];
  const fovs = new Set(
    (project.attitudes ?? []).filter((profile) => profile.sensors?.some((sensor) => sensor.fov)).map((profile) => profile.spacecraft),
  );
  list.forEach((study, index) => {
    const path = `coverage[${index}]`;
    if (seen.has(study.id)) errors.push(`${path}.id: duplicate "${study.id}"`);
    seen.add(study.id);
    study.spacecraft.forEach((id, at) => {
      if (!spacecraftIds.has(id)) errors.push(`${path}.spacecraft[${at}]: no spacecraft "${id}"`);
    });
    if (study.bounds && !(study.bounds[1] <= study.bounds[3])) errors.push(`${path}.bounds: south must not exceed north`);
    if (study.access === "sensors" && !study.spacecraft.some((id) => fovs.has(id))) {
      errors.push(`${path}: access "sensors" needs a spacecraft with an attitude sensor fov`);
    }
  });
  if (list.filter((study) => study.show).length > 1) errors.push("coverage: show on more than one study");
  return errors;
}

// --- Running ---------------------------------------------------------------------

/** What a result depends on: the study's inputs (not its label, metric or `show`), its spacecraft entries and profiles. */
function inputsOf(study) {
  const entries = globe.recordedSources().spacecraft;
  const profiles = savedAttitudes();
  const { spacecraft, access, minElevationDeg, resolution, bounds, stepS } = settings(study);
  return [
    JSON.stringify([spacecraft, access, minElevationDeg, resolution, bounds, stepS]),
    ...spacecraft.flatMap((id) => [entries.get(id), profiles.find((profile) => profile.spacecraft === id)]),
  ];
}

const same = (a, b) => a?.length === b.length && a.every((value, index) => value === b[index]);

/** `globe.coverage` options for `study`; throws on a missing or unpropagated spacecraft. */
export function coverageRequest(study) {
  const s = settings(study);
  const entries = globe.recordedSources().spacecraft;
  const spacecraft = s.spacecraft.map((id) => {
    const entry = entries.get(id);
    if (!entry?.info) throw new Error(entry ? `Spacecraft "${id}" not propagated` : `No spacecraft "${id}"`);
    const observer = { propagation: { ...entry.propagation, kvn: false } };
    if (s.access !== "sensors") return observer;
    const profile = savedAttitudes().find((candidate) => candidate.spacecraft === id);
    const sensors = (profile?.sensors ?? [])
      .filter((sensor) => sensor.fov)
      .map((sensor) => ({ body: sensor.body, ...sensor.fov }));
    if (!sensors.length) return null;
    const { stepS: _step, aem: _aem, sensors: _sensors, ...law } = attitudeOptions(profile);
    return { ...observer, attitude: law, sensors };
  });
  const observers = spacecraft.filter(Boolean);
  if (!observers.length) throw new Error("No spacecraft has an attitude sensor with a field of view");
  const request = {
    spacecraft: observers,
    resolution: s.resolution,
    minElevationDeg: s.minElevationDeg,
    stepS: s.stepS,
    boundaries: true,
  };
  if (s.bounds) request.bounds = s.bounds;
  return request;
}

/** Runs study `id` now; notifies result listeners. */
export function runCoverage(id) {
  const study = studies.find((candidate) => candidate.id === id);
  if (!study) return;
  const inputs = inputsOf(study);
  const t0 = performance.now();
  try {
    const result = globe.coverage(coverageRequest(study));
    results.set(id, { inputs, result, ms: performance.now() - t0 });
  } catch (error) {
    results.set(id, { inputs, error: error.message ?? String(error) });
  }
  notify(resultListeners);
}

/** `{result, ms, stale}`, `{error, stale}`, or `null` before the first run. */
export function coverageResult(id) {
  const kept = results.get(id);
  const study = studies.find((candidate) => candidate.id === id);
  if (!kept || !study) return null;
  const stale = !same(kept.inputs, inputsOf(study));
  return kept.error ? { error: kept.error, stale } : { result: kept.result, ms: kept.ms, stale };
}

/** Re-runs the shown study when stale and draws it; removes the drawing when none is shown. Cheap when nothing changed. */
export function syncCoverage() {
  const study = studies.find((candidate) => candidate.show);
  if (study) {
    const kept = coverageResult(study.id);
    if (!kept || kept.stale) runCoverage(study.id);
  }
  const result = study && results.get(study.id)?.result;
  const metric = study && settings(study).metric;
  if (!result) {
    if (drawn) {
      drawn = null;
      globe.quietly(() => globe.removeOverlay(COVERAGE_LAYER));
    }
    return;
  }
  if (drawn?.[0] === result && drawn[1] === metric) return;
  drawn = [result, metric];
  globe.quietly(() =>
    globe.addOverlay(COVERAGE_LAYER, {
      label: `Coverage: ${study.label ?? study.id}`,
      text: JSON.stringify(coverageGeoJson(result, metric)),
      simpleStyle: true,
      lineColor: "#00000000",
      lineWidthPx: 0,
    }),
  );
}

// --- Summaries -------------------------------------------------------------------

/** `{min, max}` of `metric` over cells that have it; spans for gaps of never-covered cells count. */
export function metricRange(result, metric) {
  let min = Infinity;
  let max = -Infinity;
  for (const cell of result.cells) {
    const value = METRICS[metric].value(cell);
    if (value === null || value === undefined) continue;
    min = Math.min(min, value);
    max = Math.max(max, value);
  }
  return Number.isFinite(min) ? { min, max } : { min: 0, max: 0 };
}

/** Cell statistics summed up: share of cells ever covered, mean coverage, gaps. */
export function summarizeCoverage(result) {
  const cells = result.cells;
  const covered = cells.filter((cell) => cell.fraction > 0);
  const revisits = cells.map((cell) => cell.meanGapS).filter((value) => value !== null && value !== undefined);
  const gaps = cells.map((cell) => cell.maxGapS).sort((a, b) => a - b);
  return {
    cells: cells.length,
    coveredShare: cells.length ? covered.length / cells.length : 0,
    meanFraction: cells.length ? cells.reduce((sum, cell) => sum + cell.fraction, 0) / cells.length : 0,
    medianMaxGapS: gaps.length ? gaps[Math.floor(gaps.length / 2)] : 0,
    worstMaxGapS: gaps.at(-1) ?? 0,
    meanRevisitS: revisits.length ? revisits.reduce((sum, value) => sum + value, 0) / revisits.length : null,
  };
}

/** `RAMP` colour at `s` in [0, 1]. */
export function rampColor(s) {
  const x = Math.min(Math.max(Number.isFinite(s) ? s : 0, 0), 1) * (RAMP.length - 1);
  const i = Math.min(Math.floor(x), RAMP.length - 2);
  const [a, b] = [RAMP[i], RAMP[i + 1]].map((hex) => [1, 3, 5].map((at) => parseInt(hex.slice(at, at + 2), 16)));
  const t = x - i;
  return `#${a.map((value, k) => Math.round(value + (b[k] - value) * t).toString(16).padStart(2, "0")).join("")}`;
}

/**
 * Cells as GeoJSON polygons filled by `metric` on `RAMP`; cells without a
 * value, and never-covered cells for coverage and accesses, left out.
 */
export function coverageGeoJson(result, metric) {
  const { label, value, format } = METRICS[metric];
  const { min, max } = metricRange(result, metric);
  const features = [];
  for (const cell of result.cells) {
    const v = value(cell);
    if (v === null || v === undefined || !cell.boundary) continue;
    if ((metric === "fraction" || metric === "accesses") && cell.fraction === 0) continue;
    features.push({
      type: "Feature",
      geometry: { type: "Polygon", coordinates: [ring(cell.boundary)] },
      properties: {
        title: cell.id,
        [label.toLowerCase()]: format(v),
        "fill": rampColor(max > min ? (v - min) / (max - min) : 1),
        "fill-opacity": FILL_OPACITY,
      },
    });
  }
  return { type: "FeatureCollection", features };
}

/**
 * Closed GeoJSON ring of an H3 outline: longitudes unwrapped; a cell around a
 * pole closed along it in 90° steps, so the globe's shortest-turn unwrapping
 * keeps the sweep.
 */
export function ring(boundary) {
  const out = [];
  let previous = null;
  for (const [lon, lat] of boundary) {
    let unwrapped = lon;
    if (previous !== null) unwrapped = previous + ((((lon - previous + 180) % 360) + 360) % 360) - 180;
    out.push([unwrapped, lat]);
    previous = unwrapped;
  }
  const [first] = out;
  const last = out.at(-1);
  const closing = ((((first[0] - last[0] + 180) % 360) + 360) % 360) - 180;
  if (Math.abs(last[0] + closing - first[0]) > 180) {
    const pole = out.reduce((sum, [, lat]) => sum + lat, 0) > 0 ? 90 : -90;
    const end = last[0] + closing;
    out.push([end, first[1]]);
    for (let k = 0; k <= 4; k++) out.push([end + ((first[0] - end) * k) / 4, pole]);
  }
  out.push([...first]);
  return out;
}

/** CSV rows: `[id, latDeg, lonDeg, fraction, accesses, maxGapS, meanGapS, firstAccessS]`. */
export function coverageRows(result) {
  return result.cells.map((cell) => [
    cell.id,
    cell.latDeg,
    cell.lonDeg,
    cell.fraction,
    cell.accesses,
    cell.maxGapS,
    cell.meanGapS ?? "",
    cell.firstAccessS ?? "",
  ]);
}

export const COVERAGE_COLUMNS = ["h3", "lat_deg", "lon_deg", "fraction", "accesses", "max_gap_s", "mean_gap_s", "first_access_s"];
