//! Generates `docs/earth_to_mars.svg` for this crate's README: an animated
//! plot of a spacecraft followed the way
//! [`terramenta_solare::spacecraft::Spacecraft::update`] actually flies one —
//! a heliocentric cruise on a Lambert-solved transfer ellipse from Earth's
//! position at departure, propagated day by day until it crosses into Mars'
//! sphere of influence and is captured, with no jump in position at the
//! hand-off. A marker rides the transfer arc from the launch pulse at Earth
//! to the landing pulse at Mars, while Earth and Mars themselves crawl along
//! their own orbit rings at the pace the cruise actually took. The departure
//! burn is treated as instantaneous, the same simplification
//! [`terramenta_solare::mission::plan_transfer`] documents: a
//! heliocentric-only trip, silent about escaping Earth's own gravity well,
//! same as that function's delta-v. Run with:
//!
//! ```sh
//! cargo run --example earth_to_mars -p terramenta-solare
//! ```

use std::fmt::Write as _;

use glam::DVec3;

use terramenta_solare::bodies::GM_SUN_KM3_S2;
use terramenta_solare::lambert::{self, TransferDirection};
use terramenta_solare::spacecraft::{Primary, Spacecraft};
use terramenta_solare::{Epoch, FrameId, FrameTree, StateVector, solar_system};

const DAY_SECONDS: f64 = 86_400.0;
const EARTH_ORBIT_PERIOD_DAYS: f64 = 365.25;
const MARS_ORBIT_PERIOD_DAYS: f64 = 686.98;
/// How far past the Lambert-solved arrival date to keep following the
/// spacecraft, in case Mars' capture happens a little after it.
const ARRIVAL_MARGIN_DAYS: f64 = 30.0;

const WIDTH: f64 = 760.0;
const HEIGHT: f64 = 760.0;
const MARGIN: f64 = 46.0;
const ORBIT_SAMPLE_STEPS: usize = 180;
/// One full loop of the animation, in wall-clock seconds — arbitrary, chosen
/// purely for a watchable pace, not tied to the mission's real duration.
const ANIMATION_LOOP_SECONDS: f64 = 10.0;

fn main() {
    let mut tree = solar_system();
    let (sun, earth, mars) = (
        tree.find("Sun").unwrap(),
        tree.find("Earth").unwrap(),
        tree.find("Mars").unwrap(),
    );

    // The same near-optimal Earth-Mars window `mission::plan_transfer`'s
    // own test uses — Earth and Mars only line up for a cheap transfer
    // roughly once a synodic period (~780 days).
    let departure = Epoch::J2000.advanced_by_seconds(1_253.0 * DAY_SECONDS);
    let arrival = departure.advanced_by_seconds(204.0 * DAY_SECONDS);

    let earth_at_departure = tree.state_of_relative_to(earth, sun, departure);
    let mars_at_arrival = tree.state_of_relative_to(mars, sun, arrival);

    let transfer = lambert::solve(
        earth_at_departure.position_km,
        mars_at_arrival.position_km,
        arrival.to_unix_seconds() - departure.to_unix_seconds(),
        GM_SUN_KM3_S2,
        TransferDirection::Prograde,
    )
    .expect("a real Earth-Mars launch window should solve");

    let departure_delta_v_km_s =
        (transfer.velocity_at_departure_km_s - earth_at_departure.velocity_km_s).length();
    let arrival_delta_v_km_s =
        (mars_at_arrival.velocity_km_s - transfer.velocity_at_arrival_km_s).length();

    // Heliocentric from the start — [`Primary::sun`] has nothing to escape
    // to ([`Primary::orbits`] is `None`), so the only hand-off
    // [`Spacecraft::update`] can make from here is inward, into Mars'
    // sphere of influence.
    let sun_with_mars_capture =
        Primary::sun(sun).with_capture_candidates(vec![Primary::mars(mars, sun)]);

    let mut spacecraft = Spacecraft::spawn(
        &mut tree,
        "Voyager",
        sun_with_mars_capture,
        StateVector::new(
            earth_at_departure.position_km,
            transfer.velocity_at_departure_km_s,
        ),
        departure,
    );

    let total_hours = ((arrival.to_unix_seconds() - departure.to_unix_seconds()) / 3_600.0
        + ARRIVAL_MARGIN_DAYS * 24.0)
        .round() as u32;

    let mut capture: Option<Epoch> = None;
    let mut trajectory_km: Vec<DVec3> = vec![earth_at_departure.position_km];
    // Earth's and Mars' own positions, sampled at the same cadence as the
    // trajectory, so the animation can show them advancing along their
    // orbits at the pace the cruise actually took rather than sitting still.
    let mut earth_track_km: Vec<DVec3> = vec![earth_at_departure.position_km];
    let mut mars_track_km: Vec<DVec3> =
        vec![tree.state_of_relative_to(mars, sun, departure).position_km];

    for hour in 1..=total_hours {
        let epoch = departure.advanced_by_seconds(hour as f64 * 3_600.0);
        if spacecraft.update(&mut tree, epoch) {
            capture = Some(epoch);
        }
        if hour % 24 == 0 || capture.is_some() {
            trajectory_km.push(
                tree.state_of_relative_to(spacecraft.frame, sun, epoch)
                    .position_km,
            );
            earth_track_km.push(tree.state_of_relative_to(earth, sun, epoch).position_km);
            mars_track_km.push(tree.state_of_relative_to(mars, sun, epoch).position_km);
        }
        if capture.is_some() {
            break;
        }
    }

    let capture =
        capture.expect("should be captured by Mars' sphere of influence within the window");

    println!(
        "Departure {} \u{2192} arrival {} ({:.0} days of flight)",
        iso_date(departure.to_unix_seconds()),
        iso_date(arrival.to_unix_seconds()),
        (arrival.to_unix_seconds() - departure.to_unix_seconds()) / DAY_SECONDS
    );
    println!(
        "Departure burn {departure_delta_v_km_s:.3} km/s, arrival burn {arrival_delta_v_km_s:.3} km/s \
         (heliocentric only — no Earth-departure or Mars-capture burn included)"
    );
    println!(
        "Captured by Mars' sphere of influence on {} ({:.1} days after departure)",
        iso_date(capture.to_unix_seconds()),
        (capture.to_unix_seconds() - departure.to_unix_seconds()) / DAY_SECONDS
    );

    let earth_orbit_km = sample_orbit(&tree, earth, sun, departure, EARTH_ORBIT_PERIOD_DAYS);
    let mars_orbit_km = sample_orbit(&tree, mars, sun, departure, MARS_ORBIT_PERIOD_DAYS);

    let svg = render_svg(
        &earth_orbit_km,
        &mars_orbit_km,
        &trajectory_km,
        &earth_track_km,
        &mars_track_km,
    );
    std::fs::create_dir_all("docs").expect("docs/ should be creatable");
    std::fs::write("docs/earth_to_mars.svg", &svg)
        .expect("docs/earth_to_mars.svg should be writable");
    println!("Wrote docs/earth_to_mars.svg");
}

/// `body`'s heliocentric position at `steps` points spaced across one full
/// `period_days` starting at `anchor` — enough to trace a closed orbit ring
/// regardless of what phase `anchor` catches the body at.
fn sample_orbit(
    tree: &FrameTree,
    body: FrameId,
    sun: FrameId,
    anchor: Epoch,
    period_days: f64,
) -> Vec<DVec3> {
    (0..=ORBIT_SAMPLE_STEPS)
        .map(|step| {
            let epoch = anchor.advanced_by_seconds(
                period_days * DAY_SECONDS * step as f64 / ORBIT_SAMPLE_STEPS as f64,
            );
            tree.state_of_relative_to(body, sun, epoch).position_km
        })
        .collect()
}

fn render_svg(
    earth_orbit_km: &[DVec3],
    mars_orbit_km: &[DVec3],
    trajectory_km: &[DVec3],
    earth_track_km: &[DVec3],
    mars_track_km: &[DVec3],
) -> String {
    let max_radius_km = earth_orbit_km
        .iter()
        .chain(mars_orbit_km)
        .chain(trajectory_km)
        .map(|p| p.length())
        .fold(0.0_f64, f64::max);
    let plot_radius = WIDTH.min(HEIGHT) / 2.0 - MARGIN;
    let scale = plot_radius / max_radius_km;
    let (center_x, center_y) = (WIDTH / 2.0, HEIGHT / 2.0);
    // Viewed from the north ecliptic pole, the same sense
    // `TransferDirection::Prograde` is defined in — x/y plotted directly,
    // z (out of the ecliptic) dropped.
    let to_point = |p: DVec3| (center_x + p.x * scale, center_y - p.y * scale);

    let mut svg = String::new();
    let _ = writeln!(
        svg,
        "<!--\n  Terramenta - a heliocentric Earth-to-Mars trip: Spacecraft::update\n  \
         followed on a Lambert-solved transfer ellipse from Earth's position at\n  \
         departure until it crosses into Mars' own sphere of influence and is\n  \
         captured, with no jump in position at the hand-off. Earth, Mars, and the\n  \
         spacecraft marker are animated along the tracks actually sampled during\n  \
         the cruise.\n\n  Generated by the `earth_to_mars` example in this crate \
         (see its own README); do not edit by hand.\n-->"
    );
    let _ = writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {HEIGHT}" width="{WIDTH}" height="{HEIGHT}" font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, monospace">"#
    );
    let _ = writeln!(
        svg,
        r##"<rect width="{WIDTH}" height="{HEIGHT}" fill="#06121f"/>"##
    );

    write_ring(&mut svg, earth_orbit_km, to_point, "rgba(120,170,255,0.45)");
    write_ring(&mut svg, mars_orbit_km, to_point, "rgba(255,140,90,0.45)");
    write_path(&mut svg, trajectory_km, to_point, "#f4d35e");

    let (sun_x, sun_y) = to_point(DVec3::ZERO);
    let _ = writeln!(
        svg,
        r##"<circle cx="{sun_x:.2}" cy="{sun_y:.2}" r="7" fill="#ffd23f"/>"##
    );

    let earth_departure_km = earth_track_km[0];
    let mars_capture_km = *mars_track_km.last().expect("mars_track_km is non-empty");

    write_traveling_marker(&mut svg, earth_track_km, to_point, "#78aaff", 5.0);
    write_traveling_marker(&mut svg, mars_track_km, to_point, "#ff8c5a", 5.0);
    write_traveling_marker(&mut svg, trajectory_km, to_point, "#f4d35e", 4.0);

    // A launch flash at Earth's position at departure and a landing flash at
    // Mars' position at capture, both timed to the same loop as the markers
    // above so they fire right as the spacecraft marker reaches each end.
    write_launch_pulse(&mut svg, to_point(earth_departure_km), "#cfe0ff");
    write_landing_pulse(&mut svg, to_point(mars_capture_km), "#ffd9c2");

    let (earth_x, earth_y) = to_point(earth_departure_km);
    let _ = writeln!(
        svg,
        r##"<text x="{:.2}" y="{:.2}" fill="#cfe0ff" font-size="12">Earth, launch</text>"##,
        earth_x + 9.0,
        earth_y - 9.0
    );

    let (mars_x, mars_y) = to_point(mars_capture_km);
    let _ = writeln!(
        svg,
        r##"<text x="{:.2}" y="{:.2}" fill="#cfe0ff" font-size="12">Mars, landing</text>"##,
        mars_x + 9.0,
        mars_y - 9.0
    );

    let _ = writeln!(
        svg,
        r##"<text x="{:.2}" y="24" fill="#cfe0ff" font-size="13" text-anchor="middle">Earth → Mars: a Lambert-solved transfer, flown patched-conics</text>"##,
        WIDTH / 2.0
    );
    let _ = writeln!(
        svg,
        r##"<text x="{:.2}" y="{:.2}" fill="rgba(207,224,255,0.55)" font-size="11" text-anchor="middle">Yellow: spacecraft · blue: Earth · orange: Mars, all riding their tracks from launch to landing</text>"##,
        WIDTH / 2.0,
        HEIGHT - 14.0
    );

    let _ = writeln!(svg, "</svg>");
    svg
}

/// A marker that rides `track_km` once per [`ANIMATION_LOOP_SECONDS`], then
/// snaps back to the start and repeats — the sampled points are assumed
/// evenly spaced in time, so no explicit `keyTimes` are needed.
fn write_traveling_marker(
    svg: &mut String,
    track_km: &[DVec3],
    to_point: impl Fn(DVec3) -> (f64, f64),
    color: &str,
    radius: f64,
) {
    let points: Vec<(f64, f64)> = track_km.iter().map(|&p| to_point(p)).collect();
    let xs = points
        .iter()
        .map(|(x, _)| format!("{x:.2}"))
        .collect::<Vec<_>>()
        .join(";");
    let ys = points
        .iter()
        .map(|(_, y)| format!("{y:.2}"))
        .collect::<Vec<_>>()
        .join(";");
    let _ = writeln!(svg, r##"<circle r="{radius}" fill="{color}">"##);
    let _ = writeln!(
        svg,
        r#"<animate attributeName="cx" dur="{ANIMATION_LOOP_SECONDS}s" repeatCount="indefinite" values="{xs}"/>"#
    );
    let _ = writeln!(
        svg,
        r#"<animate attributeName="cy" dur="{ANIMATION_LOOP_SECONDS}s" repeatCount="indefinite" values="{ys}"/>"#
    );
    let _ = writeln!(svg, "</circle>");
}

/// An expanding, fading ring flashed right at the start of each loop — the
/// launch, timed to land right as the traveling markers leave this point.
fn write_launch_pulse(svg: &mut String, (x, y): (f64, f64), color: &str) {
    write_pulse(svg, (x, y), color, "0;0.08;1", "4;20;4", "0.9;0;0");
}

/// An expanding, fading ring flashed right at the end of each loop — the
/// landing, timed to land right as the traveling markers reach this point.
fn write_landing_pulse(svg: &mut String, (x, y): (f64, f64), color: &str) {
    write_pulse(svg, (x, y), color, "0;0.92;1", "4;4;20", "0;0;0.9");
}

fn write_pulse(
    svg: &mut String,
    (x, y): (f64, f64),
    color: &str,
    key_times: &str,
    radius_values: &str,
    opacity_values: &str,
) {
    let _ = writeln!(
        svg,
        r##"<circle cx="{x:.2}" cy="{y:.2}" r="4" fill="none" stroke="{color}" stroke-width="2">"##
    );
    let _ = writeln!(
        svg,
        r#"<animate attributeName="r" dur="{ANIMATION_LOOP_SECONDS}s" repeatCount="indefinite" keyTimes="{key_times}" values="{radius_values}" calcMode="linear"/>"#
    );
    let _ = writeln!(
        svg,
        r#"<animate attributeName="opacity" dur="{ANIMATION_LOOP_SECONDS}s" repeatCount="indefinite" keyTimes="{key_times}" values="{opacity_values}" calcMode="linear"/>"#
    );
    let _ = writeln!(svg, "</circle>");
}

fn write_ring(
    svg: &mut String,
    points_km: &[DVec3],
    to_point: impl Fn(DVec3) -> (f64, f64),
    color: &str,
) {
    write_polyline(svg, points_km, to_point, color, 1.1, "none");
}

fn write_path(
    svg: &mut String,
    points_km: &[DVec3],
    to_point: impl Fn(DVec3) -> (f64, f64),
    color: &str,
) {
    write_polyline(svg, points_km, to_point, color, 2.2, "5,4");
}

fn write_polyline(
    svg: &mut String,
    points_km: &[DVec3],
    to_point: impl Fn(DVec3) -> (f64, f64),
    color: &str,
    stroke_width: f64,
    dash: &str,
) {
    let mut path = String::new();
    for (index, &point_km) in points_km.iter().enumerate() {
        let (x, y) = to_point(point_km);
        let command = if index == 0 { "M" } else { "L" };
        let _ = write!(path, "{command}{x:.2},{y:.2} ");
    }
    let dash_attr = if dash == "none" {
        String::new()
    } else {
        format!(r#" stroke-dasharray="{dash}""#)
    };
    let _ = writeln!(
        svg,
        r#"<path d="{path}" fill="none" stroke="{color}" stroke-width="{stroke_width}"{dash_attr}/>"#
    );
}

fn iso_date(unix_seconds: f64) -> String {
    let (year, month, day) = civil_from_days((unix_seconds / DAY_SECONDS).floor() as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the Unix epoch to a proleptic Gregorian date — Howard
/// Hinnant's `civil_from_days` algorithm, chosen over a date-handling
/// dependency for the one thing this docs-generating example needs once:
/// printed dates.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
