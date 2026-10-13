//! Epochs, backed by [`hifitime`].
//!
//! - Unix seconds are UTC (leap seconds applied by hifitime).
//! - [`Epoch::julian_centuries`] / [`Epoch::days`] are TDB since J2000.0 (ephemeris argument).
//! - [`Epoch::tt_jd`] / [`Epoch::ut1_jd`] are SOFA two-part Julian dates for Earth orientation.
//!
//! TAI → UTC is done here, not by hifitime 4.3: hifitime keys its leap-second lookup on TAI
//! seconds, which is 1 s off for the ΔAT seconds preceding each leap second.

use hifitime::{Duration, TimeScale, Unit};

const SECONDS_PER_DAY: f64 = 86_400.0;
const DAYS_PER_JULIAN_CENTURY: f64 = 36_525.0;
/// JD of J2000.0.
pub const J2000_JD: f64 = 2_451_545.0;
/// TT − TAI, s.
const TT_MINUS_TAI_S: f64 = 32.184;
/// 1900-01-01 → 1970-01-01, s.
const UNIX_OFFSET_S: f64 = 2_208_988_800.0;

pub use hifitime;

/// An instant. Stored in TAI: hifitime arithmetic runs in the left operand's scale, and UTC drops leap seconds.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Epoch(hifitime::Epoch);

impl Epoch {
    /// 2000-01-01T12:00:00 TT. hifitime TAI origin is 1900-01-01T00:00 TAI (36 524 d earlier).
    pub const J2000: Epoch = Epoch(hifitime::Epoch::from_tai_duration(Duration::from_parts(
        0,
        3_155_716_767_816_000_000,
    )));

    /// From Unix seconds (UTC).
    pub fn from_unix_seconds(unix_seconds: f64) -> Self {
        Self::from_hifitime(hifitime::Epoch::from_unix_seconds(unix_seconds))
    }

    /// Unix seconds (UTC).
    pub fn to_unix_seconds(self) -> f64 {
        self.utc_label().to_tai_seconds() - UNIX_OFFSET_S
    }

    /// TAI-scale epoch whose count is the UTC label (seconds since 1900 without leap seconds).
    /// hifitime's table is keyed on that count. In a leap second, the label repeats the
    /// following 00:00:00 (POSIX); [`Epoch::to_gregorian`] reports `:60` instead.
    fn utc_label(self) -> hifitime::Epoch {
        let first = self.0.leap_seconds(true).unwrap_or(0.0);
        let guess = self.0 - first * Unit::Second;
        let dat = guess.leap_seconds(true).unwrap_or(0.0);
        self.0 - dat * Unit::Second
    }

    /// Calendar fields `(y, mo, d, h, mi, s, ns)` in `scale`. UTC leap seconds read `s = 60`.
    pub fn to_gregorian(self, scale: TimeScale) -> (i32, u8, u8, u8, u8, u8, u32) {
        if scale != TimeScale::UTC {
            return self.0.to_gregorian(scale);
        }
        let label = self.utc_label();
        let dat = (self.0 - label).to_seconds();
        let next = label.leap_seconds(true).unwrap_or(0.0);
        if next != dat {
            let (y, mo, d, h, mi, s, ns) =
                (self.0 - next * Unit::Second).to_gregorian(TimeScale::TAI);
            return (y, mo, d, h, mi, s + 1, ns);
        }
        label.to_gregorian(TimeScale::TAI)
    }

    pub fn from_hifitime(epoch: hifitime::Epoch) -> Self {
        Self(epoch.to_time_scale(TimeScale::TAI))
    }

    pub fn as_hifitime(self) -> hifitime::Epoch {
        self.0
    }

    /// From TT seconds since J2000.0.
    pub fn from_tt_seconds_j2000(seconds: f64) -> Self {
        Self(Self::J2000.0 + seconds * Unit::Second)
    }

    /// From TDB seconds since J2000.0 (SPICE ET).
    pub fn from_tdb_seconds_j2000(seconds: f64) -> Self {
        Self::from_hifitime(hifitime::Epoch::from_tdb_seconds(seconds))
    }

    /// From UTC calendar fields.
    pub fn from_utc(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: f64) -> Self {
        let whole = second.floor();
        let nanos = ((second - whole) * 1.0e9).round() as u32;
        Self::from_hifitime(hifitime::Epoch::from_gregorian(
            year,
            month,
            day,
            hour,
            minute,
            whole as u8,
            nanos,
            TimeScale::UTC,
        ))
    }

    /// TDB Julian centuries since J2000.0.
    pub fn julian_centuries(self) -> f64 {
        self.0.to_tdb_centuries_since_j2000()
    }

    /// TDB days since J2000.0.
    pub fn days(self) -> f64 {
        self.0.to_tdb_days_since_j2000()
    }

    /// TDB seconds since J2000.0 (SPICE ET).
    pub fn tdb_seconds(self) -> f64 {
        self.0.to_tdb_seconds()
    }

    /// TT seconds since J2000.0.
    pub fn tt_seconds(self) -> f64 {
        (self.0 - Self::J2000.0).to_seconds()
    }

    /// TT Julian centuries since J2000.0.
    pub fn tt_centuries(self) -> f64 {
        self.tt_seconds() / SECONDS_PER_DAY / DAYS_PER_JULIAN_CENTURY
    }

    /// TAI − UTC, s.
    pub fn tai_minus_utc(self) -> f64 {
        (self.0 - self.utc_label()).to_seconds()
    }

    /// TT − UTC, s.
    pub fn tt_minus_utc(self) -> f64 {
        self.tai_minus_utc() + TT_MINUS_TAI_S
    }

    /// TT as SOFA two-part JD `(J2000_JD, days)`.
    pub fn tt_jd(self) -> (f64, f64) {
        (J2000_JD, self.tt_seconds() / SECONDS_PER_DAY)
    }

    /// UTC as SOFA two-part JD `(J2000_JD, days)`. Leap-second days are not stretched.
    pub fn utc_jd(self) -> (f64, f64) {
        let tt_days = self.tt_seconds() / SECONDS_PER_DAY;
        (J2000_JD, tt_days - self.tt_minus_utc() / SECONDS_PER_DAY)
    }

    /// UT1 as SOFA two-part JD, given UT1 − UTC in seconds.
    pub fn ut1_jd(self, ut1_minus_utc: f64) -> (f64, f64) {
        let (jd1, jd2) = self.utc_jd();
        (jd1, jd2 + ut1_minus_utc / SECONDS_PER_DAY)
    }

    /// MJD (UTC), for EOP lookup.
    pub fn mjd_utc(self) -> f64 {
        self.utc_label().to_mjd_tai_days()
    }

    pub fn advanced_by_seconds(self, seconds: f64) -> Self {
        Self(self.0 + seconds * Unit::Second)
    }

    /// Elapsed SI seconds `self − earlier`.
    pub fn seconds_since(self, earlier: Epoch) -> f64 {
        (self.0 - earlier.0).to_seconds()
    }
}

impl std::fmt::Display for Epoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (y, mo, d, h, mi, s, ns) = self.to_gregorian(TimeScale::UTC);
        write!(f, "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")?;
        if ns != 0 {
            let frac = format!("{ns:09}");
            write!(f, ".{}", frac.trim_end_matches('0'))?;
        }
        write!(f, " UTC")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn j2000_is_tt_noon() {
        // 2000-01-01T11:58:55.816 UTC (TAI − UTC = 32 s).
        let utc = Epoch::from_utc(2000, 1, 1, 11, 58, 55.816);
        assert!(utc.seconds_since(Epoch::J2000).abs() < 1.0e-6);
        assert!(Epoch::J2000.tt_seconds().abs() < 1.0e-9);
        // TDB − TT at J2000 ≈ −7.3e-5 s.
        assert!(Epoch::J2000.tdb_seconds().abs() < 1.0e-3);
    }

    #[test]
    fn unix_round_trip() {
        let t = 1_700_000_000.25;
        assert!((Epoch::from_unix_seconds(t).to_unix_seconds() - t).abs() < 1.0e-6);
    }

    #[test]
    fn leap_seconds() {
        assert_eq!(Epoch::from_utc(2004, 4, 6, 0, 0, 0.0).tai_minus_utc(), 32.0);
        assert_eq!(Epoch::from_utc(2017, 1, 2, 0, 0, 0.0).tai_minus_utc(), 37.0);
    }

    #[test]
    fn arithmetic_spans_leap_seconds() {
        // 5 leap seconds between 1992-08-20 and 2000-01-01.
        let (jd1, jd2) = Epoch::from_unix_seconds(714_312_840.0).utc_jd();
        assert!((jd1 + jd2 - 2_448_855.009_722_222).abs() < 1.0e-9);
        let a = Epoch::from_utc(2016, 12, 31, 23, 59, 59.0);
        let b = Epoch::from_utc(2017, 1, 1, 0, 0, 0.0);
        assert!((b.seconds_since(a) - 2.0).abs() < 1.0e-9);
    }

    #[test]
    fn utc_output_before_leap_second() {
        // Within ΔAT seconds of the 2017 leap second.
        let e = Epoch::from_utc(2016, 12, 31, 23, 59, 58.0);
        assert_eq!(
            e.to_gregorian(TimeScale::UTC),
            (2016, 12, 31, 23, 59, 58, 0)
        );
        assert_eq!(e.tai_minus_utc(), 36.0);
        assert_eq!(e.to_unix_seconds(), 1_483_228_798.0);
        assert_eq!(Epoch::from_unix_seconds(1_483_228_798.0), e);
        assert_eq!(e.to_string(), "2016-12-31T23:59:58 UTC");
        // Leap second itself, then the following second.
        let leap = e.advanced_by_seconds(2.5);
        assert_eq!(leap.tai_minus_utc(), 36.0);
        assert_eq!(
            leap.to_gregorian(TimeScale::UTC),
            (2016, 12, 31, 23, 59, 60, 500_000_000)
        );
        let after = e.advanced_by_seconds(3.0);
        assert_eq!(after.to_gregorian(TimeScale::UTC), (2017, 1, 1, 0, 0, 0, 0));
        assert_eq!(after.tai_minus_utc(), 37.0);
        assert!((after.mjd_utc() - 57_754.0).abs() < 1e-12);
    }

    #[test]
    fn utc_jd_matches_calendar() {
        let (jd1, jd2) = Epoch::from_utc(2004, 4, 6, 7, 51, 28.386009).utc_jd();
        assert!((jd1 + jd2 - 2_453_101.827_411_875).abs() < 1.0e-9);
    }

    #[test]
    fn a_julian_century_is_36525_days() {
        let later = Epoch::J2000.advanced_by_seconds(DAYS_PER_JULIAN_CENTURY * SECONDS_PER_DAY);
        assert!((later.tt_centuries() - 1.0).abs() < 1.0e-12);
        assert!((later.julian_centuries() - 1.0).abs() < 1.0e-9);
    }
}
