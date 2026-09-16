# terramenta-globe

The renderer: a navigable 3D globe of Earth on [Bevy](https://bevy.org), drawn
through WebGPU in the browser and through Metal, Vulkan or DX12 on the desktop,
from the same code.

It is a component, not an application. Everything it can do is reachable from
outside — see [The control surface](#the-control-surface) — and
[`terramenta-webapp`](../terramenta-webapp/) is the reference example of driving
it. The native binary is the same globe with its own overlay and key bindings,
which is the quickest way to work on the renderer itself.

## Building it

```sh
./scripts/fetch-assets.sh                     # once: the NASA base textures
cargo run --release --bin terramenta-globe    # native
./scripts/build-wasm.sh --release             # WebAssembly, into ./dist
```

`build-wasm.sh` compiles to `wasm32-unknown-unknown`, runs `wasm-bindgen`, and
writes the module and an `assets/` folder to `--out-dir` (`./dist` by default).
That is the whole deliverable — no page, no styling. The web app's own
`scripts/build.sh` calls this with its own output directory.

The `wasm-bindgen` CLI must match the crate version exactly; the script checks
and tells you the command if it does not.

## The control surface

[`src/api.rs`](src/api.rs) is the whole of it, and [`src/wasm.rs`](src/wasm.rs)
binds it to JavaScript. It works in two directions, and both are one-way.

**Commands go in.** They are queued from wherever the caller happens to be and
drained inside the schedule, which is the only place that touches the `World`.
Nothing is applied mid-tick, so a burst from one interaction lands together on
the next frame — and a command sent before the globe has started is fine, it
lands on the first one. That is what lets an interface be built and wired up
while the module is still downloading.

**State comes out.** A snapshot is rebuilt from the same resources the built-in
readout draws from, and pushed to a listener about ten times a second — and
immediately whenever something with a control on it changes, so a toggle answers
on the next frame rather than on the next tick of the throttle. Nothing polls.

Because the built-in overlay reads that same snapshot, an external interface and
the globe's own readout cannot disagree; and because the key bindings go through
the same commands, an interface stays in step with the keyboard for free. Press
`P` with the reference app open and its pause control changes with it.

### From JavaScript

```js
import init, * as globe from "./globe/terramenta_globe.js";

await init();

// The catalogue answers before the globe exists, so an interface can be built
// from it rather than from a hard-coded copy.
globe.layers();  // [{index, label, protocol, maxLevel, tileSize, format}, ...]
globe.limits();  // {minAltitudeKm, maxAltitudeKm, minTimeScale, maxTimeScale}

globe.onState((state) => {
  // {camera: {center, altitudeKm}, frame: {mode, label}, sun, imagery,
  //  overlays, hud, cursor, keyboard} — see `GlobeState` in src/api.rs
});

globe.addOverlay("quakes", {
  url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson",
  refreshSeconds: 60,
});

globe.setHudVisible(false);        // queued; applied when the globe starts
globe.start("#terramenta", "globe/assets");
```

`start` takes the canvas to draw on and where the assets are, both relative to
the page. The asset path matters if the module is kept in a subfolder: the
browser resolves it against the page, not against the module, so a globe at
`globe/terramenta_globe.js` still looks in `assets/` unless told otherwise.

`start` does not return the way it looks like it should. Winit hands control to
the browser's event loop by unwinding through an exception, so the call throws
once the globe is running. [`globe.js`][globejs] in the reference app shows how
to tell that apart from a real failure.

[globejs]: ../terramenta-webapp/src/globe.js

| | |
| --- | --- |
| `layers()` `limits()` | The imagery presets and the ranges the controls accept |
| `lookAt(lat, lon, altitudeKm?)` | Look straight down at a coordinate |
| `setAltitude(km)` `zoomBy(exp)` `orbitBy(yawDeg, pitchDeg)` `resetView()` | Move the camera |
| `setFrame("ecef" \| "eci")` `toggleFrame()` | Which frame the scene is drawn in |
| `setSunPaused(bool)` `setTimeScale(n)` `setClock(unixSeconds)` `snapClockToNow()` | The simulated clock |
| `setSunShaded(bool)` | Terminator, or flat full daylight |
| `setLayer(i)` `nextLayer()` `previousLayer()` `setImageryEnabled(bool)` | Streamed imagery |
| `addOverlay(id, options)` `removeOverlay(id)` | GeoJSON overlays |
| `setOverlayVisible(id, bool)` `setOverlayStyle(id, style)` `setOverlaysEnabled(bool)` | How an overlay is drawn |
| `setOverlayAltitude(id, altitude)` | How high it is drawn |
| `setOverlayRefresh(id, seconds)` `refreshOverlay(id)` | When it refetches |
| `setPickingEnabled(bool)` `pinFeature(layer, index)` `clearPinnedFeature()` | Picking features out of an overlay |
| `setHudVisible(bool)` `setHelpVisible(bool)` `setKeyboardEnabled(bool)` | The globe's own overlay and keys |
| `onState(callback)` | The state stream. One listener; registering again replaces it |

### From Rust

A native embedder uses the same queue through `api::send`, and reads state
straight out of the `World` from the `LatestState` resource rather than through
a listener. `app(GlobeConfig { .. })` builds the `App` without running it, for a
host that wants to add plugins of its own first.

## GeoJSON overlays

Vector data drawn over the imagery: markers, lines and filled rings from any
[GeoJSON](https://datatracker.ietf.org/doc/html/rfc7946) document. Several
layers can be up at once, each with its own colours, its own visibility and its
own refresh period, and the state stream reports what every one of them holds
and is doing.

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

// A file the user picked. The globe cannot fetch it, so it is handed the text.
globe.addOverlay("local", { text: await file.text() });
```

An id is the layer. Adding a second overlay under one already in use replaces
it, which is what makes re-sending a re-read file an update rather than a second
copy of the layer.

### Height

A GeoJSON position may carry a third element, and the globe draws it: a marker
stands off the surface, and a line climbs evenly between the heights of the
corners it was given, so a flight path or a balloon track is drawn where it says
it is rather than flattened onto the ground.

What that element *means* is the layer's to say, because RFC 7946 calls it
elevation in metres loosely enough that feeds disagree:

```js
// Kilometres above the ground rather than metres.
globe.addOverlay("flight", { url, altitudeScale: 1000 });

// The USGS feeds put depth there, not height. Draw the layer flat.
globe.addOverlay("quakes", { url, altitudeMode: "clampToSurface" });

// Either can be changed later. The layer is rebuilt where it stands: nothing is
// refetched, and a pinned feature stays pinned.
globe.setOverlayAltitude("quakes", { altitudeMode: "relativeToSurface" });
```

`altitudeScale` is metres of height per unit of that element, and a negative one
reads a feed that counts downward. Nothing is ever drawn below the surface: a
height at or under sea level draws exactly where a clamped one does.

### Extruding

A ring at a height is a lid hanging in the air with nothing under it. `extrude`
drops a filled wall from every edge of every ring to the surface, which is what
KML means by the same word:

```js
// A square ring at 222 km, walled to the ground: a box standing on the equator.
globe.addOverlay("cube", { url, extrude: true });
```

The walls are part of the fill — same colour, same draw — because a wall in a
different colour from the lid it holds up reads as two shapes rather than one
solid. They are densified like an outline, so a wall around anything large
follows the curve of the globe instead of cutting through it, and a ring whose
corners are at different heights gets a wall whose top edge slopes the way its
outline does. Holes are walled too, which is what makes an extruded ring with a
hole read as a shape with a shaft through it.

It is a layer setting rather than a per-feature one, like colour: a layer of
building footprints wants all of them extruded, and a layer of airspace shelves
wants none of them, because the shelves *are* the shape. Nothing in a document's
`properties` is read to decide it — see the note on properties above.

Two things it is not. It does not fill an arbitrary vertical face: the fill is
triangulated in latitude and longitude, where a wall is degenerate, so walls are
generated from edges rather than given as geometry. And it does not extrude
lines — a curtain under a flight path is the same machinery, but it is not
wired up.

Three things follow from how it is done, and are worth knowing before relying on
it:

- Height is measured up from the radius overlays are draped at, not from the
  sphere. That drape is what clears the imagery, and the imagery is the ground
  as far as anything looking at the screen is concerned — a tile stands up to
  eleven kilometres proud of the sphere at its corners, so measuring from the
  sphere would swallow the first ten kilometres of every track.
- A **fill** is drawn at one height, the mean of its outer ring, where its
  outline follows every corner. Triangulation duplicates and reorders corners,
  so a height per corner would have to be carried through ear clipping, and what
  it would buy is a fill that folds. The way to build a shape that has structure
  in the vertical, then, is several rings rather than one: a `MultiPolygon` whose
  polygons each sit flat at their own height stacks into a volume, which is what
  the airspace in the reference app's sample is made of — a surface core, three
  annular shelves with floors stepping up as they reach further out, and a
  ceiling over all of it.
- **Picking** reads the ground, not the height. A shape at altitude is picked
  where it stands rather than where it is drawn — the same place looking
  straight down, and further apart the more the camera is tilted.

### The two kinds of source

A **URL** is the globe's to fetch, over the same asset machinery the imagery
tiles use — [`overlays.rs`](src/overlays.rs) registers a `geojson://` source
whose reader turns a layer's path into its URL, so an overlay loads with an
ordinary `asset_server.load` and gets asynchronous I/O on native and in the
browser alike. Being able to fetch it is also what makes it refreshable.

**Text** is the embedder's. A browser will not let the globe open a path off the
user's machine, so a local file is read by the page and the GeoJSON handed over
as a string. The globe cannot go back for more of it; refreshing a file is the
embedder re-reading it and sending it again, which
[`overlays.js`](../terramenta-webapp/src/overlays.js) in the reference app does
on a timer of its own.

### Refreshing

`refreshSeconds` sets the period, `setOverlayRefresh(id, null)` stops it, and
`refreshOverlay(id)` refetches now. The old geometry stays on screen until the
new document lands, so a feed that goes down does not blank the layer; if the
fetch fails, the layer reports `status: "failed"` with the reason and keeps
drawing what it had.

Refreshing has to get past two caches — Bevy's, which is keyed by asset path,
and the browser's, which is keyed by URL. Both are handled the same way: the
path carries a generation, and from the second fetch onward so does the request,
as a `_terramenta=<n>` parameter. A layer that never refreshes never gets that
parameter, so a signed or otherwise parameter-sensitive URL still works.

### How it is drawn

Markers and lines are sized in **pixels**, not on the ground. A dot six
kilometres across is a continent from orbit and invisible from a low pass, so
the mesh carries one anchor per marker and one spine per line and the corners
are spread in [`vector.wgsl`](assets/shaders/vector.wgsl), from how much world
one pixel covers at that depth. A layer never has to be rebuilt for a zoom.

Rings are filled by ear clipping in [`tessellate.rs`](src/tessellate.rs), which
splices holes into the outer ring along a bridge first. Two things about it are
worth knowing:

- A ring crossing the **antimeridian** is handled: longitudes are allowed to run
  past ±180 so the ring stays the shape it looks like on a globe.
- A ring enclosing a **pole** is not. It does not close in a latitude/longitude
  plane at all, so Antarctica as a single polygon fills wrong. Its outline is
  still right, and outlines are drawn whether or not the fill is.

Past 8,000 corners a polygon is outlined but not filled, and past 2,000 its
holes are dropped — ear clipping is quadratic, and a coastline dataset would
otherwise stall a frame.

Overlays are drawn unlit and above the deepest imagery tile. Unlit because an
overlay is annotation rather than imagery: a track across the night side has to
stay as readable as the same track at noon.

### Picking

The globe hit-tests the cursor against overlay geometry on every frame it is
over the globe, reports what it found, and draws a halo around it.

```js
globe.onState(({overlays}) => {
  overlays.hovered;  // {layer, label, index, id, kind, properties} or null
  overlays.pinned;   // the same, for whatever was pinned
});

// A click is not something the globe knows about. It reports what is under the
// pointer; deciding that one of those is *the* selection is the interface's.
globe.pinFeature(hovered.layer, hovered.index);
globe.clearPinnedFeature();
```

`properties` is the feature's `properties` object exactly as the document wrote
it. Nothing in the globe reads it — what a `mag` or a `place` means is the
feed's business and the interface's — so it goes out untouched, nesting and all.

**A feature, not a shape.** A `MultiPolygon` of forty islands is one feature, so
picking any island highlights the country. That is why
[`geojson.rs`](src/geojson.rs) flattens the document without throwing away which
feature each shape came from.

**Topmost wins by kind, then by distance** — a marker over a line over a fill,
which is the order they are drawn in and the only order that makes a marker on a
filled country selectable at all. The rule is applied across layers as well as
within one.

**The tolerance is in pixels**, because the geometry is: a nine-pixel marker is
a nine-pixel target from any altitude. [`picking.rs`](src/picking.rs) converts
that to degrees at the cursor's distance and works in degrees from there,
scaling longitude by the cosine of the latitude so that the number means the
same thing at any latitude — and progressively less near the poles, where
nothing round stays round on this projection anyway.

**Lines are measured the way they are drawn.** A segment is interpolated in
latitude and longitude rather than along a great circle, and the hit test does
the same; measuring the chord in three dimensions would quietly disagree with
the line on screen for any segment long enough to matter.

The highlight is the picked feature drawn again — larger, near-white, and
*behind* itself, so a marker keeps its own colour and gains a halo rather than
disappearing under a blob. It is rebuilt only when the pick changes, and a pick
is forgotten when its layer is refreshed: feature seven of the new document is a
different earthquake.

### Things to get right

- **CORS, again.** The browser needs `Access-Control-Allow-Origin` from whoever
  serves the document. The USGS feeds send `*`; most other things do not.
- **Order is longitude, then latitude.** RFC 7946 positions are `[lon, lat]`,
  which is the opposite of how a coordinate is spoken. A third element —
  elevation, or depth in the USGS feeds — is read past: everything is draped on
  the surface.
- **Properties are for picking, not for styling.** They are kept and handed
  back when a feature is picked, but nothing reads them to decide a colour:
  styling is per layer, so two feeds are told apart by being two colours.

## Controls

The globe binds these itself. `setKeyboardEnabled(false)` switches them all off
for an embedder that would rather bind its own.

| Input | Action |
| --- | --- |
| Drag / one-finger drag | Orbit |
| Scroll, pinch, two-finger pinch | Zoom |
| `W` `A` `S` `D` or arrows | Orbit |
| `+` `-` | Zoom |
| `Space` | Switch between the ECEF and ECI reference frames |
| `R` | Reset the view |
| `P` | Pause the sun |
| `,` `.` | Halve / double the sun's speed |
| `N` | Snap the clock back to now |
| `I` | Drop the terminator and light the whole globe |
| `T` | Toggle streamed imagery |
| `L` / `Shift`+`L` | Next / previous imagery layer |
| `H` | Hide the control legend |

## Layout

```
src/
  lib.rs       Plugin wiring, the layer presets, and the two ways in
  main.rs      The native entry point, and nothing else
  api.rs       Commands in, state out — the control surface
  wasm.rs      That surface, bound to JavaScript
  geo.rs       Lat/lon conventions, ray-sphere math, the sphere mesh builder
  globe.rs     Surface, atmosphere and starfield materials and entities
  camera.rs    Altitude-scaled orbit controller (mouse, keys, touch, gestures)
  frame.rs     ECEF/ECI, and the rotation that relates them
  sun.rs       Simulated clock and solar position
  imagery.rs   Layer selection and the `imagery://` asset source
  wms.rs       WMS GetMap request URLs
  wmts.rs      WMTS GetTile request URLs, REST and KVP
  tiles.rs     Tile grids, level-of-detail selection and streaming
  geojson.rs   The GeoJSON document format, flattened to drawable geometry
  tessellate.rs  Rings to triangles: ear clipping, holes and the antimeridian
  picking.rs   Which feature is under the cursor
  overlays.rs  Overlay layers: sources, refresh, meshes and the `geojson://` source
  hud.rs       The built-in readout, formatted from the state snapshot
assets/shaders/
  globe.wgsl        Day/night, city lights, ocean specular, clouds, limb haze
  atmosphere.wgsl   Additive scattering shell
  starfield.wgsl    Procedural stars and galactic band
  tile.wgsl         A single streamed imagery tile
  vector.wgsl       Overlay markers, lines and fills, sized in pixels
scripts/
  fetch-assets.sh  Downloads the NASA imagery
  build-wasm.sh    Builds the WebAssembly module and its assets
```

### Conventions worth knowing

The globe is a unit sphere in Bevy's Y-up world space: `+Y` is the north pole,
`+Z` is the prime meridian, `+X` is 90° east — that is the Earth-fixed (ECEF)
frame, which everything geographic is stored in. Textures are equirectangular with
`v == 0` at the north pole. Bevy's built-in `Sphere` primitive is Z-up and wraps
the other way, so [`geo::equirectangular_sphere`](src/geo.rs) generates its own
grid instead — that one convention is what makes the coordinate readout, the
texture alignment and the sun position agree.

[`frame::ReferenceFrame`](src/frame.rs) decides which frame world space *is*.
In ECEF it is the identity: the globe stands still and the sun sweeps around it
once a day. In ECI, world space is inertial — the stars hold still, the sun
holds still but for the degree a day the Earth's orbit moves it, and the globe
turns underneath at the sidereal rate. Anything Earth-fixed (the globe mesh, the
imagery tiles) is rotated into world space by `earth_to_world`, and anything
read back out of the scene — the cursor coordinate, the tile quadtree walk — is
rotated back by `world_to_earth`. The sun direction handed to the shaders is
always a world-space vector, so the terminator lands on the same ground in
either frame.

The surface is lit in [`globe.wgsl`](assets/shaders/globe.wgsl) rather than
through Bevy's PBR pipeline. There is no `DirectionalLight` in the scene at all:
one sun direction uniform drives the terminator, the specular and the city
lights, which keeps the planet to a single draw call and makes the look directly
adjustable.

## Streaming imagery

The globe can drape itself in imagery from any [OGC Web Map
Service](https://www.ogc.org/standards/wms/) or [Web Map Tile
Service](https://www.ogc.org/standards/wmts/). The readout shows the protocol
and layer in use, the deepest level on screen, and how many tiles are drawn or
in flight.

### The two protocols

They differ in who owns the tiling, and that difference drives the design.

**WMS** answers `GetMap` for an arbitrary bounding box. That is too open-ended to
cache, so requests are pinned to a grid of our choosing — the **global geodetic
quadtree**: level 0 is two 180°x180° tiles, and each level quarters them, giving
`2^(n+1) x 2^n` tiles at level `n`.

**WMTS** only serves tiles it has already cut, addressed by a **tile matrix set**
the server publishes. That is the better deal — every tile is pre-rendered and
cached, so responses are fast and identical for everybody — but the grid is no
longer ours to pick, and published grids are often not the tidy one.

NASA GIBS is the case in point. Its `EPSG:4326` matrix sets start from a level-0
tile 288° on a side, not 180°, so:

- the coarse levels **hang off the edge of the world** — tile `0/0/0` covers
  180° W to 108° E and runs 108° past the south pole, with the overhang
  returned as black pixels;
- the matrices are **not square powers of two**. They run 2x1, 3x2, 5x3, 10x5,
  20x10 — enough tiles of that span to reach the far edge, rounded up. Asking
  for a column past the end is a `400`, not an empty tile.

Both are handled by [`TileGrid`](src/tiles.rs), which describes any plate-carrée
pyramid with two numbers — the north-west origin and the level-0 tile span — and
by `TileGrid::clipped_bounds`, which trims a tile to the part that is really on
the globe. A clipped tile is meshed over only its real extent, with texture
coordinates still measured against its full one, and given fewer quads rather
than smaller ones so its geometry meets its neighbours' without a crack.

### Level of detail

Each frame the tree is walked from the roots. A tile splits when its projected
size on screen exceeds the resolution of the image behind it, so detail follows
the camera; tiles over the horizon, and tiles that fall entirely off the world,
are skipped. Tiles that have not arrived yet fall back to the nearest ancestor
that has, and failing that to the base globe underneath, so the view is never
holed. Requests are capped both in flight and per frame, because one fast zoom
will otherwise queue hundreds of tiles that are stale before they land.

### Fetching

Fetching is handed to Bevy rather than hand-rolled.
[`imagery.rs`](src/imagery.rs) registers an `imagery://` asset source whose
reader turns a tile path like `0/4/9/3.jpg` into whatever URL the active layer
wants — a `GetMap` query for WMS, a REST path or `GetTile` query for WMTS — and
delegates to Bevy's HTTP reader. A tile then loads with an ordinary
`asset_server.load`, and async I/O, image decoding, GPU upload and reference
counting all come for free — identically on native and on the web.

### Pointing it at your own server

Layers live in `imagery_layers()` in [`lib.rs`](src/lib.rs). For WMTS, the
quickest route is a `ResourceURL` template copied out of the server's
`WMTSCapabilities.xml`:

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

`TileGrid::from_scale_denominator` does the conversion the specification
prescribes — denominator x 0.28 mm, read as degrees — so the level-0
`ScaleDenominator` and `TileWidth` can be transcribed straight from the document
rather than worked out by hand. Servers that publish no template take
`.with_kvp(endpoint)` instead, which sends `GetTile` as a query string.

Things to get right:

- **Row is latitude, column is longitude.** `TileRow` counts south and `TileCol`
  counts east, so a `{TileRow}/{TileCol}` template takes `y` before `x`.
  Swapping them yields imagery that is wrong rather than missing.
- **`max_level` means different things.** For WMS it is taste — how far to
  refine before the imagery stops repaying the requests. For WMTS it is the
  depth of the matrix set, and a level past it is an error from the server.
- **Axis order, for WMS.** 1.3.0 declares `EPSG:4326` latitude-first and renamed
  `SRS` to `CRS`; 1.1.1 treats the same code as longitude-first. Picking the
  wrong `WmsVersion` yields a world turned on its side rather than an error, so
  `WmsVersion` encodes both differences together.
- **CORS.** The browser build needs `Access-Control-Allow-Origin` from the
  imagery host. NASA GIBS sends `*`; a private GeoServer usually needs
  configuring.

Adding a layer needs nothing on the JavaScript side: the presets are published
through `layers()`, so an interface built from the catalogue picks it up.
