import assert from "node:assert/strict";
import { test } from "node:test";
import { burnCounts, describeCommand, flownShape, segmentRows, sequenceErrors } from "../src/sequence.js";

const BURN = { durationS: 60, thrustN: 20, ispS: 220, direction: [1, 0, 0] };

test("burn counts include target bodies", () => {
  const sequence = [
    { maneuver: { deltaVKmS: [0.1, 0, 0] } },
    {
      target: {
        variables: [],
        goals: [],
        sequence: [{ finiteBurn: BURN }, { maneuver: { deltaVKmS: [0, 0, 0] } }, { propagate: { durationS: 1 } }],
      },
    },
  ];
  assert.deepEqual(burnCounts(sequence), { maneuvers: 2, finiteBurns: 1 });
  const shape = flownShape({ propagation: { initial: {} }, sequence });
  assert.equal(shape.maneuvers.length, 2);
  assert.equal(shape.finiteBurns.length, 1);
  const plain = { initial: {}, maneuvers: [] };
  assert.equal(flownShape({ propagation: plain }), plain);
});

test("commands describe themselves", () => {
  assert.equal(describeCommand({ propagate: { to: "apoapsis", count: 2 } }), "propagate to apoapsis #2");
  assert.equal(describeCommand({ propagate: { quantity: "altitudeKm", value: 400, direction: "decreasing" } }), "propagate to altitudeKm decreasing 400");
  assert.equal(describeCommand({ label: "toi", maneuver: { deltaVKmS: [0.0024, 0, 0] } }), "toi: maneuver [2.4, 0.0, 0.0] m/s vnb");
  assert.equal(describeCommand({ finiteBurn: BURN }), "finite burn 60 s at 20 N");
  assert.equal(
    describeCommand({ target: { variables: [{}], goals: [{ quantity: "smaKm" }, { quantity: "eccentricity" }], sequence: [] } }),
    "target 1 variable → smaKm, eccentricity",
  );
});

test("segment rows", () => {
  const elements = { smaKm: 7000, eccentricity: 0.001, inclinationDeg: 28.5, altitudeKm: 600 };
  const rows = segmentRows({
    segments: [
      { label: "toi", kind: "target", depth: 0, start: 0, stop: 100, massKg: 900, elements, target: { converged: false, iterations: 25 } },
      { kind: "maneuver", depth: 1, start: 0, stop: 0, massKg: 900, deltaVKmS: 0.05, elements },
    ],
  });
  assert.deepEqual(
    rows.map((r) => [r.name, r.depth, r.durationS, r.deltaVMS, r.status]),
    [
      ["Target · toi", 0, 100, null, "NOT converged in 25"],
      ["Maneuver", 1, 0, 50, null],
    ],
  );
});

const OPTIMIZE = {
  label: "gto",
  optimize: {
    variables: [
      { command: "toi", parameter: "dv1", lower: 0, upper: 3 },
      { command: "goi", parameter: "dv2" },
    ],
    minimize: "deltaV",
    constraints: [{ quantity: "smaKm", value: 42164, tolerance: 0.01 }, { quantity: "eccentricity", relation: "atMost", value: 1e-4, tolerance: 1e-6 }],
    sequence: [
      { label: "toi", maneuver: { deltaVKmS: [2.4, 0, 0] } },
      { propagate: { to: "apoapsis" } },
      { label: "goi", maneuver: { deltaVKmS: [1.5, 0, 0] } },
    ],
  },
};

test("optimize bodies validate, count and describe", () => {
  assert.deepEqual(sequenceErrors([OPTIMIZE]), []);
  assert.deepEqual(burnCounts([OPTIMIZE]), { maneuvers: 2, finiteBurns: 0 });
  assert.equal(describeCommand(OPTIMIZE), "gto: optimize 2 variables: minimize deltaV, 2 constraints");
  const bad = structuredClone(OPTIMIZE);
  bad.optimize.maximize = "massKg";
  bad.optimize.variables[0].lower = 5;
  bad.optimize.variables[1].command = "nope";
  assert.deepEqual(sequenceErrors([bad]), [
    "sequence[0].optimize.variables[0]: lower above upper",
    'sequence[0].optimize.variables[1]: no command "nope" in this optimize\'s sequence',
    "sequence[0].optimize: exactly one of minimize, maximize",
  ]);
});

test("optimize segment status", () => {
  const elements = { smaKm: 42164, eccentricity: 0, inclinationDeg: 10, altitudeKm: 35786 };
  const segment = (optimize) => ({ kind: "optimize", depth: 0, start: 0, stop: 1, massKg: 1, elements, optimize });
  const rows = segmentRows({
    segments: [
      segment({ converged: true, feasible: true, iterations: 9, objective: 4.044094 }),
      segment({ converged: false, feasible: true, iterations: 50, objective: 4.1 }),
      segment({ converged: false, feasible: false, iterations: 50, objective: 3.9 }),
    ],
  });
  assert.deepEqual(rows.map((r) => r.status), ["optimal in 9 · f 4.04409", "NOT optimal in 50 · f 4.10000", "NOT feasible in 50 · f 3.90000"]);
});
