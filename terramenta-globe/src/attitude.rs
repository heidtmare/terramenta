//! Body axes and sensor boresights on ephemeris layers, from sampled attitude.
//!
//! A [`Track`] is body → TEME quaternions on a Unix-seconds grid (the frame
//! [`crate::omm`] holds tabulated motion in), slerped at the sim clock and drawn at
//! every object the layer drew this frame: X red, Y green, Z blue, a fixed fraction of
//! the camera distance long, so they hold their size on screen. A sensor is a ray along
//! its body axis to the globe surface, or four axis lengths when it misses; one with a
//! field of view also draws its footprint outline on the globe sphere (rays past the
//! limb land on it) and four edges of its cone. Outside a track's span nothing is drawn.
//!
//! Tracks live in a process-wide table keyed by layer id, set from the JavaScript API.
//! A track whose layer is gone, hidden or off draws nothing. [`crate::uncertainty`]
//! ellipsoids share the mesh.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use bevy::camera::visibility::NoFrustumCulling;
use bevy::math::{DQuat, DVec3};
use bevy::prelude::*;

use crate::ephemeris::EphemerisSettings;
use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::EARTH_RADIUS_KM;
use crate::globe::GLOBE_RADIUS;
use crate::gnc::scene_from_canonical;
use crate::overlays::{Paint, VectorMaterial, VectorMode, WorldPath, world_line_mesh};
use crate::time::SimClock;

/// Axis length as a fraction of the camera distance.
const AXIS_FRACTION: f32 = 0.06;
/// Boresight length, in axis lengths, when it misses the globe.
const MISS_LENGTH: f32 = 4.0;
const AXIS_WIDTH_PX: f32 = 2.5;
const SENSOR_WIDTH_PX: f32 = 1.5;
const AXIS_COLORS: [Srgba; 3] = [
    Srgba::rgb(1.0, 0.33, 0.33),
    Srgba::rgb(0.4, 0.9, 0.4),
    Srgba::rgb(0.4, 0.6, 1.0),
];

/// A body axis drawn as a boresight, and its field of view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sensor {
    pub body: DVec3,
    pub color: Srgba,
    pub fov: Option<terramenta_solare::coverage::Sensor>,
}

/// Footprint outline rays.
const FOOTPRINT_RAYS: usize = 48;
/// Segments per outline edge, laid on the sphere.
const FOOTPRINT_SUBDIVISIONS: usize = 4;
/// Outline radius: over the highest imagery tile, as the GNC surface lines.
const FOOTPRINT_RADIUS: f32 = crate::tiles::MAX_TILE_RADIUS + GLOBE_RADIUS * 3.0e-4;

/// Sampled attitude of one layer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    /// `(unix seconds, body → TEME)`, ascending.
    pub samples: Vec<(f64, DQuat)>,
    pub axes: bool,
    pub sensors: Vec<Sensor>,
}

impl Track {
    /// Slerped attitude at `unix`; `None` outside the span.
    pub fn at(&self, unix: f64) -> Option<DQuat> {
        let (first, last) = (self.samples.first()?, self.samples.last()?);
        if !(first.0..=last.0).contains(&unix) {
            return None;
        }
        let i = self.samples.partition_point(|s| s.0 <= unix);
        if i >= self.samples.len() {
            return Some(last.1);
        }
        let (a, b) = (self.samples[i - 1], self.samples[i]);
        let s = if b.0 > a.0 {
            (unix - a.0) / (b.0 - a.0)
        } else {
            0.0
        };
        Some(terramenta_solare::attitude::slerp(a.1, b.1, s))
    }
}

static TRACKS: LazyLock<RwLock<HashMap<String, Arc<Track>>>> = LazyLock::new(Default::default);

fn tracks() -> std::sync::RwLockReadGuard<'static, HashMap<String, Arc<Track>>> {
    TRACKS.read().unwrap_or_else(|e| e.into_inner())
}

fn tracks_mut() -> std::sync::RwLockWriteGuard<'static, HashMap<String, Arc<Track>>> {
    TRACKS.write().unwrap_or_else(|e| e.into_inner())
}

pub fn set(id: impl Into<String>, track: Track) {
    tracks_mut().insert(id.into(), Arc::new(track));
}

pub fn remove(id: &str) -> bool {
    tracks_mut().remove(id).is_some()
}

pub fn clear() {
    tracks_mut().clear();
}

pub struct AttitudePlugin;

impl Plugin for AttitudePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            draw_attitude
                .in_set(FrameSet::Apply)
                .after(crate::ephemeris::draw_ephemerides),
        );
    }
}

/// The one entity and mesh all tracks are drawn into.
#[derive(Default)]
struct Drawn {
    entity: Option<Entity>,
    mesh: Option<Handle<Mesh>>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "reads the clock, frame, view, camera and layers; owns one mesh"
)]
fn draw_attitude(
    mut commands: Commands,
    clock: Res<SimClock>,
    frame: Res<ReferenceFrame>,
    view: Res<crate::view::ViewState>,
    ephemerides: Res<EphemerisSettings>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    mut drawn: Local<Drawn>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VectorMaterial>>,
) {
    let eye = camera.iter().next().map(GlobalTransform::translation);
    let paths = match eye {
        Some(eye) if !crate::view::is_departing_view(&view) => {
            let mut paths = paths(&ephemerides, clock.unix_seconds, &frame, eye);
            paths.extend(crate::uncertainty::paths(
                &ephemerides,
                clock.unix_seconds,
                &frame,
            ));
            paths
        }
        _ => Vec::new(),
    };
    let mesh = world_line_mesh(&paths);

    let visible = mesh.is_some();
    if let Some(mesh) = mesh {
        match drawn.mesh.as_ref() {
            Some(handle) => {
                let _ = meshes.insert(handle.id(), mesh);
            }
            None => {
                let handle = meshes.add(mesh);
                let material = materials.add(
                    VectorMaterial::new(
                        VectorMode::Line,
                        Paint {
                            color: AXIS_COLORS[0],
                            size_px: AXIS_WIDTH_PX,
                        },
                    )
                    .above(),
                );
                drawn.entity = Some(
                    commands
                        .spawn((
                            Name::new("Attitude"),
                            Mesh3d(handle.clone()),
                            MeshMaterial3d(material),
                            Transform::IDENTITY,
                            NoFrustumCulling,
                        ))
                        .id(),
                );
                drawn.mesh = Some(handle);
            }
        }
    }
    if let Some(entity) = drawn.entity {
        commands.entity(entity).insert(if visible {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

/// World-space paths for every track at `now`.
fn paths(
    ephemerides: &EphemerisSettings,
    now: f64,
    frame: &ReferenceFrame,
    eye: Vec3,
) -> Vec<WorldPath> {
    let to_world = frame.inertial_to_world();
    let world = |v: DVec3| to_world * scene_from_canonical(v);
    let mut paths = Vec::new();
    for (id, track) in tracks().iter() {
        let Some(q) = track.at(now) else {
            continue;
        };
        let Some((catalogue, objects)) = ephemerides.drawn_objects(id) else {
            continue;
        };
        for index in objects {
            let Some(teme) = catalogue.satellites[index].position_teme_km(now) else {
                continue;
            };
            let at = world(DVec3::from_array(teme) / f64::from(EARTH_RADIUS_KM));
            let length = AXIS_FRACTION * eye.distance(at);
            let line = |direction: Vec3, length: f32, color: Srgba, size_px: f32| WorldPath {
                points: vec![at, at + direction * length],
                paint: Some(Paint { color, size_px }),
            };
            if track.axes {
                for (axis, color) in [DVec3::X, DVec3::Y, DVec3::Z].into_iter().zip(AXIS_COLORS) {
                    paths.push(line(world(q * axis), length, color, AXIS_WIDTH_PX));
                }
            }
            for sensor in &track.sensors {
                let direction = world(q * sensor.body).normalize_or_zero();
                let reach = surface_hit(at, direction).unwrap_or(MISS_LENGTH * length);
                paths.push(line(direction, reach, sensor.color, SENSOR_WIDTH_PX));
                if let Some(fov) = &sensor.fov {
                    let ground: Vec<Vec3> = fov
                        .boundary(FOOTPRINT_RAYS)
                        .into_iter()
                        .map(|d| surface_point(at, world(q * d).normalize_or_zero()))
                        .collect();
                    let paint = Some(Paint {
                        color: sensor.color,
                        size_px: SENSOR_WIDTH_PX,
                    });
                    for corner in ground.iter().step_by(ground.len() / 4) {
                        paths.push(WorldPath {
                            points: vec![at, *corner],
                            paint,
                        });
                    }
                    paths.push(WorldPath {
                        points: outline(&ground),
                        paint,
                    });
                }
            }
        }
    }
    paths
}

/// Where unit `direction` from `from` (outside) meets the globe sphere, or the limb
/// point in the plane of the ray and the centre when it misses.
fn surface_point(from: Vec3, direction: Vec3) -> Vec3 {
    if let Some(t) = surface_hit(from, direction) {
        return from + direction * t;
    }
    let r = from.length();
    let up = from / r;
    let side = (direction - up * direction.dot(up))
        .try_normalize()
        .unwrap_or_else(|| up.any_orthonormal_vector());
    let k = (GLOBE_RADIUS / r).min(1.0);
    (up * k + side * (1.0 - k * k).sqrt()) * GLOBE_RADIUS
}

/// Closed ring through `ground`, each edge subdivided along the sphere and lifted.
fn outline(ground: &[Vec3]) -> Vec<Vec3> {
    let radius = FOOTPRINT_RADIUS;
    let mut points = Vec::with_capacity(ground.len() * FOOTPRINT_SUBDIVISIONS + 1);
    for (i, &a) in ground.iter().enumerate() {
        let b = ground[(i + 1) % ground.len()];
        for k in 0..FOOTPRINT_SUBDIVISIONS {
            let s = k as f32 / FOOTPRINT_SUBDIVISIONS as f32;
            points.push(a.lerp(b, s).normalize_or_zero() * radius);
        }
    }
    points.extend(points.first().copied());
    points
}

/// Distance along unit `direction` from `from` to the globe sphere, if it is hit ahead.
fn surface_hit(from: Vec3, direction: Vec3) -> Option<f32> {
    let b = from.dot(direction);
    let c = from.length_squared() - GLOBE_RADIUS * GLOBE_RADIUS;
    let disc = b * b - c;
    if c <= 0.0 || disc < 0.0 {
        return None;
    }
    let t = -b - disc.sqrt();
    (t > 0.0).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_slerps_inside_its_span_only() {
        let track = Track {
            samples: vec![
                (0.0, DQuat::IDENTITY),
                (10.0, DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2)),
            ],
            ..Track::default()
        };
        let half = track.at(5.0).unwrap();
        assert!((half.angle_between(DQuat::IDENTITY) - std::f64::consts::FRAC_PI_4).abs() < 1e-12);
        assert_eq!(track.at(10.0), Some(track.samples[1].1));
        assert!(track.at(-1.0).is_none() && track.at(10.5).is_none());
        assert!(Track::default().at(0.0).is_none());
    }

    #[test]
    fn a_boresight_stops_at_the_surface() {
        let from = Vec3::new(0.0, 0.0, 2.0);
        assert!((surface_hit(from, -Vec3::Z).unwrap() - 1.0).abs() < 1e-6);
        assert!(surface_hit(from, Vec3::Z).is_none());
        assert!(surface_hit(from, Vec3::X).is_none());
    }

    #[test]
    fn footprints_lie_on_the_sphere() {
        let from = Vec3::new(0.0, 0.0, 2.0);
        assert!((surface_point(from, -Vec3::Z) - Vec3::Z).length() < 1e-6);
        // Past the limb: tangent, so perpendicular to the line of sight.
        let limb = surface_point(from, Vec3::X);
        assert!((limb.length() - 1.0).abs() < 1e-6);
        assert!((from - limb).dot(limb).abs() < 1e-5);
        let ring = outline(&[Vec3::X, Vec3::Y, Vec3::Z]);
        assert_eq!(ring.len(), 3 * FOOTPRINT_SUBDIVISIONS + 1);
        assert_eq!(ring.first(), ring.last());
        assert!(
            ring.iter()
                .all(|p| (p.length() - FOOTPRINT_RADIUS).abs() < 1e-5)
        );
    }

    #[test]
    fn tracks_are_set_and_cleared() {
        set("attitude-test", Track::default());
        assert!(remove("attitude-test"));
        assert!(!remove("attitude-test"));
    }
}
