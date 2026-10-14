/**
 * Propagation environment: SPK/PCK kernel, EOP, space weather.
 *
 * Loaded into the wasm module's global environment, which `globe.reset` does
 * not clear: once loaded, data stays for the session and across projects. A
 * URL source is recorded and saved with the project (`environment`); a local
 * file is not. Loading a kind again replaces it (a kernel adds to the
 * almanac).
 */

import * as globe from "./globe.js";

export const KINDS = {
  eop: {
    label: "EOP",
    url: "https://celestrak.org/SpaceData/EOP-All.csv",
    accept: ".csv,.all,.txt",
    note: "EOP-All.csv or finals2000A.all (TEME → GCRF).",
  },
  spaceWeather: {
    label: "Weather",
    url: "https://celestrak.org/SpaceData/SW-All.csv",
    accept: ".csv,.txt",
    note: "SW-All.csv (F10.7, Ap for NRLMSISE-00).",
  },
  kernel: {
    label: "Kernel",
    url: "",
    accept: ".bsp,.bpc",
    note: "SPK/PCK, e.g. de440s.bsp (else analytic Sun/Moon). NAIF lacks CORS: use a file.",
  },
};

/** Per kind: `{url}` or `{file}` last loaded, `{loading}`, or `{error}`. */
const loaded = { eop: null, spaceWeather: null, kernel: null };
const listeners = new Set();

/** `callback()` on any load, failure or start; returns an unsubscribe. */
export function subscribe(callback) {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

/** `{kind: {url | file | loading | error}}` and the module's coverage report. */
export function status() {
  let coverage = null;
  try {
    coverage = globe.propagationEnvironment();
  } catch {
    // Module not loaded (tests).
  }
  return { sources: { ...loaded }, coverage };
}

/** URL sources, as a project's `environment`; `undefined` if none. */
export function recordedEnvironment() {
  const entries = Object.entries(loaded).filter(([, source]) => source?.url && !source.error);
  return entries.length ? Object.fromEntries(entries.map(([kind, source]) => [kind, source.url])) : undefined;
}

/**
 * Loads a project's `environment` (`{kind: url}`), skipping kinds already
 * loaded from the same URL. Never throws; failures land in `status()`.
 * `resolve(url)` maps relative URLs.
 */
export async function applyEnvironment(environment = {}, resolve = (url) => url) {
  await Promise.all(
    Object.entries(environment)
      .filter(([kind, url]) => loaded[kind]?.url !== url || loaded[kind]?.error)
      .map(([kind, url]) => loadUrl(kind, url, resolve(url)).catch(() => {})),
  );
}

/** Fetches and loads `kind` from `url`; `fetchUrl` defaults to `url`. Throws on failure. */
export async function loadUrl(kind, url, fetchUrl = url) {
  return load(kind, { url }, async () => {
    const response = await fetch(fetchUrl);
    if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
    return kind === "kernel" ? new Uint8Array(await response.arrayBuffer()) : response.text();
  });
}

/** Loads `kind` from a local `File`. Throws on failure. */
export async function loadFile(kind, file) {
  return load(kind, { file: file.name }, async () =>
    kind === "kernel" ? new Uint8Array(await file.arrayBuffer()) : file.text(),
  );
}

async function load(kind, source, read) {
  if (!(kind in KINDS)) throw new Error(`unknown environment kind ${kind}`);
  set(kind, { ...source, loading: true });
  try {
    const data = await read();
    if (kind === "kernel") globe.loadKernel(data);
    else if (kind === "eop") globe.loadEop(data);
    else globe.loadSpaceWeather(data);
    set(kind, source);
  } catch (error) {
    console.warn(`Environment: ${KINDS[kind].label}`, error);
    set(kind, { ...source, error: error.message ?? String(error) });
    throw error;
  }
}

function set(kind, source) {
  loaded[kind] = source;
  for (const callback of listeners) callback();
}
