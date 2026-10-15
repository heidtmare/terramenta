/**
 * Analysis tab, reports half: saved plots, per-spacecraft reports, CSV and
 * CZML export. Both sections hide while the project has neither spacecraft
 * nor plots.
 *
 * Samples are the spacecraft's OEM (`globe.propagate` with `kvn`), parsed
 * and derived by `reports.js`, cached per `globe.recordedSources().spacecraft`
 * entry (re-propagating replaces the entry), with 1σ position columns from
 * the entry's `covariance`. Plots are `analysis.js`'s; clicking one sets the
 * clock.
 */

import * as analysis from "./analysis.js";
import * as coverage from "./coverage.js";
import { lineChart } from "./chart.js";
import { download, el, row, section } from "./dom.js";
import { clock, duration } from "./format.js";
import * as globe from "./globe.js";
import {
  PLOT_QUANTITIES,
  UNCERTAINTY_QUANTITIES,
  deltaVBudget,
  epochSeconds,
  isoUtc,
  parseOem,
  reportCsv,
  reportRows,
  summarize,
  toCsv,
  toCzml,
  columnName,
  withUncertainty,
} from "./reports.js";
import { stacCatalog, zip } from "./stac.js";
import { button } from "./widgets.js";
import * as workspace from "./workspace.js";

/** Quantities in a spacecraft report's table; uncertainty rows follow for covariance runs. */
const SUMMARY = ["altitudeKm", "smaKm", "eccentricity", "inclinationDeg", "periodMin", "perigeeAltKm", "apogeeAltKm"];

const cache = new WeakMap();
const stamps = new WeakMap();
let nextStamp = 0;

/** `{oem, rows, budget}` or `{error}` for spacecraft `id`; `null` if absent or failed. */
export function spacecraftData(id) {
  const entry = globe.recordedSources().spacecraft.get(id);
  if (!entry?.info) return null;
  let data = cache.get(entry);
  if (!data) {
    try {
      const oemText = globe.propagate({ ...entry.propagation, kvn: true }).oem;
      const oem = parseOem(oemText);
      const rows = withUncertainty(reportRows(oem), entry.info.covariance);
      data = { oem, oemText, rows, budget: deltaVBudget(entry.propagation) };
    } catch (error) {
      data = { error: error.message ?? String(error) };
    }
    cache.set(entry, data);
  }
  return data;
}

/** Changes whenever the spacecraft set, an entry, or a label/colour changes. */
function spacecraftSignature(state) {
  const looks = new Map(state.ephemerides.layers.map((layer) => [layer.id, `${layer.label}|${layer.style.pointColor}`]));
  return [...globe.recordedSources().spacecraft]
    .map(([id, entry]) => {
      if (!stamps.has(entry)) stamps.set(entry, ++nextStamp);
      return `${id}:${stamps.get(entry)}:${looks.get(id) ?? ""}`;
    })
    .join(",");
}

/** Label and colour from the spacecraft's layer, else its recorded display. */
function lookOf(id, state) {
  const layer = state.ephemerides.layers.find((candidate) => candidate.id === id);
  const display = globe.recordedSources().spacecraft.get(id)?.display ?? {};
  return {
    label: layer?.label || display.label || id,
    color: (layer?.style.pointColor ?? display.pointColor ?? "#ffffff").slice(0, 7),
  };
}

function burnMarkers(id, label) {
  const data = spacecraftData(id);
  if (!data?.budget) return [];
  return data.budget.burns.map((burn) => ({
    t: epochSeconds(burn.epoch),
    label: `${label}: ${burn.kind === "finite" ? "finite burn" : "burn"} ${burn.index}`,
  }));
}

export function mountReports(bind, signal) {
  return [mountPlots(bind, signal), mountSpacecraftReports(bind)];
}

// --- Plots -----------------------------------------------------------------------

function mountPlots(bind, signal) {
  const list = el("div", { class: "layers" });
  const empty = el("p", { class: "detail" }, "No plots. Add one below.");
  let cards = [];
  let signature = "";
  let editsPending = true;
  const unsubscribe = analysis.subscribe(() => (editsPending = true));
  signal?.addEventListener("abort", unsubscribe);

  const editor = mountPlotEditor();

  bind(list, (state) => {
    const scene = spacecraftSignature(state);
    if (editsPending || scene !== signature) {
      editsPending = false;
      signature = scene;
      cards = analysis.savedPlots().map((plot) => plotCard(plot, state));
      list.replaceChildren(...cards.map((card) => card.node));
      empty.hidden = cards.length > 0;
      node.hidden = !cards.length && !globe.recordedSources().spacecraft.size;
      editor.refresh(state);
    }
    for (const card of cards) card.chart.setCursor(state.sun.unixSeconds);
  });

  const node = section("Plots", empty, list, editor.node);
  return node;
}

function plotCard(plot, state) {
  const quantity = PLOT_QUANTITIES[plot.quantity];
  const series = plot.spacecraft.map((id) => {
    const look = lookOf(id, state);
    const data = spacecraftData(id);
    return { id, ...look, rows: data?.rows ?? [], missing: !data?.rows };
  });
  const chart = lineChart({
    series: series.map(({ label, color, rows }) => ({
      label,
      color,
      points: rows.map((row) => [row.t, row[plot.quantity]]),
    })),
    markers: series.flatMap(({ id, label }) => burnMarkers(id, label)),
    unit: quantity.unit,
    onPick: (t) => globe.setClock(t),
  });
  const legend = el(
    "p",
    { class: "detail chart-legend" },
    ...series.map(({ label, color, missing }) =>
      el("span", {}, el("i", { style: `background: ${color}` }), missing ? `${label} (no data)` : label),
    ),
  );
  const title = plot.label || `${quantity.label}${quantity.unit ? ` (${quantity.unit})` : ""}`;
  const node = el(
    "div",
    { class: "layer plot" },
    el(
      "div",
      { class: "layer-head" },
      el("span", { class: "mission-title", title }, title),
      button("CSV", () => exportPlot(plot, series)),
      button("Remove", () => analysis.removePlot(plot.id)),
    ),
    legend,
    el("div", { class: "chart-wrap" }, chart.node),
  );
  return { node, chart };
}

function exportPlot(plot, series) {
  const rows = series.flatMap(({ id, rows: samples }) =>
    samples.map((sample) => [isoUtc(sample.t), id, sample[plot.quantity]]),
  );
  download(
    `${plot.id}.csv`,
    new Blob([toCsv(["utc", "spacecraft", columnName(plot.quantity)], rows)], { type: "text/csv" }),
  );
}

function mountPlotEditor() {
  const quantity = el(
    "select",
    { class: "select" },
    ...Object.entries(PLOT_QUANTITIES).map(([key, { label, unit }]) =>
      el("option", { value: key }, unit ? `${label} (${unit})` : label),
    ),
  );
  const label = el("input", { class: "coordinate", type: "text", placeholder: "Optional" });
  const choices = el("div", { class: "timeline-capture" });
  const error = el("p", { class: "detail error", hidden: true });
  let known = "";

  function refresh(state) {
    const ids = [...globe.recordedSources().spacecraft.keys()];
    const next = ids.map((id) => `${id}|${lookOf(id, state).label}`).join(",");
    if (next === known) return;
    known = next;
    const checked = new Set(selected());
    choices.replaceChildren(
      ...ids.map((id) =>
        el(
          "label",
          { class: "toggle" },
          el("input", { type: "checkbox", value: id, checked: checked.has(id) }),
          el("span", {}, lookOf(id, state).label),
        ),
      ),
    );
    if (!ids.length) choices.append(el("p", { class: "detail" }, "No spacecraft in this project."));
  }

  const selected = () => [...choices.querySelectorAll("input:checked")].map((input) => input.value);

  function add() {
    const spacecraft = selected();
    if (!spacecraft.length) {
      error.textContent = "Pick at least one spacecraft.";
      error.hidden = false;
      return;
    }
    error.hidden = true;
    const text = label.value.trim();
    const plot = { id: analysis.uniqueId("plots", slug(text || quantity.value)), quantity: quantity.value, spacecraft };
    if (text) plot.label = text;
    analysis.addPlot(plot);
    label.value = "";
  }

  const node = el(
    "details",
    { class: "editor" },
    el("summary", {}, "Add plot"),
    row("Quantity", quantity),
    row("Label", label),
    el("h3", { class: "editor-title" }, "Spacecraft"),
    choices,
    error,
    el("div", { class: "buttons" }, button("Add", add)),
  );
  return { node, refresh };
}

// --- Spacecraft reports ----------------------------------------------------------

function mountSpacecraftReports(bind) {
  const list = el("div", { class: "layers" });
  const empty = el("p", { class: "detail" }, "No spacecraft. Add one in the Spacecraft tab.");
  let signature = null;

  const czml = button("CZML (all spacecraft)", () => exportCzml(lastState));
  const stacButton = button("STAC catalog (zip)", () => exportStac(lastState), {
    title: "STAC 1.1 catalog: spacecraft (ground track, OEM, CZML) and coverage results",
  });
  let lastState = null;

  bind(list, (state) => {
    lastState = state;
    const next = spacecraftSignature(state);
    if (next === signature) return;
    signature = next;
    const ids = [...globe.recordedSources().spacecraft.keys()];
    list.replaceChildren(...ids.map((id) => spacecraftReport(id, state)));
    empty.hidden = ids.length > 0;
    node.hidden = !ids.length;
    czml.disabled = !ids.some((id) => spacecraftData(id)?.oem);
  });

  const node = section(
    "Spacecraft reports",
    empty,
    list,
    el("div", { class: "buttons" }, czml, stacButton),
    el("p", { class: "detail" }, "GCRF/UTC. Osculating two-body elements; altitude over equatorial radius."),
  );
  return node;
}

function spacecraftReport(id, state) {
  const { label } = lookOf(id, state);
  const entry = globe.recordedSources().spacecraft.get(id);
  const data = spacecraftData(id);
  const head = el(
    "div",
    { class: "layer-head" },
    el("span", { class: "mission-title", title: label }, label),
    data?.rows ? button("CSV", () => exportReport(id, data.rows)) : null,
  );
  const node = el("div", { class: "layer report" }, head);
  if (!data || data.error) {
    node.dataset.status = "failed";
    node.append(el("p", { class: "detail error" }, entry?.error ?? data?.error ?? "Not propagated"));
    return node;
  }

  const { rows, budget } = data;
  const [start, stop] = [rows[0].t, rows.at(-1).t];
  node.append(
    el("p", { class: "detail" }, `${clock(start)} → ${clock(stop)} · ${duration(stop - start)} · ${rows.length} samples`),
    summaryTable(rows),
  );

  const burns = budget.burns.map((burn) =>
    el(
      "tr",
      {},
      el("td", {}, `${burn.kind === "finite" ? "Finite" : "Burn"} ${burn.index}`),
      el("td", {}, clock(epochSeconds(burn.epoch))),
      el("td", {}, `${(burn.deltaVKmS * 1000).toFixed(2)} m/s`),
      el("td", {}, `${burn.massAfterKg.toFixed(2)} kg`),
    ),
  );
  node.append(
    el(
      "p",
      { class: "detail" },
      `Δv ${(budget.totalDeltaVKmS * 1000).toFixed(2)} m/s · propellant ${budget.propellantKg.toFixed(2)} kg · ` +
        `final mass ${entry.info.finalMassKg.toFixed(2)} kg`,
    ),
  );
  if (burns.length) {
    node.append(
      el(
        "div",
        { class: "table-wrap" },
        el(
          "table",
          { class: "report-table" },
          el("thead", {}, el("tr", {}, ...["Burn", "Epoch", "Δv", "Mass after"].map((text) => el("th", {}, text)))),
          el("tbody", {}, ...burns),
        ),
      ),
    );
  }
  return node;
}

function summaryTable(rows) {
  const format = (key, value) =>
    Number.isFinite(value) ? Number(value.toFixed(PLOT_QUANTITIES[key].digits > 4 ? 6 : 3)).toLocaleString() : "—";
  const keys = [...SUMMARY, ...(rows.some((row) => row.sigmaRadialM !== undefined) ? Object.keys(UNCERTAINTY_QUANTITIES) : [])];
  const table = el(
    "table",
    { class: "report-table" },
    el("thead", {}, el("tr", {}, ...["", "Start", "End", "Min", "Max"].map((text) => el("th", {}, text)))),
    el(
      "tbody",
      {},
      ...keys.map((key) => {
        const { label, unit } = PLOT_QUANTITIES[key];
        const stats = summarize(rows, key);
        return el(
          "tr",
          {},
          el("th", {}, unit ? `${label} (${unit})` : label),
          ...(stats ? [stats.first, stats.last, stats.min, stats.max] : [NaN, NaN, NaN, NaN]).map((value) =>
            el("td", {}, format(key, value)),
          ),
        );
      }),
    ),
  );
  return el("div", { class: "table-wrap" }, table);
}

function exportReport(id, rows) {
  download(`${id}.csv`, new Blob([reportCsv(rows)], { type: "text/csv" }));
}

function exportCzml(state) {
  if (!state) return;
  const entries = [...globe.recordedSources().spacecraft.keys()]
    .map((id) => ({ id, ...lookOf(id, state), oem: spacecraftData(id)?.oem }))
    .filter((entry) => entry.oem);
  const name = workspace.snapshot().project?.name ?? "Terramenta";
  const czml = toCzml(name, entries, { unixSeconds: state.sun.unixSeconds, timeScale: state.sun.timeScale });
  download(`${slug(name) || "spacecraft"}.czml`, new Blob([JSON.stringify(czml)], { type: "application/json" }));
}

/** STAC catalog of propagated spacecraft and coverage results, zipped. */
function exportStac(state) {
  if (!state) return;
  const name = workspace.snapshot().project?.name ?? "Terramenta";
  const clockInfo = { unixSeconds: state.sun.unixSeconds, timeScale: state.sun.timeScale };
  const spacecraft = [];
  for (const [id, entry] of globe.recordedSources().spacecraft) {
    const data = spacecraftData(id);
    if (!data?.oemText) continue;
    const look = lookOf(id, state);
    const [start, stop] = [data.rows[0].t, data.rows.at(-1).t];
    let track = null;
    try {
      // ≤ ~2000 points per track.
      track = globe.groundTrack({ ...entry.propagation, kvn: false }, { stepS: Math.max(60, (stop - start) / 2000) });
    } catch {
      // Item without geometry.
    }
    const czml = toCzml(look.label, [{ id, ...look, oem: data.oem }], clockInfo);
    spacecraft.push({ id, label: look.label, start, stop, track, oem: data.oemText, czml });
  }
  const studies = coverage.savedCoverage().flatMap((study) => {
    const kept = coverage.coverageResult(study.id);
    if (!kept?.result) return [];
    const { result } = kept;
    const s = coverage.settings(study);
    const summary = coverage.summarizeCoverage(result);
    return [
      {
        id: study.id,
        label: study.label ?? study.id,
        start: result.start,
        stop: result.stop,
        bounds: s.bounds,
        properties: {
          "terramenta:spacecraft": s.spacecraft,
          "terramenta:access": s.access,
          "terramenta:min_elevation_deg": s.minElevationDeg,
          "terramenta:h3_resolution": result.resolution,
          "terramenta:step_s": result.stepS,
          "terramenta:cells": summary.cells,
          "terramenta:covered_share": summary.coveredShare,
          "terramenta:mean_fraction": summary.meanFraction,
          "terramenta:median_max_gap_s": summary.medianMaxGapS,
          "terramenta:worst_max_gap_s": summary.worstMaxGapS,
          "terramenta:mean_revisit_s": summary.meanRevisitS,
          "terramenta:stale": kept.stale,
        },
        geojson: coverage.coverageFeatures(result, s.metric),
        csv: toCsv(coverage.COVERAGE_COLUMNS, coverage.coverageRows(result)),
      },
    ];
  });
  const id = slug(name) || "terramenta";
  const files = stacCatalog({ id, title: name, spacecraft, coverage: studies });
  download(`${id}-stac.zip`, new Blob([zip(files)], { type: "application/zip" }));
}

function slug(text) {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}
