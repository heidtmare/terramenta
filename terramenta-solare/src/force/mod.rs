//! Force models for numerical propagation.
//!
//! States are central-body-centred, ICRF axes, km, km/s, kg. Accelerations km/s².

use glam::{DMat3, DQuat, DVec3};

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
pub use gravity::{EGM2008_GM_KM3_S2, GravityField, PointMass, SphericalHarmonics};
pub use msis::Nrlmsise00;
pub use relativity::Relativity;
pub use srp::{SolarRadiationPressure, shadow_fraction};
pub use third_body::ThirdBody;

/// Standard gravity, m/s².
pub const G0_M_S2: f64 = 9.806_65;

/// Physical properties used by non-gravitational forces.
///
/// With [`Self::plates`], drag takes its area from the plates facing the flow (still with
/// [`Self::drag_coefficient`]) and SRP sums per-plate pressure; `drag_area_m2`,
/// `srp_area_m2` and `reflectivity` are then unused. Plates need an attitude
/// ([`crate::propagate::Propagator::attitude`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Vehicle {
    pub dry_mass_kg: f64,
    pub drag_area_m2: f64,
    pub drag_coefficient: f64,
    pub srp_area_m2: f64,
    pub reflectivity: f64,
    pub plates: Vec<Plate>,
}

/// One-sided flat plate, body axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plate {
    /// Outward unit normal.
    pub normal: DVec3,
    pub area_m2: f64,
    /// Specular reflectivity ρs.
    pub specular: f64,
    /// Diffuse reflectivity ρd; absorbed 1 − ρs − ρd.
    pub diffuse: f64,
}

impl Plate {
    /// Area projected on unit `toward` (body axes): `A·max(0, n̂·d̂)`.
    pub fn projected_area_m2(&self, toward: DVec3) -> f64 {
        self.area_m2 * self.normal.dot(toward).max(0.0)
    }

    /// Radiation force per unit pressure, m², body axes; `sun` the unit vector to the Sun.
    /// Montenbruck & Gill §3.4.2: `−A cosθ [(1 − ρs) ŝ + 2 (ρs cosθ + ρd / 3) n̂]`.
    pub fn radiation_m2(&self, sun: DVec3) -> DVec3 {
        let cos = self.normal.dot(sun);
        if cos <= 0.0 {
            return DVec3::ZERO;
        }
        -self.area_m2
            * cos
            * ((1.0 - self.specular) * sun
                + 2.0 * (self.specular * cos + self.diffuse / 3.0) * self.normal)
    }
}

impl Vehicle {
    /// Box plates: `±x, ±y, ±z` faces of areas `[a_x, a_y, a_z]`, one ρs, ρd.
    pub fn box_plates(areas_m2: [f64; 3], specular: f64, diffuse: f64) -> Vec<Plate> {
        DVec3::AXES
            .into_iter()
            .zip(areas_m2)
            .flat_map(|(axis, area_m2)| {
                [axis, -axis].map(|normal| Plate {
                    normal,
                    area_m2,
                    specular,
                    diffuse,
                })
            })
            .collect()
    }
}

impl Default for Vehicle {
    fn default() -> Self {
        Self {
            dry_mass_kg: 850.0,
            drag_area_m2: 15.0,
            drag_coefficient: 2.2,
            srp_area_m2: 1.0,
            reflectivity: 1.8,
            plates: Vec::new(),
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
    /// Body → inertial, when the propagator has an attitude and a force or plates need it;
    /// `None` also where the attitude law is undefined (the propagation then fails).
    pub attitude: Option<DQuat>,
}

impl ForceContext<'_> {
    /// Inertial → body-fixed rotation; identity if none.
    pub fn body_rotation(&self) -> DMat3 {
        self.body_fixed.map_or(DMat3::IDENTITY, |t| t.rotation())
    }

    /// [`Self::sun_km`], else `fallback` at `epoch`.
    pub fn sun(&self, fallback: &dyn Ephemeris) -> DVec3 {
        self.sun_km
            .unwrap_or_else(|| fallback.position_at(self.epoch))
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

    /// Needs [`ForceContext::attitude`] regardless of the vehicle.
    fn requires_attitude(&self) -> bool {
        false
    }

    /// Epochs where the model is discontinuous; the propagator stops on them.
    fn discontinuities(&self) -> Vec<Epoch> {
        Vec::new()
    }

    /// `(∂a/∂r, ∂a/∂v)`, s⁻², s⁻¹. `None`: the propagator takes central differences.
    fn partials(&self, _ctx: &ForceContext) -> Option<(DMat3, DMat3)> {
        None
    }

    /// Acceleration depends on velocity. `false` skips velocity differences.
    fn velocity_dependent(&self) -> bool {
        true
    }
}
