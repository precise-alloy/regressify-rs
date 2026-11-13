//! Reuse of generated capture previews across snapshot generations.
//!
//! Decoding captures and re-encoding their previews dominates snapshot time, while most images
//! are byte-identical between generations. The index maps each published image path to the
//! cache version of the source bytes it was generated from, the grid thumbnail modes it used,
//! and the cache versions of the generated files. A reuse is only accepted when the source
//! bytes, modes, and generator all match and the previously published files still hash to the
//! recorded versions, so a stale, edited, or partially published index can only cause a cache
//! miss, never a preview that does not belong to its source.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::site::{checked_file, read_bounded};

/// Kept outside `_snapshot` so pruning, artifact inventories, and deployments never see it; the
/// managed output `.gitignore` already ignores top-level dotted files.
pub(super) const CACHE_FILE: &str = ".preview-cache.json";
/// Bump whenever preview, grid thumbnail, or report generation changes without a release
/// version change, because development builds of one commit share the generator identity.
/// Snapshot input fingerprints include it for the same reason.
pub(super) const FORMAT: u32 = 1;
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;

fn generator() -> &'static str {
    super::site::GENERATOR
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheFile {
    format: u32,
    generator: String,
    images: BTreeMap<String, CachedImage>,
}

/// Cache versions (`get_asset_version`, 20 characters) of one published image's files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CachedImage {
    pub source: String,
    pub preview: String,
    /// Grid thumbnail versions keyed by mode suffix; the key set must match exactly.
    pub grids: BTreeMap<String, String>,
}

#[derive(Default)]
pub(super) struct PreviewCache {
    images: BTreeMap<String, CachedImage>,
}

impl PreviewCache {
    /// The cache is an optimization: an unreadable or foreign index only disables reuse.
    pub(super) fn load(root: &Path) -> Self {
        match Self::read(root) {
            Ok(cache) => cache,
            Err(error) => {
                warn(&format!(
                    "Warning: Ignoring unreadable snapshot preview cache '{}'; previews will be regenerated: {error:#}",
                    root.join(CACHE_FILE).display()
                ));
                Self::default()
            }
        }
    }

    fn read(root: &Path) -> Result<Self> {
        let Some(path) = checked_file(root, Path::new(CACHE_FILE), true)? else {
            return Ok(Self::default());
        };
        let file: CacheFile = serde_json::from_slice(&read_bounded(&path, MAX_CACHE_BYTES)?)
            .context("Invalid snapshot preview cache")?;
        if file.format != FORMAT || file.generator != generator() {
            return Ok(Self::default());
        }
        Ok(Self {
            images: file.images,
        })
    }

    pub(super) fn get(&self, key: &str) -> Option<&CachedImage> {
        self.images.get(key)
    }

    /// Replaces the index with exactly the images of the generation that was just published.
    pub(super) fn save(root: &Path, images: BTreeMap<String, CachedImage>) -> Result<()> {
        let bytes = serde_json::to_vec(&CacheFile {
            format: FORMAT,
            generator: generator().into(),
            images,
        })?;
        anyhow::ensure!(
            bytes.len() <= MAX_CACHE_BYTES,
            "Snapshot preview cache exceeds its size limit"
        );
        crate::helpers::write_file_atomically(&root.join(CACHE_FILE), &bytes).with_context(|| {
            format!(
                "Failed to save snapshot preview cache '{}'",
                root.join(CACHE_FILE).display()
            )
        })
    }
}

pub(super) fn key(suite: &str, role: &str, name: &str) -> String {
    format!("{suite}/{role}/{name}")
}

/// Cache version of a stored asset URL, as produced by `store_asset`.
pub(super) fn url_version(url: &str) -> Option<&str> {
    url.split_once("?v=").map(|(_, version)| version)
}

pub(super) fn warn(message: &str) {
    use crate::diagnostics::{record::*, runtime};
    eprintln!("{message}");
    runtime::event(
        runtime::current_stage().unwrap_or(Stage::SnapshotStaging),
        DiagnosticEvent::Decision,
        Severity::Warn,
        // The message can contain machine paths; diagnostics only record which cache failed.
        [(
            Field::Artifact,
            FieldValue::Identity(SafeIdentity::source("snapshot preview cache")),
        )],
    );
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
    };

    use serde_json::json;

    use super::{CACHE_FILE, CacheFile};
    use crate::{
        cli::args::SnapshotArgs, config::WorkspaceConfig, snapshot::helpers::get_asset_version,
    };

    fn png(width: u32, value: u8) -> Vec<u8> {
        let pixels = (0..width * 4)
            .flat_map(|index| [value, (index % 251) as u8, value / 2, 255])
            .collect::<Vec<_>>();
        lodepng::encode32(&pixels, width as usize, 4).unwrap()
    }

    /// An untracked suite with one failed capture that has reference and test images: a
    /// mismatch, or a capture error without a comparison when `error` is set.
    fn fixture(root: &Path, error: Option<&str>) {
        let suite = root.join("suite");
        for path in ["html_report", "bitmaps_test/current", "bitmaps_reference"] {
            fs::create_dir_all(suite.join(path)).unwrap();
        }
        let reference = suite.join("bitmaps_reference/home.png");
        if !reference.exists() {
            fs::write(&reference, png(20, 40)).unwrap();
            fs::write(suite.join("bitmaps_test/current/home.png"), png(20, 90)).unwrap();
        }
        let mut report = json!({ "id": "suite", "tests": [{ "status": "fail", "pair": {
            "label": "Home", "scenarioLabel": "Home", "selector": "document", "fileName": "home.png",
            "reference": "../bitmaps_reference/home.png", "test": "../bitmaps_test/current/home.png",
            "misMatchThreshold": 0.1, "engineErrorMsg": error,
            "diff": { "isSameDimensions": true, "dimensionDifference": { "width": 0, "height": 0 }, "rawMisMatchPercentage": 1.0 }
        }}] });
        if error.is_some() {
            // A measured mismatch would turn the message into a comparison diagnostic.
            report["tests"][0]["pair"]
                .as_object_mut()
                .unwrap()
                .remove("diff");
        }
        fs::write(
            suite.join("html_report/config.js"),
            format!("report({report});"),
        )
        .unwrap();
        fs::write(
            suite.join("bitmaps_test/current/report.json"),
            report.to_string(),
        )
        .unwrap();
    }

    fn generate_suites(root: &Path, suites: &[String]) {
        crate::snapshot::site::generate_site(
            root,
            suites,
            &SnapshotArgs {
                test_suite: Vec::new(),
                title: None,
                heading: None,
                subheading: None,
            },
            &WorkspaceConfig::default(),
        )
        .unwrap();
    }

    fn generate(root: &Path) {
        generate_suites(root, &["suite".into()]);
    }

    fn index(root: &Path) -> CacheFile {
        serde_json::from_slice(&fs::read(root.join(CACHE_FILE)).unwrap()).unwrap()
    }

    fn save_index(root: &Path, index: &CacheFile) {
        fs::write(root.join(CACHE_FILE), serde_json::to_vec(index).unwrap()).unwrap();
    }

    /// Generated files other than the timestamped manifest and status.
    fn generated(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        walkdir::WalkDir::new(root.join("_snapshot"))
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.path().strip_prefix(root).unwrap().to_path_buf())
            .filter(|path| !path.ends_with("manifest.json") && !path.ends_with("status.json"))
            .map(|path| {
                let bytes = fs::read(root.join(&path)).unwrap();
                (path, bytes)
            })
            .collect()
    }

    /// Simulates a cached preview by replacing the published file and its recorded version
    /// with distinguishable bytes: only a cache hit can publish them again.
    fn plant_preview(root: &Path, role: &str) -> Vec<u8> {
        let marker = png(3, 7);
        fs::write(
            root.join(format!("_snapshot/images/suite/{role}-preview/home.png")),
            &marker,
        )
        .unwrap();
        let mut cache = index(root);
        cache
            .images
            .get_mut(&format!("suite/{role}/home.png"))
            .unwrap()
            .preview = get_asset_version(&marker, 20);
        save_index(root, &cache);
        marker
    }

    fn preview(root: &Path, role: &str) -> Vec<u8> {
        fs::read(root.join(format!("_snapshot/images/suite/{role}-preview/home.png"))).unwrap()
    }

    #[test]
    fn cached_generation_matches_a_fresh_one_and_stays_out_of_the_published_site() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fixture(&root, None);
        generate(&root);
        let fresh = generated(&root);
        let cache = index(&root);
        assert_eq!(
            cache.images.keys().collect::<Vec<_>>(),
            ["suite/reference/home.png", "suite/test/home.png"]
        );
        assert_eq!(
            cache.images["suite/test/home.png"]
                .grids
                .keys()
                .collect::<Vec<_>>(),
            ["top"]
        );
        assert!(cache.images["suite/reference/home.png"].grids.is_empty());
        generate(&root);
        assert_eq!(generated(&root), fresh);
        // The index is private build state: never pruned, inventoried, or deployed.
        assert!(root.join(CACHE_FILE).is_file());
        assert!(
            !crate::snapshot::site::artifact_paths(&root, &["suite".into()])
                .unwrap()
                .iter()
                .any(|path| path.to_string_lossy().contains("cache"))
        );
    }

    #[test]
    fn previews_are_reused_only_for_identical_sources_modes_and_generator() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fixture(&root, None);
        generate(&root);
        let fresh_reference = preview(&root, "reference");
        let fresh_test = preview(&root, "test");

        // Identical source bytes reuse the recorded preview instead of decoding again.
        let marker = plant_preview(&root, "test");
        generate(&root);
        assert_eq!(preview(&root, "test"), marker);
        let report = fs::read_to_string(root.join("_snapshot/data/suites/suite.json")).unwrap();
        assert!(report.contains(&get_asset_version(&marker, 20)));

        // Approving a new baseline keeps the run but changes the reference bytes.
        let marker = plant_preview(&root, "test");
        fs::write(root.join("suite/bitmaps_reference/home.png"), png(20, 41)).unwrap();
        generate(&root);
        assert_eq!(preview(&root, "test"), marker);
        assert_ne!(preview(&root, "reference"), fresh_reference);
        let fresh_reference = preview(&root, "reference");

        // Another generator may encode previews differently.
        plant_preview(&root, "test");
        let mut cache = index(&root);
        cache.generator = "0.0.0 (other)".into();
        save_index(&root, &cache);
        generate(&root);
        assert_eq!(preview(&root, "test"), fresh_test);
        assert_eq!(index(&root).generator, super::generator());

        // A published file that no longer matches its recorded version is regenerated.
        plant_preview(&root, "test");
        fs::write(
            root.join("_snapshot/images/suite/test-preview/home.png"),
            png(5, 9),
        )
        .unwrap();
        generate(&root);
        assert_eq!(preview(&root, "test"), fresh_test);

        // A missing grid thumbnail is a miss for the whole image, not a partial reuse.
        plant_preview(&root, "test");
        fs::remove_file(root.join("_snapshot/images/suite/test-thumbnail/home.png")).unwrap();
        generate(&root);
        assert_eq!(preview(&root, "test"), fresh_test);
        assert!(
            root.join("_snapshot/images/suite/test-thumbnail/home.png")
                .is_file()
        );

        // An unreadable index only disables reuse and is replaced by a valid one.
        plant_preview(&root, "test");
        fs::write(root.join(CACHE_FILE), b"{not json").unwrap();
        generate(&root);
        assert_eq!(preview(&root, "test"), fresh_test);
        assert_eq!(preview(&root, "reference"), fresh_reference);
        assert_eq!(index(&root).images.len(), 2);
    }

    #[test]
    fn grid_mode_changes_regenerate_thumbnails_for_unchanged_pixels() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fixture(&root, None);
        generate(&root);
        let marker = plant_preview(&root, "test");
        // A capture error moves the grid from the test image to a dimmed reference overlay,
        // so the test image keeps its pixels but loses its top-crop thumbnail.
        fixture(&root, Some("Navigation timeout"));
        generate(&root);
        assert_ne!(preview(&root, "test"), marker);
        let cache = index(&root);
        assert!(cache.images["suite/test/home.png"].grids.is_empty());
        assert_eq!(
            cache.images["suite/reference/home.png"]
                .grids
                .keys()
                .collect::<Vec<_>>(),
            ["error-overlay"]
        );
        // The new entry is reused on the next unchanged generation.
        let marker = plant_preview(&root, "reference");
        generate(&root);
        assert_eq!(preview(&root, "reference"), marker);
    }

    #[test]
    fn deselected_suites_leave_the_index_with_their_pruned_previews() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fixture(&root, None);
        generate(&root);
        generate_suites(&root, &[]);
        assert!(index(&root).images.is_empty());
        assert!(!root.join("_snapshot/images/suite").exists());
    }
}
