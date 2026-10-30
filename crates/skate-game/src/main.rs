mod frame_timing;
mod animation;
mod crash_report;
mod crash_context;
mod multiplayer;
mod apt_vm;
mod apt_display;
mod apt_movie;
mod apt_text;
mod apt_scene;
mod apt_imports;
mod hud_runtime;
mod scoring_runtime;
mod scoring_hud;
mod animation_pose;
mod app;
mod assets;
mod camera;
mod config;
mod setup;
mod updater;
mod map_library;
mod map_render;
mod map_transition;
mod difficulty;
mod custom_difficulty;
mod graph_host;
mod graph_runtime;
mod input;
mod session_marker;
mod physics;
mod skater_animation;
mod verification;
mod performance;
mod gpu_frame_time;
mod profiling;
mod graphics_menu;
mod retail_menus;
mod retail_menu_movies;
mod modding;
mod customiser;
mod customiser_parts;
mod customiser_material;
mod custom_models;
mod teleport_menu;
mod render_capacity;
mod page_cache;
mod render_options;
mod retail_render;
mod retail_character;
mod retail_shadow_bounds;
mod retail_exposure;
mod retail_irradiance;
mod retail_sky;
mod presentation;
mod debug_cam;
mod replay;
mod world;
mod world_diag;
mod prepass_order;
mod grind_world;
mod skate_world;
mod map_validation;
mod loading_screen;
mod water_splash;
mod game_audio;
pub(crate) mod world_audio;
pub(crate) mod ui_audio;
mod trigger_volumes;
#[cfg(target_os = "horizon")]
mod horizon;

fn main() -> bevy::app::AppExit {
    LAUNCH.get_or_init(std::time::Instant::now);
    #[cfg(target_os = "horizon")]
    horizon::init();
    // After `init`, which reads options.txt's `env` lines (SKATE_EARLY_SPLASH).
    #[cfg(target_os = "horizon")]
    loading_screen::show_early_splash();
    // The Switch has no setup tools and runs one process, so no crash-reporting supervisor; its
    // updater tidies up after an interrupted update in `horizon::init`.
    #[cfg(not(target_os = "horizon"))]
    {
        match updater::recover() {
            Ok(true) => return bevy::app::AppExit::Success,
            Err(error) => { eprintln!("{error}"); return bevy::app::AppExit::Success; },
            Ok(false) => {}
        }
        // Setup tool mode: `--extract-ocean-pca <default.xex> <ocean-pca.json>`.
        // Before the crash supervisor, whose report window would block setup on
        // a failed (non-zero) extraction.
        if let Some(code) = extract_ocean_pca() { std::process::exit(code); }
        // Setup tool mode: `--extract-menu-tables <default.xex> <menu-tables.json>`.
        if let Some(code) = extract_menu_tables() { std::process::exit(code); }
        if let Some(code) = crash_report::entry() { std::process::exit(code); }
    }
    let _trace = match profiling::init() {
        Ok(guard) => guard,
        Err(error) => { eprintln!("{error}"); return bevy::app::AppExit::error(); }
    };
    let _startup = bevy::log::info_span!("startup").entered();
    report_stage("configuration_and_installation");
    let config = match bevy::log::info_span!("load_configuration_and_map").in_scope(config::Config::from_env) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    profiling::map_metadata(&config);
    log_memory("configuration and map loaded");
    eprintln!("REPORT_META startup=map_fingerprint:{:016x} difficulty:{} multiplayer_requested:{} custom_appearance:{} renderer:Vulkan", config.map_fingerprint, config.difficulty.key(), config.multiplayer.host.is_some() || config.multiplayer.direct.is_some(), config.multiplayer.appearance.is_some());
    report_stage("gameplay_configuration");
    if let Err(error) = skate_data::input_config::StockGameplayConfig::load(&config.asset_root) {
        eprintln!("{error}");
        return bevy::app::AppExit::error();
    }
    report_stage("asset_manifest");
    let manifest = match bevy::log::info_span!("load_manifest").in_scope(|| skate_data::GameAssets::load(&config.asset_root)) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    report_stage("stock_graphs");
    let graphs = match bevy::log::info_span!("load_graphs").in_scope(|| graph_runtime::StockGraphs::load(&config.asset_root, &manifest)) {
        Ok(graphs) => graphs,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    if config.validate_maps {
        return match map_validation::run(&config, &graphs) {
            Ok(()) => bevy::app::AppExit::Success,
            Err(error) => {
                eprintln!("{error}");
                bevy::app::AppExit::error()
            }
        };
    }
    report_stage("map_validation");
    if let Some(map) = &config.map {
        if let Err(error) = skate_world::validate_runtime(map) {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    }
    eprintln!("SKATE_DIFFICULTY mode={} native_index={}", config.difficulty.key(), config.difficulty as u32);
    report_stage("physics_initialization");
    let mut physics = match bevy::log::info_span!("load_physics").in_scope(|| physics::GamePhysics::load_with_difficulty(&config.asset_root, config.map.as_ref(), config.difficulty)) {
        Ok(physics) => physics,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    log_memory("physics loaded");
    if config.multiplayer.spawn_offset != 0.0 {
        let mut spawn = physics.board.part_transforms()[skate_core::physics::board::BodyId::Deck.index()];
        spawn.translation.x += config.multiplayer.spawn_offset;
        physics.board.set_transform(spawn);
    }
    report_stage("skater_initialization");
    let skater = match bevy::log::info_span!("load_skater").in_scope(|| physics::SkaterRuntime::load(&config.asset_root, &graphs, &physics, config.difficulty.profile_key())) {
        Ok(skater) => skater,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    log_memory("skater loaded");
    report_stage("controls_initialization");
    let controls = match physics::PlayerControls::load(&config.asset_root) {
        Ok(controls) => controls,
        Err(error) => { eprintln!("{error}"); return bevy::app::AppExit::error(); }
    };
    if config.check_assets {
        if let Err(error) = camera::CameraRuntime::load(&config.asset_root) {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
        match trigger_volumes::check(config.map_path.as_deref(), config.map.as_ref()) {
            Ok(count) => eprintln!("SKATE_TRIGGERS_READY volumes={count}"),
            Err(error) => {
                eprintln!("{error}");
                return bevy::app::AppExit::error();
            }
        }
        eprintln!("SKATE_ASSETS_READY");
        return bevy::app::AppExit::Success;
    }
    report_stage("renderer_and_app_initialization");
    let mut app = app::build(config, manifest, graphs, physics, skater);
    app.insert_resource(controls);
    log_memory("app built");
    report_stage("app_run");
    // The same moment with a tracing timestamp, which every later log line has.
    bevy::log::info!("BOOT_TIMING stage=app_run since_launch_ms={}", since_launch_ms());
    drop(_startup);
    app.run()
}

/// When `main` started. The homebrew loader's load of the NRO comes before it and isn't counted.
static LAUNCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub(crate) fn since_launch_ms() -> u128 {
    LAUNCH.get().map_or(0, |launch| launch.elapsed().as_millis())
}

/// `REPORT_META stage=<stage>` (the crash report keeps the last), then `BOOT_TIMING`: the boot
/// before the renderer has no tracing timestamps.
fn report_stage(stage: &str) {
    eprintln!("REPORT_META stage={stage}");
    eprintln!("BOOT_TIMING stage={stage} since_launch_ms={}", since_launch_ms());
    loading_screen::boot_stage(stage);
}

/// On the Switch, logs `[Memory] <stage>: ` and bevy_horizon's `memory_report` to log.txt: the
/// one heap there holds the game's allocations and all GPU memory. Does nothing elsewhere.
pub(crate) fn log_memory(stage: &str) {
    #[cfg(target_os = "horizon")]
    eprintln!("[Memory] {stage}: {}", bevy_horizon::memory_report());
    #[cfg(not(target_os = "horizon"))]
    let _ = stage;
}

#[cfg(test)]
#[path = "tests/action_host.rs"]
mod action_host_tests;

/// Writes the retail water/ocean animation table for setup (see
/// skate_data::ocean_pca). Runs before any game initialization.
#[cfg(not(target_os = "horizon"))]
fn extract_ocean_pca() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != "--extract-ocean-pca" {
        return None;
    }
    let (Some(xex), Some(out), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("Usage: skate3rust --extract-ocean-pca <default.xex> <ocean-pca.json>");
        return Some(2);
    };
    let result = std::fs::read(&xex)
        .map_err(|e| format!("{}: {e}", std::path::Path::new(&xex).display()))
        .and_then(|bytes| skate_data::ocean_pca::json_from_xex(&bytes))
        .and_then(|json| std::fs::write(&out, json).map_err(|e| format!("{}: {e}", std::path::Path::new(&out).display())));
    match result {
        Ok(()) => {
            println!("OCEAN_PCA_READY");
            Some(0)
        }
        Err(error) => {
            eprintln!("Ocean PCA extraction failed: {error}");
            Some(1)
        }
    }
}

/// Writes the retail front-end menu tables for setup (see
/// skate_data::menu_tables). Runs before any game initialization.
#[cfg(not(target_os = "horizon"))]
fn extract_menu_tables() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != "--extract-menu-tables" {
        return None;
    }
    let (Some(xex), Some(out), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("Usage: skate3rust --extract-menu-tables <default.xex> <menu-tables.json>");
        return Some(2);
    };
    let result = std::fs::read(&xex)
        .map_err(|e| format!("{}: {e}", std::path::Path::new(&xex).display()))
        .and_then(|bytes| skate_data::menu_tables::json_from_xex(&bytes))
        .and_then(|json| std::fs::write(&out, json).map_err(|e| format!("{}: {e}", std::path::Path::new(&out).display())));
    match result {
        Ok(()) => {
            println!("MENU_TABLES_READY");
            Some(0)
        }
        Err(error) => {
            eprintln!("Menu table extraction failed: {error}");
            Some(1)
        }
    }
}
