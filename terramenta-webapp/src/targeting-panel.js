/**
 * Targeting section (Spacecraft tab): targets from `targeting.js`, their last
 * solution, and an editor. Solve runs `targeting.solveTarget` (synchronous,
 * one propagation per variable per iteration); Apply re-propagates the
 * spacecraft with the solved burns. Units: Δv m/s, epochs and durations s.
 */

import { el, row, section } from "./dom.js";
import * as globe from "./globe.js";
import { PROJECT_VERSION, layerDisplay, parseProject } from "./project.js";
import { epochSeconds } from "./reports.js";
import * as targeting from "./targeting.js";
import { button, rowList } from "./widgets.js";

const AXES = { vnb: ["V", "N", "B"], rsw: ["R", "S", "W"], lvlh: ["x", "y", "z"], inertial: ["x", "y", "z"] };
/** Variable defaults as `globe.target`'s, in editor units. */
const DEFAULTS = { dv: { perturbation: 0.01, maxStep: 100 }, s: { perturbation: 1, maxStep: 600 } };

export function mountTargeting(bind, signal) {
  let lastState = null;
  const editor = mountEditor();
  const list = el("div", { class: "layers" });
  let entries = [];

  const render = () => {
    const targets = targeting.savedTargets();
    list.replaceChildren(...targets.map((target) => targetRow(target, editor, () => lastState)));
    list.classList.toggle("empty", !targets.length);
    editor.refresh();
  };
  const unsubscribe = [targeting.subscribe(render), targeting.onResult(render)];
  signal?.addEventListener("abort", () => unsubscribe.forEach((off) => off()));

  bind(list, (state) => {
    lastState = state;
    const next = [...globe.recordedSources().spacecraft.values()];
    if (next.length === entries.length && next.every((entry, index) => entry === entries[index])) return;
    entries = next;
    render();
  });

  return section(
    "Targeting",
    list,
    editor.node,
    el(
      "p",
      { class: "detail" },
      "Newton differential correction, finite-difference Jacobian. Goals are two-body osculating values about Earth.",
    ),
  );
}

// --- Rows ------------------------------------------------------------------------

function targetRow(target, editor, getState) {
  const entry = globe.recordedSources().spacecraft.get(target.spacecraft);
  const solved = targeting.targetResult(target.id);
  const status = el("p", { class: "detail" });
  const solve = button("Solve", () => {
    solve.disabled = true;
    status.textContent = "Solving…";
    status.classList.remove("error");
    // Let the status paint before the synchronous solve.
    setTimeout(() => targeting.solveTarget(target.id), 30);
  });
  const apply = button("Apply", () => {
    const layer = getState()?.ephemerides.layers.find((candidate) => candidate.id === target.spacecraft);
    try {
      targeting.applyTarget(target.id, layer ? layerDisplay(layer) : undefined);
    } catch (error) {
      alert(`Apply failed: ${error.message ?? error}`);
    }
  });
  solve.disabled = !entry?.info;
  apply.disabled = !solved?.result || solved.stale || solved.applied;
  const label = target.label ?? target.id;
  const spacecraft = entry?.display.label ?? target.spacecraft;

  const node = el(
    "div",
    { class: "layer report" },
    el("div", { class: "layer-head" }, el("span", { class: "mission-title", title: target.id }, label)),
    el(
      "p",
      { class: "detail" },
      `${spacecraft} · ${count(target.variables.length, "variable")} · ${count(target.goals.length, "goal")}`,
    ),
    el(
      "div",
      { class: "buttons" },
      solve,
      apply,
      button("Edit", () => editor.edit(target)),
      button("Remove", () => targeting.removeTarget(target.id)),
    ),
    status,
  );

  if (!entry) {
    status.textContent = `No spacecraft "${target.spacecraft}"`;
    status.classList.add("error");
    node.dataset.status = "failed";
    return node;
  }
  if (!solved) {
    status.textContent = entry.info ? "Not solved" : "Spacecraft not propagated";
    return node;
  }
  if (solved.error) {
    status.textContent = solved.error;
    status.classList.add("error");
    node.dataset.status = "failed";
    return node;
  }
  const { result } = solved;
  const worst = result.history.at(-1);
  status.textContent = [
    result.converged
      ? `Converged in ${count(result.iterations, "iteration")}`
      : `Not converged: ${count(result.iterations, "iteration")}, worst ${formatRatio(worst)}× tolerance`,
    solved.applied ? "applied" : solved.stale ? "spacecraft changed since" : null,
  ]
    .filter(Boolean)
    .join(" · ");
  status.classList.toggle("error", !result.converged);
  node.dataset.status = result.converged ? "ready" : "failed";

  const initial = solved.applied ? null : entry.propagation;
  node.append(
    table(
      ["Variable", "Initial", "Solved"],
      target.variables.map((variable, index) => [
        variableLabel(variable, entry.propagation),
        initial ? formatVariable(variable, targeting.variableValue(variable, initial)) : "—",
        formatVariable(variable, result.values[index]),
      ]),
    ),
    table(
      ["Goal", "Desired", "Achieved", "Error"],
      target.goals.map((goal, index) => {
        const { achieved, error } = result.goals[index];
        const spec = targeting.GOAL_QUANTITIES[goal.quantity];
        const miss = !(Math.abs(error) <= goal.tolerance);
        return {
          miss,
          cells: [
            `${spec.label} · ${targeting.atName(goal.at)}`,
            formatQuantity(goal.value, spec),
            formatQuantity(achieved, spec),
            `${error.toPrecision(2)}${spec.unit ? ` ${spec.unit}` : ""}`,
          ],
        };
      }),
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
      el(
        "tbody",
        {},
        ...rows.map((entry) => {
          const { cells, miss } = Array.isArray(entry) ? { cells: entry } : entry;
          return el("tr", miss ? { class: "miss" } : {}, ...cells.map((text) => el("td", {}, text)));
        }),
      ),
    ),
  );
}

const count = (n, noun) => `${n} ${noun}${n === 1 ? "" : "s"}`;
const formatRatio = (ratio) => (ratio >= 100 ? ratio.toExponential(1) : ratio.toFixed(1));

function formatQuantity(value, spec) {
  // No "-0.000".
  const text = Number.isFinite(value) ? value.toFixed(spec.digits).replace(/^-(0\.?0*)$/, "$1") : "—";
  return `${text}${spec.unit ? ` ${spec.unit}` : ""}`;
}

/** `"Burn 2 Δv V"`, `"Burn 2 epoch"`, `"Finite 1 duration"`. */
function variableLabel(variable, propagation) {
  if (variable.burn === undefined) return `Finite ${variable.finite} ${variable.parameter}`;
  if (variable.parameter === "epoch") return `Burn ${variable.burn} epoch`;
  const frame = propagation.maneuvers?.[variable.burn - 1]?.frame ?? "vnb";
  return `Burn ${variable.burn} Δv ${AXES[frame][Number(variable.parameter[2]) - 1]}`;
}

function formatVariable(variable, value) {
  if (!Number.isFinite(value)) return "—";
  if (variable.parameter.startsWith("dv")) return `${(value * 1000).toFixed(4)} m/s`;
  if (variable.parameter === "duration") return `${value.toFixed(3)} s`;
  return `${value >= 0 ? "+" : "−"}${Math.abs(value).toFixed(3)} s`;
}

// --- Editor ----------------------------------------------------------------------

function mountEditor() {
  let editing = null;
  const text = (props = {}) => el("input", { class: "coordinate", spellcheck: false, ...props });
  const number = (props = {}) => el("input", { class: "coordinate", type: "number", step: "any", ...props });
  const select = (options, value) => {
    const node = el("select", { class: "select" });
    setOptions(node, options, value);
    return node;
  };

  const title = el("summary", {}, "Define a target");
  const name = text({ placeholder: "Name" });
  const spacecraft = select([], "");
  const maxIterations = number({ min: "1", max: "200", step: "1", placeholder: "25" });
  const variables = rowList(variableRow, "Add variable");
  const goals = rowList(goalRow, "Add goal");
  const error = el("p", { class: "detail error", hidden: true });
  const submit = button("Add", save);
  const cancel = button("Cancel", reset, { hidden: true });

  const propagation = () => globe.recordedSources().spacecraft.get(spacecraft.value)?.propagation ?? {};
  const refreshRows = () => {
    for (const entry of [...variables.entries(), ...goals.entries()]) entry.refresh(propagation());
  };
  spacecraft.onchange = refreshRows;

  /** Spacecraft choices follow the scene. */
  function refresh() {
    const ids = [...globe.recordedSources().spacecraft].map(([id, entry]) => [id, entry.display.label ?? id]);
    setOptions(spacecraft, ids, spacecraft.value || ids[0]?.[0]);
    refreshRows();
  }

  function edit(target) {
    reset();
    editing = target.id;
    name.value = target.label ?? target.id;
    refresh();
    setOptions(spacecraft, [...spacecraft.options].map((option) => [option.value, option.text]), target.spacecraft);
    if (spacecraft.value !== target.spacecraft) {
      spacecraft.append(el("option", { value: target.spacecraft }, target.spacecraft));
      spacecraft.value = target.spacecraft;
    }
    maxIterations.value = target.maxIterations ?? "";
    for (const variable of target.variables) variables.add(variable);
    for (const goal of target.goals) goals.add(goal);
    title.textContent = `Edit ${target.label ?? target.id}`;
    submit.textContent = "Save";
    cancel.hidden = false;
    node.open = true;
    node.scrollIntoView({ block: "nearest" });
  }

  function reset() {
    editing = null;
    name.value = "";
    maxIterations.value = "";
    variables.clear();
    goals.clear();
    title.textContent = "Define a target";
    submit.textContent = "Add";
    cancel.hidden = true;
    error.hidden = true;
  }

  function save() {
    error.hidden = true;
    try {
      const label = name.value.trim();
      if (!label) throw new Error("Give the target a name.");
      const entry = globe.recordedSources().spacecraft.get(spacecraft.value);
      if (!entry) throw new Error("Pick a spacecraft.");
      const target = {
        id: editing ?? targeting.uniqueTargetId(label),
        label,
        spacecraft: spacecraft.value,
        variables: variables.values(),
        goals: goals.values(),
      };
      if (maxIterations.value.trim()) target.maxIterations = Number(maxIterations.value);
      // Same rules as a project file.
      parseProject(
        {
          terramenta: PROJECT_VERSION,
          spacecraft: [{ id: target.spacecraft, propagation: entry.propagation }],
          targets: [target],
        },
        { warn: () => {} },
      );
      targeting.addTarget(target);
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
    row("Craft", spacecraft),
    row("Iterations", maxIterations),
    el("h3", { class: "editor-title" }, "Vary"),
    variables.node,
    el("h3", { class: "editor-title" }, "Achieve"),
    goals.node,
    error,
    el("div", { class: "buttons" }, submit, cancel),
  );
  return { node, edit, refresh };

  function variableRow(variable = {}) {
    const which = select([], "");
    const parameter = select([], "");
    const perturbation = number();
    const maxStep = number();
    const isDv = () => parameter.value.startsWith("dv");
    const units = () => {
      const defaults = isDv() ? DEFAULTS.dv : DEFAULTS.s;
      const unit = isDv() ? "m/s" : "s";
      perturbation.placeholder = `${defaults.perturbation} ${unit}`;
      maxStep.placeholder = `${defaults.maxStep} ${unit}`;
    };
    const scale = () => (isDv() ? 1000 : 1);
    const refresh = (p) => {
      const current = which.value || (variable.burn ? `burn:${variable.burn}` : variable.finite ? `finite:${variable.finite}` : "");
      setOptions(which, burnChoices(p), current);
      const [kind, n] = which.value.split(":");
      const axes = AXES[p.maneuvers?.[n - 1]?.frame ?? "vnb"];
      const choices =
        kind === "finite"
          ? [
              ["start", "Start (offset)"],
              ["duration", "Duration"],
            ]
          : [...axes.map((axis, index) => [`dv${index + 1}`, `Δv ${axis}`]), ["epoch", "Epoch (offset)"]];
      setOptions(parameter, choices, parameter.value || variable.parameter);
      units();
    };
    which.onchange = () => refresh(propagation());
    parameter.onchange = units;
    refresh(propagation());
    if (variable.perturbation !== undefined) perturbation.value = variable.perturbation * scale();
    if (variable.maxStep !== undefined) maxStep.value = variable.maxStep * scale();
    return {
      fields: [
        row("Burn", which),
        row("Parameter", parameter),
        row("Perturb", perturbation),
        row("Max step", maxStep),
      ],
      refresh,
      value: () => {
        const [kind, n] = which.value.split(":");
        const out = { [kind || "burn"]: Number(n), parameter: parameter.value };
        if (perturbation.value.trim()) out.perturbation = Number(perturbation.value) / scale();
        if (maxStep.value.trim()) out.maxStep = Number(maxStep.value) / scale();
        return out;
      },
    };
  }

  function goalRow(goal = {}) {
    const quantity = select(
      Object.entries(targeting.GOAL_QUANTITIES).map(([key, spec]) => [key, `${spec.label}${spec.unit ? `, ${spec.unit}` : ""}`]),
      goal.quantity ?? "smaKm",
    );
    const at = select([], "");
    const epoch = text({ placeholder: "UTC epoch", hidden: true });
    const value = number({ value: goal.value ?? "" });
    const tolerance = number({ value: goal.tolerance ?? "", placeholder: "> 0" });
    const custom = goal.at && !/^(end|burn|finite)/.test(goal.at);
    if (custom) epoch.value = goal.at;
    const showEpoch = () => (epoch.hidden = at.value !== "epoch");
    const refresh = (p) => {
      setOptions(at, atChoices(p), at.value || (custom ? "epoch" : (goal.at ?? "end")));
      showEpoch();
    };
    at.onchange = showEpoch;
    refresh(propagation());
    return {
      fields: [row("Quantity", quantity), row("At", at), epoch, row("Value", value), row("Tolerance", tolerance)],
      refresh,
      value: () => ({
        quantity: quantity.value,
        at: at.value === "epoch" ? epoch.value.trim() : at.value,
        value: value.value.trim() === "" ? Number.NaN : Number(value.value),
        tolerance: tolerance.value.trim() === "" ? Number.NaN : Number(tolerance.value),
      }),
    };
  }
}

/** `[[value, label]]` for every burn of `propagation`. */
function burnChoices(propagation) {
  return [
    ...(propagation.maneuvers ?? []).map((maneuver, index) => [`burn:${index + 1}`, `Burn ${index + 1} · ${when(maneuver.epoch)}`]),
    ...(propagation.finiteBurns ?? []).map((burn, index) => [`finite:${index + 1}`, `Finite ${index + 1} · ${when(burn.start)}`]),
  ];
}

function atChoices(propagation) {
  return [
    ["end", "End"],
    ...(propagation.maneuvers ?? []).flatMap((_, index) => [
      [`burn${index + 1}.before`, `Before burn ${index + 1}`],
      [`burn${index + 1}`, `After burn ${index + 1}`],
    ]),
    ...(propagation.finiteBurns ?? []).flatMap((_, index) => [
      [`finite${index + 1}`, `Start of finite ${index + 1}`],
      [`finite${index + 1}.end`, `End of finite ${index + 1}`],
    ]),
    ["epoch", "UTC epoch…"],
  ];
}

/** `MM-DD hh:mm:ss`. */
function when(epoch) {
  const seconds = epochSeconds(epoch);
  return Number.isFinite(seconds) ? new Date(seconds * 1000).toISOString().slice(5, 19).replace("T", " ") : epoch;
}

/** Replaces `node`'s options, keeping `value` when it is still one of them. */
function setOptions(node, options, value) {
  node.replaceChildren(...options.map(([key, label]) => el("option", { value: key }, label)));
  if (options.some(([key]) => key === value)) node.value = value;
}
