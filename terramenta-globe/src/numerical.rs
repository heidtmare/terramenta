//! Numerical propagation for the JavaScript API: request → [`terramenta_solare`] spacecraft →
//! CCSDS OEM text or SPK bytes.
//!
//! Third-body Sun/Moon come from a loaded kernel ([`load_kernel`]) or the analytic series.
//! EOP ([`load_eop`]) feed Earth orientation and TEME↔GCRF; NRLMSISE-00 drag needs
//! [`load_space_weather`]. States are GCRF, UTC epochs.

use std::sync::{Arc, LazyLock, RwLock};

use serde::{Deserialize, Serialize};
use terramenta_solare::bodies::{GM_MOON_KM3_S2, GM_SUN_KM3_S2};
use terramenta_solare::ccsds::{Oem, OemMetadata, OemSegment, TimeSystem};
use terramenta_solare::earth::EarthModel;
use terramenta_solare::eop::{EopTable, FinalsKind};
use terramenta_solare::events::{self, GroundStation, OrbitEventKind, Shadow};
use terramenta_solare::force::{
    Drag, EGM2008_GM_KM3_S2, ExponentialAtmosphere, HarrisPriester, Nrlmsise00, PointMass,
    Relativity, SolarRadiationPressure, SphericalHarmonics, ThirdBody, Vehicle,
};
use terramenta_solare::frame::Relative;
use terramenta_solare::interpolate::{Interpolation, SampledEphemeris};
use terramenta_solare::maneuver::{FiniteBurn, ImpulsiveManeuver, LocalFrame};
use terramenta_solare::propagate::{BodyOrientation, EarthOrientation, Integrator, Method};
use terramenta_solare::space_weather::SpaceWeather;
use terramenta_solare::spacecraft::{Propagation, Spacecraft};
use terramenta_solare::spice::{self, KernelEphemeris, anise::prelude::Almanac, naif};
use terramenta_solare::spk::{self, SpkSegment, SpkSegmentKind};
use terramenta_solare::tle::Sgp4Ephemeris;
use terramenta_solare::{Ephemeris, Epoch, Propagator, SpacecraftState, StateVector};

/// Earth equatorial radius for SRP shadow, km.
const EARTH_RADIUS_KM: f64 = 6378.137;
/// Default SPK target id for an unnamed spacecraft.
const DEFAULT_NAIF_ID: i32 = -100_900;
/// Sample cap per request.
const MAX_SAMPLES: usize = 200_000;

/// Data the propagator reads, loaded from JavaScript.
#[derive(Default)]
pub struct Environment {
    pub almanac: Option<Arc<Almanac>>,
    pub eop: Option<Arc<EopTable>>,
    pub space_weather: Option<Arc<SpaceWeather>>,
}

static ENVIRONMENT: LazyLock<RwLock<Environment>> = LazyLock::new(Default::default);

fn environment() -> std::sync::RwLockReadGuard<'static, Environment> {
    ENVIRONMENT.read().unwrap_or_else(|e| e.into_inner())
}

fn environment_mut() -> std::sync::RwLockWriteGuard<'static, Environment> {
    ENVIRONMENT.write().unwrap_or_else(|e| e.into_inner())
}

/// Adds an SPK/PCK to the almanac.
pub fn load_kernel(bytes: &[u8]) -> Result<(), String> {
    let mut env = environment_mut();
    let base = env.almanac.as_deref().cloned().unwrap_or_default();
    let almanac = spice::load_kernel_bytes(base, bytes).map_err(|e| e.to_string())?;
    env.almanac = Some(Arc::new(almanac));
    Ok(())
}

/// IERS `finals2000A.all` / `finals.all`, or CelesTrak `EOP-All.csv`.
pub fn load_eop(text: &str) -> Result<(), String> {
    let table = if text.trim_start().starts_with("DATE") {
        EopTable::from_celestrak_csv(text)
    } else {
        EopTable::from_iers_finals(text, FinalsKind::Iau2000A)
    }
    .map_err(|e| e.to_string())?;
    environment_mut().eop = Some(Arc::new(table));
    Ok(())
}

/// CelesTrak `SW-All.csv`.
pub fn load_space_weather(text: &str) -> Result<(), String> {
    let sw = SpaceWeather::from_celestrak_csv(text).map_err(|e| e.to_string())?;
    environment_mut().space_weather = Some(Arc::new(sw));
    Ok(())
}

/// Loaded EOP table, if any.
pub fn eop_table() -> Option<Arc<EopTable>> {
    environment().eop.clone()
}

#[derive(Serialize, Debug, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentInfo {
    pub kernel: bool,
    /// MJD (UTC) coverage.
    pub eop: Option<(f64, f64)>,
    pub space_weather: Option<(i64, i64)>,
}

pub fn describe_environment() -> EnvironmentInfo {
    let env = environment();
    EnvironmentInfo {
        kernel: env.almanac.is_some(),
        eop: env.eop.as_ref().and_then(|t| t.coverage()),
        space_weather: env.space_weather.as_ref().map(|s| s.coverage()),
    }
}

/// `propagate` options.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropagationRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// OEM `OBJECT_ID`.
    #[serde(default)]
    pub object_id: Option<String>,
    /// SPK target.
    #[serde(default)]
    pub naif_id: Option<i32>,
    pub initial: Initial,
    /// UTC ISO 8601. Default: initial epoch.
    #[serde(default)]
    pub start: Option<String>,
    /// UTC ISO 8601; else `durationS` after start.
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default)]
    pub duration_s: Option<f64>,
    #[serde(default = "default_step")]
    pub step_s: f64,
    #[serde(default)]
    pub vehicle: VehicleOptions,
    #[serde(default)]
    pub forces: ForceOptions,
    #[serde(default)]
    pub integrator: IntegratorOptions,
    #[serde(default)]
    pub maneuvers: Vec<ManeuverOptions>,
    #[serde(default)]
    pub finite_burns: Vec<FiniteBurnOptions>,
    /// Return OEM KVN text. `false` skips formatting and the JS copy (`addPropagated` only).
    #[serde(default = "default_kvn")]
    pub kvn: bool,
}

fn default_kvn() -> bool {
    true
}

fn default_step() -> f64 {
    60.0
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", untagged)]
pub enum Initial {
    Tle {
        tle: [String; 2],
    },
    Omm {
        omm: serde_json::Value,
    },
    /// GCRF.
    State {
        epoch: String,
        #[serde(rename = "positionKm")]
        position_km: [f64; 3],
        #[serde(rename = "velocityKmS")]
        velocity_km_s: [f64; 3],
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VehicleOptions {
    pub mass_kg: f64,
    pub dry_mass_kg: Option<f64>,
    pub drag_area_m2: Option<f64>,
    pub drag_coefficient: Option<f64>,
    pub srp_area_m2: Option<f64>,
    pub reflectivity: Option<f64>,
}

impl Default for VehicleOptions {
    fn default() -> Self {
        Self {
            mass_kg: 1000.0,
            dry_mass_kg: None,
            drag_area_m2: None,
            drag_coefficient: None,
            srp_area_m2: None,
            reflectivity: None,
        }
    }
}

impl VehicleOptions {
    fn resolve(&self) -> Vehicle {
        let d = Vehicle::default();
        Vehicle {
            dry_mass_kg: self.dry_mass_kg.unwrap_or(d.dry_mass_kg.min(self.mass_kg)),
            drag_area_m2: self.drag_area_m2.unwrap_or(d.drag_area_m2),
            drag_coefficient: self.drag_coefficient.unwrap_or(d.drag_coefficient),
            srp_area_m2: self.srp_area_m2.unwrap_or(d.srp_area_m2),
            reflectivity: self.reflectivity.unwrap_or(d.reflectivity),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DragModel {
    None,
    Exponential,
    HarrisPriester,
    Nrlmsise00,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ForceOptions {
    /// `[degree, order]`, EGM2008, ≤ 20. `[0, 0]`: point mass.
    pub gravity: [usize; 2],
    pub third_body: bool,
    pub drag: DragModel,
    pub srp: bool,
    pub relativity: bool,
}

impl Default for ForceOptions {
    fn default() -> Self {
        Self {
            gravity: [8, 8],
            third_body: true,
            drag: DragModel::Exponential,
            srp: true,
            relativity: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IntegratorOptions {
    /// `dop853` | `rkf78` | `rk4`.
    pub method: String,
    pub relative_tolerance: Option<f64>,
    pub absolute_tolerance: Option<f64>,
    /// RK4 step, s.
    pub fixed_step_s: Option<f64>,
}

impl Default for IntegratorOptions {
    fn default() -> Self {
        Self {
            method: "dop853".into(),
            relative_tolerance: None,
            absolute_tolerance: None,
            fixed_step_s: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManeuverOptions {
    pub epoch: String,
    pub delta_v_km_s: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isp_s: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FiniteBurnOptions {
    pub start: String,
    pub duration_s: f64,
    pub thrust_n: f64,
    pub isp_s: f64,
    pub direction: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
}

/// Propagation output.
pub struct Propagated {
    pub name: String,
    pub object_id: String,
    pub naif_id: i32,
    pub states: Vec<SpacecraftState>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PropagatedInfo {
    /// KVN; absent when `kvn: false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oem: Option<String>,
    pub samples: usize,
    pub final_mass_kg: f64,
    pub start: String,
    pub stop: String,
}

fn epoch(s: &str) -> Result<Epoch, String> {
    TimeSystem::Utc
        .parse_epoch(s)
        .ok_or_else(|| format!("bad UTC epoch {s:?}"))
}

fn local_frame(s: Option<&str>) -> Result<LocalFrame, String> {
    Ok(match s.map(str::to_ascii_lowercase).as_deref() {
        None | Some("vnb") => LocalFrame::Vnb,
        Some("rsw" | "ric") => LocalFrame::Rsw,
        Some("lvlh") => LocalFrame::Lvlh,
        Some("inertial" | "gcrf" | "eci") => LocalFrame::Inertial,
        Some(other) => return Err(format!("unknown maneuver frame {other:?}")),
    })
}

/// Geocentric Sun and Moon: kernel when loaded, analytic otherwise.
fn sun_and_moon(env: &Environment) -> (Arc<dyn Ephemeris>, Arc<dyn Ephemeris>) {
    let tree = Arc::new(terramenta_solare::solar_system());
    let analytic_sun =
        Relative::by_name(tree, "Sun", "Earth").expect("solar_system has Sun, Earth");
    match &env.almanac {
        Some(a) => (
            Arc::new(
                KernelEphemeris::new(a.clone(), naif::SUN, naif::EARTH).with_fallback(analytic_sun),
            ),
            Arc::new(
                KernelEphemeris::new(a.clone(), naif::MOON, naif::EARTH)
                    .with_fallback(terramenta_solare::moon::Moon),
            ),
        ),
        None => (
            Arc::new(analytic_sun),
            Arc::new(terramenta_solare::moon::Moon),
        ),
    }
}

/// `Arc<dyn Ephemeris>` as `impl Ephemeris`.
#[derive(Debug, Clone)]
struct Shared(Arc<dyn Ephemeris>);

impl Ephemeris for Shared {
    fn state_at(&self, epoch: Epoch) -> StateVector {
        self.0.state_at(epoch)
    }

    fn position_at(&self, epoch: Epoch) -> bevy::math::DVec3 {
        self.0.position_at(epoch)
    }
}

fn propagator(req: &PropagationRequest, env: &Environment) -> Result<Propagator, String> {
    let i = &req.integrator;
    let method = match i.method.to_ascii_lowercase().as_str() {
        "dop853" => Method::Dop853,
        "rkf78" => Method::Rkf78,
        "rk4" => Method::Rk4 {
            step_s: i.fixed_step_s.unwrap_or(10.0),
        },
        other => return Err(format!("unknown integrator {other:?}")),
    };
    let mut integrator = Integrator::new(method);
    if i.relative_tolerance.is_some() || i.absolute_tolerance.is_some() {
        integrator = integrator.with_tolerance(
            i.relative_tolerance.unwrap_or(1e-12),
            i.absolute_tolerance.unwrap_or(1e-9),
        );
    }

    let f = &req.forces;
    let [degree, order] = f.gravity;
    if degree > 20 || order > degree {
        return Err(format!(
            "gravity [{degree}, {order}]: need order ≤ degree ≤ 20"
        ));
    }
    let mut p = Propagator::new(integrator).with_orientation(EarthOrientation::new(
        env.eop.clone(),
        EarthModel::Iau2006Cio,
    ));
    p = if degree >= 2 {
        p.with_force(SphericalHarmonics::egm2008(degree, order))
    } else {
        p.with_force(PointMass {
            gm_km3_s2: EGM2008_GM_KM3_S2,
        })
    };
    let (sun, moon) = sun_and_moon(env);
    if f.third_body || f.srp || matches!(f.drag, DragModel::HarrisPriester) {
        p = p.with_sun(Shared(sun.clone()));
    }
    if f.third_body {
        p = p
            .with_force(ThirdBody::sun(GM_SUN_KM3_S2, Shared(sun.clone())))
            .with_force(ThirdBody::new(GM_MOON_KM3_S2, Shared(moon)));
    }
    if f.srp {
        p = p.with_force(SolarRadiationPressure::new(
            Shared(sun.clone()),
            EARTH_RADIUS_KM,
        ));
    }
    p = match f.drag {
        DragModel::None => p,
        DragModel::Exponential => p.with_force(Drag::new(ExponentialAtmosphere)),
        DragModel::HarrisPriester => p.with_force(Drag::new(HarrisPriester::new(Shared(sun)))),
        DragModel::Nrlmsise00 => {
            let sw = env
                .space_weather
                .clone()
                .ok_or("nrlmsise00 drag needs loadSpaceWeather")?;
            p.with_force(Drag::new(Nrlmsise00::new(sw)))
        }
    };
    if f.relativity {
        p = p.with_force(Relativity {
            gm_km3_s2: EGM2008_GM_KM3_S2,
        });
    }
    for m in &req.maneuvers {
        let mut man = ImpulsiveManeuver::new(
            epoch(&m.epoch)?,
            m.delta_v_km_s.into(),
            local_frame(m.frame.as_deref())?,
        );
        if let Some(isp) = m.isp_s {
            man = man.with_isp(isp);
        }
        p = p.with_maneuver(man);
    }
    for b in &req.finite_burns {
        let mut burn = FiniteBurn {
            start: epoch(&b.start)?,
            duration_s: b.duration_s,
            thrust_n: b.thrust_n,
            isp_s: b.isp_s,
            direction: b.direction.into(),
            frame: local_frame(b.frame.as_deref())?,
        };
        let valid = |x: f64| x.is_finite() && x > 0.0;
        if !valid(burn.direction.length())
            || !valid(b.thrust_n)
            || !valid(b.isp_s)
            || !valid(b.duration_s)
        {
            return Err(
                "finite burn needs nonzero direction and positive duration, thrust, isp".into(),
            );
        }
        burn.direction = burn.direction.normalize();
        p = p.with_force(burn);
    }
    Ok(p)
}

pub fn propagate(req: &PropagationRequest) -> Result<Propagated, String> {
    let env = environment();
    let mass = req.vehicle.mass_kg;
    if mass.is_nan() || mass <= 0.0 {
        return Err("vehicle.massKg must be positive".into());
    }
    let sgp4 = |s: Sgp4Ephemeris| {
        let eop = env
            .eop
            .as_ref()
            .map(|t| t.at(s.epoch()))
            .unwrap_or_default();
        Spacecraft::from_sgp4("", s.with_eop(eop), mass).map_err(|e| e.to_string())
    };
    let numerical = || Ok::<_, String>(Propagation::Numerical(Box::new(propagator(req, &env)?)));
    let mut sc = match &req.initial {
        Initial::Tle { tle } => {
            sgp4(Sgp4Ephemeris::from_tle(&tle[0], &tle[1]).map_err(|e| e.to_string())?)?
        }
        Initial::Omm { omm } => {
            let el: sgp4::Elements =
                serde_json::from_value(omm.clone()).map_err(|e| format!("bad OMM: {e}"))?;
            sgp4(Sgp4Ephemeris::from_elements(el).map_err(|e| e.to_string())?)?
        }
        Initial::State {
            epoch: e,
            position_km,
            velocity_km_s,
        } => Spacecraft::new(
            "",
            SpacecraftState::new(
                epoch(e)?,
                StateVector::new((*position_km).into(), (*velocity_km_s).into()),
                mass,
            ),
            numerical()?,
        )
        .with_vehicle(req.vehicle.resolve()),
    };
    let start = req
        .start
        .as_deref()
        .map(epoch)
        .transpose()?
        .unwrap_or(sc.epoch());
    let stop = match (&req.stop, req.duration_s) {
        (Some(s), _) => epoch(s)?,
        (None, Some(d)) => start.advanced_by_seconds(d),
        (None, None) => return Err("need stop or durationS".into()),
    };
    let step = req.step_s.abs();
    if step.is_nan() || step <= 0.0 || stop.seconds_since(start).abs() / step > MAX_SAMPLES as f64 {
        return Err(format!(
            "stepS must be positive and yield ≤ {MAX_SAMPLES} samples"
        ));
    }
    // SGP4 carries TLE/OMM to `start`; a state is numerical from its own epoch so
    // maneuvers between it and `start` apply.
    sc.step_to(start).map_err(|e| e.to_string())?;
    if !matches!(req.initial, Initial::State { .. }) {
        sc.vehicle = req.vehicle.resolve();
        sc.set_propagation(numerical()?);
    }
    let states = sc.samples(stop, step).map_err(|e| e.to_string())?;
    let name = req.name.clone().unwrap_or_else(|| "SPACECRAFT".into());
    Ok(Propagated {
        object_id: req.object_id.clone().unwrap_or_else(|| name.clone()),
        name,
        naif_id: req.naif_id.unwrap_or(DEFAULT_NAIF_ID),
        states,
    })
}

/// Maneuver arcs: split at impulsive maneuvers (repeated epochs); single-state arcs dropped.
fn arcs(states: &[(Epoch, StateVector)]) -> impl Iterator<Item = &[(Epoch, StateVector)]> {
    states.chunk_by(|a, b| a.0 != b.0).filter(|g| g.len() >= 2)
}

impl Propagated {
    /// Time-ordered states.
    fn ordered_states(&self) -> Vec<(Epoch, StateVector)> {
        let mut states: Vec<(Epoch, StateVector)> = self
            .states
            .iter()
            .map(|s| (s.epoch, s.state_vector()))
            .collect();
        if states.first().map(|s| s.0) > states.last().map(|s| s.0) {
            states.reverse();
        }
        states
    }

    /// One segment per maneuver arc; GCRF, UTC, Hermite 7.
    pub fn to_oem(&self) -> Oem {
        let metadata = OemMetadata {
            object_name: self.name.clone(),
            object_id: self.object_id.clone(),
            center_name: "EARTH".into(),
            ref_frame: "GCRF".into(),
            time_system: TimeSystem::Utc,
            interpolation: Some(Interpolation::default()),
        };
        Oem {
            segments: arcs(&self.ordered_states())
                .map(|states| OemSegment {
                    metadata: metadata.clone(),
                    states: states.to_vec(),
                })
                .collect(),
            ..Default::default()
        }
    }

    /// Type 13 per maneuver arc, center Earth, frame J2000 (GCRF; frame bias ≤ 23 mas
    /// ignored). Short arcs shrink the window.
    pub fn to_spk(&self) -> Result<Vec<u8>, String> {
        let name: String = self
            .name
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(spk::MAX_NAME_LEN)
            .collect();
        let name = if name.trim().is_empty() {
            "SPACECRAFT".into()
        } else {
            name
        };
        let window = SpkSegmentKind::from_interpolation(Interpolation::default()).window();
        let states = self.ordered_states();
        let segments: Vec<SpkSegment> = arcs(&states)
            .map(|states| SpkSegment {
                target: self.naif_id,
                center: naif::EARTH,
                frame: spk::J2000_FRAME,
                kind: SpkSegmentKind::Hermite {
                    window: window.min(states.len()),
                },
                name: name.clone(),
                states: states.into(),
            })
            .collect();
        spk::write_spk(&segments, "TERRAMENTA").map_err(|e| e.to_string())
    }

    /// `oem`: [`Self::to_oem`], passed in so the caller can keep it.
    /// `oem`: KVN source; `None` omits the text.
    pub fn info(&self, oem: Option<&Oem>) -> PropagatedInfo {
        let (first, last) = (self.states[0], self.states[self.states.len() - 1]);
        PropagatedInfo {
            oem: oem.map(Oem::to_kvn),
            samples: self.states.len(),
            final_mass_kg: last.mass_kg,
            start: TimeSystem::Utc.format(first.epoch),
            stop: TimeSystem::Utc.format(last.epoch),
        }
    }
}

// --- Events ---------------------------------------------------------------------------

/// `findEvents` options.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRequest {
    #[serde(default)]
    pub stations: Vec<StationOptions>,
    #[serde(default)]
    pub eclipses: bool,
    #[serde(default)]
    pub apsides: bool,
    #[serde(default)]
    pub nodes: bool,
    /// Bracketing grid; shorter intervals may be missed.
    #[serde(default = "default_event_step")]
    pub step_s: f64,
}

fn default_event_step() -> f64 {
    30.0
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StationOptions {
    pub id: String,
    pub lat_deg: f64,
    pub lon_deg: f64,
    #[serde(default)]
    pub alt_km: f64,
    #[serde(default)]
    pub min_elevation_deg: f64,
}

/// Event times are Unix seconds (UTC).
#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Events {
    pub contacts: Vec<ContactInfo>,
    pub eclipses: Vec<EclipseInfo>,
    pub orbit: Vec<OrbitEventInfo>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ContactInfo {
    pub station: String,
    pub aos: f64,
    pub los: f64,
    pub max_elevation_at: f64,
    pub max_elevation_deg: f64,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EclipseInfo {
    /// `"penumbra"` (umbra included) or `"umbra"`.
    pub kind: &'static str,
    pub start: f64,
    pub stop: f64,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrbitEventInfo {
    /// `periapsis`, `apoapsis`, `ascendingNode`, `descendingNode`.
    pub kind: &'static str,
    pub at: f64,
}

/// Root refinement tolerance, s.
const EVENT_TOLERANCE_S: f64 = 1e-3;
/// Full Earth orientation recomputed on this grid; Earth rotation exact in between.
const ORIENTATION_GRID_S: f64 = 600.0;
/// Bracketing samples cap per search.
const MAX_EVENT_SAMPLES: f64 = 1e6;

/// Contacts, eclipses (Earth, conical), apsides and nodes (GCRF equator) over the
/// propagated span. Interpolates the propagated states (Hermite 7), so the propagation
/// step bounds accuracy.
pub fn find_events(propagated: &Propagated, req: &EventRequest) -> Result<Events, String> {
    let step = req.step_s.abs();
    let states = propagated.ordered_states();
    if states.len() < 2 {
        return Ok(Events::default());
    }
    let (start, stop) = (states[0].0, states[states.len() - 1].0);
    if step.is_nan() || step <= 0.0 || stop.seconds_since(start) / step > MAX_EVENT_SAMPLES {
        return Err(format!(
            "stepS must be positive and yield ≤ {MAX_EVENT_SAMPLES} samples"
        ));
    }
    let ephemeris = SampledEphemeris::new(states, Interpolation::default());
    let env = environment();
    let mut out = Events::default();

    if !req.stations.is_empty() {
        let orientation = EarthOrientation::new(env.eop.clone(), EarthModel::Iau2006Cio);
        let mut anchors = std::collections::HashMap::new();
        let mut itrf = |e: Epoch| {
            let t = e.seconds_since(start);
            let k = (t / ORIENTATION_GRID_S).floor() as i64;
            let anchor = *anchors.entry(k).or_insert_with(|| {
                orientation.transform(start.advanced_by_seconds(k as f64 * ORIENTATION_GRID_S))
            });
            anchor
                .advanced_by(t - k as f64 * ORIENTATION_GRID_S)
                .rotation()
                * ephemeris.position_at(e)
        };
        for s in &req.stations {
            if !(-90.0..=90.0).contains(&s.lat_deg) || !s.lon_deg.is_finite() {
                return Err(format!("station {:?}: bad latitude/longitude", s.id));
            }
            let station = GroundStation {
                name: s.id.clone(),
                latitude_rad: s.lat_deg.to_radians(),
                longitude_rad: s.lon_deg.to_radians(),
                altitude_km: s.alt_km,
                min_elevation_rad: s.min_elevation_deg.to_radians(),
            };
            out.contacts.extend(
                events::contacts(&station, &mut itrf, start, stop, step, EVENT_TOLERANCE_S)
                    .into_iter()
                    .map(|c| ContactInfo {
                        station: s.id.clone(),
                        aos: c.aos.to_unix_seconds(),
                        los: c.los.to_unix_seconds(),
                        max_elevation_at: c.max_elevation_epoch.to_unix_seconds(),
                        max_elevation_deg: c.max_elevation_rad.to_degrees(),
                    }),
            );
        }
        out.contacts.sort_by(|a, b| a.aos.total_cmp(&b.aos));
    }

    if req.eclipses {
        let (sun, _) = sun_and_moon(&env);
        out.eclipses = events::eclipses(
            &mut |e| ephemeris.position_at(e),
            &mut |e| sun.position_at(e),
            EARTH_RADIUS_KM,
            start,
            stop,
            step,
            EVENT_TOLERANCE_S,
        )
        .into_iter()
        .map(|e| EclipseInfo {
            kind: match e.shadow {
                Shadow::Penumbra => "penumbra",
                Shadow::Umbra => "umbra",
            },
            start: e.start.to_unix_seconds(),
            stop: e.stop.to_unix_seconds(),
        })
        .collect();
        out.eclipses.sort_by(|a, b| a.start.total_cmp(&b.start));
    }

    if req.apsides || req.nodes {
        out.orbit = events::orbit_events(
            &mut |e| ephemeris.state_at(e),
            req.apsides,
            req.nodes,
            start,
            stop,
            step,
            EVENT_TOLERANCE_S,
        )
        .into_iter()
        .map(|e| OrbitEventInfo {
            kind: match e.kind {
                OrbitEventKind::Periapsis => "periapsis",
                OrbitEventKind::Apoapsis => "apoapsis",
                OrbitEventKind::AscendingNode => "ascendingNode",
                OrbitEventKind::DescendingNode => "descendingNode",
            },
            at: e.epoch.to_unix_seconds(),
        })
        .collect();
    }
    Ok(out)
}

// --- Targeting ------------------------------------------------------------------------

/// `target` options.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetRequest {
    pub variables: Vec<TargetVariable>,
    pub goals: Vec<TargetGoal>,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
}

fn default_max_iterations() -> usize {
    25
}

/// One of `burn` (impulsive, 1-based; `parameter` `dv1`–`dv3` km/s in its frame, or
/// `epoch` s offset) or `finite` (1-based; `start` s offset, `duration` s).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetVariable {
    #[serde(default)]
    pub burn: Option<usize>,
    #[serde(default)]
    pub finite: Option<usize>,
    pub parameter: String,
    /// Forward-difference step and scale. Default: 1e-5 km/s, 1 s.
    #[serde(default)]
    pub perturbation: Option<f64>,
    /// Largest change per iteration. Default: 0.1 km/s, 600 s.
    #[serde(default)]
    pub max_step: Option<f64>,
}

/// `quantity` (as reports: `smaKm`, `eccentricity`, … plus `flightPathAngleDeg`, `massKg`)
/// at `at`: `end`, `burnN` (after), `burnN.before`, `finiteN`, `finiteN.end` or a UTC epoch.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetGoal {
    pub quantity: String,
    pub at: String,
    pub value: f64,
    pub tolerance: f64,
}

/// `target` result; `maneuvers`/`finiteBurns` are the request's with `values` applied.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Targeted {
    pub converged: bool,
    pub iterations: usize,
    /// Largest |error / tolerance| at the start and after each iteration.
    pub history: Vec<f64>,
    /// Final variable values, request order: km/s, s offset, s.
    pub values: Vec<f64>,
    pub goals: Vec<GoalResult>,
    pub maneuvers: Vec<ManeuverOptions>,
    pub finite_burns: Vec<FiniteBurnOptions>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GoalResult {
    pub achieved: f64,
    /// Achieved − desired; angles wrapped to ±180°.
    pub error: f64,
}

const QUANTITY_NAMES: [&str; 14] = [
    "altitudeKm",
    "radiusKm",
    "speedKmS",
    "smaKm",
    "eccentricity",
    "inclinationDeg",
    "raanDeg",
    "argPerigeeDeg",
    "trueAnomalyDeg",
    "periodMin",
    "perigeeAltKm",
    "apogeeAltKm",
    "flightPathAngleDeg",
    "massKg",
];

/// Two-body osculating quantity about Earth (EGM2008 GM); altitudes above the equatorial
/// radius. Matches the webapp's `reports.js`.
fn quantity(name: &str, s: &SpacecraftState) -> f64 {
    let (r, v) = (s.position_km, s.velocity_km_s);
    let mu = EGM2008_GM_KM3_S2;
    let (radius, speed) = (r.length(), v.length());
    let h = r.cross(v);
    let rv = r.dot(v);
    let e_vec = ((speed * speed - mu / radius) * r - rv * v) / mu;
    let e = e_vec.length();
    let a = -mu / (2.0 * (speed * speed / 2.0 - mu / radius));
    let node = bevy::math::DVec3::new(-h.y, h.x, 0.0);
    let wrap = |x: f64| x.rem_euclid(360.0);
    let angle = |a: bevy::math::DVec3, b: bevy::math::DVec3| {
        (a.dot(b) / (a.length() * b.length()))
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    };
    match name {
        "altitudeKm" => radius - EARTH_RADIUS_KM,
        "radiusKm" => radius,
        "speedKmS" => speed,
        "smaKm" => a,
        "eccentricity" => e,
        "inclinationDeg" => (h.z / h.length()).clamp(-1.0, 1.0).acos().to_degrees(),
        "raanDeg" => wrap(node.y.atan2(node.x).to_degrees()),
        "argPerigeeDeg" => {
            let w = angle(node, e_vec);
            wrap(if e_vec.z < 0.0 { -w } else { w })
        }
        "trueAnomalyDeg" => {
            let nu = angle(e_vec, r);
            if rv < 0.0 { 360.0 - nu } else { nu }
        }
        "periodMin" => 2.0 * std::f64::consts::PI * (a.powi(3) / mu).sqrt() / 60.0,
        "perigeeAltKm" => a * (1.0 - e) - EARTH_RADIUS_KM,
        "apogeeAltKm" => a * (1.0 + e) - EARTH_RADIUS_KM,
        "flightPathAngleDeg" => (rv / (radius * speed)).clamp(-1.0, 1.0).asin().to_degrees(),
        "massKg" => s.mass_kg,
        _ => f64::NAN,
    }
}

fn is_angle(name: &str) -> bool {
    matches!(name, "raanDeg" | "argPerigeeDeg" | "trueAnomalyDeg")
}

/// Goal epoch in `req`, or `None` for the end of the propagation.
fn goal_epoch(at: &str, req: &PropagationRequest) -> Result<Option<Epoch>, String> {
    let index = |s: &str, n: usize| -> Result<usize, String> {
        s.parse::<usize>()
            .ok()
            .filter(|i| (1..=n).contains(i))
            .map(|i| i - 1)
            .ok_or_else(|| format!("goal at {at:?}: no such burn"))
    };
    if at == "end" {
        return Ok(None);
    }
    if let Some(rest) = at.strip_prefix("burn") {
        let i = index(rest.trim_end_matches(".before"), req.maneuvers.len())?;
        return epoch(&req.maneuvers[i].epoch).map(Some);
    }
    if let Some(rest) = at.strip_prefix("finite") {
        let (n, end) = match rest.strip_suffix(".end") {
            Some(n) => (n, true),
            None => (rest, false),
        };
        let b = &req.finite_burns[index(n, req.finite_burns.len())?];
        let start = epoch(&b.start)?;
        return Ok(Some(if end {
            start.advanced_by_seconds(b.duration_s)
        } else {
            start
        }));
    }
    epoch(at).map(Some)
}

/// UTC ISO 8601 with `Z`, nanoseconds.
fn utc_iso(e: Epoch) -> String {
    format!("{}Z", TimeSystem::Utc.format(e))
}

/// As [`utc_iso`], rounded to milliseconds.
fn utc_iso_ms(e: Epoch) -> String {
    let s = TimeSystem::Utc.format(e.advanced_by_seconds(5e-4));
    format!("{}Z", &s[..s.len() - 6])
}

/// Differential correction ([`terramenta_solare::targeting`]) of `req`'s burns to meet
/// `target.goals`. Each evaluation is a full propagation, ended a step after the latest
/// goal unless one is at `end`.
pub fn target(req: &PropagationRequest, target: &TargetRequest) -> Result<Targeted, String> {
    use terramenta_solare::targeting::{Variable, solve};

    if target.variables.is_empty() || target.goals.is_empty() {
        return Err("need at least one variable and one goal".into());
    }
    let mut variables = Vec::with_capacity(target.variables.len());
    let mut seen = std::collections::HashSet::new();
    for (k, v) in target.variables.iter().enumerate() {
        let (value, perturbation, max_step) = match (v.burn, v.finite, v.parameter.as_str()) {
            (Some(n), None, p @ ("dv1" | "dv2" | "dv3" | "epoch")) => {
                let m = req
                    .maneuvers
                    .get(n.wrapping_sub(1))
                    .ok_or(format!("variables[{k}]: no burn {n}"))?;
                match p {
                    "epoch" => (0.0, 1.0, 600.0),
                    _ => (
                        m.delta_v_km_s[p.as_bytes()[2] as usize - b'1' as usize],
                        1e-5,
                        0.1,
                    ),
                }
            }
            (None, Some(n), p @ ("start" | "duration")) => {
                let b = req
                    .finite_burns
                    .get(n.wrapping_sub(1))
                    .ok_or(format!("variables[{k}]: no finite burn {n}"))?;
                (if p == "start" { 0.0 } else { b.duration_s }, 1.0, 600.0)
            }
            _ => {
                return Err(format!(
                    "variables[{k}]: need burn with dv1|dv2|dv3|epoch or finite with start|duration"
                ));
            }
        };
        if !seen.insert((v.burn, v.finite, v.parameter.as_str())) {
            return Err(format!("variables[{k}]: duplicate"));
        }
        let variable = Variable {
            value,
            perturbation: v.perturbation.unwrap_or(perturbation),
            max_step: v.max_step.unwrap_or(max_step),
        };
        if !(variable.perturbation > 0.0 && variable.max_step > 0.0) {
            return Err(format!(
                "variables[{k}]: perturbation and maxStep must be positive"
            ));
        }
        variables.push(variable);
    }
    for (k, g) in target.goals.iter().enumerate() {
        if !QUANTITY_NAMES.contains(&g.quantity.as_str()) {
            return Err(format!("goals[{k}]: unknown quantity {:?}", g.quantity));
        }
        if g.tolerance.is_nan() || g.tolerance <= 0.0 || !g.value.is_finite() {
            return Err(format!(
                "goals[{k}]: need a finite value and positive tolerance"
            ));
        }
        goal_epoch(&g.at, req).map_err(|e| format!("goals[{k}]: {e}"))?;
    }

    let base_epochs: Vec<Epoch> = req
        .maneuvers
        .iter()
        .map(|m| epoch(&m.epoch))
        .collect::<Result<_, _>>()?;
    let base_starts: Vec<Epoch> = req
        .finite_burns
        .iter()
        .map(|b| epoch(&b.start))
        .collect::<Result<_, _>>()?;
    let apply = |x: &[f64]| -> PropagationRequest {
        let mut r = req.clone();
        for (v, &value) in target.variables.iter().zip(x) {
            match (v.burn, v.finite, v.parameter.as_str()) {
                (Some(n), _, "epoch") => {
                    r.maneuvers[n - 1].epoch =
                        utc_iso(base_epochs[n - 1].advanced_by_seconds(value));
                }
                (Some(n), _, p) => {
                    r.maneuvers[n - 1].delta_v_km_s[p.as_bytes()[2] as usize - b'1' as usize] =
                        value;
                }
                (_, Some(n), "start") => {
                    r.finite_burns[n - 1].start =
                        utc_iso(base_starts[n - 1].advanced_by_seconds(value));
                }
                (_, Some(n), _) => r.finite_burns[n - 1].duration_s = value,
                _ => unreachable!("validated"),
            }
        }
        r
    };
    let at_end = target.goals.iter().any(|g| g.at == "end");
    let evaluate = |x: &[f64]| -> Result<Vec<(f64, f64)>, String> {
        let mut r = apply(x);
        let epochs: Vec<Option<Epoch>> = target
            .goals
            .iter()
            .map(|g| goal_epoch(&g.at, &r))
            .collect::<Result<_, _>>()?;
        if !at_end {
            let latest = epochs
                .iter()
                .flatten()
                .max_by(|a, b| a.seconds_since(**b).total_cmp(&0.0));
            if let (Some(latest), Initial::State { epoch: e, .. }) = (latest, &r.initial) {
                let start = epoch(r.start.as_deref().unwrap_or(e))?;
                let stop = match (&r.stop, r.duration_s) {
                    (Some(s), _) => Some(epoch(s)?),
                    (None, Some(d)) => Some(start.advanced_by_seconds(d)),
                    _ => None,
                };
                let early = latest.advanced_by_seconds(2.0 * r.step_s.abs());
                if stop.is_some_and(|stop| {
                    early.seconds_since(stop) < 0.0 && early.seconds_since(start) > 0.0
                }) {
                    r.stop = Some(utc_iso(early));
                }
            }
        }
        let out = propagate(&r)?;
        let states = {
            let mut s = out.states.clone();
            if s.first().map(|s| s.epoch) > s.last().map(|s| s.epoch) {
                s.reverse();
            }
            s
        };
        let ephemeris = (states.len() >= 2).then(|| {
            SampledEphemeris::new(
                states.iter().map(|s| (s.epoch, s.state_vector())).collect(),
                Interpolation::default(),
            )
        });
        target
            .goals
            .iter()
            .zip(&epochs)
            .map(|(g, e)| {
                let state = match e {
                    None => *states.last().ok_or("empty propagation")?,
                    Some(e) if g.at.starts_with("burn") => {
                        let mut at = states.iter().filter(|s| s.epoch == *e);
                        *if g.at.ends_with(".before") {
                            at.next()
                        } else {
                            at.next_back()
                        }
                        .ok_or(format!("{}: outside the propagation", g.at))?
                    }
                    Some(e) => {
                        let sv = ephemeris
                            .as_ref()
                            .and_then(|eph| eph.try_state_at(*e))
                            .ok_or(format!("{}: outside the propagation", g.at))?;
                        let i = states.partition_point(|s| s.epoch <= *e).max(1) - 1;
                        SpacecraftState::new(*e, sv, states[i].mass_kg)
                    }
                };
                let achieved = quantity(&g.quantity, &state);
                let mut error = achieved - g.value;
                if is_angle(&g.quantity) {
                    error = (error + 180.0).rem_euclid(360.0) - 180.0;
                }
                Ok((achieved, error))
            })
            .collect()
    };
    let tolerances: Vec<f64> = target.goals.iter().map(|g| g.tolerance).collect();
    let solution = solve(
        |x| Ok::<_, String>(evaluate(x)?.into_iter().map(|(_, e)| e).collect()),
        &variables,
        &tolerances,
        target.max_iterations,
    )?;
    let goals = evaluate(&solution.values)?
        .into_iter()
        .map(|(achieved, error)| GoalResult { achieved, error })
        .collect();
    let mut solved = apply(&solution.values);
    for m in &mut solved.maneuvers {
        m.epoch = utc_iso_ms(epoch(&m.epoch)?);
    }
    for b in &mut solved.finite_burns {
        b.start = utc_iso_ms(epoch(&b.start)?);
    }
    // Untouched epochs keep the request's spelling.
    for (m, orig) in solved.maneuvers.iter_mut().zip(&req.maneuvers) {
        if epoch(&m.epoch)? == epoch(&orig.epoch)? {
            m.epoch.clone_from(&orig.epoch);
        }
    }
    for (b, orig) in solved.finite_burns.iter_mut().zip(&req.finite_burns) {
        if epoch(&b.start)? == epoch(&orig.start)? {
            b.start.clone_from(&orig.start);
        }
    }
    Ok(Targeted {
        converged: solution.converged,
        iterations: solution.iterations,
        history: solution.history,
        values: solution.values,
        goals,
        maneuvers: solved.maneuvers,
        finite_burns: solved.finite_burns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(json: &str) -> PropagationRequest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn state_request_with_burn_round_trips_oem_and_spk() {
        let req = request(
            r#"{
                "name": "DEMO",
                "initial": { "epoch": "2024-03-01T00:00:00Z",
                             "positionKm": [6778.137, 0, 0], "velocityKmS": [0, 4.7, 6.0] },
                "durationS": 7200, "stepS": 60,
                "vehicle": { "massKg": 500, "dryMassKg": 300 },
                "forces": { "gravity": [4, 4], "drag": "harrisPriester" },
                "maneuvers": [{ "epoch": "2024-03-01T00:30:00", "deltaVKmS": [0.01, 0, 0], "ispS": 300 }]
            }"#,
        );
        let out = propagate(&req).unwrap();
        // 121 grid samples + pre-burn state at 00:30.
        assert_eq!(out.states.len(), 122);
        assert!(out.states[121].mass_kg < 500.0);
        let text = out.info(Some(&out.to_oem())).oem.unwrap();
        let back = Oem::from_kvn(&text).unwrap();
        let lens: Vec<_> = back.segments.iter().map(|s| s.states.len()).collect();
        assert_eq!(lens, [31, 91]);
        assert_eq!(back.segments[0].states[30].0, back.segments[1].states[0].0);
        let spk = out.to_spk().unwrap();
        assert_eq!(&spk[..8], b"DAF/SPK ");

        // In-memory load matches the KVN path.
        let direct = crate::omm::from_oem(&out.to_oem()).unwrap();
        let parsed = crate::omm::parse(&text).unwrap();
        let t = out.states[60].epoch.to_unix_seconds();
        let (a, b) = (
            direct.satellites[0].position_teme_km(t).unwrap(),
            parsed.satellites[0].position_teme_km(t).unwrap(),
        );
        let d: f64 = (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt();
        assert!(d < 1e-3, "{d} km");
    }

    #[test]
    fn state_before_start_applies_gap_maneuvers() {
        let run = |maneuvers: &str| {
            let req = request(&format!(
                r#"{{
                    "initial": {{ "epoch": "2024-03-01T00:00:00Z",
                                 "positionKm": [6778.137, 0, 0], "velocityKmS": [0, 4.7, 6.0] }},
                    "start": "2024-03-01T01:00:00Z", "durationS": 600, "stepS": 60,
                    "vehicle": {{ "massKg": 500 }},
                    "maneuvers": [{maneuvers}]
                }}"#
            ));
            propagate(&req).unwrap().states[0]
        };
        let coast = run("");
        let burn = run(
            r#"{ "epoch": "2024-03-01T00:30:00Z", "deltaVKmS": [0.01, 0, 0], "frame": "VNB" }"#,
        );
        assert!((burn.velocity_km_s - coast.velocity_km_s).length() > 1e-3);
    }

    #[test]
    fn tle_request_starts_at_element_epoch() {
        let req = request(
            r#"{
                "initial": { "tle": [
                    "1 00005U 58002B   00179.78495062  .00000023  00000-0  28098-4 0  4753",
                    "2 00005  34.2682 348.7242 1859667 331.7664  19.3264 10.82419157413667"] },
                "durationS": 600, "forces": { "gravity": [2, 0], "thirdBody": false, "srp": false, "drag": "none" }
            }"#,
        );
        let out = propagate(&req).unwrap();
        assert_eq!(out.states.len(), 11);
        let r = out.states[10].position_km.length();
        assert!((6600.0..10_000.0).contains(&r), "{r}");
    }

    #[test]
    fn kvn_false_omits_oem_text() {
        let req = request(
            r#"{
                "initial": { "epoch": "2024-03-01T00:00:00", "positionKm": [7000,0,0], "velocityKmS": [0,7.5,0] },
                "durationS": 600, "forces": { "gravity": [2, 0], "thirdBody": false, "srp": false, "drag": "none" },
                "kvn": false
            }"#,
        );
        assert!(!req.kvn);
        let out = propagate(&req).unwrap();
        let info = serde_json::to_value(out.info(req.kvn.then_some(&out.to_oem()))).unwrap();
        assert!(info.get("oem").is_none());
        assert_eq!(info["samples"], 11);
    }

    #[test]
    fn rejects_bad_requests() {
        let base = r#""initial": { "epoch": "2024-03-01T00:00:00", "positionKm": [7000,0,0], "velocityKmS": [0,7.5,0] }"#;
        for extra in [
            r#""stepS": 60"#,
            r#""durationS": 600, "forces": { "gravity": [30, 30] }"#,
            r#""durationS": 600, "forces": { "drag": "nrlmsise00" }"#,
            r#""durationS": 600, "integrator": { "method": "euler" }"#,
        ] {
            let req = request(&format!("{{{base}, {extra}}}"));
            assert!(propagate(&req).is_err(), "{extra}");
        }
    }

    /// Circular 51.6° LEO for a day: one umbra per orbit inside its penumbra, two apsides
    /// and nodes per orbit, passes over a mid-latitude station within its mask.
    #[test]
    fn events_over_a_day_of_leo() {
        let v = (398_600.441_8_f64 / 6778.137).sqrt();
        let (s, c) = 51.6_f64.to_radians().sin_cos();
        let req = request(&format!(
            r#"{{ "initial": {{ "epoch": "2024-03-01T00:00:00Z",
                   "positionKm": [6778.137, 0, 0], "velocityKmS": [0, {}, {}] }},
                 "durationS": 86400, "stepS": 60, "forces": {{ "gravity": [2, 0] }} }}"#,
            v * c,
            v * s
        ));
        let out = propagate(&req).unwrap();
        let events: EventRequest = serde_json::from_str(
            r#"{ "stations": [{ "id": "wallops", "latDeg": 37.94, "lonDeg": -75.46, "minElevationDeg": 5 }],
                 "eclipses": true, "apsides": true, "nodes": true }"#,
        )
        .unwrap();
        let found = find_events(&out, &events).unwrap();
        let umbra: Vec<_> = found
            .eclipses
            .iter()
            .filter(|e| e.kind == "umbra")
            .collect();
        let penumbra = found.eclipses.len() - umbra.len();
        // ~92.2 min period: 15–16 orbits.
        assert!((15..=16).contains(&umbra.len()), "{}", umbra.len());
        assert_eq!(penumbra, umbra.len());
        for u in &umbra {
            let span = u.stop - u.start;
            assert!((1500.0..2400.0).contains(&span), "umbra {span} s");
        }
        let nodes = found
            .orbit
            .iter()
            .filter(|e| e.kind.ends_with("Node"))
            .count();
        assert!((30..=32).contains(&nodes), "{nodes} nodes");
        assert!(found.orbit.iter().any(|e| e.kind == "periapsis"));
        assert!(!found.contacts.is_empty());
        for pass in &found.contacts {
            assert!(pass.los > pass.aos && pass.los - pass.aos < 900.0);
            assert!((5.0..=90.0).contains(&pass.max_elevation_deg));
            assert!(pass.aos <= pass.max_elevation_at && pass.max_elevation_at <= pass.los);
        }
        let bad: EventRequest =
            serde_json::from_str(r#"{ "stations": [{ "id": "x", "latDeg": 95, "lonDeg": 0 }] }"#)
                .unwrap();
        assert!(find_events(&out, &bad).is_err());
    }
    /// Point-mass 500 → 800 km Hohmann: vary both Δv and the second burn's epoch to put it
    /// at apoapsis on a circular 800 km orbit; matches the analytic transfer.
    #[test]
    fn targets_a_hohmann_transfer() {
        let mu = 398_600.441_8_f64;
        let (r1, r2) = (6878.137_f64, 7178.137_f64);
        let v = (mu / r1).sqrt();
        let req = request(&format!(
            r#"{{ "initial": {{ "epoch": "2026-01-01T00:00:00Z",
                   "positionKm": [{r1}, 0, 0], "velocityKmS": [0, {v}, 0] }},
                 "durationS": 21600, "stepS": 60,
                 "forces": {{ "gravity": [0, 0], "thirdBody": false, "srp": false, "drag": "none" }},
                 "maneuvers": [
                   {{ "epoch": "2026-01-01T00:30:00Z", "deltaVKmS": [0.07, 0, 0] }},
                   {{ "epoch": "2026-01-01T01:18:20Z", "deltaVKmS": [0.07, 0, 0], "ispS": 300 }}
                 ] }}"#
        ));
        let goals: TargetRequest = serde_json::from_str(&format!(
            r#"{{ "variables": [
                   {{ "burn": 1, "parameter": "dv1" }},
                   {{ "burn": 2, "parameter": "epoch" }},
                   {{ "burn": 2, "parameter": "dv1" }} ],
                 "goals": [
                   {{ "quantity": "flightPathAngleDeg", "at": "burn2.before", "value": 0, "tolerance": 1e-4 }},
                   {{ "quantity": "radiusKm", "at": "burn2", "value": {r2}, "tolerance": 1e-3 }},
                   {{ "quantity": "smaKm", "at": "burn2", "value": {r2}, "tolerance": 1e-3 }} ] }}"#
        ))
        .unwrap();
        let out = target(&req, &goals).unwrap();
        assert!(out.converged, "{out:?}");
        assert!(out.iterations <= 8, "{} iterations", out.iterations);
        let a = (r1 + r2) / 2.0;
        let dv1 = (mu * (2.0 / r1 - 1.0 / a)).sqrt() - v;
        let dv2 = (mu / r2).sqrt() - (mu * (2.0 / r2 - 1.0 / a)).sqrt();
        let half = std::f64::consts::PI * (a.powi(3) / mu).sqrt();
        assert!(
            (out.values[0] - dv1).abs() < 1e-6,
            "{} vs {dv1}",
            out.values[0]
        );
        assert!(
            (out.values[2] - dv2).abs() < 1e-6,
            "{} vs {dv2}",
            out.values[2]
        );
        assert!((1800.0 + 2900.0 + out.values[1] - 1800.0 - half).abs() < 0.05);
        assert!(
            out.goals
                .iter()
                .zip(&goals.goals)
                .all(|(g, w)| g.error.abs() <= w.tolerance)
        );
        // Untouched epoch kept verbatim; solved one rounded to ms.
        assert_eq!(out.maneuvers[0].epoch, "2026-01-01T00:30:00Z");
        assert!(out.maneuvers[1].epoch.starts_with("2026-01-01T01:18:"));
        assert_eq!(out.maneuvers[1].epoch.len(), 24);
        assert_eq!(out.maneuvers[1].isp_s, Some(300.0));

        for (variables, goals) in [
            (
                r#"[{ "burn": 3, "parameter": "dv1" }]"#,
                r#"[{ "quantity": "smaKm", "at": "end", "value": 7000, "tolerance": 1 }]"#,
            ),
            (
                r#"[{ "burn": 1, "parameter": "dv4" }]"#,
                r#"[{ "quantity": "smaKm", "at": "end", "value": 7000, "tolerance": 1 }]"#,
            ),
            (
                r#"[{ "burn": 1, "parameter": "dv1" }]"#,
                r#"[{ "quantity": "sma", "at": "end", "value": 7000, "tolerance": 1 }]"#,
            ),
            (
                r#"[{ "burn": 1, "parameter": "dv1" }]"#,
                r#"[{ "quantity": "smaKm", "at": "burn9", "value": 7000, "tolerance": 1 }]"#,
            ),
            (
                r#"[{ "burn": 1, "parameter": "dv1" }]"#,
                r#"[{ "quantity": "smaKm", "at": "end", "value": 7000, "tolerance": 0 }]"#,
            ),
            (
                r#"[{ "burn": 1, "parameter": "dv1" }, { "burn": 1, "parameter": "dv1" }]"#,
                r#"[{ "quantity": "smaKm", "at": "end", "value": 7000, "tolerance": 1 }]"#,
            ),
        ] {
            let bad: TargetRequest = serde_json::from_str(&format!(
                r#"{{ "variables": {variables}, "goals": {goals} }}"#
            ))
            .unwrap();
            assert!(target(&req, &bad).is_err(), "{variables} {goals}");
        }
    }
}
