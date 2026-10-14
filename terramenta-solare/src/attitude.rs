//! Attitude laws: align/constrain pointing, inertial hold, spin, LVLH Euler angles.
//!
//! Quaternions are body → inertial (`v_inertial = q * v_body`), the frame rotation that
//! carries inertial axes onto body axes; CCSDS AEM `ATTITUDE_DIR = A2B` with A inertial.

use glam::{DMat3, DQuat, DVec3};

use crate::maneuver::LocalFrame;
use crate::time::Epoch;

/// What a pointing direction is resolved against: geocentric inertial vectors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub position_km: DVec3,
    pub velocity_km_s: DVec3,
    /// Sun, geocentric.
    pub sun_km: DVec3,
    /// Pointing target (e.g. a ground site), geocentric.
    pub target_km: Option<DVec3>,
}

/// An inertial direction derived from [`Geometry`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    /// −r̂.
    Nadir,
    /// r̂.
    Zenith,
    /// v̂.
    Velocity,
    AntiVelocity,
    /// ĥ = (r × v)̂.
    OrbitNormal,
    AntiOrbitNormal,
    /// Spacecraft → Sun.
    Sun,
    AntiSun,
    /// Spacecraft → target.
    Target,
    /// Fixed inertial direction.
    Inertial(DVec3),
}

impl Direction {
    /// Unit vector, or `None` when undefined (no target, zero vector).
    pub fn resolve(self, g: &Geometry) -> Option<DVec3> {
        let (r, v) = (g.position_km, g.velocity_km_s);
        let d = match self {
            Self::Nadir => -r,
            Self::Zenith => r,
            Self::Velocity => v,
            Self::AntiVelocity => -v,
            Self::OrbitNormal => r.cross(v),
            Self::AntiOrbitNormal => v.cross(r),
            Self::Sun => g.sun_km - r,
            Self::AntiSun => r - g.sun_km,
            Self::Target => g.target_km? - r,
            Self::Inertial(d) => d,
        };
        d.try_normalize()
    }
}

/// Body `primary` exactly along `primary_to`; body `secondary` as close to `secondary_to`
/// as the first allows (TRIAD).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlignConstrain {
    pub primary: DVec3,
    pub primary_to: Direction,
    pub secondary: DVec3,
    pub secondary_to: Direction,
}

impl AlignConstrain {
    /// `None` when either pair is parallel or a direction is undefined.
    pub fn attitude(&self, g: &Geometry) -> Option<DQuat> {
        let body = triad(self.primary, self.secondary)?;
        let inertial = triad(self.primary_to.resolve(g)?, self.secondary_to.resolve(g)?)?;
        Some(DQuat::from_mat3(&(inertial * body.transpose())).normalize())
    }
}

/// Orthonormal columns: â, (a × b)̂, â × (a × b)̂.
fn triad(a: DVec3, b: DVec3) -> Option<DMat3> {
    let t1 = a.try_normalize()?;
    let t2 = t1.cross(b).try_normalize()?;
    Some(DMat3::from_cols(t1, t2, t1.cross(t2)))
}

/// Base law.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Law {
    /// Fixed body → inertial quaternion.
    Inertial(DQuat),
    AlignConstrain(AlignConstrain),
}

/// Rotation about a body axis on top of the base law: `rate_rad_s · (t − epoch)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spin {
    pub axis: DVec3,
    pub rate_rad_s: f64,
    pub epoch: Epoch,
}

/// Base law plus optional spin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Profile {
    pub law: Law,
    pub spin: Option<Spin>,
}

impl Profile {
    /// Body → inertial at `epoch`; `None` where the law is undefined.
    pub fn attitude(&self, epoch: Epoch, g: &Geometry) -> Option<DQuat> {
        let base = match self.law {
            Law::Inertial(q) => {
                let n = q.length();
                if !(n > 0.0 && n.is_finite()) {
                    return None;
                }
                q / n
            }
            Law::AlignConstrain(a) => a.attitude(g)?,
        };
        Some(match self.spin {
            Some(s) => {
                let angle = s.rate_rad_s * epoch.seconds_since(s.epoch);
                (base * DQuat::from_axis_angle(s.axis.try_normalize()?, angle)).normalize()
            }
            None => base,
        })
    }
}

/// Angle between body axis `body` and inertial unit `direction`, rad.
pub fn off_axis_angle(q: DQuat, body: DVec3, direction: DVec3) -> f64 {
    (q * body.normalize()).angle_between(direction)
}

/// Body relative to CCSDS LVLH (z nadir, y −ĥ) as 3-2-1 Euler angles `(roll, pitch, yaw)`,
/// rad: body → LVLH = Rz(yaw)·Ry(pitch)·Rx(roll).
pub fn lvlh_euler_321(q: DQuat, r: DVec3, v: DVec3) -> DVec3 {
    let m = LocalFrame::Lvlh.to_inertial(r, v).transpose() * DMat3::from_quat(q);
    // m.col(c)[row]
    let pitch = -m.x_axis.z.clamp(-1.0, 1.0).asin();
    let roll = m.y_axis.z.atan2(m.z_axis.z);
    let yaw = m.x_axis.y.atan2(m.x_axis.x);
    DVec3::new(roll, pitch, yaw)
}

/// Quaternion between samples: shortest-path slerp.
pub fn slerp(a: DQuat, b: DQuat, s: f64) -> DQuat {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    a.slerp(b, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leo() -> Geometry {
        Geometry {
            position_km: DVec3::new(7000.0, 0.0, 0.0),
            velocity_km_s: DVec3::new(0.0, 7.5, 0.0),
            sun_km: DVec3::new(0.0, 0.0, 1.5e8),
            target_km: Some(DVec3::new(6378.0, 100.0, 0.0)),
        }
    }

    fn close(a: DVec3, b: DVec3) -> bool {
        (a - b).length() < 1e-9
    }

    #[test]
    fn nadir_velocity_is_lvlh() {
        let g = leo();
        let q = AlignConstrain {
            primary: DVec3::Z,
            primary_to: Direction::Nadir,
            secondary: DVec3::X,
            secondary_to: Direction::Velocity,
        }
        .attitude(&g)
        .unwrap();
        assert!(close(q * DVec3::Z, -DVec3::X));
        assert!(close(q * DVec3::X, DVec3::Y));
        assert!(close(q * DVec3::Y, -DVec3::Z), "y = −ĥ");
        let euler = lvlh_euler_321(q, g.position_km, g.velocity_km_s);
        assert!(euler.length() < 1e-9, "{euler}");
    }

    #[test]
    fn secondary_is_only_a_constraint() {
        let g = leo();
        // Sun ≈ ⟂ line of sight (parallax 5e-5 rad): −X reaches it.
        let q = AlignConstrain {
            primary: DVec3::Z,
            primary_to: Direction::Target,
            secondary: -DVec3::X,
            secondary_to: Direction::Sun,
        }
        .attitude(&g)
        .unwrap();
        let to_target = (g.target_km.unwrap() - g.position_km).normalize();
        assert!(close(q * DVec3::Z, to_target));
        let sun = Direction::Sun.resolve(&g).unwrap();
        assert!(off_axis_angle(q, -DVec3::X, sun) < 1e-4);
    }

    #[test]
    fn degenerate_and_missing_directions_are_none() {
        let mut g = leo();
        let parallel = AlignConstrain {
            primary: DVec3::Z,
            primary_to: Direction::Zenith,
            secondary: DVec3::X,
            secondary_to: Direction::Nadir,
        };
        assert!(parallel.attitude(&g).is_none());
        g.target_km = None;
        assert!(Direction::Target.resolve(&g).is_none());
    }

    #[test]
    fn euler_angles_read_back() {
        let g = leo();
        let lvlh = LocalFrame::Lvlh.to_inertial(g.position_km, g.velocity_km_s);
        let (roll, pitch, yaw) = (0.1, -0.2, 0.3);
        let body_to_lvlh = DMat3::from_rotation_z(yaw)
            * DMat3::from_rotation_y(pitch)
            * DMat3::from_rotation_x(roll);
        let q = DQuat::from_mat3(&(lvlh * body_to_lvlh));
        let euler = lvlh_euler_321(q, g.position_km, g.velocity_km_s);
        assert!(close(euler, DVec3::new(roll, pitch, yaw)), "{euler}");
    }

    #[test]
    fn spin_turns_about_the_body_axis() {
        let t0 = Epoch::from_unix_seconds(0.0);
        let profile = Profile {
            law: Law::Inertial(DQuat::IDENTITY),
            spin: Some(Spin {
                axis: DVec3::Z,
                rate_rad_s: std::f64::consts::FRAC_PI_2 / 10.0,
                epoch: t0,
            }),
        };
        let q = profile
            .attitude(t0.advanced_by_seconds(10.0), &leo())
            .unwrap();
        assert!(close(q * DVec3::X, DVec3::Y));
        assert!(close(q * DVec3::Z, DVec3::Z));
        let half = slerp(DQuat::IDENTITY, -q, 0.5);
        assert!(close(
            half * DVec3::X,
            DVec3::new(1.0, 1.0, 0.0).normalize()
        ));
    }
}
