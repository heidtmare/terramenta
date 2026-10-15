// Records docs/tour.mp4 and docs/tour.gif from the bundled projects.
//
//   (cd terramenta-webapp && python3 -m http.server 8091) &
//   npm i --no-save playwright && npx playwright install chromium
//   node docs/tour.mjs [segment,...]      frames → docs/tour-frames/ (headed Chromium, WebGPU)
//
// Projects switch in place; CDP screencast frames are kept only while a
// segment runs, then concatenated with their capture timestamps (see end).
import { chromium } from "playwright";
import fs from "node:fs";

const BASE = "http://localhost:8091/index.html";
const OUT = new URL("./tour-frames/", import.meta.url).pathname;
const only = process.argv[2];
fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });

const browser = await chromium.launch({
  headless: false,
  args: ["--enable-unsafe-webgpu", "--window-size=1600,1000", "--disable-background-timer-throttling"],
});
const page = await browser.newPage({ viewport: { width: 1600, height: 900 } });
page.on("dialog", (d) => d.accept());
// Celestrak is unreachable here: fail fast so environments fall back, and drop
// the catalogue from the conjunction study rather than show a load error.
await page.route(/celestrak\.org/, (r) => r.abort());
await page.route(/conjunction-screening\.terramenta\.json/, async (r) => {
  const d = await (await r.fetch()).json();
  delete d.ephemerides;
  for (const c of d.analysis.conjunctions) delete c.layers;
  r.fulfill({ json: d });
});
page.on("pageerror", (e) => console.log("pageerror:", String(e).slice(0, 200)));

let recording = false;
const frames = [];
const cdp = await page.context().newCDPSession(page);
cdp.on("Page.screencastFrame", async ({ data, metadata, sessionId }) => {
  cdp.send("Page.screencastFrameAck", { sessionId }).catch(() => {});
  if (!recording) return;
  const file = `${OUT}${String(frames.length).padStart(5, "0")}.jpg`;
  fs.writeFileSync(file, Buffer.from(data, "base64"));
  frames.push({ file, t: metadata.timestamp });
});

const g = (fn, ...args) =>
  page.evaluate(async ([fn, args]) => (await import("/src/globe.js"))[fn](...args), [fn, args]);
const wait = (ms) => page.waitForTimeout(ms);

async function open(project) {
  await page.evaluate(async (url) => {
    const ws = await import("/src/workspace.js");
    await ws.openUrl(url);
  }, `projects/${project}.terramenta.json`);
}

async function caption(title, detail) {
  await page.evaluate(
    ([title, detail]) => {
      let el = document.getElementById("tour-caption");
      if (!el) {
        const style = document.createElement("style");
        style.textContent = `
          #tour-caption { position: fixed; bottom: 34px; left: calc((100vw - 300px) / 2); transform: translateX(-50%);
            z-index: 50; text-align: center; pointer-events: none; transition: opacity .35s; opacity: 0;
            font-family: ui-monospace, "SF Mono", Menlo, monospace; color: #e8eef7; max-width: 900px;
            background: rgba(6, 10, 18, .72); border: 1px solid rgba(140, 180, 230, .25); border-radius: 4px; padding: 12px 22px; }
          #tour-caption b { display: block; font-size: 19px; letter-spacing: .14em; text-transform: uppercase; font-weight: 600; }
          #tour-caption span { display: block; font-size: 13px; margin-top: 6px; color: #9fb3cc; letter-spacing: .03em; }`;
        document.head.append(style);
        el = document.createElement("div");
        el.id = "tour-caption";
        document.body.append(el);
      }
      el.innerHTML = `<b></b><span></span>`;
      el.querySelector("b").textContent = title;
      el.querySelector("span").textContent = detail;
      el.style.opacity = title ? "1" : "0";
    },
    [title, detail],
  );
}

/** Orbits the camera by `yaw`/`pitch` degrees per second for `ms`. */
const drift = (ms, yaw, pitch = 0, zoom = 0) =>
  page.evaluate(
    async ([ms, yaw, pitch, zoom]) => {
      const g = await import("/src/globe.js");
      let last = performance.now();
      const end = last + ms;
      await new Promise((done) => {
        const tick = (now) => {
          const dt = (now - last) / 1000;
          last = now;
          if (yaw || pitch) g.orbitBy(yaw * dt, pitch * dt);
          if (zoom) g.zoomBy(zoom * dt);
          now < end ? requestAnimationFrame(tick) : done();
        };
        requestAnimationFrame(tick);
      });
    },
    [ms, yaw, pitch, zoom],
  );

/** Keeps the camera `aboveKm` over ephemeris layer `id`'s first point for `ms`. */
const chase = (ms, id, aboveKm) =>
  page.evaluate(
    async ([ms, id, aboveKm]) => {
      const g = await import("/src/globe.js");
      const end = performance.now() + ms;
      await new Promise((done) => {
        const tick = (now) => {
          const c = g.ephemerisGeometry(id)?.points?.coords;
          if (c?.length) g.lookAt(c[1], c[0], c[2] / 1000 + aboveKm);
          now < end ? requestAnimationFrame(tick) : done();
        };
        requestAnimationFrame(tick);
      });
    },
    [ms, id, aboveKm],
  );

const ready = (id) =>
  page.waitForFunction(
    async (id) => (await import("/src/globe.js")).ephemerisGeometry(id)?.points?.coords.length > 0,
    id,
    { timeout: 60000, polling: 250 },
  );

async function clickButton(section, text) {
  const btn = page.locator("#panel section, #panel details, #panel .section").filter({ hasText: section })
    .locator("button", { hasText: new RegExp(`^${text}$`) }).first();
  await btn.scrollIntoViewIfNeeded().catch(() => {});
  await btn.click({ timeout: 5000 }).catch((e) => console.log("click failed", section, text, String(e).slice(0, 120)));
}

async function tab(name) {
  await page.locator("#panel [role=tab], #panel nav button, #panel .tabs button", { hasText: new RegExp(`^${name}$`, "i") })
    .first().click({ timeout: 3000 }).catch(() => console.log("tab missing", name));
}

const segments = [
  {
    id: "globe",
    project: "overview",
    async setup() {
      await g("setClock", Date.parse("2026-06-21T12:00:00Z") / 1000);
      await g("lookAt", 30, -20, 15000);
      await wait(6000);
    },
    async run() {
      await caption("WebGPU globe", "Rust + Bevy → WebAssembly · WMTS imagery · vector tiles · GeoJSON · live feeds");
      await drift(7000, 7, 0, 0.05);
    },
  },
  {
    id: "contacts",
    project: "ground-contacts",
    async setup() {
      await g("lookAt", 65, 15, 14000);
      await wait(4000);
    },
    async run() {
      await caption("Events & ground contacts", "AOS/LOS, eclipses, nodes, apsides · station masks · terrain horizons");
      await drift(7000, 6);
    },
  },
  {
    id: "coverage",
    project: "coverage-study",
    async setup() {
      await g("lookAt", 50, 12, 8500);
      await wait(6000);
    },
    async run() {
      await caption("Sensor coverage", "Footprints on WGS-84 · H3 revisit, gap and access statistics");
      await drift(8000, -4, 0, 0.03);
    },
  },
  {
    id: "attitude",
    project: "attitude-modes",
    async setup() {
      await ready("tracker");
      await chase(3000, "tracker", 2500);
    },
    async run() {
      await caption("Attitude", "Align/constrain laws, LVLH, inertial spin · body axes and boresights · AEM");
      await chase(7000, "tracker", 2500);
    },
  },
  {
    id: "covariance",
    project: "leo-transfer",
    async setup() {
      await ready("transfer");
      await chase(500, "transfer", 20000);
      await wait(2500);
    },
    async run() {
      await caption("Cowell propagation & covariance", "EGM2008, NRLMSISE-00, SRP, third body · STM · 3σ ellipsoids · Δv budget");
      await drift(7000, 6);
    },
  },
  {
    id: "sequence",
    project: "geo-transfer",
    async setup() {
      await ready("geo");
      await g("setEphemerisTrail", "geo", { leadingOrbits: 1, trailingOrbits: 1, trailSamples: 720, trailPath: "orbit" });
      await g("setClock", Date.parse("2026-03-21T02:30:00Z") / 1000);
      await wait(500);
      await page.evaluate(async () => {
        const g = await import("/src/globe.js");
        const c = g.ephemerisGeometry("geo").points.coords;
        g.lookAt(35, c[0] - 50, 90000);
      });
      await page.locator("#panel summary", { hasText: "Sequence" }).first().click().catch(() => console.log("no sequence summary"));
      await wait(3000);
    },
    async run() {
      await caption("Mission sequences & targeting", "Propagate-until, burns, nested differential correction · LEO → GEO");
      await drift(7000, 8);
    },
  },
  {
    id: "conjunction",
    project: "conjunction-screening",
    async setup() {
      await wait(3000);
      await clickButton("Conjunctions", "Run");
      await wait(6000);
    },
    async run() {
      await caption("Conjunction assessment", "TCA search · Foster Pc · Alfano max Pc · CDM in and out");
      await drift(7000, 4);
    },
  },
  {
    id: "od",
    project: "orbit-determination",
    async setup() {
      await g("lookAt", 40, -40, 18000);
      await wait(3000);
      for (let i = 0; i < 3; i++) {
        await clickButton("Orbit determination", "Run");
        await wait(2500);
      }
      await wait(4000);
    },
    async run() {
      await caption("Orbit determination", "Batch least squares, EKF · radar and GNSS · residuals · TDM");
      await drift(7000, 5);
    },
  },
  {
    id: "porkchop",
    project: "mars-windows",
    async setup() {
      await wait(6000);
    },
    async run() {
      await caption("Interplanetary design", "Lambert porkchops · launch-window search · heliocentric view");
      await drift(7000, 4);
    },
  },
];

await page.goto(`${BASE}?project=projects/overview.terramenta.json`);
await page.waitForFunction(() => document.getElementById("status").hidden, null, { timeout: 120000 });
await cdp.send("Page.startScreencast", { format: "jpeg", quality: 92, maxWidth: 1600, maxHeight: 900, everyNthFrame: 1 });

const marks = [];
for (const s of segments) {
  if (only && !only.split(",").includes(s.id)) continue;
  console.log("segment", s.id);
  await caption("", "");
  if (s.project !== "overview" || marks.length) await open(s.project);
  await s.setup();
  recording = true;
  const start = frames.length;
  await s.run();
  await caption("", "");
  await wait(400);
  recording = false;
  marks.push({ id: s.id, start, end: frames.length });
}
await cdp.send("Page.stopScreencast");
await browser.close();

// ffconcat list: each frame held until the next capture, capped at 0.2 s.
let list = "ffconcat version 1.0\n";
for (const m of marks)
  for (let i = m.start; i < m.end; i++) {
    const d = i + 1 < m.end ? frames[i + 1].t - frames[i].t : 1 / 30;
    list += `file ${frames[i].file}\nduration ${Math.min(Math.max(d, 0.001), 0.2).toFixed(4)}\n`;
  }
fs.writeFileSync(`${OUT}list.txt`, list);
console.log(marks);
console.log(`ffmpeg -f concat -safe 0 -i ${OUT}list.txt -vf fps=30,format=yuv420p -c:v libx264 -preset veryslow -crf 28 -tune animation -movflags +faststart docs/tour.mp4
ffmpeg -i docs/tour.mp4 -vf "setpts=PTS/1.4,fps=8,scale=640:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=96[p];[b][p]paletteuse=dither=none:diff_mode=rectangle" tour.gif
gifsicle -O3 --lossy=60 tour.gif -o docs/tour.gif`);
