//! Wiring `terramenta-solare`'s [`FrameTree`] up to the scene: the floating
//! origin that keeps a body drawn smoothly regardless of how far it is from
//! the solar system's actual origin, the Solar System Barycentre.
//!
//! World space here is `f32`, and a `Transform` a few hundred million
//! kilometres from its origin has already lost more precision than the
//! distance between two nearby objects.
//! [`FrameTree::state_of_relative_to`] fixes this in the model: it walks two
//! frames up only as far as their nearest common ancestor and subtracts
//! there, in `f64`, before anything is narrowed to fewer bits. This module
//! carries that fix to the screen: every tick, [`FloatingOrigin`] names the
//! frame world space is currently centred on — ordinarily whichever body the
//! camera is closest to — and [`place_solar_bodies`] re-centres every
//! [`SolarBody`] on it before narrowing the result to the `f32` a
//! [`Transform`] takes. The large, shared part of two nearby positions never
//! survives long enough to be rounded.
//!
//! [`TrackedSpacecraft`] and [`update_spacecraft`] are the ECS half of
//! [`terramenta_solare::spacecraft`]'s patched-conics switch: the model
//! decides, on every tick's simulated clock, whether a spacecraft has
//! crossed a sphere of influence and which body it belongs to next;
//! `update_spacecraft` applies that decision inside the running scene,
//! reparenting the frame [`SolarSystem`]'s tree — and therefore the
//! spacecraft's own [`SolarBody`] — swaps to. Drawing is unaffected: a
//! spacecraft is a [`SolarBody`] like any other, and [`place_solar_bodies`]
//! does not need to know that its frame's parent just changed.

use bevy::prelude::*;
use terramenta_solare::bodies::ASTRONOMICAL_UNIT_KM;
use terramenta_solare::spacecraft::{Primary, Spacecraft};
use terramenta_solare::{Epoch, FrameId, FrameTree, StateVector};

use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::EARTH_RADIUS_KM;
use crate::gnc::scene_from_canonical;
use crate::sun::Sun;
use crate::view::ViewState;

/// The solar system's frame tree, rooted at the Solar System Barycentre with
/// the Sun, Earth and Mars as its direct children — see
/// [`terramenta_solare::solar_system`].
#[derive(Resource)]
pub struct SolarSystem {
    tree: FrameTree,
}

impl SolarSystem {
    /// Looks up one of the tree's named frames — `"Sun"`, `"Earth"` or
    /// `"Mars"` — for building the [`Primary`] chain [`spawn_spacecraft`]
    /// needs.
    pub fn find(&self, name: &str) -> Option<FrameId> {
        self.tree.find(name)
    }

    /// The Solar System Barycentre, the tree's root — the origin a
    /// heliocentric view draws relative to.
    pub fn root(&self) -> FrameId {
        self.tree.root()
    }

    /// The frame tree itself, for callers — [`crate::heliocentric`]'s own
    /// camera rig, in particular — that need [`floating_offset`] directly
    /// rather than through [`place_solar_bodies`].
    pub(crate) fn tree(&self) -> &FrameTree {
        &self.tree
    }
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

/// A spacecraft on a two-body orbit around whichever body currently
/// dominates it, per [`terramenta_solare::spacecraft::Spacecraft`].
/// [`update_spacecraft`] advances it every tick and lets it reparent itself
/// in [`SolarSystem`]'s tree the moment it crosses a sphere of influence;
/// nothing else about the entity — its [`SolarBody`] frame id, in
/// particular — ever changes, since the model's whole point is that the
/// frame's *parent* moves while the spacecraft's own identity does not.
#[derive(Component)]
pub struct TrackedSpacecraft(pub Spacecraft);

pub struct SolarSystemPlugin;

impl Plugin for SolarSystemPlugin {
    fn build(&self, app: &mut App) {
        let tree = terramenta_solare::solar_system();
        let earth = tree
            .find("Earth")
            .expect("terramenta_solare::solar_system always adds an Earth frame");
        app.insert_resource(SolarSystem { tree })
            .insert_resource(FloatingOrigin { frame: earth })
            .add_systems(
                Update,
                // Advanced first and then drawn, so a spacecraft that
                // reparents this tick is already read back out of its new
                // frame rather than one tick behind it.
                (update_spacecraft, place_solar_bodies)
                    .chain()
                    .in_set(FrameSet::Apply),
            );
    }
}

/// Builds the bundle a spacecraft needs to join the scene — a [`SolarBody`]
/// with the [`Transform`] [`place_solar_bodies`] writes into, and the
/// [`TrackedSpacecraft`] [`update_spacecraft`] advances and reparents every
/// tick — and adds its frame to `solar_system`'s tree. Spawning the returned
/// bundle (with `Commands::spawn`, alongside whatever visual components the
/// caller wants on it) is the caller's own to do, the same way spawning any
/// other entity is.
pub fn spawn_spacecraft(
    solar_system: &mut SolarSystem,
    name: &'static str,
    primary: Primary,
    state: StateVector,
    epoch: Epoch,
) -> (SolarBody, TrackedSpacecraft, Transform) {
    let spacecraft = Spacecraft::spawn(&mut solar_system.tree, name, primary, state, epoch);
    let frame = spacecraft.frame;
    (
        SolarBody(frame),
        TrackedSpacecraft(spacecraft),
        Transform::default(),
    )
}

/// Advances every [`TrackedSpacecraft`] against the simulated clock and lets
/// it swap its gravitational parent — the Bevy half of
/// [`terramenta_solare::spacecraft::Spacecraft::update`], run once a tick so
/// a spacecraft escaping Earth or being captured by Mars reparents on its
/// own, entirely inside [`SolarSystem`]'s tree, without any other system
/// having to name which body is "current".
fn update_spacecraft(
    mut solar_system: ResMut<SolarSystem>,
    sun: Res<Sun>,
    mut spacecraft: Query<&mut TrackedSpacecraft>,
) {
    let epoch = Epoch::from_unix_seconds(sun.unix_seconds);
    for mut craft in &mut spacecraft {
        craft.0.update(&mut solar_system.tree, epoch);
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
/// `orientation` and `km_per_unit` are how the two views this crate draws
/// differ: the globe rotates the result through
/// [`ReferenceFrame::inertial_to_world`] (the same rotation an orbit computed
/// in ECI needs) and scales by an Earth radius, while the heliocentric view
/// holds a fixed ICRF-aligned orientation and scales by an astronomical unit.
/// Routing a heliocentric placement through the globe's rotation would be
/// wrong outright, not just differently scaled — that rotation tracks Earth's
/// own sidereal spin, which would turn the whole modeled solar system once a
/// day.
pub(crate) fn floating_offset(
    tree: &FrameTree,
    target: FrameId,
    origin: FrameId,
    epoch: Epoch,
    orientation: Quat,
    km_per_unit: f64,
) -> Vec3 {
    let relative_km = tree.state_of_relative_to(target, origin, epoch).position_km;
    orientation * scene_from_canonical(relative_km / km_per_unit)
}

fn place_solar_bodies(
    solar_system: Res<SolarSystem>,
    origin: Res<FloatingOrigin>,
    sun: Res<Sun>,
    frame: Res<ReferenceFrame>,
    view: Res<ViewState>,
    mut bodies: Query<(&SolarBody, &mut Transform)>,
) {
    let epoch = Epoch::from_unix_seconds(sun.unix_seconds);
    let (orientation, km_per_unit) = if view.is_heliocentric() {
        (Quat::IDENTITY, ASTRONOMICAL_UNIT_KM)
    } else {
        (frame.inertial_to_world(), f64::from(EARTH_RADIUS_KM))
    };
    for (body, mut transform) in &mut bodies {
        transform.translation = floating_offset(
            &solar_system.tree,
            body.0,
            origin.frame,
            epoch,
            orientation,
            km_per_unit,
        );
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

        let offset = floating_offset(
            &tree,
            probe,
            mars,
            Epoch::J2000,
            ReferenceFrame::default().inertial_to_world(),
            f64::from(EARTH_RADIUS_KM),
        );

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
        let orientation = ReferenceFrame::default().inertial_to_world();

        let far = floating_offset(
            &tree,
            mars,
            earth,
            Epoch::J2000,
            orientation,
            f64::from(EARTH_RADIUS_KM),
        );
        let near = floating_offset(
            &tree,
            mars,
            mars,
            Epoch::J2000,
            orientation,
            f64::from(EARTH_RADIUS_KM),
        );

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

    /// The whole feature, end to end and inside a running (if renderer-less)
    /// Bevy app rather than a bare call to the model: a spacecraft spawned
    /// on an Earth departure hyperbola gets its frame reparented to the Sun
    /// by nothing but ticking the app forward on the simulated clock, the
    /// same escape [`terramenta_solare::spacecraft`]'s own tests check
    /// directly against the tree.
    #[test]
    fn ticking_the_app_lets_an_escaping_spacecraft_swap_its_gravitational_parent() {
        let mut tree = FrameTree::new();
        let root = tree.root();
        let sun_frame = tree.add("Sun", root, terramenta_solare::frame::FixedAtParent);
        let earth_frame = tree.add(
            "Earth",
            root,
            FixedOffset(StateVector::new(
                DVec3::new(1.495_98e8, 0.0, 0.0),
                DVec3::ZERO,
            )),
        );
        let mut solar_system = SolarSystem { tree };

        // Comfortably above local escape velocity, the same departure the
        // model crate's own escape tests use.
        let departure = StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 11.5, 2.0));
        let bundle = spawn_spacecraft(
            &mut solar_system,
            "Escaper",
            Primary::earth(earth_frame, sun_frame),
            departure,
            Epoch::J2000,
        );
        let frame = bundle.0.0;
        assert_eq!(solar_system.tree.parent(frame), Some(earth_frame));

        let mut app = App::new();
        app.add_plugins(bevy::app::TaskPoolPlugin::default())
            .insert_resource(solar_system)
            .insert_resource(FloatingOrigin { frame: earth_frame })
            .insert_resource(ReferenceFrame::default())
            .insert_resource(ViewState::default())
            .insert_resource(Sun {
                unix_seconds: 946_728_000.0, // Epoch::J2000, as Unix seconds.
                ..Sun::default()
            })
            .add_systems(Update, (update_spacecraft, place_solar_bodies).chain());
        app.world_mut().spawn(bundle);

        let mut escaped = false;
        for _ in 0..(60 * 24) {
            app.world_mut().resource_mut::<Sun>().unix_seconds += 3_600.0;
            app.update();
            if app.world().resource::<SolarSystem>().tree.parent(frame) == Some(sun_frame) {
                escaped = true;
                break;
            }
        }
        assert!(
            escaped,
            "the app never reparented the spacecraft to the Sun"
        );

        // And it is still drawn sensibly afterward — no panic, no NaN — now
        // that it is the Sun, rather than Earth, that its frame hangs off.
        let transform = app
            .world_mut()
            .query::<(&SolarBody, &Transform)>()
            .iter(app.world())
            .find(|(body, _)| body.0 == frame)
            .map(|(_, transform)| *transform)
            .expect("the spacecraft's own entity");
        assert!(transform.translation.is_finite(), "{transform:?}");
    }
}
