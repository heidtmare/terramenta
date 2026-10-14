/**
 * Spacecraft tab: propagation environment, numerically propagated spacecraft,
 * and the editor that defines them.
 *
 * A spacecraft is a `propagate` request plus ephemeris display options
 * (`globe.addSpacecraft`), saved under the project's `spacecraft`. The editor
 * builds the request, validates it as a project entry (`parseProject`), and
 * propagates; editing an existing one re-propagates under its id. Rows are
 * rebuilt from `globe.recordedSources().spacecraft`, so failed ones show too.
 * Units in the editor: km, km/s, m/s for Δv, s, kg, N.
 */

import * as environment from "./environment.js";
import * as globe from "./globe.js";
import { download, el, row, section } from "./dom.js";
import { clock, utcSeconds } from "./format.js";
import { PROJECT_VERSION, layerDisplay, parseProject } from "./project.js";
import { button, toggle } from "./widgets.js";

const PALETTE = ["#ffb86b", "#ff8fa3", "#c3ff7a", "#ffe66b", "#7affea"];
const nextColor = () => PALETTE[globe.recordedSources().spacecraft.size % PALETTE.length];

const MU_EARTH = 398_600.4418;
const EARTH_RADIUS_KM = 6_378.137;
const MJD_UNIX_EPOCH = 40_587;

const FRAMES = [
  ["vnb", "VNB"],
  ["rsw", "RSW"],
  ["lvlh", "LVLH"],
  ["inertial", "GCRF"],
];
const DRAG = [
  ["none", "None"],
  ["exponential", "Exponential"],
  ["harrisPriester", "Harris-Priester"],
  ["nrlmsise00", "NRLMSISE-00 (space weather)"],
];
const INTEGRATORS = [
  ["dop853", "DOP853"],
  ["rkf78", "RKF7(8)"],
  ["rk4", "RK4 (fixed step)"],
];
const INITIAL = [
  ["state", "GCRF state"],
  ["tle", "TLE"],
  ["omm", "OMM JSON"],
];

/** `bind(node, sync)`: the panel's registration. `signal` aborts on close. */
export function mountSpacecraft(bind, signal) {
  return [mountSpacecraftList(bind), mountEnvironment(signal)];
}

// --- Environment -----------------------------------------------------------------

function mountEnvironment(signal) {
  const rows = Object.entries(environment.KINDS).map(([kind, spec]) => {
    const url = el("input", { class: "coordinate", type: "url", spellcheck: false, value: spec.url, placeholder: "URL" });
    const status = el("p", { class: "detail" }, "—");
    const error = el("p", { class: "detail error", hidden: true });
    const run = (loading) => {
      error.hidden = true;
      loading.catch((caught) => {
        error.textContent = caught.message ?? String(caught);
        error.hidden = false;
      });
    };
    const file = el("input", {
      type: "file",
      accept: spec.accept,
      hidden: true,
      onchange: (event) => {
        const [picked] = event.target.files ?? [];
        event.target.value = "";
        if (picked) run(environment.loadFile(kind, picked));
      },
    });
    const node = el(
      "div",
      { class: "layer" },
      row(spec.label, url),
      el(
        "div",
        { class: "buttons" },
        button("Load URL", () => url.value.trim() && run(environment.loadUrl(kind, url.value.trim()))),
        button("Local file…", () => file.click()),
      ),
      file,
      status,
      error,
      el("p", { class: "detail" }, spec.note),
    );
    return { kind, node, status };
  });

  const render = () => {
    const { sources, coverage } = environment.status();
    for (const { kind, node, status } of rows) {
      const source = sources[kind];
      const covered = describeCoverage(kind, coverage);
      node.dataset.status = source?.error ? "failed" : source?.loading ? "loading" : covered ? "ready" : "";
      const from = source ? ` · ${source.url ?? source.file}` : "";
      status.textContent = source?.loading ? `Loading${from}` : `${covered ?? "Not loaded"}${from}`;
    }
  };
  render();
  const unsubscribe = environment.subscribe(render);
  signal?.addEventListener("abort", unsubscribe);

  return section(
    { title: "Propagation environment", open: false },
    ...rows.map((entry) => entry.node),
    el(
      "p",
      { class: "detail" },
      "Kept across projects. URL sources save with the project; local files do not.",
    ),
  );
}

function describeCoverage(kind, coverage) {
  if (!coverage) return null;
  if (kind === "kernel") return coverage.kernel ? "Loaded" : null;
  const span = coverage[kind];
  return span ? `${mjdDate(span[0])} → ${mjdDate(span[1])}` : null;
}

function mjdDate(mjd) {
  return new Date((mjd - MJD_UNIX_EPOCH) * 86_400_000).toISOString().slice(0, 10);
}

// --- Spacecraft list ---------------------------------------------------------------

function mountSpacecraftList(bind) {
  let lastState = null;
  const editor = mountEditor(() => lastState);
  const list = el("div", { class: "layers" });
  const rows = new Map();

  bind(list, (state) => {
    lastState = state;
    const recorded = globe.recordedSources().spacecraft;
    for (const [id, entry] of rows) {
      if (!recorded.has(id)) {
        entry.node.remove();
        rows.delete(id);
      }
    }
    for (const [id, spacecraft] of recorded) {
      let entry = rows.get(id);
      if (!entry) {
        entry = spacecraftRow(id, editor);
        rows.set(id, entry);
        list.append(entry.node);
      }
      entry.sync(spacecraft, state.ephemerides.layers.find((layer) => layer.id === id), state);
    }
    list.classList.toggle("empty", recorded.size === 0);
  });

  return section("Spacecraft", list, editor.node);
}

function spacecraftRow(id, editor) {
  let layer = null;
  let spacecraft = null;
  const visible = toggle(id, (value) => globe.setEphemerisVisible(id, value));
  const color = el("input", {
    class: "swatch",
    type: "color",
    title: "Colour",
    oninput: (event) =>
      globe.setEphemerisStyle(id, { pointColor: event.target.value, lineColor: `${event.target.value}a6` }),
  });
  const trails = toggle("Orbit trails", (value) => globe.setEphemerisTrails(id, value));
  const detail = el("p", { class: "detail" }, "—");
  const error = el("p", { class: "detail error", hidden: true });

  const node = el(
    "div",
    { class: "layer" },
    el("div", { class: "layer-head" }, visible.node, color),
    detail,
    error,
    trails.node,
    el(
      "div",
      { class: "buttons" },
      button("Edit", () => editor.edit(id, spacecraft, layer)),
      button("Go to start", () => spacecraft.info && globe.setClock(utcSeconds(spacecraft.info.start))),
      button("OEM", () => exportOem(id, spacecraft.propagation)),
      button("SPK", () => exportSpk(id, spacecraft.propagation)),
      button("Remove", () => globe.removeSpacecraft(id)),
    ),
  );

  return {
    node,
    sync(nextSpacecraft, nextLayer, state) {
      spacecraft = nextSpacecraft;
      layer = nextLayer ?? null;
      const label = layer?.label ?? spacecraft.display.label ?? id;
      visible.node.querySelector("span").textContent = label;
      visible.set(layer?.visible ?? false);
      visible.input.disabled = !layer;
      trails.set(layer?.trails ?? false);
      trails.input.disabled = !layer;
      if (layer && document.activeElement !== color) color.value = layer.style.pointColor.slice(0, 7).toLowerCase();

      error.hidden = !spacecraft.error;
      error.textContent = spacecraft.error ?? "";
      node.dataset.status = spacecraft.error ? "failed" : layer ? "ready" : "loading";
      detail.textContent = spacecraft.info ? describeInfo(spacecraft, state) : "Not propagated";
    },
  };
}

function describeInfo({ info, propagation }, state) {
  const start = utcSeconds(info.start);
  const stop = utcSeconds(info.stop);
  const now = state.sun.unixSeconds;
  const where = now < start ? "clock before span" : now > stop ? "clock after span" : "in span";
  const burns = (propagation.maneuvers?.length ?? 0) + (propagation.finiteBurns?.length ?? 0);
  return (
    `${clock(start)} → ${clock(stop)} · ${info.samples} samples · ${info.finalMassKg.toFixed(1)} kg` +
    `${burns ? ` · ${burns} burn${burns > 1 ? "s" : ""}` : ""} · ${where}`
  );
}

function exportOem(id, propagation) {
  try {
    const { oem } = globe.propagate({ ...propagation, kvn: true });
    const created = new Date().toISOString().replace("Z", "");
    download(`${id}.oem`, new Blob([oem.replace(/^CREATION_DATE = *$/m, `CREATION_DATE = ${created}`)], { type: "text/plain" }));
  } catch (error) {
    alert(`OEM export failed: ${error.message ?? error}`);
  }
}

function exportSpk(id, propagation) {
  try {
    download(`${id}.bsp`, new Blob([globe.propagateSpk(propagation)], { type: "application/octet-stream" }));
  } catch (error) {
    alert(`SPK export failed: ${error.message ?? error}`);
  }
}

// --- Editor ------------------------------------------------------------------------

function mountEditor(getState) {
  /** Id being edited; `null` for a new spacecraft. */
  let editing = null;
  let editingDisplay = null;

  const text = (props = {}) => el("input", { class: "coordinate", spellcheck: false, ...props });
  const number = (props = {}) => el("input", { class: "coordinate", type: "number", step: "any", ...props });
  const select = (options, value) => {
    const node = el("select", { class: "select" }, ...options.map(([key, label]) => el("option", { value: key }, label)));
    node.value = value;
    return node;
  };
  const vector = (placeholder) => {
    const inputs = [0, 1, 2].map((index) => number({ placeholder: placeholder[index] }));
    return { node: el("div", { class: "vector" }, ...inputs), inputs };
  };

  const title = el("h3", { class: "editor-title" }, "New spacecraft");
  const name = text({ value: "Spacecraft" });

  const initialKind = select(INITIAL, "state");
  const epoch = text({ placeholder: "2026-01-01T00:00:00Z" });
  const position = vector(["x km", "y km", "z km"]);
  const velocity = vector(["vx km/s", "vy km/s", "vz km/s"]);
  const circular = number({ value: "500", title: "Altitude, km" });
  const inclination = number({ value: "51.6", title: "Inclination, °" });
  const tle = el("textarea", { class: "coordinate", rows: 3, spellcheck: false, placeholder: "Two-line element set" });
  const omm = el("textarea", { class: "coordinate", rows: 4, spellcheck: false, placeholder: "OMM JSON object" });

  const stateFields = el(
    "div",
    {},
    row("Epoch", epoch),
    row("Position", position.node),
    row("Velocity", velocity.node),
    el(
      "div",
      { class: "buttons" },
      el("span", { class: "row-label" }, "Circular"),
      circular,
      el("span", { class: "row-label" }, "km at"),
      inclination,
      el("span", { class: "row-label" }, "°"),
      button("Fill", () => fillCircular()),
    ),
  );
  const tleFields = el("div", {}, tle);
  const ommFields = el("div", {}, omm);
  const showInitial = () => {
    stateFields.hidden = initialKind.value !== "state";
    tleFields.hidden = initialKind.value !== "tle";
    ommFields.hidden = initialKind.value !== "omm";
  };
  initialKind.onchange = showInitial;

  const start = text({ placeholder: "Default: initial epoch" });
  const durationH = number({ value: "6", min: "0" });
  const stepS = number({ value: "60", min: "0" });

  const massKg = number({ value: "1000", min: "0" });
  const dryMassKg = number({ placeholder: "default" });
  const dragAreaM2 = number({ placeholder: "default" });
  const dragCoefficient = number({ placeholder: "default" });
  const srpAreaM2 = number({ placeholder: "default" });
  const reflectivity = number({ placeholder: "default" });

  const degree = number({ value: "8", min: "0", max: "20", step: "1" });
  const order = number({ value: "8", min: "0", max: "20", step: "1" });
  const drag = select(DRAG, "exponential");
  const thirdBody = toggle("Sun/Moon third body", () => {});
  const srp = toggle("Solar radiation pressure", () => {});
  const relativity = toggle("Relativity", () => {});
  const integrator = select(INTEGRATORS, "dop853");
  const fixedStepS = number({ placeholder: "RK4 step, s" });

  const maneuvers = rowList(maneuverRow, "Add impulsive burn");
  const burns = rowList(finiteBurnRow, "Add finite burn");

  const error = el("p", { class: "detail error", hidden: true });

  function fillCircular() {
    const radius = EARTH_RADIUS_KM + Number(circular.value || 0);
    const speed = Math.sqrt(MU_EARTH / radius);
    const inc = (Number(inclination.value || 0) * Math.PI) / 180;
    const values = [radius, 0, 0, 0, speed * Math.cos(inc), speed * Math.sin(inc)];
    [...position.inputs, ...velocity.inputs].forEach((input, index) => {
      input.value = String(Number(values[index].toFixed(6)));
    });
    if (!epoch.value) epoch.value = clockIso();
  }

  const clockIso = () => {
    const state = getState();
    return new Date((state ? state.sun.unixSeconds : Date.now() / 1000) * 1000).toISOString().replace(/\.\d+Z$/, "Z");
  };

  function reset() {
    editing = null;
    editingDisplay = null;
    title.textContent = "New spacecraft";
    name.value = `Spacecraft ${globe.recordedSources().spacecraft.size + 1}`;
    initialKind.value = "state";
    epoch.value = "";
    tle.value = "";
    omm.value = "";
    start.value = "";
    durationH.value = "6";
    stepS.value = "60";
    massKg.value = "1000";
    for (const input of [dryMassKg, dragAreaM2, dragCoefficient, srpAreaM2, reflectivity, fixedStepS]) input.value = "";
    degree.value = "8";
    order.value = "8";
    drag.value = "exponential";
    thirdBody.set(true);
    srp.set(true);
    relativity.set(false);
    integrator.value = "dop853";
    maneuvers.clear();
    burns.clear();
    error.hidden = true;
    fillCircular();
    showInitial();
  }

  function edit(id, spacecraft, layer) {
    reset();
    editing = id;
    editingDisplay = layer ? layerDisplay(layer) : spacecraft.display;
    title.textContent = `Edit ${id}`;
    const p = spacecraft.propagation;
    name.value = editingDisplay.label ?? p.name ?? id;
    const initial = p.initial;
    if (initial.tle) {
      initialKind.value = "tle";
      tle.value = initial.tle.join("\n");
    } else if (initial.omm) {
      initialKind.value = "omm";
      omm.value = JSON.stringify(initial.omm, null, 2);
    } else {
      initialKind.value = "state";
      epoch.value = initial.epoch;
      initial.positionKm.forEach((value, index) => (position.inputs[index].value = String(value)));
      initial.velocityKmS.forEach((value, index) => (velocity.inputs[index].value = String(value)));
    }
    showInitial();
    start.value = p.start ?? "";
    durationH.value = p.durationS !== undefined ? String(p.durationS / 3600) : "";
    if (p.stop !== undefined && p.durationS === undefined) {
      const from = Date.parse(p.start ?? initial.epoch ?? "");
      if (Number.isFinite(from)) durationH.value = String((Date.parse(p.stop) - from) / 3_600_000);
    }
    stepS.value = String(p.stepS ?? 60);
    const v = p.vehicle ?? {};
    massKg.value = String(v.massKg ?? 1000);
    dryMassKg.value = v.dryMassKg ?? "";
    dragAreaM2.value = v.dragAreaM2 ?? "";
    dragCoefficient.value = v.dragCoefficient ?? "";
    srpAreaM2.value = v.srpAreaM2 ?? "";
    reflectivity.value = v.reflectivity ?? "";
    const f = p.forces ?? {};
    [degree.value, order.value] = (f.gravity ?? [8, 8]).map(String);
    drag.value = f.drag ?? "exponential";
    thirdBody.set(f.thirdBody ?? true);
    srp.set(f.srp ?? true);
    relativity.set(f.relativity ?? false);
    integrator.value = p.integrator?.method ?? "dop853";
    fixedStepS.value = p.integrator?.fixedStepS ?? "";
    for (const maneuver of p.maneuvers ?? []) maneuvers.add(maneuver);
    for (const burn of p.finiteBurns ?? []) burns.add(burn);
    details.open = true;
    details.scrollIntoView({ block: "nearest" });
  }

  /** The editor as `{id, display, propagation}`; throws a message. */
  function read() {
    const label = name.value.trim() || "Spacecraft";
    const id = editing ?? uniqueId(label);
    const propagation = { name: label };
    if (initialKind.value === "tle") {
      const lines = tle.value
        .split("\n")
        .map((line) => line.trimEnd())
        .filter((line) => /^[12] /.test(line));
      if (lines.length !== 2) throw new Error("TLE: expected lines 1 and 2");
      propagation.initial = { tle: lines };
    } else if (initialKind.value === "omm") {
      let parsed;
      try {
        parsed = JSON.parse(omm.value);
      } catch (caught) {
        throw new Error(`OMM: not JSON (${caught.message})`);
      }
      propagation.initial = { omm: Array.isArray(parsed) ? parsed[0] : parsed };
    } else {
      propagation.initial = {
        epoch: epoch.value.trim(),
        positionKm: position.inputs.map(numeric),
        velocityKmS: velocity.inputs.map(numeric),
      };
    }
    assign(propagation, "start", start.value.trim() || undefined);
    const hours = optional(durationH);
    assign(propagation, "durationS", hours === undefined ? undefined : hours * 3600);
    assign(propagation, "stepS", optional(stepS));
    propagation.vehicle = compact({
      massKg: optional(massKg),
      dryMassKg: optional(dryMassKg),
      dragAreaM2: optional(dragAreaM2),
      dragCoefficient: optional(dragCoefficient),
      srpAreaM2: optional(srpAreaM2),
      reflectivity: optional(reflectivity),
    });
    propagation.forces = {
      gravity: [numeric(degree), numeric(order)],
      thirdBody: thirdBody.input.checked,
      drag: drag.value,
      srp: srp.input.checked,
      relativity: relativity.input.checked,
    };
    propagation.integrator = compact({ method: integrator.value, fixedStepS: optional(fixedStepS) });
    const impulsive = maneuvers.values();
    if (impulsive.length) propagation.maneuvers = impulsive;
    const finite = burns.values();
    if (finite.length) propagation.finiteBurns = finite;

    const color = nextColor();
    const display = editingDisplay
      ? { ...editingDisplay, label }
      : { label, pointColor: color, lineColor: `${color}a6`, leadingOrbits: 0.5, trailingOrbits: 0.5 };
    return { id, display, propagation };
  }

  function propagate() {
    error.hidden = true;
    try {
      const { id, display, propagation } = read();
      // Same rules as a project file.
      parseProject(
        { terramenta: PROJECT_VERSION, spacecraft: [{ id, ...display, propagation }] },
        { warn: () => {} },
      );
      const info = globe.addSpacecraft(id, propagation, display);
      const now = getState()?.sun.unixSeconds;
      const [from, to] = [utcSeconds(info.start), utcSeconds(info.stop)];
      if (now === undefined || now < Math.min(from, to) || now > Math.max(from, to)) globe.setClock(from);
      editing = id;
      editingDisplay = display;
      title.textContent = `Edit ${id}`;
    } catch (caught) {
      error.textContent = caught.errors ? caught.errors.join("\n") : (caught.message ?? String(caught));
      error.hidden = false;
    }
  }

  const details = el(
    "details",
    { class: "editor" },
    el("summary", {}, "Define a spacecraft"),
    title,
    row("Name", name),
    row("Initial", initialKind),
    stateFields,
    tleFields,
    ommFields,
    row("Start", start),
    row("Duration", el("div", { class: "period" }, durationH, el("span", {}, "hours"))),
    row("Step", el("div", { class: "period" }, stepS, el("span", {}, "seconds"))),
    el("h3", { class: "editor-title" }, "Vehicle"),
    row("Mass kg", massKg),
    row("Dry kg", dryMassKg),
    row("Drag m²", dragAreaM2),
    row("Cd", dragCoefficient),
    row("SRP m²", srpAreaM2),
    row("Cr", reflectivity),
    el("h3", { class: "editor-title" }, "Forces"),
    row("Gravity", el("div", { class: "vector two" }, degree, order)),
    row("Drag", drag),
    thirdBody.node,
    srp.node,
    relativity.node,
    row("Integrator", integrator),
    row("RK4 step", fixedStepS),
    el("h3", { class: "editor-title" }, "Impulsive burns (Δv m/s)"),
    maneuvers.node,
    el("h3", { class: "editor-title" }, "Finite burns"),
    burns.node,
    error,
    el(
      "div",
      { class: "buttons" },
      button("Propagate", propagate),
      button("New", reset),
    ),
    el(
      "p",
      { class: "detail" },
      "Cowell in GCRF: EGM2008 20×20, Sun/Moon, drag, SRP. TLE/OMM via SGP4 to the start epoch.",
    ),
  );

  reset();
  return { node: details, edit };

  function maneuverRow(maneuver = {}) {
    const at = text({ value: maneuver.epoch ?? clockIso(), placeholder: "Epoch" });
    const frame = select(FRAMES, maneuver.frame ?? "vnb");
    const dv = vector(["V m/s", "N m/s", "B m/s"]);
    (maneuver.deltaVKmS ?? []).forEach((value, index) => (dv.inputs[index].value = String(round(value * 1000))));
    const isp = number({ placeholder: "Isp s", value: maneuver.ispS ?? "" });
    frame.onchange = () => relabel(dv, frame.value);
    relabel(dv, frame.value);
    return {
      fields: [row("Epoch", at), row("Frame", frame), dv.node, row("Isp s", isp)],
      value: () =>
        compact({
          epoch: at.value.trim(),
          deltaVKmS: dv.inputs.map((input) => numeric(input) / 1000),
          frame: frame.value,
          ispS: optional(isp),
        }),
    };
  }

  function finiteBurnRow(burn = {}) {
    const at = text({ value: burn.start ?? clockIso(), placeholder: "Start" });
    const duration = number({ value: burn.durationS ?? 60 });
    const thrust = number({ value: burn.thrustN ?? 20 });
    const isp = number({ value: burn.ispS ?? 220 });
    const frame = select(FRAMES, burn.frame ?? "vnb");
    const direction = vector(["V", "N", "B"]);
    (burn.direction ?? [1, 0, 0]).forEach((value, index) => (direction.inputs[index].value = String(value)));
    frame.onchange = () => relabel(direction, frame.value, "");
    relabel(direction, frame.value, "");
    return {
      fields: [
        row("Start", at),
        row("Duration s", duration),
        row("Thrust N", thrust),
        row("Isp s", isp),
        row("Frame", frame),
        row("Direction", direction.node),
      ],
      value: () =>
        compact({
          start: at.value.trim(),
          durationS: numeric(duration),
          thrustN: numeric(thrust),
          ispS: numeric(isp),
          direction: direction.inputs.map(numeric),
          frame: frame.value,
        }),
    };
  }
}

/** A growable list of sub-forms; `make(value?)` → `{fields, value()}`. */
function rowList(make, addLabel) {
  const entries = [];
  const list = el("div", { class: "layers" });
  const add = (value) => {
    const entry = make(value);
    const node = el(
      "div",
      { class: "layer" },
      ...entry.fields,
      el(
        "div",
        { class: "buttons" },
        button("Remove", () => {
          node.remove();
          entries.splice(entries.indexOf(entry), 1);
        }),
      ),
    );
    entries.push(entry);
    list.append(node);
  };
  return {
    node: el("div", {}, list, el("div", { class: "buttons" }, button(addLabel, () => add()))),
    add,
    clear() {
      entries.length = 0;
      list.replaceChildren();
    },
    values: () => entries.map((entry) => entry.value()),
  };
}

function relabel(vector, frame, unit = " m/s") {
  const axes = { vnb: ["V", "N", "B"], rsw: ["R", "S", "W"], lvlh: ["LVLH x", "y", "z"], inertial: ["x", "y", "z"] }[frame];
  vector.inputs.forEach((input, index) => (input.placeholder = `${axes[index]}${unit}`));
}

function uniqueId(label) {
  const base = `sc-${label.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "spacecraft"}`;
  const taken = (id) => globe.recordedSources().spacecraft.has(id) || globe.recordedSources().ephemerides.has(id);
  let id = base;
  for (let n = 2; taken(id); n++) id = `${base}-${n}`;
  return id;
}

/** Blank → `undefined`; otherwise the number (NaN stays, for the schema to report). */
function optional(input) {
  return input.value.trim() === "" ? undefined : Number(input.value);
}

function numeric(input) {
  return input.value.trim() === "" ? Number.NaN : Number(input.value);
}

function assign(target, key, value) {
  if (value !== undefined) target[key] = value;
}

function compact(object) {
  return Object.fromEntries(Object.entries(object).filter(([, value]) => value !== undefined));
}

function round(value) {
  return Math.round(value * 1e9) / 1e9;
}
