/**
 * Workspace: the open project and everything around it.
 *
 * - Switching: `open` resets the globe, applies the new project, loads its
 *   timeline, analysis, events, targets, attitudes, coverage and conjunctions and rebuilds the UI in place — no reload, the module and renderer
 *   stay up. Each snapshot runs the timeline (`timeline.js`).
 * - Unsaved changes: any scene-changing command outside `globe.quietly`, or a
 *   timeline, analysis or events edit, marks the project dirty (`globe.onSceneChange`). Opening another project asks
 *   first; leaving the page prompts.
 * - Autosave: a dirty scene is captured to IndexedDB `AUTOSAVE_KEY` after
 *   `AUTOSAVE_DELAY_MS` of quiet; cleared on save or discard. One left over
 *   at boot moves to `RECOVERY_KEY` and is offered by the Project section
 *   until recovered or dismissed.
 * - Recent projects: last `RECENT_LIMIT` opened, in IndexedDB `RECENT_KEY`.
 *   Local files keep their text (up to `RECENT_TEXT_LIMIT` characters).
 * - Drop: a project file dropped anywhere on the page opens.
 */

import * as globe from "./globe.js";
import * as store from "./store.js";
import {
  SESSION_SOURCE,
  applyProject,
  applySpacecraft,
  captureProject,
  downloadProject,
  emptyProject,
  fetchProject,
  projectFromText,
  readProjectFile,
  setProjectParam,
  stashProject,
  startOptions,
} from "./project.js";
import { openTimeline, runTimeline, subscribe as onTimelineEdit } from "./timeline.js";
import { openAnalysis, subscribe as onAnalysisEdit } from "./analysis.js";
import { openEvents, subscribe as onEventsEdit } from "./events.js";
import { openTargets, subscribe as onTargetsEdit } from "./targeting.js";
import { openAttitudes, subscribe as onAttitudesEdit } from "./attitude.js";
import { openConjunctions, subscribe as onConjunctionEdit } from "./conjunction.js";
import { openCoverage, subscribe as onCoverageEdit } from "./coverage.js";

const AUTOSAVE_KEY = "autosave";
const AUTOSAVE_DELAY_MS = 3_000;
const RECOVERY_KEY = "recovery";
const RECENT_KEY = "recent";
const RECENT_LIMIT = 8;
const RECENT_TEXT_LIMIT = 4_000_000;

let mountView = null;
let view = null;
let current = null;
let lastState = null;
let started = false;
/** Bumped per `open`; stale async application checks it. */
let generation = 0;
let dirty = false;
let autosaveTimer = null;
let recovery = null;
let recent = [];
const subscribers = new Set();

/**
 * `mount(project, source, signal)` builds the UI for a project and returns
 * `{sync(state)}`; `signal` aborts when the project is closed.
 */
export async function initWorkspace({ mount }) {
  mountView = mount;
  const [autosaved, kept, recents] = await Promise.all([
    store.get(AUTOSAVE_KEY),
    store.get(RECOVERY_KEY),
    store.get(RECENT_KEY),
  ]);
  recovery = autosaved ?? kept ?? null;
  if (autosaved) {
    store.set(RECOVERY_KEY, autosaved);
    store.remove(AUTOSAVE_KEY);
  }
  recent = recents ?? [];

  globe.onState((state) => {
    lastState = state;
    view?.sync(state);
    globe.quietly(() => runTimeline(state));
  });
  globe.onSceneChange(markDirty);
  onTimelineEdit(markDirty);
  onAnalysisEdit(markDirty);
  onEventsEdit(markDirty);
  onTargetsEdit(markDirty);
  onAttitudesEdit(markDirty);
  onCoverageEdit(markDirty);
  onConjunctionEdit(markDirty);

  addEventListener("beforeunload", (event) => {
    if (dirty) event.preventDefault();
  });
  addEventListener("pagehide", () => dirty && autosave());
  installDrop();
}

/** Starts the globe on the open project. Call once, after the first `open`. */
export async function start(canvasSelector) {
  await globe.start(canvasSelector, startOptions(current.project));
  started = true;
}

/** `{project, source, dirty, recovery, recent, state}` for the Project section. */
export function snapshot() {
  return { ...current, dirty, recovery, recent, state: lastState };
}

/** `callback()` whenever `snapshot()` changes; returns an unsubscribe. */
export function subscribe(callback) {
  subscribers.add(callback);
  return () => subscribers.delete(callback);
}

function notify() {
  for (const callback of subscribers) callback();
}

// --- Opening -------------------------------------------------------------------

/**
 * Opens a loaded project (`{project, source, param, text?}`) in place.
 * Resolves `false` if the user kept unsaved changes, else `true` once its
 * environment has loaded and its spacecraft are propagated. The scene and UI
 * are in place synchronously, so `start` need not wait. `recent: false` keeps
 * it off the recent list.
 */
export async function open(loaded, { force = false, recent: listed = true } = {}) {
  if (!force && !confirmDiscard()) return false;
  if (dirty) store.remove(AUTOSAVE_KEY);

  view?.abort();
  if (started) globe.reset();
  globe.quietly(() => {
    // This app draws its own readout; a project's `hud` overrides.
    globe.setHudVisible(false);
    applyProject(loaded.project);
    if (started && loaded.project.view === "heliocentric") globe.setView("heliocentric");
  });

  current = { project: loaded.project, source: loaded.source };
  openTimeline(loaded.project);
  openAnalysis(loaded.project);
  openEvents(loaded.project);
  openTargets(loaded.project);
  openAttitudes(loaded.project);
  openCoverage(loaded.project);
  openConjunctions(loaded.project);
  const controller = new AbortController();
  view = { ...mountView(loaded.project, loaded.source, controller.signal), abort: () => controller.abort() };
  // The last snapshot is the old scene.
  lastState = null;

  if (loaded.param === SESSION_SOURCE) stashProject(loaded);
  setProjectParam(loaded.param);
  setDirty(false);
  const opened = ++generation;
  await Promise.all([
    applySpacecraft(loaded.project, () => opened === generation),
    listed ? remember(loaded) : null,
  ]);
  return true;
}

export const openUrl = async (url) => open(await fetchProject(url));
export const openFile = async (file) => open(await readProjectFile(file));
export const openEmpty = () => open(emptyProject());

/** Opens a recent-projects entry. */
export async function openRecent(entry) {
  if (entry.text !== undefined) return open(projectFromText(entry.text, entry.source));
  return openUrl(entry.param);
}

/** Opens the autosaved scene as an unsaved local project. */
export async function recover() {
  if (!recovery) return false;
  const text = JSON.stringify(recovery.project);
  const loaded = projectFromText(text, `${recovery.source ?? recovery.project.name} (recovered)`);
  const kept = recovery;
  if (!(await open(loaded, { recent: false }))) return false;
  recovery = null;
  store.remove(RECOVERY_KEY);
  // Still unsaved: dirty, and autosaved until saved or discarded.
  store.set(AUTOSAVE_KEY, kept);
  setDirty(true);
  return true;
}

export function dismissRecovery() {
  recovery = null;
  store.remove(RECOVERY_KEY);
  notify();
}

function confirmDiscard() {
  if (!dirty) return true;
  return confirm(`Discard unsaved changes to "${current?.project.name ?? "Untitled"}"?`);
}

async function remember({ project, source, param, text }) {
  if (!source) return;
  const entry = { name: project.name ?? source, source, param, openedAt: Date.now() };
  if (param === SESSION_SOURCE) {
    if (text === undefined || text.length > RECENT_TEXT_LIMIT) return;
    entry.text = text;
  }
  recent = [entry, ...recent.filter((other) => other.source !== source)].slice(0, RECENT_LIMIT);
  await store.set(RECENT_KEY, recent);
  notify();
}

export function forgetRecent() {
  recent = [];
  store.remove(RECENT_KEY);
  notify();
}

// --- Saving --------------------------------------------------------------------

/** The current scene as a project; `null` before the first snapshot. */
export function capture() {
  return lastState ? captureProject(lastState, current.project) : null;
}

/** Downloads the current scene and marks it saved. */
export function save() {
  const project = capture();
  if (!project) return;
  downloadProject(project);
  store.remove(AUTOSAVE_KEY);
  setDirty(false);
}

function markDirty() {
  if (!current) return;
  if (!dirty) setDirty(true);
  clearTimeout(autosaveTimer);
  autosaveTimer = setTimeout(autosave, AUTOSAVE_DELAY_MS);
}

function setDirty(value) {
  dirty = value;
  if (!value) clearTimeout(autosaveTimer);
  const name = current?.project.name ?? "Untitled";
  document.title = `${dirty ? "• " : ""}${name} — Terramenta`;
  notify();
}

function autosave() {
  const project = dirty ? capture() : null;
  if (project) store.set(AUTOSAVE_KEY, { project, source: current.source, savedAt: Date.now() });
}

// --- Drop ----------------------------------------------------------------------

function installDrop() {
  const hasFiles = (event) => event.dataTransfer?.types.includes("Files");
  let depth = 0;
  addEventListener("dragenter", (event) => {
    if (!hasFiles(event)) return;
    depth++;
    document.body.classList.add("dropping");
  });
  addEventListener("dragleave", () => {
    if (--depth <= 0) {
      depth = 0;
      document.body.classList.remove("dropping");
    }
  });
  addEventListener("dragover", (event) => {
    if (hasFiles(event)) event.preventDefault();
  });
  addEventListener("drop", (event) => {
    depth = 0;
    document.body.classList.remove("dropping");
    const [file] = event.dataTransfer?.files ?? [];
    if (!file) return;
    event.preventDefault();
    openFile(file).catch(reportDropError);
  });
}

let dropErrorHandler = (error) => console.error(error);

/** Where errors from a dropped file go (the Project section shows them). */
export function onDropError(handler) {
  dropErrorHandler = handler;
}

function reportDropError(error) {
  dropErrorHandler(error);
}
