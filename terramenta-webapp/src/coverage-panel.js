/**
 * Coverage section (Analysis tab): studies from `coverage.js` as cards with a
 * summary, the shaded metric's legend, Run/Show, CSV and GeoJSON export, and
 * an editor. The shown study follows every state via `coverage.syncCoverage`.
 */

import * as coverage from "./coverage.js";
import { download, el, row, section } from "./dom.js";
import { duration } from "./format.js";
import * as globe from "./globe.js";
import { PROJECT_VERSION, parseProject } from "./project.js";
import { savedAttitudes } from "./attitude.js";
import { savedStations } from "./events.js";
import { toCsv } from "./reports.js";
import { button } from "./widgets.js";

/** H3 resolution → approximate cell edge, km. */
const EDGE_KM = [1281, 483, 183, 69, 26, 9.9, 3.7, 1.4];
/** Cells on the globe per resolution. */
const CELLS = [122, 842, 5882, 41162, 288122, 2016842, 14117882, 98825162];

export function mountCoverage(bind, signal) {
  const editor = mountEditor();
  const list = el("div", { class: "layers" });
  let entries = [];
  let profiles = null;

  const render = () => {
    const studies = coverage.savedCoverage();
    list.replaceChildren(...studies.map((study) => studyCard(study, editor)));
    list.classList.toggle("empty", !studies.length);
    editor.refresh();
  };
  const unsubscribe = [coverage.subscribe(render), coverage.onResult(render)];
  signal?.addEventListener("abort", () => unsubscribe.forEach((off) => off()));

  bind(list, () => {
    coverage.syncCoverage();
    const next = [...globe.recordedSources().spacecraft.values()];
    const attitudes = savedAttitudes();
    if (attitudes !== profiles || next.length !== entries.length || next.some((entry, index) => entry !== entries[index])) {
      entries = next;
      profiles = attitudes;
      render();
    }
    node.hidden = !entries.length && !coverage.savedCoverage().length;
  });

  const node = section(
    "Coverage",
    list,
    editor.node,
    el(
      "p",
      { class: "detail" },
      "H3 cells, covered while their centre is in view: above the elevation mask, and inside a sensor's field of view for sensor access. Sampled each step; gaps resolve to one step.",
    ),
  );
  return node;
}

// --- Cards -----------------------------------------------------------------------

function studyCard(study, editor) {
  const s = coverage.settings(study);
  const kept = coverage.coverageResult(study.id);
  const names = s.spacecraft.map((id) => globe.recordedSources().spacecraft.get(id)?.display.label ?? id);
  const craft = names.length > 2 ? `${names.length} spacecraft` : names.join(", ");
  const access = s.access === "sensors" ? "sensor FOV" : `horizon ≥ ${s.minElevationDeg}°`;
  const region = s.bounds ? `[${s.bounds.join(", ")}]` : "globe";
  const status = el("p", { class: "detail" });
  const metric = el(
    "select",
    { class: "select", title: "Shaded metric" },
    ...Object.entries(coverage.METRICS).map(([key, { label }]) => el("option", { value: key }, label)),
  );
  metric.value = s.metric;
  metric.onchange = () => coverage.setMetric(study.id, metric.value);
  const run = button(kept ? "Re-run" : "Run", () => coverage.runCoverage(study.id));
  const show = button(study.show ? "Hide" : "Show", () => coverage.showCoverage(study.show ? null : study.id));
  const exports = [
    button("CSV", () => exportCsv(study, kept.result)),
    button("GeoJSON", () => exportGeoJson(study, kept.result, s.metric)),
  ];
  for (const node of exports) node.disabled = !kept?.result;
  const node = el(
    "div",
    { class: "layer report" },
    el("div", { class: "layer-head" }, el("span", { class: "mission-title", title: study.id }, study.label ?? study.id)),
    el(
      "p",
      { class: "detail", title: names.join(", ") },
      `${craft} · ${access} · H3 ${s.resolution} (~${EDGE_KM[s.resolution]} km) · ${region} · ${s.stepS} s`,
    ),
    el("div", { class: "buttons" }, run, show, metric, ...exports, button("Edit", () => editor.edit(study)), button("Remove", () => coverage.removeCoverage(study.id))),
    status,
  );
  if (!kept) {
    status.textContent = "Not run";
    return node;
  }
  if (kept.error) {
    status.textContent = kept.error;
    status.classList.add("error");
    node.dataset.status = "failed";
    return node;
  }
  const { result } = kept;
  node.dataset.status = kept.stale ? "stale" : "ready";
  const summary = coverage.summarizeCoverage(result);
  status.textContent = [
    kept.stale ? "Stale: inputs changed" : null,
    `${result.samples} samples · ${summary.cells} cells · ${(kept.ms / 1000).toFixed(2)} s`,
  ]
    .filter(Boolean)
    .join(" · ");
  if (kept.stale) status.classList.add("error");
  const { min, max } = coverage.metricRange(result, s.metric);
  const { format, label } = coverage.METRICS[s.metric];
  node.append(
    table(
      ["Statistic", "Value"],
      [
        ["Cells ever seen", `${(100 * summary.coveredShare).toFixed(1)} %`],
        ["Mean time covered", `${(100 * summary.meanFraction).toFixed(2)} %`],
        ["Max gap, median cell", duration(summary.medianMaxGapS)],
        ["Max gap, worst cell", duration(summary.worstMaxGapS)],
        ["Mean revisit", summary.meanRevisitS === null ? "—" : duration(summary.meanRevisitS)],
      ],
    ),
    el(
      "div",
      { class: "ramp-legend" },
      el("span", {}, label),
      el("span", {}, format(min)),
      el("i", { class: "ramp", style: `background: linear-gradient(to right, ${coverage.RAMP.join(", ")})` }),
      el("span", {}, format(max)),
    ),
  );
  return node;
}

function table(headings, rows) {
  return el(
    "div",
    { class: "table-wrap" },
    el(
      "table",
      { class: "report-table" },
      el("thead", {}, el("tr", {}, ...headings.map((text) => el("th", {}, text)))),
      el("tbody", {}, ...rows.map((cells) => el("tr", {}, ...cells.map((text) => el("td", {}, text))))),
    ),
  );
}

function exportCsv(study, result) {
  download(`${study.id}.csv`, new Blob([toCsv(coverage.COVERAGE_COLUMNS, coverage.coverageRows(result))], { type: "text/csv" }));
}

function exportGeoJson(study, result, metric) {
  const collection = coverage.coverageGeoJson(result, metric);
  // Every statistic on every cell, for a GIS.
  const byId = new Map(result.cells.map((cell) => [cell.id, cell]));
  for (const feature of collection.features) {
    const { boundary: _boundary, ...cell } = byId.get(feature.properties.title);
    Object.assign(feature.properties, cell);
  }
  download(`${study.id}.geojson`, new Blob([JSON.stringify(collection)], { type: "application/geo+json" }));
}

// --- Editor ----------------------------------------------------------------------

function mountEditor() {
  let editing = null;
  const number = (props = {}) => el("input", { class: "coordinate", type: "number", step: "any", ...props });
  const select = (options, value) => {
    const node = el("select", { class: "select" }, ...options.map(([key, label]) => el("option", { value: key }, label)));
    node.value = value;
    return node;
  };

  const title = el("summary", {}, "Add a coverage study");
  const name = el("input", { class: "coordinate", type: "text", placeholder: "Name" });
  const choices = el("div", { class: "timeline-capture" });
  const access = select(
    [
      ["horizon", "Line of sight above mask"],
      ["sensors", "Attitude sensor FOV"],
    ],
    "horizon",
  );
  const elevation = number({ min: "-90", max: "89", value: "0" });
  const resolution = select(
    EDGE_KM.map((edge, index) => [String(index), `${index} · ~${edge} km · ${CELLS[index].toLocaleString()} on the globe`]),
    String(coverage.COVERAGE_DEFAULTS.resolution),
  );
  const bounds = ["West", "South", "East", "North"].map((placeholder) => number({ placeholder }));
  const step = number({ min: "0", placeholder: String(coverage.COVERAGE_DEFAULTS.stepS) });
  const error = el("p", { class: "detail error", hidden: true });
  const submit = button("Add", save);
  const cancel = button("Cancel", reset, { hidden: true });

  function refresh() {
    const checked = new Set(selected());
    choices.replaceChildren(
      ...[...globe.recordedSources().spacecraft].map(([id, entry]) =>
        el(
          "label",
          { class: "toggle" },
          el("input", { type: "checkbox", value: id, checked: checked.has(id) }),
          el("span", {}, entry.display.label ?? id),
        ),
      ),
    );
  }

  const selected = () => [...choices.querySelectorAll("input:checked")].map((input) => input.value);

  function edit(study) {
    reset();
    editing = study.id;
    const s = coverage.settings(study);
    name.value = study.label ?? study.id;
    refresh();
    for (const input of choices.querySelectorAll("input")) input.checked = s.spacecraft.includes(input.value);
    access.value = s.access;
    elevation.value = s.minElevationDeg;
    resolution.value = String(s.resolution);
    bounds.forEach((input, index) => (input.value = s.bounds?.[index] ?? ""));
    step.value = study.stepS ?? "";
    title.textContent = `Edit ${study.label ?? study.id}`;
    submit.textContent = "Save";
    cancel.hidden = false;
    node.open = true;
    node.scrollIntoView({ block: "nearest" });
  }

  function reset() {
    editing = null;
    name.value = "";
    access.value = "horizon";
    elevation.value = "0";
    resolution.value = String(coverage.COVERAGE_DEFAULTS.resolution);
    for (const input of bounds) input.value = "";
    step.value = "";
    refresh();
    for (const input of choices.querySelectorAll("input")) input.checked = true;
    title.textContent = "Add a coverage study";
    submit.textContent = "Add";
    cancel.hidden = true;
    error.hidden = true;
  }

  function save() {
    error.hidden = true;
    try {
      const label = name.value.trim() || "Coverage";
      const previous = coverage.savedCoverage().find((study) => study.id === editing);
      const study = { id: editing ?? coverage.uniqueCoverageId(label), label, spacecraft: selected() };
      if (access.value !== "horizon") study.access = access.value;
      if (Number(elevation.value)) study.minElevationDeg = Number(elevation.value);
      study.resolution = Number(resolution.value);
      const box = bounds.map((input) => input.value.trim());
      if (box.some(Boolean)) {
        if (!box.every(Boolean)) throw new Error("Bounds: give all four, or none for the globe.");
        study.bounds = box.map(Number);
      }
      if (step.value.trim()) study.stepS = Number(step.value);
      if (previous?.metric) study.metric = previous.metric;
      if (previous?.show) study.show = true;
      const entries = globe.recordedSources().spacecraft;
      // Same rules as a project file.
      parseProject(
        {
          terramenta: PROJECT_VERSION,
          spacecraft: [...entries].map(([id, entry]) => ({ id, propagation: entry.propagation })),
          groundStations: savedStations(),
          attitudes: savedAttitudes().filter((profile) => entries.has(profile.spacecraft)),
          analysis: { coverage: [study] },
        },
        { warn: () => {} },
      );
      coverage.addCoverage(study);
      if (!previous) coverage.runCoverage(study.id);
      reset();
    } catch (caught) {
      error.textContent = caught.errors ? caught.errors.join("\n") : (caught.message ?? String(caught));
      error.hidden = false;
    }
  }

  const node = el(
    "details",
    { class: "editor" },
    title,
    row("Name", name),
    el("h3", { class: "editor-title" }, "Spacecraft"),
    choices,
    row("Access", access),
    row("Mask °", elevation),
    row("H3 res", resolution),
    row("Bounds °", el("span", { class: "pair" }, ...bounds)),
    row("Step s", step),
    error,
    el("div", { class: "buttons" }, submit, cancel),
  );
  reset();
  return { node, edit, refresh };
}
