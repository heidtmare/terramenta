/**
 * Attitude section (Spacecraft tab): a profile per spacecraft from
 * `attitude.js`, its sampled sensor angles (or LVLH Euler angles) as a chart
 * and min/max table, AEM export, and an editor. Samples follow every state via
 * `attitude.syncAttitudes`; clicking the chart sets the clock.
 */

import * as attitude from "./attitude.js";
import { lineChart } from "./chart.js";
import { download, el, row, section } from "./dom.js";
import { savedStations } from "./events.js";
import * as globe from "./globe.js";
import { PROJECT_VERSION, parseProject } from "./project.js";
import { isoUtc } from "./reports.js";
import { button, rowList, toggle } from "./widgets.js";

const AXES = [
  ["+X", [1, 0, 0]],
  ["−X", [-1, 0, 0]],
  ["+Y", [0, 1, 0]],
  ["−Y", [0, -1, 0]],
  ["+Z", [0, 0, 1]],
  ["−Z", [0, 0, -1]],
];
const EULER = [
  ["Roll", "#ff6b6b"],
  ["Pitch", "#6bd66b"],
  ["Yaw", "#6b9bff"],
];

export function mountAttitude(bind, signal) {
  const editor = mountEditor();
  const list = el("div", { class: "layers" });
  let cards = [];
  let entries = [];

  const render = () => {
    const profiles = attitude.savedAttitudes();
    cards = profiles.map((profile) => profileCard(profile, editor));
    list.replaceChildren(...cards.map((card) => card.node));
    list.classList.toggle("empty", !profiles.length);
    editor.refresh();
  };
  const unsubscribe = [attitude.subscribe(render), attitude.onResult(render)];
  signal?.addEventListener("abort", () => unsubscribe.forEach((off) => off()));

  bind(list, (state) => {
    attitude.syncAttitudes();
    const next = [...globe.recordedSources().spacecraft.values()];
    if (next.length !== entries.length || next.some((entry, index) => entry !== entries[index])) {
      entries = next;
      render();
    }
    for (const card of cards) card.chart?.setCursor(state.sun.unixSeconds);
  });

  return section(
    "Attitude",
    list,
    editor.node,
    el(
      "p",
      { class: "detail" },
      "Primary axis exact, secondary as close as it allows. Body axes drawn X red, Y green, Z blue. Euler angles 3-2-1 from LVLH.",
    ),
  );
}

// --- Cards -----------------------------------------------------------------------

function profileCard(profile, editor) {
  const id = profile.spacecraft;
  const entry = globe.recordedSources().spacecraft.get(id);
  const name = entry?.display.label ?? id;
  const sampled = attitude.attitudeResult(id);
  const status = el("p", { class: "detail" });
  const aem = button("AEM", () => {
    try {
      download(`${id}.aem`, new Blob([attitude.attitudeAem(id)], { type: "text/plain" }));
    } catch (error) {
      alert(`AEM export failed: ${error.message ?? error}`);
    }
  });
  aem.disabled = !sampled?.result;
  const node = el(
    "div",
    { class: "layer report" },
    el("div", { class: "layer-head" }, el("span", { class: "mission-title", title: id }, profile.label ?? name)),
    el("p", { class: "detail" }, `${profile.label ? `${name} · ` : ""}${attitude.lawName(profile)}`),
    ...fovLine(profile),
    el(
      "div",
      { class: "buttons" },
      aem,
      button("Edit", () => editor.edit(profile)),
      button("Remove", () => attitude.removeAttitude(id)),
    ),
    status,
  );
  if (!sampled) {
    status.textContent = "Not sampled";
    return { node };
  }
  if (sampled.error) {
    status.textContent = sampled.error;
    status.classList.add("error");
    node.dataset.status = "failed";
    return { node };
  }
  const { result } = sampled;
  node.dataset.status = "ready";
  const step = profile.stepS ?? attitude.DEFAULT_STEP_S;
  status.textContent = [
    `${result.times.length} samples · ${step} s`,
    result.undefined ? `${result.undefined} undefined (parallel directions)` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  if (result.undefined) status.classList.add("error");

  const sensors = attitude.angledSensors(profile).map((sensor, index) => ({
    label: sensor.label ?? `${attitude.axisName(sensor.body)} → ${attitude.directionName(sensor.to)}`,
    color: sensor.color ?? attitude.SENSOR_COLOR,
    values: result.sensorAnglesDeg[index],
  }));
  const euler = EULER.map(([label, color], index) => ({
    label,
    color,
    values: result.eulerDeg.map((angles) => angles[index]),
  }));
  const charted = sensors.length ? sensors : euler;
  const chart = lineChart({
    series: charted.map(({ label, color, values }) => ({
      label,
      color,
      points: values.map((value, index) => [result.times[index], value]),
    })),
    unit: "°",
    onPick: (t) => globe.setClock(t),
  });
  node.append(
    el(
      "p",
      { class: "detail chart-legend" },
      el("span", {}, sensors.length ? "Sensor angle" : "LVLH Euler"),
      ...charted.map(({ label, color }) => el("span", {}, el("i", { style: `background: ${color}` }), label)),
    ),
    el("div", { class: "chart-wrap" }, chart.node),
    table(
      ["Angle", "Min", "At", "Max"],
      [...sensors, ...euler].map(({ label, values }) => {
        const { min, max, at } = extremes(values, result.times);
        // No "at" for a constant.
        const when = at === null || max - min < 1e-9 ? "—" : isoUtc(at).slice(5, 19).replace("T", " ");
        return [label, degrees(min), when, degrees(max)];
      }),
    ),
  );
  return { node, chart };
}

/** `["FOV Camera cone 20° · …"]` paragraph, or none. */
function fovLine(profile) {
  const fovs = (profile.sensors ?? [])
    .filter((sensor) => sensor.fov)
    .map((sensor) => `${sensor.label ?? attitude.axisName(sensor.body)} ${attitude.fovName(sensor.fov)}`);
  return fovs.length ? [el("p", { class: "detail" }, `FOV ${fovs.join(" · ")}`)] : [];
}

/** Min (with its time) and max of the finite values. */
function extremes(values, times) {
  let min = Infinity;
  let max = -Infinity;
  let at = null;
  values.forEach((value, index) => {
    if (!Number.isFinite(value)) return;
    if (value < min) [min, at] = [value, times[index]];
    max = Math.max(max, value);
  });
  return { min, max, at };
}

const degrees = (value) => (Number.isFinite(value) ? `${value.toFixed(2).replace(/^-(0\.00)$/, "$1")}°` : "—");

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
  const text = (props = {}) => el("input", { class: "coordinate", spellcheck: false, ...props });
  const number = (props = {}) => el("input", { class: "coordinate", type: "number", step: "any", ...props });
  const select = (options, value) => {
    const node = el("select", { class: "select" });
    setOptions(node, options, value);
    return node;
  };

  const title = el("summary", {}, "Define an attitude");
  const name = text({ placeholder: "Optional" });
  const spacecraft = select([], "");
  const mode = select(
    [
      ["point", "Align / constrain"],
      ["inertial", "Inertial hold"],
    ],
    "point",
  );
  const primary = pointing({ body: [0, 0, 1], to: "nadir" });
  const secondary = pointing({ body: [1, 0, 0], to: "velocity" });
  const quaternion = text({ placeholder: "x, y, z, w (body → GCRF)", value: "0, 0, 0, 1" });
  const spinRate = number({ placeholder: "none" });
  const spinAxis = axisSelect([0, 0, 1]);
  const site = select([], "none");
  const lat = number({ min: "-90", max: "90", placeholder: "Latitude °" });
  const lon = number({ placeholder: "Longitude °" });
  const alt = number({ value: "0", placeholder: "Altitude km" });
  const siteFields = el("div", { hidden: true }, row("Lat", lat), row("Lon", lon), row("Alt km", alt));
  const step = number({ min: "0", placeholder: String(attitude.DEFAULT_STEP_S) });
  const axes = toggle("Draw body axes", () => {});
  axes.set(true);
  const sensors = rowList(sensorRow, "Add sensor");
  const error = el("p", { class: "detail error", hidden: true });
  const submit = button("Add", save);
  const cancel = button("Cancel", reset, { hidden: true });

  const pointFields = el("div", {}, row("Primary", primary.node), row("Secondary", secondary.node));
  const inertialFields = el("div", { hidden: true }, row("Quaternion", quaternion));
  const showMode = () => {
    pointFields.hidden = mode.value !== "point";
    inertialFields.hidden = mode.value !== "inertial";
  };
  mode.onchange = showMode;
  site.onchange = () => (siteFields.hidden = site.value !== "site");

  /** Spacecraft and station choices follow the scene. */
  function refresh() {
    const taken = new Set(attitude.savedAttitudes().map((profile) => profile.spacecraft));
    const ids = [...globe.recordedSources().spacecraft]
      .filter(([id]) => id === editing || !taken.has(id))
      .map(([id, entry]) => [id, entry.display.label ?? id]);
    setOptions(spacecraft, ids, editing ?? (spacecraft.value || ids[0]?.[0]));
    if (editing && spacecraft.value !== editing) {
      spacecraft.append(el("option", { value: editing }, editing));
      spacecraft.value = editing;
    }
    const stations = savedStations().map((station) => [`station:${station.id}`, `Station ${station.label ?? station.id}`]);
    setOptions(site, [["none", "None"], ...stations, ["site", "Site…"]], site.value);
    siteFields.hidden = site.value !== "site";
  }

  function edit(profile) {
    reset();
    editing = profile.spacecraft;
    name.value = profile.label ?? "";
    refresh();
    spacecraft.disabled = true;
    mode.value = profile.quaternion ? "inertial" : "point";
    if (profile.quaternion) quaternion.value = profile.quaternion.join(", ");
    else {
      primary.set(profile.primary);
      secondary.set(profile.secondary);
    }
    if (profile.spin) {
      spinRate.value = profile.spin.rateDegS;
      setAxis(spinAxis, profile.spin.axis);
    }
    if (profile.station !== undefined) {
      if (![...site.options].some((option) => option.value === `station:${profile.station}`)) {
        site.append(el("option", { value: `station:${profile.station}` }, profile.station));
      }
      site.value = `station:${profile.station}`;
    } else if (profile.target) {
      site.value = "site";
      lat.value = profile.target.latDeg;
      lon.value = profile.target.lonDeg;
      alt.value = profile.target.altKm ?? 0;
    }
    siteFields.hidden = site.value !== "site";
    step.value = profile.stepS ?? "";
    axes.set(profile.axes ?? true);
    for (const sensor of profile.sensors ?? []) sensors.add(sensor);
    showMode();
    title.textContent = `Edit ${profile.label ?? profile.spacecraft}`;
    submit.textContent = "Save";
    cancel.hidden = false;
    node.open = true;
    node.scrollIntoView({ block: "nearest" });
  }

  function reset() {
    editing = null;
    spacecraft.disabled = false;
    name.value = "";
    mode.value = "point";
    primary.set({ body: [0, 0, 1], to: "nadir" });
    secondary.set({ body: [1, 0, 0], to: "velocity" });
    quaternion.value = "0, 0, 0, 1";
    spinRate.value = "";
    setAxis(spinAxis, [0, 0, 1]);
    site.value = "none";
    lat.value = lon.value = "";
    alt.value = "0";
    step.value = "";
    axes.set(true);
    sensors.clear();
    showMode();
    refresh();
    title.textContent = "Define an attitude";
    submit.textContent = "Add";
    cancel.hidden = true;
    error.hidden = true;
  }

  function save() {
    error.hidden = true;
    try {
      const entry = globe.recordedSources().spacecraft.get(spacecraft.value);
      if (!entry) throw new Error("Pick a spacecraft.");
      const profile = { spacecraft: spacecraft.value };
      if (name.value.trim()) profile.label = name.value.trim();
      if (mode.value === "inertial") {
        profile.quaternion = quaternion.value.split(/[\s,]+/).filter(Boolean).map(Number);
      } else {
        profile.primary = primary.value();
        profile.secondary = secondary.value();
      }
      if (spinRate.value.trim()) profile.spin = { axis: axisValue(spinAxis), rateDegS: Number(spinRate.value) };
      const stations = savedStations();
      if (site.value.startsWith("station:")) profile.station = site.value.slice("station:".length);
      else if (site.value === "site") {
        profile.target = { latDeg: Number(lat.value), lonDeg: Number(lon.value) };
        if (alt.value.trim() && Number(alt.value)) profile.target.altKm = Number(alt.value);
      }
      if (step.value.trim()) profile.stepS = Number(step.value);
      if (!axes.input.checked) profile.axes = false;
      const list = sensors.values();
      if (list.length) profile.sensors = list;
      // Same rules as a project file.
      parseProject(
        {
          terramenta: PROJECT_VERSION,
          spacecraft: [{ id: profile.spacecraft, propagation: entry.propagation }],
          groundStations: stations,
          attitudes: [profile],
        },
        { warn: () => {} },
      );
      attitude.setAttitude(profile);
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
    row("Craft", spacecraft),
    row("Name", name),
    row("Law", mode),
    pointFields,
    inertialFields,
    row("Spin °/s", spinRate),
    row("Spin axis", spinAxis),
    row("Target", site),
    siteFields,
    row("Step s", step),
    axes.node,
    el("h3", { class: "editor-title" }, "Sensors"),
    sensors.node,
    error,
    el("div", { class: "buttons" }, submit, cancel),
  );
  return { node, edit, refresh };

  function sensorRow(sensor = {}) {
    const label = text({ placeholder: "Name", value: sensor.label ?? "" });
    const aim = pointing({ body: sensor.body ?? [0, 0, 1], to: "body" in sensor ? sensor.to : "sun" }, { optional: true });
    const color = el("input", { class: "swatch", type: "color", value: (sensor.color ?? attitude.SENSOR_COLOR).slice(0, 7) });
    const draw = toggle("Draw", () => {});
    draw.set(sensor.draw ?? true);
    const fov = sensor.fov ?? {};
    const kind = select(
      [
        ["none", "None"],
        ["cone", "Cone"],
        ["rectangle", "Rectangle"],
      ],
      fov.halfAngleDeg !== undefined ? "cone" : fov.halfXDeg !== undefined ? "rectangle" : "none",
    );
    const half = number({ min: "0", max: "180", placeholder: "Half-angle °", value: fov.halfAngleDeg ?? "" });
    const halfX = number({ min: "0", max: "90", placeholder: "Half-width °", value: fov.halfXDeg ?? "" });
    const halfY = number({ min: "0", max: "90", placeholder: "Half-height °", value: fov.halfYDeg ?? "" });
    const reference = axisSelect(fov.reference ?? [1, 0, 0]);
    const cone = el("span", { class: "pair" }, half);
    const rectangle = el("span", { class: "pair" }, halfX, "×", halfY, "width along", reference);
    const showKind = () => {
      cone.hidden = kind.value !== "cone";
      rectangle.hidden = kind.value !== "rectangle";
    };
    kind.onchange = showKind;
    showKind();
    return {
      fields: [
        row("Name", label),
        row("Axis", aim.node),
        row("FOV", el("span", { class: "pair" }, kind, cone, rectangle)),
        row("Colour", color),
        draw.node,
      ],
      value: () => {
        const { body, to } = aim.value();
        const out = { body };
        if (to !== undefined) out.to = to;
        out.color = color.value;
        if (label.value.trim()) out.label = label.value.trim();
        if (!draw.input.checked) out.draw = false;
        if (kind.value === "cone") out.fov = { halfAngleDeg: Number(half.value) };
        if (kind.value === "rectangle") {
          out.fov = { halfXDeg: Number(halfX.value), halfYDeg: Number(halfY.value), reference: axisValue(reference) };
        }
        return out;
      },
    };
  }
}

/**
 * Body axis + direction selects: `{node, set({body, to}), value()}`. `optional`:
 * a "no angle" choice, `to` undefined.
 */
function pointing({ body, to }, { optional = false } = {}) {
  const axis = axisSelect(body);
  const direction = el("select", { class: "select" });
  const set = (value) => {
    setAxis(axis, value.body);
    const options = Object.entries(attitude.DIRECTIONS);
    if (optional) options.unshift(["", "No angle"]);
    if (Array.isArray(value.to)) options.push([JSON.stringify(value.to), attitude.directionName(value.to)]);
    setOptions(direction, options, Array.isArray(value.to) ? JSON.stringify(value.to) : (value.to ?? ""));
  };
  set({ body, to });
  const read = () => {
    if (direction.value === "") return undefined;
    return direction.value.startsWith("[") ? JSON.parse(direction.value) : direction.value;
  };
  return {
    node: el("span", { class: "pair" }, axis, "→", direction),
    set,
    value: () => ({ body: axisValue(axis), to: read() }),
  };
}

function axisSelect(vector) {
  const node = el("select", { class: "select" });
  setAxis(node, vector);
  return node;
}

/** Selects `vector`, adding it as a choice when it is not a signed unit axis. */
function setAxis(node, vector) {
  const key = JSON.stringify(vector);
  const options = AXES.map(([label, axis]) => [JSON.stringify(axis), label]);
  if (!options.some(([value]) => value === key)) options.push([key, attitude.axisName(vector)]);
  setOptions(node, options, key);
}

const axisValue = (node) => JSON.parse(node.value);

/** Replaces `node`'s options, keeping `value` when it is still one of them. */
function setOptions(node, options, value) {
  node.replaceChildren(...options.map(([key, label]) => el("option", { value: key }, label)));
  if (options.some(([key]) => key === value)) node.value = value;
}
