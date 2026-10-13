//! Low-precision geocentric Moon (Montenbruck & Gill, *Satellite Orbits*, §3.3.2).
//!
//! Mean ecliptic and equinox of J2000 (−1.3972° T precession term), rotated to EME2000.
//! ~0.1° direction, ~500 km distance. Velocity by central difference. For third-body
//! perturbation where no kernel is loaded.

use glam::DVec3;

use crate::ecliptic::to_equatorial;
use crate::frame::{Ephemeris, StateVector};
use crate::time::Epoch;

/// Geocentric; EME2000 axes, km.
#[derive(Debug, Clone, Copy, Default)]
pub struct Moon;

/// Central-difference half-step for velocity, s.
const DIFF_STEP_S: f64 = 60.0;

pub fn position_km(epoch: Epoch) -> DVec3 {
    let t = epoch.tt_centuries();
    let deg = |x: f64| x.to_radians();
    let arcsec = |x: f64| (x / 3600.0).to_radians();
    let l0 = deg(218.316_17 + 481_267.880_88 * t - 1.3972 * t);
    let l = deg(134.962_92 + 477_198.867_53 * t);
    let lp = deg(357.525_43 + 35_999.049_44 * t);
    let f = deg(93.272_83 + 483_202.018_73 * t);
    let d = deg(297.850_27 + 445_267.111_35 * t);
    let lon = l0
        + arcsec(
            22_640.0 * l.sin() + 769.0 * (2.0 * l).sin() - 4_586.0 * (l - 2.0 * d).sin()
                + 2_370.0 * (2.0 * d).sin()
                - 668.0 * lp.sin()
                - 412.0 * (2.0 * f).sin()
                - 212.0 * (2.0 * l - 2.0 * d).sin()
                - 206.0 * (l + lp - 2.0 * d).sin()
                + 192.0 * (l + 2.0 * d).sin()
                - 165.0 * (lp - 2.0 * d).sin()
                + 148.0 * (l - lp).sin()
                - 125.0 * d.sin()
                - 110.0 * (l + lp).sin()
                - 55.0 * (2.0 * f - 2.0 * d).sin(),
        );
    let lat = arcsec(
        18_520.0 * (f + lon - l0 + arcsec(412.0 * (2.0 * f).sin() + 541.0 * lp.sin())).sin()
            - 526.0 * (f - 2.0 * d).sin()
            + 44.0 * (l + f - 2.0 * d).sin()
            - 31.0 * (-l + f - 2.0 * d).sin()
            - 25.0 * (-2.0 * l + f).sin()
            - 23.0 * (lp + f - 2.0 * d).sin()
            + 21.0 * (-l + f).sin()
            + 11.0 * (-lp + f - 2.0 * d).sin(),
    );
    let r = 385_000.0
        - 20_905.0 * l.cos()
        - 3_699.0 * (2.0 * d - l).cos()
        - 2_956.0 * (2.0 * d).cos()
        - 570.0 * (2.0 * l).cos()
        + 246.0 * (2.0 * l - 2.0 * d).cos()
        - 205.0 * (lp - 2.0 * d).cos()
        - 171.0 * (l + 2.0 * d).cos()
        - 152.0 * (l + lp - 2.0 * d).cos();
    to_equatorial(r * DVec3::new(lon.cos() * lat.cos(), lon.sin() * lat.cos(), lat.sin()))
}

impl Ephemeris for Moon {
    fn state_at(&self, epoch: Epoch) -> StateVector {
        let ahead = position_km(epoch.advanced_by_seconds(DIFF_STEP_S));
        let behind = position_km(epoch.advanced_by_seconds(-DIFF_STEP_S));
        StateVector::new(position_km(epoch), (ahead - behind) / (2.0 * DIFF_STEP_S))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_and_speed() {
        for days in [0.0, 7.3, 15.0, 3000.0, 9000.0] {
            let s = Moon.state_at(Epoch::J2000.advanced_by_seconds(days * 86_400.0));
            let (r, v) = (s.position_km.length(), s.velocity_km_s.length());
            assert!((356_000.0..407_000.0).contains(&r), "{days} d: {r} km");
            assert!((0.95..1.1).contains(&v), "{days} d: {v} km/s");
        }
    }
}
