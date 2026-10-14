//! Position uncertainty ellipsoids on ephemeris layers, from propagated covariance.
//!
//! A [`Track`] is TEME position covariance (km²) on a Unix-seconds grid, interpolated
//! linearly at the sim clock and drawn at every object the layer drew this frame as the
//! three principal ellipses of the `sigma` ellipsoid. Outside a track's span nothing is
//! drawn. Drawn into [`crate::attitude`]'s mesh.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use bevy::math::DVec3;
use bevy::prelude::*;
use terramenta_solare::covariance::symmetric_eigen3;

use crate::ephemeris::EphemerisSettings;
use crate::frame::ReferenceFrame;
use crate::geo::EARTH_RADIUS_KM;
use crate::gnc::scene_from_canonical;
use crate::overlays::{Paint, WorldPath};

const RING_POINTS: usize = 64;
const WIDTH_PX: f32 = 1.5;

/// 3×3 symmetric, km².
pub type Covariance3 = [[f64; 3]; 3];

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// `(unix seconds, TEME position covariance)`, ascending; a repeated time (a burn)
    /// switches at that time.
    pub samples: Vec<(f64, Covariance3)>,
    /// Ellipsoid scale, σ.
    pub sigma: f64,
    pub color: Srgba,
}

impl Track {
    /// Linearly interpolated covariance at `unix`; `None` outside the span.
    pub fn at(&self, unix: f64) -> Option<Covariance3> {
        let (first, last) = (self.samples.first()?, self.samples.last()?);
        if !(first.0..=last.0).contains(&unix) {
            return None;
        }
        let i = self.samples.partition_point(|s| s.0 <= unix);
        if i >= self.samples.len() {
            return Some(last.1);
        }
        let (a, b) = (&self.samples[i - 1], &self.samples[i]);
        let s = if b.0 > a.0 {
            (unix - a.0) / (b.0 - a.0)
        } else {
            0.0
        };
        Some(std::array::from_fn(|r| {
            std::array::from_fn(|c| a.1[r][c] + s * (b.1[r][c] - a.1[r][c]))
        }))
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

/// World-space ellipses for every track at `now`.
pub(crate) fn paths(
    ephemerides: &EphemerisSettings,
    now: f64,
    frame: &ReferenceFrame,
) -> Vec<WorldPath> {
    let to_world = frame.inertial_to_world();
    let world = |v: DVec3| to_world * scene_from_canonical(v);
    let mut paths = Vec::new();
    for (id, track) in tracks().iter() {
        let Some(p) = track.at(now) else {
            continue;
        };
        let Some((catalogue, objects)) = ephemerides.drawn_objects(id) else {
            continue;
        };
        let axes = ellipsoid_axes(&p, track.sigma / f64::from(EARTH_RADIUS_KM));
        let paint = Some(Paint {
            color: track.color,
            size_px: WIDTH_PX,
        });
        for index in objects {
            let Some(teme) = catalogue.satellites[index].position_teme_km(now) else {
                continue;
            };
            let at = world(DVec3::from_array(teme) / f64::from(EARTH_RADIUS_KM));
            for (u, v) in [(0, 1), (0, 2), (1, 2)] {
                paths.push(WorldPath {
                    points: ring(axes[u], axes[v]).map(|d| at + world(d)).collect(),
                    paint,
                });
            }
        }
    }
    paths
}

/// Principal semi-axes of `scale`·√eig(`p`), as vectors.
fn ellipsoid_axes(p: &Covariance3, scale: f64) -> [DVec3; 3] {
    let (values, vectors) = symmetric_eigen3(*p);
    std::array::from_fn(|i| vectors[i] * values[i].max(0.0).sqrt() * scale)
}

/// Closed ring `u cos θ + v sin θ`.
fn ring(u: DVec3, v: DVec3) -> impl Iterator<Item = DVec3> {
    (0..=RING_POINTS).map(move |k| {
        let theta = std::f64::consts::TAU * k as f64 / RING_POINTS as f64;
        u * theta.cos() + v * theta.sin()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(a: f64, b: f64, c: f64) -> Covariance3 {
        [[a, 0.0, 0.0], [0.0, b, 0.0], [0.0, 0.0, c]]
    }

    #[test]
    fn a_track_interpolates_inside_its_span_only() {
        let track = Track {
            samples: vec![(0.0, diag(1.0, 1.0, 1.0)), (10.0, diag(3.0, 1.0, 1.0))],
            sigma: 3.0,
            color: Srgba::WHITE,
        };
        assert_eq!(track.at(5.0).unwrap()[0][0], 2.0);
        assert_eq!(track.at(10.0).unwrap()[0][0], 3.0);
        assert!(track.at(-1.0).is_none() && track.at(11.0).is_none());
    }

    #[test]
    fn axes_scale_with_sigma() {
        let axes = ellipsoid_axes(&diag(4.0, 9.0, 1.0), 3.0);
        let mut lengths = axes.map(DVec3::length);
        lengths.sort_by(f64::total_cmp);
        assert_eq!(lengths, [3.0, 6.0, 9.0]);
        let points: Vec<_> = ring(DVec3::X, DVec3::Y * 2.0).collect();
        assert_eq!(points.len(), RING_POINTS + 1);
        assert!((points[0] - points[RING_POINTS]).length() < 1e-12);
    }
}
