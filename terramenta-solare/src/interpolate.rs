//! Tabulated ephemerides: Lagrange and Hermite interpolation (CCSDS OEM `INTERPOLATION`).

use glam::DVec3;

use crate::frame::{Ephemeris, StateVector};
use crate::time::Epoch;

/// Windows are capped at [`MAX_POINTS`] nodes: Lagrange degree ≤ 31, Hermite degree ≤ 63.
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

/// Window size cap; higher degrees are clamped.
pub const MAX_POINTS: usize = 32;

impl Interpolation {
    fn points(self) -> usize {
        match self {
            Self::Lagrange { degree } => degree + 1,
            Self::Hermite { degree } => degree.div_ceil(2).max(1),
        }
        .min(MAX_POINTS)
    }
}

/// Outside coverage, the edge window is extrapolated. A repeated epoch starts a new
/// segment (e.g. pre/post impulsive burn); windows never span segments, and the later
/// segment owns the boundary epoch.
#[derive(Debug, Clone, PartialEq)]
pub struct SampledEphemeris {
    origin: Epoch,
    /// Seconds since `origin`, non-decreasing.
    times: Vec<f64>,
    states: Vec<StateVector>,
    /// Segment start indices; `[0, ..]`.
    breaks: Vec<usize>,
    pub method: Interpolation,
}

impl SampledEphemeris {
    /// Stable sort by epoch. Panics on fewer than two samples.
    pub fn new(mut samples: Vec<(Epoch, StateVector)>, method: Interpolation) -> Self {
        assert!(samples.len() >= 2, "need at least two samples");
        samples.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let origin = samples[0].0;
        let times: Vec<f64> = samples.iter().map(|s| s.0.seconds_since(origin)).collect();
        let breaks = std::iter::once(0)
            .chain((1..times.len()).filter(|&i| times[i] == times[i - 1]))
            .collect();
        Self {
            origin,
            times,
            states: samples.into_iter().map(|s| s.1).collect(),
            breaks,
            method,
        }
    }

    pub fn coverage(&self) -> (Epoch, Epoch) {
        (
            self.origin,
            self.origin.advanced_by_seconds(*self.times.last().unwrap()),
        )
    }

    /// All samples; boundary epochs appear once per adjacent segment.
    pub fn samples(&self) -> impl Iterator<Item = (Epoch, StateVector)> + '_ {
        self.times
            .iter()
            .zip(&self.states)
            .map(|(t, s)| (self.origin.advanced_by_seconds(*t), *s))
    }

    /// Samples per segment, each with strictly increasing epochs.
    pub fn segments(&self) -> impl Iterator<Item = Vec<(Epoch, StateVector)>> + '_ {
        let ends = self.breaks[1..].iter().copied().chain([self.times.len()]);
        self.breaks.iter().zip(ends).map(|(&lo, hi)| {
            (lo..hi)
                .map(|i| {
                    (
                        self.origin.advanced_by_seconds(self.times[i]),
                        self.states[i],
                    )
                })
                .collect()
        })
    }

    pub fn try_state_at(&self, epoch: Epoch) -> Option<StateVector> {
        let (a, b) = self.coverage();
        (epoch >= a && epoch <= b).then(|| self.state_at(epoch))
    }

    fn window(&self, t: f64) -> std::ops::Range<usize> {
        let i = self.times.partition_point(|x| *x <= t);
        // Segment holding the last node ≤ t (the first segment before coverage).
        let s = self.breaks.partition_point(|&b| b < i.max(1)) - 1;
        let lo = self.breaks[s];
        let hi = self.breaks.get(s + 1).copied().unwrap_or(self.times.len());
        let n = self.method.points().min(hi - lo);
        // ≥ 1 node at or before t, so a node is always in its own window.
        let start = i.saturating_sub((n / 2).max(1)).clamp(lo, hi - n);
        start..start + n
    }
}

impl Ephemeris for SampledEphemeris {
    fn state_at(&self, epoch: Epoch) -> StateVector {
        let t = epoch.seconds_since(self.origin);
        let w = self.window(t);
        let (ts, ss) = (&self.times[w.clone()], &self.states[w]);
        match self.method {
            Interpolation::Lagrange { .. } => {
                let w = lagrange_weights(ts, t);
                StateVector::new(
                    weighted(&w, ss.iter().map(|s| s.position_km)),
                    weighted(&w, ss.iter().map(|s| s.velocity_km_s)),
                )
            }
            Interpolation::Hermite { .. } => {
                let (r, v) = hermite(ts, ss, t);
                StateVector::new(r, v)
            }
        }
    }

    fn position_at(&self, epoch: Epoch) -> DVec3 {
        let t = epoch.seconds_since(self.origin);
        let w = self.window(t);
        let (ts, ss) = (&self.times[w.clone()], &self.states[w]);
        match self.method {
            Interpolation::Lagrange { .. } => {
                weighted(&lagrange_weights(ts, t), ss.iter().map(|s| s.position_km))
            }
            Interpolation::Hermite { .. } => hermite(ts, ss, t).0,
        }
    }
}

/// Lagrange basis at `t`; `ts.len() <= MAX_POINTS`.
fn lagrange_weights(ts: &[f64], t: f64) -> [f64; MAX_POINTS] {
    let mut w = [0.0; MAX_POINTS];
    for (i, ti) in ts.iter().enumerate() {
        w[i] = ts
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, tj)| (t - tj) / (ti - tj))
            .product();
    }
    w
}

fn weighted(w: &[f64], ys: impl Iterator<Item = DVec3>) -> DVec3 {
    ys.zip(w).map(|(y, w)| y * *w).sum()
}

/// Newton divided differences on doubled nodes; returns `(p(t), p'(t))`.
/// `ts.len() <= MAX_POINTS`.
fn hermite(ts: &[f64], ss: &[StateVector], t: f64) -> (DVec3, DVec3) {
    let n = 2 * ts.len();
    let mut z = [0.0; 2 * MAX_POINTS];
    let mut q = [DVec3::ZERO; 2 * MAX_POINTS];
    for (i, (t, s)) in ts.iter().zip(ss).enumerate() {
        z[2 * i..2 * i + 2].fill(*t);
        q[2 * i..2 * i + 2].fill(s.position_km);
    }
    // In place: after pass `j`, `q[j]` is the `j`th Newton coefficient.
    for j in 1..n {
        for i in (j..n).rev() {
            q[i] = if j == 1 && i % 2 == 1 {
                ss[i / 2].velocity_km_s
            } else {
                (q[i] - q[i - 1]) / (z[i] - z[i - j])
            };
        }
    }
    let (mut p, mut dp) = (q[n - 1], DVec3::ZERO);
    for i in (0..n - 1).rev() {
        dp = dp * (t - z[i]) + p;
        p = p * (t - z[i]) + q[i];
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
                let p = eph.position_at(Epoch::J2000.advanced_by_seconds(t));
                assert_eq!(p, s.position_km, "{method:?} t={t}");
            }
            // Exact at nodes.
            let s = eph.state_at(Epoch::J2000.advanced_by_seconds(600.0));
            assert!((s.position_km - samples[10].1.position_km).length() < 1e-9);
        }
    }

    #[test]
    fn one_point_window_is_exact_at_nodes() {
        let samples: Vec<_> = (0..5)
            .map(|k| {
                let k = k as f64;
                (
                    Epoch::J2000.advanced_by_seconds(60.0 * k),
                    StateVector::new(DVec3::splat(k), DVec3::splat(-k)),
                )
            })
            .collect();
        for method in [
            Interpolation::Lagrange { degree: 0 },
            Interpolation::Hermite { degree: 1 },
        ] {
            let eph = SampledEphemeris::new(samples.clone(), method);
            for (e, s) in &samples {
                assert_eq!(eph.state_at(*e).position_km, s.position_km, "{method:?}");
            }
        }
    }

    #[test]
    fn degree_above_old_cap_uses_full_window() {
        assert_eq!(Interpolation::Lagrange { degree: 20 }.points(), 21);
        assert_eq!(Interpolation::Lagrange { degree: 99 }.points(), MAX_POINTS);
    }

    #[test]
    fn repeated_epoch_splits_segments() {
        // Δv at t = 600: windows on either side must not see the other arc.
        let gm = 398_600.441_8;
        let s0 = StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 6.5, 4.0));
        let a = OrbitalElements::from_state(s0, gm, 0.0);
        let burn = a.state_at(gm, 600.0);
        let b = OrbitalElements::from_state(
            StateVector::new(burn.position_km, burn.velocity_km_s * 1.05),
            gm,
            0.0,
        );
        let at = |t: f64| Epoch::J2000.advanced_by_seconds(t);
        let mut samples: Vec<_> = (0..=10)
            .map(|k| (at(k as f64 * 60.0), a.state_at(gm, k as f64 * 60.0)))
            .collect();
        samples.extend(
            (10..=20).map(|k| (at(k as f64 * 60.0), b.state_at(gm, k as f64 * 60.0 - 600.0))),
        );
        let eph = SampledEphemeris::new(samples, Interpolation::default());
        assert_eq!(
            eph.segments().map(|s| s.len()).collect::<Vec<_>>(),
            [11, 11]
        );
        for (t, truth) in [
            (570.0, a.state_at(gm, 570.0)),
            (630.0, b.state_at(gm, 30.0)),
        ] {
            let s = eph.state_at(at(t));
            assert!((s.position_km - truth.position_km).length() < 1e-6, "t={t}");
            assert!(
                (s.velocity_km_s - truth.velocity_km_s).length() < 1e-9,
                "t={t}"
            );
        }
        // Boundary belongs to the post-burn arc.
        let s = eph.state_at(at(600.0));
        assert!((s.velocity_km_s - burn.velocity_km_s * 1.05).length() < 1e-12);
    }
}
