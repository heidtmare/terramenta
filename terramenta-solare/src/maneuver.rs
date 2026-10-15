//! Impulsive and finite maneuvers; local orbital frames.

use glam::{DMat3, DQuat, DVec3};

use crate::force::{ForceContext, ForceModel, G0_M_S2};
use crate::time::Epoch;

/// Direction frame for a Δv or thrust vector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LocalFrame {
    /// Inertial axes of the propagation frame.
    #[default]
    Inertial,
    /// x = v̂, y = ĥ (orbit normal), z = x × y (binormal).
    Vnb,
    /// Radial / in-track / cross-track (Vallado RSW): x = r̂, z = ĥ, y = z × x.
    Rsw,
    /// CCSDS LVLH: z = −r̂, y = −ĥ, x = y × z.
    Lvlh,
}

impl LocalFrame {
    /// Columns are the local axes in inertial coordinates (local → inertial).
    pub fn to_inertial(self, r: DVec3, v: DVec3) -> DMat3 {
        let h = r.cross(v).normalize();
        match self {
            Self::Inertial => DMat3::IDENTITY,
            Self::Vnb => {
                let x = v.normalize();
                DMat3::from_cols(x, h, x.cross(h))
            }
            Self::Rsw => {
                let x = r.normalize();
                DMat3::from_cols(x, h.cross(x), h)
            }
            Self::Lvlh => {
                let (z, y) = (-r.normalize(), -h);
                DMat3::from_cols(y.cross(z), y, z)
            }
        }
    }
}

/// Instantaneous Δv. Mass drops per the rocket equation when `isp_s` is set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpulsiveManeuver {
    pub epoch: Epoch,
    pub delta_v_km_s: DVec3,
    pub frame: LocalFrame,
    pub isp_s: Option<f64>,
}

impl ImpulsiveManeuver {
    pub fn new(epoch: Epoch, delta_v_km_s: DVec3, frame: LocalFrame) -> Self {
        Self {
            epoch,
            delta_v_km_s,
            frame,
            isp_s: None,
        }
    }

    pub fn with_isp(mut self, isp_s: f64) -> Self {
        self.isp_s = Some(isp_s);
        self
    }

    /// `(Δv inertial, mass after)`.
    pub fn apply(&self, r: DVec3, v: DVec3, mass_kg: f64) -> (DVec3, f64) {
        let dv = self.frame.to_inertial(r, v) * self.delta_v_km_s;
        let mass = match self.isp_s {
            Some(isp) => mass_kg * (-dv.length() * 1e3 / (isp * G0_M_S2)).exp(),
            None => mass_kg,
        };
        (dv, mass)
    }

    /// Inverse of [`Self::apply`] from the post-burn state: `(Δv inertial, mass before)`.
    /// Local frames depend on the pre-burn velocity, so solve
    /// `v_pre = v_post − R(r, v_pre)·Δv` by fixed-point iteration (contraction ≈ |Δv|/|v|).
    pub fn undo(&self, r: DVec3, v_post: DVec3, mass_kg: f64) -> (DVec3, f64) {
        let mut dv = self.frame.to_inertial(r, v_post) * self.delta_v_km_s;
        if self.frame != LocalFrame::Inertial {
            for _ in 0..50 {
                let next = self.frame.to_inertial(r, v_post - dv) * self.delta_v_km_s;
                let done = (next - dv).length() <= 1e-15 * v_post.length();
                dv = next;
                if done {
                    break;
                }
            }
        }
        let mass = match self.isp_s {
            Some(isp) => mass_kg * (dv.length() * 1e3 / (isp * G0_M_S2)).exp(),
            None => mass_kg,
        };
        (dv, mass)
    }
}

/// Thrust direction frame: a local orbital frame, or body axes under the attitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnFrame {
    Local(LocalFrame),
    /// Body axes: needs [`crate::propagate::Propagator::attitude`].
    Body,
}

impl Default for BurnFrame {
    fn default() -> Self {
        Self::Local(LocalFrame::default())
    }
}

impl From<LocalFrame> for BurnFrame {
    fn from(frame: LocalFrame) -> Self {
        Self::Local(frame)
    }
}

impl BurnFrame {
    /// Frame axes in inertial coordinates; `None` for body axes without an attitude.
    pub fn to_inertial(self, r: DVec3, v: DVec3, attitude: Option<DQuat>) -> Option<DMat3> {
        match self {
            Self::Local(frame) => Some(frame.to_inertial(r, v)),
            Self::Body => attitude.map(DMat3::from_quat),
        }
    }
}

/// Constant-thrust burn over `[start, start + duration]`. Cuts off at `dry_mass_kg`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FiniteBurn {
    pub start: Epoch,
    pub duration_s: f64,
    pub thrust_n: f64,
    pub isp_s: f64,
    /// Unit vector in `frame`.
    pub direction: DVec3,
    pub frame: BurnFrame,
}

impl FiniteBurn {
    pub fn end(&self) -> Epoch {
        self.start.advanced_by_seconds(self.duration_s)
    }

    fn active(&self, ctx: &ForceContext) -> bool {
        let t = ctx.switch_epoch.seconds_since(self.start);
        (0.0..self.duration_s).contains(&t) && ctx.switch_mass_kg > ctx.vehicle.dry_mass_kg
    }
}

impl ForceModel for FiniteBurn {
    fn acceleration(&self, ctx: &ForceContext) -> DVec3 {
        if !self.active(ctx) {
            return DVec3::ZERO;
        }
        let Some(axes) = self
            .frame
            .to_inertial(ctx.position_km, ctx.velocity_km_s, ctx.attitude)
        else {
            return DVec3::ZERO;
        };
        self.thrust_n / ctx.mass_kg * 1e-3 * (axes * self.direction.normalize())
    }

    fn mass_rate(&self, ctx: &ForceContext) -> f64 {
        if self.active(ctx) {
            -self.thrust_n / (self.isp_s * G0_M_S2)
        } else {
            0.0
        }
    }

    fn discontinuities(&self) -> Vec<Epoch> {
        vec![self.start, self.end()]
    }

    fn requires_attitude(&self) -> bool {
        self.frame == BurnFrame::Body
    }

    fn velocity_dependent(&self) -> bool {
        self.frame != BurnFrame::Local(LocalFrame::Inertial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_frames_are_orthonormal_and_right_handed() {
        let (r, v) = (DVec3::new(7000.0, 100.0, -50.0), DVec3::new(0.5, 7.4, 1.0));
        for f in [LocalFrame::Vnb, LocalFrame::Rsw, LocalFrame::Lvlh] {
            let m = f.to_inertial(r, v);
            assert!(
                (m.transpose() * m - DMat3::IDENTITY).abs_diff_eq(DMat3::ZERO, 1e-14),
                "{f:?}"
            );
            assert!((m.determinant() - 1.0).abs() < 1e-14, "{f:?}");
        }
        let vnb = LocalFrame::Vnb.to_inertial(r, v);
        assert!((vnb.x_axis - v.normalize()).length() < 1e-15);
        let lvlh = LocalFrame::Lvlh.to_inertial(r, v);
        assert!((lvlh.z_axis + r.normalize()).length() < 1e-15);
        // LVLH x ≈ along-track for near-circular.
        assert!(lvlh.x_axis.dot(v.normalize()) > 0.99);
    }

    #[test]
    fn impulsive_rocket_equation() {
        let m = ImpulsiveManeuver::new(Epoch::J2000, DVec3::new(0.1, 0.0, 0.0), LocalFrame::Vnb)
            .with_isp(300.0);
        let (dv, mass) = m.apply(DVec3::X * 7000.0, DVec3::Y * 7.5, 1000.0);
        assert!((dv - DVec3::Y * 0.1).length() < 1e-15);
        assert!((mass - 1000.0 * (-100.0 / (300.0 * G0_M_S2)).exp()).abs() < 1e-9);
    }

    #[test]
    fn undo_inverts_apply() {
        let (r, v) = (DVec3::new(7000.0, 100.0, -50.0), DVec3::new(0.5, 7.4, 1.0));
        let dv_local = DVec3::new(0.3, -0.5, 0.2);
        for f in [
            LocalFrame::Inertial,
            LocalFrame::Vnb,
            LocalFrame::Rsw,
            LocalFrame::Lvlh,
        ] {
            let m = ImpulsiveManeuver::new(Epoch::J2000, dv_local, f).with_isp(300.0);
            let (dv, mass) = m.apply(r, v, 1000.0);
            let (dv_back, mass_back) = m.undo(r, v + dv, mass);
            assert!((v + dv - dv_back - v).length() < 1e-14, "{f:?}");
            assert!((mass_back - 1000.0).abs() < 1e-9, "{f:?}");
        }
    }
}
