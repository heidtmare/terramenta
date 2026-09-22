//! The hierarchical reference frame tree.
//!
//! A globe of just the Earth can put the planet's centre at the origin.
//! A solar system cannot: Mars is a couple of hundred million kilometres
//! from that origin, and an `f32` — or even an `f64` doing arithmetic
//! *relative to a distant origin* — starts losing metres of precision at
//! that range, right when a spacecraft needs metres to rendezvous with
//! something. The fix, standard in mission-planning tools: nothing is
//! stored relative to a single universal origin. Each body keeps its
//! position relative to *its own parent*, and a position far from home is
//! reached by summing a short chain of short vectors, each accurate where
//! it is taken.
//!
//! The tree's root is the Solar System Barycentre (SSB), the solar system's
//! actual centre of mass, a few solar radii from the Sun's centre because
//! Jupiter and Saturn are heavy enough to pull it off-centre. Sun, Earth
//! and Mars hang off it as direct children, each an [`Ephemeris`] that
//! reports its own position relative to the SSB. A spacecraft hangs off
//! *whichever body's gravity currently dominates it* — Earth while bound to
//! Earth orbit, the Sun once far enough out that Earth's pull no longer
//! shapes its path — and is free to change parents at run time as that
//! changes; see [`crate::spacecraft`].
//!
//! Every vector in the tree is stated in the same, fixed orientation: the
//! ICRF (International Celestial Reference Frame), the inertial axes the
//! solar system's ephemerides are conventionally published in. Because
//! every node shares that orientation, reparenting or comparing two frames
//! is nothing but vector addition and subtraction, with no rotation to
//! carry along — what makes walking the tree cheap. (This is not
//! `terramenta-globe`'s ECEF/ECI split: those are two *orientations* of one
//! origin, Earth's centre. This tree is one orientation and many origins.
//! A body's own spin — ECEF, or the Martian equivalent — is a rotation
//! applied on top of the position this tree gives its centre, and stays
//! that crate's concern.)

use std::collections::HashMap;

use glam::DVec3;

use crate::time::Epoch;

/// A position and velocity, in ICRF axes, in kilometres and kilometres per
/// second.
///
/// Always relative to *something* — a parent frame, another frame, or the
/// SSB — so this carries no origin of its own. What it is relative to is
/// the caller's business, tracked by which [`FrameTree`] method produced
/// it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StateVector {
    pub position_km: DVec3,
    pub velocity_km_s: DVec3,
}

impl StateVector {
    pub const ZERO: Self = Self {
        position_km: DVec3::ZERO,
        velocity_km_s: DVec3::ZERO,
    };

    pub fn new(position_km: DVec3, velocity_km_s: DVec3) -> Self {
        Self {
            position_km,
            velocity_km_s,
        }
    }
}

impl std::ops::Add for StateVector {
    type Output = StateVector;

    fn add(self, rhs: StateVector) -> StateVector {
        StateVector::new(
            self.position_km + rhs.position_km,
            self.velocity_km_s + rhs.velocity_km_s,
        )
    }
}

impl std::ops::Sub for StateVector {
    type Output = StateVector;

    fn sub(self, rhs: StateVector) -> StateVector {
        StateVector::new(
            self.position_km - rhs.position_km,
            self.velocity_km_s - rhs.velocity_km_s,
        )
    }
}

/// Something that knows where it is relative to its parent frame, as a
/// function of time.
///
/// Implemented by the low-precision planetary series in [`crate::planets`]
/// for the tree's top-level bodies, and by a two-body propagator for a
/// spacecraft between the moments its trajectory is re-planned. Either way,
/// the tree itself only needs the state this returns, never how it was
/// worked out.
pub trait Ephemeris: std::fmt::Debug + Send + Sync {
    /// This body's state relative to its parent frame, at the given epoch.
    fn state_at(&self, epoch: Epoch) -> StateVector;
}

/// An [`Ephemeris`] for a frame that sits exactly on its parent — the root's
/// own placeholder, and a convenient stand-in in tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct FixedAtParent;

impl Ephemeris for FixedAtParent {
    fn state_at(&self, _epoch: Epoch) -> StateVector {
        StateVector::ZERO
    }
}

/// An [`Ephemeris`] for a frame at a constant state relative to its parent —
/// convenient for tests, and for a placeholder body whose real ephemeris
/// isn't wired up yet.
#[derive(Debug, Clone, Copy)]
pub struct FixedOffset(pub StateVector);

impl Ephemeris for FixedOffset {
    fn state_at(&self, _epoch: Epoch) -> StateVector {
        self.0
    }
}

/// A node in the [`FrameTree`], identified by the arena index it was inserted
/// at.
///
/// Cheap to copy and compare, which matters because a spacecraft holds one of
/// these as *which body it currently orbits* and swaps it every tick it
/// checks — see [`crate::spacecraft::Spacecraft`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameId(u32);

struct Node {
    name: &'static str,
    parent: Option<FrameId>,
    ephemeris: Box<dyn Ephemeris>,
}

/// The tree itself: an arena of frames, each pointing at its parent, rooted
/// at the Solar System Barycentre.
///
/// Nothing here is a Bevy resource — `terramenta-solare` has no dependency on
/// Bevy or on any renderer. It is meant to be wrapped in one by whatever
/// embeds it, the way [`crate`] deliberately mirrors `terramenta-globe`'s own
/// split between a plain-Rust model and the app that drives it.
pub struct FrameTree {
    nodes: Vec<Node>,
    root: FrameId,
    by_name: HashMap<&'static str, FrameId>,
}

impl FrameTree {
    /// The Solar System Barycentre's name, for lookups via [`FrameTree::find`].
    pub const SSB: &'static str = "SSB";

    /// A new tree with nothing in it but the root.
    pub fn new() -> Self {
        let root_node = Node {
            name: Self::SSB,
            parent: None,
            ephemeris: Box::new(FixedAtParent),
        };
        let mut by_name = HashMap::new();
        by_name.insert(Self::SSB, FrameId(0));
        Self {
            nodes: vec![root_node],
            root: FrameId(0),
            by_name,
        }
    }

    /// The Solar System Barycentre, the root every other frame ultimately
    /// hangs off.
    pub fn root(&self) -> FrameId {
        self.root
    }

    /// Adds a body as a child of `parent`, reporting its own position through
    /// `ephemeris`.
    ///
    /// # Panics
    ///
    /// If `parent` is not a frame in this tree, or `name` is already taken by
    /// another frame in it — [`FrameTree::find`] can only ever return one
    /// [`FrameId`] per name, so a silent second write here would leave the
    /// first frame in the tree but unreachable by name.
    pub fn add(
        &mut self,
        name: &'static str,
        parent: FrameId,
        ephemeris: impl Ephemeris + 'static,
    ) -> FrameId {
        assert!(
            (parent.0 as usize) < self.nodes.len(),
            "parent frame {parent:?} is not in this tree"
        );
        assert!(
            !self.by_name.contains_key(name),
            "a frame named {name:?} already exists in this tree"
        );
        let id = FrameId(self.nodes.len() as u32);
        self.nodes.push(Node {
            name,
            parent: Some(parent),
            ephemeris: Box::new(ephemeris),
        });
        self.by_name.insert(name, id);
        id
    }

    /// Looks up a frame by the name it was added under.
    pub fn find(&self, name: &str) -> Option<FrameId> {
        self.by_name.get(name).copied()
    }

    pub fn name(&self, frame: FrameId) -> &'static str {
        self.node(frame).name
    }

    pub fn parent(&self, frame: FrameId) -> Option<FrameId> {
        self.node(frame).parent
    }

    /// Replaces the ephemeris a frame reports its state through, and — when
    /// `new_parent` is given — moves it to a different parent at the same
    /// time.
    ///
    /// This is the operation a spacecraft crossing a sphere of influence
    /// needs: see [`crate::spacecraft::Spacecraft::retarget`], which computes
    /// the state relative to the *new* parent before calling this, so the
    /// body's actual position in space — its state relative to the SSB —
    /// does not jump when the parent changes. `FrameTree` itself does not
    /// enforce that continuity; it only stores whatever ephemeris it is
    /// handed.
    pub fn reparent(
        &mut self,
        frame: FrameId,
        new_parent: FrameId,
        ephemeris: impl Ephemeris + 'static,
    ) {
        assert!(
            (new_parent.0 as usize) < self.nodes.len(),
            "parent frame {new_parent:?} is not in this tree"
        );
        let node = self.node_mut(frame);
        node.parent = Some(new_parent);
        node.ephemeris = Box::new(ephemeris);
    }

    /// This frame's state relative to its immediate parent.
    pub fn local_state(&self, frame: FrameId, epoch: Epoch) -> StateVector {
        self.node(frame).ephemeris.state_at(epoch)
    }

    /// This frame's state relative to the root (the SSB) — the sum of every
    /// local state from here up the chain of parents.
    ///
    /// Lets two frames anywhere in the tree be compared: walk each to a
    /// shared, fixed reference and difference there. Comparing two things
    /// that are actually close together this way reintroduces the precision
    /// loss the tree exists to avoid — prefer
    /// [`FrameTree::state_of_relative_to`], which never sums a chain longer
    /// than the two frames' nearest common ancestor requires.
    pub fn state_relative_to_root(&self, frame: FrameId, epoch: Epoch) -> StateVector {
        let mut state = StateVector::ZERO;
        let mut current = frame;
        while let Some(parent) = self.parent(current) {
            state = state + self.local_state(current, epoch);
            current = parent;
        }
        state
    }

    /// `frame`'s state relative to `other`, found by walking each up to their
    /// nearest common ancestor rather than all the way to the SSB.
    ///
    /// A spacecraft in Earth orbit asking where it is relative to Earth, or
    /// Earth asking where it is relative to Mars, only needs to look as far
    /// up the tree as the two frames' lowest common ancestor — for a
    /// spacecraft that is its parent directly, at zero hops. Going by way of
    /// the SSB would add Earth's and Mars's own
    /// multi-hundred-million-kilometre states into a subtraction meant to
    /// land on a distance of a few hundred kilometres, reintroducing the
    /// precision loss the tree is for.
    pub fn state_of_relative_to(
        &self,
        frame: FrameId,
        other: FrameId,
        epoch: Epoch,
    ) -> StateVector {
        let (ancestors_of_frame, ancestors_of_other) =
            (self.ancestors(frame), self.ancestors(other));

        // The nearest common ancestor is the first of `frame`'s ancestors
        // (root-most last) that also appears among `other`'s.
        let common = ancestors_of_frame
            .iter()
            .find(|candidate| ancestors_of_other.contains(candidate))
            .copied()
            .unwrap_or(self.root);

        let up_from_frame = self.state_up_to(frame, common, epoch);
        let up_from_other = self.state_up_to(other, common, epoch);
        up_from_frame - up_from_other
    }

    /// `frame`'s state relative to `ancestor`, which must be `frame` itself or
    /// one of its ancestors.
    fn state_up_to(&self, frame: FrameId, ancestor: FrameId, epoch: Epoch) -> StateVector {
        let mut state = StateVector::ZERO;
        let mut current = frame;
        while current != ancestor {
            let Some(parent) = self.parent(current) else {
                break;
            };
            state = state + self.local_state(current, epoch);
            current = parent;
        }
        state
    }

    /// `frame`, then its parent, then its parent's parent, and so on up to
    /// and including the root — nearest first.
    fn ancestors(&self, frame: FrameId) -> Vec<FrameId> {
        let mut chain = vec![frame];
        let mut current = frame;
        while let Some(parent) = self.parent(current) {
            chain.push(parent);
            current = parent;
        }
        chain
    }

    fn node(&self, frame: FrameId) -> &Node {
        &self.nodes[frame.0 as usize]
    }

    fn node_mut(&mut self, frame: FrameId) -> &mut Node {
        &mut self.nodes[frame.0 as usize]
    }
}

impl Default for FrameTree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body on a fixed offset from its parent — enough to exercise the tree
    /// without pulling in a real ephemeris.
    #[derive(Debug, Clone, Copy)]
    struct Offset(DVec3);

    impl Ephemeris for Offset {
        fn state_at(&self, _epoch: Epoch) -> StateVector {
            StateVector::new(self.0, DVec3::ZERO)
        }
    }

    fn sample_tree() -> (FrameTree, FrameId, FrameId, FrameId) {
        let mut tree = FrameTree::new();
        let root = tree.root();
        let sun = tree.add("Sun", root, Offset(DVec3::new(1.0, 0.0, 0.0)));
        let earth = tree.add("Earth", root, Offset(DVec3::new(0.0, 2.0, 0.0)));
        let spacecraft = tree.add("Spacecraft", earth, Offset(DVec3::new(0.0, 0.0, 3.0)));
        (tree, sun, earth, spacecraft)
    }

    #[test]
    fn root_has_no_parent_and_sits_at_the_origin() {
        let tree = FrameTree::new();
        assert_eq!(tree.parent(tree.root()), None);
        assert_eq!(
            tree.state_relative_to_root(tree.root(), Epoch::J2000),
            StateVector::ZERO
        );
    }

    #[test]
    fn state_relative_to_root_sums_the_chain_of_parents() {
        let (tree, _sun, _earth, spacecraft) = sample_tree();
        let state = tree.state_relative_to_root(spacecraft, Epoch::J2000);
        // Earth's offset from the SSB, plus the spacecraft's offset from Earth.
        assert_eq!(state.position_km, DVec3::new(0.0, 2.0, 3.0));
    }

    #[test]
    fn state_between_siblings_does_not_go_through_the_root() {
        let (tree, sun, earth, _spacecraft) = sample_tree();
        // Sun at (1, 0, 0), Earth at (0, 2, 0): Earth relative to Sun is
        // their difference.
        let earth_from_sun = tree.state_of_relative_to(earth, sun, Epoch::J2000);
        assert_eq!(earth_from_sun.position_km, DVec3::new(-1.0, 2.0, 0.0));
    }

    #[test]
    fn state_relative_to_own_parent_matches_local_state() {
        let (tree, _sun, earth, spacecraft) = sample_tree();
        let local = tree.local_state(spacecraft, Epoch::J2000);
        let relative = tree.state_of_relative_to(spacecraft, earth, Epoch::J2000);
        assert_eq!(local, relative);
    }

    #[test]
    fn state_of_a_frame_relative_to_itself_is_zero() {
        let (tree, _sun, _earth, spacecraft) = sample_tree();
        assert_eq!(
            tree.state_of_relative_to(spacecraft, spacecraft, Epoch::J2000),
            StateVector::ZERO
        );
    }

    #[test]
    fn reparenting_can_preserve_the_state_relative_to_root() {
        let (mut tree, sun, earth, spacecraft) = sample_tree();
        let before = tree.state_relative_to_root(spacecraft, Epoch::J2000);

        // Move the spacecraft to orbit the Sun instead of the Earth, handing
        // it the offset from the Sun that lands it in the same place — the
        // continuity `Spacecraft::retarget` is responsible for keeping.
        let sun_state = tree.state_relative_to_root(sun, Epoch::J2000);
        let new_offset = before.position_km - sun_state.position_km;
        tree.reparent(spacecraft, sun, Offset(new_offset));

        assert_eq!(tree.parent(spacecraft), Some(sun));
        let after = tree.state_relative_to_root(spacecraft, Epoch::J2000);
        assert!((after.position_km - before.position_km).length() < 1.0e-9);
        let _ = earth;
    }

    #[test]
    fn find_looks_up_a_frame_by_name() {
        let mut tree = FrameTree::new();
        let earth = tree.add("Earth", tree.root(), FixedAtParent);
        assert_eq!(tree.find("Earth"), Some(earth));
        assert_eq!(tree.find(FrameTree::SSB), Some(tree.root()));
        assert_eq!(tree.find("Pluto"), None);
    }

    #[test]
    #[should_panic(expected = "a frame named \"Earth\" already exists")]
    fn adding_a_second_frame_under_a_taken_name_panics() {
        let mut tree = FrameTree::new();
        tree.add("Earth", tree.root(), FixedAtParent);
        tree.add("Earth", tree.root(), FixedAtParent);
    }
}
