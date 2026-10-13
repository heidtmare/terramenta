//! Numerical propagation validation.
//!
//! - Analytic: J2 secular rates; Jacobi integral in a uniformly rotating 20×20 field;
//!   circular-orbit drag decay.
//! - External: Moon geocentric state propagated 27 d against DE440s.
//! - Consistency: RKF78 vs DOP853 under full LEO force model.
//! - NRLMSISE-00 with CelesTrak space weather vs Harris–Priester decay.
//!
//! Kernel/EOP tests skip without `data/` (`scripts/fetch-data.sh`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use glam::{DMat3, DVec3};
use terramenta_solare::bodies::*;
use terramenta_solare::earth::{EarthModel, TerrestrialTransform};
use terramenta_solare::eop::{EopTable, FinalsKind};
use terramenta_solare::force::*;
use terramenta_solare::orbit::OrbitalElements;
use terramenta_solare::propagate::{BodyOrientation, EarthOrientation, Integrator, Method};
use terramenta_solare::space_weather::SpaceWeather;
use terramenta_solare::spice::{self, KernelEphemeris, naif};
use terramenta_solare::{Ephemeris, Epoch, Propagator, SpacecraftState, StateVector};

fn kernels() -> Option<Arc<spice::anise::prelude::Almanac>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/de440s.bsp");
    if !path.exists() {
        eprintln!("skipped: {} missing", path.display());
        return None;
    }
    Some(Arc::new(
        spice::load_kernel_file(Default::default(), path.to_str()?).unwrap(),
    ))
}

fn eop() -> Option<Arc<EopTable>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/finals2000A.all");
    let text = std::fs::read_to_string(path).ok()?;
    Some(Arc::new(
        EopTable::from_iers_finals(&text, FinalsKind::Iau2000A).unwrap(),
    ))
}

fn epoch0() -> Epoch {
    Epoch::from_utc(2024, 3, 1, 0, 0, 0.0)
}

fn elements(s: &SpacecraftState, gm: f64) -> OrbitalElements {
    OrbitalElements::from_state(s.state_vector(), gm, 0.0)
}

/// Mean RAAN and argument-of-perigee rates vs `−3/2 n J2 (R/p)² cos i` and
/// `3/4 n J2 (R/p)² (5cos²i − 1)`.
#[test]
fn j2_secular_rates() {
    let field = GravityField::egm2008(2);
    let (gm, re, j2) = (field.gm_km3_s2, field.radius_km, field.j(2));
    let s0 = SpacecraftState::new(
        epoch0(),
        StateVector::new(DVec3::new(7200.0, 0.0, 0.0), DVec3::new(0.0, 5.0, 5.2)),
        1000.0,
    );
    let mut p = Propagator::new(Integrator::new(Method::Dop853))
        .with_force(SphericalHarmonics::new(field, 2, 0))
        .with_orientation(EarthOrientation::default());
    let days = 10.0;
    let mut raan = Vec::new();
    let mut argp = Vec::new();
    let mut el0 = None;
    p.propagate_with(
        &s0,
        epoch0().advanced_by_seconds(days * 86_400.0),
        60.0,
        |s| {
            let el = elements(s, gm);
            el0.get_or_insert(el);
            raan.push(el.raan_rad);
            argp.push(el.arg_periapsis_rad);
        },
    )
    .unwrap();
    let unwrap = |v: &[f64]| {
        let mut out = vec![v[0]];
        for w in v.windows(2) {
            let d = (w[1] - w[0] + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
                - std::f64::consts::PI;
            out.push(out.last().unwrap() + d);
        }
        out
    };
    // Least-squares slope, rad/s.
    let slope = |y: &[f64]| {
        let n = y.len() as f64;
        let t: Vec<f64> = (0..y.len()).map(|i| i as f64 * 60.0).collect();
        let (mt, my) = (t.iter().sum::<f64>() / n, y.iter().sum::<f64>() / n);
        t.iter()
            .zip(y)
            .map(|(t, y)| (t - mt) * (y - my))
            .sum::<f64>()
            / t.iter().map(|t| (t - mt).powi(2)).sum::<f64>()
    };
    let el = el0.unwrap();
    let (a, e, i) = (el.semi_major_axis_km, el.eccentricity, el.inclination_rad);
    let n = (gm / a.powi(3)).sqrt();
    let k = n * j2 * (re / (a * (1.0 - e * e))).powi(2);
    let raan_rate = -1.5 * k * i.cos();
    let argp_rate = 0.75 * k * (5.0 * i.cos().powi(2) - 1.0);
    let got_raan = slope(&unwrap(&raan));
    let got_argp = slope(&unwrap(&argp));
    eprintln!("Ω̇ {got_raan:e} vs {raan_rate:e}; ω̇ {got_argp:e} vs {argp_rate:e}");
    assert!((got_raan / raan_rate - 1.0).abs() < 5e-3);
    assert!((got_argp / argp_rate - 1.0).abs() < 3e-2);
}

/// Body rotating uniformly about inertial z.
#[derive(Debug)]
struct UniformRotation;

const OMEGA: f64 = 7.292_115e-5;

impl BodyOrientation for UniformRotation {
    fn transform(&self, epoch: Epoch) -> TerrestrialTransform {
        let th = OMEGA * epoch.tt_seconds();
        let (s, c) = th.sin_cos();
        TerrestrialTransform {
            inertial_to_tirs: DMat3::from_cols(
                DVec3::new(c, -s, 0.0),
                DVec3::new(s, c, 0.0),
                DVec3::Z,
            ),
            polar_motion: DMat3::IDENTITY,
            omega_rad_s: OMEGA,
        }
    }
}

/// `C = ½|v|² − ω·(r × v) − U(r_bf)` is conserved in a uniformly rotating field.
#[test]
fn jacobi_integral_20x20() {
    let field = GravityField::egm2008(20);
    let s0 = SpacecraftState::new(
        epoch0(),
        StateVector::new(DVec3::new(6900.0, 0.0, 300.0), DVec3::new(0.0, 4.0, 6.4)),
        1000.0,
    );
    let jacobi = |s: &SpacecraftState| {
        let rot = UniformRotation.transform(s.epoch).rotation();
        let (r, v) = (s.position_km, s.velocity_km_s);
        0.5 * v.length_squared() - OMEGA * r.cross(v).z - field.potential(rot * r, 20, 20)
    };
    let mut p = Propagator::new(Integrator::new(Method::Rkf78).with_tolerance(1e-13, 1e-10))
        .with_force(SphericalHarmonics::new(field.clone(), 20, 20))
        .with_orientation(UniformRotation);
    p.orientation_refresh_s = 0.0;
    let c0 = jacobi(&s0);
    let mut worst: f64 = 0.0;
    p.propagate_with(&s0, epoch0().advanced_by_seconds(86_400.0), 300.0, |s| {
        worst = worst.max(((jacobi(s) - c0) / c0).abs());
    })
    .unwrap();
    eprintln!("Jacobi |ΔC/C| ≤ {worst:e}");
    assert!(worst < 1e-11);
}

/// Δa per revolution ≈ −2π ρ (C_D A/m) a², circular orbit, constant ρ.
#[test]
fn drag_decay_matches_circular_estimate() {
    #[derive(Debug)]
    struct Constant;
    impl drag::Atmosphere for Constant {
        fn density(&self, _: &drag::AtmosphereSample) -> f64 {
            1e-11
        }
    }
    let gm = GM_EARTH_KM3_S2;
    let a = 6378.137 + 300.0;
    let vc = (gm / a).sqrt();
    let s0 = SpacecraftState::new(
        epoch0(),
        StateVector::new(DVec3::new(a, 0.0, 0.0), DVec3::new(0.0, vc, 0.0)),
        500.0,
    );
    let vehicle = Vehicle {
        drag_area_m2: 5.0,
        drag_coefficient: 2.2,
        ..Default::default()
    };
    // Non-rotating atmosphere: isolates the estimate.
    let mut p = Propagator::new(Integrator::new(Method::Dop853))
        .with_force(PointMass { gm_km3_s2: gm })
        .with_force(Drag::new(Constant))
        .with_orientation(NoRotation)
        .with_vehicle(vehicle);
    let period = std::f64::consts::TAU * (a.powi(3) / gm).sqrt();
    let revs = 10.0;
    let s1 = p
        .propagate(&s0, epoch0().advanced_by_seconds(revs * period))
        .unwrap();
    let da = elements(&s1, gm).semi_major_axis_km - a;
    // ρ [kg/m³] · B [m²/kg] · a² [km²] → ×1e3 for km.
    let expect = -std::f64::consts::TAU * 1e-11 * 2.2 * 5.0 / 500.0 * a * a * 1e3 * revs;
    eprintln!("Δa {da} km vs {expect} km");
    assert!((da / expect - 1.0).abs() < 0.02);
}

#[derive(Debug)]
struct NoRotation;

impl BodyOrientation for NoRotation {
    fn transform(&self, _: Epoch) -> TerrestrialTransform {
        TerrestrialTransform {
            inertial_to_tirs: DMat3::IDENTITY,
            polar_motion: DMat3::IDENTITY,
            omega_rad_s: 0.0,
        }
    }
}

/// Moon geocentric motion: Earth+Moon point mass, Earth J2, Sun and planets, vs DE440s.
#[test]
fn moon_matches_de440s() {
    let Some(almanac) = kernels() else { return };
    let k = |t| KernelEphemeris::new(almanac.clone(), t, naif::EARTH);
    let t0 = Epoch::from_tdb_seconds_j2000(8.0e8);
    let moon = k(naif::MOON);
    let s0 = SpacecraftState::new(t0, moon.state_at(t0), 1.0);
    let mut p = Propagator::new(Integrator::new(Method::Dop853).with_tolerance(1e-13, 1e-9))
        .with_force(PointMass {
            gm_km3_s2: GM_EARTH_KM3_S2 + GM_MOON_KM3_S2,
        })
        .with_force(SphericalHarmonics::egm2008(2, 0))
        .with_force(ThirdBody::new(GM_SUN_KM3_S2, k(naif::SUN)))
        .with_force(ThirdBody::new(GM_VENUS_KM3_S2, k(naif::VENUS_BARYCENTER)))
        .with_force(ThirdBody::new(GM_MARS_KM3_S2, k(naif::MARS_BARYCENTER)))
        .with_force(ThirdBody::new(
            GM_JUPITER_SYSTEM_KM3_S2,
            k(naif::JUPITER_BARYCENTER),
        ))
        .with_force(ThirdBody::new(
            GM_SATURN_SYSTEM_KM3_S2,
            k(naif::SATURN_BARYCENTER),
        ))
        .with_orientation(EarthOrientation::new(eop(), EarthModel::Iau2006Cio));
    // The harmonic model includes its own GM·r/r³ term; cancel it so the central term is μ_E + μ_M.
    p = p.with_force(PointMass {
        gm_km3_s2: -GravityField::egm2008(2).gm_km3_s2,
    });
    let mut worst: f64 = 0.0;
    p.propagate_with(
        &s0,
        t0.advanced_by_seconds(27.0 * 86_400.0),
        86_400.0,
        |s| {
            worst = worst.max((s.position_km - moon.state_at(s.epoch).position_km).length());
        },
    )
    .unwrap();
    eprintln!("Moon vs DE440s, 27 d: max |Δr| = {worst:.3} km");
    assert!(worst < 1.0, "{worst} km");
}

fn full_force(almanac: &Arc<spice::anise::prelude::Almanac>, method: Method) -> Propagator {
    let k = |t| KernelEphemeris::new(almanac.clone(), t, naif::EARTH);
    Propagator::new(Integrator::new(method).with_tolerance(1e-12, 1e-9))
        .with_force(SphericalHarmonics::egm2008(20, 20))
        .with_force(ThirdBody::new(GM_SUN_KM3_S2, k(naif::SUN)))
        .with_force(ThirdBody::new(GM_MOON_KM3_S2, k(naif::MOON)))
        .with_force(SolarRadiationPressure::new(k(naif::SUN), 6378.137))
        .with_force(Drag::new(HarrisPriester::new(k(naif::SUN))))
        .with_force(Relativity {
            gm_km3_s2: GM_EARTH_KM3_S2,
        })
        .with_orientation(EarthOrientation::new(eop(), EarthModel::Iau2006Cio))
}

#[test]
fn full_force_leo_integrators_agree() {
    let Some(almanac) = kernels() else { return };
    let s0 = SpacecraftState::new(
        epoch0(),
        StateVector::new(DVec3::new(6778.137, 0.0, 0.0), DVec3::new(0.0, 4.7, 6.0)),
        850.0,
    );
    let to = epoch0().advanced_by_seconds(86_400.0);
    let mut out = Vec::new();
    for m in [Method::Rkf78, Method::Dop853] {
        let mut p = full_force(&almanac, m);
        let t = Instant::now();
        let s = p.propagate(&s0, to).unwrap();
        eprintln!("{m:?}: {:?}, {:?}", t.elapsed(), p.integrator.stats);
        out.push(s);
    }
    let d = (out[0].position_km - out[1].position_km).length();
    eprintln!("RKF78 vs DOP853, 1 d LEO: {:.3} m", d * 1e3);
    assert!(d < 1e-3, "{d} km");
    // Decayed, not escaped.
    let a = |s: &SpacecraftState| elements(s, GM_EARTH_KM3_S2).semi_major_axis_km;
    assert!(a(&out[0]) < a(&s0) + 10.0 && a(&out[0]) > a(&s0) - 20.0);
}

/// One day at 400 km during solar maximum (2024-03): MSIS decay exceeds mean-activity HP.
#[test]
fn nrlmsise_space_weather_decay() {
    let Some(almanac) = kernels() else { return };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/SW-All.csv");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("skipped: {} missing", path.display());
        return;
    };
    let weather = Arc::new(SpaceWeather::from_celestrak_csv(&text).unwrap());
    let d = weather.msis_drivers(epoch0());
    eprintln!("2024-03-01 drivers: {d:?}");
    assert!((100.0..300.0).contains(&d.f107_average));
    let sun = KernelEphemeris::new(almanac.clone(), naif::SUN, naif::EARTH);
    let s0 = SpacecraftState::new(
        epoch0(),
        StateVector::new(DVec3::new(6778.137, 0.0, 0.0), DVec3::new(0.0, 4.7, 6.0)),
        850.0,
    );
    let decay = |atmosphere: Drag| {
        let mut p = Propagator::new(Integrator::new(Method::Dop853))
            .with_force(SphericalHarmonics::egm2008(4, 4))
            .with_force(atmosphere)
            .with_orientation(EarthOrientation::new(eop(), EarthModel::Iau2006Cio));
        let t = Instant::now();
        let s = p
            .propagate(&s0, epoch0().advanced_by_seconds(86_400.0))
            .unwrap();
        eprintln!("{:?}", t.elapsed());
        elements(&s, GM_EARTH_KM3_S2).semi_major_axis_km
    };
    let msis = decay(Drag::new(Nrlmsise00::new(weather)));
    let hp = decay(Drag::new(HarrisPriester::new(sun)));
    let a0 = elements(&s0, GM_EARTH_KM3_S2).semi_major_axis_km;
    eprintln!("Δa 1 d: MSIS {:.3} km, HP {:.3} km", msis - a0, hp - a0);
    assert!(msis < hp && hp < a0 + 5.0);
}

/// Analytic Moon vs DE440s over 20 years: direction and distance.
#[test]
fn analytic_moon_vs_de440s() {
    let Some(almanac) = kernels() else { return };
    let de = KernelEphemeris::new(almanac, naif::MOON, naif::EARTH);
    let (mut worst_deg, mut worst_km) = (0.0f64, 0.0f64);
    for k in 0..2000 {
        let t = Epoch::J2000.advanced_by_seconds(k as f64 * 3.65 * 86_400.0);
        let (a, b) = (
            terramenta_solare::moon::position_km(t),
            de.state_at(t).position_km,
        );
        worst_deg = worst_deg.max(a.angle_between(b).to_degrees());
        worst_km = worst_km.max((a.length() - b.length()).abs());
    }
    eprintln!("analytic Moon: {worst_deg:.3}°, {worst_km:.0} km");
    assert!(worst_deg < 0.2 && worst_km < 1500.0);
}

/// Degree 120 from `data/EGM2008_120.gfc`: gradient of the potential, and the embedded 20×20
/// subset agrees.
#[test]
fn egm2008_degree_120() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/EGM2008_120.gfc");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("skipped: {} missing", path.display());
        return;
    };
    let f = GravityField::from_gfc(&text, 120).unwrap();
    assert_eq!(f.max_degree, 120);
    let small = GravityField::egm2008(20);
    for n in 2..=20 {
        for m in 0..=n {
            assert_eq!(f.coefficient(n, m), small.coefficient(n, m));
        }
    }
    // 4th-order central differences; residual is rounding in U (∝ 1/h).
    let r = DVec3::new(4100.0, -3500.0, 4300.0);
    let a = f.acceleration(r, 120, 120);
    let u = |p: DVec3| f.potential(p, 120, 120);
    let h = 0.5;
    let d = |e: DVec3| {
        (-u(r + 2.0 * e * h) + 8.0 * u(r + e * h) - 8.0 * u(r - e * h) + u(r - 2.0 * e * h))
            / (12.0 * h)
    };
    let grad = DVec3::new(d(DVec3::X), d(DVec3::Y), d(DVec3::Z));
    assert!((a - grad).length() < 1e-7 * a.length(), "{a} vs {grad}");
    let t = Instant::now();
    for _ in 0..100 {
        std::hint::black_box(f.acceleration(r, 70, 70));
    }
    eprintln!("70×70 acceleration: {:?}/eval", t.elapsed() / 100);
}
