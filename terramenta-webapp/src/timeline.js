/**
 * Mission timeline: events on the simulated clock, and the runner that applies
 * their actions.
 *
 * Events:
 * - Scene, no actions: `<spacecraft>.start`, `.end`, `.burnN` (impulsive),
 *   `.finiteN`, `.finiteN.end` (from `globe.recordedSources().spacecraft`);
 *   searched events (`events.js` `eventTimeline`): `.aos.<station>.N`,
 *   `.los.<station>.N`, `.umbraN[.end]`, `.penumbraN[.end]`, `.periapsisN`,
 *   `.apoapsisN`, `.ascendingNodeN`, `.descendingNodeN`;
 *   `<mission>.departure`, `.escape`, `.encounter`, `.flyby`, `.exit`,
 *   `.arrival`, `.capture` (`state.missions`, `null` until known; the flyby
 *   three only with `via`).
 * - Project `timeline.events`: `{id, label, at, actions}`; `at` is a date-time
 *   or `{event, offsetS}` naming a scene event or another project event.
 * - Director (`director.js`): generated from `project.director`, not saved.
 *
 * Directed (`timeline.directed`, default true): when the latest event at or
 * before the clock changes (played past, scrubbed, jumped), the baseline
 * (project `clock`, `view`, `frame`) and the actions of every event up to it
 * are merged. A key is applied if its merged value changed since the last
 * application or the latest event sets it, so manual control holds until an
 * event changes that key. `camera` applies from the latest event only.
 * `timeScale` (or `"max"`) keeps the clock's direction; `rampDoublingS`
 * doubles it every that many wall-clock seconds up to the maximum, until the
 * next change. A view request repeats until it lands.
 *
 * `skipTo` (project, else director): once it resolves, a clock before it jumps
 * forward to it, once per open.
 */

import { DIRECTORS } from "./director.js";
import { eventTimeline } from "./events.js";
import { utcSeconds } from "./format.js";
import * as globe from "./globe.js";
import { flownShape } from "./sequence.js";

export const MISSION_PHASES = ["departure", "escape", "encounter", "flyby", "exit", "arrival", "capture"];
const REFERENCE = /^([^.]+)\.(start|end|burn([1-9]\d*)|finite([1-9]\d*)(\.end)?|departure|escape|encounter|flyby|exit|arrival|capture)$/;
/** Searched events; counts are known only after propagation, so unchecked. */
const SEARCHED = /^([^.]+)\.(?:(?:aos|los)\.([^.]+)\.[1-9]\d*|(?:umbra|penumbra)[1-9]\d*(?:\.end)?|(?:periapsis|apoapsis|ascendingNode|descendingNode)[1-9]\d*)$/;

let events = [];
let directed = true;
let skipTo;
let director = { events: [] };
let baseline = {};
let run = freshRun();
const listeners = new Set();

function freshRun() {
  return { active: undefined, applied: { ...baseline }, pendingView: null, ramp: null, skipped: false };
}

/** Loads `project.timeline` and its director; resets the runner. */
export function openTimeline(project) {
  const timeline = project.timeline ?? {};
  events = structuredClone(timeline.events ?? []);
  directed = timeline.directed ?? true;
  skipTo = timeline.skipTo;
  director = project.director ? DIRECTORS[project.director.type].events(project.director) : { events: [] };
  baseline = baselineOf(project, [...events, ...director.events]);
  run = freshRun();
}

function baselineOf(project, scripted) {
  const base = {};
  const clock = project.clock ?? {};
  if (clock.timeScale !== undefined) base.timeScale = Math.abs(clock.timeScale);
  if (clock.paused !== undefined || scripted.some((event) => event.actions?.paused !== undefined)) {
    base.paused = clock.paused ?? false;
  }
  if (project.view) base.view = project.view;
  if (project.frame) base.frame = project.frame;
  return base;
}

/** `callback()` on any edit (not `openTimeline`); returns an unsubscribe. */
export function subscribe(callback) {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

function notify() {
  for (const callback of listeners) callback();
}

// --- Editing -------------------------------------------------------------------

export const isDirected = () => directed;

export function setDirected(value) {
  directed = value;
  run.active = undefined;
  run.ramp = null;
  notify();
}

/** Adds a project event (`{id, label, at, actions}`), replacing one with its id. */
export function addEvent(event) {
  events = [...events.filter((other) => other.id !== event.id), structuredClone(event)];
  run.active = undefined;
  notify();
}

export function removeEvent(id) {
  events = events.filter((event) => event.id !== id);
  run.active = undefined;
  notify();
}

/** `base`, or `base-N`, not yet a project or director event id. */
export function uniqueEventId(base) {
  const taken = new Set([...events, ...director.events].map((event) => event.id));
  if (!taken.has(base)) return base;
  let n = 2;
  while (taken.has(`${base}-${n}`)) n++;
  return `${base}-${n}`;
}

/**
 * `project.timeline` for saving, events whose references no longer hold
 * dropped; `undefined` if empty. `scene`: `{spacecraft, stationIds, missionIds, layerIds}`.
 */
export function timelineSection(scene) {
  let timeline = { events: structuredClone(events) };
  if (skipTo !== undefined) timeline.skipTo = structuredClone(skipTo);
  for (;;) {
    const found = problems(timeline, scene);
    if (!found.length) break;
    const dropped = new Set(found.map((problem) => problem.index));
    if (dropped.has("skipTo")) delete timeline.skipTo;
    timeline.events = timeline.events.filter((_, index) => !dropped.has(index));
  }
  if (!directed) timeline = { directed, ...timeline };
  if (!timeline.events.length) delete timeline.events;
  return Object.keys(timeline).length ? timeline : undefined;
}

// --- Validation ----------------------------------------------------------------

/**
 * Reference errors in `project.timeline` (paths from `timeline`); the project
 * already matches the schema. `layerIds`: ephemeris and spacecraft ids.
 */
export function timelineErrors(project, { missionIds, layerIds }) {
  if (!project.timeline) return [];
  const spacecraft = new Map((project.spacecraft ?? []).map((entry) => [entry.id, flownShape(entry)]));
  const stationIds = new Set((project.groundStations ?? []).map((station) => station.id));
  return problems(project.timeline, { spacecraft, stationIds, missionIds, layerIds }).map(
    ({ path, message }) => `${path}: ${message}`,
  );
}

/** `[{index, path, message}]`; `index` is the event's, or `"skipTo"`. */
function problems(timeline, { spacecraft, stationIds = new Set(), missionIds, layerIds }) {
  const found = [];
  const list = timeline.events ?? [];
  const byId = new Map();
  list.forEach((event, index) => {
    if (byId.has(event.id)) found.push({ index, path: `events[${index}].id`, message: `duplicate "${event.id}"` });
    else byId.set(event.id, event);
  });

  const reference = (at, index, path) => {
    if (typeof at === "string") return;
    const message = referenceProblem(at.event, { spacecraft, stationIds, missionIds, eventIds: byId });
    if (message) found.push({ index, path: `${path}.event`, message });
  };
  if (timeline.skipTo !== undefined) reference(timeline.skipTo, "skipTo", "skipTo");

  list.forEach((event, index) => {
    const path = `events[${index}]`;
    reference(event.at, index, `${path}.at`);
    const seen = new Set([event.id]);
    for (let at = event.at; typeof at !== "string" && byId.has(at.event); at = byId.get(at.event).at) {
      if (seen.has(at.event)) {
        found.push({ index, path: `${path}.at.event`, message: `cycle through "${at.event}"` });
        break;
      }
      seen.add(at.event);
    }
    const { focus, follow } = event.actions ?? {};
    if (focus && !layerIds.has(focus.layer)) {
      found.push({ index, path: `${path}.actions.focus.layer`, message: `no ephemeris layer or spacecraft "${focus.layer}"` });
    }
    if (follow !== undefined && !missionIds.has(follow)) {
      found.push({ index, path: `${path}.actions.follow`, message: `no mission "${follow}"` });
    }
  });
  return found;
}

function referenceProblem(name, { spacecraft, stationIds, missionIds, eventIds }) {
  const searched = SEARCHED.exec(name);
  if (searched && !eventIds.has(name)) {
    const [, id, station] = searched;
    if (!spacecraft.has(id)) return `no spacecraft "${id}"`;
    if (station !== undefined && !stationIds.has(station)) return `no ground station "${station}"`;
    return null;
  }
  const match = REFERENCE.exec(name);
  if (!match) return eventIds.has(name) ? null : `no event "${name}"`;
  const [, id, kind, burn, finite] = match;
  const phase = MISSION_PHASES.includes(kind);
  if (spacecraft.has(id)) {
    const { maneuvers = [], finiteBurns = [] } = spacecraft.get(id);
    if (phase) return `"${kind}" is a mission event; "${id}" is a spacecraft`;
    if (burn && Number(burn) > maneuvers.length) return `spacecraft "${id}" has ${maneuvers.length} impulsive burns`;
    if (finite && Number(finite) > finiteBurns.length) return `spacecraft "${id}" has ${finiteBurns.length} finite burns`;
    return null;
  }
  if (missionIds.has(id)) return phase ? null : `"${kind}" is a spacecraft event; "${id}" is a mission`;
  return `no spacecraft or mission "${id}"`;
}

// --- Resolution ----------------------------------------------------------------

/**
 * Scene events: `[{id, label, source, unixSeconds}]`, `unixSeconds` `null`
 * until known. `spacecraft`: `globe.recordedSources().spacecraft`.
 */
export function sceneEvents(state, spacecraft = globe.recordedSources().spacecraft) {
  const found = [];
  const add = (id, label, source, unixSeconds) =>
    found.push({ id, label, source, unixSeconds: Number.isFinite(unixSeconds) ? unixSeconds : null });

  for (const [id, entry] of spacecraft) {
    const { propagation, display, info } = entry;
    const name = display.label ?? propagation.name ?? id;
    const start = info ? utcSeconds(info.start) : seconds(propagation.start ?? propagation.initial.epoch);
    const end = info
      ? utcSeconds(info.stop)
      : propagation.stop !== undefined
        ? seconds(propagation.stop)
        : after(start, propagation.durationS);
    add(`${id}.start`, `${name}: start`, "spacecraft", start);
    add(`${id}.end`, `${name}: end`, "spacecraft", end);
    propagation.maneuvers?.forEach((maneuver, index) => {
      const metres = Math.hypot(...maneuver.deltaVKmS) * 1000;
      add(`${id}.burn${index + 1}`, `${name}: burn ${index + 1} · ${metres.toFixed(1)} m/s`, "spacecraft", seconds(maneuver.epoch));
    });
    propagation.finiteBurns?.forEach((burn, index) => {
      const from = seconds(burn.start);
      add(`${id}.finite${index + 1}`, `${name}: finite burn ${index + 1} · ${burn.thrustN} N`, "spacecraft", from);
      add(`${id}.finite${index + 1}.end`, `${name}: finite burn ${index + 1} end`, "spacecraft", after(from, burn.durationS));
    });
    for (const event of eventTimeline(id, name, entry)) add(event.id, event.label, "event", event.unixSeconds);
  }

  for (const mission of state?.missions ?? []) {
    for (const phase of mission.phases ?? []) {
      add(`${mission.id}.${phase.id}`, `${mission.id}: ${phase.label}`, "mission", phase.unixSeconds);
    }
  }
  return found;
}

/**
 * Every event, resolved and sorted by time (unknown last):
 * `[{id, label, source, unixSeconds, at?, actions?}]`, `source` one of
 * `spacecraft`, `event`, `mission`, `director`, `project`.
 */
export function timelineEvents(state, spacecraft) {
  return resolver(state, spacecraft).all;
}

function resolver(state, spacecraft) {
  const scene = sceneEvents(state, spacecraft);
  const scripted = [
    ...director.events.map((event) => ({ ...event, source: "director" })),
    ...events.map((event) => ({ ...event, source: "project" })),
  ];
  const times = new Map(scene.map((event) => [event.id, event.unixSeconds]));
  const byId = new Map(scripted.map((event) => [event.id, event]));
  const resolving = new Set();

  const resolve = (at) => {
    if (typeof at === "string") return seconds(at);
    let base = null;
    if (times.has(at.event)) base = times.get(at.event);
    else if (byId.has(at.event) && !resolving.has(at.event)) {
      resolving.add(at.event);
      base = resolve(byId.get(at.event).at);
      resolving.delete(at.event);
      times.set(at.event, base);
    }
    return after(base, at.offsetS ?? 0);
  };

  const all = [
    ...scene,
    ...scripted.map((event) => ({ ...event, label: event.label ?? event.id, unixSeconds: resolve(event.at) })),
  ];
  all.sort((a, b) => (a.unixSeconds ?? Infinity) - (b.unixSeconds ?? Infinity));
  return { all, resolve };
}

// --- Running -------------------------------------------------------------------

/** Applies the timeline against a state snapshot; call per snapshot, inside `globe.quietly`. */
export function runTimeline(state, spacecraft) {
  if (!directed) {
    run.skipped = true;
    return;
  }
  const { all, resolve } = resolver(state, spacecraft);
  const now = state.sun.unixSeconds;

  if (!run.skipped) {
    const target = skipTo ?? director.skipTo;
    const at = target === undefined ? null : resolve(target);
    if (target === undefined || at !== null) run.skipped = true;
    if (at !== null && now < at) {
      globe.setClock(at);
      return;
    }
  }

  const reached = all.filter((event) => event.actions && event.unixSeconds !== null && event.unixSeconds <= now);
  const latest = reached.at(-1) ?? null;
  if ((latest?.id ?? null) !== run.active) {
    run.active = latest?.id ?? null;
    apply(merge(reached), latest?.actions ?? {}, state);
  }

  if (run.pendingView) {
    if (state.view === run.pendingView) run.pendingView = null;
    else if (state.view !== "transitioning") globe.setView(run.pendingView);
  }
  if (run.ramp) {
    const { from, doublingS, startedAt } = run.ramp;
    const max = globe.limits().maxTimeScale;
    const value = Math.min(from * 2 ** ((performance.now() - startedAt) / 1000 / doublingS), max);
    globe.setTimeScale(sign(state) * value);
    if (value >= max) run.ramp = null;
  }
}

function merge(reached) {
  const merged = { ...baseline };
  for (const { actions } of reached) {
    const { camera, ...rest } = actions;
    if (rest.timeScale !== undefined) delete merged.rampDoublingS;
    Object.assign(merged, rest);
  }
  return merged;
}

function apply(merged, latest, state) {
  const changed = (key) => key in latest || JSON.stringify(merged[key]) !== JSON.stringify(run.applied[key]);

  if (changed("timeScale") || changed("rampDoublingS")) {
    run.ramp = null;
    if (merged.timeScale !== undefined) {
      const magnitude = merged.timeScale === "max" ? globe.limits().maxTimeScale : merged.timeScale;
      globe.setTimeScale(sign(state) * magnitude);
      if (merged.rampDoublingS) {
        run.ramp = { from: magnitude, doublingS: merged.rampDoublingS, startedAt: performance.now() };
      }
    }
  }
  if (changed("paused") && merged.paused !== undefined) globe.setSunPaused(merged.paused);
  if (changed("view") && merged.view) run.pendingView = merged.view;
  if (changed("frame") && merged.frame) globe.setFrame(merged.frame);
  if (changed("focus") && merged.focus) {
    const { layer, noradId } = merged.focus;
    const id = noradId ?? globe.ephemerisObjects(layer)?.[0]?.noradId;
    if (id !== undefined) globe.setGncFocus(layer, id);
  }
  if (changed("follow") && merged.follow) globe.followMission(merged.follow);
  if (changed("helioDistanceAu") && merged.helioDistanceAu) globe.setHeliocentricDistance(merged.helioDistanceAu);
  if (changed("faceFlyby") && merged.faceFlyby) globe.faceFlyby(merged.faceFlyby);
  if (latest.camera) globe.lookAt(latest.camera.lat, latest.camera.lon, latest.camera.altitudeKm);
  run.applied = merged;
}

const after = (unixSeconds, offsetS) => (unixSeconds === null ? null : unixSeconds + offsetS);
const sign = (state) => (state.sun.timeScale < 0 ? -1 : 1);

function seconds(text) {
  if (text === undefined) return null;
  const value = Date.parse(text) / 1000;
  return Number.isFinite(value) ? value : null;
}
