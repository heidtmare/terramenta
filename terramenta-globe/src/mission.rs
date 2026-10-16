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
//! [`MissionRequest::via`] makes it a gravity assist: a three-axis search
//! ([`terramenta_solare::flyby::find_best_flyby`]) and
//! [`terramenta_solare::flyby::FlybyGuidance`] on the spacecraft
//! ([`crate::solar::Guidance`]) for the correction and periapsis burns.
//!
//! A mission's own condition is also read out as [`MissionInfo`], flattened
//! for [`crate::api::GlobeState`] through [`MissionReport`] so an interface
//! can show a launch without reaching into this module's state machine.
//! [`crate::heliocentric`] draws the spacecraft; this module still spawns
//! only the physics half.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use serde::Serialize;
use terramenta_solare::bodies::{
    EARTH_MEAN_RADIUS_KM, GM_SUN_KM3_S2, GM_VENUS_KM3_S2, MARS_MEAN_RADIUS_KM,
    VENUS_MEAN_RADIUS_KM, sphere_of_influence_km,
};
use terramenta_solare::flyby::{
    FlybyBody, FlybyGuidance, FlybyPlan, FlybySearch, find_best_flyby, flyby_primary_chain,
};
use terramenta_solare::lambert::TransferDirection;
use terramenta_solare::mission::{TransferPlan, find_best_transfer_window};
use terramenta_solare::spacecraft::Primary;
use terramenta_solare::time::hifitime::TimeScale;
use terramenta_solare::{Epoch, FrameId, escape_injection_state};

use crate::solar::{Guidance, SolarSystem, TrackedSpacecraft, spawn_spacecraft};
use crate::time::SimClock;
use crate::trail::MissionTrail;

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
    /// Gravity-assist body. Replaces the arrival search with
    /// [`MissionRequest::first_leg_days`] × [`MissionRequest::second_leg_days`].
    pub via: Option<String>,
    /// Origin → `via` duration range, days.
    pub first_leg_days: (f64, f64),
    /// `via` → destination duration range, days.
    pub second_leg_days: (f64, f64),
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
            via: None,
            first_leg_days: (80.0, 250.0),
            second_leg_days: (120.0, 400.0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum MissionState {
    Searching,
    Waiting {
        plan: TransferPlan,
        flyby: Option<FlybyPlan>,
        origin_frame: FrameId,
        destination_frame: FrameId,
    },
    Launched {
        entity: Entity,
        /// The spacecraft's own frame in [`SolarSystem`]'s tree — fixed for
        /// the life of this launch, whichever body it currently hangs off.
        /// What [`MissionSettings::camera_frame`] hands the heliocentric
        /// camera to follow.
        frame: FrameId,
        plan: TransferPlan,
        flyby: Option<FlybyPlan>,
        destination_frame: FrameId,
        via_frame: Option<FrameId>,
        last_primary: FrameId,
        /// When the spacecraft's primary first became the Sun — set by
        /// [`log_mission_progress`] the tick it detects the escape, since it
        /// is a physical event this module observes rather than one Lambert
        /// scored in advance.
        escape_unix_seconds: Option<f64>,
        /// Flyby SOI entry and exit, observed.
        encounter_unix_seconds: Option<f64>,
        exit_unix_seconds: Option<f64>,
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
    /// How many spacecraft [`launch_missions`] has spawned, across every
    /// mission. [`terramenta_solare::frame::FrameTree`] never frees a name
    /// once added (see [`crate::solar::spawn_spacecraft`]), so each launch —
    /// a relaunch after the clock ran back past departure, or a mission
    /// removed and re-added under the same id — needs a name never used.
    launches: u32,
}

impl MissionSettings {
    /// Queues a mission, replacing any already under the same id.
    pub fn add(&mut self, request: MissionRequest) {
        self.remove(&request.id);
        self.missions.push(Mission {
            request,
            state: MissionState::Searching,
        });
    }

    /// Cancels every mission.
    pub fn reset(&mut self) {
        let ids: Vec<String> = self
            .missions
            .iter()
            .map(|mission| mission.request.id.clone())
            .collect();
        for id in ids {
            self.remove(&id);
        }
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

    /// Mission `id`'s flyby plane normal (`v̂∞in × v̂∞out`, ICRF), once planned.
    pub(crate) fn flyby_normal(&self, id: &str) -> Option<bevy::math::DVec3> {
        let mission = self
            .missions
            .iter()
            .find(|mission| mission.request.id == id)?;
        let flyby = match mission.state {
            MissionState::Waiting { flyby, .. } | MissionState::Launched { flyby, .. } => flyby?,
            _ => return None,
        };
        Some(
            flyby
                .flyby
                .v_infinity_in_km_s
                .cross(flyby.flyby.v_infinity_out_km_s)
                .normalize(),
        )
    }

    /// The id of the first mission still tracked, in the order they were
    /// added — what the heliocentric view's follow key locks onto.
    pub(crate) fn first_id(&self) -> Option<&str> {
        self.missions
            .first()
            .map(|mission| mission.request.id.as_str())
    }

    /// Mission `id`'s spacecraft frame, if it is launched right now — what
    /// [`crate::camera`]'s globe-view follow locks onto. Unlike
    /// [`MissionSettings::camera_frame`] there is no stand-in before launch:
    /// the globe camera already has Earth in view without being told to.
    pub(crate) fn spacecraft_frame(&self, id: &str) -> Option<FrameId> {
        let mission = self
            .missions
            .iter()
            .find(|mission| mission.request.id == id)?;
        match mission.state {
            MissionState::Launched { frame, .. } => Some(frame),
            _ => None,
        }
    }

    /// The frame a camera following mission `id` should orbit right now: the
    /// spacecraft's own once it has launched, and the origin body's before
    /// then — which is where the spacecraft will appear, so a lock taken out
    /// while the mission is still searching or waiting picks the spacecraft
    /// up without a jump the tick it departs. `None` for a mission that is
    /// not tracked, or that failed without ever resolving its origin.
    ///
    /// Asked for every tick rather than once: a relaunch (the clock run back
    /// past departure and forward again) spawns the spacecraft under a fresh
    /// frame, so a frame id captured at lock time would go stale.
    pub(crate) fn camera_frame(&self, id: &str, solar_system: &SolarSystem) -> Option<FrameId> {
        let mission = self
            .missions
            .iter()
            .find(|mission| mission.request.id == id)?;
        match mission.state {
            MissionState::Launched { frame, .. } => Some(frame),
            MissionState::Waiting { origin_frame, .. } => Some(origin_frame),
            MissionState::Searching | MissionState::Failed { .. } => {
                solar_system.find(&mission.request.origin)
            }
        }
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
        let arrival_start =
            search_start.advanced_by_seconds(mission.request.arrival_search_start_days * 86_400.0);
        let arrival_end =
            search_start.advanced_by_seconds(mission.request.arrival_search_end_days * 86_400.0);

        if let Some(via) = mission.request.via.clone() {
            mission.state = search_flyby(
                &mission.request,
                &via,
                &solar_system,
                sun_frame,
                origin_frame,
                destination_frame,
                search_start,
                departure_end,
            );
            continue;
        }

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
                    flyby: None,
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

#[allow(clippy::too_many_arguments)]
fn search_flyby(
    request: &MissionRequest,
    via: &str,
    solar_system: &SolarSystem,
    sun_frame: FrameId,
    origin_frame: FrameId,
    destination_frame: FrameId,
    departure_start: Epoch,
    departure_end: Epoch,
) -> MissionState {
    let Some(body) = solar_system
        .find(via)
        .and_then(|frame| flyby_body(via, frame))
    else {
        warn!("mission {}: unknown flyby body {via:?}", request.id);
        return MissionState::Failed {
            reason: "unknown body",
        };
    };
    let search = FlybySearch {
        departure_start,
        departure_end,
        first_leg_days: request.first_leg_days,
        second_leg_days: request.second_leg_days,
        steps: request.search_steps,
    };
    let Some(flyby) = find_best_flyby(
        solar_system.tree(),
        sun_frame,
        GM_SUN_KM3_S2,
        origin_frame,
        &body,
        destination_frame,
        &search,
    ) else {
        warn!(
            "mission {}: no {}-{via}-{} flyby found in the search range",
            request.id, request.origin, request.destination
        );
        return MissionState::Failed {
            reason: "no flyby trajectory found in the search range",
        };
    };
    info!(
        "mission {}: flyby found — departing {} {}, {via} {} at {:.0} km, arriving {} {} \
         ({:.2} km/s total)",
        request.id,
        request.origin,
        format_epoch(flyby.first_leg.departure),
        format_epoch(flyby.flyby_epoch()),
        flyby.flyby.periapsis_radius_km - body_radius_km(via).unwrap_or(0.0),
        request.destination,
        format_epoch(flyby.second_leg.arrival),
        flyby.total_delta_v_km_s(),
    );
    MissionState::Waiting {
        plan: flyby.overall(),
        flyby: Some(flyby),
        origin_frame,
        destination_frame,
    }
}

/// Lowest flyby periapsis altitude allowed, km.
const FLYBY_MIN_ALTITUDE_KM: f64 = 300.0;

fn body_radius_km(name: &str) -> Option<f64> {
    match name {
        "Venus" => Some(VENUS_MEAN_RADIUS_KM),
        "Earth" => Some(EARTH_MEAN_RADIUS_KM),
        "Mars" => Some(MARS_MEAN_RADIUS_KM),
        _ => None,
    }
}

fn flyby_body(name: &str, frame: FrameId) -> Option<FlybyBody> {
    let gm_km3_s2 = match name {
        "Venus" => GM_VENUS_KM3_S2,
        "Earth" => terramenta_solare::bodies::GM_EARTH_KM3_S2,
        "Mars" => terramenta_solare::bodies::GM_MARS_KM3_S2,
        _ => return None,
    };
    Some(FlybyBody {
        frame,
        gm_km3_s2,
        min_periapsis_radius_km: body_radius_km(name)? + FLYBY_MIN_ALTITUDE_KM,
    })
}

/// Spawns the spacecraft for every [`MissionState::Waiting`] mission whose
/// planned departure date the simulated clock has reached — not before:
/// [`terramenta_solare::spacecraft::PatchedConicSpacecraft::spawn`] anchors its orbital
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

    let MissionSettings {
        missions, launches, ..
    } = &mut *settings;
    for mission in missions {
        if let MissionState::Launched {
            entity,
            plan,
            flyby,
            ..
        } = mission.state
        {
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
                    flyby,
                    origin_frame,
                    destination_frame,
                };
            }
            continue;
        }

        let (plan, flyby, origin_frame, destination_frame) = match mission.state {
            MissionState::Waiting {
                plan,
                flyby,
                origin_frame,
                destination_frame,
            } => (plan, flyby, origin_frame, destination_frame),
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

        let via = match (&mission.request.via, flyby) {
            (Some(name), Some(_)) => {
                let resolved = solar_system.find(name).and_then(|frame| {
                    Some((
                        flyby_body(name, frame)?,
                        primary_for(name, frame, sun_frame)?,
                    ))
                });
                let Some(resolved) = resolved else {
                    mission.state = MissionState::Failed {
                        reason: "unknown body",
                    };
                    continue;
                };
                Some(resolved)
            }
            _ => None,
        };

        let legs: Vec<(FrameId, TransferPlan)> = match (flyby, &via) {
            (Some(flyby), Some((body, _))) => vec![
                (origin_frame, flyby.first_leg),
                (body.frame, flyby.second_leg),
            ],
            _ => vec![(origin_frame, plan)],
        };
        let trail = MissionTrail::plan(
            &solar_system,
            &legs,
            destination_frame,
            via.as_ref().map(|(body, _)| body.frame),
            sun_frame,
            departure_state,
            origin_primary.gm_km3_s2,
            origin_soi_km,
        );

        let primary = match &via {
            Some((_, via_primary)) => flyby_primary_chain(
                origin_primary,
                sun_frame,
                via_primary.clone(),
                destination_primary,
            ),
            None => Primary {
                orbits: Some(Box::new(
                    Primary::sun(sun_frame).with_capture_candidates(vec![destination_primary]),
                )),
                ..origin_primary
            },
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
            frame_name(&mission.request.id, *launches),
            primary,
            departure_state,
            plan.departure,
        );
        *launches += 1;
        let frame = bundle.0.0;
        let mut spawned = commands.spawn((bundle, trail));
        if let (Some(flyby), Some((body, _))) = (flyby, &via) {
            spawned.insert(Guidance(FlybyGuidance::new(
                flyby,
                sun_frame,
                body,
                destination_frame,
                plan.departure,
            )));
        }
        let entity = spawned.id();
        mission.state = MissionState::Launched {
            entity,
            frame,
            plan,
            flyby,
            destination_frame,
            via_frame: via.as_ref().map(|(body, _)| body.frame),
            last_primary: origin_frame,
            escape_unix_seconds: None,
            encounter_unix_seconds: None,
            exit_unix_seconds: None,
            capture_unix_seconds: None,
        };
    }
}

/// A fresh, never-reused frame name for the `launches`-th spacecraft.
///
/// [`terramenta_solare::frame::FrameTree::add`] rejects a name already in the
/// tree, and the tree never frees one — see [`MissionSettings::launches`]. Leaked rather than stored: a `'static str` is what
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
/// [`TrackedSpacecraft::update`](terramenta_solare::spacecraft::PatchedConicSpacecraft::update):
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
            via_frame,
            last_primary,
            escape_unix_seconds,
            encounter_unix_seconds,
            exit_unix_seconds,
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
        let previous = std::mem::replace(last_primary, current);

        // The two hand-offs a patched-conic transfer ever makes: away from
        // the origin's SOI (primary becomes the Sun) and into the
        // destination's (primary becomes `destination_frame`) — stamped with
        // the simulated clock the moment each is observed, for
        // [`MissionInfo::phases`] to report alongside the Lambert-planned
        // departure/arrival dates.
        if Some(current) == sun_frame && Some(previous) == *via_frame {
            *exit_unix_seconds = Some(clock.unix_seconds);
        } else if Some(current) == sun_frame {
            *escape_unix_seconds = Some(clock.unix_seconds);
        } else if Some(current) == *via_frame {
            *encounter_unix_seconds = Some(clock.unix_seconds);
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
        "Venus" => Some(Primary::venus(frame, sun_frame)),
        _ => None,
    }
}

fn despawn_retired_missions(mut commands: Commands, mut settings: ResMut<MissionSettings>) {
    for entity in settings.retired.drain(..) {
        commands.entity(entity).despawn();
    }
}

/// UTC calendar date.
fn format_epoch(epoch: Epoch) -> String {
    let (year, month, day, ..) = epoch.to_gregorian(TimeScale::UTC);
    format!("{year:04}-{month:02}-{day:02}")
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
    /// Gravity-assist body, if any.
    pub via: Option<String>,
    /// `"searching"`, `"waiting"`, `"enroute"`, `"arrived"` or `"failed"`.
    pub status: &'static str,
    /// Set from `"waiting"` onward — a plan has been found by then.
    pub departure_unix_seconds: Option<f64>,
    pub arrival_unix_seconds: Option<f64>,
    pub departure_delta_v_km_s: Option<f64>,
    pub arrival_delta_v_km_s: Option<f64>,
    /// Set from `"waiting"` onward for a flyby mission.
    pub flyby: Option<FlybyInfo>,
    /// Executed guidance burns (flyby missions), in order.
    pub burns: Vec<BurnInfo>,
    /// Set only for `"enroute"` or `"arrived"` — the body the spacecraft
    /// currently orbits, `terramenta_solare::solar_system`'s own name for it.
    pub orbiting: Option<String>,
    /// Set only for `"failed"`.
    pub reason: Option<&'static str>,
    /// Milestones, in order: departure, escape, then for a flyby encounter
    /// (SOI entry), flyby (periapsis), exit (SOI exit); then arrival (planned)
    /// and capture. See [`MissionPhase`].
    pub phases: Vec<MissionPhase>,
}

/// Designed flyby hyperbola.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FlybyInfo {
    /// Planned periapsis epoch (heliocentric plan).
    pub unix_seconds: f64,
    pub periapsis_altitude_km: f64,
    pub turn_angle_deg: f64,
    pub v_infinity_in_km_s: f64,
    pub v_infinity_out_km_s: f64,
    pub periapsis_delta_v_km_s: f64,
    /// Departure v∞ + periapsis burn + arrival v∞.
    pub total_delta_v_km_s: f64,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BurnInfo {
    /// `"cruise"`, `"approach"`, `"periapsis"` or `"retarget"`.
    pub id: &'static str,
    pub unix_seconds: f64,
    pub delta_v_km_s: f64,
}

/// One milestone in a mission's timeline, for an interface to build a
/// scrubber or a set of "jump to" controls from.
///
/// Always reported, even before it is known — `unix_seconds` is `None` until
/// then, rather than the entry being left out, so an interface can render a
/// fixed timeline and grey out what is not there yet instead of reflowing as
/// the mission progresses.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MissionPhase {
    /// `"departure"`, `"escape"`, `"encounter"`, `"flyby"`, `"exit"`,
    /// `"arrival"` or `"capture"`.
    pub id: &'static str,
    pub label: String,
    pub unix_seconds: Option<f64>,
    /// Whether the simulated clock has reached this milestone yet. `false`
    /// while `unix_seconds` is still `None`, since an unknown moment cannot
    /// have been reached.
    pub reached: bool,
}

impl MissionPhase {
    fn new(
        id: &'static str,
        label: String,
        unix_seconds: Option<f64>,
        sun_unix_seconds: f64,
    ) -> Self {
        Self {
            id,
            label,
            unix_seconds,
            reached: unix_seconds.is_some_and(|seconds| sun_unix_seconds >= seconds),
        }
    }
}

/// Observed milestone times.
#[derive(Default, Clone, Copy)]
struct Observed {
    escape: Option<f64>,
    encounter: Option<f64>,
    periapsis: Option<f64>,
    exit: Option<f64>,
    capture: Option<f64>,
}

impl MissionInfo {
    fn new(request: &MissionRequest, status: &'static str) -> Self {
        Self {
            id: request.id.clone(),
            origin: request.origin.clone(),
            destination: request.destination.clone(),
            via: request.via.clone(),
            status,
            departure_unix_seconds: None,
            arrival_unix_seconds: None,
            departure_delta_v_km_s: None,
            arrival_delta_v_km_s: None,
            flyby: None,
            burns: Vec::new(),
            orbiting: None,
            reason: None,
            phases: Vec::new(),
        }
    }

    fn with_plan(mut self, plan: &TransferPlan, flyby: Option<&FlybyPlan>) -> Self {
        self.departure_unix_seconds = Some(plan.departure.to_unix_seconds());
        self.arrival_unix_seconds = Some(plan.arrival.to_unix_seconds());
        self.departure_delta_v_km_s = Some(plan.departure_delta_v_magnitude_km_s());
        self.arrival_delta_v_km_s = Some(plan.arrival_delta_v_magnitude_km_s());
        self.flyby = flyby.map(|flyby| {
            let radius = self.via.as_deref().and_then(body_radius_km).unwrap_or(0.0);
            FlybyInfo {
                unix_seconds: flyby.flyby_epoch().to_unix_seconds(),
                periapsis_altitude_km: flyby.flyby.periapsis_radius_km - radius,
                turn_angle_deg: flyby.flyby.turn_angle_rad.to_degrees(),
                v_infinity_in_km_s: flyby.flyby.v_infinity_in_km_s.length(),
                v_infinity_out_km_s: flyby.flyby.v_infinity_out_km_s.length(),
                periapsis_delta_v_km_s: flyby.flyby.periapsis_delta_v_km_s,
                total_delta_v_km_s: flyby.total_delta_v_km_s(),
            }
        });
        self
    }

    /// Fills in [`MissionInfo::phases`] — every milestone, drawing
    /// departure/arrival (and the planned periapsis) from
    /// [`MissionInfo::with_plan`], the rest from observation. Called for every
    /// mission regardless of state.
    fn with_phases(mut self, observed: Observed, sun_unix_seconds: f64) -> Self {
        let phase =
            |id, label, unix_seconds| MissionPhase::new(id, label, unix_seconds, sun_unix_seconds);
        let mut phases = vec![
            phase(
                "departure",
                format!("Depart {}", self.origin),
                self.departure_unix_seconds,
            ),
            phase("escape", format!("Escape {}", self.origin), observed.escape),
        ];
        if let Some(via) = &self.via {
            let planned = self.flyby.as_ref().map(|flyby| flyby.unix_seconds);
            phases.extend([
                phase("encounter", format!("Enter {via} SOI"), observed.encounter),
                phase(
                    "flyby",
                    format!("{via} flyby"),
                    observed.periapsis.or(planned),
                ),
                phase("exit", format!("Leave {via} SOI"), observed.exit),
            ]);
        }
        phases.extend([
            phase(
                "arrival",
                format!("Arrive at {}", self.destination),
                self.arrival_unix_seconds,
            ),
            phase(
                "capture",
                format!("Captured by {}", self.destination),
                observed.capture,
            ),
        ]);
        self.phases = phases;
        self
    }
}

type SpacecraftQuery<'w, 's> =
    Query<'w, 's, (&'static TrackedSpacecraft, Option<&'static Guidance>)>;

impl Mission {
    fn describe(
        &self,
        solar_system: &SolarSystem,
        spacecraft: &SpacecraftQuery,
        sun_unix_seconds: f64,
    ) -> MissionInfo {
        match &self.state {
            MissionState::Searching => MissionInfo::new(&self.request, "searching")
                .with_phases(Observed::default(), sun_unix_seconds),
            MissionState::Waiting { plan, flyby, .. } => MissionInfo::new(&self.request, "waiting")
                .with_plan(plan, flyby.as_ref())
                .with_phases(Observed::default(), sun_unix_seconds),
            MissionState::Launched {
                entity,
                plan,
                flyby,
                destination_frame,
                escape_unix_seconds,
                encounter_unix_seconds,
                exit_unix_seconds,
                capture_unix_seconds,
                ..
            } => {
                let craft = spacecraft.get(*entity).ok();
                let orbiting = craft.map(|(craft, _)| craft.0.primary_frame());
                let burns: Vec<BurnInfo> = craft
                    .and_then(|(_, guidance)| guidance)
                    .map(|guidance| {
                        guidance
                            .0
                            .burns()
                            .iter()
                            .map(|burn| BurnInfo {
                                id: burn.kind.id(),
                                unix_seconds: burn.epoch.to_unix_seconds(),
                                delta_v_km_s: burn.delta_v_km_s.length(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let periapsis = burns
                    .iter()
                    .find(|burn| burn.id == "periapsis")
                    .map(|burn| burn.unix_seconds);
                let arrived = orbiting == Some(*destination_frame);
                let mut info =
                    MissionInfo::new(&self.request, if arrived { "arrived" } else { "enroute" })
                        .with_plan(plan, flyby.as_ref())
                        .with_phases(
                            Observed {
                                escape: *escape_unix_seconds,
                                encounter: *encounter_unix_seconds,
                                periapsis,
                                exit: *exit_unix_seconds,
                                capture: *capture_unix_seconds,
                            },
                            sun_unix_seconds,
                        );
                info.burns = burns;
                info.orbiting = orbiting.map(|frame| solar_system.tree().name(frame).to_string());
                info
            }
            MissionState::Failed { reason } => {
                let mut info = MissionInfo::new(&self.request, "failed")
                    .with_phases(Observed::default(), sun_unix_seconds);
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
        spacecraft: &SpacecraftQuery,
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
/// currently orbits (and its [`Guidance`] burns), and [`SolarSystem`] to name
/// it. One parameter rather than three because a system may only take
/// sixteen, same as [`crate::placemark::PlacemarkPicks`].
#[derive(SystemParam)]
pub struct MissionReport<'w, 's> {
    settings: Res<'w, MissionSettings>,
    solar_system: Res<'w, SolarSystem>,
    spacecraft: SpacecraftQuery<'w, 's>,
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
    use bevy::ecs::system::SystemState;

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
    fn format_epoch_is_utc_date() {
        for (h, m) in [(0, 0), (11, 59), (12, 0), (23, 59)] {
            let e = Epoch::from_utc(2026, 3, 1, h, m, 0.0);
            assert_eq!(format_epoch(e), "2026-03-01", "{h:02}:{m:02}");
        }
        assert_eq!(
            format_epoch(Epoch::from_utc(1999, 12, 31, 6, 0, 0.0)),
            "1999-12-31"
        );
    }

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
            panic!(
                "expected the mission to still be Launched, got {:?}",
                mission.state
            );
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
        let spacecraft_count = |app: &mut App| {
            app.world_mut()
                .query::<&TrackedSpacecraft>()
                .iter(app.world())
                .count()
        };

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

    /// Removed and re-added under the same id (a project reopened in place),
    /// a launched mission relaunches under a fresh frame name.
    #[test]
    fn a_mission_re_added_under_its_id_relaunches() {
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
        let request = MissionRequest {
            id: "test".to_string(),
            ..MissionRequest::default()
        };
        let is_launched = |app: &App| {
            app.world()
                .resource::<MissionSettings>()
                .missions
                .iter()
                .any(|mission| matches!(mission.state, MissionState::Launched { .. }))
        };
        let run_until_launched = |app: &mut App| {
            for _ in 0..(150 * 24) {
                app.world_mut().resource_mut::<SimClock>().unix_seconds += 3_600.0;
                app.update();
                if is_launched(app) {
                    return;
                }
            }
            panic!("the mission never launched");
        };

        app.world_mut()
            .resource_mut::<MissionSettings>()
            .add(request.clone());
        run_until_launched(&mut app);
        let launch_time = app.world().resource::<SimClock>().unix_seconds;

        app.world_mut().resource_mut::<MissionSettings>().reset();
        app.world_mut().resource_mut::<SimClock>().unix_seconds = start_unix_seconds;
        app.update();
        app.world_mut()
            .resource_mut::<MissionSettings>()
            .add(request);
        run_until_launched(&mut app);
        assert_eq!(app.world().resource::<SimClock>().unix_seconds, launch_time);
    }

    /// What a heliocentric camera following a mission orbits: the origin
    /// body until launch, the spacecraft's own frame from then on, and a
    /// fresh frame — not the retired one — after a relaunch.
    #[test]
    fn a_followed_mission_resolves_to_its_origin_then_its_spacecraft() {
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

        let camera_frame = |app: &App| {
            let world = app.world();
            world
                .resource::<MissionSettings>()
                .camera_frame("test", world.resource::<SolarSystem>())
        };
        let earth = app.world().resource::<SolarSystem>().find("Earth");
        let spacecraft_frame = |app: &mut App| {
            app.world_mut()
                .query_filtered::<&crate::solar::SolarBody, With<TrackedSpacecraft>>()
                .iter(app.world())
                .next()
                .map(|body| body.0)
        };

        // Searching, before the first tick has run the search.
        assert_eq!(camera_frame(&app), earth);
        assert_eq!(
            app.world()
                .resource::<MissionSettings>()
                .camera_frame("nope", app.world().resource::<SolarSystem>()),
            None
        );

        for _ in 0..(60 * 24) {
            app.world_mut().resource_mut::<SimClock>().unix_seconds += 3_600.0;
            app.update();
            if spacecraft_frame(&mut app).is_some() {
                break;
            }
            assert_eq!(camera_frame(&app), earth, "waiting sits on the origin");
        }
        let first = spacecraft_frame(&mut app).expect("the mission never launched");
        assert_eq!(camera_frame(&app), Some(first));

        // Back past departure and forward again: a relaunch, under a new frame.
        for _ in 0..(90 * 24) {
            app.world_mut().resource_mut::<SimClock>().unix_seconds -= 3_600.0;
            app.update();
        }
        assert_eq!(camera_frame(&app), earth);
        for _ in 0..(150 * 24) {
            app.world_mut().resource_mut::<SimClock>().unix_seconds += 3_600.0;
            app.update();
            if spacecraft_frame(&mut app).is_some() {
                break;
            }
        }
        let second = spacecraft_frame(&mut app).expect("the mission never relaunched");
        assert_ne!(first, second);
        assert_eq!(camera_frame(&app), Some(second));
    }

    /// Earth–Venus–Mars from the 2034 window: escape, Venus SOI in and out,
    /// Mars capture, with all four guidance burns reported.
    #[test]
    fn a_venus_flyby_mission_reaches_mars() {
        let start = Epoch::from_utc(2034, 6, 1, 0, 0, 0.0).to_unix_seconds();
        let mut app = App::new();
        app.add_plugins(TaskPoolPlugin::default())
            .init_resource::<ReferenceFrame>()
            .init_resource::<ViewState>()
            .insert_resource(SimClock {
                unix_seconds: start,
                ..SimClock::default()
            })
            .add_plugins(SolarSystemPlugin)
            .add_plugins(MissionPlugin {
                initial: Vec::new(),
            });
        app.world_mut()
            .resource_mut::<MissionSettings>()
            .add(MissionRequest {
                id: "evm".to_string(),
                via: Some("Venus".to_string()),
                search_steps: 13,
                ..MissionRequest::default()
            });

        let mut captured = false;
        // Ticks of 6 h; guidance sub-steps.
        for _ in 0..(450 * 4) {
            app.world_mut().resource_mut::<SimClock>().unix_seconds += 6.0 * 3_600.0;
            app.update();
            if let Some(MissionState::Launched {
                capture_unix_seconds: Some(_),
                ..
            }) = app
                .world()
                .resource::<MissionSettings>()
                .missions
                .first()
                .map(|mission| &mission.state)
            {
                captured = true;
                break;
            }
        }
        assert!(captured, "never captured at Mars");

        let now = app.world().resource::<SimClock>().unix_seconds;
        let mut state = SystemState::<MissionReport>::new(app.world_mut());
        let info = state.get(app.world()).unwrap().describe(now).remove(0);
        let ids: Vec<_> = info.phases.iter().map(|phase| phase.id).collect();
        assert_eq!(
            ids,
            [
                "departure",
                "escape",
                "encounter",
                "flyby",
                "exit",
                "arrival",
                "capture"
            ]
        );
        let times: Vec<f64> = info
            .phases
            .iter()
            .filter(|phase| phase.id != "arrival")
            .map(|phase| phase.unix_seconds.expect("observed"))
            .collect();
        assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
        let burns: Vec<_> = info.burns.iter().map(|burn| burn.id).collect();
        assert_eq!(burns, ["cruise", "approach", "periapsis", "retarget"]);
        assert_eq!(info.status, "arrived");
        assert!(info.flyby.unwrap().periapsis_altitude_km >= FLYBY_MIN_ALTITUDE_KM);
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
