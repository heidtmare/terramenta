import assert from "node:assert/strict";
import { test } from "node:test";

import * as stac from "../src/stac.js";

const T0 = Date.parse("2026-01-01T00:00:00Z") / 1000;

function catalog() {
  return stac.stacCatalog({
    id: "demo",
    title: "Demo",
    spacecraft: [
      {
        id: "sat 1",
        label: "Sat 1",
        start: T0,
        stop: T0 + 3600,
        track: { coords: [[170, 0], [-170, 10], [-150, 20]] },
        oem: "CCSDS_OEM_VERS = 2.0\n",
        czml: [{ id: "document" }],
      },
      { id: "sat2", label: "Sat 2", start: T0 + 60, stop: T0 + 7200, track: null, oem: "CCSDS_OEM_VERS = 2.0\n" },
    ],
    coverage: [
      {
        id: "europe",
        label: "Europe",
        start: T0,
        stop: T0 + 86400,
        bounds: [-15, 35, 35, 65],
        properties: { "terramenta:h3_resolution": 4 },
        geojson: { type: "FeatureCollection", features: [] },
        csv: "h3\n",
      },
    ],
  });
}

const byPath = (files) => new Map(files.map(({ path, data }) => [path, data]));

/** Resolves `href` relative to `path` within the archive. */
function resolve(path, href) {
  const parts = path.split("/").slice(0, -1);
  for (const segment of href.split("/")) {
    if (segment === "..") parts.pop();
    else if (segment !== ".") parts.push(segment);
  }
  return parts.join("/");
}

test("antimeridian crossing splits the track onto ±180", () => {
  const geometry = stac.trackGeometry([[170, 0], [-170, 10], [-150, 20]]);
  assert.equal(geometry.type, "MultiLineString");
  const [west, east] = geometry.coordinates;
  assert.deepEqual(west, [[170, 0], [180, 5]]);
  assert.deepEqual(east, [[-180, 5], [-170, 10], [-150, 20]]);
  // Westward too.
  const back = stac.trackGeometry([[-170, 0], [170, 10]]);
  assert.deepEqual(back.coordinates[0].at(-1), [-180, 5]);
  assert.deepEqual(back.coordinates[1][0], [180, 5]);
  assert.equal(stac.trackGeometry([[0, 0], [10, 0]]).type, "LineString");
  assert.equal(stac.trackGeometry([[0, 0]]), null);
});

test("bounds across the antimeridian become a MultiPolygon", () => {
  assert.equal(stac.boundsGeometry([-10, -5, 10, 5]).type, "Polygon");
  const wrapped = stac.boundsGeometry([170, -5, -170, 5]);
  assert.equal(wrapped.type, "MultiPolygon");
  assert.deepEqual(stac.bboxOf(wrapped), [-180, -5, 180, 5]);
});

test("catalog links and assets resolve inside the archive", () => {
  const files = catalog();
  const paths = byPath(files);
  assert.equal(files[0].path, "catalog.json");
  for (const [path, data] of paths) {
    if (!path.endsWith(".json")) continue;
    const doc = JSON.parse(data);
    assert.equal(doc.stac_version, stac.STAC_VERSION, path);
    for (const { href } of doc.links) assert.ok(paths.has(resolve(path, href)), `${path} → ${href}`);
    for (const { href } of Object.values(doc.assets ?? {})) assert.ok(paths.has(resolve(path, href)), `${path} → ${href}`);
  }
  const item = JSON.parse(paths.get("spacecraft/sat-1/sat-1.json"));
  assert.equal(item.type, "Feature");
  assert.equal(item.collection, "demo-spacecraft");
  assert.equal(item.geometry.type, "MultiLineString");
  assert.deepEqual(item.bbox, [-180, 0, 180, 20]);
  assert.equal(item.properties.datetime, null);
  assert.equal(item.properties.start_datetime, "2026-01-01T00:00:00.000Z");
  assert.deepEqual(Object.keys(item.assets), ["oem", "czml"]);
  // No geometry, no bbox.
  const bare = JSON.parse(paths.get("spacecraft/sat2/sat2.json"));
  assert.equal(bare.geometry, null);
  assert.equal("bbox" in bare, false);
  const spacecraft = JSON.parse(paths.get("spacecraft/collection.json"));
  assert.deepEqual(spacecraft.extent.temporal.interval, [["2026-01-01T00:00:00.000Z", "2026-01-01T02:00:00.000Z"]]);
  const study = JSON.parse(paths.get("coverage/europe/europe.json"));
  assert.deepEqual(study.bbox, [-15, 35, 35, 65]);
  assert.equal(study.properties["terramenta:h3_resolution"], 4);
});

test("empty sections are left out", () => {
  const files = stac.stacCatalog({ id: "x", title: "X" });
  assert.deepEqual(
    files.map(({ path }) => path),
    ["catalog.json"],
  );
  assert.deepEqual(JSON.parse(files[0].data).links.map(({ rel }) => rel), ["root"]);
});

test("zip entries round-trip with matching CRCs", () => {
  assert.equal(stac.crc32(new TextEncoder().encode("123456789")), 0xcbf43926);
  const files = catalog();
  const bytes = stac.zip(files, new Date(2026, 0, 2, 3, 4, 6));
  const view = new DataView(bytes.buffer);
  const end = bytes.length - 22;
  assert.equal(view.getUint32(end, true), 0x06054b50);
  assert.equal(view.getUint16(end + 10, true), files.length);
  let at = view.getUint32(end + 16, true);
  const decoder = new TextDecoder();
  for (const file of files) {
    assert.equal(view.getUint32(at, true), 0x02014b50);
    const size = view.getUint32(at + 20, true);
    const nameLength = view.getUint16(at + 28, true);
    const local = view.getUint32(at + 42, true);
    assert.equal(decoder.decode(bytes.subarray(at + 46, at + 46 + nameLength)), file.path);
    const body = bytes.subarray(local + 30 + nameLength, local + 30 + nameLength + size);
    assert.equal(decoder.decode(body), file.data);
    assert.equal(view.getUint32(at + 16, true), stac.crc32(body));
    at += 46 + nameLength;
  }
});
