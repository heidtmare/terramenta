/**
 * Something to overlay.
 *
 * Two kinds. One is the sample shipped beside this app, which exists to show
 * every geometry a GeoJSON document can hold; the others are live USGS
 * earthquake feeds, which are as good a demonstration as they are convenient —
 * they are public, they send `Access-Control-Allow-Origin: *`, so the browser
 * will let the globe fetch them, and they change while you watch.
 *
 * The live ones are picked to show what the layer does rather than just that it
 * works: a feed that changes while you watch it, one dense enough to cover the
 * globe, and one sparse enough to read a single marker in.
 */

/**
 * The sample document, as a URL rather than as text.
 *
 * Absolute, and resolved against wherever the page was served from: the globe
 * fetches a layer through its own asset source, which speaks `http` and `https`
 * and nothing else — a path relative to the page would mean nothing to it. It
 * is same-origin either way, so the browser asks no questions.
 */
export const SAMPLE = {
  id: "geometry-tour",
  label: "Geometry tour · every shape",
  // Resolved when it is asked for rather than when this module loads, so that
  // reading the catalogue does not require a page to be standing yet.
  get url() {
    return new URL("data/geometry-tour.geojson", document.baseURI).href;
  },
  // A file on disk that nothing is writing to. Refetching it would only ask the
  // same question again.
  refreshSeconds: null,
};

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

/** The live one the app puts up on its own, alongside [`SAMPLE`]. */
export const DEFAULT_FEED = FEEDS[0];

/** Everything the panel offers as a button, sample first. */
export const CATALOGUE = [SAMPLE, ...FEEDS];
