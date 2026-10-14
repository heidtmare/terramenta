/**
 * Project files: `*.terramenta.json`, format version `PROJECT_VERSION`.
 *
 * A project is the full initial scene — layers, missions, clock, camera, UI
 * layout and an optional director. Every project, bundled or user-supplied,
 * loads the same way: parsed here, then applied onto a reset globe by
 * `workspace.js`. `?project=<url>` (or `?project=session` for a local file
 * stashed in `sessionStorage`) names the open project, so a reload reopens it.
 * Format: `schema/project.schema.json`; README, "Project files".
 *
 * Relative URLs (project path and layer `url`s) resolve against the app root,
 * not the project file.
 */

import * as globe from "./globe.js";
import { DIRECTORS } from "./director.js";
import { validate } from "./schema.js";
import SCHEMA from "../schema/project.schema.json" with { type: "json" };

/** Format version written; older ones are upgraded on load (`migrateProject`). */
export const PROJECT_VERSION = 2;
export const PROJECT_EXTENSION = ".terramenta.json";

/** Bundled projects: `[{file, name, description}]`, paths relative to `projects/`. */
export const MANIFEST_URL = "projects/manifest.json";

const SESSION_KEY = "terramenta.project";
export const SESSION_SOURCE = "session";

/** Loaded with no `?project=`. */
export const EMPTY_PROJECT = { terramenta: PROJECT_VERSION, name: "Untitled" };

export class ProjectError extends Error {
  constructor(errors) {
    super(errors.join("\n"));
    this.errors = errors;
  }
}

// --- Loading -----------------------------------------------------------------
// Loaders resolve to `{project, source, param}`: `source` is what the UI shows
// (path or file name, `null` if unsaved), `param` the `?project=` value that
// reopens it (`null`: none).

/** `?project=` value, or `null`. */
export function projectParam() {
  return new URLSearchParams(location.search).get("project");
}

/** Sets or clears `?project=` without navigating. */
export function setProjectParam(param) {
  const params = new URLSearchParams(location.search);
  if (param) params.set("project", param);
  else params.delete("project");
  const query = params.toString();
  history.replaceState(history.state, "", `${location.pathname}${query ? `?${query}` : ""}`);
}

/** The project named by `?project=`; the empty project if none. */
export async function loadRequestedProject() {
  const param = projectParam();
  if (!param) return emptyProject();
  if (param === SESSION_SOURCE) {
    const stashed = readSession();
    if (!stashed) throw new ProjectError(["No local project in this session — open the file again."]);
    return projectFromText(stashed.text, stashed.name);
  }
  return fetchProject(param);
}

export function emptyProject() {
  return { project: parseProject(structuredClone(EMPTY_PROJECT)), source: null, param: null };
}

/** Fetches a project by URL (relative to the app root, or absolute). */
export async function fetchProject(url) {
  const response = await fetch(resolveUrl(url));
  if (!response.ok) throw new ProjectError([`${url}: HTTP ${response.status}`]);
  return projectFromText(await response.text(), url, url);
}

/** Reads a local file; reopened on reload from `sessionStorage`. */
export async function readProjectFile(file) {
  return projectFromText(await file.text(), file.name);
}

/**
 * Parses project text. `param` omitted: a local project, stashed in
 * `sessionStorage` once opened (`stashProject`).
 */
export function projectFromText(text, name, param = SESSION_SOURCE) {
  let json;
  try {
    json = JSON.parse(text);
  } catch (error) {
    throw new ProjectError([`${name}: not JSON (${error.message})`]);
  }
  return { project: parseProject(json), source: name, param, text };
}

/** Stashes a local project so `?project=session` reopens it. Best effort. */
export function stashProject({ source, text }) {
  try {
    sessionStorage.setItem(SESSION_KEY, JSON.stringify({ name: source, text }));
    return true;
  } catch (error) {
    console.warn(`Project: cannot stash ${source} in sessionStorage`, error);
    return false;
  }
}

/** Bundled project list; `[]` if the manifest is missing. */
export async function loadManifest() {
  try {
    const response = await fetch(resolveUrl(MANIFEST_URL));
    return response.ok ? await response.json() : [];
  } catch {
    return [];
  }
}

function readSession() {
  try {
    const raw = sessionStorage.getItem(SESSION_KEY);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

function resolveUrl(url) {
  return new URL(url, document.baseURI).href;
}

/** Same-origin URLs under the app root become root-relative; others are kept. */
function portableUrl(url) {
  if (!url) return url;
  const base = new URL(".", document.baseURI).href;
  return url.startsWith(base) ? url.slice(base.length) : url;
}

// --- Validation --------------------------------------------------------------

/**
 * Upgrades, validates and returns a parsed project. Throws `ProjectError`
 * listing every problem. Structure is checked against `SCHEMA`; ids and
 * references here. Unknown keys go to `warn`, not fail.
 */
export function parseProject(json, { warn = (message) => console.warn(`Project: ${message}`) } = {}) {
  const project = migrateProject(json, warn);
  const errors = validate(SCHEMA, project, { warn: (path) => warn(`unknown key ${path}`) });
  // References assume the structure holds.
  if (!errors.length) errors.push(...referenceErrors(project));
  if (errors.length) throw new ProjectError(errors);
  return project;
}

/**
 * Brings an older format version up to `PROJECT_VERSION`; returns a copy.
 * `MIGRATIONS[n]` rewrites version `n` into `n + 1` in place.
 */
export function migrateProject(json, warn = () => {}) {
  if (json === null || typeof json !== "object" || Array.isArray(json)) return json;
  const version = json.terramenta;
  if (!Number.isInteger(version) || version < 1) {
    throw new ProjectError([`terramenta: expected a format version 1–${PROJECT_VERSION}, got ${JSON.stringify(version)}`]);
  }
  if (version > PROJECT_VERSION) {
    throw new ProjectError([
      `terramenta: format version ${version} is newer than this build reads (${PROJECT_VERSION})`,
    ]);
  }
  if (version === PROJECT_VERSION) return json;
  const project = structuredClone(json);
  for (let from = version; from < PROJECT_VERSION; from++) {
    MIGRATIONS[from](project);
    project.terramenta = from + 1;
  }
  warn(`upgraded from format version ${version} to ${PROJECT_VERSION}; saving writes ${PROJECT_VERSION}`);
  return project;
}

const MIGRATIONS = {
  /** 1 → 2: `picking` moves under `overlays`, where the state stream reports it. */
  1(project) {
    if (project.picking !== undefined) {
      project.overlays = { ...project.overlays, picking: project.picking };
      delete project.picking;
    }
  },
};

/** Ids, uniqueness and cross-references; `project` already matches `SCHEMA`. */
function referenceErrors(project) {
  const errors = [];
  const ids = (path, entries = []) => {
    const seen = new Set();
    entries.forEach((entry, index) => {
      if (seen.has(entry.id)) errors.push(`${path}[${index}].id: duplicate "${entry.id}"`);
      seen.add(entry.id);
    });
    return seen;
  };
  const overlayIds = ids("overlays.layers", project.overlays?.layers);
  const ephemerisIds = ids("ephemerides.layers", project.ephemerides?.layers);
  const missionIds = ids("missions", project.missions);

  for (const key of ["overlays", "ephemerides"]) {
    project[key]?.layers?.forEach((layer, index) => {
      if ((layer.url === undefined) === (layer.text === undefined)) {
        errors.push(`${key}.layers[${index}]: exactly one of url and text`);
      }
    });
  }

  const camera = project.camera ?? {};
  if ((camera.lat === undefined) !== (camera.lon === undefined)) errors.push("camera: lat and lon go together");
  if (camera.follow !== undefined && camera.anchor !== undefined) {
    errors.push("camera: follow and anchor are exclusive");
  }
  if (camera.follow !== undefined && !missionIds.has(camera.follow)) {
    errors.push(`camera.follow: no mission "${camera.follow}"`);
  }

  const focus = project.gnc?.focus;
  if (focus && !ephemerisIds.has(focus.layer)) {
    errors.push(
      overlayIds.has(focus.layer)
        ? "gnc.focus.layer: names an overlay, expected an ephemeris layer"
        : `gnc.focus.layer: no ephemeris layer "${focus.layer}"`,
    );
  }

  if (project.director) {
    const director = DIRECTORS[project.director.type];
    errors.push(...director.validate(project.director, { missionIds }).map((error) => `director.${error}`));
  }
  return errors;
}

// --- Applying ----------------------------------------------------------------

/**
 * Queues the project's scene onto the globe: before `globe.start()` (pass
 * `startOptions(project)` to `start`), or after `globe.reset()`. Only keys the
 * project sets are sent; everything else keeps the fresh-globe default.
 */
export function applyProject(project) {
  if (project.hud?.visible !== undefined) globe.setHudVisible(project.hud.visible);
  if (project.hud?.help !== undefined) globe.setHelpVisible(project.hud.help);
  if (project.keyboard !== undefined) globe.setKeyboardEnabled(project.keyboard);
  if (project.frame) globe.setFrame(project.frame);

  const imagery = project.imagery ?? {};
  if (imagery.enabled !== undefined) globe.setImageryEnabled(imagery.enabled);
  if (imagery.layer !== undefined) globe.setLayer(presetIndex(globe.layers(), imagery.layer, "imagery"));

  const vectorTiles = project.vectorTiles ?? {};
  if (vectorTiles.enabled !== undefined) globe.setVectorTilesEnabled(vectorTiles.enabled);
  if (vectorTiles.layer !== undefined) {
    globe.setVectorTileLayer(presetIndex(globe.vectorLayers(), vectorTiles.layer, "vectorTiles"));
  }
  if (vectorTiles.style) globe.setVectorTileStyle(vectorTiles.style);

  if (project.overlays?.enabled !== undefined) globe.setOverlaysEnabled(project.overlays.enabled);
  if (project.ephemerides?.enabled !== undefined) globe.setEphemeridesEnabled(project.ephemerides.enabled);
  if (project.placemarks?.enabled !== undefined) globe.setPlacemarksEnabled(project.placemarks.enabled);
  if (project.overlays?.picking !== undefined) globe.setPickingEnabled(project.overlays.picking);

  const gnc = project.gnc ?? {};
  const gncSetters = {
    enabled: globe.setGncEnabled,
    eciAxes: globe.setGncEciAxes,
    ecefAxes: globe.setGncEcefAxes,
    graticule: globe.setGncGraticule,
    graticuleStepDeg: globe.setGncGraticuleStep,
    sidereal: globe.setGncSidereal,
    track: globe.setGncTrack,
    trackOrbits: globe.setGncTrackOrbits,
  };
  for (const [key, set] of Object.entries(gncSetters)) {
    if (gnc[key] !== undefined) set(gnc[key]);
  }

  const clock = project.clock ?? {};
  if (clock.epoch !== undefined && clock.epoch !== "now") globe.setClock(Date.parse(clock.epoch) / 1000);
  if (clock.timeScale !== undefined) globe.setTimeScale(clock.timeScale);
  if (clock.paused !== undefined) globe.setSunPaused(clock.paused);
  if (clock.shaded !== undefined) globe.setSunShaded(clock.shaded);

  const camera = project.camera ?? {};
  if (camera.lat !== undefined) globe.lookAt(camera.lat, camera.lon, camera.altitudeKm);
  else if (camera.altitudeKm !== undefined) globe.setAltitude(camera.altitudeKm);

  for (const { id, ...options } of project.overlays?.layers ?? []) {
    globe.addOverlay(id, withResolvedUrl(options));
  }
  for (const { id, ...options } of project.ephemerides?.layers ?? []) {
    globe.addEphemeris(id, withResolvedUrl(options));
  }
  if (gnc.focus) globe.setGncFocus(gnc.focus.layer, gnc.focus.noradId);

  for (const { id, ...options } of project.missions ?? []) globe.addMission(id, options);
  if (camera.follow !== undefined) globe.followMission(camera.follow);
  if (camera.anchor !== undefined) globe.setHeliocentricAnchor(camera.anchor);
}

/** Options for `globe.start`. */
export function startOptions(project) {
  return { view: project.view };
}

/** Installs the project's director, if any; returns a state callback or `null`. */
export function directorFor(project) {
  return project.director ? DIRECTORS[project.director.type].create(project.director) : null;
}

function withResolvedUrl(options) {
  return options.url === undefined ? options : { ...options, url: resolveUrl(options.url) };
}

function presetIndex(presets, layer, key) {
  if (Number.isInteger(layer)) return layer;
  const preset = presets.find((preset) => preset.label === layer);
  if (!preset) {
    console.warn(`Project: ${key}.layer "${layer}" not found; using preset 0`);
    return 0;
  }
  return preset.index;
}

// --- Saving ------------------------------------------------------------------

/**
 * The current scene as a project. `base` is the loaded project: its name,
 * description, `ui` and `director` carry over, since the globe does not
 * report them.
 */
export function captureProject(state, base = {}) {
  const sources = globe.recordedSources();
  const project = {
    terramenta: PROJECT_VERSION,
    name: base.name ?? "Untitled",
  };
  if (base.description) project.description = base.description;
  if (base.ui) project.ui = base.ui;

  project.view = state.view === "heliocentric" ? "heliocentric" : "globe";
  project.frame = state.frame.mode;
  project.clock = {
    epoch: new Date(state.sun.unixSeconds * 1000).toISOString(),
    timeScale: state.sun.timeScale,
    paused: state.sun.paused,
    shaded: state.sun.shaded,
  };

  project.camera = {
    lat: round(state.camera.center.lat, 6),
    lon: round(state.camera.center.lon, 6),
    altitudeKm: round(state.camera.altitudeKm, 3),
  };
  const missionIds = new Set(state.missions.map((mission) => mission.id));
  if (sources.camera?.mission !== undefined && missionIds.has(sources.camera.mission)) {
    project.camera.follow = sources.camera.mission;
  } else if (sources.camera?.anchor !== undefined) {
    project.camera.anchor = sources.camera.anchor;
  }

  project.imagery = {
    enabled: state.imagery.enabled,
    layer: globe.layers()[state.imagery.layerIndex]?.label ?? state.imagery.layerIndex,
  };
  project.vectorTiles = {
    enabled: state.vectorTiles.enabled,
    layer: globe.vectorLayers()[state.vectorTiles.layerIndex]?.label ?? state.vectorTiles.layerIndex,
    style: styleOf(state.vectorTiles.style),
  };

  project.overlays = {
    enabled: state.overlays.enabled,
    picking: state.overlays.picking,
    layers: state.overlays.layers.map((layer) => {
      const recorded = sources.overlays.get(layer.id) ?? {};
      return {
        id: layer.id,
        label: layer.label,
        ...sourceOf(layer, recorded),
        visible: layer.visible,
        simpleStyle: layer.simpleStyle,
        altitudeMode: layer.altitude.mode,
        altitudeScale: layer.altitude.scale,
        extrude: layer.altitude.extrude,
        ...styleOf(layer.style),
      };
    }),
  };

  project.ephemerides = {
    enabled: state.ephemerides.enabled,
    layers: state.ephemerides.layers.map((layer) => {
      const recorded = sources.ephemerides.get(layer.id) ?? {};
      const entry = {
        id: layer.id,
        label: layer.label,
        ...sourceOf(layer, recorded),
        visible: layer.visible,
        trails: layer.trails,
        leadingOrbits: layer.trail.leadingOrbits,
        trailingOrbits: layer.trail.trailingOrbits,
        trailSamples: layer.trail.samples,
        trailPath: layer.trail.path,
        ...styleOf(layer.style),
      };
      if (sources.selected.has(layer.id)) {
        const objects = globe.ephemerisObjects(layer.id);
        entry.select = objects
          ? objects.filter((object) => object.selected).map((object) => object.noradId)
          : recorded.select;
      }
      return entry;
    }),
  };

  project.missions = state.missions.map((mission) => ({
    id: mission.id,
    ...(sources.missions.get(mission.id) ?? {}),
    origin: mission.origin,
    destination: mission.destination,
  }));

  project.placemarks = { enabled: state.placemarks.enabled };

  const gnc = state.gnc;
  project.gnc = {
    enabled: gnc.enabled,
    eciAxes: gnc.eciAxes,
    ecefAxes: gnc.ecefAxes,
    graticule: gnc.graticule,
    graticuleStepDeg: gnc.graticuleStepDeg,
    sidereal: gnc.sidereal,
    track: gnc.track,
    trackOrbits: gnc.trackOrbits,
  };
  if (gnc.focus) project.gnc.focus = { layer: gnc.focus.layer, noradId: gnc.focus.noradId };

  project.hud = { visible: state.hud.visible, help: state.hud.helpVisible };
  project.keyboard = state.keyboard;

  if (base.director && (!base.director.mission || missionIds.has(base.director.mission))) {
    project.director = base.director;
  }
  return project;
}

/** Downloads `project` as `<name>.terramenta.json`. */
export function downloadProject(project) {
  const blob = new Blob([`${JSON.stringify(project, null, 2)}\n`], { type: "application/json" });
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = `${slug(project.name) || "project"}${PROJECT_EXTENSION}`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 0);
}

function sourceOf(layer, recorded) {
  if (layer.url) return { url: portableUrl(layer.url), refreshSeconds: layer.refreshSeconds ?? null };
  return { text: recorded.text ?? "" };
}

function styleOf(style) {
  return {
    pointColor: style.pointColor,
    pointSizePx: style.pointSizePx,
    lineColor: style.lineColor,
    lineWidthPx: style.lineWidthPx,
    fillColor: style.fillColor,
  };
}

function round(value, digits) {
  const scale = 10 ** digits;
  return Math.round(value * scale) / scale;
}

function slug(name) {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}
