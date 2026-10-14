import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { beforeEach, test } from "node:test";
import { DIRECTORS } from "../src/director.js";
import * as globe from "../src/globe.js";
import {
  EMPTY_PROJECT,
  PROJECT_VERSION,
  ProjectError,
  applyProject,
  captureProject,
  migrateProject,
  parseProject,
  projectFromText,
} from "../src/project.js";
import { attachFake, commands } from "./fake-globe.js";

const APP = new URL("../", import.meta.url);
const SCHEMA = readJson("schema/project.schema.json");
const MANIFEST = readJson("projects/manifest.json");

globalThis.document = { baseURI: "http://localhost:8080/app/" };

function readJson(path) {
  return JSON.parse(readFileSync(new URL(path, APP), "utf8"));
}

/** `parseProject` with warnings collected instead of logged. */
function parse(json) {
  const warnings = [];
  const project = parseProject(json, { warn: (message) => warnings.push(message) });
  return { project, warnings };
}

function errorsOf(json) {
  try {
    parse(json);
  } catch (error) {
    assert.ok(error instanceof ProjectError, error);
    return error.errors;
  }
  assert.fail("expected a ProjectError");
}

const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });

// --- Bundled projects and schema ----------------------------------------------

test("every bundled project parses without warnings", () => {
  assert.ok(MANIFEST.length > 0);
  for (const entry of MANIFEST) {
    const { project, warnings } = parse(readJson(`projects/${entry.file}`));
    assert.deepEqual(warnings, [], entry.file);
    assert.equal(project.terramenta, PROJECT_VERSION, entry.file);
    assert.equal(project.name, entry.name, `${entry.file}: manifest name`);
  }
});

test("the empty project parses", () => {
  assert.deepEqual(parse(structuredClone(EMPTY_PROJECT)).warnings, []);
});

test("schema version is PROJECT_VERSION", () => {
  assert.equal(SCHEMA.properties.terramenta.const, PROJECT_VERSION);
});

test("schema lists exactly the registered directors, each with a $def", () => {
  assert.deepEqual(SCHEMA.$defs.director.properties.type.enum.sort(), Object.keys(DIRECTORS).sort());
  for (const type of Object.keys(DIRECTORS)) {
    assert.ok(SCHEMA.$defs[type], `$defs.${type}`);
    const branch = SCHEMA.$defs.director.allOf.find((rule) => rule.if.properties.type.const === type);
    assert.equal(branch?.then.$ref, `#/$defs/${type}`, `director branch for ${type}`);
  }
});

test("schema uses only keywords src/schema.js implements", () => {
  const supported = new Set([
    "$schema", "$ref", "$defs", "title", "description",
    "type", "const", "enum", "properties", "required", "additionalProperties", "items",
    "minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "pattern", "format",
    "anyOf", "allOf", "if", "then", "else",
  ]);
  const walk = (node, path) => {
    if (Array.isArray(node)) return node.forEach((item, index) => walk(item, `${path}[${index}]`));
    if (node === null || typeof node !== "object") return;
    for (const [key, child] of Object.entries(node)) {
      if (key === "properties" || key === "$defs") {
        for (const [name, sub] of Object.entries(child)) walk(sub, `${path}.${key}.${name}`);
        continue;
      }
      assert.ok(supported.has(key), `${path}.${key}`);
      if (key === "$ref") assert.ok(child.startsWith("#/$defs/") && SCHEMA.$defs[child.slice(8)], `${path}: ${child}`);
      if (key === "format") assert.equal(child, "date-time", path);
      if (key !== "enum" && key !== "const" && key !== "required") walk(child, `${path}.${key}`);
    }
  };
  walk(SCHEMA, "");
});

// --- Validation -----------------------------------------------------------------

test("structural errors list every failing path", () => {
  assert.deepEqual(
    errorsOf(
      v({
        name: 3,
        view: "side",
        camera: { lat: 91, altitudeKm: 0 },
        overlays: { layers: [{ url: "a.geojson", pointColor: "red", altitudeMode: "up" }] },
        ephemerides: { layers: [{ id: "s", url: "s.json", select: [1, "2"], trailPath: "x" }] },
        missions: [{ id: "m", searchSteps: 0 }],
        clock: { epoch: "soon" },
      }),
    ),
    [
      "name: expected a string, got 3",
      'view: expected one of globe, heliocentric, got "side"',
      "camera.lat: expected a number ≤ 90, got 91",
      "camera.altitudeKm: expected a number > 0, got 0",
      "overlays.layers[0].id: required",
      'overlays.layers[0].pointColor: expected a hex colour (#rgb, #rgba, #rrggbb, #rrggbbaa), got "red"',
      'overlays.layers[0].altitudeMode: expected one of relativeToSurface, clampToSurface, got "up"',
      'ephemerides.layers[0].select[1]: expected an integer, got "2"',
      'ephemerides.layers[0].trailPath: expected one of track, orbit, got "x"',
      "missions[0].searchSteps: expected a number ≥ 1, got 0",
      'clock.epoch: expected "now" or an ISO 8601 date-time, got "soon"',
    ],
  );
});

test("reference errors once the structure holds", () => {
  assert.deepEqual(
    errorsOf(
      v({
        camera: { lat: 0, follow: "nowhere", anchor: "sun" },
        overlays: { layers: [{ id: "a", url: "a" }, { id: "a", url: "a", text: "{}" }] },
        ephemerides: { layers: [{ id: "s" }] },
        missions: [{ id: "m" }, { id: "m" }],
        gnc: { focus: { layer: "a", noradId: 1 } },
        director: { type: "missionFlight", mission: "x" },
      }),
    ),
    [
      'overlays.layers[1].id: duplicate "a"',
      'missions[1].id: duplicate "m"',
      "overlays.layers[1]: exactly one of url and text",
      "ephemerides.layers[0]: exactly one of url and text",
      "camera: lat and lon go together",
      "camera: follow and anchor are exclusive",
      'camera.follow: no mission "nowhere"',
      "gnc.focus.layer: names an overlay, expected an ephemeris layer",
      'director.mission: no mission "x"',
    ],
  );
  assert.deepEqual(errorsOf(v({ gnc: { focus: { layer: "z", noradId: 1 } } })), [
    'gnc.focus.layer: no ephemeris layer "z"',
  ]);
});

test("directors are typed by their $def", () => {
  assert.deepEqual(errorsOf(v({ director: { type: "tour" } })), ['director.type: expected one of missionFlight, got "tour"']);
  assert.deepEqual(
    errorsOf(v({ missions: [{ id: "m" }], director: { type: "missionFlight", departureTimeScale: -1 } })),
    ["director.mission: required", "director.departureTimeScale: expected a number > 0, got -1"],
  );
});

test("unknown keys warn and still load", () => {
  const { warnings } = parse(v({ futureKey: 1, ui: { theme: "dark" }, overlays: { layers: [{ id: "a", url: "a", z: 1 }] } }));
  assert.deepEqual(warnings, ["unknown key futureKey", "unknown key ui.theme", "unknown key overlays.layers[0].z"]);
});

test("non-JSON and non-object input", () => {
  assert.throws(() => projectFromText("{", "bad.json"), (error) => /^bad\.json: not JSON/.test(error.errors[0]));
  assert.deepEqual(errorsOf([]), ["(root): expected Terramenta project, got []"]);
});

// --- Versions -----------------------------------------------------------------

test("version 1 upgrades: picking moves under overlays", () => {
  const v1 = { terramenta: 1, picking: false, overlays: { enabled: true } };
  const { project, warnings } = parse(v1);
  assert.deepEqual(project, { terramenta: 2, overlays: { enabled: true, picking: false } });
  assert.equal(v1.picking, false, "input left alone");
  assert.match(warnings[0], /upgraded from format version 1/);
  assert.deepEqual(parse({ terramenta: 1 }).project, { terramenta: 2 });
});

test("current version passes through untouched", () => {
  const json = v({ name: "x" });
  assert.equal(migrateProject(json), json);
});

test("newer, missing and malformed versions are refused", () => {
  assert.deepEqual(errorsOf({ terramenta: PROJECT_VERSION + 1 }), [
    `terramenta: format version ${PROJECT_VERSION + 1} is newer than this build reads (${PROJECT_VERSION})`,
  ]);
  for (const version of [undefined, 0, "2", 1.5]) {
    assert.match(errorsOf({ terramenta: version })[0], /^terramenta: expected a format version 1–/);
  }
});

// --- Applying and saving --------------------------------------------------------

let calls;
beforeEach(() => {
  calls = attachFake({ objects: { stations: [{ noradId: 25544, selected: true }, { noradId: 1, selected: false }] } });
});

test("applying sends only what the project sets", () => {
  applyProject(parse(v({ name: "Nothing" })).project);
  assert.deepEqual(commands(calls), []);
});

test("applying the overview resolves layer URLs against the app root", () => {
  const overview = parse(readJson("projects/overview.terramenta.json")).project;
  applyProject(overview);
  const overlays = calls.filter(([name]) => name === "addOverlay");
  assert.deepEqual(
    overlays.map(([, id, options]) => [id, options.url]),
    [
      ["geometry-tour", "http://localhost:8080/app/data/geometry-tour.geojson"],
      ["extruded-cube", "http://localhost:8080/app/data/extruded-cube.geojson"],
      ["quakes-hour", "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson"],
    ],
  );
  assert.equal(calls.filter(([name]) => name === "addEphemeris").length, 1);
});

test("applying names presets by label and picks by overlays.picking", () => {
  applyProject(parse(v({ imagery: { layer: "OSM" }, vectorTiles: { layer: 0 }, overlays: { picking: false } })).project);
  assert.deepEqual(
    calls.filter(([name]) => !["layers", "vectorLayers"].includes(name)),
    [["setLayer", 1], ["setVectorTileLayer", 0], ["setPickingEnabled", false]],
  );
});

/** A state-stream snapshot, as `captureProject` reads it. */
function snapshot() {
  const style = { pointColor: "#FF9E3D", pointSizePx: 6, lineColor: "#FF9E3DFF", lineWidthPx: 2, fillColor: "#FF9E3D3A" };
  return {
    view: "globe",
    frame: { mode: "eci" },
    sun: { unixSeconds: 1_790_000_000, timeScale: 60, paused: false, shaded: true },
    camera: { center: { lat: 12.3456789, lon: -45.6789012 }, altitudeKm: 20_000.123456 },
    missions: [{ id: "mars", origin: "Earth", destination: "Mars" }],
    imagery: { enabled: true, layerIndex: 1 },
    vectorTiles: { enabled: false, layerIndex: 0, style },
    overlays: {
      enabled: true,
      picking: false,
      layers: [
        {
          id: "local",
          label: "Local",
          url: null,
          refreshSeconds: null,
          visible: true,
          simpleStyle: true,
          altitude: { mode: "clampToSurface", scale: 1, extrude: false },
          style,
        },
        {
          id: "remote",
          label: "Remote",
          url: "http://localhost:8080/app/data/geometry-tour.geojson",
          refreshSeconds: 60,
          visible: false,
          simpleStyle: false,
          altitude: { mode: "relativeToSurface", scale: 2, extrude: true },
          style,
        },
      ],
    },
    ephemerides: {
      enabled: true,
      layers: [
        {
          id: "stations",
          label: "Stations",
          url: "https://celestrak.org/stations.json",
          refreshSeconds: null,
          visible: true,
          trails: true,
          trail: { leadingOrbits: 0.5, trailingOrbits: 0.25, samples: 128, path: "orbit" },
          style,
        },
      ],
    },
    placemarks: { enabled: true },
    gnc: {
      enabled: true,
      eciAxes: true,
      ecefAxes: false,
      graticule: true,
      graticuleStepDeg: 15,
      sidereal: false,
      track: true,
      trackOrbits: 2,
      focus: { layer: "stations", noradId: 25544 },
    },
    hud: { visible: false, helpVisible: false },
    keyboard: true,
  };
}

test("a captured scene parses cleanly and reapplies", () => {
  globe.addOverlay("local", { text: '{"type":"FeatureCollection","features":[]}' });
  globe.addMission("mars", { origin: "Earth", destination: "Mars", searchSteps: 40 });
  globe.setEphemerisSelection("stations", [25544]);
  globe.followMission("mars");

  const base = { name: "Saved", description: "d", ui: { tab: "data" }, director: { type: "missionFlight", mission: "mars" } };
  const captured = captureProject(snapshot(), base);
  const { project, warnings } = parse(JSON.parse(JSON.stringify(captured)));
  assert.deepEqual(warnings, []);

  assert.equal(project.terramenta, PROJECT_VERSION);
  assert.deepEqual(project.ui, base.ui);
  assert.deepEqual(project.director, base.director);
  assert.equal(project.clock.epoch, new Date(1_790_000_000_000).toISOString());
  assert.deepEqual(project.camera, { lat: 12.345679, lon: -45.678901, altitudeKm: 20_000.123, follow: "mars" });
  assert.equal(project.imagery.layer, "OSM");
  assert.equal(project.overlays.picking, false);
  assert.equal(project.overlays.layers[0].text, '{"type":"FeatureCollection","features":[]}');
  assert.equal(project.overlays.layers[1].url, "data/geometry-tour.geojson", "portable URL");
  assert.deepEqual(project.ephemerides.layers[0].select, [25544]);
  assert.equal(project.missions[0].searchSteps, 40);

  calls.length = 0;
  applyProject(project);
  const applied = commands(calls);
  for (const name of ["addOverlay", "addEphemeris", "addMission", "followMission", "setGncFocus", "setClock", "lookAt"]) {
    assert.ok(applied.includes(name), name);
  }
});

test("a director whose mission is gone is not saved", () => {
  const state = { ...snapshot(), missions: [] };
  const captured = captureProject(state, { director: { type: "missionFlight", mission: "mars" } });
  assert.equal(captured.director, undefined);
  assert.deepEqual(parse(JSON.parse(JSON.stringify(captured))).warnings, []);
});
