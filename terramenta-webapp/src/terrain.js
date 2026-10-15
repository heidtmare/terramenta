/**
 * Terrain horizon masks from Terrarium DEM tiles (AWS Terrain Tiles, Web
 * Mercator XYZ, `h = 256 R + G + B/256 − 32768` m, EGM96 heights).
 *
 * Tiles covering `maxRangeKm` around a station are fetched, mosaicked on a
 * canvas and passed as RGBA to `globe.terrainHorizon`, which ray-marches the
 * DEM per azimuth. The result is a plain `[azimuthDeg, elevationDeg]` mask,
 * saved on the station like a hand-entered one.
 */

import * as globe from "./globe.js";

export const TERRAIN_URL = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png";
export const TILE_SIZE = 256;
export const MAX_ZOOM = 12;
export const MAX_TILES = 64;
export const DEFAULTS = { maxRangeKm: 50, heightAboveGroundM: 2, azimuthStepDeg: 1, refraction: 0 };

const EARTH_A_KM = 6378.137;
const MERCATOR_LAT = 85.05112878;

const tileX = (lonDeg, n) => Math.floor(((lonDeg + 180) / 360) * n);

function tileY(latDeg, n) {
  const lat = (Math.max(-MERCATOR_LAT, Math.min(MERCATOR_LAT, latDeg)) * Math.PI) / 180;
  const y = Math.floor((0.5 - Math.log(Math.tan(Math.PI / 4 + lat / 2)) / (2 * Math.PI)) * n);
  return Math.max(0, Math.min(n - 1, y));
}

/**
 * XYZ block `{zoom, x0, y0, columns, rows, tileSize}` covering `rangeKm`
 * around a point at `zoom`; columns may run past `2^zoom` (wrap).
 */
export function tileRange(latDeg, lonDeg, rangeKm, zoom) {
  const n = 2 ** zoom;
  const dLat = (rangeKm / (EARTH_A_KM * (1 - 1 / 298.257223563)) * 180) / Math.PI;
  const dLon = Math.min(180, ((rangeKm / (EARTH_A_KM * Math.max(1e-6, Math.cos((latDeg * Math.PI) / 180)))) * 180) / Math.PI);
  const west = tileX(lonDeg - dLon, n);
  const east = tileX(lonDeg + dLon, n);
  const north = tileY(latDeg + dLat, n);
  const south = tileY(latDeg - dLat, n);
  return {
    zoom,
    x0: ((west % n) + n) % n,
    y0: north,
    columns: Math.min(n, east - west + 1),
    rows: south - north + 1,
    tileSize: TILE_SIZE,
  };
}

/** Finest block up to `MAX_ZOOM` within `MAX_TILES` tiles. */
export function chooseTiles(latDeg, lonDeg, rangeKm) {
  for (let zoom = MAX_ZOOM; zoom > 0; zoom--) {
    const block = tileRange(latDeg, lonDeg, rangeKm, zoom);
    if (block.columns * block.rows <= MAX_TILES) return block;
  }
  return tileRange(latDeg, lonDeg, rangeKm, 0);
}

/** `[{url, column, row}]` for `block`, wrapping columns. */
export function tileRequests(block, template = TERRAIN_URL) {
  const n = 2 ** block.zoom;
  const out = [];
  for (let row = 0; row < block.rows; row++) {
    for (let column = 0; column < block.columns; column++) {
      const x = (block.x0 + column) % n;
      const y = block.y0 + row;
      out.push({ url: template.replace("{z}", block.zoom).replace("{x}", x).replace("{y}", y), column, row });
    }
  }
  return out;
}

/**
 * RGBA mosaic of `block` (Uint8Array). Missing tiles stay transparent (voids).
 * Browser only: needs `createImageBitmap` and `OffscreenCanvas`.
 */
export async function fetchMosaic(block, { template = TERRAIN_URL, signal } = {}) {
  const size = block.tileSize;
  const canvas = new OffscreenCanvas(block.columns * size, block.rows * size);
  const context = canvas.getContext("2d", { willReadFrequently: true });
  let fetched = 0;
  await Promise.all(
    tileRequests(block, template).map(async ({ url, column, row }) => {
      const response = await fetch(url, { signal }).catch(() => null);
      if (!response?.ok) return;
      // No colour management: the channels are the data.
      const bitmap = await createImageBitmap(await response.blob(), { colorSpaceConversion: "none", premultiplyAlpha: "none" });
      context.drawImage(bitmap, column * size, row * size, size, size);
      bitmap.close();
      fetched++;
    }),
  );
  if (!fetched) throw new Error("no terrain tiles could be fetched");
  return new Uint8Array(context.getImageData(0, 0, canvas.width, canvas.height).data.buffer);
}

/**
 * `{groundM, horizon, zoom}` for `station` (`latDeg`, `lonDeg`); `options` as
 * `DEFAULTS`. `fetchTiles(block)` → RGBA is injectable for tests.
 */
export async function terrainHorizon(station, options = {}, fetchTiles = fetchMosaic) {
  const settings = { ...DEFAULTS, ...options };
  const block = chooseTiles(station.latDeg, station.lonDeg, settings.maxRangeKm);
  const rgba = await fetchTiles(block);
  const { groundM, horizon } = globe.terrainHorizon(
    {
      latDeg: station.latDeg,
      lonDeg: station.lonDeg,
      tiles: block,
      maxRangeKm: settings.maxRangeKm,
      heightAboveGroundM: settings.heightAboveGroundM,
      azimuthStepDeg: settings.azimuthStepDeg,
      refraction: settings.refraction,
    },
    rgba,
  );
  return { groundM, horizon: simplify(horizon, 0.05), zoom: block.zoom };
}

/**
 * Drops interior points of `horizon` that linear interpolation between the
 * kept neighbours reproduces within `toleranceDeg` (wrapping mask).
 */
export function simplify(horizon, toleranceDeg) {
  if (horizon.length < 4) return horizon;
  const keep = [horizon[0]];
  let anchor = 0;
  for (let i = 1; i < horizon.length - 1; i++) {
    const [a0, e0] = horizon[anchor];
    const [a1, e1] = horizon[i + 1];
    const fits = horizon.slice(anchor + 1, i + 1).every(([a, e]) => Math.abs(e0 + ((e1 - e0) * (a - a0)) / (a1 - a0) - e) <= toleranceDeg);
    if (!fits) {
      keep.push(horizon[i]);
      anchor = i;
    }
  }
  keep.push(horizon[horizon.length - 1]);
  return keep;
}
