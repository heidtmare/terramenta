/**
 * The GeoJSON overlay controls: adding a layer, and living with the ones that
 * are up.
 *
 * Two halves. At the top is where a layer comes from — a URL the globe will
 * fetch, or a file off this machine — and below it one row per layer that is
 * up, rebuilt from the state snapshot like everything else in the panel.
 *
 * The one thing this file does that the rest of the app does not is keep state
 * of its own, and it is worth saying why. The globe refreshes a URL by
 * refetching it; a file it was handed once it cannot fetch again, and it does
 * not hold on to the `File` the browser gave us either. So for a local file the
 * *app* owns the timer: it keeps the `File`, re-reads it on the period the user
 * asked for, and re-sends the text under the same layer id, which the globe
 * treats as a replacement. A layer from a URL is left entirely to the globe.
 *
 * The other place it departs from the one-way rule is any field with a caret
 * in it — the URL box, the refresh period — which is left alone while it has
 * focus. See `setUnlessFocused` in `widgets.js`.
 */

import * as globe from "./globe.js";
import { el, row, section } from "./dom.js";
import { CATALOGUE, DEFAULT_FEED } from "./feeds.js";
import { duration } from "./format.js";
import { button, setUnlessFocused, toggle } from "./widgets.js";

/**
 * What a new layer is coloured, in turn.
 *
 * Every hue here has to read against ocean, vegetation, desert, cloud and the
 * night side alike, which rules out most of the spectrum: what is left is warm
 * and bright. Layers cycle through them so that a second one does not arrive
 * looking exactly like the first.
 */
const PALETTE = ["#ff9e3d", "#7fd4ff", "#ffe066", "#ff7aa2", "#9df08a"];
let added = 0;

/** Nothing sensible polls faster than this, and the globe clamps it anyway. */
const MIN_REFRESH_SECONDS = 5;

/**
 * Puts a feed up: a URL, a name for it, and how often to go back for more.
 *
 * Exported because `main.js` puts two up at boot — the layer is the whole
 * feature, and a panel showing an empty list is a poor way to introduce it.
 */
export function addFeed({ id, url, label, refreshSeconds = null, options = {} }) {
  const { fillAlpha, ...rest } = options;
  globe.addOverlay(id ?? idForUrl(url), {
    url,
    label: label ?? labelForUrl(url),
    refreshSeconds,
    ...paint(PALETTE[added++ % PALETTE.length], fillAlpha),
    ...rest,
  });
}

/**
 * The three colours of a layer, from the one an interface picks.
 *
 * `fillAlpha` is a hex byte, for the layers that want a denser fill than a flat
 * ring does — an extruded solid has walls as well as a lid, and at the opacity
 * that suits a ring it would barely be there.
 */
function paint(hex, fillAlpha = "3a") {
  return {
    pointColor: hex,
    lineColor: hex,
    // The fill is the same colour at a fraction of the opacity, so a filled
    // ring reads as the interior of its outline rather than as its own shape.
    fillColor: `${hex}${fillAlpha}`,
  };
}

/**
 * Builds the overlay section.
 *
 * `bind(node, sync)` is the panel's own registration: it is what arranges for
 * `sync` to be called with every state snapshot.
 */
export function mountOverlays(bind) {
  /** Local files, by layer id, so they can be re-read when a refresh is due. */
  const files = new Map();

  // --- Adding a layer ------------------------------------------------------

  const urlInput = el("input", {
    class: "coordinate",
    type: "url",
    value: DEFAULT_FEED.url,
    spellcheck: false,
    placeholder: "https://…/features.geojson",
  });

  const periodInput = el("input", {
    class: "coordinate",
    type: "number",
    min: String(MIN_REFRESH_SECONDS),
    step: "5",
    value: String(DEFAULT_FEED.refreshSeconds),
  });

  const refreshToggle = toggle("Refresh automatically", () => {});
  const period = () => (refreshToggle.input.checked ? readPeriod(periodInput) : null);

  const addUrl = () => {
    const url = urlInput.value.trim();
    if (url) addFeed({ url, refreshSeconds: period() });
  };

  // A file input cannot be styled, so the real one is hidden behind a button
  // that reads like the others.
  const fileInput = el("input", {
    type: "file",
    accept: ".geojson,.json,application/geo+json,application/json",
    hidden: true,
    onchange: (event) => {
      const [file] = event.target.files ?? [];
      if (file) addFile(file);
      // Cleared so that picking the same file twice still fires a change.
      event.target.value = "";
    },
  });

  const addFile = async (file) => {
    const id = `file:${file.name}`;
    const existing = files.get(id);
    files.set(id, {
      file,
      periodSeconds: existing?.periodSeconds ?? period(),
      color: existing?.color ?? PALETTE[added++ % PALETTE.length],
      since: performance.now(),
    });
    await sendFile(id);
  };

  /** Re-reads a file and hands the globe its text, replacing the layer. */
  const sendFile = async (id) => {
    const entry = files.get(id);
    if (!entry) return;
    try {
      const text = await entry.file.text();
      entry.since = performance.now();
      globe.addOverlay(id, {
        text,
        label: entry.file.name,
        // Kept, so a re-read does not repaint a layer the user has recoloured.
        ...paint(entry.color),
      });
    } catch (error) {
      // The file moved or was replaced since it was picked. Nothing to do but
      // stop asking for it.
      console.warn(`Could not re-read ${entry.file.name}:`, error);
      files.delete(id);
    }
  };

  // One timer for every file, rather than one each: they are all doing the same
  // thing at second resolution, and a single tick is easier to reason about.
  setInterval(() => {
    const now = performance.now();
    for (const [id, entry] of files) {
      if (!entry.periodSeconds) continue;
      if (now - entry.since >= entry.periodSeconds * 1000) sendFile(id);
    }
  }, 1000);

  const adding = el(
    "div",
    {},
    row("Source", urlInput),
    el(
      "div",
      { class: "places" },
      ...CATALOGUE.map((feed) =>
        button(feed.label, () => {
          urlInput.value = feed.url;
          // A document that nothing is rewriting has no period to offer, so the
          // box is left saying whatever it said and the switch goes off.
          if (feed.refreshSeconds) periodInput.value = String(feed.refreshSeconds);
          refreshToggle.set(Boolean(feed.refreshSeconds));
        }),
      ),
    ),
    refreshToggle.node,
    row("Every", el("div", { class: "period" }, periodInput, el("span", {}, "seconds"))),
    el(
      "div",
      { class: "buttons" },
      button("Add layer", addUrl),
      button("Local file…", () => fileInput.click()),
    ),
    fileInput,
    el(
      "p",
      { class: "detail" },
      "A URL is the globe's to fetch and refetch. A file is read here and sent " +
        "as text, so this app re-reads it on the same period — which is also " +
        "what re-picking a file that has changed on disk would do.",
    ),
  );

  // --- The layers that are up ----------------------------------------------

  const list = el("div", { class: "layers" });
  const rows = new Map();

  bind(list, (state) => {
    const layers = state.overlays.layers;

    // Rows are kept and updated rather than rebuilt, so a colour picker or a
    // period being edited is not torn out from under the pointer.
    for (const [id, existing] of rows) {
      if (!layers.some((layer) => layer.id === id)) {
        existing.node.remove();
        rows.delete(id);
        files.delete(id);
      }
    }

    for (const layer of layers) {
      let entry = rows.get(layer.id);
      if (!entry) {
        entry = layerRow(layer, files);
        rows.set(layer.id, entry);
        list.append(entry.node);
      }
      entry.sync(layer);
    }

    list.classList.toggle("empty", layers.length === 0);
  });

  const overlaysToggle = toggle("Draw overlays", globe.setOverlaysEnabled);
  bind(overlaysToggle.node, (state) => overlaysToggle.set(state.overlays.enabled));

  const pickingToggle = toggle("Pick features under the cursor", globe.setPickingEnabled);
  bind(pickingToggle.node, (state) => pickingToggle.set(state.overlays.picking));

  return section(
    "GeoJSON overlays",
    overlaysToggle.node,
    pickingToggle.node,
    el(
      "p",
      { class: "detail" },
      "The globe hit-tests the geometry and reports what is under the pointer, " +
        "with its properties; clicking keeps one selected. Both show top left.",
    ),
    adding,
    list,
  );
}

/**
 * One layer: what it is, what it is doing, and the four things that can be done
 * to it.
 */
function layerRow(layer, files) {
  const visible = toggle(layer.label, (value) => globe.setOverlayVisible(layer.id, value));

  const color = el("input", {
    class: "swatch",
    type: "color",
    value: layer.style.pointColor.slice(0, 7).toLowerCase(),
    title: "Layer colour",
    oninput: (event) => {
      const hex = event.target.value;
      // A file's colour is remembered here, because re-reading it re-adds the
      // layer and the globe would otherwise hand back the default.
      const file = files.get(layer.id);
      if (file) file.color = hex;
      globe.setOverlayStyle(layer.id, paint(hex));
    },
  });

  const periodInput = el("input", {
    class: "coordinate",
    type: "number",
    min: String(MIN_REFRESH_SECONDS),
    step: "5",
    title: "Seconds between refreshes",
  });

  const refreshToggle = toggle("Auto", (on) => setRefresh(layer.id, on ? readPeriod(periodInput) : null));
  periodInput.onchange = () => {
    if (refreshToggle.input.checked) setRefresh(layer.id, readPeriod(periodInput));
  };

  // GeoJSON's third element is a height, in principle. In practice a feed may
  // put anything there — the USGS ones put depth in kilometres — so this is the
  // escape hatch: draw the layer flat and ignore whatever it says.
  //
  // Extrude is the other end of the same question: a ring at a height is a lid
  // hanging in the air until its edges are walled down to the ground. Both are
  // sent together, because the globe takes the height setting whole.
  const height = () => ({
    altitudeMode: clampToggle.input.checked ? "clampToSurface" : "relativeToSurface",
    extrude: extrudeToggle.input.checked,
  });
  const clampToggle = toggle("Clamp to surface", () =>
    globe.setOverlayAltitude(layer.id, height()),
  );
  const extrudeToggle = toggle("Extrude to ground", () =>
    globe.setOverlayAltitude(layer.id, height()),
  );

  /** A file's timer lives here; a URL's lives in the globe. */
  const setRefresh = (id, seconds) => {
    const file = files.get(id);
    if (file) {
      file.periodSeconds = seconds;
      file.since = performance.now();
    } else {
      globe.setOverlayRefresh(id, seconds);
    }
  };

  const refreshNow = () => {
    if (files.has(layer.id)) {
      files.get(layer.id).since = 0;
    } else {
      globe.refreshOverlay(layer.id);
    }
  };

  const detail = el("p", { class: "detail" }, "—");

  const node = el(
    "div",
    { class: "layer" },
    el("div", { class: "layer-head" }, visible.node, color),
    detail,
    el("div", { class: "layer-height" }, clampToggle.node, extrudeToggle.node),
    el(
      "div",
      { class: "buttons" },
      refreshToggle.node,
      periodInput,
      button("Now", refreshNow),
      button("Remove", () => globe.removeOverlay(layer.id)),
    ),
  );

  return {
    node,
    sync(layer) {
      visible.set(layer.visible);
      clampToggle.set(layer.altitude.mode === "clampToSurface");
      extrudeToggle.set(layer.altitude.extrude);
      node.dataset.status = layer.status;

      const local = files.get(layer.id);
      const period = local ? local.periodSeconds : layer.refreshSeconds;
      refreshToggle.set(Boolean(period));
      // Left enabled even when auto is off, so a period can be typed first and
      // switched on second.
      setUnlessFocused(periodInput, String(period ?? DEFAULT_FEED.refreshSeconds));

      if (document.activeElement !== color) {
        color.value = layer.style.pointColor.slice(0, 7).toLowerCase();
      }
      detail.textContent = describe(layer, local);
    },
  };
}

/** The line under a layer's name: what it holds, and what it is doing. */
function describe(layer, local) {
  if (layer.status === "failed") return `failed — ${layer.error ?? "unknown error"}`;
  if (layer.status === "loading" && layer.features === 0 && layer.points === 0) {
    return "loading…";
  }

  const geometry = [
    layer.features && `${layer.features} features`,
    layer.points && `${layer.points} points`,
    layer.lines && `${layer.lines} lines`,
    layer.polygons && `${layer.polygons} polygons`,
  ].filter(Boolean);

  const timing = [];
  if (layer.status === "loading") {
    timing.push("refreshing…");
  } else if (local) {
    // A file's age is the app's to know: the globe never fetched it.
    timing.push(`read ${duration((performance.now() - local.since) / 1000)} ago`);
    if (local.periodSeconds) {
      timing.push(
        `next in ${duration(local.periodSeconds - (performance.now() - local.since) / 1000)}`,
      );
    }
  } else {
    timing.push(`${duration(layer.ageSeconds)} ago`);
    if (layer.nextRefreshSeconds != null) {
      timing.push(`next in ${duration(layer.nextRefreshSeconds)}`);
    }
  }

  return [geometry.join(" · ") || "nothing to draw", ...timing].join(" — ");
}

/** The period box, floored at something that is a refresh rather than a loop. */
function readPeriod(input) {
  const seconds = Number(input.value);
  return Number.isFinite(seconds) && seconds > 0
    ? Math.max(seconds, MIN_REFRESH_SECONDS)
    : DEFAULT_FEED.refreshSeconds;
}

/**
 * A stable id for a URL, so re-adding the same feed replaces the layer instead
 * of stacking a second copy of it on the first.
 */
function idForUrl(url) {
  return `url:${url}`;
}

/** The file name out of a URL, which is what a feed is usefully called. */
function labelForUrl(url) {
  try {
    const path = new URL(url, document.baseURI).pathname;
    return decodeURIComponent(path.split("/").filter(Boolean).pop() ?? url);
  } catch {
    return url;
  }
}
