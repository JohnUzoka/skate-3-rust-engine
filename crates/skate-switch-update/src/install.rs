//! Putting a checked download in the running NRO's place, and finishing or undoing that at the
//! next start if the game was stopped halfway.
//!
//! Horizon's file systems have no atomic replace, so the swap is two renames: the running NRO to
//! `<nro>.update.old`, then the download to `<nro>`. The old one stays until the next start
//! (`recover`), so an update that went wrong can be undone there, or by hand.
use semver::Version;
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use crate::nro;

/// Where a download goes until it has been checked.
pub(crate) const STAGED: &str = ".update.tmp";
/// Where the NRO it replaces stays until the next start.
pub(crate) const BACKUP: &str = ".update.old";

/// `nro` with `suffix` added to its file name.
pub(crate) fn beside(nro: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(nro.as_os_str());
    name.push(suffix);
    name.into()
}

/// Puts the checked download at `staged` in `nro`'s place, keeping the NRO it replaces as
/// `<nro>.update.old`, then checks that `nro` is the NRO for `version`. Anything that fails puts
/// the old NRO back.
pub(crate) fn install(nro: &Path, staged: &Path, version: &Version) -> Result<(), String> {
    let backup = beside(nro, BACKUP);
    if backup.exists() {
        fs::remove_file(&backup).map_err(|e| format!("couldn't delete the old {}: {e}", backup.display()))?;
    }
    let had_old = nro.exists();
    if had_old {
        fs::rename(nro, &backup).map_err(|e| format!("couldn't move {} aside: {e}", nro.display()))?;
    }
    let put_back = || {
        let _ = fs::remove_file(nro);
        if had_old {
            let _ = fs::rename(&backup, nro);
        }
    };
    if let Err(e) = fs::rename(staged, nro) {
        put_back();
        return Err(format!("couldn't move the download into place: {e}"));
    }
    match nro::version(nro) {
        Ok(text) if Version::parse(&text).is_ok_and(|found| found == *version) => Ok(()),
        Ok(text) => {
            put_back();
            Err(format!("the NRO in place shows version {text}, not {version}"))
        }
        Err(e) => {
            put_back();
            Err(format!("the NRO in place isn't whole: {e}"))
        }
    }
}

/// Run at start-up, before anything else: deletes a download an earlier run didn't finish, and
/// if an update was interrupted, keeps whichever NRO is whole. Returns what it did, for the log,
/// or `None` when there was nothing to do.
pub fn recover(nro: &Path) -> Result<Option<String>, String> {
    let mut done = Vec::new();
    let staged = beside(nro, STAGED);
    if staged.exists() {
        fs::remove_file(&staged).map_err(|e| format!("couldn't delete {}: {e}", staged.display()))?;
        done.push(format!("deleted the unfinished download {}", staged.display()));
    }
    let backup = beside(nro, BACKUP);
    if backup.exists() {
        if nro::version(nro).is_ok() {
            fs::remove_file(&backup).map_err(|e| format!("couldn't delete {}: {e}", backup.display()))?;
            done.push(format!("deleted {}, the version the last update replaced", backup.display()));
        } else if nro::version(&backup).is_ok() {
            let _ = fs::remove_file(nro);
            fs::rename(&backup, nro).map_err(|e| format!("couldn't put {} back: {e}", backup.display()))?;
            done.push(format!("put back {}: the update replacing it didn't finish", backup.display()));
        } else {
            return Err(format!("neither {} nor {} is a whole NRO", nro.display(), backup.display()));
        }
    }
    Ok((!done.is_empty()).then(|| done.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nro::fake;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skate-switch-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn install_swaps_in_the_download_and_keeps_the_old_nro() {
        let dir = folder("install");
        let (nro, staged) = (dir.join("skate3rust.nro"), dir.join("skate3rust.nro.update.tmp"));
        fs::write(&nro, fake("0.1.4")).unwrap();
        fs::write(&staged, fake("0.1.5")).unwrap();
        install(&nro, &staged, &Version::new(0, 1, 5)).unwrap();
        assert_eq!(fs::read(&nro).unwrap(), fake("0.1.5"));
        assert_eq!(fs::read(dir.join("skate3rust.nro.update.old")).unwrap(), fake("0.1.4"));
        assert!(!staged.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn install_puts_the_old_nro_back_when_the_new_one_is_wrong() {
        let dir = folder("install-wrong");
        let (nro, staged) = (dir.join("skate3rust.nro"), dir.join("skate3rust.nro.update.tmp"));
        fs::write(&nro, fake("0.1.4")).unwrap();
        fs::write(&staged, fake("0.1.6")).unwrap();
        assert!(install(&nro, &staged, &Version::new(0, 1, 5)).is_err());
        assert_eq!(fs::read(&nro).unwrap(), fake("0.1.4"));
        assert!(!dir.join("skate3rust.nro.update.old").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recover_keeps_the_whole_nro() {
        let dir = folder("recover");
        let nro = dir.join("skate3rust.nro");
        let (staged, backup) = (beside(&nro, STAGED), beside(&nro, BACKUP));
        assert_eq!(recover(&nro), Ok(None));

        // Stopped while downloading.
        fs::write(&nro, fake("0.1.4")).unwrap();
        fs::write(&staged, b"partial").unwrap();
        assert!(recover(&nro).unwrap().is_some());
        assert!(!staged.exists());

        // The update finished: the new NRO starts and drops the old one.
        fs::write(&backup, fake("0.1.3")).unwrap();
        assert!(recover(&nro).unwrap().is_some());
        assert!(!backup.exists());
        assert_eq!(fs::read(&nro).unwrap(), fake("0.1.4"));

        // Stopped between the two renames, or the new NRO is damaged.
        fs::rename(&nro, &backup).unwrap();
        assert!(recover(&nro).unwrap().is_some());
        assert_eq!(fs::read(&nro).unwrap(), fake("0.1.4"));
        fs::write(&backup, fake("0.1.3")).unwrap();
        fs::write(&nro, b"damaged").unwrap();
        recover(&nro).unwrap();
        assert_eq!(fs::read(&nro).unwrap(), fake("0.1.3"));
        assert!(!backup.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
