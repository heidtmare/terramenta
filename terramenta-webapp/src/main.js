/**
 * Boot: load the module, load the project, build the panel it asks for, queue
 * its scene, start the globe, fan out the state stream.
 *
 * Commands queue until `start`, so the whole project is applied on the first
 * frame. `layers()`/`limits()` answer from compiled-in tables, so panels build
 * before the renderer exists.
 */

import * as globe from "./globe.js";
import { mountFeature } from "./feature.js";
import { mountFlightPanel } from "./flight-panel.js";
import { mountPanel } from "./panel.js";
import { applyProject, directorFor, loadRequestedProject, startOptions } from "./project.js";
import { mountReadout } from "./readout.js";

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

async function boot() {
  if (!globe.isSupported()) {
    fail(
      "This build renders through <strong>WebGPU</strong>, which this browser " +
        "does not expose. Try Chrome or Edge 113+, Safari 26+, or Firefox 141+ " +
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
  const { project, source } = loaded;
  const ui = project.ui ?? {};

  document.title = project.name ? `${project.name} — Terramenta` : "Terramenta";
  subtitle.textContent = project.name ?? "Untitled";

  const listeners = [];
  if (ui.panel === "flight") {
    const missionId = project.director?.mission ?? project.missions?.[0]?.id;
    document.body.classList.add("layout-flight");
    listeners.push(mountFlightPanel(panelRoot, { missionId, project, source }));
  } else {
    listeners.push(mountPanel(panelRoot, { project, source, tab: ui.tab }));
  }

  if (ui.readout === false) {
    readoutRoot.hidden = true;
  } else {
    listeners.push(mountReadout(readoutRoot));
    // Clicks on the canvas become pins; see `feature.js`.
    listeners.push(mountFeature(featureRoot, document.querySelector(CANVAS_SELECTOR)));
  }

  const director = directorFor(project);
  shell.hidden = false;

  panelToggle.addEventListener("click", () =>
    setCollapsed(!document.body.classList.contains("panel-collapsed")),
  );
  if (ui.collapsed) setCollapsed(true);

  // The globe keeps one listener only.
  globe.onState((state) => {
    for (const listener of listeners) listener.sync(state);
    director?.(state);
  });

  // This app draws its own readout; a project's `hud` overrides.
  globe.setHudVisible(false);
  applyProject(project);

  try {
    await globe.start(CANVAS_SELECTOR, startOptions(project));
    dismissStatus();
  } catch (error) {
    console.error(error);
    fail(`Failed to start the globe: <code>${escape(String(error))}</code>`);
  }
}

boot();
