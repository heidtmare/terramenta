/**
 * Boot for the mission-planning demo page.
 *
 * Same order as `main.js`: load the module, build the interface from the
 * catalogue, queue the demo mission, start the globe, join the state stream.
 * The differences are all in what this page asks the globe to do — keep every
 * Earth-specific layer off rather than trimming a panel down to hide them,
 * lock the camera onto the demo's spacecraft from the start, and let
 * `directMission` steer the view and the clock through the flight.
 */

import * as globe from "./globe.js";
import { mountMissionDemoPanel } from "./mission-demo-panel.js";

const CANVAS_SELECTOR = "#terramenta";
const DEMO_MISSION_ID = "demo";

/** How long before departure the clock is set to once a window is found. */
const LAUNCH_LEAD_SECONDS = 3_600;
/**
 * The clock's rate as the spacecraft leaves its parking orbit — slow enough
 * that the departure burn reads, about an Earth radius a second — and how
 * often it doubles from there, until the escape is a quarter-minute or so of
 * watching Earth fall away rather than several.
 */
const DEPARTURE_TIME_SCALE = 600;
const DEPARTURE_DOUBLING_SECONDS = 2;

const status = document.getElementById("status");
const statusMessage = document.getElementById("status-message");
const panelRoot = document.getElementById("panel");
const panelToggle = document.getElementById("panel-toggle");
const shell = document.getElementById("shell");

function fail(html) {
  status.hidden = false;
  status.classList.remove("faded");
  status.querySelector(".spinner")?.remove();
  statusMessage.innerHTML = html;
}

/**
 * Steers the demo through the flight off the mission's own reports, one leg
 * per body the spacecraft orbits: the globe view while it climbs out of its
 * parking orbit around the origin, with the clock ramping up from
 * `DEPARTURE_TIME_SCALE`; the heliocentric view once it has escaped, with the
 * clock at its fastest for the cruise; and the clock paused on capture at the
 * destination, which is where the demo ends. The wait for the launch window is skipped
 * outright, once.
 *
 * Rates are set on entering a leg, so the clock controls are the viewer's
 * again for the rest of it — except the departure ramp, which runs until the
 * escape. Rates keep whichever direction the clock is already running in, so
 * running it back through the flight replays the legs in reverse, and a
 * mission jump button lands in the right view. The view itself is asked for
 * on every snapshot until it has landed, since a request made mid-transition
 * is dropped.
 */
function directMission(missionId) {
  const cruiseTimeScale = globe.limits().maxTimeScale;
  let skippedWait = false;
  let leg;
  let rampStartedAt = null;

  return (state) => {
    const mission = state.missions.find((mission) => mission.id === missionId);
    if (!mission) return;
    const sign = state.sun.timeScale < 0 ? -1 : 1;

    if (mission.status === "waiting" && !skippedWait) {
      skippedWait = true;
      globe.setClock(mission.departureUnixSeconds - LAUNCH_LEAD_SECONDS);
      globe.setTimeScale(sign * DEPARTURE_TIME_SCALE);
    }

    const orbiting =
      mission.status === "enroute" || mission.status === "arrived" ? mission.orbiting : null;
    if (orbiting !== leg) {
      leg = orbiting;
      rampStartedAt = null;
      if (leg === mission.origin) {
        rampStartedAt = performance.now();
      } else if (leg === mission.destination) {
        globe.setSunPaused(true);
      } else if (leg) {
        globe.setTimeScale(sign * cruiseTimeScale);
      }
    }

    if (rampStartedAt != null) {
      const doublings = (performance.now() - rampStartedAt) / 1000 / DEPARTURE_DOUBLING_SECONDS;
      const rate = Math.min(DEPARTURE_TIME_SCALE * 2 ** doublings, cruiseTimeScale);
      globe.setTimeScale(sign * rate);
    }

    // Around the origin (or before launch) is the globe's; anywhere else —
    // the cruise, and the destination — is the heliocentric view's.
    const view = leg == null || leg === mission.origin ? "globe" : "heliocentric";
    if (state.view !== view && state.view !== "transitioning") globe.setView(view);
  };
}

function dismissStatus() {
  // The first frame lands a moment after `start` resolves, so the fade covers
  // the gap rather than exposing a black canvas between the two.
  setTimeout(() => {
    status.classList.add("faded");
    setTimeout(() => (status.hidden = true), 400);
  }, 300);
}

async function boot() {
  if (!globe.isSupported()) {
    fail(
      "This build renders through <strong>WebGPU</strong>, which this browser " +
        "does not expose. Try Chrome or Edge 113+, Safari 26+, or Firefox 141+ " +
        "(on Linux, Firefox still needs <code>dom.webgpu.enabled</code>).",
    );
    return;
  }

  try {
    await globe.load();
  } catch (error) {
    console.error(error);
    fail(`Failed to load the globe: <code>${error}</code>`);
    return;
  }

  const panel = mountMissionDemoPanel(panelRoot, DEMO_MISSION_ID);
  shell.hidden = false;

  panelToggle.addEventListener("click", () => {
    const collapsed = document.body.classList.toggle("panel-collapsed");
    panelToggle.setAttribute("aria-expanded", String(!collapsed));
  });

  const direct = directMission(DEMO_MISSION_ID);
  globe.onState((state) => {
    panel.sync(state);
    direct(state);
  });

  // This page draws its own controls, so the globe's own HUD is switched off.
  globe.setHudVisible(false);

  // The globe is only ever the backdrop to the departure here, so nothing
  // Earth-specific is drawn on it — just the base image. The departure for
  // the heliocentric view drops these anyway; switching them off up front
  // keeps them from streaming in under a camera that is following the
  // spacecraft rather than looking at the ground.
  globe.setImageryEnabled(false);
  globe.setVectorTilesEnabled(false);
  globe.setOverlaysEnabled(false);
  globe.setEphemeridesEnabled(false);
  globe.setPlacemarksEnabled(false);

  // Queued like every other command: this lands on the globe's first tick,
  // same as `main.js`'s own layers queued before `start()` resolves.
  globe.addMission(DEMO_MISSION_ID, { origin: "Earth", destination: "Mars" });
  // Taken out before launch; the camera picks the spacecraft up as it
  // appears, and the same lock carries on into the heliocentric view.
  globe.followMission(DEMO_MISSION_ID);

  try {
    await globe.start(CANVAS_SELECTOR);
    dismissStatus();
  } catch (error) {
    console.error(error);
    fail(`Failed to start the globe: <code>${error}</code>`);
  }
}

boot();
