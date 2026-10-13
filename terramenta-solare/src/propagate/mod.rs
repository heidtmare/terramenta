//! Numerical (Cowell) propagation.
//!
//! State `[r, v, m]`: central-body-centred, ICRF axes (GCRF for Earth), km, km/s, kg.
//! Impulsive maneuvers apply on `[start, end)`: a maneuver at the start epoch fires, one at
//! the end epoch does not.

use std::sync::Arc;

use glam::DVec3;

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

    fn from_slice(epoch: Epoch, y: &[f64]) -> Self {
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
    cache: Option<(f64, TerrestrialTransform)>,
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
            cache: None,
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
        self.cache = None;
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
        self.run(state, to, None, |_| {})
    }

    /// Calls `on_sample` at `state.epoch`, every `step_s`, and at `to`. At each impulsive
    /// maneuver, also emits the states before and after it (same epoch, in run order).
    pub fn propagate_with(
        &mut self,
        state: &SpacecraftState,
        to: Epoch,
        step_s: f64,
        on_sample: impl FnMut(&SpacecraftState),
    ) -> Result<SpacecraftState, PropagationError> {
        self.run(state, to, Some(step_s.abs()), on_sample)
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
        let base = match self.cache {
            Some((a, tf)) if a == anchor => tf,
            _ => {
                let tf = orientation.transform(Epoch::from_tt_seconds_j2000(anchor));
                self.cache = Some((anchor, tf));
                tf
            }
        };
        Some(base.advanced_by(t - anchor))
    }

    fn eval(&mut self, epoch: Epoch, y: &[f64], d: &mut [f64]) {
        let bf = self.body_fixed(epoch);
        let ctx = ForceContext {
            epoch,
            position_km: DVec3::new(y[0], y[1], y[2]),
            velocity_km_s: DVec3::new(y[3], y[4], y[5]),
            mass_kg: y[6],
            vehicle: &self.vehicle,
            body_fixed: bf.as_ref(),
            switch_epoch: self.switch_epoch.unwrap_or(epoch),
            switch_mass_kg: self.switch_mass.unwrap_or(y[6]),
            sun_km: self.sun.as_ref().map(|s| s.state_at(epoch).position_km),
        };
        let (mut a, mut mdot) = (DVec3::ZERO, 0.0);
        for f in &self.forces {
            a += f.acceleration(&ctx);
            mdot += f.mass_rate(&ctx);
        }
        d[..3].copy_from_slice(&y[3..6]);
        d[3..6].copy_from_slice(&a.to_array());
        d[6] = mdot;
    }

    /// Offset in `(t, ts)` where mass reaches dry; ṁ is constant between discontinuities.
    fn depletion(&mut self, start: Epoch, t: f64, y: &[f64], ts: f64) -> Option<f64> {
        let dry = self.vehicle.dry_mass_kg;
        if y[6] <= dry {
            return None;
        }
        let mut d = [0.0; 7];
        self.eval(start.advanced_by_seconds(t), y, &mut d);
        let td = t + (dry - y[6]) / d[6];
        (d[6] < 0.0 && td > t && td < ts).then_some(td)
    }

    fn run(
        &mut self,
        state: &SpacecraftState,
        to: Epoch,
        step_s: Option<f64>,
        mut on_sample: impl FnMut(&SpacecraftState),
    ) -> Result<SpacecraftState, PropagationError> {
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

        let mut y = state.to_array();
        let mut t = 0.0;
        let mut integ = std::mem::replace(&mut self.integrator, Integrator::new(Method::Rkf78));
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
                    let mut emit = |tt: f64, yy: &[f64]| {
                        on_sample(&SpacecraftState::from_slice(
                            start.advanced_by_seconds(tt),
                            yy,
                        ));
                    };
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
                            on_sample(&SpacecraftState::from_slice(epoch, &y));
                            emitted = Some(t);
                        }
                        let m = self.maneuvers[m];
                        let (r, v) = (DVec3::new(y[0], y[1], y[2]), DVec3::new(y[3], y[4], y[5]));
                        let (dv, mass) = if dir > 0.0 {
                            m.apply(r, v, y[6])
                        } else {
                            let (dv, mass) = m.undo(r, v, y[6]);
                            (-dv, mass)
                        };
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
                    on_sample(&SpacecraftState::from_slice(epoch, &y));
                    emitted = Some(t);
                }
                i += 1;
            }
            Ok(SpacecraftState::from_slice(to, &y))
        })();
        self.integrator = integ;
        self.switch_epoch = None;
        self.switch_mass = None;
        result
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
}
