/**
 * Directors: scripted view/clock control driven by the state stream, selected
 * by a project's `director.type`.
 *
 * Each entry: `validate(config, {missionIds})` → reference errors (paths
 * relative to `director`), and `create(config)` → `(state) => void`. Fields are
 * typed in `schema/project.schema.json` (`$defs/<type>`, listed in
 * `$defs/director`).
 */

import * as globe from "./globe.js";

/**
 * `missionFlight`: flies the camera and clock through one mission.
 *
 * - `waiting`: clock to `launchLeadSeconds` before departure at `departureTimeScale`.
 * - Orbiting origin: rate doubles every `departureDoublingSeconds` (wall clock) up to max.
 * - Heliocentric cruise: max rate, heliocentric view.
 * - Orbiting destination: pause.
 *
 * Rates keep the sign of the current time scale, so reverse playback works.
 * The view is re-requested each snapshot until it lands (requests made
 * mid-transition are dropped).
 */
const missionFlight = {
  defaults: { launchLeadSeconds: 3_600, departureTimeScale: 600, departureDoublingSeconds: 2 },

  validate(config, { missionIds }) {
    return missionIds.has(config.mission) ? [] : [`mission: no mission "${config.mission}"`];
  },

  create(config) {
    const { mission: missionId, launchLeadSeconds, departureTimeScale, departureDoublingSeconds } = {
      ...this.defaults,
      ...config,
    };
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
        globe.setClock(mission.departureUnixSeconds - launchLeadSeconds);
        globe.setTimeScale(sign * departureTimeScale);
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
        const doublings = (performance.now() - rampStartedAt) / 1000 / departureDoublingSeconds;
        globe.setTimeScale(sign * Math.min(departureTimeScale * 2 ** doublings, cruiseTimeScale));
      }

      const view = leg == null || leg === mission.origin ? "globe" : "heliocentric";
      if (state.view !== view && state.view !== "transitioning") globe.setView(view);
    };
  },
};

export const DIRECTORS = { missionFlight };
