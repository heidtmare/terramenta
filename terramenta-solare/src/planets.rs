//! Earth and Mars, as seen from the Solar System Barycentre.
//!
//! Each is stated the way the JPL low-precision planetary theory publishes
//! it: mean orbital elements at J2000.0 and their rate per Julian century,
//! valid from 1800 to 2050 to a few arcseconds — plenty for a scene, not
//! enough for a real mission — the same approach `terramenta-globe`'s own sun
//! and moon already take. A planet's own gravity
//! perturbs the others' orbits at a level these linear rates already fold in
//! as an average; what they do not capture is the day-to-day wobble that
//! comes from *where* the perturbing planets currently are, which is why this
//! is good for "where is Mars this year" and not for targeting a specific
//! transfer window down to the day.
//!
//! A planet's state relative to the SSB — the frame tree's actual root — is
//! its heliocentric state plus [`crate::sun::ssb_relative_state`]: exactly
//! the vector sum the tree would compute anyway by walking Earth or Mars up
//! through a Sun node and out to the root, done once here instead so that
//! Earth and Mars can sit as the SSB's direct children the way
//! [`crate::frame`] describes, rather than needing the Sun as an
//! intermediate hop.

use crate::ecliptic::to_equatorial;
use crate::frame::{Ephemeris, StateVector};
use crate::orbit::OrbitalElements;
use crate::sun::ssb_relative_state;
use crate::time::Epoch;

use crate::bodies::GM_SUN_KM3_S2;

const AU_KM: f64 = 1.495_978_707e8;

/// A linear element: its value at J2000.0 and its rate per Julian century.
#[derive(Debug, Clone, Copy)]
struct Rate {
    at_j2000: f64,
    per_century: f64,
}

impl Rate {
    fn at(self, julian_centuries: f64) -> f64 {
        self.at_j2000 + self.per_century * julian_centuries
    }
}

/// A planet's heliocentric mean orbital elements, in the units the JPL table
/// publishes them: astronomical units and degrees.
#[derive(Debug, Clone, Copy)]
struct KeplerianSeries {
    semi_major_axis_au: Rate,
    eccentricity: Rate,
    inclination_deg: Rate,
    /// `L`: the mean longitude, measured all the way from the equinox
    /// through the ascending node and the periapsis to the planet itself —
    /// not the mean anomaly, which is `L` less the longitude of periapsis.
    mean_longitude_deg: Rate,
    /// `ϖ` (variously "longitude of periapsis" or "long. peri."): the
    /// periapsis's own longitude, measured the same way — not the argument of
    /// periapsis, which is `ϖ` less the longitude of the ascending node.
    longitude_of_periapsis_deg: Rate,
    longitude_of_ascending_node_deg: Rate,
}

impl KeplerianSeries {
    /// This series' osculating elements at a given epoch, ready to propagate
    /// with [`OrbitalElements::state_at`].
    fn elements_at(&self, epoch: Epoch) -> OrbitalElements {
        let t = epoch.julian_centuries();
        let longitude_of_periapsis = self.longitude_of_periapsis_deg.at(t);
        let longitude_of_ascending_node = self.longitude_of_ascending_node_deg.at(t);

        OrbitalElements {
            semi_major_axis_km: self.semi_major_axis_au.at(t) * AU_KM,
            eccentricity: self.eccentricity.at(t),
            inclination_rad: self.inclination_deg.at(t).to_radians(),
            raan_rad: longitude_of_ascending_node.to_radians(),
            arg_periapsis_rad: (longitude_of_periapsis - longitude_of_ascending_node).to_radians(),
            mean_anomaly_at_epoch: (self.mean_longitude_deg.at(t) - longitude_of_periapsis)
                .to_radians(),
            epoch_seconds: epoch.to_unix_seconds(),
        }
    }
}

/// The Earth-Moon barycentre's elements — Earth's own centre wanders from
/// this by up to about 4,700 km as the Moon orbits it, which is the Moon's
/// business (`terramenta_globe::moon`) and not this crate's; this is Earth's
/// position to the precision everything else here is stated at.
const EARTH_ELEMENTS: KeplerianSeries = KeplerianSeries {
    semi_major_axis_au: Rate {
        at_j2000: 1.000_002_61,
        per_century: 0.000_005_62,
    },
    eccentricity: Rate {
        at_j2000: 0.016_711_23,
        per_century: -0.000_043_92,
    },
    inclination_deg: Rate {
        at_j2000: -0.000_015_31,
        per_century: -0.012_946_68,
    },
    mean_longitude_deg: Rate {
        at_j2000: 100.464_571_66,
        per_century: 35_999.372_449_81,
    },
    longitude_of_periapsis_deg: Rate {
        at_j2000: 102.937_681_93,
        per_century: 0.323_273_64,
    },
    longitude_of_ascending_node_deg: Rate {
        at_j2000: 0.0,
        per_century: 0.0,
    },
};

const MARS_ELEMENTS: KeplerianSeries = KeplerianSeries {
    semi_major_axis_au: Rate {
        at_j2000: 1.523_710_34,
        per_century: 0.000_018_47,
    },
    eccentricity: Rate {
        at_j2000: 0.093_394_10,
        per_century: 0.000_078_82,
    },
    inclination_deg: Rate {
        at_j2000: 1.849_691_42,
        per_century: -0.008_131_31,
    },
    mean_longitude_deg: Rate {
        at_j2000: -4.553_432_05,
        per_century: 19_140.302_684_99,
    },
    longitude_of_periapsis_deg: Rate {
        at_j2000: -23.943_629_59,
        per_century: 0.444_410_88,
    },
    longitude_of_ascending_node_deg: Rate {
        at_j2000: 49.559_538_91,
        per_century: -0.292_573_43,
    },
};

/// A planet as a node in the frame tree: a direct child of the SSB, reporting
/// its heliocentric Keplerian position plus the Sun's own SSB offset.
#[derive(Debug, Clone, Copy)]
pub struct HeliocentricPlanet {
    series: &'static KeplerianSeries,
}

impl Ephemeris for HeliocentricPlanet {
    fn state_at(&self, epoch: Epoch) -> StateVector {
        let elements = self.series.elements_at(epoch);
        let heliocentric = elements.state_at(GM_SUN_KM3_S2, epoch.to_unix_seconds());
        let heliocentric_equatorial = StateVector::new(
            to_equatorial(heliocentric.position_km),
            to_equatorial(heliocentric.velocity_km_s),
        );
        ssb_relative_state(epoch) + heliocentric_equatorial
    }
}

pub const EARTH: HeliocentricPlanet = HeliocentricPlanet {
    series: &EARTH_ELEMENTS,
};
pub const MARS: HeliocentricPlanet = HeliocentricPlanet {
    series: &MARS_ELEMENTS,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earth_stays_close_to_one_astronomical_unit_from_the_sun() {
        for days in [0.0, 90.0, 180.0, 270.0, 3_650.0] {
            let epoch = Epoch::J2000.advanced_by_seconds(days * 86_400.0);
            let sun = ssb_relative_state(epoch);
            let earth = EARTH.state_at(epoch);
            let distance_au = (earth.position_km - sun.position_km).length() / AU_KM;
            assert!(
                (0.98..1.02).contains(&distance_au),
                "{days}d: {distance_au} AU"
            );
        }
    }

    #[test]
    fn mars_stays_close_to_its_known_semi_major_axis() {
        for days in [0.0, 200.0, 400.0, 700.0] {
            let epoch = Epoch::J2000.advanced_by_seconds(days * 86_400.0);
            let sun = ssb_relative_state(epoch);
            let mars = MARS.state_at(epoch);
            let distance_au = (mars.position_km - sun.position_km).length() / AU_KM;
            assert!(
                (1.38..1.67).contains(&distance_au),
                "{days}d: {distance_au} AU"
            );
        }
    }

    #[test]
    fn earth_completes_an_orbit_in_about_a_year() {
        let start = EARTH.state_at(Epoch::J2000).position_km;
        let quarter_year_later = EARTH
            .state_at(Epoch::J2000.advanced_by_seconds(91.31 * 86_400.0))
            .position_km;
        let year_later = EARTH
            .state_at(Epoch::J2000.advanced_by_seconds(365.25 * 86_400.0))
            .position_km;

        // A quarter of the way round should be nowhere near where it started...
        assert!((quarter_year_later - start).length() > AU_KM);
        // ...but a full year should be close to it again (the SSB offset the
        // Sun itself moves by in a year keeps this from being exact).
        assert!(
            (year_later - start).length() < 0.05 * AU_KM,
            "{:?} vs {:?}",
            year_later,
            start
        );
    }

    #[test]
    fn mars_orbits_slower_than_earth() {
        // Kepler's third law: farther out means slower, in a fixed ratio this
        // low-precision model should reproduce from the elements alone.
        let period_from_semi_major_axis = |elements: &KeplerianSeries| {
            let a_au = elements.semi_major_axis_au.at(0.0);
            a_au.powf(1.5) // years, since Earth's is defined as 1 AU / 1 year
        };
        let earth_years = period_from_semi_major_axis(&EARTH_ELEMENTS);
        let mars_years = period_from_semi_major_axis(&MARS_ELEMENTS);
        assert!((earth_years - 1.0).abs() < 0.01, "{earth_years}");
        assert!((mars_years - 1.88).abs() < 0.05, "{mars_years}");
    }

    #[test]
    fn a_planets_position_relative_to_the_ssb_includes_the_suns_own_offset() {
        let epoch = Epoch::J2000;
        let sun = ssb_relative_state(epoch);
        let earth = EARTH.state_at(epoch);
        // Earth relative to the Sun should be about 1 AU regardless of where
        // the Sun itself sits relative to the SSB.
        assert!(((earth - sun).position_km.length() - AU_KM).abs() / AU_KM < 0.02);
        assert!(
            sun.position_km.length() > 1_000.0,
            "the Sun should not sit exactly on the SSB"
        );
    }
}
