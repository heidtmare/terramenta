//! GeoJSON, as much of [RFC 7946](https://datatracker.ietf.org/doc/html/rfc7946)
//! as a globe has anything to draw with.
//!
//! The document is flattened as it is parsed, straight into the GeoArrow
//! buffers of a [`FeatureSet`]. A globe draws points, lines and filled rings; it
//! does not care whether a line arrived as a `LineString`, as one strand of a
//! `MultiLineString`, or nested three deep inside a `GeometryCollection` inside
//! a `Feature`. So the whole tree collapses into three arrays, which is exactly
//! what [`crate::overlays`] builds meshes from — and no intermediate tree of
//! `Vec`s is built on the way, because the reader writes coordinates into the
//! final buffers as it walks.
//!
//! What the flattening does *not* throw away is which feature each shape came
//! from. Every shape carries an index into the feature columns, and that is
//! what makes it pickable: the globe hit-tests the geometry, and the feature
//! behind the shape it hits is what an interface is handed, properties and all.
//! It is also why a `MultiPolygon` of forty islands highlights as one country
//! rather than as the island under the cursor.
//!
//! Parsing is deliberately forgiving about everything except the shape of the
//! document. A coordinate that is not a pair of finite numbers is dropped, a
//! ring with fewer than three distinct corners is dropped, and a geometry left
//! empty by either goes with them — a feed with one bad record is still worth
//! drawing. A `type` that is not a GeoJSON type, or coordinates nested to the
//! wrong depth, is an error: that is not one bad record, it is a document that
//! was never GeoJSON.
//!
//! Properties are kept exactly as they arrived, as the JSON text the store
//! holds them in, and handed back out untouched when a feature is picked.
//! Nothing here reads them: what a `mag` or a `place` means is the feed's
//! business and the interface's, not the globe's.
//!
//! They are written back out to text rather than lifted out of the source
//! unparsed, and that is a `serde` constraint rather than a choice: a GeoJSON
//! object is discriminated by its `type` member, which makes it an internally
//! tagged enum, and `serde` buffers the body of one of those before handing it
//! to the variant — which is exactly what a `RawValue` cannot survive. So the
//! cost is one round trip through `Value` per feature, paid once when a
//! document loads.

use serde::Deserialize;

use crate::features::{FeatureSet, FeatureSetBuilder};
use crate::geo::Position;

/// Parses a document. See the module docs for what is tolerated.
pub fn parse(text: &str) -> Result<FeatureSet, GeoJsonError> {
    let object: Object = serde_json::from_str(text)?;
    let mut builder = FeatureSetBuilder::new();
    object.collect(&mut builder, None);
    Ok(builder.finish())
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

/// One position, exactly as GeoJSON writes it: `[longitude, latitude]`, with an
/// optional third element. RFC 7946 calls that element elevation in metres, but
/// says so loosely enough that feeds disagree — the USGS earthquake feeds put
/// depth in kilometres there — so it is read as a number here and left for the
/// layer to interpret. See [`crate::overlays::OverlayAltitude`].
type RawPosition = Vec<f64>;

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
        /// Written straight back out as text on the way into the store — see
        /// the module docs for why it cannot simply be carried as one.
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
        coordinates: RawPosition,
    },
    MultiPoint {
        #[serde(default)]
        coordinates: Vec<RawPosition>,
    },
    LineString {
        #[serde(default)]
        coordinates: Vec<RawPosition>,
    },
    MultiLineString {
        #[serde(default)]
        coordinates: Vec<Vec<RawPosition>>,
    },
    Polygon {
        #[serde(default)]
        coordinates: Vec<Vec<RawPosition>>,
    },
    MultiPolygon {
        #[serde(default)]
        coordinates: Vec<Vec<Vec<RawPosition>>>,
    },
}

impl Object {
    /// Walks the document, adding what it finds to `into`.
    ///
    /// `feature` is the feature whatever is found belongs to: `Some` once a
    /// `Feature` has been entered, `None` above that. A geometry reached with
    /// `None` gets a feature of its own — see [`owner`] — so a bare `Point` of a
    /// document is as pickable as one inside a feed.
    fn collect(self, into: &mut FeatureSetBuilder, feature: Option<u32>) {
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
                let index = into.feature(
                    id.as_ref().and_then(identifier),
                    // A member that was absent, or explicitly `null`, is no
                    // properties — and storing the word for every feature of a
                    // large feed would be four bytes apiece to say so.
                    (!properties.is_null()).then(|| properties.to_string()),
                );
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
                    let feature = owner(into, feature);
                    into.push_point(feature, point);
                }
            }
            Self::MultiPoint { coordinates } => {
                let points: Vec<Position> = coordinates.iter().filter_map(position).collect();
                if !points.is_empty() {
                    let feature = owner(into, feature);
                    for point in points {
                        into.push_point(feature, point);
                    }
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

/// The feature a geometry belongs to, inventing one for a geometry that arrived
/// outside any — so that everything drawn is pickable and nothing has to
/// special-case a document written without features.
fn owner(into: &mut FeatureSetBuilder, feature: Option<u32>) -> u32 {
    feature.unwrap_or_else(|| into.feature(None, None))
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

fn push_line(into: &mut FeatureSetBuilder, feature: Option<u32>, positions: &[RawPosition]) {
    let line: Vec<Position> = positions.iter().filter_map(position).collect();
    // A line of one point is a point that cannot be drawn, not a point. Checked
    // before a feature is invented for it, so a document of nothing but
    // unusable lines does not come back full of empty features.
    if line.len() >= 2 {
        let feature = owner(into, feature);
        into.push_line(feature, line);
    }
}

fn push_polygon(into: &mut FeatureSetBuilder, feature: Option<u32>, rings: &[Vec<RawPosition>]) {
    let rings: Vec<Vec<Position>> = rings.iter().filter_map(|ring| self::ring(ring)).collect();
    // Holes without an outer ring are not holes in anything. Because the outer
    // ring is first, a polygon that lost it would silently promote a hole.
    if rings.first().is_some_and(|outer| outer.len() >= 3) {
        let feature = owner(into, feature);
        into.push_polygon(feature, rings);
    }
}

/// A linear ring, opened: the repeated closing position RFC 7946 requires is
/// dropped, and a ring left with fewer than three corners cannot bound an area.
fn ring(positions: &[RawPosition]) -> Option<Vec<Position>> {
    let mut ring: Vec<Position> = positions.iter().filter_map(position).collect();
    // By coordinate rather than by position: a ring that closes at a different
    // height is still a ring closing on itself, and keeping the repeat would
    // leave a zero-length edge for the outline to step off.
    let closes = match (ring.first(), ring.last()) {
        (Some(first), Some(last)) => first.lat == last.lat && first.lon == last.lon,
        _ => false,
    };
    if ring.len() >= 2 && closes {
        ring.pop();
    }
    (ring.len() >= 3).then_some(ring)
}

/// Reads one position, or `None` for anything that is not a usable coordinate.
///
/// Nothing is narrowed: the numbers JSON wrote are the numbers the store holds,
/// which is the whole reason the coordinate buffers are `f64`. The third
/// element is optional and, unlike the first two, not worth dropping a record
/// over: a height that is not a finite number is no height, which is what most
/// positions have anyway.
fn position(position: &RawPosition) -> Option<Position> {
    let (&longitude, &latitude) = (position.first()?, position.get(1)?);
    if !longitude.is_finite() || !latitude.is_finite() {
        return None;
    }
    let altitude = position
        .get(2)
        .copied()
        .filter(|altitude| altitude.is_finite())
        .unwrap_or(0.0);
    Some(Position::new(
        // A latitude past the pole is meaningless rather than wrong-by-a-turn,
        // so it is clamped; a longitude past the antimeridian is the same place
        // said the long way round, so it is wrapped.
        latitude.clamp(-90.0, 90.0),
        wrap_longitude(longitude),
        altitude,
    ))
}

/// Brings a longitude into `[-180, 180)`.
fn wrap_longitude(longitude: f64) -> f64 {
    (longitude + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A position on the ground, which most of these are.
    fn at(lat: f64, lon: f64) -> Position {
        Position::new(lat, lon, 0.0)
    }

    /// Every point of a set, without the feature indices.
    fn points(set: &FeatureSet) -> Vec<Position> {
        set.points().map(|(_, point)| point).collect()
    }

    #[test]
    fn a_feature_collection_flattens_to_its_geometry() {
        let parsed = parse(
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

        assert_eq!(parsed.feature_count(), 2);
        // The third element is kept as it was written. What -8000 *means* is
        // the layer's to say: this feed counts depth, and a layer reading it as
        // metres up would have to be told to clamp.
        assert_eq!(points(&parsed), vec![Position::new(37.8, -122.4, -8000.0)]);
        assert_eq!(parsed.line_count(), 2);
        assert_eq!(parsed.line(1).1.len(), 3);

        // The point belongs to the first feature and both strands of the
        // `MultiLineString` to the second, which is what makes picking either
        // strand highlight the whole of it.
        assert_eq!(parsed.point(0).0, 0);
        assert_eq!(parsed.line(0).0, 1);
        assert_eq!(parsed.line(1).0, 1);
        assert_eq!(
            parsed.feature_properties(0),
            serde_json::json!({"mag": 4.2})
        );
        assert_eq!(parsed.feature_properties(1), serde_json::Value::Null);
    }

    #[test]
    fn an_id_arrives_as_text_whichever_way_it_was_written() {
        let parsed = parse(
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

        let ids: Vec<Option<&str>> = (0..parsed.feature_count())
            .map(|index| parsed.feature_id(index))
            .collect();
        assert_eq!(ids, vec![Some("nc75096121"), Some("42"), None, None]);
    }

    #[test]
    fn a_bare_geometry_is_a_document_too() {
        let parsed = parse(r#"{"type": "Point", "coordinates": [10, 20]}"#).expect("valid");
        assert_eq!(points(&parsed), vec![at(20.0, 10.0)]);
        // Nothing wrapped it in a feature, so it is given one — otherwise the
        // point would be drawn and then not be pickable.
        assert_eq!(parsed.feature_count(), 1);
        assert_eq!(parsed.feature_id(0), None);
        assert_eq!(parsed.feature_properties(0), serde_json::Value::Null);
    }

    #[test]
    fn geometry_collections_nest() {
        let parsed = parse(
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
        assert_eq!(parsed.point_count(), 2);
        // Two geometries outside any feature are two things to pick, not one.
        assert_eq!(parsed.feature_count(), 2);
        assert_eq!(parsed.point(1).0, 1);
    }

    #[test]
    fn rings_are_opened_and_holes_kept_in_order() {
        let parsed = parse(
            r#"{
                "type": "Polygon",
                "coordinates": [
                    [[0, 0], [4, 0], [4, 4], [0, 4], [0, 0]],
                    [[1, 1], [2, 1], [2, 2], [1, 2], [1, 1]]
                ]
            }"#,
        )
        .expect("valid");

        let (_, polygon) = parsed.polygon(0);
        assert_eq!(polygon.outer().len(), 4);
        assert_eq!(polygon.holes().count(), 1);
        assert_eq!(polygon.holes().next().expect("a hole").len(), 4);
    }

    #[test]
    fn one_bad_record_does_not_take_the_document_with_it() {
        let parsed = parse(
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
        assert_eq!(parsed.feature_count(), 5);
        assert_eq!(points(&parsed), vec![at(6.0, 5.0)]);
        assert_eq!(parsed.point(0).0, 4);
        assert_eq!(parsed.line_count(), 0);
        assert_eq!(parsed.polygon_count(), 0);
    }

    #[test]
    fn a_document_that_was_never_geojson_is_an_error() {
        assert!(parse("not json at all").is_err());
        assert!(parse(r#"{"type": "Raster", "coordinates": []}"#).is_err());
        assert!(parse(r#"{"type": "Point", "coordinates": [[1, 2]]}"#).is_err());
    }

    #[test]
    fn a_third_element_is_kept_as_a_height() {
        let parsed = parse(r#"{"type": "MultiPoint", "coordinates": [[10, 20, 1500], [11, 21]]}"#)
            .expect("valid");
        assert_eq!(
            points(&parsed),
            vec![Position::new(20.0, 10.0, 1500.0), at(21.0, 11.0)]
        );
    }

    #[test]
    fn a_fourth_element_is_not_ours_to_read() {
        // RFC 7946 leaves anything past the third to whoever wrote the
        // document, and the third is read as it stands — no longer narrowed to
        // an `f32`, so a height no longer has a ceiling to fall off.
        let parsed =
            parse(r#"{"type": "Point", "coordinates": [10, 20, 1e30, 4]}"#).expect("valid");
        assert_eq!(points(&parsed), vec![Position::new(20.0, 10.0, 1.0e30)]);
    }

    #[test]
    fn a_coordinate_keeps_every_digit_it_was_written_with() {
        // Eleven significant figures: past the seven an `f32` holds, which on
        // the ground is the difference between a building and its street.
        let parsed = parse(r#"{"type": "Point", "coordinates": [-122.4194159983, 37.7749294961]}"#)
            .expect("valid");
        assert_eq!(
            points(&parsed),
            vec![Position::new(37.774_929_496_1, -122.419_415_998_3, 0.0)]
        );
    }

    #[test]
    fn a_ring_closing_at_another_height_is_still_closed() {
        let parsed = parse(
            r#"{"type": "Polygon", "coordinates":
                [[[0, 0, 100], [1, 0, 200], [1, 1, 300], [0, 0, 400]]]}"#,
        )
        .expect("valid");
        // Three corners left, not four: the repeat went, height and all.
        let (_, polygon) = parsed.polygon(0);
        assert_eq!(polygon.outer().len(), 3);
        assert_eq!(polygon.outer().get(0).altitude_m, 100.0);
    }

    #[test]
    fn coordinates_are_wrapped_and_clamped() {
        let parsed = parse(r#"{"type": "MultiPoint", "coordinates": [[190, 95], [-200, -95]]}"#)
            .expect("valid");
        assert_eq!(points(&parsed), vec![at(90.0, -170.0), at(-90.0, 160.0)]);
    }
}
