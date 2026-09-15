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
- **A live readout.** Latitude and longitude under the cursor, camera altitude in
  kilometres, and the subsolar point.

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
| `H` | Hide the control legend |

## Layout

```
src/
  main.rs      App and plugin wiring
  geo.rs       Lat/lon conventions, ray-sphere math, the sphere mesh builder
  globe.rs     Surface, atmosphere and starfield materials and entities
  camera.rs    Altitude-scaled orbit controller (mouse, keys, touch, gestures)
  sun.rs       Simulated clock and solar position
  hud.rs       On-screen readout
assets/shaders/
  globe.wgsl        Day/night, city lights, ocean specular, clouds, limb haze
  atmosphere.wgsl   Additive scattering shell
  starfield.wgsl    Procedural stars and galactic band
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

## Where to take it next

- Swap in higher-resolution Blue Marble tiles (the fetch script pulls the 2048px
  set; NASA publishes up to 21600px) or a proper tile pyramid with LOD.
- Add a bathymetry/elevation map for a normal-mapped surface and real terrain
  relief.
- Place markers, great-circle arcs or GeoJSON overlays — `geo::LatLon` already
  converts both ways.
- Click-to-fly: `geo::ray_sphere_intersection` gives you the target; animating
  `OrbitCamera`'s yaw/pitch/distance targets does the rest.

## Credits

Earth imagery: [NASA Visible Earth](https://visibleearth.nasa.gov) Blue Marble
(public domain). Everything else is MIT OR Apache-2.0.
