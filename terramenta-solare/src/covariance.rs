//! Orbit state covariance: 6×6 `[r, v]` matrices (km, km/s), linear propagation through the
//! state transition matrix, local-frame rotation, process noise, position ellipsoids.
//!
//! `P(t) = Φ P₀ Φᵀ + Q`. Local frames rotate both blocks by the same matrix (CDM/OEM
//! convention); the frame's angular velocity is not applied to the velocity block.

use glam::{DMat3, DVec3};

use crate::maneuver::LocalFrame;

/// Row-major 6×6.
pub type Matrix6 = [[f64; 6]; 6];

pub fn identity() -> Matrix6 {
    std::array::from_fn(|i| std::array::from_fn(|j| f64::from(u8::from(i == j))))
}

pub fn diagonal(d: [f64; 6]) -> Matrix6 {
    std::array::from_fn(|i| std::array::from_fn(|j| if i == j { d[i] } else { 0.0 }))
}

pub fn mul(a: &Matrix6, b: &Matrix6) -> Matrix6 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..6).map(|k| a[i][k] * b[k][j]).sum()))
}

pub fn transpose(a: &Matrix6) -> Matrix6 {
    std::array::from_fn(|i| std::array::from_fn(|j| a[j][i]))
}

pub fn add(a: &Matrix6, b: &Matrix6) -> Matrix6 {
    std::array::from_fn(|i| std::array::from_fn(|j| a[i][j] + b[i][j]))
}

/// `A P Aᵀ`, symmetrised.
pub fn congruence(a: &Matrix6, p: &Matrix6) -> Matrix6 {
    symmetrize(&mul(&mul(a, p), &transpose(a)))
}

pub fn symmetrize(p: &Matrix6) -> Matrix6 {
    std::array::from_fn(|i| std::array::from_fn(|j| 0.5 * (p[i][j] + p[j][i])))
}

/// Gauss–Jordan with partial pivoting. `None` if singular.
pub fn inverse(a: &Matrix6) -> Option<Matrix6> {
    let mut m = *a;
    let mut inv = identity();
    let scale = a.iter().flatten().fold(0.0f64, |s, x| s.max(x.abs()));
    for c in 0..6 {
        let p = (c..6).max_by(|&i, &j| m[i][c].abs().total_cmp(&m[j][c].abs()))?;
        if m[p][c].abs() <= 1e-14 * scale {
            return None;
        }
        m.swap(c, p);
        inv.swap(c, p);
        let d = m[c][c];
        for j in 0..6 {
            m[c][j] /= d;
            inv[c][j] /= d;
        }
        for i in (0..6).filter(|&i| i != c) {
            let f = m[i][c];
            if f != 0.0 {
                for j in 0..6 {
                    m[i][j] -= f * m[c][j];
                    inv[i][j] -= f * inv[c][j];
                }
            }
        }
    }
    Some(inv)
}

/// `diag(R, R)`: `R` 3×3 as a block rotation.
pub fn block(r: DMat3) -> Matrix6 {
    let c = r.to_cols_array_2d();
    std::array::from_fn(|i| {
        std::array::from_fn(|j| if i / 3 == j / 3 { c[j % 3][i % 3] } else { 0.0 })
    })
}

/// Inertial covariance → `frame` axes at `(r, v)`.
pub fn to_local(p: &Matrix6, frame: LocalFrame, r: DVec3, v: DVec3) -> Matrix6 {
    congruence(&block(frame.to_inertial(r, v).transpose()), p)
}

/// `frame` axes at `(r, v)` → inertial covariance.
pub fn from_local(p: &Matrix6, frame: LocalFrame, r: DVec3, v: DVec3) -> Matrix6 {
    congruence(&block(frame.to_inertial(r, v)), p)
}

/// 1σ per component: `√P_ii`.
pub fn sigmas(p: &Matrix6) -> [f64; 6] {
    std::array::from_fn(|i| p[i][i].max(0.0).sqrt())
}

/// Lower triangle, row by row (21 values; CCSDS OEM/CDM order).
pub fn lower_triangle(p: &Matrix6) -> [f64; 21] {
    let mut out = [0.0; 21];
    let mut k = 0;
    for (i, row) in p.iter().enumerate() {
        for &x in &row[..=i] {
            out[k] = x;
            k += 1;
        }
    }
    out
}

/// Inverse of [`lower_triangle`].
#[allow(clippy::needless_range_loop)]
pub fn from_lower_triangle(t: &[f64; 21]) -> Matrix6 {
    let mut p = [[0.0; 6]; 6];
    let mut k = 0;
    for i in 0..6 {
        for j in 0..=i {
            p[i][j] = t[k];
            p[j][i] = t[k];
            k += 1;
        }
    }
    p
}

/// White acceleration noise (state noise compensation): spectral density `psd_km2_s3` per
/// axis of `frame`, km²/s³.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessNoise {
    pub psd_km2_s3: DVec3,
    pub frame: LocalFrame,
}

impl ProcessNoise {
    /// Inertial `Q` over `dt_s` at `(r, v)`:
    /// `[[dt³/3 S, dt²/2 S], [dt²/2 S, dt S]]`, `S = R diag(psd) Rᵀ`.
    pub fn q(&self, dt_s: f64, r: DVec3, v: DVec3) -> Matrix6 {
        let rot = self.frame.to_inertial(r, v);
        let s = rot * DMat3::from_diagonal(self.psd_km2_s3) * rot.transpose();
        let s = s.to_cols_array_2d();
        let dt = dt_s.abs();
        std::array::from_fn(|i| {
            std::array::from_fn(|j| {
                let f = match (i / 3, j / 3) {
                    (0, 0) => dt.powi(3) / 3.0,
                    (1, 1) => dt,
                    _ => dt * dt / 2.0,
                };
                f * s[j % 3][i % 3]
            })
        })
    }
}

/// Linear covariance propagation along samples of `Φ(t, t₀)`, with process noise added
/// between consecutive samples: `P_k = Φ_k,k-1 P_k-1 Φ_k,k-1ᵀ + Q(t_k − t_k-1)`.
#[derive(Debug, Clone)]
pub struct CovariancePropagation {
    p0: Matrix6,
    noise: Option<ProcessNoise>,
    /// `(t, Φ(t, t₀), P(t))` at the last sample.
    last: Option<(f64, Matrix6, Matrix6)>,
}

impl CovariancePropagation {
    pub fn new(p0: Matrix6, noise: Option<ProcessNoise>) -> Self {
        Self {
            p0: symmetrize(&p0),
            noise,
            last: None,
        }
    }

    /// `P` at offset `t_s` with `Φ(t, t₀)` and state `(r, v)`. Samples in time order.
    pub fn sample(&mut self, t_s: f64, phi: &Matrix6, r: DVec3, v: DVec3) -> Matrix6 {
        let p = match (&self.last, self.noise) {
            (Some((t0, phi0, p0)), Some(noise)) => match inverse(phi0) {
                Some(inv) => {
                    let p = congruence(&mul(phi, &inv), p0);
                    if t_s == *t0 {
                        p
                    } else {
                        add(&p, &noise.q(t_s - t0, r, v))
                    }
                }
                None => *p0,
            },
            _ => congruence(phi, &self.p0),
        };
        self.last = Some((t_s, *phi, p));
        p
    }
}

/// Position 1σ ellipsoid: semi-axes (km, descending) and their unit directions.
pub fn position_ellipsoid(p: &Matrix6) -> ([f64; 3], [DVec3; 3]) {
    let a = [
        [p[0][0], p[0][1], p[0][2]],
        [p[1][0], p[1][1], p[1][2]],
        [p[2][0], p[2][1], p[2][2]],
    ];
    let (values, vectors) = symmetric_eigen3(a);
    let mut order = [0, 1, 2];
    order.sort_by(|&i, &j| values[j].total_cmp(&values[i]));
    (
        order.map(|i| values[i].max(0.0).sqrt()),
        order.map(|i| vectors[i]),
    )
}

/// Cyclic Jacobi. Eigenvalues and unit eigenvectors.
#[allow(clippy::needless_range_loop)]
pub fn symmetric_eigen3(mut a: [[f64; 3]; 3]) -> ([f64; 3], [DVec3; 3]) {
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..50 {
        let off = a[0][1].powi(2) + a[0][2].powi(2) + a[1][2].powi(2);
        let diag = a[0][0].powi(2) + a[1][1].powi(2) + a[2][2].powi(2);
        if off <= 1e-30 * diag || off == 0.0 {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q] == 0.0 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let (c, s) = (1.0 / (t * t + 1.0).sqrt(), t / (t * t + 1.0).sqrt());
            for k in 0..3 {
                let (akp, akq) = (a[k][p], a[k][q]);
                a[k][p] = c * akp - s * akq;
                a[k][q] = s * akp + c * akq;
            }
            for k in 0..3 {
                let (apk, aqk) = (a[p][k], a[q][k]);
                a[p][k] = c * apk - s * aqk;
                a[q][k] = s * apk + c * aqk;
            }
            for row in &mut v {
                let (vp, vq) = (row[p], row[q]);
                row[p] = c * vp - s * vq;
                row[q] = s * vp + c * vq;
            }
        }
    }
    (
        [a[0][0], a[1][1], a[2][2]],
        std::array::from_fn(|i| DVec3::new(v[0][i], v[1][i], v[2][i])),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::force::PointMass;
    use crate::frame::StateVector;
    use crate::propagate::{Integrator, Method, Propagator, SpacecraftState};
    use crate::time::Epoch;

    fn sample() -> Matrix6 {
        let a: Matrix6 =
            std::array::from_fn(|i| std::array::from_fn(|j| ((i * 7 + j * 3) % 5) as f64 - 1.5));
        add(&mul(&a, &transpose(&a)), &diagonal([1.0; 6]))
    }

    #[test]
    fn algebra_round_trips() {
        let p = sample();
        let inv = inverse(&p).unwrap();
        let id = mul(&p, &inv);
        for (a, b) in id.iter().flatten().zip(identity().iter().flatten()) {
            assert!((a - b).abs() < 1e-12);
        }
        assert_eq!(from_lower_triangle(&lower_triangle(&p)), p);
        assert!(inverse(&[[1.0; 6]; 6]).is_none());
        let (r, v) = (DVec3::new(7000.0, 100.0, -50.0), DVec3::new(0.5, 7.4, 1.0));
        let back = from_local(&to_local(&p, LocalFrame::Rsw, r, v), LocalFrame::Rsw, r, v);
        for i in 0..6 {
            for j in 0..6 {
                assert!((back[i][j] - p[i][j]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn ellipsoid_axes_are_eigenpairs() {
        let p = sample();
        let (axes, dirs) = position_ellipsoid(&p);
        assert!(axes[0] >= axes[1] && axes[1] >= axes[2]);
        let m = DMat3::from_cols_array_2d(&[
            [p[0][0], p[1][0], p[2][0]],
            [p[0][1], p[1][1], p[2][1]],
            [p[0][2], p[1][2], p[2][2]],
        ]);
        for (a, d) in axes.iter().zip(dirs) {
            assert!((d.length() - 1.0).abs() < 1e-12);
            assert!((m * d - a * a * d).length() < 1e-10);
        }
        let (axes, _) = position_ellipsoid(&diagonal([4.0, 9.0, 1.0, 0.0, 0.0, 0.0]));
        assert_eq!(axes, [3.0, 2.0, 1.0]);
    }

    #[test]
    fn radial_error_becomes_along_track() {
        let gm = 398_600.441_5;
        let s0 = SpacecraftState::new(
            Epoch::J2000,
            StateVector::new(
                DVec3::new(7000.0, 0.0, 0.0),
                DVec3::new(0.0, (gm / 7000.0f64).sqrt(), 0.0),
            ),
            1000.0,
        );
        let mut prop = Propagator::new(Integrator::new(Method::Dop853))
            .with_force(PointMass { gm_km3_s2: gm });
        // 100 m radial, 1 mm/s in-track: along-track grows secularly.
        let p0 = from_local(
            &diagonal([0.1f64.powi(2), 0.0, 0.0, 0.0, 1e-6f64.powi(2), 0.0]),
            LocalFrame::Rsw,
            s0.position_km,
            s0.velocity_km_s,
        );
        let mut plain = CovariancePropagation::new(p0, None);
        let noise = ProcessNoise {
            psd_km2_s3: DVec3::splat(1e-15),
            frame: LocalFrame::Rsw,
        };
        let mut noisy = CovariancePropagation::new(p0, Some(noise));
        let mut out = Vec::new();
        let day = 86_400.0;
        prop.propagate_stm_with(
            &s0,
            Epoch::J2000.advanced_by_seconds(day),
            600.0,
            |s, phi| {
                let t = s.epoch.seconds_since(Epoch::J2000);
                let a = plain.sample(t, phi, s.position_km, s.velocity_km_s);
                let b = noisy.sample(t, phi, s.position_km, s.velocity_km_s);
                out.push((
                    sigmas(&to_local(
                        &a,
                        LocalFrame::Rsw,
                        s.position_km,
                        s.velocity_km_s,
                    )),
                    sigmas(&to_local(
                        &b,
                        LocalFrame::Rsw,
                        s.position_km,
                        s.velocity_km_s,
                    )),
                ));
            },
        )
        .unwrap();
        let (first, last) = (out[0].0, out.last().unwrap().0);
        assert!((first[0] - 0.1).abs() < 1e-12 && first[1] < 1e-12);
        // Secular in-track drift ≈ 3π·δa per orbit with δa = 2δr (circular): kilometres per day.
        assert!(last[1] > 10.0 * last[0] && last[1] > 5.0, "{last:?}");
        assert!(last[2] < 1e-9, "planar errors stay planar");
        let with_noise = out.last().unwrap().1;
        assert!(
            with_noise[2] > 1e-3 && with_noise[1] > last[1],
            "{with_noise:?}"
        );
        // Q blocks: dt³/3, dt²/2, dt.
        let q = noise.q(10.0, s0.position_km, s0.velocity_km_s);
        assert!(q[0][0] > 0.0 && (q[0][3] / (5.0 * q[3][3]) - 1.0).abs() < 1e-12);
    }
}
