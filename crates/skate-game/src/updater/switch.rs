//! The Switch's updates (docs/switch/README.md, "Updates"). At start-up skate-switch-update asks
//! GitHub for a newer release the SD card's game files work with, unless options.txt has
//! `no_update_check`. The pause menu's Updates row shows what it found, and each press takes the
//! next step: check, download and install, restart into the new version. A press while checking
//! or downloading stops it.
use bevy::prelude::*;
use skate_switch_update::{Curl, HeldBack, Offer, Settings, State};
use std::path::Path;

/// The version this NRO was built as, the one its NACP shows: build_game.sh's VERSION (build.rs).
const VERSION: &str = env!("SKATE_SWITCH_VERSION");

#[derive(Resource)]
pub(crate) struct Updater {
    /// The updater, or why updates are off.
    updater: Result<skate_switch_update::Updater, String>,
    /// Set when the player asks to restart into an installed update; `poll` does it.
    restart: bool,
}

impl Default for Updater {
    fn default() -> Self {
        let updater = settings()
            .map(|settings| skate_switch_update::Updater::new(settings, Curl::new(&format!("skate3rust-switch/{VERSION}"))));
        if let Err(why) = &updater {
            info!("[Update] updates are off: {why}");
        }
        Self { updater, restart: false }
    }
}

fn settings() -> Result<Settings, String> {
    if VERSION.is_empty() {
        return Err("this build has no version number; release builds have one".into());
    }
    let nro = nro_path().ok_or("the game wasn't started from an .nro file on the SD card")?;
    Settings::new(VERSION, nro)
}

/// The running NRO, `argv[0]`, with the SD card's device name when the loader left it out.
fn nro_path() -> Option<String> {
    let path = bevy_horizon::nro_path()?;
    let path = if path.starts_with('/') { format!("sdmc:{path}") } else { path.to_owned() };
    path.to_ascii_lowercase().ends_with(".nro").then_some(path)
}

/// Deletes what an update cut short left beside the NRO, or puts the old NRO back if the new one
/// isn't whole. horizon::init runs it before anything else.
pub(crate) fn recover() {
    let Some(nro) = nro_path() else { return };
    match skate_switch_update::recover(Path::new(&nro)) {
        Ok(None) => {}
        Ok(Some(done)) => eprintln!("[Update] {done}"),
        Err(error) => eprintln!("[Update] couldn't tidy up after the last update: {error}"),
    }
}

impl Updater {
    /// `automatic` at start-up: a check, unless options.txt has `no_update_check`. Otherwise, the
    /// Updates row's next step. Returns nothing for the status line, where `status` shows the
    /// updater's state live while the row is selected.
    pub(crate) fn open(&mut self, automatic: bool) -> String {
        let updater = match &mut self.updater {
            Ok(updater) => updater,
            Err(_) => return String::new(),
        };
        if automatic {
            if crate::horizon::update_check_at_start() {
                updater.check();
            }
            return String::new();
        }
        match updater.state() {
            State::Available { .. } => {
                updater.download();
            }
            State::Checking | State::Downloading { .. } => updater.cancel(),
            State::Installed { .. } => self.restart = true,
            State::Idle | State::UpToDate { .. } | State::Failed(_) | State::Cancelled => {
                updater.check();
            }
        }
        String::new()
    }

    /// The menu row's text.
    pub(crate) fn label(&self) -> String {
        let Ok(updater) = &self.updater else { return "Updates".into() };
        match updater.state() {
            State::Checking => "Updates    checking...".into(),
            State::Available { offer, .. } => format!("Updates    {} available", offer.version),
            State::Downloading { offer, done } => format!("Updates    {}%", done * 100 / offer.size.max(1)),
            State::Installed { .. } => "Updates    restart to finish".into(),
            _ => "Updates".into(),
        }
    }

    /// The status line while the Updates row is selected.
    pub(crate) fn status(&self) -> Option<String> {
        let updater = match &self.updater {
            Ok(updater) => updater,
            Err(why) => return Some(format!("Updates are off: {why}.")),
        };
        Some(match updater.state() {
            State::Idle => format!("This is version {VERSION}. Select to look for a newer one."),
            State::Checking => "Looking for a newer version on GitHub... Select to stop.".into(),
            State::UpToDate { held_back: None } => format!("You have the newest version, {VERSION}."),
            State::UpToDate { held_back: Some(held) } => {
                format!("You have the newest version your game files work with, {VERSION}.\n{}", needs_files(&held))
            }
            State::Available { offer, held_back } => {
                let mut text = format!("{} is out ({}). Select to download and install it.", offer.version, megabytes(offer.size));
                for line in [summary(&offer), held_back.as_ref().map(needs_files)].into_iter().flatten() {
                    text = format!("{text}\n{line}");
                }
                text
            }
            State::Downloading { offer, done } => {
                format!("Downloading {}: {} of {}. Select to stop.", offer.version, megabytes(done), megabytes(offer.size))
            }
            State::Installed { offer } => {
                format!("{} is installed. Select to restart into it, or it starts the next time you play.", offer.version)
            }
            State::Failed(error) => format!("Update: {error}. Select to try again."),
            State::Cancelled => "Stopped. Select to look again.".into(),
        })
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1e6)
}

/// The first line of the release notes.
fn summary(offer: &Offer) -> Option<String> {
    let line = offer.notes.lines().map(|line| line.trim_start_matches(['#', '-', '*', ' ']).trim()).find(|line| !line.is_empty())?;
    Some(line.chars().take(120).collect())
}

fn needs_files(held: &HeldBack) -> String {
    format!(
        "{} needs game files set up by PC {}: update the PC version, then copy the files with that release's switch_export.py.",
        held.version, held.pc_build
    )
}

pub(crate) struct UpdaterPlugin;
impl Plugin for UpdaterPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Updater>()
            .add_systems(Startup, |mut updater: ResMut<Updater>| {
                updater.open(true);
            })
            .add_systems(Update, poll);
    }
}

/// Gives a finished check's or download's thread back, and restarts into an installed update
/// when the player asks: the homebrew loader starts the new NRO after this one quits, or, if it
/// takes no such request, the game quits to the homebrew menu.
fn poll(mut updater: ResMut<Updater>, mut exit: MessageWriter<AppExit>) {
    if let Ok(running) = &mut updater.updater {
        running.reap();
    }
    if !std::mem::take(&mut updater.restart) {
        return;
    }
    match nro_path() {
        Some(path) if bevy_horizon::set_next_load(&path) => info!("[Update] restarting into the new {path}"),
        _ => info!("[Update] the homebrew loader takes no restart request; quitting to the homebrew menu"),
    }
    exit.write(AppExit::Success);
}
