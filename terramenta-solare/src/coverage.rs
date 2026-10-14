//! Sensor fields of view, ground footprints on the WGS-84 ellipsoid, and access
//! coverage of an H3 grid.
//!
//! Coverage is sampled: a cell is covered at a sample when some observer sees its
//! centre, and statistics count samples, so times resolve to one step. Access is
//! line of sight above a minimum elevation, and, for an observer with sensors, inside
//! a field of view. No refraction, terrain or light time.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use glam::{DQuat, DVec3};
pub use h3o;
use h3o::{CellIndex, LatLng, Resolution};

use crate::earth::{WGS84_A_KM, WGS84_F, ecef_to_geodetic, geodetic_to_ecef};

/// WGS-84 polar radius, km.
const WGS84_B_KM: f64 = WGS84_A_KM * (1.0 - WGS84_F);

// --- Fields of view -------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldOfView {
    /// Half-angle `(0, π)`.
    Cone { half_angle_rad: f64 },
    /// Half-angles about the sensor's y and x axes, each `(0, π/2)`.
    Rectangle { half_x_rad: f64, half_y_rad: f64 },
}

/// A body-mounted sensor. Its frame: +z along `boresight`, +x toward `reference`
/// (made orthogonal), +y = z × x.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sensor {
    x: DVec3,
    y: DVec3,
    z: DVec3,
    pub fov: FieldOfView,
}

impl Sensor {
    /// `None` for a zero boresight, a reference parallel to it, or angles out of range.
    /// `reference`: default the body axis least aligned with the boresight.
    pub fn new(boresight: DVec3, reference: Option<DVec3>, fov: FieldOfView) -> Option<Self> {
        let ok = |a: f64, max: f64| a > 0.0 && a < max;
        let valid = match fov {
            FieldOfView::Cone { half_angle_rad } => ok(half_angle_rad, PI),
            FieldOfView::Rectangle {
                half_x_rad,
                half_y_rad,
            } => ok(half_x_rad, FRAC_PI_2) && ok(half_y_rad, FRAC_PI_2),
        };
        let z = boresight.try_normalize().filter(|_| valid)?;
        let reference = reference.unwrap_or_else(|| {
            let a = z.abs();
            if a.x <= a.y && a.x <= a.z {
                DVec3::X
            } else if a.y <= a.z {
                DVec3::Y
            } else {
                DVec3::Z
            }
        });
        let x = (reference - z * reference.dot(z)).try_normalize()?;
        Some(Self {
            x,
            y: z.cross(x),
            z,
            fov,
        })
    }

    pub fn boresight(&self) -> DVec3 {
        self.z
    }

    /// Whether body direction `d` is inside the field of view (edge included).
    pub fn contains(&self, d: DVec3) -> bool {
        let (dx, dy, dz) = (d.dot(self.x), d.dot(self.y), d.dot(self.z));
        match self.fov {
            FieldOfView::Cone { half_angle_rad } => dx.hypot(dy).atan2(dz) <= half_angle_rad,
            FieldOfView::Rectangle {
                half_x_rad,
                half_y_rad,
            } => dz > 0.0 && dx.abs() <= half_x_rad.tan() * dz && dy.abs() <= half_y_rad.tan() * dz,
        }
    }

    /// `n` (≥ 4) unit body directions around the edge, counter-clockwise about the
    /// boresight; a rectangle's corners included.
    pub fn boundary(&self, n: usize) -> Vec<DVec3> {
        let n = n.max(4);
        match self.fov {
            FieldOfView::Cone { half_angle_rad } => {
                let (s, c) = half_angle_rad.sin_cos();
                (0..n)
                    .map(|i| {
                        let (st, ct) = (TAU * i as f64 / n as f64).sin_cos();
                        self.z * c + (self.x * ct + self.y * st) * s
                    })
                    .collect()
            }
            FieldOfView::Rectangle {
                half_x_rad,
                half_y_rad,
            } => {
                let (tx, ty) = (half_x_rad.tan(), half_y_rad.tan());
                let corners = [(tx, -ty), (tx, ty), (-tx, ty), (-tx, -ty)];
                let per_side = n.div_ceil(4);
                (0..4)
                    .flat_map(|side| {
                        let (a, b) = (corners[side], corners[(side + 1) % 4]);
                        (0..per_side).map(move |k| {
                            let s = k as f64 / per_side as f64;
                            (a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s)
                        })
                    })
                    .map(|(u, v)| (self.x * u + self.y * v + self.z).normalize())
                    .collect()
            }
        }
    }
}

// --- Ground intersection --------------------------------------------------------------

/// ITRF → space where the ellipsoid is the sphere of radius `WGS84_A_KM`.
fn scaled(v: DVec3) -> DVec3 {
    DVec3::new(v.x, v.y, v.z * WGS84_A_KM / WGS84_B_KM)
}

fn unscaled(v: DVec3) -> DVec3 {
    DVec3::new(v.x, v.y, v.z * WGS84_B_KM / WGS84_A_KM)
}

/// First point where the ray `origin + t·dir`, `t > 0` (ITRF, km), meets the
/// ellipsoid; `None` if it misses or starts inside.
pub fn ellipsoid_hit(origin: DVec3, dir: DVec3) -> Option<DVec3> {
    let (o, d) = (scaled(origin), scaled(dir));
    let a = d.length_squared();
    let b = o.dot(d);
    let c = o.length_squared() - WGS84_A_KM * WGS84_A_KM;
    let disc = b * b - a * c;
    if a == 0.0 || c <= 0.0 || disc < 0.0 {
        return None;
    }
    let t = (-b - disc.sqrt()) / a;
    (t > 0.0).then(|| origin + dir * t)
}

/// Where `dir` from `origin` meets the ellipsoid, or, when it misses, the limb point
/// in the plane of the ray and the Earth's centre. `origin` outside the ellipsoid.
pub fn ground_point(origin: DVec3, dir: DVec3) -> DVec3 {
    if let Some(hit) = ellipsoid_hit(origin, dir) {
        return hit;
    }
    let o = scaled(origin);
    let r = o.length();
    let u = o / r;
    let d = scaled(dir);
    let w = (d - u * d.dot(u))
        .try_normalize()
        .unwrap_or_else(|| u.any_orthonormal_vector());
    let k = (WGS84_A_KM / r).min(1.0);
    unscaled((u * k + w * (1.0 - k * k).sqrt()) * WGS84_A_KM)
}

/// Ground outline of `sensor` on an observer at `position` (ITRF, km) with attitude
/// `body_to_itrf`: `n` points, geodetic `(lat, lon)` rad. Rays past the limb
/// land on it.
pub fn footprint(
    sensor: &Sensor,
    position: DVec3,
    body_to_itrf: DQuat,
    n: usize,
) -> Vec<(f64, f64)> {
    sensor
        .boundary(n)
        .into_iter()
        .map(|d| {
            let (lat, lon, _) = ecef_to_geodetic(ground_point(position, body_to_itrf * d));
            (lat, lon)
        })
        .collect()
}

// --- Grid -----------------------------------------------------------------------------

/// Geodetic box, rad. `west > east` crosses the antimeridian.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl Region {
    fn width(&self) -> f64 {
        let w = (self.east - self.west).rem_euclid(TAU);
        if w == 0.0 && self.east != self.west {
            TAU
        } else {
            w
        }
    }

    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        self.near(lat, lon, 0.0)
    }

    /// Whether `(lat, lon)` is within about `margin` (rad, central angle) of the box.
    fn near(&self, lat: f64, lon: f64, margin: f64) -> bool {
        if lat < self.south - margin || lat > self.north + margin {
            return false;
        }
        let pole = (lat.abs() + margin).min(FRAC_PI_2 - 1e-3);
        let slack = margin / pole.cos();
        let width = self.width() + 2.0 * slack;
        width >= TAU || (lon - (self.west - slack)).rem_euclid(TAU) <= width
    }
}

/// H3 cells at one resolution, each with its centre on the ellipsoid.
#[derive(Debug, Clone)]
pub struct Grid {
    pub resolution: Resolution,
    pub cells: Vec<CellIndex>,
    /// Centre, ITRF km, height 0.
    pub points: Vec<DVec3>,
    /// Geodetic up at the centre; also the centre's direction as H3 reads latitude.
    ups: Vec<DVec3>,
    /// Grid indices by ancestor at `bucket_resolution`.
    buckets: HashMap<CellIndex, Vec<u32>>,
    bucket_resolution: Resolution,
}

impl Grid {
    /// Cells whose centres lie in `region` (all when `None`); error past `max_cells`.
    pub fn new(
        resolution: Resolution,
        region: Option<Region>,
        max_cells: usize,
    ) -> Result<Self, String> {
        let too_many =
            || format!("more than {max_cells} cells; lower the resolution or narrow the region");
        if region.is_none() && resolution.cell_count() > max_cells as u64 {
            return Err(too_many());
        }
        let mut cells = Vec::new();
        let mut stack: Vec<CellIndex> = CellIndex::base_cells().collect();
        while let Some(cell) = stack.pop() {
            let centre = LatLng::from(cell);
            let (lat, lon) = (centre.lat_radians(), centre.lng_radians());
            if cell.resolution() == resolution {
                if region.is_none_or(|r| r.contains(lat, lon)) {
                    if cells.len() == max_cells {
                        return Err(too_many());
                    }
                    cells.push(cell);
                }
                continue;
            }
            if let Some(r) = region {
                // Descendants stray past a cell's own outline; twice its radius bounds them.
                let radius = cell
                    .boundary()
                    .iter()
                    .map(|v| centre.distance_rads(*v))
                    .fold(0.0, f64::max);
                if !r.near(lat, lon, 2.0 * radius) {
                    continue;
                }
            }
            stack.extend(cell.children(cell.resolution().succ().expect("below target")));
        }
        cells.sort_unstable();
        let (points, ups) = cells
            .iter()
            .map(|&cell| {
                let c = LatLng::from(cell);
                let (lat, lon) = (c.lat_radians(), c.lng_radians());
                let (sl, cl) = lat.sin_cos();
                (
                    geodetic_to_ecef(lat, lon, 0.0),
                    DVec3::new(cl * lon.cos(), cl * lon.sin(), sl),
                )
            })
            .unzip();
        let bucket_resolution =
            Resolution::try_from(u8::from(resolution).saturating_sub(2)).expect("0–15");
        let mut buckets: HashMap<CellIndex, Vec<u32>> = HashMap::new();
        for (i, &c) in cells.iter().enumerate() {
            let parent = c.parent(bucket_resolution).expect("coarser");
            buckets.entry(parent).or_default().push(i as u32);
        }
        Ok(Self {
            resolution,
            cells,
            points,
            ups,
            buckets,
            bucket_resolution,
        })
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Grid indices of cells whose centres are within about `radius_rad` (central
    /// angle, 0.01 rad slack) of `(lat, lon)` rad.
    fn near(&self, lat: f64, lon: f64, radius_rad: f64, out: &mut Vec<u32>) {
        out.clear();
        let (sl, cl) = lat.sin_cos();
        let centre = DVec3::new(cl * lon.cos(), cl * lon.sin(), sl);
        let min_dot = (radius_rad + 0.01).min(PI).cos();
        let mut take = |indices: &[u32]| {
            out.extend(
                indices
                    .iter()
                    .filter(|&&i| self.ups[i as usize].dot(centre) >= min_dot),
            );
        };
        let edge = self.bucket_resolution.edge_length_rads();
        let k = ((radius_rad + 0.01) / (1.2 * edge)).ceil() + 2.0;
        let Ok(at) = LatLng::from_radians(lat, lon) else {
            return;
        };
        if 3.0 * k * (k + 1.0) + 1.0 >= self.buckets.len() as f64 {
            self.buckets.values().for_each(|b| take(b));
            return;
        }
        let disk: Vec<CellIndex> = at.to_cell(self.bucket_resolution).grid_disk(k as u32);
        for cell in disk {
            if let Some(b) = self.buckets.get(&cell) {
                take(b);
            }
        }
    }
}

// --- Coverage -------------------------------------------------------------------------

/// One observer at one sample. No sensors: line of sight above the minimum elevation.
#[derive(Debug, Clone, Copy)]
pub struct Observer<'a> {
    pub position_itrf_km: DVec3,
    pub body_to_itrf: DQuat,
    pub sensors: &'a [Sensor],
}

/// Per-cell statistics over the sampled span.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CellCoverage {
    /// Covered samples / samples.
    pub fraction: f64,
    /// Runs of covered samples.
    pub accesses: u32,
    /// Longest uncovered span, leading and trailing included; the span if never covered.
    pub max_gap_s: f64,
    /// Mean of the gaps between accesses; `None` below two accesses.
    pub mean_gap_s: Option<f64>,
    /// From the first sample to the first covered one.
    pub first_access_s: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Tally {
    covered: u32,
    accesses: u32,
    first: f64,
    last: f64,
    last_sample: u32,
    max_gap: f64,
    gap_sum: f64,
}

/// Accumulates coverage of `grid` sample by sample.
#[derive(Debug)]
pub struct Coverage {
    pub grid: Grid,
    pub min_elevation_rad: f64,
    samples: u32,
    start: Option<f64>,
    end: f64,
    tallies: Vec<Tally>,
    scratch: Vec<u32>,
}

impl Coverage {
    pub fn new(grid: Grid, min_elevation_rad: f64) -> Self {
        let tallies = vec![Tally::default(); grid.len()];
        Self {
            grid,
            min_elevation_rad,
            samples: 0,
            start: None,
            end: 0.0,
            tallies,
            scratch: Vec::new(),
        }
    }

    /// Adds the sample at `t` s (ascending): cells any of `observers` sees.
    pub fn sample(&mut self, t: f64, observers: &[Observer]) {
        self.start.get_or_insert(t);
        self.end = t;
        self.samples += 1;
        let stamp = self.samples;
        let sin_min = self.min_elevation_rad.sin();
        let mut scratch = std::mem::take(&mut self.scratch);
        for o in observers {
            let r = o.position_itrf_km;
            // Ground disks to search: the horizon cap, or each footprint's.
            let mut disks = Vec::new();
            if o.sensors.is_empty() {
                let (lat, lon, _) = ecef_to_geodetic(r);
                let k = (WGS84_B_KM * self.min_elevation_rad.max(0.0).cos() / r.length()).min(1.0);
                disks.push((lat, lon, k.acos() - self.min_elevation_rad.max(0.0)));
            } else {
                for s in o.sensors {
                    let centre = ground_point(r, o.body_to_itrf * s.boresight());
                    let radius = s
                        .boundary(16)
                        .into_iter()
                        .map(|d| central_angle(centre, ground_point(r, o.body_to_itrf * d)))
                        .fold(0.0, f64::max);
                    let (lat, lon, _) = ecef_to_geodetic(centre);
                    disks.push((lat, lon, radius * 1.1));
                }
            }
            let to_body = o.body_to_itrf.inverse();
            for (lat, lon, radius) in disks {
                self.grid.near(lat, lon, radius, &mut scratch);
                for &i in &scratch {
                    let i = i as usize;
                    let tally = &mut self.tallies[i];
                    if tally.covered > 0 && tally.last_sample == stamp {
                        continue;
                    }
                    let p = self.grid.points[i];
                    let rho = r - p;
                    let range = rho.length();
                    if rho.dot(self.grid.ups[i]) < sin_min * range {
                        continue;
                    }
                    if !o.sensors.is_empty() {
                        let d = to_body * (-rho / range);
                        if !o.sensors.iter().any(|s| s.contains(d)) {
                            continue;
                        }
                    }
                    if tally.covered == 0 {
                        tally.first = t;
                        tally.accesses = 1;
                    } else if tally.last_sample + 1 != stamp {
                        let gap = t - tally.last;
                        tally.accesses += 1;
                        tally.gap_sum += gap;
                        tally.max_gap = tally.max_gap.max(gap);
                    }
                    tally.covered += 1;
                    tally.last = t;
                    tally.last_sample = stamp;
                }
            }
        }
        self.scratch = scratch;
    }

    pub fn samples(&self) -> usize {
        self.samples as usize
    }

    /// Statistics per grid cell, in grid order.
    pub fn cells(&self) -> Vec<CellCoverage> {
        let start = self.start.unwrap_or(0.0);
        let span = self.end - start;
        let n = f64::from(self.samples.max(1));
        self.tallies
            .iter()
            .map(|t| {
                if t.covered == 0 {
                    return CellCoverage {
                        max_gap_s: span,
                        ..CellCoverage::default()
                    };
                }
                CellCoverage {
                    fraction: f64::from(t.covered) / n,
                    accesses: t.accesses,
                    max_gap_s: t.max_gap.max(t.first - start).max(self.end - t.last),
                    mean_gap_s: (t.accesses > 1).then(|| t.gap_sum / f64::from(t.accesses - 1)),
                    first_access_s: Some(t.first - start),
                }
            })
            .collect()
    }
}

/// Angle between two geocentric points.
fn central_angle(a: DVec3, b: DVec3) -> f64 {
    a.cross(b).length().atan2(a.dot(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: f64 = PI / 180.0;

    #[test]
    fn fields_of_view_contain_their_boundaries() {
        let cone = Sensor::new(
            DVec3::Z,
            None,
            FieldOfView::Cone {
                half_angle_rad: 10.0 * D,
            },
        )
        .unwrap();
        assert!(cone.contains(DVec3::Z));
        assert!(cone.contains(DVec3::new(0.17, 0.0, 1.0).normalize()));
        assert!(!cone.contains(DVec3::new(0.18, 0.0, 1.0).normalize()));
        let rect = Sensor::new(
            DVec3::Z,
            Some(DVec3::X),
            FieldOfView::Rectangle {
                half_x_rad: 20.0 * D,
                half_y_rad: 5.0 * D,
            },
        )
        .unwrap();
        assert!(rect.contains(DVec3::new((19.0 * D).tan(), 0.0, 1.0)));
        assert!(!rect.contains(DVec3::new(0.0, (6.0 * D).tan(), 1.0)));
        assert!(!rect.contains(-DVec3::Z));
        for s in [cone, rect] {
            let edge = s.boundary(12);
            assert_eq!(edge.len(), 12);
            for d in edge {
                assert!((d.length() - 1.0).abs() < 1e-12);
                assert!(s.contains(d * (1.0 - 1e-9) + s.boresight() * 1e-9));
            }
        }
        assert!(
            Sensor::new(
                DVec3::Z,
                Some(DVec3::Z),
                FieldOfView::Cone {
                    half_angle_rad: 0.1
                }
            )
            .is_none()
        );
        assert!(
            Sensor::new(
                DVec3::Z,
                None,
                FieldOfView::Cone {
                    half_angle_rad: 0.0
                }
            )
            .is_none()
        );
    }

    #[test]
    fn rays_meet_the_ellipsoid_or_its_limb() {
        let origin = DVec3::new(WGS84_A_KM + 500.0, 0.0, 0.0);
        let down = ellipsoid_hit(origin, -DVec3::X).unwrap();
        assert!((down - DVec3::new(WGS84_A_KM, 0.0, 0.0)).length() < 1e-9);
        let pole = ellipsoid_hit(DVec3::new(0.0, 0.0, 9000.0), -DVec3::Z).unwrap();
        assert!((pole.z - WGS84_B_KM).abs() < 1e-9);
        assert!(ellipsoid_hit(origin, DVec3::Y).is_none());
        // Past the limb: the tangent point, at the horizon from the origin.
        let limb = ground_point(origin, DVec3::new(-0.1, 1.0, 0.0));
        assert!((limb.length() - WGS84_A_KM).abs() < 1e-6);
        assert!((origin - limb).dot(limb).abs() < 1e-6 * WGS84_A_KM * WGS84_A_KM);
    }

    #[test]
    fn a_nadir_cone_footprint_is_a_circle() {
        let origin = DVec3::new(WGS84_A_KM + 700.0, 0.0, 0.0);
        let sensor = Sensor::new(
            DVec3::Z,
            None,
            FieldOfView::Cone {
                half_angle_rad: 20.0 * D,
            },
        )
        .unwrap();
        // Body +Z onto −X (nadir).
        let q = DQuat::from_rotation_arc(DVec3::Z, -DVec3::X);
        // Earth-central angle: asin(r sin η / R) − η.
        let eta = 20.0 * D;
        let expected = (origin.length() * eta.sin() / WGS84_A_KM).asin() - eta;
        for d in sensor.boundary(24) {
            let angle = central_angle(DVec3::X, ground_point(origin, q * d));
            assert!(
                (angle - expected).abs() < 1e-3 * expected,
                "{angle} {expected}"
            );
        }
        let ring = footprint(&sensor, origin, q, 24);
        assert_eq!(ring.len(), 24);
        assert!(
            ring.iter()
                .all(|&(lat, lon)| lat.abs() < 2.0 * expected && lon.abs() < 2.0 * expected)
        );
    }

    #[test]
    fn grids_cover_the_globe_or_a_region() {
        let all = Grid::new(Resolution::One, None, 1000).unwrap();
        assert_eq!(all.len(), 842);
        assert!(Grid::new(Resolution::Two, None, 1000).is_err());
        let region = Region {
            west: 170.0 * D,
            south: -30.0 * D,
            east: -170.0 * D,
            north: 10.0 * D,
        };
        let some = Grid::new(Resolution::Three, Some(region), 10_000).unwrap();
        let expected = Grid::new(Resolution::Three, None, 50_000)
            .unwrap()
            .cells
            .into_iter()
            .filter(|&c| {
                let ll = LatLng::from(c);
                region.contains(ll.lat_radians(), ll.lng_radians())
            })
            .count();
        assert_eq!(some.len(), expected);
        assert!(some.len() > 100);
    }

    /// A geostationary observer over (0°, 0°) sees the cells within its horizon cap
    /// at every sample; a nadir cone sees a smaller cap.
    #[test]
    fn coverage_counts_samples_accesses_and_gaps() {
        let r = DVec3::new(42_164.0, 0.0, 0.0);
        let grid = Grid::new(Resolution::One, None, 1000).unwrap();
        let mut horizon = Coverage::new(grid.clone(), 10.0 * D);
        let observer = Observer {
            position_itrf_km: r,
            body_to_itrf: DQuat::IDENTITY,
            sensors: &[],
        };
        for t in 0..10 {
            horizon.sample(f64::from(t) * 60.0, &[observer]);
        }
        let cells = horizon.cells();
        let covered = cells.iter().filter(|c| c.fraction == 1.0).count();
        assert_eq!(cells.iter().filter(|c| c.fraction > 0.0).count(), covered);
        // Cap half-angle 71.4° at 10°: 1 − cos ≈ 0.68 of a hemisphere, ≈ 34 % of cells.
        let share = covered as f64 / cells.len() as f64;
        assert!((share - 0.34).abs() < 0.03, "{share}");
        let seen = cells.iter().find(|c| c.fraction == 1.0).unwrap();
        assert_eq!(
            (seen.accesses, seen.max_gap_s, seen.first_access_s),
            (1, 0.0, Some(0.0))
        );
        assert_eq!(
            cells.iter().find(|c| c.fraction == 0.0).unwrap().max_gap_s,
            540.0
        );

        let sensor = Sensor::new(
            DVec3::Z,
            None,
            FieldOfView::Cone {
                half_angle_rad: 1.0 * D,
            },
        )
        .unwrap();
        let mut cone = Coverage::new(Grid::new(Resolution::Two, None, 10_000).unwrap(), 0.0);
        let q = DQuat::from_rotation_arc(DVec3::Z, -DVec3::X);
        let sensors = [sensor];
        // Seen at 0 and 3: one interior gap of 180 s, trailing 360 s.
        for t in 0..10 {
            let pointing = if t == 0 || t == 3 {
                q
            } else {
                DQuat::from_rotation_arc(DVec3::Z, DVec3::X)
            };
            cone.sample(
                f64::from(t) * 60.0,
                &[Observer {
                    position_itrf_km: r,
                    body_to_itrf: pointing,
                    sensors: &sensors,
                }],
            );
        }
        let cells = cone.cells();
        let seen: Vec<_> = cells.iter().filter(|c| c.fraction > 0.0).collect();
        // Cap ≈ 5.6°: ≈ 1.2e6 km², ≈ 14 cells of 8.7e4 km².
        assert!((8..25).contains(&seen.len()), "{}", seen.len());
        for c in seen {
            assert_eq!(c.accesses, 2);
            assert_eq!(c.mean_gap_s, Some(180.0));
            assert_eq!(c.max_gap_s, 360.0);
            assert!((c.fraction - 0.2).abs() < 1e-12);
        }
    }
}
