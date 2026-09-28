//! A mission spacecraft's trails: a solid white line along where it has
//! been, and a dashed yellow one along where the mission plan says it is
//! going.
//!
//! [`MissionTrail`] rides on the spacecraft entity itself, inserted by
//! [`crate::mission::launch_missions`] alongside the physics half, so it goes
//! when the spacecraft does — an un-launch (the clock run back past
//! departure) takes the whole history with it, and a relaunch starts clean.
//!
//! **Where it has been** is recorded rather than recomputed: the spacecraft
//! crosses from one conic to the next at a sphere of influence, and the
//! record is the one account of that path that needs nothing re-derived when
//! it does. Each sample is kept twice over — relative to the origin body for
//! the globe view, whose floating origin is that body, and relative to the
//! barycentre for the heliocentric view, whose floating origin that is — so
//! drawing either never has to walk the frame tree once per sample. Running
//! the clock backwards trims the record back to the clock.
//!
//! **Where it is going** is worked out once, at launch, from the same inputs
//! the launch itself used: the departure hyperbola, relative to the origin,
//! out to the origin's sphere of influence — what the globe view shows — and
//! the Lambert transfer [`terramenta_solare::mission::TransferPlan`] was
//! solved from, across to arrival — what the heliocentric view shows. It is
//! the plan, not a prediction: the real flight leaves the origin's sphere of
//! influence a little off the idealised patched-conic departure point, so the
//! two part by a hair the heliocentric view is far too coarse to show.
//!
//! Both are drawn through [`crate::overlays`]' pixel-width ribbons, rebuilt
//! every tick from wherever the spacecraft is now. The dashes are cut on the
//! CPU, at a length proportional to each point's distance from the camera, so
//! they stay about the same size on screen at any zoom and in either view.

use bevy::camera::visibility::NoFrustumCulling;
use bevy::math::DVec3;
use bevy::prelude::*;
use terramenta_solare::bodies::{ASTRONOMICAL_UNIT_KM, GM_SUN_KM3_S2};
use terramenta_solare::orbit::OrbitalElements;
use terramenta_solare::{Epoch, FrameId, StateVector, TransferPlan};

use crate::frame::ReferenceFrame;
use crate::geo::EARTH_RADIUS_KM;
use crate::gnc::scene_from_canonical;
use crate::heliocentric::HeliocentricCamera;
use crate::overlays::{Paint, VectorMaterial, VectorMode, WorldPath, world_line_mesh};
use crate::solar::{FloatingOrigin, SolarBody, SolarSystem, TrackedSpacecraft};
use crate::time::SimClock;
use crate::view::ViewState;

/// Where the spacecraft has been.
const PAST_PAINT: Paint = Paint {
    color: Srgba::new(1.0, 1.0, 1.0, 0.85),
    size_px: 2.0,
};
/// Where the mission plan has it going — the same yellow as its marker.
const PLANNED_PAINT: Paint = Paint {
    color: Srgba::new(1.0, 0.85, 0.2, 0.9),
    size_px: 2.0,
};

/// How often the record takes a sample, in simulated seconds: finely inside
/// a body's sphere of influence, where a parking orbit turns through a
/// radian in a quarter-hour, and coarsely on the cruise between, where a day
/// is a fraction of a degree of arc around the Sun.
const NEAR_BODY_SAMPLE_SECONDS: f64 = 60.0;
const CRUISE_SAMPLE_SECONDS: f64 = 6.0 * 3_600.0;

/// How finely the departure hyperbola is sampled: each step covers this
/// fraction of the time the spacecraft takes to cover its own distance from
/// the origin at its current speed — short steps round periapsis, where it
/// turns hardest, lengthening as it climbs out onto the asymptote.
const HYPERBOLA_STEP_FRACTION: f64 = 0.02;
const HYPERBOLA_MAX_SAMPLES: usize = 4_000;
/// How finely the Lambert transfer is sampled — a day's arc of a 300-day
/// cruise is a smooth enough curve at any zoom the heliocentric camera
/// reaches.
const TRANSFER_STEP_SECONDS: f64 = 86_400.0;

/// A dash, and the gap after it, as a fraction of the dash's distance from
/// the camera — about 12 px each at the globe's field of view on a 900 px
/// tall viewport.
const DASH_FRACTION: f32 = 0.012;

/// One sample of a path: when, and where relative to each of the two frames
/// either view is drawn from.
#[derive(Debug, Clone, Copy)]
struct Sample {
    unix_seconds: f64,
    /// Relative to the mission's origin body — what the globe view draws.
    origin_km: DVec3,
    /// Relative to the barycentre — what the heliocentric view draws.
    root_km: DVec3,
}

/// A mission spacecraft's record of where it has been, and the plan of where
/// it is going. See the module docs.
#[derive(Component)]
pub(crate) struct MissionTrail {
    origin_frame: FrameId,
    destination_frame: FrameId,
    past: Vec<Sample>,
    /// The departure hyperbola, relative to the origin body, out to its
    /// sphere of influence. `root_km` is unused.
    planned_departure: Vec<Sample>,
    /// The Lambert transfer, relative to the barycentre. `origin_km` is
    /// unused.
    planned_transfer: Vec<Sample>,
}

impl MissionTrail {
    /// The plan for a spacecraft launched on `departure_state` — relative to
    /// `origin_frame`, on `plan.departure` — as [`crate::mission`] launches
    /// it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn plan(
        solar_system: &SolarSystem,
        plan: &TransferPlan,
        origin_frame: FrameId,
        destination_frame: FrameId,
        sun_frame: FrameId,
        departure_state: StateVector,
        origin_gm_km3_s2: f64,
        origin_soi_km: f64,
    ) -> Self {
        let tree = solar_system.tree();
        let departure_seconds = plan.departure.to_unix_seconds();
        let arrival_seconds = plan.arrival.to_unix_seconds();

        let hyperbola =
            OrbitalElements::from_state(departure_state, origin_gm_km3_s2, departure_seconds);
        let mut planned_departure = Vec::new();
        let mut t = departure_seconds;
        while planned_departure.len() < HYPERBOLA_MAX_SAMPLES {
            let state = hyperbola.state_at(origin_gm_km3_s2, t);
            planned_departure.push(Sample {
                unix_seconds: t,
                origin_km: state.position_km,
                root_km: DVec3::ZERO,
            });
            let radius = state.position_km.length();
            if radius > origin_soi_km {
                break;
            }
            let speed = state.velocity_km_s.length().max(1.0e-6);
            t += (radius / speed * HYPERBOLA_STEP_FRACTION).max(1.0);
        }

        let origin_at_departure =
            tree.state_of_relative_to(origin_frame, sun_frame, plan.departure);
        let transfer_start = StateVector::new(
            origin_at_departure.position_km,
            origin_at_departure.velocity_km_s + plan.departure_delta_v_km_s,
        );
        let transfer =
            OrbitalElements::from_state(transfer_start, GM_SUN_KM3_S2, departure_seconds);
        let steps = ((arrival_seconds - departure_seconds) / TRANSFER_STEP_SECONDS)
            .ceil()
            .max(1.0) as usize;
        let planned_transfer = (0..=steps)
            .map(|step| {
                let t = departure_seconds
                    + (arrival_seconds - departure_seconds) * step as f64 / steps as f64;
                let sun = tree
                    .state_relative_to_root(sun_frame, Epoch::from_unix_seconds(t))
                    .position_km;
                Sample {
                    unix_seconds: t,
                    origin_km: DVec3::ZERO,
                    root_km: sun + transfer.state_at(GM_SUN_KM3_S2, t).position_km,
                }
            })
            .collect();

        Self {
            origin_frame,
            destination_frame,
            past: Vec::new(),
            planned_departure,
            planned_transfer,
        }
    }
}

/// The two ribbons a trail is drawn with, spawned as children of the
/// spacecraft so they go when it does. Their meshes are built relative to
/// the spacecraft's own position — which is also what keeps a trail near
/// Mars as precise as the marker it ends at.
#[derive(Component)]
struct TrailDrawing {
    past: Handle<Mesh>,
    planned: Handle<Mesh>,
    past_entity: Entity,
    planned_entity: Entity,
}

pub struct TrailPlugin;

impl Plugin for TrailPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            (record_trails, draw_trails)
                .chain()
                .before(bevy::transform::TransformSystems::Propagate),
        );
    }
}

/// Trims each record back to the clock, then samples where the spacecraft is
/// now if long enough has passed since the last sample.
fn record_trails(
    solar_system: Res<SolarSystem>,
    clock: Res<SimClock>,
    mut trails: Query<(&SolarBody, &TrackedSpacecraft, &mut MissionTrail)>,
) {
    let now = clock.unix_seconds;
    let epoch = Epoch::from_unix_seconds(now);
    let tree = solar_system.tree();
    for (body, craft, mut trail) in &mut trails {
        let kept = trail
            .past
            .partition_point(|sample| sample.unix_seconds <= now);
        trail.past.truncate(kept);

        let primary = craft.0.primary_frame();
        let near_body = primary == trail.origin_frame || primary == trail.destination_frame;
        let interval = if near_body {
            NEAR_BODY_SAMPLE_SECONDS
        } else {
            CRUISE_SAMPLE_SECONDS
        };
        if trail
            .past
            .last()
            .is_some_and(|last| now - last.unix_seconds < interval)
        {
            continue;
        }
        let origin_frame = trail.origin_frame;
        trail.past.push(Sample {
            unix_seconds: now,
            origin_km: tree
                .state_of_relative_to(body.0, origin_frame, epoch)
                .position_km,
            root_km: tree.state_relative_to_root(body.0, epoch).position_km,
        });
    }
}

/// Rebuilds both ribbons for every trail against this tick's view, clock and
/// camera, spawning them the first time a trail is seen.
#[allow(clippy::too_many_arguments)]
fn draw_trails(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VectorMaterial>>,
    solar_system: Res<SolarSystem>,
    origin: Res<FloatingOrigin>,
    clock: Res<SimClock>,
    frame: Res<ReferenceFrame>,
    view: Res<ViewState>,
    camera: Single<&Transform, With<HeliocentricCamera>>,
    trails: Query<(
        Entity,
        &SolarBody,
        &TrackedSpacecraft,
        &MissionTrail,
        Option<&TrailDrawing>,
    )>,
) {
    let now = clock.unix_seconds;
    let epoch = Epoch::from_unix_seconds(now);
    let tree = solar_system.tree();
    let heliocentric = view.is_heliocentric();
    let (orientation, km_per_unit) = if heliocentric {
        (Quat::IDENTITY, ASTRONOMICAL_UNIT_KM)
    } else {
        (frame.inertial_to_world(), f64::from(EARTH_RADIUS_KM))
    };

    for (entity, body, craft, trail, drawing) in &trails {
        // The globe view is drawn around one body; a trail around another
        // has nothing there to be drawn against.
        let drawable = heliocentric || origin.frame == trail.origin_frame;

        // Everything relative to the spacecraft, in the same units and
        // orientation `place_solar_bodies` put it in.
        let here_km = if heliocentric {
            tree.state_relative_to_root(body.0, epoch).position_km
        } else {
            tree.state_of_relative_to(body.0, trail.origin_frame, epoch)
                .position_km
        };
        let spacecraft_world = crate::solar::floating_offset(
            tree,
            body.0,
            origin.frame,
            epoch,
            orientation,
            km_per_unit,
        );
        let to_local = |km: DVec3| -> Vec3 {
            orientation * scene_from_canonical((km - here_km) / km_per_unit)
        };
        let pick = |sample: &Sample| {
            if heliocentric {
                sample.root_km
            } else {
                sample.origin_km
            }
        };

        let mut past: Vec<Vec3> = trail.past.iter().map(|s| to_local(pick(s))).collect();
        past.push(Vec3::ZERO);

        // Captured at the destination, the plan has nothing left to say.
        let arrived = craft.0.primary_frame() == trail.destination_frame;
        let plan = if heliocentric {
            &trail.planned_transfer
        } else {
            &trail.planned_departure
        };
        let mut planned = Vec::new();
        if !arrived {
            planned.push(Vec3::ZERO);
            planned.extend(
                plan.iter()
                    .filter(|sample| sample.unix_seconds > now)
                    .map(|s| to_local(pick(s))),
            );
        }
        let camera_local = camera.translation - spacecraft_world;
        let dashes: Vec<WorldPath> = dash(&planned, camera_local)
            .into_iter()
            .map(|points| WorldPath {
                points,
                paint: None,
            })
            .collect();

        let past_mesh = world_line_mesh(&[WorldPath {
            points: past,
            paint: None,
        }]);
        let planned_mesh = world_line_mesh(&dashes);

        let Some(drawing) = drawing else {
            let past = meshes.add(past_mesh.unwrap_or_else(empty_mesh));
            let planned = meshes.add(planned_mesh.unwrap_or_else(empty_mesh));
            let past_entity = commands
                .spawn(ribbon(
                    "Mission trail (past)",
                    past.clone(),
                    materials.add(VectorMaterial::new(VectorMode::Line, PAST_PAINT).above()),
                ))
                .id();
            let planned_entity = commands
                .spawn(ribbon(
                    "Mission trail (planned)",
                    planned.clone(),
                    materials.add(VectorMaterial::new(VectorMode::Line, PLANNED_PAINT).above()),
                ))
                .id();
            commands
                .entity(entity)
                .add_children(&[past_entity, planned_entity])
                .insert(TrailDrawing {
                    past,
                    planned,
                    past_entity,
                    planned_entity,
                });
            continue;
        };

        // Replaced rather than written into — see `crate::gnc::put` for why a
        // render-world-only mesh cannot be edited in place.
        for (handle, mesh, child) in [
            (&drawing.past, past_mesh, drawing.past_entity),
            (&drawing.planned, planned_mesh, drawing.planned_entity),
        ] {
            let visible = drawable && mesh.is_some();
            if let Some(mesh) = mesh {
                let _ = meshes.insert(handle.id(), mesh);
            }
            // Set outright rather than inherited: the spacecraft these hang
            // off is itself hidden in the globe view — see
            // `crate::heliocentric::GlobeSpacecraftMarker`.
            commands.entity(child).insert(if visible {
                Visibility::Visible
            } else {
                Visibility::Hidden
            });
        }
    }
}

fn ribbon(name: &'static str, mesh: Handle<Mesh>, material: Handle<VectorMaterial>) -> impl Bundle {
    (
        Name::new(name),
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::IDENTITY,
        Visibility::Hidden,
        // Widened in the vertex shader, so the mesh's own bounds understate
        // what is drawn.
        NoFrustumCulling,
    )
}

/// A placeholder for a ribbon with nothing in it yet — one degenerate
/// segment, hidden until the first real build replaces it.
fn empty_mesh() -> Mesh {
    world_line_mesh(&[WorldPath {
        points: vec![Vec3::ZERO, Vec3::X * 1.0e-6],
        paint: None,
    }])
    .expect("a two-point path always builds")
}

/// Cuts `path` into dashes whose length, and the gaps between them, are
/// [`DASH_FRACTION`] of their distance from `camera` — so they read as the
/// same size on screen whether near the camera or far across the scene.
///
/// Walks the path accumulating a phase measured in dash lengths, splitting a
/// segment wherever the phase crosses a whole number: even phases draw, odd
/// ones are the gaps.
fn dash(path: &[Vec3], camera: Vec3) -> Vec<Vec<Vec3>> {
    let mut dashes = Vec::new();
    let mut current: Vec<Vec3> = Vec::new();
    let mut phase = 0.0_f32;
    for pair in path.windows(2) {
        let (mut a, b) = (pair[0], pair[1]);
        loop {
            let on = (phase as u32).is_multiple_of(2);
            if on && current.is_empty() {
                current.push(a);
            }
            let dash_length = (a.distance(camera) * DASH_FRACTION).max(1.0e-9);
            let remaining = a.distance(b);
            let to_boundary = (phase.floor() + 1.0 - phase) * dash_length;
            if to_boundary >= remaining {
                phase += remaining / dash_length;
                if on {
                    current.push(b);
                }
                break;
            }
            let split = a.lerp(b, to_boundary / remaining);
            if on {
                current.push(split);
                dashes.push(std::mem::take(&mut current));
            }
            phase = phase.floor() + 1.0;
            a = split;
        }
    }
    if current.len() >= 2 {
        dashes.push(current);
    }
    dashes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_straight_path_dashes_into_alternating_equal_pieces() {
        // Camera far off to one side, so every dash is near enough the same
        // length.
        let camera = Vec3::new(0.0, 1_000.0, 0.0);
        let dash_length = 1_000.0 * DASH_FRACTION;
        let path = [Vec3::ZERO, Vec3::X * dash_length * 10.0];
        let dashes = dash(&path, camera);
        assert_eq!(dashes.len(), 5);
        for piece in &dashes {
            let length = piece[0].distance(*piece.last().unwrap());
            assert!(
                (length - dash_length).abs() < dash_length * 0.01,
                "{length}"
            );
        }
    }

    #[test]
    fn a_dash_carries_across_a_corner_in_the_path() {
        let camera = Vec3::new(0.0, 0.0, 1_000.0);
        let dash_length = 1_000.0 * DASH_FRACTION;
        // A corner a third of the way into the first dash.
        let corner = Vec3::X * dash_length / 3.0;
        let path = [Vec3::ZERO, corner, corner + Vec3::Y * dash_length * 3.0];
        let dashes = dash(&path, camera);
        assert_eq!(dashes[0].len(), 3, "the first dash bends round the corner");
    }
}
