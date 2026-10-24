//! Pad chords + intents only — mirrors `skate3_debug_cam.cpp`.
//! `SetManualCamMode` / `UpdateManualCam` / `GetMatrix` live in `ManualCam`.
use bevy::prelude::*;
use crate::app::SimulationSet;

const DPAD_RIGHT: u16 = 0x0008;
const R3: u16 = 0x0080;
const B: u16 = 0x2000;

#[derive(Resource, Default)]
pub(crate) struct DebugCam {
    want_toggle: bool,
    want_spawn: bool,
    previous_buttons: u16,
    release_controls: bool,
}

impl DebugCam {
    pub fn active(camera: &crate::camera::CameraRuntime) -> bool {
        camera.manual_cam.manual_cam_active()
    }

    pub fn suppress_gameplay(&self, camera: &crate::camera::CameraRuntime) -> bool {
        camera.manual_cam.manual_cam_active() || self.release_controls
    }

    pub fn pose(camera: &crate::camera::CameraRuntime) -> Option<Transform> {
        let Some(base) = camera.frame else { return None; };
        if !camera.manual_cam.manual_cam_active() {
            return None;
        }
        let frame = camera.manual_cam.get_matrix(&base);
        let [right, up, at] = frame.basis.columns.map(Vec3::from_array);
        Some(Transform {
            translation: Vec3::new(frame.position[0], frame.position[1], frame.position[2]),
            rotation: Quat::from_mat3(&Mat3::from_cols(-right, up, -at)).normalize(),
            scale: Vec3::ONE,
        })
    }
}

pub(crate) struct DebugCamPlugin;
impl Plugin for DebugCamPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DebugCam>()
            .add_systems(
                FixedUpdate,
                on_camera_input
                    .in_set(SimulationSet::Input)
                    .before(crate::input::publish_actions),
            )
            .add_systems(Startup, setup_overlay)
            .add_systems(Update, update_overlay);
    }
}

fn chord(buttons: u16, mask: u16, previous: u16) -> bool {
    buttons & mask == mask && previous & mask != mask
}

fn gameplay_blocked(
    menu: Option<Res<crate::graphics_menu::Menu>>,
    replay: &crate::replay::Replay,
    customiser: Option<Res<crate::customiser::Customiser>>,
    travel: Option<Res<crate::teleport_menu::Travel>>,
    mods: Option<Res<crate::modding::ModMenu>>,
    custom_models: Option<Res<crate::custom_models::CustomModels>>,
) -> bool {
    replay.active
        || !crate::graphics_menu::gameplay_active(menu)
        || customiser.is_some_and(|c| c.open)
        || travel.is_some_and(|t| t.open)
        || mods.is_some_and(|m| m.open)
        || custom_models.is_some_and(|m| m.open)
}

fn idle(buttons: u16, triggers: [f32; 2], sticks: [[f32; 2]; 2]) -> bool {
    buttons == 0
        && triggers.iter().all(|v| *v < 0.05)
        && sticks.iter().flatten().all(|v| v.abs() < 0.18)
}

/// `PollCameraControllerChords` + `DebugCam_OnCameraInput` at `CameraPresentation::Input`.
fn on_camera_input(
    mut debug: ResMut<DebugCam>,
    mut camera: ResMut<crate::camera::CameraRuntime>,
    mut skater: ResMut<crate::physics::SkaterRuntime>,
    input: Res<crate::input::ControllerInput>,
    replay: Res<crate::replay::Replay>,
    menu: Option<Res<crate::graphics_menu::Menu>>,
    customiser: Option<Res<crate::customiser::Customiser>>,
    travel: Option<Res<crate::teleport_menu::Travel>>,
    mods: Option<Res<crate::modding::ModMenu>>,
    custom_models: Option<Res<crate::custom_models::CustomModels>>,
    time: Res<Time<Fixed>>,
) {
    if replay.active && camera.manual_cam.manual_cam_active() {
        debug.want_toggle = true;
    }

    let raw = input.raw_input();
    if !gameplay_blocked(menu, &replay, customiser, travel, mods, custom_models) {
        if chord(raw.buttons, DPAD_RIGHT | R3, debug.previous_buttons) {
            debug.want_toggle = true;
        }
        if camera.manual_cam.manual_cam_active() && chord(raw.buttons, B, debug.previous_buttons) {
            debug.want_spawn = true;
        }
    }
    debug.previous_buttons = raw.buttons;

    let cur = camera.manual_cam.manual_cam_active();
    let want_toggle = debug.want_toggle;
    let want_spawn = debug.want_spawn;
    debug.want_toggle = false;
    debug.want_spawn = false;

    if want_toggle {
        let frame = camera.frame.clone();
        if cur {
            camera.manual_cam.set_manual_cam_mode(false, frame.as_ref());
            debug.release_controls = true;
        } else if frame.is_some() {
            camera.manual_cam.set_manual_cam_mode(true, frame.as_ref());
            debug.release_controls = true;
        }
    }
    if want_spawn && camera.manual_cam.manual_cam_active() {
        let matrix = camera.manual_cam.spawn_matrix();
        let frame = camera.frame.clone();
        camera.manual_cam.set_manual_cam_mode(false, frame.as_ref());
        debug.release_controls = true;
        if let Err(error) = skater.travel_to(matrix) {
            bevy::log::warn!("debug cam spawn: {error}");
        }
    }

    if camera.manual_cam.manual_cam_active() {
        let settings = camera.manual_cam_settings;
        let actions = manual_actions_from_raw(raw);
        camera
            .manual_cam
            .update_manual_cam(&actions, settings, time.delta_secs());
    }

    if debug.release_controls
        && !camera.manual_cam.manual_cam_active()
        && idle(raw.buttons, raw.triggers, [raw.left, raw.right])
    {
        debug.release_controls = false;
    }
}

fn manual_actions_from_raw(
    raw: crate::input::RawInput,
) -> skate_core::input::gameplay_map::GameplayActions {
    use skate_core::input::gameplay_map::GameplayActions;
    GameplayActions::from_values([
        raw.left[0],
        raw.left[1],
        f32::from(raw.buttons & 0x0040 != 0),
        raw.right[0],
        raw.right[1],
        0.0,
        raw.triggers[0],
        raw.triggers[1],
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    ])
}

#[derive(Component)]
struct DebugCamOverlay;

fn setup_overlay(mut commands: Commands) {
    commands.spawn((
        DebugCamOverlay,
        GlobalZIndex(6),
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            bottom: px(24),
            left: percent(8),
            width: percent(84),
            padding: UiRect::all(px(12)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.035, 0.05, 0.92)),
        Text::new(
            "DEBUG CAM   |   Left stick  Move   |   Right stick  Look   |   Triggers  Up / down   |   L3  Fast   |   B  Spawn at camera   |   D-pad Right + R3  Exit",
        ),
        TextFont {
            font_size: 14.,
            ..default()
        },
        TextColor(Color::srgb(0.75, 0.82, 0.86)),
    ));
}

fn update_overlay(
    camera: Res<crate::camera::CameraRuntime>,
    mut roots: Query<&mut Node, With<DebugCamOverlay>>,
) {
    // Only on change: Bevy counts every write as one, and lays the UI out again (findings 63 §6.2).
    let display = if camera.manual_cam.manual_cam_active() {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut roots {
        if node.display != display {
            node.display = display;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_needs_both_buttons_and_is_edge_only() {
        assert!(chord(DPAD_RIGHT | R3, DPAD_RIGHT | R3, 0));
        assert!(!chord(DPAD_RIGHT | R3, DPAD_RIGHT | R3, DPAD_RIGHT | R3));
        assert!(!chord(DPAD_RIGHT, DPAD_RIGHT | R3, 0));
        assert!(chord(DPAD_RIGHT | R3, DPAD_RIGHT | R3, DPAD_RIGHT));
        assert!(!chord(B, B, B));
        assert!(chord(B, B, 0));
    }
}
