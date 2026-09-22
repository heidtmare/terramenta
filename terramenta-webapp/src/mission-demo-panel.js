/**
 * The mission-planning demo's control surface: the clock, the one mission
 * running, and a few quick looks — not a trimmed copy of `panel.js`, but
 * built from the same pieces (`dom.js`, `widgets.js`) since this page has no
 * imagery, overlays, satellites or reference-frame switch to show.
 *
 * Follows the same one-way rule every control in this app does: a control
 * sends a command and forgets it, and a state snapshot is what sets it back
 * — see `panel.js`'s own docs for why. The one exception is which way the
 * clock runs: the globe reports only a signed `timeScale`, so the
 * forward/backward choice and the speed slider both derive from its sign and
 * magnitude rather than carrying independent state of their own.
 */

import { el, row, section } from "./dom.js";
import { clock, timeScale } from "./format.js";
import * as globe from "./globe.js";
import { button, choice, slider, toggle } from "./widgets.js";

const PHASE_IDS = ["departure", "escape", "arrival", "capture"];

export function mountMissionDemoPanel(root, missionId) {
  const limits = globe.limits();
  const controls = [];

  const bind = (node, sync, isHeld = () => false) => {
    controls.push({ sync, isHeld });
    return node;
  };

  // The clock's magnitude and direction are read from `state.sun.timeScale`'s
  // sign; kept here too so a direction click can be applied against whatever
  // speed the slider was last set to, without waiting for the next snapshot.
  let direction = "forward";
  let speed = limits.minTimeScale;

  const applyRate = () => {
    globe.setTimeScale(direction === "backward" ? -speed : speed);
  };

  // --- Clock -----------------------------------------------------------

  const pauseToggle = toggle("Run the clock", (running) => globe.setSunPaused(!running));
  bind(pauseToggle.node, (state) => pauseToggle.set(!state.sun.paused));

  const directionChoice = choice(
    [
      ["forward", "Forward"],
      ["backward", "Backward"],
    ],
    (value) => {
      direction = value;
      applyRate();
    },
  );
  bind(directionChoice.node, (state) => {
    direction = state.sun.timeScale < 0 ? "backward" : "forward";
    directionChoice.set(direction);
  });

  const rate = slider({
    min: limits.minTimeScale,
    max: limits.maxTimeScale,
    format: (value) => timeScale(direction === "backward" ? -value : value),
    onInput: (value) => {
      speed = value;
      applyRate();
    },
  });
  bind(
    rate.node,
    (state) => {
      speed = Math.abs(state.sun.timeScale);
      rate.set(speed);
    },
    rate.isHeld,
  );

  const clockSection = section(
    "Clock",
    pauseToggle.node,
    row("Direction", directionChoice.node),
    rate.node,
  );

  // --- Mission -----------------------------------------------------------

  const status = el("p", { class: "detail" }, "Searching for a launch window…");
  const phaseButtons = new Map(
    PHASE_IDS.map((id) => [
      id,
      {
        unixSeconds: null,
        node: button("", () => {
          const entry = phaseButtons.get(id);
          if (entry?.unixSeconds != null) globe.setClock(entry.unixSeconds);
        }),
      },
    ]),
  );
  for (const entry of phaseButtons.values()) entry.node.disabled = true;

  bind(status, (state) => {
    const mission = state.missions.find((mission) => mission.id === missionId);
    if (!mission) return;
    status.textContent = describeMission(mission);
    for (const phase of mission.phases) {
      const entry = phaseButtons.get(phase.id);
      if (!entry) continue;
      entry.unixSeconds = phase.unixSeconds;
      entry.node.textContent = phase.label;
      entry.node.disabled = phase.unixSeconds == null;
      entry.node.classList.toggle("selected", phase.reached);
    }
  });

  const missionSection = section(
    "Mission — Earth → Mars",
    status,
    el("div", { class: "buttons" }, ...Array.from(phaseButtons.values(), (entry) => entry.node)),
  );

  // --- Look at -------------------------------------------------------------

  const anchorSection = section(
    "Look at",
    choice(
      [
        ["sun", "Sun"],
        ["earth", "Earth"],
        ["mars", "Mars"],
        ["barycenter", "Barycenter"],
      ],
      globe.setHeliocentricAnchor,
    ).node,
  );

  root.append(clockSection, missionSection, anchorSection);

  return {
    sync(state) {
      for (const control of controls) {
        if (!control.isHeld()) control.sync(state);
      }
    },
  };
}

/** The line under the mission's name: what it found, and where it is with it. */
function describeMission(mission) {
  switch (mission.status) {
    case "searching":
      return "Searching for a launch window…";
    case "waiting":
      return (
        `Launch window found — departs ${clock(mission.departureUnixSeconds)}, ` +
        `arrives ${clock(mission.arrivalUnixSeconds)}.`
      );
    case "enroute":
      return `En route since ${clock(mission.departureUnixSeconds)}, now orbiting ${mission.orbiting}.`;
    case "arrived":
      return `Arrived at ${mission.destination} on ${clock(mission.arrivalUnixSeconds)}.`;
    case "failed":
      return `Failed to find a window — ${mission.reason}.`;
    default:
      return "—";
  }
}
