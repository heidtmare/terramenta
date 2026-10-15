//! Cross-check against GMAT R2026a.
//!
//! References in `tests/gmat/*.txt` from `validation/gmat/*.script`
//! (`scripts/gmat-reference.sh`). Shared setup: 2024-03-01 00:00 UTC, EarthICRF, DE440s,
//! GM_sun/GM_moon as [`bodies`], WGS-84 Earth shape, FK5 Earth orientation, 850 kg,
//! C_D 2.2 / 15 m², C_R 1.8 / 20 m², PrinceDormand78 at 1e-13. NRLMSISE-00 driven by
//! CSSI space weather (GMAT `SpaceWeather-All-v1.2.txt`, `data/SW-All.csv`).
//! Needs `data/` (`scripts/fetch-data.sh`); skipped if absent.

use std::path::PathBuf;
use std::sync::Arc;

use glam::DVec3;
use terramenta_solare::bodies::*;
use terramenta_solare::earth::EarthModel;
use terramenta_solare::eop::{EopTable, FinalsKind};
use terramenta_solare::force::drag::Atmosphere;
use terramenta_solare::force::*;
use terramenta_solare::propagate::{EarthOrientation, Integrator, Method};
use terramenta_solare::space_weather::SpaceWeather;
use terramenta_solare::spice::{self, KernelEphemeris, naif};
use terramenta_solare::{Epoch, Propagator, SpacecraftState, StateVector};

/// GMAT built-in Earth μ; also EGM2008's.
const GM_EARTH_GMAT: f64 = 398_600.441_5;

fn data(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(name);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipped: {} missing", path.display());
        None
    }
}

struct Env {
    almanac: Arc<spice::anise::prelude::Almanac>,
    eop: Arc<EopTable>,
    weather: Arc<SpaceWeather>,
}

fn env() -> Option<Env> {
    let kernel = data("de440s.bsp")?;
    let finals = std::fs::read_to_string(data("finals2000A.all")?).ok()?;
    let weather = std::fs::read_to_string(data("SW-All.csv")?).ok()?;
    Some(Env {
        almanac: Arc::new(spice::load_kernel_file(Default::default(), kernel.to_str()?).unwrap()),
        eop: Arc::new(EopTable::from_iers_finals(&finals, FinalsKind::Iau2000A).unwrap()),
        weather: Arc::new(SpaceWeather::from_celestrak_csv(&weather).unwrap()),
    })
}

impl Env {
    fn body(&self, target: i32) -> KernelEphemeris {
        KernelEphemeris::new(self.almanac.clone(), target, naif::EARTH)
    }

    fn propagator(&self) -> Propagator {
        Propagator::new(Integrator::new(Method::Dop853).with_tolerance(1e-13, 1e-10))
            .with_orientation(EarthOrientation::new(
                Some(self.eop.clone()),
                EarthModel::Iau76Fk5,
            ))
            .with_sun(self.body(naif::SUN))
            .with_vehicle(Vehicle {
                dry_mass_kg: 850.0,
                drag_area_m2: 15.0,
                drag_coefficient: 2.2,
                srp_area_m2: 20.0,
                reflectivity: 1.8,
                plates: Vec::new(),
            })
    }

    fn point_mass(&self) -> Propagator {
        self.propagator().with_force(PointMass {
            gm_km3_s2: GM_EARTH_GMAT,
        })
    }

    fn msis(&self) -> Drag {
        Drag::new(Nrlmsise00::new(self.weather.clone()))
    }

    fn srp(&self) -> SolarRadiationPressure {
        SolarRadiationPressure::new(self.body(naif::SUN), 6378.137)
    }
}

/// `(elapsed s, state, extra columns)` rows.
fn reference(case: &str) -> Vec<(f64, StateVector, Vec<f64>)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/gmat/{case}.txt"));
    let text = std::fs::read_to_string(&path).unwrap();
    let rows: Vec<Vec<f64>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| l.split_whitespace().map(|x| x.parse().unwrap()).collect())
        .collect();
    let mjd0 = rows[0][0];
    rows.iter()
        .map(|r| {
            // GMAT stops on whole seconds; MJD rounding is ~1e-7 s.
            let t = ((r[0] - mjd0) * 86_400.0 * 1e3).round() / 1e3;
            let s = StateVector::new(DVec3::new(r[1], r[2], r[3]), DVec3::new(r[4], r[5], r[6]));
            (t, s, r[7..].to_vec())
        })
        .collect()
}

/// Max position and velocity difference (km, km/s) over the reference.
fn compare(case: &str, mut p: Propagator) -> (f64, f64) {
    let rows = reference(case);
    let t0 = Epoch::from_utc(2024, 3, 1, 0, 0, 0.0);
    let mut s = SpacecraftState::new(t0, rows[0].1, 850.0);
    let (mut dr, mut dv) = (0.0f64, 0.0f64);
    for (t, want, _) in &rows[1..] {
        s = p.propagate(&s, t0.advanced_by_seconds(*t)).unwrap();
        dr = dr.max((s.position_km - want.position_km).length());
        dv = dv.max((s.velocity_km_s - want.velocity_km_s).length());
    }
    eprintln!(
        "{case}: max |Δr| {:.3} m, |Δv| {:.3} mm/s over {:.1} d",
        dr * 1e3,
        dv * 1e6,
        rows.last().unwrap().0 / 86_400.0
    );
    (dr, dv)
}

#[test]
fn two_body() {
    let Some(e) = env() else { return };
    let (dr, _) = compare("two_body", e.point_mass());
    assert!(dr < 1e-4, "{dr} km");
}

#[test]
fn harmonics_20x20() {
    let Some(e) = env() else { return };
    let (dr, _) = compare(
        "harmonics_20x20",
        e.propagator()
            .with_force(SphericalHarmonics::egm2008(20, 20)),
    );
    assert!(dr < 5e-3, "{dr} km");
}

#[test]
fn third_body() {
    let Some(e) = env() else { return };
    let p = e
        .point_mass()
        .with_force(ThirdBody::new(GM_SUN_KM3_S2, e.body(naif::SUN)))
        .with_force(ThirdBody::new(GM_MOON_KM3_S2, e.body(naif::MOON)));
    let (dr, _) = compare("third_body", p);
    assert!(dr < 1e-3, "{dr} km");
}

#[test]
fn srp() {
    let Some(e) = env() else { return };
    let (dr, _) = compare("srp", e.point_mass().with_force(e.srp()));
    assert!(dr < 1e-3, "{dr} km");
}

#[test]
fn drag_exponential() {
    let Some(e) = env() else { return };
    let p = e.point_mass().with_force(Drag::new(ExponentialAtmosphere));
    let (dr, _) = compare("drag_exponential", p);
    assert!(dr < 5e-3, "{dr} km");
}

/// GMAT's NRLMSISE-00 space-weather selection is undocumented; densities agree pointwise
/// within 6 % (well inside the model's ~15 %), ≈ 2 % of the 1-day drag displacement.
#[test]
fn drag_msis() {
    use terramenta_solare::earth::ecef_to_geodetic;
    use terramenta_solare::force::drag::AtmosphereSample;
    use terramenta_solare::propagate::BodyOrientation;
    let Some(e) = env() else { return };
    let msis = Nrlmsise00::new(e.weather.clone());
    let orientation = EarthOrientation::new(Some(e.eop.clone()), EarthModel::Iau76Fk5);
    let t0 = Epoch::from_utc(2024, 3, 1, 0, 0, 0.0);
    let mut worst: f64 = 0.0;
    for (t, s, extra) in reference("drag_msis") {
        let epoch = t0.advanced_by_seconds(t);
        let fixed = orientation.transform(epoch).to_itrf(s);
        let rho = msis.density(&AtmosphereSample {
            epoch,
            position_inertial_km: s.position_km,
            position_body_fixed_km: fixed.position_km,
            geodetic: ecef_to_geodetic(fixed.position_km),
            sun_km: None,
        });
        // GMAT kg/km³.
        worst = worst.max((rho / (extra[0] * 1e-9)).ln().abs());
    }
    eprintln!("drag_msis: max |ln ρ/ρ_GMAT| {worst:.4}");
    assert!(worst < 0.06, "{worst}");
    let (dr, _) = compare("drag_msis", e.point_mass().with_force(e.msis()));
    assert!(dr < 10.0, "{dr} km");
}

#[test]
fn relativity() {
    let Some(e) = env() else { return };
    let p = e.point_mass().with_force(Relativity {
        gm_km3_s2: GM_EARTH_GMAT,
    });
    let (dr, _) = compare("relativity", p);
    assert!(dr < 1e-3, "{dr} km");
}

#[test]
fn full_leo() {
    let Some(e) = env() else { return };
    let p = e
        .propagator()
        .with_force(SphericalHarmonics::egm2008(20, 20))
        .with_force(ThirdBody::new(GM_SUN_KM3_S2, e.body(naif::SUN)))
        .with_force(ThirdBody::new(GM_MOON_KM3_S2, e.body(naif::MOON)))
        .with_force(e.msis())
        .with_force(e.srp())
        .with_force(Relativity {
            gm_km3_s2: GM_EARTH_GMAT,
        });
    // Dominated by the NRLMSISE-00 difference; see `drag_msis`.
    let (dr, _) = compare("full_leo", p);
    assert!(dr < 12.0, "{dr} km");
}
