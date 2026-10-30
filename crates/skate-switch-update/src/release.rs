//! What GitHub's release listing and a release's `switch-release.json` say, and which releases a
//! check may offer.
use semver::Version;
use serde::Deserialize;
use std::collections::BTreeMap;

use crate::{MANIFEST_ASSET, NRO_ASSET, TAG_PREFIX};

/// The asset set this build reads: tools/switch_assets.json, the PC build whose exported files it
/// was made and tested with.
const BUNDLED: &str = include_str!("../../../tools/switch_assets.json");
/// The `schema` tools/switch_release.py writes into `switch-release.json`.
const MANIFEST_SCHEMA: u32 = 1;
/// NRO sizes outside these are no build of this game.
const NRO_SIZES: std::ops::RangeInclusive<u64> = (1 << 20)..=(512 << 20);
/// A `switch-release.json` is a few hundred bytes.
const MANIFEST_MAX: u64 = 64 << 10;

/// The asset files an NRO reads, which a PC build's setup makes from the player's own disc and
/// tools/switch_export.py copies to the SD card. An update can't change them, so it is only for
/// players whose files it can read.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct AssetSet {
    /// The PC build whose setup makes these files, as players know it: "experimental build 49".
    pub pc_build: String,
    /// Each asset group's pipeline fingerprint, from that PC build's release.json
    /// (tools/asset_pipeline/versions.py makes them).
    pub asset_pipelines: BTreeMap<String, String>,
    /// Per group, `[made by, read as]` fingerprint pairs that make the same files, as in the PC's
    /// tools/asset_pipeline/pipeline-equivalence.json.
    #[serde(default)]
    pub pipeline_equivalence: BTreeMap<String, Vec<[String; 2]>>,
}

impl AssetSet {
    /// This build's set (tools/switch_assets.json).
    pub fn bundled() -> Result<Self, String> {
        serde_json::from_str(BUNDLED).map_err(|e| format!("tools/switch_assets.json: {e}"))
    }

    /// Whether files exported for `exported` serve an NRO that wants `self`: every group it reads
    /// has the same fingerprint, or a pair `self` lists as equivalent. A group `self` doesn't read
    /// doesn't matter; one the files lack does.
    pub fn accepts(&self, exported: &AssetSet) -> bool {
        self.asset_pipelines.iter().all(|(group, wanted)| match exported.asset_pipelines.get(group) {
            Some(made) => {
                made == wanted
                    || self.pipeline_equivalence.get(group).is_some_and(|pairs| {
                        pairs.iter().any(|[from, to]| from == made && to == wanted)
                    })
            }
            None => false,
        })
    }
}

/// A release file: where it is, its size and the SHA-256 GitHub computed when it was uploaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asset {
    pub url: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// A published release newer than the running NRO, with both files an update needs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub version: Version,
    pub tag: String,
    pub notes: String,
    pub nro: Asset,
    pub manifest_url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ReleaseFile>,
}

#[derive(Deserialize)]
struct ReleaseFile {
    name: String,
    size: u64,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    digest: Option<String>,
    browser_download_url: String,
}

/// The releases in GitHub's listing (`GET /repos/{repository}/releases`) that are published, not
/// pre-releases, tagged `switch-v<version>` with a version above `installed`, and have an
/// uploaded `skate3rust.nro` with a SHA-256 digest and a `switch-release.json`, both downloaded
/// from this repository's releases. Newest first.
pub(crate) fn newer(listing: &[u8], repository: &str, installed: &Version) -> Result<Vec<Candidate>, String> {
    let releases: Vec<Release> =
        serde_json::from_slice(listing).map_err(|e| format!("GitHub's release list is unreadable: {e}"))?;
    let downloads = format!("https://github.com/{repository}/releases/download/");
    let mut candidates: Vec<Candidate> = releases
        .into_iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            let version = Version::parse(release.tag_name.strip_prefix(TAG_PREFIX)?).ok()?;
            if version <= *installed {
                return None;
            }
            let uploaded = |name: &str| {
                release.assets.iter().find(|file| {
                    file.name == name
                        && file.state.as_deref().is_none_or(|state| state == "uploaded")
                        && file.browser_download_url.starts_with(&downloads)
                })
            };
            let nro = uploaded(NRO_ASSET).filter(|file| NRO_SIZES.contains(&file.size))?;
            let manifest = uploaded(MANIFEST_ASSET).filter(|file| file.size <= MANIFEST_MAX)?;
            Some(Candidate {
                nro: Asset {
                    url: nro.browser_download_url.clone(),
                    size: nro.size,
                    sha256: sha256_digest(nro.digest.as_deref()?)?,
                },
                manifest_url: manifest.browser_download_url.clone(),
                version,
                tag: release.tag_name,
                notes: release.body.unwrap_or_default(),
            })
        })
        .collect();
    candidates.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(candidates)
}

/// GitHub's `digest` for a release file, `sha256:` and 64 hex digits.
fn sha256_digest(digest: &str) -> Option<[u8; 32]> {
    let hex = digest.strip_prefix("sha256:")?.as_bytes();
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(hex.chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

#[derive(Deserialize)]
struct Manifest {
    schema: u32,
    version: String,
    #[serde(flatten)]
    assets: AssetSet,
}

/// The asset set a release's `switch-release.json` says its NRO reads, once the file says it is
/// that release's.
pub(crate) fn manifest(json: &[u8], version: &Version) -> Result<AssetSet, String> {
    let manifest: Manifest = serde_json::from_slice(json).map_err(|e| format!("unreadable: {e}"))?;
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(format!("schema {}, but this build reads schema {MANIFEST_SCHEMA}", manifest.schema));
    }
    match Version::parse(&manifest.version) {
        Ok(stated) if stated == *version => {}
        _ => return Err(format!("it is for version {}, not {version}", manifest.version)),
    }
    if manifest.assets.asset_pipelines.is_empty() {
        return Err("it names no asset pipelines".into());
    }
    Ok(manifest.assets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const REPO: &str = "JohnUzoka/skate-3-rust-engine";

    fn file(tag: &str, name: &str, size: u64, digest: Option<&str>) -> Value {
        json!({
            "name": name, "size": size, "state": "uploaded", "digest": digest,
            "browser_download_url": format!("https://github.com/{REPO}/releases/download/{tag}/{name}"),
        })
    }

    fn release(tag: &str, draft: bool, prerelease: bool, files: Vec<Value>) -> Value {
        json!({ "tag_name": tag, "body": format!("notes for {tag}"), "draft": draft, "prerelease": prerelease, "assets": files })
    }

    fn both(tag: &str) -> Vec<Value> {
        vec![
            file(tag, NRO_ASSET, 100 << 20, Some(&format!("sha256:{}", "ab".repeat(32)))),
            file(tag, MANIFEST_ASSET, 600, None),
        ]
    }

    #[test]
    fn newer_keeps_published_switch_releases_that_have_both_files_newest_first() {
        let mut bad_url = both("switch-v0.1.9");
        bad_url[0]["browser_download_url"] = json!("https://example.com/skate3rust.nro");
        let mut no_digest = both("switch-v0.2.1");
        no_digest[0]["digest"] = Value::Null;
        let mut too_small = both("switch-v0.2.2");
        too_small[0]["size"] = json!(4096);
        let listing = json!([
            release("switch-v0.1.5", false, false, both("switch-v0.1.5")),
            release("switch-v0.3.0", true, false, both("switch-v0.3.0")),
            release("switch-v0.2.5", false, true, both("switch-v0.2.5")),
            release("experimental", false, false, both("experimental")),
            release("switch-v0.1.4", false, false, both("switch-v0.1.4")),
            release("switch-v0.1.3", false, false, both("switch-v0.1.3")),
            release("switch-v0.2.0", false, false, vec![both("switch-v0.2.0").remove(0)]),
            release("switch-v0.1.9", false, false, bad_url),
            release("switch-v0.2.1", false, false, no_digest),
            release("switch-v0.2.2", false, false, too_small),
            release("switch-v0.1.10", false, false, both("switch-v0.1.10")),
        ]);
        let candidates = newer(listing.to_string().as_bytes(), REPO, &Version::new(0, 1, 4)).unwrap();
        let tags: Vec<&str> = candidates.iter().map(|c| c.tag.as_str()).collect();
        assert_eq!(tags, ["switch-v0.1.10", "switch-v0.1.5"]);
        assert_eq!(candidates[0].nro.sha256, [0xab; 32]);
        assert_eq!(candidates[0].manifest_url, format!("https://github.com/{REPO}/releases/download/switch-v0.1.10/{MANIFEST_ASSET}"));
    }

    #[test]
    fn a_release_candidate_build_is_offered_its_final_release() {
        let listing = json!([release("switch-v0.1.3", false, false, both("switch-v0.1.3"))]);
        let installed = Version::parse("0.1.3-rc1").unwrap();
        assert_eq!(newer(listing.to_string().as_bytes(), REPO, &installed).unwrap().len(), 1);
    }

    fn set(pipelines: &[(&str, &str)], equivalence: &[(&str, &str, &str)]) -> AssetSet {
        let mut set = AssetSet::default();
        for (group, fingerprint) in pipelines {
            set.asset_pipelines.insert(group.to_string(), fingerprint.to_string());
        }
        for (group, from, to) in equivalence {
            set.pipeline_equivalence.entry(group.to_string()).or_default().push([from.to_string(), to.to_string()]);
        }
        set
    }

    #[test]
    fn accepts_matching_or_equivalent_groups_only() {
        let exported = set(&[("core", "a"), ("maps", "m1"), ("extra", "x")], &[]);
        assert!(set(&[("core", "a"), ("maps", "m1")], &[]).accepts(&exported), "a group it doesn't read");
        assert!(!set(&[("core", "a"), ("maps", "m2")], &[]).accepts(&exported), "a changed group");
        assert!(set(&[("core", "a"), ("maps", "m2")], &[("maps", "m1", "m2")]).accepts(&exported));
        assert!(!set(&[("core", "a"), ("maps", "m2")], &[("maps", "m2", "m1")]).accepts(&exported), "the pair the other way");
        assert!(!set(&[("core", "a"), ("audio", "s")], &[]).accepts(&exported), "a group the files lack");
    }

    #[test]
    fn manifest_must_be_the_release_s_own() {
        let body = |schema: u32, version: &str| {
            json!({ "schema": schema, "version": version, "pc_build": "experimental build 52", "asset_pipelines": { "core": "a" } })
                .to_string()
        };
        let version = Version::new(0, 1, 5);
        let read = manifest(body(1, "0.1.5").as_bytes(), &version).unwrap();
        assert_eq!((read.pc_build.as_str(), read.asset_pipelines["core"].as_str()), ("experimental build 52", "a"));
        assert!(manifest(body(1, "0.1.6").as_bytes(), &version).is_err());
        assert!(manifest(body(2, "0.1.5").as_bytes(), &version).is_err());
    }

    #[test]
    fn the_bundled_set_reads() {
        let bundled = AssetSet::bundled().unwrap();
        assert!(!bundled.pc_build.is_empty() && !bundled.asset_pipelines.is_empty());
        assert!(bundled.accepts(&bundled));
    }
}
