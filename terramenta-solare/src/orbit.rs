//! Two-body Keplerian orbital elements: the propagator a spacecraft uses
//! between the moments its trajectory is re-planned, and the model every
//! planet's own heliocentric position is stated in.
//!
//! This is the restricted two-body problem — one central mass, one massless
//! body, no perturbation from anything else. Exact for a planet's motion
//! around the Sun to the precision the tables in [`crate::planets`] use, and
//! exact for a spacecraft for as long as one body's gravity dominates it —
//! the premise [`crate::spacecraft`]'s sphere-of-influence switch relies on.
//! No perturbation term is needed because nothing here models a transfer
//! under two bodies' gravity at once.

use glam::DVec3;

use crate::frame::StateVector;

/// Classical orbital elements, plus the mean anomaly at a stated epoch — the
/// six numbers a two-body orbit needs to be evaluated at any other time.
///
/// Angles are radians throughout. `eccentricity < 1.0` is an ellipse and
/// `> 1.0` a hyperbola; `== 1.0` (a parabola) is a measure-zero case this
/// does not handle specially and will misbehave on. Same for a circular
/// (`eccentricity == 0.0`) or equatorial (`inclination == 0.0`) orbit, where
/// the argument of periapsis and the ascending node stop being well-defined.
/// None of the orbits this crate constructs land exactly on those cases.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitalElements {
    pub semi_major_axis_km: f64,
    pub eccentricity: f64,
    pub inclination_rad: f64,
    pub raan_rad: f64,
    pub arg_periapsis_rad: f64,
    /// Mean anomaly at `epoch_seconds`. For a hyperbolic orbit this is the
    /// hyperbolic mean anomaly `e sinh(H) - H`, unwrapped rather than an
    /// angle — a hyperbolic trajectory passes periapsis once and never
    /// repeats, so there is no `2π` to wrap it into.
    pub mean_anomaly_at_epoch: f64,
    pub epoch_seconds: f64,
}

impl OrbitalElements {
    /// The semi-latus rectum, `a(1 - e²)` — positive for both an ellipse and a
    /// hyperbola, since a hyperbola's `a` is negative by convention.
    fn semi_latus_rectum_km(&self) -> f64 {
        self.semi_major_axis_km * (1.0 - self.eccentricity * self.eccentricity)
    }

    /// Mean motion: how fast mean anomaly advances, in radians per second.
    fn mean_motion(&self, gm_km3_s2: f64) -> f64 {
        (gm_km3_s2 / self.semi_major_axis_km.abs().powi(3)).sqrt()
    }

    /// This orbit's state at a given time, relative to the body `gm_km3_s2`
    /// belongs to.
    pub fn state_at(&self, gm_km3_s2: f64, unix_seconds: f64) -> StateVector {
        let dt = unix_seconds - self.epoch_seconds;
        let mean_anomaly = self.mean_anomaly_at_epoch + self.mean_motion(gm_km3_s2) * dt;

        let true_anomaly = if self.eccentricity < 1.0 {
            let eccentric = solve_kepler_elliptical(wrap_radians(mean_anomaly), self.eccentricity);
            true_anomaly_from_eccentric(eccentric, self.eccentricity)
        } else {
            let hyperbolic = solve_kepler_hyperbolic(mean_anomaly, self.eccentricity);
            true_anomaly_from_hyperbolic(hyperbolic, self.eccentricity)
        };

        self.state_at_true_anomaly(gm_km3_s2, true_anomaly)
    }

    /// This orbit's state at a given true anomaly, sidestepping time and
    /// Kepler's equation entirely — the shape of the orbit, evaluated at a
    /// point on it.
    ///
    /// Used to place a body the moment its elements are set (`dt == 0` would
    /// do the same through [`OrbitalElements::state_at`]), and to check
    /// [`OrbitalElements::from_state`]'s round trip.
    fn state_at_true_anomaly(&self, gm_km3_s2: f64, true_anomaly: f64) -> StateVector {
        let p = self.semi_latus_rectum_km();
        let r = p / (1.0 + self.eccentricity * true_anomaly.cos());
        let h = (gm_km3_s2 * p).sqrt();

        // Position and velocity in the perifocal frame: periapsis on the
        // +x axis, the orbit's own angular momentum on +z.
        let (sin_nu, cos_nu) = true_anomaly.sin_cos();
        let position_pqw = DVec3::new(r * cos_nu, r * sin_nu, 0.0);
        let velocity_pqw = DVec3::new(
            -(gm_km3_s2 / h) * sin_nu,
            (gm_km3_s2 / h) * (self.eccentricity + cos_nu),
            0.0,
        );

        let (position, velocity) = perifocal_to_reference(
            position_pqw,
            velocity_pqw,
            self.raan_rad,
            self.inclination_rad,
            self.arg_periapsis_rad,
        );
        StateVector::new(position, velocity)
    }

    /// Derives the elements a state vector is instantaneously on, at the
    /// epoch that state was measured. Needed when a spacecraft crosses a
    /// sphere of influence: [`crate::frame::FrameTree::reparent`] gives it a
    /// state relative to its new parent but no elements to keep propagating
    /// with.
    pub fn from_state(state: StateVector, gm_km3_s2: f64, epoch_seconds: f64) -> Self {
        let (r, v) = (state.position_km, state.velocity_km_s);
        let r_mag = r.length();

        let h_vec = r.cross(v);
        let h = h_vec.length();
        let node_vec = DVec3::Z.cross(h_vec);
        let node_mag = node_vec.length();

        let e_vec = v.cross(h_vec) / gm_km3_s2 - r / r_mag;
        let eccentricity = e_vec.length();

        let specific_energy = v.length_squared() / 2.0 - gm_km3_s2 / r_mag;
        let semi_major_axis_km = -gm_km3_s2 / (2.0 * specific_energy);

        let inclination_rad = (h_vec.z / h).clamp(-1.0, 1.0).acos();

        let raan_rad = {
            let raan = (node_vec.x / node_mag).clamp(-1.0, 1.0).acos();
            if node_vec.y < 0.0 {
                std::f64::consts::TAU - raan
            } else {
                raan
            }
        };

        let arg_periapsis_rad = {
            let cos_argp = (node_vec.dot(e_vec) / (node_mag * eccentricity)).clamp(-1.0, 1.0);
            let argp = cos_argp.acos();
            if e_vec.z < 0.0 {
                std::f64::consts::TAU - argp
            } else {
                argp
            }
        };

        let true_anomaly = {
            let cos_nu = (e_vec.dot(r) / (eccentricity * r_mag)).clamp(-1.0, 1.0);
            let nu = cos_nu.acos();
            if r.dot(v) < 0.0 { -nu } else { nu }
        };

        let mean_anomaly_at_epoch = if eccentricity < 1.0 {
            let eccentric = eccentric_from_true_anomaly(true_anomaly, eccentricity);
            eccentric - eccentricity * eccentric.sin()
        } else {
            let hyperbolic = hyperbolic_from_true_anomaly(true_anomaly, eccentricity);
            eccentricity * hyperbolic.sinh() - hyperbolic
        };

        Self {
            semi_major_axis_km,
            eccentricity,
            inclination_rad,
            raan_rad,
            arg_periapsis_rad,
            mean_anomaly_at_epoch,
            epoch_seconds,
        }
    }
}

/// Rotates a perifocal (periapsis-on-`x`, orbit-normal-on-`z`) position and
/// velocity into the frame the elements' angles are measured in — Vallado's
/// PQW-to-IJK transform, applied to both vectors since it is a pure rotation.
fn perifocal_to_reference(
    position_pqw: DVec3,
    velocity_pqw: DVec3,
    raan_rad: f64,
    inclination_rad: f64,
    arg_periapsis_rad: f64,
) -> (DVec3, DVec3) {
    let (sin_raan, cos_raan) = raan_rad.sin_cos();
    let (sin_inc, cos_inc) = inclination_rad.sin_cos();
    let (sin_argp, cos_argp) = arg_periapsis_rad.sin_cos();

    let r11 = cos_raan * cos_argp - sin_raan * sin_argp * cos_inc;
    let r12 = -cos_raan * sin_argp - sin_raan * cos_argp * cos_inc;
    let r21 = sin_raan * cos_argp + cos_raan * sin_argp * cos_inc;
    let r22 = -sin_raan * sin_argp + cos_raan * cos_argp * cos_inc;
    let r31 = sin_argp * sin_inc;
    let r32 = cos_argp * sin_inc;

    let rotate = |v: DVec3| {
        DVec3::new(
            r11 * v.x + r12 * v.y,
            r21 * v.x + r22 * v.y,
            r31 * v.x + r32 * v.y,
        )
    };
    (rotate(position_pqw), rotate(velocity_pqw))
}

/// Solves `M = E - e sin(E)` for the eccentric anomaly, by Newton's method
/// from the standard first-order guess.
fn solve_kepler_elliptical(mean_anomaly: f64, eccentricity: f64) -> f64 {
    let mut eccentric = mean_anomaly + eccentricity * mean_anomaly.sin();
    for _ in 0..8 {
        let error = eccentric - eccentricity * eccentric.sin() - mean_anomaly;
        eccentric -= error / (1.0 - eccentricity * eccentric.cos());
    }
    eccentric
}

/// Solves `M = e sinh(H) - H` for the hyperbolic anomaly.
///
/// The initial guess is the asymptotic inverse of `sinh` (Vallado's), because
/// unlike the elliptical mean anomaly this one is not wrapped to a small
/// range — a spacecraft an hour into a departure hyperbola can already be at
/// a mean anomaly of several dozen radians, where starting Newton's method
/// from `H = M` converges only linearly and takes dozens of steps to catch up.
fn solve_kepler_hyperbolic(mean_anomaly: f64, eccentricity: f64) -> f64 {
    let mut hyperbolic =
        mean_anomaly.signum() * ((2.0 * mean_anomaly.abs() / eccentricity) + 1.8).ln();
    for _ in 0..50 {
        let error = eccentricity * hyperbolic.sinh() - hyperbolic - mean_anomaly;
        hyperbolic -= error / (eccentricity * hyperbolic.cosh() - 1.0);
    }
    hyperbolic
}

fn true_anomaly_from_eccentric(eccentric: f64, eccentricity: f64) -> f64 {
    2.0 * ((1.0 + eccentricity).sqrt() * (eccentric / 2.0).sin())
        .atan2((1.0 - eccentricity).sqrt() * (eccentric / 2.0).cos())
}

fn eccentric_from_true_anomaly(true_anomaly: f64, eccentricity: f64) -> f64 {
    2.0 * ((1.0 - eccentricity).sqrt() * (true_anomaly / 2.0).sin())
        .atan2((1.0 + eccentricity).sqrt() * (true_anomaly / 2.0).cos())
}

fn true_anomaly_from_hyperbolic(hyperbolic: f64, eccentricity: f64) -> f64 {
    2.0 * ((eccentricity + 1.0).sqrt() * (hyperbolic / 2.0).sinh())
        .atan2((eccentricity - 1.0).sqrt() * (hyperbolic / 2.0).cosh())
}

fn hyperbolic_from_true_anomaly(true_anomaly: f64, eccentricity: f64) -> f64 {
    let x = ((eccentricity - 1.0) / (eccentricity + 1.0)).sqrt() * (true_anomaly / 2.0).tan();
    2.0 * x.atanh()
}

/// Folds an angle into `-π..π`, which is where Newton's method for the
/// elliptical case converges fastest and most reliably.
fn wrap_radians(radians: f64) -> f64 {
    (radians + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Earth's own, close enough for these self-consistency checks.
    const GM_SUN_KM3_S2: f64 = 1.327_124_4e11;
    const GM_EARTH_KM3_S2: f64 = 398_600.441_8;

    fn earth_like_orbit() -> OrbitalElements {
        OrbitalElements {
            semi_major_axis_km: 1.496e8,
            eccentricity: 0.0167,
            inclination_rad: 0.0,
            raan_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_at_epoch: 0.0,
            epoch_seconds: 0.0,
        }
    }

    #[test]
    fn a_circular_equatorial_orbit_starts_on_the_x_axis_moving_in_y() {
        let elements = OrbitalElements {
            semi_major_axis_km: 7000.0,
            eccentricity: 0.0,
            inclination_rad: 0.0,
            raan_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_at_epoch: 0.0,
            epoch_seconds: 0.0,
        };
        let state = elements.state_at(GM_EARTH_KM3_S2, 0.0);
        assert!((state.position_km - DVec3::new(7000.0, 0.0, 0.0)).length() < 1.0e-6);
        assert!(state.velocity_km_s.x.abs() < 1.0e-9);
        assert!(state.velocity_km_s.y > 0.0);
        assert!(state.velocity_km_s.z.abs() < 1.0e-9);
    }

    #[test]
    fn a_polar_orbit_reaches_the_pole_a_quarter_period_after_the_node() {
        // Ascending node on +x (RAAN = 0, argument of periapsis = 0), 90°
        // inclination: a quarter of the way round from periapsis is a quarter
        // of the way from the equator to the pole, which for a polar orbit
        // is the pole itself.
        let elements = OrbitalElements {
            semi_major_axis_km: 7000.0,
            eccentricity: 0.0,
            inclination_rad: std::f64::consts::FRAC_PI_2,
            raan_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_at_epoch: std::f64::consts::FRAC_PI_2,
            epoch_seconds: 0.0,
        };
        let state = elements.state_at(GM_EARTH_KM3_S2, 0.0);
        assert!(state.position_km.x.abs() < 1.0e-6);
        assert!(state.position_km.y.abs() < 1.0e-6);
        assert!((state.position_km.z - 7000.0).abs() < 1.0e-6);
    }

    #[test]
    fn energy_is_conserved_around_an_elliptical_orbit() {
        let elements = OrbitalElements {
            inclination_rad: 0.3,
            raan_rad: 1.1,
            arg_periapsis_rad: 0.4,
            ..earth_like_orbit()
        };
        let energy_at = |t: f64| {
            let state = elements.state_at(GM_SUN_KM3_S2, t);
            state.velocity_km_s.length_squared() / 2.0 - GM_SUN_KM3_S2 / state.position_km.length()
        };
        let expected = -GM_SUN_KM3_S2 / (2.0 * elements.semi_major_axis_km);
        for days in [0.0, 10.0, 100.0, 200.0, 364.0] {
            let energy = energy_at(days * 86_400.0);
            assert!(
                (energy - expected).abs() / expected.abs() < 1.0e-9,
                "{days}d: {energy} vs {expected}"
            );
        }
    }

    #[test]
    fn a_year_of_an_earth_like_orbit_returns_to_the_start() {
        let elements = earth_like_orbit();
        let start = elements.state_at(GM_SUN_KM3_S2, 0.0);
        let period_seconds =
            std::f64::consts::TAU * (elements.semi_major_axis_km.powi(3) / GM_SUN_KM3_S2).sqrt();
        let one_year_later = elements.state_at(GM_SUN_KM3_S2, period_seconds);
        assert!((one_year_later.position_km - start.position_km).length() < 1.0e-3);
    }

    #[test]
    fn from_state_round_trips_an_elliptical_orbit() {
        let original = OrbitalElements {
            semi_major_axis_km: 24_396.0,
            eccentricity: 0.72,
            inclination_rad: 0.9,
            raan_rad: 2.1,
            arg_periapsis_rad: 0.5,
            mean_anomaly_at_epoch: 1.7,
            epoch_seconds: 1_000.0,
        };
        let state = original.state_at(GM_EARTH_KM3_S2, 1_000.0);
        let derived = OrbitalElements::from_state(state, GM_EARTH_KM3_S2, 1_000.0);

        assert!((derived.semi_major_axis_km - original.semi_major_axis_km).abs() < 1.0e-6);
        assert!((derived.eccentricity - original.eccentricity).abs() < 1.0e-9);
        assert!((derived.inclination_rad - original.inclination_rad).abs() < 1.0e-9);
        assert!((derived.raan_rad - original.raan_rad).abs() < 1.0e-9);
        assert!((derived.arg_periapsis_rad - original.arg_periapsis_rad).abs() < 1.0e-9);

        // Propagating the derived elements from their own epoch should land
        // on the same state; the mean anomaly itself is checked indirectly
        // through it.
        let replayed = derived.state_at(GM_EARTH_KM3_S2, 1_000.0);
        assert!((replayed.position_km - state.position_km).length() < 1.0e-6);
        assert!((replayed.velocity_km_s - state.velocity_km_s).length() < 1.0e-9);
    }

    #[test]
    fn from_state_round_trips_a_hyperbolic_orbit() {
        // An Earth departure hyperbola: well above local escape velocity at
        // perigee altitude.
        let state = StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 11.5, 2.0));
        let elements = OrbitalElements::from_state(state, GM_EARTH_KM3_S2, 500.0);
        assert!(elements.eccentricity > 1.0, "{elements:?}");
        assert!(elements.semi_major_axis_km < 0.0, "{elements:?}");

        let replayed = elements.state_at(GM_EARTH_KM3_S2, 500.0);
        assert!((replayed.position_km - state.position_km).length() < 1.0e-6);
        assert!((replayed.velocity_km_s - state.velocity_km_s).length() < 1.0e-9);
    }

    #[test]
    fn a_hyperbolic_orbit_keeps_receding() {
        let state = StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 11.5, 2.0));
        let elements = OrbitalElements::from_state(state, GM_EARTH_KM3_S2, 0.0);
        let mut previous_r = 0.0;
        for hours in [1.0, 2.0, 5.0, 10.0, 24.0] {
            let r = elements
                .state_at(GM_EARTH_KM3_S2, hours * 3600.0)
                .position_km
                .length();
            assert!(r > previous_r, "{hours}h: {r} vs {previous_r}");
            previous_r = r;
        }
    }
}
