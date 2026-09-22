//! Interplanetary mission planning: turning "leave here on this date, arrive
//! there on that one" into the velocity changes a spacecraft actually needs.
//!
//! Two functions answer two different questions. [`hohmann_transfer`] takes
//! two orbit radii and nothing about *when* — the always-optimal,
//! always-coplanar, always-circular transfer textbooks lead with, and here
//! mostly the number a real plan should be judged against. [`plan_transfer`]
//! takes the two bodies and actual departure and arrival dates, and runs
//! [`crate::lambert::solve`] against wherever they really are on those dates
//! — eccentric, inclined, on whatever schedule the caller asked for rather
//! than the one Hohmann's ellipse would choose. The two agree to within a
//! percent or so when the dates given to the second land on the transfer
//! the first describes; a much shorter transfer than Hohmann's optimal time
//! is what a "fast transfer, more fuel" mission profile costs, made
//! concrete in the delta-v this returns.

use glam::DVec3;

use crate::frame::{FrameId, FrameTree};
use crate::lambert::{self, TransferDirection};
use crate::time::Epoch;

/// The delta-v a Hohmann transfer needs between two circular, coplanar
/// orbits — the textbook baseline, not a plan for any specific date.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HohmannTransfer {
    /// The burn at the inner orbit, raising a circular orbit onto the
    /// transfer ellipse (or lowering it, leaving one — the same manoeuvre
    /// either way, just signed oppositely, which is why this and
    /// `arrival_delta_v_km_s` are plain magnitudes).
    pub departure_delta_v_km_s: f64,
    /// The burn at the outer orbit, circularizing out of the transfer
    /// ellipse.
    pub arrival_delta_v_km_s: f64,
    pub total_delta_v_km_s: f64,
    /// Half the transfer ellipse's own period — the one transfer time a
    /// Hohmann transfer ever takes, since the ellipse itself is fixed by the
    /// two radii.
    pub transfer_time_seconds: f64,
}

/// The minimum-energy transfer between two circular orbits of radius
/// `departure_radius_km` and `arrival_radius_km` around a body of
/// `gm_km3_s2` — Earth and Mars's own mean solar distances and
/// [`crate::bodies::GM_SUN_KM3_S2`] for the interplanetary case this module
/// exists for, though nothing here is specific to the Sun.
///
/// An approximation on two counts: real planetary orbits are eccentric and
/// mutually inclined rather than perfectly circular and coplanar, and this
/// says nothing about escaping the departure planet's own gravity well or
/// being captured by the destination's — both, like
/// [`crate::lambert::solve`] itself, are stated purely in the heliocentric
/// frame.
pub fn hohmann_transfer(
    departure_radius_km: f64,
    arrival_radius_km: f64,
    gm_km3_s2: f64,
) -> HohmannTransfer {
    let transfer_semi_major_axis_km = (departure_radius_km + arrival_radius_km) / 2.0;

    let departure_circular_speed_km_s = (gm_km3_s2 / departure_radius_km).sqrt();
    let arrival_circular_speed_km_s = (gm_km3_s2 / arrival_radius_km).sqrt();

    let transfer_speed_at_departure_km_s =
        (gm_km3_s2 * (2.0 / departure_radius_km - 1.0 / transfer_semi_major_axis_km)).sqrt();
    let transfer_speed_at_arrival_km_s =
        (gm_km3_s2 * (2.0 / arrival_radius_km - 1.0 / transfer_semi_major_axis_km)).sqrt();

    let departure_delta_v_km_s =
        (transfer_speed_at_departure_km_s - departure_circular_speed_km_s).abs();
    let arrival_delta_v_km_s = (arrival_circular_speed_km_s - transfer_speed_at_arrival_km_s).abs();

    HohmannTransfer {
        departure_delta_v_km_s,
        arrival_delta_v_km_s,
        total_delta_v_km_s: departure_delta_v_km_s + arrival_delta_v_km_s,
        transfer_time_seconds: std::f64::consts::PI
            * (transfer_semi_major_axis_km.powi(3) / gm_km3_s2).sqrt(),
    }
}

/// A Lambert-solved transfer between two frames in the tree, on a specific
/// departure and arrival date.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransferPlan {
    pub departure: Epoch,
    pub arrival: Epoch,
    pub time_of_flight_seconds: f64,
    /// The transfer orbit's velocity at departure, less the departing
    /// body's own — the burn a spacecraft already moving with that body
    /// needs to get onto the transfer orbit.
    pub departure_delta_v_km_s: DVec3,
    /// The arriving body's own velocity, less the transfer orbit's — the
    /// burn needed to match it, ignoring whatever capture manoeuvre the
    /// destination's gravity well demands, which a heliocentric Lambert
    /// solution does not account for.
    pub arrival_delta_v_km_s: DVec3,
}

impl TransferPlan {
    pub fn departure_delta_v_magnitude_km_s(&self) -> f64 {
        self.departure_delta_v_km_s.length()
    }

    pub fn arrival_delta_v_magnitude_km_s(&self) -> f64 {
        self.arrival_delta_v_km_s.length()
    }

    pub fn total_delta_v_km_s(&self) -> f64 {
        self.departure_delta_v_magnitude_km_s() + self.arrival_delta_v_magnitude_km_s()
    }
}

/// Plans a transfer from `origin` to `destination` — frames anywhere in
/// `tree`, though in practice the planets [`crate::planets`] adds —
/// departing at `departure` and arriving at `arrival`.
///
/// Both bodies' positions are taken relative to `central_body`, the mass
/// the transfer orbit is shaped by, with `central_body_gm_km3_s2` its `GM`.
/// [`crate::lambert::solve`] needs only the number, not which body it
/// belongs to, so this is as generic over the central body as
/// [`hohmann_transfer`] is over its own `gm_km3_s2`. In practice
/// `central_body` is the Sun and `central_body_gm_km3_s2` is
/// [`crate::bodies::GM_SUN_KM3_S2`] for any transfer between this crate's
/// own [`crate::planets`] bodies. `direction` resolves the same
/// short-way/long-way ambiguity [`crate::lambert::solve`] documents —
/// [`TransferDirection::Prograde`] for essentially any real interplanetary
/// transfer, since every planet this crate models orbits the Sun that way.
///
/// Returns `None` for whatever [`crate::lambert::solve`] would: `arrival`
/// at or before `departure`, or a transfer this simplified two-body
/// geometry cannot resolve — occasionally a genuine launch-window dead
/// zone, more often a transfer time too short for the distance involved.
#[allow(clippy::too_many_arguments)]
pub fn plan_transfer(
    tree: &FrameTree,
    central_body: FrameId,
    central_body_gm_km3_s2: f64,
    origin: FrameId,
    destination: FrameId,
    departure: Epoch,
    arrival: Epoch,
    direction: TransferDirection,
) -> Option<TransferPlan> {
    let time_of_flight_seconds = arrival.to_unix_seconds() - departure.to_unix_seconds();

    let departure_state = tree.state_of_relative_to(origin, central_body, departure);
    let arrival_state = tree.state_of_relative_to(destination, central_body, arrival);

    let solution = lambert::solve(
        departure_state.position_km,
        arrival_state.position_km,
        time_of_flight_seconds,
        central_body_gm_km3_s2,
        direction,
    )?;

    Some(TransferPlan {
        departure,
        arrival,
        time_of_flight_seconds,
        departure_delta_v_km_s: solution.velocity_at_departure_km_s - departure_state.velocity_km_s,
        arrival_delta_v_km_s: arrival_state.velocity_km_s - solution.velocity_at_arrival_km_s,
    })
}

/// `steps` epochs evenly spaced from `start` to `end`, inclusive of both.
/// `steps <= 1` collapses to just `start`.
fn linspace_epochs(start: Epoch, end: Epoch, steps: usize) -> Vec<Epoch> {
    if steps <= 1 {
        return vec![start];
    }
    let start_seconds = start.to_unix_seconds();
    let end_seconds = end.to_unix_seconds();
    (0..steps)
        .map(|step| {
            let fraction = step as f64 / (steps - 1) as f64;
            Epoch::from_unix_seconds(start_seconds + (end_seconds - start_seconds) * fraction)
        })
        .collect()
}

/// Scans a grid of candidate departure/arrival dates for the single cheapest
/// [`plan_transfer`] solution — for an automated caller that needs one
/// number, rather than the whole grid `terramenta-charta` draws a porkchop
/// plot from.
///
/// Cost is `O(departure_steps * arrival_steps)` Lambert solves; keep the
/// axes coarse for a caller on a tight startup budget. Returns `None` only
/// if nothing on the grid solves, which shouldn't happen for an Earth-Mars
/// grid spanning a full synodic period (~780 days) of departure dates.
#[allow(clippy::too_many_arguments)]
pub fn find_best_transfer_window(
    tree: &FrameTree,
    central_body: FrameId,
    central_body_gm_km3_s2: f64,
    origin: FrameId,
    destination: FrameId,
    departure_start: Epoch,
    departure_end: Epoch,
    departure_steps: usize,
    arrival_start: Epoch,
    arrival_end: Epoch,
    arrival_steps: usize,
    direction: TransferDirection,
) -> Option<TransferPlan> {
    let departures = linspace_epochs(departure_start, departure_end, departure_steps);
    let arrivals = linspace_epochs(arrival_start, arrival_end, arrival_steps);

    departures
        .iter()
        .flat_map(|&departure| arrivals.iter().map(move |&arrival| (departure, arrival)))
        .filter_map(|(departure, arrival)| {
            plan_transfer(
                tree,
                central_body,
                central_body_gm_km3_s2,
                origin,
                destination,
                departure,
                arrival,
                direction,
            )
        })
        .min_by(|a, b| {
            a.total_delta_v_km_s()
                .partial_cmp(&b.total_delta_v_km_s())
                .expect("delta-v is never NaN for a transfer plan_transfer returned")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bodies::GM_SUN_KM3_S2;
    use crate::solar_system;

    const AU_KM: f64 = 1.495_978_707e8;
    const MARS_SEMI_MAJOR_AXIS_KM: f64 = 1.523_710_34 * AU_KM;

    #[test]
    fn a_hohmann_earth_mars_transfer_matches_the_textbook_ballpark() {
        // Heliocentric-only figures — no departure hyperbola or capture burn
        // folded in — which is what puts the well-known "~5.7 km/s including
        // Earth departure" figure outside this range.
        let transfer = hohmann_transfer(AU_KM, MARS_SEMI_MAJOR_AXIS_KM, GM_SUN_KM3_S2);
        assert!(
            (2.5..3.5).contains(&transfer.departure_delta_v_km_s),
            "{transfer:?}"
        );
        assert!(
            (2.0..3.0).contains(&transfer.arrival_delta_v_km_s),
            "{transfer:?}"
        );
        // A bit over half a year — the number every Earth-Mars mission
        // planning table quotes as "about 260 days".
        let transfer_days = transfer.transfer_time_seconds / 86_400.0;
        assert!((250.0..270.0).contains(&transfer_days), "{transfer_days}");
    }

    #[test]
    fn a_wider_orbit_gap_costs_more_delta_v() {
        let near = hohmann_transfer(AU_KM, 1.1 * AU_KM, GM_SUN_KM3_S2);
        let far = hohmann_transfer(AU_KM, MARS_SEMI_MAJOR_AXIS_KM, GM_SUN_KM3_S2);
        assert!(far.total_delta_v_km_s > near.total_delta_v_km_s);
    }

    #[test]
    fn plans_a_lambert_transfer_from_earth_to_mars() {
        let tree = solar_system();
        let (sun, earth, mars) = (
            tree.find("Sun").unwrap(),
            tree.find("Earth").unwrap(),
            tree.find("Mars").unwrap(),
        );

        // Earth and Mars only line up for an efficient transfer roughly once
        // a synodic period (~780 days) — this pair, found by scanning for
        // the window nearest J2000, is one of those, not an arbitrary date.
        // A transfer at a random phase can cost several times this.
        let departure = Epoch::J2000.advanced_by_seconds(1_253.0 * 86_400.0);
        let arrival = departure.advanced_by_seconds(204.0 * 86_400.0);

        let plan = plan_transfer(
            &tree,
            sun,
            GM_SUN_KM3_S2,
            earth,
            mars,
            departure,
            arrival,
            TransferDirection::Prograde,
        )
        .expect("a real Earth-Mars launch window should solve");

        // Close to the heliocentric Hohmann figure for this pair
        // (about 5.6 km/s combined).
        assert!((4.0..8.0).contains(&plan.total_delta_v_km_s()), "{plan:?}");
    }

    #[test]
    fn finds_the_known_good_window_inside_a_wider_search_grid() {
        let tree = solar_system();
        let (sun, earth, mars) = (
            tree.find("Sun").unwrap(),
            tree.find("Earth").unwrap(),
            tree.find("Mars").unwrap(),
        );

        // Wide enough to contain the known-good window
        // `plans_a_lambert_transfer_from_earth_to_mars` uses (departure
        // +1253 days, arrival 204 days later), but coarse, like a real
        // search.
        let departure_start = Epoch::J2000.advanced_by_seconds(1_200.0 * 86_400.0);
        let departure_end = Epoch::J2000.advanced_by_seconds(1_320.0 * 86_400.0);
        let arrival_start = Epoch::J2000.advanced_by_seconds(1_400.0 * 86_400.0);
        let arrival_end = Epoch::J2000.advanced_by_seconds(1_520.0 * 86_400.0);

        let plan = find_best_transfer_window(
            &tree,
            sun,
            GM_SUN_KM3_S2,
            earth,
            mars,
            departure_start,
            departure_end,
            13,
            arrival_start,
            arrival_end,
            13,
            TransferDirection::Prograde,
        )
        .expect("a grid spanning a real Earth-Mars window should find a solvable transfer");

        // Should match the ballpark of the known-good window, not wander
        // onto a costlier corner of the grid.
        assert!((3.0..10.0).contains(&plan.total_delta_v_km_s()), "{plan:?}");
    }

    #[test]
    fn an_arrival_before_departure_has_no_plan() {
        let tree = solar_system();
        let (sun, earth, mars) = (
            tree.find("Sun").unwrap(),
            tree.find("Earth").unwrap(),
            tree.find("Mars").unwrap(),
        );
        let departure = Epoch::J2000;
        let arrival = departure.advanced_by_seconds(-1.0);

        assert!(
            plan_transfer(
                &tree,
                sun,
                GM_SUN_KM3_S2,
                earth,
                mars,
                departure,
                arrival,
                TransferDirection::Prograde
            )
            .is_none()
        );
    }
}
