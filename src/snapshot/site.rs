use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use super::helpers::get_asset_version;
use crate::{cli::args::SnapshotArgs, config::WorkspaceConfig, helpers::write_file_atomically};

include!("data/assets.rs");
pub(crate) const FRONTEND_HTML: &str = include_str!("data/index.html");

pub(super) const MAX_REPORT_BYTES: usize = 32 * 1024 * 1024;
pub(super) const MAX_IMAGE_BYTES: usize = 512 * 1024 * 1024;
const STATUS_PATH: &str = "_snapshot/status.json";

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SiteStatus {
    version: u8,
    generated_at: String,
    suites: usize,
    total: usize,
    passed: usize,
    failed: usize,
    errors: usize,
}

impl SiteStatus {
    fn from_manifest(manifest: &Manifest) -> Result<Self> {
        let mut status = Self {
            version: 1,
            generated_at: manifest.generated_at.clone(),
            suites: manifest.suites.len(),
            total: 0,
            passed: 0,
            failed: 0,
            errors: 0,
        };
        for suite in &manifest.suites {
            ensure!(
                suite.passed.checked_add(suite.failed) == Some(suite.total)
                    && suite.errors <= suite.failed,
                "Invalid snapshot suite summary"
            );
            for (sum, count) in [
                (&mut status.total, suite.total),
                (&mut status.passed, suite.passed),
                (&mut status.failed, suite.failed),
                (&mut status.errors, suite.errors),
            ] {
                *sum = sum
                    .checked_add(count)
                    .filter(|value| (*value as u64) <= 9_007_199_254_740_991)
                    .context("Snapshot status count exceeds its limit")?;
            }
        }
        Ok(status)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Manifest {
    pub version: u8,
    pub title: String,
    pub heading: String,
    pub subheading: String,
    pub generated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regressify_version: Option<String>,
    pub suites: Vec<SuiteSummary>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SuiteSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub errors: usize,
    pub report: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SuiteReport {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub run_id: String,
    pub tests: Vec<Capture>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Capture {
    pub id: String,
    pub label: String,
    pub scenario: String,
    pub selector: String,
    pub status: String,
    pub threshold: f64,
    pub mismatch: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mismatch_pixels: Option<u64>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison_error: Option<String>,
    pub reference: Option<String>,
    pub test: Option<String>,
    pub diff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<crate::compare::image_compare::DiffAlignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_sizes: Option<ImageSizes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previews: Option<CapturePreviews>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<CaptureLogs>,
    pub reference_url: Option<String>,
    pub test_url: Option<String>,
    pub dimensions: Option<Dimensions>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct CaptureLogs {
    pub reference: Option<ExecutionLog>,
    pub test: Option<ExecutionLog>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExecutionLog {
    pub state: crate::diagnostics::journal::ArchiveState,
    pub operation_id: Option<crate::diagnostics::record::OperationId>,
    pub execution_id: Option<crate::diagnostics::record::ExecutionId>,
    pub chunks: Vec<String>,
    pub records: u64,
    pub missing_records: Option<u64>,
    pub build: Option<crate::diagnostics::journal::BuildIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct ImageSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ImageSizes {
    pub reference: Option<ImageSize>,
    pub test: Option<ImageSize>,
    pub diff: Option<ImageSize>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct CapturePreviews {
    pub reference: Option<String>,
    pub test: Option<String>,
    pub diff: Option<String>,
    pub grid: Option<String>,
}

impl CapturePreviews {
    fn images(&self) -> impl Iterator<Item = &String> {
        [&self.reference, &self.test, &self.diff, &self.grid]
            .into_iter()
            .flatten()
    }
}

impl Capture {
    fn images(&self) -> impl Iterator<Item = &String> {
        [&self.reference, &self.test, &self.diff]
            .into_iter()
            .flatten()
            .chain(self.previews.iter().flat_map(CapturePreviews::images))
    }

    #[cfg(feature = "desktop")]
    fn images_mut(&mut self) -> impl Iterator<Item = &mut String> {
        [&mut self.reference, &mut self.test, &mut self.diff]
            .into_iter()
            .flatten()
            .chain(self.previews.iter_mut().flat_map(|previews| {
                [
                    &mut previews.reference,
                    &mut previews.test,
                    &mut previews.diff,
                    &mut previews.grid,
                ]
                .into_iter()
                .flatten()
            }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Dimensions {
    pub same: bool,
    pub width: i64,
    pub height: i64,
}

pub(super) fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.chars().any(char::is_control)
        && !value.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*'])
        && !value.ends_with(['.', ' '])
        && !crate::helpers::is_windows_reserved_path_component(value)
}

pub(super) fn valid_suite_id(value: &str) -> bool {
    valid_segment(value)
        && !value.starts_with('.')
        && !crate::test::is_reserved_suite_identifier(value)
}

fn normal_relative(relative: &Path) -> Result<()> {
    ensure!(
        !relative.as_os_str().is_empty()
            && relative.components().all(|part| match part {
                Component::Normal(name) => name.to_str().is_some_and(valid_segment),
                _ => false,
            }),
        "Snapshot artifact path must remain inside its output directory"
    );
    Ok(())
}

pub(super) fn checked_file(
    root: &Path,
    relative: &Path,
    optional: bool,
) -> Result<Option<PathBuf>> {
    normal_relative(relative)?;
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "Snapshot artifacts cannot be symlinks: {}",
                relative.display()
            ),
            Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Snapshot artifact is unavailable: {}", relative.display())
                });
            }
        }
    }
    ensure!(
        path.is_file()
            && path
                .canonicalize()
                .with_context(|| format!(
                    "Failed to resolve snapshot artifact '{}'",
                    path.display()
                ))?
                .starts_with(root),
        "Snapshot artifact must be a regular file inside its output directory: {}",
        path.display()
    );
    Ok(Some(path))
}

pub(super) fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = fs::File::open(path)
        .with_context(|| format!("Failed to open snapshot artifact '{}'", path.display()))?;
    ensure!(
        file.metadata()
            .with_context(|| format!("Failed to inspect snapshot artifact '{}'", path.display()))?
            .len()
            <= limit as u64,
        "Snapshot artifact exceeds its size limit: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("Failed to read snapshot artifact '{}'", path.display()))?;
    ensure!(
        bytes.len() <= limit,
        "Snapshot artifact grew beyond its size limit"
    );
    Ok(bytes)
}

enum RollbackStep {
    File {
        path: PathBuf,
        prior: Option<PathBuf>,
    },
    Directory {
        path: PathBuf,
    },
    Case {
        current: PathBuf,
        original: PathBuf,
    },
}

fn ensure_exact_case(path: &Path, rollback: Option<&mut Vec<RollbackStep>>) -> Result<()> {
    let Some(expected) = path.file_name() else {
        return Ok(());
    };
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve snapshot artifact '{}'", path.display()))?;
    if canonical.file_name() != Some(expected) {
        let parent = path.parent().context("Snapshot path has no parent")?;
        let original_name = canonical.file_name().map(std::ffi::OsStr::to_os_string);
        let temporary = parent.join(format!(".case-{}", uuid::Uuid::new_v4()));
        fs::rename(path, &temporary).with_context(|| {
            format!(
                "Failed to normalize snapshot path case '{}'",
                path.display()
            )
        })?;
        if let Err(error) = fs::rename(&temporary, path) {
            if let Some(original_name) = &original_name {
                let _ = fs::rename(&temporary, parent.join(original_name));
            }
            return Err(error).with_context(|| {
                format!(
                    "Failed to normalize snapshot path case '{}'",
                    path.display()
                )
            });
        }
        if let (Some(rollback), Some(original_name)) = (rollback, original_name) {
            rollback.push(RollbackStep::Case {
                current: path.to_path_buf(),
                original: parent.join(original_name),
            });
        }
    }
    Ok(())
}

fn ensure_output_parent(
    root: &Path,
    relative: &Path,
    mut rollback: Option<&mut Vec<RollbackStep>>,
    mut verified: Option<&mut BTreeSet<PathBuf>>,
) -> Result<()> {
    let mut parent = root.to_path_buf();
    for component in relative
        .parent()
        .context("Snapshot path has no parent")?
        .components()
    {
        parent.push(component);
        if verified
            .as_deref()
            .is_some_and(|checked| checked.contains(&parent))
        {
            continue;
        }
        match fs::symlink_metadata(&parent) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "Snapshot output parent must be a real directory: {}",
                    parent.display()
                );
                ensure_exact_case(&parent, rollback.as_deref_mut())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&parent) {
                    Ok(()) => {
                        if let Some(rollback) = rollback.as_deref_mut() {
                            rollback.push(RollbackStep::Directory {
                                path: parent.clone(),
                            });
                        }
                    }
                    Err(create_error)
                        if create_error.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        let metadata = fs::symlink_metadata(&parent).with_context(|| {
                            format!(
                                "Failed to inspect snapshot output directory '{}'",
                                parent.display()
                            )
                        })?;
                        ensure!(
                            metadata.is_dir() && !metadata.file_type().is_symlink(),
                            "Snapshot output parent must be a real directory: {}",
                            parent.display()
                        );
                        ensure_exact_case(&parent, rollback.as_deref_mut())?;
                    }
                    Err(create_error) => {
                        return Err(create_error).with_context(|| {
                            format!(
                                "Failed to create snapshot output directory '{}'",
                                parent.display()
                            )
                        });
                    }
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Failed to inspect snapshot output directory '{}'",
                        parent.display()
                    )
                });
            }
        }
        if let Some(checked) = verified.as_deref_mut() {
            checked.insert(parent.clone());
        }
    }
    Ok(())
}

fn output_file(root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let relative = Path::new(relative);
    normal_relative(relative)?;
    ensure_output_parent(root, relative, None, None)?;
    if let Some(existing) = checked_file(root, relative, true)?
        && fs::read(&existing).with_context(|| {
            format!(
                "Failed to read existing snapshot artifact '{}'",
                existing.display()
            )
        })? == bytes
    {
        ensure_exact_case(&existing, None)?;
        return Ok(());
    }
    let path = root.join(relative);
    write_file_atomically(&path, bytes)
        .with_context(|| format!("Failed to write snapshot artifact '{}'", path.display()))?;
    ensure_exact_case(&path, None)
}

fn publish_staged_files(staged_root: &Path, root: &Path, dependencies: &[PathBuf]) -> Result<()> {
    let manifest_rel = Path::new("_snapshot/data/manifest.json");
    let status_rel = Path::new(STATUS_PATH);
    let index_rel = Path::new("index.html");
    let gitignore_rel = Path::new(".gitignore");
    let mut ordered: Vec<&Path> = dependencies
        .iter()
        .map(PathBuf::as_path)
        .filter(|path| *path != manifest_rel && *path != status_rel && *path != index_rel)
        .collect();
    for tail in [manifest_rel, status_rel] {
        if dependencies.iter().any(|path| path.as_path() == tail) {
            ordered.push(tail);
        }
    }
    ordered.push(gitignore_rel);
    ordered.push(index_rel);
    let total_files = ordered.len();
    snapshot_phase(&format!(
        "Publishing {total_files} snapshot {} to report output",
        if total_files == 1 { "file" } else { "files" }
    ));

    let mut applied: Vec<RollbackStep> = Vec::new();
    let mut verified_parents: BTreeSet<PathBuf> = BTreeSet::new();
    let mut backup_index = 0usize;
    let mut last_update = std::time::Instant::now();
    let result = (|| -> Result<()> {
        for (index, relative) in ordered.into_iter().enumerate() {
            normal_relative(relative)?;
            let limit = if relative == gitignore_rel || relative == index_rel {
                MAX_REPORT_BYTES
            } else {
                MAX_IMAGE_BYTES
            };
            let bytes = read_bounded(&staged_root.join(relative), limit)?;
            ensure_output_parent(
                root,
                relative,
                Some(&mut applied),
                Some(&mut verified_parents),
            )?;
            let prior = if let Some(existing) = checked_file(root, relative, true)? {
                let existing_bytes = fs::read(&existing).with_context(|| {
                    format!(
                        "Failed to read existing snapshot artifact '{}'",
                        existing.display()
                    )
                })?;
                if existing_bytes == bytes {
                    ensure_exact_case(&existing, Some(&mut applied))?;
                    if index + 1 == total_files
                        || (index + 1).is_multiple_of(50)
                        || last_update.elapsed().as_secs() >= 1
                    {
                        snapshot_phase(&format!(
                            "Published {} of {total_files} snapshot files to report output",
                            index + 1
                        ));
                        last_update = std::time::Instant::now();
                    }
                    continue;
                }
                let backup = staged_root.join(format!(".prior-{backup_index}"));
                backup_index += 1;
                fs::write(&backup, &existing_bytes).with_context(|| {
                    format!(
                        "Failed to stage rollback backup for snapshot artifact '{}'",
                        existing.display()
                    )
                })?;
                Some(backup)
            } else {
                None
            };
            let path = root.join(relative);
            write_file_atomically(&path, &bytes).with_context(|| {
                format!("Failed to write snapshot artifact '{}'", path.display())
            })?;
            applied.push(RollbackStep::File {
                path: path.clone(),
                prior,
            });
            ensure_exact_case(&path, Some(&mut applied))?;
            if index + 1 == total_files
                || (index + 1).is_multiple_of(50)
                || last_update.elapsed().as_secs() >= 1
            {
                snapshot_phase(&format!(
                    "Published {} of {total_files} snapshot files to report output",
                    index + 1
                ));
                last_update = std::time::Instant::now();
            }
        }
        Ok(())
    })();
    if result.is_err() {
        for step in applied.into_iter().rev() {
            match step {
                RollbackStep::File { path, prior } => match prior {
                    Some(backup) => {
                        if let Ok(bytes) = fs::read(&backup) {
                            let _ = write_file_atomically(&path, &bytes);
                        }
                    }
                    None => {
                        let _ = fs::remove_file(&path);
                    }
                },
                RollbackStep::Directory { path } => {
                    let _ = fs::remove_dir(&path);
                }
                RollbackStep::Case { current, original } => {
                    if let Some(parent) = current.parent() {
                        let temporary = parent.join(format!(".case-{}", uuid::Uuid::new_v4()));
                        if fs::rename(&current, &temporary).is_ok() {
                            let _ = fs::rename(&temporary, &original);
                        }
                    }
                }
            }
        }
    }
    result
}

fn encoded_path(relative: &str) -> Result<String> {
    ensure!(
        relative.split('/').all(valid_segment),
        "Invalid snapshot URL path"
    );
    const URL_SEGMENT: &percent_encoding::AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    Ok(relative
        .split('/')
        .map(|part| utf8_percent_encode(part, URL_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/"))
}

pub(super) fn store_asset(root: &Path, relative: &str, bytes: &[u8]) -> Result<String> {
    let kind = if relative.starts_with("_snapshot/data/") {
        "data"
    } else {
        "images"
    };
    let url = format!(
        "{}?v={}",
        encoded_path(relative)?,
        get_asset_version(bytes, 20)
    );
    versioned_path(&url, kind)?;
    output_file(root, relative, bytes)?;
    Ok(url)
}

pub(super) fn store_json(root: &Path, relative: &str, value: &impl Serialize) -> Result<String> {
    store_asset(root, relative, &serde_json::to_vec(value)?)
}

pub(crate) fn escape_html(title: &str) -> String {
    title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(super) fn snapshot_phase(name: &str) {
    use crate::diagnostics::{record::*, runtime};
    runtime::event(
        runtime::current_stage().unwrap_or(Stage::SnapshotStaging),
        DiagnosticEvent::Decision,
        Severity::Info,
        [(
            Field::Artifact,
            FieldValue::Identity(SafeIdentity::source(name)),
        )],
    );
    crate::progress::emit(crate::progress::Event::Phase { name: name.into() });
}

fn publish_index(root: &Path, manifest: &Manifest) -> Result<()> {
    let manifest_url = store_json(root, "_snapshot/data/manifest.json", manifest)?;
    output_file(
        root,
        STATUS_PATH,
        &serde_json::to_vec(&SiteStatus::from_manifest(manifest)?)?,
    )?;
    let index = FRONTEND_HTML
        .replace("__SNAPSHOT_MANIFEST__", &manifest_url)
        .replace("__SNAPSHOT_TITLE__", &escape_html(&manifest.title));
    // Publish the entry point only after all of its versioned dependencies are complete.
    output_file(root, "index.html", index.as_bytes())
}

pub(crate) fn generate_site(
    root: &Path,
    suites: &[String],
    args: &SnapshotArgs,
    workspace: &WorkspaceConfig,
) -> Result<()> {
    let root = root.canonicalize().with_context(|| {
        format!(
            "Failed to resolve snapshot output directory '{}'",
            root.display()
        )
    })?;
    let staging = crate::run_history::staging::StagingDirectory::create(
        &root,
        crate::run_history::staging::StagingKind::Snapshot,
    )
    .with_context(|| {
        format!(
            "Failed to create snapshot staging directory in '{}'",
            root.display()
        )
    })?;
    let staged_root = staging.path().canonicalize().with_context(|| {
        format!(
            "Failed to resolve snapshot staging directory '{}'",
            staging.path().display()
        )
    })?;
    let mut summaries = Vec::new();
    let mut images = BTreeMap::new();
    let mut logs = super::logs::LogCache::new();
    if !suites.is_empty() {
        snapshot_phase(&format!(
            "Preparing report snapshot for {} {}",
            suites.len(),
            if suites.len() == 1 { "suite" } else { "suites" }
        ));
    }
    for (index, id) in suites.iter().enumerate() {
        snapshot_phase(&format!(
            "Loading snapshot suite {}/{}: {id}",
            index + 1,
            suites.len()
        ));
        if let Some(report) =
            super::capture_report::load_suite(&root, &staged_root, id, &mut images, &mut logs)
                .with_context(|| format!("Failed to process snapshot suite '{id}'"))?
        {
            let passed = report
                .tests
                .iter()
                .filter(|capture| capture.status == "pass")
                .count();
            summaries.push(SuiteSummary {
                id: id.clone(),
                display_name: report.display_name.clone(),
                total: report.tests.len(),
                passed,
                failed: report.tests.len() - passed,
                errors: report
                    .tests
                    .iter()
                    .filter(|capture| capture.error.is_some())
                    .count(),
                report: store_json(
                    &staged_root,
                    &format!("_snapshot/data/suites/{id}.json"),
                    &report,
                )?,
            });
        }
    }
    summaries.sort_by(|first, second| first.id.cmp(&second.id));
    let manifest = Manifest {
        version: 2,
        title: args
            .title
            .as_deref()
            .or(workspace.snapshot_title.as_deref())
            .unwrap_or("Visual Tests Summary | Regressify")
            .into(),
        heading: args
            .heading
            .as_deref()
            .or(workspace.snapshot_heading.as_deref())
            .unwrap_or("Visual Tests Summary")
            .into(),
        subheading: args
            .subheading
            .as_deref()
            .or(workspace.snapshot_subheading.as_deref())
            .unwrap_or("Snapshot regression test results")
            .into(),
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        regressify_version: Some(
            concat!(
                env!("CARGO_PKG_VERSION"),
                " (",
                env!("BINARY_GIT_HASH"),
                ")"
            )
            .into(),
        ),
        suites: summaries,
    };
    for (name, bytes) in FRONTEND_FILES {
        output_file(
            &staged_root,
            if *name == "gitignore" {
                ".gitignore"
            } else {
                name
            },
            bytes,
        )?;
    }
    publish_index(&staged_root, &manifest)?;
    let selected = manifest
        .suites
        .iter()
        .map(|suite| suite.id.clone())
        .collect::<Vec<_>>();
    snapshot_phase(&format!(
        "Validating generated snapshot manifest and assets for {} {}",
        manifest.suites.len(),
        if manifest.suites.len() == 1 {
            "suite"
        } else {
            "suites"
        }
    ));
    let dependencies = artifact_paths(&staged_root, &selected)?;
    // Invalid report input must never overwrite a stable path in the published site.
    publish_staged_files(&staged_root, &root, &dependencies)?;
    prune_generated_files(&root, &dependencies)?;
    artifact_paths(&root, &selected)?;
    for suite in &manifest.suites {
        remove_legacy(&root, &format!("{}/html_report/index.html", suite.id))?;
        for name in [
            "assets",
            "diff.js",
            "diverged.js",
            "divergedWorker.js",
            "index_bundle.js",
            "index_bundle.js.LICENSE.txt",
        ] {
            remove_legacy(&root, &format!("{}/html_report/{name}", suite.id))?;
        }
    }
    for name in [
        "styles.css",
        "script.js",
        "index_bundle.js",
        "diff.js",
        "diverged.js",
        "divergedWorker.js",
        "Lato-Bold.ttf",
        "Lato-Regular.ttf",
        "lato-bold-webfont.woff",
        "lato-bold-webfont.woff2",
        "lato-regular-webfont.woff",
        "lato-regular-webfont.woff2",
        "favicon.ico",
        "favicon.svg",
        "favicon-16x16.png",
        "favicon-32x32.png",
        "apple-touch-icon.png",
    ] {
        remove_legacy(&root, name)?;
    }
    {
        use crate::diagnostics::{record::*, runtime};
        runtime::event(
            Stage::SnapshotStaging,
            DiagnosticEvent::StageFinished,
            Severity::Info,
            [
                (Field::Command, FieldValue::Kind(SafeValue::Snapshot)),
                (
                    Field::SelectedSuites,
                    FieldValue::Number(manifest.suites.len() as u64),
                ),
                (
                    Field::FileCount,
                    FieldValue::Number(dependencies.len() as u64),
                ),
            ],
        );
    }
    eprintln!(
        "Snapshot generated at: {}",
        root.join("index.html").display()
    );
    Ok(())
}

fn remove_legacy(root: &Path, relative: &str) -> Result<()> {
    normal_relative(Path::new(relative))?;
    let path = root.join(relative);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(
                !metadata.file_type().is_symlink(),
                "Legacy snapshot artifacts cannot be symlinks"
            );
            if metadata.is_dir() {
                fs::remove_dir_all(&path).with_context(|| {
                    format!(
                        "Failed to remove legacy snapshot artifact '{}'",
                        path.display()
                    )
                })?;
            } else {
                fs::remove_file(&path).with_context(|| {
                    format!(
                        "Failed to remove legacy snapshot artifact '{}'",
                        path.display()
                    )
                })?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "Failed to inspect legacy snapshot artifact '{}'",
                path.display()
            )
        }),
    }
}

fn prune_generated_files(root: &Path, active: &[PathBuf]) -> Result<()> {
    let active: BTreeSet<_> = active.iter().collect();
    let generated = root.join("_snapshot");
    let entries = walkdir::WalkDir::new(&generated)
        .follow_links(false)
        .contents_first(true)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| {
            format!(
                "Failed to list generated snapshot files in '{}'",
                generated.display()
            )
        })?;
    ensure!(
        entries.iter().all(|entry| !entry.file_type().is_symlink()),
        "Generated snapshot files cannot be symlinks"
    );
    for entry in entries {
        if entry.file_type().is_file()
            && !active.contains(&entry.path().strip_prefix(root)?.to_path_buf())
        {
            fs::remove_file(entry.path()).with_context(|| {
                format!(
                    "Failed to remove stale snapshot artifact '{}'",
                    entry.path().display()
                )
            })?;
        } else if entry.file_type().is_dir() {
            let empty = fs::read_dir(entry.path())
                .and_then(|mut entries| entries.next().transpose())
                .with_context(|| {
                    format!(
                        "Failed to inspect generated snapshot directory '{}'",
                        entry.path().display()
                    )
                })?
                .is_none();
            // Empty directories carry no report data; sync clients can block their deletion.
            if empty
                && let Err(error) = fs::remove_dir(entry.path())
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!(
                    "Warning: Could not remove empty snapshot directory '{}'; leaving it in place: {error}",
                    entry.path().display()
                );
                use crate::diagnostics::{record::*, runtime};
                if let Some(context) = runtime::current() {
                    let mut fields = vec![
                        (Field::CleanupFailed, FieldValue::Boolean(true)),
                        (
                            Field::Artifact,
                            FieldValue::Identity(SafeIdentity::fingerprint(
                                entry.path().as_os_str().as_encoded_bytes(),
                            )),
                        ),
                    ];
                    if let Some(code) = error
                        .raw_os_error()
                        .and_then(|code| u64::try_from(code).ok())
                    {
                        fields.push((Field::OsErrorCode, FieldValue::Number(code)));
                    }
                    context.record(
                        Stage::CompletionCleanup,
                        DiagnosticEvent::StageFinished,
                        Severity::Warn,
                        fields,
                        runtime::Completion {
                            outcome: Some(Outcome::Failure),
                            error: Some(SanitizedError::capture(&error.into())),
                            duration_ms: None,
                        },
                    );
                }
            }
        }
    }
    Ok(())
}

pub(super) fn versioned_path(path: &str, kind: &str) -> Result<PathBuf> {
    let extension = if kind == "data" { ".json" } else { ".png" };
    let (relative, version) = path
        .split_once("?v=")
        .context("Snapshot URL requires a cache version")?;
    ensure!(
        relative.starts_with(&format!("_snapshot/{kind}/"))
            && relative.to_ascii_lowercase().ends_with(extension)
            && version.len() == 20
            && version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "Invalid versioned snapshot URL"
    );
    let decoded = relative
        .split('/')
        .map(|part| {
            let decoded = percent_decode_str(part)
                .decode_utf8()
                .context("Snapshot path is not UTF-8")?;
            ensure!(
                valid_segment(&decoded),
                "Snapshot URL escapes its artifact directory"
            );
            Ok(decoded.into_owned())
        })
        .collect::<Result<Vec<_>>>()?
        .join("/");
    ensure!(
        encoded_path(&decoded)? == relative,
        "Snapshot URL is not canonically encoded"
    );
    let path = PathBuf::from(decoded);
    normal_relative(&path)?;
    Ok(path)
}

fn read_versioned(root: &Path, path: &str, kind: &str) -> Result<Vec<u8>> {
    let relative = versioned_path(path, kind)?;
    let source = checked_file(root, &relative, false)?.context("Snapshot artifact missing")?;
    let bytes = read_bounded(
        &source,
        if kind == "data" {
            MAX_REPORT_BYTES
        } else {
            MAX_IMAGE_BYTES
        },
    )?;
    ensure!(
        path.split_once("?v=").map(|(_, version)| version)
            == Some(get_asset_version(&bytes, 20).as_str()),
        "Snapshot content does not match its cache version; regenerate the snapshot"
    );
    Ok(bytes)
}

fn parse_manifest(root: &Path, html: &str) -> Result<(String, Manifest)> {
    let marker = "name=\"regressify-manifest\" content=\"";
    ensure!(
        html.matches(marker).count() == 1,
        "Snapshot entry page has no unique manifest; regenerate the snapshot"
    );
    let path = html
        .split_once(marker)
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(path, _)| path)
        .context("Snapshot manifest URL is missing")?;
    ensure!(
        versioned_path(path, "data")? == Path::new("_snapshot/data/manifest.json"),
        "Snapshot manifest URL is invalid; regenerate the snapshot"
    );
    let manifest: Manifest = serde_json::from_slice(&read_versioned(root, path, "data")?)?;
    ensure!(
        manifest.version == 2,
        "Unsupported snapshot manifest version"
    );
    Ok((path.to_owned(), manifest))
}

fn read_manifest(root: &Path) -> Result<(String, Manifest)> {
    let source = checked_file(root, Path::new("index.html"), false)?
        .context("Run snapshot before deploying reports")?;
    let html = String::from_utf8(read_bounded(&source, MAX_REPORT_BYTES)?)?;
    parse_manifest(root, &html)
}

/// Inventory only the active manifest, never stale assets, captures, or workspace state.
#[cfg_attr(not(feature = "desktop"), allow(dead_code))]
pub(crate) fn artifact_paths(output: &Path, selected: &[String]) -> Result<Vec<PathBuf>> {
    let root = output.canonicalize()?;
    let (manifest_path, manifest) = read_manifest(&root)?;
    let mut expected = BTreeSet::new();
    for id in selected {
        ensure!(
            valid_suite_id(id) && expected.insert(id.as_str()),
            "Invalid or duplicate snapshot suite selection"
        );
    }
    ensure!(
        manifest.suites.len() == expected.len()
            && manifest
                .suites
                .iter()
                .all(|suite| expected.contains(suite.id.as_str())),
        "Regenerate the snapshot for the selected suites before deploying"
    );
    let mut seen = BTreeSet::new();
    let mut paths = BTreeSet::from([
        PathBuf::from("index.html"),
        versioned_path(&manifest_path, "data")?,
    ]);
    if let Some(path) = checked_file(&root, Path::new(STATUS_PATH), true)? {
        let status: SiteStatus = serde_json::from_slice(&read_bounded(&path, 64 * 1024)?)?;
        ensure!(
            status == SiteStatus::from_manifest(&manifest)?,
            "Snapshot status does not match the published report; regenerate the snapshot"
        );
        paths.insert(PathBuf::from(STATUS_PATH));
    }
    let mut image_versions = BTreeMap::<PathBuf, String>::new();
    let mut validated_logs = BTreeSet::new();
    for (name, _) in FRONTEND_FILES {
        if *name != "gitignore" {
            let relative = PathBuf::from(name);
            checked_file(&root, &relative, false)?;
            paths.insert(relative);
        }
    }
    for summary in manifest.suites {
        ensure!(
            valid_suite_id(&summary.id)
                && seen.insert(summary.id.clone())
                && summary.passed.checked_add(summary.failed) == Some(summary.total)
                && summary.errors <= summary.failed,
            "Invalid snapshot suite summary"
        );
        let report_path = versioned_path(&summary.report, "data")?;
        ensure!(
            report_path == Path::new(&format!("_snapshot/data/suites/{}.json", summary.id)),
            "Snapshot suite report path does not match its summary"
        );
        let report: SuiteReport =
            serde_json::from_slice(&read_versioned(&root, &summary.report, "data")?)?;
        let passed = report
            .tests
            .iter()
            .filter(|capture| capture.status == "pass")
            .count();
        let errors = report
            .tests
            .iter()
            .filter(|capture| capture.error.is_some())
            .count();
        ensure!(
            report.id == summary.id
                && report.tests.len() == summary.total
                && passed == summary.passed
                && report.tests.len() - passed == summary.failed
                && errors == summary.errors,
            "Snapshot suite data does not match its summary"
        );
        paths.insert(report_path);
        let mut captures = BTreeSet::new();
        for capture in report.tests {
            ensure!(
                valid_segment(&capture.id)
                    && captures.insert(capture.id.clone())
                    && matches!(capture.status.as_str(), "pass" | "fail")
                    && (capture.error.is_none() || capture.status == "fail"),
                "Invalid snapshot capture"
            );
            for image in capture.images() {
                let relative = versioned_path(image, "images")?;
                if let Some(previous) = image_versions.insert(relative.clone(), image.clone()) {
                    ensure!(
                        previous == *image,
                        "Conflicting cache versions for a snapshot image"
                    );
                }
                if paths.insert(relative) {
                    read_versioned(&root, image, "images")?;
                }
            }
            if let Some(logs) = &capture.logs {
                for log in [&logs.reference, &logs.test].into_iter().flatten() {
                    ensure!(
                        log.chunks.len() <= 16_384,
                        "Snapshot log has too many chunks"
                    );
                    ensure!(
                        matches!(
                            log.state,
                            crate::diagnostics::journal::ArchiveState::Complete
                                | crate::diagnostics::journal::ArchiveState::Partial
                        ) || (log.chunks.is_empty() && log.records == 0),
                        "Unavailable snapshot log cannot include chunks"
                    );
                    ensure!(
                        log.state != crate::diagnostics::journal::ArchiveState::Complete
                            || (log.missing_records == Some(0) && !log.chunks.is_empty()),
                        "Complete snapshot log has missing records"
                    );
                    if log.chunks.is_empty() {
                        ensure!(log.records == 0, "Snapshot log record count mismatch");
                    } else if validated_logs.insert((
                        log.operation_id.clone(),
                        log.execution_id.clone(),
                        log.chunks.clone(),
                        log.records,
                    )) {
                        let mut records = 0;
                        let mut chunks = BTreeSet::new();
                        for chunk in &log.chunks {
                            let relative = versioned_path(chunk, "data")?;
                            let operation = log
                                .operation_id
                                .as_ref()
                                .context("Log chunk has no operation identity")?;
                            ensure!(
                                relative.starts_with(
                                    Path::new("_snapshot/data/logs").join(operation.as_str())
                                ) && chunks.insert(relative.clone()),
                                "Snapshot log path does not belong to its operation"
                            );
                            if let Some(previous) =
                                image_versions.insert(relative.clone(), chunk.clone())
                            {
                                ensure!(previous == *chunk, "Conflicting snapshot log versions");
                            }
                            let bytes = read_versioned(&root, chunk, "data")?;
                            super::logs::validate_chunk(&bytes, log, &mut records)?;
                            paths.insert(relative);
                        }
                        ensure!(records == log.records, "Snapshot log record count mismatch");
                    }
                }
            }
        }
    }
    Ok(paths.into_iter().collect())
}

#[cfg(feature = "desktop")]
pub(crate) fn should_update_suite(
    output: &Path,
    old_id: &str,
    new_id: Option<&str>,
) -> Result<bool> {
    ensure!(
        valid_suite_id(old_id) && new_id.is_none_or(valid_suite_id),
        "Invalid snapshot suite identity"
    );
    let root = output.canonicalize()?;
    let Some(source) = checked_file(&root, Path::new("index.html"), true)? else {
        return Ok(false);
    };
    let html = String::from_utf8(read_bounded(&source, MAX_REPORT_BYTES)?)?;
    if !html.contains("name=\"regressify-manifest\"") {
        return Ok(false);
    }
    let (_, manifest) = parse_manifest(&root, &html)?;
    ensure!(
        new_id.is_none_or(|id| {
            id.eq_ignore_ascii_case(old_id)
                || !manifest
                    .suites
                    .iter()
                    .any(|existing| existing.id.eq_ignore_ascii_case(id))
        }),
        "A published report already uses the destination suite ID"
    );
    Ok(manifest.suites.iter().any(|suite| suite.id == old_id))
}

#[cfg(feature = "desktop")]
pub(crate) fn update_suite(
    output: &Path,
    old_id: &str,
    new_id: Option<&str>,
    display_name: Option<&str>,
) -> Result<()> {
    if !should_update_suite(output, old_id, new_id)? {
        return Ok(());
    }
    let root = output.canonicalize()?;
    let (_, mut manifest) = read_manifest(&root)?;
    let selected: Vec<_> = manifest
        .suites
        .iter()
        .map(|suite| suite.id.clone())
        .collect();
    artifact_paths(&root, &selected)?;
    let mut reports = Vec::new();
    for summary in &manifest.suites {
        reports.push(serde_json::from_slice::<SuiteReport>(&read_versioned(
            &root,
            &summary.report,
            "data",
        )?)?);
    }
    let mut summaries = Vec::new();
    for (mut summary, mut report) in manifest.suites.into_iter().zip(reports) {
        let mut modified = false;
        if summary.id == old_id {
            let Some(new_id) = new_id else { continue };
            summary.id = new_id.into();
            summary.display_name = display_name
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned);
            report.id = new_id.into();
            report.display_name = summary.display_name.clone();
            modified = true;
        }
        if let Some(new_id) = new_id.filter(|id| *id != old_id) {
            let prefix = Path::new("_snapshot/images").join(old_id);
            let mut staged_images = Vec::new();
            for capture in &mut report.tests {
                for image in capture.images_mut() {
                    let relative = versioned_path(image, "images")?;
                    if let Ok(suffix) = relative.strip_prefix(&prefix) {
                        let bytes = read_versioned(&root, image, "images")?;
                        let destination = Path::new("_snapshot/images").join(new_id).join(suffix);
                        staged_images.push((
                            image,
                            destination.to_string_lossy().replace('\\', "/"),
                            bytes,
                        ));
                    }
                }
            }
            if !staged_images.is_empty() {
                modified = true;
            }
            for (image, destination, bytes) in staged_images {
                *image = store_asset(&root, &destination, &bytes)?;
            }
        }
        if modified {
            summary.report = store_json(
                &root,
                &format!("_snapshot/data/suites/{}.json", summary.id),
                &report,
            )?;
        }
        summaries.push(summary);
    }
    manifest.suites = summaries;
    manifest
        .suites
        .sort_by(|first, second| first.id.cmp(&second.id));
    publish_index(&root, &manifest)?;
    let selected = manifest
        .suites
        .iter()
        .map(|suite| suite.id.clone())
        .collect::<Vec<_>>();
    let active = artifact_paths(&root, &selected)?;
    prune_generated_files(&root, &active)?;
    artifact_paths(&root, &selected)?;
    Ok(())
}

#[cfg(feature = "desktop")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ImageRole {
    Passed,
    Comparison,
    Protected,
}

#[cfg(feature = "desktop")]
pub(crate) fn image_roles(output: &Path) -> Result<Option<BTreeMap<PathBuf, ImageRole>>> {
    let root = output.canonicalize()?;
    let source = checked_file(&root, Path::new("index.html"), false)?
        .context("Snapshot entry page missing")?;
    let html = String::from_utf8(read_bounded(&source, MAX_REPORT_BYTES)?)?;
    if !html.contains("name=\"regressify-manifest\"") {
        return Ok(None);
    }
    let (_, manifest) = parse_manifest(&root, &html)?;
    let mut roles = BTreeMap::<PathBuf, ImageRole>::new();
    for summary in manifest.suites {
        let report: SuiteReport =
            serde_json::from_slice(&read_versioned(&root, &summary.report, "data")?)?;
        for capture in report.tests {
            for (image, diff) in [
                (&capture.reference, false),
                (&capture.test, false),
                (&capture.diff, true),
            ] {
                if let Some(image) = image {
                    let role = if diff || capture.error.is_some() {
                        ImageRole::Protected
                    } else if capture.status == "pass" {
                        ImageRole::Passed
                    } else {
                        ImageRole::Comparison
                    };
                    roles
                        .entry(versioned_path(image, "images")?)
                        .and_modify(|previous| *previous = (*previous).max(role))
                        .or_insert(role);
                }
            }
            for image in capture.previews.iter().flat_map(CapturePreviews::images) {
                roles.insert(versioned_path(image, "images")?, ImageRole::Protected);
            }
        }
    }
    Ok(Some(roles))
}

/// Deployment compression changes bytes, so refresh every affected URL's cache version.
#[cfg(feature = "desktop")]
pub(crate) fn refresh_site_versions(output: &Path) -> Result<()> {
    let root = output.canonicalize()?;
    let (_, mut manifest) = read_manifest(&root)?;
    let mut replacements = BTreeMap::<PathBuf, String>::new();
    for summary in &mut manifest.suites {
        let mut report: SuiteReport =
            serde_json::from_slice(&read_versioned(&root, &summary.report, "data")?)?;
        for capture in &mut report.tests {
            for image in capture.images_mut() {
                let relative = versioned_path(image, "images")?;
                let replacement = if let Some(replacement) = replacements.get(&relative) {
                    replacement.clone()
                } else {
                    let source = checked_file(&root, &relative, false)?
                        .context("Compressed snapshot image is missing")?;
                    let path = image
                        .split_once("?v=")
                        .context("Snapshot image version missing")?
                        .0;
                    let replacement = format!(
                        "{path}?v={}",
                        get_asset_version(&read_bounded(&source, MAX_IMAGE_BYTES)?, 20)
                    );
                    replacements.insert(relative, replacement.clone());
                    replacement
                };
                *image = replacement;
            }
        }
        summary.report = store_json(
            &root,
            &format!("_snapshot/data/suites/{}.json", summary.id),
            &report,
        )?;
    }
    publish_index(&root, &manifest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn manifest_round_trips_optional_execution_log_fields() {
        use crate::diagnostics::journal::ArchiveState;
        let legacy = serde_json::json!({"id":"page.png", "label":"Page", "scenario":"Page", "selector":"document", "status":"pass",
            "threshold":0.1,"mismatch":0.0,"error":null,"reference":null,"test":null,"diff":null,
            "referenceUrl":null,"testUrl":null,"dimensions":null});
        let capture: super::Capture = serde_json::from_value(legacy.clone()).unwrap();
        assert!(capture.logs.is_none());
        let manifest = serde_json::json!({"version":2,"title":"Report","heading":"Report","subheading":"","generatedAt":"2026-10-01T00:00:00Z","suites":[]});
        assert_eq!(
            serde_json::from_value::<super::Manifest>(manifest)
                .unwrap()
                .version,
            2
        );
        let mut states = std::collections::BTreeSet::new();
        for state in [
            ArchiveState::Complete,
            ArchiveState::Partial,
            ArchiveState::Missing,
            ArchiveState::LegacyUnavailable,
            ArchiveState::LoadingError,
        ] {
            let log = super::ExecutionLog {
                state,
                operation_id: None,
                execution_id: None,
                chunks: Vec::new(),
                records: 0,
                missing_records: None,
                build: None,
            };
            let logs = super::CaptureLogs {
                reference: None,
                test: Some(log),
            };
            let mut value = legacy.clone();
            value["logs"] = serde_json::to_value(&logs).unwrap();
            let decoded: super::Capture = serde_json::from_value(value).unwrap();
            assert_eq!(decoded.logs.unwrap(), logs);
            states.insert(serde_json::to_string(&state).unwrap());
        }
        assert_eq!(states.len(), 5);
    }

    use super::*;

    fn args() -> SnapshotArgs {
        SnapshotArgs {
            test_suite: Vec::new(),
            title: None,
            heading: None,
            subheading: None,
        }
    }

    #[test]
    fn site_status_counts_validate_and_never_include_report_details() {
        let mut manifest = Manifest {
            version: 2,
            title: "Private title".into(),
            heading: "Private heading".into(),
            subheading: "Private subheading".into(),
            generated_at: "2026-09-24T00:00:00Z".into(),
            regressify_version: None,
            suites: vec![SuiteSummary {
                id: "private-suite".into(),
                display_name: None,
                total: 3,
                passed: 1,
                failed: 2,
                errors: 1,
                report: "private-report".into(),
            }],
        };
        let status = SiteStatus::from_manifest(&manifest).unwrap();
        assert_eq!(
            (
                status.suites,
                status.total,
                status.passed,
                status.failed,
                status.errors
            ),
            (1, 3, 1, 2, 1)
        );
        assert!(
            !serde_json::to_string(&status)
                .unwrap()
                .to_lowercase()
                .contains("private")
        );
        manifest.suites[0].errors = 3;
        assert!(SiteStatus::from_manifest(&manifest).is_err());
        manifest.suites[0].errors = 0;
        manifest.suites[0].passed = usize::MAX;
        assert!(SiteStatus::from_manifest(&manifest).is_err());
        manifest.suites.clear();
        assert_eq!(SiteStatus::from_manifest(&manifest).unwrap().total, 0);
    }

    #[test]
    fn site_status_is_published_staged_and_checked_against_the_manifest() {
        let directory = tempfile::tempdir().unwrap();
        generate_site(directory.path(), &[], &args(), &WorkspaceConfig::default()).unwrap();
        let path = directory.path().join(STATUS_PATH);
        let status: SiteStatus = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(status.suites, 0);
        assert!(
            artifact_paths(directory.path(), &[])
                .unwrap()
                .contains(&PathBuf::from(STATUS_PATH))
        );
        let mut tampered = status;
        tampered.total = 1;
        fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert!(artifact_paths(directory.path(), &[]).is_err());
    }

    #[test]
    fn frontend_resource_versions_match_the_rust_asset_helper() {
        let html = include_str!("data/index.html");
        let css = FRONTEND_FILES
            .iter()
            .filter(|(name, _)| name.ends_with(".css"))
            .map(|(_, bytes)| std::str::from_utf8(bytes).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        for (name, bytes) in FRONTEND_FILES {
            if name.ends_with(".js") || name.ends_with(".css") {
                assert!(
                    html.contains(&format!("/{name}?v={}", get_asset_version(bytes, 20))),
                    "{name}"
                );
            } else if name.ends_with(".woff2") {
                assert!(
                    css.contains(&format!("/{name}?v={}", get_asset_version(bytes, 20))),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn versioned_urls_encode_unicode_and_reserved_characters_and_reject_escapes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let path = "_snapshot/images/caf\u{e9} & shop/reference/home #1 %2F.png";
        let url = store_asset(&root, path, b"image").unwrap();
        assert!(url.contains("caf%C3%A9%20%26%20shop/reference/home%20%231%20%252F.png?v="));
        assert_eq!(versioned_path(&url, "images").unwrap(), PathBuf::from(path));
        assert_eq!(read_versioned(&root, &url, "images").unwrap(), b"image");
        for unsafe_path in [
            "_snapshot/images/../private.png",
            "_snapshot/images/%2E%2E/private.png",
            "_snapshot/images/suite/%2Fprivate.png",
            "_snapshot/images/suite/%5Cprivate.png",
            "_snapshot/images/suite/%00.png",
            "_snapshot/images/suite/CON.png",
            "_snapshot/images/suite/file.png/",
            "https://example.com/_snapshot/images/home.png",
        ] {
            assert!(
                versioned_path(&format!("{unsafe_path}?v={}", "A".repeat(20)), "images").is_err(),
                "{unsafe_path}"
            );
        }
        for invalid in [
            path.to_owned(),
            format!("{url}&extra=1"),
            format!("{url}#fragment"),
            format!("{url}?v=again"),
        ] {
            assert!(versioned_path(&invalid, "images").is_err());
        }
    }

    #[test]
    fn snapshot_text_precedence_empty_values_and_hostile_text_are_preserved_safely() {
        let temp = tempfile::tempdir().unwrap();
        let legacy: Manifest = serde_json::from_value(serde_json::json!({
            "version": 2, "title": "", "heading": "", "subheading": "",
            "generatedAt": "2026-09-24T00:00:00Z", "suites": []
        }))
        .unwrap();
        assert!(legacy.regressify_version.is_none());
        let workspace = WorkspaceConfig {
            snapshot_title: Some("Shop <report> & \"results\"".into()),
            snapshot_heading: Some("Shop & results".into()),
            snapshot_subheading: Some("**Staging** <script>alert(1)</script>".into()),
            ..Default::default()
        };
        generate_site(temp.path(), &[], &args(), &workspace).unwrap();
        let (_, manifest) = read_manifest(&temp.path().canonicalize().unwrap()).unwrap();
        assert_eq!(
            manifest.regressify_version.as_deref(),
            Some(concat!(
                env!("CARGO_PKG_VERSION"),
                " (",
                env!("BINARY_GIT_HASH"),
                ")"
            ))
        );
        assert_eq!(manifest.heading, "Shop & results");
        assert_eq!(manifest.subheading, "**Staging** <script>alert(1)</script>");
        let html = fs::read_to_string(temp.path().join("index.html")).unwrap();
        assert!(html.contains("Shop &lt;report&gt; &amp; &quot;results&quot;"));
        assert!(!html.contains("<script>alert(1)"));
        let overrides = SnapshotArgs {
            title: Some(String::new()),
            heading: Some("Visual Tests Summary".into()),
            subheading: Some(String::new()),
            ..args()
        };
        generate_site(temp.path(), &[], &overrides, &workspace).unwrap();
        let (_, manifest) = read_manifest(&temp.path().canonicalize().unwrap()).unwrap();
        assert_eq!(manifest.title, "");
        assert_eq!(manifest.heading, "Visual Tests Summary");
        assert_eq!(manifest.subheading, "");
        generate_site(temp.path(), &[], &args(), &WorkspaceConfig::default()).unwrap();
        let (_, manifest) = read_manifest(&temp.path().canonicalize().unwrap()).unwrap();
        assert_eq!(manifest.title, "Visual Tests Summary | Regressify");
        assert_eq!(manifest.heading, "Visual Tests Summary");
    }

    #[test]
    fn snapshots_have_only_versioned_dependencies_and_matching_host_policies() {
        let temp = tempfile::tempdir().unwrap();
        generate_site(temp.path(), &[], &args(), &WorkspaceConfig::default()).unwrap();
        let paths = artifact_paths(temp.path(), &[]).unwrap();
        assert!(paths.iter().any(|path| {
            path.extension()
                .is_some_and(|extension| extension == "woff2")
        }));
        assert!(
            paths
                .iter()
                .all(|path| path.file_name().is_none_or(|name| name != "config.js"))
        );
        let config: serde_json::Value = serde_json::from_slice(
            &fs::read(temp.path().join("staticwebapp.config.json")).unwrap(),
        )
        .unwrap();
        let headers = fs::read_to_string(temp.path().join("_headers")).unwrap();
        for (name, value) in config["globalHeaders"].as_object().unwrap() {
            let expected = format!("{name}: {}", value.as_str().unwrap());
            assert!(
                headers.lines().any(|line| line.trim() == expected),
                "Netlify is missing Azure's global header: {expected}"
            );
        }
        assert_eq!(config["navigationFallback"]["rewrite"], "/index.html");
        let cache_policies = headers
            .lines()
            .filter_map(|line| line.trim().strip_prefix("Cache-Control: "))
            .filter(|policy| *policy != "no-cache")
            .collect::<Vec<_>>();
        assert!(!cache_policies.is_empty());
        for policy in cache_policies {
            let directives = policy.split(',').map(str::trim).collect::<BTreeSet<_>>();
            for required in ["public", "max-age=31536000", "immutable"] {
                assert!(
                    directives.contains(required),
                    "Asset cache policy is missing {required}: {policy}"
                );
            }
        }
        let status_headers = headers
            .split("/_snapshot/status.json")
            .nth(1)
            .unwrap()
            .trim_start();
        assert!(status_headers.starts_with("Access-Control-Allow-Origin: *"));
        assert!(
            status_headers
                .lines()
                .take_while(|line| !line.is_empty())
                .any(|line| line.trim() == "Cache-Control: no-cache")
        );
        assert_eq!(config["routes"][0]["route"], "/_snapshot/status.json");
        assert_eq!(
            config["routes"][0]["headers"]["Access-Control-Allow-Origin"],
            "*"
        );
        assert_eq!(config["routes"][0]["headers"]["Cache-Control"], "no-cache");
        assert!(
            fs::read_to_string(temp.path().join("_redirects"))
                .unwrap()
                .contains("/suites/* /index.html 200")
        );
        let gitignore = fs::read_to_string(temp.path().join(".gitignore")).unwrap();
        for required in [
            "/_diagnostics/",
            "/_snapshot/",
            "/states/",
            "!bitmaps_reference/*.png",
        ] {
            assert!(
                gitignore.lines().any(|line| line.trim() == required),
                "Missing {required} in generated .gitignore"
            );
        }
    }

    #[test]
    fn asset_paths_stay_stable_while_versions_follow_bytes_and_reject_escapes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let path = "_snapshot/images/suite/test/home.png";
        let first = store_asset(&root, path, b"image").unwrap();
        assert_eq!(first, store_asset(&root, path, b"image").unwrap());
        let changed = store_asset(&root, path, b"changed").unwrap();
        assert_ne!(first, changed);
        assert_eq!(
            versioned_path(&first, "images").unwrap(),
            versioned_path(&changed, "images").unwrap()
        );
        assert_eq!(
            changed,
            format!("{path}?v={}", get_asset_version(b"changed", 20))
        );
        assert!(read_versioned(&root, &first, "images").is_err());
        assert_eq!(
            read_versioned(&root, &changed, "images").unwrap(),
            b"changed"
        );
        for path in [
            "../escape.png",
            "/escape.png",
            "images/../../escape.png",
            "C:\\escape.png",
            "images/CON.png",
        ] {
            assert!(output_file(&root, path, b"bad").is_err(), "{path}");
        }
        assert!(versioned_path("_snapshot/images/../../state.json", "images").is_err());
        fs::write(root.join(path), b"corrupted").unwrap();
        assert!(read_versioned(&root, &first, "images").is_err());
    }

    #[test]
    fn write_failure_preserves_previous_index_and_optional_read_errors_are_not_absence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(root.join("index.html"), "old report").unwrap();
        fs::write(root.join("_snapshot"), "blocking file").unwrap();
        assert!(generate_site(&root, &[], &args(), &WorkspaceConfig::default()).is_err());
        assert_eq!(
            fs::read_to_string(root.join("index.html")).unwrap(),
            "old report"
        );
        fs::create_dir(root.join("not-a-file")).unwrap();
        assert!(checked_file(&root, Path::new("not-a-file"), true).is_err());
        assert!(read_bounded(&root.join("index.html"), 2).is_err());
    }

    #[test]
    fn snapshot_io_errors_identify_output_and_staging_directories() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("report-output");
        let error = generate_site(&root, &[], &args(), &WorkspaceConfig::default()).unwrap_err();
        assert!(error.downcast_ref::<std::io::Error>().is_some());
        let message = format!("{error:#}");
        assert!(
            message.contains("Failed to resolve snapshot output directory"),
            "{message}"
        );
        assert!(message.contains(&root.display().to_string()), "{message}");

        fs::write(&root, b"private blocking contents")?;
        let canonical = root.canonicalize()?;
        let error = generate_site(&root, &[], &args(), &WorkspaceConfig::default()).unwrap_err();
        assert!(error.downcast_ref::<std::io::Error>().is_some());
        let message = format!("{error:#}");
        assert!(
            message.contains("Failed to create snapshot staging directory"),
            "{message}"
        );
        assert!(
            message.contains(&canonical.display().to_string()),
            "{message}"
        );
        assert!(!message.contains("private blocking contents"), "{message}");
        assert_eq!(fs::read(&root)?, b"private blocking contents");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_io_errors_identify_locked_existing_artifacts() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;

        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let initial = SnapshotArgs {
            title: Some("private old report".into()),
            ..args()
        };
        generate_site(&root, &[], &initial, &WorkspaceConfig::default())?;
        let path = root.join("index.html");
        let previous_index = fs::read(&path)?;
        let previous_manifest = fs::read(root.join("_snapshot/data/manifest.json"))?;
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)?;
        let updated = SnapshotArgs {
            title: Some("Updated Title".into()),
            ..args()
        };
        let result = generate_site(&root, &[], &updated, &WorkspaceConfig::default());
        drop(locked);

        let error = result.unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(32)
        );
        let message = format!("{error:#}");
        assert!(
            message.contains("Failed to read existing snapshot artifact"),
            "{message}"
        );
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(!message.contains("private old report"), "{message}");
        assert_eq!(fs::read(&path)?, previous_index);
        assert_eq!(
            fs::read(root.join("_snapshot/data/manifest.json"))?,
            previous_manifest
        );
        assert!(artifact_paths(&root, &[]).is_ok());
        generate_site(&root, &[], &updated, &WorkspaceConfig::default())?;
        assert!(artifact_paths(&root, &[]).is_ok());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_cleanup_keeps_blocked_empty_directories_without_failing() -> Result<()> {
        use crate::diagnostics::{journal::JournalReader, record::*, runtime};
        use std::os::windows::fs::OpenOptionsExt;

        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        generate_site(&root, &[], &args(), &WorkspaceConfig::default())?;
        let before = artifact_paths(&root, &[])?;
        let relative = Path::new("_snapshot/images/suite/diff");
        let directory = root.join(relative);
        fs::create_dir_all(&directory)?;
        let removable = root.join("_snapshot/images/obsolete/test");
        fs::create_dir_all(&removable)?;
        let capture = root.join("suite/bitmaps_reference/capture.png");
        fs::create_dir_all(capture.parent().unwrap())?;
        fs::write(&capture, b"original capture")?;

        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&directory)?;
        let removal = fs::remove_dir(&directory).expect_err("Directory deletion must be blocked");
        assert!(matches!(removal.raw_os_error(), Some(5 | 32)), "{removal}");
        let operation = runtime::Operation::start(&root, OperationKind::Command, None, None, None);
        let result = runtime::in_scope(Some(operation.context()), || {
            generate_site(&root, &[], &args(), &WorkspaceConfig::default())
        });
        drop(locked);

        result?;
        let reference = operation.finish_blocking(Outcome::Success);
        let reader = JournalReader::open(&root, &reference.operation_id)?;
        let mut warnings = 0;
        reader.visit(|record| {
            if record.fields.get(&Field::CleanupFailed) == Some(&FieldValue::Boolean(true)) {
                warnings += 1;
                assert_eq!(record.severity, Severity::Warn);
                assert!(record.error_cause.is_some());
                assert!(matches!(
                    record.fields.get(&Field::OsErrorCode),
                    Some(FieldValue::Number(5 | 32))
                ));
                assert!(
                    !serde_json::to_string(&record)
                        .unwrap()
                        .contains("_snapshot/images")
                );
            }
        })?;
        assert!(
            warnings > 0,
            "Cleanup warnings must survive command completion"
        );
        assert!(directory.is_dir());
        assert!(fs::read_dir(&directory)?.next().is_none());
        assert!(!removable.exists());
        let inventory = artifact_paths(&root, &[])?;
        assert_eq!(inventory, before);
        assert!(!inventory.iter().any(|path| path.starts_with(relative)));
        assert_eq!(fs::read(&capture)?, b"original capture");

        generate_site(&root, &[], &args(), &WorkspaceConfig::default())?;
        assert!(!root.join("_snapshot/images").exists());
        assert_eq!(artifact_paths(&root, &[])?, before);
        assert_eq!(fs::read(&capture)?, b"original capture");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_io_errors_identify_cleanup_paths() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;

        const FILE_SHARE_READ: u32 = 1;
        for (relative, operation) in [
            ("styles.css", "Failed to remove legacy snapshot artifact"),
            (
                "_snapshot/stale.png",
                "Failed to remove stale snapshot artifact",
            ),
        ] {
            let temporary = tempfile::tempdir()?;
            let root = temporary.path().canonicalize()?;
            generate_site(&root, &[], &args(), &WorkspaceConfig::default())?;
            let path = root.join(relative);
            let contents = b"private obsolete artifact contents";
            fs::write(&path, contents)?;
            let locked = fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&path)?;

            let result = generate_site(&root, &[], &args(), &WorkspaceConfig::default());
            drop(locked);

            let error = result.expect_err("Protected snapshot artifacts must not be deleted");
            assert_eq!(
                error
                    .downcast_ref::<std::io::Error>()
                    .unwrap()
                    .raw_os_error(),
                Some(32)
            );
            assert_eq!(fs::read(&path)?, contents);
            assert!(artifact_paths(&root, &[]).is_ok());
            let message = format!("{error:#}");
            assert!(message.contains(operation), "{message}");
            assert!(message.contains(&path.display().to_string()), "{message}");
            assert!(
                !message.contains("private obsolete artifact contents"),
                "{message}"
            );

            generate_site(&root, &[], &args(), &WorkspaceConfig::default())?;
            assert!(!path.exists());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_artifacts_reject_symlinked_parents_and_files() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("_snapshot")).unwrap();
        assert!(store_asset(&root, "_snapshot/images/suite/test/home.png", b"image").is_err());
        fs::write(outside.path().join("image.png"), "private").unwrap();
        std::os::unix::fs::symlink(outside.path().join("image.png"), root.join("linked.png"))
            .unwrap();
        assert!(checked_file(&root, Path::new("linked.png"), false).is_err());
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn compressed_images_refresh_all_versions_without_renaming_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let first = store_asset(
            &root,
            "_snapshot/images/one/reference/home.png",
            b"first image",
        )
        .unwrap();
        let second =
            store_asset(&root, "_snapshot/images/one/test/home.png", b"second image").unwrap();
        let preview = store_asset(
            &root,
            "_snapshot/images/one/test-thumbnail/home.png",
            b"preview image",
        )
        .unwrap();
        let capture: Capture = serde_json::from_value(serde_json::json!({
            "id": "home.png", "label": "Home", "scenario": "Home", "selector": "document", "status": "fail", "threshold": 0.1,
            "mismatch": 1.0, "error": null, "reference": first, "test": second, "diff": null, "referenceUrl": null, "testUrl": null, "dimensions": null,
            "previews": { "reference": null, "test": preview, "diff": null, "grid": preview }
        })).unwrap();
        let report = SuiteReport {
            id: "one".into(),
            display_name: None,
            run_id: "run".into(),
            tests: vec![capture],
        };
        let manifest = Manifest {
            version: 2,
            title: "Review".into(),
            heading: "Review".into(),
            subheading: String::new(),
            generated_at: "2026-09-23T00:00:00Z".into(),
            regressify_version: None,
            suites: vec![SuiteSummary {
                id: "one".into(),
                display_name: None,
                total: 1,
                passed: 0,
                failed: 1,
                errors: 0,
                report: store_json(&root, "_snapshot/data/suites/one.json", &report).unwrap(),
            }],
        };
        for (name, bytes) in FRONTEND_FILES {
            if *name != "gitignore" {
                output_file(&root, name, bytes).unwrap();
            }
        }
        publish_index(&root, &manifest).unwrap();
        let old_index = fs::read(root.join("index.html")).unwrap();
        let first_path = versioned_path(&first, "images").unwrap();
        let second_path = versioned_path(&second, "images").unwrap();
        let preview_path = versioned_path(&preview, "images").unwrap();
        fs::write(root.join(&first_path), b"second image").unwrap();
        fs::write(root.join(&second_path), b"third image").unwrap();
        fs::write(root.join(&preview_path), b"updated preview").unwrap();
        refresh_site_versions(&root).unwrap();
        artifact_paths(&root, &["one".into()]).unwrap();
        assert_ne!(fs::read(root.join("index.html")).unwrap(), old_index);
        let (_, updated) = read_manifest(&root).unwrap();
        let report: SuiteReport = serde_json::from_slice(
            &read_versioned(&root, &updated.suites[0].report, "data").unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_versioned(&root, report.tests[0].reference.as_ref().unwrap(), "images").unwrap(),
            b"second image"
        );
        assert_eq!(
            read_versioned(&root, report.tests[0].test.as_ref().unwrap(), "images").unwrap(),
            b"third image"
        );
        assert_eq!(
            versioned_path(report.tests[0].reference.as_ref().unwrap(), "images").unwrap(),
            first_path
        );
        assert_eq!(
            versioned_path(report.tests[0].test.as_ref().unwrap(), "images").unwrap(),
            second_path
        );
        let previews = report.tests[0].previews.as_ref().unwrap();
        assert_eq!(previews.test, previews.grid);
        assert_eq!(
            read_versioned(&root, previews.grid.as_ref().unwrap(), "images").unwrap(),
            b"updated preview"
        );
        assert_eq!(
            versioned_path(previews.grid.as_ref().unwrap(), "images").unwrap(),
            preview_path
        );
        let index = fs::read(root.join("index.html")).unwrap();
        refresh_site_versions(&root).unwrap();
        assert_eq!(fs::read(root.join("index.html")).unwrap(), index);
        update_suite(&root, "one", Some("One"), None).unwrap();
        let case_renamed = artifact_paths(&root, &["One".into()]).unwrap();
        assert!(case_renamed.contains(&PathBuf::from(
            "_snapshot/images/One/test-thumbnail/home.png"
        )));
        assert_eq!(
            root.join("_snapshot/images/One")
                .canonicalize()
                .unwrap()
                .file_name()
                .and_then(|name| name.to_str()),
            Some("One")
        );
        update_suite(&root, "One", Some("renamed"), None).unwrap();
        let inventory = artifact_paths(&root, &["renamed".into()]).unwrap();
        assert!(inventory.contains(&PathBuf::from(
            "_snapshot/images/renamed/test-thumbnail/home.png"
        )));
        assert!(!root.join(preview_path).exists());
        update_suite(&root, "renamed", None, None).unwrap();
        assert!(!root.join("_snapshot/images/renamed").exists());
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn suite_rename_and_delete_refresh_published_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for id in ["shop", "other"] {
            fs::create_dir_all(root.join(format!("{id}/html_report"))).unwrap();
            fs::write(
                root.join(format!("{id}/html_report/config.js")),
                format!("report({{\"id\":\"{id}\",\"tests\":[]}});"),
            )
            .unwrap();
        }
        generate_site(
            &root,
            &["shop".into(), "other".into()],
            &args(),
            &WorkspaceConfig::default(),
        )
        .unwrap();
        let other = fs::read(root.join("_snapshot/data/suites/other.json")).unwrap();
        update_suite(&root, "shop", Some("checkout"), Some("Customer checkout")).unwrap();
        artifact_paths(&root, &["checkout".into(), "other".into()]).unwrap();
        let (_, manifest) = read_manifest(&root).unwrap();
        assert_eq!(
            manifest.suites[0].display_name.as_deref(),
            Some("Customer checkout")
        );
        assert!(!root.join("_snapshot/data/suites/shop.json").exists());
        assert_eq!(
            fs::read(root.join("_snapshot/data/suites/other.json")).unwrap(),
            other
        );
        assert!(update_suite(&root, "checkout", Some("other"), None).is_err());
        update_suite(&root, "checkout", None, None).unwrap();
        artifact_paths(&root, &["other".into()]).unwrap();
        assert!(!root.join("_snapshot/data/suites/checkout.json").exists());
        update_suite(&root, "other", None, None).unwrap();
        artifact_paths(&root, &[]).unwrap();
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn versioned_image_roles_protect_errors_diffs_and_shared_captures() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let shared = store_asset(&root, "_snapshot/images/one/test/shared.png", b"shared").unwrap();
        let passed = store_asset(
            &root,
            "_snapshot/images/one/reference/passed.png",
            b"passed",
        )
        .unwrap();
        let diff = store_asset(&root, "_snapshot/images/one/diff/shared.png", b"diff").unwrap();
        let preview = store_asset(
            &root,
            "_snapshot/images/one/test-preview/shared.png",
            b"preview",
        )
        .unwrap();
        let tests = [("pass", None, passed.clone()), ("fail", Some("Timeout"), shared.clone())].into_iter().enumerate().map(|(index, (status, error, reference))| serde_json::from_value(serde_json::json!({
            "id": format!("capture-{index}"), "label": "", "scenario": "", "selector": "", "status": status, "threshold": 0.1, "mismatch": null,
            "error": error, "reference": reference, "test": shared, "diff": diff, "referenceUrl": null, "testUrl": null, "dimensions": null,
            "previews": { "reference": null, "test": preview, "diff": null, "grid": preview }
        })).unwrap()).collect();
        let report = SuiteReport {
            id: "one".into(),
            display_name: None,
            run_id: "run".into(),
            tests,
        };
        let manifest = Manifest {
            version: 2,
            title: "Review".into(),
            heading: "Review".into(),
            subheading: String::new(),
            generated_at: "2026-09-23T00:00:00Z".into(),
            regressify_version: None,
            suites: vec![SuiteSummary {
                id: "one".into(),
                display_name: None,
                total: 2,
                passed: 1,
                failed: 1,
                errors: 1,
                report: store_json(&root, "_snapshot/data/suites/one.json", &report).unwrap(),
            }],
        };
        publish_index(&root, &manifest).unwrap();
        let roles = image_roles(&root).unwrap().unwrap();
        assert_eq!(
            roles[&versioned_path(&shared, "images").unwrap()],
            ImageRole::Protected
        );
        assert_eq!(
            roles[&versioned_path(&diff, "images").unwrap()],
            ImageRole::Protected
        );
        assert_eq!(
            roles[&versioned_path(&passed, "images").unwrap()],
            ImageRole::Passed
        );
        assert_eq!(
            roles[&versioned_path(&preview, "images").unwrap()],
            ImageRole::Protected
        );
    }
}
