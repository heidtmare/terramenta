/**
 * The picked thing: what is under the cursor, and everything that is known
 * about it.
 *
 * The globe does the hit tests — it has the geometry and the camera — and
 * reports two pairs on every snapshot: `overlays.hovered` and `overlays.pinned`
 * for the features drawn on the ground, and `ephemerides.hovered` and
 * `ephemerides.pinned` for the satellites drawn above them. It haloes the
 * pinned one of each pair in preference to the hovered one, and this panel
 * shows the same.
 *
 * Two kinds and one panel, so there is a precedence, and it is the one the eye
 * already expects. Pinned beats hovered, because pinning is someone saying what
 * they meant; and between two of the same rank the satellite wins, because it
 * is the smaller target and the one drawn in front — getting the cursor onto a
 * seven-pixel dot moving across a coastline is not something that happens by
 * accident.
 *
 * Which leaves one job here, and it is the interesting one: deciding that a
 * click means "pin this". The globe does not know what a click is; it knows
 * what is under the pointer. So this file watches the canvas for a press and
 * release that did not move — an orbit drag ends in a release too, and pinning
 * something every time someone spun the globe would be unbearable — and turns
 * that into `pinSatellite` or `pinFeature`, or into clearing both over empty
 * sky.
 *
 * Properties are rendered without being understood. The globe hands a feature's
 * back exactly as the feed wrote them, and a feed is free to put an object
 * inside an object; so scalars are shown as they are, a URL becomes a link
 * because that is always useful, and anything else falls back to its JSON. A
 * satellite has no properties — its catalogue record is elements, not
 * annotations — so this file makes the list that a person would want instead.
 */

import * as globe from "./globe.js";
import { el } from "./dom.js";
import { altitude, coordinate, duration } from "./format.js";

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
      onclick: () => {
        globe.clearPinnedFeature();
        globe.clearPinnedSatellite();
      },
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
    const feature = lastState?.overlays.hovered;

    // Whichever was clicked becomes the selection, and the other is let go:
    // both are haloed independently, and leaving a pin behind in the other kind
    // would leave a halo on the globe that this panel is no longer explaining.
    if (satellite) {
      globe.pinSatellite(satellite.layer, satellite.noradId);
      globe.clearPinnedFeature();
    } else if (feature) {
      globe.pinFeature(feature.layer, feature.index);
      globe.clearPinnedSatellite();
    } else {
      // A click on nothing is how a selection is let go, which is what makes
      // the globe feel like it is the thing being clicked on.
      globe.clearPinnedFeature();
      globe.clearPinnedSatellite();
    }
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

/** The one of the four picks this panel is showing, already unpacked. */
function choose(state) {
  const { hovered: satellite, pinned: pinnedSatellite } = state.ephemerides;
  const { hovered: feature, pinned: pinnedFeature } = state.overlays;

  if (pinnedSatellite) return describeSatellite(pinnedSatellite, true);
  if (pinnedFeature) return describeFeature(pinnedFeature, true);
  if (satellite) return describeSatellite(satellite, false);
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
