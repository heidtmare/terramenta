/**
 * The picked feature: what is under the cursor, and everything the document
 * said about it.
 *
 * The globe does the hit test — it has the geometry and the camera — and
 * reports two things on every snapshot: `hovered`, whatever the pointer is
 * over, and `pinned`, whatever was last asked to stay. It highlights the pinned
 * one in preference to the hovered one, and this panel shows the same.
 *
 * Which leaves one job here, and it is the interesting one: deciding that a
 * click means "pin this". The globe does not know what a click is; it knows
 * what is under the pointer. So this file watches the canvas for a press and
 * release that did not move — an orbit drag ends in a release too, and pinning
 * a feature every time someone spun the globe would be unbearable — and turns
 * that into `pinFeature`, or into `clearPinnedFeature` over empty ocean.
 *
 * Properties are rendered without being understood. The globe hands them back
 * exactly as the feed wrote them, and a feed is free to put an object inside an
 * object; so scalars are shown as they are, a URL becomes a link because that
 * is always useful, and anything else falls back to its JSON.
 */

import * as globe from "./globe.js";
import { el } from "./dom.js";

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
      onclick: () => globe.clearPinnedFeature(),
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

    const hovered = lastState?.overlays.hovered;
    if (hovered) {
      globe.pinFeature(hovered.layer, hovered.index);
    } else {
      // A click on nothing is how a selection is let go, which is what makes
      // the globe feel like it is the thing being clicked on.
      globe.clearPinnedFeature();
    }
  });

  return {
    sync(state) {
      lastState = state;
      const { pinned, hovered } = state.overlays;
      const feature = pinned ?? hovered;

      root.hidden = !feature;
      if (!feature) return;

      root.classList.toggle("pinned", Boolean(pinned));
      title.textContent = feature.label;
      subtitle.textContent = [feature.kind, feature.id ?? `feature ${feature.index}`].join(" · ");
      hint.hidden = Boolean(pinned);

      render(properties, feature.properties);
    },
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
