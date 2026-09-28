# terramenta-webapp

Reference client for [`terramenta-globe`](../terramenta-globe/) and
[`terramenta-charta`](../terramenta-charta/). Plain HTML, CSS and ES modules:
no npm, no bundler. Two pages:

- **`index.html`** — the full control panel, exercising the globe's API.
- **`mission-demo.html`** — a single scripted Earth → Mars mission with only
  mission-related controls (the recording in the [root README](../README.md)).

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

## `index.html`: control panel

The panel has four tabs. All controls stay bound while hidden, so switching
tabs does not resync anything.

| Tab | Section | Controls |
| --- | --- | --- |
| **Imagery** | Imagery | The eight presets grouped by protocol (WMS/WMTS) with tile size, format and depth; previous/next; streaming on/off |
| **Data layers** | Vector tiles | Source (MapLibre boundaries, OSM Shortbread), its source layers and depth, colour, ring fill, streaming on/off |
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

### Startup layers

- [`data/geometry-tour.geojson`](data/geometry-tour.geojson): one feature per
  GeoJSON geometry type, plus a polygon with a hole, an antimeridian-crossing
  polygon, a launch profile climbing to 420 km, and a stepped terminal control
  area over Denver (five rings at increasing floors up to 12,000 ft). The
  airspace layering is only visible at low altitude with the camera tilted
  (Ctrl+drag).
- [`data/extruded-cube.geojson`](data/extruded-cube.geojson): a square ring at
  222 km over the equator on a layer with `extrude` on, producing a 222 km box.
  Kept separate from the tour because extrusion is per layer.
- USGS earthquakes, past hour, refreshed every 60 s. The feed's third
  coordinate is depth; use the layer's clamp-to-surface toggle to ignore it.
- Celestrak `stations` group.

Also available from the overlay catalogue
([`feeds.js`](src/feeds.js)):
[`data/simplestyle-examples.geojson`](data/simplestyle-examples.geojson) (one
labelled example per simplestyle member, including a deliberately invalid one
that is ignored), USGS past day, and USGS significant past month. The USGS
feeds are used because they send `Access-Control-Allow-Origin: *`.

Satellite groups ([`orbits.js`](src/orbits.js)): crewed stations, GPS,
geostationary, Molniya, and a full Starlink shell (exceeds the 600-object
marker cap).

## `mission-demo.html`: mission demo

[`mission-demo.js`](src/mission-demo.js) disables imagery, vector tiles,
overlays, satellites, placemarks and the HUD, adds one Earth → Mars mission,
and calls `followMission` before `start`. On each state update it directs the
flight:

1. When the mission reaches `waiting`, sets the clock to one hour before
   departure at 600×.
2. While orbiting Earth, doubles the rate every 2 s up to the maximum.
3. On entering heliocentric cruise, sets the maximum rate and switches to the
   heliocentric view.
4. On Mars capture, pauses the clock.

The view follows the current primary: globe view around Earth, heliocentric
elsewhere. All rate changes preserve the sign of the current time scale, so
the demo also runs backward.

[`mission-demo-panel.js`](src/mission-demo-panel.js) provides:

- **Clock**: run/pause, forward/backward, rate slider.
- **Mission**: status line and four phase buttons (departure, escape, arrival,
  capture) that jump the clock to each milestone once known and highlight when
  reached.
- **Look at**: Sun, Earth, Mars, barycentre, or spacecraft
  (`setHeliocentricAnchor` / `followMission`).

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

[`main.js`](src/main.js) loads the module, builds the panel from `layers()`,
`vectorLayers()` and `limits()` (available before the renderer starts), queues
startup commands, then calls `start`. Commands sent before `start` are applied
on the first frame.

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
index.html               Control panel page
mission-demo.html        Mission demo page
assets/terramenta.png    Loading-screen banner
data/
  geometry-tour.geojson         One feature per geometry type, loaded at boot
  extruded-cube.geojson         Extruded ring, loaded at boot
  simplestyle-examples.geojson  simplestyle-spec member examples
styles/app.css           Styles for both pages
src/
  main.js                Boot for index.html
  mission-demo.js        Boot and flight direction for mission-demo.html
  mission-demo-panel.js  Mission demo controls
  globe.js               Sole wrapper around the globe WASM module
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
