/**
 * Project `attitudes`: one attitude profile per spacecraft, held between open
 * and save like `targeting.js`, sampled with `globe.attitude` and drawn on the
 * spacecraft's layer with `globe.showAttitude`.
 *
 * - Profile: `{spacecraft, label?, quaternion | primary + secondary, spin?,
 *   station? | target?, sensors?, stepS?, axes?}`.
 * - Pointing: `{body: [x, y, z], to}`; `to` a `DIRECTIONS` key or a GCRF vector.
 *   `primary` is exact; `secondary` as close as the primary allows.
 * - Sensor: `{label?, body, to?, color?, draw?, fov?}`; its angle to `to` is
 *   sampled and, unless `draw: false`, its boresight drawn. `fov`:
 *   `{halfAngleDeg}` (cone) or `{halfXDeg, halfYDeg, reference?}` (rectangle,
 *   width along body `reference`); its footprint is drawn and `coverage.js`
 *   counts access through it.
 * - `target` direction: ground station `station` (by id) or site `target`.
 *
 * `syncAttitudes()` (every state) re-samples a profile when it, its spacecraft
 * or its station changes, and redraws. Samples are not saved.
 */

import { savedStations } from "./events.js";
import * as globe from "./globe.js";

export const DIRECTIONS = {
  nadir: "Nadir",
  zenith: "Zenith",
  velocity: "Velocity",
  antiVelocity: "Anti-velocity",
  orbitNormal: "Orbit normal",
  antiOrbitNormal: "Anti-normal",
  sun: "Sun",
  antiSun: "Anti-sun",
  target: "Target",
};
export const SENSOR_COLOR = "#ffcc33";
export const DEFAULT_STEP_S = 60;

let profiles = [];
/** spacecraft id → `{profile, entry, site, result | error}`; `profile`/`entry` as sampled. */
const results = new Map();
/** Spacecraft ids with a drawing up. */
const drawn = new Set();
const listeners = new Set();
const resultListeners = new Set();

/** Loads `project.attitudes`; does not notify. Call after `globe.reset`. */
export function openAttitudes(project) {
  profiles = structuredClone(project.attitudes ?? []);
  results.clear();
  drawn.clear();
}

/** `callback()` on any edit (not `openAttitudes`); returns an unsubscribe. */
export function subscribe(callback) {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

/** `callback()` when samples change; returns an unsubscribe. */
export function onResult(callback) {
  resultListeners.add(callback);
  return () => resultListeners.delete(callback);
}

function notify(set = listeners) {
  for (const callback of set) callback();
}

export const savedAttitudes = () => profiles;

/** Adds `profile`, replacing its spacecraft's. */
export function setAttitude(profile) {
  const copy = structuredClone(profile);
  const index = profiles.findIndex((other) => other.spacecraft === profile.spacecraft);
  profiles = index < 0 ? [...profiles, copy] : profiles.with(index, copy);
  notify();
}

export function removeAttitude(spacecraft) {
  profiles = profiles.filter((profile) => profile.spacecraft !== spacecraft);
  notify();
}

/** `{attitudes?}` for saving; profiles of spacecraft no longer in the scene dropped. */
export function attitudesSection({ spacecraftIds }) {
  const kept = profiles.filter((profile) => spacecraftIds.has(profile.spacecraft));
  return kept.length ? { attitudes: structuredClone(kept) } : {};
}

/**
 * Reference errors in `project.attitudes`: duplicate or unknown spacecraft,
 * unknown station, `quaternion` with `primary`/`secondary`, `target` direction
 * without a site.
 */
export function attitudesErrors(project) {
  const errors = [];
  const seen = new Set();
  const spacecraft = new Set((project.spacecraft ?? []).map((entry) => entry.id));
  const stations = new Set((project.groundStations ?? []).map((station) => station.id));
  project.attitudes?.forEach((profile, index) => {
    const path = `attitudes[${index}]`;
    if (seen.has(profile.spacecraft)) errors.push(`${path}.spacecraft: second profile for "${profile.spacecraft}"`);
    seen.add(profile.spacecraft);
    if (!spacecraft.has(profile.spacecraft)) errors.push(`${path}.spacecraft: no spacecraft "${profile.spacecraft}"`);
    if (profile.quaternion && (profile.primary || profile.secondary)) {
      errors.push(`${path}: quaternion or primary/secondary, not both`);
    }
    if (profile.station !== undefined && profile.target) errors.push(`${path}: station or target, not both`);
    if (profile.station !== undefined && !stations.has(profile.station)) {
      errors.push(`${path}.station: no ground station "${profile.station}"`);
    }
    const toTarget = [profile.primary, profile.secondary, ...(profile.sensors ?? [])].some((p) => p?.to === "target");
    if (toTarget && profile.station === undefined && !profile.target) {
      errors.push(`${path}: direction "target" needs a station or target`);
    }
  });
  return errors;
}

// --- Sampling --------------------------------------------------------------------

/** `globe.attitude` site for `profile`: its station's or its own; `null` if none. */
function siteOf(profile) {
  if (profile.target) return profile.target;
  if (profile.station === undefined) return null;
  const station = savedStations().find((candidate) => candidate.id === profile.station);
  return station ? { latDeg: station.latDeg, lonDeg: station.lonDeg, altKm: station.altKm ?? 0 } : undefined;
}

/** `globe.attitude` options for `profile`. */
export function attitudeOptions(profile, { aem = false } = {}) {
  const options = { stepS: profile.stepS ?? DEFAULT_STEP_S, aem };
  if (profile.quaternion) options.quaternion = profile.quaternion;
  else Object.assign(options, { primary: profile.primary, secondary: profile.secondary });
  if (profile.spin) options.spin = profile.spin;
  const site = siteOf(profile);
  if (site) options.target = site;
  options.sensors = angledSensors(profile).map(({ body, to }) => ({ body, to }));
  return options;
}

/** `profile`'s sensors with a direction: the ones `globe.attitude` reports angles for, in order. */
export const angledSensors = (profile) => (profile.sensors ?? []).filter((sensor) => sensor.to !== undefined);

/**
 * Samples and draws every profile whose spacecraft, profile or station changed
 * since; hides drawings left without a profile or spacecraft. Cheap when
 * nothing changed.
 */
export function syncAttitudes() {
  const spacecraft = globe.recordedSources().spacecraft;
  let changed = false;
  for (const profile of profiles) {
    const id = profile.spacecraft;
    const entry = spacecraft.get(id);
    const site = JSON.stringify(siteOf(profile) ?? null);
    const kept = results.get(id);
    if (kept && kept.profile === profile && kept.entry === entry && kept.site === site) continue;
    changed = true;
    if (!entry?.info) {
      results.set(id, { profile, entry, site, error: entry ? "Spacecraft not propagated" : `No spacecraft "${id}"` });
      hide(id);
      continue;
    }
    try {
      if (siteOf(profile) === undefined) throw new Error(`no ground station "${profile.station}"`);
      const result = globe.attitude({ ...entry.propagation, kvn: false }, attitudeOptions(profile));
      results.set(id, { profile, entry, site, result });
      globe.showAttitude(id, {
        axes: profile.axes ?? true,
        sensors: (profile.sensors ?? [])
          .filter((sensor) => sensor.draw !== false)
          .map((sensor) => ({
            body: sensor.body,
            color: sensor.color ?? SENSOR_COLOR,
            ...(sensor.fov ? { fov: sensor.fov } : {}),
          })),
      });
      drawn.add(id);
    } catch (error) {
      results.set(id, { profile, entry, site, error: error.message ?? String(error) });
      hide(id);
    }
  }
  for (const id of [...results.keys()]) {
    if (profiles.some((profile) => profile.spacecraft === id)) continue;
    results.delete(id);
    hide(id);
    changed = true;
  }
  if (changed) notify(resultListeners);
}

function hide(id) {
  if (drawn.delete(id)) globe.hideAttitude(id);
}

/** Last samples of spacecraft `id`'s profile: `{result}` or `{error}`; `null` before the first sync. */
export function attitudeResult(id) {
  const kept = results.get(id);
  return kept ? (kept.error ? { error: kept.error } : { result: kept.result }) : null;
}

/** CCSDS AEM KVN for spacecraft `id`'s profile; throws on failure. */
export function attitudeAem(id) {
  const profile = profiles.find((candidate) => candidate.spacecraft === id);
  const entry = globe.recordedSources().spacecraft.get(id);
  if (!profile || !entry?.info) throw new Error(`no attitude for "${id}"`);
  return globe.attitude({ ...entry.propagation, kvn: false }, attitudeOptions(profile, { aem: true })).aem;
}

// --- Names -----------------------------------------------------------------------

const AXIS_NAMES = ["X", "Y", "Z"];

/** `"+Z"` for a signed unit axis, else `"[x, y, z]"`. */
export function axisName(vector) {
  const nonzero = vector.map((value, index) => [value, index]).filter(([value]) => value !== 0);
  if (nonzero.length === 1) return `${nonzero[0][0] > 0 ? "+" : "−"}${AXIS_NAMES[nonzero[0][1]]}`;
  return `[${vector.join(", ")}]`;
}

/** `"cone 20°"`, `"20° × 5°"`; `""` without a field of view. */
export function fovName(fov) {
  if (!fov) return "";
  return fov.halfAngleDeg !== undefined ? `cone ${fov.halfAngleDeg}°` : `${fov.halfXDeg}° × ${fov.halfYDeg}°`;
}

/** `"Nadir"`, or `"[x, y, z]"` for a GCRF vector. */
export const directionName = (to) => (Array.isArray(to) ? `GCRF [${to.join(", ")}]` : (DIRECTIONS[to] ?? to));

/** `"+Z → Nadir, +X → Velocity · spin 2°/s"`-style summary. */
export function lawName(profile) {
  const pointing = ({ body, to }) => `${axisName(body)} → ${directionName(to)}`;
  const law = profile.quaternion
    ? `Inertial [${profile.quaternion.map((value) => +value.toFixed(4)).join(", ")}]`
    : `${pointing(profile.primary)}, ${pointing(profile.secondary)}`;
  const spin = profile.spin ? ` · spin ${profile.spin.rateDegS}°/s about ${axisName(profile.spin.axis)}` : "";
  return law + spin;
}
