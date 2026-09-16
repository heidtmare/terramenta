//! GeoJSON, as much of [RFC 7946](https://datatracker.ietf.org/doc/html/rfc7946)
//! as a globe has anything to draw with.
//!
//! The document is flattened as it is parsed. A globe draws points, lines and
//! filled rings; it does not care whether a line arrived as a `LineString`, as
//! one strand of a `MultiLineString`, or nested three deep inside a
//! `GeometryCollection` inside a `Feature`. So the whole tree collapses into a
//! [`GeoJson`] of three lists, which is exactly what [`crate::overlays`] builds
//! meshes from.
//!
//! What the flattening does *not* throw away is which feature each shape came
//! from. Every [`Shape`] carries an index into [`GeoJson::features`], and that
//! is what makes a shape pickable: the globe hit-tests the geometry, and the
//! feature behind the shape it hits is what an interface is handed, properties
//! and all. It is also why a `MultiPolygon` of forty islands highlights as one
//! country rather than as the island under the cursor.
//!
//! Parsing is deliberately forgiving about everything except the shape of the
//! document. A coordinate that is not a pair of finite numbers is dropped, a
//! ring with fewer than three distinct corners is dropped, and a geometry left
//! empty by either goes with them — a feed with one bad record is still worth
//! drawing. A `type` that is not a GeoJSON type, or coordinates nested to the
//! wrong depth, is an error: that is not one bad record, it is a document that
//! was never GeoJSON.
//!
//! Properties are kept exactly as they arrived, as JSON, and handed back out
//! untouched when a feature is picked. Nothing here reads them: what a `mag` or
//! a `place` means is the feed's business and the interface's, not the globe's.

use serde::{Deserialize, Serialize};

use crate::geo::LatLon;

/// The drawable geometry of one GeoJSON document, and the features it belongs
/// to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeoJson {
    /// Every `Feature` the document held, in the order it held them — which is
    /// what a feed means by "how many earthquakes". The geometry lists below
    /// are lists of *shapes*, and one feature can be many of those.
    ///
    /// A geometry that arrived outside any feature gets one of its own, with no
    /// properties, so that everything drawn is pickable and nothing has to
    /// special-case a document written without features.
    pub features: Vec<Feature>,
    /// Every `Point`, and every position of every `MultiPoint`.
    pub points: Vec<Shape<LatLon>>,
    /// Every `LineString`, and every strand of every `MultiLineString`.
    pub lines: Vec<Shape<Vec<LatLon>>>,
    pub polygons: Vec<Shape<Polygon>>,
}

impl GeoJson {
    /// Parses a document. See the module docs for what is tolerated.
    pub fn parse(text: &str) -> Result<Self, GeoJsonError> {
        let object: Object = serde_json::from_str(text)?;
        let mut collected = Self::default();
        object.collect(&mut collected, None);
        Ok(collected)
    }

    /// The feature a geometry belongs to, inventing one for a geometry that
    /// arrived outside any.
    fn owner(&mut self, feature: Option<usize>) -> usize {
        feature.unwrap_or_else(|| {
            self.features.push(Feature::default());
            self.features.len() - 1
        })
    }
}

/// One drawable shape, and which feature it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape<T> {
    /// An index into [`GeoJson::features`].
    pub feature: usize,
    pub geometry: T,
}

/// One `Feature`: everything about it that is not geometry.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Feature {
    /// The `id` member, if it had one. GeoJSON allows a string or a number and
    /// says nothing about what either means, so both arrive here as text.
    pub id: Option<String>,
    /// The `properties` object, exactly as it arrived — `null` when there was
    /// none. Handed straight back out to whoever picks the feature.
    pub properties: serde_json::Value,
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
        #[serde(default)]
        properties: serde_json::Value,
        /// A string or a number, per the specification; kept as whichever it
        /// was until a feature is built out of it.
        #[serde(default)]
        id: Option<serde_json::Value>,
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
    /// Walks the document, adding what it finds to `into`.
    ///
    /// `feature` is the feature whatever is found belongs to: `Some` once a
    /// `Feature` has been entered, `None` above that. A geometry reached with
    /// `None` gets a feature of its own — see [`GeoJson::owner`] — so a bare
    /// `Point` of a document is as pickable as one inside a feed.
    fn collect(self, into: &mut GeoJson, feature: Option<usize>) {
        match self {
            Self::FeatureCollection { features } => {
                for member in features {
                    member.collect(into, feature);
                }
            }
            Self::Feature {
                geometry,
                properties,
                id,
            } => {
                into.features.push(Feature {
                    id: id.as_ref().and_then(identifier),
                    properties,
                });
                let index = into.features.len() - 1;
                if let Some(geometry) = geometry {
                    geometry.collect(into, Some(index));
                }
            }
            Self::GeometryCollection { geometries } => {
                for member in geometries {
                    member.collect(into, feature);
                }
            }
            Self::Point { coordinates } => {
                if let Some(point) = position(&coordinates) {
                    let feature = into.owner(feature);
                    into.points.push(Shape {
                        feature,
                        geometry: point,
                    });
                }
            }
            Self::MultiPoint { coordinates } => {
                let points: Vec<LatLon> = coordinates.iter().filter_map(position).collect();
                if !points.is_empty() {
                    let feature = into.owner(feature);
                    into.points.extend(points.into_iter().map(|point| Shape {
                        feature,
                        geometry: point,
                    }));
                }
            }
            Self::LineString { coordinates } => push_line(into, feature, &coordinates),
            Self::MultiLineString { coordinates } => {
                for line in &coordinates {
                    push_line(into, feature, line);
                }
            }
            Self::Polygon { coordinates } => push_polygon(into, feature, &coordinates),
            Self::MultiPolygon { coordinates } => {
                for polygon in &coordinates {
                    push_polygon(into, feature, polygon);
                }
            }
        }
    }
}

/// A feature's `id`, as text. A number is written the way JSON wrote it;
/// anything else — an object, an array — is not an identifier and is dropped.
fn identifier(id: &serde_json::Value) -> Option<String> {
    match id {
        serde_json::Value::String(id) => Some(id.clone()),
        serde_json::Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

fn push_line(into: &mut GeoJson, feature: Option<usize>, positions: &[Position]) {
    let line: Vec<LatLon> = positions.iter().filter_map(position).collect();
    // A line of one point is a point that cannot be drawn, not a point.
    if line.len() >= 2 {
        let feature = into.owner(feature);
        into.lines.push(Shape {
            feature,
            geometry: line,
        });
    }
}

fn push_polygon(into: &mut GeoJson, feature: Option<usize>, rings: &[Vec<Position>]) {
    let rings: Vec<Vec<LatLon>> = rings.iter().filter_map(|ring| self::ring(ring)).collect();
    // Holes without an outer ring are not holes in anything. Because the outer
    // ring is first, a polygon that lost it would silently promote a hole.
    if let Some(outer) = rings.first()
        && !outer.is_empty()
    {
        let feature = into.owner(feature);
        into.polygons.push(Shape {
            feature,
            geometry: Polygon { rings },
        });
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

    /// The geometry of a shape list, without the feature indices.
    fn geometry<T: Clone>(shapes: &[Shape<T>]) -> Vec<T> {
        shapes.iter().map(|shape| shape.geometry.clone()).collect()
    }

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

        assert_eq!(parsed.features.len(), 2);
        assert_eq!(geometry(&parsed.points), vec![LatLon::new(37.8, -122.4)]);
        assert_eq!(parsed.lines.len(), 2);
        assert_eq!(parsed.lines[1].geometry.len(), 3);

        // The point belongs to the first feature and both strands of the
        // `MultiLineString` to the second, which is what makes picking either
        // strand highlight the whole of it.
        assert_eq!(parsed.points[0].feature, 0);
        assert_eq!(parsed.lines[0].feature, 1);
        assert_eq!(parsed.lines[1].feature, 1);
        assert_eq!(
            parsed.features[0].properties,
            serde_json::json!({"mag": 4.2})
        );
        assert_eq!(parsed.features[1].properties, serde_json::Value::Null);
    }

    #[test]
    fn an_id_arrives_as_text_whichever_way_it_was_written() {
        let parsed = GeoJson::parse(
            r#"{
                "type": "FeatureCollection",
                "features": [
                    {"type": "Feature", "id": "nc75096121", "geometry": null},
                    {"type": "Feature", "id": 42, "geometry": null},
                    {"type": "Feature", "id": {"nonsense": true}, "geometry": null},
                    {"type": "Feature", "geometry": null}
                ]
            }"#,
        )
        .expect("valid");

        let ids: Vec<Option<&str>> = parsed
            .features
            .iter()
            .map(|feature| feature.id.as_deref())
            .collect();
        assert_eq!(ids, vec![Some("nc75096121"), Some("42"), None, None]);
    }

    #[test]
    fn a_bare_geometry_is_a_document_too() {
        let parsed =
            GeoJson::parse(r#"{"type": "Point", "coordinates": [10, 20]}"#).expect("valid");
        assert_eq!(geometry(&parsed.points), vec![LatLon::new(20.0, 10.0)]);
        // Nothing wrapped it in a feature, so it is given one — otherwise the
        // point would be drawn and then not be pickable.
        assert_eq!(parsed.features.len(), 1);
        assert_eq!(parsed.features[0], Feature::default());
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
        // Two geometries outside any feature are two things to pick, not one.
        assert_eq!(parsed.features.len(), 2);
        assert_eq!(parsed.points[1].feature, 1);
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

        let polygon = &parsed.polygons[0].geometry;
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

        // Every feature is still counted — a feed reporting five earthquakes
        // reported five, whether or not each came with usable geometry.
        assert_eq!(parsed.features.len(), 5);
        assert_eq!(geometry(&parsed.points), vec![LatLon::new(6.0, 5.0)]);
        assert_eq!(parsed.points[0].feature, 4);
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
            geometry(&parsed.points),
            vec![LatLon::new(90.0, -170.0), LatLon::new(-90.0, 160.0)]
        );
    }
}
