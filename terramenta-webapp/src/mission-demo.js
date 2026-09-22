/**
 * Boot for the mission-planning demo page.
 *
 * Same order as `main.js`: load the module, build the interface from the
 * catalogue, queue the demo mission, start the globe, join the state stream.
 * The differences are all in what this page asks the globe to do — start
 * already in the heliocentric view rather than the globe, and keep every
 * Earth-specific layer off rather than trimming a panel down to hide them.
 */

import * as globe from "./globe.js";
import { mountMissionDemoPanel } from "./mission-demo-panel.js";

const CANVAS_SELECTOR = "#terramenta";
const DEMO_MISSION_ID = "demo";

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

  globe.onState((state) => panel.sync(state));

  // This page draws its own controls, so the globe's own HUD is switched off.
  globe.setHudVisible(false);

  // Nothing Earth-specific is ever shown on this page. Starting in the
  // heliocentric view already hides the globe, its imagery and its
  // placemarks; these are belt-and-braces on top of that, in case a layer is
  // ever turned on elsewhere and its setting carries over.
  globe.setImageryEnabled(false);
  globe.setVectorTilesEnabled(false);
  globe.setOverlaysEnabled(false);
  globe.setEphemeridesEnabled(false);

  // Queued like every other command: this lands on the globe's first tick,
  // same as `main.js`'s own layers queued before `start()` resolves.
  globe.addMission(DEMO_MISSION_ID, { origin: "Earth", destination: "Mars" });

  try {
    await globe.start(CANVAS_SELECTOR, { view: "heliocentric" });
    dismissStatus();
  } catch (error) {
    console.error(error);
    fail(`Failed to start the globe: <code>${error}</code>`);
  }
}

boot();
