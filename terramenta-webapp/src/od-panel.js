/**
 * Orbit determination section (Analysis tab): studies from `od.js` as cards
 * (estimate, σ, truth error, iterations, residual statistics and charts;
 * apply, CSV, TDM) and an editor. The editor tracks every selected station
 * with one σ per type; per-station tracking in a project file is shown, and
 * replaced on save.
 */

import { lineChart } from "./chart.js";
import { download, el, row, section } from "./dom.js";
import { savedStations, stationLabel } from "./events.js";
import * as globe from "./globe.js";
import * as od from "./od.js";
import { AXIS, SERIES } from "./palette.js";
import { PROJECT_VERSION, layerDisplay, parseProject } from "./project.js";
import { isoUtc, toCsv } from "./reports.js";
import { button } from "./widgets.js";

const TYPE_COLORS = {
  range: AXIS[1],
  rangeRate: SERIES.amber,
  azimuth: AXIS[2],
  elevation: AXIS[0],
  rightAscension: SERIES.violet,
  declination: SERIES.pink,
  positionX: SERIES.sky,
  positionY: SERIES.teal,
  positionZ: SERIES.ocean,
};
const RSW = [
  ["Radial", AXIS[0]],
  ["In-track", AXIS[1]],
  ["Cross-track", AXIS[2]],
];
/** A gap longer than this breaks a residual line (between passes). */
const BREAK_S = 300;

export function mountOrbitDetermination(bind, signal) {
  const editor = mountEditor();
  const list = el("div", { class: "layers" });
  let entries = [];
  let stations = null;
  let lastState = null;

  const render = () => {
    const studies = od.savedStudies();
    list.replaceChildren(...studies.map((study) => studyCard(study, editor, () => lastState)));
    list.classList.toggle("empty", !studies.length);
    editor.refresh();
  };
  const unsubscribe = [od.subscribe(render), od.onResult(render)];
  signal?.addEventListener("abort", () => unsubscribe.forEach((off) => off()));

  bind(list, (state) => {
    lastState = state;
    const next = [...globe.recordedSources().spacecraft.values()];
    if (next.length !== entries.length || next.some((entry, index) => entry !== entries[index]) || savedStations() !== stations) {
      entries = next;
      stations = savedStations();
      render();
    }
    node.hidden = entries.length < 1 && !od.savedStudies().length;
  });

  const node = section(
    "Orbit determination",
    list,
    editor.node,
    el(
      "p",
      { class: "detail" },
      "Batch: weighted least squares about the spacecraft's initial state, its covariance as a priori, σ-editing, optional Cd/Cr. EKF: sequential, from the initial state and covariance, estimate at the last observation. Residuals: observed − computed; batch final, EKF before each update.",
    ),
  );
  return node;
}

const label = (id) => globe.recordedSources().spacecraft.get(id)?.display.label ?? id;
const m = (km) => `${(km * 1e3).toFixed(km * 1e3 < 10 ? 2 : 1)}`;
const mms = (kms) => `${(kms * 1e6).toFixed(1)}`;

// --- Cards -----------------------------------------------------------------------

function describe(study) {
  const s = od.settings(study);
  const parts = [label(s.spacecraft), od.ESTIMATORS[s.estimator]];
  if (s.stations) parts.push(s.stations.map(stationLabel).join(", "));
  if (s.simulate) {
    const types = [...new Set(s.simulate.tracking.map((t) => od.OBSERVATION_TYPES[t.type]?.label ?? t.type))];
    parts.push(`simulated from ${label(s.simulate.truth)}: ${types.join(", ").toLowerCase()}`);
  }
  if (s.tdm) parts.push(s.tdm.startsWith("data:") ? "TDM (inline)" : `TDM ${s.tdm}`);
  if (s.solveFor.length) parts.push(`solve ${s.solveFor.map((p) => od.PARAMETERS[p.parameter]).join(", ")}`);
  return parts.join(" · ");
}

function studyCard(study, editor, getState) {
  const kept = od.odResult(study.id);
  const busy = od.isRunning(study.id);
  const status = el("p", { class: "detail" });
  const run = button(kept ? "Re-run" : "Run", () => od.runOrbitDetermination(study.id));
  run.disabled = busy;
  const apply = button("Apply", () => {
    const layer = getState()?.ephemerides.layers.find((candidate) => candidate.id === study.spacecraft);
    try {
      od.applyEstimate(study.id, layer ? layerDisplay(layer) : undefined);
    } catch (error) {
      alert(`Apply failed: ${error.message ?? error}`);
    }
  });
  apply.disabled = !kept?.result || kept.stale || kept.applied;
  apply.title = "Replace the spacecraft's initial state, covariance and estimated parameters with the estimate";
  const csv = button("CSV", () =>
    download(`${study.id}-residuals.csv`, new Blob([toCsv(od.RESIDUAL_COLUMNS, od.residualRows(kept.result))], { type: "text/csv" })),
  );
  csv.disabled = !kept?.result;
  const tdm = button("TDM", () => download(`${study.id}.tdm`, new Blob([kept.result.tdm], { type: "text/plain" })));
  tdm.disabled = !kept?.result?.tdm;
  tdm.title = "Observations used, CCSDS TDM";
  const node = el(
    "div",
    { class: "layer report" },
    el("div", { class: "layer-head" }, el("span", { class: "mission-title", title: study.id }, study.label ?? study.id)),
    el("p", { class: "detail" }, describe(study)),
    el(
      "div",
      { class: "buttons" },
      run,
      apply,
      csv,
      tdm,
      button("Edit", () => editor.edit(study)),
      button("Remove", () => od.removeStudy(study.id)),
    ),
    status,
  );
  if (busy) {
    status.textContent = "Running…";
    return node;
  }
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
  node.dataset.status = kept.stale ? "stale" : result.converged ? "ready" : "failed";
  const iterations = result.iterations.length;
  status.textContent = [
    kept.stale ? "Stale: inputs changed" : null,
    kept.applied ? "applied" : null,
    result.estimator === "batch"
      ? result.converged
        ? `Converged in ${iterations} iteration${iterations === 1 ? "" : "s"}`
        : `Not converged after ${iterations} iterations`
      : null,
    `RMS ${result.rms.toFixed(2)}`,
    `${result.observations} observations${result.edited ? `, ${result.edited} edited` : ""}`,
    `${(kept.ms / 1000).toFixed(2)} s`,
  ]
    .filter(Boolean)
    .join(" · ");
  if (kept.stale || !result.converged) status.classList.add("error");

  const sigma = result.sigmaRsw;
  const rows = [
    ["Epoch (UTC)", isoUtc(result.epoch).slice(0, 19).replace("T", " "), "", ""],
    ["1σ position m", m(sigma[0]), m(sigma[1]), m(sigma[2])],
    ["1σ velocity mm/s", mms(sigma[3]), mms(sigma[4]), mms(sigma[5])],
  ];
  if (result.truthErrorRsw) {
    const e = result.truthErrorRsw;
    rows.push(
      ["Truth error m", ...[0, 1, 2].map((i) => `${m(e[i])} (${(Math.abs(e[i]) / sigma[i]).toFixed(1)}σ)`)],
      ["Truth error mm/s", mms(e[3]), mms(e[4]), mms(e[5])],
    );
  }
  node.append(table(["Estimate", "Radial", "In-track", "Cross"], rows));
  if (result.parameters.length) {
    node.append(
      table(
        ["Parameter", "A priori", "Estimate", "1σ"],
        result.parameters.map((p) => [od.PARAMETERS[p.parameter], p.apriori.toFixed(3), p.value.toFixed(3), p.sigma.toFixed(3)]),
      ),
    );
  }
  if (iterations) {
    node.append(
      table(
        ["Iteration", "RMS", "Edited", "|δr| m", "|δv| mm/s"],
        result.iterations.map((it, index) => [
          String(index + 1),
          it.rms.toFixed(3),
          String(it.edited),
          (it.correctionKm * 1e3).toPrecision(3),
          (it.correctionKmS * 1e6).toPrecision(3),
        ]),
      ),
    );
  }
  node.append(
    table(
      ["Station", "Type", "Count", "Mean", "RMS"],
      od.residualStats(result).map((g) => [
        g.station ? stationLabel(g.station) : "—",
        od.OBSERVATION_TYPES[g.type].label,
        g.edited ? `${g.count} (${g.edited} edited)` : String(g.count),
        Number.isFinite(g.mean) ? od.formatResidual(g.type, g.mean) : "—",
        Number.isFinite(g.rms) ? od.formatResidual(g.type, g.rms) : "—",
      ]),
    ),
  );
  node.append(...residualChart(result));
  if (result.history) node.append(...historyChart(result.history));
  return node;
}

function residualChart(result) {
  const series = od.normalisedResiduals(result).map(({ type, points }) => ({
    label: od.OBSERVATION_TYPES[type].label,
    color: TYPE_COLORS[type],
    points: points.flatMap((point, index) =>
      index && point[0] - points[index - 1][0] > BREAK_S ? [[point[0] - 1, Number.NaN], point] : [point],
    ),
  }));
  const chart = lineChart({ series, unit: "σ", onPick: (t) => globe.setClock(t) });
  return [
    el(
      "p",
      { class: "detail chart-legend" },
      el("span", {}, result.estimator === "ekf" ? "Post-fit / σ" : "Residual / σ"),
      ...series.map(({ label, color }) => el("span", {}, el("i", { style: `background: ${color}` }), label)),
    ),
    el("div", { class: "chart-wrap" }, chart.node),
  ];
}

function historyChart(history) {
  const series = RSW.map(([label, color], axis) => ({
    label,
    color,
    points: history.times.map((t, index) => [t, history.sigmaRsw[index][axis] * 1e3]),
  }));
  const chart = lineChart({ series, unit: "m", onPick: (t) => globe.setClock(t) });
  return [
    el(
      "p",
      { class: "detail chart-legend" },
      el("span", {}, "Posterior 1σ"),
      ...series.map(({ label, color }) => el("span", {}, el("i", { style: `background: ${color}` }), label)),
    ),
    el("div", { class: "chart-wrap" }, chart.node),
  ];
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

// --- Editor ----------------------------------------------------------------------

/** Editor tracking rows: types, display unit, display → globe unit, default σ (display). */
const TRACKING = [
  { types: ["range"], label: "Range", unit: "m", scale: 1e-3, sigma: 10 },
  { types: ["rangeRate"], label: "Range rate", unit: "mm/s", scale: 1e-6, sigma: 10 },
  { types: ["azimuth", "elevation"], label: "Az/El", unit: "″", scale: 1 / 3600, sigma: 18 },
  { types: ["rightAscension", "declination"], label: "RA/Dec", unit: "″", scale: 1 / 3600, sigma: 2 },
  { types: ["positionX", "positionY", "positionZ"], label: "GNSS", unit: "m", scale: 1e-3, sigma: 10 },
];

function mountEditor() {
  let editing = null;
  const number = (props = {}) => el("input", { class: "coordinate", type: "number", step: "any", ...props });

  const title = el("summary", {}, "Add an orbit determination study");
  const name = el("input", { class: "coordinate", type: "text", placeholder: "Name" });
  const spacecraft = el("select", { class: "select" });
  const estimator = el(
    "select",
    { class: "select" },
    ...Object.entries(od.ESTIMATORS).map(([value, text]) => el("option", { value }, text)),
  );
  const stations = el("div", { class: "timeline-capture" });
  const truth = el("select", { class: "select" });
  const tracking = TRACKING.map((spec) => ({
    spec,
    on: el("input", { type: "checkbox" }),
    sigma: number({ min: "0", placeholder: String(spec.sigma) }),
  }));
  const step = number({ min: "0", placeholder: "60" });
  const seed = number({ min: "0", step: "1", placeholder: "1" });
  const tdmUrl = el("input", { class: "coordinate", type: "text", placeholder: "TDM URL (optional)" });
  const tdmFile = el("input", {
    type: "file",
    accept: ".tdm,.kvn,.txt,text/plain",
    hidden: true,
    onchange: async (event) => {
      const [file] = event.target.files ?? [];
      event.target.value = "";
      if (file) tdmUrl.value = `data:text/plain,${encodeURIComponent(await file.text())}`;
    },
  });
  const solve = Object.keys(od.PARAMETERS).map((parameter) => ({
    parameter,
    on: el("input", { type: "checkbox" }),
    sigma: number({ min: "0", placeholder: "0.5" }),
  }));
  const editSigma = number({ min: "0", placeholder: "3 batch, 5 EKF" });
  const error = el("p", { class: "detail error", hidden: true });
  const submit = button("Add", save);
  const cancel = button("Cancel", reset, { hidden: true });

  const checked = (box) => [...box.querySelectorAll("input:checked")].map((input) => input.value);
  const choice = (id, text, on) =>
    el("label", { class: "toggle" }, el("input", { type: "checkbox", value: id, checked: on }), el("span", {}, text));

  function refresh() {
    const ids = [...globe.recordedSources().spacecraft.keys()];
    const [current, currentTruth] = [spacecraft.value, truth.value];
    spacecraft.replaceChildren(...ids.map((id) => el("option", { value: id }, label(id))));
    spacecraft.value = ids.includes(current) ? current : (ids[0] ?? "");
    truth.replaceChildren(el("option", { value: "" }, "None"), ...ids.map((id) => el("option", { value: id }, label(id))));
    truth.value = ids.includes(currentTruth) ? currentTruth : "";
    refreshStations(checked(stations));
  }

  function refreshStations(selected) {
    stations.replaceChildren(...savedStations().map((s) => choice(s.id, s.label ?? s.id, selected.includes(s.id))));
  }

  function edit(study) {
    reset();
    editing = study.id;
    const s = od.settings(study);
    name.value = study.label ?? study.id;
    refresh();
    spacecraft.value = s.spacecraft;
    estimator.value = s.estimator;
    refreshStations(s.stations ?? savedStations().map((station) => station.id));
    truth.value = s.simulate?.truth ?? "";
    for (const row of tracking) {
      const used = s.simulate?.tracking.find((t) => row.spec.types.includes(t.type));
      row.on.checked = Boolean(used);
      row.sigma.value = used?.sigma !== undefined ? Number((used.sigma / row.spec.scale).toPrecision(6)) : "";
    }
    step.value = s.simulate?.stepS ?? "";
    seed.value = s.simulate?.seed ?? "";
    tdmUrl.value = s.tdm ?? "";
    for (const p of solve) {
      const used = s.solveFor.find((candidate) => candidate.parameter === p.parameter);
      p.on.checked = Boolean(used);
      p.sigma.value = used?.sigma ?? "";
    }
    editSigma.value = study.editSigma ?? "";
    title.textContent = `Edit ${study.label ?? study.id}`;
    submit.textContent = "Save";
    cancel.hidden = false;
    node.open = true;
    node.scrollIntoView({ block: "nearest" });
  }

  function reset() {
    editing = null;
    name.value = "";
    estimator.value = "batch";
    for (const input of [step, seed, tdmUrl, editSigma, ...tracking.map((t) => t.sigma), ...solve.map((p) => p.sigma)]) input.value = "";
    for (const row of tracking) row.on.checked = row.spec.types[0] === "range" || row.spec.types[0] === "azimuth";
    for (const p of solve) p.on.checked = false;
    refresh();
    refreshStations(savedStations().map((station) => station.id));
    title.textContent = "Add an orbit determination study";
    submit.textContent = "Add";
    cancel.hidden = true;
    error.hidden = true;
  }

  function save() {
    error.hidden = true;
    try {
      const text = name.value.trim() || "Orbit determination";
      const study = { id: editing ?? od.uniqueStudyId(text), label: text, spacecraft: spacecraft.value };
      if (estimator.value !== "batch") study.estimator = estimator.value;
      const picked = checked(stations);
      if (picked.length !== savedStations().length) study.stations = picked;
      if (truth.value) {
        const rows = tracking.filter((row) => row.on.checked);
        if (!rows.length) throw new Error("Simulation: pick at least one tracking type.");
        study.simulate = {
          truth: truth.value,
          tracking: rows.flatMap(({ spec, sigma }) =>
            spec.types.map((type) => ({
              type,
              sigma: Number(((sigma.value.trim() ? Number(sigma.value) : spec.sigma) * spec.scale).toPrecision(12)),
            })),
          ),
        };
        if (step.value.trim()) study.simulate.stepS = Number(step.value);
        if (seed.value.trim()) study.simulate.seed = Number(seed.value);
      }
      if (tdmUrl.value.trim()) study.tdm = tdmUrl.value.trim();
      const solveFor = solve
        .filter((p) => p.on.checked)
        .map((p) => ({ parameter: p.parameter, sigma: p.sigma.value.trim() ? Number(p.sigma.value) : 0.5 }));
      if (solveFor.length) study.solveFor = solveFor;
      if (editSigma.value.trim()) study.editSigma = Number(editSigma.value);
      const sources = globe.recordedSources();
      // Same rules as a project file.
      parseProject(
        {
          terramenta: PROJECT_VERSION,
          spacecraft: [...sources.spacecraft].map(([id, entry]) => ({ id, propagation: entry.propagation })),
          groundStations: savedStations(),
          analysis: { orbitDetermination: [study] },
        },
        { warn: () => {} },
      );
      const previous = od.savedStudies().some((other) => other.id === editing);
      od.addStudy(study);
      if (!previous) od.runOrbitDetermination(study.id);
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
    row("A priori", spacecraft),
    row("Estimator", estimator),
    el("h3", { class: "editor-title" }, "Stations"),
    stations,
    el("h3", { class: "editor-title" }, "Simulated tracking"),
    row("Truth", truth),
    ...tracking.map(({ spec, on, sigma }) => row(`${spec.label} σ ${spec.unit}`, el("span", { class: "pair" }, on, sigma))),
    row("Step s", step),
    row("Seed", seed),
    el("h3", { class: "editor-title" }, "Tracking data"),
    row("TDM", el("span", { class: "pair" }, tdmUrl, tdmFile, button("File…", () => tdmFile.click()))),
    el("h3", { class: "editor-title" }, "Solve for (batch)"),
    ...solve.map(({ parameter, on, sigma }) => row(`${od.PARAMETERS[parameter]} σ`, el("span", { class: "pair" }, on, sigma))),
    row("Edit σ", editSigma),
    error,
    el("div", { class: "buttons" }, submit, cancel),
  );
  reset();
  return { node, edit, refresh };
}
