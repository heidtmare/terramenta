//! Numerical propagation for the JavaScript API: request → [`terramenta_solare`] spacecraft →
//! CCSDS OEM text or SPK bytes.
//!
//! Third-body Sun/Moon come from a loaded kernel ([`load_kernel`]) or the analytic series.
//! EOP ([`load_eop`]) feed Earth orientation and TEME↔GCRF; NRLMSISE-00 drag needs
//! [`load_space_weather`]. States are GCRF, UTC epochs.

use std::sync::{Arc, LazyLock, RwLock};

use bevy::math::{DMat3, DQuat, DVec3};
use serde::{Deserialize, Serialize};
use terramenta_solare::attitude::{
    self, AlignConstrain, Direction, Geometry, Law, Profile, Spin, Steering,
};
use terramenta_solare::bodies::{GM_MOON_KM3_S2, GM_SUN_KM3_S2};
use terramenta_solare::ccsds::{
    Aem, AemMetadata, AemSegment, Cdm, CdmObject, Header, Oem, OemCovariance, OemMetadata,
    OemSegment, TimeSystem,
};
use terramenta_solare::covariance::{self, CovariancePropagation, Matrix6, ProcessNoise};
use terramenta_solare::coverage::{self, Coverage, FieldOfView, Grid, Observer, Region};
use terramenta_solare::earth::{EarthModel, TerrestrialTransform};
use terramenta_solare::eop::{EopTable, FinalsKind};
use terramenta_solare::events::{self, GroundStation, OrbitEventKind, Shadow};
use terramenta_solare::force::{
    Drag, EGM2008_GM_KM3_S2, ExponentialAtmosphere, HarrisPriester, Nrlmsise00, Plate, PointMass,
    Relativity, SolarRadiationPressure, SphericalHarmonics, ThirdBody, Vehicle,
};
use terramenta_solare::frame::Relative;
use terramenta_solare::interpolate::{Interpolation, SampledEphemeris};
use terramenta_solare::maneuver::{BurnFrame, FiniteBurn, ImpulsiveManeuver, LocalFrame};
use terramenta_solare::orbit::OrbitalElements;
use terramenta_solare::propagate::{BodyOrientation, EarthOrientation, Integrator, Method};
use terramenta_solare::sequence::{self, Quantity};
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

pub(crate) fn environment() -> std::sync::RwLockReadGuard<'static, Environment> {
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
    /// Linear covariance from the propagation start.
    #[serde(default)]
    pub covariance: Option<CovarianceOptions>,
    /// Attitude flown for `vehicle.plates` and `"body"` finite burns (validated even without
    /// them).
    #[serde(default)]
    pub attitude: Option<AttitudeLaw>,
}

/// Initial `[r, v]` covariance at the propagation start: sigmas or a full matrix.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CovarianceOptions {
    /// rsw (default; ric, rtn) | vnb | lvlh | gcrf.
    #[serde(default)]
    pub frame: Option<String>,
    /// 1σ per frame axis, km.
    #[serde(default)]
    pub position_sigma_km: Option<[f64; 3]>,
    /// 1σ per frame axis, km/s.
    #[serde(default)]
    pub velocity_sigma_km_s: Option<[f64; 3]>,
    /// Lower triangle row by row (21), km², km²/s, km²/s². Instead of sigmas.
    #[serde(default)]
    pub lower_triangle: Option<Vec<f64>>,
    /// White acceleration noise spectral density per frame axis, km²/s³.
    #[serde(default)]
    pub process_noise_km2_s3: Option<[f64; 3]>,
}

impl CovarianceOptions {
    /// `(frame, P₀ in frame, noise)`.
    pub(crate) fn resolve(&self) -> Result<(LocalFrame, Matrix6, Option<ProcessNoise>), String> {
        let frame = match self
            .frame
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("rsw" | "ric" | "rtn") => LocalFrame::Rsw,
            Some(other) => local_frame(Some(other)).map_err(|_| {
                format!("covariance.frame: expected rsw, vnb, lvlh or gcrf, got {other:?}")
            })?,
        };
        let bad = |v: &[f64]| v.iter().any(|x| !x.is_finite() || *x < 0.0);
        let p0 = match (
            &self.lower_triangle,
            self.position_sigma_km,
            self.velocity_sigma_km_s,
        ) {
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                return Err("covariance: lowerTriangle or sigmas, not both".into());
            }
            (Some(t), None, None) => {
                let t: [f64; 21] = t
                    .as_slice()
                    .try_into()
                    .map_err(|_| "covariance.lowerTriangle: expected 21 values".to_string())?;
                let p = covariance::from_lower_triangle(&t);
                let diag: [f64; 6] = std::array::from_fn(|i| p[i][i]);
                let correlated = (0..6).any(|i| {
                    (0..i).any(|j| p[i][j].abs() > (1.0 + 1e-9) * (diag[i] * diag[j]).sqrt())
                });
                if t.iter().any(|x| !x.is_finite()) || bad(&diag) || correlated {
                    return Err("covariance.lowerTriangle: not a covariance matrix".into());
                }
                p
            }
            (None, None, None) => {
                return Err(
                    "covariance: needs positionSigmaKm/velocitySigmaKmS or lowerTriangle".into(),
                );
            }
            (None, r, v) => {
                let (r, v) = (r.unwrap_or_default(), v.unwrap_or_default());
                if bad(&r) || bad(&v) {
                    return Err("covariance: sigmas must be finite and non-negative".into());
                }
                covariance::diagonal([r[0], r[1], r[2], v[0], v[1], v[2]].map(|x| x * x))
            }
        };
        let noise = match self.process_noise_km2_s3 {
            Some(q) if bad(&q) => {
                return Err("covariance.processNoiseKm2S3 must be finite and non-negative".into());
            }
            Some(q) => Some(ProcessNoise {
                psd_km2_s3: q.into(),
                frame,
            }),
            None => None,
        };
        Ok((frame, p0, noise))
    }
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
    /// Osculating Keplerian elements, GCRF, EGM2008 GM.
    Elements {
        epoch: String,
        elements: ElementsOptions,
    },
}

impl Initial {
    /// Epoch of a numerical initial state; `None` for TLE/OMM.
    fn epoch(&self) -> Option<&str> {
        match self {
            Self::State { epoch, .. } | Self::Elements { epoch, .. } => Some(epoch),
            Self::Tle { .. } | Self::Omm { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementsOptions {
    pub sma_km: f64,
    pub eccentricity: f64,
    pub inclination_deg: f64,
    pub raan_deg: f64,
    pub arg_perigee_deg: f64,
    pub true_anomaly_deg: f64,
}

impl ElementsOptions {
    fn state(&self) -> Result<StateVector, String> {
        let angles = [
            self.inclination_deg,
            self.raan_deg,
            self.arg_perigee_deg,
            self.true_anomaly_deg,
        ];
        if !(self.sma_km.is_finite() && angles.iter().all(|a| a.is_finite())) {
            return Err("elements: values must be finite".into());
        }
        if !(0.0..1.0).contains(&self.eccentricity) {
            return Err("elements: need 0 ≤ eccentricity < 1".into());
        }
        if self.sma_km * (1.0 - self.eccentricity) <= EARTH_RADIUS_KM {
            return Err("elements: perigee below the surface".into());
        }
        Ok(OrbitalElements {
            semi_major_axis_km: self.sma_km,
            eccentricity: self.eccentricity,
            inclination_rad: self.inclination_deg.to_radians(),
            raan_rad: self.raan_deg.to_radians(),
            arg_periapsis_rad: self.arg_perigee_deg.to_radians(),
            mean_anomaly_at_epoch: 0.0,
            epoch_seconds: 0.0,
        }
        .state_at_true_anomaly(EGM2008_GM_KM3_S2, self.true_anomaly_deg.to_radians()))
    }
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
    /// Flat plates, body axes: replace `dragAreaM2`, `srpAreaM2`, `reflectivity`.
    pub plates: Vec<PlateOptions>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlateOptions {
    pub normal: [f64; 3],
    pub area_m2: f64,
    #[serde(default)]
    pub specular: f64,
    #[serde(default)]
    pub diffuse: f64,
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
            plates: Vec::new(),
        }
    }
}

impl VehicleOptions {
    fn resolve(&self) -> Result<Vehicle, String> {
        let d = Vehicle::default();
        let plates = self
            .plates
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let normal = DVec3::from_array(p.normal)
                    .try_normalize()
                    .ok_or_else(|| format!("plates[{i}]: normal must be non-zero"))?;
                let unit = |x: f64| (0.0..=1.0).contains(&x);
                if !(p.area_m2.is_finite() && p.area_m2 >= 0.0) {
                    return Err(format!("plates[{i}]: areaM2 must be ≥ 0"));
                }
                if !(unit(p.specular) && unit(p.diffuse) && unit(p.specular + p.diffuse)) {
                    return Err(format!(
                        "plates[{i}]: specular, diffuse and their sum must be in [0, 1]"
                    ));
                }
                Ok(Plate {
                    normal,
                    area_m2: p.area_m2,
                    specular: p.specular,
                    diffuse: p.diffuse,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Vehicle {
            dry_mass_kg: self.dry_mass_kg.unwrap_or(d.dry_mass_kg.min(self.mass_kg)),
            drag_area_m2: self.drag_area_m2.unwrap_or(d.drag_area_m2),
            drag_coefficient: self.drag_coefficient.unwrap_or(d.drag_coefficient),
            srp_area_m2: self.srp_area_m2.unwrap_or(d.srp_area_m2),
            reflectivity: self.reflectivity.unwrap_or(d.reflectivity),
            plates,
        })
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
    /// GCRF, one per state; empty without `covariance`.
    pub covariances: Vec<Matrix6>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub covariance: Option<CovarianceInfo>,
}

/// Per sample, in sample order.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CovarianceInfo {
    /// Unix s, UTC.
    pub times: Vec<f64>,
    /// 1σ RSW: radial, in-track, cross-track (km), then their rates (km/s).
    pub sigma_rsw: Vec<[f64; 6]>,
    /// 1σ position ellipsoid semi-axes, km, descending.
    pub ellipsoid_km: Vec<[f64; 3]>,
}

/// Now, UTC, `YYYY-MM-DDThh:mm:ss.sss` (CCSDS `CREATION_DATE`).
pub(crate) fn creation_date() -> String {
    #[cfg(target_arch = "wasm32")]
    let iso = String::from(js_sys::Date::new_0().to_iso_string());
    #[cfg(not(target_arch = "wasm32"))]
    let iso = {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        TimeSystem::Utc.format(Epoch::from_unix_seconds(unix))
    };
    iso.trim_end_matches('Z').chars().take(23).collect()
}

pub(crate) fn epoch(s: &str) -> Result<Epoch, String> {
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
        Some("body") => return Err("frame \"body\" is for finite burns only".into()),
        Some(other) => return Err(format!("unknown maneuver frame {other:?}")),
    })
}

/// [`local_frame`], or `"body"`.
fn burn_frame(s: Option<&str>) -> Result<BurnFrame, String> {
    match s {
        Some(f) if f.eq_ignore_ascii_case("body") => Ok(BurnFrame::Body),
        _ => local_frame(s).map(BurnFrame::Local),
    }
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

pub(crate) fn propagator(
    req: &PropagationRequest,
    env: &Environment,
) -> Result<Propagator, String> {
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
    if let Some(law) = &req.attitude {
        p = p.with_attitude(steering(req, law)?);
    }
    if f.third_body
        || f.srp
        || req.attitude.is_some()
        || matches!(f.drag, DragModel::HarrisPriester)
    {
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
            frame: burn_frame(b.frame.as_deref())?,
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
    let (sc, start) = spacecraft_at_start(req, &env)?;
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
    let (states, covariances) = match &req.covariance {
        None => (
            sc.samples(stop, step).map_err(|e| e.to_string())?,
            Vec::new(),
        ),
        Some(options) => {
            let (frame, p0, noise) = options.resolve()?;
            let mut p = numerical_propagator(&sc);
            let s0 = *sc.state();
            let p0 = covariance::from_local(&p0, frame, s0.position_km, s0.velocity_km_s);
            let mut linear = CovariancePropagation::new(p0, noise);
            let (mut states, mut covariances) = (Vec::new(), Vec::new());
            p.propagate_stm_with(&s0, stop, step, |s, phi| {
                let t = s.epoch.seconds_since(s0.epoch);
                covariances.push(linear.sample(t, phi, s.position_km, s.velocity_km_s));
                states.push(*s);
            })
            .map_err(|e| e.to_string())?;
            (states, covariances)
        }
    };
    let name = req.name.clone().unwrap_or_else(|| "SPACECRAFT".into());
    Ok(Propagated {
        object_id: req.object_id.clone().unwrap_or_else(|| name.clone()),
        name,
        naif_id: req.naif_id.unwrap_or(DEFAULT_NAIF_ID),
        states,
        covariances,
    })
}

/// The Cowell propagator of a spacecraft from [`spacecraft_at_start`], with its vehicle.
pub(crate) fn numerical_propagator(sc: &Spacecraft) -> Propagator {
    let Propagation::Numerical(p) = sc.propagation() else {
        unreachable!("numerical from start");
    };
    let mut p = (**p).clone();
    p.vehicle = sc.vehicle.clone();
    p
}

/// The spacecraft at `start` (default: initial epoch), numerical from there, and `start`.
pub(crate) fn spacecraft_at_start(
    req: &PropagationRequest,
    env: &Environment,
) -> Result<(Spacecraft, Epoch), String> {
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
    let numerical = || Ok::<_, String>(Propagation::Numerical(Box::new(propagator(req, env)?)));
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
        .with_vehicle(req.vehicle.resolve()?),
        Initial::Elements { epoch: e, elements } => Spacecraft::new(
            "",
            SpacecraftState::new(epoch(e)?, elements.state()?, mass),
            numerical()?,
        )
        .with_vehicle(req.vehicle.resolve()?),
    };
    let start = req
        .start
        .as_deref()
        .map(epoch)
        .transpose()?
        .unwrap_or(sc.epoch());
    // SGP4 carries TLE/OMM to `start`; a state is numerical from its own epoch so
    // maneuvers between it and `start` apply.
    sc.step_to(start).map_err(|e| e.to_string())?;
    if req.initial.epoch().is_none() {
        sc.vehicle = req.vehicle.resolve()?;
        sc.set_propagation(numerical()?);
    }
    Ok((sc, start))
}

/// [`propagate`] without covariance, for analyses of the trajectory alone.
pub fn trajectory(req: &PropagationRequest) -> Result<Propagated, String> {
    match req.covariance {
        None => propagate(req),
        Some(_) => propagate(&PropagationRequest {
            covariance: None,
            ..req.clone()
        }),
    }
}

/// Maneuver arcs: split at impulsive maneuvers (repeated epochs); single-state arcs dropped.
fn arcs<T>(states: &[(Epoch, T)]) -> impl Iterator<Item = &[(Epoch, T)]> {
    states.chunk_by(|a, b| a.0 != b.0).filter(|g| g.len() >= 2)
}

impl Propagated {
    /// Time-ordered states.
    pub(crate) fn ordered_states(&self) -> Vec<(Epoch, StateVector)> {
        self.ordered()
            .into_iter()
            .map(|(e, (s, _))| (e, s))
            .collect()
    }

    /// Time-ordered states with their covariance, if any.
    fn ordered(&self) -> Vec<(Epoch, (StateVector, Option<Matrix6>))> {
        let mut states: Vec<_> = self
            .states
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    s.epoch,
                    (s.state_vector(), self.covariances.get(i).copied()),
                )
            })
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
            header: Header {
                creation_date: creation_date(),
                ..Header::default()
            },
            segments: arcs(&self.ordered())
                .map(|arc| OemSegment {
                    metadata: metadata.clone(),
                    states: arc.iter().map(|(e, (s, _))| (*e, *s)).collect(),
                    covariances: arc
                        .iter()
                        .filter_map(|(e, (_, p))| {
                            p.map(|matrix| OemCovariance {
                                epoch: *e,
                                ref_frame: None,
                                matrix,
                            })
                        })
                        .collect(),
                })
                .collect(),
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

    /// `(unix s, TEME position covariance, km²)` per sample; empty without covariance.
    pub fn position_covariance_teme(&self) -> Vec<(f64, [[f64; 3]; 3])> {
        let env = environment();
        let mut grid = terramenta_solare::earth::TemeGrid::new(|e| {
            env.eop.as_ref().map(|t| t.at(e)).unwrap_or_default()
        });
        let mut out: Vec<_> = self
            .states
            .iter()
            .zip(&self.covariances)
            .map(|(s, p)| {
                // TEME → GCRF.
                let m = grid.at(s.epoch).to_cols_array_2d();
                let gcrf: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| p[i][j]));
                let teme = std::array::from_fn(|i| {
                    std::array::from_fn(|j| {
                        (0..3)
                            .flat_map(|k| (0..3).map(move |l| (k, l)))
                            .map(|(k, l)| m[i][k] * gcrf[k][l] * m[j][l])
                            .sum()
                    })
                });
                (s.epoch.to_unix_seconds(), teme)
            })
            .collect();
        if out.first().map(|s| s.0) > out.last().map(|s| s.0) {
            out.reverse();
        }
        out
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
            covariance: (!self.covariances.is_empty()).then(|| CovarianceInfo {
                times: self
                    .states
                    .iter()
                    .map(|s| s.epoch.to_unix_seconds())
                    .collect(),
                sigma_rsw: self
                    .states
                    .iter()
                    .zip(&self.covariances)
                    .map(|(s, p)| {
                        let local = covariance::to_local(
                            p,
                            LocalFrame::Rsw,
                            s.position_km,
                            s.velocity_km_s,
                        );
                        covariance::sigmas(&local)
                    })
                    .collect(),
                ellipsoid_km: self
                    .covariances
                    .iter()
                    .map(|p| covariance::position_ellipsoid(p).0)
                    .collect(),
            }),
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StationOptions {
    pub id: String,
    pub lat_deg: f64,
    pub lon_deg: f64,
    #[serde(default)]
    pub alt_km: f64,
    #[serde(default)]
    pub min_elevation_deg: f64,
    /// Horizon mask `[azimuthDeg, elevationDeg]`, azimuth ascending in `[0, 360)`.
    #[serde(default)]
    pub horizon: Vec<[f64; 2]>,
}

impl StationOptions {
    pub(crate) fn station(&self) -> Result<GroundStation, String> {
        let bad = |what: &str| Err(format!("station {:?}: bad {what}", self.id));
        if !(-90.0..=90.0).contains(&self.lat_deg) || !self.lon_deg.is_finite() {
            return bad("latitude/longitude");
        }
        let ascending = self.horizon.windows(2).all(|w| w[0][0] < w[1][0]);
        let in_range = self
            .horizon
            .iter()
            .all(|&[az, el]| (0.0..360.0).contains(&az) && (-90.0..=90.0).contains(&el));
        if !(ascending && in_range) {
            return bad("horizon (azimuth ascending in [0, 360), elevation in [-90, 90])");
        }
        Ok(GroundStation {
            name: self.id.clone(),
            latitude_rad: self.lat_deg.to_radians(),
            longitude_rad: self.lon_deg.to_radians(),
            altitude_km: self.alt_km,
            min_elevation_rad: self.min_elevation_deg.to_radians(),
            horizon: self
                .horizon
                .iter()
                .map(|&[az, el]| (az.to_radians(), el.to_radians()))
                .collect(),
        })
    }
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

/// GCRF → ITRF: full Earth orientation on an [`ORIENTATION_GRID_S`] grid from `start`,
/// Earth rotation exact in between.
struct Terrestrial {
    orientation: EarthOrientation,
    start: Epoch,
    anchors: std::collections::HashMap<i64, TerrestrialTransform>,
}

impl Terrestrial {
    fn new(env: &Environment, start: Epoch) -> Self {
        Self {
            orientation: EarthOrientation::new(env.eop.clone(), EarthModel::Iau2006Cio),
            start,
            anchors: std::collections::HashMap::new(),
        }
    }

    fn rotation(&mut self, e: Epoch) -> DMat3 {
        let t = e.seconds_since(self.start);
        let k = (t / ORIENTATION_GRID_S).floor() as i64;
        let (orientation, start) = (&self.orientation, self.start);
        let anchor = *self.anchors.entry(k).or_insert_with(|| {
            orientation.transform(start.advanced_by_seconds(k as f64 * ORIENTATION_GRID_S))
        });
        anchor
            .advanced_by(t - k as f64 * ORIENTATION_GRID_S)
            .rotation()
    }
}

/// Sub-satellite points every `step_s` over the propagated span (and at its end):
/// ITRF, WGS-84 geodetic, IAU 2006/2000A with loaded EOP.
#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct GroundTrack {
    /// Unix seconds (UTC).
    pub times: Vec<f64>,
    /// `[lonDeg, latDeg, heightKm]`, longitude in [−180, 180).
    pub coords: Vec<[f64; 3]>,
}

pub fn ground_track(propagated: &Propagated, step_s: f64) -> Result<GroundTrack, String> {
    let states = propagated.ordered_states();
    if states.len() < 2 {
        return Ok(GroundTrack::default());
    }
    let (start, stop) = (states[0].0, states[states.len() - 1].0);
    let span = stop.seconds_since(start);
    if step_s.is_nan() || step_s <= 0.0 || span / step_s > MAX_EVENT_SAMPLES {
        return Err(format!(
            "stepS must be positive and yield ≤ {MAX_EVENT_SAMPLES} samples"
        ));
    }
    let ephemeris = SampledEphemeris::new(states, Interpolation::default());
    let env = environment();
    let mut earth = Terrestrial::new(&env, start);
    let n = (span / step_s).floor() as usize;
    let mut offsets: Vec<f64> = (0..=n).map(|k| k as f64 * step_s).collect();
    if offsets[n] < span {
        offsets.push(span);
    }
    let mut out = GroundTrack::default();
    for t in offsets {
        let e = start.advanced_by_seconds(t);
        let (lat, lon, h) = terramenta_solare::earth::ecef_to_geodetic(
            earth.rotation(e) * ephemeris.position_at(e),
        );
        let lon = (lon.to_degrees() + 180.0).rem_euclid(360.0) - 180.0;
        out.times.push(e.to_unix_seconds());
        out.coords.push([lon, lat.to_degrees(), h]);
    }
    Ok(out)
}

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
        let mut earth = Terrestrial::new(&env, start);
        let mut itrf = |e: Epoch| earth.rotation(e) * ephemeris.position_at(e);
        for s in &req.stations {
            let station = s.station()?;
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
        if Quantity::from_name(&g.quantity).is_none() {
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
            if let (Some(latest), Some(e)) = (latest, r.initial.epoch()) {
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
        let out = trajectory(&r)?;
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
                let q = Quantity::from_name(&g.quantity).expect("validated");
                let (achieved, error) = (q.of(&state), q.error(&state, g.value));
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

// --- Mission sequence -----------------------------------------------------------------

/// `runSequence` command: optional `label`, and one of `propagate`, `maneuver`,
/// `finiteBurn`, `target`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagate: Option<PropagateOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maneuver: Option<SequenceManeuverOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finite_burn: Option<SequenceBurnOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<SequenceTargetOptions>,
}

/// One of `durationS`, `epoch` (UTC), `to` (`periapsis` | `apoapsis` | `ascendingNode` |
/// `descendingNode`), or `quantity` + `value` (+ `direction`: `increasing` | `decreasing` |
/// `either`, default either). Crossings: `count`-th occurrence, bracketed every `stepS`,
/// searched up to `maxDurationS`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PropagateOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_s: Option<f64>,
}

/// Impulse at the current epoch.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequenceManeuverOptions {
    pub delta_v_km_s: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isp_s: Option<f64>,
}

/// Thrust from the current epoch for `durationS`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequenceBurnOptions {
    pub duration_s: f64,
    pub thrust_n: f64,
    pub isp_s: f64,
    pub direction: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
}

/// Varies labelled commands in `sequence` until `goals` hold at its end.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequenceTargetOptions {
    pub variables: Vec<SequenceVariable>,
    pub goals: Vec<SequenceGoal>,
    pub sequence: Vec<CommandOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<usize>,
}

/// `parameter`: `dv1`–`dv3` (maneuver, km/s in its frame) or `durationS` (finite burn, or
/// a `durationS` propagate). Defaults: perturbation 1e-5 km/s / 1 s, maxStep 0.5 km/s / 600 s.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequenceVariable {
    pub command: String,
    pub parameter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perturbation: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_step: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequenceGoal {
    pub quantity: String,
    pub value: f64,
    pub tolerance: f64,
}

/// `runSequence` result.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SequenceResult {
    /// The input propagation with `maneuvers`, `finiteBurns` and `stop` resolved; feeds
    /// `propagate` and every other analysis unchanged.
    pub propagation: serde_json::Value,
    /// Run order; a target's body follows it at `depth + 1`.
    pub segments: Vec<SegmentInfo>,
    /// Impulsive plus finite (rocket equation), km/s.
    pub delta_v_km_s: f64,
    pub final_mass_kg: f64,
    /// Every target converged.
    pub converged: bool,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SegmentInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `propagate` | `maneuver` | `finiteBurn` | `target`.
    pub kind: &'static str,
    pub depth: usize,
    /// Unix s.
    pub start: f64,
    pub stop: f64,
    pub mass_kg: f64,
    /// Maneuver or finite burn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_v_km_s: Option<f64>,
    /// At `stop`, osculating.
    pub elements: SegmentElements,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetInfo>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SegmentElements {
    pub sma_km: f64,
    pub eccentricity: f64,
    pub inclination_deg: f64,
    pub raan_deg: f64,
    pub arg_perigee_deg: f64,
    pub true_anomaly_deg: f64,
    pub altitude_km: f64,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    pub converged: bool,
    pub iterations: usize,
    pub history: Vec<f64>,
    /// Request order.
    pub values: Vec<f64>,
    pub goals: Vec<SequenceGoalResult>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SequenceGoalResult {
    pub quantity: String,
    pub value: f64,
    pub achieved: f64,
    pub error: f64,
}

const DEFAULT_SEQUENCE_STEP_S: f64 = 60.0;
const DEFAULT_MAX_DURATION_S: f64 = 30.0 * 86_400.0;

fn frame_name(frame: LocalFrame) -> &'static str {
    match frame {
        LocalFrame::Inertial => "inertial",
        LocalFrame::Vnb => "vnb",
        LocalFrame::Rsw => "rsw",
        LocalFrame::Lvlh => "lvlh",
    }
}

fn burn_frame_name(frame: BurnFrame) -> &'static str {
    match frame {
        BurnFrame::Local(f) => frame_name(f),
        BurnFrame::Body => "body",
    }
}

fn sequence_commands(
    commands: &[CommandOptions],
    path: &str,
) -> Result<Vec<sequence::Command>, String> {
    use sequence::{Action, Direction, Goal, Parameter, Stop, Target, TargetVariable};
    commands
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let at = format!("{path}[{k}]");
            let given = [
                c.propagate.is_some(),
                c.maneuver.is_some(),
                c.finite_burn.is_some(),
                c.target.is_some(),
            ];
            if given.iter().filter(|g| **g).count() != 1 {
                return Err(format!(
                    "{at}: need one of propagate, maneuver, finiteBurn, target"
                ));
            }
            let action = if let Some(p) = &c.propagate {
                let stops = [
                    p.duration_s.is_some(),
                    p.epoch.is_some(),
                    p.to.is_some(),
                    p.quantity.is_some(),
                ];
                if stops.iter().filter(|g| **g).count() != 1 {
                    return Err(format!(
                        "{at}.propagate: need one of durationS, epoch, to, quantity"
                    ));
                }
                let stop = if let Some(d) = p.duration_s {
                    Stop::Duration(d)
                } else if let Some(e) = &p.epoch {
                    Stop::Epoch(epoch(e)?)
                } else if let Some(to) = &p.to {
                    match to.as_str() {
                        "periapsis" => Stop::periapsis(),
                        "apoapsis" => Stop::apoapsis(),
                        "ascendingNode" => Stop::ascending_node(),
                        "descendingNode" => Stop::descending_node(),
                        other => return Err(format!("{at}.propagate.to: unknown {other:?}")),
                    }
                } else {
                    let name = p.quantity.as_deref().unwrap_or_default();
                    let quantity = Quantity::from_name(name)
                        .ok_or(format!("{at}.propagate.quantity: unknown {name:?}"))?;
                    let value = p
                        .value
                        .ok_or(format!("{at}.propagate: quantity needs value"))?;
                    let direction = match p.direction.as_deref() {
                        None | Some("either") => Direction::Either,
                        Some("increasing") => Direction::Increasing,
                        Some("decreasing") => Direction::Decreasing,
                        Some(other) => {
                            return Err(format!("{at}.propagate.direction: unknown {other:?}"));
                        }
                    };
                    Stop::crossing(quantity, value, direction)
                };
                Action::Propagate {
                    stop,
                    count: p.count.unwrap_or(1),
                    step_s: p.step_s.unwrap_or(DEFAULT_SEQUENCE_STEP_S),
                    max_duration_s: p.max_duration_s.unwrap_or(DEFAULT_MAX_DURATION_S),
                }
            } else if let Some(m) = &c.maneuver {
                Action::Maneuver {
                    delta_v_km_s: m.delta_v_km_s.into(),
                    frame: local_frame(m.frame.as_deref())?,
                    isp_s: m.isp_s,
                }
            } else if let Some(b) = &c.finite_burn {
                Action::FiniteBurn {
                    duration_s: b.duration_s,
                    thrust_n: b.thrust_n,
                    isp_s: b.isp_s,
                    direction: b.direction.into(),
                    frame: burn_frame(b.frame.as_deref())?,
                }
            } else {
                let t = c.target.as_ref().expect("one given");
                let variables = t
                    .variables
                    .iter()
                    .enumerate()
                    .map(|(j, v)| {
                        let (parameter, perturbation, max_step) = match v.parameter.as_str() {
                            "dv1" => (Parameter::DeltaV(0), 1e-5, 0.5),
                            "dv2" => (Parameter::DeltaV(1), 1e-5, 0.5),
                            "dv3" => (Parameter::DeltaV(2), 1e-5, 0.5),
                            "durationS" => (Parameter::Duration, 1.0, 600.0),
                            other => {
                                return Err(format!(
                                    "{at}.target.variables[{j}]: unknown parameter {other:?}"
                                ));
                            }
                        };
                        Ok(TargetVariable {
                            command: v.command.clone(),
                            parameter,
                            perturbation: v.perturbation.unwrap_or(perturbation),
                            max_step: v.max_step.unwrap_or(max_step),
                        })
                    })
                    .collect::<Result<_, String>>()?;
                let goals = t
                    .goals
                    .iter()
                    .enumerate()
                    .map(|(j, g)| {
                        Ok(Goal {
                            quantity: Quantity::from_name(&g.quantity).ok_or(format!(
                                "{at}.target.goals[{j}]: unknown quantity {:?}",
                                g.quantity
                            ))?,
                            value: g.value,
                            tolerance: g.tolerance,
                        })
                    })
                    .collect::<Result<_, String>>()?;
                Action::Target(Box::new(Target {
                    variables,
                    goals,
                    body: sequence_commands(&t.sequence, &format!("{at}.target.sequence"))?,
                    max_iterations: t.max_iterations.unwrap_or_else(default_max_iterations),
                }))
            };
            Ok(sequence::Command {
                label: c.label.clone(),
                action,
            })
        })
        .collect()
}

/// Runs `commands` from `propagation`'s state at `start` (its `stop`/`durationS` ignored;
/// `maneuvers`/`finiteBurns` must be empty). A maneuver as the last command is reported but
/// not in the resolved propagation (maneuvers at the stop epoch do not fire).
pub fn run_sequence(
    propagation: &serde_json::Value,
    commands: &[CommandOptions],
) -> Result<SequenceResult, String> {
    let mut req: PropagationRequest =
        serde_json::from_value(propagation.clone()).map_err(|e| format!("propagation: {e}"))?;
    if !req.maneuvers.is_empty() || !req.finite_burns.is_empty() {
        return Err("propagation: maneuvers and finiteBurns come from the sequence".into());
    }
    if commands.is_empty() {
        return Err("sequence is empty".into());
    }
    let commands = sequence_commands(commands, "sequence")?;
    req.covariance = None;
    let env = environment();
    let (sc, _) = spacecraft_at_start(&req, &env)?;
    drop(env);
    let run = sequence::run(&numerical_propagator(&sc), *sc.state(), &commands)
        .map_err(|e| e.to_string())?;

    let mut resolved = propagation.clone();
    let object = resolved
        .as_object_mut()
        .ok_or("propagation: expected an object")?;
    object.remove("durationS");
    object.insert(
        "stop".into(),
        serde_json::Value::String(utc_iso(run.end.epoch)),
    );
    let maneuvers: Vec<ManeuverOptions> = run
        .maneuvers
        .iter()
        .map(|m| ManeuverOptions {
            epoch: utc_iso(m.epoch),
            delta_v_km_s: m.delta_v_km_s.to_array(),
            frame: Some(frame_name(m.frame).into()),
            isp_s: m.isp_s,
        })
        .collect();
    let burns: Vec<FiniteBurnOptions> = run
        .finite_burns
        .iter()
        .map(|b| FiniteBurnOptions {
            start: utc_iso(b.start),
            duration_s: b.duration_s,
            thrust_n: b.thrust_n,
            isp_s: b.isp_s,
            direction: b.direction.to_array(),
            frame: Some(burn_frame_name(b.frame).into()),
        })
        .collect();
    object.insert(
        "maneuvers".into(),
        serde_json::to_value(maneuvers).expect("serializable"),
    );
    object.insert(
        "finiteBurns".into(),
        serde_json::to_value(burns).expect("serializable"),
    );

    let g0 = terramenta_solare::force::G0_M_S2 * 1e-3;
    let mut total = 0.0;
    let mut converged = true;
    let segments = run
        .segments
        .iter()
        .map(|s| {
            let e = &s.end;
            let (kind, delta_v, target) = match &s.kind {
                sequence::SegmentKind::Propagate => ("propagate", None, None),
                sequence::SegmentKind::Maneuver(_, dv) => ("maneuver", Some(dv.length()), None),
                sequence::SegmentKind::FiniteBurn(b) => (
                    "finiteBurn",
                    Some(b.isp_s * g0 * (s.start.mass_kg / s.end.mass_kg).ln()),
                    None,
                ),
                sequence::SegmentKind::Target(t) => {
                    converged &= t.converged;
                    let goals = t
                        .goals
                        .iter()
                        .map(|g| SequenceGoalResult {
                            quantity: g.quantity.name().into(),
                            value: g.value,
                            achieved: g.achieved,
                            error: g.error,
                        })
                        .collect();
                    (
                        "target",
                        None,
                        Some(TargetInfo {
                            converged: t.converged,
                            iterations: t.iterations,
                            history: t.history.clone(),
                            values: t.values.clone(),
                            goals,
                        }),
                    )
                }
            };
            total += delta_v.unwrap_or(0.0);
            SegmentInfo {
                label: s.label.clone(),
                kind,
                depth: s.depth,
                start: s.start.epoch.to_unix_seconds(),
                stop: e.epoch.to_unix_seconds(),
                mass_kg: e.mass_kg,
                delta_v_km_s: delta_v,
                elements: SegmentElements {
                    sma_km: Quantity::SmaKm.of(e),
                    eccentricity: Quantity::Eccentricity.of(e),
                    inclination_deg: Quantity::InclinationDeg.of(e),
                    raan_deg: Quantity::RaanDeg.of(e),
                    arg_perigee_deg: Quantity::ArgPerigeeDeg.of(e),
                    true_anomaly_deg: Quantity::TrueAnomalyDeg.of(e),
                    altitude_km: Quantity::AltitudeKm.of(e),
                },
                target,
            }
        })
        .collect();
    Ok(SequenceResult {
        propagation: resolved,
        segments,
        delta_v_km_s: total,
        final_mass_kg: run.end.mass_kg,
        converged,
    })
}

// --- Attitude -------------------------------------------------------------------------

/// An attitude law: `quaternion`, or `primary` + `secondary` (align/constrain); `spin`
/// on top of either.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttitudeLaw {
    /// Body → GCRF, `[x, y, z, w]`.
    pub quaternion: Option<[f64; 4]>,
    pub primary: Option<PointingOptions>,
    pub secondary: Option<PointingOptions>,
    pub spin: Option<SpinOptions>,
    /// Site for `"target"`.
    pub target: Option<SiteOptions>,
}

/// `attitude` options: an [`AttitudeLaw`], sensors and sampling.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttitudeRequest {
    #[serde(flatten)]
    pub law: AttitudeLaw,
    /// Body axes whose angle to a direction is reported per sample.
    #[serde(default)]
    pub sensors: Vec<PointingOptions>,
    #[serde(default = "default_attitude_step")]
    pub step_s: f64,
    /// Return CCSDS AEM KVN.
    #[serde(default)]
    pub aem: bool,
}

fn default_attitude_step() -> f64 {
    60.0
}

/// Body axis `body` toward `to`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointingOptions {
    pub body: [f64; 3],
    pub to: DirectionOptions,
}

/// A [`Direction`] name, or a fixed GCRF vector.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DirectionOptions {
    Named(String),
    Inertial([f64; 3]),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpinOptions {
    pub axis: [f64; 3],
    pub rate_deg_s: f64,
    /// Zero-angle epoch; default the first sample.
    pub epoch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteOptions {
    pub lat_deg: f64,
    pub lon_deg: f64,
    #[serde(default)]
    pub alt_km: f64,
}

/// Direction names `attitude` accepts.
pub const DIRECTION_NAMES: [&str; 9] = [
    "nadir",
    "zenith",
    "velocity",
    "antiVelocity",
    "orbitNormal",
    "antiOrbitNormal",
    "sun",
    "antiSun",
    "target",
];

fn direction(d: &DirectionOptions) -> Result<Direction, String> {
    Ok(match d {
        DirectionOptions::Inertial(v) => {
            let v = DVec3::from_array(*v);
            if !(v.length() > 0.0 && v.is_finite()) {
                return Err("inertial direction must be non-zero".into());
            }
            Direction::Inertial(v)
        }
        DirectionOptions::Named(name) => match name.as_str() {
            "nadir" => Direction::Nadir,
            "zenith" => Direction::Zenith,
            "velocity" => Direction::Velocity,
            "antiVelocity" => Direction::AntiVelocity,
            "orbitNormal" => Direction::OrbitNormal,
            "antiOrbitNormal" => Direction::AntiOrbitNormal,
            "sun" => Direction::Sun,
            "antiSun" => Direction::AntiSun,
            "target" => Direction::Target,
            other => {
                return Err(format!(
                    "unknown direction {other:?} (one of {})",
                    DIRECTION_NAMES.join(", ")
                ));
            }
        },
    })
}

fn body_axis(v: [f64; 3], what: &str) -> Result<DVec3, String> {
    DVec3::from_array(v)
        .try_normalize()
        .ok_or_else(|| format!("{what}: body axis must be non-zero"))
}

/// Attitude samples. Times Unix seconds (UTC); samples where the law is undefined
/// (parallel directions) are dropped and counted.
#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct AttitudeSamples {
    pub times: Vec<f64>,
    /// Body → GCRF, `[x, y, z, w]`.
    pub quaternions: Vec<[f64; 4]>,
    /// Body relative to LVLH, 3-2-1 `[roll, pitch, yaw]`, degrees.
    pub euler_deg: Vec<[f64; 3]>,
    /// Per sensor, per sample: angle between its body axis and its direction, degrees.
    pub sensor_angles_deg: Vec<Vec<f64>>,
    pub undefined: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aem: Option<String>,
    /// Body → TEME per sample, for drawing.
    #[serde(skip)]
    pub teme: Vec<(f64, DQuat)>,
}

fn profile(att: &AttitudeLaw, start: Epoch) -> Result<Profile, String> {
    let law = match (&att.quaternion, &att.primary, &att.secondary) {
        (Some(q), None, None) => {
            let q = DQuat::from_array(*q);
            if !(q.length() > 0.0 && q.is_finite()) {
                return Err("quaternion must be non-zero".into());
            }
            Law::Inertial(q.normalize())
        }
        (None, Some(p), Some(s)) => Law::AlignConstrain(AlignConstrain {
            primary: body_axis(p.body, "primary")?,
            primary_to: direction(&p.to)?,
            secondary: body_axis(s.body, "secondary")?,
            secondary_to: direction(&s.to)?,
        }),
        _ => return Err("give quaternion, or primary and secondary".into()),
    };
    let spin = match &att.spin {
        None => None,
        Some(s) => {
            if !s.rate_deg_s.is_finite() {
                return Err("spin rateDegS must be finite".into());
            }
            Some(Spin {
                axis: body_axis(s.axis, "spin")?,
                rate_rad_s: s.rate_deg_s.to_radians(),
                epoch: s.epoch.as_deref().map(epoch).transpose()?.unwrap_or(start),
            })
        }
    };
    Ok(Profile { law, spin })
}

/// An [`AttitudeRequest`]'s law, target site (ITRF, km) and sensors, validated.
struct Pointing {
    profile: Profile,
    site: Option<DVec3>,
    sensors: Vec<(DVec3, Direction)>,
}

/// [`AttitudeLaw::target`] in ITRF, km; `Err` when a direction needs it and there is none.
fn site(law: &AttitudeLaw, needed: bool) -> Result<Option<DVec3>, String> {
    match &law.target {
        Some(t) if (-90.0..=90.0).contains(&t.lat_deg) && t.lon_deg.is_finite() => Ok(Some(
            GroundStation {
                latitude_rad: t.lat_deg.to_radians(),
                longitude_rad: t.lon_deg.to_radians(),
                altitude_km: t.alt_km,
                ..GroundStation::default()
            }
            .position_itrf_km(),
        )),
        Some(_) => Err("target: bad latitude/longitude".into()),
        None if needed => Err("direction \"target\" needs a target site".into()),
        None => Ok(None),
    }
}

/// The propagation's flown attitude; spin zero angle defaults to the propagation start.
fn steering(req: &PropagationRequest, law: &AttitudeLaw) -> Result<Steering, String> {
    let start = req
        .start
        .as_deref()
        .or(req.initial.epoch())
        .map(epoch)
        .transpose()?;
    let start = match (start, &law.spin) {
        (Some(e), _) => e,
        (None, Some(SpinOptions { epoch: Some(e), .. })) => epoch(e)?,
        (None, Some(_)) => return Err("attitude spin needs an epoch (or a start)".into()),
        (None, None) => Epoch::J2000,
    };
    let mut steering = Steering::new(profile(law, start).map_err(|e| format!("attitude: {e}"))?);
    steering.target_body_fixed_km =
        site(law, steering.uses_target()).map_err(|e| format!("attitude: {e}"))?;
    Ok(steering)
}

fn pointing(att: &AttitudeRequest, start: Epoch) -> Result<Pointing, String> {
    let profile = profile(&att.law, start)?;
    let uses_target = |d: Direction| d == Direction::Target;
    let wants_target = match profile.law {
        Law::AlignConstrain(a) => uses_target(a.primary_to) || uses_target(a.secondary_to),
        Law::Inertial(_) => false,
    };
    let sensors = att
        .sensors
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Ok((
                body_axis(s.body, &format!("sensors[{i}]"))?,
                direction(&s.to)?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let site = site(
        &att.law,
        wants_target || sensors.iter().any(|&(_, d)| uses_target(d)),
    )?;
    Ok(Pointing {
        profile,
        site,
        sensors,
    })
}

impl Pointing {
    fn geometry(
        &self,
        e: Epoch,
        s: &StateVector,
        sun: &dyn Ephemeris,
        earth: &mut Terrestrial,
    ) -> Geometry {
        Geometry {
            position_km: s.position_km,
            velocity_km_s: s.velocity_km_s,
            sun_km: sun.position_at(e),
            target_km: self.site.map(|itrf| earth.rotation(e).transpose() * itrf),
        }
    }
}

/// Samples `att` over the propagated span every `stepS` (and at its end). Sun from
/// [`sun_and_moon`]; the target site through full Earth orientation.
pub fn attitude(propagated: &Propagated, att: &AttitudeRequest) -> Result<AttitudeSamples, String> {
    let states = propagated.ordered_states();
    let (Some(&(start, _)), Some(&(stop, _))) = (states.first(), states.last()) else {
        return Ok(AttitudeSamples::default());
    };
    let span = stop.seconds_since(start);
    let step = att.step_s;
    if step.is_nan() || step <= 0.0 || span / step > MAX_SAMPLES as f64 {
        return Err(format!(
            "stepS must be positive and yield ≤ {MAX_SAMPLES} samples"
        ));
    }
    let pointing = pointing(att, start)?;
    let sensors = &pointing.sensors;

    let ephemeris = SampledEphemeris::new(states, Interpolation::default());
    let env = environment();
    let (sun, _) = sun_and_moon(&env);
    let mut earth = Terrestrial::new(&env, start);
    let mut teme_grid = terramenta_solare::earth::TemeGrid::new(|e| {
        env.eop.as_ref().map(|t| t.at(e)).unwrap_or_default()
    });

    let count = (span / step).ceil() as usize;
    let mut out = AttitudeSamples {
        sensor_angles_deg: vec![Vec::new(); sensors.len()],
        ..AttitudeSamples::default()
    };
    let mut aem = Vec::new();
    for i in 0..=count {
        let t = (i as f64 * step).min(span);
        let e = start.advanced_by_seconds(t);
        let s = ephemeris.state_at(e);
        let g = pointing.geometry(e, &s, sun.as_ref(), &mut earth);
        let Some(q) = pointing.profile.attitude(e, &g) else {
            out.undefined += 1;
            continue;
        };
        // Sign continuous, first sample w ≥ 0, so linear interpolation of the AEM holds.
        let previous = out.quaternions.last().map(|q| DQuat::from_array(*q));
        let q = match previous {
            Some(p) if p.dot(q) < 0.0 => -q,
            None if q.w < 0.0 => -q,
            _ => q,
        };
        let unix = e.to_unix_seconds();
        out.times.push(unix);
        out.quaternions.push(q.to_array());
        let euler = attitude::lvlh_euler_321(q, g.position_km, g.velocity_km_s);
        out.euler_deg.push([
            euler.x.to_degrees(),
            euler.y.to_degrees(),
            euler.z.to_degrees(),
        ]);
        for (angles, &(body, to)) in out.sensor_angles_deg.iter_mut().zip(sensors) {
            angles.push(to.resolve(&g).map_or(f64::NAN, |d| {
                attitude::off_axis_angle(q, body, d).to_degrees()
            }));
        }
        let gcrf_to_teme = DQuat::from_mat3(&teme_grid.at(e).transpose());
        out.teme.push((unix, (gcrf_to_teme * q).normalize()));
        if att.aem {
            aem.push((e, q));
        }
        if t >= span {
            break;
        }
    }
    if att.aem {
        out.aem = Some(
            Aem {
                header: Header {
                    version: "1.0".into(),
                    creation_date: creation_date(),
                    ..Header::default()
                },
                segments: vec![AemSegment {
                    metadata: AemMetadata {
                        object_name: propagated.name.clone(),
                        object_id: propagated.object_id.clone(),
                        ..AemMetadata::default()
                    },
                    quaternions: aem,
                }],
            }
            .to_kvn(),
        );
    }
    Ok(out)
}

// --- Coverage -------------------------------------------------------------------------

/// Cell cap per coverage grid.
pub const MAX_COVERAGE_CELLS: usize = 200_000;

/// `coverage` options.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageRequest {
    pub spacecraft: Vec<CoverageObserver>,
    /// H3 resolution, 0–15.
    #[serde(default = "default_resolution")]
    pub resolution: u8,
    /// `[west, south, east, north]`, degrees; default the globe. `west > east` crosses
    /// the antimeridian.
    pub bounds: Option<[f64; 4]>,
    #[serde(default)]
    pub min_elevation_deg: f64,
    #[serde(default = "default_coverage_step")]
    pub step_s: f64,
    /// UTC ISO 8601; default the spans all spacecraft share.
    pub start: Option<String>,
    pub stop: Option<String>,
    /// Cell outlines in the result.
    #[serde(default)]
    pub boundaries: bool,
}

fn default_resolution() -> u8 {
    3
}

fn default_coverage_step() -> f64 {
    60.0
}

/// A spacecraft and, for sensor access, its pointing and fields of view.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageObserver {
    pub propagation: PropagationRequest,
    /// Pointing law as `attitude` takes it; its `sensors`, `stepS`, `aem` unused.
    pub attitude: Option<AttitudeRequest>,
    /// Empty: line of sight above the minimum elevation.
    #[serde(default)]
    pub sensors: Vec<FovOptions>,
}

/// A field of view along body axis `body`: `halfAngleDeg` (cone) or `halfXDeg` and
/// `halfYDeg` (rectangle, sensor x toward `reference`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FovOptions {
    pub body: [f64; 3],
    pub reference: Option<[f64; 3]>,
    pub half_angle_deg: Option<f64>,
    pub half_x_deg: Option<f64>,
    pub half_y_deg: Option<f64>,
}

impl FovOptions {
    pub fn sensor(&self) -> Result<coverage::Sensor, String> {
        let fov = match (self.half_angle_deg, self.half_x_deg, self.half_y_deg) {
            (Some(a), None, None) => FieldOfView::Cone {
                half_angle_rad: a.to_radians(),
            },
            (None, Some(x), Some(y)) => FieldOfView::Rectangle {
                half_x_rad: x.to_radians(),
                half_y_rad: y.to_radians(),
            },
            _ => return Err("fov: give halfAngleDeg, or halfXDeg and halfYDeg".into()),
        };
        coverage::Sensor::new(
            DVec3::from_array(self.body),
            self.reference.map(DVec3::from_array),
            fov,
        )
        .ok_or_else(|| {
            "fov: body non-zero, reference not along it, cone half-angle in (0, 180), \
             rectangle half-angles in (0, 90)"
                .into()
        })
    }
}

/// Coverage of a grid over a common span. Times Unix seconds (UTC); gaps and first
/// access seconds, resolved to one step.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CoverageResult {
    pub start: f64,
    pub stop: f64,
    pub step_s: f64,
    pub samples: usize,
    pub resolution: u8,
    pub cells: Vec<CellInfo>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CellInfo {
    /// H3 index, hex.
    pub id: String,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub fraction: f64,
    pub accesses: u32,
    pub max_gap_s: f64,
    pub mean_gap_s: Option<f64>,
    pub first_access_s: Option<f64>,
    /// `[lonDeg, latDeg]` ring, open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<Vec<[f64; 2]>>,
}

/// Samples every spacecraft every `stepS` over the span they share and accumulates
/// access to the grid's cell centres.
pub fn coverage(req: &CoverageRequest) -> Result<CoverageResult, String> {
    if req.spacecraft.is_empty() {
        return Err("coverage needs a spacecraft".into());
    }
    let resolution = coverage::h3o::Resolution::try_from(req.resolution)
        .map_err(|_| format!("resolution {} not in 0–15", req.resolution))?;
    let region = match req.bounds {
        None => None,
        Some([west, south, east, north]) => {
            if !(-90.0..=90.0).contains(&south) || !(south..=90.0).contains(&north) {
                return Err("bounds: need -90 ≤ south ≤ north ≤ 90".into());
            }
            if !(west.is_finite() && east.is_finite()) {
                return Err("bounds: west and east must be finite".into());
            }
            Some(Region {
                west: west.to_radians(),
                south: south.to_radians(),
                east: east.to_radians(),
                north: north.to_radians(),
            })
        }
    };
    if !(-90.0..90.0).contains(&req.min_elevation_deg) {
        return Err("minElevationDeg must be in [-90, 90)".into());
    }

    struct Craft {
        ephemeris: SampledEphemeris,
        pointing: Option<Pointing>,
        sensors: Vec<coverage::Sensor>,
    }
    let mut crafts = Vec::new();
    let (mut start, mut stop) = (None::<Epoch>, None::<Epoch>);
    for (i, o) in req.spacecraft.iter().enumerate() {
        let what = |e: String| format!("spacecraft[{i}]: {e}");
        let states = trajectory(&o.propagation).map_err(what)?.ordered_states();
        let (Some(&(first, _)), Some(&(last, _))) = (states.first(), states.last()) else {
            return Err(what("no states".into()));
        };
        start = Some(start.map_or(first, |s| {
            if first.seconds_since(s) > 0.0 {
                first
            } else {
                s
            }
        }));
        stop = Some(stop.map_or(last, |s| if last.seconds_since(s) < 0.0 { last } else { s }));
        let sensors = o
            .sensors
            .iter()
            .map(FovOptions::sensor)
            .collect::<Result<Vec<_>, _>>()
            .map_err(what)?;
        let pointing = match (&o.attitude, sensors.is_empty()) {
            (_, true) => None,
            (Some(att), false) => Some(pointing(att, first).map_err(what)?),
            (None, false) => return Err(what("sensors need an attitude".into())),
        };
        crafts.push(Craft {
            ephemeris: SampledEphemeris::new(states, Interpolation::default()),
            pointing,
            sensors,
        });
    }
    let (mut start, mut stop) = (start.expect("spacecraft"), stop.expect("spacecraft"));
    if let Some(s) = &req.start {
        let s = epoch(s)?;
        if s.seconds_since(start) > 0.0 {
            start = s;
        }
    }
    if let Some(s) = &req.stop {
        let s = epoch(s)?;
        if s.seconds_since(stop) < 0.0 {
            stop = s;
        }
    }
    let span = stop.seconds_since(start);
    if span.is_nan() || span < 0.0 {
        return Err("spacecraft spans do not overlap the window".into());
    }
    let step = req.step_s;
    if step.is_nan() || step <= 0.0 || (span / step) * crafts.len() as f64 > MAX_SAMPLES as f64 {
        return Err(format!(
            "stepS must be positive and yield ≤ {MAX_SAMPLES} samples over all spacecraft"
        ));
    }

    let grid = Grid::new(resolution, region, MAX_COVERAGE_CELLS)?;
    let mut cover = Coverage::new(grid, req.min_elevation_deg.to_radians());
    let env = environment();
    let (sun, _) = sun_and_moon(&env);
    let mut earth = Terrestrial::new(&env, start);
    let count = (span / step).ceil() as usize;
    let mut observers = Vec::with_capacity(crafts.len());
    for i in 0..=count {
        let t = (i as f64 * step).min(span);
        let e = start.advanced_by_seconds(t);
        let to_itrf = earth.rotation(e);
        let to_itrf_q = DQuat::from_mat3(&to_itrf);
        observers.clear();
        for c in &crafts {
            let s = c.ephemeris.state_at(e);
            let body_to_itrf = match &c.pointing {
                None => DQuat::IDENTITY,
                Some(p) => {
                    let g = p.geometry(e, &s, sun.as_ref(), &mut earth);
                    match p.profile.attitude(e, &g) {
                        Some(q) => (to_itrf_q * q).normalize(),
                        None => continue,
                    }
                }
            };
            observers.push(Observer {
                position_itrf_km: to_itrf * s.position_km,
                body_to_itrf,
                sensors: &c.sensors,
            });
        }
        cover.sample(t, &observers);
        if t >= span {
            break;
        }
    }

    let samples = cover.samples();
    let cells = cover
        .cells()
        .into_iter()
        .zip(&cover.grid.cells)
        .map(|(c, &cell)| {
            let centre = coverage::h3o::LatLng::from(cell);
            // Degrees to 1e-5 (≈ 1 m), fractions to 1e-6: a smaller result for JavaScript.
            let deg = |x: f64| (x * 1e5).round() / 1e5;
            CellInfo {
                id: cell.to_string(),
                lat_deg: deg(centre.lat()),
                lon_deg: deg(centre.lng()),
                fraction: (c.fraction * 1e6).round() / 1e6,
                accesses: c.accesses,
                max_gap_s: c.max_gap_s,
                mean_gap_s: c.mean_gap_s,
                first_access_s: c.first_access_s,
                boundary: req.boundaries.then(|| {
                    cell.boundary()
                        .iter()
                        .map(|v| [deg(v.lng()), deg(v.lat())])
                        .collect()
                }),
            }
        })
        .collect();
    Ok(CoverageResult {
        start: start.to_unix_seconds(),
        stop: stop.to_unix_seconds(),
        step_s: step,
        samples,
        resolution: req.resolution,
        cells,
    })
}

// --- Conjunctions ---------------------------------------------------------------------

/// `conjunctions` options.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConjunctionRequest {
    /// As `propagate`; its `covariance`, if any, feeds Pc.
    pub primary: PropagationRequest,
    pub secondaries: Vec<SecondaryOptions>,
    /// UTC; default the primary's span.
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default = "default_screening_step")]
    pub step_s: f64,
    #[serde(default = "default_threshold")]
    pub threshold_km: f64,
    /// Combined hard-body radius.
    #[serde(default = "default_hard_body")]
    pub hard_body_radius_m: f64,
    /// RSW position 1σ for secondaries without a covariance (SGP4, or no `covariance`).
    #[serde(default)]
    pub secondary_sigma_km: Option<[f64; 3]>,
    /// Catalogue numbers not screened (e.g. the primary itself).
    #[serde(default)]
    pub exclude: Vec<u64>,
    /// CDM KVN per conjunction.
    #[serde(default)]
    pub cdm: bool,
}

fn default_screening_step() -> f64 {
    60.0
}

fn default_threshold() -> f64 {
    5.0
}

fn default_hard_body() -> f64 {
    20.0
}

/// One of `propagation`, `tle`, `omm`, `layer`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecondaryOptions {
    /// Reported as `secondary`; default the object name.
    #[serde(default)]
    pub id: Option<String>,
    /// Cowell, as `propagate`.
    #[serde(default)]
    pub propagation: Option<Box<PropagationRequest>>,
    /// SGP4.
    #[serde(default)]
    pub tle: Option<[String; 2]>,
    #[serde(default)]
    pub omm: Option<serde_json::Value>,
    /// Every object of a loaded ephemeris layer (SGP4 or OEM).
    #[serde(default)]
    pub layer: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConjunctionResult {
    pub start: f64,
    pub stop: f64,
    /// Objects searched.
    pub screened: usize,
    /// Objects skipped by the perigee/apogee filter.
    pub filtered: usize,
    pub conjunctions: Vec<ConjunctionInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConjunctionInfo {
    pub secondary: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub norad_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Unix s.
    pub tca: f64,
    pub miss_km: f64,
    pub relative_speed_km_s: f64,
    /// Secondary − primary, primary RSW.
    pub relative_position_rsw_km: [f64; 3],
    pub relative_velocity_rsw_km_s: [f64; 3],
    /// Combined 1σ in the encounter plane, principal axes.
    pub sigma_km: Option<[f64; 2]>,
    pub pc: Option<f64>,
    /// Maximum over covariance scale (isotropic without one).
    pub pc_max: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdm: Option<String>,
}

/// Perigee/apogee pad for SGP4 mean elements vs osculating radius, km.
const BAND_PAD_KM: f64 = 50.0;
/// TCA tolerance, s.
const TCA_TOLERANCE_S: f64 = 1e-4;

/// A numerically propagated secondary.
struct Sampled {
    id: String,
    name: String,
    object_id: String,
    ephemeris: SampledEphemeris,
    covariance: Vec<(Epoch, Matrix6)>,
}

enum Target<'a> {
    Sampled(&'a Sampled),
    Sgp4 {
        id: Option<&'a str>,
        layer: Option<&'a str>,
        satellite: &'a crate::omm::Satellite,
    },
}

/// Linear interpolation of time-ordered covariance samples; `None` outside or empty.
fn covariance_at(samples: &[(Epoch, Matrix6)], e: Epoch) -> Option<Matrix6> {
    let (first, last) = (samples.first()?, samples.last()?);
    if e < first.0 || e > last.0 {
        return None;
    }
    let i = samples.partition_point(|s| s.0 <= e);
    if i >= samples.len() {
        return Some(last.1);
    }
    let (a, b) = (&samples[i - 1], &samples[i]);
    let span = b.0.seconds_since(a.0);
    let f = if span > 0.0 {
        e.seconds_since(a.0) / span
    } else {
        0.0
    };
    Some(std::array::from_fn(|r| {
        std::array::from_fn(|c| a.1[r][c] + f * (b.1[r][c] - a.1[r][c]))
    }))
}

fn time_ordered_covariance(p: &Propagated) -> Vec<(Epoch, Matrix6)> {
    p.ordered()
        .into_iter()
        .filter_map(|(e, (_, c))| c.map(|c| (e, c)))
        .collect()
}

/// Screens `req.primary` against every secondary.
pub fn conjunctions(req: &ConjunctionRequest) -> Result<ConjunctionResult, String> {
    let layers = req
        .secondaries
        .iter()
        .filter_map(|s| s.layer.as_deref())
        .map(|id| {
            crate::ephemeris::catalogue_of(id)
                .map(|l| (id.to_string(), l.catalogue.clone()))
                .ok_or_else(|| format!("no loaded ephemeris layer {id:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    screen(req, &layers)
}

/// [`conjunctions`] with layer catalogues resolved.
fn screen(
    req: &ConjunctionRequest,
    layers: &[(String, Arc<crate::omm::Catalogue>)],
) -> Result<ConjunctionResult, String> {
    let step = req.step_s.abs();
    if req.threshold_km.is_nan() || req.threshold_km <= 0.0 {
        return Err("thresholdKm must be positive".into());
    }
    if req.hard_body_radius_m.is_nan() || req.hard_body_radius_m < 0.0 {
        return Err("hardBodyRadiusM must not be negative".into());
    }
    if req
        .secondary_sigma_km
        .is_some_and(|s| !s.iter().all(|x| *x >= 0.0))
    {
        return Err("secondarySigmaKm must not be negative".into());
    }
    let primary = propagate(&PropagationRequest {
        kvn: false,
        ..req.primary.clone()
    })?;
    let states = primary.ordered_states();
    let (first, last) = (states[0].0, states[states.len() - 1].0);
    let start = req
        .start
        .as_deref()
        .map(epoch)
        .transpose()?
        .unwrap_or(first);
    let stop = req.stop.as_deref().map(epoch).transpose()?.unwrap_or(last);
    if start < first || stop > last || stop <= start {
        return Err("start/stop must lie inside the primary's span, start first".into());
    }
    let span = stop.seconds_since(start);
    if step.is_nan() || step < 1e-3 || span / step > MAX_EVENT_SAMPLES {
        return Err(format!(
            "stepS must be positive and yield ≤ {MAX_EVENT_SAMPLES} samples"
        ));
    }
    let primary_covariance = time_ordered_covariance(&primary);
    let primary_ephemeris = SampledEphemeris::new(states, Interpolation::default());

    let mut owned = Vec::new();
    let mut sampled = Vec::new();
    for s in &req.secondaries {
        let given = [
            s.propagation.is_some(),
            s.tle.is_some(),
            s.omm.is_some(),
            s.layer.is_some(),
        ];
        if given.iter().filter(|g| **g).count() != 1 {
            return Err("each secondary needs one of propagation, tle, omm, layer".into());
        }
        if let Some(p) = &s.propagation {
            let out = propagate(&PropagationRequest {
                kvn: false,
                ..(**p).clone()
            })?;
            sampled.push(Sampled {
                id: s.id.clone().unwrap_or_else(|| out.name.clone()),
                name: out.name.clone(),
                object_id: out.object_id.clone(),
                covariance: time_ordered_covariance(&out),
                ephemeris: SampledEphemeris::new(out.ordered_states(), Interpolation::default()),
            });
        } else if s.tle.is_some() || s.omm.is_some() {
            let elements = match (&s.tle, &s.omm) {
                (Some(t), _) => sgp4::Elements::from_tle(None, t[0].as_bytes(), t[1].as_bytes())
                    .map_err(|e| format!("bad TLE: {e}"))?,
                (_, Some(o)) => {
                    serde_json::from_value(o.clone()).map_err(|e| format!("bad OMM: {e}"))?
                }
                _ => unreachable!(),
            };
            let satellite = crate::omm::satellite(&elements)
                .ok_or_else(|| "SGP4 rejected the elements".to_string())?;
            owned.push((s.id.as_deref(), satellite));
        }
    }
    let mut targets: Vec<Target> = sampled.iter().map(Target::Sampled).collect();
    targets.extend(owned.iter().map(|(id, satellite)| Target::Sgp4 {
        id: *id,
        layer: None,
        satellite,
    }));
    for (layer, catalogue) in layers {
        targets.extend(catalogue.satellites.iter().map(|satellite| Target::Sgp4 {
            id: None,
            layer: Some(layer.as_str()),
            satellite,
        }));
    }

    // Primary and TEME → GCRF on the search grid, shared by every secondary.
    let env = environment();
    let mut teme = terramenta_solare::earth::TemeGrid::new(|e| {
        env.eop.as_ref().map(|t| t.at(e)).unwrap_or_default()
    });
    let n = (span / step).ceil().max(1.0) as usize;
    let grid: Vec<(f64, StateVector, DMat3)> = (0..=n)
        .map(|k| {
            let t = (k as f64 * step).min(span);
            let e = start.advanced_by_seconds(t);
            (t, primary_ephemeris.state_at(e), teme.at(e))
        })
        .collect();
    let (r_min, r_max) = grid.iter().fold((f64::INFINITY, 0.0f64), |(lo, hi), g| {
        let r = g.1.position_km.length();
        (lo.min(r), hi.max(r))
    });
    let hard_body_km = req.hard_body_radius_m * 1e-3;

    let mut out = ConjunctionResult {
        start: start.to_unix_seconds(),
        stop: stop.to_unix_seconds(),
        ..Default::default()
    };
    for target in &targets {
        if let Target::Sgp4 { satellite, .. } = target {
            if req.exclude.contains(&satellite.norad_id) {
                continue;
            }
            if !satellite.is_tabulated() {
                let a = (EGM2008_GM_KM3_S2
                    * (satellite.period_minutes * 60.0 / std::f64::consts::TAU).powi(2))
                .cbrt();
                let (perigee, apogee) = (
                    a * (1.0 - satellite.eccentricity),
                    a * (1.0 + satellite.eccentricity),
                );
                let pad = req.threshold_km + BAND_PAD_KM;
                if perigee > r_max + pad || apogee < r_min - pad {
                    out.filtered += 1;
                    continue;
                }
            }
        }
        out.screened += 1;
        // GCRF state at `e`, with TEME → GCRF `m`.
        let state = |e: Epoch, m: DMat3| match target {
            Target::Sampled(s) => s.ephemeris.try_state_at(e),
            Target::Sgp4 { satellite, .. } => satellite
                .state_teme(e.to_unix_seconds())
                .map(|(r, v)| StateVector::new(m * DVec3::from(r), m * DVec3::from(v))),
        };
        let mut relative = |t: f64| {
            let k = (t / step).round() as usize;
            let (p, m) = match grid.get(k) {
                Some(g) if g.0 == t => (g.1, g.2),
                _ => {
                    let e = start.advanced_by_seconds(t);
                    (primary_ephemeris.state_at(e), teme.at(e))
                }
            };
            let s = state(start.advanced_by_seconds(t), m)?;
            Some((
                s.position_km - p.position_km,
                s.velocity_km_s - p.velocity_km_s,
            ))
        };
        let found = terramenta_solare::conjunction::closest_approaches(
            &mut relative,
            span,
            step,
            req.threshold_km,
            TCA_TOLERANCE_S,
        );
        for approach in found {
            let e = start.advanced_by_seconds(approach.t_s);
            let p = primary_ephemeris.state_at(e);
            let Some(s) = state(e, teme.at(e)) else {
                continue;
            };
            let (pr, pv) = (p.position_km, p.velocity_km_s);
            let (sr, sv) = (s.position_km, s.velocity_km_s);
            let p_cov = covariance_at(&primary_covariance, e);
            let (s_cov, s_default) = match target {
                Target::Sampled(s) if !s.covariance.is_empty() => {
                    (covariance_at(&s.covariance, e), false)
                }
                _ => (
                    req.secondary_sigma_km.map(|[a, b, c]| {
                        covariance::from_local(
                            &covariance::diagonal([a * a, b * b, c * c, 0.0, 0.0, 0.0]),
                            LocalFrame::Rsw,
                            sr,
                            sv,
                        )
                    }),
                    true,
                ),
            };
            let Some(a) = terramenta_solare::conjunction::assess(
                (pr, pv),
                (sr, sv),
                p_cov.as_ref(),
                s_cov.as_ref(),
                hard_body_km,
            ) else {
                continue;
            };
            let (id, name, norad_id, layer, object_id) = match target {
                Target::Sampled(s) => (
                    s.id.clone(),
                    s.name.clone(),
                    None,
                    None,
                    s.object_id.clone(),
                ),
                Target::Sgp4 {
                    id,
                    layer,
                    satellite,
                } => (
                    id.map_or_else(|| satellite.name.clone(), str::to_string),
                    satellite.name.clone(),
                    Some(satellite.norad_id),
                    layer.map(str::to_string),
                    satellite.norad_id.to_string(),
                ),
            };
            let cdm = req.cdm.then(|| {
                let rtn = |c: Option<Matrix6>, r, v| {
                    c.map(|c| covariance::to_local(&c, LocalFrame::Rsw, r, v))
                        .unwrap_or([[0.0; 6]; 6])
                };
                let mut cdm = Cdm::new(e, a.miss_km);
                cdm.header.creation_date = creation_date();
                cdm.message_id = format!(
                    "{}_{}_{}",
                    primary.object_id,
                    object_id,
                    e.to_unix_seconds().round()
                );
                cdm.relative_speed_km_s = Some(a.relative_speed_km_s);
                cdm.relative_position_rtn_km = Some(a.position_rsw_km);
                cdm.relative_velocity_rtn_km_s = Some(a.velocity_rsw_km_s);
                cdm.collision_probability = a.probability;
                cdm.collision_probability_method = a.probability.map(|_| "FOSTER-1992".to_string());
                cdm.objects = [
                    CdmObject {
                        designator: primary.object_id.clone(),
                        object_name: primary.name.clone(),
                        ephemeris_name: primary.name.clone(),
                        covariance_method: if p_cov.is_some() {
                            "CALCULATED"
                        } else {
                            "DEFAULT"
                        }
                        .into(),
                        state: p,
                        covariance_rtn: rtn(p_cov, pr, pv),
                        ..Default::default()
                    },
                    CdmObject {
                        designator: object_id,
                        object_name: name.clone(),
                        international_designator: match target {
                            Target::Sgp4 { satellite, .. } => satellite
                                .international_designator
                                .clone()
                                .unwrap_or_else(|| "UNKNOWN".into()),
                            Target::Sampled(_) => "UNKNOWN".into(),
                        },
                        covariance_method: if s_cov.is_some() && !s_default {
                            "CALCULATED"
                        } else {
                            "DEFAULT"
                        }
                        .into(),
                        state: s,
                        covariance_rtn: rtn(s_cov, sr, sv),
                        ..Default::default()
                    },
                ];
                cdm.to_kvn()
            });
            out.conjunctions.push(ConjunctionInfo {
                secondary: id,
                name,
                norad_id,
                layer,
                tca: e.to_unix_seconds(),
                miss_km: a.miss_km,
                relative_speed_km_s: a.relative_speed_km_s,
                relative_position_rsw_km: a.position_rsw_km.to_array(),
                relative_velocity_rsw_km_s: a.velocity_rsw_km_s.to_array(),
                sigma_km: a.sigma_km,
                pc: a.probability,
                pc_max: a.max_probability,
                cdm,
            });
        }
    }
    out.conjunctions.sort_by(|a, b| a.tca.total_cmp(&b.tca));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(json: &str) -> PropagationRequest {
        serde_json::from_str(json).unwrap()
    }

    /// LEO → GEO: targeted TOI, coast to apoapsis, targeted circularization + plane change.
    /// The resolved propagation reproduces the sequence's end state.
    #[test]
    fn sequence_geo_transfer_resolves_to_a_reproducible_propagation() {
        let propagation: serde_json::Value = serde_json::from_str(
            r#"{ "initial": { "epoch": "2026-03-20T00:00:00Z",
                   "elements": { "smaKm": 6678.137, "eccentricity": 0, "inclinationDeg": 28.5,
                                 "raanDeg": 0, "argPerigeeDeg": 0, "trueAnomalyDeg": 0 } },
                 "durationS": 60, "stepS": 120, "vehicle": { "massKg": 3000 },
                 "forces": { "gravity": [0, 0], "thirdBody": false, "srp": false, "drag": "none" } }"#,
        )
        .unwrap();
        let commands: Vec<CommandOptions> = serde_json::from_str(
            r#"[
              { "label": "toi", "target": {
                  "variables": [{ "command": "toi-burn", "parameter": "dv1" }],
                  "goals": [{ "quantity": "radiusKm", "value": 42164, "tolerance": 0.01 }],
                  "sequence": [
                    { "label": "toi-burn", "maneuver": { "deltaVKmS": [2, 0, 0], "frame": "vnb", "ispS": 310 } },
                    { "propagate": { "to": "apoapsis" } } ] } },
              { "label": "goi", "target": {
                  "variables": [{ "command": "goi-burn", "parameter": "dv1" },
                                { "command": "goi-burn", "parameter": "dv2" }],
                  "goals": [{ "quantity": "eccentricity", "value": 0, "tolerance": 1e-5 },
                            { "quantity": "inclinationDeg", "value": 0, "tolerance": 1e-3 }],
                  "sequence": [
                    { "label": "goi-burn", "maneuver": { "deltaVKmS": [1.5, 0, 0], "frame": "vnb", "ispS": 310 } } ] } },
              { "propagate": { "durationS": 3600 } }
            ]"#,
        )
        .unwrap();
        let out = run_sequence(&propagation, &commands).unwrap();
        assert!(out.converged, "{:?}", out.segments);
        // TOI ≈ 2.43 km/s; GOI with 28.5° plane change ≈ 1.80 km/s.
        assert!(
            (out.delta_v_km_s - 4.23).abs() < 0.03,
            "{}",
            out.delta_v_km_s
        );
        let last = out.segments.last().unwrap();
        assert!((last.elements.sma_km - 42_164.0).abs() < 1.0);
        assert!(last.elements.inclination_deg < 2e-3);
        assert!(out.propagation.get("durationS").is_none());

        let req: PropagationRequest = serde_json::from_value(out.propagation.clone()).unwrap();
        assert_eq!(req.maneuvers.len(), 2);
        let p = propagate(&req).unwrap();
        let end = p.states.last().unwrap();
        assert!((end.mass_kg - out.final_mass_kg).abs() < 1e-6);
        assert!(
            (Quantity::SmaKm.of(end) - last.elements.sma_km).abs() < 0.05,
            "{} vs {}",
            Quantity::SmaKm.of(end),
            last.elements.sma_km
        );
    }

    /// Plates and body-frame burns fly `attitude`; without them it is ignored.
    #[test]
    fn attitude_couples_plates_and_body_burns() {
        let base = serde_json::json!({
            "initial": { "epoch": "2026-03-20T00:00:00Z",
                         "elements": { "smaKm": 6628.137, "eccentricity": 0, "inclinationDeg": 51.6,
                                       "raanDeg": 0, "argPerigeeDeg": 0, "trueAnomalyDeg": 0 } },
            "durationS": 21600, "stepS": 600,
            "vehicle": { "massKg": 500, "dryMassKg": 300, "dragAreaM2": 1 },
            "forces": { "gravity": [0, 0], "thirdBody": false, "srp": true, "drag": "exponential" } });
        let flown = |patch: serde_json::Value| {
            let mut json = base.clone();
            json_merge(&mut json, patch);
            let req: PropagationRequest = serde_json::from_value(json).unwrap();
            propagate(&req).map(|p| Quantity::SmaKm.of(p.states.last().unwrap()))
        };
        let ram = serde_json::json!({
            "primary": { "body": [1, 0, 0], "to": "velocity" },
            "secondary": { "body": [0, 0, 1], "to": "orbitNormal" } });
        let plates = |front: f64| {
            serde_json::json!([{ "normal": [1, 0, 0], "areaM2": front },
                               { "normal": [0, 1, 0], "areaM2": 10 }])
        };
        let cannonball = flown(serde_json::json!({})).unwrap();
        let attitude_only = flown(serde_json::json!({ "attitude": ram })).unwrap();
        assert_eq!(cannonball, attitude_only, "ignored without plates");
        let front =
            flown(serde_json::json!({ "vehicle": { "plates": plates(1.0) }, "attitude": ram }))
                .unwrap();
        let side = flown(
            serde_json::json!({ "vehicle": { "plates": plates(1.0) }, "attitude": {
            "primary": { "body": [0, 1, 0], "to": "velocity" },
            "secondary": { "body": [0, 0, 1], "to": "orbitNormal" } } }),
        )
        .unwrap();
        let start = 6628.137;
        assert!(
            ((start - front) / (start - cannonball) - 1.0).abs() < 0.1,
            "{front} {cannonball}"
        );
        assert!((start - side) > 5.0 * (start - front), "{side} {front}");
        assert!(
            flown(serde_json::json!({ "vehicle": { "plates": plates(1.0) } }))
                .unwrap_err()
                .contains("require an attitude")
        );
        assert!(
            flown(
                serde_json::json!({ "vehicle": { "plates": [{ "normal": [0, 0, 0], "areaM2": 1 }] },
                                      "attitude": ram })
            )
            .unwrap_err()
            .contains("normal")
        );

        // A body-x burn pointed along velocity is a VNB burn.
        let burn = |frame: &str| {
            serde_json::json!({ "forces": { "drag": "none", "srp": false },
                "finiteBurns": [{ "start": "2026-03-20T00:10:00Z", "durationS": 300, "thrustN": 200,
                                  "ispS": 300, "direction": [1, 0, 0], "frame": frame }],
                "attitude": ram })
        };
        let vnb = flown(burn("vnb")).unwrap();
        let body = flown(burn("body")).unwrap();
        assert!(
            vnb - start > 50.0 && (body - vnb).abs() < 1e-6,
            "{body} {vnb}"
        );
        let mut no_attitude = burn("body");
        no_attitude.as_object_mut().unwrap().remove("attitude");
        assert!(
            flown(no_attitude)
                .unwrap_err()
                .contains("require an attitude")
        );
        let impulsive = serde_json::json!({ "maneuvers": [{ "epoch": "2026-03-20T00:10:00Z",
            "deltaVKmS": [0.01, 0, 0], "frame": "body" }] });
        assert!(flown(impulsive).unwrap_err().contains("finite burns only"));

        // Sequences resolve body-frame burns with their frame.
        let mut propagation = base.clone();
        json_merge(&mut propagation, burn("body"));
        let object = propagation.as_object_mut().unwrap();
        object.remove("finiteBurns");
        object.remove("durationS");
        let commands: Vec<CommandOptions> = serde_json::from_str(
            r#"[{ "finiteBurn": { "durationS": 300, "thrustN": 200, "ispS": 300,
                                  "direction": [1, 0, 0], "frame": "body" } },
                { "propagate": { "durationS": 600 } }]"#,
        )
        .unwrap();
        let out = run_sequence(&propagation, &commands).unwrap();
        assert_eq!(out.propagation["finiteBurns"][0]["frame"], "body");
        assert!(out.propagation.get("attitude").is_some());
    }

    /// Recursive object merge, `patch` over `json`.
    fn json_merge(json: &mut serde_json::Value, patch: serde_json::Value) {
        match (json, patch) {
            (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
                for (k, v) in b {
                    json_merge(a.entry(k).or_insert(serde_json::Value::Null), v);
                }
            }
            (a, b) => *a = b,
        }
    }

    #[test]
    fn sequence_rejects_ambiguous_commands() {
        let propagation = serde_json::json!({
            "initial": { "epoch": "2026-03-20T00:00:00Z",
                         "positionKm": [7000, 0, 0], "velocityKmS": [0, 7.5, 0] } });
        let both: Vec<CommandOptions> =
            serde_json::from_str(r#"[{ "propagate": { "durationS": 60, "to": "apoapsis" } }]"#)
                .unwrap();
        assert!(
            run_sequence(&propagation, &both)
                .unwrap_err()
                .contains("one of")
        );
        let none: Vec<CommandOptions> = serde_json::from_str(r#"[{ "label": "x" }]"#).unwrap();
        assert!(
            run_sequence(&propagation, &none)
                .unwrap_err()
                .contains("one of")
        );
        assert!(serde_json::from_str::<Vec<CommandOptions>>(r#"[{ "coast": {} }]"#).is_err());
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
    fn covariance_grows_in_track_and_round_trips_oem() {
        let json = |covariance: &str| {
            request(&format!(
                r#"{{
                    "initial": {{ "epoch": "2024-03-01T00:00:00Z",
                                 "positionKm": [6778.137, 0, 0], "velocityKmS": [0, 4.7, 6.0] }},
                    "durationS": 21600, "stepS": 600,
                    "forces": {{ "gravity": [4, 4], "drag": "exponential" }},
                    "maneuvers": [{{ "epoch": "2024-03-01T01:00:00", "deltaVKmS": [0.01, 0, 0] }}],
                    "covariance": {covariance}
                }}"#
            ))
        };
        let plain = propagate(&request(
            r#"{ "initial": { "epoch": "2024-03-01T00:00:00Z",
                              "positionKm": [6778.137, 0, 0], "velocityKmS": [0, 4.7, 6.0] },
                 "durationS": 21600, "stepS": 600,
                 "forces": { "gravity": [4, 4], "drag": "exponential" },
                 "maneuvers": [{ "epoch": "2024-03-01T01:00:00", "deltaVKmS": [0.01, 0, 0] }] }"#,
        ))
        .unwrap();
        let out = propagate(&json(
            r#"{ "positionSigmaKm": [0.1, 0.3, 0.05], "velocitySigmaKmS": [1e-4, 5e-5, 5e-5] }"#,
        ))
        .unwrap();
        assert_eq!(
            out.states, plain.states,
            "covariance leaves the trajectory alone"
        );
        assert_eq!(out.covariances.len(), out.states.len());
        let info = out.info(Some(&out.to_oem()));
        let cov = info.covariance.as_ref().unwrap();
        assert_eq!(cov.times.len(), out.states.len());
        let first = cov.sigma_rsw[0];
        for (a, b) in first.iter().zip([0.1, 0.3, 0.05, 1e-4, 5e-5, 5e-5]) {
            assert!((a - b).abs() < 1e-12, "{first:?}");
        }
        let last = cov.sigma_rsw.last().unwrap();
        assert!(last[1] > 20.0 * last[0] && last[1] > 5.0, "{last:?}");
        assert!(cov.ellipsoid_km.last().unwrap()[0] >= last[1]);

        let back = Oem::from_kvn(info.oem.as_deref().unwrap()).unwrap();
        let blocks: Vec<_> = back.segments.iter().map(|s| s.covariances.len()).collect();
        assert_eq!(blocks, [7, 31], "one per state, split at the burn");
        let p = &back.segments[1].covariances.last().unwrap().matrix;
        let q = out.covariances.last().unwrap();
        assert!((p[1][1] / q[1][1] - 1.0).abs() < 1e-6);

        // Process noise only adds; a full matrix equals its sigmas.
        let noisy = propagate(&json(
            r#"{ "positionSigmaKm": [0.1, 0.3, 0.05], "velocitySigmaKmS": [1e-4, 5e-5, 5e-5],
                 "processNoiseKm2S3": [1e-12, 1e-12, 1e-12] }"#,
        ))
        .unwrap();
        let grown = |p: &Propagated| p.covariances.last().unwrap()[2][2];
        assert!(grown(&noisy) > grown(&out));
        let mut triangle = [0.0; 21];
        for (k, x) in [0.1, 0.3, 0.05, 1e-4, 5e-5, 5e-5].iter().enumerate() {
            triangle[k * (k + 3) / 2] = x * x;
        }
        let full = propagate(&json(&format!(r#"{{ "lowerTriangle": {triangle:?} }}"#))).unwrap();
        assert_eq!(full.covariances, out.covariances);

        // TEME is a rotation of GCRF: trace kept.
        let teme = out.position_covariance_teme();
        assert_eq!(teme.len(), out.states.len());
        let (p, q) = (&out.covariances[20], &teme[20].1);
        let trace = p[0][0] + p[1][1] + p[2][2];
        assert!(((q[0][0] + q[1][1] + q[2][2]) / trace - 1.0).abs() < 1e-12);
        assert!((q[0][1] - q[1][0]).abs() < 1e-12 * trace);
    }

    #[test]
    fn rejects_bad_covariance() {
        for (covariance, message) in [
            (r#"{}"#, "needs positionSigmaKm"),
            (r#"{ "positionSigmaKm": [-1, 0, 0] }"#, "non-negative"),
            (
                r#"{ "positionSigmaKm": [1, 1, 1], "lowerTriangle": [] }"#,
                "not both",
            ),
            (r#"{ "lowerTriangle": [1, 2] }"#, "21 values"),
            (
                r#"{ "lowerTriangle": [1, 5, 1, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1] }"#,
                "not a covariance",
            ),
            (
                r#"{ "positionSigmaKm": [1, 1, 1], "frame": "body" }"#,
                "covariance.frame",
            ),
        ] {
            let req = request(&format!(
                r#"{{ "initial": {{ "epoch": "2024-03-01T00:00:00Z",
                                   "positionKm": [6778.137, 0, 0], "velocityKmS": [0, 7.7, 0] }},
                     "durationS": 600, "covariance": {covariance} }}"#
            ));
            let err = propagate(&req).err().unwrap();
            assert!(err.contains(message), "{covariance}: {err}");
        }
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

    /// Circular 51.6° LEO: latitude within ±51.6°, height ≈ 400 km over the ellipsoid,
    /// longitude in [−180, 180), last sample at the span end.
    #[test]
    fn ground_track_of_leo() {
        let v = (398_600.441_8_f64 / 6778.137).sqrt();
        let (s, c) = 51.6_f64.to_radians().sin_cos();
        let req = request(&format!(
            r#"{{ "initial": {{ "epoch": "2024-03-01T00:00:00Z",
                   "positionKm": [6778.137, 0, 0], "velocityKmS": [0, {}, {}] }},
                 "durationS": 5430, "stepS": 60, "forces": {{ "gravity": [2, 0] }} }}"#,
            v * c,
            v * s
        ));
        let out = propagate(&req).unwrap();
        let track = ground_track(&out, 600.0).unwrap();
        assert_eq!(track.times.len(), 11);
        assert!((track.times[10] - track.times[0] - 5430.0).abs() < 1e-6);
        let max_lat = track.coords.iter().map(|c| c[1].abs()).fold(0.0, f64::max);
        assert!(max_lat < 51.7 && max_lat > 45.0, "{max_lat}");
        for [lon, _, h] in &track.coords {
            assert!((-180.0..180.0).contains(lon));
            assert!((370.0..435.0).contains(h), "{h}");
        }
        assert!(ground_track(&out, 0.0).is_err());
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

    /// Nadir/velocity-pointed circular orbit: zero LVLH angles, the +Z sensor always at
    /// nadir, a fixed-inertial hold sweeping pitch at orbital rate.
    #[test]
    fn attitude_laws_over_an_orbit() {
        let v = (398_600.441_8_f64 / 6878.137).sqrt();
        let req = request(&format!(
            r#"{{ "initial": {{ "epoch": "2026-03-20T00:00:00Z",
                   "positionKm": [6878.137, 0, 0], "velocityKmS": [0, {v}, 0] }},
                 "durationS": 5700, "stepS": 60,
                 "forces": {{ "gravity": [0, 0], "thirdBody": false, "srp": false, "drag": "none" }} }}"#
        ));
        let out = propagate(&req).unwrap();
        let att: AttitudeRequest = serde_json::from_str(
            r#"{ "primary": { "body": [0, 0, 1], "to": "nadir" },
                 "secondary": { "body": [1, 0, 0], "to": "velocity" },
                 "sensors": [{ "body": [0, 0, 1], "to": "nadir" }, { "body": [0, 0, 1], "to": "sun" }],
                 "stepS": 100, "aem": true }"#,
        )
        .unwrap();
        let s = attitude(&out, &att).unwrap();
        assert_eq!(s.times.len(), 58, "57 steps and the end");
        assert_eq!(s.undefined, 0);
        assert!((s.times[57] - s.times[0] - 5700.0).abs() < 1e-6);
        assert!(s.euler_deg.iter().flatten().all(|a| a.abs() < 1e-6));
        assert!(s.sensor_angles_deg[0].iter().all(|a| a.abs() < 1e-6));
        // Equinox, equatorial orbit: the sun angle sweeps 0–180°.
        let sun = &s.sensor_angles_deg[1];
        let (lo, hi) = sun
            .iter()
            .fold((180.0f64, 0.0f64), |(l, h), &a| (l.min(a), h.max(a)));
        assert!(lo < 5.0 && hi > 175.0, "{lo} {hi}");
        let aem = s.aem.as_deref().unwrap();
        let back = Aem::from_kvn(aem).unwrap();
        assert_eq!(back.segments[0].quaternions.len(), 58);
        assert!(back.segments[0].metadata.ref_frame_a == "ICRF");
        assert_eq!(back.header.creation_date.len(), 23);
        let q: Vec<DQuat> = s
            .quaternions
            .iter()
            .map(|q| DQuat::from_array(*q))
            .collect();
        assert!(q[0].w >= 0.0 && q.windows(2).all(|p| p[0].dot(p[1]) > 0.0));

        let hold: AttitudeRequest =
            serde_json::from_str(r#"{ "quaternion": [0, 0, 0, 1], "stepS": 60 }"#).unwrap();
        let s = attitude(&out, &hold).unwrap();
        let pitch: Vec<f64> = s.euler_deg.iter().map(|e| e[1]).collect();
        assert!(pitch.iter().any(|p| p.abs() > 80.0));

        let spin: AttitudeRequest = serde_json::from_str(
            r#"{ "quaternion": [0, 0, 0, 1], "spin": { "axis": [0, 0, 1], "rateDegS": 1.5 }, "stepS": 60 }"#,
        )
        .unwrap();
        let s = attitude(&out, &spin).unwrap();
        let q1 = DQuat::from_array(s.quaternions[1]);
        assert!((q1.angle_between(DQuat::IDENTITY).to_degrees() - 90.0).abs() < 1e-6);

        for bad in [
            r#"{ "primary": { "body": [0, 0, 1], "to": "nadir" } }"#,
            r#"{ "primary": { "body": [0, 0, 1], "to": "moon" }, "secondary": { "body": [1, 0, 0], "to": "sun" } }"#,
            r#"{ "primary": { "body": [0, 0, 0], "to": "nadir" }, "secondary": { "body": [1, 0, 0], "to": "sun" } }"#,
            r#"{ "primary": { "body": [0, 0, 1], "to": "target" }, "secondary": { "body": [1, 0, 0], "to": "sun" } }"#,
            r#"{ "quaternion": [0, 0, 0, 1], "stepS": 0 }"#,
        ] {
            let att: AttitudeRequest = serde_json::from_str(bad).unwrap();
            assert!(attitude(&out, &att).is_err(), "{bad}");
        }
    }

    /// Target tracking: +Z stays on a site starting under the spacecraft.
    #[test]
    fn attitude_tracks_a_ground_site() {
        let v = (398_600.441_8_f64 / 6878.137).sqrt();
        let req = request(&format!(
            r#"{{ "initial": {{ "epoch": "2026-03-20T00:00:00Z",
                   "positionKm": [6878.137, 0, 0], "velocityKmS": [0, {v}, 0] }},
                 "durationS": 600, "stepS": 60, "forces": {{ "gravity": [0, 0] }} }}"#
        ));
        let out = propagate(&req).unwrap();
        let start = epoch("2026-03-20T00:00:00Z").unwrap().to_unix_seconds();
        let lon = -crate::frame::sidereal_radians(start).to_degrees();
        let att: AttitudeRequest = serde_json::from_str(&format!(
            r#"{{ "primary": {{ "body": [0, 0, 1], "to": "target" }},
                 "secondary": {{ "body": [1, 0, 0], "to": "velocity" }},
                 "target": {{ "latDeg": 0, "lonDeg": {lon} }},
                 "sensors": [{{ "body": [0, 0, 1], "to": "target" }}, {{ "body": [0, 0, 1], "to": "nadir" }}] }}"#
        ))
        .unwrap();
        let s = attitude(&out, &att).unwrap();
        assert!(s.sensor_angles_deg[0].iter().all(|a| a.abs() < 1e-6));
        // Off-nadir grows as the site falls behind.
        let off = &s.sensor_angles_deg[1];
        // ~0.36° precession since J2000 (GMST is of date) is ~5° off-nadir at 500 km.
        assert!(off[0] < 8.0 && off.last().unwrap() > &30.0, "{off:?}");
        assert_eq!(s.teme.len(), s.times.len());
    }

    #[test]
    fn elements_initial_state_matches_its_orbit() {
        let req = request(
            r#"{ "initial": { "epoch": "2026-01-01T00:00:00Z",
                   "elements": { "smaKm": 7000, "eccentricity": 0.01, "inclinationDeg": 98,
                                 "raanDeg": 30, "argPerigeeDeg": 90, "trueAnomalyDeg": 0 } },
                 "durationS": 60, "stepS": 60,
                 "forces": { "gravity": [0, 0], "thirdBody": false, "srp": false, "drag": "none" } }"#,
        );
        let out = propagate(&req).unwrap();
        let s = &out.states[0];
        // Perigee at the ascending node + 90°: the northernmost point of the orbit.
        assert!((s.position_km.length() - 6930.0).abs() < 1e-6);
        assert!((s.position_km.z / s.position_km.length() - 98f64.to_radians().sin()).abs() < 1e-9);
        assert!((Quantity::SmaKm.of(s) - 7000.0).abs() < 1e-6);
        assert!((Quantity::InclinationDeg.of(s) - 98.0).abs() < 1e-9);
        let low = r#"{ "initial": { "epoch": "2026-01-01T00:00:00Z",
                   "elements": { "smaKm": 6500, "eccentricity": 0.1, "inclinationDeg": 0,
                                 "raanDeg": 0, "argPerigeeDeg": 0, "trueAnomalyDeg": 0 } },
                 "durationS": 60 }"#;
        assert!(propagate(&request(low)).err().unwrap().contains("perigee"));
    }

    #[test]
    fn a_horizon_mask_shortens_contacts() {
        let req = request(
            r#"{ "initial": { "epoch": "2024-03-01T00:00:00Z",
                   "elements": { "smaKm": 6878.137, "eccentricity": 0, "inclinationDeg": 51.6,
                                 "raanDeg": 0, "argPerigeeDeg": 0, "trueAnomalyDeg": 0 } },
                 "durationS": 86400, "stepS": 60, "forces": { "gravity": [2, 0] } }"#,
        );
        let out = propagate(&req).unwrap();
        let search = |horizon: &str| {
            let events: EventRequest = serde_json::from_str(&format!(
                r#"{{ "stations": [{{ "id": "w", "latDeg": 37.94, "lonDeg": -75.46,
                                      "minElevationDeg": 5, "horizon": {horizon} }}] }}"#
            ))
            .unwrap();
            find_events(&out, &events)
        };
        let open = search("[]").unwrap().contacts;
        let masked = search("[[0, 5], [90, 5], [180, 25], [270, 5]]")
            .unwrap()
            .contacts;
        let total = |c: &[ContactInfo]| c.iter().map(|c| c.los - c.aos).sum::<f64>();
        assert!(!masked.is_empty() && masked.len() <= open.len());
        assert!(
            total(&masked) < 0.95 * total(&open),
            "{} {}",
            total(&masked),
            total(&open)
        );
        assert!(search("[[90, 5], [0, 5]]").unwrap_err().contains("horizon"));
    }

    #[test]
    fn coverage_of_two_satellites() {
        let craft = |raan: f64, sensors: &str| {
            format!(
                r#"{{ "propagation": {{ "initial": {{ "epoch": "2026-01-01T00:00:00Z",
                     "elements": {{ "smaKm": 7078.137, "eccentricity": 0, "inclinationDeg": 97.5,
                                    "raanDeg": {raan}, "argPerigeeDeg": 0, "trueAnomalyDeg": 0 }} }},
                     "durationS": 21600, "stepS": 60,
                     "forces": {{ "gravity": [2, 0], "thirdBody": false, "srp": false, "drag": "none" }} }},
                   "attitude": {{ "primary": {{ "body": [0, 0, 1], "to": "nadir" }},
                                  "secondary": {{ "body": [1, 0, 0], "to": "velocity" }} }},
                   "sensors": {sensors} }}"#
            )
        };
        let run = |json: String| coverage(&serde_json::from_str::<CoverageRequest>(&json).unwrap());
        let horizon = run(format!(
            r#"{{ "spacecraft": [{}, {}], "resolution": 1, "minElevationDeg": 10, "stepS": 120,
                 "boundaries": true }}"#,
            craft(0.0, "[]"),
            craft(90.0, "[]")
        ))
        .unwrap();
        assert_eq!(horizon.cells.len(), 842);
        assert_eq!(horizon.samples, 181);
        assert!((horizon.stop - horizon.start - 21600.0).abs() < 1e-6);
        let seen = horizon.cells.iter().filter(|c| c.fraction > 0.0).count();
        assert!(seen > 700, "{seen}");
        let pole = horizon
            .cells
            .iter()
            .max_by(|a, b| a.lat_deg.total_cmp(&b.lat_deg))
            .unwrap();
        // Near-polar, both over the pole together: an access each orbit (99 min).
        assert!(
            pole.accesses >= 3 && pole.mean_gap_s.unwrap() < 5940.0,
            "{pole:?}"
        );
        assert!(
            horizon
                .cells
                .iter()
                .all(|c| c.boundary.as_ref().is_some_and(|b| b.len() >= 5))
        );

        let cone = r#"[{ "body": [0, 0, 1], "halfAngleDeg": 30 }]"#;
        let imaged = run(format!(
            r#"{{ "spacecraft": [{}], "resolution": 2, "stepS": 60,
                 "bounds": [-30, 30, 40, 70] }}"#,
            craft(0.0, cone)
        ))
        .unwrap();
        assert!(imaged.cells.iter().all(|c| c.boundary.is_none()));
        assert!(
            (100..400).contains(&imaged.cells.len()),
            "{}",
            imaged.cells.len()
        );
        let fraction =
            |cells: &[CellInfo]| cells.iter().map(|c| c.fraction).sum::<f64>() / cells.len() as f64;
        let wide = run(format!(
            r#"{{ "spacecraft": [{}], "resolution": 2, "stepS": 60, "bounds": [-30, 30, 40, 70] }}"#,
            craft(0.0, "[]")
        ))
        .unwrap();
        assert!(fraction(&imaged.cells) > 0.0);
        assert!(fraction(&imaged.cells) < 0.5 * fraction(&wide.cells));

        for (json, message) in [
            (r#"{ "spacecraft": [] }"#.to_string(), "needs a spacecraft"),
            (
                format!(
                    r#"{{ "spacecraft": [{}], "resolution": 16 }}"#,
                    craft(0.0, "[]")
                ),
                "resolution",
            ),
            (
                format!(
                    r#"{{ "spacecraft": [{}], "resolution": 4 }}"#,
                    craft(0.0, "[]")
                ),
                "cells",
            ),
            (
                format!(
                    r#"{{ "spacecraft": [{}], "bounds": [0, 10, 10, 0] }}"#,
                    craft(0.0, "[]")
                ),
                "bounds",
            ),
            (
                format!(
                    r#"{{ "spacecraft": [{}] }}"#,
                    craft(0.0, r#"[{ "body": [0, 0, 1] }]"#)
                ),
                "halfAngleDeg",
            ),
        ] {
            let err = run(json).unwrap_err();
            assert!(err.contains(message), "{err}");
        }
    }

    #[test]
    fn screens_a_crossing_secondary_and_filters_a_distant_tle() {
        let forces =
            r#""forces": { "gravity": [2, 0], "thirdBody": false, "srp": false, "drag": "none" }"#;
        let primary = request(&format!(
            r#"{{
                "name": "PRIMARY", "objectId": "PRI",
                "initial": {{ "epoch": "2024-03-01T00:00:00Z",
                              "positionKm": [6778.137, 0, 0], "velocityKmS": [0, 4.7, 6.0] }},
                "durationS": 7200, "stepS": 60, {forces},
                "covariance": {{ "positionSigmaKm": [0.05, 0.2, 0.05], "velocitySigmaKmS": [1e-4, 5e-5, 5e-5] }}
            }}"#
        ));
        // Secondary at the primary's 1 h position + 200 m radial, velocity turned 30° about r̂.
        let at = trajectory(&primary).unwrap().states[60];
        let (r, v) = (at.position_km, at.velocity_km_s);
        let v2 = DQuat::from_axis_angle(r.normalize(), 30f64.to_radians()) * v;
        let r2 = r + r.normalize() * 0.2;
        let mut req: ConjunctionRequest = serde_json::from_value(serde_json::json!({
            "primary": serde_json::from_str::<serde_json::Value>(&format!(
                r#"{{ "name": "PRIMARY", "objectId": "PRI",
                     "initial": {{ "epoch": "2024-03-01T00:00:00Z",
                                   "positionKm": [6778.137, 0, 0], "velocityKmS": [0, 4.7, 6.0] }},
                     "durationS": 7200, "stepS": 60, {forces},
                     "covariance": {{ "positionSigmaKm": [0.05, 0.2, 0.05],
                                      "velocitySigmaKmS": [1e-4, 5e-5, 5e-5] }} }}"#
            )).unwrap(),
            "secondaries": [
                { "id": "crossing", "propagation": serde_json::from_str::<serde_json::Value>(&format!(
                    r#"{{ "name": "CROSSER",
                         "initial": {{ "epoch": "2024-03-01T01:00:00Z",
                                       "positionKm": [{}, {}, {}], "velocityKmS": [{}, {}, {}] }},
                         "start": "2024-03-01T00:00:00Z", "durationS": 7200, "stepS": 60, {forces} }}"#,
                    r2.x, r2.y, r2.z, v2.x, v2.y, v2.z
                )).unwrap() },
                { "tle": [
                    "1 00005U 58002B   00179.78495062  .00000023  00000-0  28098-4 0  4753",
                    "2 00005  34.2682 348.7242 1859667 331.7664  19.3264 10.82419157413667"] }
            ],
            "thresholdKm": 1.0,
            "secondarySigmaKm": [0.1, 0.5, 0.1],
            "cdm": true
        }))
        .unwrap();
        let out = screen(&req, &[]).unwrap();
        assert_eq!((out.screened, out.filtered), (1, 1));
        let [c] = &out.conjunctions[..] else {
            panic!("{:?}", out.conjunctions)
        };
        assert_eq!(c.secondary, "crossing");
        assert!(
            (c.tca - at.epoch.to_unix_seconds()).abs() < 1e-2,
            "{}",
            c.tca
        );
        assert!((c.miss_km - 0.2).abs() < 1e-4, "{}", c.miss_km);
        assert!((c.relative_position_rsw_km[0] - 0.2).abs() < 1e-4);
        assert!((c.relative_speed_km_s - 2.0 * v.length() * 15f64.to_radians().sin()).abs() < 1e-3);
        let pc = c.pc.unwrap();
        assert!(pc > 1e-6 && pc < 1.0 && c.pc_max >= pc, "{pc} {}", c.pc_max);
        let cdm = Cdm::from_kvn(c.cdm.as_deref().unwrap()).unwrap();
        assert_eq!(cdm.objects[0].designator, "PRI");
        assert_eq!(cdm.objects[0].covariance_method, "CALCULATED");
        assert_eq!(cdm.objects[1].covariance_method, "DEFAULT");
        assert_eq!(cdm.collision_probability, Some(pc));

        // Below the miss: nothing. Bad secondaries refused.
        req.threshold_km = 0.1;
        assert!(screen(&req, &[]).unwrap().conjunctions.is_empty());
        req.secondaries[1].omm = Some(serde_json::json!({}));
        assert!(screen(&req, &[]).unwrap_err().contains("one of"));
    }
}
