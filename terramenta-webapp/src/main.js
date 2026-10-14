/**
 * Boot: load the module and the requested project, open it in the workspace,
 * start the globe.
 *
 * Commands queue until `start`, so the first project is applied on the first
 * frame. `layers()`/`limits()` answer from compiled-in tables, so panels build
 * before the renderer exists. Later projects open in place (`workspace.js`):
 * `mountProject` rebuilds the panel; the readout and feature card persist and
 * are shown or hidden per project.
 */

import * as globe from "./globe.js";
import { mountFeature } from "./feature.js";
import { mountFlightPanel } from "./flight-panel.js";
import { mountPanel } from "./panel.js";
import { loadRequestedProject } from "./project.js";
import { mountReadout } from "./readout.js";
import * as workspace from "./workspace.js";

const CANVAS_SELECTOR = "#terramenta";

const status = document.getElementById("status");
const statusMessage = document.getElementById("status-message");
const panelRoot = document.getElementById("panel");
const panelToggle = document.getElementById("panel-toggle");
const readoutRoot = document.getElementById("readout");
const featureRoot = document.getElementById("feature");
const subtitle = document.querySelector("#masthead p");
const shell = document.getElementById("shell");

function fail(html) {
  status.hidden = false;
  status.classList.remove("faded");
  status.querySelector(".spinner")?.remove();
  statusMessage.innerHTML = html;
}

function escape(text) {
  const node = document.createElement("span");
  node.textContent = text;
  return node.innerHTML;
}

function dismissStatus() {
  // First frame lands shortly after `start` resolves; the fade covers the gap.
  setTimeout(() => {
    status.classList.add("faded");
    setTimeout(() => (status.hidden = true), 400);
  }, 300);
}

function setCollapsed(collapsed) {
  document.body.classList.toggle("panel-collapsed", collapsed);
  panelToggle.setAttribute("aria-expanded", String(!collapsed));
}

/** Builds the per-project UI; see `workspace.initWorkspace`. */
function mountProject(readout, feature) {
  return (project, source, signal) => {
    const ui = project.ui ?? {};
    subtitle.textContent = project.name ?? "Untitled";
    panelRoot.replaceChildren();

    const flight = ui.panel === "flight";
    document.body.classList.toggle("layout-flight", flight);
    const panel = flight
      ? mountFlightPanel(panelRoot, {
          missionId: project.director?.mission ?? project.missions?.[0]?.id,
          project,
          source,
          signal,
        })
      : mountPanel(panelRoot, { project, source, tab: ui.tab, signal });

    const showReadout = ui.readout !== false;
    readoutRoot.hidden = !showReadout;
    featureRoot.hidden ||= !showReadout;
    setCollapsed(Boolean(ui.collapsed));

    return {
      sync(state) {
        panel.sync(state);
        if (showReadout) {
          readout.sync(state);
          feature.sync(state);
        }
      },
    };
  };
}

async function boot() {
  if (!globe.isSupported()) {
    fail(
      "This build renders through <strong>WebGPU</strong>, which this browser " +
        "does not expose. Try Chrome or Edge 123+, Safari 26+, or Firefox 141+ " +
        "(on Linux, Firefox still needs <code>dom.webgpu.enabled</code>).",
    );
    return;
  }

  let loaded;
  try {
    [, loaded] = await Promise.all([globe.load(), loadRequestedProject()]);
  } catch (error) {
    console.error(error);
    const detail = error.errors ? error.errors.map(escape).join("<br>") : escape(String(error));
    fail(`Failed to load: <code>${detail}</code><br><a href="${location.pathname}">Start an empty project</a>`);
    return;
  }

  // Clicks on the canvas become pins; see `feature.js`.
  const readout = mountReadout(readoutRoot);
  const feature = mountFeature(featureRoot, document.querySelector(CANVAS_SELECTOR));
  await workspace.initWorkspace({ mount: mountProject(readout, feature) });
  await workspace.open(loaded, { force: true });

  shell.hidden = false;
  panelToggle.addEventListener("click", () =>
    setCollapsed(!document.body.classList.contains("panel-collapsed")),
  );

  try {
    await workspace.start(CANVAS_SELECTOR);
    dismissStatus();
  } catch (error) {
    console.error(error);
    fail(`Failed to start the globe: <code>${escape(String(error))}</code>`);
  }
}

boot();
