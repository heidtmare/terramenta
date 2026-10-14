//! Numerical (Cowell) propagation.
//!
//! State `[r, v, m]`: central-body-centred, ICRF axes (GCRF for Earth), km, km/s, kg.
//! Impulsive maneuvers apply on `[start, end)`: a maneuver at the start epoch fires, one at
//! the end epoch does not.
//!
//! State transition matrix `Φ = ∂(r, v)/∂(r₀, v₀)`: variational equations integrated with the
//! state (error control on the state only). `∂a/∂(r, v)` per force from
//! [`ForceModel::partials`], else central differences. Impulsive maneuvers map `Φ` through
//! their own Jacobian. Mass is not a solve-for: `∂m` terms are dropped.

use std::sync::Arc;

use glam::{DMat3, DVec3};

use crate::covariance::Matrix6;
use crate::earth::{EarthModel, TerrestrialTransform, gcrf_to_itrf};
use crate::eop::{Eop, EopTable};
use crate::force::{ForceContext, ForceModel, Vehicle};
use crate::frame::{Ephemeris, StateVector};
use crate::maneuver::ImpulsiveManeuver;
use crate::time::Epoch;

pub mod integrator;

pub use integrator::{IntegrationError, Integrator, Method, Tolerance};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpacecraftState {
    pub epoch: Epoch,
    pub position_km: DVec3,
    pub velocity_km_s: DVec3,
    pub mass_kg: f64,
}

impl SpacecraftState {
    pub fn new(epoch: Epoch, state: StateVector, mass_kg: f64) -> Self {
        Self {
            epoch,
            position_km: state.position_km,
            velocity_km_s: state.velocity_km_s,
            mass_kg,
        }
    }

    pub fn state_vector(&self) -> StateVector {
        StateVector::new(self.position_km, self.velocity_km_s)
    }

    fn to_array(self) -> [f64; 7] {
        let (r, v) = (self.position_km, self.velocity_km_s);
        [r.x, r.y, r.z, v.x, v.y, v.z, self.mass_kg]
    }

    pub(crate) fn from_slice(epoch: Epoch, y: &[f64]) -> Self {
        Self {
            epoch,
            position_km: DVec3::new(y[0], y[1], y[2]),
            velocity_km_s: DVec3::new(y[3], y[4], y[5]),
            mass_kg: y[6],
        }
    }
}

/// Inertial → body-fixed provider.
pub trait BodyOrientation: std::fmt::Debug + Send + Sync {
    fn transform(&self, epoch: Epoch) -> TerrestrialTransform;
}

/// GCRF → ITRF. Without EOP: UT1 = UTC, no polar motion or nutation corrections.
#[derive(Debug, Clone, Default)]
pub struct EarthOrientation {
    pub eop: Option<Arc<EopTable>>,
    pub model: EarthModel,
}

impl EarthOrientation {
    pub fn new(eop: Option<Arc<EopTable>>, model: EarthModel) -> Self {
        Self { eop, model }
    }
}

impl BodyOrientation for EarthOrientation {
    fn transform(&self, epoch: Epoch) -> TerrestrialTransform {
        let eop = self.eop.as_ref().map_or(Eop::default(), |t| t.at(epoch));
        gcrf_to_itrf(epoch, &eop, self.model)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PropagationError {
    Integration(IntegrationError),
    /// A force model needs a body-fixed frame and none is set.
    MissingOrientation,
}

impl std::fmt::Display for PropagationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Integration(e) => write!(f, "{e}"),
            Self::MissingOrientation => write!(f, "force model requires a body orientation"),
        }
    }
}

impl std::error::Error for PropagationError {}

impl From<IntegrationError> for PropagationError {
    fn from(e: IntegrationError) -> Self {
        Self::Integration(e)
    }
}

const ORIENTATION_CACHE: usize = 16;

#[derive(Debug, Clone)]
pub struct Propagator {
    pub forces: Vec<Arc<dyn ForceModel>>,
    pub vehicle: Vehicle,
    pub orientation: Option<Arc<dyn BodyOrientation>>,
    /// Full orientation is recomputed on a grid of this spacing (TT); Earth rotation is
    /// applied exactly in between.
    pub orientation_refresh_s: f64,
    /// Sun relative to the central body; evaluated once per stage into
    /// [`ForceContext::sun_km`].
    pub sun: Option<Arc<dyn Ephemeris>>,
    pub maneuvers: Vec<ImpulsiveManeuver>,
    pub integrator: Integrator,
    /// `(anchor TT s, transform)`, LRU, most recent first. Holds every anchor one RKF78
    /// step (13 stages) can touch.
    cache: [Option<(f64, TerrestrialTransform)>; ORIENTATION_CACHE],
    switch_epoch: Option<Epoch>,
    switch_mass: Option<f64>,
}

impl Propagator {
    pub fn new(integrator: Integrator) -> Self {
        Self {
            forces: Vec::new(),
            vehicle: Vehicle::default(),
            orientation: None,
            orientation_refresh_s: 600.0,
            sun: None,
            maneuvers: Vec::new(),
            integrator,
            cache: [None; ORIENTATION_CACHE],
            switch_epoch: None,
            switch_mass: None,
        }
    }

    pub fn with_force(mut self, force: impl ForceModel + 'static) -> Self {
        self.forces.push(Arc::new(force));
        self
    }

    pub fn with_orientation(mut self, orientation: impl BodyOrientation + 'static) -> Self {
        self.orientation = Some(Arc::new(orientation));
        self.cache = [None; ORIENTATION_CACHE];
        self
    }

    pub fn with_sun(mut self, sun: impl Ephemeris + 'static) -> Self {
        self.sun = Some(Arc::new(sun));
        self
    }

    pub fn with_vehicle(mut self, vehicle: Vehicle) -> Self {
        self.vehicle = vehicle;
        self
    }

    pub fn with_maneuver(mut self, maneuver: ImpulsiveManeuver) -> Self {
        self.maneuvers.push(maneuver);
        self
    }

    /// State at `to`.
    pub fn propagate(
        &mut self,
        state: &SpacecraftState,
        to: Epoch,
    ) -> Result<SpacecraftState, PropagationError> {
        let y = self.run(state, to, None, false, |_, _| {})?;
        Ok(SpacecraftState::from_slice(to, &y))
    }

    /// Calls `on_sample` at `state.epoch`, every `step_s`, and at `to`. At each impulsive
    /// maneuver, also emits the states before and after it (same epoch, in run order).
    pub fn propagate_with(
        &mut self,
        state: &SpacecraftState,
        to: Epoch,
        step_s: f64,
        mut on_sample: impl FnMut(&SpacecraftState),
    ) -> Result<SpacecraftState, PropagationError> {
        let y = self.run(state, to, Some(step_s.abs()), false, |e, y| {
            on_sample(&SpacecraftState::from_slice(e, y))
        })?;
        Ok(SpacecraftState::from_slice(to, &y))
    }

    /// State and `Φ(to, state.epoch)` at `to`.
    pub fn propagate_stm(
        &mut self,
        state: &SpacecraftState,
        to: Epoch,
    ) -> Result<(SpacecraftState, Matrix6), PropagationError> {
        self.propagate_stm_with(state, to, 0.0, |_, _| {})
    }

    /// As [`Self::propagate_with`], with `Φ(t, state.epoch)` at each sample.
    pub fn propagate_stm_with(
        &mut self,
        state: &SpacecraftState,
        to: Epoch,
        step_s: f64,
        mut on_sample: impl FnMut(&SpacecraftState, &Matrix6),
    ) -> Result<(SpacecraftState, Matrix6), PropagationError> {
        let sampling = (step_s != 0.0).then_some(step_s.abs());
        let y = self.run(state, to, sampling, true, |e, y| {
            on_sample(&SpacecraftState::from_slice(e, y), &stm(y))
        })?;
        Ok((SpacecraftState::from_slice(to, &y), stm(&y)))
    }

    /// Samples every `step_s` from `state.epoch` to `to` inclusive.
    pub fn ephemeris(
        &mut self,
        state: &SpacecraftState,
        to: Epoch,
        step_s: f64,
    ) -> Result<Vec<SpacecraftState>, PropagationError> {
        let mut out = Vec::new();
        self.propagate_with(state, to, step_s, |s| out.push(*s))?;
        Ok(out)
    }

    /// `[ṙ, v̇, ṁ]` at a state.
    pub fn derivative(&mut self, state: &SpacecraftState) -> (DVec3, DVec3, f64) {
        let mut d = [0.0; 7];
        let y = state.to_array();
        self.eval(state.epoch, &y, &mut d);
        (
            DVec3::new(d[0], d[1], d[2]),
            DVec3::new(d[3], d[4], d[5]),
            d[6],
        )
    }

    fn body_fixed(&mut self, epoch: Epoch) -> Option<TerrestrialTransform> {
        let orientation = self.orientation.as_ref()?;
        let t = epoch.tt_seconds();
        let step = self.orientation_refresh_s;
        let anchor = if step > 0.0 {
            (t / step).round() * step
        } else {
            t
        };
        let hit = self
            .cache
            .iter()
            .position(|c| c.is_some_and(|(a, _)| a == anchor));
        let base = match hit {
            Some(i) => {
                self.cache[..=i].rotate_right(1);
                self.cache[0].unwrap().1
            }
            None => {
                let tf = orientation.transform(Epoch::from_tt_seconds_j2000(anchor));
                self.cache.rotate_right(1);
                self.cache[0] = Some((anchor, tf));
                tf
            }
        };
        Some(base.advanced_by(t - anchor))
    }

    fn eval(&mut self, epoch: Epoch, y: &[f64], d: &mut [f64]) {
        let bf = if self.forces.iter().any(|f| f.requires_body_fixed()) {
            self.body_fixed(epoch)
        } else {
            None
        };
        let ctx = ForceContext {
            epoch,
            position_km: DVec3::new(y[0], y[1], y[2]),
            velocity_km_s: DVec3::new(y[3], y[4], y[5]),
            mass_kg: y[6],
            vehicle: &self.vehicle,
            body_fixed: bf.as_ref(),
            switch_epoch: self.switch_epoch.unwrap_or(epoch),
            switch_mass_kg: self.switch_mass.unwrap_or(y[6]),
            sun_km: self.sun.as_ref().map(|s| s.position_at(epoch)),
        };
        let (mut a, mut mdot) = (DVec3::ZERO, 0.0);
        for f in &self.forces {
            a += f.acceleration(&ctx);
            mdot += f.mass_rate(&ctx);
        }
        d[..3].copy_from_slice(&y[3..6]);
        d[3..6].copy_from_slice(&a.to_array());
        d[6] = mdot;
        if y.len() > STATE {
            let (g, h) = self.jacobian(&ctx);
            variational(g, h, &y[STATE..], &mut d[STATE..]);
        }
    }

    /// `(∂a/∂r, ∂a/∂v)`: analytic where a force has them, central differences for the rest.
    fn jacobian(&self, ctx: &ForceContext) -> (DMat3, DMat3) {
        let (mut g, mut h) = (DMat3::ZERO, DMat3::ZERO);
        let mut rest = Vec::new();
        for f in &self.forces {
            match f.partials(ctx) {
                Some((dr, dv)) => {
                    g += dr;
                    h += dv;
                }
                None => rest.push(f),
            }
        }
        if rest.is_empty() {
            return (g, h);
        }
        let accel = |c: &ForceContext| rest.iter().map(|f| f.acceleration(c)).sum::<DVec3>();
        let (r, v) = (ctx.position_km, ctx.velocity_km_s);
        let step_r = 1e-5 * r.length().max(1.0);
        let cols = DVec3::AXES.map(|e| {
            let at = |s: f64| ForceContext {
                position_km: r + s * e,
                ..*ctx
            };
            (accel(&at(step_r)) - accel(&at(-step_r))) / (2.0 * step_r)
        });
        g += DMat3::from_cols(cols[0], cols[1], cols[2]);
        if rest.iter().any(|f| f.velocity_dependent()) {
            let step_v = 1e-5 * v.length().max(1e-3);
            let cols = DVec3::AXES.map(|e| {
                let at = |s: f64| ForceContext {
                    velocity_km_s: v + s * e,
                    ..*ctx
                };
                (accel(&at(step_v)) - accel(&at(-step_v))) / (2.0 * step_v)
            });
            h += DMat3::from_cols(cols[0], cols[1], cols[2]);
        }
        (g, h)
    }

    /// Offset in `(t, ts)` where mass reaches dry; ṁ is constant between discontinuities.
    fn depletion(&mut self, start: Epoch, t: f64, y: &[f64], ts: f64) -> Option<f64> {
        let dry = self.vehicle.dry_mass_kg;
        if y[6] <= dry {
            return None;
        }
        let mut d = [0.0; STATE];
        self.eval(start.advanced_by_seconds(t), &y[..STATE], &mut d);
        let td = t + (dry - y[6]) / d[6];
        (d[6] < 0.0 && td > t && td < ts).then_some(td)
    }

    /// Final `y`: `[r, v, m]`, then `Φ` row-major when `with_stm`.
    fn run(
        &mut self,
        state: &SpacecraftState,
        to: Epoch,
        step_s: Option<f64>,
        with_stm: bool,
        mut on_sample: impl FnMut(Epoch, &[f64]),
    ) -> Result<Vec<f64>, PropagationError> {
        if self.orientation.is_none() && self.forces.iter().any(|f| f.requires_body_fixed()) {
            return Err(PropagationError::MissingOrientation);
        }
        let start = state.epoch;
        let span = to.seconds_since(start);
        let dir = if span < 0.0 { -1.0 } else { 1.0 };
        // Offsets from `start`, scaled by `dir` so they ascend.
        let inside = |t: f64| t * dir >= 0.0 && t * dir < span * dir;

        let mut stops: Vec<(f64, Stop)> = vec![(span, Stop::End)];
        for (i, m) in self.maneuvers.iter().enumerate() {
            let t = m.epoch.seconds_since(start);
            // Forward fires on [start, end); backward undoes on (end, start].
            let fires = if dir > 0.0 {
                inside(t)
            } else {
                t <= 0.0 && t > span
            };
            if fires {
                stops.push((t, Stop::Maneuver(i)));
            }
        }
        for f in &self.forces {
            for e in f.discontinuities() {
                let t = e.seconds_since(start);
                if inside(t) && t != 0.0 {
                    stops.push((t, Stop::Discontinuity));
                }
            }
        }
        if let Some(step) = step_s.filter(|s| *s > 0.0) {
            let n = (span.abs() / step).floor() as usize;
            for k in 0..=n {
                let t = dir * k as f64 * step;
                if inside(t) {
                    stops.push((t, Stop::Sample));
                }
            }
        }
        stops.sort_by(|a, b| (a.0 * dir).total_cmp(&(b.0 * dir)).then(a.1.cmp(&b.1)));

        let mut y = state.to_array().to_vec();
        if with_stm {
            y.extend(IDENTITY6.iter().flatten());
        }
        let mut t = 0.0;
        let mut integ = std::mem::replace(&mut self.integrator, Integrator::new(Method::Rkf78));
        let controlled = integ.controlled;
        integ.controlled = with_stm.then_some(STATE).or(controlled);
        integ.reset();
        let sampling = step_s.is_some();
        // Last emitted offset; a burn epoch emits pre- and post-burn states once each.
        let mut emitted: Option<f64> = None;
        let result = (|| {
            let mut i = 0;
            while let Some(&(ts, kind)) = stops.get(i) {
                if ts != t {
                    // Samples before the next other stop are dense outputs of one segment.
                    let j = i + stops[i..]
                        .iter()
                        .position(|s| s.1 != Stop::Sample)
                        .expect("End is last");
                    let te = stops[j].0;
                    let outputs: Vec<f64> = stops[i..j].iter().map(|s| s.0).collect();
                    let mut emit =
                        |tt: f64, yy: &[f64]| on_sample(start.advanced_by_seconds(tt), yy);
                    self.switch_epoch = Some(start.advanced_by_seconds(0.5 * (t + te)));
                    self.switch_mass = Some(y[6]);
                    let mut from = 0;
                    if let Some(td) = self.depletion(start, t, &y, te) {
                        from = outputs.partition_point(|o| (o - td) * dir < 0.0);
                        integ.integrate_dense(
                            &mut |tt, yy, dd| self.eval(start.advanced_by_seconds(tt), yy, dd),
                            t,
                            &mut y,
                            td,
                            &outputs[..from],
                            &mut emit,
                        )?;
                        y[6] = self.vehicle.dry_mass_kg;
                        self.switch_mass = Some(y[6]);
                        integ.reset();
                        t = td;
                    }
                    integ.integrate_dense(
                        &mut |tt, yy, dd| self.eval(start.advanced_by_seconds(tt), yy, dd),
                        t,
                        &mut y,
                        te,
                        &outputs[from..],
                        &mut emit,
                    )?;
                    t = te;
                    i = j;
                    continue;
                }
                let epoch = start.advanced_by_seconds(t);
                match kind {
                    Stop::Maneuver(m) => {
                        if sampling && emitted != Some(t) {
                            on_sample(epoch, &y);
                            emitted = Some(t);
                        }
                        let m = self.maneuvers[m];
                        let (r, v) = (DVec3::new(y[0], y[1], y[2]), DVec3::new(y[3], y[4], y[5]));
                        let (dv, mass) = impulse(&m, dir, r, v, y[6]);
                        if with_stm {
                            maneuver_stm(&m, dir, r, v, y[6], &mut y[STATE..]);
                        }
                        y[3] += dv.x;
                        y[4] += dv.y;
                        y[5] += dv.z;
                        y[6] = mass;
                        integ.reset();
                    }
                    Stop::Discontinuity => integ.reset(),
                    Stop::Sample | Stop::End => {}
                }
                let emit = sampling
                    && match kind {
                        // Post-burn, after the last maneuver at this epoch.
                        Stop::Maneuver(_) => !matches!(
                            stops.get(i + 1),
                            Some(&(tn, Stop::Maneuver(_))) if tn == t
                        ),
                        Stop::Sample | Stop::End => emitted != Some(t),
                        Stop::Discontinuity => false,
                    };
                if emit {
                    on_sample(epoch, &y);
                    emitted = Some(t);
                }
                i += 1;
            }
            Ok(())
        })();
        integ.controlled = controlled;
        self.integrator = integ;
        self.switch_epoch = None;
        self.switch_mass = None;
        result.map(|()| y)
    }
}

/// `[r, v, m]` length.
const STATE: usize = 7;

const IDENTITY6: Matrix6 = {
    let mut m = [[0.0; 6]; 6];
    let mut i = 0;
    while i < 6 {
        m[i][i] = 1.0;
        i += 1;
    }
    m
};

/// `Φ` from `y[STATE..]`.
fn stm(y: &[f64]) -> Matrix6 {
    let mut m = [[0.0; 6]; 6];
    for (i, row) in m.iter_mut().enumerate() {
        row.copy_from_slice(&y[STATE + 6 * i..STATE + 6 * i + 6]);
    }
    m
}

/// `Φ̇ = [[0, I], [G, H]] Φ`, row-major.
fn variational(g: DMat3, h: DMat3, p: &[f64], d: &mut [f64]) {
    for j in 0..6 {
        let pos = DVec3::new(p[j], p[6 + j], p[12 + j]);
        let vel = DVec3::new(p[18 + j], p[24 + j], p[30 + j]);
        let acc = g * pos + h * vel;
        for (i, x) in vel.to_array().into_iter().chain(acc.to_array()).enumerate() {
            d[6 * i + j] = x;
        }
    }
}

/// Signed inertial Δv and mass after: applied forward, undone backward.
fn impulse(m: &ImpulsiveManeuver, dir: f64, r: DVec3, v: DVec3, mass: f64) -> (DVec3, f64) {
    if dir > 0.0 {
        m.apply(r, v, mass)
    } else {
        let (dv, mass) = m.undo(r, v, mass);
        (-dv, mass)
    }
}

/// `Φ ← J Φ`, `J = ∂(r, v + Δv(r, v))/∂(r, v)` by central differences. Inertial Δv: `J = I`.
fn maneuver_stm(m: &ImpulsiveManeuver, dir: f64, r: DVec3, v: DVec3, mass: f64, p: &mut [f64]) {
    if m.frame == crate::maneuver::LocalFrame::Inertial {
        return;
    }
    let dv = |r, v| impulse(m, dir, r, v, mass).0;
    let (step_r, step_v) = (1e-5 * r.length(), 1e-5 * v.length());
    // ∂Δv/∂(r, v), one column per state component.
    let cols: [DVec3; 6] = std::array::from_fn(|k| {
        let e = DVec3::AXES[k % 3];
        if k < 3 {
            (dv(r + step_r * e, v) - dv(r - step_r * e, v)) / (2.0 * step_r)
        } else {
            (dv(r, v + step_v * e) - dv(r, v - step_v * e)) / (2.0 * step_v)
        }
    });
    for j in 0..6 {
        let col: [f64; 6] = std::array::from_fn(|k| p[6 * k + j]);
        for i in 0..3 {
            p[6 * (3 + i) + j] += (0..6).map(|k| cols[k][i] * col[k]).sum::<f64>();
        }
    }
}

/// Ordered so that, at one epoch, maneuvers apply before the sample is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stop {
    Maneuver(usize),
    Discontinuity,
    Sample,
    End,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::force::{PointMass, SphericalHarmonics};
    use crate::maneuver::{FiniteBurn, LocalFrame};
    use crate::orbit::OrbitalElements;

    const GM: f64 = 398_600.441_5;

    fn leo() -> SpacecraftState {
        SpacecraftState::new(
            Epoch::from_utc(2024, 1, 1, 0, 0, 0.0),
            StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 6.0, 4.5)),
            1000.0,
        )
    }

    fn two_body(method: Method) -> Propagator {
        Propagator::new(Integrator::new(method)).with_force(PointMass { gm_km3_s2: GM })
    }

    #[test]
    fn two_body_matches_kepler() {
        let s0 = leo();
        let el = OrbitalElements::from_state(s0.state_vector(), GM, 0.0);
        let dt = 86_400.0;
        let expect = el.state_at(GM, dt);
        for m in [Method::Rkf78, Method::Dop853] {
            let s1 = two_body(m)
                .propagate(&s0, s0.epoch.advanced_by_seconds(dt))
                .unwrap();
            let err = (s1.position_km - expect.position_km).length();
            assert!(err < 1e-4, "{m:?}: {err} km");
        }
        let s1 = two_body(Method::Rk4 { step_s: 10.0 })
            .propagate(&s0, s0.epoch.advanced_by_seconds(dt))
            .unwrap();
        assert!((s1.position_km - expect.position_km).length() < 1e-3);
    }

    #[test]
    fn backward_returns_to_start() {
        let s0 = leo();
        let mut p = two_body(Method::Dop853);
        let s1 = p
            .propagate(&s0, s0.epoch.advanced_by_seconds(20_000.0))
            .unwrap();
        let back = p.propagate(&s1, s0.epoch).unwrap();
        assert!((back.position_km - s0.position_km).length() < 1e-5);
    }

    #[test]
    fn samples_cover_span() {
        let s0 = leo();
        let to = s0.epoch.advanced_by_seconds(1000.0);
        let eph = two_body(Method::Rkf78).ephemeris(&s0, to, 300.0).unwrap();
        let t: Vec<f64> = eph
            .iter()
            .map(|s| s.epoch.seconds_since(s0.epoch))
            .collect();
        assert_eq!(t.len(), 5);
        for (a, b) in t.iter().zip([0.0, 300.0, 600.0, 900.0, 1000.0]) {
            assert!((a - b).abs() < 1e-6, "{t:?}");
        }
    }

    #[test]
    fn dense_samples_match_kepler() {
        let s0 = leo();
        let to = s0.epoch.advanced_by_seconds(20_000.0);
        let el = OrbitalElements::from_state(s0.state_vector(), GM, 0.0);
        let mut p = two_body(Method::Dop853);
        let eph = p.ephemeris(&s0, to, 10.0).unwrap();
        assert_eq!(eph.len(), 2001);
        for s in &eph {
            let expect = el.state_at(GM, s.epoch.seconds_since(s0.epoch));
            assert!((s.position_km - expect.position_km).length() < 1e-5);
            assert!((s.velocity_km_s - expect.velocity_km_s).length() < 1e-8);
        }
        // Samples do not shorten steps.
        let sampled = p.integrator.stats.evaluations;
        p.integrator.stats = Default::default();
        p.propagate(&s0, to).unwrap();
        assert!(sampled < 2 * p.integrator.stats.evaluations, "{sampled}");
    }

    #[test]
    fn impulsive_maneuver_raises_apogee() {
        let s0 = leo();
        let burn = s0.epoch.advanced_by_seconds(100.0);
        let mut p = two_body(Method::Dop853).with_maneuver(
            ImpulsiveManeuver::new(burn, DVec3::new(0.1, 0.0, 0.0), LocalFrame::Vnb)
                .with_isp(300.0),
        );
        let s1 = p
            .propagate(&s0, s0.epoch.advanced_by_seconds(200.0))
            .unwrap();
        let reference = two_body(Method::Dop853)
            .propagate(&s0, s0.epoch.advanced_by_seconds(200.0))
            .unwrap();
        let e = |s: &SpacecraftState| {
            0.5 * s.velocity_km_s.length_squared() - GM / s.position_km.length()
        };
        assert!(e(&s1) > e(&reference));
        assert!(s1.mass_kg < 1000.0);
        // Undo by propagating back across the maneuver.
        let back = p.propagate(&s1, s0.epoch).unwrap();
        assert!((back.position_km - s0.position_km).length() < 1e-6);
        assert!((back.mass_kg - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn samples_bracket_impulsive_maneuver() {
        // Burn off the sample grid and on it: each yields pre and post states once.
        let s0 = leo();
        let to = s0.epoch.advanced_by_seconds(600.0);
        for burn_s in [150.0, 300.0] {
            let burn = s0.epoch.advanced_by_seconds(burn_s);
            let mut p = two_body(Method::Dop853).with_maneuver(ImpulsiveManeuver::new(
                burn,
                DVec3::new(0.1, 0.0, 0.0),
                LocalFrame::Vnb,
            ));
            let eph = p.ephemeris(&s0, to, 100.0).unwrap();
            let at: Vec<_> = eph.iter().filter(|s| s.epoch == burn).collect();
            assert_eq!(at.len(), 2, "burn at {burn_s}");
            assert_eq!(at[0].position_km, at[1].position_km);
            let dv = at[1].velocity_km_s - at[0].velocity_km_s;
            assert!((dv - 0.1 * at[0].velocity_km_s.normalize()).length() < 1e-12);
            assert!(eph.windows(2).all(|w| w[1].epoch >= w[0].epoch));
            assert_eq!(eph.len(), 7 + usize::from(burn_s == 150.0) + 1);
        }
    }

    #[test]
    fn backward_undoes_local_frame_maneuver() {
        // Radial + normal Δv rotates v, so the frame differs pre/post burn.
        let s0 = leo();
        let burn = s0.epoch.advanced_by_seconds(100.0);
        let mut p = two_body(Method::Dop853).with_maneuver(
            ImpulsiveManeuver::new(burn, DVec3::new(0.5, 0.0, 0.3), LocalFrame::Rsw)
                .with_isp(300.0),
        );
        let s1 = p
            .propagate(&s0, s0.epoch.advanced_by_seconds(200.0))
            .unwrap();
        let back = p.propagate(&s1, s0.epoch).unwrap();
        assert!((back.position_km - s0.position_km).length() < 1e-6);
        assert!((back.velocity_km_s - s0.velocity_km_s).length() < 1e-9);
        assert!((back.mass_kg - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn finite_burn_matches_rocket_equation() {
        // Free space: Δv = Isp g0 ln(m0/m1).
        let s0 = SpacecraftState::new(
            Epoch::J2000,
            StateVector::new(DVec3::X * 1e6, DVec3::ZERO),
            1000.0,
        );
        let burn = FiniteBurn {
            start: Epoch::J2000.advanced_by_seconds(10.0),
            duration_s: 300.0,
            thrust_n: 500.0,
            isp_s: 320.0,
            direction: DVec3::Y,
            frame: LocalFrame::Inertial,
        };
        let mut p = Propagator::new(Integrator::new(Method::Dop853)).with_force(burn);
        let s1 = p
            .propagate(&s0, Epoch::J2000.advanced_by_seconds(400.0))
            .unwrap();
        let mdot = 500.0 / (320.0 * crate::force::G0_M_S2);
        assert!(
            (s1.mass_kg - (1000.0 - 300.0 * mdot)).abs() < 1e-9,
            "{} vs {}",
            s1.mass_kg,
            1000.0 - 300.0 * mdot
        );
        let dv = 320.0 * crate::force::G0_M_S2 * (1000.0 / s1.mass_kg).ln() * 1e-3;
        assert!(
            (s1.velocity_km_s.y - dv).abs() < 1e-12,
            "{} vs {dv}",
            s1.velocity_km_s.y
        );
    }

    #[test]
    fn finite_burn_cuts_off_at_dry_mass() {
        let s0 = SpacecraftState::new(
            Epoch::J2000,
            StateVector::new(DVec3::X * 1e6, DVec3::ZERO),
            1000.0,
        );
        let burn = FiniteBurn {
            start: Epoch::J2000.advanced_by_seconds(10.0),
            duration_s: 1000.0,
            thrust_n: 500.0,
            isp_s: 320.0,
            direction: DVec3::Y,
            frame: LocalFrame::Inertial,
        };
        let vehicle = crate::force::Vehicle {
            dry_mass_kg: 900.0,
            ..Default::default()
        };
        let mut p = Propagator::new(Integrator::new(Method::Dop853))
            .with_vehicle(vehicle)
            .with_force(burn);
        let s1 = p
            .propagate(&s0, Epoch::J2000.advanced_by_seconds(2000.0))
            .unwrap();
        assert_eq!(s1.mass_kg, 900.0);
        let dv = 320.0 * crate::force::G0_M_S2 * (1000.0f64 / 900.0).ln() * 1e-3;
        assert!(
            (s1.velocity_km_s.y - dv).abs() < 1e-12,
            "{} vs {dv}",
            s1.velocity_km_s.y
        );
    }

    #[test]
    fn harmonics_need_orientation() {
        let mut p = Propagator::new(Integrator::new(Method::Rkf78))
            .with_force(SphericalHarmonics::egm2008(4, 4));
        let s0 = leo();
        assert_eq!(
            p.propagate(&s0, s0.epoch.advanced_by_seconds(10.0)),
            Err(PropagationError::MissingOrientation)
        );
    }

    #[test]
    fn orientation_cache_matches_direct() {
        let orient = EarthOrientation::default();
        let mut p =
            Propagator::new(Integrator::new(Method::Rkf78)).with_orientation(orient.clone());
        let e = Epoch::from_utc(2024, 5, 1, 3, 7, 11.0);
        let cached = p.body_fixed(e).unwrap().rotation();
        let direct = orient.transform(e).rotation();
        let r = DVec3::new(7000.0, 0.0, 0.0);
        assert!(
            ((cached - direct) * r).length() < 1e-5,
            "{} km",
            ((cached - direct) * r).length()
        );
    }

    #[test]
    fn orientation_built_once_per_anchor() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Debug)]
        struct Counted(AtomicUsize);
        impl BodyOrientation for Arc<Counted> {
            fn transform(&self, epoch: Epoch) -> TerrestrialTransform {
                self.0.fetch_add(1, Ordering::Relaxed);
                EarthOrientation::default().transform(epoch)
            }
        }

        let calls = Arc::new(Counted(AtomicUsize::new(0)));
        let s0 = leo();
        let to = s0.epoch.advanced_by_seconds(86_400.0);
        let mut p = two_body(Method::Dop853).with_orientation(calls.clone());
        p.propagate(&s0, to).unwrap();
        assert_eq!(calls.0.load(Ordering::Relaxed), 0, "no force needs it");

        let mut p = p.with_force(SphericalHarmonics::egm2008(4, 4));
        p.propagate(&s0, to).unwrap();
        let anchors = (86_400.0 / p.orientation_refresh_s) as usize + 2;
        let n = calls.0.load(Ordering::Relaxed);
        assert!(n <= anchors, "{n} > {anchors}");
    }

    #[test]
    fn orientation_cache_survives_long_steps() {
        use std::sync::Mutex;

        #[derive(Debug)]
        struct Logged(Mutex<Vec<f64>>);
        impl BodyOrientation for Arc<Logged> {
            fn transform(&self, epoch: Epoch) -> TerrestrialTransform {
                self.0.lock().unwrap().push(epoch.tt_seconds());
                EarthOrientation::default().transform(epoch)
            }
        }

        // GEO RKF78 steps span many 60 s anchors; stage order revisits them.
        let log = Arc::new(Logged(Mutex::new(Vec::new())));
        let s0 = SpacecraftState::new(
            Epoch::from_utc(2024, 1, 1, 0, 0, 0.0),
            StateVector::new(DVec3::new(42_164.0, 0.0, 0.0), DVec3::new(0.0, 3.0747, 0.0)),
            1000.0,
        );
        let mut p = Propagator::new(Integrator::new(Method::Rkf78))
            .with_force(SphericalHarmonics::egm2008(4, 4))
            .with_orientation(log.clone());
        p.orientation_refresh_s = 60.0;
        p.propagate(&s0, s0.epoch.advanced_by_seconds(86_400.0))
            .unwrap();
        let mut a = log.0.lock().unwrap().clone();
        let n = a.len();
        a.sort_by(f64::total_cmp);
        a.dedup();
        assert!(n == a.len(), "{n} builds, {} anchors", a.len());
    }

    #[test]
    fn shared_sun_evaluated_once_per_stage() {
        use crate::force::{Drag, HarrisPriester, SolarRadiationPressure, ThirdBody};
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Debug, Clone)]
        struct CountedSun(Arc<AtomicUsize>);
        impl Ephemeris for CountedSun {
            fn state_at(&self, epoch: Epoch) -> StateVector {
                self.0.fetch_add(1, Ordering::Relaxed);
                let w = epoch.seconds_since(Epoch::J2000) * 2e-7;
                StateVector::new(1.496e8 * DVec3::new(w.cos(), w.sin(), 0.1), DVec3::ZERO)
            }
        }

        let run = |shared: bool| {
            let (own, prop) = (Arc::<AtomicUsize>::default(), Arc::<AtomicUsize>::default());
            let sun = CountedSun(own.clone());
            let mut p = two_body(Method::Dop853)
                .with_orientation(EarthOrientation::default())
                .with_force(ThirdBody::sun(1.327e11, sun.clone()))
                .with_force(SolarRadiationPressure::new(sun.clone(), 6378.0))
                .with_force(Drag::new(HarrisPriester::new(sun)));
            if shared {
                p = p.with_sun(CountedSun(prop.clone()));
            }
            let s0 = leo();
            let s1 = p
                .propagate(&s0, s0.epoch.advanced_by_seconds(3600.0))
                .unwrap();
            (
                s1,
                own.load(Ordering::Relaxed),
                prop.load(Ordering::Relaxed),
            )
        };
        let (base, own0, _) = run(false);
        let (s1, own1, prop1) = run(true);
        assert_eq!(s1, base);
        assert_eq!(own1, 0);
        assert!(own0 >= 3 * prop1, "{own0} vs {prop1}");
    }

    /// `Φ` column `k` by central differences of whole propagations.
    fn stm_by_differences(p: &mut Propagator, s0: &SpacecraftState, to: Epoch) -> Matrix6 {
        let mut phi = [[0.0; 6]; 6];
        for k in 0..6 {
            let step = if k < 3 { 1e-3 } else { 1e-6 };
            let shifted = |sign: f64| {
                let mut y = s0.to_array();
                y[k] += sign * step;
                SpacecraftState::from_slice(s0.epoch, &y)
            };
            let plus = p.propagate(&shifted(1.0), to).unwrap().to_array();
            let minus = p.propagate(&shifted(-1.0), to).unwrap().to_array();
            for i in 0..6 {
                phi[i][k] = (plus[i] - minus[i]) / (2.0 * step);
            }
        }
        phi
    }

    fn assert_stm_close(a: &Matrix6, b: &Matrix6, tol: f64) {
        for i in 0..6 {
            for j in 0..6 {
                // Scale by the block's magnitude: position/velocity rows and columns differ.
                let scale = (0..6).map(|k| b[i][k].abs()).fold(1e-9, f64::max);
                assert!(
                    (a[i][j] - b[i][j]).abs() <= tol * scale,
                    "Φ[{i}][{j}]: {} vs {}",
                    a[i][j],
                    b[i][j]
                );
            }
        }
    }

    #[test]
    fn stm_matches_differences_with_harmonics_and_drag() {
        use crate::force::{Drag, ExponentialAtmosphere};
        let s0 = SpacecraftState::new(
            Epoch::from_utc(2024, 1, 1, 0, 0, 0.0),
            StateVector::new(DVec3::new(6778.0, 0.0, 0.0), DVec3::new(0.0, 5.9, 4.9)),
            1000.0,
        );
        let to = s0.epoch.advanced_by_seconds(6000.0);
        let mut p = Propagator::new(Integrator::new(Method::Dop853))
            .with_force(SphericalHarmonics::egm2008(8, 8))
            .with_force(Drag::new(ExponentialAtmosphere))
            .with_orientation(EarthOrientation::default());
        let (s1, phi) = p.propagate_stm(&s0, to).unwrap();
        assert_eq!(
            s1,
            p.propagate(&s0, to).unwrap(),
            "Φ does not perturb the state"
        );
        assert_stm_close(&phi, &stm_by_differences(&mut p, &s0, to), 1e-5);
    }

    #[test]
    fn stm_maps_through_local_frame_burns_both_ways() {
        let s0 = leo();
        let to = s0.epoch.advanced_by_seconds(3000.0);
        let mut p = two_body(Method::Dop853).with_maneuver(ImpulsiveManeuver::new(
            s0.epoch.advanced_by_seconds(1000.0),
            DVec3::new(0.2, 0.05, -0.1),
            LocalFrame::Vnb,
        ));
        let (s1, phi) = p.propagate_stm(&s0, to).unwrap();
        assert_stm_close(&phi, &stm_by_differences(&mut p, &s0, to), 1e-5);
        let (_, back) = p.propagate_stm(&s1, s0.epoch).unwrap();
        let round = crate::covariance::mul(&back, &phi);
        assert_stm_close(&round, &crate::covariance::identity(), 1e-6);
    }

    #[test]
    fn sampled_stm_ends_at_the_final_one() {
        let s0 = leo();
        let to = s0.epoch.advanced_by_seconds(5000.0);
        let mut p = two_body(Method::Dop853);
        let mut samples = Vec::new();
        let (_, last) = p
            .propagate_stm_with(&s0, to, 1000.0, |s, phi| samples.push((s.epoch, *phi)))
            .unwrap();
        assert_eq!(samples.len(), 6);
        assert_eq!(samples[0].1, crate::covariance::identity());
        assert_stm_close(&samples[5].1, &last, 1e-12);
        let (_, mid) = p
            .propagate_stm(&s0, s0.epoch.advanced_by_seconds(2000.0))
            .unwrap();
        assert_stm_close(&samples[2].1, &mid, 1e-8);
    }
}
