# terramenta-webapp

Reference client for [`terramenta-globe`](../terramenta-globe/) and
[`terramenta-charta`](../terramenta-charta/). Plain HTML, CSS and ES modules:
no npm, no bundler. One page, `index.html`, which loads a project file.

| URL | Loads |
| --- | --- |
| `index.html` | Empty project |
| `index.html?project=projects/overview.terramenta.json` | Bundled project (any same-origin or CORS URL) |
| `index.html?project=session` | Local file opened from the Project tab (`sessionStorage`) |

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

## Project files

`*.terramenta.json`. Every project, bundled or user-supplied, loads the same
way: page load → fetch → validate → queue onto the globe → `start`. Opening
another project reloads the page, so each starts on a fresh globe. Errors list
every failing path; unknown keys warn. Omitted fields keep globe defaults.

Relative URLs resolve against the app root (the directory holding
`index.html`), not the project file.

| Key | Type | Applies |
| --- | --- | --- |
| `terramenta` | `1` | Format version. Required. |
| `name`, `description` | string | Masthead, Project tab, page title |
| `ui.panel` | `"full"` \| `"flight"` | Full tabbed panel (default) or [flight panel](#flight-panel) |
| `ui.tab` | `"project"` \| `"imagery"` \| `"data"` \| `"other"` \| `"mission"` | Initial tab (full panel) |
| `ui.readout`, `ui.collapsed` | bool | Telemetry + picked-item panels (default on); panel collapsed |
| `view` | `"globe"` \| `"heliocentric"` | Initial view, no transition |
| `frame` | `"ecef"` \| `"eci"` | `setFrame` |
| `clock` | `{epoch, timeScale, paused, shaded}` | `epoch`: ISO 8601 UTC or `"now"` |
| `camera` | `{lat, lon, altitudeKm, follow \| anchor}` | `lookAt`; `follow`: mission id; `anchor`: `sun`/`earth`/`mars`/`barycenter` |
| `imagery`, `vectorTiles` | `{enabled, layer, style?}` | `layer`: preset label or index |
| `overlays` | `{enabled, layers: [{id, ...addOverlay options}]}` | One of `url`/`text` per layer |
| `ephemerides` | `{enabled, layers: [{id, ...addEphemeris options}]}` | OMM JSON or KVN OEM |
| `missions` | `[{id, ...addMission options}]` | |
| `placemarks.enabled`, `picking`, `keyboard` | bool | |
| `gnc` | `{enabled, eciAxes, ecefAxes, graticule, graticuleStepDeg, sidereal, track, trackOrbits, focus: {layer, noradId}}` | |
| `hud` | `{visible, help}` | Globe's built-in HUD (off unless set) |
| `director` | `{type, ...}` | Scripted clock/view control; see [`director.js`](src/director.js) |

**Save** (Project tab) writes the current scene: state-stream values plus
what [`globe.js`](src/globe.js) recorded from `add*` calls (inline text,
mission search options, explicit satellite selections, camera lock). `ui`,
`description` and `director` carry over from the loaded project. Not saved:
per-satellite trail toggles, local-file overlay refresh timers, porkchop
inputs.

### Bundled projects

Listed in [`projects/manifest.json`](projects/manifest.json).

| Project | Contents |
| --- | --- |
| [`overview`](projects/overview.terramenta.json) | Geometry tour, extruded cube, USGS past hour (60 s refresh), Celestrak `stations` |
| [`mars-mission`](projects/mars-mission.terramenta.json) | Earth → Mars, flight panel, `missionFlight` director, Earth layers off |

## Full panel

The panel has five tabs. All controls stay bound while hidden, so switching
tabs does not resync anything.

| Tab | Section | Controls |
| --- | --- | --- |
| **Project** | Project | Name, description, source; open file/URL; save; new; bundled projects |
| **Imagery** | Imagery | The eight presets grouped by protocol (WMS/WMTS) with tile size, format and depth; previous/next; streaming on/off |
| **Data** | Vector tiles | Source (MapLibre boundaries, OSM Shortbread), its source layers and depth, colour, ring fill, streaming on/off |
| | GeoJSON overlays | Add from URL or local file; bundled samples and USGS feeds; refresh period; picking on/off. Per layer: visibility, colour, clamp to surface, extrude, simplestyle on/off, refresh, remove, feature counts and age |
| | Satellites | Add OMM from URL or local file; five Celestrak groups. Per layer: visibility, colour, trails and trail window, refresh, remove, and a filterable object list with per-object draw and trail toggles |
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

## Flight panel

`ui.panel: "flight"` ([`flight-panel.js`](src/flight-panel.js)): one mission
(`director.mission`, else the first), no tabs.

- **Clock**: run/pause, forward/backward, rate slider.
- **Mission**: status line and four phase buttons (departure, escape, arrival,
  capture) that jump the clock to each milestone once known and highlight when
  reached.
- **Look at**: Sun, Earth, Mars, barycentre, or spacecraft
  (`setHeliocentricAnchor` / `followMission`).
- **Project**: as the Project tab.

## Directors

[`director.js`](src/director.js). A director is a state-stream callback that
drives clock and view.

`missionFlight` — `{mission, launchLeadSeconds = 3600, departureTimeScale = 600,
departureDoublingSeconds = 2}`:

1. Mission `waiting`: clock to `launchLeadSeconds` before departure at
   `departureTimeScale`.
2. Orbiting origin: rate doubles every `departureDoublingSeconds` (wall clock)
   up to the maximum.
3. Heliocentric cruise: maximum rate, heliocentric view.
4. Orbiting destination: pause.

View: globe around the origin, heliocentric elsewhere. Rate changes keep the
sign of the current time scale, so reverse playback works.

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

[`main.js`](src/main.js) loads the module and the project in parallel, builds
the panel the project names from `layers()`, `vectorLayers()` and `limits()`
(available before the renderer starts), queues the project
([`project.js`](src/project.js) `applyProject`), then calls `start` with the
project's `view`. Commands sent before `start` are applied on the first frame.

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
projects/
  manifest.json                 Bundled project list
  overview.terramenta.json      Layers tour
  mars-mission.terramenta.json  Directed Earth → Mars flight
data/
  geometry-tour.geojson         One feature per geometry type
  extruded-cube.geojson         Extruded ring
  simplestyle-examples.geojson  simplestyle-spec member examples
styles/app.css           Styles
src/
  main.js                Boot: project → panel → globe
  project.js             Project load, validate, apply, capture, save
  project-panel.js       Project section (open, save, bundled list)
  director.js            Scripted clock/view control (missionFlight)
  flight-panel.js        Flight panel
  globe.js               Sole wrapper around the globe WASM module; records add* sources
  porkchop.js            Sole wrapper around the charta WASM module
  panel.js               Control panel tabs and the one-way bind rule
  overlays.js            GeoJSON layer controls, local-file refresh
  ephemeris.js           Satellite layer controls, object list
  mission.js             Mission tab: porkchop plot and launch
  feature.js             Picking, click handling, placemark calculations
  readout.js             Telemetry panel
  widgets.js             Buttons, toggles, choices, sliders
  format.js              Coordinate, altitude, rate and duration formatting
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
