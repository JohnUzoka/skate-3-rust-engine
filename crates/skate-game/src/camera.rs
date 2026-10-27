//! Normal gameplay camera rendering endpoint. Simulation owns the cadence of
//! CameraRuntime::advance after its complete physical/animation publication.
mod settings;
mod collision;
mod world_query;
mod shot_data;
mod stock_names;
mod shake_data;
mod trajectory;
mod subject;
mod graph_subject;
mod graph_conditions;
mod graph;
mod runtime;
mod publication;
pub(crate) mod angle;
pub(crate) use publication::{
    snapshot as publish_camera_subject, CameraPublicationInputs, CameraStateOutput,
    CameraAnimationOutput, CameraAirOutput, CameraOffboardOutput, CameraGrindOutput,
    CameraEventsOutput, CameraPreferences,
};
pub(crate) use graph_subject::CameraGraphEnvironment;
pub(crate) use runtime::CameraRuntime;
pub(crate) use angle::{CameraAngle, CameraAngleSettings};
use bevy::prelude::*;
use crate::{app::FrameSet, config::Config};

#[derive(Component)]
pub(crate) struct GameplayCamera;

pub(crate) struct CameraPlugin;
impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn)
            // After the pause menu, before the fixed simulation steps that advance the camera.
            .add_systems(PreUpdate, angle::sync_runtime.after(crate::graphics_menu::MenuInput))
            .add_systems(Update, present.after(FrameSet::Animation).before(FrameSet::Verification));
    }
}
fn spawn(mut commands: Commands, config: Res<Config>, retail: Res<crate::retail_render::RetailScene>,
    options: Option<Res<crate::render_options::RenderOptions>>) {
    let runtime = CameraRuntime::load(&config.asset_root)
        .unwrap_or_else(|error| panic!("Cannot initialize normal gameplay camera: {error}"));
    commands.insert_resource(runtime);
    commands.insert_resource(CameraAngleSettings::load(&config.asset_root));
    let mut camera = commands.spawn((
        GameplayCamera,
        Camera3d::default(),
        Camera { is_active: false, ..default() },
        Transform::default(),
    ));
    if retail.0 {
        camera.insert((bevy::render::view::Hdr, bevy::core_pipeline::tonemapping::Tonemapping::None,
            crate::retail_render::RetailTone::default()));
    } else {
        let options = options.map_or_else(crate::render_options::RenderOptions::default, |o| *o);
        camera.insert(options.procedural_tonemapping);
        if options.procedural_hdr {
            camera.insert(bevy::render::view::Hdr);
        }
    }
}

pub(crate) fn present(mut runtime: ResMut<CameraRuntime>, windows: Query<&Window>,
    history: Res<crate::presentation::Presentation>, time: Res<Time<Fixed>>,
    replay: Res<crate::replay::Replay>,
    customiser: Option<Res<crate::customiser::Customiser>>,
    config: Option<Res<Config>>, current: Option<Res<crate::map_transition::CurrentMap>>,
    mut cameras: Query<(&mut Camera, &mut Transform, &mut Projection), With<GameplayCamera>>) {
    // Off while the Switch's start map loads behind the loading screen: there is nothing to draw
    // yet. Switched off here, not just left alone: the menu's setup replaces its `Camera` with one
    // that is on.
    if config.is_some_and(|c| c.boot_map.is_some()) && current.is_none_or(|c| c.generation == 0) {
        for (mut camera, ..) in &mut cameras {
            if camera.is_active {
                camera.is_active = false;
            }
        }
        return;
    }
    if let Ok(window) = windows.single() {
        // A minimized window reports 0x0, and 0/0 is NaN. That poisons the
        // projection's field of view and trips the non-finite frame check, so
        // keep the last good ratio until the window has area again.
        let ratio = window.width() / window.height();
        if ratio.is_finite() && ratio > 0. {
            runtime.set_aspect_ratio(ratio);
        }
    }
    let Some((previous, current, alpha)) = history.view(&replay, time.overstep_fraction()) else { return; };
    let manual_pose = crate::debug_cam::DebugCam::pose(&runtime);
    for (mut camera, mut transform, mut projection) in &mut cameras {
        *transform = crate::presentation::blend(previous.camera, current.camera, alpha);
        if replay.active {
            if let Some(free) = replay.free_camera { *transform = free; }
        } else if let Some(pose) = manual_pose {
            *transform = pose;
        }
        if let Projection::Perspective(p) = &mut *projection {
            p.fov = previous.fov + (current.fov - previous.fov) * alpha;
        }
        if let Some(customiser) = customiser.as_ref().filter(|c| c.open) {
            let (height,distance,yaw)=customiser.preview_camera();
            let center = current.root.translation + Vec3::Y * height;
            let offset = current.root.rotation * Quat::from_rotation_y(yaw) * Vec3::new(0.0, 0.25, distance);
            let eye = center + offset;
            let right = offset.normalize().cross(Vec3::Y).normalize();
            *transform = Transform::from_translation(eye).looking_at(center + right * (distance * 0.265625), Vec3::Y);
            if let Projection::Perspective(p) = &mut *projection { p.fov = 50_f32.to_radians(); }
        }
        camera.is_active = true;
    }
}

/// Keep the presentation camera/target/settings while replacing world-specific
/// postprocessing. A procedural scene must not inherit the retail tone; it keeps HDR only when
/// `RenderOptions::procedural_hdr` says so (render_options.rs `SKATE_PROCEDURAL_HDR`).
pub(crate) fn set_world_environment(world: &mut World, retail: bool) {
    let options = world
        .get_resource::<crate::render_options::RenderOptions>()
        .copied()
        .unwrap_or_default();
    let cameras: Vec<_> = world.query_filtered::<Entity, With<GameplayCamera>>().iter(world).collect();
    for id in cameras {
        let mut camera = world.entity_mut(id);
        if retail {
            camera.insert((bevy::render::view::Hdr, bevy::core_pipeline::tonemapping::Tonemapping::None,
                crate::retail_render::RetailTone::default()));
        } else {
            camera.remove::<crate::retail_render::RetailTone>();
            camera.insert(options.procedural_tonemapping);
            if options.procedural_hdr {
                camera.insert(bevy::render::view::Hdr);
            } else {
                camera.remove::<bevy::render::view::Hdr>();
            }
        }
        camera.get_mut::<Camera>().unwrap().is_active = false;
        *camera.get_mut::<Transform>().unwrap() = Transform::default();
    }
    if !retail {
        info!(
            "[Skate] camera: a scene without a retail map, through {}, tonemapping {:?} {}",
            if options.procedural_hdr { "an HDR camera" } else { "an 8-bit camera" },
            options.procedural_tonemapping,
            if options.procedural_hdr { "in a pass of its own" } else { "in each material's shader" },
        );
    }
}

#[cfg(test)]
mod environment_tests {
    use super::*;
    #[test]
    fn native_procedural_switch_clears_tone_hdr_and_retains_camera_settings() {
        let mut world = World::new();
        let id = world.spawn((GameplayCamera, Camera3d::default(), Msaa::Sample4,
            bevy::camera::RenderTarget::default())).id();
        for retail in [true, false, true, false] {
            set_world_environment(&mut world, retail);
            let entity = world.entity(id);
            assert_eq!(entity.contains::<bevy::render::view::Hdr>(), retail);
            assert_eq!(entity.contains::<crate::retail_render::RetailTone>(), retail);
            assert_eq!(*entity.get::<Msaa>().unwrap(), Msaa::Sample4);
            assert!(!entity.get::<Camera>().unwrap().is_active);
            assert_eq!(*entity.get::<bevy::core_pipeline::tonemapping::Tonemapping>().unwrap(),
                if retail { bevy::core_pipeline::tonemapping::Tonemapping::None }
                else { bevy::core_pipeline::tonemapping::Tonemapping::default() });
        }
    }
}
