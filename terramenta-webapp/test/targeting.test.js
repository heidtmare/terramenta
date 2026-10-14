import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import * as globe from "../src/globe.js";
import { PROJECT_VERSION, captureProject, parseProject } from "../src/project.js";
import {
  addTarget,
  applyTarget,
  atName,
  onResult,
  openTargets,
  removeTarget,
  solveTarget,
  subscribe,
  targetResult,
  targetsSection,
  uniqueTargetId,
  variableValue,
} from "../src/targeting.js";
import { attachFake } from "./fake-globe.js";

globalThis.document = { baseURI: "http://localhost:8080/app/" };

const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });
const PROPAGATION = {
  initial: { epoch: "2026-01-01T00:00:00Z", positionKm: [6878, 0, 0], velocityKmS: [0, 7.6, 0] },
  durationS: 21_600,
  maneuvers: [
    { epoch: "2026-01-01T00:30:00Z", deltaVKmS: [0.08, 0, 0] },
    { epoch: "2026-01-01T01:18:00Z", deltaVKmS: [0.08, 0, 0] },
  ],
};
const HOHMANN = {
  id: "hohmann",
  label: "Hohmann",
  spacecraft: "sat",
  variables: [
    { burn: 1, parameter: "dv1" },
    { burn: 2, parameter: "epoch" },
    { burn: 2, parameter: "dv1" },
  ],
  goals: [
    { quantity: "flightPathAngleDeg", at: "burn2.before", value: 0, tolerance: 1e-4 },
    { quantity: "smaKm", at: "burn2", value: 7178.137, tolerance: 1e-3 },
  ],
};

let calls;
beforeEach(() => {
  calls = attachFake();
  openTargets(v({}));
});

test("edits notify and drop results; ids slug and stay unique; saving drops orphans", () => {
  let edits = 0;
  let solves = 0;
  const offEdit = subscribe(() => edits++);
  const offSolve = onResult(() => solves++);
  globe.addSpacecraft("sat", PROPAGATION, {});
  addTarget(HOHMANN);
  solveTarget("hohmann");
  assert.ok(targetResult("hohmann").result);
  addTarget({ ...HOHMANN, maxIterations: 10 });
  assert.equal(targetResult("hohmann"), null, "edit drops the result");
  assert.equal(uniqueTargetId("Hohmann"), "hohmann-2");
  assert.equal(uniqueTargetId("Drag make-up (LEO)"), "drag-make-up-leo");
  addTarget({ ...HOHMANN, id: "other", spacecraft: "gone" });
  assert.deepEqual(targetsSection({ spacecraftIds: new Set(["sat"]) }), { targets: [{ ...HOHMANN, maxIterations: 10 }] });
  removeTarget("other");
  removeTarget("hohmann");
  assert.deepEqual(targetsSection({ spacecraftIds: new Set(["sat"]) }), {});
  offEdit();
  offSolve();
  assert.deepEqual([edits, solves], [5, 1]);
});

test("solve passes the recorded propagation; apply re-adds the spacecraft, tracked", () => {
  globe.addSpacecraft("sat", PROPAGATION, { label: "Sat" });
  addTarget({ ...HOHMANN, maxIterations: 7 });
  calls.length = 0;
  const solved = solveTarget("hohmann");
  const [, propagation, options] = calls.find(([name]) => name === "target");
  assert.deepEqual(propagation, { ...PROPAGATION, kvn: false });
  assert.deepEqual(options, { variables: HOHMANN.variables, goals: HOHMANN.goals, maxIterations: 7 });
  assert.equal(solved.result.converged, true);
  assert.equal(solved.stale, false);

  let dirty = 0;
  globe.onSceneChange(() => dirty++);
  applyTarget("hohmann");
  globe.onSceneChange(null);
  assert.ok(dirty > 0);
  const entry = globe.recordedSources().spacecraft.get("sat");
  assert.deepEqual(
    entry.propagation.maneuvers.map((maneuver) => maneuver.deltaVKmS[0]),
    [1.08, 1.08],
  );
  assert.equal(entry.display.label, "Sat");
  assert.deepEqual([targetResult("hohmann").applied, targetResult("hohmann").stale], [true, false]);

  // Re-propagated elsewhere: stale, not applicable.
  globe.addSpacecraft("sat", PROPAGATION, {});
  assert.equal(targetResult("hohmann").stale, true);
  assert.throws(() => applyTarget("hohmann"), /solve first/);
});

test("a solve that throws is kept as an error", () => {
  globe.addSpacecraft("sat", PROPAGATION, {});
  addTarget({ ...HOHMANN, goals: [] });
  assert.match(solveTarget("hohmann").error, /need at least one/);
  addTarget({ ...HOHMANN, spacecraft: "gone" });
  assert.match(solveTarget("hohmann").error, /no spacecraft "gone"/);
});

test("projects: references to spacecraft and burns checked", () => {
  const project = (target) => v({ spacecraft: [{ id: "sat", propagation: PROPAGATION }], targets: [target] });
  assert.doesNotThrow(() => parseProject(project(HOHMANN)));
  for (const [target, message] of [
    [{ ...HOHMANN, spacecraft: "gone" }, /targets\[0\]\.spacecraft: no spacecraft "gone"/],
    [{ ...HOHMANN, variables: [{ burn: 3, parameter: "dv1" }] }, /variables\[0\]\.burn: no burn 3/],
    [{ ...HOHMANN, variables: [{ finite: 1, parameter: "start" }] }, /variables\[0\]\.finite: no finite 1/],
    [{ ...HOHMANN, variables: [HOHMANN.variables[0], HOHMANN.variables[0]] }, /burn1\.dv1 varied twice/],
    [{ ...HOHMANN, goals: [{ ...HOHMANN.goals[0], at: "burn5" }] }, /goals\[0\]\.at: no burn 5/],
    [{ ...HOHMANN, variables: [{ burn: 1, parameter: "duration" }] }, /one of dv1, dv2, dv3, epoch, got "duration"/],
    [{ ...HOHMANN, variables: [{ finite: 1, parameter: "dv1" }] }, /one of start, duration, got "dv1"/],
    [{ ...HOHMANN, goals: [{ ...HOHMANN.goals[0], at: "apoapsis" }] }, /end, burnN/],
    [{ ...HOHMANN, goals: [{ ...HOHMANN.goals[0], tolerance: 0 }] }, /tolerance/],
  ]) {
    assert.throws(() => parseProject(project(target)), message);
  }
  assert.throws(
    () => parseProject(v({ spacecraft: [{ id: "sat", propagation: PROPAGATION }], targets: [HOHMANN, HOHMANN] })),
    /targets\[1\]\.id: duplicate "hohmann"/,
  );
});

test("capture saves targets; helpers name and read variables", () => {
  globe.addSpacecraft("sat", PROPAGATION, {});
  addTarget(HOHMANN);
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
  const project = captureProject(state, { name: "Test" });
  assert.deepEqual(project.targets, [HOHMANN]);
  assert.doesNotThrow(() => parseProject(project));

  assert.equal(variableValue({ burn: 2, parameter: "dv1" }, PROPAGATION), 0.08);
  assert.equal(variableValue({ burn: 2, parameter: "epoch" }, PROPAGATION), 0);
  assert.ok(Number.isNaN(variableValue({ finite: 1, parameter: "duration" }, PROPAGATION)));
  assert.deepEqual(
    ["end", "burn2", "burn2.before", "finite1", "finite1.end", "2026-01-01T00:00:00Z"].map(atName),
    ["End", "After burn 2", "Before burn 2", "Start of finite 1", "End of finite 1", "2026-01-01T00:00:00Z"],
  );
});
