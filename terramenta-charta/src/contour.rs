//! Contour lines out of a scalar field on a regular grid — marching squares,
//! the same algorithm every porkchop plot's delta-v contours are drawn with.
//!
//! This is deliberately generic on the field rather than tied to
//! [`crate::grid::PorkchopGrid`]: it takes plain coordinate axes and a
//! row-major grid of `Option<f64>` — [`PorkchopGrid::total_delta_v_km_s`]
//! is one such grid, but so is the departure- or arrival-only half of it, if
//! a caller wants those contoured separately.
//!
//! [`PorkchopGrid::total_delta_v_km_s`]: crate::grid::PorkchopGrid::total_delta_v_km_s

/// One point on a contour line, in the same units as the `xs`/`ys` axes
/// passed to [`contours`] — for a porkchop plot, Unix seconds on both axes,
/// so a caller can place a point directly on a date scale with no unit
/// conversion.
pub type Point = (f64, f64);

/// Every line segment [`contours`] found at one field value.
///
/// Segments are left unstitched into polylines: marching squares produces
/// them one grid cell at a time with no ordering between cells, and a porkchop
/// plot's contours are typically drawn as a soup of short strokes rather than
/// continuous paths anyway, so joining them costs more than it is worth here.
/// A caller that wants single paths can still chain segments whose endpoints
/// match to within its own tolerance.
#[derive(Debug, Clone, PartialEq)]
pub struct Contour {
    pub value: f64,
    pub segments: Vec<(Point, Point)>,
}

/// Traces every level in `values` at each value in `levels`, over a grid
/// `values.len() == xs.len() * ys.len()` wide, row-major in `ys` (`values[row
/// * xs.len() + col]` is the sample at `(xs[col], ys[row])`).
///
/// A grid cell contributes no segment wherever any of its four corners is
/// `None` — the unsolved half of a porkchop grid stays a gap in the contours
/// rather than an invented boundary. `xs` and `ys` must each have at least 2
/// entries and be monotonically increasing; anything smaller returns no
/// contours.
pub fn contours(xs: &[f64], ys: &[f64], values: &[Option<f64>], levels: &[f64]) -> Vec<Contour> {
    let (width, height) = (xs.len(), ys.len());
    if width < 2 || height < 2 || values.len() != width * height {
        return Vec::new();
    }

    levels
        .iter()
        .map(|&level| Contour {
            value: level,
            segments: trace_level(xs, ys, values, level),
        })
        .collect()
}

fn trace_level(xs: &[f64], ys: &[f64], values: &[Option<f64>], level: f64) -> Vec<(Point, Point)> {
    let width = xs.len();
    let mut segments = Vec::new();

    for row in 0..ys.len() - 1 {
        for col in 0..width - 1 {
            let corners = [
                values[row * width + col],
                values[row * width + col + 1],
                values[(row + 1) * width + col + 1],
                values[(row + 1) * width + col],
            ];
            let Some(corners) = corners[0]
                .zip(corners[1])
                .zip(corners[2])
                .zip(corners[3])
                .map(|(((a, b), c), d)| [a, b, c, d])
            else {
                continue;
            };

            let points = [
                (xs[col], ys[row]),
                (xs[col + 1], ys[row]),
                (xs[col + 1], ys[row + 1]),
                (xs[col], ys[row + 1]),
            ];

            segments.extend(marching_square(corners, points, level));
        }
    }

    segments
}

/// One grid cell's contribution at `level`, given its four corner values and
/// positions in winding order (top-left, top-right, bottom-right,
/// bottom-left).
///
/// Corners above `level` are bit 0..3; the resulting 4-bit case selects which
/// pair of edges the contour crosses, by the standard marching-squares table.
/// Cases 5 and 10 are the ambiguous saddles — which diagonal pair connects
/// depends on whether the cell's centre reads above or below `level` — and
/// every other case has exactly one answer.
fn marching_square(corners: [f64; 4], points: [Point; 4], level: f64) -> Vec<(Point, Point)> {
    // Edges in corner-index order: 0-1 (top), 1-2 (right), 2-3 (bottom), 3-0
    // (left).
    let edges = [(0, 1), (1, 2), (2, 3), (3, 0)];
    let crossing = |edge: (usize, usize)| -> Point {
        let (a, b) = edge;
        interpolate(points[a], corners[a], points[b], corners[b], level)
    };

    let case = corners
        .iter()
        .enumerate()
        .fold(0u8, |case, (index, &value)| {
            case | ((value > level) as u8) << index
        });

    let pair = |first: (usize, usize), second: (usize, usize)| {
        vec![(crossing(edges[first.0]), crossing(edges[first.1]))]
            .into_iter()
            .chain(if second == (usize::MAX, usize::MAX) {
                None
            } else {
                Some((crossing(edges[second.0]), crossing(edges[second.1])))
            })
            .collect::<Vec<_>>()
    };
    const NONE: (usize, usize) = (usize::MAX, usize::MAX);

    match case {
        0 | 15 => Vec::new(),
        1 | 14 => pair((3, 0), NONE),
        2 | 13 => pair((0, 1), NONE),
        3 | 12 => pair((3, 1), NONE),
        4 | 11 => pair((1, 2), NONE),
        6 | 9 => pair((0, 2), NONE),
        7 | 8 => pair((3, 2), NONE),
        5 => {
            // Corners 0 and 2 are the ones above `level`; connects to corner
            // 1's side of the saddle unless the average of all four corners
            // reads above `level`, in which case the high region is actually
            // joined the other way.
            if corners.iter().sum::<f64>() / 4.0 > level {
                pair((0, 1), (2, 3))
            } else {
                pair((3, 0), (1, 2))
            }
        }
        10 => {
            if corners.iter().sum::<f64>() / 4.0 > level {
                pair((3, 0), (1, 2))
            } else {
                pair((0, 1), (2, 3))
            }
        }
        _ => unreachable!("case is a 4-bit value, all 16 of which are handled above"),
    }
}

/// Where the field crosses `level` between two corners, by linear
/// interpolation along the straight edge between them.
fn interpolate(a: Point, a_value: f64, b: Point, b_value: f64, level: f64) -> Point {
    if (a_value - b_value).abs() < f64::EPSILON {
        return a;
    }
    let fraction = (level - a_value) / (b_value - a_value);
    (a.0 + (b.0 - a.0) * fraction, a.1 + (b.1 - a.1) * fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A perfect cone `value = x + y`, so the contour at any level is exactly
    /// the line `x + y = level` — easy to check every crossing against by
    /// hand.
    fn plane(xs: &[f64], ys: &[f64]) -> Vec<Option<f64>> {
        ys.iter()
            .flat_map(|&y| xs.iter().map(move |&x| Some(x + y)))
            .collect()
    }

    #[test]
    fn a_level_outside_the_field_has_no_segments() {
        let xs = [0.0, 1.0, 2.0];
        let ys = [0.0, 1.0, 2.0];
        let values = plane(&xs, &ys);
        let found = contours(&xs, &ys, &values, &[100.0]);
        assert_eq!(found[0].segments.len(), 0);
    }

    #[test]
    fn a_level_through_a_plane_crosses_every_cell_it_actually_passes_through() {
        let xs = [0.0, 1.0, 2.0];
        let ys = [0.0, 1.0, 2.0];
        let values = plane(&xs, &ys);
        // x + y = 1.5 cuts through 3 of the 2x2 grid's 4 cells; the fourth
        // (the corner where every sample is already 2 or more) never dips
        // below the level at all.
        let found = contours(&xs, &ys, &values, &[1.5]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].value, 1.5);
        assert_eq!(found[0].segments.len(), 3);
        for (a, b) in &found[0].segments {
            for point in [a, b] {
                assert!((point.0 + point.1 - 1.5).abs() < 1.0e-9, "{point:?}");
            }
        }
    }

    #[test]
    fn a_hole_in_the_field_leaves_a_gap_rather_than_a_boundary() {
        let xs = [0.0, 1.0, 2.0];
        let ys = [0.0, 1.0, 2.0];
        let mut values = plane(&xs, &ys);
        // Punch out the centre sample: every cell touching it should
        // contribute nothing, even though the plane's own value there would
        // have crossed the level.
        values[1 * xs.len() + 1] = None;
        let found = contours(&xs, &ys, &values, &[2.0]);
        assert_eq!(found[0].segments.len(), 0);
    }

    #[test]
    fn too_small_an_axis_has_no_contours() {
        assert_eq!(
            contours(&[0.0], &[0.0, 1.0], &[Some(0.0), Some(1.0)], &[0.5]).len(),
            0
        );
    }
}
