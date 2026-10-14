/**
 * Mission planning and mission flying, in that order.
 *
 * The porkchop plot is total delta-v as a contoured field over departure date
 * against arrival date, for a Lambert transfer between two of
 * `terramenta-solare`'s bodies — the standard tool an interplanetary launch
 * window is actually chosen with, because the cheapest date to leave and the
 * cheapest date to arrive are a joint choice, not two separate ones. It owns
 * its own state entirely: there is no globe snapshot a porkchop plot could be
 * read off, since what it draws depends on nothing the globe is currently
 * doing. It computes once, on request, into an inline SVG (`chart.js`'s
 * `svg()`). Inputs save as studies in the project's `analysis.porkchops`
 * (`analysis.js`); the first one opens and computes with the project. The
 * computed grid exports as CSV.
 *
 * "Launch" is the rest of the panel's own pattern again — `globe.addMission`
 * sent on a click, a list rebuilt from `state.missions` the way
 * `ephemeris.js` rebuilds its layer list from `state.ephemerides` — because
 * once a mission is up, what it is doing *is* something the globe is
 * currently doing.
 */

import * as analysis from "./analysis.js";
import { svg } from "./chart.js";
import { download, el, row, section } from "./dom.js";
import { clock, duration } from "./format.js";
import * as globe from "./globe.js";
import { button } from "./widgets.js";
import * as porkchop from "./porkchop.js";
import { isoUtc, toCsv } from "./reports.js";

const CHART_WIDTH = 520;
const CHART_HEIGHT = 380;
const MARGIN = { top: 12, right: 12, bottom: 40, left: 60 };
const TICK_COUNT = 5;
const CONTOUR_COUNT = 8;
// A cell right along the "arrival before departure" edge is asking for an
// almost-instantaneous transfer, which Lambert answers with a genuine but
// enormous delta-v — real chemical propulsion is never going to spend it.
// Capping the colour scale and the contour levels this far above the
// cheapest cell keeps that corner from crushing the whole interesting range
// into a sliver of blue.
const COST_CAP_ABOVE_MIN_KM_S = 20;
// 33x33 is dense enough for a smooth-looking field and few enough Lambert
// solves — computed twice, once to find the delta-v range and once more with
// contour levels spaced across it — to stay well under a second.
const GRID_STEPS = 33;
const DAY_SECONDS = 86_400;

function dateInputValue(date) {
  return date.toISOString().slice(0, 10);
}

function daysFromNow(days) {
  return dateInputValue(new Date(Date.now() + days * DAY_SECONDS * 1000));
}

/** `count` values strictly between `min` and `max` — never the boundary itself. */
function evenlySpaced(min, max, count) {
  return Array.from({ length: count }, (_, index) => min + ((max - min) * (index + 1)) / (count + 1));
}

/** Cheapest transfers read blue, priciest read red, with nothing implied beyond that ordering. */
function costColor(value, min, max) {
  // Clamped rather than left to wrap: a value above `max` is over the
  // display cap, and CSS treats an out-of-range hue as an angle rather than
  // an error, so an unclamped `t > 1` would wrap around to green instead of
  // saturating at red.
  const t = max > min ? Math.min((value - min) / (max - min), 1) : 0;
  return `hsl(${220 - 220 * t}deg 75% 45%)`;
}

function isoDate(unixSeconds) {
  return new Date(unixSeconds * 1000).toISOString().slice(0, 10);
}

export function mountMission(bind, signal) {
  const originSelect = el(
    "select",
    { class: "select" },
    el("option", { value: "", disabled: true }, "Loading…"),
  );
  const destinationSelect = el(
    "select",
    { class: "select" },
    el("option", { value: "", disabled: true }, "Loading…"),
  );

  const departureStart = el("input", { class: "coordinate", type: "date", value: daysFromNow(0) });
  const departureEnd = el("input", { class: "coordinate", type: "date", value: daysFromNow(120) });
  const arrivalStart = el("input", { class: "coordinate", type: "date", value: daysFromNow(150) });
  const arrivalEnd = el("input", { class: "coordinate", type: "date", value: daysFromNow(420) });

  const retrogradeInput = el("input", { type: "checkbox" });
  const retrogradeToggle = el(
    "label",
    { class: "toggle" },
    retrogradeInput,
    el("span", {}, "Retrograde (the long way round)"),
  );

  const status = el(
    "p",
    { class: "detail" },
    "Pick a departure window and an arrival window, then compute.",
  );
  const chart = svg("svg", {
    class: "porkchop-chart",
    viewBox: `0 0 ${CHART_WIDTH} ${CHART_HEIGHT}`,
    role: "img",
    "aria-label": "Porkchop plot",
  });

  const computeButton = button("Compute", compute);
  const stepsInput = el("input", { class: "coordinate", type: "number", min: "2", max: "101", step: "1", value: String(GRID_STEPS) });
  const best = el("p", { class: "detail", hidden: true });
  const csvButton = button("CSV", () => lastPlot && exportGrid(lastPlot, originSelect.value, destinationSelect.value));
  csvButton.disabled = true;
  let lastPlot = null;
  const studies = mountStudies(signal, currentStudy, (study) => openStudy(study), status);

  porkchop
    .load()
    .then(() => {
      populateBodies();
      const [first] = analysis.savedPorkchops();
      if (first) openStudy(first);
    })
    .catch((error) => {
      console.error(error);
      status.textContent = `Failed to load the porkchop module: ${error}`;
      computeButton.disabled = true;
    });

  function populateBodies() {
    const names = porkchop.bodies();
    const options = () => names.map((name) => el("option", { value: name }, name));
    originSelect.replaceChildren(...options());
    destinationSelect.replaceChildren(...options());
    originSelect.value = names.includes("Earth") ? "Earth" : names[0];
    destinationSelect.value = names.includes("Mars") ? "Mars" : names[names.length - 1];
  }

  function dateRange(startInput, endInput) {
    const start = new Date(`${startInput.value}T00:00:00Z`);
    const end = new Date(`${endInput.value}T00:00:00Z`);
    if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime()) || !(start < end)) return null;
    return { start, end, steps: gridSteps() };
  }

  function gridSteps() {
    const steps = Math.round(Number(stepsInput.value));
    return Number.isFinite(steps) ? Math.min(Math.max(steps, 2), 101) : GRID_STEPS;
  }

  /** Fills the inputs from a saved study and computes. */
  function openStudy(study) {
    originSelect.value = study.origin;
    destinationSelect.value = study.destination;
    departureStart.value = study.departure.start.slice(0, 10);
    departureEnd.value = study.departure.end.slice(0, 10);
    arrivalStart.value = study.arrival.start.slice(0, 10);
    arrivalEnd.value = study.arrival.end.slice(0, 10);
    retrogradeInput.checked = study.retrograde ?? false;
    stepsInput.value = String(study.steps ?? GRID_STEPS);
    studies.label.value = study.label ?? "";
    compute();
  }

  /** The inputs as a study; `null` if a range is invalid. */
  function currentStudy() {
    const departure = dateRange(departureStart, departureEnd);
    const arrival = dateRange(arrivalStart, arrivalEnd);
    if (!departure || !arrival) return null;
    const study = {
      origin: originSelect.value,
      destination: destinationSelect.value,
      departure: { start: departureStart.value, end: departureEnd.value },
      arrival: { start: arrivalStart.value, end: arrivalEnd.value },
    };
    if (retrogradeInput.checked) study.retrograde = true;
    if (gridSteps() !== GRID_STEPS) study.steps = gridSteps();
    return study;
  }

  async function compute() {
    const departure = dateRange(departureStart, departureEnd);
    const arrival = dateRange(arrivalStart, arrivalEnd);
    if (!departure || !arrival) {
      status.textContent = "Each window needs a start strictly before its end.";
      return;
    }

    computeButton.disabled = true;
    status.textContent = "Solving Lambert's problem across the grid…";
    try {
      await porkchop.load();
      const options = { retrograde: retrogradeInput.checked };
      const origin = originSelect.value;
      const destination = destinationSelect.value;

      // A first pass finds the delta-v range, since the levels to contour
      // have to be chosen from it; a second pass asks for the contours
      // themselves. The grid itself is identical both times — solving it
      // twice is simpler than threading the bounds back into a single call.
      const probe = porkchop.computePlot(origin, destination, departure, arrival, options);
      const bounds = probe?.totalDeltaVBoundsKmS ?? null;
      if (!probe || !bounds) {
        status.textContent =
          "No transfer solved anywhere in that window — try a wider range, or an " +
          "arrival window further past the departure window's start.";
        chart.replaceChildren();
        lastPlot = null;
        best.hidden = true;
        csvButton.disabled = true;
        return;
      }

      const [min, max] = bounds;
      const displayMax = Math.min(max, min + COST_CAP_ABOVE_MIN_KM_S);
      const plot = porkchop.computePlot(origin, destination, departure, arrival, {
        ...options,
        contourLevelsKmS: evenlySpaced(min, displayMax, CONTOUR_COUNT),
      });
      render(plot, displayMax);
      lastPlot = plot;
      csvButton.disabled = false;
      const cheapest = minimumCell(plot);
      best.hidden = !cheapest;
      if (cheapest) {
        best.textContent =
          `Minimum ${cheapest.totalDeltaVKmS.toFixed(3)} km/s ` +
          `(${cheapest.departureDeltaVKmS.toFixed(3)} + ${cheapest.arrivalDeltaVKmS.toFixed(3)}): ` +
          `depart ${isoDate(cheapest.departureUnixSeconds)}, arrive ${isoDate(cheapest.arrivalUnixSeconds)}, ` +
          `${cheapest.timeOfFlightDays.toFixed(0)} days`;
      }
      status.textContent =
        displayMax < max
          ? `Δv from ${min.toFixed(2)} km/s, chart capped at ${displayMax.toFixed(0)}+ — the far ` +
            `corner needs far more (up to ${max.toFixed(0)} km/s) for a near-instantaneous transfer. ` +
            "Blue is cheapest, red is priciest."
          : `Total Δv from ${min.toFixed(2)} to ${max.toFixed(2)} km/s across the grid — ` +
            "blue is cheapest, red is priciest.";
    } catch (error) {
      console.error(error);
      status.textContent = `Failed: ${error}`;
    } finally {
      computeButton.disabled = false;
    }
  }

  function render(plot, costMaxDisplay) {
    const { departuresUnixSeconds: xs, arrivalsUnixSeconds: ys, cells, contours, totalDeltaVBoundsKmS } = plot;
    const [costMin] = totalDeltaVBoundsKmS;
    const costMax = costMaxDisplay;
    const plotWidth = CHART_WIDTH - MARGIN.left - MARGIN.right;
    const plotHeight = CHART_HEIGHT - MARGIN.top - MARGIN.bottom;
    const [xMin, xMax] = [xs[0], xs[xs.length - 1]];
    const [yMin, yMax] = [ys[0], ys[ys.length - 1]];

    const toX = (unixSeconds) => MARGIN.left + ((unixSeconds - xMin) / (xMax - xMin)) * plotWidth;
    // Arrival increases upward — later dates toward the top, the way a
    // porkchop plot is conventionally drawn.
    const toY = (unixSeconds) =>
      MARGIN.top + plotHeight - ((unixSeconds - yMin) / (yMax - yMin)) * plotHeight;

    const cellWidth = plotWidth / xs.length;
    const cellHeight = plotHeight / ys.length;

    const cellRects = [];
    for (let departureIndex = 0; departureIndex < xs.length; departureIndex++) {
      for (let arrivalIndex = 0; arrivalIndex < ys.length; arrivalIndex++) {
        const cell = cells[departureIndex * ys.length + arrivalIndex];
        if (!cell) continue;
        cellRects.push(
          svg("rect", {
            x: toX(cell.departureUnixSeconds) - cellWidth / 2,
            y: toY(cell.arrivalUnixSeconds) - cellHeight / 2,
            width: cellWidth,
            height: cellHeight,
            fill: costColor(cell.totalDeltaVKmS, costMin, costMax),
          }, svg("title", {}, `${isoDate(cell.departureUnixSeconds)} → ${isoDate(cell.arrivalUnixSeconds)}: ${cell.totalDeltaVKmS.toFixed(2)} km/s`)),
        );
      }
    }

    const contourPaths = contours.map((contour) =>
      svg("path", {
        class: "porkchop-contour",
        d: contour.segments
          .map(([[x1, y1], [x2, y2]]) => `M${toX(x1)},${toY(y1)} L${toX(x2)},${toY(y2)}`)
          .join(" "),
      }),
    );

    const xTicks = evenlySpacedIndices(xs.length, TICK_COUNT).flatMap((index) => {
      const x = toX(xs[index]);
      return [
        svg("line", { class: "porkchop-axis", x1: x, y1: MARGIN.top, x2: x, y2: CHART_HEIGHT - MARGIN.bottom }),
        svg(
          "text",
          {
            class: "porkchop-axis-label",
            x,
            y: CHART_HEIGHT - MARGIN.bottom + 14,
            transform: `rotate(-30 ${x} ${CHART_HEIGHT - MARGIN.bottom + 14})`,
          },
          isoDate(xs[index]),
        ),
      ];
    });

    const yTicks = evenlySpacedIndices(ys.length, TICK_COUNT).flatMap((index) => {
      const y = toY(ys[index]);
      return [
        svg("line", { class: "porkchop-axis", x1: MARGIN.left, y1: y, x2: CHART_WIDTH - MARGIN.right, y2: y }),
        svg("text", { class: "porkchop-axis-label", x: MARGIN.left - 6, y: y + 3, "text-anchor": "end" }, isoDate(ys[index])),
      ];
    });

    const frame = svg("rect", {
      class: "porkchop-frame",
      x: MARGIN.left,
      y: MARGIN.top,
      width: plotWidth,
      height: plotHeight,
    });

    const cheapest = minimumCell(plot);
    const minimum = cheapest
      ? svg(
          "circle",
          { class: "porkchop-minimum", cx: toX(cheapest.departureUnixSeconds), cy: toY(cheapest.arrivalUnixSeconds), r: 4 },
          svg("title", {}, `Minimum ${cheapest.totalDeltaVKmS.toFixed(3)} km/s`),
        )
      : null;

    chart.replaceChildren(...cellRects, ...contourPaths, ...xTicks, ...yTicks, frame, minimum ?? "");
  }

  const dateRangeRow = (start, end) => el("div", { class: "porkchop-date-range" }, start, end);

  const controls = section(
    "Porkchop plot",
    row("From", originSelect),
    row("To", destinationSelect),
    row("Depart", dateRangeRow(departureStart, departureEnd)),
    row("Arrive", dateRangeRow(arrivalStart, arrivalEnd)),
    row("Grid steps", stepsInput),
    retrogradeToggle,
    el("div", { class: "buttons" }, computeButton, csvButton),
    status,
    best,
    el("div", { class: "porkchop-chart-wrap" }, chart),
    el(
      "p",
      { class: "detail" },
      "Solved with terramenta-charta's own Lambert-transfer grid — a real scan " +
        "of the two-body problem at every date pair, not a canned figure.",
    ),
  );

  const launch = mountLaunch(bind, originSelect, destinationSelect);

  return el("div", {}, controls, studies.node, launch);
}

/**
 * Launching a mission on the pair `originSelect`/`destinationSelect` are set
 * to, and the list of every one still up — searching, waiting on its
 * departure date, en route, arrived, or failed.
 *
 * Shares those two selects with the porkchop plot above rather than keeping
 * its own: the pair worth launching is the pair just planned for, and the
 * globe only knows how to fly between the three bodies `porkchop.bodies()`
 * offers anyway.
 */
function mountLaunch(bind, originSelect, destinationSelect) {
  let launched = 0;

  const launchButton = button("Launch", () => {
    globe.addMission(`mission-${++launched}`, {
      origin: originSelect.value,
      destination: destinationSelect.value,
    });
  });

  const list = el("div", { class: "layers" });
  const rows = new Map();

  bind(list, (state) => {
    const missions = state.missions;

    for (const [id, existing] of rows) {
      if (!missions.some((mission) => mission.id === id)) {
        existing.node.remove();
        rows.delete(id);
      }
    }

    for (const mission of missions) {
      let entry = rows.get(mission.id);
      if (!entry) {
        entry = missionRow(mission.id);
        rows.set(mission.id, entry);
        list.append(entry.node);
      }
      entry.sync(mission, state.sun.unixSeconds);
    }

    list.classList.toggle("empty", missions.length === 0);
  });

  return section(
    "Launch",
    el(
      "p",
      { class: "detail" },
      "Searches for a real launch window from wherever the simulated clock is " +
        "now, then departs the moment it gets there — watch it happen in the " +
        "heliocentric view.",
    ),
    el("div", { class: "buttons" }, launchButton),
    list,
  );
}

/** One mission: what it is doing, and a way to call it off. */
function missionRow(id) {
  const title = el("span", { class: "mission-title" }, "");
  const detail = el("p", { class: "detail" }, "");
  const node = el(
    "div",
    { class: "layer" },
    el("div", { class: "layer-head" }, title, button("Remove", () => globe.removeMission(id))),
    detail,
  );
  return {
    node,
    sync(mission, sunUnixSeconds) {
      node.dataset.status = mission.status;
      title.textContent = `${mission.origin} → ${mission.destination}`;
      detail.textContent = describeMission(mission, sunUnixSeconds);
    },
  };
}

/** The line under a mission's name: what it found, and where it is with it. */
function describeMission(mission, sunUnixSeconds) {
  switch (mission.status) {
    case "searching":
      return "searching for a launch window…";
    case "waiting":
      return (
        `window found — departs ${clock(mission.departureUnixSeconds)} ` +
        `(in ${duration(mission.departureUnixSeconds - sunUnixSeconds)}), ` +
        `arrives ${clock(mission.arrivalUnixSeconds)} — ` +
        `Δv ${mission.departureDeltaVKmS.toFixed(2)} + ${mission.arrivalDeltaVKmS.toFixed(2)} km/s`
      );
    case "enroute":
      return (
        `en route since ${clock(mission.departureUnixSeconds)}, now orbiting ${mission.orbiting} — ` +
        `arrival planned ${clock(mission.arrivalUnixSeconds)}`
      );
    case "arrived":
      return `arrived at ${mission.destination} on ${clock(mission.arrivalUnixSeconds)}`;
    case "failed":
      return `failed — ${mission.reason}`;
    default:
      return "—";
  }
}

/**
 * Saved studies: Save stores the inputs under the label (replacing a study
 * with that label); Open fills the inputs and computes.
 */
function mountStudies(signal, currentStudy, open, status) {
  const label = el("input", { class: "coordinate", type: "text", placeholder: "Study label" });
  const list = el("div", { class: "layers" });

  function rebuild() {
    const saved = analysis.savedPorkchops();
    list.replaceChildren(
      ...saved.map((study) =>
        el(
          "div",
          { class: "layer study" },
          el(
            "div",
            { class: "layer-head" },
            el("span", { class: "mission-title", title: study.label ?? study.id }, study.label ?? study.id),
            button("Open", () => open(study)),
            button("Remove", () => analysis.removePorkchop(study.id)),
          ),
          el(
            "p",
            { class: "detail" },
            `${study.origin} → ${study.destination} · depart ${study.departure.start.slice(0, 10)}–` +
              `${study.departure.end.slice(0, 10)} · arrive ${study.arrival.start.slice(0, 10)}–` +
              `${study.arrival.end.slice(0, 10)}${study.retrograde ? " · retrograde" : ""}`,
          ),
        ),
      ),
    );
    list.classList.toggle("empty", saved.length === 0);
  }

  function save() {
    const study = currentStudy();
    if (!study) {
      status.textContent = "Each window needs a start strictly before its end.";
      return;
    }
    const text = label.value.trim() || `${study.origin} → ${study.destination}`;
    const existing = analysis.savedPorkchops().find((other) => (other.label ?? other.id) === text);
    const id = existing?.id ?? analysis.uniqueId("porkchops", slug(text) || "study");
    analysis.addPorkchop({ id, label: text, ...study });
  }

  const unsubscribe = analysis.subscribe(rebuild);
  signal?.addEventListener("abort", unsubscribe);
  rebuild();

  const node = section(
    "Porkchop studies",
    row("Label", label),
    el("div", { class: "buttons" }, button("Save study", save)),
    list,
  );
  return { node, label };
}

/** The solved cell with the lowest total Δv; `null` if none solved. */
function minimumCell(plot) {
  let cheapest = null;
  for (const cell of plot.cells) {
    if (cell && (!cheapest || cell.totalDeltaVKmS < cheapest.totalDeltaVKmS)) cheapest = cell;
  }
  return cheapest;
}

/** Every grid cell as CSV, unsolved cells with empty values. */
function exportGrid(plot, origin, destination) {
  const { departuresUnixSeconds: xs, arrivalsUnixSeconds: ys, cells } = plot;
  const rows = [];
  xs.forEach((departure, i) =>
    ys.forEach((arrival, j) => {
      const cell = cells[i * ys.length + j];
      rows.push([
        isoUtc(departure),
        isoUtc(arrival),
        cell?.timeOfFlightDays ?? NaN,
        cell?.departureDeltaVKmS ?? NaN,
        cell?.arrivalDeltaVKmS ?? NaN,
        cell?.totalDeltaVKmS ?? NaN,
      ]);
    }),
  );
  const header = ["departure_utc", "arrival_utc", "tof_days", "departure_dv_km_s", "arrival_dv_km_s", "total_dv_km_s"];
  download(
    `porkchop-${slug(origin)}-${slug(destination)}.csv`,
    new Blob([toCsv(header, rows)], { type: "text/csv" }),
  );
}

function slug(text) {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}

/** `count` indices spanning `0..length-1`, including both ends. */
function evenlySpacedIndices(length, count) {
  if (length <= count) return Array.from({ length }, (_, index) => index);
  return Array.from({ length: count }, (_, index) => Math.round((index * (length - 1)) / (count - 1)));
}
