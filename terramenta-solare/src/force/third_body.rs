//! Third-body point-mass perturbation, Battin's `f(q)` form (*Mathematical Methods of
//! Astrodynamics*, §8.4) to avoid cancellation when |r| ≪ |s|.

use std::sync::Arc;

use glam::DVec3;

use super::{ForceContext, ForceModel};
use crate::frame::Ephemeris;

#[derive(Debug, Clone)]
pub struct ThirdBody {
    pub gm_km3_s2: f64,
    /// Body relative to the central body.
    pub ephemeris: Arc<dyn Ephemeris>,
    /// Body is the Sun: [`ForceContext::sun_km`] overrides `ephemeris`.
    pub is_sun: bool,
}

impl ThirdBody {
    pub fn new(gm_km3_s2: f64, ephemeris: impl Ephemeris + 'static) -> Self {
        Self {
            gm_km3_s2,
            ephemeris: Arc::new(ephemeris),
            is_sun: false,
        }
    }

    pub fn sun(gm_km3_s2: f64, ephemeris: impl Ephemeris + 'static) -> Self {
        Self {
            is_sun: true,
            ..Self::new(gm_km3_s2, ephemeris)
        }
    }
}

/// `μ [(s − r)/|s − r|³ − s/|s|³]`.
pub fn third_body_acceleration(gm: f64, r: DVec3, s: DVec3) -> DVec3 {
    let q = r.dot(r - 2.0 * s) / s.length_squared();
    let f = q * (3.0 + 3.0 * q + q * q) / (1.0 + (1.0 + q).powf(1.5));
    let d = s - r;
    -gm / d.length().powi(3) * (r + f * s)
}

impl ForceModel for ThirdBody {
    fn acceleration(&self, ctx: &ForceContext) -> DVec3 {
        let s = if self.is_sun {
            ctx.sun(&*self.ephemeris)
        } else {
            self.ephemeris.position_at(ctx.epoch)
        };
        third_body_acceleration(self.gm_km3_s2, ctx.position_km, s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battin_matches_direct() {
        let gm = 4902.8;
        let s = DVec3::new(380_000.0, 50_000.0, -20_000.0);
        let r = DVec3::new(7000.0, -2000.0, 500.0);
        let direct = gm * ((s - r) / (s - r).length().powi(3) - s / s.length().powi(3));
        let battin = third_body_acceleration(gm, r, s);
        assert!(((battin - direct).length() / direct.length()) < 1e-10);
    }
}
