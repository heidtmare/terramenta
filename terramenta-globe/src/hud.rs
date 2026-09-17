//! On-screen readout: where the cursor is pointing on the globe, how high the
//! camera is, and what the simulated clock says.
//!
//! None of these numbers are worked out here. They are read from
//! [`crate::api::LatestState`], which is the same snapshot an embedder sees, so
//! the built-in readout and an external interface can never disagree — and an
//! embedder that would rather draw its own can switch this one off through
//! [`HudSettings`].

use bevy::prelude::*;
use bevy::text::FontSize;
use bevy::ui::widget::Text;

use crate::api::{GlobeState, LatestState, keyboard_enabled, publish_state};
use crate::frame::FrameSet;

const HELP_TEXT: &str = "drag  orbit\n\
                         ctrl drag  rotate / tilt\n\
                         shift drag  look around\n\
                         scroll / pinch  zoom\n\
                         WASD / arrows  orbit\n\
                         +  -  zoom\n\
                         space  ECI / ECEF frame\n\
                         R  reset view\n\
                         P  pause sun    , .  sun speed    N  now\n\
                         I  full illumination\n\
                         T  imagery    L  /  shift L  next / previous layer\n\
                         V  vector tiles    shift V  next vector layer\n\
                         O  satellites    shift O  orbit trails\n\
                         H  hide this";

/// Whether the readout and its key list are drawn.
#[derive(Resource, Debug, Clone)]
pub struct HudSettings {
    /// The whole overlay. Off, the globe fills the canvas with nothing on it.
    pub visible: bool,
    /// The key list alone, which is what `H` toggles.
    pub help_visible: bool,
}

impl Default for HudSettings {
    fn default() -> Self {
        Self {
            visible: true,
            help_visible: true,
        }
    }
}

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HudSettings>()
            .add_systems(Startup, spawn_hud)
            .add_systems(
                Update,
                (
                    // The snapshot is built during `Apply`; formatting it has to
                    // follow, or the readout is always a frame behind.
                    update_readout.in_set(FrameSet::Apply).after(publish_state),
                    toggle_help.run_if(keyboard_enabled),
                    apply_visibility,
                ),
            );
    }
}

#[derive(Component)]
struct ReadoutText;

#[derive(Component)]
struct HelpText;

fn spawn_hud(mut commands: Commands) {
    let font = TextFont {
        font_size: FontSize::Px(13.0),
        ..default()
    };
    let color = TextColor(Color::srgba(0.82, 0.90, 1.0, 0.85));

    commands.spawn((
        ReadoutText,
        Text::default(),
        font.clone(),
        color,
        Node {
            position_type: PositionType::Absolute,
            top: px(14),
            left: px(16),
            ..default()
        },
    ));

    commands.spawn((
        HelpText,
        Text::new(HELP_TEXT),
        font,
        TextColor(Color::srgba(0.82, 0.90, 1.0, 0.5)),
        Node {
            position_type: PositionType::Absolute,
            bottom: px(14),
            left: px(16),
            ..default()
        },
    ));
}

fn toggle_help(keys: Res<ButtonInput<KeyCode>>, mut hud: ResMut<HudSettings>) {
    if keys.just_pressed(KeyCode::KeyH) {
        hud.help_visible = !hud.help_visible;
    }
}

fn apply_visibility(
    hud: Res<HudSettings>,
    mut readout: Single<&mut Visibility, (With<ReadoutText>, Without<HelpText>)>,
    mut help: Single<&mut Visibility, (With<HelpText>, Without<ReadoutText>)>,
) {
    if !hud.is_changed() {
        return;
    }
    **readout = visibility(hud.visible);
    **help = visibility(hud.visible && hud.help_visible);
}

fn visibility(visible: bool) -> Visibility {
    if visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    }
}

fn update_readout(latest: Res<LatestState>, mut readout: Single<&mut Text, With<ReadoutText>>) {
    let Some(state) = latest.0.as_ref() else {
        return;
    };
    readout.0 = format_readout(state);
}

/// The readout's text, split out so its shape is testable without a `World`.
fn format_readout(state: &GlobeState) -> String {
    let cursor = match state.cursor {
        Some(coordinate) => coordinate.format(),
        None => "—".to_string(),
    };

    let imagery = if state.imagery.enabled {
        format!(
            "{}\n           level {} · {} drawn · {} loading",
            state.imagery.label,
            state.imagery.deepest_level,
            state.imagery.visible_tiles,
            state.imagery.loading_tiles,
        )
    } else {
        "off".to_string()
    };

    // Named `vectors` rather than `tiles` in the readout: the row above is
    // already tiles, and what tells the two apart is that one is pictures.
    let vectors = if state.vector_tiles.enabled {
        format!(
            "{}\n           level {} · {} drawn · {} loading · {} features",
            state.vector_tiles.label,
            state.vector_tiles.deepest_level,
            state.vector_tiles.visible_tiles,
            state.vector_tiles.loading_tiles,
            state.vector_tiles.features,
        )
    } else {
        "off".to_string()
    };

    // Only worth a line when there is something to say: a globe with no
    // overlays should not carry a row reporting that there are none.
    let overlays = match state.overlays.layers.len() {
        0 => String::new(),
        total => format!(
            "\noverlays  {} of {total} drawn{}",
            state.overlays.drawn,
            if state.overlays.enabled {
                ""
            } else {
                "  (off)"
            },
        ),
    };

    // Whatever is pinned, or failing that whatever the cursor is over — the
    // same preference the highlight itself follows.
    let picked = match state
        .overlays
        .pinned
        .as_ref()
        .or(state.overlays.hovered.as_ref())
    {
        Some(feature) => format!(
            "\npicked    {} · {}",
            feature.label,
            feature
                .id
                .clone()
                .unwrap_or_else(|| format!("#{}", feature.index)),
        ),
        None => String::new(),
    };

    format!(
        "TERRAMENTA\n\
         cursor    {cursor}\n\
         altitude  {:.0} km\n\
         frame     {}\n\
         sun over  {}{}\n\
         clock     {}{}\n\
         imagery   {imagery}\n\
         vectors   {vectors}{overlays}{picked}",
        state.camera.altitude_km,
        state.frame.label,
        state.sun.subsolar.format(),
        if state.sun.shaded { "" } else { "  (unshaded)" },
        state.sun.utc,
        if state.sun.paused { "  (paused)" } else { "" },
    )
}
