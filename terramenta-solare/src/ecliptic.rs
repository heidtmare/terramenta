//! The one rotation shared by every heliocentric model in this crate.
//!
//! Planetary orbital elements are conventionally published relative to the
//! ecliptic — the plane of Earth's own orbit, which nearly everything in
//! the solar system roughly shares. The frame tree, like
//! `terramenta-globe`'s ECI, is stated relative to the equator instead. A
//! fixed rotation by the obliquity of the ecliptic (the ~23.4° tilt between
//! the two) is all that separates them — the same rotation
//! `terramenta_globe`'s lunar series applies to convert an ecliptic
//! longitude and latitude to a right ascension and declination.
//!
//! This crate does not model precession, so the obliquity used here is
//! pinned to its J2000 value rather than tracked as a function of time —
//! consistent with treating every top-level frame as non-rotating ICRF axes
//! rather than a frame *of date*.

use glam::DVec3;

/// The obliquity of the ecliptic at J2000.0.
pub const OBLIQUITY_J2000_RAD: f64 = 23.439_291_1 * std::f64::consts::PI / 180.0;

/// Rotates a vector from ecliptic axes (`x` toward the equinox, `z` the
/// ecliptic pole) to equatorial axes (`z` the celestial pole) — a rotation
/// about the shared `x` axis, since the equinox is where the two planes
/// cross.
///
/// Linear, so it applies to a velocity exactly as it does to a position.
pub fn to_equatorial(v: DVec3) -> DVec3 {
    let (sin_e, cos_e) = OBLIQUITY_J2000_RAD.sin_cos();
    DVec3::new(v.x, v.y * cos_e - v.z * sin_e, v.y * sin_e + v.z * cos_e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_equinox_is_fixed_by_the_rotation() {
        // The vernal equinox is where the ecliptic and equator cross, so a
        // vector along the shared x axis is unmoved by tilting one into the
        // other.
        let equinox = DVec3::new(1.0, 0.0, 0.0);
        assert_eq!(to_equatorial(equinox), equinox);
    }

    #[test]
    fn the_ecliptic_pole_tilts_by_the_obliquity() {
        let pole = DVec3::new(0.0, 0.0, 1.0);
        let rotated = to_equatorial(pole);
        let angle_from_celestial_pole = rotated.angle_between(DVec3::Z);
        assert!((angle_from_celestial_pole - OBLIQUITY_J2000_RAD).abs() < 1.0e-12);
    }
}
