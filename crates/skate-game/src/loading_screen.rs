//! The loading screen, laid out like Hails's start-up splash on her `world/living-world` branch
//! (splash.rs): "SKATE 3 RUST ENGINE", a side-view skateboard rolling across, the step that is
//! loading with the seconds since the load began, and the last four finished steps dimmed below.
//! Here it is twice her splash's size, in the middle of the screen.
//!
//! Bevy's UI draws it over the start map's load, which on the Switch runs in the background, and
//! over every map change. Hidden, it leaves the layout too (`Display::None`), so once a world is
//! up it costs the game nothing. `SKATE_EARLY_SPLASH=1` also puts it up from the first moment of
//! `main` ([`show_early_splash`]), through a libnx framebuffer; off by default, as the game then ran
//! at 20 fps in game where it ran at 33.7 without it (findings 59 §14, §16). The screen stays up
//! until the published world is on the GPU, so nothing pops in once it goes: every mesh, image and
//! storage buffer the scene published is ready in the render world (meshes bevy_horizon holds back
//! for later frames aren't), and no shader pipeline is still compiling, for a few frames in a row
//! (findings 59).
//!
//! Once a world is committed it runs behind the screen, so its first frames render and their
//! pipelines compile, but the player's input doesn't reach it (`input::publish_actions`). The
//! render-side check runs only while the screen waits for a world.
//!
//! The layout and the board are Hails's (splash.rs `paint` and `draw_board`), redrawn without GDI
//! so they work on the Switch. Both renderers write in Bevy's built-in Fira Mono.
#[cfg(any(target_os = "horizon", test))]
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use bevy::{
    asset::{AssetId, RenderAssetUsages},
    image::ImageSampler,
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        mesh::RenderMesh,
        render_asset::RenderAssets,
        render_resource::{Extent3d, PipelineCache, TextureDimension, TextureFormat},
        storage::{GpuShaderStorageBuffer, ShaderStorageBuffer},
        texture::GpuImage,
    },
    text::{LineBreak, TextLayout},
    window::PrimaryWindow,
};
use std::sync::{Arc, Mutex, PoisonError, atomic::{AtomicU64, Ordering}};
use std::time::{Duration, Instant};

/// Frames in a row the world must be all on the GPU before the screen goes: a mesh that has just
/// arrived asks for its pipelines a frame or two later.
const READY_FRAMES: u8 = 5;
/// The longest the screen waits for a published world before it gives way.
const MAX_WAIT: Duration = Duration::from_secs(90);

/// Hails's splash, 560 × 240 of its pixels, drawn at `SCALE` times that in the middle of a
/// 1280 × 720 screen (the early splash's framebuffer, and Bevy's UI in proportion): the screen is
/// `SCREEN` splash pixels, and her box sits at `PANEL` on it.
const SPLASH_WIDTH: f32 = 560.;
const SPLASH_HEIGHT: f32 = 240.;
const SCALE: f32 = BOARD_SCALE as f32;
const SCREEN_WIDTH: u32 = 1280;
const SCREEN_HEIGHT: u32 = 720;
const SCREEN: (f32, f32) = (SCREEN_WIDTH as f32 / SCALE, SCREEN_HEIGHT as f32 / SCALE);
const PANEL: (f32, f32) = ((SCREEN.0 - SPLASH_WIDTH) / 2., (SCREEN.1 - SPLASH_HEIGHT) / 2.);
/// Where her `paint` puts each part, in splash pixels: the title's top, the board's, the status
/// line's and the first finished line's, the finished lines' spacing, and the lines' side margin.
const TITLE_TOP: f32 = 16.;
const BOARD_TOP: f32 = 52.;
const LINE_TOP: f32 = 122.;
const HISTORY_TOP: f32 = 152.;
const HISTORY_SPACING: f32 = 19.;
const LINE_MARGIN: f32 = 16.;
/// Her font sizes, in splash pixels: the title, the status line, the finished lines.
const TITLE_SIZE: f32 = 13.;
const LINE_SIZE: f32 = 16.;
const HISTORY_SIZE: f32 = 13.;
const HISTORY_LINES: usize = 4;
/// Splash pixels a second the board rolls.
const ROLL_SPEED: f32 = 170.;
const TITLE: &str = "SKATE 3 RUST ENGINE";
/// Fira Mono's advance, in ems: every glyph is 600 of its 1000 units wide.
const ADVANCE: f32 = 0.6;

const BACKGROUND: [u8; 3] = [14, 14, 17];
const TITLE_COLOUR: [u8; 3] = [160, 160, 165];
const LINE_COLOUR: [u8; 3] = [217, 217, 217];
const HISTORY_COLOUR: [u8; 3] = [110, 110, 115];

const fn colour([r, g, b]: [u8; 3]) -> Color {
    Color::srgb_u8(r, g, b)
}

pub(crate) struct LoadingScreenPlugin;

impl Plugin for LoadingScreenPlugin {
    fn build(&self, app: &mut App) {
        // A `--verify` capture has to show the world, not this.
        let enabled = app.world().resource::<crate::config::Config>().verification_capture.is_none();
        let readiness = Readiness::default();
        app.insert_resource(LoadingScreen::new(enabled, readiness.clone()))
            .add_systems(Startup, spawn_screen)
            .add_systems(PreUpdate, update_screen.after(crate::map_transition::MapTransitionSet))
            .add_systems(Update, roll_board);
        #[cfg(target_os = "horizon")]
        app.add_systems(PreUpdate, release_splash.after(update_screen)).add_systems(Last, ease_off_while_held);
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            // After `RenderSystems::Render`, where the pipeline cache works through its queue.
            render.insert_resource(readiness).add_systems(
                Render,
                check_world.after(RenderSystems::Render).before(RenderSystems::Cleanup),
            );
        }
    }
}

/// What the screen says: the step that is loading, the finished ones (oldest first) and when
/// the steps began. One copy, shared by the early splash's thread and Bevy's UI.
struct Steps {
    current: String,
    finished: Vec<String>,
    started: Instant,
}

impl Steps {
    fn new(started: Instant) -> Self {
        Self { current: String::new(), finished: Vec::new(), started }
    }

    /// A new step ("Loading physics"); the current one moves to the finished lines.
    fn step(&mut self, label: &str) {
        if self.current == label {
            return;
        }
        let previous = std::mem::replace(&mut self.current, label.to_owned());
        if !previous.is_empty() {
            self.finished.push(previous);
            let excess = self.finished.len().saturating_sub(HISTORY_LINES);
            self.finished.drain(..excess);
        }
    }

    /// The status line as Hails's splash writes it, and the finished lines, newest first.
    fn lines(&self, now: Instant) -> (String, Vec<String>) {
        let current = if self.current.is_empty() { "Starting" } else { self.current.as_str() };
        let seconds = now.saturating_duration_since(self.started).as_secs();
        (format!("{current}  ({seconds} s)"), self.finished.iter().rev().cloned().collect())
    }
}

static STEPS: Mutex<Option<Steps>> = Mutex::new(None);

/// A new step for the loading screen; the first one starts its clock.
pub(crate) fn step(label: &str) {
    let mut steps = STEPS.lock().unwrap_or_else(PoisonError::into_inner);
    steps.get_or_insert_with(|| Steps::new(Instant::now())).step(label);
}

/// `main`'s boot stage (`report_stage`) as a step.
pub(crate) fn boot_stage(stage: &str) {
    step(match stage {
        "configuration_and_installation" => "Reading the settings",
        "gameplay_configuration" => "Loading the gameplay settings",
        "asset_manifest" => "Reading the asset manifest",
        "stock_graphs" => "Loading the animation graphs",
        "map_validation" => "Checking the map",
        "physics_initialization" => "Loading physics",
        "skater_initialization" => "Loading the skater",
        "controls_initialization" => "Loading the controls",
        "renderer_and_app_initialization" => "Starting the renderer",
        "app_run" => "Starting the game",
        other => other,
    });
}

fn current_lines() -> (String, Vec<String>) {
    let steps = STEPS.lock().unwrap_or_else(PoisonError::into_inner);
    steps.as_ref().map_or_else(|| ("Starting  (0 s)".to_owned(), Vec::new()), |s| s.lines(Instant::now()))
}

/// The steps of a map change after the start, from now.
fn restart_steps() {
    *STEPS.lock().unwrap_or_else(PoisonError::into_inner) = Some(Steps::new(Instant::now()));
}

/// `text` cut to what fits between the lines' margins at `size`, ending "..." when cut.
fn fit(text: &str, size: f32) -> String {
    // 528 / 9.6 is 54.99… in f32.
    let room = ((SPLASH_WIDTH - 2. * LINE_MARGIN) / (size * ADVANCE) + 1e-3) as usize;
    if text.chars().count() <= room {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(room.saturating_sub(3)).collect();
    cut.push_str("...");
    cut
}

/// The board image's left edge, in splash pixels from the screen's left edge, `seconds` after the
/// launch: it rolls left to right across the whole screen at Hails's speed and comes round again.
/// Across the screen, not her box: on a full screen her box's edges are nowhere, and the board
/// vanished at them. From the launch, so the board carries on where the early splash left it.
fn board_left(seconds: f32) -> f32 {
    let width = BOARD_LENGTH + 2. * BOARD_PAD;
    (seconds * ROLL_SPEED) % (SCREEN.0 + width) - width
}

fn since_launch() -> f32 {
    crate::since_launch_ms() as f32 / 1000.
}

/// With `SKATE_EARLY_SPLASH=1`, puts the loading screen up from the first moment of the start,
/// before the window exists (bevy_horizon's splash, a libnx framebuffer). With the start map
/// loading in the background the splash holds the screen until that world is on the GPU
/// (`release_splash` lets go), drawn on a thread of its own however busy the game is. Off by
/// default: after the splash the game ran at 20 fps in game, with the splash's pixel format put
/// back and with three buffers, where it ran at 33.7 without it (findings 59 §14, §16).
#[cfg(target_os = "horizon")]
pub(crate) fn show_early_splash() {
    if !std::env::var("SKATE_EARLY_SPLASH").is_ok_and(|v| v.trim() == "1") {
        return;
    }
    let started = Instant::now();
    let mut drawing = SplashDrawing::new();
    let shown = bevy_horizon::splash::show(SCREEN_WIDTH, SCREEN_HEIGHT, Duration::from_millis(33), move |frame| {
        let mut painter = Painter { pixels: frame.pixels, width: frame.width as usize, height: frame.height as usize, stride: frame.stride };
        let (line, history) = current_lines();
        drawing.draw(&mut painter, since_launch(), &line, &history);
    });
    let held = shown && crate::config::load_start_map_in_background();
    bevy_horizon::splash::hold(held);
    eprintln!(
        "[Skate] start-up splash {}{} ({} ms to set up)",
        if shown { "up" } else { "unavailable" },
        if held { ", holding the screen until the start map is on the GPU" } else { "" },
        started.elapsed().as_millis(),
    );
}

/// The early splash's frames: Hails's layout at `SCALE`, in the middle of the frame. The
/// framebuffer keeps what was drawn, so after the first frame only the board's band is drawn
/// again, and the lines when their text changes: a full frame each time made the board stutter.
#[cfg(any(target_os = "horizon", test))]
struct SplashDrawing {
    font: Option<SplashFont>,
    /// The board over the background, so drawing it is a copy.
    board: Rgba,
    /// The lines drawn last; `None` before the first frame.
    drawn: Option<(String, Vec<String>)>,
}

#[cfg(any(target_os = "horizon", test))]
impl SplashDrawing {
    fn new() -> Self {
        Self { font: SplashFont::new(), board: board_pixels().over(BACKGROUND), drawn: None }
    }

    fn draw(&mut self, painter: &mut Painter<'_>, seconds: f32, line: &str, history: &[String]) {
        let origin = ((painter.width as f32 - SCREEN.0 * SCALE) / 2., (painter.height as f32 - SCREEN.1 * SCALE) / 2.);
        let x = |splash: f32| origin.0 + splash * SCALE;
        let y = |splash: f32| origin.1 + splash * SCALE;
        let screen = (0, painter.width as i32);
        let margins = (x(PANEL.0 + LINE_MARGIN) as i32, x(PANEL.0 + SPLASH_WIDTH - LINE_MARGIN) as i32);
        let centre = x(SCREEN.0 / 2.);
        if self.drawn.is_none() {
            painter.fill_rows(0, painter.height, BACKGROUND);
            if let Some(font) = &self.font {
                font.line(painter, TITLE, TITLE_SIZE * SCALE, centre, y(PANEL.1 + TITLE_TOP), TITLE_COLOUR, screen);
            }
        }
        let top = y(PANEL.1 + BOARD_TOP).round() as usize;
        painter.fill_rows(top, top + self.board.height, BACKGROUND);
        painter.copy(x(board_left(seconds)).round() as i32, top, &self.board);
        if self.drawn.as_ref().is_none_or(|(drawn, finished)| drawn != line || finished[..] != *history) {
            let bottom = y(PANEL.1 + HISTORY_TOP + HISTORY_LINES as f32 * HISTORY_SPACING) as usize;
            painter.fill_rows(y(PANEL.1 + LINE_TOP) as usize, bottom, BACKGROUND);
            if let Some(font) = &self.font {
                font.line(painter, &fit(line, LINE_SIZE), LINE_SIZE * SCALE, centre, y(PANEL.1 + LINE_TOP), LINE_COLOUR, margins);
                for (i, finished) in history.iter().take(HISTORY_LINES).enumerate() {
                    let top = y(PANEL.1 + HISTORY_TOP + i as f32 * HISTORY_SPACING);
                    font.line(painter, &fit(finished, HISTORY_SIZE), HISTORY_SIZE * SCALE, centre, top, HISTORY_COLOUR, margins);
                }
            }
            self.drawn = Some((line.to_owned(), history.to_vec()));
        }
    }
}

/// RGBA8 rows to draw the early splash into: the framebuffer's, or a test's.
#[cfg(any(target_os = "horizon", test))]
struct Painter<'a> {
    pixels: &'a mut [u8],
    width: usize,
    height: usize,
    /// Bytes from one row to the next.
    stride: usize,
}

#[cfg(any(target_os = "horizon", test))]
impl Painter<'_> {
    /// The rows from `top` to `bottom` in `colour`.
    fn fill_rows(&mut self, top: usize, bottom: usize, [r, g, b]: [u8; 3]) {
        let (top, bottom) = (top.min(self.height), bottom.min(self.height));
        if top >= bottom {
            return;
        }
        let (first, row) = (top * self.stride, self.width * 4);
        for pixel in self.pixels[first..first + row].chunks_exact_mut(4) {
            pixel.copy_from_slice(&[r, g, b, 255]);
        }
        for y in top + 1..bottom {
            self.pixels.copy_within(first..first + row, y * self.stride);
        }
    }

    /// An opaque image from column `left` on row `top`, cut to the frame.
    fn copy(&mut self, left: i32, top: usize, image: &Rgba) {
        let start = left.clamp(0, self.width as i32) as usize;
        let end = (left + image.width as i32).clamp(0, self.width as i32) as usize;
        if start >= end {
            return;
        }
        let skip = (start as i32 - left) as usize;
        for y in 0..image.height.min(self.height.saturating_sub(top)) {
            let from = (y * image.width + skip) * 4;
            let to = (top + y) * self.stride + start * 4;
            self.pixels[to..to + (end - start) * 4].copy_from_slice(&image.data[from..from + (end - start) * 4]);
        }
    }

    /// `colour` at `alpha` over the pixel at (`x`, `y`), if it is inside the columns `clip`.
    fn blend(&mut self, x: i32, y: i32, colour: [u8; 3], alpha: f32, clip: (i32, i32)) {
        if alpha <= 0. || x < clip.0.max(0) || x >= clip.1.min(self.width as i32) || y < 0 || y >= self.height as i32 {
            return;
        }
        let at = y as usize * self.stride + x as usize * 4;
        let alpha = alpha.min(1.);
        for (channel, value) in self.pixels[at..at + 3].iter_mut().zip(colour) {
            *channel = (f32::from(*channel) + (f32::from(value) - f32::from(*channel)) * alpha).round() as u8;
        }
    }
}

/// Bevy's built-in Fira Mono, for the early splash, which is drawn before Bevy exists.
#[cfg(any(target_os = "horizon", test))]
struct SplashFont(FontRef<'static>);

#[cfg(any(target_os = "horizon", test))]
impl SplashFont {
    fn new() -> Option<Self> {
        FontRef::try_from_slice(bevy::text::DEFAULT_FONT_DATA).ok().map(Self)
    }

    /// `text` on one line, centred on `centre`, its top at `top`, inside the columns `clip`. `size`
    /// is the em, as Bevy's font size and GDI's are.
    fn line(&self, painter: &mut Painter<'_>, text: &str, size: f32, centre: f32, top: f32, colour: [u8; 3], clip: (i32, i32)) {
        let scale = PxScale::from(size * self.0.height_unscaled() / self.0.units_per_em().unwrap_or(1000.));
        let font = self.0.as_scaled(scale);
        let (mut caret, mut previous) = (0., None);
        let glyphs: Vec<_> = text
            .chars()
            .map(|c| {
                let id = font.glyph_id(c);
                if let Some(previous) = previous {
                    caret += font.kern(previous, id);
                }
                let glyph = id.with_scale_and_position(scale, ab_glyph::point(caret, font.ascent()));
                caret += font.h_advance(id);
                previous = Some(id);
                glyph
            })
            .collect();
        let (left, top) = ((centre - caret / 2.).round() as i32, top.round() as i32);
        for glyph in glyphs {
            let Some(outline) = self.0.outline_glyph(glyph) else { continue };
            let bounds = outline.px_bounds();
            outline.draw(|x, y, coverage| {
                painter.blend(left + bounds.min.x as i32 + x as i32, top + bounds.min.y as i32 + y as i32, colour, coverage, clip);
            });
        }
    }
}

/// The world the screen waits for, and what the render world last found of it.
#[derive(Resource, Clone, Default)]
struct Readiness(Arc<Shared>);

#[derive(Default)]
struct Shared {
    wanted: Mutex<Option<Wanted>>,
    /// The token of the wanted world once the render world found all of it on the GPU, else 0.
    ready: AtomicU64,
    /// What it was still waiting for, for the log.
    missing: Mutex<Missing>,
}

struct Wanted {
    token: u64,
    meshes: Vec<AssetId<Mesh>>,
    images: Vec<AssetId<Image>>,
    buffers: Vec<AssetId<ShaderStorageBuffer>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Missing {
    meshes: usize,
    images: usize,
    buffers: usize,
    pipelines: usize,
}

#[derive(Resource)]
pub(crate) struct LoadingScreen {
    enabled: bool,
    shown: bool,
    shown_at: Option<Instant>,
    /// How many times it has come up: the first is the start, whose steps began in `main`.
    ups: u32,
    /// The published world the screen waits for: its token and when it was published.
    waiting: Option<(u64, Instant)>,
    ready_frames: u8,
    last_token: u64,
    readiness: Readiness,
}

/// What a frame changed, for the log.
#[derive(Debug, PartialEq)]
enum Change {
    None,
    Up,
    /// The world was ready this long after its publish, and the screen went.
    Ready(Duration),
    /// It still wasn't, this long after, and the screen went anyway.
    GaveUp(Duration),
    /// The load ended with no world to wait for: it failed.
    Down,
}

impl LoadingScreen {
    fn new(enabled: bool, readiness: Readiness) -> Self {
        Self { enabled, shown: false, shown_at: None, ups: 0, waiting: None, ready_frames: 0, last_token: 0, readiness }
    }

    pub(crate) fn shown(&self) -> bool {
        self.shown
    }

    /// A world was published; returns the token it waits on, 0 when the screen is off.
    fn wait_for(&mut self, now: Instant) -> u64 {
        if !self.enabled {
            return 0;
        }
        self.last_token += 1;
        self.waiting = Some((self.last_token, now));
        self.ready_frames = 0;
        self.last_token
    }

    /// One frame: `busy` while a map loads, `ready` the token the render world last found all on
    /// the GPU. A world that gets ready while another load runs ends its wait quietly: the screen
    /// stays up for the load.
    fn advance(&mut self, busy: bool, ready: u64, now: Instant) -> Change {
        let mut change = Change::None;
        if let Some((token, published)) = self.waiting {
            self.ready_frames = if ready == token { self.ready_frames.saturating_add(1) } else { 0 };
            let waited = now.saturating_duration_since(published);
            if self.ready_frames >= READY_FRAMES {
                self.waiting = None;
                if !busy {
                    change = Change::Ready(waited);
                }
            } else if waited > MAX_WAIT {
                self.waiting = None;
                if !busy {
                    change = Change::GaveUp(waited);
                }
            }
        }
        let shown = self.enabled && (busy || self.waiting.is_some());
        if shown && !self.shown {
            self.shown_at = Some(now);
            self.ups += 1;
            change = Change::Up;
        } else if !shown && self.shown && change == Change::None {
            change = Change::Down;
        }
        self.shown = shown;
        change
    }
}

/// The scene just published `meshes`, `images` and `buffers` (`PreparedScene::publish`): the
/// screen stays up until they are on the GPU. Does nothing without the screen (bare-world tests).
pub(crate) fn await_world(
    world: &mut World,
    meshes: Vec<AssetId<Mesh>>,
    images: Vec<AssetId<Image>>,
    buffers: Vec<AssetId<ShaderStorageBuffer>>,
) {
    let Some(mut screen) = world.get_resource_mut::<LoadingScreen>() else { return };
    let token = screen.wait_for(Instant::now());
    if token == 0 {
        return;
    }
    let shared = &screen.readiness.0;
    shared.ready.store(0, Ordering::Relaxed);
    *shared.wanted.lock().unwrap_or_else(PoisonError::into_inner) = Some(Wanted { token, meshes, images, buffers });
}

/// Render world: whether the wanted world is all there, every frame while the screen waits.
fn check_world(
    readiness: Res<Readiness>,
    meshes: Option<Res<RenderAssets<RenderMesh>>>,
    images: Option<Res<RenderAssets<GpuImage>>>,
    buffers: Option<Res<RenderAssets<GpuShaderStorageBuffer>>>,
    pipelines: Res<PipelineCache>,
) {
    let wanted = readiness.0.wanted.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(wanted) = wanted.as_ref() else { return };
    let missing = Missing {
        meshes: meshes.map_or(wanted.meshes.len(), |ready| wanted.meshes.iter().filter(|&&id| ready.get(id).is_none()).count()),
        images: images.map_or(wanted.images.len(), |ready| wanted.images.iter().filter(|&&id| ready.get(id).is_none()).count()),
        buffers: buffers.map_or(wanted.buffers.len(), |ready| wanted.buffers.iter().filter(|&&id| ready.get(id).is_none()).count()),
        pipelines: pipelines.waiting_pipelines().count(),
    };
    let ready = if missing == Missing::default() { wanted.token } else { 0 };
    readiness.0.ready.store(ready, Ordering::Relaxed);
    *readiness.0.missing.lock().unwrap_or_else(PoisonError::into_inner) = missing;
}

#[derive(Component)]
struct ScreenRoot;
#[derive(Component)]
struct Board;
/// Which of the screen's text lines a `Text` is.
#[derive(Component, Clone, Copy)]
enum Line {
    Title,
    Status,
    Finished(usize),
}

impl Line {
    fn size(self) -> f32 {
        match self {
            Line::Title => TITLE_SIZE,
            Line::Status => LINE_SIZE,
            Line::Finished(_) => HISTORY_SIZE,
        }
    }
}

/// Hails's layout in Bevy's UI: her box's lines in the middle of the screen, each where her
/// `paint` puts it, in percent of the box; the board on the screen itself, so it rolls from edge
/// to edge.
fn spawn_screen(mut commands: Commands, mut images: ResMut<Assets<Image>>, screen: Res<LoadingScreen>) {
    if !screen.enabled {
        return;
    }
    let board = images.add(board_image());
    let across = |splash: f32| Val::Percent(splash / SPLASH_WIDTH * 100.);
    let down = |splash: f32| Val::Percent(splash / SPLASH_HEIGHT * 100.);
    let row = |top: f32, margin: f32| Node {
        position_type: PositionType::Absolute,
        top: down(top),
        left: across(margin),
        width: across(SPLASH_WIDTH - 2. * margin),
        justify_content: JustifyContent::Center,
        ..default()
    };
    let text = |line: Line, colour_of: [u8; 3]| {
        (line, Text::new(""), TextFont { font_size: line.size() * SCALE, ..default() }, TextColor(colour(colour_of)), TextLayout::new_with_linebreak(LineBreak::NoWrap))
    };
    commands
        .spawn((
            ScreenRoot,
            // Over the menu, the HUD and the frame-time counter.
            GlobalZIndex(1000),
            Visibility::Hidden,
            bevy::ui::FocusPolicy::Block,
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                width: Val::Percent(100.),
                height: Val::Percent(100.),
                ..default()
            },
            BackgroundColor(colour(BACKGROUND)),
        ))
        .with_children(|root| {
            root.spawn((
                Board,
                ImageNode::new(board),
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Percent((PANEL.1 + BOARD_TOP) / SCREEN.1 * 100.),
                    left: Val::Percent(board_left(since_launch()) / SCREEN.0 * 100.),
                    width: Val::Percent((BOARD_LENGTH + 2. * BOARD_PAD) / SCREEN.0 * 100.),
                    height: Val::Percent(BOARD_HEIGHT / SCREEN.1 * 100.),
                    ..default()
                },
            ));
            root.spawn(Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(PANEL.0 / SCREEN.0 * 100.),
                top: Val::Percent(PANEL.1 / SCREEN.1 * 100.),
                width: Val::Percent(SPLASH_WIDTH / SCREEN.0 * 100.),
                height: Val::Percent(SPLASH_HEIGHT / SCREEN.1 * 100.),
                overflow: Overflow::clip(),
                ..default()
            })
            .with_children(|panel| {
                panel.spawn(row(TITLE_TOP, 0.)).with_children(|row| {
                    row.spawn(text(Line::Title, TITLE_COLOUR));
                });
                panel.spawn(row(LINE_TOP, LINE_MARGIN)).with_children(|row| {
                    row.spawn(text(Line::Status, LINE_COLOUR));
                });
                for i in 0..HISTORY_LINES {
                    panel.spawn(row(HISTORY_TOP + i as f32 * HISTORY_SPACING, LINE_MARGIN)).with_children(|row| {
                        row.spawn(text(Line::Finished(i), HISTORY_COLOUR));
                    });
                }
            });
        });
}

fn update_screen(
    mut screen: ResMut<LoadingScreen>,
    transition: Res<crate::map_transition::MapTransition>,
    current: Res<crate::map_transition::CurrentMap>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut roots: Query<(&mut Visibility, &mut Node), With<ScreenRoot>>,
    mut texts: Query<(&Line, &mut Text, &mut TextFont)>,
) {
    let now = Instant::now();
    let ready = screen.readiness.0.ready.load(Ordering::Relaxed);
    let waiting = screen.waiting;
    let shown_for = screen.shown_at.map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
    match screen.advance(transition.busy(), ready, now) {
        Change::None => {}
        Change::Up => {
            // The start's steps began in `main`; a later load's begin now.
            if screen.ups > 1 {
                restart_steps();
            }
            info!("LOADING_SCREEN up: {}", transition.step().unwrap_or_else(|| format!("{} published", current.name)));
        }
        Change::Ready(waited) => info!(
            "LOADING_SCREEN down: {} on the GPU {:.1} s after its publish; the screen was up {:.1} s",
            current.name, waited.as_secs_f32(), shown_for.as_secs_f32(),
        ),
        Change::GaveUp(waited) => {
            let missing = *screen.readiness.0.missing.lock().unwrap_or_else(PoisonError::into_inner);
            warn!("LOADING_SCREEN gave up waiting for {} after {} s, still not on the GPU: {missing:?}", current.name, waited.as_secs());
        }
        Change::Down => info!("LOADING_SCREEN down: the load ended without a world to wait for"),
    }
    // Done waiting: the render world stops checking.
    if let Some((token, _)) = waiting.filter(|_| screen.waiting.is_none()) {
        let mut wanted = screen.readiness.0.wanted.lock().unwrap_or_else(PoisonError::into_inner);
        if wanted.as_ref().is_some_and(|w| w.token == token) {
            *wanted = None;
        }
    }
    // Hidden, it leaves the layout as well, so once a world is up it costs the game nothing.
    let (visibility, display) =
        if screen.shown { (Visibility::Inherited, Display::Flex) } else { (Visibility::Hidden, Display::None) };
    for (mut root, mut node) in &mut roots {
        root.set_if_neq(visibility);
        if node.display != display {
            node.display = display;
        }
    }
    if !screen.shown {
        return;
    }
    step(&transition.step().unwrap_or_else(|| format!("Loading {}: uploading to the GPU", current.name)));
    let (line, history) = current_lines();
    // Logical pixels a splash pixel: `SCALE` on a 720-row screen, in proportion on others.
    let pixel = windows.single().map_or(SCALE, |w| w.height() / SCREEN_HEIGHT as f32 * SCALE);
    for (&which, mut text, mut font) in &mut texts {
        let value = match which {
            Line::Title => TITLE.to_owned(),
            Line::Status => fit(&line, LINE_SIZE),
            Line::Finished(i) => history.get(i).map_or_else(String::new, |h| fit(h, HISTORY_SIZE)),
        };
        if text.0 != value {
            text.0 = value;
        }
        let size = which.size() * pixel;
        if font.font_size != size {
            font.font_size = size;
        }
    }
}

fn roll_board(screen: Res<LoadingScreen>, mut boards: Query<&mut Node, With<Board>>) {
    if !screen.shown() {
        return;
    }
    let left = Val::Percent(board_left(since_launch()) / SCREEN.0 * 100.);
    for mut node in &mut boards {
        node.left = left;
    }
}

/// Gives the screen back to the game once the start's world is on the GPU, or its load failed and
/// the menu has to show why; at once when no start map loads behind the splash (a map named in
/// `options.txt` is read before the first frame). The window attaches the frame after.
#[cfg(target_os = "horizon")]
fn release_splash(screen: Res<LoadingScreen>, config: Res<crate::config::Config>) {
    let done = !screen.enabled || config.boot_map.is_none() || (screen.ups > 0 && !screen.shown);
    if done && bevy_horizon::splash::held() {
        bevy_horizon::splash::hold(false);
        info!("LOADING_SCREEN the start-up splash gives the screen to the game");
    }
}

/// While the splash holds the screen and the loader thread reads the start map, the game draws
/// nothing and only polls it, so it updates 20 times a second instead of 60, leaving the CPU to
/// the loader. Once the map is published it runs at full speed, uploading it and compiling its
/// pipelines behind the splash.
#[cfg(target_os = "horizon")]
fn ease_off_while_held(transition: Res<crate::map_transition::MapTransition>, mut last: Local<Option<Instant>>) {
    if bevy_horizon::splash::held() && transition.loading() {
        if let Some(rest) = last.and_then(|previous| Duration::from_millis(50).checked_sub(previous.elapsed())) {
            std::thread::sleep(rest);
        }
    }
    *last = Some(Instant::now());
}

fn rgba_image(width: u32, height: u32, data: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d { width, height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        // The UI lays the node out from the main world's copy.
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::linear();
    image
}

/// RGBA8 pixels, row after row.
struct Rgba {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

#[cfg(any(target_os = "horizon", test))]
impl Rgba {
    /// These pixels over an opaque `background`.
    fn over(mut self, background: [u8; 3]) -> Self {
        for pixel in self.data.chunks_exact_mut(4) {
            let alpha = f32::from(pixel[3]) / 255.;
            for (channel, back) in pixel[..3].iter_mut().zip(background) {
                *channel = (f32::from(back) + (f32::from(*channel) - f32::from(back)) * alpha).round() as u8;
            }
            pixel[3] = 255;
        }
        self
    }
}

/// The board's length and the height of its box, in the splash's pixels, and the margin either
/// side of it.
const BOARD_LENGTH: f32 = 180.;
const BOARD_HEIGHT: f32 = 40.;
const BOARD_PAD: f32 = 6.;
/// Image pixels per splash pixel, and samples per image pixel along each axis.
const BOARD_SCALE: usize = 2;
const SUPERSAMPLE: usize = 4;

/// The board's colours (Hails's splash.rs), as palette indices; 0 is clear.
const SHADOW: u8 = 1;
const PLY_EDGE: u8 = 2;
const PLY: u8 = 3;
const PLY_DARK: u8 = 4;
const GRIP: u8 = 5;
const METAL: u8 = 6;
const METAL_DARK: u8 = 7;
const WHEEL: u8 = 8;
const WHEEL_EDGE: u8 = 9;
const WHEEL_CORE: u8 = 10;
const BEARING: u8 = 11;
const PALETTE: [[u8; 4]; 12] = [
    [0, 0, 0, 0],
    // The splash's (7, 7, 9) on its background, which is this screen's.
    [0, 0, 0, 128],
    [92, 62, 36, 255],
    [214, 176, 124, 255],
    [150, 104, 62, 255],
    [34, 34, 38, 255],
    [176, 181, 191, 255],
    [96, 100, 110, 255],
    [240, 235, 220, 255],
    [168, 162, 148, 255],
    [86, 146, 214, 255],
    [52, 54, 60, 255],
];

/// Palette indices at `SUPERSAMPLE` samples per image pixel, painted shape over shape.
struct Canvas {
    width: usize,
    height: usize,
    /// Samples per splash pixel.
    scale: f32,
    samples: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        let per = BOARD_SCALE * SUPERSAMPLE;
        let width = (BOARD_LENGTH + 2. * BOARD_PAD) as usize * per;
        let height = BOARD_HEIGHT as usize * per;
        Self { width, height, scale: per as f32, samples: vec![0; width * height] }
    }

    /// A sample column's centre, in splash pixels.
    fn x_of(&self, column: usize) -> f32 {
        (column as f32 + 0.5) / self.scale - BOARD_PAD
    }
    /// The first column whose centre is at `x` or right of it.
    fn column_from(&self, x: f32) -> usize {
        ((x + BOARD_PAD) * self.scale - 0.5).ceil().clamp(0., self.width as f32) as usize
    }
    fn y_of(&self, row: usize) -> f32 {
        (row as f32 + 0.5) / self.scale
    }
    fn rows(&self, top: f32, bottom: f32) -> std::ops::Range<usize> {
        let first = (top * self.scale - 0.5).ceil().clamp(0., self.height as f32) as usize;
        let last = (bottom * self.scale - 0.5).ceil().clamp(0., self.height as f32) as usize;
        first..last
    }

    fn span(&mut self, row: usize, from: f32, to: f32, colour: u8) {
        let (start, end) = (self.column_from(from), self.column_from(to));
        if start < end {
            self.samples[row * self.width + start..row * self.width + end].fill(colour);
        }
    }

    /// A closed polygon, even-odd.
    fn polygon(&mut self, points: &[(f32, f32)], colour: u8) {
        let top = points.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let bottom = points.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
        let mut crossings = Vec::new();
        for row in self.rows(top, bottom) {
            let y = self.y_of(row);
            crossings.clear();
            for (i, &(x0, y0)) in points.iter().enumerate() {
                let (x1, y1) = points[(i + 1) % points.len()];
                if (y0 <= y && y < y1) || (y1 <= y && y < y0) {
                    crossings.push(x0 + (y - y0) * (x1 - x0) / (y1 - y0));
                }
            }
            crossings.sort_by(f32::total_cmp);
            for pair in crossings.chunks_exact(2) {
                self.span(row, pair[0], pair[1], colour);
            }
        }
    }

    fn ellipse(&mut self, (cx, cy): (f32, f32), (rx, ry): (f32, f32), colour: u8) {
        for row in self.rows(cy - ry, cy + ry) {
            let dy = (self.y_of(row) - cy) / ry;
            if dy.abs() <= 1. {
                let half = rx * (1. - dy * dy).sqrt();
                self.span(row, cx - half, cx + half, colour);
            }
        }
    }

    /// Paints the samples within `reach` of a boundary inside `bounds`, `distance` giving a
    /// sample's signed distance to it.
    fn band(&mut self, (left, top, right, bottom): (f32, f32, f32, f32), reach: f32, distance: impl Fn(f32, f32) -> f32, colour: u8) {
        let columns = self.column_from(left - reach)..self.column_from(right + reach);
        for row in self.rows(top - reach, bottom + reach) {
            let y = self.y_of(row);
            for column in columns.clone() {
                if distance(self.x_of(column), y).abs() <= reach {
                    self.samples[row * self.width + column] = colour;
                }
            }
        }
    }

    /// The outline of a closed polygon, `width` wide and centred on its edges, as GDI's pen draws it.
    fn stroke(&mut self, points: &[(f32, f32)], width: f32, colour: u8) {
        for (i, &a) in points.iter().enumerate() {
            let b = points[(i + 1) % points.len()];
            let bounds = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
            self.band(bounds, width / 2., |x, y| segment_distance((x, y), a, b), colour);
        }
    }

    fn ring(&mut self, (cx, cy): (f32, f32), r: f32, width: f32, colour: u8) {
        let bounds = (cx - r, cy - r, cx + r, cy + r);
        self.band(bounds, width / 2., |x, y| (x - cx).hypot(y - cy) - r, colour);
    }

    /// Averages each pixel's samples in linear light.
    fn into_rgba(self) -> Rgba {
        let per = SUPERSAMPLE;
        let (width, height) = (self.width / per, self.height / per);
        let linear = PALETTE.map(|[r, g, b, a]| [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), f32::from(a) / 255.]);
        let mut data = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let mut sum = [0f32; 4];
                for sy in 0..per {
                    for sx in 0..per {
                        let [r, g, b, a] = linear[usize::from(self.samples[(y * per + sy) * self.width + x * per + sx])];
                        sum = [sum[0] + r * a, sum[1] + g * a, sum[2] + b * a, sum[3] + a];
                    }
                }
                let alpha = sum[3] / (per * per) as f32;
                if sum[3] > 0. {
                    data.extend([linear_to_srgb(sum[0] / sum[3]), linear_to_srgb(sum[1] / sum[3]), linear_to_srgb(sum[2] / sum[3])]);
                } else {
                    data.extend([0, 0, 0]);
                }
                data.push((alpha * 255.).round() as u8);
            }
        }
        Rgba { width, height, data }
    }
}

fn segment_distance((px, py): (f32, f32), (ax, ay): (f32, f32), (bx, by): (f32, f32)) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let length = dx * dx + dy * dy;
    let t = if length > 0. { (((px - ax) * dx + (py - ay) * dy) / length).clamp(0., 1.) } else { 0. };
    (px - (ax + t * dx)).hypot(py - (ay + t * dy))
}

fn srgb_to_linear(value: u8) -> f32 {
    let v = f32::from(value) / 255.;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(value: f32) -> u8 {
    let v = value.clamp(0., 1.);
    let encoded = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1. / 2.4) - 0.055 };
    (encoded * 255.).round() as u8
}

/// The board, side view: kicked nose and tail, griptape over a maple edge, two trucks, solid
/// urethane wheels and a shadow on the ground (splash.rs `draw_board`, the splash's coordinates).
fn board_pixels() -> Rgba {
    let mut canvas = Canvas::new();
    let l = BOARD_LENGTH;
    canvas.ellipse((l / 2., 35.), ((l - 20.) / 2., 2.5), SHADOW);
    // The deck's top line: tail kick, flat middle, nose kick (the nose a little longer and higher).
    let top = [
        (1.0, 3.0), (5.0, 3.6), (11.0, 5.6), (18.0, 8.6), (25.0, 11.0), (31.0, 12.0),
        (l - 33.0, 12.0), (l - 26.0, 10.8), (l - 18.0, 7.8), (l - 10.0, 4.4), (l - 4.0, 2.4), (l - 1.0, 2.0),
    ];
    let outline = |upper: f32, lower: f32| -> Vec<(f32, f32)> {
        top.iter().map(|&(x, y)| (x, y + upper)).chain(top.iter().rev().map(|&(x, y)| (x, y + lower))).collect()
    };
    // Maple edge (5 px thick) with a darker centre ply, then the griptape on top.
    let wood = outline(0., 5.);
    canvas.polygon(&wood, PLY);
    canvas.stroke(&wood, 0.5, PLY_EDGE);
    canvas.polygon(&outline(2.6, 3.4), PLY_DARK);
    canvas.polygon(&outline(0., 1.4), GRIP);
    // Trucks: baseplate under the flat of the deck, the hanger down to the axle, then the wheel
    // with a coloured core and the bearing.
    for ax in [40., l - 42.] {
        let plate = [(ax - 10., 17.), (ax + 10., 17.), (ax + 9., 19.5), (ax - 9., 19.5)];
        canvas.polygon(&plate, METAL);
        canvas.stroke(&plate, 0.5, METAL_DARK);
        let hanger = [(ax - 6., 19.5), (ax + 6., 19.5), (ax + 3.5, 26.), (ax - 3.5, 26.)];
        canvas.polygon(&hanger, METAL);
        canvas.stroke(&hanger, 0.5, METAL_DARK);
        canvas.ellipse((ax, 26.5), (7.5, 7.5), WHEEL);
        canvas.ring((ax, 26.5), 7.5, 0.6, WHEEL_EDGE);
        canvas.ellipse((ax, 26.5), (3.9, 3.9), WHEEL_CORE);
        canvas.ellipse((ax, 26.5), (1.7, 1.7), BEARING);
    }
    canvas.into_rgba()
}

fn board_image() -> Image {
    let Rgba { width, height, data } = board_pixels();
    rgba_image(width as u32, height as u32, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> LoadingScreen {
        LoadingScreen::new(true, Readiness::default())
    }

    #[test]
    fn the_screen_stays_up_until_the_published_world_is_ready_for_some_frames() {
        let start = Instant::now();
        let mut s = screen();
        assert_eq!(s.advance(true, 0, start), Change::Up);
        // The commit publishes while the load is still busy, then the load ends.
        let token = s.wait_for(start);
        assert_eq!(s.advance(false, 0, start), Change::None);
        assert!(s.shown());
        for _ in 1..READY_FRAMES {
            assert_eq!(s.advance(false, token, start), Change::None);
        }
        // A frame something was missing again starts the count over.
        assert_eq!(s.advance(false, 0, start), Change::None);
        for _ in 1..READY_FRAMES {
            assert_eq!(s.advance(false, token, start), Change::None);
        }
        assert!(s.shown());
        let later = start + Duration::from_secs(3);
        assert_eq!(s.advance(false, token, later), Change::Ready(Duration::from_secs(3)));
        assert!(!s.shown());
        // The token of an older world doesn't count for a newer one.
        let newer = s.wait_for(later);
        assert_ne!(newer, token);
        assert_eq!(s.advance(false, token, later), Change::Up);
        assert_eq!(s.ups, 2);
    }

    #[test]
    fn a_world_ready_during_another_load_ends_its_wait_quietly() {
        let start = Instant::now();
        let mut s = screen();
        // The Switch's empty start scene, published before its start map loads.
        let token = s.wait_for(start);
        assert_eq!(s.advance(true, token, start), Change::Up);
        for _ in 1..READY_FRAMES {
            assert_eq!(s.advance(true, token, start), Change::None);
        }
        assert_eq!(s.advance(true, token, start), Change::None);
        assert!(s.shown() && s.waiting.is_none());
    }

    #[test]
    fn a_world_that_never_gets_ready_gives_way() {
        let start = Instant::now();
        let mut s = screen();
        s.wait_for(start);
        assert_eq!(s.advance(false, 0, start), Change::Up);
        let waited = MAX_WAIT + Duration::from_secs(1);
        assert_eq!(s.advance(false, 0, start + waited), Change::GaveUp(waited));
        assert!(!s.shown());
    }

    #[test]
    fn a_load_that_ends_without_a_world_takes_the_screen_down() {
        let start = Instant::now();
        let mut s = screen();
        assert_eq!(s.advance(true, 0, start), Change::Up);
        assert_eq!(s.advance(false, 0, start), Change::Down);
        assert!(!s.shown());
    }

    #[test]
    fn a_verify_capture_never_shows_it() {
        let mut s = LoadingScreen::new(false, Readiness::default());
        assert_eq!(s.wait_for(Instant::now()), 0);
        assert_eq!(s.advance(true, 0, Instant::now()), Change::None);
        assert!(!s.shown());
    }

    #[test]
    fn finished_steps_move_below_and_the_last_four_stay() {
        let start = Instant::now();
        let mut steps = Steps::new(start);
        assert_eq!(steps.lines(start), ("Starting  (0 s)".to_owned(), Vec::new()));
        for label in ["a", "b", "c", "d", "e", "f"] {
            steps.step(label);
        }
        // The same step again changes nothing.
        steps.step("f");
        let (line, finished) = steps.lines(start + Duration::from_secs(7));
        assert_eq!(line, "f  (7 s)");
        assert_eq!(finished, ["e", "d", "c", "b"]);
    }

    #[test]
    fn long_lines_are_cut_to_the_margins() {
        assert_eq!(fit("Loading physics  (3 s)", LINE_SIZE), "Loading physics  (3 s)");
        let cut = fit(&"x".repeat(80), LINE_SIZE);
        // 528 pixels between the margins, 9.6 a character at 16.
        assert_eq!(cut.chars().count(), 55);
        assert!(cut.ends_with("..."));
    }

    #[test]
    fn fira_mono_is_as_wide_as_the_cut_assumes() {
        let font = SplashFont::new().unwrap();
        for c in ['W', 'i', ' ', '(', '.'] {
            let advance = font.0.h_advance_unscaled(font.0.glyph_id(c)) / font.0.units_per_em().unwrap();
            assert_eq!(advance, ADVANCE, "{c:?}");
        }
    }

    const FRAME: (usize, usize) = (SCREEN_WIDTH as usize, SCREEN_HEIGHT as usize);

    fn at(pixels: &[u8], x: usize, y: usize) -> [u8; 3] {
        let at = (y * FRAME.0 + x) * 4;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    }

    fn draw(drawing: &mut SplashDrawing, pixels: &mut [u8], seconds: f32, line: &str, history: &[String]) {
        drawing.draw(&mut Painter { pixels, width: FRAME.0, height: FRAME.1, stride: FRAME.0 * 4 }, seconds, line, history);
    }

    /// When the board's image starts `left` splash pixels from the screen's left edge.
    fn when_left(left: f32) -> f32 {
        (left + BOARD_LENGTH + 2. * BOARD_PAD) / ROLL_SPEED
    }

    #[test]
    fn the_early_splash_draws_hails_layout_then_only_what_changes() {
        let mut pixels = vec![0u8; FRAME.0 * FRAME.1 * 4];
        let mut drawing = SplashDrawing::new();
        let history = ["Reading the settings".to_owned()];
        draw(&mut drawing, &mut pixels, when_left(300.), "Loading physics  (3 s)", &history);
        assert_eq!(at(&pixels, 0, 0), BACKGROUND);
        // Her box is 1120 × 480 at (80, 120); each text band has something lit.
        let lit = |pixels: &[u8], top: f32, size: f32| {
            let first = ((PANEL.1 + top) * SCALE) as usize;
            (first..first + (size * SCALE) as usize).any(|y| (0..FRAME.0).any(|x| at(pixels, x, y)[0] > 40))
        };
        assert!(lit(&pixels, TITLE_TOP, TITLE_SIZE), "title");
        assert!(lit(&pixels, LINE_TOP, LINE_SIZE), "status line");
        assert!(lit(&pixels, HISTORY_TOP, HISTORY_SIZE), "the finished line");
        assert!(!lit(&pixels, HISTORY_TOP + HISTORY_SPACING, HISTORY_SIZE), "only one finished line");
        // The tail wheel's coloured core, where the board's own image has it.
        let core = |left: f32| (((left + 42.8 + BOARD_PAD) * SCALE) as usize, ((PANEL.1 + BOARD_TOP + 26.5) * SCALE) as usize);
        let (x, y) = core(300.);
        assert_eq!(at(&pixels, x, y), [86, 146, 214]);
        // A later frame moves the board and clears where it was; the text stays.
        draw(&mut drawing, &mut pixels, when_left(400.), "Loading physics  (3 s)", &history);
        assert_eq!(at(&pixels, x, y), BACKGROUND, "where the board was");
        let (x, y) = core(400.);
        assert_eq!(at(&pixels, x, y), [86, 146, 214]);
        assert!(lit(&pixels, TITLE_TOP, TITLE_SIZE) && lit(&pixels, LINE_TOP, LINE_SIZE));
        // New lines replace the old ones.
        draw(&mut drawing, &mut pixels, when_left(400.), "Loading the skater  (5 s)", &[]);
        assert!(lit(&pixels, LINE_TOP, LINE_SIZE));
        assert!(!lit(&pixels, HISTORY_TOP, HISTORY_SIZE), "no finished lines now");
    }

    #[test]
    fn the_board_rolls_in_from_the_screen_edge_not_her_box() {
        let mut pixels = vec![0u8; FRAME.0 * FRAME.1 * 4];
        let mut drawing = SplashDrawing::new();
        // Half the board off the left edge: its middle, mid-deck, is at the edge, left of her box.
        let left = -(BOARD_LENGTH + 2. * BOARD_PAD) / 2.;
        assert!((board_left(when_left(left)) - left).abs() < 1e-3);
        draw(&mut drawing, &mut pixels, when_left(left), "", &[]);
        let y = ((PANEL.1 + BOARD_TOP + 14.) * SCALE) as usize;
        assert_eq!(at(&pixels, 2, y), [214, 176, 124], "the maple, at the screen's edge");
    }

    fn pixel(image: &Rgba, x: f32, y: f32) -> [u8; 4] {
        // Splash coordinates to the image pixel they fall in.
        let (px, py) = (((x + BOARD_PAD) * BOARD_SCALE as f32) as usize, (y * BOARD_SCALE as f32) as usize);
        image.data[(py * image.width + px) * 4..][..4].try_into().unwrap()
    }

    #[test]
    fn the_board_has_its_deck_wheels_and_shadow_where_the_splash_draws_them() {
        let board = board_pixels();
        assert_eq!((board.width, board.height), (384, 80));
        assert_eq!(pixel(&board, -5., 1.), [0, 0, 0, 0], "clear around it");
        // The maple between the griptape (to 13.4) and the centre ply (from 14.6), mid-deck.
        assert_eq!(pixel(&board, 90., 14.), [214, 176, 124, 255]);
        assert_eq!(pixel(&board, 90., 12.5), [34, 34, 38, 255], "griptape");
        // The tail wheel's coloured core, between the bearing and the urethane.
        assert_eq!(pixel(&board, 42.8, 26.5), [86, 146, 214, 255]);
        assert_eq!(pixel(&board, BOARD_LENGTH - 42. + 5.5, 26.5), [240, 235, 220, 255], "nose wheel");
        assert_eq!(pixel(&board, 90., 35.), [0, 0, 0, 128], "shadow");
    }
}
