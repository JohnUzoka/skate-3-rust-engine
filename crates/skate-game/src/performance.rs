//! Frame timing harness. Active only when `SKATE_PERF_REPORT` names an output
//! path: warms up, samples a fixed window, writes JSON, then exits.
//! `SKATE_PERF_WARMUP` and `SKATE_PERF_SAMPLE` set the two spans in seconds.
//!
//! On the Switch an existing report is kept (the next is `perf-2.json`, and so
//! on), and when bevy_horizon's CPU profiler runs (`BEVY_HORIZON_CPU_PROFILE`)
//! it covers the same window and is written beside the report as `.cpu.txt`.
//!
//! `SKATE_PERF_MATRIX` measures several settings in one launch: steps separated by `;`, each
//! a `,`-separated list of `key=value` changes to the launch settings, or `base` for none.
//! The keys are `prepass` (0 or 1), `cascades` (1-4), `distance` (metres), `filter`
//! (`hardware`, `gaussian` or `temporal`), `draw` (metres), `world_shadows`, `shadow_bounds`,
//! `tight` (0 or 1, `SKATE_SHADOW_BOUNDS_TIGHT`),
//! `lookups` (0 or 1, `SKATE_SHADOW_LOOKUPS`), `hide` (0 or 1, `SKATE_HIDE_SKATER`),
//! `sun_receivers`, `depth_copy`, `cascade_skip`, `hud_retain`, `hudrect` (0 or 1,
//! `SKATE_HUD_RECT`), `direct`, `tdirect`
//! (`SKATE_TONE_DIRECT`), `tgamma` (`SKATE_TONE_GAMMA`) and `occlusion` (0 or 1),
//! `span` (0-32, `SKATE_OCCLUSION_SPAN`), `obatch` (0 or 1, `SKATE_OCCLUSION_BATCH`),
//! `pyramid` (0 or 1, `SKATE_PYRAMID_ONCE`),
//! `ptile` (0 or 1, `SKATE_PYRAMID_TILE`), `ldirect` (0 or 1, `SKATE_LATE_PREPROCESS_DIRECT`),
//! `ipf` (0 or 1, `SKATE_INDIRECT_PHASE_FILTER`), `pblock` (0 or 1, `SKATE_PYRAMID_BLOCK`),
//! `ssc` (0 or 1, `SKATE_SUN_SCISSOR`),
//! `tricull` (0-32 or `nodepth`, `SKATE_TRIANGLE_CULL`), `tcdebug` (0-63,
//! `SKATE_TRIANGLE_CULL_DEBUG`), `tcv1` (0 or 1, `SKATE_TRIANGLE_CULL_V1`),
//! `vcull` (0-3, `SKATE_VIS_CULL`), `vkernel` (0-4, `SKATE_VIS_CULL_KERNEL`),
//! `vafter` (0 or 1, `SKATE_VIS_CULL_AFTER_TEST`), `porder` (0 or 1, `SKATE_PREPASS_ORDER`),
//! `ccull` (0-32, `SKATE_CLUSTER_CULL`), `ccut` (0 or 1, `SKATE_CLUSTER_CUTOUTS`),
//! `ckern` (0 or 1, `SKATE_CLUSTER_KERNEL`),
//! and the world shader's `trim`, `packed`, `lod` and `fetch` (0 or 1), `probe` (0-6),
//! `pprobe` (0-6, `SKATE_PREPASS_PROBE`), `pageu` (0 or 1, `SKATE_WORLD_PAGE_UNIFORM`),
//! `features` (0 or 1, `SKATE_WORLD_FEATURES`), `wfall` (0 or 1,
//! `SKATE_WORLD_SLOT_WATERFALL`), `gpr48` (0 or 1, `SKATE_WORLD_GPR48`), `cflat` (0 or 1,
//! `SKATE_CUTOUT_FLAT`), `slives` (0 or 1, `SKATE_WORLD_SHORT_LIVES`), `lean` (0 or 1,
//! `SKATE_WORLD_LEAN`), `imp` (0 or 1, `SKATE_WORLD_IMPLICIT`), `upscale` (`rcas`, `easu` or
//! `bilinear`, `SKATE_UPSCALE`) and `pwin` (0 or 1, `SKATE_PRESENT_TO_WINDOW`), as in
//! render_options.rs, and `scale`, the 3D
//! render scale in percent (one the graphics menu offers; never saved). The first step warms up
//! for `SKATE_PERF_WARMUP`, each
//! later one settles for `SKATE_PERF_SETTLE` (15 s unless set), and each samples for
//! `SKATE_PERF_SAMPLE` and writes its own report (`perf.json`, `perf-2.json`...). For example
//! `base;prepass=0;cascades=1,distance=40;filter=hardware;draw=300;scale=50`.
//!
//! On the Switch six more keys set NVK's measurement switches (bevy_horizon `nvk_tune`), for
//! measuring only: `ezh`, the GPU's opportunistic early-Z hysteresis (0-19 or 31); `zcull` (0 turns
//! ZCULL off); `zser`, ZCULL serialization (`ENABLE` in bit 0, `APPLIED` in bits 4-5); `fez`
//! (1 tests depth before every fragment shader, even one that can discard: a wrong picture);
//! `zdir`, ZCULL's direction (0 `LESS`, NVK's own, or 1 `GREATER`, for Bevy's reversed depth); and
//! `dskip` (0 sets up every indirect draw, as NVK does without bevy-horizon's
//! `patches/mesa-switch/0012`; 1, NVK's own, skips the setup of the empty ones that GPU culling
//! leaves: the same picture). A step that doesn't name one leaves NVK as it is, but `ezh` and
//! `zser` are GPU state that stays until set again, so a later step goes back by naming a value:
//! the list Linux loads for the Tegra X1 and desktop Maxwell alike has `ezh=6` and `zser=48`; the
//! Switch's own isn't public.
//! A step that doesn't name `zdir` takes the launch direction: bevy_horizon's default, `greater`,
//! or `less` with `BEVY_HORIZON_ZCULL_DIR=less`. The render world hands a step's switches to
//! NVK as its next frame begins (`apply_nvk_switches`), so every render pass of a frame sees the
//! same ones.
//!
//! `SKATE_PERF_FREEZE=1` stops the game clock (`Time<Virtual>`, its speed set to zero) as the
//! first step starts sampling, so every step draws the same moment: the sun, the skater's pose and
//! the physics stand still while frames are still drawn. Steps then differ only by their settings,
//! and a setting that draws the same pixels shows the same picture. Frozen frames run no physics
//! or animation, so their CPU times are lower than in play.
//!
//! `SKATE_PERF_CAMERA="x,y,z,qx,qy,qz,qw"` holds the gameplay camera at that position and
//! rotation from the first frame, as a report's `camera` gives them (`translation` and
//! `rotation_xyzw`), so a launch can measure a view found in play. The skater stays where it
//! spawned, and the field of view stays the game's.
//!
//! No step samples while a render pipeline is still compiling: after its warmup or settle it
//! waits for the last one, up to 90 s, and reports the wait as `pipeline_wait_seconds`. On the
//! Switch, the first step to sample also ends the game's memory log (horizon.rs), whose
//! allocator walks hold the allocator's lock for tens of milliseconds each.
//!
//! `SKATE_PERF_GPU=1` adds Bevy's GPU timers for each pass (`RenderDiagnosticsPlugin`) and
//! gpu_frame_time.rs's for the whole frame. Each step then prints its passes' times
//! (`SKATE_PERF_GPU_MS`) and one `SKATE_PERF_GPU_FRAME` line: the frame's GPU time, the GPU's
//! time between frames, and how much of the frame no pass timer covers. A
//! `SKATE_PERF_GPU_FRAME_SPREAD` line follows with the 10th, 50th and 90th percentiles and the
//! most of the first two, over the same frames.
//!
//! With `SKATE_PERF_GPU=1` a step can name `timers=0` to measure what Bevy's timers cost: the
//! step runs without its pass timers and pipeline statistics (`switch_pass_timers`), while
//! gpu_frame_time.rs's frame timer stays, so the step's `busy` and frame times compare with a
//! step that keeps them. Its report gives `pass_timers` false and no pass times, and its
//! `untimed` is its whole `busy`. The next step without `timers=0` has them again.
//!
//! The `Performance` resource is absent in normal play, which is why callers
//! take it as `Option<ResMut<_>>` — instrumentation must never cost anything in
//! a shipping session.
use bevy::prelude::*;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// Seconds discarded before sampling, counted from the first frame, so shader
/// compilation, asset streaming and the map publish settle do not pollute the
/// window. The first frame, not startup: on the Switch it comes ~30 s in.
const WARMUP: f32 = 10.0;
/// Seconds of samples retained.
const SAMPLE: f32 = 15.0;
/// Seconds each `SKATE_PERF_MATRIX` step after the first runs before it samples, for the
/// pipelines a new setting needs to compile and the render target to resize.
const SETTLE: f32 = 15.0;
/// Seconds a step waits past its warmup or settle for pipelines still compiling
/// (`PendingPipelines`) before it samples anyway.
const PIPELINE_WAIT: f32 = 90.0;
/// A frame slower than this is counted separately: at 300 FPS the budget is
/// 3.33 ms, so 8 ms is an unambiguous hitch rather than jitter.
const HITCH_MS: f32 = 8.0;

/// Draw statistics for one frame, owned by the main world.
///
/// `world_draws` and `world_triangles` count the world leaves the active 3D cameras draw after
/// frustum culling and `SKATE_DRAW_DISTANCE`; `sun_cascade_*` count the leaves each of the scene
/// sun's shadow cascades for those cameras draws (`world_draw_stats`), and the texels it may draw
/// into. Nothing fills the mod and other counts yet, so `draw_calls` and `triangles` in the
/// report cover the world alone.
#[derive(Resource, Default, Clone, Copy)]
pub(crate) struct DrawStats {
    pub world_draws: u32,
    pub world_triangles: u32,
    pub mod_draws: u32,
    pub mod_triangles: u32,
    pub other_draws: u32,
    /// One entry per cascade, nearest first; render_options.rs allows at most 4.
    pub sun_cascade_draws: [u32; 4],
    pub sun_cascade_triangles: [u32; 4],
    /// The area of the rectangle `SKATE_SUN_SCISSOR` keeps each cascade to, the whole layer's
    /// without one, or 0 for a cascade that draws nothing.
    pub sun_cascade_texels: [u32; 4],
}
impl DrawStats {
    pub(crate) fn total_draws(&self) -> u32 {
        self.world_draws + self.mod_draws + self.other_draws
    }
}

/// CPU cost of the render schedule, split at phase boundaries.
///
/// Bevy pipelines the render sub-app against the next frame's main schedule, so
/// when `frame_ms` exceeds `main_schedule_ms` the render app is the critical
/// path and this is the only way to see which phase owns it. The main world and
/// the render world hold clones of the same `Arc`, which is why the counters are
/// atomics rather than a plain resource: they are written in the render world
/// and read in the main world with no ordering guarantee between the two.
#[derive(Resource, Clone, Default)]
pub(crate) struct RenderPhases(Arc<Phases>);

#[derive(Default)]
struct Phases {
    started: AtomicU64,
    extract_ns: AtomicU64,
    assets_ns: AtomicU64,
    views_ns: AtomicU64,
    queue_ns: AtomicU64,
    prepare_ns: AtomicU64,
    total_ns: AtomicU64,
    frames: AtomicU64,
}

/// How many pipelines the render world's cache was still compiling as its last frame ended.
/// Bevy skips a draw until its pipeline is ready, so a step that needs new ones, like a world
/// shader variant (render_options.rs `WorldVariant`), would otherwise sample frames missing those
/// draws. On the Switch the world's pipelines take seconds each to compile.
#[derive(Resource, Clone, Default)]
pub(crate) struct PendingPipelines(Arc<AtomicUsize>);

fn count_pending_pipelines(
    cache: Res<bevy::render::render_resource::PipelineCache>,
    pending: Res<PendingPipelines>,
) {
    pending.0.store(cache.waiting_pipelines().count(), Ordering::Relaxed);
}

/// Wall-clock nanoseconds since an arbitrary fixed origin.
///
/// `Instant` cannot live in an atomic, and the render world needs to hand a
/// timestamp to systems that run later in the same schedule.
fn now_ns() -> u64 {
    use std::sync::LazyLock;
    static ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);
    ORIGIN.elapsed().as_nanos() as u64
}

impl RenderPhases {
    /// Mean milliseconds per frame for each phase, in report order.
    fn means(&self) -> [f32; 6] {
        let frames = self.0.frames.load(Ordering::Relaxed).max(1) as f32;
        let mean = |counter: &AtomicU64| {
            counter.load(Ordering::Relaxed) as f32 / frames / 1.0e6
        };
        [
            mean(&self.0.extract_ns),
            mean(&self.0.assets_ns),
            mean(&self.0.views_ns),
            mean(&self.0.queue_ns),
            mean(&self.0.prepare_ns),
            mean(&self.0.total_ns),
        ]
    }

    fn reset(&self) {
        for counter in [
            &self.0.extract_ns,
            &self.0.assets_ns,
            &self.0.views_ns,
            &self.0.queue_ns,
            &self.0.prepare_ns,
            &self.0.total_ns,
            &self.0.frames,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }
}

/// Counts the world leaves, and their triangles, that the active 3D cameras and the scene sun's
/// shadow cascades for those cameras draw this frame.
///
/// This reads each view's own list because `ViewVisibility` can't tell views apart: Bevy also
/// marks an entity visible when only a shadow cascade sees it, so that casters outside the
/// camera's view still render (bevy_light 0.18.1 `check_dir_light_mesh_visibility`). Only the 3D
/// cameras' cascades count. Bevy builds cascades for every active camera, the 2D HUD and UI
/// cameras included (bevy_light `build_directional_light_cascades`), but renders shadow maps
/// only for `Camera3d` views (bevy_pbr `prepare_lights`). A cascade's texels are those of the
/// rectangle the shadow pass keeps it to (`ShadowScissors`), if its map is the size the
/// rectangles are for, as the pass checks.
fn world_draw_stats(
    mut stats: ResMut<DrawStats>,
    leaves: Query<&crate::skate_world::LeafTriangles>,
    cameras: Query<(Entity, &Camera, &bevy::camera::visibility::VisibleEntities), With<Camera3d>>,
    suns: Query<
        (Entity, &bevy::camera::visibility::CascadesVisibleEntities),
        With<crate::render_options::SceneSun>,
    >,
    shadow_map: Option<Res<bevy::light::DirectionalLightShadowMap>>,
    scissors: Option<Res<bevy::pbr::ShadowScissors>>,
) {
    let count = |entities: &[Entity]| {
        leaves
            .iter_many(entities)
            .fold((0, 0), |(draws, triangles), leaf| (draws + 1, triangles + leaf.0))
    };
    let mesh = std::any::TypeId::of::<Mesh3d>();
    let size = shadow_map.map_or(0, |map| u32::try_from(map.size).unwrap_or(u32::MAX));
    let scissors = scissors.as_deref().filter(|scissors| scissors.size == size);
    let (mut draws, mut triangles) = (0, 0);
    stats.sun_cascade_draws = [0; 4];
    stats.sun_cascade_triangles = [0; 4];
    stats.sun_cascade_texels = [0; 4];
    for (view, _, visible) in cameras.iter().filter(|(_, camera, _)| camera.is_active) {
        let (d, t) = count(visible.get(mesh));
        draws += d;
        triangles += t;
        for (sun, cascades) in
            suns.iter().filter_map(|(sun, visible)| Some((sun, visible.entities.get(&view)?)))
        {
            for (i, cascade) in cascades.iter().enumerate().take(4) {
                let (d, t) = count(&cascade.entities);
                stats.sun_cascade_draws[i] += d;
                stats.sun_cascade_triangles[i] += t;
                if cascade.entities.is_empty() {
                    continue;
                }
                let key = crate::retail_shadow_bounds::cascade_key(sun, view, i);
                let texels = match scissors.and_then(|scissors| scissors.rects.get(&key)) {
                    Some(rect) => {
                        let max = rect.max.min(UVec2::splat(size));
                        (max - rect.min.min(max)).element_product()
                    }
                    None => size.saturating_mul(size),
                };
                stats.sun_cascade_texels[i] = stats.sun_cascade_texels[i].saturating_add(texels);
            }
        }
    }
    stats.world_draws = draws;
    stats.world_triangles = triangles;
}

/// Accumulates the span from the recorded phase start to now, then re-arms the
/// start for the next phase.
fn mark(phases: &RenderPhases, counter: &AtomicU64) {
    let now = now_ns();
    let start = phases.0.started.swap(now, Ordering::Relaxed);
    counter.fetch_add(now.saturating_sub(start), Ordering::Relaxed);
}

#[derive(Default, Clone, Copy)]
struct Frame {
    total_ms: f32,
    main_ms: f32,
    physics_ms: f32,
    draws: u32,
    triangles: u32,
}

/// One `SKATE_PERF_MATRIX` step: what it changes from the launch settings, and those settings
/// with the change made.
#[derive(Clone, Debug, PartialEq)]
struct Step {
    label: String,
    options: crate::render_options::RenderOptions,
    scale: Option<u32>,
    /// NVK's measurement switches, in `NVK_KEYS` order: -1 leaves NVK as it is.
    nvk: [i32; 6],
    /// Bevy's pass timers and pipeline statistics: false for a `timers=0` step.
    timers: bool,
}

/// The `SKATE_PERF_MATRIX` keys for NVK's measurement switches, in bevy_horizon's `nvk_tune::Knob`
/// order.
const NVK_KEYS: [&str; 6] = ["ezh", "zcull", "zser", "fez", "zdir", "dskip"];

/// A step's NVK switches, from the main world's `apply_step` to the render world's
/// `apply_nvk_switches`, which hands them to NVK as its next frame begins. With pipelined
/// rendering `apply_step` runs while the render world still records the frame before, and a
/// change of ZCULL's direction between one frame's render passes costs that frame its ZCULL
/// (bevy_horizon `nvk_tune::Knob::ZcullDir`).
#[derive(Resource, Clone, Default)]
struct NvkSwitches(Arc<Mutex<Option<[i32; 6]>>>);

/// Hands the switches of a step that started since the last frame to NVK, before this frame's
/// render passes are recorded.
#[cfg(target_os = "horizon")]
fn apply_nvk_switches(switches: Res<NvkSwitches>) {
    let Some(values) = switches.0.lock().unwrap_or_else(PoisonError::into_inner).take() else {
        return;
    };
    use bevy_horizon::nvk_tune::{Knob, set};
    let knobs = [
        Knob::EarlyZHysteresis,
        Knob::Zcull,
        Knob::ZcullSerialization,
        Knob::EarlyZ,
        Knob::ZcullDir,
        Knob::SkipEmptyDraws,
    ];
    for ((knob, value), key) in knobs.into_iter().zip(values).zip(NVK_KEYS) {
        if !set(knob, value) {
            warn!("SKATE_PERF_MATRIX: NVK can't take {key}={value}; it keeps what it had");
        }
    }
}

/// Whether the render world keeps Bevy's pass timers and pipeline statistics, from the main
/// world's `apply_step` to `switch_pass_timers`.
#[derive(Resource, Clone)]
struct PassTimers(Arc<AtomicBool>);

impl Default for PassTimers {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}

/// Bevy's diagnostics recorder while a `timers=0` step runs without it.
#[derive(Resource)]
struct StashedRecorder<T: Resource>(T);

/// Adds `switch_pass_timers` to the render world. Bevy's recorder (`DiagnosticsRecorder`) is
/// public but in a private module, so its type is taken from `RenderContext::new`'s parameter.
fn add_pass_timer_switch<T: Resource>(
    render: &mut bevy::app::SubApp,
    timers: PassTimers,
    _recorder_type: fn(
        bevy::render::renderer::RenderDevice,
        Option<T>,
    ) -> bevy::render::renderer::RenderContext<'static>,
) {
    use bevy::render::{Render, RenderSystems};
    render
        .insert_resource(timers)
        .add_systems(Render, switch_pass_timers::<T>.before(RenderSystems::Render));
}

/// Takes Bevy's recorder out of the render world for a `timers=0` step, and puts it back for the
/// next, before the frame's passes are recorded: `render_system` records pass times and pipeline
/// statistics only while the recorder is there. Frames recorded before a step stopped them are
/// collected once it is back, in the next step's settle.
fn switch_pass_timers<T: Resource>(world: &mut World) {
    if world.resource::<PassTimers>().0.load(Ordering::Relaxed) {
        if let Some(StashedRecorder(recorder)) = world.remove_resource::<StashedRecorder<T>>() {
            world.insert_resource(recorder);
            info!("SKATE_PERF: Bevy's pass timers are back");
        }
    } else if let Some(recorder) = world.remove_resource::<T>() {
        world.insert_resource(StashedRecorder(recorder));
        info!("SKATE_PERF: Bevy's pass timers are off for this step");
    }
}

/// The steps in `matrix`. Each starts from the launch settings, which `var` reads (the
/// environment, in the game). An unknown key or value is logged and left out, so a typo measures
/// the launch setting rather than nothing.
fn parse_matrix(
    matrix: &str,
    var: impl Fn(&str) -> Option<String>,
) -> Vec<Step> {
    const KEYS: [(&str, &str); 53] = [
        ("prepass", "SKATE_DEPTH_PREPASS"),
        ("cascades", "SKATE_SUN_CASCADES"),
        ("distance", "SKATE_SUN_SHADOW_DISTANCE"),
        ("filter", "SKATE_SHADOW_FILTER"),
        ("draw", "SKATE_DRAW_DISTANCE"),
        ("world_shadows", "SKATE_WORLD_SHADOWS"),
        ("shadow_bounds", "SKATE_SHADOW_BOUNDS"),
        ("tight", "SKATE_SHADOW_BOUNDS_TIGHT"),
        ("lookups", "SKATE_SHADOW_LOOKUPS"),
        ("hide", "SKATE_HIDE_SKATER"),
        ("sun_receivers", "SKATE_SUN_RECEIVERS"),
        ("depth_copy", "SKATE_PREPASS_DEPTH_COPY"),
        ("cascade_skip", "SKATE_CASCADE_SKIP"),
        ("hud_retain", "SKATE_HUD_RETAIN"),
        ("hudrect", "SKATE_HUD_RECT"),
        ("direct", "SKATE_PRESENT_DIRECT"),
        ("tdirect", "SKATE_TONE_DIRECT"),
        ("tgamma", "SKATE_TONE_GAMMA"),
        ("occlusion", "SKATE_OCCLUSION"),
        ("span", "SKATE_OCCLUSION_SPAN"),
        ("obatch", "SKATE_OCCLUSION_BATCH"),
        ("pyramid", "SKATE_PYRAMID_ONCE"),
        ("ptile", "SKATE_PYRAMID_TILE"),
        ("ldirect", "SKATE_LATE_PREPROCESS_DIRECT"),
        ("ipf", "SKATE_INDIRECT_PHASE_FILTER"),
        ("pblock", "SKATE_PYRAMID_BLOCK"),
        ("ssc", "SKATE_SUN_SCISSOR"),
        ("tricull", "SKATE_TRIANGLE_CULL"),
        ("tcdebug", "SKATE_TRIANGLE_CULL_DEBUG"),
        ("tcv1", "SKATE_TRIANGLE_CULL_V1"),
        ("vcull", "SKATE_VIS_CULL"),
        ("vkernel", "SKATE_VIS_CULL_KERNEL"),
        ("vafter", "SKATE_VIS_CULL_AFTER_TEST"),
        ("porder", "SKATE_PREPASS_ORDER"),
        ("ccull", "SKATE_CLUSTER_CULL"),
        ("ccut", "SKATE_CLUSTER_CUTOUTS"),
        ("ckern", "SKATE_CLUSTER_KERNEL"),
        ("trim", "SKATE_WORLD_TRIM"),
        ("packed", "SKATE_WORLD_PACKED"),
        ("lod", "SKATE_WORLD_LOD"),
        ("probe", "SKATE_WORLD_PROBE"),
        ("pprobe", "SKATE_PREPASS_PROBE"),
        ("fetch", "SKATE_WORLD_FETCH"),
        ("pageu", "SKATE_WORLD_PAGE_UNIFORM"),
        ("features", "SKATE_WORLD_FEATURES"),
        ("wfall", "SKATE_WORLD_SLOT_WATERFALL"),
        ("gpr48", "SKATE_WORLD_GPR48"),
        ("cflat", "SKATE_CUTOUT_FLAT"),
        ("slives", "SKATE_WORLD_SHORT_LIVES"),
        ("lean", "SKATE_WORLD_LEAN"),
        ("imp", "SKATE_WORLD_IMPLICIT"),
        ("upscale", "SKATE_UPSCALE"),
        ("pwin", "SKATE_PRESENT_TO_WINDOW"),
    ];
    let mut steps = Vec::new();
    // Every step starts from the launch settings, and bevy_horizon sets ZCULL's direction at
    // launch: `greater` unless `BEVY_HORIZON_ZCULL_DIR=less` (its nvk_tune.rs).
    let launch_zdir = match var("BEVY_HORIZON_ZCULL_DIR").as_deref().map(str::trim) {
        Some("less") => 0,
        _ => 1,
    };
    for label in matrix.split(';').map(str::trim).filter(|step| !step.is_empty()) {
        let mut set = Vec::new();
        let mut scale = None;
        let mut nvk = [-1, -1, -1, -1, launch_zdir, -1];
        let mut timers = true;
        for item in label.split(',').map(str::trim).filter(|item| *item != "base") {
            let (key, value) = item.split_once('=').unwrap_or((item, ""));
            let (key, value) = (key.trim(), value.trim());
            if key == "scale" {
                match value.parse() {
                    Ok(percent) => scale = Some(percent),
                    Err(_) => warn!("SKATE_PERF_MATRIX: `{item}` is not a scale in percent"),
                }
            } else if key == "timers" {
                match value {
                    "0" => timers = false,
                    "1" => timers = true,
                    _ => warn!("SKATE_PERF_MATRIX: `{item}` is not timers=0 or timers=1"),
                }
            } else if let Some((_, name)) = KEYS.iter().find(|(k, _)| *k == key) {
                set.push((*name, value.to_string()));
            } else if let Some(knob) = NVK_KEYS.iter().position(|k| *k == key) {
                match value.parse::<i32>() {
                    // ZCULL on, each shader's own early Z and skipping empty draws are NVK's own
                    // behaviour.
                    Ok(1) if key == "zcull" || key == "dskip" => nvk[knob] = -1,
                    Ok(0) if key == "fez" => nvk[knob] = -1,
                    Ok(value) => nvk[knob] = value,
                    Err(_) => warn!("SKATE_PERF_MATRIX: `{item}` is not a number"),
                }
            } else {
                warn!("SKATE_PERF_MATRIX: unknown setting `{item}` in step `{label}`");
            }
        }
        let options = crate::render_options::RenderOptions::parse(|name| {
            set.iter()
                .rev()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
                .or_else(|| var(name))
        });
        steps.push(Step { label: label.to_string(), options, scale, nvk, timers });
    }
    steps
}

#[derive(Resource)]
pub(crate) struct Performance {
    output: PathBuf,
    warmup: f32,
    sample: f32,
    settle: f32,
    /// `SKATE_PERF_MATRIX`'s steps; empty for a single report at the launch settings.
    steps: Vec<Step>,
    step: usize,
    /// The step whose settings are in place.
    applied: Option<usize>,
    /// The graphics menu's render scale before the first step changed it.
    launch_scale: Option<u32>,
    /// When the current step started; the first frame for the first.
    step_started: Option<Instant>,
    /// When the current step's sample window opened.
    sampling_started: Option<Instant>,
    /// Seconds the current step waited past its warmup or settle for pipelines to compile.
    pipeline_wait: f32,
    pipeline_wait_logged: bool,
    first_frame: Option<Instant>,
    frame_started: Option<Instant>,
    main_elapsed: Duration,
    physics_elapsed: Duration,
    samples: Vec<Frame>,
    menu_open_frames: usize,
    sampling: bool,
    finished: bool,
    /// `SKATE_PERF_FREEZE`: the game clock stops as the first step starts sampling.
    freeze: bool,
    /// The current step's NVK switches, for the render world.
    nvk: NvkSwitches,
    /// Whether the current step keeps Bevy's pass timers, for the render world.
    timers: PassTimers,
    /// The thread naming and writing the last step's report (`write_report_later`).
    writer: Option<std::thread::JoinHandle<()>>,
}

impl Performance {
    fn new(
        output: PathBuf,
        warmup: f32,
        sample: f32,
        settle: f32,
        steps: Vec<Step>,
        freeze: bool,
    ) -> Self {
        Self {
            output,
            warmup,
            sample,
            settle,
            steps,
            step: 0,
            applied: None,
            launch_scale: None,
            step_started: None,
            sampling_started: None,
            pipeline_wait: 0.0,
            pipeline_wait_logged: false,
            first_frame: None,
            frame_started: None,
            main_elapsed: Duration::ZERO,
            physics_elapsed: Duration::ZERO,
            // 15 s at an optimistic 600 FPS; growth beyond this is harmless.
            samples: Vec::with_capacity(9_000),
            menu_open_frames: 0,
            sampling: false,
            finished: false,
            freeze,
            nvk: NvkSwitches::default(),
            timers: PassTimers::default(),
            writer: None,
        }
    }

    /// Physics tick cost, reported by `physics::advance`. FixedUpdate may run
    /// zero or several times per frame, so this accumulates until the frame ends.
    pub(crate) fn physics(&mut self, elapsed: Duration) {
        self.physics_elapsed += elapsed;
    }
}

pub(crate) struct PerformancePlugin;
impl Plugin for PerformancePlugin {
    fn build(&self, app: &mut App) {
        let Some(output) = std::env::var_os("SKATE_PERF_REPORT").map(PathBuf::from) else {
            return;
        };
        // Runs are compared on the Switch by copying the reports off afterwards,
        // so one must not overwrite the last.
        #[cfg(target_os = "horizon")]
        let output = unused_path(output);
        let warmup = seconds_from_env("SKATE_PERF_WARMUP", WARMUP);
        let sample = seconds_from_env("SKATE_PERF_SAMPLE", SAMPLE);
        let settle = seconds_from_env("SKATE_PERF_SETTLE", SETTLE);
        let freeze = std::env::var("SKATE_PERF_FREEZE").is_ok_and(|value| value.trim() == "1");
        let steps = std::env::var("SKATE_PERF_MATRIX")
            .map(|matrix| parse_matrix(&matrix, |name| std::env::var(name).ok()))
            .unwrap_or_default();
        info!(
            "SKATE_PERF starting: warmup={warmup}s sample={sample}s output={output:?} steps={:?} \
             freeze={freeze}",
            steps.iter().map(|step| step.label.as_str()).collect::<Vec<_>>()
        );
        let camera = std::env::var("SKATE_PERF_CAMERA").ok().and_then(|value| {
            let pose = parse_camera(&value);
            if pose.is_none() {
                warn!("SKATE_PERF_CAMERA: `{value}` is not x,y,z,qx,qy,qz,qw");
            }
            pose
        });
        let phases = RenderPhases::default();
        let pending = PendingPipelines::default();
        let performance = Performance::new(output, warmup, sample, settle, steps, freeze);
        #[cfg(target_os = "horizon")]
        let nvk = performance.nvk.clone();
        let timers = performance.timers.clone();
        app.insert_resource(performance)
            .init_resource::<DrawStats>()
            .insert_resource(phases.clone())
            .insert_resource(pending.clone())
            .add_systems(First, frame_begin)
            .add_systems(
                PostUpdate,
                world_draw_stats
                    .after(bevy::camera::visibility::VisibilitySystems::CheckVisibility)
                    .after(bevy::light::SimulationLightSystems::CheckLightVisibility)
                    .after(crate::retail_shadow_bounds::sun_receivers),
            )
            .add_systems(Last, frame_end);
        if let Some(pose) = camera {
            info!(
                "SKATE_PERF_CAMERA: the camera holds translation={:?} rotation_xyzw={:?}",
                pose.translation.to_array(),
                pose.rotation.to_array()
            );
            // After the game places the camera, and before the frame's checks and the transform
            // propagation that culling and the shadow cascades read.
            app.insert_resource(HeldCamera(pose)).add_systems(
                Update,
                hold_camera
                    .after(crate::camera::present)
                    .before(crate::app::FrameSet::Verification),
            );
        }
        use bevy::render::{Render, RenderApp, RenderSystems};
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            // The cache's queue is processed in `RenderSystems::Render`.
            render.insert_resource(pending).add_systems(
                Render,
                count_pending_pipelines
                    .after(RenderSystems::Render)
                    .before(RenderSystems::Cleanup),
            );
            // Render passes are recorded in `RenderSystems::Render`.
            #[cfg(target_os = "horizon")]
            render
                .insert_resource(nvk)
                .add_systems(Render, apply_nvk_switches.before(RenderSystems::Render));
        }
        // GPU timestamp/statistics queries add work of their own. Ordinary CPU
        // frame comparisons must not enable them implicitly.
        if std::env::var("SKATE_PERF_GPU").as_deref() == Ok("1") {
            app.add_plugins((
                bevy::render::diagnostic::RenderDiagnosticsPlugin,
                crate::gpu_frame_time::GpuFrameTimePlugin,
            ));
            if let Some(render) = app.get_sub_app_mut(RenderApp) {
                add_pass_timer_switch(render, timers, bevy::render::renderer::RenderContext::new);
            }
        }

        // Phase-boundary systems serialize otherwise overlapping render work.
        // Only enable that detailed attribution when explicitly requested.
        if std::env::var("SKATE_PERF_RENDER").as_deref() != Ok("1") {
            return;
        }

        let Some(render) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render.insert_resource(phases);
        render.add_systems(
            Render,
            (
                begin_render_frame.before(RenderSystems::ExtractCommands),
                after_extract
                    .after(RenderSystems::ExtractCommands)
                    .before(RenderSystems::PrepareAssets),
                after_assets
                    .after(RenderSystems::PrepareMeshes)
                    .after(RenderSystems::PrepareAssets)
                    .before(RenderSystems::ManageViews),
                after_views
                    .after(RenderSystems::ManageViews)
                    .before(RenderSystems::Queue),
                after_queue
                    .after(RenderSystems::QueueSweep)
                    .before(RenderSystems::PhaseSort),
                after_prepare
                    .after(RenderSystems::PrepareBindGroups)
                    .before(RenderSystems::Render),
                end_render_frame.after(RenderSystems::Cleanup),
            ),
        );
    }
}

/// A non-negative number of seconds from `name`, or `default`.
fn seconds_from_env(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .unwrap_or(default)
}

/// `SKATE_PERF_CAMERA`'s pose.
#[derive(Resource, Clone, Copy)]
struct HeldCamera(Transform);

/// `x,y,z,qx,qy,qz,qw`: a position, and a rotation that is normalized.
fn parse_camera(value: &str) -> Option<Transform> {
    let numbers = value
        .split(',')
        .map(|number| number.trim().parse::<f32>().ok().filter(|number| number.is_finite()))
        .collect::<Option<Vec<f32>>>()?;
    let [x, y, z, qx, qy, qz, qw] = numbers[..] else {
        return None;
    };
    let rotation = Quat::from_xyzw(qx, qy, qz, qw);
    (rotation.length_squared() > 1e-6)
        .then(|| Transform::from_xyz(x, y, z).with_rotation(rotation.normalize()))
}

fn hold_camera(
    held: Res<HeldCamera>,
    mut cameras: Query<&mut Transform, With<crate::camera::GameplayCamera>>,
) {
    for mut transform in &mut cameras {
        *transform = held.0;
    }
}

/// `path` if nothing is there, else the first free `<stem>-2.<ext>`, `<stem>-3.<ext>`...
fn unused_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let extension = path.extension().map(|extension| extension.to_string_lossy().into_owned());
    (2..)
        .map(|n| match &extension {
            Some(extension) => path.with_file_name(format!("{stem}-{n}.{extension}")),
            None => path.with_file_name(format!("{stem}-{n}")),
        })
        .find(|candidate| !candidate.exists())
        .expect("an unbounded range has a free name")
}

fn begin_render_frame(phases: Res<RenderPhases>) {
    phases.0.started.store(now_ns(), Ordering::Relaxed);
}

fn after_extract(phases: Res<RenderPhases>) {
    mark(&phases, &phases.0.extract_ns);
}

fn after_assets(phases: Res<RenderPhases>) {
    mark(&phases, &phases.0.assets_ns);
}

fn after_views(phases: Res<RenderPhases>) {
    mark(&phases, &phases.0.views_ns);
}

fn after_queue(phases: Res<RenderPhases>) {
    mark(&phases, &phases.0.queue_ns);
}

fn after_prepare(phases: Res<RenderPhases>) {
    mark(&phases, &phases.0.prepare_ns);
}

/// Closes the frame: the residual span is the render graph plus cleanup, and the
/// frame counter is what turns every accumulator into a per-frame mean.
fn end_render_frame(phases: Res<RenderPhases>) {
    mark(&phases, &phases.0.total_ns);
    phases.0.frames.fetch_add(1, Ordering::Relaxed);
}

fn frame_begin(mut performance: ResMut<Performance>) {
    let now = Instant::now();
    performance.first_frame.get_or_insert(now);
    performance.frame_started = Some(now);
    performance.main_elapsed = Duration::ZERO;
    performance.physics_elapsed = Duration::ZERO;
}

fn frame_end(
    mut performance: ResMut<Performance>,
    time: Res<Time<Real>>,
    draws: Res<DrawStats>,
    phases: Res<RenderPhases>,
    pending: Res<PendingPipelines>,
    diagnostics: Res<bevy::diagnostic::DiagnosticsStore>,
    mut menu: Option<ResMut<crate::graphics_menu::Menu>>,
    mut options: Option<ResMut<crate::render_options::RenderOptions>>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform, &Projection), With<crate::camera::GameplayCamera>>,
    frame_state: Option<Res<crate::retail_render::FrameStateData>>,
    mut clock: ResMut<Time<Virtual>>,
    mut exit: MessageWriter<AppExit>,
) {
    if performance.finished {
        return;
    }
    if performance.applied != Some(performance.step) {
        apply_step(&mut performance, menu.as_deref_mut(), options.as_deref_mut());
    }
    let started = performance.step_started.or(performance.first_frame);
    let elapsed = started.map_or(0.0, |started| started.elapsed().as_secs_f32());
    let warmup = if performance.step == 0 { performance.warmup } else { performance.settle };
    let sample = performance.sample;
    if !performance.sampling {
        if elapsed < warmup {
            return;
        }
        let compiling = pending.0.load(Ordering::Relaxed);
        if compiling > 0 && elapsed < warmup + PIPELINE_WAIT {
            if !performance.pipeline_wait_logged {
                performance.pipeline_wait_logged = true;
                info!("SKATE_PERF waiting for {compiling} pipelines to compile before sampling");
            }
            return;
        }
        if compiling > 0 {
            warn!("SKATE_PERF {compiling} pipelines still compiling after {PIPELINE_WAIT}s more; sampling anyway");
        }
        performance.pipeline_wait = elapsed - warmup;
        performance.sampling = true;
        performance.sampling_started = Some(Instant::now());
        // Warmup includes shader compilation and asset streaming, whose cost
        // would otherwise dominate the phase accumulators.
        phases.reset();
        #[cfg(target_os = "horizon")]
        {
            // The last step's CPU profile is written on the report's thread: it must be on the
            // card before its samples go.
            if bevy_horizon::profiler::running()
                && let Some(writer) = performance.writer.take()
            {
                let _ = writer.join();
            }
            bevy_horizon::profiler::reset();
            crate::horizon::end_memory_log();
        }
        info!(
            "SKATE_PERF warmup complete after {:.1}s waiting for pipelines, sampling {sample}s",
            performance.pipeline_wait
        );
        if performance.freeze && performance.step == 0 {
            clock.set_relative_speed(0.0);
            info!("SKATE_PERF_FREEZE: the game clock stops; every step samples this moment");
        }
    }
    // Gameplay stops while the menu is open, so those frames are not play.
    if menu.as_ref().is_some_and(|menu| menu.open) {
        performance.menu_open_frames += 1;
    }

    // Wall-clock delta rather than the frame_begin instant: it includes the
    // presentation wait, which is what the player actually experiences.
    let total_ms = time.delta().as_secs_f32() * 1000.0;
    let main_ms = performance
        .frame_started
        .map_or(0.0, |start| start.elapsed().as_secs_f32() * 1000.0);
    let physics_ms = performance.physics_elapsed.as_secs_f32() * 1000.0;
    performance.samples.push(Frame {
        total_ms,
        main_ms,
        physics_ms,
        draws: draws.total_draws(),
        triangles: draws.world_triangles + draws.mod_triangles,
    });

    let sampled = performance.sampling_started.map_or(0.0, |at| at.elapsed().as_secs_f32());
    if sampled < sample {
        return;
    }
    let step = performance.steps.get(performance.step).map(|step| step.label.clone());
    let last = performance.step + 1 >= performance.steps.len();
    let mut report = summarise(&performance.samples, &draws);
    report.step = step.clone();
    report.pass_timers = std::env::var("SKATE_PERF_GPU").as_deref() == Ok("1")
        && performance.steps.get(performance.step).is_none_or(|step| step.timers);
    report.render_phase_ms = phases.means();
    report.warmup_seconds = warmup;
    report.pipeline_wait_seconds = performance.pipeline_wait;
    report.sample_seconds = sample;
    report.menu_open_frames = performance.menu_open_frames;
    report.clock_frozen = performance.freeze;
    report.graphics_settings = menu.as_ref().map(|menu| menu.diagnostic_settings());
    report.render_options = options.as_ref().map(|options| format!("{:?}", **options));
    report.shadow_casters = frame_state.map(|state| state.casters.to_array());
    #[cfg(target_os = "horizon")]
    {
        report.clocks = crate::horizon::clocks();
    }
    report.window = windows
        .single()
        .ok()
        .map(|window| [window.resolution.physical_width(), window.resolution.physical_height()]);
    report.camera = cameras
        .iter()
        .find(|(camera, ..)| camera.is_active)
        .and_then(|(_, transform, projection)| match projection {
            Projection::Perspective(lens) => Some(CameraPose {
                translation: transform.translation().to_array(),
                rotation: transform.rotation().to_array(),
                fov: lens.fov,
                aspect_ratio: lens.aspect_ratio,
                near: lens.near,
                far: lens.far,
            }),
            _ => None,
        });
    // Every render diagnostic, so GPU pass costs land in the report without
    // this module needing to know the pass names the render graph happens to use.
    let since = performance.sampling_started;
    report.gpu = diagnostics
        .iter()
        .filter(|d| d.path().as_str().starts_with("render/"))
        .filter_map(|d| {
            let ms = d.smoothed()?;
            let (frame_ms, count) = match since {
                // A pass measured nowhere in the window did not run in this step (a
                // skipped cascade, say), and its smoothed time is an earlier step's.
                Some(since) => per_frame(d.measurements().map(|m| (m.time, m.value)), since)?,
                None => (ms, 1),
            };
            Some(GpuDiagnostic { name: d.path().to_string(), ms, frame_ms, count })
        })
        .filter(|d| d.ms > 0.0)
        .collect();
    report.gpu.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    let [extract, assets, views, queue, prepare, graph] = report.render_phase_ms;
    eprintln!(
        "SKATE_PERF step={} fps={:.1} frame_ms_mean={:.3} p99={:.3} main={:.3} draws={} triangles={} \
         sun_cascade_draws={:?} sun_cascade_triangles={:?} sun_cascade_texels={:?}",
        step.as_deref().unwrap_or("launch"),
        report.fps,
        report.frame_ms_mean,
        report.frame_ms_p99,
        report.main_schedule_ms_mean,
        report.draw_calls,
        report.triangles,
        report.sun_cascade_draws,
        report.sun_cascade_triangles,
        report.sun_cascade_texels
    );
    eprintln!(
        "SKATE_PERF_SETUP clocks={} graphics={} render_options={} robust_buffer_access={}",
        report.clocks.as_deref().unwrap_or("unknown"),
        report.graphics_settings.as_deref().unwrap_or("unknown"),
        report.render_options.as_deref().unwrap_or("unknown"),
        std::env::var("BH_ROBUST_BUFFER_ACCESS").as_deref().unwrap_or("default")
    );
    eprintln!(
        "SKATE_PERF_RENDER extract={extract:.3} assets={assets:.3} views={views:.3} \
         queue={queue:.3} prepare={prepare:.3} graph={graph:.3}"
    );
    if let Some(camera) = report.camera {
        eprintln!(
            "SKATE_PERF_CAMERA translation={:?} rotation_xyzw={:?} fov={} aspect={} near={} far={}",
            camera.translation,
            camera.rotation,
            camera.fov,
            camera.aspect_ratio,
            camera.near,
            camera.far
        );
    }
    // Times first and in full: sorting the whole set by value buries
    // sub-millisecond durations under invocation counts in the millions.
    // A pass the graph runs more than once a frame also prints its whole frame's time.
    for d in report.gpu.iter().filter(|d| d.name.ends_with("elapsed_gpu")) {
        if d.count > 1 {
            eprintln!("SKATE_PERF_GPU_MS {} {:.4} frame={:.4} x{}", d.name, d.ms, d.frame_ms, d.count);
        } else {
            eprintln!("SKATE_PERF_GPU_MS {} {:.4}", d.name, d.ms);
        }
    }
    for d in report.gpu.iter().filter(|d| d.name.ends_with("invocations")).take(8) {
        eprintln!("SKATE_PERF_GPU {} {:.0}", d.name, d.ms);
    }
    // The whole frame on the GPU (gpu_frame_time.rs) against the passes' own timers, each a
    // mean over the same recent frames: `untimed` is the frame's work no pass timer covers.
    let window = |path: &bevy::diagnostic::DiagnosticPath| {
        report.gpu.iter().find(|d| d.name == path.as_str()).map(|d| d.frame_ms)
    };
    if let Some(busy) = window(&crate::gpu_frame_time::BUSY) {
        let timed: f64 = report
            .gpu
            .iter()
            .filter(|d| d.name.ends_with("elapsed_gpu"))
            .map(|d| d.frame_ms)
            .sum();
        let wait = window(&crate::gpu_frame_time::WAIT).map_or("-".to_owned(), |w| format!("{w:.3}"));
        eprintln!(
            "SKATE_PERF_GPU_FRAME busy={busy:.3} wait={wait} timed={timed:.3} untimed={:.3}",
            busy - timed
        );
        // Their spread over the same frames: whether the GPU's own work varies (`busy`) or
        // the time between frames does (`wait`, while the CPU or the screen paces them).
        let mut line = String::from("SKATE_PERF_GPU_FRAME_SPREAD");
        for (name, path) in [("busy", &crate::gpu_frame_time::BUSY), ("wait", &crate::gpu_frame_time::WAIT)] {
            let values = diagnostics
                .get(path)
                .into_iter()
                .flat_map(|d| d.measurements())
                .filter(|m| since.is_none_or(|since| m.time >= since))
                .map(|m| m.value);
            if let Some(([p10, p50, p90, most], n)) = spread(values) {
                line += &format!(
                    " {name}_n={n} {name}_p10={p10:.3} {name}_p50={p50:.3} {name}_p90={p90:.3} {name}_max={most:.3}"
                );
            }
        }
        eprintln!("{line}");
    }
    write_report_later(&mut performance, report);
    if !last {
        performance.step += 1;
        performance.samples.clear();
        performance.menu_open_frames = 0;
        performance.sampling = false;
        performance.sampling_started = None;
        performance.pipeline_wait = 0.0;
        performance.pipeline_wait_logged = false;
        return;
    }
    // The game exits after this frame, and the last report must be on the card before then.
    if let Some(writer) = performance.writer.take() {
        let _ = writer.join();
    }
    performance.finished = true;
    exit.write(AppExit::Success);
}

/// Names and writes a step's report, with the CPU profile beside it while the profiler runs,
/// on a thread of its own, once the last step's has finished. On the main thread they held a
/// frame up at each step's end: the name's search stats every earlier step's file on the SD
/// card, about a millisecond apiece, before the write of about 70 KB.
fn write_report_later(performance: &mut Performance, report: Report) {
    if let Some(writer) = performance.writer.take() {
        let _ = writer.join();
    }
    let job = Arc::new(Mutex::new(Some((performance.output.clone(), performance.step, report))));
    let theirs = Arc::clone(&job);
    let spawned = std::thread::Builder::new().name("perf-report".into()).spawn(move || {
        let job = theirs.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some((output, step, report)) = job {
            write_step_report(output, step, report);
        }
    });
    match spawned {
        Ok(writer) => performance.writer = Some(writer),
        Err(error) => {
            warn!("SKATE_PERF couldn't start a thread for the report ({error}); writing it here");
            let job = job.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some((output, step, report)) = job {
                write_step_report(output, step, report);
            }
        }
    }
}

fn write_step_report(
    output: PathBuf,
    step: usize,
    #[cfg_attr(not(target_os = "horizon"), allow(unused_mut))] mut report: Report,
) {
    // Each matrix step after the first takes the next free name beside the first report.
    let output = if step == 0 { output } else { unused_path(output) };
    #[cfg(target_os = "horizon")]
    if bevy_horizon::profiler::running() {
        let path = output.with_extension("cpu.txt");
        match bevy_horizon::profiler::write_report_to(&path) {
            Ok(()) => {
                info!("SKATE_PERF CPU profile written to {path:?}");
                report.cpu_profile = Some(path.to_string_lossy().into_owned());
            }
            Err(error) => error!("SKATE_PERF CPU profile could not be written: {error}"),
        }
    }
    match write_report(&output, &report) {
        Ok(()) => info!("SKATE_PERF_REPORT written to {output:?}"),
        Err(error) => error!("SKATE_PERF_REPORT could not be written: {error}"),
    }
}

/// Puts the current `SKATE_PERF_MATRIX` step's settings in place and restarts its clock. A step
/// without `scale` gets the scale the game launched with.
fn apply_step(
    performance: &mut Performance,
    menu: Option<&mut crate::graphics_menu::Menu>,
    options: Option<&mut crate::render_options::RenderOptions>,
) {
    performance.applied = Some(performance.step);
    if performance.step > 0 {
        performance.step_started = Some(Instant::now());
    }
    let Some(step) = performance.steps.get(performance.step).cloned() else {
        return;
    };
    info!("SKATE_PERF step {} of {}: {}", performance.step + 1, performance.steps.len(), step.label);
    if let Some(options) = options {
        if *options != step.options {
            *options = step.options;
        }
    }
    // Handed to NVK by the render world as its next frame begins (`apply_nvk_switches`).
    *performance.nvk.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(step.nvk);
    // Taken by the render world as its next frame begins (`switch_pass_timers`).
    performance.timers.0.store(step.timers, Ordering::Relaxed);
    if let Some(menu) = menu {
        let launch = *performance.launch_scale.get_or_insert(menu.render_scale());
        let scale = step.scale.unwrap_or(launch);
        if menu.render_scale() != scale && !menu.set_render_scale(scale) {
            warn!("SKATE_PERF_MATRIX: the graphics menu has no {scale}% scale; keeping {}%", menu.render_scale());
        }
    }
}

/// One render diagnostic. `ms` is Bevy's moving average, as earlier reports gave it. A pass the
/// render graph runs more than once a frame (`upscaling`, once a camera) adds a measurement each
/// time, all stamped with the same instant (bevy_render `sync_diagnostics`), and the average
/// gives the later ones no weight: it weighs each by the time since the one before
/// (bevy_diagnostic `Diagnostic::add_measurement`). So `ms` is the first run's time, and
/// `frame_ms` the frame's total over `count` runs.
struct GpuDiagnostic {
    name: String,
    ms: f64,
    frame_ms: f64,
    count: usize,
}

/// The mean per-frame total of the measurements stamped at or after `since`, and the most a
/// frame had, from (stamp, value) pairs oldest first. `None` when none are that recent.
fn per_frame(measurements: impl Iterator<Item = (Instant, f64)>, since: Instant) -> Option<(f64, usize)> {
    let mut frames = 0usize;
    let mut total = 0.0;
    let mut most = 0usize;
    let mut current: Option<(Instant, usize)> = None;
    for (time, value) in measurements.filter(|(time, _)| *time >= since) {
        total += value;
        match &mut current {
            Some((stamp, count)) if *stamp == time => *count += 1,
            _ => {
                frames += 1;
                current = Some((time, 1));
            }
        }
        most = most.max(current.map_or(0, |(_, count)| count));
    }
    (frames > 0).then(|| (total / frames as f64, most))
}

/// The 10th, 50th and 90th percentiles of `values` (nearest rank) and the most, with how many
/// values there were. `None` when there are none.
fn spread(values: impl Iterator<Item = f64>) -> Option<([f64; 4], usize)> {
    let mut values: Vec<f64> = values.collect();
    values.sort_by(f64::total_cmp);
    let last = values.len().checked_sub(1)?;
    let at = |q: f64| values[(last as f64 * q).round() as usize];
    Some(([at(0.1), at(0.5), at(0.9), values[last]], values.len()))
}

struct Report {
    frames: usize,
    fps: f32,
    fps_1_percent_low: f32,
    slowest_1_percent_ms_mean: f32,
    frame_ms_mean: f32,
    frame_ms_median: f32,
    frame_ms_p95: f32,
    frame_ms_p99: f32,
    frame_ms_max: f32,
    frames_over_hitch: usize,
    main_schedule_ms_mean: f32,
    physics_ms_mean: f32,
    draw_calls: u32,
    triangles: u32,
    sun_cascade_draws: [u32; 4],
    sun_cascade_triangles: [u32; 4],
    sun_cascade_texels: [u32; 4],
    /// extract, assets, views, queue, prepare, graph — see `RenderPhases`.
    render_phase_ms: [f32; 6],
    /// Render diagnostics, highest first. Includes GPU pass timings when the
    /// adapter supports timestamp queries.
    gpu: Vec<GpuDiagnostic>,
    samples: Vec<Frame>,
    warmup_seconds: f32,
    /// Seconds past the warmup or settle spent waiting for pipelines (`PendingPipelines`).
    pipeline_wait_seconds: f32,
    sample_seconds: f32,
    /// Sampled frames with the graphics menu open, when gameplay is stopped.
    menu_open_frames: usize,
    /// `SKATE_PERF_FREEZE`: the game clock stood still while this step sampled.
    clock_frozen: bool,
    /// The graphics menu's settings as the window closed (resolution, scale).
    graphics_settings: Option<String>,
    /// The primary window's physical size.
    window: Option<[u32; 2]>,
    /// Where bevy_horizon's CPU profile of the same window was written.
    cpu_profile: Option<String>,
    /// render_options.rs `RenderOptions` as the window closed.
    render_options: Option<String>,
    /// The sphere the world looked up the skater's shadow within as the window closed: its
    /// centre and radius, 0 for everywhere and negative for nowhere (retail_shadow_bounds.rs
    /// `caster_bounds`).
    shadow_casters: Option<[f32; 4]>,
    /// The CPU, GPU and memory clocks as the window closed (Switch only, horizon.rs `clocks`).
    clocks: Option<String>,
    /// The `SKATE_PERF_MATRIX` step measured.
    step: Option<String>,
    /// Whether Bevy's pass timers and pipeline statistics ran: `SKATE_PERF_GPU=1`, in a step
    /// without `timers=0`.
    pass_timers: bool,
    /// The gameplay camera as the window closed.
    camera: Option<CameraPose>,
}

/// Where the gameplay camera stood and looked, and its lens: enough to draw its view again
/// offline.
#[derive(Clone, Copy, Debug)]
struct CameraPose {
    translation: [f32; 3],
    /// x, y, z, w.
    rotation: [f32; 4],
    /// Vertical, in radians.
    fov: f32,
    aspect_ratio: f32,
    near: f32,
    far: f32,
}

fn summarise(samples: &[Frame], draws: &DrawStats) -> Report {
    let frames = samples.len();
    let mut sorted: Vec<f32> = samples.iter().map(|f| f.total_ms).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mean = |values: &[f32]| -> f32 {
        if values.is_empty() { 0.0 } else { values.iter().sum::<f32>() / values.len() as f32 }
    };
    // Nearest-rank percentile. Exact interpolation is not worth it here: the
    // sample count is in the thousands and the budget is stated in whole ms.
    let percentile = |q: f32| -> f32 {
        if sorted.is_empty() {
            return 0.0;
        }
        // Nearest-rank: the smallest sample at or below which `q` of the frames
        // fall, i.e. rank `ceil(q*N)` counting from one. Reporting "95% of frames
        // were at or under this" only means that under this definition.
        let rank = (q * sorted.len() as f32).ceil() as usize;
        sorted[rank.clamp(1, sorted.len()) - 1]
    };
    let frame_ms_mean = mean(&sorted);
    // A 1% low averages the slowest ceil(N / 100) frame times, then takes
    // their reciprocal. It is distinct from the reciprocal of the p99 cutoff.
    let slowest_1_percent_ms_mean = mean(&sorted[frames - frames.div_ceil(100)..]);
    Report {
        frames,
        fps: if frame_ms_mean > 0.0 { 1000.0 / frame_ms_mean } else { 0.0 },
        fps_1_percent_low: if slowest_1_percent_ms_mean > 0.0 {
            1000.0 / slowest_1_percent_ms_mean
        } else { 0.0 },
        slowest_1_percent_ms_mean,
        frame_ms_mean,
        frame_ms_median: percentile(0.50),
        frame_ms_p95: percentile(0.95),
        frame_ms_p99: percentile(0.99),
        frame_ms_max: sorted.last().copied().unwrap_or(0.0),
        frames_over_hitch: samples.iter().filter(|f| f.total_ms > HITCH_MS).count(),
        main_schedule_ms_mean: mean(&samples.iter().map(|f| f.main_ms).collect::<Vec<_>>()),
        physics_ms_mean: mean(&samples.iter().map(|f| f.physics_ms).collect::<Vec<_>>()),
        draw_calls: draws.total_draws(),
        triangles: draws.world_triangles + draws.mod_triangles,
        sun_cascade_draws: draws.sun_cascade_draws,
        sun_cascade_triangles: draws.sun_cascade_triangles,
        sun_cascade_texels: draws.sun_cascade_texels,
        render_phase_ms: [0.0; 6],
        gpu: Vec::new(),
        samples: samples.to_vec(),
        warmup_seconds: WARMUP,
        pipeline_wait_seconds: 0.0,
        sample_seconds: SAMPLE,
        menu_open_frames: 0,
        clock_frozen: false,
        graphics_settings: None,
        window: None,
        cpu_profile: None,
        render_options: None,
        shadow_casters: None,
        clocks: None,
        step: None,
        pass_timers: false,
        camera: None,
    }
}

fn write_report(path: &std::path::Path, report: &Report) -> std::io::Result<()> {
    // Two `json!` objects merged: one object this size passes the macro's recursion limit.
    let mut json = serde_json::json!({
        "frames": report.frames,
        "gpu_queries_enabled": std::env::var("SKATE_PERF_GPU").as_deref() == Ok("1"),
        "render_phase_instrumentation": std::env::var("SKATE_PERF_RENDER").as_deref() == Ok("1"),
        "fps": report.fps,
        "fps_1_percent_low": report.fps_1_percent_low,
        "slowest_1_percent_ms_mean": report.slowest_1_percent_ms_mean,
        "frame_ms_mean": report.frame_ms_mean,
        "frame_ms_median": report.frame_ms_median,
        "frame_ms_p95": report.frame_ms_p95,
        "frame_ms_p99": report.frame_ms_p99,
        "frame_ms_max": report.frame_ms_max,
        "frames_over_8ms": report.frames_over_hitch,
        "main_schedule_ms_mean": report.main_schedule_ms_mean,
        "physics_ms_mean": report.physics_ms_mean,
        "draw_calls": report.draw_calls,
        "triangles": report.triangles,
        "sun_cascade_draws": report.sun_cascade_draws,
        "sun_cascade_triangles": report.sun_cascade_triangles,
        "sun_cascade_texels": report.sun_cascade_texels,
        "render_extract_ms_mean": report.render_phase_ms[0],
        "render_assets_ms_mean": report.render_phase_ms[1],
        "render_views_ms_mean": report.render_phase_ms[2],
        "render_queue_ms_mean": report.render_phase_ms[3],
        "render_prepare_ms_mean": report.render_phase_ms[4],
        "render_graph_ms_mean": report.render_phase_ms[5],
    });
    let context = serde_json::json!({
        "render_diagnostics": report.gpu.iter()
            .map(|d| serde_json::json!({
                "name": d.name,
                "ms": d.ms,
                "frame_ms": d.frame_ms,
                "per_frame": d.count,
            }))
            .collect::<Vec<_>>(),
        "warmup_seconds": report.warmup_seconds,
        "pipeline_wait_seconds": report.pipeline_wait_seconds,
        "sample_seconds": report.sample_seconds,
        "frames_with_menu_open": report.menu_open_frames,
        "clock_frozen": report.clock_frozen,
        "graphics_settings": report.graphics_settings,
        "window_physical_size": report.window,
        "cpu_profile": report.cpu_profile,
        "matrix_step": report.step,
        "pass_timers": report.pass_timers,
        "camera": report.camera.map(|camera| serde_json::json!({
            "translation": camera.translation,
            "rotation_xyzw": camera.rotation,
            "fov_y_radians": camera.fov,
            "aspect_ratio": camera.aspect_ratio,
            "near": camera.near,
            "far": camera.far,
        })),
        "render_options": report.render_options,
        "shadow_casters": report.shadow_casters,
        "robust_buffer_access": std::env::var("BH_ROBUST_BUFFER_ACCESS").ok(),
        "clocks": report.clocks,
        "samples": report.samples.iter().map(|f| serde_json::json!({
            "frame_ms": f.total_ms,
            "main_ms": f.main_ms,
            "physics_ms": f.physics_ms,
            "draws": f.draws,
            "triangles": f.triangles,
        })).collect::<Vec<_>>(),
    });
    if let (Some(map), serde_json::Value::Object(context)) = (json.as_object_mut(), context) {
        map.extend(context);
    }
    std::fs::write(path, serde_json::to_vec_pretty(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(total_ms: f32) -> Frame {
        Frame { total_ms, ..default() }
    }

    #[test]
    fn per_frame_totals_the_passes_a_frame_runs_more_than_once() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        // Before `since`, then three frames: two runs, one, two.
        let measurements = [(at(0), 9.0), (at(10), 1.0), (at(10), 2.0), (at(20), 4.0), (at(30), 0.5), (at(30), 0.5)];
        let (mean, most) = per_frame(measurements.into_iter(), at(5)).unwrap();
        assert!((mean - 8.0 / 3.0).abs() < 1e-9);
        assert_eq!(most, 2);
        assert_eq!(per_frame(measurements.into_iter(), at(31)), None);
        assert_eq!(per_frame([(at(1), 3.0)].into_iter(), at(0)), Some((3.0, 1)));
    }

    #[test]
    fn spread_takes_nearest_ranks_in_any_order() {
        // 0..=10 shuffled: ranks 1, 5 and 9 of 0-10.
        let values = [7.0, 3.0, 10.0, 0.0, 5.0, 9.0, 1.0, 8.0, 2.0, 6.0, 4.0];
        assert_eq!(spread(values.into_iter()), Some(([1.0, 5.0, 9.0, 10.0], 11)));
        assert_eq!(spread([2.5].into_iter()), Some(([2.5; 4], 1)));
        assert_eq!(spread(std::iter::empty()), None);
    }

    #[test]
    fn matrix_steps_start_from_the_launch_settings() {
        use crate::render_options::RenderOptions;
        use bevy::light::ShadowFilteringMethod;
        let launch = |name: &str| (name == "SKATE_SUN_CASCADES").then(|| "2".to_string());
        let steps = parse_matrix(
            " base ; prepass=1, scale=50 ;cascades=1,distance=40;filter=hardware;bogus=3;;\
             world_shadows=0;shadow_bounds=0;sun_receivers=0",
            launch,
        );
        let labels: Vec<_> = steps.iter().map(|step| step.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "base",
                "prepass=1, scale=50",
                "cascades=1,distance=40",
                "filter=hardware",
                "bogus=3",
                "world_shadows=0",
                "shadow_bounds=0",
                "sun_receivers=0",
            ]
        );
        let base = RenderOptions { sun_cascades: 2, ..RenderOptions::default() };
        assert_eq!(steps[0].options, base);
        assert_eq!(steps[0].scale, None);
        assert_eq!(steps[1].options, RenderOptions { depth_prepass: true, ..base });
        assert_eq!(steps[1].scale, Some(50));
        assert_eq!(
            steps[2].options,
            RenderOptions { sun_cascades: 1, sun_shadow_distance: 40., ..base }
        );
        assert_eq!(
            steps[3].options,
            RenderOptions { shadow_filter: ShadowFilteringMethod::Hardware2x2, ..base }
        );
        // An unknown key measures the launch settings.
        assert_eq!(steps[4].options, base);
        assert_eq!(steps[5].options, RenderOptions { world_shadows: false, ..base });
        assert_eq!(steps[6].options, RenderOptions { shadow_bounds: false, ..base });
        assert_eq!(steps[7].options, RenderOptions { sun_receivers: false, ..base });
        assert!(parse_matrix("", launch).is_empty());
    }

    #[test]
    fn a_matrix_step_can_stop_the_pass_timers() {
        let steps = parse_matrix("base;timers=0;timers=1,vcull=1;timers=off", |_| None);
        let timers: Vec<_> = steps.iter().map(|step| step.timers).collect();
        assert_eq!(timers, [true, false, true, true]);
        // The timers alone change no render setting.
        assert_eq!(steps[1].options, steps[0].options);
        assert_ne!(steps[2].options, steps[0].options);
    }

    #[test]
    fn occlusion_is_a_matrix_setting() {
        let steps = parse_matrix(
            "base;occlusion=1;occlusion=1,span=8,pyramid=1,ptile=1;obatch=0",
            |_| None,
        );
        assert!(!steps[0].options.occlusion);
        assert!(steps[1].options.occlusion);
        assert_eq!(steps[1].options.occlusion_span, 0);
        assert!(!steps[1].options.pyramid_once);
        assert!(!steps[1].options.pyramid_tile);
        assert_eq!(steps[2].options.occlusion_span, 8);
        assert!(steps[2].options.pyramid_once);
        assert!(steps[2].options.pyramid_tile);
        assert!(steps[2].options.occlusion_batch);
        assert_eq!(
            steps[3].options,
            crate::render_options::RenderOptions { occlusion_batch: false, ..steps[0].options }
        );
    }

    #[test]
    fn preprocessing_dispatch_is_a_matrix_setting() {
        let launch =
            |name: &str| (name == "SKATE_LATE_PREPROCESS_DIRECT").then(|| "0".to_string());
        let steps =
            parse_matrix("base;ldirect=1;ipf=0;ldirect=1,ipf=1;ldirect=0,ipf=0", launch);
        assert!(!steps[0].options.late_preprocess_direct);
        assert!(steps[0].options.indirect_phase_filter);
        assert_eq!(
            steps[1].options,
            crate::render_options::RenderOptions {
                late_preprocess_direct: true,
                ..steps[0].options
            }
        );
        assert_eq!(
            steps[2].options,
            crate::render_options::RenderOptions {
                indirect_phase_filter: false,
                ..steps[0].options
            }
        );
        assert!(steps[3].options.late_preprocess_direct);
        assert!(steps[3].options.indirect_phase_filter);
        assert!(!steps[4].options.late_preprocess_direct);
        assert!(!steps[4].options.indirect_phase_filter);
    }

    #[test]
    fn pyramid_block_is_a_matrix_setting() {
        let steps = parse_matrix("base;pblock=1;ldirect=1,ipf=1,pblock=1", |_| None);
        assert!(!steps[0].options.pyramid_block);
        assert_eq!(
            steps[1].options,
            crate::render_options::RenderOptions {
                pyramid_block: true,
                ..steps[0].options
            }
        );
        assert!(steps[2].options.pyramid_block);
        assert!(steps[2].options.late_preprocess_direct);
        assert!(steps[2].options.indirect_phase_filter);
    }

    #[test]
    fn sun_scissor_is_a_matrix_setting() {
        let steps = parse_matrix("base;ssc=0;ssc=1", |_| None);
        assert!(steps[0].options.sun_scissor);
        assert_eq!(
            steps[1].options,
            crate::render_options::RenderOptions { sun_scissor: false, ..steps[0].options }
        );
        assert_eq!(steps[2].options, steps[0].options);
        let launch = |name: &str| (name == "SKATE_SUN_SCISSOR").then(|| "0".to_string());
        let steps = parse_matrix("base;ssc=1", launch);
        assert!(!steps[0].options.sun_scissor);
        assert!(steps[1].options.sun_scissor);
    }

    #[test]
    fn page_uniform_is_a_matrix_setting() {
        let launch = |name: &str| (name == "SKATE_WORLD_PAGE_UNIFORM").then(|| "1".to_string());
        let steps = parse_matrix("base;pageu=0;pageu=1,pprobe=3", launch);
        assert!(steps[0].options.world.page_uniform);
        assert!(!steps[1].options.world.page_uniform);
        assert!(steps[2].options.world.page_uniform);
        assert_eq!(steps[2].options.world.prepass_probe, 3);
    }

    #[test]
    fn nvk_switches_are_matrix_settings() {
        let steps = parse_matrix(
            "base;ezh=31;zcull=0,fez=1;zcull=1,fez=0,zser=49;ezh=x;zdir=1;zdir=0;dskip=0;dskip=1",
            |_| None,
        );
        let nvk: Vec<[i32; 6]> = steps.iter().map(|step| step.nvk).collect();
        assert_eq!(
            nvk,
            [
                [-1, -1, -1, -1, 1, -1],
                [31, -1, -1, -1, 1, -1],
                [-1, 0, -1, 1, 1, -1],
                [-1, -1, 49, -1, 1, -1],
                [-1, -1, -1, -1, 1, -1],
                [-1, -1, -1, -1, 1, -1],
                [-1, -1, -1, -1, 0, -1],
                [-1, -1, -1, -1, 1, 0],
                [-1, -1, -1, -1, 1, -1],
            ]
        );
        assert_eq!(steps[1].options, steps[0].options);
    }

    #[test]
    fn zcull_direction_starts_from_the_launch_one() {
        let launch = |name: &str| (name == "BEVY_HORIZON_ZCULL_DIR").then(|| " greater ".to_string());
        let zdir: Vec<i32> = parse_matrix("base;zdir=0;zcull=0", launch).iter().map(|step| step.nvk[4]).collect();
        assert_eq!(zdir, [1, 0, 1]);
        let launch = |name: &str| (name == "BEVY_HORIZON_ZCULL_DIR").then(|| "less".to_string());
        assert_eq!(parse_matrix("base", launch)[0].nvk[4], 0);
        assert_eq!(parse_matrix("base", |_| None)[0].nvk[4], 1);
    }

    #[test]
    fn features_is_a_matrix_setting() {
        let steps = parse_matrix("base;features=1;pageu=1,features=1;base", |_| None);
        let features: Vec<bool> = steps.iter().map(|step| step.options.world.features).collect();
        assert_eq!(features, [false, true, true, false]);
        assert!(steps[2].options.world.page_uniform);
    }

    #[test]
    fn upscale_is_a_matrix_setting() {
        use crate::render_options::{RenderOptions, Upscale};
        let launch = |name: &str| (name == "SKATE_UPSCALE").then(|| "bilinear".to_string());
        let steps = parse_matrix("base;scale=75,upscale=rcas;scale=75,upscale=EASU;base", launch);
        let upscales: Vec<Upscale> = steps.iter().map(|step| step.options.upscale).collect();
        assert_eq!(upscales, [Upscale::Bilinear, Upscale::Rcas, Upscale::Easu, Upscale::Bilinear]);
        assert_eq!(steps[1].scale, Some(75));
        assert_eq!(RenderOptions::parse(|_| None).upscale, Upscale::Rcas);
    }

    #[test]
    fn present_to_window_is_a_matrix_setting() {
        let steps = parse_matrix("scale=75;scale=75,pwin=0;base", |_| None);
        let to_window: Vec<bool> =
            steps.iter().map(|step| step.options.present_to_window).collect();
        assert_eq!(to_window, [true, false, true]);
    }

    #[test]
    fn slot_waterfall_is_a_matrix_setting() {
        let launch = |name: &str| (name == "SKATE_WORLD_FEATURES").then(|| "1".to_string());
        let steps = parse_matrix("base;wfall=1;base;wfall=1,pageu=1;wfall=0", launch);
        let waterfall: Vec<bool> =
            steps.iter().map(|step| step.options.world.slot_waterfall).collect();
        assert_eq!(waterfall, [false, true, false, true, false]);
        assert!(steps.iter().all(|step| step.options.world.features));
        assert!(steps[3].options.world.page_uniform);
        assert_eq!(steps[2].options, steps[0].options);
    }

    #[test]
    fn tone_direct_is_a_matrix_setting() {
        let steps = parse_matrix("base;tdirect=0;base;tdirect=0,direct=0", |_| None);
        let direct: Vec<bool> = steps.iter().map(|step| step.options.tone_direct).collect();
        assert_eq!(direct, [true, false, true, false]);
        assert!(!steps[3].options.present_direct);
        assert_eq!(steps[2].options, steps[0].options);
    }

    #[test]
    fn tone_gamma_and_gpr48_are_matrix_settings() {
        let steps = parse_matrix("base;gpr48=1;tgamma=0;base", |_| None);
        let gamma: Vec<bool> = steps.iter().map(|step| step.options.tone_gamma).collect();
        let gpr48: Vec<bool> = steps.iter().map(|step| step.options.world.gpr48).collect();
        assert_eq!(gamma, [true, true, false, true]);
        assert_eq!(gpr48, [false, true, false, false]);
        assert_eq!(steps[3].options, steps[0].options);
    }

    #[test]
    fn cutout_flat_and_the_sixth_prepass_probe_are_matrix_settings() {
        let steps = parse_matrix("base;cflat=0;pprobe=6;cflat=0,pprobe=2;base", |_| None);
        let flat: Vec<bool> = steps.iter().map(|step| step.options.world.cutout_flat).collect();
        let probe: Vec<u8> = steps.iter().map(|step| step.options.world.prepass_probe).collect();
        assert_eq!(flat, [true, false, true, false, true]);
        assert_eq!(probe, [0, 0, 6, 2, 0]);
        assert_eq!(steps[4].options, steps[0].options);
    }

    #[test]
    fn short_lives_is_a_matrix_setting() {
        let steps = parse_matrix("base;slives=1;base;slives=1", |_| None);
        let short: Vec<bool> = steps.iter().map(|step| step.options.world.short_lives).collect();
        assert_eq!(short, [false, true, false, true]);
        assert_eq!(steps[2].options, steps[0].options);
        assert_eq!(steps[3].options, steps[1].options);
    }

    #[test]
    fn lean_and_implicit_lod_are_matrix_settings() {
        let steps = parse_matrix("base;lean=1;lean=1,imp=1;base;imp=1", |_| None);
        let lean: Vec<bool> = steps.iter().map(|step| step.options.world.lean).collect();
        let imp: Vec<bool> = steps.iter().map(|step| step.options.world.implicit_lod).collect();
        assert_eq!(lean, [false, true, true, false, false]);
        assert_eq!(imp, [false, false, true, false, true]);
        assert_eq!(steps[3].options, steps[0].options);
    }

    #[test]
    fn marking_after_the_alpha_test_is_a_matrix_setting() {
        let steps = parse_matrix("vcull=1;vcull=1,vafter=1;vcull=2,vafter=1;base", |_| None);
        let after: Vec<bool> =
            steps.iter().map(|step| step.options.world.mark_after_test).collect();
        assert_eq!(after, [false, true, false, false]);
        assert_eq!(steps[1].options.world.vis_cull, 1);
    }

    #[test]
    fn a_held_camera_is_parsed() {
        let pose = parse_camera(" -171.5, 186.5,-702.25, 0, 0.6, 0, 0.8 ").unwrap();
        assert_eq!(pose.translation, Vec3::new(-171.5, 186.5, -702.25));
        assert!((pose.rotation.length() - 1.0).abs() < 1e-6);
        assert!(parse_camera("1,2,3,0,0,0,2").is_some_and(|pose| pose.rotation == Quat::IDENTITY));
        assert!(parse_camera("1,2,3,0,0,0,1,5").is_none());
        assert!(parse_camera("1,2,3,0,0,0").is_none());
        assert!(parse_camera("1,2,3,0,0,0,0").is_none());
        assert!(parse_camera("1,2,x,0,0,0,1").is_none());
        assert!(parse_camera("1,2,inf,0,0,0,1").is_none());
    }

    #[test]
    fn percentiles_follow_sample_order() {
        // Values 1..=100, so the Nth smallest sample is exactly N ms and every
        // rank is readable directly.
        let samples: Vec<Frame> = (1..=100).map(|ms| frame(ms as f32)).collect();
        let report = summarise(&samples, &DrawStats::default());
        assert_eq!(report.frames, 100);
        assert_eq!(report.frame_ms_median, 50.0);
        assert_eq!(report.frame_ms_p95, 95.0);
        assert_eq!(report.frame_ms_p99, 99.0);
        assert_eq!(report.frame_ms_max, 100.0);
        // A single sample is every percentile of itself.
        let one = summarise(&[frame(7.0)], &DrawStats::default());
        assert_eq!(
            (one.frame_ms_median, one.frame_ms_p95, one.frame_ms_p99),
            (7.0, 7.0, 7.0)
        );
    }

    #[test]
    fn fps_is_the_reciprocal_of_mean_frame_time() {
        // 300 FPS is the contract; 3.333 ms per frame must read back as ~300.
        let samples = vec![frame(10.0 / 3.0); 64];
        let report = summarise(&samples, &DrawStats::default());
        assert!((report.fps - 300.0).abs() < 1.0, "fps={}", report.fps);
    }

    #[test]
    fn hitches_are_counted_against_the_8ms_threshold() {
        let samples = vec![frame(3.0), frame(9.0), frame(8.0), frame(20.0)];
        let report = summarise(&samples, &DrawStats::default());
        assert_eq!(report.frames_over_hitch, 2);
    }

    #[test]
    fn earlier_reports_are_kept() {
        let dir = std::env::temp_dir().join(format!("skate-perf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("perf.json");
        assert_eq!(unused_path(first.clone()), first);
        std::fs::write(&first, "{}").unwrap();
        assert_eq!(unused_path(first.clone()), dir.join("perf-2.json"));
        std::fs::write(dir.join("perf-2.json"), "{}").unwrap();
        let third = unused_path(first.clone());
        assert_eq!(third, dir.join("perf-3.json"));
        // The CPU profile beside it.
        assert_eq!(third.with_extension("cpu.txt"), dir.join("perf-3.cpu.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_sample_set_does_not_panic() {
        let report = summarise(&[], &DrawStats::default());
        assert_eq!(report.frames, 0);
        assert_eq!(report.fps, 0.0);
        assert_eq!(report.fps_1_percent_low, 0.0);
    }

    #[test]
    fn one_percent_low_averages_the_tail_including_fractional_sample_counts() {
        let mut samples = vec![frame(2.0); 199];
        samples.extend([frame(10.0), frame(18.0)]);
        let report = summarise(&samples, &DrawStats::default());
        assert_eq!(report.frame_ms_p99, 2.0);
        assert_eq!(report.slowest_1_percent_ms_mean, 10.0);
        assert_eq!(report.fps_1_percent_low, 100.0);
        let one = summarise(&[frame(8.0)], &DrawStats::default());
        assert_eq!(one.fps_1_percent_low, 125.0);
    }

    #[test]
    fn draw_stats_count_active_3d_cameras_and_only_their_cascades() {
        use bevy::camera::visibility::{
            CascadesVisibleEntities, VisibleEntities, VisibleMeshEntities,
        };
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.init_resource::<DrawStats>();
        let [a, b, c] =
            [10, 20, 40].map(|n| world.spawn(crate::skate_world::LeafTriangles(n)).id());
        let mesh = std::any::TypeId::of::<Mesh3d>();
        let camera = |world: &mut World, is_active: bool, is_3d: bool, sees: &[Entity]| {
            let mut visible = VisibleEntities::default();
            for &leaf in sees {
                visible.push(leaf, mesh);
            }
            let camera = Camera { is_active, ..default() };
            match is_3d {
                true => world.spawn((Camera3d::default(), camera, visible)).id(),
                false => world.spawn((Camera2d, camera, visible)).id(),
            }
        };
        let main = camera(&mut world, true, true, &[a, b]);
        let parked = camera(&mut world, false, true, &[c]);
        // Bevy builds cascades for an active 2D camera too, but never renders them.
        let hud = camera(&mut world, true, false, &[a, b, c]);
        let cascades = |lists: &[&[Entity]]| -> Vec<VisibleMeshEntities> {
            lists.iter().map(|list| VisibleMeshEntities { entities: list.to_vec() }).collect()
        };
        let mut sun = CascadesVisibleEntities::default();
        sun.entities.insert(main, cascades(&[&[a], &[a, c], &[]]));
        sun.entities.insert(parked, cascades(&[&[b], &[b]]));
        sun.entities.insert(hud, cascades(&[&[a, b, c], &[a, b, c]]));
        let sun = world.spawn((crate::render_options::SceneSun, sun)).id();
        world.run_system_once(world_draw_stats).unwrap();
        let stats = *world.resource::<DrawStats>();
        assert_eq!((stats.world_draws, stats.world_triangles), (2, 30));
        assert_eq!(stats.sun_cascade_draws, [1, 2, 0, 0]);
        assert_eq!(stats.sun_cascade_triangles, [10, 50, 0, 0]);
        assert_eq!(stats.sun_cascade_texels, [0; 4], "no map, no texels");
        // A rectangle counts where it lies on the map, and the cascades without one draw whole.
        world.insert_resource(bevy::light::DirectionalLightShadowMap { size: 2048 });
        let mut scissors = bevy::pbr::ShadowScissors { size: 2048, ..default() };
        let key = |i| crate::retail_shadow_bounds::cascade_key(sun, main, i);
        scissors.rects.insert(key(1), URect::new(10, 20, 110, 70));
        scissors.rects.insert(key(2), URect::new(0, 0, 2048, 2048));
        world.insert_resource(scissors.clone());
        world.run_system_once(world_draw_stats).unwrap();
        let whole = 2048 * 2048;
        assert_eq!(world.resource::<DrawStats>().sun_cascade_texels, [whole, 5000, 0, 0]);
        scissors.rects.insert(key(1), URect::new(2000, 2040, 2100, 2100));
        world.insert_resource(scissors.clone());
        world.run_system_once(world_draw_stats).unwrap();
        assert_eq!(world.resource::<DrawStats>().sun_cascade_texels, [whole, 48 * 8, 0, 0]);
        // Rectangles for another size of map are passed over, as the shadow pass does.
        world.insert_resource(bevy::pbr::ShadowScissors { size: 1024, ..scissors });
        world.run_system_once(world_draw_stats).unwrap();
        assert_eq!(world.resource::<DrawStats>().sun_cascade_texels, [whole, whole, 0, 0]);
    }
}
