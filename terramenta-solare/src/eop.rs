//! Earth orientation parameters (EOP).
//!
//! Sources:
//! - IERS `finals2000A.all` / `finals.all` (fixed-width, Bulletin A columns).
//! - CelesTrak `EOP-All.csv`.
//!
//! Linear interpolation in MJD (UTC). UT1−UTC is de-stepped across leap seconds.
//! Outside coverage, the nearest record is held.

use crate::time::Epoch;

const ARCSEC_TO_RAD: f64 = std::f64::consts::PI / (180.0 * 3600.0);
const MAS_TO_ARCSEC: f64 = 1.0e-3;

/// EOP at one instant. Angles rad, times s.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Eop {
    /// Polar motion x_p.
    pub xp: f64,
    /// Polar motion y_p.
    pub yp: f64,
    /// UT1 − UTC.
    pub ut1_minus_utc: f64,
    /// Excess length of day.
    pub lod: f64,
    /// IAU-1980 nutation corrections δΔψ, δΔε.
    pub dpsi: f64,
    pub deps: f64,
    /// IAU-2000A CIP offsets δX, δY.
    pub dx: f64,
    pub dy: f64,
}

/// One tabulated EOP record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EopRecord {
    pub mjd_utc: f64,
    pub eop: Eop,
}

/// Which nutation corrections an IERS finals file carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalsKind {
    /// `finals2000A.*`: δX, δY.
    Iau2000A,
    /// `finals.*`: δΔψ, δΔε.
    Iau1980,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EopError {
    Empty,
    MissingColumn(&'static str),
    Parse { line: usize, field: &'static str },
}

impl std::fmt::Display for EopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "no EOP records"),
            Self::MissingColumn(c) => write!(f, "missing column {c}"),
            Self::Parse { line, field } => write!(f, "line {line}: bad {field}"),
        }
    }
}

impl std::error::Error for EopError {}

/// Time-ordered EOP table.
#[derive(Debug, Clone, Default)]
pub struct EopTable {
    records: Vec<EopRecord>,
}

impl EopTable {
    pub fn from_records(mut records: Vec<EopRecord>) -> Result<Self, EopError> {
        if records.is_empty() {
            return Err(EopError::Empty);
        }
        records.sort_by(|a, b| a.mjd_utc.total_cmp(&b.mjd_utc));
        Ok(Self { records })
    }

    /// Parses IERS `finals*.all` / `finals*.daily` / `finals*.data`.
    /// Rows lacking UT1−UTC (beyond prediction horizon) are skipped.
    pub fn from_iers_finals(text: &str, kind: FinalsKind) -> Result<Self, EopError> {
        let mut records = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line_no = index + 1;
            let field = |start: usize, end: usize| -> Option<&str> {
                line.get(start - 1..end)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            };
            let parse = |start, end, name| -> Result<Option<f64>, EopError> {
                field(start, end)
                    .map(|s| {
                        s.parse::<f64>().map_err(|_| EopError::Parse {
                            line: line_no,
                            field: name,
                        })
                    })
                    .transpose()
            };
            let Some(ut1) = parse(59, 68, "UT1-UTC")? else {
                continue;
            };
            let Some(mjd) = parse(8, 15, "MJD")? else {
                continue;
            };
            let xp = parse(19, 27, "PM-x")?.unwrap_or(0.0);
            let yp = parse(38, 46, "PM-y")?.unwrap_or(0.0);
            let lod_ms = parse(80, 86, "LOD")?.unwrap_or(0.0);
            let n1_mas = parse(98, 106, "nutation 1")?.unwrap_or(0.0);
            let n2_mas = parse(117, 125, "nutation 2")?.unwrap_or(0.0);

            let n1 = n1_mas * MAS_TO_ARCSEC * ARCSEC_TO_RAD;
            let n2 = n2_mas * MAS_TO_ARCSEC * ARCSEC_TO_RAD;
            let mut eop = Eop {
                xp: xp * ARCSEC_TO_RAD,
                yp: yp * ARCSEC_TO_RAD,
                ut1_minus_utc: ut1,
                lod: lod_ms * 1.0e-3,
                ..Eop::default()
            };
            match kind {
                FinalsKind::Iau2000A => (eop.dx, eop.dy) = (n1, n2),
                FinalsKind::Iau1980 => (eop.dpsi, eop.deps) = (n1, n2),
            }
            records.push(EopRecord { mjd_utc: mjd, eop });
        }
        Self::from_records(records)
    }

    /// Parses CelesTrak `EOP-All.csv` (header-driven; arcsec, s).
    pub fn from_celestrak_csv(text: &str) -> Result<Self, EopError> {
        let mut lines = text.lines().enumerate();
        let (_, header) = lines.next().ok_or(EopError::Empty)?;
        let columns: Vec<&str> = header.split(',').map(str::trim).collect();
        let col = |name: &'static str| {
            columns
                .iter()
                .position(|c| c.eq_ignore_ascii_case(name))
                .ok_or(EopError::MissingColumn(name))
        };
        let (c_mjd, c_x, c_y, c_ut1, c_lod) = (
            col("MJD")?,
            col("X")?,
            col("Y")?,
            col("UT1-UTC")?,
            col("LOD")?,
        );
        let (c_dpsi, c_deps, c_dx, c_dy) = (
            col("DPSI").ok(),
            col("DEPS").ok(),
            col("DX").ok(),
            col("DY").ok(),
        );

        let mut records = Vec::new();
        for (index, line) in lines {
            if line.trim().is_empty() {
                continue;
            }
            let cells: Vec<&str> = line.split(',').map(str::trim).collect();
            let get = |c: usize, name: &'static str| -> Result<f64, EopError> {
                cells
                    .get(c)
                    .and_then(|s| s.parse().ok())
                    .ok_or(EopError::Parse {
                        line: index + 1,
                        field: name,
                    })
            };
            let opt = |c: Option<usize>| {
                c.and_then(|c| cells.get(c))
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0)
            };
            records.push(EopRecord {
                mjd_utc: get(c_mjd, "MJD")?,
                eop: Eop {
                    xp: get(c_x, "X")? * ARCSEC_TO_RAD,
                    yp: get(c_y, "Y")? * ARCSEC_TO_RAD,
                    ut1_minus_utc: get(c_ut1, "UT1-UTC")?,
                    lod: get(c_lod, "LOD")?,
                    dpsi: opt(c_dpsi) * ARCSEC_TO_RAD,
                    deps: opt(c_deps) * ARCSEC_TO_RAD,
                    dx: opt(c_dx) * ARCSEC_TO_RAD,
                    dy: opt(c_dy) * ARCSEC_TO_RAD,
                },
            });
        }
        Self::from_records(records)
    }

    pub fn records(&self) -> &[EopRecord] {
        &self.records
    }

    /// Covered MJD (UTC) range.
    pub fn coverage(&self) -> Option<(f64, f64)> {
        Some((self.records.first()?.mjd_utc, self.records.last()?.mjd_utc))
    }

    pub fn at(&self, epoch: Epoch) -> Eop {
        self.at_mjd_utc(epoch.mjd_utc())
    }

    pub fn at_mjd_utc(&self, mjd: f64) -> Eop {
        let r = &self.records;
        match r.len() {
            0 => return Eop::default(),
            1 => return r[0].eop,
            _ => {}
        }
        if mjd <= r[0].mjd_utc {
            return r[0].eop;
        }
        if mjd >= r[r.len() - 1].mjd_utc {
            return r[r.len() - 1].eop;
        }
        let i = r.partition_point(|x| x.mjd_utc <= mjd) - 1;
        let (a, b) = (&r[i], &r[i + 1]);
        let f = (mjd - a.mjd_utc) / (b.mjd_utc - a.mjd_utc);
        let lerp = |x: f64, y: f64| x + f * (y - x);
        let mut dut1 = b.eop.ut1_minus_utc - a.eop.ut1_minus_utc;
        dut1 -= dut1.round(); // leap-second step
        let (ea, eb) = (a.eop, b.eop);
        Eop {
            xp: lerp(ea.xp, eb.xp),
            yp: lerp(ea.yp, eb.yp),
            ut1_minus_utc: ea.ut1_minus_utc + f * dut1,
            lod: lerp(ea.lod, eb.lod),
            dpsi: lerp(ea.dpsi, eb.dpsi),
            deps: lerp(ea.deps, eb.deps),
            dx: lerp(ea.dx, eb.dx),
            dy: lerp(ea.dy, eb.dy),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // finals2000A.all rows, 2016-12-30..2017-01-02 (leap second at 2017-01-01).
    const FINALS: &str = "\
161230 57752.00 I  0.082883 0.000052  0.263539 0.000038  I-0.4069180 0.0000079  0.8286 0.0056  I     0.022    0.119    -0.155    0.024  0.082924  0.263518 -0.4069106    -0.023    -0.038
161231 57753.00 I  0.081400 0.000052  0.263094 0.000039  I-0.4077601 0.0000078  0.8842 0.0055  I     0.025    0.119    -0.169    0.024  0.081318  0.262990 -0.4077600    -0.021    -0.048
17 1 1 57754.00 I  0.080504 0.000028  0.263145 0.000028  I 0.5912821 0.0000077  1.0342 0.0050  I     0.012    0.119    -0.168    0.018  0.080450  0.263074  0.5912975    -0.019    -0.057
17 1 2 57755.00 I  0.080285 0.000029  0.263605 0.000032  I 0.5901752 0.0000063  1.1730 0.0048  I    -0.001    0.040    -0.156    0.033  0.080275  0.263595  0.5902149    -0.027    -0.065
";

    #[test]
    fn parses_finals_columns() {
        let t = EopTable::from_iers_finals(FINALS, FinalsKind::Iau2000A).unwrap();
        assert_eq!(t.records().len(), 4);
        let e = t.at_mjd_utc(57752.0);
        assert!((e.xp / ARCSEC_TO_RAD - 0.082883).abs() < 1e-12);
        assert!((e.yp / ARCSEC_TO_RAD - 0.263539).abs() < 1e-12);
        assert_eq!(e.ut1_minus_utc, -0.4069180);
        assert!((e.lod - 0.8286e-3).abs() < 1e-15);
        assert!((e.dx / ARCSEC_TO_RAD - 0.022e-3).abs() < 1e-12);
        assert!((e.dy / ARCSEC_TO_RAD + 0.155e-3).abs() < 1e-12);
    }

    #[test]
    fn interpolates_across_leap_second() {
        let t = EopTable::from_iers_finals(FINALS, FinalsKind::Iau2000A).unwrap();
        let mid = t.at_mjd_utc(57753.5).ut1_minus_utc;
        assert!((mid - (-0.4077601 + 0.5 * (0.5912821 - 1.0 + 0.4077601))).abs() < 1e-12);
    }

    #[test]
    fn holds_endpoints() {
        let t = EopTable::from_iers_finals(FINALS, FinalsKind::Iau2000A).unwrap();
        assert_eq!(t.at_mjd_utc(50000.0), t.records()[0].eop);
        assert_eq!(t.at_mjd_utc(60000.0), t.records()[3].eop);
    }

    #[test]
    fn parses_celestrak_csv() {
        let csv = "DATE,MJD,X,Y,UT1-UTC,LOD,DPSI,DEPS,DX,DY,DAT,DATA_TYPE\n\
                   2004-04-06,53101,-0.140682,0.333309,-0.4399619,0.0015563,-0.052195,-0.003875,0.000000,0.000000,32,O\n";
        let t = EopTable::from_celestrak_csv(csv).unwrap();
        let e = t.at_mjd_utc(53101.0);
        assert!((e.dpsi / ARCSEC_TO_RAD + 0.052195).abs() < 1e-12);
        assert_eq!(e.ut1_minus_utc, -0.4399619);
    }
}
