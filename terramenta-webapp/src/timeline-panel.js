/**
 * Timeline tab: every event (`timeline.js`) on a strip and in a list, with the
 * clock against them. Click the strip or Go to jump; Previous/Next step
 * between events. Project events are added at the clock (or relative to
 * another event) with actions taken from the current scene, and removed;
 * scene and director events are read-only.
 */

import { el, row, section } from "./dom.js";
import { clock, duration } from "./format.js";
import * as globe from "./globe.js";
import * as timeline from "./timeline.js";
import { button, toggle } from "./widgets.js";

/** Within this many seconds of the clock, an event is "now" for Previous/Next. */
const NEAR_S = 0.5;

export function mountTimeline(bind) {
  let lastState = null;
  let resolved = [];
  let signature = "";

  const directed = toggle("Directed: apply event actions", timeline.setDirected);
  bind(directed.node, () => directed.set(timeline.isDirected()));

  const step = (direction) => {
    if (!lastState) return;
    const now = lastState.sun.unixSeconds;
    const known = resolved.filter((event) => event.unixSeconds !== null);
    const target =
      direction > 0
        ? known.find((event) => event.unixSeconds > now + NEAR_S)
        : known.findLast((event) => event.unixSeconds < now - NEAR_S);
    if (target) globe.setClock(target.unixSeconds);
  };

  const status = el("p", { class: "detail" }, "—");
  const cursor = el("div", { class: "timeline-cursor" });
  const markers = el("div", { class: "timeline-markers" });
  let range = null;
  const strip = el(
    "div",
    {
      class: "timeline-strip",
      title: "Click to set the clock",
      onclick: (event) => {
        if (!range) return;
        const box = strip.getBoundingClientRect();
        const fraction = Math.min(Math.max((event.clientX - box.left) / box.width, 0), 1);
        globe.setClock(range.from + fraction * (range.to - range.from));
      },
    },
    markers,
    cursor,
  );
  const span = el("div", { class: "timeline-span" }, el("span", {}, ""), el("span", {}, ""));

  const list = el("div", { class: "layers" });
  const rows = [];
  const editor = mountEditor(() => lastState, () => resolved);

  bind(list, (state) => {
    lastState = state;
    resolved = timeline.timelineEvents(state);
    const next = JSON.stringify(resolved.map((event) => [event.id, event.unixSeconds, event.label]));
    if (next !== signature) {
      signature = next;
      rebuild();
      editor.refresh();
    }
    update(state.sun.unixSeconds);
  });

  function rebuild() {
    const known = resolved.filter((event) => event.unixSeconds !== null);
    range = null;
    if (known.length) {
      const from = known[0].unixSeconds;
      const to = known.at(-1).unixSeconds;
      const pad = Math.max((to - from) * 0.03, 60);
      range = { from: from - pad, to: to + pad };
    }
    markers.replaceChildren(
      ...known.map((event) =>
        el("div", {
          class: `timeline-marker ${event.source}`,
          style: `left: ${position(event.unixSeconds)}%`,
          title: `${event.label}\n${clock(event.unixSeconds)}`,
        }),
      ),
    );
    strip.hidden = span.hidden = !range;
    if (range) [span.firstChild.textContent, span.lastChild.textContent] = [clock(range.from), clock(range.to)];

    rows.length = 0;
    rows.push(...resolved.map(eventRow));
    list.replaceChildren(...rows.map((entry) => entry.node));
    list.classList.toggle("empty", rows.length === 0);
  }

  function position(unixSeconds) {
    return ((unixSeconds - range.from) / (range.to - range.from)) * 100;
  }

  function update(now) {
    if (range) {
      cursor.style.left = `${Math.min(Math.max(position(now), 0), 100)}%`;
      cursor.classList.toggle("outside", now < range.from || now > range.to);
    }
    let upcoming = null;
    for (const entry of rows) {
      const { unixSeconds } = entry.event;
      const reached = unixSeconds !== null && unixSeconds <= now + NEAR_S;
      if (!reached && unixSeconds !== null && !upcoming) upcoming = entry;
      entry.node.dataset.status = reached ? "reached" : "pending";
      entry.when.textContent = unixSeconds === null ? "time not yet known" : relative(unixSeconds - now);
    }
    rows.forEach((entry) => entry.node.classList.toggle("next", entry === upcoming));
    status.textContent = upcoming
      ? `Next: ${upcoming.event.label}, ${relative(upcoming.event.unixSeconds - now)}`
      : resolved.length
        ? "No events ahead."
        : "No events. Burns, mission phases and added events appear here.";
  }

  return section(
    "Timeline",
    directed.node,
    el("div", { class: "buttons" }, button("Previous", () => step(-1)), button("Next", () => step(1))),
    status,
    strip,
    span,
    list,
    editor.node,
  );
}

function eventRow(event) {
  const when = el("span", {}, "");
  const head = el(
    "div",
    { class: "layer-head" },
    el("span", { class: "mission-title", title: event.label }, event.label),
    event.unixSeconds === null ? null : button("Go", () => globe.setClock(event.unixSeconds)),
    event.source === "project" ? button("Remove", () => timeline.removeEvent(event.id)) : null,
  );
  const detail = el(
    "p",
    { class: "detail" },
    event.unixSeconds === null ? "" : `${clock(event.unixSeconds)} · `,
    when,
    ` · ${event.source}`,
    event.actions ? ` · ${describeActions(event.actions)}` : "",
  );
  const node = el("div", { class: "layer timeline-event" }, head, detail);
  return { event, node, when };
}

function relative(seconds) {
  if (Math.abs(seconds) < NEAR_S) return "now";
  return seconds > 0 ? `in ${duration(seconds)}` : `${duration(-seconds)} ago`;
}

/** `rate 10× · ramp ×2/2 s · pause · heliocentric · …` */
export function describeActions(actions) {
  const parts = [];
  if (actions.timeScale !== undefined) {
    parts.push(`rate ${actions.timeScale === "max" ? "max" : `${actions.timeScale.toLocaleString()}×`}`);
  }
  if (actions.rampDoublingS !== undefined) parts.push(`ramp ×2/${actions.rampDoublingS} s`);
  if (actions.paused !== undefined) parts.push(actions.paused ? "pause" : "run");
  if (actions.view) parts.push(`${actions.view} view`);
  if (actions.frame) parts.push(actions.frame.toUpperCase());
  if (actions.focus) parts.push(`focus ${actions.focus.layer}`);
  if (actions.follow) parts.push(`follow ${actions.follow}`);
  if (actions.camera) parts.push("camera");
  return parts.join(" · ") || "no actions";
}

/** "Add event": label, time (clock or an event plus offset), actions from the scene. */
function mountEditor(getState, getEvents) {
  const label = el("input", { class: "coordinate", type: "text", placeholder: "Label" });
  const anchor = el("select", { class: "select" });
  const offset = el("input", { class: "coordinate", type: "number", step: "1", value: "0" });
  const error = el("p", { class: "detail error", hidden: true });

  const capture = [
    ["timeScale", "Rate", (state) => Math.abs(state.sun.timeScale)],
    ["paused", "Paused/running", (state) => state.sun.paused],
    ["view", "View", (state) => (state.view === "heliocentric" ? "heliocentric" : "globe")],
    ["frame", "Frame", (state) => state.frame.mode],
    [
      "camera",
      "Camera",
      (state) => ({
        lat: round(state.camera.center.lat, 4),
        lon: round(state.camera.center.lon, 4),
        altitudeKm: round(state.camera.altitudeKm, 1),
      }),
    ],
    ["focus", "Focus", (state) => state.gnc.focus && { layer: state.gnc.focus.layer, noradId: state.gnc.focus.noradId }],
  ].map(([key, text, read]) => {
    const input = el("input", { type: "checkbox", checked: key === "timeScale" });
    return { key, read, input, node: el("label", { class: "toggle" }, input, el("span", {}, text)) };
  });

  function refresh() {
    const kept = anchor.value;
    anchor.replaceChildren(
      el("option", { value: "" }, "Clock"),
      ...getEvents()
        .filter((event) => event.source !== "director")
        .map((event) => el("option", { value: event.id }, event.label)),
    );
    anchor.value = [...anchor.options].some((option) => option.value === kept) ? kept : "";
  }

  function add() {
    const state = getState();
    if (!state) return;
    const offsetS = Number(offset.value) || 0;
    const at = anchor.value
      ? { event: anchor.value, ...(offsetS ? { offsetS } : {}) }
      : new Date((state.sun.unixSeconds + offsetS) * 1000).toISOString();
    const actions = {};
    for (const { key, read, input } of capture) {
      const value = input.checked ? read(state) : undefined;
      if (value !== undefined && value !== null) actions[key] = value;
    }
    const text = label.value.trim();
    if (!text) {
      error.textContent = "Give the event a label.";
      error.hidden = false;
      return;
    }
    error.hidden = true;
    timeline.addEvent({ id: timeline.uniqueEventId(slug(text) || "event"), label: text, at, actions });
    label.value = "";
  }

  const node = el(
    "details",
    { class: "editor" },
    el("summary", {}, "Add event"),
    row("Label", label),
    row("At", anchor),
    row("Offset (s)", offset),
    el("h3", { class: "editor-title" }, "Actions, from the current scene"),
    el("div", { class: "timeline-capture" }, ...capture.map((entry) => entry.node)),
    error,
    el("div", { class: "buttons" }, button("Add", add)),
  );
  return { node, refresh };
}

function round(value, digits) {
  const scale = 10 ** digits;
  return Math.round(value * scale) / scale;
}

function slug(text) {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}
