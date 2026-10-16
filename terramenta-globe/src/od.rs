//! `determineOrbit`: batch least squares or EKF on the `propagate` force model, from
//! observations, TDM text or simulated tracking of a truth propagation.

use bevy::math::DVec3;
use serde::{Deserialize, Serialize};
use terramenta_solare::ccsds::{Header, Tdm, TimeSystem};
use terramenta_solare::covariance::{self, Matrix6};
use terramenta_solare::events::GroundStation;
use terramenta_solare::interpolate::{Interpolation, SampledEphemeris};
use terramenta_solare::maneuver::LocalFrame;
use terramenta_solare::od::{
    self, BatchSettings, FilterSettings, Kind, Observation, Parameter, Tracking,
};
use terramenta_solare::{Epoch, SpacecraftState};

use crate::numerical::{
    PropagationRequest, StationOptions, creation_date, environment, epoch, numerical_propagator,
    propagate, spacecraft_at_start,
};

const MAX_OBSERVATIONS: usize = 100_000;

/// `determineOrbit` options.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OdRequest {
    /// A priori: `initial` at `start`, forces, vehicle; `covariance` is the a priori
    /// covariance and EKF process noise. Stop and duration unused.
    pub propagation: PropagationRequest,
    #[serde(default)]
    pub stations: Vec<StationOptions>,
    #[serde(default)]
    pub observations: Vec<ObservationOptions>,
    /// TDM KVN; weights from `sigmas`.
    #[serde(default)]
    pub tdm: Option<String>,
    #[serde(default)]
    pub simulate: Option<SimulateOptions>,
    #[serde(default)]
    pub sigmas: SigmaOptions,
    /// `batch` (default) | `ekf`.
    #[serde(default)]
    pub estimator: Option<String>,
    #[serde(default)]
    pub solve_for: Vec<SolveForOptions>,
    #[serde(default = "yes")]
    pub light_time: bool,
    #[serde(default = "default_iterations")]
    pub max_iterations: usize,
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
    /// Batch: × previous RMS (default 3); EKF: innovation σ (default 5); 0 disables.
    #[serde(default)]
    pub edit_sigma: Option<f64>,
    /// Return the observations as TDM KVN.
    #[serde(default)]
    pub export_tdm: bool,
}

fn yes() -> bool {
    true
}

fn default_iterations() -> usize {
    10
}

fn default_tolerance() -> f64 {
    1e-3
}

/// `type`: range (km), rangeRate (km/s), azimuth, elevation, rightAscension, declination
/// (deg), positionX/Y/Z (GCRF km).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationOptions {
    pub time: String,
    #[serde(default)]
    pub station: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub value: f64,
    /// Default from `sigmas`.
    #[serde(default)]
    pub sigma: Option<f64>,
}

/// Default 1σ per type.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SigmaOptions {
    pub range_km: f64,
    pub range_rate_km_s: f64,
    pub angle_deg: f64,
    pub position_km: f64,
}

impl Default for SigmaOptions {
    fn default() -> Self {
        Self {
            range_km: 0.01,
            range_rate_km_s: 1e-5,
            angle_deg: 0.005,
            position_km: 0.01,
        }
    }
}

impl SigmaOptions {
    /// Display units.
    fn of(&self, kind: Kind) -> f64 {
        match kind {
            Kind::Range => self.range_km,
            Kind::RangeRate => self.range_rate_km_s,
            Kind::Position(_) => self.position_km,
            _ => self.angle_deg,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulateOptions {
    /// As `propagate`; its span bounds the tracking.
    pub truth: PropagationRequest,
    pub tracking: Vec<TrackingOptions>,
    /// UTC; default the truth span.
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default = "default_tracking_step")]
    pub step_s: f64,
    #[serde(default = "default_seed")]
    pub seed: u64,
}

fn default_tracking_step() -> f64 {
    60.0
}

fn default_seed() -> u64 {
    1
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackingOptions {
    #[serde(rename = "type")]
    pub kind: String,
    /// Station id; absent: every station (station types).
    #[serde(default)]
    pub station: Option<String>,
    /// Display units; default from `sigmas`.
    #[serde(default)]
    pub sigma: Option<f64>,
    #[serde(default)]
    pub bias: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolveForOptions {
    /// dragCoefficient | reflectivity.
    pub parameter: String,
    /// A priori 1σ.
    pub sigma: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OdResult {
    pub estimator: &'static str,
    pub converged: bool,
    /// Unix s, UTC. Batch: the a priori epoch; EKF: the last observation.
    pub epoch: f64,
    pub position_km: [f64; 3],
    pub velocity_km_s: [f64; 3],
    /// 1σ RSW, km and km/s.
    pub sigma_rsw: [f64; 6],
    /// GCRF `[r, v]`, lower triangle (21).
    pub covariance: Vec<f64>,
    pub parameters: Vec<ParameterInfo>,
    pub iterations: Vec<IterationInfo>,
    pub residuals: Vec<ResidualInfo>,
    /// Weighted RMS of accepted residuals (batch: final; EKF: post-fit).
    pub rms: f64,
    pub observations: usize,
    pub edited: usize,
    /// EKF: posterior 1σ RSW after each observation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryInfo>,
    /// Simulated runs: estimate − truth at `epoch`, truth RSW.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truth_error_rsw: Option<[f64; 6]>,
    /// `propagate` patch for the estimate: `initial`, `covariance`, `vehicle`.
    pub estimate: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tdm: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterInfo {
    pub parameter: &'static str,
    pub apriori: f64,
    pub value: f64,
    pub sigma: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IterationInfo {
    pub rms: f64,
    pub accepted: usize,
    pub edited: usize,
    pub correction_km: f64,
    pub correction_km_s: f64,
}

/// Values in display units (km, km/s, deg).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResidualInfo {
    /// Unix s.
    pub time: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub observed: f64,
    /// Observed − computed: batch final, EKF pre-fit innovation.
    pub residual: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_fit: Option<f64>,
    pub sigma: f64,
    pub edited: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryInfo {
    pub times: Vec<f64>,
    pub sigma_rsw: Vec<[f64; 6]>,
}

fn kind(name: &str) -> Result<Kind, String> {
    Ok(match name {
        "range" => Kind::Range,
        "rangeRate" => Kind::RangeRate,
        "azimuth" => Kind::Azimuth,
        "elevation" => Kind::Elevation,
        "rightAscension" => Kind::RightAscension,
        "declination" => Kind::Declination,
        "positionX" => Kind::Position(0),
        "positionY" => Kind::Position(1),
        "positionZ" => Kind::Position(2),
        other => {
            return Err(format!(
                "unknown observation type {other:?}: range, rangeRate, azimuth, elevation, \
                 rightAscension, declination, positionX/Y/Z"
            ));
        }
    })
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Range => "range",
        Kind::RangeRate => "rangeRate",
        Kind::Azimuth => "azimuth",
        Kind::Elevation => "elevation",
        Kind::RightAscension => "rightAscension",
        Kind::Declination => "declination",
        Kind::Position(0) => "positionX",
        Kind::Position(1) => "positionY",
        Kind::Position(_) => "positionZ",
    }
}

/// Internal (km, km/s, rad) per display unit.
fn scale(kind: Kind) -> f64 {
    match kind {
        Kind::Range | Kind::RangeRate | Kind::Position(_) => 1.0,
        _ => 1f64.to_radians(),
    }
}

fn parameter(name: &str) -> Result<Parameter, String> {
    match name {
        "dragCoefficient" => Ok(Parameter::DragCoefficient),
        "reflectivity" => Ok(Parameter::Reflectivity),
        other => Err(format!(
            "solveFor: unknown parameter {other:?}: dragCoefficient, reflectivity"
        )),
    }
}

fn parameter_name(p: Parameter) -> &'static str {
    match p {
        Parameter::DragCoefficient => "dragCoefficient",
        Parameter::Reflectivity => "reflectivity",
    }
}

fn rsw_sigmas(p: &Matrix6, s: &SpacecraftState) -> [f64; 6] {
    covariance::sigmas(&covariance::to_local(
        p,
        LocalFrame::Rsw,
        s.position_km,
        s.velocity_km_s,
    ))
}

pub fn determine_orbit(req: &OdRequest) -> Result<OdResult, String> {
    let env = environment();
    let stations = req
        .stations
        .iter()
        .map(StationOptions::station)
        .collect::<Result<Vec<GroundStation>, _>>()?;
    let station_index = |id: &str| {
        stations
            .iter()
            .position(|s| s.name == id)
            .ok_or_else(|| format!("unknown station {id:?}"))
    };
    crate::numerical::earth_only(req.propagation.central_body, "orbit determination")?;
    let (sc, _) = spacecraft_at_start(&req.propagation, &env)?;
    let apriori = *sc.state();
    let mut propagator = numerical_propagator(&sc);
    drop(env);

    // Observations: explicit, TDM, simulated.
    let mut observations = Vec::new();
    for (i, o) in req.observations.iter().enumerate() {
        let k = kind(&o.kind).map_err(|e| format!("observations[{i}]: {e}"))?;
        let station = match (&o.station, k.needs_station()) {
            (Some(id), true) => Some(station_index(id)?),
            (None, true) => return Err(format!("observations[{i}]: {} needs a station", o.kind)),
            _ => None,
        };
        let sigma = o.sigma.unwrap_or(req.sigmas.of(k));
        if !(sigma.is_finite() && sigma > 0.0 && o.value.is_finite()) {
            return Err(format!("observations[{i}]: bad value or sigma"));
        }
        observations.push(Observation {
            epoch: epoch(&o.time)?,
            kind: k,
            station,
            value: o.value * scale(k),
            sigma: sigma * scale(k),
        });
    }
    if let Some(text) = &req.tdm {
        let tdm = Tdm::from_kvn(text).map_err(|e| format!("TDM: {e}"))?;
        let sigmas = req.sigmas;
        observations.extend(
            od::from_tdm(&tdm, &stations, |k| sigmas.of(k) * scale(k))
                .map_err(|e| format!("TDM: {e}"))?,
        );
    }
    let mut truth = None;
    if let Some(sim) = &req.simulate {
        let t = propagate(&PropagationRequest {
            covariance: None,
            kvn: false,
            ..sim.truth.clone()
        })
        .map_err(|e| format!("simulate.truth: {e}"))?;
        let ephemeris = SampledEphemeris::new(t.ordered_states(), Interpolation::default());
        let (first, last) = ephemeris.coverage();
        let start = sim
            .start
            .as_deref()
            .map(epoch)
            .transpose()?
            .unwrap_or(first);
        let stop = sim.stop.as_deref().map(epoch).transpose()?.unwrap_or(last);
        let step = sim.step_s.abs();
        if step.is_nan()
            || step <= 0.0
            || stop.seconds_since(start) / step > MAX_OBSERVATIONS as f64
        {
            return Err(format!(
                "simulate.stepS must be positive and yield ≤ {MAX_OBSERVATIONS} epochs"
            ));
        }
        let mut tracking = Vec::new();
        for (i, t) in sim.tracking.iter().enumerate() {
            let k = kind(&t.kind).map_err(|e| format!("simulate.tracking[{i}]: {e}"))?;
            let sigma = t.sigma.unwrap_or(req.sigmas.of(k)) * scale(k);
            let bias = t.bias * scale(k);
            if !(sigma.is_finite() && sigma >= 0.0 && bias.is_finite()) {
                return Err(format!("simulate.tracking[{i}]: bad sigma or bias"));
            }
            let sites: Vec<Option<usize>> = match (&t.station, k.needs_station()) {
                (_, false) => vec![None],
                (Some(id), true) => vec![Some(station_index(id)?)],
                (None, true) => (0..stations.len()).map(Some).collect(),
            };
            tracking.extend(sites.into_iter().map(|station| Tracking {
                kind: k,
                station,
                sigma,
                bias,
            }));
        }
        let orientation = propagator
            .orientation
            .clone()
            .ok_or("no Earth orientation")?;
        let mut at = |e: Epoch| {
            ephemeris
                .try_state_at(e)
                .map(|s| (s.position_km, s.velocity_km_s))
        };
        let mut simulated = od::simulate(
            &mut at,
            orientation.as_ref(),
            &stations,
            &tracking,
            start,
            stop,
            step,
            req.light_time,
            sim.seed,
        );
        // Zero-σ tracking is noiseless; weight it as `sigmas`.
        for o in &mut simulated {
            if o.sigma == 0.0 {
                o.sigma = req.sigmas.of(o.kind) * scale(o.kind);
            }
        }
        observations.extend(simulated);
        truth = Some(ephemeris);
    }
    if observations.is_empty() {
        return Err("no observations".into());
    }
    if observations.len() > MAX_OBSERVATIONS {
        return Err(format!("more than {MAX_OBSERVATIONS} observations"));
    }

    let a_priori_cov = match &req.propagation.covariance {
        Some(options) => {
            let (frame, p0, noise) = options.resolve()?;
            Some((
                covariance::from_local(&p0, frame, apriori.position_km, apriori.velocity_km_s),
                noise,
            ))
        }
        None => None,
    };
    let edit = |default: f64| match req.edit_sigma {
        Some(k) if k <= 0.0 => None,
        Some(k) => Some(k),
        None => Some(default),
    };
    let solve_for = req
        .solve_for
        .iter()
        .map(|s| {
            let p = parameter(&s.parameter)?;
            if p == Parameter::Reflectivity && !propagator.vehicle.plates.is_empty() {
                return Err("solveFor reflectivity: plates have no reflectivity".into());
            }
            if !(s.sigma.is_finite() && s.sigma > 0.0) {
                return Err(format!("solveFor {}: sigma must be positive", s.parameter));
            }
            Ok((p, s.sigma))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let station_name = |o: &Observation| o.station.map(|s| stations[s].name.clone());
    let info = |o: &Observation, residual: f64, post_fit: Option<f64>, edited: bool| {
        let s = scale(o.kind);
        ResidualInfo {
            time: o.epoch.to_unix_seconds(),
            station: station_name(o),
            kind: kind_name(o.kind),
            observed: o.value / s,
            residual: residual / s,
            post_fit: post_fit.map(|p| p / s),
            sigma: o.sigma / s,
            edited,
        }
    };

    let estimator = req.estimator.as_deref().unwrap_or("batch");
    let (mut out, at) = match estimator {
        "batch" => {
            let settings = BatchSettings {
                light_time: req.light_time,
                max_iterations: req.max_iterations.clamp(1, 50),
                tolerance: req.tolerance,
                edit_sigma: edit(3.0),
                parameters: solve_for.clone(),
            };
            let apriori_values: Vec<f64> = solve_for
                .iter()
                .map(|(p, _)| match p {
                    Parameter::DragCoefficient => propagator.vehicle.drag_coefficient,
                    Parameter::Reflectivity => propagator.vehicle.reflectivity,
                })
                .collect();
            let sol = od::batch(
                &mut propagator,
                &stations,
                &observations,
                &apriori,
                a_priori_cov.as_ref().map(|(p, _)| p),
                &settings,
            )
            .map_err(|e| e.to_string())?;
            let residuals: Vec<ResidualInfo> = sol
                .residuals
                .iter()
                .map(|r| info(&observations[r.observation], r.value, None, r.edited))
                .collect();
            let last = sol.iterations.last().expect("at least one iteration");
            let result = OdResult {
                estimator: "batch",
                converged: sol.converged,
                epoch: sol.state.epoch.to_unix_seconds(),
                position_km: sol.state.position_km.to_array(),
                velocity_km_s: sol.state.velocity_km_s.to_array(),
                sigma_rsw: rsw_sigmas(&sol.covariance, &sol.state),
                covariance: covariance::lower_triangle(&sol.covariance).to_vec(),
                parameters: sol
                    .parameters
                    .iter()
                    .zip(apriori_values)
                    .map(|(&(p, value, sigma), apriori)| ParameterInfo {
                        parameter: parameter_name(p),
                        apriori,
                        value,
                        sigma,
                    })
                    .collect(),
                iterations: sol
                    .iterations
                    .iter()
                    .map(|i| IterationInfo {
                        rms: i.rms,
                        accepted: i.accepted,
                        edited: i.edited,
                        correction_km: i.correction_km,
                        correction_km_s: i.correction_km_s,
                    })
                    .collect(),
                rms: last.rms,
                observations: observations.len(),
                edited: last.edited,
                residuals,
                history: None,
                truth_error_rsw: None,
                estimate: serde_json::Value::Null,
                tdm: None,
            };
            (result, sol.state.epoch)
        }
        "ekf" => {
            if !solve_for.is_empty() {
                return Err("solveFor needs the batch estimator".into());
            }
            let (p0, noise) = a_priori_cov.ok_or("ekf needs propagation.covariance")?;
            let settings = FilterSettings {
                light_time: req.light_time,
                edit_sigma: edit(5.0),
                process_noise: noise,
            };
            let steps = od::filter(
                &mut propagator,
                &stations,
                &observations,
                &apriori,
                &p0,
                &settings,
            )
            .map_err(|e| e.to_string())?;
            let last = steps.last().expect("one step per observation");
            let accepted: Vec<_> = steps.iter().filter(|s| !s.edited).collect();
            let rms = (accepted
                .iter()
                .map(|s| (s.post_fit / observations[s.observation].sigma).powi(2))
                .sum::<f64>()
                / accepted.len().max(1) as f64)
                .sqrt();
            let result = OdResult {
                estimator: "ekf",
                converged: true,
                epoch: last.state.epoch.to_unix_seconds(),
                position_km: last.state.position_km.to_array(),
                velocity_km_s: last.state.velocity_km_s.to_array(),
                sigma_rsw: rsw_sigmas(&last.covariance, &last.state),
                covariance: covariance::lower_triangle(&last.covariance).to_vec(),
                parameters: Vec::new(),
                iterations: Vec::new(),
                rms,
                observations: observations.len(),
                edited: steps.len() - accepted.len(),
                residuals: steps
                    .iter()
                    .map(|s| {
                        info(
                            &observations[s.observation],
                            s.residual,
                            Some(s.post_fit),
                            s.edited,
                        )
                    })
                    .collect(),
                history: Some(HistoryInfo {
                    times: steps
                        .iter()
                        .map(|s| s.state.epoch.to_unix_seconds())
                        .collect(),
                    sigma_rsw: steps
                        .iter()
                        .map(|s| rsw_sigmas(&s.covariance, &s.state))
                        .collect(),
                }),
                truth_error_rsw: None,
                estimate: serde_json::Value::Null,
                tdm: None,
            };
            (result, last.state.epoch)
        }
        other => return Err(format!("estimator: expected batch or ekf, got {other:?}")),
    };

    if let Some(t) = truth.as_ref().and_then(|eph| eph.try_state_at(at)) {
        let rot = LocalFrame::Rsw
            .to_inertial(t.position_km, t.velocity_km_s)
            .transpose();
        let dr = rot * (DVec3::from_array(out.position_km) - t.position_km);
        let dv = rot * (DVec3::from_array(out.velocity_km_s) - t.velocity_km_s);
        out.truth_error_rsw = Some([dr.x, dr.y, dr.z, dv.x, dv.y, dv.z]);
    }
    let mut vehicle = serde_json::Map::new();
    for p in &out.parameters {
        vehicle.insert(p.parameter.into(), p.value.into());
    }
    out.estimate = serde_json::json!({
        "initial": {
            "epoch": format!("{}Z", TimeSystem::Utc.format(at)),
            "positionKm": out.position_km,
            "velocityKmS": out.velocity_km_s,
        },
        "covariance": { "frame": "gcrf", "lowerTriangle": out.covariance },
        "vehicle": vehicle,
    });
    if req.export_tdm {
        let mut tdm = od::to_tdm(
            &observations,
            &stations,
            req.propagation.name.as_deref().unwrap_or("SPACECRAFT"),
        );
        tdm.header = Header {
            creation_date: creation_date(),
            ..Header::default()
        };
        out.tdm = Some(tdm.to_kvn());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(estimator: &str, extra: serde_json::Value) -> OdRequest {
        let forces = serde_json::json!({ "gravity": [4, 4], "thirdBody": false, "srp": false, "drag": "none" });
        let mut req = serde_json::json!({
            "propagation": {
                "name": "SAT",
                "initial": { "epoch": "2024-03-01T00:00:00Z",
                             "positionKm": [7003.0, -2.0, 1.5], "velocityKmS": [0.002, 3.19, 6.84] },
                "forces": forces,
                "covariance": { "positionSigmaKm": [5, 5, 5], "velocitySigmaKmS": [0.005, 0.005, 0.005] }
            },
            "stations": [
                { "id": "svalbard", "latDeg": 78.23, "lonDeg": 15.39, "minElevationDeg": 5 },
                { "id": "fairbanks", "latDeg": 64.86, "lonDeg": -147.85, "minElevationDeg": 5 },
                { "id": "wallops", "latDeg": 37.94, "lonDeg": -75.46, "minElevationDeg": 5 },
                { "id": "hartrao", "latDeg": -25.89, "lonDeg": 27.69, "minElevationDeg": 5 }
            ],
            "simulate": {
                "truth": {
                    "initial": { "epoch": "2024-03-01T00:00:00Z",
                                 "positionKm": [7000.0, 0, 0], "velocityKmS": [0, 3.1891, 6.8390] },
                    "durationS": 43200, "stepS": 60, "forces": forces
                },
                "tracking": [{ "type": "range" }, { "type": "azimuth" }, { "type": "elevation" }],
                "stepS": 30, "seed": 4
            },
            "estimator": estimator,
            "exportTdm": true
        });
        for (k, v) in extra.as_object().unwrap() {
            req[k] = v.clone();
        }
        serde_json::from_value(req).unwrap()
    }

    #[test]
    fn batch_and_ekf_recover_a_simulated_truth() {
        let batch = determine_orbit(&request("batch", serde_json::json!({}))).unwrap();
        assert!(batch.converged, "{:?}", batch.iterations);
        assert!(batch.observations > 100);
        let err = batch.truth_error_rsw.unwrap();
        let miss = (err[0].powi(2) + err[1].powi(2) + err[2].powi(2)).sqrt();
        assert!(miss < 0.01, "{err:?}");
        for i in 0..3 {
            assert!(
                err[i].abs() < 4.0 * batch.sigma_rsw[i],
                "{err:?} vs {:?}",
                batch.sigma_rsw
            );
        }
        assert!((0.7..1.3).contains(&batch.rms), "{}", batch.rms);
        assert_eq!(batch.residuals.len(), batch.observations);
        assert_eq!(
            batch.estimate["initial"]["epoch"],
            "2024-03-01T00:00:00.000000000Z"
        );

        let ekf = determine_orbit(&request("ekf", serde_json::json!({}))).unwrap();
        let err = ekf.truth_error_rsw.unwrap();
        let miss = (err[0].powi(2) + err[1].powi(2) + err[2].powi(2)).sqrt();
        assert!(miss < 0.05, "{err:?}");
        let history = ekf.history.unwrap();
        assert_eq!(history.times.len(), ekf.observations);
        assert!(history.sigma_rsw.last().unwrap()[1] < 0.1 * 5.0);

        // TDM written by one run feeds another.
        let tdm = batch.tdm.unwrap();
        let mut again = request("batch", serde_json::json!({ "tdm": tdm }));
        again.simulate = None;
        again.sigmas.angle_deg = 0.005;
        let from_tdm = determine_orbit(&again).unwrap();
        assert_eq!(from_tdm.observations, batch.observations);
        assert!((from_tdm.position_km[0] - batch.position_km[0]).abs() < 1e-3);
    }

    #[test]
    fn refuses_bad_requests() {
        let mut req = request("ekf", serde_json::json!({}));
        req.propagation.covariance = None;
        assert!(determine_orbit(&req).unwrap_err().contains("covariance"));
        let req = request(
            "batch",
            serde_json::json!({ "observations": [
                { "time": "2024-03-01T00:10:00Z", "type": "range", "value": 1000 }
            ]}),
        );
        assert!(
            determine_orbit(&req)
                .unwrap_err()
                .contains("needs a station")
        );
        let req = request("lsq", serde_json::json!({}));
        assert!(determine_orbit(&req).unwrap_err().contains("estimator"));
        let req = request(
            "batch",
            serde_json::json!({ "solveFor": [{ "parameter": "mass", "sigma": 1 }] }),
        );
        assert!(
            determine_orbit(&req)
                .unwrap_err()
                .contains("unknown parameter")
        );
    }
}
