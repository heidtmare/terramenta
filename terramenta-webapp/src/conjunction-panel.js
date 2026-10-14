/**
 * Conjunctions section (Analysis tab): studies from `conjunction.js` as cards
 * with a ranked table (row: clock to TCA, catalogue object pinned; CDM per
 * row), CSV export and an editor.
 */

import * as conjunction from "./conjunction.js";
import { download, el, row, section } from "./dom.js";
import * as globe from "./globe.js";
import { PROJECT_VERSION, parseProject } from "./project.js";
import { isoUtc, toCsv } from "./reports.js";
import { button } from "./widgets.js";

/** Rows shown per card; CSV has all. */
const SHOWN = 20;

export function mountConjunctions(bind, signal) {
  const editor = mountEditor();
  const list = el("div", { class: "layers" });
  let entries = [];
  let layerIds = [];

  const render = () => {
    const studies = conjunction.savedConjunctions();
    list.replaceChildren(...studies.map((study) => studyCard(study, editor)));
    list.classList.toggle("empty", !studies.length);
    editor.refresh();
  };
  const unsubscribe = [conjunction.subscribe(render), conjunction.onResult(render)];
  signal?.addEventListener("abort", () => unsubscribe.forEach((off) => off()));

  bind(list, () => {
    const sources = globe.recordedSources();
    const next = [...sources.spacecraft.values()];
    const layers = catalogueLayers();
    if (
      next.length !== entries.length ||
      next.some((entry, index) => entry !== entries[index]) ||
      layers.join() !== layerIds.join()
    ) {
      entries = next;
      layerIds = layers;
      render();
    }
    node.hidden = entries.length < 1 && !conjunction.savedConjunctions().length;
  });

  const node = section(
    "Conjunctions",
    list,
    editor.node,
    el(
      "p",
      { class: "detail" },
      "Closest approaches within the threshold, refined from a step grid. Pc: Foster 2D, short encounter, combined covariance in the encounter plane; Pc max: largest over the covariance's scale. Catalogue objects outside the primary's radius band are filtered.",
    ),
  );
  return node;
}

/** Ephemeris layers that are not spacecraft. */
function catalogueLayers() {
  const sources = globe.recordedSources();
  return [...sources.ephemerides.keys()].filter((id) => !sources.spacecraft.has(id));
}

const label = (id) => globe.recordedSources().spacecraft.get(id)?.display.label ?? globe.recordedSources().ephemerides.get(id)?.label ?? id;

// --- Cards -----------------------------------------------------------------------

function studyCard(study, editor) {
  const s = conjunction.settings(study);
  const kept = conjunction.conjunctionResult(study.id);
  const against = [...s.secondaries, ...s.layers].map(label);
  const status = el("p", { class: "detail" });
  const run = button(kept ? "Re-run" : "Run", () => conjunction.runConjunction(study.id));
  const csv = button("CSV", () => exportCsv(study, kept.result));
  csv.disabled = !kept?.result?.conjunctions.length;
  const sigma = s.secondarySigmaKm ? ` · σ ${s.secondarySigmaKm.map((km) => km * 1e3).join("/")} m` : "";
  const node = el(
    "div",
    { class: "layer report" },
    el("div", { class: "layer-head" }, el("span", { class: "mission-title", title: study.id }, study.label ?? study.id)),
    el(
      "p",
      { class: "detail", title: against.join(", ") },
      `${label(s.primary)} vs ${against.length > 2 ? `${against.length} sources` : against.join(", ")} · ≤ ${s.thresholdKm} km · R ${s.hardBodyRadiusM} m · ${s.stepS} s${sigma}`,
    ),
    el(
      "div",
      { class: "buttons" },
      run,
      csv,
      button("Edit", () => editor.edit(study)),
      button("Remove", () => conjunction.removeConjunction(study.id)),
    ),
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
  status.textContent = [
    kept.stale ? "Stale: inputs changed" : null,
    kept.missing.length ? `Not loaded: ${kept.missing.map(label).join(", ")}` : null,
    `${result.conjunctions.length} found · ${result.screened} screened${result.filtered ? `, ${result.filtered} filtered` : ""} · ${(kept.ms / 1000).toFixed(2)} s`,
  ]
    .filter(Boolean)
    .join(" · ");
  if (kept.stale || kept.missing.length) status.classList.add("error");
  if (!result.conjunctions.length) return node;
  const ranked = conjunction.ranked(result);
  node.append(
    el(
      "div",
      { class: "table-wrap events-table" },
      el(
        "table",
        { class: "report-table" },
        el("thead", {}, el("tr", {}, ...["TCA (UTC)", "Object", "Miss", "Pc", "Pc max", ""].map((text) => el("th", {}, text)))),
        el("tbody", {}, ...ranked.slice(0, SHOWN).map((c) => conjunctionRow(study, c))),
      ),
    ),
  );
  if (ranked.length > SHOWN) node.append(el("p", { class: "detail" }, `${ranked.length - SHOWN} more in the CSV`));
  return node;
}

function conjunctionRow(study, c) {
  const cdm = button("CDM", (event) => {
    event.stopPropagation();
    download(`${study.id}-${c.noradId ?? c.secondary}-${isoUtc(c.tca).replace(/[:.]/g, "")}.cdm`, new Blob([c.cdm], { type: "text/plain" }));
  });
  cdm.disabled = !c.cdm;
  const go = () => {
    globe.setClock(c.tca);
    if (c.layer) globe.pinSatellite(c.layer, c.noradId);
  };
  return el(
    "tr",
    { class: "pick", title: "Set the clock to TCA", onclick: go },
    el("td", { title: isoUtc(c.tca) }, isoUtc(c.tca).slice(5, 19).replace("T", " ")),
    el("td", { title: c.noradId ? `${c.name} · ${c.noradId}` : c.name }, c.layer ? c.name : label(c.secondary)),
    el("td", { title: `${c.relativeSpeedKmS.toFixed(2)} km/s relative` }, `${Math.round(c.missKm * 1e3).toLocaleString()} m`),
    el("td", { title: c.sigmaKm ? `σ ${c.sigmaKm.map((km) => Math.round(km * 1e3)).join(" × ")} m` : "No covariance" }, conjunction.formatPc(c.pc)),
    el("td", {}, conjunction.formatPc(c.pcMax)),
    el("td", {}, cdm),
  );
}

function exportCsv(study, result) {
  download(`${study.id}-conjunctions.csv`, new Blob([toCsv(conjunction.CONJUNCTION_COLUMNS, conjunction.conjunctionRows(result))], { type: "text/csv" }));
}

// --- Editor ----------------------------------------------------------------------

function mountEditor() {
  let editing = null;
  const number = (props = {}) => el("input", { class: "coordinate", type: "number", step: "any", ...props });
  const D = conjunction.CONJUNCTION_DEFAULTS;

  const title = el("summary", {}, "Add a conjunction study");
  const name = el("input", { class: "coordinate", type: "text", placeholder: "Name" });
  const primary = el("select", { class: "select" });
  const secondaries = el("div", { class: "timeline-capture" });
  const layers = el("div", { class: "timeline-capture" });
  const threshold = number({ min: "0", placeholder: String(D.thresholdKm) });
  const radius = number({ min: "0", placeholder: String(D.hardBodyRadiusM) });
  const step = number({ min: "0", placeholder: String(D.stepS) });
  const sigma = ["Radial", "In-track", "Cross"].map((placeholder) => number({ min: "0", placeholder }));
  const error = el("p", { class: "detail error", hidden: true });
  const submit = button("Add", save);
  const cancel = button("Cancel", reset, { hidden: true });
  // A new primary with nothing left checked screens against every other spacecraft.
  primary.onchange = () => {
    const kept = checked(secondaries).filter((id) => id !== primary.value);
    refreshChoices({ secondaries: kept.length ? kept : [...globe.recordedSources().spacecraft.keys()], layers: checked(layers) });
  };

  const checked = (box) => [...box.querySelectorAll("input:checked")].map((input) => input.value);
  const choice = (id, on) =>
    el("label", { class: "toggle" }, el("input", { type: "checkbox", value: id, checked: on }), el("span", {}, label(id)));

  function refresh() {
    const ids = [...globe.recordedSources().spacecraft.keys()];
    const current = primary.value;
    primary.replaceChildren(...ids.map((id) => el("option", { value: id }, label(id))));
    primary.value = ids.includes(current) ? current : (ids[0] ?? "");
    refreshChoices();
  }

  function refreshChoices(selected = { secondaries: checked(secondaries), layers: checked(layers) }) {
    secondaries.replaceChildren(
      ...[...globe.recordedSources().spacecraft.keys()]
        .filter((id) => id !== primary.value)
        .map((id) => choice(id, selected.secondaries.includes(id))),
    );
    layers.replaceChildren(...catalogueLayers().map((id) => choice(id, selected.layers.includes(id))));
  }

  function edit(study) {
    reset();
    editing = study.id;
    const s = conjunction.settings(study);
    name.value = study.label ?? study.id;
    refresh();
    primary.value = s.primary;
    refreshChoices({ secondaries: s.secondaries, layers: s.layers });
    threshold.value = study.thresholdKm ?? "";
    radius.value = study.hardBodyRadiusM ?? "";
    step.value = study.stepS ?? "";
    sigma.forEach((input, index) => (input.value = study.secondarySigmaKm ? study.secondarySigmaKm[index] * 1e3 : ""));
    title.textContent = `Edit ${study.label ?? study.id}`;
    submit.textContent = "Save";
    cancel.hidden = false;
    node.open = true;
    node.scrollIntoView({ block: "nearest" });
  }

  function reset() {
    editing = null;
    name.value = "";
    for (const input of [threshold, radius, step, ...sigma]) input.value = "";
    refresh();
    refreshChoices({ secondaries: [...globe.recordedSources().spacecraft.keys()], layers: [] });
    title.textContent = "Add a conjunction study";
    submit.textContent = "Add";
    cancel.hidden = true;
    error.hidden = true;
  }

  function save() {
    error.hidden = true;
    try {
      const text = name.value.trim() || "Conjunctions";
      const study = { id: editing ?? conjunction.uniqueConjunctionId(text), label: text, primary: primary.value };
      const others = checked(secondaries);
      const catalogues = checked(layers);
      if (others.length) study.secondaries = others;
      if (catalogues.length) study.layers = catalogues;
      if (threshold.value.trim()) study.thresholdKm = Number(threshold.value);
      if (radius.value.trim()) study.hardBodyRadiusM = Number(radius.value);
      if (step.value.trim()) study.stepS = Number(step.value);
      const sigmas = sigma.map((input) => input.value.trim());
      if (sigmas.some(Boolean)) {
        if (!sigmas.every(Boolean)) throw new Error("Secondary σ: give all three, or none.");
        study.secondarySigmaKm = sigmas.map((m) => Number((Number(m) / 1e3).toPrecision(12)));
      }
      const sources = globe.recordedSources();
      // Same rules as a project file.
      parseProject(
        {
          terramenta: PROJECT_VERSION,
          ephemerides: { layers: catalogueLayers().map((id) => ({ id, url: sources.ephemerides.get(id)?.url ?? "inline" })) },
          spacecraft: [...sources.spacecraft].map(([id, entry]) => ({ id, propagation: entry.propagation })),
          analysis: { conjunctions: [study] },
        },
        { warn: () => {} },
      );
      const previous = conjunction.savedConjunctions().some((other) => other.id === editing);
      conjunction.addConjunction(study);
      if (!previous) conjunction.runConjunction(study.id);
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
    row("Primary", primary),
    el("h3", { class: "editor-title" }, "Spacecraft"),
    secondaries,
    el("h3", { class: "editor-title" }, "Catalogue layers"),
    layers,
    row("Threshold km", threshold),
    row("Radius m", radius),
    row("Step s", step),
    row("Catalogue σ m", el("span", { class: "pair" }, ...sigma)),
    error,
    el("div", { class: "buttons" }, submit, cancel),
  );
  reset();
  return { node, edit, refresh };
}
