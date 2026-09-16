//! Geographic conventions shared by the mesh builder, the camera and the HUD.
//!
//! The globe is a unit sphere in Bevy's right-handed, Y-up world space:
//!
//! * `+Y` is the north pole,
//! * `+Z` is 0° longitude (the prime meridian faces the default camera),
//! * `+X` is 90° east.
//!
//! Textures are equirectangular (plate carrée): `u` runs west→east from the
//! antimeridian, `v` runs north→south from the pole, which is how every NASA
//! Blue Marble image is laid out.

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use std::f32::consts::{PI, TAU};

/// Mean Earth radius, used only to turn scene units into a human-readable altitude.
pub const EARTH_RADIUS_KM: f32 = 6371.0;

/// A point on the globe in degrees.
///
/// This is the shape every coordinate in [`crate::api::GlobeState`] takes, so
/// it serializes straight into the state an embedder reads.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct LatLon {
    pub lat: f32,
    pub lon: f32,
}

impl LatLon {
    pub const fn new(lat: f32, lon: f32) -> Self {
        Self { lat, lon }
    }

    /// The outward unit normal at this coordinate.
    pub fn to_direction(self) -> Vec3 {
        let (lat, lon) = (self.lat.to_radians(), self.lon.to_radians());
        let (sin_lat, cos_lat) = lat.sin_cos();
        let (sin_lon, cos_lon) = lon.sin_cos();
        Vec3::new(cos_lat * sin_lon, sin_lat, cos_lat * cos_lon)
    }

    /// Inverse of [`LatLon::to_direction`]. The input need not be normalized.
    pub fn from_direction(dir: Vec3) -> Self {
        let dir = dir.normalize_or_zero();
        Self {
            lat: dir.y.clamp(-1.0, 1.0).asin().to_degrees(),
            lon: dir.x.atan2(dir.z).to_degrees(),
        }
    }

    /// Formats as `40.7128° N, 74.0060° W`.
    pub fn format(self) -> String {
        let ns = if self.lat >= 0.0 { 'N' } else { 'S' };
        let ew = if self.lon >= 0.0 { 'E' } else { 'W' };
        format!(
            "{:.4}° {}, {:.4}° {}",
            self.lat.abs(),
            ns,
            self.lon.abs(),
            ew
        )
    }
}

/// An axis-aligned latitude/longitude rectangle, as WMS understands a bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoBounds {
    pub lat_min: f32,
    pub lat_max: f32,
    pub lon_min: f32,
    pub lon_max: f32,
}

impl GeoBounds {
    /// The whole world, which is also what a tile is clipped against.
    pub const WORLD: Self = Self {
        lat_min: -90.0,
        lat_max: 90.0,
        lon_min: -180.0,
        lon_max: 180.0,
    };

    /// The overlap between two boxes, or `None` when they do not meet.
    ///
    /// A tile matrix set is free to define tiles that hang off the edge of the
    /// world — see [`crate::tiles::TileGrid`] — so this is what decides how
    /// much of one is real.
    pub fn intersect(self, other: Self) -> Option<Self> {
        let clipped = Self {
            lat_min: self.lat_min.max(other.lat_min),
            lat_max: self.lat_max.min(other.lat_max),
            lon_min: self.lon_min.max(other.lon_min),
            lon_max: self.lon_max.min(other.lon_max),
        };
        // A shared edge is not an overlap: it would be a tile of no width.
        (clipped.lat_min < clipped.lat_max && clipped.lon_min < clipped.lon_max).then_some(clipped)
    }

    pub fn center(self) -> LatLon {
        LatLon::new(
            (self.lat_min + self.lat_max) * 0.5,
            (self.lon_min + self.lon_max) * 0.5,
        )
    }

    pub fn lat_span(self) -> f32 {
        self.lat_max - self.lat_min
    }

    pub fn lon_span(self) -> f32 {
        self.lon_max - self.lon_min
    }
}

/// Returns the near-side intersection of a ray with a sphere centered on the origin,
/// or `None` when the ray misses it.
pub fn ray_sphere_intersection(origin: Vec3, dir: Vec3, radius: f32) -> Option<Vec3> {
    let b = origin.dot(dir);
    let c = origin.length_squared() - radius * radius;
    let discriminant = b * b - c;
    if discriminant < 0.0 {
        return None;
    }
    let sqrt_d = discriminant.sqrt();
    // Prefer the near hit, but accept the far one when the origin is inside the sphere.
    let t = [-b - sqrt_d, -b + sqrt_d].into_iter().find(|t| *t >= 0.0)?;
    Some(origin + dir * t)
}

/// Builds a UV sphere that matches the conventions documented above.
///
/// Bevy's built-in [`Sphere`] primitive is Z-up and wraps its texture the other
/// way around, so the globe generates its own grid: the seam sits at the
/// antimeridian and `v == 0` is the north pole.
pub fn equirectangular_sphere(radius: f32, segments: u32, rings: u32) -> Mesh {
    let segments = segments.max(3);
    let rings = rings.max(2);

    let vertex_count = ((segments + 1) * (rings + 1)) as usize;
    let mut positions = Vec::with_capacity(vertex_count);
    let mut normals = Vec::with_capacity(vertex_count);
    let mut uvs = Vec::with_capacity(vertex_count);
    let mut indices = Vec::with_capacity((segments * rings * 6) as usize);

    for ring in 0..=rings {
        // v == 0 at the north pole, v == 1 at the south pole.
        let v = ring as f32 / rings as f32;
        let polar = v * PI;
        let (sin_polar, cos_polar) = polar.sin_cos();

        for segment in 0..=segments {
            // u == 0 at 180° W, wrapping back to u == 1 at 180° E.
            let u = segment as f32 / segments as f32;
            let lon = u * TAU - PI;
            let (sin_lon, cos_lon) = lon.sin_cos();

            let normal = Vec3::new(sin_polar * sin_lon, cos_polar, sin_polar * cos_lon);
            positions.push((normal * radius).to_array());
            normals.push(normal.to_array());
            uvs.push([u, v]);
        }
    }

    let stride = segments + 1;
    for ring in 0..rings {
        for segment in 0..segments {
            let top_left = ring * stride + segment;
            let top_right = top_left + 1;
            let bottom_left = top_left + stride;
            let bottom_right = bottom_left + 1;

            // Skip the degenerate triangles that collapse into the poles.
            if ring != 0 {
                indices.extend_from_slice(&[top_left, bottom_left, top_right]);
            }
            if ring != rings - 1 {
                indices.extend_from_slice(&[top_right, bottom_left, bottom_right]);
            }
        }
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}
