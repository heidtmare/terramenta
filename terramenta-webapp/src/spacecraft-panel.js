/**
 * Spacecraft tab: numerically propagated spacecraft and their editor,
 * targeting (`targeting-panel.js`), attitude (`attitude-panel.js`), ground
 * stations (`events-panel.js`), propagation environment.
 *
 * A spacecraft is a `propagate` request plus ephemeris display options
 * (`globe.addSpacecraft`), saved under the project's `spacecraft`. The editor
 * builds the request, validates it as a project entry (`parseProject`), and
 * propagates; editing an existing one re-propagates under its id. A mission
 * sequence (`sequence.js`, JSON) replaces duration and burns: it resolves them.
 * Rows are rebuilt from `globe.recordedSources().spacecraft`, so failed ones
 * show too.
 * Units in the editor: km, km/s, m/s for Δv, s, kg, N.
 */

import { mountAttitude } from "./attitude-panel.js";
import { flightOptions } from "./attitude.js";
import * as environment from "./environment.js";
import { mountStations } from "./events-panel.js";
import { mountTargeting } from "./targeting-panel.js";
import * as globe from "./globe.js";
import { download, el, row, section } from "./dom.js";
import { clock, duration, utcSeconds } from "./format.js";
import { SPACECRAFT_COLORS as PALETTE } from "./palette.js";
import { PROJECT_VERSION, layerDisplay, parseProject } from "./project.js";
import { segmentRows } from "./sequence.js";
import { button, rowList, toggle } from "./widgets.js";

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
/** Finite burns also thrust along body axes, under the spacecraft's attitude profile. */
const BURN_FRAMES = [...FRAMES, ["body", "Body"]];
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
/** Project covariance frames → editor `FRAMES` keys. */
const COVARIANCE_FRAMES = { ric: "rsw", rtn: "rsw", gcrf: "inertial", eci: "inertial" };
/** Editor template: LEO → GEO; transfer at the ascending node, circularize and plane change at the descending node. */
const SEQUENCE_TEMPLATE = [
  { propagate: { to: "ascendingNode" } },
  {
    label: "TOI",
    target: {
      variables: [{ command: "toi-burn", parameter: "dv1" }],
      goals: [{ quantity: "radiusKm", value: 42164, tolerance: 0.1 }],
      sequence: [
        { label: "toi-burn", maneuver: { deltaVKmS: [2.4, 0, 0], frame: "vnb", ispS: 320 } },
        { propagate: { to: "descendingNode" } },
      ],
    },
  },
  {
    label: "GOI",
    target: {
      variables: ["dv1", "dv2", "dv3"].map((parameter) => ({ command: "goi-burn", parameter })),
      goals: [
        { quantity: "smaKm", value: 42164, tolerance: 1 },
        { quantity: "inclinationDeg", value: 0, tolerance: 0.01 },
        { quantity: "flightPathAngleDeg", value: 0, tolerance: 0.001 },
      ],
      sequence: [{ label: "goi-burn", maneuver: { deltaVKmS: [1.4, -0.8, 0], frame: "vnb", ispS: 320 } }],
    },
  },
  { propagate: { durationS: 86400 } },
];

const INITIAL = [
  ["elements", "Keplerian elements"],
  ["state", "GCRF state"],
  ["tle", "TLE"],
  ["omm", "OMM JSON"],
];

/** `bind(node, sync)`: the panel's registration. `signal` aborts on close. */
export function mountSpacecraft(bind, signal) {
  return [
    mountSpacecraftList(bind),
    mountTargeting(bind, signal),
    mountAttitude(bind, signal),
    mountStations(signal),
    mountEnvironment(signal),
  ];
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
  const segments = el("details", { class: "sequence-report", hidden: true });
  let shownRun = null;

  const node = el(
    "div",
    { class: "layer" },
    el("div", { class: "layer-head" }, visible.node, color),
    detail,
    error,
    segments,
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
      segments.hidden = !spacecraft.run;
      if (spacecraft.run && spacecraft.run !== shownRun) {
        shownRun = spacecraft.run;
        segments.replaceChildren(...sequenceReport(spacecraft.run));
      }
    },
  };
}

function describeInfo({ info, propagation }, state) {
  const start = utcSeconds(info.start);
  const stop = utcSeconds(info.stop);
  const now = state.sun.unixSeconds;
  const where = now < start ? "clock before span" : now > stop ? "clock after span" : "in span";
  const burns = (propagation.maneuvers?.length ?? 0) + (propagation.finiteBurns?.length ?? 0);
  const sigma = info.covariance?.sigmaRsw.at(-1);
  const spread = sigma ? ` · 1σ ${length(Math.hypot(...sigma.slice(0, 3)))} at end` : "";
  return (
    `${clock(start)} → ${clock(stop)} · ${info.samples} samples · ${info.finalMassKg.toFixed(1)} kg` +
    `${burns ? ` · ${burns} burn${burns > 1 ? "s" : ""}` : ""}${spread} · ${where}`
  );
}

/** Summary and segment table of a `runSequence` result; a row sets the clock to its start. */
function sequenceReport(run) {
  const status = `${run.segments.length} segments · Δv ${(run.deltaVKmS * 1000).toFixed(1)} m/s` +
    `${run.converged ? "" : " · a target did not converge"}`;
  const fixed = (value, digits) => (value === null ? "" : value.toFixed(digits));
  const rows = segmentRows(run).map((segment) =>
    el(
      "tr",
      { class: "pick", title: `${clock(segment.start)} → ${clock(segment.stop)}`, onclick: () => globe.setClock(segment.start) },
      el("td", { style: `padding-left: ${0.4 + segment.depth}em` }, segment.name),
      el("td", {}, segment.durationS > 0 ? duration(segment.durationS) : ""),
      el("td", {}, fixed(segment.deltaVMS, 1)),
      el("td", {}, segment.altitudeKm.toFixed(0)),
      el("td", {}, segment.eccentricity.toFixed(4)),
      el("td", {}, segment.inclinationDeg.toFixed(2)),
      el("td", { class: segment.status?.startsWith("NOT") ? "error" : "" }, segment.status ?? segment.massKg.toFixed(1)),
    ),
  );
  return [
    el("summary", { class: run.converged ? "" : "error" }, `Sequence · ${status}`),
    el(
      "div",
      { class: "table-wrap" },
      el(
        "table",
        { class: "report-table" },
        el("thead", {}, el("tr", {}, ...["Segment", "Span", "Δv m/s", "Alt km", "e", "i °", "kg / target"].map((text) => el("th", {}, text)))),
        el("tbody", {}, ...rows),
      ),
    ),
  ];
}

function length(km) {
  return km < 1 ? `${(km * 1000).toFixed(1)} m` : `${km.toFixed(2)} km`;
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

  const initialKind = select(INITIAL, "elements");
  const epoch = text({ placeholder: "2026-01-01T00:00:00Z" });
  const position = vector(["x km", "y km", "z km"]);
  const velocity = vector(["vx km/s", "vy km/s", "vz km/s"]);
  const circular = number({ value: "500", title: "Altitude, km" });
  const inclination = number({ value: "51.6", title: "Inclination, °" });
  const elements = Object.fromEntries(
    [
      ["smaKm", "Semi-major axis, km"],
      ["eccentricity", "Eccentricity"],
      ["inclinationDeg", "Inclination, °"],
      ["raanDeg", "RAAN, °"],
      ["argPerigeeDeg", "Argument of perigee, °"],
      ["trueAnomalyDeg", "True anomaly, °"],
    ].map(([key, label]) => [key, number({ title: label, placeholder: label })]),
  );
  const tle = el("textarea", { class: "coordinate", rows: 3, spellcheck: false, placeholder: "Two-line element set" });
  const omm = el("textarea", { class: "coordinate", rows: 4, spellcheck: false, placeholder: "OMM JSON object" });

  const epochRow = row("Epoch", epoch);
  const elementsFields = el(
    "div",
    {},
    row("a km", elements.smaKm),
    row("e", elements.eccentricity),
    row("i °", elements.inclinationDeg),
    row("RAAN °", elements.raanDeg),
    row("ω °", elements.argPerigeeDeg),
    row("ν °", elements.trueAnomalyDeg),
  );
  const stateFields = el(
    "div",
    {},
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
    epochRow.hidden = initialKind.value !== "state" && initialKind.value !== "elements";
    elementsFields.hidden = initialKind.value !== "elements";
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
  const plates = rowList(plateRow, "Add plate");

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

  const sequenceText = el("textarea", {
    class: "coordinate",
    rows: 10,
    spellcheck: false,
    placeholder: "Mission sequence: JSON array of commands (blank: none)",
  });
  /** Shown without a sequence; a sequence resolves duration and burns. */
  const manual = [];
  const showSequence = () => {
    const sequenced = sequenceText.value.trim() !== "";
    for (const node of manual) node.hidden = sequenced;
  };
  sequenceText.oninput = showSequence;

  const covarianceFrame = select(FRAMES, "rsw");
  const positionSigma = vector([]);
  const velocitySigma = vector([]);
  const processNoise = vector([]);
  const ellipsoidSigma = number({ value: "3", min: "0", title: "Ellipsoid drawn at this many σ; 0 hides" });
  /** A project's `lowerTriangle`, kept while no sigma is typed. */
  let keptMatrix = null;
  const matrixNote = el("p", { class: "detail", hidden: true }, "Full covariance matrix from the project; sigmas replace it.");
  const relabelCovariance = () => {
    relabel(positionSigma, covarianceFrame.value, " m");
    relabel(velocitySigma, covarianceFrame.value, " m/s");
    relabel(processNoise, covarianceFrame.value, " m²/s³");
  };
  covarianceFrame.onchange = relabelCovariance;

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
    initialKind.value = "elements";
    epoch.value = "";
    const defaults = { smaKm: EARTH_RADIUS_KM + 500, eccentricity: 0, inclinationDeg: 51.6, raanDeg: 0, argPerigeeDeg: 0, trueAnomalyDeg: 0 };
    for (const [key, input] of Object.entries(elements)) input.value = String(defaults[key]);
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
    plates.clear();
    sequenceText.value = "";
    covarianceFrame.value = "rsw";
    for (const input of [...positionSigma.inputs, ...velocitySigma.inputs, ...processNoise.inputs]) input.value = "";
    ellipsoidSigma.value = "3";
    keptMatrix = null;
    matrixNote.hidden = true;
    relabelCovariance();
    error.hidden = true;
    fillCircular();
    showInitial();
    showSequence();
  }

  function edit(id, spacecraft, layer) {
    reset();
    editing = id;
    editingDisplay = layer ? layerDisplay(layer) : spacecraft.display;
    title.textContent = `Edit ${id}`;
    const p = spacecraft.base ?? spacecraft.propagation;
    name.value = editingDisplay.label ?? p.name ?? id;
    const initial = p.initial;
    if (initial.tle) {
      initialKind.value = "tle";
      tle.value = initial.tle.join("\n");
    } else if (initial.omm) {
      initialKind.value = "omm";
      omm.value = JSON.stringify(initial.omm, null, 2);
    } else if (initial.elements) {
      initialKind.value = "elements";
      epoch.value = initial.epoch;
      for (const [key, input] of Object.entries(elements)) input.value = String(initial.elements[key]);
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
    for (const plate of v.plates ?? []) plates.add(plate);
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
    sequenceText.value = spacecraft.sequence ? JSON.stringify(spacecraft.sequence, null, 2) : "";
    showSequence();
    const c = p.covariance ?? {};
    covarianceFrame.value = COVARIANCE_FRAMES[c.frame] ?? c.frame ?? "rsw";
    const fill = (vector, values, scale) =>
      vector.inputs.forEach((input, index) => (input.value = values ? String(Number((values[index] * scale).toPrecision(12))) : ""));
    fill(positionSigma, c.positionSigmaKm, 1e3);
    fill(velocitySigma, c.velocitySigmaKmS, 1e3);
    fill(processNoise, c.processNoiseKm2S3, 1e6);
    keptMatrix = c.lowerTriangle ?? null;
    matrixNote.hidden = !keptMatrix;
    ellipsoidSigma.value = String(spacecraft.display.uncertaintySigma ?? 3);
    relabelCovariance();
    details.open = true;
    details.scrollIntoView({ block: "nearest" });
  }

  /** The editor as `{id, display, propagation, sequence?}`; throws a message. */
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
    } else if (initialKind.value === "elements") {
      propagation.initial = {
        epoch: epoch.value.trim(),
        elements: Object.fromEntries(Object.entries(elements).map(([key, input]) => [key, numeric(input)])),
      };
    } else {
      propagation.initial = {
        epoch: epoch.value.trim(),
        positionKm: position.inputs.map(numeric),
        velocityKmS: velocity.inputs.map(numeric),
      };
    }
    let sequence;
    if (sequenceText.value.trim()) {
      try {
        sequence = JSON.parse(sequenceText.value);
      } catch (caught) {
        throw new Error(`Sequence: not JSON (${caught.message})`);
      }
      if (!Array.isArray(sequence)) throw new Error("Sequence: expected an array of commands");
    }
    assign(propagation, "start", start.value.trim() || undefined);
    const hours = sequence ? undefined : optional(durationH);
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
    const plated = plates.values();
    if (plated.length) propagation.vehicle.plates = plated;
    propagation.forces = {
      gravity: [numeric(degree), numeric(order)],
      thirdBody: thirdBody.input.checked,
      drag: drag.value,
      srp: srp.input.checked,
      relativity: relativity.input.checked,
    };
    propagation.integrator = compact({ method: integrator.value, fixedStepS: optional(fixedStepS) });
    const impulsive = sequence ? [] : maneuvers.values();
    if (impulsive.length) propagation.maneuvers = impulsive;
    const finite = sequence ? [] : burns.values();
    if (finite.length) propagation.finiteBurns = finite;
    const covariance = readCovariance();
    if (covariance) propagation.covariance = covariance;

    const color = nextColor();
    const display = editingDisplay
      ? { ...editingDisplay, label }
      : { label, pointColor: color, lineColor: `${color}a6`, leadingOrbits: 0.5, trailingOrbits: 0.5 };
    const sigma = optional(ellipsoidSigma);
    delete display.uncertaintySigma;
    if (covariance && sigma !== undefined && sigma !== 3) display.uncertaintySigma = sigma;
    return { id, display, propagation, sequence };
  }

  /** Editor fields → `propagation.covariance` (km), or `undefined` when none is set. */
  function readCovariance() {
    const read = (vector, divisor) => {
      const values = vector.inputs.map(optional);
      return values.some((value) => value !== undefined) ? values.map((value) => Number(((value ?? 0) / divisor).toPrecision(12))) : undefined;
    };
    const position = read(positionSigma, 1e3);
    const velocity = read(velocitySigma, 1e3);
    const noise = read(processNoise, 1e6);
    const matrix = position || velocity ? undefined : (keptMatrix ?? undefined);
    if (!position && !velocity && !matrix && !noise) return undefined;
    return compact({
      frame: covarianceFrame.value === "rsw" ? undefined : covarianceFrame.value,
      positionSigmaKm: position,
      velocitySigmaKmS: velocity,
      lowerTriangle: matrix,
      processNoiseKm2S3: noise,
    });
  }

  function propagate() {
    error.hidden = true;
    try {
      const { id, display, propagation, sequence } = read();
      // Same rules as a project file.
      parseProject(
        { terramenta: PROJECT_VERSION, spacecraft: [{ id, ...display, propagation, ...(sequence ? { sequence } : {}) }] },
        { warn: () => {} },
      );
      const info = globe.addSpacecraft(id, propagation, display, flightOptions(id, propagation, sequence));
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

  const durationRow = row("Duration", el("div", { class: "period" }, durationH, el("span", {}, "hours")));
  const burnFields = [
    el("h3", { class: "editor-title" }, "Impulsive burns (Δv m/s)"),
    maneuvers.node,
    el("h3", { class: "editor-title" }, "Finite burns"),
    burns.node,
  ];
  manual.push(durationRow, ...burnFields);
  const details = el(
    "details",
    { class: "editor" },
    el("summary", {}, "Define a spacecraft"),
    title,
    row("Name", name),
    row("Initial", initialKind),
    epochRow,
    elementsFields,
    stateFields,
    tleFields,
    ommFields,
    row("Start", start),
    durationRow,
    row("Step", el("div", { class: "period" }, stepS, el("span", {}, "seconds"))),
    el("h3", { class: "editor-title" }, "Vehicle"),
    row("Mass kg", massKg),
    row("Dry kg", dryMassKg),
    row("Drag m²", dragAreaM2),
    row("Cd", dragCoefficient),
    row("SRP m²", srpAreaM2),
    row("Cr", reflectivity),
    el("h3", { class: "editor-title" }, "Plates"),
    el(
      "p",
      { class: "detail" },
      "Flat plates in body axes replace the drag and SRP areas and Cr; the spacecraft then flies its attitude profile.",
    ),
    plates.node,
    el("h3", { class: "editor-title" }, "Forces"),
    row("Gravity", el("div", { class: "vector two" }, degree, order)),
    row("Drag", drag),
    thirdBody.node,
    srp.node,
    relativity.node,
    row("Integrator", integrator),
    row("RK4 step", fixedStepS),
    ...burnFields,
    el("h3", { class: "editor-title" }, "Mission sequence"),
    sequenceText,
    el(
      "div",
      { class: "buttons" },
      button("LEO → GEO template", () => {
        sequenceText.value = JSON.stringify(SEQUENCE_TEMPLATE, null, 2);
        showSequence();
      }),
    ),
    el(
      "p",
      { class: "detail" },
      "Commands in order: propagate (durationS | epoch | to periapsis/apoapsis/ascendingNode/descendingNode | " +
        "quantity + value), maneuver, finiteBurn, target {variables, goals, sequence}. Replaces duration and burns.",
    ),
    el("h3", { class: "editor-title" }, "Uncertainty (1σ at start)"),
    row("Frame", covarianceFrame),
    row("Position", positionSigma.node),
    row("Velocity", velocitySigma.node),
    row("Noise", processNoise.node),
    matrixNote,
    row("Ellipsoid σ", ellipsoidSigma),
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
      "Cowell in GCRF: EGM2008 20×20, Sun/Moon, drag, SRP. TLE/OMM via SGP4 to the start epoch. " +
        "Uncertainty: linear covariance through the state transition matrix; noise is white acceleration spectral density.",
    ),
  );

  reset();
  return { node: details, edit };

  function plateRow(plate = {}) {
    const normal = vector(["body x", "y", "z"]);
    (plate.normal ?? [1, 0, 0]).forEach((value, index) => (normal.inputs[index].value = String(value)));
    const area = number({ value: plate.areaM2 ?? 1, min: "0" });
    const specular = number({ value: plate.specular ?? "", min: "0", max: "1", placeholder: "0" });
    const diffuse = number({ value: plate.diffuse ?? "", min: "0", max: "1", placeholder: "0" });
    return {
      fields: [row("Normal", normal.node), row("Area m²", area), row("Specular", specular), row("Diffuse", diffuse)],
      value: () =>
        compact({
          normal: normal.inputs.map(numeric),
          areaM2: numeric(area),
          specular: optional(specular),
          diffuse: optional(diffuse),
        }),
    };
  }

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
    const frame = select(BURN_FRAMES, burn.frame ?? "vnb");
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

function relabel(vector, frame, unit = " m/s") {
  const axes = {
    vnb: ["V", "N", "B"],
    rsw: ["R", "S", "W"],
    lvlh: ["LVLH x", "y", "z"],
    inertial: ["x", "y", "z"],
    body: ["body x", "y", "z"],
  }[frame];
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
