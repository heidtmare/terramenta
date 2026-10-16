/**
 * Monte Carlo section (Analysis tab): studies from `monte-carlo.js` as cards
 * (counts, end-quantity statistics, histograms, RSW scatter; CSV) and an editor.
 */

import { histogramChart, scatterChart } from "./chart.js";
import { download, el, row, section } from "./dom.js";
import * as globe from "./globe.js";
import * as mc from "./monte-carlo.js";
import { AXIS, SERIES } from "./palette.js";
import { PROJECT_VERSION, parseProject } from "./project.js";
import { toCsv } from "./reports.js";
import { button } from "./widgets.js";

export function mountMonteCarlo(bind, signal) {
  const editor = mountEditor();
  const list = el("div", { class: "layers" });
  let entries = [];

  const render = () => {
    const studies = mc.savedStudies();
    list.replaceChildren(...studies.map((study) => studyCard(study, editor)));
    list.classList.toggle("empty", !studies.length);
    editor.refresh();
  };
  const unsubscribe = [mc.subscribe(render), mc.onResult(render)];
  signal?.addEventListener("abort", () => unsubscribe.forEach((off) => off()));

  bind(list, () => {
    const next = [...globe.recordedSources().spacecraft.values()];
    if (next.length !== entries.length || next.some((entry, index) => entry !== entries[index])) {
      entries = next;
      render();
    }
    node.hidden = entries.length < 1 && !mc.savedStudies().length;
  });

  const node = section(
    "Monte Carlo",
    list,
    editor.node,
    el(
      "p",
      { class: "detail" },
      "Dispersed runs of the spacecraft's sequence, or its burns. Initial state drawn from its covariance; per-burn magnitude, pointing, thrust and Isp errors (1σ). Open loop flies the nominal target solutions; closed loop re-targets each trial from its true state.",
    ),
  );
  return node;
}

const label = (id) => globe.recordedSources().spacecraft.get(id)?.display.label ?? id;

// --- Cards -----------------------------------------------------------------------

function describe(study) {
  const s = mc.settings(study);
  const parts = [label(s.spacecraft), `${s.samples} samples`, mc.GUIDANCE[s.guidance], `seed ${s.seed}`];
  const sigmas = Object.entries(mc.DISPERSIONS)
    .filter(([key]) => s.dispersions[key])
    .map(([key, { label, unit, scale }]) => `${label} ${Number((s.dispersions[key] / scale).toPrecision(6))}${unit === "%" ? "%" : ` ${unit}`}`);
  if (sigmas.length) parts.push(`σ: ${sigmas.join(", ")}`);
  return parts.join(" · ");
}

function format(quantity, value) {
  if (!Number.isFinite(value)) return "—";
  const digits = mc.MC_QUANTITIES[quantity]?.digits ?? 4;
  return Math.abs(value) >= 1e5 ? value.toExponential(digits) : value.toFixed(digits);
}

const unitOf = (quantity) => mc.MC_QUANTITIES[quantity]?.unit ?? "";

function studyCard(study, editor) {
  const kept = mc.monteCarloResult(study.id);
  const busy = mc.isRunning(study.id);
  const status = el("p", { class: "detail" });
  const run = button(kept ? "Re-run" : "Run", () => mc.runMonteCarlo(study.id));
  run.disabled = busy;
  const csv = button("CSV", () =>
    download(`${study.id}-trials.csv`, new Blob([toCsv(mc.trialColumns(kept.result), mc.trialRows(kept.result))], { type: "text/csv" })),
  );
  csv.disabled = !kept?.result;
  csv.title = "One row per trial; trial 0 the nominal";
  const node = el(
    "div",
    { class: "layer report" },
    el("div", { class: "layer-head" }, el("span", { class: "mission-title", title: study.id }, study.label ?? study.id)),
    el("p", { class: "detail" }, describe(study)),
    el("div", { class: "buttons" }, run, csv, button("Edit", () => editor.edit(study)), button("Remove", () => mc.removeStudy(study.id))),
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
  const bad = result.failed + result.unconverged;
  node.dataset.status = kept.stale ? "stale" : bad ? "failed" : "ready";
  status.textContent = [
    kept.stale ? "Stale: inputs changed" : null,
    `${result.samples - result.failed}/${result.samples} trials`,
    result.failed ? `${result.failed} failed` : null,
    result.unconverged ? `${result.unconverged} not converged` : null,
    `${(kept.ms / 1000).toFixed(2)} s`,
  ]
    .filter(Boolean)
    .join(" · ");
  if (kept.stale || bad) status.classList.add("error");

  const statRow = (name, unit, nominal, s, fmt) => [
    unit ? `${name} ${unit}` : name,
    fmt(nominal),
    s ? fmt(s.mean) : "—",
    s ? fmt(s.sigma) : "—",
    s ? fmt(s.p05) : "—",
    s ? fmt(s.p95) : "—",
    s ? `${fmt(s.min)} – ${fmt(s.max)}` : "—",
  ];
  const ms = (kms) => (Number.isFinite(kms) ? (kms * 1e3).toFixed(2) : "—");
  const seconds = (s) => (Number.isFinite(s) ? s.toFixed(1) : "—");
  node.append(
    table(
      ["End", "Nominal", "Mean", "1σ", "5%", "95%", "Range"],
      [
        ...result.quantities.map((q, i) =>
          statRow(mc.MC_QUANTITIES[q]?.label ?? q, unitOf(q), result.nominal.values[i], result.summaries[i], (v) => format(q, v)),
        ),
        statRow("Δv", "m/s", result.nominal.deltaVKmS, result.deltaVKmS, ms),
        statRow("Duration", "s", result.nominal.durationS, result.durationS, seconds),
      ],
    ),
  );
  if (result.dispersion) {
    const s = result.dispersion.sigmaRsw;
    const km = (v) => (v < 1 ? `${(v * 1e3).toFixed(1)} m` : `${v.toFixed(3)} km`);
    const mms = (v) => (v < 1e-3 ? `${(v * 1e6).toFixed(1)} mm/s` : `${(v * 1e3).toFixed(3)} m/s`);
    node.append(
      table(
        ["End 1σ", "Radial", "In-track", "Cross"],
        [
          ["Position", km(s[0]), km(s[1]), km(s[2])],
          ["Velocity", mms(s[3]), mms(s[4]), mms(s[5])],
        ],
      ),
    );
  }
  const ok = result.trials.filter((t) => !t.error);
  const colors = [AXIS[2], SERIES.amber, AXIS[1], SERIES.violet, SERIES.sky, SERIES.pink, AXIS[0]];
  result.quantities.forEach((q, i) => {
    const values = ok.map((t) => t.values[i]);
    node.append(
      el("p", { class: "detail chart-legend" }, el("span", {}, `${mc.MC_QUANTITIES[q]?.label ?? q}: trials, nominal dashed`)),
      el(
        "div",
        { class: "chart-wrap" },
        histogramChart({ ...mc.histogram(values), marker: result.nominal.values[i], color: colors[i % colors.length], unit: unitOf(q) }),
      ),
    );
  });
  node.append(
    el("p", { class: "detail chart-legend" }, el("span", {}, "End position − nominal: in-track (x) vs radial (y)")),
    el(
      "div",
      { class: "chart-wrap" },
      scatterChart({ points: ok.map((t) => [t.offsetRswKm[1], t.offsetRswKm[0]]), color: SERIES.sky, xUnit: "km", yUnit: "km", origin: true }),
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

// --- Editor ----------------------------------------------------------------------

function mountEditor() {
  let editing = null;
  const number = (props = {}) => el("input", { class: "coordinate", type: "number", step: "any", ...props });

  const title = el("summary", {}, "Add a Monte Carlo study");
  const name = el("input", { class: "coordinate", type: "text", placeholder: "Name" });
  const spacecraft = el("select", { class: "select" });
  const samples = number({ min: "1", max: String(mc.MAX_SAMPLES), step: "1", placeholder: String(mc.MC_DEFAULTS.samples) });
  const seed = number({ min: "0", step: "1", placeholder: String(mc.MC_DEFAULTS.seed) });
  const guidance = el(
    "select",
    { class: "select" },
    ...Object.entries(mc.GUIDANCE).map(([value, text]) => el("option", { value }, text)),
  );
  const sigmas = Object.entries(mc.DISPERSIONS).map(([key, spec]) => ({ key, spec, input: number({ min: "0", placeholder: "0" }) }));
  const quantities = el("div", { class: "timeline-capture" });
  const error = el("p", { class: "detail error", hidden: true });
  const submit = button("Add", save);
  const cancel = button("Cancel", reset, { hidden: true });

  const choice = (id, text, on) =>
    el("label", { class: "toggle" }, el("input", { type: "checkbox", value: id, checked: on }), el("span", {}, text));
  const setQuantities = (picked) =>
    quantities.replaceChildren(...Object.entries(mc.MC_QUANTITIES).map(([key, { label }]) => choice(key, label, picked.includes(key))));

  function refresh() {
    const ids = [...globe.recordedSources().spacecraft.keys()];
    const current = spacecraft.value;
    spacecraft.replaceChildren(...ids.map((id) => el("option", { value: id }, label(id))));
    spacecraft.value = ids.includes(current) ? current : (ids[0] ?? "");
  }

  function edit(study) {
    reset();
    editing = study.id;
    const s = mc.settings(study);
    name.value = study.label ?? study.id;
    refresh();
    spacecraft.value = s.spacecraft;
    samples.value = study.samples ?? "";
    seed.value = study.seed ?? "";
    guidance.value = s.guidance;
    for (const { key, spec, input } of sigmas) {
      input.value = s.dispersions[key] !== undefined ? Number((s.dispersions[key] / spec.scale).toPrecision(6)) : "";
    }
    setQuantities(s.quantities);
    title.textContent = `Edit ${study.label ?? study.id}`;
    submit.textContent = "Save";
    cancel.hidden = false;
    node.open = true;
    node.scrollIntoView({ block: "nearest" });
  }

  function reset() {
    editing = null;
    name.value = "";
    for (const input of [samples, seed, ...sigmas.map((s) => s.input)]) input.value = "";
    guidance.value = mc.MC_DEFAULTS.guidance;
    setQuantities(mc.MC_DEFAULTS.quantities);
    refresh();
    title.textContent = "Add a Monte Carlo study";
    submit.textContent = "Add";
    cancel.hidden = true;
    error.hidden = true;
  }

  function save() {
    error.hidden = true;
    try {
      const text = name.value.trim() || "Monte Carlo";
      const study = { id: editing ?? mc.uniqueStudyId(text), label: text, spacecraft: spacecraft.value };
      if (samples.value.trim()) study.samples = Number(samples.value);
      if (seed.value.trim()) study.seed = Number(seed.value);
      if (guidance.value !== mc.MC_DEFAULTS.guidance) study.guidance = guidance.value;
      const dispersions = Object.fromEntries(
        sigmas
          .filter(({ input }) => input.value.trim() && Number(input.value) !== 0)
          .map(({ key, spec, input }) => [key, Number((Number(input.value) * spec.scale).toPrecision(12))]),
      );
      if (Object.keys(dispersions).length) study.dispersions = dispersions;
      const picked = [...quantities.querySelectorAll("input:checked")].map((input) => input.value);
      if (!picked.length) throw new Error("Pick at least one quantity.");
      if (picked.join() !== mc.MC_DEFAULTS.quantities.join()) study.quantities = picked;
      const sources = globe.recordedSources();
      // Same rules as a project file.
      parseProject(
        {
          terramenta: PROJECT_VERSION,
          spacecraft: [...sources.spacecraft].map(([id, entry]) => ({ id, propagation: entry.propagation })),
          analysis: { monteCarlo: [study] },
        },
        { warn: () => {} },
      );
      const previous = mc.savedStudies().some((other) => other.id === editing);
      mc.addStudy(study);
      if (!previous) mc.runMonteCarlo(study.id);
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
    row("Spacecraft", spacecraft),
    row("Samples", samples),
    row("Seed", seed),
    row("Guidance", guidance),
    el("h3", { class: "editor-title" }, "1σ dispersions"),
    ...sigmas.map(({ spec, input }) => row(`${spec.label} ${spec.unit}`, input)),
    el("h3", { class: "editor-title" }, "End quantities"),
    quantities,
    error,
    el("div", { class: "buttons" }, submit, cancel),
  );
  reset();
  return { node, edit, refresh };
}
