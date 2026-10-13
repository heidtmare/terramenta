//! Earth orientation: GCRF/TEME ↔ ITRF.
//!
//! Models (SOFA via `sofars`):
//! - [`EarthModel::Iau76Fk5`]: IAU-1976 precession, IAU-1980 nutation + δΔψ/δΔε, GMST82 + eqeq94.
//!   Vallado, *Fundamentals of Astrodynamics*, 4th ed., §3.7.
//! - [`EarthModel::Iau2006Cio`]: IAU-2006/2000A, CIO-based (X, Y, s) + δX/δY, ERA. IERS TN36 ch. 5.
//!
//! Chain: `r_itrf = W · R(θ) · Q · r_inertial`;
//! `v_itrf = W · (R(θ) · Q · v_inertial − ω × r_tirs)`, with `ω = Ω_E (1 − LOD/86400) ẑ`.

use glam::{DMat3, DVec3};

use crate::eop::Eop;
use crate::frame::StateVector;
use crate::time::Epoch;

/// Nominal Earth rotation rate, rad/s (IERS).
pub const OMEGA_EARTH_RAD_S: f64 = 7.292_115_146_706_979e-5;
/// WGS-84 semi-major axis, km.
pub const WGS84_A_KM: f64 = 6378.137;
/// WGS-84 flattening.
pub const WGS84_F: f64 = 1.0 / 298.257_223_563;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EarthModel {
    /// Equinox-based IAU-76/FK5.
    Iau76Fk5,
    /// CIO-based IAU-2006/2000A.
    #[default]
    Iau2006Cio,
}

/// Inertial → ITRF transform at one epoch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerrestrialTransform {
    /// Inertial → TIRS/PEF (includes Earth rotation angle).
    pub inertial_to_tirs: DMat3,
    /// TIRS/PEF → ITRF (polar motion, W).
    pub polar_motion: DMat3,
    /// Earth angular velocity in TIRS, rad/s.
    pub omega_rad_s: f64,
}

impl TerrestrialTransform {
    pub fn rotation(&self) -> DMat3 {
        self.polar_motion * self.inertial_to_tirs
    }

    pub fn to_itrf(&self, s: StateVector) -> StateVector {
        let r_tirs = self.inertial_to_tirs * s.position_km;
        let v_tirs = self.inertial_to_tirs * s.velocity_km_s - self.omega().cross(r_tirs);
        StateVector::new(self.polar_motion * r_tirs, self.polar_motion * v_tirs)
    }

    pub fn from_itrf(&self, s: StateVector) -> StateVector {
        let w_t = self.polar_motion.transpose();
        let q_t = self.inertial_to_tirs.transpose();
        let r_tirs = w_t * s.position_km;
        let v_tirs = w_t * s.velocity_km_s + self.omega().cross(r_tirs);
        StateVector::new(q_t * r_tirs, q_t * v_tirs)
    }

    /// Rotates by `ω·dt` about the TIRS pole, holding precession–nutation and polar motion.
    pub fn advanced_by(&self, dt_s: f64) -> Self {
        Self {
            inertial_to_tirs: r3(self.omega_rad_s * dt_s) * self.inertial_to_tirs,
            ..*self
        }
    }

    fn omega(&self) -> DVec3 {
        DVec3::new(0.0, 0.0, self.omega_rad_s)
    }
}

/// GCRF → ITRF.
pub fn gcrf_to_itrf(epoch: Epoch, eop: &Eop, model: EarthModel) -> TerrestrialTransform {
    let (tt1, tt2) = epoch.tt_jd();
    let (ut1, ut2) = epoch.ut1_jd(eop.ut1_minus_utc);
    let (inertial_to_tirs, sp) = match model {
        EarthModel::Iau76Fk5 => {
            let p = sofars::pnp::pmat76(tt1, tt2);
            let (dpsi, deps) = sofars::pnp::nut80(tt1, tt2);
            let epsa = sofars::pnp::obl80(tt1, tt2);
            let n = sofars::pnp::numat(epsa, dpsi + eop.dpsi, deps + eop.deps);
            let gast = gast_from(epoch, eop.ut1_minus_utc, tt1, tt2, dpsi + eop.dpsi, epsa);
            (r3(gast) * mat(n) * mat(p), 0.0)
        }
        EarthModel::Iau2006Cio => {
            let (x, y, s) = sofars::pnp::xys06a(tt1, tt2);
            let c2i = sofars::pnp::c2ixys(x + eop.dx, y + eop.dy, s);
            let era = sofars::erst::era00(ut1, ut2);
            (r3(era) * mat(c2i), sofars::pnp::sp00(tt1, tt2))
        }
    };
    TerrestrialTransform {
        inertial_to_tirs,
        polar_motion: mat(sofars::pnp::pom00(eop.xp, eop.yp, sp)),
        omega_rad_s: OMEGA_EARTH_RAD_S * (1.0 - eop.lod / 86_400.0),
    }
}

/// TEME (SGP4 output) → ITRF. Vallado, *Revisiting Spacetrack Report #3*, AIAA 2006-6753.
pub fn teme_to_itrf(epoch: Epoch, eop: &Eop) -> TerrestrialTransform {
    TerrestrialTransform {
        inertial_to_tirs: r3(gmst82(epoch, eop.ut1_minus_utc)),
        polar_motion: mat(sofars::pnp::pom00(eop.xp, eop.yp, 0.0)),
        omega_rad_s: OMEGA_EARTH_RAD_S * (1.0 - eop.lod / 86_400.0),
    }
}

/// TEME → GCRF (IAU-76/FK5 chain). Vallado AIAA 2006-6753 eq. 1.
pub fn teme_to_gcrf(epoch: Epoch, eop: &Eop) -> DMat3 {
    let fk5 = gcrf_to_itrf(epoch, eop, EarthModel::Iau76Fk5);
    // GCRF → PEF via FK5, then PEF → TEME via GMST: TEME = R3(−GMST)·FK5.
    (r3(-gmst82(epoch, eop.ut1_minus_utc)) * fk5.inertial_to_tirs).transpose()
}

/// Greenwich mean sidereal time (IAU-1982), rad.
pub fn gmst82(epoch: Epoch, ut1_minus_utc: f64) -> f64 {
    let (ut1, ut2) = epoch.ut1_jd(ut1_minus_utc);
    sofars::erst::gmst82(ut1, ut2)
}

/// Greenwich apparent sidereal time, IAU-76/FK5 with δΔψ, rad.
pub fn gast82(epoch: Epoch, eop: &Eop) -> f64 {
    let (tt1, tt2) = epoch.tt_jd();
    let (dpsi, _) = sofars::pnp::nut80(tt1, tt2);
    let epsa = sofars::pnp::obl80(tt1, tt2);
    gast_from(epoch, eop.ut1_minus_utc, tt1, tt2, dpsi + eop.dpsi, epsa)
}

/// GMST82 + eqeq94 from precomputed total Δψ (incl. δΔψ) and mean obliquity; avoids
/// `eqeq94` re-evaluating `nut80`/`obl80`.
fn gast_from(epoch: Epoch, ut1_minus_utc: f64, tt1: f64, tt2: f64, dpsi: f64, epsa: f64) -> f64 {
    const AS2R: f64 = std::f64::consts::PI / 648_000.0;
    let t = ((tt1 - 2_451_545.0) + tt2) / 36_525.0;
    // eqeq94 complementary terms; Ω mod 2π is irrelevant under sin.
    let om = (450_160.280 + (-482_890.539 + (7.455 + 0.008 * t) * t) * t) * AS2R
        + (-5.0 * t % 1.0) * std::f64::consts::TAU;
    let ee = dpsi * epsa.cos() + AS2R * (0.002_64 * om.sin() + 0.000_063 * (2.0 * om).sin());
    (gmst82(epoch, ut1_minus_utc) + ee).rem_euclid(std::f64::consts::TAU)
}

/// Earth rotation angle (IAU-2000), rad.
pub fn era(epoch: Epoch, ut1_minus_utc: f64) -> f64 {
    let (ut1, ut2) = epoch.ut1_jd(ut1_minus_utc);
    sofars::erst::era00(ut1, ut2)
}

/// Geodetic (WGS-84) → ECEF, km. Latitude/longitude rad, height km.
pub fn geodetic_to_ecef(lat: f64, lon: f64, h_km: f64) -> DVec3 {
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let (sl, cl) = lat.sin_cos();
    let n = WGS84_A_KM / (1.0 - e2 * sl * sl).sqrt();
    DVec3::new(
        (n + h_km) * cl * lon.cos(),
        (n + h_km) * cl * lon.sin(),
        (n * (1.0 - e2) + h_km) * sl,
    )
}

/// ECEF → geodetic (WGS-84), `(lat rad, lon rad, h km)`. Bowring, 3 iterations (< 1 mm for |h| < 1e4 km).
pub fn ecef_to_geodetic(r: DVec3) -> (f64, f64, f64) {
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let p = r.x.hypot(r.y);
    let lon = r.y.atan2(r.x);
    let mut lat = r.z.atan2(p * (1.0 - e2));
    let mut n = WGS84_A_KM;
    for _ in 0..3 {
        let sl = lat.sin();
        n = WGS84_A_KM / (1.0 - e2 * sl * sl).sqrt();
        lat = (r.z + e2 * n * sl).atan2(p);
    }
    let cl = lat.cos();
    let h = if cl.abs() > 1e-10 {
        p / cl - n
    } else {
        r.z.abs() - n * (1.0 - e2)
    };
    (lat, lon, h)
}

/// Passive rotation about z.
fn r3(a: f64) -> DMat3 {
    let (s, c) = a.sin_cos();
    DMat3::from_cols(DVec3::new(c, -s, 0.0), DVec3::new(s, c, 0.0), DVec3::Z)
}

/// SOFA row-major → glam.
fn mat(m: [[f64; 3]; 3]) -> DMat3 {
    DMat3::from_cols_array_2d(&m).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r3_matches_sofa() {
        let mut m = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        sofars::vm::rz(0.3, &mut m);
        assert!((mat(m) - r3(0.3)).abs_diff_eq(DMat3::ZERO, 1e-15));
    }

    #[test]
    fn gast82_matches_sofa_eqeq94() {
        let eop = Eop {
            dpsi: -5.0e-7,
            ut1_minus_utc: 0.3,
            ..Eop::default()
        };
        for epoch in [
            Epoch::from_utc(1992, 8, 20, 12, 14, 0.0),
            Epoch::from_utc(2024, 6, 20, 12, 0, 0.0),
            Epoch::from_utc(2049, 1, 1, 0, 0, 0.0),
        ] {
            let (tt1, tt2) = epoch.tt_jd();
            let ee = sofars::erst::eqeq94(tt1, tt2) + eop.dpsi * sofars::pnp::obl80(tt1, tt2).cos();
            let sofa = (gmst82(epoch, eop.ut1_minus_utc) + ee).rem_euclid(std::f64::consts::TAU);
            assert!((gast82(epoch, &eop) - sofa).abs() < 1e-14);
        }
    }

    #[test]
    fn geodetic_round_trip() {
        for (lat, lon, h) in [
            (0.7, -1.2, 0.5),
            (-1.5, 3.0, 400.0),
            (0.0, 0.0, 0.0),
            (1.2, 0.1, -0.4),
        ] {
            let (la, lo, hh) = ecef_to_geodetic(geodetic_to_ecef(lat, lon, h));
            assert!((la - lat).abs() < 1e-12 && (lo - lon).abs() < 1e-12 && (hh - h).abs() < 1e-9);
        }
    }

    #[test]
    fn transform_round_trips() {
        let epoch = Epoch::from_utc(2024, 3, 1, 6, 0, 0.0);
        let eop = Eop {
            xp: 1e-6,
            yp: 2e-6,
            ut1_minus_utc: -0.2,
            lod: 1e-3,
            ..Eop::default()
        };
        let s = StateVector::new(
            DVec3::new(7000.0, -1200.0, 300.0),
            DVec3::new(1.0, 7.0, -0.5),
        );
        for model in [EarthModel::Iau76Fk5, EarthModel::Iau2006Cio] {
            let t = gcrf_to_itrf(epoch, &eop, model);
            let back = t.from_itrf(t.to_itrf(s));
            assert!((back.position_km - s.position_km).length() < 1e-9);
            assert!((back.velocity_km_s - s.velocity_km_s).length() < 1e-12);
        }
    }
}
