use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{lock_artifact_file, now_millis, read_bounded_regular_file, require_real_directory};
use crate::helpers::FileLock;

const STAGING_ROOT: &str = ".staging";
const OWNER_FILE: &str = "owner.json";

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StagingKind {
    Deployment,
    Snapshot,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Owner {
    version: u32,
    id: String,
    kind: StagingKind,
    created_at: u64,
}

pub struct StagingDirectory {
    container: PathBuf,
    files: PathBuf,
    lease: Option<FileLock>,
}

impl StagingDirectory {
    pub fn create(output: &Path, kind: StagingKind) -> Result<Self> {
        fs::create_dir_all(output)?;
        let output = output.canonicalize()?;
        let _catalog = lock_artifact_file(&output, ".staging.lock", false)?;
        let root = output.join(STAGING_ROOT);
        fs::create_dir_all(&root)?;
        require_real_directory(&root)?;
        let id = uuid::Uuid::new_v4().to_string();
        let container = root.join(&id);
        fs::create_dir(&container)?;
        let lease = lock_artifact_file(&container, ".lock", true)?;
        let staging = Self {
            files: container.join("files"),
            container,
            lease: Some(lease),
        };
        crate::helpers::write_file_atomically(
            &staging.container.join(OWNER_FILE),
            &serde_json::to_vec(&Owner {
                version: 1,
                id,
                kind,
                created_at: now_millis(),
            })?,
        )?;
        fs::create_dir(staging.path())?;
        Ok(staging)
    }

    pub fn claim(files: &Path) -> Result<Self> {
        let container = files.parent().context("Staging container is missing")?;
        ensure!(
            files.file_name().is_some_and(|name| name == "files")
                && container
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == STAGING_ROOT),
            "Deployment scratch directory is not managed staging"
        );
        let staging_root = container.parent().context("Staging root is missing")?;
        require_real_directory(staging_root)?;
        let output = staging_root.parent().context("Staging output is missing")?;
        let _catalog = lock_artifact_file(output, ".staging.lock", false)?;
        require_real_directory(container)?;
        require_real_directory(files)?;
        let lease = lock_artifact_file(container, ".lock", true)?;
        ensure!(
            read_owner(container)?.is_some(),
            "Staging ownership is missing"
        );
        Ok(Self {
            container: container.to_path_buf(),
            files: files.to_path_buf(),
            lease: Some(lease),
        })
    }

    pub fn path(&self) -> &Path {
        &self.files
    }
}

fn read_owner(container: &Path) -> Result<Option<Owner>> {
    let Some(bytes) =
        read_bounded_regular_file(&container.join(OWNER_FILE), 8192, "staging owner")?
    else {
        return Ok(None);
    };
    let owner: Owner = serde_json::from_slice(&bytes).context("Invalid staging owner")?;
    ensure!(
        owner.version == 1
            && container.file_name().and_then(|name| name.to_str()) == Some(owner.id.as_str())
            && uuid::Uuid::parse_str(&owner.id).is_ok(),
        "Invalid staging ownership identity"
    );
    Ok(Some(owner))
}

fn cleanup_container(container: &Path) -> Result<bool> {
    if !container.try_exists()? {
        return Ok(true);
    }
    require_real_directory(container)?;
    let lock = container.join(".lock");
    if let Ok(metadata) = fs::symlink_metadata(&lock) {
        ensure!(
            metadata.file_type().is_file(),
            "Staging lock must be a regular file"
        );
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)?;
    let lease = match FileLock::try_exclusive(file) {
        Ok(lease) => lease,
        Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    };
    let owner = read_owner(container)?;
    let payload = container.join("files");
    if owner.is_none() {
        ensure!(
            !payload.try_exists()?,
            "Unrecognized staging payload retained at '{}'",
            container.display()
        );
    }
    for entry in fs::read_dir(container)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("Invalid staging file name")?;
        ensure!(
            matches!(name, ".lock" | OWNER_FILE | "files")
                || (name.starts_with(".owner.json.") && name.ends_with(".tmp")),
            "Unrecognized staging content retained at '{}'",
            container.display()
        );
        ensure!(
            !entry.file_type()?.is_symlink(),
            "Staging control files must not be links"
        );
    }
    if payload.try_exists()? {
        require_real_directory(&payload)?;
        for entry in walkdir::WalkDir::new(&payload).follow_links(false) {
            let entry = entry?;
            ensure!(
                entry.file_type().is_file() || entry.file_type().is_dir(),
                "Staging cleanup refuses linked or special files: {}",
                entry.path().display()
            );
        }
        fs::remove_dir_all(&payload)
            .with_context(|| format!("Cannot remove staging payload '{}'", payload.display()))?;
    }
    // Ownership survives failed payload removal, allowing the next deployment to retry.
    drop(lease);
    for entry in fs::read_dir(container)? {
        fs::remove_file(entry?.path())?;
    }
    fs::remove_dir(container)?;
    Ok(true)
}

pub fn cleanup_abandoned(output: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    let result = (|| -> Result<()> {
        if !output.try_exists()? {
            return Ok(());
        }
        let output = output.canonicalize()?;
        let _catalog = lock_artifact_file(&output, ".staging.lock", false)?;
        let root = output.join(STAGING_ROOT);
        if !root.try_exists()? {
            return Ok(());
        }
        require_real_directory(&root)?;
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            let result = (|| -> Result<()> {
                ensure!(
                    entry.file_type()?.is_dir()
                        && entry
                            .file_name()
                            .to_str()
                            .is_some_and(|name| uuid::Uuid::parse_str(name).is_ok()),
                    "Unrecognized staging entry retained at '{}'",
                    path.display()
                );
                cleanup_container(&path)?;
                Ok(())
            })();
            if let Err(error) = result {
                warnings.push(format!("Staging cleanup deferred: {error:#}"));
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        warnings.push(format!("Staging cleanup could not finish: {error:#}"));
    }
    warnings
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        drop(self.lease.take());
        let cleanup = (|| -> Result<()> {
            if !self.container.try_exists()? {
                return Ok(());
            }
            let output = self
                .container
                .parent()
                .and_then(Path::parent)
                .context("Staging output is missing")?;
            // Serialize the final close/unlink interval with new claims and catalog sweeps.
            let _catalog = lock_artifact_file(output, ".staging.lock", false)?;
            cleanup_container(&self.container)?;
            Ok(())
        })();
        if let Err(error) = cleanup {
            eprintln!(
                "Staging cleanup deferred; the next deployment will retry '{}': {error:#}",
                self.container.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_cleanup_removes_payload_after_last_owner_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let owner = StagingDirectory::create(temp.path(), StagingKind::Deployment).unwrap();
        let container = owner.container.clone();
        fs::write(owner.path().join("report.zip"), b"staged").unwrap();
        let worker = StagingDirectory::claim(owner.path()).unwrap();
        drop(owner);
        assert!(container.join("files/report.zip").is_file());
        drop(worker);
        assert!(!container.exists());
    }

    #[test]
    fn staging_catalog_lock_blocks_new_claims() {
        let temp = tempfile::tempdir().unwrap();
        let stage = StagingDirectory::create(temp.path(), StagingKind::Deployment).unwrap();
        let catalog = lock_artifact_file(temp.path(), ".staging.lock", false).unwrap();
        assert!(StagingDirectory::claim(stage.path()).is_err());
        drop(catalog);
        assert!(StagingDirectory::claim(stage.path()).is_ok());
    }

    #[test]
    fn staging_catalog_lock_defers_teardown_without_losing_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let stage = StagingDirectory::create(temp.path(), StagingKind::Deployment).unwrap();
        let container = stage.container.clone();
        let catalog = lock_artifact_file(temp.path(), ".staging.lock", false).unwrap();
        drop(stage);
        assert!(container.join(OWNER_FILE).is_file());
        drop(catalog);
        assert!(cleanup_abandoned(temp.path()).is_empty());
        assert!(!container.exists());
    }

    #[test]
    fn staging_cleanup_reclaims_interrupted_operations_without_touching_active_ones() {
        let temp = tempfile::tempdir().unwrap();
        let active = StagingDirectory::create(temp.path(), StagingKind::Deployment).unwrap();
        let mut abandoned = std::mem::ManuallyDrop::new(
            StagingDirectory::create(temp.path(), StagingKind::Snapshot).unwrap(),
        );
        fs::write(abandoned.path().join("image.png"), b"image").unwrap();
        drop(abandoned.lease.take());
        assert!(cleanup_abandoned(temp.path()).is_empty());
        assert!(!abandoned.container.exists());
        assert!(active.path().is_dir());
        drop(std::mem::ManuallyDrop::into_inner(abandoned));
    }

    #[cfg(windows)]
    #[test]
    fn staging_cleanup_failure_preserves_ownership_for_the_next_attempt() {
        use std::os::windows::fs::OpenOptionsExt;
        let temp = tempfile::tempdir().unwrap();
        let stage = StagingDirectory::create(temp.path(), StagingKind::Deployment).unwrap();
        let container = stage.container.clone();
        let path = stage.path().join("locked.zip");
        fs::write(&path, b"package").unwrap();
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        drop(stage);
        assert!(container.join(OWNER_FILE).is_file());
        assert!(path.is_file());
        drop(locked);
        assert!(cleanup_abandoned(temp.path()).is_empty());
        assert!(!container.exists());
    }

    #[test]
    fn staging_cleanup_reports_unknown_content_without_deleting_it() {
        let temp = tempfile::tempdir().unwrap();
        let unknown = temp
            .path()
            .join(STAGING_ROOT)
            .join(uuid::Uuid::new_v4().to_string())
            .join("files");
        fs::create_dir_all(&unknown).unwrap();
        fs::write(unknown.join("keep"), b"unknown").unwrap();
        assert!(!cleanup_abandoned(temp.path()).is_empty());
        assert!(unknown.join("keep").is_file());
    }
}
