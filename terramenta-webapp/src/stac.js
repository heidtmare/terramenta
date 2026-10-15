/**
 * STAC 1.1 export: a self-contained catalog (relative links) zipped with its
 * assets.
 *
 *   catalog.json
 *   spacecraft/collection.json        one item per propagated spacecraft
 *   spacecraft/<id>/<id>.json         geometry: ground track; assets: OEM, CZML
 *   coverage/collection.json          one item per coverage study with a result
 *   coverage/<id>/<id>.json           geometry: study bounds; assets: cells GeoJSON, CSV
 *
 * Pure: inputs are plain data, output `[{path, data}]` or ZIP bytes.
 */

export const STAC_VERSION = "1.1.0";
const JSON_TYPE = "application/json";
const GEOJSON_TYPE = "application/geo+json";
const WORLD = [-180, -90, 180, 90];

const iso = (unixSeconds) => new Date(Math.round(unixSeconds * 1000)).toISOString();

/** `id` safe as a file name and STAC id. */
export function safeId(text) {
  return (
    String(text)
      .replace(/[^A-Za-z0-9_-]+/g, "-")
      .replace(/^-+|-+$/g, "") || "item"
  );
}

// --- Geometry --------------------------------------------------------------------

/**
 * Ground track `[[lonDeg, latDeg, …]]` (lon in [−180, 180)) as GeoJSON split at
 * the antimeridian (RFC 7946 §3.1.9): crossings interpolated onto ±180.
 */
export function trackGeometry(coords) {
  const lines = [];
  let line = [];
  for (let i = 0; i < coords.length; i++) {
    const [lon, lat] = coords[i];
    if (i) {
      const [lon0, lat0] = coords[i - 1];
      const d = lon - lon0;
      if (Math.abs(d) > 180) {
        // Unwrapped next longitude, crossing at ±180.
        const east = d < 0;
        const edge = east ? 180 : -180;
        const next = lon + (east ? 360 : -360);
        const t = (edge - lon0) / (next - lon0);
        const latX = lat0 + (lat - lat0) * t;
        line.push([edge, latX]);
        lines.push(line);
        line = [[-edge, latX]];
      }
    }
    line.push([lon, lat]);
  }
  if (line.length) lines.push(line);
  const kept = lines.filter((part) => part.length >= 2);
  if (!kept.length) return null;
  return kept.length === 1 ? { type: "LineString", coordinates: kept[0] } : { type: "MultiLineString", coordinates: kept };
}

/** `[west, south, east, north]` as a Polygon, or a MultiPolygon when it crosses the antimeridian. */
export function boundsGeometry([west, south, east, north] = WORLD) {
  const box = (w, e) => [
    [
      [w, south],
      [e, south],
      [e, north],
      [w, north],
      [w, south],
    ],
  ];
  return west <= east ? { type: "Polygon", coordinates: box(west, east) } : { type: "MultiPolygon", coordinates: [box(west, 180), box(-180, east)] };
}

/** 2D bbox of any GeoJSON geometry. */
export function bboxOf(geometry) {
  let [w, s, e, n] = [Infinity, Infinity, -Infinity, -Infinity];
  const visit = (value) => {
    if (typeof value[0] === "number") {
      w = Math.min(w, value[0]);
      e = Math.max(e, value[0]);
      s = Math.min(s, value[1]);
      n = Math.max(n, value[1]);
    } else value.forEach(visit);
  };
  visit(geometry.coordinates);
  return [w, s, e, n];
}

function union(boxes) {
  if (!boxes.length) return WORLD;
  return [Math.min(...boxes.map((b) => b[0])), Math.min(...boxes.map((b) => b[1])), Math.max(...boxes.map((b) => b[2])), Math.max(...boxes.map((b) => b[3]))];
}

// --- Catalog ---------------------------------------------------------------------

const link = (rel, href, extra = {}) => ({ rel, href, type: JSON_TYPE, ...extra });

/**
 * Catalog files for `{id, title, description?, spacecraft, coverage}`:
 * `spacecraft: [{id, label, start, stop, track: {coords}, oem, czml?}]`
 * (Unix seconds, OEM KVN text, CZML packets); `coverage: [{id, label, start,
 * stop, bounds?, properties, geojson, csv}]`. → `[{path, data}]`, JSON
 * stringified.
 */
export function stacCatalog({ id, title, description, spacecraft = [], coverage = [] }) {
  const files = [];
  const json = (path, value) => files.push({ path, data: JSON.stringify(value, null, 1) });
  const children = [];

  const collection = (key, label, summary, entries, itemOf) => {
    if (!entries.length) return;
    const items = entries.map((entry) => ({ entry, item: itemOf(entry, safeId(entry.id)) }));
    const start = Math.min(...entries.map((entry) => entry.start));
    const stop = Math.max(...entries.map((entry) => entry.stop));
    json(`${key}/collection.json`, {
      type: "Collection",
      stac_version: STAC_VERSION,
      id: `${safeId(id)}-${key}`,
      title: label,
      description: summary,
      license: "other",
      extent: {
        spatial: { bbox: [union(items.map(({ item }) => item.bbox).filter(Boolean))] },
        temporal: { interval: [[iso(start), iso(stop)]] },
      },
      links: [link("root", "../catalog.json"), link("parent", "../catalog.json"), ...items.map(({ item }) => link("item", `./${item.id}/${item.id}.json`, { type: GEOJSON_TYPE }))],
    });
    for (const { item } of items) {
      item.collection = `${safeId(id)}-${key}`;
      item.links = [link("root", "../../catalog.json"), link("parent", "../collection.json"), link("collection", "../collection.json")];
      json(`${key}/${item.id}/${item.id}.json`, item);
    }
    children.push(link("child", `./${key}/collection.json`, { title: label }));
  };

  const item = (itemId, geometry, entry, properties, assets) => ({
    type: "Feature",
    stac_version: STAC_VERSION,
    id: itemId,
    geometry,
    ...(geometry ? { bbox: bboxOf(geometry) } : {}),
    properties: {
      title: entry.label ?? entry.id,
      datetime: null,
      start_datetime: iso(entry.start),
      end_datetime: iso(entry.stop),
      ...properties,
    },
    links: [],
    assets,
  });

  collection("spacecraft", "Spacecraft", "Propagated trajectories: ground track geometry, CCSDS OEM and CZML.", spacecraft, (entry, itemId) => {
    const assets = {
      oem: { href: `./${itemId}.oem`, type: "text/plain", title: "CCSDS OEM (KVN), GCRF/UTC", roles: ["data"] },
    };
    files.push({ path: `spacecraft/${itemId}/${itemId}.oem`, data: entry.oem });
    if (entry.czml) {
      assets.czml = { href: `./${itemId}.czml`, type: JSON_TYPE, title: "CZML", roles: ["visual"] };
      files.push({ path: `spacecraft/${itemId}/${itemId}.czml`, data: JSON.stringify(entry.czml) });
    }
    return item(itemId, trackGeometry(entry.track?.coords ?? []), entry, { platform: entry.label ?? entry.id }, assets);
  });

  collection("coverage", "Coverage", "H3 access coverage studies: per-cell fraction, accesses and gaps.", coverage, (entry, itemId) => {
    files.push({ path: `coverage/${itemId}/${itemId}.geojson`, data: JSON.stringify(entry.geojson) });
    files.push({ path: `coverage/${itemId}/${itemId}.csv`, data: entry.csv });
    return item(itemId, boundsGeometry(entry.bounds ?? WORLD), entry, entry.properties ?? {}, {
      cells: { href: `./${itemId}.geojson`, type: GEOJSON_TYPE, title: "H3 cells, all statistics", roles: ["data"] },
      table: { href: `./${itemId}.csv`, type: "text/csv", title: "H3 cells, CSV", roles: ["data"] },
    });
  });

  files.unshift({
    path: "catalog.json",
    data: JSON.stringify(
      {
        type: "Catalog",
        stac_version: STAC_VERSION,
        id: safeId(id),
        title,
        description: description || `${title}: Terramenta export.`,
        links: [link("root", "./catalog.json"), ...children],
      },
      null,
      1,
    ),
  });
  return files;
}

// --- ZIP (stored, no compression) ------------------------------------------------

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

export function crc32(bytes) {
  let c = 0xffffffff;
  for (const byte of bytes) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** Store-only ZIP of `[{path, data: string | Uint8Array}]`; `date` sets entry times. */
export function zip(files, date = new Date()) {
  const encoder = new TextEncoder();
  const time = (date.getHours() << 11) | (date.getMinutes() << 5) | (date.getSeconds() >> 1);
  const day = ((date.getFullYear() - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate();
  const locals = [];
  const centrals = [];
  let offset = 0;
  for (const { path, data } of files) {
    const name = encoder.encode(path);
    const body = typeof data === "string" ? encoder.encode(data) : data;
    const crc = crc32(body);
    const header = (signature, central) => {
      const view = new DataView(new ArrayBuffer(central ? 46 : 30));
      let at = 0;
      const u16 = (v) => (view.setUint16(at, v, true), (at += 2));
      const u32 = (v) => (view.setUint32(at, v, true), (at += 4));
      u32(signature);
      if (central) u16(20); // made by
      u16(20); // version needed
      u16(0x0800); // UTF-8 names
      u16(0); // stored
      u16(time);
      u16(day);
      u32(crc);
      u32(body.length);
      u32(body.length);
      u16(name.length);
      u16(0); // extra
      if (central) {
        u16(0); // comment
        u16(0); // disk
        u16(0); // internal attributes
        u32(0); // external attributes
        u32(offset);
      }
      return new Uint8Array(view.buffer);
    };
    const local = header(0x04034b50, false);
    locals.push(local, name, body);
    centrals.push(header(0x02014b50, true), name);
    offset += local.length + name.length + body.length;
  }
  const centralSize = centrals.reduce((sum, part) => sum + part.length, 0);
  const end = new DataView(new ArrayBuffer(22));
  end.setUint32(0, 0x06054b50, true);
  end.setUint16(8, files.length, true);
  end.setUint16(10, files.length, true);
  end.setUint32(12, centralSize, true);
  end.setUint32(16, offset, true);
  const parts = [...locals, ...centrals, new Uint8Array(end.buffer)];
  const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}
