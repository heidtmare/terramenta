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

Everything, which is the point. The panel is in four tabs: **Imagery** for the
basemap under everything, **Data layers** for what is drawn over it, **Other**
for how the scene is viewed rather than what is in it, and **Mission** for
planning and flying an interplanetary trip. Only the open pane is shown — every
control in the panel stays bound and keeps taking its value from each snapshot,
so nothing has to catch up when a tab is opened.

| Tab | Section | |
| --- | --- | --- |
| **Imagery** | **Imagery** | All eight presets, grouped by protocol, with the tile size, format and pyramid depth of the active one; previous/next; streaming on or off |
| **Data layers** | **Vector tiles** | Both keyless MVT sources, the source layers and pyramid depth of the active one, its colour, whether rings are filled, and streaming on or off |
|  | **GeoJSON overlays** | A layer from a URL or a local file, two bundled samples and three live feeds to try, auto-refresh with a period, picking on or off, and per layer: visibility, colour, clamp to surface, extrude to ground, refresh now, remove — plus what each one holds and how stale it is. A document that styles its own features the simplestyle way is drawn that way, and a toggle appears to take the layer back |
|  | **Satellites** | An OMM catalogue from a URL or a local file, five Celestrak groups to try, and per layer: visibility, colour, orbit trails on or off, how far ahead and behind they run, refetching, remove — plus a filterable list of every object in it, each with its own switch for being drawn and for being trailed |
| **Other** | **Sun & clock** | Run or pause, the rate from real time to a day a second, jump to now or forward by hours or days, and whether the night side is shaded at all |
|  | **Reference frame** | ECEF or ECI |
|  | **Both frames at once** | The inertial triad, the Earth-fixed triad, the graticule and its spacing, the arc measuring the sidereal angle between the two, and one satellite's orbit and ground track drawn together over a window you set — plus the rotation as a quaternion, and the followed object's speed measured in each frame |
|  | **Camera** | Altitude, latitude and longitude to fly to, nine places to try, and reading the current view back into the boxes |
|  | **Globe chrome** | The globe's own readout, its key list, and its key bindings — each switchable |
| **Mission** | **Porkchop plot** | Total delta-v as a contoured field over departure date against arrival date, for a Lambert transfer between two of `terramenta-solare`'s bodies — the standard tool a launch window is actually chosen with |
|  | **Launch** | Sends the pair just plotted on a real search-then-depart mission — `terramenta-globe` scores its own grid for the cheapest window, waits for the clock to reach it, and departs, drawn as a small glowing marker in the heliocentric view. Per mission: status, dates, delta-v, and remove |

Alongside them is a telemetry panel showing every field the globe reports:
cursor and camera coordinates, altitude, frame, the sidereal angle and the
ECI→ECEF quaternion, subsolar point, clock, rate, layer, overlays, satellites,
and what both tile streamers are doing — and under
it, whatever the cursor is over: a feature with its properties, a satellite with
its elements, or the sun's or moon's placemark with where that body is in the
sky from the middle of the view.

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

[`data/simplestyle-examples.geojson`](data/simplestyle-examples.geojson) is the third
sample, and the only one that has an opinion about how it looks. A GeoJSON
document may style its own features in the members of
[simplestyle-spec 1.1.0][simplestyle] — `marker-size`, `marker-color`, `stroke`,
`stroke-width`, `fill`, `fill-opacity` and the rest — and this one is a board of
examples laid out over empty ocean in the South Atlantic, each labelled with
the member it is demonstrating: three marker sizes, a line given a colour and a
width, a line given nothing but an opacity, three rings filled three different
ways, and a line whose styling is deliberately misspelt to show that a member
that cannot be read is dropped on its own rather than taken as an error.

Scattered through it are controls that say nothing about themselves. **Change
the layer's colour from the panel** and only those move: a member overrides
exactly the thing it names, so what the document did not ask about is still the
interface's to choose. The row's **use the file's own colours** switch — which
only appears for a document that styles something — takes the whole layer back.

Two of the members the globe reads it cannot draw. `title` and `description`
need a label engine, and `marker-symbol` needs an icon atlas; there is neither
here. They go out with the feature instead, which is why clicking anything on
this layer titles the readout with the name the *document* chose rather than the
layer's.

[simplestyle]: https://github.com/mapbox/simplestyle-spec/tree/master/1.1.0

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

Satellites are picked the same way and reported separately — `pinSatellite` by
catalogue number rather than by row, so a pin holds when the layer refetches —
and so are the two placemarks, by `pinPlacemark("sun")` or `"moon"`, since there
are only ever those two and the name is the whole of the address. Which leaves
this file with a precedence to settle, because there is one panel and three
kinds of pick. Pinned beats hovered, and between three of the same rank they
rank by how hard they are to hit: the satellite first, since it is the smallest
target and the one drawn in front, then the placemark, then the feature under
both.

A placemark has no record behind it — it is one coordinate, recomputed every
frame from the globe's own clock — so what the panel shows for one is worked out
here, from that coordinate and the snapshot around it. How high the body is
above the horizon at the view centre is ninety degrees less the angle to its
sub-point; for the sun, what a sundial at the view centre would read is the
difference from the subsolar meridian, fifteen degrees to the hour; and for the
moon, how far its point is from the sun's is the elongation, which *is* the
phase — together is new, opposite is full, and `(1 - cos elongation) / 2` is how
much of it is lit. The globe draws both icons and says none of that, because it
has nothing to say it in.

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
assets/
  terramenta.png    The banner on the loading screen
data/               Documents served beside the page
  geometry-tour.geojson   One feature per GeoJSON geometry, up at boot
  extruded-cube.geojson   A ring at a height, walled to the ground
  simplestyle-examples.geojson  A document that styles itself, member by member
styles/app.css      The whole look
src/
  main.js           Boot: build the interface, start the globe, join the two
  globe.js          The only file that talks to the wasm module
  panel.js          The controls, and the one-way sync rule
  overlays.js       The GeoJSON layer controls, and the local-file timer
  ephemeris.js      The satellite controls, and the pulled object list
  feature.js        The picked feature, satellite or placemark, and what a click means
  readout.js        The telemetry overlay
  widgets.js        Buttons, toggles, choices and sliders
  format.js         Coordinates, altitudes, clock rates, durations, log sliders
  places.js         Somewhere to fly to
  feeds.js          Something to overlay
  orbits.js         Something to propagate
  dom.js            The little bit of element building the rest does over and over,
                    and the panel's tab strip
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
   reads back. A new section goes into one of the three tabs at the foot of the
   file; `bind` is what keeps it in step, not which pane it lands in.

If it is something to display rather than to set, only the state struct and
[`readout.js`](src/readout.js) are involved.
