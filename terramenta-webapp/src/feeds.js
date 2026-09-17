/**
 * Something to overlay.
 *
 * Two kinds. Three are samples shipped beside this app — every geometry a
 * GeoJSON document can hold, a ring extruded into a box, and a document that
 * styles its own features the simplestyle way; the others are live USGS
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

/**
 * A box standing on the equator, which is one square ring plus `extrude`.
 *
 * Its own layer rather than another feature of the tour, because extrude is a
 * per-layer setting and the tour must not have it: the airspace in there is
 * meant to float, and walling its shelves to the ground would bury the shape
 * they make.
 */
export const CUBE = {
  id: "extruded-cube",
  label: "Extruded cube · walls to the ground",
  get url() {
    return new URL("data/extruded-cube.geojson", document.baseURI).href;
  },
  refreshSeconds: null,
  options: {
    extrude: true,
    // Denser than the default fill: a box reads as a solid rather than as four
    // panes of glass, and there is enough of it on screen to take the weight.
    fillAlpha: "8c",
  },
};

/**
 * A board of examples, each labelled with the simplestyle member it is
 * demonstrating.
 *
 * Its own layer for the same reason the cube has one: what it is showing is a
 * per-layer setting seen from the other side. The tour next door says nothing
 * about how it wants to look, so it comes out in whatever colour the panel
 * gives it; this one says a great deal, and comes out in its own — except for
 * the controls dotted through it, which are there to be compared against the
 * rest when the layer's colour is changed.
 *
 * Laid out over empty ocean on purpose. A swatch card is not a place, and the
 * colours are easier to judge against one background than against five.
 */
export const STYLED = {
  id: "simplestyle-examples",
  label: "simplestyle · a document with opinions",
  get url() {
    return new URL("data/simplestyle-examples.geojson", document.baseURI).href;
  },
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

/** Everything the panel offers as a button, samples first. */
export const CATALOGUE = [SAMPLE, CUBE, STYLED, ...FEEDS];
