//! Physical constants for the bodies the tree knows about.
//!
//! Just enough per body to place it and to tell whether something is still
//! inside its gravitational grip: a standard gravitational parameter (`GM`,
//! kilometres cubed per second squared — mass alone is useless to an orbit
//! equation, and `GM` is both what every propagator here actually wants and
//! the one of the two that is known to far more digits) and a mean radius,
//! for turning an altitude into a distance from centre.

/// The Sun's standard gravitational parameter.
pub const GM_SUN_KM3_S2: f64 = 1.327_124_400_18e11;
/// Earth's, including its atmosphere — the value the ephemerides here and
/// `terramenta-globe`'s propagator both assume.
pub const GM_EARTH_KM3_S2: f64 = 398_600.435_507;
pub const GM_MARS_KM3_S2: f64 = 42_828.375_214;

pub const EARTH_MEAN_RADIUS_KM: f64 = 6_371.0;
pub const MARS_MEAN_RADIUS_KM: f64 = 3_389.5;
pub const SUN_MEAN_RADIUS_KM: f64 = 696_000.0;

/// The IAU-defined astronomical unit, in kilometres — the unit a heliocentric
/// view scales world space in, the way `terramenta-globe`'s Earth-centered
/// scene scales it in Earth radii.
pub const ASTRONOMICAL_UNIT_KM: f64 = 149_597_870.7;

/// The radius of a body's sphere of influence: how far its own gravity
/// dominates a third, much lighter object's trajectory over its parent's,
/// against a parent it sits `distance_from_parent_km` from.
///
/// This is Laplace's approximation, `r * (m / M)^(2/5)`, restated in `GM`
/// since that is what this crate already has on hand for every body — the
/// mass ratio and the `GM` ratio are the same number. It is a boundary of
/// convenience rather than a hard wall (real gravity doesn't end at a
/// sphere), which is exactly the sense [`crate::spacecraft`] uses it in: a
/// place to decide which body's frame is the *more useful* one to describe
/// the spacecraft in, not the moment Earth's pull "switches off".
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
