//! Gravity-assist transfers: two Lambert legs joined at an intermediate body,
//! and the guidance that flies them under patched conics.
//!
//! Powered flyby, impulse at periapsis. Turn `δ = ∠(v∞in, v∞out)` splits as
//! `δ = asin(1/e_in) + asin(1/e_out)`, `e = 1 + r_p v∞²/μ`; solved for `r_p` by
//! bisection. Periapsis Δv `= |√(v∞out² + 2μ/r_p) − √(v∞in² + 2μ/r_p)|`.
//!
//! Guidance ([`FlybyGuidance`]) adds the corrections patched conics needs:
//! - cruise: at origin SOI exit, Lambert to the flyby aim point (B-plane offset);
//! - approach: at flyby-body SOI entry, Lambert to the designed periapsis;
//! - periapsis: the powered-flyby burn, along velocity;
//! - retarget: at flyby-body SOI exit, Lambert to the destination.

use glam::DVec3;

use crate::frame::{FrameId, FrameTree, StateVector};
use crate::lambert::{self, TransferDirection};
use crate::mission::{TransferPlan, linspace_epochs, plan_transfer};
use crate::orbit::OrbitalElements;
use crate::spacecraft::{PatchedConicSpacecraft, Primary};
use crate::time::Epoch;

/// Hyperbola at the flyby body, body-centred ICRF.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlybyGeometry {
    pub v_infinity_in_km_s: DVec3,
    pub v_infinity_out_km_s: DVec3,
    pub turn_angle_rad: f64,
    pub periapsis_radius_km: f64,
    /// Magnitude of the periapsis burn; zero for an unpowered flyby.
    pub periapsis_delta_v_km_s: f64,
}

impl FlybyGeometry {
    /// `None` if either v∞ is zero or they are (anti)parallel.
    pub fn solve(v_in: DVec3, v_out: DVec3, gm_km3_s2: f64) -> Option<Self> {
        let (vi, vo) = (v_in.length(), v_out.length());
        if vi < 1e-9 || vo < 1e-9 || v_in.cross(v_out).length() < 1e-12 * vi * vo {
            return None;
        }
        let delta = (v_in.dot(v_out) / (vi * vo)).clamp(-1.0, 1.0).acos();
        let half = |rp: f64, v: f64| (1.0 / (1.0 + rp * v * v / gm_km3_s2)).asin();
        // Decreasing in r_p: π − δ at 0, → −δ as r_p → ∞.
        let residual = |rp: f64| half(rp, vi) + half(rp, vo) - delta;

        let mut hi = gm_km3_s2 / (vi.min(vo) * vi.max(vo));
        for _ in 0..200 {
            if residual(hi) < 0.0 {
                break;
            }
            hi *= 2.0;
        }
        let mut lo = 0.0;
        for _ in 0..200 {
            let mid = 0.5 * (lo + hi);
            if residual(mid) > 0.0 {
                lo = mid;
            } else {
                hi = mid;
            }
            if hi - lo < 1e-9 * hi {
                break;
            }
        }
        let rp = 0.5 * (lo + hi);
        let periapsis_speed = |v: f64| (v * v + 2.0 * gm_km3_s2 / rp).sqrt();
        Some(Self {
            v_infinity_in_km_s: v_in,
            v_infinity_out_km_s: v_out,
            turn_angle_rad: delta,
            periapsis_radius_km: rp,
            periapsis_delta_v_km_s: (periapsis_speed(vo) - periapsis_speed(vi)).abs(),
        })
    }

    /// Flyby plane normal, `v̂in × v̂out`; the orbit's angular momentum.
    fn normal(&self) -> DVec3 {
        self.v_infinity_in_km_s
            .cross(self.v_infinity_out_km_s)
            .normalize()
    }

    fn periapsis_direction(&self, gm_km3_s2: f64) -> (DVec3, DVec3) {
        let n = self.normal();
        let vi = self.v_infinity_in_km_s.length();
        let e_in = 1.0 + self.periapsis_radius_km * vi * vi / gm_km3_s2;
        let (sin, cos) = (1.0 / e_in).asin().sin_cos();
        let v_hat = self.v_infinity_in_km_s / vi;
        let velocity = v_hat * cos + n.cross(v_hat) * sin;
        (velocity.cross(n), velocity)
    }

    /// Periapsis state on the incoming hyperbola (before the burn).
    pub fn periapsis_state(&self, gm_km3_s2: f64) -> StateVector {
        let (r_hat, v_hat) = self.periapsis_direction(gm_km3_s2);
        let rp = self.periapsis_radius_km;
        let vi = self.v_infinity_in_km_s.length();
        StateVector::new(r_hat * rp, v_hat * (vi * vi + 2.0 * gm_km3_s2 / rp).sqrt())
    }

    /// B-vector: offset of the incoming asymptote from the body centre, km.
    pub fn aim_offset_km(&self, gm_km3_s2: f64) -> DVec3 {
        let (r_hat, _) = self.periapsis_direction(gm_km3_s2);
        let v_hat = self.v_infinity_in_km_s.normalize();
        let rp = self.periapsis_radius_km;
        let vi = self.v_infinity_in_km_s.length();
        let b = (rp * rp + 2.0 * gm_km3_s2 * rp / (vi * vi)).sqrt();
        (r_hat - v_hat * r_hat.dot(v_hat)).normalize() * b
    }
}

/// Flyby body: frame, GM, lowest allowed periapsis radius (surface + margin).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlybyBody {
    pub frame: FrameId,
    pub gm_km3_s2: f64,
    pub min_periapsis_radius_km: f64,
}

/// Origin → flyby body → destination, Sun-centred legs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlybyPlan {
    pub first_leg: TransferPlan,
    pub second_leg: TransferPlan,
    pub flyby: FlybyGeometry,
}

impl FlybyPlan {
    pub fn flyby_epoch(&self) -> Epoch {
        self.first_leg.arrival
    }

    /// Departure v∞ + periapsis burn + arrival v∞, km/s.
    pub fn total_delta_v_km_s(&self) -> f64 {
        self.first_leg.departure_delta_v_magnitude_km_s()
            + self.flyby.periapsis_delta_v_km_s
            + self.second_leg.arrival_delta_v_magnitude_km_s()
    }

    /// First-leg departure, second-leg arrival, as one plan.
    pub fn overall(&self) -> TransferPlan {
        TransferPlan {
            departure: self.first_leg.departure,
            arrival: self.second_leg.arrival,
            time_of_flight_seconds: self
                .second_leg
                .arrival
                .seconds_since(self.first_leg.departure),
            departure_delta_v_km_s: self.first_leg.departure_delta_v_km_s,
            arrival_delta_v_km_s: self.second_leg.arrival_delta_v_km_s,
        }
    }
}

/// Lambert legs on the given dates, joined by a powered flyby. `None` if a leg
/// does not solve or periapsis falls below `via.min_periapsis_radius_km`.
#[allow(clippy::too_many_arguments)]
pub fn plan_flyby(
    tree: &FrameTree,
    sun: FrameId,
    gm_sun_km3_s2: f64,
    origin: FrameId,
    via: &FlybyBody,
    destination: FrameId,
    departure: Epoch,
    flyby: Epoch,
    arrival: Epoch,
) -> Option<FlybyPlan> {
    let leg = |from, to, start, end| {
        plan_transfer(
            tree,
            sun,
            gm_sun_km3_s2,
            from,
            to,
            start,
            end,
            TransferDirection::Prograde,
        )
    };
    let first_leg = leg(origin, via.frame, departure, flyby)?;
    let second_leg = leg(via.frame, destination, flyby, arrival)?;
    // `arrival_delta_v` is body − spacecraft: v∞in is its negation.
    let geometry = FlybyGeometry::solve(
        -first_leg.arrival_delta_v_km_s,
        second_leg.departure_delta_v_km_s,
        via.gm_km3_s2,
    )?;
    (geometry.periapsis_radius_km >= via.min_periapsis_radius_km).then_some(FlybyPlan {
        first_leg,
        second_leg,
        flyby: geometry,
    })
}

/// Grid for [`find_best_flyby`]: departure dates × leg durations (days).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlybySearch {
    pub departure_start: Epoch,
    pub departure_end: Epoch,
    pub first_leg_days: (f64, f64),
    pub second_leg_days: (f64, f64),
    /// Per axis; `steps³` candidates per pass.
    pub steps: usize,
}

/// Refinement passes after the coarse grid, each one cell either side of the
/// incumbent at the same step count.
const REFINEMENTS: usize = 3;
const DAY: f64 = 86_400.0;

/// Cheapest [`plan_flyby`] by [`FlybyPlan::total_delta_v_km_s`] over `search`.
pub fn find_best_flyby(
    tree: &FrameTree,
    sun: FrameId,
    gm_sun_km3_s2: f64,
    origin: FrameId,
    via: &FlybyBody,
    destination: FrameId,
    search: &FlybySearch,
) -> Option<FlybyPlan> {
    let steps = search.steps.max(2);
    let scan = |departures: Vec<Epoch>, first: (f64, f64), second: (f64, f64)| {
        let first_days = linspace(first, steps);
        let second_days = linspace(second, steps);
        let mut best: Option<FlybyPlan> = None;
        for &departure in &departures {
            for &t1 in &first_days {
                let flyby = departure.advanced_by_seconds(t1 * DAY);
                for &t2 in &second_days {
                    let arrival = flyby.advanced_by_seconds(t2 * DAY);
                    let Some(plan) = plan_flyby(
                        tree,
                        sun,
                        gm_sun_km3_s2,
                        origin,
                        via,
                        destination,
                        departure,
                        flyby,
                        arrival,
                    ) else {
                        continue;
                    };
                    if best.is_none_or(|b| plan.total_delta_v_km_s() < b.total_delta_v_km_s()) {
                        best = Some(plan);
                    }
                }
            }
        }
        best
    };

    let mut best = scan(
        linspace_epochs(search.departure_start, search.departure_end, steps),
        search.first_leg_days,
        search.second_leg_days,
    )?;
    let cell = |(lo, hi): (f64, f64)| (hi - lo) / (steps - 1) as f64;
    let mut departure_cell =
        search.departure_end.seconds_since(search.departure_start) / DAY / (steps - 1) as f64;
    let (mut first_cell, mut second_cell) =
        (cell(search.first_leg_days), cell(search.second_leg_days));
    for _ in 0..REFINEMENTS {
        let around = |center: f64, half: f64, (lo, hi): (f64, f64)| {
            ((center - half).max(lo), (center + half).min(hi))
        };
        let t1 = best.first_leg.time_of_flight_seconds / DAY;
        let t2 = best.second_leg.time_of_flight_seconds / DAY;
        let departure = best.first_leg.departure;
        let departures = linspace_epochs(
            departure.advanced_by_seconds(-departure_cell * DAY),
            departure.advanced_by_seconds(departure_cell * DAY),
            steps,
        );
        if let Some(refined) = scan(
            departures,
            around(t1, first_cell, search.first_leg_days),
            around(t2, second_cell, search.second_leg_days),
        ) && refined.total_delta_v_km_s() < best.total_delta_v_km_s()
        {
            best = refined;
        }
        let shrink = 2.0 / (steps - 1) as f64;
        departure_cell *= shrink;
        first_cell *= shrink;
        second_cell *= shrink;
    }
    Some(best)
}

fn linspace((lo, hi): (f64, f64), steps: usize) -> Vec<f64> {
    (0..steps)
        .map(|i| lo + (hi - lo) * i as f64 / (steps - 1) as f64)
        .collect()
}

/// Primary chain for a flyby trip: origin → Sun (captures `via`) → `via` →
/// Sun (captures `destination`).
pub fn flyby_primary_chain(
    origin: Primary,
    sun: FrameId,
    via: Primary,
    destination: Primary,
) -> Primary {
    let via = Primary {
        orbits: Some(Box::new(
            Primary::sun(sun).with_capture_candidates(vec![destination]),
        )),
        ..via
    };
    Primary {
        orbits: Some(Box::new(
            Primary::sun(sun).with_capture_candidates(vec![via]),
        )),
        ..origin
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlybyBurnKind {
    Cruise,
    Approach,
    Periapsis,
    Retarget,
}

impl FlybyBurnKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Cruise => "cruise",
            Self::Approach => "approach",
            Self::Periapsis => "periapsis",
            Self::Retarget => "retarget",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlybyBurn {
    pub kind: FlybyBurnKind,
    pub epoch: Epoch,
    pub delta_v_km_s: DVec3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Departing,
    Cruise,
    Encounter,
    Outbound,
    Arrived,
}

/// Sub-step limits for [`FlybyGuidance::advance`], s.
const NEAR_BODY_STEP: f64 = 600.0;
const CRUISE_STEP: f64 = 6.0 * 3_600.0;

/// Flies a [`FlybyPlan`] on a [`PatchedConicSpacecraft`] spawned with
/// [`flyby_primary_chain`]. Sub-steps forward so SOI crossings and the
/// periapsis burn land on time at any clock rate; backward is a plain
/// [`PatchedConicSpacecraft::update`] (no burn is undone).
#[derive(Debug, Clone)]
pub struct FlybyGuidance {
    plan: FlybyPlan,
    sun: FrameId,
    via: FrameId,
    via_gm_km3_s2: f64,
    destination: FrameId,
    stage: Stage,
    last: Epoch,
    /// Periapsis epoch the approach correction targets; the periapsis burn's.
    periapsis: Epoch,
    burns: Vec<FlybyBurn>,
}

impl FlybyGuidance {
    /// `launch`: the epoch the spacecraft was spawned at.
    pub fn new(
        plan: FlybyPlan,
        sun: FrameId,
        via: &FlybyBody,
        destination: FrameId,
        launch: Epoch,
    ) -> Self {
        Self {
            plan,
            sun,
            via: via.frame,
            via_gm_km3_s2: via.gm_km3_s2,
            destination,
            stage: Stage::Departing,
            last: launch,
            periapsis: plan.first_leg.arrival,
            burns: Vec::new(),
        }
    }

    pub fn plan(&self) -> &FlybyPlan {
        &self.plan
    }

    /// Executed burns, in order.
    pub fn burns(&self) -> &[FlybyBurn] {
        &self.burns
    }

    /// Advances `craft` to `epoch`. Returns whether its primary changed.
    pub fn advance(
        &mut self,
        craft: &mut PatchedConicSpacecraft,
        tree: &mut FrameTree,
        epoch: Epoch,
    ) -> bool {
        if epoch.seconds_since(self.last) <= 0.0 {
            self.last = epoch;
            return craft.update(tree, epoch);
        }
        let mut changed = false;
        while epoch.seconds_since(self.last) > 0.0 {
            let step = if craft.primary_frame() == self.sun {
                CRUISE_STEP
            } else {
                NEAR_BODY_STEP
            };
            let mut next = self.last.advanced_by_seconds(step);
            if next.seconds_since(epoch) > 0.0 {
                next = epoch;
            }
            let periapsis = self.periapsis;
            let burn_due = self.stage == Stage::Encounter
                && periapsis.seconds_since(self.last) > 0.0
                && next.seconds_since(periapsis) >= 0.0;
            if burn_due {
                next = periapsis;
            }
            let before = craft.primary_frame();
            if craft.update(tree, next) {
                changed = true;
                self.on_transition(craft, tree, before, next);
            }
            if burn_due && self.stage == Stage::Encounter && craft.primary_frame() == self.via {
                self.periapsis_burn(craft, tree, next);
            }
            self.last = next;
        }
        changed
    }

    fn on_transition(
        &mut self,
        craft: &mut PatchedConicSpacecraft,
        tree: &mut FrameTree,
        from: FrameId,
        now: Epoch,
    ) {
        let primary = craft.primary_frame();
        match self.stage {
            Stage::Departing if primary == self.sun => {
                self.stage = Stage::Cruise;
                let flyby = self.plan.flyby_epoch();
                let target = tree.position_of_relative_to(self.via, self.sun, flyby)
                    + self.plan.flyby.aim_offset_km(self.via_gm_km3_s2);
                self.correct(craft, tree, now, target, flyby, FlybyBurnKind::Cruise);
            }
            Stage::Cruise if primary == self.via => {
                self.stage = Stage::Encounter;
                // Osculating periapsis time: M < 0 inbound.
                let gm = self.via_gm_km3_s2;
                let elements =
                    OrbitalElements::from_state(craft.state_at(now), gm, now.tt_seconds());
                let mean_motion = (gm / elements.semi_major_axis_km.abs().powi(3)).sqrt();
                if elements.eccentricity > 1.0 && elements.mean_anomaly_at_epoch < 0.0 {
                    self.periapsis =
                        now.advanced_by_seconds(-elements.mean_anomaly_at_epoch / mean_motion);
                }
                let target = self.plan.flyby.periapsis_state(gm).position_km;
                self.correct(
                    craft,
                    tree,
                    now,
                    target,
                    self.periapsis,
                    FlybyBurnKind::Approach,
                );
            }
            Stage::Encounter if primary == self.sun && from == self.via => {
                self.stage = Stage::Outbound;
                let arrival = self.plan.second_leg.arrival;
                let target = tree.position_of_relative_to(self.destination, self.sun, arrival);
                self.correct(craft, tree, now, target, arrival, FlybyBurnKind::Retarget);
            }
            _ if primary == self.destination => self.stage = Stage::Arrived,
            _ => {}
        }
    }

    /// Lambert from the current state (primary-centred) to `target` at `at`:
    /// prograde about the Sun, short way about a planet. Skipped if `at` has
    /// passed or Lambert fails.
    fn correct(
        &mut self,
        craft: &mut PatchedConicSpacecraft,
        tree: &mut FrameTree,
        now: Epoch,
        target: DVec3,
        at: Epoch,
        kind: FlybyBurnKind,
    ) {
        let tof = at.seconds_since(now);
        if tof <= 0.0 {
            return;
        }
        let state = craft.state_at(now);
        let direction =
            if craft.primary_frame() == self.sun || state.position_km.cross(target).z >= 0.0 {
                TransferDirection::Prograde
            } else {
                TransferDirection::Retrograde
            };
        let Some(solution) = lambert::solve(
            state.position_km,
            target,
            tof,
            craft.primary_gm_km3_s2(),
            direction,
        ) else {
            return;
        };
        self.burn(
            craft,
            tree,
            kind,
            now,
            solution.velocity_at_departure_km_s - state.velocity_km_s,
        );
    }

    /// Along velocity, to the designed outgoing energy at the current radius.
    fn periapsis_burn(
        &mut self,
        craft: &mut PatchedConicSpacecraft,
        tree: &mut FrameTree,
        now: Epoch,
    ) {
        let state = craft.state_at(now);
        let v_out = self.plan.flyby.v_infinity_out_km_s.length();
        let speed = state.velocity_km_s.length();
        let target = (v_out * v_out + 2.0 * self.via_gm_km3_s2 / state.position_km.length()).sqrt();
        let delta_v = state.velocity_km_s / speed * (target - speed);
        self.burn(craft, tree, FlybyBurnKind::Periapsis, now, delta_v);
    }

    fn burn(
        &mut self,
        craft: &mut PatchedConicSpacecraft,
        tree: &mut FrameTree,
        kind: FlybyBurnKind,
        epoch: Epoch,
        delta_v_km_s: DVec3,
    ) {
        craft.apply_impulse(tree, epoch, delta_v_km_s);
        self.burns.push(FlybyBurn {
            kind,
            epoch,
            delta_v_km_s,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bodies::{GM_SUN_KM3_S2, GM_VENUS_KM3_S2, VENUS_MEAN_RADIUS_KM};
    use crate::departure::escape_injection_state;
    use crate::{bodies, solar_system};

    #[test]
    fn unpowered_flyby_matches_the_closed_form() {
        // sin(δ/2) = 1/e for equal v∞.
        let gm = GM_VENUS_KM3_S2;
        let v_in = DVec3::new(5.0, 0.0, 0.0);
        let turn = 40f64.to_radians();
        let v_out = DVec3::new(turn.cos(), turn.sin(), 0.0) * 5.0;
        let g = FlybyGeometry::solve(v_in, v_out, gm).unwrap();
        let e = 1.0 / (turn / 2.0).sin();
        let rp = (e - 1.0) * gm / 25.0;
        assert!((g.periapsis_radius_km - rp).abs() < 1e-3, "{g:?}");
        assert!(g.periapsis_delta_v_km_s < 1e-9);
    }

    #[test]
    fn the_designed_hyperbola_turns_v_infinity_in_into_v_infinity_out() {
        let gm = GM_VENUS_KM3_S2;
        let v_in = DVec3::new(4.0, -1.0, 0.5);
        let v_out = DVec3::new(2.0, 3.5, -0.3);
        let g = FlybyGeometry::solve(v_in, v_out, gm).unwrap();
        let p = g.periapsis_state(gm);
        assert!(
            p.position_km.dot(p.velocity_km_s).abs()
                < 1e-6 * p.velocity_km_s.length() * g.periapsis_radius_km
        );

        // Incoming: back along the hyperbola to the asymptote.
        let incoming = OrbitalElements::from_state(p, gm, 0.0);
        let far_in = incoming.state_at(gm, -400.0 * DAY);
        assert!(
            far_in.velocity_km_s.angle_between(v_in) < 1e-2,
            "{:?}",
            far_in.velocity_km_s
        );

        // Outgoing: after the periapsis burn.
        let speed = (v_out.length_squared() + 2.0 * gm / g.periapsis_radius_km).sqrt();
        let burned = StateVector::new(p.position_km, p.velocity_km_s.normalize() * speed);
        let outgoing = OrbitalElements::from_state(burned, gm, 0.0);
        let far_out = outgoing.state_at(gm, 400.0 * DAY);
        assert!(
            far_out.velocity_km_s.angle_between(v_out) < 1e-2,
            "{:?}",
            far_out.velocity_km_s
        );
        assert!((far_out.velocity_km_s.length() - v_out.length()).abs() < 1e-2);

        // B-plane offset ⟂ v∞in, larger than r_p.
        let b = g.aim_offset_km(gm);
        assert!(b.dot(v_in).abs() < 1e-6 * b.length() * v_in.length());
        assert!(b.length() > g.periapsis_radius_km);
    }

    fn venus_body(tree: &FrameTree) -> FlybyBody {
        FlybyBody {
            frame: tree.find("Venus").unwrap(),
            gm_km3_s2: GM_VENUS_KM3_S2,
            min_periapsis_radius_km: VENUS_MEAN_RADIUS_KM + 300.0,
        }
    }

    /// 2034 Earth–Venus–Mars window: near-unpowered flyby.
    fn search() -> FlybySearch {
        FlybySearch {
            departure_start: Epoch::from_utc(2034, 6, 1, 0, 0, 0.0),
            departure_end: Epoch::from_utc(2034, 9, 30, 0, 0, 0.0),
            first_leg_days: (80.0, 250.0),
            second_leg_days: (120.0, 400.0),
            steps: 13,
        }
    }

    fn best() -> (FrameTree, FlybyPlan) {
        let tree = solar_system();
        let via = venus_body(&tree);
        let plan = find_best_flyby(
            &tree,
            tree.find("Sun").unwrap(),
            GM_SUN_KM3_S2,
            tree.find("Earth").unwrap(),
            &via,
            tree.find("Mars").unwrap(),
            &search(),
        )
        .expect("an Earth–Venus–Mars window in 2034");
        (tree, plan)
    }

    #[test]
    fn finds_an_earth_venus_mars_window() {
        let (_, plan) = best();
        assert!(plan.total_delta_v_km_s() < 10.0, "{plan:?}");
        assert!(plan.flyby.periapsis_delta_v_km_s < 0.1, "{plan:?}");
        assert!(plan.flyby.periapsis_radius_km >= VENUS_MEAN_RADIUS_KM + 300.0);
        assert!(plan.flyby.turn_angle_rad > 0.0);
    }

    #[test]
    fn guidance_flies_earth_venus_mars_under_patched_conics() {
        let (mut tree, plan) = best();
        let (sun, earth, mars) = (
            tree.find("Sun").unwrap(),
            tree.find("Earth").unwrap(),
            tree.find("Mars").unwrap(),
        );
        let via = venus_body(&tree);
        let departure = plan.first_leg.departure;
        let earth_sun = tree.position_of_relative_to(earth, sun, departure).length();
        let soi = bodies::sphere_of_influence_km(bodies::GM_EARTH_KM3_S2, GM_SUN_KM3_S2, earth_sun);
        let state = escape_injection_state(
            plan.first_leg.departure_delta_v_km_s,
            soi,
            bodies::EARTH_MEAN_RADIUS_KM + 300.0,
            bodies::GM_EARTH_KM3_S2,
        );
        let chain = flyby_primary_chain(
            Primary::earth(earth, sun),
            sun,
            Primary::venus(via.frame, sun),
            Primary::mars(mars, sun),
        );
        let mut craft = PatchedConicSpacecraft::spawn(&mut tree, "EVM", chain, state, departure);
        let mut guidance = FlybyGuidance::new(plan, sun, &via, mars, departure);

        let mut primaries = vec![earth];
        let end = plan.second_leg.arrival.advanced_by_seconds(30.0 * DAY);
        let mut t = departure;
        let mut periapsis_radius = None;
        while end.seconds_since(t) > 0.0 {
            // Coarse ticks: guidance sub-steps.
            t = t.advanced_by_seconds(DAY);
            if guidance.advance(&mut craft, &mut tree, t) {
                primaries.push(craft.primary_frame());
            }
            if craft.primary_frame() == via.frame
                && let Some(burn) = guidance.burns().get(2)
            {
                periapsis_radius.get_or_insert(craft.state_at(burn.epoch).position_km.length());
            }
            if craft.primary_frame() == mars {
                break;
            }
        }
        assert_eq!(
            primaries,
            vec![earth, sun, via.frame, sun, mars],
            "{:?}",
            guidance.burns()
        );
        let kinds: Vec<_> = guidance.burns().iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                FlybyBurnKind::Cruise,
                FlybyBurnKind::Approach,
                FlybyBurnKind::Periapsis,
                FlybyBurnKind::Retarget
            ]
        );
        // Periapsis burn within hours of the heliocentric plan's flyby epoch.
        let periapsis = guidance.burns()[2];
        assert!(periapsis.epoch.seconds_since(plan.flyby_epoch()).abs() < 12.0 * 3_600.0);
        assert!(
            (periapsis.delta_v_km_s.length() - plan.flyby.periapsis_delta_v_km_s).abs() < 0.1,
            "{periapsis:?} vs {:?}",
            plan.flyby
        );
        let rp = periapsis_radius.expect("periapsis burn inside Venus SOI");
        assert!(
            (rp / plan.flyby.periapsis_radius_km - 1.0).abs() < 0.01,
            "{rp}"
        );
        // Corrections stay small next to the plan.
        for burn in &guidance.burns()[..2] {
            assert!(burn.delta_v_km_s.length() < 0.05, "{burn:?}");
        }
        assert!(
            guidance.burns()[3].delta_v_km_s.length() < 0.2,
            "{:?}",
            guidance.burns()[3]
        );
    }
}
