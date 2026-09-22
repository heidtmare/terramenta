//! A spacecraft, tracked relative to whichever body's gravity currently
//! dominates its trajectory.
//!
//! A spacecraft leaving Earth starts out described as a two-body orbit
//! relative to Earth's centre, in kilometres, with Earth's own
//! multi-hundred-million-kilometre wander around the Sun nowhere in the
//! numbers to lose precision to. Far enough out, Earth stops being the
//! dominant pull, and a two-body approximation centred on Earth becomes a
//! worse fit than one centred on the Sun. [`Spacecraft::update`] makes that
//! call by comparing distance from the current primary against its sphere
//! of influence ([`crate::bodies::sphere_of_influence_km`]), and
//! re-expresses the spacecraft's state relative to the next body up the
//! moment it crosses.
//!
//! That re-expression has to preserve continuity.
//! [`crate::frame::FrameTree::reparent`] accepts any ephemeris it's handed,
//! jump included — [`Spacecraft::update`] keeps continuity by computing the
//! state relative to the *new* primary from the state relative to the old
//! one plus the old primary's own state relative to the new one, all at the
//! same instant, before the swap reaches the tree.
//!
//! Escaping Earth is only half the trip. The other half is arriving: a
//! spacecraft heliocentric under the Sun has nothing further to escape
//! *from*, but can still be captured *by* whatever it flies close enough to
//! — so [`Primary`] carries [`Primary::capture_candidates`] as well as
//! [`Primary::orbits`], and [`Spacecraft::update`] checks both directions
//! every tick. Together the two checks form the patched-conics
//! approximation this crate implements: a hyperbolic departure relative to
//! the origin, a Keplerian ellipse relative to the Sun, and a hyperbolic
//! arrival relative to the destination, each the ordinary two-body orbit
//! [`crate::orbit`] already propagates, stitched together only at the
//! moments a sphere of influence is crossed.

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
    /// self-referential in type only: Earth points at the Sun, and the Sun
    /// — the last stop this crate models — points at nothing, which tells
    /// [`Spacecraft::update`] to stop checking for an escape.
    pub orbits: Option<Box<Primary>>,
    /// The bodies a spacecraft on *this* primary should be checked against
    /// for capture: the inward half of the patched-conics switch, mirroring
    /// `orbits`. A spacecraft heliocentric under the Sun escapes nothing
    /// further, but can fall into whichever of these it enters the sphere
    /// of influence of. Empty for Earth and Mars in this crate's own
    /// bodies, since neither is a capture candidate.
    pub capture_candidates: Vec<Primary>,
}

impl Primary {
    /// The last stop this crate models: a primary with nothing left to
    /// escape to, which tells [`Spacecraft::update`] to stop checking for an
    /// escape.
    pub fn sun(frame: FrameId) -> Self {
        Self {
            frame,
            gm_km3_s2: bodies::GM_SUN_KM3_S2,
            orbits: None,
            capture_candidates: Vec::new(),
        }
    }

    /// A primary that itself orbits `parent` — the general case every body
    /// but the Sun is. [`Primary::earth`] and [`Primary::mars`] are both just
    /// this with their own `gm_km3_s2`; any body added later gets the same
    /// one-liner rather than its own constructor.
    pub fn orbiting(frame: FrameId, gm_km3_s2: f64, parent: Primary) -> Self {
        Self {
            frame,
            gm_km3_s2,
            orbits: Some(Box::new(parent)),
            capture_candidates: Vec::new(),
        }
    }

    pub fn earth(frame: FrameId, sun: FrameId) -> Self {
        Self::orbiting(frame, bodies::GM_EARTH_KM3_S2, Self::sun(sun))
    }

    pub fn mars(frame: FrameId, sun: FrameId) -> Self {
        Self::orbiting(frame, bodies::GM_MARS_KM3_S2, Self::sun(sun))
    }

    /// Attaches the bodies a spacecraft on this primary should be checked
    /// against for capture — giving the Sun a [`Primary::mars`], for
    /// example, lets [`Spacecraft::update`] notice a heliocentric approach
    /// entering Mars' sphere of influence and hand the spacecraft off to it.
    pub fn with_capture_candidates(mut self, candidates: Vec<Primary>) -> Self {
        self.capture_candidates = candidates;
        self
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
    ///
    /// # Panics
    ///
    /// If `name` is already taken by another frame in `tree` — see
    /// [`FrameTree::add`].
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
    /// influence in both directions patched conics needs — outward, to
    /// whatever the primary itself orbits ([`Primary::orbits`]; Earth to the
    /// Sun, escaping), and inward, into whichever of the primary's own
    /// [`Primary::capture_candidates`] it has come within (the Sun to Mars,
    /// arriving) — and reparents it the moment either happens. Returns
    /// whether a transition happened.
    ///
    /// A spacecraft on its outermost primary has nothing further to escape
    /// to, and one whose primary names no capture candidates has nothing to
    /// be captured by; a real Earth-to-Mars trip needs one of each. A single
    /// call only ever crosses one boundary — a trajectory extreme enough to
    /// leap two spheres of influence in one update still only advances one
    /// level per call, catching up over subsequent calls.
    pub fn update(&mut self, tree: &mut FrameTree, epoch: Epoch) -> bool {
        self.try_escape(tree, epoch) || self.try_capture(tree, epoch)
    }

    /// The outward half of [`Spacecraft::update`]: whether this spacecraft
    /// has left its primary's sphere of influence, bound for whatever that
    /// primary orbits.
    fn try_escape(&mut self, tree: &mut FrameTree, epoch: Epoch) -> bool {
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

        // The state relative to the next primary up, at the same instant:
        // keeps the spacecraft's actual position from jumping when its
        // parent changes.
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

    /// The inward half: whether this spacecraft has come within the sphere
    /// of influence of one of its primary's [`Primary::capture_candidates`]
    /// — Mars, for example, while still described relative to the Sun — and
    /// if so, reparents to the one it has entered, the same
    /// continuity-preserving way [`Spacecraft::try_escape`] does.
    fn try_capture(&mut self, tree: &mut FrameTree, epoch: Epoch) -> bool {
        if self.primary.capture_candidates.is_empty() {
            return false;
        }

        let state_relative_to_primary = self
            .elements
            .state_at(self.primary.gm_km3_s2, epoch.to_unix_seconds());
        let primary_frame = self.primary.frame;
        let primary_gm = self.primary.gm_km3_s2;

        let captured = self
            .primary
            .capture_candidates
            .iter()
            .find_map(|candidate| {
                let candidate_relative_to_primary =
                    tree.state_of_relative_to(candidate.frame, primary_frame, epoch);
                let soi_km = bodies::sphere_of_influence_km(
                    candidate.gm_km3_s2,
                    primary_gm,
                    candidate_relative_to_primary.position_km.length(),
                );

                let state_relative_to_candidate =
                    state_relative_to_primary - candidate_relative_to_primary;
                (state_relative_to_candidate.position_km.length() <= soi_km)
                    .then(|| (candidate.clone(), state_relative_to_candidate))
            });

        let Some((candidate, state_relative_to_candidate)) = captured else {
            return false;
        };

        let new_elements = OrbitalElements::from_state(
            state_relative_to_candidate,
            candidate.gm_km3_s2,
            epoch.to_unix_seconds(),
        );
        tree.reparent(
            self.frame,
            candidate.frame,
            TwoBodyOrbit {
                elements: new_elements,
                gm_km3_s2: candidate.gm_km3_s2,
            },
        );
        self.elements = new_elements;
        self.primary = candidate;
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
    /// transition, not Earth's ephemeris accuracy.
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
        // does not handle (an undefined ascending node).
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

    /// A minimal tree with Mars added, and the Sun told to check for capture
    /// by it: the inward half of the switch
    /// `an_escape_trajectory_hands_the_spacecraft_off_to_the_sun_without_a_jump`
    /// exercises outward.
    fn earth_sun_mars_tree() -> (FrameTree, Primary, FrameId) {
        let (mut tree, earth) = earth_sun_tree();
        let sun_frame = earth.orbits.as_ref().unwrap().frame;
        // Farther than Earth and on the far side, so its position is never
        // mistaken for Earth's.
        let mars_state = StateVector::new(DVec3::new(-2.279e8, 0.0, 0.0), DVec3::ZERO);
        let mars_frame = tree.add("Mars", tree.root(), FixedOffset(mars_state));

        let earth_with_mars_capture = Primary {
            orbits: Some(Box::new(
                Primary::sun(sun_frame)
                    .with_capture_candidates(vec![Primary::mars(mars_frame, sun_frame)]),
            )),
            ..earth
        };
        (tree, earth_with_mars_capture, mars_frame)
    }

    #[test]
    fn a_heliocentric_approach_is_captured_by_mars_without_a_jump() {
        let (mut tree, earth, mars_frame) = earth_sun_mars_tree();
        let mars_state = tree.local_state(mars_frame, Epoch::J2000);

        // Already heliocentric, on a hyperbolic approach aimed at Mars from
        // just inside its sphere of influence (a few hundred thousand
        // kilometres, not the ~228 million between Mars and the Sun).
        // Slightly out of the reference plane rather than dead equatorial:
        // exactly in the plane is the degenerate case `OrbitalElements`
        // does not handle.
        let approach = StateVector::new(
            mars_state.position_km + DVec3::new(100_000.0, 0.0, 0.0),
            DVec3::new(-3.0, 1.0, 0.4),
        );
        let sun = earth.orbits.as_deref().unwrap().clone();
        let mut spacecraft = Spacecraft::spawn(&mut tree, "Arriver", sun, approach, Epoch::J2000);

        let before = tree.state_relative_to_root(spacecraft.frame, Epoch::J2000);
        assert!(
            spacecraft.update(&mut tree, Epoch::J2000),
            "should already be inside Mars' sphere of influence"
        );
        let after = tree.state_relative_to_root(spacecraft.frame, Epoch::J2000);

        assert_eq!(spacecraft.primary_frame(), mars_frame);
        assert!(
            (after.position_km - before.position_km).length() < 1.0e-3,
            "reparenting jumped by {} km",
            (after.position_km - before.position_km).length()
        );
    }

    #[test]
    fn a_trip_far_from_mars_is_never_captured() {
        let (mut tree, earth, mars_frame) = earth_sun_mars_tree();
        let sun = earth.orbits.as_deref().unwrap().clone();
        // Heliocentric, but nowhere near Mars' own position.
        let far_from_mars =
            StateVector::new(DVec3::new(1.6e8, 0.0, 0.0), DVec3::new(0.0, 25.0, 1.0));
        let mut spacecraft =
            Spacecraft::spawn(&mut tree, "Passer-by", sun, far_from_mars, Epoch::J2000);

        for hours in 1..48 {
            let epoch = Epoch::J2000.advanced_by_seconds(hours as f64 * 3_600.0);
            assert!(!spacecraft.update(&mut tree, epoch));
        }
        assert_ne!(spacecraft.primary_frame(), mars_frame);
    }

    #[test]
    fn a_spacecraft_flies_the_full_earth_to_mars_patched_conics_trip() {
        // First, a plain Earth-escape run with nothing to be captured by, so
        // the heliocentric leg can be sampled without Mars perturbing the
        // decision of when the escape itself happens.
        let (mut probe_tree, probe_earth) = earth_sun_tree();
        let mut probe = Spacecraft::spawn(
            &mut probe_tree,
            "Probe",
            probe_earth.clone(),
            StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 11.5, 2.0)),
            Epoch::J2000,
        );
        let mut epoch = Epoch::J2000;
        for _ in 0..(80 * 24) {
            epoch = epoch.advanced_by_seconds(3_600.0);
            probe.update(&mut probe_tree, epoch);
        }
        let sun_frame = probe_earth.orbits.as_ref().unwrap().frame;
        assert_eq!(
            probe.primary_frame(),
            sun_frame,
            "should be heliocentric well before day 80"
        );
        // Where that unperturbed trip has reached by day 80 — the point this
        // test puts Mars exactly on, so the run below is captured by
        // construction, not luck.
        let waypoint = probe_tree.state_relative_to_root(probe.frame, epoch);

        let (mut tree, earth) = earth_sun_tree();
        let sun_frame = earth.orbits.as_ref().unwrap().frame;
        let root = tree.root();
        let mars_frame = tree.add(
            "Mars",
            root,
            FixedOffset(StateVector::new(waypoint.position_km, DVec3::ZERO)),
        );
        let earth_with_mars_capture = Primary {
            orbits: Some(Box::new(
                Primary::sun(sun_frame)
                    .with_capture_candidates(vec![Primary::mars(mars_frame, sun_frame)]),
            )),
            ..earth
        };

        let mut spacecraft = Spacecraft::spawn(
            &mut tree,
            "Trip",
            earth_with_mars_capture,
            StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 11.5, 2.0)),
            Epoch::J2000,
        );

        let mut epoch = Epoch::J2000;
        let mut escaped = false;
        let mut captured = false;
        for _ in 0..(120 * 24) {
            epoch = epoch.advanced_by_seconds(3_600.0);
            let before = tree.state_relative_to_root(spacecraft.frame, epoch);
            if spacecraft.update(&mut tree, epoch) {
                let after = tree.state_relative_to_root(spacecraft.frame, epoch);
                assert!(
                    (after.position_km - before.position_km).length() < 1.0e-3,
                    "reparenting jumped by {} km",
                    (after.position_km - before.position_km).length()
                );
                if !escaped {
                    assert_eq!(
                        spacecraft.primary_frame(),
                        sun_frame,
                        "first hand-off should be to the Sun"
                    );
                    escaped = true;
                } else {
                    assert_eq!(
                        spacecraft.primary_frame(),
                        mars_frame,
                        "second hand-off should be to Mars"
                    );
                    captured = true;
                    break;
                }
            }
        }

        assert!(escaped, "never escaped Earth");
        assert!(captured, "never captured at Mars");
    }
}
