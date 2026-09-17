//! Where the moon is, so the sublunar point falls in the right place.
//!
//! The moon cannot be had as cheaply as the sun. A subsolar point is a
//! declination and an hour angle, and [`crate::sun`] gets away with reading
//! both off the calendar; the moon's orbit is inclined to the ecliptic, moves
//! its own perigee round in nine years, its node round in nineteen, and is
//! pulled about by the sun by more than a degree — so an orbit taken straight
//! from the elements lands over a thousand kilometres from where the moon
//! actually is. What is here is the classical low-precision series: Keplerian
//! elements for the epoch, the handful of perturbation terms that matter
//! (evection, variation, the annual equation and a dozen smaller ones), and no
//! more. That holds the direction to a few arcminutes, which is inside the
//! icon that marks it.
//!
//! Two things separate this from the solar model next door.
//!
//! **It goes through the sky.** The series gives an ecliptic longitude and
//! latitude, which become a right ascension and declination, and only the
//! Earth's own rotation turns those into a place on the ground — so the
//! sublunar longitude is measured against Greenwich's *sidereal* angle, which
//! [`crate::frame::sidereal_radians`] already keeps for the ECI frame.
//!
//! **It is worked out in `f64`.** The terms are degrees accumulated over tens
//! of thousands of days; narrowed early, the mean anomaly alone would lose more
//! than the perturbations are worth.
//!
//! Geocentric, and deliberately: this is where the moon is overhead, which is a
//! direction from the centre of the Earth. The parallax that separates it from
//! where an observer sees the moon is nearly a degree, and it belongs to the
//! observer rather than to the point on the ground.

use bevy::prelude::*;

use crate::frame::{FrameSet, sidereal_radians};
use crate::geo::LatLon;
use crate::sun::{Sun, wall_clock_unix_seconds};

/// Seconds from the Unix epoch to 1999-12-31 00:00 UT, the epoch these
/// elements are stated at — "day zero" of the series, half a day before J2000.
const EPOCH_UNIX_SECONDS: f64 = 946_598_400.0;
const SECONDS_PER_DAY: f64 = 86_400.0;

/// The moon's orbit at the epoch, and how fast each element moves, in degrees
/// and degrees per day.
const NODE_DEG: (f64, f64) = (125.1228, -0.052_953_808_3);
const INCLINATION_DEG: f64 = 5.1454;
const PERIGEE_DEG: (f64, f64) = (318.0634, 0.164_357_322_3);
const MEAN_ANOMALY_DEG: (f64, f64) = (115.3654, 13.064_992_950_9);
const ECCENTRICITY: f64 = 0.054_900;

/// The sun's, which the perturbations are driven by: the moon is pulled about
/// by where it is relative to the sun, so the solar mean anomaly and mean
/// longitude are part of the lunar series.
const SUN_PERIGEE_DEG: (f64, f64) = (282.9404, 4.709_35e-5);
const SUN_MEAN_ANOMALY_DEG: (f64, f64) = (356.0470, 0.985_600_258_5);

/// The obliquity of the ecliptic at the epoch, and its slow decline.
const OBLIQUITY_DEG: (f64, f64) = (23.4393, -3.563e-7);

/// Where the moon is, as a place on the ground.
#[derive(Resource, Debug, Clone, Copy)]
pub struct Moon {
    /// The point on Earth the moon is directly over.
    pub sublunar: LatLon,
    /// Unit vector from the globe's centre toward the moon, in Earth-fixed
    /// coordinates — the same convention [`crate::sun::Sun::direction_ecef`]
    /// follows, and rotated into world space the same way.
    pub direction_ecef: Vec3,
}

impl Default for Moon {
    fn default() -> Self {
        let mut moon = Self {
            sublunar: LatLon::new(0.0, 0.0),
            direction_ecef: Vec3::X,
        };
        // The same wall clock the sun starts on, so the first frame drawn has
        // the moon where it really is rather than off the coast of Africa.
        moon.set_clock(wall_clock_unix_seconds());
        moon
    }
}

impl Moon {
    /// Points the moon where the given moment says it should be.
    pub fn set_clock(&mut self, unix_seconds: f64) {
        self.sublunar = sublunar_point(unix_seconds);
        self.direction_ecef = self.sublunar.to_direction();
    }
}

pub struct MoonPlugin;

impl Plugin for MoonPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Moon>().add_systems(
            Update,
            // After the clock has been advanced for this frame, and before
            // anything drawn from the frame reads the result.
            track_moon
                .after(crate::sun::advance_sun)
                .before(FrameSet::Apply),
        );
    }
}

/// Follows the same simulated clock the sun does. There is only one clock, and
/// a moon on a clock of its own would drift out of phase with the terminator.
fn track_moon(sun: Res<Sun>, mut moon: ResMut<Moon>) {
    moon.set_clock(sun.unix_seconds);
}

/// The point on Earth the moon is overhead at a given moment.
fn sublunar_point(unix_seconds: f64) -> LatLon {
    let day = (unix_seconds - EPOCH_UNIX_SECONDS) / SECONDS_PER_DAY;
    let (longitude, latitude) = ecliptic_position(day);
    let (right_ascension, declination) = to_equatorial(longitude, latitude, day);

    // Right ascension is measured from the vernal equinox and Greenwich's
    // sidereal angle says where Greenwich is from the same mark, so the
    // difference is the longitude the moon is over.
    let east = (right_ascension - sidereal_radians(unix_seconds)).to_degrees();
    LatLon::new(declination.to_degrees() as f32, wrap_degrees(east) as f32)
}

/// The moon's geocentric ecliptic longitude and latitude, in radians.
fn ecliptic_position(day: f64) -> (f64, f64) {
    let node = at(NODE_DEG, day).to_radians();
    let inclination = INCLINATION_DEG.to_radians();
    let perigee = at(PERIGEE_DEG, day).to_radians();
    let mean_anomaly = at(MEAN_ANOMALY_DEG, day).to_radians();

    // In the plane of the orbit: the eccentric anomaly gives the true anomaly,
    // and the two together place the moon on its ellipse. The semi-major axis
    // cancels out of a direction, so it is left at one.
    let eccentric = eccentric_anomaly(mean_anomaly);
    let x = eccentric.cos() - ECCENTRICITY;
    let y = (1.0 - ECCENTRICITY * ECCENTRICITY).sqrt() * eccentric.sin();
    let true_anomaly = y.atan2(x);
    let argument = true_anomaly + perigee;

    // Out of the orbital plane and into the ecliptic, through the node.
    let (sin_argument, cos_argument) = argument.sin_cos();
    let (sin_node, cos_node) = node.sin_cos();
    let (sin_inclination, cos_inclination) = inclination.sin_cos();
    let ecliptic_x = cos_node * cos_argument - sin_node * sin_argument * cos_inclination;
    let ecliptic_y = sin_node * cos_argument + cos_node * sin_argument * cos_inclination;
    let ecliptic_z = sin_argument * sin_inclination;

    let longitude = ecliptic_y.atan2(ecliptic_x);
    let latitude = ecliptic_z.atan2(ecliptic_x.hypot(ecliptic_y));

    let (longitude, latitude) = perturb(longitude, latitude, day);
    (longitude, latitude)
}

/// The sun's pull on the orbit, as the classical series states it.
///
/// Every term is an angle in degrees, driven by one of four arguments: the
/// moon's mean anomaly, the sun's, the moon's elongation from the sun and its
/// argument of latitude. The first two in longitude are the ones that matter —
/// the evection at over a degree and the variation at two thirds of one — and
/// the tail is kept because it is a dozen sines for the last tenth of a degree.
fn perturb(longitude: f64, latitude: f64, day: f64) -> (f64, f64) {
    let moon_anomaly = at(MEAN_ANOMALY_DEG, day).to_radians();
    let sun_anomaly = at(SUN_MEAN_ANOMALY_DEG, day).to_radians();
    let sun_longitude = at(SUN_PERIGEE_DEG, day).to_radians() + sun_anomaly;
    let moon_longitude =
        at(NODE_DEG, day).to_radians() + at(PERIGEE_DEG, day).to_radians() + moon_anomaly;
    // How far the moon has run from the sun, and how far from its own node.
    let elongation = moon_longitude - sun_longitude;
    let latitude_argument = moon_longitude - at(NODE_DEG, day).to_radians();

    let (m, s, d, f) = (moon_anomaly, sun_anomaly, elongation, latitude_argument);
    let longitude_terms = [
        (-1.274, m - 2.0 * d), // the evection
        (0.658, 2.0 * d),      // the variation
        (-0.186, s),           // the annual equation
        (-0.059, 2.0 * m - 2.0 * d),
        (-0.057, m - 2.0 * d + s),
        (0.053, m + 2.0 * d),
        (0.046, 2.0 * d - s),
        (0.041, m - s),
        (-0.035, d), // the parallactic inequality
        (-0.031, m + s),
        (-0.015, 2.0 * f - 2.0 * d),
        (0.011, m - 4.0 * d),
    ];
    let latitude_terms = [
        (-0.173, f - 2.0 * d),
        (-0.055, m - f - 2.0 * d),
        (-0.046, m + f - 2.0 * d),
        (0.033, f + 2.0 * d),
        (0.017, 2.0 * m + f),
    ];

    (
        longitude + sum_of_sines(&longitude_terms),
        latitude + sum_of_sines(&latitude_terms),
    )
}

/// Adds up `(degrees, argument)` terms and hands back radians.
fn sum_of_sines(terms: &[(f64, f64)]) -> f64 {
    terms
        .iter()
        .map(|(amplitude, argument)| amplitude * argument.sin())
        .sum::<f64>()
        .to_radians()
}

/// Turns an ecliptic direction into a right ascension and declination, in
/// radians. The obliquity is the only thing that separates the two frames.
fn to_equatorial(longitude: f64, latitude: f64, day: f64) -> (f64, f64) {
    let obliquity = at(OBLIQUITY_DEG, day).to_radians();
    let (sin_longitude, cos_longitude) = longitude.sin_cos();
    let (sin_latitude, cos_latitude) = latitude.sin_cos();
    let (sin_obliquity, cos_obliquity) = obliquity.sin_cos();

    let x = cos_latitude * cos_longitude;
    let y = cos_latitude * sin_longitude * cos_obliquity - sin_latitude * sin_obliquity;
    let z = cos_latitude * sin_longitude * sin_obliquity + sin_latitude * cos_obliquity;

    (y.atan2(x), z.asin())
}

/// Solves Kepler's equation for the eccentric anomaly, in radians.
///
/// Newton's method from the first-order guess. The moon's eccentricity is only
/// 0.055, so this is converged well inside a metre on the ground after two
/// steps; three is for the rounding.
fn eccentric_anomaly(mean_anomaly: f64) -> f64 {
    let mut eccentric = mean_anomaly + ECCENTRICITY * mean_anomaly.sin();
    for _ in 0..3 {
        let error = eccentric - ECCENTRICITY * eccentric.sin() - mean_anomaly;
        eccentric -= error / (1.0 - ECCENTRICITY * eccentric.cos());
    }
    eccentric
}

/// An element at a given day: its value at the epoch plus its daily rate.
fn at((epoch, per_day): (f64, f64), day: f64) -> f64 {
    epoch + per_day * day
}

/// Folds an angle into -180°..180°.
fn wrap_degrees(degrees: f64) -> f64 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The total lunar eclipse of 21 January 2000, at greatest eclipse. The
    /// moon is in the Earth's shadow, so it is within a fraction of a degree of
    /// the antisolar point — which is a check on the whole chain at once:
    /// longitude, latitude, the equatorial conversion and the sidereal angle.
    const ECLIPSE_UNIX_SECONDS: f64 = 948_429_840.0;

    /// Separation between two points on the globe, in degrees.
    fn separation(a: LatLon, b: LatLon) -> f32 {
        a.to_direction()
            .angle_between(b.to_direction())
            .to_degrees()
    }

    #[test]
    fn the_moon_is_opposite_the_sun_at_a_lunar_eclipse() {
        let mut sun = Sun::default();
        sun.set_clock(ECLIPSE_UNIX_SECONDS);
        let antisolar = LatLon::new(
            -sun.subsolar.lat,
            wrap_degrees(sun.subsolar.lon as f64 + 180.0) as f32,
        );

        let sublunar = sublunar_point(ECLIPSE_UNIX_SECONDS);
        // Generous, because the *sun* is the crude one here: the solar model
        // skips the equation of time, which alone is worth a few degrees of
        // longitude. The lunar series is good to arcminutes.
        assert!(
            separation(sublunar, antisolar) < 5.0,
            "{sublunar:?} vs {antisolar:?}"
        );
    }

    #[test]
    fn the_moon_keeps_within_its_declination_limits() {
        // The orbit is inclined about five degrees to the ecliptic, so the
        // moon never runs further from the equator than the obliquity plus
        // that — and over nineteen years of nodal precession it reaches it.
        let mut extreme: f32 = 0.0;
        for day in 0..7000 {
            let point = sublunar_point(EPOCH_UNIX_SECONDS + day as f64 * SECONDS_PER_DAY);
            extreme = extreme.max(point.lat.abs());
        }
        assert!((23.0..=29.0).contains(&extreme), "{extreme}");
    }

    #[test]
    fn the_sublunar_point_creeps_eastward_by_the_day() {
        // The moon runs about 13.2° east against the stars in a day and the
        // Earth only makes up 0.99° of that in a solar one, so the point the
        // moon is over lands some twelve degrees further east each day — which
        // is the moon rising fifty minutes later, read off the ground instead
        // of off the sky. One day is not a fair sample of a rate that swings
        // with the eccentricity of the orbit, so this averages a month of them.
        let drift: f64 = (0..27)
            .map(|day| {
                let start = ECLIPSE_UNIX_SECONDS + day as f64 * SECONDS_PER_DAY;
                let step = sublunar_point(start + SECONDS_PER_DAY).lon - sublunar_point(start).lon;
                wrap_degrees(step as f64)
            })
            .sum::<f64>()
            / 27.0;
        assert!((11.5..=13.0).contains(&drift), "{drift}");
    }

    /// The moon was all but exactly opposite the sun at the eclipse above, and
    /// its ecliptic latitude within a third of a degree of zero — which is what
    /// an eclipse *is*. Against the published position for that moment, right
    /// ascension 8h11m and declination +19°53', this is the one test here that
    /// pins the series to the sky rather than to itself.
    #[test]
    fn the_moon_is_where_the_almanac_says_it_was() {
        let day = (ECLIPSE_UNIX_SECONDS - EPOCH_UNIX_SECONDS) / SECONDS_PER_DAY;
        let (longitude, latitude) = ecliptic_position(day);
        let (right_ascension, declination) = to_equatorial(longitude, latitude, day);

        let hours = right_ascension.to_degrees().rem_euclid(360.0) / 15.0;
        assert!((hours - 8.183).abs() < 0.05, "{hours} hours");
        assert!(
            (declination.to_degrees() - 19.883).abs() < 0.3,
            "{declination}"
        );
        assert!(latitude.to_degrees().abs() < 0.5, "{latitude}");
    }

    #[test]
    fn the_direction_and_the_coordinate_agree() {
        let mut moon = Moon::default();
        moon.set_clock(ECLIPSE_UNIX_SECONDS);
        assert!(
            separation(LatLon::from_direction(moon.direction_ecef), moon.sublunar) < 1.0e-3,
            "{moon:?}"
        );
    }
}
