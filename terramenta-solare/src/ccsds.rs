//! CCSDS Orbit Data Messages (502.0-B-3), KVN: OEM and OPM read/write; Attitude Data
//! Messages (504.0-B-1), KVN: quaternion AEM read/write; Conjunction Data Messages
//! (508.0-B-1), KVN: CDM read/write.
//!
//! Frames and centres are carried as strings; no frame conversion is applied.
//! OEM covariance: lower-triangle `[r, v]` blocks, `EPOCH` and optional `COV_REF_FRAME`.

use glam::{DQuat, DVec3};
use hifitime::TimeScale;

use crate::covariance::{Matrix6, from_lower_triangle};
use crate::frame::StateVector;
use crate::interpolate::Interpolation;
use crate::time::Epoch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcsdsError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for CcsdsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for CcsdsError {}

fn err(line: usize, message: impl Into<String>) -> CcsdsError {
    CcsdsError {
        line,
        message: message.into(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimeSystem {
    #[default]
    Utc,
    Tai,
    Tt,
    Tdb,
    Gps,
}

impl TimeSystem {
    fn scale(self) -> TimeScale {
        match self {
            Self::Utc => TimeScale::UTC,
            Self::Tai => TimeScale::TAI,
            Self::Tt => TimeScale::TT,
            Self::Tdb => TimeScale::TDB,
            Self::Gps => TimeScale::GPST,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Utc => "UTC",
            Self::Tai => "TAI",
            Self::Tt => "TT",
            Self::Tdb => "TDB",
            Self::Gps => "GPS",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "UTC" => Self::Utc,
            "TAI" => Self::Tai,
            "TT" => Self::Tt,
            "TDB" => Self::Tdb,
            "GPS" => Self::Gps,
            _ => return None,
        })
    }

    /// `YYYY-MM-DDThh:mm:ss.fffffffff`.
    pub fn format(self, epoch: Epoch) -> String {
        let (y, mo, d, h, mi, s, ns) = epoch.to_gregorian(self.scale());
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ns:09}")
    }

    /// `YYYY-MM-DDThh:mm:ss[.f][Z]` or `YYYY-DDDThh:mm:ss[.f][Z]`.
    pub fn parse_epoch(self, s: &str) -> Option<Epoch> {
        let s = s.trim().trim_end_matches('Z');
        let (date, time) = s.split_once('T')?;
        let parts: Vec<&str> = date.split('-').collect();
        let year: i32 = parts.first()?.parse().ok()?;
        let (month, day) = match parts.len() {
            3 => (parts[1].parse().ok()?, parts[2].parse().ok()?),
            2 => month_day(year, parts[1].parse().ok()?)?,
            _ => return None,
        };
        let mut t = time.split(':');
        let hour: u8 = t.next()?.parse().ok()?;
        let minute: u8 = t.next()?.parse().ok()?;
        let sec_str = t.next()?;
        let (whole, frac) = sec_str.split_once('.').unwrap_or((sec_str, ""));
        let second: u8 = whole.parse().ok()?;
        // Leap second: build :59 and add 1 s.
        let (second, extra) = if second == 60 {
            (59, 1.0)
        } else {
            (second, 0.0)
        };
        let nanos: u32 = if frac.is_empty() {
            0
        } else {
            let digits: String = frac.chars().chain(std::iter::repeat('0')).take(9).collect();
            digits.parse().ok()?
        };
        Some(
            Epoch::from_hifitime(
                hifitime::Epoch::maybe_from_gregorian(
                    year,
                    month,
                    day,
                    hour,
                    minute,
                    second,
                    nanos,
                    self.scale(),
                )
                .ok()?,
            )
            .advanced_by_seconds(extra),
        )
    }
}

fn month_day(year: i32, doy: u16) -> Option<(u8, u8)> {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let lengths = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut d = doy;
    for (m, len) in lengths.iter().enumerate() {
        if d >= 1 && d <= *len {
            return Some((m as u8 + 1, d as u8));
        }
        d = d.checked_sub(*len)?;
    }
    None
}

/// Common header.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub version: String,
    pub creation_date: String,
    pub originator: String,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            version: "2.0".into(),
            creation_date: String::new(),
            originator: "TERRAMENTA".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OemMetadata {
    pub object_name: String,
    pub object_id: String,
    pub center_name: String,
    pub ref_frame: String,
    pub time_system: TimeSystem,
    pub interpolation: Option<Interpolation>,
}

impl Default for OemMetadata {
    fn default() -> Self {
        Self {
            object_name: "UNKNOWN".into(),
            object_id: "UNKNOWN".into(),
            center_name: "EARTH".into(),
            ref_frame: "GCRF".into(),
            time_system: TimeSystem::Utc,
            interpolation: Some(Interpolation::default()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OemSegment {
    pub metadata: OemMetadata,
    pub states: Vec<(Epoch, StateVector)>,
    pub covariances: Vec<OemCovariance>,
}

/// `[r, v]` covariance, km², km²/s, km²/s².
#[derive(Debug, Clone, PartialEq)]
pub struct OemCovariance {
    pub epoch: Epoch,
    /// `COV_REF_FRAME`; `None` = the segment's `REF_FRAME`.
    pub ref_frame: Option<String>,
    pub matrix: Matrix6,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Oem {
    pub header: Header,
    pub segments: Vec<OemSegment>,
}

fn write_header(out: &mut String, kind: &str, h: &Header) {
    use std::fmt::Write;
    let _ = writeln!(out, "CCSDS_{kind}_VERS = {}", h.version);
    let _ = writeln!(out, "CREATION_DATE = {}", h.creation_date);
    let _ = writeln!(out, "ORIGINATOR = {}", h.originator);
}

/// `(line number, key, value)` with units `[..]` stripped.
fn kv(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once('=')?;
    let v = v.split('[').next().unwrap_or(v).trim();
    Some((k.trim(), v))
}

impl Oem {
    pub fn to_kvn(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        write_header(&mut out, "OEM", &self.header);
        for seg in &self.segments {
            let m = &seg.metadata;
            let ts = m.time_system;
            let (Some(first), Some(last)) = (seg.states.first(), seg.states.last()) else {
                continue;
            };
            let _ = writeln!(out, "\nMETA_START");
            let _ = writeln!(out, "OBJECT_NAME = {}", m.object_name);
            let _ = writeln!(out, "OBJECT_ID = {}", m.object_id);
            let _ = writeln!(out, "CENTER_NAME = {}", m.center_name);
            let _ = writeln!(out, "REF_FRAME = {}", m.ref_frame);
            let _ = writeln!(out, "TIME_SYSTEM = {}", ts.as_str());
            let _ = writeln!(out, "START_TIME = {}", ts.format(first.0));
            let _ = writeln!(out, "STOP_TIME = {}", ts.format(last.0));
            match m.interpolation {
                Some(Interpolation::Lagrange { degree }) => {
                    let _ = writeln!(
                        out,
                        "INTERPOLATION = LAGRANGE\nINTERPOLATION_DEGREE = {degree}"
                    );
                }
                Some(Interpolation::Hermite { degree }) => {
                    let _ = writeln!(
                        out,
                        "INTERPOLATION = HERMITE\nINTERPOLATION_DEGREE = {degree}"
                    );
                }
                None => {}
            }
            let _ = writeln!(out, "META_STOP\n");
            for (e, s) in &seg.states {
                let (r, v) = (s.position_km, s.velocity_km_s);
                let _ = writeln!(
                    out,
                    "{} {:.9} {:.9} {:.9} {:.12} {:.12} {:.12}",
                    ts.format(*e),
                    r.x,
                    r.y,
                    r.z,
                    v.x,
                    v.y,
                    v.z
                );
            }
            if !seg.covariances.is_empty() {
                let _ = writeln!(out, "\nCOVARIANCE_START");
                for c in &seg.covariances {
                    let _ = writeln!(out, "EPOCH = {}", ts.format(c.epoch));
                    if let Some(frame) = &c.ref_frame {
                        let _ = writeln!(out, "COV_REF_FRAME = {frame}");
                    }
                    for (i, row) in c.matrix.iter().enumerate() {
                        let line: Vec<String> =
                            row[..=i].iter().map(|x| format!("{x:.7e}")).collect();
                        let _ = writeln!(out, "{}", line.join(" "));
                    }
                }
                let _ = writeln!(out, "COVARIANCE_STOP");
            }
        }
        out
    }

    pub fn from_kvn(text: &str) -> Result<Self, CcsdsError> {
        let mut oem = Oem::default();
        // Covariance being read: epoch, frame, lower-triangle values so far.
        let mut cov: Option<(Epoch, Option<String>, Vec<f64>)> = None;
        let mut in_meta = false;
        let mut in_cov = false;
        let mut interp_kind: Option<String> = None;
        let mut interp_degree = 0;
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            let n = i + 1;
            if line.is_empty() || line.starts_with("COMMENT") {
                continue;
            }
            match line {
                "META_START" => {
                    in_meta = true;
                    interp_kind = None;
                    interp_degree = 0;
                    oem.segments.push(OemSegment {
                        metadata: OemMetadata {
                            interpolation: None,
                            ..OemMetadata::default()
                        },
                        states: Vec::new(),
                        covariances: Vec::new(),
                    });
                    continue;
                }
                "META_STOP" => {
                    in_meta = false;
                    let seg = oem
                        .segments
                        .last_mut()
                        .ok_or_else(|| err(n, "META_STOP without META_START"))?;
                    seg.metadata.interpolation = match interp_kind.as_deref() {
                        Some("LAGRANGE") => Some(Interpolation::Lagrange {
                            degree: interp_degree,
                        }),
                        Some("HERMITE") => Some(Interpolation::Hermite {
                            degree: interp_degree,
                        }),
                        _ => None,
                    };
                    continue;
                }
                "COVARIANCE_START" => {
                    in_cov = true;
                    continue;
                }
                "COVARIANCE_STOP" => {
                    if cov.take().is_some() {
                        return Err(err(n, "covariance needs 21 values"));
                    }
                    in_cov = false;
                    continue;
                }
                _ => {}
            }
            if in_cov {
                let seg = oem
                    .segments
                    .last_mut()
                    .ok_or_else(|| err(n, "covariance before metadata"))?;
                if let Some((k, v)) = kv(line) {
                    match k {
                        "EPOCH" => {
                            if cov.is_some() {
                                return Err(err(n, "covariance needs 21 values"));
                            }
                            let epoch = seg
                                .metadata
                                .time_system
                                .parse_epoch(v)
                                .ok_or_else(|| err(n, "bad EPOCH"))?;
                            cov = Some((epoch, None, Vec::with_capacity(21)));
                        }
                        "COV_REF_FRAME" => {
                            let c = cov
                                .as_mut()
                                .ok_or_else(|| err(n, "COV_REF_FRAME before EPOCH"))?;
                            c.1 = Some(v.into());
                        }
                        _ => {}
                    }
                    continue;
                }
                let c = cov
                    .as_mut()
                    .ok_or_else(|| err(n, "covariance values before EPOCH"))?;
                for x in line.split_whitespace() {
                    c.2.push(x.parse().map_err(|_| err(n, "bad number"))?);
                }
                if c.2.len() >= 21 {
                    let (epoch, ref_frame, values) = cov.take().unwrap();
                    let values: [f64; 21] = values
                        .try_into()
                        .map_err(|_| err(n, "covariance needs 21 values"))?;
                    seg.covariances.push(OemCovariance {
                        epoch,
                        ref_frame,
                        matrix: from_lower_triangle(&values),
                    });
                }
                continue;
            }
            if in_meta {
                let (k, v) = kv(line).ok_or_else(|| err(n, "expected KEY = value"))?;
                let m = &mut oem.segments.last_mut().unwrap().metadata;
                match k {
                    "OBJECT_NAME" => m.object_name = v.into(),
                    "OBJECT_ID" => m.object_id = v.into(),
                    "CENTER_NAME" => m.center_name = v.into(),
                    "REF_FRAME" => m.ref_frame = v.into(),
                    "TIME_SYSTEM" => {
                        m.time_system = TimeSystem::parse(v)
                            .ok_or_else(|| err(n, format!("unsupported TIME_SYSTEM {v}")))?
                    }
                    "INTERPOLATION" => interp_kind = Some(v.to_uppercase()),
                    "INTERPOLATION_DEGREE" => {
                        interp_degree = v.parse().map_err(|_| err(n, "bad INTERPOLATION_DEGREE"))?
                    }
                    _ => {}
                }
                continue;
            }
            if let Some((k, v)) = kv(line) {
                match k {
                    "CCSDS_OEM_VERS" => oem.header.version = v.into(),
                    "CREATION_DATE" => oem.header.creation_date = v.into(),
                    "ORIGINATOR" => oem.header.originator = v.into(),
                    _ => {}
                }
                continue;
            }
            let seg = oem
                .segments
                .last_mut()
                .ok_or_else(|| err(n, "data before metadata"))?;
            let mut it = line.split_whitespace();
            let epoch = it
                .next()
                .and_then(|s| seg.metadata.time_system.parse_epoch(s))
                .ok_or_else(|| err(n, "bad epoch"))?;
            let vals: Vec<f64> = it
                .take(6)
                .map(|s| s.parse::<f64>().map_err(|_| err(n, "bad number")))
                .collect::<Result<_, _>>()?;
            if vals.len() != 6 {
                return Err(err(n, "expected 6 state components"));
            }
            seg.states.push((
                epoch,
                StateVector::new(
                    DVec3::new(vals[0], vals[1], vals[2]),
                    DVec3::new(vals[3], vals[4], vals[5]),
                ),
            ));
        }
        Ok(oem)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpmManeuver {
    pub epoch_ignition: Epoch,
    pub duration_s: f64,
    /// Negative for mass loss.
    pub delta_mass_kg: f64,
    /// `MAN_REF_FRAME` string is held in [`Opm::maneuver_frames`].
    pub delta_v_km_s: DVec3,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Opm {
    pub header: Header,
    pub object_name: String,
    pub object_id: String,
    pub center_name: String,
    pub ref_frame: String,
    pub time_system: TimeSystem,
    pub epoch: Epoch,
    pub state: StateVector,
    pub mass_kg: Option<f64>,
    pub solar_rad_area_m2: Option<f64>,
    pub solar_rad_coeff: Option<f64>,
    pub drag_area_m2: Option<f64>,
    pub drag_coeff: Option<f64>,
    pub maneuvers: Vec<OpmManeuver>,
    pub maneuver_frames: Vec<String>,
}

impl Opm {
    pub fn new(epoch: Epoch, state: StateVector) -> Self {
        let m = OemMetadata::default();
        Self {
            header: Header::default(),
            object_name: m.object_name,
            object_id: m.object_id,
            center_name: m.center_name,
            ref_frame: m.ref_frame,
            time_system: m.time_system,
            epoch,
            state,
            mass_kg: None,
            solar_rad_area_m2: None,
            solar_rad_coeff: None,
            drag_area_m2: None,
            drag_coeff: None,
            maneuvers: Vec::new(),
            maneuver_frames: Vec::new(),
        }
    }

    pub fn to_kvn(&self) -> String {
        use std::fmt::Write;
        let ts = self.time_system;
        let mut out = String::new();
        write_header(&mut out, "OPM", &self.header);
        let _ = writeln!(out, "\nOBJECT_NAME = {}", self.object_name);
        let _ = writeln!(out, "OBJECT_ID = {}", self.object_id);
        let _ = writeln!(out, "CENTER_NAME = {}", self.center_name);
        let _ = writeln!(out, "REF_FRAME = {}", self.ref_frame);
        let _ = writeln!(out, "TIME_SYSTEM = {}\n", ts.as_str());
        let _ = writeln!(out, "EPOCH = {}", ts.format(self.epoch));
        let (r, v) = (self.state.position_km, self.state.velocity_km_s);
        for (k, x, u) in [
            ("X", r.x, "km"),
            ("Y", r.y, "km"),
            ("Z", r.z, "km"),
            ("X_DOT", v.x, "km/s"),
            ("Y_DOT", v.y, "km/s"),
            ("Z_DOT", v.z, "km/s"),
        ] {
            let _ = writeln!(out, "{k} = {x:.12} [{u}]");
        }
        let params = [
            ("MASS", self.mass_kg, "kg"),
            ("SOLAR_RAD_AREA", self.solar_rad_area_m2, "m**2"),
            ("SOLAR_RAD_COEFF", self.solar_rad_coeff, ""),
            ("DRAG_AREA", self.drag_area_m2, "m**2"),
            ("DRAG_COEFF", self.drag_coeff, ""),
        ];
        if params.iter().any(|p| p.1.is_some()) {
            out.push('\n');
        }
        for (k, x, u) in params {
            if let Some(x) = x {
                let unit = if u.is_empty() {
                    String::new()
                } else {
                    format!(" [{u}]")
                };
                let _ = writeln!(out, "{k} = {x}{unit}");
            }
        }
        for (i, m) in self.maneuvers.iter().enumerate() {
            let frame = self.maneuver_frames.get(i).map_or("RSW", String::as_str);
            let _ = writeln!(
                out,
                "\nMAN_EPOCH_IGNITION = {}",
                ts.format(m.epoch_ignition)
            );
            let _ = writeln!(out, "MAN_DURATION = {} [s]", m.duration_s);
            let _ = writeln!(out, "MAN_DELTA_MASS = {} [kg]", m.delta_mass_kg);
            let _ = writeln!(out, "MAN_REF_FRAME = {frame}");
            let d = m.delta_v_km_s;
            let _ = writeln!(
                out,
                "MAN_DV_1 = {:.12} [km/s]\nMAN_DV_2 = {:.12} [km/s]\nMAN_DV_3 = {:.12} [km/s]",
                d.x, d.y, d.z
            );
        }
        out
    }

    pub fn from_kvn(text: &str) -> Result<Self, CcsdsError> {
        let mut opm = Opm::new(Epoch::J2000, StateVector::ZERO);
        let mut epoch_str = None;
        let mut man_epochs: Vec<(usize, String)> = Vec::new();
        let mut comps = [None; 6];
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            let n = i + 1;
            if line.is_empty() || line.starts_with("COMMENT") {
                continue;
            }
            let Some((k, v)) = kv(line) else {
                continue;
            };
            let num = || {
                v.parse::<f64>()
                    .map_err(|_| err(n, format!("bad number for {k}")))
            };
            let last_man = |opm: &mut Opm| -> Result<usize, CcsdsError> {
                opm.maneuvers
                    .len()
                    .checked_sub(1)
                    .ok_or_else(|| err(n, "maneuver field before MAN_EPOCH_IGNITION"))
            };
            match k {
                "CCSDS_OPM_VERS" => opm.header.version = v.into(),
                "CREATION_DATE" => opm.header.creation_date = v.into(),
                "ORIGINATOR" => opm.header.originator = v.into(),
                "OBJECT_NAME" => opm.object_name = v.into(),
                "OBJECT_ID" => opm.object_id = v.into(),
                "CENTER_NAME" => opm.center_name = v.into(),
                "REF_FRAME" => opm.ref_frame = v.into(),
                "TIME_SYSTEM" => {
                    opm.time_system = TimeSystem::parse(v)
                        .ok_or_else(|| err(n, format!("unsupported TIME_SYSTEM {v}")))?
                }
                "EPOCH" => epoch_str = Some((n, v.to_string())),
                "X" => comps[0] = Some(num()?),
                "Y" => comps[1] = Some(num()?),
                "Z" => comps[2] = Some(num()?),
                "X_DOT" => comps[3] = Some(num()?),
                "Y_DOT" => comps[4] = Some(num()?),
                "Z_DOT" => comps[5] = Some(num()?),
                "MASS" => opm.mass_kg = Some(num()?),
                "SOLAR_RAD_AREA" => opm.solar_rad_area_m2 = Some(num()?),
                "SOLAR_RAD_COEFF" => opm.solar_rad_coeff = Some(num()?),
                "DRAG_AREA" => opm.drag_area_m2 = Some(num()?),
                "DRAG_COEFF" => opm.drag_coeff = Some(num()?),
                "MAN_EPOCH_IGNITION" => {
                    man_epochs.push((n, v.to_string()));
                    opm.maneuvers.push(OpmManeuver {
                        epoch_ignition: Epoch::J2000,
                        duration_s: 0.0,
                        delta_mass_kg: 0.0,
                        delta_v_km_s: DVec3::ZERO,
                    });
                    opm.maneuver_frames.push("RSW".into());
                }
                "MAN_DURATION" => {
                    let i = last_man(&mut opm)?;
                    opm.maneuvers[i].duration_s = num()?;
                }
                "MAN_DELTA_MASS" => {
                    let i = last_man(&mut opm)?;
                    opm.maneuvers[i].delta_mass_kg = num()?;
                }
                "MAN_REF_FRAME" => {
                    let i = last_man(&mut opm)?;
                    opm.maneuver_frames[i] = v.into();
                }
                "MAN_DV_1" | "MAN_DV_2" | "MAN_DV_3" => {
                    let i = last_man(&mut opm)?;
                    let x = num()?;
                    let d = &mut opm.maneuvers[i].delta_v_km_s;
                    match k {
                        "MAN_DV_1" => d.x = x,
                        "MAN_DV_2" => d.y = x,
                        _ => d.z = x,
                    }
                }
                _ => {}
            }
        }
        // TIME_SYSTEM may follow EPOCH-bearing lines only in malformed files; parse after.
        let (n, e) = epoch_str.ok_or_else(|| err(0, "missing EPOCH"))?;
        opm.epoch = opm
            .time_system
            .parse_epoch(&e)
            .ok_or_else(|| err(n, "bad EPOCH"))?;
        for (i, (n, e)) in man_epochs.into_iter().enumerate() {
            opm.maneuvers[i].epoch_ignition = opm
                .time_system
                .parse_epoch(&e)
                .ok_or_else(|| err(n, "bad MAN_EPOCH_IGNITION"))?;
        }
        let c: Vec<f64> = comps
            .iter()
            .map(|c| c.ok_or_else(|| err(0, "incomplete state vector")))
            .collect::<Result<_, _>>()?;
        opm.state = StateVector::new(DVec3::new(c[0], c[1], c[2]), DVec3::new(c[3], c[4], c[5]));
        Ok(opm)
    }
}

// --- AEM ------------------------------------------------------------------------------

/// Quaternion AEM segment metadata. Quaternions are held A → B (body → inertial in
/// [`crate::attitude`] terms when A is inertial).
#[derive(Debug, Clone, PartialEq)]
pub struct AemMetadata {
    pub object_name: String,
    pub object_id: String,
    pub center_name: Option<String>,
    pub ref_frame_a: String,
    pub ref_frame_b: String,
    pub time_system: TimeSystem,
}

impl Default for AemMetadata {
    fn default() -> Self {
        Self {
            object_name: "UNKNOWN".into(),
            object_id: "UNKNOWN".into(),
            center_name: Some("EARTH".into()),
            // GCRF axes are ICRF axes; AEM 1.0 has no GCRF.
            ref_frame_a: "ICRF".into(),
            ref_frame_b: "SC_BODY_1".into(),
            time_system: TimeSystem::Utc,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AemSegment {
    pub metadata: AemMetadata,
    pub quaternions: Vec<(Epoch, DQuat)>,
}

/// Written as `ATTITUDE_DIR = A2B`, `QUATERNION_TYPE = LAST`, linear interpolation.
#[derive(Debug, Clone, PartialEq)]
pub struct Aem {
    pub header: Header,
    pub segments: Vec<AemSegment>,
}

impl Default for Aem {
    fn default() -> Self {
        Self {
            header: Header {
                version: "1.0".into(),
                ..Header::default()
            },
            segments: Vec::new(),
        }
    }
}

impl Aem {
    pub fn to_kvn(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        write_header(&mut out, "AEM", &self.header);
        for seg in &self.segments {
            let m = &seg.metadata;
            let ts = m.time_system;
            let (Some(first), Some(last)) = (seg.quaternions.first(), seg.quaternions.last())
            else {
                continue;
            };
            let _ = writeln!(out, "\nMETA_START");
            let _ = writeln!(out, "OBJECT_NAME = {}", m.object_name);
            let _ = writeln!(out, "OBJECT_ID = {}", m.object_id);
            if let Some(center) = &m.center_name {
                let _ = writeln!(out, "CENTER_NAME = {center}");
            }
            let _ = writeln!(out, "REF_FRAME_A = {}", m.ref_frame_a);
            let _ = writeln!(out, "REF_FRAME_B = {}", m.ref_frame_b);
            let _ = writeln!(out, "ATTITUDE_DIR = A2B");
            let _ = writeln!(out, "TIME_SYSTEM = {}", ts.as_str());
            let _ = writeln!(out, "START_TIME = {}", ts.format(first.0));
            let _ = writeln!(out, "STOP_TIME = {}", ts.format(last.0));
            let _ = writeln!(out, "ATTITUDE_TYPE = QUATERNION");
            let _ = writeln!(out, "QUATERNION_TYPE = LAST");
            let _ = writeln!(out, "INTERPOLATION_METHOD = LINEAR");
            let _ = writeln!(out, "INTERPOLATION_DEGREE = 1");
            let _ = writeln!(out, "META_STOP\n\nDATA_START");
            for (e, q) in &seg.quaternions {
                let _ = writeln!(
                    out,
                    "{} {:.12} {:.12} {:.12} {:.12}",
                    ts.format(*e),
                    q.x,
                    q.y,
                    q.z,
                    q.w
                );
            }
            let _ = writeln!(out, "DATA_STOP");
        }
        out
    }

    /// Quaternion segments only; `B2A` is conjugated to A → B. Other attitude types are
    /// an error.
    pub fn from_kvn(text: &str) -> Result<Self, CcsdsError> {
        let mut aem = Aem::default();
        let mut meta: Option<(AemMetadata, bool, bool)> = None; // (metadata, b2a, scalar first)
        let mut current: Option<(bool, bool)> = None;
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            let n = i + 1;
            if line.is_empty() || line.starts_with("COMMENT") || line == "DATA_START" {
                continue;
            }
            match line {
                "META_START" => {
                    let m = AemMetadata {
                        center_name: None,
                        ..AemMetadata::default()
                    };
                    meta = Some((m, false, false));
                    continue;
                }
                "META_STOP" => {
                    let (m, b2a, first) = meta
                        .take()
                        .ok_or_else(|| err(n, "META_STOP without META_START"))?;
                    aem.segments.push(AemSegment {
                        metadata: m,
                        quaternions: Vec::new(),
                    });
                    current = Some((b2a, first));
                    continue;
                }
                "DATA_STOP" => {
                    current = None;
                    continue;
                }
                _ => {}
            }
            if let Some((m, b2a, first)) = meta.as_mut() {
                let Some((k, v)) = kv(line) else {
                    continue;
                };
                match k {
                    "OBJECT_NAME" => m.object_name = v.into(),
                    "OBJECT_ID" => m.object_id = v.into(),
                    "CENTER_NAME" => m.center_name = Some(v.into()),
                    "REF_FRAME_A" => m.ref_frame_a = v.into(),
                    "REF_FRAME_B" => m.ref_frame_b = v.into(),
                    "ATTITUDE_DIR" => *b2a = v.eq_ignore_ascii_case("B2A"),
                    "QUATERNION_TYPE" => *first = v.eq_ignore_ascii_case("FIRST"),
                    "TIME_SYSTEM" => {
                        m.time_system = TimeSystem::parse(v)
                            .ok_or_else(|| err(n, format!("unsupported TIME_SYSTEM {v}")))?
                    }
                    "ATTITUDE_TYPE" if !v.eq_ignore_ascii_case("QUATERNION") => {
                        return Err(err(n, format!("unsupported ATTITUDE_TYPE {v}")));
                    }
                    _ => {}
                }
                continue;
            }
            if let Some((k, v)) = kv(line) {
                if k == "CCSDS_AEM_VERS" {
                    aem.header.version = v.into();
                } else if k == "CREATION_DATE" {
                    aem.header.creation_date = v.into();
                } else if k == "ORIGINATOR" {
                    aem.header.originator = v.into();
                }
                continue;
            }
            let (Some((b2a, first)), Some(seg)) = (current, aem.segments.last_mut()) else {
                return Err(err(n, "data outside a segment"));
            };
            let mut fields = line.split_whitespace();
            let epoch = fields
                .next()
                .and_then(|e| seg.metadata.time_system.parse_epoch(e))
                .ok_or_else(|| err(n, "bad epoch"))?;
            let c: Vec<f64> = fields
                .take(4)
                .map(|f| f.parse::<f64>().map_err(|_| err(n, "bad quaternion")))
                .collect::<Result<_, _>>()?;
            if c.len() < 4 {
                return Err(err(n, "expected four quaternion components"));
            }
            let q = if first {
                DQuat::from_xyzw(c[1], c[2], c[3], c[0])
            } else {
                DQuat::from_xyzw(c[0], c[1], c[2], c[3])
            };
            seg.quaternions
                .push((epoch, if b2a { q.conjugate() } else { q }));
        }
        Ok(aem)
    }
}

// --- CDM ------------------------------------------------------------------------------

/// CDM covariance keys, lower triangle in [`crate::covariance::lower_triangle`] order.
const CDM_COVARIANCE: [&str; 21] = [
    "CR_R",
    "CT_R",
    "CT_T",
    "CN_R",
    "CN_T",
    "CN_N",
    "CRDOT_R",
    "CRDOT_T",
    "CRDOT_N",
    "CRDOT_RDOT",
    "CTDOT_R",
    "CTDOT_T",
    "CTDOT_N",
    "CTDOT_RDOT",
    "CTDOT_TDOT",
    "CNDOT_R",
    "CNDOT_T",
    "CNDOT_N",
    "CNDOT_RDOT",
    "CNDOT_TDOT",
    "CNDOT_NDOT",
];

/// One object of a [`Cdm`].
#[derive(Debug, Clone, PartialEq)]
pub struct CdmObject {
    /// `OBJECT_DESIGNATOR` (catalogue number).
    pub designator: String,
    pub catalog_name: String,
    pub object_name: String,
    pub international_designator: String,
    pub ephemeris_name: String,
    /// `CALCULATED` | `DEFAULT`.
    pub covariance_method: String,
    /// `YES` | `NO` | `N/A`.
    pub maneuverable: String,
    pub ref_frame: String,
    /// At TCA, km and km/s.
    pub state: StateVector,
    /// `[r, v]` in the object's RTN axes, km², km²/s, km²/s² (the KVN carries m²).
    pub covariance_rtn: Matrix6,
}

impl Default for CdmObject {
    fn default() -> Self {
        Self {
            designator: "UNKNOWN".into(),
            catalog_name: "SATCAT".into(),
            object_name: "UNKNOWN".into(),
            international_designator: "UNKNOWN".into(),
            ephemeris_name: "NONE".into(),
            covariance_method: "CALCULATED".into(),
            maneuverable: "N/A".into(),
            ref_frame: "GCRF".into(),
            state: StateVector::ZERO,
            covariance_rtn: [[0.0; 6]; 6],
        }
    }
}

/// Conjunction Data Message (508.0-B-1), KVN; UTC. Object 2 relative to object 1 in
/// object 1's RTN axes. Optional screening and drag/SRP covariance rows are not carried.
#[derive(Debug, Clone, PartialEq)]
pub struct Cdm {
    pub header: Header,
    pub message_id: String,
    pub tca: Epoch,
    pub miss_distance_km: f64,
    pub relative_speed_km_s: Option<f64>,
    pub relative_position_rtn_km: Option<DVec3>,
    pub relative_velocity_rtn_km_s: Option<DVec3>,
    pub collision_probability: Option<f64>,
    pub collision_probability_method: Option<String>,
    pub objects: [CdmObject; 2],
}

impl Cdm {
    pub fn new(tca: Epoch, miss_distance_km: f64) -> Self {
        Self {
            header: Header {
                version: "1.0".into(),
                ..Header::default()
            },
            message_id: "UNKNOWN".into(),
            tca,
            miss_distance_km,
            relative_speed_km_s: None,
            relative_position_rtn_km: None,
            relative_velocity_rtn_km_s: None,
            collision_probability: None,
            collision_probability_method: None,
            objects: Default::default(),
        }
    }

    pub fn to_kvn(&self) -> String {
        use std::fmt::Write;
        let ts = TimeSystem::Utc;
        let mut out = String::new();
        write_header(&mut out, "CDM", &self.header);
        let _ = writeln!(out, "MESSAGE_ID = {}", self.message_id);
        let _ = writeln!(out, "TCA = {}", ts.format(self.tca));
        let _ = writeln!(out, "MISS_DISTANCE = {} [m]", self.miss_distance_km * 1e3);
        if let Some(s) = self.relative_speed_km_s {
            let _ = writeln!(out, "RELATIVE_SPEED = {} [m/s]", s * 1e3);
        }
        for (prefix, value, unit) in [
            ("RELATIVE_POSITION", self.relative_position_rtn_km, "m"),
            ("RELATIVE_VELOCITY", self.relative_velocity_rtn_km_s, "m/s"),
        ] {
            if let Some(v) = value {
                for (axis, x) in ["R", "T", "N"].iter().zip(v.to_array()) {
                    let _ = writeln!(out, "{prefix}_{axis} = {} [{unit}]", x * 1e3);
                }
            }
        }
        if let Some(p) = self.collision_probability {
            let _ = writeln!(out, "COLLISION_PROBABILITY = {p:e}");
        }
        if let Some(m) = &self.collision_probability_method {
            let _ = writeln!(out, "COLLISION_PROBABILITY_METHOD = {m}");
        }
        for (i, o) in self.objects.iter().enumerate() {
            let _ = writeln!(out, "\nOBJECT = OBJECT{}", i + 1);
            for (k, v) in [
                ("OBJECT_DESIGNATOR", &o.designator),
                ("CATALOG_NAME", &o.catalog_name),
                ("OBJECT_NAME", &o.object_name),
                ("INTERNATIONAL_DESIGNATOR", &o.international_designator),
                ("EPHEMERIS_NAME", &o.ephemeris_name),
                ("COVARIANCE_METHOD", &o.covariance_method),
                ("MANEUVERABLE", &o.maneuverable),
                ("REF_FRAME", &o.ref_frame),
            ] {
                let _ = writeln!(out, "{k} = {v}");
            }
            let (r, v) = (o.state.position_km, o.state.velocity_km_s);
            for (k, x, u) in [
                ("X", r.x, "km"),
                ("Y", r.y, "km"),
                ("Z", r.z, "km"),
                ("X_DOT", v.x, "km/s"),
                ("Y_DOT", v.y, "km/s"),
                ("Z_DOT", v.z, "km/s"),
            ] {
                let _ = writeln!(out, "{k} = {x:.9} [{u}]");
            }
            let lower = crate::covariance::lower_triangle(&o.covariance_rtn);
            for (k, x) in CDM_COVARIANCE.iter().zip(lower) {
                let unit = match k.matches("DOT").count() {
                    0 => "m**2",
                    1 => "m**2/s",
                    _ => "m**2/s**2",
                };
                let _ = writeln!(out, "{k} = {:e} [{unit}]", x * 1e6);
            }
        }
        out
    }

    pub fn from_kvn(text: &str) -> Result<Self, CcsdsError> {
        let mut cdm = Cdm::new(Epoch::J2000, f64::NAN);
        let mut tca = None;
        let mut rel = [[None; 3]; 2];
        // `None`: relative metadata.
        let mut object: Option<usize> = None;
        let mut states = [[None; 6]; 2];
        let mut covariances = [[None; 21]; 2];
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            let n = i + 1;
            if line.is_empty() || line.starts_with("COMMENT") {
                continue;
            }
            let Some((k, v)) = kv(line) else {
                continue;
            };
            let num = || {
                v.parse::<f64>()
                    .map_err(|_| err(n, format!("bad number for {k}")))
            };
            if k == "OBJECT" {
                object = Some(match v {
                    "OBJECT1" => 0,
                    "OBJECT2" => 1,
                    _ => return Err(err(n, format!("unknown OBJECT {v}"))),
                });
                continue;
            }
            let Some(o) = object else {
                match k {
                    "CCSDS_CDM_VERS" => cdm.header.version = v.into(),
                    "CREATION_DATE" => cdm.header.creation_date = v.into(),
                    "ORIGINATOR" => cdm.header.originator = v.into(),
                    "MESSAGE_ID" => cdm.message_id = v.into(),
                    "TCA" => {
                        tca = Some(
                            TimeSystem::Utc
                                .parse_epoch(v)
                                .ok_or_else(|| err(n, "bad TCA"))?,
                        )
                    }
                    "MISS_DISTANCE" => cdm.miss_distance_km = num()? * 1e-3,
                    "RELATIVE_SPEED" => cdm.relative_speed_km_s = Some(num()? * 1e-3),
                    "COLLISION_PROBABILITY" => cdm.collision_probability = Some(num()?),
                    "COLLISION_PROBABILITY_METHOD" => {
                        cdm.collision_probability_method = Some(v.into())
                    }
                    _ => {
                        let axis = |s: &str| ["_R", "_T", "_N"].iter().position(|a| s == *a);
                        if let Some(a) = k.strip_prefix("RELATIVE_POSITION").and_then(axis) {
                            rel[0][a] = Some(num()? * 1e-3);
                        } else if let Some(a) = k.strip_prefix("RELATIVE_VELOCITY").and_then(axis) {
                            rel[1][a] = Some(num()? * 1e-3);
                        }
                    }
                }
                continue;
            };
            let obj = &mut cdm.objects[o];
            match k {
                "OBJECT_DESIGNATOR" => obj.designator = v.into(),
                "CATALOG_NAME" => obj.catalog_name = v.into(),
                "OBJECT_NAME" => obj.object_name = v.into(),
                "INTERNATIONAL_DESIGNATOR" => obj.international_designator = v.into(),
                "EPHEMERIS_NAME" => obj.ephemeris_name = v.into(),
                "COVARIANCE_METHOD" => obj.covariance_method = v.into(),
                "MANEUVERABLE" => obj.maneuverable = v.into(),
                "REF_FRAME" => obj.ref_frame = v.into(),
                _ => {
                    if let Some(c) = ["X", "Y", "Z", "X_DOT", "Y_DOT", "Z_DOT"]
                        .iter()
                        .position(|s| *s == k)
                    {
                        states[o][c] = Some(num()?);
                    } else if let Some(c) = CDM_COVARIANCE.iter().position(|s| *s == k) {
                        covariances[o][c] = Some(num()? * 1e-6);
                    }
                }
            }
        }
        cdm.tca = tca.ok_or_else(|| err(0, "missing TCA"))?;
        if cdm.miss_distance_km.is_nan() {
            return Err(err(0, "missing MISS_DISTANCE"));
        }
        let vector = |c: [Option<f64>; 3]| Some(DVec3::new(c[0]?, c[1]?, c[2]?));
        cdm.relative_position_rtn_km = vector(rel[0]);
        cdm.relative_velocity_rtn_km_s = vector(rel[1]);
        for o in 0..2 {
            let s = states[o]
                .iter()
                .map(|c| c.ok_or_else(|| err(0, format!("OBJECT{}: incomplete state", o + 1))))
                .collect::<Result<Vec<_>, _>>()?;
            cdm.objects[o].state =
                StateVector::new(DVec3::new(s[0], s[1], s[2]), DVec3::new(s[3], s[4], s[5]));
            let lower: [f64; 21] = std::array::from_fn(|i| covariances[o][i].unwrap_or(f64::NAN));
            if lower.iter().any(|x| x.is_nan()) {
                return Err(err(0, format!("OBJECT{}: incomplete covariance", o + 1)));
            }
            cdm.objects[o].covariance_rtn = from_lower_triangle(&lower);
        }
        Ok(cdm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CCSDS 502.0-B-3 Figure 5-2 style sample (abridged).
    const OEM_SAMPLE: &str = "CCSDS_OEM_VERS = 2.0
COMMENT  OEM WITH OPTIONAL ACCELERATIONS
CREATION_DATE = 1996-11-04T17:22:31
ORIGINATOR = NASA/JPL

META_START
OBJECT_NAME          = MARS GLOBAL SURVEYOR
OBJECT_ID            = 1996-062A
CENTER_NAME          = MARS BARYCENTER
REF_FRAME            = EME2000
TIME_SYSTEM          = UTC
START_TIME           = 1996-12-28T21:29:07.267
USEABLE_START_TIME   = 1996-12-28T22:08:02.5
USEABLE_STOP_TIME    = 1996-12-30T01:18:02.5
STOP_TIME            = 1996-12-30T01:28:02.267
INTERPOLATION        = HERMITE
INTERPOLATION_DEGREE = 7
META_STOP

COMMENT  This file was produced by M.R. Somebody, MSOO NAV/JPL, 2000 NOV 04. It is
1996-12-28T21:29:07.267 -2432.166 -063.042 1742.754 7.33702 -3.495867 -1.041945 -0.001 0.002 0.003
1996-12-28T21:59:02.267 -2445.234 -878.141 1873.073 1.86043 -3.421256 -0.996366 -0.001 0.002 0.003
1996-12-28T22:00:02.267 -2458.079 -683.858 2007.684 6.87071 -3.287047 -0.941142 -0.001 0.002 0.003
1996-12-28T22:01:02.267 -2470.668 -489.875 2141.639 4.91624 -3.212455 -0.882845 -0.001 0.002 0.003

COVARIANCE_START
EPOCH = 1996-12-28T21:29:07.267
COV_REF_FRAME = EME2000
3.3313494e-04
4.6189273e-04 6.7824216e-04
-3.0700078e-04 -4.2212341e-04 3.2319319e-04
-3.3493650e-07 -4.6860842e-07 2.4849495e-07 4.2960228e-10
-2.2118325e-07 -2.8641868e-07 1.7980986e-07 2.6088992e-10 1.7675147e-10
-3.0413460e-07 -4.9894969e-07 3.5403109e-07 1.8692631e-10 1.0088625e-10 6.2244443e-10
COVARIANCE_STOP
";

    #[test]
    fn reads_oem_sample() {
        let oem = Oem::from_kvn(OEM_SAMPLE).unwrap();
        assert_eq!(oem.header.originator, "NASA/JPL");
        let seg = &oem.segments[0];
        assert_eq!(seg.metadata.object_id, "1996-062A");
        assert_eq!(
            seg.metadata.interpolation,
            Some(Interpolation::Hermite { degree: 7 })
        );
        assert_eq!(seg.states.len(), 4);
        assert_eq!(seg.states[1].1.position_km.y, -878.141);
        let [cov] = &seg.covariances[..] else {
            panic!("{:?}", seg.covariances)
        };
        assert_eq!(cov.ref_frame.as_deref(), Some("EME2000"));
        assert_eq!(cov.matrix[0][0], 3.3313494e-04);
        assert_eq!(cov.matrix[5][4], 1.0088625e-10);
        assert_eq!(cov.matrix[4][5], 1.0088625e-10);
        assert_eq!(Oem::from_kvn(&oem.to_kvn()).unwrap(), oem);
        assert_eq!(
            TimeSystem::Utc.format(seg.states[0].0),
            "1996-12-28T21:29:07.267000000"
        );
    }

    #[test]
    fn oem_round_trip() {
        let states: Vec<_> = (0..5)
            .map(|k| {
                (
                    Epoch::from_utc(2016, 12, 31, 23, 59, 58.0)
                        .advanced_by_seconds(k as f64 * 0.75),
                    StateVector::new(
                        DVec3::new(7000.0 + k as f64, 1.25, -3.5),
                        DVec3::new(0.1, 7.5, -0.000_123_456_789),
                    ),
                )
            })
            .collect();
        for ts in [TimeSystem::Utc, TimeSystem::Tdb, TimeSystem::Gps] {
            let oem = Oem {
                header: Header::default(),
                segments: vec![OemSegment {
                    metadata: OemMetadata {
                        time_system: ts,
                        ..Default::default()
                    },
                    states: states.clone(),
                    covariances: Vec::new(),
                }],
            };
            let kvn = oem.to_kvn();
            let back = Oem::from_kvn(&kvn).unwrap_or_else(|e| panic!("{e}\n{kvn}"));
            for ((e0, s0), (e1, s1)) in states.iter().zip(&back.segments[0].states) {
                assert!(e1.seconds_since(*e0).abs() < 2e-9, "{ts:?}: {e0} vs {e1}");
                assert!((s0.position_km - s1.position_km).length() < 1e-9);
                assert!((s0.velocity_km_s - s1.velocity_km_s).length() < 1e-12);
            }
        }
    }

    #[test]
    fn opm_round_trip() {
        let mut opm = Opm::new(
            Epoch::from_utc(2024, 2, 29, 12, 0, 0.5),
            StateVector::new(
                DVec3::new(6655.9942, -40218.5751, -82.9177),
                DVec3::new(3.11548208, 0.47042605, -0.00101495),
            ),
        );
        opm.mass_kg = Some(1913.0);
        opm.drag_coeff = Some(2.2);
        opm.maneuvers.push(OpmManeuver {
            epoch_ignition: Epoch::from_utc(2024, 3, 1, 9, 0, 34.1),
            duration_s: 132.6,
            delta_mass_kg: -18.418,
            delta_v_km_s: DVec3::new(-0.023, 0.016, -0.0),
        });
        opm.maneuver_frames.push("EME2000".into());
        let back = Opm::from_kvn(&opm.to_kvn()).unwrap();
        assert!(back.epoch.seconds_since(opm.epoch).abs() < 1e-9);
        assert_eq!(back.state, opm.state);
        assert_eq!(back.mass_kg, Some(1913.0));
        assert_eq!(back.maneuver_frames, vec!["EME2000".to_string()]);
        assert_eq!(
            back.maneuvers[0].delta_v_km_s,
            opm.maneuvers[0].delta_v_km_s
        );
        assert!(
            back.maneuvers[0]
                .epoch_ignition
                .seconds_since(opm.maneuvers[0].epoch_ignition)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn day_of_year_epochs() {
        let a = TimeSystem::Utc.parse_epoch("2024-060T00:00:00").unwrap();
        let b = TimeSystem::Utc.parse_epoch("2024-02-29T00:00:00Z").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn aem_round_trips_and_honours_direction_and_order() {
        let e = TimeSystem::Utc.parse_epoch("2026-01-01T00:00:00").unwrap();
        let q = DQuat::from_rotation_z(0.3) * DQuat::from_rotation_x(-0.2);
        let aem = Aem {
            segments: vec![AemSegment {
                metadata: AemMetadata {
                    object_name: "SAT".into(),
                    object_id: "2026-001A".into(),
                    ..AemMetadata::default()
                },
                quaternions: vec![(e, q), (e.advanced_by_seconds(60.0), q)],
            }],
            ..Aem::default()
        };
        let text = aem.to_kvn();
        assert!(text.starts_with("CCSDS_AEM_VERS = 1.0"));
        assert!(text.contains("ATTITUDE_DIR = A2B"));
        let back = Aem::from_kvn(&text).unwrap();
        assert_eq!(back.segments[0].metadata, aem.segments[0].metadata);
        let (e1, q1) = back.segments[0].quaternions[1];
        assert!((e1.seconds_since(e) - 60.0).abs() < 1e-6);
        assert!((q1 - q).length() < 1e-11);

        let flipped = text
            .replace("ATTITUDE_DIR = A2B", "ATTITUDE_DIR = B2A")
            .replace("QUATERNION_TYPE = LAST", "QUATERNION_TYPE = FIRST");
        let data = format!(
            "{} {} {} {} {}",
            "2026-01-01T00:00:00.000", q.w, -q.x, -q.y, -q.z
        );
        let flipped = flipped
            .lines()
            .map(|l| {
                if l.starts_with("2026-01-01T00:00:00") {
                    data.as_str()
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let back = Aem::from_kvn(&flipped).unwrap();
        assert!((back.segments[0].quaternions[0].1 - q).length() < 1e-11);
        assert!(Aem::from_kvn(&text.replace("= QUATERNION\n", "= EULER_ANGLE\n")).is_err());
    }

    /// CCSDS 508.0-B-1 Annex C style sample (abridged, covariance rows trimmed to 6×6).
    const CDM_SAMPLE: &str = "CCSDS_CDM_VERS = 1.0
CREATION_DATE = 2010-03-12T22:31:12.000
ORIGINATOR = JSPOC
MESSAGE_FOR = SATELLITE A
MESSAGE_ID = 201113719185
TCA = 2010-03-13T22:37:52.618
MISS_DISTANCE = 715 [m]
RELATIVE_SPEED = 14762 [m/s]
RELATIVE_POSITION_R = 27.4 [m]
RELATIVE_POSITION_T = -70.2 [m]
RELATIVE_POSITION_N = 711.8 [m]
RELATIVE_VELOCITY_R = -7.2 [m/s]
RELATIVE_VELOCITY_T = -14692.0 [m/s]
RELATIVE_VELOCITY_N = -1437.2 [m/s]
COLLISION_PROBABILITY = 4.835E-05
COLLISION_PROBABILITY_METHOD = FOSTER-1992
OBJECT = OBJECT1
OBJECT_DESIGNATOR = 12345
CATALOG_NAME = SATCAT
OBJECT_NAME = SATELLITE A
INTERNATIONAL_DESIGNATOR = 1997-030E
EPHEMERIS_NAME = EPHEMERIS SATELLITE A
COVARIANCE_METHOD = CALCULATED
MANEUVERABLE = YES
REF_FRAME = EME2000
X = 2570.097065 [km]
Y = 2244.654904 [km]
Z = 6281.497978 [km]
X_DOT = 4.418769571 [km/s]
Y_DOT = 4.833547743 [km/s]
Z_DOT = -3.526774282 [km/s]
CR_R = 4.142E+01 [m**2]
CT_R = -8.579E+00 [m**2]
CT_T = 2.533E+03 [m**2]
CN_R = -2.313E+01 [m**2]
CN_T = 1.336E+01 [m**2]
CN_N = 7.098E+01 [m**2]
CRDOT_R = 2.520E-03 [m**2/s]
CRDOT_T = -5.476E+00 [m**2/s]
CRDOT_N = 8.626E-04 [m**2/s]
CRDOT_RDOT = 5.744E-03 [m**2/s**2]
CTDOT_R = -1.006E-02 [m**2/s]
CTDOT_T = 4.041E-03 [m**2/s]
CTDOT_N = -1.359E-03 [m**2/s]
CTDOT_RDOT = -1.502E-05 [m**2/s**2]
CTDOT_TDOT = 1.049E-05 [m**2/s**2]
CNDOT_R = 1.053E-03 [m**2/s]
CNDOT_T = -3.412E-03 [m**2/s]
CNDOT_N = 1.213E-02 [m**2/s]
CNDOT_RDOT = -3.004E-06 [m**2/s**2]
CNDOT_TDOT = -1.091E-06 [m**2/s**2]
CNDOT_NDOT = 5.529E-05 [m**2/s**2]
OBJECT = OBJECT2
OBJECT_DESIGNATOR = 30337
CATALOG_NAME = SATCAT
OBJECT_NAME = FENGYUN 1C DEB
INTERNATIONAL_DESIGNATOR = 1999-025AA
EPHEMERIS_NAME = NONE
COVARIANCE_METHOD = CALCULATED
MANEUVERABLE = NO
REF_FRAME = EME2000
X = 2569.540800 [km]
Y = 2245.093614 [km]
Z = 6281.599946 [km]
X_DOT = -2.888612500 [km/s]
Y_DOT = -6.007247516 [km/s]
Z_DOT = 3.328770172 [km/s]
CR_R = 1.337E+03 [m**2]
CT_R = -4.806E+04 [m**2]
CT_T = 2.492E+06 [m**2]
CN_R = -3.298E+01 [m**2]
CN_T = -7.5888E+02 [m**2]
CN_N = 7.105E+01 [m**2]
CRDOT_R = 2.591E-03 [m**2/s]
CRDOT_T = -4.152E-02 [m**2/s]
CRDOT_N = -1.784E-06 [m**2/s]
CRDOT_RDOT = 6.886E-05 [m**2/s**2]
CTDOT_R = -1.016E-02 [m**2/s]
CTDOT_T = -1.506E-04 [m**2/s]
CTDOT_N = 1.637E-03 [m**2/s]
CTDOT_RDOT = -2.987E-06 [m**2/s**2]
CTDOT_TDOT = 1.059E-05 [m**2/s**2]
CNDOT_R = 4.400E-03 [m**2/s]
CNDOT_T = 8.482E-03 [m**2/s]
CNDOT_N = 8.633E-03 [m**2/s]
CNDOT_RDOT = -1.903E-06 [m**2/s**2]
CNDOT_TDOT = -4.594E-06 [m**2/s**2]
CNDOT_NDOT = 5.178E-05 [m**2/s**2]
";

    #[test]
    fn cdm_reads_and_round_trips() {
        let cdm = Cdm::from_kvn(CDM_SAMPLE).unwrap();
        assert_eq!(cdm.message_id, "201113719185");
        assert!((cdm.miss_distance_km - 0.715).abs() < 1e-12);
        assert_eq!(cdm.collision_probability, Some(4.835e-5));
        let rel = cdm.relative_position_rtn_km.unwrap();
        assert!((rel - DVec3::new(0.0274, -0.0702, 0.7118)).length() < 1e-12);
        let [a, b] = &cdm.objects;
        assert_eq!(
            (a.designator.as_str(), b.object_name.as_str()),
            ("12345", "FENGYUN 1C DEB")
        );
        assert_eq!(b.state.velocity_km_s.y, -6.007247516);
        assert!((b.covariance_rtn[1][1] - 2.492).abs() < 1e-12);
        assert_eq!(b.covariance_rtn[0][1], b.covariance_rtn[1][0]);
        assert!((a.covariance_rtn[5][5] - 5.529e-11).abs() < 1e-24);
        let back = Cdm::from_kvn(&cdm.to_kvn()).unwrap();
        assert_eq!(back.tca, cdm.tca);
        assert!((back.miss_distance_km - cdm.miss_distance_km).abs() < 1e-15);
        assert_eq!(back.objects[1].state, cdm.objects[1].state);
        for (x, y) in back.objects[0]
            .covariance_rtn
            .iter()
            .flatten()
            .zip(cdm.objects[0].covariance_rtn.iter().flatten())
        {
            assert!((x - y).abs() <= 1e-15 * y.abs().max(1e-30), "{x} vs {y}");
        }
        assert!(
            Cdm::from_kvn(&CDM_SAMPLE.replace("CNDOT_NDOT = 5.178E-05 [m**2/s**2]\n", "")).is_err()
        );
    }
}
