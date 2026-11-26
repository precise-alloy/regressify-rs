use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::helpers::FileLock;

const LIMIT: usize = 100;
const MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const QUEUE: &str = "queue-v1.json";
const STAGING: &str = "queue-v1.tmp";

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Pending {
    pub id: Uuid,
    pub created_ms: i64,
    pub object: String,
    pub verb: String,
    pub properties: Map<String, Value>,
    pub attempts: u8,
    pub due_ms: i64,
}

pub(super) enum Selection {
    Ready(Pending),
    Wait(Duration),
}

#[derive(Clone)]
pub(super) struct Outbox(PathBuf);

impl Outbox {
    pub fn new(root: PathBuf) -> Self {
        Self(root)
    }

    fn exists(&self) -> Result<bool> {
        match fs::symlink_metadata(&self.0) {
            Ok(meta) => {
                ensure!(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "Invalid analytics directory"
                );
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn lockfile(&self, name: &str) -> io::Result<File> {
        regular_or_missing(&self.0.join(name))?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.0.join(name))
    }

    fn lock_queue(&self) -> Result<File> {
        let lock = self.lockfile("queue.lock")?;
        lock.lock()?;
        // A killed writer can leave a partial staging file. It was never committed.
        remove_if_present(&self.0.join(STAGING))?;
        Ok(lock)
    }

    pub fn try_sender(&self) -> Result<Option<FileLock>> {
        if !self.exists()? {
            return Ok(None);
        }
        let file = self.lockfile("sender.lock")?;
        match FileLock::try_exclusive(file) {
            Ok(lease) => Ok(Some(lease)),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    fn read(&self) -> Result<VecDeque<Pending>> {
        let path = self.0.join(QUEUE);
        regular_or_missing(&path)?;
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(VecDeque::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, queue: &VecDeque<Pending>) -> Result<()> {
        let path = self.0.join(QUEUE);
        if queue.is_empty() {
            remove_if_present(&path)?;
        } else {
            // A single known temporary name under the queue lock lets opt-out
            // erase interrupted writes too. Both files are private at creation.
            let staging = self.0.join(STAGING);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&staging)?;
            file.write_all(&serde_json::to_vec(queue)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(staging, path)?;
        }
        #[cfg(unix)]
        File::open(&self.0)?.sync_all()?;
        Ok(())
    }

    pub fn push(&self, item: Pending) -> Result<()> {
        #[cfg(unix)]
        ensure_durable_dir(&self.0, &|dir| File::open(dir)?.sync_all())?;
        #[cfg(not(unix))]
        if !self.exists()? {
            fs::create_dir_all(&self.0)?;
        }
        ensure!(self.exists()?, "Missing analytics directory");
        let _lock = self.lock_queue()?;
        let mut queue = self.read()?;
        prune(&mut queue, Utc::now().timestamp_millis());
        queue.push_back(item);
        queue.drain(..queue.len().saturating_sub(LIMIT));
        self.save(&queue)
    }

    pub fn select(&self, now: i64) -> Result<Selection> {
        let _lock = self.lock_queue()?;
        let mut queue = self.read()?;
        if prune(&mut queue, now) {
            self.save(&queue)?;
        }
        // A wall-clock rollback must not turn a future timestamp into a rejected event.
        let due = |item: &Pending| item.due_ms.max(item.created_ms);
        if let Some(item) = queue.iter().find(|item| due(item) <= now) {
            return Ok(Selection::Ready(item.clone()));
        }
        let wait = queue
            .iter()
            .map(|item| Duration::from_millis(due(item).abs_diff(now)))
            .min();
        Ok(Selection::Wait(wait.unwrap_or(super::POLL_INTERVAL)))
    }

    pub fn finish(&self, id: Uuid, retry: bool, now: i64) -> Result<()> {
        let _lock = self.lock_queue()?;
        let mut queue = self.read()?;
        // The record may have expired, been evicted, or been purged in flight.
        // Never rewrite a stale snapshot or reinsert an unacknowledged record.
        let Some(index) = queue.iter().position(|item| item.id == id) else {
            return Ok(());
        };
        if retry {
            let item = &mut queue[index];
            let (attempts, delay_ms) = match item.attempts {
                0 => (1, 1000),
                1 => (2, 2000),
                _ => (0, 5 * 60 * 1000),
            };
            item.attempts = attempts;
            item.due_ms = now.saturating_add(delay_ms);
        } else {
            queue.remove(index);
        }
        self.save(&queue)
    }

    pub async fn dispatch_if_pending<F: Future>(
        &self,
        id: Uuid,
        request: F,
    ) -> Result<Option<F::Output>> {
        let mut request = std::pin::pin!(request);
        let first = {
            let _lock = self.lock_queue()?;
            if !self.read()?.iter().any(|item| item.id == id) {
                return Ok(None);
            }
            // Commit dispatch (first poll) under the same lock as purge/eviction.
            // Never wait for network readiness or acknowledgement with that lock.
            futures::poll!(request.as_mut())
        };
        Ok(Some(match first {
            std::task::Poll::Ready(result) => result,
            std::task::Poll::Pending => request.await,
        }))
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> Result<bool> {
        let _lock = self.lock_queue()?;
        Ok(self.read()?.is_empty())
    }

    pub fn purge(&self) -> Result<()> {
        if self.exists()? {
            let _lock = self.lock_queue()?;
            self.save(&VecDeque::new())?;
        }
        Ok(())
    }
}

fn prune(queue: &mut VecDeque<Pending>, now: i64) -> bool {
    let before = queue.len();
    queue.retain(|item| item.created_ms > now.saturating_sub(MAX_AGE_MS));
    before != queue.len()
}

#[cfg(unix)]
fn ensure_durable_dir(path: &Path, sync_dir: &impl Fn(&Path) -> io::Result<()>) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if !path.try_exists()? {
        ensure_durable_dir(parent, sync_dir)?;
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => (),
            result => result?,
        }
    }
    // A concurrent writer may see a new directory before its creator syncs it.
    // Persist the entry before relying on it or creating the next component.
    sync_dir(parent)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn regular_or_missing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(io::Error::other("Invalid analytics file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::anonymous_event;
    use serde_json::json;

    fn event(n: usize) -> Pending {
        anonymous_event("suite", "finished", json!({"n": n})).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn first_enqueue_syncs_new_parent_entries_before_using_them() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("regressify");
        let queue = app.join("analytics");
        let fail_parent_sync = |parent: &Path| {
            if parent == root.path() {
                Err(io::Error::other("injected sync failure"))
            } else {
                File::open(parent)?.sync_all()
            }
        };
        assert!(ensure_durable_dir(&queue, &fail_parent_sync).is_err());
        assert!(app.is_dir());
        assert!(!queue.exists(), "Cannot use an uncommitted parent");
        let synced = std::cell::RefCell::new(Vec::new());
        ensure_durable_dir(&queue, &|parent| {
            File::open(parent)?.sync_all()?;
            synced.borrow_mut().push(parent.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(synced.into_inner(), [root.path(), app.as_path()]);
        let outbox = Outbox::new(queue);
        let item = event(1);
        let id = item.id;
        outbox.push(item).unwrap();
        assert!(
            matches!(outbox.select(Utc::now().timestamp_millis()).unwrap(), Selection::Ready(p) if p.id == id)
        );
    }

    #[test]
    fn capacity_evicts_in_enqueue_order_and_expiry_includes_day_thirty() {
        let root = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(root.path().into());
        let mut initial: VecDeque<_> = (0..LIMIT).map(event).collect();
        let now = Utc::now().timestamp_millis();
        initial[4].created_ms = now - MAX_AGE_MS + 60_000;
        outbox.save(&initial).unwrap();
        outbox.push(event(LIMIT)).unwrap();
        let records = outbox.read().unwrap();
        assert_eq!(records.len(), LIMIT);
        assert_eq!(records.front().unwrap().properties["n"], 1);
        assert_eq!(records.back().unwrap().properties["n"], LIMIT);
        outbox.select(now + 59_999).unwrap();
        assert_eq!(outbox.read().unwrap().len(), LIMIT);
        outbox.select(now + 60_000).unwrap();
        let records = outbox.read().unwrap();
        assert_eq!(records.len(), LIMIT - 1);
        assert!(!records.iter().any(|item| item.properties["n"] == 4));
    }

    #[test]
    fn retries_are_spaced_across_reopens_and_other_events_stay_eligible() {
        let root = tempfile::tempdir().unwrap();
        let open = || Outbox::new(root.path().into());
        let first = event(1);
        let id = first.id;
        open().push(first).unwrap();
        let mut now = Utc::now().timestamp_millis();
        for delay in [1000, 2000, 300_000, 1000] {
            open().finish(id, true, now).unwrap();
            assert!(
                matches!(open().select(now).unwrap(), Selection::Wait(d) if d.as_millis() == delay)
            );
            let second = event(2);
            let other_id = second.id;
            open().push(second).unwrap();
            let current = now.max(Utc::now().timestamp_millis());
            assert!(
                matches!(open().select(current).unwrap(), Selection::Ready(p) if p.id == other_id)
            );
            open().finish(other_id, false, current).unwrap();
            now += delay as i64;
            assert!(matches!(open().select(now).unwrap(), Selection::Ready(p) if p.id == id));
        }
    }

    #[test]
    fn opt_out_purges_committed_and_interrupted_writes_even_if_unreadable() {
        let root = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(root.path().into());
        let item = event(1);
        let id = item.id;
        outbox.push(item).unwrap();
        let _in_flight = outbox.try_sender().unwrap().unwrap();
        fs::write(root.path().join(STAGING), b"interrupted payload").unwrap();
        fs::write(root.path().join(QUEUE), b"invalid JSON").unwrap();
        crate::analytics::start_using(outbox.clone(), true);
        outbox
            .finish(id, true, Utc::now().timestamp_millis())
            .unwrap();
        assert!(!root.path().join(STAGING).exists());
        assert!(!root.path().join(QUEUE).exists());
    }

    #[test]
    fn concurrent_writers_do_not_lose_or_duplicate_events() {
        let root = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(root.path().join("analytics"));
        std::thread::scope(|s| {
            for n in 0..12 {
                let outbox = &outbox;
                s.spawn(move || outbox.push(event(n)).unwrap());
            }
        });
        let mut actual: Vec<_> = outbox
            .read()
            .unwrap()
            .into_iter()
            .map(|p| p.properties["n"].as_u64().unwrap())
            .collect();
        actual.sort();
        assert_eq!(actual, (0..12).collect::<Vec<_>>());
    }
}
