//! OMM, the Orbit Mean-Elements Message, as much of it as a propagator needs.
//!
//! An OMM is [CCSDS 502.0-B-3] written down: the mean Keplerian elements of one
//! object at one epoch, plus the drag term and the conventions that say which
//! theory they belong to. The JSON serialization is what every current
//! catalogue publishes — Celestrak's `gp.php?FORMAT=json` and Space-Track's
//! `/class/gp` both answer with an array of them — and it is the successor to
//! the two-line element set, which is the same numbers in eighty columns with
//! an implied decimal point and no room for a five-digit catalogue number.
//!
//! This module is the boundary between that document and the globe. It reads
//! the records, turns each into an SGP4 propagator, and hands back a
//! [`Catalogue`]; nothing past it knows what an OMM is, and nothing here knows
//! what a globe is.
//!
//! **Why this is not a [`geozero`] reader.** Every other vector format the
//! globe takes — GeoJSON, Mapbox Vector Tiles — is a document full of
//! coordinates, so it goes through `geozero` and lands in the GeoArrow buffers
//! of [`crate::features`] without an intermediate tree. An OMM holds no
//! coordinates at all. It holds an *orbit*: six angles, a mean motion and a drag
//! coefficient, from which a position can be computed at any moment and at no
//! particular one. So there is nothing here for a geometry decoder to decode.
//! The coordinates appear downstream, in [`crate::ephemeris`], where the
//! propagator is evaluated — and from that point on the ephemeris is in the same
//! GeoArrow arrays as every other layer, drawn by the same mesh builders and
//! handed out through the same zero-copy view.
//!
//! **What is tolerated.** A record that will not parse, or whose elements SGP4
//! refuses to initialise from, is dropped and counted; the rest of the document
//! is kept. A catalogue is a bulk product and one bad row in twelve thousand is
//! not a reason to draw nothing. A document that is not an array of records at
//! all is refused whole, because that is a wrong URL rather than a bad row.
//!
//! [CCSDS 502.0-B-3]: https://public.ccsds.org/Pubs/502x0b3e1.pdf
//! [`geozero`]: https://github.com/georust/geozero

use serde::Deserialize;

/// Minutes in a day, which is what turns a mean motion into a period.
const MINUTES_PER_DAY: f64 = 1440.0;

/// One object, ready to be propagated.
///
/// The elements themselves are gone by this point: what is kept is the
/// initialised propagator, the epoch it is measured from as a plain Unix time,
/// and the handful of fields an interface names an object by. Everything a
/// record said that SGP4 does not need and a list does not show — the element
/// set number, the revolution count — is dropped, because holding it would mean
/// holding the whole record for twelve thousand objects to answer a question
/// nothing asks.
///
/// The descriptive fields — the name, the designator, the shape of the orbit —
/// are read only by the catalogue listing an interface pulls, which is the
/// WebAssembly binding; a native build draws the same satellites without ever
/// naming one.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub struct Satellite {
    /// The catalogue number, which is the only identifier that is both stable
    /// and unique. Names repeat (there are dozens of `STARLINK-…` duplicates in
    /// any given week) and international designators are per *launch*, so a
    /// rocket body and its payload share one.
    pub norad_id: u64,
    pub name: String,
    /// Launch year, launch number and piece, as `1998-067A`.
    pub international_designator: Option<String>,
    /// The moment the elements describe, in seconds since the Unix epoch.
    ///
    /// Held here rather than as a `chrono` datetime because propagation wants a
    /// difference in minutes and the globe's clock is a Unix time — going
    /// through a calendar type to subtract two instants would be a conversion
    /// each way, per satellite, per frame.
    pub epoch_unix_seconds: f64,
    /// One revolution, in minutes. Not an SGP4 output: it is `1440 / n` from the
    /// mean motion the record stated, which is what a trail window is measured
    /// in so that half an orbit means half an orbit for a satellite in low Earth
    /// orbit and for one at geostationary height alike.
    pub period_minutes: f64,
    pub inclination_deg: f64,
    pub eccentricity: f64,
    constants: sgp4::Constants,
}

impl Satellite {
    /// Where this object is, in the True Equator Mean Equinox frame SGP4 works
    /// in, at a moment given as a Unix time. Kilometres from the Earth's centre.
    ///
    /// `None` when the propagator diverges, which is what SGP4 does when it is
    /// run far enough from its epoch — a decayed object, or elements months
    /// stale. A satellite that cannot be placed is left out of the drawing
    /// rather than drawn somewhere wrong.
    pub fn position_teme_km(&self, unix_seconds: f64) -> Option<[f64; 3]> {
        let minutes = (unix_seconds - self.epoch_unix_seconds) / 60.0;
        let prediction = self
            .constants
            .propagate(sgp4::MinutesSinceEpoch(minutes))
            .ok()?;
        prediction
            .position
            .iter()
            .all(|component| component.is_finite())
            .then_some(prediction.position)
    }

    /// How far the elements are from a moment, in days, signed.
    ///
    /// SGP4 is a fit rather than a model: it reproduces the catalogue's own
    /// state vector to a kilometre or so near the epoch and drifts from there,
    /// roughly a kilometre a day for something in low Earth orbit. An interface
    /// that shows this is showing how much to trust the dot.
    pub fn age_days(&self, unix_seconds: f64) -> f64 {
        (unix_seconds - self.epoch_unix_seconds) / 86_400.0
    }
}

/// Every object one document held, in the order it held them.
#[derive(Default)]
pub struct Catalogue {
    pub satellites: Vec<Satellite>,
    /// Records that were read but could not be turned into a propagator.
    pub rejected: usize,
}

impl Catalogue {
    pub fn len(&self) -> usize {
        self.satellites.len()
    }

    /// Where an object with this catalogue number is, if the document held one.
    pub fn index_of(&self, norad_id: u64) -> Option<usize> {
        self.satellites
            .iter()
            .position(|satellite| satellite.norad_id == norad_id)
    }
}

/// Why a document could not be read.
#[derive(Debug)]
pub enum OmmError {
    /// Not JSON, or not JSON shaped like a catalogue.
    Malformed(String),
    /// A well-formed document holding nothing that could be propagated, which
    /// is what a query that matched nothing looks like.
    Empty,
}

impl std::fmt::Display for OmmError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(error) => write!(formatter, "not valid OMM JSON: {error}"),
            Self::Empty => write!(formatter, "no propagatable objects in the document"),
        }
    }
}

impl std::error::Error for OmmError {}

/// What a catalogue endpoint may answer with.
///
/// Celestrak hands back a bare array; Space-Track's OData endpoint wraps the
/// same array in `{"value": [...]}`; asking either for one object may hand back
/// the object itself. All three are the same document as far as anything here
/// is concerned, so all three are read.
#[derive(Deserialize)]
#[serde(untagged)]
enum Document {
    Array(Vec<sgp4::Elements>),
    Wrapped {
        #[serde(rename = "value")]
        value: Vec<sgp4::Elements>,
    },
    One(Box<sgp4::Elements>),
}

impl Document {
    fn records(self) -> Vec<sgp4::Elements> {
        match self {
            Self::Array(records) | Self::Wrapped { value: records } => records,
            Self::One(record) => vec![*record],
        }
    }
}

/// Reads a document. See the module docs for what is tolerated.
pub fn parse(text: &str) -> Result<Catalogue, OmmError> {
    let document: Document =
        serde_json::from_str(text).map_err(|error| OmmError::Malformed(error.to_string()))?;

    let mut catalogue = Catalogue::default();
    for elements in document.records() {
        match satellite(&elements) {
            Some(satellite) => catalogue.satellites.push(satellite),
            None => catalogue.rejected += 1,
        }
    }

    if catalogue.satellites.is_empty() {
        return Err(OmmError::Empty);
    }
    Ok(catalogue)
}

/// Initialises one record, or drops it.
fn satellite(elements: &sgp4::Elements) -> Option<Satellite> {
    // A mean motion that is not a positive number cannot describe an orbit, and
    // it would divide by zero on its way to a period. `from_elements` refuses
    // most such records anyway; this is the one it cannot, because a period is
    // the globe's arithmetic rather than SGP4's. Spelled this way round so that
    // a `NaN` — which compares false against everything — is rejected too.
    if !elements.mean_motion.is_finite() || elements.mean_motion <= 0.0 {
        return None;
    }
    let constants = sgp4::Constants::from_elements(elements).ok()?;

    Some(Satellite {
        norad_id: elements.norad_id,
        name: elements
            .object_name
            .clone()
            .unwrap_or_else(|| format!("NORAD {}", elements.norad_id)),
        international_designator: elements.international_designator.clone(),
        // `and_utc` reads the naive timestamp as the UTC the specification says
        // it is; sub-second precision is kept because an epoch stated to the
        // microsecond is stated that way on purpose.
        epoch_unix_seconds: elements.datetime.and_utc().timestamp_micros() as f64 / 1.0e6,
        period_minutes: MINUTES_PER_DAY / elements.mean_motion,
        inclination_deg: elements.inclination,
        eccentricity: elements.eccentricity,
        constants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One record, exactly as Celestrak writes it.
    const ISS: &str = r#"[{
        "OBJECT_NAME": "ISS (ZARYA)",
        "OBJECT_ID": "1998-067A",
        "EPOCH": "2026-09-16T03:25:57.950976",
        "MEAN_MOTION": 15.49133683,
        "ECCENTRICITY": 0.00049077,
        "INCLINATION": 51.631,
        "RA_OF_ASC_NODE": 209.9325,
        "ARG_OF_PERICENTER": 145.256,
        "MEAN_ANOMALY": 214.875,
        "EPHEMERIS_TYPE": 0,
        "CLASSIFICATION_TYPE": "U",
        "NORAD_CAT_ID": 25544,
        "ELEMENT_SET_NO": 999,
        "REV_AT_EPOCH": 58585,
        "BSTAR": 0.00013461457,
        "MEAN_MOTION_DOT": 7.008e-5,
        "MEAN_MOTION_DDOT": 0
    }]"#;

    fn iss() -> Satellite {
        parse(ISS)
            .expect("a catalogue")
            .satellites
            .pop()
            .expect("one satellite")
    }

    #[test]
    fn a_record_becomes_a_propagator() {
        let satellite = iss();
        assert_eq!(satellite.norad_id, 25544);
        assert_eq!(satellite.name, "ISS (ZARYA)");
        assert_eq!(
            satellite.international_designator.as_deref(),
            Some("1998-067A")
        );
        // Fifteen and a half revolutions a day is a little over ninety minutes.
        assert!(
            (satellite.period_minutes - 92.95).abs() < 0.1,
            "{}",
            satellite.period_minutes
        );
    }

    #[test]
    fn the_epoch_reads_as_the_utc_the_specification_says_it_is() {
        // 2026-09-16T03:25:57.950976Z, checked against the same instant built
        // from the date arithmetic rather than from the parser under test.
        let satellite = iss();
        let expected = 1_789_529_157.950_976;
        assert!(
            (satellite.epoch_unix_seconds - expected).abs() < 1.0e-3,
            "{}",
            satellite.epoch_unix_seconds
        );
    }

    #[test]
    fn propagating_at_the_epoch_puts_it_in_low_earth_orbit() {
        let satellite = iss();
        let position = satellite
            .position_teme_km(satellite.epoch_unix_seconds)
            .expect("a position");
        let radius = position
            .iter()
            .map(|component| component * component)
            .sum::<f64>()
            .sqrt();
        // The station orbits around 420 km up, so 6371 + 420 give or take.
        assert!((6700.0..6850.0).contains(&radius), "{radius}");
    }

    #[test]
    fn it_moves_about_as_fast_as_a_satellite_moves() {
        let satellite = iss();
        let at = |seconds| satellite.position_teme_km(seconds).expect("a position");
        let (before, after) = (
            at(satellite.epoch_unix_seconds),
            at(satellite.epoch_unix_seconds + 60.0),
        );
        let travelled = (0..3)
            .map(|axis| (after[axis] - before[axis]).powi(2))
            .sum::<f64>()
            .sqrt();
        // A minute at roughly 7.66 km/s, minus a little for the chord.
        assert!((440.0..465.0).contains(&travelled), "{travelled}");
    }

    #[test]
    fn a_whole_orbit_comes_back_to_where_it_started() {
        let satellite = iss();
        let at = |minutes: f64| {
            satellite
                .position_teme_km(satellite.epoch_unix_seconds + minutes * 60.0)
                .expect("a position")
        };
        let (start, around) = (at(0.0), at(satellite.period_minutes));
        let apart = (0..3)
            .map(|axis| (around[axis] - start[axis]).powi(2))
            .sum::<f64>()
            .sqrt();
        // Not exactly: the period here is the Kozai mean motion's, and SGP4's
        // own is the Brouwer one, so a revolution lands tens of kilometres from
        // where it left — a fraction of a degree along the track.
        assert!(apart < 120.0, "{apart}");
    }

    #[test]
    fn the_three_shapes_a_catalogue_endpoint_answers_with_all_read() {
        let one = ISS.trim_start_matches('[').trim_end_matches(']');
        let wrapped = format!(r#"{{"value": {ISS}}}"#);
        for document in [ISS, one, &wrapped] {
            assert_eq!(parse(document).expect("a catalogue").len(), 1);
        }
    }

    #[test]
    fn a_bad_row_is_dropped_rather_than_taking_the_document_with_it() {
        let broken = ISS.replace("15.49133683", "0");
        let both = format!("[{}, {}]", inner(ISS), inner(&broken));
        let catalogue = parse(&both).expect("a catalogue");
        assert_eq!(catalogue.len(), 1);
        assert_eq!(catalogue.rejected, 1);
        assert_eq!(catalogue.index_of(25544), Some(0));
        assert_eq!(catalogue.index_of(1), None);
    }

    #[test]
    fn a_document_that_is_not_a_catalogue_is_refused_whole() {
        assert!(matches!(parse("{}"), Err(OmmError::Malformed(_))));
        assert!(matches!(parse("not json"), Err(OmmError::Malformed(_))));
        assert!(matches!(parse("[]"), Err(OmmError::Empty)));
    }

    /// The one record out of the array literal above, for building documents
    /// that hold more than one.
    fn inner(document: &str) -> String {
        document
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string()
    }
}
