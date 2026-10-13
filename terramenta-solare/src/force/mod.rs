//! Force models for numerical propagation.
//!
//! States are central-body-centred, ICRF axes, km, km/s, kg. Accelerations km/s².

use glam::{DMat3, DVec3};

use crate::earth::TerrestrialTransform;
use crate::frame::Ephemeris;
use crate::time::Epoch;

pub mod drag;
pub mod gravity;
pub mod msis;
pub mod relativity;
pub mod srp;
pub mod third_body;

pub use drag::{Atmosphere, Drag, ExponentialAtmosphere, HarrisPriester};
pub use gravity::{GravityField, PointMass, SphericalHarmonics};
pub use msis::Nrlmsise00;
pub use relativity::Relativity;
pub use srp::{SolarRadiationPressure, shadow_fraction};
pub use third_body::ThirdBody;

/// Standard gravity, m/s².
pub const G0_M_S2: f64 = 9.806_65;

/// Physical properties used by non-gravitational forces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vehicle {
    pub dry_mass_kg: f64,
    pub drag_area_m2: f64,
    pub drag_coefficient: f64,
    pub srp_area_m2: f64,
    pub reflectivity: f64,
}

impl Default for Vehicle {
    fn default() -> Self {
        Self {
            dry_mass_kg: 850.0,
            drag_area_m2: 15.0,
            drag_coefficient: 2.2,
            srp_area_m2: 1.0,
            reflectivity: 1.8,
        }
    }
}

/// Inputs to [`ForceModel`] at one evaluation.
#[derive(Debug, Clone, Copy)]
pub struct ForceContext<'a> {
    pub epoch: Epoch,
    pub position_km: DVec3,
    pub velocity_km_s: DVec3,
    pub mass_kg: f64,
    pub vehicle: &'a Vehicle,
    /// Inertial → body-fixed, when the propagator has a body orientation.
    pub body_fixed: Option<&'a TerrestrialTransform>,
    /// Epoch for on/off switching: midpoint of the current segment between
    /// [`ForceModel::discontinuities`], so stages on a segment boundary see the segment's regime.
    pub switch_epoch: Epoch,
    /// Mass for on/off switching: mass at the start of the current segment.
    pub switch_mass_kg: f64,
    /// Sun relative to the central body at `epoch`, evaluated once per stage by the
    /// propagator. Overrides per-force Sun ephemerides.
    pub sun_km: Option<DVec3>,
}

impl ForceContext<'_> {
    /// Inertial → body-fixed rotation; identity if none.
    pub fn body_rotation(&self) -> DMat3 {
        self.body_fixed.map_or(DMat3::IDENTITY, |t| t.rotation())
    }

    /// [`Self::sun_km`], else `fallback` at `epoch`.
    pub fn sun(&self, fallback: &dyn Ephemeris) -> DVec3 {
        self.sun_km
            .unwrap_or_else(|| fallback.state_at(self.epoch).position_km)
    }
}

pub trait ForceModel: std::fmt::Debug + Send + Sync {
    fn acceleration(&self, ctx: &ForceContext) -> DVec3;

    /// dm/dt, kg/s.
    fn mass_rate(&self, _ctx: &ForceContext) -> f64 {
        0.0
    }

    /// Needs [`ForceContext::body_fixed`].
    fn requires_body_fixed(&self) -> bool {
        false
    }

    /// Epochs where the model is discontinuous; the propagator stops on them.
    fn discontinuities(&self) -> Vec<Epoch> {
        Vec::new()
    }
}
