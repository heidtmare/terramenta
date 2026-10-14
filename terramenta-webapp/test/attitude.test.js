import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import * as globe from "../src/globe.js";
import {
  attitudeAem,
  attitudeResult,
  attitudesSection,
  axisName,
  fovName,
  lawName,
  onResult,
  openAttitudes,
  removeAttitude,
  setAttitude,
  subscribe,
  syncAttitudes,
} from "../src/attitude.js";
import { addStation, openEvents } from "../src/events.js";
import { PROJECT_VERSION, captureProject, parseProject } from "../src/project.js";
import { attachFake } from "./fake-globe.js";

globalThis.document = { baseURI: "http://localhost:8080/app/" };

const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });
const PROPAGATION = {
  initial: { epoch: "2026-01-01T00:00:00Z", positionKm: [6878, 0, 0], velocityKmS: [0, 7.6, 0] },
  durationS: 3600,
};
const NADIR = {
  spacecraft: "sat",
  primary: { body: [0, 0, 1], to: "nadir" },
  secondary: { body: [1, 0, 0], to: "velocity" },
  sensors: [
    { label: "Star tracker", body: [0, -1, 0], to: "sun", draw: false },
    { body: [0, 0, 1], to: "target", color: "#00ffcc" },
  ],
  station: "svalbard",
  stepS: 600,
};
const { station: _station, ...UNSITED } = NADIR;
const SVALBARD = { id: "svalbard", latDeg: 78.23, lonDeg: 15.39, altKm: 0.5 };

let calls;
beforeEach(() => {
  calls = attachFake();
  openEvents(v({}));
  openAttitudes(v({}));
});

const named = (name) => calls.filter(([call]) => call === name);

test("sync samples and draws once per change; hides when the profile goes", () => {
  addStation(SVALBARD);
  globe.addSpacecraft("sat", PROPAGATION, {});
  let edits = 0;
  let results = 0;
  const off = [subscribe(() => edits++), onResult(() => results++)];
  setAttitude(NADIR);
  calls.length = 0;
  syncAttitudes();
  syncAttitudes();
  const [[, propagation, options]] = named("attitude");
  assert.deepEqual(propagation, { ...PROPAGATION, kvn: false });
  assert.deepEqual(options, {
    stepS: 600,
    aem: false,
    primary: NADIR.primary,
    secondary: NADIR.secondary,
    target: { latDeg: 78.23, lonDeg: 15.39, altKm: 0.5 },
    sensors: NADIR.sensors.map(({ body, to }) => ({ body, to })),
  });
  assert.deepEqual(named("showAttitude"), [["showAttitude", "sat", { axes: true, sensors: [{ body: [0, 0, 1], color: "#00ffcc" }] }]]);
  assert.equal(attitudeResult("sat").result.times.length, 7);

  // Re-propagated, station moved: re-sampled each time.
  globe.addSpacecraft("sat", PROPAGATION, {});
  syncAttitudes();
  addStation({ ...SVALBARD, latDeg: 78 });
  syncAttitudes();
  assert.equal(named("attitude").length, 3);
  assert.equal(named("attitude")[2][2].target.latDeg, 78);

  removeAttitude("sat");
  syncAttitudes();
  assert.deepEqual(named("hideAttitude"), [["hideAttitude", "sat"]]);
  assert.equal(attitudeResult("sat"), null);
  off.forEach((unsubscribe) => unsubscribe());
  assert.deepEqual([edits, results], [2, 4]);
});

test("errors are kept and nothing is drawn", () => {
  globe.addSpacecraft("sat", PROPAGATION, {});
  setAttitude({ ...UNSITED, sensors: [{ body: [0, 0, 1], to: "bad" }] });
  syncAttitudes();
  assert.match(attitudeResult("sat").error, /unknown direction/);
  setAttitude({ ...NADIR, sensors: [] });
  syncAttitudes();
  assert.match(attitudeResult("sat").error, /no ground station "svalbard"/);
  setAttitude({ ...NADIR, spacecraft: "gone" });
  syncAttitudes();
  assert.match(attitudeResult("gone").error, /No spacecraft "gone"/);
  assert.equal(named("showAttitude").length, 0);
});

test("AEM on demand; untracked commands", () => {
  globe.addSpacecraft("sat", PROPAGATION, {});
  setAttitude({ spacecraft: "sat", quaternion: [0, 0, 0, 1] });
  let changes = 0;
  globe.onSceneChange(() => changes++);
  syncAttitudes();
  assert.match(attitudeAem("sat"), /^CCSDS_AEM_VERS/);
  globe.onSceneChange(null);
  assert.equal(changes, 0);
  assert.equal(named("attitude").at(-1)[2].aem, true);
  assert.throws(() => attitudeAem("other"), /no attitude/);
});

test("projects: references checked; capture keeps profiles of present spacecraft", () => {
  const project = (profile, extra = {}) =>
    v({ spacecraft: [{ id: "sat", propagation: PROPAGATION }], groundStations: [SVALBARD], attitudes: [profile], ...extra });
  assert.doesNotThrow(() => parseProject(project(NADIR)));
  assert.doesNotThrow(() => parseProject(project({ spacecraft: "sat", quaternion: [0, 0, 0, 1] })));
  for (const [profile, message] of [
    [{ ...NADIR, spacecraft: "gone" }, /attitudes\[0\]\.spacecraft: no spacecraft "gone"/],
    [{ ...NADIR, station: "nowhere" }, /no ground station "nowhere"/],
    [UNSITED, /"target" needs a station or target/],
    [{ ...NADIR, target: { latDeg: 0, lonDeg: 0 } }, /station or target, not both/],
    [{ ...NADIR, quaternion: [0, 0, 0, 1] }, /quaternion or primary\/secondary/],
    [{ spacecraft: "sat", primary: NADIR.primary }, /secondary: required/],
    [{ ...NADIR, primary: { body: [0, 0], to: "nadir" } }, /primary\.body: expected three numbers/],
    [{ ...NADIR, primary: { body: [0, 0, 1], to: "moon" } }, /one of nadir, .*target or three numbers, got "moon"/],
    [{ ...NADIR, stepS: 0 }, /stepS/],
  ]) {
    assert.throws(() => parseProject(project(profile)), message);
  }
  assert.throws(() => parseProject(project(NADIR, { attitudes: [NADIR, NADIR] })), /second profile for "sat"/);

  globe.addSpacecraft("sat", PROPAGATION, {});
  setAttitude(NADIR);
  setAttitude({ ...NADIR, spacecraft: "gone" });
  assert.deepEqual(attitudesSection({ spacecraftIds: new Set(["sat"]) }), { attitudes: [NADIR] });
  const state = {
    view: "globe",
    frame: { mode: "eci" },
    sun: { unixSeconds: 0, timeScale: 1, paused: false, shaded: true },
    camera: { center: { lat: 0, lon: 0 }, altitudeKm: 20_000 },
    imagery: { enabled: true, layerIndex: 0 },
    vectorTiles: { enabled: false, layerIndex: 0, style: { pointColor: "#fff", pointSizePx: 4, lineColor: "#fff", lineWidthPx: 1, fillColor: "#fff" } },
    overlays: { enabled: true, picking: false, layers: [] },
    ephemerides: { enabled: true, layers: [] },
    missions: [],
    placemarks: { enabled: true },
    gnc: { enabled: false, eciAxes: false, ecefAxes: false, graticule: false, graticuleStepDeg: 15, sidereal: false, track: false, trackOrbits: 1 },
    hud: { visible: false, helpVisible: false },
    keyboard: true,
  };
  addStation(SVALBARD);
  const captured = captureProject(state, { name: "Test" });
  assert.deepEqual(captured.attitudes, [NADIR]);
  assert.doesNotThrow(() => parseProject(captured));
});

test("names", () => {
  assert.deepEqual([[0, 0, -1], [1, 0, 0], [0.6, 0.8, 0]].map(axisName), ["−Z", "+X", "[0.6, 0.8, 0]"]);
  assert.equal(lawName(NADIR), "+Z → Nadir, +X → Velocity");
  assert.equal(
    lawName({ quaternion: [0, 0, 0.70710678, 0.70710678], spin: { axis: [0, 0, 1], rateDegS: 2 } }),
    "Inertial [0, 0, 0.7071, 0.7071] · spin 2°/s about +Z",
  );
  assert.equal(lawName({ ...NADIR, secondary: { body: [1, 0, 0], to: [0, 0, 1] } }), "+Z → Nadir, +X → GCRF [0, 0, 1]");
});

test("sensors without a direction are drawn with their footprint, not sampled", () => {
  globe.addSpacecraft("sat", PROPAGATION, {});
  const imager = { label: "Imager", body: [0, 0, 1], fov: { halfXDeg: 10, halfYDeg: 2, reference: [1, 0, 0] } };
  setAttitude({ ...UNSITED, sensors: [imager, { body: [0, 1, 0], to: "sun" }] });
  syncAttitudes();
  const [[, , options]] = named("attitude");
  assert.deepEqual(options.sensors, [{ body: [0, 1, 0], to: "sun" }]);
  assert.deepEqual(named("showAttitude")[0][2].sensors[0], { body: [0, 0, 1], color: "#ffcc33", fov: imager.fov });
  assert.equal(attitudeResult("sat").result.sensorAnglesDeg.length, 1);
  assert.equal(fovName(imager.fov), "10° × 2°");
  assert.equal(fovName({ halfAngleDeg: 20 }), "cone 20°");
});
