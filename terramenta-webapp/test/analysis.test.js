import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import * as analysis from "../src/analysis.js";
import * as globe from "../src/globe.js";
import { PROJECT_VERSION, ProjectError, captureProject, parseProject } from "../src/project.js";
import { attachFake } from "./fake-globe.js";

globalThis.document = { baseURI: "http://localhost:8080/app/" };

const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });
const INITIAL = { epoch: "2026-01-01T00:00:00Z", positionKm: [6878, 0, 0], velocityKmS: [0, 4.7, 6] };
const craft = (id) => ({ id, propagation: { initial: INITIAL, durationS: 3600 } });
const STUDY = {
  id: "mars-2026",
  label: "Earth → Mars 2026",
  origin: "Earth",
  destination: "Mars",
  departure: { start: "2026-09-01", end: "2027-01-31" },
  arrival: { start: "2027-06-01", end: "2028-03-01" },
};

function errorsOf(json) {
  try {
    parseProject(json, { warn: () => {} });
  } catch (error) {
    assert.ok(error instanceof ProjectError, error);
    return error.errors;
  }
  assert.fail("expected a ProjectError");
}

beforeEach(() => {
  attachFake();
  analysis.openAnalysis(v({}));
});

test("analysis is typed and cross-checked", () => {
  const good = v({
    spacecraft: [craft("a"), craft("b")],
    analysis: { plots: [{ id: "alt", quantity: "altitudeKm", spacecraft: ["a", "b"] }], porkchops: [STUDY] },
  });
  assert.deepEqual(parseProject(good, { warn: assert.fail }).analysis, good.analysis);

  assert.deepEqual(
    errorsOf(
      v({
        analysis: {
          plots: [{ id: "x", quantity: "mass", spacecraft: [] }],
          porkchops: [{ ...STUDY, origin: "Venus", steps: 1 }],
        },
      }),
    ),
    [
      "analysis.plots[0].quantity: expected one of altitudeKm, radiusKm, speedKmS, smaKm, eccentricity, inclinationDeg, raanDeg, argPerigeeDeg, trueAnomalyDeg, periodMin, perigeeAltKm, apogeeAltKm, got \"mass\"",
      "analysis.plots[0].spacecraft: expected ≥ 1 items, got []",
      "analysis.porkchops[0].origin: expected one of Sun, Earth, Mars, got \"Venus\"",
      "analysis.porkchops[0].steps: expected a number ≥ 2, got 1",
    ],
  );

  assert.deepEqual(
    errorsOf(
      v({
        spacecraft: [craft("a")],
        analysis: {
          plots: [
            { id: "p", quantity: "smaKm", spacecraft: ["a", "gone"] },
            { id: "p", quantity: "smaKm", spacecraft: ["a"] },
          ],
          porkchops: [{ ...STUDY, destination: "Earth", arrival: { start: "2028-01-01", end: "2027-01-01" } }],
        },
      }),
    ),
    [
      'analysis.plots[1].id: duplicate "p"',
      'analysis.plots[0].spacecraft[1]: no spacecraft "gone"',
      "analysis.porkchops[0]: origin and destination are the same body",
      "analysis.porkchops[0].arrival: start must be before end",
    ],
  );
});

test("edits notify, ids stay unique, saving drops removed spacecraft", () => {
  let edits = 0;
  const unsubscribe = analysis.subscribe(() => edits++);
  analysis.openAnalysis(
    v({
      analysis: {
        plots: [
          { id: "alt", quantity: "altitudeKm", spacecraft: ["a", "b"] },
          { id: "b-only", quantity: "smaKm", spacecraft: ["b"] },
        ],
      },
    }),
  );
  assert.equal(edits, 0, "opening is not an edit");
  assert.equal(analysis.uniqueId("plots", "alt"), "alt-2");
  analysis.addPorkchop(STUDY);
  analysis.addPorkchop({ ...STUDY, label: "Renamed" });
  assert.deepEqual(analysis.savedPorkchops().map((study) => study.label), ["Renamed"], "same id replaces");
  analysis.removePlot("nothing");
  assert.equal(edits, 3);
  unsubscribe();

  assert.deepEqual(analysis.analysisSection({ spacecraftIds: new Set(["a"]) }), {
    plots: [{ id: "alt", quantity: "altitudeKm", spacecraft: ["a"] }],
    porkchops: [{ ...STUDY, label: "Renamed" }],
  });
  analysis.removePorkchop(STUDY.id);
  assert.equal(analysis.analysisSection({ spacecraftIds: new Set() }), undefined);
});

test("captured projects carry analysis and parse cleanly", () => {
  globe.addSpacecraft("a", craft("a").propagation, { label: "A" });
  analysis.openAnalysis(
    v({ analysis: { plots: [{ id: "alt", quantity: "altitudeKm", spacecraft: ["a", "gone"] }], porkchops: [STUDY] } }),
  );
  const state = {
    view: "globe",
    frame: { mode: "eci" },
    sun: { unixSeconds: 1_790_000_000, timeScale: 60, paused: false, shaded: true },
    camera: { center: { lat: 0, lon: 0 }, altitudeKm: 20_000 },
    missions: [],
    imagery: { enabled: true, layerIndex: 0 },
    vectorTiles: { enabled: false, layerIndex: 0, style: {} },
    overlays: { enabled: true, picking: true, layers: [] },
    ephemerides: { enabled: true, layers: [] },
    placemarks: { enabled: true },
    gnc: { enabled: false },
    hud: { visible: false, helpVisible: false },
    keyboard: true,
  };
  const captured = JSON.parse(JSON.stringify(captureProject(state)));
  assert.deepEqual(captured.analysis, {
    plots: [{ id: "alt", quantity: "altitudeKm", spacecraft: ["a"] }],
    porkchops: [STUDY],
  });
  parseProject(captured, { warn: assert.fail });
});
