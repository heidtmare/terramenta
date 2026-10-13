/**
 * Project files: `*.terramenta.json`, format version `PROJECT_VERSION`.
 *
 * A project is the full initial scene — layers, missions, clock, camera, UI
 * layout and an optional director. Every project, bundled or user-supplied,
 * loads the same way: `?project=<url>` (or `?project=session` for a local file
 * stashed in `sessionStorage`), then a page load onto a fresh globe. Format:
 * README, "Project files".
 *
 * Relative URLs (project path and layer `url`s) resolve against the app root,
 * not the project file.
 */

import * as globe from "./globe.js";
import { DIRECTORS } from "./director.js";

export const PROJECT_VERSION = 1;
export const PROJECT_EXTENSION = ".terramenta.json";

/** Bundled projects: `[{file, name, description}]`, paths relative to `projects/`. */
export const MANIFEST_URL = "projects/manifest.json";

const SESSION_KEY = "terramenta.project";
const SESSION_SOURCE = "session";

/** Loaded with no `?project=`. */
export const EMPTY_PROJECT = { terramenta: PROJECT_VERSION, name: "Untitled" };

const PANELS = ["full", "flight"];
const TABS = ["project", "imagery", "data", "other", "mission"];
const VIEWS = ["globe", "heliocentric"];
const FRAMES = ["ecef", "eci"];
const ANCHORS = ["sun", "earth", "mars", "barycenter"];
const TRAIL_PATHS = ["track", "orbit"];
const ALTITUDE_MODES = ["relativeToSurface", "clampToSurface"];
const STYLE_FIELDS = ["pointColor", "pointSizePx", "lineColor", "lineWidthPx", "fillColor"];

export class ProjectError extends Error {
  constructor(errors) {
    super(errors.join("\n"));
    this.errors = errors;
  }
}

// --- Loading -----------------------------------------------------------------

/** `?project=` value, or `null`. */
export function projectParam() {
  return new URLSearchParams(location.search).get("project");
}

/** Fetches and parses the project named by `?project=`; `EMPTY_PROJECT` if none. */
export async function loadRequestedProject() {
  const param = projectParam();
  if (!param) return { project: parseProject(EMPTY_PROJECT), source: null };
  if (param === SESSION_SOURCE) {
    const stashed = readSession();
    if (!stashed) throw new ProjectError(["No local project in this session — open the file again."]);
    return { project: parseProject(JSON.parse(stashed.text)), source: stashed.name };
  }
  const url = resolveUrl(param);
  const response = await fetch(url);
  if (!response.ok) throw new ProjectError([`${param}: HTTP ${response.status}`]);
  let json;
  try {
    json = await response.json();
  } catch (error) {
    throw new ProjectError([`${param}: not JSON (${error.message})`]);
  }
  return { project: parseProject(json), source: param };
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

/** Reloads onto a project by URL (relative to the app root, or absolute). */
export function openProjectUrl(url) {
  const params = new URLSearchParams(location.search);
  params.set("project", url);
  location.assign(`${location.pathname}?${params}`);
}

/** Validates a local file, stashes it, reloads onto it. Throws `ProjectError` if invalid. */
export async function openProjectFile(file) {
  const text = await file.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch (error) {
    throw new ProjectError([`${file.name}: not JSON (${error.message})`]);
  }
  parseProject(json);
  try {
    sessionStorage.setItem(SESSION_KEY, JSON.stringify({ name: file.name, text }));
  } catch (error) {
    throw new ProjectError([`${file.name}: cannot stash in sessionStorage (${error.message})`]);
  }
  if (projectParam() === SESSION_SOURCE) location.reload();
  else openProjectUrl(SESSION_SOURCE);
}

/** Reloads with no project. */
export function openEmptyProject() {
  location.assign(location.pathname);
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
 * Validates and normalises a parsed project. Throws `ProjectError` listing
 * every problem. Unknown keys warn, not fail.
 */
export function parseProject(json) {
  const errors = [];
  const v = validator(errors);

  if (!v.object("", json)) throw new ProjectError(errors);
  if (json.terramenta !== PROJECT_VERSION) {
    errors.push(
      `terramenta: expected format version ${PROJECT_VERSION}, got ${JSON.stringify(json.terramenta)}`,
    );
  }
  v.keys("", json, [
    "$schema",
    "terramenta",
    "name",
    "description",
    "ui",
    "view",
    "frame",
    "clock",
    "camera",
    "imagery",
    "vectorTiles",
    "overlays",
    "ephemerides",
    "missions",
    "placemarks",
    "picking",
    "gnc",
    "hud",
    "keyboard",
    "director",
  ]);
  v.string("name", json.name);
  v.string("description", json.description);
  v.oneOf("view", json.view, VIEWS);
  v.oneOf("frame", json.frame, FRAMES);
  v.bool("picking", json.picking);
  v.bool("keyboard", json.keyboard);

  if (v.object("ui", json.ui, true)) {
    v.keys("ui", json.ui, ["panel", "tab", "readout", "collapsed"]);
    v.oneOf("ui.panel", json.ui.panel, PANELS);
    v.oneOf("ui.tab", json.ui.tab, TABS);
    v.bool("ui.readout", json.ui.readout);
    v.bool("ui.collapsed", json.ui.collapsed);
  }

  if (v.object("clock", json.clock, true)) {
    const clock = json.clock;
    v.keys("clock", clock, ["epoch", "timeScale", "paused", "shaded"]);
    if (clock.epoch !== undefined && clock.epoch !== "now" && !Number.isFinite(Date.parse(clock.epoch))) {
      errors.push(`clock.epoch: expected ISO 8601 UTC or "now", got ${JSON.stringify(clock.epoch)}`);
    }
    v.number("clock.timeScale", clock.timeScale);
    v.bool("clock.paused", clock.paused);
    v.bool("clock.shaded", clock.shaded);
  }

  if (v.object("camera", json.camera, true)) {
    const camera = json.camera;
    v.keys("camera", camera, ["lat", "lon", "altitudeKm", "follow", "anchor"]);
    v.number("camera.lat", camera.lat);
    v.number("camera.lon", camera.lon);
    v.number("camera.altitudeKm", camera.altitudeKm);
    if ((camera.lat === undefined) !== (camera.lon === undefined)) {
      errors.push("camera: lat and lon go together");
    }
    v.string("camera.follow", camera.follow);
    v.oneOf("camera.anchor", camera.anchor, ANCHORS);
    if (camera.follow !== undefined && camera.anchor !== undefined) {
      errors.push("camera: follow and anchor are exclusive");
    }
  }

  for (const key of ["imagery", "vectorTiles"]) {
    if (!v.object(key, json[key], true)) continue;
    v.keys(key, json[key], key === "imagery" ? ["enabled", "layer"] : ["enabled", "layer", "style"]);
    v.bool(`${key}.enabled`, json[key].enabled);
    const layer = json[key].layer;
    if (layer !== undefined && typeof layer !== "string" && !Number.isInteger(layer)) {
      errors.push(`${key}.layer: expected a preset label or index`);
    }
  }
  if (json.vectorTiles && v.object("vectorTiles.style", json.vectorTiles.style, true)) {
    v.style("vectorTiles.style", json.vectorTiles.style, true);
  }

  if (v.object("placemarks", json.placemarks, true)) {
    v.keys("placemarks", json.placemarks, ["enabled"]);
    v.bool("placemarks.enabled", json.placemarks.enabled);
  }

  if (v.object("hud", json.hud, true)) {
    v.keys("hud", json.hud, ["visible", "help"]);
    v.bool("hud.visible", json.hud.visible);
    v.bool("hud.help", json.hud.help);
  }

  if (v.object("gnc", json.gnc, true)) {
    const gnc = json.gnc;
    v.keys("gnc", gnc, [
      "enabled",
      "eciAxes",
      "ecefAxes",
      "graticule",
      "graticuleStepDeg",
      "sidereal",
      "track",
      "trackOrbits",
      "focus",
    ]);
    for (const key of ["enabled", "eciAxes", "ecefAxes", "graticule", "sidereal", "track"]) {
      v.bool(`gnc.${key}`, gnc[key]);
    }
    v.number("gnc.graticuleStepDeg", gnc.graticuleStepDeg);
    v.number("gnc.trackOrbits", gnc.trackOrbits);
    if (v.object("gnc.focus", gnc.focus, true)) {
      v.string("gnc.focus.layer", gnc.focus.layer, true);
      v.integer("gnc.focus.noradId", gnc.focus.noradId, true);
    }
  }

  const overlayIds = layerSection(v, "overlays", json.overlays, (path, layer) => {
    v.keys(path, layer, [
      "id",
      "url",
      "text",
      "label",
      "refreshSeconds",
      "visible",
      "simpleStyle",
      "altitudeMode",
      "altitudeScale",
      "extrude",
      ...STYLE_FIELDS,
    ]);
    v.bool(`${path}.simpleStyle`, layer.simpleStyle);
    v.oneOf(`${path}.altitudeMode`, layer.altitudeMode, ALTITUDE_MODES);
    v.number(`${path}.altitudeScale`, layer.altitudeScale);
    v.bool(`${path}.extrude`, layer.extrude);
    v.style(path, layer);
  });

  layerSection(v, "ephemerides", json.ephemerides, (path, layer) => {
    v.keys(path, layer, [
      "id",
      "url",
      "text",
      "label",
      "refreshSeconds",
      "visible",
      "select",
      "trails",
      "leadingOrbits",
      "trailingOrbits",
      "trailSamples",
      "trailPath",
      ...STYLE_FIELDS,
    ]);
    if (layer.select !== undefined && layer.select !== null) {
      if (!Array.isArray(layer.select) || !layer.select.every(Number.isInteger)) {
        errors.push(`${path}.select: expected an array of NORAD ids`);
      }
    }
    v.bool(`${path}.trails`, layer.trails);
    v.number(`${path}.leadingOrbits`, layer.leadingOrbits);
    v.number(`${path}.trailingOrbits`, layer.trailingOrbits);
    v.integer(`${path}.trailSamples`, layer.trailSamples);
    v.oneOf(`${path}.trailPath`, layer.trailPath, TRAIL_PATHS);
    v.style(path, layer);
  });

  const missionIds = new Set();
  if (v.array("missions", json.missions, true)) {
    json.missions.forEach((mission, index) => {
      const path = `missions[${index}]`;
      if (!v.object(path, mission)) return;
      v.keys(path, mission, [
        "id",
        "origin",
        "destination",
        "departureSearchDays",
        "arrivalSearchStartDays",
        "arrivalSearchEndDays",
        "searchSteps",
        "parkingAltitudeKm",
      ]);
      if (v.string(`${path}.id`, mission.id, true)) {
        if (missionIds.has(mission.id)) errors.push(`${path}.id: duplicate "${mission.id}"`);
        missionIds.add(mission.id);
      }
      v.string(`${path}.origin`, mission.origin);
      v.string(`${path}.destination`, mission.destination);
      v.number(`${path}.departureSearchDays`, mission.departureSearchDays);
      v.number(`${path}.arrivalSearchStartDays`, mission.arrivalSearchStartDays);
      v.number(`${path}.arrivalSearchEndDays`, mission.arrivalSearchEndDays);
      v.integer(`${path}.searchSteps`, mission.searchSteps);
      v.number(`${path}.parkingAltitudeKm`, mission.parkingAltitudeKm);
    });
  }

  if (json.camera?.follow !== undefined && !missionIds.has(json.camera.follow)) {
    errors.push(`camera.follow: no mission "${json.camera.follow}"`);
  }
  if (json.gnc?.focus && overlayIds.has(json.gnc.focus.layer)) {
    errors.push("gnc.focus.layer: names an overlay, expected an ephemeris layer");
  }

  if (v.object("director", json.director, true)) {
    const director = DIRECTORS[json.director.type];
    if (!director) {
      errors.push(
        `director.type: expected one of ${Object.keys(DIRECTORS).join(", ")}, ` +
          `got ${JSON.stringify(json.director.type)}`,
      );
    } else {
      errors.push(...director.validate(json.director, { missionIds }).map((e) => `director.${e}`));
    }
  }

  if (errors.length) throw new ProjectError(errors);
  return json;
}

/** Validates `{enabled, layers: [...]}`; returns the layer ids. */
function layerSection(v, key, section, checkLayer) {
  const ids = new Set();
  if (!v.object(key, section, true)) return ids;
  v.keys(key, section, ["enabled", "layers"]);
  v.bool(`${key}.enabled`, section.enabled);
  if (!v.array(`${key}.layers`, section.layers, true)) return ids;
  section.layers.forEach((layer, index) => {
    const path = `${key}.layers[${index}]`;
    if (!v.object(path, layer)) return;
    if (v.string(`${path}.id`, layer.id, true)) {
      if (ids.has(layer.id)) v.errors.push(`${path}.id: duplicate "${layer.id}"`);
      ids.add(layer.id);
    }
    if ((layer.url === undefined) === (layer.text === undefined)) {
      v.errors.push(`${path}: exactly one of url and text`);
    }
    v.string(`${path}.url`, layer.url);
    v.string(`${path}.text`, layer.text);
    v.string(`${path}.label`, layer.label);
    if (layer.refreshSeconds !== null) v.number(`${path}.refreshSeconds`, layer.refreshSeconds);
    v.bool(`${path}.visible`, layer.visible);
    checkLayer(path, layer);
  });
  return ids;
}

/** Type checks that push `path: message` and return whether the value passed. */
function validator(errors) {
  const fail = (path, expected, value) => {
    errors.push(`${path || "(root)"}: expected ${expected}, got ${JSON.stringify(value)}`);
    return false;
  };
  const optional = (value, required) => value === undefined && !required;
  const v = {
    errors,
    object: (path, value, optionalValue = false) =>
      value === undefined && optionalValue
        ? false
        : (value !== null && typeof value === "object" && !Array.isArray(value)) ||
          fail(path, "an object", value),
    array: (path, value, optionalValue = false) =>
      value === undefined && optionalValue ? false : Array.isArray(value) || fail(path, "an array", value),
    string: (path, value, required = false) =>
      optional(value, required) ? false : typeof value === "string" || fail(path, "a string", value),
    bool: (path, value) => optional(value) || typeof value === "boolean" || fail(path, "a boolean", value),
    number: (path, value, required = false) =>
      optional(value, required) || Number.isFinite(value) || fail(path, "a number", value),
    integer: (path, value, required = false) =>
      optional(value, required) || Number.isInteger(value) || fail(path, "an integer", value),
    oneOf: (path, value, allowed) =>
      optional(value) || allowed.includes(value) || fail(path, `one of ${allowed.join(", ")}`, value),
    style(path, value, strict = false) {
      if (strict) v.keys(path, value, STYLE_FIELDS);
      for (const key of ["pointColor", "lineColor", "fillColor"]) {
        const color = value[key];
        if (color !== undefined && !/^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(color)) {
          fail(`${path}.${key}`, "a hex colour", color);
        }
      }
      v.number(`${path}.pointSizePx`, value.pointSizePx);
      v.number(`${path}.lineWidthPx`, value.lineWidthPx);
    },
    keys(path, value, allowed) {
      for (const key of Object.keys(value)) {
        if (!allowed.includes(key)) console.warn(`Project: unknown key ${path ? `${path}.` : ""}${key}`);
      }
    },
  };
  return v;
}

// --- Applying ----------------------------------------------------------------

/**
 * Queues the project's scene onto the globe. Call between `globe.load()` and
 * `globe.start()`; pass `startOptions(project)` to `start`.
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
  if (project.picking !== undefined) globe.setPickingEnabled(project.picking);

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
  project.picking = state.overlays.picking;

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
