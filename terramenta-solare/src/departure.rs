//! Converts a target hyperbolic excess velocity (v∞) into a state vector a
//! spacecraft can be spawned on.
//!
//! [`crate::mission::plan_transfer`] gives the velocity a spacecraft needs
//! at the departure body's location — a v∞ relative to that body, not a
//! state near its surface. [`injection_state`] builds the close-in
//! hyperbola that reaches that v∞ asymptotically; [`escape_injection_state`]
//! corrects for the sphere of influence being finite, not infinite (see its
//! own docs).
//!
//! The departure plane (which side of the body the hyperbola swings past) is
//! picked arbitrarily: any plane containing v∞ gives the same asymptotic
//! velocity, and nothing downstream depends on which one was chosen.

use glam::DVec3;

use crate::frame::StateVector;

/// Like [`injection_state`], but targets `escape_velocity_km_s` at
/// `sphere_of_influence_radius_km` — where [`crate::spacecraft::Spacecraft::update`]
/// actually reparents to the next body — rather than at infinity.
///
/// [`injection_state`] only reaches its target speed in the limit of
/// infinite range. At any finite radius, `v(r)² = v∞² + 2GM/r` (true for
/// any hyperbola of this energy, independent of periapsis) leaves a real
/// speed excess: direction has already converged by then, but a
/// percent-level speed error here compounds, over an interplanetary
/// transfer, into a miss many times the destination's sphere of influence.
/// This solves for the v∞ whose speed at `sphere_of_influence_radius_km`,
/// not at infinity, equals `escape_velocity_km_s`.
pub fn escape_injection_state(
    escape_velocity_km_s: DVec3,
    sphere_of_influence_radius_km: f64,
    periapsis_radius_km: f64,
    gm_km3_s2: f64,
) -> StateVector {
    let target_speed = escape_velocity_km_s.length();
    let asymptotic_speed_km_s =
        (target_speed * target_speed - 2.0 * gm_km3_s2 / sphere_of_influence_radius_km).sqrt();
    let v_infinity_km_s = escape_velocity_km_s.normalize() * asymptotic_speed_km_s;
    injection_state(v_infinity_km_s, periapsis_radius_km, gm_km3_s2)
}

/// State vector at periapsis of the hyperbola that departs with hyperbolic
/// excess velocity `v_infinity_km_s`, passing `periapsis_radius_km` from the
/// central body's centre — reached only in the limit of infinite range, not
/// at any real, finite sphere of influence; see [`escape_injection_state`]
/// for the version that corrects for that.
///
/// Standard patched-conics departure targeting (Vallado, *Fundamentals of
/// Astrodynamics and Applications*): eccentricity follows from
/// `periapsis_radius_km` and the hyperbola's specific energy (fixed by
/// `v_infinity_km_s`'s magnitude alone), which fixes the true anomaly of the
/// outgoing asymptote; periapsis sits that many degrees ahead of it, in
/// whichever plane contains `v_infinity_km_s`. Undefined for
/// `v_infinity_km_s == 0` (parabolic case), not handled specially, same as
/// the rest of this crate's orbits.
pub fn injection_state(
    v_infinity_km_s: DVec3,
    periapsis_radius_km: f64,
    gm_km3_s2: f64,
) -> StateVector {
    let v_inf = v_infinity_km_s.length();
    let v_inf_hat = v_infinity_km_s / v_inf;

    let eccentricity = 1.0 + periapsis_radius_km * v_inf * v_inf / gm_km3_s2;
    // True anomaly of the outgoing asymptote; periapsis (true anomaly 0) is
    // behind it by exactly this angle.
    let true_anomaly_at_infinity = (-1.0 / eccentricity).acos();
    let (sin_at_infinity, cos_at_infinity) = true_anomaly_at_infinity.sin_cos();

    // Any axis not parallel to v_inf_hat spans a valid departure plane with
    // it; this just avoids the one that would make the cross product below
    // degenerate.
    let reference = if v_inf_hat.z.abs() < 0.9 {
        DVec3::Z
    } else {
        DVec3::X
    };
    let normal = v_inf_hat.cross(reference).normalize();

    // Periapsis direction = v_inf_hat rotated -true_anomaly_at_infinity
    // about normal.
    let periapsis_hat = cos_at_infinity * v_inf_hat - sin_at_infinity * normal.cross(v_inf_hat);
    // Perpendicular to periapsis_hat, in-plane, in the direction of motion.
    let motion_hat = normal.cross(periapsis_hat);

    let periapsis_speed_km_s = (v_inf * v_inf + 2.0 * gm_km3_s2 / periapsis_radius_km).sqrt();

    StateVector::new(
        periapsis_radius_km * periapsis_hat,
        periapsis_speed_km_s * motion_hat,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orbit::OrbitalElements;

    const GM_EARTH_KM3_S2: f64 = 398_600.435_507;
    const PARKING_RADIUS_KM: f64 = 6_678.0; // Earth mean radius + 300 km.

    #[test]
    fn the_injection_state_sits_exactly_at_the_requested_periapsis() {
        let v_infinity = DVec3::new(2.9, 1.4, 0.6);
        let state = injection_state(v_infinity, PARKING_RADIUS_KM, GM_EARTH_KM3_S2);
        assert!(
            (state.position_km.length() - PARKING_RADIUS_KM).abs() < 1.0e-6,
            "{}",
            state.position_km.length()
        );
    }

    #[test]
    fn velocity_is_perpendicular_to_position_at_periapsis() {
        let v_infinity = DVec3::new(2.9, 1.4, 0.6);
        let state = injection_state(v_infinity, PARKING_RADIUS_KM, GM_EARTH_KM3_S2);
        let cos_angle = state.position_km.dot(state.velocity_km_s)
            / (state.position_km.length() * state.velocity_km_s.length());
        assert!(cos_angle.abs() < 1.0e-9, "{cos_angle}");
    }

    #[test]
    fn propagating_far_enough_out_recovers_the_requested_v_infinity() {
        let v_infinity = DVec3::new(2.9, 1.4, 0.6);
        let state = injection_state(v_infinity, PARKING_RADIUS_KM, GM_EARTH_KM3_S2);
        let elements = OrbitalElements::from_state(state, GM_EARTH_KM3_S2, 0.0);
        assert!(elements.eccentricity > 1.0, "{elements:?}");

        // Error falls off as ~GM / (v_inf^2 * t) once far from periapsis —
        // slow for this wide, low-v_inf hyperbola, so it takes months, not
        // days, to close within a few m/s.
        let far = elements.state_at(GM_EARTH_KM3_S2, 90.0 * 86_400.0);
        let error = (far.velocity_km_s - v_infinity).length();
        assert!(error < 1.0e-2, "{error} km/s off after 90 days");
    }

    #[test]
    fn escape_injection_state_matches_the_target_speed_at_the_soi_crossing() {
        use crate::bodies::{ASTRONOMICAL_UNIT_KM, GM_SUN_KM3_S2, sphere_of_influence_km};

        // Regression guard: plain `injection_state` for this v_infinity
        // overshoots the speed at Earth's SOI by ~130 m/s (~4%), enough to
        // miss an interplanetary arrival by tens of millions of km.
        let escape_velocity = DVec3::new(2.9, 1.4, 0.6);
        let soi = sphere_of_influence_km(GM_EARTH_KM3_S2, GM_SUN_KM3_S2, ASTRONOMICAL_UNIT_KM);
        let state =
            escape_injection_state(escape_velocity, soi, PARKING_RADIUS_KM, GM_EARTH_KM3_S2);
        let elements = OrbitalElements::from_state(state, GM_EARTH_KM3_S2, 0.0);

        let crossing_seconds = time_of_radius(&elements, GM_EARTH_KM3_S2, soi);
        let at_crossing = elements.state_at(GM_EARTH_KM3_S2, crossing_seconds);

        let speed_error = (at_crossing.velocity_km_s.length() - escape_velocity.length()).abs();
        assert!(speed_error < 1.0e-3, "{speed_error} km/s off at the SOI");
    }

    /// Bisects for the first time this hyperbola's radius crosses
    /// `radius_km`, mirroring [`crate::spacecraft::Spacecraft::try_escape`]'s
    /// own SOI check without a tree/primary chain.
    fn time_of_radius(elements: &OrbitalElements, gm_km3_s2: f64, radius_km: f64) -> f64 {
        let (mut too_early, mut too_late) = (0.0_f64, 30.0 * 86_400.0);
        for _ in 0..80 {
            let mid = (too_early + too_late) / 2.0;
            let r = elements.state_at(gm_km3_s2, mid).position_km.length();
            if r < radius_km {
                too_early = mid;
            } else {
                too_late = mid;
            }
        }
        too_late
    }

    #[test]
    fn works_when_v_infinity_is_nearly_parallel_to_the_fallback_reference_axis() {
        // Forces the fallback from the Z reference axis to X.
        let v_infinity = DVec3::new(0.01, 0.02, 3.2);
        let state = injection_state(v_infinity, PARKING_RADIUS_KM, GM_EARTH_KM3_S2);
        assert!(state.position_km.is_finite(), "{state:?}");
        assert!(state.velocity_km_s.is_finite(), "{state:?}");
        assert!((state.position_km.length() - PARKING_RADIUS_KM).abs() < 1.0e-6);
    }
}
