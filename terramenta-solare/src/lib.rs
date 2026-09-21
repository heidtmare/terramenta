//! Solar-system mission planning, starting from the one thing that makes it a
//! different problem from a globe of just the Earth: there is no single
//! origin a spacecraft's position can be usefully measured from all the way
//! from launch to arrival.
//!
//! [`frame`] has the reasoning and the tree itself — the Solar System
//! Barycentre at the root, [`sun::Sun`], [`planets::EARTH`] and
//! [`planets::MARS`] as its direct children, each reporting its own position
//! the low-precision way `terramenta-globe`'s sun and moon models already do.
//! [`orbit`] is the two-body mechanics everything propagates with, and
//! [`spacecraft`] is where the tree earns its keep: a [`spacecraft::Spacecraft`]
//! orbits whichever body currently dominates it and hands itself off to the
//! next one out the moment that stops being true, without its actual
//! position ever jumping to show for it.
//!
//! [`lambert`] and [`mission`] are the other direction a spacecraft's
//! trajectory comes from: not propagated forward from a known state, but
//! solved for — given where two bodies are on two given dates, the one orbit
//! that connects them, and the velocity change departing and arriving on it
//! actually costs.
//!
//! Nothing here depends on Bevy or draws anything — this crate is the model,
//! the same way `terramenta-globe`'s own frame and ephemeris modules are
//! plain Rust underneath the systems that read them. Wiring a solar system
//! scene up to this tree is future work for whatever embeds it.

pub mod bodies;
pub mod ecliptic;
pub mod frame;
pub mod lambert;
pub mod mission;
pub mod orbit;
pub mod planets;
pub mod spacecraft;
pub mod sun;
pub mod time;

pub use frame::{Ephemeris, FrameId, FrameTree, StateVector};
pub use lambert::{LambertSolution, TransferDirection};
pub use mission::{HohmannTransfer, TransferPlan, hohmann_transfer, plan_transfer};
pub use time::Epoch;

/// The tree this crate exists to build: the Solar System Barycentre at the
/// root, with the Sun, Earth and Mars hanging off it as direct children —
/// exactly the top-level shape described in [`frame`]'s module docs, and the
/// one starting point every embedder needs rather than three separate calls
/// to [`FrameTree::add`] with the right [`Ephemeris`] for each.
///
/// The frames are reachable afterward by [`FrameTree::find`], under the names
/// `"Sun"`, `"Earth"` and `"Mars"`.
pub fn solar_system() -> FrameTree {
    let mut tree = FrameTree::new();
    let root = tree.root();
    tree.add("Sun", root, sun::Sun);
    tree.add("Earth", root, planets::EARTH);
    tree.add("Mars", root, planets::MARS);
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spacecraft::{Primary, Spacecraft};
    use glam::DVec3;

    #[test]
    fn solar_system_wires_up_the_three_top_level_bodies() {
        let tree = solar_system();
        for name in ["Sun", "Earth", "Mars"] {
            let frame = tree.find(name).unwrap_or_else(|| panic!("no {name} frame"));
            assert_eq!(tree.parent(frame), Some(tree.root()));
        }
        assert_eq!(tree.find("Pluto"), None);
    }

    #[test]
    fn earth_and_mars_are_a_plausible_distance_apart() {
        let tree = solar_system();
        let (earth, mars) = (tree.find("Earth").unwrap(), tree.find("Mars").unwrap());
        // From about half an AU at closest approach to a bit over two and a
        // half at opposition on the far side of the Sun.
        for days in [0.0, 100.0, 400.0, 900.0] {
            let epoch = Epoch::J2000.advanced_by_seconds(days * 86_400.0);
            let separation_au = tree
                .state_of_relative_to(earth, mars, epoch)
                .position_km
                .length()
                / bodies::ASTRONOMICAL_UNIT_KM;
            assert!(
                (0.3..2.7).contains(&separation_au),
                "{days}d: {separation_au} AU"
            );
        }
    }

    /// The scenario from this crate's own module docs, worked end to end: a
    /// spacecraft leaves Earth orbit, is described relative to Earth's
    /// centre for as long as that is the useful description, and hands
    /// itself off to a heliocentric frame the moment it isn't — all against
    /// the real (if low-precision) Earth ephemeris, not a fixed stand-in.
    #[test]
    fn a_spacecraft_escaping_earth_ends_up_heliocentric() {
        let mut tree = solar_system();
        let (sun, earth) = (tree.find("Sun").unwrap(), tree.find("Earth").unwrap());
        let primary = Primary::earth(earth, sun);

        let departure = StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 11.5, 2.0));
        let mut spacecraft =
            Spacecraft::spawn(&mut tree, "Escaper", primary, departure, Epoch::J2000);

        let mut epoch = Epoch::J2000;
        let mut escaped = false;
        for _ in 0..(60 * 24) {
            epoch = epoch.advanced_by_seconds(3_600.0);
            if spacecraft.update(&mut tree, epoch) {
                escaped = true;
                break;
            }
        }

        assert!(
            escaped,
            "never crossed Earth's sphere of influence within 60 days"
        );
        assert_eq!(spacecraft.primary_frame(), sun);

        // Heliocentric now: its distance from the Sun should be in the same
        // ballpark as Earth's own, not still hovering at a few Earth radii.
        let from_sun = tree
            .state_of_relative_to(spacecraft.frame, sun, epoch)
            .position_km
            .length();
        assert!(from_sun > 1.0e7, "{from_sun} km from the Sun");
    }
}
