import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { openEvents } from "../src/events.js";
import * as globe from "../src/globe.js";
import {
  addStudy,
  applyEstimate,
  estimatedPropagation,
  normalisedResiduals,
  odRequest,
  odResult,
  odStudies,
  openOrbitDetermination,
  removeStudy,
  residualRows,
  residualStats,
  runOrbitDetermination,
  uniqueStudyId,
} from "../src/od.js";
import { PROJECT_VERSION, parseProject } from "../src/project.js";
import { attachFake } from "./fake-globe.js";

globalThis.document = { baseURI: "http://localhost:8080/app/" };

const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });
const PRIOR = {
  initial: { epoch: "2026-01-01T00:00:00Z", positionKm: [7003, 0, 0], velocityKmS: [0, 3.19, 6.84] },
  durationS: 21600,
  covariance: { positionSigmaKm: [5, 5, 5], velocitySigmaKmS: [0.005, 0.005, 0.005], processNoiseKm2S3: [1e-12, 1e-12, 1e-12] },
};
const TRUTH = { initial: { epoch: "2026-01-01T00:00:00Z", positionKm: [7000, 0, 0], velocityKmS: [0, 3.19, 6.84] }, durationS: 21600 };
const STATIONS = [
  { id: "svalbard", latDeg: 78.23, lonDeg: 15.39, minElevationDeg: 5 },
  { id: "wallops", latDeg: 37.94, lonDeg: -75.46 },
];
const STUDY = {
  id: "od",
  spacecraft: "prior",
  simulate: { truth: "truth", tracking: [{ type: "range", sigma: 0.01 }, { type: "azimuth", station: "wallops" }], stepS: 30 },
  solveFor: [{ parameter: "dragCoefficient", sigma: 0.5 }],
};

let calls;
beforeEach(() => {
  calls = attachFake();
  openOrbitDetermination(v({}));
  openEvents(v({ groundStations: STATIONS }));
  globe.addSpacecraft("prior", PRIOR, {});
  globe.addSpacecraft("truth", TRUTH, {});
});

const named = (name) => calls.filter(([call]) => call === name);

test("requests carry the a priori, stations, truth and options", () => {
  const request = odRequest({ ...STUDY, stations: ["wallops"], editSigma: 4 });
  assert.deepEqual(request, {
    propagation: { ...PRIOR, kvn: false },
    stations: [{ id: "wallops", latDeg: 37.94, lonDeg: -75.46, altKm: 0, minElevationDeg: 0, horizon: [] }],
    estimator: "batch",
    lightTime: true,
    exportTdm: true,
    simulate: { tracking: STUDY.simulate.tracking, stepS: 30, truth: { ...TRUTH, kvn: false } },
    solveFor: STUDY.solveFor,
    editSigma: 4,
  });
  assert.equal(odRequest({ id: "t", spacecraft: "prior", tdm: "a.tdm" }, "TEXT").tdm, "TEXT");
  assert.throws(() => odRequest({ ...STUDY, spacecraft: "gone" }), /No spacecraft "gone"/);
  openEvents(v({}));
  assert.throws(() => odRequest(STUDY), /needs ground stations/);
});

test("results are kept until inputs change, summarised and applied", async () => {
  addStudy(STUDY);
  assert.equal(odResult("od"), null);
  await runOrbitDetermination("od");
  const kept = odResult("od");
  assert.equal(kept.stale, false);
  assert.equal(kept.result.residuals.length, 3);
  const stats = residualStats(kept.result);
  assert.deepEqual(
    stats.map((g) => [g.station, g.type, g.count]),
    [
      ["svalbard", "range", 1],
      ["wallops", "range", 1],
      ["wallops", "azimuth", 1],
    ],
  );
  assert.equal(stats[0].rms, 0.002);
  assert.deepEqual(normalisedResiduals(kept.result)[0].points[0][1], 0.2);
  assert.deepEqual(residualRows(kept.result)[0], ["2026-01-01T00:00:00.000Z", "svalbard", "range", 1000, 0.002, "", 0.01, 0]);

  const propagation = estimatedPropagation("od");
  assert.deepEqual(propagation.initial, kept.result.estimate.initial);
  assert.equal(propagation.vehicle.dragCoefficient, 2.4);
  assert.deepEqual(propagation.covariance.processNoiseKm2S3, PRIOR.covariance.processNoiseKm2S3);
  assert.equal(propagation.start, undefined, "batch estimate at the a priori epoch");
  applyEstimate("od");
  assert.equal(globe.recordedSources().spacecraft.get("prior").propagation.vehicle.dragCoefficient, 2.4);
  assert.deepEqual(odResult("od"), { ...odResult("od"), stale: false, applied: true });

  globe.addSpacecraft("truth", { ...TRUTH, durationS: 3600 }, {});
  assert.equal(odResult("od").stale, true);
  assert.throws(() => estimatedPropagation("od"), /run first/);
  assert.equal(named("determineOrbit").length, 1);
  removeStudy("od");
  assert.equal(odResult("od"), null);
});

test("an EKF estimate keeps the a priori span", async () => {
  addStudy({ id: "ekf", spacecraft: "prior", estimator: "ekf", simulate: { truth: "truth", tracking: [{ type: "positionX" }] } });
  await runOrbitDetermination("ekf");
  const kept = odResult("ekf");
  assert.ok(kept.result.history);
  assert.deepEqual(residualStats(kept.result)[0].station, null);
  assert.equal(estimatedPropagation("ekf").start, "2026-01-01T00:00:00Z");
});

test("errors are kept; TDM is fetched", async () => {
  addStudy({ id: "none", spacecraft: "prior", tdm: "missing.tdm" });
  globalThis.fetch = async (url) => ({ ok: String(url).endsWith("ok.tdm"), status: 404, text: async () => "TDM" });
  await runOrbitDetermination("none");
  assert.match(odResult("none").error, /HTTP 404/);
  addStudy({ id: "tdm", spacecraft: "prior", tdm: "ok.tdm" });
  await runOrbitDetermination("tdm");
  assert.equal(named("determineOrbit").at(-1)[1].tdm, "TDM");
  delete globalThis.fetch;
});

test("project references are checked and dropped on capture", () => {
  const project = v({
    spacecraft: [
      { id: "prior", propagation: PRIOR },
      { id: "truth", propagation: TRUTH },
    ],
    groundStations: STATIONS,
    analysis: { orbitDetermination: [STUDY] },
  });
  parseProject(project);
  const bad = (study) => {
    try {
      parseProject({ ...project, analysis: { orbitDetermination: [study] } });
    } catch (error) {
      return error.errors.join("\n");
    }
    return "";
  };
  assert.match(bad({ ...STUDY, spacecraft: "gone" }), /spacecraft: no spacecraft "gone"/);
  assert.match(bad({ ...STUDY, simulate: { ...STUDY.simulate, truth: "prior" } }), /a priori spacecraft itself/);
  assert.match(bad({ ...STUDY, stations: ["nowhere"] }), /no ground station "nowhere"/);
  assert.match(bad({ id: "x", spacecraft: "prior" }), /needs tdm or simulate/);
  assert.match(bad({ ...STUDY, estimator: "ekf" }), /solveFor: batch only/);
  assert.match(bad({ ...STUDY, simulate: { ...STUDY.simulate, tracking: [{ type: "positionX", station: "wallops" }] } }), /takes no station/);
  assert.match(bad({ ...STUDY, estimator: "lsq" }), /one of batch, ekf/);

  addStudy(STUDY);
  addStudy({ id: "gnss", spacecraft: "prior", simulate: { truth: "gone", tracking: [{ type: "positionX" }] } });
  const ids = { spacecraftIds: new Set(["prior", "truth"]), stationIds: new Set(["svalbard"]) };
  assert.deepEqual(odStudies(ids), [{ ...STUDY, simulate: { ...STUDY.simulate, tracking: [STUDY.simulate.tracking[0]] } }]);
  assert.equal(uniqueStudyId("OD"), "od-2");
});
