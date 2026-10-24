//! The Nintendo Switch build, through bevy-horizon (../../../bevy-horizon, see its
//! docs/porting-a-bevy-game.md). Everything the game does differently on Horizon starts here.
//!
//! The SD card layout, beside the NRO:
//! - `sdmc:/switch/skate3rust/skate3rust.nro`
//! - `sdmc:/switch/skate3rust/assets/`: what `SKATE3_ASSETS` names on a PC
//! - `sdmc:/switch/skate3rust/mods/`: `SKATE3_MODS`
//! - `sdmc:/switch/skate3rust/log.txt`: stdout, stderr and the tracing log, rewritten each launch
//! - `sdmc:/switch/skate3rust/cache/`: each map's world texture pages as built at its first
//!   launch, read at the next instead of built again (page_cache.rs); safe to delete
//! - `sdmc:/switch/skate3rust/options.txt`: optional, one option per line:
//!   - `log=<filter>`: the tracing filter (`RUST_LOG` on a PC)
//!   - `no_pipelined_rendering`: render on the main thread instead of Bevy's render thread
//!   - `multi_threaded`: run the main world's systems on Bevy's task pool. Without it they run
//!     one at a time on the main thread, the default since 0.1.3-rc2 (findings 63 §6.2)
//!   - `rgba_pages`: keep the world texture pages RGBA8, four times the memory of the BC3 they
//!     are otherwise stored as (retail_render.rs `PageFormat`)
//!   - `no_upload_throttle`: hand Bevy's mesh allocator every new mesh at once and never wait for
//!     the GPU to free upload copies, as Bevy does without bevy_horizon's `render_0_18` (for
//!     comparing; `install` below)
//!   - `shadow_map=<size>`: the directional shadow maps' width and height, a power of two; 1024
//!     unless set (`install` below)
//!   - `memory_log`: log `[Memory]` lines once the game runs: the memory log, a line every 2 s
//!     for the first 3 minutes (`start_memory_log` below), and bevy_horizon's heartbeat line once
//!     a minute. Without it, neither runs (`init` sets `BEVY_HORIZON_MEMORY_EVERY=0` unless an
//!     `env` line sets it). Each line's `mallinfo` walk stalls every thread that allocates for
//!     tens of milliseconds; in play, the minute's line made a frame of 65-72 ms (findings 33),
//!     and perf27's 2 s lines made frames of 62-70 ms (findings 47). The lines while the map
//!     loads still print
//!   - `no_memory_log`: the default; it wins over `memory_log`
//!   - `env NAME=value`: sets the environment variable `NAME` before the game starts, for what a
//!     PC reads from its environment: `SKATE_PERF_REPORT` and the rest of performance.rs (not
//!     `SKATE_TEST_WORLD=1`: the Switch build has no test world, map_library.rs), the rendering
//!     trade-offs in render_options.rs
//!     (`SKATE_DEPTH_PREPASS`, `SKATE_SUN_CASCADES`, `SKATE_SUN_SHADOW_DISTANCE`,
//!     `SKATE_SHADOW_FILTER`, `SKATE_DRAW_DISTANCE`, `SKATE_WORLD_SHADOWS`, and
//!     `SKATE_UPSCALE=easu|bilinear`, how a render scale under 100 % is stretched to the
//!     screen), skate_world.rs's
//!     `SKATE_VERTEX_PACKING=0` (the world's standard vertex attributes) and
//!     `SKATE_WORLD_ORDER=centroid` (the world's old triangle order), both for comparing,
//!     map_transition.rs's `SKATE_MAP_CHANGE=<map name>@<seconds>` (request a map change that
//!     many seconds after the first frame, once, for measuring a map change's memory),
//!     page_cache.rs's `SKATE_PAGE_CACHE=0` (build the texture pages at every launch) and
//!     `SKATE_PAGE_CACHE_VERIFY=1` (build them anyway and compare them with the cached copy),
//!     game_audio's `SKATE_AUDIO_EARLY_LOAD=0` (load the start-up sounds in the first frame,
//!     not while the scene is built; native/startup.rs), Mesa's `NAK_DEBUG` and
//!     `NVK_SWITCH_DEAD_COLOR` (bevy_horizon's docs/toolchain.md), or bevy_horizon's
//!     `BH_THREAD_CORES` (the cores new threads may run
//!     on; its docs/platform-notes.md), `BEVY_HORIZON_CPU_PROFILE` (its profiler.rs),
//!     `BH_ROBUST_BUFFER_ACCESS=0` (its patched wgpu-hal: drops the bounds check NVK otherwise
//!     puts on every storage-buffer load) and `BH_RT_COMPRESSION` (the same wgpu-hal: large
//!     render targets get memory of their own, which NVK can keep compressed; its
//!     docs/rendering.md). Set after `log=`, so an `env RUST_LOG=` line wins, and after
//!     `RELEASE_DEFAULTS` below, so `env SKATE_WORLD_LEAN=0` turns one of those off.
//!
//! Without an `options.txt`, the game runs with `RELEASE_DEFAULTS`: the rendering options
//! perf36's `options-lean.txt` played with on the Switch (findings 55).
use bevy::app::{
    First, FixedFirst, FixedLast, FixedMain, FixedPostUpdate, FixedPreUpdate, FixedUpdate, Last,
    Main, PluginGroupBuilder, PostStartup, PostUpdate, PreStartup, PreUpdate, RunFixedMainLoop,
    SpawnScene, Startup, Update,
};
use bevy::ecs::schedule::{ExecutorKind, ScheduleLabel};
use bevy::prelude::{App, Local};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static OPTIONS: OnceLock<String> = OnceLock::new();

/// The environment the release sets before `options.txt`'s `env` lines (top of file): perf36's
/// `options-lean.txt`, LEAN on and IMPLICIT off (findings 55). The rest of that file was
/// already the default: its log filter (`init`) and `SKATE_WORLD_IMPLICIT=0`.
const RELEASE_DEFAULTS: &[(&str, &str)] = &[
    ("SKATE_WORLD_LEAF_BUDGET", "1024"),
    ("SKATE_WORLD_PAGE_ARRAY", "1"),
    ("SKATE_WORLD_PAGE_UNIFORM", "1"),
    ("SKATE_WORLD_FEATURES", "1"),
    ("SKATE_OCCLUSION", "1"),
    ("SKATE_OCCLUSION_SPAN", "8"),
    ("SKATE_PYRAMID_ONCE", "1"),
    ("SKATE_WORLD_SLOT_WATERFALL", "1"),
    ("BH_RT_COMPRESSION", "1"),
    ("SKATE_VIS_CULL", "2"),
    ("SKATE_SHADOW_BOUNDS_TIGHT", "1"),
    ("SKATE_WORLD_SHORT_LIVES", "1"),
    ("SKATE_HUD_RECT", "1"),
    ("SKATE_WORLD_LEAN", "1"),
];

/// Whether `options.txt` has `name` on a line of its own.
fn option(name: &str) -> bool {
    OPTIONS.get().is_some_and(|options| options.lines().any(|line| line.trim() == name))
}

/// The value of `options.txt`'s `name=value` line.
fn value(name: &str) -> Option<&'static str> {
    OPTIONS.get()?.lines().find_map(|line| line.trim().strip_prefix(name)?.strip_prefix('='))
}

/// Whether `options.txt` asks for the `[Memory]` lines while the game runs (top of file).
fn memory_log() -> bool {
    option("memory_log") && !option("no_memory_log")
}

/// Whether `options.txt` asks for uncompressed world texture pages.
pub(crate) fn rgba_pages() -> bool {
    option("rgba_pages")
}

/// Runs first in `main`, before anything reads the environment or the SD card.
pub(crate) fn init() {
    bevy_horizon::init_switch_logging("skate3rust");
    // bevy_horizon sets BEVY_ASSET_ROOT to the NRO's folder before `main`.
    let root = std::env::var("BEVY_ASSET_ROOT").unwrap_or_else(|_| "sdmc:/switch/skate3rust".into());
    // libnx already starts in the NRO's folder; setting it makes relative paths certain.
    if let Err(error) = std::env::set_current_dir(&root) {
        eprintln!("[Skate] could not make {root} the working directory: {error}");
    }
    let options = OPTIONS.get_or_init(|| std::fs::read_to_string(format!("{root}/options.txt")).unwrap_or_default());
    // Bevy's `trace` feature (Cargo.toml) wraps every system run in an info-level span; a filter
    // that lets those through makes every frame pay for them. The game's log targets start with
    // its binary's name, skate3rust, not the package's.
    let filter = value("log").unwrap_or("warn,skate3rust=info,bevy_horizon=info");
    // SAFETY: nothing else is running yet: no other thread exists, and nothing has read these.
    unsafe {
        std::env::set_var("SKATE3_ASSETS", format!("{root}/assets"));
        std::env::set_var("SKATE3_MODS", format!("{root}/mods"));
        std::env::set_var("RUST_LOG", filter);
        for (name, value) in RELEASE_DEFAULTS {
            if std::env::var_os(name).is_none() {
                std::env::set_var(name, value);
            }
        }
        for (name, value) in options.lines().filter_map(|line| line.trim().strip_prefix("env ")?.split_once('=')) {
            if !name.trim().is_empty() {
                std::env::set_var(name.trim(), value.trim());
            }
        }
        // Without `memory_log`, bevy_horizon's heartbeat has no `[Memory]` line (top of file).
        if !memory_log() && std::env::var_os("BEVY_HORIZON_MEMORY_EVERY").is_none() {
            std::env::set_var("BEVY_HORIZON_MEMORY_EVERY", "0");
        }
    }
    eprintln!("[Skate] assets {root}/assets, log filter {filter:?}, options.txt {:?}", options.trim());
    let release: Vec<String> = RELEASE_DEFAULTS
        .iter()
        .map(|(name, _)| format!("{name}={}", std::env::var(name).unwrap_or_default()))
        .collect();
    eprintln!("[Skate] release options, after options.txt: {}", release.join(" "));
}

/// Changes `DefaultPlugins` for the Switch.
pub(crate) fn plugins(plugins: PluginGroupBuilder) -> PluginGroupBuilder {
    if option("no_pipelined_rendering") {
        eprintln!("[Skate] options.txt: no_pipelined_rendering");
        plugins.disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>()
    } else {
        plugins
    }
}

/// Adds bevy_horizon: the frame loop, window, display mode, HOME menu handling and the
/// controller (which input/platform.rs reads through `bevy_horizon::raw_pad`).
pub(crate) fn install(app: &mut App) {
    app.add_plugins(bevy_horizon::HorizonPlugin);
    // Bevy uploads every new image and mesh in the next frame, by default. University's world
    // texture pages add up to about 235 MiB as BC3 (938 MiB as RGBA8), and while one uploads it
    // is in the heap up to three times: its pixels, wgpu's staging copy and the GPU texture. At
    // most 64 MiB a frame, or one larger asset: the limit never splits an asset (bevy_render
    // `RenderAssetBytesPerFrame`). It doesn't reach the meshes, which Bevy's mesh allocator
    // uploads before the limit counts them; bevy_horizon's `render_0_18` feature (Cargo.toml)
    // paces those, and waits for the GPU after each frame that uploads, so wgpu frees the
    // frame's staging copies before the next frame makes more (bevy_horizon `uploads.rs`).
    app.insert_resource(bevy::render::render_asset::RenderAssetBytesPerFrame::new(64 << 20));
    // Bevy packs the meshes that share a vertex layout into one buffer, a slab, that grows by half
    // again each time it fills, up to 512 MiB, and growing creates the new buffer while the old
    // one still holds what it copies (bevy_render `mesh/allocator.rs`, `reallocate_slab`). NVK
    // takes each buffer from the heap as one block (bevy-horizon docs/platform-notes.md, "GPU
    // memory comes from the same heap"). University's merged world is a few hundred MiB in one
    // layout, so its slab's last growth wants a block that size beside the one it replaces.
    // 64 MiB slabs keep both small; a mesh of 32 MiB or more gets a buffer of its own size.
    if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
        render_app.insert_resource(bevy::render::mesh::allocator::MeshAllocatorSettings {
            max_slab_size: 64 << 20,
            large_threshold: 32 << 20,
            ..Default::default()
        });
    }
    // Bevy draws every directional light's shadow cascades into one Depth32 texture array, a
    // layer per cascade, each `DirectionalLightShadowMap::size` squared (bevy_pbr
    // `render/light.rs`, `prepare_lights`), which NVK takes from the heap as one block. The
    // retail scene's two shadow lights have five cascades between them (retail_character.rs
    // `spawn_shadow_sources`): 80 MiB at Bevy's default of 2048, 20 MiB at 1024, the Switch's
    // default here for its 1280x720 screen.
    let shadow_map = value("shadow_map")
        .and_then(|size| size.trim().parse::<usize>().ok())
        .filter(|size| size.is_power_of_two())
        .unwrap_or(1024);
    if value("shadow_map").is_some() {
        eprintln!("[Skate] options.txt: shadow_map={shadow_map}");
    }
    app.insert_resource(bevy::light::DirectionalLightShadowMap { size: shadow_map });
    if option("no_upload_throttle") {
        eprintln!("[Skate] options.txt: no_upload_throttle");
        app.insert_resource(bevy_horizon::HorizonUploadSettings {
            mesh_bytes_per_frame: None,
            reclaim_after_bytes: None,
            ..Default::default()
        });
    }
    app.add_systems(Last, start_memory_log);
    // One thread for the main world is the default (findings 63 §6.2: 52.5 to 57.5 fps at 67 %
    // while skating). `multi_threaded` in options.txt gives it Bevy's task pool back.
    let single_threaded = !option("multi_threaded");
    if single_threaded {
        eprintln!("[Skate] main world: single-threaded (options.txt `multi_threaded` turns it off)");
    } else {
        eprintln!("[Skate] main world: multi-threaded (options.txt multi_threaded)");
    }
    if single_threaded {
        let main_schedules = [
            Main.intern(), PreStartup.intern(), Startup.intern(), PostStartup.intern(),
            First.intern(), PreUpdate.intern(), RunFixedMainLoop.intern(), FixedMain.intern(),
            FixedFirst.intern(), FixedPreUpdate.intern(), FixedUpdate.intern(),
            FixedPostUpdate.intern(), FixedLast.intern(), Update.intern(),
            SpawnScene.intern(), PostUpdate.intern(), Last.intern(),
        ];
        for label in main_schedules {
            app.edit_schedule(label, |schedule| {
                schedule.set_executor_kind(ExecutorKind::SingleThreaded);
            });
        }
    }
}

/// The thread `start_memory_log` starts, which `stop_memory_log` joins when the NRO exits.
static MEMORY_LOG: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static STOP_MEMORY_LOG: AtomicBool = AtomicBool::new(false);

/// How long after the first frame the memory log runs, and how often it logs.
const MEMORY_LOG_FOR: u64 = 180;
const MEMORY_LOG_EVERY: u64 = 2;

/// On the first frame, under `memory_log` (top of file), starts a thread that logs a `[Memory]`
/// line every 2 s for 3 minutes, while Bevy uploads the scene a little each frame (`install`
/// above), or until `end_memory_log`.
/// bevy_horizon's heartbeat logs memory once a minute, and only between frames; a thread keeps
/// logging through a frame that stalls.
fn start_memory_log(mut started: Local<bool>) {
    if std::mem::replace(&mut *started, true) {
        return;
    }
    if !memory_log() {
        eprintln!("[Skate] no memory log; options.txt's `memory_log` line starts it");
        return;
    }
    let first_frame = Instant::now();
    let spawned = std::thread::Builder::new().name("memory-log".into()).spawn(move || {
        let mut next = 0;
        while next <= MEMORY_LOG_FOR && !STOP_MEMORY_LOG.load(Ordering::Relaxed) {
            let elapsed = first_frame.elapsed().as_secs();
            if elapsed >= next {
                crate::log_memory(&format!("{elapsed} s after the first frame"));
                next = elapsed + MEMORY_LOG_EVERY;
            }
            // Short sleeps, so `stop_memory_log` doesn't wait long at exit.
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    match spawned {
        Ok(thread) => {
            *MEMORY_LOG.lock().unwrap_or_else(PoisonError::into_inner) = Some(thread);
            bevy_horizon::register_release_hook(stop_memory_log);
        }
        Err(error) => eprintln!("[Skate] could not start the memory log: {error}"),
    }
}

/// Ends the memory log before `SKATE_PERF_REPORT` samples (performance.rs). Each line's figures
/// come from newlib's `mallinfo`, which walks every free block with malloc's lock held: about
/// 41 ms once the map has loaded, while every other thread that allocates waits. perf5's samples
/// had one of those every 2 s (findings 21). The thread sees this within 100 ms and returns.
pub(crate) fn end_memory_log() {
    let running = MEMORY_LOG
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|thread| !thread.is_finished());
    if !STOP_MEMORY_LOG.swap(true, Ordering::Relaxed) && running {
        eprintln!("[Skate] memory log ended for SKATE_PERF sampling");
    }
}

/// Stops the memory log when the NRO exits. A thread still running then keeps its stack's heap
/// pages locked, and bevy_horizon leaves through the HOME menu instead of returning to the
/// homebrew menu (its lifecycle.rs, `release_horizon_services`).
fn stop_memory_log() {
    STOP_MEMORY_LOG.store(true, Ordering::Relaxed);
    if let Some(thread) = MEMORY_LOG.lock().unwrap_or_else(PoisonError::into_inner).take() {
        let _ = thread.join();
    }
}

/// The clocks as `CPU 1020.0 MHz, GPU 307.2 MHz, memory 1331.2 MHz`, read through libnx's clkrst
/// the way bevy-horizon's switch_captest reads them (its platform.rs `Clocks`), or `None` when the
/// service won't open. For the perf report: the GPU drops to 76.8 MHz while CPU boost mode is on.
pub(crate) fn clocks() -> Option<String> {
    // libnx `ClkrstSession` is one 16-byte `Service`; this is larger and at least as aligned.
    #[repr(C, align(8))]
    struct ClkrstSession([u8; 32]);
    unsafe extern "C" {
        fn clkrstInitialize() -> u32;
        fn clkrstExit();
        fn clkrstOpenSession(session: *mut ClkrstSession, module_id: u32, unknown: u32) -> u32;
        fn clkrstGetClockRate(session: *mut ClkrstSession, hz: *mut u32) -> u32;
        fn clkrstCloseSession(session: *mut ClkrstSession);
    }
    // SAFETY: libnx's clkrst calls, each session opened and closed here, the service released below.
    if unsafe { clkrstInitialize() } != 0 {
        return None;
    }
    let mhz = |module| {
        let mut session = ClkrstSession([0; 32]);
        if unsafe { clkrstOpenSession(&mut session, module, 3) } != 0 {
            return "?".to_string();
        }
        let mut hz = 0;
        let rc = unsafe { clkrstGetClockRate(&mut session, &mut hz) };
        unsafe { clkrstCloseSession(&mut session) };
        if rc == 0 { format!("{:.1}", hz as f64 / 1e6) } else { "?".to_string() }
    };
    // libnx pcv.h `PcvModuleId_CpuBus`, `_GPU` and `_EMC`.
    let clocks = format!(
        "CPU {} MHz, GPU {} MHz, memory {} MHz",
        mhz(0x4000_0001),
        mhz(0x4000_0002),
        mhz(0x4000_0039)
    );
    unsafe { clkrstExit() };
    Some(clocks)
}
