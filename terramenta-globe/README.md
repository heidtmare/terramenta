# terramenta-globe

A navigable 3D Earth globe and heliocentric solar-system scene on
[Bevy](https://bevy.org) 0.19. The same code runs in the browser over WebGPU
and on the desktop over Metal, Vulkan or DX12.

The crate is a component: every feature is reachable through a command/state
API ([The control surface](#the-control-surface)).
[`terramenta-webapp`](../terramenta-webapp/) is the reference embedder. The
native binary is the same globe with its built-in HUD and key bindings.

## Building

```sh
./scripts/fetch-assets.sh                     # once: NASA base textures
cargo run --release --bin terramenta-globe    # native
./scripts/build-wasm.sh --release             # WebAssembly, into ./dist
```

`build-wasm.sh` compiles for `wasm32-unknown-unknown`, runs `wasm-bindgen`, and
writes the JS/WASM module plus an `assets/` folder to `--out-dir` (default
`./dist`). `--release` uses the workspace's size-optimized `web-release`
profile. The installed `wasm-bindgen` CLI must match the crate version exactly;
the script checks and prints the install command on mismatch.

## The control surface

[`src/api.rs`](src/api.rs) defines it; [`src/wasm.rs`](src/wasm.rs) binds it to
JavaScript.

- **Commands** are queued from any caller and drained inside the Bevy schedule,
  the only place that mutates the `World`. Commands sent before `start` are
  held and applied on the first frame, so an interface can be built while the
  module is still initializing.
- **State** is a `GlobeState` snapshot built from the same resources the HUD
  reads, pushed to one listener at ~10 Hz and immediately after any
  controllable value changes.

The built-in key bindings issue the same commands, so an external interface
driven by the snapshot stays in sync with keyboard input.

### From JavaScript

```js
import init, * as globe from "./globe/terramenta_globe.js";

await init();

// Static catalogue, available before start().
globe.layers();        // [{index, label, protocol, maxLevel, tileSize, format}, ...]
globe.vectorLayers();  // [{index, label, maxLevel, sourceLayers}, ...]
globe.limits();        // {minAltitudeKm, maxAltitudeKm, minTimeScale, maxTimeScale,
                       //  maxVectorTileLatitude}

globe.onState((state) => {
  // {camera, frame, gnc, sun, imagery, vectorTiles, overlays, ephemerides,
  //  missions, view, placemarks, hud, cursor, keyboard} — see `GlobeState`
});

globe.addOverlay("quakes", {
  url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson",
  refreshSeconds: 60,
});
globe.addEphemeris("stations", {
  url: "https://celestrak.org/NORAD/elements/gp.php?GROUP=stations&FORMAT=json",
});
globe.setHudVisible(false);        // queued until start
globe.start("#terramenta", "globe/assets");
```

`start(canvasSelector, assetPath)` resolves `assetPath` against the page URL,
not the module URL. It throws once the globe is running, because winit hands
control to the browser event loop by unwinding;
[`globe.js`](../terramenta-webapp/src/globe.js) shows how to distinguish that
from a real failure.

| Function | Purpose |
| --- | --- |
| `layers()` `vectorLayers()` `limits()` | Imagery and vector tile presets; control ranges |
| `lookAt(lat, lon, altitudeKm?)` | Point the camera straight down at a coordinate |
| `setAltitude(km)` `zoomBy(exp)` `orbitBy(yawDeg, pitchDeg)` `resetView()` | Camera movement |
| `setFrame("ecef" \| "eci")` `toggleFrame()` | Reference frame of the globe scene |
| `setView("globe" \| "heliocentric")` `toggleView()` | Globe or heliocentric scene |
| `setHeliocentricAnchor("sun" \| "earth" \| "mars" \| "barycenter")` | Heliocentric camera target; releases a mission lock |
| `followMission(id)` | Lock both cameras onto a mission's spacecraft |
| `setGncEnabled(bool)` `toggleGnc()` | Draw both frames at once |
| `setGncEciAxes(bool)` `setGncEcefAxes(bool)` `setGncSidereal(bool)` | Triads and sidereal-angle arc |
| `setGncGraticule(bool)` `setGncGraticuleStep(deg)` | Lat/lon graticule |
| `setGncTrack(bool)` `setGncTrackOrbits(n)` `setGncFocus(layer, noradId)` | One satellite's orbit and ground track together |
| `setSunPaused(bool)` `setTimeScale(n)` `setClock(unixSeconds)` `snapClockToNow()` | Simulated clock; negative `n` runs backward |
| `setSunShaded(bool)` | Terminator or uniform daylight |
| `setLayer(i)` `nextLayer()` `previousLayer()` `setImageryEnabled(bool)` | Streamed imagery |
| `setVectorTileLayer(i)` `nextVectorTileLayer()` `previousVectorTileLayer()` `setVectorTilesEnabled(bool)` | Streamed vector tiles |
| `setVectorTileStyle(style)` | Vector tile colours and widths |
| `addOverlay(id, options)` `removeOverlay(id)` | GeoJSON overlays |
| `setOverlayVisible(id, bool)` `setOverlayStyle(id, style)` `setOverlaysEnabled(bool)` | Overlay drawing |
| `setOverlaySimpleStyle(id, bool)` | Honour or ignore per-feature simplestyle members |
| `setOverlayAltitude(id, altitude)` | Altitude mode, scale, extrusion |
| `setOverlayRefresh(id, seconds)` `refreshOverlay(id)` | Overlay refetch |
| `overlayGeometry(id)` | Overlay coordinates as zero-copy typed arrays |
| `addEphemeris(id, options)` `removeEphemeris(id)` | Satellite layers from OMM JSON |
| `setEphemerisVisible(id, bool)` `setEphemerisStyle(id, style)` `setEphemeridesEnabled(bool)` | Satellite drawing |
| `setEphemerisSelection(id, noradIds)` `selectSatellite(id, noradId, bool)` | Which objects are drawn |
| `setEphemerisTrails(id, bool)` `setSatelliteTrail(id, noradId, bool)` `setEphemerisTrail(id, window)` | Orbit trails |
| `setEphemerisRefresh(id, seconds)` `refreshEphemeris(id)` | Catalogue refetch |
| `ephemerisObjects(id)` `ephemerisGeometry(id)` | Object list; current positions and arcs |
| `addMission(id, options)` `removeMission(id)` | Flown interplanetary missions |
| `setPickingEnabled(bool)` `pinFeature(layer, index)` `clearPinnedFeature()` | Overlay feature picking |
| `pinSatellite(layer, noradId)` `clearPinnedSatellite()` | Satellite picking |
| `pinPlacemark("sun" \| "moon")` `clearPinnedPlacemark()` `setPlacemarksEnabled(bool)` | Subsolar/sublunar placemarks |
| `setHudVisible(bool)` `setHelpVisible(bool)` `setKeyboardEnabled(bool)` | Built-in HUD and key bindings |
| `onState(callback)` | State stream; one listener, re-registering replaces it |

### From Rust

Native embedders enqueue through `api::send(GlobeCommand)` and read the
`LatestState` resource. `app(GlobeConfig { .. })` builds the `App` without
running it, so a host can add plugins or pass its own imagery, vector tile,
overlay and mission presets first.

## Earth scene

- **Surface.** Equirectangular Blue Marble textures on a custom UV sphere
  ([`geo::equirectangular_sphere`](src/geo.rs)), shaded in
  [`globe.wgsl`](assets/shaders/globe.wgsl): terminator, city lights on the
  night side, ocean specular, animated clouds, limb haze. There is no Bevy
  `DirectionalLight`; one sun-direction uniform drives all lighting, keeping
  the planet to one draw call.
- **Atmosphere.** Additive back-face shell
  ([`atmosphere.wgsl`](assets/shaders/atmosphere.wgsl)) with density rising
  toward the limb and a twilight arc past the terminator.
- **Sky.** Procedural stars from a hashed 3D cell grid
  ([`starfield.wgsl`](assets/shaders/starfield.wgsl)); no texture, no pole
  distortion. Shared with the heliocentric view.
- **Clock.** [`time.rs`](src/time.rs): starts at wall-clock UTC, default 360×,
  magnitude clamped to 1×–86,400×, sign preserved for backward time.
- **Sun and Moon.** [`sun.rs`](src/sun.rs): low-precision solar model
  (declination + hour angle, ~1°). [`moon.rs`](src/moon.rs): low-precision
  lunar series (mean elements plus principal perturbations, a few arcminutes).
- **Camera.** [`camera.rs`](src/camera.rs): drag sensitivity scales with
  altitude. Drag orbits the surface; Ctrl+drag changes heading and tilt about
  the look-at point; Shift+drag rotates the view in place. Mouse, keyboard,
  trackpad gestures and multi-touch.

## Reference frames

[`frame::ReferenceFrame`](src/frame.rs) selects which frame world space is:

- **ECEF**: identity. The globe is fixed; the sun moves.
- **ECI**: world space is inertial. The globe rotates at the sidereal rate.

Earth-fixed geometry (globe mesh, imagery tiles, overlays) is rotated into world
space by `earth_to_world`; anything read back from the scene (cursor coordinate,
tile quadtree walk) uses `world_to_earth`. The sun direction passed to shaders
is always in world space.

### Both frames at once (GNC overlay)

[`gnc.rs`](src/gnc.rs) (`G`) draws over the active frame:

- inertial triad (cyan): vernal equinox, +90° RA axis, celestial equator;
- Earth-fixed triad (amber): equator, prime meridian, graticule;
- the sidereal angle between them as an arc;
- one satellite's ECI orbit ellipse and ECEF ground track from a single
  propagation.

Rigid geometry (triads, graticule) is built once in its own frame and placed by
a quaternion on its `Transform`; switching frames only swaps which quaternion
is identity. Ground tracks cannot be rigid, since each sample belongs to a
different epoch, so each sample is rotated by the sidereal angle of its own
timestamp when the mesh is built. No precession, nutation or polar motion is
modelled, so the pole is shared.

`gnc.rs` carries the rotation as both a DCM `R3(θ)` and a unit quaternion, with
tests pinning them to each other, to the renderer's Earth rotation, and to the
axis relabelling between the GNC basis (Z = pole) and Bevy's Y-up scene.
Velocity includes the transport term:

```text
v_ecef = R3(θ) · ( v_eci − ω × r_eci )
```

The HUD shows both speeds for the focused object (e.g. ISS ≈ 7.66 km/s
inertial vs 7.36 km/s ground-relative; GEO ≈ 3.07 vs 0).

## Streaming imagery

WMS and WMTS imagery draped on the globe. [`tiles.rs`](src/tiles.rs) walks a
quadtree from the camera each frame, splitting a tile when its projected size
exceeds its image resolution. Tiles over the horizon or off the world are
skipped; missing tiles fall back to the nearest loaded ancestor, then to the
base texture. Requests are capped in flight and per frame.

[`imagery.rs`](src/imagery.rs) registers an `imagery://` asset source whose
reader maps a tile path (`0/4/9/3.jpg`) to the active layer's URL — a `GetMap`
query ([`wms.rs`](src/wms.rs)) or a REST/KVP `GetTile` request
([`wmts.rs`](src/wmts.rs)) — and delegates to Bevy's HTTP reader. Decoding,
upload and reference counting use the standard `asset_server.load` path on both
targets.

### Grids

- **WMS** requests are pinned to a global geodetic quadtree: level `n` is
  `2^(n+1) × 2^n` tiles, level 0 being two 180°×180° tiles.
- **WMTS** uses the server's tile matrix set. NASA GIBS `EPSG:4326` sets start
  from a 288° level-0 tile, so coarse levels extend past the world edge and
  matrices are non-power-of-two (2×1, 3×2, 5×3, 10×5, …).

[`TileGrid`](src/tiles.rs) describes any plate-carrée pyramid by north-west
origin and level-0 tile span. `TileGrid::clipped_bounds` trims tiles to the
world; clipped tiles are meshed over their real extent with texture coordinates
against their full extent, using fewer quads rather than smaller ones to avoid
seams.

### Adding a layer

Presets live in `imagery_layers()` in [`lib.rs`](src/lib.rs) (four GIBS WMTS,
four GIBS WMS). A WMTS layer can be transcribed from `WMTSCapabilities.xml`:

```rust
WmtsConfig {
    label: "My layer".into(),
    layer: "workspace:layer".into(),
    style: "default".into(),
    tile_matrix_set: "EPSG:4326".into(),
    grid: TileGrid::from_scale_denominator(LatLon::new(90.0, -180.0), 279_541_132.0, 256),
    encoding: WmtsEncoding::Rest {
        template: "https://example.org/wmts/{Layer}/{Style}/{TileMatrixSet}/\
                   {TileMatrix}/{TileRow}/{TileCol}.png".into(),
    },
    format: ImageFormat::Png,
    tile_size: 256,
    max_level: 9,
    ..
}
.with_dimension("Time", "2026-09-10")
```

`TileGrid::from_scale_denominator` applies the spec's 0.28 mm pixel conversion.
Use `.with_kvp(endpoint)` for servers without a REST template. Presets are
published through `layers()`, so no JavaScript change is needed.

Notes:

- `TileRow` counts south and `TileCol` east; a `{TileRow}/{TileCol}` template
  takes `y` before `x`.
- `max_level` is a refinement limit for WMS and the matrix-set depth for WMTS
  (exceeding it is a server error).
- `WmsVersion` encodes both 1.3.0 differences: `CRS` instead of `SRS`, and
  latitude-first `EPSG:4326` axis order.
- The browser build requires `Access-Control-Allow-Origin` from the tile host.

## Streaming vector tiles

[Mapbox Vector Tiles](https://github.com/mapbox/vector-tile-spec) from any
`{z}/{x}/{y}` service, drawn as pixel-width lines, rings and markers.

```js
globe.vectorLayers();
globe.setVectorTileLayer(1);       // OpenStreetMap Shortbread
globe.setVectorTileStyle({ lineColor: "#8fd6ff", lineWidthPx: 1.2 });
globe.setVectorTilesEnabled(false);
```

Presets in `vector_tile_layers()` ([`lib.rs`](src/lib.rs)) are the two keyless
sources: MapLibre demo boundaries and OpenStreetMap Shortbread. Custom layers:

```rust
VectorTileLayer::new("My basemap", "https://example.org/tiles/{z}/{x}/{y}.mvt")
    .with_max_level(12)
    .with_source_layers(["water", "boundary"])
```

Pipeline:

1. [`vector_tiles.rs`](src/vector_tiles.rs) walks its own Web Mercator quadtree
   (`2^n × 2^n` per level; `TileId` is shared with imagery) and fetches through
   an `mvt://` asset source.
2. [`mvt.rs`](src/mvt.rs) decodes with `geozero` in Bevy's asset pipeline (task
   thread natively, microtask in the browser). Only the requested source layers
   are decoded.
3. Geometry is clipped to the tile square in tile coordinates to remove buffer
   overlap. Rings are clipped twice: closed against the tile edge for fills,
   and as open paths for outlines, so tile edges are not drawn as boundaries.
4. `mvt::unproject` converts tile coordinates to WGS 84 lat/lon using the
   spherical-Mercator inverse, and positions are drawn on the same sphere as
   everything else. Coverage stops at ±85.05°
   (`limits().maxVectorTileLatitude`).
5. Output uses the overlay mesh builders and
   [`vector.wgsl`](assets/shaders/vector.wgsl). Meshing runs on the schedule and
   is limited to two tiles per frame.

Fills are off by default; a fully transparent fill skips triangulation. Vector
tiles draw above imagery and below overlays. Each feature carries a
`sourceLayer` property. Vector tile features are not pickable.

The web build accepts gzip-encoded tiles (the browser decompresses). The native
build reports a gzip-encoded response as an error.

## GeoJSON overlays

Markers, lines and filled rings from [RFC 7946](https://datatracker.ietf.org/doc/html/rfc7946)
documents. Multiple layers, each with its own style, visibility and refresh
period; the state stream reports per-layer status, counts and staleness.

```js
globe.addOverlay("quakes", {
  url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson",
  label: "Earthquakes, past hour",
  refreshSeconds: 60,
  pointColor: "#ff9e3d",
  pointSizePx: 9,
  lineColor: "#ff9e3d",
  lineWidthPx: 2,
  fillColor: "#ff9e3d3a",
});

// Local file: the page reads it and passes the text.
globe.addOverlay("local", { text: await file.text() });
```

Adding under an existing id replaces that layer.

### Sources and refresh

- **URL** sources load through a `geojson://` asset source
  ([`overlays.rs`](src/overlays.rs), [`fetch.rs`](src/fetch.rs)) and can
  refresh. `refreshSeconds` sets the period, `setOverlayRefresh(id, null)`
  stops it, `refreshOverlay(id)` refetches immediately.
- **Text** sources cannot be refetched by the globe; the embedder re-sends text
  under the same id ([`overlays.js`](../terramenta-webapp/src/overlays.js) does
  this on a timer).

Old geometry stays drawn until the new document arrives; a failed fetch sets
`status: "failed"` with a reason and keeps the previous geometry. Cache busting
uses a generation in the asset path and, from the second fetch onward, a
`_terramenta=<n>` query parameter; layers that never refresh are requested with
their original URL.

### Altitude and extrusion

The optional third coordinate is drawn as height: markers are offset from the
surface and lines interpolate height between vertices.

```js
globe.addOverlay("flight", { url, altitudeScale: 1000 });            // km instead of m
globe.addOverlay("quakes", { url, altitudeMode: "clampToSurface" }); // USGS z is depth
globe.addOverlay("cube",   { url, extrude: true });                  // wall rings to ground
globe.setOverlayAltitude("quakes", { altitudeMode: "relativeToSurface" }); // rebuild in place
```

- `altitudeScale` is metres per unit; negative values read downward-positive
  feeds. Heights at or below zero draw on the surface.
- Height is measured from the overlay drape radius, which clears the deepest
  imagery tile.
- A fill is drawn at one height (mean of its outer ring); outlines follow every
  vertex. Stacked volumes are built from multiple flat rings (e.g. a
  `MultiPolygon` of shelves).
- `extrude` is per layer. It walls each ring edge (holes included) to the
  surface in the fill colour, densified to follow curvature. Lines are not
  extruded.
- Picking uses ground position, not drawn height, so tilted views offset the
  hit area from the drawn shape.

### Drawing

- Markers and lines are sized in pixels; [`vector.wgsl`](assets/shaders/vector.wgsl)
  expands anchors and spines per pixel footprint, so zoom never triggers a
  rebuild.
- Rings are ear-clipped in [`tessellate.rs`](src/tessellate.rs) with holes
  bridged into the outer ring. Antimeridian crossings are handled by letting
  longitudes run past ±180. Rings enclosing a pole are not filled correctly
  (outlines are).
- Polygons over 8,000 vertices are outlined but not filled; holes are dropped
  past 2,000 vertices.
- Overlays are unlit and drawn above imagery and vector tiles.

### simplestyle-spec

[`simplestyle.rs`](src/simplestyle.rs) reads
[simplestyle-spec 1.1.0](https://github.com/mapbox/simplestyle-spec/tree/master/1.1.0)
members from feature `properties`: `marker-size`, `marker-color`, `stroke`,
`stroke-opacity`, `stroke-width`, `fill`, `fill-opacity`, plus `title`,
`description` and `marker-symbol`.

- Each member overrides only the attribute it names; spec defaults are not
  applied. Features without members use the layer style.
- Per-feature colour and size are stored in vertices; unstyled vertices carry a
  sentinel that selects the material uniform. `setOverlayStyle` therefore stays
  a uniform write.
- `title`, `description` and `marker-symbol` are parsed and returned with the
  picked feature under `style`; they are not rendered.
- Unparseable members are dropped individually.
- `setOverlaySimpleStyle(id, false)` (or `simpleStyle: false` at creation)
  ignores all members and rebuilds the layer. State reports `styledFeatures`.

### Picking

```js
globe.onState(({ overlays }) => {
  overlays.hovered;  // {layer, label, index, id, kind, properties, style} or null
  overlays.pinned;
});
globe.pinFeature(hovered.layer, hovered.index);
globe.clearPinnedFeature();
```

[`picking.rs`](src/picking.rs) hit-tests every frame the cursor is over the
globe:

- Results are per feature, so any part of a multi-geometry selects the whole
  feature ([`geojson.rs`](src/geojson.rs) keeps the owning feature per shape).
- Priority is marker > line > fill, then distance, across all layers.
- Tolerance is in pixels (per-feature for simplestyle-sized markers), converted
  to degrees at the cursor with longitude scaled by `cos(lat)`.
- Line segments are tested in lat/lon interpolation, matching how they are
  drawn.
- `properties` is returned as authored; nothing in `properties` affects styling
  except simplestyle members.
- The highlight redraws the feature larger, near-white and behind itself; it is
  rebuilt only when the pick changes and cleared when the layer refreshes.

Click semantics belong to the embedder: the globe reports hover, and the
embedder calls `pinFeature`.

## Satellites

Ephemeris layers load [OMM](https://public.ccsds.org/Pubs/502x0b3e1.pdf) JSON.
[`omm.rs`](src/omm.rs) builds an SGP4 propagator per record;
[`ephemeris.rs`](src/ephemeris.rs) propagates, draws and picks. Layers share
[`fetch.rs`](src/fetch.rs), the GeoArrow store and mesh builders with overlays.

```js
globe.addEphemeris("stations", {
  url: "https://celestrak.org/NORAD/elements/gp.php?GROUP=stations&FORMAT=json",
  label: "Crewed stations",
});
globe.ephemerisObjects("stations");
// [{noradId: 25544, name: "ISS (ZARYA)", periodMinutes: 92.9, selected: true, ...}, ...]
globe.selectSatellite("stations", 25544, true);
globe.setSatelliteTrail("stations", 25544, true);
globe.pinSatellite("stations", 25544);
```

- **Propagation** runs every frame against the simulated clock
  (`sun.unixSeconds`), so pausing, rate changes and clock jumps apply.
- **Budgets.** `MAX_TRACKED` caps markers at 600 per layer. Trails share a
  fixed sample budget across all trailed objects (more objects → coarser arcs,
  constant cost). Arcs rebuild when simulated time has moved a set fraction of
  the arc window, between 2 and 10 times per second. State reports `objects`,
  `tracked` and `trailed`.
- **Frames.** SGP4 outputs TEME. Each arc sample is rotated into the active
  frame at its own epoch, so ECI arcs are closed ellipses and ECEF arcs are
  ground tracks. Meshes are built in world space; switching frames rebuilds
  them. Trail mode is ground track or orbit path per object.
- **Coordinates** are geocentric (declination, RA, radius → height above a
  mean-radius sphere), matching the drawn sphere rather than WGS 84 (differs by
  up to ~0.2° latitude).
- **Object list** is pulled, not streamed: the snapshot carries a per-layer
  `revision`; call `ephemerisObjects(id)` when it changes.
  `ephemerisGeometry(id)` returns zero-copy views of drawn points and arcs,
  rebuilt every frame — read synchronously and `.slice()` to keep.
- **Picking** is screen-space distance to the marker, since markers are drawn
  at altitude. Pins are by NORAD id and survive refetch. Picks report orbital
  elements, epoch age and position.

Operational notes:

- SGP4 accuracy degrades with element age (~1 km/day in LEO); layers report
  `oldestElementsDays`.
- Objects whose propagation fails are omitted; arcs containing a failed sample
  are dropped whole.
- Unparseable records are dropped and counted as `rejected`.
- Refresh period is floored at five minutes.
- Celestrak serves `Access-Control-Allow-Origin: *`; Space-Track requires a
  proxy.

## Placemarks

[`placemark.rs`](src/placemark.rs) draws the subsolar and sublunar points as
pixel-sized icons ([`icon.wgsl`](assets/shaders/icon.wgsl)), positioned from
[`sun.rs`](src/sun.rs) and [`moon.rs`](src/moon.rs).

- Icons ignore the depth buffer and are hidden by an explicit horizon test, so
  they are never partially clipped by the globe.
- Picking projects the anchor to the viewport and tests the icon rectangle.
- `setPlacemarksEnabled(false)` hides and disables picking; existing pins
  persist.

```js
globe.onState(({ placemarks }) => {
  placemarks.hovered;  // {body: "sun" | "moon", label, coordinate} or null
  placemarks.pinned;
});
globe.pinPlacemark("moon");
```

## Heliocentric view

[`view.rs`](src/view.rs) switches the scene between the Earth-centred globe and
a heliocentric scene around the Solar System Barycentre (`U`, or
`setView`/`toggleView`). The switch changes floating origin, unit scale, camera
rig and projection planes on the same tick. The globe → heliocentric leg pulls
the camera back to 60 Earth radii over 2 s against a stripped-down scene, then
fades; the return leg takes 1 s. `state.view` is `"globe"`, `"heliocentric"` or
`"transitioning"`; requests mid-transition are ignored.

[`heliocentric.rs`](src/heliocentric.rs):

- Sun, Earth and Mars as lit spheres with art-directed radii (true scale is
  sub-pixel at AU distances). The Sun uses procedural granulation and limb
  darkening ([`sun.wgsl`](assets/shaders/sun.wgsl)) and an additive corona
  shell ([`sun_corona.wgsl`](assets/shaders/sun_corona.wgsl)).
- Positions come from `terramenta-solare`'s frame tree evaluated at the
  simulated clock.
- Camera orbits an anchor (Sun, Earth, Mars, barycentre, or a mission
  spacecraft) with drag/WASD/arrows and zooms with scroll/`+`/`-`. Keys `1`–`4`
  select Sun, Earth, Mars, barycentre; `5` follows the first mission.

[`solar.rs`](src/solar.rs) bridges the frame tree to the scene. `FloatingOrigin`
names the frame world space is centred on; `place_solar_bodies` computes each
`SolarBody`'s state relative to it in `f64` (via
`FrameTree::state_of_relative_to`) before narrowing to `f32`.
`update_spacecraft` applies sphere-of-influence reparenting each tick.

## Missions

[`mission.rs`](src/mission.rs) flies interplanetary transfers against the
simulated clock using `terramenta-solare`.

```js
globe.addMission("mars-1", {});  // Earth → Mars, searched from the current clock
globe.addMission("mars-2", { departureSearchDays: 400, arrivalSearchEndDays: 900 });
globe.followMission("mars-1");
globe.removeMission("mars-1");

globe.onState(({ missions }) => {
  // [{id, origin, destination, status, departureUnixSeconds, arrivalUnixSeconds,
  //   departureDeltaVKmS, arrivalDeltaVKmS, orbiting, reason, phases}, ...]
});
```

Request options and defaults: `origin` `"Earth"`, `destination` `"Mars"`,
`departureSearchDays` 120, `arrivalSearchStartDays` 150,
`arrivalSearchEndDays` 420, `searchSteps` 17, `parkingAltitudeKm` 300.

Lifecycle (`status`):

1. `searching` — `find_best_transfer_window` scores a departure × arrival grid
   of Lambert transfers.
2. `waiting` — window found; waits for the clock to reach departure.
3. `enroute` — spacecraft spawned in a parking orbit on the escape hyperbola
   from `escape_injection_state` (which matches the target v∞ at the
   sphere-of-influence radius rather than at infinity). Patched-conics
   propagation hands it Earth → Sun → Mars.
4. `arrived` on capture, or `failed` with a `reason`.

`orbiting` names the current primary. `phases` always holds four entries
(`departure`, `escape`, `arrival`, `capture`) with `unixSeconds` (null until
known) and `reached`. Running the clock back past departure un-launches the
spacecraft; running forward relaunches it.

[`trail.rs`](src/trail.rs) draws two trails per spacecraft:

- **Flown** (solid white): recorded samples, stored relative to both the origin
  body and the barycentre, trimmed when the clock runs backward.
- **Planned** (dashed yellow): computed at launch — the departure hyperbola to
  the origin's SOI (globe view) and the Lambert arc to arrival (heliocentric
  view). Dash length scales with camera distance.

`followMission(id)` locks by mission id, so it can be set before launch (camera
waits on the origin body) and persists across relaunch. In the globe view the
camera trails the spacecraft out of its parking orbit with Earth behind it;
`setHeliocentricAnchor` releases the lock.

## Geometry storage

All vector data (GeoJSON, MVT, computed orbits) is stored in one
[GeoArrow](https://geoarrow.org) store ([`features.rs`](src/features.rs)) built
with the [`geoarrow`](https://github.com/geoarrow/geoarrow-rs) crates: point,
line and polygon arrays, an owner column mapping shapes to features, and two
per-feature string columns (`id`, raw `properties` JSON).

```
points     coords                                              owner
           [ lon lat h | lon lat h | ... ]                      [ 0, 0, 3, ... ]

lines      coords                                   offsets     owner
           [ lon lat h | lon lat h | ... ]           [0, 2, 7]   [ 1, 4, ... ]

polygons   coords                    ringOffsets    offsets     owner
           [ lon lat h | ... ]        [0, 4, 7]      [0, 2]      [ 2, ... ]
```

- Coordinates are `f64` from parse through picking, tessellation and export;
  the only narrowing is to `f32` when writing GPU vertex buffers.
- One allocation per buffer, not per ring. Picking reads buffers directly.
- Interleaved `xyz` layout (GeoArrow's alternative to separated arrays), chosen
  because all consumers read whole coordinates, and it exports as one typed
  array.
- Rings are stored open (closing vertex dropped).
- Multi-geometries are flattened into points, lines and polygons sharing an
  owner; there is no `MultiLineString` or `GeometryCollection` type.
- `properties` is kept as JSON text and parsed only on pick.

```js
const g = globe.overlayGeometry("quakes");
const [lon, lat, height] = g.points.coords.subarray(0, 3);  // Float64Array view into WASM memory
const feature = g.points.features[0];                        // index accepted by pinFeature
```

Line `i` spans `offsets[i]..offsets[i+1]` in coordinates; polygon `offsets`
index `ringOffsets`, which index `coords` (outer ring first). Views are
invalidated by any WASM allocation and by layer refresh or removal: read
synchronously, `.slice()` to keep or post to a worker.

## Key bindings

`setKeyboardEnabled(false)` disables all of them.

| Input | Action |
| --- | --- |
| Drag / one-finger drag | Orbit |
| Ctrl + drag | Heading and tilt about the look-at point (globe view) |
| Shift + drag | Rotate the view in place (globe view) |
| Scroll, trackpad pinch, two-finger pinch | Zoom |
| `W` `A` `S` `D` / arrows | Orbit |
| `+` `-` | Zoom |
| `U` | Switch globe / heliocentric view |
| `1` `2` `3` `4` | Heliocentric anchor: Sun, Earth, Mars, barycentre |
| `5` | Follow the first mission |
| `Space` | Switch ECEF / ECI |
| `G` | Toggle GNC overlay (both frames) |
| `R` | Reset the view |
| `P` | Pause / run the clock |
| `,` `.` | Halve / double the clock rate |
| `N` | Set the clock to now |
| `I` | Toggle terminator shading |
| `T` | Toggle streamed imagery |
| `L` / `Shift`+`L` | Next / previous imagery layer |
| `V` / `Shift`+`V` | Toggle vector tiles / next vector tile source |
| `O` / `Shift`+`O` | Toggle satellites / orbit trails |
| `H` | Toggle the key legend |

## Conventions

- Unit sphere in Bevy's Y-up world: `+Y` north pole, `+Z` prime meridian, `+X`
  90° E (ECEF). Geographic data is stored in this frame.
- Equirectangular textures with `v = 0` at the north pole. Bevy's `Sphere`
  primitive is Z-up with opposite winding, hence the custom sphere builder.
- Positions are drawn on a sphere, not the WGS 84 ellipsoid.

## Layout

```
src/
  lib.rs           Plugin wiring, layer presets, GlobeConfig, app()
  main.rs          Native entry point
  api.rs           GlobeCommand queue and GlobeState snapshot
  wasm.rs          JavaScript bindings
  geo.rs           Lat/lon types, ray-sphere math, sphere mesh
  globe.rs         Surface, atmosphere and starfield entities
  starfield.rs     Shared procedural starfield material
  camera.rs        Globe orbit camera
  frame.rs         ECEF/ECI selection and rotation
  gnc.rs           Dual-frame overlay, DCM/quaternion, velocity transport
  time.rs          Simulated clock
  sun.rs           Solar position
  moon.rs          Lunar position
  imagery.rs       Imagery layers and imagery:// source
  wms.rs           WMS GetMap URLs
  wmts.rs          WMTS GetTile URLs (REST, KVP)
  tiles.rs         Tile grids, LOD selection, imagery streaming
  mvt.rs           MVT decode, Web Mercator, clipping
  vector_tiles.rs  Vector tile layers, mvt:// source, meshing
  features.rs      GeoArrow feature store
  fetch.rs         Slot-addressed asset source for layer documents
  geojson.rs       GeoJSON parsing and flattening
  simplestyle.rs   simplestyle-spec 1.1.0
  omm.rs           OMM parsing into SGP4 propagators
  tessellate.rs    Ear clipping, holes, antimeridian
  picking.rs       Overlay hit-testing
  overlays.rs      Overlay layers, geojson:// source, refresh, meshes
  ephemeris.rs     Satellite layers, omm:// source, propagation, arcs
  placemark.rs     Subsolar/sublunar icons
  view.rs          Globe/heliocentric view state and transition
  heliocentric.rs  Heliocentric bodies, spacecraft meshes, camera
  solar.rs         Frame tree resource, floating origin, spacecraft update
  mission.rs       Mission search/wait/launch state machine
  trail.rs         Flown and planned spacecraft trails
  hud.rs           Built-in readout
assets/shaders/
  globe.wgsl       Surface lighting, city lights, specular, clouds, haze
  atmosphere.wgsl  Additive atmosphere shell
  starfield.wgsl   Procedural stars
  tile.wgsl        Streamed imagery tile
  vector.wgsl      Pixel-sized markers, lines, fills
  icon.wgsl        Placemark icons
  sun.wgsl         Solar surface
  sun_corona.wgsl  Solar corona shell
assets/icons/      sun32.png, moon32.png
scripts/
  fetch-assets.sh  Downloads NASA textures
  build-wasm.sh    Builds the WASM module and assets
```
