import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import {
  STATION_LAYER,
  addStation,
  eventOptions,
  eventTimeline,
  eventsSection,
  openEvents,
  removeStation,
  setEventOptions,
  spacecraftEvents,
  subscribe,
  uniqueStationId,
} from "../src/events.js";
import * as globe from "../src/globe.js";
import { PROJECT_VERSION, ProjectError, applyProject, captureProject, parseProject } from "../src/project.js";
import { openTimeline, timelineEvents, timelineSection } from "../src/timeline.js";
import { attachFake } from "./fake-globe.js";

globalThis.document = { baseURI: "http://localhost:8080/app/" };

const T0 = Date.parse("2026-01-01T00:00:00Z") / 1000;
const v = (fields) => ({ terramenta: PROJECT_VERSION, ...fields });
const PROPAGATION = {
  initial: { epoch: "2026-01-01T00:00:00Z", positionKm: [6878, 0, 0], velocityKmS: [0, 4.7, 6] },
  durationS: 21_600,
};
const STATIONS = [
  { id: "wallops", label: "Wallops", latDeg: 37.94, lonDeg: -75.46, minElevationDeg: 5 },
  { id: "svalbard", latDeg: 78.23, lonDeg: 15.41 },
];

let calls;
beforeEach(() => {
  calls = attachFake();
  openEvents(v({}));
  calls.length = 0;
});

test("stations draw as one unsaved, untracked overlay; removing the last removes it", () => {
  let dirty = 0;
  globe.onSceneChange(() => dirty++);
  openEvents(v({ groundStations: STATIONS }));
  const [name, id, options] = calls.at(-1);
  assert.deepEqual([name, id], ["addOverlay", STATION_LAYER]);
  const features = JSON.parse(options.text).features;
  assert.deepEqual(
    features.map((feature) => [feature.properties.title, feature.geometry.coordinates]),
    [["Wallops", [-75.46, 37.94]], ["svalbard", [15.41, 78.23]]],
  );
  removeStation("wallops");
  removeStation("svalbard");
  assert.deepEqual(calls.at(-1), ["removeOverlay", STATION_LAYER]);
  assert.equal(dirty, 0, "drawn quietly");
  globe.onSceneChange(null);
});

test("edits notify; options keep only non-defaults; ids slug and stay unique", () => {
  let notified = 0;
  const unsubscribe = subscribe(() => notified++);
  addStation(STATIONS[0]);
  addStation({ ...STATIONS[0], minElevationDeg: 10 });
  setEventOptions({ eclipses: false, apsides: true });
  setEventOptions({ apsides: false });
  unsubscribe();
  assert.equal(notified, 4);
  assert.deepEqual(eventsSection(), {
    groundStations: [{ ...STATIONS[0], minElevationDeg: 10 }],
    events: { eclipses: false },
  });
  assert.equal(eventOptions().stepS, 30);
  assert.equal(uniqueStationId("Wallops"), "wallops-2");
  assert.equal(uniqueStationId("Kiruna (ESA)"), "kiruna-esa");
  openEvents(v({}));
  assert.deepEqual(eventsSection(), {});
});

test("searches pass stations and options, cache until an edit", () => {
  openEvents(v({ groundStations: STATIONS, events: { apsides: true, stepS: 20 } }));
  globe.addSpacecraft("sat", PROPAGATION, {});
  calls.length = 0;
  const first = spacecraftEvents("sat");
  assert.equal(spacecraftEvents("sat"), first, "cached");
  const searches = calls.filter(([name]) => name === "findEvents");
  assert.equal(searches.length, 1);
  assert.deepEqual(searches[0][2], {
    stations: [
      { id: "wallops", latDeg: 37.94, lonDeg: -75.46, altKm: 0, minElevationDeg: 5 },
      { id: "svalbard", latDeg: 78.23, lonDeg: 15.41, altKm: 0, minElevationDeg: 0 },
    ],
    eclipses: true,
    apsides: true,
    nodes: false,
    stepS: 20,
  });
  assert.equal(first.contacts.length, 4);
  setEventOptions({ eclipses: false });
  assert.notEqual(spacecraftEvents("sat"), first);
  assert.equal(spacecraftEvents("missing"), null);
});

test("timeline: numbered per kind, referenceable, unknown station or spacecraft refused", () => {
  openEvents(v({ groundStations: STATIONS, events: { apsides: true } }));
  globe.addSpacecraft("sat", PROPAGATION, { label: "Sat" });
  const timeline = eventTimeline("sat", "Sat");
  assert.deepEqual(
    timeline.map((event) => [event.id, event.unixSeconds - T0]),
    [
      ["sat.aos.wallops.1", 600],
      ["sat.los.wallops.1", 1020],
      ["sat.aos.wallops.2", 6000],
      ["sat.los.wallops.2", 6420],
      ["sat.aos.svalbard.1", 1200],
      ["sat.los.svalbard.1", 1620],
      ["sat.aos.svalbard.2", 6600],
      ["sat.los.svalbard.2", 7020],
      ["sat.penumbra1", 1990],
      ["sat.penumbra1.end", 4210],
      ["sat.umbra1", 2000],
      ["sat.umbra1.end", 4200],
      ["sat.periapsis1", 100],
    ],
  );
  assert.equal(timeline[0].label, "Sat: AOS Wallops · max 30°");

  const project = v({
    spacecraft: [{ id: "sat", propagation: PROPAGATION }],
    groundStations: STATIONS,
    timeline: { events: [{ id: "slow", at: { event: "sat.aos.svalbard.2", offsetS: -60 }, actions: { timeScale: 10 } }] },
  });
  assert.doesNotThrow(() => parseProject(project));
  openTimeline(project);
  const slow = timelineEvents({ missions: [] }).find((event) => event.id === "slow");
  assert.equal(slow.unixSeconds, T0 + 6540);
  assert.ok(timelineEvents({ missions: [] }).some((event) => event.source === "event"));

  for (const [event, message] of [
    ["sat.aos.kiruna.1", 'no ground station "kiruna"'],
    ["other.umbra1", 'no spacecraft "other"'],
  ]) {
    const bad = structuredClone(project);
    bad.timeline.events[0].at.event = event;
    assert.throws(() => parseProject(bad), (error) => error instanceof ProjectError && error.message.includes(message));
  }
  // Saving keeps a reference to a station still present, drops one that is gone.
  const saved = timelineSection({ spacecraft: new Map([["sat", PROPAGATION]]), stationIds: new Set(["svalbard"]), missionIds: new Set(), layerIds: new Set() });
  assert.equal(saved.events.length, 1);
  assert.equal(
    timelineSection({ spacecraft: new Map([["sat", PROPAGATION]]), stationIds: new Set(), missionIds: new Set(), layerIds: new Set() }),
    undefined,
  );
});

test("projects: duplicate station ids and the reserved overlay id refused", () => {
  assert.throws(
    () => parseProject(v({ groundStations: [STATIONS[0], STATIONS[0]] })),
    /groundStations\[1\]\.id: duplicate "wallops"/,
  );
  assert.throws(
    () => parseProject(v({ overlays: { layers: [{ id: STATION_LAYER, text: "{}" }] } })),
    /reserved for ground stations/,
  );
  assert.throws(() => parseProject(v({ groundStations: [{ id: "a.b", latDeg: 0, lonDeg: 0 }] })), /groundStations\[0\]\.id/);
});

test("capture saves stations and options, not their overlay", () => {
  openEvents(v({ groundStations: STATIONS, events: { nodes: true } }));
  applyProject(v({}));
  const state = sceneState([{ id: STATION_LAYER, label: "Ground stations" }]);
  const project = captureProject(state, { name: "Test" });
  assert.deepEqual(project.groundStations, STATIONS);
  assert.deepEqual(project.events, { nodes: true });
  assert.deepEqual(project.overlays.layers, []);
  assert.doesNotThrow(() => parseProject(project));
});

function sceneState(overlays) {
  const style = { pointColor: "#fff", pointSizePx: 4, lineColor: "#fff", lineWidthPx: 1, fillColor: "#fff" };
  return {
    view: "globe",
    frame: { mode: "ecef" },
    sun: { unixSeconds: T0, timeScale: 1, paused: false, shaded: true },
    camera: { center: { lat: 0, lon: 0 }, altitudeKm: 20_000 },
    imagery: { enabled: true, layerIndex: 0 },
    vectorTiles: { enabled: false, layerIndex: 0, style: style },
    overlays: {
      enabled: true,
      picking: false,
      layers: overlays.map((layer) => ({
        ...layer,
        url: null,
        visible: true,
        simpleStyle: true,
        altitude: { mode: "clampToSurface", scale: 1, extrude: false },
        style,
      })),
    },
    ephemerides: { enabled: true, layers: [] },
    missions: [],
    placemarks: { enabled: true },
    gnc: { enabled: false, eciAxes: false, ecefAxes: false, graticule: false, graticuleStepDeg: 15, sidereal: false, track: false, trackOrbits: 1 },
    hud: { visible: false, helpVisible: false },
    keyboard: true,
  };
}
