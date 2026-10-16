//! IAU/IAG WGCCRE body orientation (Archinal et al. 2018, *Celest. Mech. Dyn. Astr.* 130:22;
//! NAIF `pck00011.tpc`).
//!
//! `α = α₀ + α₁T + Σ aᵢ sin θᵢ`, `δ = δ₀ + δ₁T + Σ dᵢ cos θᵢ`,
//! `W = W₀ + W₁d + W₂d² + Σ wᵢ sin θᵢ`, `θᵢ = θᵢ₀ + θᵢ₁T + θᵢ₂T²`. Degrees; `T` TDB Julian
//! centuries, `d` TDB days from J2000.
//!
//! ICRF → body-fixed: `B · R3(W) · R1(90° − δ) · R3(90° + α)`, `B` a constant frame offset
//! (identity for IAU frames). Pole rates are dropped from the body angular velocity.

use glam::DMat3;

use crate::earth::TerrestrialTransform;
use crate::propagate::BodyOrientation;
use crate::time::Epoch;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotationElements {
    /// α: deg, deg/cy, deg/cy².
    pub pole_ra: [f64; 3],
    /// δ: deg, deg/cy, deg/cy².
    pub pole_dec: [f64; 3],
    /// W: deg, deg/day, deg/day².
    pub prime_meridian: [f64; 3],
    /// θᵢ: deg, deg/cy, deg/cy².
    pub angles: &'static [[f64; 3]],
    /// `aᵢ` (sin), deg; missing trailing terms are zero.
    pub ra_terms: &'static [f64],
    /// `dᵢ` (cos), deg.
    pub dec_terms: &'static [f64],
    /// `wᵢ` (sin), deg.
    pub pm_terms: &'static [f64],
}

const DAYS_PER_CENTURY: f64 = 36_525.0;

/// IAU 2015 Moon (`IAU_MOON`, mean-Earth/polar-axis approximation).
pub const MOON: RotationElements = RotationElements {
    pole_ra: [269.9949, 0.0031, 0.0],
    pole_dec: [66.5392, 0.0130, 0.0],
    prime_meridian: [38.3213, 13.176_358_15, -1.4e-12],
    angles: &[
        [125.045, -1_935.536_452_5, 0.0],
        [250.089, -3_871.072_905, 0.0],
        [260.008, 475_263.332_872_5, 0.0],
        [176.625, 487_269.629_985, 0.0],
        [357.529, 35_999.050_957_5, 0.0],
        [311.589, 964_468.499_31, 0.0],
        [134.963, 477_198.869_325, 0.0],
        [276.617, 12_006.300_765, 0.0],
        [34.226, 63_863.513_242_5, 0.0],
        [15.134, -5_806.609_357_5, 0.0],
        [119.743, 131.840_64, 0.0],
        [239.961, 6_003.150_382_5, 0.0],
        [25.053, 473_327.796_42, 0.0],
    ],
    ra_terms: &[
        -3.8787, -0.1204, 0.0700, -0.0172, 0.0, 0.0072, 0.0, 0.0, 0.0, -0.0052, 0.0, 0.0, 0.0043,
    ],
    dec_terms: &[
        1.5419, 0.0239, -0.0278, 0.0068, 0.0, -0.0029, 0.0009, 0.0, 0.0, 0.0008, 0.0, 0.0, -0.0009,
    ],
    pm_terms: &[
        3.5610, 0.1208, -0.0642, 0.0158, 0.0252, -0.0066, -0.0047, -0.0046, 0.0028, 0.0052, 0.0040,
        0.0019, -0.0044,
    ],
};

/// IAU 2015 Mars (`IAU_MARS`).
pub const MARS: RotationElements = RotationElements {
    pole_ra: [317.269_202, -0.109_275_47, 0.0],
    pole_dec: [54.432_516, -0.058_271_05, 0.0],
    prime_meridian: [176.049_863, 350.891_982_443_297, 0.0],
    angles: &[
        [190.726_466_43, 15_917.108_186_95, 0.0],
        [21.468_924_70, 31_834.279_340_54, 0.0],
        [332.860_827_93, 19_139.896_947_42, 0.0],
        [394.932_564_37, 38_280.796_318_35, 0.0],
        [189.632_715_60, 41_215_158.184_200_5, 12.711_923_222],
        [121.468_936_64, 660.228_034_74, 0.0],
        [231.050_285_81, 660.991_235_40, 0.0],
        [251.373_140_25, 1_320.501_452_45, 0.0],
        [217.986_359_55, 38_279.961_255_50, 0.0],
        [196.197_294_02, 19_139.836_286_08, 0.0],
        [198.991_226, 19_139.481_998_5, 0.0],
        [226.292_679, 38_280.851_128_1, 0.0],
        [249.663_391, 57_420.725_159_3, 0.0],
        [266.183_510, 76_560.636_795_0, 0.0],
        [79.398_797, 0.504_261_5, 0.0],
        [122.433_576, 19_139.940_747_6, 0.0],
        [43.058_401, 38_280.875_327_2, 0.0],
        [57.663_379, 57_420.751_720_5, 0.0],
        [79.476_401, 76_560.649_500_4, 0.0],
        [166.325_722, 0.504_261_5, 0.0],
        [129.071_773, 19_140.032_824_4, 0.0],
        [36.352_167, 38_281.047_359_1, 0.0],
        [56.668_646, 57_420.929_536_0, 0.0],
        [67.364_003, 76_560.255_221_5, 0.0],
        [104.792_680, 95_700.438_757_8, 0.0],
        [95.391_654, 0.504_261_5, 0.0],
    ],
    ra_terms: &[
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.000_068, 0.000_238, 0.000_052,
        0.000_009, 0.419_057,
    ],
    dec_terms: &[
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.000_051,
        0.000_141, 0.000_031, 0.000_005, 1.591_274,
    ],
    pm_terms: &[
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.000_145, 0.000_157, 0.000_040, 0.000_001, 0.000_001, 0.584_542,
    ],
};

/// Moon mean-Earth (DE421 convention) → DE440 principal axes, arcsec, z-y-x
/// (`TKFRAME_31009`, `moon_de440_250416.tf`): `r_PA = [θ₁]₃ [θ₂]₂ [θ₃]₁ r_ME`.
pub const MOON_ME_TO_PA_ARCSEC: [f64; 3] = [67.8526, 78.6944, 0.2785];

impl RotationElements {
    /// `(α, δ, W)` rad and `dW/dt` rad/s at `epoch`.
    pub fn angles_at(&self, epoch: Epoch) -> (f64, f64, f64, f64) {
        let d = epoch.tdb_seconds() / 86_400.0;
        let t = d / DAYS_PER_CENTURY;
        let poly = |c: [f64; 3], x: f64| c[0] + x * (c[1] + x * c[2]);
        let (mut ra, mut dec) = (poly(self.pole_ra, t), poly(self.pole_dec, t));
        let mut w = poly(self.prime_meridian, d);
        // deg/day.
        let mut w_rate = self.prime_meridian[1] + 2.0 * self.prime_meridian[2] * d;
        for (i, theta) in self.angles.iter().enumerate() {
            let (s, c) = poly(*theta, t).to_radians().sin_cos();
            let rate = (theta[1] + 2.0 * theta[2] * t).to_radians() / DAYS_PER_CENTURY;
            let term = |terms: &[f64]| terms.get(i).copied().unwrap_or(0.0);
            ra += term(self.ra_terms) * s;
            dec += term(self.dec_terms) * c;
            w += term(self.pm_terms) * s;
            w_rate += term(self.pm_terms) * c * rate;
        }
        (
            ra.to_radians(),
            dec.to_radians(),
            w.to_radians(),
            w_rate.to_radians() / 86_400.0,
        )
    }
}

/// Passive rotation about axis 1, 2, 3.
fn r1(a: f64) -> DMat3 {
    DMat3::from_rotation_x(-a)
}

fn r2(a: f64) -> DMat3 {
    DMat3::from_rotation_y(-a)
}

fn r3(a: f64) -> DMat3 {
    DMat3::from_rotation_z(-a)
}

/// IAU body-fixed frame as a [`BodyOrientation`].
///
/// [`TerrestrialTransform::inertial_to_tirs`] holds `R3(W)·R1(90° − δ)·R3(90° + α)`,
/// [`TerrestrialTransform::polar_motion`] the offset `B`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IauOrientation {
    pub elements: RotationElements,
    pub offset: DMat3,
}

impl IauOrientation {
    pub fn new(elements: RotationElements) -> Self {
        Self {
            elements,
            offset: DMat3::IDENTITY,
        }
    }

    /// Lunar principal axes (frame of GRAIL fields): IAU Moon + [`MOON_ME_TO_PA_ARCSEC`].
    /// ≤ 0.005° from DE440 librations (2000–2040).
    pub fn moon_pa() -> Self {
        let [a, b, c] = MOON_ME_TO_PA_ARCSEC.map(|x| (x / 3600.0).to_radians());
        Self {
            elements: MOON,
            offset: r3(a) * r2(b) * r1(c),
        }
    }

    pub fn mars() -> Self {
        Self::new(MARS)
    }

    pub fn transform(&self, epoch: Epoch) -> TerrestrialTransform {
        let (ra, dec, w, w_rate) = self.elements.angles_at(epoch);
        let half_pi = std::f64::consts::FRAC_PI_2;
        TerrestrialTransform {
            inertial_to_tirs: r3(w) * r1(half_pi - dec) * r3(half_pi + ra),
            polar_motion: self.offset,
            omega_rad_s: w_rate,
        }
    }
}

impl BodyOrientation for IauOrientation {
    fn transform(&self, epoch: Epoch) -> TerrestrialTransform {
        IauOrientation::transform(self, epoch)
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;

    #[test]
    fn pole_is_body_z() {
        let e = Epoch::from_utc(2030, 1, 1, 0, 0, 0.0);
        for o in [IauOrientation::new(MOON), IauOrientation::mars()] {
            let (ra, dec, _, _) = o.elements.angles_at(e);
            let pole = DVec3::new(dec.cos() * ra.cos(), dec.cos() * ra.sin(), dec.sin());
            let z = o.transform(e).rotation() * pole;
            assert!((z - DVec3::Z).length() < 1e-14, "{z}");
        }
    }

    #[test]
    fn rate_matches_finite_difference() {
        let e = Epoch::from_utc(2027, 6, 1, 0, 0, 0.0);
        for el in [MOON, MARS] {
            let (_, _, _, rate) = el.angles_at(e);
            let w = |s: f64| el.angles_at(e.advanced_by_seconds(s)).2;
            let fd = (w(60.0) - w(-60.0)) / 120.0;
            assert!((rate - fd).abs() < 1e-7 * fd.abs(), "{rate} {fd}");
        }
    }

    #[test]
    fn advanced_by_tracks_spin() {
        let o = IauOrientation::moon_pa();
        let e = Epoch::from_utc(2027, 6, 1, 0, 0, 0.0);
        let r = DVec3::new(1800.0, 200.0, -300.0);
        let exact = o.transform(e.advanced_by_seconds(600.0)).rotation() * r;
        let advanced = o.transform(e).advanced_by(600.0).rotation() * r;
        // Pole drift over 10 min (α, δ libration terms): ~7e-7 rad.
        assert!(
            (exact - advanced).length() < 2e-3,
            "{}",
            (exact - advanced).length()
        );
    }

    #[test]
    fn moon_pa_offset_is_small() {
        let o = IauOrientation::moon_pa();
        let angle = (o.offset * DVec3::X).angle_between(DVec3::X).to_degrees() * 3600.0;
        assert!((angle - 103.0).abs() < 2.0, "{angle}");
    }
}
