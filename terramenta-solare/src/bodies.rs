//! Physical constants for the bodies the tree knows about.
//!
//! Per body: a standard gravitational parameter (`GM`, km³/s²) and a mean
//! radius. `GM` is what every propagator here uses directly and is known to
//! more digits than mass alone; mean radius converts an altitude to a
//! distance from centre.

/// The Sun's standard gravitational parameter.
pub const GM_SUN_KM3_S2: f64 = 1.327_124_400_18e11;
/// Earth's, including its atmosphere. Matches the value the ephemerides here
/// and `terramenta-globe`'s propagator both assume.
///
/// DE440 (TDB-compatible). EGM2008's TT-compatible
/// [`crate::force::EGM2008_GM_KM3_S2`] differs by 1.5e-8: ~5 m/day LEO along-track
/// between two-body and harmonic propagation. Hand off with the matching GM.
pub const GM_EARTH_KM3_S2: f64 = 398_600.435_507;
pub const GM_MARS_KM3_S2: f64 = 42_828.375_214;
/// DE440 (`gm_de440.tpc`).
pub const GM_MOON_KM3_S2: f64 = 4_902.800_118;
pub const GM_VENUS_KM3_S2: f64 = 324_858.592;
/// System (barycentre) values, DE440.
pub const GM_JUPITER_SYSTEM_KM3_S2: f64 = 126_712_764.1;
pub const GM_SATURN_SYSTEM_KM3_S2: f64 = 37_940_584.841_8;

pub const EARTH_MEAN_RADIUS_KM: f64 = 6_371.0;
pub const MARS_MEAN_RADIUS_KM: f64 = 3_389.5;
pub const VENUS_MEAN_RADIUS_KM: f64 = 6_051.8;
pub const SUN_MEAN_RADIUS_KM: f64 = 696_000.0;
pub const MOON_MEAN_RADIUS_KM: f64 = 1_737.4;

/// The IAU-defined astronomical unit, in kilometres. Scales heliocentric
/// world space the way `terramenta-globe`'s Earth-centered scene scales in
/// Earth radii.
pub const ASTRONOMICAL_UNIT_KM: f64 = 149_597_870.7;

/// The radius of a body's sphere of influence: the distance from a parent
/// (`distance_from_parent_km` away) within which the body's own gravity,
/// rather than the parent's, dominates a third, much lighter object's
/// trajectory.
///
/// Laplace's approximation, `r * (m / M)^(2/5)`, restated in `GM` since mass
/// ratio and `GM` ratio are the same number. A boundary of convenience, not
/// a hard wall. [`crate::spacecraft`] uses it to decide which body's frame
/// best describes a spacecraft's trajectory.
pub fn sphere_of_influence_km(
    gm_body_km3_s2: f64,
    gm_parent_km3_s2: f64,
    distance_from_parent_km: f64,
) -> f64 {
    distance_from_parent_km * (gm_body_km3_s2 / gm_parent_km3_s2).powf(0.4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earths_sphere_of_influence_is_about_nine_hundred_thousand_kilometres() {
        // The textbook figure, to a percent or so.
        let soi = sphere_of_influence_km(GM_EARTH_KM3_S2, GM_SUN_KM3_S2, 149_598_023.0);
        assert!((soi - 924_000.0).abs() / 924_000.0 < 0.02, "{soi}");
    }

    #[test]
    fn mars_has_a_smaller_sphere_of_influence_than_earth_despite_being_farther_out() {
        let earth_soi = sphere_of_influence_km(GM_EARTH_KM3_S2, GM_SUN_KM3_S2, 149_598_023.0);
        let mars_soi = sphere_of_influence_km(GM_MARS_KM3_S2, GM_SUN_KM3_S2, 227_939_366.0);
        // Distance helps Mars, but Earth outmasses it by more than the extra
        // distance buys back.
        assert!(mars_soi < earth_soi, "{mars_soi} vs {earth_soi}");
    }
}
