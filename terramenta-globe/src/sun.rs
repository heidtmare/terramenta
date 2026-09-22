//! Where the sun is, so the terminator falls in the right place.
//!
//! Low-precision solar position model: declination from day of year, hour
//! angle from UTC, no equation of time. Accurate to within roughly a degree,
//! which is sufficient for the terminator and avoids pulling in a full
//! ephemeris.
//!
//! Everything here is Earth-fixed: the subsolar point is a latitude and
//! longitude, and the direction derived from it points at the sun in ECEF.
//! [`crate::frame::ReferenceFrame`] rotates that into whichever frame the
//! scene is drawn in.
//!
//! This only answers *where* the sun is for a given moment — *when* that
//! moment is, and how fast it moves, belongs to [`crate::time::SimClock`].

use bevy::prelude::*;

use crate::api::keyboard_enabled;
use crate::geo::LatLon;
use crate::time::SimClock;

const SECONDS_PER_DAY: f64 = 86_400.0;
const DAYS_PER_YEAR: f32 = 365.2422;
/// Earth's axial tilt.
const OBLIQUITY_DEG: f32 = 23.44;

/// Where the sun is, as seen from Earth's centre, and whether it lights the
/// globe at all.
#[derive(Resource, Debug, Clone)]
pub struct Sun {
    /// Unit vector from the globe's center toward the sun, in Earth-fixed
    /// coordinates. Multiply by [`crate::frame::ReferenceFrame::earth_to_world`]
    /// before handing it to a shader.
    pub direction_ecef: Vec3,
    /// The point on Earth directly beneath the sun.
    pub subsolar: LatLon,
    /// Whether the sun lights the globe at all. Turned off, there is no
    /// terminator and no night side: every face is lit as though the sun were
    /// straight overhead, which is how you read imagery of a place that
    /// happens to be in darkness.
    pub shaded: bool,
}

impl Default for Sun {
    fn default() -> Self {
        let mut sun = Self {
            direction_ecef: Vec3::X,
            subsolar: LatLon::new(0.0, 0.0),
            shaded: true,
        };
        sun.set_clock(crate::time::wall_clock_unix_seconds());
        sun
    }
}

impl Sun {
    /// What the shaders scale their sunlight terms by: the real terminator at
    /// `1.0`, flat full daylight at `0.0`.
    pub fn shading(&self) -> f32 {
        if self.shaded { 1.0 } else { 0.0 }
    }

    /// Points the sun where a given moment says it should be.
    pub fn set_clock(&mut self, unix_seconds: f64) {
        let days_since_epoch = unix_seconds / SECONDS_PER_DAY;
        // 1970-01-01 was day 1 of the year; the +10 offset places the solstice
        // near the end of December, where it belongs.
        let day_of_year = (days_since_epoch as f32).rem_euclid(DAYS_PER_YEAR) + 1.0;
        let orbital_angle = std::f32::consts::TAU * (day_of_year + 10.0) / DAYS_PER_YEAR;
        let declination = -OBLIQUITY_DEG * orbital_angle.cos();

        // The subsolar meridian is noon: opposite the 00:00 UTC meridian, moving
        // west at 15° per hour.
        let longitude = 180.0 - crate::time::utc_hours(unix_seconds) * 15.0;
        let longitude = (longitude + 180.0).rem_euclid(360.0) - 180.0;

        self.subsolar = LatLon::new(declination, longitude);
        self.direction_ecef = self.subsolar.to_direction();
    }
}

pub struct SunPlugin;

impl Plugin for SunPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Sun>().add_systems(
            Update,
            (sun_controls.run_if(keyboard_enabled), track_sun_clock)
                .chain()
                .after(crate::time::advance_clock),
        );
    }
}

fn sun_controls(keys: Res<ButtonInput<KeyCode>>, mut sun: ResMut<Sun>) {
    if keys.just_pressed(KeyCode::KeyI) {
        sun.shaded = !sun.shaded;
    }
}

/// Follows the simulated clock. There is only one clock, and a sun on one of
/// its own would drift out of phase with everything else drawn from
/// [`SimClock`] — the same reason [`crate::moon::track_moon`] follows it too.
fn track_sun_clock(clock: Res<SimClock>, mut sun: ResMut<Sun>) {
    sun.set_clock(clock.unix_seconds);
}
