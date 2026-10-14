/**
 * Project `analysis.conjunctions`: close-approach screening studies (Analysis
 * tab), held between open and save like `coverage.js`, run with
 * `globe.conjunctions`.
 *
 * - Study: `{id, label?, primary, secondaries?: [spacecraft id],
 *   layers?: [ephemeris layer id], thresholdKm?, hardBodyRadiusM?, stepS?,
 *   secondarySigmaKm?: [r, s, w]}`; defaults in `CONJUNCTION_DEFAULTS`.
 * - Pc needs a covariance: the primary's `propagation.covariance`, a
 *   secondary spacecraft's, or `secondarySigmaKm` for catalogue objects and
 *   spacecraft without one. `pcMax` needs none.
 *
 * Results are kept per study until it or a spacecraft entry changes; not
 * saved. Layers not loaded when a study runs are skipped and listed.
 */

import * as globe from "./globe.js";
import { isoUtc } from "./reports.js";

export const CONJUNCTION_DEFAULTS = { thresholdKm: 5, hardBodyRadiusM: 20, stepS: 60 };

let studies = [];
/** id → `{inputs, result, missing, ms} | {inputs, error}`. */
const results = new Map();
const listeners = new Set();
const resultListeners = new Set();

/** Loads `project.analysis.conjunctions`; does not notify. Call after `globe.reset`. */
export function openConjunctions(project) {
  studies = structuredClone(project.analysis?.conjunctions ?? []);
  results.clear();
}

/** `callback()` on any edit (not `openConjunctions`); returns an unsubscribe. */
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

export const savedConjunctions = () => studies;
export const settings = (study) => ({ ...CONJUNCTION_DEFAULTS, secondaries: [], layers: [], ...study });

/** Adds `study`, replacing one with its id. */
export function addConjunction(study) {
  const copy = structuredClone(study);
  const index = studies.findIndex((other) => other.id === study.id);
  studies = index < 0 ? [...studies, copy] : studies.with(index, copy);
  notify();
}

export function removeConjunction(id) {
  studies = studies.filter((study) => study.id !== id);
  results.delete(id);
  notify();
}

/** `base` (slugged), or `base-N`, not yet a study id. */
export function uniqueConjunctionId(base) {
  const root =
    base
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "") || "conjunctions";
  const taken = new Set(studies.map((study) => study.id));
  if (!taken.has(root)) return root;
  let n = 2;
  while (taken.has(`${root}-${n}`)) n++;
  return `${root}-${n}`;
}

/**
 * Studies for `analysis.conjunctions`: absent secondaries and layers dropped;
 * studies whose primary is gone, or with nothing left to screen, dropped.
 */
export function conjunctionStudies({ spacecraftIds, ephemerisIds }) {
  const kept = (study, key, ids) => {
    if (!study[key]) return study;
    const left = study[key].filter((id) => ids.has(id) && id !== study.primary);
    const { [key]: _all, ...rest } = study;
    return left.length ? { ...rest, [key]: left } : rest;
  };
  return structuredClone(
    studies
      .filter((study) => spacecraftIds.has(study.primary))
      .map((study) => kept(kept(study, "secondaries", spacecraftIds), "layers", ephemerisIds))
      .filter((study) => study.secondaries?.length || study.layers?.length),
  );
}

/** Reference errors in `project.analysis.conjunctions` (paths from `analysis`). */
export function conjunctionErrors(project, { spacecraftIds, ephemerisIds }) {
  const errors = [];
  const seen = new Set();
  (project.analysis?.conjunctions ?? []).forEach((study, index) => {
    const path = `conjunctions[${index}]`;
    if (seen.has(study.id)) errors.push(`${path}.id: duplicate "${study.id}"`);
    seen.add(study.id);
    if (!spacecraftIds.has(study.primary)) errors.push(`${path}.primary: no spacecraft "${study.primary}"`);
    (study.secondaries ?? []).forEach((id, at) => {
      if (!spacecraftIds.has(id)) errors.push(`${path}.secondaries[${at}]: no spacecraft "${id}"`);
      else if (id === study.primary) errors.push(`${path}.secondaries[${at}]: the primary itself`);
    });
    (study.layers ?? []).forEach((id, at) => {
      if (!ephemerisIds.has(id)) errors.push(`${path}.layers[${at}]: no ephemeris layer "${id}"`);
    });
    if (!study.secondaries?.length && !study.layers?.length) errors.push(`${path}: needs secondaries or layers`);
  });
  return errors;
}

// --- Running ---------------------------------------------------------------------

/** What a result depends on: the study's inputs (not its label) and its spacecraft entries. */
function inputsOf(study) {
  const entries = globe.recordedSources().spacecraft;
  const { primary, secondaries, layers, thresholdKm, hardBodyRadiusM, stepS, secondarySigmaKm } = settings(study);
  return [
    JSON.stringify([primary, secondaries, layers, thresholdKm, hardBodyRadiusM, stepS, secondarySigmaKm]),
    ...[primary, ...secondaries].map((id) => entries.get(id)),
  ];
}

const same = (a, b) => a?.length === b.length && a.every((value, index) => value === b[index]);

function propagationOf(id) {
  const entry = globe.recordedSources().spacecraft.get(id);
  if (!entry?.info) throw new Error(entry ? `Spacecraft "${id}" not propagated` : `No spacecraft "${id}"`);
  return { ...entry.propagation, kvn: false };
}

/**
 * `{request, missing}`: `globe.conjunctions` options for `study`, and the
 * layers left out because they have not loaded. Throws on a missing or
 * unpropagated spacecraft, or nothing to screen.
 */
export function conjunctionRequest(study) {
  const s = settings(study);
  const missing = s.layers.filter((id) => !globe.ephemerisObjects(id));
  const secondaries = [
    ...s.secondaries.map((id) => ({ id, propagation: propagationOf(id) })),
    ...s.layers.filter((id) => !missing.includes(id)).map((layer) => ({ layer })),
  ];
  if (!secondaries.length) throw new Error(missing.length ? `Not loaded: ${missing.join(", ")}` : "Nothing to screen");
  const request = {
    primary: propagationOf(s.primary),
    secondaries,
    thresholdKm: s.thresholdKm,
    hardBodyRadiusM: s.hardBodyRadiusM,
    stepS: s.stepS,
    cdm: true,
  };
  if (s.secondarySigmaKm) request.secondarySigmaKm = s.secondarySigmaKm;
  return { request, missing };
}

/** Runs study `id` now; notifies result listeners. */
export function runConjunction(id) {
  const study = studies.find((candidate) => candidate.id === id);
  if (!study) return;
  const inputs = inputsOf(study);
  const t0 = performance.now();
  try {
    const { request, missing } = conjunctionRequest(study);
    const result = globe.conjunctions(request);
    results.set(id, { inputs, result, missing, ms: performance.now() - t0 });
  } catch (error) {
    results.set(id, { inputs, error: error.message ?? String(error) });
  }
  notify(resultListeners);
}

/** `{result, missing, ms, stale}`, `{error, stale}`, or `null` before the first run. */
export function conjunctionResult(id) {
  const kept = results.get(id);
  const study = studies.find((candidate) => candidate.id === id);
  if (!kept || !study) return null;
  const stale = !same(kept.inputs, inputsOf(study));
  return kept.error ? { error: kept.error, stale } : { result: kept.result, missing: kept.missing, ms: kept.ms, stale };
}

// --- Summaries -------------------------------------------------------------------

/** Conjunctions, highest risk first: Pc, then Pc max, then miss distance. */
export function ranked(result) {
  return [...result.conjunctions].sort(
    (a, b) => (b.pc ?? -1) - (a.pc ?? -1) || b.pcMax - a.pcMax || a.missKm - b.missKm,
  );
}

/** `"1.2e-4"`, `"< 1e-10"` below, `"—"` without. */
export function formatPc(pc) {
  if (pc === null || pc === undefined) return "—";
  if (pc < 1e-10) return "< 1e-10";
  return pc.toExponential(1);
}

export const CONJUNCTION_COLUMNS = [
  "tca_utc",
  "secondary",
  "name",
  "norad_id",
  "miss_m",
  "relative_speed_m_s",
  "radial_m",
  "in_track_m",
  "cross_track_m",
  "sigma_major_m",
  "sigma_minor_m",
  "pc",
  "pc_max",
];

/** CSV rows in `CONJUNCTION_COLUMNS` order, by TCA. */
export function conjunctionRows(result) {
  const m = (km) => km * 1e3;
  return result.conjunctions.map((c) => [
    isoUtc(c.tca),
    c.secondary,
    c.name,
    c.noradId ?? "",
    m(c.missKm),
    m(c.relativeSpeedKmS),
    ...c.relativePositionRswKm.map(m),
    c.sigmaKm ? m(c.sigmaKm[0]) : "",
    c.sigmaKm ? m(c.sigmaKm[1]) : "",
    c.pc ?? "",
    c.pcMax,
  ]);
}
