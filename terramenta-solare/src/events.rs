//! Event detection: sign changes of a scalar function of time, bracketed on a fixed grid
//! and refined by Brent's method. Ground-station visibility, conical eclipse, apsides and
//! nodes are built on it.
//!
//! The grid step bounds resolution: an interval shorter than one step (a grazing pass, a
//! brief penumbra) can fall between samples and be missed. No light-time or aberration.

use glam::DVec3;

use crate::bodies::SUN_MEAN_RADIUS_KM;
use crate::earth::geodetic_to_ecef;
use crate::frame::StateVector;
use crate::time::Epoch;

/// Zero of an event function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossing {
    pub epoch: Epoch,
    /// `g` goes from ≤ 0 to > 0.
    pub rising: bool,
}

/// Whether `g(start) > 0`, and the crossings of `g` on `(start, stop]`, in time order.
/// `stop` must not precede `start`. NaN samples break the bracket they touch.
pub fn scan(
    g: &mut impl FnMut(Epoch) -> f64,
    start: Epoch,
    stop: Epoch,
    step_s: f64,
    tolerance_s: f64,
) -> (bool, Vec<Crossing>) {
    let span = stop.seconds_since(start).max(0.0);
    let step = step_s.abs().max(1e-3);
    let n = (span / step).ceil().max(1.0) as usize;
    let mut at = |t: f64| g(start.advanced_by_seconds(t));
    let (mut t0, mut g0) = (0.0, at(0.0));
    let initial = g0 > 0.0;
    let mut out = Vec::new();
    for i in 1..=n {
        let t1 = (i as f64 * step).min(span);
        let g1 = at(t1);
        if !g0.is_nan() && !g1.is_nan() && (g0 > 0.0) != (g1 > 0.0) {
            let t = brent(&mut at, t0, t1, g0, g1, tolerance_s);
            out.push(Crossing {
                epoch: start.advanced_by_seconds(t),
                rising: g1 > 0.0,
            });
        }
        (t0, g0) = (t1, g1);
    }
    (initial, out)
}

/// Spans of `[start, stop]` where `g > 0`; spans open at either end are clipped to it.
pub fn intervals(
    g: &mut impl FnMut(Epoch) -> f64,
    start: Epoch,
    stop: Epoch,
    step_s: f64,
    tolerance_s: f64,
) -> Vec<(Epoch, Epoch)> {
    let (initial, crossings) = scan(g, start, stop, step_s, tolerance_s);
    let mut out = Vec::new();
    let mut open = initial.then_some(start);
    for c in crossings {
        match (open, c.rising) {
            (None, true) => open = Some(c.epoch),
            (Some(from), false) => {
                out.push((from, c.epoch));
                open = None;
            }
            _ => {}
        }
    }
    if let Some(from) = open {
        out.push((from, stop));
    }
    out
}

/// Brent's root finder on `[a, b]`, `f(a)` and `f(b)` of opposite sign (as `> 0`).
fn brent(f: &mut impl FnMut(f64) -> f64, a: f64, b: f64, fa: f64, fb: f64, tol: f64) -> f64 {
    let (mut a, mut b, mut fa, mut fb) = (a, b, fa, fb);
    if fa.abs() < fb.abs() {
        (a, b, fa, fb) = (b, a, fb, fa);
    }
    let (mut c, mut fc) = (a, fa);
    let mut d = b - a;
    let mut bisected = true;
    for _ in 0..100 {
        if fb == 0.0 || (b - a).abs() <= tol {
            break;
        }
        let mut s = if fa != fc && fb != fc {
            a * fb * fc / ((fa - fb) * (fa - fc))
                + b * fa * fc / ((fb - fa) * (fb - fc))
                + c * fa * fb / ((fc - fa) * (fc - fb))
        } else {
            b - fb * (b - a) / (fb - fa)
        };
        let lo = (3.0 * a + b) / 4.0;
        let outside = !((s > lo.min(b)) && (s < lo.max(b)));
        let slow = if bisected {
            (s - b).abs() >= (b - c).abs() / 2.0 || (b - c).abs() < tol
        } else {
            (s - b).abs() >= (c - d).abs() / 2.0 || (c - d).abs() < tol
        };
        bisected = outside || slow;
        if bisected {
            s = (a + b) / 2.0;
        }
        let fs = f(s);
        (d, c, fc) = (c, b, fb);
        if (fa > 0.0) != (fs > 0.0) {
            (b, fb) = (s, fs);
        } else {
            (a, fa) = (s, fs);
        }
        if fa.abs() < fb.abs() {
            (a, b, fa, fb) = (b, a, fb, fa);
        }
    }
    b
}

/// Maximum of a unimodal `g` on `[a, b]` by golden-section search: `(epoch, g)`.
pub fn maximize(
    g: &mut impl FnMut(Epoch) -> f64,
    a: Epoch,
    b: Epoch,
    tolerance_s: f64,
) -> (Epoch, f64) {
    const R: f64 = 0.618_033_988_749_894_9;
    let mut at = |t: f64| g(a.advanced_by_seconds(t));
    let (mut lo, mut hi) = (0.0, b.seconds_since(a).max(0.0));
    let mut x1 = hi - R * (hi - lo);
    let mut x2 = lo + R * (hi - lo);
    let (mut f1, mut f2) = (at(x1), at(x2));
    while hi - lo > tolerance_s.max(1e-6) {
        if f1 < f2 {
            lo = x1;
            (x1, f1) = (x2, f2);
            x2 = lo + R * (hi - lo);
            f2 = at(x2);
        } else {
            hi = x2;
            (x2, f2) = (x1, f1);
            x1 = hi - R * (hi - lo);
            f1 = at(x1);
        }
    }
    let t = (lo + hi) / 2.0;
    (a.advanced_by_seconds(t), at(t))
}

// --- Ground stations ------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroundStation {
    pub name: String,
    /// Geodetic WGS-84, rad.
    pub latitude_rad: f64,
    pub longitude_rad: f64,
    pub altitude_km: f64,
    /// Visibility mask, rad.
    pub min_elevation_rad: f64,
    /// Horizon (terrain) mask: `(azimuth, elevation)`, rad, azimuth ascending in
    /// `[0, 2π)`; linear in azimuth, wrapping. Empty: none.
    pub horizon: Vec<(f64, f64)>,
}

/// Topocentric direction to a target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LookAngles {
    /// From north, clockwise, `[0, 2π)`.
    pub azimuth_rad: f64,
    pub elevation_rad: f64,
    pub range_km: f64,
}

impl GroundStation {
    pub fn position_itrf_km(&self) -> DVec3 {
        geodetic_to_ecef(self.latitude_rad, self.longitude_rad, self.altitude_km)
    }

    /// Visibility threshold toward `azimuth_rad`: the larger of `min_elevation_rad`
    /// and the horizon mask.
    pub fn min_elevation_at(&self, azimuth_rad: f64) -> f64 {
        use std::f64::consts::TAU;
        let mask = match self.horizon.as_slice() {
            [] => return self.min_elevation_rad,
            [(_, el)] => *el,
            points => {
                let az = azimuth_rad.rem_euclid(TAU);
                let i = points.partition_point(|p| p.0 <= az);
                let (a, b) = match i {
                    0 => (
                        (points[points.len() - 1].0 - TAU, points[points.len() - 1].1),
                        points[0],
                    ),
                    i if i == points.len() => (points[i - 1], (points[0].0 + TAU, points[0].1)),
                    i => (points[i - 1], points[i]),
                };
                let span = b.0 - a.0;
                if span > 0.0 {
                    a.1 + (b.1 - a.1) * (az - a.0) / span
                } else {
                    a.1.max(b.1)
                }
            }
        };
        mask.max(self.min_elevation_rad)
    }

    /// Look angles to `target` (ITRF, km), against the geodetic horizon.
    pub fn look_angles(&self, target_itrf_km: DVec3) -> LookAngles {
        let rho = target_itrf_km - self.position_itrf_km();
        let (sp, cp) = self.latitude_rad.sin_cos();
        let (sl, cl) = self.longitude_rad.sin_cos();
        let up = DVec3::new(cp * cl, cp * sl, sp);
        let east = DVec3::new(-sl, cl, 0.0);
        let north = DVec3::new(-sp * cl, -sp * sl, cp);
        let range = rho.length();
        LookAngles {
            azimuth_rad: rho
                .dot(east)
                .atan2(rho.dot(north))
                .rem_euclid(std::f64::consts::TAU),
            elevation_rad: (rho.dot(up) / range).clamp(-1.0, 1.0).asin(),
            range_km: range,
        }
    }
}

/// One pass above a station's mask.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    /// Acquisition of signal; the search start if already visible.
    pub aos: Epoch,
    /// Loss of signal; the search stop if still visible.
    pub los: Epoch,
    pub max_elevation_epoch: Epoch,
    pub max_elevation_rad: f64,
}

/// Passes of a target, position `itrf(epoch)` (km), above `station`'s mask and
/// horizon on `[start, stop]`.
pub fn contacts(
    station: &GroundStation,
    itrf: &mut impl FnMut(Epoch) -> DVec3,
    start: Epoch,
    stop: Epoch,
    step_s: f64,
    tolerance_s: f64,
) -> Vec<Contact> {
    let spans = intervals(
        &mut |e| {
            let look = station.look_angles(itrf(e));
            look.elevation_rad - station.min_elevation_at(look.azimuth_rad)
        },
        start,
        stop,
        step_s,
        tolerance_s,
    );
    spans
        .into_iter()
        .map(|(aos, los)| {
            let mut elevation = |e: Epoch| station.look_angles(itrf(e)).elevation_rad;
            let (max_elevation_epoch, max_elevation_rad) =
                maximize(&mut elevation, aos, los, tolerance_s);
            Contact {
                aos,
                los,
                max_elevation_epoch,
                max_elevation_rad,
            }
        })
        .collect()
}

// --- Eclipse --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shadow {
    /// Any part of the solar disc hidden (umbra included).
    Penumbra,
    /// Whole disc hidden.
    Umbra,
}

/// Conical shadow functions at `r`, occulting body of radius `body_radius_km` at the
/// origin, Sun at `sun` (km, same frame): `(penumbra, umbra)`, each `> 0` inside its
/// region. Montenbruck & Gill §3.4; an annular eclipse is penumbra only.
pub fn shadow_functions(r: DVec3, sun: DVec3, body_radius_km: f64) -> (f64, f64) {
    let to_sun = sun - r;
    let a = (SUN_MEAN_RADIUS_KM / to_sun.length()).min(1.0).asin();
    let b = (body_radius_km / r.length()).min(1.0).asin();
    let c = (-r).angle_between(to_sun);
    (a + b - c, b - a - c)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Eclipse {
    pub shadow: Shadow,
    /// Clipped to the search span.
    pub start: Epoch,
    pub stop: Epoch,
}

/// Penumbra and umbra spans on `[start, stop]`, in that order; `position` and `sun` are
/// relative to the occulting body (km, same frame).
pub fn eclipses(
    position: &mut impl FnMut(Epoch) -> DVec3,
    sun: &mut impl FnMut(Epoch) -> DVec3,
    body_radius_km: f64,
    start: Epoch,
    stop: Epoch,
    step_s: f64,
    tolerance_s: f64,
) -> Vec<Eclipse> {
    let mut out = Vec::new();
    for shadow in [Shadow::Penumbra, Shadow::Umbra] {
        let mut g = |e: Epoch| {
            let (p, u) = shadow_functions(position(e), sun(e), body_radius_km);
            if shadow == Shadow::Penumbra { p } else { u }
        };
        out.extend(
            intervals(&mut g, start, stop, step_s, tolerance_s)
                .into_iter()
                .map(|(start, stop)| Eclipse {
                    shadow,
                    start,
                    stop,
                }),
        );
    }
    out
}

// --- Apsides and nodes ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrbitEventKind {
    Periapsis,
    Apoapsis,
    AscendingNode,
    DescendingNode,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitEvent {
    pub kind: OrbitEventKind,
    pub epoch: Epoch,
}

/// Apsides (`r·v = 0`) and nodes (`z = 0`, the frame's equator) of `state(epoch)` on
/// `[start, stop]`, time-ordered.
pub fn orbit_events(
    state: &mut impl FnMut(Epoch) -> StateVector,
    apsides: bool,
    nodes: bool,
    start: Epoch,
    stop: Epoch,
    step_s: f64,
    tolerance_s: f64,
) -> Vec<OrbitEvent> {
    let mut out = Vec::new();
    if apsides {
        let mut g = |e: Epoch| {
            let s = state(e);
            s.position_km.dot(s.velocity_km_s)
        };
        let (_, found) = scan(&mut g, start, stop, step_s, tolerance_s);
        out.extend(found.into_iter().map(|c| OrbitEvent {
            kind: if c.rising {
                OrbitEventKind::Periapsis
            } else {
                OrbitEventKind::Apoapsis
            },
            epoch: c.epoch,
        }));
    }
    if nodes {
        let mut g = |e: Epoch| state(e).position_km.z;
        let (_, found) = scan(&mut g, start, stop, step_s, tolerance_s);
        out.extend(found.into_iter().map(|c| OrbitEvent {
            kind: if c.rising {
                OrbitEventKind::AscendingNode
            } else {
                OrbitEventKind::DescendingNode
            },
            epoch: c.epoch,
        }));
    }
    out.sort_by(|a, b| {
        a.epoch
            .partial_cmp(&b.epoch)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bodies::{ASTRONOMICAL_UNIT_KM, GM_EARTH_KM3_S2};
    use crate::earth::WGS84_A_KM;
    use std::f64::consts::{FRAC_PI_2, TAU};

    const T0: Epoch = Epoch::J2000;

    #[test]
    fn crossings_of_a_sine_are_refined_to_tolerance() {
        let mut g = |e: Epoch| (e.seconds_since(T0) * TAU / 100.0).sin();
        let (initial, found) = scan(&mut g, T0, T0.advanced_by_seconds(260.0), 7.0, 1e-9);
        assert!(!initial, "sin(0) = 0 is not > 0");
        let times: Vec<f64> = found.iter().map(|c| c.epoch.seconds_since(T0)).collect();
        // g(0) = 0 is not > 0, so leaving it counts as a crossing.
        assert_eq!(times.len(), 6, "{times:?}");
        for (t, want) in times.iter().zip([0.0, 50.0, 100.0, 150.0, 200.0, 250.0]) {
            assert!((t - want).abs() < 1e-6, "{t} vs {want}");
        }
        assert_eq!(
            found.iter().map(|c| c.rising).collect::<Vec<_>>(),
            [true, false, true, false, true, false]
        );
    }

    #[test]
    fn intervals_clip_open_ends() {
        let mut g = |e: Epoch| (e.seconds_since(T0) * TAU / 100.0 + 0.1).sin();
        let spans = intervals(&mut g, T0, T0.advanced_by_seconds(130.0), 5.0, 1e-9);
        let spans: Vec<(f64, f64)> = spans
            .iter()
            .map(|(a, b)| (a.seconds_since(T0), b.seconds_since(T0)))
            .collect();
        let shift = 0.1 * 100.0 / TAU;
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!(spans[0].0, 0.0);
        assert!((spans[0].1 - (50.0 - shift)).abs() < 1e-6);
        assert!((spans[1].0 - (100.0 - shift)).abs() < 1e-6);
        assert!((spans[1].1 - 130.0).abs() < 1e-9);
    }

    #[test]
    fn golden_section_finds_the_peak() {
        let mut g = |e: Epoch| -(e.seconds_since(T0) - 42.5).powi(2);
        let (at, value) = maximize(&mut g, T0, T0.advanced_by_seconds(100.0), 1e-6);
        assert!((at.seconds_since(T0) - 42.5).abs() < 1e-5);
        assert!(value.abs() < 1e-9);
    }

    #[test]
    fn look_angles_at_zenith_and_horizon() {
        let station = GroundStation {
            name: "Equator".into(),
            latitude_rad: 0.0,
            longitude_rad: 0.0,
            altitude_km: 0.0,
            min_elevation_rad: 0.0,
            horizon: Vec::new(),
        };
        let overhead = station.look_angles(DVec3::new(WGS84_A_KM + 500.0, 0.0, 0.0));
        assert!((overhead.elevation_rad - FRAC_PI_2).abs() < 1e-12);
        assert!((overhead.range_km - 500.0).abs() < 1e-9);
        // Due north along the horizon plane; due east.
        let north = station.look_angles(DVec3::new(WGS84_A_KM, 0.0, 1000.0));
        assert!(north.elevation_rad.abs() < 1e-12 && north.azimuth_rad.abs() < 1e-12);
        let east = station.look_angles(DVec3::new(WGS84_A_KM, 1000.0, 0.0));
        assert!((east.azimuth_rad - FRAC_PI_2).abs() < 1e-12);
    }

    /// Circular equatorial orbit over a non-rotating Earth: the pass over an equatorial
    /// station is symmetric about the overflight, and its half-width follows from the
    /// geometry of the mask.
    #[test]
    fn equatorial_pass_matches_geometry() {
        let radius = WGS84_A_KM + 700.0;
        let n = (GM_EARTH_KM3_S2 / radius.powi(3)).sqrt();
        let station = GroundStation {
            name: "Equator".into(),
            latitude_rad: 0.0,
            longitude_rad: 0.0,
            altitude_km: 0.0,
            min_elevation_rad: 10f64.to_radians(),
            horizon: Vec::new(),
        };
        // Overhead at t = 1000 s.
        let mut itrf = |e: Epoch| {
            let u = n * (e.seconds_since(T0) - 1000.0);
            DVec3::new(radius * u.cos(), radius * u.sin(), 0.0)
        };
        let found = contacts(
            &station,
            &mut itrf,
            T0,
            T0.advanced_by_seconds(3000.0),
            30.0,
            1e-6,
        );
        assert_eq!(found.len(), 1);
        let pass = found[0];
        // Earth-central half-angle: λ = acos(Re cos ε / r) − ε.
        let e = station.min_elevation_rad;
        let half = ((WGS84_A_KM * e.cos() / radius).acos() - e) / n;
        assert!((pass.aos.seconds_since(T0) - (1000.0 - half)).abs() < 1e-3);
        assert!((pass.los.seconds_since(T0) - (1000.0 + half)).abs() < 1e-3);
        assert!((pass.max_elevation_epoch.seconds_since(T0) - 1000.0).abs() < 1e-2);
        assert!((pass.max_elevation_rad - FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn horizon_mask_interpolates_and_wraps() {
        let d = f64::to_radians;
        let mut station = GroundStation {
            min_elevation_rad: d(5.0),
            horizon: vec![(d(0.0), d(10.0)), (d(180.0), d(30.0))],
            ..GroundStation::default()
        };
        for (az, el) in [
            (0.0, 10.0),
            (90.0, 20.0),
            (180.0, 30.0),
            (270.0, 20.0),
            (-90.0, 20.0),
        ] {
            assert!(
                (station.min_elevation_at(d(az)) - d(el)).abs() < 1e-12,
                "{az}"
            );
        }
        station.horizon = vec![(d(45.0), d(2.0))];
        assert_eq!(station.min_elevation_at(1.0), d(5.0));
    }

    /// The pass of [`equatorial_pass_matches_geometry`] rises over a 30° western ridge
    /// and sets against the 10° mask.
    #[test]
    fn horizon_mask_delays_acquisition() {
        let radius = WGS84_A_KM + 700.0;
        let n = (GM_EARTH_KM3_S2 / radius.powi(3)).sqrt();
        let station = GroundStation {
            min_elevation_rad: 10f64.to_radians(),
            horizon: vec![
                (90f64.to_radians(), 0.0),
                (270f64.to_radians(), 30f64.to_radians()),
            ],
            ..GroundStation::default()
        };
        let mut itrf = |e: Epoch| {
            let u = n * (e.seconds_since(T0) - 1000.0);
            DVec3::new(radius * u.cos(), radius * u.sin(), 0.0)
        };
        let found = contacts(
            &station,
            &mut itrf,
            T0,
            T0.advanced_by_seconds(3000.0),
            30.0,
            1e-6,
        );
        let half = |e: f64| ((WGS84_A_KM * e.cos() / radius).acos() - e) / n;
        assert_eq!(found.len(), 1);
        let aos = found[0].aos.seconds_since(T0);
        let los = found[0].los.seconds_since(T0);
        assert!(
            (aos - (1000.0 - half(30f64.to_radians()))).abs() < 1e-3,
            "{aos}"
        );
        assert!(
            (los - (1000.0 + half(10f64.to_radians()))).abs() < 1e-3,
            "{los}"
        );
    }

    #[test]
    fn shadow_regions_agree_with_srp_shadow_fraction() {
        let sun = DVec3::new(ASTRONOMICAL_UNIT_KM, 0.0, 0.0);
        let re = WGS84_A_KM;
        for (r, penumbra, umbra) in [
            (DVec3::new(7000.0, 0.0, 0.0), false, false),
            (DVec3::new(-7000.0, 0.0, 0.0), true, true),
            (DVec3::new(0.0, 7000.0, 0.0), false, false),
        ] {
            let (p, u) = shadow_functions(r, sun, re);
            assert_eq!((p > 0.0, u > 0.0), (penumbra, umbra), "{r}");
            let lit = crate::force::srp::shadow_fraction(r, sun, re);
            assert_eq!(lit < 1.0, penumbra);
        }
    }

    /// Circular equatorial LEO, Sun fixed on +x: one umbra per orbit, centred on the
    /// anti-Sun point, inside a slightly longer penumbra.
    #[test]
    fn eclipse_per_orbit() {
        let radius = WGS84_A_KM + 500.0;
        let n = (GM_EARTH_KM3_S2 / radius.powi(3)).sqrt();
        let period = TAU / n;
        let mut position = |e: Epoch| {
            let u = n * e.seconds_since(T0);
            DVec3::new(radius * u.cos(), radius * u.sin(), 0.0)
        };
        let mut sun = |_| DVec3::new(ASTRONOMICAL_UNIT_KM, 0.0, 0.0);
        let found = eclipses(
            &mut position,
            &mut sun,
            WGS84_A_KM,
            T0,
            T0.advanced_by_seconds(period),
            30.0,
            1e-6,
        );
        assert_eq!(
            found.iter().map(|e| e.shadow).collect::<Vec<_>>(),
            [Shadow::Penumbra, Shadow::Umbra]
        );
        let (pen, umb) = (found[0], found[1]);
        let mid = |e: &Eclipse| (e.start.seconds_since(T0) + e.stop.seconds_since(T0)) / 2.0;
        assert!((mid(&umb) - period / 2.0).abs() < 1e-3);
        assert!(pen.start < umb.start && pen.stop > umb.stop);
        // Cylindrical estimate of the umbra half-angle: asin(Re / r).
        let half = (WGS84_A_KM / radius).asin() / n;
        let width = umb.stop.seconds_since(umb.start);
        assert!(
            (width / 2.0 - half).abs() < 10.0,
            "{width} vs {}",
            2.0 * half
        );
    }

    #[test]
    fn apsides_and_nodes_of_an_inclined_ellipse() {
        let (rp, e, i): (f64, f64, f64) = (7000.0, 0.1, 0.5);
        let a = rp / (1.0 - e);
        let n = (GM_EARTH_KM3_S2 / a.powi(3)).sqrt();
        let period = TAU / n;
        // Kepler's equation; argument of periapsis 0, so periapsis is the ascending node.
        let mut state = |ep: Epoch| {
            let m = n * ep.seconds_since(T0);
            let mut big_e = m;
            for _ in 0..30 {
                big_e -= (big_e - e * big_e.sin() - m) / (1.0 - e * big_e.cos());
            }
            let (s, c) = big_e.sin_cos();
            let (x, y) = (a * (c - e), a * (1.0 - e * e).sqrt() * s);
            let rdot = n * a / (1.0 - e * c);
            let (vx, vy) = (-rdot * s, rdot * (1.0 - e * e).sqrt() * c);
            let (si, ci) = i.sin_cos();
            StateVector::new(
                DVec3::new(x, y * ci, y * si),
                DVec3::new(vx, vy * ci, vy * si),
            )
        };
        let start = T0.advanced_by_seconds(-0.1 * period);
        let found = orbit_events(
            &mut state,
            true,
            true,
            start,
            start.advanced_by_seconds(period),
            60.0,
            1e-6,
        );
        let kinds: Vec<OrbitEventKind> = found.iter().map(|e| e.kind).collect();
        assert_eq!(kinds.len(), 4, "{found:?}");
        let at = |kind| {
            found
                .iter()
                .find(|e| e.kind == kind)
                .unwrap()
                .epoch
                .seconds_since(T0)
        };
        assert!(at(OrbitEventKind::Periapsis).abs() < 1e-3);
        assert!(at(OrbitEventKind::AscendingNode).abs() < 1e-3);
        assert!((at(OrbitEventKind::Apoapsis) - period / 2.0).abs() < 1e-3);
        assert!((at(OrbitEventKind::DescendingNode) - period / 2.0).abs() < 1e-3);
    }
}
