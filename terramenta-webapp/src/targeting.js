/**
 * Project `targets`: differential-correction problems on a spacecraft's burns,
 * held between open and save like `analysis.js`, and their solutions.
 *
 * - Target: `{id, label?, spacecraft, variables, goals, maxIterations?}`.
 * - Variable: `{burn, parameter: "dv1"|"dv2"|"dv3"|"epoch"}` or
 *   `{finite, parameter: "start"|"duration"}`, burns 1-based, plus
 *   `perturbation?`, `maxStep?` (km/s or s).
 * - Goal: `{quantity, at, value, tolerance}`; `at` `end`, `burnN` (after),
 *   `burnN.before`, `finiteN`, `finiteN.end` or a UTC epoch.
 *
 * `solveTarget(id)` runs `globe.target` on the spacecraft's recorded
 * propagation; results are kept per target, not saved. `applyTarget(id)`
 * re-adds the spacecraft with the solved burns.
 */

import * as globe from "./globe.js";
import { QUANTITIES } from "./reports.js";

/** `QUANTITIES` plus the goal-only ones. */
export const GOAL_QUANTITIES = {
  ...QUANTITIES,
  flightPathAngleDeg: { label: "Flight-path angle", unit: "°", digits: 5 },
  massKg: { label: "Mass", unit: "kg", digits: 3 },
};
export const BURN_PARAMETERS = ["dv1", "dv2", "dv3", "epoch"];
export const FINITE_PARAMETERS = ["start", "duration"];
const AT = /^(end|burn([1-9]\d*)(\.before)?|finite([1-9]\d*)(\.end)?)$/;

let targets = [];
let revision = 0;
/** id → `{revision, entry, result}` or `{revision, entry, error}`; `entry` the spacecraft solved against. */
const results = new Map();
const listeners = new Set();
const solveListeners = new Set();

/** Loads `project.targets`; does not notify. */
export function openTargets(project) {
  targets = structuredClone(project.targets ?? []);
  results.clear();
  revision++;
}

/** `callback()` on any edit (not `openTargets`); returns an unsubscribe. */
export function subscribe(callback) {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

/** `callback()` when a result changes (solve, apply); returns an unsubscribe. */
export function onResult(callback) {
  solveListeners.add(callback);
  return () => solveListeners.delete(callback);
}

function notify(set = listeners) {
  for (const callback of set) callback();
}

export const savedTargets = () => targets;

/** Adds `target`, replacing one with its id; drops its result. */
export function addTarget(target) {
  const copy = structuredClone(target);
  const index = targets.findIndex((other) => other.id === target.id);
  targets = index < 0 ? [...targets, copy] : targets.with(index, copy);
  results.delete(target.id);
  revision++;
  notify();
}

export function removeTarget(id) {
  targets = targets.filter((target) => target.id !== id);
  results.delete(id);
  notify();
}

/** `base` (slugged), or `base-N`, not yet a target id. */
export function uniqueTargetId(base) {
  const root =
    base
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "") || "target";
  const taken = new Set(targets.map((target) => target.id));
  if (!taken.has(root)) return root;
  let n = 2;
  while (taken.has(`${root}-${n}`)) n++;
  return `${root}-${n}`;
}

/** `{targets?}` for saving; targets of spacecraft no longer in the scene dropped. */
export function targetsSection({ spacecraftIds }) {
  const kept = targets.filter((target) => spacecraftIds.has(target.spacecraft));
  return kept.length ? { targets: structuredClone(kept) } : {};
}

/**
 * Reference errors in `project.targets`: duplicate ids, unknown spacecraft,
 * burns the spacecraft does not have, a variable set twice.
 */
export function targetsErrors(project) {
  const errors = [];
  const seen = new Set();
  const spacecraft = new Map((project.spacecraft ?? []).map((entry) => [entry.id, entry.propagation]));
  project.targets?.forEach((target, index) => {
    const path = `targets[${index}]`;
    if (seen.has(target.id)) errors.push(`${path}.id: duplicate "${target.id}"`);
    seen.add(target.id);
    const propagation = spacecraft.get(target.spacecraft);
    if (!propagation) {
      errors.push(`${path}.spacecraft: no spacecraft "${target.spacecraft}"`);
      return;
    }
    const counts = { burn: propagation.maneuvers?.length ?? 0, finite: propagation.finiteBurns?.length ?? 0 };
    const varied = new Set();
    target.variables.forEach((variable, at) => {
      const kind = variable.burn !== undefined ? "burn" : "finite";
      if (variable[kind] > counts[kind]) errors.push(`${path}.variables[${at}].${kind}: no ${kind} ${variable[kind]}`);
      const key = `${kind}${variable[kind]}.${variable.parameter}`;
      if (varied.has(key)) errors.push(`${path}.variables[${at}]: ${key} varied twice`);
      varied.add(key);
    });
    target.goals.forEach((goal, at) => {
      const match = AT.exec(goal.at);
      if (match?.[2] && Number(match[2]) > counts.burn) errors.push(`${path}.goals[${at}].at: no burn ${match[2]}`);
      if (match?.[4] && Number(match[4]) > counts.finite) errors.push(`${path}.goals[${at}].at: no finite burn ${match[4]}`);
    });
  });
  return errors;
}

// --- Solving -------------------------------------------------------------------

/**
 * Last solve of target `id`: `{result}` (`globe.target`'s) or `{error}`, plus
 * `stale` when its spacecraft changed since and `applied` once applied; `null`
 * if never solved or the target was edited since.
 */
export function targetResult(id) {
  const kept = results.get(id);
  const target = targets.find((candidate) => candidate.id === id);
  if (!kept || !target) return null;
  const entry = globe.recordedSources().spacecraft.get(target.spacecraft);
  return { ...kept, stale: kept.entry !== entry };
}

/** Solves target `id` against its spacecraft's current propagation; synchronous. */
export function solveTarget(id) {
  const target = targets.find((candidate) => candidate.id === id);
  if (!target) return null;
  const entry = globe.recordedSources().spacecraft.get(target.spacecraft);
  let kept;
  try {
    if (!entry) throw new Error(`no spacecraft "${target.spacecraft}"`);
    const { variables, goals, maxIterations } = target;
    const options = { variables, goals };
    if (maxIterations !== undefined) options.maxIterations = maxIterations;
    kept = { entry, result: globe.target({ ...entry.propagation, kvn: false }, options) };
  } catch (error) {
    kept = { entry, error: error.message ?? String(error) };
  }
  results.set(id, kept);
  notify(solveListeners);
  return targetResult(id);
}

/**
 * Re-adds target `id`'s spacecraft with its solved burns and `display`
 * (`addSpacecraft` options; default the recorded ones). Throws when there is
 * no current solution or the propagation fails.
 */
export function applyTarget(id, display) {
  const target = targets.find((candidate) => candidate.id === id);
  const solved = targetResult(id);
  if (!target || !solved?.result || solved.stale) throw new Error("solve first");
  const { result } = solved;
  const entry = globe.recordedSources().spacecraft.get(target.spacecraft);
  const propagation = { ...entry.propagation };
  if (result.maneuvers.length) propagation.maneuvers = result.maneuvers;
  if (result.finiteBurns.length) propagation.finiteBurns = result.finiteBurns;
  try {
    globe.addSpacecraft(target.spacecraft, propagation, display ?? entry.display);
    // The result now describes the spacecraft as applied.
    results.set(id, { entry: globe.recordedSources().spacecraft.get(target.spacecraft), result, applied: true });
  } finally {
    notify(solveListeners);
  }
}

/** `"Burn 2 · dv1"`-style name of a variable. */
export function variableName(variable) {
  return variable.burn !== undefined
    ? `Burn ${variable.burn} · ${variable.parameter}`
    : `Finite ${variable.finite} · ${variable.parameter}`;
}

/** Variable value from `propagation`: Δv km/s, duration s; epoch/start offsets are 0. */
export function variableValue(variable, propagation) {
  if (variable.burn !== undefined) {
    const maneuver = propagation.maneuvers?.[variable.burn - 1];
    if (!maneuver) return Number.NaN;
    return variable.parameter === "epoch" ? 0 : maneuver.deltaVKmS[Number(variable.parameter[2]) - 1];
  }
  const burn = propagation.finiteBurns?.[variable.finite - 1];
  if (!burn) return Number.NaN;
  return variable.parameter === "start" ? 0 : burn.durationS;
}

/** `"After burn 2"`-style name of a goal's `at`. */
export function atName(at) {
  const match = AT.exec(at);
  if (!match) return at;
  if (match[1] === "end") return "End";
  if (match[2]) return `${match[3] ? "Before" : "After"} burn ${match[2]}`;
  return `${match[5] ? "End of" : "Start of"} finite ${match[4]}`;
}
