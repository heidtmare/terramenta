import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import * as globe from "../src/globe.js";
import {
  addStudy,
  histogram,
  monteCarloRequest,
  monteCarloResult,
  monteCarloStudies,
  openMonteCarlo,
  removeStudy,
  runMonteCarlo,
  trialColumns,
  trialRows,
  uniqueStudyId,
} from "../src/monte-carlo.js";
import { PROJECT_VERSION, parseProject } from "../src/project.js";
import { attachFake } from "./fake-globe.js";

const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });
const BURNS = {
  initial: { epoch: "2026-01-01T00:00:00Z", positionKm: [7000, 0, 0], velocityKmS: [0, 7.5, 0] },
  durationS: 7200,
  covariance: { positionSigmaKm: [0.1, 1, 0.1] },
  maneuvers: [{ epoch: "2026-01-01T00:30:00Z", deltaVKmS: [0.01, 0, 0] }],
};
const BASE = { initial: BURNS.initial, durationS: 60, attitude: { primary: { body: [1, 0, 0], to: "velocity" } } };
const SEQUENCE = [{ label: "burn", maneuver: { deltaVKmS: [0.1, 0, 0] } }, { propagate: { to: "apoapsis" } }];
const STUDY = {
  id: "mc",
  spacecraft: "burns",
  samples: 4,
  guidance: "closed",
  dispersions: { maneuverMagnitudeSigma: 0.01, massSigmaKg: 2 },
  quantities: ["altitudeKm", "smaKm"],
};

let calls;
beforeEach(() => {
  calls = attachFake();
  openMonteCarlo(v({}));
  globe.addSpacecraft("burns", BURNS, {});
  globe.addSpacecraft("sequenced", BASE, {}, { sequence: SEQUENCE });
});

const named = (name) => calls.filter(([call]) => call === name);

test("requests: burns fly the propagation, sequences the given one with its attitude", () => {
  assert.deepEqual(monteCarloRequest(STUDY), [
    { ...BURNS, kvn: false },
    null,
    { samples: 4, seed: 1, guidance: "closed", quantities: ["altitudeKm", "smaKm"], maneuverMagnitudeSigma: 0.01, massSigmaKg: 2 },
  ]);
  const [propagation, sequence, options] = monteCarloRequest({ id: "s", spacecraft: "sequenced" });
  assert.deepEqual(sequence, SEQUENCE);
  assert.equal(propagation.maneuvers, undefined);
  assert.deepEqual(propagation.attitude, BASE.attitude);
  assert.deepEqual(options, { samples: 100, seed: 1, guidance: "open", quantities: ["altitudeKm", "smaKm", "eccentricity", "inclinationDeg", "raanDeg"] });
  assert.throws(() => monteCarloRequest({ ...STUDY, spacecraft: "gone" }), /No spacecraft "gone"/);
});

test("results are kept until inputs change; CSV rows per trial", async () => {
  addStudy(STUDY);
  assert.equal(monteCarloResult("mc"), null);
  await runMonteCarlo("mc");
  const kept = monteCarloResult("mc");
  assert.equal(kept.stale, false);
  assert.equal(kept.result.failed, 1);
  assert.deepEqual(trialColumns(kept.result).slice(0, 4), ["trial", "altitudeKm", "smaKm", "delta_v_km_s"]);
  const rows = trialRows(kept.result);
  assert.equal(rows.length, 5);
  assert.deepEqual(rows[1], [1, 401, 401, 0.1, 3600, 1, -1, 0, 900, 1, ""]);
  assert.deepEqual(rows[3], [3, "", "", "", "", "", "", "", "", 0, "not reached"]);

  addStudy({ ...STUDY, label: "Renamed" });
  assert.equal(monteCarloResult("mc").stale, false, "label is not an input");
  globe.addSpacecraft("burns", { ...BURNS, durationS: 3600 }, {});
  assert.equal(monteCarloResult("mc").stale, true);
  assert.equal(named("monteCarlo").length, 1);
  removeStudy("mc");
  assert.equal(monteCarloResult("mc"), null);
});

test("errors are kept", async () => {
  globe.addSpacecraft("empty", BASE, {}, { sequence: SEQUENCE });
  globe.recordedSources().spacecraft.get("empty").sequence = [];
  addStudy({ id: "bad", spacecraft: "empty" });
  await runMonteCarlo("bad");
  assert.match(monteCarloResult("bad").error, /sequence is empty/);
});

test("histogram bins the finite range", () => {
  assert.deepEqual(histogram([0, 1, 2, 3, 4, NaN], 4), { edges: [0, 1, 2, 3, 4], counts: [1, 1, 1, 2] });
  assert.deepEqual(histogram([5, 5]), { edges: [5, 5], counts: [2] });
  assert.deepEqual(histogram([]), { edges: [], counts: [] });
});

test("project references are checked and dropped on capture", () => {
  const project = v({ spacecraft: [{ id: "burns", propagation: BURNS }], analysis: { monteCarlo: [STUDY] } });
  parseProject(project);
  const bad = (study) => {
    try {
      parseProject({ ...project, analysis: { monteCarlo: [study] } });
    } catch (error) {
      return error.errors.join("\n");
    }
    return "";
  };
  assert.match(bad({ ...STUDY, spacecraft: "gone" }), /spacecraft: no spacecraft "gone"/);
  assert.match(bad({ ...STUDY, guidance: "x" }), /one of open, closed/);
  assert.match(bad({ ...STUDY, samples: 0 }), /samples/);
  assert.match(bad({ ...STUDY, quantities: ["x"] }), /quantities/);
  assert.match(bad({ ...STUDY, dispersions: { thrustSigma: -1 } }), /thrustSigma/);

  addStudy(STUDY);
  addStudy({ id: "other", spacecraft: "gone" });
  assert.deepEqual(monteCarloStudies({ spacecraftIds: new Set(["burns"]) }), [STUDY]);
  assert.equal(uniqueStudyId("MC"), "mc-2");
});
