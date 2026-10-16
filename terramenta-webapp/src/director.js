/**
 * Directors: timeline presets, selected by a project's `director.type`.
 *
 * Each entry: `validate(config, {missionIds})` → reference errors (paths
 * relative to `director`), and `events(config)` → `{events, skipTo?}` in
 * `timeline` form, run by `timeline.js` alongside the project's own events.
 * Fields are typed in `schema/project.schema.json` (`$defs/<type>`, listed in
 * `$defs/director`).
 */

/**
 * `missionFlight`: flies the clock and view through one mission.
 *
 * - Skip to `launchLeadSeconds` before departure; globe view at `departureTimeScale`.
 * - Departure: the rate doubles every `departureDoublingSeconds` (wall clock) up to max.
 * - Escape: max rate, heliocentric view.
 * - Flyby (missions with `via`; events never resolve otherwise): face the flyby
 *   plane and zoom in over the 3 days before periapsis, `flybyTimeScale` ±8 h
 *   about it, back out after.
 * - Capture: pause.
 */
const HOUR = 3_600;
const DAY = 86_400;
/** The globe's heliocentric follow distance. */
const FOLLOW_DISTANCE_AU = 1.5;

const missionFlight = {
  defaults: { launchLeadSeconds: 3_600, departureTimeScale: 600, departureDoublingSeconds: 2, flybyTimeScale: 900 },

  validate(config, { missionIds }) {
    return missionIds.has(config.mission) ? [] : [`mission: no mission "${config.mission}"`];
  },

  events(config) {
    const { mission, launchLeadSeconds, departureTimeScale, departureDoublingSeconds, flybyTimeScale } = {
      ...this.defaults,
      ...config,
    };
    const at = (phase, offsetS) => ({ event: `${mission}.${phase}`, ...(offsetS ? { offsetS } : {}) });
    const approach = at("departure", -launchLeadSeconds);
    return {
      skipTo: approach,
      events: [
        {
          id: "director-approach",
          label: "Launch approach",
          at: approach,
          actions: { timeScale: departureTimeScale, view: "globe" },
        },
        {
          id: "director-departure",
          label: "Departure ramp",
          at: at("departure"),
          actions: { timeScale: departureTimeScale, rampDoublingS: departureDoublingSeconds },
        },
        { id: "director-cruise", label: "Cruise", at: at("escape"), actions: { timeScale: "max", view: "heliocentric" } },
        {
          id: "director-flyby-approach",
          label: "Flyby approach",
          at: at("flyby", -3 * DAY),
          actions: { timeScale: 8 * flybyTimeScale, helioDistanceAu: 0.03, faceFlyby: mission },
        },
        {
          id: "director-flyby",
          label: "Flyby",
          at: at("flyby", -8 * HOUR),
          actions: { timeScale: flybyTimeScale, helioDistanceAu: 0.0015 },
        },
        {
          id: "director-flyby-depart",
          label: "Flyby departure",
          at: at("flyby", 8 * HOUR),
          actions: { timeScale: 8 * flybyTimeScale, helioDistanceAu: 0.03 },
        },
        {
          id: "director-flyby-cruise",
          label: "Cruise after flyby",
          at: at("flyby", 3 * DAY),
          actions: { timeScale: "max", helioDistanceAu: FOLLOW_DISTANCE_AU },
        },
        { id: "director-capture", label: "Hold at capture", at: at("capture"), actions: { paused: true } },
      ],
    };
  },
};

export const DIRECTORS = { missionFlight };
