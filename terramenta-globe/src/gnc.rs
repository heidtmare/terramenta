//! Both reference frames at once: the inertial triad, the Earth-fixed grid, and
//! one satellite drawn in each of them at the same time.
//!
//! Everywhere else in the globe a frame is a *choice*:
//! [`crate::frame::ReferenceFrame`] says which one world space is, and the scene
//! is drawn in it. That is the right answer for looking at the Earth and the
//! wrong one for understanding the relationship, because whichever frame you
//! pick, the other one is invisible — and the whole of guidance, navigation and
//! control lives in the step between the two.
//!
//! So this module draws both, in whichever frame world space happens to be, and
//! the step between them becomes something on screen rather than something to
//! take on trust:
//!
//! * the **inertial triad** — the vernal equinox, the axis 90° east of it, and
//!   the celestial equator as a hoop in space, in cyan;
//! * the **Earth-fixed triad and graticule** — the prime meridian, 90° east, the
//!   equator and a lat/lon grid, in amber, lying on the ground;
//! * the **sidereal angle** between the two, drawn as the arc it is, which is
//!   the one number that relates the frames and the only thing that changes
//!   when the clock runs;
//! * and one satellite's path, drawn **twice at once** — the smooth closed
//!   ellipse it is in the inertial frame, and the ground track the same
//!   satellite writes across the turning Earth.
//!
//! ## What the drawing is actually made of
//!
//! Three transforms and nothing else, which is the point worth taking away.
//!
//! **Rigid geometry gets a quaternion.** The inertial triad never changes shape:
//! it is one mesh, built once in inertial coordinates, carried into world space
//! by [`ReferenceFrame::inertial_to_world`] on its `Transform`. The graticule is
//! the same in Earth-fixed coordinates under
//! [`ReferenceFrame::earth_to_world`]. Switching frames does not rebuild either
//! of them — it swaps which of the two quaternions is the identity. That is
//! [`Anchor`], and it is five lines of code for the entire frame handling of
//! everything that holds still.
//!
//! **A ground track cannot have one.** Every point of it belongs to a different
//! moment, and the Earth turned between them, so there is no single rotation
//! that takes the inertial path to the Earth-fixed one. Each sample is
//! transformed by the sidereal angle of *its own* moment, on the way into the
//! mesh — and what comes out is a curve that is then rigid in the Earth-fixed
//! frame and rides the same quaternion as the grid under it. The corkscrew is
//! not a rendering effect: it is what a fixed inertial ellipse looks like after
//! a per-sample rotation that grows by a degree every four minutes.
//!
//! **A velocity is not a position.** Rotating a position between frames is the
//! rotation and nothing more. Rotating a velocity is the rotation *and* the
//! transport term `ω × r`, because the frame it is measured in is itself
//! turning — which is why a geostationary satellite moves at three kilometres a
//! second in one frame and stands still in the other. [`velocity_eci_to_ecef`]
//! is that one line, and the readout shows both speeds side by side so the
//! difference is a number rather than a claim.
//!
//! ## Two bases, kept apart on purpose
//!
//! The arithmetic here is done in the basis GNC software actually uses —
//! right-handed, `Z` the celestial pole, `X` the vernal equinox for ECI and the
//! prime meridian for ECEF — so that [`eci_to_ecef_dcm`] is the `R3(θ)` out of
//! any astrodynamics text and can be checked against one.
//!
//! The scene is not in that basis. Bevy is Y-up, and the globe puts `+Y` at the
//! north pole and `+Z` on the prime meridian (see [`crate::geo`]). That is a
//! relabelling of axes rather than a rotation, and it happens in exactly one
//! place — [`scene_from_canonical`] — so that no other function here has to hold
//! both conventions in its head at once. Mixing the two silently is the classic
//! way to get a frame conversion that is right to within a rotation, and the
//! test suite below pins the bridge between them.

use std::sync::Arc;

use bevy::camera::visibility::NoFrustumCulling;
use bevy::ecs::system::SystemParam;
use bevy::math::{DMat3, DQuat, DVec3};
use bevy::prelude::*;

use crate::api::keyboard_enabled;
use crate::ephemeris::EphemerisSettings;
use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::{EARTH_RADIUS_KM, LatLon};
use crate::globe::GLOBE_RADIUS;
use crate::omm::Catalogue;
use crate::overlays::{Paint, VectorMaterial, VectorMode, WorldPath, world_line_mesh};
use crate::sun::Sun;
use crate::tiles::MAX_TILE_RADIUS;

// ---------------------------------------------------------------------------
// The frame mathematics
// ---------------------------------------------------------------------------

/// Earth's inertial rotation rate about the pole, in radians per second.
///
/// Derived from the sidereal rate the rest of the globe turns at rather than
/// stated again, because two spellings of the Earth's rotation is one more than
/// any program should have: a drawing that turned at one rate and a velocity
/// that was corrected at another would disagree by a part in a thousand, which
/// is a metre a second at the equator and exactly the kind of error that never
/// looks wrong on screen.
pub const EARTH_RATE_RAD_S: f64 = crate::frame::SIDEREAL_DEGREES_PER_DAY
    * (std::f64::consts::PI / 180.0)
    / crate::frame::SECONDS_PER_DAY;

/// The direction cosine matrix that takes an inertial vector into the
/// Earth-fixed frame: the classical `R3(θ)`, with `θ` the Greenwich mean
/// sidereal angle.
///
/// ```text
///            ⎡  cos θ   sin θ   0 ⎤
/// R3(θ)  =   ⎢ -sin θ   cos θ   0 ⎥        r_ecef = R3(θ) · r_eci
///            ⎣   0        0     1 ⎦
/// ```
///
/// Note the sign, which is the single most common way to get this wrong. This
/// is a *frame* rotation: the axes turn east by `θ` and the vector stands still,
/// so its components go the other way. Rotating the vector itself by `θ` gives
/// the transpose, and a program that mixes the two is correct only at `θ = 0`
/// and at noon on the day nobody tests.
pub fn eci_to_ecef_dcm(gmst_rad: f64) -> DMat3 {
    let (sin, cos) = gmst_rad.sin_cos();
    // Column-major: these are the columns, so the rows read as written above.
    DMat3::from_cols(
        DVec3::new(cos, -sin, 0.0),
        DVec3::new(sin, cos, 0.0),
        DVec3::Z,
    )
}

/// The same rotation as a unit quaternion.
///
/// Identical in effect to [`eci_to_ecef_dcm`] and cheaper to compose, invert and
/// interpolate, which is why flight software carries attitude as a quaternion
/// and builds a matrix only where one is needed. A rotation *of the frame* by
/// `+θ` is a rotation *of a vector* by `-θ`, so that is the angle that goes in.
pub fn eci_to_ecef_quat(gmst_rad: f64) -> DQuat {
    DQuat::from_rotation_z(-gmst_rad)
}

/// The inverse: Earth-fixed back to inertial.
pub fn ecef_to_eci_quat(gmst_rad: f64) -> DQuat {
    eci_to_ecef_quat(gmst_rad).conjugate()
}

/// An inertial velocity, in the Earth-fixed frame.
///
/// The transport theorem, which is the part of a frame conversion that a
/// rotation alone does not do:
///
/// ```text
/// v_ecef = R3(θ) · ( v_eci − ω × r_eci )
/// ```
///
/// The subtracted term is the velocity the rotating frame itself has at that
/// point — 465 m/s at the equator, nothing at the poles. Leave it out and every
/// ground-relative speed is wrong by up to half a kilometre a second, and a
/// geostationary satellite appears to be travelling at three.
///
/// Both vectors are in the canonical basis, in kilometres and kilometres per
/// second.
pub fn velocity_eci_to_ecef(position_eci: DVec3, velocity_eci: DVec3, gmst_rad: f64) -> DVec3 {
    let omega = DVec3::new(0.0, 0.0, EARTH_RATE_RAD_S);
    eci_to_ecef_dcm(gmst_rad) * (velocity_eci - omega.cross(position_eci))
}

/// The canonical basis, relabelled into the scene's.
///
/// Not a rotation — a permutation of axes. The globe is Bevy's right-handed
/// Y-up space with `+Y` at the north pole, `+Z` on the prime meridian and `+X`
/// at 90° east; the canonical basis is `Z` up the pole and `X` out along the
/// reference direction. So `(x, y, z)` becomes `(y, z, x)`, which is a cyclic
/// swap and therefore still right-handed — a drawing that came out mirrored
/// would be a sign error somewhere else, not here.
///
/// Scaled on the way through, because the canonical side is in kilometres and
/// the scene is in Earth radii.
pub fn scene_from_canonical(canonical: DVec3) -> Vec3 {
    Vec3::new(canonical.y as f32, canonical.z as f32, canonical.x as f32)
}

/// Where a canonical Earth-fixed position stands on the globe, and how far above
/// it.
///
/// Geocentric, like everything else the globe places: the radius is measured
/// against the sphere that is actually drawn rather than against the WGS 84
/// ellipsoid, for the reasons [`crate::ephemeris`] sets out.
pub fn subpoint(position_ecef_km: DVec3) -> (LatLon, f64) {
    let radius = position_ecef_km.length();
    if radius <= 0.0 {
        return (LatLon::new(0.0, 0.0), 0.0);
    }
    let latitude = (position_ecef_km.z / radius).clamp(-1.0, 1.0).asin();
    let longitude = position_ecef_km.y.atan2(position_ecef_km.x);
    (
        LatLon::new(latitude.to_degrees() as f32, longitude.to_degrees() as f32),
        radius - f64::from(EARTH_RADIUS_KM),
    )
}

// ---------------------------------------------------------------------------
// What the drawing looks like
// ---------------------------------------------------------------------------

/// Just clear of the imagery, which stands a little proud of the sphere at a
/// tile's corners — the same clearance every other drape on the globe uses.
const SURFACE_RADIUS: f32 = MAX_TILE_RADIUS + GLOBE_RADIUS * 3.0e-4;

/// Where an axis starts. Inside an opaque planet an axis is not drawn so much as
/// hidden, so the ray begins at the ground it points out of.
const AXIS_INNER: f32 = SURFACE_RADIUS;
/// How far the inertial triad reaches, in Earth radii.
const ECI_AXIS_OUTER: f32 = 1.85;
/// And the Earth-fixed one, drawn shorter so the two are told apart at a glance
/// even where they briefly overlap — which is once a day, when the prime
/// meridian passes the equinox.
const ECEF_AXIS_OUTER: f32 = 1.45;
/// The tick across the end of an axis, as a fraction of an Earth radius.
const AXIS_TIP: f32 = 0.055;
/// Where the celestial equator hoop is drawn: outside the globe, so it reads as
/// a ring in space rather than as another line on the ground.
const ECI_RING_RADIUS: f32 = 1.62;
/// Where the arc measuring the sidereal angle is drawn, between the two.
const SIDEREAL_RADIUS: f32 = 1.22;

/// Cyan is inertial and amber is Earth-fixed, throughout — the axes, the rings
/// and the two satellite paths. That pairing is the legend: anything cyan on
/// screen is fixed to the stars, anything amber is fixed to the ground.
const ECI_PRIMARY: Srgba = Srgba::new(0.31, 0.85, 1.0, 0.95);
const ECI_SECONDARY: Srgba = Srgba::new(0.20, 0.58, 0.74, 0.8);
const ECEF_PRIMARY: Srgba = Srgba::new(1.0, 0.70, 0.27, 0.95);
const ECEF_SECONDARY: Srgba = Srgba::new(0.76, 0.48, 0.15, 0.8);
/// The pole is the same axis in both frames — this model has no precession,
/// nutation or polar motion in it — so it is drawn once, in neither colour,
/// rather than as two lines lying on each other pretending to be two facts.
const POLE_COLOR: Srgba = Srgba::new(0.88, 0.93, 1.0, 0.9);
const GRATICULE_COLOR: Srgba = Srgba::new(0.62, 0.76, 0.92, 0.32);
const SIDEREAL_COLOR: Srgba = Srgba::new(1.0, 1.0, 1.0, 0.55);
const NADIR_COLOR: Srgba = Srgba::new(1.0, 1.0, 1.0, 0.45);

const AXIS_WIDTH_PX: f32 = 2.4;
const RING_WIDTH_PX: f32 = 1.6;
const GRATICULE_WIDTH_PX: f32 = 1.1;
const TRACK_WIDTH_PX: f32 = 2.2;
const NADIR_WIDTH_PX: f32 = 1.2;

/// Points around a full circle. A great circle seen edge-on from low orbit is
/// the worst case, and this is where it stops reading as a polygon.
const RING_SAMPLES: usize = 256;
/// Degrees between points along a graticule line.
const GRATICULE_STEP_DEGREES: f32 = 2.0;
/// Points along the inertial orbit, which is a smooth conic and needs few.
const ORBIT_SAMPLES: u32 = 384;
/// And along the ground track, which is not, and needs more.
const GROUND_SAMPLES: u32 = 640;

/// How finely the graticule may be asked for, in degrees of spacing.
pub const MIN_GRATICULE_STEP: f32 = 5.0;
pub const MAX_GRATICULE_STEP: f32 = 45.0;
/// How much orbit the two paths may span, each side of now, in revolutions.
pub const MIN_TRACK_ORBITS: f32 = 0.05;
pub const MAX_TRACK_ORBITS: f32 = 2.0;

/// How often the two satellite paths are rebuilt, in seconds of real time.
///
/// They are not rebuilt every frame for the same reason an ephemeris trail is
/// not: the curve through where a satellite has been changes far more slowly
/// than the satellite moves along it. Ten times a second is past the point
/// where a moving clock shows any stepping.
const TRACK_INTERVAL_SECONDS: f32 = 0.1;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// The satellite the two paths are drawn for, named the way every other command
/// names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SatelliteFocus {
    /// The ephemeris layer's id.
    pub layer: String,
    pub norad_id: u64,
}

/// What the frame drawing shows.
#[derive(Resource, Debug, Clone)]
pub struct GncSettings {
    /// The master switch. Off, nothing here is drawn and nothing is propagated.
    pub enabled: bool,
    /// The inertial triad and the celestial equator.
    pub eci_axes: bool,
    /// The Earth-fixed triad, the equator and the prime meridian.
    pub ecef_axes: bool,
    /// The lat/lon grid on the ground.
    pub graticule: bool,
    /// Its spacing, in degrees.
    pub graticule_step_deg: f32,
    /// The arc measuring the sidereal angle between the two triads. Only drawn
    /// when both triads are, since it is the angle *between* them.
    pub sidereal: bool,
    /// The two satellite paths.
    pub track: bool,
    /// How much orbit each path spans, each side of now, in revolutions.
    pub track_orbits: f32,
    /// Which satellite they follow, or `None` to follow whichever one is
    /// pinned — so clicking a satellite is enough, and there is no second
    /// selection to get out of step with the first.
    pub focus: Option<SatelliteFocus>,
}

impl Default for GncSettings {
    fn default() -> Self {
        Self {
            // Off until asked for. It is an explanation drawn over the Earth,
            // and a globe that started with a grid and two sets of axes on it
            // would be explaining something nobody had asked about yet.
            enabled: false,
            eci_axes: true,
            ecef_axes: true,
            graticule: true,
            graticule_step_deg: 30.0,
            sidereal: true,
            track: true,
            // Three quarters of a revolution each way: a revolution and a half
            // of ground track, which is the least that shows the westward shift
            // between one pass and the next — the thing the picture is for.
            track_orbits: 0.75,
            focus: None,
        }
    }
}

impl GncSettings {
    pub fn set_graticule_step(&mut self, degrees: f32) {
        if degrees.is_finite() {
            self.graticule_step_deg = degrees.clamp(MIN_GRATICULE_STEP, MAX_GRATICULE_STEP);
        }
    }

    pub fn set_track_orbits(&mut self, orbits: f32) {
        if orbits.is_finite() {
            self.track_orbits = orbits.clamp(MIN_TRACK_ORBITS, MAX_TRACK_ORBITS);
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The frame drawing, as an embedder sees it — including the rotation itself,
/// in both of the forms flight software carries it in.
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GncState {
    pub enabled: bool,
    pub eci_axes: bool,
    pub ecef_axes: bool,
    pub graticule: bool,
    pub graticule_step_deg: f32,
    pub sidereal: bool,
    pub track: bool,
    pub track_orbits: f32,
    /// Greenwich mean sidereal time as an angle, in degrees east of the vernal
    /// equinox. This is the whole relationship between the two frames.
    pub gmst_deg: f64,
    /// Earth's rotation rate about the pole, in degrees per second.
    pub earth_rate_deg_s: f64,
    /// The ECI→ECEF rotation as a unit quaternion, `[x, y, z, w]`, in the
    /// canonical basis.
    pub quaternion: [f64; 4],
    /// The same rotation as a direction cosine matrix, row-major — the `R3(θ)`
    /// the quaternion above is the other spelling of.
    pub dcm: [[f64; 3]; 3],
    /// The satellite the paths follow, and where it is in both frames.
    pub focus: Option<FocusState>,
}

/// One satellite, resolved in both frames at the same moment.
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FocusState {
    pub layer: String,
    pub norad_id: u64,
    pub name: String,
    pub period_minutes: f64,
    /// Inertial position and velocity, canonical basis, km and km/s.
    pub position_eci_km: [f64; 3],
    pub velocity_eci_km_s: [f64; 3],
    /// The same state in the Earth-fixed frame — the position rotated, the
    /// velocity rotated *and* transported.
    pub position_ecef_km: [f64; 3],
    pub velocity_ecef_km_s: [f64; 3],
    /// The two speeds, which differ by the rotation of the frame and by nothing
    /// else. A geostationary satellite is the extreme case: about 3.07 km/s
    /// inertial and effectively zero over the ground.
    pub speed_eci_km_s: f64,
    pub speed_ecef_km_s: f64,
    /// Right ascension, degrees — where it is against the stars.
    pub right_ascension_deg: f64,
    /// Where it is over the Earth, and how far above it.
    pub subsatellite: LatLon,
    pub altitude_km: f64,
}

/// The active frame and the frame drawing, as one system parameter.
///
/// One rather than two because a system may take only sixteen of them, and
/// [`crate::api::publish_state`] already spans every controllable part of the
/// globe — the same squeeze [`crate::placemark::PlacemarkPicks`] answers, and
/// the same answer. The two belong together anyway: what the snapshot says
/// about the frames and what it says about the drawing of them are one subject.
#[derive(SystemParam)]
pub struct FrameReport<'w> {
    pub frame: Res<'w, ReferenceFrame>,
    settings: Res<'w, GncSettings>,
}

impl FrameReport<'_> {
    pub fn describe(&self, ephemerides: &EphemerisSettings, unix_seconds: f64) -> GncState {
        describe(&self.settings, ephemerides, unix_seconds)
    }
}

/// Describes the drawing for the state snapshot.
pub fn describe(
    settings: &GncSettings,
    ephemerides: &EphemerisSettings,
    unix_seconds: f64,
) -> GncState {
    let gmst = crate::frame::sidereal_radians(unix_seconds);
    let quaternion = eci_to_ecef_quat(gmst);
    let dcm = eci_to_ecef_dcm(gmst);
    GncState {
        enabled: settings.enabled,
        eci_axes: settings.eci_axes,
        ecef_axes: settings.ecef_axes,
        graticule: settings.graticule,
        graticule_step_deg: settings.graticule_step_deg,
        sidereal: settings.sidereal,
        track: settings.track,
        track_orbits: settings.track_orbits,
        gmst_deg: gmst.to_degrees(),
        earth_rate_deg_s: EARTH_RATE_RAD_S.to_degrees(),
        quaternion: [quaternion.x, quaternion.y, quaternion.z, quaternion.w],
        dcm: [
            [dcm.x_axis.x, dcm.y_axis.x, dcm.z_axis.x],
            [dcm.x_axis.y, dcm.y_axis.y, dcm.z_axis.y],
            [dcm.x_axis.z, dcm.y_axis.z, dcm.z_axis.z],
        ],
        focus: focus_state(settings, ephemerides, unix_seconds, gmst),
    }
}

fn focus_state(
    settings: &GncSettings,
    ephemerides: &EphemerisSettings,
    unix_seconds: f64,
    gmst: f64,
) -> Option<FocusState> {
    let (layer, norad_id) = resolve_focus(settings, ephemerides)?;
    let (catalogue, index) = ephemerides.propagator(&layer, norad_id)?;
    let satellite = &catalogue.satellites[index];
    let (position, velocity) = satellite.state_teme(unix_seconds)?;
    let position_eci = DVec3::from_array(position);
    let velocity_eci = DVec3::from_array(velocity);
    let position_ecef = eci_to_ecef_dcm(gmst) * position_eci;
    let velocity_ecef = velocity_eci_to_ecef(position_eci, velocity_eci, gmst);
    let (subsatellite, altitude_km) = subpoint(position_ecef);

    Some(FocusState {
        layer,
        norad_id,
        name: satellite.name.clone(),
        period_minutes: satellite.period_minutes,
        position_eci_km: position_eci.to_array(),
        velocity_eci_km_s: velocity_eci.to_array(),
        position_ecef_km: position_ecef.to_array(),
        velocity_ecef_km_s: velocity_ecef.to_array(),
        speed_eci_km_s: velocity_eci.length(),
        speed_ecef_km_s: velocity_ecef.length(),
        right_ascension_deg: position_eci
            .y
            .atan2(position_eci.x)
            .to_degrees()
            .rem_euclid(360.0),
        subsatellite,
        altitude_km,
    })
}

/// Which satellite the paths follow: what was asked for, or what is pinned.
fn resolve_focus(settings: &GncSettings, ephemerides: &EphemerisSettings) -> Option<(String, u64)> {
    match settings.focus.as_ref() {
        Some(focus) => Some((focus.layer.clone(), focus.norad_id)),
        None => ephemerides.pinned_object(),
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Which frame a piece of the drawing is rigid in, and therefore which
/// quaternion carries it into world space.
///
/// This is the whole of the frame handling for everything that holds still. A
/// mesh is built once, in the coordinates of the frame it belongs to, and
/// [`carry_frames`] puts the right rotation on it every tick — so switching
/// frames costs one quaternion per entity rather than a rebuild, and the two
/// sets of geometry stay in step with each other by construction.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// Fixed to the stars: the inertial triad, the celestial equator, the orbit.
    Eci,
    /// Fixed to the ground: the graticule, the Earth-fixed triad, the ground
    /// track — which is a fixed curve in this frame, however complicated it
    /// looks, and that is exactly what makes it a *ground* track.
    Ecef,
    /// Neither, because it spans both: the sidereal arc, and the tether from a
    /// satellite down to the point it is over. Rebuilt every tick instead.
    World,
}

/// The pieces of the drawing, each its own entity so each can be rebuilt on its
/// own cadence and hidden on its own switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Part {
    EciAxes,
    EcefAxes,
    Graticule,
    Sidereal,
    Orbit,
    GroundTrack,
    Nadir,
}

impl Part {
    const ALL: [Self; 7] = [
        Self::EciAxes,
        Self::EcefAxes,
        Self::Graticule,
        Self::Sidereal,
        Self::Orbit,
        Self::GroundTrack,
        Self::Nadir,
    ];

    fn anchor(self) -> Anchor {
        match self {
            Self::EciAxes | Self::Orbit => Anchor::Eci,
            Self::EcefAxes | Self::Graticule | Self::GroundTrack => Anchor::Ecef,
            Self::Sidereal | Self::Nadir => Anchor::World,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::EciAxes => "ECI axes",
            Self::EcefAxes => "ECEF axes",
            Self::Graticule => "graticule",
            Self::Sidereal => "sidereal angle",
            Self::Orbit => "inertial orbit",
            Self::GroundTrack => "ground track",
            Self::Nadir => "nadir",
        }
    }

    /// Whether this piece is switched on at all.
    fn wanted(self, settings: &GncSettings) -> bool {
        match self {
            Self::EciAxes => settings.eci_axes,
            Self::EcefAxes => settings.ecef_axes,
            Self::Graticule => settings.graticule,
            // An angle between two things nobody is drawing is not an angle.
            Self::Sidereal => settings.sidereal && settings.eci_axes && settings.ecef_axes,
            Self::Orbit | Self::GroundTrack | Self::Nadir => settings.track,
        }
    }
}

/// One drawn piece: its entity and the assets behind it, reused rather than
/// respawned — the same bookkeeping [`crate::ephemeris`] does, for the same
/// reason.
#[derive(Default)]
struct Drawn {
    entity: Option<Entity>,
    mesh: Option<Handle<Mesh>>,
}

/// What is on screen, and what it was built from.
#[derive(Resource, Default)]
struct GncDrawn {
    parts: Vec<(Part, Drawn)>,
    /// The graticule spacing the grid on screen was built at, so it is rebuilt
    /// when it changes and not otherwise.
    graticule_step: f32,
    /// Real seconds since the two satellite paths were last built.
    since_track: f32,
    /// What they were built for: the moment, the window, and the object. Any of
    /// the three changing rebuilds them, however recently it was done.
    track_clock: f64,
    track_orbits: f32,
    track_focus: Option<(String, u64)>,
}

impl GncDrawn {
    fn part(&mut self, part: Part) -> &mut Drawn {
        if let Some(index) = self.parts.iter().position(|(held, _)| *held == part) {
            return &mut self.parts[index].1;
        }
        self.parts.push((part, Drawn::default()));
        &mut self.parts.last_mut().expect("just pushed").1
    }
}

pub struct GncPlugin;

impl Plugin for GncPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GncSettings>()
            .init_resource::<GncDrawn>()
            .add_systems(
                Update,
                gnc_controls
                    .in_set(FrameSet::Settle)
                    .run_if(keyboard_enabled),
            )
            .add_systems(
                Update,
                // Built first and then carried, so a mesh spawned this tick is
                // already under the right rotation on the frame it appears —
                // otherwise every piece flashes at the identity for one frame
                // whenever the drawing is switched on in ECI.
                (draw_gnc, carry_frames).chain().in_set(FrameSet::Apply),
            );
    }
}

fn gnc_controls(keys: Res<ButtonInput<KeyCode>>, mut settings: ResMut<GncSettings>) {
    if keys.just_pressed(KeyCode::KeyG) {
        settings.enabled = !settings.enabled;
    }
}

/// Puts the frame's rotation on everything rigid.
///
/// The entire per-frame cost of drawing two reference frames at once: one
/// quaternion each, and no geometry touched at all. In ECEF the Earth-fixed
/// pieces sit at the identity and the inertial ones turn backwards; in ECI it is
/// the other way round; and in both the angle left between them on screen is the
/// sidereal time.
fn carry_frames(frame: Res<ReferenceFrame>, mut drawn: Query<(&Anchor, &mut Transform)>) {
    let (eci, ecef) = (frame.inertial_to_world(), frame.earth_to_world());
    for (anchor, mut transform) in &mut drawn {
        transform.rotation = match anchor {
            Anchor::Eci => eci,
            Anchor::Ecef => ecef,
            // Already in world space: whatever built it did so this tick.
            Anchor::World => Quat::IDENTITY,
        };
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the drawing spans the clock, the frame, its own settings and the satellite layer it follows, and builds meshes from all four"
)]
fn draw_gnc(
    mut commands: Commands,
    time: Res<Time>,
    sun: Res<Sun>,
    frame: Res<ReferenceFrame>,
    settings: Res<GncSettings>,
    ephemerides: Res<EphemerisSettings>,
    drawn: ResMut<GncDrawn>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VectorMaterial>>,
) {
    let drawn = drawn.into_inner();
    drawn.since_track += time.delta_secs();

    if !settings.enabled {
        for (_, part) in &mut drawn.parts {
            if let Some(entity) = part.entity {
                commands.entity(entity).insert(Visibility::Hidden);
            }
        }
        return;
    }

    let now = sun.unix_seconds;
    let gmst = crate::frame::sidereal_radians(now);

    // The rigid pieces are rebuilt only when what they are made of changes: the
    // grid when its spacing does, the triads never. Their frame is a rotation on
    // the transform, not a property of the mesh — see `carry_frames`.
    let rebuild_graticule = drawn.graticule_step != settings.graticule_step_deg;
    if rebuild_graticule {
        drawn.graticule_step = settings.graticule_step_deg;
    }

    let focus = resolve_focus(&settings, &ephemerides);
    let rebuild_track = settings.track
        && (drawn.track_focus != focus
            || drawn.track_orbits != settings.track_orbits
            || (drawn.since_track >= TRACK_INTERVAL_SECONDS && drawn.track_clock != now));
    if rebuild_track {
        drawn.since_track = 0.0;
        drawn.track_clock = now;
        drawn.track_orbits = settings.track_orbits;
        drawn.track_focus = focus.clone();
    }

    // Propagated once for all three satellite pieces, because propagating it
    // three times would be three answers to one question. `Some(None)` is a
    // rebuild that found nothing to draw — no satellite pinned, or a propagator
    // that diverged — which is different from not having rebuilt at all.
    let rebuilt = rebuild_track.then(|| {
        let (layer, norad_id) = focus.as_ref()?;
        let (catalogue, index) = ephemerides.propagator(layer, *norad_id)?;
        satellite_paths(&catalogue, index, now, settings.track_orbits)
    });

    for part in Part::ALL {
        let wanted = part.wanted(&settings);
        // A mesh that is built once stays built while it is hidden, so switching
        // a piece back on is a visibility flip rather than a rebuild. `None`
        // here says exactly that: leave the buffers as they are.
        let mesh = match (wanted, part) {
            (false, _) => None,
            (true, Part::EciAxes) => unbuilt(drawn, part).then(eci_axes_mesh),
            (true, Part::EcefAxes) => unbuilt(drawn, part).then(ecef_axes_mesh),
            (true, Part::Graticule) => (rebuild_graticule || unbuilt(drawn, part))
                .then(|| graticule_mesh(settings.graticule_step_deg)),
            // The one piece that genuinely has to be rebuilt every tick: it is
            // the angle between the two frames, so it moves whenever either does.
            (true, Part::Sidereal) => Some(sidereal_mesh(&frame)),
            (true, Part::Orbit) => rebuilt
                .as_ref()
                .map(|paths| paths.as_ref().and_then(orbit_mesh)),
            (true, Part::GroundTrack) => rebuilt
                .as_ref()
                .map(|paths| paths.as_ref().and_then(ground_track_mesh)),
            // Follows the satellite, so it is rebuilt every tick like the arc.
            (true, Part::Nadir) => Some(nadir_mesh(&settings, &ephemerides, now, gmst, &frame)),
        };

        put(
            &mut commands,
            &mut meshes,
            &mut materials,
            drawn,
            part,
            wanted,
            mesh,
        );
    }
}

/// Whether a part has never been built, which is the other reason to build one.
fn unbuilt(drawn: &mut GncDrawn, part: Part) -> bool {
    drawn.part(part).mesh.is_none()
}

/// Spawns or refreshes one piece.
///
/// `mesh` is `None` for a piece whose geometry has not changed since it was last
/// built, and for one that is switched off — the two are the same instruction
/// here: leave the buffers alone. Visibility is set every tick regardless, which
/// is what makes a switch instant.
fn put(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<VectorMaterial>,
    drawn: &mut GncDrawn,
    part: Part,
    visible: bool,
    mesh: Option<Option<Mesh>>,
) {
    let anchor = part.anchor();
    let name = part.name();
    let held = drawn.part(part);

    // An empty build is a piece with nothing in it — no satellite pinned, a
    // propagator that diverged — which is hidden rather than left showing what
    // it held a moment ago.
    let empty = matches!(mesh, Some(None));
    let mesh = mesh.flatten();
    let visible = visible && !empty && (held.mesh.is_some() || mesh.is_some());

    if let Some(mesh) = mesh {
        match held.mesh.as_ref() {
            // Replacing the asset rather than writing into it: these meshes are
            // render-world only, so once one has been extracted the main world
            // holds a placeholder and reaching into it for an attribute panics.
            Some(handle) => {
                let _ = meshes.insert(handle.id(), mesh);
            }
            None => {
                let handle = meshes.add(mesh);
                // The colours are per vertex, so the material's own is only the
                // fallback for a path that asked for nothing — which none of
                // them do. Above the overlays, with the satellites.
                let material = materials.add(
                    VectorMaterial::new(
                        VectorMode::Line,
                        Paint {
                            color: POLE_COLOR,
                            size_px: AXIS_WIDTH_PX,
                        },
                    )
                    .above(),
                );
                let entity = commands
                    .spawn((
                        Name::new(format!("GNC ({name})")),
                        anchor,
                        Mesh3d(handle.clone()),
                        MeshMaterial3d(material),
                        Transform::IDENTITY,
                        // The ribbon is widened in the vertex shader, so the
                        // mesh's own bounds understate what is drawn — and an
                        // axis pointing away from the camera is a line whose
                        // bounds are a point.
                        NoFrustumCulling,
                    ))
                    .id();
                held.mesh = Some(handle);
                held.entity = Some(entity);
            }
        }
    }

    if let Some(entity) = held.entity {
        commands.entity(entity).insert(if visible {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

// ---------------------------------------------------------------------------
// The geometry
// ---------------------------------------------------------------------------

/// A ray out along a direction, with a cross tick at the end of it.
///
/// The tick is what makes a triad readable: three rays out of a sphere are
/// ambiguous about which end is the tip, and two short strokes across the far
/// end settle it without needing text on the screen.
fn axis_path(direction: Vec3, outer: f32, color: Srgba, width_px: f32) -> Vec<WorldPath> {
    let paint = Some(Paint {
        color,
        size_px: width_px,
    });
    let tip = direction * outer;
    // Any two directions perpendicular to the axis will do; `any_orthonormal_pair`
    // picks a pair without a special case for an axis that happens to be aligned
    // with one of the scene's own.
    let (across, up) = direction.any_orthonormal_pair();
    vec![
        WorldPath {
            points: vec![direction * AXIS_INNER, tip],
            paint,
        },
        WorldPath {
            points: vec![tip - across * AXIS_TIP, tip + across * AXIS_TIP],
            paint,
        },
        WorldPath {
            points: vec![tip - up * AXIS_TIP, tip + up * AXIS_TIP],
            paint,
        },
    ]
}

/// A circle of a given radius about the origin, in the plane whose normal is
/// `pole`.
fn ring_path(pole: Vec3, radius: f32, color: Srgba, width_px: f32) -> WorldPath {
    let (x, y) = pole.any_orthonormal_pair();
    let points = (0..=RING_SAMPLES)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / RING_SAMPLES as f32;
            let (sin, cos) = angle.sin_cos();
            (x * cos + y * sin) * radius
        })
        .collect();
    WorldPath {
        points,
        paint: Some(Paint {
            color,
            size_px: width_px,
        }),
    }
}

/// The inertial triad: the vernal equinox, the axis 90° east of it in right
/// ascension, the pole, and the celestial equator they lie in.
///
/// Built in inertial coordinates, which for a drawing means the scene basis with
/// no rotation applied — `+Z` is the equinox, because that is where the prime
/// meridian sits when the sidereal angle is zero and the two frames coincide.
fn eci_axes_mesh() -> Option<Mesh> {
    let mut paths = Vec::new();
    // X: the vernal equinox, the direction the whole inertial frame is defined
    // from — where the sun crosses the equator going north, which is a direction
    // in space rather than a place on Earth.
    paths.extend(axis_path(
        Vec3::Z,
        ECI_AXIS_OUTER,
        ECI_PRIMARY,
        AXIS_WIDTH_PX,
    ));
    // Y: completes the right-handed set, 90° east of the equinox.
    paths.extend(axis_path(
        Vec3::X,
        ECI_AXIS_OUTER * 0.86,
        ECI_SECONDARY,
        AXIS_WIDTH_PX,
    ));
    // Z: the celestial pole, which this model shares with the Earth-fixed frame
    // exactly — so it is drawn here, once, and left out of the other triad.
    paths.extend(axis_path(
        Vec3::Y,
        ECI_AXIS_OUTER,
        POLE_COLOR,
        AXIS_WIDTH_PX,
    ));
    paths.push(ring_path(
        Vec3::Y,
        ECI_RING_RADIUS,
        ECI_SECONDARY,
        RING_WIDTH_PX,
    ));
    world_line_mesh(&paths)
}

/// The Earth-fixed triad: the prime meridian at the equator, 90° east of it, the
/// equator itself and the prime meridian as a whole circle.
///
/// No pole: it is the same axis the inertial triad already drew.
fn ecef_axes_mesh() -> Option<Mesh> {
    let mut paths = Vec::new();
    // X: 0° N, 0° E — the intersection of the equator and the prime meridian,
    // which is what the Earth-fixed frame is defined from.
    paths.extend(axis_path(
        Vec3::Z,
        ECEF_AXIS_OUTER,
        ECEF_PRIMARY,
        AXIS_WIDTH_PX,
    ));
    // Y: 0° N, 90° E.
    paths.extend(axis_path(
        Vec3::X,
        ECEF_AXIS_OUTER * 0.86,
        ECEF_SECONDARY,
        AXIS_WIDTH_PX,
    ));
    // The equator and the prime meridian, on the ground where they belong: the
    // two lines the triad above is the skeleton of.
    paths.push(ring_path(
        Vec3::Y,
        SURFACE_RADIUS,
        ECEF_PRIMARY,
        RING_WIDTH_PX,
    ));
    paths.push(ring_path(
        Vec3::X,
        SURFACE_RADIUS,
        ECEF_SECONDARY,
        RING_WIDTH_PX,
    ));
    world_line_mesh(&paths)
}

/// The lat/lon grid, in Earth-fixed coordinates.
///
/// Meridians stop just short of the poles: a line running into the pole and out
/// the other side is one line rather than two, and the join is where a ribbon
/// built from directions has no direction to work with.
fn graticule_mesh(step_deg: f32) -> Option<Mesh> {
    let step = step_deg.clamp(MIN_GRATICULE_STEP, MAX_GRATICULE_STEP);
    let paint = Some(Paint {
        color: GRATICULE_COLOR,
        size_px: GRATICULE_WIDTH_PX,
    });
    let mut paths = Vec::new();

    let mut longitude = -180.0_f32;
    while longitude < 180.0 {
        let points = samples(-89.0, 89.0, GRATICULE_STEP_DEGREES)
            .map(|latitude| LatLon::new(latitude, longitude).to_direction() * SURFACE_RADIUS)
            .collect();
        paths.push(WorldPath { points, paint });
        longitude += step;
    }

    // From the equator outwards, symmetrically, so the grid is the same either
    // side of it whatever the spacing divides into 90 as — and so the equator
    // itself is always one of the lines, whatever the spacing.
    let mut offset = 0.0_f32;
    while offset < 90.0 {
        let mut parallels = vec![offset];
        if offset > 0.0 {
            parallels.push(-offset);
        }
        for latitude in parallels {
            let points = samples(-180.0, 180.0, GRATICULE_STEP_DEGREES)
                .map(|longitude| LatLon::new(latitude, longitude).to_direction() * SURFACE_RADIUS)
                .collect();
            paths.push(WorldPath { points, paint });
        }
        offset += step;
    }

    world_line_mesh(&paths)
}

/// Inclusive steps from `from` to `to`, at most `step` apart, landing exactly on
/// both ends.
fn samples(from: f32, to: f32, step: f32) -> impl Iterator<Item = f32> {
    let count = (((to - from).abs() / step).ceil() as usize).max(1);
    (0..=count).map(move |index| from + (to - from) * index as f32 / count as f32)
}

/// The sidereal angle, drawn as the arc it is: from the Earth-fixed reference
/// direction round to the inertial one, in the equatorial plane, with a spoke at
/// each end.
///
/// The one piece that is rebuilt every tick, and necessarily: it is not rigid in
/// either frame, because it is the *difference* between them. Watching it open
/// out from nothing to a full turn over a sidereal day is watching the only
/// quantity in this whole module that actually changes.
fn sidereal_mesh(frame: &ReferenceFrame) -> Option<Mesh> {
    let paint = Some(Paint {
        color: SIDEREAL_COLOR,
        size_px: RING_WIDTH_PX,
    });
    // The equinox, in world space, whichever frame that is. The prime meridian
    // is the same direction turned east by the sidereal angle — which is what
    // the angle means, and is why the arc can be swept out from one to the other
    // without asking where either of them is.
    let equinox = frame.sky_rotation();
    // The angle the renderer is actually turning the Earth by, rather than the
    // one recomputed from the clock: an arc drawn to measure the rotation has to
    // measure *that* rotation, not another one worked out to a different
    // precision a few microseconds later.
    let sweep = frame.earth_rotation();
    let steps = ((RING_SAMPLES as f32 * (sweep.abs() / std::f32::consts::TAU)).ceil() as usize)
        .clamp(2, RING_SAMPLES);

    let at = |turn: f32| Quat::from_rotation_y(equinox + turn) * Vec3::Z * SIDEREAL_RADIUS;
    let arc = (0..=steps)
        .map(|step| at(sweep * step as f32 / steps as f32))
        .collect();

    world_line_mesh(&[
        WorldPath { points: arc, paint },
        // The spokes: one out to the equinox, one out to the prime meridian, so
        // the arc is visibly *between* two things rather than floating.
        WorldPath {
            points: vec![at(0.0).normalize() * AXIS_INNER, at(0.0)],
            paint,
        },
        WorldPath {
            points: vec![at(sweep).normalize() * AXIS_INNER, at(sweep)],
            paint,
        },
    ])
}

/// One satellite, sampled over a window and reduced to the two curves this
/// module exists to put side by side.
struct SatellitePaths {
    /// The orbit in inertial coordinates, scene basis, ready to be carried by
    /// [`Anchor::Eci`]. Smooth, closed, and the same shape every revolution.
    orbit: Vec<Vec3>,
    /// The ground track in Earth-fixed coordinates, ready to be carried by
    /// [`Anchor::Ecef`]. The same samples, each turned by the sidereal angle of
    /// its own moment and dropped onto the surface.
    ground: Vec<Vec3>,
}

/// Propagates the window and builds both curves from the same samples.
///
/// The same samples, deliberately: if the two were propagated separately they
/// could differ, and the entire point of the picture is that they are one set of
/// states seen from two frames. So each moment is evaluated once, and then used
/// twice — unrotated for the inertial path, and rotated by
/// [`eci_to_ecef_dcm`] at *that moment's* sidereal angle for the ground track.
/// That per-sample rotation is the difference between a closed ellipse and a
/// corkscrew, and there is no rigid transform that does it.
fn satellite_paths(
    catalogue: &Arc<Catalogue>,
    index: usize,
    now: f64,
    orbits: f32,
) -> Option<SatellitePaths> {
    let satellite = catalogue.satellites.get(index)?;
    let span_minutes = f64::from(orbits) * 2.0 * satellite.period_minutes;
    if span_minutes <= 0.0 || !span_minutes.is_finite() {
        return None;
    }
    let start = now - f64::from(orbits) * satellite.period_minutes * 60.0;

    // The ground track is sampled more finely than the orbit because it is a
    // more complicated curve: the orbit is a conic and the track is that conic
    // plus a rotation, so it turns where the orbit does not.
    let steps = ORBIT_SAMPLES.max(GROUND_SAMPLES);
    let mut orbit = Vec::with_capacity(steps as usize + 1);
    let mut ground = Vec::with_capacity(steps as usize + 1);
    let keep_orbit = steps / ORBIT_SAMPLES.max(1);

    for step in 0..=steps {
        let at = start + span_minutes * 60.0 * f64::from(step) / f64::from(steps);
        // SGP4 diverges rather than failing, and the samples either side of a
        // divergence are not on any orbit — so the window is dropped whole
        // rather than drawn with a kink through the middle of it.
        let position = DVec3::from_array(satellite.position_teme_km(at)?);
        let gmst = crate::frame::sidereal_radians(at);

        if step % keep_orbit.max(1) == 0 || step == steps {
            orbit.push(scene_from_canonical(position / f64::from(EARTH_RADIUS_KM)));
        }
        // On the surface, not at altitude: a ground track is where the satellite
        // was *over*, which is the radial projection of where it was.
        let ecef = eci_to_ecef_dcm(gmst) * position;
        ground.push(scene_from_canonical(ecef.normalize_or_zero()) * SURFACE_RADIUS);
    }

    Some(SatellitePaths { orbit, ground })
}

fn orbit_mesh(paths: &SatellitePaths) -> Option<Mesh> {
    world_line_mesh(&[WorldPath {
        points: paths.orbit.clone(),
        paint: Some(Paint {
            color: ECI_PRIMARY,
            size_px: TRACK_WIDTH_PX,
        }),
    }])
}

fn ground_track_mesh(paths: &SatellitePaths) -> Option<Mesh> {
    world_line_mesh(&[WorldPath {
        points: paths.ground.clone(),
        paint: Some(Paint {
            color: ECEF_PRIMARY,
            size_px: TRACK_WIDTH_PX,
        }),
    }])
}

/// The line from where the satellite is to the point it is over.
///
/// Drawn in world space and rebuilt every tick, because it is the one piece that
/// joins the two frames at a single moment: the top of it belongs to the orbit
/// and the bottom of it to the ground track, and watching it sweep is what makes
/// the two curves visibly the same satellite.
fn nadir_mesh(
    settings: &GncSettings,
    ephemerides: &EphemerisSettings,
    now: f64,
    gmst: f64,
    frame: &ReferenceFrame,
) -> Option<Mesh> {
    let (layer, norad_id) = resolve_focus(settings, ephemerides)?;
    let (catalogue, index) = ephemerides.propagator(&layer, norad_id)?;
    let position = DVec3::from_array(catalogue.satellites[index].position_teme_km(now)?);

    // The satellite is one point in space; which frame it is *drawn* in is only
    // a question of which rotation is applied, and at this one moment the two
    // answers are the same place. Taking the inertial route because that is the
    // frame SGP4 handed it over in.
    let satellite =
        frame.inertial_to_world() * scene_from_canonical(position / f64::from(EARTH_RADIUS_KM));
    let ecef = eci_to_ecef_dcm(gmst) * position;
    let subpoint =
        frame.earth_to_world() * scene_from_canonical(ecef.normalize_or_zero()) * SURFACE_RADIUS;

    world_line_mesh(&[WorldPath {
        points: vec![subpoint, satellite],
        paint: Some(Paint {
            color: NADIR_COLOR,
            size_px: NADIR_WIDTH_PX,
        }),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handful of angles including the two that hide sign errors: zero, and a
    /// right angle.
    const ANGLES: [f64; 5] = [0.0, 0.3, std::f64::consts::FRAC_PI_2, 2.9, 5.7];

    fn probes() -> [DVec3; 5] {
        [
            DVec3::X,
            DVec3::Y,
            DVec3::Z,
            DVec3::new(6878.0, 0.0, 0.0),
            DVec3::new(-1200.0, 4300.0, 5100.0),
        ]
    }

    #[test]
    fn the_dcm_is_a_rotation() {
        for angle in ANGLES {
            let dcm = eci_to_ecef_dcm(angle);
            // Orthonormal with a determinant of one: a rotation, not a
            // reflection and not a scaling.
            assert!(
                (dcm.determinant() - 1.0).abs() < 1.0e-12,
                "{angle}: {}",
                dcm.determinant()
            );
            let identity = dcm * dcm.transpose();
            assert!(
                (identity - DMat3::IDENTITY)
                    .to_cols_array()
                    .iter()
                    .all(|v| v.abs() < 1.0e-12),
                "{angle}: {identity:?}"
            );
        }
    }

    #[test]
    fn the_quaternion_and_the_matrix_are_the_same_rotation() {
        for angle in ANGLES {
            let (dcm, quaternion) = (eci_to_ecef_dcm(angle), eci_to_ecef_quat(angle));
            for probe in probes() {
                let difference = (dcm * probe) - (quaternion * probe);
                assert!(
                    difference.length() < 1.0e-9,
                    "{angle} on {probe}: {difference}"
                );
            }
        }
    }

    #[test]
    fn a_frame_rotation_is_the_vector_rotation_backwards() {
        // The sign trap, pinned: turning the axes east by the sidereal angle is
        // turning a vector's components west by it.
        for angle in ANGLES {
            let difference = eci_to_ecef_dcm(angle) - DMat3::from_rotation_z(-angle);
            assert!(
                difference.to_cols_array().iter().all(|v| v.abs() < 1.0e-12),
                "{angle}: {difference:?}"
            );
        }
    }

    #[test]
    fn the_frames_meet_at_the_equinox() {
        // At a sidereal angle of zero the prime meridian is under the vernal
        // equinox, so the two frames are the same frame.
        for probe in probes() {
            assert!((eci_to_ecef_dcm(0.0) * probe - probe).length() < 1.0e-12);
        }
        // And a quarter turn later, the equinox is over 90° west.
        let quarter = eci_to_ecef_dcm(std::f64::consts::FRAC_PI_2) * DVec3::X;
        assert!((quarter - DVec3::NEG_Y).length() < 1.0e-12, "{quarter}");
    }

    #[test]
    fn the_rotation_round_trips() {
        for angle in ANGLES {
            for probe in probes() {
                let there = eci_to_ecef_quat(angle) * probe;
                let back = ecef_to_eci_quat(angle) * there;
                assert!((back - probe).length() < 1.0e-9, "{angle} on {probe}");
            }
        }
    }

    #[test]
    fn a_geostationary_satellite_stands_still_over_the_ground() {
        // The whole reason a velocity needs the transport term. At the
        // geostationary radius, moving at exactly the rate the Earth turns, the
        // inertial speed is three kilometres a second and the ground-relative
        // one is nothing.
        let radius = 42_164.0;
        for angle in ANGLES {
            let position = ecef_to_eci_quat(angle) * DVec3::new(radius, 0.0, 0.0);
            let velocity = DVec3::new(0.0, 0.0, EARTH_RATE_RAD_S).cross(position);
            assert!(
                (velocity.length() - 3.0746).abs() < 1.0e-3,
                "{angle}: {}",
                velocity.length()
            );
            let over_ground = velocity_eci_to_ecef(position, velocity, angle);
            assert!(over_ground.length() < 1.0e-9, "{angle}: {over_ground}");
        }
    }

    #[test]
    fn a_polar_satellite_keeps_its_speed_over_the_ground() {
        // The other end of the same rule: over the pole the rotating frame has
        // no velocity of its own, so the two speeds agree.
        let position = DVec3::new(0.0, 0.0, 7000.0);
        let velocity = DVec3::new(7.5, 0.0, 0.0);
        let over_ground = velocity_eci_to_ecef(position, velocity, 1.1);
        assert!(
            (over_ground.length() - velocity.length()).abs() < 1.0e-9,
            "{over_ground}"
        );
    }

    #[test]
    fn the_scene_basis_agrees_with_the_globe() {
        // The relabelling, checked against the conversion every other part of
        // the globe places a coordinate with. A mismatch here would draw a
        // correct frame in the wrong place, which is the hardest kind of wrong
        // to see.
        for (lat, lon) in [(0.0, 0.0), (0.0, 90.0), (90.0, 0.0), (35.0, -120.0)] {
            let canonical = {
                let (lat, lon) = (f64::from(lat).to_radians(), f64::from(lon).to_radians());
                DVec3::new(lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin())
            };
            let difference = scene_from_canonical(canonical) - LatLon::new(lat, lon).to_direction();
            assert!(difference.length() < 1.0e-6, "{lat},{lon}: {difference}");
        }
    }

    #[test]
    fn the_canonical_rotation_is_the_one_the_scene_is_drawn_with() {
        // The bridge the whole module stands on: rotating a vector between the
        // frames in the canonical basis and then relabelling it into the scene
        // has to land where the renderer's own quaternion puts it. If these two
        // ever disagree, the axes are drawn against a sidereal angle the globe
        // is not turning at.
        for angle in ANGLES {
            for probe in probes() {
                let probe = probe.normalize();
                let through_math = scene_from_canonical(eci_to_ecef_dcm(angle) * probe);
                let through_scene =
                    Quat::from_rotation_y(-angle as f32) * scene_from_canonical(probe);
                assert!(
                    (through_math - through_scene).length() < 1.0e-6,
                    "{angle} on {probe}: {through_math} vs {through_scene}"
                );
            }
        }
    }

    #[test]
    fn the_earth_rate_is_the_rate_the_globe_turns_at() {
        // A sidereal day at this rate is one full turn of the renderer's own
        // sidereal angle, which is the only thing that makes the drawn axes and
        // the reported velocities the same physics.
        let sidereal_day = std::f64::consts::TAU / EARTH_RATE_RAD_S;
        let start = crate::frame::sidereal_radians(0.0);
        let later = crate::frame::sidereal_radians(sidereal_day);
        assert!((later - start).abs() < 1.0e-6, "{later} vs {start}");
    }

    #[test]
    fn a_subpoint_is_read_back_where_it_was_placed() {
        for (lat, lon, altitude) in [(0.0, 0.0, 400.0), (51.5, -0.13, 0.0), (-33.9, 151.2, 780.0)] {
            let radius = f64::from(EARTH_RADIUS_KM) + altitude;
            let (rlat, rlon) = (f64::from(lat).to_radians(), f64::from(lon).to_radians());
            let position =
                DVec3::new(rlat.cos() * rlon.cos(), rlat.cos() * rlon.sin(), rlat.sin()) * radius;
            let (coordinate, height) = subpoint(position);
            assert!((coordinate.lat - lat).abs() < 1.0e-3, "{coordinate:?}");
            assert!((coordinate.lon - lon).abs() < 1.0e-3, "{coordinate:?}");
            assert!((height - altitude).abs() < 1.0e-6, "{height}");
        }
    }

    #[test]
    fn an_axis_is_drawn_as_a_ray_with_a_tick() {
        let paths = axis_path(Vec3::Z, 1.8, POLE_COLOR, 2.0);
        assert_eq!(paths.len(), 3);
        assert!((paths[0].points[0].length() - AXIS_INNER).abs() < 1.0e-6);
        assert!((paths[0].points[1].length() - 1.8).abs() < 1.0e-6);
        // The ticks cross the axis rather than running along it.
        for tick in &paths[1..] {
            let along = (tick.points[1] - tick.points[0]).normalize();
            assert!(along.dot(Vec3::Z).abs() < 1.0e-6, "{along}");
        }
    }

    #[test]
    fn a_ring_closes_on_itself() {
        let ring = ring_path(Vec3::Y, 1.5, POLE_COLOR, 1.0);
        assert_eq!(ring.points.len(), RING_SAMPLES + 1);
        let (first, last) = (ring.points[0], ring.points[RING_SAMPLES]);
        assert!((first - last).length() < 1.0e-5, "{first} vs {last}");
        // Every point is in the plane the ring was asked for, at its radius.
        for point in &ring.points {
            assert!(point.y.abs() < 1.0e-5, "{point}");
            assert!((point.length() - 1.5).abs() < 1.0e-5, "{point}");
        }
    }

    #[test]
    fn the_graticule_spans_the_globe_at_the_spacing_asked_for() {
        // Twelve meridians thirty degrees apart, and the parallels at 0, ±30 and
        // ±60 — everything but the poles, which a meridian already reaches.
        let mesh = graticule_mesh(30.0).expect("a grid");
        assert!(mesh.count_vertices() > 0);
        let paths = 12 + 5;
        // Two vertices per point of every path, and every path the same length
        // here because the spacing divides both spans evenly.
        assert_eq!(
            mesh.count_vertices(),
            2 * ((12 * 90) + (5 * 181)),
            "{paths} paths"
        );
    }

    /// The drawing, run for a tick in a world with no renderer in it.
    ///
    /// Worth a test of its own because everything above checks the arithmetic
    /// and none of it checks that the arithmetic ever reaches the screen: this
    /// is what catches a system that panics on an empty catalogue, a part that
    /// is never spawned, or an anchor put on the wrong piece.
    fn headless(settings: GncSettings, ephemerides: EphemerisSettings) -> App {
        let mut app = App::new();
        app.add_plugins((
            bevy::app::TaskPoolPlugin::default(),
            bevy::asset::AssetPlugin::default(),
        ))
        .init_asset::<Mesh>()
        .init_asset::<VectorMaterial>()
        .init_resource::<ButtonInput<KeyCode>>()
        .init_resource::<Time>()
        .init_resource::<ReferenceFrame>()
        .init_resource::<Sun>()
        .init_resource::<GncDrawn>()
        .insert_resource(ephemerides)
        .insert_resource(settings)
        .add_systems(Update, (draw_gnc, carry_frames).chain());
        app.update();
        app
    }

    /// One layer holding the ISS, and that object pinned — which is what the
    /// drawing follows when it is given nothing more specific.
    fn tracking() -> EphemerisSettings {
        let mut ephemerides =
            crate::ephemeris::test_settings(crate::ephemeris::TEST_ISS, "stations");
        assert!(ephemerides.pin("stations", 25544));
        ephemerides
    }

    #[test]
    fn nothing_is_spawned_until_the_drawing_is_asked_for() {
        let mut app = headless(GncSettings::default(), tracking());
        assert_eq!(
            app.world_mut().query::<&Anchor>().iter(app.world()).count(),
            0
        );
    }

    #[test]
    fn a_pinned_satellite_is_drawn_in_both_frames_at_once() {
        // The whole feature, end to end: one pinned object, and both of its
        // paths on screen — the inertial one anchored to the stars and the
        // ground track anchored to the ground, from the same propagation.
        let mut app = headless(
            GncSettings {
                enabled: true,
                eci_axes: false,
                ecef_axes: false,
                graticule: false,
                sidereal: false,
                ..GncSettings::default()
            },
            tracking(),
        );
        let anchors: Vec<Anchor> = app
            .world_mut()
            .query_filtered::<&Anchor, With<Mesh3d>>()
            .iter(app.world())
            .copied()
            .collect();
        assert_eq!(anchors.len(), 3, "{anchors:?}");
        assert!(
            anchors.contains(&Anchor::Eci),
            "no inertial orbit: {anchors:?}"
        );
        assert!(
            anchors.contains(&Anchor::Ecef),
            "no ground track: {anchors:?}"
        );
        assert!(
            anchors.contains(&Anchor::World),
            "no nadir line: {anchors:?}"
        );

        // And every one of them is visible, rather than spawned and hidden.
        let hidden = app
            .world_mut()
            .query::<&Visibility>()
            .iter(app.world())
            .filter(|visibility| **visibility == Visibility::Hidden)
            .count();
        assert_eq!(hidden, 0);
    }

    #[test]
    fn the_paths_are_hidden_when_nothing_is_being_followed() {
        let mut app = headless(
            GncSettings {
                enabled: true,
                ..GncSettings::default()
            },
            crate::ephemeris::test_settings(crate::ephemeris::TEST_ISS, "stations"),
        );
        // Nothing pinned, so the three satellite pieces have nothing to draw and
        // only the two frames and the angle between them are up.
        assert_eq!(
            app.world_mut()
                .query_filtered::<&Anchor, With<Mesh3d>>()
                .iter(app.world())
                .count(),
            4
        );
    }

    #[test]
    fn each_piece_is_anchored_to_the_frame_it_is_rigid_in() {
        // No satellite is pinned, so the three that follow one draw nothing and
        // the rest are spawned under the frame each belongs to.
        let mut app = headless(
            GncSettings {
                enabled: true,
                track: false,
                ..GncSettings::default()
            },
            tracking(),
        );
        let anchors: Vec<Anchor> = app
            .world_mut()
            .query::<&Anchor>()
            .iter(app.world())
            .copied()
            .collect();
        assert_eq!(
            anchors
                .iter()
                .filter(|anchor| **anchor == Anchor::Eci)
                .count(),
            1,
            "{anchors:?}"
        );
        assert_eq!(
            anchors
                .iter()
                .filter(|anchor| **anchor == Anchor::Ecef)
                .count(),
            2,
            "{anchors:?}"
        );
        assert_eq!(
            anchors
                .iter()
                .filter(|anchor| **anchor == Anchor::World)
                .count(),
            1,
            "{anchors:?}"
        );
    }

    #[test]
    fn a_switched_off_piece_is_hidden_rather_than_rebuilt() {
        let mut app = headless(
            GncSettings {
                enabled: true,
                track: false,
                ..GncSettings::default()
            },
            tracking(),
        );
        let meshes: Vec<AssetId<Mesh>> = app
            .world_mut()
            .query::<&Mesh3d>()
            .iter(app.world())
            .map(|mesh| mesh.id())
            .collect();

        app.world_mut().resource_mut::<GncSettings>().graticule = false;
        app.update();
        app.world_mut().resource_mut::<GncSettings>().graticule = true;
        app.update();

        let after: Vec<AssetId<Mesh>> = app
            .world_mut()
            .query::<&Mesh3d>()
            .iter(app.world())
            .map(|mesh| mesh.id())
            .collect();
        assert_eq!(meshes, after, "a piece was rebuilt rather than hidden");
    }

    #[test]
    fn a_ground_track_drifts_west_by_exactly_the_earth_rotation() {
        // The picture's whole claim, as arithmetic. Take the *same* inertial
        // direction at two moments: the point has not moved against the stars at
        // all, and its Earth-fixed longitude has nonetheless gone west by
        // however far the planet turned in between. Every westward step between
        // one pass of a ground track and the next is this and nothing else.
        let inertial = DVec3::new(6878.0, 0.0, 0.0);
        let start = 1_700_000_000.0_f64;
        for gap_seconds in [60.0, 3600.0, 5400.0] {
            let first = subpoint(eci_to_ecef_dcm(crate::frame::sidereal_radians(start)) * inertial);
            let later = subpoint(
                eci_to_ecef_dcm(crate::frame::sidereal_radians(start + gap_seconds)) * inertial,
            );
            let drift = f64::from(first.0.lon - later.0.lon).rem_euclid(360.0);
            let turned = (EARTH_RATE_RAD_S * gap_seconds)
                .to_degrees()
                .rem_euclid(360.0);
            assert!(
                (drift - turned).abs() < 1.0e-3,
                "{gap_seconds}s: {drift} vs {turned}"
            );
            // And the latitude has not moved, because the rotation is about the
            // pole and nothing else.
            assert!((first.0.lat - later.0.lat).abs() < 1.0e-4);
        }
    }

    #[test]
    fn an_inertial_path_is_rigid_and_a_ground_track_is_not() {
        // Why one of the two curves can ride a quaternion and the other cannot.
        // A circular orbit, propagated by hand so the test rests on the frame
        // arithmetic rather than on SGP4: in the inertial frame consecutive
        // samples are a fixed angle apart, and in the Earth-fixed frame they are
        // not, because the Earth turned by a different amount between each pair.
        let period_seconds = 90.0 * 60.0;
        let radius = 6878.0;
        let now = 1_700_000_000.0_f64;
        let at = |t: f64| {
            let angle = (t - now) * std::f64::consts::TAU / period_seconds;
            // Inclined, so the ground track has something to wander in.
            DVec3::new(
                radius * angle.cos(),
                radius * angle.sin() * 0.6,
                radius * angle.sin() * 0.8,
            )
        };

        let step = period_seconds / 64.0;
        let mut inertial_steps = Vec::new();
        let mut ground_steps = Vec::new();
        for index in 0..64 {
            let (a, b) = (now + step * index as f64, now + step * (index + 1) as f64);
            let (ia, ib) = (at(a), at(b));
            inertial_steps.push(ia.normalize().dot(ib.normalize()).clamp(-1.0, 1.0).acos());
            let ga = eci_to_ecef_dcm(crate::frame::sidereal_radians(a)) * ia;
            let gb = eci_to_ecef_dcm(crate::frame::sidereal_radians(b)) * ib;
            ground_steps.push(ga.normalize().dot(gb.normalize()).clamp(-1.0, 1.0).acos());
        }

        let spread = |steps: &[f64]| {
            let max = steps.iter().copied().fold(f64::MIN, f64::max);
            let min = steps.iter().copied().fold(f64::MAX, f64::min);
            max - min
        };
        // Evenly spaced in the frame it is a conic in...
        assert!(
            spread(&inertial_steps) < 1.0e-9,
            "{}",
            spread(&inertial_steps)
        );
        // ...and measurably not in the frame the ground is fixed to. No rigid
        // rotation takes one to the other, which is why the ground track has to
        // be built sample by sample.
        assert!(spread(&ground_steps) > 1.0e-4, "{}", spread(&ground_steps));
    }
}
