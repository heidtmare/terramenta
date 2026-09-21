//! Which of the two views the scene is drawn in: the Earth-centered globe,
//! or the heliocentric view of the Sun, Earth and Mars around the Solar
//! System Barycentre.
//!
//! This is deliberately not folded into [`crate::frame`]'s ECEF/ECI split —
//! that toggle picks which way Earth's own axes are drawn, and stays
//! meaningful in either view. This one picks which body the whole scene is
//! drawn *around*, and changes far more than an orientation: the
//! [`crate::solar::FloatingOrigin`], the unit scale [`crate::solar::floating_offset`]
//! converts kilometres into, the camera rig driving the screen, and the
//! projection's near/far planes all have to land together, on the same
//! tick, for a switch to read as one continuous motion rather than a jump
//! cut interrupted by a stutter.
//!
//! The switch itself is not something anything here decides on its own —
//! [`RequestViewChange`] is the only door in, written by [`view_controls`]'s
//! keybind today and left open for a future automatic trigger (a camera
//! zoomed out past some threshold, say) to write the same message without
//! [`drive_view_transition`] changing at all. [`ViewState`] tracks the two
//! settled views and the two legs of getting between them; [`ViewChanged`]
//! fires once, at the tick the actual cut happens, for anything (today, just
//! [`toggle_body_visibility`]) that needs to react at that exact moment
//! rather than poll the state every frame.

use bevy::prelude::*;

use crate::camera::OrbitCamera;
use crate::frame::FrameSet;
use crate::globe::AtmosphereMaterial;
use crate::globe::Globe;
use crate::heliocentric::{HELIO_DEFAULT_DISTANCE_AU, HeliocentricCamera, HeliocentricVisual};
use crate::placemark::Placemark;
use crate::solar::{FloatingOrigin, SolarSystem};
use crate::starfield::StarfieldMaterial;
use crate::tiles::TileEntity;
use crate::vector_tiles::VectorTileEntity;

/// How far out the globe camera pulls back before the cut into the
/// heliocentric view — comfortably past [`crate::camera`]'s own maximum
/// zoomed-out distance, so the departure reads as leaving the planet behind
/// rather than as one more zoom step.
const DEPARTURE_DISTANCE: f32 = crate::globe::GLOBE_RADIUS * 60.0;
/// Long enough for the pull-back in [`drop_departure_clutter`]'s stripped-down
/// scene — just the base globe image against black — to read as a deliberate,
/// slow departure rather than a snap.
const TRANSITION_OUT_SECONDS: f32 = 2.0;
const TRANSITION_IN_SECONDS: f32 = 1.0;

/// How long the screen stays faded to black once the heliocentric view has
/// settled, before [`apply_transition_fade`] clears it — see [`TransitionFade`].
const FADE_IN_SECONDS: f32 = 0.5;

/// Fraction of [`TRANSITION_OUT_SECONDS`] the pull-back gets to itself,
/// clearly visible against the stripped-down scene [`drop_departure_clutter`]
/// leaves behind, before [`TransitionFade`] starts covering the last stretch
/// of it — the cut still needs masking (see [`TransitionFade`]'s own doc), but
/// not before the zoom itself has had a chance to read as the departure.
const FADE_START_FRACTION: f32 = 0.6;

/// The near/far planes the camera's [`Projection`] needs once it is drawing
/// the heliocentric scene, in astronomical units — near enough to still
/// resolve a close pass, far enough that Mars's aphelion (~1.66 AU) is
/// comfortably inside it.
const HELIO_NEAR_AU: f32 = 0.0005;
const HELIO_FAR_AU: f32 = 20.0;
/// The globe view's own near/far planes, restored on the way back in — see
/// [`crate::camera::spawn_camera`].
const GLOBE_NEAR: f32 = 0.001;
const GLOBE_FAR: f32 = 1000.0;

/// The two views [`ViewState`] settles into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Globe,
    Heliocentric,
}

impl ViewMode {
    /// The stable name the control surface names this view by, on the same
    /// terms as [`crate::frame::FrameMode::id`].
    pub fn id(self) -> &'static str {
        match self {
            Self::Globe => "globe",
            Self::Heliocentric => "heliocentric",
        }
    }

    /// Parses [`ViewMode::id`] back, for a view named by an embedder.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "globe" => Some(Self::Globe),
            "heliocentric" => Some(Self::Heliocentric),
            _ => None,
        }
    }
}

/// Which view is showing, and — for the two legs of getting between
/// them — how far into the transition the scene is.
///
/// The transition legs carry their own elapsed time rather than reusing
/// [`Time`] directly so [`drive_view_transition`] can tell a transition
/// apart from having just started one this tick.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub enum ViewState {
    #[default]
    Globe,
    /// `start_distance` is [`OrbitCamera::distance`] at the moment the leg
    /// began, so the pull-back can ease from wherever the camera actually
    /// was rather than assuming it started at rest.
    TransitioningOut { elapsed: f32, start_distance: f32 },
    Heliocentric,
    TransitioningIn { elapsed: f32 },
}

impl ViewState {
    /// Whether [`crate::solar::place_solar_bodies`] should draw relative to
    /// the Solar System Barycentre at astronomical-unit scale rather than
    /// relative to Earth at Earth-radius scale.
    ///
    /// True only once [`ViewState::Heliocentric`] has actually settled: the
    /// two transition legs keep [`crate::solar::FloatingOrigin`] and the
    /// globe camera as they were until the cut, so every [`crate::solar::SolarBody`]
    /// — a spacecraft included — stays correctly placed throughout the pull
    /// back, and only jumps unit regime at the same instant the camera does.
    pub(crate) fn is_heliocentric(&self) -> bool {
        matches!(self, ViewState::Heliocentric)
    }
}

/// The only way anything asks for the view to change. A keybind writes this
/// today; a future automatic trigger — the camera crossing some distance
/// while zoomed out, say — would be just one more writer, with no change to
/// [`drive_view_transition`] itself.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestViewChange(pub ViewMode);

/// Fired once, on the tick the hard cut between the two unit regimes
/// actually happens — the moment [`toggle_body_visibility`] and anything
/// like it needs to react at, rather than polling [`ViewState`] every frame.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewChanged {
    pub mode: ViewMode,
}

/// Fired once, the tick the globe camera begins departing for the
/// heliocentric view — before the pull-back itself moves anything, so
/// [`drop_departure_clutter`] can strip the scene down to the base globe
/// image first and the zoom then has nothing else in frame to look awkward
/// against.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
struct DepartureStarted;

/// How opaque the black screen-space fade drawn by [`TransitionFadeOverlay`]
/// should be right now, `0.0` clear to `1.0` fully covering the scene.
///
/// The globe pulling back and the heliocentric bodies snapping into their
/// AU-scale positions cannot be cross-dissolved directly — [`FloatingOrigin`]
/// and the unit scale [`crate::solar::floating_offset`] converts into are
/// only ever meaningful for one regime at a time, so every [`crate::solar::SolarBody`]
/// is positioned correctly in the old regime right up to the hard cut, and
/// correctly in the new one from the same tick on, with no in-between state
/// to animate through. Ramping this to black over [`TRANSITION_OUT_SECONDS`]
/// — while the globe camera is still visibly pulling back, not waiting for
/// it to finish — and clearing it again over [`FADE_IN_SECONDS`] once
/// [`ViewState::Heliocentric`] has settled hides that cut inside a fade
/// instead of letting it read as a jump.
#[derive(Resource, Debug, Clone, Copy, Default)]
struct TransitionFade(f32);

/// The full-screen [`Node`] [`TransitionFade`] is painted onto.
#[derive(Component)]
struct TransitionFadeOverlay;

/// Where the view's own systems run relative to [`FrameSet`]: settled before
/// the camera is built for this tick, the same discipline [`FrameSet`]
/// itself documents — a switch only looks seamless if the state, the camera
/// and everything drawn from it land together.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewSet {
    Settle,
}

pub struct ViewPlugin;

impl Plugin for ViewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewState>()
            .init_resource::<TransitionFade>()
            .add_message::<RequestViewChange>()
            .add_message::<ViewChanged>()
            .add_message::<DepartureStarted>()
            .configure_sets(
                Update,
                ViewSet::Settle
                    .after(FrameSet::Settle)
                    .before(FrameSet::Camera),
            )
            .add_systems(Startup, spawn_transition_fade_overlay)
            .add_systems(
                Update,
                (view_controls, drive_view_transition, apply_transition_fade)
                    .chain()
                    .in_set(ViewSet::Settle),
            )
            .add_systems(
                Update,
                (toggle_body_visibility, drop_departure_clutter).in_set(FrameSet::Apply),
            );
    }
}

/// Whether the globe's own camera and input should be live this tick — every
/// state except the heliocentric view itself, since both transition legs are
/// still, mechanically, the globe camera pulling back from or returning to
/// Earth.
pub(crate) fn not_heliocentric_view(view: Res<ViewState>) -> bool {
    !view.is_heliocentric()
}

/// Whether the globe's own mouse/touch/keyboard input should be read —
/// narrower than [`not_heliocentric_view`], since input during a transition
/// would fight the pull-back or return animation driving the same fields.
pub(crate) fn in_globe_view(view: Res<ViewState>) -> bool {
    matches!(*view, ViewState::Globe)
}

/// Whether the heliocentric camera's own input and placement should run.
pub(crate) fn in_heliocentric_view(view: Res<ViewState>) -> bool {
    view.is_heliocentric()
}

/// True from the moment the globe camera starts departing for the
/// heliocentric view, not just once it has settled there — the plain
/// function version, for a system that needs to fold this into a
/// [`Visibility`] decision it is already computing (`show_ephemerides`,
/// `orient_overlays`) rather than stopping the whole system, since those
/// still have upstream work (fetching, ageing, rebuilding) worth doing while
/// departing.
pub(crate) fn is_departing_view(view: &ViewState) -> bool {
    matches!(*view, ViewState::TransitioningOut { .. } | ViewState::Heliocentric)
}

/// Whether imagery, vector tiles and placemarks should keep streaming,
/// positioning and re-showing themselves at all — false from the moment the
/// globe camera starts departing for the heliocentric view, not just once it
/// has settled there. Unlike [`is_departing_view`], this stops the entire
/// per-tick chain for these three, spawning included, since nothing about
/// them is worth computing with no camera looking at the globe they drape
/// onto — so there is nothing left to spawn mid-departure that could
/// undo [`drop_departure_clutter`]'s hide the way a half-gated chain could.
pub(crate) fn not_departing_view(view: Res<ViewState>) -> bool {
    !is_departing_view(&view)
}

fn view_controls(
    keys: Res<ButtonInput<KeyCode>>,
    view: Res<ViewState>,
    mut requests: MessageWriter<RequestViewChange>,
) {
    if !keys.just_pressed(KeyCode::KeyU) {
        return;
    }
    match *view {
        ViewState::Globe => {
            requests.write(RequestViewChange(ViewMode::Heliocentric));
        }
        ViewState::Heliocentric => {
            requests.write(RequestViewChange(ViewMode::Globe));
        }
        // A request mid-transition is ignored rather than queued or
        // reversed — the same "the switch already in flight wins" choice
        // `frame_controls` doesn't have to make, since its switch is instant.
        _ => {}
    }
}

/// The transition's whole state machine.
///
/// `TransitioningOut` is the one leg that animates something itself: it eases
/// [`OrbitCamera::distance`] from wherever it was to [`DEPARTURE_DISTANCE`]
/// over [`TRANSITION_OUT_SECONDS`], smoothstepped, and pins `target_distance`
/// to the same value every tick so `apply_orbit`'s own exponential smoothing
/// (still running throughout — see [`not_heliocentric_view`]) has nothing
/// left to do — the fast snap-then-wait that smoothing produces on its own is
/// exactly what read as an awkward fly-out. [`TransitioningIn`](ViewState::TransitioningIn)
/// still leaves its own leg to that smoothing; only the departure needs the
/// deliberate pace. The hard cut — flipping [`FloatingOrigin`], the camera's
/// projection, and firing [`ViewChanged`] — happens in the same tick the
/// relevant timer elapses, so nothing is ever drawn half-cut.
#[allow(clippy::too_many_arguments)]
fn drive_view_transition(
    time: Res<Time>,
    mut requests: MessageReader<RequestViewChange>,
    mut view: ResMut<ViewState>,
    mut changed: MessageWriter<ViewChanged>,
    mut departed: MessageWriter<DepartureStarted>,
    mut origin: ResMut<FloatingOrigin>,
    mut fade: ResMut<TransitionFade>,
    solar_system: Res<SolarSystem>,
    mut camera: Single<(
        &mut OrbitCamera,
        &mut HeliocentricCamera,
        &mut Projection,
    )>,
) {
    let (orbit, helio, projection) = &mut *camera;

    for request in requests.read() {
        match (*view, request.0) {
            (ViewState::Globe, ViewMode::Heliocentric) => {
                let start_distance = orbit.distance;
                orbit.target_distance = DEPARTURE_DISTANCE;
                *view = ViewState::TransitioningOut {
                    elapsed: 0.0,
                    start_distance,
                };
                departed.write(DepartureStarted);
            }
            (ViewState::Heliocentric, ViewMode::Globe) => {
                orbit.distance = DEPARTURE_DISTANCE;
                orbit.target_distance = orbit.target_distance.min(DEPARTURE_DISTANCE);
                *view = ViewState::TransitioningIn { elapsed: 0.0 };
            }
            _ => {}
        }
    }

    match &mut *view {
        ViewState::TransitioningOut {
            elapsed,
            start_distance,
        } => {
            *elapsed += time.delta_secs();
            let t = (*elapsed / TRANSITION_OUT_SECONDS).clamp(0.0, 1.0);
            let eased = smoothstep(t);
            let distance = *start_distance + (DEPARTURE_DISTANCE - *start_distance) * eased;
            orbit.distance = distance;
            orbit.target_distance = distance;
            fade.0 = ((t - FADE_START_FRACTION) / (1.0 - FADE_START_FRACTION)).clamp(0.0, 1.0);
            if *elapsed >= TRANSITION_OUT_SECONDS {
                origin.frame = solar_system.root();
                set_projection(projection, HELIO_NEAR_AU, HELIO_FAR_AU);
                helio.anchor = solar_system.find("Earth");
                helio.yaw = orbit.yaw;
                helio.target_yaw = orbit.yaw;
                helio.pitch = 0.35;
                helio.target_pitch = 0.35;
                helio.distance = HELIO_DEFAULT_DISTANCE_AU;
                helio.target_distance = HELIO_DEFAULT_DISTANCE_AU;
                changed.write(ViewChanged {
                    mode: ViewMode::Heliocentric,
                });
                *view = ViewState::Heliocentric;
            }
        }
        ViewState::TransitioningIn { elapsed } => {
            if *elapsed == 0.0 {
                origin.frame = solar_system
                    .find("Earth")
                    .expect("terramenta_solare::solar_system always adds an Earth frame");
                set_projection(projection, GLOBE_NEAR, GLOBE_FAR);
                changed.write(ViewChanged {
                    mode: ViewMode::Globe,
                });
            }
            *elapsed += time.delta_secs();
            if *elapsed >= TRANSITION_IN_SECONDS {
                *view = ViewState::Globe;
            }
        }
        ViewState::Heliocentric => {
            fade.0 = (fade.0 - time.delta_secs() / FADE_IN_SECONDS).max(0.0);
        }
        ViewState::Globe => {}
    }
}

/// Spawns the full-screen, initially-clear `Node` [`apply_transition_fade`]
/// paints [`TransitionFade`] onto.
fn spawn_transition_fade_overlay(mut commands: Commands) {
    commands.spawn((
        TransitionFadeOverlay,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(0.0),
            left: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::BLACK.with_alpha(0.0)),
    ));
}

/// Paints the current [`TransitionFade`] level onto [`TransitionFadeOverlay`].
fn apply_transition_fade(
    fade: Res<TransitionFade>,
    mut overlay: Single<&mut BackgroundColor, With<TransitionFadeOverlay>>,
) {
    overlay.0 = Color::BLACK.with_alpha(fade.0);
}

fn set_projection(projection: &mut Projection, near: f32, far: f32) {
    if let Projection::Perspective(perspective) = projection {
        perspective.near = near;
        perspective.far = far;
    }
}

/// Eases `t` (already clamped to `[0, 1]`) with zero velocity at both ends,
/// so [`OrbitCamera::distance`] comes to rest instead of arriving at
/// [`DEPARTURE_DISTANCE`] still moving the way a linear ramp would.
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Strips the scene down to the base globe image the moment the departure
/// begins — atmosphere, the Earth-fixed starfield, imagery, vector tiles and
/// placemarks all drop out together, before the camera has even started
/// pulling back, so the following zoom has a clean, cohesive shot of just the
/// globe shrinking into the distance rather than everything else lagging out
/// mid-motion.
///
/// Satellite markers/trails and GeoJSON overlays hide themselves instead —
/// `show_ephemerides` and `orient_overlays` fold [`is_departing_view`] into
/// the [`Visibility`] they already recompute every tick from their own
/// settings, since their surrounding systems (fetching, ageing, rebuilding)
/// keep running while departing and could otherwise spawn something after
/// this system's own one-shot hide had already run.
///
/// [`not_departing_view`] keeps the five groups here from being streamed,
/// positioned or redrawn back in on the next tick, so this system only needs
/// to land the initial hide, once, off [`DepartureStarted`].
#[allow(clippy::type_complexity)]
fn drop_departure_clutter(
    mut started: MessageReader<DepartureStarted>,
    mut visuals: ParamSet<(
        Query<&mut Visibility, With<MeshMaterial3d<AtmosphereMaterial>>>,
        Query<&mut Visibility, With<MeshMaterial3d<StarfieldMaterial>>>,
        Query<&mut Visibility, With<TileEntity>>,
        Query<&mut Visibility, With<VectorTileEntity>>,
        Query<&mut Visibility, With<Placemark>>,
    )>,
) {
    if started.read().next().is_none() {
        return;
    }
    for mut visibility in &mut visuals.p0() {
        *visibility = Visibility::Hidden;
    }
    for mut visibility in &mut visuals.p1() {
        *visibility = Visibility::Hidden;
    }
    for mut visibility in &mut visuals.p2() {
        *visibility = Visibility::Hidden;
    }
    for mut visibility in &mut visuals.p3() {
        *visibility = Visibility::Hidden;
    }
    for mut visibility in &mut visuals.p4() {
        *visibility = Visibility::Hidden;
    }
}

/// Shows the globe's own visuals or the heliocentric bodies' meshes,
/// whichever [`ViewChanged`] just switched to; the other set hides.
///
/// The queries below all write [`Visibility`] on entity sets Bevy has
/// no static way to prove disjoint from one another (different marker/material
/// types, no shared `Without`), so they go through a [`ParamSet`] rather than
/// plain `Query` parameters — the same conflict [`crate::hud`] avoids
/// with paired `Without` filters, done here with a set instead since there
/// are more than two groups.
///
/// Imagery tiles, vector tiles and placemarks get the one-way treatment:
/// hidden going out to heliocentric, but left alone coming back, since
/// [`crate::tiles`], [`crate::vector_tiles`] and [`crate::placemark`]'s own
/// per-tick systems resume that same tick and recompute their visibility on
/// their own — see `not_heliocentric_view`. Forcing them visible here would
/// just be a wrong guess for that one frame, showing whatever the globe's
/// own systems had already hidden or retired.
#[allow(clippy::type_complexity)]
fn toggle_body_visibility(
    mut changed: MessageReader<ViewChanged>,
    mut visuals: ParamSet<(
        Query<&mut Visibility, With<Globe>>,
        Query<&mut Visibility, With<MeshMaterial3d<AtmosphereMaterial>>>,
        Query<&mut Visibility, With<MeshMaterial3d<StarfieldMaterial>>>,
        Query<&mut Visibility, With<HeliocentricVisual>>,
        Query<&mut Visibility, With<TileEntity>>,
        Query<&mut Visibility, With<VectorTileEntity>>,
        Query<&mut Visibility, With<Placemark>>,
    )>,
) {
    let Some(change) = changed.read().last() else {
        return;
    };
    let (globe_visible, heliocentric_visible) = match change.mode {
        ViewMode::Globe => (true, false),
        ViewMode::Heliocentric => (false, true),
    };
    for mut visibility in &mut visuals.p0() {
        *visibility = as_visibility(globe_visible);
    }
    for mut visibility in &mut visuals.p1() {
        *visibility = as_visibility(globe_visible);
    }
    for mut visibility in &mut visuals.p2() {
        *visibility = as_visibility(globe_visible);
    }
    for mut visibility in &mut visuals.p3() {
        *visibility = as_visibility(heliocentric_visible);
    }
    if heliocentric_visible {
        for mut visibility in &mut visuals.p4() {
            *visibility = Visibility::Hidden;
        }
        for mut visibility in &mut visuals.p5() {
            *visibility = Visibility::Hidden;
        }
        for mut visibility in &mut visuals.p6() {
            *visibility = Visibility::Hidden;
        }
    }
}

fn as_visibility(visible: bool) -> Visibility {
    if visible {
        Visibility::Visible
    } else {
        Visibility::Hidden
    }
}
