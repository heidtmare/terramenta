//! When a state vector is evaluated at.
//!
//! Every ephemeris here is a function of time since J2000.0, the only clock
//! this crate knows about — no leap seconds, no time scales beyond what a
//! low-precision orbit model can tell apart. An embedder hands in Unix
//! seconds, the same clock `terramenta-globe`'s `Sun` resource runs on, so
//! both crates can be driven from one wall clock without a conversion at
//! the boundary.

/// Seconds from the Unix epoch to J2000.0 — noon, 1 January 2000, Terrestrial
/// Time. Matches the constant `terramenta-globe` measures sidereal time from,
/// so a moment handed to both crates means the same instant in both.
const J2000_UNIX_SECONDS: f64 = 946_728_000.0;
const SECONDS_PER_DAY: f64 = 86_400.0;
const DAYS_PER_JULIAN_CENTURY: f64 = 36_525.0;

/// A moment in time, as Julian centuries since J2000.0.
///
/// Orbital elements are conventionally stated at an epoch and a rate per
/// century, so this is the unit every ephemeris in this crate wants.
/// Wrapping it in a type keeps a raw `f64` of days from being handed in
/// where centuries are expected — an easy mixup with a rate table this
/// shape.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Epoch(f64);

impl Epoch {
    pub const J2000: Epoch = Epoch(0.0);

    /// Builds an epoch from Unix seconds — the wall clock an embedder runs on.
    pub fn from_unix_seconds(unix_seconds: f64) -> Self {
        let days_since_j2000 = (unix_seconds - J2000_UNIX_SECONDS) / SECONDS_PER_DAY;
        Self(days_since_j2000 / DAYS_PER_JULIAN_CENTURY)
    }

    pub fn to_unix_seconds(self) -> f64 {
        self.0 * DAYS_PER_JULIAN_CENTURY * SECONDS_PER_DAY + J2000_UNIX_SECONDS
    }

    /// Julian centuries since J2000.0 — the unit the planetary element tables
    /// in [`crate::planets`] are stated in.
    pub fn julian_centuries(self) -> f64 {
        self.0
    }

    /// Days since J2000.0.
    pub fn days(self) -> f64 {
        self.0 * DAYS_PER_JULIAN_CENTURY
    }

    pub fn advanced_by_seconds(self, seconds: f64) -> Self {
        Self(self.0 + seconds / SECONDS_PER_DAY / DAYS_PER_JULIAN_CENTURY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn j2000_round_trips_through_unix_seconds() {
        let epoch = Epoch::from_unix_seconds(J2000_UNIX_SECONDS);
        assert_eq!(epoch, Epoch::J2000);
        assert_eq!(epoch.to_unix_seconds(), J2000_UNIX_SECONDS);
    }

    #[test]
    fn a_julian_century_is_36525_days_later() {
        let one_century_later = Epoch::from_unix_seconds(
            J2000_UNIX_SECONDS + DAYS_PER_JULIAN_CENTURY * SECONDS_PER_DAY,
        );
        assert!((one_century_later.julian_centuries() - 1.0).abs() < 1.0e-9);
    }
}
