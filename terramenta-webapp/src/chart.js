/**
 * Inline SVG charts: `svg()` element builder, a time-series line chart, a
 * histogram and a scatter plot.
 * Sizes are viewBox units; CSS scales the element to its container.
 */

const SVG_NS = "http://www.w3.org/2000/svg";
const MARGIN = { top: 16, right: 8, bottom: 30, left: 50 };
const TICKS = 5;
/** Per-series points drawn at most this many min/max buckets (twice the points). */
const BUCKETS = 400;

/** `el()` for SVG: attributes set verbatim, `null` children skipped. */
export function svg(tag, attrs = {}, ...children) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value !== undefined && value !== null) node.setAttribute(key, value);
  }
  node.append(...children.flat().filter((child) => child != null));
  return node;
}

/**
 * `series`: `[{label, color, points: [[unixSeconds, value], …]}]`, non-finite
 * values break the line. `markers`: `[{t, label}]` vertical rules (burns).
 * `onPick(unixSeconds)` on click. → `{node, setCursor(unixSeconds)}`.
 */
export function lineChart({ series, markers = [], unit = "", width = 320, height = 200, onPick }) {
  const finite = series.flatMap((entry) => entry.points).filter(([, value]) => Number.isFinite(value));
  const node = svg("svg", { class: "line-chart", viewBox: `0 0 ${width} ${height}`, role: "img" });
  if (!finite.length) {
    node.append(svg("text", { class: "chart-empty", x: width / 2, y: height / 2, "text-anchor": "middle" }, "No data"));
    return { node, setCursor() {} };
  }

  let [xMin, xMax] = extent(finite.map(([t]) => t));
  let [yMin, yMax] = extent(finite.map(([, value]) => value));
  if (xMax === xMin) [xMin, xMax] = [xMin - 1, xMax + 1];
  if (yMax === yMin) [yMin, yMax] = [yMin - (Math.abs(yMin) || 1) * 0.01, yMax + (Math.abs(yMax) || 1) * 0.01];
  const yTicks = niceTicks(yMin, yMax, TICKS);
  [yMin, yMax] = [Math.min(yMin, yTicks[0]), Math.max(yMax, yTicks.at(-1))];

  const plotWidth = width - MARGIN.left - MARGIN.right;
  const plotHeight = height - MARGIN.top - MARGIN.bottom;
  const toX = (t) => MARGIN.left + ((t - xMin) / (xMax - xMin)) * plotWidth;
  const toY = (value) => MARGIN.top + plotHeight - ((value - yMin) / (yMax - yMin)) * plotHeight;

  const grid = yTicks.flatMap((value) => [
    svg("line", { class: "chart-grid", x1: MARGIN.left, x2: width - MARGIN.right, y1: toY(value), y2: toY(value) }),
    svg("text", { class: "chart-label", x: MARGIN.left - 6, y: toY(value) + 3, "text-anchor": "end" }, tickText(value)),
  ]);
  // Times of day fit five ticks across; dates fit three.
  const short = xMax - xMin < 2 * 86_400;
  const xCount = short ? TICKS : 3;
  const xTicks = Array.from({ length: xCount }, (_, index) => xMin + ((xMax - xMin) * index) / (xCount - 1));
  const timeFormat = short ? (t) => isoText(t).slice(11, 16) : (t) => isoText(t).slice(0, 10);
  const xLabels = xTicks.map((t, index) =>
    svg(
      "text",
      {
        class: "chart-label",
        x: toX(t),
        y: height - MARGIN.bottom + 14,
        "text-anchor": index === 0 ? "start" : index === xCount - 1 ? "end" : "middle",
      },
      timeFormat(t),
    ),
  );
  const unitLabel = unit
    ? svg("text", { class: "chart-label", x: MARGIN.left - 6, y: MARGIN.top - 6, "text-anchor": "end" }, unit)
    : null;
  const spanLabel = svg(
    "text",
    { class: "chart-label", x: width - MARGIN.right, y: height - 4, "text-anchor": "end" },
    `${isoText(xMin).slice(0, 16)} → ${isoText(xMax).slice(0, 16)} UTC`,
  );

  const rules = markers
    .filter(({ t }) => t >= xMin && t <= xMax)
    .map(({ t, label }) =>
      svg(
        "line",
        { class: "chart-marker", x1: toX(t), x2: toX(t), y1: MARGIN.top, y2: MARGIN.top + plotHeight },
        svg("title", {}, label),
      ),
    );

  const lines = series.map(({ label, color, points }) =>
    svg(
      "path",
      { class: "chart-line", stroke: color, d: pathData(decimate(points, xMin, xMax), toX, toY) },
      svg("title", {}, label),
    ),
  );

  const frame = svg("rect", { class: "chart-frame", x: MARGIN.left, y: MARGIN.top, width: plotWidth, height: plotHeight });
  const cursor = svg("line", { class: "chart-cursor", y1: MARGIN.top, y2: MARGIN.top + plotHeight, visibility: "hidden" });
  node.append(...grid, ...rules, ...lines, frame, ...xLabels, unitLabel ?? "", spanLabel, cursor);

  if (onPick) {
    node.classList.add("pickable");
    node.addEventListener("click", (event) => {
      const box = node.getBoundingClientRect();
      const x = ((event.clientX - box.left) / box.width) * width;
      const fraction = (x - MARGIN.left) / plotWidth;
      if (fraction >= 0 && fraction <= 1) onPick(xMin + fraction * (xMax - xMin));
    });
  }

  return {
    node,
    setCursor(t) {
      const inside = Number.isFinite(t) && t >= xMin && t <= xMax;
      cursor.setAttribute("visibility", inside ? "visible" : "hidden");
      if (!inside) return;
      cursor.setAttribute("x1", toX(t));
      cursor.setAttribute("x2", toX(t));
    },
  };
}

/**
 * Bars of `counts` over `edges` (`counts.length + 1` ascending). `marker`: a
 * value drawn as a rule (nominal). `unit` labels the x axis.
 */
export function histogramChart({ edges, counts, marker, color, unit = "", width = 320, height = 160 }) {
  const node = svg("svg", { class: "line-chart", viewBox: `0 0 ${width} ${height}`, role: "img" });
  if (!counts.length) {
    node.append(svg("text", { class: "chart-empty", x: width / 2, y: height / 2, "text-anchor": "middle" }, "No data"));
    return node;
  }
  let [xMin, xMax] = [Math.min(edges[0], marker ?? Infinity), Math.max(edges.at(-1), marker ?? -Infinity)];
  if (xMax === xMin) [xMin, xMax] = [xMin - (Math.abs(xMin) || 1) * 1e-6, xMax + (Math.abs(xMax) || 1) * 1e-6];
  const { toX, toY, parts } = numericAxes({ xMin, xMax, yMin: 0, yMax: Math.max(...counts), width, height, xUnit: unit });
  const bars = counts.map((count, index) => {
    const x0 = edges[index] === edges[index + 1] ? toX(edges[index]) - 2 : toX(edges[index]);
    const x1 = edges[index] === edges[index + 1] ? toX(edges[index]) + 2 : toX(edges[index + 1]);
    return svg(
      "rect",
      { class: "chart-bar", fill: color, x: x0, y: toY(count), width: Math.max(x1 - x0 - 0.5, 0.5), height: toY(0) - toY(count) },
      svg("title", {}, `${tickText(edges[index])} – ${tickText(edges[index + 1])}: ${count}`),
    );
  });
  const rule = Number.isFinite(marker)
    ? svg("line", { class: "chart-marker", x1: toX(marker), x2: toX(marker), y1: MARGIN.top, y2: toY(0) }, svg("title", {}, `Nominal ${tickText(marker)}`))
    : null;
  node.append(...parts.grid, ...bars, rule ?? "", parts.frame, ...parts.labels);
  return node;
}

/** `points`: `[[x, y]]`, drawn as dots; `origin` adds a cross at (0, 0). */
export function scatterChart({ points, color, xUnit = "", yUnit = "", origin = false, width = 320, height = 220 }) {
  const node = svg("svg", { class: "line-chart", viewBox: `0 0 ${width} ${height}`, role: "img" });
  const finite = points.filter(([x, y]) => Number.isFinite(x) && Number.isFinite(y));
  if (!finite.length) {
    node.append(svg("text", { class: "chart-empty", x: width / 2, y: height / 2, "text-anchor": "middle" }, "No data"));
    return node;
  }
  const pad = (lo, hi) => (hi === lo ? [lo - (Math.abs(lo) || 1) * 0.01, hi + (Math.abs(hi) || 1) * 0.01] : [lo, hi]);
  const [xMin, xMax] = pad(...extent(finite.map(([x]) => x).concat(origin ? [0] : [])));
  const [yMin, yMax] = pad(...extent(finite.map(([, y]) => y).concat(origin ? [0] : [])));
  const { toX, toY, parts } = numericAxes({ xMin, xMax, yMin, yMax, width, height, xUnit, yUnit });
  const dots = finite.map(([x, y]) => svg("circle", { class: "chart-dot", fill: color, cx: toX(x).toFixed(1), cy: toY(y).toFixed(1), r: 1.8 }));
  const cross = origin
    ? [
        svg("line", { class: "chart-marker", x1: toX(0) - 5, x2: toX(0) + 5, y1: toY(0), y2: toY(0) }),
        svg("line", { class: "chart-marker", x1: toX(0), x2: toX(0), y1: toY(0) - 5, y2: toY(0) + 5 }),
      ]
    : [];
  node.append(...parts.grid, ...dots, ...cross, parts.frame, ...parts.labels);
  return node;
}

/** Linear x and y axes with nice ticks; ranges widened to them. */
function numericAxes({ xMin, xMax, yMin, yMax, width, height, xUnit = "", yUnit = "" }) {
  const xTicks = niceTicks(xMin, xMax, TICKS);
  const yTicks = niceTicks(yMin, yMax || 1, TICKS);
  [xMin, xMax] = [Math.min(xMin, xTicks[0]), Math.max(xMax, xTicks.at(-1))];
  [yMin, yMax] = [Math.min(yMin, yTicks[0]), Math.max(yMax, yTicks.at(-1))];
  const plotWidth = width - MARGIN.left - MARGIN.right;
  const plotHeight = height - MARGIN.top - MARGIN.bottom;
  const toX = (x) => MARGIN.left + ((x - xMin) / (xMax - xMin)) * plotWidth;
  const toY = (y) => MARGIN.top + plotHeight - ((y - yMin) / (yMax - yMin)) * plotHeight;
  const grid = yTicks.flatMap((value) => [
    svg("line", { class: "chart-grid", x1: MARGIN.left, x2: width - MARGIN.right, y1: toY(value), y2: toY(value) }),
    svg("text", { class: "chart-label", x: MARGIN.left - 6, y: toY(value) + 3, "text-anchor": "end" }, tickText(value)),
  ]);
  const labels = xTicks.map((value, index) =>
    svg(
      "text",
      {
        class: "chart-label",
        x: toX(value),
        y: height - MARGIN.bottom + 14,
        "text-anchor": index === 0 ? "start" : index === xTicks.length - 1 ? "end" : "middle",
      },
      tickText(value),
    ),
  );
  if (xUnit) labels.push(svg("text", { class: "chart-label", x: width - MARGIN.right, y: height - 4, "text-anchor": "end" }, xUnit));
  if (yUnit) labels.push(svg("text", { class: "chart-label", x: MARGIN.left - 6, y: MARGIN.top - 6, "text-anchor": "end" }, yUnit));
  const frame = svg("rect", { class: "chart-frame", x: MARGIN.left, y: MARGIN.top, width: plotWidth, height: plotHeight });
  return { toX, toY, parts: { grid, labels, frame } };
}

/** Min and max of each of `BUCKETS` time buckets, in time order; small series untouched. */
function decimate(points, xMin, xMax) {
  if (points.length <= BUCKETS * 2) return points;
  const buckets = Array.from({ length: BUCKETS }, () => null);
  for (const point of points) {
    const index = Math.min(Math.floor(((point[0] - xMin) / (xMax - xMin)) * BUCKETS), BUCKETS - 1);
    const bucket = (buckets[index] ??= { min: point, max: point, gap: false });
    if (!Number.isFinite(point[1])) bucket.gap = true;
    else {
      if (!(bucket.min[1] <= point[1])) bucket.min = point;
      if (!(bucket.max[1] >= point[1])) bucket.max = point;
    }
  }
  return buckets.flatMap((bucket) => {
    if (!bucket) return [];
    const pair = bucket.min[0] <= bucket.max[0] ? [bucket.min, bucket.max] : [bucket.max, bucket.min];
    return bucket.gap ? [...pair, [pair[1][0], NaN]] : pair;
  });
}

function pathData(points, toX, toY) {
  let pen = "M";
  const parts = [];
  for (const [t, value] of points) {
    if (!Number.isFinite(value)) {
      pen = "M";
      continue;
    }
    parts.push(`${pen}${toX(t).toFixed(1)},${toY(value).toFixed(1)}`);
    pen = "L";
  }
  return parts.join("");
}

function extent(values) {
  let min = Infinity;
  let max = -Infinity;
  for (const value of values) {
    if (value < min) min = value;
    if (value > max) max = value;
  }
  return [min, max];
}

/** ~`count` round values covering `[min, max]`. */
export function niceTicks(min, max, count) {
  const raw = (max - min) / Math.max(count - 1, 1);
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((factor) => factor * magnitude).find((candidate) => candidate >= raw);
  const first = Math.floor(min / step) * step;
  const ticks = [];
  for (let value = first; value <= max + step * 1e-9; value += step) ticks.push(Number(value.toPrecision(12)));
  if (ticks.at(-1) < max) ticks.push(Number((ticks.at(-1) + step).toPrecision(12)));
  return ticks;
}

function tickText(value) {
  const magnitude = Math.abs(value);
  if (magnitude !== 0 && (magnitude >= 1e6 || magnitude < 1e-3)) return value.toExponential(2);
  return Number(value.toPrecision(6)).toLocaleString();
}

function isoText(unixSeconds) {
  return new Date(unixSeconds * 1000).toISOString().replace("T", " ");
}
