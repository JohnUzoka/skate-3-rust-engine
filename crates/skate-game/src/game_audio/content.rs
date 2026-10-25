//! Audio content overlays at run time (doc 16 "Audio modding"): which running mods ship an
//! `audio.json` (`skate_mods::audio_content`), the merged [`Library`] and the native runtime's
//! start and restarts.
//!
//! - An overlay is active only while its mod runs: the mod manager's running set is diffed every
//!   frame ([`AudioContent::sync`], called by the mod system after its scan and after applying
//!   commands, so a failed mod's overlay goes at once). Changes are coalesced into one restart.
//! - The native runtime starts after the first mod scan ([`frame`], the first system of the audio
//!   pass), so an audio mod enabled at boot costs no second start. Without overlays the startup
//!   [`Library`] is used as it is: no merge, no rebuild. Its runtime is built on a worker; content
//!   remains `Starting` until that worker is finished, then the game thread installs the stream.
//! - A restart rebuilds the [`Library`] (install + overlays in mod-id order: the first owner of an
//!   identity wins), takes the runtime and its stream down and starts a new one that continues
//!   the old one's post ids with `map_epoch` + 1, and replaces the world / NPC / speech hosts with
//!   fresh ones: old node ids and Splice sounds are forgotten, never released into the new
//!   runtime. The map-keyed state (emitters, reverb zones, zone ambience, location sets, map
//!   audio) keys on [`AudioContent::generation`] and rebuilds. A short cut in the sound, at mod
//!   enable / disable / failure only (user decision 2026-10-03).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use skate_mods::audio_content::{AudioOverlay, Loaded, MAX_PCM_BYTES_TOTAL};

use super::Library;
use super::library::{ContentReport, OverlaySource};

/// A running mod's checked overlay.
pub(crate) struct Registered {
    pub root: PathBuf,
    pub overlay: AudioOverlay,
    pcm_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Runtime {
    /// Waiting for the first mod scan.
    #[default]
    NotStarted,
    /// The first runtime is being assembled by `native::BackgroundStart` off the game thread.
    Starting,
    Started,
    /// The start failed (no AEMS data): retried only when the content changes.
    Failed,
}

#[derive(Resource, Default)]
pub(crate) struct AudioContent {
    /// The running mods' overlays by mod id (the merge order).
    pub(crate) overlays: BTreeMap<String, Registered>,
    /// Package fingerprint whose `audio.json` was looked at, by mod id (no overlay, or one that
    /// failed: not read again until the package changes).
    checked: BTreeMap<String, u64>,
    /// Mods whose `audio.json` failed its checks (shown in the mod menu until the mod changes).
    pub(crate) load_errors: BTreeMap<String, String>,
    /// The overlay set changed: [`frame`] rebuilds at the next audio pass.
    pending: bool,
    /// Bumped by every rebuild: the map-keyed audio state keys on it.
    pub(crate) generation: u64,
    /// The last rebuild's conflicts, warnings and rejections (kept until the next rebuild).
    pub(crate) report: ContentReport,
    /// The mod manager scanned at least once (set by the mod system).
    pub(crate) scanned: bool,
    /// The optional merge report for the asynchronous first start.
    starting_report: Option<ContentReport>,
    runtime: Runtime,
    /// Restarts done.
    pub(crate) restarts: u64,
    /// Bumped when a running overlay with `rules` comes or goes (`mod_rules` recompiles; rules
    /// alone never restart the sound).
    pub(crate) rules_generation: u64,
    /// Bumped by every runtime restart only (not by a hot swap): what holds runtime handles (post
    /// ids, mixer voices, private MixMap instances) forgets them when it changes.
    pub(crate) runtime_generation: u64,
    /// Bumped by a restart and by a hot swap that changed the world layer (beds, zones, records,
    /// sets, map audio: `Library::world_key`): the map-keyed state rebuilds when it changes.
    pub(crate) world_generation: u64,
    /// Hot swaps done (doc 16 L1), and the last content change: "swap" or "restart: <reasons>".
    pub(crate) swaps: u64,
    pub(crate) last_change: Option<String>,
}

impl AudioContent {
    /// Diff the running packages (id, root, content fingerprint) against the registered
    /// overlays: a newly running or changed package's `audio.json` is read and checked, a stopped
    /// one's overlay is dropped. Cheap when nothing changed (a map lookup per running package).
    pub(crate) fn sync<'a>(&mut self, running: impl Iterator<Item = (&'a str, &'a Path, u64)>) {
        let mut alive = Vec::new();
        for (id, root, fingerprint) in running {
            alive.push(id);
            if self.checked.get(id) == Some(&fingerprint) {
                continue;
            }
            self.checked.insert(id.to_owned(), fingerprint);
            let was = self.overlays.remove(id);
            self.load_errors.remove(id);
            match skate_mods::audio_content::load(root) {
                Ok(Some(loaded)) => match self.admit(id, &loaded) {
                    Ok(()) => {
                        info!("Game audio: mod {id} audio content: {}", loaded.overlay.summary().join(", "));
                        // Only content restarts the sound; rules apply without a restart.
                        self.pending |= loaded.overlay.has_content();
                        if !loaded.overlay.rules.is_empty() {
                            self.rules_generation += 1;
                        }
                        self.overlays.insert(id.to_owned(), Registered { root: root.to_owned(), overlay: loaded.overlay, pcm_bytes: loaded.pcm_bytes });
                    }
                    Err(e) => self.refuse(id, e),
                },
                Ok(None) => {}
                Err(e) => self.refuse(id, e),
            }
            if let Some(old) = was {
                self.pending |= old.overlay.has_content();
                if !old.overlay.rules.is_empty() {
                    self.rules_generation += 1;
                }
            }
        }
        let stopped: Vec<String> = self.overlays.keys().filter(|id| !alive.contains(&id.as_str())).cloned().collect();
        for id in stopped {
            info!("Game audio: mod {id} stopped: its audio content is removed");
            if let Some(old) = self.overlays.remove(&id) {
                self.pending |= old.overlay.has_content();
                if !old.overlay.rules.is_empty() {
                    self.rules_generation += 1;
                }
            }
        }
        self.checked.retain(|id, _| alive.contains(&id.as_str()));
        self.load_errors.retain(|id, _| alive.contains(&id.as_str()));
    }

    /// The total PCM budget across running overlays.
    fn admit(&self, id: &str, loaded: &Loaded) -> Result<(), String> {
        let others: u64 = self.overlays.iter().filter(|(k, _)| *k != id).map(|(_, r)| r.pcm_bytes).sum();
        if others + loaded.pcm_bytes > MAX_PCM_BYTES_TOTAL {
            return Err(format!("audio.json: all audio mods together exceed {} MiB of PCM; this one is left out", MAX_PCM_BYTES_TOTAL / (1024 * 1024)));
        }
        Ok(())
    }

    fn refuse(&mut self, id: &str, e: String) {
        warn!("Game audio: mod {id}: {e}");
        self.load_errors.insert(id.to_owned(), e);
    }

    /// Tests: whether the content changed (a restart at the next pass).
    #[cfg(test)]
    pub(crate) fn restart_pending(&self) -> bool {
        self.pending
    }

    /// Request a rebuild at the next audio pass (tests; an engine importer that changed data).
    /// Engine API: no in-game caller yet (the hot swap handles mod changes itself).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn invalidate(&mut self) {
        self.pending = true;
    }

    /// The menu lines about one mod's audio content: its load error, conflicts, rejection and
    /// warnings (the first ones; the log has all).
    pub(crate) fn messages_for(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(e) = self.load_errors.get(id) {
            out.push(format!("Audio content not loaded: {e}"));
        }
        let r = &self.report;
        for m in r.rejected.iter().filter(|m| m.owner == id) {
            out.push(format!("Audio content left out: {}", m.text));
        }
        let conflicts: Vec<_> = r.conflicts.iter().filter(|m| m.owner == id).collect();
        if let Some(first) = conflicts.first() {
            out.push(format!("Audio conflict ({}): {}", conflicts.len(), first.text));
        }
        let warnings = r.warnings.iter().filter(|m| m.owner == id).count();
        if warnings > 0 {
            out.push(format!("Audio content: {warnings} entries this install does not have were skipped (see the log)"));
        }
        out
    }

    /// One line for the mod list when nothing is selected: how many audio conflicts there are.
    pub(crate) fn summary(&self) -> Option<String> {
        let n = self.report.conflicts.len() + self.report.rejected.len() + self.load_errors.len();
        (n > 0).then(|| format!("Audio mods: {n} problems (two mods changing one sound: the first by mod id wins). Select a mod for details."))
    }

    /// The overlays in merge order.
    fn sources(&self) -> Vec<OverlaySource<'_>> {
        self.overlays.iter().map(|(id, r)| OverlaySource { id, root: &r.root, overlay: &r.overlay }).collect()
    }
}

/// Rebuild the library from the install and the registered overlays (the install alone when
/// the merge fails) and make it the resource. None when the install has no audio.
fn rebuild(world: &mut World) -> Option<ContentReport> {
    let (library, report) = merged(world)?;
    world.insert_resource(library);
    Some(report)
}

/// The library of the install and the registered overlays (see [`rebuild`]), not inserted.
fn merged(world: &mut World) -> Option<(Library, ContentReport)> {
    let root = world.get_resource::<crate::config::Config>()?.asset_root.clone();
    let content = world.resource::<AudioContent>();
    let result = Library::load_with(&root, &content.sources());
    let (library, report) = match result {
        Ok(v) => v,
        Err(e) => {
            error!("Game audio: the audio content overlays could not be merged ({e}); playing the install's audio");
            let library = Library::load(&root).ok()?;
            let mut report = ContentReport::default();
            report.rejected = content.overlays.keys().map(|id| skate_mods::audio_merge::Message { owner: id.clone(), text: e.clone() }).collect();
            (library, report)
        }
    };
    for m in &report.conflicts {
        warn!("Game audio: mod {}: {}", m.owner, m.text);
    }
    for m in report.warnings.iter().chain(&report.rejected) {
        warn!("Game audio: mod {}: {}", m.owner, m.text);
    }
    Some((library, report))
}

/// Replace the hosts that hold runtime handles (node ids, Splice sounds, MixMap instances,
/// loaded speech takes) with fresh ones: the old handles are forgotten, never released into the
/// new runtime.
fn reset_hosts(world: &mut World) {
    world.insert_resource(super::world_sources::WorldHost::default());
    world.insert_resource(super::npc_skaters::NpcHost::default());
    world.insert_resource(super::world_speech::WorldSpeech::default());
}

fn finish_first_start(world: &mut World, report: Option<ContentReport>, started: bool, clear_pending: bool) {
    let mut content = world.resource_mut::<AudioContent>();
    if let Some(report) = report {
        content.report = report;
        content.generation += 1;
        content.world_generation += 1;
    }
    if clear_pending {
        content.pending = false;
    }
    content.runtime = if started { Runtime::Started } else { Runtime::Failed };
}

/// The first system of the audio pass: the native runtime's first start (after the first mod
/// scan, or at once without the mod system, and once the start-up banks' thread is done) and the
/// restart after an overlay change.
pub(super) fn frame(world: &mut World) {
    let mods = world.contains_resource::<crate::modding::Mods>();
    let Some(content) = world.get_resource::<AudioContent>() else { return };
    let (runtime, pending, scanned, overlays) = (content.runtime, content.pending, content.scanned, !content.overlays.is_empty());
    if !world.contains_resource::<Library>() {
        // No audio install: nothing to start or rebuild.
        if pending {
            world.resource_mut::<AudioContent>().pending = false;
        }
        return;
    }
    match runtime {
        Runtime::NotStarted => {
            if mods && !scanned {
                return;
            }
            // Startup banks were read while the scene built. Don't start the native runtime until
            // they are ready; the runtime assembly itself runs in the background too.
            if world.get_resource::<super::native::startup::EarlyLoads>().is_some_and(|early| !early.finished()) {
                return;
            }
            // Overlays from the first mod scan belong in the first runtime. The snapshot below
            // shares the immutable manifest and paths; audio::Library stays available to systems
            // while the worker parses projects and loads banks.
            let report = if overlays { rebuild(world) } else { None };
            let Some(settings) = world.get_resource::<super::AudioSettings>() else {
                let started = super::native::launch(world, None);
                super::seed::from_env(world);
                finish_first_start(world, report, started, true);
                return;
            };
            let instances = if settings.more_audible_world() { super::native::WorldInstances::MORE_AUDIBLE } else { super::native::WorldInstances::RETAIL };
            let library = world.resource::<Library>().startup_view();
            let early = world.remove_resource::<super::native::startup::EarlyLoads>()
                .map(super::native::startup::EarlyLoads::join).unwrap_or_default();
            match super::native::start_background(library, instances, early) {
                Ok(start) => {
                    world.insert_resource(start);
                    let mut content = world.resource_mut::<AudioContent>();
                    content.starting_report = report;
                    content.pending = false;
                    content.runtime = Runtime::Starting;
                    return;
                }
                Err((error, early)) => {
                    warn!("Game audio: no runtime startup thread ({error}); starting on the game thread");
                    let started = super::native::launch_with_early(world, None, early);
                    super::seed::from_env(world);
                    finish_first_start(world, report, started, true);
                }
            }
        }
        Runtime::Starting => {
            let Some(start) = world.get_resource::<super::native::BackgroundStart>() else {
                panic!("audio runtime is Starting without its worker");
            };
            if !start.finished() {
                return;
            }
            let start = world.remove_resource::<super::native::BackgroundStart>().unwrap();
            let started = super::native::finish_background(world, start);
            super::seed::from_env(world);
            let report = world.resource_mut::<AudioContent>().starting_report.take();
            finish_first_start(world, report, started, false);
        }
        Runtime::Started if pending => swap_or_restart(world),
        Runtime::Failed if pending => restart(world),
        _ => {}
    }
    // Tuning writes apply here, between passes (after a restart: onto the new Library); then a
    // seed of the random state (doc 16 L5; nothing while unseeded).
    super::tuning::apply(world);
    super::seed::apply(world);
}

/// The running mods' audio content changed: swap the new library in without a restart where that
/// is exact (`swap.rs`, doc 16 L1), else restart the runtime (the fallback, with its reasons).
pub(crate) fn swap_or_restart(world: &mut World) {
    let Some((library, report)) = merged(world) else { return };
    let plan = match (world.get_resource::<Library>(), world.get_resource::<super::native::Native>()) {
        (Some(old), Some(native)) => super::swap::plan(old, &library, native),
        _ => {
            let mut p = super::swap::Plan::default();
            p.restart.push("no running runtime".into());
            p
        }
    };
    if !plan.restart.is_empty() {
        info!("Game audio: the audio content change needs a restart: {}", plan.restart.join("; "));
        world.insert_resource(library);
        restart_with(world, report, Some(plan.restart.join("; ")));
        return;
    }
    if let Err(e) = super::swap::apply(world, &plan, &library) {
        warn!("Game audio: the hot swap failed ({e}); restarting the audio instead");
        world.insert_resource(library);
        restart_with(world, report, Some(format!("the swap failed: {e}")));
        return;
    }
    if !plan.tuning.is_empty() {
        let changed = plan.tuning.clone();
        world.insert_resource(library);
        super::tuning::retune(world, &changed);
    } else {
        world.insert_resource(library);
    }
    let mut content = world.resource_mut::<AudioContent>();
    content.report = report;
    content.generation += 1;
    content.swaps += 1;
    if plan.world {
        content.world_generation += 1;
    }
    content.pending = false;
    content.last_change = Some("swap".into());
    info!(
        "Game audio: swapped the mods' audio content in place (generation {}, overlays {:?}; banks replaced {:?}, unloaded {:?}, Splice {:?}, wheels {}, projects in {:?} out {:?}, tuning {:?}, speech {}, world {})",
        content.generation, content.overlays.keys().collect::<Vec<_>>(), plan.replace, plan.unload, plan.splice, plan.wheels, plan.projects_in, plan.projects_out.iter().map(|p| &p.0).collect::<Vec<_>>(),
        plan.tuning.iter().map(|t| t.0).collect::<Vec<_>>(), plan.speech, plan.world
    );
}

/// Rebuild the library and restart the native runtime (see the module docs).
pub(crate) fn restart(world: &mut World) {
    let Some(report) = rebuild(world) else { return };
    restart_with(world, report, None);
}

/// Restart the runtime on the library in place (`report`: its merge report).
fn restart_with(world: &mut World, report: ContentReport, why: Option<String>) {
    let carry = super::native::shutdown(world);
    reset_hosts(world);
    let started = super::native::launch(world, carry);
    // Mods' global overrides hold in the new runtime too (their posts are gone: the handles read
    // `live = false` and the mods post again).
    if world.contains_resource::<super::mod_audio::AudioApi>() {
        world.resource_scope(|world, mut api: Mut<super::mod_audio::AudioApi>| {
            if let Some(native) = world.get_resource::<super::native::Native>() {
                api.reapply_globals(native);
            }
        });
    }
    let mut content = world.resource_mut::<AudioContent>();
    content.report = report;
    content.generation += 1;
    content.runtime_generation += 1;
    content.world_generation += 1;
    content.restarts += 1;
    content.last_change = Some(format!("restart{}", why.map_or(String::new(), |w| format!(": {w}"))));
    content.pending = false;
    content.runtime = if started { Runtime::Started } else { Runtime::Failed };
    info!("Game audio: restarted for the mods' audio content (generation {}, overlays {:?})", content.generation, content.overlays.keys().collect::<Vec<_>>());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(dir: &Path, id: &str, overlay: Option<serde_json::Value>) -> PathBuf {
        let root = dir.join(id);
        std::fs::create_dir_all(root.join("audio")).unwrap();
        std::fs::write(root.join("audio/a.wav"), super::super::library::tests::test_wav(100, 48000, 9)).unwrap();
        match overlay {
            Some(o) => std::fs::write(root.join("audio.json"), o.to_string()).unwrap(),
            None => {
                let _ = std::fs::remove_file(root.join("audio.json"));
            }
        }
        root
    }

    /// The running-set diff: an overlay registers while its mod runs, is re-read when the package
    /// changes, goes when the mod stops; a broken `audio.json` is a menu message, never a crash;
    /// nothing is re-read while nothing changes.
    #[test]
    fn overlays_follow_the_running_mods() {
        let dir = std::env::temp_dir().join(format!("skate-audio-content-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = package(&dir, "dev.a", Some(serde_json::json!({"version": 1, "replace": {"samples": {"x": {"0": "audio/a.wav"}}}})));
        let b = package(&dir, "dev.b", None);
        let mut c = AudioContent::default();
        c.sync([("dev.a", a.as_path(), 1), ("dev.b", b.as_path(), 1)].into_iter());
        assert!(c.pending && c.overlays.contains_key("dev.a") && !c.overlays.contains_key("dev.b"));
        c.pending = false;
        // Unchanged: nothing is read (even a now-broken file is not noticed until the package
        // fingerprint changes, as the manager reloads it then).
        std::fs::write(a.join("audio.json"), "{broken").unwrap();
        c.sync([("dev.a", a.as_path(), 1), ("dev.b", b.as_path(), 1)].into_iter());
        assert!(!c.pending && c.overlays.contains_key("dev.a"));
        // Changed and broken: the overlay goes, the error is kept for the menu.
        c.sync([("dev.a", a.as_path(), 2), ("dev.b", b.as_path(), 1)].into_iter());
        assert!(c.pending && c.overlays.is_empty());
        assert!(c.messages_for("dev.a")[0].starts_with("Audio content not loaded"));
        assert!(c.summary().is_some());
        c.pending = false;
        // Fixed, then the mod stops (disabled or its script failed): removed. (An overlay without
        // content, e.g. rules only, never needs a restart: audio/moddability-2.)
        package(&dir, "dev.a", Some(serde_json::json!({"version": 1, "replace": {"samples": {"x": {"0": "audio/a.wav"}}}})));
        c.sync([("dev.a", a.as_path(), 3)].into_iter());
        assert!(c.pending && c.overlays.contains_key("dev.a") && c.load_errors.is_empty());
        c.pending = false;
        c.sync(std::iter::empty());
        assert!(c.pending && c.overlays.is_empty());
        c.pending = false;
        package(&dir, "dev.a", Some(serde_json::json!({"version": 1})));
        c.sync([("dev.a", a.as_path(), 4)].into_iter());
        assert!(!c.pending && c.overlays.contains_key("dev.a"), "an empty overlay: registered, no restart");
        c.sync(std::iter::empty());
        assert!(!c.pending && c.overlays.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn config(root: &Path) -> crate::config::Config {
        crate::config::Config {
            asset_root: root.to_owned(), verification_capture: None, map: None, map_path: None, boot_map: None,
            difficulty: Default::default(), check_assets: false, validate_maps: false, start_paused: false,
            multiplayer: Default::default(), map_fingerprint: 0, teleport: None, mute: true,
        }
    }

    fn wait_for_first_start(world: &mut World) {
        if world.resource::<AudioContent>().runtime == Runtime::NotStarted {
            frame(world);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while world.resource::<AudioContent>().runtime == Runtime::Starting {
            frame(world);
            assert!(std::time::Instant::now() < deadline, "native audio startup worker did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn first_runtime_start_is_pending_then_reports_missing_projects() {
        let (root, _) = super::super::library::tests::content_fixture("async-first-start");
        let mut world = World::new();
        let config = config(&root);
        world.insert_resource(super::super::AudioSettings::load(&config));
        world.insert_resource(config);
        world.insert_resource(Library::load(&root).unwrap());
        world.init_resource::<Assets<super::super::native::NativeStream>>();
        world.init_resource::<AudioContent>();
        world.init_resource::<super::super::mod_audio::AudioApi>();
        reset_hosts(&mut world);
        world.resource_mut::<AudioContent>().scanned = true;

        frame(&mut world);
        assert_eq!(world.resource::<AudioContent>().runtime, Runtime::Starting);
        assert!(!world.contains_resource::<super::super::native::Native>());
        assert!(world.contains_resource::<super::super::native::BackgroundStart>());

        wait_for_first_start(&mut world);
        assert_eq!(world.resource::<AudioContent>().runtime, Runtime::Failed);
        assert!(!world.contains_resource::<super::super::native::BackgroundStart>());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A world with what [`frame`] / [`restart`] use, the install's library and the runtime
    /// started as the game starts it.
    fn game_world(root: &Path) -> World {
        let mut world = World::new();
        let config = config(root);
        world.insert_resource(super::super::AudioSettings::load(&config));
        world.insert_resource(config);
        world.insert_resource(Library::load(root).unwrap());
        world.init_resource::<Assets<super::super::native::NativeStream>>();
        world.init_resource::<AudioContent>();
        world.init_resource::<super::super::mod_audio::AudioApi>();
        reset_hosts(&mut world);
        world.resource_mut::<AudioContent>().scanned = true;
        wait_for_first_start(&mut world);
        assert!(world.contains_resource::<super::super::native::Native>(), "the runtime starts at the first pass after the scan");
        world
    }

    /// A first emitter bank of DownTown's `.ems` that the install has, and its patch.
    fn emitter_bank(library: &Library) -> (String, i32) {
        let r = library.emitters("sfx_downtown").iter().find(|r| r.kind == 1 && r.bank.as_ref().is_some_and(|b| library.aems().banks.contains_key(b)))
            .expect("a DownTown emitter with a bank");
        (r.bank.clone().unwrap(), r.patch)
    }

    /// The scenario after the restart point: the emitter bank loaded, one `c_emitter` post held
    /// and redelivered every 8 blocks, 600 blocks rendered (6.4 s). Returns the stereo output.
    fn scenario(world: &mut World, bank: &str, patch: i32) -> Vec<f32> {
        let library = world.remove_resource::<Library>().unwrap();
        let mut native = world.resource_mut::<super::super::native::Native>();
        native.ensure_bank(&library, bank).unwrap();
        let payload = native.emitter_payload(None, 0.8, 9000, patch);
        let node = native.post_emitter(&payload).expect("c_emitter");
        let mut out = vec![0.0f32; 2 * skate_audio::BLOCK];
        let mut all = Vec::new();
        for b in 0..600 {
            if b % 8 == 0 {
                native.redeliver(node, &payload);
            }
            native.shared.lock().unwrap().fill_stereo(&mut out);
            all.extend_from_slice(&out);
        }
        drop(native);
        world.insert_resource(library);
        all
    }

    /// R1e (data-gated): an audio mod found by the first scan is merged before the first start
    /// (no restart); its `preload` bank loads at start and survives a map change; its other bank
    /// loads on use and goes with the map like any map bank.
    #[test]
    #[ignore = "needs the private install data"]
    fn preloaded_mod_banks_survive_map_changes() {
        use super::super::native::Native;
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"));
        if Library::load(root).is_err() {
            panic!("missing private data: no audio install");
        }
        let dir = std::env::temp_dir().join(format!("skate-audio-resident-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        use skate_audio::eval::synthetic;
        let abk = synthetic::bank(&[synthetic::player_module(2)], &[], &[(1000, false)]).data;
        std::fs::write(dir.join("audio/k.abk"), &abk).unwrap();
        std::fs::write(dir.join("audio/a.wav"), super::super::library::tests::test_wav(1000, 48000, 100)).unwrap();
        std::fs::write(dir.join("audio.json"), serde_json::json!({"version": 1, "add": {"banks": {
            "MOD_keep": {"abk": "audio/k.abk", "samples": ["audio/a.wav"], "preload": true, "group": "player"},
            "MOD_map": {"abk": "audio/k.abk", "samples": ["audio/a.wav"]}}}}).to_string()).unwrap();
        let mut world = World::new();
        let config = config(root);
        world.insert_resource(super::super::AudioSettings::load(&config));
        world.insert_resource(config);
        world.insert_resource(Library::load(root).unwrap());
        world.init_resource::<Assets<super::super::native::NativeStream>>();
        world.init_resource::<AudioContent>();
        reset_hosts(&mut world);
        {
            let mut c = world.resource_mut::<AudioContent>();
            c.scanned = true;
            c.sync([("dev.resident", dir.as_path(), 1)].into_iter());
        }
        wait_for_first_start(&mut world);
        let c = world.resource::<AudioContent>();
        assert_eq!((c.restarts, c.generation), (0, 1), "merged before the first start");
        assert_eq!(c.report.applied, ["dev.resident"]);
        let library = world.remove_resource::<Library>().unwrap();
        let mut native = world.resource_mut::<Native>();
        assert!(native.bank_loaded("MOD_keep") && !native.bank_loaded("MOD_map"));
        native.ensure_bank(&library, "MOD_map").unwrap();
        native.unload_map_banks();
        assert!(native.bank_loaded("MOD_keep"), "the preloaded bank stays");
        assert!(!native.bank_loaded("MOD_map"), "a map bank goes");
        assert!(native.bank_loaded("emitter_utility"));
        drop(native);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn same(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
    }

    /// R1d (data-gated): a restart mid-scenario (a held emitter post, blocks rendered) continues
    /// the old runtime's post ids, bumps the map epoch, respawns the stream and resets the hosts;
    /// from the restart point the output equals a fresh runtime's (same first post id) bit for
    /// bit. A mod replacing the emitter bank's samples changes the output; removing the mod
    /// (another restart) gives the retail output again.
    #[test]
    #[ignore = "needs the private install data"]
    fn a_restart_mid_scenario_plays_like_a_fresh_start_and_a_removed_mod_restores_retail() {
        use super::super::native::{Native, NativeOutputCount, WorldInstances};
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"));
        if Library::load(root).is_err() {
            panic!("missing private data: no audio install");
        }
        let mut world = game_world(root);
        let (bank, patch) = emitter_bank(world.resource::<Library>());
        // Before the restart: the scenario runs on the first runtime.
        let before = scenario(&mut world, &bank, patch);
        assert!(before.iter().any(|s| *s != 0.0), "{bank} sounds");
        let held = world.resource::<Native>().next_node() - 1;
        let (next, epoch) = (world.resource::<Native>().next_node(), world.resource::<Native>().map_epoch);
        world.resource_mut::<AudioContent>().invalidate();
        restart(&mut world);
        assert_eq!(world.resource::<AudioContent>().restarts, 1);
        assert_eq!(world.resource::<AudioContent>().generation, 1);
        {
            let n = world.resource::<Native>();
            assert_eq!(n.map_epoch, epoch + 1);
            let rt = n.shared.lock().unwrap();
            assert!(rt.eval.node_class(skate_audio::eval::NodeId(held)).is_none(), "an old id is not a post of the new runtime");
            assert!(rt.eval.next_node() > next, "the boot posts took ids after the old ones");
        }
        assert_eq!(NativeOutputCount::of(&mut world), 1, "one stream");
        // Releasing an old id (a host that missed the reset) touches nothing in the new runtime.
        let live = world.resource::<Native>().shared.lock().unwrap().eval.node_count();
        world.resource::<Native>().release(skate_audio::eval::NodeId(held));
        assert_eq!(world.resource::<Native>().shared.lock().unwrap().eval.node_count(), live);
        // From the restart point: the same as a fresh runtime with the same first id.
        let restarted = scenario(&mut world, &bank, patch);
        let fresh = {
            let mut w = World::new();
            w.insert_resource(Library::load(root).unwrap());
            let library = w.resource::<Library>();
            let native = Native::start_from(library, WorldInstances::RETAIL, next).unwrap();
            w.insert_resource(native);
            scenario(&mut w, &bank, patch)
        };
        assert!(same(&restarted, &fresh), "restart = fresh start from that point");
        // A mod replacing the bank's samples (a tone of another rate and length) is heard...
        let dir = std::env::temp_dir().join(format!("skate-audio-restart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        let samples = world.resource::<Library>().bank_len(&bank);
        let tone: Vec<u8> = super::super::library::tests::test_wav(22050, 22050, 6000);
        std::fs::write(dir.join("audio/tone.wav"), &tone).unwrap();
        let overlay = serde_json::json!({"version": 1, "replace": {"banks": {bank.clone(): {"samples": vec!["audio/tone.wav"; samples]}}}});
        std::fs::write(dir.join("audio.json"), overlay.to_string()).unwrap();
        world.resource_mut::<AudioContent>().sync([("dev.restart", dir.as_path(), 1)].into_iter());
        restart(&mut world);
        assert!(world.resource::<AudioContent>().report.applied == ["dev.restart"], "{:?}", world.resource::<AudioContent>().report);
        let modded = scenario(&mut world, &bank, patch);
        assert!(!same(&modded, &fresh), "the mod's samples play");
        // ...and removing it restores retail: the output equals a fresh no-mod runtime's.
        world.resource_mut::<AudioContent>().sync(std::iter::empty());
        let next = world.resource::<Native>().next_node();
        restart(&mut world);
        assert_eq!(world.resource::<AudioContent>().restarts, 3);
        let restored = scenario(&mut world, &bank, patch);
        let fresh = {
            let mut w = World::new();
            w.insert_resource(Library::load(root).unwrap());
            let native = Native::start_from(w.resource::<Library>(), WorldInstances::RETAIL, next).unwrap();
            w.insert_resource(native);
            scenario(&mut w, &bank, patch)
        };
        assert!(same(&restored, &fresh), "removed mod = retail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L4 (data-gated): a mod's Csis project brings a class and a global; its preloaded bank binds
    /// to the class. Swapped in (no restart): the class and global resolve, a post to the class
    /// plays the mod bank. Swapped out: the names resolve no more and the bank is gone; retail's
    /// lookups never changed. A project reusing a retail name is left out (first owner wins); a
    /// restart installs the mod project after the install's.
    #[test]
    #[ignore = "needs the private install data"]
    fn a_mod_csis_project_is_installed_and_taken_out_without_a_restart() {
        use super::super::native::Native;
        use skate_audio::eval::synthetic;
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"));
        if Library::load(root).is_err() {
            panic!("missing private data: no audio install");
        }
        let mut world = game_world(root);
        let retail_emitter = world.resource::<Native>().shared.lock().unwrap().eval.class_id("c_emitter");
        let dir = std::env::temp_dir().join(format!("skate-audio-csi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        let sym = |name: &str, name_id: u16, default: i32| skate_audio::formats::csi::Symbol { name: name.into(), name_id, default };
        let project = |class: &str| skate_audio::formats::Project { name: "mod.csi".into(), id: synthetic::PROJECT, tables: [vec![], vec![sym(class, 1, 0)], vec![sym("g_dev_mod", 2, 41)]] };
        std::fs::write(dir.join("audio/mod.csi"), project("c_dev_mod").to_bytes()).unwrap();
        let abk = synthetic::bank(&[synthetic::player_module(2)], &[synthetic::Ex { module: 0, kind: 1, name_id: 1, name: "c_dev_mod", at: None }], &[(4800, false)]).data;
        std::fs::write(dir.join("audio/mod.abk"), &abk).unwrap();
        std::fs::write(dir.join("audio/a.wav"), super::super::library::tests::test_wav(4800, 48000, 9000)).unwrap();
        std::fs::write(dir.join("audio.json"), serde_json::json!({"version": 1, "add": {
            "projects": ["audio/mod.csi"],
            "banks": {"MOD_csi": {"abk": "audio/mod.abk", "samples": ["audio/a.wav"], "preload": true, "group": "world"}}}}).to_string()).unwrap();
        world.resource_mut::<AudioContent>().sync([("dev.csi", dir.as_path(), 1)].into_iter());
        frame(&mut world);
        {
            let c = world.resource::<AudioContent>();
            assert_eq!((c.restarts, c.swaps), (0, 1), "{:?} {:?}", c.last_change, c.report.rejected);
        }
        {
            let n = world.resource::<Native>();
            assert!(n.bank_loaded("MOD_csi"));
            assert_eq!(n.mod_projects.len(), 1);
            let mut rt = n.shared.lock().unwrap();
            let class = rt.eval.class_id("c_dev_mod").expect("the mod's class");
            let g = rt.eval.global_id("g_dev_mod").expect("the mod's global");
            assert_eq!(rt.eval.global(g), Some(41));
            let before = rt.eval.instance_count();
            rt.post(class, &[1, 4096, 0]);
            assert_eq!(rt.eval.instance_count(), before + 1, "the mod bank answers the mod class");
            assert_eq!(rt.eval.class_id("c_emitter"), retail_emitter);
        }
        world.resource_mut::<AudioContent>().sync(std::iter::empty());
        frame(&mut world);
        {
            assert_eq!((world.resource::<AudioContent>().restarts, world.resource::<AudioContent>().swaps), (0, 2));
            let n = world.resource::<Native>();
            assert!(!n.bank_loaded("MOD_csi") && n.mod_projects.is_empty());
            let rt = n.shared.lock().unwrap();
            assert!(rt.eval.class_id("c_dev_mod").is_none() && rt.eval.global_id("g_dev_mod").is_none());
            assert_eq!(rt.eval.class_id("c_emitter"), retail_emitter);
        }
        // A project that reuses a retail class name is left out.
        std::fs::write(dir.join("audio/mod.csi"), project("c_emitter").to_bytes()).unwrap();
        world.resource_mut::<AudioContent>().sync([("dev.csi", dir.as_path(), 2)].into_iter());
        frame(&mut world);
        assert!(world.resource::<AudioContent>().report.rejected.iter().any(|m| m.owner == "dev.csi" && m.text.contains("c_emitter")), "{:?}", world.resource::<AudioContent>().report);
        // A restart with the mod: its project after the install's.
        std::fs::write(dir.join("audio/mod.csi"), project("c_dev_mod").to_bytes()).unwrap();
        world.resource_mut::<AudioContent>().sync([("dev.csi", dir.as_path(), 3)].into_iter());
        restart(&mut world);
        let n = world.resource::<Native>();
        assert_eq!(n.mod_projects.len(), 1);
        assert!(n.shared.lock().unwrap().eval.class_id("c_dev_mod").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    pub(crate) fn render(world: &World, blocks: usize) -> Vec<f32> {
        let n = world.resource::<super::super::native::Native>();
        let mut out = vec![0.0f32; 2 * skate_audio::BLOCK];
        let mut all = Vec::with_capacity(blocks * out.len());
        for _ in 0..blocks {
            n.shared.lock().unwrap().fill_stereo(&mut out);
            all.extend_from_slice(&out);
        }
        all
    }

    /// The bank the runtime holds under `stem` equals what loading it from `library` gives (its
    /// samples' headers, its program bytes): what a restart would have loaded.
    fn holds_library_bank(world: &World, library: &Library, stem: &str) -> bool {
        let n = world.resource::<super::super::native::Native>();
        let id = n.bank_id(stem).expect("loaded");
        let (fresh, _) = library.bank_source(stem).unwrap().load().unwrap();
        let rt = n.shared.lock().unwrap();
        let held = rt.eval.bank(id).unwrap();
        held.data == fresh.data && held.samples == fresh.samples
    }

    /// L1 (data-gated): a mod's audio content coming and going is swapped in place, no restart:
    /// the runtime (and its stream) stays, the replaced bank keeps its runtime id and its place
    /// among its class's constructors and holds exactly the new library's content, a held post of
    /// it keeps sounding on the new samples (re-bound with its payload), an unrelated held post
    /// keeps its instance. Removing the mod swaps the install's bank back. A MixMap replacement
    /// cannot be swapped exactly: it restarts (the fallback, with its reason).
    #[test]
    #[ignore = "needs the private install data"]
    fn a_mod_is_swapped_in_and_out_without_a_restart() {
        use super::super::native::Native;
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"));
        if Library::load(root).is_err() {
            panic!("missing private data: no audio install");
        }
        let (mut a, mut b) = (game_world(root), game_world(root));
        let (bank, patch) = emitter_bank(a.resource::<Library>());
        let mut nodes = Vec::new();
        for w in [&mut a, &mut b] {
            let library = w.remove_resource::<Library>().unwrap();
            let mut native = w.resource_mut::<Native>();
            native.ensure_bank(&library, &bank).unwrap();
            let payload = native.emitter_payload(None, 0.8, 9000, patch);
            nodes.push(native.post_emitter(&payload).expect("c_emitter"));
            drop(native);
            w.insert_resource(library);
        }
        assert!(same(&render(&a, 200), &render(&b, 200)), "two equal worlds");
        let shared = std::sync::Arc::as_ptr(&a.resource::<Native>().shared);
        let id = a.resource::<Native>().bank_id(&bank).unwrap();
        let utility = a.resource::<Native>().bank_id("emitter_utility").unwrap();
        let (class, constructors, utility_instances) = {
            let rt = a.resource::<Native>().shared.lock().unwrap();
            let class = rt.eval.node_class(nodes[0]).unwrap();
            (class, rt.eval.registry.classes[class].constructors.clone(), rt.eval.instances().into_iter().filter(|i| i.1 == utility).collect::<Vec<_>>())
        };
        let dir = std::env::temp_dir().join(format!("skate-audio-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        let samples = a.resource::<Library>().bank_len(&bank);
        std::fs::write(dir.join("audio/tone.wav"), super::super::library::tests::test_wav(22050, 22050, 6000)).unwrap();
        std::fs::write(dir.join("audio.json"), serde_json::json!({"version": 1, "replace": {"banks": {bank.clone(): {"samples": vec!["audio/tone.wav"; samples]}}}}).to_string()).unwrap();
        a.resource_mut::<AudioContent>().sync([("dev.swap", dir.as_path(), 1)].into_iter());
        frame(&mut a);
        {
            let c = a.resource::<AudioContent>();
            assert_eq!((c.restarts, c.swaps, c.runtime_generation, c.last_change.as_deref()), (0, 1, 0, Some("swap")), "{:?}", c.last_change);
            assert_eq!(c.report.applied, ["dev.swap"]);
        }
        assert_eq!(std::sync::Arc::as_ptr(&a.resource::<Native>().shared), shared, "the same runtime");
        assert_eq!(super::super::native::NativeOutputCount::of(&mut a), 1, "the same stream");
        assert_eq!(a.resource::<Native>().bank_id(&bank), Some(id), "the same bank id");
        assert!(holds_library_bank(&a, a.resource::<Library>(), &bank), "the new library's bank");
        {
            let rt = a.resource::<Native>().shared.lock().unwrap();
            assert_eq!(rt.eval.registry.classes[class].constructors, constructors, "the same constructor places");
            assert_eq!(rt.eval.node_class(nodes[0]), Some(class), "the post is still held");
            assert!(rt.eval.instances().iter().any(|i| i.1 == id), "and re-bound to the new bank");
            assert_eq!(rt.eval.instances().into_iter().filter(|i| i.1 == utility).collect::<Vec<_>>(), utility_instances, "an unrelated post keeps its instance");
        }
        let (modded, retail) = (render(&a, 600), render(&b, 600));
        assert!(modded.iter().any(|s| *s != 0.0) && !same(&modded, &retail), "the mod's samples play on the held post");
        // L7: the WAV edited in place while the mod runs (same path, new content; the package's
        // fingerprint changes): swapped again, its new header in the runtime.
        let header = |w: &World| {
            let n = w.resource::<Native>();
            let rt = n.shared.lock().unwrap();
            rt.eval.bank(id).unwrap().samples[0].1
        };
        let before = header(&a);
        std::fs::write(dir.join("audio/tone.wav"), super::super::library::tests::test_wav(11025, 22050, 3000)).unwrap();
        a.resource_mut::<AudioContent>().sync([("dev.swap", dir.as_path(), 2)].into_iter());
        frame(&mut a);
        assert_eq!((a.resource::<AudioContent>().restarts, a.resource::<AudioContent>().swaps), (0, 2));
        assert!(holds_library_bank(&a, a.resource::<Library>(), &bank) && header(&a) != before, "the edited WAV");
        // Out again: the install's bank is swapped back.
        a.resource_mut::<AudioContent>().sync(std::iter::empty());
        frame(&mut a);
        assert_eq!((a.resource::<AudioContent>().restarts, a.resource::<AudioContent>().swaps), (0, 3));
        assert!(holds_library_bank(&a, b.resource::<Library>(), &bank), "the install's bank again");
        assert!(render(&a, 300).iter().any(|s| *s != 0.0), "still sounding");
        // A MixMap replacement restarts (the fallback).
        let mxb = a.resource::<Library>().aems().mixmap.clone().expect("a MixMap");
        std::fs::copy(a.resource::<Library>().path(&mxb), dir.join("audio/mix.mxb")).unwrap();
        std::fs::write(dir.join("audio.json"), serde_json::json!({"version": 1, "replace": {"mixmap": "audio/mix.mxb"}}).to_string()).unwrap();
        a.resource_mut::<AudioContent>().sync([("dev.swap", dir.as_path(), 3)].into_iter());
        frame(&mut a);
        let c = a.resource::<AudioContent>();
        assert_eq!((c.restarts, c.swaps), (1, 3));
        assert!(c.last_change.as_deref().is_some_and(|l| l.starts_with("restart: the MixMap file changed")), "{:?}", c.last_change);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
