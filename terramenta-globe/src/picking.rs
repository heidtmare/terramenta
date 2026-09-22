//! Working out which feature is under the cursor.
//!
//! The cursor is already a latitude and longitude by the time it gets here —
//! [`crate::api::Cursor`] does the ray-sphere intersection — so picking is a
//! two-dimensional problem: which shape of a [`FeatureSet`] is within
//! tolerance of a coordinate, and which feature that shape belongs to.
//!
//! Four rules govern it.
//!
//! **Tolerance is in pixels, because the geometry is.** A marker is nine
//! pixels across regardless of altitude, so "on it" is defined as nine
//! pixels too: the caller converts that into degrees at the cursor's
//! distance and hands it over, and everything below works in degrees.
//!
//! **Degrees are made comparable by scaling longitude.** A degree of
//! longitude is a degree of arc only at the equator, so every distance here
//! scales it by the cosine of the latitude. That keeps a tolerance meaning
//! the same thing at any latitude, though it degrades near the poles, where
//! this projection distorts everything anyway.
//!
//! **Lines are measured the way they are drawn.** The renderer interpolates
//! a segment in latitude and longitude rather than along a great circle (see
//! `crate::overlays::densify`), and the hit test does the same; measuring
//! the chord in three dimensions would disagree with the rendered line for
//! any sufficiently long segment.
//!
//! **The arithmetic is `f64`, and reads the store's coordinates directly.**
//! Every vertex comes from the GeoArrow buffers exactly as the document
//! wrote it (see [`crate::features`]), unmodified and uncopied. Tolerances
//! and the returned distance stay `f32`, since both are pixel counts from
//! the camera.
//!
//! This is not a depth test: the topmost shape wins by *kind* — marker over
//! line over polygon — matching draw order, which is what makes a marker on
//! top of a polygon selectable.
//!
//! It also does not read heights: an elevated shape is picked where it
//! stands on the ground rather than where it is drawn. The two coincide
//! when the camera looks straight down and diverge as the view tilts.
//! Following the drawn geometry would require casting the cursor ray against
//! it in three dimensions, a different mechanism from this one.
//!
//! Satellites are not picked here for the same reason: nothing an ephemeris
//! draws is on the ground, and at altitudes of hundreds of kilometres the two
//! positions are far apart. [`crate::ephemeris`] instead projects its markers
//! into the viewport and measures the pointer against them in pixels — a
//! separate mechanism that works on points only, needs the camera every
//! frame, and cannot cull anything in advance since everything it holds
//! moves each frame.

use bevy::math::DVec2;

use crate::features::{Coords, FeatureSet, PolygonRef};
use crate::geo::{LatLon, Position};

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

/// How close a shape has to be to count.
///
/// In pixels rather than in degrees, and with the conversion carried alongside,
/// because a shape's reach is its own size on screen and a document is allowed
/// to size its features one at a time — see [`crate::simplestyle`]. Resolving
/// it here rather than at the call site is what keeps a feed of small markers
/// and one large one from all being grabbable at the large one's radius.
#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    /// How much globe one device pixel covers, in degrees, at the depth the
    /// cursor is at.
    pub degrees_per_pixel: f32,
    /// The layer's marker diameter, for the features the document did not size.
    pub point_px: f32,
    /// The layer's line width, likewise.
    pub line_px: f32,
    /// How far off a shape the cursor may still be, on top of the shape's own
    /// half-width. A two-pixel line is otherwise a two-pixel target.
    pub slack_px: f32,
    /// Whether the document's own sizes count. Off, every feature is measured
    /// at the layer's size — which is what it was drawn at, because the layer
    /// is ignoring the document's style there too.
    pub simple_style: bool,
}

impl Tolerance {
    /// How far from a shape of this size on screen the cursor still counts, in
    /// the degrees [`separation`] measures.
    ///
    /// Half the size, because a shape is drawn either side of where it is.
    fn reach(&self, size_px: f32) -> f64 {
        f64::from((size_px * 0.5 + self.slack_px) * self.degrees_per_pixel)
    }

    /// The reach of one feature's markers, or of its lines.
    fn point_reach(&self, document: &FeatureSet, feature: u32) -> f64 {
        self.reach(self.sized(document, feature, PickKind::Point))
    }

    fn line_reach(&self, document: &FeatureSet, feature: u32) -> f64 {
        self.reach(self.sized(document, feature, PickKind::Line))
    }

    /// The size on screen the document asked for, or the layer's.
    fn sized(&self, document: &FeatureSet, feature: u32, kind: PickKind) -> f32 {
        let layer = match kind {
            PickKind::Point => self.point_px,
            _ => self.line_px,
        };
        if !self.simple_style {
            return layer;
        }
        let Some(style) = document.feature_style(feature as usize) else {
            return layer;
        };
        match kind {
            PickKind::Point => style
                .marker_size
                .map_or(layer, crate::simplestyle::MarkerSize::diameter_px),
            _ => style.stroke_width.unwrap_or(layer),
        }
    }

    /// The widest anything in this document is drawn, which is as far as the
    /// broad phase has to look before it can dismiss a shape outright.
    fn widest_line_reach(&self, document: &FeatureSet) -> f64 {
        if !self.simple_style || !document.has_styles() {
            return self.reach(self.line_px);
        }
        let widest = (0..document.feature_count())
            .map(|feature| self.sized(document, feature as u32, PickKind::Line))
            .fold(self.line_px, f32::max);
        self.reach(widest)
    }
}

/// One feature found under the cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// An index into the set's features.
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
pub fn kind_of(document: &FeatureSet, feature: usize) -> Option<PickKind> {
    let owned = |owners: &[u32]| owners.iter().any(|owner| *owner as usize == feature);
    if owned(document.point_owners()) {
        return Some(PickKind::Point);
    }
    if owned(document.line_owners()) {
        return Some(PickKind::Line);
    }
    if owned(document.polygon_owners()) {
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
    pub fn build(document: &FeatureSet) -> Self {
        Self {
            lines: document
                .lines()
                .map(|(_, line)| Reach::of(line.iter().map(ground)))
                .collect(),
            polygons: document
                .polygons()
                .map(|(_, polygon)| Reach::of(polygon.vertices().map(ground)))
                .collect(),
        }
    }

    /// The feature under `cursor`, or `None` when nothing is close enough.
    ///
    /// The index has to have been built from this document; a layer that has
    /// been refreshed since gets a new one along with its new geometry.
    pub fn pick(&self, document: &FeatureSet, cursor: LatLon, tolerance: Tolerance) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        let cursor = DVec2::new(f64::from(cursor.lon), f64::from(cursor.lat));
        // The broad phase has to be generous enough for the widest feature in
        // the document; the measurement that follows is per feature, so being
        // generous here only costs a distance test that then fails.
        let widest = tolerance.widest_line_reach(document);

        for (feature, point) in document.points() {
            let distance = separation(cursor, ground(point));
            if distance <= tolerance.point_reach(document, feature) {
                consider(
                    &mut best,
                    Hit {
                        feature: feature as usize,
                        kind: PickKind::Point,
                        distance: distance as f32,
                    },
                );
            }
        }

        for ((feature, line), reach) in document.lines().zip(&self.lines) {
            if !reach.could_reach(cursor, widest) {
                continue;
            }
            let line_reach = tolerance.line_reach(document, feature);
            if let Some(distance) = path_distance(line, cursor, line_reach) {
                consider(
                    &mut best,
                    Hit {
                        feature: feature as usize,
                        kind: PickKind::Line,
                        distance: distance as f32,
                    },
                );
            }
        }

        for ((feature, polygon), reach) in document.polygons().zip(&self.polygons) {
            if !reach.could_reach(cursor, widest) {
                continue;
            }
            let line_reach = tolerance.line_reach(document, feature);
            // The outline counts as much as the fill: a ring with a transparent
            // fill is still a shape on screen, and has to be grabbable by the
            // only part of it that was drawn.
            let outline = polygon
                .rings()
                .filter_map(|ring| ring_distance(ring, cursor, line_reach))
                .fold(None, |best: Option<f64>, distance| {
                    Some(best.map_or(distance, |best| best.min(distance)))
                });

            let distance = match outline {
                Some(distance) => distance,
                None if inside(polygon, cursor) => 0.0,
                None => continue,
            };
            consider(
                &mut best,
                Hit {
                    feature: feature as usize,
                    kind: PickKind::Polygon,
                    distance: distance as f32,
                },
            );
        }

        best
    }
}

/// Where a position stands on the ground, which is the only part of it picking
/// reads: longitude on `x`, latitude on `y`.
fn ground(position: Position) -> DVec2 {
    DVec2::new(position.lon, position.lat)
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
    center: DVec2,
    degrees: f64,
}

impl Reach {
    /// A shape with no vertices, which nothing can be near.
    fn nowhere() -> Self {
        Self {
            center: DVec2::ZERO,
            degrees: -1.0,
        }
    }

    fn of(vertices: impl Iterator<Item = DVec2> + Clone) -> Self {
        // Longitudes are walked continuously before being averaged, so a shape
        // straddling the antimeridian gets its centre over itself rather than
        // on the far side of the world.
        let mut count = 0.0;
        let mut latitude = 0.0;
        let mut longitude = 0.0;
        let mut running = None;
        for vertex in vertices.clone() {
            let unwrapped = match running {
                Some(previous) => previous + shortest_turn(vertex.x - previous),
                None => vertex.x,
            };
            running = Some(unwrapped);
            latitude += vertex.y;
            longitude += unwrapped;
            count += 1.0;
        }
        if count == 0.0 {
            return Self::nowhere();
        }

        let center = DVec2::new(
            shortest_turn(longitude / count).clamp(-180.0, 180.0),
            latitude / count,
        );
        Self {
            center,
            degrees: vertices.fold(0.0_f64, |most, vertex| most.max(separation(center, vertex))),
        }
    }

    fn could_reach(&self, cursor: DVec2, tolerance: f64) -> bool {
        separation(self.center, cursor) <= self.degrees + tolerance
    }
}

/// How far apart two coordinates are, in degrees, with longitude scaled so that
/// the number means the same thing at any latitude.
fn separation(from: DVec2, to: DVec2) -> f64 {
    offset(from, to).length()
}

/// `to`, as an offset from `from` in the flat local degrees everything here is
/// measured in: east on `x`, north on `y`.
fn offset(from: DVec2, to: DVec2) -> DVec2 {
    let scale = ((from.y + to.y) * 0.5).to_radians().cos();
    DVec2::new(shortest_turn(to.x - from.x) * scale, to.y - from.y)
}

/// How far the cursor is from a path, or `None` when it is further than the
/// tolerance from every segment of it.
fn path_distance(path: Coords<'_>, cursor: DVec2, tolerance: f64) -> Option<f64> {
    let edges = (0..path.len().saturating_sub(1))
        .map(|index| (ground(path.get(index)), ground(path.get(index + 1))));
    segment_distance(edges, cursor).filter(|distance| *distance <= tolerance)
}

/// The same, for a ring — which is a path that comes back to where it started.
fn ring_distance(ring: Coords<'_>, cursor: DVec2, tolerance: f64) -> Option<f64> {
    if ring.len() < 2 {
        return None;
    }
    let edges =
        (0..ring.len()).map(|index| (ground(ring.get(index)), ground(ring.wrapping(index + 1))));
    segment_distance(edges, cursor).filter(|distance| *distance <= tolerance)
}

fn segment_distance(segments: impl Iterator<Item = (DVec2, DVec2)>, cursor: DVec2) -> Option<f64> {
    segments
        .map(|(from, to)| {
            // Measured in a plane pinned to the cursor, so the projection is at
            // its most accurate exactly where the answer matters.
            let (from, to) = (offset(cursor, from), offset(cursor, to));
            let along = to - from;
            let fraction = if along.length_squared() < 1.0e-24 {
                0.0
            } else {
                (-from.dot(along) / along.length_squared()).clamp(0.0, 1.0)
            };
            (from + along * fraction).length()
        })
        .fold(None, |best: Option<f64>, distance| {
            Some(best.map_or(distance, |best| best.min(distance)))
        })
}

/// Whether the cursor is inside a polygon, holes taken out.
///
/// A ray cast east, counting how many ring edges it crosses: odd is inside.
/// Counting over every ring at once is what takes the holes out — a point in a
/// hole crosses the outer ring once and the hole once, and two is even.
fn inside(polygon: PolygonRef<'_>, cursor: DVec2) -> bool {
    let mut crossings = 0;
    for ring in polygon.rings() {
        if ring.len() < 3 {
            continue;
        }
        // Every corner is placed relative to the cursor, so a ring that
        // straddles the antimeridian is still a ring from where we are
        // standing. A ring more than half the world wide is not, but neither is
        // it something the fill could have drawn.
        let longitude = |corner: DVec2| cursor.x + shortest_turn(corner.x - cursor.x);

        for index in 0..ring.len() {
            let (from, to) = (ground(ring.get(index)), ground(ring.wrapping(index + 1)));
            if (from.y > cursor.y) == (to.y > cursor.y) {
                continue;
            }
            let fraction = (cursor.y - from.y) / (to.y - from.y);
            let crossing = longitude(from) + fraction * (longitude(to) - longitude(from));
            if crossing > cursor.x {
                crossings += 1;
            }
        }
    }
    crossings % 2 == 1
}

/// Brings an angle in degrees into `[-180, 180)`.
fn shortest_turn(degrees: f64) -> f64 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(json: &str) -> FeatureSet {
        crate::geojson::parse(json).expect("valid")
    }

    /// Half a degree of reach for everything, so the tests read in degrees:
    /// one pixel to the degree, a shape a degree across, and no slack.
    const HALF_A_DEGREE: Tolerance = Tolerance {
        degrees_per_pixel: 1.0,
        point_px: 1.0,
        line_px: 1.0,
        slack_px: 0.0,
        simple_style: true,
    };

    fn pick_at(document: &FeatureSet, lat: f32, lon: f32) -> Option<Hit> {
        PickIndex::build(document).pick(document, LatLon::new(lat, lon), HALF_A_DEGREE)
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
    fn a_marker_is_grabbable_at_the_size_it_was_drawn() {
        // A document that sized its own markers gets a target to match — a
        // large marker with a small marker's reach is a thing you can see and
        // cannot click.
        let quakes = document(
            r##"{"type": "FeatureCollection", "features": [
                {"type": "Feature", "properties": {"marker-size": "large"},
                 "geometry": {"type": "Point", "coordinates": [0, 0]}},
                {"type": "Feature", "properties": {"marker-size": "small"},
                 "geometry": {"type": "Point", "coordinates": [40, 0]}}
            ]}"##,
        );
        // One degree per pixel, so a marker's reach in degrees is half the
        // pixels it was drawn at.
        let tolerance = Tolerance {
            degrees_per_pixel: 1.0,
            point_px: 1.0,
            line_px: 1.0,
            slack_px: 0.0,
            simple_style: true,
        };
        let index = PickIndex::build(&quakes);
        let at = |lon: f32| index.pick(&quakes, LatLon::new(0.0, lon), tolerance);

        let large = crate::simplestyle::MARKER_LARGE_PX * 0.5;
        let small = crate::simplestyle::MARKER_SMALL_PX * 0.5;
        assert!(at(f32::midpoint(0.0, large)).is_some());
        assert!(at(large + 1.0).is_none());
        // And the small one is not grabbable at the large one's reach.
        assert!(at(40.0 + small - 0.5).is_some());
        assert!(at(40.0 + small + 1.0).is_none());

        // A layer that ignores the document measures every feature at its own
        // size, which is also what it drew them at.
        let plain = Tolerance {
            simple_style: false,
            ..tolerance
        };
        assert!(
            index
                .pick(&quakes, LatLon::new(0.0, large - 1.0), plain)
                .is_none()
        );
    }

    #[test]
    fn the_nearer_of_two_markers_wins() {
        let pair = document(r#"{"type": "MultiPoint", "coordinates": [[0, 0], [0.4, 0]]}"#);
        // Both are inside the tolerance; the closer one is the one meant.
        let index = PickIndex::build(&pair);
        let hit = index
            .pick(&pair, LatLon::new(0.0, 0.3), HALF_A_DEGREE)
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
        assert!(!index.lines[0].could_reach(DVec2::new(90.0, 0.0), 0.5));
        assert!(index.lines[0].could_reach(DVec2::new(1.0, 0.0), 0.5));
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

    #[test]
    fn a_feature_with_no_geometry_is_of_no_kind() {
        let sparse = document(
            r#"{"type": "FeatureCollection", "features": [
                {"type": "Feature", "geometry": null},
                {"type": "Feature", "geometry": {"type": "Point", "coordinates": [0, 0]}}
            ]}"#,
        );
        assert_eq!(kind_of(&sparse, 0), None);
        assert_eq!(kind_of(&sparse, 1), Some(PickKind::Point));
    }
}
