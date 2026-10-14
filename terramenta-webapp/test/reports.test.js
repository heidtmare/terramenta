import assert from "node:assert/strict";
import { test } from "node:test";
import { niceTicks } from "../src/chart.js";
import {
  EARTH_RADIUS_KM,
  MU_EARTH,
  QUANTITIES,
  deltaVBudget,
  epochSeconds,
  parseOem,
  quantities,
  reportCsv,
  reportRows,
  summarize,
  toCsv,
  toCzml,
  withUncertainty,
} from "../src/reports.js";

const close = (actual, expected, tolerance, message) =>
  assert.ok(Math.abs(actual - expected) <= tolerance, `${message ?? ""} ${actual} vs ${expected}`);

const OEM = `CCSDS_OEM_VERS = 2.0
CREATION_DATE = 2026-01-01T00:00:00
ORIGINATOR = TERRAMENTA

META_START
OBJECT_NAME = DEMO
OBJECT_ID = DEMO
CENTER_NAME = EARTH
REF_FRAME = GCRF
TIME_SYSTEM = UTC
START_TIME = 2026-01-01T00:00:00.000000000
STOP_TIME = 2026-01-01T00:01:00.000000000
META_STOP

COMMENT a comment
2026-01-01T00:00:00.000000000 6878.137 0 0 0 4.728554669 5.965951219
2026-01-01T00:01:00.250000000 6870 300 380 -0.3 4.72 5.96

COVARIANCE_START
EPOCH = 2026-01-01T00:00:00
1 2 3
COVARIANCE_STOP

META_START
OBJECT_NAME = DEMO
REF_FRAME = GCRF
META_STOP
2026-01-01T00:01:00.250000000 6870 300 380 -0.3 4.80 6.00
`;

test("OEM: segments, sub-millisecond epochs, comments and covariance skipped", () => {
  const { segments } = parseOem(OEM);
  assert.equal(segments.length, 2);
  assert.deepEqual(
    segments.map((segment) => [segment.name, segment.frame, segment.samples.length]),
    [["DEMO", "GCRF", 2], ["DEMO", "GCRF", 1]],
  );
  assert.equal(segments[0].samples[1].t, Date.parse("2026-01-01T00:01:00Z") / 1000 + 0.25);
  assert.deepEqual(segments[1].samples[0].v, [-0.3, 4.8, 6]);
  assert.throws(() => parseOem("META_START\nMETA_STOP\n2026-01-01T00:00:00 1 2 x 4 5 6"), /line 3/);
  assert.equal(epochSeconds("2026-01-01T00:00:00+01:00"), Date.parse("2025-12-31T23:00:00Z") / 1000);
});

test("quantities: circular inclined LEO", () => {
  const q = quantities([6878.137, 0, 0], [0, 4.728554669, 5.965951219]);
  close(q.altitudeKm, 500, 1e-6);
  close(q.speedKmS, Math.sqrt(MU_EARTH / 6878.137), 1e-6);
  close(q.smaKm, 6878.137, 0.01);
  assert.ok(q.eccentricity < 1e-6);
  close(q.inclinationDeg, (Math.atan2(5.965951219, 4.728554669) * 180) / Math.PI, 1e-9);
  close(q.raanDeg, 0, 1e-9);
  close(q.periodMin, 94.62, 0.01);
  close(q.trueAnomalyDeg, 0, 1e-6, "argument of latitude at the node");
  for (const key of Object.keys(QUANTITIES)) assert.ok(key in q, key);
});

test("quantities: elliptical equatorial, both sides of perigee", () => {
  const perigee = quantities([7000, 0, 0], [0, 8, 0]);
  const energy = 32 - MU_EARTH / 7000;
  close(perigee.smaKm, -MU_EARTH / (2 * energy), 1e-6);
  close(perigee.eccentricity, ((64 - MU_EARTH / 7000) * 7000) / MU_EARTH, 1e-12);
  close(perigee.perigeeAltKm, 7000 - EARTH_RADIUS_KM, 1e-6);
  close(perigee.trueAnomalyDeg, 0, 1e-9);
  close(perigee.inclinationDeg, 0, 1e-9);
  // Approaching perigee (r·v < 0): true anomaly past 180°.
  const inbound = quantities([7000, 100, 0], [-0.5, 8, 0]);
  assert.ok(inbound.trueAnomalyDeg > 180, `${inbound.trueAnomalyDeg}`);
  // Escape: no period or apogee.
  const hyperbolic = quantities([7000, 0, 0], [0, 12, 0]);
  assert.ok(hyperbolic.eccentricity > 1);
  assert.ok(Number.isNaN(hyperbolic.periodMin) && Number.isNaN(hyperbolic.apogeeAltKm));
});

test("report rows, summary and CSV", () => {
  const rows = reportRows(parseOem(OEM));
  assert.deepEqual(rows.map((row) => row.segment), [0, 0, 1]);
  const altitude = summarize(rows, "altitudeKm");
  close(altitude.first, 500, 1e-6);
  assert.equal(altitude.last, rows[2].altitudeKm);
  assert.equal(summarize([], "altitudeKm"), null);

  const csv = reportCsv(rows, ["altitudeKm", "periodMin"]).split("\r\n");
  assert.equal(csv[0], "utc,segment,x_km,y_km,z_km,vx_km_s,vy_km_s,vz_km_s,altitude_km,period_min");
  assert.match(csv[1], /^2026-01-01T00:00:00\.000Z,0,6878\.137,0,0,0,4\.728554669,5\.965951219,500,94\.6/);
  assert.equal(csv.length, 5, "header, three rows, trailing newline");

  assert.equal(toCsv(["a", "b"], [['x,"y"', NaN], [1, null]]), 'a,b\r\n"x,""y""",\r\n1,\r\n');
});

test("Δv budget: rocket equation, Isp-less burns free, finite burns cut at dry mass", () => {
  const g0 = 9.80665;
  const budget = deltaVBudget({
    vehicle: { massKg: 500, dryMassKg: 450 },
    maneuvers: [
      { epoch: "2026-01-01T01:00:00Z", deltaVKmS: [0.03, 0.04, 0], ispS: 300 },
      { epoch: "2026-01-01T00:10:00Z", deltaVKmS: [0.01, 0, 0] },
    ],
    finiteBurns: [{ start: "2026-01-01T02:00:00Z", durationS: 1e6, thrustN: 10, ispS: 200, direction: [1, 0, 0] }],
  });
  assert.deepEqual(budget.burns.map((burn) => [burn.kind, burn.index]), [["impulsive", 2], ["impulsive", 1], ["finite", 1]]);
  close(budget.burns[0].deltaVKmS, 0.01, 1e-12);
  assert.equal(budget.burns[0].massAfterKg, 500, "no Isp, no propellant");
  const afterFirst = 500 * Math.exp(-50 / (300 * g0));
  close(budget.burns[1].massAfterKg, afterFirst, 1e-9);
  assert.equal(budget.burns[2].massAfterKg, 450, "cut off at dry mass");
  close(budget.burns[2].deltaVKmS, (200 * g0 * Math.log(afterFirst / 450)) / 1000, 1e-12);
  close(budget.totalDeltaVKmS, 0.06 + budget.burns[2].deltaVKmS, 1e-12);
  close(budget.propellantKg, 50, 1e-9);

  // Dry mass defaults to min(850, wet): a 500 kg vehicle has no finite-burn propellant.
  const dry = deltaVBudget({
    vehicle: { massKg: 500 },
    finiteBurns: [{ start: "2026-01-01T00:00:00Z", durationS: 60, thrustN: 10, ispS: 200, direction: [1, 0, 0] }],
  });
  assert.equal(dry.totalDeltaVKmS, 0);
});

test("CZML: document clock, one inertial position interval per segment, metres", () => {
  const oem = parseOem(OEM);
  const [document, packet] = toCzml("Demo", [{ id: "demo", label: "Demo", color: "#ffb86ba6", oem }], {
    unixSeconds: 0,
    timeScale: 30,
  });
  assert.equal(document.id, "document");
  assert.equal(document.clock.interval, "2026-01-01T00:00:00.000Z/2026-01-01T00:01:00.250Z");
  assert.equal(document.clock.currentTime, "2026-01-01T00:00:00.000Z", "clamped into the span");
  assert.equal(document.clock.multiplier, 30);
  assert.equal(packet.position.length, 2);
  assert.equal(packet.position[0].referenceFrame, "INERTIAL");
  assert.deepEqual(packet.position[0].cartesian.slice(0, 4), [0, 6_878_137, 0, 0]);
  assert.equal(packet.position[0].cartesian[4], 60.25);
  assert.equal(packet.position[1].interpolationDegree, 0);
  assert.deepEqual(packet.point.color.rgba, [255, 184, 107, 166]);
  assert.equal(packet.availability, document.clock.interval);
});

test("chart ticks are round and cover the range", () => {
  assert.deepEqual(niceTicks(0, 1, 5), [0, 0.25, 0.5, 0.75, 1]);
  const ticks = niceTicks(497.3, 812.9, 5);
  assert.ok(ticks[0] <= 497.3 && ticks.at(-1) >= 812.9, `${ticks}`);
  assert.ok(ticks.every((tick) => tick % 50 === 0), `${ticks}`);
});

test("uncertainty: matched in time order across a burn; CSV columns follow", () => {
  const rows = reportRows(parseOem(OEM));
  const t0 = rows[0].t;
  const covariance = {
    times: [t0, t0 + 60.25, t0 + 60.25],
    sigmaRsw: [
      [0.001, 0.002, 0.002, 0, 0, 0],
      [0.003, 0.004, 0, 0, 0, 0],
      [0.006, 0.008, 0, 0, 0, 0],
    ],
  };
  const sigmas = withUncertainty(rows, covariance);
  assert.deepEqual(
    sigmas.map((row) => [row.sigmaRadialM, row.sigmaInTrackM, row.sigmaPositionM]),
    [
      [1, 2, 3],
      [3, 4, 5],
      [6, 8, 10],
    ],
  );
  assert.equal(withUncertainty(rows, undefined), rows);
  assert.equal(withUncertainty(rows, { times: [t0 + 1], sigmaRsw: [[1, 1, 1, 0, 0, 0]] })[0].sigmaRadialM, undefined);
  const [header, first] = reportCsv(sigmas).split("\r\n");
  assert.ok(header.endsWith(",sigma_radial_m,sigma_in_track_m,sigma_cross_track_m,sigma_position_m"), header);
  assert.ok(first.endsWith(",1,2,2,3"), first);
  assert.ok(!reportCsv(rows).includes("sigma"));
});
