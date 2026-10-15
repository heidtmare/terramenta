/**
 * Ground stations (Spacecraft tab) and event search results (Analysis tab).
 * Both edit and read `events.js`; results are `events.spacecraftEvents`,
 * rebuilt when the spacecraft set or the event settings change. Clicking a
 * contact or eclipse row sets the clock to its start.
 */

import { download, el, row, section } from "./dom.js";
import * as events from "./events.js";
import { clock, duration } from "./format.js";
import * as globe from "./globe.js";
import { isoUtc, toCsv } from "./reports.js";
import * as terrain from "./terrain.js";
import { button, toggle } from "./widgets.js";

// --- Ground stations -----------------------------------------------------------

export function mountStations(signal) {
  const list = el("div", { class: "layers" });
  const editor = mountStationEditor();
  const render = () => {
    const stations = events.savedStations();
    list.replaceChildren(...stations.map((station) => stationRow(station, editor)));
    list.classList.toggle("empty", !stations.length);
  };
  render();
  const unsubscribe = events.subscribe(render);
  signal?.addEventListener("abort", unsubscribe);
  return section("Ground stations", list, editor.node);
}

function stationRow(station, editor) {
  const mask = station.minElevationDeg ?? 0;
  const alt = station.altKm ? ` · ${station.altKm} km` : "";
  return el(
    "div",
    { class: "layer" },
    el(
      "div",
      { class: "layer-head" },
      el("i", { class: "station-dot", style: `background: ${station.color ?? events.STATION_COLOR}` }),
      el("span", { class: "mission-title", title: station.id }, station.label ?? station.id),
      button("Edit", () => editor.edit(station)),
      button("Remove", () => events.removeStation(station.id)),
    ),
    el("p", { class: "detail" }, `${latLon(station)}${alt} · mask ${mask}°${station.horizon?.length ? ` · horizon ${station.horizon.length} pts` : ""}`),
  );
}

function latLon({ latDeg, lonDeg }) {
  const lon = ((((lonDeg + 180) % 360) + 360) % 360) - 180;
  return `${Math.abs(latDeg).toFixed(3)}° ${latDeg >= 0 ? "N" : "S"}, ${Math.abs(lon).toFixed(3)}° ${lon >= 0 ? "E" : "W"}`;
}

function mountStationEditor() {
  const number = (props = {}) => el("input", { class: "coordinate", type: "number", step: "any", ...props });
  const label = el("input", { class: "coordinate", type: "text", placeholder: "Name" });
  const lat = number({ min: "-90", max: "90", placeholder: "−90 … 90" });
  const lon = number({ placeholder: "−180 … 180" });
  const alt = number({ value: "0" });
  const mask = number({ value: "5", min: "-90", max: "90" });
  const horizon = el("input", { class: "coordinate", type: "text", spellcheck: false, placeholder: "az:el, … (optional)" });
  const color = el("input", { class: "swatch", type: "color", value: events.STATION_COLOR });
  const range = number({ value: String(terrain.DEFAULTS.maxRangeKm), min: "1", max: "500", title: "Ray length, km" });
  const eye = number({ value: String(terrain.DEFAULTS.heightAboveGroundM), min: "0", title: "Antenna above ground, m" });
  const terrainStatus = el("p", { class: "detail", hidden: true });
  const fromTerrain = button("From terrain", computeHorizon, { title: "Horizon mask from Terrarium DEM tiles (AWS Terrain Tiles)" });
  const error = el("p", { class: "detail error", hidden: true });
  const title = el("summary", {}, "Add station");
  const submit = button("Add", save);
  const cancel = button("Cancel", reset, { hidden: true });
  let editing = null;

  function edit(station) {
    editing = station.id;
    label.value = station.label ?? station.id;
    lat.value = station.latDeg;
    lon.value = station.lonDeg;
    alt.value = station.altKm ?? 0;
    mask.value = station.minElevationDeg ?? 0;
    horizon.value = events.formatHorizon(station.horizon);
    color.value = (station.color ?? events.STATION_COLOR).slice(0, 7);
    title.textContent = `Edit ${station.label ?? station.id}`;
    submit.textContent = "Save";
    cancel.hidden = false;
    node.open = true;
  }

  function reset() {
    editing = null;
    label.value = lat.value = lon.value = "";
    alt.value = "0";
    mask.value = "5";
    horizon.value = "";
    terrainStatus.hidden = true;
    color.value = events.STATION_COLOR;
    title.textContent = "Add station";
    submit.textContent = "Add";
    cancel.hidden = true;
    error.hidden = true;
  }

  function save() {
    const name = label.value.trim();
    const values = { latDeg: lat.valueAsNumber, lonDeg: lon.valueAsNumber, altKm: alt.valueAsNumber || 0, minElevationDeg: mask.valueAsNumber || 0 };
    let terrain = [];
    try {
      terrain = events.parseHorizon(horizon.value);
    } catch (caught) {
      error.textContent = caught.message;
      error.hidden = false;
      return;
    }
    const horizonProblem = events.horizonProblem(terrain);
    const problem = !name
      ? "Give the station a name."
      : !(Math.abs(values.latDeg) <= 90)
        ? "Latitude must be within ±90°."
        : !Number.isFinite(values.lonDeg) || values.lonDeg < -180 || values.lonDeg > 360
          ? "Longitude must be within −180° … 360°."
          : !(Math.abs(values.minElevationDeg) <= 90)
            ? "Mask must be within ±90°."
            : horizonProblem
              ? `Horizon: ${horizonProblem}.`
              : null;
    if (problem) {
      error.textContent = problem;
      error.hidden = false;
      return;
    }
    const station = { id: editing ?? events.uniqueStationId(name), label: name, ...values };
    if (!values.altKm) delete station.altKm;
    if (terrain.length) station.horizon = terrain;
    if (color.value !== events.STATION_COLOR) station.color = color.value;
    events.addStation(station);
    reset();
  }

  async function computeHorizon() {
    const station = { latDeg: lat.valueAsNumber, lonDeg: lon.valueAsNumber };
    if (!(Math.abs(station.latDeg) <= 90) || !Number.isFinite(station.lonDeg)) {
      terrainStatus.textContent = "Set latitude and longitude first.";
      terrainStatus.hidden = false;
      return;
    }
    fromTerrain.disabled = true;
    terrainStatus.textContent = "Fetching terrain…";
    terrainStatus.hidden = false;
    try {
      const result = await terrain.terrainHorizon(station, {
        maxRangeKm: range.valueAsNumber || terrain.DEFAULTS.maxRangeKm,
        heightAboveGroundM: Number.isFinite(eye.valueAsNumber) ? eye.valueAsNumber : terrain.DEFAULTS.heightAboveGroundM,
      });
      horizon.value = events.formatHorizon(result.horizon);
      terrainStatus.textContent = `Ground ${Math.round(result.groundM)} m (EGM96) · z${result.zoom} · ${result.horizon.length} pts`;
    } catch (caught) {
      terrainStatus.textContent = `Terrain: ${caught.message}`;
    } finally {
      fromTerrain.disabled = false;
    }
  }

  const node = el(
    "details",
    { class: "editor" },
    title,
    row("Name", label),
    row("Latitude °", lat),
    row("Longitude °", lon),
    row("Altitude km", alt),
    row("Mask °", mask),
    row("Horizon", horizon),
    row("Range km", range),
    row("Eye m", eye),
    el("div", { class: "buttons" }, fromTerrain),
    terrainStatus,
    row("Colour", color),
    error,
    el("div", { class: "buttons" }, submit, cancel),
  );
  return { node, edit };
}

// --- Event results -------------------------------------------------------------

export function mountEvents(bind, signal) {
  const settings = mountSettings();
  const list = el("div", { class: "layers" });
  let signature = null;
  let pending = true;
  const unsubscribe = events.subscribe(() => {
    pending = true;
    settings.render();
  });
  signal?.addEventListener("abort", unsubscribe);

  bind(list, (state) => {
    const looks = state.ephemerides.layers.map((layer) => `${layer.id}|${layer.label}`).join(",");
    const ids = [...globe.recordedSources().spacecraft.entries()];
    const next = `${looks}#${ids.map(([id, entry]) => `${id}:${entry.info?.start}:${entry.info?.stop}:${entry.info?.samples}`).join(",")}`;
    if (!pending && next === signature) return;
    pending = false;
    signature = next;
    list.replaceChildren(...ids.map(([id]) => eventReport(id, state)));
    node.hidden = !ids.length;
  });

  const node = section(
    "Events",
    settings.node,
    list,
    el("p", { class: "detail" }, "Contacts above each station's mask and terrain horizon; Earth shadow conical; nodes on the GCRF equator."),
  );
  return node;
}

function mountSettings() {
  const eclipses = toggle("Eclipses", (value) => events.setEventOptions({ eclipses: value }));
  const apsides = toggle("Apsides", (value) => events.setEventOptions({ apsides: value }));
  const nodes = toggle("Nodes", (value) => events.setEventOptions({ nodes: value }));
  const step = el("input", {
    class: "coordinate",
    type: "number",
    min: "1",
    step: "any",
    onchange: () => step.valueAsNumber > 0 && events.setEventOptions({ stepS: step.valueAsNumber }),
  });
  const render = () => {
    const options = events.eventOptions();
    eclipses.set(options.eclipses);
    apsides.set(options.apsides);
    nodes.set(options.nodes);
    step.value = options.stepS;
  };
  render();
  const node = el(
    "div",
    {},
    el("div", { class: "timeline-capture" }, eclipses.node, apsides.node, nodes.node),
    row("Search step s", step),
  );
  return { node, render };
}

function eventReport(id, state) {
  const layer = state.ephemerides.layers.find((candidate) => candidate.id === id);
  const label = layer?.label || id;
  const data = events.spacecraftEvents(id);
  const head = el("div", { class: "layer-head" }, el("span", { class: "mission-title", title: label }, label));
  const node = el("div", { class: "layer report" }, head);
  if (!data || data.error) {
    node.dataset.status = "failed";
    node.append(el("p", { class: "detail error" }, data?.error ?? "Not propagated"));
    return node;
  }
  head.append(button("CSV", () => exportEvents(id, data)));

  const { contacts, eclipses, orbit } = data;
  const total = (spans, from, to) => spans.reduce((sum, span) => sum + span[to] - span[from], 0);
  const umbra = eclipses.filter((eclipse) => eclipse.kind === "umbra");
  const summary = [];
  if (events.savedStations().length) {
    summary.push(`${contacts.length} contacts · ${duration(total(contacts, "aos", "los"))}`);
  }
  if (events.eventOptions().eclipses) {
    const longest = Math.max(0, ...umbra.map((eclipse) => eclipse.stop - eclipse.start));
    summary.push(`${umbra.length} umbra · ${duration(total(umbra, "start", "stop"))}, longest ${duration(longest)}`);
  }
  const apsides = orbit.filter((event) => event.kind.endsWith("apsis")).length;
  if (apsides) summary.push(`${apsides} apsides`);
  if (orbit.length > apsides) summary.push(`${orbit.length - apsides} nodes`);
  node.append(el("p", { class: "detail" }, summary.join(" · ") || "No searches. Add a ground station or turn on a search."));

  if (contacts.length) {
    node.append(
      table(
        ["Station", "AOS", "Duration", "Max el"],
        contacts.map((contact) => ({
          at: contact.aos,
          cells: [
            events.stationLabel(contact.station),
            clock(contact.aos).slice(5, 16),
            minutes(contact.los - contact.aos),
            `${contact.maxElevationDeg.toFixed(1)}°`,
          ],
        })),
      ),
    );
  }
  if (eclipses.length) {
    // One row per penumbra, with the umbra inside it (none for a partial or annular pass).
    const penumbrae = eclipses.filter((eclipse) => eclipse.kind === "penumbra");
    node.append(
      table(
        ["Eclipse", "Entry", "Umbra", "Total"],
        penumbrae.map((penumbra, index) => {
          const inner = umbra.find((eclipse) => eclipse.start >= penumbra.start && eclipse.stop <= penumbra.stop);
          return {
            at: penumbra.start,
            cells: [
              String(index + 1),
              clock(penumbra.start).slice(5, 16),
              inner ? minutes(inner.stop - inner.start) : "—",
              minutes(penumbra.stop - penumbra.start),
            ],
          };
        }),
      ),
    );
  }
  return node;
}

/** `m:ss`. */
function minutes(seconds) {
  const whole = Math.round(seconds);
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
}

/** `rows`: `[{at, cells}]`; a row click sets the clock to `at`. */
function table(headings, rows) {
  return el(
    "div",
    { class: "table-wrap events-table" },
    el(
      "table",
      { class: "report-table" },
      el("thead", {}, el("tr", {}, ...headings.map((text) => el("th", {}, text)))),
      el(
        "tbody",
        {},
        ...rows.map(({ at, cells }) =>
          el(
            "tr",
            { class: "pick", title: "Set the clock here", onclick: () => globe.setClock(at) },
            ...cells.map((text) => el("td", {}, text)),
          ),
        ),
      ),
    ),
  );
}

/** One row per event: `utc, end_utc, event, station, max_elevation_deg`. */
function exportEvents(id, { contacts, eclipses, orbit }) {
  const rows = [
    ...contacts.map((contact) => [contact.aos, contact.los, "contact", contact.station, contact.maxElevationDeg]),
    ...eclipses.map((eclipse) => [eclipse.start, eclipse.stop, eclipse.kind, "", ""]),
    ...orbit.map((event) => [event.at, null, event.kind, "", ""]),
  ]
    .sort((a, b) => a[0] - b[0])
    .map(([start, stop, ...rest]) => [isoUtc(start), stop === null ? "" : isoUtc(stop), ...rest]);
  download(
    `${id}-events.csv`,
    new Blob([toCsv(["utc", "end_utc", "event", "station", "max_elevation_deg"], rows)], { type: "text/csv" }),
  );
}
