//! Spacecraft missions: a body-to-body departure driven by whatever
//! [`MissionRequest`] an embedder asks for —
//! [`GlobeConfig::missions`](crate::GlobeConfig::missions) at startup, or
//! [`GlobeCommand::AddMission`](crate::api::GlobeCommand::AddMission) at
//! runtime, the same pattern as [`crate::overlays`]/[`crate::ephemeris`].
//!
//! A request names a search: [`origin`](MissionRequest::origin) and
//! [`destination`](MissionRequest::destination) (frame names
//! [`terramenta_solare::solar_system`] adds — `"Earth"`/`"Mars"` by default,
//! not hardcoded here), how far ahead to search for a departure and how long
//! after it to allow for arrival, and grid resolution.
//!
//! Each request moves through a state machine, advanced only by
//! [`MissionPlugin`]'s own systems: [`MissionState::Searching`] →
//! [`MissionState::Waiting`] (window found; waits for the simulated clock to
//! reach the departure date — spawning earlier would evaluate the departure
//! hyperbola before its own epoch, landing on the wrong branch) →
//! [`MissionState::Launched`] or [`MissionState::Failed`] (no window solved,
//! or an unknown body).
//!
//! Departure uses [`terramenta_solare::escape_injection_state`], not
//! [`terramenta_solare::injection_state`]: the plain version only reaches
//! its target velocity at infinite range, and the residual left at the
//! origin's finite sphere of influence is enough, given Lambert-transfer
//! sensitivity to departure velocity, to miss the destination by many SOI
//! radii.
//!
//! A mission's own condition is also read out as [`MissionInfo`], flattened
//! for [`crate::api::GlobeState`] through [`MissionReport`] so an interface
//! can show a launch without reaching into this module's state machine.
//! [`crate::heliocentric`] draws the spacecraft; this module still spawns
//! only the physics half.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use serde::Serialize;
use terramenta_solare::bodies::{GM_SUN_KM3_S2, sphere_of_influence_km};
use terramenta_solare::lambert::TransferDirection;
use terramenta_solare::mission::{TransferPlan, find_best_transfer_window};
use terramenta_solare::spacecraft::Primary;
use terramenta_solare::{Epoch, FrameId, escape_injection_state};

use crate::solar::{SolarSystem, TrackedSpacecraft, spawn_spacecraft};
use crate::time::SimClock;

/// A body-to-body transfer, searched for from wherever the simulated clock
/// is when this request is added.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionRequest {
    /// Identifies this mission for [`MissionSettings::remove`] — re-adding
    /// under the same id replaces it, as with [`crate::overlays::OverlayRequest`]
    /// and [`crate::ephemeris::EphemerisRequest`].
    pub id: String,
    /// A frame name [`terramenta_solare::solar_system`] adds.
    pub origin: String,
    pub destination: String,
    /// How far past the clock's current reading to search for a departure.
    pub departure_search_days: f64,
    /// How far past the departure search's start (not the departure date
    /// found) to open the arrival search — matches `terramenta-webapp`'s
    /// porkchop-plot panel convention.
    pub arrival_search_start_days: f64,
    pub arrival_search_end_days: f64,
    /// Grid resolution per axis. Coarser than the webapp's 33×33 porkchop
    /// grid, since this runs once per request just to find *a* good window.
    pub search_steps: usize,
    /// Altitude of the departure hyperbola's periapsis above `origin`'s
    /// surface — a token parking-orbit altitude; no launch/parking orbit is
    /// modeled.
    pub parking_altitude_km: f64,
}

impl Default for MissionRequest {
    fn default() -> Self {
        Self {
            id: String::new(),
            origin: "Earth".to_string(),
            destination: "Mars".to_string(),
            departure_search_days: 120.0,
            arrival_search_start_days: 150.0,
            arrival_search_end_days: 420.0,
            search_steps: 17,
            parking_altitude_km: 300.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum MissionState {
    Searching,
    Waiting {
        plan: TransferPlan,
        origin_frame: FrameId,
        destination_frame: FrameId,
    },
    Launched {
        entity: Entity,
        plan: TransferPlan,
        destination_frame: FrameId,
        last_primary: FrameId,
        /// When the spacecraft's primary first became the Sun — set by
        /// [`log_mission_progress`] the tick it detects the escape, since it
        /// is a physical event this module observes rather than one Lambert
        /// scored in advance.
        escape_unix_seconds: Option<f64>,
        /// When the spacecraft's primary first became `destination_frame` —
        /// set the same way, and generally a little after
        /// [`TransferPlan::arrival`]: that date is the idealised patched-conic
        /// arrival, while this is the real SOI capture landing shortly after it.
        capture_unix_seconds: Option<f64>,
    },
    Failed {
        reason: &'static str,
    },
}

struct Mission {
    request: MissionRequest,
    state: MissionState,
    /// How many times [`launch_missions`] has spawned this mission's
    /// spacecraft — 0 until the first launch. [`terramenta_solare::frame::FrameTree`]
    /// never frees a name once added, and never will (see
    /// [`crate::solar::spawn_spacecraft`]), so a mission un-launched and
    /// relaunched — the clock running back past its departure and then
    /// forward again — needs a fresh frame name each time rather than
    /// reusing one the tree already has.
    launches: u32,
}

/// Every mission this globe has been asked for, in whatever state its
/// search or launch has reached.
#[derive(Resource, Default)]
pub struct MissionSettings {
    missions: Vec<Mission>,
    /// Entities [`MissionSettings::remove`] has taken down but not yet
    /// despawned — drained by [`despawn_retired_missions`], since a plain
    /// resource method has no [`Commands`] to despawn with (same hand-off as
    /// [`crate::overlays::OverlaySettings`]'s own `retired` list).
    retired: Vec<Entity>,
}

impl MissionSettings {
    /// Queues a mission, replacing any already under the same id.
    pub fn add(&mut self, request: MissionRequest) {
        self.remove(&request.id);
        self.missions.push(Mission {
            request,
            state: MissionState::Searching,
            launches: 0,
        });
    }

    /// Cancels a mission. Returns whether there was one. If it had already
    /// launched, its spacecraft is retired for [`despawn_retired_missions`]
    /// to clean up rather than despawned here.
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(index) = self
            .missions
            .iter()
            .position(|mission| mission.request.id == id)
        else {
            return false;
        };
        let mission = self.missions.remove(index);
        if let MissionState::Launched { entity, .. } = mission.state {
            self.retired.push(entity);
        }
        true
    }
}

pub struct MissionPlugin {
    /// Missions to search for at startup — empty by default, same terms as
    /// [`crate::GlobeConfig::overlays`]. A web embedder adds its own via
    /// [`crate::api::GlobeCommand::AddMission`] once the module has loaded.
    pub initial: Vec<MissionRequest>,
}

impl Plugin for MissionPlugin {
    fn build(&self, app: &mut App) {
        let mut settings = MissionSettings::default();
        for request in &self.initial {
            settings.add(request.clone());
        }
        app.insert_resource(settings).add_systems(
            Update,
            (
                search_missions,
                launch_missions,
                log_mission_progress,
                despawn_retired_missions,
            )
                .chain(),
        );
    }
}

/// Scores a Lambert transfer for every mission still
/// [`MissionState::Searching`], advancing it to [`MissionState::Waiting`] on
/// a solvable window or [`MissionState::Failed`] otherwise.
fn search_missions(
    mut settings: ResMut<MissionSettings>,
    solar_system: Res<SolarSystem>,
    clock: Res<SimClock>,
) {
    for mission in &mut settings.missions {
        if !matches!(mission.state, MissionState::Searching) {
            continue;
        }

        let bodies = solar_system
            .find("Sun")
            .zip(solar_system.find(&mission.request.origin))
            .zip(solar_system.find(&mission.request.destination));
        let Some(((sun_frame, origin_frame), destination_frame)) = bodies else {
            warn!(
                "mission {}: unknown body — origin {:?}, destination {:?}",
                mission.request.id, mission.request.origin, mission.request.destination
            );
            mission.state = MissionState::Failed {
                reason: "unknown body",
            };
            continue;
        };

        let search_start = Epoch::from_unix_seconds(clock.unix_seconds);
        let departure_end =
            search_start.advanced_by_seconds(mission.request.departure_search_days * 86_400.0);
        let arrival_start = search_start
            .advanced_by_seconds(mission.request.arrival_search_start_days * 86_400.0);
        let arrival_end =
            search_start.advanced_by_seconds(mission.request.arrival_search_end_days * 86_400.0);

        let plan = find_best_transfer_window(
            solar_system.tree(),
            sun_frame,
            GM_SUN_KM3_S2,
            origin_frame,
            destination_frame,
            search_start,
            departure_end,
            mission.request.search_steps,
            arrival_start,
            arrival_end,
            mission.request.search_steps,
            TransferDirection::Prograde,
        );

        mission.state = match plan {
            Some(plan) => {
                info!(
                    "mission {}: launch window found — departing {} {} for {}, arriving {} \
                     ({:.0} day transfer, {:.2} km/s departure burn)",
                    mission.request.id,
                    mission.request.origin,
                    format_epoch(plan.departure),
                    mission.request.destination,
                    format_epoch(plan.arrival),
                    plan.time_of_flight_seconds / 86_400.0,
                    plan.departure_delta_v_magnitude_km_s(),
                );
                MissionState::Waiting {
                    plan,
                    origin_frame,
                    destination_frame,
                }
            }
            None => {
                warn!(
                    "mission {}: no {}-{} transfer window found in the search range",
                    mission.request.id, mission.request.origin, mission.request.destination
                );
                MissionState::Failed {
                    reason: "no transfer window found in the search range",
                }
            }
        };
    }
}

/// Spawns the spacecraft for every [`MissionState::Waiting`] mission whose
/// planned departure date the simulated clock has reached — not before:
/// [`terramenta_solare::spacecraft::Spacecraft::spawn`] anchors its orbital
/// elements to the given epoch, and evaluating them before that epoch lands
/// on the hyperbola's incoming branch instead of its outgoing one, looking
/// like an instant, spurious escape.
///
/// Also the reverse: a [`MissionState::Launched`] mission the clock has run
/// back to *before* its own departure — the clock can now run backward, see
/// [`crate::time::SimClock::set_time_scale`] — is un-launched rather than
/// left for [`crate::solar::update_spacecraft`] to evaluate on that same
/// wrong branch. [`search_missions`] never re-solves it: the plan already
/// found is still the one this departure date belongs to, so reverting to
/// [`MissionState::Waiting`] with it is enough for [`launch_missions`] to
/// spawn the identical spacecraft again once the clock crosses forward.
fn launch_missions(
    mut commands: Commands,
    mut settings: ResMut<MissionSettings>,
    mut solar_system: ResMut<SolarSystem>,
    clock: Res<SimClock>,
) {
    let Some(sun_frame) = solar_system.find("Sun") else {
        return;
    };

    for mission in &mut settings.missions {
        if let MissionState::Launched { entity, plan, .. } = mission.state {
            if clock.unix_seconds < plan.departure.to_unix_seconds() {
                let bodies = solar_system
                    .find(&mission.request.origin)
                    .zip(solar_system.find(&mission.request.destination));
                let Some((origin_frame, destination_frame)) = bodies else {
                    continue;
                };
                commands.entity(entity).despawn();
                mission.state = MissionState::Waiting {
                    plan,
                    origin_frame,
                    destination_frame,
                };
            }
            continue;
        }

        let (plan, origin_frame, destination_frame) = match mission.state {
            MissionState::Waiting {
                plan,
                origin_frame,
                destination_frame,
            } => (plan, origin_frame, destination_frame),
            _ => continue,
        };
        if clock.unix_seconds < plan.departure.to_unix_seconds() {
            continue;
        }

        let Some(origin_primary) = primary_for(&mission.request.origin, origin_frame, sun_frame)
        else {
            mission.state = MissionState::Failed {
                reason: "unknown body",
            };
            continue;
        };
        let Some(destination_primary) =
            primary_for(&mission.request.destination, destination_frame, sun_frame)
        else {
            mission.state = MissionState::Failed {
                reason: "unknown body",
            };
            continue;
        };

        // Corrects the departure delta-v (a v∞ relative to the origin) for
        // the origin's finite SOI radius — see `escape_injection_state`'s
        // own docs for why this matters.
        let origin_sun_distance_km = solar_system
            .tree()
            .state_of_relative_to(origin_frame, sun_frame, plan.departure)
            .position_km
            .length();
        let origin_soi_km = sphere_of_influence_km(
            origin_primary.gm_km3_s2,
            GM_SUN_KM3_S2,
            origin_sun_distance_km,
        );
        let departure_state = escape_injection_state(
            plan.departure_delta_v_km_s,
            origin_soi_km,
            mission.request.parking_altitude_km,
            origin_primary.gm_km3_s2,
        );

        let primary = Primary {
            orbits: Some(Box::new(
                Primary::sun(sun_frame).with_capture_candidates(vec![destination_primary]),
            )),
            ..origin_primary
        };

        info!(
            "mission {}: departing {} now, as planned, for arrival at {} {}",
            mission.request.id,
            mission.request.origin,
            mission.request.destination,
            format_epoch(plan.arrival),
        );

        let bundle = spawn_spacecraft(
            &mut solar_system,
            frame_name(&mission.request.id, mission.launches),
            primary,
            departure_state,
            plan.departure,
        );
        mission.launches += 1;
        let entity = commands.spawn(bundle).id();
        mission.state = MissionState::Launched {
            entity,
            plan,
            destination_frame,
            last_primary: origin_frame,
            escape_unix_seconds: None,
            capture_unix_seconds: None,
        };
    }
}

/// A fresh, never-reused frame name for a mission's `launches`-th spacecraft.
///
/// [`terramenta_solare::frame::FrameTree::add`] rejects a name already in the
/// tree, and the tree never frees one — see the field doc on
/// [`Mission::launches`] for why a relaunch cannot reuse the name its
/// previous launch took. Leaked rather than stored: a `'static str` is what
/// [`crate::solar::spawn_spacecraft`] takes, or a compile-time constant would
/// do here, but that's not available for a name built at runtime from the
/// mission's own id — and missions are added by an embedder's explicit
/// action, at a scale of a handful over an app's lifetime, not thousands, so
/// leaking a few dozen bytes per launch is the right side of that trade.
fn frame_name(mission_id: &str, launches: u32) -> &'static str {
    Box::leak(format!("mission:{mission_id}:{launches}").into_boxed_str())
}

/// Logs each launched mission's primary body the moment it changes — the
/// same transition [`MissionReport`]'s `"enroute"`/`"arrived"` status and
/// [`crate::heliocentric`]'s drawing also read off
/// [`crate::solar::update_spacecraft`] driving
/// [`TrackedSpacecraft::update`](terramenta_solare::spacecraft::Spacecraft::update):
/// origin → Sun (escape), then Sun → destination (capture).
fn log_mission_progress(
    mut settings: ResMut<MissionSettings>,
    solar_system: Res<SolarSystem>,
    clock: Res<SimClock>,
    spacecraft: Query<&TrackedSpacecraft>,
) {
    let sun_frame = solar_system.find("Sun");

    for mission in &mut settings.missions {
        let MissionState::Launched {
            entity,
            destination_frame,
            last_primary,
            escape_unix_seconds,
            capture_unix_seconds,
            ..
        } = &mut mission.state
        else {
            continue;
        };
        let Ok(craft) = spacecraft.get(*entity) else {
            continue;
        };

        let current = craft.0.primary_frame();
        if current == *last_primary {
            continue;
        }
        *last_primary = current;

        // The two hand-offs a patched-conic transfer ever makes: away from
        // the origin's SOI (primary becomes the Sun) and into the
        // destination's (primary becomes `destination_frame`) — stamped with
        // the simulated clock the moment each is observed, for
        // [`MissionInfo::phases`] to report alongside the Lambert-planned
        // departure/arrival dates.
        if Some(current) == sun_frame {
            *escape_unix_seconds = Some(clock.unix_seconds);
        } else if current == *destination_frame {
            *capture_unix_seconds = Some(clock.unix_seconds);
        }

        info!(
            "mission {}: now orbiting {}, as of {}",
            mission.request.id,
            solar_system.tree().name(current),
            format_epoch(Epoch::from_unix_seconds(clock.unix_seconds)),
        );
    }
}

/// The [`Primary`] (gravitational parameter included) a named body resolves
/// to. [`terramenta_solare::bodies`] only states GM for the bodies
/// [`terramenta_solare::solar_system`] adds, so an unknown name fails
/// cleanly here rather than propagating a made-up mass. `frame` is already
/// resolved by [`search_missions`]; this only supplies the constant.
fn primary_for(name: &str, frame: FrameId, sun_frame: FrameId) -> Option<Primary> {
    match name {
        "Sun" => Some(Primary::sun(frame)),
        "Earth" => Some(Primary::earth(frame, sun_frame)),
        "Mars" => Some(Primary::mars(frame, sun_frame)),
        _ => None,
    }
}

fn despawn_retired_missions(mut commands: Commands, mut settings: ResMut<MissionSettings>) {
    for entity in settings.retired.drain(..) {
        commands.entity(entity).despawn();
    }
}

fn format_epoch(epoch: Epoch) -> String {
    let (year, month, day) = civil_from_days(epoch.days().floor() as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since J2000.0 to a proleptic Gregorian date, via Unix days — Howard
/// Hinnant's `civil_from_days` algorithm, also used by
/// `terramenta-solare`'s `earth_to_mars` example.
fn civil_from_days(days_since_j2000: i64) -> (i64, u32, u32) {
    const J2000_UNIX_DAYS: i64 = 10_957; // 2000-01-01 is Unix day 10957.
    let days = days_since_j2000 + J2000_UNIX_DAYS;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

// ---------------------------------------------------------------------------
// State reported to an interface
// ---------------------------------------------------------------------------

/// One mission, as an interface sees it — flattened out of whichever
/// [`MissionState`] the request has reached, so a caller can read it without
/// knowing this module's own state machine.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MissionInfo {
    pub id: String,
    pub origin: String,
    pub destination: String,
    /// `"searching"`, `"waiting"`, `"enroute"`, `"arrived"` or `"failed"`.
    pub status: &'static str,
    /// Set from `"waiting"` onward — a plan has been found by then.
    pub departure_unix_seconds: Option<f64>,
    pub arrival_unix_seconds: Option<f64>,
    pub departure_delta_v_km_s: Option<f64>,
    pub arrival_delta_v_km_s: Option<f64>,
    /// Set only for `"enroute"` or `"arrived"` — the body the spacecraft
    /// currently orbits, `terramenta_solare::solar_system`'s own name for it.
    pub orbiting: Option<String>,
    /// Set only for `"failed"`.
    pub reason: Option<&'static str>,
    /// The mission's milestones, always in this order: departure, escape
    /// (from `origin`'s sphere of influence), arrival (the Lambert-planned
    /// date) and capture (by `destination`). See [`MissionPhase`].
    pub phases: Vec<MissionPhase>,
}

/// One milestone in a mission's timeline, for an interface to build a
/// scrubber or a set of "jump to" controls from.
///
/// Always reported, even before it is known — `unix_seconds` is `None` until
/// then, rather than the entry being left out, so an interface can render a
/// fixed four-stop timeline and grey out what is not there yet instead of
/// reflowing as the mission progresses.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MissionPhase {
    /// `"departure"`, `"escape"`, `"arrival"` or `"capture"`.
    pub id: &'static str,
    pub label: String,
    pub unix_seconds: Option<f64>,
    /// Whether the simulated clock has reached this milestone yet. `false`
    /// while `unix_seconds` is still `None`, since an unknown moment cannot
    /// have been reached.
    pub reached: bool,
}

impl MissionPhase {
    fn new(id: &'static str, label: String, unix_seconds: Option<f64>, sun_unix_seconds: f64) -> Self {
        Self {
            id,
            label,
            unix_seconds,
            reached: unix_seconds.is_some_and(|seconds| sun_unix_seconds >= seconds),
        }
    }
}

impl MissionInfo {
    fn new(request: &MissionRequest, status: &'static str) -> Self {
        Self {
            id: request.id.clone(),
            origin: request.origin.clone(),
            destination: request.destination.clone(),
            status,
            departure_unix_seconds: None,
            arrival_unix_seconds: None,
            departure_delta_v_km_s: None,
            arrival_delta_v_km_s: None,
            orbiting: None,
            reason: None,
            phases: Vec::new(),
        }
    }

    fn with_plan(mut self, plan: &TransferPlan) -> Self {
        self.departure_unix_seconds = Some(plan.departure.to_unix_seconds());
        self.arrival_unix_seconds = Some(plan.arrival.to_unix_seconds());
        self.departure_delta_v_km_s = Some(plan.departure_delta_v_magnitude_km_s());
        self.arrival_delta_v_km_s = Some(plan.arrival_delta_v_magnitude_km_s());
        self
    }

    /// Fills in [`MissionInfo::phases`] — always all four milestones, drawing
    /// the departure/arrival dates from whatever [`MissionInfo::with_plan`]
    /// already set, and escape/capture from [`log_mission_progress`]'s own
    /// observations. Called for every mission regardless of state, so a
    /// mission still searching (or one that failed) reports the same four
    /// milestones with everything past what is known left as `None`.
    fn with_phases(
        mut self,
        escape_unix_seconds: Option<f64>,
        capture_unix_seconds: Option<f64>,
        sun_unix_seconds: f64,
    ) -> Self {
        let departure_unix_seconds = self.departure_unix_seconds;
        let arrival_unix_seconds = self.arrival_unix_seconds;
        self.phases = vec![
            MissionPhase::new(
                "departure",
                format!("Depart {}", self.origin),
                departure_unix_seconds,
                sun_unix_seconds,
            ),
            MissionPhase::new(
                "escape",
                format!("Escape {}", self.origin),
                escape_unix_seconds,
                sun_unix_seconds,
            ),
            MissionPhase::new(
                "arrival",
                format!("Arrive at {}", self.destination),
                arrival_unix_seconds,
                sun_unix_seconds,
            ),
            MissionPhase::new(
                "capture",
                format!("Captured by {}", self.destination),
                capture_unix_seconds,
                sun_unix_seconds,
            ),
        ];
        self
    }
}

impl Mission {
    fn describe(
        &self,
        solar_system: &SolarSystem,
        spacecraft: &Query<&TrackedSpacecraft>,
        sun_unix_seconds: f64,
    ) -> MissionInfo {
        match &self.state {
            MissionState::Searching => MissionInfo::new(&self.request, "searching")
                .with_phases(None, None, sun_unix_seconds),
            MissionState::Waiting { plan, .. } => MissionInfo::new(&self.request, "waiting")
                .with_plan(plan)
                .with_phases(None, None, sun_unix_seconds),
            MissionState::Launched {
                entity,
                plan,
                destination_frame,
                escape_unix_seconds,
                capture_unix_seconds,
                ..
            } => {
                let orbiting = spacecraft
                    .get(*entity)
                    .ok()
                    .map(|craft| craft.0.primary_frame());
                let arrived = orbiting == Some(*destination_frame);
                let mut info =
                    MissionInfo::new(&self.request, if arrived { "arrived" } else { "enroute" })
                        .with_plan(plan)
                        .with_phases(*escape_unix_seconds, *capture_unix_seconds, sun_unix_seconds);
                info.orbiting = orbiting.map(|frame| solar_system.tree().name(frame).to_string());
                info
            }
            MissionState::Failed { reason } => {
                let mut info = MissionInfo::new(&self.request, "failed")
                    .with_phases(None, None, sun_unix_seconds);
                info.reason = Some(reason);
                info
            }
        }
    }
}

impl MissionSettings {
    fn describe(
        &self,
        solar_system: &SolarSystem,
        spacecraft: &Query<&TrackedSpacecraft>,
        sun_unix_seconds: f64,
    ) -> Vec<MissionInfo> {
        self.missions
            .iter()
            .map(|mission| mission.describe(solar_system, spacecraft, sun_unix_seconds))
            .collect()
    }
}

/// What the state snapshot reads missions through: [`MissionSettings`]
/// itself, the [`TrackedSpacecraft`] query for which body a launched mission
/// currently orbits, and [`SolarSystem`] to name it. One parameter rather
/// than three because a system may only take sixteen, same as
/// [`crate::placemark::PlacemarkPicks`].
#[derive(SystemParam)]
pub struct MissionReport<'w, 's> {
    settings: Res<'w, MissionSettings>,
    solar_system: Res<'w, SolarSystem>,
    spacecraft: Query<'w, 's, &'static TrackedSpacecraft>,
}

impl MissionReport<'_, '_> {
    pub fn describe(&self, sun_unix_seconds: f64) -> Vec<MissionInfo> {
        self.settings
            .describe(&self.solar_system, &self.spacecraft, sun_unix_seconds)
    }
}

#[cfg(test)]
mod tests {
    use bevy::app::TaskPoolPlugin;

    use super::*;
    use crate::frame::ReferenceFrame;
    use crate::solar::SolarSystemPlugin;
    use crate::view::ViewState;

    /// End-to-end through [`MissionSettings::add`] (the same entry point
    /// [`crate::api::GlobeCommand::AddMission`] reaches), ticked inside a
    /// running (renderer-less) app: Earth escape then Mars capture, the same
    /// two hand-offs
    /// `terramenta_solare::tests::a_spacecraft_escaping_earth_ends_up_heliocentric`
    /// and `spacecraft::tests::a_spacecraft_flies_the_full_earth_to_mars_patched_conics_trip`
    /// check against the model directly.
    ///
    /// Clock starts at the known-good Earth-Mars window
    /// `mission::tests::finds_the_known_good_window_inside_a_wider_search_grid`
    /// (in `terramenta-solare`) scans for — departure ~53 days into the
    /// default request's 120-day search range — rather than wall-clock
    /// "now", so this is deterministic.
    #[test]
    fn a_default_mission_escapes_earth_and_is_captured_by_mars() {
        let start_unix_seconds = Epoch::J2000
            .advanced_by_seconds(1_200.0 * 86_400.0)
            .to_unix_seconds();

        let mut app = App::new();
        app.add_plugins(TaskPoolPlugin::default())
            .init_resource::<ReferenceFrame>()
            .init_resource::<ViewState>()
            .insert_resource(SimClock {
                unix_seconds: start_unix_seconds,
                ..SimClock::default()
            })
            .add_plugins(SolarSystemPlugin)
            .add_plugins(MissionPlugin {
                initial: Vec::new(),
            });

        app.world_mut()
            .resource_mut::<MissionSettings>()
            .add(MissionRequest {
                id: "test".to_string(),
                ..MissionRequest::default()
            });

        let mut launched = false;
        let mut escaped = false;
        let mut captured = false;
        // Covers the departure window (120d) plus the arrival window past it
        // (420d), plus margin for the SOI capture landing a bit after the
        // scored arrival date — same margin `earth_to_mars.rs` keeps.
        //
        // Reads `MissionSettings`'s own state rather than separately querying
        // `TrackedSpacecraft::primary_frame` — that would race
        // `log_mission_progress` against `update_spacecraft`'s reparenting on
        // whichever tick the hand-off lands on, since the two have no
        // ordering constraint relative to each other. Reading the state
        // `log_mission_progress` itself wrote is what this test means to
        // check anyway.
        for _ in 0..(560 * 24) {
            app.world_mut().resource_mut::<SimClock>().unix_seconds += 3_600.0;
            app.update();

            let settings = app.world().resource::<MissionSettings>();
            let mission = settings
                .missions
                .iter()
                .find(|mission| mission.request.id == "test")
                .expect("the mission is still tracked");
            let MissionState::Launched {
                escape_unix_seconds,
                capture_unix_seconds,
                ..
            } = &mission.state
            else {
                continue;
            };
            launched = true;
            escaped = escape_unix_seconds.is_some();
            if capture_unix_seconds.is_some() {
                captured = true;
                break;
            }
        }

        assert!(launched, "the mission never launched");
        assert!(escaped, "the mission's spacecraft never escaped Earth");
        assert!(captured, "the mission's spacecraft never arrived at Mars");

        // The two hand-offs `log_mission_progress` stamped, in order — the
        // same two moments `MissionInfo::phases` reports as `"escape"` and
        // `"capture"`.
        let settings = app.world().resource::<MissionSettings>();
        let mission = settings
            .missions
            .iter()
            .find(|mission| mission.request.id == "test")
            .expect("the mission is still tracked");
        let MissionState::Launched {
            plan,
            escape_unix_seconds,
            capture_unix_seconds,
            ..
        } = &mission.state
        else {
            panic!("expected the mission to still be Launched, got {:?}", mission.state);
        };
        let escape_unix_seconds = escape_unix_seconds.expect("escape was observed above");
        let capture_unix_seconds = capture_unix_seconds.expect("capture was observed above");
        assert!(plan.departure.to_unix_seconds() < escape_unix_seconds);
        assert!(escape_unix_seconds < capture_unix_seconds);
    }

    /// Running the clock back past a launched mission's own departure date
    /// un-launches it — caught by the mission-planning demo page's reverse
    /// clock control, which can send the simulated clock anywhere, including
    /// back across a departure `search_missions` already solved. Without
    /// this, `update_spacecraft` would keep evaluating the escape hyperbola
    /// on the epoch it spawned from, the same wrong-branch concern
    /// `launch_missions`'s own docs already raise about launching too early.
    #[test]
    fn a_mission_reversed_past_its_departure_is_unlaunched_and_relaunches_forward() {
        let start_unix_seconds = Epoch::J2000
            .advanced_by_seconds(1_200.0 * 86_400.0)
            .to_unix_seconds();

        let mut app = App::new();
        app.add_plugins(TaskPoolPlugin::default())
            .init_resource::<ReferenceFrame>()
            .init_resource::<ViewState>()
            .insert_resource(SimClock {
                unix_seconds: start_unix_seconds,
                ..SimClock::default()
            })
            .add_plugins(SolarSystemPlugin)
            .add_plugins(MissionPlugin {
                initial: Vec::new(),
            });

        app.world_mut()
            .resource_mut::<MissionSettings>()
            .add(MissionRequest {
                id: "test".to_string(),
                ..MissionRequest::default()
            });

        let is_launched = |app: &App| {
            matches!(
                app.world()
                    .resource::<MissionSettings>()
                    .missions
                    .iter()
                    .find(|mission| mission.request.id == "test")
                    .map(|mission| &mission.state),
                Some(MissionState::Launched { .. })
            )
        };
        let spacecraft_count =
            |app: &mut App| app.world_mut().query::<&TrackedSpacecraft>().iter(app.world()).count();

        // Run forward past the known-good departure (~53 days in) until launched.
        for _ in 0..(60 * 24) {
            app.world_mut().resource_mut::<SimClock>().unix_seconds += 3_600.0;
            app.update();
            if is_launched(&app) {
                break;
            }
        }
        assert!(is_launched(&app), "the mission never launched");
        assert_eq!(spacecraft_count(&mut app), 1);

        // A little further forward, still nowhere near escaping Earth.
        for _ in 0..24 {
            app.world_mut().resource_mut::<SimClock>().unix_seconds += 3_600.0;
            app.update();
        }

        // Now run the clock all the way back past the departure date.
        for _ in 0..(90 * 24) {
            app.world_mut().resource_mut::<SimClock>().unix_seconds -= 3_600.0;
            app.update();
        }
        assert!(
            !is_launched(&app),
            "a mission reversed past its own departure should have un-launched"
        );
        assert_eq!(
            spacecraft_count(&mut app),
            0,
            "the un-launched mission's spacecraft should have despawned"
        );

        // And forward again should relaunch it, identically — further this
        // time, since the reversal above ran the clock back well before the
        // departure date, not just up to it.
        for _ in 0..(150 * 24) {
            app.world_mut().resource_mut::<SimClock>().unix_seconds += 3_600.0;
            app.update();
            if is_launched(&app) {
                break;
            }
        }
        assert!(is_launched(&app), "the mission never relaunched");
        assert_eq!(spacecraft_count(&mut app), 1);
    }

    /// An unknown body name fails the search cleanly rather than panicking.
    #[test]
    fn a_mission_naming_an_unknown_body_fails_without_panicking() {
        let mut app = App::new();
        app.add_plugins(TaskPoolPlugin::default())
            .init_resource::<ReferenceFrame>()
            .init_resource::<ViewState>()
            .insert_resource(SimClock::default())
            .add_plugins(SolarSystemPlugin)
            .add_plugins(MissionPlugin {
                initial: Vec::new(),
            });

        app.world_mut()
            .resource_mut::<MissionSettings>()
            .add(MissionRequest {
                id: "test".to_string(),
                destination: "Jupiter".to_string(),
                ..MissionRequest::default()
            });

        app.update();

        let settings = app.world().resource::<MissionSettings>();
        assert!(matches!(
            settings.missions.first().map(|mission| &mission.state),
            Some(MissionState::Failed { .. })
        ));
    }
}
