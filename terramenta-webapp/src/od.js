/**
 * Project `analysis.orbitDetermination`: orbit determination studies (Analysis
 * tab), held between open and save like `conjunction.js`, run with
 * `globe.determineOrbit`.
 *
 * - Study: `{id, label?, spacecraft, estimator?, stations?, tdm?, simulate?:
 *   {truth, tracking: [{type, station?, sigma?, bias?}], stepS?, seed?},
 *   solveFor?: [{parameter, sigma}], editSigma?, lightTime?}`. Defaults in
 *   `OD_DEFAULTS`.
 * - `spacecraft`: its propagation is the a priori; `propagation.covariance` the
 *   a priori covariance (required by the EKF) and process noise.
 * - Observations: `tdm` (URL, relative to the app root, or `data:`) and/or
 *   `simulate` (tracking of spacecraft `truth`). `stations`: ground station
 *   ids, default every station.
 * - Units: `sigma`, `bias` in km, km/s, deg (`globe.determineOrbit`).
 *
 * Results are kept per study until it, its spacecraft entries or the stations
 * change; not saved. `applyEstimate` re-adds the a priori spacecraft from the
 * estimate.
 */

import { eventsRevision, savedStations, stationOptions } from "./events.js";
import * as globe from "./globe.js";
import { isoUtc } from "./reports.js";

export const OD_DEFAULTS = { estimator: "batch", lightTime: true };
export const ESTIMATORS = { batch: "Batch least squares", ekf: "Extended Kalman filter" };
export const OBSERVATION_TYPES = {
  range: { label: "Range", unit: "m", scale: 1e3, station: true },
  rangeRate: { label: "Range rate", unit: "mm/s", scale: 1e6, station: true },
  azimuth: { label: "Azimuth", unit: "″", scale: 3600, station: true },
  elevation: { label: "Elevation", unit: "″", scale: 3600, station: true },
  rightAscension: { label: "Right ascension", unit: "″", scale: 3600, station: true },
  declination: { label: "Declination", unit: "″", scale: 3600, station: true },
  positionX: { label: "GNSS x", unit: "m", scale: 1e3, station: false },
  positionY: { label: "GNSS y", unit: "m", scale: 1e3, station: false },
  positionZ: { label: "GNSS z", unit: "m", scale: 1e3, station: false },
};
export const PARAMETERS = { dragCoefficient: "Cd", reflectivity: "Cr" };

let studies = [];
/** id → `{inputs, result, ms, applied?} | {inputs, error}`. */
const results = new Map();
/** URL → TDM text. */
const fetched = new Map();
const running = new Set();
const listeners = new Set();
const resultListeners = new Set();

/** Loads `project.analysis.orbitDetermination`; does not notify. Call after `globe.reset`. */
export function openOrbitDetermination(project) {
  studies = structuredClone(project.analysis?.orbitDetermination ?? []);
  results.clear();
  running.clear();
}

/** `callback()` on any edit (not `openOrbitDetermination`); returns an unsubscribe. */
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
export const settings = (study) => ({ ...OD_DEFAULTS, solveFor: [], ...study });
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
      .replace(/^-|-$/g, "") || "od";
  const taken = new Set(studies.map((study) => study.id));
  if (!taken.has(root)) return root;
  let n = 2;
  while (taken.has(`${root}-${n}`)) n++;
  return `${root}-${n}`;
}

/**
 * Studies for `analysis.orbitDetermination`: absent stations and their
 * tracking dropped; studies whose spacecraft is gone, or left without
 * observations, dropped.
 */
export function odStudies({ spacecraftIds, stationIds }) {
  return structuredClone(
    studies
      .filter((study) => spacecraftIds.has(study.spacecraft))
      .map((study) => {
        let out = study;
        if (out.stations) {
          const { stations, ...rest } = out;
          const left = stations.filter((id) => stationIds.has(id));
          out = left.length ? { ...rest, stations: left } : rest;
        }
        if (out.simulate) {
          const { simulate, ...rest } = out;
          const tracking = simulate.tracking.filter((t) => t.station === undefined || stationIds.has(t.station));
          out = spacecraftIds.has(simulate.truth) && tracking.length ? { ...rest, simulate: { ...simulate, tracking } } : rest;
        }
        return out;
      })
      .filter((study) => study.tdm || study.simulate),
  );
}

/** Reference errors in `project.analysis.orbitDetermination` (paths from `analysis`). */
export function odErrors(project, { spacecraftIds }) {
  const stationIds = new Set((project.groundStations ?? []).map((station) => station.id));
  const errors = [];
  const seen = new Set();
  (project.analysis?.orbitDetermination ?? []).forEach((study, index) => {
    const path = `orbitDetermination[${index}]`;
    if (seen.has(study.id)) errors.push(`${path}.id: duplicate "${study.id}"`);
    seen.add(study.id);
    if (!spacecraftIds.has(study.spacecraft)) errors.push(`${path}.spacecraft: no spacecraft "${study.spacecraft}"`);
    (study.stations ?? []).forEach((id, at) => {
      if (!stationIds.has(id)) errors.push(`${path}.stations[${at}]: no ground station "${id}"`);
    });
    const sim = study.simulate;
    if (sim) {
      if (!spacecraftIds.has(sim.truth)) errors.push(`${path}.simulate.truth: no spacecraft "${sim.truth}"`);
      else if (sim.truth === study.spacecraft) errors.push(`${path}.simulate.truth: the a priori spacecraft itself`);
      sim.tracking.forEach((t, at) => {
        if (t.station !== undefined && !stationIds.has(t.station)) {
          errors.push(`${path}.simulate.tracking[${at}].station: no ground station "${t.station}"`);
        }
        if (t.station !== undefined && !OBSERVATION_TYPES[t.type]?.station) {
          errors.push(`${path}.simulate.tracking[${at}]: ${t.type} takes no station`);
        }
      });
    }
    if (!study.tdm && !sim) errors.push(`${path}: needs tdm or simulate`);
    if (study.estimator === "ekf" && study.solveFor?.length) errors.push(`${path}.solveFor: batch only`);
  });
  return errors;
}

// --- Running ---------------------------------------------------------------------

function entryOf(id) {
  const entry = globe.recordedSources().spacecraft.get(id);
  if (!entry?.info) throw new Error(entry ? `Spacecraft "${id}" not propagated` : `No spacecraft "${id}"`);
  return entry;
}

/** What a result depends on: the study (not its label), its spacecraft entries, the stations. */
function inputsOf(study) {
  const entries = globe.recordedSources().spacecraft;
  const { label: _label, ...rest } = study;
  return [JSON.stringify(rest), entries.get(study.spacecraft), entries.get(study.simulate?.truth), eventsRevision()];
}

const same = (a, b) => a?.length === b.length && a.every((value, index) => value === b[index]);

/**
 * `globe.determineOrbit` options for `study`, with `tdm` text when the study
 * reads one. Throws on a missing or unpropagated spacecraft, or no stations
 * for station tracking.
 */
export function odRequest(study, tdm) {
  const s = settings(study);
  const stations = s.stations ? savedStations().filter((station) => s.stations.includes(station.id)) : savedStations();
  const request = {
    propagation: { ...entryOf(s.spacecraft).propagation, kvn: false },
    stations: stationOptions(stations),
    estimator: s.estimator,
    lightTime: s.lightTime,
    exportTdm: true,
  };
  if (tdm !== undefined) request.tdm = tdm;
  if (s.simulate) {
    const { truth, ...simulate } = s.simulate;
    if (simulate.tracking.some((t) => OBSERVATION_TYPES[t.type]?.station) && !stations.length) {
      throw new Error("Station tracking needs ground stations");
    }
    request.simulate = { ...simulate, truth: { ...entryOf(truth).propagation, kvn: false } };
  }
  if (s.solveFor.length) request.solveFor = s.solveFor;
  if (s.editSigma !== undefined) request.editSigma = s.editSigma;
  return request;
}

async function tdmText(url) {
  if (!fetched.has(url)) {
    const response = await fetch(new URL(url, document.baseURI));
    if (!response.ok) throw new Error(`TDM ${url}: HTTP ${response.status}`);
    fetched.set(url, await response.text());
  }
  return fetched.get(url);
}

/** Runs study `id` (fetching its TDM first); notifies result listeners when it starts and ends. */
export async function runOrbitDetermination(id) {
  const study = studies.find((candidate) => candidate.id === id);
  if (!study || running.has(id)) return;
  running.add(id);
  notify(resultListeners);
  const inputs = inputsOf(study);
  try {
    const tdm = study.tdm ? await tdmText(study.tdm) : undefined;
    const t0 = performance.now();
    const result = globe.determineOrbit(odRequest(study, tdm));
    results.set(id, { inputs, result, ms: performance.now() - t0 });
  } catch (error) {
    results.set(id, { inputs, error: error.message ?? String(error) });
  } finally {
    running.delete(id);
    notify(resultListeners);
  }
}

/** `{result, ms, stale, applied}`, `{error, stale}`, or `null` before the first run. */
export function odResult(id) {
  const kept = results.get(id);
  const study = studies.find((candidate) => candidate.id === id);
  if (!kept || !study) return null;
  const stale = !same(kept.inputs, inputsOf(study));
  return kept.error ? { error: kept.error, stale } : { result: kept.result, ms: kept.ms, stale, applied: Boolean(kept.applied) };
}

/**
 * Propagation of study `id`'s spacecraft from its estimate: initial state,
 * GCRF covariance, estimated vehicle parameters; the a priori span kept. A priori
 * process noise is kept, now on GCRF axes.
 */
export function estimatedPropagation(id) {
  const study = studies.find((candidate) => candidate.id === id);
  const kept = odResult(id);
  if (!study || !kept?.result || kept.stale) throw new Error("run first");
  const { estimate } = kept.result;
  const prior = entryOf(study.spacecraft).propagation;
  const propagation = { ...prior, initial: estimate.initial, vehicle: { ...prior.vehicle, ...estimate.vehicle } };
  const noise = prior.covariance?.processNoiseKm2S3;
  propagation.covariance = noise ? { ...estimate.covariance, frame: "gcrf", processNoiseKm2S3: noise } : estimate.covariance;
  const start = prior.start ?? prior.initial.epoch;
  if (start && start !== estimate.initial.epoch) propagation.start = start;
  return propagation;
}

/** Re-adds study `id`'s spacecraft from its estimate with `display` (default the recorded one). */
export function applyEstimate(id, display) {
  const study = studies.find((candidate) => candidate.id === id);
  const propagation = estimatedPropagation(id);
  const entry = entryOf(study.spacecraft);
  const kept = results.get(id);
  try {
    globe.addSpacecraft(study.spacecraft, propagation, display ?? entry.display);
    results.set(id, { ...kept, inputs: inputsOf(study), applied: true });
  } finally {
    notify(resultListeners);
  }
}

// --- Summaries -------------------------------------------------------------------

/** `"1.2 m"`-style value of `type` given in its globe unit. */
export function formatResidual(type, value) {
  const { unit, scale } = OBSERVATION_TYPES[type];
  const v = value * scale;
  const digits = Math.abs(v) >= 100 ? 0 : Math.abs(v) >= 10 ? 1 : 2;
  return `${v.toFixed(digits)} ${unit}`;
}

/** Per station and type: `{station, type, count, edited, mean, rms}`, accepted residuals, globe units. */
export function residualStats(result) {
  const groups = new Map();
  for (const r of result.residuals) {
    const key = `${r.station ?? ""}\u0000${r.type}`;
    if (!groups.has(key)) groups.set(key, { station: r.station ?? null, type: r.type, count: 0, edited: 0, sum: 0, sum2: 0 });
    const g = groups.get(key);
    g.count++;
    if (r.edited) {
      g.edited++;
      continue;
    }
    const value = r.postFit ?? r.residual;
    g.sum += value;
    g.sum2 += value * value;
  }
  return [...groups.values()].map(({ sum, sum2, ...g }) => {
    const n = g.count - g.edited;
    return { ...g, mean: n ? sum / n : Number.NaN, rms: n ? Math.sqrt(sum2 / n) : Number.NaN };
  });
}

/** Normalised residual series per type: `{type, points: [[t, residual/σ]]}`; edited points left out. */
export function normalisedResiduals(result) {
  const series = new Map();
  for (const r of result.residuals) {
    if (r.edited) continue;
    if (!series.has(r.type)) series.set(r.type, []);
    series.get(r.type).push([r.time, (r.postFit ?? r.residual) / r.sigma]);
  }
  return [...series].map(([type, points]) => ({ type, points }));
}

export const RESIDUAL_COLUMNS = ["time_utc", "station", "type", "observed", "residual", "post_fit", "sigma", "edited"];

/** CSV rows in `RESIDUAL_COLUMNS` order; values in globe units (km, km/s, deg). */
export function residualRows(result) {
  return result.residuals.map((r) => [
    isoUtc(r.time),
    r.station ?? "",
    r.type,
    r.observed,
    r.residual,
    r.postFit ?? "",
    r.sigma,
    r.edited ? 1 : 0,
  ]);
}
