//! Non-Earth central bodies: IAU orientation vs ANISE/NAIF kernels, embedded fields in flight.
//!
//! Orientation references need `data/pck00011.tpc`, `data/gm_de440.tpc`,
//! `data/moon_pa_de440_200625.bpc` (`scripts/fetch-data.sh`); skipped if absent.

use std::path::PathBuf;

use glam::{DMat3, DVec3};
use terramenta_solare::bodies::CentralBody;
use terramenta_solare::force::SphericalHarmonics;
use terramenta_solare::iau::{IauOrientation, MOON};
use terramenta_solare::orbit::OrbitalElements;
use terramenta_solare::propagate::{Integrator, Method};
use terramenta_solare::spice::anise::naif::BPC;
use terramenta_solare::spice::anise::naif::kpl::parser::convert_tpc;
use terramenta_solare::spice::anise::prelude::{Almanac, Frame};
use terramenta_solare::{Epoch, Propagator, SpacecraftState, StateVector};

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

fn almanac() -> Option<Almanac> {
    let pck = data("pck00011.tpc")?;
    let gm = data("gm_de440.tpc")?;
    let bpc = data("moon_pa_de440_200625.bpc")?;
    let planetary = convert_tpc(pck, gm).unwrap();
    let bpc = BPC::load(bpc.to_str()?).unwrap();
    Some(
        Almanac::default()
            .with_planetary_data(planetary)
            .with_bpc(bpc),
    )
}

/// ICRF (J2000) → `orientation_id`, ANISE.
fn reference(almanac: &Almanac, center: i32, orientation_id: i32, e: Epoch) -> DMat3 {
    let dcm = almanac
        .rotate(
            Frame::new(center, 1),
            Frame::new(center, orientation_id),
            e.as_hifitime(),
        )
        .unwrap();
    let m = dcm.rot_mat;
    DMat3::from_cols_array_2d(&std::array::from_fn(|c| std::array::from_fn(|r| m[(r, c)])))
}

/// Rotation angle between two DCMs, rad (small-angle: skew part of `a bᵀ`).
fn angle(a: DMat3, b: DMat3) -> f64 {
    let d = a * b.transpose();
    0.5 * DVec3::new(
        d.y_axis.z - d.z_axis.y,
        d.z_axis.x - d.x_axis.z,
        d.x_axis.y - d.y_axis.x,
    )
    .length()
}

fn epochs() -> impl Iterator<Item = Epoch> {
    [2000, 2010, 2024, 2031, 2040]
        .into_iter()
        .map(|y| Epoch::from_utc(y, 3, 17, 6, 0, 0.0))
}

#[test]
fn iau_moon_and_mars_match_anise() {
    let Some(a) = almanac() else { return };
    // ANISE 0.10 drops the quadratic W₂ (−1.4e-12 °/d²; 3.4e-7 rad by 2010); SPICE keeps it.
    let mut linear = MOON;
    linear.prime_meridian[2] = 0.0;
    for e in epochs() {
        let moon = IauOrientation::new(linear).transform(e).rotation();
        let mars = IauOrientation::mars().transform(e).rotation();
        let (dm, dr) = (
            angle(moon, reference(&a, 301, 301, e)),
            angle(mars, reference(&a, 499, 499, e)),
        );
        // 1e-9 rad: 2 mm on the lunar surface.
        assert!(dm < 1e-9 && dr < 1e-9, "{e:?}: moon {dm:e}, mars {dr:e}");
    }
}

#[test]
fn moon_pa_tracks_de440_librations() {
    let Some(a) = almanac() else { return };
    let mut worst = 0.0_f64;
    for e in epochs() {
        let ours = IauOrientation::moon_pa().transform(e).rotation();
        // MOON_PA_DE440.
        worst = worst.max(angle(ours, reference(&a, 301, 31008, e)));
    }
    // NAIF: IAU vs DE440 ME ≤ 0.0051° (2000–2040). Without the PA offset ≈ 0.029°.
    eprintln!("moon PA: worst {:.5}°", worst.to_degrees());
    assert!(worst.to_degrees() < 0.006, "{}", worst.to_degrees());
}

/// Mars J2 nodal regression in the Mars equator: `Ω̇ = −3/2 n J2 (R/p)² cos i`.
#[test]
fn mars_j2_nodal_rate() {
    let body = CentralBody::Mars;
    let field = body.gravity_field(2);
    let (gm, re, j2) = (field.gm_km3_s2, field.radius_km, field.j(2));
    let orient = body.iau_orientation().unwrap();
    let e0 = Epoch::from_utc(2030, 1, 1, 0, 0, 0.0);
    // ICRF → Mars equator of epoch; pole drift over 2 days ≪ tolerance.
    let pole = orient.transform(e0).rotation().transpose() * DVec3::Z;
    let x = DVec3::Z.cross(pole).normalize();
    let eq = DMat3::from_cols(x, pole.cross(x), pole).transpose();
    let (a, i) = (3_800.0, 50_f64.to_radians());
    let el = OrbitalElements {
        semi_major_axis_km: a,
        eccentricity: 0.001,
        inclination_rad: i,
        raan_rad: 0.3,
        arg_periapsis_rad: 0.0,
        mean_anomaly_at_epoch: 0.0,
        epoch_seconds: 0.0,
    };
    let s = el.state_at_true_anomaly(gm, 0.0);
    let s0 = SpacecraftState::new(
        e0,
        StateVector::new(
            eq.transpose() * s.position_km,
            eq.transpose() * s.velocity_km_s,
        ),
        500.0,
    );
    let mut p = Propagator::new(Integrator::new(Method::Dop853))
        .with_force(SphericalHarmonics::new(field, 2, 0))
        .with_orientation(orient);
    let mut raan = Vec::new();
    p.propagate_with(&s0, e0.advanced_by_seconds(2.0 * 86_400.0), 60.0, |s| {
        let sv = StateVector::new(eq * s.position_km, eq * s.velocity_km_s);
        raan.push(OrbitalElements::from_state(sv, gm, 0.0).raan_rad);
    })
    .unwrap();
    let n = raan.len() as f64;
    let t: Vec<f64> = (0..raan.len()).map(|k| k as f64 * 60.0).collect();
    let (mt, my) = (t.iter().sum::<f64>() / n, raan.iter().sum::<f64>() / n);
    let slope = t
        .iter()
        .zip(&raan)
        .map(|(t, y)| (t - mt) * (y - my))
        .sum::<f64>()
        / t.iter().map(|t| (t - mt).powi(2)).sum::<f64>();
    let k = (gm / a.powi(3)).sqrt() * j2 * (re / a).powi(2);
    let want = -1.5 * k * i.cos();
    eprintln!("Mars Ω̇ {slope:e} vs {want:e}");
    assert!((slope / want - 1.0).abs() < 1e-2);
}

/// 100 km polar lunar orbit, one day: degree 20 departs from J2-only by km (mascons) and
/// stays above the surface.
#[test]
fn lunar_low_orbit_feels_mascons() {
    let body = CentralBody::Moon;
    let gm = body.gm_km3_s2();
    let e0 = Epoch::from_utc(2027, 6, 1, 0, 0, 0.0);
    let r = body.equatorial_radius_km() + 100.0;
    let v = (gm / r).sqrt();
    let s0 = SpacecraftState::new(
        e0,
        StateVector::new(DVec3::new(r, 0.0, 0.0), DVec3::new(0.0, 0.0, v)),
        500.0,
    );
    let run = |degree| {
        let mut p = Propagator::new(Integrator::new(Method::Dop853))
            .with_force(SphericalHarmonics::new(
                body.gravity_field(degree),
                degree,
                degree,
            ))
            .with_orientation(body.iau_orientation().unwrap());
        let mut low = f64::INFINITY;
        let end = p
            .propagate_with(&s0, e0.advanced_by_seconds(86_400.0), 30.0, |s| {
                low = low.min(s.position_km.length());
            })
            .unwrap();
        (end.position_km, low - body.equatorial_radius_km())
    };
    let (r2, _) = run(2);
    let (r20, low) = run(20);
    let d = (r20 - r2).length();
    eprintln!("20×20 vs 2×2 after 1 d: {d:.1} km; lowest {low:.1} km");
    assert!(d > 1.0, "{d}");
    assert!(low > 50.0, "{low}");
}
