/**
 * Project `groundStations` and `events`, held between open and save like
 * `analysis.js`, and the event searches they drive.
 *
 * - Station: `{id, label?, latDeg, lonDeg, altKm?, minElevationDeg?, horizon?,
 *   color?}`; `horizon` a terrain mask `[[azimuthDeg, elevationDeg], …]`,
 *   azimuth ascending in `[0, 360)`.
 * - Events: `{eclipses?, apsides?, nodes?, stepS?}`; contacts with every
 *   station are always searched. Defaults in `EVENT_DEFAULTS`.
 *
 * `spacecraftEvents(id)` runs `globe.findEvents` on the spacecraft's recorded
 * propagation, cached per entry and settings revision. Stations are drawn as
 * overlay `STATION_LAYER`: not listed in Layers, not saved as an overlay.
 */

import * as globe from "./globe.js";

export const STATION_LAYER = "ground-stations";
export const STATION_COLOR = "#a6ff6b";
export const EVENT_DEFAULTS = { eclipses: true, apsides: false, nodes: false, stepS: 30 };

let stations = [];
let options = {};
let revision = 0;
const cache = new WeakMap();
const listeners = new Set();

/** Loads `project.groundStations` and `project.events`, draws the stations; does not notify. */
export function openEvents(project) {
  stations = structuredClone(project.groundStations ?? []);
  options = structuredClone(project.events ?? {});
  revision++;
  drawStations();
}

/** `callback()` on any edit (not `openEvents`); returns an unsubscribe. */
export function subscribe(callback) {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

function changed() {
  revision++;
  drawStations();
  for (const callback of listeners) callback();
}

export const savedStations = () => stations;
export const eventOptions = () => ({ ...EVENT_DEFAULTS, ...options });
/** Bumped on every edit and open; a cached search older than it is stale. */
export const eventsRevision = () => revision;

/** Adds `station`, replacing one with its id. */
export function addStation(station) {
  const index = stations.findIndex((other) => other.id === station.id);
  const copy = structuredClone(station);
  stations = index < 0 ? [...stations, copy] : stations.map((other, i) => (i === index ? copy : other));
  changed();
}

export function removeStation(id) {
  stations = stations.filter((station) => station.id !== id);
  changed();
}

/** Merges `patch` into the event options; a default value is dropped. */
export function setEventOptions(patch) {
  options = { ...options, ...patch };
  for (const [key, value] of Object.entries(options)) {
    if (value === EVENT_DEFAULTS[key] || value === undefined) delete options[key];
  }
  changed();
}

/** `base` (slugged), or `base-N`, not yet a station id. */
export function uniqueStationId(base) {
  const root = slug(base) || "station";
  const taken = new Set(stations.map((station) => station.id));
  if (!taken.has(root)) return root;
  let n = 2;
  while (taken.has(`${root}-${n}`)) n++;
  return `${root}-${n}`;
}

export const stationLabel = (id) => stations.find((station) => station.id === id)?.label ?? id;

/** `{groundStations?, events?}` for saving. */
export function eventsSection() {
  const section = {};
  if (stations.length) section.groundStations = structuredClone(stations);
  if (Object.keys(options).length) section.events = structuredClone(options);
  return section;
}

/** Reference errors in `project.groundStations`; structure already valid. */
export function eventsErrors(project) {
  const errors = [];
  const seen = new Set();
  project.groundStations?.forEach((station, index) => {
    if (seen.has(station.id)) errors.push(`groundStations[${index}].id: duplicate "${station.id}"`);
    seen.add(station.id);
    const problem = horizonProblem(station.horizon ?? []);
    if (problem) errors.push(`groundStations[${index}].horizon: ${problem}`);
  });
  return errors;
}

/** Why `horizon` is not a valid mask, or `null`. */
export function horizonProblem(horizon) {
  if (horizon.some(([az, el]) => !(az >= 0 && az < 360) || !(Math.abs(el) <= 90))) {
    return "azimuth in [0, 360), elevation within ±90°";
  }
  if (horizon.some(([az], index) => index && !(az > horizon[index - 1][0]))) return "azimuths must ascend";
  return null;
}

/** `"0:5, 90:12.5"` → `[[0, 5], [90, 12.5]]`; throws on a malformed pair. */
export function parseHorizon(text) {
  return text
    .split(/[,;\n]+/)
    .map((pair) => pair.trim())
    .filter(Boolean)
    .map((pair) => {
      const values = pair.split(/[:\s/]+/).map(Number);
      if (values.length !== 2 || values.some((value) => !Number.isFinite(value))) {
        throw new Error(`Horizon: "${pair}" is not azimuth:elevation`);
      }
      return values;
    });
}

export const formatHorizon = (horizon) => (horizon ?? []).map(([az, el]) => `${az}:${el}`).join(", ");

// --- Searching -----------------------------------------------------------------

/** Globe station options (`findEvents`, `determineOrbit`) for `list`. */
export function stationOptions(list = stations) {
  return list.map(({ id, latDeg, lonDeg, altKm, minElevationDeg, horizon }) => ({
    id,
    latDeg,
    lonDeg,
    altKm: altKm ?? 0,
    minElevationDeg: minElevationDeg ?? 0,
    horizon: horizon ?? [],
  }));
}

/**
 * `{contacts, eclipses, orbit}` (`globe.findEvents`, Unix seconds) or
 * `{error}` for spacecraft `id`; `null` if absent or not propagated.
 */
export function spacecraftEvents(id, entry = globe.recordedSources().spacecraft.get(id)) {
  if (!entry?.info) return null;
  const cached = cache.get(entry);
  if (cached?.revision === revision) return cached.data;
  let data;
  try {
    const { eclipses, apsides, nodes, stepS } = eventOptions();
    data = globe.findEvents(
      { ...entry.propagation, kvn: false },
      {
        stations: stationOptions(),
        eclipses,
        apsides,
        nodes,
        stepS,
      },
    );
  } catch (error) {
    data = { error: error.message ?? String(error) };
  }
  cache.set(entry, { revision, data });
  return data;
}

const ORBIT_LABELS = {
  periapsis: "periapsis",
  apoapsis: "apoapsis",
  ascendingNode: "ascending node",
  descendingNode: "descending node",
};

/**
 * Timeline scene events for spacecraft `id` named `name` (`entry`: its
 * recorded source): `[{id, label,
 * unixSeconds}]`. Ids: `<sc>.aos.<station>.<n>`, `<sc>.los.<station>.<n>`,
 * `<sc>.umbra<n>`, `<sc>.umbra<n>.end` (penumbra likewise),
 * `<sc>.<kind><n>` for apsides and nodes; `n` counts from 1 per kind.
 */
export function eventTimeline(id, name, entry) {
  const data = spacecraftEvents(id, entry);
  if (!data || data.error) return [];
  const out = [];
  const counts = new Map();
  const next = (key) => {
    const n = (counts.get(key) ?? 0) + 1;
    counts.set(key, n);
    return n;
  };
  for (const contact of data.contacts) {
    const n = next(`contact.${contact.station}`);
    const station = stationLabel(contact.station);
    const peak = `max ${contact.maxElevationDeg.toFixed(0)}°`;
    out.push({ id: `${id}.aos.${contact.station}.${n}`, label: `${name}: AOS ${station} · ${peak}`, unixSeconds: contact.aos });
    out.push({ id: `${id}.los.${contact.station}.${n}`, label: `${name}: LOS ${station}`, unixSeconds: contact.los });
  }
  for (const eclipse of data.eclipses) {
    const n = next(eclipse.kind);
    out.push({ id: `${id}.${eclipse.kind}${n}`, label: `${name}: ${eclipse.kind} entry`, unixSeconds: eclipse.start });
    out.push({ id: `${id}.${eclipse.kind}${n}.end`, label: `${name}: ${eclipse.kind} exit`, unixSeconds: eclipse.stop });
  }
  for (const event of data.orbit) {
    out.push({ id: `${id}.${event.kind}${next(event.kind)}`, label: `${name}: ${ORBIT_LABELS[event.kind]}`, unixSeconds: event.at });
  }
  return out;
}

// --- Drawing -------------------------------------------------------------------

function drawStations() {
  globe.quietly(() => {
    if (!stations.length) {
      globe.removeOverlay(STATION_LAYER);
      return;
    }
    const features = stations.map((station) => ({
      type: "Feature",
      geometry: { type: "Point", coordinates: [station.lonDeg, station.latDeg] },
      properties: {
        title: station.label ?? station.id,
        id: station.id,
        "min elevation": `${station.minElevationDeg ?? 0}°`,
        ...(station.horizon?.length ? { horizon: formatHorizon(station.horizon) } : {}),
        "marker-color": station.color ?? STATION_COLOR,
      },
    }));
    globe.addOverlay(STATION_LAYER, {
      label: "Ground stations",
      text: JSON.stringify({ type: "FeatureCollection", features }),
      simpleStyle: true,
      pointColor: STATION_COLOR,
      pointSizePx: 9,
    });
  });
}

function slug(text) {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}
