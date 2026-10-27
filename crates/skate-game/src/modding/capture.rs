//! Extra cameras that write a named render target for `sdk.graphics.mesh_buffer`.
use bevy::{
    camera::{visibility::RenderLayers, RenderTarget},
    image::ImageSampler,
    prelude::*,
    render::{render_resource::TextureFormat, view::Hdr},
};
use skate_mods::CaptureOptions;
use std::collections::BTreeMap;

pub(super) const SCREEN_LAYER: usize = 31;

// Capture surfaces are excluded from every capture camera to avoid GPU feedback.
pub(super) fn show_screens(world: &mut World) {
    let cameras: Vec<_> = world.query_filtered::<(Entity, Option<&RenderLayers>), With<crate::camera::GameplayCamera>>()
        .iter(world).map(|(e, layers)| (e, layers.cloned().unwrap_or_default().with(SCREEN_LAYER))).collect();
    for (e, layers) in cameras { world.entity_mut(e).insert(layers); }
}

type Key = (String, String);
const MAX_PER_MOD: usize = 2;
const MAX_TOTAL: usize = 4;

struct Capture {
    entity: Entity,
    image: Handle<Image>,
    width: u32,
    height: u32,
}

#[derive(Resource, Default)]
struct ModCaptures {
    slots: BTreeMap<Key, Capture>,
}

#[derive(Component)]
struct CaptureCamera;

pub(super) fn install(app: &mut App) {
    app.init_resource::<ModCaptures>();
}

pub(super) fn texture(world: &World, owner: &str, key: &str) -> Option<Handle<Image>> {
    world
        .get_resource::<ModCaptures>()?
        .slots
        .get(&(owner.to_owned(), key.to_owned()))
        .map(|c| c.image.clone())
}

pub(super) fn set(
    world: &mut World,
    owner: &str,
    key: String,
    options: CaptureOptions,
) -> Result<Handle<Image>, String> {
    let look = Vec3::from_array(options.look_at);
    let eye = Vec3::from_array(options.position);
    if (look - eye).length_squared() < 1e-6 {
        return Err("capture look_at must differ from position".into());
    }
    let slot = (owner.to_owned(), key);
    let env = gameplay_environment(world);
    world.resource_scope(|world, mut state: Mut<ModCaptures>| {
        if !state.slots.contains_key(&slot)
            && (state.slots.len() >= MAX_TOTAL
                || state.slots.keys().filter(|(o, _)| o == owner).count() >= MAX_PER_MOD)
        {
            return Err("Camera capture limit reached (2/mod, 4 total)".into());
        }
        if let Some(existing) = state.slots.get(&slot) {
            if existing.width == options.width && existing.height == options.height {
                pose_camera(world, existing.entity, eye, look, options.fov);
                return Ok(existing.image.clone());
            }
        }
        if let Some(old) = state.slots.remove(&slot) {
            world.despawn(old.entity);
            world.resource_mut::<Assets<Image>>().remove(old.image.id());
        }
        let mut image = Image::new_target_texture(
            options.width,
            options.height,
            TextureFormat::Rgba8UnormSrgb,
            None,
        );
        image.sampler = ImageSampler::linear();
        let handle = world.resource_mut::<Assets<Image>>().add(image);
        let mut entity = world.spawn((
            CaptureCamera,
            Camera3d::default(),
            Camera {
                order: -8,
                is_active: true,
                clear_color: ClearColorConfig::Custom(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                ..default()
            },
            Projection::from(PerspectiveProjection {
                fov: options.fov,
                aspect_ratio: options.width as f32 / options.height as f32,
                ..default()
            }),
            Transform::from_translation(eye).looking_at(look, Vec3::Y),
            RenderTarget::Image(handle.clone().into()),
            Msaa::Off,
        ));
        entity.insert(env.tonemapping);
        if env.hdr {
            entity.insert(Hdr);
        }
        if let Some(tone) = env.tone {
            entity.insert(tone);
        }
        entity.insert(env.layers.unwrap_or_default().without(SCREEN_LAYER));
        let id = entity.id();
        state.slots.insert(
            slot,
            Capture {
                entity: id,
                image: handle.clone(),
                width: options.width,
                height: options.height,
            },
        );
        Ok(handle)
    })
}

pub(super) fn remove(world: &mut World, owner: &str, key: &str) {
    world.resource_scope(|world, mut state: Mut<ModCaptures>| {
        if let Some(old) = state.slots.remove(&(owner.to_owned(), key.to_owned())) {
            world.despawn(old.entity);
            world.resource_mut::<Assets<Image>>().remove(old.image.id());
        }
    });
}

pub(super) fn clear_owner(world: &mut World, owner: &str) {
    world.resource_scope(|world, mut state: Mut<ModCaptures>| {
        let keys: Vec<_> = state
            .slots
            .keys()
            .filter(|(o, _)| o == owner)
            .cloned()
            .collect();
        for key in keys {
            if let Some(old) = state.slots.remove(&key) {
                world.despawn(old.entity);
                world.resource_mut::<Assets<Image>>().remove(old.image.id());
            }
        }
    });
}

pub(super) fn clear(world: &mut World) {
    world.resource_scope(|world, mut state: Mut<ModCaptures>| {
        for (_, old) in std::mem::take(&mut state.slots) {
            world.despawn(old.entity);
            world.resource_mut::<Assets<Image>>().remove(old.image.id());
        }
    });
}

struct GameplayEnv {
    hdr: bool,
    tonemapping: bevy::core_pipeline::tonemapping::Tonemapping,
    tone: Option<crate::retail_render::RetailTone>,
    layers: Option<RenderLayers>,
}

fn gameplay_environment(world: &mut World) -> GameplayEnv {
    let mut query = world.query_filtered::<(
        Option<&Hdr>,
        Option<&bevy::core_pipeline::tonemapping::Tonemapping>,
        Option<&crate::retail_render::RetailTone>,
        Option<&RenderLayers>,
    ), With<crate::camera::GameplayCamera>>();
    let Some((hdr, tonemapping, tone, layers)) = query.iter(world).next() else {
        return GameplayEnv {
            hdr: false,
            tonemapping: default(),
            tone: None,
            layers: None,
        };
    };
    GameplayEnv {
        hdr: hdr.is_some(),
        tonemapping: tonemapping.copied().unwrap_or_default(),
        tone: tone.copied(),
        layers: layers.cloned(),
    }
}

fn pose_camera(world: &mut World, entity: Entity, eye: Vec3, look: Vec3, fov: f32) {
    if let Some(mut transform) = world.get_mut::<Transform>(entity) {
        *transform = Transform::from_translation(eye).looking_at(look, Vec3::Y);
    }
    if let Some(mut projection) = world.get_mut::<Projection>(entity) {
        if let Projection::Perspective(p) = &mut *projection {
            p.fov = fov;
        }
    }
}
