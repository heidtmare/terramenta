/**
 * Boot: put the interface up, start the globe, join the two together.
 *
 * The order matters and is the part worth copying. The panel is built from the
 * catalogue as soon as the module has loaded, before the renderer has drawn
 * anything — `layers()` and `limits()` answer from compiled-in tables, and
 * commands queue until there is a globe to apply them to. The globe is started
 * last, and the state stream is what brings the interface to life.
 */

import * as globe from "./globe.js";
import { DEFAULT_FEED } from "./feeds.js";
import { addFeed } from "./overlays.js";
import { mountPanel } from "./panel.js";
import { mountReadout } from "./readout.js";

const CANVAS_SELECTOR = "#terramenta";

const status = document.getElementById("status");
const statusMessage = document.getElementById("status-message");
const panelRoot = document.getElementById("panel");
const panelToggle = document.getElementById("panel-toggle");
const readoutRoot = document.getElementById("readout");
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

  const panel = mountPanel(panelRoot);
  const readout = mountReadout(readoutRoot);
  shell.hidden = false;

  // Collapsing the panel is worth having for its own sake on a phone, and for
  // seeing the globe unobstructed on anything else.
  panelToggle.addEventListener("click", () => {
    const collapsed = document.body.classList.toggle("panel-collapsed");
    panelToggle.setAttribute("aria-expanded", String(!collapsed));
  });

  // One listener, fanned out here: the globe keeps only the most recent one, so
  // registering twice would silently disconnect the first.
  globe.onState((state) => {
    panel.sync(state);
    readout.sync(state);
  });

  // This app draws its own readout, so the globe's is switched off — the panel
  // can turn it back on, which is the quickest way to see they agree.
  globe.setHudVisible(false);

  // A live GeoJSON feed, up before the first frame is drawn. Queued like every
  // other command, so the fetch starts as soon as the globe does — and it
  // refreshes itself from then on, which is the whole point of the feature and
  // not something an empty panel would ever show.
  addFeed(DEFAULT_FEED);

  try {
    await globe.start(CANVAS_SELECTOR);
    dismissStatus();
  } catch (error) {
    console.error(error);
    fail(`Failed to start the globe: <code>${error}</code>`);
  }
}

boot();
