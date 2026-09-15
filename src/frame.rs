//! Which reference frame the scene is drawn in.
//!
//! World space is one frame or the other, never a mix of the two:
//!
//! * **ECEF** — Earth-centred, Earth-fixed. The globe stands still, so the
//!   camera keeps looking at the same place on the ground. The sun sweeps
//!   around the planet once a day and the stars turn with it.
//! * **ECI** — Earth-centred inertial. The stars stand still and the sun with
//!   them, give or take the degree a day the Earth's orbit moves it; the globe
//!   turns underneath at the sidereal rate. This is the frame orbits are flown
//!   in, and the one where a satellite track is a closed ellipse.
//!
//! Everything Earth-fixed — the globe mesh, the imagery tiles, the subsolar
//! point — is stored in ECEF and rotated into world space by
//! [`ReferenceFrame::earth_to_world`]. Anything reading a world-space direction
//! back out as a latitude and longitude has to undo that with
//! [`ReferenceFrame::world_to_earth`].
//!
//! Switching frames turns the globe through most of a full rotation at once,
//! so the switch announces itself with a [`FrameRealigned`] message: whatever
//! is looking at the ground adds that angle to stay where it was, instead of
//! being left staring at the other side of the planet.
//!
//! That single step is also why [`FrameSet`] exists. A switch only looks
//! seamless if the frame, the camera and everything drawn from the two land on
//! the same tick; read the frame half a tick early and the globe is drawn where
//! it used to be, or the tile walk asks which tiles are in view of a camera
//! that has not caught up yet and blinks the imagery out for a frame.

use bevy::prelude::*;

use crate::sun::Sun;

/// Seconds from the Unix epoch to J2000.0, the epoch the sidereal angle is
/// measured from.
const J2000_UNIX_SECONDS: f64 = 946_728_000.0;
/// Greenwich's sidereal angle at J2000.0.
const SIDEREAL_EPOCH_DEGREES: f64 = 280.460_618_37;
/// Degrees of sidereal rotation per day: a full turn plus the extra the Earth
/// has to make up to face the sun again, which is what makes a sidereal day
/// about four minutes short of a solar one.
const SIDEREAL_DEGREES_PER_DAY: f64 = 360.985_647_366_29;

const SECONDS_PER_DAY: f64 = 86_400.0;

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
    pub fn label(self) -> &'static str {
        match self {
            Self::Ecef => "ECEF · Earth-fixed",
            Self::Eci => "ECI · inertial",
        }
    }

    fn toggled(self) -> Self {
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
    fn earth_yaw(&self) -> f32 {
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
                    .after(crate::sun::advance_sun),
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

fn sync_earth_rotation(sun: Res<Sun>, mut frame: ResMut<ReferenceFrame>) {
    frame.earth_rotation = sidereal_angle(sun.unix_seconds);
}

/// Greenwich mean sidereal time as an angle in radians.
fn sidereal_angle(unix_seconds: f64) -> f32 {
    let days_since_j2000 = (unix_seconds - J2000_UNIX_SECONDS) / SECONDS_PER_DAY;
    let degrees =
        (SIDEREAL_EPOCH_DEGREES + SIDEREAL_DEGREES_PER_DAY * days_since_j2000).rem_euclid(360.0);
    (degrees as f32).to_radians()
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
