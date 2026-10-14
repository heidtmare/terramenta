import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import * as globe from "../src/globe.js";
import { openAttitudes, setAttitude } from "../src/attitude.js";
import {
  COVERAGE_LAYER,
  RAMP,
  addCoverage,
  coverageGeoJson,
  coverageRequest,
  coverageResult,
  coverageStudies,
  onResult,
  openCoverage,
  rampColor,
  removeCoverage,
  ring,
  runCoverage,
  savedCoverage,
  setMetric,
  showCoverage,
  subscribe,
  summarizeCoverage,
  syncCoverage,
  uniqueCoverageId,
} from "../src/coverage.js";
import { openEvents } from "../src/events.js";
import { PROJECT_VERSION, captureProject, parseProject } from "../src/project.js";
import { attachFake } from "./fake-globe.js";

globalThis.document = { baseURI: "http://localhost:8080/app/" };

const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });
const PROPAGATION = {
  initial: {
    epoch: "2026-01-01T00:00:00Z",
    elements: { smaKm: 7078, eccentricity: 0, inclinationDeg: 97.5, raanDeg: 0, argPerigeeDeg: 0, trueAnomalyDeg: 0 },
  },
  durationS: 3600,
};
const NADIR = {
  spacecraft: "a",
  primary: { body: [0, 0, 1], to: "nadir" },
  secondary: { body: [1, 0, 0], to: "velocity" },
  sensors: [{ label: "Imager", body: [0, 0, 1], fov: { halfAngleDeg: 20 } }, { body: [0, 1, 0], to: "sun" }],
};
const STUDY = { id: "both", spacecraft: ["a", "b"], resolution: 3, bounds: [-10, 30, 40, 70] };

let calls;
beforeEach(() => {
  calls = attachFake();
  openEvents(v({}));
  openAttitudes(v({}));
  openCoverage(v({}));
});

const named = (name) => calls.filter(([call]) => call === name);

test("requests: horizon for every spacecraft; sensors through attitude profiles", () => {
  globe.addSpacecraft("a", PROPAGATION, {});
  globe.addSpacecraft("b", PROPAGATION, {});
  assert.deepEqual(coverageRequest(STUDY), {
    spacecraft: [{ propagation: { ...PROPAGATION, kvn: false } }, { propagation: { ...PROPAGATION, kvn: false } }],
    resolution: 3,
    minElevationDeg: 0,
    stepS: 60,
    boundaries: true,
    bounds: [-10, 30, 40, 70],
  });
  setAttitude(NADIR);
  const sensed = coverageRequest({ ...STUDY, access: "sensors" });
  assert.equal(sensed.spacecraft.length, 1, "b has no profile");
  assert.deepEqual(sensed.spacecraft[0].sensors, [{ body: [0, 0, 1], halfAngleDeg: 20 }]);
  assert.deepEqual(sensed.spacecraft[0].attitude, { primary: NADIR.primary, secondary: NADIR.secondary });
  assert.throws(() => coverageRequest({ ...STUDY, spacecraft: ["b"], access: "sensors" }), /field of view/);
  assert.throws(() => coverageRequest({ ...STUDY, spacecraft: ["gone"] }), /No spacecraft "gone"/);
});

test("the shown study runs when stale and is drawn once per result and metric", () => {
  globe.addSpacecraft("a", PROPAGATION, {});
  globe.addSpacecraft("b", PROPAGATION, {});
  let edits = 0;
  let results = 0;
  const off = [subscribe(() => edits++), onResult(() => results++)];
  addCoverage(STUDY);
  syncCoverage();
  assert.equal(named("coverage").length, 0, "not shown, not run");
  assert.equal(coverageResult("both"), null);

  showCoverage("both");
  calls.length = 0;
  syncCoverage();
  syncCoverage();
  assert.equal(named("coverage").length, 1);
  const [[, id, options]] = named("addOverlay");
  assert.equal(id, COVERAGE_LAYER);
  const drawn = JSON.parse(options.text);
  assert.equal(drawn.features.length, 1, "uncovered cell left out of coverage");
  assert.equal(drawn.features[0].properties.coverage, "25.0 %");
  assert.equal(named("addOverlay").length, 1);

  setMetric("both", "maxGap");
  syncCoverage();
  assert.equal(named("coverage").length, 1, "metric change redraws without re-running");
  assert.equal(JSON.parse(named("addOverlay")[1][2].text).features.length, 2);

  globe.addSpacecraft("a", PROPAGATION, {});
  assert.equal(coverageResult("both").stale, true);
  syncCoverage();
  assert.equal(named("coverage").length, 2);
  assert.equal(coverageResult("both").stale, false);

  showCoverage(null);
  syncCoverage();
  assert.deepEqual(named("removeOverlay"), [["removeOverlay", COVERAGE_LAYER]]);
  off.forEach((unsubscribe) => unsubscribe());
  assert.deepEqual([edits, results], [4, 2]);
});

test("one study shown at a time; errors kept; ids unique", () => {
  addCoverage({ id: "x", spacecraft: ["a"], show: true });
  addCoverage({ id: "y", spacecraft: ["a"], show: true });
  assert.deepEqual(
    savedCoverage().map((study) => [study.id, !!study.show]),
    [
      ["x", false],
      ["y", true],
    ],
  );
  runCoverage("x");
  assert.match(coverageResult("x").error, /No spacecraft "a"/);
  assert.equal(uniqueCoverageId("X"), "x-2");
  assert.equal(uniqueCoverageId("Europe imaging"), "europe-imaging");
  removeCoverage("x");
  assert.equal(coverageResult("x"), null);
});

test("projects: references checked; capture keeps studies of present spacecraft", () => {
  const project = (study, extra = {}) =>
    v({
      spacecraft: [
        { id: "a", propagation: PROPAGATION },
        { id: "b", propagation: PROPAGATION },
      ],
      analysis: { coverage: [study] },
      ...extra,
    });
  assert.doesNotThrow(() => parseProject(project(STUDY)));
  assert.doesNotThrow(() => parseProject(project({ ...STUDY, access: "sensors" }, { attitudes: [NADIR] })));
  for (const [study, message] of [
    [{ ...STUDY, spacecraft: ["a", "gone"] }, /analysis\.coverage\[0\]\.spacecraft\[1\]: no spacecraft "gone"/],
    [{ ...STUDY, bounds: [0, 50, 10, 40] }, /south must not exceed north/],
    [{ ...STUDY, access: "sensors" }, /needs a spacecraft with an attitude sensor fov/],
    [{ ...STUDY, resolution: 8 }, /resolution/],
    [{ ...STUDY, spacecraft: [] }, /spacecraft/],
    [{ ...STUDY, metric: "best" }, /metric/],
  ]) {
    assert.throws(() => parseProject(project(study)), message);
  }
  assert.throws(
    () => parseProject(project(STUDY, { analysis: { coverage: [{ ...STUDY, show: true }, { ...STUDY, id: "two", show: true }] } })),
    /show on more than one study/,
  );
  const wide = { ...NADIR, sensors: [{ body: [0, 0, 1], fov: { halfAngleDeg: 200 } }] };
  assert.throws(() => parseProject(project(STUDY, { attitudes: [wide] })), /sensors\[0\]\.fov: expected \{halfAngleDeg\} or/);

  globe.addSpacecraft("a", PROPAGATION, {});
  addCoverage(STUDY);
  assert.deepEqual(coverageStudies({ spacecraftIds: new Set(["a"]) }), [{ ...STUDY, spacecraft: ["a"] }]);
  const state = {
    view: "globe",
    frame: { mode: "eci" },
    sun: { unixSeconds: 0, timeScale: 1, paused: false, shaded: true },
    camera: { center: { lat: 0, lon: 0 }, altitudeKm: 20_000 },
    imagery: { enabled: true, layerIndex: 0 },
    vectorTiles: { enabled: false, layerIndex: 0, style: { pointColor: "#fff", pointSizePx: 4, lineColor: "#fff", lineWidthPx: 1, fillColor: "#fff" } },
    overlays: { enabled: true, picking: false, layers: [{ id: COVERAGE_LAYER, label: "Coverage", url: null, style: {}, visible: true }] },
    ephemerides: { enabled: true, layers: [] },
    missions: [],
    placemarks: { enabled: true },
    gnc: { enabled: false, eciAxes: false, ecefAxes: false, graticule: false, graticuleStepDeg: 15, sidereal: false, track: false, trackOrbits: 1 },
    hud: { visible: false, helpVisible: false },
    keyboard: true,
  };
  const captured = captureProject(state, { name: "Test" });
  assert.deepEqual(captured.analysis.coverage, [{ ...STUDY, spacecraft: ["a"] }]);
  assert.deepEqual(captured.overlays.layers, [], "coverage layer not saved");
  assert.doesNotThrow(() => parseProject(captured));
});

test("rings unwrap across the antimeridian and close through a pole", () => {
  assert.deepEqual(
    ring([
      [179, 0],
      [-179, 0],
      [-179, 1],
    ]),
    [
      [179, 0],
      [181, 0],
      [181, 1],
      [179, 0],
    ],
  );
  const polar = ring([
    [0, 88],
    [120, 88],
    [-120, 88],
  ]);
  assert.deepEqual(polar, [
    [0, 88],
    [120, 88],
    [240, 88],
    [360, 88],
    [360, 90],
    [270, 90],
    [180, 90],
    [90, 90],
    [0, 90],
    [0, 88],
  ]);
  assert.equal(rampColor(0), RAMP[0]);
  assert.equal(rampColor(1), RAMP.at(-1));
  assert.equal(rampColor(Number.NaN), RAMP[0]);
});

test("summaries and GeoJSON", () => {
  const result = {
    cells: [
      { id: "1", fraction: 0.5, accesses: 3, maxGapS: 600, meanGapS: 300, boundary: [[0, 0], [1, 0], [1, 1]] },
      { id: "2", fraction: 0, accesses: 0, maxGapS: 3600, meanGapS: null, boundary: [[2, 0], [3, 0], [3, 1]] },
    ],
  };
  assert.deepEqual(summarizeCoverage(result), {
    cells: 2,
    coveredShare: 0.5,
    meanFraction: 0.25,
    medianMaxGapS: 3600,
    worstMaxGapS: 3600,
    meanRevisitS: 300,
  });
  const gaps = coverageGeoJson(result, "maxGap");
  assert.deepEqual(
    gaps.features.map((feature) => feature.properties.fill),
    [RAMP[0], RAMP.at(-1)],
  );
  assert.equal(coverageGeoJson(result, "meanGap").features.length, 1);
});
