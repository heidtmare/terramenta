# terramenta-webapp

The reference implementation: a web app that embeds
[`terramenta-globe`](../terramenta-globe/) and puts every feature the globe has
behind a control.

It exists to be read as much as used. The globe's control surface is documented
in its own README, but a description of an API is not the same as a worked
example of living with one — so this app deliberately uses all of it, including
the parts an embedder would normally skip, and the code is arranged so that each
concern is in one file.

## Running it

```sh
./scripts/serve.sh          # builds the globe if needed, then http://localhost:8080
./scripts/serve.sh 3000     # somewhere else
./scripts/build.sh --release
```

`build.sh` is the only build step, and it builds the globe, not the app: the app
is HTML, CSS and ES modules served as they are. There is no npm, no bundler and
nothing to install beyond the Rust toolchain the globe needs.

WebGPU needs a secure context, so it has to be served — opening `index.html`
from the filesystem will not work.

## What it controls

Everything, which is the point.

| Section | |
| --- | --- |
| **Imagery** | All eight presets, grouped by protocol, with the tile size, format and pyramid depth of the active one; previous/next; streaming on or off |
| **Vector tiles** | Both keyless MVT sources, the source layers and pyramid depth of the active one, its colour, whether rings are filled, and streaming on or off |
| **GeoJSON overlays** | A layer from a URL or a local file, two bundled samples and three live feeds to try, auto-refresh with a period, picking on or off, and per layer: visibility, colour, clamp to surface, extrude to ground, refresh now, remove — plus what each one holds and how stale it is |
| **Satellites** | An OMM catalogue from a URL or a local file, five Celestrak groups to try, and per layer: visibility, colour, orbit trails on or off, how far ahead and behind they run, refetching, remove — plus a filterable list of every object in it, each with its own switch for being drawn and for being trailed |
| **Sun & clock** | Run or pause, the rate from real time to a day a second, jump to now or forward by hours or days, and whether the night side is shaded at all |
| **Reference frame** | ECEF or ECI |
| **Camera** | Altitude, latitude and longitude to fly to, nine places to try, and reading the current view back into the boxes |
| **Globe chrome** | The globe's own readout, its key list, and its key bindings — each switchable |

Alongside them is a telemetry panel showing every field the globe reports:
cursor and camera coordinates, altitude, frame, subsolar point, clock, rate,
layer, overlays, satellites, and what both tile streamers are doing — and under
it, whatever feature the cursor is over, with its properties.

The app starts with three overlays already up, because a layer control with
nothing in it is a poor way to introduce the feature, and because no one
document shows everything a layer does.

[`data/geometry-tour.geojson`](data/geometry-tour.geojson) is the shape of the
thing: one feature per GeoJSON geometry type, labelled with what it is, so every
kind of shape the globe can draw is on screen at once. It also holds the cases
worth seeing drawn — a ring with a hole in it, a polygon running across the
antimeridian, a launch profile climbing to 420 km, and a stepped terminal
control area over Denver, which is five rings each flat at its own floor,
stacked into a volume. Click anything on it and the properties panel says which
geometry it came from. It is served from beside the page and fetched as an
ordinary URL, so it goes through the same path a remote layer does.

The airspace is the one that needs looking for. Its floors span under four
kilometres from the surface to the 12,000 ft ceiling, which is nothing at the
scale of a planet: fly down to Denver, drop to a low pass, and **ctrl + drag**
to tilt — the shelves only separate once the camera is low and looking across
them rather than down at them.

[`data/extruded-cube.geojson`](data/extruded-cube.geojson) is the second sample
and the shortest document here: one square ring at 222 km over the equator, on a
layer with **extrude** switched on, which walls every edge of it down to the
surface. What is drawn is a box 222 km on a side standing on the ground — the
lid is the ring, the sides are the walls. It is its own layer because extrude is
a per-layer setting and the tour must not have it: the airspace next door is
meant to float, and walling its shelves to the ground would bury the shape they
make.

Both switches are on every layer row, so the tour can be extruded and the cube
flattened to see what each one is doing.

The second is live: the USGS feed of [earthquakes in the past hour][usgs],
refetched every minute, which is the half a static document cannot show. The
feeds in [`feeds.js`](src/feeds.js) are all USGS, chosen as much because they
send `Access-Control-Allow-Origin: *` as because they are interesting: without
that header the browser will not let the globe fetch them at all.

Those feeds also put earthquake *depth* in the third element of a position,
where GeoJSON nominally puts height. That is what the per-layer **clamp to
surface** switch is for: it ignores heights and drapes the layer flat.

[usgs]: https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson

It also starts with one satellite layer up: Celestrak's `stations` group, which
is twenty-odd objects including the two places anyone is currently living. It is
the one layer that moves while you watch it without anything being refetched at
all, because its geometry is computed from the globe's own clock rather than
fetched — so pausing the sun stops the satellites, and running a day every four
minutes sweeps them round.

Press `Space` with it up. In ECEF each orbit is a corkscrew, because the Earth
turns underneath the satellite while it goes round; in ECI the same arc is the
closed ellipse it really is. That difference is the clearest thing on the globe,
and it is why the frame toggle and the satellites arrived together.

The other groups in [`orbits.js`](src/orbits.js) are chosen for their orbits
rather than for their contents: GPS in six planes at half a sidereal day, the
geostationary ring — which in ECEF does not appear to move at all, that being
the meaning of the word — the Molniya ellipses that loiter over the north for
most of a half-day, and a full Starlink shell, which is thousands of objects and
so is the one that shows what the globe's budgets do when a catalogue is larger
than it will draw.

## How it is wired

**The data flow only goes one way.** A control sends a command and then forgets
about it — it never sets its own value from a click. The globe applies the
command, the next state snapshot comes back, and every control is set from it.

That one rule buys most of the app's behaviour. The panel cannot drift out of
step with the globe, because it is never the source of truth for anything. The
globe's own keyboard shortcuts keep working with the panel open, and the panel
follows them: press `P` and the pause control changes, because both are drawn
from the same snapshot. And switching the globe's built-in readout back on is
the quickest way to confirm the two agree, which is why that toggle is there.

There are two exceptions, and both are the same exception. A slider being
dragged reports that it is held and is left alone until it is let go — a value
overwritten mid-drag fights the pointer. A text field is left alone while it has
focus, because writing to one moves the caret to the end, which makes editing a
URL or a refresh period impossible.

[`feature.js`](src/feature.js) is where the rule is most visible, and where the
app earns its keep. The globe hit-tests the geometry and says what is under the
pointer; it has no idea what a click is. So this app watches the canvas for a
press and release that did not move — an orbit drag ends in a release too — and
turns that into `pinFeature`, or into `clearPinnedFeature` over empty ocean. The
globe then highlights the pinned feature in preference to the hovered one, and
the panel shows the same, so the two cannot disagree about which is selected.

`overlays.js` keeps a little state of its own on top of that, which no other
control does. The globe refreshes a URL by refetching it; a local file it was
handed once, it cannot fetch again. So for a file the app holds the `File`,
re-reads it on the period the user asked for, and sends the text again under the
same layer id — which the globe treats as a replacement rather than a second
layer.

[`ephemeris.js`](src/ephemeris.js) is the one place the app *asks* the globe for
something instead of waiting to be told. A satellite catalogue can be eight
thousand objects, and putting that list into a snapshot that goes out ten times
a second would cost more than drawing the satellites does — so the snapshot
carries a `revision` per layer instead, and the app calls `ephemerisObjects(id)`
only when that number moves. Even then the list is windowed to two hundred rows
with a filter box over it, because eight thousand checkboxes is eight thousand
DOM nodes built so that a dozen can be looked at. The rule still holds: nothing
here is the source of truth, the pull is just how the truth gets across.

```
index.html          Canvas, overlay, loading state
data/               Documents served beside the page
  geometry-tour.geojson   One feature per GeoJSON geometry, up at boot
  extruded-cube.geojson   A ring at a height, walled to the ground
styles/app.css      The whole look
src/
  main.js           Boot: build the interface, start the globe, join the two
  globe.js          The only file that talks to the wasm module
  panel.js          The controls, and the one-way sync rule
  overlays.js       The GeoJSON layer controls, and the local-file timer
  ephemeris.js      The satellite controls, and the pulled object list
  feature.js        The picked feature's properties, and what a click means
  readout.js        The telemetry overlay
  widgets.js        Buttons, toggles, choices and sliders
  format.js         Coordinates, altitudes, clock rates, durations, log sliders
  places.js         Somewhere to fly to
  feeds.js          Something to overlay
  orbits.js         Something to propagate
  dom.js            The little bit of element building the rest does over and over
globe/              Build output: the module and its assets (git-ignored)
scripts/
  build.sh          Builds the globe module into globe/
  serve.sh          Serves the app
```

### The boot order

`main.js` is worth reading first, because the order is the part to copy.

The module is loaded, but the globe is not started. `layers()` and `limits()`
answer from compiled-in tables, so the entire panel — the layer list, the slider
ranges — is built from the globe's own catalogue before the renderer has drawn
anything. Commands queue until there is a globe to apply them to, so
`setHudVisible(false)` can be sent to a globe that does not exist yet. Then the
globe starts, the state stream begins, and the interface comes to life.

### Adding a control

1. Add the command to `GlobeCommand` and apply it in
   [`api.rs`](../terramenta-globe/src/api.rs).
2. Bind it in [`wasm.rs`](../terramenta-globe/src/wasm.rs).
3. Re-export it from [`globe.js`](src/globe.js) with a line saying what it does.
4. Add the control in [`panel.js`](src/panel.js) — building it out of
   [`widgets.js`](src/widgets.js) — and `bind` it to the field of the snapshot it
   reads back.

If it is something to display rather than to set, only the state struct and
[`readout.js`](src/readout.js) are involved.
