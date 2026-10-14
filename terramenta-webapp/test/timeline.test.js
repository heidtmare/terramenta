import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import * as globe from "../src/globe.js";
import { PROJECT_VERSION, ProjectError, parseProject } from "../src/project.js";
import {
  addEvent,
  openTimeline,
  removeEvent,
  runTimeline,
  sceneEvents,
  setDirected,
  subscribe,
  timelineEvents,
  timelineSection,
  uniqueEventId,
} from "../src/timeline.js";
import { attachFake, commands } from "./fake-globe.js";

globalThis.document = { baseURI: "http://localhost:8080/app/" };

const T0 = Date.parse("2026-01-01T00:00:00Z") / 1000;
const iso = (offsetS) => new Date((T0 + offsetS) * 1000).toISOString();
const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });

const PROPAGATION = {
  initial: { epoch: iso(0), positionKm: [6878, 0, 0], velocityKmS: [0, 4.7, 6] },
  durationS: 7200,
  maneuvers: [
    { epoch: iso(1800), deltaVKmS: [0.08, 0, 0] },
    { epoch: iso(4800), deltaVKmS: [0, 0.06, 0] },
  ],
  finiteBurns: [{ start: iso(6000), durationS: 300, thrustN: 2, ispS: 220, direction: [1, 0, 0] }],
};
const SPACECRAFT = [{ id: "sc", label: "Craft", propagation: PROPAGATION }];
const MISSION = { id: "mars", origin: "Earth", destination: "Mars" };

const phases = (times = {}) =>
  ["departure", "escape", "arrival", "capture"].map((id) => ({
    id,
    label: id,
    unixSeconds: times[id] ?? null,
    reached: false,
  }));

/** A state snapshot at `T0 + offsetS`. */
const state = (offsetS, { timeScale = 60, view = "globe", missions = [] } = {}) => ({
  sun: { unixSeconds: T0 + offsetS, timeScale, paused: false },
  view,
  missions,
});

function errorsOf(json) {
  try {
    parseProject(json, { warn: () => {} });
  } catch (error) {
    assert.ok(error instanceof ProjectError, error);
    return error.errors;
  }
  assert.fail("expected a ProjectError");
}

let calls;
beforeEach(() => {
  calls = attachFake({ objects: { sc: [{ noradId: 900 }] } });
});

const sent = () => calls.filter(([name]) => commands([[name]]).length);
const spacecraftSources = () => new Map([["sc", { propagation: PROPAGATION, display: { label: "Craft" }, info: null }]]);

// --- Validation ----------------------------------------------------------------

test("timeline references are checked against spacecraft, missions and events", () => {
  const event = (id, at, actions) => ({ id, at, ...(actions ? { actions } : {}) });
  const errors = errorsOf(
    v({
      spacecraft: SPACECRAFT,
      missions: [MISSION],
      timeline: {
        skipTo: { event: "ghost.start" },
        events: [
          event("ok", { event: "sc.burn2", offsetS: -60 }),
          event("burn", { event: "sc.burn3" }),
          event("finite", { event: "sc.finite2.end" }),
          event("phase", { event: "sc.departure" }),
          event("kind", { event: "mars.burn1" }),
          event("missing", { event: "nothing" }),
          event("a", { event: "b" }),
          event("b", { event: "a" }),
          event("ok", iso(0)),
          event("focus", iso(0), { focus: { layer: "nope" }, follow: "venus" }),
          event("chained", { event: "ok", offsetS: 5 }),
          event("phases", { event: "mars.capture" }),
        ],
      },
    }),
  );
  assert.deepEqual(errors, [
    "timeline.events[8].id: duplicate \"ok\"",
    "timeline.skipTo.event: no spacecraft or mission \"ghost\"",
    "timeline.events[1].at.event: spacecraft \"sc\" has 2 impulsive burns",
    "timeline.events[2].at.event: spacecraft \"sc\" has 1 finite burns",
    "timeline.events[3].at.event: \"departure\" is a mission event; \"sc\" is a spacecraft",
    "timeline.events[4].at.event: \"burn1\" is a spacecraft event; \"mars\" is a mission",
    "timeline.events[5].at.event: no event \"nothing\"",
    "timeline.events[6].at.event: cycle through \"a\"",
    "timeline.events[7].at.event: cycle through \"b\"",
    "timeline.events[9].actions.focus.layer: no ephemeris layer or spacecraft \"nope\"",
    "timeline.events[9].actions.follow: no mission \"venus\"",
  ]);
});

test("timeline structure: ids without dots, typed actions", () => {
  const errors = errorsOf(
    v({ timeline: { events: [{ id: "a.b", at: iso(0), actions: { timeScale: -5, view: "side" } }] } }),
  );
  assert.deepEqual(errors, [
    "timeline.events[0].id: expected an id without dots, got \"a.b\"",
    "timeline.events[0].actions.timeScale: expected a number or \"max\", got -5",
    "timeline.events[0].actions.view: expected one of globe, heliocentric, got \"side\"",
  ]);
});

// --- Resolution ----------------------------------------------------------------

test("scene events: spacecraft span and burns, mission phases once known", () => {
  const events = sceneEvents(
    { missions: [{ id: "mars", phases: phases({ departure: T0 + 100 }) }] },
    spacecraftSources(),
  );
  const byId = Object.fromEntries(events.map((event) => [event.id, event.unixSeconds]));
  assert.deepEqual(byId, {
    "sc.start": T0,
    "sc.end": T0 + 7200,
    "sc.burn1": T0 + 1800,
    "sc.burn2": T0 + 4800,
    "sc.finite1": T0 + 6000,
    "sc.finite1.end": T0 + 6300,
    "mars.departure": T0 + 100,
    "mars.escape": null,
    "mars.arrival": null,
    "mars.capture": null,
  });
  assert.equal(events.find((event) => event.id === "sc.burn1").label, "Craft: burn 1 · 80.0 m/s");

  // Propagated: the span comes from propagate's info (UTC, no zone).
  const info = { start: "2026-01-01T00:00:10.000", stop: "2026-01-01T01:00:00.000" };
  const [start, end] = sceneEvents({}, new Map([["sc", { propagation: PROPAGATION, display: {}, info }]]));
  assert.deepEqual([start.unixSeconds, end.unixSeconds], [T0 + 10, T0 + 3600]);
});

test("project events resolve through offsets and chains, sorted, unknown last", () => {
  openTimeline(
    v({
      timeline: {
        events: [
          { id: "late", at: { event: "early", offsetS: 600 } },
          { id: "early", label: "Early", at: { event: "sc.burn1", offsetS: -60 } },
          { id: "fixed", at: iso(10) },
          { id: "waiting", at: { event: "mars.capture" } },
        ],
      },
    }),
  );
  const events = timelineEvents({ missions: [{ id: "mars", phases: phases() }] }, spacecraftSources());
  const project = events.filter((event) => event.source === "project");
  assert.deepEqual(
    project.map((event) => [event.id, event.unixSeconds === null ? null : event.unixSeconds - T0, event.label]),
    [
      ["fixed", 10, "fixed"],
      ["early", 1740, "Early"],
      ["late", 2340, "late"],
      ["waiting", null, "waiting"],
    ],
  );
  assert.equal(events.at(-1).unixSeconds, null);
});

// --- Running -------------------------------------------------------------------

const STEPS = {
  clock: { timeScale: 60 },
  frame: "eci",
  timeline: {
    events: [
      { id: "slow", at: iso(100), actions: { timeScale: 10, camera: { lat: 1, lon: 2, altitudeKm: 3 } } },
      { id: "view", at: iso(200), actions: { frame: "ecef" } },
      { id: "hold", at: iso(300), actions: { paused: true } },
    ],
  },
};

test("crossing an event applies its actions; manual control holds until a key changes", () => {
  openTimeline(v(STEPS));
  runTimeline(state(0), new Map());
  assert.deepEqual(sent(), [], "baseline is what the project already applied");

  runTimeline(state(100, { timeScale: -60 }), new Map());
  assert.deepEqual(sent(), [
    ["setTimeScale", -10],
    ["lookAt", 1, 2, 3],
  ]);

  calls.length = 0;
  runTimeline(state(150, { timeScale: -500 }), new Map());
  runTimeline(state(250, { timeScale: -500 }), new Map());
  assert.deepEqual(sent(), [["setFrame", "ecef"]], "rate set by hand is kept; camera only from its own event");

  calls.length = 0;
  runTimeline(state(300), new Map());
  assert.deepEqual(sent(), [["setSunPaused", true]]);

  // Scrubbing back before every event restores the baseline.
  calls.length = 0;
  runTimeline(state(50), new Map());
  assert.deepEqual(sent(), [
    ["setTimeScale", 60],
    ["setSunPaused", false],
    ["setFrame", "eci"],
  ]);
});

test("not directed: nothing applied; directing again applies at the next snapshot", () => {
  openTimeline(v(STEPS));
  setDirected(false);
  runTimeline(state(120), new Map());
  assert.deepEqual(sent(), []);
  setDirected(true);
  runTimeline(state(120), new Map());
  assert.deepEqual(sent(), [
    ["setTimeScale", 10],
    ["lookAt", 1, 2, 3],
  ]);
});

test("a view request repeats until it lands; focus defaults to the layer's first object", () => {
  openTimeline(v({ timeline: { events: [{ id: "a", at: iso(0), actions: { view: "heliocentric", focus: { layer: "sc" } } }] } }));
  runTimeline(state(0), new Map());
  runTimeline(state(1, { view: "transitioning" }), new Map());
  runTimeline(state(2, { view: "globe" }), new Map());
  runTimeline(state(3, { view: "heliocentric" }), new Map());
  runTimeline(state(4, { view: "globe" }), new Map());
  assert.deepEqual(sent(), [
    ["setGncFocus", "sc", 900],
    ["setView", "heliocentric"],
    ["setView", "heliocentric"],
  ]);
});

test("skipTo jumps an earlier clock forward once it resolves, once", () => {
  openTimeline(v({ timeline: { skipTo: { event: "mars.departure", offsetS: -3600 } } }));
  const missions = (departure) => [{ id: "mars", phases: phases({ departure }) }];
  runTimeline(state(0, { missions: missions(null) }), new Map());
  assert.deepEqual(sent(), [], "not known yet");
  runTimeline(state(0, { missions: missions(T0 + 10_000) }), new Map());
  runTimeline(state(0, { missions: missions(T0 + 10_000) }), new Map());
  assert.deepEqual(sent(), [["setClock", T0 + 6400]]);

  calls.length = 0;
  openTimeline(v({ timeline: { skipTo: iso(-10) } }));
  runTimeline(state(0), new Map());
  assert.deepEqual(sent(), [], "a later clock stays");
});

test("missionFlight is a timeline preset", () => {
  openTimeline(v({ missions: [MISSION], director: { type: "missionFlight", mission: "mars", launchLeadSeconds: 100 } }));
  const missions = (times) => [{ id: "mars", phases: phases(times) }];
  const known = { departure: T0 + 1000, escape: T0 + 5000, capture: T0 + 9000 };

  runTimeline(state(0, { missions: missions(known) }), new Map());
  assert.deepEqual(sent(), [["setClock", T0 + 900]]);

  calls.length = 0;
  runTimeline(state(900, { missions: missions(known) }), new Map());
  assert.deepEqual(sent(), [["setTimeScale", 600]]);

  calls.length = 0;
  runTimeline(state(1000, { missions: missions(known) }), new Map());
  assert.deepEqual(sent()[0], ["setTimeScale", 600], "ramp starts");
  assert.ok(sent().slice(1).every(([name, rate]) => name === "setTimeScale" && rate >= 600));

  calls.length = 0;
  runTimeline(state(5000, { timeScale: -1, view: "globe", missions: missions(known) }), new Map());
  assert.deepEqual(sent(), [
    ["setTimeScale", -1e6],
    ["setView", "heliocentric"],
  ]);

  calls.length = 0;
  runTimeline(state(9000, { view: "heliocentric", missions: missions(known) }), new Map());
  assert.deepEqual(sent(), [["setSunPaused", true]]);

  const ids = timelineEvents({ missions: missions(known) }, new Map())
    .filter((event) => event.source === "director")
    .map((event) => event.id);
  assert.deepEqual(ids, ["director-approach", "director-departure", "director-cruise", "director-capture"]);
});

// --- Editing and saving ----------------------------------------------------------

test("edits notify, ids stay unique, and saving drops broken references", () => {
  openTimeline(v({}));
  let edits = 0;
  const unsubscribe = subscribe(() => edits++);
  addEvent({ id: uniqueEventId("burn"), at: { event: "sc.burn1" }, actions: { timeScale: 5 } });
  addEvent({ id: uniqueEventId("burn"), at: { event: "burn", offsetS: 60 } });
  addEvent({ id: "orphan", at: { event: "gone.start" } });
  addEvent({ id: "after-orphan", at: { event: "orphan" } });
  addEvent({ id: "fixed", at: iso(0), actions: { focus: { layer: "gone" } } });
  setDirected(false);
  removeEvent("fixed");
  unsubscribe();
  assert.equal(edits, 7);

  const scene = { spacecraft: new Map([["sc", PROPAGATION]]), missionIds: new Set(), layerIds: new Set(["sc"]) };
  const section = timelineSection(scene);
  assert.deepEqual(section, {
    directed: false,
    events: [
      { id: "burn", at: { event: "sc.burn1" }, actions: { timeScale: 5 } },
      { id: "burn-2", at: { event: "burn", offsetS: 60 } },
    ],
  });
  parseProject(v({ spacecraft: SPACECRAFT, timeline: section }), { warn: assert.fail });

  openTimeline(v({}));
  assert.equal(timelineSection(scene), undefined, "empty and directed: omitted");
  globe.reset();
});
