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
- **Streaming OGC imagery.** A quadtree of tiles fetched from any OGC Web Map
  Service (WMS) or Web Map Tile Service (WMTS), refined as you descend, with
  NASA GIBS layers wired up over both by default.
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
  main.rs      App and plugin wiring
  geo.rs       Lat/lon conventions, ray-sphere math, the sphere mesh builder
  globe.rs     Surface, atmosphere and starfield materials and entities
  camera.rs    Altitude-scaled orbit controller (mouse, keys, touch, gestures)
  sun.rs       Simulated clock and solar position
  imagery.rs   Layer selection and the `imagery://` asset source
  wms.rs       WMS GetMap request URLs
  wmts.rs      WMTS GetTile request URLs, REST and KVP
  tiles.rs     Tile grids, level-of-detail selection and streaming
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
Service](https://www.ogc.org/standards/wmts/). Press `T` to toggle it and `L` to
cycle layers; the readout shows the protocol and layer in use, the deepest level
on screen, and how many tiles are drawn or in flight.

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

Layers live in `imagery_layers()` in [`main.rs`](src/main.rs). For WMTS, the
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

## Where to take it next

- Read `GetCapabilities` / `WMTSCapabilities.xml` to discover a server's layers,
  tile matrix sets and legal zoom range instead of transcribing them by hand.
- Cache tiles to disk or IndexedDB so a revisit does not refetch the pyramid.
- Support a Web Mercator tile matrix set, which would need the quadtree walk to
  reproject rather than read a tile's bounds straight off as lat/lon.
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
