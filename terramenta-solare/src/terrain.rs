//! Digital elevation models and terrain horizon masks.
//!
//! Heights are metres above the DEM's vertical datum (Terrarium: EGM96 geoid). Masks treat
//! them as ellipsoidal: the geoid–ellipsoid separation is near constant over the ranges
//! involved and cancels to first order. Elevations are geodetic, as
//! [`crate::events::GroundStation::look_angles`].

use std::f64::consts::{PI, TAU};

use glam::DVec3;

use crate::earth::{WGS84_A_KM, WGS84_F, geodetic_to_ecef};

/// Height at a geodetic point.
pub trait Terrain {
    /// Metres; `None` outside coverage or on a void.
    fn height_m(&self, lat_rad: f64, lon_rad: f64) -> Option<f64>;
}

/// Mosaic of square Web Mercator (EPSG:3857, XYZ) tiles at one zoom, bilinear.
#[derive(Debug, Clone)]
pub struct MercatorTiles {
    pub zoom: u8,
    /// Tile column/row of the north-west tile; columns wrap at the antimeridian.
    pub x0: u32,
    pub y0: u32,
    pub columns: u32,
    pub rows: u32,
    /// Pixels per tile side.
    pub tile_px: u32,
    /// Row-major mosaic, `columns·tile_px` wide, north to south; NaN for voids.
    pub heights_m: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainError {
    /// Pixel buffer length is not `4·width·height`.
    Size {
        expected: usize,
        got: usize,
    },
    Empty,
}

impl std::fmt::Display for TerrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Size { expected, got } => write!(f, "expected {expected} RGBA bytes, got {got}"),
            Self::Empty => write!(f, "empty mosaic"),
        }
    }
}

impl std::error::Error for TerrainError {}

impl MercatorTiles {
    /// Terrarium encoding (Mapzen/AWS Terrain Tiles): `h = 256 R + G + B/256 − 32768` m.
    /// Alpha 0 is a void.
    pub fn from_terrarium_rgba(
        zoom: u8,
        (x0, y0): (u32, u32),
        (columns, rows): (u32, u32),
        tile_px: u32,
        rgba: &[u8],
    ) -> Result<Self, TerrainError> {
        let n = (columns * tile_px) as usize * (rows * tile_px) as usize;
        if n == 0 {
            return Err(TerrainError::Empty);
        }
        if rgba.len() != 4 * n {
            return Err(TerrainError::Size {
                expected: 4 * n,
                got: rgba.len(),
            });
        }
        let heights_m = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                if p[3] == 0 {
                    f32::NAN
                } else {
                    (p[0] as f32 * 256.0 + p[1] as f32 + p[2] as f32 / 256.0) - 32768.0
                }
            })
            .collect();
        Ok(Self {
            zoom,
            x0,
            y0,
            columns,
            rows,
            tile_px,
            heights_m,
        })
    }

    fn width(&self) -> usize {
        (self.columns * self.tile_px) as usize
    }

    fn height(&self) -> usize {
        (self.rows * self.tile_px) as usize
    }

    /// Global pixel coordinates (pixel centres at `+0.5`) at this zoom.
    fn global_px(&self, lat: f64, lon: f64) -> (f64, f64) {
        let size = (1u64 << self.zoom) as f64 * self.tile_px as f64;
        let x = (lon / TAU + 0.5).rem_euclid(1.0) * size;
        let lat = lat.clamp(-1.484_422_229_745_332, 1.484_422_229_745_332); // ±85.0511°
        let y = (0.5 - (PI / 4.0 + lat / 2.0).tan().ln() / TAU) * size;
        (x, y)
    }

    fn sample(&self, col: i64, row: i64) -> Option<f64> {
        let world = (1i64 << self.zoom) * self.tile_px as i64;
        let dx = (col - (self.x0 * self.tile_px) as i64).rem_euclid(world);
        let dy = row - (self.y0 * self.tile_px) as i64;
        if dx >= self.width() as i64 || !(0..self.height() as i64).contains(&dy) {
            return None;
        }
        let h = self.heights_m[dy as usize * self.width() + dx as usize];
        (!h.is_nan()).then_some(h as f64)
    }

    /// Ground size of one pixel at `lat`, m.
    pub fn pixel_m(&self, lat_rad: f64) -> f64 {
        TAU * WGS84_A_KM * 1e3 * lat_rad.cos() / ((1u64 << self.zoom) as f64 * self.tile_px as f64)
    }
}

impl Terrain for MercatorTiles {
    fn height_m(&self, lat_rad: f64, lon_rad: f64) -> Option<f64> {
        let (x, y) = self.global_px(lat_rad, lon_rad);
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (c, r) = (fx.floor() as i64, fy.floor() as i64);
        let (tx, ty) = (fx - c as f64, fy - r as f64);
        let h00 = self.sample(c, r)?;
        let h10 = self.sample(c + 1, r).unwrap_or(h00);
        let h01 = self.sample(c, r + 1).unwrap_or(h00);
        let h11 = self.sample(c + 1, r + 1).unwrap_or(h10);
        Some((h00 * (1.0 - tx) + h10 * tx) * (1.0 - ty) + (h01 * (1.0 - tx) + h11 * tx) * ty)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HorizonOptions {
    pub azimuth_step_rad: f64,
    pub max_range_km: f64,
    /// Ray sample spacing, km; ~ the DEM resolution.
    pub step_km: f64,
    /// Eye above the ground at the station, m.
    pub observer_height_m: f64,
    /// Refraction coefficient k (ray curvature / Earth curvature): adds `k·d / 2R` to
    /// each elevation. 0 geometric; 0.13 optical; 0.25 radio (4/3 Earth).
    pub refraction: f64,
}

impl Default for HorizonOptions {
    fn default() -> Self {
        Self {
            azimuth_step_rad: 1f64.to_radians(),
            max_range_km: 50.0,
            step_km: 0.1,
            observer_height_m: 2.0,
            refraction: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Horizon {
    /// DEM height at the station, m.
    pub ground_m: f64,
    /// `(azimuth, elevation)`, rad, azimuth ascending in `[0, 2π)`: the
    /// [`crate::events::GroundStation::horizon`] form.
    pub mask: Vec<(f64, f64)>,
}

/// Highest terrain elevation angle per azimuth from `(lat, lon)`, rays to
/// `max_range_km`. `None` if the DEM has no height at the station. Samples off the DEM
/// end their ray.
pub fn horizon_mask(
    terrain: &impl Terrain,
    lat_rad: f64,
    lon_rad: f64,
    options: &HorizonOptions,
) -> Option<Horizon> {
    let ground_m = terrain.height_m(lat_rad, lon_rad)?;
    let eye = geodetic_to_ecef(
        lat_rad,
        lon_rad,
        (ground_m + options.observer_height_m) * 1e-3,
    );
    let (sl, cl) = lat_rad.sin_cos();
    let (so, co) = lon_rad.sin_cos();
    let up = DVec3::new(cl * co, cl * so, sl);
    // Gaussian mean radius at the station.
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let radius = WGS84_A_KM * (1.0 - e2).sqrt() / (1.0 - e2 * sl * sl);
    let n_az = (TAU / options.azimuth_step_rad).round().max(4.0) as usize;
    let n_step = (options.max_range_km / options.step_km).ceil().max(1.0) as usize;
    let mask = (0..n_az)
        .map(|i| {
            let az = i as f64 * TAU / n_az as f64;
            let (sa, ca) = az.sin_cos();
            let mut best = -PI / 2.0;
            for k in 1..=n_step {
                let d = (k as f64 * options.step_km).min(options.max_range_km);
                // Spherical direct problem.
                let s = d / radius;
                let (ss, cs) = s.sin_cos();
                let lat = (sl * cs + cl * ss * ca).asin();
                let lon = lon_rad + (sa * ss * cl).atan2(cs - sl * lat.sin());
                let Some(h) = terrain.height_m(lat, lon) else {
                    break;
                };
                let los = geodetic_to_ecef(lat, lon, h * 1e-3) - eye;
                let el = (up.dot(los) / los.length()).asin() + options.refraction * s / 2.0;
                best = best.max(el);
            }
            (az, best)
        })
        .collect();
    Some(Horizon { ground_m, mask })
}

/// XYZ tile range `(x0, y0, columns, rows)` covering `range_km` around a point, at `zoom`.
/// Columns may wrap past `2^zoom`.
pub fn tile_range(lat_rad: f64, lon_rad: f64, range_km: f64, zoom: u8) -> (u32, u32, u32, u32) {
    let n = (1u64 << zoom) as f64;
    let dlat = range_km / (WGS84_A_KM * (1.0 - WGS84_F));
    let dlon = (range_km / (WGS84_A_KM * lat_rad.cos().max(1e-6))).min(PI);
    let x = |lon: f64| ((lon / TAU + 0.5) * n).floor();
    let y = |lat: f64| {
        let lat = lat.clamp(-1.484_422_229_745_332, 1.484_422_229_745_332);
        ((0.5 - (PI / 4.0 + lat / 2.0).tan().ln() / TAU) * n)
            .floor()
            .clamp(0.0, n - 1.0)
    };
    let (west, east) = (x(lon_rad - dlon), x(lon_rad + dlon));
    let (north, south) = (y(lat_rad + dlat), y(lat_rad - dlat));
    let columns = ((east - west + 1.0) as u32).min(1 << zoom);
    (
        west.rem_euclid(n) as u32,
        north as u32,
        columns,
        (south - north + 1.0) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Analytic terrain.
    struct Field<F: Fn(f64, f64) -> Option<f64>>(F);

    impl<F: Fn(f64, f64) -> Option<f64>> Terrain for Field<F> {
        fn height_m(&self, lat: f64, lon: f64) -> Option<f64> {
            (self.0)(lat, lon)
        }
    }

    #[test]
    fn terrarium_decode_and_bilinear() {
        // 1 × 1 tile of 2 px at zoom 0: heights 0, 100 / 200, 300.
        let enc = |h: f64| {
            let v = h + 32768.0;
            [(v / 256.0).floor() as u8, (v % 256.0) as u8, 0, 255]
        };
        let rgba: Vec<u8> = [0.0, 100.0, 200.0, 300.0]
            .into_iter()
            .flat_map(enc)
            .collect();
        let t = MercatorTiles::from_terrarium_rgba(0, (0, 0), (1, 1), 2, &rgba).unwrap();
        assert_eq!(t.heights_m, vec![0.0, 100.0, 200.0, 300.0]);
        // Map centre is the corner shared by all four pixels.
        assert!((t.height_m(0.0, 0.0).unwrap() - 150.0).abs() < 1e-9);
        // Pixel centre (−90°, 66.5°N) is exact.
        let lat = (2.0 * (0.5 * PI).exp().atan()) - PI / 2.0;
        assert!((t.height_m(lat, -PI / 2.0).unwrap()).abs() < 1e-6);
        assert!(MercatorTiles::from_terrarium_rgba(0, (0, 0), (1, 1), 2, &rgba[..8]).is_err());
    }

    #[test]
    fn flat_terrain_dips_below_horizon() {
        let flat = Field(|_, _| Some(0.0));
        let o = HorizonOptions {
            observer_height_m: 100.0,
            max_range_km: 100.0,
            azimuth_step_rad: 30f64.to_radians(),
            ..Default::default()
        };
        let h = horizon_mask(&flat, 0.7, 0.3, &o).unwrap();
        assert_eq!(h.mask.len(), 12);
        // Dip −√(2h/R) ≈ −0.32° at 100 m, from the sample nearest the tangent (≈ 36 km).
        for &(_, el) in &h.mask {
            let deg = el.to_degrees();
            assert!((-0.33..-0.30).contains(&deg), "{deg}");
        }
    }

    #[test]
    fn ridge_east_raises_mask() {
        // 1 km ridge 10 km east of (0, 0), 1 km wide.
        let ridge_lon = 10.0 / WGS84_A_KM;
        let ridge = Field(move |_, lon: f64| {
            Some(if (lon - ridge_lon).abs() * WGS84_A_KM < 0.5 {
                1000.0
            } else {
                0.0
            })
        });
        let o = HorizonOptions {
            observer_height_m: 0.0,
            step_km: 0.05,
            ..Default::default()
        };
        let h = horizon_mask(&ridge, 0.0, 0.0, &o).unwrap();
        let at = |deg: usize| h.mask[deg].1.to_degrees();
        // atan(1 / 9.5) less curvature drop.
        let expect = (1.0f64 / 9.5).atan().to_degrees() - (9.5 / (2.0 * WGS84_A_KM)).to_degrees();
        assert!((at(90) - expect).abs() < 0.05, "{} vs {expect}", at(90));
        assert!(at(270) < 0.0 && at(0) < 0.0);
        let refracted = horizon_mask(
            &ridge,
            0.0,
            0.0,
            &HorizonOptions {
                refraction: 0.25,
                ..o
            },
        )
        .unwrap();
        assert!(refracted.mask[90].1 > h.mask[90].1);
    }

    #[test]
    fn voids_end_rays_and_station_needs_height() {
        let island = Field(|lat: f64, _| (lat.abs() < 0.001).then_some(0.0));
        assert!(horizon_mask(&island, 0.01, 0.0, &HorizonOptions::default()).is_none());
        let h = horizon_mask(&island, 0.0, 0.0, &HorizonOptions::default()).unwrap();
        assert!(h.mask.iter().all(|&(_, el)| el <= 0.0));
    }

    #[test]
    fn tile_range_covers_and_wraps() {
        let (x0, y0, c, r) = tile_range(0.0, 0.0, 10.0, 10);
        assert_eq!((x0, c), (511, 2));
        assert_eq!((y0, r), (511, 2));
        // Antimeridian: starts east of 180° W wrap.
        let (x0, _, c, _) = tile_range(0.0, PI - 1e-4, 50.0, 8);
        assert!(x0 + c > 256);
        // Contains the station tile.
        let (lat, lon) = (0.8f64, -2.0f64);
        let (x0, y0, c, r) = tile_range(lat, lon, 30.0, 11);
        let t = MercatorTiles {
            zoom: 11,
            x0,
            y0,
            columns: c,
            rows: r,
            tile_px: 1,
            heights_m: vec![0.0; (c * r) as usize],
        };
        assert!(t.height_m(lat, lon).is_some());
    }
}
