/** Turning numbers from the state snapshot into something readable. */

/** `40.7128° N, 74.0060° W`, the way the globe's own readout writes it. */
export function coordinate(point) {
  if (!point) return "—";
  const ns = point.lat >= 0 ? "N" : "S";
  const ew = point.lon >= 0 ? "E" : "W";
  return `${Math.abs(point.lat).toFixed(4)}° ${ns}, ${Math.abs(point.lon).toFixed(4)}° ${ew}`;
}

/** Kilometres, coarsened as the number grows: metres of precision are noise from orbit. */
export function altitude(km) {
  if (km < 10) return `${km.toFixed(2)} km`;
  if (km < 1000) return `${km.toFixed(0)} km`;
  return `${Math.round(km).toLocaleString()} km`;
}

/**
 * How fast the clock is running, said the way you would say it out loud.
 *
 * `360×` is true but not much use; "1 day / 4 min" is the thing you actually
 * want to know when you are choosing a speed to watch the terminator at.
 *
 * A negative `scale` runs the clock backward — reported the same way, with
 * "reverse" added, since the magnitude is what says how fast either way.
 */
export function timeScale(scale) {
  const secondsPerDay = 86_400;
  const magnitude = Math.abs(scale);
  const realSeconds = secondsPerDay / magnitude;
  const suffix = scale < 0 ? " reverse" : "";
  if (realSeconds < 1) return `${magnitude.toLocaleString()}×${suffix} · ${(1 / realSeconds).toFixed(1)} days / s`;
  if (realSeconds < 90) return `${magnitude.toLocaleString()}×${suffix} · 1 day / ${realSeconds.toFixed(0)} s`;
  if (realSeconds < 5400)
    return `${magnitude.toLocaleString()}×${suffix} · 1 day / ${(realSeconds / 60).toFixed(0)} min`;
  return `${magnitude.toLocaleString()}×${suffix} · 1 day / ${(realSeconds / 3600).toFixed(1)} h`;
}

/**
 * A span of seconds, coarsened as it grows.
 *
 * Used for how stale a layer is and how long until it refreshes, where the
 * difference between 47 and 48 seconds is noise but the difference between
 * seconds and hours is the whole point.
 */
export function duration(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  if (seconds < 90) return `${Math.round(seconds)} s`;
  if (seconds < 5400) return `${Math.round(seconds / 60)} min`;
  if (seconds < 172_800) return `${(seconds / 3600).toFixed(1)} h`;
  return `${Math.round(seconds / 86_400)} days`;
}

/** The simulated clock as a full date, since it can be days from today. */
export function clock(unixSeconds) {
  const date = new Date(unixSeconds * 1000);
  return `${date.toISOString().slice(0, 10)} ${date.toISOString().slice(11, 16)} UTC`;
}

/**
 * Maps a slider's linear position onto a range that spans orders of magnitude.
 *
 * Altitude runs from ~130 km to ~83,000 km and the clock from 1× to 86,400×.
 * Linearly, the entire useful low end of both is crushed into the first
 * pixels of the track; geometrically, every part of the range gets its share.
 */
export const logScale = {
  toValue: (position, min, max) => min * (max / min) ** position,
  toPosition: (value, min, max) => Math.log(value / min) / Math.log(max / min),
};
