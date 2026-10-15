/**
 * Data colours (hex; written to project files and sent to the globe).
 * Interface colours are CSS tokens in `styles/app.css`.
 */

/** X/Y/Z, roll/pitch/yaw, R/S/W. */
export const AXIS = ["#ff6b6b", "#6bd66b", "#6b9bff"];

/** Extra chart series beyond the three axes. */
export const SERIES = { amber: "#ffd36b", violet: "#c38bff", pink: "#ff9bd2", sky: "#8ce0ff", teal: "#4fb3d9", ocean: "#2a7fa6" };

/** Default colour cycles, one per layer kind so kinds stay distinguishable. */
export const OVERLAY_COLORS = ["#ff9e3d", "#7fd4ff", "#ffe066", "#ff7aa2", "#9df08a"];
export const EPHEMERIS_COLORS = ["#8ce0ff", "#b9a8ff", "#7ef0c8", "#ffd9f0", "#a8c8ff"];
export const SPACECRAFT_COLORS = ["#ffb86b", "#ff8fa3", "#c3ff7a", "#ffe66b", "#7affea"];

export const VECTOR_COLOR = "#8cd9ff";
export const SENSOR_COLOR = "#ffcc33";
/** Matches `--event` in `styles/app.css`. */
export const STATION_COLOR = "#a6ff6b";
