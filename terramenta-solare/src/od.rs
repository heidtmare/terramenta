//! Orbit determination: tracking measurement models with analytic partials, batch weighted
//! least squares (state, optionally Cd and Cr) and an extended Kalman filter over
//! [`Propagator`] variational equations; measurement simulation; TDM conversion.
//!
//! States: central-body-centred GCRF `[r, v]`, km, km/s. Stations: ITRF through the
//! propagator's [`BodyOrientation`]. Observations are tagged at receive time. Light time
//! (one leg, spacecraft → station): `r(t − τ) ≈ r(t) − v τ` in estimation, exact in
//! [`simulate`]; partials hold `τ` fixed. No aberration, refraction or media delays.
//! Measurement noise uncorrelated.
//!
//! Batch (Gauss–Newton): `Λ δx = N`, `Λ = P̄⁻¹ + Σ Hᵀ W H`, `N = P̄⁻¹ (x̄ − x) + Σ Hᵀ W y`,
//! `H = H̃ [Φ(t, t₀) | ∂x(t)/∂p]`, parameter columns by forward differences of the
//! trajectory. Covariance `Λ⁻¹` (Jacobi-scaled Cholesky). Converged when the weighted RMS
//! changes by less than `tolerance` (relative) or the correction is negligible,
//! `δxᵀ Λ δx ≤ tolerance · n`. Editing from the second iteration:
//! `|y|/σ > edit_sigma · max(RMS₋₁, 1)`.
//!
//! EKF: scalar sequential updates, Joseph form, reference reset to the estimate after each
//! update; `P ← Φ P Φᵀ + Q` between epochs. Gate: `|y| / √(H P Hᵀ + σ²) > edit_sigma`.
//!
//! Tapley, Schutz & Born, *Statistical Orbit Determination* (2004), ch. 4.

use glam::DVec3;

use crate::ccsds::{Tdm, TdmMetadata, TdmRecord, TdmSegment};
use crate::covariance::{Matrix6, ProcessNoise, add, congruence, identity, mul, symmetrize};
use crate::earth::TerrestrialTransform;
use crate::events::GroundStation;
use crate::frame::StateVector;
use crate::propagate::{BodyOrientation, PropagationError, Propagator, SpacecraftState};
use crate::time::Epoch;

/// km/s.
pub const SPEED_OF_LIGHT_KM_S: f64 = 299_792.458;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// One-way geometric range, km.
    Range,
    /// km/s.
    RangeRate,
    /// From north, clockwise, rad.
    Azimuth,
    /// Above the geodetic horizon, rad.
    Elevation,
    /// Topocentric GCRF, rad.
    RightAscension,
    Declination,
    /// GCRF component `0..3`, km (GNSS). No station.
    Position(u8),
}

impl Kind {
    pub fn needs_station(self) -> bool {
        !matches!(self, Self::Position(_))
    }

    /// Residuals wrap to `[−π, π)`.
    fn wraps(self) -> bool {
        matches!(self, Self::Azimuth | Self::RightAscension)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    pub epoch: Epoch,
    pub kind: Kind,
    /// Index into the station list; `None` for [`Kind::Position`].
    pub station: Option<usize>,
    pub value: f64,
    /// 1σ, same unit as `value`.
    pub sigma: f64,
}

/// Station in GCRF at one epoch, with its topocentric axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StationState {
    pub position_km: DVec3,
    pub velocity_km_s: DVec3,
    pub east: DVec3,
    pub north: DVec3,
    pub up: DVec3,
}

pub fn station_state(station: &GroundStation, tf: &TerrestrialTransform) -> StationState {
    let s = tf.from_itrf(StateVector::new(station.position_itrf_km(), DVec3::ZERO));
    let (sp, cp) = station.latitude_rad.sin_cos();
    let (sl, cl) = station.longitude_rad.sin_cos();
    let back = tf.rotation().transpose();
    StationState {
        position_km: s.position_km,
        velocity_km_s: s.velocity_km_s,
        east: back * DVec3::new(-sl, cl, 0.0),
        north: back * DVec3::new(-sp * cl, -sp * sl, cp),
        up: back * DVec3::new(cp * cl, cp * sl, sp),
    }
}

/// Value and `∂h/∂[r, v]` from the light-time-corrected relative position `rho`
/// (spacecraft − station), relative velocity `dv` and light time `tau`.
fn model(
    kind: Kind,
    r: DVec3,
    rho: DVec3,
    dv: DVec3,
    tau: f64,
    st: Option<&StationState>,
) -> (f64, [f64; 6]) {
    // `g = ∂h/∂ρ`; `∂ρ/∂r = I`, `∂ρ/∂v = −τ I`.
    let through_rho = |value: f64, g: DVec3| {
        let v = -tau * g;
        (value, [g.x, g.y, g.z, v.x, v.y, v.z])
    };
    let d = rho.length();
    match kind {
        Kind::Position(i) => {
            let i = usize::from(i.min(2));
            let mut h = [0.0; 6];
            h[i] = 1.0;
            (r[i], h)
        }
        Kind::Range => through_rho(d, rho / d),
        Kind::RangeRate => {
            let u = rho / d;
            let rate = u.dot(dv);
            let g = (dv - rate * u) / d;
            let hv = u - tau * g;
            (rate, [g.x, g.y, g.z, hv.x, hv.y, hv.z])
        }
        Kind::Azimuth | Kind::Elevation => {
            let st = st.expect("station kinds carry a station");
            let (e, n, u) = (rho.dot(st.east), rho.dot(st.north), rho.dot(st.up));
            let h2 = e * e + n * n;
            if kind == Kind::Azimuth {
                let az = e.atan2(n).rem_euclid(std::f64::consts::TAU);
                through_rho(az, (n * st.east - e * st.north) / h2)
            } else {
                let el = (u / d).clamp(-1.0, 1.0).asin();
                through_rho(el, (st.up * d * d - u * rho) / (d * d * h2.sqrt()))
            }
        }
        Kind::RightAscension => {
            let h2 = rho.x * rho.x + rho.y * rho.y;
            let ra = rho.y.atan2(rho.x).rem_euclid(std::f64::consts::TAU);
            through_rho(ra, DVec3::new(-rho.y, rho.x, 0.0) / h2)
        }
        Kind::Declination => {
            let dec = (rho.z / d).clamp(-1.0, 1.0).asin();
            let g =
                (DVec3::Z * d * d - rho.z * rho) / (d * d * (rho.x * rho.x + rho.y * rho.y).sqrt());
            through_rho(dec, g)
        }
    }
}

/// Computed value and `∂h/∂[r, v]` at the receive epoch. `None` if `kind` needs a
/// station and `station` is `None`.
pub fn predict(
    kind: Kind,
    r: DVec3,
    v: DVec3,
    station: Option<&StationState>,
    light_time: bool,
) -> Option<(f64, [f64; 6])> {
    let Some(st) = station.filter(|_| kind.needs_station()) else {
        return (!kind.needs_station()).then(|| model(kind, r, r, v, 0.0, None));
    };
    let mut tau = 0.0;
    if light_time {
        for _ in 0..3 {
            tau = (r - v * tau - st.position_km).length() / SPEED_OF_LIGHT_KM_S;
        }
    }
    let rho = r - v * tau - st.position_km;
    Some(model(kind, r, rho, v - st.velocity_km_s, tau, Some(st)))
}

/// Observed − computed; angles that wrap reduced to `[−π, π)`.
pub fn residual(kind: Kind, observed: f64, computed: f64) -> f64 {
    let y = observed - computed;
    if kind.wraps() {
        use std::f64::consts::{PI, TAU};
        (y + PI).rem_euclid(TAU) - PI
    } else {
        y
    }
}

#[derive(Debug)]
pub enum OdError {
    Propagation(PropagationError),
    /// Station observations without a propagator orientation.
    MissingOrientation,
    /// Observation index whose station is missing or out of range.
    BadStation(usize),
    NoObservations,
    /// Normal or covariance matrix not positive definite.
    Singular,
}

impl std::fmt::Display for OdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Propagation(e) => write!(f, "{e}"),
            Self::MissingOrientation => {
                write!(f, "station observations need an Earth orientation")
            }
            Self::BadStation(i) => write!(f, "observation {i}: no such station"),
            Self::NoObservations => write!(f, "no observations"),
            Self::Singular => write!(
                f,
                "normal matrix singular: observations do not determine the state"
            ),
        }
    }
}

impl std::error::Error for OdError {}

impl From<PropagationError> for OdError {
    fn from(e: PropagationError) -> Self {
        Self::Propagation(e)
    }
}

/// Station state per observation (`None` for stationless kinds).
fn station_states(
    orientation: Option<&dyn BodyOrientation>,
    stations: &[GroundStation],
    observations: &[Observation],
) -> Result<Vec<Option<StationState>>, OdError> {
    observations
        .iter()
        .enumerate()
        .map(|(i, o)| {
            if !o.kind.needs_station() {
                return Ok(None);
            }
            let station = o
                .station
                .and_then(|s| stations.get(s))
                .ok_or(OdError::BadStation(i))?;
            let orientation = orientation.ok_or(OdError::MissingOrientation)?;
            Ok(Some(station_state(
                station,
                &orientation.transform(o.epoch),
            )))
        })
        .collect()
}

/// Distinct epochs ascending, and each observation's index into them.
fn epoch_grid(observations: &[Observation]) -> (Vec<Epoch>, Vec<usize>) {
    let mut order: Vec<usize> = (0..observations.len()).collect();
    order.sort_by(|&a, &b| {
        observations[a]
            .epoch
            .seconds_since(observations[b].epoch)
            .total_cmp(&0.0)
    });
    let mut epochs: Vec<Epoch> = Vec::new();
    let mut slot = vec![0; observations.len()];
    for i in order {
        let e = observations[i].epoch;
        if epochs.last() != Some(&e) {
            epochs.push(e);
        }
        slot[i] = epochs.len() - 1;
    }
    (epochs, slot)
}

/// State (and `Φ(t, t₀)` if `stm`) at each of `epochs` (ascending), walking out from
/// `x0.epoch` in both directions.
fn trajectory(
    p: &mut Propagator,
    x0: &SpacecraftState,
    epochs: &[Epoch],
    stm: bool,
) -> Result<Vec<(SpacecraftState, Matrix6)>, PropagationError> {
    let mut out = vec![(*x0, identity()); epochs.len()];
    let split = epochs.partition_point(|e| e.seconds_since(x0.epoch) < 0.0);
    let order = (split..epochs.len()).chain((0..split).rev());
    let (mut s, mut phi) = (*x0, identity());
    for i in order {
        if i + 1 == split {
            (s, phi) = (*x0, identity());
        }
        if epochs[i] != s.epoch {
            let (next, step) = if stm {
                p.propagate_stm(&s, epochs[i])?
            } else {
                (p.propagate(&s, epochs[i])?, identity())
            };
            s = next;
            phi = mul(&step, &phi);
        }
        out[i] = (s, phi);
    }
    Ok(out)
}

// --- Dense symmetric algebra (n = 6 + parameters) ---------------------------------------

type Dense = Vec<Vec<f64>>;

/// Inverse of a symmetric positive-definite matrix: Jacobi scaling, Cholesky.
fn inverse_spd(a: &Dense) -> Option<Dense> {
    let n = a.len();
    let d: Vec<f64> = (0..n)
        .map(|i| (a[i][i] > 0.0).then(|| a[i][i].sqrt().recip()))
        .collect::<Option<_>>()?;
    let s: Dense = (0..n)
        .map(|i| (0..n).map(|j| a[i][j] * d[i] * d[j]).collect())
        .collect();
    let mut l = vec![vec![0.0; n]; n];
    for j in 0..n {
        let diag = s[j][j] - (0..j).map(|k| l[j][k] * l[j][k]).sum::<f64>();
        if diag <= 1e-13 {
            return None;
        }
        l[j][j] = diag.sqrt();
        for i in j + 1..n {
            l[i][j] = (s[i][j] - (0..j).map(|k| l[i][k] * l[j][k]).sum::<f64>()) / l[j][j];
        }
    }
    // L⁻¹ (lower), then S⁻¹ = L⁻ᵀ L⁻¹.
    let mut li = vec![vec![0.0; n]; n];
    for j in 0..n {
        li[j][j] = 1.0 / l[j][j];
        for i in j + 1..n {
            li[i][j] = -(j..i).map(|k| l[i][k] * li[k][j]).sum::<f64>() / l[i][i];
        }
    }
    Some(
        (0..n)
            .map(|i| {
                (0..n)
                    .map(|j| (i.max(j)..n).map(|k| li[k][i] * li[k][j]).sum::<f64>() * d[i] * d[j])
                    .collect()
            })
            .collect(),
    )
}

fn to_dense(p: &Matrix6) -> Dense {
    p.iter().map(|r| r.to_vec()).collect()
}

fn state_block(p: &Dense) -> Matrix6 {
    std::array::from_fn(|i| std::array::from_fn(|j| p[i][j]))
}

// --- Batch least squares -----------------------------------------------------------------

/// Estimated vehicle parameter (on [`Propagator::vehicle`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parameter {
    DragCoefficient,
    Reflectivity,
}

impl Parameter {
    fn get(self, p: &Propagator) -> f64 {
        match self {
            Self::DragCoefficient => p.vehicle.drag_coefficient,
            Self::Reflectivity => p.vehicle.reflectivity,
        }
    }

    fn set(self, p: &mut Propagator, value: f64) {
        match self {
            Self::DragCoefficient => p.vehicle.drag_coefficient = value,
            Self::Reflectivity => p.vehicle.reflectivity = value,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchSettings {
    pub light_time: bool,
    pub max_iterations: usize,
    /// Relative change in weighted RMS; `δxᵀ Λ δx / n`.
    pub tolerance: f64,
    pub edit_sigma: Option<f64>,
    /// Solve-for parameters with a priori 1σ; a priori values from the propagator.
    pub parameters: Vec<(Parameter, f64)>,
}

impl Default for BatchSettings {
    fn default() -> Self {
        Self {
            light_time: true,
            max_iterations: 10,
            tolerance: 1e-3,
            edit_sigma: Some(3.0),
            parameters: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Residual {
    /// Index into the observations.
    pub observation: usize,
    /// Observed − computed.
    pub value: f64,
    pub sigma: f64,
    pub edited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Iteration {
    /// Weighted RMS of accepted residuals at this iteration's reference.
    pub rms: f64,
    pub accepted: usize,
    pub edited: usize,
    /// Correction computed from it: `|δr|` km, `|δv|` km/s.
    pub correction_km: f64,
    pub correction_km_s: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchSolution {
    /// At the a priori epoch.
    pub state: SpacecraftState,
    /// `[r, v]` block of `full_covariance`.
    pub covariance: Matrix6,
    /// `(parameter, estimate, 1σ)`, in settings order.
    pub parameters: Vec<(Parameter, f64, f64)>,
    /// `(6 + p)²`: `[r, v, parameters]`.
    pub full_covariance: Vec<Vec<f64>>,
    /// At `state`, in observation order.
    pub residuals: Vec<Residual>,
    pub iterations: Vec<Iteration>,
    pub converged: bool,
}

/// Batch weighted least squares about `apriori`. The propagator's vehicle is restored.
pub fn batch(
    propagator: &mut Propagator,
    stations: &[GroundStation],
    observations: &[Observation],
    apriori: &SpacecraftState,
    apriori_covariance: Option<&Matrix6>,
    settings: &BatchSettings,
) -> Result<BatchSolution, OdError> {
    let vehicle = propagator.vehicle.clone();
    let out = run_batch(
        propagator,
        stations,
        observations,
        apriori,
        apriori_covariance,
        settings,
    );
    propagator.vehicle = vehicle;
    out
}

fn run_batch(
    p: &mut Propagator,
    stations: &[GroundStation],
    observations: &[Observation],
    apriori: &SpacecraftState,
    apriori_covariance: Option<&Matrix6>,
    settings: &BatchSettings,
) -> Result<BatchSolution, OdError> {
    if observations.is_empty() {
        return Err(OdError::NoObservations);
    }
    let sites = station_states(p.orientation.as_deref(), stations, observations)?;
    let (epochs, slot) = epoch_grid(observations);
    let params: Vec<Parameter> = settings.parameters.iter().map(|(k, _)| *k).collect();
    let n = 6 + params.len();
    let pack = |s: &SpacecraftState, values: &[f64]| -> Vec<f64> {
        let (r, v) = (s.position_km, s.velocity_km_s);
        [r.x, r.y, r.z, v.x, v.y, v.z]
            .into_iter()
            .chain(values.iter().copied())
            .collect()
    };
    let prior_values: Vec<f64> = params.iter().map(|k| k.get(p)).collect();
    let prior = pack(apriori, &prior_values);
    let mut prior_info = vec![vec![0.0; n]; n];
    if let Some(p0) = apriori_covariance {
        let inv = inverse_spd(&to_dense(&symmetrize(p0))).ok_or(OdError::Singular)?;
        for i in 0..6 {
            prior_info[i][..6].copy_from_slice(&inv[i]);
        }
    }
    for (k, (_, sigma)) in settings.parameters.iter().enumerate() {
        prior_info[6 + k][6 + k] = sigma.powi(-2);
    }

    let mut x = *apriori;
    let mut values = prior_values.clone();
    let mut iterations = Vec::new();
    let mut prev_rms: Option<f64> = None;
    for iter in 0..settings.max_iterations.max(1) {
        for (k, v) in params.iter().zip(&values) {
            k.set(p, *v);
        }
        let reference = trajectory(p, &x, &epochs, true)?;
        let mut sensitivity = Vec::new();
        for (k, kind) in params.iter().enumerate() {
            let step = 1e-3 * values[k].abs().max(1e-2);
            kind.set(p, values[k] + step);
            let shifted = trajectory(p, &x, &epochs, false)?;
            kind.set(p, values[k]);
            sensitivity.push(
                shifted
                    .iter()
                    .zip(&reference)
                    .map(|((a, _), (b, _))| {
                        let d = |u: DVec3, w: DVec3| (u - w) / step;
                        let (dr, dv) = (
                            d(a.position_km, b.position_km),
                            d(a.velocity_km_s, b.velocity_km_s),
                        );
                        [dr.x, dr.y, dr.z, dv.x, dv.y, dv.z]
                    })
                    .collect::<Vec<_>>(),
            );
        }

        let gate = settings
            .edit_sigma
            .filter(|_| iter > 0)
            .map(|k| k * prev_rms.unwrap_or(1.0).max(1.0));
        let deviation: Vec<f64> = {
            let current = pack(&x, &values);
            prior.iter().zip(&current).map(|(a, b)| a - b).collect()
        };
        let mut lambda = prior_info.clone();
        let mut rhs: Vec<f64> = (0..n)
            .map(|i| (0..n).map(|j| prior_info[i][j] * deviation[j]).sum())
            .collect();
        let mut residuals = Vec::with_capacity(observations.len());
        let (mut chi2, mut accepted) = (0.0, 0usize);
        for (i, o) in observations.iter().enumerate() {
            let (s, phi) = &reference[slot[i]];
            let (computed, h) = predict(
                o.kind,
                s.position_km,
                s.velocity_km_s,
                sites[i].as_ref(),
                settings.light_time,
            )
            .ok_or(OdError::BadStation(i))?;
            let y = residual(o.kind, o.value, computed);
            let edited = gate.is_some_and(|g| (y / o.sigma).abs() > g);
            residuals.push(Residual {
                observation: i,
                value: y,
                sigma: o.sigma,
                edited,
            });
            if edited {
                continue;
            }
            let mut row = vec![0.0; n];
            for (j, slot_j) in row.iter_mut().take(6).enumerate() {
                *slot_j = (0..6).map(|k| h[k] * phi[k][j]).sum();
            }
            for (k, sens) in sensitivity.iter().enumerate() {
                row[6 + k] = (0..6).map(|c| h[c] * sens[slot[i]][c]).sum();
            }
            let w = o.sigma.powi(-2);
            for a in 0..n {
                rhs[a] += row[a] * w * y;
                for b in 0..n {
                    lambda[a][b] += row[a] * w * row[b];
                }
            }
            chi2 += y * y * w;
            accepted += 1;
        }
        if accepted == 0 {
            return Err(OdError::NoObservations);
        }
        let rms = (chi2 / accepted as f64).sqrt();
        let cov = inverse_spd(&lambda).ok_or(OdError::Singular)?;
        let dx: Vec<f64> = (0..n)
            .map(|i| (0..n).map(|j| cov[i][j] * rhs[j]).sum())
            .collect();
        let dr = DVec3::new(dx[0], dx[1], dx[2]);
        let dv = DVec3::new(dx[3], dx[4], dx[5]);
        iterations.push(Iteration {
            rms,
            accepted,
            edited: observations.len() - accepted,
            correction_km: dr.length(),
            correction_km_s: dv.length(),
        });
        // `δxᵀ Λ δx`: the correction in units of its own uncertainty.
        let step_chi2: f64 = (0..n)
            .map(|i| dx[i] * (0..n).map(|j| lambda[i][j] * dx[j]).sum::<f64>())
            .sum();
        let converged = step_chi2 <= settings.tolerance * n as f64
            || prev_rms
                .is_some_and(|prev| (prev - rms).abs() <= settings.tolerance * prev.max(rms));
        if converged || iter + 1 == settings.max_iterations.max(1) {
            let sigmas: Vec<f64> = (0..params.len())
                .map(|k| cov[6 + k][6 + k].max(0.0).sqrt())
                .collect();
            return Ok(BatchSolution {
                state: x,
                covariance: symmetrize(&state_block(&cov)),
                parameters: params
                    .iter()
                    .zip(&values)
                    .zip(sigmas)
                    .map(|((k, v), s)| (*k, *v, s))
                    .collect(),
                full_covariance: cov,
                residuals,
                iterations,
                converged,
            });
        }
        x.position_km += dr;
        x.velocity_km_s += dv;
        for (k, v) in values.iter_mut().enumerate() {
            *v += dx[6 + k];
        }
        prev_rms = Some(rms);
    }
    unreachable!("the last iteration returns")
}

// --- Extended Kalman filter --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterSettings {
    pub light_time: bool,
    /// Innovation gate, σ of `H P Hᵀ + σ²`.
    pub edit_sigma: Option<f64>,
    pub process_noise: Option<ProcessNoise>,
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self {
            light_time: true,
            edit_sigma: Some(5.0),
            process_noise: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterStep {
    /// Index into the observations.
    pub observation: usize,
    /// After this update.
    pub state: SpacecraftState,
    pub covariance: Matrix6,
    /// Innovation (pre-fit), observed − computed.
    pub residual: f64,
    pub post_fit: f64,
    /// `√(H P⁻ Hᵀ + σ²)`.
    pub innovation_sigma: f64,
    pub edited: bool,
}

/// EKF from `initial` with covariance `p0`, one step per observation in epoch order.
pub fn filter(
    propagator: &mut Propagator,
    stations: &[GroundStation],
    observations: &[Observation],
    initial: &SpacecraftState,
    p0: &Matrix6,
    settings: &FilterSettings,
) -> Result<Vec<FilterStep>, OdError> {
    if observations.is_empty() {
        return Err(OdError::NoObservations);
    }
    let sites = station_states(propagator.orientation.as_deref(), stations, observations)?;
    let mut order: Vec<usize> = (0..observations.len()).collect();
    order.sort_by(|&a, &b| {
        observations[a]
            .epoch
            .seconds_since(observations[b].epoch)
            .total_cmp(&0.0)
    });
    let (mut x, mut p) = (*initial, symmetrize(p0));
    let mut out = Vec::with_capacity(observations.len());
    for i in order {
        let o = &observations[i];
        if o.epoch != x.epoch {
            let dt = o.epoch.seconds_since(x.epoch);
            let (next, phi) = propagator.propagate_stm(&x, o.epoch)?;
            p = congruence(&phi, &p);
            if let Some(q) = settings.process_noise {
                p = add(&p, &q.q(dt, next.position_km, next.velocity_km_s));
            }
            x = next;
        }
        let measure = |x: &SpacecraftState| {
            predict(
                o.kind,
                x.position_km,
                x.velocity_km_s,
                sites[i].as_ref(),
                settings.light_time,
            )
            .ok_or(OdError::BadStation(i))
        };
        let (computed, h) = measure(&x)?;
        let y = residual(o.kind, o.value, computed);
        let ph: [f64; 6] = std::array::from_fn(|r| (0..6).map(|c| p[r][c] * h[c]).sum());
        let r = o.sigma * o.sigma;
        let s = (0..6).map(|c| h[c] * ph[c]).sum::<f64>() + r;
        let innovation_sigma = s.max(0.0).sqrt();
        let edited = settings
            .edit_sigma
            .is_some_and(|k| y.abs() > k * innovation_sigma);
        if !edited {
            let k: [f64; 6] = std::array::from_fn(|c| ph[c] / s);
            let a: Matrix6 = std::array::from_fn(|row| {
                std::array::from_fn(|col| f64::from(u8::from(row == col)) - k[row] * h[col])
            });
            let kr: Matrix6 =
                std::array::from_fn(|row| std::array::from_fn(|col| k[row] * r * k[col]));
            p = add(&congruence(&a, &p), &kr);
            x.position_km += DVec3::new(k[0], k[1], k[2]) * y;
            x.velocity_km_s += DVec3::new(k[3], k[4], k[5]) * y;
        }
        let post_fit = residual(o.kind, o.value, measure(&x)?.0);
        out.push(FilterStep {
            observation: i,
            state: x,
            covariance: p,
            residual: y,
            post_fit,
            innovation_sigma,
            edited,
        });
    }
    Ok(out)
}

// --- Simulation ----------------------------------------------------------------------------

/// One simulated measurement type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tracking {
    pub kind: Kind,
    /// Station index; ignored for [`Kind::Position`].
    pub station: Option<usize>,
    /// Noise 1σ, also written to the observation.
    pub sigma: f64,
    /// Constant added to every value.
    pub bias: f64,
}

/// Observations of `trajectory` (GCRF `(r, v)` at an epoch) every `step_s` on
/// `[start, stop]`; station kinds only while the geometric elevation clears the station's
/// mask. Gaussian noise from `seed` (xorshift64*, Box–Muller). Exact light time.
#[allow(clippy::too_many_arguments)]
pub fn simulate(
    trajectory: &mut impl FnMut(Epoch) -> Option<(DVec3, DVec3)>,
    orientation: &dyn BodyOrientation,
    stations: &[GroundStation],
    tracking: &[Tracking],
    start: Epoch,
    stop: Epoch,
    step_s: f64,
    light_time: bool,
    seed: u64,
) -> Vec<Observation> {
    let mut rng = Gaussian::new(seed);
    let span = stop.seconds_since(start);
    let step = step_s.abs().max(1e-3);
    let n = (span / step).floor().max(0.0) as usize;
    let mut out = Vec::new();
    for k in 0..=n {
        let epoch = start.advanced_by_seconds(k as f64 * step);
        let Some((r, v)) = trajectory(epoch) else {
            continue;
        };
        let tf = orientation.transform(epoch);
        let mut sites: Vec<Option<(StationState, bool)>> = vec![None; stations.len()];
        for t in tracking {
            let (value, station) = if !t.kind.needs_station() {
                (model(t.kind, r, r, v, 0.0, None).0, None)
            } else {
                let Some(si) = t.station.filter(|s| *s < stations.len()) else {
                    continue;
                };
                let (st, visible) = *sites[si].get_or_insert_with(|| {
                    let st = station_state(&stations[si], &tf);
                    let rho = r - st.position_km;
                    let az = rho.dot(st.east).atan2(rho.dot(st.north));
                    let el = (rho.dot(st.up) / rho.length()).asin();
                    (
                        st,
                        el >= stations[si].min_elevation_at(az.rem_euclid(std::f64::consts::TAU)),
                    )
                });
                if !visible {
                    continue;
                }
                let (mut rt, mut vt, mut tau) = (r, v, 0.0);
                if light_time {
                    for _ in 0..4 {
                        tau = (rt - st.position_km).length() / SPEED_OF_LIGHT_KM_S;
                        if let Some((a, b)) = trajectory(epoch.advanced_by_seconds(-tau)) {
                            (rt, vt) = (a, b);
                        }
                    }
                }
                let rho = rt - st.position_km;
                (
                    model(t.kind, rt, rho, vt - st.velocity_km_s, tau, Some(&st)).0,
                    Some(si),
                )
            };
            let mut value = value + t.bias + t.sigma * rng.next();
            if t.kind.wraps() {
                value = value.rem_euclid(std::f64::consts::TAU);
            }
            out.push(Observation {
                epoch,
                kind: t.kind,
                station,
                value,
                sigma: t.sigma,
            });
        }
    }
    out
}

/// Standard normal deviates: xorshift64*, Box–Muller.
struct Gaussian {
    state: u64,
    spare: Option<f64>,
}

impl Gaussian {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15 | 1,
            spare: None,
        }
    }

    fn uniform(&mut self) -> f64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let x = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn next(&mut self) -> f64 {
        if let Some(z) = self.spare.take() {
            return z;
        }
        let (u, v) = (self.uniform(), self.uniform());
        let radius = (-2.0 * u.ln()).sqrt();
        let (s, c) = (std::f64::consts::TAU * v).sin_cos();
        self.spare = Some(radius * s);
        radius * c
    }
}

// --- TDM ------------------------------------------------------------------------------------

/// Observations from TDM records. The station is the participant named like a station
/// (`GroundStation::name`). `RANGE` (km) is halved on a round-trip path (first participant =
/// last); `DOPPLER_INSTANTANEOUS` is range rate (km/s); `ANGLE_1`/`ANGLE_2` are AZEL or
/// RADEC (deg). Other keywords are skipped. `sigma(kind)` sets the weight.
pub fn from_tdm(
    tdm: &Tdm,
    stations: &[GroundStation],
    sigma: impl Fn(Kind) -> f64,
) -> Result<Vec<Observation>, String> {
    let mut out = Vec::new();
    for (k, seg) in tdm.segments.iter().enumerate() {
        let m = &seg.metadata;
        let station = m
            .participants
            .iter()
            .find_map(|p| stations.iter().position(|s| s.name == *p));
        let round_trip = m.path.len() > 2 && m.path.first() == m.path.last();
        let angles = m.angle_type.as_deref().map(str::to_ascii_uppercase);
        for r in &seg.records {
            let (kind, value) = match (r.keyword.as_str(), angles.as_deref()) {
                ("RANGE", _) => {
                    match m.range_units.as_deref() {
                        None | Some("km") => {}
                        Some(u) => {
                            return Err(format!("segment {}: RANGE_UNITS {u} unsupported", k + 1));
                        }
                    }
                    (
                        Kind::Range,
                        if round_trip { r.value / 2.0 } else { r.value },
                    )
                }
                ("DOPPLER_INSTANTANEOUS", _) => (Kind::RangeRate, r.value),
                ("ANGLE_1", Some("AZEL")) => (Kind::Azimuth, r.value.to_radians()),
                ("ANGLE_2", Some("AZEL")) => (Kind::Elevation, r.value.to_radians()),
                ("ANGLE_1", Some("RADEC")) => (Kind::RightAscension, r.value.to_radians()),
                ("ANGLE_2", Some("RADEC")) => (Kind::Declination, r.value.to_radians()),
                ("ANGLE_1" | "ANGLE_2", other) => {
                    return Err(format!(
                        "segment {}: ANGLE_TYPE {} unsupported",
                        k + 1,
                        other.unwrap_or("missing")
                    ));
                }
                _ => continue,
            };
            let Some(station) = station else {
                return Err(format!(
                    "segment {}: no participant matches a ground station",
                    k + 1
                ));
            };
            out.push(Observation {
                epoch: r.epoch,
                kind,
                station: Some(station),
                value,
                sigma: sigma(kind),
            });
        }
    }
    Ok(out)
}

/// TDM of station observations: one segment per station and angle type, participant 1 the
/// station, 2 `spacecraft`, one-way `PATH = 2,1`. [`Kind::Position`] is skipped.
pub fn to_tdm(observations: &[Observation], stations: &[GroundStation], spacecraft: &str) -> Tdm {
    let mut segments: Vec<((usize, Option<&str>), TdmSegment)> = Vec::new();
    for o in observations {
        let Some(si) = o.station.filter(|s| *s < stations.len()) else {
            continue;
        };
        let (keyword, angle, value) = match o.kind {
            Kind::Range => ("RANGE", None, o.value),
            Kind::RangeRate => ("DOPPLER_INSTANTANEOUS", None, o.value),
            Kind::Azimuth => ("ANGLE_1", Some("AZEL"), o.value.to_degrees()),
            Kind::Elevation => ("ANGLE_2", Some("AZEL"), o.value.to_degrees()),
            Kind::RightAscension => ("ANGLE_1", Some("RADEC"), o.value.to_degrees()),
            Kind::Declination => ("ANGLE_2", Some("RADEC"), o.value.to_degrees()),
            Kind::Position(_) => continue,
        };
        // Range and Doppler share a segment with any angle type of the same station.
        let at = segments
            .iter()
            .position(|((s, a), _)| *s == si && (angle.is_none() || a.is_none() || *a == angle));
        let at = match at {
            Some(i) => i,
            None => {
                segments.push((
                    (si, angle),
                    TdmSegment {
                        metadata: TdmMetadata {
                            participants: vec![stations[si].name.clone(), spacecraft.into()],
                            path: vec![2, 1],
                            range_units: Some("km".into()),
                            reference_frame: Some("EME2000".into()),
                            ..TdmMetadata::default()
                        },
                        records: Vec::new(),
                    },
                ));
                segments.len() - 1
            }
        };
        let ((_, a), seg) = &mut segments[at];
        if a.is_none() {
            *a = angle;
        }
        seg.records.push(TdmRecord {
            keyword: keyword.into(),
            epoch: o.epoch,
            value,
        });
    }
    Tdm {
        segments: segments
            .into_iter()
            .map(|((_, angle), mut seg)| {
                seg.metadata.angle_type = angle.map(Into::into);
                seg
            })
            .collect(),
        ..Tdm::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bodies;
    use crate::covariance::diagonal;
    use crate::force::{PointMass, SphericalHarmonics};
    use crate::propagate::{EarthOrientation, Integrator, Method};

    fn stations() -> Vec<GroundStation> {
        [
            ("Svalbard", 78.23, 15.39),
            ("Fairbanks", 64.86, -147.85),
            ("Wallops", 37.94, -75.46),
            ("Hartebeesthoek", -25.89, 27.69),
            ("Kiruna", 67.86, 20.96),
        ]
        .into_iter()
        .map(|(name, lat, lon): (&str, f64, f64)| GroundStation {
            name: name.into(),
            latitude_rad: lat.to_radians(),
            longitude_rad: lon.to_radians(),
            altitude_km: 0.1,
            min_elevation_rad: 5f64.to_radians(),
            horizon: Vec::new(),
        })
        .collect()
    }

    /// Circular, 65° inclination, radius `r` km.
    fn circular(r: f64) -> SpacecraftState {
        let v = (bodies::GM_EARTH_KM3_S2 / r).sqrt();
        let i = 65f64.to_radians();
        SpacecraftState::new(
            Epoch::from_utc(2026, 3, 1, 0, 0, 0.0),
            StateVector::new(
                DVec3::new(r, 0.0, 0.0),
                DVec3::new(0.0, v * i.cos(), v * i.sin()),
            ),
            500.0,
        )
    }

    fn truth() -> SpacecraftState {
        circular(7000.0)
    }

    fn propagator() -> Propagator {
        Propagator::new(Integrator::new(Method::Dop853).with_tolerance(1e-12, 1e-12))
            .with_force(SphericalHarmonics::egm2008(4, 4))
            .with_orientation(EarthOrientation::default())
    }

    #[test]
    fn partials_match_differences() {
        let st = station_state(
            &stations()[2],
            &EarthOrientation::default().transform(truth().epoch),
        );
        let x = circular(7000.0);
        // Over Wallops-ish longitude so every angle is well conditioned.
        let (r, v) = (
            DVec3::new(1500.0, -5500.0, 4200.0),
            x.velocity_km_s.normalize() * 7.5,
        );
        for kind in [
            Kind::Range,
            Kind::RangeRate,
            Kind::Azimuth,
            Kind::Elevation,
            Kind::RightAscension,
            Kind::Declination,
            Kind::Position(1),
        ] {
            let (_, h) = predict(kind, r, v, Some(&st), true).unwrap();
            for c in 0..6 {
                let step = if c < 3 { 1e-4 } else { 1e-7 };
                let shift = |s: f64| {
                    let (mut r, mut v) = (r, v);
                    if c < 3 {
                        r[c] += s;
                    } else {
                        v[c - 3] += s;
                    }
                    predict(kind, r, v, Some(&st), true).unwrap().0
                };
                let fd = residual(kind, shift(step), shift(-step)) / (2.0 * step);
                let scale = h.iter().map(|x| x.abs()).fold(1e-12, f64::max);
                // τ held fixed in the analytic partials: ~τ relative.
                assert!(
                    (fd - h[c]).abs() <= 1e-4 * scale,
                    "{kind:?}[{c}]: {} vs {fd}",
                    h[c]
                );
            }
        }
    }

    /// Observations every 60 s of `x0` under `p`.
    fn simulated_with(
        p: &Propagator,
        x0: &SpacecraftState,
        tracking: &[Tracking],
        hours: f64,
        seed: u64,
    ) -> Vec<Observation> {
        let stop = x0.epoch.advanced_by_seconds(hours * 3600.0);
        let mut p = p.clone();
        let eph = p.ephemeris(x0, stop, 60.0).unwrap();
        let mut at = |e: Epoch| {
            let t = e.seconds_since(x0.epoch);
            let i = ((t / 60.0).round().max(0.0) as usize).min(eph.len() - 1);
            let s = if eph[i].epoch == e {
                eph[i]
            } else {
                p.propagate(&eph[i], e).ok()?
            };
            Some((s.position_km, s.velocity_km_s))
        };
        simulate(
            &mut at,
            &EarthOrientation::default(),
            &stations(),
            tracking,
            x0.epoch,
            stop,
            60.0,
            true,
            seed,
        )
    }

    fn simulated(tracking: &[Tracking], hours: f64, seed: u64) -> Vec<Observation> {
        simulated_with(&propagator(), &truth(), tracking, hours, seed)
    }

    fn radar(sigma_km: f64, sigma_rad: f64) -> Vec<Tracking> {
        (0..stations().len())
            .flat_map(|s| {
                [
                    (Kind::Range, sigma_km),
                    (Kind::Azimuth, sigma_rad),
                    (Kind::Elevation, sigma_rad),
                ]
                .map(|(kind, sigma)| Tracking {
                    kind,
                    station: Some(s),
                    sigma,
                    bias: 0.0,
                })
            })
            .collect()
    }

    #[test]
    fn batch_recovers_the_truth_from_a_bad_guess() {
        let obs = simulated(&radar(0.01, 1e-4), 12.0, 7);
        assert!(obs.len() > 100, "{} observations", obs.len());
        let x0 = truth();
        let mut guess = x0;
        guess.position_km += DVec3::new(15.0, -10.0, 5.0);
        guess.velocity_km_s += DVec3::new(0.01, 0.005, -0.008);
        let mut p = propagator();
        let sol = batch(
            &mut p,
            &stations(),
            &obs,
            &guess,
            None,
            &BatchSettings::default(),
        )
        .unwrap();
        assert!(sol.converged, "{:?}", sol.iterations);
        let err = (sol.state.position_km - x0.position_km).length();
        let sigma = (sol.covariance[0][0] + sol.covariance[1][1] + sol.covariance[2][2]).sqrt();
        assert!(err < 0.01, "{err} km");
        assert!(err < 4.0 * sigma, "{err} km vs σ {sigma}");
        let rms = sol.iterations.last().unwrap().rms;
        assert!((0.7..1.3).contains(&rms), "RMS {rms}");
    }

    #[test]
    fn batch_edits_outliers_and_estimates_drag() {
        use crate::force::{Drag, ExponentialAtmosphere};
        let x0 = circular(6728.0);
        let mut p = propagator().with_force(Drag::new(ExponentialAtmosphere));
        p.vehicle.drag_coefficient = 2.2;
        let mut obs = simulated_with(&p, &x0, &radar(0.005, 5e-5), 24.0, 3);
        for o in obs.iter_mut().step_by(37) {
            o.value += 100.0 * o.sigma;
        }
        let planted = obs.len().div_ceil(37);
        let mut guess = x0;
        guess.position_km += DVec3::new(1.0, -1.0, 0.5);
        p.vehicle.drag_coefficient = 1.6;
        let settings = BatchSettings {
            parameters: vec![(Parameter::DragCoefficient, 1.0)],
            ..BatchSettings::default()
        };
        let sol = batch(&mut p, &stations(), &obs, &guess, None, &settings).unwrap();
        assert!(sol.converged, "{:?}", sol.iterations);
        assert_eq!(p.vehicle.drag_coefficient, 1.6, "vehicle restored");
        let (_, cd, sigma_cd) = sol.parameters[0];
        assert!((cd - 2.2).abs() < 4.0 * sigma_cd, "Cd {cd} ± {sigma_cd}");
        assert!(sigma_cd < 0.05, "σ Cd {sigma_cd}");
        let edited = sol.residuals.iter().filter(|r| r.edited).count();
        assert!(
            edited >= planted && edited < planted + obs.len() / 50,
            "{edited} edited, {planted} planted"
        );
        assert!((sol.state.position_km - x0.position_km).length() < 0.01);
    }

    #[test]
    fn ekf_converges_and_covariance_is_consistent() {
        let obs = simulated(&radar(0.01, 1e-4), 12.0, 5);
        let x0 = truth();
        let mut guess = x0;
        guess.position_km += DVec3::new(2.0, -1.0, 1.0);
        guess.velocity_km_s += DVec3::new(0.002, 0.0, -0.001);
        let p0 = diagonal([25.0, 25.0, 25.0, 1e-4, 1e-4, 1e-4]);
        let mut p = propagator();
        let steps = filter(
            &mut p,
            &stations(),
            &obs,
            &guess,
            &p0,
            &FilterSettings::default(),
        )
        .unwrap();
        let last = steps.last().unwrap();
        let truth_then = propagator().propagate(&x0, last.state.epoch).unwrap();
        let err = (last.state.position_km - truth_then.position_km).length();
        let sigma = (last.covariance[0][0] + last.covariance[1][1] + last.covariance[2][2]).sqrt();
        assert!(err < 0.02, "{err} km");
        assert!(err < 4.0 * sigma, "{err} km vs σ {sigma}");
        // Normalised innovations after the first pass ~ N(0, 1).
        let tail = &steps[steps.len() / 2..];
        let nis = tail
            .iter()
            .filter(|s| !s.edited)
            .map(|s| (s.residual / s.innovation_sigma).powi(2))
            .sum::<f64>()
            / tail.len() as f64;
        assert!((0.6..1.5).contains(&nis), "NIS {nis}");
    }

    #[test]
    fn gnss_positions_determine_the_state_without_stations() {
        let tracking: Vec<Tracking> = (0..3)
            .map(|i| Tracking {
                kind: Kind::Position(i),
                station: None,
                sigma: 0.005,
                bias: 0.0,
            })
            .collect();
        // Two-body: no orientation anywhere in the estimate.
        let mut p = Propagator::new(Integrator::new(Method::Dop853).with_tolerance(1e-12, 1e-12))
            .with_force(PointMass {
                gm_km3_s2: bodies::GM_EARTH_KM3_S2,
            });
        let x0 = truth();
        let obs = simulated_with(&p, &x0, &tracking, 1.0, 9);
        assert_eq!(obs.len(), 61 * 3);
        let mut guess = x0;
        guess.velocity_km_s += DVec3::splat(0.003);
        let sol = batch(&mut p, &[], &obs, &guess, None, &BatchSettings::default()).unwrap();
        let e = (sol.state.position_km - x0.position_km).length();
        assert!(sol.converged && e < 0.005, "{e} km {:?}", sol.iterations);
    }

    #[test]
    fn tdm_round_trip_keeps_observations() {
        let obs = simulated(&radar(0.01, 1e-4), 3.0, 1);
        let st = stations();
        let tdm = to_tdm(&obs, &st, "SAT");
        let text = tdm.to_kvn();
        let back = from_tdm(&Tdm::from_kvn(&text).unwrap(), &st, |k| match k {
            Kind::Range => 0.01,
            _ => 1e-4,
        })
        .unwrap();
        assert_eq!(back.len(), obs.len());
        let mut a = obs.clone();
        let mut b = back;
        let key = |o: &Observation| {
            (
                o.station,
                format!("{:?}", o.kind),
                o.epoch.tt_seconds().to_bits(),
            )
        };
        a.sort_by_key(key);
        b.sort_by_key(key);
        for (x, y) in a.iter().zip(&b) {
            assert_eq!((x.kind, x.station), (y.kind, y.station));
            assert!((x.value - y.value).abs() <= 1e-10 * x.value.abs().max(1.0));
        }
        let mut two_way = Tdm::from_kvn(&text).unwrap();
        two_way.segments[0].metadata.path = vec![1, 2, 1];
        let halved = from_tdm(&two_way, &st, |_| 1.0).unwrap();
        let first_range = two_way.segments[0]
            .records
            .iter()
            .find(|r| r.keyword == "RANGE")
            .unwrap();
        assert!(
            halved
                .iter()
                .any(|o| o.kind == Kind::Range && o.value == first_range.value / 2.0)
        );
    }
}
