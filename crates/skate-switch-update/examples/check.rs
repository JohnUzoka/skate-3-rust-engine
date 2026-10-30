//! What Switch players on a version are offered, checked from a PC with the system's libcurl:
//!
//!     cargo run -p skate-switch-update --features system-curl --example check -- 0.1.4
//!
//! `--install <folder>` then downloads the offered NRO to `<folder>/skate3rust.nro` with every
//! check the Switch makes, and keeps a `skate3rust.nro` already there as
//! `skate3rust.nro.update.old`, as the Switch does. Run it after publishing a release
//! (docs/switch/README.md, "Publishing an update").
use skate_switch_update::{Curl, NRO_ASSET, Settings, State, Updater};
use std::{path::PathBuf, process::ExitCode, time::Duration};

const USAGE: &str = "usage: check <the version players have> [--install <folder>]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (installed, folder) = match args.as_slice() {
        [installed] => (installed, None),
        [installed, flag, folder] if flag == "--install" => (installed, Some(PathBuf::from(folder))),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let nro = folder.clone().unwrap_or_default().join(NRO_ASSET);
    let settings = match Settings::new(installed, nro) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut updater = Updater::new(settings, Curl::new(&format!("skate3rust-switch-check/{installed}")));
    updater.check();
    let state = wait(&updater);
    println!("{installed} is offered: {state:?}");
    if folder.is_none() || !updater.download() {
        return if matches!(state, State::Failed(_)) { ExitCode::FAILURE } else { ExitCode::SUCCESS };
    }
    let state = wait(&updater);
    println!("{state:?}");
    if matches!(state, State::Installed { .. }) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// The state once the check or download has finished, with the download's progress meanwhile.
fn wait(updater: &Updater) -> State {
    loop {
        match updater.state() {
            State::Checking => {}
            State::Downloading { done, offer } => eprint!("\r{} of {} MiB", done >> 20, offer.size >> 20),
            state => {
                eprintln!();
                return state;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
