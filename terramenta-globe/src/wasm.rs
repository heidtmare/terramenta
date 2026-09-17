//! The JavaScript binding: [`crate::api`] as a module an embedder can import.
//!
//! Everything here is a thin adapter. Commands go in as plain arguments and are
//! queued rather than applied, so calling one before the globe has started is
//! fine — it lands on the first tick. State comes back out as ordinary
//! JavaScript objects through a callback, which is the only channel that has to
//! cross in that direction.
//!
//! The shape of those objects is `crate::api::GlobeState`; the module's own
//! documentation is `terramenta-webapp/src/globe.js`, which wraps every one of
//! these calls.

use serde::Deserialize;
use wasm_bindgen::prelude::*;

use crate::GlobeConfig;
use crate::api::{
    self, AltitudeMode, EphemerisRequest, GlobeCommand, GlobeState, Limits, OverlayAltitude,
    OverlayRequest, OverlaySource, OverlayStyle, Selection, TrailPath, TrailWindow,
};
use crate::frame::FrameMode;
use crate::geo::LatLon;

/// Starts the globe on a canvas, by CSS selector, loading its assets from a
/// path relative to the page.
///
/// Either argument may be `null` for its default — `"#terramenta"` and
/// `"assets"`. This does not return in the usual sense: winit hands control to
/// the browser's event loop by unwinding through an exception, which the caller
/// is expected to catch and ignore. `terramenta-webapp/src/globe.js` shows what
/// that looks like.
#[wasm_bindgen]
pub fn start(canvas_selector: Option<String>, asset_path: Option<String>) {
    let defaults = GlobeConfig::default();
    crate::app(GlobeConfig {
        canvas_selector: canvas_selector.unwrap_or(defaults.canvas_selector),
        asset_path: asset_path.unwrap_or(defaults.asset_path),
        // A web embedder adds its layers through `addOverlay` and `addEphemeris`
        // once the module has loaded; the queue holds them until the globe is
        // there to take them.
        overlays: defaults.overlays,
        ephemerides: defaults.ephemerides,
    })
    .run();
}

// ---------------------------------------------------------------------------
// Catalogue
// ---------------------------------------------------------------------------

/// The imagery presets the globe offers, in the order they cycle.
///
/// Available before [`start`], so an interface can be built before the globe is.
#[wasm_bindgen]
pub fn layers() -> JsValue {
    to_js(&crate::layers())
}

/// The Mapbox Vector Tile presets the globe offers, in the order they cycle.
///
/// Available before [`start`], so an interface can be built before the globe is.
#[wasm_bindgen(js_name = vectorLayers)]
pub fn vector_layers() -> JsValue {
    to_js(&crate::vector_layers())
}

/// The ranges the controls accept, for an interface building its own inputs.
#[wasm_bindgen]
pub fn limits() -> JsValue {
    to_js(&Limits::current())
}

// ---------------------------------------------------------------------------
// Camera
// ---------------------------------------------------------------------------

/// Looks straight down at a coordinate, optionally from a given height in
/// kilometres.
#[wasm_bindgen(js_name = lookAt)]
pub fn look_at(lat: f32, lon: f32, altitude_km: Option<f32>) {
    api::send(GlobeCommand::LookAt {
        coordinate: LatLon::new(lat, lon),
        altitude_km,
    });
}

#[wasm_bindgen(js_name = setAltitude)]
pub fn set_altitude(altitude_km: f32) {
    api::send(GlobeCommand::SetAltitude(altitude_km));
}

/// Turns the view by an angle in degrees, the way a drag would.
#[wasm_bindgen(js_name = orbitBy)]
pub fn orbit_by(yaw_deg: f32, pitch_deg: f32) {
    api::send(GlobeCommand::OrbitBy { yaw_deg, pitch_deg });
}

/// Zooms by an exponent: positive closes in, negative pulls back.
#[wasm_bindgen(js_name = zoomBy)]
pub fn zoom_by(exponent: f32) {
    api::send(GlobeCommand::ZoomBy(exponent));
}

#[wasm_bindgen(js_name = resetView)]
pub fn reset_view() {
    api::send(GlobeCommand::ResetView);
}

// ---------------------------------------------------------------------------
// Reference frame
// ---------------------------------------------------------------------------

/// Draws the scene in `"ecef"` or `"eci"`. An unknown name is ignored.
#[wasm_bindgen(js_name = setFrame)]
pub fn set_frame(mode: &str) {
    if let Some(mode) = FrameMode::from_id(mode) {
        api::send(GlobeCommand::SetFrame(mode));
    }
}

#[wasm_bindgen(js_name = toggleFrame)]
pub fn toggle_frame() {
    api::send(GlobeCommand::ToggleFrame);
}

// ---------------------------------------------------------------------------
// Sun and clock
// ---------------------------------------------------------------------------

#[wasm_bindgen(js_name = setSunPaused)]
pub fn set_sun_paused(paused: bool) {
    api::send(GlobeCommand::SetSunPaused(paused));
}

/// Simulated seconds per real second.
#[wasm_bindgen(js_name = setTimeScale)]
pub fn set_time_scale(scale: f32) {
    api::send(GlobeCommand::SetTimeScale(scale));
}

/// Jumps the simulated clock to a moment, in seconds since the Unix epoch —
/// which is `Date.now() / 1000`, not milliseconds.
#[wasm_bindgen(js_name = setClock)]
pub fn set_clock(unix_seconds: f64) {
    api::send(GlobeCommand::SetClock(unix_seconds));
}

#[wasm_bindgen(js_name = snapClockToNow)]
pub fn snap_clock_to_now() {
    api::send(GlobeCommand::SnapClockToNow);
}

/// Whether the sun lights the globe. Off, there is no terminator: every face is
/// lit as though the sun were overhead, which is how imagery of somewhere in
/// darkness is read.
#[wasm_bindgen(js_name = setSunShaded)]
pub fn set_sun_shaded(shaded: bool) {
    api::send(GlobeCommand::SetSunShaded(shaded));
}

// ---------------------------------------------------------------------------
// Imagery
// ---------------------------------------------------------------------------

/// Selects a preset by its index in [`layers`], wrapping past the end.
#[wasm_bindgen(js_name = setLayer)]
pub fn set_layer(index: usize) {
    api::send(GlobeCommand::SetLayer(index));
}

#[wasm_bindgen(js_name = nextLayer)]
pub fn next_layer() {
    api::send(GlobeCommand::NextLayer);
}

#[wasm_bindgen(js_name = previousLayer)]
pub fn previous_layer() {
    api::send(GlobeCommand::PreviousLayer);
}

/// Whether imagery tiles are streamed at all. Off, the globe falls back to its
/// built-in base texture.
#[wasm_bindgen(js_name = setImageryEnabled)]
pub fn set_imagery_enabled(enabled: bool) {
    api::send(GlobeCommand::SetImageryEnabled(enabled));
}

// ---------------------------------------------------------------------------
// Vector tiles
// ---------------------------------------------------------------------------

/// Whether Mapbox Vector Tiles are streamed at all.
///
/// Off, every tile is dropped rather than hidden — a vector tile is cheap to
/// ask for again and the meshes are not cheap to keep — so switching back
/// re-walks the view and refetches what it needs.
#[wasm_bindgen(js_name = setVectorTilesEnabled)]
pub fn set_vector_tiles_enabled(enabled: bool) {
    api::send(GlobeCommand::SetVectorTilesEnabled(enabled));
}

/// Selects a preset by its index in [`vector_layers`], wrapping past the end.
#[wasm_bindgen(js_name = setVectorTileLayer)]
pub fn set_vector_tile_layer(index: usize) {
    api::send(GlobeCommand::SetVectorTileLayer(index));
}

#[wasm_bindgen(js_name = nextVectorTileLayer)]
pub fn next_vector_tile_layer() {
    api::send(GlobeCommand::NextVectorTileLayer);
}

#[wasm_bindgen(js_name = previousVectorTileLayer)]
pub fn previous_vector_tile_layer() {
    api::send(GlobeCommand::PreviousVectorTileLayer);
}

/// Recolours the vector tile layer. Takes the same colour and size fields
/// [`add_overlay`] does; anything left out goes back to its default.
///
/// ```js
/// setVectorTileStyle({ lineColor: "#8fd6ff", lineWidthPx: 1.2 });
/// // Rings filled as well as outlined — off by default, because a basemap's
/// // fills would hide the imagery under them:
/// setVectorTileStyle({ fillColor: "#8fd6ff33" });
/// ```
///
/// Unlike an overlay this rebuilds the tiles on screen rather than swapping a
/// colour on them: a tile's meshes are keyed to the style they were built with,
/// and a layer whose fill was transparent never built a fill at all.
#[wasm_bindgen(js_name = setVectorTileStyle)]
pub fn set_vector_tile_style(style: JsValue) -> bool {
    let Some(style) = from_js::<StyleOptions>(&style) else {
        return false;
    };
    api::send(GlobeCommand::SetVectorTileStyle(style.resolve()));
    true
}

// ---------------------------------------------------------------------------
// GeoJSON overlays
// ---------------------------------------------------------------------------

/// The options an overlay is added with, as a plain JavaScript object.
///
/// Exactly one of `url` and `text` says where the data comes from — a URL the
/// globe fetches and can keep refetching, or GeoJSON the embedder already has,
/// which is how a file the user picked gets here. Everything else is optional,
/// and colours are hex strings so a `<input type="color">` value can be passed
/// straight through.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OverlayOptions {
    url: Option<String>,
    text: Option<String>,
    label: Option<String>,
    /// Seconds between refetches. Omit or `null` to fetch once.
    refresh_seconds: Option<f32>,
    visible: Option<bool>,
    #[serde(flatten)]
    altitude: AltitudeOptions,
    #[serde(flatten)]
    style: StyleOptions,
}

/// What the layer does with the third element of its positions.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AltitudeOptions {
    /// `"relativeToSurface"` to draw heights, `"clampToSurface"` to ignore them
    /// and drape everything on the ground. Anything else leaves the default.
    altitude_mode: Option<String>,
    /// Metres of height per unit of that element: `1000` for a feed in
    /// kilometres, negative for one that counts downward.
    altitude_scale: Option<f32>,
    /// Whether polygons are joined to the ground by walls, turning a ring at a
    /// height into a solid standing on the surface.
    extrude: Option<bool>,
}

impl AltitudeOptions {
    fn resolve(&self) -> OverlayAltitude {
        let defaults = OverlayAltitude::default();
        OverlayAltitude {
            mode: self
                .altitude_mode
                .as_deref()
                .and_then(AltitudeMode::from_id)
                .unwrap_or(defaults.mode),
            scale: self
                .altitude_scale
                .filter(|scale| scale.is_finite())
                .unwrap_or(defaults.scale),
            extrude: self.extrude.unwrap_or(defaults.extrude),
        }
    }
}

/// The parts of an overlay's appearance, each falling back to the default.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct StyleOptions {
    point_color: Option<String>,
    point_size_px: Option<f32>,
    line_color: Option<String>,
    line_width_px: Option<f32>,
    fill_color: Option<String>,
}

impl StyleOptions {
    /// Fills in whatever was left out. A colour that will not parse is left at
    /// its default rather than failing the call: a layer in the wrong colour is
    /// recoverable, and a layer that never appeared is a puzzle.
    fn resolve(&self) -> OverlayStyle {
        self.resolve_or(OverlayStyle::default())
    }

    /// The same, against a different set of defaults — an ephemeris is drawn in
    /// a palette of its own, because it is the one layer that is never on the
    /// ground and has to read against the sky as well as against imagery.
    fn resolve_or(&self, defaults: OverlayStyle) -> OverlayStyle {
        let color = |hex: &Option<String>, fallback| {
            hex.as_deref()
                .and_then(|hex| bevy::color::Srgba::hex(hex).ok())
                .unwrap_or(fallback)
        };
        OverlayStyle {
            point_color: color(&self.point_color, defaults.point_color),
            point_size_px: self.point_size_px.unwrap_or(defaults.point_size_px),
            line_color: color(&self.line_color, defaults.line_color),
            line_width_px: self.line_width_px.unwrap_or(defaults.line_width_px),
            fill_color: color(&self.fill_color, defaults.fill_color),
        }
    }
}

/// Puts a GeoJSON overlay up, or replaces the one already under this id.
///
/// ```js
/// addOverlay("quakes", {
///   url: "https://earthquake.usgs.gov/.../all_hour.geojson",
///   label: "Earthquakes, past hour",
///   refreshSeconds: 60,
///   pointColor: "#ff9e3d",
/// });
/// addOverlay("local", { text: await file.text() });
/// // A feed whose third element is depth in kilometres, drawn flat:
/// addOverlay("quakes", { url, altitudeMode: "clampToSurface" });
/// // A track written in kilometres above the ground:
/// addOverlay("flight", { url, altitudeScale: 1000 });
/// // Footprints at a height, walled down to the ground:
/// addOverlay("buildings", { url, extrude: true });
/// ```
///
/// Returns whether the options could be read. A layer that fails to *load*
/// still returns `true` — the failure arrives on the state stream, with the
/// reason, because by then the call is long over.
#[wasm_bindgen(js_name = addOverlay)]
pub fn add_overlay(id: String, options: JsValue) -> bool {
    let Some(options) = from_js::<OverlayOptions>(&options) else {
        return false;
    };
    // A URL wins if somehow both were given, because it is the one the globe
    // can go back to.
    let source = match (options.url, options.text) {
        (Some(url), _) => OverlaySource::Url(url),
        (None, Some(text)) => OverlaySource::Text(text),
        (None, None) => return false,
    };

    api::send(GlobeCommand::AddOverlay(OverlayRequest {
        id,
        label: options.label.unwrap_or_default(),
        source,
        style: options.style.resolve(),
        altitude: options.altitude.resolve(),
        refresh_seconds: options.refresh_seconds,
        visible: options.visible.unwrap_or(true),
    }));
    true
}

#[wasm_bindgen(js_name = removeOverlay)]
pub fn remove_overlay(id: String) {
    api::send(GlobeCommand::RemoveOverlay(id));
}

#[wasm_bindgen(js_name = setOverlayVisible)]
pub fn set_overlay_visible(id: String, visible: bool) {
    api::send(GlobeCommand::SetOverlayVisible { id, visible });
}

/// Restyles a layer without refetching or rebuilding it. Takes the same colour
/// and size fields [`add_overlay`] does; anything left out goes back to its
/// default.
#[wasm_bindgen(js_name = setOverlayStyle)]
pub fn set_overlay_style(id: String, style: JsValue) -> bool {
    let Some(style) = from_js::<StyleOptions>(&style) else {
        return false;
    };
    api::send(GlobeCommand::SetOverlayStyle {
        id,
        style: style.resolve(),
    });
    true
}

/// Sets how often a layer refetches, in seconds, or stops it refreshing when
/// given nothing. Only a layer the globe fetched itself can refresh; one given
/// as text has nowhere to fetch from, and reports `refreshSeconds: null`
/// whatever is asked here.
/// Sets how a layer reads the heights in its positions. Takes the same
/// `altitudeMode`, `altitudeScale` and `extrude` fields [`add_overlay`] does;
/// anything left out goes back to its default.
///
/// The layer is rebuilt where it stands — nothing is refetched, and a pinned
/// feature stays pinned.
#[wasm_bindgen(js_name = setOverlayAltitude)]
pub fn set_overlay_altitude(id: String, altitude: JsValue) -> bool {
    let Some(altitude) = from_js::<AltitudeOptions>(&altitude) else {
        return false;
    };
    api::send(GlobeCommand::SetOverlayAltitude {
        id,
        altitude: altitude.resolve(),
    });
    true
}

#[wasm_bindgen(js_name = setOverlayRefresh)]
pub fn set_overlay_refresh(id: String, seconds: Option<f32>) {
    api::send(GlobeCommand::SetOverlayRefresh { id, seconds });
}

/// Refetches now, whatever the period says.
#[wasm_bindgen(js_name = refreshOverlay)]
pub fn refresh_overlay(id: String) {
    api::send(GlobeCommand::RefreshOverlay(id));
}

/// Whether overlays are drawn at all. Off, every layer stays loaded and simply
/// stops being drawn, so switching back is instant.
#[wasm_bindgen(js_name = setOverlaysEnabled)]
pub fn set_overlays_enabled(enabled: bool) {
    api::send(GlobeCommand::SetOverlaysEnabled(enabled));
}

// ---------------------------------------------------------------------------
// Ephemerides
// ---------------------------------------------------------------------------

/// The options an ephemeris layer is added with, as a plain JavaScript object.
///
/// Exactly one of `url` and `text` says where the OMM JSON comes from, on the
/// same terms an overlay has: a URL is the globe's to fetch and refetch, and
/// `text` is a catalogue the embedder already has.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct EphemerisOptions {
    url: Option<String>,
    text: Option<String>,
    label: Option<String>,
    /// Seconds between refetches. Omit or `null` to fetch once. Floored at five
    /// minutes — a catalogue is regenerated a few times a day.
    refresh_seconds: Option<f32>,
    visible: Option<bool>,
    /// Catalogue numbers to draw, or omit for everything the document holds,
    /// down to the budget the state stream reports as `maxTracked`.
    select: Option<Vec<u64>>,
    /// Whether orbit arcs are drawn at all.
    trails: Option<bool>,
    #[serde(flatten)]
    trail: TrailOptions,
    #[serde(flatten)]
    style: StyleOptions,
}

/// How far an orbit arc runs either side of now, and how finely it is drawn.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TrailOptions {
    /// How far ahead the arc runs, in orbits. `0.5` is half a revolution.
    leading_orbits: Option<f32>,
    /// How far behind it runs, in orbits.
    trailing_orbits: Option<f32>,
    /// Samples per whole orbit. A ceiling: a layer with many trails divides one
    /// budget among them, so this is what a lightly loaded layer gets.
    trail_samples: Option<u32>,
    /// `"track"` for where the satellite went over the ground — a corkscrew in
    /// ECEF, and the figure of eight a navigation constellation is usually
    /// drawn as — or `"orbit"` for the orbit itself, which is the same curve in
    /// both frames and so does not move when the frame is switched. Only ECEF
    /// tells them apart. `"track"` if omitted or unrecognised.
    trail_path: Option<String>,
}

impl TrailOptions {
    fn resolve(&self) -> TrailWindow {
        let defaults = TrailWindow::default();
        TrailWindow {
            leading_orbits: self.leading_orbits.unwrap_or(defaults.leading_orbits),
            trailing_orbits: self.trailing_orbits.unwrap_or(defaults.trailing_orbits),
            samples: self.trail_samples.unwrap_or(defaults.samples),
            path: self
                .trail_path
                .as_deref()
                .and_then(TrailPath::from_id)
                .unwrap_or(defaults.path),
        }
    }
}

/// Puts an ephemeris layer up, or replaces the one already under this id.
///
/// The document is [OMM] JSON — an array of orbit mean-element records, which
/// is what every current catalogue publishes and the successor to the two-line
/// element set. Each record becomes an SGP4 propagator; the globe evaluates
/// them against its own simulated clock, so the constellation obeys
/// `setTimeScale`, `setSunPaused` and `setClock` like everything else.
///
/// ```js
/// addEphemeris("stations", {
///   url: "https://celestrak.org/NORAD/elements/gp.php?GROUP=stations&FORMAT=json",
///   label: "Crewed stations",
/// });
/// // One satellite, a whole orbit of arc either side of it, drawn finely:
/// addEphemeris("iss", { url, select: [25544], leadingOrbits: 1, trailingOrbits: 1,
///                       trailSamples: 512 });
/// // A catalogue the app already has, markers only:
/// addEphemeris("mine", { text: await file.text(), trails: false });
/// ```
///
/// Switch to the ECI frame to see an orbit as the closed ellipse it is; in
/// ECEF the same arc is the corkscrew a ground track is, because the Earth
/// turns underneath it.
///
/// Returns whether the options could be read. A layer that fails to *load*
/// still returns `true` — the failure arrives on the state stream, with the
/// reason.
///
/// [OMM]: https://public.ccsds.org/Pubs/502x0b3e1.pdf
#[wasm_bindgen(js_name = addEphemeris)]
pub fn add_ephemeris(id: String, options: JsValue) -> bool {
    let Some(options) = from_js::<EphemerisOptions>(&options) else {
        return false;
    };
    let source = match (options.url, options.text) {
        (Some(url), _) => OverlaySource::Url(url),
        (None, Some(text)) => OverlaySource::Text(text),
        (None, None) => return false,
    };

    api::send(GlobeCommand::AddEphemeris(EphemerisRequest {
        id,
        label: options.label.unwrap_or_default(),
        source,
        style: options.style.resolve_or(crate::api::ephemeris_style()),
        trail: options.trail.resolve(),
        trails: options.trails.unwrap_or(true),
        selection: options.select.map_or(Selection::All, Selection::only),
        refresh_seconds: options.refresh_seconds,
        visible: options.visible.unwrap_or(true),
    }));
    true
}

#[wasm_bindgen(js_name = removeEphemeris)]
pub fn remove_ephemeris(id: String) {
    api::send(GlobeCommand::RemoveEphemeris(id));
}

#[wasm_bindgen(js_name = setEphemerisVisible)]
pub fn set_ephemeris_visible(id: String, visible: bool) {
    api::send(GlobeCommand::SetEphemerisVisible { id, visible });
}

/// Recolours a layer without repropagating it. Takes the same colour and size
/// fields an overlay does; `fillColor` is ignored, because an ephemeris has no
/// rings in it.
#[wasm_bindgen(js_name = setEphemerisStyle)]
pub fn set_ephemeris_style(id: String, style: JsValue) -> bool {
    let Some(style) = from_js::<StyleOptions>(&style) else {
        return false;
    };
    api::send(GlobeCommand::SetEphemerisStyle {
        id,
        style: style.resolve_or(crate::api::ephemeris_style()),
    });
    true
}

/// Replaces which objects the layer draws, by catalogue number.
///
/// Pass `null` for everything the document holds, down to the budget. A
/// selection larger than that budget is drawn down to it in catalogue order,
/// and the layer reports `tracked` alongside `objects` so an interface can say
/// so.
#[wasm_bindgen(js_name = setEphemerisSelection)]
pub fn set_ephemeris_selection(id: String, norad_ids: Option<Vec<u32>>) {
    api::send(GlobeCommand::SetEphemerisSelection {
        id,
        // `u32` at the boundary, `u64` behind it: a catalogue number is nine
        // digits at most, and taking it as a `u64` would make every one of them
        // a `BigInt` in JavaScript for no reason.
        selection: norad_ids.map_or(Selection::All, |ids| {
            Selection::only(ids.into_iter().map(u64::from))
        }),
    });
}

/// Draws one object, or stops drawing it. `noradId` is what
/// `ephemerisObjects(id)` lists.
#[wasm_bindgen(js_name = selectSatellite)]
pub fn select_satellite(id: String, norad_id: u32, selected: bool) {
    api::send(GlobeCommand::SelectSatellite {
        id,
        norad_id: u64::from(norad_id),
        selected,
    });
}

/// Draws one object's orbit arc, or stops drawing it.
#[wasm_bindgen(js_name = setSatelliteTrail)]
pub fn set_satellite_trail(id: String, norad_id: u32, trail: bool) {
    api::send(GlobeCommand::SetSatelliteTrail {
        id,
        norad_id: u64::from(norad_id),
        trail,
    });
}

/// Whether the layer draws orbit arcs at all. Off, the arcs it has are hidden
/// rather than discarded, so switching back is instant.
#[wasm_bindgen(js_name = setEphemerisTrails)]
pub fn set_ephemeris_trails(id: String, trails: bool) {
    api::send(GlobeCommand::SetEphemerisTrails { id, trails });
}

/// How far the arcs run either side of now, how finely, and what they are a
/// picture of. Takes `leadingOrbits`, `trailingOrbits`, `trailSamples` and
/// `trailPath`; anything left out goes back to its default.
///
/// `trailPath` is `"track"` or `"orbit"`, and it is the answer to why an arc
/// changes shape when the frame is switched. A track is where the satellite
/// passed over the ground, so each sample is placed against the rotation at
/// *its own* moment — which is what makes it a corkscrew in ECEF, and the
/// figure of eight a navigation constellation is usually drawn as. An orbit is
/// the path itself, every sample placed against the *current* rotation, so it
/// is the same curve in ECEF and ECI and switching between them does not move
/// it. Only ECEF can tell the two apart.
#[wasm_bindgen(js_name = setEphemerisTrail)]
pub fn set_ephemeris_trail(id: String, trail: JsValue) -> bool {
    let Some(trail) = from_js::<TrailOptions>(&trail) else {
        return false;
    };
    api::send(GlobeCommand::SetEphemerisTrail {
        id,
        trail: trail.resolve(),
    });
    true
}

#[wasm_bindgen(js_name = setEphemerisRefresh)]
pub fn set_ephemeris_refresh(id: String, seconds: Option<f32>) {
    api::send(GlobeCommand::SetEphemerisRefresh { id, seconds });
}

/// Refetches the catalogue now, whatever the period says.
#[wasm_bindgen(js_name = refreshEphemeris)]
pub fn refresh_ephemeris(id: String) {
    api::send(GlobeCommand::RefreshEphemeris(id));
}

/// Whether ephemerides are drawn at all. Off, every layer stays loaded and
/// stops being propagated — which is where the cost of one goes.
#[wasm_bindgen(js_name = setEphemeridesEnabled)]
pub fn set_ephemerides_enabled(enabled: bool) {
    api::send(GlobeCommand::SetEphemeridesEnabled(enabled));
}

/// Lists what one ephemeris layer holds, with what is drawn and what is
/// trailed.
///
/// ```js
/// ephemerisObjects("stations");
/// // [{noradId, name, internationalDesignator, epochUnixSeconds,
/// //   periodMinutes, inclinationDeg, eccentricity, selected, trail}, ...]
/// ```
///
/// Pulled rather than streamed: a catalogue can be twelve thousand rows and the
/// state snapshot goes out ten times a second. What the stream carries is the
/// layer's `revision`, which changes whenever this list would — a catalogue
/// landing, a selection moving — so an interface pulls again when it does.
///
/// `epochUnixSeconds` is on the same clock as `sun.unixSeconds`, so subtracting
/// the two says how stale the elements are; SGP4 is a fit around its epoch and
/// drifts roughly a kilometre a day away from it in low Earth orbit.
///
/// Returns `null` for a layer that is not up, or has not loaded yet.
#[wasm_bindgen(js_name = ephemerisObjects)]
pub fn ephemeris_objects(id: &str) -> JsValue {
    let Some(published) = crate::ephemeris::catalogue_of(id) else {
        return JsValue::NULL;
    };
    to_js(&crate::ephemeris::describe_objects(&published))
}

/// An ephemeris layer's drawn geometry as typed arrays viewing the module's own
/// memory, exactly as [`overlay_geometry`] hands out an overlay's.
///
/// ```js
/// { points: {coords, features}, lines: {coords, offsets, features} }
/// ```
///
/// `points` is where every drawn object is at the moment on the globe's clock;
/// `lines` is the orbit arcs. A shape's `features` entry indexes the layer's own
/// feature list, and each feature's id is the object's catalogue number as a
/// string — so `ephemerisObjects` is how a coordinate here is given a name.
///
/// **The coordinates are in whichever frame the scene is drawn in.** In ECEF
/// they are the ordinary latitude and longitude under the satellite; in ECI the
/// second component is a right ascension rather than a longitude. Height is
/// metres above a sphere of mean Earth radius — geocentric, not geodetic.
///
/// The same rule as `overlayGeometry` applies and applies harder: these buffers
/// are rebuilt *every frame*, so read them synchronously and `slice()` anything
/// worth keeping.
///
/// Returns `null` for a layer that is not up, or has not drawn yet.
#[wasm_bindgen(js_name = ephemerisGeometry)]
pub fn ephemeris_geometry(id: &str) -> JsValue {
    let Some(geometry) = crate::ephemeris::geometry_of(id) else {
        return JsValue::NULL;
    };

    // SAFETY: as `overlay_geometry` — every view is built from a slice of
    // `geometry`, which the `Arc` holds alive for the whole of this function,
    // and nothing allocates between building the views and returning them.
    let points = object(&[
        ("coords", unsafe {
            view_f64(geometry.positions.point_coords())
        }),
        ("features", unsafe {
            view_u32(geometry.positions.point_owners())
        }),
    ]);
    let lines = object(&[
        ("coords", unsafe { view_f64(geometry.trails.line_coords()) }),
        ("offsets", unsafe {
            view_i32(geometry.trails.line_offsets())
        }),
        ("features", unsafe {
            view_u32(geometry.trails.line_owners())
        }),
    ]);

    object(&[
        (
            "features",
            JsValue::from(geometry.positions.feature_count() as u32),
        ),
        ("points", points),
        ("lines", lines),
    ])
}

// ---------------------------------------------------------------------------
// Geometry, without a copy
// ---------------------------------------------------------------------------

/// Hands back a layer's geometry as typed arrays viewing the module's own
/// memory — the GeoArrow buffers the globe is drawing from, not a copy of them.
///
/// The shape returned is [GeoArrow], which is what [`crate::features`] stores:
///
/// ```js
/// {
///   features: 1234,
///   points:   { coords, features },
///   lines:    { coords, offsets, features },
///   polygons: { coords, ringOffsets, offsets, features },
/// }
/// ```
///
/// `coords` is a `Float64Array` of `longitude, latitude, height` repeated —
/// degrees on WGS 84, metres above the surface, exactly as the document wrote
/// them and at full precision. `offsets` is an `Int32Array` one longer than the
/// number of shapes, giving where each one starts and ends *in coordinates*
/// rather than in doubles: line `i` runs from `offsets[i]` to `offsets[i + 1]`.
/// A polygon's `offsets` index into `ringOffsets`, and `ringOffsets` index into
/// `coords`, so ring `r` of polygon `i` is `ringOffsets[offsets[i] + r]`
/// onward, with the outer ring first and the holes after it. `features` is a
/// `Uint32Array` saying which feature each shape belongs to — the same index
/// `pinFeature` takes and `overlays.hovered.index` reports.
///
/// Rings are stored *open*: the repeated closing position GeoJSON requires has
/// been dropped, so the last edge of a ring is the one back to its first point.
///
/// **These arrays are windows onto live memory, and there are two ways to lose
/// them.** The module's memory may be resized by anything that allocates, which
/// detaches every view onto it; and the layer may be refreshed or removed,
/// which frees the buffers underneath. So read them *synchronously*, before
/// calling anything else on the globe — and to keep the data, copy it first:
/// `geometry.points.coords.slice()` returns an ordinary array that owns its
/// bytes and is safe to keep, to post to a worker, or to transfer.
///
/// Returns `null` for a layer that is not up, or is still loading.
///
/// [GeoArrow]: https://geoarrow.org
#[wasm_bindgen(js_name = overlayGeometry)]
pub fn overlay_geometry(id: &str) -> JsValue {
    let Some(document) = crate::overlays::geometry_of(id) else {
        return JsValue::NULL;
    };

    // SAFETY: every view below is built from a slice of `document`, which is
    // held alive by the `Arc` for the whole of this function, and is handed to
    // the caller without anything in between being allocated. Past the return
    // the guarantee is the caller's to keep, which is what the note above is
    // for — there is no way to express "do not allocate" in a JavaScript type.
    let points = object(&[
        ("coords", unsafe { view_f64(document.point_coords()) }),
        ("features", unsafe { view_u32(document.point_owners()) }),
    ]);
    let lines = object(&[
        ("coords", unsafe { view_f64(document.line_coords()) }),
        ("offsets", unsafe { view_i32(document.line_offsets()) }),
        ("features", unsafe { view_u32(document.line_owners()) }),
    ]);
    let polygons = object(&[
        ("coords", unsafe { view_f64(document.polygon_coords()) }),
        ("ringOffsets", unsafe { view_i32(document.ring_offsets()) }),
        ("offsets", unsafe { view_i32(document.polygon_offsets()) }),
        ("features", unsafe { view_u32(document.polygon_owners()) }),
    ]);

    object(&[
        ("features", JsValue::from(document.feature_count() as u32)),
        ("points", points),
        ("lines", lines),
        ("polygons", polygons),
    ])
}

/// Builds a plain JavaScript object out of named values.
fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let object = js_sys::Object::new();
    for (name, value) in fields {
        // Setting a fresh string key on a fresh object cannot fail.
        let _ = js_sys::Reflect::set(&object, &JsValue::from_str(name), value);
    }
    object.into()
}

/// # Safety
///
/// The returned array views this module's memory and is invalidated by anything
/// that resizes it, and by `slice` being dropped. See [`overlay_geometry`].
unsafe fn view_f64(slice: &[f64]) -> JsValue {
    unsafe { js_sys::Float64Array::view(slice) }.into()
}

/// # Safety
///
/// As [`view_f64`].
unsafe fn view_i32(slice: &[i32]) -> JsValue {
    unsafe { js_sys::Int32Array::view(slice) }.into()
}

/// # Safety
///
/// As [`view_f64`].
unsafe fn view_u32(slice: &[u32]) -> JsValue {
    unsafe { js_sys::Uint32Array::view(slice) }.into()
}

// ---------------------------------------------------------------------------
// Picking
// ---------------------------------------------------------------------------

/// Whether the cursor picks anything.
///
/// On, every state snapshot carries `overlays.hovered` — the feature under the
/// pointer, with its properties — and `ephemerides.hovered`, the satellite
/// under it, with its elements and where it is now; the globe draws a halo
/// around each. Off, none of that happens, and a pin already set stays set.
///
/// One switch for both, because they are one thing to whoever is pointing at
/// the globe. What they are not is one hit test: a feature is picked where it
/// stands on the ground, and a satellite where its marker was drawn, hundreds
/// of kilometres above it.
#[wasm_bindgen(js_name = setPickingEnabled)]
pub fn set_picking_enabled(enabled: bool) {
    api::send(GlobeCommand::SetPickingEnabled(enabled));
}

/// Keeps a feature selected, whatever the cursor does afterwards.
///
/// This is what a click is made of: the globe says what is under the pointer,
/// and deciding that one of those is *the* selection is the interface's to do.
/// `layer` is an overlay's id and `index` a feature's position in it, both as
/// `overlays.hovered` reports them.
#[wasm_bindgen(js_name = pinFeature)]
pub fn pin_feature(layer: String, index: usize) {
    api::send(GlobeCommand::PinFeature { layer, index });
}

#[wasm_bindgen(js_name = clearPinnedFeature)]
pub fn clear_pinned_feature() {
    api::send(GlobeCommand::ClearPinnedFeature);
}

/// Keeps a satellite selected, whatever the cursor does afterwards.
///
/// The same idea as `pinFeature`, named the way every other satellite control
/// names an object: `layer` is an ephemeris layer's id and `noradId` a
/// catalogue number, both as `ephemerides.hovered` reports them. By catalogue
/// number rather than by position, so the pin holds when the layer refetches
/// and the document is renumbered.
#[wasm_bindgen(js_name = pinSatellite)]
pub fn pin_satellite(layer: String, norad_id: u32) {
    api::send(GlobeCommand::PinSatellite {
        layer,
        // `u32` at the boundary and `u64` behind it, as every other control
        // that names an object does it: taken as a `u64`, wasm-bindgen would
        // demand a `BigInt` from JavaScript, and the catalogue number in a
        // snapshot is an ordinary number.
        norad_id: u64::from(norad_id),
    });
}

#[wasm_bindgen(js_name = clearPinnedSatellite)]
pub fn clear_pinned_satellite() {
    api::send(GlobeCommand::ClearPinnedSatellite);
}

// ---------------------------------------------------------------------------
// Chrome and input
// ---------------------------------------------------------------------------

/// Whether the globe draws its own readout. An interface that draws its own
/// turns this off.
#[wasm_bindgen(js_name = setHudVisible)]
pub fn set_hud_visible(visible: bool) {
    api::send(GlobeCommand::SetHudVisible(visible));
}

/// Whether the globe draws its key list.
#[wasm_bindgen(js_name = setHelpVisible)]
pub fn set_help_visible(visible: bool) {
    api::send(GlobeCommand::SetHelpVisible(visible));
}

/// Whether the globe's own keyboard shortcuts are live. An embedder that binds
/// its own keys turns these off so the two do not both fire.
#[wasm_bindgen(js_name = setKeyboardEnabled)]
pub fn set_keyboard_enabled(enabled: bool) {
    api::send(GlobeCommand::SetKeyboardEnabled(enabled));
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

// The listener the globe pushes state to. A `thread_local` rather than a
// `static`: a `js_sys::Function` is neither `Send` nor `Sync`, and on this
// target there is only ever the one thread it could be called from anyway.
thread_local! {
    static LISTENER: std::cell::RefCell<Option<js_sys::Function>> =
        const { std::cell::RefCell::new(None) };
}

/// Registers the callback the globe reports its state to.
///
/// It fires about ten times a second, and immediately whenever something an
/// interface has a control for changes, so a toggle answers on the next frame
/// rather than on the next tick of the throttle. Only one listener is kept;
/// registering again replaces it.
#[wasm_bindgen(js_name = onState)]
pub fn on_state(callback: Option<js_sys::Function>) {
    LISTENER.with(|listener| *listener.borrow_mut() = callback);
}

/// Hands a snapshot to the registered listener, if there is one.
pub(crate) fn publish(state: &GlobeState) {
    LISTENER.with(|listener| {
        if let Some(callback) = listener.borrow().as_ref() {
            // A listener that throws is the embedder's problem, not the globe's:
            // swallowing it here keeps one bad frame from stopping the stream.
            let _ = callback.call1(&JsValue::NULL, &to_js(state));
        }
    });
}

/// Serializes through JSON, which is the one representation `serde` and
/// JavaScript both already agree on without another dependency.
fn to_js<T: serde::Serialize>(value: &T) -> JsValue {
    serde_json::to_string(value)
        .ok()
        .and_then(|json| js_sys::JSON::parse(&json).ok())
        .unwrap_or(JsValue::NULL)
}

/// Reads a JavaScript object back the same way, for the calls that take a bag
/// of options rather than a fixed argument list.
fn from_js<T: serde::de::DeserializeOwned>(value: &JsValue) -> Option<T> {
    // An absent argument is an empty options object, not a failure — every
    // field of one is optional.
    if value.is_undefined() || value.is_null() {
        return serde_json::from_str("{}").ok();
    }
    let json = js_sys::JSON::stringify(value).ok()?;
    serde_json::from_str(&String::from(json)).ok()
}
