//! On-screen readout: where the cursor is pointing on the globe, how high the
//! camera is, and what the simulated clock says.

use bevy::prelude::*;
use bevy::text::FontSize;
use bevy::ui::widget::Text;

use crate::camera::OrbitCamera;
use crate::frame::{FrameSet, ReferenceFrame};
use crate::geo::{EARTH_RADIUS_KM, LatLon, ray_sphere_intersection};
use crate::globe::GLOBE_RADIUS;
use crate::sun::Sun;
use crate::tiles::TileCache;
use crate::wms::WmsSettings;

const HELP_TEXT: &str = "drag  orbit\n\
                         scroll / pinch  zoom\n\
                         WASD / arrows  orbit\n\
                         +  -  zoom\n\
                         space  ECI / ECEF frame\n\
                         R  reset view\n\
                         P  pause sun    , .  sun speed    N  now\n\
                         I  full illumination\n\
                         T  WMS imagery    L  next layer\n\
                         H  hide this";

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_hud).add_systems(
            Update,
            (update_readout.in_set(FrameSet::Apply), toggle_help),
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

fn toggle_help(keys: Res<ButtonInput<KeyCode>>, mut help: Single<&mut Visibility, With<HelpText>>) {
    if keys.just_pressed(KeyCode::KeyH) {
        **help = match **help {
            Visibility::Hidden => Visibility::Inherited,
            _ => Visibility::Hidden,
        };
    }
}

fn update_readout(
    camera: Single<(&Camera, &GlobalTransform, &OrbitCamera)>,
    windows: Query<&Window>,
    sun: Res<Sun>,
    frame: Res<ReferenceFrame>,
    wms: Res<WmsSettings>,
    tiles: Res<TileCache>,
    mut readout: Single<&mut Text, With<ReadoutText>>,
) {
    let (camera, camera_transform, orbit) = *camera;

    let cursor_coordinate = windows
        .iter()
        .find_map(|window| window.cursor_position())
        .and_then(|cursor| camera.viewport_to_world(camera_transform, cursor).ok())
        .and_then(|ray| ray_sphere_intersection(ray.origin, *ray.direction, GLOBE_RADIUS))
        // The hit is in world space; the coordinate under it is Earth-fixed.
        .map(|hit| LatLon::from_direction(frame.world_to_earth() * hit));

    let altitude_km = (orbit.distance - GLOBE_RADIUS) * EARTH_RADIUS_KM;
    let cursor = match cursor_coordinate {
        Some(coordinate) => coordinate.format(),
        None => "—".to_string(),
    };

    let imagery = if wms.enabled {
        format!(
            "{}\n           level {} · {} drawn · {} loading",
            wms.label(),
            tiles.deepest_level,
            tiles.visible_tiles,
            tiles.loading_tiles,
        )
    } else {
        "off".to_string()
    };

    readout.0 = format!(
        "TERRAMENTA\n\
         cursor    {cursor}\n\
         altitude  {altitude_km:.0} km\n\
         frame     {}\n\
         sun over  {}{}\n\
         clock     {}{}\n\
         imagery   {imagery}",
        frame.mode.label(),
        sun.subsolar.format(),
        if sun.shaded { "" } else { "  (unshaded)" },
        sun.format_utc(),
        if sun.paused { "  (paused)" } else { "" },
    );
}
