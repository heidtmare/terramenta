//! The porkchop plot's data: total delta-v (or either half of it) as a
//! function of departure date and arrival date, for every pair of dates in
//! two ranges.
//!
//! A porkchop plot is that grid read directly rather than summarized —
//! Lambert's problem has no closed form for "the best date to leave", so a
//! mission designer instead solves it everywhere a launch could plausibly
//! happen and reads the cheapest region off the result. [`porkchop_grid`] is
//! exactly [`terramenta_solare::mission::plan_transfer`] run at every
//! (departure, arrival) pair on two linearly spaced date axes; nothing here
//! is specific to Earth and Mars, or to the Sun as the transfer's mass —
//! whatever `sun`, `origin` and `destination` name in the tree.

use terramenta_solare::lambert::TransferDirection;
use terramenta_solare::{Epoch, FrameId, FrameTree};

/// One solved (or unsolved) cell of a [`PorkchopGrid`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PorkchopCell {
    pub time_of_flight_days: f64,
    pub departure_delta_v_km_s: f64,
    pub arrival_delta_v_km_s: f64,
    pub total_delta_v_km_s: f64,
}

/// A grid of [`PorkchopCell`]s over a departure date axis and an arrival date
/// axis, in row-major order: `cells[departure_index * arrivals.len() +
/// arrival_index]`.
///
/// A cell is `None` wherever
/// [`plan_transfer`](terramenta_solare::mission::plan_transfer) found no
/// solution — arrival at or before departure, which fills in the whole
/// lower-right triangle below the diagonal where the axes overlap, and the
/// occasional genuine Lambert dead zone elsewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct PorkchopGrid {
    pub departures: Vec<Epoch>,
    pub arrivals: Vec<Epoch>,
    cells: Vec<Option<PorkchopCell>>,
}

impl PorkchopGrid {
    pub fn departure_count(&self) -> usize {
        self.departures.len()
    }

    pub fn arrival_count(&self) -> usize {
        self.arrivals.len()
    }

    pub fn cell(&self, departure_index: usize, arrival_index: usize) -> Option<&PorkchopCell> {
        self.cells
            .get(departure_index * self.arrivals.len() + arrival_index)
            .and_then(|cell| cell.as_ref())
    }

    /// The lowest and highest total delta-v anywhere in the grid — the range
    /// a caller would normally space contour levels across. `None` if every
    /// cell is unsolved.
    pub fn total_delta_v_bounds_km_s(&self) -> Option<(f64, f64)> {
        self.cells
            .iter()
            .flatten()
            .map(|cell| cell.total_delta_v_km_s)
            .fold(None, |bounds, value| match bounds {
                None => Some((value, value)),
                Some((min, max)) => Some((min.min(value), max.max(value))),
            })
    }

    /// The total delta-v field as a row-major (departure-major) grid of
    /// `Option<f64>`, the shape [`crate::contour::contours`] takes.
    pub fn total_delta_v_km_s(&self) -> Vec<Option<f64>> {
        self.cells
            .iter()
            .map(|cell| cell.map(|cell| cell.total_delta_v_km_s))
            .collect()
    }
}

/// Linearly spaced epochs from `start` to `end` inclusive, `steps` of them —
/// `steps` must be at least 2, or the axis collapses to just `start`.
fn date_axis(start: Epoch, end: Epoch, steps: usize) -> Vec<Epoch> {
    if steps < 2 {
        return vec![start];
    }
    let start_seconds = start.to_unix_seconds();
    let span_seconds = end.to_unix_seconds() - start_seconds;
    (0..steps)
        .map(|index| {
            let fraction = index as f64 / (steps - 1) as f64;
            Epoch::from_unix_seconds(start_seconds + span_seconds * fraction)
        })
        .collect()
}

/// Solves [`plan_transfer`](terramenta_solare::mission::plan_transfer) at
/// every pair of dates on a `departure_steps`-point axis from
/// `departure_start` to `departure_end` and an `arrival_steps`-point axis
/// from `arrival_start` to `arrival_end`.
///
/// `direction` is the same short-way/long-way choice
/// [`solve`](terramenta_solare::lambert::solve) takes —
/// [`TransferDirection::Prograde`] for essentially every real interplanetary
/// transfer.
pub fn porkchop_grid(
    tree: &FrameTree,
    sun: FrameId,
    origin: FrameId,
    destination: FrameId,
    departure_start: Epoch,
    departure_end: Epoch,
    departure_steps: usize,
    arrival_start: Epoch,
    arrival_end: Epoch,
    arrival_steps: usize,
    direction: TransferDirection,
) -> PorkchopGrid {
    let departures = date_axis(departure_start, departure_end, departure_steps);
    let arrivals = date_axis(arrival_start, arrival_end, arrival_steps);

    let cells = departures
        .iter()
        .flat_map(|&departure| {
            arrivals.iter().map(move |&arrival| {
                terramenta_solare::mission::plan_transfer(
                    tree,
                    sun,
                    origin,
                    destination,
                    departure,
                    arrival,
                    direction,
                )
                .map(|plan| PorkchopCell {
                    time_of_flight_days: plan.time_of_flight_seconds / 86_400.0,
                    departure_delta_v_km_s: plan.departure_delta_v_magnitude_km_s(),
                    arrival_delta_v_km_s: plan.arrival_delta_v_magnitude_km_s(),
                    total_delta_v_km_s: plan.total_delta_v_km_s(),
                })
            })
        })
        .collect();

    PorkchopGrid {
        departures,
        arrivals,
        cells,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use terramenta_solare::solar_system;

    /// The same near-optimal Earth-Mars window `terramenta-solare`'s own
    /// `mission` tests use, widened into a small window either side of it —
    /// enough for the grid to have both solved cells and, below the
    /// diagonal, unsolved ones.
    fn earth_mars_window() -> (
        FrameTree,
        FrameId,
        FrameId,
        FrameId,
        Epoch,
        Epoch,
        Epoch,
        Epoch,
    ) {
        let tree = solar_system();
        let (sun, earth, mars) = (
            tree.find("Sun").unwrap(),
            tree.find("Earth").unwrap(),
            tree.find("Mars").unwrap(),
        );
        let departure_start = Epoch::J2000.advanced_by_seconds(1_230.0 * 86_400.0);
        let departure_end = Epoch::J2000.advanced_by_seconds(1_270.0 * 86_400.0);
        let arrival_start = Epoch::J2000.advanced_by_seconds(1_400.0 * 86_400.0);
        let arrival_end = Epoch::J2000.advanced_by_seconds(1_500.0 * 86_400.0);
        (
            tree,
            sun,
            earth,
            mars,
            departure_start,
            departure_end,
            arrival_start,
            arrival_end,
        )
    }

    #[test]
    fn a_grid_is_shaped_by_its_step_counts() {
        let (tree, sun, earth, mars, d_start, d_end, a_start, a_end) = earth_mars_window();
        let grid = porkchop_grid(
            &tree,
            sun,
            earth,
            mars,
            d_start,
            d_end,
            5,
            a_start,
            a_end,
            7,
            TransferDirection::Prograde,
        );
        assert_eq!(grid.departure_count(), 5);
        assert_eq!(grid.arrival_count(), 7);
        assert_eq!(grid.departures[0], d_start);
        assert_eq!(grid.departures[4], d_end);
    }

    #[test]
    fn cells_near_the_known_window_solve_and_are_a_plausible_cost() {
        let (tree, sun, earth, mars, d_start, d_end, a_start, a_end) = earth_mars_window();
        let grid = porkchop_grid(
            &tree,
            sun,
            earth,
            mars,
            d_start,
            d_end,
            5,
            a_start,
            a_end,
            5,
            TransferDirection::Prograde,
        );
        let (min, max) = grid
            .total_delta_v_bounds_km_s()
            .expect("some cell in a real launch window should solve");
        assert!(min > 0.0 && min < 20.0, "{min}");
        assert!(max >= min);
    }

    #[test]
    fn arrival_at_or_before_departure_never_solves() {
        let (tree, sun, earth, mars, d_start, d_end, a_start, a_end) = earth_mars_window();
        // Swap the axes so every arrival date sits at or before every
        // departure date — the whole grid should come back empty.
        let grid = porkchop_grid(
            &tree,
            sun,
            earth,
            mars,
            a_start,
            a_end,
            4,
            d_start,
            d_end,
            4,
            TransferDirection::Prograde,
        );
        assert_eq!(grid.total_delta_v_bounds_km_s(), None);
        for departure_index in 0..grid.departure_count() {
            for arrival_index in 0..grid.arrival_count() {
                assert_eq!(grid.cell(departure_index, arrival_index), None);
            }
        }
    }
}
