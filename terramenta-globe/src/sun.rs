//! Where the sun is, so the terminator falls in the right place.
//!
//! This is a low-precision solar position model — declination from the day of
//! year, hour angle from UTC, no equation of time — which puts the terminator
//! within roughly a degree of the real one. That is plenty for a globe you look
//! at, and it avoids pulling in an ephemeris.
//!
//! Everything here is Earth-fixed: the subsolar point is a latitude and
//! longitude, and the direction that falls out of it points at the sun in ECEF.
//! Rotating that into whichever frame the scene is drawn in is the job of
//! [`crate::frame::ReferenceFrame`].

use bevy::prelude::*;

use crate::api::keyboard_enabled;
use crate::geo::LatLon;

const SECONDS_PER_DAY: f64 = 86_400.0;
const DAYS_PER_YEAR: f32 = 365.2422;
/// Earth's axial tilt.
const OBLIQUITY_DEG: f32 = 23.44;
/// Simulated seconds that pass per real second by default: one day every four minutes.
const DEFAULT_TIME_SCALE: f32 = 360.0;
/// The slowest the clock runs: real time.
pub const MIN_TIME_SCALE: f32 = 1.0;
/// The fastest: a whole day every second.
pub const MAX_TIME_SCALE: f32 = 86_400.0;

/// The simulated clock and the resulting sun direction.
#[derive(Resource, Debug, Clone)]
pub struct Sun {
    /// Unit vector from the globe's center toward the sun, in Earth-fixed
    /// coordinates. Multiply by [`crate::frame::ReferenceFrame::earth_to_world`]
    /// before handing it to a shader.
    pub direction_ecef: Vec3,
    /// The point on Earth directly beneath the sun.
    pub subsolar: LatLon,
    /// Seconds since the Unix epoch, as simulated.
    pub unix_seconds: f64,
    /// Simulated seconds per real second.
    pub time_scale: f32,
    pub paused: bool,
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
            unix_seconds: wall_clock_unix_seconds(),
            time_scale: DEFAULT_TIME_SCALE,
            paused: false,
            shaded: true,
        };
        sun.recompute();
        sun
    }
}

impl Sun {
    /// What the shaders scale their sunlight terms by: the real terminator at
    /// `1.0`, flat full daylight at `0.0`.
    pub fn shading(&self) -> f32 {
        if self.shaded { 1.0 } else { 0.0 }
    }

    /// Hour of the UTC day, in `0.0..24.0`.
    pub fn utc_hours(&self) -> f32 {
        (self.unix_seconds.rem_euclid(SECONDS_PER_DAY) / 3600.0) as f32
    }

    /// Formats the simulated clock as `14:32 UTC`.
    pub fn format_utc(&self) -> String {
        let hours = self.utc_hours();
        let minutes = (hours.fract() * 60.0) as u32;
        format!("{:02}:{:02} UTC", hours as u32, minutes)
    }

    /// Jumps the simulated clock back to the real one.
    pub fn snap_to_now(&mut self) {
        self.set_clock(wall_clock_unix_seconds());
    }

    /// Jumps the simulated clock to a given moment.
    pub fn set_clock(&mut self, unix_seconds: f64) {
        self.unix_seconds = unix_seconds;
        self.recompute();
    }

    /// Sets how fast the clock runs, within the range the controls allow.
    pub fn set_time_scale(&mut self, time_scale: f32) {
        self.time_scale = time_scale.clamp(MIN_TIME_SCALE, MAX_TIME_SCALE);
    }

    fn recompute(&mut self) {
        let days_since_epoch = self.unix_seconds / SECONDS_PER_DAY;
        // 1970-01-01 was day 1 of the year; the +10 offset places the solstice
        // near the end of December, where it belongs.
        let day_of_year = (days_since_epoch as f32).rem_euclid(DAYS_PER_YEAR) + 1.0;
        let orbital_angle = std::f32::consts::TAU * (day_of_year + 10.0) / DAYS_PER_YEAR;
        let declination = -OBLIQUITY_DEG * orbital_angle.cos();

        // The subsolar meridian is noon: opposite the 00:00 UTC meridian, moving
        // west at 15° per hour.
        let longitude = 180.0 - self.utc_hours() * 15.0;
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
            (sun_controls.run_if(keyboard_enabled), advance_sun).chain(),
        );
    }
}

fn sun_controls(keys: Res<ButtonInput<KeyCode>>, mut sun: ResMut<Sun>) {
    if keys.just_pressed(KeyCode::KeyP) {
        sun.paused = !sun.paused;
    }
    if keys.just_pressed(KeyCode::Comma) {
        let halved = sun.time_scale / 2.0;
        sun.set_time_scale(halved);
    }
    if keys.just_pressed(KeyCode::Period) {
        let doubled = sun.time_scale * 2.0;
        sun.set_time_scale(doubled);
    }
    if keys.just_pressed(KeyCode::KeyN) {
        sun.snap_to_now();
    }
    if keys.just_pressed(KeyCode::KeyI) {
        sun.shaded = !sun.shaded;
    }
}

pub(crate) fn advance_sun(time: Res<Time>, mut sun: ResMut<Sun>) {
    if sun.paused {
        return;
    }
    sun.unix_seconds += (time.delta_secs() * sun.time_scale) as f64;
    sun.recompute();
}

/// Seconds since the Unix epoch, falling back to zero if the platform has no clock.
///
/// Shared with [`crate::moon`], which starts on the same wall clock this does —
/// a moon that began at the epoch and caught up on the first tick would be
/// drawn half a world from where it belongs for one frame.
pub(crate) fn wall_clock_unix_seconds() -> f64 {
    #[cfg(not(target_arch = "wasm32"))]
    use std::time::{SystemTime, UNIX_EPOCH};
    #[cfg(target_arch = "wasm32")]
    use web_time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or_default()
}
