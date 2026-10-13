//! NAIF SPK writer: little-endian DAF, ND = 2, NI = 6; segment types 9 and 13.
//!
//! Layout: file record; summary/name record pairs (25 summaries each, chained by
//! NEXT/PREV); then segment data, contiguous from the next record. No comment area.
//!
//! Type 9 (Lagrange) and type 13 (Hermite), unequal steps, per SPK Required Reading:
//! `N` states (x, y, z, vx, vy, vz; km, km/s), `N` epochs (ET: TDB s past J2000, SPICE model),
//! `floor((N - 1) / 100)` directory epochs (every 100th), then type 9 degree
//! (`window - 1`) or type 13 `window - 1`, then `N`.

use crate::frame::StateVector;
use crate::interpolate::{Interpolation, SampledEphemeris};
use crate::time::Epoch;

/// NAIF frame code for J2000/ICRF.
pub const J2000_FRAME: i32 = 1;
/// SPICE `MAXDEG` for types 9 and 13 (`spkw09`/`spkw13`).
pub const MAX_DEGREE: usize = 15;

const RECORD_BYTES: usize = 1024;
const RECORD_WORDS: usize = RECORD_BYTES / 8;
const ND: usize = 2;
const NI: usize = 6;
/// Summary size in doubles: `ND + (NI + 1) / 2`.
const SUMMARY_WORDS: usize = ND + NI.div_ceil(2);
/// Summaries per summary record, after NEXT, PREV, NSUM.
const SUMMARIES_PER_RECORD: usize = (RECORD_WORDS - 3) / SUMMARY_WORDS;
/// Segment name length: `8 * SUMMARY_WORDS`.
pub const MAX_NAME_LEN: usize = 8 * SUMMARY_WORDS;
pub const MAX_INTERNAL_NAME_LEN: usize = 60;
/// FTP corruption check, file record bytes 699..727.
const FTPSTR: &[u8; 28] = b"FTPSTR:\r:\n:\r\n:\r\x00:\x81:\x10\xce:ENDFTP";
const DIRECTORY_STRIDE: usize = 100;

/// Interpolation and window size in states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpkSegmentKind {
    /// Type 13. Degree `2 * window - 1`; `1..=(MAX_DEGREE + 1) / 2`.
    Hermite { window: usize },
    /// Type 9. Degree `window - 1`; `2..=MAX_DEGREE + 1`.
    Lagrange { window: usize },
}

impl SpkSegmentKind {
    pub fn data_type(self) -> i32 {
        match self {
            Self::Hermite { .. } => 13,
            Self::Lagrange { .. } => 9,
        }
    }

    pub fn window(self) -> usize {
        match self {
            Self::Hermite { window } | Self::Lagrange { window } => window,
        }
    }

    /// Hermite{d} → `(d + 1) / 2` states; Lagrange{d} → `d + 1`. Matches [`SampledEphemeris`].
    pub fn from_interpolation(method: Interpolation) -> Self {
        match method {
            Interpolation::Hermite { degree } => Self::Hermite {
                window: degree.div_ceil(2).max(1),
            },
            Interpolation::Lagrange { degree } => Self::Lagrange { window: degree + 1 },
        }
    }

    fn degree(self) -> usize {
        match self {
            Self::Hermite { window } => (2 * window).saturating_sub(1),
            Self::Lagrange { window } => window.saturating_sub(1),
        }
    }
}

/// One SPK segment: `target` relative to `center` in `frame` (NAIF codes).
#[derive(Debug, Clone, PartialEq)]
pub struct SpkSegment {
    pub target: i32,
    pub center: i32,
    pub frame: i32,
    pub kind: SpkSegmentKind,
    /// Segment identifier, ≤ [`MAX_NAME_LEN`] printable ASCII.
    pub name: String,
    /// Strictly increasing.
    pub states: Vec<(Epoch, StateVector)>,
}

impl SpkSegment {
    /// Type and window from `eph.method`; J2000 frame.
    pub fn from_sampled(
        eph: &SampledEphemeris,
        target: i32,
        center: i32,
        name: impl Into<String>,
    ) -> Self {
        Self {
            target,
            center,
            frame: J2000_FRAME,
            kind: SpkSegmentKind::from_interpolation(eph.method),
            name: name.into(),
            states: eph.samples().collect(),
        }
    }

    fn validate(&self, index: usize) -> Result<Vec<f64>, SpkError> {
        let err = |reason| SpkError::Segment { index, reason };
        check_name(&self.name, MAX_NAME_LEN).map_err(err)?;
        let degree = self.kind.degree();
        let ok = match self.kind {
            SpkSegmentKind::Hermite { window } => window >= 1 && degree <= MAX_DEGREE,
            SpkSegmentKind::Lagrange { .. } => (1..=MAX_DEGREE).contains(&degree),
        };
        if !ok {
            return Err(err(SegmentFault::Window(self.kind.window())));
        }
        if self.states.len() < self.kind.window() || self.states.len() < 2 {
            return Err(err(SegmentFault::TooFewStates(self.states.len())));
        }
        let ets: Vec<f64> = self.states.iter().map(|(e, _)| et_seconds(*e)).collect();
        for (i, (et, (_, s))) in ets.iter().zip(&self.states).enumerate() {
            let finite = et.is_finite() && s.position_km.is_finite() && s.velocity_km_s.is_finite();
            if !finite {
                return Err(err(SegmentFault::NonFinite(i)));
            }
            if i > 0 && *et <= ets[i - 1] {
                return Err(err(SegmentFault::EpochsNotIncreasing(i)));
            }
        }
        Ok(ets)
    }

    /// Segment data array in doubles.
    fn data(&self, ets: &[f64]) -> Vec<f64> {
        let n = self.states.len();
        let mut d = Vec::with_capacity(7 * n + n / DIRECTORY_STRIDE + 2);
        for (_, s) in &self.states {
            d.extend(s.position_km.to_array());
            d.extend(s.velocity_km_s.to_array());
        }
        d.extend_from_slice(ets);
        d.extend((1..=(n - 1) / DIRECTORY_STRIDE).map(|k| ets[k * DIRECTORY_STRIDE - 1]));
        d.push((self.kind.window() - 1) as f64);
        d.push(n as f64);
        d
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SegmentFault {
    NameTooLong(usize),
    NameNotPrintable,
    NameEmpty,
    Window(usize),
    TooFewStates(usize),
    NonFinite(usize),
    EpochsNotIncreasing(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpkError {
    NoSegments,
    InternalName(SegmentFault),
    Segment {
        index: usize,
        reason: SegmentFault,
    },
    /// DAF addresses are 32-bit.
    TooLarge,
}

impl std::fmt::Display for SegmentFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameTooLong(n) => write!(f, "name is {n} chars"),
            Self::NameNotPrintable => write!(f, "name not printable ASCII"),
            Self::NameEmpty => write!(f, "name empty"),
            Self::Window(w) => write!(f, "window {w} outside NAIF limits"),
            Self::TooFewStates(n) => write!(f, "{n} states, fewer than window"),
            Self::NonFinite(i) => write!(f, "state {i} not finite"),
            Self::EpochsNotIncreasing(i) => write!(f, "epoch {i} not strictly increasing"),
        }
    }
}

impl std::fmt::Display for SpkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSegments => write!(f, "no segments"),
            Self::InternalName(r) => write!(f, "internal file name: {r}"),
            Self::Segment { index, reason } => write!(f, "segment {index}: {reason}"),
            Self::TooLarge => write!(f, "file exceeds DAF address range"),
        }
    }
}

impl std::error::Error for SpkError {}

/// SPICE ET (hifitime's ET model, as anise reads it). Differs from
/// [`Epoch::tdb_seconds`] by up to tens of ns.
pub fn et_seconds(epoch: Epoch) -> f64 {
    epoch.as_hifitime().to_et_seconds()
}

fn check_name(name: &str, max: usize) -> Result<(), SegmentFault> {
    if name.trim().is_empty() {
        return Err(SegmentFault::NameEmpty);
    }
    if !name.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(SegmentFault::NameNotPrintable);
    }
    if name.len() > max {
        return Err(SegmentFault::NameTooLong(name.len()));
    }
    Ok(())
}

fn padded(s: &str, len: usize) -> Vec<u8> {
    let mut b = s.as_bytes().to_vec();
    b.resize(len, b' ');
    b
}

/// Serialises `segments` as an SPK, in order. Segments sharing a target should be
/// time-ordered: SPICE searches last-loaded-first, anise requires non-overlap.
pub fn write_spk(segments: &[SpkSegment], internal_name: &str) -> Result<Vec<u8>, SpkError> {
    if segments.is_empty() {
        return Err(SpkError::NoSegments);
    }
    check_name(internal_name, MAX_INTERNAL_NAME_LEN).map_err(SpkError::InternalName)?;
    let data: Vec<(Vec<f64>, f64, f64)> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let ets = s.validate(i)?;
            Ok((s.data(&ets), ets[0], ets[ets.len() - 1]))
        })
        .collect::<Result<_, SpkError>>()?;

    // Records are 1-based; word addresses are 1-based and record r starts at (r - 1) * 128 + 1.
    let blocks = segments.len().div_ceil(SUMMARIES_PER_RECORD);
    let summary_record = |b: usize| 2 + 2 * b;
    let first_data_record = summary_record(blocks);
    let total_words: usize = data.iter().map(|d| d.0.len()).sum();
    let data_records = total_words.div_ceil(RECORD_WORDS);
    let free = (first_data_record - 1) * RECORD_WORDS + total_words + 1;
    if free > i32::MAX as usize {
        return Err(SpkError::TooLarge);
    }

    let mut out = vec![0u8; (first_data_record - 1 + data_records) * RECORD_BYTES];

    // File record.
    let fr = &mut out[..RECORD_BYTES];
    fr[0..8].copy_from_slice(b"DAF/SPK ");
    fr[8..12].copy_from_slice(&(ND as i32).to_le_bytes());
    fr[12..16].copy_from_slice(&(NI as i32).to_le_bytes());
    fr[16..76].copy_from_slice(&padded(internal_name, MAX_INTERNAL_NAME_LEN));
    fr[76..80].copy_from_slice(&(summary_record(0) as i32).to_le_bytes());
    fr[80..84].copy_from_slice(&(summary_record(blocks - 1) as i32).to_le_bytes());
    fr[84..88].copy_from_slice(&(free as i32).to_le_bytes());
    fr[88..96].copy_from_slice(b"LTL-IEEE");
    fr[699..727].copy_from_slice(FTPSTR);

    // Data, then summaries pointing at it.
    let mut addr = (first_data_record - 1) * RECORD_WORDS + 1;
    for (b, chunk) in segments.chunks(SUMMARIES_PER_RECORD).enumerate() {
        let rec = summary_record(b);
        let next = if b + 1 < blocks {
            summary_record(b + 1)
        } else {
            0
        };
        let prev = if b > 0 { summary_record(b - 1) } else { 0 };
        let words = [next as f64, prev as f64, chunk.len() as f64];
        let mut summaries = Vec::with_capacity(chunk.len() * SUMMARY_WORDS * 8);
        let mut names = Vec::with_capacity(RECORD_BYTES);
        for (j, seg) in chunk.iter().enumerate() {
            let (d, start, end) = &data[b * SUMMARIES_PER_RECORD + j];
            let (begin, last) = (addr, addr + d.len() - 1);
            let at = (begin - 1) * 8;
            for (k, w) in d.iter().enumerate() {
                out[at + 8 * k..at + 8 * k + 8].copy_from_slice(&w.to_le_bytes());
            }
            addr = last + 1;
            summaries.extend(start.to_le_bytes());
            summaries.extend(end.to_le_bytes());
            for i in [
                seg.target,
                seg.center,
                seg.frame,
                seg.kind.data_type(),
                begin as i32,
                last as i32,
            ] {
                summaries.extend(i.to_le_bytes());
            }
            names.extend(padded(&seg.name, MAX_NAME_LEN));
        }
        let at = (rec - 1) * RECORD_BYTES;
        for (k, w) in words.iter().enumerate() {
            out[at + 8 * k..at + 8 * k + 8].copy_from_slice(&w.to_le_bytes());
        }
        out[at + 24..at + 24 + summaries.len()].copy_from_slice(&summaries);
        out[at + RECORD_BYTES..at + RECORD_BYTES + names.len()].copy_from_slice(&names);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;

    use super::*;
    use crate::frame::Ephemeris;
    use crate::orbit::OrbitalElements;
    use crate::spice::anise::prelude::Almanac;
    use crate::spice::{KernelEphemeris, load_kernel_bytes, naif};

    const GM: f64 = 398_600.441_8;
    const TARGET: i32 = -1000;

    fn orbit() -> OrbitalElements {
        OrbitalElements::from_state(
            StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 6.5, 4.0)),
            GM,
            0.0,
        )
    }

    /// `n` samples, `step` s apart, from `t0` s past J2000 (SPICE ET).
    fn samples(t0: f64, step: f64, n: usize) -> Vec<(Epoch, StateVector)> {
        let el = orbit();
        (0..n)
            .map(|k| {
                let t = t0 + k as f64 * step;
                (from_et(t), el.state_at(GM, t))
            })
            .collect()
    }

    fn from_et(t: f64) -> Epoch {
        Epoch::from_hifitime(hifitime::Epoch::from_et_seconds(t))
    }

    fn load(bytes: &[u8]) -> KernelEphemeris {
        let almanac = load_kernel_bytes(Almanac::default(), bytes).expect("anise rejects SPK");
        KernelEphemeris::new(Arc::new(almanac), TARGET, naif::EARTH)
    }

    fn le_i32(b: &[u8], at: usize) -> i32 {
        i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    fn le_f64(b: &[u8], word: usize) -> f64 {
        let at = (word - 1) * 8;
        f64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    #[test]
    fn file_record_layout() {
        let seg = SpkSegment {
            target: TARGET,
            center: naif::EARTH,
            frame: J2000_FRAME,
            kind: SpkSegmentKind::Lagrange { window: 4 },
            name: "TEST".into(),
            states: samples(0.0, 60.0, 201),
        };
        let b = write_spk(&[seg], "terramenta").unwrap();
        assert_eq!(b.len() % RECORD_BYTES, 0);
        assert_eq!(&b[0..8], b"DAF/SPK ");
        assert_eq!((le_i32(&b, 8), le_i32(&b, 12)), (2, 6));
        assert_eq!(&b[16..26], b"terramenta");
        assert_eq!((le_i32(&b, 76), le_i32(&b, 80)), (2, 2));
        assert_eq!(&b[88..96], b"LTL-IEEE");
        assert_eq!(&b[699..727], FTPSTR);
        // Summary: NEXT, PREV, NSUM; DC; IC.
        let s = RECORD_WORDS;
        assert_eq!(
            [le_f64(&b, s + 1), le_f64(&b, s + 2), le_f64(&b, s + 3)],
            [0.0, 0.0, 1.0]
        );
        let ic = RECORD_BYTES + 24 + 16;
        let (begin, end) = (le_i32(&b, ic + 16) as usize, le_i32(&b, ic + 20) as usize);
        assert_eq!(le_i32(&b, ic + 12), 9);
        assert_eq!(begin, 3 * RECORD_WORDS + 1);
        // 201 states: 2 directory epochs (100th, 200th).
        assert_eq!(end - begin + 1, 7 * 201 + 2 + 2);
        assert_eq!(le_i32(&b, 84) as usize, end + 1);
        assert_eq!(le_f64(&b, end - 1), 3.0);
        assert_eq!(le_f64(&b, end), 201.0);
        assert!((le_f64(&b, end - 3) - 99.0 * 60.0).abs() < 1e-6);
        assert!((le_f64(&b, end - 2) - 199.0 * 60.0).abs() < 1e-6);
        assert_eq!(&b[2 * RECORD_BYTES..2 * RECORD_BYTES + 5], b"TEST ");
    }

    #[test]
    fn directory_count() {
        for (n, dirs) in [(100, 0), (101, 1), (200, 1), (201, 2)] {
            let seg = SpkSegment {
                target: TARGET,
                center: naif::EARTH,
                frame: J2000_FRAME,
                kind: SpkSegmentKind::Hermite { window: 4 },
                name: "D".into(),
                states: samples(0.0, 10.0, n),
            };
            let ets = seg.validate(0).unwrap();
            assert_eq!(seg.data(&ets).len(), 7 * n + dirs + 2, "n={n}");
        }
    }

    /// Anise query vs Kepler truth and [`SampledEphemeris`] on the same samples.
    fn check_against_source(method: Interpolation, tol_between_km: f64) {
        let src = samples(1_000.0, 60.0, 250);
        let eph = SampledEphemeris::new(src.clone(), method);
        let seg = SpkSegment::from_sampled(&eph, TARGET, naif::EARTH, "KEPLER");
        let k = load(&write_spk(&[seg], "kepler").unwrap());
        // Node epochs as written (`samples()` rebuilds them from an origin; ns rounding).
        for (e, s) in eph.samples() {
            let q = k.try_state_at(e).expect("in coverage");
            assert!(
                (q.position_km - s.position_km).length() <= 1e-9,
                "{method:?}"
            );
            assert!((q.velocity_km_s - s.velocity_km_s).length() <= 1e-12);
        }
        let el = orbit();
        for t in [1_030.0, 2_345.6, 7_777.0, 13_210.0, 15_910.0] {
            let e = from_et(t);
            let q = k.try_state_at(e).unwrap();
            let truth = el.state_at(GM, t);
            let local = eph.state_at(e);
            assert!(
                (q.position_km - truth.position_km).length() <= tol_between_km,
                "{method:?} t={t}"
            );
            assert!(
                (q.position_km - local.position_km).length() <= tol_between_km,
                "{method:?} t={t}"
            );
            assert!((q.velocity_km_s - truth.velocity_km_s).length() <= 1e-8);
        }
    }

    #[test]
    fn hermite_type13_round_trips_through_anise() {
        check_against_source(Interpolation::Hermite { degree: 7 }, 1e-6);
    }

    #[test]
    fn lagrange_type9_round_trips_through_anise() {
        check_against_source(Interpolation::Lagrange { degree: 8 }, 1e-6);
    }

    #[test]
    fn chained_summary_records() {
        let n_segs = 2 * SUMMARIES_PER_RECORD + 3;
        let per = 12;
        let step = 60.0;
        let src = samples(0.0, step, n_segs * (per - 1) + 1);
        let segs: Vec<_> = (0..n_segs)
            .map(|i| SpkSegment {
                target: TARGET,
                center: naif::EARTH,
                frame: J2000_FRAME,
                kind: if i % 2 == 0 {
                    SpkSegmentKind::Hermite { window: 4 }
                } else {
                    SpkSegmentKind::Lagrange { window: 8 }
                },
                name: format!("SEG {i}"),
                // Adjacent segments share a boundary node.
                states: src[i * (per - 1)..=(i + 1) * (per - 1)].to_vec(),
            })
            .collect();
        let b = write_spk(&segs, "chained").unwrap();
        assert_eq!((le_i32(&b, 76), le_i32(&b, 80)), (2, 6));
        assert_eq!(le_f64(&b, RECORD_WORDS + 1), 4.0);
        assert_eq!(le_f64(&b, 5 * RECORD_WORDS + 2), 4.0);
        assert_eq!(le_f64(&b, 5 * RECORD_WORDS + 3), 3.0);
        let k = load(&b);
        let el = orbit();
        for (e, s) in &src {
            let q = k.try_state_at(*e).expect("in coverage");
            assert!((q.position_km - s.position_km).length() <= 1e-9);
        }
        for i in 0..src.len() - 1 {
            let t = (i as f64 + 0.37) * step;
            let q = k.try_state_at(from_et(t)).unwrap();
            let err = (q.position_km - el.state_at(GM, t).position_km).length();
            assert!(err <= 1e-3, "t={t}: {err} km");
        }
    }

    #[test]
    fn rejects_invalid_segments() {
        let ok = SpkSegment {
            target: TARGET,
            center: naif::EARTH,
            frame: J2000_FRAME,
            kind: SpkSegmentKind::Hermite { window: 4 },
            name: "OK".into(),
            states: samples(0.0, 60.0, 10),
        };
        let fault = |seg: SpkSegment| match write_spk(&[seg], "x") {
            Err(SpkError::Segment { reason, .. }) => reason,
            r => panic!("{r:?}"),
        };
        let mut s = ok.clone();
        s.name = "X".repeat(41);
        assert_eq!(fault(s), SegmentFault::NameTooLong(41));
        let mut s = ok.clone();
        s.kind = SpkSegmentKind::Hermite { window: 9 };
        assert_eq!(fault(s), SegmentFault::Window(9));
        let mut s = ok.clone();
        s.kind = SpkSegmentKind::Lagrange { window: 1 };
        assert_eq!(fault(s), SegmentFault::Window(1));
        let mut s = ok.clone();
        s.kind = SpkSegmentKind::Lagrange { window: 11 };
        assert_eq!(fault(s), SegmentFault::TooFewStates(10));
        let mut s = ok.clone();
        s.states.swap(3, 4);
        assert_eq!(fault(s), SegmentFault::EpochsNotIncreasing(4));
        assert_eq!(write_spk(&[], "x"), Err(SpkError::NoSegments));
        assert!(matches!(
            write_spk(&[ok], &"y".repeat(61)),
            Err(SpkError::InternalName(SegmentFault::NameTooLong(61)))
        ));
    }
}
