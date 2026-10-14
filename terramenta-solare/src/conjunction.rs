//! Conjunction assessment: closest approaches between two trajectories, the encounter
//! plane and the 2D probability of collision (Foster/Alfano).
//!
//! Approaches are interior local minima of range: rising zero crossings of `Δr·Δv`,
//! bracketed on a fixed grid and refined by Brent's method. A bracket whose nearer end is
//! farther than `threshold + |Δv|·step` is not refined. Pc assumes a short encounter:
//! rectilinear relative motion, constant Gaussian position error, the combined covariance
//! projected onto the plane normal to the relative velocity, and a hard-body sphere whose
//! radius is the sum of both objects'.

use glam::{DMat3, DVec3};

use crate::covariance::{Matrix6, symmetric_eigen3};
use crate::events::brent;
use crate::maneuver::LocalFrame;

/// Secondary relative to primary at TCA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Approach {
    /// Seconds from the search start.
    pub t_s: f64,
    pub position_km: DVec3,
    pub velocity_km_s: DVec3,
}

impl Approach {
    pub fn miss_km(&self) -> f64 {
        self.position_km.length()
    }
}

/// Closest approaches within `threshold_km` on `[0, span_s]`, in time order.
/// `relative(t)` is secondary − primary; `None` breaks the brackets it touches.
pub fn closest_approaches(
    relative: &mut impl FnMut(f64) -> Option<(DVec3, DVec3)>,
    span_s: f64,
    step_s: f64,
    threshold_km: f64,
    tolerance_s: f64,
) -> Vec<Approach> {
    let span = span_s.max(0.0);
    let step = step_s.abs().max(1e-3);
    let n = (span / step).ceil().max(1.0) as usize;
    // (t, Δr·Δv, |Δr|, |Δv|)
    fn sample(
        relative: &mut impl FnMut(f64) -> Option<(DVec3, DVec3)>,
        t: f64,
    ) -> Option<(f64, f64, f64, f64)> {
        relative(t).map(|(r, v)| (t, r.dot(v), r.length(), v.length()))
    }
    let mut out = Vec::new();
    let mut prev = sample(relative, 0.0);
    for i in 1..=n {
        let next = sample(relative, (i as f64 * step).min(span));
        if let (Some(a), Some(b)) = (prev, next)
            && a.1 <= 0.0
            && b.1 > 0.0
            && a.2.min(b.2) - a.3.max(b.3) * (b.0 - a.0) <= threshold_km
        {
            let mut g = |t: f64| relative(t).map_or(f64::NAN, |(r, v)| r.dot(v));
            let t = brent(&mut g, a.0, b.0, a.1, b.1, tolerance_s);
            if let Some((r, v)) = relative(t)
                && r.length() <= threshold_km
            {
                out.push(Approach {
                    t_s: t,
                    position_km: r,
                    velocity_km_s: v,
                });
            }
        }
        prev = next;
    }
    out
}

/// Encounter plane at an approach: unit `x` along the miss (normal to `Δv`), `y` along `Δv`,
/// `z = x × y`. A zero miss takes any `x` normal to `Δv`.
pub fn encounter_axes(position_km: DVec3, velocity_km_s: DVec3) -> Option<[DVec3; 3]> {
    let y = velocity_km_s.try_normalize()?;
    let x = (position_km - y * position_km.dot(y))
        .try_normalize()
        .unwrap_or_else(|| y.any_orthonormal_vector());
    Some([x, y, x.cross(y)])
}

/// Position block of a 6×6 covariance.
pub fn position_block(p: &Matrix6) -> DMat3 {
    DMat3::from_cols_array_2d(&std::array::from_fn(|c| std::array::from_fn(|r| p[r][c])))
}

/// `[[xx, xz], [xz, zz]]` of a 3×3 position covariance in the encounter plane.
pub fn encounter_covariance(c: &DMat3, axes: &[DVec3; 3]) -> [[f64; 2]; 2] {
    let (x, z) = (axes[0], axes[2]);
    let xz = x.dot(*c * z);
    [[x.dot(*c * x), xz], [xz, z.dot(*c * z)]]
}

/// Probability that a 2D Gaussian (mean `miss_km` in the plane, covariance `c` km²) falls
/// within `radius_km` of the origin. Principal-axis 1D quadrature over `x = R sin θ`.
pub fn probability(miss_km: [f64; 2], c: [[f64; 2]; 2], radius_km: f64) -> f64 {
    if radius_km.is_nan() || radius_km <= 0.0 {
        return 0.0;
    }
    let (s1, s2, angle) = principal(c);
    let floor = radius_km * 1e-9;
    let (s1, s2) = (s1.max(floor), s2.max(floor));
    let (sin, cos) = angle.sin_cos();
    let u = cos * miss_km[0] + sin * miss_km[1];
    let w = -sin * miss_km[0] + cos * miss_km[1];
    let n = ((64.0 * radius_km / s1.min(s2)).ceil() as usize).clamp(64, 20_000) & !1;
    let h = std::f64::consts::PI / n as f64;
    let mut sum = 0.0;
    for k in 0..=n {
        let theta = -std::f64::consts::FRAC_PI_2 + k as f64 * h;
        let (x, half) = (radius_km * theta.sin(), radius_km * theta.cos());
        let density =
            (-0.5 * ((x - u) / s1).powi(2)).exp() / (s1 * (2.0 * std::f64::consts::PI).sqrt());
        let f = density * normal_between((-half - w) / s2, (half - w) / s2) * half;
        let weight = if k == 0 || k == n {
            1.0
        } else if k % 2 == 1 {
            4.0
        } else {
            2.0
        };
        sum += weight * f;
    }
    (sum * h / 3.0).clamp(0.0, 1.0)
}

/// Largest [`probability`] over a scale factor on the covariance `c` (identity when
/// `None`: isotropic): Alfano's maximum Pc, for an unknown or unreliable covariance size.
pub fn max_probability(miss_km: [f64; 2], c: Option<[[f64; 2]; 2]>, radius_km: f64) -> f64 {
    let c = c.unwrap_or([[1.0, 0.0], [0.0, 1.0]]);
    let size = (c[0][0] + c[1][1]).max(f64::MIN_POSITIVE).sqrt();
    let reach = miss_km[0].hypot(miss_km[1]).max(radius_km);
    let at = |log_k: f64| {
        let k = log_k.exp();
        probability(miss_km, c.map(|row| row.map(|v| v * k)), radius_km)
    };
    // σ from reach/1000 to 1000·reach.
    let (mut lo, mut hi) = (
        2.0 * (reach * 1e-3 / size).ln(),
        2.0 * (reach * 1e3 / size).ln(),
    );
    const R: f64 = 0.618_033_988_749_894_9;
    let mut x1 = hi - R * (hi - lo);
    let mut x2 = lo + R * (hi - lo);
    let (mut f1, mut f2) = (at(x1), at(x2));
    for _ in 0..80 {
        if f1 < f2 {
            lo = x1;
            (x1, f1) = (x2, f2);
            x2 = lo + R * (hi - lo);
            f2 = at(x2);
        } else {
            hi = x2;
            (x2, f2) = (x1, f1);
            x1 = hi - R * (hi - lo);
            f1 = at(x1);
        }
    }
    f1.max(f2)
}

/// `(σ major, σ minor, angle of the major axis from x)`.
fn principal(c: [[f64; 2]; 2]) -> (f64, f64, f64) {
    let (a, b, d) = (c[0][0], c[0][1], c[1][1]);
    let mean = 0.5 * (a + d);
    let radius = (0.25 * (a - d).powi(2) + b * b).sqrt();
    let angle = 0.5 * (2.0 * b).atan2(a - d);
    (
        (mean + radius).max(0.0).sqrt(),
        (mean - radius).max(0.0).sqrt(),
        angle,
    )
}

/// `P(lo < Z < hi)`, standard normal, accurate in the tails.
fn normal_between(lo: f64, hi: f64) -> f64 {
    let s = std::f64::consts::FRAC_1_SQRT_2;
    if lo >= 0.0 {
        0.5 * (erfc(lo * s) - erfc(hi * s))
    } else if hi <= 0.0 {
        0.5 * (erfc(-hi * s) - erfc(-lo * s))
    } else {
        1.0 - 0.5 * (erfc(-lo * s) + erfc(hi * s))
    }
}

/// Complementary error function: positive-term series below 2.5, continued fraction above.
pub fn erfc(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x < 0.0 {
        return 2.0 - erfc(-x);
    }
    let pi = std::f64::consts::PI;
    if x < 2.5 {
        // erf x = 2/√π e^{−x²} Σ 2ⁿ x^{2n+1} / (2n+1)!!
        let (mut term, mut sum) = (x, x);
        for n in 1..200 {
            term *= 2.0 * x * x / (2 * n + 1) as f64;
            sum += term;
            if term < sum * 1e-17 {
                break;
            }
        }
        return 1.0 - 2.0 / pi.sqrt() * (-x * x).exp() * sum;
    }
    // erfc x = e^{−x²}/√π · 1/(x + (1/2)/(x + 1/(x + (3/2)/(x + …)))), modified Lentz.
    let tiny = 1e-300;
    let mut f = x;
    let (mut c, mut d) = (x, 0.0);
    for n in 1..300 {
        let a = n as f64 / 2.0;
        d = x + a * d;
        d = if d.abs() < tiny { tiny } else { d };
        c = x + a / c;
        c = if c.abs() < tiny { tiny } else { c };
        d = 1.0 / d;
        let delta = c * d;
        f *= delta;
        if (delta - 1.0).abs() < 1e-16 {
            break;
        }
    }
    (-x * x).exp() / (pi.sqrt() * f)
}

/// Assessment of one approach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Assessment {
    pub miss_km: f64,
    pub relative_speed_km_s: f64,
    /// Secondary − primary in the primary's RSW (RTN) axes.
    pub position_rsw_km: DVec3,
    pub velocity_rsw_km_s: DVec3,
    /// Combined position covariance in the encounter plane, 1σ along the principal axes
    /// (km, major first); `None` without a covariance.
    pub sigma_km: Option<[f64; 2]>,
    /// Foster Pc; `None` without a covariance.
    pub probability: Option<f64>,
    /// [`max_probability`] over the combined covariance's scale (isotropic without one).
    pub max_probability: f64,
}

/// Assesses an approach of `secondary` to `primary` (inertial `(r, v)`), each with an
/// optional inertial covariance; a missing one counts as zero unless both are missing.
pub fn assess(
    primary: (DVec3, DVec3),
    secondary: (DVec3, DVec3),
    primary_covariance: Option<&Matrix6>,
    secondary_covariance: Option<&Matrix6>,
    hard_body_radius_km: f64,
) -> Option<Assessment> {
    let (dr, dv) = (secondary.0 - primary.0, secondary.1 - primary.1);
    let axes = encounter_axes(dr, dv)?;
    let miss = [dr.dot(axes[0]), dr.dot(axes[2])];
    let combined = match (primary_covariance, secondary_covariance) {
        (None, None) => None,
        (a, b) => Some(
            a.map(position_block).unwrap_or(DMat3::ZERO)
                + b.map(position_block).unwrap_or(DMat3::ZERO),
        ),
    };
    let plane = combined.map(|c| encounter_covariance(&c, &axes));
    let rsw = LocalFrame::Rsw
        .to_inertial(primary.0, primary.1)
        .transpose();
    Some(Assessment {
        miss_km: dr.length(),
        relative_speed_km_s: dv.length(),
        position_rsw_km: rsw * dr,
        velocity_rsw_km_s: rsw * dv,
        sigma_km: plane.map(|c| {
            let (a, b, _) = principal(c);
            [a, b]
        }),
        probability: plane.map(|c| probability(miss, c, hard_body_radius_km)),
        max_probability: max_probability(miss, plane, hard_body_radius_km),
    })
}

/// Principal semi-axes (km, descending) of a 3×3 covariance's 1σ ellipsoid.
pub fn sigma_axes(c: &DMat3) -> [f64; 3] {
    let a = c.transpose().to_cols_array_2d();
    let (mut values, _) = symmetric_eigen3(a);
    values.sort_by(|a, b| b.total_cmp(a));
    values.map(|v| v.max(0.0).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erfc_matches_reference_values() {
        for (x, want) in [
            (0.0, 1.0),
            (0.5, 0.479_500_122_186_953_5),
            (1.0, 0.157_299_207_050_285_1),
            (2.0, 4.677_734_981_047_266e-3),
            (3.0, 2.209_049_699_858_544e-5),
            (5.0, 1.537_459_794_428_035e-12),
            (-1.0, 1.842_700_792_949_715),
        ] {
            let got = erfc(x);
            assert!(((got - want) / want).abs() < 1e-13, "erfc({x}) = {got}");
        }
    }

    #[test]
    fn isotropic_head_on_matches_rayleigh() {
        let (sigma, radius): (f64, f64) = (0.1, 0.02);
        let c = [[sigma * sigma, 0.0], [0.0, sigma * sigma]];
        let want = 1.0 - (-radius * radius / (2.0 * sigma * sigma)).exp();
        let got = probability([0.0, 0.0], c, radius);
        assert!(((got - want) / want).abs() < 1e-9, "{got} vs {want}");
    }

    #[test]
    fn small_body_matches_density_times_area_and_rotation() {
        // Correlated, offset: Pc ≈ πR² N(m; C) for R ≪ σ.
        let c: [[f64; 2]; 2] = [[0.04, 0.006], [0.006, 0.0025]];
        let m: [f64; 2] = [0.15, -0.02];
        let r: f64 = 1e-3;
        let det = c[0][0] * c[1][1] - c[0][1] * c[0][1];
        let q = (c[1][1] * m[0] * m[0] - 2.0 * c[0][1] * m[0] * m[1] + c[0][0] * m[1] * m[1]) / det;
        let want = std::f64::consts::PI * r * r * (-0.5 * q).exp()
            / (2.0 * std::f64::consts::PI * det.sqrt());
        let got = probability(m, c, r);
        assert!(((got - want) / want).abs() < 1e-4, "{got} vs {want}");
        // Rotating miss and covariance together leaves Pc unchanged.
        let (s, k) = 0.7f64.sin_cos();
        let rot = [[k, -s], [s, k]];
        let rm = [
            rot[0][0] * m[0] + rot[0][1] * m[1],
            rot[1][0] * m[0] + rot[1][1] * m[1],
        ];
        let rc: [[f64; 2]; 2] = std::array::from_fn(|i| {
            std::array::from_fn(|j| {
                (0..2)
                    .flat_map(|a| (0..2).map(move |b| (a, b)))
                    .map(|(a, b)| rot[i][a] * c[a][b] * rot[j][b])
                    .sum()
            })
        });
        let rotated = probability(rm, rc, r);
        assert!(((rotated - got) / got).abs() < 1e-9, "{rotated} vs {got}");
    }

    #[test]
    fn max_probability_is_alfanos_bound() {
        // Isotropic, R ≪ d: max over σ is R²/(e d²).
        let (d, r) = (1.0, 0.01);
        let want = r * r / (std::f64::consts::E * d * d);
        let got = max_probability([d, 0.0], None, r);
        assert!(((got - want) / want).abs() < 1e-3, "{got} vs {want}");
        let c = [[0.01, 0.0], [0.0, 0.0004]];
        assert!(max_probability([d, 0.0], Some(c), r) >= probability([d, 0.0], c, r));
    }

    #[test]
    fn finds_a_crossing_and_ignores_a_distant_one() {
        // Two straight passes at miss 0.3 km (t = 100 s) and 50 km (t = 400 s).
        let pass = |t: f64, t0: f64, miss: f64| {
            (
                DVec3::new(miss, 7.0 * (t - t0), 0.0),
                DVec3::new(0.0, 7.0, 0.0),
            )
        };
        let mut rel = |t: f64| {
            Some(if t < 250.0 {
                pass(t, 100.0, 0.3)
            } else {
                pass(t, 400.0, 50.0)
            })
        };
        let found = closest_approaches(&mut rel, 500.0, 30.0, 5.0, 1e-6);
        assert_eq!(found.len(), 1);
        assert!((found[0].t_s - 100.0).abs() < 1e-5 && (found[0].miss_km() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn assesses_in_primary_rsw() {
        let primary = (DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 7.5, 0.0));
        let secondary = (DVec3::new(7000.2, 0.0, 0.0), DVec3::new(0.0, 0.0, 7.5));
        let p = crate::covariance::diagonal([0.01, 0.04, 0.0025, 0.0, 0.0, 0.0]);
        let a = assess(primary, secondary, Some(&p), None, 0.01).unwrap();
        assert!((a.position_rsw_km - DVec3::new(0.2, 0.0, 0.0)).length() < 1e-12);
        assert!((a.miss_km - 0.2).abs() < 1e-12);
        let pc = a.probability.unwrap();
        assert!(pc > 0.0 && pc <= a.max_probability * (1.0 + 1e-6));
        // Encounter plane spans radial and the in-track/cross-track mix normal to Δv.
        let sigma = a.sigma_km.unwrap();
        assert!(sigma[0] >= sigma[1] && sigma[1] > 0.0);
    }
}
