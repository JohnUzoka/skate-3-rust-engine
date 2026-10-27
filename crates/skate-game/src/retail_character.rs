//! Character SH/key/rim shader, independent of the world's additive lights.
use crate::retail_irradiance::Irradiance;
use bevy::{
    asset::embedded_asset,
    camera::visibility::RenderLayers,
    gltf::GltfMaterialName,
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
    shader::ShaderRef,
};
use serde::Deserialize;
use std::collections::HashMap;

pub(crate) struct CharacterLightingPlugin;
impl Plugin for CharacterLightingPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "retail_character.wgsl");
        embedded_asset!(app, "retail_character_depth.wgsl");
        embedded_asset!(app, "retail_character_common.wgsl");
        let common: Handle<Shader> = bevy::asset::load_embedded_asset!(app.world().resource::<AssetServer>(), "retail_character_common.wgsl");
        app.insert_resource(CharacterShader(common));
        app.add_plugins(MaterialPlugin::<CharacterMaterial>::default())
            // After the scene build (world.rs `spawn`), so the lights `load` counts are there.
            .add_systems(Startup, load.after(crate::world::spawn))
            .add_systems(PreUpdate, load.after(crate::map_transition::MapTransitionSet).run_if(crate::retail_render::world_changed))
            .add_systems(Update, (bind, shadow_views).chain())
            .add_systems(
                Update,
                update.after(crate::multiplayer::RemoteRenderSet),
            )
            .add_systems(
                PostUpdate,
                (
                    crate::retail_shadow_bounds::caster_bounds,
                    crate::retail_shadow_bounds::sun_receivers.after(crate::render_options::apply),
                )
                    .chain()
                    .after(bevy::transform::TransformSystems::Propagate)
                    .after(bevy::mesh::InheritWeightSystems)
                    .after(bevy::asset::AssetEventSystems)
                    .after(bevy::light::SimulationLightSystems::UpdateDirectionalLightCascades)
                    .after(bevy::light::SimulationLightSystems::CheckLightVisibility),
            );
    }
}
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct MaterialData {
    pub shader: String,
    pub params: Vec<[f32; 4]>,
    pub specular: Option<String>,
}
impl MaterialData {
    pub(crate) fn is_hair(&self) -> bool {
        matches!(self.shader.as_str(), "character.hair" | "character.hair_ropa"
            | "character.default_hair" | "character.default_hair_ropa")
    }
}

#[derive(Resource)]
struct CharacterShader(#[allow(dead_code)] Handle<Shader>);
#[derive(Deserialize)]
struct LightingData {
    materials: HashMap<String, MaterialData>,
    default_sh: [[f32; 3]; 9],
    #[serde(default)]
    native: HashMap<String, HashMap<String, MaterialData>>,
}
#[derive(Resource)]
pub(crate) struct Lighting {
    data: LightingData,
    probes: Irradiance,
    pub(crate) light: Vec4,
    display_sh: Option<[Vec4; 9]>,
}

impl Lighting {
    pub(crate) fn default_sh(&self) -> [Vec4; 9] {
        std::array::from_fn(|i| Vec3::from_array(self.data.default_sh[i]).extend(0.))
    }
}
#[derive(Clone, Debug, Default, ShaderType)]
pub(crate) struct CharacterParams {
    pub light: Vec4,
    pub tint: Vec4,
    // normal map present, dedicated specular mask, alpha cutoff, hair family
    pub options: Vec4,
    pub rows: [Vec4; 9],
    pub sh: [Vec4; 9],
}
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct CharacterMaterial {
    #[uniform(0)]
    params: CharacterParams,
    #[texture(1)]
    #[sampler(2)]
    diffuse: Option<Handle<Image>>,
    #[texture(3)]
    #[sampler(4)]
    normal: Option<Handle<Image>>,
    #[texture(5)]
    #[sampler(6)]
    mask: Option<Handle<Image>>,
    alpha: AlphaMode,
}
impl Material for CharacterMaterial {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from(bevy::asset::embedded_path!("retail_character.wgsl")).with_source("embedded").into()
    }
    fn prepass_fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from(bevy::asset::embedded_path!("retail_character_depth.wgsl")).with_source("embedded").into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        self.alpha
    }
}
#[derive(Component)]
struct ShadowSource;

/// The light whose map the baked world samples for the shadows of the skater,
/// the board and mod graphics (retail_world.wgsl). `caster_bounds`
/// (retail_shadow_bounds.rs) bounds what it draws.
#[derive(Component)]
pub(crate) struct BakedWorldShadows;

/// Package-owned graphics retain their authored PBR materials.
#[derive(Component)]
pub(crate) struct ModGraphicsLit;

#[derive(Component)]
struct OriginalCharacterMaterial {
    material: Handle<StandardMaterial>,
    layers: Option<RenderLayers>,
}
/// What lights the skater on a map without retail lighting (`SKATE_PROCEDURAL_CHARACTER`): a sun
/// straight overhead, at the exposure the retail scenes' character light has.
const PROCEDURAL_LIGHT: Vec4 = Vec4::new(0., 1., 0., 2.5);

fn load(mut commands: Commands, config: Res<crate::config::Config>, sources: Query<Entity, With<ShadowSource>>, mut shadow: ResMut<crate::retail_render::FrameStateData>,
    retail: Res<crate::retail_render::RetailScene>,
    options: Option<Res<crate::render_options::RenderOptions>>,
    originals: Query<(Entity, &OriginalCharacterMaterial)>,
    cameras: Query<(Entity, &RenderLayers), With<crate::camera::GameplayCamera>>,
    mut customiser: ResMut<Assets<crate::customiser_material::SkaterMaterial>>,
    lights: Query<(), (Or<(With<DirectionalLight>, With<PointLight>, With<SpotLight>)>, Without<ShadowSource>)>,
) {
    for (_, material) in customiser.iter_mut() { material.extension.retail.light = Vec4::ZERO; }
    commands.remove_resource::<Lighting>();
    for e in &sources { commands.entity(e).try_despawn(); }
    // Only the floor: the clock and ocean PCA in this resource are rewritten
    // every frame and are not this system's to reset.
    shadow.shadow = Vec4::ZERO;
    // A map change runs this alongside the transition that despawns the skater it
    // is restoring, and the despawn lands first. Restoring a material on an
    // entity that is already gone is the expected case, not an error.
    for (entity, original) in &originals {
        let mut entity = commands.entity(entity);
        entity.try_remove::<(MeshMaterial3d<CharacterMaterial>, OriginalCharacterMaterial)>()
            .try_insert(MeshMaterial3d(original.material.clone()));
        if let Some(layers) = &original.layers { entity.try_insert(layers.clone()); }
        else { entity.try_remove::<RenderLayers>(); }
    }
    for (entity, layers) in &cameras {
        commands.entity(entity).try_insert(layers.clone().without(28));
    }
    if !retail.0 {
        // `SKATE_PROCEDURAL_CHARACTER` (render_options.rs): unset, the skater takes the character
        // shader when the scene has no light of its own, where Bevy's lit material would have
        // only the ambient light. It gets the default probe, a sun overhead and no shadow sources.
        // The retail suns despawned above are still in the world until the commands apply, so
        // `lights` leaves them out.
        let setting = options.as_ref().and_then(|options| options.procedural_character);
        let lit = !lights.is_empty();
        let why = match setting {
            Some(true) => "SKATE_PROCEDURAL_CHARACTER=1",
            Some(false) => "SKATE_PROCEDURAL_CHARACTER=0",
            None if lit => "the scene has a light of its own",
            None => "the scene has no light of its own",
        };
        if !setting.unwrap_or(!lit) {
            info!("SKATE_CHARACTER_LIGHTING: no retail map, and {why}, so the skater keeps Bevy's lit material");
            return;
        }
        match read_lighting_data(&config.asset_root) {
            Ok(data) => {
                info!("SKATE_CHARACTER_LIGHTING: no retail map, and {why}, so the skater takes the character shader, lit by the default probe and a sun overhead");
                commands.insert_resource(Lighting {
                    data,
                    probes: Irradiance::default(),
                    light: PROCEDURAL_LIGHT,
                    display_sh: None,
                });
            }
            Err(e) => warn!("SKATE_CHARACTER_LIGHTING: {e}"),
        }
        return;
    }
    let Some(map_path) = &config.map_path else {
        return;
    };
    let data = match read_lighting_data(&config.asset_root) {
        Ok(data) => data,
        Err(e) => {
            warn!("SKATE_CHARACTER_LIGHTING: {e}");
            return;
        }
    };
    let name = map_path.file_stem().unwrap_or_default().to_string_lossy();
    let overlay = config
        .asset_root
        .join("private/native-lighting")
        .join(format!("{name}.irradiance"));
    let probe_path = if overlay.is_file() {
        overlay
    } else {
        map_path.with_extension("irradiance")
    };
    let probes = match std::fs::read(probe_path)
        .map_err(|e| e.to_string())
        .and_then(|b| Irradiance::parse(&b))
    {
        Ok(probes) => probes,
        Err(e) => {
            warn!("SKATE_CHARACTER_LIGHTING: spatial data unavailable: {e}");
            return;
        }
    };
    let sky_path = config
        .asset_root
        .join("private/native-skies")
        .join(format!("{name}.json"));
    let sky: serde_json::Value = match std::fs::read(sky_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        Some(sky) => sky,
        None => {
            warn!("SKATE_CHARACTER_LIGHTING: missing authored light direction");
            return;
        }
    };
    let Some(sun) = sky["environment"]["sun_direction"].as_array() else {
        return;
    };
    if sun.len() != 3 {
        return;
    }
    let mut light = Vec4::new(0., 0., 0., 2.5); // Existing retail scene exposure.
    for i in 0..3 {
        let Some(v) = sun[i].as_f64() else {
            return;
        };
        light[i] = v as f32;
    }
    info!("SKATE_CHARACTER_LIGHTING: loaded authored {name} irradiance and character parameters");
    spawn_shadow_sources(&mut commands, light.truncate());
    commands.insert_resource(Lighting {
        data,
        probes,
        light,
        display_sh: None,
    });
}
/// `character-lighting.json`, with the native models' tables from `native-lighting.json` if any.
fn read_lighting_data(root: &std::path::Path) -> Result<LightingData, String> {
    let mut data = std::fs::read(root.join("private/character-lighting.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice::<LightingData>(&b).map_err(|e| e.to_string()))?;
    data.native = std::fs::read(crate::customiser_parts::asset_directory(root).join("native-lighting.json"))
        .ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    Ok(data)
}
fn spawn_shadow_sources(commands: &mut Commands, light: Vec3) {
    // Scene sun for authored PBR materials, plus all-caster shadow visibility
    // sampled by the native character shader.
    // `RenderOptions` replaces these cascades when SKATE_SUN_CASCADES or
    // SKATE_SUN_SHADOW_DISTANCE is set (render_options.rs).
    commands.spawn((
        ShadowSource,
        crate::render_options::SceneSun,
        Name::new("Scene sun and shadow visibility"),
        DirectionalLight {
            illuminance: 11_000.,
            shadows_enabled: true,
            affects_lightmapped_mesh_diffuse: false,
            ..default()
        },
        Transform::default().looking_to(-light, Vec3::Y),
        bevy::light::CascadeShadowConfigBuilder {
            maximum_distance: 100.,
            first_cascade_far_bound: 10.,
            ..default()
        }
        .build(),
    ));
    // Characters and mod graphics inhabit layer 28. The world receiver samples
    // this separate map so baked building/terrain shadows are not re-applied.
    commands.spawn((
        ShadowSource,
        BakedWorldShadows,
        Name::new("Dynamic object shadows onto baked world"),
        DirectionalLight {
            illuminance: 0.,
            shadows_enabled: true,
            affects_lightmapped_mesh_diffuse: true,
            // Receiver-only map: no self-shadow acne to hide with large bias.
            shadow_depth_bias: 0.002,
            shadow_normal_bias: 0.0,
            ..default()
        },
        RenderLayers::layer(28),
        Transform::default().looking_to(-light, Vec3::Y),
        bevy::light::CascadeShadowConfigBuilder {
            num_cascades: 1,
            maximum_distance: 24.,
            ..default()
        }
        .build(),
    ));
}

fn bind(
    mut commands: Commands,
    lighting: Option<Res<Lighting>>,
    source: Res<Assets<StandardMaterial>>,
    server: Res<AssetServer>,
    mut materials: ResMut<Assets<CharacterMaterial>>,
    entities: Query<(Entity, &GltfMaterialName, &MeshMaterial3d<StandardMaterial>, Option<&RenderLayers>)>,
    parents: Query<&ChildOf>,
    players: Query<(), Or<(With<crate::world::PlayerRoot>, With<crate::multiplayer::appearance::RemoteCharacter>)>>,
    mod_graphics: Query<(), With<ModGraphicsLit>>,
    parts: Query<(), With<crate::customiser_parts::PartRoot>>,
    native: Query<&crate::custom_models::NativeModelRoot>,
    imports: Query<(), With<crate::custom_models::CustomModelRoot>>,
) {
    let Some(lighting) = lighting else {
        return;
    };
    for (entity, name, handle, layers) in &entities {
        let under_player = parents.iter_ancestors(entity).any(|e| players.contains(e));
        let under_mod = parents.iter_ancestors(entity).any(|e| mod_graphics.contains(e));
        if under_mod || !under_player {
            continue;
        }
        // Modular CAC pieces own an extended material with tattoos/hair coverage.
        // Giving them a second material races publication and can render twice.
        if under_player && parents.iter_ancestors(entity).any(|e| parts.contains(e)) {
            continue;
        }
        let native_key = parents.iter_ancestors(entity).find_map(|e| native.get(e).ok().map(|n| &n.0));
        if under_player
            && native_key.is_none()
            && parents.iter_ancestors(entity).any(|e| imports.contains(e))
        {
            continue;
        }
        let table = if let Some(key) = native_key {
            lighting.data.native.get(key)
        } else {
            Some(&lighting.data.materials)
        };
        let data = table.and_then(|m| m.get(&name.0));
        let Some(data) = data else {
            continue;
        };
        if data.params.len() != 9 {
            continue;
        }
        let Some(m) = source.get(&handle.0) else {
            continue;
        };
        let alpha_cutoff = if let AlphaMode::Mask(cutoff) = m.alpha_mode {
            cutoff
        } else {
            -1.
        };
        let tint=Vec4::from_array(m.base_color.to_linear().to_f32_array());
        let specular = data.specular.as_ref();
        let material = materials.add(CharacterMaterial {
            params: CharacterParams {
                light: lighting.light,
                tint,
                options: Vec4::new(
                    f32::from(m.normal_map_texture.is_some()),
                    f32::from(specular.is_some()),
                    alpha_cutoff,
                    f32::from(data.is_hair()),
                ),
                rows: std::array::from_fn(|i| Vec4::from_array(data.params[i])),
                sh: lighting
                    .data
                    .default_sh
                    .map(|v| Vec3::from_array(v).extend(0.)),
            },
            diffuse: m.base_color_texture.clone(),
            normal: m.normal_map_texture.clone(),
            mask: specular.map(|path| {
                server.load_with_settings(
                    path.clone(),
                    |settings: &mut bevy::image::ImageLoaderSettings| {
                        settings.is_srgb = false;
                    },
                )
            }),
            alpha: m.alpha_mode,
        });
        // Skater meshes come and go with the outfit and the map, so binding one
        // that has since been despawned is routine rather than a fault.
        commands
            .entity(entity)
            .try_remove::<MeshMaterial3d<StandardMaterial>>()
            .try_insert((
                OriginalCharacterMaterial { material: handle.0.clone(), layers: layers.cloned() },
                MeshMaterial3d(material),
                RenderLayers::from_layers(&[0, 28]),
            ));
    }
}
fn shadow_views(
    mut commands: Commands,
    lighting: Option<Res<Lighting>>,
    cameras: Query<(Entity, Option<&RenderLayers>), With<crate::camera::GameplayCamera>>,
) {
    if lighting.is_none() {
        return;
    }
    for (entity, layers) in &cameras {
        let layers = layers.cloned().unwrap_or_default();
        if !layers.intersects(&RenderLayers::layer(28)) {
            commands.entity(entity).insert(layers.with(28));
        }
    }
}
fn update(
    lighting: Option<ResMut<Lighting>>,
    local_root: Query<(Entity, &Transform), With<crate::world::PlayerRoot>>,
    remote_roots: Query<(Entity, &Transform), With<crate::multiplayer::appearance::RemoteCharacter>>,
    character_meshes: Query<(Entity, &MeshMaterial3d<CharacterMaterial>)>,
    customiser_meshes: Query<(Entity, &MeshMaterial3d<crate::customiser_material::SkaterMaterial>)>,
    parents: Query<&ChildOf>,
    mut materials: ResMut<Assets<CharacterMaterial>>,
    mut customiser: ResMut<Assets<crate::customiser_material::SkaterMaterial>>,
    mut shadow: ResMut<crate::retail_render::FrameStateData>,
    time: Res<Time>,
) {
    let Some(mut lighting) = lighting else {
        return;
    };
    let fallback = lighting
        .data
        .default_sh
        .map(|v| Vec3::from_array(v).extend(0.));
    if let Ok((local, root)) = local_root.single() {
        let sh = lighting.probes.sample(root.translation, fallback);
        let displayed = lighting.display_sh.map_or(sh, |old| {
            let weight = 1. - (-time.delta_secs().clamp(0., 0.05) / 0.35).exp();
            std::array::from_fn(|i| old[i].lerp(sh[i], weight))
        });
        apply_sh_to_hierarchy(
            local,
            displayed,
            lighting.light,
            &character_meshes,
            &customiser_meshes,
            &parents,
            &mut materials,
            &mut customiser,
        );
        lighting.display_sh = Some(displayed);
        // Adapter floor: the local probe's direction-independent ambient term.
        shadow.approach(sh[0].truncate(), time.delta_secs());
    }
    for (remote, root) in &remote_roots {
        let sh = lighting.probes.sample(root.translation, fallback);
        apply_sh_to_hierarchy(
            remote,
            sh,
            lighting.light,
            &character_meshes,
            &customiser_meshes,
            &parents,
            &mut materials,
            &mut customiser,
        );
    }
}

fn customiser_retail_enabled(retail: &CharacterParams) -> bool {
    // Warm stamps options.z = -1 for authored CAC rows; tint.w is also forced on.
    retail.options.z < 0. || retail.tint.w > 0.
}

pub(crate) fn seed_customiser_retail(material: &mut crate::customiser_material::SkaterMaterial, light: Vec4, sh: [Vec4; 9]) {
    if !customiser_retail_enabled(&material.extension.retail) {
        return;
    }
    material.extension.retail.light = light;
    material.extension.retail.sh = sh;
}

fn apply_sh_to_hierarchy(
    root: Entity,
    sh: [Vec4; 9],
    light: Vec4,
    character_meshes: &Query<(Entity, &MeshMaterial3d<CharacterMaterial>)>,
    customiser_meshes: &Query<(Entity, &MeshMaterial3d<crate::customiser_material::SkaterMaterial>)>,
    parents: &Query<&ChildOf>,
    materials: &mut Assets<CharacterMaterial>,
    customiser: &mut Assets<crate::customiser_material::SkaterMaterial>,
) {
    for (entity, handle) in character_meshes {
        if !parents.iter_ancestors(entity).any(|ancestor| ancestor == root) {
            continue;
        }
        // Assets::get_mut queues Modified even when no field is written. Read
        // first so stationary objects do not rebuild their GPU materials.
        if materials.get(&handle.0).is_some_and(|material| lighting_changed(&material.params, &sh, light)) {
            let material = materials.get_mut(&handle.0).unwrap();
            material.params.sh = sh;
            material.params.light = light;
        }
    }
    for (entity, handle) in customiser_meshes {
        if !parents.iter_ancestors(entity).any(|ancestor| ancestor == root) {
            continue;
        }
        if customiser.get(&handle.0).is_some_and(|material| {
            customiser_retail_enabled(&material.extension.retail)
                && lighting_changed(&material.extension.retail, &sh, light)
        }) {
            let material = customiser.get_mut(&handle.0).unwrap();
            material.extension.retail.light = light;
            material.extension.retail.sh = sh;
        }
    }
}

fn lighting_changed(params: &CharacterParams, sh: &[Vec4; 9], light: Vec4) -> bool {
    params.light.to_array().map(f32::to_bits) != light.to_array().map(f32::to_bits)
        || params.sh.iter().zip(sh).any(|(a, b)| {
            a.to_array().map(f32::to_bits) != b.to_array().map(f32::to_bits)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mod_materials_keep_package_textures_even_with_stock_material_names() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .init_asset::<CharacterMaterial>();
        let world = app.world_mut();
        let diffuse = world.resource_mut::<Assets<Image>>().add(Image::default());
        let normal = world.resource_mut::<Assets<Image>>().add(Image::default());
        let source = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial {
            base_color_texture: Some(diffuse.clone()), normal_map_texture: Some(normal.clone()),
            base_color: Color::srgba(1., 0., 0., 0.5), alpha_mode: AlphaMode::Blend, ..default()
        });
        world.insert_resource(Lighting {
            data: LightingData {
                materials: HashMap::from([
                    ("Retail_Rostral".into(), MaterialData { shader: "character.face".into(), params: vec![[9.; 4]; 9], specular: Some("Rostral_specular.png".into()) }),
                    ("Retail_Organ".into(), MaterialData { shader: "character.default_cloth".into(), params: vec![[2.; 4]; 9], specular: Some("stock_cloth_specular.png".into()) }),
                ]), default_sh: [[0.; 3]; 9], native: HashMap::new(),
            }, probes: default(), light: Vec4::ONE, display_sh: None,
        });
        let root = world.spawn(ModGraphicsLit).id();
        let meshes: Vec<_> = ["CarPaint", "Retail_Rostral"].into_iter().map(|name| {
            world.spawn((ChildOf(root), GltfMaterialName(name.into()), MeshMaterial3d(source.clone()))).id()
        }).collect();
        world.run_system_once(bind).unwrap();
        for mesh in meshes {
            assert!(world.get::<MeshMaterial3d<CharacterMaterial>>(mesh).is_none());
            assert_eq!(world.get::<MeshMaterial3d<StandardMaterial>>(mesh).unwrap().0, source);
        }
    }

    #[test]
    fn unchanged_lighting_does_not_emit_material_modifications() {
        use bevy::asset::AssetEvent;
        use bevy::ecs::system::RunSystemOnce;
        use crate::customiser_material::SkaterMaterial;

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<CharacterMaterial>()
            .init_asset::<SkaterMaterial>();
        let character = app.world_mut().resource_mut::<Assets<CharacterMaterial>>().add(CharacterMaterial {
            params: CharacterParams::default(), diffuse: None, normal: None, mask: None,
            alpha: AlphaMode::Opaque,
        });
        let mut skater = SkaterMaterial::default();
        skater.extension.retail.options.z = -1.;
        let customiser = app.world_mut().resource_mut::<Assets<SkaterMaterial>>().add(skater);
        let root = app.world_mut().spawn_empty().id();
        app.world_mut().spawn((ChildOf(root), MeshMaterial3d(character.clone())));
        app.world_mut().spawn((ChildOf(root), MeshMaterial3d(customiser.clone())));
        // Flush Added events before measuring updates.
        app.update();
        app.world_mut().resource_mut::<Messages<AssetEvent<CharacterMaterial>>>().clear();
        app.world_mut().resource_mut::<Messages<AssetEvent<SkaterMaterial>>>().clear();

        for (sh, light, expected) in [
            ([Vec4::ZERO; 9], Vec4::ZERO, 0),
            ([Vec4::ONE; 9], Vec4::ONE, 1),
            ([Vec4::ONE; 9], Vec4::ONE, 0),
            ([Vec4::ONE; 9], Vec4::ZERO, 1),
        ] {
            app.world_mut().run_system_once(move |
                meshes: Query<(Entity, &MeshMaterial3d<CharacterMaterial>)>,
                cac: Query<(Entity, &MeshMaterial3d<SkaterMaterial>)>,
                parents: Query<&ChildOf>,
                mut materials: ResMut<Assets<CharacterMaterial>>,
                mut customiser: ResMut<Assets<SkaterMaterial>>,
            | apply_sh_to_hierarchy(root, sh, light, &meshes, &cac, &parents, &mut materials, &mut customiser)).unwrap();
            app.update();
            let character_events: Vec<_> = app.world_mut().resource_mut::<Messages<AssetEvent<CharacterMaterial>>>().drain().collect();
            let customiser_events: Vec<_> = app.world_mut().resource_mut::<Messages<AssetEvent<SkaterMaterial>>>().drain().collect();
            assert_eq!(character_events.iter().filter(|event| matches!(event, AssetEvent::Modified { .. })).count(), expected);
            assert_eq!(customiser_events.iter().filter(|event| matches!(event, AssetEvent::Modified { .. })).count(), expected);
            let params = &app.world().resource::<Assets<CharacterMaterial>>().get(&character).unwrap().params;
            assert_eq!(params.sh, sh);
            assert_eq!(params.light, light);
            let params = &app.world().resource::<Assets<SkaterMaterial>>().get(&customiser).unwrap().extension.retail;
            assert_eq!(params.sh, sh);
            assert_eq!(params.light, light);
        }
    }

    #[test]
    fn customiser_hair_families_include_pro_skater_defaults() {
        for shader in ["character.hair", "character.hair_ropa", "character.default_hair", "character.default_hair_ropa"] {
            assert!(MaterialData {shader:shader.into(),params:vec![],specular:None}.is_hair(), "{shader}");
        }
        for shader in ["character.default_skin", "character.default_cloth", "character.default_cloth_ropa"] {
            assert!(!MaterialData {shader:shader.into(),params:vec![],specular:None}.is_hair(), "{shader}");
        }
    }
    #[test]
    fn customiser_material_binding_uses_native_rows_and_leaves_cac_material_owned() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_resource::<Assets<StandardMaterial>>().init_resource::<Assets<CharacterMaterial>>();
        let world = app.world_mut();
        let material = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
        let authored = MaterialData { shader: "character.cloth".into(), params: vec![[2., 3., 4., 5.]; 9], specular: None };
        world.insert_resource(Lighting {
            data: LightingData { materials: HashMap::new(), default_sh: [[0.; 3]; 9],
                native: HashMap::from([("pro".into(), HashMap::from([("Retail_Torso".into(), authored)]))]) },
            probes: default(), light: Vec4::ONE, display_sh: None,
        });
        let player = world.spawn(crate::world::PlayerRoot).id();
        let pro = world.spawn((crate::custom_models::NativeModelRoot("pro".into()), ChildOf(player))).id();
        let torso = world.spawn((ChildOf(pro), GltfMaterialName("Retail_Torso".into()), MeshMaterial3d(material.clone()))).id();
        let part = world.spawn((crate::customiser_parts::PartRoot("shirt".into()), ChildOf(player))).id();
        let cloth = world.spawn((ChildOf(part), GltfMaterialName("Retail_Torso".into()), MeshMaterial3d(material))).id();
        let hair_data=MaterialData {shader:"character.default_hair".into(),params:vec![[1.;4];9],specular:None};
        world.resource_mut::<Lighting>().data.native.get_mut("pro").unwrap().insert("Retail_Hair".into(),hair_data);
        let hair_material=world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
        let hair=world.spawn((ChildOf(pro),GltfMaterialName("Retail_Hair".into()),MeshMaterial3d(hair_material))).id();
        world.run_system_once(bind).unwrap();
        let hair_handle=&world.get::<MeshMaterial3d<CharacterMaterial>>(hair).unwrap().0;
        assert_eq!(world.resource::<Assets<CharacterMaterial>>().get(hair_handle).unwrap().params.options.w,1.);

        let h = &world.get::<MeshMaterial3d<CharacterMaterial>>(torso).unwrap().0;
        assert_eq!(world.resource::<Assets<CharacterMaterial>>().get(h).unwrap().params.rows[0], Vec4::new(2., 3., 4., 5.));
        assert!(world.get::<MeshMaterial3d<StandardMaterial>>(torso).is_none());
        assert!(world.get::<MeshMaterial3d<CharacterMaterial>>(cloth).is_none());
        assert!(world.get::<MeshMaterial3d<StandardMaterial>>(cloth).is_some());
    }

    #[test]
    fn custom_scene_restores_character_and_removes_only_gameplay_shadow_layer() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(crate::config::Config {
            asset_root: "unused".into(), verification_capture: None,
            map: None, map_path: None, boot_map: None, difficulty: crate::difficulty::Difficulty::Hardcore,
            check_assets: false, validate_maps: false, start_paused: false, teleport: None, mute: false,
            multiplayer: Default::default(), map_fingerprint: 0,
        });
        world.insert_resource(crate::retail_render::RetailScene(false));
        world.init_resource::<Assets<crate::customiser_material::SkaterMaterial>>();
        world.insert_resource(crate::retail_render::FrameStateData {
            shadow: Vec4::ONE, clock: Vec4::ONE, pca: [Vec4::ONE; 7], casters: Vec4::ONE,
        });
        let material = Handle::<StandardMaterial>::default();
        let player = world.spawn((
            OriginalCharacterMaterial { material: material.clone(), layers: None },
            MeshMaterial3d(Handle::<CharacterMaterial>::default()),
            RenderLayers::from_layers(&[0, 28]),
        )).id();
        let light = world.spawn(ShadowSource).id();
        let camera = world.spawn((crate::camera::GameplayCamera, RenderLayers::from_layers(&[0, 28]))).id();
        let overlay = world.spawn(RenderLayers::layer(31)).id();
        world.run_system_once(load).unwrap();
        assert!(world.get_entity(light).is_err());
        assert_eq!(world.get::<MeshMaterial3d<StandardMaterial>>(player).unwrap().0, material);
        assert!(world.get::<MeshMaterial3d<CharacterMaterial>>(player).is_none());
        assert!(world.get::<RenderLayers>(player).is_none());
        assert_eq!(world.get::<RenderLayers>(camera).unwrap(), &RenderLayers::default());
        assert_eq!(world.get::<RenderLayers>(overlay).unwrap(), &RenderLayers::layer(31));
        assert_eq!(world.resource::<crate::retail_render::FrameStateData>().shadow, Vec4::ZERO);
    }

    #[test]
    fn world_receiver_source_excludes_world_casters_and_emits_no_light() {
        let mut world = World::new();
        let mut queue = bevy::ecs::world::CommandQueue::default();
        spawn_shadow_sources(&mut Commands::new(&mut queue, &world), Vec3::Y);
        queue.apply(&mut world);
        let player = RenderLayers::from_layers(&[0, 28]);
        let terrain = RenderLayers::default();
        let mut receivers = 0;
        let mut character_sources = 0;
        for (light, layers, cascades) in world
            .query::<(&DirectionalLight, Option<&RenderLayers>, &bevy::light::CascadeShadowConfig)>()
            .iter(&world)
        {
            let layers = layers.cloned().unwrap_or_default();
            assert!(light.shadows_enabled);
            assert!(layers.intersects(&player));
            if light.affects_lightmapped_mesh_diffuse {
                assert_eq!(light.illuminance, 0.);
                assert!(!layers.intersects(&terrain));
                assert_eq!(cascades.bounds, vec![24.]);
                assert_eq!(light.shadow_normal_bias, 0.);
                assert!(light.shadow_depth_bias < 0.005);
                receivers += 1;
            } else {
                assert_eq!(light.illuminance, 11_000.);
                assert!(layers.intersects(&terrain));
                character_sources += 1;
            }
        }
        assert_eq!((receivers, character_sources), (1, 1));
    }
}
