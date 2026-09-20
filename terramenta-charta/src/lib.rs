//! 2D mission-planning charts — the analysis a `terramenta-solare` frame tree
//! feeds, but that a tree of frames and orbits has no view onto by itself.
//!
//! The one this crate exists for is the porkchop plot: contours of total
//! delta-v over a grid of departure date against arrival date, which is how
//! an interplanetary launch window actually gets chosen — not read off a
//! single Hohmann transfer, but scanned for, because the cheapest date to
//! leave depends on the cheapest date to arrive right along with it.
//! [`grid::porkchop_grid`] is that scan, one
//! [`terramenta_solare::mission::plan_transfer`] per cell, and [`contour`] is
//! the marching-squares pass that turns the resulting grid into the lines a
//! plot actually draws.
//!
//! Like `terramenta-solare` underneath it, this crate draws nothing itself.
//! [`wasm`] is the binding that hands both the grid and its contours to
//! JavaScript as plain data, for `terramenta-webapp` to render as an SVG —
//! the same split `terramenta-globe` and its own `wasm` module make between
//! the model and the page around it.

pub mod contour;
pub mod grid;
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use contour::{Contour, Point, contours};
pub use grid::{PorkchopCell, PorkchopGrid, porkchop_grid};
