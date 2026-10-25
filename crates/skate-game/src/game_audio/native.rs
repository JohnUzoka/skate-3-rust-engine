//! Host for the native AEMS runtime (`crates/skate-audio`): retail's own patch programs and voice
//! graph. Always on (2026-10-03: the interim cue tables and their opt-outs `SKATE_AEMS=0` /
//! `"interim": true` are gone; docs/hails-additions/11-audio.md). An install without the data
//! (AEMS banks, MixMap, grain recordings, Splice trees) logs an error naming what is missing and
//! those sounds stay silent: there is no fallback.
//!
//! - At startup every Csis project is installed and `emitter_utility.abk` is loaded and posted
//!   (retail posts `c_emitter_utility` once at boot; it feeds the `*_snd` / `random_*_gbl` globals).
//! - Banks are loaded on first use, with their decoded WAVs as PCM, and unloaded on map change.
//! - The first runtime's projects and banks are parsed, installed and posted on `audio-runtime-start`
//!   after the separate early-bank loader finishes; the game thread installs the completed stream.
//!   Audio mods that require a runtime restart still use the synchronous restart path.
//! - One device-paced stream: rodio pulls 48 kHz stereo; each 256-frame block renders the voices
//!   and ticks the evaluator, so programs run on the audio clock exactly like retail. Game code
//!   posts, redelivers and releases between blocks under the runtime's lock.
//! - The stream follows the master volume (the AEMS voices × ambience, the rolling bed × effects)
//!   and pauses while the menu or a replay runs.
//! - The MixMap mixer (`MixMapSK8.mxb`, `skate_audio::mixmap`) runs on the game thread, clocked by
//!   the physics steps ([`HostClock`]: one host tick per step since the last frame, none while
//!   nothing steps or the game is silenced): [`mixmap_frame`] writes the inputs we can supply
//!   (category gains, the local player's physics and 3-D position, the emitter states' positions),
//!   ticks, and the systems read its outputs (the `c_emitter` words; the rolling bed's levels,
//!   pitch, filters and pan).
//!
//! Which systems use it: the skater's sounds (`player_audio.rs`), the `.ems` world emitters
//! (`emitters.rs`) and the granular rolling bed (`grain_bed.rs`). Location sets, zone beds and
//! crossfades still use interim Bevy playback; the zone fades and gains use native MixMap controls.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bevy::audio::{AddAudioSource, AudioPlayer, Decodable, PlaybackSettings, Source, Volume};
use bevy::prelude::*;
use skate_audio::eval::NodeId;
use skate_audio::formats::Project;
use skate_audio::mixmap::{MixMap, keys};
use skate_audio::runtime::Runtime;

use super::Library;

pub(crate) mod prefetch;
pub(crate) mod startup;

/// The host's step: one 60 Hz physics step. The host ticks once per physics step ([`HostClock`]);
/// the MixMap evaluates on every second one (the console's 30 Hz, `skate_audio::mixmap::cadence`).
pub(crate) const MIX_STEP: f32 = 1.0 / 60.0;
/// The most host ticks one frame takes (the steps beyond are dropped; their pulses stay latched).
/// Retail's audio manager runs once per rendered frame and never catches up: a long frame is one
/// call with its long dt (both halves once). Our host counts physics steps to stay on the console
/// grid at any frame rate, so it bounds the catch-up instead: 4 steps = 2 console evaluations,
/// enough for frame-rate independence down to 15 fps (the old Real-time accumulator's cap, 4 ×
/// [`MIX_STEP`]), while a hitch (Bevy runs up to 15 physics steps after a 250 ms frame) never
/// releases a burst of evaluations, Jitter steps or poster calls.
pub(crate) const MAX_STEPS_PER_FRAME: u32 = 4;

/// What a frame's host pass runs ([`HostClock::pass`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pass {
    /// Host ticks: the physics steps taken (capped at [`MAX_STEPS_PER_FRAME`]).
    pub ticks: usize,
    /// Console evaluations they complete (the console cadence).
    pub calls: usize,
    /// `sub_82491180` (the eEQChain clear) runs in this pass.
    pub clear_eq: bool,
}

/// The audio host's clock: the physics steps `skate_events::observe` published since the last
/// frame (2026-10-03, PR #32 review: before, a `Time<Real>` accumulator of its own drifted from
/// the physics grid after hitches, re-processed a stale sample on frames without a step, dropped
/// the first of two steps' pulses and kept ticking while the menu paused the game).
#[derive(Default)]
pub(crate) struct HostClock {
    /// The console's 30 Hz evaluation grid over the steps.
    cadence: skate_audio::mixmap::cadence::Cadence,
}

impl HostClock {
    /// Take the frame's steps. `None` (no pass: no inputs, process, tick or update) when nothing
    /// stepped, or while the game is silenced (menu, replay): those steps and their pulses are
    /// dropped, as the stream is paused.
    pub(crate) fn pass(&mut self, cues: &mut super::skate_events::Cues, silenced: bool) -> Option<Pass> {
        let steps = cues.take_steps();
        if steps == 0 || silenced {
            return None;
        }
        let ticks = steps.min(MAX_STEPS_PER_FRAME) as usize;
        // The console cadence (`skate_audio::mixmap::cadence`): retail's audio manager evaluates the
        // MixMap, steps the Jitter and clears the eEQChain buses once per 1/30 s console frame with
        // dt 1/30; here every second step, the flag inputs held in between.
        let calls = self.cadence.advance(ticks);
        // sub_82491180 in half 1 of the audio manager: every console frame (both halves run when the
        // frame is longer than 20 ms).
        let clear_eq = calls > 0;
        Some(Pass { ticks, calls, clear_eq })
    }
}
/// CSTATEMGR_Emitter's pool = the MixMap's Emitter instances.
pub(crate) const EMITTER_STATES: usize = 5;

/// The runtime, shared with the audio thread.
pub(crate) type Shared = Arc<Mutex<Runtime>>;

/// The asset rodio plays: an endless 48 kHz stereo stream pulled from the runtime.
#[derive(Asset, TypePath, Clone)]
pub(crate) struct NativeStream {
    shared: Shared,
}

pub(crate) struct NativeDecoder {
    shared: Shared,
    buffer: Vec<f32>,
    at: usize,
}

impl Iterator for NativeDecoder {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.at >= self.buffer.len() {
            match super::timing::lock(&self.shared, &super::timing::AUDIO_LOCK) {
                Ok(mut runtime) => {
                    {
                        let _render = super::timing::scope(&super::timing::RENDER);
                        runtime.fill_stereo(&mut self.buffer);
                    }
                    // The buffer is one block of stereo: one render per fill.
                    super::timing::block_load(&runtime);
                }
                Err(_) => self.buffer.fill(0.0),
            }
            self.at = 0;
        }
        let v = self.buffer[self.at];
        self.at += 1;
        Some(v)
    }
}

impl Source for NativeDecoder {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        2
    }
    fn sample_rate(&self) -> u32 {
        skate_audio::MIX_RATE
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

impl Decodable for NativeStream {
    type DecoderItem = f32;
    type Decoder = NativeDecoder;
    fn decoder(&self) -> NativeDecoder {
        // One block of stereo per lock.
        NativeDecoder { shared: self.shared.clone(), buffer: vec![0.0; 2 * skate_audio::BLOCK], at: usize::MAX }
    }
}

/// Marks the stream's player entity.
#[derive(Component)]
struct NativeOutput;

/// Tests: how many stream entities exist.
#[cfg(test)]
pub(crate) struct NativeOutputCount;
#[cfg(test)]
impl NativeOutputCount {
    pub(crate) fn of(world: &mut World) -> usize {
        world.query_filtered::<Entity, With<NativeOutput>>().iter(world).count()
    }
}

/// The running native runtime and what the game has loaded into it.
#[derive(Resource)]
pub(crate) struct Native {
    pub(crate) shared: Shared,
    /// Bank stem → runtime bank id.
    banks: HashMap<String, usize>,
    emitter_class: Option<usize>,
    /// The MixMap, when the install has `MixMapSK8.mxb`.
    pub(crate) mixmap: Option<MixMap>,
    /// The host's clock (the physics steps).
    clock: HostClock,
    /// This frame's host ticks (0: no pass), for the systems after [`mixmap_frame`] (the bed).
    pub(crate) frame_ticks: usize,
    /// Camera cuts seen (teleport, map change; [`mixmap_frame`]): the world hosts reset their own
    /// camera velocity when it changes.
    pub(crate) cuts: u64,
    /// The cut signal last seen (`Presentation::cuts`, the map generation) and the wheel sample
    /// of the step at the cut (no seam interpolation across it).
    cut_seen: Option<(u64, u64)>,
    cut_wheels: Option<[[f32; 3]; 4]>,
    /// The flag inputs are held between evaluations (set once, at the first pass).
    holds: bool,
    /// The local player's MixMap inputs and components (None without a MixMap).
    pub(crate) player: Option<super::player_audio::PlayerAudio>,
    /// Which emitter states (MixMap Emitter instances 0..4) are taken.
    emitter_states: [bool; EMITTER_STATES],
    /// The granular rolling bed's game-side state (None: no rolling sound; an error is logged).
    pub(crate) bed: Option<super::grain_bed::Bed>,
    /// World emitter banks read and decoded ahead of need on a worker thread ([`prefetch`]).
    pub(crate) prefetch: prefetch::Prefetch,
    /// Bumped by every [`Native::unload_map_banks`] (map change): the world / NPC hosts release
    /// their nodes and reset their objects when it changes (an owner that survives the change would
    /// otherwise keep redelivering to a node whose instances the unload destroyed).
    pub(crate) map_epoch: u64,
    /// The world / NPC owners' instance counts the MixMap was built with.
    pub(crate) world: WorldInstances,
    /// This frame's pass between [`mixmap_frame`] (inputs, the local player's process) and
    /// [`mixmap_tick`] (the ticks, the update): the world / NPC hosts' process runs in between, as
    /// retail runs every owner's process before its tick and the update after it.
    pub(crate) pending: Option<PendingPass>,
    /// Retail's front-end audio object (the `fe` records: the session marker's sounds; None on
    /// installs without them).
    pub(crate) frontend: Option<super::frontend::FrontendHost>,
    /// Banks audio content overlays load at start and keep across map changes (`preload`).
    resident: Vec<String>,
    /// The overlays' Csis projects installed in the runtime: (file, content stamp, registry token).
    pub(crate) mod_projects: Vec<(String, String, u64)>,
    /// The first start's banks, read while the scene built ([`startup`]); empty after the start.
    early: startup::Loaded,
}

/// The first AEMS runtime's CPU and file work, off the game thread. The worker only sees a
/// `Library::startup_view`; the live Library remains in the World for other systems.
#[derive(Resource)]
pub(super) struct BackgroundStart {
    thread: JoinHandle<Result<Native, String>>,
    started: Instant,
    projects: usize,
}

impl BackgroundStart {
    pub(super) fn finished(&self) -> bool {
        self.thread.is_finished()
    }

    fn join(self) -> (Result<Native, String>, usize, Duration) {
        let elapsed = self.started.elapsed();
        let result: Result<Native, String> = match self.thread.join() {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        };
        (result, self.projects, elapsed)
    }
}


/// A pass [`mixmap_frame`] began and [`mixmap_tick`] finishes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingPass {
    /// Console evaluations (MixMap ticks) in this pass.
    pub(crate) calls: usize,
    s: skate_audio::player::AudioState,
    speed_scale: Option<f32>,
    loose: u32,
    reverb: bool,
}

/// How many MixMap instances the world / NPC owners get: retail's free-skate layout
/// (`mixmap::RETAIL_INSTANCES`: 4 traffic, 15 pedestrian, 2 Player = the local player + 1 NPC
/// skater) or the opt-in non-retail "more audible" layout (settings/audio.json
/// `"more_audible_world": true`, user decision 2026-10-03; read at start): 8 traffic, 24
/// pedestrian, 4 Player (3 NPC / remote skaters). The MixMap is built with these counts, so the
/// extra objects get the same B lookups, distance curves and posts as retail's instances; only
/// their number is not retail. Instance 0 of every slot is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorldInstances {
    pub traffic: usize,
    pub peds: usize,
    /// Player-slot instances for NPC / remote skaters (instances 1..=npc).
    pub npc: usize,
}

impl WorldInstances {
    pub(crate) const RETAIL: Self = Self { traffic: 4, peds: 15, npc: 1 };
    pub(crate) const MORE_AUDIBLE: Self = Self { traffic: 8, peds: 24, npc: 3 };

    /// The MixMap's instances per slot.
    pub(crate) fn mixmap_instances(self) -> [usize; 14] {
        let mut n = skate_audio::mixmap::RETAIL_INSTANCES;
        n[skate_audio::world::keys::TRAFFIC as usize] = self.traffic;
        n[skate_audio::world::keys::PEDESTRIAN as usize] = self.peds;
        n[1] = 1 + self.npc;
        n
    }
}

impl Native {
    /// Start with retail's instance layout (the data-gated tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn start(library: &Library) -> Result<Self, String> {
        Self::start_with(library, WorldInstances::RETAIL)
    }

    pub(super) fn start_with(library: &Library, world: WorldInstances) -> Result<Self, String> {
        Self::start_from(library, world, 1)
    }

    /// Start, the evaluator handing out post ids from `first_node` on (1 = a fresh runtime; a
    /// restart continues the old runtime's counter, `content::restart`).
    pub(super) fn start_from(library: &Library, world: WorldInstances, first_node: u32) -> Result<Self, String> {
        Self::start_early(library, world, first_node, startup::Loaded::default())
    }

    /// [`Native::start_from`] taking the banks `early` holds from their sources instead of reading
    /// them again (the first start, [`startup`]).
    pub(super) fn start_early(library: &Library, world: WorldInstances, first_node: u32, early: startup::Loaded) -> Result<Self, String> {
        let files = library.aems();
        if files.projects.is_empty() {
            return Err("this install has no AEMS banks (run setup to refresh the audio)".into());
        }
        let mut runtime = Runtime::new();
        runtime.eval.continue_nodes(first_node);
        let mut mod_projects = Vec::new();
        for file in &files.projects {
            let bytes = library.read(file).map_err(|e| format!("{file}: {e}"))?;
            let project = Project::parse(file, &bytes).map_err(|e| e.to_string())?;
            let token = runtime.install_project(&project);
            // An overlay's project (doc 16 "Mod Csis projects"): after the install's, in mod-id order.
            if skate_mods::audio_merge::split_mod_ref(file).is_some() {
                mod_projects.push((file.clone(), library.stamp(file), token));
            }
        }
        let mixmap = match &files.mixmap {
            Some(file) => {
                let bytes = library.read(file).map_err(|e| format!("{file}: {e}"))?;
                let file = skate_audio::mixmap::MixMapFile::parse(&bytes).map_err(|e| e.to_string())?;
                let m = MixMap::new(&file, &world.mixmap_instances());
                if world != WorldInstances::RETAIL {
                    info!("Game audio: more audible world (not retail): {} traffic, {} pedestrian, {} NPC skater instances", world.traffic, world.peds, world.npc);
                }
                info!("Game audio: MixMap {} controllers ({} output blocks)", m.controller_count(), m.output_blocks());
                Some(m)
            }
            None => {
                error!("Game audio: this install has no MixMapSK8.mxb: the skater's sounds and rolling are silent and emitter words use defaults (run setup to refresh the audio)");
                None
            }
        };
        let bed = if mixmap.is_some() { super::grain_bed::Bed::new(library) } else { None };
        let player = mixmap.as_ref().map(|_| {
            super::player_audio::PlayerAudio::new(library.player_tuning(), true)
        });
        if bed.is_none() {
            error!("Game audio: no MixMap, whole grain recordings or grain tuning in this install: rolling is silent (run setup to refresh the audio)");
        }
        let mut native = Self {
            shared: Arc::new(Mutex::new(runtime)),
            banks: HashMap::new(),
            emitter_class: None,
            mixmap,
            clock: HostClock::default(),
            frame_ticks: 0,
            cuts: 0,
            cut_seen: None,
            cut_wheels: None,
            holds: false,
            player,
            emitter_states: [false; EMITTER_STATES],
            bed,
            prefetch: Default::default(),
            map_epoch: 0,
            world,
            pending: None,
            frontend: None,
            resident: Vec::new(),
            mod_projects,
            early,
        };
        // The environment (reverb) network and the eEQChain buses (optional install data).
        let (presets, eq) = library.bus_tuning();
        if !presets.is_empty() || !eq.is_empty() {
            let mut runtime = native.shared.lock().map_err(|_| "audio lock poisoned")?;
            info!("Game audio: native buses on ({} reverb presets, {} eEQChain buses)", presets.len(), eq.len());
            runtime.mixer.buses.env.presets = presets;
            runtime.mixer.buses.eq.set_records(&eq);
        }
        // The FlangeSub effect returns (GRINDS and the skid send into return A).
        if let Some([a, b]) = library.flange_presets() {
            let mut runtime = native.shared.lock().map_err(|_| "audio lock poisoned")?;
            info!("Game audio: native FlangeSub returns on");
            runtime.mixer.buses.flange.set_presets(a, b);
        }
        // The FootStep SubMix graphs (`sub_82494188`: each foot sound through its EQ record, env
        // send and panner).
        if let Ok(mut runtime) = native.shared.lock() {
            runtime.mixer.buses.submix.enabled = true;
        }
        native.ensure_bank(library, "emitter_utility")?;
        native.load_player_banks(library);
        let mut runtime = native.shared.lock().map_err(|_| "audio lock poisoned")?;
        let utility = runtime.eval.class_id("c_emitter_utility").ok_or("no c_emitter_utility class")?;
        runtime.post(utility, &[]);
        drop(runtime);
        // Retail's second boot utility (Start_up_Play_ctl, Common.abk): the Seams program's sample
        // shuffles. Older installs without Common.abk keep the fixed seam samples.
        match native.ensure_bank(library, skate_audio::player::seams::UTILITY_BANK) {
            Ok(_) => {
                let mut runtime = native.shared.lock().map_err(|_| "audio lock poisoned")?;
                if let Some(class) = runtime.eval.class_id(skate_audio::player::seams::UTILITY) {
                    runtime.post(class, &[]);
                }
            }
            Err(e) => warn!("Game audio: {e}; seam hits play fixed samples (rerun setup)"),
        }
        // Then the foley utility (the cloth_trick programs call it): retail's boot order
        // c_emitter_utility → Start_up_Play_ctl → c_foley_utility, which matters because every
        // program shares one random generator.
        if native.player.as_ref().is_some_and(|p| p.components && p.tricks_on) {
            let mut runtime = native.shared.lock().map_err(|_| "audio lock poisoned")?;
            if let Some(id) = runtime.eval.class_id(skate_audio::player::tricks::FOLEY_UTILITY) {
                runtime.post(id, &[]);
            }
        }
        let runtime = native.shared.lock().map_err(|_| "audio lock poisoned")?;
        native.emitter_class = runtime.eval.class_id("c_emitter");
        drop(runtime);
        // The front-end sounds (the session marker's cellphone UI; sk8_menu).
        native.frontend = super::frontend::FrontendHost::load(&native, library);
        native.load_resident_banks(library);
        std::mem::take(&mut native.early).finish();
        Ok(native)
    }

    /// The banks audio content overlays mark `preload`, after the retail boot (none without
    /// overlays): loaded now, in their volume group, and kept across map changes.
    fn load_resident_banks(&mut self, library: &Library) {
        for (stem, player) in library.resident_banks() {
            match self.ensure_bank(library, &stem) {
                Ok(id) => {
                    if let Ok(mut runtime) = self.shared.lock() {
                        runtime.mixer.set_bank_group(id, if player { skate_audio::mixer::GROUP_PLAYER } else { skate_audio::mixer::GROUP_WORLD });
                    }
                    self.resident.push(stem);
                }
                Err(e) => warn!("Game audio: mod bank {stem}: {e}"),
            }
        }
    }

    /// Replace a loaded bank in place from the library (an audio content hot swap, `swap.rs`): the
    /// same runtime id and constructor places, the new program and samples, held posts re-bound
    /// (`Runtime::replace_bank`); its volume group from the overlay, else the one it was loaded
    /// with (the player's banks: the player group). False when the bank is not loaded (it loads
    /// on use).
    pub(crate) fn replace_bank(&mut self, library: &Library, stem: &str) -> Result<bool, String> {
        let Some(&id) = self.banks.get(stem) else { return Ok(false) };
        self.prefetch.drop_bank(stem);
        let (bank, pcm) = library.bank_source(stem)?.load()?;
        let player = super::player_audio::BANKS.contains(&stem) || super::player_audio::OPTIONAL_BANKS.iter().any(|b| b.contains(&stem));
        let mut runtime = self.shared.lock().map_err(|_| "audio lock poisoned")?;
        runtime.replace_bank(id, bank, pcm);
        let group = library.bank_group(stem).unwrap_or(player);
        runtime.mixer.set_bank_group(id, if group { skate_audio::mixer::GROUP_PLAYER } else { skate_audio::mixer::GROUP_WORLD });
        Ok(true)
    }

    /// Unload one bank (an audio content hot swap: the bank left the audio).
    pub(crate) fn unload_bank(&mut self, stem: &str) {
        self.prefetch.drop_bank(stem);
        self.resident.retain(|r| r != stem);
        if let Some(id) = self.banks.remove(stem) {
            if let Ok(mut runtime) = self.shared.lock() {
                runtime.unload_bank(id);
            }
        }
    }

    /// The overlays' preloaded banks of a new library (an audio content hot swap): the new ones
    /// load now; ones no longer preloaded go at the next map change like any map bank.
    pub(crate) fn set_resident(&mut self, library: &Library) {
        self.resident.clear();
        self.load_resident_banks(library);
    }

    /// The banks the runtime holds: (stem, runtime id), sorted by stem.
    pub(crate) fn bank_ids(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self.banks.iter().map(|(s, &id)| (s.clone(), id)).collect();
        v.sort();
        v
    }

    /// The id the runtime's next post gets (a restart continues from it).
    pub(crate) fn next_node(&self) -> u32 {
        self.shared.lock().map_or(1, |r| r.eval.next_node())
    }

    /// Load a bank (and its samples) unless it is loaded already. A bank the prefetch worker has
    /// read and decoded is taken from it (waiting if it is mid-decode), and at the first start one
    /// the start-up thread read from the same source ([`startup`]); otherwise it is read and
    /// decoded here, by the same `BankSource::load`. Either way `load_bank` runs now.
    pub(crate) fn ensure_bank(&mut self, library: &Library, stem: &str) -> Result<usize, String> {
        if let Some(&id) = self.banks.get(stem) {
            return Ok(id);
        }
        let (bank, pcm) = match self.prefetch.take(stem) {
            Some(loaded) => loaded,
            None => {
                let source = library.bank_source(stem)?;
                match self.early.take_bank(&source) {
                    Some(loaded) => loaded,
                    None => source.load()?,
                }
            }
        };
        let mut runtime = self.shared.lock().map_err(|_| "audio lock poisoned")?;
        let id = runtime.load_bank(bank, pcm);
        // An overlay can put a bank in a volume group (none without overlays).
        if let Some(player) = library.bank_group(stem) {
            runtime.mixer.set_bank_group(id, if player { skate_audio::mixer::GROUP_PLAYER } else { skate_audio::mixer::GROUP_WORLD });
        }
        drop(runtime);
        self.banks.insert(stem.to_owned(), id);
        Ok(id)
    }

    /// The player components' banks, in the player volume group; without them the components
    /// stay off and the skater's sounds are silent.
    fn load_player_banks(&mut self, library: &Library) {
        if !self.player.as_ref().is_some_and(|p| p.components) {
            return;
        }
        for stem in super::player_audio::BANKS {
            match self.ensure_bank(library, stem) {
                Ok(id) => {
                    if let Ok(mut runtime) = self.shared.lock() {
                        runtime.mixer.set_bank_group(id, skate_audio::mixer::GROUP_PLAYER);
                    }
                }
                Err(e) => {
                    error!("Game audio: the skater's sounds are silent ({e}; run setup to refresh the audio)");
                    if let Some(p) = &mut self.player {
                        p.components = false;
                    }
                    return;
                }
            }
        }
        info!("Game audio: native player components on (Class_grind, SenseOfSpeed, Class_foot_drag)");
        self.load_optional_player_banks(library);
        // The Splice banks (pops, landings, touchdowns), decoded up front so a sample's first
        // trigger sounds like every later one.
        let mut first = false;
        for (i, stem) in super::player_audio::SPLICE_BANKS.iter().enumerate() {
            let loaded = library.splice_source(stem).and_then(|source| self.early.take_splice(stem, &source).or_else(|| source.load()));
            match loaded {
                Some((bank, pcm)) => {
                    if let Ok(mut runtime) = self.shared.lock() {
                        let rt = &mut *runtime;
                        rt.splice.load_bank(stem, bank, pcm, &mut rt.mixer);
                        first |= i == 0;
                    }
                }
                None => error!("Game audio: {stem} has no patch tree in this install: its sounds are silent (run setup to refresh the audio)"),
            }
        }
        // SFXObj_Wheels' spin-down recordings, decoded now (first trigger = later triggers).
        let streams: Vec<_> = super::player_audio::WHEEL_STREAMS
            .iter()
            .map(|n| {
                let file = library.wheels_source(n)?;
                self.early.take_wheel(n, &file).or_else(|| super::library::wheels_load(&file))
            })
            .collect();
        let wheels = streams.iter().all(Option::is_some);
        if let Ok(mut runtime) = self.shared.lock() {
            runtime.load_streams(streams);
        }
        if let Some(p) = &mut self.player {
            p.contacts_on = first;
            p.set_footstep_materials(library.footstep_materials());
            p.footsteps_on = first;
            p.contact_tuning = library.contacts_tuning();
            p.wheels_on = wheels;
            if p.contacts_on {
                info!("Game audio: native board contacts on (Splice: pops, landings, touchdowns, foot taps, scuffs; collision pairs: {} materials)", p.tuning.collision.materials.len());
            }
            if wheels {
                info!("Game audio: native wheel spin on (SFXObj_Wheels)");
            }
        }
    }

    /// The optional components' banks (rolling layers, rattle, board slide, tricks, treatment):
    /// each component runs when its first bank is in the install; missing later banks only drop
    /// their layers (a Class_rolling post reaches every bank bound to the class).
    fn load_optional_player_banks(&mut self, library: &Library) {
        use super::player_audio::{RATTLE_BANKS, ROLLING_BANKS, SLIDE_BANKS, TREATMENT_BANKS, TRICKS_BANKS};
        let load = |me: &mut Self, banks: &[&str]| -> bool {
            let mut first = false;
            for (i, stem) in banks.iter().enumerate() {
                match me.ensure_bank(library, stem) {
                    Ok(id) => {
                        if let Ok(mut runtime) = me.shared.lock() {
                            runtime.mixer.set_bank_group(id, skate_audio::mixer::GROUP_PLAYER);
                        }
                        first |= i == 0;
                    }
                    Err(e) => warn!("Game audio: {e}; its native layers stay off (run setup to refresh the audio)"),
                }
            }
            first
        };
        let rolling = load(self, ROLLING_BANKS);
        let rattle = load(self, RATTLE_BANKS);
        let slide = load(self, SLIDE_BANKS);
        let tricks = load(self, TRICKS_BANKS);
        let treatment = load(self, TREATMENT_BANKS);
        // With the tricks the foley utility is posted at boot, after Start_up_Play_ctl (`start`).
        if let Some(p) = &mut self.player {
            (p.rolling_on, p.rattle_on, p.slide_on, p.tricks_on, p.treatment_on) = (rolling, rattle, slide, tricks, treatment);
            info!("Game audio: native rolling layers {rolling}, rattle {rattle}, board slide {slide}, tricks {tricks}, treatment {treatment}");
        }
    }

    /// The runtime's id of a loaded bank.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn bank_id(&self, stem: &str) -> Option<usize> {
        self.banks.get(stem).copied()
    }

    /// Whether the runtime holds the bank.
    pub(crate) fn bank_loaded(&self, stem: &str) -> bool {
        self.banks.contains_key(stem)
    }

    /// The loaded banks' stems, sorted (the audio catalog).
    pub(crate) fn loaded_banks(&self) -> Vec<String> {
        let mut v: Vec<String> = self.banks.keys().cloned().collect();
        v.sort();
        v
    }

    /// A runtime the tests built by hand (no MixMap, no player).
    #[cfg(test)]
    pub(crate) fn for_test(runtime: Runtime) -> Self {
        let runtime = Arc::new(Mutex::new(runtime));
        let emitter_class = runtime.lock().unwrap().eval.class_id("c_emitter");
        Self {
            shared: runtime,
            banks: HashMap::new(),
            emitter_class,
            mixmap: None,
            clock: HostClock::default(),
            frame_ticks: 0,
            cuts: 0,
            cut_seen: None,
            cut_wheels: None,
            holds: false,
            player: None,
            emitter_states: [false; EMITTER_STATES],
            bed: None,
            prefetch: Default::default(),
            map_epoch: 0,
            world: WorldInstances::RETAIL,
            pending: None,
            frontend: None,
            resident: Vec::new(),
            mod_projects: Vec::new(),
            early: Default::default(),
        }
    }

    /// Tests: a pass of `calls` console evaluations is pending (what `mixmap_frame` leaves for the
    /// systems after the ticks), without the player's inputs.
    #[cfg(test)]
    pub(crate) fn test_pass(&mut self, calls: usize) {
        self.pending = Some(PendingPass { calls, s: Default::default(), speed_scale: None, loose: 0, reverb: false });
    }

    /// Tests outside `game_audio` (the mod system's): start the runtime for `library`.
    #[cfg(test)]
    pub(crate) fn start_for_test(library: &Library) -> Result<Self, String> {
        Self::start(library)
    }

    /// Unload every bank but the utility and the player's (map change); forget the prefetched ones.
    pub(crate) fn unload_map_banks(&mut self) {
        self.map_epoch += 1;
        self.prefetch.clear();
        let Ok(mut runtime) = self.shared.lock() else { return };
        let resident = &self.resident;
        self.banks.retain(|stem, id| {
            let keep = stem == "emitter_utility" || stem == skate_audio::player::seams::UTILITY_BANK || super::player_audio::BANKS.contains(&stem.as_str()) || super::player_audio::OPTIONAL_BANKS.iter().any(|b| b.contains(&stem.as_str()))
                || resident.iter().any(|r| r == stem);
            if !keep {
                runtime.unload_bank(*id);
            }
            keep
        });
    }

    pub(crate) fn has_bank(&self, library: &Library, stem: &str) -> bool {
        library.aems().banks.contains_key(stem)
    }

    /// Post to `c_emitter` (payload words: level, dry, send, azimuth, pitch, low-pass, high-pass,
    /// unused, selector).
    pub(crate) fn post_emitter(&self, payload: &[i32; 9]) -> Option<NodeId> {
        let class = self.emitter_class?;
        Some(self.shared.lock().ok()?.post(class, payload))
    }

    /// Take a free emitter state (0..4), if any.
    pub(crate) fn claim_emitter_state(&mut self) -> Option<usize> {
        let g = self.emitter_states.iter().position(|used| !used)?;
        self.emitter_states[g] = true;
        Some(g)
    }

    /// Free an emitter state; its 3-D input goes inactive.
    pub(crate) fn release_emitter_state(&mut self, g: usize) {
        if let Some(used) = self.emitter_states.get_mut(g) {
            *used = false;
        }
        if let Some(m) = &mut self.mixmap {
            m.set_input(keys::emitter_pos(g as u32), keys::pos::FLAGS, 0);
        }
    }

    /// The emitter state's SFXCTL 3-D input (what B0 reads: camera distance and azimuth). Who
    /// writes it in retail is not traced (mixmap-spec §10); we follow the 3DObjPos layout (§7.3).
    pub(crate) fn set_emitter_position(&mut self, g: usize, listener: &GlobalTransform, skater: Vec3, source: Vec3) {
        let Some(m) = &mut self.mixmap else { return };
        write_position(m, keys::emitter_pos(g as u32), listener, skater, source);
    }

    /// The positional `c_emitter` payload from the MixMap (mixmap-spec §6.3, `sub_824DCF08`):
    /// w1 dry = out4 × level, w2 send = out8 × level, w3 pan = out0 (raw azimuth of B0), w4 pitch
    /// = out5 through the pitch reader, w5 low-pass = out6; w8 = the attribute patch. Without a
    /// MixMap (or a state) the old defaults apply: dry = level, send 0, pitch 4096, 25 kHz.
    pub(crate) fn emitter_payload(&self, g: Option<usize>, level: f32, azimuth: i32, patch: i32) -> [i32; 9] {
        let level = level.clamp(0.0, 1.0);
        let patch = patch.clamp(0, 500);
        match (&self.mixmap, g) {
            (Some(m), Some(g)) => {
                let key = keys::emitter(g as u32);
                let scaled = |id: usize| (m.level(key, id) as f32 * level) as i32;
                [32767, scaled(4), scaled(8), m.raw(key, 0), m.pitch_4096(key, 5), m.filter_hz(key, 6), 0, 0, patch]
            }
            _ => [32767, (level * 32767.0).round() as i32, 0, azimuth, 4096, 25000, 0, 0, patch],
        }
    }

    pub(crate) fn redeliver(&self, node: NodeId, payload: &[i32]) {
        if let Ok(mut runtime) = self.shared.lock() {
            runtime.redeliver(node, payload);
        }
    }

    pub(crate) fn release(&self, node: NodeId) {
        if let Ok(mut runtime) = self.shared.lock() {
            runtime.release(node);
        }
    }
}

pub(crate) fn register(app: &mut App) {
    // The runtime starts in `content::frame` (after the first mod scan, so an audio mod enabled at
    // boot costs no second start), and restarts there when the running mods' audio content changes.
    app.add_audio_source::<NativeStream>()
        .add_systems(PostUpdate, follow_volume);
}

/// Write an emitter state's 3DObjPos-style input block (mixmap-spec §7.3): in0 = f32 distance
/// from the skater, in1 = f32 distance from the camera itself, in2 / in3 = azimuths (u16 scale,
/// both in the camera frame here: who writes the emitter blocks in retail is not traced, §10),
/// in15 bit 0 = active. The emitters are static: relative speeds (in13/14) stay 0.
pub(crate) fn write_position(m: &mut MixMap, key: u32, listener: &GlobalTransform, skater: Vec3, source: Vec3) {
    let az = azimuth(listener, source);
    m.set_input_f32(key, keys::pos::DIST_SKATER, skater.distance(source));
    m.set_input_f32(key, keys::pos::DIST_CAMERA, listener.translation().distance(source));
    m.set_input(key, keys::pos::AZ_SKATER, az);
    m.set_input(key, keys::pos::AZ_CAMERA, az);
    m.set_input(key, keys::pos::FLAGS, 1);
}

/// The one-frame flag inputs the console's writers set for a whole evaluation, held between our 60
/// Hz writes and the 30 Hz evaluations ([`MixMap::hold_input`]): Contacts 1 / 6 (the landing
/// swell and the landing-material flag), Rail 1 (the grind ended), SkateBoard 0 / 4 (the surface
/// change, the push plant), Cracks 0 (a seam hit). Everything else the host writes is a level that
/// holds between writes.
pub(crate) fn hold_flag_inputs(m: &mut MixMap) {
    for (key, id) in [(keys::contacts(0), 1), (keys::contacts(0), 6), (keys::rail(0), 1), (keys::skateboard(0), 0), (keys::skateboard(0), 4), (keys::cracks(0), 0)] {
        m.hold_input(key, id);
    }
}

/// The host pass of a frame (spec §1: inputs → components' process → tick → components' update),
/// on frames that took physics steps ([`HostClock::pass`]: as many host ticks as steps, at most
/// [`MAX_STEPS_PER_FRAME`]; none on a frame without a step, none while silenced). Written here:
/// Master.in1–4 (in1..3 = the option volumes, `super::RetailVolumes`), Music.in1/2/5, Reverb.in0..6 (free-skate values, §7.4); through `player_audio`:
/// PlayerPhysics 0–14, the two 3DObjPos blocks (skater COM and board, with relative speeds and the
/// sign-flip bits), Jitter, Contacts 1/2/6, Rail 0/1, OffBoard 0; the board owner inputs
/// (`grain_bed.rs`). Left at their free-skate 0: VU (no output meter; only the ambience reads it,
/// not native yet), Menu / NIS / HOM / Challenge / Speech flags (no such modes in free skate),
/// HandGrabs, Music.in3/6 (combo emphasis, not wired), and Pause.in0: nothing ticks while the menu
/// or a replay silences the game (the stream itself is paused), so no evaluation would read it.
/// Class_Seams' console cadence runs on every rendered frame that is not silenced
/// ([`PlayerAudio::seam_frame`](super::player_audio::PlayerAudio::seam_frame), game time).
#[allow(clippy::too_many_arguments)]
pub(super) fn mixmap_frame(
    native: Option<ResMut<Native>>,
    mut cues: ResMut<super::skate_events::Cues>,
    listener: Query<&GlobalTransform, With<super::GameAudioListener>>,
    time: Res<Time<Virtual>>,
    fixed: Res<Time<Fixed>>,
    menu: Option<Res<crate::graphics_menu::Menu>>,
    replay: Res<crate::replay::Replay>,
    cuts: (Option<Res<crate::presentation::Presentation>>, Option<Res<crate::map_transition::CurrentMap>>),
    teleport: Option<Res<crate::ui_audio::TeleportEffect>>,
    settings: Option<Res<super::AudioSettings>>,
) {
    let _timing = super::timing::scope(&super::timing::MIXMAP_FRAME);
    let silenced = super::silenced(menu.as_deref(), &replay);
    let Some(mut native) = native else {
        cues.take_steps();
        return;
    };
    let native = &mut *native;
    // For the systems after this one (the bed): no pass until the clock says so.
    native.frame_ticks = 0;
    native.pending = None;
    if let Some(bed) = &mut native.bed {
        bed.slew_calls = Some(0);
    }
    if native.mixmap.is_none() {
        cues.take_steps();
        return;
    }
    // A teleport, camera cut or map change: no camera velocity across it (Doppler), and no seam
    // interpolation from the old place (the step at the cut is both ends of the pair).
    let signal = (cuts.0.as_deref().map_or(0, |p| p.cuts), cuts.1.as_deref().map_or(0, |m| m.generation));
    let now = cues.riding.audio.wheel_position;
    if native.cut_seen.is_some_and(|seen| seen != signal) {
        native.cuts += 1;
        native.cut_wheels = Some(now);
        if let Some(player) = &mut native.player {
            player.reset_listener();
        }
    }
    native.cut_seen = Some(signal);
    if native.cut_wheels.is_some_and(|w| w != now) {
        native.cut_wheels = None;
    }
    let before = if native.cut_wheels.is_some() { now } else { cues.riding.wheels_before };
    // Class_Seams on the console's 30 fps process cadence at the rendered board's wheels, on every
    // rendered frame (Listening test 9), in game time (`Time<Virtual>`: still while paused).
    let Some(m) = &mut native.mixmap else { return };
    if let Some(player) = &mut native.player {
        player.seam_alpha = Some(fixed.overstep_fraction());
        player.step_wheels(before, now);
        if !silenced {
            if let Ok(mut runtime) = super::timing::lock(&native.shared, &super::timing::GAME_LOCK) {
                player.seam_frame(m, &cues.riding.audio, time.delta_secs(), &mut runtime);
            }
        }
    }
    let Some(pass) = native.clock.pass(&mut cues, silenced) else { return };
    native.frame_ticks = pass.ticks;
    // Master.in1..in3 = the option volumes (Settings menu Music / SFX / Dialog) as retail's Master
    // step `sub_824D5160` writes them every pass; in4 is not an option volume (32767 in free skate).
    let volumes = settings.as_deref().map_or_else(|| super::RetailVolumes::default().master_inputs(), super::AudioSettings::master_inputs);
    for (id, value) in volumes {
        m.set_input(keys::MASTER, id, value);
    }
    m.set_input(keys::MASTER, 4, 32767);
    for id in [1, 2, 5] {
        m.set_input(keys::MUSIC, id, 32767);
    }
    // SFXObj_Reverb's first step (`sub_824DF468`): Reverb.in0..6 from the number of the preset
    // being faded to (in5 = reverb11/12/15/16/22, which ducks the global env scale out4 by 4 dB).
    // (Without the runtime lock: in5 = 32767 and no scale this pass.)
    let reverb = super::timing::lock(&native.shared, &super::timing::GAME_LOCK).ok().map(|r| r.mixer.buses.env.reverb_inputs());
    match reverb {
        Some(v) => {
            for (id, x) in v.into_iter().enumerate() {
                m.set_input(keys::REVERB, id, x);
            }
        }
        None => m.set_input(keys::REVERB, 5, 32767),
    }
    m.set_input(keys::PAUSE, 0, 0);
    let (ticks, calls) = (pass.ticks, pass.calls);
    // The bed's turn / brake slews step once per console evaluation (`grain_bed::Bed::slew_calls`).
    if let Some(bed) = &mut native.bed {
        bed.slew_calls = Some(calls);
    }
    if !native.holds {
        hold_flag_inputs(m);
        native.holds = true;
    }
    // sub_82491180 (half 1): the jittered eEQChain buses take the walk's values and every bus may
    // re-roll again.
    if pass.clear_eq {
        let jitter = native.player.as_ref().and_then(|p| p.eq_jitter());
        if let Ok(mut runtime) = native.shared.lock() {
            runtime.mixer.buses.eq.clear(jitter);
        }
    }
    let s = cues.riding.audio;
    if let Some(player) = &mut native.player {
        let dt = ticks as f32 * MIX_STEP;
        let l = listener.single().ok().map(|t| player.listener(t.translation().to_array(), t.forward().as_vec3().to_array(), dt, &s));
        // SFXObj_Jitter's walk steps once per console evaluation (half 1's process).
        player.jitter_steps = Some(calls);
        // The presentation block's teleport field as of this pass (Class_Treatment's update reads it).
        player.teleport_effect = teleport.as_deref().and_then(crate::ui_audio::TeleportEffect::amount);
        player.write_inputs(m, &s, l.as_ref());
    }
    // With the native rolling layers the owner's surface routing (player::rolling) writes
    // SkateBoard inputs 0 / 6 and drives the bed's binds; otherwise the bed routes itself.
    let routing = native.player.as_ref().is_some_and(|p| p.components && p.rolling_on);
    if let Some(bed) = &mut native.bed {
        bed.write_inputs(m, &s, !routing);
    }
    let speed_scale = native.bed.as_ref().and_then(|b| b.push_scale());
    let loose = super::player_audio::PlayerAudio::loose_board(&s, &cues.riding);
    if let (Some(player), Ok(mut runtime)) = (&mut native.player, super::timing::lock(&native.shared, &super::timing::GAME_LOCK)) {
        player.process(m, &s, &mut runtime, speed_scale, loose);
    }
    // The world / NPC owners' process runs next (`world_sources::pre`, `npc_skaters::pre`), then
    // [`mixmap_tick`].
    native.pending = Some(PendingPass { calls, s, speed_scale, loose, reverb: reverb.is_some() });
}

/// The second half of [`mixmap_frame`]'s pass, after the world / NPC owners' process: the console
/// evaluations (MixMap ticks), the local player's update and SFXObj_Reverb's update. The world /
/// NPC owners' update follows (`world_sources::post`, `npc_skaters::post`).
pub(super) fn mixmap_tick(native: Option<ResMut<Native>>, inputs: Option<ResMut<super::mixmap_inputs::MixMapInputs>>, content: Option<Res<super::AudioContent>>) {
    let _timing = super::timing::scope(&super::timing::MIXMAP_FRAME);
    let Some(mut native) = native else { return };
    let native = &mut *native;
    let Some(PendingPass { calls, s, speed_scale, loose, reverb }) = native.pending else { return };
    let Some(m) = &mut native.mixmap else { return };
    // Mods' MixMap input writes (doc 16 L2), after every host write of the pass, before the
    // evaluations (nothing without writes).
    if let Some(mut inputs) = inputs {
        inputs.apply(m, content.map_or(0, |c| c.runtime_generation));
    }
    for _ in 0..calls {
        m.tick(skate_audio::mixmap::cadence::CONSOLE_DT);
    }
    if let (Some(player), Ok(mut runtime)) = (&mut native.player, super::timing::lock(&native.shared, &super::timing::GAME_LOCK)) {
        player.update(m, &s, &mut runtime, speed_scale, loose);
    }
    // SFXObj_Reverb's update (`sub_824DF220`): the FlangeSub returns' levels and the global env
    // scale (manager +104 = out4) from the Reverb owner.
    if let Ok(mut runtime) = super::timing::lock(&native.shared, &super::timing::GAME_LOCK) {
        runtime.mixer.buses.flange.frame(std::array::from_fn(|i| m.level(keys::REVERB, i)));
        if reverb {
            runtime.mixer.buses.env.scale_frame(m.level(keys::REVERB, 4));
        }
    }
}

/// What a (re)start carries over from the runtime it replaces.
#[derive(Clone, Copy, Debug)]
pub(super) struct Carry {
    /// The old evaluator's next post id: ids it handed out never name a post of the new one.
    pub first_node: u32,
    /// The new runtime's `map_epoch` (old + 1: every host holding nodes resets).
    pub epoch: u64,
    pub world: WorldInstances,
}

/// Start the runtime and its output stream synchronously (restarts, and the thread-creation
/// fallback). The first start uses [`start_background`] while its worker is alive.
pub(super) fn launch(world: &mut World, carry: Option<Carry>) -> bool {
    let early = world.remove_resource::<startup::EarlyLoads>().map(startup::EarlyLoads::join).unwrap_or_default();
    launch_with_early(world, carry, early)
}

/// Synchronous fallback when the worker thread can't be created. `early` has already been joined.
pub(super) fn launch_with_early(world: &mut World, carry: Option<Carry>, early: startup::Loaded) -> bool {
    let Some(settings) = world.get_resource::<super::AudioSettings>() else { return false };
    let instances = carry.map_or_else(|| if settings.more_audible_world() { WorldInstances::MORE_AUDIBLE } else { WorldInstances::RETAIL }, |c| c.world);
    let Some(library) = world.get_resource::<Library>() else { return false };
    init_dsp();
    let started = Instant::now();
    let projects = library.aems().projects.len();
    let result = Native::start_early(library, instances, carry.map_or(1, |c| c.first_node), early);
    install(world, result, projects, started.elapsed(), carry.map(|c| c.epoch), false)
}

/// Start initial audio off-thread; returns the already-loaded data if spawning fails, so the caller
/// can retain the old synchronous fallback without losing or decoding the banks twice.
pub(super) fn start_background(
    library: Library,
    instances: WorldInstances,
    early: startup::Loaded,
) -> Result<BackgroundStart, (std::io::Error, startup::Loaded)> {
    let projects = library.aems().projects.len();
    let started = Instant::now();
    let early = Arc::new(Mutex::new(Some(early)));
    let worker_early = Arc::clone(&early);
    let thread = thread::Builder::new().name("audio-runtime-start".into()).spawn(move || {
        let early = worker_early.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default();
        init_dsp();
        Native::start_early(&library, instances, 1, early)
    });
    match thread {
        Ok(thread) => Ok(BackgroundStart { thread, started, projects }),
        Err(error) => Err((error, early.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default())),
    }
}

/// Poll-completion path for the first start. A panic retains the previous panic behavior.
pub(super) fn finish_background(world: &mut World, start: BackgroundStart) -> bool {
    let (result, projects, elapsed) = start.join();
    install(world, result, projects, elapsed, None, true)
}

fn init_dsp() {
    static DSP: std::sync::Once = std::sync::Once::new();
    // The hardware-FMA dispatch changes only which compiled copy runs; both produce the same bits.
    DSP.call_once(|| info!("AUDIO_DSP {}", skate_audio::dsp::init_fma()));
}

fn install(world: &mut World, started: Result<Native, String>, projects: usize, elapsed: Duration, epoch: Option<u64>, background: bool) -> bool {
    match started {
        Ok(mut native) => {
            info!("Game audio: native AEMS runtime on ({projects} projects)");
            if background {
                info!("AUDIO_RUNTIME_START background_ms={}", elapsed.as_millis());
            }
            if let Some(epoch) = epoch {
                native.map_epoch = epoch;
            }
            let handle = world.resource_mut::<Assets<NativeStream>>().add(NativeStream { shared: native.shared.clone() });
            // ONCE, not LOOP: the stream never ends, and Bevy's LOOP wraps it in rodio's
            // `repeat_infinite`, i.e. `Buffered`, which renders 32768 samples (64 blocks, 341 ms)
            // at a time inside the device callback and keeps every chunk forever: a lock burst of
            // 64 renders every 341 ms (game-thread stalls = the 19:01 stutter), up to 341 ms of
            // event-to-sound latency and ~23 MB/min of memory growth.
            world.spawn((NativeOutput, AudioPlayer(handle), PlaybackSettings::ONCE.with_volume(Volume::Linear(0.0))));
            world.insert_resource(native);
            true
        }
        Err(error) => {
            error!("Game audio: the native AEMS runtime could not start, so the skater's sounds, the world emitters and rolling are silent: {error}");
            false
        }
    }
}

/// Take the running runtime down for a restart: its stream entity is despawned (the sink and
/// rodio's copy of the stream drop with it; a block mid-render finishes on the old runtime), the
/// resource removed and dropped (the prefetch worker's sender with it: the worker ends after its
/// current decode, its result discarded). Returns what the new runtime carries over.
pub(super) fn shutdown(world: &mut World) -> Option<Carry> {
    let outputs: Vec<Entity> = world.query_filtered::<Entity, With<NativeOutput>>().iter(world).collect();
    for e in outputs {
        world.despawn(e);
    }
    let native = world.remove_resource::<Native>()?;
    let carry = Carry { first_node: native.next_node(), epoch: native.map_epoch + 1, world: native.world };
    drop(native);
    Some(carry)
}

/// Master volume on the stream; inside it the AEMS voices (world emitters: ambience) and the
/// rolling bed (effects) take their category volumes. Pause while silenced.
fn follow_volume(
    settings: Option<Res<super::AudioSettings>>,
    native: Option<Res<Native>>,
    menu: Option<Res<crate::graphics_menu::Menu>>,
    replay: Res<crate::replay::Replay>,
    mut sinks: Query<&mut AudioSink, With<NativeOutput>>,
    voices: Option<ResMut<super::Voices>>,
    mut listeners: Query<&mut SpatialListener, With<super::GameAudioListener>>,
) {
    let Some(settings) = settings else { return };
    // rodio 0.20's spatial source gives the ear FARTHER from the source the larger factor
    // (((d_left - d_right) / gap + 1) / 4 + 0.5 on the left channel), so with the ears where Bevy
    // puts them every Bevy positional sound is mirrored: the ears are swapped so Bevy sounds come
    // from the side native voices (Pan2D1) put them on.
    for mut listener in &mut listeners {
        let gap = listener.left_ear_offset.distance(listener.right_ear_offset);
        let want = Vec3::X * gap / 2.0;
        if listener.left_ear_offset != want {
            listener.left_ear_offset = want;
            listener.right_ear_offset = -want;
        }
    }
    let silenced = super::silenced(menu.as_deref(), &replay);
    let volume = settings.master().clamp(0.0, 1.0);
    // The measured world layers (zone beds, location sets, crossfades) are tuned at measured retail
    // level × RETAIL_SCALE; against the native voices (retail level) they play at the measured
    // level. Bevy and native voices share the ears: the Bevy ones fold like native voices. (Before
    // 2026-10-03 an install without the native runtime kept ×RETAIL_SCALE and Bevy's own panning,
    // a leftover of the removed interim tables; the native runtime is required now.)
    if let Some(mut voices) = voices {
        let scale = 1.0 / super::voices::RETAIL_SCALE;
        if voices.scale != scale {
            voices.scale = scale;
        }
        if !voices.native_fold {
            voices.native_fold = true;
        }
    }
    if let Some(native) = native {
        if let Ok(mut runtime) = native.shared.lock() {
            runtime.aems_gain = settings.category(super::Category::Ambience).clamp(0.0, 1.0);
            runtime.player_gain = settings.category(super::Category::Effects).clamp(0.0, 1.0);
            runtime.grains.gain = settings.category(super::Category::Effects).clamp(0.0, 1.0);
        }
    }
    for mut sink in &mut sinks {
        sink.set_volume(Volume::Linear(volume));
        if silenced && !sink.is_paused() {
            sink.pause();
        } else if !silenced && sink.is_paused() {
            sink.play();
        }
    }
}

/// The reverb preset (`SFXObj_Reverb`, `sub_824DE548`): the district's `audio_reverb` region at the
/// skater's x, z (reverb01 when none), crossfaded over 1 s of game time; per frame.
pub(super) fn reverb_frame(
    native: Option<Res<Native>>,
    library: Option<Res<Library>>,
    cues: Res<super::skate_events::Cues>,
    time: Res<Time<Real>>,
    listener: Query<&GlobalTransform, With<super::GameAudioListener>>,
    zones: Res<super::emitters::ReverbZones>,
    audio: Res<super::map_audio::MapAudio>,
) {
    let (Some(native), Some(library)) = (native, library) else { return };
    let at = cues.riding.board;
    let key = audio.region_key(&library, "audio_reverb", at.x, at.z).unwrap_or(skate_audio::bus::env::DEFAULT_PRESET);
    let camera = listener.single().ok().map(|t| skate_audio::bus::zones::Camera {
        position: t.translation().to_array(),
        forward: t.forward().as_vec3().to_array(),
    });
    if let Ok(mut runtime) = native.shared.lock() {
        let env = &mut runtime.mixer.buses.env;
        if env.enabled() {
            // SFXObj_Reverb's update (`sub_824DE548`) in retail's order, with the reverb-zone
            // emitters the listener is in (`emitters::reverb_zones`).
            let zones = &zones.zones[..];
            env.update(time.delta_secs().clamp(0.0, 0.25), key, zones, camera.as_ref());
        }
    }
}

/// The `c_emitter` azimuth word for a source seen from the listener: 0 = straight ahead,
/// increasing clockwise (to the right), 65536 = 360°.
pub(crate) fn azimuth(listener: &GlobalTransform, source: Vec3) -> i32 {
    let local = listener.affine().inverse().transform_point3(source);
    // Bevy cameras look down −Z with +X to the right.
    let degrees = local.x.atan2(-local.z).to_degrees();
    ((degrees / 360.0 * 65536.0).round() as i32).rem_euclid(65536)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azimuth_is_clockwise_from_the_view_direction() {
        let listener = GlobalTransform::from(Transform::from_xyz(0.0, 0.0, 0.0));
        assert_eq!(azimuth(&listener, Vec3::new(0.0, 0.0, -5.0)), 0);
        assert_eq!(azimuth(&listener, Vec3::new(5.0, 0.0, 0.0)), 16384);
        assert_eq!(azimuth(&listener, Vec3::new(-5.0, 0.0, 0.0)), 49152);
        assert_eq!(azimuth(&listener, Vec3::new(0.0, 0.0, 5.0)), 32768);
        // Turned to face +X, a source at +X is ahead.
        let turned = GlobalTransform::from(Transform::from_xyz(1.0, 0.0, 0.0).looking_at(Vec3::new(10.0, 0.0, 0.0), Vec3::Y));
        assert_eq!(azimuth(&turned, Vec3::new(10.0, 0.0, 0.0)), 0);
    }

    fn native(mixmap: Option<MixMap>) -> Native {
        Native {
            shared: Arc::new(Mutex::new(Runtime::new())),
            banks: HashMap::new(),
            emitter_class: None,
            mixmap,
            clock: HostClock::default(),
            frame_ticks: 0,
            cuts: 0,
            cut_seen: None,
            cut_wheels: None,
            holds: false,
            player: None,
            emitter_states: [false; EMITTER_STATES],
            bed: None,
            prefetch: Default::default(),
            map_epoch: 0,
            world: WorldInstances::RETAIL,
            pending: None,
            frontend: None,
            resident: Vec::new(),
            mod_projects: Vec::new(),
            early: Default::default(),
        }
    }

    /// The host clock under jittered frame times, in a Bevy app with the game's clocks (the 60 Hz
    /// physics period of `physics/clock.rs`, `Time<Virtual>`'s 250 ms frame cap): a FixedUpdate
    /// system publishes one sample per physics step as `skate_events::observe` does (the step's
    /// number in `pushes`, a push-plant pulse on some steps), an Update system takes them as
    /// [`mixmap_frame`] does. Frames: alternating 1 and 2 steps, frames without a step, a 300 ms
    /// hitch, a paused stretch (the menu: `Time<Virtual>` paused) and a silenced one that still
    /// steps (a replay / the multiplayer menu). Every step is taken by exactly one frame; each
    /// pulse reaches exactly one pass and no pass sees a pulse its steps did not have; a pass
    /// ticks once per step (4 at most); nothing passes while paused or silenced or on a frame
    /// without a step; the console evaluations are every second tick.
    #[test]
    fn the_host_clock_takes_every_physics_step_once() {
        use super::super::skate_events::{Cues, Riding};
        use bevy::time::{TimePlugin, TimeUpdateStrategy};
        use std::time::Duration;

        #[derive(Resource, Default)]
        struct Log {
            steps: u32,
            /// The steps with a pulse.
            pulses: Vec<u32>,
            /// Per frame: (first step, last step) taken, the pass (ticks, calls, pulse seen) if any,
            /// and whether the frame was paused / silenced.
            frames: Vec<(u32, u32, Option<(usize, usize, bool)>, bool, bool)>,
            taken: u32,
        }
        #[derive(Resource, Default)]
        struct Host {
            clock: HostClock,
            paused: bool,
            silenced: bool,
        }
        let publish = |mut cues: ResMut<Cues>, mut log: ResMut<Log>| {
            log.steps += 1;
            let step = log.steps;
            // A pulse on every 5th step (the hitch frame takes three).
            let pulse = step % 5 == 0;
            if pulse {
                log.pulses.push(step);
            }
            let mut r = Riding { pushes: step, ..Default::default() };
            r.audio.push_trigger = pulse;
            cues.publish(r);
        };
        let take = |mut cues: ResMut<Cues>, mut log: ResMut<Log>, mut host: ResMut<Host>| {
            let host = &mut *host;
            let (from, to) = (log.taken + 1, cues.riding.pushes);
            let had = cues.steps;
            let pass = host.clock.pass(&mut cues, host.silenced).map(|p| (p.ticks, p.calls, cues.riding.audio.push_trigger));
            assert_eq!(cues.steps, 0, "the frame takes its steps");
            if had > 0 {
                assert_eq!(to - from + 1, had, "steps counted = steps published");
                log.taken = to;
            }
            log.frames.push((from, if had > 0 { to } else { from - 1 }, pass, host.paused, host.silenced));
        };
        let period = Duration::from_nanos(166_666 * 100);
        let mut app = App::new();
        app.add_plugins(TimePlugin)
            .insert_resource(Time::<Fixed>::from_duration(period))
            .init_resource::<Cues>()
            .init_resource::<Log>()
            .init_resource::<Host>()
            .add_systems(FixedUpdate, publish)
            .add_systems(Update, take);
        let ms = |x: f64| Duration::from_secs_f64(x / 1000.0);
        let frame = |app: &mut App, dt: Duration, paused: bool, silenced: bool| {
            app.insert_resource(TimeUpdateStrategy::ManualDuration(dt));
            {
                let mut v = app.world_mut().resource_mut::<Time<Virtual>>();
                if paused { v.pause() } else { v.unpause() }
            }
            let mut h = app.world_mut().resource_mut::<Host>();
            (h.paused, h.silenced) = (paused, silenced);
            app.update();
        };
        let step = period.as_secs_f64() * 1000.0;
        for _ in 0..2 {
            frame(&mut app, ms(step), false, false);
        }
        // Alternating 1 and 2 steps (~40 fps), then 0-step frames (a fast renderer), then 30 fps.
        for i in 0..40 {
            frame(&mut app, ms(if i % 2 == 0 { step } else { 2.0 * step }), false, false);
        }
        for _ in 0..30 {
            frame(&mut app, ms(step / 3.0), false, false);
        }
        for _ in 0..10 {
            frame(&mut app, ms(2.0 * step), false, false);
        }
        // A 300 ms hitch (Time<Virtual> caps it at 250 ms: 15 steps).
        frame(&mut app, ms(300.0), false, false);
        frame(&mut app, ms(step), false, false);
        // The menu: the game paused (no steps) and silenced, for a second.
        for _ in 0..60 {
            frame(&mut app, ms(step), true, true);
        }
        // A replay / the multiplayer menu: silenced while the clock still steps.
        for _ in 0..20 {
            frame(&mut app, ms(step), false, true);
        }
        for i in 0..40 {
            frame(&mut app, ms(if i % 3 == 0 { 0.0 } else { 1.5 * step }), false, false);
        }
        let log = app.world().resource::<Log>();
        assert!(log.steps > 150, "{} steps", log.steps);
        // Every step is taken by exactly one frame, in order.
        let mut next = 1;
        for &(from, to, ..) in &log.frames {
            assert_eq!(from, next);
            next = to + 1;
        }
        assert_eq!(next, log.steps + 1, "every step taken");
        let mut seen_pulses = 0;
        let (mut ticks, mut calls) = (0usize, 0usize);
        let (mut hitch, mut zero_frames, mut double, mut paused_frames, mut merged) = (false, 0, 0, 0, 0);
        for &(from, to, pass, paused, silenced) in &log.frames {
            let n = (to + 1 - from) as usize;
            let pulses = log.pulses.iter().filter(|&&p| (from..=to).contains(&p)).count();
            match pass {
                Some((t, c, pulse)) => {
                    assert!(!paused && !silenced, "a pass while paused / silenced");
                    assert_eq!(t, n.min(MAX_STEPS_PER_FRAME as usize), "a tick per step, capped");
                    assert_eq!(pulse, pulses > 0, "steps {from}..={to}: pulse seen = pulse published");
                    seen_pulses += pulses;
                    ticks += t;
                    calls += c;
                    hitch |= n >= 15;
                    merged += usize::from(pulses > 1);
                    double += usize::from(n == 2);
                }
                None => {
                    assert!(n == 0 || silenced, "steps {from}..={to} not processed");
                    zero_frames += usize::from(n == 0 && !paused);
                    paused_frames += usize::from(paused);
                    assert!(!paused || n == 0, "a step while paused");
                }
            }
        }
        let dropped = log.frames.iter().filter(|f| f.4 && f.2.is_none()).map(|f| log.pulses.iter().filter(|&&p| (f.0..=f.1).contains(&p)).count()).sum::<usize>();
        assert_eq!(seen_pulses + dropped, log.pulses.len(), "each pulse in exactly one frame");
        assert!(dropped > 0 && seen_pulses > 20);
        assert!(hitch && merged > 0 && zero_frames > 20 && double > 20 && paused_frames == 60);
        assert_eq!(calls, ticks / 2, "a console evaluation every second tick");
    }

    #[test]
    fn emitter_words_without_a_mixmap_keep_the_old_defaults() {
        let n = native(None);
        assert_eq!(n.emitter_payload(Some(0), 0.5, 16384, 81), [32767, 16384, 0, 16384, 4096, 25000, 0, 0, 81]);
        assert_eq!(n.emitter_payload(None, 3.0, 0, 900)[1], 32767);
        assert_eq!(n.emitter_payload(None, -1.0, 0, -4)[..2], [32767, 0]);
        assert_eq!(n.emitter_payload(None, 0.0, 0, -4)[8], 0);
    }

    #[test]
    fn emitter_states_are_the_five_mixmap_instances() {
        let mut n = native(None);
        let got: Vec<_> = (0..6).map(|_| n.claim_emitter_state()).collect();
        assert_eq!(got, vec![Some(0), Some(1), Some(2), Some(3), Some(4), None]);
        n.release_emitter_state(2);
        assert_eq!(n.claim_emitter_state(), Some(2));
    }

    /// With the retail MixMap (when the private install has it): a positional emitter's dry word is
    /// out4 (−600 mB + the Global ducks' rest) × level, the pan is the B0 azimuth we wrote, the
    /// send rolls off with camera distance (4 → 70 m) and the filter stays open.
    #[test]
    #[ignore = "needs the private install data"]
    fn emitter_words_from_the_retail_mixmap() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/private/audio/aems/MixMapSK8.mxb");
        let Ok(bytes) = std::fs::read(path) else {
            panic!("missing private data: no {path}");
        };
        let mut n = native(Some(MixMap::from_bytes(&bytes).unwrap()));
        let g = n.claim_emitter_state().unwrap();
        let listener = GlobalTransform::from(Transform::from_xyz(0.0, 0.0, 0.0));
        let source = Vec3::new(10.0, 0.0, 0.0);
        n.set_emitter_position(g, &listener, Vec3::ZERO, source);
        let m = n.mixmap.as_mut().unwrap();
        for id in 1..=4 {
            m.set_input(keys::MASTER, id, 32767);
        }
        for _ in 0..3 {
            m.tick(MIX_STEP);
        }
        let near = n.emitter_payload(Some(g), 1.0, 0, 81);
        assert_eq!(near[1], 16365);
        assert_eq!(near[3], 16384, "pan = the azimuth written");
        assert_eq!((near[4], near[5], near[8]), (4096, 24971, 81));
        assert!(near[2] > 0);
        assert_eq!(n.emitter_payload(Some(g), 0.5, 0, 81)[1], 16365 / 2);
        n.set_emitter_position(g, &listener, Vec3::ZERO, Vec3::new(100.0, 0.0, 0.0));
        n.mixmap.as_mut().unwrap().tick(MIX_STEP);
        let far = n.emitter_payload(Some(g), 1.0, 0, 81);
        assert_eq!(far[2], 0, "beyond 70 m the send is silent");
        assert_eq!(far[1], near[1], "the dry level has no distance roll-off");
    }

    /// The stutter of the 19:01 build: a looped (`repeat_infinite`) stream renders 64 blocks on
    /// its first pull; played once it renders one block per 256 frames.
    #[test]
    fn the_stream_renders_one_block_per_pull_unless_looped() {
        let looped = NativeStream { shared: Arc::new(Mutex::new(Runtime::new())) };
        let mut source = looped.decoder().repeat_infinite();
        source.next();
        assert_eq!(looped.shared.lock().unwrap().blocks, 64, "Buffered pulls 32768 samples");
        let once = NativeStream { shared: Arc::new(Mutex::new(Runtime::new())) };
        let mut source = once.decoder();
        source.next();
        assert_eq!(once.shared.lock().unwrap().blocks, 1);
    }

    #[test]
    fn the_stream_pulls_blocks_from_the_runtime() {
        let stream = NativeStream { shared: Arc::new(Mutex::new(Runtime::new())) };
        let mut decoder = stream.decoder();
        let samples: Vec<f32> = (&mut decoder).take(4 * skate_audio::BLOCK).collect();
        assert!(samples.iter().all(|&s| s == 0.0));
        assert_eq!(stream.shared.lock().unwrap().blocks, 2);
        assert_eq!((decoder.channels(), decoder.sample_rate()), (2, 48000));
    }

    /// An install that lacks some of its data must still start (or log an error and leave those
    /// sounds silent), never panic. Each case loads a copy of the
    /// dev install's manifest with parts removed (the data folders are linked, not copied).
    /// Data-gated: skipped without the install.
    #[test]
    #[ignore = "needs the private install data"]
    fn missing_install_parts_fall_back_without_breaking() {
        let real = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/private/audio"));
        let Ok(text) = std::fs::read_to_string(real.join("audio_manifest.json")) else { panic!("missing private data: no audio install") };
        let full: serde_json::Value = serde_json::from_str(&text).unwrap();
        if full["aems"]["projects"].as_array().is_none_or(|p| p.is_empty()) {
            panic!("missing private data: the install has no AEMS banks");
        }
        let dir = std::env::temp_dir().join(format!("skate-audio-fallback-{}", std::process::id()));
        let audio = dir.join("private/audio");
        std::fs::create_dir_all(&audio).unwrap();
        let mut links = Vec::new();
        for sub in ["aems", "banks", "grains", "wheels", "ambience"] {
            let (link, target) = (audio.join(sub), real.join(sub));
            if !target.is_dir() {
                continue;
            }
            // mklink does not take the `\\?\` form canonicalize returns.
            let target = target.canonicalize().unwrap().to_string_lossy().trim_start_matches(r"\\?\").to_owned();
            let out = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link.to_string_lossy().replace('/', "\\")).arg(&target).output();
            if !out.as_ref().is_ok_and(|o| o.status.success()) {
                let _ = std::fs::remove_dir_all(&dir);
                let why = out.map_or_else(|e| e.to_string(), |o| String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr));
                panic!("missing private data: could not link {sub} ({})", why.trim());
            }
            links.push(link);
        }
        let start = |name: &str, edit: &dyn Fn(&mut serde_json::Value)| -> Result<Native, String> {
            let mut m = full.clone();
            edit(&mut m);
            std::fs::write(audio.join("audio_manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
            let library = super::super::Library::load(&dir).unwrap_or_else(|e| panic!("{name}: {e}"));
            let r = Native::start(&library);
            println!("{name}: {}", match &r {
                Ok(n) => format!("native on, player {:?}", n.player.as_ref().map(|p| (p.components, p.tricks_on, p.treatment_on, p.contacts_on, p.footsteps_on))),
                Err(e) => format!("no native runtime (silent): {e}"),
            });
            r
        };
        let remove_bank = |stem: &'static str| move |m: &mut serde_json::Value| {
            m["aems"]["banks"].as_object_mut().unwrap().remove(stem);
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let n = start("full install", &|_| {}).expect("the full install starts");
            let p = n.player.as_ref().expect("player components with a MixMap");
            assert!(p.components && p.tricks_on && p.treatment_on);
            assert!(start("no AEMS projects", &|m| m["aems"]["projects"] = serde_json::json!([])).is_err());
            assert!(start("no emitter_utility bank", &remove_bank("emitter_utility")).is_err());
            let n = start("no MixMap", &|m| m["aems"]["mixmap"] = serde_json::Value::Null).expect("starts without a MixMap");
            assert!(n.player.is_none() && n.bed.is_none());
            let n = start("no GRINDS bank", &remove_bank("GRINDS")).expect("starts without the player banks");
            assert!(!n.player.as_ref().unwrap().components, "the components stay off (silent, error logged)");
            let n = start("no Treatments bank", &remove_bank("Treatments")).expect("starts without Treatments");
            let p = n.player.as_ref().unwrap();
            assert!(p.components && p.tricks_on && !p.treatment_on);
            let n = start("Treatments listed, file missing", &|m| m["aems"]["banks"]["Treatments"] = serde_json::json!("aems/missing/Treatments.abk"))
                .expect("starts with a missing bank file");
            assert!(!n.player.as_ref().unwrap().treatment_on);
            start("no Common bank (seam utility)", &remove_bank(skate_audio::player::seams::UTILITY_BANK)).expect("starts without Common");
            start("no Splice trees", &|m| m["aems"]["splice"] = serde_json::json!({})).expect("starts without the Splice trees");
            start("no bus tuning", &|m| {
                m.as_object_mut().unwrap().remove("bus_tuning");
            })
            .expect("starts without the bus tuning");
        }));
        for link in &links {
            let _ = std::fs::remove_dir(link);
        }
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}
