/**
 * Something to overlay.
 *
 * A GeoJSON layer control is only worth trying if there is a URL to hand, and
 * these are picked to show what the layer does rather than just that it works:
 * a feed that changes while you watch it, one dense enough to cover the globe,
 * and one sparse enough to read a single marker in.
 *
 * All three are USGS earthquake feeds, which are as good a demonstration as
 * they are convenient — they are public, they send
 * `Access-Control-Allow-Origin: *`, so the browser will let the globe fetch
 * them, and they are genuinely live.
 */

export const FEEDS = [
  {
    id: "quakes-hour",
    label: "Earthquakes · past hour",
    url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson",
    // The feed itself is rebuilt about every minute, so there is nothing to be
    // gained by asking more often than that.
    refreshSeconds: 60,
  },
  {
    id: "quakes-day",
    label: "Earthquakes · past day",
    url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_day.geojson",
    refreshSeconds: 300,
  },
  {
    id: "quakes-significant",
    label: "Significant · past month",
    url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/significant_month.geojson",
    refreshSeconds: 900,
  },
];

/** The one the app puts up on its own, so the feature is visible on arrival. */
export const DEFAULT_FEED = FEEDS[0];
