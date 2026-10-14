import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import * as globe from "../src/globe.js";
import {
  addConjunction,
  conjunctionRequest,
  conjunctionResult,
  conjunctionRows,
  conjunctionStudies,
  formatPc,
  openConjunctions,
  ranked,
  removeConjunction,
  runConjunction,
  savedConjunctions,
  uniqueConjunctionId,
} from "../src/conjunction.js";
import { PROJECT_VERSION, captureProject, parseProject } from "../src/project.js";
import { attachFake } from "./fake-globe.js";

globalThis.document = { baseURI: "http://localhost:8080/app/" };

const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });
const PROPAGATION = {
  initial: { epoch: "2026-01-01T00:00:00Z", positionKm: [6878, 0, 0], velocityKmS: [0, 4.7, 6] },
  durationS: 21600,
};
const COVARIANT = { ...PROPAGATION, covariance: { positionSigmaKm: [0.05, 0.2, 0.03] } };
const STUDY = { id: "screen", primary: "a", secondaries: ["b"], layers: ["stations"], secondarySigmaKm: [0.2, 1, 0.2] };

let calls;
beforeEach(() => {
  calls = attachFake({ objects: { stations: [{ noradId: 25544, name: "ISS (ZARYA)" }] } });
  openConjunctions(v({}));
});

const named = (name) => calls.filter(([call]) => call === name);

test("requests carry propagations, loaded layers and the screening options", () => {
  globe.addSpacecraft("a", COVARIANT, {});
  globe.addSpacecraft("b", PROPAGATION, {});
  const { request, missing } = conjunctionRequest({ ...STUDY, layers: ["stations", "starlink"] });
  assert.deepEqual(missing, ["starlink"]);
  assert.deepEqual(request, {
    primary: { ...COVARIANT, kvn: false },
    secondaries: [{ id: "b", propagation: { ...PROPAGATION, kvn: false } }, { layer: "stations" }],
    thresholdKm: 5,
    hardBodyRadiusM: 20,
    stepS: 60,
    cdm: true,
    secondarySigmaKm: [0.2, 1, 0.2],
  });
  assert.throws(() => conjunctionRequest({ id: "x", primary: "a", layers: ["starlink"] }), /Not loaded: starlink/);
  assert.throws(() => conjunctionRequest({ ...STUDY, primary: "gone" }), /No spacecraft "gone"/);
});

test("results are kept until inputs change, ranked by Pc, and exported", () => {
  globe.addSpacecraft("a", COVARIANT, {});
  globe.addSpacecraft("b", PROPAGATION, {});
  addConjunction(STUDY);
  assert.equal(conjunctionResult("screen"), null);
  runConjunction("screen");
  const kept = conjunctionResult("screen");
  assert.equal(kept.stale, false);
  assert.equal(kept.result.conjunctions.length, 2);
  assert.deepEqual(
    ranked(kept.result).map((c) => c.pc),
    [1e-4, 5e-5],
  );
  const rows = conjunctionRows(kept.result);
  assert.deepEqual(rows[0].slice(0, 6), ["2026-01-01T01:00:00.000Z", "b", "SPACECRAFT", "", 100, 10000]);
  assert.equal(rows[1][3], 25544);
  assert.equal(formatPc(null), "—");
  assert.equal(formatPc(1e-12), "< 1e-10");
  assert.equal(formatPc(1.234e-4), "1.2e-4");
  globe.addSpacecraft("b", { ...PROPAGATION, durationS: 3600 }, {});
  assert.equal(conjunctionResult("screen").stale, true);
  assert.equal(named("conjunctions").length, 1);
  removeConjunction("screen");
  assert.equal(conjunctionResult("screen"), null);
});

test("failures are kept as errors", () => {
  globe.addSpacecraft("a", PROPAGATION, {});
  openConjunctions(v({ analysis: { conjunctions: [{ id: "x", primary: "a", secondaries: ["missing"] }] } }));
  runConjunction("x");
  assert.match(conjunctionResult("x").error, /No spacecraft "missing"/);
});

test("projects validate references and capture studies without stale ones", () => {
  const project = v({
    ephemerides: { layers: [{ id: "stations", url: "https://celestrak.org/x" }] },
    spacecraft: [
      { id: "a", propagation: COVARIANT },
      { id: "b", propagation: PROPAGATION },
    ],
    analysis: { conjunctions: [STUDY] },
  });
  assert.doesNotThrow(() => parseProject(project, { warn: () => {} }));
  const bad = structuredClone(project);
  bad.analysis.conjunctions = [{ id: "y", primary: "a", layers: ["stations"], thresholdKm: 0 }];
  assert.throws(() => parseProject(bad, { warn: () => {} }), /conjunctions\[0\]\.thresholdKm: expected a number > 0/);
  bad.analysis.conjunctions = [
    { id: "x", primary: "zz", secondaries: ["a", "a"], layers: ["nope"] },
    { id: "x", primary: "a" },
  ];
  assert.throws(
    () => parseProject(bad, { warn: () => {} }),
    (error) => {
      const text = error.errors.join("\n");
      for (const expected of [
        /conjunctions\[0\]\.primary: no spacecraft "zz"/,
        /conjunctions\[0\]\.layers\[0\]: no ephemeris layer "nope"/,
        /conjunctions\[1\]\.id: duplicate "x"/,
        /conjunctions\[1\]: needs secondaries or layers/,
      ]) {
        if (!expected.test(text)) assert.fail(`${expected} not in\n${text}`);
      }
      return true;
    },
  );

  openConjunctions(project);
  assert.deepEqual(savedConjunctions(), [STUDY]);
  assert.deepEqual(conjunctionStudies({ spacecraftIds: new Set(["a"]), ephemerisIds: new Set(["stations"]) }), [
    { id: "screen", primary: "a", layers: ["stations"], secondarySigmaKm: [0.2, 1, 0.2] },
  ]);
  assert.deepEqual(conjunctionStudies({ spacecraftIds: new Set(["a"]), ephemerisIds: new Set() }), []);
  assert.equal(uniqueConjunctionId("Screen"), "screen-2");
});
