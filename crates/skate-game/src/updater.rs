//! Updates: a check at start-up and the pause menu's Updates row (EXTRAS).
//! - PC (pc.rs): the packaged helper, tools/updater.py, owns networking, its window and staging.
//! - Switch (switch.rs): skate-switch-update replaces the NRO with the newest release the SD
//!   card's game files work with (docs/switch/README.md, "Updates").
#[cfg(not(target_os = "horizon"))]
mod pc;
#[cfg(target_os = "horizon")]
mod switch;

#[cfg(not(target_os = "horizon"))]
pub(crate) use pc::{Updater, UpdaterPlugin, recover};
#[cfg(target_os = "horizon")]
pub(crate) use switch::{Updater, UpdaterPlugin, recover};
