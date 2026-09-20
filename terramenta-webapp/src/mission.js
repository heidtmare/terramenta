/**
 * The porkchop plot: total delta-v as a contoured field over departure date
 * against arrival date, for a Lambert transfer between two of
 * `terramenta-solare`'s bodies — the standard tool an interplanetary launch
 * window is actually chosen with, because the cheapest date to leave and the
 * cheapest date to arrive are a joint choice, not two separate ones.
 *
 * Unlike the rest of the panel this owns its own state entirely: there is no
 * globe snapshot a porkchop plot could be read off, since what it draws
 * depends on nothing the globe is currently doing. It computes once, on
 * request, into an inline SVG built by hand — `dom.js`'s `el()` creates HTML
 * elements, which is not what an `<svg>` needs, so this file has its own
 * tiny version of it.
 */

import { el, row, section } from "./dom.js";
import { button } from "./widgets.js";
import * as porkchop from "./porkchop.js";

const SVG_NS = "http://www.w3.org/2000/svg";
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

function svg(tag, attrs = {}, ...children) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
  node.append(...children.flat().filter((child) => child != null));
  return node;
}

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

export function mountMission() {
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

  porkchop
    .load()
    .then(populateBodies)
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
    return { start, end, steps: GRID_STEPS };
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
        return;
      }

      const [min, max] = bounds;
      const displayMax = Math.min(max, min + COST_CAP_ABOVE_MIN_KM_S);
      const plot = porkchop.computePlot(origin, destination, departure, arrival, {
        ...options,
        contourLevelsKmS: evenlySpaced(min, displayMax, CONTOUR_COUNT),
      });
      render(plot, displayMax);
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
          }),
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

    chart.replaceChildren(...cellRects, ...contourPaths, ...xTicks, ...yTicks, frame);
  }

  const dateRangeRow = (start, end) => el("div", { class: "porkchop-date-range" }, start, end);

  const controls = section(
    "Porkchop plot",
    row("From", originSelect),
    row("To", destinationSelect),
    row("Depart", dateRangeRow(departureStart, departureEnd)),
    row("Arrive", dateRangeRow(arrivalStart, arrivalEnd)),
    retrogradeToggle,
    el("div", { class: "buttons" }, computeButton),
    status,
    el("div", { class: "porkchop-chart-wrap" }, chart),
    el(
      "p",
      { class: "detail" },
      "Solved with terramenta-charta's own Lambert-transfer grid — a real scan " +
        "of the two-body problem at every date pair, not a canned figure.",
    ),
  );

  return controls;
}

/** `count` indices spanning `0..length-1`, including both ends. */
function evenlySpacedIndices(length, count) {
  if (length <= count) return Array.from({ length }, (_, index) => index);
  return Array.from({ length: count }, (_, index) => Math.round((index * (length - 1)) / (count - 1)));
}
