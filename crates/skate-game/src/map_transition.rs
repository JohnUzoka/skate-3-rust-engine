//! Transactional world replacement: prepare without mutating the live session,
//! then commit at a schedule boundary with gameplay suspended.
use bevy::prelude::*;
use std::{path::PathBuf, sync::{Arc, atomic::{AtomicU8, Ordering}}, thread::JoinHandle, time::Instant};
use crate::{config::Config, map_library::Entry, map_render::PreparedScene,
    physics::{GamePhysics, PlayerControls, SkaterRuntime}};

/// Persistent character customisation can reapply after this set, while the
/// transition still holds the loading overlay and gameplay remains suspended.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MapTransitionSet;
#[derive(Message)]
pub(crate) struct WorldChanged;


#[cfg(test)]
#[path = "tests/map_transition.rs"]
mod tests;

#[derive(Resource, Debug)]
pub(crate) struct CurrentMap {
    pub path: Option<PathBuf>,
    pub name: String,
    pub spawn: [f32; 3],
    pub heading: f32,
    pub generation: u64,
    /// The map's own audio definition: its `AUDO` extension (schema 1, UTF-8 JSON;
    /// `game_audio::map_audio`), if it has one.
    pub audio_tag: Option<Arc<[u8]>>,
}
impl CurrentMap {
    fn from_package(path: Option<PathBuf>, map: Option<&skate_data::skate_map::SkateMap>) -> Self {
        Self { path, name: map.map_or_else(|| "Test world".into(), |m| m.name.clone()),
            spawn: map.map_or([0., crate::physics::ground::HEIGHT, 0.], |m| m.spawn),
            heading: map.map_or(0., |m| m.heading), generation: 0,
            audio_tag: map.and_then(|m| m.extensions.iter().find(|e| e.tag == *b"AUDO" && e.schema == 1)).map(|e| Arc::from(&e.payload[..])) }
    }
}

struct PreparedWorld {
    map_fingerprint: u64,
    scene: PreparedScene,
    physics: GamePhysics,
    skater: SkaterRuntime,
    controls: PlayerControls,
    camera: crate::camera::CameraRuntime,
    metadata: CurrentMap,
    retail: bool,
    triggers: crate::trigger_volumes::TriggerVolumes,
    difficulty: crate::difficulty::Difficulty,
}

enum Phase {
    Idle,
    Requested(Entry),
    Loading { entry: Entry, progress: Arc<AtomicU8>, job: JoinHandle<Result<PreparedWorld, String>> },
    // Let extraction see the committed scene before releasing the pause menu.
    // `resume` is false after a failed change reloaded the map that was live.
    Publishing { frames: u8, notice: String, resume: bool },
}
#[derive(Resource)]
pub(crate) struct MapTransition {
    phase: Phase,
    /// While a failed map change reloads the map that was live: why it failed.
    restoring: Option<String>,
}
impl Default for MapTransition {
    fn default() -> Self { Self { phase: Phase::Idle, restoring: None } }
}
impl MapTransition {
    pub fn busy(&self) -> bool { !matches!(self.phase, Phase::Idle) }
    /// Whether the loader thread is (about to be) reading the map, before anything is published.
    #[cfg(target_os = "horizon")]
    pub fn loading(&self) -> bool { matches!(self.phase, Phase::Requested(_) | Phase::Loading { .. }) }
    pub fn request(&mut self, entry: Entry) {
        if !self.busy() { self.phase = Phase::Requested(entry); }
    }
    pub fn label(&self) -> String {
        match &self.phase {
            Phase::Idle => String::new(),
            Phase::Requested(entry) => format!("Loading {} — preparing…", entry.label),
            Phase::Loading { entry, progress, .. } => format!("Loading {} — {}…", entry.label,
                match progress.load(Ordering::Relaxed) {
                    0 => "reading and validating map", 1 => "building collision and rendering",
                    2 => "initializing skater, camera and rendering", _ => "finishing meshes, textures and sky",
                }),
            Phase::Publishing { .. } => "Loading — publishing the new world…".into(),
        }
    }
    /// The load's step for the loading screen: ASCII, since its font is Bevy's subset of Fira
    /// Mono, and short, since its status line holds 55 characters.
    pub(crate) fn step(&self) -> Option<String> {
        match &self.phase {
            Phase::Idle => None,
            Phase::Requested(entry) => Some(format!("Loading {}", entry.label)),
            Phase::Loading { entry, progress, .. } => Some(format!("Loading {}: {}", entry.label,
                match progress.load(Ordering::Relaxed) {
                    0 => "reading the map", 1 => "building collision",
                    2 => "setting up the skater", _ => "finishing the scene",
                })),
            Phase::Publishing { .. } => Some("Publishing the new world".into()),
        }
    }
}

pub(crate) struct MapTransitionPlugin;
impl Plugin for MapTransitionPlugin {
    fn build(&self, app: &mut App) {
        let config = app.world().resource::<Config>();
        let current = CurrentMap::from_package(config.map_path.clone(), config.map.as_ref());
        app.insert_resource(current)
            .init_resource::<MapTransition>()
            .add_message::<WorldChanged>()
            .add_systems(PreUpdate, poll.in_set(MapTransitionSet).after(crate::graphics_menu::MenuInput)
                .before(crate::input::poll_controllers))
            .add_systems(Update, scheduled_map_change)
            .configure_sets(FixedUpdate, (
                crate::app::SimulationSet::Input, crate::app::SimulationSet::Controls,
                crate::app::SimulationSet::Physics,
            ).run_if(crate::graphics_menu::gameplay_active));
    }
}

/// `env SKATE_MAP_CHANGE=<map name>@<seconds>` (options.txt `env` line): requests a map change
/// that many seconds after the first world is on screen (the loading screen first gone), once.
/// The emulator harness can't reliably drive the menu with timed key presses, and a map change is
/// the case the memory log has to catch (findings 57 §4.5). `<map name>` is a stem under the
/// installation's `maps/`, case-insensitive.
fn scheduled_map_change(
    config: Res<Config>,
    mut transition: ResMut<MapTransition>,
    loading: Option<Res<crate::loading_screen::LoadingScreen>>,
    mut fired: Local<Option<(String, f32, std::time::Instant, bool)>>,
) {
    // A request while a load runs would be dropped, and the hook fires once.
    if transition.busy() || loading.is_some_and(|l| l.shown()) {
        return;
    }
    if fired.is_none() {
        let spec = std::env::var("SKATE_MAP_CHANGE").ok().filter(|s| !s.is_empty());
        *fired = spec.and_then(|spec| {
            let (name, secs) = spec.rsplit_once('@')?;
            let secs = secs.trim().parse::<f32>().ok()?;
            Some((name.trim().to_string(), secs, std::time::Instant::now(), false))
        });
        if let Some((name, secs, _, _)) = fired.as_ref() {
            eprintln!("[Skate] SKATE_MAP_CHANGE={name}@{secs}s armed");
        }
    }
    // Wall clock, not Time<Virtual>: the emulator's start-up menu pauses virtual time, and
    // the point is to fire a fixed real time after the world first shows.
    let Some(state) = fired.as_mut() else { return };
    if state.3 || state.2.elapsed().as_secs_f32() < state.1 {
        return;
    }
    state.3 = true;
    let (name, fired_at) = (state.0.clone(), state.2.elapsed().as_secs_f32());
    let root = config.asset_root.clone();
    let entry = crate::map_library::discover(&root)
        .into_iter()
        .find(|e| e.path.as_ref().is_some_and(|p| {
            p.file_stem().is_some_and(|s| s.eq_ignore_ascii_case(&name))
        }));
    match entry {
        Some(entry) => {
            eprintln!("[Skate] SKATE_MAP_CHANGE firing {fired_at:.1}s after arming: {:?}", entry.path);
            transition.request(entry);
        }
        None => eprintln!("[Skate] SKATE_MAP_CHANGE: no map named {name} under {}", root.parent().unwrap_or(&root).join("maps").display()),
    }
}

fn start(world: &mut World, entry: Entry) -> Result<Phase, String> {
    // Release the live world's visuals and collision before the new map is read
    // and decoded. A map change used to build the new map on top of the old
    // world, which ran the Switch heap out (findings 19 §6.3, doc 58 §3.3); the
    // collision alone is 243 MiB for Downtown, and a running mod's rapier copy
    // of it 220 MiB more. If the new map then fails to load, `poll` reloads the
    // map that was live.
    crate::map_render::MapAssets::retire(world);
    world.resource_mut::<GamePhysics>().release_map_geometry()?;
    crate::modding::release_ground(world);
    let config = world.resource::<Config>();
    let root = config.asset_root.clone();
    let difficulty = config.difficulty;
    let graphs = world.resource::<crate::graph_runtime::StockGraphs>().clone();
    let source = world.resource::<SkaterRuntime>().animation.source.clone();
    let preferences = world.resource::<PlayerControls>().preferences;
    let mut scene = PreparedScene::new(world);
    let selected = entry.clone();
    let progress = Arc::new(AtomicU8::new(0));
    let stage = progress.clone();
    let job = std::thread::Builder::new().name("map-loader".into()).spawn(move || {
        let _span = info_span!("load_map_transition").entered();
        let started = Instant::now();
        let (map, map_fingerprint) = match selected.path.as_deref() {
            Some(path) => {
                let (map, fingerprint) = info_span!("read_map").in_scope(|| skate_data::skate_map::SkateMap::load_with_fingerprint(path))?;
                (Some(map), fingerprint)
            }
            None => (None, crate::config::map_fingerprint(None)?),
        };
        let read_time = started.elapsed();
        let validation_started = Instant::now();
        if let Some(map) = &map { crate::skate_world::validate_runtime(map)?; }
        let validation_time = validation_started.elapsed();
        let triggers = crate::trigger_volumes::TriggerVolumes::load_or_warn(selected.path.as_deref(), map.as_ref());
        let metadata = CurrentMap::from_package(selected.path, map.as_ref());
        let retail = map.as_ref().is_some_and(|m| crate::retail_render::RetailScene::for_map(m));
        // Both builders only read the decoded package. Reserve render handles
        // on a second worker while the first constructs fresh simulation state;
        // neither publishes to the live world until both have succeeded.
        let (physics, skater, controls, camera, simulation_time, render_time) =
            std::thread::scope(|scope| -> Result<_, String> {
                let rendering = std::thread::Builder::new().name("map-render-loader".into())
                    .spawn_scoped(scope, || {
                        let render_started = Instant::now();
                        scene.prepare(map.as_ref(), &root);
                        render_started.elapsed()
                    }).map_err(|e| format!("Could not start render loader: {e}"))?;
                let simulation_started = Instant::now();
                let simulation = (|| -> Result<_, String> {
                    stage.store(1, Ordering::Relaxed);
                    let physics = info_span!("load_physics").in_scope(|| GamePhysics::load_with_difficulty(&root, map.as_ref(), difficulty))?;
                    stage.store(2, Ordering::Relaxed);
                    let skater = SkaterRuntime::load_for_world(&root, &graphs, &physics, difficulty.profile_key(), Some(source))?;
                    let mut controls = PlayerControls::load(&root)?;
                    controls.preferences = preferences;
                    let camera = crate::camera::CameraRuntime::load(&root)?;
                    Ok((physics, skater, controls, camera))
                })();
                let simulation_time = simulation_started.elapsed();
                stage.store(3, Ordering::Relaxed);
                // Explicitly join even on a simulation error: no background
                // render work or reserved scene can outlive a failed request.
                let render_time = rendering.join()
                    .map_err(|_| "Map render preparation failed unexpectedly".to_string())?;
                let (physics, skater, controls, camera) = simulation?;
                Ok((physics, skater, controls, camera, simulation_time, render_time))
            })?;
        eprintln!("MAP_LOAD_TIMING name={:?} read_ms={} validation_ms={} simulation_ms={} render_ms={} prepare_ms={} parallel=true",
            metadata.name, read_time.as_millis(), validation_time.as_millis(),
            simulation_time.as_millis(), render_time.as_millis(), started.elapsed().as_millis());
        // Drop the decoded package on this worker. Physics and rendering now
        // own their data; retaining it would double large-city CPU memory.
        Ok(PreparedWorld { map_fingerprint, scene, physics, skater, controls, camera, metadata, retail, triggers, difficulty })
    }).map_err(|e| format!("Could not start map loader: {e}"))?;
    Ok(Phase::Loading { entry, progress, job })
}

fn poll(world: &mut World) {
    let ready = match &world.resource::<MapTransition>().phase {
        Phase::Idle => false,
        Phase::Loading { job, .. } => job.is_finished(),
        _ => true,
    };
    if !ready { return; }
    let phase = std::mem::replace(&mut world.resource_mut::<MapTransition>().phase, Phase::Idle);
    let next = match phase {
        Phase::Requested(entry) => {
            let label = entry.label.clone();
            start(world, entry).unwrap_or_else(|error| failed(world, &label, error))
        }
        Phase::Loading { job, entry, .. } => match job.join().unwrap_or_else(|_| Err("Map loader failed unexpectedly".into())) {
            Ok(prepared) => {
                let mut notice = commit(world, prepared);
                let config = world.resource::<Config>();
                if config.map_path.is_none() && !crate::map_library::SAVE_TEST_WORLD {
                    notice.push_str(" Not saved as the default map.");
                } else {
                    match crate::map_library::save_default(&config.asset_root, config.map_path.as_deref()) {
                        Ok(()) => notice.push_str(" Default map saved."),
                        Err(error) => notice.push_str(&format!(" Could not save default: {error}")),
                    }
                }
                match world.resource_mut::<MapTransition>().restoring.take() {
                    // Back on the map that was live; the menu stays open on why.
                    Some(failure) => Phase::Publishing { frames: 3, notice: format!("{failure}\n{notice}"), resume: false },
                    None => Phase::Publishing { frames: 3, notice, resume: true },
                }
            }
            Err(error) => failed(world, &entry.label, error),
        },
        Phase::Publishing { frames, notice, resume } if frames > 0 => Phase::Publishing { frames: frames - 1, notice, resume },
        Phase::Publishing { notice, resume, .. } => {
            world.resource_mut::<crate::graphics_menu::Menu>().transition_finished(notice, resume);
            if resume {
                world.resource_mut::<Time<Virtual>>().unpause();
            }
            Phase::Idle
        }
        Phase::Idle => Phase::Idle,
    };
    world.resource_mut::<MapTransition>().phase = next;
}

/// Loading `label` failed. `start` released the live map's visuals and collision
/// first, so the change goes back to that map; if reloading it is what failed,
/// the menu stays open on both errors.
fn failed(world: &mut World, label: &str, error: String) -> Phase {
    let live = Entry { label: world.resource::<CurrentMap>().name.clone(), path: world.resource::<Config>().map_path.clone() };
    // With no map loaded yet (the Switch's empty start scene) the live "map" is the test world,
    // which that build doesn't have.
    let restorable = live.path.is_some() || crate::map_library::TEST_WORLD_AVAILABLE;
    let outcome = after_failure(&mut world.resource_mut::<MapTransition>().restoring, restorable.then_some(live), label, error);
    match outcome {
        Ok(live) => {
            warn!("MAP_TRANSITION_FAILED {}; reloading {}",
                world.resource::<MapTransition>().restoring.as_deref().unwrap_or_default(), live.label);
            Phase::Requested(live)
        }
        Err(message) => {
            warn!("MAP_TRANSITION_FAILED {message}");
            world.resource_mut::<crate::graphics_menu::Menu>()
                .transition_finished(format!("{message}\nChoose another map."), false);
            Phase::Idle
        }
    }
}

/// The map to reload after loading `label` failed (`live`, once), or, when that
/// reload is what failed or there is no live map to reload, the message to leave
/// the menu on.
fn after_failure(restoring: &mut Option<String>, live: Option<Entry>, label: &str, error: String) -> Result<Entry, String> {
    match (restoring.take(), live) {
        (None, Some(live)) => {
            *restoring = Some(format!("Could not load {label}: {error}"));
            Ok(live)
        }
        (None, None) => Err(format!("Could not load {label}: {error}")),
        (Some(failure), _) => Err(format!("{failure}\nReloading {label} failed too: {error}")),
    }
}

fn commit(world: &mut World, mut prepared: PreparedWorld) -> String {
    let publication_started = Instant::now();
    prepared.metadata.generation = world.resource::<CurrentMap>().generation + 1;
    // The live world's visuals were already released when the transition
    // started, to make room for the new map's decode; this is the idempotent
    // safety net for a world that was never retired. All fallible
    // decoding/validation/construction finished before this point, so no
    // simulation system can observe a mixture of the two worlds.
    crate::map_render::MapAssets::retire(world);
    // The old world's small blocks now sit in bevy-horizon's cache, where NVK, falling back to
    // malloc for a GPU block of the new world's, can't take them back.
    #[cfg(target_os = "horizon")]
    if bevy_horizon::release_cached_memory() {
        crate::log_memory("map change, the small-block cache emptied");
    }
    prepared.scene.publish(world);
    crate::camera::set_world_environment(world, prepared.retail);
    world.insert_resource(crate::retail_render::RetailScene(prepared.retail));
    crate::retail_render::set_retail_map(prepared.retail);
    info!("SKATE_TRIGGERS map={:?} volumes={} source={}", prepared.metadata.name, prepared.triggers.map.len(), prepared.triggers.origin);
    // Mod-added volumes and switches are world-scoped, like sdk.volumes.
    world.insert_resource(prepared.triggers);
    world.insert_resource(crate::grind_world::GrindGeometry::for_world(prepared.metadata.path.is_none()));
    world.insert_resource(Time::<Fixed>::from_duration(prepared.physics.period()));
    let root_transform = Transform::from_matrix(crate::animation::native_matrix(
        prepared.skater.animated_skeleton.roots.animation_to_world));
    for mut transform in world.query_filtered::<&mut Transform, With<crate::world::PlayerRoot>>().iter_mut(world) {
        *transform = root_transform;
    }
    world.insert_resource(prepared.physics);
    world.insert_resource(prepared.skater);
    world.insert_resource(prepared.controls);
    world.insert_resource(prepared.camera);
    world.insert_resource(crate::presentation::Presentation::default());
    world.insert_resource(crate::replay::Replay::default());
    world.insert_resource(crate::input::ControllerInput::default());
    world.insert_resource(crate::input::PublishedTickInput::default());
    world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
    world.resource_mut::<ButtonInput<MouseButton>>().reset_all();
    let mut config = world.resource_mut::<Config>();
    config.map_path = prepared.metadata.path.clone();
    config.map_fingerprint = prepared.map_fingerprint;
    config.difficulty = prepared.difficulty;
    let notice = format!("Loaded {}.", prepared.metadata.name);
    info!("MAP_TRANSITION_COMMITTED generation={} name={:?} spawn={:?} heading={} pid={}",
        prepared.metadata.generation, prepared.metadata.name, prepared.metadata.spawn,
        prepared.metadata.heading, std::process::id());
    if let Some(mut messages) = world.get_resource_mut::<Messages<WorldChanged>>() {
        messages.write(WorldChanged);
    }
    world.insert_resource(prepared.metadata);
    eprintln!("MAP_PUBLISH_TIMING cpu_ms={}", publication_started.elapsed().as_millis());
    notice
}
