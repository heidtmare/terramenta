//! Spacecraft: state, physical properties, and the model that advances the state.
//!
//! [`Propagation`] puts two-body, SGP4, numerical (Cowell) and tabulated motion behind one
//! stepping interface. States are central-body-centred, ICRF axes, km, km/s, kg.
//! Sphere-of-influence hand-off across a [`crate::FrameTree`]: [`PatchedConicSpacecraft`].

pub mod patched;

use std::sync::Arc;

pub use patched::{PatchedConicSpacecraft, Primary};

use crate::earth::TemeGrid;
use crate::force::Vehicle;
use crate::frame::{Ephemeris, StateVector};
use crate::interpolate::{Interpolation, SampledEphemeris};
use crate::maneuver::ImpulsiveManeuver;
use crate::orbit::OrbitalElements;
use crate::propagate::{PropagationError, Propagator, SpacecraftState};
use crate::time::Epoch;
use crate::tle::Sgp4Ephemeris;

#[derive(Debug, Clone)]
pub enum Propagation {
    /// Keplerian about a point mass.
    TwoBody { gm_km3_s2: f64 },
    /// GCRF output of [`Sgp4Ephemeris`]. Mass constant; not maneuverable.
    Sgp4(Box<Sgp4Ephemeris>),
    /// Cowell; [`Spacecraft::vehicle`] overrides [`Propagator::vehicle`].
    Numerical(Box<Propagator>),
    /// Precomputed (OEM, SPK, …). Mass constant; not maneuverable.
    Tabulated(Arc<dyn Ephemeris>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpacecraftError {
    Propagation(PropagationError),
    /// Impulsive maneuver on SGP4 or tabulated motion.
    NotManeuverable,
    /// Maneuver epoch before the current state.
    ManeuverInPast,
    /// SGP4 failed (decay, divergence).
    Sgp4 {
        epoch: Epoch,
    },
    /// Fewer than two samples, or non-positive step.
    Span,
}

impl std::fmt::Display for SpacecraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Propagation(e) => write!(f, "{e}"),
            Self::NotManeuverable => f.write_str("propagation does not accept maneuvers"),
            Self::ManeuverInPast => f.write_str("maneuver precedes current state"),
            Self::Sgp4 { epoch } => write!(f, "SGP4 failed at {epoch}"),
            Self::Span => f.write_str("ephemeris span yields fewer than two samples"),
        }
    }
}

impl std::error::Error for SpacecraftError {}

impl From<PropagationError> for SpacecraftError {
    fn from(e: PropagationError) -> Self {
        Self::Propagation(e)
    }
}

#[derive(Debug, Clone)]
pub struct Spacecraft {
    pub name: String,
    pub vehicle: Vehicle,
    state: SpacecraftState,
    propagation: Propagation,
}

impl Spacecraft {
    pub fn new(name: impl Into<String>, state: SpacecraftState, propagation: Propagation) -> Self {
        Self {
            name: name.into(),
            vehicle: Vehicle::default(),
            state,
            propagation,
        }
    }

    /// State at the element epoch.
    pub fn from_sgp4(
        name: impl Into<String>,
        sgp4: Sgp4Ephemeris,
        mass_kg: f64,
    ) -> Result<Self, SpacecraftError> {
        let epoch = sgp4.epoch();
        let sv = sgp4
            .state_gcrf(epoch, &sgp4.eop)
            .ok_or(SpacecraftError::Sgp4 { epoch })?;
        Ok(Self::new(
            name,
            SpacecraftState::new(epoch, sv, mass_kg),
            Propagation::Sgp4(Box::new(sgp4)),
        ))
    }

    pub fn with_vehicle(mut self, vehicle: Vehicle) -> Self {
        self.vehicle = vehicle;
        self
    }

    pub fn state(&self) -> &SpacecraftState {
        &self.state
    }

    pub fn epoch(&self) -> Epoch {
        self.state.epoch
    }

    pub fn propagation(&self) -> &Propagation {
        &self.propagation
    }

    /// Continues from the current state under `propagation` (e.g. SGP4 → numerical).
    pub fn set_propagation(&mut self, propagation: Propagation) {
        self.propagation = propagation;
    }

    /// Replaces the state; propagation is unchanged.
    pub fn set_state(&mut self, state: SpacecraftState) {
        self.state = state;
    }

    /// Advances (or retreats) to `epoch`.
    pub fn step_to(&mut self, epoch: Epoch) -> Result<&SpacecraftState, SpacecraftError> {
        self.state = advance(&self.state, self.vehicle, &mut self.propagation, epoch)?;
        Ok(&self.state)
    }

    pub fn step_by(&mut self, dt_s: f64) -> Result<&SpacecraftState, SpacecraftError> {
        self.step_to(self.state.epoch.advanced_by_seconds(dt_s))
    }

    /// Steps to the maneuver epoch, then applies it.
    pub fn maneuver(&mut self, m: &ImpulsiveManeuver) -> Result<&SpacecraftState, SpacecraftError> {
        if matches!(
            self.propagation,
            Propagation::Sgp4(_) | Propagation::Tabulated(_)
        ) {
            return Err(SpacecraftError::NotManeuverable);
        }
        if m.epoch < self.state.epoch {
            return Err(SpacecraftError::ManeuverInPast);
        }
        self.step_to(m.epoch)?;
        let s = &mut self.state;
        let (dv, mass) = m.apply(s.position_km, s.velocity_km_s, s.mass_kg);
        s.velocity_km_s += dv;
        s.mass_kg = mass;
        Ok(&self.state)
    }

    /// Samples every `step_s` from the current epoch to `to` inclusive; state unchanged.
    pub fn ephemeris(
        &self,
        to: Epoch,
        step_s: f64,
        method: Interpolation,
    ) -> Result<SampledEphemeris, SpacecraftError> {
        let samples = self.samples(to, step_s)?;
        if samples.len() < 2 {
            return Err(SpacecraftError::Span);
        }
        Ok(SampledEphemeris::new(
            samples
                .into_iter()
                .map(|s| (s.epoch, s.state_vector()))
                .collect(),
            method,
        ))
    }

    /// As [`Self::ephemeris`], with mass.
    pub fn samples(&self, to: Epoch, step_s: f64) -> Result<Vec<SpacecraftState>, SpacecraftError> {
        let step = step_s.abs();
        if step.is_nan() || step == 0.0 {
            return Err(SpacecraftError::Span);
        }
        if let Propagation::Numerical(p) = &self.propagation {
            let mut p = p.clone();
            p.vehicle = self.vehicle;
            return Ok(p.ephemeris(&self.state, to, step)?);
        }
        let span = to.seconds_since(self.state.epoch);
        let n = (span.abs() / step).floor() as usize;
        let mut epochs: Vec<Epoch> = (0..=n)
            .map(|k| {
                self.state
                    .epoch
                    .advanced_by_seconds(span.signum() * step * k as f64)
            })
            .collect();
        if epochs
            .last()
            .is_some_and(|e| e.seconds_since(to).abs() > 1e-9)
        {
            epochs.push(to);
        }
        let mut path = match &self.propagation {
            Propagation::TwoBody { gm_km3_s2 } => SamplePath::TwoBody(
                OrbitalElements::from_state(self.state.state_vector(), *gm_km3_s2, 0.0),
                *gm_km3_s2,
            ),
            Propagation::Sgp4(e) => SamplePath::Sgp4(e, TemeGrid::new(|_| e.eop)),
            _ => SamplePath::Exact,
        };
        epochs
            .into_iter()
            .map(|e| {
                let sv = match &mut path {
                    SamplePath::TwoBody(el, gm) => {
                        el.state_at(*gm, e.seconds_since(self.state.epoch))
                    }
                    SamplePath::Sgp4(sgp4, grid) => sgp4
                        .state_rotated(e, grid.at(e))
                        .ok_or(SpacecraftError::Sgp4 { epoch: e })?,
                    SamplePath::Exact => state_vector_at(&self.state, &self.propagation, e)?,
                };
                Ok(SpacecraftState::new(e, sv, self.state.mass_kg))
            })
            .collect()
    }
}

/// Per-sample evaluation in [`Spacecraft::samples`], with setup hoisted out of the loop.
enum SamplePath<'a, F> {
    TwoBody(OrbitalElements, f64),
    Sgp4(&'a Sgp4Ephemeris, TemeGrid<F>),
    Exact,
}

/// `s` carried to `epoch`.
fn advance(
    s: &SpacecraftState,
    vehicle: Vehicle,
    propagation: &mut Propagation,
    epoch: Epoch,
) -> Result<SpacecraftState, SpacecraftError> {
    if let Propagation::Numerical(p) = propagation {
        p.vehicle = vehicle;
        return Ok(p.propagate(s, epoch)?);
    }
    let sv = state_vector_at(s, propagation, epoch)?;
    Ok(SpacecraftState::new(epoch, sv, s.mass_kg))
}

/// Position/velocity at `epoch` for stateless propagation; mass is unchanged.
fn state_vector_at(
    s: &SpacecraftState,
    propagation: &Propagation,
    epoch: Epoch,
) -> Result<StateVector, SpacecraftError> {
    Ok(match propagation {
        Propagation::TwoBody { gm_km3_s2 } => {
            OrbitalElements::from_state(s.state_vector(), *gm_km3_s2, 0.0)
                .state_at(*gm_km3_s2, epoch.seconds_since(s.epoch))
        }
        Propagation::Sgp4(e) => e
            .state_gcrf(epoch, &e.eop)
            .ok_or(SpacecraftError::Sgp4 { epoch })?,
        Propagation::Tabulated(e) => e.state_at(epoch),
        Propagation::Numerical(_) => unreachable!("numerical propagation is stateful"),
    })
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;
    use crate::bodies::GM_EARTH_KM3_S2;
    use crate::force::PointMass;
    use crate::maneuver::LocalFrame;
    use crate::propagate::{Integrator, Method};

    fn leo() -> SpacecraftState {
        SpacecraftState::new(
            Epoch::J2000,
            StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 6.0, 4.5)),
            500.0,
        )
    }

    fn two_body() -> Propagation {
        Propagation::TwoBody {
            gm_km3_s2: GM_EARTH_KM3_S2,
        }
    }

    fn numerical() -> Propagation {
        Propagation::Numerical(Box::new(
            Propagator::new(Integrator::new(Method::Dop853)).with_force(PointMass {
                gm_km3_s2: GM_EARTH_KM3_S2,
            }),
        ))
    }

    #[test]
    fn two_body_and_numerical_agree() {
        let mut a = Spacecraft::new("a", leo(), two_body());
        let mut b = Spacecraft::new("b", leo(), numerical());
        let m = ImpulsiveManeuver::new(
            Epoch::J2000.advanced_by_seconds(1800.0),
            DVec3::new(0.05, 0.0, 0.0),
            LocalFrame::Vnb,
        )
        .with_isp(300.0);
        for sc in [&mut a, &mut b] {
            sc.step_by(600.0).unwrap();
            sc.maneuver(&m).unwrap();
            sc.step_to(Epoch::J2000.advanced_by_seconds(7200.0))
                .unwrap();
        }
        let d = (a.state().position_km - b.state().position_km).length();
        assert!(d < 1e-6, "{d} km");
        assert!((a.state().mass_kg - b.state().mass_kg).abs() < 1e-12);
        assert!(a.state().mass_kg < 500.0);
        assert_eq!(a.maneuver(&m), Err(SpacecraftError::ManeuverInPast));
    }

    #[test]
    fn ephemeris_matches_stepping() {
        let sc = Spacecraft::new("n", leo(), numerical());
        let to = Epoch::J2000.advanced_by_seconds(3000.0);
        let eph = sc.ephemeris(to, 60.0, Interpolation::default()).unwrap();
        assert_eq!(sc.epoch(), Epoch::J2000);
        let mut stepped = sc.clone();
        let t = Epoch::J2000.advanced_by_seconds(1234.5);
        stepped.step_to(t).unwrap();
        let d = (eph.state_at(t).position_km - stepped.state().position_km).length();
        assert!(d < 1e-6, "{d} km");
        // Off-grid end sample lands on `to`; backward spans work.
        let tb = Spacecraft::new("t", leo(), two_body());
        let s = tb
            .samples(Epoch::J2000.advanced_by_seconds(-130.0), 60.0)
            .unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(s[3].epoch.seconds_since(Epoch::J2000), -130.0);
    }

    #[test]
    fn sgp4_samples_match_exact_rotation() {
        let sgp4 = Sgp4Ephemeris::from_tle(
            "1 00005U 58002B   00179.78495062  .00000023  00000-0  28098-4 0  4753",
            "2 00005  34.2682 348.7242 1859667 331.7664  19.3264 10.82419157413667",
        )
        .unwrap();
        let sc = Spacecraft::from_sgp4("vanguard", sgp4, 1.5).unwrap();
        let Propagation::Sgp4(e) = &sc.propagation else {
            unreachable!()
        };
        let samples = sc
            .samples(sc.epoch().advanced_by_seconds(2.0 * 86_400.0), 997.0)
            .unwrap();
        assert_eq!(samples.len(), 175);
        for s in &samples {
            let exact = e.state_gcrf(s.epoch, &e.eop).unwrap();
            assert!((s.position_km - exact.position_km).length() < 1e-5);
            assert!((s.velocity_km_s - exact.velocity_km_s).length() < 1e-8);
        }
    }

    #[test]
    fn sgp4_hands_off_to_numerical() {
        let sgp4 = Sgp4Ephemeris::from_tle(
            "1 00005U 58002B   00179.78495062  .00000023  00000-0  28098-4 0  4753",
            "2 00005  34.2682 348.7242 1859667 331.7664  19.3264 10.82419157413667",
        )
        .unwrap();
        let mut sc = Spacecraft::from_sgp4("vanguard", sgp4, 1.5).unwrap();
        let burn = ImpulsiveManeuver::new(sc.epoch(), DVec3::X * 0.01, LocalFrame::Vnb);
        assert_eq!(sc.maneuver(&burn), Err(SpacecraftError::NotManeuverable));
        sc.step_by(600.0).unwrap();
        let v0 = sc.state().velocity_km_s.length();
        sc.set_propagation(numerical());
        sc.maneuver(&ImpulsiveManeuver {
            epoch: sc.epoch(),
            ..burn
        })
        .unwrap();
        assert!((sc.state().velocity_km_s.length() - v0 - 0.01).abs() < 1e-9);
        sc.step_by(60.0).unwrap();
    }
}
