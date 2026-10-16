/**
 * Workspace: the open project and everything around it.
 *
 * - Switching: `open` resets the globe, applies the new project, loads its
 *   timeline, analysis, events, targets, attitudes, coverage, conjunctions and orbit determination and rebuilds the UI in place — no reload, the module and renderer
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
 * - Undo/redo: whole-scene snapshots. Edits settle for `CHECKPOINT_DELAY_MS`,
 *   then the scene before them goes on the undo stack (`HISTORY_LIMIT` deep).
 *   Clock time, rate, pause, camera position and view are not history: they
 *   are excluded from the comparison and kept live on restore. Restoring
 *   reapplies the snapshot like `open`, without the prompt or recents.
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
import { openOrbitDetermination, subscribe as onOdEdit } from "./od.js";

const AUTOSAVE_KEY = "autosave";
const AUTOSAVE_DELAY_MS = 3_000;
const RECOVERY_KEY = "recovery";
const RECENT_KEY = "recent";
const RECENT_LIMIT = 8;
const RECENT_TEXT_LIMIT = 4_000_000;
const HISTORY_LIMIT = 50;
const CHECKPOINT_DELAY_MS = 400;

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
/** `{project, key}` entries; `baseline` is the scene they lead to. */
let undoStack = [];
let redoStack = [];
let baseline = null;
/** Capture `baseline` on the next state (after an open settles). */
let needsBaseline = false;
let checkpointTimer = null;
let restoring = false;
/** History key of the scene as last saved or opened; `null` if never saved. */
let savedKey = null;

/**
 * `mount(project, source, signal, {restore})` builds the UI for a project and
 * returns `{sync(state)}`; `signal` aborts when the project is closed.
 * `restore`: an undo/redo of the same project — keep tab, scroll and layout.
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
    if (needsBaseline && !checkpointTimer) settleBaseline();
  });
  globe.onSceneChange(markDirty);
  onTimelineEdit(markDirty);
  onAnalysisEdit(markDirty);
  onEventsEdit(markDirty);
  onTargetsEdit(markDirty);
  onAttitudesEdit(markDirty);
  onCoverageEdit(markDirty);
  onConjunctionEdit(markDirty);
  onOdEdit(markDirty);

  addEventListener("beforeunload", (event) => {
    if (dirty) event.preventDefault();
  });
  addEventListener("pagehide", () => dirty && autosave());
  installDrop();
  installShortcuts();
}

/** Starts the globe on the open project. Call once, after the first `open`. */
export async function start(canvasSelector) {
  await globe.start(canvasSelector, startOptions(current.project));
  started = true;
}

/** `{project, source, dirty, recovery, recent, state, canUndo, canRedo}` for the Project section. */
export function snapshot() {
  return {
    ...current,
    dirty,
    recovery,
    recent,
    state: lastState,
    canUndo: undoStack.length > 0 || checkpointTimer !== null,
    canRedo: redoStack.length > 0,
  };
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

  resetHistory();
  current = { project: loaded.project, source: loaded.source };
  const opened = mountScene(loaded.project);

  if (loaded.param === SESSION_SOURCE) stashProject(loaded);
  setProjectParam(loaded.param);
  setDirty(false);
  await Promise.all([
    applySpacecraft(loaded.project, () => opened === generation),
    listed ? remember(loaded) : null,
  ]);
  if (opened === generation) needsBaseline = true;
  return true;
}

/**
 * Resets the globe and applies `project` with its UI, for `current.source`.
 * `options` go to the view's `mount`. Returns the new generation; spacecraft
 * are left to the caller.
 */
function mountScene(project, options = {}) {
  view?.abort();
  if (started) globe.reset();
  globe.quietly(() => {
    // This app draws its own readout; a project's `hud` overrides.
    globe.setHudVisible(false);
    applyProject(project);
    if (started && project.view === "heliocentric") globe.setView("heliocentric");
  });

  openTimeline(project);
  openAnalysis(project);
  openEvents(project);
  openTargets(project);
  openAttitudes(project);
  openCoverage(project);
  openConjunctions(project);
  openOrbitDetermination(project);
  const controller = new AbortController();
  view = { ...mountView(project, current.source, controller.signal, options), abort: () => controller.abort() };
  // The last snapshot is the old scene.
  lastState = null;
  return ++generation;
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
  savedKey = historyKey(project);
  setDirty(false);
}

function markDirty() {
  if (!current) return;
  if (!restoring) scheduleCheckpoint();
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

// --- Undo/redo ------------------------------------------------------------------

export const undo = () => restore(undoStack, redoStack);
export const redo = () => restore(redoStack, undoStack);

function resetHistory() {
  undoStack = [];
  redoStack = [];
  baseline = null;
  needsBaseline = false;
  savedKey = null;
  clearTimeout(checkpointTimer);
  checkpointTimer = null;
}

function settleBaseline() {
  const project = capture();
  if (!project) return;
  needsBaseline = false;
  baseline = { project, key: historyKey(project) };
  if (!dirty) savedKey = baseline.key;
}

function scheduleCheckpoint() {
  const notifying = checkpointTimer === null;
  clearTimeout(checkpointTimer);
  checkpointTimer = setTimeout(checkpoint, CHECKPOINT_DELAY_MS);
  if (notifying) notify();
}

/** Records the scene before the settled edits, if they changed anything. */
function checkpoint() {
  clearTimeout(checkpointTimer);
  checkpointTimer = null;
  const project = capture();
  if (project) {
    const key = historyKey(project);
    if (!baseline) {
      // Edited before the opened scene was captured: nothing to go back to.
      needsBaseline = false;
    } else if (key !== baseline.key) {
      undoStack.push(baseline);
      if (undoStack.length > HISTORY_LIMIT) undoStack.shift();
      redoStack = [];
    }
    baseline = { project, key };
  }
  notify();
}

/** Moves the scene one step from `from` to `to`; ignored while one is applying. */
async function restore(from, to) {
  if (restoring || !current) return false;
  if (checkpointTimer) checkpoint();
  const target = from.pop();
  if (!target || !baseline) return false;
  to.push(baseline);
  baseline = target;

  restoring = true;
  try {
    const project = withLiveView(target.project);
    const opened = mountScene(project, { restore: true });
    if (target.key === savedKey) {
      store.remove(AUTOSAVE_KEY);
      setDirty(false);
    } else {
      markDirty();
    }
    await applySpacecraft(project, () => opened === generation);
  } finally {
    restoring = false;
  }
  return true;
}

/** `project` with the live clock, camera position and view. */
function withLiveView(project) {
  const live = capture();
  if (!live) return project;
  const { epoch, timeScale, paused } = live.clock;
  const { lat, lon, altitudeKm } = live.camera;
  return {
    ...project,
    view: live.view,
    clock: { ...project.clock, epoch, timeScale, paused },
    camera: { ...project.camera, lat, lon, altitudeKm },
  };
}

/** `project` as compared for history: without what `withLiveView` keeps. */
function historyKey(project) {
  const { view, clock = {}, camera = {}, ...rest } = project;
  const { epoch, timeScale, paused, ...clockKept } = clock;
  const { lat, lon, altitudeKm, ...cameraKept } = camera;
  return JSON.stringify({ ...rest, clock: clockKept, camera: cameraKept });
}

const TEXT_FIELDS =
  "textarea, input:not([type=checkbox], [type=radio], [type=range], [type=button], [type=file], [type=color])";

/** Cmd/Ctrl+Z undo, Shift+Cmd/Ctrl+Z or Ctrl+Y redo, Cmd/Ctrl+S save; text fields keep their own. */
function installShortcuts() {
  addEventListener("keydown", (event) => {
    if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
    const key = event.key.toLowerCase();
    if (key === "s") {
      event.preventDefault();
      save();
      return;
    }
    if (key !== "z" && key !== "y") return;
    const target = event.target;
    if (target instanceof HTMLElement && (target.isContentEditable || target.matches(TEXT_FIELDS))) return;
    event.preventDefault();
    if (key === "y" || event.shiftKey) redo();
    else undo();
  });
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
