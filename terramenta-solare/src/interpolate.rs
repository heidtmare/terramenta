//! Tabulated ephemerides: Lagrange and Hermite interpolation (CCSDS OEM `INTERPOLATION`).

use glam::DVec3;

use crate::frame::{Ephemeris, StateVector};
use crate::time::Epoch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    /// `degree + 1` points per component; position and velocity interpolated independently.
    Lagrange { degree: usize },
    /// `(degree + 1) / 2` points, position and velocity; velocity is the polynomial's derivative.
    Hermite { degree: usize },
}

impl Default for Interpolation {
    fn default() -> Self {
        Self::Hermite { degree: 7 }
    }
}

impl Interpolation {
    fn points(self) -> usize {
        match self {
            Self::Lagrange { degree } => degree + 1,
            Self::Hermite { degree } => degree.div_ceil(2).max(1),
        }
    }
}

/// Outside coverage, the edge window is extrapolated.
#[derive(Debug, Clone, PartialEq)]
pub struct SampledEphemeris {
    origin: Epoch,
    /// Seconds since `origin`, ascending.
    times: Vec<f64>,
    states: Vec<StateVector>,
    pub method: Interpolation,
}

impl SampledEphemeris {
    /// Sorts by epoch. Panics on fewer than two samples.
    pub fn new(mut samples: Vec<(Epoch, StateVector)>, method: Interpolation) -> Self {
        assert!(samples.len() >= 2, "need at least two samples");
        samples.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let origin = samples[0].0;
        Self {
            origin,
            times: samples.iter().map(|s| s.0.seconds_since(origin)).collect(),
            states: samples.into_iter().map(|s| s.1).collect(),
            method,
        }
    }

    pub fn coverage(&self) -> (Epoch, Epoch) {
        (
            self.origin,
            self.origin.advanced_by_seconds(*self.times.last().unwrap()),
        )
    }

    pub fn samples(&self) -> impl Iterator<Item = (Epoch, StateVector)> + '_ {
        self.times
            .iter()
            .zip(&self.states)
            .map(|(t, s)| (self.origin.advanced_by_seconds(*t), *s))
    }

    pub fn try_state_at(&self, epoch: Epoch) -> Option<StateVector> {
        let (a, b) = self.coverage();
        (epoch >= a && epoch <= b).then(|| self.state_at(epoch))
    }

    fn window(&self, t: f64) -> std::ops::Range<usize> {
        let n = self.method.points().min(self.times.len());
        let i = self.times.partition_point(|x| *x <= t);
        let start = (i.saturating_sub(n / 2)).min(self.times.len() - n);
        start..start + n
    }
}

impl Ephemeris for SampledEphemeris {
    fn state_at(&self, epoch: Epoch) -> StateVector {
        let t = epoch.seconds_since(self.origin);
        let w = self.window(t);
        let (ts, ss) = (&self.times[w.clone()], &self.states[w]);
        match self.method {
            Interpolation::Lagrange { .. } => StateVector::new(
                lagrange(ts, ss.iter().map(|s| s.position_km), t),
                lagrange(ts, ss.iter().map(|s| s.velocity_km_s), t),
            ),
            Interpolation::Hermite { .. } => {
                let (r, v) = hermite(ts, ss, t);
                StateVector::new(r, v)
            }
        }
    }
}

fn lagrange(ts: &[f64], ys: impl Iterator<Item = DVec3>, t: f64) -> DVec3 {
    ys.enumerate()
        .map(|(i, y)| {
            let w: f64 = ts
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, tj)| (t - tj) / (ts[i] - tj))
                .product();
            y * w
        })
        .sum()
}

/// Newton divided differences on doubled nodes; returns `(p(t), p'(t))`.
fn hermite(ts: &[f64], ss: &[StateVector], t: f64) -> (DVec3, DVec3) {
    let n = 2 * ts.len();
    let z: Vec<f64> = ts.iter().flat_map(|t| [*t, *t]).collect();
    let mut q: Vec<DVec3> = ss
        .iter()
        .flat_map(|s| [s.position_km, s.position_km])
        .collect();
    let mut coef = vec![q[0]];
    for j in 1..n {
        for i in (j..n).rev() {
            q[i] = if j == 1 && i % 2 == 1 {
                ss[i / 2].velocity_km_s
            } else {
                (q[i] - q[i - 1]) / (z[i] - z[i - j])
            };
        }
        coef.push(q[j]);
    }
    let (mut p, mut dp) = (coef[n - 1], DVec3::ZERO);
    for i in (0..n - 1).rev() {
        dp = dp * (t - z[i]) + p;
        p = p * (t - z[i]) + coef[i];
    }
    (p, dp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orbit::OrbitalElements;

    #[test]
    fn interpolates_kepler_orbit() {
        let gm = 398_600.441_8;
        let el = OrbitalElements::from_state(
            StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 6.5, 4.0)),
            gm,
            0.0,
        );
        let samples: Vec<_> = (0..40)
            .map(|k| {
                let t = k as f64 * 60.0;
                (Epoch::J2000.advanced_by_seconds(t), el.state_at(gm, t))
            })
            .collect();
        for (method, tol_r, tol_v) in [
            (Interpolation::Lagrange { degree: 8 }, 1e-6, 1e-9),
            (Interpolation::Hermite { degree: 7 }, 1e-6, 1e-9),
            (Interpolation::Hermite { degree: 3 }, 1e-1, 1e-3),
        ] {
            let eph = SampledEphemeris::new(samples.clone(), method);
            for t in [30.0, 1234.5, 2300.0] {
                let s = eph.state_at(Epoch::J2000.advanced_by_seconds(t));
                let e = el.state_at(gm, t);
                assert!(
                    (s.position_km - e.position_km).length() < tol_r,
                    "{method:?} t={t}"
                );
                assert!(
                    (s.velocity_km_s - e.velocity_km_s).length() < tol_v,
                    "{method:?} t={t}"
                );
            }
            // Exact at nodes.
            let s = eph.state_at(Epoch::J2000.advanced_by_seconds(600.0));
            assert!((s.position_km - samples[10].1.position_km).length() < 1e-9);
        }
    }
}
