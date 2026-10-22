use crate::app::FrameSet;
use bevy::{asset::LoadState, gltf::Gltf, prelude::*};
use skate_data::GameAssets;

#[derive(Resource)]
pub(crate) struct AssetManifest(pub GameAssets);
#[derive(Resource)]
pub(crate) struct CharacterAsset(pub Handle<Gltf>);
#[derive(Resource, Default, PartialEq, Eq)]
pub(crate) enum AssetStatus {
    #[default]
    Loading,
    Ready,
    Failed,
}

pub(crate) struct GameAssetsPlugin;
impl Plugin for GameAssetsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AssetStatus>()
            .add_systems(Startup, load)
            .add_systems(Update, check.in_set(FrameSet::Assets));
    }
}
fn load(mut commands: Commands, server: Res<AssetServer>, manifest: Res<AssetManifest>) {
    commands.insert_resource(CharacterAsset(
        server.load(manifest.0.character_scene.clone()),
    ));
}
fn check(
    server: Res<AssetServer>,
    character: Res<CharacterAsset>,
    mut status: ResMut<AssetStatus>,
    mut exit: MessageWriter<AppExit>,
    // Since when it has checked, and whether it has said what the asset still waits for.
    mut checking: Local<Option<(std::time::Instant, bool)>>,
) {
    if *status != AssetStatus::Loading {
        return;
    }
    let (since, said) = checking.get_or_insert_with(|| (std::time::Instant::now(), false));
    if let Some(LoadState::Failed(error)) = server.get_load_state(character.0.id()) {
        error!("Character asset failed to load: {error}");
        *status = AssetStatus::Failed;
        exit.write(AppExit::error());
    } else if server.is_loaded_with_dependencies(character.0.id()) {
        *status = AssetStatus::Ready;
        if *said {
            info!("the character's asset loaded with its dependencies after {:.1} s", since.elapsed().as_secs_f32());
        }
    } else if !*said && since.elapsed() >= std::time::Duration::from_secs(20) {
        warn!(
            "the character's asset hasn't loaded with its dependencies after 20 s: {:?}",
            server.get_load_states(character.0.id()),
        );
        *said = true;
    }
}
