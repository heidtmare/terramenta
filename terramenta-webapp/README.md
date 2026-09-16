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
| **GeoJSON overlays** | A layer from a URL or a local file, a bundled sample and three live feeds to try, auto-refresh with a period, picking on or off, and per layer: visibility, colour, clamp to surface, refresh now, remove — plus what each one holds and how stale it is |
| **Sun & clock** | Run or pause, the rate from real time to a day a second, jump to now or forward by hours or days, and whether the night side is shaded at all |
| **Reference frame** | ECEF or ECI |
| **Camera** | Altitude, latitude and longitude to fly to, nine places to try, and reading the current view back into the boxes |
| **Globe chrome** | The globe's own readout, its key list, and its key bindings — each switchable |

Alongside them is a telemetry panel showing every field the globe reports:
cursor and camera coordinates, altitude, frame, subsolar point, clock, rate,
layer, overlays, and what the tile streamer is doing — and under it, whatever
feature the cursor is over, with its properties.

The app starts with two overlays already up, because a layer control with
nothing in it is a poor way to introduce the feature, and because the two halves
of what a layer does are not visible in the same document.

[`data/geometry-tour.geojson`](data/geometry-tour.geojson) is the shape of the
thing: one feature per GeoJSON geometry type, labelled with what it is, so every
kind of shape the globe can draw is on screen at once. It also holds the three
cases worth seeing drawn — a ring with a hole in it, a polygon running across
the antimeridian, and a launch profile climbing to 420 km, which is what the
overlay's height handling looks like when it is being used. Click anything on it
and the properties panel says which geometry it came from. It is served from
beside the page and fetched as an ordinary URL, so it goes through the same path
a remote layer does.

The second is live: the USGS feed of [earthquakes in the past hour][usgs],
refetched every minute, which is the half a static document cannot show. The
feeds in [`feeds.js`](src/feeds.js) are all USGS, chosen as much because they
send `Access-Control-Allow-Origin: *` as because they are interesting: without
that header the browser will not let the globe fetch them at all.

Those feeds also put earthquake *depth* in the third element of a position,
where GeoJSON nominally puts height. That is what the per-layer **clamp to
surface** switch is for: it ignores heights and drapes the layer flat.

[usgs]: https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/all_hour.geojson

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

```
index.html          Canvas, overlay, loading state
data/               Documents served beside the page
  geometry-tour.geojson   One feature per GeoJSON geometry, up at boot
styles/app.css      The whole look
src/
  main.js           Boot: build the interface, start the globe, join the two
  globe.js          The only file that talks to the wasm module
  panel.js          The controls, and the one-way sync rule
  overlays.js       The GeoJSON layer controls, and the local-file timer
  feature.js        The picked feature's properties, and what a click means
  readout.js        The telemetry overlay
  widgets.js        Buttons, toggles, choices and sliders
  format.js         Coordinates, altitudes, clock rates, durations, log sliders
  places.js         Somewhere to fly to
  feeds.js          Something to overlay
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
