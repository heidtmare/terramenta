/**
 * Reports: spacecraft samples, derived quantities, position uncertainty, Δv
 * budget, CSV and CZML. Pure; no globe or DOM.
 *
 * Samples come from `propagate`'s OEM (central-body-centred, GCRF/ICRF, UTC, km,
 * km/s), one array per segment (segments split at impulsive burns, sharing the
 * burn epoch). Osculating elements are two-body about the segment's
 * `CENTER_NAME` (`BODIES`); altitudes are radius − equatorial radius, not geodetic.
 */

export const MU_EARTH = 398_600.4418;
export const EARTH_RADIUS_KM = 6_378.137;
/** OEM `CENTER_NAME` → `{mu, radiusKm}`: field GM (solare `CentralBody`), equatorial radius. */
export const BODIES = {
  EARTH: { mu: MU_EARTH, radiusKm: EARTH_RADIUS_KM },
  MOON: { mu: 4_902.79996708864, radiusKm: 1_737.4 },
  MARS: { mu: 42_828.3756639565, radiusKm: 3_396.19 },
};
const G0 = 9.80665;
/** `vehicle.dryMassKg` default: this, capped at the wet mass (solare `Vehicle`). */
const DEFAULT_DRY_MASS_KG = 850;
const DEG = 180 / Math.PI;

/** `key: {label, unit, digits}`; values from `quantities(r, v)`. */
export const QUANTITIES = {
  altitudeKm: { label: "Altitude", unit: "km", digits: 3 },
  radiusKm: { label: "Radius", unit: "km", digits: 3 },
  speedKmS: { label: "Speed", unit: "km/s", digits: 6 },
  smaKm: { label: "Semi-major axis", unit: "km", digits: 3 },
  eccentricity: { label: "Eccentricity", unit: "", digits: 7 },
  inclinationDeg: { label: "Inclination", unit: "°", digits: 5 },
  raanDeg: { label: "RAAN", unit: "°", digits: 5 },
  argPerigeeDeg: { label: "Argument of perigee", unit: "°", digits: 5 },
  trueAnomalyDeg: { label: "True anomaly", unit: "°", digits: 5 },
  periodMin: { label: "Period", unit: "min", digits: 4 },
  perigeeAltKm: { label: "Perigee altitude", unit: "km", digits: 3 },
  apogeeAltKm: { label: "Apogee altitude", unit: "km", digits: 3 },
};

/** 1σ position from `propagate`'s `covariance` (RSW), metres; rows of covariance runs only. */
export const UNCERTAINTY_QUANTITIES = {
  sigmaRadialM: { label: "1σ radial", unit: "m", digits: 3 },
  sigmaInTrackM: { label: "1σ in-track", unit: "m", digits: 3 },
  sigmaCrossTrackM: { label: "1σ cross-track", unit: "m", digits: 3 },
  sigmaPositionM: { label: "1σ position (RSS)", unit: "m", digits: 3 },
};

/** Plot axes: `QUANTITIES` and `UNCERTAINTY_QUANTITIES`. */
export const PLOT_QUANTITIES = { ...QUANTITIES, ...UNCERTAINTY_QUANTITIES };

// --- OEM -----------------------------------------------------------------------

/**
 * CCSDS OEM KVN → `{segments: [{name, frame, center, samples: [{t, r, v}]}]}`,
 * `t` Unix seconds (UTC), `r` km, `v` km/s. Covariance blocks are skipped.
 * Throws on a data line it cannot read.
 */
export function parseOem(text) {
  const segments = [];
  let meta = null;
  let segment = null;
  let skipping = false;
  text.split(/\r?\n/).forEach((raw, index) => {
    const line = raw.trim();
    if (!line || line.startsWith("COMMENT")) return;
    if (line === "COVARIANCE_START") skipping = true;
    else if (line === "COVARIANCE_STOP") skipping = false;
    else if (skipping) return;
    else if (line === "META_START") meta = {};
    else if (line === "META_STOP") {
      segment = { name: meta.OBJECT_NAME ?? "", frame: meta.REF_FRAME ?? "", center: meta.CENTER_NAME ?? "", samples: [] };
      segments.push(segment);
      meta = null;
    } else if (meta) {
      const [key, value] = line.split("=").map((part) => part.trim());
      meta[key] = value;
    } else if (segment) {
      const [epoch, ...numbers] = line.split(/\s+/);
      const values = numbers.slice(0, 6).map(Number);
      const t = epochSeconds(epoch);
      if (values.length < 6 || values.some((value) => !Number.isFinite(value)) || !Number.isFinite(t)) {
        throw new Error(`OEM line ${index + 1}: expected an epoch and six numbers`);
      }
      segment.samples.push({ t, r: values.slice(0, 3), v: values.slice(3, 6) });
    }
  });
  return { segments };
}

/** `YYYY-MM-DDThh:mm:ss[.f…][Z]`, UTC → Unix seconds, sub-millisecond kept; else `Date.parse`. */
export function epochSeconds(text) {
  const match = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(\.\d+)?Z?$/.exec(text);
  if (!match) return Date.parse(text) / 1000;
  return Date.parse(`${match[1]}Z`) / 1000 + Number(`0${match[2] ?? ""}`);
}

// --- Quantities ------------------------------------------------------------------

/**
 * Every `QUANTITIES` key for one state about `center` (`BODIES` key, default
 * Earth); non-finite where undefined (hyperbolic period).
 */
export function quantities(r, v, center = "EARTH") {
  const { mu: MU, radiusKm: RADIUS_KM } = BODIES[center?.toUpperCase()] ?? BODIES.EARTH;
  const radius = norm(r);
  const speed = norm(v);
  const h = cross(r, v);
  const hNorm = norm(h);
  const node = [-h[1], h[0], 0];
  const nodeNorm = norm(node);
  const rv = dot(r, v);
  const eVector = r.map((component, i) => ((speed ** 2 - MU / radius) * component - rv * v[i]) / MU);
  const e = norm(eVector);
  const energy = speed ** 2 / 2 - MU / radius;
  const a = -MU / (2 * energy);
  const inclination = Math.acos(clamp(h[2] / hNorm)) * DEG;

  const equatorial = nodeNorm < 1e-12;
  const circular = e < 1e-10;
  const raan = equatorial ? 0 : wrap(Math.atan2(node[1], node[0]) * DEG);
  let argPerigee = 0;
  if (!circular) {
    argPerigee = equatorial
      ? Math.atan2(eVector[1], eVector[0]) * DEG * Math.sign(h[2] || 1)
      : Math.acos(clamp(dot(node, eVector) / (nodeNorm * e))) * DEG * (eVector[2] < 0 ? -1 : 1);
  }
  // Circular: argument of latitude (or true longitude, equatorial) as true anomaly.
  const reference = circular ? (equatorial ? [1, 0, 0] : node.map((x) => x / nodeNorm)) : eVector.map((x) => x / e);
  let anomaly = Math.acos(clamp(dot(reference, r) / radius)) * DEG;
  if (circular ? dot(cross(reference, r), h) < 0 : rv < 0) anomaly = 360 - anomaly;

  const bound = a > 0 && e < 1;
  return {
    altitudeKm: radius - RADIUS_KM,
    radiusKm: radius,
    speedKmS: speed,
    smaKm: a,
    eccentricity: e,
    inclinationDeg: inclination,
    raanDeg: raan,
    argPerigeeDeg: wrap(argPerigee),
    trueAnomalyDeg: wrap(anomaly),
    periodMin: bound ? (2 * Math.PI * Math.sqrt(a ** 3 / MU)) / 60 : NaN,
    perigeeAltKm: a * (1 - e) - RADIUS_KM,
    apogeeAltKm: bound ? a * (1 + e) - RADIUS_KM : NaN,
  };
}

/** `[{t, segment, ...quantities}]` across every segment, in order. */
export function reportRows(oem) {
  return oem.segments.flatMap((segment, index) =>
    segment.samples.map(({ t, r, v }) => ({ t, segment: index, r, v, ...quantities(r, v, segment.center) })),
  );
}

/**
 * `rows` with `UNCERTAINTY_QUANTITIES` from `covariance` (`{times, sigmaRsw}`,
 * km), matched by time in order, so a burn epoch's pre- and post-burn rows take
 * the pre- and post-burn samples. Unmatched rows are left as they are.
 */
export function withUncertainty(rows, covariance) {
  if (!covariance) return rows;
  const samples = covariance.times.map((t, index) => ({ t, sigma: covariance.sigmaRsw[index] })).sort((a, b) => a.t - b.t);
  let k = 0;
  return rows.map((row) => {
    while (k < samples.length && samples[k].t < row.t - 1e-3) k++;
    if (k >= samples.length || samples[k].t > row.t + 1e-3) return row;
    const [r, s, w] = samples[k++].sigma.map((km) => km * 1000);
    return { ...row, sigmaRadialM: r, sigmaInTrackM: s, sigmaCrossTrackM: w, sigmaPositionM: Math.hypot(r, s, w) };
  });
}

/** `{min, max, first, last}` of `key` over `rows`, non-finite values skipped; `null` if none. */
export function summarize(rows, key) {
  const values = rows.map((row) => row[key]).filter(Number.isFinite);
  if (!values.length) return null;
  return { min: Math.min(...values), max: Math.max(...values), first: values[0], last: values.at(-1) };
}

// --- Δv budget -------------------------------------------------------------------

/**
 * Burns in time order with Δv and mass, by the rocket equation from the
 * request alone (as the propagator models them): impulsive burns spend
 * `ispS` propellant (none without it), finite burns flow at `thrustN / (ispS·g0)`
 * and cut off at dry mass.
 * → `{burns: [{kind, index, epoch, deltaVKmS, massBeforeKg, massAfterKg}], totalDeltaVKmS, propellantKg, finalMassKg}`.
 */
export function deltaVBudget(propagation) {
  const vehicle = propagation.vehicle ?? {};
  const wet = vehicle.massKg;
  const dry = vehicle.dryMassKg ?? Math.min(DEFAULT_DRY_MASS_KG, wet);
  const burns = [
    ...(propagation.maneuvers ?? []).map((burn, index) => ({ kind: "impulsive", index: index + 1, burn, at: epochSeconds(burn.epoch) })),
    ...(propagation.finiteBurns ?? []).map((burn, index) => ({ kind: "finite", index: index + 1, burn, at: epochSeconds(burn.start) })),
  ].sort((a, b) => a.at - b.at);

  let mass = wet;
  const rows = burns.map(({ kind, index, burn }) => {
    const before = mass;
    let deltaV;
    if (kind === "impulsive") {
      deltaV = norm(burn.deltaVKmS);
      if (burn.ispS) mass *= Math.exp((-deltaV * 1e3) / (burn.ispS * G0));
    } else {
      const flow = burn.thrustN / (burn.ispS * G0);
      if (mass > dry) mass = Math.max(dry, mass - flow * burn.durationS);
      deltaV = (burn.ispS * G0 * Math.log(before / mass)) / 1e3;
    }
    return {
      kind,
      index,
      epoch: kind === "impulsive" ? burn.epoch : burn.start,
      deltaVKmS: deltaV,
      massBeforeKg: before,
      massAfterKg: mass,
    };
  });
  return {
    burns: rows,
    totalDeltaVKmS: rows.reduce((sum, row) => sum + row.deltaVKmS, 0),
    propellantKg: wet - mass,
    finalMassKg: mass,
  };
}

// --- CSV -------------------------------------------------------------------------

/** RFC 4180 text from `header` and `rows` (arrays); non-finite numbers empty. */
export function toCsv(header, rows) {
  const cell = (value) => {
    if (typeof value === "number") return Number.isFinite(value) ? String(value) : "";
    const text = String(value ?? "");
    return /[",\r\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
  };
  return `${[header, ...rows].map((row) => row.map(cell).join(",")).join("\r\n")}\r\n`;
}

/** Report rows as CSV: UTC, GCRF state, then `keys` (default `QUANTITIES`, and `UNCERTAINTY_QUANTITIES` when the rows have them). */
export function reportCsv(rows, keys = reportKeys(rows)) {
  const header = [
    "utc", "segment", "x_km", "y_km", "z_km", "vx_km_s", "vy_km_s", "vz_km_s",
    ...keys.map((key) => columnName(key)),
  ];
  return toCsv(
    header,
    rows.map((row) => [
      isoUtc(row.t), row.segment, ...row.r, ...row.v,
      ...keys.map((key) => {
        const value = round(row[key], PLOT_QUANTITIES[key].digits);
        return PLOT_QUANTITIES[key].unit === "°" && value === 360 ? 0 : value;
      }),
    ]),
  );
}

/** `QUANTITIES` keys, then `UNCERTAINTY_QUANTITIES` keys if any row has them. */
export function reportKeys(rows) {
  const uncertain = rows.some((row) => row.sigmaRadialM !== undefined);
  return [...Object.keys(QUANTITIES), ...(uncertain ? Object.keys(UNCERTAINTY_QUANTITIES) : [])];
}

/** `smaKm` → `sma_km`. */
export function columnName(key) {
  return key.replace(/([A-Z])/g, "_$1").toLowerCase();
}

// --- CZML ------------------------------------------------------------------------

/**
 * CZML document (array) for Cesium: one packet per spacecraft, positions in
 * `INERTIAL` (ICRF ≈ GCRF) metres, one position interval per OEM segment so
 * burns are not interpolated across. Earth-centred segments only.
 * `entries`: `[{id, label, color: "#rrggbb[aa]", oem}]`; `clock`: `{unixSeconds, timeScale}` or omitted.
 */
export function toCzml(name, entries, clock) {
  const geocentric = (segment) => !segment.center || segment.center.toUpperCase() === "EARTH";
  entries = entries
    .map((entry) => ({ ...entry, oem: { ...entry.oem, segments: entry.oem.segments.filter(geocentric) } }))
    .filter((entry) => entry.oem.segments.some((segment) => segment.samples.length));
  const spans = entries
    .flatMap(({ oem }) => oem.segments.flatMap((segment) => segment.samples))
    .map((sample) => sample.t);
  const start = Math.min(...spans);
  const stop = Math.max(...spans);
  const document = { id: "document", name, version: "1.0" };
  if (spans.length) {
    const current = clock ? Math.min(Math.max(clock.unixSeconds, start), stop) : start;
    document.clock = {
      interval: interval(start, stop),
      currentTime: isoUtc(current),
      multiplier: clock?.timeScale ?? 60,
      range: "LOOP_STOP",
      step: "SYSTEM_CLOCK_MULTIPLIER",
    };
  }
  return [
    document,
    ...entries.map(({ id, label, color, oem }) => {
      const segments = oem.segments.filter((segment) => segment.samples.length);
      const times = segments.flatMap((segment) => segment.samples.map((sample) => sample.t));
      const rgba = rgbaOf(color);
      return {
        id,
        name: label ?? id,
        availability: interval(Math.min(...times), Math.max(...times)),
        position: segments.map(({ samples }) => {
          const epoch = samples[0].t;
          return {
            interval: interval(epoch, samples.at(-1).t),
            epoch: isoUtc(epoch),
            referenceFrame: "INERTIAL",
            interpolationAlgorithm: "LAGRANGE",
            interpolationDegree: Math.min(5, samples.length - 1),
            cartesian: samples.flatMap(({ t, r }) => [round(t - epoch, 6), ...r.map((km) => round(km * 1e3, 3))]),
          };
        }),
        point: { pixelSize: 6, color: { rgba } },
        path: { width: 1.5, leadTime: 0, material: { solidColor: { color: { rgba: [rgba[0], rgba[1], rgba[2], 166] } } } },
        label: { text: label ?? id, font: "12px sans-serif", pixelOffset: { cartesian2: [10, 0] }, fillColor: { rgba } },
      };
    }),
  ];
}

function interval(from, to) {
  return `${isoUtc(from)}/${isoUtc(to)}`;
}

function rgbaOf(color = "#ffffff") {
  const hex = color.replace("#", "");
  const channel = (index) => parseInt(hex.slice(index * 2, index * 2 + 2), 16);
  return [channel(0), channel(1), channel(2), hex.length >= 8 ? channel(3) : 255];
}

// --- Helpers ---------------------------------------------------------------------

/** Unix seconds → `YYYY-MM-DDThh:mm:ss.sssZ`. */
export function isoUtc(unixSeconds) {
  return new Date(Math.round(unixSeconds * 1000)).toISOString();
}

function round(value, digits) {
  if (!Number.isFinite(value)) return value;
  const scale = 10 ** digits;
  return Math.round(value * scale) / scale;
}

const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const norm = (a) => Math.sqrt(dot(a, a));
const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const clamp = (x) => Math.min(Math.max(x, -1), 1);
const wrap = (degrees) => ((degrees % 360) + 360) % 360;
