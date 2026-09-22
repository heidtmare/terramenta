//! Lambert's problem: given where a body starts, where it should end up, and
//! how long it has to get there, the one two-body orbit that connects them —
//! and, with it, the velocity that orbit needs at each end.
//!
//! Two position vectors and a transfer time are geometry alone, silent about
//! how fast anything moves; [`solve`] is the one two-body orbit consistent
//! with the gravity in play and the time available. This is what turns
//! "leave Earth on this date, arrive at Mars on that one" into a
//! spacecraft's required velocity, as opposed to
//! [`crate::mission::hohmann_transfer`]'s always-optimal, always-181-degree
//! ellipse, which asks nothing about *when*, only about how far apart two
//! circular orbits are.
//!
//! Two positions alone are ambiguous about which way around the focus a
//! transfer sweeps; [`TransferDirection`] resolves that ambiguity explicitly.
//!
//! Solves the zero-revolution case only — one arc from departure to arrival,
//! shortest or longest way round depending on [`TransferDirection`], never
//! one that laps the focus first. Every interplanetary transfer
//! [`crate::mission`] plans is exactly that shape; multi-revolution
//! solutions (more fuel-efficient for some very slow transfers, with a
//! second free parameter) are left for whoever needs one.

use glam::DVec3;

/// Which way a transfer sweeps around its focus.
///
/// Two position vectors describe a chord, not a direction of travel — the
/// short way round (less than a half-turn) and the long way round (more
/// than one) are both orbits through the same two points, and nothing about
/// the points themselves says which one a spacecraft should fly. This makes
/// that choice explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Counterclockwise viewed from the north ecliptic pole — the sense
    /// every planet in this crate orbits the Sun in, and so the right
    /// choice for essentially any real interplanetary transfer.
    Prograde,
    Retrograde,
}

/// The velocity a transfer orbit needs at each end.
///
/// Neither vector is a velocity *change* by itself — that is the difference
/// between this and whatever the departing or arriving body was already
/// doing, handled by [`crate::mission::plan_transfer`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LambertSolution {
    pub velocity_at_departure_km_s: DVec3,
    pub velocity_at_arrival_km_s: DVec3,
}

/// Solves Lambert's problem by Vallado's universal-variable formulation:
/// one equation, `t(z) = Δt`, solved for the single parameter `z` that makes
/// the geometry's implied transfer orbit take exactly as long as asked.
///
/// Returns `None` for a transfer this can't resolve: `r1` and `r2` collinear
/// (the transfer angle is exactly `0` or `π`, so no orbital plane is
/// determined), a non-positive time of flight, or the iteration failing to
/// converge — which in practice means a transfer time wildly incompatible
/// with the geometry (impossibly short for the distance involved, most often).
pub fn solve(
    r1: DVec3,
    r2: DVec3,
    time_of_flight_s: f64,
    gm_km3_s2: f64,
    direction: TransferDirection,
) -> Option<LambertSolution> {
    if !time_of_flight_s.is_finite() || time_of_flight_s <= 0.0 {
        return None;
    }

    let r1_mag = r1.length();
    let r2_mag = r2.length();
    let cos_delta_nu = (r1.dot(r2) / (r1_mag * r2_mag)).clamp(-1.0, 1.0);

    // The short way (< π) if the transfer's sense of rotation matches
    // `direction`; the long way (> π) otherwise. `r1 × r2`'s `z` component
    // is positive exactly when going from `r1` to `r2` the short way is
    // counterclockwise about `+z`, i.e. prograde.
    let short_way = match direction {
        TransferDirection::Prograde => r1.cross(r2).z >= 0.0,
        TransferDirection::Retrograde => r1.cross(r2).z < 0.0,
    };
    let delta_nu = if short_way {
        cos_delta_nu.acos()
    } else {
        std::f64::consts::TAU - cos_delta_nu.acos()
    };

    // `A` carries both the geometry and the direction: negative for a
    // long-way transfer, and headed to zero as the transfer angle heads to
    // `0` or `π`, where no single orbital plane is determined by `r1` and
    // `r2` alone.
    let a = delta_nu.sin() * (r1_mag * r2_mag / (1.0 - cos_delta_nu)).sqrt();
    if a.abs() < 1.0e-6 {
        return None;
    }

    let sqrt_gm = gm_km3_s2.sqrt();
    let z = solve_universal_anomaly(time_of_flight_s, r1_mag, r2_mag, a, sqrt_gm)?;
    let y = universal_y(z, r1_mag, r2_mag, a);
    if !y.is_finite() || y < 0.0 {
        return None;
    }

    // The `f` and `g` series, evaluated at the converged `z`: the standard
    // way to turn two positions and the orbit connecting them into the
    // velocities at each end, without ever naming the orbit's own elements.
    let f = 1.0 - y / r1_mag;
    let g = a * (y / gm_km3_s2).sqrt();
    let g_dot = 1.0 - y / r2_mag;
    if !g.is_finite() || g.abs() < 1.0e-9 {
        return None;
    }

    Some(LambertSolution {
        velocity_at_departure_km_s: (r2 - f * r1) / g,
        velocity_at_arrival_km_s: (g_dot * r2 - r1) / g,
    })
}

/// The Stumpff function `C(z)`, by its series near `z = 0` (where the closed
/// forms below divide `0` by `0`) and its closed forms either side of it.
fn stumpff_c(z: f64) -> f64 {
    if z > 1.0e-6 {
        let sqrt_z = z.sqrt();
        (1.0 - sqrt_z.cos()) / z
    } else if z < -1.0e-6 {
        let sqrt_neg_z = (-z).sqrt();
        (sqrt_neg_z.cosh() - 1.0) / (-z)
    } else {
        1.0 / 2.0 - z / 24.0 + z * z / 720.0
    }
}

/// The Stumpff function `S(z)`, on the same terms as [`stumpff_c`].
fn stumpff_s(z: f64) -> f64 {
    if z > 1.0e-6 {
        let sqrt_z = z.sqrt();
        (sqrt_z - sqrt_z.sin()) / sqrt_z.powi(3)
    } else if z < -1.0e-6 {
        let sqrt_neg_z = (-z).sqrt();
        (sqrt_neg_z.sinh() - sqrt_neg_z) / sqrt_neg_z.powi(3)
    } else {
        1.0 / 6.0 - z / 120.0 + z * z / 5040.0
    }
}

/// `y(z)`: the universal-variable stand-in for how far apart `r1` and `r2`
/// end up being explained by the same transfer orbit at parameter `z`.
///
/// Only meaningful where non-negative — [`solve`] treats a negative `y` as
/// this `z` not describing a real transfer.
fn universal_y(z: f64, r1_mag: f64, r2_mag: f64, a: f64) -> f64 {
    r1_mag + r2_mag + a * (z * stumpff_s(z) - 1.0) / stumpff_c(z).sqrt()
}

/// The time of flight a transfer orbit at parameter `z` implies.
fn universal_time_of_flight(z: f64, r1_mag: f64, r2_mag: f64, a: f64, sqrt_gm: f64) -> f64 {
    let y = universal_y(z, r1_mag, r2_mag, a);
    let chi = (y / stumpff_c(z)).sqrt();
    (chi.powi(3) * stumpff_s(z) + a * y.sqrt()) / sqrt_gm
}

/// Solves `universal_time_of_flight(z) == time_of_flight_s` for `z`, by
/// Newton's method against a numerical derivative — the time-of-flight
/// equation has no simple closed-form derivative, and this is not called
/// often enough for one to be worth deriving.
///
/// `z`'s valid domain runs from very negative (a sharply hyperbolic
/// transfer) up to `4π²` exclusive, where the implied transfer orbit's
/// period goes to infinity; past it is a second revolution, not solved for
/// here. When `a > 0` (a short-way transfer), [`universal_y`] can start out
/// negative for `z` near zero on a wide transfer angle — the walk-forward
/// loop below, Vallado's own fix, handles that.
fn solve_universal_anomaly(
    time_of_flight_s: f64,
    r1_mag: f64,
    r2_mag: f64,
    a: f64,
    sqrt_gm: f64,
) -> Option<f64> {
    const MAX_Z: f64 = 4.0 * std::f64::consts::PI * std::f64::consts::PI;
    const MAX_ITERATIONS: usize = 200;
    const TOLERANCE_SECONDS: f64 = 1.0e-6;

    let y_is_valid = |z: f64| a <= 0.0 || universal_y(z, r1_mag, r2_mag, a) >= 0.0;

    let mut z = 0.0;
    let mut steps = 0;
    while !y_is_valid(z) && steps < 1000 {
        z += 0.1;
        steps += 1;
    }
    if !y_is_valid(z) {
        return None;
    }

    for _ in 0..MAX_ITERATIONS {
        let t = universal_time_of_flight(z, r1_mag, r2_mag, a, sqrt_gm);
        let error = t - time_of_flight_s;
        if error.abs() < TOLERANCE_SECONDS {
            return Some(z);
        }

        let step = 1.0e-6 * z.abs().max(1.0);
        let mut probe = (z + step).min(MAX_Z - 1.0e-6);
        // Keep the derivative's probe point inside the domain `y` is
        // defined on, halving back toward `z` (always valid by this point)
        // rather than evaluating the Stumpff functions past it.
        let mut halvings = 0;
        while !y_is_valid(probe) && halvings < 60 {
            probe = (z + probe) / 2.0;
            halvings += 1;
        }
        let derivative =
            (universal_time_of_flight(probe, r1_mag, r2_mag, a, sqrt_gm) - t) / (probe - z);
        if !derivative.is_finite() || derivative.abs() < 1.0e-12 {
            return None;
        }

        let mut next_z = (z - error / derivative).min(MAX_Z - 1.0e-6);
        let mut damping = 0;
        while !y_is_valid(next_z) && damping < 60 {
            next_z = (z + next_z) / 2.0;
            damping += 1;
        }
        z = next_z;
    }

    let final_error = universal_time_of_flight(z, r1_mag, r2_mag, a, sqrt_gm) - time_of_flight_s;
    (final_error.abs() < TOLERANCE_SECONDS * 100.0).then_some(z)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orbit::OrbitalElements;

    const GM_SUN_KM3_S2: f64 = 1.327_124_4e11;
    const GM_EARTH_KM3_S2: f64 = 398_600.441_8;

    /// An eccentric, equatorial (in this test's own axes — the solver knows
    /// nothing of the ecliptic) heliocentric-ish orbit, eccentric enough
    /// that a wrong `z` shows up as a wrong answer rather than an
    /// accidentally correct one.
    fn sample_orbit() -> OrbitalElements {
        OrbitalElements {
            semi_major_axis_km: 1.496e8,
            eccentricity: 0.2,
            inclination_rad: 0.0,
            raan_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_at_epoch: 0.3,
            epoch_seconds: 0.0,
        }
    }

    #[test]
    fn recovers_the_velocities_of_a_known_two_body_arc() {
        let elements = sample_orbit();
        let departure = elements.state_at(GM_SUN_KM3_S2, 0.0);
        // Two months: comfortably under half this orbit's roughly one-year
        // period, so the transfer angle stays under half a turn and
        // `TransferDirection::Prograde` (this orbit's sense of motion, per
        // `orbit::state_at`'s always-positive angular momentum) is the
        // short way round.
        let time_of_flight = 60.0 * 86_400.0;
        let arrival = elements.state_at(GM_SUN_KM3_S2, time_of_flight);

        let solution = solve(
            departure.position_km,
            arrival.position_km,
            time_of_flight,
            GM_SUN_KM3_S2,
            TransferDirection::Prograde,
        )
        .expect("a well-posed short-way transfer should solve");

        assert!(
            (solution.velocity_at_departure_km_s - departure.velocity_km_s).length() < 1.0e-5,
            "{:?} vs {:?}",
            solution.velocity_at_departure_km_s,
            departure.velocity_km_s
        );
        assert!(
            (solution.velocity_at_arrival_km_s - arrival.velocity_km_s).length() < 1.0e-5,
            "{:?} vs {:?}",
            solution.velocity_at_arrival_km_s,
            arrival.velocity_km_s
        );
    }

    #[test]
    fn also_recovers_a_short_earth_orbit_transfer() {
        // A different regime — an Earth-orbit arc a few thousand kilometres
        // out rather than a heliocentric one — so this isn't just a test of
        // one magnitude of number working out.
        let elements = OrbitalElements {
            semi_major_axis_km: 8_000.0,
            eccentricity: 0.1,
            inclination_rad: 0.0,
            raan_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_at_epoch: 0.0,
            epoch_seconds: 0.0,
        };
        let departure = elements.state_at(GM_EARTH_KM3_S2, 0.0);
        let time_of_flight = 1_800.0;
        let arrival = elements.state_at(GM_EARTH_KM3_S2, time_of_flight);

        let solution = solve(
            departure.position_km,
            arrival.position_km,
            time_of_flight,
            GM_EARTH_KM3_S2,
            TransferDirection::Prograde,
        )
        .expect("a well-posed short-way transfer should solve");

        assert!((solution.velocity_at_departure_km_s - departure.velocity_km_s).length() < 1.0e-6);
        assert!((solution.velocity_at_arrival_km_s - arrival.velocity_km_s).length() < 1.0e-6);
    }

    #[test]
    fn the_long_way_round_is_a_different_orbit_than_the_short_way() {
        let elements = sample_orbit();
        let departure = elements.state_at(GM_SUN_KM3_S2, 0.0);
        let time_of_flight = 60.0 * 86_400.0;
        let arrival = elements.state_at(GM_SUN_KM3_S2, time_of_flight);

        let short_way = solve(
            departure.position_km,
            arrival.position_km,
            time_of_flight,
            GM_SUN_KM3_S2,
            TransferDirection::Prograde,
        )
        .unwrap();
        let long_way = solve(
            departure.position_km,
            arrival.position_km,
            time_of_flight,
            GM_SUN_KM3_S2,
            TransferDirection::Retrograde,
        )
        .unwrap();

        assert!(
            (short_way.velocity_at_departure_km_s - long_way.velocity_at_departure_km_s).length()
                > 1.0,
            "the short and long way round should need noticeably different velocities"
        );
    }

    #[test]
    fn collinear_positions_have_no_well_defined_transfer_plane() {
        let r1 = DVec3::new(1.0e8, 0.0, 0.0);
        let r2 = DVec3::new(2.0e8, 0.0, 0.0);
        assert!(solve(r1, r2, 1.0e6, GM_SUN_KM3_S2, TransferDirection::Prograde).is_none());
    }

    #[test]
    fn a_non_positive_time_of_flight_has_no_solution() {
        let r1 = DVec3::new(1.0e8, 0.0, 0.0);
        let r2 = DVec3::new(0.0, 1.5e8, 0.0);
        assert!(solve(r1, r2, 0.0, GM_SUN_KM3_S2, TransferDirection::Prograde).is_none());
        assert!(solve(r1, r2, -1.0e6, GM_SUN_KM3_S2, TransferDirection::Prograde).is_none());
    }
}
