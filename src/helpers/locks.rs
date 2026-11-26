#![allow(dead_code)]

use std::{
    fs::{File, TryLockError},
    sync::OnceLock,
};

use anyhow::{Result, anyhow};

#[derive(Debug)]
pub(crate) struct FileLock(File);

impl FileLock {
    /// Adopt the lock after a caller finishes retrying on the same open file.
    pub(crate) fn from_locked(file: File) -> Self {
        Self(file)
    }

    pub(crate) fn try_exclusive(file: File) -> Result<Self, TryLockError> {
        file.try_lock()?;
        Ok(Self(file))
    }

    pub(crate) fn try_shared(file: File) -> Result<Self, TryLockError> {
        file.try_lock_shared()?;
        Ok(Self(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // A concurrent Unix fork shares the open file description until exec. Closing
        // our descriptor alone would leave the child's copy holding this lease.
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "Could not unlock lease before closing its file");
        }
    }
}

pub trait OnceLockExtensions<T> {
    fn get_value(&self) -> Option<&T>;
    fn get_required(&self) -> Result<&T>;
    fn set_value(&self, value: T) -> Result<()>;
}

pub trait OnceLockBoolExtensions {
    fn get_bool(&self) -> bool;
    fn set_bool(&self, value: bool) -> Result<()>;
}

impl<T> OnceLockExtensions<T> for OnceLock<T> {
    fn get_value(&self) -> Option<&T> {
        self.get()
    }

    fn get_required(&self) -> Result<&T> {
        self.get().ok_or_else(|| anyhow!("Value not set"))
    }

    fn set_value(&self, value: T) -> Result<()> {
        self.set(value).map_err(|_| anyhow!("Failed to set value"))
    }
}

impl OnceLockBoolExtensions for OnceLock<bool> {
    fn get_bool(&self) -> bool {
        *self.get_value().unwrap_or(&false)
    }

    fn set_bool(&self, value: bool) -> Result<()> {
        self.set(value).map_err(|_| anyhow!("Failed to set value"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn dropping_file_lock_releases_ownership_with_a_fork_duplicate_alive() {
        for acquire in [FileLock::try_shared, FileLock::try_exclusive] {
            let path = tempfile::NamedTempFile::new().unwrap();
            let file = path.reopen().unwrap();
            // dup and fork share the same open file description; no timing or child needed.
            let inherited = file.try_clone().unwrap();
            let lease = acquire(file).unwrap();
            let peer = path.reopen().unwrap();
            assert!(matches!(peer.try_lock(), Err(TryLockError::WouldBlock)));
            drop(lease);
            peer.try_lock()
                .expect("the lease, not its duplicate, owns the lock");
            drop(inherited);
        }
    }
}
