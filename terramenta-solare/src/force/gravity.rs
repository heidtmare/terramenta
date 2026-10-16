//! Central-body gravity: point mass and fully normalised spherical harmonics.
//!
//! Harmonics use the Cunningham V/W recursion (Montenbruck & Gill, *Satellite Orbits*, §3.2.4)
//! in fully normalised form; singularity-free at the poles.

use glam::{DMat3, DVec3};

use super::{ForceContext, ForceModel};

/// Fully normalised geopotential coefficients.
#[derive(Debug, Clone, PartialEq)]
pub struct GravityField {
    pub gm_km3_s2: f64,
    pub radius_km: f64,
    pub max_degree: usize,
    /// Index `n(n+1)/2 + m`.
    c: Vec<f64>,
    s: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GfcError {
    MissingHeader(&'static str),
    Parse { line: usize },
}

impl std::fmt::Display for GfcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingHeader(k) => write!(f, "gfc header missing {k}"),
            Self::Parse { line } => write!(f, "gfc parse error at line {line}"),
        }
    }
}

impl std::error::Error for GfcError {}

/// EGM2008 GM, km³/s² (`earth_gravity_constant` of the embedded `.gfc`).
pub const EGM2008_GM_KM3_S2: f64 = 398_600.441_5;

/// Largest V̄/W̄ degree held on the stack: embedded field's 20, +1 for acceleration.
const STACK_DEGREE: usize = 21;

/// EGM2008, degree/order ≤ 20, tide-free.
const EGM2008_20: &str = include_str!("egm2008_20.gfc");
/// GRGM900C (Lemoine et al. 2014), degree/order ≤ 20, lunar principal axes.
const GRGM900C_20: &str = include_str!("grgm900c_20.sha");
/// JGMRO120F (Konopliv et al. 2020), degree/order ≤ 20, Mars-fixed.
const JGMRO120F_20: &str = include_str!("jgmro120f_20.sha");

const fn idx(n: usize, m: usize) -> usize {
    n * (n + 1) / 2 + m
}

impl GravityField {
    pub fn point_mass(gm_km3_s2: f64, radius_km: f64) -> Self {
        Self {
            gm_km3_s2,
            radius_km,
            max_degree: 0,
            c: vec![1.0],
            s: vec![0.0],
        }
    }

    /// Embedded EGM2008, truncated to `degree` ≤ 20.
    pub fn egm2008(degree: usize) -> Self {
        assert!(
            degree <= 20,
            "embedded EGM2008 is degree 20; load a .gfc for more"
        );
        Self::from_gfc(EGM2008_20, degree).expect("embedded EGM2008")
    }

    /// Embedded GRGM900C (Moon, principal axes), truncated to `degree` ≤ 20.
    pub fn grgm900c(degree: usize) -> Self {
        assert!(
            degree <= 20,
            "embedded GRGM900C is degree 20; load a SHADR for more"
        );
        Self::from_shadr(GRGM900C_20, degree).expect("embedded GRGM900C")
    }

    /// Embedded JGMRO120F (Mars), truncated to `degree` ≤ 20.
    pub fn jgmro120f(degree: usize) -> Self {
        assert!(
            degree <= 20,
            "embedded JGMRO120F is degree 20; load a SHADR for more"
        );
        Self::from_shadr(JGMRO120F_20, degree).expect("embedded JGMRO120F")
    }

    /// PDS SHADR table (`*_sha.tab`), truncated to `max_degree`. Header: radius km, GM km³/s²,
    /// σGM, degree, order, normalisation (1), ref lon, ref lat; rows `n, m, C, S, σC, σS`.
    pub fn from_shadr(text: &str, max_degree: usize) -> Result<Self, GfcError> {
        let fields = |line: &str| -> Vec<f64> {
            line.split(',')
                .filter_map(|v| parse_f64(v.trim()))
                .collect()
        };
        let mut lines = text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty());
        let head = lines
            .next()
            .map(|(_, l)| fields(l))
            .filter(|h| h.len() >= 6)
            .ok_or(GfcError::MissingHeader("SHADR header"))?;
        if head[5] != 1.0 {
            return Err(GfcError::MissingHeader("normalised coefficients"));
        }
        let degree = max_degree.min(head[3] as usize);
        let mut field = Self {
            gm_km3_s2: head[1],
            radius_km: head[0],
            max_degree: degree,
            c: vec![0.0; idx(degree, degree) + 1],
            s: vec![0.0; idx(degree, degree) + 1],
        };
        field.c[0] = 1.0;
        for (i, line) in lines {
            let row = fields(line);
            let [n, m, c, s, ..] = row[..] else {
                return Err(GfcError::Parse { line: i + 1 });
            };
            let (n, m) = (n as usize, m as usize);
            if n > degree {
                break;
            }
            if m <= n {
                field.c[idx(n, m)] = c;
                field.s[idx(n, m)] = s;
            }
        }
        Ok(field)
    }

    /// ICGEM `.gfc` (fully normalised), truncated to `max_degree`.
    pub fn from_gfc(text: &str, max_degree: usize) -> Result<Self, GfcError> {
        let mut gm = None;
        let mut radius = None;
        let mut file_degree = None;
        let mut lines = text.lines().enumerate();
        for (_, line) in lines.by_ref() {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("earth_gravity_constant") => gm = it.next().and_then(parse_f64),
                Some("radius") => radius = it.next().and_then(parse_f64),
                Some("max_degree") => file_degree = it.next().and_then(|v| v.parse::<usize>().ok()),
                Some("end_of_head") => break,
                _ => {}
            }
        }
        let gm = gm.ok_or(GfcError::MissingHeader("earth_gravity_constant"))?;
        let radius = radius.ok_or(GfcError::MissingHeader("radius"))?;
        let degree = max_degree.min(file_degree.unwrap_or(max_degree));
        let mut field = Self {
            gm_km3_s2: gm * 1e-9,
            radius_km: radius * 1e-3,
            max_degree: degree,
            c: vec![0.0; idx(degree, degree) + 1],
            s: vec![0.0; idx(degree, degree) + 1],
        };
        for (i, line) in lines {
            let mut it = line.split_whitespace();
            if it.next() != Some("gfc") {
                continue;
            }
            let err = || GfcError::Parse { line: i + 1 };
            let n: usize = it.next().and_then(|v| v.parse().ok()).ok_or_else(err)?;
            let m: usize = it.next().and_then(|v| v.parse().ok()).ok_or_else(err)?;
            let c = it.next().and_then(parse_f64).ok_or_else(err)?;
            let s = it.next().and_then(parse_f64).ok_or_else(err)?;
            if n <= degree && m <= n {
                field.c[idx(n, m)] = c;
                field.s[idx(n, m)] = s;
            }
        }
        Ok(field)
    }

    /// `(C̄nm, S̄nm)`.
    pub fn coefficient(&self, n: usize, m: usize) -> (f64, f64) {
        if n > self.max_degree || m > n {
            return (0.0, 0.0);
        }
        (self.c[idx(n, m)], self.s[idx(n, m)])
    }

    /// Unnormalised zonal `Jn = −√(2n+1) C̄n0`.
    pub fn j(&self, n: usize) -> f64 {
        -((2 * n + 1) as f64).sqrt() * self.coefficient(n, 0).0
    }

    /// `f(V̄, W̄)`; stack buffers up to degree [`STACK_DEGREE`], heap above.
    fn with_vw<T>(
        &self,
        r: DVec3,
        n_max: usize,
        m_max: usize,
        f: impl FnOnce(&[f64], &[f64]) -> T,
    ) -> T {
        const LEN: usize = idx(STACK_DEGREE, STACK_DEGREE) + 1;
        let size = idx(n_max, n_max) + 1;
        if size <= LEN {
            let (mut v, mut w) = ([0.0; LEN], [0.0; LEN]);
            let (v, w) = (&mut v[..size], &mut w[..size]);
            self.vw(r, n_max, m_max, v, w);
            f(v, w)
        } else {
            let (mut v, mut w) = (vec![0.0; size], vec![0.0; size]);
            self.vw(r, n_max, m_max, &mut v, &mut w);
            f(&v, &w)
        }
    }

    /// Normalised V̄/W̄ to degree `n_max`, order `m_max`, at body-fixed `r`, into zeroed `v`, `w`.
    fn vw(&self, r: DVec3, n_max: usize, m_max: usize, v: &mut [f64], w: &mut [f64]) {
        let r2 = r.length_squared();
        let re = self.radius_km;
        let (x0, y0, z0, rho) = (r.x * re / r2, r.y * re / r2, r.z * re / r2, re * re / r2);
        v[0] = re / r2.sqrt();
        for m in 0..=m_max.min(n_max) {
            if m > 0 {
                let k = if m == 1 { 2.0 } else { 1.0 };
                let f = (k * (2 * m + 1) as f64 / (2 * m) as f64).sqrt();
                let (vp, wp) = (v[idx(m - 1, m - 1)], w[idx(m - 1, m - 1)]);
                v[idx(m, m)] = f * (x0 * vp - y0 * wp);
                w[idx(m, m)] = f * (x0 * wp + y0 * vp);
            }
            for n in (m + 1)..=n_max {
                let (nf, mf) = (n as f64, m as f64);
                let a = ((2.0 * nf + 1.0) * (2.0 * nf - 1.0) / ((nf - mf) * (nf + mf))).sqrt();
                let (v1, w1) = (v[idx(n - 1, m)], w[idx(n - 1, m)]);
                let (mut vn, mut wn) = (a * z0 * v1, a * z0 * w1);
                if n >= m + 2 {
                    let b = ((2.0 * nf + 1.0) * (nf + mf - 1.0) * (nf - mf - 1.0)
                        / ((2.0 * nf - 3.0) * (nf + mf) * (nf - mf)))
                        .sqrt();
                    vn -= b * rho * v[idx(n - 2, m)];
                    wn -= b * rho * w[idx(n - 2, m)];
                }
                v[idx(n, m)] = vn;
                w[idx(n, m)] = wn;
            }
        }
    }

    /// Potential at body-fixed `r` (km), km²/s². Truncated to (`degree`, `order`).
    pub fn potential(&self, r: DVec3, degree: usize, order: usize) -> f64 {
        let degree = degree.min(self.max_degree);
        let u = self.with_vw(r, degree, order, |v, w| {
            let mut u = 0.0;
            for n in 0..=degree {
                for m in 0..=n.min(order) {
                    let (c, s) = self.coefficient(n, m);
                    u += c * v[idx(n, m)] + s * w[idx(n, m)];
                }
            }
            u
        });
        self.gm_km3_s2 / self.radius_km * u
    }

    /// Acceleration at body-fixed `r` (km), km/s², body-fixed axes.
    pub fn acceleration(&self, r: DVec3, degree: usize, order: usize) -> DVec3 {
        let degree = degree.min(self.max_degree);
        let order = order.min(degree);
        let (ax, ay, az) = self.with_vw(r, degree + 1, order + 1, |v, w| {
            let (mut ax, mut ay, mut az) = (0.0, 0.0, 0.0);
            for n in 0..=degree {
                let nf = n as f64;
                let q = (2.0 * nf + 1.0) / (2.0 * nf + 3.0);
                for m in 0..=n.min(order) {
                    let (c, s) = self.coefficient(n, m);
                    if c == 0.0 && s == 0.0 {
                        continue;
                    }
                    let mf = m as f64;
                    let f3 = (q * (nf - mf + 1.0) * (nf + mf + 1.0)).sqrt();
                    az += f3 * (-c * v[idx(n + 1, m)] - s * w[idx(n + 1, m)]);
                    if m == 0 {
                        let f1 = ((2.0 * nf + 1.0) * (nf + 1.0) * (nf + 2.0)
                            / (2.0 * (2.0 * nf + 3.0)))
                            .sqrt();
                        ax -= c * f1 * v[idx(n + 1, 1)];
                        ay -= c * f1 * w[idx(n + 1, 1)];
                    } else {
                        let f1 = (q * (nf + mf + 1.0) * (nf + mf + 2.0)).sqrt();
                        let k = if m == 1 { 2.0 } else { 1.0 };
                        let f2 = (k * q * (nf - mf + 2.0) * (nf - mf + 1.0)).sqrt();
                        let (vp, wp) = (v[idx(n + 1, m + 1)], w[idx(n + 1, m + 1)]);
                        let (vm, wm) = (v[idx(n + 1, m - 1)], w[idx(n + 1, m - 1)]);
                        ax += 0.5 * (f1 * (-c * vp - s * wp) + f2 * (c * vm + s * wm));
                        ay += 0.5 * (f1 * (-c * wp + s * vp) + f2 * (-c * wm + s * vm));
                    }
                }
            }
            (ax, ay, az)
        });
        DVec3::new(ax, ay, az) * (self.gm_km3_s2 / (self.radius_km * self.radius_km))
    }
}

fn parse_f64(s: &str) -> Option<f64> {
    s.replace(['d', 'D'], "e").parse().ok()
}

/// Central-body point mass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointMass {
    pub gm_km3_s2: f64,
}

impl ForceModel for PointMass {
    fn acceleration(&self, ctx: &ForceContext) -> DVec3 {
        let r = ctx.position_km;
        -self.gm_km3_s2 * r / r.length().powi(3)
    }

    /// `−μ/r³ (I − 3 r̂r̂ᵀ)`.
    fn partials(&self, ctx: &ForceContext) -> Option<(DMat3, DMat3)> {
        let r = ctx.position_km;
        let u = r.normalize();
        let outer = DMat3::from_cols(u * u.x, u * u.y, u * u.z);
        let g = (DMat3::IDENTITY - 3.0 * outer) * (-self.gm_km3_s2 / r.length().powi(3));
        Some((g, DMat3::ZERO))
    }

    fn velocity_dependent(&self) -> bool {
        false
    }
}

/// Central-body spherical harmonics, evaluated in the body-fixed frame. Includes the
/// central term (`C̄00`).
#[derive(Debug, Clone, PartialEq)]
pub struct SphericalHarmonics {
    pub field: GravityField,
    pub degree: usize,
    pub order: usize,
}

impl SphericalHarmonics {
    pub fn new(field: GravityField, degree: usize, order: usize) -> Self {
        let degree = degree.min(field.max_degree);
        Self {
            field,
            degree,
            order: order.min(degree),
        }
    }

    /// EGM2008 `degree × order` (≤ 20).
    pub fn egm2008(degree: usize, order: usize) -> Self {
        Self::new(GravityField::egm2008(degree), degree, order)
    }

    /// GRGM900C `degree × order` (≤ 20); needs lunar principal axes
    /// ([`crate::iau::IauOrientation::moon_pa`]).
    pub fn grgm900c(degree: usize, order: usize) -> Self {
        Self::new(GravityField::grgm900c(degree), degree, order)
    }

    /// JGMRO120F `degree × order` (≤ 20); needs Mars-fixed axes
    /// ([`crate::iau::IauOrientation::mars`]).
    pub fn jgmro120f(degree: usize, order: usize) -> Self {
        Self::new(GravityField::jgmro120f(degree), degree, order)
    }
}

impl ForceModel for SphericalHarmonics {
    fn acceleration(&self, ctx: &ForceContext) -> DVec3 {
        let rot = ctx.body_rotation();
        let a_bf = self
            .field
            .acceleration(rot * ctx.position_km, self.degree, self.order);
        rot.transpose() * a_bf
    }

    fn requires_body_fixed(&self) -> bool {
        self.degree >= 2
    }

    fn velocity_dependent(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field() -> GravityField {
        GravityField::egm2008(20)
    }

    #[test]
    fn parses_embedded_egm2008() {
        let f = field();
        assert_eq!(f.gm_km3_s2, EGM2008_GM_KM3_S2);
        assert!((f.radius_km - 6378.1363).abs() < 1e-12);
        assert_eq!(f.coefficient(2, 0).0, -0.484165143790815e-03);
        assert_eq!(f.coefficient(20, 20).1, -0.126949126479726e-07);
        assert!((f.j(2) - 1.082_626_173_85e-3).abs() < 1e-14);
    }

    #[test]
    #[allow(clippy::excessive_precision)] // file values verbatim
    fn parses_embedded_shadr() {
        let moon = GravityField::grgm900c(20);
        assert_eq!(moon.gm_km3_s2, 4902.79996708864);
        assert_eq!(moon.radius_km, 1738.0);
        assert_eq!(moon.coefficient(0, 0).0, 1.0);
        assert_eq!(moon.coefficient(2, 0).0, -9.0886616361343905e-05);
        assert_eq!(moon.coefficient(20, 20).1, -3.5878114300843799e-07);
        let mars = GravityField::jgmro120f(4);
        assert_eq!(mars.max_degree, 4);
        assert_eq!(mars.coefficient(2, 0).0, -0.8750219819894000E-03);
        assert_eq!(mars.coefficient(5, 0), (0.0, 0.0));
        // Mars J2 ≈ 1.956e-3.
        assert!((mars.j(2) - 1.9566e-3).abs() < 1e-6, "{}", mars.j(2));
    }

    #[test]
    fn point_mass_limit() {
        let f = GravityField::point_mass(398_600.441_5, 6378.1363);
        let r = DVec3::new(7000.0, -300.0, 1200.0);
        let a = f.acceleration(r, 0, 0);
        let expect = -398_600.441_5 * r / r.length().powi(3);
        assert!((a - expect).length() < 1e-15 * expect.length());
    }

    /// Independent potential: Σ (R/r)^n P̄nm(sin φ)(C̄ cos mλ + S̄ sin mλ), with P̄nm from the
    /// standard column recursion.
    #[allow(clippy::needless_range_loop)]
    fn reference_potential(f: &GravityField, r: DVec3, degree: usize) -> f64 {
        let rr = r.length();
        let (sphi, cphi) = (r.z / rr, r.x.hypot(r.y) / rr);
        let lam = r.y.atan2(r.x);
        let mut p = vec![vec![0.0; degree + 1]; degree + 1];
        p[0][0] = 1.0;
        for m in 0..=degree {
            if m > 0 {
                let k = if m == 1 {
                    3.0
                } else {
                    (2 * m + 1) as f64 / (2 * m) as f64
                };
                p[m][m] = k.sqrt() * cphi * p[m - 1][m - 1];
            }
            for n in (m + 1)..=degree {
                let (nf, mf) = (n as f64, m as f64);
                let a = ((2.0 * nf - 1.0) * (2.0 * nf + 1.0) / ((nf - mf) * (nf + mf))).sqrt();
                let b = if n >= m + 2 {
                    ((2.0 * nf + 1.0) * (nf + mf - 1.0) * (nf - mf - 1.0)
                        / ((nf - mf) * (nf + mf) * (2.0 * nf - 3.0)))
                        .sqrt()
                } else {
                    0.0
                };
                p[n][m] = a * sphi * p[n - 1][m] - if n >= 2 { b * p[n - 2][m] } else { 0.0 };
            }
        }
        let mut u = 0.0;
        for n in 0..=degree {
            let ratio = (f.radius_km / rr).powi(n as i32);
            for m in 0..=n {
                let (c, s) = f.coefficient(n, m);
                let ml = m as f64 * lam;
                u += ratio * p[n][m] * (c * ml.cos() + s * ml.sin());
            }
        }
        f.gm_km3_s2 / rr * u
    }

    #[test]
    fn potential_matches_legendre_reference() {
        let f = field();
        for r in [
            DVec3::new(6900.0, 1200.0, -2500.0),
            DVec3::new(-100.0, 50.0, 7100.0),
            DVec3::new(-26_000.0, 30_000.0, 4000.0),
        ] {
            let u = f.potential(r, 20, 20);
            let u_ref = reference_potential(&f, r, 20);
            assert!(((u - u_ref) / u_ref).abs() < 1e-13, "{r}: {u} vs {u_ref}");
        }
    }

    #[test]
    fn acceleration_is_potential_gradient() {
        let f = field();
        for r in [
            DVec3::new(6900.0, 1200.0, -2500.0),
            DVec3::new(1.0, -2.0, 6800.0),
            DVec3::new(-4000.0, -5000.0, 1000.0),
        ] {
            // Non-central part only, to keep finite-difference rounding below 1e-15.
            let a = f.acceleration(r, 20, 20) + f.gm_km3_s2 * r / r.length().powi(3);
            let u = |p: DVec3| f.potential(p, 20, 20) - f.gm_km3_s2 / p.length();
            let h = 1e-2;
            let d = |e: DVec3| (u(r + e * h) - u(r - e * h)) / (2.0 * h);
            let grad = DVec3::new(d(DVec3::X), d(DVec3::Y), d(DVec3::Z));
            assert!(
                (a - grad).length() < 1e-6 * a.length(),
                "{r}: {a} vs {grad}"
            );
        }
    }

    #[test]
    fn j2_matches_closed_form() {
        let f = field();
        let r = DVec3::new(5000.0, 2000.0, 4000.0);
        let a = f.acceleration(r, 2, 0) + f.gm_km3_s2 * r / r.length().powi(3);
        let (rr, j2, re) = (r.length(), f.j(2), f.radius_km);
        let k = -1.5 * j2 * f.gm_km3_s2 * re * re / rr.powi(5);
        let z2 = 5.0 * r.z * r.z / (rr * rr);
        let expect = DVec3::new(
            k * r.x * (1.0 - z2),
            k * r.y * (1.0 - z2),
            k * r.z * (3.0 - z2),
        );
        assert!((a - expect).length() < 1e-17, "{a} vs {expect}");
    }
}
