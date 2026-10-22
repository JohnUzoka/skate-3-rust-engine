//! Original APT HUD rendered independently of the world's resolution scale.
use crate::{
    apt_scene, apt_vm::Value, config::Config, hud_runtime, physics::SkaterRuntime,
    render_options::RenderOptions,
};
use bevy::{
    asset::{RenderAssetUsages, embedded_asset},
    camera::{
        CameraOutputMode, RenderTarget, ScalingMode, Viewport,
        visibility::{NoFrustumCulling, RenderLayers},
    },
    prelude::*,
    render::render_resource::{
        AsBindGroup, BlendState, Extent3d, PrimitiveTopology, RenderPipelineDescriptor, ShaderType,
        TextureDimension, TextureFormat,
    },
    shader::ShaderRef,
    sprite_render::{AlphaMode2d, Material2d, Material2dPlugin, MeshMaterial2d},
    ui_render::UiMaterialPlugin,
    window::PrimaryWindow,
};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Copy, Debug, ShaderType)]
struct ColorTransform {
    multiply: Vec4,
    add: Vec4,
}
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
struct HudMaterial {
    #[uniform(0)]
    color: ColorTransform,
    #[texture(1)]
    #[sampler(2)]
    atlas: Handle<Image>,
}
impl Material2d for HudMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://skate3rust/hud_render.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}
/// The offscreen target already contains RGB multiplied by coverage. Applying
/// ImageNode's straight-alpha blend again suppresses the original soft glow.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
struct HudComposite {
    #[texture(0)]
    #[sampler(1)]
    image: Handle<Image>,
    /// 1 while the node covers only part of the screen (`RenderOptions::hud_rect`): each of its
    /// pixels then loads the image's pixel under it (hud_composite.wgsl).
    #[uniform(2)]
    load: u32,
}
impl UiMaterial for HudComposite {
    fn fragment_shader() -> ShaderRef {
        "embedded://skate3rust/hud_composite.wgsl".into()
    }
    fn specialize(descriptor: &mut RenderPipelineDescriptor, _: UiMaterialKey<Self>) {
        if let Some(fragment) = &mut descriptor.fragment {
            for target in fragment.targets.iter_mut().flatten() {
                target.blend = Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING);
            }
        }
    }
}
struct Slot {
    entity: Entity,
    mesh: Handle<Mesh>,
    material: Handle<HudMaterial>,
    /// What the mesh and material hold, so a frame that draws the same leaves them alone.
    draw: apt_scene::Draw,
    visible: bool,
}
#[derive(Resource)]
struct Hud {
    target: Handle<Image>,
    composite: Handle<HudComposite>,
    rebind_after_resize: bool,
    runtime: hud_runtime::Runtime,
    source: serde_json::Value,
    shapes: apt_scene::Shapes,
    textures: BTreeMap<String, Handle<Image>>,
    slots: Vec<Slot>,
    generation: u64,
    failed: bool,
    /// The camera that draws the slots into `target`, and the node that lays `target` over the
    /// screen (`finish_setup`).
    camera: Entity,
    composite_node: Entity,
    /// Whether the slots drew anything last frame, and whether `camera` and `composite_node`
    /// are on, by `RenderOptions::hud_retain` (`render`).
    drawn: bool,
    camera_on: bool,
    node_on: bool,
    /// By `RenderOptions::hud_rect`, the part of `target` that `camera` draws and
    /// `composite_node` covers, in physical pixels, and the target's size then; `None`, all of it
    /// (`render`).
    rect: Option<(URect, UVec2)>,
    /// How many times `rect` has changed, so that only the first few are logged.
    rect_changes: u32,
    /// Whether `SKATE_HUD_DEMO`'s trick has been shown since the runtime last loaded (`advance`).
    demo_shown: bool,
}
/// Same string the scoring HUD draws: language table first, then a readable fallback.
pub(crate) fn display_trick(world: &World, label: &str) -> String {
    if label.is_empty() {
        return String::new();
    }
    localize_trick(
        label,
        world
            .get_resource::<Hud>()
            .map(|hud| &hud.runtime.bindings.movie.text_assets),
    )
}

pub(crate) use crate::apt_text::localize_trick;


pub(crate) struct ScoringHudPlugin;
impl Plugin for ScoringHudPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "hud_render.wgsl");
        embedded_asset!(app, "hud_composite.wgsl");
        app.add_plugins((
            Material2dPlugin::<HudMaterial>::default(),
            UiMaterialPlugin::<HudComposite>::default(),
        ))
        .add_systems(
            PostStartup,
            setup.after(crate::graphics_menu::PresentationSetup),
        )
        .add_systems(
            FixedUpdate,
            advance
                .after(crate::app::SimulationSet::Physics)
                .run_if(crate::graphics_menu::gameplay_active),
        )
        .add_systems(
            Update,
            (finish_setup, resize_target, reset, render)
                .chain()
                .after(crate::app::FrameSet::Physics),
        );
    }
}
/// What the HUD reads from the SD card. Read on a thread of its own (`setup`): read in
/// `PostStartup`, beside the start map's and the audio's loaders, it held the first frame back
/// 2.4-6.1 s on the Switch (sk8 findings 59 §16).
struct HudFiles {
    runtime: hud_runtime::Runtime,
    source: serde_json::Value,
    shapes: apt_scene::Shapes,
    /// Each texture's path, size and RGBA8 pixels.
    textures: Vec<(String, [u32; 2], Vec<u8>)>,
}

/// The HUD's files being read, from `setup` until `finish_setup` builds the HUD from them.
#[derive(Resource)]
struct PendingHud {
    root: PathBuf,
    job: Option<std::thread::JoinHandle<Result<HudFiles, String>>>,
}

fn read_files(root: &std::path::Path, input: hud_runtime::Input) -> Result<HudFiles, String> {
    let source: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("runtime/trickdisplay.json"))
            .map_err(|e| format!("{}: {e}", root.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let runtime = hud_runtime::Runtime::load(&source, input)?;
    let shapes: apt_scene::Shapes =
        serde_json::from_value(source["shapes"].clone()).map_err(|e| e.to_string())?;
    let mut files = BTreeMap::new();
    for texture in shapes.values().flatten().filter_map(|s| s.texture.as_ref()) {
        files.insert(texture.rgba.clone(), [texture.width, texture.height]);
    }
    for font in runtime.bindings.movie.text_assets.fonts.values() {
        files.insert(font.texture.clone(), font.size);
        if let Some(fg) = &font.foreground {
            files.insert(fg.texture.clone(), fg.size);
        }
    }
    let mut textures = Vec::with_capacity(files.len());
    for (path, size) in files {
        let bytes = std::fs::read(root.join(&path)).map_err(|e| format!("HUD {path}: {e}"))?;
        if bytes.len() != size[0] as usize * size[1] as usize * 4 {
            return Err(format!("Invalid HUD texture size {path}"));
        }
        textures.push((path, size, bytes));
    }
    Ok(HudFiles { runtime, source, shapes, textures })
}

fn setup(mut commands: Commands, config: Res<Config>, skater: Res<SkaterRuntime>) {
    let root = std::env::var_os("SKATE_SCORING_HUD_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.asset_root.join("private/hud"));
    let input = skater.scoring.hud_input();
    let reading = root.clone();
    match std::thread::Builder::new().name("hud-files".into()).spawn(move || read_files(&reading, input)) {
        Ok(job) => commands.insert_resource(PendingHud { root, job: Some(job) }),
        Err(error) => error!("Original scoring HUD could not load from {}: no thread ({error})", root.display()),
    }
}

/// Builds the HUD once `setup`'s thread has read its files.
fn finish_setup(
    mut commands: Commands,
    pending: Option<ResMut<PendingHud>>,
    cameras: Query<Entity, With<IsDefaultUiCamera>>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut composites: ResMut<Assets<HudComposite>>,
) {
    let Some(mut pending) = pending else { return };
    if pending.job.as_ref().is_some_and(|job| !job.is_finished()) {
        return;
    }
    commands.remove_resource::<PendingHud>();
    let root = std::mem::take(&mut pending.root);
    let Some(job) = pending.job.take() else { return };
    let files = match job.join() {
        Ok(Ok(files)) => files,
        Ok(Err(error)) => {
            error!("Original scoring HUD could not load from {}: {error}. See docs/hud-installation.md", root.display());
            return;
        }
        Err(_) => {
            error!("Original scoring HUD could not load from {}: its thread panicked", root.display());
            return;
        }
    };
    let Ok(output) = cameras.single() else {
        error!("Original HUD requires the presentation camera");
        return;
    };
    let textures = files
        .textures
        .into_iter()
        .map(|(path, size, bytes)| {
            let image = Image::new(
                Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                bytes,
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::RENDER_WORLD,
            );
            (path, images.add(image))
        })
        .collect();
    let mut hud = Hud {
        target: Handle::default(),
        composite: Handle::default(),
        rebind_after_resize: false,
        runtime: files.runtime,
        source: files.source,
        shapes: files.shapes,
        textures,
        slots: Vec::new(),
        generation: 0,
        failed: false,
        camera: Entity::PLACEHOLDER,
        composite_node: Entity::PLACEHOLDER,
        drawn: false,
        camera_on: true,
        node_on: true,
        rect: None,
        rect_changes: 0,
        demo_shown: false,
    };
    let target = images.add(Image::new_target_texture(
        window.physical_width().max(1),
        window.physical_height().max(1),
        TextureFormat::Rgba8UnormSrgb,
        None,
    ));
    hud.target = target.clone();
    hud.camera = commands
        .spawn((
            Camera2d,
            Projection::Orthographic(OrthographicProjection {
                scaling_mode: ScalingMode::Fixed {
                    width: 1280.,
                    height: 720.,
                },
                ..OrthographicProjection::default_2d()
            }),
            Camera {
                // Before the gameplay camera, which draws the composite itself when it
                // draws into the window (graphics_menu.rs `choose_presentation`): at one
                // order, the cameras that draw into a window go first.
                order: -2,
                clear_color: ClearColorConfig::Custom(Color::NONE),
                ..default()
            },
            RenderTarget::Image(target.clone().into()),
            RenderLayers::layer(31),
            Msaa::Off,
        ))
        .id();
    hud.composite = composites.add(HudComposite {
        image: target,
        load: 0,
    });
    hud.composite_node = commands
        .spawn((
            MaterialNode(hud.composite.clone()),
            UiTargetCamera(output),
            GlobalZIndex(1),
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                ..default()
            },
        ))
        .id();
    commands.insert_resource(hud);
    info!("Original scoring HUD loaded from {}", root.display());
}

fn resize_target(
    hud: Option<ResMut<Hud>>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut composites: ResMut<Assets<HudComposite>>,
) {
    let Some(mut hud) = hud else {
        return;
    };
    // Bevy 0.18's UI material preparation has no dependency on GpuImage
    // preparation. Retry once on the following frame, when the replacement
    // image is available regardless of preparation order during the resize.
    if hud.rebind_after_resize {
        if let Some(material) = composites.get_mut(&hud.composite) {
            material.image = hud.target.clone();
        }
    }
    let size = Extent3d {
        width: window.physical_width().max(1),
        height: window.physical_height().max(1),
        depth_or_array_layers: 1,
    };
    hud.rebind_after_resize = resize_image(
        &mut images,
        &mut composites,
        &hud.target,
        &hud.composite,
        size,
    );
}
fn resize_image(
    images: &mut Assets<Image>,
    composites: &mut Assets<HudComposite>,
    target: &Handle<Image>,
    composite: &Handle<HudComposite>,
    size: Extent3d,
) -> bool {
    if images
        .get(target)
        .is_some_and(|image| image.texture_descriptor.size != size)
    {
        // Assets::get_mut emits Modified even without a write. Doing that
        // every frame recreates the GPU target behind the compositor's
        // cached bind group. Only invalidate the image on a real resize.
        if let Some(image) = images.get_mut(target) {
            image.resize(size);
        }
        // The UI material retains its bind group. A real target resize must
        // reprepare it so it samples the replacement GPU texture view.
        if let Some(material) = composites.get_mut(composite) {
            material.image = target.clone();
        }
        return true;
    }
    false
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hud_target_changes_only_on_resize_and_refreshes_composite() {
        // Asset scheduling only: no render plugin, window or gameplay systems.
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<HudComposite>();
        let target =
            app.world_mut()
                .resource_mut::<Assets<Image>>()
                .add(Image::new_target_texture(
                    1280,
                    720,
                    TextureFormat::Rgba8UnormSrgb,
                    None,
                ));
        let composite = app
            .world_mut()
            .resource_mut::<Assets<HudComposite>>()
            .add(HudComposite {
                image: target.clone(),
                load: 0,
            });
        app.update();
        app.world_mut()
            .resource_mut::<Messages<AssetEvent<Image>>>()
            .clear();
        app.world_mut()
            .resource_mut::<Messages<AssetEvent<HudComposite>>>()
            .clear();
        for (width, height, changed) in
            [(1280, 720, false), (1920, 1080, true), (1920, 1080, false)]
        {
            app.world_mut()
                .resource_scope(|world, mut images: Mut<Assets<Image>>| {
                    let mut composites = world.resource_mut::<Assets<HudComposite>>();
                    resize_image(
                        &mut images,
                        &mut composites,
                        &target,
                        &composite,
                        Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                    );
                });
            app.update();
            let images: Vec<_> = app
                .world_mut()
                .resource_mut::<Messages<AssetEvent<Image>>>()
                .drain()
                .collect();
            let materials: Vec<_> = app
                .world_mut()
                .resource_mut::<Messages<AssetEvent<HudComposite>>>()
                .drain()
                .collect();
            assert_eq!(
                images
                    .iter()
                    .any(|e| matches!(e, AssetEvent::Modified { id } if *id == target.id())),
                changed
            );
            assert_eq!(
                materials
                    .iter()
                    .any(|e| matches!(e, AssetEvent::Modified { id } if *id == composite.id())),
                changed
            );
        }
    }

    #[test]
    fn hud_rect_covers_the_draws_on_a_grid() {
        let size = UVec2::new(1280, 720);
        assert_eq!(hud_rect([], size), None);
        // A trick name at the bottom middle: 4 pixels more on each side, out to the grid.
        assert_eq!(
            hud_rect([[500., 600.], [780., 650.], [640., 620.]], size),
            Some(URect::new(496, 592, 784, 656))
        );
        // The grid stops at the screen's edges.
        assert_eq!(
            hud_rect([[2., 3.], [1279., 719.]], size),
            Some(URect::new(0, 0, 1280, 720))
        );
        // At 1920x1080, the same APT positions scale first.
        assert_eq!(
            hud_rect([[500., 600.], [780., 650.]], UVec2::new(1920, 1080)),
            Some(URect::new(736, 896, 1184, 992))
        );
        // Wholly off the screen covers none of it.
        assert_eq!(hud_rect([[1300., 10.], [1400., 20.]], size), None);
    }

    #[test]
    fn hud_view_keeps_each_draw_on_its_pixels() {
        let size = UVec2::new(1280, 720);
        assert_eq!(
            hud_view(URect::new(0, 0, 1280, 720), size),
            (Vec2::new(1280., 720.), Vec2::ZERO)
        );
        for (rect, size) in [
            (URect::new(496, 592, 784, 656), size),
            (URect::new(0, 16, 320, 720), size),
            (URect::new(736, 896, 1184, 992), UVec2::new(1920, 1080)),
        ] {
            let (area, centre) = hud_view(rect, size);
            // The view's corners in APT's space, as the whole target's view places them.
            let pixel = |apt: Vec2| apt * size.as_vec2() / Vec2::new(1280., 720.);
            let top_left = Vec2::new(centre.x - area.x / 2. + 640., 360. - centre.y - area.y / 2.);
            let bottom_right = top_left + area;
            assert!(pixel(top_left).abs_diff_eq(rect.min.as_vec2(), 1e-3), "{rect:?}");
            assert!(pixel(bottom_right).abs_diff_eq(rect.max.as_vec2(), 1e-3), "{rect:?}");
        }
    }
}
fn reset(
    hud: Option<ResMut<Hud>>,
    skater: Res<SkaterRuntime>,
    map: Res<crate::map_transition::CurrentMap>,
) {
    let Some(mut hud) = hud else {
        return;
    };
    if hud.generation == map.generation {
        return;
    }
    match hud_runtime::Runtime::load(&hud.source, skater.scoring.hud_input()) {
        Ok(runtime) => {
            hud.runtime = runtime;
            hud.generation = map.generation;
            hud.failed = false;
            hud.demo_shown = false;
        }
        Err(error) => {
            error!("Original scoring HUD reset: {error}");
            hud.failed = true;
        }
    }
}
fn advance(
    hud: Option<ResMut<Hud>>,
    skater: Res<SkaterRuntime>,
    map: Res<crate::map_transition::CurrentMap>,
) {
    let Some(mut hud) = hud else {
        return;
    };
    if hud.failed || hud.generation != map.generation {
        return;
    }
    let mut input = skater.scoring.hud_input();
    let mut events = (
        skater.scoring.new_trick,
        skater.scoring.modified_trick,
        skater.scoring.close_tricks,
    );
    if let Some(label) = demo_trick() {
        let open = !hud.demo_shown;
        input.trick_name = label.to_owned();
        input.trick_metrics[0] = Value::Text(label.to_owned());
        input.trick_metrics[4] = Value::Bool(open);
        input.sequence_score = 1234;
        input.multiplier = 2.;
        events = (open, false, false);
        hud.demo_shown = true;
    }
    let (new_trick, modified_trick, close_tricks) = events;
    if let Err(error) = hud
        .runtime
        .update(input, new_trick, modified_trick, close_tricks)
    {
        error!("Original scoring HUD stopped: {error}");
        hud.failed = true;
    }
}
/// `SKATE_HUD_DEMO`, to check the HUD where nothing scores, such as an emulator's standing
/// skater: the trick the display opens on and keeps until the map reloads, scoring 1,234 at
/// 2x. A leading `#` shows the label as written (apt_text.rs `localize_trick`).
fn demo_trick() -> Option<&'static str> {
    static DEMO: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    DEMO.get_or_init(|| {
        std::env::var("SKATE_HUD_DEMO")
            .ok()
            .filter(|label| !label.is_empty())
    })
    .as_deref()
}
fn render(
    mut commands: Commands,
    hud: Option<ResMut<Hud>>,
    options: Option<Res<RenderOptions>>,
    window: Single<&Window, With<PrimaryWindow>>,
    ui_scale: Res<UiScale>,
    mut cameras: Query<(&mut Camera, &mut Projection, &mut Transform)>,
    mut nodes: Query<&mut Node>,
    mut composites: ResMut<Assets<HudComposite>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<HudMaterial>>,
) {
    let Some(mut hud) = hud else {
        return;
    };
    let draws = if hud.failed {
        Vec::new()
    } else {
        match apt_scene::draw(&hud.runtime.bindings.movie, &hud.runtime.vm, &hud.shapes) {
            Ok(draws) => draws,
            Err(error) => {
                error!("Original HUD geometry: {error}");
                hud.failed = true;
                Vec::new()
            }
        }
    };
    let count = draws.len();
    for (index, draw) in draws.into_iter().enumerate() {
        let Some(texture) = hud.textures.get(&draw.texture).cloned() else {
            continue;
        };
        if index == hud.slots.len() {
            let mesh = meshes.add(hud_mesh(&draw));
            let material = materials.add(hud_material(&draw, texture));
            let entity = commands
                .spawn((
                    Mesh2d(mesh.clone()),
                    MeshMaterial2d(material.clone()),
                    Transform::from_xyz(0., 0., index as f32 * 0.01),
                    RenderLayers::layer(31),
                    // `hud_rect` narrows the view to what the slots cover now. Rather than depend
                    // on when each slot's bounds catch up with its mesh, slots skip culling: off
                    // the target, the GPU clips them.
                    NoFrustumCulling,
                ))
                .id();
            hud.slots.push(Slot {
                entity,
                mesh,
                material,
                draw,
                visible: true,
            });
        } else {
            // Assets::get_mut marks the asset modified, which uploads the mesh again and
            // prepares the material again, so only what changed is written.
            let slot = &mut hud.slots[index];
            if slot.draw.vertices != draw.vertices {
                if let Some(old) = meshes.get_mut(&slot.mesh) {
                    *old = hud_mesh(&draw);
                }
            }
            if (&slot.draw.texture, slot.draw.multiply, slot.draw.add)
                != (&draw.texture, draw.multiply, draw.add)
            {
                if let Some(old) = materials.get_mut(&slot.material) {
                    *old = hud_material(&draw, texture);
                }
            }
            slot.draw = draw;
            if !slot.visible {
                commands.entity(slot.entity).insert(Visibility::Visible);
                slot.visible = true;
            }
        }
    }
    for slot in hud.slots.iter_mut().skip(count) {
        if slot.visible {
            commands.entity(slot.entity).insert(Visibility::Hidden);
            slot.visible = false;
        }
    }
    // With nothing to draw the target is clear, and its premultiplied composite leaves the
    // screen as it was. `hud_retain` then stops the composite and, after one more frame to clear
    // the target, the camera, so whichever renders first when the HUD returns, the composite
    // never reads a stale trick.
    let (retain, rect_on) = options.map_or_else(
        || {
            let defaults = RenderOptions::default();
            (defaults.hud_retain, defaults.hud_rect)
        },
        |options| (options.hud_retain, options.hud_rect),
    );
    let drawn = hud.slots.iter().any(|slot| slot.visible);
    let (camera_on, node_on) = (!retain || drawn || hud.drawn, !retain || drawn);
    hud.drawn = drawn;
    if camera_on != hud.camera_on {
        hud.camera_on = camera_on;
        if let Ok((mut camera, ..)) = cameras.get_mut(hud.camera) {
            camera.is_active = camera_on;
        }
    }
    if node_on != hud.node_on {
        hud.node_on = node_on;
        commands.entity(hud.composite_node).insert(if node_on {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
    // `hud_rect`: the camera draws, and copies into the target, only round what the slots cover
    // while they draw, and the composite covers no more. The rest of the target keeps whatever
    // it held, which the composite no longer reads. Whether the composite loads by position
    // follows the option, not the rect, so that the two never disagree for a frame.
    if composites
        .get(&hud.composite)
        .is_some_and(|composite| (composite.load != 0) != rect_on)
        && let Some(composite) = composites.get_mut(&hud.composite)
    {
        composite.load = rect_on.into();
    }
    let size = UVec2::new(window.physical_width().max(1), window.physical_height().max(1));
    let kept = hud.rect.filter(|&(_, at)| at == size);
    let rect = match (rect_on, drawn) {
        (false, _) => None,
        (true, true) => {
            let positions = hud
                .slots
                .iter()
                .filter(|slot| slot.visible)
                .flat_map(|slot| slot.draw.vertices.iter().map(|vertex| vertex.position));
            hud_rect(positions, size).map(|rect| (rect, size)).or(kept)
        }
        (true, false) => kept,
    };
    if rect == hud.rect {
        return;
    }
    hud.rect = rect;
    hud.rect_changes += 1;
    if let Ok((mut camera, mut projection, mut transform)) = cameras.get_mut(hud.camera) {
        let (area, centre) = rect.map_or((Vec2::new(1280., 720.), Vec2::ZERO), |(rect, size)| {
            hud_view(rect, size)
        });
        camera.viewport = rect.map(|(rect, _)| Viewport {
            physical_position: rect.min,
            physical_size: rect.size(),
            ..default()
        });
        // The copy writes only the viewport, so it needn't clear the target first.
        camera.output_mode = match rect {
            Some(_) => CameraOutputMode::Write {
                blend_state: None,
                clear_color: ClearColorConfig::None,
            },
            None => CameraOutputMode::default(),
        };
        if let Projection::Orthographic(projection) = &mut *projection {
            projection.scaling_mode = ScalingMode::Fixed {
                width: area.x,
                height: area.y,
            };
        }
        transform.translation.x = centre.x;
        transform.translation.y = centre.y;
    }
    if let Ok(mut node) = nodes.get_mut(hud.composite_node) {
        match rect {
            Some((rect, size)) => {
                // 2 pixels inside the camera's viewport, except at the screen's edges, so that
                // rounding the layout never shows a pixel the camera didn't write this frame.
                let inset = |edge: bool| if edge { 0 } else { 2 };
                let min = rect.min + UVec2::new(inset(rect.min.x == 0), inset(rect.min.y == 0));
                let max = rect.max.saturating_sub(UVec2::new(
                    inset(rect.max.x == size.x),
                    inset(rect.max.y == size.y),
                ));
                let extent = max.saturating_sub(min).max(UVec2::ONE).as_vec2();
                let scale = window.scale_factor() * ui_scale.0;
                node.left = px(min.x as f32 / scale);
                node.top = px(min.y as f32 / scale);
                node.width = px(extent.x / scale);
                node.height = px(extent.y / scale);
            }
            None => {
                node.left = Val::Auto;
                node.top = Val::Auto;
                node.width = percent(100);
                node.height = percent(100);
            }
        }
    }
    let changes = hud.rect_changes;
    if changes <= 16 || changes % 256 == 0 {
        let what = rect.map_or("the whole screen".to_owned(), |(rect, size)| {
            let share = 100. * rect.size().as_vec2().element_product()
                / size.as_vec2().element_product();
            let (at, extent) = (rect.min, rect.size());
            format!("{}x{} at {},{}, {share:.1}% of the screen", extent.x, extent.y, at.x, at.y)
        });
        info!("[Skate] scoring HUD rect {changes}: {what}");
    }
}
/// The part of a `size` target that `positions`, in APT's 1280x720 space, cover: 4 pixels more
/// on each side, out to a 16-pixel grid, and `None` when that leaves none of the target.
fn hud_rect(positions: impl IntoIterator<Item = [f32; 2]>, size: UVec2) -> Option<URect> {
    let (mut min, mut max) = (Vec2::INFINITY, Vec2::NEG_INFINITY);
    for position in positions {
        min = min.min(position.into());
        max = max.max(position.into());
    }
    let scale = size.as_vec2() / Vec2::new(1280., 720.);
    let low = (((min * scale - 4.) / 16.).floor() * 16.).max(Vec2::ZERO);
    let high = (((max * scale + 4.) / 16.).ceil() * 16.).min(size.as_vec2());
    low.cmplt(high)
        .all()
        .then(|| URect::from_corners(low.as_uvec2(), high.as_uvec2()))
}
/// The HUD camera's projection size and position, in APT's space, when it draws `rect` of a
/// `size` target: what it draws lands on the pixels it would over the whole target.
fn hud_view(rect: URect, size: UVec2) -> (Vec2, Vec2) {
    let scale = Vec2::new(1280., 720.) / size.as_vec2();
    let extent = rect.size().as_vec2();
    let centre = rect.min.as_vec2() + extent / 2. - size.as_vec2() / 2.;
    (extent * scale, Vec2::new(centre.x, -centre.y) * scale)
}
fn hud_mesh(draw: &apt_scene::Draw) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        draw.vertices
            .iter()
            .map(|v| [v.position[0] - 640., 360. - v.position[1], 0.])
            .collect::<Vec<_>>(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        draw.vertices.iter().map(|v| v.uv).collect::<Vec<_>>(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0., 0., 1.]; draw.vertices.len()],
    );
    mesh
}
fn hud_material(draw: &apt_scene::Draw, atlas: Handle<Image>) -> HudMaterial {
    HudMaterial {
        color: ColorTransform {
            multiply: draw.multiply.into(),
            add: draw.add.into(),
        },
        atlas,
    }
}
