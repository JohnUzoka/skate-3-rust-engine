use crate::assets::AssetManifest;
use bevy::prelude::*;

/// World transform owner. Animation only writes descendant bone transforms.
#[derive(Component)]
pub(crate) struct PlayerRoot;

pub(crate) struct WorldPlugin;
impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(Color::srgb(0.065, 0.08, 0.10)))
            .insert_resource(GlobalAmbientLight {
                color: Color::WHITE,
                brightness: 350.,
                ..default()
            })
            .add_systems(Startup, spawn);
    }
}
/// The scene build (Startup). Audio `setup` runs before it, so the start-up banks load meanwhile.
///
/// With a `Config::boot_map` the scene starts empty and the map loader reads the map behind the
/// loading screen, as a map change does. Empty, not the scene for no map, which is the test world:
/// it would only be thrown away when the map commits.
pub(crate) fn spawn(world: &mut World) {
    let scene = {
        let server = world.resource::<AssetServer>();
        let manifest = world.resource::<AssetManifest>();
        server.load(GltfAssetLabel::Scene(0).from_asset(manifest.0.character_scene.clone()))
    };
    world.spawn((PlayerRoot, Transform::default(), Visibility::default()))
        .with_children(|parent| { parent.spawn(SceneRoot(scene)); });
    let mut prepared = crate::map_render::PreparedScene::new(world);
    let (map, root, boot) = {
        let mut config = world.resource_mut::<crate::config::Config>();
        (config.map.take(), config.asset_root.clone(), config.boot_map.clone())
    };
    if let Some(path) = boot {
        prepared.publish(world);
        let label = path.file_stem().map_or_else(String::new, |s| s.to_string_lossy().replace('_', " "));
        info!("[Skate] start map {} loads behind the loading screen", path.display());
        world.resource_mut::<crate::map_transition::MapTransition>()
            .request(crate::map_library::Entry { label, path: Some(path) });
        return;
    }
    crate::log_memory("scene preparation starting");
    prepared.prepare(map.as_ref(), &root);
    crate::log_memory("scene prepared");
    prepared.publish(world);
    drop(map);
    crate::log_memory("scene published, map data freed");
}

/// The ground the physics knows, drawn plainly, and a sun. Three switches, read as it spawns,
/// were for finding what hung the Switch's GPU here (findings 22 §2.3); it was the light loops of
/// Bevy's lit shader in a fragment shader's helper lanes, which bevy-horizon's Mesa patch 0032
/// fixed. `SKATE_TEST_WORLD_SHADOWS=0` turns the sun's shadows off, `SKATE_TEST_WORLD_INDEXED=1`
/// gives the ground indices, and `SKATE_TEST_WORLD_UNLIT=1` draws the ground unlit. The Switch
/// starts here only with `SKATE_TEST_WORLD=1` (map_library.rs).
pub(crate) fn spawn_test_world(
    commands: &mut crate::map_render::SceneCommands,
    meshes: &mut impl crate::map_render::AssetSink<Mesh>,
    materials: &mut impl crate::map_render::AssetSink<StandardMaterial>,
) {
    let set = |name: &str, value: &str| std::env::var(name).is_ok_and(|v| v.trim() == value);
    let shadows = !set("SKATE_TEST_WORLD_SHADOWS", "0");
    let indexed = set("SKATE_TEST_WORLD_INDEXED", "1");
    let unlit = set("SKATE_TEST_WORLD_UNLIT", "1");
    info!("[Skate] test world: shadows={shadows} indexed={indexed} unlit={unlit}");
    let colors = [
        Color::srgb(0.16, 0.19, 0.21),
        Color::srgb(0.48, 0.35, 0.22),
        Color::srgb(0.30, 0.43, 0.48),
        Color::srgb(0.24, 0.48, 0.31),
    ];
    for (quads, color) in crate::physics::ground::surfaces().into_iter().zip(colors) {
        let positions: Vec<[f32; 3]> = quads.into_iter().flat_map(|vertices| {
            [0, 2, 1, 0, 3, 2].map(|i| {
                let v = vertices[i];
                [v.x, v.y, v.z]
            })
        }).collect();
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        ).with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.compute_flat_normals();
        if indexed {
            let count = mesh.count_vertices() as u32;
            mesh.insert_indices(bevy::mesh::Indices::U32((0..count).collect()));
        }
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: color,
                perceptual_roughness: 0.9,
                double_sided: true,
                cull_mode: None,
                unlit,
                ..default()
            })),
            Transform::default(),
        ));
    }
    commands.spawn((
        DirectionalLight {
            illuminance: 11000.,
            shadows_enabled: shadows,
            ..default()
        },
        Transform::from_xyz(4., 7., 4.).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}
