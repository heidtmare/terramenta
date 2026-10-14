/**
 * Stand-in for the wasm module (`globe.attach`): records every call, answers
 * the catalogue and object queries, accepts every `add*`.
 */

import * as globe from "../src/globe.js";

export const IMAGERY = [
  { index: 0, label: "Blue Marble" },
  { index: 1, label: "OSM" },
];
export const VECTOR = [{ index: 0, label: "Boundaries" }];

/** Installs a fresh fake; returns its call log `[[name, ...args]]`. */
export function attachFake({ objects = {} } = {}) {
  const calls = [];
  const answers = {
    layers: () => IMAGERY,
    vectorLayers: () => VECTOR,
    limits: () => ({ maxTimeScale: 1e6 }),
    ephemerisObjects: (id) => objects[id] ?? null,
    propagationEnvironment: () => ({ kernel: false, eop: null, spaceWeather: null }),
    /** Throws for a TLE whose first line is `"bad"`. `covariance` when asked for. */
    propagate: (options) => {
      if (options.initial.tle?.[0] === "bad") throw new Error("bad TLE");
      const info = { samples: 10, finalMassKg: 500, start: "2026-01-01T00:00:00.000", stop: "2026-01-01T06:00:00.000" };
      if (!options.covariance) return info;
      // σ in-track grows 1 km per sample.
      const t0 = Date.parse("2026-01-01T00:00:00Z") / 1000;
      const times = Array.from({ length: 10 }, (_, i) => t0 + i * 2400);
      return { ...info, covariance: { times, sigmaRsw: times.map((_, i) => [0.1, 0.1 + i, 0.05, 0, 0, 0]), ellipsoidKm: times.map((_, i) => [0.1 + i, 0.1, 0.05]) } };
    },
    /** Two passes per station, one umbra, one periapsis; times from 2026-01-01T00:00Z. */
    findEvents: (_, { stations = [], eclipses, apsides } = {}) => {
      const t0 = Date.parse("2026-01-01T00:00:00Z") / 1000;
      return {
        contacts: stations.flatMap(({ id }, index) =>
          [0, 1].map((pass) => {
            const aos = t0 + 600 * (index + 1) + 5_400 * pass;
            return { station: id, aos, los: aos + 420, maxElevationAt: aos + 210, maxElevationDeg: 30 + pass };
          }),
        ),
        eclipses: eclipses ? [{ kind: "penumbra", start: t0 + 1990, stop: t0 + 4210 }, { kind: "umbra", start: t0 + 2000, stop: t0 + 4200 }] : [],
        orbit: apsides ? [{ kind: "periapsis", at: t0 + 100 }] : [],
      };
    },
    /** Moves every variable by +1 and meets every goal; throws with no goals. */
    target: (propagation, { variables, goals }) => {
      if (!goals?.length) throw new Error("need at least one variable and one goal");
      const maneuvers = structuredClone(propagation.maneuvers ?? []);
      for (const variable of variables) {
        if (variable.burn && variable.parameter.startsWith("dv")) maneuvers[variable.burn - 1].deltaVKmS[variable.parameter[2] - 1] += 1;
      }
      return {
        converged: true,
        iterations: 2,
        history: [40, 3, 0.5],
        values: variables.map(() => 1),
        goals: goals.map((goal) => ({ achieved: goal.value, error: 0 })),
        maneuvers,
        finiteBurns: structuredClone(propagation.finiteBurns ?? []),
      };
    },
    /** One sample per `stepS` over an hour; sensors 10°…; LVLH Euler zero. Throws on `to: "bad"`. */
    attitude: (propagation, options) => {
      if ([options.primary, ...(options.sensors ?? [])].some((p) => p?.to === "bad")) throw new Error("unknown direction");
      const t0 = Date.parse("2026-01-01T00:00:00Z") / 1000;
      const times = Array.from({ length: Math.floor(3600 / (options.stepS ?? 60)) + 1 }, (_, i) => t0 + i * (options.stepS ?? 60));
      return {
        times,
        quaternions: times.map(() => [0, 0, 0, 1]),
        eulerDeg: times.map(() => [0, 0, 0]),
        sensorAnglesDeg: (options.sensors ?? []).map(() => times.map((_, i) => 10 + i)),
        undefined: 0,
        ...(options.aem ? { aem: "CCSDS_AEM_VERS = 1.0\n" } : {}),
      };
    },
    /** Two cells per resolution step; the first covered when any spacecraft is given. Throws with no spacecraft. */
    coverage: ({ spacecraft = [], resolution = 3, stepS = 60, boundaries }) => {
      if (!spacecraft.length) throw new Error("coverage needs a spacecraft");
      const t0 = Date.parse("2026-01-01T00:00:00Z") / 1000;
      const cell = (id, lonDeg, fraction) => ({
        id,
        latDeg: 0,
        lonDeg,
        fraction,
        accesses: fraction ? 2 : 0,
        maxGapS: fraction ? 1800 : 3600,
        meanGapS: fraction ? 1200 : null,
        firstAccessS: fraction ? 600 : null,
        ...(boundaries ? { boundary: [[lonDeg - 1, -1], [lonDeg + 1, -1], [lonDeg + 1, 1], [lonDeg - 1, 1]] } : {}),
      });
      return {
        start: t0,
        stop: t0 + 3600,
        stepS,
        samples: Math.floor(3600 / stepS) + 1,
        resolution,
        cells: [cell("8a", 10, 0.25), cell("8b", 20, 0)],
      };
    },
  };
  const module = new Proxy(
    {},
    {
      get: (_, name) =>
        (...args) => {
          calls.push([name, ...args]);
          // `add*` report acceptance; `globe.js` records sources only then.
          return name in answers ? answers[name](...args) : name.startsWith("add");
        },
    },
  );
  globe.attach(module);
  globe.reset();
  calls.length = 0;
  return calls;
}

/** Names of the scene commands in `calls`, queries left out. */
export const commands = (calls) =>
  calls
    .map(([name]) => name)
    .filter(
      (name) => !(name in { layers: 0, vectorLayers: 0, limits: 0, ephemerisObjects: 0, propagationEnvironment: 0, findEvents: 0, target: 0, attitude: 0, coverage: 0 }),
    );
