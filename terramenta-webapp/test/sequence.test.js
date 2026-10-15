import assert from "node:assert/strict";
import { test } from "node:test";
import { burnCounts, describeCommand, flownShape, segmentRows } from "../src/sequence.js";

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
