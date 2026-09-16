//! Working out which feature is under the cursor.
//!
//! The cursor is already a latitude and longitude by the time it gets here —
//! [`crate::api::Cursor`] has done the ray-sphere intersection — so picking is
//! a two-dimensional problem: which shape of an overlay is within a tolerance
//! of a coordinate, and which feature that shape belongs to.
//!
//! Three things decide how it is done.
//!
//! **The tolerance is in pixels, because the geometry is.** A marker is nine
//! pixels across whatever the altitude, so what counts as "on it" has to be
//! nine pixels too — the caller converts that into degrees at the cursor's
//! distance and hands it over, and everything below works in degrees.
//!
//! **Degrees are made comparable by scaling longitude.** A degree of longitude
//! is a degree of arc only at the equator, so every distance here scales it by
//! the cosine of the latitude. That makes a tolerance mean the same thing in
//! Norway as in Kenya, at the cost of meaning progressively less at the poles,
//! where nothing round stays round on this projection anyway.
//!
//! **Lines are measured the way they are drawn.** The renderer interpolates a
//! segment in latitude and longitude rather than along a great circle (see
//! `crate::overlays::densify`), so the hit test does too. Measuring the chord
//! in three dimensions instead would quietly disagree with the line on screen
//! for any segment long enough to matter.
//!
//! What this is not is a depth test. The topmost thing wins by *kind* — a
//! marker over a line over a polygon — which is the order they are drawn in and
//! the order that makes a marker on top of a country selectable at all.
//!
//! Nor does it read heights. A shape placed at altitude is picked where it
//! stands on the ground rather than where it is drawn — the same place while
//! the camera looks straight down, and further apart the more the view is
//! tilted. Following the drawn geometry instead would mean casting the cursor
//! ray against it in three dimensions, which is a different piece of machinery
//! from the one below.

use bevy::math::Vec2;

use crate::geo::{LatLon, Position};
use crate::geojson::{GeoJson, Polygon};

/// What a hit landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    Point,
    Line,
    Polygon,
}

impl PickKind {
    /// The stable name the control surface calls this by.
    pub fn id(self) -> &'static str {
        match self {
            Self::Point => "point",
            Self::Line => "line",
            Self::Polygon => "polygon",
        }
    }

    /// Which kind wins when two are both under the cursor. Lower is topmost,
    /// and matches the order they are drawn in.
    fn rank(self) -> u8 {
        match self {
            Self::Point => 0,
            Self::Line => 1,
            Self::Polygon => 2,
        }
    }
}

/// How close a shape has to be to count, in degrees, per kind of shape.
#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    pub point: f32,
    pub line: f32,
}

/// One feature found under the cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// An index into [`GeoJson::features`].
    pub feature: usize,
    pub kind: PickKind,
    /// How far the cursor was from it, in degrees. Zero inside a polygon.
    pub distance: f32,
}

impl Hit {
    /// Whether this hit is the one to report, against one already found.
    ///
    /// Public because the choice has to be made across layers as well as within
    /// one, and it has to be made the same way both times — otherwise a marker
    /// would win over the polygon under it in one layer and lose to a polygon
    /// in another.
    pub fn beats(self, other: Self) -> bool {
        (self.kind.rank(), self.distance) < (other.kind.rank(), other.distance)
    }
}

/// Which kind of shape a feature is drawn as, for a feature named rather than
/// picked — an embedder pinning one by index has no hit to read it off.
pub fn kind_of(document: &GeoJson, feature: usize) -> Option<PickKind> {
    if document.points.iter().any(|shape| shape.feature == feature) {
        return Some(PickKind::Point);
    }
    if document.lines.iter().any(|shape| shape.feature == feature) {
        return Some(PickKind::Line);
    }
    if document
        .polygons
        .iter()
        .any(|shape| shape.feature == feature)
    {
        return Some(PickKind::Polygon);
    }
    None
}

/// Where each shape of a document is, roughly, so that most of them can be
/// dismissed without walking their vertices.
///
/// Built once when a layer's geometry is built. Without it, every frame the
/// cursor is over the globe would measure the distance to every segment of
/// every line in the layer — fine for a feed of fifty earthquakes, not fine for
/// a coastline.
#[derive(Debug, Default)]
pub struct PickIndex {
    lines: Vec<Reach>,
    polygons: Vec<Reach>,
}

impl PickIndex {
    pub fn build(document: &GeoJson) -> Self {
        Self {
            lines: document
                .lines
                .iter()
                .map(|shape| Reach::of(ground(&shape.geometry)))
                .collect(),
            polygons: document
                .polygons
                .iter()
                .map(|shape| Reach::of(shape.geometry.rings.iter().flatten().map(ground_of)))
                .collect(),
        }
    }

    /// The feature under `cursor`, or `None` when nothing is close enough.
    ///
    /// The index has to have been built from this document; a layer that has
    /// been refreshed since gets a new one along with its new geometry.
    pub fn pick(&self, document: &GeoJson, cursor: LatLon, tolerance: Tolerance) -> Option<Hit> {
        let mut best: Option<Hit> = None;

        for shape in &document.points {
            let distance = separation(cursor, ground_of(&shape.geometry));
            if distance <= tolerance.point {
                consider(
                    &mut best,
                    Hit {
                        feature: shape.feature,
                        kind: PickKind::Point,
                        distance,
                    },
                );
            }
        }

        for (shape, reach) in document.lines.iter().zip(&self.lines) {
            if !reach.could_reach(cursor, tolerance.line) {
                continue;
            }
            if let Some(distance) = path_distance(&shape.geometry, cursor, tolerance.line) {
                consider(
                    &mut best,
                    Hit {
                        feature: shape.feature,
                        kind: PickKind::Line,
                        distance,
                    },
                );
            }
        }

        for (shape, reach) in document.polygons.iter().zip(&self.polygons) {
            if !reach.could_reach(cursor, tolerance.line) {
                continue;
            }
            // The outline counts as much as the fill: a ring with a transparent
            // fill is still a shape on screen, and has to be grabbable by the
            // only part of it that was drawn.
            let outline = shape
                .geometry
                .rings
                .iter()
                .filter_map(|ring| ring_distance(ring, cursor, tolerance.line))
                .fold(None, |best: Option<f32>, distance| {
                    Some(best.map_or(distance, |best| best.min(distance)))
                });

            let distance = match outline {
                Some(distance) => distance,
                None if inside(&shape.geometry, cursor) => 0.0,
                None => continue,
            };
            consider(
                &mut best,
                Hit {
                    feature: shape.feature,
                    kind: PickKind::Polygon,
                    distance,
                },
            );
        }

        best
    }
}

/// Where a position stands on the ground, which is the only part of it picking
/// reads.
fn ground_of(position: &Position) -> LatLon {
    position.coordinate
}

/// The same over a path, as an iterator that can be walked more than once.
fn ground(path: &[Position]) -> impl Iterator<Item = LatLon> + Clone {
    path.iter().map(ground_of)
}

/// Keeps whichever of the two is more nearly under the cursor: the topmost kind
/// first, and within a kind the nearer one.
fn consider(best: &mut Option<Hit>, candidate: Hit) {
    if best.is_none_or(|held| candidate.beats(held)) {
        *best = Some(candidate);
    }
}

/// A shape's whereabouts: a centre, and how far from it any part of the shape
/// gets, both in the degrees [`separation`] measures.
#[derive(Debug)]
struct Reach {
    center: LatLon,
    degrees: f32,
}

impl Reach {
    /// A shape with no vertices, which nothing can be near.
    fn nowhere() -> Self {
        Self {
            center: LatLon::new(0.0, 0.0),
            degrees: -1.0,
        }
    }

    fn of(vertices: impl Iterator<Item = LatLon> + Clone) -> Self {
        // Longitudes are walked continuously before being averaged, so a shape
        // straddling the antimeridian gets its centre over itself rather than
        // on the far side of the world.
        let mut count = 0.0;
        let mut latitude = 0.0;
        let mut longitude = 0.0;
        let mut running = None;
        for vertex in vertices.clone() {
            let unwrapped = match running {
                Some(previous) => previous + shortest_turn(vertex.lon - previous),
                None => vertex.lon,
            };
            running = Some(unwrapped);
            latitude += vertex.lat;
            longitude += unwrapped;
            count += 1.0;
        }
        if count == 0.0 {
            return Self::nowhere();
        }

        let center = LatLon::new(
            latitude / count,
            shortest_turn(longitude / count).clamp(-180.0, 180.0),
        );
        Self {
            center,
            degrees: vertices.fold(0.0_f32, |most, vertex| most.max(separation(center, vertex))),
        }
    }

    fn could_reach(&self, cursor: LatLon, tolerance: f32) -> bool {
        separation(self.center, cursor) <= self.degrees + tolerance
    }
}

/// How far apart two coordinates are, in degrees, with longitude scaled so that
/// the number means the same thing at any latitude.
fn separation(from: LatLon, to: LatLon) -> f32 {
    offset(from, to).length()
}

/// `to`, as an offset from `from` in the flat local degrees everything here is
/// measured in: east on `x`, north on `y`.
fn offset(from: LatLon, to: LatLon) -> Vec2 {
    let scale = ((from.lat + to.lat) * 0.5).to_radians().cos();
    Vec2::new(shortest_turn(to.lon - from.lon) * scale, to.lat - from.lat)
}

/// How far the cursor is from a path, or `None` when it is further than the
/// tolerance from every segment of it.
fn path_distance(path: &[Position], cursor: LatLon, tolerance: f32) -> Option<f32> {
    segment_distance(
        path.windows(2)
            .map(|pair| (ground_of(&pair[0]), ground_of(&pair[1]))),
        cursor,
    )
    .filter(|distance| *distance <= tolerance)
}

/// The same, for a ring — which is a path that comes back to where it started.
fn ring_distance(ring: &[Position], cursor: LatLon, tolerance: f32) -> Option<f32> {
    if ring.len() < 2 {
        return None;
    }
    let edges = (0..ring.len()).map(|index| {
        (
            ground_of(&ring[index]),
            ground_of(&ring[(index + 1) % ring.len()]),
        )
    });
    segment_distance(edges, cursor).filter(|distance| *distance <= tolerance)
}

fn segment_distance(
    segments: impl Iterator<Item = (LatLon, LatLon)>,
    cursor: LatLon,
) -> Option<f32> {
    segments
        .map(|(from, to)| {
            // Measured in a plane pinned to the cursor, so the projection is at
            // its most accurate exactly where the answer matters.
            let (from, to) = (offset(cursor, from), offset(cursor, to));
            let along = to - from;
            let fraction = if along.length_squared() < 1.0e-12 {
                0.0
            } else {
                (-from.dot(along) / along.length_squared()).clamp(0.0, 1.0)
            };
            (from + along * fraction).length()
        })
        .fold(None, |best: Option<f32>, distance| {
            Some(best.map_or(distance, |best| best.min(distance)))
        })
}

/// Whether the cursor is inside a polygon, holes taken out.
///
/// A ray cast east, counting how many ring edges it crosses: odd is inside.
/// Counting over every ring at once is what takes the holes out — a point in a
/// hole crosses the outer ring once and the hole once, and two is even.
fn inside(polygon: &Polygon, cursor: LatLon) -> bool {
    let mut crossings = 0;
    for ring in &polygon.rings {
        if ring.len() < 3 {
            continue;
        }
        // Every corner is placed relative to the cursor, so a ring that
        // straddles the antimeridian is still a ring from where we are
        // standing. A ring more than half the world wide is not, but neither is
        // it something the fill could have drawn.
        let longitude = |corner: LatLon| cursor.lon + shortest_turn(corner.lon - cursor.lon);

        for index in 0..ring.len() {
            let (from, to) = (
                ground_of(&ring[index]),
                ground_of(&ring[(index + 1) % ring.len()]),
            );
            if (from.lat > cursor.lat) == (to.lat > cursor.lat) {
                continue;
            }
            let fraction = (cursor.lat - from.lat) / (to.lat - from.lat);
            let crossing = longitude(from) + fraction * (longitude(to) - longitude(from));
            if crossing > cursor.lon {
                crossings += 1;
            }
        }
    }
    crossings % 2 == 1
}

/// Brings an angle in degrees into `[-180, 180)`.
fn shortest_turn(degrees: f32) -> f32 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(json: &str) -> GeoJson {
        GeoJson::parse(json).expect("valid")
    }

    fn pick_at(document: &GeoJson, lat: f32, lon: f32) -> Option<Hit> {
        PickIndex::build(document).pick(
            document,
            LatLon::new(lat, lon),
            Tolerance {
                point: 0.5,
                line: 0.5,
            },
        )
    }

    #[test]
    fn a_marker_is_picked_within_its_tolerance_and_not_beyond_it() {
        let quakes = document(
            r#"{"type": "FeatureCollection", "features": [
                {"type": "Feature", "properties": {"mag": 1},
                 "geometry": {"type": "Point", "coordinates": [10, 20]}},
                {"type": "Feature", "properties": {"mag": 2},
                 "geometry": {"type": "Point", "coordinates": [40, 20]}}
            ]}"#,
        );

        let hit = pick_at(&quakes, 20.2, 10.0).expect("a hit");
        assert_eq!(hit.feature, 0);
        assert_eq!(hit.kind, PickKind::Point);
        assert!(pick_at(&quakes, 21.0, 10.0).is_none());

        assert_eq!(pick_at(&quakes, 20.0, 40.0).expect("a hit").feature, 1);
    }

    #[test]
    fn the_nearer_of_two_markers_wins() {
        let pair = document(r#"{"type": "MultiPoint", "coordinates": [[0, 0], [0.4, 0]]}"#);
        // Both are inside the tolerance; the closer one is the one meant.
        let index = PickIndex::build(&pair);
        let hit = index
            .pick(
                &pair,
                LatLon::new(0.0, 0.3),
                Tolerance {
                    point: 0.5,
                    line: 0.5,
                },
            )
            .expect("a hit");
        assert!((hit.distance - 0.1).abs() < 1.0e-3, "{hit:?}");
    }

    #[test]
    fn a_line_is_picked_along_its_length_and_not_past_its_end() {
        let track =
            document(r#"{"type": "LineString", "coordinates": [[0, 0], [10, 0], [10, 10]]}"#);
        // Beside the middle of the first segment.
        assert!(pick_at(&track, 0.2, 5.0).is_some());
        // Beside the corner, on the inside of the bend.
        assert!(pick_at(&track, 0.2, 9.8).is_some());
        // Past the far end.
        assert!(pick_at(&track, 11.0, 10.0).is_none());
        // Well off to one side.
        assert!(pick_at(&track, 3.0, 5.0).is_none());
    }

    #[test]
    fn a_marker_over_a_polygon_is_the_one_picked() {
        let both = document(
            r#"{"type": "FeatureCollection", "features": [
                {"type": "Feature", "properties": {"name": "country"}, "geometry":
                 {"type": "Polygon", "coordinates": [[[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]]]}},
                {"type": "Feature", "properties": {"name": "city"}, "geometry":
                 {"type": "Point", "coordinates": [5, 5]}}
            ]}"#,
        );

        assert_eq!(pick_at(&both, 5.0, 5.0).expect("a hit").feature, 1);
        // Away from the marker, the polygon underneath is what is left.
        let hit = pick_at(&both, 8.0, 8.0).expect("a hit");
        assert_eq!(hit.feature, 0);
        assert_eq!(hit.kind, PickKind::Polygon);
    }

    #[test]
    fn a_hole_is_not_part_of_the_polygon() {
        let ring = document(
            r#"{"type": "Polygon", "coordinates": [
                [[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]],
                [[3, 3], [7, 3], [7, 7], [3, 7], [3, 3]]
            ]}"#,
        );
        assert!(pick_at(&ring, 1.5, 1.5).is_some());
        assert!(pick_at(&ring, 5.0, 5.0).is_none());
        // On the hole's own edge, which is drawn and so is pickable.
        assert!(pick_at(&ring, 5.0, 3.0).is_some());
    }

    #[test]
    fn a_polygon_over_the_antimeridian_is_still_under_the_cursor() {
        let fiji = document(
            r#"{"type": "Polygon", "coordinates":
                [[[178, -18], [-178, -18], [-178, -16], [178, -16], [178, -18]]]}"#,
        );
        assert!(pick_at(&fiji, -17.0, 179.5).is_some());
        assert!(pick_at(&fiji, -17.0, -179.0).is_some());
        assert!(pick_at(&fiji, -17.0, 170.0).is_none());
    }

    #[test]
    fn a_shape_the_cursor_is_nowhere_near_is_dismissed_without_walking_it() {
        let track = document(r#"{"type": "LineString", "coordinates": [[0, 0], [1, 0], [2, 0]]}"#);
        let index = PickIndex::build(&track);
        assert!(!index.lines[0].could_reach(LatLon::new(0.0, 90.0), 0.5));
        assert!(index.lines[0].could_reach(LatLon::new(0.0, 1.0), 0.5));
    }

    #[test]
    fn every_shape_of_a_feature_picks_that_feature() {
        let islands = document(
            r#"{"type": "Feature", "properties": {"name": "archipelago"}, "geometry":
                {"type": "MultiPoint", "coordinates": [[0, 0], [40, 40]]}}"#,
        );
        assert_eq!(pick_at(&islands, 0.0, 0.0).expect("a hit").feature, 0);
        assert_eq!(pick_at(&islands, 40.0, 40.0).expect("a hit").feature, 0);
    }
}
