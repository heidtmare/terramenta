//! Atmospheric drag, co-rotating atmosphere.
//!
//! `a = −½ ρ (C_D A / m) |v_r| v_r`, `v_r` = velocity relative to the body-fixed frame.

use std::sync::Arc;

use glam::DVec3;

use super::{ForceContext, ForceModel};
use crate::earth::ecef_to_geodetic;
use crate::frame::{Ephemeris, StateVector};
use crate::time::Epoch;

/// Inputs to [`Atmosphere::density`].
#[derive(Debug, Clone, Copy)]
pub struct AtmosphereSample {
    pub epoch: Epoch,
    pub position_inertial_km: DVec3,
    pub position_body_fixed_km: DVec3,
    /// WGS-84 geodetic latitude, longitude (rad), height (km).
    pub geodetic: (f64, f64, f64),
}

pub trait Atmosphere: std::fmt::Debug + Send + Sync {
    /// kg/m³.
    fn density(&self, s: &AtmosphereSample) -> f64;
}

/// Vallado, *Fundamentals of Astrodynamics*, 4th ed., Table 8-4. Static, 0–1000 km.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExponentialAtmosphere;

/// `(h0 km, ρ0 kg/m³, H km)`.
const VALLADO_EXP: [(f64, f64, f64); 28] = [
    (0.0, 1.225, 7.249),
    (25.0, 3.899e-2, 6.349),
    (30.0, 1.774e-2, 6.682),
    (40.0, 3.972e-3, 7.554),
    (50.0, 1.057e-3, 8.382),
    (60.0, 3.206e-4, 7.714),
    (70.0, 8.770e-5, 6.549),
    (80.0, 1.905e-5, 5.799),
    (90.0, 3.396e-6, 5.382),
    (100.0, 5.297e-7, 5.877),
    (110.0, 9.661e-8, 7.263),
    (120.0, 2.438e-8, 9.473),
    (130.0, 8.484e-9, 12.636),
    (140.0, 3.845e-9, 16.149),
    (150.0, 2.070e-9, 22.523),
    (180.0, 5.464e-10, 29.740),
    (200.0, 2.789e-10, 37.105),
    (250.0, 7.248e-11, 45.546),
    (300.0, 2.418e-11, 53.628),
    (350.0, 9.518e-12, 53.298),
    (400.0, 3.725e-12, 58.515),
    (450.0, 1.585e-12, 60.828),
    (500.0, 6.967e-13, 63.822),
    (600.0, 1.454e-13, 71.835),
    (700.0, 3.614e-14, 88.667),
    (800.0, 1.170e-14, 124.64),
    (900.0, 5.245e-15, 181.05),
    (1000.0, 3.019e-15, 268.00),
];

impl ExponentialAtmosphere {
    pub fn density_at_height(h_km: f64) -> f64 {
        if h_km < 0.0 {
            return VALLADO_EXP[0].1;
        }
        let i = VALLADO_EXP
            .partition_point(|row| row.0 <= h_km)
            .saturating_sub(1);
        let (h0, rho0, scale) = VALLADO_EXP[i];
        rho0 * (-(h_km - h0) / scale).exp()
    }
}

impl Atmosphere for ExponentialAtmosphere {
    fn density(&self, s: &AtmosphereSample) -> f64 {
        Self::density_at_height(s.geodetic.2)
    }
}

/// Harris–Priester, mean solar activity (Montenbruck & Gill, *Satellite Orbits*, §3.5.2,
/// Table 3.8). 100–1000 km; below, [`ExponentialAtmosphere`] scaled to match at 100 km;
/// zero above.
#[derive(Debug, Clone)]
pub struct HarrisPriester {
    /// Sun relative to the central body.
    pub sun: Arc<dyn Ephemeris>,
    /// `n` in `cos^n(ψ/2)`: 2 low inclination … 6 polar.
    pub exponent: f64,
    /// Diurnal bulge lag, rad.
    pub lag_rad: f64,
}

/// `(h km, ρ_min, ρ_max g/km³)`.
const HP: [(f64, f64, f64); 50] = [
    (100.0, 497_400.0, 497_400.0),
    (120.0, 24_900.0, 24_900.0),
    (130.0, 8377.0, 8710.0),
    (140.0, 3899.0, 4059.0),
    (150.0, 2122.0, 2215.0),
    (160.0, 1263.0, 1344.0),
    (170.0, 800.8, 875.8),
    (180.0, 528.3, 601.0),
    (190.0, 361.7, 429.7),
    (200.0, 255.7, 316.2),
    (210.0, 183.9, 239.6),
    (220.0, 134.1, 185.3),
    (230.0, 99.49, 145.5),
    (240.0, 74.88, 115.7),
    (250.0, 57.09, 93.08),
    (260.0, 44.03, 75.55),
    (270.0, 34.30, 61.82),
    (280.0, 26.97, 50.95),
    (290.0, 21.39, 42.26),
    (300.0, 17.08, 35.26),
    (320.0, 10.99, 25.11),
    (340.0, 7.214, 18.19),
    (360.0, 4.824, 13.37),
    (380.0, 3.274, 9.955),
    (400.0, 2.249, 7.492),
    (420.0, 1.558, 5.684),
    (440.0, 1.091, 4.355),
    (460.0, 0.7701, 3.362),
    (480.0, 0.5474, 2.612),
    (500.0, 0.3916, 2.042),
    (520.0, 0.2819, 1.605),
    (540.0, 0.2042, 1.267),
    (560.0, 0.1488, 1.005),
    (580.0, 0.1092, 0.7997),
    (600.0, 0.080_70, 0.6390),
    (620.0, 0.060_12, 0.5123),
    (640.0, 0.045_19, 0.4121),
    (660.0, 0.034_30, 0.3325),
    (680.0, 0.026_32, 0.2691),
    (700.0, 0.020_43, 0.2185),
    (720.0, 0.016_07, 0.1779),
    (740.0, 0.012_81, 0.1452),
    (760.0, 0.010_36, 0.1190),
    (780.0, 0.008_496, 0.097_76),
    (800.0, 0.007_069, 0.080_59),
    (840.0, 0.004_680, 0.057_41),
    (880.0, 0.003_200, 0.042_10),
    (920.0, 0.002_210, 0.031_30),
    (960.0, 0.001_560, 0.023_60),
    (1000.0, 0.001_150, 0.018_10),
];

impl HarrisPriester {
    pub fn new(sun: impl Ephemeris + 'static) -> Self {
        Self {
            sun: Arc::new(sun),
            exponent: 4.0,
            lag_rad: 30f64.to_radians(),
        }
    }

    /// `(ρ_min, ρ_max)`, kg/m³.
    fn bounds(h_km: f64) -> Option<(f64, f64)> {
        if !(HP[0].0..HP[HP.len() - 1].0).contains(&h_km) {
            return None;
        }
        let i = HP.partition_point(|row| row.0 <= h_km) - 1;
        let (h0, min0, max0) = HP[i];
        let (h1, min1, max1) = HP[i + 1];
        let interp = |a: f64, b: f64| a * ((h_km - h0) / (h1 - h0) * (b / a).ln()).exp();
        Some((interp(min0, min1) * 1e-12, interp(max0, max1) * 1e-12))
    }
}

impl Atmosphere for HarrisPriester {
    fn density(&self, s: &AtmosphereSample) -> f64 {
        let h = s.geodetic.2;
        if h < HP[0].0 {
            // Vallado band below 100 km, extended to 100 km for continuity.
            let (h0, rho0, sh) = VALLADO_EXP[VALLADO_EXP.partition_point(|r| r.0 < HP[0].0) - 1];
            let scale = HP[0].1 * 1e-12 / (rho0 * (-(HP[0].0 - h0) / sh).exp());
            return scale * ExponentialAtmosphere::density_at_height(h);
        }
        let Some((lo, hi)) = Self::bounds(h) else {
            return 0.0;
        };
        let sun = self.sun.state_at(s.epoch).position_km;
        let ra = sun.y.atan2(sun.x) + self.lag_rad;
        let dec = (sun.z / sun.length()).asin();
        let bulge = DVec3::new(dec.cos() * ra.cos(), dec.cos() * ra.sin(), dec.sin());
        let cos_psi = s.position_inertial_km.normalize().dot(bulge);
        let c2 = (0.5 + 0.5 * cos_psi).max(0.0);
        lo + (hi - lo) * c2.powf(0.5 * self.exponent)
    }
}

#[derive(Debug, Clone)]
pub struct Drag {
    pub atmosphere: Arc<dyn Atmosphere>,
}

impl Drag {
    pub fn new(atmosphere: impl Atmosphere + 'static) -> Self {
        Self {
            atmosphere: Arc::new(atmosphere),
        }
    }
}

impl ForceModel for Drag {
    fn acceleration(&self, ctx: &ForceContext) -> DVec3 {
        let Some(bf) = ctx.body_fixed else {
            return DVec3::ZERO;
        };
        let fixed = bf.to_itrf(StateVector::new(ctx.position_km, ctx.velocity_km_s));
        let sample = AtmosphereSample {
            epoch: ctx.epoch,
            position_inertial_km: ctx.position_km,
            position_body_fixed_km: fixed.position_km,
            geodetic: ecef_to_geodetic(fixed.position_km),
        };
        let rho = self.atmosphere.density(&sample);
        if rho == 0.0 {
            return DVec3::ZERO;
        }
        let v_rel = bf.rotation().transpose() * fixed.velocity_km_s;
        let v = ctx.vehicle;
        // ρ [kg/m³] · v² [km²/s² → 1e6 m²/s²] / m → m/s² → 1e-3 km/s²: net 1e3.
        -0.5 * rho * v.drag_coefficient * v.drag_area_m2 / ctx.mass_kg
            * v_rel.length()
            * v_rel
            * 1e3
    }

    fn requires_body_fixed(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FixedOffset;

    #[test]
    fn exponential_table() {
        assert!((ExponentialAtmosphere::density_at_height(400.0) - 3.725e-12).abs() < 1e-20);
        let r = ExponentialAtmosphere::density_at_height(425.0);
        assert!((r - 3.725e-12 * (-25.0f64 / 58.515).exp()).abs() < 1e-20);
        // Continuous to ~5 % at band edges.
        for w in VALLADO_EXP.windows(2) {
            let below = w[0].1 * (-(w[1].0 - w[0].0) / w[0].2).exp();
            assert!((below / w[1].1 - 1.0).abs() < 0.06, "{} km", w[1].0);
        }
    }

    #[test]
    fn harris_priester_bulge() {
        let sun = DVec3::new(1.496e8, 0.0, 0.0);
        let hp = HarrisPriester {
            lag_rad: 0.0,
            ..HarrisPriester::new(FixedOffset(StateVector::new(sun, DVec3::ZERO)))
        };
        let sample = |x: f64| AtmosphereSample {
            epoch: Epoch::J2000,
            position_inertial_km: DVec3::new(x, 0.0, 0.0),
            position_body_fixed_km: DVec3::ZERO,
            geodetic: (0.0, 0.0, 400.0),
        };
        assert!((hp.density(&sample(6778.0)) - 7.492e-12).abs() < 1e-18);
        assert!((hp.density(&sample(-6778.0)) - 2.249e-12).abs() < 1e-18);
    }

    #[test]
    fn harris_priester_below_table() {
        let hp = HarrisPriester::new(FixedOffset(StateVector::new(
            DVec3::new(1.496e8, 0.0, 0.0),
            DVec3::ZERO,
        )));
        let sample = |h: f64| AtmosphereSample {
            epoch: Epoch::J2000,
            position_inertial_km: DVec3::new(6378.0 + h, 0.0, 0.0),
            position_body_fixed_km: DVec3::ZERO,
            geodetic: (0.0, 0.0, h),
        };
        let at = hp.density(&sample(100.0));
        assert!((at - 4.974e-7).abs() < 1e-15);
        // Continuous at 100 km, monotone below.
        let below = hp.density(&sample(100.0 - 1e-9));
        assert!((below / at - 1.0).abs() < 1e-9, "{below:e} {at:e}");
        let (r90, r50) = (hp.density(&sample(90.0)), hp.density(&sample(50.0)));
        assert!(r90 > at && r50 > r90);
        let ratio = r90 / ExponentialAtmosphere::density_at_height(90.0);
        assert!((ratio - 4.974e-7 / (3.396e-6 * (-10.0f64 / 5.382).exp())).abs() < 1e-12);
        assert_eq!(hp.density(&sample(1000.0)), 0.0);
    }
}
