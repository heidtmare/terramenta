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
globe.layers();        // [{index, label, protocol, maxLevel, tileSize, format}, ...]
globe.vectorLayers();  // [{index, label, maxLevel, sourceLayers}, ...]
globe.limits();        // {minAltitudeKm, maxAltitudeKm, minTimeScale, maxTimeScale,
                       //  maxVectorTileLatitude}

globe.onState((state) => {
  // {camera: {center, altitudeKm}, frame: {mode, label}, sun, imagery,
  //  vectorTiles, overlays, ephemerides, hud, cursor, keyboard} — see
  // `GlobeState` in src/api.rs
});

globe.addOverlay("quakes", {
  url: "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson",
  refreshSeconds: 60,
});

globe.addEphemeris("stations", {
  url: "https://celestrak.org/NORAD/elements/gp.php?GROUP=stations&FORMAT=json",
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
| `layers()` `vectorLayers()` `limits()` | The imagery and vector tile presets, and the ranges the controls accept |
| `lookAt(lat, lon, altitudeKm?)` | Look straight down at a coordinate |
| `setAltitude(km)` `zoomBy(exp)` `orbitBy(yawDeg, pitchDeg)` `resetView()` | Move the camera |
| `setFrame("ecef" \| "eci")` `toggleFrame()` | Which frame the scene is drawn in |
| `setGncEnabled(bool)` `toggleGnc()` | Draw *both* frames at once, over whichever one is active |
| `setGncEciAxes(bool)` `setGncEcefAxes(bool)` `setGncSidereal(bool)` | The two triads and the angle between them |
| `setGncGraticule(bool)` `setGncGraticuleStep(deg)` | The lat/lon grid on the ground |
| `setGncTrack(bool)` `setGncTrackOrbits(n)` `setGncFocus(layer, noradId)` | One satellite's orbit and ground track, drawn together |
| `setSunPaused(bool)` `setTimeScale(n)` `setClock(unixSeconds)` `snapClockToNow()` | The simulated clock |
| `setSunShaded(bool)` | Terminator, or flat full daylight |
| `setLayer(i)` `nextLayer()` `previousLayer()` `setImageryEnabled(bool)` | Streamed imagery |
| `setVectorTileLayer(i)` `nextVectorTileLayer()` `previousVectorTileLayer()` `setVectorTilesEnabled(bool)` | Streamed vector tiles |
| `setVectorTileStyle(style)` | What they are drawn in |
| `addOverlay(id, options)` `removeOverlay(id)` | GeoJSON overlays |
| `setOverlayVisible(id, bool)` `setOverlayStyle(id, style)` `setOverlaysEnabled(bool)` | How an overlay is drawn |
| `setOverlaySimpleStyle(id, bool)` | Whether the document's own simplestyle members override that |
| `setOverlayAltitude(id, altitude)` | How high it is drawn |
| `setOverlayRefresh(id, seconds)` `refreshOverlay(id)` | When it refetches |
| `setPickingEnabled(bool)` `pinFeature(layer, index)` `clearPinnedFeature()` | Picking features out of an overlay |
| `overlayGeometry(id)` | A layer's coordinates as typed arrays, without a copy |
| `addEphemeris(id, options)` `removeEphemeris(id)` | Satellite layers, from an OMM catalogue |
| `setEphemerisVisible(id, bool)` `setEphemerisStyle(id, style)` `setEphemeridesEnabled(bool)` | How one is drawn |
| `setEphemerisSelection(id, noradIds)` `selectSatellite(id, noradId, bool)` | Which objects are drawn |
| `setEphemerisTrails(id, bool)` `setSatelliteTrail(id, noradId, bool)` `setEphemerisTrail(id, window)` | How much orbit is drawn through each |
| `setEphemerisRefresh(id, seconds)` `refreshEphemeris(id)` | When the catalogue refetches |
| `ephemerisObjects(id)` `ephemerisGeometry(id)` | What a layer holds, and where it currently is |
| `pinPlacemark("sun" \| "moon")` `clearPinnedPlacemark()` | Picking the subsolar and sublunar icons |
| `setHudVisible(bool)` `setHelpVisible(bool)` `setKeyboardEnabled(bool)` | The globe's own overlay and keys |
| `onState(callback)` | The state stream. One listener; registering again replaces it |

### From Rust

A native embedder uses the same queue through `api::send`, and reads state
straight out of the `World` from the `LatestState` resource rather than through
a listener. `app(GlobeConfig { .. })` builds the `App` without running it, for a
host that wants to add plugins of its own first.

## Streaming vector tiles

[Mapbox Vector Tiles](https://github.com/mapbox/vector-tile-spec) from any
`{z}/{x}/{y}` service, drawn on the globe as lines, rings and markers. It is the
imagery streamer's idea applied to geometry: a quadtree walked from the camera
each frame, tiles fetched through an asset source, coarser ancestors standing in
for whatever has not arrived. What is in the tile is geometry rather than
pixels, so instead of a texture it produces a vertex buffer.

```js
globe.vectorLayers();              // the presets, with their source layers
globe.setVectorTileLayer(1);       // OpenStreetMap Shortbread
globe.setVectorTileStyle({ lineColor: "#8fd6ff", lineWidthPx: 1.2 });
globe.setVectorTilesEnabled(false);
```

Two keyless sources are wired up in `vector_tile_layers()` in
[`lib.rs`](src/lib.rs) — MapLibre's demo country boundaries, and OpenStreetMap's
own Shortbread tiles — because almost every hosted vector tile service wants a
token, and a globe that draws nothing until one is pasted in looks broken. An
embedder with a key of its own builds the `App` itself and passes its own
presets. A layer is a URL template, a depth, a list of source layers and a
style:

```rust
VectorTileLayer::new("My basemap", "https://example.org/tiles/{z}/{x}/{y}.mvt")
    .with_max_level(12)
    .with_source_layers(["water", "boundary"])
```

### Decoding

The protobuf is read by [`geozero`](https://github.com/georust/geozero), whose
MVT reader walks a source layer and calls back for every ring, strand and point.
[`mvt.rs`](src/mvt.rs) is what sits either side of that walk: it collects the
geometry, clips it, unprojects it, and hands back the same flattened lists of
points, lines and rings a GeoJSON document collapses into — which is why
[`vector_tiles.rs`](src/vector_tiles.rs) can build its meshes with the overlay
mesh builders unchanged, and why a vector tile is drawn by the same
[`vector.wgsl`](assets/shaders/vector.wgsl) that draws an overlay, in pixels
rather than in kilometres.

Decoding happens in Bevy's asset pipeline — a task thread natively, a microtask
in the browser — so the schedule only ever sees a finished tile. Meshing has to
happen on the schedule, where `Assets<Mesh>` is, so it is rationed to two tiles
a frame: one zoomed-in city tile can hold several thousand rings, and ear
clipping all of them at once is a visible hitch.

### The grid, and the projection

Vector tiles are cut in **Web Mercator**, where level `n` is a square `2^n`
tiles on a side. The imagery here is cut in plate carrée, where level `n` is
`2^(n+1)` by `2^n`. The two cannot share a `TileGrid`, so the walk in
`vector_tiles.rs` is its own; `TileId` is shared, because a level with a column
and a row is a level with a column and a row.

Inside a tile, coordinates are integers running `0..extent` with `y` pointing
south. `mvt::unproject` takes them back to WGS 84 latitude and longitude, which
is the coordinate system everything else here speaks. Two things about that are
worth being exact about:

- **Web Mercator projects geodetic WGS 84 latitudes through spherical Mercator
  formulas.** That mismatch is the standard's own, and every tile that has ever
  been cut assumes it, so the inverse here does the same and hands back the
  geodetic coordinate the tile's data was surveyed in.
- **Where that coordinate is drawn is a separate question**, and the answer is
  the sphere everything else is drawn on. Giving vector tiles an ellipsoid of
  their own would put their lines up to twenty kilometres off the imagery they
  annotate.

**Nothing above 85.05° is in any tile.** Mercator sends the poles to infinity,
so the grid stops where the projected world is square — which is why a Mercator
basemap has no Arctic Ocean. `limits().maxVectorTileLatitude` reports it, so an
interface can say so rather than leave someone wondering.

### Clipping, and why a ring is clipped twice

Tiles are cut with a buffer, so a road that leaves the tile is carried some way
past the edge and the neighbouring tile carries the same stretch back the other
way. Drawn as they arrive, every seam in the world gets two copies of everything
crossing it — a ladder of darker rungs on alpha-blended lines, a darker frame
around every fill. So geometry is clipped to the tile's own square first, in
tile coordinates, where the square is exact.

A ring cannot be clipped once, though, because the two things drawn from it want
opposite answers. A **fill** wants the ring closed against the tile edge, so the
piece of Brazil in this tile and the piece in the next meet along the seam with
no gap and no overlap. An **outline** wants the opposite: the tile edge is not a
coastline, and closing the ring against it would draw the grid over the planet.
So every ring is clipped both ways — as a ring for the fills, and as an open
path for the outline, whose surviving runs are the parts that are really a
boundary.

### How it is drawn

Fills are **off by default**: a basemap's rings are most of its geometry, and
filling them would hide the imagery the globe is showing. A fully transparent
fill is not drawn at all rather than drawn invisibly, which also skips the
triangulation — the most expensive part of a tile.

Vector tiles sit at the same radius as the overlays, clear of the deepest
imagery tile, but under them in the transparent pass: a basemap is the thing
annotations are drawn *on*, so a coastline out of a tile stays beneath the
earthquakes over it and beneath the halo around the one that is picked.

### Things to get right

- **Gzip.** Many services answer `Content-Encoding: gzip`. A browser unwraps
  that before the bytes reach the globe, so the web build takes any service; a
  native build may receive it compressed, and a tile that arrives gzipped is
  reported as such rather than as a broken protobuf.
- **Source layers.** A zoom-14 tile holds roads, buildings, land use, water,
  labels and housenumbers. Drawing all six is a mat of ink, so name the ones you
  want; the rest are skipped before they are decoded.
- **Features are drawn, not picked.** A screenful of tiles is tens of thousands
  of features against an overlay's tens, and the index that makes picking cheap
  would cost more to build, every time the camera moved, than picking a basemap
  is worth. Every feature does carry the layer it came from as a `sourceLayer`
  property, alongside its own, for whoever wants to change that.
- **CORS**, as ever. The browser needs `Access-Control-Allow-Origin` from the
  tile host.

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

### When the document has its own opinion

A layer has one style, but a GeoJSON document may style itself feature by
feature, in the `properties` members of [simplestyle-spec 1.1.0][simplestyle] —
`marker-size`, `marker-color`, `stroke`, `stroke-opacity`, `stroke-width`,
`fill`, `fill-opacity`, and `title`, `description` and `marker-symbol` beside
them. [`simplestyle.rs`](src/simplestyle.rs) reads them as the document is
parsed, off the values the decoder has already produced.

Three rules make it fit a globe that already had a colour for every layer:

- **A member overrides the one thing it names, and nothing else.** The
  specification's defaults — grey markers, a `#555555` stroke, a fill at 0.6 —
  are *not* applied: a document that says nothing about its appearance is drawn
  in the layer's colours, exactly as before. `fill` replaces the fill colour and
  leaves the layer's opacity; `fill-opacity` replaces the opacity and leaves the
  colour. So an interface can recolour a layer and only the features that asked
  for nothing move.
- **It is still three draws.** A styled feature's colour and size ride in its
  own vertices; a vertex whose feature asked for nothing carries a sentinel that
  sends the shader back to the material's uniform. That is what lets one draw
  hold nine hundred rings following the interface's colour and one in the red
  the document asked for — and what keeps `setOverlayStyle` a uniform write
  rather than a rebuild, without it stamping over the red one.
- **What cannot be drawn is handed on.** `title`, `description` and
  `marker-symbol` need a label engine and an icon atlas, and the globe has
  neither. They go out with the picked feature, parsed, under `style`.

`setOverlaySimpleStyle(id, false)` — or `simpleStyle: false` when the layer goes
up — ignores the lot and draws the layer in its own colours. That one *is* a
rebuild, because where a feature's colour lives is in its vertices. The state
stream reports `styledFeatures` per layer, so an interface can say why a colour
it chose did not reach all of them.

A member that cannot be read is dropped and the layer's value stands: a document
should not lose its geometry over a typo in its styling.

### Picking

The globe hit-tests the cursor against overlay geometry on every frame it is
over the globe, reports what it found, and draws a halo around it.

```js
globe.onState(({overlays}) => {
  overlays.hovered;  // {layer, label, index, id, kind, properties, style} or null
  overlays.pinned;   // the same, for whatever was pinned
});

// A click is not something the globe knows about. It reports what is under the
// pointer; deciding that one of those is *the* selection is the interface's.
globe.pinFeature(hovered.layer, hovered.index);
globe.clearPinnedFeature();
```

`properties` is the feature's `properties` object exactly as the document wrote
it — what a `mag` or a `place` means is the feed's business and the interface's,
so it goes out untouched, nesting and all. `style` beside it is the one part the
globe does read: the [simplestyle-spec 1.1.0][simplestyle] members, parsed, for
a feature that carried any, so that an interface wanting a `title` or a
`marker-symbol` does not have to re-implement the parsing to find one.

[simplestyle]: https://github.com/mapbox/simplestyle-spec/tree/master/1.1.0

**A feature, not a shape.** A `MultiPolygon` of forty islands is one feature, so
picking any island highlights the country. That is why
[`geojson.rs`](src/geojson.rs) flattens the document without throwing away which
feature each shape came from.

**Topmost wins by kind, then by distance** — a marker over a line over a fill,
which is the order they are drawn in and the only order that makes a marker on a
filled country selectable at all. The rule is applied across layers as well as
within one.

**The tolerance is in pixels**, because the geometry is: a nine-pixel marker is
a nine-pixel target from any altitude — and per feature, because a document that
sized its own markers gets a target to match each one. [`picking.rs`](src/picking.rs) converts
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

## Satellites

An ephemeris layer is a catalogue of orbits rather than a document of places.
The source is [OMM] JSON — the Orbit Mean-Elements Message, the CCSDS standard
that replaced the two-line element set and the format every current catalogue
publishes. [`omm.rs`](src/omm.rs) reads the records and turns each into an SGP4
propagator; [`ephemeris.rs`](src/ephemeris.rs) evaluates them and draws the
result. Several layers can be up at once, each with its own colours, its own
selection and its own trail window, exactly as the overlays are — the two share
[`fetch.rs`](src/fetch.rs), the GeoArrow store and the mesh builders.

```js
addEphemeris("stations", {
  url: "https://celestrak.org/NORAD/elements/gp.php?GROUP=stations&FORMAT=json",
  label: "Crewed stations",
});
ephemerisObjects("stations");
// [{noradId: 25544, name: "ISS (ZARYA)", periodMinutes: 92.9, selected: true, ...}, ...]
selectSatellite("stations", 25544, true);
setSatelliteTrail("stations", 25544, true);
```

Four things about it are not like the other layers.

### It is computed, not loaded

Every other layer parses a document once and draws it until it is refetched.
This one propagates every drawn object **every frame**, against
`sun.unixSeconds` — the globe's *simulated* clock, not the wall one. So the
constellation obeys the clock controls like everything else: pause it and the
satellites stop, run a day every four minutes and they sweep, jump the clock a
week and they are where they will be.

That is also where the whole cost of the feature goes, and why there are
budgets. `MAX_TRACKED` bounds the markers at six hundred objects a layer, which
is a millisecond or two a frame. Trails are bounded differently, by a *sample*
budget shared out among whatever is trailed: one satellite gets the smoothest
orbit it asked for and sixty get a coarse one, and both cost the same. Holding
a per-satellite sample count fixed instead would make a layer slower as it was
switched on, where this makes it coarser — which is the right way round. The
arcs are also rebuilt on their own clock rather than per frame: when the
simulated time has moved a worthwhile fraction of the window an arc spans, at
most ten times a second and at least twice.

A layer reports `objects`, `tracked` and `trailed` separately, so an interface
can say that it drew six hundred of eight thousand rather than leaving the
difference to be discovered.

### The reference frame is in the geometry

SGP4 works in TEME, which is inertial; the globe draws in whichever frame world
space currently *is*. A marker could be carried across that by a transform, the
way an overlay is — but an arc could not, because each of its points belongs to
a **different moment**, and in ECEF the Earth turned underneath between them.

That is the whole difference between the two pictures. In ECI an orbit is the
closed ellipse it really is; in ECEF the same arc is the corkscrew a ground
track is. It cannot be a rigid rotation of one mesh, so every sample is turned
into the active frame at the moment it belongs to, the mesh comes out already in
world space, and the ephemeris entities carry no rotation at all. Switching
frames rebuilds them.

Press `Space` with a layer up and it is the clearest thing on the globe — and
`G` draws **both** frames at once, so the two pictures are side by side rather
than one after the other. See [Both frames at once](#both-frames-at-once).

### Coordinates are geocentric

A propagated position is reduced to a declination, a right ascension and a
radius; the radius becomes a height above a sphere of mean Earth radius. That is
not the WGS 84 ellipsoid, and deliberately not: it is the sphere the globe
actually draws, so a satellite sits over the imagery it is really over. A
geodetic latitude would be right about the Earth and wrong about the mesh. The
two differ by up to about a fifth of a degree of latitude.

### The object list is pulled, not streamed

A catalogue can be eight thousand rows and the state snapshot goes out ten times
a second, so the list is not in it. `ephemerisObjects(id)` returns it on demand,
and the snapshot carries a `revision` per layer that changes whenever the list
would — a catalogue landing, a selection moving. An interface pulls when that
number moves and not otherwise; [`ephemeris.js`][ephemerisjs] is the worked
example.

`ephemerisGeometry(id)` is the other half, and is `overlayGeometry` for
satellites: the drawn points and arcs as typed arrays viewing the module's own
memory. The same rule applies and applies harder, because these buffers are
rebuilt every frame — read them synchronously and `.slice()` anything worth
keeping.

### Things to get right

- **Elements go stale.** SGP4 is a fit around its epoch, not a model of the
  solar system: it reproduces the catalogue's own state vector to a kilometre or
  so near the epoch and drifts from there, roughly a kilometre a day in low
  Earth orbit. A layer reports `oldestElementsDays` so an interface can say how
  much to trust the dots.
- **A propagator can diverge.** A decayed object, or elements months old, will
  run away rather than fail politely. A satellite that cannot be placed is left
  out of the drawing; an arc with a divergence anywhere along it is dropped
  whole, because the samples either side of one are not on any orbit.
- **A catalogue is a bulk product.** A record that will not parse is dropped and
  counted (`rejected`), not a reason to draw nothing.
- **Polite refresh.** The period is floored at five minutes. A catalogue is
  regenerated a few times a day from observations that are themselves hours old,
  and the public endpoints that serve it ask that you do not poll harder.
- **CORS**, as ever. Celestrak answers `Access-Control-Allow-Origin: *`; a
  Space-Track session does not, and needs a proxy.

[OMM]: https://public.ccsds.org/Pubs/502x0b3e1.pdf
[ephemerisjs]: ../terramenta-webapp/src/ephemeris.js

## Both frames at once

`Space` is a choice: the scene is drawn in one frame or the other, and whichever
you pick, the other one is invisible. `G` draws both of them over whatever is
active, so the step between them is something to look at rather than something
to take on trust. That step is the whole of a guidance, navigation and control
frame conversion, and there is not much to it once it is on screen:

- an **inertial triad** in cyan — the vernal equinox, the axis 90° east of it in
  right ascension, and the celestial equator as a hoop in space;
- an **Earth-fixed triad** in amber, with the equator, the prime meridian and a
  lat/lon graticule on the ground;
- the **sidereal angle** between the two, drawn as the arc it is. It is the one
  number that relates the frames, and running the clock is what opens it out;
- and one satellite's path drawn **twice at the same time** — the smooth closed
  ellipse it is against the stars, and the ground track the same object writes
  across the turning Earth. Click a satellite to pick which one.

The pole is drawn once rather than twice, because in this model it is the same
axis in both frames: there is no precession, nutation or polar motion in it.

### Rigid geometry gets a quaternion; a ground track cannot have one

The triads and the graticule never change shape. Each is one mesh, built once in
the coordinates of the frame it belongs to, and carried into world space by a
quaternion on its `Transform` — `inertial_to_world` for one, `earth_to_world`
for the other. Switching frames does not rebuild either: it swaps which of the
two is the identity. That is the entire frame handling for everything that holds
still, and it is about five lines.

The ground track cannot work that way, for the reason the section above gives:
every one of its points belongs to a different moment. Each sample is rotated by
the sidereal angle of *its own* moment on the way into the mesh, and what comes
out is then rigid in the Earth-fixed frame and rides the same quaternion as the
grid under it. Both curves come out of **one** propagation, so they are one set
of states seen from two frames rather than two answers to the same question.

### A position rotates; a velocity rotates and transports

`gnc.rs` carries the rotation in both of the forms flight software does — the
direction cosine matrix `R3(θ)` and the unit quaternion — with tests that pin
them to each other, to the sidereal angle the renderer actually turns the Earth
by, and to the axis relabelling between the canonical GNC basis (`Z` the pole,
`X` the reference direction) and Bevy's Y-up scene. The sign is the part worth
checking: this is a *frame* rotation, so the axes turn east by `θ` and a vector's
components go the other way.

A velocity needs one term more, because the frame it is measured in is itself
turning:

```text
v_ecef = R3(θ) · ( v_eci − ω × r_eci )
```

The readout shows both speeds for whatever is being followed, so the difference
is a number rather than a claim: the ISS is about 7.66 km/s inertial and 7.36
over the ground, and a geostationary satellite is 3.07 and zero.

## Placemarks

Two icons stand on the globe from the moment it starts: the **subsolar** point,
where the sun is directly overhead, and the **sublunar** point, where the moon
is. They are [`placemark.rs`](src/placemark.rs), drawn by
[`icon.wgsl`](assets/shaders/icon.wgsl) and sized in pixels like everything else
over the surface.

Neither position is worked out there. The sun's comes from
[`sun.rs`](src/sun.rs), which the terminator is already drawn from; the moon's
comes from [`moon.rs`](src/moon.rs), which is a low-precision lunar series —
Keplerian elements plus the perturbations that matter, through a right ascension
and declination, against Greenwich's sidereal angle. Good to a few arcminutes,
which is inside the icon. How far apart the two icons are *is* the phase:
together is new, opposite is full.

**An icon is never cut by the ground it stands on.** A flat quad held up to the
camera at a point on a sphere is always partly inside that sphere — the surface
curves away from it, so from anything but a view straight down the globe rises
through the icon and the depth test eats the rest. Standing the icon on its
anchor rather than centring it buys the common case and no more: toward the limb
there is no height that clears the ground. So a placemark ignores the depth
buffer and is drawn whole over the scene, and the one thing that should hide it
— the planet — hides it explicitly, by a horizon test against the camera. All
there, or not there: half an icon reads as a different icon.

**Picking is in the viewport**, for the same reason a satellite's is: the icon
is not where its coordinate is. It stands above the anchor by its own height, so
the anchor is projected into the viewport and the pointer measured against the
rectangle the icon covers there. A pick haloes the icon and goes out with the
state, under the same switch as everything else pickable.

```js
globe.onState(({placemarks}) => {
  placemarks.hovered;  // {body, label, coordinate} or null — body is "sun" | "moon"
  placemarks.pinned;   // the same, for whatever was pinned
});

globe.pinPlacemark("moon");
globe.clearPinnedPlacemark();
```

## Geometry in memory

Everything that arrives as vector data — a GeoJSON document, a Mapbox Vector
Tile — is flattened into one [GeoArrow] store, built with the [`geoarrow`]
crates and held in [`features.rs`](src/features.rs). Not a tree of `Vec`s: three
GeoArrow arrays of points, lines and polygons, a column saying which feature
each shape belongs to, and two columns of per-feature attributes.

```
points     coords                                              owner
           [ lon lat h | lon lat h | ... ]                      [ 0, 0, 3, ... ]

lines      coords                                   offsets     owner
           [ lon lat h | lon lat h | ... ]           [0, 2, 7]   [ 1, 4, ... ]

polygons   coords                    ringOffsets    offsets     owner
           [ lon lat h | ... ]        [0, 4, 7]      [0, 2]      [ 2, ... ]
```

Three things follow from it, and they are the reasons for it.

**The coordinates are `f64`, end to end.** A coordinate goes from the document
into the store, through the hit test and the tessellator, and out to an embedder
without ever being narrowed — [`geo::Position`](src/geo.rs) is `f64` and so is
everything that reads it. The one narrowing left is the last one, where a vertex
is written into a mesh, because a GPU vertex buffer is `f32` and nothing can be
done about that. Eleven significant figures is the difference between a building
and the street it is on, and it is now preserved everywhere except on screen.

**It is one allocation per buffer, not one per ring.** A layer of ten thousand
country outlines is six allocations rather than ten thousand, and walking it is
a linear scan. Picking, which walks every shape of every visible layer on every
frame the cursor is over the globe, reads straight out of those buffers and
copies nothing.

**The buffers cross into JavaScript without being copied.**

```js
const g = globe.overlayGeometry("quakes");
const [lon, lat, height] = g.points.coords.subarray(0, 3);  // f64, exactly as fetched
const feature = g.points.features[0];                        // what pinFeature takes
```

`coords` is a `Float64Array` viewing the module's own linear memory, so an
embedder reads the same bytes the renderer is drawing from. Offsets say where
each shape starts and ends *in coordinates*: line `i` runs `offsets[i]` to
`offsets[i + 1]`; a polygon's `offsets` index into `ringOffsets`, which index
into `coords`, outer ring first and holes after it. Rings are stored open — the
repeated closing position GeoJSON requires is dropped on the way in, so a ring's
last edge is the one back to its first point.

The arrays are windows onto live memory, with the two ways of losing one that
implies: anything that allocates inside the module detaches every view onto it,
and refreshing or removing the layer frees what they point at. Read them
synchronously, and to keep the data — or to post it to a worker — `.slice()`
first, which returns an ordinary array that owns its bytes.

Coordinates are interleaved (`xyzxyzxyz`) rather than separated (`xxx`, `yyy`,
`zzz`), which is the other layout GeoArrow allows and the one its Rust crate
defaults to. Every consumer here walks a shape vertex by vertex reading all
three values at once, so one cache line carrying a whole coordinate beats three
streams carrying a third of one each — and it is the layout that hands out as a
single typed array.

Attributes are two string arrays rather than a parsed tree: the `id` member, and
the `properties` object as the JSON text it arrived as. Nothing in the globe
reads them, so holding them as text keeps a tile of five thousand features to
two allocations and costs one parse when a feature is actually picked.

What the store is *not* is a general geometry library. There is no
`MultiLineString` and no `GeometryCollection`: a globe draws points, lines and
filled rings, and the readers flatten everything else into those three on the
way in — a `MultiPolygon` of forty islands becomes forty polygons that all name
the same feature, which is what makes picking one island highlight the country.

[GeoArrow]: https://geoarrow.org
[`geoarrow`]: https://github.com/geoarrow/geoarrow-rs

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
| `G` | Draw both reference frames at once |
| `R` | Reset the view |
| `P` | Pause the sun |
| `,` `.` | Halve / double the sun's speed |
| `N` | Snap the clock back to now |
| `I` | Drop the terminator and light the whole globe |
| `T` | Toggle streamed imagery |
| `L` / `Shift`+`L` | Next / previous imagery layer |
| `V` / `Shift`+`V` | Toggle streamed vector tiles / next vector tile source |
| `O` / `Shift`+`O` | Toggle satellites / their orbit trails |
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
  gnc.rs       Both frames drawn at once: the DCM, the quaternion and the transport term
  sun.rs       Simulated clock and solar position
  moon.rs      Lunar position, as the point the moon is overhead
  imagery.rs   Layer selection and the `imagery://` asset source
  wms.rs       WMS GetMap request URLs
  wmts.rs      WMTS GetTile request URLs, REST and KVP
  tiles.rs     Tile grids, level-of-detail selection and streaming
  mvt.rs       Mapbox Vector Tiles: the protobuf, the Web Mercator grid, clipping
  vector_tiles.rs  Vector tile layers: the `mvt://` source, streaming and meshing
  features.rs  The GeoArrow store every vector coordinate lives in
  fetch.rs     The slot-addressed asset source a layer's document is fetched over
  geojson.rs   The GeoJSON document format, flattened to drawable geometry
  simplestyle.rs  simplestyle-spec 1.1.0: how a document says it wants to look
  omm.rs       OMM catalogues, read into SGP4 propagators
  tessellate.rs  Rings to triangles: ear clipping, holes and the antimeridian
  picking.rs   Which feature is under the cursor
  overlays.rs  Overlay layers: sources, refresh, meshes and the `geojson://` source
  ephemeris.rs Satellite layers: the `omm://` source, propagation, markers and arcs
  placemark.rs Icons pinned to a coordinate — the subsolar and sublunar points
  hud.rs       The built-in readout, formatted from the state snapshot
assets/shaders/
  globe.wgsl        Day/night, city lights, ocean specular, clouds, limb haze
  atmosphere.wgsl   Additive scattering shell
  starfield.wgsl    Procedural stars and galactic band
  tile.wgsl         A single streamed imagery tile
  vector.wgsl       Overlay markers, lines and fills, sized in pixels
  icon.wgsl         A placemark's icon, sized in pixels
assets/icons/
  sun32.png         The subsolar placemark
  moon32.png        The sublunar placemark
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
