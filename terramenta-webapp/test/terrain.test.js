import assert from "node:assert/strict";
import { test } from "node:test";

import * as terrain from "../src/terrain.js";
import { attachFake } from "./fake-globe.js";

test("tile range covers the station and wraps the antimeridian", () => {
  const block = terrain.tileRange(0, 0, 10, 10);
  assert.deepEqual([block.x0, block.y0, block.columns, block.rows], [511, 511, 2, 2]);
  const wrapped = terrain.tileRange(0, 179.99, 50, 8);
  assert.ok(wrapped.x0 + wrapped.columns > 256);
  const urls = terrain.tileRequests(wrapped).map(({ url }) => url);
  assert.ok(urls.some((url) => url.includes("/8/0/")), "column 256 wraps to 0");
});

test("zoom drops until the block fits", () => {
  const near = terrain.chooseTiles(46.5, 7.9, 50);
  assert.ok(near.columns * near.rows <= terrain.MAX_TILES);
  assert.ok(near.zoom < terrain.MAX_ZOOM);
  const small = terrain.chooseTiles(46.5, 7.9, 2);
  assert.equal(small.zoom, terrain.MAX_ZOOM);
  // Polar: wide in longitude, still bounded.
  const polar = terrain.chooseTiles(78.2, 15.4, 50);
  assert.ok(polar.columns * polar.rows <= terrain.MAX_TILES);
});

test("tile URLs follow the template", () => {
  const [first] = terrain.tileRequests({ zoom: 3, x0: 4, y0: 2, columns: 1, rows: 1, tileSize: 256 }, "t/{z}/{x}/{y}.png");
  assert.deepEqual(first, { url: "t/3/4/2.png", column: 0, row: 0 });
});

test("simplify keeps corners, drops collinear points", () => {
  const mask = [];
  for (let az = 0; az < 360; az++) mask.push([az, az >= 80 && az <= 100 ? 3 : 0]);
  const out = terrain.simplify(mask, 0.05);
  assert.ok(out.length < 10, `${out.length}`);
  for (const az of [79, 80, 100, 101]) assert.ok(out.some(([a]) => a === az), `${az}`);
  assert.deepEqual(out[0], [0, 0]);
  assert.deepEqual(out.at(-1), [359, 0]);
});

test("terrainHorizon passes the mosaic and returns a compact mask", async () => {
  const calls = attachFake();
  let fetched = null;
  const result = await terrain.terrainHorizon({ latDeg: 46.5, lonDeg: 7.9 }, { maxRangeKm: 20, heightAboveGroundM: 10 }, async (block) => {
    fetched = block;
    return new Uint8Array(4 * block.columns * block.rows * block.tileSize ** 2);
  });
  const [, options, rgba] = calls.find(([name]) => name === "terrainHorizon");
  assert.equal(options.tiles, fetched);
  assert.equal(options.heightAboveGroundM, 10);
  assert.equal(options.maxRangeKm, 20);
  assert.ok(rgba instanceof Uint8Array);
  assert.equal(result.groundM, 120);
  assert.ok(result.horizon.length < 12);
  assert.equal(result.zoom, fetched.zoom);
});
