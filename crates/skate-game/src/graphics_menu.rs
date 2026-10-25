//! Native-resolution pause UI over a separately scaled 3D render target. At 100% scale the world
//! can be drawn straight into the window instead (`choose_presentation`).
use crate::difficulty::Difficulty;
use crate::custom_difficulty::{Tuning, OPTIONS};
use bevy::ui::RelativeCursorPosition;
use bevy::{
    asset::embedded_asset,
    camera::RenderTarget,
    image::ImageSampler,
    prelude::*,
    render::{
        render_resource::{AsBindGroup, Extent3d, TextureFormat},
    },
    shader::ShaderRef,
    ui_render::UiMaterialPlugin,
    window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode, WindowRef},
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

/// The Switch scales the window to a 16:9 screen, handheld or docked, so anything else is
/// stretched onto it: a 1280x800 setting carried over from a PC squashed every frame by a tenth.
#[cfg(target_os = "horizon")]
const RESOLUTIONS: &[(u32, u32)] = &[(1280, 720)];
#[cfg(not(target_os = "horizon"))]
const RESOLUTIONS: &[(u32, u32)] = &[
    (1280, 720),
    (1280, 800),
    (1600, 900),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (2560, 1600),
    (3840, 2160),
];
const SCALES: &[u32] = &[25, 50, 67, 75, 85, 100];
/// The resolution until one is saved. The Switch's screen is 1280x720: 1280x800 doesn't fit it.
#[cfg(target_os = "horizon")]
const DEFAULT_RESOLUTION: (u32, u32) = (1280, 720);
#[cfg(not(target_os = "horizon"))]
const DEFAULT_RESOLUTION: (u32, u32) = (1280, 800);
const DAY_SPEEDS: &[u32] = &[0, 1, 10, 30, 60, 120, 360, 720];
const LIMITS: &[u32] = &[0, 30, 60, 90, 120, 144, 165, 240];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct GraphicsSettings {
    width: u32,
    height: u32,
    scale: u32,
    fps: u32,
    hour: f32,
    day_speed: u32,
    ambient_level: Option<u32>,
    /// On-screen frame-time counter (`frame_timing`); off by default.
    frame_stats: bool,
    /// With the counter on: the small readout (fps, frame time, 1 % low, worst and a tiny spike
    /// graph) instead of the verbose one. Older settings files lack it and stay verbose.
    frame_stats_simple: bool,
}
impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            width: DEFAULT_RESOLUTION.0,
            height: DEFAULT_RESOLUTION.1,
            scale: 100,
            fps: 0,
            hour: 12.,
            day_speed: 60,
            ambient_level: None,
            frame_stats: false,
            frame_stats_simple: false,
        }
    }
}
impl GraphicsSettings {
    fn validated(mut self) -> Self {
        self.ambient_level = self.ambient_level.map(|level| level.min(100));
        self.hour = if self.hour.is_finite() { self.hour.rem_euclid(24.) } else { 12. };
        if !DAY_SPEEDS.contains(&self.day_speed) { self.day_speed = 60; }
        if !RESOLUTIONS.contains(&(self.width, self.height)) {
            (self.width, self.height) = DEFAULT_RESOLUTION;
        }
        if !SCALES.contains(&self.scale) {
            self.scale = 100;
        }
        if !LIMITS.contains(&self.fps) {
            self.fps = 0;
        }
        self
    }
    fn internal_size(&self, window: UVec2) -> UVec2 {
        (window * self.scale / 100).max(UVec2::ONE)
    }
}
#[derive(Resource)]
pub(crate) struct Menu {
    pub(crate) open: bool,
    selected: usize,
    settings: GraphicsSettings,
    path: PathBuf,
    difficulty: Difficulty,
    custom: Tuning,
    custom_defaults: Tuning,
    custom_apply: bool,
    custom_dirty: bool,
    status: String,
    maps: Vec<crate::map_library::Entry>,
    selected_map: usize,
    multiplayer: bool,
    browser: bool,
    network_page: u8,
    browser_count: usize,
    lan_browser: bool,
    daylight: bool,
    section: usize,
    /// True: the section list has focus, and up and down change the section. False: the item list has it.
    section_focus: bool,
    custom_sections: Vec<(String, Vec<(String,String,String)>)>,
    map_detail: bool,
    destinations: Vec<crate::teleport_menu::Destination>,
    pending_travel: Option<(Option<PathBuf>, [[f32; 4]; 4])>,
}
impl Menu {
    #[cfg(test)]
    pub(crate) fn advance_day(&mut self, seconds: f32) -> f32 {
        if !self.open && self.settings.day_speed > 0 {
            self.settings.hour = (self.settings.hour + seconds * self.settings.day_speed as f32 / 3600.).rem_euclid(24.);
        }
        self.settings.hour
    }
    /// Whether the frame-time counter is shown (graphics menu row, saved).
    pub(crate) fn frame_stats_visible(&self) -> bool {
        self.settings.frame_stats
    }
    /// The counter is on in its simplified form (graphics menu row, saved).
    pub(crate) fn frame_stats_simple(&self) -> bool {
        self.settings.frame_stats && self.settings.frame_stats_simple
    }
    pub(crate) fn diagnostic_settings(&self) -> String {
        format!("{:?}", self.settings)
    }
    /// The 3D render scale in percent of the window.
    pub(crate) fn render_scale(&self) -> u32 {
        self.settings.scale
    }
    /// Sets the 3D render scale without saving it, as performance.rs's `SKATE_PERF_MATRIX` does.
    /// False, and nothing changes, for a scale the menu doesn't offer.
    pub(crate) fn set_render_scale(&mut self, percent: u32) -> bool {
        if !SCALES.contains(&percent) {
            return false;
        }
        self.settings.scale = percent;
        true
    }
    pub(crate) fn transition_finished(&mut self, status: String, resume: bool) {
        self.status = status;
        self.open = !resume;
    }
}
pub(crate) fn gameplay_active(menu: Option<Res<Menu>>) -> bool {
    menu.is_none_or(|m| !m.open)
}

/// Retail "Camera Angle" (Game Settings > Control Settings), in the SKATER section.
const CAMERA_ANGLE_ROW: usize = 5;
const SECTIONS: &[(&str, &str)] = &[
    ("MAPS", "Choose a map, then pick your drop-in spot."),
    ("SKATER", "Make it yours."),
    ("GRAPHICS", "Dial in your display, performance and sound."),
    ("MULTIPLAYER", "A session is better with friends."),
    ("EXTRAS", "Mods, updates and more."),
];
#[derive(Component)] struct MenuTitle;
#[derive(Component)] struct MenuSubtitle;
#[derive(Component)] struct MenuScroll;
impl Menu {
    fn rows(&self) -> Vec<usize> {
        if self.daylight { return (0..4).collect(); }
        if self.multiplayer {
            return if self.browser {
                // "Join with a code" is a Steam row: not in LAN mode, and not on the Switch.
                let tail: &[usize] = if self.lan_browser || cfg!(target_os = "horizon") {
                    &[6, 7, 10]
                } else {
                    &[6, 7, 8, 10]
                };
                std::iter::once(0)
                    .chain(1..=self.browser_count.min(5))
                    .chain(tail.iter().copied())
                    .collect()
            } else {
                match self.network_page {
                    1 => vec![3, 4, 10],
                    2 => vec![5, 7, 10],
                    3 => vec![0, 1, 10],
                    4 => (20..27).chain([10]).collect(),
                    // The Steam rows (2, 6) need the relay files: PC only.
                    _ if cfg!(target_os = "horizon") => vec![13, 14, 15, 16, 17],
                    _ => vec![2, 6, 13, 14, 15, 16, 17],
                }
            };
        }
        match self.section {
            0 if self.map_detail => {
                let mut rows = vec![50, 51];
                if let Some(path) = self.maps.get(self.selected_map).and_then(|m| m.path.as_deref()) {
                    rows.extend(self.destinations.iter().enumerate().filter(|(_,d)| d.matrix.is_some() && crate::teleport_menu::same_map(path, &d.map)).map(|(i,_)| 1_000_000 + i));
                }
                rows
            }
            0 => (1000..1000 + self.maps.len()).collect(),
            1 if self.difficulty == Difficulty::Custom => std::iter::once(3).chain(300..337).chain([CAMERA_ANGLE_ROW,8,10]).collect(),
            1 => vec![3, CAMERA_ANGLE_ROW, 8, 10],
            2 => vec![0, 1, 2, 13, 16, 17, 18, FRAME_STATS_ROW],
            4 => vec![7, 11, 14, 19],
            i if i >= SECTIONS.len() => self.custom_sections.get(i-SECTIONS.len()).map_or(Vec::new(), |(_,entries)| (200..200+entries.len()).collect()),
            _ => Vec::new(),
        }
    }
    fn select_section(&mut self, section: usize) {
        self.section = section;
        self.map_detail = false;
        self.multiplayer = section == 3;
        self.browser = false;
        self.lan_browser = false;
        self.network_page = 0;
        self.daylight = false;
        self.selected = self.rows().first().copied().unwrap_or(100 + section);
        self.status.clear();
    }

    /// Whether the item list has focus. An empty list never does: the section list keeps it.
    fn items_focused(&self) -> bool {
        !self.section_focus && !self.rows().is_empty()
    }
    fn focus_sections(&mut self) {
        self.section_focus = true;
    }
    fn focus_items(&mut self) {
        self.section_focus = false;
    }
    /// Up (`-1`) or down (`1`) in the focused list, wrapping at both ends. Sections change through
    /// `select_section`, so the item list follows them.
    fn move_cursor(&mut self, delta: isize) {
        if self.items_focused() {
            let rows = self.rows();
            let index = rows.iter().position(|r| *r == self.selected).unwrap_or(0) as isize;
            self.selected = rows[(index + delta).rem_euclid(rows.len() as isize) as usize];
        } else {
            let count = (SECTIONS.len() + self.custom_sections.len()) as isize;
            self.select_section((self.section as isize + delta).rem_euclid(count) as usize);
        }
    }
}

#[derive(Resource)]
struct SceneTarget(Handle<Image>);
/// The camera that presents the scene image and draws the UI over it, its full-screen image
/// node, and whether the gameplay camera draws into the window itself instead
/// (`choose_presentation`).
#[derive(Resource)]
struct Presenter {
    output: Entity,
    node: Entity,
    direct: bool,
    /// The presentation camera draws into the window itself (`choose_presentation`).
    to_window: bool,
}
/// FSR 1's edge-adaptive upscale of the scene image (fsr_easu.wgsl), which the presentation
/// camera's full-screen node draws below 100% scale with `SKATE_UPSCALE=easu`
/// (`choose_upscaler`).
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
struct Upscaler {
    #[texture(0)]
    #[sampler(1)]
    scene: Handle<Image>,
}
impl UiMaterial for Upscaler {
    fn fragment_shader() -> ShaderRef {
        "embedded://skate3rust/fsr_easu.wgsl".into()
    }
    fn specialize(
        descriptor: &mut bevy::render::render_resource::RenderPipelineDescriptor,
        _key: bevy::ui_render::ui_material::UiMaterialKey<Self>,
    ) {
        opaque(descriptor);
    }
}
/// FSR 1's sharpening on the scene image's bilinear stretch (fsr_rcas.wgsl), which the
/// presentation camera's full-screen node draws below 100% scale unless `SKATE_UPSCALE` says
/// otherwise (`choose_upscaler`).
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
struct Sharpener {
    #[texture(0)]
    #[sampler(1)]
    scene: Handle<Image>,
}
impl UiMaterial for Sharpener {
    fn fragment_shader() -> ShaderRef {
        "embedded://skate3rust/fsr_rcas.wgsl".into()
    }
    fn specialize(
        descriptor: &mut bevy::render::render_resource::RenderPipelineDescriptor,
        _key: bevy::ui_render::ui_material::UiMaterialKey<Self>,
    ) {
        opaque(descriptor);
    }
}
/// The upscalers write alpha 1 over the whole window, so blending their pixels over what was
/// there gives the same pixels; without it the pass doesn't read the target first.
fn opaque(descriptor: &mut bevy::render::render_resource::RenderPipelineDescriptor) {
    if let Some(target) = descriptor
        .fragment
        .as_mut()
        .and_then(|fragment| fragment.targets.first_mut())
        .and_then(Option::as_mut)
    {
        target.blend = None;
    }
}
#[derive(Resource)]
struct UpscalerMaterials {
    easu: Handle<Upscaler>,
    rcas: Handle<Sharpener>,
}
#[derive(Resource)]
struct FramePacer(Instant);
#[derive(Component)]
struct MenuRoot;
#[derive(Component)]
pub(crate) struct MenuLayoutRoot;
#[derive(Component)]
pub(crate) struct MenuRow(usize);
#[derive(Component)]
struct MenuLabel(usize);
#[derive(Component)]
struct StatusLabel;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MenuInput;

/// The presentation camera must exist before overlays select their UI target.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PresentationSetup;

/// `choose_presentation`. Overlay cameras that draw into the window start and stop before it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PresentationMode;

pub(crate) struct GraphicsMenuPlugin;
impl Plugin for GraphicsMenuPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "fsr_easu.wgsl");
        embedded_asset!(app, "fsr_rcas.wgsl");
        app.add_plugins((UiMaterialPlugin::<Upscaler>::default(), UiMaterialPlugin::<Sharpener>::default()));
        app.insert_resource(FramePacer(Instant::now()))
            .add_systems(PostStartup, setup.in_set(PresentationSetup))
            .add_systems(PreUpdate, (refresh_sections, interact, apply_custom_difficulty).chain().in_set(MenuInput).after(bevy::input::InputSystems))
            .add_systems(PreUpdate, finish_menu_travel.after(crate::map_transition::MapTransitionSet).before(crate::input::poll_controllers))
            .add_systems(PreUpdate, toggle_fullscreen.after(bevy::input::InputSystems))
            .add_systems(Update, preview_menu.before(labels))
            .add_systems(Update, custom_sliders.before(labels))
            .add_systems(Update, (crate::map_render::advance_day, apply, labels, scroll_menu, resize_menu).chain())
            .add_systems(Update, choose_presentation.in_set(PresentationMode).after(crate::camera::present))
            .add_systems(Update, choose_upscaler.after(apply))
            .add_systems(PostUpdate, crate::map_render::position_celestial_bodies.before(bevy::transform::TransformSystems::Propagate))
            .add_systems(Last, pace);
        use bevy::render::{Render, RenderApp, RenderSystems};
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(Render, prewarm_ui_pipelines.in_set(RenderSystems::Queue));
        }
    }
}
fn setup(
    mut commands: Commands,
    config: Res<crate::config::Config>,
    mut images: ResMut<Assets<Image>>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    cameras: Query<Entity, With<Camera3d>>,
    mut time: ResMut<Time<Virtual>>,
    mut upscalers: ResMut<Assets<Upscaler>>,
    mut sharpeners: ResMut<Assets<Sharpener>>,
) {
    let path = config
        .asset_root
        .parent()
        .unwrap_or(&config.asset_root)
        .join("settings/graphics.json");
    let settings = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<GraphicsSettings>(&bytes).unwrap_or_else(|e| {
            warn!("Graphics settings: {e}");
            GraphicsSettings::default()
        }),
        Err(_) => GraphicsSettings::default(),
    }
    .validated();
    window
        .resolution
        .set_physical_resolution(settings.width, settings.height);
    window.present_mode = PresentMode::AutoNoVsync;
    let size = settings.internal_size(window.physical_size());
    let mut image = Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
    image.sampler = ImageSampler::linear();
    let target = images.add(image);
    for camera in &cameras {
        commands.entity(camera).insert((
            RenderTarget::Image(target.clone().into()),
            // Fixed policy, not a setting: see `render_capacity`.
            Msaa::Off,
        ));
        // Render the world before the presentation camera consumes its image.
        commands.entity(camera).insert(Camera {
            order: -1,
            ..default()
        });
    }
    let output = commands
        .spawn((Camera2d, Msaa::Off, IsDefaultUiCamera))
        .id();
    let node = commands.spawn((
        Node {
            width: percent(100),
            height: percent(100),
            position_type: PositionType::Absolute,
            ..default()
        },
        ImageNode::new(target.clone()),
        UiTargetCamera(output),
    )).id();
    commands.insert_resource(Presenter { output, node, direct: false, to_window: false });
    commands.insert_resource(UpscalerMaterials {
        easu: upscalers.add(Upscaler { scene: target.clone() }),
        rcas: sharpeners.add(Sharpener { scene: target.clone() }),
    });
    let data = skate_data::collections::Collections::load(&config.asset_root).expect("validated startup collections");
    let custom_defaults = Tuning::defaults(&data).expect("validated Easy difficulty");
    let custom = Tuning::load(&config.asset_root, &data).expect("validated Custom difficulty");
    let maps = crate::map_library::discover(&config.asset_root);
    let destinations = crate::teleport_menu::load(&config.asset_root).unwrap_or_else(|e| { warn!("Map destinations: {e}"); Vec::new() });
    commands.spawn((MenuRoot, MenuLayoutRoot, UiTargetCamera(output), GlobalZIndex(10), Node {
        display: Display::None, width:percent(100), height:percent(100), align_items:AlignItems::Center,
        justify_content:JustifyContent::Center, position_type:PositionType::Absolute, ..default()
    }, BackgroundColor(Color::srgba(0.015,0.02,0.025,0.90)))).with_children(|root| {
        root.spawn((Node { width:px(1180), height:px(700), flex_shrink:0., padding:UiRect::all(px(24)), column_gap:px(28), ..default() },
            BackgroundColor(Color::srgb(0.035,0.045,0.05)))).with_children(|panel| {
            panel.spawn(Node { width:px(210), flex_shrink:0., flex_direction:FlexDirection::Column, row_gap:px(8), overflow:Overflow::scroll_y(), ..default() }).with_children(|rail| {
                rail.spawn((Text::new("SKATE / 3"),TextFont {font_size:30.,..default()},TextColor(Color::srgb(0.78,0.96,0.3))));
                rail.spawn((Text::new("OFF THE BOARD"),TextFont {font_size:12.,..default()},TextColor(Color::srgb(0.55,0.62,0.62))));
                rail.spawn(Node {height:px(24),..default()});
                for i in 0..SECTIONS.len()+8 {
                    rail.spawn((Button,MenuRow(100+i),Node {width:percent(100),min_height:px(44),padding:UiRect::all(px(12)),align_items:AlignItems::Center,..default()},BackgroundColor(Color::NONE)))
                        .with_child((MenuLabel(100+i),Text::new(""),TextFont {font_size:16.,..default()},TextColor(Color::WHITE)));
                }
                rail.spawn((Text::new("ESC / START  /  RESUME"),TextFont {font_size:12.,..default()},TextColor(Color::srgb(0.55,0.62,0.62)),Node {margin:UiRect::top(px(20)),..default()}));
            });
            panel.spawn(Node {flex_grow:1.,min_width:px(0),flex_direction:FlexDirection::Column,row_gap:px(12),..default()}).with_children(|body| {
                body.spawn((Text::new("MAPS"),MenuTitle,TextFont {font_size:40.,..default()},TextColor(Color::WHITE)));
                body.spawn((Text::new(""),MenuSubtitle,TextFont {font_size:16.,..default()},TextColor(Color::srgb(0.65,0.72,0.72))));
                body.spawn((Node {height:px(3),width:px(64),margin:UiRect::bottom(px(10)),..default()},BackgroundColor(Color::srgb(0.78,0.96,0.3))));
                body.spawn((MenuScroll,ScrollPosition::default(),Node {flex_grow:1.,min_height:px(0),overflow:Overflow::scroll_y(),flex_direction:FlexDirection::Column,row_gap:px(8),..default()})).with_children(|list| {
                    for i in (0..4).chain(300..337).chain(4..10).chain(11..20).chain(20..27).chain([10,FRAME_STATS_ROW]).chain(200..264).chain([50,51]).chain(1000..1000+maps.len()).chain(1_000_000..1_000_000+destinations.len()) {
                        list.spawn((Button,MenuRow(i),Node {flex_direction:if (300..335).contains(&i) {FlexDirection::Column} else {FlexDirection::Row},width:percent(100),min_height:px(56),flex_shrink:0.,padding:UiRect::axes(px(18),px(12)),align_items:AlignItems::Center,border_radius:BorderRadius::all(px(4)),..default()},BackgroundColor(Color::srgb(0.075,0.09,0.095))))
                            .with_children(|row| {
                                row.spawn((MenuLabel(i),Text::new(""),TextFont {font_size:18.,..default()},TextColor(Color::WHITE)));
                                if (300..335).contains(&i) && !OPTIONS[i-300].boolean {
                                    row.spawn((CustomTrack(i-300),RelativeCursorPosition::default(),Node {width:percent(100),height:px(14),margin:UiRect::top(px(8)),..default()},BackgroundColor(Color::srgb(0.15,0.19,0.20))))
                                        .with_child((CustomFill(i-300),Node {width:percent(0),height:percent(100),..default()},BackgroundColor(Color::srgb(0.65,0.85,0.25))));
                                }
                            });
                    }
                });
                body.spawn((StatusLabel,Text::new(""),TextFont {font_size:14.,..default()},TextColor(Color::srgb(0.78,0.96,0.3))));
                body.spawn((Text::new("Up/Down Navigate    Enter / A Select    Left/Right Adjust    LB/RB Switch list    Tab Sections\nCustom tuning: use Apply below. Other settings save automatically."),TextFont {font_size:13.,..default()},TextColor(Color::srgb(0.55,0.62,0.62))));
            });
        });
    });
    commands.insert_resource(SceneTarget(target));
    // The start map, also while it loads behind the loading screen (`Config::boot_map`).
    let current = config.map_path.as_ref().or(config.boot_map.as_ref());
    let selected_map = maps.iter().position(|m| m.path.as_ref() == current).unwrap_or(0);
    if config.start_paused { time.pause(); }
    commands.insert_resource(Menu {
        open: config.start_paused,
        selected: 1000,
        settings,
        path,
        difficulty: config.difficulty,
        custom, custom_defaults, custom_apply:false, custom_dirty:false,
        status: String::new(),
        maps,
        selected_map,
        multiplayer: false,
        browser: false, network_page: 0, browser_count: 0,
        lan_browser: false,
        daylight: false, section: 0, section_focus: false, custom_sections: Vec::new(), map_detail: false, destinations, pending_travel: None,
    });
}
/// GRAPHICS-section rows for the game_audio volume settings.
const FRAME_STATS_ROW: usize = 28;
const AUDIO_ROWS: std::ops::Range<usize> = 16..19;
fn audio_row(row: usize) -> crate::game_audio::AudioRow {
    use crate::game_audio::AudioRow;
    match row { 16 => AudioRow::Master, 17 => AudioRow::Ambience, _ => AudioRow::Effects }
}
fn cycle<T: PartialEq + Copy>(values: &[T], value: T, direction: i32) -> T {
    let index = values.iter().position(|x| *x == value).unwrap_or(0) as i32;
    values[(index + direction).rem_euclid(values.len() as i32) as usize]
}
fn refresh_sections(mods: Res<crate::modding::Mods>, mut menu: ResMut<Menu>) {
    let mut groups = std::collections::BTreeMap::<String,Vec<(String,String,String)>>::new();
    for ((owner,key),definition) in &mods.custom_menus {
        if let Some(section)=&definition.section {
            groups.entry(section.clone()).or_default().push((owner.clone(),key.clone(),definition.title.clone()));
        }
    }
    let selected=menu.custom_sections.get(menu.section.saturating_sub(SECTIONS.len())).filter(|_|menu.section>=SECTIONS.len()).map(|(name,_)|name.clone());
    menu.custom_sections=groups.into_iter().collect();
    if let Some(name)=selected {
        if let Some(i)=menu.custom_sections.iter().position(|(n,_)|n==&name) {menu.section=SECTIONS.len()+i;} else {menu.select_section(0);}
    }
}

pub(crate) fn interact(
    mut config: ResMut<crate::config::Config>,
    mut transition: ResMut<crate::map_transition::MapTransition>,
    mut customiser: ResMut<crate::customiser::Customiser>,
    mut custom_models: ResMut<crate::custom_models::CustomModels>,
    nav: Res<crate::customiser::Navigation>,
    mut physics: ResMut<crate::physics::GamePhysics>,
    keys: Res<ButtonInput<KeyCode>>,
    mut menu: ResMut<Menu>,
    mut time: ResMut<Time<Virtual>>,
    buttons: Query<(&Interaction, &MenuRow), Changed<Interaction>>,
    mut exit: MessageWriter<AppExit>,
    mut net: ResMut<crate::multiplayer::Multiplayer>,
    mut typing: MessageReader<bevy::input::keyboard::KeyboardInput>,
    (mut updater, mut audio, mut camera_angle): (ResMut<crate::updater::Updater>, ResMut<crate::game_audio::AudioSettings>, ResMut<crate::camera::CameraAngleSettings>),
    travel: Res<crate::teleport_menu::Travel>,
    mut mods: ResMut<crate::modding::ModMenu>,
) {
    if transition.busy() {
        menu.open = true;
        time.pause();
        return;
    }
    if travel.open || travel.closed_this_frame || customiser.open || custom_models.open || mods.open {
        return;
    }
    if keys.just_pressed(KeyCode::Escape) || nav.pressed & 0x10 != 0 || (menu.open && nav.pressed & 0x2000 != 0) {
        if menu.open && menu.multiplayer && (menu.browser || menu.network_page != 0) {
            menu.browser = false;
            if menu.network_page == 1 {
                menu.browser = true;
                menu.network_page = 0;
                menu.selected = 8;
            } else {
                let from_debug = menu.network_page == 4;
                menu.network_page = 0;
                menu.selected = if from_debug {
                    15
                } else if menu.lan_browser {
                    17
                } else {
                    2
                };
            }
        } else if menu.open && menu.map_detail {
            menu.map_detail = false;
            menu.selected = 1000 + menu.selected_map;
        } else { menu.open = !menu.open; }
    }
    let mut action = None;
    for event in typing.read() {
        if !menu.open
            || !menu.multiplayer
            || menu.browser
            || !matches!(menu.selected, 3 | 7)
            || !menu.items_focused()
            || !event.state.is_pressed()
        {
            continue;
        }
        if menu.selected == 7 {
            let mut name = net.player_name.clone();
            if event.key_code == KeyCode::Backspace { name.pop(); }
            if let Some(text) = &event.text {
                for ch in text.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_')) {
                    if name.chars().count() < 16 { name.push(ch); }
                }
            }
            if name != net.player_name { net.set_player_name(name); }
            continue;
        }
        if event.key_code == KeyCode::Backspace {
            net.join_code.pop();
        }
        if let Some(text) = &event.text {
            for ch in text.chars().filter(|c| c.is_ascii_hexdigit() || *c == '-') {
                if net.join_code.len() < 40 {
                    net.join_code.push(ch);
                }
            }
        }
    }
    if menu.open {
        menu.browser_count = net.browser_rows.len();
        menu.lan_browser = net.browser_lan;
        if keys.just_pressed(KeyCode::Tab) {
            let direction = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) { SECTIONS.len()+menu.custom_sections.len()-1 } else { 1 };
            let section = (menu.section + direction) % (SECTIONS.len()+menu.custom_sections.len());
            menu.select_section(section);
        }
        if keys.just_pressed(KeyCode::ArrowUp) || nav.pressed & 1 != 0 {
            menu.move_cursor(-1);
        }
        if keys.just_pressed(KeyCode::ArrowDown) || nav.pressed & 2 != 0 {
            menu.move_cursor(1);
        }
        // Bumpers switch focus between the section list (LB) and the item list (RB).
        if nav.pressed & 0x0100 != 0 {
            menu.focus_sections();
        }
        if nav.pressed & 0x0200 != 0 {
            menu.focus_items();
        }
        // Left and right change the value of an adjustable item in the item list.
        let items = menu.items_focused();
        let adjustable = items && ((menu.section == 1 && menu.difficulty == Difficulty::Custom && (300..335).contains(&menu.selected)) || (menu.daylight && menu.selected < 3) || (!menu.multiplayer && !menu.daylight && (menu.selected < 4 || AUDIO_ROWS.contains(&menu.selected) || menu.selected == FRAME_STATS_ROW))
            || (!menu.multiplayer && !menu.daylight && menu.section == 1 && menu.selected == CAMERA_ANGLE_ROW));
        if keys.just_pressed(KeyCode::ArrowLeft) || nav.pressed & 4 != 0 {
            if adjustable { action = Some((menu.selected, -1)); }
        }
        if keys.just_pressed(KeyCode::ArrowRight) || nav.pressed & 8 != 0 {
            if adjustable { action = Some((menu.selected, 1)); }
        }
        if (keys.just_pressed(KeyCode::Enter) && !keys.pressed(KeyCode::AltLeft) && !keys.pressed(KeyCode::AltRight)) || nav.pressed & 0x1000 != 0 {
            action = Some((if menu.items_focused() { menu.selected } else { 100 + menu.section }, 1));
        }
        let mut visible = menu.rows();
        visible.extend(100..100 + SECTIONS.len()+menu.custom_sections.len());
        for (interaction, row) in &buttons {
            if *interaction == Interaction::Pressed && visible.contains(&row.0) {
                // A section row is handled below; an item row takes focus.
                if !(100..100 + SECTIONS.len()+menu.custom_sections.len()).contains(&row.0) {
                    menu.focus_items();
                    menu.selected = row.0;
                }
                action = Some((row.0, 1));
            }
        }
    }
    if let Some((row, _)) = action {
        if (100..100 + SECTIONS.len()+menu.custom_sections.len()).contains(&row) {
            menu.select_section(row - 100);
            menu.focus_items();
            action = None;
        } else if (200..264).contains(&row) {
            if let Some((_,entries))=menu.custom_sections.get(menu.section.saturating_sub(SECTIONS.len())) {
                if let Some((owner,key,_))=entries.get(row-200) {mods.open_registered(owner.clone(),key.clone());}
            }
            action=None;
        } else if row == 50 {
            menu.map_detail = false;
            menu.selected = 1000 + menu.selected_map;
            menu.status.clear();
            action = None;
        } else if row == 51 || row >= 1_000_000 {
            if let Some(entry) = menu.maps.get(menu.selected_map).cloned() {
                let matrix = row.checked_sub(1_000_000).and_then(|i| menu.destinations.get(i)).and_then(|d| d.matrix);
                if net.active() && entry.path != config.map_path {
                    menu.status = "Leave multiplayer before switching maps".into();
                } else if let Some(matrix) = matrix {
                    menu.pending_travel = Some((entry.path.clone(), matrix));
                    if entry.path != config.map_path { transition.request(entry); }
                    menu.status = "Travelling to your spot...".into();
                } else if row == 51 {
                    if net.active() { menu.status = "Leave multiplayer before reloading a map".into(); }
                    else { transition.request(entry); menu.status = "Loading map...".into(); }
                }
            }
            action = None;
        } else if row >= 1000 {
            menu.selected_map = row - 1000;
            menu.map_detail = true;
            menu.selected = 51;
            menu.status.clear();
            action = None;
        }
    }

    if let Some((row, direction)) = action {
        let day_action = menu.daylight;
        if menu.daylight {
            match row {
                0 => menu.settings.hour = ((menu.settings.hour * 4.).round() + direction as f32).rem_euclid(96.) / 4.,
                1 => menu.settings.day_speed = cycle(DAY_SPEEDS, menu.settings.day_speed, direction),
                2 => {
                    // Auto, 0%, 5%, ... 100%, then Auto again.
                    let index = menu.settings.ambient_level.map_or(0, |level| level as i32 / 5 + 1);
                    let next = (index + direction).rem_euclid(22);
                    menu.settings.ambient_level = if next == 0 { None } else { Some((next as u32 - 1) * 5) };
                }
                _ => { menu.daylight = false; menu.selected = 13; }
            }
        } else if menu.browser {
            match row {
                0 => net.browse(0),
                1..=5 => net.join_row(row - 1),
                6 => {
                    let page = net.browser_page.saturating_sub(1);
                    net.browse(page);
                }
                7 => {
                    let page = net.browser_page + 1;
                    if page * 5 < net.browser_total {
                        net.browse(page);
                    }
                }
                8 => { menu.browser=false;menu.network_page=1;menu.selected=3; },
                9 => {
                    exit.write(AppExit::Success);
                }
                10 => {
                    menu.browser = false;
                    menu.selected = if menu.lan_browser { 17 } else { 6 };
                }
                _ => {}
            }
        } else if menu.multiplayer {
            match row {
                0 => net.local(true),
                1 => net.local(false),
                2 => net.steam(true),
                3 => {}
                4 => net.steam(false),
                5 => net.leave(),
                6 => {
                    net.browser_lan = false;
                    net.browse(0);
                    menu.browser = true;
                    menu.lan_browser = false;
                    menu.selected = 0;
                }
                16 => net.host_lan(),
                17 => {
                    net.find_lan();
                    menu.browser = true;
                    menu.lan_browser = true;
                    menu.selected = 0;
                }
                12 => { menu.browser=false;menu.network_page=1;menu.selected=3; },
                8 => menu.open = false,
                9 => {
                    exit.write(AppExit::Success);
                }
                10 => {
                    if menu.network_page==1 {menu.browser=true;menu.network_page=0;menu.selected=8;} else {let from_debug=menu.network_page==4;menu.network_page = 0; menu.selected = if from_debug {15} else {2};}
                }
                13 => { menu.network_page = 2; menu.selected = 7; }
                14 => { menu.network_page = 3; menu.selected = 0; }
                15 => { menu.network_page = 4; menu.selected = 20; }
                _ => {}
            }
        } else {
            match row {
                300..=334 => { menu.custom.adjust(row-300,direction); menu.custom_dirty=true; },
                335 => menu.custom_apply=true,
                336 => { menu.custom=menu.custom_defaults.clone(); menu.custom_dirty=true; menu.status="Easy values restored. Select Apply custom tuning to use them.".into(); },
                0 => {
                    let size = cycle(
                        RESOLUTIONS,
                        (menu.settings.width, menu.settings.height),
                        direction,
                    );
                    (menu.settings.width, menu.settings.height) = size;
                }
                1 => menu.settings.scale = cycle(SCALES, menu.settings.scale, direction),
                2 => menu.settings.fps = cycle(LIMITS, menu.settings.fps, direction),
                3 => {
                    menu.difficulty = cycle(&Difficulty::ALL, menu.difficulty, direction);
                    physics.set_difficulty(menu.difficulty);
                    config.difficulty = menu.difficulty;
                    menu.status = match menu.difficulty.save(&config.asset_root) {
                        Ok(()) => "Difficulty saved".into(),
                        Err(e) => format!("Applied, but could not save: {e}"),
                    };
                }
                CAMERA_ANGLE_ROW => {
                    camera_angle.selected = cycle(&crate::camera::CameraAngle::ALL, camera_angle.selected, direction);
                    menu.status = match camera_angle.save() {
                        Ok(()) if camera_angle.forced_by().is_some() => format!(
                            "Camera angle saved. Mod {} is forcing the camera right now.",
                            camera_angle.forced_by().unwrap_or_default()),
                        Ok(()) => "Camera angle saved".into(),
                        Err(e) => format!("Applied, but could not save: {e}"),
                    };
                }
                6 => menu.open = false,
                7 => {
                    exit.write(AppExit::Success);
                }
                8 => { custom_models.request_stock(); customiser.begin(); },
                9 => {
                    menu.multiplayer = true;
                    menu.selected = 0;
                }
                10 => custom_models.begin(),
                11 => menu.status = updater.open(false),
                12 => {
                    menu.select_section(0);
                    menu.selected_map = menu.maps.iter().position(|m| m.path == config.map_path).unwrap_or(0);
                    menu.map_detail = true;
                    menu.selected = 51;
                },
                13 => { menu.daylight = true; menu.selected = 0; menu.status = "Custom maps: change time, cycle speed and ambient light. Retail lighting stays authored.".into(); },
                14 => mods.begin(),
                16..=18 => menu.status = audio.adjust(audio_row(row), direction),
                FRAME_STATS_ROW => {
                    // Off, Simple, Verbose (the original readout).
                    let modes = [(false, false), (true, true), (true, false)];
                    let s = &mut menu.settings;
                    let now = if s.frame_stats { (true, s.frame_stats_simple) } else { (false, false) };
                    (s.frame_stats, s.frame_stats_simple) = cycle(&modes, now, direction);
                }
                _ => {}
            }
        }
        if ((row < 3 || row == FRAME_STATS_ROW) && !menu.multiplayer && !menu.daylight && !day_action) || (day_action && row < 3) {
            let save = (|| -> Result<(), String> {
                std::fs::create_dir_all(menu.path.parent().unwrap()).map_err(|e| e.to_string())?;
                std::fs::write(
                    &menu.path,
                    serde_json::to_vec_pretty(&menu.settings).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())
            })();
            menu.status = match save {
                Ok(()) => "Saved".into(),
                Err(e) => format!("Could not save: {e}"),
            };
        }
    }
    // The LAN finder probes only while its list is the browser's (closing the menu keeps it).
    if !menu.browser && net.browser_lan {
        net.stop_finding();
    }
    if menu.open && !net.active() {
        time.pause();
    } else {
        time.unpause();
    }
}
/// Read-only troubleshooting row: who is in each controller slot.
fn controller_label(input: &crate::input::ControllerInput) -> String {
    let rows: Vec<String> = input.kinds.iter().enumerate()
        .filter_map(|(slot, kind)| kind.as_deref().map(|kind| format!("{slot}: {}", kind.summary())))
        .collect();
    if rows.is_empty() {
        "Controller            none connected".into()
    } else {
        format!("Controller            {}", rows.join("\n                      "))
    }
}

fn toggle_fullscreen(
    keys: Res<ButtonInput<KeyCode>>,
    menu: Res<Menu>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
) {
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    if !alt || !keys.just_pressed(KeyCode::Enter) {
        return;
    }
    match window.mode {
        WindowMode::Windowed => {
            window.mode = WindowMode::BorderlessFullscreen(MonitorSelection::Primary);
        }
        WindowMode::BorderlessFullscreen(_) | WindowMode::Fullscreen(_, _) => {
            window.mode = WindowMode::Windowed;
            window
                .resolution
                .set_physical_resolution(menu.settings.width, menu.settings.height);
        }
    }
}
fn apply(
    menu: Res<Menu>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    target: Res<SceneTarget>,
    mut images: ResMut<Assets<Image>>,
    mut previous: Local<Option<GraphicsSettings>>,
) {
    if window.mode == WindowMode::Windowed
        && previous
            .as_ref()
            .is_none_or(|p| p.width != menu.settings.width || p.height != menu.settings.height)
    {
        window
            .resolution
            .set_physical_resolution(menu.settings.width, menu.settings.height);
    }
    let size = menu.settings.internal_size(window.physical_size());
    if let Some(image) = images.get(&target.0) {
        if image.size() != size {
            images.get_mut(&target.0).unwrap().resize(Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 1,
            });
        }
    }
    *previous = Some(menu.settings.clone());
}
/// Below 100% scale the presentation camera's node draws the scene image through the render
/// options' upscale (`RenderOptions::upscale`, `SKATE_UPSCALE`): FSR 1's sharpening on its
/// bilinear stretch (`Sharpener`) unless set, FSR 1's edge-adaptive upscale (`Upscaler`), or the
/// image's bilinear filter alone. At 100% it draws through the image's own filter, one scene pixel
/// to each window pixel. The scene image changes size with the scale (`apply`), so the material in
/// use is marked changed then, for its bind group to take the new texture.
fn choose_upscaler(
    mut commands: Commands,
    menu: Res<Menu>,
    options: Option<Res<crate::render_options::RenderOptions>>,
    presenter: Option<Res<Presenter>>,
    target: Res<SceneTarget>,
    upscalers: Option<Res<UpscalerMaterials>>,
    mut easu: ResMut<Assets<Upscaler>>,
    mut rcas: ResMut<Assets<Sharpener>>,
    images: Res<Assets<Image>>,
    mut applied: Local<Option<(crate::render_options::Upscale, UVec2)>>,
) {
    use crate::render_options::{RenderOptions, Upscale};
    let (Some(presenter), Some(upscalers)) = (presenter, upscalers) else {
        return;
    };
    let upscale = if menu.settings.scale < 100 {
        options.map_or(RenderOptions::default().upscale, |options| options.upscale)
    } else {
        Upscale::Bilinear
    };
    let size = images.get(&target.0).map_or(UVec2::ZERO, |image| image.size());
    if *applied == Some((upscale, size)) {
        return;
    }
    if applied.is_none_or(|(was, _)| was != upscale) {
        let mut node = commands.entity(presenter.node);
        node.remove::<(ImageNode, MaterialNode<Upscaler>, MaterialNode<Sharpener>)>();
        match upscale {
            Upscale::Rcas => node.insert(MaterialNode(upscalers.rcas.clone())),
            Upscale::Easu => node.insert(MaterialNode(upscalers.easu.clone())),
            Upscale::Bilinear => node.insert(ImageNode::new(target.0.clone())),
        };
        info!(
            "[Skate] presentation: the {}x{} scene image reaches the window through {}",
            size.x,
            size.y,
            match upscale {
                Upscale::Rcas => "FSR 1's sharpening on its bilinear stretch",
                Upscale::Easu => "FSR 1's edge-adaptive upscale",
                Upscale::Bilinear => "its own filter",
            }
        );
    }
    match upscale {
        Upscale::Rcas => {
            rcas.get_mut(&upscalers.rcas);
        }
        Upscale::Easu => {
            easu.get_mut(&upscalers.easu);
        }
        Upscale::Bilinear => {}
    }
    *applied = Some((upscale, size));
}
/// At 100% scale the scene image is the window's size, so the presentation camera only clears a
/// texture and its depth, copies the image into it under the UI, and copies it into the window:
/// about 18 MB of reads and writes a frame at 1280x720 for no change. With
/// `RenderOptions::present_direct` the gameplay camera then draws into the window itself, the
/// UI over the world, and the presentation camera stops. It takes over again at other scales,
/// while the gameplay camera is stopped (loading, before the first frame), and while another
/// camera draws into the window: the session-marker HUD's draws over the presentation camera's
/// texture, which the world's would not be in (session_marker/hud.rs).
fn choose_presentation(
    mut commands: Commands,
    options: Option<Res<crate::render_options::RenderOptions>>,
    menu: Res<Menu>,
    target: Res<SceneTarget>,
    mut presenter: ResMut<Presenter>,
    mut cameras: Query<(Entity, &mut Camera, &RenderTarget, Has<crate::camera::GameplayCamera>)>,
    ui_targets: Query<(Entity, &UiTargetCamera)>,
) {
    let mut gameplay = None;
    let mut overlaid = false;
    for (entity, camera, render_target, is_gameplay) in &cameras {
        if is_gameplay {
            gameplay = Some((entity, camera.is_active));
        } else if entity != presenter.output
            && camera.is_active
            && matches!(render_target, RenderTarget::Window(_))
        {
            overlaid = true;
        }
    }
    let Some((gameplay, active)) = gameplay else {
        return;
    };
    let options = options.map_or_else(crate::render_options::RenderOptions::default, |options| *options);
    let enabled = options.present_direct;
    // While bevy_horizon's splash holds the screen there is no window to draw into: the world
    // renders into the scene image instead, so its pipelines compile behind the splash.
    #[cfg(target_os = "horizon")]
    let enabled = enabled && !bevy_horizon::splash::held();
    let direct = enabled && menu.settings.scale == 100 && active && !overlaid;
    let (from, to) = if direct { (presenter.output, gameplay) } else { (gameplay, presenter.output) };
    if direct != presenter.direct {
        presenter.direct = direct;
        commands.entity(gameplay).insert(if direct {
            RenderTarget::Window(WindowRef::Primary)
        } else {
            RenderTarget::Image(target.0.clone().into())
        });
        commands.entity(from).remove::<IsDefaultUiCamera>();
        commands.entity(to).insert(IsDefaultUiCamera);
        commands.entity(presenter.node).insert(if direct { Visibility::Hidden } else { Visibility::Inherited });
        if let Ok((_, mut camera, ..)) = cameras.get_mut(presenter.output) {
            camera.is_active = !direct;
        }
        info!(
            "[Skate] presentation: {}",
            if direct { "the gameplay camera draws into the window" } else { "through the scene image" }
        );
    }
    // The presentation camera's UI is the image's node, which covers the window, and the UI over
    // it, so unless another camera draws into the window after it, the UI goes into the window
    // itself and the camera's own texture is neither cleared nor copied
    // (`RenderOptions::present_to_window`). The node draws without blending (`opaque`).
    let to_window = !direct && !overlaid && options.present_to_window;
    if to_window != presenter.to_window {
        presenter.to_window = to_window;
        if let Ok((_, mut camera, ..)) = cameras.get_mut(presenter.output) {
            (camera.output_mode, camera.clear_color) = if to_window {
                (bevy::camera::CameraOutputMode::Skip, ClearColorConfig::None)
            } else {
                (bevy::camera::CameraOutputMode::default(), ClearColorConfig::Default)
            };
        }
        if to_window {
            commands.entity(presenter.output).insert(bevy::ui_render::UiToOutput);
        } else {
            commands.entity(presenter.output).remove::<bevy::ui_render::UiToOutput>();
        }
        info!(
            "[Skate] presentation: the presentation camera draws {}",
            if to_window {
                "into the window itself"
            } else {
                "into its own texture, which upscaling copies into the window"
            }
        );
    }
    // Every frame, since an overlay spawned after the switch names the camera it found then.
    for (entity, ui_target) in &ui_targets {
        if ui_target.entity() == from && entity != presenter.node {
            commands.entity(entity).insert(UiTargetCamera(to));
        }
    }
}
/// Bevy compiles a camera's UI pipeline the first time a node draws through it. With direct
/// presentation the menu draws through the gameplay camera, whose HDR target needs a variant
/// nothing else uses there (the HUD composite has its own pipeline), so the first pause queued it
/// behind any world pipelines still compiling and the menu stayed invisible until it was ready.
/// This asks for every camera's variant as soon as the camera is active, with the key
/// `queue_uinodes` uses; after the first frame each request is a cache lookup. While the tone
/// pass writes the window (retail_exposure.rs `choose_tone_output`), the gameplay camera's UI
/// view isn't HDR, and the key is the presentation camera's.
fn prewarm_ui_pipelines(
    ui_pipeline: Option<Res<bevy::ui_render::UiPipeline>>,
    pipelines: Option<ResMut<bevy::render::render_resource::SpecializedRenderPipelines<bevy::ui_render::UiPipeline>>>,
    pipeline_cache: Res<bevy::render::render_resource::PipelineCache>,
    cameras: Query<(&bevy::ui_render::UiCameraView, Option<&bevy::ui_render::UiAntiAlias>), With<bevy::render::view::ExtractedView>>,
    ui_views: Query<&bevy::render::view::ExtractedView>,
    mut seen: Local<Vec<(bevy::render::render_resource::CachedRenderPipelineId, bool)>>,
) {
    let (Some(ui_pipeline), Some(mut pipelines)) = (ui_pipeline, pipelines) else {
        return;
    };
    for (ui_view, anti_alias) in &cameras {
        if let Ok(view) = ui_views.get(ui_view.0) {
            let hdr = view.hdr;
            let anti_alias = matches!(anti_alias, None | Some(bevy::ui_render::UiAntiAlias::On));
            let id = pipelines.specialize(
                &pipeline_cache,
                &ui_pipeline,
                bevy::ui_render::UiPipelineKey { hdr, anti_alias },
            );
            // Once per variant, so a log shows whether the menu's was ready before a pause.
            match seen.iter_mut().find(|(seen_id, _)| *seen_id == id) {
                None => {
                    info!("[Skate] UI pipeline (hdr {hdr}, anti-alias {anti_alias}): first asked for");
                    seen.push((id, false));
                }
                Some((_, ready)) if !*ready && pipeline_cache.get_render_pipeline(id).is_some() => {
                    *ready = true;
                    info!("[Skate] UI pipeline (hdr {hdr}, anti-alias {anti_alias}): ready");
                }
                _ => {}
            }
        }
    }
}
fn labels(
    menu: Res<Menu>,
    transition: Res<crate::map_transition::MapTransition>,
    time: Res<Time<Real>>,
    customiser: Res<crate::customiser::Customiser>,
    custom_models: Res<crate::custom_models::CustomModels>,
    travel: Res<crate::teleport_menu::Travel>,
    mods: Res<crate::modding::ModMenu>,
    net: Res<crate::multiplayer::Multiplayer>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut root: Single<&mut Node, With<MenuRoot>>,
    mut labels: Query<(&MenuLabel, &mut Text), (Without<StatusLabel>, Without<MenuTitle>, Without<MenuSubtitle>)>,
    mut headings: Query<(&mut Text, Has<MenuTitle>), (Or<(With<MenuTitle>, With<MenuSubtitle>)>, Without<StatusLabel>)>,
    mut status: Single<&mut Text, With<StatusLabel>>,
    debug: (Res<crate::modding::Mods>, Res<crate::physics::GamePhysics>, Res<crate::multiplayer::appearance::Appearances>, Res<crate::input::ControllerInput>),
    (audio, camera_angle): (Option<Res<crate::game_audio::AudioSettings>>, Res<crate::camera::CameraAngleSettings>),
    mut buttons: Query<(&MenuRow, &Interaction, &mut BackgroundColor, &mut Node), Without<MenuRoot>>,
) {
    // Only on change: Bevy counts every write as one, and lays the UI out again (findings 63 §6.2).
    let display = if menu.open && !travel.open && !customiser.open && !custom_models.open && !mods.open {
        Display::Flex
    } else {
        Display::None
    };
    if root.display != display {
        root.display = display;
    }
    if !menu.open {
        return;
    }
    for (mut text, title) in &mut headings {
        **text = if menu.map_detail {
            if title { menu.maps.get(menu.selected_map).map_or("MAP", |m| m.label.as_str()) }
            else { "Choose a teleport spot, or skate from the default spawn." }
        } else if menu.daylight {
            if title { "DAY & NIGHT" } else { "Custom maps: time and ambient light. Retail lighting stays authored." }
        } else if menu.browser {
            if menu.lan_browser {
                if title { "FIND A LAN GAME" } else { "Games hosted on this Wi-Fi network. Pick one to join." }
            } else if title {
                "FIND A SESSION"
            } else {
                "Browse public lobbies and join a crew."
            }
        } else if menu.multiplayer && menu.network_page != 0 {
            match (menu.network_page, title) {
                (1,true) => "JOIN A FRIEND", (1,false) => "Select the code field, type your friend's code, then choose Join session.",
                (2,true) => "PLAYER & SESSION",
                (2,false) if cfg!(target_os = "horizon") => "Your name comes from settings/player.json on the SD card. Leave your current game here.",
                (2,false) => "Select your name to edit it. Leave your current session here.",
                (4,true) => "MULTIPLAYER DEBUG", (4,false) => "Live network and mod diagnostics. Scroll or use Up/Down to inspect.",
                (3,true) => "LOCAL TESTING", (_,false) => "Advanced: host or join a local test session without Steam.",
                _ => "MULTIPLAYER",
            }
        } else if title { SECTIONS.get(menu.section).map_or_else(||menu.custom_sections.get(menu.section-SECTIONS.len()).map_or("",|(n,_)|n.as_str()),|s|s.0) } else { SECTIONS.get(menu.section).map_or("Choose an activity.",|s|s.1) }.into();
    }
    let mut debug_rows = Vec::new();
    if menu.multiplayer && menu.network_page == 4 {
        debug_rows.extend(net.debug_sections());
        debug_rows.push(format!("CHARACTERS & COLLISIONS\n{} {}\nPlayer contacts: {} | Network active: {}",
            debug.2.progress, debug.2.status, debug.1.network_contacts, debug.1.network_active));
        debug_rows.extend(debug.0.multiplayer_debug_sections());
    }
    let s = &menu.settings;
    let size = s.internal_size(window.physical_size());
    for (label, mut text) in &mut labels {
        **text = if (100..113).contains(&label.0) {
            let i=label.0-100;
            let name=SECTIONS.get(i).map(|s|s.0).or_else(||menu.custom_sections.get(i.saturating_sub(SECTIONS.len())).map(|(n,_)|n.as_str())).unwrap_or("");
            format!("{:02}   {}",i+1,name)
        } else if (200..264).contains(&label.0) {
            menu.custom_sections.get(menu.section.saturating_sub(SECTIONS.len())).and_then(|(_,e)|e.get(label.0-200)).map(|(_,_,t)|t.clone()).unwrap_or_default()
        } else if label.0 >= 1_000_000 {
            menu.destinations.get(label.0 - 1_000_000).map(|d| d.name.clone()).unwrap_or_default()
        } else if label.0 == 50 { "<  All maps".into()
        } else if label.0 == 51 { "Skate from default spawn".into()
        } else if label.0 >= 1000 {
            menu.maps.get(label.0 - 1000).map(|entry| format!("{}    /    VIEW SPOTS", entry.label)).unwrap_or_default()
        } else if menu.multiplayer && menu.network_page == 4 && (20..27).contains(&label.0) {
            debug_rows.get(label.0 - 20).cloned().unwrap_or_default()
        } else if menu.daylight {
            match label.0 {
                0 => { let minutes = (s.hour * 60.).floor() as u32 % 1440; format!("Time of day          {:02}:{:02}", minutes / 60, minutes % 60) },
                1 => if s.day_speed == 0 { "Cycle speed          Frozen".into() } else { format!("Cycle speed          {}x ({} min/day)", s.day_speed, 1440 / s.day_speed) },
                2 => match s.ambient_level {
                    Some(level) => format!("Ambient light        {level}%"),
                    None => "Ambient light        Auto (day/night)".into(),
                },
                3 => "Back".into(),
                _ => String::new(),
            }
        } else if menu.browser {
            match label.0 {
                0 => "Refresh sessions".into(),
                1..=5 if menu.lan_browser => net.lan_label(label.0 - 1),
                1..=5 => net
                    .browser_rows
                    .get(label.0 - 1)
                    .map(|row| {
                        format!(
                            "{} | {}/{} | #{}{}",
                            row.map,
                            row.players,
                            row.capacity,
                            row.id % 100000,
                            if row.compatible { "" } else { " | Update required" }
                        )
                    })
                    .unwrap_or_else(|| "--".into()),
                6 => "Previous page".into(),
                7 => "Next page".into(),
                8 => "Join with a code".into(),
                9 => "Quit game".into(),
                _ => "Back to multiplayer".into(),
            }
        } else if menu.multiplayer {
            match label.0 {
                0 => "Host local session".into(),
                1 => "Join local session".into(),
                2 => "Host a session".into(),
                3 => format!(
                    "Join code: {}{}",
                    net.join_code,
                    if menu.selected == 3 && menu.items_focused() { "_" } else { "" }
                ),
                4 => "Join session".into(),
                5 => "Leave multiplayer".into(),
                6 => "Find a session".into(),
                7 => format!("Your name: {}{}", net.player_name, if menu.selected == 7 && menu.items_focused() { "_" } else { "" }),
                16 => "Host LAN game".into(),
                17 => "Find LAN games".into(),
                12 => "Join with a code".into(),
                8 => "Resume".into(),
                9 => "Quit game".into(),
                13 => "Player & session".into(),
                14 => "Advanced / local testing".into(),
                15 => "Debug".into(),
                _ => "<  Multiplayer".into(),
            }
        } else {
            match label.0 {
                0 => format!("Resolution          {} x {}", s.width, s.height),
                1 => format!(
                    "Internal resolution   {}%  ({} x {})",
                    s.scale, size.x, size.y
                ),
                2 => format!(
                    "FPS limit             {}",
                    if s.fps == 0 {
                        "Unlimited".into()
                    } else {
                        s.fps.to_string()
                    }
                ),
                3 => format!("Difficulty            {}", menu.difficulty.label()),
                CAMERA_ANGLE_ROW => format!("Camera angle          {}", camera_angle.selected.label()),
                300..=334 => menu.custom.label(label.0-300),
                335 => if menu.custom_dirty {"Apply custom tuning *".into()} else {"Apply custom tuning".into()},
                336 => "Reset custom tuning to Easy".into(),
                6 => "Resume".into(),
                7 => "Quit game".into(),
                8 => "Character customiser".into(),
                10 => "Custom models".into(),
                11 => "Updates".into(),
                12 => "Teleport".into(),
                13 => "Day & night".into(),
                14 => "Mods".into(),
                16..=18 => audio.as_ref().map(|a| a.label(audio_row(label.0))).unwrap_or_default(),
                19 => controller_label(&debug.3),
                FRAME_STATS_ROW => format!("Frame-time counter    {}", if !s.frame_stats { "Off" } else if s.frame_stats_simple { "Simple" } else { "Verbose" }),
                _ => "Multiplayer".into(),
            }
        };
    }
    ***status = if transition.busy() {
        format!("{} {}\nGameplay is paused. Please wait.", ["|", "/", "-", "\\"][(time.elapsed_secs() * 4.) as usize % 4], transition.label())
    } else if menu.multiplayer && menu.network_page == 4 {
        "Diagnostics stay in this menu; gameplay shows names and ping only.".into()
    } else if menu.browser {
        net.browser_status.clone()
    } else if menu.multiplayer {
        format!(
            "{}{}",
            net.status,
            if net.host_code.is_empty() {
                String::new()
            } else {
                format!("\nYour connection code: {}", net.host_code)
            }
        )
    } else if menu.items_focused() && menu.section == 1 && menu.difficulty == Difficulty::Custom && (300..335).contains(&menu.selected) {
        format!("{}\n{}", OPTIONS[menu.selected-300].description,
            if menu.custom_dirty {"Changes pending - select Apply custom tuning below. Left/Right adjusts; drag bars with the mouse."} else {"Left/Right adjusts; drag bars with the mouse. Custom starts from Easy."})
    } else {
        menu.status.clone()
    };
    let visible = menu.rows();
    let items = menu.items_focused();
    let open = 100 + menu.section;
    for (row, interaction, mut color, mut node) in &mut buttons {
        node.display = if visible.contains(&row.0) || (100..100+SECTIONS.len()+menu.custom_sections.len()).contains(&row.0) { Display::Flex } else { Display::None };
        // The focused row is bright. While the items have focus, the open section stays dimly marked.
        let focused = if items { row.0 == menu.selected } else { row.0 == open };
        color.0 = if focused || *interaction == Interaction::Hovered {
            Color::srgb(0.24, 0.33, 0.12)
        } else if row.0 == open {
            Color::srgb(0.15, 0.21, 0.10)
        } else {
            Color::srgb(0.075, 0.09, 0.095)
        };
    }
}
// Opt-in screenshot coverage for overlays; never changes an ordinary session.
fn preview_menu(config: Res<crate::config::Config>, mut menu: ResMut<Menu>, mut mods: ResMut<crate::modding::ModMenu>, mut done: Local<bool>) {
    if *done || config.verification_capture.is_none() { return; }
    *done = true;
    match std::env::var("SKATE_VERIFY_MENU").as_deref() {
        Ok("custom") => { menu.open = true; menu.select_section(1); menu.selected=300; },
        Ok("mods") => { menu.open = true; mods.begin(); }
        Ok("multiplayer") => { menu.open = true; menu.select_section(3); }
        _ => {}
    }
}
fn finish_menu_travel(
    mut menu: ResMut<Menu>, transition: Res<crate::map_transition::MapTransition>,
    current: Res<crate::map_transition::CurrentMap>, mut skater: ResMut<crate::physics::SkaterRuntime>,
    mut time: ResMut<Time<Virtual>>,
) {
    if transition.busy() { return; }
    let Some((path, matrix)) = menu.pending_travel.take() else { return; };
    // A failed map transaction retains the previous world: never apply another map's coordinates there.
    if current.path != path { return; }
    match skater.travel_to(matrix) {
        Ok(()) => { menu.open = false; time.unpause(); }
        Err(e) => { menu.open = true; menu.status = format!("Could not travel: {e}"); }
    }
}
fn resize_menu(
    window: Single<&Window, With<PrimaryWindow>>, roots: Query<Entity, With<MenuLayoutRoot>>,
    children: Query<&Children>, mut nodes: Query<(&mut Node, Option<&mut TextFont>)>,
    mut previous: Local<Option<f32>>,
) {
    let scale = (window.width() / 1280.).min(window.height() / 800.).max(0.25);
    let factor = scale / previous.unwrap_or(1.);
    if (factor - 1.).abs() < 0.0001 { return; }
    fn resize(value: &mut Val, factor: f32) { if let Val::Px(px) = value { *px *= factor; } }
    for root in &roots {
        for entity in children.iter_descendants(root) {
            if let Ok((mut node, font)) = nodes.get_mut(entity) {
                let node = &mut *node;
                for value in [&mut node.width, &mut node.height, &mut node.min_width, &mut node.min_height,
                    &mut node.max_width, &mut node.max_height, &mut node.row_gap, &mut node.column_gap,
                    &mut node.padding.left, &mut node.padding.right, &mut node.padding.top, &mut node.padding.bottom,
                    &mut node.margin.left, &mut node.margin.right, &mut node.margin.top, &mut node.margin.bottom] {
                    resize(value, factor);
                }
                if let Some(mut font) = font { font.font_size *= factor; }
            }
        }
    }
    *previous = Some(scale);
}
fn scroll_menu(
    menu: Res<Menu>,
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    mut scroll: Query<(&mut ScrollPosition, &ComputedNode, &Node), With<MenuScroll>>,
    rows: Query<(&MenuRow, &ComputedNode)>,
    mut previous: Local<Option<(usize, bool, bool, usize, bool, u8)>>,
) {
    let delta: f32 = wheel.read().map(|e| e.y * if e.unit == bevy::input::mouse::MouseScrollUnit::Line { 40. } else { 1. }).sum();
    if !menu.open { return; }
    let state = (menu.section, menu.daylight, menu.browser, menu.selected, menu.map_detail, menu.network_page);
    let visible = menu.rows();
    for (mut pos, node, layout) in &mut scroll {
        let scale = node.inverse_scale_factor();
        let height = node.size().y * scale;
        let max = (node.content_size().y * scale - height).max(0.);
        if previous.as_ref().is_none_or(|p| (p.0,p.1,p.2,p.4,p.5) != (state.0,state.1,state.2,state.4,state.5)) { pos.y = 0.; }
        else if previous.as_ref() != Some(&state) {
            let mut top = 0.;
            for id in &visible {
                let row_height = rows.iter().find(|(r,_)| r.0 == *id).map_or(56., |(_,n)| n.size().y * scale);
                if *id == menu.selected {
                    if top < pos.y { pos.y = top; }
                    else if top + row_height > pos.y + height { pos.y = top + row_height - height; }
                    break;
                }
                top += row_height + if let Val::Px(gap) = layout.row_gap { gap } else { 0. };
            }
        }
        pos.y = (pos.y - delta).clamp(0., max);
    }
    *previous = Some(state);
}
fn pace(menu: Option<Res<Menu>>, mut pacer: ResMut<FramePacer>) {
    let Some(menu) = menu else {
        return;
    };
    if menu.settings.fps > 0 {
        let period = Duration::from_secs_f64(1. / f64::from(menu.settings.fps));
        if let Some(wait) = period.checked_sub(pacer.0.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    pacer.0 = Instant::now();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn culling_can_toggle_with_msaa_and_render_scale_changes() {
        let mut app = App::new();
        let mut images = Assets::<Image>::default();
        let target = images.add(Image::new_target_texture(
            1280,
            800,
            TextureFormat::Rgba8UnormSrgb,
            None,
        ));
        app.insert_resource(SceneTarget(target.clone()))
            .insert_resource(images)
            .insert_resource(Menu {
                open: false, selected: 0, settings: GraphicsSettings::default(),
                difficulty: Difficulty::Easy, custom:Tuning::default(), custom_defaults:Tuning::default(), custom_apply:false, custom_dirty:false, path: PathBuf::new(), status: String::new(),
                multiplayer: false, browser: false, network_page: 0, browser_count: 0, lan_browser: false, daylight: false, section: 0, section_focus: false, custom_sections: Vec::new(), map_detail: false, destinations: Vec::new(), pending_travel: None,
                maps: vec![crate::map_library::Entry { label: "Test world".into(), path: None }], selected_map: 0,
            })
            .add_systems(Update, apply);
        {
            let mut menu = app.world_mut().resource_mut::<Menu>();
            menu.settings.hour = 23.5;
            menu.settings.day_speed = 60;
            assert!((menu.advance_day(60.) - 0.5).abs() < 0.0001);
            menu.open = true;
            assert_eq!(menu.advance_day(60.), 0.5);
            menu.open = false;
            menu.settings.day_speed = 0;
            assert_eq!(menu.advance_day(60.), 0.5);
        }
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        let camera = app.world_mut().spawn((Camera3d::default(), Msaa::Off)).id();
        app.update();
        // MSAA and occlusion culling are no longer settings, so the only thing
        // `apply` still adapts is the internal render target size.
        {
            let mut menu = app.world_mut().resource_mut::<Menu>();
            menu.settings.scale = 67;
        }
        app.update();
        assert_eq!(*app.world().get::<Msaa>(camera).unwrap(), Msaa::Off);
        assert_eq!(
            app.world()
                .resource::<Assets<Image>>()
                .get(&target)
                .unwrap()
                .size(),
            UVec2::new(857, 536)
        );
    }
    #[test]
    fn sections_expose_only_real_rows_and_all_maps() {
        let mut menu = Menu {
            open: true, selected: 1000, settings: GraphicsSettings::default(),
            path: PathBuf::new(), difficulty: Difficulty::Easy, custom:Tuning::default(), custom_defaults:Tuning::default(), custom_apply:false, custom_dirty:false, status: String::new(),
            maps: (0..40).map(|i| crate::map_library::Entry { label: format!("Map {i}"), path: None }).collect(),
            selected_map: 0, multiplayer: false, browser: false, network_page: 0, browser_count: 0, lan_browser: false, daylight: false, section: 0, section_focus: false, custom_sections: Vec::new(), map_detail: false, destinations: Vec::new(), pending_travel: None,
        };
        for section in 0..SECTIONS.len() {
            menu.select_section(section);
            let rows = menu.rows();
            assert!(rows.contains(&menu.selected));
            assert!(rows.windows(2).all(|pair| pair[0] < pair[1]));
            // Audio, controller identity, and frame-time rows are spawned.
            assert!(rows.iter().all(|id| *id < 20 || *id == FRAME_STATS_ROW || *id >= 1000));
        }
        menu.select_section(1);
        assert_eq!(menu.rows(),vec![3,CAMERA_ANGLE_ROW,8,10]);
        menu.custom_sections=vec![("Challenges".into(),vec![("test.mod".into(),"race".into(),"Race".into())])];
        menu.select_section(SECTIONS.len());
        assert_eq!(menu.rows(),vec![200]);
        menu.select_section(0);
        assert_eq!(menu.rows(), (1000..1040).collect::<Vec<_>>());
        menu.maps[0].path = Some(PathBuf::from("University.skate"));
        menu.destinations = vec![
            crate::teleport_menu::Destination { id: "uni".into(), name: "Campus".into(), map: "University".into(), matrix: Some([[0.;4];4]) },
            crate::teleport_menu::Destination { id: "dt".into(), name: "Downtown".into(), map: "DownTown".into(), matrix: Some([[0.;4];4]) },
        ];
        menu.map_detail = true;
        assert_eq!(menu.rows(), vec![50,51,1_000_000]);
        menu.select_section(0);
        menu.maps.clear();
        menu.select_section(0);
        assert_eq!(menu.selected, 100);
        assert!(menu.rows().is_empty());
        menu.select_section(3);
        assert!(menu.multiplayer);
        assert_eq!(menu.rows(), vec![2,6,13,14,15,16,17]);
        menu.network_page = 4;
        assert_eq!(menu.rows(), (20..27).chain([10]).collect::<Vec<_>>());
        menu.network_page = 1;
        assert_eq!(menu.rows(), vec![3,4,10]);
        menu.network_page = 3;
        assert_eq!(menu.rows(), vec![0,1,10]);
        menu.browser = true;
        menu.browser_count = 2;
        menu.lan_browser = true;
        assert_eq!(menu.rows(), vec![0,1,2,6,7,10]);
        menu.lan_browser = false;
        assert!(menu.rows().contains(&8));
        assert!(!SECTIONS.iter().any(|(name,_)| matches!(*name,"SESSION"|"WORLD")));
        menu.select_section(2);
        assert!(!menu.multiplayer && !menu.browser);
        assert_eq!(menu.rows(), vec![0,1,2,13,16,17,18,FRAME_STATS_ROW]);
        menu.daylight = true;
        assert_eq!(menu.rows(), vec![0,1,2,3]);
    }
    #[test]
    fn frame_counter_setting_defaults_off_and_round_trips() {
        assert!(!GraphicsSettings::default().frame_stats);
        // Settings files written before the row existed keep working.
        let old: GraphicsSettings = serde_json::from_str(r#"{"width":1920,"height":1080,"scale":100,"fps":0}"#).unwrap();
        assert!(!old.validated().frame_stats);
        let on = GraphicsSettings { frame_stats: true, ..default() };
        let saved: GraphicsSettings = serde_json::from_slice(&serde_json::to_vec(&on).unwrap()).unwrap();
        assert!(saved.validated().frame_stats);
        // A counter saved as on before Simple existed opens as Verbose.
        let old_on: GraphicsSettings = serde_json::from_str(r#"{"frame_stats":true}"#).unwrap();
        assert!(old_on.frame_stats && !old_on.frame_stats_simple);
        let simple = GraphicsSettings { frame_stats: true, frame_stats_simple: true, ..default() };
        let saved: GraphicsSettings = serde_json::from_slice(&serde_json::to_vec(&simple).unwrap()).unwrap();
        assert!(saved.frame_stats && saved.frame_stats_simple);
    }
    #[test]
    fn invalid_saved_values_fall_back() {
        let settings: GraphicsSettings =
            serde_json::from_str(r#"{"width":0,"height":999999,"scale":0,"fps":1}"#)
                .unwrap();
        assert_eq!(settings.validated(), GraphicsSettings::default());
    }
    #[test]
    fn scaled_target_and_cycle_boundaries() {
        let s = GraphicsSettings {
            scale: 50,
            ..default()
        };
        assert_eq!(
            s.internal_size(UVec2::new(1920, 1080)),
            UVec2::new(960, 540)
        );
        assert_eq!(s.internal_size(UVec2::ZERO), UVec2::ONE);
        assert_eq!(cycle(LIMITS, 0, -1), 240);
        assert_eq!(cycle(LIMITS, 240, 1), 0);
    }
    #[test]
    fn focus_moves_between_section_and_item_lists() {
        let mut menu = Menu {
            open: true, selected: 0, settings: GraphicsSettings::default(),
            path: PathBuf::new(), difficulty: Difficulty::Easy, custom:Tuning::default(), custom_defaults:Tuning::default(), custom_apply:false, custom_dirty:false, status: String::new(),
            maps: Vec::new(), selected_map: 0, multiplayer: false, browser: false, network_page: 0, browser_count: 0, lan_browser: false, daylight: false, section: 0, section_focus: false, custom_sections: Vec::new(), map_detail: false, destinations: Vec::new(), pending_travel: None,
        };
        // SKATER rows are 3, camera angle, 8, 10. Up and down wrap inside the item list.
        menu.select_section(1);
        assert!(menu.items_focused());
        let order: Vec<usize> = (0..5).map(|_| { menu.move_cursor(1); menu.selected }).collect();
        assert_eq!(order, vec![CAMERA_ANGLE_ROW, 8, 10, 3, CAMERA_ANGLE_ROW]);
        menu.move_cursor(-1);
        assert_eq!(menu.selected, 3);
        // Focusing the section list: up and down now change the section, and the item list follows.
        menu.focus_sections();
        assert!(!menu.items_focused());
        menu.move_cursor(1);
        assert_eq!((menu.section, menu.multiplayer), (2, false));
        assert_eq!(menu.selected, 0);
        assert!(!menu.items_focused());
        // Up from the first section wraps to the last.
        menu.select_section(0);
        menu.focus_sections();
        menu.move_cursor(-1);
        assert_eq!(menu.section, SECTIONS.len() - 1);
        // Focusing the items again: they keep their own cursor and wrap.
        menu.select_section(4);
        assert_eq!(menu.rows(), vec![7, 11, 14, 19]);
        assert_eq!(menu.selected, 7);
        menu.focus_items();
        assert!(menu.items_focused());
        for expected in [11, 14, 19, 7] {
            menu.move_cursor(1);
            assert_eq!(menu.selected, expected);
        }
        // An empty item list cannot take focus: the section list keeps it.
        menu.custom_sections = vec![("Empty".into(), Vec::new())];
        menu.select_section(SECTIONS.len());
        menu.focus_items();
        assert!(menu.rows().is_empty());
        assert!(!menu.items_focused());
        menu.move_cursor(1);
        assert_eq!(menu.section, 0);
    }
}

#[derive(Component)] struct CustomTrack(usize);
#[derive(Component)] struct CustomFill(usize);
fn custom_sliders(mut menu: ResMut<Menu>, mouse: Res<ButtonInput<MouseButton>>,
    tracks: Query<(&CustomTrack,&RelativeCursorPosition)>, mut fills: Query<(&CustomFill,&mut Node)>,
    mut dragging: Local<Option<usize>>) {
    if !menu.open || menu.section != 1 || menu.difficulty != Difficulty::Custom { *dragging=None; return; }
    if mouse.just_pressed(MouseButton::Left) {
        *dragging=tracks.iter().find(|(_,p)|p.cursor_over()).map(|(t,_)|t.0);
    }
    if !mouse.pressed(MouseButton::Left) { *dragging=None; }
    if let Some(index)=*dragging {
        if let Some((_,position))=tracks.iter().find(|(t,_)|t.0==index) {
            if let Some(pos)=position.normalized {
                menu.custom.set_fraction(index,pos.x); menu.custom_dirty=true; menu.selected=300+index; menu.focus_items();
            }
        }
    }
    for (fill,mut node) in &mut fills {node.width=percent(menu.custom.value(fill.0)/OPTIONS[fill.0].max*100.);}
}
fn apply_custom_difficulty(mut menu: ResMut<Menu>, config: Res<crate::config::Config>,
    mut skater: ResMut<crate::physics::SkaterRuntime>) {
    if !std::mem::take(&mut menu.custom_apply) {return;}
    let result=(|| -> Result<(),String> {
        let mut data=skate_data::collections::Collections::load(&config.asset_root)?;
        menu.custom.overlay(&mut data)?;
        skater.reload_difficulty(&data)?;
        menu.custom.save(&config.asset_root)?;
        Ok(())
    })();
    menu.status=match result {
        Ok(())=> {menu.custom_dirty=false;"Custom tuning applied and saved.".into()},
        Err(e)=>format!("Custom tuning: {e}"),
    };
}
