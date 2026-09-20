//! Wiring `terramenta-solare`'s [`FrameTree`] up to the scene: the floating
//! origin that keeps a body drawn smoothly regardless of how far it is from
//! the solar system's actual origin, the Solar System Barycentre.
//!
//! World space here is `f32`, and a `Transform` a few hundred million
//! kilometres from its origin has already lost more precision than the
//! distance between two nearby objects — which is exactly backwards for a
//! scene where what matters is how close together things are, not how far
//! from an arbitrary point they all happen to be. [`FrameTree::
//! state_of_relative_to`] already does the fix in the model: it walks two
//! frames up only as far as their nearest common ancestor and subtracts
//! there, in `f64`, before anything is asked to fit in fewer bits. This
//! module is what makes that reach the screen: every tick, [`FloatingOrigin`]
//! names the frame world space is currently centred on — ordinarily whichever
//! body the camera is closest to — and [`place_solar_bodies`] re-centres
//! every [`SolarBody`] on it before narrowing the result to the `f32` a
//! [`Transform`] takes. The large, shared part of two nearby positions never
//! survives long enough to be rounded.

use bevy::prelude::*;
use terramenta_solare::{Epoch, FrameId, FrameTree};

use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::EARTH_RADIUS_KM;
use crate::gnc::scene_from_canonical;
use crate::sun::Sun;

/// The solar system's frame tree, rooted at the Solar System Barycentre with
/// the Sun, Earth and Mars as its direct children — see
/// [`terramenta_solare::solar_system`].
#[derive(Resource)]
pub struct SolarSystem {
    tree: FrameTree,
}

/// Which frame world space is currently drawn relative to.
///
/// This is the one piece of state that makes camera-relative rendering work:
/// everything a [`SolarBody`] draws is only ever asked for *relative to this
/// frame*, never relative to the SSB. Left at Earth, which is where this
/// crate's own camera stays — see [`crate::camera`] — a body already close to
/// Earth draws with all the precision an `f32` has to give, and one far from
/// it (Mars, most of the time) draws exactly as far off as it should, no more
/// precisely than a distance that size can be.
#[derive(Resource, Clone, Copy)]
pub struct FloatingOrigin {
    pub frame: FrameId,
}

/// Marks an entity whose [`Transform::translation`] tracks a frame in the
/// [`SolarSystem`] tree, relative to whatever [`FloatingOrigin`] is currently
/// set to.
#[derive(Component, Clone, Copy)]
pub struct SolarBody(pub FrameId);

pub struct SolarSystemPlugin;

impl Plugin for SolarSystemPlugin {
    fn build(&self, app: &mut App) {
        let tree = terramenta_solare::solar_system();
        let earth = tree
            .find("Earth")
            .expect("terramenta_solare::solar_system always adds an Earth frame");
        app.insert_resource(SolarSystem { tree })
            .insert_resource(FloatingOrigin { frame: earth })
            .add_systems(Update, place_solar_bodies.in_set(FrameSet::Apply));
    }
}

/// `target`'s position relative to `origin`, in Bevy world space.
///
/// The subtraction that matters — cancelling out however far both frames are
/// from the SSB — happens in [`FrameTree::state_of_relative_to`], entirely in
/// `f64`. What is left over by the time this narrows to a [`Vec3`] is only
/// ever as large as the distance between `target` and `origin` actually is,
/// which is the whole reason a spacecraft near Mars can sit metres from where
/// it belongs instead of jittering across kilometres.
///
/// The result is inertial — [`FrameTree`] states everything in ICRF axes —
/// so it goes through [`ReferenceFrame::inertial_to_world`], the same
/// rotation an orbit computed in ECI needs before it can be drawn; see
/// [`crate::gnc`].
pub(crate) fn floating_offset(
    tree: &FrameTree,
    target: FrameId,
    origin: FrameId,
    epoch: Epoch,
    frame: &ReferenceFrame,
) -> Vec3 {
    let relative_km = tree.state_of_relative_to(target, origin, epoch).position_km;
    frame.inertial_to_world() * scene_from_canonical(relative_km / f64::from(EARTH_RADIUS_KM))
}

fn place_solar_bodies(
    solar_system: Res<SolarSystem>,
    origin: Res<FloatingOrigin>,
    sun: Res<Sun>,
    frame: Res<ReferenceFrame>,
    mut bodies: Query<(&SolarBody, &mut Transform)>,
) {
    let epoch = Epoch::from_unix_seconds(sun.unix_seconds);
    for (body, mut transform) in &mut bodies {
        transform.translation =
            floating_offset(&solar_system.tree, body.0, origin.frame, epoch, &frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::DVec3;
    use terramenta_solare::frame::FixedOffset;
    use terramenta_solare::{StateVector, solar_system};

    #[test]
    fn an_offset_near_mars_keeps_its_precision_relative_to_mars() {
        let mut tree = solar_system();
        let mars = tree.find("Mars").unwrap();
        // A spacecraft 500 km from Mars's centre — the kind of distance a
        // rendezvous actually needs metres of precision for.
        let probe = tree.add(
            "Probe",
            mars,
            FixedOffset(StateVector::new(DVec3::new(500.0, 0.0, 0.0), DVec3::ZERO)),
        );

        let offset = floating_offset(&tree, probe, mars, Epoch::J2000, &ReferenceFrame::default());

        let expected_units = 500.0 / f64::from(EARTH_RADIUS_KM);
        assert!(
            (f64::from(offset.length()) - expected_units).abs() < 1.0e-6,
            "{offset:?}"
        );
    }

    #[test]
    fn the_origin_choice_is_what_keeps_a_nearby_body_small() {
        // Mars relative to Earth is on the order of an astronomical unit —
        // hundreds of thousands of world units, exactly the number that would
        // blow an `f32` vertex apart. Relative to itself, which is what a
        // camera actually near Mars would use as the origin, it is nothing.
        let tree = solar_system();
        let (earth, mars) = (tree.find("Earth").unwrap(), tree.find("Mars").unwrap());
        let frame = ReferenceFrame::default();

        let far = floating_offset(&tree, mars, earth, Epoch::J2000, &frame);
        let near = floating_offset(&tree, mars, mars, Epoch::J2000, &frame);

        assert!(far.length() > 1_000.0, "{far:?}");
        assert_eq!(near, Vec3::ZERO);
    }

    #[test]
    fn narrowing_to_f32_before_subtracting_is_the_bug_this_module_avoids() {
        // The failure mode `FloatingOrigin` exists to sidestep: an SSB-relative
        // position on the order of Mars's has already lost more precision as
        // an `f32` than the 500 m gap being asked about.
        let tree = solar_system();
        let mars = tree.find("Mars").unwrap();
        let absolute_km = tree.state_relative_to_root(mars, Epoch::J2000).position_km;
        let nearby_km = absolute_km + DVec3::new(0.5, 0.0, 0.0);

        let correct = (absolute_km - nearby_km).as_vec3();
        let naive = absolute_km.as_vec3() - nearby_km.as_vec3();

        assert!((correct.length() - 0.5).abs() < 1.0e-6, "{correct:?}");
        assert!(
            (naive.length() - 0.5).abs() > 1.0e-3,
            "naive f32 subtraction was accidentally precise: {naive:?}"
        );
    }
}
