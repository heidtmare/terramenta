/**
 * The picked thing: what is under the cursor, and everything that is known
 * about it.
 *
 * The globe does the hit tests — it has the geometry and the camera — and
 * reports three pairs on every snapshot: `overlays.hovered` and
 * `overlays.pinned` for the features drawn on the ground,
 * `ephemerides.hovered` and `ephemerides.pinned` for the satellites drawn above
 * them, and `placemarks.hovered` and `placemarks.pinned` for the two icons
 * standing on the points the sun and the moon are overhead. It haloes the
 * pinned one of each pair in preference to the hovered one, and this panel
 * shows the same.
 *
 * Three kinds and one panel, so there is a precedence, and it is the one the
 * eye already expects. Pinned beats hovered, because pinning is someone saying
 * what they meant; and between three of the same rank they rank by how hard
 * they are to hit and what is drawn in front of what. The satellite wins —
 * getting the cursor onto a seven-pixel dot moving across a coastline is not
 * something that happens by accident — then the placemark, whose icon is drawn
 * over the ground and is still a small target beside a country, and last the
 * feature.
 *
 * Which leaves one job here, and it is the interesting one: deciding that a
 * click means "pin this". The globe does not know what a click is; it knows
 * what is under the pointer. So this file watches the canvas for a press and
 * release that did not move — an orbit drag ends in a release too, and pinning
 * something every time someone spun the globe would be unbearable — and turns
 * that into `pinSatellite`, `pinPlacemark` or `pinFeature`, or into clearing
 * all three over empty sky.
 *
 * Properties are rendered without being understood. The globe hands a feature's
 * back exactly as the feed wrote them, and a feed is free to put an object
 * inside an object; so scalars are shown as they are, a URL becomes a link
 * because that is always useful, and anything else falls back to its JSON.
 * Neither of the other two kinds has properties at all — a satellite's
 * catalogue record is elements and a placemark is a single moving coordinate —
 * so this file makes the list that a person would want instead.
 */

import * as globe from "./globe.js";
import { el } from "./dom.js";
import { altitude, clock, coordinate, duration } from "./format.js";

/** How far the pointer may travel and still count as a click rather than a drag. */
const CLICK_SLOP_PX = 5;
/** And how long it may be held. A slow press is still a click; a hold is not. */
const CLICK_HOLD_MS = 600;

export function mountFeature(root, canvas) {
  let lastState = null;

  const title = el("h2", {}, "—");
  const badge = el("span", { class: "feature-badge" }, "pinned");
  const subtitle = el("p", { class: "detail" }, "—");
  const properties = el("dl", { class: "properties" });
  const hint = el("p", { class: "detail feature-hint" }, "Click to keep this open.");

  const clear = el(
    "button",
    {
      class: "button feature-clear",
      type: "button",
      title: "Clear the selection",
      onclick: clearEverything,
    },
    "✕",
  );

  root.append(
    el("div", { class: "feature-head" }, title, badge, clear),
    subtitle,
    properties,
    hint,
  );

  // --- Click to pin --------------------------------------------------------

  // Whichever kind was clicked becomes the selection and the other two are let
  // go: all three are haloed independently, so leaving a pin behind in another
  // kind would leave a halo on the globe that this panel is no longer
  // explaining.
  function clearEverything() {
    globe.clearPinnedFeature();
    globe.clearPinnedSatellite();
    globe.clearPinnedPlacemark();
  }

  let pressedAt = 0;
  let pressedX = 0;
  let pressedY = 0;

  canvas.addEventListener("pointerdown", (event) => {
    pressedAt = event.timeStamp;
    pressedX = event.clientX;
    pressedY = event.clientY;
  });

  canvas.addEventListener("pointerup", (event) => {
    const travelled = Math.hypot(event.clientX - pressedX, event.clientY - pressedY);
    if (travelled > CLICK_SLOP_PX || event.timeStamp - pressedAt > CLICK_HOLD_MS) return;

    const satellite = lastState?.ephemerides.hovered;
    const placemark = lastState?.placemarks.hovered;
    const feature = lastState?.overlays.hovered;

    // The same precedence the panel displays by, so what a click keeps is what
    // was already being shown when it was made.
    clearEverything();
    if (satellite) {
      globe.pinSatellite(satellite.layer, satellite.noradId);
    } else if (placemark) {
      globe.pinPlacemark(placemark.body);
    } else if (feature) {
      globe.pinFeature(feature.layer, feature.index);
    }
    // And a click on nothing keeps nothing, which is what makes the globe feel
    // like it is the thing being clicked on.
  });

  return {
    sync(state) {
      lastState = state;
      const picked = choose(state);

      root.hidden = !picked;
      if (!picked) return;

      root.classList.toggle("pinned", picked.pinned);
      title.textContent = picked.title;
      subtitle.textContent = picked.subtitle;
      hint.hidden = picked.pinned;

      render(properties, picked.properties);
    },
  };
}

/** The one of the six picks this panel is showing, already unpacked. */
function choose(state) {
  const { hovered: satellite, pinned: pinnedSatellite } = state.ephemerides;
  const { hovered: placemark, pinned: pinnedPlacemark } = state.placemarks;
  const { hovered: feature, pinned: pinnedFeature } = state.overlays;

  if (pinnedSatellite) return describeSatellite(pinnedSatellite, true);
  if (pinnedPlacemark) return describePlacemark(pinnedPlacemark, true, state);
  if (pinnedFeature) return describeFeature(pinnedFeature, true);
  if (satellite) return describeSatellite(satellite, false);
  if (placemark) return describePlacemark(placemark, false, state);
  if (feature) return describeFeature(feature, false);
  return null;
}

function describeFeature(feature, pinned) {
  // simplestyle-spec's `title` is the one name a document gives its own
  // features, so it beats the layer's — which then moves down to the subtitle,
  // where it still says which feed this came out of. The globe parses it for
  // us; `feature.style` is absent for a document that styles nothing.
  const title = feature.style?.title;
  return {
    pinned,
    title: title ?? feature.label,
    subtitle: [title ? feature.label : null, feature.kind, feature.id ?? `feature ${feature.index}`]
      .filter(Boolean)
      .join(" · "),
    properties: feature.properties,
  };
}

/**
 * What to say about a satellite.
 *
 * Elements rather than properties, and in the order they answer the questions
 * someone pointing at a moving dot is actually asking: what is it, where is it,
 * and how much should the dot be trusted — which is what the age of the
 * elements is. SGP4 is a fit around its epoch and drifts away from it.
 */
function describeSatellite(satellite, pinned) {
  const properties = {
    Layer: satellite.layerLabel,
    Catalogue: `#${satellite.noradId}`,
    Designator: satellite.internationalDesignator ?? "—",
    Position: coordinate(satellite.position),
    Altitude: satellite.position ? altitude(satellite.position.altitudeKm) : "—",
    Period: duration(satellite.periodMinutes * 60),
    Inclination: `${satellite.inclinationDeg.toFixed(2)}°`,
    Eccentricity: satellite.eccentricity.toFixed(6),
    Elements: `${duration(Math.abs(satellite.elementsAgeDays) * 86_400)} old`,
    Trail: satellite.trail ? "drawn" : "off",
  };

  return {
    pinned,
    title: satellite.name,
    subtitle: ["satellite", `#${satellite.noradId}`].join(" · "),
    properties,
  };
}

/**
 * What to say about a placemark.
 *
 * A placemark has no record behind it at all — it is one coordinate, recomputed
 * every frame from the globe's own clock — so everything below is either that
 * coordinate said a second way or worked out from it here. Which is the point:
 * the icon says *where* the body is overhead, and this says what that is worth
 * knowing, which is where the body is in the sky from the part of the globe you
 * are actually looking at.
 *
 * The moon's rows are the ones that could not be read off the snapshot. How far
 * the sublunar point is from the subsolar point is the moon's elongation, and
 * the elongation is the phase: together is new, opposite is full. The globe
 * draws both icons and never says so, because it has nothing to say it in — so
 * the arithmetic is here, against the two coordinates the snapshot does carry.
 */
function describePlacemark(placemark, pinned, state) {
  // `"Sun · subsolar point"` — the globe's own name for it, split so the body
  // is the heading and what the icon marks reads as the subtitle. Anything the
  // globe names differently still lands somewhere sensible.
  const [name, point = "placemark"] = placemark.label.split(" · ");
  const centre = state.camera.center;
  const properties = {
    Point: coordinate(placemark.coordinate),
  };

  if (placemark.body === "moon") {
    const elongation = separation(placemark.coordinate, state.sun.subsolar);
    properties.Elongation = `${elongation.toFixed(1)}° from the sun`;
    properties.Phase = phase(elongation, placemark.coordinate, state.sun.subsolar);
  } else {
    // The subsolar latitude *is* the sun's declination, which is the one
    // reading of it that says what time of year the clock is at.
    properties.Declination = `${Math.abs(placemark.coordinate.lat).toFixed(2)}° ${
      placemark.coordinate.lat >= 0 ? "N" : "S"
    }`;
  }

  properties["At view centre"] = [
    placemark.body === "sun" ? solarTime(centre.lon, state.sun.subsolar.lon) : null,
    elevation(centre, placemark.coordinate),
  ]
    .filter(Boolean)
    .join(" · ");
  properties.Clock = clock(state.sun.unixSeconds) + (state.sun.paused ? " (paused)" : "");

  return {
    pinned,
    title: name,
    subtitle: ["placemark", point].join(" · "),
    properties,
  };
}

/**
 * The angle between two points on the globe, in degrees.
 *
 * Both placemarks are sub-points of a direction from the centre of the Earth,
 * so the angle between two of them on the ground *is* the angle between the two
 * bodies in the sky — which is what makes one subtraction answer both the
 * elongation and how high something is above the horizon.
 */
function separation(a, b) {
  const rad = Math.PI / 180;
  const cosine =
    Math.sin(a.lat * rad) * Math.sin(b.lat * rad) +
    Math.cos(a.lat * rad) * Math.cos(b.lat * rad) * Math.cos((b.lon - a.lon) * rad);
  return Math.acos(Math.min(1, Math.max(-1, cosine))) / rad;
}

/**
 * How high a body is above the horizon at a point on the ground.
 *
 * Geocentric, like the placemark itself: at the sub-point the body is straight
 * up, and ninety degrees away it is on the horizon. Below it, the point is on
 * the night side of that body — which for the sun is the same line the globe
 * shades along, so this row and the terminator on screen agree.
 */
function elevation(at, subpoint) {
  const degrees = 90 - separation(at, subpoint);
  return `${Math.abs(degrees).toFixed(0)}° ${degrees >= 0 ? "above" : "below"} the horizon`;
}

/**
 * Local apparent solar time at a longitude: what a sundial there would read.
 *
 * Noon is on the subsolar meridian by definition, and every fifteen degrees
 * away from it is an hour — so the whole of it is the difference between the
 * two longitudes. Apparent rather than mean, which is the honest answer here:
 * the sundial is what the globe is drawing.
 */
function solarTime(lon, subsolarLon) {
  const hours = (12 + (lon - subsolarLon) / 15 + 24) % 24;
  const minutes = Math.round(hours * 60) % 1440;
  const pad = (value) => String(value).padStart(2, "0");
  return `${pad(Math.floor(minutes / 60))}:${pad(minutes % 60)} solar`;
}

/**
 * The moon's phase, from how far its point is from the sun's.
 *
 * The lit fraction is the standard one, `(1 - cos elongation) / 2`, which
 * treats the sunlight as parallel — the sun is far enough away that the
 * difference is smaller than the number is rounded to.
 *
 * Waxing or waning is the side it is on rather than how far. The moon moves
 * east through the sky, so its sub-point sits east of the sun's through the
 * half of the month it is filling and west of it through the half it is
 * emptying.
 */
function phase(elongation, sublunar, subsolar) {
  const lit = Math.round(((1 - Math.cos((elongation * Math.PI) / 180)) / 2) * 100);
  const waxing = ((sublunar.lon - subsolar.lon + 540) % 360) - 180 >= 0;
  const waxed = waxing ? "waxing" : "waning";
  const name =
    elongation < 10
      ? "new"
      : elongation > 170
        ? "full"
        : elongation < 80
          ? `${waxed} crescent`
          : elongation > 100
            ? `${waxed} gibbous`
            : waxing
              ? "first quarter"
              : "last quarter";
  return `${name} · ${lit}% lit`;
}

/** Fills the list from a feature's properties, whatever shape they are. */
function render(list, values) {
  list.replaceChildren();

  const entries = values && typeof values === "object" && !Array.isArray(values)
    ? Object.entries(values)
    : [];

  if (entries.length === 0) {
    list.append(el("p", { class: "detail" }, "No properties."));
    return;
  }

  for (const [name, value] of entries) {
    list.append(el("dt", {}, name), el("dd", {}, present(value)));
  }
}

/** One property value, as something to put in the page. */
function present(value) {
  if (value == null || value === "") return "—";
  if (typeof value === "string" && /^https?:\/\//.test(value)) {
    return el("a", { href: value, target: "_blank", rel: "noreferrer noopener" }, value);
  }
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}
