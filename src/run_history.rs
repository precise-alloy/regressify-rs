use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::compare::report_gen::read_bounded_regular_file;
use crate::helpers::FileLock;

pub mod cleanup;
pub mod staging;

pub const REGISTRY_FILE: &str = "runs.json";
const REGISTRY_VERSION: u32 = 1;
const MAX_REGISTRY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RunState {
    Running,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub id: String,
    pub state: RunState,
    pub full_suite: bool,
    pub started_at: u64,
    pub completed_at: Option<u64>,
    #[serde(default)]
    pub compared: bool,
    #[serde(default)]
    pub legacy: bool,
}

impl RunRecord {
    pub fn is_default_candidate(&self) -> bool {
        self.state == RunState::Completed && self.full_suite
    }

    pub fn order_key(&self) -> (u64, &str) {
        (self.completed_at.unwrap_or(self.started_at), &self.id)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunRegistry {
    version: u32,
    runs: Vec<RunRecord>,
    #[serde(default)]
    pending_logs: Vec<PendingLog>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingLog {
    run_id: String,
    operation_id: crate::diagnostics::record::OperationId,
}

fn read_registry(suite_dir: &Path) -> Result<Option<RunRegistry>> {
    let path = suite_dir.join(REGISTRY_FILE);
    let Some(bytes) = read_bounded_regular_file(&path, MAX_REGISTRY_BYTES, "run registry")? else {
        return Ok(None);
    };
    let registry: RunRegistry = serde_json::from_slice(&bytes)
        .with_context(|| format!("Invalid run registry '{}'", path.display()))?;
    anyhow::ensure!(
        registry.version == REGISTRY_VERSION,
        "Unsupported run registry version {}",
        registry.version
    );
    let mut ids = HashSet::new();
    for run in &registry.runs {
        crate::cli::args::run_id_parser(&run.id).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(ids.insert(&run.id), "Duplicate run ID in run registry");
    }
    for log in &registry.pending_logs {
        crate::cli::args::run_id_parser(&log.run_id).map_err(anyhow::Error::msg)?;
    }
    Ok(Some(registry))
}

pub fn is_tracked(suite_dir: &Path) -> Result<bool> {
    Ok(read_registry(suite_dir)?.is_some())
}

pub fn capture_runs(tests_dir: &Path) -> Result<Vec<RunRecord>> {
    let suite_dir = tests_dir
        .parent()
        .context("Capture directory has no suite parent")?;
    let registry = read_registry(suite_dir)?;
    let mut runs = Vec::new();
    for entry in fs::read_dir(tests_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().into_string().map_err(|name| {
            anyhow::anyhow!(
                "Capture run directory in '{}' is not valid UTF-8: {:?}",
                tests_dir.display(),
                name
            )
        })?;
        crate::cli::args::run_id_parser(&id).map_err(anyhow::Error::msg)?;
        if let Some(registry) = &registry {
            if let Some(run) = registry.runs.iter().find(|run| run.id == id) {
                runs.push(run.clone());
            }
        } else {
            let timestamp = entry
                .metadata()?
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64;
            runs.push(RunRecord {
                id,
                state: RunState::Completed,
                full_suite: true,
                started_at: timestamp,
                completed_at: Some(timestamp),
                compared: entry.path().join("report.json").is_file(),
                legacy: true,
            });
        }
    }
    runs.sort_by(|left, right| right.order_key().cmp(&left.order_key()));
    Ok(runs)
}

pub fn latest_completed_capture_run(tests_dir: &Path) -> Result<Option<String>> {
    Ok(capture_runs(tests_dir)?
        .into_iter()
        .find(RunRecord::is_default_candidate)
        .map(|run| run.id))
}

pub(crate) fn require_real_directory(path: &Path) -> Result<()> {
    anyhow::ensure!(
        fs::symlink_metadata(path)?.file_type().is_dir(),
        "Artifact directory '{}' must be a real directory, not a link",
        path.display()
    );
    Ok(())
}

fn write_registry(suite_dir: &Path, runs: Vec<RunRecord>) -> Result<()> {
    let pending_logs = read_registry(suite_dir)?
        .map(|registry| registry.pending_logs)
        .unwrap_or_default();
    write_registry_with_pending(suite_dir, runs, pending_logs)
}

fn write_registry_with_pending(
    suite_dir: &Path,
    runs: Vec<RunRecord>,
    pending_logs: Vec<PendingLog>,
) -> Result<()> {
    let path = suite_dir.join(REGISTRY_FILE);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        anyhow::ensure!(
            metadata.file_type().is_file(),
            "Run registry must be a regular file"
        );
    }
    let bytes = serde_json::to_vec_pretty(&RunRegistry {
        version: REGISTRY_VERSION,
        runs,
        pending_logs,
    })?;
    anyhow::ensure!(
        bytes.len() <= MAX_REGISTRY_BYTES,
        "Run registry exceeds its size limit"
    );
    crate::helpers::write_file_atomically(&path, &bytes)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn lock_artifact_file(suite_dir: &Path, name: &str, shared: bool) -> Result<FileLock> {
    fs::create_dir_all(suite_dir)?;
    require_real_directory(suite_dir)?;
    let path = suite_dir.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_file(),
                "Run lock must be a regular file"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let result = if shared {
        FileLock::try_shared(file)
    } else {
        FileLock::try_exclusive(file)
    };
    result.map_err(|error| {
        anyhow::anyhow!(
            "Suite artifacts are busy at '{}': {error}",
            suite_dir.display()
        )
    })
}

pub struct ArtifactLease {
    _file: FileLock,
}

impl ArtifactLease {
    pub fn read(suite_dir: &Path) -> Result<Self> {
        Ok(Self {
            _file: lock_artifact_file(suite_dir, ".artifacts.lock", true)?,
        })
    }
}

pub struct SuiteLease {
    suite_dir: PathBuf,
    _file: FileLock,
    _artifacts: ArtifactLease,
}

impl SuiteLease {
    pub fn acquire(suite_dir: &Path) -> Result<Self> {
        Self::acquire_inner(suite_dir, false)
    }

    pub fn for_cleanup(suite_dir: &Path) -> Result<Self> {
        Self::acquire_inner(suite_dir, true)
    }

    fn acquire_inner(suite_dir: &Path, cleanup: bool) -> Result<Self> {
        let artifacts = ArtifactLease {
            _file: lock_artifact_file(suite_dir, ".artifacts.lock", !cleanup)?,
        };
        let file = lock_artifact_file(suite_dir, ".runs.lock", false)?;
        Ok(Self {
            suite_dir: suite_dir.to_path_buf(),
            _file: file,
            _artifacts: artifacts,
        })
    }

    pub fn mark_compared(&self, run_id: &str) -> Result<()> {
        if let Some(mut registry) = read_registry(&self.suite_dir)? {
            let record = registry
                .runs
                .iter_mut()
                .find(|run| run.id == run_id)
                .context("Compared run is missing from its registry")?;
            record.compared = true;
            write_registry(&self.suite_dir, registry.runs)?;
        }
        Ok(())
    }
}

pub struct RunLease {
    lease: SuiteLease,
    run_id: String,
    active: bool,
    full_suite: bool,
}

impl RunLease {
    pub fn suite_dir(&self) -> &Path {
        &self.lease.suite_dir
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn is_full_suite(&self) -> bool {
        self.full_suite
    }

    pub fn begin(suite_dir: &Path, run_id: &str, full_suite: bool) -> Result<Self> {
        crate::cli::args::run_id_parser(run_id).map_err(anyhow::Error::msg)?;
        let lease = SuiteLease::acquire(suite_dir)?;
        let tests_dir = suite_dir.join("bitmaps_test");
        fs::create_dir_all(&tests_dir)?;
        require_real_directory(&tests_dir)?;
        let mut runs = capture_runs(&tests_dir)?;
        anyhow::ensure!(
            !runs.iter().any(|run| run.id == run_id)
                && fs::symlink_metadata(tests_dir.join(run_id))
                    .is_err_and(|error| { error.kind() == std::io::ErrorKind::NotFound }),
            "Capture run '{run_id}' already exists or cannot be inspected"
        );
        for run in &mut runs {
            if run.state == RunState::Running {
                run.state = RunState::Interrupted;
            }
        }
        let started_at = now_millis().max(
            runs.iter()
                .map(|run| run.completed_at.unwrap_or(run.started_at).saturating_add(1))
                .max()
                .unwrap_or_default(),
        );
        runs.push(RunRecord {
            id: run_id.to_owned(),
            state: RunState::Running,
            full_suite,
            started_at,
            completed_at: None,
            compared: false,
            legacy: false,
        });
        // Register before creating captures so abrupt termination cannot produce a trusted legacy run.
        write_registry(suite_dir, runs)?;
        fs::create_dir(tests_dir.join(run_id))?;
        Ok(Self {
            lease,
            run_id: run_id.to_owned(),
            active: true,
            full_suite,
        })
    }

    pub fn finish(&mut self, state: RunState, compared: bool) -> Result<()> {
        anyhow::ensure!(
            state != RunState::Running,
            "A finished run needs a terminal state"
        );
        let mut registry =
            read_registry(&self.lease.suite_dir)?.context("Active run registry is missing")?;
        let record = registry
            .runs
            .iter_mut()
            .find(|run| run.id == self.run_id)
            .context("Active run is missing from its registry")?;
        record.state = state;
        record.completed_at =
            (state == RunState::Completed).then(|| now_millis().max(record.started_at));
        record.compared = compared;
        write_registry(&self.lease.suite_dir, registry.runs)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        if self.active && self.finish(RunState::Interrupted, false).is_err() {
            eprintln!(
                "Could not record interrupted run state; the run remains ineligible for default results"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_history_completion_and_interruption_preserve_full_suite_default() {
        let temp = tempfile::tempdir().unwrap();
        let suite = temp.path().join("shop");
        let tests = suite.join("bitmaps_test");
        let mut run = RunLease::begin(&suite, "full", true).unwrap();
        assert_eq!(latest_completed_capture_run(&tests).unwrap(), None);
        run.finish(RunState::Completed, false).unwrap();
        drop(run);
        assert_eq!(
            latest_completed_capture_run(&tests).unwrap().as_deref(),
            Some("full")
        );

        drop(RunLease::begin(&suite, "stopped", true).unwrap());
        assert_eq!(
            capture_runs(&tests).unwrap()[0].state,
            RunState::Interrupted
        );
        let mut partial = RunLease::begin(&suite, "partial", false).unwrap();
        partial.finish(RunState::Completed, true).unwrap();
        drop(partial);
        assert_eq!(
            latest_completed_capture_run(&tests).unwrap().as_deref(),
            Some("full")
        );
        let mut failed = RunLease::begin(&suite, "failed", true).unwrap();
        failed.finish(RunState::Failed, false).unwrap();
        drop(failed);
        assert_eq!(
            latest_completed_capture_run(&tests).unwrap().as_deref(),
            Some("full")
        );
    }

    #[test]
    fn run_history_locks_protect_active_suites_independently() {
        let temp = tempfile::tempdir().unwrap();
        let suite = temp.path().join("shop");
        let active = RunLease::begin(&suite, "active", true).unwrap();
        assert!(SuiteLease::acquire(&suite).is_err());
        let reader = ArtifactLease::read(&suite).unwrap();
        assert!(SuiteLease::for_cleanup(&suite).is_err());
        assert!(RunLease::begin(&suite, "other", true).is_err());
        let other_suite = SuiteLease::acquire(&temp.path().join("other")).unwrap();
        drop(other_suite);
        drop(active);
        assert!(SuiteLease::acquire(&suite).is_ok());
        assert!(SuiteLease::for_cleanup(&suite).is_err());
        drop(reader);
        assert!(SuiteLease::for_cleanup(&suite).is_ok());
    }

    #[test]
    fn run_history_imports_legacy_before_registering_new_capture() {
        let temp = tempfile::tempdir().unwrap();
        let suite = temp.path().join("shop");
        let tests = suite.join("bitmaps_test");
        fs::create_dir_all(tests.join("z-legacy")).unwrap();
        let mut run = RunLease::begin(&suite, "a-new", true).unwrap();
        assert_eq!(
            latest_completed_capture_run(&tests).unwrap().as_deref(),
            Some("z-legacy")
        );
        assert!(capture_runs(&tests).unwrap().iter().any(|run| run.legacy));
        run.finish(RunState::Completed, true).unwrap();
        assert_eq!(
            latest_completed_capture_run(&tests).unwrap().as_deref(),
            Some("a-new")
        );
    }

    #[test]
    fn run_history_rejects_invalid_registry_without_falling_back() {
        let temp = tempfile::tempdir().unwrap();
        let tests = temp.path().join("bitmaps_test");
        fs::create_dir_all(tests.join("legacy")).unwrap();
        for bytes in [
            "not json",
            r#"{"version":2,"runs":[]}"#,
            r#"{"version":1,"runs":[{"id":"../escape","state":"completed","fullSuite":true,"startedAt":0,"completedAt":0}]}"#,
        ] {
            fs::write(temp.path().join(REGISTRY_FILE), bytes).unwrap();
            assert!(capture_runs(&tests).is_err());
        }
    }
}
