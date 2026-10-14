//! Solar radiation pressure, cannonball model, conical shadow of the central body
//! (Montenbruck & Gill, *Satellite Orbits*, §3.4).

use std::sync::Arc;

use glam::DVec3;

use super::{ForceContext, ForceModel};
use crate::bodies::{ASTRONOMICAL_UNIT_KM, SUN_MEAN_RADIUS_KM};
use crate::force::relativity::C_KM_S;
use crate::frame::Ephemeris;

/// Total solar irradiance at 1 AU, W/m² (IAU 2015 B3).
pub const SOLAR_IRRADIANCE_W_M2: f64 = 1361.0;

#[derive(Debug, Clone)]
pub struct SolarRadiationPressure {
    /// Sun relative to the central body.
    pub sun: Arc<dyn Ephemeris>,
    /// Occulting central body radius, km; `0` disables shadowing.
    pub body_radius_km: f64,
    pub irradiance_w_m2: f64,
}

impl SolarRadiationPressure {
    pub fn new(sun: impl Ephemeris + 'static, body_radius_km: f64) -> Self {
        Self {
            sun: Arc::new(sun),
            body_radius_km,
            irradiance_w_m2: SOLAR_IRRADIANCE_W_M2,
        }
    }
}

/// Fraction of the solar disc visible from `r` (central body at origin, radius `body_radius`).
/// `1` sunlit, `0` umbra.
pub fn shadow_fraction(r: DVec3, sun: DVec3, body_radius: f64) -> f64 {
    if body_radius <= 0.0 {
        return 1.0;
    }
    let to_sun = sun - r;
    let a = (SUN_MEAN_RADIUS_KM / to_sun.length()).asin();
    let b = (body_radius / r.length()).min(1.0).asin();
    let c = (-r).angle_between(to_sun);
    if c >= a + b {
        1.0
    } else if c <= b - a {
        0.0
    } else if c <= a - b {
        // Annular.
        1.0 - (b * b) / (a * a)
    } else {
        let x = (c * c + a * a - b * b) / (2.0 * c);
        let y = (a * a - x * x).max(0.0).sqrt();
        let area = a * a * (x / a).clamp(-1.0, 1.0).acos()
            + b * b * ((c - x) / b).clamp(-1.0, 1.0).acos()
            - c * y;
        1.0 - area / (std::f64::consts::PI * a * a)
    }
}

impl ForceModel for SolarRadiationPressure {
    fn acceleration(&self, ctx: &ForceContext) -> DVec3 {
        let sun = ctx.sun(&*self.sun);
        let nu = shadow_fraction(ctx.position_km, sun, self.body_radius_km);
        if nu == 0.0 {
            return DVec3::ZERO;
        }
        let d = ctx.position_km - sun;
        let dist = d.length();
        // N/m² at the spacecraft.
        let p = self.irradiance_w_m2 / (C_KM_S * 1e3) * (ASTRONOMICAL_UNIT_KM / dist).powi(2);
        let v = ctx.vehicle;
        // m/s² → km/s².
        nu * p * v.reflectivity * v.srp_area_m2 / ctx.mass_kg * 1e-3 * d / dist
    }

    fn velocity_dependent(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RE: f64 = 6378.137;

    #[test]
    fn shadow_regions() {
        let sun = DVec3::new(ASTRONOMICAL_UNIT_KM, 0.0, 0.0);
        assert_eq!(shadow_fraction(DVec3::new(7000.0, 0.0, 0.0), sun, RE), 1.0);
        assert_eq!(shadow_fraction(DVec3::new(-7000.0, 0.0, 0.0), sun, RE), 0.0);
        assert_eq!(shadow_fraction(DVec3::new(0.0, 7000.0, 0.0), sun, RE), 1.0);
        // Penumbra boundary sweep at x = −7000: fraction monotone in y.
        let mut prev = 0.0;
        for i in 0..200 {
            let y = 6300.0 + i as f64 * 1.0;
            let nu = shadow_fraction(DVec3::new(-7000.0, y, 0.0), sun, RE);
            assert!(
                (0.0..=1.0).contains(&nu) && nu >= prev - 1e-12,
                "y={y}: {nu}"
            );
            prev = nu;
        }
        assert_eq!(prev, 1.0);
    }

    #[test]
    fn magnitude_at_one_au() {
        let ctx_vehicle = super::super::Vehicle {
            srp_area_m2: 1.0,
            reflectivity: 1.0,
            ..Default::default()
        };
        let sun = DVec3::new(ASTRONOMICAL_UNIT_KM, 0.0, 0.0);
        let srp = SolarRadiationPressure::new(
            crate::frame::FixedOffset(crate::StateVector::new(sun, DVec3::ZERO)),
            0.0,
        );
        let ctx = ForceContext {
            epoch: crate::Epoch::J2000,
            position_km: DVec3::ZERO,
            velocity_km_s: DVec3::ZERO,
            mass_kg: 1.0,
            vehicle: &ctx_vehicle,
            body_fixed: None,
            switch_epoch: crate::Epoch::J2000,
            switch_mass_kg: 1.0,
            sun_km: None,
        };
        let a = srp.acceleration(&ctx);
        // 4.54e-6 m/s² away from the Sun.
        assert!((a.x * 1e3 + 4.5398e-6).abs() < 1e-9, "{a}");
    }
}
