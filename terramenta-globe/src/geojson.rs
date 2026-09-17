//! GeoJSON, as much of [RFC 7946](https://datatracker.ietf.org/doc/html/rfc7946)
//! as a globe has anything to draw with.
//!
//! The document is read by [`geozero`], the same decoder [`crate::mvt`] runs a
//! vector tile through, and for the same reason: a reader that walks a document
//! and calls back for every ring, strand and point can write straight into the
//! GeoArrow buffers of a [`FeatureSet`] without building an intermediate tree of
//! `Vec`s on the way. What this module adds is the sink — [`Collector`] — and
//! everything a globe wants that a general-purpose decoder has no opinion on.
//!
//! **The document is flattened as it is walked.** A globe draws points, lines
//! and filled rings; it does not care whether a line arrived as a `LineString`,
//! as one strand of a `MultiLineString`, or nested three deep inside a
//! `GeometryCollection` inside a `Feature`. So the whole tree collapses into
//! three arrays, which is exactly what [`crate::overlays`] builds meshes from.
//!
//! What the flattening does *not* throw away is which feature each shape came
//! from. Every shape carries an index into the feature columns, and that is
//! what makes it pickable: the globe hit-tests the geometry, and the feature
//! behind the shape it hits is what an interface is handed, properties and all.
//! It is also why a `MultiPolygon` of forty islands highlights as one country
//! rather than as the island under the cursor.
//!
//! **Why the identifiers are read twice.** `geozero`'s feature callbacks carry
//! a feature's properties and its position in the collection, and nothing else
//! — there is no hook for a GeoJSON `id`. So the identifiers are scanned off
//! the text up front and matched to features by position, which is exactly what
//! [`crate::mvt`] does with the ids of a tile layer for exactly the same reason.
//! The scan deserialises two members and skips the rest of the document, so it
//! costs a pass over the bytes and no allocation to speak of.
//!
//! **What is tolerated and what is not.** A ring with fewer than three distinct
//! corners is dropped, a line left with fewer than two positions is dropped,
//! and a polygon that lost its outer ring goes with them — a feed with one
//! unusable shape is still worth drawing. Everything else is the decoder's
//! judgement, and the decoder holds to the specification: a position of fewer
//! than two numbers, a coordinate array nested to the wrong depth, a `type`
//! that is not a GeoJSON type, or an `id` that is neither a string nor a number
//! is a document that was never GeoJSON, and the whole of it is refused.
//!
//! Properties are kept as the JSON text the store holds them in, rebuilt from
//! the values the decoder reports, and handed back out untouched when a feature
//! is picked. What a `mag` or a `place` means is the feed's business and the
//! interface's, not the globe's. One detail of the round trip is the decoder's:
//! a property whose value is `null` is not reported, so it does not survive
//! into the store — which is the same thing the store already does with a
//! feature whose whole `properties` member is `null`.
//!
//! **Ten of those properties are read on the way past.** A document is allowed
//! to say how it wants to look, in the members of [simplestyle-spec 1.1.0], and
//! those are picked out as each feature's properties arrive — see
//! [`crate::simplestyle`] for what they are and what is done with them. They
//! are read off the values the decoder has already produced, and left in the
//! properties as well, so a feed that styles itself is neither parsed twice nor
//! handed to an interface with members missing. Nothing else in `properties` is
//! looked at, here or anywhere else.
//!
//! [simplestyle-spec 1.1.0]: https://github.com/mapbox/simplestyle-spec/tree/master/1.1.0

use geozero::{
    ColumnValue, CoordDimensions, FeatureProcessor, GeomProcessor, GeozeroDatasource,
    PropertyProcessor, geojson::GeoJson,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::features::{FeatureSet, FeatureSetBuilder};
use crate::geo::Position;
use crate::simplestyle::SimpleStyle;

/// Parses a document. See the module docs for what is tolerated.
pub fn parse(text: &str) -> Result<FeatureSet, GeoJsonError> {
    let mut builder = FeatureSetBuilder::new();
    let mut collector = Collector::new(identifiers(text), &mut builder);
    GeoJson(text)
        .process(&mut collector)
        .map_err(|error| GeoJsonError::Malformed(error.to_string()))?;
    Ok(builder.finish())
}

/// Why a document could not be read.
#[derive(Debug)]
pub enum GeoJsonError {
    /// Not JSON, or not a JSON document shaped like GeoJSON.
    Malformed(String),
}

impl std::fmt::Display for GeoJsonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The decoder reports the line and column of a JSON failure, which
            // is the part that makes a parse failure actionable.
            Self::Malformed(error) => write!(formatter, "not valid GeoJSON: {error}"),
        }
    }
}

impl std::error::Error for GeoJsonError {}

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Just enough of a document to find the `id` of every feature in it.
///
/// Both members default, so this reads a `FeatureCollection`, a bare `Feature`
/// and a bare geometry without distinguishing between them: a document with no
/// `features` member and no `id` simply has no identifiers in it.
#[derive(Debug, Default, Deserialize)]
struct IdScan {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    features: Vec<IdScan>,
}

/// Every feature's identifier, in the order the decoder will report them.
///
/// Anything that is not a document comes back empty rather than as an error:
/// the decoder is the one that judges a document, and it is about to.
fn identifiers(text: &str) -> Vec<Option<String>> {
    let Ok(scan) = serde_json::from_str::<IdScan>(text) else {
        return Vec::new();
    };
    if scan.features.is_empty() {
        // A bare `Feature` is feature zero of its own document.
        vec![scan.id.as_ref().and_then(identifier)]
    } else {
        scan.features
            .iter()
            .map(|feature| feature.id.as_ref().and_then(identifier))
            .collect()
    }
}

/// A feature's `id`, as text. A number is written the way JSON wrote it;
/// anything else is not an identifier the specification allows, and the decoder
/// will refuse the document over it.
fn identifier(id: &Value) -> Option<String> {
    match id {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The sink
// ---------------------------------------------------------------------------

/// A feature read off the document, before it has a row of its own.
#[derive(Debug, Default)]
struct PendingFeature {
    id: Option<String>,
    /// `None` until the decoder announces a `properties` member, so that a
    /// feature that had none is told apart from one whose properties were `{}`.
    properties: Option<Map<String, Value>>,
}

/// Turns the decoder's walk of a document into rows and coordinates.
///
/// Geometry is accumulated a strand at a time, because the two things that
/// happen to a strand — opening a ring, and dropping one too short to draw —
/// can only be decided once the whole of it is in hand.
struct Collector<'a> {
    /// Identifiers by feature position; see the module docs for why they are
    /// not simply carried along by the walk.
    ids: Vec<Option<String>>,
    /// The feature being read, held until it is given a row.
    pending: Option<PendingFeature>,
    /// The row every shape found right now belongs to.
    owner: Option<u32>,
    /// Whether the walk is inside a `Feature`, which is what decides where the
    /// next shape's row comes from: the feature's, or one invented for it.
    in_feature: bool,
    /// The positions of the strand, ring or point group being read.
    current: Vec<Position>,
    /// The rings of the polygon being read, outer first.
    rings: Vec<Vec<Position>>,
    /// How deep inside a polygon the walk is, which is what tells a ring from a
    /// line: the decoder announces both as linestrings.
    polygon_depth: u32,
    into: &'a mut FeatureSetBuilder,
}

impl<'a> Collector<'a> {
    fn new(ids: Vec<Option<String>>, into: &'a mut FeatureSetBuilder) -> Self {
        Self {
            ids,
            pending: None,
            owner: None,
            in_feature: false,
            current: Vec::new(),
            rings: Vec::new(),
            polygon_depth: 0,
            into,
        }
    }

    /// Gives the feature being read a row, with whatever has arrived for it.
    fn commit(&mut self) -> u32 {
        let pending = self.pending.take().unwrap_or_default();
        // Storing the word `null` for every feature of a large feed would be
        // four bytes apiece to say the feature had no properties.
        // Read before the object is turned back into text, because the
        // decoder has already done the work of turning the members into values
        // and a second parse of the same document would buy nothing. See
        // [`crate::simplestyle`] for why this is the one thing in `properties`
        // the globe reads.
        let style = pending
            .properties
            .as_ref()
            .map(SimpleStyle::read)
            .unwrap_or_default();
        let properties = pending
            .properties
            .map(|properties| Value::Object(properties).to_string());
        let owner = self.into.styled_feature(pending.id, properties, style);
        self.owner = Some(owner);
        owner
    }

    /// The row the shape in hand belongs to, inventing a feature for a geometry
    /// that arrived outside any — so that everything drawn is pickable and
    /// nothing has to special-case a document written without features.
    fn owner(&mut self) -> u32 {
        match self.owner {
            Some(owner) => owner,
            None => self.commit(),
        }
    }

    /// Ends a geometry that stood on its own, so the next one gets its own
    /// feature rather than joining this one's. Inside a `Feature` there is
    /// nothing to end: every shape of it belongs to the one row.
    fn released(&mut self) {
        if !self.in_feature {
            self.owner = None;
        }
    }

    /// Adds one strand, if there is enough of it left to draw.
    ///
    /// A line of one position is a point that cannot be drawn, not a point.
    /// Checked before a feature is invented for it, so a document of nothing
    /// but unusable lines does not come back full of empty features.
    fn push_line(&mut self, line: Vec<Position>) {
        if line.len() >= 2 {
            let feature = self.owner();
            self.into.push_line(feature, line);
        }
        self.released();
    }

    /// Adds one ring to the polygon being read, opened: the repeated closing
    /// position RFC 7946 requires is dropped, and a ring left with fewer than
    /// three corners cannot bound an area.
    fn push_ring(&mut self, mut ring: Vec<Position>) {
        // By coordinate rather than by position: a ring that closes at a
        // different height is still a ring closing on itself, and keeping the
        // repeat would leave a zero-length edge for the outline to step off.
        let closes = match (ring.first(), ring.last()) {
            (Some(first), Some(last)) => first.lat == last.lat && first.lon == last.lon,
            _ => false,
        };
        if ring.len() >= 2 && closes {
            ring.pop();
        }
        if ring.len() >= 3 {
            self.rings.push(ring);
        }
    }

    /// Closes off the polygon being read.
    fn finish_polygon(&mut self) {
        let rings = std::mem::take(&mut self.rings);
        // Holes without an outer ring are not holes in anything. Because the
        // outer ring is first, a polygon that lost it would silently promote a
        // hole. Checked here rather than left to the builder, because inventing
        // the feature is what the check is guarding against.
        if rings.first().is_some_and(|outer| outer.len() >= 3) {
            let feature = self.owner();
            self.into.push_polygon(feature, rings);
        }
        self.released();
    }
}

impl GeomProcessor for Collector<'_> {
    /// The third element of a position is wanted, which is what makes the
    /// decoder report coordinates through [`Self::coordinate`] rather than
    /// through `xy`.
    fn dimensions(&self) -> CoordDimensions {
        CoordDimensions::xyz()
    }

    fn xy(&mut self, x: f64, y: f64, idx: usize) -> geozero::error::Result<()> {
        self.coordinate(x, y, None, None, None, None, idx)
    }

    /// Reads one position, dropping anything that is not a usable coordinate.
    ///
    /// Nothing is narrowed: the numbers JSON wrote are the numbers the store
    /// holds, which is the whole reason the coordinate buffers are `f64`. The
    /// height is optional and, unlike the other two, not worth dropping a
    /// position over: a height that is not a finite number is no height, which
    /// is what most positions have anyway.
    fn coordinate(
        &mut self,
        x: f64,
        y: f64,
        z: Option<f64>,
        _m: Option<f64>,
        _t: Option<f64>,
        _tm: Option<u64>,
        _idx: usize,
    ) -> geozero::error::Result<()> {
        if !x.is_finite() || !y.is_finite() {
            return Ok(());
        }
        let altitude = z.filter(|altitude| altitude.is_finite()).unwrap_or(0.0);
        self.current.push(Position::new(
            // A latitude past the pole is meaningless rather than
            // wrong-by-a-turn, so it is clamped; a longitude past the
            // antimeridian is the same place said the long way round, so it is
            // wrapped.
            y.clamp(-90.0, 90.0),
            wrap_longitude(x),
            altitude,
        ));
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
        let points = std::mem::take(&mut self.current);
        if !points.is_empty() {
            let feature = self.owner();
            for point in points {
                self.into.push_point(feature, point);
            }
        }
        self.released();
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

impl PropertyProcessor for Collector<'_> {
    fn property(
        &mut self,
        _idx: usize,
        name: &str,
        value: &ColumnValue<'_>,
    ) -> geozero::error::Result<bool> {
        let converted = match value {
            ColumnValue::Bool(value) => Value::Bool(*value),
            // An object or an array is reported as the JSON text of it, and
            // goes back into the store as the object or array it was: a feed
            // that nested its properties should not have them handed to an
            // interface as a string it has to parse a second time.
            ColumnValue::Json(value) => {
                serde_json::from_str(value).unwrap_or_else(|_| Value::String((*value).to_string()))
            }
            ColumnValue::String(value) | ColumnValue::DateTime(value) => {
                Value::String((*value).to_string())
            }
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
            // JSON has no binary type, so this is unreachable rather than lossy.
            ColumnValue::Binary(_) => Value::Null,
        };

        if let Some(properties) = self
            .pending
            .as_mut()
            .and_then(|feature| feature.properties.as_mut())
        {
            properties.insert(name.to_string(), converted);
        }
        // `false` would stop the walk; every property is wanted.
        Ok(true)
    }
}

impl FeatureProcessor for Collector<'_> {
    fn feature_begin(&mut self, idx: u64) -> geozero::error::Result<()> {
        self.pending = Some(PendingFeature {
            id: self.ids.get(idx as usize).cloned().flatten(),
            ..PendingFeature::default()
        });
        self.owner = None;
        self.in_feature = true;
        self.current.clear();
        self.rings.clear();
        self.polygon_depth = 0;
        Ok(())
    }

    fn properties_begin(&mut self) -> geozero::error::Result<()> {
        if let Some(feature) = self.pending.as_mut() {
            feature.properties = Some(Map::new());
        }
        Ok(())
    }

    /// The row is made here rather than at the end of the feature, because the
    /// geometry that follows has to be able to point at it — and because the
    /// decoder reports properties before geometry, everything the row is made
    /// of has already arrived.
    fn geometry_begin(&mut self) -> geozero::error::Result<()> {
        self.commit();
        Ok(())
    }

    fn feature_end(&mut self, _idx: u64) -> geozero::error::Result<()> {
        // A feature whose geometry was absent or `null` still happened: a feed
        // reporting five earthquakes reported five, whether or not each came
        // with somewhere to draw it.
        if self.owner.is_none() {
            self.commit();
        }
        self.owner = None;
        self.pending = None;
        self.in_feature = false;
        Ok(())
    }
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
    fn nested_properties_survive_the_round_trip() {
        // The decoder reports an object or an array as JSON text; a feed that
        // nested its properties gets them back as what it wrote, not as a
        // string an interface would have to parse again.
        let parsed = parse(
            r#"{
                "type": "Feature",
                "properties": {"place": {"name": "Ridgecrest"}, "felt": [1, 2], "tsunami": null},
                "geometry": null
            }"#,
        )
        .expect("valid");
        // A `null` property is not reported by the decoder, so it does not
        // reach the store — the same thing that happens to a feature whose
        // whole `properties` member is `null`.
        assert_eq!(
            parsed.feature_properties(0),
            serde_json::json!({"place": {"name": "Ridgecrest"}, "felt": [1, 2]})
        );
    }

    #[test]
    fn an_empty_properties_member_is_not_no_properties() {
        let parsed =
            parse(r#"{"type": "Feature", "properties": {}, "geometry": null}"#).expect("valid");
        assert_eq!(parsed.feature_properties(0), serde_json::json!({}));
    }

    #[test]
    fn an_id_arrives_as_text_whichever_way_it_was_written() {
        let parsed = parse(
            r#"{
                "type": "FeatureCollection",
                "features": [
                    {"type": "Feature", "id": "nc75096121", "geometry": null},
                    {"type": "Feature", "id": 42, "geometry": null},
                    {"type": "Feature", "geometry": null}
                ]
            }"#,
        )
        .expect("valid");

        let ids: Vec<Option<&str>> = (0..parsed.feature_count())
            .map(|index| parsed.feature_id(index))
            .collect();
        assert_eq!(ids, vec![Some("nc75096121"), Some("42"), None]);
    }

    #[test]
    fn simplestyle_members_are_read_and_still_handed_on() {
        let parsed = parse(
            r##"{
                "type": "FeatureCollection",
                "features": [
                    {"type": "Feature",
                     "properties": {"title": "Route", "stroke": "#ff0000", "stroke-width": 6},
                     "geometry": {"type": "LineString", "coordinates": [[0, 0], [1, 1]]}},
                    {"type": "Feature",
                     "properties": {"mag": 4.2},
                     "geometry": {"type": "Point", "coordinates": [2, 3]}}
                ]
            }"##,
        )
        .expect("valid");

        let style = parsed.feature_style(0).expect("a style");
        assert_eq!(style.title.as_deref(), Some("Route"));
        assert_eq!(style.stroke_width, Some(6.0));
        // Read, not consumed: an interface still gets the whole object, and a
        // feed that keeps other data beside its styling keeps all of it.
        assert_eq!(
            parsed.feature_properties(0),
            serde_json::json!({"title": "Route", "stroke": "#ff0000", "stroke-width": 6})
        );

        // A feature that styled nothing carries nothing, and the column is only
        // as long as it has to be.
        assert!(parsed.feature_style(1).is_none());
        assert_eq!(parsed.styled_features(), 1);
        assert!(parsed.styles_paint());
    }

    #[test]
    fn a_document_that_styles_nothing_carries_no_styles_at_all() {
        let parsed = parse(
            r#"{"type": "Feature", "properties": {"mag": 4.2},
                "geometry": {"type": "Point", "coordinates": [0, 0]}}"#,
        )
        .expect("valid");
        assert!(!parsed.has_styles());
        assert!(!parsed.styles_paint());
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
    fn a_multipolygon_is_one_feature_when_a_feature_owns_it() {
        // Forty islands highlight as one country: every polygon of the
        // `MultiPolygon` points back at the feature that held it.
        let parsed = parse(
            r#"{
                "type": "Feature",
                "properties": {"name": "Japan"},
                "geometry": {"type": "MultiPolygon", "coordinates": [
                    [[[0, 0], [1, 0], [1, 1], [0, 0]]],
                    [[[5, 5], [6, 5], [6, 6], [5, 5]]]
                ]}
            }"#,
        )
        .expect("valid");
        assert_eq!(parsed.feature_count(), 1);
        assert_eq!(parsed.polygon_count(), 2);
        assert_eq!(parsed.polygon(0).0, 0);
        assert_eq!(parsed.polygon(1).0, 0);
    }

    #[test]
    fn one_unusable_shape_does_not_take_the_document_with_it() {
        let parsed = parse(
            r#"{
                "type": "FeatureCollection",
                "features": [
                    {"type": "Feature", "geometry": null},
                    {"type": "Feature", "geometry": {"type": "LineString", "coordinates": [[0, 0]]}},
                    {"type": "Feature",
                     "geometry": {"type": "Polygon", "coordinates": [[[0, 0], [1, 0], [0, 0]]]}},
                    {"type": "Feature", "geometry": {"type": "Point", "coordinates": [5, 6]}}
                ]
            }"#,
        )
        .expect("valid");

        // Every feature is still counted — a feed reporting four earthquakes
        // reported four, whether or not each came with usable geometry.
        assert_eq!(parsed.feature_count(), 4);
        assert_eq!(points(&parsed), vec![at(6.0, 5.0)]);
        assert_eq!(parsed.point(0).0, 3);
        assert_eq!(parsed.line_count(), 0);
        assert_eq!(parsed.polygon_count(), 0);
    }

    #[test]
    fn a_document_that_was_never_geojson_is_an_error() {
        assert!(parse("not json at all").is_err());
        assert!(parse(r#"{"type": "Raster", "coordinates": []}"#).is_err());
        // Coordinates nested to the wrong depth, and a position that is not a
        // position: the specification allows neither, and the decoder holds to
        // the specification rather than guessing what was meant.
        assert!(parse(r#"{"type": "Point", "coordinates": [[1, 2]]}"#).is_err());
        assert!(parse(r#"{"type": "Point", "coordinates": []}"#).is_err());
        // An `id` is a string or a number, and nothing else is one.
        assert!(
            parse(r#"{"type": "Feature", "id": {"nonsense": true}, "geometry": null}"#).is_err()
        );
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
