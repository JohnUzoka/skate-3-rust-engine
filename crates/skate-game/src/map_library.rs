//! Installed map discovery and the default pointer, shared by startup and the menu.
//!
//! On the Switch the test world is opt-in: every run of it there hung the GPU (findings 22 §2.3
//! and 55) until bevy-horizon's Mesa patch 0032 (`NAK_DEBUG=hzero`). The menu offers it, and an
//! unset default starts it, only with `SKATE_TEST_WORLD=1`; otherwise an unset default starts an
//! installed map, and with no map installed the game stops with an error. `SWITCH_TEST_WORLD`
//! false takes the opt-in away too: `--test-world` is then an error and `SKATE_TEST_WORLD=1` is
//! ignored.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub label: String,
    pub path: Option<PathBuf>,
}

/// Whether the Switch build can start the test world. `true` brings back the opt-in the hang
/// investigation used: `SKATE_TEST_WORLD=1` (an `env` line in options.txt) starts on it, and the
/// menu then offers it. Without that line the Switch still hides it.
const SWITCH_TEST_WORLD: bool = true;

/// Whether this build can start the test world at all: everywhere but the Switch
/// (`SWITCH_TEST_WORLD`).
pub(crate) const TEST_WORLD_AVAILABLE: bool = !cfg!(target_os = "horizon") || SWITCH_TEST_WORLD;

/// `SKATE_TEST_WORLD=1`: start on the test world, as `--test-world` does.
pub(crate) fn test_world_requested() -> bool {
    std::env::var("SKATE_TEST_WORLD").is_ok_and(|v| v.trim() == "1")
}

/// Whether the menu offers the test world and an unset default starts it: everywhere but the
/// Switch, and there only when `SWITCH_TEST_WORLD` allows it and `SKATE_TEST_WORLD=1` asks.
fn test_world_offered() -> bool {
    !cfg!(target_os = "horizon") || (SWITCH_TEST_WORLD && test_world_requested())
}

/// Whether picking the test world makes it the default. Never on the Switch, so a test run
/// cannot leave every later launch on it.
pub(crate) const SAVE_TEST_WORLD: bool = !cfg!(target_os = "horizon");

pub(crate) fn discover(assets: &Path) -> Vec<Entry> {
    discover_with(assets, test_world_offered())
}

fn discover_with(assets: &Path, test_world: bool) -> Vec<Entry> {
    let mut maps = Vec::new();
    let root = assets.parent().unwrap_or(assets).join("maps");
    for directory in [&root, &root.join("private")] {
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("skate")) {
                    maps.push(Entry {
                        label: path.file_stem().unwrap().to_string_lossy().replace('_', " "),
                        path: Some(path.canonicalize().unwrap_or(path)),
                    });
                }
            }
        }
    }
    maps.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    if test_world {
        maps.insert(0, Entry { label: "Test world".into(), path: None });
    }
    maps
}

/// The map to start on without `--map`: the installed map `SKATE_START_MAP` names (its file's
/// name without `.skate`), for a launch that must start where earlier ones did whatever was
/// played last, as the Switch's `options.txt` can set it; it leaves the saved default as it is.
/// Otherwise the default pointer's. With none saved (no pointer, or `null`), the test world where
/// it is offered; elsewhere the map the PC installer makes the default
/// (tools/asset_pipeline/install.py): University, else the first, else an error.
pub(crate) fn start_map(assets: &Path) -> Result<Option<PathBuf>, String> {
    if let Some(name) = std::env::var("SKATE_START_MAP").ok().filter(|name| !name.trim().is_empty()) {
        let maps = discover_with(assets, false);
        match maps.iter().find(|m| m.label.eq_ignore_ascii_case(name.trim())).and_then(|m| m.path.clone()) {
            Some(path) => {
                eprintln!("[Skate] SKATE_START_MAP={name}: starting {}, not the saved default", path.display());
                return Ok(Some(path));
            }
            None => eprintln!("[Skate] SKATE_START_MAP={name}: no installed map by that name; starting the saved default"),
        }
    }
    start_map_with(assets, test_world_offered())
}

fn start_map_with(assets: &Path, test_world: bool) -> Result<Option<PathBuf>, String> {
    let saved = default_map(assets)?;
    if saved.is_some() || test_world {
        return Ok(saved);
    }
    let maps = discover_with(assets, false);
    let map = maps.iter().find(|m| m.label == "University").or(maps.first()).and_then(|m| m.path.clone())
        .ok_or_else(|| format!("No maps installed in {}, and the Switch build has no test world to start instead",
            assets.parent().unwrap_or(assets).join("maps").display()))?;
    eprintln!("[Skate] no default map saved: starting {}, not the test world", map.display());
    Ok(Some(map))
}

pub(crate) fn default_map(assets: &Path) -> Result<Option<PathBuf>, String> {
    // Only a completed release installation creates this pointer. Existing
    // development checkouts continue to boot the test world.
    let pointer = assets.parent().unwrap_or(assets).join("settings/default-map.json");
    let bytes = match std::fs::read(&pointer) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", pointer.display())),
    };
    let relative: Option<String> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let Some(relative) = relative else { return Ok(None); };
    let relative = Path::new(&relative);
    if relative.is_absolute() || relative.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err("Invalid installed default map path".into());
    }
    Ok(Some(assets.parent().unwrap_or(assets).join(relative)))
}

/// Save only a successfully activated map. Null explicitly selects the test world.
pub(crate) fn save_default(assets: &Path, map: Option<&Path>) -> Result<(), String> {
    let root = assets.parent().unwrap_or(assets);
    let relative = map.map(|path| path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .map_err(|_| "Map is outside this installation; default map unchanged".to_string())).transpose()?;
    let directory = root.join("settings");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let temporary = directory.join("default-map.json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&relative).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(temporary, directory.join("default-map.json")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_pointer_round_trips_map_and_explicit_test_world() {
        let root = std::env::temp_dir().join(format!("skate-map-pointer-{}", std::process::id()));
        let assets = root.join("assets");
        let map = root.join("maps/example.skate");
        save_default(&assets, Some(&map)).unwrap();
        assert_eq!(default_map(&assets).unwrap(), Some(map));
        save_default(&assets, None).unwrap();
        assert_eq!(default_map(&assets).unwrap(), None);
        std::fs::write(root.join("settings/default-map.json"), br#""../outside.skate""#).unwrap();
        assert!(default_map(&assets).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hidden_test_world_is_neither_listed_nor_started() {
        let root = std::env::temp_dir().join(format!("skate-map-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("maps")).unwrap();
        // save_default keeps only paths inside the installation, as canonical ones.
        let root = root.canonicalize().unwrap();
        let assets = root.join("assets");
        let labels = |test_world| discover_with(&assets, test_world).into_iter().map(|m| m.label).collect::<Vec<_>>();
        // No maps: the test world is all there is, and where it is not offered, nothing starts.
        assert_eq!(labels(true), ["Test world"]);
        assert!(labels(false).is_empty());
        assert_eq!(start_map_with(&assets, true).unwrap(), None);
        assert!(start_map_with(&assets, false).is_err());
        std::fs::write(root.join("maps/Black_Box_Park.skate"), b"").unwrap();
        let park = root.join("maps/Black_Box_Park.skate").canonicalize().unwrap();
        assert_eq!(labels(true), ["Test world", "Black Box Park"]);
        assert_eq!(labels(false), ["Black Box Park"]);
        assert_eq!(start_map_with(&assets, false).unwrap(), Some(park.clone()));
        std::fs::write(root.join("maps/University.skate"), b"").unwrap();
        let university = root.join("maps/University.skate").canonicalize().unwrap();
        // No pointer, then a `null` one: the test world where offered, else University.
        for pointer in [None, Some("null")] {
            if let Some(pointer) = pointer {
                std::fs::create_dir_all(root.join("settings")).unwrap();
                std::fs::write(root.join("settings/default-map.json"), pointer).unwrap();
            }
            assert_eq!(start_map_with(&assets, true).unwrap(), None);
            assert_eq!(start_map_with(&assets, false).unwrap(), Some(university.clone()));
        }
        // A saved map wins either way.
        save_default(&assets, Some(&park)).unwrap();
        assert_eq!(start_map_with(&assets, true).unwrap(), Some(park.clone()));
        assert_eq!(start_map_with(&assets, false).unwrap(), Some(park));
        std::fs::remove_dir_all(root).unwrap();
    }
}
