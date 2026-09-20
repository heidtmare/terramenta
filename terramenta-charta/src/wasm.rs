//! The JavaScript binding: [`compute_porkchop_plot`] as a module
//! `terramenta-webapp` imports.
//!
//! Unlike `terramenta-globe`'s binding, there is no running state to queue
//! commands against or stream a callback out of — a porkchop plot is a pure
//! function of the dates and bodies asked for, so the whole surface is one
//! call in and one value out, both plain JSON. `terramenta-webapp/src/
//! porkchop.js` is the one place that calls it.

use serde::Serialize;
use wasm_bindgen::prelude::*;

use terramenta_solare::lambert::TransferDirection;
use terramenta_solare::{Epoch, solar_system};

use crate::contour::Contour;
use crate::grid::PorkchopGrid;

/// The bodies a plot can be drawn between — every frame `solar_system()`
/// wires up, before any of them are asked to plan a transfer. Answers
/// without touching Lambert at all, the same way `terramenta-globe`'s own
/// catalogue calls do, so a page can list its choices while the rest of the
/// module is still loading.
#[wasm_bindgen(js_name = bodies)]
pub fn bodies() -> JsValue {
    to_js(&["Sun", "Earth", "Mars"])
}

/// One (departure, arrival) date pair's transfer cost, laid out for a porkchop
/// plot's colour field — both axes in Unix seconds, the same clock the rest
/// of this workspace runs on.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Cell {
    departure_unix_seconds: f64,
    arrival_unix_seconds: f64,
    time_of_flight_days: f64,
    departure_delta_v_km_s: f64,
    arrival_delta_v_km_s: f64,
    total_delta_v_km_s: f64,
}

/// A traced contour line, at one field value, as unstitched segments — see
/// [`crate::contour::Contour`] for why they are not joined into paths here.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContourLine {
    value: f64,
    segments: Vec<[[f64; 2]; 2]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PorkchopPlot {
    departures_unix_seconds: Vec<f64>,
    arrivals_unix_seconds: Vec<f64>,
    /// Every solved cell, departure-major — empty wherever
    /// [`terramenta_solare::mission::plan_transfer`] found no solution for
    /// that pair, most commonly because the arrival date is at or before the
    /// departure date.
    cells: Vec<Option<Cell>>,
    /// The lowest and highest total delta-v in the grid, `null` if nothing
    /// solved — what a caller would normally space `contourLevels` across
    /// before asking again, or use directly to colour the field.
    total_delta_v_bounds_km_s: Option<[f64; 2]>,
    contours: Vec<ContourLine>,
}

/// Computes a porkchop plot between two bodies named as [`bodies`] lists
/// them, over a departure date axis and an arrival date axis each given as
/// `[start_unix_seconds, end_unix_seconds, step_count]`.
///
/// `contour_levels_km_s` are the total delta-v values to trace contours at;
/// pass an empty array for the grid alone with no contours computed.
/// `retrograde` selects the long way round a transfer sweeps its focus —
/// `false` for essentially any real interplanetary transfer.
///
/// Returns `null` if either body name is not one [`bodies`] lists, or if
/// either axis's step count is below 2.
#[wasm_bindgen(js_name = computePorkchopPlot)]
#[allow(clippy::too_many_arguments)]
pub fn compute_porkchop_plot(
    origin_body: &str,
    destination_body: &str,
    departure_start_unix_seconds: f64,
    departure_end_unix_seconds: f64,
    departure_steps: usize,
    arrival_start_unix_seconds: f64,
    arrival_end_unix_seconds: f64,
    arrival_steps: usize,
    contour_levels_km_s: Vec<f64>,
    retrograde: bool,
) -> JsValue {
    if departure_steps < 2 || arrival_steps < 2 {
        return JsValue::NULL;
    }

    let tree = solar_system();
    let Some(sun) = tree.find("Sun") else {
        return JsValue::NULL;
    };
    let (Some(origin), Some(destination)) = (tree.find(origin_body), tree.find(destination_body))
    else {
        return JsValue::NULL;
    };

    let direction = if retrograde {
        TransferDirection::Retrograde
    } else {
        TransferDirection::Prograde
    };

    let grid = crate::grid::porkchop_grid(
        &tree,
        sun,
        origin,
        destination,
        Epoch::from_unix_seconds(departure_start_unix_seconds),
        Epoch::from_unix_seconds(departure_end_unix_seconds),
        departure_steps,
        Epoch::from_unix_seconds(arrival_start_unix_seconds),
        Epoch::from_unix_seconds(arrival_end_unix_seconds),
        arrival_steps,
        direction,
    );

    to_js(&plot(&grid, &contour_levels_km_s))
}

fn plot(grid: &PorkchopGrid, contour_levels_km_s: &[f64]) -> PorkchopPlot {
    let departures_unix_seconds: Vec<f64> = grid
        .departures
        .iter()
        .map(|epoch| epoch.to_unix_seconds())
        .collect();
    let arrivals_unix_seconds: Vec<f64> = grid
        .arrivals
        .iter()
        .map(|epoch| epoch.to_unix_seconds())
        .collect();

    let cells = departures_unix_seconds
        .iter()
        .enumerate()
        .flat_map(|(departure_index, &departure_unix_seconds)| {
            arrivals_unix_seconds.iter().enumerate().map(
                move |(arrival_index, &arrival_unix_seconds)| {
                    grid.cell(departure_index, arrival_index).map(|cell| Cell {
                        departure_unix_seconds,
                        arrival_unix_seconds,
                        time_of_flight_days: cell.time_of_flight_days,
                        departure_delta_v_km_s: cell.departure_delta_v_km_s,
                        arrival_delta_v_km_s: cell.arrival_delta_v_km_s,
                        total_delta_v_km_s: cell.total_delta_v_km_s,
                    })
                },
            )
        })
        .collect();

    let contours = crate::contour::contours(
        &departures_unix_seconds,
        &arrivals_unix_seconds,
        &grid.total_delta_v_km_s(),
        contour_levels_km_s,
    )
    .into_iter()
    .map(|Contour { value, segments }| ContourLine {
        value,
        segments: segments
            .into_iter()
            .map(|(a, b)| [[a.0, a.1], [b.0, b.1]])
            .collect(),
    })
    .collect();

    PorkchopPlot {
        departures_unix_seconds,
        arrivals_unix_seconds,
        cells,
        total_delta_v_bounds_km_s: grid
            .total_delta_v_bounds_km_s()
            .map(|(min, max)| [min, max]),
        contours,
    }
}

/// Serializes through JSON, which is the one representation `serde` and
/// JavaScript both already agree on without another dependency — the same
/// choice `terramenta-globe`'s own binding makes.
fn to_js<T: serde::Serialize>(value: &T) -> JsValue {
    serde_json::to_string(value)
        .ok()
        .and_then(|json| js_sys::JSON::parse(&json).ok())
        .unwrap_or(JsValue::NULL)
}
