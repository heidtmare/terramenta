# terramenta-webapp

Reference client for [`terramenta-globe`](../terramenta-globe/) and
[`terramenta-charta`](../terramenta-charta/). Plain HTML, CSS and ES modules:
no dependencies, no bundler. One page, `index.html`, which loads a project file.

| URL | Loads |
| --- | --- |
| `index.html` | Empty project |
| `index.html?project=projects/overview.terramenta.json` | Bundled project (any same-origin or CORS URL) |
| `index.html?project=session` | Local file opened from the Project tab or dropped (`sessionStorage`) |

`?project=` tracks the open project, so a reload reopens it.

## Running

```sh
./scripts/serve.sh             # builds the WASM modules if missing, serves http://localhost:8080
./scripts/serve.sh 3000        # alternate port
./scripts/build.sh --release   # rebuild globe/ and charta/ with the web-release profile
```

`build.sh` runs `terramenta-globe/scripts/build-wasm.sh` into `globe/` and
`terramenta-charta/scripts/build-wasm.sh` into `charta/`. The app itself has no
build step. WebGPU requires a secure context, so the pages must be served (not
opened from `file://`).

```sh
npm test                       # node --test (Node 22+); no dependencies
```

Tests ([`test/`](test/)) cover the schema validator, project validation,
migrations, spacecraft/environment application, timeline resolution and
runner, analysis sections, report math (OEM parsing, osculating elements, Δv
budget, CSV, CZML), and capture → parse → apply round trips against a fake globe module (`globe.attach`).

## Project files

`*.terramenta.json`, described by
[`schema/project.schema.json`](schema/project.schema.json) (JSON Schema
2020-12; point `$schema` at it for editor completion). Every project, bundled
or user-supplied, loads the same way: fetch/read → upgrade → validate →
`globe.reset()` → queue onto the globe. The first project is queued before
`start`; later ones switch in place (no reload). Omitted fields keep
fresh-globe defaults.

Validation ([`schema.js`](src/schema.js), [`project.js`](src/project.js)
`parseProject`): structure against the schema, then ids and references
(duplicate ids, `url` xor `text`, `camera.follow`, `gnc.focus.layer`, director
mission, timeline event references and cycles, plot spacecraft, porkchop
bodies and date ranges, ground station ids). Errors list every failing path. Unknown keys warn and are ignored, so
files with keys from newer builds still open.

Versions: older files are upgraded on load (`migrateProject`) and saved at the
current version; newer ones are refused.

| Version | Change |
| --- | --- |
| 1 | Initial |
| 2 | `picking` → `overlays.picking` |
| 3 | `ui.tab` regrouped: `imagery`/`data` → `layers`, `reports`/`mission` → `analysis`, `other` → `view` |

Relative URLs resolve against the app root (the directory holding
`index.html`), not the project file.

| Key | Type | Applies |
| --- | --- | --- |
| `terramenta` | `3` | Format version. Required. |
| `name`, `description` | string | Masthead, Project tab, page title |
| `ui.panel` | `"full"` \| `"flight"` | Full tabbed panel (default) or [flight panel](#flight-panel) |
| `ui.tab` | `"project"` \| `"layers"` \| `"spacecraft"` \| `"timeline"` \| `"analysis"` \| `"view"` | Initial tab (full panel) |
| `ui.readout`, `ui.collapsed` | bool | Telemetry + picked-item panels (default on); panel collapsed |
| `view` | `"globe"` \| `"heliocentric"` | Initial view, no transition |
| `frame` | `"ecef"` \| `"eci"` | `setFrame` |
| `clock` | `{epoch, timeScale, paused, shaded}` | `epoch`: ISO 8601 UTC or `"now"` |
| `camera` | `{lat, lon, altitudeKm, follow \| anchor}` | `lookAt`; `follow`: mission id; `anchor`: `sun`/`earth`/`mars`/`barycenter` |
| `imagery`, `vectorTiles` | `{enabled, layer, style?}` | `layer`: preset label or index |
| `overlays` | `{enabled, picking, layers: [{id, ...addOverlay options}]}` | One of `url`/`text` per layer |
| `ephemerides` | `{enabled, layers: [{id, ...addEphemeris options}]}` | OMM JSON or KVN OEM |
| `environment` | `{eop, spaceWeather, kernel}` | URLs; see [Spacecraft](#spacecraft) |
| `spacecraft` | `[{id, ...addEphemeris display options, uncertaintySigma, propagation}]` | `propagation`: `propagate` options; one of `stop`/`durationS`; `initial` a TLE, OMM, GCRF state or `{epoch, elements}`; optional `covariance`. `uncertaintySigma`: ellipsoid scale, default 3, 0 hides. Id unique across `ephemerides`. Optional `sequence`: see [Mission sequence](#mission-sequence) |
| `missions` | `[{id, ...addMission options}]` | |
| `placemarks.enabled`, `keyboard` | bool | |
| `gnc` | `{enabled, eciAxes, ecefAxes, graticule, graticuleStepDeg, sidereal, track, trackOrbits, focus: {layer, noradId}}` | |
| `hud` | `{visible, help}` | Globe's built-in HUD (off unless set) |
| `director` | `{type, ...}` | Timeline preset; see [Directors](#directors) |
| `timeline` | `{directed, skipTo, events: [{id, label, at, actions}]}` | See [Timeline](#timeline) |
| `analysis` | `{plots: [{id, label, quantity, spacecraft}], porkchops: [{id, label, origin, destination, departure, arrival, retrograde, steps}], coverage: [{id, label, spacecraft, access, minElevationDeg, resolution, bounds, stepS, metric, show}], conjunctions: [{id, label, primary, secondaries, layers, thresholdKm, hardBodyRadiusM, stepS, secondarySigmaKm}], orbitDetermination: [{id, label, spacecraft, estimator, stations, tdm, simulate, solveFor, editSigma, lightTime}]}` | See [Reports](#reports), [Coverage](#coverage), [Conjunctions](#conjunctions), [Orbit determination](#orbit-determination). Overlay id `coverage` is reserved |
| `groundStations` | `[{id, label, latDeg, lonDeg, altKm, minElevationDeg, horizon, color}]` | See [Events](#events). Overlay id `ground-stations` is reserved |
| `events` | `{eclipses, apsides, nodes, stepS}` | See [Events](#events) |
| `targets` | `[{id, label, spacecraft, variables, goals, maxIterations}]` | See [Targeting](#targeting) |
| `attitudes` | `[{spacecraft, label, quaternion \| primary + secondary, spin, station \| target, sensors, stepS, axes}]` | See [Attitude](#attitude) |

**Save** (Project tab) writes the current scene: state-stream values plus
what [`globe.js`](src/globe.js) recorded from `add*` calls (inline text,
mission search options, explicit satellite selections, camera lock). `ui`,
`description` and `director` carry over from the loaded project; the timeline,
analysis, ground stations and event options are saved as edited, minus events whose references no longer hold
and plot, coverage, conjunction and orbit determination spacecraft, layers and stations no longer in the scene. Not saved: per-satellite trail
toggles, local-file overlay refresh timers, unsaved porkchop inputs.

### Workspace

[`workspace.js`](src/workspace.js) owns the open project.

| | |
| --- | --- |
| Open | Project tab (file, URL, recent, examples) or drop a file on the page. Switches in place: abort old panel → `globe.reset()` → apply → mount. |
| Unsaved changes | Any scene-changing `globe.js` command outside `globe.quietly`, or a timeline, analysis, station or event-option edit, marks the project dirty (`•` in title and Project tab). Camera moves, view switches, pins, refreshes, project application and the timeline runner do not. Opening another project asks; leaving the page prompts. |
| Autosave | Dirty scene captured to IndexedDB 3 s after the last change; cleared on save or discard. One left at boot is offered under the Project tab (Recover / Dismiss). |
| Recent | Last 8 opened projects (IndexedDB). Local files keep their text up to 4 MB. |

### Bundled projects

Listed in [`projects/manifest.json`](projects/manifest.json).

| Project | Contents |
| --- | --- |
| [`overview`](projects/overview.terramenta.json) | Geometry tour, extruded cube, USGS past hour (60 s refresh), Celestrak `stations` |
| [`mars-mission`](projects/mars-mission.terramenta.json) | Earth → Mars, flight panel, `missionFlight` director, Earth layers off |
| [`mars-windows`](projects/mars-windows.terramenta.json) | Earth → Mars porkchop studies, 2026 and 2028 windows; opens on Analysis |
| [`leo-transfer`](projects/leo-transfer.terramenta.json) | 500 → 800 km Hohmann transfer vs. coasting reference; CelesTrak EOP and space weather; timeline slows the clock around each burn; RSW covariance with 3σ ellipsoids; altitude, SMA, eccentricity and 1σ in-track plots |
| [`ground-contacts`](projects/ground-contacts.terramenta.json) | 700 km SSO for a day over five ground stations; contacts, eclipses, nodes; timeline slows the clock for `polar.aos.svalbard.1` |
| [`targeted-transfer`](projects/targeted-transfer.terramenta.json) | Rough first-guess burns, solved on the Spacecraft tab: Hohmann Δv and second-burn epoch for a circular 800 km orbit; 20 N finite-burn duration for a 150 km SMA raise |
| [`attitude-modes`](projects/attitude-modes.terramenta.json) | Three pointing laws over six hours: LVLH observer with a star-tracker sun angle, SSO tracking Svalbard with its array sun-constrained, 6°/s inertial spinner; body axes and boresights on the globe |
| [`conjunction-screening`](projects/conjunction-screening.terramenta.json) | 550 km SSO satellite and a debris object crossing it at 70° with a 120 m radial miss, both with covariance; screened against the debris and the CelesTrak visual catalogue; timeline slows through TCA; opens on Analysis |
| [`orbit-determination`](projects/orbit-determination.terramenta.json) | 450 km satellite (truth Cd 2.6) and a 3 km-off a priori (Cd 2.2) with covariance; simulated radar (four stations) and GNSS tracking; radar batch solving for Cd, GNSS EKF, radar batch with Cd fixed; opens on Analysis |
| [`coverage-study`](projects/coverage-study.terramenta.json) | Six nadir imagers in three 700 km SSO planes from Keplerian elements, cone and rectangular fields of view with footprints; one-day sensor revisit over Europe (H3 4, shown) and global access above 10° (H3 2); Svalbard with a terrain horizon; opens on Analysis |

## Full panel

Six tabs on one row. Hidden panes stay mounted and bound. Sections collapse;
a non-default open/closed state is remembered per title (`localStorage`).
Sections marked *closed* start collapsed; Plots and Spacecraft reports hide
while the project has neither spacecraft nor plots, Events while it has no
spacecraft, Coverage, Conjunctions and Orbit determination while it has neither spacecraft nor studies.

| Tab | Section | Controls |
| --- | --- | --- |
| **Project** | Project | Name, description, source, unsaved marker; open file/URL; save; new; recover; recent; bundled projects |
| **Layers** | Imagery | The eight presets grouped by protocol (WMS/WMTS) with tile size, format and depth; previous/next; streaming on/off |
| | Vector tiles | Source (MapLibre boundaries, OSM Shortbread), its source layers and depth, colour, ring fill, streaming on/off |
| | GeoJSON overlays | Add from URL or local file; bundled samples and USGS feeds; refresh period; picking on/off. Per layer: visibility, colour, clamp to surface, extrude, simplestyle on/off, refresh, remove, feature counts and age |
| | Satellites | Add OMM from URL or local file; five Celestrak groups. Per layer: visibility, colour, trails and trail window, refresh, remove, and a filterable object list with per-object draw and trail toggles |
| **Spacecraft** | Spacecraft | Per spacecraft: visibility, colour, trails, span/samples/final mass/final 1σ, edit, go to start, OEM/SPK export, remove. Editor: initial state (Keplerian elements, GCRF state with circular-orbit fill, TLE, OMM), start/duration/step, vehicle, forces, integrator, impulsive and finite burns, uncertainty (frame, 1σ position and velocity, process noise, ellipsoid σ) |
| | Targeting | Per target: spacecraft, solve, apply, edit, remove; last solution's variables (initial, solved) and goals (desired, achieved, error; misses in red). Editor: name, spacecraft, iterations, variables (burn, parameter, perturbation, max step), goals (quantity, at, value, tolerance) |
| | Attitude | Per profile: law, AEM export, edit, remove; sensor angles (or LVLH Euler angles) chart with clock cursor (click to set the clock), min/time/max table. Editor: spacecraft, name, law (align/constrain axes and directions, or inertial quaternion), spin, target (station or site), step, body axes on/off, sensors (name, axis, direction or none, field of view: cone or rectangle, colour, drawing on/off) |
| | Ground stations | Per station: name, position, altitude, mask, horizon points, edit, remove. Add/edit: name, lat/lon, altitude, mask, horizon (`az:el, …`), colour. Drawn as a point overlay |
| | Propagation environment (*closed*) | EOP, space weather, kernel: URL or local file, coverage |
| **Timeline** | Timeline | Directed on/off; searched events shown/hidden (remembered, default hidden); previous/next event; strip (click to set the clock) and list of every event with time, source, actions, Go, Remove (project events). Add event: label, at clock or an event plus offset, actions from the current scene (rate, paused, view, frame, camera, focus) |
| **Analysis** | Plots | Saved plots: one quantity against time per spacecraft, burn markers, clock cursor (click to set the clock), CSV, remove. Add plot: quantity, spacecraft, label |
| | Spacecraft reports | Per spacecraft: span, samples, start/end/min/max of altitude, SMA, eccentricity, inclination, period, perigee/apogee, and 1σ radial/in-track/cross-track/RSS with covariance; Δv budget per burn; CSV. CZML of all spacecraft |
| | Events | Search options (eclipses, apsides, nodes, step). Per spacecraft: contact and umbra totals; contacts (station, AOS, duration, max elevation) and eclipses (entry, umbra, total) — click a row to set the clock; CSV |
| | Conjunctions | Per study: primary, secondaries and layers, threshold, radius, step, assumed σ; run/re-run, CSV, edit, remove; found/screened/filtered, layers not loaded; top 20 by Pc: TCA, object, miss (speed on hover), Pc (σ on hover), Pc max, CDM. Row: clock to TCA, catalogue object pinned. Editor: name, primary, spacecraft, catalogue layers, threshold, radius, step, catalogue σ |
| | Orbit determination | Per study: a priori, estimator, stations, tracking source, solve-for; run/re-run, apply, residual CSV, TDM, edit, remove; converged/iterations, RMS, observations and edits; estimate epoch, RSW 1σ, error against the truth (simulated); parameters; iterations; per station and type count, mean, RMS; residual/σ chart (EKF: post-fit) and EKF posterior 1σ chart (click to set the clock). Editor: name, a priori, estimator, stations, truth, tracking types with σ, step, seed, TDM URL or file, Cd/Cr with σ, edit σ |
| | Coverage | Per study: spacecraft, access, H3 resolution, region, step; run/re-run, show/hide on the globe, shaded metric, CSV, GeoJSON, edit, remove; cells seen, mean time covered, max gap (median and worst cell), mean revisit; metric legend. Editor: name, spacecraft, access (line of sight or sensor FOV), mask, resolution, bounds, step |
| | Porkchop plot | Origin/destination bodies, departure/arrival date ranges, grid steps; contoured total-Δv SVG from `terramenta-charta`, minimum marked and listed, grid CSV |
| | Porkchop studies | Save the inputs under a label (same label replaces); Open fills and computes; remove. The first study opens with the project |
| | Launch | Calls `addMission` for the selected pair. Per mission: status, dates, Δv, remove |
| **View** | Clock | Run/pause, rate (1× to 86,400×), jump to now or by hours/days, terminator shading |
| | Reference frame | ECEF / ECI |
| | Frame overlay (*closed*) | ECI and ECEF triads, graticule and spacing, sidereal arc, followed satellite's orbit and ground track over a set number of orbits; GMST, quaternion and per-frame speeds |
| | Camera | Fly to lat/lon/altitude, nine preset places, read back current view, reset |
| | Globe chrome (*closed*) | Built-in HUD, key legend, key bindings |

Launched spacecraft are visible in the heliocentric view (`U`; `5` follows the
first mission).

A telemetry panel shows the state snapshot: cursor and camera position,
altitude, frame, sidereal angle and ECI→ECEF quaternion, subsolar point, clock
and rate, active layers, overlay and satellite status, and both tile streamers.
Below it, the picked item: a feature's properties, a satellite's elements, or a
placemark's position.

For placemarks, [`feature.js`](src/feature.js) derives values the globe does
not report: body elevation at the view centre (90° minus the angular distance
to the sub-point), apparent solar time at the view centre (15° per hour from the
subsolar meridian), and lunar elongation and illuminated fraction
(`(1 − cos elongation) / 2`).

### Sample data

- [`data/geometry-tour.geojson`](data/geometry-tour.geojson): one feature per
  GeoJSON geometry type, plus a polygon with a hole, an antimeridian-crossing
  polygon, a launch profile climbing to 420 km, and a stepped terminal control
  area over Denver (five rings at increasing floors up to 12,000 ft). The
  airspace layering is only visible at low altitude with the camera tilted
  (Ctrl+drag).
- [`data/extruded-cube.geojson`](data/extruded-cube.geojson): a square ring at
  222 km over the equator on a layer with `extrude` on, producing a 222 km box.
  Kept separate from the tour because extrusion is per layer.
- [`data/simplestyle-examples.geojson`](data/simplestyle-examples.geojson): one
  labelled example per simplestyle member, including a deliberately invalid one
  that is ignored.

Overlay catalogue ([`feeds.js`](src/feeds.js)): the three samples, USGS past
hour, past day and significant past month (USGS sends
`Access-Control-Allow-Origin: *`). USGS third coordinate is depth; use the
layer's clamp-to-surface toggle to ignore it.

Satellite groups ([`orbits.js`](src/orbits.js)): crewed stations, GPS,
geostationary, Molniya, and a full Starlink shell (exceeds the 600-object
marker cap).

## Spacecraft

[`spacecraft-panel.js`](src/spacecraft-panel.js),
[`environment.js`](src/environment.js). A spacecraft is a `propagate` request
plus ephemeris display options: `globe.addSpacecraft(id, propagation, display)`
propagates (Cowell, synchronous) and puts the result up as ephemeris layer
`id` via `addPropagated`. Spacecraft layers are listed under Spacecraft, not
Satellites.

- **Open**: `workspace.open` applies the scene synchronously, then
  `applySpacecraft` loads `environment` (skipping URLs already loaded) and
  propagates each spacecraft. The globe starts without waiting. A spacecraft
  that fails keeps its spec (saved as is) and shows the error.
- **Editor**: validates the entry as a project would (`parseProject`), then
  propagates; editing re-propagates under the same id, keeping display
  options. Δv in m/s in the editor, km/s in the file. The clock jumps to the
  start if it is outside the span.
- **Environment**: wasm-global, not cleared by `reset`; stays loaded across
  projects. URL sources are saved; local files are not. CelesTrak serves
  `EOP-All.csv`/`SW-All.csv` with CORS; NAIF kernels need a local file or a
  same-origin URL.
- **Uncertainty**: `propagation.covariance` (1σ per axis or a 21-value lower
  triangle, in RSW, VNB, LVLH or GCRF; optional white-acceleration process
  noise) at the start, propagated linearly through the state transition
  matrix. The editor takes metres, m/s and m²/s³. The position ellipsoid is
  drawn at `uncertaintySigma` σ in the point colour. Events, attitude,
  coverage and targeting ignore it.
- **Export**: OEM (KVN, GCRF/UTC; covariance blocks with a covariance) and SPK
  Type 13 re-run the propagation. CSV and CZML: [Reports](#reports).

## Reports

[`reports.js`](src/reports.js) (pure), [`reports-panel.js`](src/reports-panel.js),
[`chart.js`](src/chart.js), [`analysis.js`](src/analysis.js).

- **Samples**: the spacecraft's OEM (`propagate` with `kvn`), parsed per
  segment; cached per propagation.
- **Quantities**: osculating two-body elements about Earth
  (μ = 398600.4418 km³/s²) from each GCRF state: altitude, radius, speed, SMA,
  eccentricity, inclination, RAAN, argument of perigee, true anomaly (argument
  of latitude when circular), period, perigee/apogee altitude. Altitudes are
  radius − 6378.137 km, not geodetic. With a covariance: 1σ radial,
  in-track, cross-track and RSS position (m), matched to samples by time.
- **Δv budget**: from the request by the rocket equation, as the propagator
  models burns: impulsive burns spend propellant only with `ispS`; finite
  burns flow at `thrustN / (ispS·g0)` and stop at dry mass (default
  min(850 kg, wet)).
- **CSV**: per spacecraft (UTC, GCRF state, every quantity, 1σ columns with a covariance), per plot
  (`utc, spacecraft, value`), per porkchop grid (dates, time of flight,
  departure/arrival/total Δv; unsolved cells empty).
- **CZML**: all spacecraft, `INERTIAL` frame (ICRF ≈ GCRF), metres, one
  Lagrange position interval per OEM segment so burns are not interpolated
  across; document clock spans the data.
- **Saved** under `analysis`: `plots` (`quantity` is a `reports.js`
  `PLOT_QUANTITIES` key) and `porkchops` (dates `YYYY-MM-DD`, UTC; `steps` 2–101,
  default 33).

## Events

[`events.js`](src/events.js), [`events-panel.js`](src/events-panel.js);
search in `terramenta-solare` `events` via `globe.findEvents(propagation,
options)`, which re-runs the propagation.

- **Method**: event function sampled every `stepS` (default 30 s), sign
  changes refined by Brent's method to 1 ms; spans shorter than one step can
  be missed. States are Hermite-interpolated from the propagation's samples.
- **Contacts**: elevation above the station's geodetic horizon (WGS 84) above
  `minElevationDeg`; GCRF → ITRF with IAU 2006/2000A (CIO) and loaded EOP,
  recomputed every 600 s with Earth rotation exact in between. Max elevation by
  golden-section search. A station's `horizon` (`[[azimuthDeg,
  elevationDeg], …]`, azimuth ascending in [0, 360), linear between points,
  wrapping) raises the mask toward terrain: visible when elevation exceeds
  the larger of the two. No light time or refraction.
- **Eclipses**: Earth (6378.137 km sphere) conical shadow, Sun from the loaded
  kernel or the analytic series; penumbra spans include the umbra.
- **Apsides** (`r·v = 0`) and **nodes** (GCRF `z = 0`): off by default.
- Spans open at either end are clipped to the propagation span. Results are
  cached per spacecraft entry and invalidated by any station or option edit.
- **CSV**: `utc, end_utc, event, station, max_elevation_deg`, time-ordered.

## Mission sequence

[`sequence.js`](src/sequence.js); runner in `terramenta-solare` `sequence` via
`globe.runSequence(propagation, sequence)`.

- A spacecraft's `sequence` is an ordered command list run from its
  propagation's `initial` state at `start`; the propagation then has no
  `stop`, `durationS`, `maneuvers` or `finiteBurns`. Commands: optional
  `label` plus one of `propagate`, `maneuver`, `finiteBurn`, `target` (schema
  `sequenceCommand`).
- The run resolves burn epochs, Δv and stop into the flown propagation; it is
  what the layer, reports, events, coverage, conjunctions, OD and timeline
  (`<id>.burnN`, `<id>.finiteN`) use. Saving writes the source propagation and
  sequence. Burn references are counted from the sequence before it runs.
- Validation: one action per command, one stop per propagate, unique labels,
  target variables naming a labelled command of the right kind in their own
  `sequence`.
- Spacecraft tab: the editor takes the sequence as JSON (LEO → GEO template);
  each sequenced spacecraft lists its segments (span, Δv, altitude, e, i,
  target convergence); a row sets the clock to its start.
- Example: `projects/geo-transfer.terramenta.json`.

## Targeting

[`targeting.js`](src/targeting.js), [`targeting-panel.js`](src/targeting-panel.js);
solver in `terramenta-solare` `targeting` via `globe.target(propagation,
options)`.

- **Variables**: impulsive burn `dv1`–`dv3` (km/s, components in the burn's
  frame) or `epoch` (s offset); finite burn `start` (s offset) or `duration`
  (s). Burns 1-based, in propagation order. `perturbation` (default 1e-5 km/s,
  1 s) is the finite-difference step and the variable's scale; `maxStep`
  (default 0.1 km/s, 600 s) caps each iteration's change.
- **Goals**: `quantity` from the report quantities plus `flightPathAngleDeg`
  and `massKg`, two-body osculating about Earth; `at` `end`, `burnN` (after
  the burn), `burnN.before`, `finiteN`, `finiteN.end` or a UTC epoch.
  Angles compare modulo 360°. Met when every |error| ≤ `tolerance`.
- **Method**: Newton on residuals scaled by tolerance; forward-difference
  Jacobian, one propagation per variable per iteration. Minimum-norm step with
  more variables than goals, least squares with fewer; the step is scaled to
  respect `maxStep`, then halved until the residual norm drops. Stops after
  `maxIterations` (default 25) or when no step improves.
- Each evaluation is a full propagation, ended two steps after the latest goal
  unless a goal is at `end`. Burn-relative goals use the propagator's exact
  burn states; other epochs are Hermite-interpolated.
- **Apply** re-propagates the spacecraft with the solved burns (epochs rounded
  to 1 ms) and marks the project unsaved. Results are not saved; editing a
  target drops its result, re-propagating the spacecraft marks it stale.

## Attitude

[`attitude.js`](src/attitude.js), [`attitude-panel.js`](src/attitude-panel.js);
laws in `terramenta-solare` `attitude` via `globe.attitude(propagation,
options)`, drawn with `globe.showAttitude`.

- **Laws**: `primary` body axis exactly on its direction, `secondary` as close
  to its own as that allows (TRIAD); or `quaternion` (body → GCRF, `[x, y, z,
  w]`) held. `spin` (`axis`, `rateDegS`, optional zero-angle `epoch`) turns
  the body about its own axis on top of either.
- **Directions**: `nadir`, `zenith`, `velocity`, `antiVelocity`,
  `orbitNormal`, `antiOrbitNormal`, `sun`, `antiSun`, `target` (a ground
  `station` by id, or a `target` site), or a GCRF vector `[x, y, z]`. Sun as
  for eclipses; sites through full Earth orientation.
- **Sensors**: a body axis and, optionally, a direction; the angle between
  them is sampled. Drawn as a boresight to the surface (or four axis lengths
  into space) unless `draw: false`. `fov`: `{halfAngleDeg}` (cone) or
  `{halfXDeg, halfYDeg, reference}` (rectangle, width along body `reference`,
  default the axis least aligned with the boresight); its footprint is drawn
  on the globe (rays past the limb on the limb) with four cone edges, and
  coverage counts access through it.
- Sampled every `stepS` (default 60) over the propagation span. Euler angles
  are 3-2-1 roll/pitch/yaw of the body from CCSDS LVLH (z nadir, y −orbit
  normal). Samples where the law is undefined (parallel directions) are
  dropped and counted.
- Body axes X red, Y green, Z blue, at every drawn object of the spacecraft's
  layer; slerped between samples; nothing outside the span.
- **AEM**: CCSDS 504.0-B-1 KVN, `ICRF` → `SC_BODY_1`, `A2B`, scalar last,
  linear interpolation, sign-continuous.
- Re-sampled when the profile, its spacecraft or its station changes. Samples
  are not saved; profiles of removed spacecraft are dropped on save.

## Coverage

[`coverage.js`](src/coverage.js), [`coverage-panel.js`](src/coverage-panel.js);
`terramenta-solare` `coverage` via `globe.coverage(options)`, which re-runs
each propagation.

- **Grid**: H3 cells at `resolution` (0–7, default 2) whose centres lie in
  `bounds` (`[west, south, east, north]`°, west > east crosses the
  antimeridian; default the globe); at most 200 000 cells.
- **Access**: a cell is covered at a sample when some spacecraft sees its
  centre above `minElevationDeg` (geodetic up, WGS 84). `access: "sensors"`
  also requires it inside a field of view of the spacecraft's attitude
  profile; spacecraft without one are left out.
- **Sampling**: every `stepS` (default 60) over the span all spacecraft share;
  times resolve to one step.
- **Per cell**: `fraction` of samples covered, `accesses` (runs of covered
  samples), `maxGapS` (leading and trailing spans included; the span if never
  covered), `meanGapS` (between accesses), `firstAccessS`.
- **Globe**: the `show` study (one at a time) is drawn as overlay `coverage`,
  cells filled by `metric` (`fraction`, `maxGap`, `meanGap`, `accesses`) on a
  six-stop ramp from the study's minimum to maximum; never-covered cells left
  out for `fraction` and `accesses`. Cells around a pole close through it.
- **Results** are not saved; the shown study re-runs when it, a spacecraft
  entry or a profile changes, others on Run. Changing the metric or label
  does not re-run.
- **Export**: CSV (`h3, lat_deg, lon_deg, fraction, accesses, max_gap_s,
  mean_gap_s, first_access_s`) and GeoJSON (drawn cells with every statistic).

## Conjunctions

[`conjunction.js`](src/conjunction.js), [`conjunction-panel.js`](src/conjunction-panel.js);
`terramenta-solare` `conjunction` via `globe.conjunctions(options)`, which re-runs
each propagation.

- **Sources**: `primary` and `secondaries` are spacecraft ids; `layers` are
  ephemeris layers (every object, SGP4 or OEM). Layers not loaded when the
  study runs are skipped and listed.
- **Search**: closest approaches within `thresholdKm` (default 5) on a
  `stepS` grid (default 60), refined to 0.1 ms; SGP4 objects outside the
  primary's radius band are filtered.
- **Pc**: Foster 2D with `hardBodyRadiusM` (combined, default 20) and the
  combined covariance: spacecraft `propagation.covariance`, or
  `secondarySigmaKm` (RSW 1σ) for catalogue objects and spacecraft without
  one. Pc max: largest Pc over the covariance's scale.
- **Results** are not saved; Run re-runs. A study is stale when it or a
  spacecraft entry changes.
- **Export**: CSV (`tca_utc, secondary, name, norad_id, miss_m,
  relative_speed_m_s, radial_m, in_track_m, cross_track_m, sigma_major_m,
  sigma_minor_m, pc, pc_max`), CCSDS CDM per conjunction.

## Orbit determination

[`od.js`](src/od.js), [`od-panel.js`](src/od-panel.js); `terramenta-solare` `od`
via `globe.determineOrbit(options)`.

- **A priori**: `spacecraft`'s propagation (initial state, forces, vehicle);
  its `covariance` is the a priori covariance and EKF process noise. EKF
  requires one.
- **Observations**: `tdm` (CCSDS TDM URL or `data:`; `RANGE`,
  `DOPPLER_INSTANTANEOUS`, `ANGLE_1/2` AZEL/RADEC; participants match station
  ids) and/or `simulate`: tracking of spacecraft `truth` every `stepS`
  (default 60) while above each station's mask, Gaussian noise `sigma`, `bias`
  (km, km/s, deg), `seed`. A tracking entry without `station` uses every study
  station; `stations` defaults to all ground stations.
- **Estimators**: `batch` — weighted least squares about the a priori epoch,
  `solveFor` Cd/Cr with a priori σ, σ-editing at `editSigma`×RMS (default 3);
  `ekf` — sequential, Joseph form, innovation gate `editSigma` (default 5),
  estimate at the last observation. Light time on by default.
- **Apply**: re-adds the a priori spacecraft from the estimate (initial
  state, GCRF covariance, estimated Cd/Cr; process noise kept on GCRF axes;
  the a priori start kept).
- **Results** are not saved; Run re-runs. A study is stale when it, its
  spacecraft entries or the ground stations change.
- **Export**: CSV (`time_utc, station, type, observed, residual, post_fit,
  sigma, edited`; km, km/s, deg), CCSDS TDM of the observations used.

## Timeline

[`timeline.js`](src/timeline.js), [`timeline-panel.js`](src/timeline-panel.js).
Events on the simulated clock; the workspace runs the timeline on every
snapshot.

| Source | Ids | Actions |
| --- | --- | --- |
| Spacecraft | `<id>.start`, `.end`, `.burnN`, `.finiteN`, `.finiteN.end` | none |
| Searched | `<id>.aos.<station>.N`, `.los.<station>.N`, `.umbraN`, `.umbraN.end`, `.penumbraN`, `.penumbraN.end`, `.periapsisN`, `.apoapsisN`, `.ascendingNodeN`, `.descendingNodeN` ([Events](#events); N per kind from 1) | none |
| Missions | `<id>.departure`, `.escape`, `.arrival`, `.capture` (once known) | none |
| Director | `director-*` | generated, not saved |
| Project | `timeline.events[].id` (no dots) | `actions` |

`at`: a date-time, or `{event, offsetS}` naming any event above (chains
allowed, cycles rejected). Searched-event references are checked for the
spacecraft and station only; one past the last found resolves to unknown. `skipTo` (same form): once it resolves, a clock
before it jumps forward to it, once per open.

Actions: `timeScale` (number or `"max"`; keeps the clock's direction),
`rampDoublingS` (doubles the rate every N wall-clock seconds up to the
maximum), `paused`, `view`, `frame`, `focus: {layer, noradId?}` (default:
the layer's first object), `follow` (mission id), `camera: {lat, lon,
altitudeKm}`.

Directed (`timeline.directed`, default true): when the latest event at or
before the clock changes — played past, scrubbed, jumped — the baseline
(project `clock.timeScale`/`paused`, `view`, `frame`) and the actions of every
event up to it are merged. A key is applied if its merged value changed since
the last application or the latest event sets it, so a rate set by hand holds
until an event changes the rate, and scrubbing back before every event
restores the baseline. `camera` applies from the latest event only. A view
request repeats until it lands.

## Flight panel

`ui.panel: "flight"` ([`flight-panel.js`](src/flight-panel.js)): one mission
(`director.mission`, else the first), no tabs.

- **Clock**: directed on/off, run/pause, forward/backward, rate slider.
- **Mission**: status line and four phase buttons (departure, escape, arrival,
  capture) that jump the clock to each milestone once known and highlight when
  reached.
- **Look at**: Sun, Earth, Mars, barycentre, or spacecraft
  (`setHeliocentricAnchor` / `followMission`).
- **Project**: as the Project tab.

## Directors

[`director.js`](src/director.js). A director is a timeline preset:
`events(config)` returns `{events, skipTo}` in `timeline` form, run alongside
the project's own events and shown read-only. Fields are typed in the schema
(`$defs/<type>`); a new director needs a `$defs` entry and an `allOf` branch
in `$defs/director`.

`missionFlight` — `{mission, launchLeadSeconds = 3600, departureTimeScale = 600,
departureDoublingSeconds = 2}`:

| At | Actions |
| --- | --- |
| `skipTo`, `<mission>.departure − launchLeadSeconds` | `departureTimeScale`, globe view |
| `<mission>.departure` | `departureTimeScale`, `rampDoublingS: departureDoublingSeconds` |
| `<mission>.escape` | `"max"`, heliocentric view |
| `<mission>.capture` | pause |

## Data flow

Controls are one-way: a control sends a command and does not set its own
value. Each state snapshot sets every control. As a result the panel cannot
drift from the globe, and changes made through the globe's own key bindings
(e.g. `P`) are reflected in the panel.

Exceptions: a slider is not updated while held, and a text field is not
updated while focused.

Other patterns:

- **Picking** ([`feature.js`](src/feature.js)): the globe reports hover only.
  The app treats a press and release without movement as a click and calls
  `pinFeature` / `pinSatellite` / `pinPlacemark`, or clears the pin over empty
  space. Satellites are pinned by NORAD id. Priority when several are hit:
  pinned over hovered; then satellite, placemark, feature.
- **Local overlay files** ([`overlays.js`](src/overlays.js)): the app keeps the
  `File`, re-reads it on the configured period, and re-sends it under the same
  id.
- **Satellite object lists** ([`ephemeris.js`](src/ephemeris.js)): the app
  calls `ephemerisObjects(id)` only when a layer's `revision` changes, and
  renders at most 200 rows behind a filter.
- **Porkchop plots** ([`porkchop.js`](src/porkchop.js),
  [`mission.js`](src/mission.js)): `terramenta-charta` is stateless, so the
  plot is computed on request — once without levels to get the Δv range, then
  with levels across it — and drawn as hand-built SVG. Studies are saved
  in the project (`analysis.porkchops`).

### Boot order

[`main.js`](src/main.js) loads the module and the project in parallel, opens
the project in the workspace — builds the panel it names from `layers()`,
`vectorLayers()` and `limits()` (available before the renderer starts) and
queues it ([`project.js`](src/project.js) `applyProject`) — then calls `start`
with the project's `view`. Commands sent before `start` are applied on the
first frame. Panels take an `AbortSignal` that fires when the project closes;
anything with a timer or subscription must stop on it.

### Adding a control

1. Add a `GlobeCommand` variant and apply it in
   [`api.rs`](../terramenta-globe/src/api.rs).
2. Bind it in [`wasm.rs`](../terramenta-globe/src/wasm.rs).
3. Re-export it from [`globe.js`](src/globe.js).
4. Build the control in [`panel.js`](src/panel.js) from
   [`widgets.js`](src/widgets.js) and `bind` it to the snapshot field it
   reflects.

Display-only values need only a `GlobeState` field and a line in
[`readout.js`](src/readout.js).

## Layout

```
index.html               The page
assets/terramenta.png    Loading-screen banner
package.json             `npm test` only
schema/
  project.schema.json           Project format (JSON Schema 2020-12)
projects/
  manifest.json                 Bundled project list
  overview.terramenta.json      Layers tour
  mars-mission.terramenta.json  Directed Earth → Mars flight
  mars-windows.terramenta.json  Porkchop studies
  leo-transfer.terramenta.json  Propagated Hohmann transfer
  ground-contacts.terramenta.json  Ground station contacts and eclipses
  targeted-transfer.terramenta.json  Differential correction of burns
  attitude-modes.terramenta.json  Pointing laws, body axes, sensor angles
  coverage-study.terramenta.json  Imager constellation, footprints, H3 coverage
  conjunction-screening.terramenta.json  Close approaches, Pc, CDM
  orbit-determination.terramenta.json  Simulated tracking, batch/EKF, Cd
data/
  geometry-tour.geojson         One feature per geometry type
  extruded-cube.geojson         Extruded ring
  simplestyle-examples.geojson  simplestyle-spec member examples
styles/app.css           Styles
src/
  main.js                Boot: project → workspace → globe; per-project UI mount
  workspace.js           Open project: in-place switching, dirty tracking, autosave, recent, drop
  store.js               IndexedDB key-value store
  project.js             Project load, upgrade, validate, apply, capture, save
  environment.js         Propagation environment: EOP, space weather, kernel
  schema.js              JSON Schema subset validator
  project-panel.js       Project section (open, save, recover, recent, examples)
  director.js            Timeline presets (missionFlight)
  timeline.js            Timeline: scene/project/director events, resolution, runner
  analysis.js            Project analysis: saved plots and porkchop studies
  events.js              Ground stations, event options, findEvents cache, searched timeline events
  sequence.js            Mission sequence validation, burn counts, segment rows
  targeting.js           Project targets, solve/apply via globe.target
  attitude.js            Project attitude profiles, sampling and drawing via globe.attitude
  coverage.js            Project coverage studies, globe.coverage runs, shading, CSV/GeoJSON
  conjunction.js         Project conjunction studies, globe.conjunctions runs, ranking, CSV
  od.js                  Project orbit determination studies, globe.determineOrbit runs, apply, residual statistics
  reports.js             OEM parsing, osculating elements, Δv budget, CSV, CZML
  flight-panel.js        Flight panel
  globe.js               Sole wrapper around the globe WASM module; records add* sources, reports scene changes
  porkchop.js            Sole wrapper around the charta WASM module
  panel.js               Control panel tabs and the one-way bind rule
  overlays.js            GeoJSON layer controls, local-file refresh
  ephemeris.js           Satellite layer controls, object list
  mission.js             Analysis tab: porkchop plot, studies, grid CSV, launch
  spacecraft-panel.js    Spacecraft tab: spacecraft list, editor, OEM/SPK export, environment
  events-panel.js        Ground stations section; Analysis tab events section, CSV
  targeting-panel.js     Spacecraft tab targeting section and editor
  attitude-panel.js      Spacecraft tab attitude section: charts, AEM, editor
  coverage-panel.js      Analysis tab coverage section: summaries, legend, export, editor
  conjunction-panel.js   Analysis tab conjunctions section: ranked table, CDM/CSV, editor
  od-panel.js            Analysis tab orbit determination section: estimate, residuals, charts, editor
  timeline-panel.js      Timeline tab: strip, event list, add event
  reports-panel.js       Analysis tab: plots, spacecraft reports, CSV/CZML export
  chart.js               SVG builder and time-series line chart
  feature.js             Picking, click handling, placemark calculations
  readout.js             Telemetry panel
  widgets.js             Buttons, toggles, choices, sliders
  format.js              Coordinate, altitude, rate, duration and UTC time formatting
  dom.js                 Element helpers, collapsible sections, tab strip, download
  places.js              Camera presets
  feeds.js               Overlay catalogue
  orbits.js              Satellite catalogue
scripts/
  build.sh               Builds globe/ and charta/
  serve.sh               Builds if needed, then serves
globe/                   Build output (git-ignored)
charta/                  Build output (git-ignored)
```
