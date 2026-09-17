/**
 * The satellite controls: putting a catalogue up, choosing what out of it to
 * draw, and how much orbit to draw through each one.
 *
 * Structurally this is `overlays.js` again — a source at the top, a row per
 * layer under it, everything rebuilt from the state snapshot — and it departs
 * from that file in exactly two places, both because a catalogue is a different
 * kind of thing from a document.
 *
 * **The object list is pulled, not streamed.** Every other control here is
 * drawn from the snapshot the globe pushes ten times a second. A catalogue can
 * be eight thousand rows, and serialising those into every snapshot to discover
 * that none of them changed would cost more than the propagation does. So the
 * snapshot carries a `revision` per layer instead, and this file calls
 * `ephemerisObjects(id)` when that number moves — a catalogue landing, a
 * selection being toggled — and not otherwise.
 *
 * **The list is windowed.** Even pulled, eight thousand rows is eight thousand
 * DOM nodes to build so that a hundred can be looked at. So the list draws the
 * first `LIST_LIMIT` matches and says how many it left out; the filter box is
 * what gets you to the rest, and it is also how you find one object by name in
 * a shell of thousands.
 */

import * as globe from "./globe.js";
import { el, row, section } from "./dom.js";
import { duration } from "./format.js";
import { CATALOGUES, DEFAULT_CATALOGUE } from "./orbits.js";
import { button, setUnlessFocused, toggle } from "./widgets.js";

/**
 * What a new layer is coloured, in turn.
 *
 * Cool where the overlay palette is warm, and deliberately: a satellite is the
 * one thing drawn here that is never on the ground, so it reads as its own kind
 * of thing rather than as another feed of places.
 */
const PALETTE = ["#8ce0ff", "#b9a8ff", "#7ef0c8", "#ffd9f0", "#a8c8ff"];
let added = 0;

/** The globe floors a refresh at five minutes; so does the box. */
const MIN_REFRESH_SECONDS = 300;

/** How many objects one layer's list draws before it asks you to filter. */
const LIST_LIMIT = 200;

/**
 * Puts a catalogue up.
 *
 * Exported because `main.js` puts one up at boot, for the same reason it puts
 * overlays up: a panel introducing a feature with an empty list is a poor
 * introduction.
 */
export function addCatalogue({ id, url, label, refreshSeconds = null, options = {} }) {
  globe.addEphemeris(id ?? `omm:${url}`, {
    url,
    label: label ?? url,
    refreshSeconds,
    ...paint(PALETTE[added++ % PALETTE.length]),
    ...options,
  });
}

/** The two colours of a satellite layer, from the one an interface picks. */
function paint(hex) {
  return {
    pointColor: hex,
    // The arc is the same colour, dimmed: it is where the marker has been
    // rather than a second thing to look at, and at full strength a shell of
    // sixty orbits is a ball of wool.
    lineColor: `${hex}a6`,
  };
}

/**
 * Builds the ephemeris section.
 *
 * `bind(node, sync)` is the panel's own registration, exactly as in
 * `overlays.js`.
 */
export function mountEphemeris(bind) {
  // --- Adding a layer ------------------------------------------------------

  const urlInput = el("input", {
    class: "coordinate",
    type: "url",
    value: DEFAULT_CATALOGUE.url,
    spellcheck: false,
    placeholder: "https://…&FORMAT=json",
  });

  const periodInput = el("input", {
    class: "coordinate",
    type: "number",
    min: String(MIN_REFRESH_SECONDS),
    step: "300",
    value: String(MIN_REFRESH_SECONDS * 4),
  });

  const refreshToggle = toggle("Refetch the catalogue", () => {});
  refreshToggle.set(false);
  const period = () => (refreshToggle.input.checked ? readPeriod(periodInput) : null);

  let pendingOptions = { ...DEFAULT_CATALOGUE.options };

  const addUrl = () => {
    const url = urlInput.value.trim();
    if (url) {
      addCatalogue({ url, refreshSeconds: period(), options: pendingOptions });
      pendingOptions = {};
    }
  };

  const fileInput = el("input", {
    type: "file",
    accept: ".json,application/json",
    hidden: true,
    onchange: async (event) => {
      const [file] = event.target.files ?? [];
      event.target.value = "";
      if (!file) return;
      // A catalogue handed over as text cannot be refetched, and unlike a
      // GeoJSON file there is no reason to want to: the elements in it do not
      // change on disk. So there is no timer here, where `overlays.js` keeps
      // one — the file is read once and that is the layer.
      globe.addEphemeris(`file:${file.name}`, {
        text: await file.text(),
        label: file.name,
        ...paint(PALETTE[added++ % PALETTE.length]),
      });
    },
  });

  const adding = el(
    "div",
    {},
    row("Catalogue", urlInput),
    el(
      "div",
      { class: "places" },
      ...CATALOGUES.map((entry) =>
        button(entry.label, () => {
          urlInput.value = entry.url;
          pendingOptions = { ...entry.options };
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
      "OMM JSON: orbit mean elements, the successor to the two-line element " +
        "set. Each record becomes an SGP4 propagator run against the globe's " +
        "own clock, so pausing it or running a day a second takes the " +
        "satellites with it.",
    ),
  );

  // --- The layers that are up ----------------------------------------------

  const list = el("div", { class: "layers" });
  const rows = new Map();

  bind(list, (state) => {
    const layers = state.ephemerides.layers;

    for (const [id, existing] of rows) {
      if (!layers.some((layer) => layer.id === id)) {
        existing.node.remove();
        rows.delete(id);
      }
    }

    for (const layer of layers) {
      let entry = rows.get(layer.id);
      if (!entry) {
        entry = layerRow(layer);
        rows.set(layer.id, entry);
        list.append(entry.node);
      }
      entry.sync(layer, state);
    }

    list.classList.toggle("empty", layers.length === 0);
  });

  const enabledToggle = toggle("Draw satellites", globe.setEphemeridesEnabled);
  bind(enabledToggle.node, (state) => enabledToggle.set(state.ephemerides.enabled));

  return section(
    "Satellites",
    enabledToggle.node,
    el(
      "p",
      { class: "detail" },
      "Switch to the ECI frame to see an orbit as the closed ellipse it is. " +
        "In ECEF the same arc is a corkscrew, because the Earth turns " +
        "underneath it while the satellite goes round — which is why switching " +
        "frames reshapes it. A layer can draw the orbit itself instead, which " +
        "is the same curve in both.",
    ),
    adding,
    list,
  );
}

/** One layer: what it holds, how it is drawn, and what out of it is drawn. */
function layerRow(layer) {
  const visible = toggle(layer.label, (value) => globe.setEphemerisVisible(layer.id, value));

  const color = el("input", {
    class: "swatch",
    type: "color",
    value: layer.style.pointColor.slice(0, 7).toLowerCase(),
    title: "Layer colour",
    oninput: (event) => globe.setEphemerisStyle(layer.id, paint(event.target.value)),
  });

  const trailsToggle = toggle("Orbit trails", (value) =>
    globe.setEphemerisTrails(layer.id, value),
  );

  // The window is sent whole, because the globe returns anything left out to
  // its default — so moving one end alone would silently reset the other.
  const sendTrail = () =>
    globe.setEphemerisTrail(layer.id, {
      leadingOrbits: Number(leading.input.value),
      trailingOrbits: Number(trailing.input.value),
      trailSamples: layer.trail.samples,
      trailPath: pathToggle.input.checked ? "orbit" : "track",
    });
  const trailing = orbits("Behind", sendTrail);
  const leading = orbits("Ahead", sendTrail);

  // The one control that explains why an arc changes shape when the frame is
  // switched: a track is where the satellite went over the ground, an orbit is
  // the path itself and is the same curve in both frames.
  const pathToggle = toggle("Draw the orbit, not the ground track", sendTrail);

  const periodInput = el("input", {
    class: "coordinate",
    type: "number",
    min: String(MIN_REFRESH_SECONDS),
    step: "300",
    title: "Seconds between refetches",
  });
  const refreshToggle = toggle("Auto", (on) =>
    globe.setEphemerisRefresh(layer.id, on ? readPeriod(periodInput) : null),
  );
  periodInput.onchange = () => {
    if (refreshToggle.input.checked) globe.setEphemerisRefresh(layer.id, readPeriod(periodInput));
  };

  const detail = el("p", { class: "detail" }, "—");
  const objects = objectList(layer.id);

  const node = el(
    "div",
    { class: "layer" },
    el("div", { class: "layer-head" }, visible.node, color),
    detail,
    trailsToggle.node,
    el("div", { class: "trail-window" }, trailing.node, leading.node),
    pathToggle.node,
    objects.node,
    el(
      "div",
      { class: "buttons" },
      refreshToggle.node,
      periodInput,
      button("Now", () => globe.refreshEphemeris(layer.id)),
      button("Remove", () => globe.removeEphemeris(layer.id)),
    ),
  );

  return {
    node,
    sync(next, state) {
      layer = next;
      visible.set(next.visible);
      trailsToggle.set(next.trails);
      leading.set(next.trail.leadingOrbits);
      trailing.set(next.trail.trailingOrbits);
      pathToggle.set(next.trail.path === "orbit");
      node.dataset.status = next.status;

      refreshToggle.set(Boolean(next.refreshSeconds));
      setUnlessFocused(periodInput, String(next.refreshSeconds ?? MIN_REFRESH_SECONDS * 4));

      if (document.activeElement !== color) {
        color.value = next.style.pointColor.slice(0, 7).toLowerCase();
      }
      detail.textContent = describe(next, state.ephemerides);
      objects.sync(next);
    },
  };
}

/**
 * How much of an orbit an arc covers, either side of now.
 *
 * A plain linear range rather than the panel's logarithmic `slider`, because
 * this one runs from zero and a logarithm of zero is not a position on a track.
 */
function orbits(label, onInput) {
  const value = el("span", { class: "slider-value" }, "—");
  const input = el("input", {
    class: "slider",
    type: "range",
    min: "0",
    max: "1",
    step: "0.05",
    value: "0.5",
    oninput: () => {
      value.textContent = format(Number(input.value));
      onInput();
    },
  });
  const format = (orbit) => (orbit === 0 ? "none" : `${Math.round(orbit * 100)}% of an orbit`);
  const node = el(
    "div",
    { class: "slider-row" },
    el("span", { class: "row-label" }, label),
    input,
    value,
  );
  return {
    node,
    input,
    set: (orbit) => {
      if (document.activeElement === input) return;
      input.value = String(orbit);
      value.textContent = format(orbit);
    },
  };
}

/**
 * The objects of one layer, with a switch for drawing each and one for trailing
 * it.
 *
 * Pulled when the layer's `revision` moves, and windowed to `LIST_LIMIT` rows —
 * see the file header for why both.
 */
function objectList(id) {
  let revision = null;
  let objects = [];

  const filter = el("input", {
    class: "coordinate",
    type: "search",
    placeholder: "Filter by name or catalogue number",
    spellcheck: false,
    oninput: () => render(),
  });

  const rows = el("div", { class: "objects" });
  const note = el("p", { class: "detail" }, "");

  const matching = () => {
    const needle = filter.value.trim().toLowerCase();
    if (!needle) return objects;
    return objects.filter(
      (object) =>
        object.name.toLowerCase().includes(needle) || String(object.noradId).includes(needle),
    );
  };

  const render = () => {
    const found = matching();
    rows.replaceChildren(
      ...found.slice(0, LIST_LIMIT).map((object) => objectRow(id, object)),
    );
    note.textContent =
      found.length > LIST_LIMIT
        ? `showing ${LIST_LIMIT} of ${found.length} — filter to narrow it`
        : "";
  };

  const node = el(
    "details",
    { class: "object-list" },
    el("summary", {}, "Objects"),
    filter,
    el(
      "div",
      { class: "buttons" },
      button("Draw all", () => globe.setEphemerisSelection(id, null)),
      button("Draw none", () => globe.setEphemerisSelection(id, [])),
    ),
    rows,
    note,
  );

  return {
    node,
    sync(layer) {
      // The whole point of the revision: without it this would pull eight
      // thousand rows across the boundary ten times a second to find out that
      // nothing had changed.
      if (layer.revision === revision) return;
      revision = layer.revision;
      objects = globe.ephemerisObjects(id) ?? [];
      render();
    },
  };
}

/** One object: draw it, and trail it. */
function objectRow(id, object) {
  const drawn = toggle(object.name, (on) => globe.selectSatellite(id, object.noradId, on));
  drawn.set(object.selected);

  const trail = toggle("orbit", (on) => globe.setSatelliteTrail(id, object.noradId, on));
  trail.set(object.trail);
  trail.input.disabled = !object.selected;

  return el(
    "div",
    { class: "object" },
    drawn.node,
    el(
      "span",
      { class: "object-detail" },
      `${object.noradId} · ${Math.round(object.periodMinutes)} min · ${object.inclinationDeg.toFixed(1)}°`,
    ),
    trail.node,
  );
}

/** The line under a layer's name: what it holds, and what it is doing with it. */
function describe(layer, ephemerides) {
  if (layer.status === "failed") return `failed — ${layer.error ?? "unknown error"}`;
  if (layer.status === "loading" && layer.objects === 0) return "loading…";

  const parts = [`${layer.objects.toLocaleString()} objects`];
  if (layer.tracked < layer.objects) {
    // The budget is the interesting case, so it is spelled out rather than left
    // as two numbers that happen not to match.
    parts.push(`${layer.tracked.toLocaleString()} drawn of ${ephemerides.maxTracked} the globe will propagate`);
  } else {
    parts.push(`${layer.tracked.toLocaleString()} drawn`);
  }
  if (layer.trails) parts.push(`${layer.trailed} trailed`);
  if (layer.rejected) parts.push(`${layer.rejected} records unusable`);

  const timing = [];
  if (layer.status === "loading") {
    timing.push("refetching…");
  } else {
    timing.push(`fetched ${duration(layer.ageSeconds)} ago`);
    if (layer.nextRefreshSeconds != null) {
      timing.push(`next in ${duration(layer.nextRefreshSeconds)}`);
    }
  }
  if (layer.oldestElementsDays != null) {
    // How much to trust the dots: SGP4 is a fit around its epoch, and drifts
    // roughly a kilometre a day away from it in low Earth orbit.
    const days = Math.abs(layer.oldestElementsDays);
    timing.push(`elements up to ${days < 1 ? `${Math.round(days * 24)} h` : `${days.toFixed(1)} days`} old`);
  }

  return [parts.join(" · "), ...timing].join(" — ");
}

/** The period box, floored at what a catalogue is worth asking for. */
function readPeriod(input) {
  const seconds = Number(input.value);
  return Number.isFinite(seconds) && seconds > 0
    ? Math.max(seconds, MIN_REFRESH_SECONDS)
    : MIN_REFRESH_SECONDS * 4;
}
