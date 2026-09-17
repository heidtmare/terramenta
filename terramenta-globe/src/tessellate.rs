//! Turning a GeoJSON polygon into triangles.
//!
//! The work is done in the plane, in degrees of longitude and latitude, which
//! is the same plate-carrée mapping the imagery tiles are cut in — so a ring
//! triangulated flat and then lifted onto the sphere lands where the imagery
//! under it does. Two things follow from that and are worth knowing before
//! trusting a fill:
//!
//! * **The antimeridian is not a wall.** A ring that crosses 180° arrives with
//!   its longitudes flipping between `+179` and `-179`, which in the plane is a
//!   ring that sweeps the long way around the world. [`unwrap_ring`] undoes
//!   that by letting longitude run past ±180 — a ring over Fiji becomes one
//!   spanning 178° to 182° — and the sphere does not notice, because a
//!   longitude is an angle and 182° is 178° W.
//! * **A pole is.** A ring enclosing a pole does not close in this plane at
//!   all: it runs off one side of the map and comes back on the other with a
//!   discontinuity no unwrapping can remove. Antarctica as a single polygon
//!   fills wrong. Its outline, which does not go through here, is still right.
//!
//! The algorithm is ear clipping, with holes spliced into the outer ring by a
//! bridge first. It is quadratic, which is the reason for the ceilings below:
//! it runs once when a layer's data changes rather than per frame, but a
//! coastline dataset would still stall a frame for a noticeable time, so past a
//! point the fill is dropped and the outline left to carry the shape.

use bevy::math::DVec2;

use crate::features::{Coords, PolygonRef};

/// Past this many vertices in one polygon the fill is dropped entirely.
const MAX_FILL_VERTICES: usize = 8_000;
/// Past this many, holes are dropped and the outer ring is filled solid —
/// bridging searches every pair of vertices, so it gives out sooner than the
/// clipping does.
const MAX_BRIDGED_VERTICES: usize = 2_000;

/// Below this, a corner is treated as flat rather than as turning one way or
/// the other. Degrees squared, against coordinates in degrees.
const AREA_EPSILON: f64 = 1.0e-12;

/// Triangulates a polygon, returning its corners and the triangles over them.
///
/// Corners come back as `DVec2` in the plane this works in — longitude on `x`,
/// latitude on `y` — and at full precision, because the caller refines them
/// further before anything is narrowed onto a vertex.
///
/// The corners are the polygon's own, plus the duplicates that bridging a hole
/// into the outer ring introduces. Indices are into that list, three per
/// triangle.
///
/// Heights are dropped here: bridging duplicates corners and ear clipping
/// reorders them, and a fill is drawn at one height across the whole polygon
/// anyway — see [`crate::overlays`] — so the flat problem is the one worth
/// solving. A ring that climbs is drawn as a flat lid with its outline, which
/// does climb, around it. An empty result means the polygon was degenerate or past the
/// ceilings above — it is drawn as an outline either way, so this is a missing
/// fill rather than a missing shape.
pub fn triangulate(polygon: PolygonRef<'_>) -> (Vec<DVec2>, Vec<u32>) {
    let empty = (Vec::new(), Vec::new());

    let outer = polygon.outer();
    let total: usize = polygon.rings().map(|ring| ring.len()).sum();
    if outer.len() < 3 || total > MAX_FILL_VERTICES {
        return empty;
    }

    // Every ring of the polygon is unwrapped against the same meridian, so a
    // hole cannot end up a world away from the ring it is a hole in.
    let reference = outer.get(0).lon;
    let mut merged = unwrap_ring(outer, reference);
    orient(&mut merged, Winding::CounterClockwise);

    if total <= MAX_BRIDGED_VERTICES {
        // Rightmost first: a hole is bridged to the ring around it, and once a
        // hole has been spliced in it becomes part of that ring, so bridging
        // the outermost ones first keeps every later bridge a shorter reach.
        let mut holes: Vec<Vec<DVec2>> = polygon
            .holes()
            .filter(|hole| hole.len() >= 3)
            .map(|hole| {
                let mut hole = unwrap_ring(hole, reference);
                // A hole runs against its ring, so that the bridge leaves and
                // returns along the two sides of a single seam.
                orient(&mut hole, Winding::Clockwise);
                hole
            })
            .collect();
        holes.sort_by(|a, b| rightmost(b).total_cmp(&rightmost(a)));

        for index in 0..holes.len() {
            let (hole, pending) = (&holes[index], &holes[index + 1..]);
            merged = bridge(merged, hole, pending);
        }
    }

    let indices = ear_clip(&merged);
    (merged, indices)
}

/// Lets a ring's longitudes run continuously, past ±180 if that is what keeps
/// consecutive corners next to each other.
///
/// Every step is taken the short way: a jump of more than 180° between two
/// corners is read as the seam being crossed rather than as a ring that really
/// does sweep most of the way around the planet.
fn unwrap_ring(ring: Coords<'_>, reference: f64) -> Vec<DVec2> {
    let mut unwrapped = Vec::with_capacity(ring.len());
    let mut previous = reference;
    for corner in ring.iter() {
        let longitude = previous + shortest_turn(corner.lon - previous);
        unwrapped.push(DVec2::new(longitude, corner.lat));
        previous = longitude;
    }
    unwrapped
}

/// Brings an angle in degrees into `[-180, 180)`.
fn shortest_turn(degrees: f64) -> f64 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Winding {
    CounterClockwise,
    Clockwise,
}

/// Twice the signed area: positive counter-clockwise, negative clockwise.
fn signed_area(ring: &[DVec2]) -> f64 {
    let mut total = 0.0;
    for (index, current) in ring.iter().enumerate() {
        let next = ring[(index + 1) % ring.len()];
        total += current.x * next.y - next.x * current.y;
    }
    total
}

fn orient(ring: &mut [DVec2], winding: Winding) {
    let counter_clockwise = signed_area(ring) > 0.0;
    if counter_clockwise != (winding == Winding::CounterClockwise) {
        ring.reverse();
    }
}

fn rightmost(ring: &[DVec2]) -> f64 {
    ring.iter().fold(f64::MIN, |most, point| most.max(point.x))
}

/// Splices a hole into the ring around it along a bridge: a segment from one
/// ring corner to one hole corner that crosses nothing.
///
/// The pair is the closest one that can see each other, found by trying them
/// all. That is the expensive part, and the reason for [`MAX_BRIDGED_VERTICES`]
/// — the textbook alternative picks the hole's rightmost corner and casts a ray,
/// which is faster but has to then reason about which reflex corner the ray
/// really landed behind. Searching is slower and simply correct.
///
/// The seam is walked out and back, so the result is one ring again: up to the
/// bridge corner, around the whole hole, then back to where it left off.
fn bridge(ring: Vec<DVec2>, hole: &[DVec2], pending: &[Vec<DVec2>]) -> Vec<DVec2> {
    let mut best: Option<(f64, usize, usize)> = None;

    for (ring_index, &from) in ring.iter().enumerate() {
        for (hole_index, &to) in hole.iter().enumerate() {
            let distance = from.distance_squared(to);
            if best.is_some_and(|(best_distance, _, _)| distance >= best_distance) {
                continue;
            }
            // The bridge shares a corner with both rings, so touching them
            // there is expected; anything else in the way disqualifies it.
            let blocked = crosses(from, to, &ring, ring_index)
                || crosses(from, to, hole, hole_index)
                || pending
                    .iter()
                    .any(|other| crosses(from, to, other, usize::MAX));
            if !blocked {
                best = Some((distance, ring_index, hole_index));
            }
        }
    }

    // No bridge at all means the hole overlaps its ring, which is not a polygon
    // this can make sense of. The outer shape is still worth filling.
    let Some((_, ring_index, hole_index)) = best else {
        return ring;
    };

    let mut spliced = Vec::with_capacity(ring.len() + hole.len() + 2);
    spliced.extend_from_slice(&ring[..=ring_index]);
    spliced.extend(hole[hole_index..].iter().copied());
    spliced.extend(hole[..=hole_index].iter().copied());
    spliced.extend_from_slice(&ring[ring_index..]);
    spliced
}

/// Whether a segment crosses any edge of a ring, ignoring the two edges that
/// meet at `skip` — the corner the segment starts or ends on.
fn crosses(from: DVec2, to: DVec2, ring: &[DVec2], skip: usize) -> bool {
    let count = ring.len();
    (0..count).any(|index| {
        let next = (index + 1) % count;
        if index == skip || next == skip {
            return false;
        }
        segments_cross(from, to, ring[index], ring[next])
    })
}

/// Whether two segments properly cross — sharing an endpoint or merely touching
/// does not count, which is what lets a bridge land on a corner.
fn segments_cross(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> bool {
    let side = |p: DVec2, q: DVec2, r: DVec2| (q - p).perp_dot(r - p);
    let (abc, abd) = (side(a, b, c), side(a, b, d));
    let (cda, cdb) = (side(c, d, a), side(c, d, b));
    (abc > 0.0) != (abd > 0.0) && (cda > 0.0) != (cdb > 0.0)
}

/// Ear clipping over a simple, counter-clockwise ring.
///
/// A corner is an ear when it turns the same way the ring does and no other
/// corner sits inside the triangle it cuts off. Clipping one leaves a smaller
/// ring, and repeating until three corners remain triangulates the whole thing.
fn ear_clip(ring: &[DVec2]) -> Vec<u32> {
    if ring.len() < 3 {
        return Vec::new();
    }

    let mut remaining: Vec<u32> = (0..ring.len() as u32).collect();
    let mut triangles = Vec::with_capacity((ring.len() - 2) * 3);

    while remaining.len() > 3 {
        let count = remaining.len();
        let clipped = (0..count).find(|&position| {
            let corner = corner_at(&remaining, position);
            is_ear(ring, &remaining, corner)
        });

        // Nothing qualifies when the ring crosses itself, which real data does.
        // Cutting the flattest corner anyway keeps this terminating: the result
        // is a fill that is wrong in the way the data is, rather than a hang.
        let position = clipped.unwrap_or_else(|| flattest_corner(ring, &remaining));
        triangles.extend(corner_at(&remaining, position));
        remaining.remove(position);
    }

    triangles.extend(remaining);
    triangles
}

/// The three ring indices meeting at the corner in `position`.
fn corner_at(remaining: &[u32], position: usize) -> [u32; 3] {
    let count = remaining.len();
    [
        remaining[(position + count - 1) % count],
        remaining[position],
        remaining[(position + 1) % count],
    ]
}

fn is_ear(ring: &[DVec2], remaining: &[u32], [a, b, c]: [u32; 3]) -> bool {
    let (a, b, c) = (ring[a as usize], ring[b as usize], ring[c as usize]);
    let turn = (b - a).perp_dot(c - b);
    if turn <= AREA_EPSILON {
        // A reflex corner cuts off ground outside the ring; a flat one cuts off
        // nothing, and is left to the fallback so it cannot be mistaken for
        // real progress here.
        return false;
    }

    !remaining.iter().any(|&index| {
        let point = ring[index as usize];
        // Only a reflex corner can be the one trapped inside: a convex corner
        // inside the triangle would mean the ring crosses itself. This is also
        // what lets a bridge's doubled corners lie on an edge without blocking
        // every ear in the ring.
        point != a
            && point != b
            && point != c
            && is_reflex(ring, remaining, index)
            && inside(point, a, b, c)
    })
}

fn is_reflex(ring: &[DVec2], remaining: &[u32], index: u32) -> bool {
    let Some(position) = remaining.iter().position(|&other| other == index) else {
        return false;
    };
    let [a, b, c] = corner_at(remaining, position);
    let (a, b, c) = (ring[a as usize], ring[b as usize], ring[c as usize]);
    (b - a).perp_dot(c - b) <= 0.0
}

/// Whether a point is inside a counter-clockwise triangle, its edges included.
///
/// Inclusive on purpose. A reflex corner lying exactly *on* an edge is the case
/// an L-shape hits on its first cut — the diagonal of the L passes straight
/// through the corner of the notch — and a strict test calls that triangle an
/// ear, cuts it, and takes a bite out of the shape that was never in it.
fn inside(point: DVec2, a: DVec2, b: DVec2, c: DVec2) -> bool {
    (b - a).perp_dot(point - a) >= 0.0
        && (c - b).perp_dot(point - b) >= 0.0
        && (a - c).perp_dot(point - c) >= 0.0
}

/// The corner cutting off the least area, for when no real ear can be found.
fn flattest_corner(ring: &[DVec2], remaining: &[u32]) -> usize {
    (0..remaining.len())
        .min_by(|&left, &right| {
            area_at(ring, remaining, left).total_cmp(&area_at(ring, remaining, right))
        })
        .unwrap_or(0)
}

fn area_at(ring: &[DVec2], remaining: &[u32], position: usize) -> f64 {
    let [a, b, c] = corner_at(remaining, position);
    let (a, b, c) = (ring[a as usize], ring[b as usize], ring[c as usize]);
    (b - a).perp_dot(c - b).abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{FeatureSet, FeatureSetBuilder};
    use crate::geo::Position;

    /// A one-polygon set, so the tessellator can be handed the real thing the
    /// renderer hands it.
    fn polygon(rings: &[&[(f64, f64)]]) -> FeatureSet {
        let mut builder = FeatureSetBuilder::new();
        let feature = builder.feature(None, None);
        builder.push_polygon(
            feature,
            rings.iter().map(|ring| {
                ring.iter()
                    .map(|&(lon, lat)| Position::new(lat, lon, 0.0))
                    .collect::<Vec<_>>()
            }),
        );
        builder.finish()
    }

    /// Total area of the triangulation, in the plane it was built in.
    fn triangulated_area(set: &FeatureSet) -> f64 {
        let (corners, indices) = triangulate(set.polygon(0).1);
        assert_eq!(indices.len() % 3, 0);
        indices
            .chunks_exact(3)
            .map(|triangle| {
                let corner = |index: u32| corners[index as usize];
                let (a, b, c) = (
                    corner(triangle[0]),
                    corner(triangle[1]),
                    corner(triangle[2]),
                );
                (b - a).perp_dot(c - a).abs() * 0.5
            })
            .sum()
    }

    #[test]
    fn a_square_is_two_triangles() {
        let square = polygon(&[&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]]);
        let (_, indices) = triangulate(square.polygon(0).1);
        assert_eq!(indices.len(), 6);
        assert!((triangulated_area(&square) - 16.0).abs() < 1.0e-9);
    }

    #[test]
    fn a_clockwise_ring_triangulates_the_same() {
        let clockwise = polygon(&[&[(0.0, 0.0), (0.0, 4.0), (4.0, 4.0), (4.0, 0.0)]]);
        assert!((triangulated_area(&clockwise) - 16.0).abs() < 1.0e-9);
    }

    #[test]
    fn a_concave_ring_keeps_its_notch() {
        // An L: a 4x4 square with its top-right quadrant removed.
        let shape = polygon(&[&[
            (0.0, 0.0),
            (4.0, 0.0),
            (4.0, 2.0),
            (2.0, 2.0),
            (2.0, 4.0),
            (0.0, 4.0),
        ]]);
        assert!((triangulated_area(&shape) - 12.0).abs() < 1.0e-9);
    }

    #[test]
    fn a_hole_is_left_unfilled() {
        let with_hole = polygon(&[
            &[(0.0, 0.0), (6.0, 0.0), (6.0, 6.0), (0.0, 6.0)],
            &[(2.0, 2.0), (4.0, 2.0), (4.0, 4.0), (2.0, 4.0)],
        ]);
        // 36 for the square, less the 4 the hole takes out of it.
        assert!((triangulated_area(&with_hole) - 32.0).abs() < 1.0e-9);
    }

    #[test]
    fn two_holes_are_both_bridged() {
        let with_holes = polygon(&[
            &[(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)],
            &[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)],
            &[(6.0, 2.0), (8.0, 2.0), (8.0, 4.0), (6.0, 4.0)],
        ]);
        assert!((triangulated_area(&with_holes) - (60.0 - 8.0)).abs() < 1.0e-9);
    }

    #[test]
    fn a_ring_across_the_antimeridian_stays_small() {
        let fiji = polygon(&[&[
            (178.0, -18.0),
            (-178.0, -18.0),
            (-178.0, -16.0),
            (178.0, -16.0),
        ]]);
        let (corners, _) = triangulate(fiji.polygon(0).1);
        let span = corners
            .iter()
            .fold(f64::MIN, |most, corner| most.max(corner.x))
            - corners
                .iter()
                .fold(f64::MAX, |least, corner| least.min(corner.x));
        // Four degrees across, not the 356 a naive reading would give it.
        assert!(span < 5.0, "{span}");
        assert!((triangulated_area(&fiji) - 8.0).abs() < 1.0e-9);
    }

    #[test]
    fn a_degenerate_ring_produces_nothing_and_does_not_hang() {
        let collapsed = polygon(&[&[(0.0, 0.0), (1.0, 1.0), (2.0, 2.0), (3.0, 3.0)]]);
        assert!((triangulated_area(&collapsed)).abs() < 1.0e-9);
    }

    #[test]
    fn a_corner_is_placed_exactly_where_the_document_put_it() {
        // The tolerances above are `f64` tolerances rather than the `f32` ones
        // this file used to carry, and this is why: a corner goes through
        // unwrapping, orientation and ear clipping without losing a digit.
        let precise = polygon(&[&[
            (-122.419_415_998_3, 37.774_929_496_1),
            (-122.419_415_998_3, 37.874_929_496_1),
            (-122.319_415_998_3, 37.874_929_496_1),
        ]]);
        let (corners, _) = triangulate(precise.polygon(0).1);
        assert!(
            corners
                .iter()
                .any(|corner| corner.x == -122.419_415_998_3 && corner.y == 37.774_929_496_1)
        );
    }
}
