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
    /** Throws for a TLE whose first line is `"bad"`. */
    propagate: (options) => {
      if (options.initial.tle?.[0] === "bad") throw new Error("bad TLE");
      return { samples: 10, finalMassKg: 500, start: "2026-01-01T00:00:00.000", stop: "2026-01-01T06:00:00.000" };
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
      (name) => !(name in { layers: 0, vectorLayers: 0, limits: 0, ephemerisObjects: 0, propagationEnvironment: 0, findEvents: 0 }),
    );
