//! The simulated clock: what moment the scene is drawn at, and how fast that
//! moment moves.
//!
//! Deliberately separate from anything computed *from* the clock — where the
//! sun or moon actually are ([`crate::sun`], [`crate::moon`]), where a
//! satellite's propagated to, when a mission departs. Everything downstream
//! reads [`SimClock::unix_seconds`]; this module is the one place that says
//! what it is and moves it forward (or, since [`SimClock::set_time_scale`]
//! accepts a negative rate, backward).

use bevy::prelude::*;

use crate::api::keyboard_enabled;

const SECONDS_PER_DAY: f64 = 86_400.0;
/// Simulated seconds that pass per real second by default: one day every four minutes.
const DEFAULT_TIME_SCALE: f32 = 360.0;
/// The slowest the clock runs, forward or back: real time.
pub const MIN_TIME_SCALE: f32 = 1.0;
/// The fastest, either direction: a whole day every second.
pub const MAX_TIME_SCALE: f32 = 86_400.0;

/// The simulated clock everything else in the scene reads time from.
#[derive(Resource, Debug, Clone, Copy)]
pub struct SimClock {
    /// Seconds since the Unix epoch, as simulated.
    pub unix_seconds: f64,
    /// Simulated seconds per real second. Negative runs the clock backward —
    /// [`advance_clock`] just adds `delta_secs * time_scale`, so a negative
    /// rate needs no special casing there.
    pub time_scale: f32,
    pub paused: bool,
}

impl Default for SimClock {
    fn default() -> Self {
        Self {
            unix_seconds: wall_clock_unix_seconds(),
            time_scale: DEFAULT_TIME_SCALE,
            paused: false,
        }
    }
}

impl SimClock {
    /// Hour of the UTC day, in `0.0..24.0`.
    pub fn utc_hours(&self) -> f32 {
        utc_hours(self.unix_seconds)
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
    }

    /// Sets how fast the clock runs, within the range the controls allow.
    ///
    /// Only the magnitude is clamped, so a negative rate a caller asks for
    /// survives — that is what runs the clock backward.
    pub fn set_time_scale(&mut self, time_scale: f32) {
        let sign = if time_scale < 0.0 { -1.0 } else { 1.0 };
        self.time_scale = sign * time_scale.abs().clamp(MIN_TIME_SCALE, MAX_TIME_SCALE);
    }
}

/// Hour of the UTC day a moment falls at, in `0.0..24.0` — shared by
/// [`SimClock::utc_hours`] and [`crate::sun`]'s subsolar longitude, which is
/// stated against the same hour angle.
pub(crate) fn utc_hours(unix_seconds: f64) -> f32 {
    (unix_seconds.rem_euclid(SECONDS_PER_DAY) / 3600.0) as f32
}

pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SimClock>().add_systems(
            Update,
            (clock_controls.run_if(keyboard_enabled), advance_clock).chain(),
        );
    }
}

fn clock_controls(keys: Res<ButtonInput<KeyCode>>, mut clock: ResMut<SimClock>) {
    if keys.just_pressed(KeyCode::KeyP) {
        clock.paused = !clock.paused;
    }
    if keys.just_pressed(KeyCode::Comma) {
        let halved = clock.time_scale / 2.0;
        clock.set_time_scale(halved);
    }
    if keys.just_pressed(KeyCode::Period) {
        let doubled = clock.time_scale * 2.0;
        clock.set_time_scale(doubled);
    }
    if keys.just_pressed(KeyCode::KeyN) {
        clock.snap_to_now();
    }
}

pub(crate) fn advance_clock(time: Res<Time>, mut clock: ResMut<SimClock>) {
    if clock.paused {
        return;
    }
    clock.unix_seconds += (time.delta_secs() * clock.time_scale) as f64;
}

/// Seconds since the Unix epoch, falling back to zero if the platform has no clock.
///
/// Shared with [`crate::sun`] and [`crate::moon`], which both start on this
/// same wall clock as this — a body that began at the epoch and caught up on
/// the first tick would be drawn half a world from where it belongs for one
/// frame.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_scale_clamps_magnitude_but_keeps_sign() {
        let mut clock = SimClock::default();

        clock.set_time_scale(-360.0);
        assert_eq!(clock.time_scale, -360.0);

        clock.set_time_scale(-0.1);
        assert_eq!(clock.time_scale, -MIN_TIME_SCALE);

        clock.set_time_scale(-1_000_000.0);
        assert_eq!(clock.time_scale, -MAX_TIME_SCALE);

        clock.set_time_scale(1_000_000.0);
        assert_eq!(clock.time_scale, MAX_TIME_SCALE);
    }
}
