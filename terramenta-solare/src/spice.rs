//! SPICE kernel ephemerides via [`anise`] (DE440/DE440s and other SPK).
//!
//! States are geometric (no aberration), ICRF ("J2000" in SPICE), km and km/s, TDB.
//! Outside kernel coverage, [`KernelEphemeris`] uses its fallback, else returns
//! [`StateVector::ZERO`].

use std::sync::Arc;

use anise::prelude::{Almanac, Frame};
use glam::DVec3;

use crate::frame::{Ephemeris, FrameTree, StateVector};
use crate::time::Epoch;
use crate::{planets, sun};

pub use anise;

/// NAIF IDs.
pub mod naif {
    pub const SSB: i32 = 0;
    pub const MERCURY_BARYCENTER: i32 = 1;
    pub const VENUS_BARYCENTER: i32 = 2;
    pub const EARTH_MOON_BARYCENTER: i32 = 3;
    pub const MARS_BARYCENTER: i32 = 4;
    pub const JUPITER_BARYCENTER: i32 = 5;
    pub const SATURN_BARYCENTER: i32 = 6;
    pub const URANUS_BARYCENTER: i32 = 7;
    pub const NEPTUNE_BARYCENTER: i32 = 8;
    pub const PLUTO_BARYCENTER: i32 = 9;
    pub const SUN: i32 = 10;
    pub const MOON: i32 = 301;
    pub const EARTH: i32 = 399;
}

pub type KernelError = anise::errors::AlmanacError;

/// Loads an SPK/BPC/PCK from memory (WASM-safe).
pub fn load_kernel_bytes(almanac: Almanac, bytes: &[u8]) -> Result<Almanac, KernelError> {
    almanac.load_from_bytes(bytes::BytesMut::from(bytes))
}

/// Loads an SPK/BPC/PCK from disk.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_kernel_file(almanac: Almanac, path: &str) -> Result<Almanac, KernelError> {
    almanac.load(path)
}

/// `target` relative to `observer`, from loaded kernels.
#[derive(Clone)]
pub struct KernelEphemeris {
    almanac: Arc<Almanac>,
    target: i32,
    observer: i32,
    fallback: Option<Arc<dyn Ephemeris>>,
}

impl std::fmt::Debug for KernelEphemeris {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelEphemeris")
            .field("target", &self.target)
            .field("observer", &self.observer)
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}

impl KernelEphemeris {
    pub fn new(almanac: Arc<Almanac>, target: i32, observer: i32) -> Self {
        Self {
            almanac,
            target,
            observer,
            fallback: None,
        }
    }

    /// Ephemeris used outside kernel coverage. Must share `observer`.
    pub fn with_fallback(mut self, fallback: impl Ephemeris + 'static) -> Self {
        self.fallback = Some(Arc::new(fallback));
        self
    }

    pub fn try_state_at(&self, epoch: Epoch) -> Option<StateVector> {
        let s = self
            .almanac
            .translate(
                Frame::from_ephem_j2000(self.target),
                Frame::from_ephem_j2000(self.observer),
                epoch.as_hifitime(),
                None,
            )
            .ok()?;
        Some(StateVector::new(
            DVec3::new(s.radius_km.x, s.radius_km.y, s.radius_km.z),
            DVec3::new(s.velocity_km_s.x, s.velocity_km_s.y, s.velocity_km_s.z),
        ))
    }
}

impl Ephemeris for KernelEphemeris {
    fn state_at(&self, epoch: Epoch) -> StateVector {
        self.try_state_at(epoch)
            .or_else(|| self.fallback.as_ref().map(|f| f.state_at(epoch)))
            .unwrap_or(StateVector::ZERO)
    }
}

/// [`crate::solar_system`] backed by kernels.
///
/// Root SSB; children Sun, Mercury, Venus, Earth, Mars, Jupiter, Saturn, Uranus, Neptune, Pluto
/// (barycentres except Earth); Moon under Earth. Sun, Earth, Mars fall back to the
/// low-precision series outside coverage.
pub fn solar_system_from_kernels(almanac: Arc<Almanac>) -> FrameTree {
    use naif::*;
    let k = |target, observer| KernelEphemeris::new(almanac.clone(), target, observer);
    let mut tree = FrameTree::new();
    let root = tree.root();
    tree.add("Sun", root, k(SUN, SSB).with_fallback(sun::Sun));
    tree.add("Mercury", root, k(MERCURY_BARYCENTER, SSB));
    tree.add("Venus", root, k(VENUS_BARYCENTER, SSB));
    let earth = tree.add("Earth", root, k(EARTH, SSB).with_fallback(planets::EARTH));
    tree.add("Moon", earth, k(MOON, EARTH));
    tree.add(
        "Mars",
        root,
        k(MARS_BARYCENTER, SSB).with_fallback(planets::MARS),
    );
    tree.add("Jupiter", root, k(JUPITER_BARYCENTER, SSB));
    tree.add("Saturn", root, k(SATURN_BARYCENTER, SSB));
    tree.add("Uranus", root, k(URANUS_BARYCENTER, SSB));
    tree.add("Neptune", root, k(NEPTUNE_BARYCENTER, SSB));
    tree.add("Pluto", root, k(PLUTO_BARYCENTER, SSB));
    tree
}
