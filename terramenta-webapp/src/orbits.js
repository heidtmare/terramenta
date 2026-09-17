/**
 * Something to propagate.
 *
 * All of these are [Celestrak's][celestrak] general-perturbations endpoint,
 * asked for OMM JSON. It is the right one to wire up for the same reasons the
 * USGS feeds are in `feeds.js`: it is public and keyless, it answers
 * `Access-Control-Allow-Origin: *` so the browser will let the globe fetch it,
 * and the elements behind it are regenerated several times a day from real
 * observations — so what is drawn is where these objects actually are, not
 * where a canned file says they were.
 *
 * The groups are picked to show different *kinds* of orbit rather than more of
 * the same one, because the shape of an orbit is most of what an ephemeris
 * layer has to show. A low circular one whips around the planet in an hour and
 * a half; a geostationary one hangs still over a point on the equator, which in
 * ECI is a satellite tracing a circle and in ECEF is a satellite that does not
 * appear to move at all; a Molniya one is a long ellipse that loiters over the
 * north for most of a half-day and then falls past the Earth in an hour.
 *
 * `refreshSeconds` is `null` on all of them. A catalogue is regenerated a few
 * times a day, so refetching while you watch shows nothing — and the globe
 * floors the period at five minutes anyway. The panel has a switch for it
 * because leaving a globe up for an afternoon is a real thing to do.
 *
 * [celestrak]: https://celestrak.org/NORAD/documentation/gp-data-formats.php
 */

/** Celestrak's GP endpoint, asked for one group as OMM JSON. */
function group(name) {
  return `https://celestrak.org/NORAD/elements/gp.php?GROUP=${name}&FORMAT=json`;
}

export const CATALOGUES = [
  {
    id: "stations",
    label: "Crewed stations",
    url: group("stations"),
    refreshSeconds: null,
    // Twenty-odd objects, two of which are the only places anyone is currently
    // living. Small enough that every one of them gets a trail and the sample
    // budget still draws each orbit smoothly.
    options: {},
  },
  {
    id: "gps",
    label: "GPS · medium orbit",
    url: group("gps-ops"),
    refreshSeconds: null,
    // Thirty-one satellites in six planes at half a sidereal day. In ECI the
    // planes are obvious the moment the trails are on; in ECEF the same orbits
    // are the figure-of-eight ground tracks a navigation constellation is
    // usually drawn as.
    options: { leadingOrbits: 0.5, trailingOrbits: 0.5 },
  },
  {
    id: "geo",
    label: "Geostationary",
    url: group("geo"),
    refreshSeconds: null,
    // Several hundred objects on one ring 35,786 km up, which is where the
    // marker budget starts to matter — and the one case where the two reference
    // frames look completely different: a circle in ECI, a stationary dot in
    // ECEF, which is the whole meaning of the word.
    options: { trails: false, pointSizePx: 5 },
  },
  {
    id: "molniya",
    label: "Molniya · high ellipse",
    url: group("molniya"),
    refreshSeconds: null,
    // The most eccentric orbit in regular use: apogee over the north at 40,000
    // km, perigee at 500. A whole revolution of trail is the only way to see
    // what that shape is, so it gets one.
    options: { leadingOrbits: 1, trailingOrbits: 1, trailSamples: 256 },
  },
  {
    id: "starlink",
    label: "Starlink · a full shell",
    url: group("starlink"),
    refreshSeconds: null,
    // Thousands of objects, which is past every budget the globe has — so this
    // is the one that demonstrates them: the layer reports what it held and
    // what it drew, and the panel says so rather than leaving the difference to
    // be discovered.
    options: { trails: false, pointSizePx: 4 },
  },
];

/** The one the app puts up on its own. */
export const DEFAULT_CATALOGUE = CATALOGUES[0];
