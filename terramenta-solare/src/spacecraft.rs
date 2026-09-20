//! A spacecraft, tracked relative to whichever body's gravity currently
//! dominates its trajectory.
//!
//! This is the case the whole tree exists for. A spacecraft leaving Earth
//! starts out described the cheap, precise way any Earth-orbiting thing is:
//! a two-body orbit relative to Earth's centre, in kilometres, with Earth's
//! own multi-hundred-million-kilometre wander around the Sun nowhere in the
//! numbers to lose precision to. Far enough out, Earth stops being the
//! dominant pull and the same description stops being the useful one — not
//! because the physics changed at a sharp boundary, but because a two-body
//! approximation centred on Earth is a worse fit to what is actually shaping
//! the trajectory than one centred on the Sun. [`Spacecraft::update`] is that
//! judgement call, made the same way real mission design makes it: by
//! comparing distance from the current primary against its sphere of
//! influence ([`crate::bodies::sphere_of_influence_km`]), and re-expressing
//! the spacecraft's state relative to the next body up the moment it crosses.
//!
//! That re-expression is the one step that has to be careful.
//! [`crate::frame::FrameTree::reparent`] will cheerfully accept any ephemeris
//! it's handed, jump included — the continuity is [`Spacecraft::update`]'s
//! job, done by computing the state relative to the *new* primary from the
//! state relative to the old one plus the old primary's own state relative to
//! the new one, all at the same instant, before the swap ever reaches the
//! tree.

use crate::bodies;
use crate::frame::{Ephemeris, FrameId, FrameTree, StateVector};
use crate::orbit::OrbitalElements;
use crate::time::Epoch;

/// A body a spacecraft can be described as orbiting: enough to propagate a
/// two-body orbit around it, and — for every primary but the last — enough to
/// tell when that orbit has stopped being a good description.
#[derive(Debug, Clone)]
pub struct Primary {
    pub frame: FrameId,
    pub gm_km3_s2: f64,
    /// What this primary itself orbits, boxed because the chain is
    /// self-referential in type only, never in practice: Earth points at the
    /// Sun, and the Sun — the last stop this crate models — points at
    /// nothing, which is what tells [`Spacecraft::update`] to stop checking.
    pub orbits: Option<Box<Primary>>,
}

impl Primary {
    pub fn sun(frame: FrameId) -> Self {
        Self {
            frame,
            gm_km3_s2: bodies::GM_SUN_KM3_S2,
            orbits: None,
        }
    }

    pub fn earth(frame: FrameId, sun: FrameId) -> Self {
        Self {
            frame,
            gm_km3_s2: bodies::GM_EARTH_KM3_S2,
            orbits: Some(Box::new(Self::sun(sun))),
        }
    }

    pub fn mars(frame: FrameId, sun: FrameId) -> Self {
        Self {
            frame,
            gm_km3_s2: bodies::GM_MARS_KM3_S2,
            orbits: Some(Box::new(Self::sun(sun))),
        }
    }
}

/// The [`Ephemeris`] a spacecraft's frame reports through: whatever two-body
/// orbit it is currently on, relative to whatever it currently orbits.
/// Replaced wholesale, along with the frame's parent, every time
/// [`Spacecraft::update`] finds it has crossed a sphere of influence.
#[derive(Debug, Clone, Copy)]
struct TwoBodyOrbit {
    elements: OrbitalElements,
    gm_km3_s2: f64,
}

impl Ephemeris for TwoBodyOrbit {
    fn state_at(&self, epoch: Epoch) -> StateVector {
        self.elements
            .state_at(self.gm_km3_s2, epoch.to_unix_seconds())
    }
}

/// A spacecraft on a two-body orbit around its current [`Primary`], as a
/// frame in a [`FrameTree`].
pub struct Spacecraft {
    pub frame: FrameId,
    primary: Primary,
    elements: OrbitalElements,
}

impl Spacecraft {
    /// Adds a spacecraft to the tree, orbiting `primary` on the given state
    /// relative to it.
    pub fn spawn(
        tree: &mut FrameTree,
        name: &'static str,
        primary: Primary,
        state: StateVector,
        epoch: Epoch,
    ) -> Self {
        let elements =
            OrbitalElements::from_state(state, primary.gm_km3_s2, epoch.to_unix_seconds());
        let frame = tree.add(
            name,
            primary.frame,
            TwoBodyOrbit {
                elements,
                gm_km3_s2: primary.gm_km3_s2,
            },
        );
        Self {
            frame,
            primary,
            elements,
        }
    }

    /// The body this spacecraft is currently described as orbiting.
    pub fn primary_frame(&self) -> FrameId {
        self.primary.frame
    }

    /// Checks this spacecraft against its current primary's sphere of
    /// influence and, if it has crossed it, reparents it to whatever that
    /// primary itself orbits — Earth to the Sun, for the one escape this
    /// crate's bodies know about. Returns whether a transition happened.
    ///
    /// A spacecraft already on its outermost primary (the Sun) has nothing
    /// further to check against and this always returns `false` for it,
    /// which is also why a single call only ever crosses one boundary: a
    /// trajectory extreme enough to leap two spheres of influence in one
    /// update still only advances one level per call, catching up over
    /// however many calls follow, the same way a fast-moving object can only
    /// cross one tile boundary at a time in a tile-based system without that
    /// being a bug.
    pub fn update(&mut self, tree: &mut FrameTree, epoch: Epoch) -> bool {
        let Some(next_primary) = self.primary.orbits.as_deref() else {
            return false;
        };

        let state_relative_to_primary = self
            .elements
            .state_at(self.primary.gm_km3_s2, epoch.to_unix_seconds());
        let primary_relative_to_next =
            tree.state_of_relative_to(self.primary.frame, next_primary.frame, epoch);

        let soi_km = bodies::sphere_of_influence_km(
            self.primary.gm_km3_s2,
            next_primary.gm_km3_s2,
            primary_relative_to_next.position_km.length(),
        );
        if state_relative_to_primary.position_km.length() <= soi_km {
            return false;
        }

        // The state relative to the next primary up, at the same instant —
        // this is what keeps the spacecraft's actual position from jumping
        // at the moment its parent changes.
        let state_relative_to_next = state_relative_to_primary + primary_relative_to_next;
        let new_elements = OrbitalElements::from_state(
            state_relative_to_next,
            next_primary.gm_km3_s2,
            epoch.to_unix_seconds(),
        );

        let next_primary = next_primary.clone();
        tree.reparent(
            self.frame,
            next_primary.frame,
            TwoBodyOrbit {
                elements: new_elements,
                gm_km3_s2: next_primary.gm_km3_s2,
            },
        );
        self.elements = new_elements;
        self.primary = next_primary;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{FixedAtParent, FixedOffset};
    use glam::DVec3;

    /// A minimal tree — SSB, Sun, Earth — with Earth held at a fixed offset
    /// rather than propagated, since these tests are about the spacecraft's
    /// own transition and not about how well Earth's ephemeris matches the
    /// real planet.
    fn earth_sun_tree() -> (FrameTree, Primary) {
        let mut tree = FrameTree::new();
        let root = tree.root();
        let sun = tree.add("Sun", root, FixedAtParent);
        let earth_state = StateVector::new(DVec3::new(1.495_98e8, 0.0, 0.0), DVec3::ZERO);
        let earth = tree.add("Earth", root, FixedOffset(earth_state));
        (tree, Primary::earth(earth, sun))
    }

    #[test]
    fn a_bound_orbit_never_leaves_earth() {
        let (mut tree, earth) = earth_sun_tree();
        // Slightly inclined rather than dead equatorial: an orbit exactly in
        // the reference plane is the degenerate case `OrbitalElements`
        // documents itself as not handling (an undefined ascending node).
        let leo = StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 7.3, 1.0));
        let mut spacecraft =
            Spacecraft::spawn(&mut tree, "ISS-like", earth.clone(), leo, Epoch::J2000);

        for hours in 1..48 {
            let epoch = Epoch::J2000.advanced_by_seconds(hours as f64 * 3_600.0);
            assert!(!spacecraft.update(&mut tree, epoch));
        }
        assert_eq!(spacecraft.primary_frame(), earth.frame);
    }

    #[test]
    fn an_escape_trajectory_hands_the_spacecraft_off_to_the_sun_without_a_jump() {
        let (mut tree, earth) = earth_sun_tree();
        let sun_frame = earth.orbits.as_ref().unwrap().frame;
        // Comfortably above local escape velocity at this altitude.
        let departure = StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 11.5, 2.0));
        let mut spacecraft =
            Spacecraft::spawn(&mut tree, "Escaper", earth.clone(), departure, Epoch::J2000);

        let mut transitioned = false;
        let mut epoch = Epoch::J2000;
        for _ in 0..(60 * 24) {
            epoch = epoch.advanced_by_seconds(3_600.0);

            // The spacecraft's position relative to the SSB, just before the
            // update this iteration might reparent it in.
            let before = tree.state_relative_to_root(spacecraft.frame, epoch);
            if spacecraft.update(&mut tree, epoch) {
                let after = tree.state_relative_to_root(spacecraft.frame, epoch);
                assert!(
                    (after.position_km - before.position_km).length() < 1.0e-3,
                    "reparenting jumped by {} km",
                    (after.position_km - before.position_km).length()
                );
                transitioned = true;
                break;
            }
        }

        assert!(
            transitioned,
            "never crossed Earth's sphere of influence within 60 days"
        );
        assert_eq!(spacecraft.primary_frame(), sun_frame);
        // The Sun is the last stop this crate models: nothing left to escape to.
        assert!(!spacecraft.update(&mut tree, epoch.advanced_by_seconds(3_600.0)));
    }
}
