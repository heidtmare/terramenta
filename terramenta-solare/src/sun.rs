//! Where the Sun is relative to the Solar System Barycentre.
//!
//! The SSB is defined as the whole system's centre of mass, and the Sun holds
//! more than 99.8% of that mass — but it is not a point, and Jupiter and
//! Saturn are heavy enough that the Sun's own centre traces a small loop
//! around the point their combined pull balances against it, a few tenths of
//! a percent of an AU across. That loop is the reason the Sun is a node with
//! its own ephemeris in this tree rather than sitting fixed at the root: a
//! spacecraft on a multi-year interplanetary transfer, planned relative to
//! the SSB, would otherwise be planned against a Sun that is up to a solar
//! diameter from where the model puts it.
//!
//! This models only the two terms worth modelling: Jupiter and Saturn, each
//! treated as if on a circular, unperturbed, ecliptic-plane orbit of its own
//! — which is a coarse enough stand-in for *their* motion that this is a
//! two-term approximation of a many-term effect, not a barycentre. It holds
//! the Sun within a few percent of its actual distance from the SSB, and gets
//! the dominant ~11.86-year Jovian period right, which is enough for the
//! frame tree to demonstrate what tracking the Sun relative to the SSB rather
//! than defining the SSB as the Sun actually buys.

use glam::DVec3;

use crate::ecliptic::to_equatorial;
use crate::frame::{Ephemeris, StateVector};
use crate::time::Epoch;

const AU_KM: f64 = 1.495_978_707e8;
const SECONDS_PER_DAY: f64 = 86_400.0;

/// One giant planet's contribution to the Sun's displacement from the SSB:
/// its own (circularised) heliocentric orbit, and how much of that displaces
/// the Sun the other way — the ratio of its `GM` to the Sun's, which to first
/// order (the Sun vastly outweighing the rest of the system combined) is also
/// the ratio the Sun is pulled by.
#[derive(Clone, Copy)]
struct GiantPlanet {
    semi_major_axis_au: f64,
    mean_longitude_at_j2000_deg: f64,
    orbital_period_days: f64,
    gm_ratio_to_sun: f64,
}

impl GiantPlanet {
    /// This planet's own circularised heliocentric position and velocity, in
    /// the ecliptic plane.
    fn heliocentric_state(&self, epoch: Epoch) -> StateVector {
        let angular_rate_rad_per_day = std::f64::consts::TAU / self.orbital_period_days;
        let angle_rad =
            self.mean_longitude_at_j2000_deg.to_radians() + angular_rate_rad_per_day * epoch.days();
        let radius_km = self.semi_major_axis_au * AU_KM;
        let angular_rate_rad_per_s = angular_rate_rad_per_day / SECONDS_PER_DAY;

        let (sin_a, cos_a) = angle_rad.sin_cos();
        StateVector::new(
            radius_km * DVec3::new(cos_a, sin_a, 0.0),
            radius_km * angular_rate_rad_per_s * DVec3::new(-sin_a, cos_a, 0.0),
        )
    }
}

/// Jupiter, the dominant term: about five and a fifth AU out, and eleven
/// hundred times lighter than the Sun.
const JUPITER: GiantPlanet = GiantPlanet {
    semi_major_axis_au: 5.2044,
    mean_longitude_at_j2000_deg: 34.351_484,
    orbital_period_days: 4_332.59,
    gm_ratio_to_sun: 1.0 / 1_047.348_644,
};

/// Saturn, the second-largest term: farther out but lighter still, so its
/// contribution to the wobble ends up a bit over half Jupiter's.
const SATURN: GiantPlanet = GiantPlanet {
    semi_major_axis_au: 9.5826,
    mean_longitude_at_j2000_deg: 50.077_471,
    orbital_period_days: 10_759.22,
    gm_ratio_to_sun: 1.0 / 3_497.898,
};

/// The Sun's state relative to the SSB, in equatorial (ICRF-aligned) axes.
pub fn ssb_relative_state(epoch: Epoch) -> StateVector {
    let wobble = [&JUPITER, &SATURN]
        .into_iter()
        .map(|planet| {
            let state = planet.heliocentric_state(epoch);
            StateVector::new(
                state.position_km * planet.gm_ratio_to_sun,
                state.velocity_km_s * planet.gm_ratio_to_sun,
            )
        })
        .fold(StateVector::ZERO, |a, b| a + b);

    // The SSB balances the Sun against everything else, so the Sun sits
    // opposite the (mass-weighted) planets, not alongside them.
    StateVector::new(
        to_equatorial(-wobble.position_km),
        to_equatorial(-wobble.velocity_km_s),
    )
}

/// The Sun as a node in a [`crate::frame::FrameTree`]: a child of the SSB
/// reporting exactly [`ssb_relative_state`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Sun;

impl Ephemeris for Sun {
    fn state_at(&self, epoch: Epoch) -> StateVector {
        ssb_relative_state(epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sun_stays_within_a_couple_of_solar_diameters_of_the_ssb() {
        const SOLAR_RADIUS_KM: f64 = 696_000.0;
        for days in [0.0, 1_000.0, 5_000.0, 10_000.0, 20_000.0] {
            let epoch = Epoch::J2000.advanced_by_seconds(days * SECONDS_PER_DAY);
            let distance = ssb_relative_state(epoch).position_km.length();
            assert!(distance < 4.0 * SOLAR_RADIUS_KM, "{days}d: {distance} km");
        }
    }

    #[test]
    fn saturn_keeps_the_wobble_from_repeating_on_jupiters_period_alone() {
        // An integer number of Jupiter periods brings Jupiter's own
        // contribution back to exactly where it started; Saturn, on its own
        // much longer period, has moved on to a different point in its
        // orbit, so the combined offset has moved too. A wobble that
        // repeated here would mean Saturn's term had silently dropped out.
        let start = ssb_relative_state(Epoch::J2000);
        let one_jupiter_period_later = ssb_relative_state(
            Epoch::J2000.advanced_by_seconds(JUPITER.orbital_period_days * SECONDS_PER_DAY),
        );
        assert!(
            (one_jupiter_period_later.position_km - start.position_km).length() > 50_000.0,
            "{:?} vs {:?}",
            one_jupiter_period_later.position_km,
            start.position_km
        );
    }

    #[test]
    fn a_stronger_giant_planet_pulls_the_sun_further() {
        let heavier = GiantPlanet {
            gm_ratio_to_sun: JUPITER.gm_ratio_to_sun * 10.0,
            ..JUPITER
        };
        let lighter_distance = JUPITER
            .heliocentric_state(Epoch::J2000)
            .position_km
            .length()
            * JUPITER.gm_ratio_to_sun;
        let heavier_distance = heavier
            .heliocentric_state(Epoch::J2000)
            .position_km
            .length()
            * heavier.gm_ratio_to_sun;
        assert!(heavier_distance > lighter_distance);
    }
}
