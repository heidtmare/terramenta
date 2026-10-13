//! Schwarzschild term, PPN β = γ = 1 (IERS TN36 eq. 10.12, central body only).

use glam::DVec3;

use super::{ForceContext, ForceModel};

/// Speed of light, km/s.
pub const C_KM_S: f64 = 299_792.458;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Relativity {
    pub gm_km3_s2: f64,
}

impl ForceModel for Relativity {
    fn acceleration(&self, ctx: &ForceContext) -> DVec3 {
        let (r, v) = (ctx.position_km, ctx.velocity_km_s);
        let rr = r.length();
        let gm = self.gm_km3_s2;
        gm / (C_KM_S * C_KM_S * rr.powi(3))
            * ((4.0 * gm / rr - v.length_squared()) * r + 4.0 * r.dot(v) * v)
    }
}
