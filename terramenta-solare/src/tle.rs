//! SGP4/SDP4 via [`sgp4`]: TLE/OMM in, TEME or GCRF states out.
//!
//! AFSPC compatibility mode (WGS-72, as catalogue elements are fitted); matches Vallado's
//! verification set. Time since element epoch is taken in UTC (no leap seconds).

use glam::DVec3;

use crate::earth::teme_to_gcrf;
use crate::eop::Eop;
use crate::frame::{Ephemeris, StateVector};
use crate::time::Epoch;

pub use sgp4;

#[derive(Debug, Clone)]
pub enum Sgp4Error {
    Elements(String),
    Constants(String),
}

impl std::fmt::Display for Sgp4Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Elements(e) => write!(f, "elements: {e}"),
            Self::Constants(e) => write!(f, "SGP4 init: {e}"),
        }
    }
}

impl std::error::Error for Sgp4Error {}

/// [`Ephemeris`] output is GCRF with the stored `eop`; zero state where SGP4 fails.
#[derive(Debug, Clone)]
pub struct Sgp4Ephemeris {
    pub elements: sgp4::Elements,
    constants: sgp4::Constants,
    epoch: Epoch,
    pub eop: Eop,
}

impl Sgp4Ephemeris {
    pub fn from_tle(line1: &str, line2: &str) -> Result<Self, Sgp4Error> {
        let el = sgp4::Elements::from_tle(None, line1.as_bytes(), line2.as_bytes())
            .map_err(|e| Sgp4Error::Elements(e.to_string()))?;
        Self::from_elements(el)
    }

    /// From parsed elements (e.g. OMM via `serde_json`).
    pub fn from_elements(elements: sgp4::Elements) -> Result<Self, Sgp4Error> {
        let constants = sgp4::Constants::from_elements_afspc_compatibility_mode(&elements)
            .map_err(|e| Sgp4Error::Constants(e.to_string()))?;
        let unix = elements.datetime.and_utc().timestamp_micros() as f64 * 1e-6;
        Ok(Self {
            elements,
            constants,
            epoch: Epoch::from_unix_seconds(unix),
            eop: Eop::default(),
        })
    }

    pub fn with_eop(mut self, eop: Eop) -> Self {
        self.eop = eop;
        self
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// TEME, km and km/s. `None` on SGP4 error or non-finite output.
    pub fn state_teme(&self, epoch: Epoch) -> Option<StateVector> {
        let minutes = (epoch.to_unix_seconds() - self.epoch.to_unix_seconds()) / 60.0;
        let p = self
            .constants
            .propagate_afspc_compatibility_mode(sgp4::MinutesSinceEpoch(minutes))
            .ok()?;
        let (r, v) = (DVec3::from_array(p.position), DVec3::from_array(p.velocity));
        (r.is_finite() && v.is_finite()).then(|| StateVector::new(r, v))
    }

    /// GCRF via the IAU-76/FK5 chain. TEME is quasi-inertial: velocity rotates with position.
    pub fn state_gcrf(&self, epoch: Epoch, eop: &Eop) -> Option<StateVector> {
        let s = self.state_teme(epoch)?;
        let m = teme_to_gcrf(epoch, eop);
        Some(StateVector::new(m * s.position_km, m * s.velocity_km_s))
    }
}

impl Ephemeris for Sgp4Ephemeris {
    fn state_at(&self, epoch: Epoch) -> StateVector {
        self.state_gcrf(epoch, &self.eop)
            .unwrap_or(StateVector::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vallado et al. AIAA 2006-6753, SGP4-VER case 00005 (`tcppver.out`).
    #[test]
    fn vallado_case_00005() {
        let s = Sgp4Ephemeris::from_tle(
            "1 00005U 58002B   00179.78495062  .00000023  00000-0  28098-4 0  4753",
            "2 00005  34.2682 348.7242 1859667 331.7664  19.3264 10.82419157413667",
        )
        .unwrap();
        let t0 = s.state_teme(s.epoch()).unwrap();
        let r = DVec3::new(7022.46529266, -1400.08296755, 0.03995155);
        let v = DVec3::new(1.893841015, 6.405893759, 4.534807250);
        assert!((t0.position_km - r).length() < 1e-6, "{}", t0.position_km);
        assert!(
            (t0.velocity_km_s - v).length() < 1e-8,
            "{}",
            t0.velocity_km_s
        );
        let t1 = s
            .state_teme(s.epoch().advanced_by_seconds(360.0 * 60.0))
            .unwrap();
        let r1 = DVec3::new(-7154.03120202, -3783.17682504, -3536.19412294);
        assert!((t1.position_km - r1).length() < 1e-6, "{}", t1.position_km);
        // GCRF rotation preserves magnitude.
        let g = s.state_at(s.epoch());
        assert!((g.position_km.length() - t0.position_km.length()).abs() < 1e-9);
    }
}
