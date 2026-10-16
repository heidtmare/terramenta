//! Monte Carlo dispersion of a mission sequence.
//!
//! Per trial: initial `[r, v]` drawn from `N(x₀, P₀)` (Cholesky), mass from `N(m₀, σ²)`;
//! each maneuver and finite burn flown with execution errors ([`sequence::run_with`]):
//! magnitude / thrust / Isp scaled by `1 + N(0, σ²)`, direction rotated by
//! `N(0, σ²)` about each of two axes normal to it.
//!
//! [`Guidance::Open`] flies the nominal target and optimize solutions ([`sequence::freeze`]);
//! [`Guidance::Closed`] re-solves each target and optimize from the trial's true state (perfect
//! navigation). Trials are independent: trial `k` draws from [`Rng::trial`]`(seed, k)`.

use glam::DVec3;

use crate::covariance::{self, Matrix6};
use crate::propagate::{Propagator, SpacecraftState};
use crate::sequence::{self, Action, Command, Run, SequenceError};

/// 1σ dispersions. Zero disables.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Dispersions {
    /// Initial `[r, v]` covariance, inertial, km, km/s.
    pub state: Option<Matrix6>,
    pub mass_kg: f64,
    /// Impulsive Δv magnitude, fraction.
    pub maneuver_magnitude: f64,
    /// Impulsive Δv direction, rad per transverse axis.
    pub maneuver_pointing_rad: f64,
    /// Fraction.
    pub thrust: f64,
    /// Fraction.
    pub isp: f64,
    /// Finite burn direction, rad per transverse axis.
    pub burn_pointing_rad: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Guidance {
    /// Nominal target and optimize solutions.
    #[default]
    Open,
    /// Targets and optimizes re-solved per trial.
    Closed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Trial {
    /// Dispersed start.
    pub initial: SpacecraftState,
    pub outcome: Result<Run, SequenceError>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonteCarlo {
    pub nominal: Run,
    pub trials: Vec<Trial>,
}

/// Runs the nominal, then `samples` dispersed trials. `Err` only if the nominal fails.
pub fn run(
    propagator: &Propagator,
    state: SpacecraftState,
    commands: &[Command],
    dispersions: &Dispersions,
    guidance: Guidance,
    samples: usize,
    seed: u64,
) -> Result<MonteCarlo, SequenceError> {
    let nominal = sequence::run(propagator, state, commands)?;
    let frozen;
    let flown = match guidance {
        Guidance::Open => {
            frozen = sequence::freeze(commands, &nominal);
            &frozen[..]
        }
        Guidance::Closed => commands,
    };
    let l = dispersions.state.as_ref().map(covariance::cholesky);
    let trials = (0..samples)
        .map(|k| {
            let mut rng = Rng::trial(seed, k as u64);
            let mut initial = state;
            if let Some(l) = &l {
                let z: [f64; 6] = std::array::from_fn(|_| rng.normal());
                let dx: [f64; 6] = std::array::from_fn(|i| (0..=i).map(|j| l[i][j] * z[j]).sum());
                initial.position_km += DVec3::new(dx[0], dx[1], dx[2]);
                initial.velocity_km_s += DVec3::new(dx[3], dx[4], dx[5]);
            }
            if dispersions.mass_kg > 0.0 {
                initial.mass_kg =
                    (state.mass_kg + dispersions.mass_kg * rng.normal()).max(1e-3 * state.mass_kg);
            }
            let outcome = sequence::run_with(propagator, initial, flown, &mut |a| {
                disperse(a, dispersions, &mut rng)
            });
            Trial { initial, outcome }
        })
        .collect();
    Ok(MonteCarlo { nominal, trials })
}

fn disperse(action: &mut Action, d: &Dispersions, rng: &mut Rng) {
    match action {
        Action::Maneuver { delta_v_km_s, .. } => {
            let n = delta_v_km_s.length();
            if n > 0.0 {
                let u = point(*delta_v_km_s / n, d.maneuver_pointing_rad, rng);
                *delta_v_km_s = u * n * (1.0 + d.maneuver_magnitude * rng.normal());
            }
        }
        Action::FiniteBurn {
            thrust_n,
            isp_s,
            direction,
            ..
        } => {
            // Floors keep the burn valid; a 1 % floor is a 99σ event at any sane σ.
            *thrust_n *= (1.0 + d.thrust * rng.normal()).max(0.01);
            *isp_s *= (1.0 + d.isp * rng.normal()).max(0.01);
            *direction = point(direction.normalize(), d.burn_pointing_rad, rng);
        }
        _ => {}
    }
}

/// Unit `u` rotated by `(θ₁, θ₂) ~ N(0, σ²)` about two axes normal to it. Draws 2 normals.
fn point(u: DVec3, sigma_rad: f64, rng: &mut Rng) -> DVec3 {
    let (t1, t2) = (sigma_rad * rng.normal(), sigma_rad * rng.normal());
    let theta = t1.hypot(t2);
    if theta == 0.0 {
        return u;
    }
    let (e1, e2) = u.any_orthonormal_pair();
    let w = (e1 * t1 + e2 * t2) / theta;
    u * theta.cos() + w * theta.sin()
}

/// SplitMix64 with Box–Muller normals. Deterministic, not cryptographic.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Independent stream for trial `k` of `seed`.
    pub fn trial(seed: u64, k: u64) -> Self {
        Self(mix(seed ^ mix(k.wrapping_add(1))))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix(self.0)
    }

    /// `(0, 1]`.
    pub fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) + 1) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// `N(0, 1)`.
    pub fn normal(&mut self) -> f64 {
        let (u1, u2) = (self.uniform(), self.uniform());
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Sample statistics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean: f64,
    /// Sample (n − 1); 0 for one value.
    pub sigma: f64,
    pub min: f64,
    pub max: f64,
    pub p05: f64,
    pub p50: f64,
    pub p95: f64,
}

/// Finite values only; `None` if there are none. Percentiles interpolate linearly.
pub fn summarize(values: &[f64]) -> Option<Summary> {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let mean = v.iter().sum::<f64>() / n as f64;
    let var = if n > 1 {
        v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64
    } else {
        0.0
    };
    let pct = |p: f64| {
        let x = p * (n - 1) as f64;
        let (i, f) = (x.floor() as usize, x.fract());
        if i + 1 < n {
            v[i] + f * (v[i + 1] - v[i])
        } else {
            v[i]
        }
    };
    Some(Summary {
        count: n,
        mean,
        sigma: var.sqrt(),
        min: v[0],
        max: v[n - 1],
        p05: pct(0.05),
        p50: pct(0.5),
        p95: pct(0.95),
    })
}

/// Sample mean and covariance (n − 1) of `[r, v]`. `None` for fewer than two.
#[allow(clippy::needless_range_loop)]
pub fn state_covariance(states: &[SpacecraftState]) -> Option<([f64; 6], Matrix6)> {
    if states.len() < 2 {
        return None;
    }
    let x = |s: &SpacecraftState| {
        let (r, v) = (s.position_km, s.velocity_km_s);
        [r.x, r.y, r.z, v.x, v.y, v.z]
    };
    let n = states.len() as f64;
    let mut mean = [0.0; 6];
    for s in states {
        for (m, xi) in mean.iter_mut().zip(x(s)) {
            *m += xi / n;
        }
    }
    let mut p = [[0.0; 6]; 6];
    for s in states {
        let d: [f64; 6] = std::array::from_fn(|i| x(s)[i] - mean[i]);
        for i in 0..6 {
            for j in 0..6 {
                p[i][j] += d[i] * d[j] / (n - 1.0);
            }
        }
    }
    Some((mean, p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::force::{EGM2008_GM_KM3_S2, PointMass};
    use crate::maneuver::LocalFrame;
    use crate::propagate::{Integrator, Method};
    use crate::sequence::{Goal, Parameter, Quantity, Stop, Target, TargetVariable};
    use crate::{Epoch, StateVector};

    const MU: f64 = EGM2008_GM_KM3_S2;
    const R1: f64 = 6678.137;

    fn two_body() -> Propagator {
        Propagator::new(Integrator::new(Method::Dop853).with_tolerance(1e-12, 1e-9))
            .with_force(PointMass { gm_km3_s2: MU })
    }

    fn leo() -> SpacecraftState {
        SpacecraftState::new(
            Epoch::from_utc(2026, 1, 1, 0, 0, 0.0),
            StateVector::new(
                DVec3::new(R1, 0.0, 0.0),
                DVec3::new(0.0, (MU / R1).sqrt(), 0.0),
            ),
            1000.0,
        )
    }

    fn cmd(label: Option<&str>, action: Action) -> Command {
        Command {
            label: label.map(Into::into),
            action,
        }
    }

    /// Burn at periapsis, coast to apoapsis.
    fn transfer(dv: f64) -> Vec<Command> {
        vec![
            cmd(
                Some("burn"),
                Action::Maneuver {
                    delta_v_km_s: DVec3::new(dv, 0.0, 0.0),
                    frame: LocalFrame::Vnb,
                    isp_s: None,
                },
            ),
            cmd(
                None,
                Action::Propagate {
                    stop: Stop::apoapsis(),
                    count: 1,
                    step_s: 600.0,
                    max_duration_s: 86_400.0,
                },
            ),
        ]
    }

    /// Target apoapsis radius `ra` with the burn's dv1.
    fn targeted(ra: f64) -> Vec<Command> {
        vec![cmd(
            None,
            Action::Target(Box::new(Target {
                variables: vec![TargetVariable {
                    command: "burn".into(),
                    parameter: Parameter::DeltaV(0),
                    perturbation: 1e-5,
                    max_step: 0.5,
                }],
                goals: vec![Goal {
                    quantity: Quantity::RadiusKm,
                    value: ra,
                    tolerance: 1e-3,
                }],
                body: transfer(1.0),
                max_iterations: 25,
            })),
        )]
    }

    #[test]
    fn normals_are_standard() {
        let mut rng = Rng::new(7);
        let x: Vec<f64> = (0..20_000).map(|_| rng.normal()).collect();
        let s = summarize(&x).unwrap();
        assert!(s.mean.abs() < 0.03, "{}", s.mean);
        assert!((s.sigma - 1.0).abs() < 0.03, "{}", s.sigma);
        assert!((s.p95 - 1.645).abs() < 0.06, "{}", s.p95);
        let a: Vec<u64> = (0..4).map(|k| Rng::trial(1, k).next_u64()).collect();
        assert!(a.iter().enumerate().all(|(i, x)| !a[i + 1..].contains(x)));
    }

    #[test]
    fn summary_percentiles_interpolate() {
        let s = summarize(&[4.0, f64::NAN, 1.0, 3.0, 2.0, 5.0]).unwrap();
        assert_eq!(
            (s.count, s.min, s.max, s.p50, s.mean),
            (5, 1.0, 5.0, 3.0, 3.0)
        );
        assert!((s.p05 - 1.2).abs() < 1e-12);
        assert!((s.sigma - 2.5f64.sqrt()).abs() < 1e-12);
        assert!(summarize(&[f64::NAN]).is_none());
    }

    #[test]
    fn no_dispersion_reproduces_the_nominal() {
        let mc = run(
            &two_body(),
            leo(),
            &targeted(30_000.0),
            &Dispersions::default(),
            Guidance::Open,
            3,
            1,
        )
        .unwrap();
        for t in &mc.trials {
            let end = t.outcome.as_ref().unwrap().end;
            assert!((end.position_km - mc.nominal.end.position_km).length() < 1e-6);
        }
    }

    /// σ(rₐ) from a magnitude error matches the linearised `drₐ/dv = 4a²v/μ`.
    #[test]
    fn magnitude_error_matches_linear_theory() {
        let ra = 30_000.0;
        let a = 0.5 * (R1 + ra);
        let v = (MU * (2.0 / R1 - 1.0 / a)).sqrt();
        let dv = v - (MU / R1).sqrt();
        let sigma = 1e-3;
        let d = Dispersions {
            maneuver_magnitude: sigma,
            ..Default::default()
        };
        let mc = run(
            &two_body(),
            leo(),
            &transfer(dv),
            &d,
            Guidance::Open,
            400,
            3,
        )
        .unwrap();
        let radii: Vec<f64> = mc
            .trials
            .iter()
            .map(|t| t.outcome.as_ref().unwrap().end.position_km.length())
            .collect();
        let s = summarize(&radii).unwrap();
        let expected = 4.0 * a * a * v / MU * sigma * dv;
        assert!((s.mean - ra).abs() < 0.2 * expected, "{} vs {ra}", s.mean);
        assert!(
            (s.sigma / expected - 1.0).abs() < 0.1,
            "{} vs {expected}",
            s.sigma
        );
    }

    #[test]
    fn closed_loop_retargets_open_loop_does_not() {
        let ra = 30_000.0;
        let d = Dispersions {
            state: Some(covariance::diagonal([1.0, 1.0, 1.0, 1e-6, 1e-6, 1e-6])),
            mass_kg: 10.0,
            ..Default::default()
        };
        let spread = |guidance| {
            let mc = run(&two_body(), leo(), &targeted(ra), &d, guidance, 20, 5).unwrap();
            let r: Vec<f64> = mc
                .trials
                .iter()
                .map(|t| t.outcome.as_ref().unwrap().end.position_km.length() - ra)
                .collect();
            r.iter().fold(0.0f64, |m, x| m.max(x.abs()))
        };
        assert!(spread(Guidance::Closed) < 1e-2);
        assert!(spread(Guidance::Open) > 1.0);
    }

    #[test]
    fn pointing_rotates_by_the_drawn_angle() {
        let mut rng = Rng::new(11);
        let u = DVec3::new(0.0, 0.6, 0.8);
        let angles: Vec<f64> = (0..5000)
            .map(|_| point(u, 1e-2, &mut rng).angle_between(u))
            .collect();
        // Rayleigh: mean σ√(π/2).
        let mean = angles.iter().sum::<f64>() / angles.len() as f64;
        assert!((mean / (1e-2 * (std::f64::consts::PI / 2.0).sqrt()) - 1.0).abs() < 0.03);
    }

    #[test]
    fn sample_covariance_of_a_drawn_state() {
        let p = covariance::diagonal([4.0, 1.0, 0.25, 1e-6, 4e-6, 1e-6]);
        let mc = run(
            &two_body(),
            leo(),
            &[cmd(
                None,
                Action::Propagate {
                    stop: Stop::Duration(0.0),
                    count: 1,
                    step_s: 60.0,
                    max_duration_s: 60.0,
                },
            )],
            &Dispersions {
                state: Some(p),
                ..Default::default()
            },
            Guidance::Open,
            4000,
            9,
        )
        .unwrap();
        let states: Vec<_> = mc.trials.iter().map(|t| t.initial).collect();
        let (_, c) = state_covariance(&states).unwrap();
        for i in 0..6 {
            assert!((c[i][i] / p[i][i] - 1.0).abs() < 0.08, "{i}: {}", c[i][i]);
        }
    }
}
