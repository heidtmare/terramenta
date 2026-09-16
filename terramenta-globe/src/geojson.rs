//! GeoJSON, as much of [RFC 7946](https://datatracker.ietf.org/doc/html/rfc7946)
//! as a globe has anything to draw with.
//!
//! The document is flattened as it is parsed. A globe draws points, lines and
//! filled rings; it does not care whether a line arrived as a `LineString`, as
//! one strand of a `MultiLineString`, or nested three deep inside a
//! `GeometryCollection` inside a `Feature`. So the whole tree collapses into a
//! [`GeoJson`] of three lists, which is exactly what [`crate::overlays`] builds
//! meshes from, and the feature count is kept only because an interface wants
//! something to show.
//!
//! Parsing is deliberately forgiving about everything except the shape of the
//! document. A coordinate that is not a pair of finite numbers is dropped, a
//! ring with fewer than three distinct corners is dropped, and a geometry left
//! empty by either goes with them — a feed with one bad record is still worth
//! drawing. A `type` that is not a GeoJSON type, or coordinates nested to the
//! wrong depth, is an error: that is not one bad record, it is a document that
//! was never GeoJSON.
//!
//! Properties are read past. Styling here is per layer rather than per feature
//! — see [`crate::overlays::OverlayStyle`] — so there is nothing in them to
//! keep.

use serde::Deserialize;

use crate::geo::LatLon;

/// The drawable geometry of one GeoJSON document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeoJson {
    /// Every `Point`, and every position of every `MultiPoint`.
    pub points: Vec<LatLon>,
    /// Every `LineString`, and every strand of every `MultiLineString`.
    pub lines: Vec<Vec<LatLon>>,
    pub polygons: Vec<Polygon>,
    /// How many `Feature` objects the document held, which is what a feed
    /// means by "how many earthquakes" — the geometry counts are lists of
    /// shapes, and one feature can be several of them.
    pub features: usize,
}

impl GeoJson {
    /// Parses a document. See the module docs for what is tolerated.
    pub fn parse(text: &str) -> Result<Self, GeoJsonError> {
        let object: Object = serde_json::from_str(text)?;
        let mut collected = Self::default();
        object.collect(&mut collected);
        Ok(collected)
    }
}

/// A polygon: an outer ring, then one ring per hole in it.
///
/// Rings are stored open — the closing position RFC 7946 requires is dropped,
/// because every consumer here would otherwise have to drop it again.
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    pub rings: Vec<Vec<LatLon>>,
}

impl Polygon {
    pub fn outer(&self) -> &[LatLon] {
        self.rings.first().map(Vec::as_slice).unwrap_or_default()
    }

    pub fn holes(&self) -> &[Vec<LatLon>] {
        self.rings.get(1..).unwrap_or_default()
    }
}

/// Why a document could not be read.
#[derive(Debug)]
pub enum GeoJsonError {
    /// Not JSON, or not a JSON document shaped like GeoJSON.
    Malformed(serde_json::Error),
}

impl std::fmt::Display for GeoJsonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // `serde_json` already reports the line and column, which is the
            // part that makes a parse failure actionable.
            Self::Malformed(error) => write!(formatter, "not valid GeoJSON: {error}"),
        }
    }
}

impl std::error::Error for GeoJsonError {}

impl From<serde_json::Error> for GeoJsonError {
    fn from(error: serde_json::Error) -> Self {
        Self::Malformed(error)
    }
}

// ---------------------------------------------------------------------------
// The document as it arrives
// ---------------------------------------------------------------------------

/// One position, as GeoJSON writes it: `[longitude, latitude]`, with an
/// optional third element — elevation, or in the USGS feeds depth — that a
/// globe draping everything on the surface has no use for.
type Position = Vec<f64>;

/// Any GeoJSON object.
///
/// One enum for all nine types rather than a separate geometry enum, because
/// the nesting is not layered the way a type hierarchy would suggest: a
/// `GeometryCollection` may hold another, a `Feature` holds a geometry, and a
/// `FeatureCollection` holds features. Internally tagged on `type`, which is
/// how GeoJSON discriminates them.
///
/// Every `coordinates` defaults, so a member missing or explicitly `null`
/// leaves an empty geometry to be dropped rather than failing the document.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum Object {
    FeatureCollection {
        #[serde(default)]
        features: Vec<Object>,
    },
    Feature {
        #[serde(default)]
        geometry: Option<Box<Object>>,
    },
    GeometryCollection {
        #[serde(default)]
        geometries: Vec<Object>,
    },
    Point {
        #[serde(default)]
        coordinates: Position,
    },
    MultiPoint {
        #[serde(default)]
        coordinates: Vec<Position>,
    },
    LineString {
        #[serde(default)]
        coordinates: Vec<Position>,
    },
    MultiLineString {
        #[serde(default)]
        coordinates: Vec<Vec<Position>>,
    },
    Polygon {
        #[serde(default)]
        coordinates: Vec<Vec<Position>>,
    },
    MultiPolygon {
        #[serde(default)]
        coordinates: Vec<Vec<Vec<Position>>>,
    },
}

impl Object {
    fn collect(self, into: &mut GeoJson) {
        match self {
            Self::FeatureCollection { features } => {
                for feature in features {
                    feature.collect(into);
                }
            }
            Self::Feature { geometry } => {
                into.features += 1;
                if let Some(geometry) = geometry {
                    geometry.collect(into);
                }
            }
            Self::GeometryCollection { geometries } => {
                for geometry in geometries {
                    geometry.collect(into);
                }
            }
            Self::Point { coordinates } => into.points.extend(position(&coordinates)),
            Self::MultiPoint { coordinates } => {
                into.points.extend(coordinates.iter().filter_map(position));
            }
            Self::LineString { coordinates } => push_line(into, &coordinates),
            Self::MultiLineString { coordinates } => {
                for line in &coordinates {
                    push_line(into, line);
                }
            }
            Self::Polygon { coordinates } => push_polygon(into, &coordinates),
            Self::MultiPolygon { coordinates } => {
                for polygon in &coordinates {
                    push_polygon(into, polygon);
                }
            }
        }
    }
}

fn push_line(into: &mut GeoJson, positions: &[Position]) {
    let line: Vec<LatLon> = positions.iter().filter_map(position).collect();
    // A line of one point is a point that cannot be drawn, not a point.
    if line.len() >= 2 {
        into.lines.push(line);
    }
}

fn push_polygon(into: &mut GeoJson, rings: &[Vec<Position>]) {
    let rings: Vec<Vec<LatLon>> = rings.iter().filter_map(|ring| self::ring(ring)).collect();
    // Holes without an outer ring are not holes in anything. Because the outer
    // ring is first, a polygon that lost it would silently promote a hole.
    if let Some(outer) = rings.first()
        && !outer.is_empty()
    {
        into.polygons.push(Polygon { rings });
    }
}

/// A linear ring, opened: the repeated closing position RFC 7946 requires is
/// dropped, and a ring left with fewer than three corners cannot bound an area.
fn ring(positions: &[Position]) -> Option<Vec<LatLon>> {
    let mut ring: Vec<LatLon> = positions.iter().filter_map(position).collect();
    if ring.len() >= 2 && ring.first() == ring.last() {
        ring.pop();
    }
    (ring.len() >= 3).then_some(ring)
}

/// Reads one position, or `None` for anything that is not a usable coordinate.
fn position(position: &Position) -> Option<LatLon> {
    let (&longitude, &latitude) = (position.first()?, position.get(1)?);
    if !longitude.is_finite() || !latitude.is_finite() {
        return None;
    }
    Some(LatLon::new(
        // A latitude past the pole is meaningless rather than wrong-by-a-turn,
        // so it is clamped; a longitude past the antimeridian is the same place
        // said the long way round, so it is wrapped.
        latitude.clamp(-90.0, 90.0) as f32,
        wrap_longitude(longitude) as f32,
    ))
}

/// Brings a longitude into `[-180, 180)`.
fn wrap_longitude(longitude: f64) -> f64 {
    (longitude + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feature_collection_flattens_to_its_geometry() {
        let parsed = GeoJson::parse(
            r#"{
                "type": "FeatureCollection",
                "features": [
                    {
                        "type": "Feature",
                        "properties": {"mag": 4.2},
                        "geometry": {"type": "Point", "coordinates": [-122.4, 37.8, -8000]}
                    },
                    {
                        "type": "Feature",
                        "properties": null,
                        "geometry": {
                            "type": "MultiLineString",
                            "coordinates": [[[0, 0], [1, 1]], [[2, 2], [3, 3], [4, 4]]]
                        }
                    }
                ]
            }"#,
        )
        .expect("valid");

        assert_eq!(parsed.features, 2);
        assert_eq!(parsed.points, vec![LatLon::new(37.8, -122.4)]);
        assert_eq!(parsed.lines.len(), 2);
        assert_eq!(parsed.lines[1].len(), 3);
    }

    #[test]
    fn a_bare_geometry_is_a_document_too() {
        let parsed =
            GeoJson::parse(r#"{"type": "Point", "coordinates": [10, 20]}"#).expect("valid");
        assert_eq!(parsed.points, vec![LatLon::new(20.0, 10.0)]);
        // Nothing was wrapped in a feature, so there are no features to count.
        assert_eq!(parsed.features, 0);
    }

    #[test]
    fn geometry_collections_nest() {
        let parsed = GeoJson::parse(
            r#"{
                "type": "GeometryCollection",
                "geometries": [
                    {"type": "Point", "coordinates": [1, 2]},
                    {
                        "type": "GeometryCollection",
                        "geometries": [{"type": "Point", "coordinates": [3, 4]}]
                    }
                ]
            }"#,
        )
        .expect("valid");
        assert_eq!(parsed.points.len(), 2);
    }

    #[test]
    fn rings_are_opened_and_holes_kept_in_order() {
        let parsed = GeoJson::parse(
            r#"{
                "type": "Polygon",
                "coordinates": [
                    [[0, 0], [4, 0], [4, 4], [0, 4], [0, 0]],
                    [[1, 1], [2, 1], [2, 2], [1, 2], [1, 1]]
                ]
            }"#,
        )
        .expect("valid");

        let polygon = &parsed.polygons[0];
        assert_eq!(polygon.outer().len(), 4);
        assert_eq!(polygon.holes().len(), 1);
        assert_eq!(polygon.holes()[0].len(), 4);
    }

    #[test]
    fn one_bad_record_does_not_take_the_document_with_it() {
        let parsed = GeoJson::parse(
            r#"{
                "type": "FeatureCollection",
                "features": [
                    {"type": "Feature", "geometry": null},
                    {"type": "Feature", "geometry": {"type": "Point", "coordinates": []}},
                    {"type": "Feature", "geometry": {"type": "LineString", "coordinates": [[0, 0]]}},
                    {"type": "Feature",
                     "geometry": {"type": "Polygon", "coordinates": [[[0, 0], [1, 0], [0, 0]]]}},
                    {"type": "Feature", "geometry": {"type": "Point", "coordinates": [5, 6]}}
                ]
            }"#,
        )
        .expect("valid");

        assert_eq!(parsed.features, 5);
        assert_eq!(parsed.points, vec![LatLon::new(6.0, 5.0)]);
        assert!(parsed.lines.is_empty());
        assert!(parsed.polygons.is_empty());
    }

    #[test]
    fn a_document_that_was_never_geojson_is_an_error() {
        assert!(GeoJson::parse("not json at all").is_err());
        assert!(GeoJson::parse(r#"{"type": "Raster", "coordinates": []}"#).is_err());
        assert!(GeoJson::parse(r#"{"type": "Point", "coordinates": [[1, 2]]}"#).is_err());
    }

    #[test]
    fn coordinates_are_wrapped_and_clamped() {
        let parsed =
            GeoJson::parse(r#"{"type": "MultiPoint", "coordinates": [[190, 95], [-200, -95]]}"#)
                .expect("valid");
        assert_eq!(
            parsed.points,
            vec![LatLon::new(90.0, -170.0), LatLon::new(-90.0, 160.0)]
        );
    }
}
