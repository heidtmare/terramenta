//! Which reference frame the scene is drawn in.
//!
//! World space is one frame or the other, never a mix:
//!
//! * **ECEF** — Earth-centred, Earth-fixed. The globe stands still; the
//!   camera keeps looking at the same ground point. The sun and stars sweep
//!   around the planet once a day.
//! * **ECI** — Earth-centred inertial. The stars and sun stand still (aside
//!   from the ~1°/day of Earth's orbital motion); the globe turns underneath
//!   at the sidereal rate. Orbits are flown in this frame, where a satellite
//!   track is a closed ellipse.
//!
//! Earth-fixed geometry — the globe mesh, imagery tiles, subsolar point — is
//! stored in ECEF and rotated into world space by
//! [`ReferenceFrame::earth_to_world`]. Converting a world-space direction back
//! to latitude/longitude requires [`ReferenceFrame::world_to_earth`].
//!
//! Switching frames rotates the globe by up to a full turn, so the switch
//! emits a [`FrameRealigned`] message; anything looking at the ground adds
//! that angle to stay pointed at the same place.
//!
//! [`FrameSet`] orders the frame update, camera update, and drawing on the
//! same tick. Reading the frame a tick early draws the globe at its old
//! orientation, or has the tile walk query visibility against a camera that
//! hasn't caught up, dropping imagery for a frame.

use bevy::prelude::*;

use crate::time::SimClock;

/// Seconds from the Unix epoch to J2000.0, the epoch the sidereal angle is
/// measured from.
const J2000_UNIX_SECONDS: f64 = 946_728_000.0;
/// Greenwich's sidereal angle at J2000.0.
const SIDEREAL_EPOCH_DEGREES: f64 = 280.460_618_37;
/// Degrees of sidereal rotation per day: a full turn plus the extra the Earth
/// has to make up to face the sun again, which is what makes a sidereal day
/// about four minutes short of a solar one.
pub(crate) const SIDEREAL_DEGREES_PER_DAY: f64 = 360.985_647_366_29;

pub(crate) const SECONDS_PER_DAY: f64 = 86_400.0;

/// The two frames the globe can be shown in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FrameMode {
    /// Earth-fixed: the ground is still, the sky moves.
    #[default]
    Ecef,
    /// Inertial: the sky is still, the ground moves.
    Eci,
}

impl FrameMode {
    /// The stable name the control surface names this frame by.
    pub fn id(self) -> &'static str {
        match self {
            Self::Ecef => "ecef",
            Self::Eci => "eci",
        }
    }

    /// Parses [`FrameMode::id`] back, for a frame named by an embedder.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "ecef" => Some(Self::Ecef),
            "eci" => Some(Self::Eci),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ecef => "ECEF · Earth-fixed",
            Self::Eci => "ECI · inertial",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Ecef => Self::Eci,
            Self::Eci => Self::Ecef,
        }
    }
}

/// The active frame, and the Earth rotation that relates the two.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ReferenceFrame {
    pub mode: FrameMode,
    /// Greenwich's angle east of the vernal equinox, in radians — the Greenwich
    /// mean sidereal time of the simulated clock.
    earth_rotation: f32,
}

/// Sent when the frame changes, carrying how far the ground moved under the
/// camera as a result — the whole Earth rotation angle, in radians about the
/// poles, gained on the way into ECI and given back on the way out.
#[derive(Message, Debug, Clone, Copy)]
pub struct FrameRealigned {
    pub ground_yaw: f32,
}

impl ReferenceFrame {
    /// How far Earth-fixed coordinates are turned in world space, in radians
    /// about the poles.
    pub(crate) fn earth_yaw(&self) -> f32 {
        match self.mode {
            FrameMode::Ecef => 0.0,
            FrameMode::Eci => self.earth_rotation,
        }
    }

    /// Takes an Earth-fixed direction or position into world space.
    ///
    /// In ECEF that is nothing at all, because world space *is* the Earth-fixed
    /// frame. In ECI it is the Earth's own rotation: the prime meridian sits at
    /// the sidereal angle rather than at 0° longitude.
    pub fn earth_to_world(&self) -> Quat {
        Quat::from_rotation_y(self.earth_yaw())
    }

    /// Inverse of [`ReferenceFrame::earth_to_world`], for turning something
    /// picked out of the scene back into a latitude and longitude.
    pub fn world_to_earth(&self) -> Quat {
        self.earth_to_world().inverse()
    }

    /// How far the sky is turned in world space, in radians about the poles.
    ///
    /// The stars are inertial, so this is the opposite of the Earth's rotation
    /// in ECEF and nothing in ECI.
    pub fn sky_rotation(&self) -> f32 {
        match self.mode {
            FrameMode::Ecef => -self.earth_rotation,
            FrameMode::Eci => 0.0,
        }
    }

    /// Takes an inertial direction or position into world space — the other
    /// half of [`ReferenceFrame::earth_to_world`], and the one an orbit is
    /// stated in.
    ///
    /// The stars are inertial and so is this, so it is exactly
    /// [`ReferenceFrame::sky_rotation`] as a quaternion: nothing in ECI, where
    /// world space *is* the inertial frame, and the Earth's rotation backwards
    /// in ECEF, where an inertial direction has to be unturned by however far
    /// Greenwich has come round.
    ///
    /// The two together are what lets both frames be drawn at once: geometry
    /// that belongs to the ground goes through `earth_to_world`, geometry that
    /// belongs to the stars goes through this, and the angle left between them
    /// on screen is the sidereal time. See [`crate::gnc`].
    pub fn inertial_to_world(&self) -> Quat {
        Quat::from_rotation_y(self.sky_rotation())
    }

    /// Greenwich's angle east of the vernal equinox, in radians — the Greenwich
    /// mean sidereal time of the simulated clock, whichever frame is being
    /// drawn in.
    ///
    /// This is the single number the two frames differ by, so anything that
    /// *reports* the relationship rather than drawing it needs it directly
    /// rather than through one of the quaternions above.
    pub fn earth_rotation(&self) -> f32 {
        self.earth_rotation
    }

    /// Points the Earth where the given moment says it should be.
    ///
    /// This runs once a tick from the simulated clock, and again whenever an
    /// embedder jumps that clock — a jump of days would otherwise be drawn at
    /// the previous tick's rotation until the next one caught up.
    pub(crate) fn sync_rotation(&mut self, unix_seconds: f64) {
        self.earth_rotation = sidereal_angle(unix_seconds);
    }
}

/// The order everything that touches the frame runs in, kept in one place
/// because a switch has to reach all of it within the tick it happens on.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameSet {
    /// Settles which frame this tick is drawn in, and announces a switch.
    Settle,
    /// Carries the camera across the switch, and finishes its transform.
    Camera,
    /// Everything read back out of the frame: the transforms the globe and its
    /// tiles are drawn with, the sun direction the shaders are lit by, the tile
    /// walk and the coordinate readout.
    Apply,
}

pub struct FramePlugin;

impl Plugin for FramePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ReferenceFrame>()
            .add_message::<FrameRealigned>()
            .configure_sets(
                Update,
                (FrameSet::Settle, FrameSet::Camera, FrameSet::Apply)
                    .chain()
                    .after(crate::time::advance_clock),
            )
            .add_systems(
                Update,
                // The rotation is a function of the simulated clock, so it is
                // only meaningful once the clock has been advanced for this
                // frame — and a toggle has to be measured against the rotation
                // as it is now, not as it was last frame.
                (sync_earth_rotation, frame_controls)
                    .chain()
                    .in_set(FrameSet::Settle),
            );
    }
}

pub(crate) fn frame_controls(
    keys: Res<ButtonInput<KeyCode>>,
    mut frame: ResMut<ReferenceFrame>,
    mut realigned: MessageWriter<FrameRealigned>,
) {
    if !keys.just_pressed(KeyCode::Space) {
        return;
    }

    let before = frame.earth_yaw();
    frame.mode = frame.mode.toggled();
    realigned.write(FrameRealigned {
        ground_yaw: frame.earth_yaw() - before,
    });
}

pub(crate) fn sync_earth_rotation(clock: Res<SimClock>, mut frame: ResMut<ReferenceFrame>) {
    frame.sync_rotation(clock.unix_seconds);
}

/// Greenwich mean sidereal time as an angle in radians.
fn sidereal_angle(unix_seconds: f64) -> f32 {
    sidereal_radians(unix_seconds) as f32
}

/// The same angle, unnarrowed.
///
/// This is the one number that relates an Earth-fixed coordinate to an inertial
/// one, so anything that *computes* in the inertial frame and has to land on the
/// ground needs it at the precision it was worked out at — see
/// [`crate::ephemeris`], which propagates orbits in TEME and turns the result
/// into the latitude and longitude the globe draws. Narrowing it here would put
/// about a metre of jitter under every satellite for no reason: the drawn
/// vertex is `f32` either way, but the arithmetic in between does not have to be.
pub(crate) fn sidereal_radians(unix_seconds: f64) -> f64 {
    let days_since_j2000 = (unix_seconds - J2000_UNIX_SECONDS) / SECONDS_PER_DAY;
    let degrees =
        (SIDEREAL_EPOCH_DEGREES + SIDEREAL_DEGREES_PER_DAY * days_since_j2000).rem_euclid(360.0);
    degrees.to_radians()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidereal_angle_advances_one_turn_per_sidereal_day() {
        let start = sidereal_angle(J2000_UNIX_SECONDS);
        // A sidereal day is the solar day scaled by the ratio of the two rates.
        let sidereal_day = SECONDS_PER_DAY * 360.0 / SIDEREAL_DEGREES_PER_DAY;
        let later = sidereal_angle(J2000_UNIX_SECONDS + sidereal_day);
        assert!((later - start).abs() < 1.0e-3, "{later} vs {start}");
    }

    #[test]
    fn toggling_reports_the_whole_earth_rotation() {
        let mut frame = ReferenceFrame {
            mode: FrameMode::Ecef,
            earth_rotation: 1.2,
        };
        let into_eci = {
            let before = frame.earth_yaw();
            frame.mode = frame.mode.toggled();
            frame.earth_yaw() - before
        };
        let back_to_ecef = {
            let before = frame.earth_yaw();
            frame.mode = frame.mode.toggled();
            frame.earth_yaw() - before
        };
        assert_eq!(into_eci, 1.2);
        assert_eq!(back_to_ecef, -1.2);
    }

    #[test]
    fn ecef_leaves_earth_fixed_coordinates_alone() {
        let frame = ReferenceFrame {
            mode: FrameMode::Ecef,
            earth_rotation: 1.2,
        };
        assert_eq!(frame.earth_to_world(), Quat::IDENTITY);
        assert_eq!(frame.sky_rotation(), -1.2);
    }

    #[test]
    fn eci_turns_the_earth_and_holds_the_sky() {
        let frame = ReferenceFrame {
            mode: FrameMode::Eci,
            earth_rotation: 1.2,
        };
        let greenwich = crate::geo::LatLon::new(0.0, 0.0).to_direction();
        let placed = crate::geo::LatLon::from_direction(frame.earth_to_world() * greenwich);
        assert!(
            (placed.lon - 1.2_f32.to_degrees()).abs() < 1.0e-2,
            "{placed:?}"
        );
        assert_eq!(frame.sky_rotation(), 0.0);
    }

    #[test]
    fn the_two_frames_differ_by_the_earth_rotation_in_both_modes() {
        // Whichever frame world space is, the angle left between the Earth-fixed
        // basis and the inertial one is the sidereal time — which is the whole
        // reason both can be drawn at once and read against each other.
        for mode in [FrameMode::Ecef, FrameMode::Eci] {
            let frame = ReferenceFrame {
                mode,
                earth_rotation: 1.2,
            };
            let ground = frame.earth_to_world() * Vec3::Z;
            let inertial = frame.inertial_to_world() * Vec3::Z;
            let between = crate::geo::LatLon::from_direction(ground).lon
                - crate::geo::LatLon::from_direction(inertial).lon;
            assert!(
                (between - 1.2_f32.to_degrees()).abs() < 1.0e-2,
                "{mode:?}: {between}"
            );
        }
    }

    #[test]
    fn world_to_earth_undoes_earth_to_world() {
        let frame = ReferenceFrame {
            mode: FrameMode::Eci,
            earth_rotation: 2.5,
        };
        let point = crate::geo::LatLon::new(35.0, -120.0).to_direction();
        let round_trip = frame.world_to_earth() * (frame.earth_to_world() * point);
        assert!((round_trip - point).length() < 1.0e-5);
    }
}
