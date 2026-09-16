/**
 * Somewhere to fly to.
 *
 * A fly-to control needs coordinates to be worth trying, and these are picked
 * to show the globe doing different things: a city at street altitude, a
 * feature only the deep imagery levels resolve, and the poles, where a
 * quadtree over a latitude/longitude grid is at its least comfortable.
 */
export const PLACES = [
  { name: "New York", lat: 40.7128, lon: -74.006, altitudeKm: 800 },
  { name: "Cairo", lat: 30.0444, lon: 31.2357, altitudeKm: 800 },
  { name: "Tokyo", lat: 35.6762, lon: 139.6503, altitudeKm: 800 },
  { name: "Sydney", lat: -33.8688, lon: 151.2093, altitudeKm: 800 },
  { name: "Amazon", lat: -3.4653, lon: -62.2159, altitudeKm: 2500 },
  { name: "Sahara", lat: 23.4162, lon: 25.6628, altitudeKm: 2500 },
  { name: "Himalaya", lat: 27.9881, lon: 86.925, altitudeKm: 1200 },
  { name: "North Pole", lat: 90, lon: 0, altitudeKm: 4000 },
  { name: "South Pole", lat: -90, lon: 0, altitudeKm: 4000 },
];
