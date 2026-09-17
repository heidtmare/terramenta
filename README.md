# Terramenta

This is a rebirth of our original JavaFX + NetBean Terramenta GIS platform from https://bitbucket.org/teamninjaneer/terramenta/  
(which has sadly now been pruned by bitbucket, but an old fork can be found here: https://github.com/emxsys/emxsys-terramenta)  
It is a spiritual successor and will not have feature parity. The entire stack has changed, but the lessons live on.

A navigable 3D globe of Earth, written in Rust with [Bevy](https://bevy.org),
rendered through WebGPU, and shipped to the browser as WebAssembly — with a web
app around it that drives every part of it from JavaScript.

## The two halves

| | |
| --- | --- |
| **[`terramenta-globe`](terramenta-globe/)** | The renderer. Draws the Earth, streams OGC imagery onto it, and exposes all of it through a control surface. Compiles to a WebAssembly module, and to a native binary that runs the same code on Metal, Vulkan or DX12. |
| **[`terramenta-webapp`](terramenta-webapp/)** | The reference implementation. Embeds that module and puts every feature it has behind a control, so there is a worked example of driving the globe rather than only a description of it. |

The split is the point. The globe knows nothing about the app around it: it
takes commands, reports state, and draws. Anything that wants a globe —
this app, a dashboard, an analysis tool — embeds the module and builds its own
interface, and the reference app is there to show what that takes.

## What it does

- **A shaded Earth.** NASA Blue Marble imagery on a unit sphere, lit by a
  simulated sun: a soft day/night terminator, city lights on the dark side, a
  specular glint off the oceans and a drifting cloud deck.
- **An atmosphere.** An additive shell that thickens toward the limb, fading into
  a twilight arc past the terminator.
- **A sky.** Stars generated procedurally from a hashed 3D cell grid, so there is
  no pole pinching and no texture to download.
- **A real clock.** The globe starts at the current UTC time and runs a full day
  every four minutes; the sun position comes from a low-precision solar model
  (declination plus hour angle), accurate to about a degree.
- **Two reference frames.** ECEF, where the ground stands still, and ECI, where
  the stars do and the Earth turns underneath at the sidereal rate.
- **Altitude-aware navigation.** Drag sensitivity scales with height, so a drag
  sweeps continents from far out and nudges streets from low orbit. Ctrl + drag
  swings the camera around whatever it is looking at — compass heading and a
  tilt from straight down to a grazing view along the horizon — without that
  place leaving the centre of the screen, and shift + drag turns the camera
  where it stands to look off it. Mouse, keyboard, trackpad pinch and
  multi-touch are all wired up.
- **Streaming OGC imagery.** A quadtree of tiles fetched from any OGC Web Map
  Service (WMS) or Web Map Tile Service (WMTS), refined as you descend, with
  NASA GIBS layers wired up over both by default.
- **Streaming vector tiles.** Mapbox Vector Tiles (MVT) from any `{z}/{x}/{y}`
  service, decoded with [`geozero`](https://github.com/georust/geozero),
  unprojected out of Web Mercator and drawn on the globe as screen-sized lines,
  rings and markers — the same quadtree walk the imagery uses, over a grid of
  its own, because the two projections do not agree on where a tile is. Two
  keyless sources are wired up: MapLibre's demo country boundaries and
  OpenStreetMap's own Shortbread tiles.
- **GeoJSON overlays.** Vector data over the imagery, from a URL or a local
  file, as screen-sized markers, lines and filled rings — several layers at
  once, each with its own colours and its own refresh period. The reference app
  starts with three: a bundled sample holding one feature per GeoJSON geometry
  type — including a stepped airspace stacked out of rings at different heights
  — a second holding a cube, and the USGS feed of earthquakes in the past hour,
  refetched every minute.
- **Placement at altitude.** A position's optional third element is drawn:
  markers stand off the surface, and a line climbs evenly between the heights of
  its corners, so a flight path or a balloon track is where it says it is. Per
  layer, because feeds disagree about what that element means — a scale says
  how many metres one unit of it is, and clamping to the surface ignores it
  entirely, which is what the USGS feeds want since theirs is a depth.
- **Extrusion.** A layer can wall its rings down to the ground, so a footprint
  at a height is drawn as a solid standing on the surface rather than a lid
  hanging over it.
- **Pickable features.** The cursor hit-tests the overlay geometry: what it is
  over is haloed on the globe and its properties are listed in the app, and a
  click keeps one selected.
- **A live readout.** Latitude and longitude under the cursor, camera altitude in
  kilometres, the subsolar point, and what the tile streamer is doing.
- **GeoArrow all the way down.** Every vector coordinate — from a GeoJSON feed
  or from a vector tile — lives in [GeoArrow](https://geoarrow.org) arrays built
  with the [`geoarrow`](https://github.com/geoarrow/geoarrow-rs) crates: flat,
  contiguous `f64` buffers rather than a tree of `Vec`s. Nothing is narrowed
  until a vertex reaches the GPU, so the full precision of the source survives
  the hit test and the round trip back out — and because the buffers are
  contiguous, `overlayGeometry(id)` hands JavaScript a `Float64Array` viewing
  the module's own memory rather than a copy, which a web worker can be given
  the same way.
- **All of it under external control.** Layer, imagery, sun, clock, frame,
  camera and even the globe's own overlay and key bindings are reachable from
  JavaScript, and the globe streams its state back the other way.

## Running it

### The web app

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128   # must match the crate version
cd terramenta-webapp
./scripts/serve.sh                                 # builds if needed, then http://localhost:8080
```

WebGPU needs a secure context, so serve it over HTTP(S) — opening `index.html`
from the filesystem will not work. Supported by Chrome/Edge 113+, Safari 26+ and
Firefox 141+; on Linux, Firefox still needs `dom.webgpu.enabled`.

### The globe on its own, natively

```sh
./terramenta-globe/scripts/fetch-assets.sh
cargo run --release --bin terramenta-globe
```

The native build has no web app around it; it is the globe with its own overlay
and key bindings, which is the quickest way to work on the renderer itself.

## Layout

```
Cargo.toml              Workspace: profiles and members
terramenta-globe/       The renderer and its control surface  → its README
terramenta-webapp/      The reference web app                 → its README
```

`terramenta-webapp` is not a Cargo member — it is HTML, CSS and ES modules with
no build step at all. The only thing it builds is the globe.

## Where to take it next

- Read `GetCapabilities` / `WMTSCapabilities.xml` to discover a server's layers,
  tile matrix sets and legal zoom range instead of transcribing them by hand.
- Cache tiles to disk or IndexedDB so a revisit does not refetch the pyramid.
- Style a vector tile per source layer rather than per tile — one colour for
  water, another for the road network. Every feature already carries the layer
  it came from as a `sourceLayer` property; nothing reads it to decide a colour.
- Unwrap gzipped vector tiles. A browser does it before the bytes reach the
  globe, so the web build takes any service; a native one answering
  `Content-Encoding: gzip` regardless is reported as such rather than drawn.
- Pick vector tile features the way overlay features are picked, which needs an
  index that can be built per tile rather than per document.
- Add a bathymetry/elevation map for a normal-mapped surface and real terrain
  relief.
- Pick against the drawn geometry rather than the ground under it, so a marker
  at altitude is grabbable where it appears once the camera is tilted.
- Style a GeoJSON overlay per feature rather than per layer — colour the
  earthquake markers by magnitude, which is the obvious next thing to want from
  the feed the reference app ships with. The properties are already carried
  through to the picking layer; nothing reads them to decide an appearance.
- Fly to a picked feature, or frame it: the globe knows where every shape is,
  and `lookAt` is already there.
- Click-to-fly: the cursor coordinate is already in the state stream, so the app
  could turn a click into a `lookAt` without the globe knowing.

## Credits

Base Earth imagery: [NASA Visible Earth](https://visibleearth.nasa.gov) Blue
Marble (public domain). Streamed layers: [NASA
GIBS](https://nasa-gibs.github.io/gibs-api-docs/). Vector tiles:
[MapLibre demo tiles](https://demotiles.maplibre.org) and
[OpenStreetMap](https://www.openstreetmap.org/copyright) (ODbL). Everything else
is MIT OR Apache-2.0.
