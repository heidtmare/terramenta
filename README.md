# Terramenta

This is a rebirth of our original JavaFX + NetBean Terramenta GIS platform from https://bitbucket.org/teamninjaneer/terramenta/  
(which has sadly now been pruned by bitbucket, but an old fork can be found here: https://github.com/emxsys/emxsys-terramenta)  
It is a spiritual successor and will not have feature parity. The entire stack has changed, but the lessons live on.

A navigable 3D globe of Earth, written in Rust with [Bevy](https://bevy.org),
rendered through WebGPU, and shipped to the browser as WebAssembly. The same
binary runs natively on the desktop (Metal, Vulkan or DX12) with no code changes.

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
- **Altitude-aware navigation.** Drag sensitivity scales with height, so a drag
  sweeps continents from far out and nudges streets from low orbit. Mouse,
  keyboard, trackpad pinch and multi-touch are all wired up.
- **Streaming WMS imagery.** A quadtree of tiles fetched from any OGC Web Map
  Service, refined as you descend, with NASA GIBS layers wired up by default.
- **A live readout.** Latitude and longitude under the cursor, camera altitude in
  kilometres, the subsolar point, and what the tile streamer is doing.

## Running it

### First, fetch the imagery

The Earth textures are public-domain NASA imagery, downloaded rather than
committed:

```sh
./scripts/fetch-assets.sh
```

### In the browser

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128   # must match the crate version
./scripts/build-web.sh --release
./scripts/serve.sh                                 # http://localhost:8080
```

`build-web.sh` compiles to `wasm32-unknown-unknown`, runs `wasm-bindgen`, and
assembles `web/dist` with the HTML shell and the assets folder beside it. Drop
that directory on any static host.

WebGPU needs a secure context, so serve it over HTTP(S) — opening `index.html`
from the filesystem will not work. Supported by Chrome/Edge 113+, Safari 26+ and
Firefox 141+; on Linux, Firefox still needs `dom.webgpu.enabled`.

### Natively

```sh
cargo run --release
```

## Controls

| Input | Action |
| --- | --- |
| Drag / one-finger drag | Orbit |
| Scroll, pinch, two-finger pinch | Zoom |
| `W` `A` `S` `D` or arrows | Orbit |
| `+` `-` | Zoom |
| `Space` | Toggle auto-rotation |
| `R` | Reset the view |
| `P` | Pause the sun |
| `,` `.` | Halve / double the sun's speed |
| `N` | Snap the clock back to now |
| `T` | Toggle WMS imagery |
| `L` | Next imagery layer |
| `H` | Hide the control legend |

## Layout

```
src/
  main.rs      App and plugin wiring
  geo.rs       Lat/lon conventions, ray-sphere math, the sphere mesh builder
  globe.rs     Surface, atmosphere and starfield materials and entities
  camera.rs    Altitude-scaled orbit controller (mouse, keys, touch, gestures)
  sun.rs       Simulated clock and solar position
  wms.rs       WMS GetMap requests and the `wms://` asset source
  tiles.rs     Tile quadtree, level-of-detail selection and streaming
  hud.rs       On-screen readout
assets/shaders/
  globe.wgsl        Day/night, city lights, ocean specular, clouds, limb haze
  atmosphere.wgsl   Additive scattering shell
  starfield.wgsl    Procedural stars and galactic band
  tile.wgsl         A single streamed imagery tile
web/
  index.html   Canvas shell, WebGPU capability check, loading state
scripts/
  fetch-assets.sh  Downloads the NASA imagery
  build-web.sh     Builds web/dist
  serve.sh         Serves it over HTTP
```

### Conventions worth knowing

The globe is a unit sphere in Bevy's Y-up world space: `+Y` is the north pole,
`+Z` is the prime meridian, `+X` is 90° east. Textures are equirectangular with
`v == 0` at the north pole. Bevy's built-in `Sphere` primitive is Z-up and wraps
the other way, so [`geo::equirectangular_sphere`](src/geo.rs) generates its own
grid instead — that one convention is what makes the coordinate readout, the
texture alignment and the sun position agree.

The surface is lit in [`globe.wgsl`](assets/shaders/globe.wgsl) rather than
through Bevy's PBR pipeline. There is no `DirectionalLight` in the scene at all:
one sun direction uniform drives the terminator, the specular and the city
lights, which keeps the planet to a single draw call and makes the look directly
adjustable.

## Streaming imagery from a WMS

The globe can drape itself in imagery from any [OGC Web Map
Service](https://www.ogc.org/standards/wms/). Press `T` to toggle it and `L` to
cycle layers; the readout shows the current layer, the deepest level on screen,
and how many tiles are drawn or in flight.

### How it works

WMS answers `GetMap` for an arbitrary bounding box, which is too open-ended to
cache. So requests are pinned to the **global geodetic quadtree** that WMS
servers expose through `EPSG:4326`: level 0 is two 180°x180° tiles, and each
level quarters them, giving `2^(n+1) x 2^n` tiles at level `n`. Because that
projection is plate carrée, a tile's bounding box is a plain lat/lon rectangle —
the same mapping the base globe's textures use, so no reprojection is needed.

Each frame the tree is walked from the roots. A tile splits when its projected
size on screen exceeds the resolution of the image behind it, so detail follows
the camera; tiles over the horizon are culled. Tiles that have not arrived yet
fall back to the nearest ancestor that has, and failing that to the base globe
underneath, so the view is never holed. Requests are capped both in flight and
per frame, because one fast zoom will otherwise queue hundreds of tiles that are
stale before they land.

Fetching is handed to Bevy rather than hand-rolled. [`wms.rs`](src/wms.rs)
registers a `wms://` asset source whose reader turns a tile path like
`0/4/9/3.jpg` into a full `GetMap` URL and delegates to Bevy's HTTP reader. A
tile then loads with an ordinary `asset_server.load`, and async I/O, image
decoding, GPU upload and reference counting all come for free — identically on
native and on the web.

### Pointing it at your own server

Layers live in `imagery_layers()` in [`main.rs`](src/main.rs):

```rust
WmsConfig {
    label: "My layer".into(),
    endpoint: "https://example.org/geoserver/wms".into(),
    layers: "workspace:layer".into(),
    version: WmsVersion::V1_3_0,
    format: WmsFormat::Jpeg,
    tile_size: 256,
    max_level: 9,
    ..
}
.with_parameter("TIME", "2026-09-10")
```

Two things to get right:

- **Axis order.** WMS 1.3.0 declares `EPSG:4326` latitude-first and renamed `SRS`
  to `CRS`; 1.1.1 treats the same code as longitude-first. Picking the wrong
  `WmsVersion` yields a world turned on its side rather than an error, so
  `WmsVersion` encodes both differences together.
- **CORS.** The browser build needs `Access-Control-Allow-Origin` from the WMS
  host. NASA GIBS sends `*`; a private GeoServer usually needs configuring.

`max_level` is the ceiling on refinement — level 8 is roughly 150 m per pixel at
the equator. Raise it for a server with deeper imagery, lower it to be kind to
one that does not.

## Where to take it next

- Read `GetCapabilities` to discover a server's layers, supported CRSs and
  legal zoom range instead of configuring them by hand.
- Cache tiles to disk or IndexedDB so a revisit does not refetch the pyramid.
- Add WMTS alongside WMS: the request shape differs, but the quadtree,
  level-of-detail selection and streaming in [`tiles.rs`](src/tiles.rs) are
  already tile-scheme agnostic.
- Add a bathymetry/elevation map for a normal-mapped surface and real terrain
  relief.
- Place markers, great-circle arcs or GeoJSON overlays — `geo::LatLon` already
  converts both ways.
- Click-to-fly: `geo::ray_sphere_intersection` gives you the target; animating
  `OrbitCamera`'s yaw/pitch/distance targets does the rest.

## Credits

Base Earth imagery: [NASA Visible Earth](https://visibleearth.nasa.gov) Blue
Marble (public domain). Streamed layers: [NASA
GIBS](https://nasa-gibs.github.io/gibs-api-docs/). Everything else is MIT OR
Apache-2.0.
