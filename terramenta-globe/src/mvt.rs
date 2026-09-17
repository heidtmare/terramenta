//! Mapbox Vector Tiles: one `.mvt` tile's geometry, unprojected back onto the
//! globe.
//!
//! An MVT tile is a protobuf holding several named *source layers* — `water`,
//! `boundary`, `transportation` — each a list of features whose geometry is
//! written in integer coordinates local to the tile, running `0..extent` across
//! it with `y` pointing south. The protobuf is decoded by [`geozero`], whose
//! reader walks a layer and calls back for every ring, strand and point; what
//! this module adds is everything either side of that walk.
//!
//! **Where the projection happens.** The coordinates that come out of the
//! decoder are in the tile's own square, and that square is a square of the Web
//! Mercator (EPSG:3857) grid. [`unproject`] takes them back to WGS 84 latitude
//! and longitude, which is the one coordinate system the rest of the globe
//! speaks — the camera, the imagery and the GeoJSON overlays all address the
//! sphere in degrees. So the result of decoding a tile is a [`GeoJson`], the
//! same flattened lists of points, lines and rings a GeoJSON document collapses
//! into, and from there [`crate::vector_tiles`] builds vertex buffers out of it
//! with exactly the mesh builders an overlay uses.
//!
//! Worth being precise about what "WGS 84" means here, because the two halves
//! of the round trip do not use the same Earth. Web Mercator projects *geodetic*
//! WGS 84 latitudes through *spherical* Mercator formulas — that mismatch is
//! the standard's own, and every tile that has ever been cut assumes it — so
//! [`unproject`] inverts the spherical formulas and hands back a geodetic
//! coordinate, which is what the datum a tile's data was surveyed in calls the
//! place. Where that coordinate is finally *drawn* is a separate question, and
//! the answer is the same sphere everything else here is drawn on: the globe is
//! one radius in every direction, so a vector tile and the imagery under it land
//! on the same surface. Giving this module an ellipsoid of its own would put its
//! lines up to twenty kilometres off the imagery they annotate.
//!
//! **Clipping is not optional, and a ring is clipped twice.** Tiles are cut
//! with a buffer, so a road that leaves the tile is carried some way past the
//! edge and the neighbouring tile carries the same stretch back the other way.
//! Drawn as they arrive, every seam in the world gets two copies of everything
//! crossing it — which on alpha-blended lines is a visible ladder of darker
//! rungs, and on fills a darker frame around every tile. So geometry is clipped
//! to the tile's own square before it leaves here, in tile coordinates, where
//! the square is exact.
//!
//! A ring cannot be clipped once, though, because the two things drawn from it
//! want opposite answers. A *fill* wants the ring closed against the tile edge,
//! so the piece of Brazil in this tile and the piece in the next meet along the
//! seam with no gap and no overlap. An *outline* wants the opposite: the tile
//! edge is not a coastline, and closing the ring against it would draw the
//! grid. So [`decode`] clips every ring both ways — as a ring for
//! [`GeoJson::polygons`], and as an open path for [`GeoJson::lines`], which is
//! where the parts of it that are really a boundary end up. A consumer draws
//! the fills from the first and *all* of its lines from the second; it must not
//! also outline the polygons, or every border would be drawn twice.

use geozero::mvt::{Message, Tile, tile};
use geozero::{ColumnValue, FeatureProcessor, GeomProcessor, PropertyProcessor};
use serde_json::{Map, Value};

use crate::geo::{GeoBounds, LatLon, Position};
use crate::geojson::{Feature, GeoJson, Polygon, Shape};
use crate::tiles::TileId;

/// The extent a layer is assumed to use when it does not say — which is the
/// value the specification recommends and very nearly everything writes.
const DEFAULT_EXTENT: u32 = 4096;

/// The property the source layer's name is reported under.
///
/// Every other property is passed through exactly as the tile wrote it, the way
/// [`crate::geojson`] passes a feature's `properties` through. This one is
/// added, because which layer a feature came from is the thing that tells a
/// coastline from a motorway and the tile does not record it per feature. The
/// name is the one the Mapbox style specification uses for the same idea, and
/// a tile that already had a property under it is overwritten: two answers to
/// "which layer is this" would be worse than one.
pub const SOURCE_LAYER_PROPERTY: &str = "sourceLayer";

/// The northern and southern edge of the Web Mercator grid.
///
/// Mercator sends the poles to infinity, so the square grid has to stop
/// somewhere, and the convention every tiling scheme follows is to stop where
/// the world is exactly square — at the latitude whose projected distance from
/// the equator equals half the world's width. Nothing above it is in any tile,
/// which is why a Mercator basemap has no Arctic Ocean.
pub const MAX_LATITUDE: f32 = 85.051_13;

// ---------------------------------------------------------------------------
// The Web Mercator grid
// ---------------------------------------------------------------------------

/// How many tiles across the grid is at a level. Level 0 is the single tile
/// holding the whole world, and every level below quarters it.
pub fn matrix_size(level: u8) -> u32 {
    1u32 << level
}

/// The latitude and longitude rectangle a tile covers.
///
/// Mercator rows are not evenly spaced in latitude — a tile at the equator is a
/// few degrees tall where one at 80° is a fraction of that — so the edges come
/// from [`unproject`] rather than from arithmetic on the span.
pub fn tile_bounds(tile: TileId) -> GeoBounds {
    let size = f64::from(matrix_size(tile.level));
    let north_west = unproject(f64::from(tile.x) / size, f64::from(tile.y) / size);
    let south_east = unproject(f64::from(tile.x + 1) / size, f64::from(tile.y + 1) / size);
    GeoBounds {
        lat_min: south_east.lat,
        lat_max: north_west.lat,
        lon_min: north_west.lon,
        lon_max: south_east.lon,
    }
}

/// Turns a position on the Web Mercator world square — both axes running `0..1`,
/// `x` east from the antimeridian and `y` *south* from the northern edge — into
/// the coordinate it stands for.
///
/// The inverse of the projection every vector tile is cut in: longitude is a
/// plain linear stretch, and latitude comes back through the Gudermannian
/// function, which is what undoes Mercator's vertical exaggeration.
pub fn unproject(world_x: f64, world_y: f64) -> LatLon {
    let longitude = world_x * 360.0 - 180.0;
    let latitude = (std::f64::consts::PI * (1.0 - 2.0 * world_y))
        .sinh()
        .atan()
        .to_degrees();
    LatLon::new(latitude as f32, longitude as f32)
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Why a tile could not be read.
#[derive(Debug)]
pub enum MvtError {
    /// The bytes are a gzip stream rather than a protobuf.
    ///
    /// Worth its own case because it is the one failure that is nobody's bug.
    /// A great many tile services answer `Content-Encoding: gzip`, which a
    /// browser unwraps before the bytes ever reach here and a plain HTTP client
    /// may not — so the same URL decodes on the web and arrives compressed on
    /// the desktop, and "invalid wire type" would be a thoroughly misleading
    /// thing to report about it.
    Gzipped,
    /// Not a vector tile, or one whose protobuf is damaged.
    Malformed(String),
    /// A geometry whose command sequence does not follow the specification —
    /// a ring that never closes, a strand that starts with a `LineTo`.
    Geometry(String),
}

impl std::fmt::Display for MvtError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gzipped => write!(
                formatter,
                "the tile arrived gzipped and nothing here unwraps it; \
                 the service has to be asked for identity encoding"
            ),
            Self::Malformed(error) => write!(formatter, "not a vector tile: {error}"),
            Self::Geometry(error) => write!(formatter, "unreadable tile geometry: {error}"),
        }
    }
}

impl std::error::Error for MvtError {}

/// Decodes one tile, keeping the source layers `wanted` names — or all of them
/// when it is empty.
///
/// The result is in the same shape a GeoJSON document parses into, with every
/// coordinate already back in degrees, so everything downstream of here can
/// treat a vector tile and an overlay alike. With one difference, and it
/// matters: a polygon's outline is in [`GeoJson::lines`] rather than implied by
/// its rings, because clipping gives a ring edges that are not boundaries. See
/// the module docs.
pub fn decode(bytes: &[u8], tile: TileId, wanted: &[String]) -> Result<GeoJson, MvtError> {
    // Two bytes, and the difference between a clear report and a puzzle.
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Err(MvtError::Gzipped);
    }

    let decoded = Tile::decode(bytes).map_err(|error| MvtError::Malformed(error.to_string()))?;

    let mut collected = GeoJson::default();
    for layer in &decoded.layers {
        if !wanted.is_empty() && !wanted.iter().any(|name| name == &layer.name) {
            continue;
        }
        let mut decoder = LayerDecoder::new(tile, layer, &mut collected);
        geozero::mvt::process(layer, &mut decoder)
            .map_err(|error| MvtError::Geometry(error.to_string()))?;
    }
    Ok(collected)
}

/// Walks one source layer, turning what the reader hands back into shapes.
///
/// Geometry is accumulated in the tile's own integer coordinates and only
/// converted once a whole line or ring is in hand, because clipping is what
/// happens in between and clipping is exact in the square and awkward on the
/// sphere.
struct LayerDecoder<'a> {
    /// Where the tile sits, as the numbers that turn a local coordinate into a
    /// position on the world square: the tile's own corner, and how much of the
    /// square one unit of the tile covers.
    origin_x: f64,
    origin_y: f64,
    unit: f64,
    /// The tile's width in its own coordinates, which is also the clip square.
    extent: f64,
    layer_name: String,
    /// Feature ids, read from the layer up front: the reader identifies a
    /// feature by its position and does not pass the `id` member along.
    ids: Vec<Option<String>>,
    /// The feature being read, held back until it has geometry worth keeping —
    /// a feature clipped away entirely should not leave a row behind it.
    pending: Option<Feature>,
    /// Its index once it has been kept.
    owner: Option<usize>,
    /// The coordinates of the ring, strand or point group being read.
    current: Vec<[f64; 2]>,
    /// The rings of the polygon being read, outer first.
    rings: Vec<Vec<Position>>,
    /// How deep inside a polygon the walk is, which is what tells a ring from a
    /// line: the reader announces both as linestrings.
    polygon_depth: u32,
    into: &'a mut GeoJson,
}

impl<'a> LayerDecoder<'a> {
    fn new(tile: TileId, layer: &tile::Layer, into: &'a mut GeoJson) -> Self {
        let size = f64::from(matrix_size(tile.level));
        let extent = f64::from(layer.extent.unwrap_or(DEFAULT_EXTENT).max(1));
        Self {
            origin_x: f64::from(tile.x) / size,
            origin_y: f64::from(tile.y) / size,
            // One tile coordinate, as a fraction of the whole world square.
            unit: 1.0 / (size * extent),
            extent,
            layer_name: layer.name.clone(),
            ids: layer
                .features
                .iter()
                .map(|feature| feature.id.map(|id| id.to_string()))
                .collect(),
            pending: None,
            owner: None,
            current: Vec::new(),
            rings: Vec::new(),
            polygon_depth: 0,
            into,
        }
    }

    /// Where a tile coordinate is, in degrees.
    fn position(&self, point: [f64; 2]) -> Position {
        Position::surface(unproject(
            self.origin_x + point[0] * self.unit,
            self.origin_y + point[1] * self.unit,
        ))
    }

    /// The index of the feature being read, keeping it on first use.
    ///
    /// A tile holds every feature of every layer it covers, and most of them
    /// are clipped away or dropped; taking the properties along only for the
    /// ones that survive is the difference between a few hundred rows per tile
    /// and a few thousand.
    fn owner(&mut self) -> usize {
        if let Some(owner) = self.owner {
            return owner;
        }
        self.into
            .features
            .push(self.pending.take().unwrap_or_default());
        let owner = self.into.features.len() - 1;
        self.owner = Some(owner);
        owner
    }

    /// Adds one clipped strand, if there is enough of it left to draw.
    fn push_line(&mut self, path: Vec<[f64; 2]>) {
        for run in clip_path(&path, self.extent) {
            if run.len() < 2 {
                continue;
            }
            let line: Vec<Position> = run.iter().map(|point| self.position(*point)).collect();
            let feature = self.owner();
            self.into.lines.push(Shape {
                feature,
                geometry: line,
            });
        }
    }

    /// Adds one ring, clipped both ways: as a ring to fill, and as the parts of
    /// its outline that are really an outline.
    ///
    /// The ring arrives closed, which is what the path clip needs to come back
    /// round the loop; the ring clip opens it itself.
    fn push_ring(&mut self, ring: Vec<[f64; 2]>) {
        self.push_line(ring.clone());

        let clipped = clip_ring(&ring, self.extent);
        if clipped.len() < 3 {
            return;
        }
        self.rings
            .push(clipped.iter().map(|point| self.position(*point)).collect());
    }

    /// Closes off the polygon being read.
    fn finish_polygon(&mut self) {
        let rings = std::mem::take(&mut self.rings);
        // Holes without an outer ring are not holes in anything — the outer
        // ring can be the one clipping removed, when a polygon reaches into the
        // tile only through the buffer.
        if rings.first().is_none_or(Vec::is_empty) {
            return;
        }
        let feature = self.owner();
        self.into.polygons.push(Shape {
            feature,
            geometry: Polygon { rings },
        });
    }
}

impl GeomProcessor for LayerDecoder<'_> {
    fn xy(&mut self, x: f64, y: f64, _idx: usize) -> geozero::error::Result<()> {
        self.current.push([x, y]);
        Ok(())
    }

    fn point_begin(&mut self, _idx: usize) -> geozero::error::Result<()> {
        self.current.clear();
        Ok(())
    }

    fn point_end(&mut self, _idx: usize) -> geozero::error::Result<()> {
        self.multipoint_end(0)
    }

    fn multipoint_begin(&mut self, size: usize, _idx: usize) -> geozero::error::Result<()> {
        self.current.clear();
        self.current.reserve(size);
        Ok(())
    }

    fn multipoint_end(&mut self, _idx: usize) -> geozero::error::Result<()> {
        for point in std::mem::take(&mut self.current) {
            // A point in the buffer belongs to the neighbouring tile, which
            // will draw it in its own right.
            if !within(point, self.extent) {
                continue;
            }
            let position = self.position(point);
            let feature = self.owner();
            self.into.points.push(Shape {
                feature,
                geometry: position,
            });
        }
        Ok(())
    }

    fn linestring_begin(
        &mut self,
        _tagged: bool,
        size: usize,
        _idx: usize,
    ) -> geozero::error::Result<()> {
        self.current.clear();
        self.current.reserve(size);
        Ok(())
    }

    fn linestring_end(&mut self, _tagged: bool, _idx: usize) -> geozero::error::Result<()> {
        let path = std::mem::take(&mut self.current);
        if self.polygon_depth > 0 {
            self.push_ring(path);
        } else {
            self.push_line(path);
        }
        Ok(())
    }

    fn polygon_begin(
        &mut self,
        _tagged: bool,
        size: usize,
        _idx: usize,
    ) -> geozero::error::Result<()> {
        self.polygon_depth += 1;
        self.rings.clear();
        self.rings.reserve(size);
        Ok(())
    }

    fn polygon_end(&mut self, _tagged: bool, _idx: usize) -> geozero::error::Result<()> {
        self.polygon_depth = self.polygon_depth.saturating_sub(1);
        self.finish_polygon();
        Ok(())
    }
}

impl PropertyProcessor for LayerDecoder<'_> {
    fn property(
        &mut self,
        _idx: usize,
        name: &str,
        value: &ColumnValue<'_>,
    ) -> geozero::error::Result<bool> {
        let converted = match value {
            ColumnValue::Bool(value) => Value::Bool(*value),
            ColumnValue::String(value)
            | ColumnValue::Json(value)
            | ColumnValue::DateTime(value) => Value::String((*value).to_string()),
            ColumnValue::Byte(value) => Value::from(*value),
            ColumnValue::UByte(value) => Value::from(*value),
            ColumnValue::Short(value) => Value::from(*value),
            ColumnValue::UShort(value) => Value::from(*value),
            ColumnValue::Int(value) => Value::from(*value),
            ColumnValue::UInt(value) => Value::from(*value),
            ColumnValue::Long(value) => Value::from(*value),
            ColumnValue::ULong(value) => Value::from(*value),
            ColumnValue::Float(value) => Value::from(*value),
            ColumnValue::Double(value) => Value::from(*value),
            // An MVT value is never binary — the wire format has no such type —
            // so this is unreachable rather than lossy.
            ColumnValue::Binary(_) => Value::Null,
        };

        if let Some(feature) = self.pending.as_mut()
            && let Value::Object(properties) = &mut feature.properties
        {
            properties.insert(name.to_string(), converted);
        }
        // `false` would stop the walk; every property is wanted.
        Ok(true)
    }
}

impl FeatureProcessor for LayerDecoder<'_> {
    fn feature_begin(&mut self, idx: u64) -> geozero::error::Result<()> {
        let mut properties = Map::new();
        properties.insert(
            SOURCE_LAYER_PROPERTY.to_string(),
            Value::String(self.layer_name.clone()),
        );
        self.pending = Some(Feature {
            id: self.ids.get(idx as usize).cloned().flatten(),
            properties: Value::Object(properties),
        });
        self.owner = None;
        self.rings.clear();
        self.current.clear();
        self.polygon_depth = 0;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Clipping
// ---------------------------------------------------------------------------

/// Whether a coordinate is inside the tile rather than out in its buffer.
fn within(point: [f64; 2], extent: f64) -> bool {
    point[0] >= 0.0 && point[0] <= extent && point[1] >= 0.0 && point[1] <= extent
}

/// Clips a path to the tile square, returning the runs of it that are inside.
///
/// Liang–Barsky, segment by segment: each segment is trimmed to the part of it
/// within the square, and a trimmed segment that begins where the last one
/// ended continues the same run rather than starting another — so a road that
/// stays inside comes back as one strand, and one that leaves and returns comes
/// back as two.
fn clip_path(path: &[[f64; 2]], extent: f64) -> Vec<Vec<[f64; 2]>> {
    let mut runs: Vec<Vec<[f64; 2]>> = Vec::new();

    for segment in path.windows(2) {
        let Some((from, to)) = clip_segment(segment[0], segment[1], extent) else {
            continue;
        };
        match runs.last_mut() {
            Some(run) if run.last().is_some_and(|last| *last == from) => run.push(to),
            _ => runs.push(vec![from, to]),
        }
    }

    runs
}

/// The part of one segment inside the square, or `None` when none of it is.
fn clip_segment(from: [f64; 2], to: [f64; 2], extent: f64) -> Option<([f64; 2], [f64; 2])> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    // How much of the segment is left, as the fraction along it that the
    // surviving part runs between.
    let (mut enter, mut exit) = (0.0_f64, 1.0_f64);

    // Each edge of the square as "this much of the parameter, against this
    // much of the distance to the edge".
    for (direction, distance) in [
        (-dx, from[0]),
        (dx, extent - from[0]),
        (-dy, from[1]),
        (dy, extent - from[1]),
    ] {
        if direction == 0.0 {
            // Parallel to this edge: outside it is outside for good.
            if distance < 0.0 {
                return None;
            }
            continue;
        }
        let crossing = distance / direction;
        if direction < 0.0 {
            enter = enter.max(crossing);
        } else {
            exit = exit.min(crossing);
        }
        if enter > exit {
            return None;
        }
    }

    let along = |fraction: f64| [from[0] + dx * fraction, from[1] + dy * fraction];
    Some((along(enter), along(exit)))
}

/// Clips a ring to the tile square.
///
/// Sutherland–Hodgman: the ring is passed through each of the four edges in
/// turn, keeping what is inside and adding a corner wherever it crosses. The
/// square is convex, which is the condition that makes this correct — the
/// result is one ring, closed, following the tile's edge wherever the original
/// ran outside it, which is exactly what makes two neighbouring tiles' fills
/// meet along the seam without overlapping.
///
/// The ring arrives closed, as the tile wrote it, and leaves open, which is how
/// [`crate::geojson::Polygon`] keeps rings.
fn clip_ring(ring: &[[f64; 2]], extent: f64) -> Vec<[f64; 2]> {
    /// Which side of one edge a point is on, positive inside.
    fn inside(point: [f64; 2], edge: usize, extent: f64) -> f64 {
        match edge {
            0 => point[0],
            1 => extent - point[0],
            2 => point[1],
            _ => extent - point[1],
        }
    }

    let mut current: Vec<[f64; 2]> = ring.to_vec();
    // A closed ring repeats its first corner; the clip walks corner to corner
    // and would otherwise see a zero-length edge.
    if current.len() > 1 && current.first() == current.last() {
        current.pop();
    }

    for edge in 0..4 {
        if current.is_empty() {
            return Vec::new();
        }
        let mut clipped = Vec::with_capacity(current.len() + 4);
        for index in 0..current.len() {
            let from = current[index];
            let to = current[(index + 1) % current.len()];
            let (from_side, to_side) = (inside(from, edge, extent), inside(to, edge, extent));

            if from_side >= 0.0 {
                clipped.push(from);
            }
            // Only a crossing adds a corner, and the two sides having different
            // signs is what a crossing is.
            if (from_side >= 0.0) != (to_side >= 0.0) {
                let span = from_side - to_side;
                if span != 0.0 {
                    let fraction = from_side / span;
                    clipped.push([
                        from[0] + (to[0] - from[0]) * fraction,
                        from[1] + (to[1] - from[1]) * fraction,
                    ]);
                }
            }
        }
        current = clipped;
    }

    current
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tile the whole world is in.
    const ROOT: TileId = TileId {
        level: 0,
        x: 0,
        y: 0,
    };

    #[test]
    fn the_root_tile_is_the_whole_mercator_world() {
        let bounds = tile_bounds(ROOT);
        assert!((bounds.lon_min - -180.0).abs() < 1.0e-3);
        assert!((bounds.lon_max - 180.0).abs() < 1.0e-3);
        // Not the poles: Mercator stops where the world is square.
        assert!((bounds.lat_max - MAX_LATITUDE).abs() < 1.0e-3);
        assert!((bounds.lat_min - -MAX_LATITUDE).abs() < 1.0e-3);
    }

    #[test]
    fn the_grid_runs_north_to_south_and_west_to_east() {
        // Row zero of level one is the northern half, column zero the western.
        let north_west = tile_bounds(TileId {
            level: 1,
            x: 0,
            y: 0,
        });
        let south_east = tile_bounds(TileId {
            level: 1,
            x: 1,
            y: 1,
        });
        assert!(north_west.lat_min.abs() < 1.0e-4);
        assert!(north_west.lon_max.abs() < 1.0e-4);
        assert!(south_east.lat_max.abs() < 1.0e-4);
        assert!(south_east.lon_min.abs() < 1.0e-4);
        assert!(north_west.lat_max > south_east.lat_max);
    }

    #[test]
    fn mercator_rows_are_not_evenly_spaced_in_latitude() {
        // Every row is the same height on the projected square, and Mercator
        // stretches the high latitudes to fit — so the same height buys far
        // more degrees at the equator than near the edge. That is the whole
        // character of the projection, and the reason the bounds come from
        // `unproject` rather than from arithmetic on the span.
        let equatorial = tile_bounds(TileId {
            level: 2,
            x: 0,
            y: 2,
        });
        let polar = tile_bounds(TileId {
            level: 2,
            x: 0,
            y: 3,
        });
        assert!(equatorial.lat_span() > polar.lat_span() * 2.0);
        // And the two together reach the edge of the grid rather than the pole.
        assert!((polar.lat_min - -MAX_LATITUDE).abs() < 1.0e-3);
    }

    #[test]
    fn a_path_across_the_buffer_is_cut_at_the_edge() {
        // Enters from the west, crosses the tile, leaves to the east.
        let runs = clip_path(&[[-500.0, 2048.0], [4596.0, 2048.0]], 4096.0);
        assert_eq!(runs.len(), 1);
        // The crossings are solved for rather than landed on, so they are
        // where the edge is to within rounding rather than exactly on it.
        let at = |point: [f64; 2], x: f64| (point[0] - x).abs() < 1.0e-6 && point[1] == 2048.0;
        assert!(at(runs[0][0], 0.0));
        assert!(at(runs[0][1], 4096.0));
    }

    #[test]
    fn a_path_that_leaves_and_returns_comes_back_in_two_strands() {
        let runs = clip_path(
            &[
                [1000.0, 1000.0],
                [-1000.0, 1000.0],
                [-1000.0, 3000.0],
                [1000.0, 3000.0],
            ],
            4096.0,
        );
        assert_eq!(runs.len(), 2);
        // Neither strand may keep a corner out in the buffer.
        for run in &runs {
            assert!(run.iter().all(|point| within(*point, 4096.0)));
        }
    }

    #[test]
    fn a_path_entirely_in_the_buffer_is_dropped() {
        assert!(clip_path(&[[-300.0, -300.0], [-100.0, -100.0]], 4096.0).is_empty());
    }

    #[test]
    fn a_ring_overhanging_the_tile_is_squared_off_against_it() {
        // A square hanging off the north-west corner.
        let clipped = clip_ring(
            &[
                [-1000.0, -1000.0],
                [2000.0, -1000.0],
                [2000.0, 2000.0],
                [-1000.0, 2000.0],
                [-1000.0, -1000.0],
            ],
            4096.0,
        );
        assert!(clipped.iter().all(|point| within(*point, 4096.0)));
        // The part inside is the 2000x2000 square at the corner, so its corners
        // are the four that square has.
        assert!(clipped.contains(&[0.0, 0.0]));
        assert!(clipped.contains(&[2000.0, 2000.0]));
        // Open, not closed: the repeat the tile wrote is gone.
        assert_ne!(clipped.first(), clipped.last());
    }

    #[test]
    fn a_ring_inside_the_tile_keeps_its_corners() {
        let ring = [
            [100.0, 100.0],
            [900.0, 100.0],
            [900.0, 900.0],
            [100.0, 900.0],
            [100.0, 100.0],
        ];
        let clipped = clip_ring(&ring, 4096.0);
        assert_eq!(clipped.len(), 4);
        assert_eq!(clipped[0], [100.0, 100.0]);
    }

    #[test]
    fn a_ring_entirely_outside_the_tile_is_dropped() {
        let ring = [
            [-900.0, -900.0],
            [-100.0, -900.0],
            [-100.0, -100.0],
            [-900.0, -100.0],
        ];
        assert!(clip_ring(&ring, 4096.0).len() < 3);
    }

    /// One layer holding one feature, built the way the specification's own
    /// worked example does.
    fn layer_with(name: &str, geom_type: tile::GeomType, geometry: Vec<u32>) -> tile::Layer {
        let mut feature = tile::Feature {
            id: Some(7),
            tags: vec![0, 0],
            geometry,
            ..Default::default()
        };
        feature.set_type(geom_type);
        tile::Layer {
            version: 2,
            name: name.to_string(),
            extent: Some(4096),
            keys: vec!["kind".to_string()],
            values: vec![tile::Value {
                string_value: Some("coastline".to_string()),
                ..Default::default()
            }],
            features: vec![feature],
        }
    }

    fn encode(layers: Vec<tile::Layer>) -> Vec<u8> {
        Tile { layers }.encode_to_vec()
    }

    #[test]
    fn a_line_comes_back_in_degrees_on_the_globe() {
        // MoveTo(2048, 2048) then LineTo(+1024, +0): the middle of the tile,
        // running east. At level 0 the tile is the world.
        let bytes = encode(vec![layer_with(
            "boundary",
            tile::GeomType::Linestring,
            vec![9, 4096, 4096, 10, 2048, 0],
        )]);

        let decoded = decode(&bytes, ROOT, &[]).expect("a tile");
        assert_eq!(decoded.lines.len(), 1);
        let line = &decoded.lines[0].geometry;
        assert_eq!(line.len(), 2);
        // Halfway across the world square is the equator on the prime meridian.
        assert!(line[0].lat().abs() < 1.0e-3);
        assert!(line[0].lon().abs() < 1.0e-3);
        // A quarter of the tile further east is a quarter of the world east.
        assert!((line[1].lon() - 90.0).abs() < 1.0e-2);
        assert!(line[1].lat().abs() < 1.0e-3);
    }

    #[test]
    fn a_features_properties_and_layer_come_through() {
        let bytes = encode(vec![layer_with(
            "boundary",
            tile::GeomType::Linestring,
            vec![9, 4096, 4096, 10, 2048, 0],
        )]);

        let decoded = decode(&bytes, ROOT, &[]).expect("a tile");
        let feature = &decoded.features[decoded.lines[0].feature];
        assert_eq!(feature.id.as_deref(), Some("7"));
        assert_eq!(
            feature.properties["kind"],
            Value::String("coastline".into())
        );
        assert_eq!(
            feature.properties[SOURCE_LAYER_PROPERTY],
            Value::String("boundary".into())
        );
    }

    #[test]
    fn only_the_wanted_source_layers_are_kept() {
        let geometry = vec![9, 4096, 4096, 10, 2048, 0];
        let bytes = encode(vec![
            layer_with("boundary", tile::GeomType::Linestring, geometry.clone()),
            layer_with("transportation", tile::GeomType::Linestring, geometry),
        ]);

        assert_eq!(decode(&bytes, ROOT, &[]).expect("a tile").lines.len(), 2);
        let filtered = decode(&bytes, ROOT, &["boundary".to_string()]).expect("a tile");
        assert_eq!(filtered.lines.len(), 1);
        assert_eq!(
            filtered.features[0].properties[SOURCE_LAYER_PROPERTY],
            Value::String("boundary".into())
        );
    }

    #[test]
    fn a_polygon_arrives_as_an_open_ring_and_an_outline_of_its_own() {
        // The specification's own polygon example, scaled up to sit inside the
        // tile: MoveTo, three LineTos, ClosePath.
        let bytes = encode(vec![layer_with(
            "water",
            tile::GeomType::Polygon,
            vec![9, 2048, 2048, 18, 1024, 0, 0, 1024, 15],
        )]);

        let decoded = decode(&bytes, ROOT, &[]).expect("a tile");
        assert_eq!(decoded.polygons.len(), 1);
        // Three corners, with the closing repeat dropped the way every ring
        // reaching the mesh builders has to be.
        assert_eq!(decoded.polygons[0].geometry.outer().len(), 3);

        // And the same ring again as a strand, because that is what the
        // outline is drawn from. This one is wholly inside the tile, so it is
        // the whole loop: four positions, closing where it started.
        assert_eq!(decoded.lines.len(), 1);
        let outline = &decoded.lines[0].geometry;
        assert_eq!(outline.len(), 4);
        assert_eq!(
            outline.first().map(|p| p.coordinate),
            outline.last().map(|p| p.coordinate)
        );
        // Both belong to the one feature, so nothing double-counts it.
        assert_eq!(decoded.features.len(), 1);
    }

    #[test]
    fn a_clipped_rings_outline_stops_at_the_edge_rather_than_running_along_it() {
        // A triangle whose third corner is out in the buffer to the west, so
        // two of its three edges cross the tile boundary.
        // MoveTo(2048, 2048), LineTo(+1024, 0), LineTo(-6144, +1024), Close.
        let bytes = encode(vec![layer_with(
            "water",
            tile::GeomType::Polygon,
            vec![9, 4096, 4096, 18, 2048, 0, 12287, 2048, 15],
        )]);

        let decoded = decode(&bytes, ROOT, &[]).expect("a tile");
        // The fill is squared off against the tile: it gains corners on the
        // western edge that the original triangle never had.
        assert_eq!(decoded.polygons.len(), 1);
        assert!(decoded.polygons[0].geometry.outer().len() > 3);

        // The outline does not. It is the two stretches of real boundary that
        // reach into the tile, and nothing along the edge between them.
        assert_eq!(decoded.lines.len(), 2);
        let west = tile_bounds(ROOT).lon_min;
        for strand in &decoded.lines {
            // No strand may run *along* the western edge — both of its ends may
            // touch it, but not every one of its positions.
            assert!(strand.geometry.iter().any(|p| (p.lon() - west).abs() > 1.0));
        }
    }

    #[test]
    fn a_feature_clipped_away_leaves_no_row_behind_it() {
        // MoveTo(-2048, -2048), LineTo(-1024, 0): a strand entirely in the
        // buffer off the north-west corner.
        let bytes = encode(vec![layer_with(
            "transportation",
            tile::GeomType::Linestring,
            vec![9, 4095, 4095, 10, 2047, 0],
        )]);

        let decoded = decode(&bytes, ROOT, &[]).expect("a tile");
        assert!(decoded.lines.is_empty());
        assert!(decoded.features.is_empty());
    }

    #[test]
    fn gzipped_bytes_say_so_rather_than_reporting_a_broken_protobuf() {
        assert!(matches!(
            decode(&[0x1f, 0x8b, 0x08, 0x00], ROOT, &[]),
            Err(MvtError::Gzipped)
        ));
    }

    #[test]
    fn bytes_that_were_never_a_tile_are_an_error() {
        assert!(matches!(
            decode(b"<html>not a tile</html>", ROOT, &[]),
            Err(MvtError::Malformed(_))
        ));
    }
}
