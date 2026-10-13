//! Reference-value validation.
//!
//! - Frames: Vallado, *Fundamentals of Astrodynamics and Applications*, 4th ed., Ex. 3-15;
//!   Vallado et al., AIAA 2006-6753 (TEME).
//! - Ephemerides: JPL Horizons (DE441), geometric, ICRF, TDB. Requires `data/de440s.bsp`
//!   (`scripts/fetch-data.sh`); skipped if absent.

#![allow(clippy::excessive_precision)] // reference values verbatim

use std::path::PathBuf;
use std::sync::Arc;

use glam::DVec3;
use terramenta_solare::earth::{self, EarthModel};
use terramenta_solare::spice::{self, KernelEphemeris, naif};
use terramenta_solare::{Eop, Ephemeris, Epoch, StateVector, planets, sun};

const AS: f64 = std::f64::consts::PI / (180.0 * 3600.0);

fn v(x: f64, y: f64, z: f64) -> DVec3 {
    DVec3::new(x, y, z)
}

fn assert_close(label: &str, got: DVec3, want: DVec3, tol: f64) {
    let err = (got - want).length();
    assert!(
        err < tol,
        "{label}: |Δ| = {err:e} (tol {tol:e})\n  got  {got:?}\n  want {want:?}"
    );
}

// --- Vallado Ex. 3-15 -------------------------------------------------------

fn ex315_epoch() -> Epoch {
    Epoch::from_utc(2004, 4, 6, 7, 51, 28.386009)
}

fn ex315_eop() -> Eop {
    Eop {
        xp: -0.140682 * AS,
        yp: 0.333309 * AS,
        ut1_minus_utc: -0.4399619,
        lod: 0.0015563,
        dpsi: -0.052195 * AS,
        deps: -0.003875 * AS,
        dx: -0.000205 * AS,
        dy: -0.000136 * AS,
    }
}

fn ex315_itrf() -> StateVector {
    StateVector::new(
        v(-1033.4793830, 7901.2952754, 6380.3565958),
        v(-3.225636520, -2.872451450, 5.531924446),
    )
}

#[test]
fn vallado_3_15_itrf_to_gcrf_fk5() {
    let t = earth::gcrf_to_itrf(ex315_epoch(), &ex315_eop(), EarthModel::Iau76Fk5);
    let gcrf = t.from_itrf(ex315_itrf());
    assert_close(
        "r_gcrf",
        gcrf.position_km,
        v(5102.508958, 6123.011401, 6378.136928),
        2e-5,
    );
    assert_close(
        "v_gcrf",
        gcrf.velocity_km_s,
        v(-4.743220157, 0.790536497, 5.533755727),
        2e-8,
    );
    let back = t.to_itrf(gcrf);
    assert_close("r_itrf", back.position_km, ex315_itrf().position_km, 1e-9);
}

#[test]
fn vallado_3_15_itrf_to_gcrf_cio() {
    let t = earth::gcrf_to_itrf(ex315_epoch(), &ex315_eop(), EarthModel::Iau2006Cio);
    let gcrf = t.from_itrf(ex315_itrf());
    // Same physical frame; models agree to cm level with matched EOP corrections.
    assert_close(
        "r_gcrf",
        gcrf.position_km,
        v(5102.508958, 6123.011401, 6378.136928),
        5e-4,
    );
    assert_close(
        "v_gcrf",
        gcrf.velocity_km_s,
        v(-4.743220157, 0.790536497, 5.533755727),
        5e-7,
    );
}

#[test]
fn vallado_3_15_pef() {
    let t = earth::gcrf_to_itrf(ex315_epoch(), &ex315_eop(), EarthModel::Iau76Fk5);
    let r_pef = t.polar_motion.transpose() * ex315_itrf().position_km;
    assert_close(
        "r_pef",
        r_pef,
        v(-1033.4750313, 7901.3055856, 6380.3445328),
        1e-6,
    );
}

#[test]
fn vallado_teme() {
    let (epoch, eop) = (ex315_epoch(), ex315_eop());
    let teme = StateVector::new(
        v(5094.18016210, 6127.64465950, 6380.34453270),
        v(-4.746131487, 0.785818041, 5.531931288),
    );
    let itrf = earth::teme_to_itrf(epoch, &eop).to_itrf(teme);
    assert_close("r_itrf", itrf.position_km, ex315_itrf().position_km, 2e-5);
    assert_close(
        "v_itrf",
        itrf.velocity_km_s,
        ex315_itrf().velocity_km_s,
        2e-8,
    );

    let gcrf = earth::teme_to_gcrf(epoch, &eop) * teme.position_km;
    assert_close(
        "r_gcrf",
        gcrf,
        v(5102.508958, 6123.011401, 6378.136928),
        2e-5,
    );
}

// --- DE kernels vs Horizons -------------------------------------------------

fn kernels() -> Option<Arc<spice::anise::prelude::Almanac>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/de440s.bsp");
    if !path.exists() {
        eprintln!(
            "skipped: {} missing (run scripts/fetch-data.sh)",
            path.display()
        );
        return None;
    }
    let almanac = spice::load_kernel_file(Default::default(), path.to_str()?).expect("load de440s");
    Some(Arc::new(almanac))
}

struct HorizonsCase {
    target: i32,
    center: i32,
    jd_tdb: f64,
    r: [f64; 3],
    v: [f64; 3],
}

#[rustfmt::skip]
const HORIZONS: &[HorizonsCase] = &[
    HorizonsCase { target: 399, center: 0, jd_tdb: 2451545.0, r: [-2.756674048281145E+07, 1.323613811535491E+08, 5.741865328625385E+07], v: [-2.978494749851088E+01, -5.029753814928081E+00, -2.180645069035755E+00] },
    HorizonsCase { target: 399, center: 0, jd_tdb: 2461326.5, r: [1.407203336868814E+08, 4.457533855080359E+07, 1.933720750159129E+07], v: [-1.032881355088585E+01, 2.570403428407412E+01, 1.114228929497191E+01] },
    HorizonsCase { target: 301, center: 399, jd_tdb: 2451545.0, r: [-2.916083841877129E+05, -2.667168338540655E+05, -7.610248730658794E+04], v: [6.435313889889519E-01, -6.660876829565195E-01, -3.013257046610932E-01] },
    HorizonsCase { target: 301, center: 399, jd_tdb: 2461326.5, r: [-2.705268077008485E+05, -2.503278167644637E+05, -1.461594890121435E+05], v: [6.969303933288429E-01, -6.422863597659697E-01, -3.012501721109779E-01] },
    HorizonsCase { target: 4, center: 0, jd_tdb: 2451545.0, r: [2.069804338364514E+08, -1.864170114371795E+05, -5.667227497526179E+06], v: [1.171985008531777E+00, 2.390670819417074E+01, 1.093392063330765E+01] },
    HorizonsCase { target: 4, center: 0, jd_tdb: 2461326.5, r: [-5.284526682675231E+06, 2.130526208781067E+08, 9.789302671751496E+07], v: [-2.329542796096434E+01, 1.169026324298289E+00, 1.164423479410951E+00] },
    HorizonsCase { target: 10, center: 0, jd_tdb: 2451545.0, r: [-1.067706805380953E+06, -3.960361847959462E+05, -1.380651842868809E+05], v: [9.312571926520472E-03, -1.170150612817771E-02, -5.251266205200356E-03] },
    HorizonsCase { target: 10, center: 0, jd_tdb: 2461326.5, r: [-1.754663705917482E+05, -7.068690536201003E+05, -2.912833148731843E+05], v: [1.042278388300828E-02, 4.219069111825785E-03, 1.585204130270411E-03] },
    HorizonsCase { target: 5, center: 0, jd_tdb: 2451545.0, r: [5.974998767925479E+08, 4.089903139317586E+08, 1.607562819387201E+08], v: [-7.900525116640771E+00, 1.017179630923791E+01, 4.552467787262923E+00] },
    HorizonsCase { target: 5, center: 0, jd_tdb: 2461326.5, r: [-5.326336590688107E+08, 5.364632992947350E+08, 2.429155588328179E+08], v: [-9.846449709421277E+00, -7.578760499106322E+00, -3.008742615604297E+00] },
];

fn tdb(jd: f64) -> Epoch {
    Epoch::from_tdb_seconds_j2000((jd - 2451545.0) * 86_400.0)
}

#[test]
fn de440s_matches_horizons() {
    let Some(almanac) = kernels() else { return };
    for c in HORIZONS {
        let s = KernelEphemeris::new(almanac.clone(), c.target, c.center).state_at(tdb(c.jd_tdb));
        let label = format!("{} wrt {} @ JD {}", c.target, c.center, c.jd_tdb);
        // DE440 vs DE441: Moon ≤ 2.3 m, others ≤ 5 cm (2000, 2026).
        assert_close(&label, s.position_km, DVec3::from(c.r), 5e-3);
        assert_close(&label, s.velocity_km_s, DVec3::from(c.v), 1e-8);
    }
}

#[test]
fn kernel_tree_composes_moon_from_ssb() {
    let Some(almanac) = kernels() else { return };
    let tree = spice::solar_system_from_kernels(almanac.clone());
    let (moon, root) = (tree.find("Moon").unwrap(), tree.root());
    let epoch = tdb(2461326.5);
    let via_tree = tree.state_of_relative_to(moon, root, epoch);
    let direct = KernelEphemeris::new(almanac, naif::MOON, naif::SSB).state_at(epoch);
    assert_close("moon/ssb", via_tree.position_km, direct.position_km, 1e-6);
}

/// Low-precision series vs DE440, 1900–2050. Measured max |Δr|: Earth 1.3e4 km, Mars 1.0e5 km, Sun–SSB 4.3e5 km.
#[test]
fn low_precision_error_bounds() {
    let Some(almanac) = kernels() else { return };
    let k = |t| KernelEphemeris::new(almanac.clone(), t, naif::SSB);
    let (earth, mars, sun_k) = (k(naif::EARTH), k(naif::MARS_BARYCENTER), k(naif::SUN));
    let mut worst = [0.0_f64; 3];
    for year in (1900..=2050).step_by(5) {
        let e = Epoch::from_utc(year, 6, 1, 0, 0, 0.0);
        let (s_lo, s_de) = (
            sun::Sun.state_at(e).position_km,
            sun_k.state_at(e).position_km,
        );
        let helio = |lo: DVec3, de: DVec3| ((lo - s_lo) - (de - s_de)).length();
        worst[0] = worst[0].max(helio(
            planets::EARTH.state_at(e).position_km,
            earth.state_at(e).position_km,
        ));
        worst[1] = worst[1].max(helio(
            planets::MARS.state_at(e).position_km,
            mars.state_at(e).position_km,
        ));
        worst[2] = worst[2].max((s_lo - s_de).length());
    }
    eprintln!(
        "low-precision max |Δr| km: earth/sun {:.0}, mars/sun {:.0}, sun/ssb {:.0}",
        worst[0], worst[1], worst[2]
    );
    // Earth series is EMB: ≥ ~4 700 km from the Earth–Moon offset alone.
    assert!(worst[0] < 2.0e4, "earth {}", worst[0]);
    assert!(worst[1] < 1.5e5, "mars {}", worst[1]);
    // Sun–SSB model: Jupiter + Saturn only.
    assert!(worst[2] < 5.0e5, "sun {}", worst[2]);
}

// --- EOP files --------------------------------------------------------------

fn data(name: &str) -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(name);
    std::fs::read_to_string(&path)
        .map_err(|_| {
            eprintln!(
                "skipped: {} missing (run scripts/fetch-data.sh)",
                path.display()
            )
        })
        .ok()
}

/// Current IERS series vs Vallado's 2004-04-06 0h values (series revised since; ≤ 0.5 mas, 0.1 ms).
#[test]
fn eop_files_match_vallado_epoch() {
    use terramenta_solare::eop::{EopTable, FinalsKind};
    let want = ex315_eop();
    let check = |label: &str, e: Eop| {
        assert!(
            (e.ut1_minus_utc - want.ut1_minus_utc).abs() < 1e-4,
            "{label} UT1−UTC {}",
            e.ut1_minus_utc
        );
        assert!(
            (e.xp - want.xp).abs() < 5e-4 * AS,
            "{label} xp {}",
            e.xp / AS
        );
        assert!(
            (e.yp - want.yp).abs() < 5e-4 * AS,
            "{label} yp {}",
            e.yp / AS
        );
    };
    if let Some(text) = data("finals2000A.all") {
        check(
            "finals2000A",
            EopTable::from_iers_finals(&text, FinalsKind::Iau2000A)
                .unwrap()
                .at_mjd_utc(53101.0),
        );
    }
    if let Some(text) = data("EOP-All.csv") {
        check(
            "celestrak",
            EopTable::from_celestrak_csv(&text)
                .unwrap()
                .at_mjd_utc(53101.0),
        );
    }
}
