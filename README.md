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
  sweeps continents from far out and nudges streets from low orbit. Mouse,
  keyboard, trackpad pinch and multi-touch are all wired up.
- **Streaming OGC imagery.** A quadtree of tiles fetched from any OGC Web Map
  Service (WMS) or Web Map Tile Service (WMTS), refined as you descend, with
  NASA GIBS layers wired up over both by default.
- **GeoJSON overlays.** Vector data over the imagery, from a URL or a local
  file, as screen-sized markers, lines and filled rings — several layers at
  once, each with its own colours and its own refresh period. The reference app
  starts with the USGS feed of earthquakes in the past hour, refetched every
  minute.
- **Pickable features.** The cursor hit-tests the overlay geometry: what it is
  over is haloed on the globe and its properties are listed in the app, and a
  click keeps one selected.
- **A live readout.** Latitude and longitude under the cursor, camera altitude in
  kilometres, the subsolar point, and what the tile streamer is doing.
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
- Support a Web Mercator tile matrix set, which would need the quadtree walk to
  reproject rather than read a tile's bounds straight off as lat/lon.
- Add a bathymetry/elevation map for a normal-mapped surface and real terrain
  relief.
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
GIBS](https://nasa-gibs.github.io/gibs-api-docs/). Everything else is MIT OR
Apache-2.0.
