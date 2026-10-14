# terramenta-webapp

Reference client for [`terramenta-globe`](../terramenta-globe/) and
[`terramenta-charta`](../terramenta-charta/). Plain HTML, CSS and ES modules:
no dependencies, no bundler. One page, `index.html`, which loads a project file.

| URL | Loads |
| --- | --- |
| `index.html` | Empty project |
| `index.html?project=projects/overview.terramenta.json` | Bundled project (any same-origin or CORS URL) |
| `index.html?project=session` | Local file opened from the Project tab or dropped (`sessionStorage`) |

`?project=` tracks the open project, so a reload reopens it.

## Running

```sh
./scripts/serve.sh             # builds the WASM modules if missing, serves http://localhost:8080
./scripts/serve.sh 3000        # alternate port
./scripts/build.sh --release   # rebuild globe/ and charta/ with the web-release profile
```

`build.sh` runs `terramenta-globe/scripts/build-wasm.sh` into `globe/` and
`terramenta-charta/scripts/build-wasm.sh` into `charta/`. The app itself has no
build step. WebGPU requires a secure context, so the pages must be served (not
opened from `file://`).

```sh
npm test                       # node --test (Node 22+); no dependencies
```

Tests ([`test/`](test/)) cover the schema validator, project validation,
migrations, spacecraft/environment application, timeline resolution and
runner, and capture → parse → apply round trips against a fake globe module (`globe.attach`).

## Project files

`*.terramenta.json`, described by
[`schema/project.schema.json`](schema/project.schema.json) (JSON Schema
2020-12; point `$schema` at it for editor completion). Every project, bundled
or user-supplied, loads the same way: fetch/read → upgrade → validate →
`globe.reset()` → queue onto the globe. The first project is queued before
`start`; later ones switch in place (no reload). Omitted fields keep
fresh-globe defaults.

Validation ([`schema.js`](src/schema.js), [`project.js`](src/project.js)
`parseProject`): structure against the schema, then ids and references
(duplicate ids, `url` xor `text`, `camera.follow`, `gnc.focus.layer`, director
mission, timeline event references and cycles). Errors list every failing path. Unknown keys warn and are ignored, so
files with keys from newer builds still open.

Versions: older files are upgraded on load (`migrateProject`) and saved at the
current version; newer ones are refused.

| Version | Change |
| --- | --- |
| 1 | Initial |
| 2 | `picking` → `overlays.picking` |

Relative URLs resolve against the app root (the directory holding
`index.html`), not the project file.

| Key | Type | Applies |
| --- | --- | --- |
| `terramenta` | `2` | Format version. Required. |
| `name`, `description` | string | Masthead, Project tab, page title |
| `ui.panel` | `"full"` \| `"flight"` | Full tabbed panel (default) or [flight panel](#flight-panel) |
| `ui.tab` | `"project"` \| `"imagery"` \| `"data"` \| `"spacecraft"` \| `"timeline"` \| `"other"` \| `"mission"` | Initial tab (full panel) |
| `ui.readout`, `ui.collapsed` | bool | Telemetry + picked-item panels (default on); panel collapsed |
| `view` | `"globe"` \| `"heliocentric"` | Initial view, no transition |
| `frame` | `"ecef"` \| `"eci"` | `setFrame` |
| `clock` | `{epoch, timeScale, paused, shaded}` | `epoch`: ISO 8601 UTC or `"now"` |
| `camera` | `{lat, lon, altitudeKm, follow \| anchor}` | `lookAt`; `follow`: mission id; `anchor`: `sun`/`earth`/`mars`/`barycenter` |
| `imagery`, `vectorTiles` | `{enabled, layer, style?}` | `layer`: preset label or index |
| `overlays` | `{enabled, picking, layers: [{id, ...addOverlay options}]}` | One of `url`/`text` per layer |
| `ephemerides` | `{enabled, layers: [{id, ...addEphemeris options}]}` | OMM JSON or KVN OEM |
| `environment` | `{eop, spaceWeather, kernel}` | URLs; see [Spacecraft](#spacecraft) |
| `spacecraft` | `[{id, ...addEphemeris display options, propagation}]` | `propagation`: `propagate` options; one of `stop`/`durationS`. Id unique across `ephemerides` |
| `missions` | `[{id, ...addMission options}]` | |
| `placemarks.enabled`, `keyboard` | bool | |
| `gnc` | `{enabled, eciAxes, ecefAxes, graticule, graticuleStepDeg, sidereal, track, trackOrbits, focus: {layer, noradId}}` | |
| `hud` | `{visible, help}` | Globe's built-in HUD (off unless set) |
| `director` | `{type, ...}` | Timeline preset; see [Directors](#directors) |
| `timeline` | `{directed, skipTo, events: [{id, label, at, actions}]}` | See [Timeline](#timeline) |

**Save** (Project tab) writes the current scene: state-stream values plus
what [`globe.js`](src/globe.js) recorded from `add*` calls (inline text,
mission search options, explicit satellite selections, camera lock). `ui`,
`description` and `director` carry over from the loaded project; the timeline
is saved as edited, minus events whose references no longer hold. Not saved:
per-satellite trail toggles, local-file overlay refresh timers, porkchop
inputs.

### Workspace

[`workspace.js`](src/workspace.js) owns the open project.

| | |
| --- | --- |
| Open | Project tab (file, URL, recent, examples) or drop a file on the page. Switches in place: abort old panel → `globe.reset()` → apply → mount. |
| Unsaved changes | Any scene-changing `globe.js` command outside `globe.quietly`, or a timeline edit, marks the project dirty (`•` in title and Project tab). Camera moves, view switches, pins, refreshes, project application and the timeline runner do not. Opening another project asks; leaving the page prompts. |
| Autosave | Dirty scene captured to IndexedDB 3 s after the last change; cleared on save or discard. One left at boot is offered under the Project tab (Recover / Dismiss). |
| Recent | Last 8 opened projects (IndexedDB). Local files keep their text up to 4 MB. |

### Bundled projects

Listed in [`projects/manifest.json`](projects/manifest.json).

| Project | Contents |
| --- | --- |
| [`overview`](projects/overview.terramenta.json) | Geometry tour, extruded cube, USGS past hour (60 s refresh), Celestrak `stations` |
| [`mars-mission`](projects/mars-mission.terramenta.json) | Earth → Mars, flight panel, `missionFlight` director, Earth layers off |
| [`leo-transfer`](projects/leo-transfer.terramenta.json) | 500 → 800 km Hohmann transfer vs. coasting reference; CelesTrak EOP and space weather; timeline slows the clock around each burn |

## Full panel

The panel has seven tabs. All controls stay bound while hidden, so switching
tabs does not resync anything.

| Tab | Section | Controls |
| --- | --- | --- |
| **Project** | Project | Name, description, source, unsaved marker; open file/URL; save; new; recover; recent; bundled projects |
| **Imagery** | Imagery | The eight presets grouped by protocol (WMS/WMTS) with tile size, format and depth; previous/next; streaming on/off |
| **Data** | Vector tiles | Source (MapLibre boundaries, OSM Shortbread), its source layers and depth, colour, ring fill, streaming on/off |
| | GeoJSON overlays | Add from URL or local file; bundled samples and USGS feeds; refresh period; picking on/off. Per layer: visibility, colour, clamp to surface, extrude, simplestyle on/off, refresh, remove, feature counts and age |
| | Satellites | Add OMM from URL or local file; five Celestrak groups. Per layer: visibility, colour, trails and trail window, refresh, remove, and a filterable object list with per-object draw and trail toggles |
| **Spacecraft** | Propagation environment | EOP, space weather, kernel: URL or local file, coverage |
| | Spacecraft | Per spacecraft: visibility, colour, trails, span/samples/final mass, edit, go to start, OEM/SPK export, remove. Editor: initial state (GCRF state with circular-orbit fill, TLE, OMM), start/duration/step, vehicle, forces, integrator, impulsive and finite burns |
| **Timeline** | Timeline | Directed on/off; previous/next event; strip (click to set the clock) and list of every event with time, source, actions, Go, Remove (project events). Add event: label, at clock or an event plus offset, actions from the current scene (rate, paused, view, frame, camera, focus) |
| **Other** | Sun & clock | Run/pause, rate (1× to 86,400×), jump to now or by hours/days, terminator shading |
| | Reference frame | ECEF / ECI |
| | Both frames at once | ECI and ECEF triads, graticule and spacing, sidereal arc, focused satellite's orbit and ground track over a set number of orbits; quaternion readout and per-frame speeds |
| | Camera | Fly to lat/lon/altitude, nine preset places, read back current view, reset |
| | Globe chrome | Built-in HUD, key legend, key bindings |
| **Mission** | Porkchop plot | Origin/destination bodies and departure/arrival date ranges; renders a contoured total-Δv SVG from `terramenta-charta` |
| | Launch | Calls `addMission` for the selected pair. Per mission: status, dates, Δv, remove |

Launched spacecraft are visible in the heliocentric view (`U`; `5` follows the
first mission).

A telemetry panel shows the state snapshot: cursor and camera position,
altitude, frame, sidereal angle and ECI→ECEF quaternion, subsolar point, clock
and rate, active layers, overlay and satellite status, and both tile streamers.
Below it, the picked item: a feature's properties, a satellite's elements, or a
placemark's position.

For placemarks, [`feature.js`](src/feature.js) derives values the globe does
not report: body elevation at the view centre (90° minus the angular distance
to the sub-point), apparent solar time at the view centre (15° per hour from the
subsolar meridian), and lunar elongation and illuminated fraction
(`(1 − cos elongation) / 2`).

### Sample data

- [`data/geometry-tour.geojson`](data/geometry-tour.geojson): one feature per
  GeoJSON geometry type, plus a polygon with a hole, an antimeridian-crossing
  polygon, a launch profile climbing to 420 km, and a stepped terminal control
  area over Denver (five rings at increasing floors up to 12,000 ft). The
  airspace layering is only visible at low altitude with the camera tilted
  (Ctrl+drag).
- [`data/extruded-cube.geojson`](data/extruded-cube.geojson): a square ring at
  222 km over the equator on a layer with `extrude` on, producing a 222 km box.
  Kept separate from the tour because extrusion is per layer.
- [`data/simplestyle-examples.geojson`](data/simplestyle-examples.geojson): one
  labelled example per simplestyle member, including a deliberately invalid one
  that is ignored.

Overlay catalogue ([`feeds.js`](src/feeds.js)): the three samples, USGS past
hour, past day and significant past month (USGS sends
`Access-Control-Allow-Origin: *`). USGS third coordinate is depth; use the
layer's clamp-to-surface toggle to ignore it.

Satellite groups ([`orbits.js`](src/orbits.js)): crewed stations, GPS,
geostationary, Molniya, and a full Starlink shell (exceeds the 600-object
marker cap).

## Spacecraft

[`spacecraft-panel.js`](src/spacecraft-panel.js),
[`environment.js`](src/environment.js). A spacecraft is a `propagate` request
plus ephemeris display options: `globe.addSpacecraft(id, propagation, display)`
propagates (Cowell, synchronous) and puts the result up as ephemeris layer
`id` via `addPropagated`. Spacecraft layers are listed under Spacecraft, not
Satellites.

- **Open**: `workspace.open` applies the scene synchronously, then
  `applySpacecraft` loads `environment` (skipping URLs already loaded) and
  propagates each spacecraft. The globe starts without waiting. A spacecraft
  that fails keeps its spec (saved as is) and shows the error.
- **Editor**: validates the entry as a project would (`parseProject`), then
  propagates; editing re-propagates under the same id, keeping display
  options. Δv in m/s in the editor, km/s in the file. The clock jumps to the
  start if it is outside the span.
- **Environment**: wasm-global, not cleared by `reset`; stays loaded across
  projects. URL sources are saved; local files are not. CelesTrak serves
  `EOP-All.csv`/`SW-All.csv` with CORS; NAIF kernels need a local file or a
  same-origin URL.
- **Export**: OEM (KVN, GCRF/UTC) and SPK Type 13 re-run the propagation.

## Timeline

[`timeline.js`](src/timeline.js), [`timeline-panel.js`](src/timeline-panel.js).
Events on the simulated clock; the workspace runs the timeline on every
snapshot.

| Source | Ids | Actions |
| --- | --- | --- |
| Spacecraft | `<id>.start`, `.end`, `.burnN`, `.finiteN`, `.finiteN.end` | none |
| Missions | `<id>.departure`, `.escape`, `.arrival`, `.capture` (once known) | none |
| Director | `director-*` | generated, not saved |
| Project | `timeline.events[].id` (no dots) | `actions` |

`at`: a date-time, or `{event, offsetS}` naming any event above (chains
allowed, cycles rejected). `skipTo` (same form): once it resolves, a clock
before it jumps forward to it, once per open.

Actions: `timeScale` (number or `"max"`; keeps the clock's direction),
`rampDoublingS` (doubles the rate every N wall-clock seconds up to the
maximum), `paused`, `view`, `frame`, `focus: {layer, noradId?}` (default:
the layer's first object), `follow` (mission id), `camera: {lat, lon,
altitudeKm}`.

Directed (`timeline.directed`, default true): when the latest event at or
before the clock changes — played past, scrubbed, jumped — the baseline
(project `clock.timeScale`/`paused`, `view`, `frame`) and the actions of every
event up to it are merged. A key is applied if its merged value changed since
the last application or the latest event sets it, so a rate set by hand holds
until an event changes the rate, and scrubbing back before every event
restores the baseline. `camera` applies from the latest event only. A view
request repeats until it lands.

## Flight panel

`ui.panel: "flight"` ([`flight-panel.js`](src/flight-panel.js)): one mission
(`director.mission`, else the first), no tabs.

- **Clock**: directed on/off, run/pause, forward/backward, rate slider.
- **Mission**: status line and four phase buttons (departure, escape, arrival,
  capture) that jump the clock to each milestone once known and highlight when
  reached.
- **Look at**: Sun, Earth, Mars, barycentre, or spacecraft
  (`setHeliocentricAnchor` / `followMission`).
- **Project**: as the Project tab.

## Directors

[`director.js`](src/director.js). A director is a timeline preset:
`events(config)` returns `{events, skipTo}` in `timeline` form, run alongside
the project's own events and shown read-only. Fields are typed in the schema
(`$defs/<type>`); a new director needs a `$defs` entry and an `allOf` branch
in `$defs/director`.

`missionFlight` — `{mission, launchLeadSeconds = 3600, departureTimeScale = 600,
departureDoublingSeconds = 2}`:

| At | Actions |
| --- | --- |
| `skipTo`, `<mission>.departure − launchLeadSeconds` | `departureTimeScale`, globe view |
| `<mission>.departure` | `departureTimeScale`, `rampDoublingS: departureDoublingSeconds` |
| `<mission>.escape` | `"max"`, heliocentric view |
| `<mission>.capture` | pause |

## Data flow

Controls are one-way: a control sends a command and does not set its own
value. Each state snapshot sets every control. As a result the panel cannot
drift from the globe, and changes made through the globe's own key bindings
(e.g. `P`) are reflected in the panel.

Exceptions: a slider is not updated while held, and a text field is not
updated while focused.

Other patterns:

- **Picking** ([`feature.js`](src/feature.js)): the globe reports hover only.
  The app treats a press and release without movement as a click and calls
  `pinFeature` / `pinSatellite` / `pinPlacemark`, or clears the pin over empty
  space. Satellites are pinned by NORAD id. Priority when several are hit:
  pinned over hovered; then satellite, placemark, feature.
- **Local overlay files** ([`overlays.js`](src/overlays.js)): the app keeps the
  `File`, re-reads it on the configured period, and re-sends it under the same
  id.
- **Satellite object lists** ([`ephemeris.js`](src/ephemeris.js)): the app
  calls `ephemerisObjects(id)` only when a layer's `revision` changes, and
  renders at most 200 rows behind a filter.
- **Porkchop plots** ([`porkchop.js`](src/porkchop.js),
  [`mission.js`](src/mission.js)): `terramenta-charta` is stateless, so the
  plot is computed on request — once without levels to get the Δv range, then
  with levels across it — and drawn as hand-built SVG.

### Boot order

[`main.js`](src/main.js) loads the module and the project in parallel, opens
the project in the workspace — builds the panel it names from `layers()`,
`vectorLayers()` and `limits()` (available before the renderer starts) and
queues it ([`project.js`](src/project.js) `applyProject`) — then calls `start`
with the project's `view`. Commands sent before `start` are applied on the
first frame. Panels take an `AbortSignal` that fires when the project closes;
anything with a timer or subscription must stop on it.

### Adding a control

1. Add a `GlobeCommand` variant and apply it in
   [`api.rs`](../terramenta-globe/src/api.rs).
2. Bind it in [`wasm.rs`](../terramenta-globe/src/wasm.rs).
3. Re-export it from [`globe.js`](src/globe.js).
4. Build the control in [`panel.js`](src/panel.js) from
   [`widgets.js`](src/widgets.js) and `bind` it to the snapshot field it
   reflects.

Display-only values need only a `GlobeState` field and a line in
[`readout.js`](src/readout.js).

## Layout

```
index.html               The page
assets/terramenta.png    Loading-screen banner
package.json             `npm test` only
schema/
  project.schema.json           Project format (JSON Schema 2020-12)
projects/
  manifest.json                 Bundled project list
  overview.terramenta.json      Layers tour
  mars-mission.terramenta.json  Directed Earth → Mars flight
  leo-transfer.terramenta.json  Propagated Hohmann transfer
data/
  geometry-tour.geojson         One feature per geometry type
  extruded-cube.geojson         Extruded ring
  simplestyle-examples.geojson  simplestyle-spec member examples
styles/app.css           Styles
src/
  main.js                Boot: project → workspace → globe; per-project UI mount
  workspace.js           Open project: in-place switching, dirty tracking, autosave, recent, drop
  store.js               IndexedDB key-value store
  project.js             Project load, upgrade, validate, apply, capture, save
  environment.js         Propagation environment: EOP, space weather, kernel
  schema.js              JSON Schema subset validator
  project-panel.js       Project section (open, save, recover, recent, examples)
  director.js            Timeline presets (missionFlight)
  timeline.js            Timeline: scene/project/director events, resolution, runner
  flight-panel.js        Flight panel
  globe.js               Sole wrapper around the globe WASM module; records add* sources, reports scene changes
  porkchop.js            Sole wrapper around the charta WASM module
  panel.js               Control panel tabs and the one-way bind rule
  overlays.js            GeoJSON layer controls, local-file refresh
  ephemeris.js           Satellite layer controls, object list
  mission.js             Mission tab: porkchop plot and launch
  spacecraft-panel.js    Spacecraft tab: environment, spacecraft list, editor, OEM/SPK export
  timeline-panel.js      Timeline tab: strip, event list, add event
  feature.js             Picking, click handling, placemark calculations
  readout.js             Telemetry panel
  widgets.js             Buttons, toggles, choices, sliders
  format.js              Coordinate, altitude, rate, duration and UTC time formatting
  dom.js                 Element helpers and tab strip
  places.js              Camera presets
  feeds.js               Overlay catalogue
  orbits.js              Satellite catalogue
scripts/
  build.sh               Builds globe/ and charta/
  serve.sh               Builds if needed, then serves
globe/                   Build output (git-ignored)
charta/                  Build output (git-ignored)
```
