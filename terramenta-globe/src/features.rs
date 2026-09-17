//! The globe's memory layout for vector geometry: [GeoArrow].
//!
//! Everything that arrives as vector data — a GeoJSON document, a Mapbox Vector
//! Tile — is flattened into one [`FeatureSet`]: three GeoArrow arrays of
//! points, lines and polygons, a column saying which feature each shape belongs
//! to, and two columns of per-feature attributes. It is the single shape the
//! mesh builders, the hit test and the control surface all read.
//!
//! Three properties are the whole reason for it, and each is worth stating
//! plainly because each one costs something elsewhere.
//!
//! **The coordinates are contiguous and they are `f64`.** A GeoArrow polygon
//! array is three buffers — a flat run of interleaved `x, y, z` doubles, an
//! offset per ring into it, and an offset per polygon into the rings — rather
//! than a `Vec<Vec<Vec<Position>>>` of separately heap-allocated rings. A layer
//! of ten thousand country outlines is six allocations instead of ten thousand,
//! walking it is a linear scan, and every coordinate keeps the full precision
//! the source wrote. Nothing is narrowed until a vertex reaches the GPU, which
//! is the one place a narrowing is forced. See [`crate::geo::Position`].
//!
//! **Those buffers cross the WebAssembly boundary without being copied.** A
//! `Float64Array` in JavaScript can be a view straight onto the module's own
//! linear memory, so an embedder — or a worker it hands the buffer to — reads
//! the same bytes the renderer is drawing from. [`crate::wasm`] is where that
//! view is handed out, and where the one rule that comes with it is written
//! down. That is only possible because the layout is flat: a tree of `Vec`s has
//! no bytes to point at.
//!
//! **Interleaved rather than separated coordinates.** GeoArrow allows either —
//! `xyzxyzxyz` in one buffer, or three buffers of `xxx`, `yyy`, `zzz` — and
//! separated is the crate's default. This uses interleaved, because every
//! consumer here walks a shape vertex by vertex reading all three values at
//! once (a mesh builder, the hit test, the tessellator), so one cache line
//! carrying a whole coordinate beats three streams carrying a third of one
//! each. It is also the layout that hands out as a single typed array.
//!
//! What this is *not* is a general geometry library. There is no
//! `MultiLineString` and no `GeometryCollection` here: a globe draws points,
//! lines and filled rings, and the readers flatten everything else into those
//! three on the way in — a `MultiPolygon` of forty islands becomes forty
//! polygons that all name the same feature. That is what makes picking one
//! island highlight the whole country.
//!
//! Attributes are two `StringArray`s rather than a parsed tree: the `id`
//! member, and the `properties` object as the JSON text it arrived as. Nothing
//! here reads them — what a `mag` or a `place` means is the feed's business and
//! the interface's — so holding them as text keeps a tile of five thousand
//! features to two allocations, hands them across the boundary with the
//! geometry, and costs one parse when a feature is actually picked.
//!
//! [GeoArrow]: https://geoarrow.org

use std::sync::Arc;

use arrow_array::{Array, StringArray, UInt32Array};
use arrow_buffer::{OffsetBuffer, ScalarBuffer};
use geoarrow_array::array::{
    CoordBuffer, InterleavedCoordBuffer, LineStringArray, PointArray, PolygonArray,
};
use geoarrow_schema::{Crs, Dimension, Metadata};

use crate::geo::Position;
use crate::simplestyle::SimpleStyle;

/// How many doubles one coordinate takes: longitude, latitude, height.
///
/// Three rather than two because a GeoJSON position may carry a third element
/// and the globe draws with it — see [`crate::overlays::OverlayAltitude`]. A
/// position that gave no height stores a zero, which is what it means.
const STRIDE: usize = 3;

/// The dimension every array here is built in, matching [`STRIDE`].
const DIMENSION: Dimension = Dimension::XYZ;

/// What the coordinates mean: longitude and latitude in degrees on WGS 84,
/// which is what GeoJSON mandates and what every tile scheme unprojects to.
///
/// No `Edges` is declared, and that is deliberate rather than an omission.
/// Declaring spherical edges would say that the segment between two vertices is
/// a great circle, and the globe does not draw it that way — a line is
/// interpolated in latitude and longitude, and the hit test measures it the
/// same way, so that what is picked is what is on screen. See
/// [`crate::picking`].
fn metadata() -> Arc<Metadata> {
    Arc::new(Metadata::new(
        Crs::from_authority_code("OGC:CRS84".to_string()),
        None,
    ))
}

/// The flattened geometry of one document or one tile, and the features it
/// belongs to.
///
/// Built through [`FeatureSetBuilder`]; read through the accessors below, which
/// borrow straight out of the Arrow buffers and copy nothing.
#[derive(Debug, Clone)]
pub struct FeatureSet {
    /// The `id` member of each feature, if it had one. GeoJSON allows a string
    /// or a number and says nothing about what either means, so both arrive
    /// here as text.
    ids: StringArray,
    /// Each feature's `properties`, as the JSON text it arrived as. Null where
    /// there was none.
    properties: StringArray,
    /// The simplestyle members read off those properties, for the features that
    /// carried any — see [`crate::simplestyle`].
    ///
    /// The one thing here that is *read* rather than carried, and so the one
    /// thing that is not a column: it is empty for every vector tile and for
    /// every document that styles nothing, and for a document that styles a
    /// little it is only as long as it has to be — a set whose fourth feature
    /// is the last styled one holds four entries, not one per feature. Short is
    /// the normal case, which is why the accessor answers past the end rather
    /// than indexing.
    styles: Vec<SimpleStyle>,

    /// Every `Point`, and every position of every `MultiPoint`.
    points: PointArray,
    /// Every `LineString`, and every strand of every `MultiLineString`.
    lines: LineStringArray,
    /// Every `Polygon`, and every member of every `MultiPolygon`: an outer
    /// ring, then one ring per hole in it.
    polygons: PolygonArray,

    /// Which feature each shape belongs to — an index into `ids` and
    /// `properties`. Its own column rather than a field on a shape, because
    /// that is what keeps the geometry buffers pure coordinates.
    point_owners: UInt32Array,
    line_owners: UInt32Array,
    polygon_owners: UInt32Array,
}

impl Default for FeatureSet {
    fn default() -> Self {
        FeatureSetBuilder::new().finish()
    }
}

impl FeatureSet {
    /// How many `Feature`s the document held, in the order it held them —
    /// which is what a feed means by "how many earthquakes". The geometry below
    /// is a list of *shapes*, and one feature can be many of those.
    pub fn feature_count(&self) -> usize {
        self.ids.len()
    }

    pub fn point_count(&self) -> usize {
        self.point_owners.len()
    }

    pub fn line_count(&self) -> usize {
        self.line_owners.len()
    }

    pub fn polygon_count(&self) -> usize {
        self.polygon_owners.len()
    }

    /// One feature's GeoJSON `id`, if it had one.
    pub fn feature_id(&self, feature: usize) -> Option<&str> {
        (feature < self.ids.len() && self.ids.is_valid(feature)).then(|| self.ids.value(feature))
    }

    /// Whether any feature of this set styled itself.
    pub fn has_styles(&self) -> bool {
        !self.styles.is_empty()
    }

    /// Whether any of those styles changes how something is *drawn*, which is
    /// what decides whether the meshes built from this set have to carry paint
    /// per vertex at all. A document whose only style member is a `title` has
    /// styles and paints nothing.
    ///
    /// Scanned rather than remembered: it is asked once per mesh built, three
    /// times a layer, over a vector that is usually empty and never longer than
    /// the feature list — beside triangulating the same document, it is free.
    pub fn styles_paint(&self) -> bool {
        self.styles.iter().any(SimpleStyle::paints)
    }

    /// How many features style their own *appearance*.
    ///
    /// Features that paint, not features that carry a member: a feed whose
    /// every feature has a `title` — the USGS earthquake feeds are exactly
    /// that — carries simplestyle on all of them and is still drawn entirely in
    /// the layer's colours, so counting those would have an interface offer a
    /// switch that does nothing. The titles reach a pick either way.
    pub fn styled_features(&self) -> usize {
        self.styles.iter().filter(|style| style.paints()).count()
    }

    /// One feature's simplestyle members, if it carried any.
    pub fn feature_style(&self, feature: usize) -> Option<&SimpleStyle> {
        self.styles.get(feature)
    }

    /// One feature's `properties`, parsed.
    ///
    /// Parsed here rather than held parsed, because this is asked once when a
    /// feature is picked and never while anything is being drawn. A document
    /// that wrote something unparseable — which is to say, never, since the
    /// text came out of a parser — reads as `null` rather than failing a pick.
    pub fn feature_properties(&self, feature: usize) -> serde_json::Value {
        if feature >= self.properties.len() || self.properties.is_null(feature) {
            return serde_json::Value::Null;
        }
        serde_json::from_str(self.properties.value(feature)).unwrap_or(serde_json::Value::Null)
    }

    /// The feature one point belongs to, and where it is.
    pub fn point(&self, index: usize) -> (u32, Position) {
        let coords = interleaved(self.points.coords());
        let at = index * STRIDE;
        (
            self.point_owners.value(index),
            position(&coords[at..at + STRIDE]),
        )
    }

    /// Every point, with the feature each belongs to.
    pub fn points(&self) -> impl ExactSizeIterator<Item = (u32, Position)> + '_ {
        (0..self.point_count()).map(|index| self.point(index))
    }

    /// The feature one line belongs to, and its vertices.
    pub fn line(&self, index: usize) -> (u32, Coords<'_>) {
        let offsets: &[i32] = self.lines.geom_offsets();
        let (from, to) = (offsets[index] as usize, offsets[index + 1] as usize);
        (
            self.line_owners.value(index),
            Coords::new(&interleaved(self.lines.coords())[from * STRIDE..to * STRIDE]),
        )
    }

    pub fn lines(&self) -> impl ExactSizeIterator<Item = (u32, Coords<'_>)> + '_ {
        (0..self.line_count()).map(|index| self.line(index))
    }

    /// The feature one polygon belongs to, and its rings.
    pub fn polygon(&self, index: usize) -> (u32, PolygonRef<'_>) {
        let geom_offsets: &[i32] = self.polygons.geom_offsets();
        (
            self.polygon_owners.value(index),
            PolygonRef {
                coords: interleaved(self.polygons.coords()),
                ring_offsets: self.polygons.ring_offsets(),
                first_ring: geom_offsets[index] as usize,
                ring_count: (geom_offsets[index + 1] - geom_offsets[index]) as usize,
            },
        )
    }

    pub fn polygons(&self) -> impl ExactSizeIterator<Item = (u32, PolygonRef<'_>)> + '_ {
        (0..self.polygon_count()).map(|index| self.polygon(index))
    }

    /// Which feature each shape belongs to, as the column it is stored as —
    /// which is how a caller that only wants to know *whether* a feature is
    /// drawn reads that without touching a coordinate. See
    /// [`crate::picking::kind_of`].
    pub fn point_owners(&self) -> &[u32] {
        self.point_owners.values()
    }

    pub fn line_owners(&self) -> &[u32] {
        self.line_owners.values()
    }

    pub fn polygon_owners(&self) -> &[u32] {
        self.polygon_owners.values()
    }
}

// ---------------------------------------------------------------------------
// The buffers themselves
// ---------------------------------------------------------------------------

/// The coordinates and offsets themselves, which is what an embedder is handed
/// a view onto. These are the GeoArrow buffers exactly as they are stored;
/// nothing is assembled to answer them, which is the point — see
/// [`crate::wasm::overlay_geometry`].
///
/// Nothing but the WebAssembly binding asks for a whole buffer: the renderer
/// and the hit test go through the accessors above, which hand back one shape
/// at a time. So on a native build these are dead code in the literal sense and
/// live code in every other one.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
impl FeatureSet {
    /// Every point's `x, y, z`, in order.
    pub fn point_coords(&self) -> &[f64] {
        interleaved(self.points.coords())
    }

    /// Every line's vertices, end to end.
    pub fn line_coords(&self) -> &[f64] {
        interleaved(self.lines.coords())
    }

    /// Where each line starts and ends in [`FeatureSet::line_coords`], as a
    /// count of coordinates rather than of doubles. One longer than the number
    /// of lines, which is how an Arrow offset buffer says where the last one
    /// ends.
    pub fn line_offsets(&self) -> &[i32] {
        self.lines.geom_offsets()
    }

    /// Every polygon ring's vertices, end to end, outer ring of each first.
    pub fn polygon_coords(&self) -> &[f64] {
        interleaved(self.polygons.coords())
    }

    /// Where each ring starts in [`FeatureSet::polygon_coords`].
    pub fn ring_offsets(&self) -> &[i32] {
        self.polygons.ring_offsets()
    }

    /// Where each polygon's rings start in [`FeatureSet::ring_offsets`].
    pub fn polygon_offsets(&self) -> &[i32] {
        self.polygons.geom_offsets()
    }
}

/// The vertices of one line or one ring: a window onto the coordinate buffer,
/// three doubles per vertex.
///
/// `Copy` and free to make, so it is passed around the way a slice would be —
/// which is what it is.
#[derive(Debug, Clone, Copy)]
pub struct Coords<'a> {
    values: &'a [f64],
}

impl<'a> Coords<'a> {
    fn new(values: &'a [f64]) -> Self {
        Self { values }
    }

    /// An empty run, for a shape that is not there.
    pub const EMPTY: Coords<'static> = Coords { values: &[] };

    pub fn len(&self) -> usize {
        self.values.len() / STRIDE
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// # Panics
    ///
    /// If `index` is past the end, exactly as indexing a slice does.
    pub fn get(&self, index: usize) -> Position {
        let at = index * STRIDE;
        position(&self.values[at..at + STRIDE])
    }

    /// The vertices in order. `Clone`, because more than one pass over a ring
    /// is the normal case — the hit test measures a ring's centre before it
    /// measures its reach.
    pub fn iter(self) -> impl ExactSizeIterator<Item = Position> + Clone + 'a {
        let values = self.values;
        (0..values.len() / STRIDE).map(move |index| position(&values[index * STRIDE..][..STRIDE]))
    }

    /// The vertex at `index`, wrapping — which is what walking a ring's edges
    /// wants, since a ring is stored open and its last edge closes it.
    pub fn wrapping(&self, index: usize) -> Position {
        self.get(index % self.len())
    }
}

/// One polygon: an outer ring, then one ring per hole in it.
///
/// Rings are stored open — the closing position RFC 7946 requires is dropped on
/// the way in, because every consumer here would otherwise have to drop it
/// again.
#[derive(Debug, Clone, Copy)]
pub struct PolygonRef<'a> {
    coords: &'a [f64],
    ring_offsets: &'a [i32],
    first_ring: usize,
    ring_count: usize,
}

impl<'a> PolygonRef<'a> {
    pub fn ring_count(&self) -> usize {
        self.ring_count
    }

    /// # Panics
    ///
    /// If `index` is past [`PolygonRef::ring_count`].
    pub fn ring(&self, index: usize) -> Coords<'a> {
        assert!(index < self.ring_count, "ring {index} is past the polygon");
        let ring = self.first_ring + index;
        let (from, to) = (
            self.ring_offsets[ring] as usize,
            self.ring_offsets[ring + 1] as usize,
        );
        Coords::new(&self.coords[from * STRIDE..to * STRIDE])
    }

    pub fn rings(&self) -> impl ExactSizeIterator<Item = Coords<'a>> + Clone + '_ {
        (0..self.ring_count()).map(|index| self.ring(index))
    }

    /// The ring that bounds the polygon. Empty for a polygon with no rings at
    /// all, which the builder does not produce but a slice of one could.
    pub fn outer(&self) -> Coords<'a> {
        if self.ring_count == 0 {
            Coords::EMPTY
        } else {
            self.ring(0)
        }
    }

    pub fn holes(&self) -> impl ExactSizeIterator<Item = Coords<'a>> + Clone + '_ {
        (1..self.ring_count()).map(|index| self.ring(index))
    }

    /// Every vertex of every ring, which is what a bounding test wants.
    pub fn vertices(&self) -> impl Iterator<Item = Position> + Clone + '_ {
        self.rings().flat_map(|ring| ring.iter())
    }
}

/// Reads one coordinate out of the buffer.
fn position(coord: &[f64]) -> Position {
    Position::new(coord[1], coord[0], coord[2])
}

/// The interleaved buffer behind a coordinate array.
///
/// Every array here is built interleaved — see the module docs — so the other
/// arm is unreachable rather than a case to handle. It is written as an empty
/// slice rather than a panic because an empty layer and a layer that cannot be
/// read should both simply draw nothing.
fn interleaved(coords: &CoordBuffer) -> &[f64] {
    match coords {
        CoordBuffer::Interleaved(buffer) => buffer.coords(),
        CoordBuffer::Separated(_) => &[],
    }
}

// ---------------------------------------------------------------------------
// Building one
// ---------------------------------------------------------------------------

/// Accumulates a [`FeatureSet`] as a reader walks a document.
///
/// Geometry goes straight into the flat buffers as it is read, so a document is
/// never materialized as a tree of `Vec`s on the way to being one that is not.
///
/// A feature is pushed before the geometry that belongs to it, and the index it
/// comes back as is what that geometry is filed under. That is what lets a
/// reader hold a feature back until it turns out to have geometry worth keeping
/// — see [`crate::mvt`], where most of a tile's features are clipped away.
#[derive(Debug, Default)]
pub struct FeatureSetBuilder {
    ids: Vec<Option<String>>,
    properties: Vec<Option<String>>,
    styles: Vec<SimpleStyle>,

    point_coords: Vec<f64>,
    point_owners: Vec<u32>,

    line_coords: Vec<f64>,
    line_offsets: Vec<i32>,
    line_owners: Vec<u32>,

    polygon_coords: Vec<f64>,
    ring_offsets: Vec<i32>,
    polygon_offsets: Vec<i32>,
    polygon_owners: Vec<u32>,
}

impl FeatureSetBuilder {
    pub fn new() -> Self {
        Self {
            // An Arrow offset buffer opens with the zero that says where the
            // first shape starts, so an empty one is `[0]` rather than empty.
            line_offsets: vec![0],
            ring_offsets: vec![0],
            polygon_offsets: vec![0],
            ..Self::default()
        }
    }

    /// Adds a feature and returns the index its geometry is filed under.
    ///
    /// `properties` is the `properties` object as JSON text, exactly as it
    /// arrived; `None` for a feature that had none.
    pub fn feature(&mut self, id: Option<String>, properties: Option<String>) -> u32 {
        self.ids.push(id);
        self.properties.push(properties);
        (self.ids.len() - 1) as u32
    }

    /// The same, for a feature whose properties turned out to hold simplestyle
    /// members. An empty style is not stored, so the column stays absent for
    /// the documents — and the formats — that never style anything.
    pub fn styled_feature(
        &mut self,
        id: Option<String>,
        properties: Option<String>,
        style: SimpleStyle,
    ) -> u32 {
        let feature = self.feature(id, properties);
        if !style.is_empty() {
            // Pads over the unstyled features in between, so the position in
            // this vector is the feature index. Only ever as far as the last
            // styled feature: see the field.
            self.styles
                .resize_with(feature as usize, SimpleStyle::default);
            self.styles.push(style);
        }
        feature
    }

    pub fn push_point(&mut self, feature: u32, position: Position) {
        push_coord(&mut self.point_coords, position);
        self.point_owners.push(feature);
    }

    /// Adds a line. A line of fewer than two vertices is one that cannot be
    /// drawn, and is dropped rather than stored; the return says which it was.
    pub fn push_line(
        &mut self,
        feature: u32,
        vertices: impl IntoIterator<Item = Position>,
    ) -> bool {
        let before = self.line_coords.len();
        for vertex in vertices {
            push_coord(&mut self.line_coords, vertex);
        }
        if (self.line_coords.len() - before) / STRIDE < 2 {
            self.line_coords.truncate(before);
            return false;
        }
        self.line_offsets
            .push((self.line_coords.len() / STRIDE) as i32);
        self.line_owners.push(feature);
        true
    }

    /// Adds a polygon, outer ring first. A ring of fewer than three corners
    /// cannot bound an area and is dropped; a polygon left without a usable
    /// outer ring is dropped whole, because a hole is not a hole in anything.
    ///
    /// The return says whether the polygon was kept.
    pub fn push_polygon<I, R>(&mut self, feature: u32, rings: I) -> bool
    where
        I: IntoIterator<Item = R>,
        R: IntoIterator<Item = Position>,
    {
        let (coords_before, rings_before) = (self.polygon_coords.len(), self.ring_offsets.len());
        let mut kept = 0;

        for ring in rings {
            let before = self.polygon_coords.len();
            for vertex in ring {
                push_coord(&mut self.polygon_coords, vertex);
            }
            // The outer ring is first, so a polygon that lost it would silently
            // promote a hole. Dropping the whole polygon is the only honest
            // answer, and the rollback below is what makes that possible after
            // the coordinates are already in the buffer.
            if (self.polygon_coords.len() - before) / STRIDE < 3 {
                self.polygon_coords.truncate(before);
                if kept == 0 {
                    break;
                }
                continue;
            }
            self.ring_offsets
                .push((self.polygon_coords.len() / STRIDE) as i32);
            kept += 1;
        }

        if kept == 0 {
            self.polygon_coords.truncate(coords_before);
            self.ring_offsets.truncate(rings_before);
            return false;
        }

        self.polygon_offsets
            .push((self.ring_offsets.len() - 1) as i32);
        self.polygon_owners.push(feature);
        true
    }

    pub fn finish(self) -> FeatureSet {
        let metadata = metadata();
        let coords = |values: Vec<f64>| {
            CoordBuffer::Interleaved(InterleavedCoordBuffer::new(
                ScalarBuffer::from(values),
                DIMENSION,
            ))
        };
        let offsets = |values: Vec<i32>| OffsetBuffer::new(ScalarBuffer::from(values));

        FeatureSet {
            ids: StringArray::from(self.ids),
            properties: StringArray::from(self.properties),
            styles: self.styles,
            points: PointArray::new(coords(self.point_coords), None, metadata.clone()),
            lines: LineStringArray::new(
                coords(self.line_coords),
                offsets(self.line_offsets),
                None,
                metadata.clone(),
            ),
            polygons: PolygonArray::new(
                coords(self.polygon_coords),
                offsets(self.polygon_offsets),
                offsets(self.ring_offsets),
                None,
                metadata,
            ),
            point_owners: UInt32Array::from(self.point_owners),
            line_owners: UInt32Array::from(self.line_owners),
            polygon_owners: UInt32Array::from(self.polygon_owners),
        }
    }
}

fn push_coord(into: &mut Vec<f64>, position: Position) {
    into.extend_from_slice(&[position.lon, position.lat, position.altitude_m]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(lat: f64, lon: f64) -> Position {
        Position::new(lat, lon, 0.0)
    }

    #[test]
    fn an_empty_set_is_readable_rather_than_absent() {
        let empty = FeatureSet::default();
        assert_eq!(empty.feature_count(), 0);
        assert_eq!(empty.point_count(), 0);
        assert_eq!(empty.line_count(), 0);
        assert_eq!(empty.polygon_count(), 0);
        assert!(empty.line_coords().is_empty());
        // The offset buffers still open with the zero Arrow requires.
        assert_eq!(empty.line_offsets(), &[0]);
        assert_eq!(empty.polygon_offsets(), &[0]);
    }

    #[test]
    fn coordinates_land_in_one_contiguous_buffer() {
        let mut builder = FeatureSetBuilder::new();
        let feature = builder.feature(Some("a".into()), Some(r#"{"mag":4.2}"#.into()));
        builder.push_point(feature, Position::new(20.0, 10.0, 1500.0));
        builder.push_line(feature, [at(0.0, 0.0), at(1.0, 1.0), at(2.0, 2.0)]);
        let set = builder.finish();

        // Longitude, latitude, height — GeoJSON's order, which is GeoArrow's.
        assert_eq!(set.point_coords(), &[10.0, 20.0, 1500.0]);
        assert_eq!(set.line_coords().len(), 9);
        assert_eq!(set.line_offsets(), &[0, 3]);
        assert_eq!(set.point_owners(), &[0]);

        let (owner, point) = set.point(0);
        assert_eq!(owner, 0);
        assert_eq!(point, Position::new(20.0, 10.0, 1500.0));
        assert_eq!(set.feature_id(0), Some("a"));
        assert_eq!(set.feature_properties(0), serde_json::json!({"mag": 4.2}));
    }

    #[test]
    fn precision_survives_the_round_trip() {
        // A coordinate with more significant digits than an `f32` can hold: at
        // `f32` the last four would be lost, which on the ground is metres.
        let exact = Position::new(37.774_929_496_1, -122.419_415_998_3, 0.0);
        let mut builder = FeatureSetBuilder::new();
        let feature = builder.feature(None, None);
        builder.push_point(feature, exact);
        let set = builder.finish();
        assert_eq!(set.point(0).1, exact);
    }

    #[test]
    fn a_polygon_keeps_its_rings_in_order() {
        let mut builder = FeatureSetBuilder::new();
        let feature = builder.feature(None, None);
        assert!(builder.push_polygon(
            feature,
            [
                vec![at(0.0, 0.0), at(0.0, 4.0), at(4.0, 4.0), at(4.0, 0.0)],
                vec![at(1.0, 1.0), at(1.0, 2.0), at(2.0, 2.0)],
            ],
        ));
        let set = builder.finish();

        let (_, polygon) = set.polygon(0);
        assert_eq!(polygon.ring_count(), 2);
        assert_eq!(polygon.outer().len(), 4);
        assert_eq!(polygon.holes().count(), 1);
        assert_eq!(polygon.holes().next().expect("a hole").len(), 3);
        assert_eq!(polygon.outer().get(1), at(0.0, 4.0));
        // A ring is stored open, so walking its edges wraps.
        assert_eq!(polygon.outer().wrapping(4), at(0.0, 0.0));
    }

    #[test]
    fn a_shape_too_small_to_draw_is_dropped_whole() {
        let mut builder = FeatureSetBuilder::new();
        let feature = builder.feature(None, None);
        assert!(!builder.push_line(feature, [at(0.0, 0.0)]));
        assert!(!builder.push_polygon(feature, [vec![at(0.0, 0.0), at(1.0, 1.0)]]));
        // A hole without a usable outer ring takes the polygon with it.
        assert!(!builder.push_polygon(
            feature,
            [
                vec![at(0.0, 0.0), at(1.0, 1.0)],
                vec![at(1.0, 1.0), at(1.0, 2.0), at(2.0, 2.0)],
            ],
        ));

        let set = builder.finish();
        assert_eq!(set.line_count(), 0);
        assert_eq!(set.polygon_count(), 0);
        // Nothing half-written was left behind in the buffers.
        assert!(set.line_coords().is_empty());
        assert!(set.polygon_coords().is_empty());
        assert_eq!(set.ring_offsets(), &[0]);
        // The feature is still counted: a feed reporting one reported one.
        assert_eq!(set.feature_count(), 1);
    }

    #[test]
    fn a_hole_that_is_too_small_does_not_take_its_polygon_with_it() {
        let mut builder = FeatureSetBuilder::new();
        let feature = builder.feature(None, None);
        assert!(builder.push_polygon(
            feature,
            [
                vec![at(0.0, 0.0), at(0.0, 4.0), at(4.0, 4.0)],
                vec![at(1.0, 1.0), at(1.0, 2.0)],
            ],
        ));
        let set = builder.finish();
        assert_eq!(set.polygon(0).1.ring_count(), 1);
    }

    #[test]
    fn a_feature_with_no_properties_reads_as_null() {
        let mut builder = FeatureSetBuilder::new();
        builder.feature(None, None);
        let set = builder.finish();
        assert_eq!(set.feature_id(0), None);
        assert_eq!(set.feature_properties(0), serde_json::Value::Null);
        // And so does one that is not there at all.
        assert_eq!(set.feature_properties(7), serde_json::Value::Null);
    }
}
