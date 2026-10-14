//! CCSDS Orbit Data Messages (502.0-B-3), KVN: OEM and OPM read/write; Attitude Data
//! Messages (504.0-B-1), KVN: quaternion AEM read/write.
//!
//! Frames and centres are carried as strings; no frame conversion is applied.
//! Covariance blocks are skipped on read.

use glam::{DQuat, DVec3};
use hifitime::TimeScale;

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
        }
        out
    }

    pub fn from_kvn(text: &str) -> Result<Self, CcsdsError> {
        let mut oem = Oem::default();
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
                    in_cov = false;
                    continue;
                }
                _ => {}
            }
            if in_cov {
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
}
