//! Solar and geomagnetic indices for atmosphere models.
//!
//! Source: CelesTrak `SW-All.csv` — observed (OBS/INT), 45-day daily prediction (PRD),
//! monthly prediction (PRM). Monthly rows are held across their month; their missing Ap is
//! replaced by the mean daily Ap of the last 81 days that carry one. Outside coverage, the
//! nearest day is held.
//!
//! F10.7: observed (at 1 AU-uncorrected Earth distance), sfu. Ap: 3-hourly, 2 nT units.

use crate::time::Epoch;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpaceWeatherDay {
    /// MJD (UTC) of 00:00.
    pub mjd: i64,
    /// 3-hourly Ap, 00–03 h … 21–24 h UT.
    pub ap: [f64; 8],
    pub ap_daily: f64,
    pub f107: f64,
    /// 81-day centred mean of observed F10.7.
    pub f107_center81: f64,
}

/// NRLMSISE-00 drivers at an instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MsisDrivers {
    /// Previous day's F10.7.
    pub f107: f64,
    /// 81-day centred mean F10.7.
    pub f107_average: f64,
    /// Daily Ap; 3-h Ap at t, t−3 h, t−6 h, t−9 h; means over t−12…−33 h and t−36…−57 h.
    pub ap: [f64; 7],
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpaceWeatherError {
    Parse { line: usize, reason: String },
    Empty,
}

impl std::fmt::Display for SpaceWeatherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse { line, reason } => write!(f, "space weather line {line}: {reason}"),
            Self::Empty => f.write_str("space weather: no records"),
        }
    }
}

impl std::error::Error for SpaceWeatherError {}

/// Contiguous daily table.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceWeather {
    days: Vec<SpaceWeatherDay>,
}

impl SpaceWeather {
    pub fn from_celestrak_csv(text: &str) -> Result<Self, SpaceWeatherError> {
        let mut lines = text.lines().enumerate();
        let header = lines.next().ok_or(SpaceWeatherError::Empty)?.1;
        let cols: Vec<&str> = header.split(',').map(str::trim).collect();
        let col = |name: &str| {
            cols.iter()
                .position(|c| *c == name)
                .ok_or_else(|| SpaceWeatherError::Parse {
                    line: 1,
                    reason: format!("missing column {name}"),
                })
        };
        let date = col("DATE")?;
        let ap1 = col("AP1")?;
        let ap_avg = col("AP_AVG")?;
        let f107 = col("F10.7_OBS")?;
        let f107_c81 = col("F10.7_OBS_CENTER81")?;
        let kind = col("F10.7_DATA_TYPE")?;

        // (row, monthly)
        let mut rows: Vec<(SpaceWeatherDay, bool)> = Vec::new();
        for (i, line) in lines {
            if line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            let err = |reason: String| SpaceWeatherError::Parse {
                line: i + 1,
                reason,
            };
            let num = |k: usize| -> Result<Option<f64>, SpaceWeatherError> {
                match f.get(k).copied().unwrap_or("") {
                    "" => Ok(None),
                    s => s
                        .parse()
                        .map(Some)
                        .map_err(|_| err(format!("bad number {s:?}"))),
                }
            };
            let mjd =
                parse_date_mjd(f[date]).ok_or_else(|| err(format!("bad date {:?}", f[date])))?;
            let monthly = f.get(kind).is_some_and(|k| *k == "PRM");
            let mut ap = [f64::NAN; 8];
            for (j, a) in ap.iter_mut().enumerate() {
                *a = num(ap1 + j)?.unwrap_or(f64::NAN);
            }
            let day = SpaceWeatherDay {
                mjd,
                ap,
                ap_daily: num(ap_avg)?.unwrap_or(f64::NAN),
                f107: num(f107)?.ok_or_else(|| err("missing F10.7_OBS".into()))?,
                f107_center81: num(f107_c81)?
                    .ok_or_else(|| err("missing F10.7_OBS_CENTER81".into()))?,
            };
            if let Some((last, _)) = rows.last()
                && day.mjd <= last.mjd
            {
                return Err(err("dates not increasing".into()));
            }
            rows.push((day, monthly));
        }
        if rows.is_empty() {
            return Err(SpaceWeatherError::Empty);
        }

        // Missing Ap: trailing mean over the last 81 rows that carry one.
        let mut recent: std::collections::VecDeque<f64> = Default::default();
        let mut fallback = 15.0;
        for (d, _) in &mut rows {
            if d.ap_daily.is_finite() {
                recent.push_back(d.ap_daily);
                if recent.len() > 81 {
                    recent.pop_front();
                }
                fallback = recent.iter().sum::<f64>() / recent.len() as f64;
            } else {
                d.ap_daily = fallback;
            }
            for a in &mut d.ap {
                if !a.is_finite() {
                    *a = d.ap_daily;
                }
            }
        }

        // Expand to daily; monthly rows hold until the next row.
        let mut days: Vec<SpaceWeatherDay> = Vec::with_capacity(rows.len() + 31 * 12);
        for (k, (row, monthly)) in rows.iter().enumerate() {
            let until = match (monthly, rows.get(k + 1)) {
                (true, Some((next, _))) => next.mjd,
                (true, None) => row.mjd + 30,
                _ => row.mjd + 1,
            };
            if let Some(prev) = days.last()
                && row.mjd > prev.mjd + 1
                && !monthly
            {
                return Err(SpaceWeatherError::Parse {
                    line: 0,
                    reason: format!("gap before MJD {}", row.mjd),
                });
            }
            // Hold the previous row across a gap before a monthly row.
            while let Some(prev) = days.last().copied()
                && prev.mjd + 1 < row.mjd
            {
                days.push(SpaceWeatherDay {
                    mjd: prev.mjd + 1,
                    ..prev
                });
            }
            for mjd in row.mjd..until {
                days.push(SpaceWeatherDay { mjd, ..*row });
            }
        }

        Ok(Self { days })
    }

    /// Constant activity.
    pub fn constant(f107: f64, f107_average: f64, ap: f64) -> Self {
        Self {
            days: vec![SpaceWeatherDay {
                mjd: 0,
                ap: [ap; 8],
                ap_daily: ap,
                f107,
                f107_center81: f107_average,
            }],
        }
    }

    pub fn days(&self) -> &[SpaceWeatherDay] {
        &self.days
    }

    /// First and last MJD.
    pub fn coverage(&self) -> (i64, i64) {
        (self.days[0].mjd, self.days[self.days.len() - 1].mjd)
    }

    /// Clamped to coverage.
    pub fn day(&self, mjd: i64) -> &SpaceWeatherDay {
        let i = (mjd - self.days[0].mjd).clamp(0, self.days.len() as i64 - 1);
        &self.days[i as usize]
    }

    /// 3-hourly Ap `back` intervals before the one containing `(mjd, slot)`.
    fn ap_back(&self, mjd: i64, slot: i64, back: i64) -> f64 {
        let k = slot - back;
        self.day(mjd + k.div_euclid(8)).ap[k.rem_euclid(8) as usize]
    }

    pub fn msis_drivers(&self, epoch: Epoch) -> MsisDrivers {
        let t = epoch.mjd_utc();
        let mjd = t.floor() as i64;
        let slot = (((t - t.floor()) * 8.0) as i64).min(7);
        let today = self.day(mjd);
        let mean8 = |first: i64| {
            (first..first + 8)
                .map(|b| self.ap_back(mjd, slot, b))
                .sum::<f64>()
                / 8.0
        };
        MsisDrivers {
            f107: self.day(mjd - 1).f107,
            f107_average: today.f107_center81,
            ap: [
                today.ap_daily,
                self.ap_back(mjd, slot, 0),
                self.ap_back(mjd, slot, 1),
                self.ap_back(mjd, slot, 2),
                self.ap_back(mjd, slot, 3),
                mean8(4),
                mean8(12),
            ],
        }
    }
}

/// `YYYY-MM-DD` → MJD.
fn parse_date_mjd(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(civil_to_mjd(y, m, d))
}

/// Gregorian date → MJD (Fliegel & Van Flandern JDN − 2400001).
pub(crate) fn civil_to_mjd(y: i64, m: i64, d: i64) -> i64 {
    let a = (14 - m) / 12;
    let (yy, mm) = (y + 4800 - a, m + 12 * a - 3);
    d + (153 * mm + 2) / 5 + 365 * yy + yy / 4 - yy / 100 + yy / 400 - 32045 - 2_400_001
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
DATE,BSRN,ND,KP1,KP2,KP3,KP4,KP5,KP6,KP7,KP8,KP_SUM,AP1,AP2,AP3,AP4,AP5,AP6,AP7,AP8,AP_AVG,CP,C9,ISN,F10.7_OBS,F10.7_ADJ,F10.7_DATA_TYPE,F10.7_OBS_CENTER81,F10.7_OBS_LAST81,F10.7_ADJ_CENTER81,F10.7_ADJ_LAST81
2026-10-11,2634,13,40,37,27,30,23,23,17,20,217,27,22,12,15,9,9,6,7,13,0.8,4,84,121.9,121.5,OBS,106.5,114.9,106.2,117.0
2026-10-12,2634,14,17,20,30,10,3,10,20,10,120,6,7,15,4,2,4,7,4,6,0.3,1,89,122.9,122.4,OBS,106.4,114.6,106.1,116.6
2026-10-13,2634,15,33,33,20,27,27,41,41,41,262,18,18,7,12,12,28,28,28,18,1.0,5,65,120.5,120.0,PRD,106.1,114.3,105.6,116.2
2026-11-01,2635,10,,,,,,,,,,,,,,,,,,,,,75,119.8,116.5,PRM,113.9,106.9,111.0,105.9
2026-12-01,2636,10,,,,,,,,,,,,,,,,,,,,,75,118.0,116.5,PRM,112.0,106.9,111.0,105.9
";

    #[test]
    fn mjd() {
        assert_eq!(parse_date_mjd("1858-11-17"), Some(0));
        assert_eq!(parse_date_mjd("2000-01-01"), Some(51_544));
    }

    #[test]
    fn parses_and_expands() {
        let sw = SpaceWeather::from_celestrak_csv(SAMPLE).unwrap();
        let first = parse_date_mjd("2026-10-11").unwrap();
        assert_eq!(
            sw.coverage(),
            (first, parse_date_mjd("2026-12-30").unwrap())
        );
        // Gap Oct 14–31 held from Oct 13.
        assert_eq!(sw.day(first + 10).f107, 120.5);
        let nov = sw.day(parse_date_mjd("2026-11-15").unwrap());
        assert_eq!(nov.f107, 119.8);
        assert!((nov.ap_daily - (13.0 + 6.0 + 18.0) / 3.0).abs() < 1e-12);
    }

    #[test]
    fn drivers_history() {
        let sw = SpaceWeather::from_celestrak_csv(SAMPLE).unwrap();
        // 2026-10-13 04:30 UTC: slot 1.
        let d = sw.msis_drivers(Epoch::from_utc(2026, 10, 13, 4, 30, 0.0));
        assert_eq!(d.f107, 122.9);
        assert_eq!(d.f107_average, 106.1);
        assert_eq!(&d.ap[..5], &[18.0, 18.0, 18.0, 4.0, 7.0]);
        // t−12…−33 h: back 4..12 → Oct 12 slots 5,4,…,0 then Oct 11 slots 7,6.
        let m1 = (4.0 + 2.0 + 4.0 + 15.0 + 7.0 + 6.0 + 7.0 + 6.0) / 8.0;
        assert!((d.ap[5] - m1).abs() < 1e-12);
    }
}
