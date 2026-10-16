/**
 * Project `analysis.monteCarlo`: dispersion studies (Analysis tab), held
 * between open and save like `od.js`, run with `globe.monteCarlo`.
 *
 * - Study: `{id, label?, spacecraft, samples?, seed?, guidance?, dispersions?:
 *   {maneuverMagnitudeSigma, maneuverPointingSigmaDeg, thrustSigma, ispSigma,
 *   burnPointingSigmaDeg, massSigmaKg}, quantities?}`. Defaults in
 *   `MC_DEFAULTS`.
 * - Flies the spacecraft's sequence (open loop: nominal target solutions;
 *   closed: re-targeted per trial), else its own burns. Initial state drawn
 *   from `propagation.covariance`.
 * - Units: fractions, deg, kg (`globe.monteCarlo`).
 *
 * Results are kept per study until it or its spacecraft entry changes; not saved.
 */

import * as globe from "./globe.js";
import { GOAL_QUANTITIES } from "./targeting.js";

export const MC_DEFAULTS = {
  samples: 100,
  seed: 1,
  guidance: "open",
  quantities: ["altitudeKm", "smaKm", "eccentricity", "inclinationDeg", "raanDeg"],
};
export const MAX_SAMPLES = 5000;
export const GUIDANCE = { open: "Open loop", closed: "Closed loop (re-target)" };
/** Dispersion key → `{label, unit, scale}`; display value × `scale` = globe value. */
export const DISPERSIONS = {
  maneuverMagnitudeSigma: { label: "Δv magnitude", unit: "%", scale: 0.01 },
  maneuverPointingSigmaDeg: { label: "Δv pointing", unit: "°", scale: 1 },
  thrustSigma: { label: "Thrust", unit: "%", scale: 0.01 },
  ispSigma: { label: "Isp", unit: "%", scale: 0.01 },
  burnPointingSigmaDeg: { label: "Burn pointing", unit: "°", scale: 1 },
  massSigmaKg: { label: "Mass", unit: "kg", scale: 1 },
};
/** `GOAL_QUANTITIES`: what `globe.monteCarlo` summarises. */
export const MC_QUANTITIES = GOAL_QUANTITIES;

let studies = [];
/** id → `{inputs, result, ms} | {inputs, error}`. */
const results = new Map();
const running = new Set();
const listeners = new Set();
const resultListeners = new Set();

/** Loads `project.analysis.monteCarlo`; does not notify. Call after `globe.reset`. */
export function openMonteCarlo(project) {
  studies = structuredClone(project.analysis?.monteCarlo ?? []);
  results.clear();
  running.clear();
}

/** `callback()` on any edit (not `openMonteCarlo`); returns an unsubscribe. */
export function subscribe(callback) {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

/** `callback()` when a result or run state changes; returns an unsubscribe. */
export function onResult(callback) {
  resultListeners.add(callback);
  return () => resultListeners.delete(callback);
}

function notify(set = listeners) {
  for (const callback of set) callback();
}

export const savedStudies = () => studies;
export const settings = (study) => ({ ...MC_DEFAULTS, dispersions: {}, ...study });
export const isRunning = (id) => running.has(id);

/** Adds `study`, replacing one with its id. */
export function addStudy(study) {
  const copy = structuredClone(study);
  const index = studies.findIndex((other) => other.id === study.id);
  studies = index < 0 ? [...studies, copy] : studies.with(index, copy);
  notify();
}

export function removeStudy(id) {
  studies = studies.filter((study) => study.id !== id);
  results.delete(id);
  notify();
}

/** `base` (slugged), or `base-N`, not yet a study id. */
export function uniqueStudyId(base) {
  const root =
    base
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "") || "monte-carlo";
  const taken = new Set(studies.map((study) => study.id));
  if (!taken.has(root)) return root;
  let n = 2;
  while (taken.has(`${root}-${n}`)) n++;
  return `${root}-${n}`;
}

/** Studies for `analysis.monteCarlo`; those whose spacecraft is gone dropped. */
export function monteCarloStudies({ spacecraftIds }) {
  return structuredClone(studies.filter((study) => spacecraftIds.has(study.spacecraft)));
}

/** Reference errors in `project.analysis.monteCarlo` (paths from `analysis`). */
export function monteCarloErrors(project, { spacecraftIds }) {
  const errors = [];
  const seen = new Set();
  (project.analysis?.monteCarlo ?? []).forEach((study, index) => {
    const path = `monteCarlo[${index}]`;
    if (seen.has(study.id)) errors.push(`${path}.id: duplicate "${study.id}"`);
    seen.add(study.id);
    if (!spacecraftIds.has(study.spacecraft)) errors.push(`${path}.spacecraft: no spacecraft "${study.spacecraft}"`);
  });
  return errors;
}

// --- Running ---------------------------------------------------------------------

function entryOf(id) {
  const entry = globe.recordedSources().spacecraft.get(id);
  if (!entry?.info) throw new Error(entry ? `Spacecraft "${id}" not propagated` : `No spacecraft "${id}"`);
  return entry;
}

/** What a result depends on: the study (not its label) and its spacecraft entry. */
function inputsOf(study) {
  const { label: _label, ...rest } = study;
  return [JSON.stringify(rest), globe.recordedSources().spacecraft.get(study.spacecraft)];
}

const same = (a, b) => a?.length === b.length && a.every((value, index) => value === b[index]);

/**
 * `globe.monteCarlo` arguments `[propagation, sequence | null, options]` for
 * `study`. A sequenced spacecraft flies its given propagation (attitude kept)
 * through the sequence; otherwise its propagation with its burns.
 */
export function monteCarloRequest(study) {
  const s = settings(study);
  const entry = entryOf(s.spacecraft);
  const propagation = entry.sequence
    ? { ...entry.base, ...(entry.propagation.attitude ? { attitude: entry.propagation.attitude } : {}), kvn: false }
    : { ...entry.propagation, kvn: false };
  return [
    propagation,
    entry.sequence ?? null,
    { samples: s.samples, seed: s.seed, guidance: s.guidance, quantities: s.quantities, ...s.dispersions },
  ];
}

/** Runs study `id`; notifies result listeners when it starts and ends. Yields once so "Running…" paints. */
export async function runMonteCarlo(id) {
  const study = studies.find((candidate) => candidate.id === id);
  if (!study || running.has(id)) return;
  running.add(id);
  notify(resultListeners);
  const inputs = inputsOf(study);
  try {
    await new Promise((resolve) => setTimeout(resolve, 0));
    const t0 = performance.now();
    const result = globe.monteCarlo(...monteCarloRequest(study));
    results.set(id, { inputs, result, ms: performance.now() - t0 });
  } catch (error) {
    results.set(id, { inputs, error: error.message ?? String(error) });
  } finally {
    running.delete(id);
    notify(resultListeners);
  }
}

/** `{result, ms, stale}`, `{error, stale}`, or `null` before the first run. */
export function monteCarloResult(id) {
  const kept = results.get(id);
  const study = studies.find((candidate) => candidate.id === id);
  if (!kept || !study) return null;
  const stale = !same(kept.inputs, inputsOf(study));
  return kept.error ? { error: kept.error, stale } : { result: kept.result, ms: kept.ms, stale };
}

// --- Summaries -------------------------------------------------------------------

/**
 * `{edges, counts}`: `bins` equal bins over the finite `values`' range; one
 * bin of width 0 when they are all equal.
 */
export function histogram(values, bins = 20) {
  const finite = values.filter(Number.isFinite);
  if (!finite.length) return { edges: [], counts: [] };
  const lo = Math.min(...finite);
  const hi = Math.max(...finite);
  if (hi === lo) return { edges: [lo, hi], counts: [finite.length] };
  const width = (hi - lo) / bins;
  const counts = new Array(bins).fill(0);
  for (const value of finite) counts[Math.min(bins - 1, Math.floor((value - lo) / width))]++;
  return { edges: Array.from({ length: bins + 1 }, (_, i) => lo + i * width), counts };
}

/** CSV columns: trial, each quantity, then Δv, duration, RSW offset, mass, converged, error. */
export function trialColumns(result) {
  return [
    "trial",
    ...result.quantities,
    "delta_v_km_s",
    "duration_s",
    "offset_r_km",
    "offset_s_km",
    "offset_w_km",
    "mass_kg",
    "converged",
    "error",
  ];
}

/** CSV rows in `trialColumns` order; trial 0 is the nominal. */
export function trialRows(result) {
  const row = (t, index) =>
    t.error
      ? [index, ...result.quantities.map(() => ""), "", "", "", "", "", "", 0, t.error]
      : [index, ...t.values, t.deltaVKmS, t.durationS, ...t.offsetRswKm, t.massKg, t.converged ? 1 : 0, ""];
  return [row(result.nominal, 0), ...result.trials.map((t, index) => row(t, index + 1))];
}
