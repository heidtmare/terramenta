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
  calls.map(([name]) => name).filter((name) => !(name in { layers: 0, vectorLayers: 0, limits: 0, ephemerisObjects: 0 }));
