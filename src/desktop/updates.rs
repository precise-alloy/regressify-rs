use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{StatusCode, Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod macos_install;

pub const CURRENT_VERSION: &str = match option_env!("REGRESSIFY_DESKTOP_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
const DOWNLOADS_PATH: &str = "downloads";
const RELEASE_DOWNLOAD_PREFIX: &str =
    "https://github.com/precise-alloy/regressify-rs/releases/download/";
const RELEASE_TAG_PREFIX: &str = "https://github.com/precise-alloy/regressify-rs/releases/tag/";
const NOTICE_STATE_NAME: &str = "notice.json";
const DESKTOP_ARCHIVE_NAME: &str = "regressify-desktop-x86_64-pc-windows-msvc.zip";
const CLI_ARCHIVE_NAME: &str = "regressify-cli-x86_64-pc-windows-msvc.zip";
const PREPARED_MANIFEST_NAME: &str = "prepared.txt";
const PREPARE_ERROR_NAME: &str = "prepare-error.txt";
const MAX_MANIFEST_SIZE: u64 = 16 * 1024;
const MAX_INSTALLER_SIZE: u64 = 1024 * 1024 * 1024;
const MAX_REDIRECT_HOPS: usize = 5;
const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_HASH_RETRIES: u8 = 3;
const HASH_RETRY_DELAY: Duration = Duration::from_secs(2 * 60 * 60);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(8 * 60 * 60);
pub const SHARED_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Version([u16; 4]);

impl FromStr for Version {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let mut parts = [0; 4];
        ensure!(value.len() <= 32, "Version is too long");
        for (index, part) in value.split('.').enumerate() {
            ensure!(
                index < 4 && !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()),
                "Version must have one to four numeric components"
            );
            let number: u16 = part.parse().context("Invalid version component")?;
            ensure!(
                number <= 9999,
                "Version components must be between 0 and 9999"
            );
            parts[index] = number;
        }
        Ok(Self(parts))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{}.{}.{}",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

impl Version {
    /// Display form for macOS notices: `2.2.2.0` reads as `2.2.2`. Display only; parsing and
    /// ordering still use all four components.
    pub fn display_short(&self) -> String {
        if self.0[3] == 0 {
            format!("{}.{}.{}", self.0[0], self.0[1], self.0[2])
        } else {
            self.to_string()
        }
    }
}

pub fn installer_name(version: Version) -> String {
    format!("Regressify-Setup-{version}-x64.exe")
}

/// Platform decision for the update feed, injected so each path is testable on any host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdatePlatform {
    /// Full download, verification, and installer launch.
    WindowsX64,
    /// Notification only: nothing is downloaded or installed.
    MacOs,
    Other,
}

impl UpdatePlatform {
    pub const fn current() -> Self {
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Self::WindowsX64
        } else if cfg!(all(
            target_os = "macos",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )) {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

#[derive(Clone, Debug)]
pub struct UpdateFeed {
    manifest_url: Url,
    platform: UpdatePlatform,
}

/// A newer release announced on a notify-only platform. Carries no installer, hash, or
/// payload data, so no download path can be built from it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateNotice {
    pub version: Version,
    tag: String,
}

impl UpdateNotice {
    /// Release page built only from the validated tag.
    pub fn release_url(&self) -> String {
        format!("{RELEASE_TAG_PREFIX}{}", self.tag)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum NoticeCheck {
    /// The feed publishes no release (HTTP 404).
    Current,
    /// The feed's release is not newer than the checking window. It is still saved: an older
    /// peer window must learn about it without waiting out another eight-hour interval.
    Older(UpdateNotice),
    Available(UpdateNotice),
}

#[derive(Debug, Eq, PartialEq)]
pub enum NoticeOutcome {
    /// The shared eight-hour interval has not elapsed.
    NotDue,
    Current,
    /// Newer, but the user dismissed this or a later version. The release is kept so the
    /// dialog can still offer its install controls.
    Dismissed(UpdateNotice),
    Available(UpdateNotice),
}

/// Result of one scheduled notice check, plus whether a feed request was actually sent.
#[derive(Debug)]
pub struct NoticeAttempt {
    /// True once the feed request was dispatched, even if it then failed. Callers advance their
    /// in-memory throttle only on this, never on cached or not-due polls.
    pub dispatched: bool,
    pub outcome: Result<NoticeOutcome>,
}

/// What "Update now" will download: built from a fresh manifest, never stored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallPlan {
    pub version: Version,
    url: String,
    meta: macos_install::ArchiveMeta,
}

/// Progress of an in-app macOS install, shared between the worker thread and the UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallState {
    Downloading {
        received: u64,
        total: u64,
    },
    Verifying,
    Installing,
    /// The new app is in place; relaunching is the only step left.
    Restart,
    Failed(String),
}

impl InstallState {
    pub fn message(&self) -> String {
        match self {
            Self::Downloading { received, total } => {
                format!(
                    "Downloading {}%",
                    (received * 100).checked_div(*total).unwrap_or(0).min(100)
                )
            }
            Self::Verifying => "Verifying".into(),
            Self::Installing => "Installing".into(),
            Self::Restart => "Installed. Restart Regressify to finish updating.".into(),
            Self::Failed(reason) => format!("Failed: {reason}"),
        }
    }

    pub fn in_progress(&self) -> bool {
        matches!(
            self,
            Self::Downloading { .. } | Self::Verifying | Self::Installing
        )
    }
}

#[derive(Clone, Debug)]
pub struct InstallHandle(std::sync::Arc<std::sync::Mutex<InstallState>>);

impl InstallHandle {
    pub fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            InstallState::Downloading {
                received: 0,
                total: 1,
            },
        )))
    }

    pub fn get(&self) -> InstallState {
        self.0
            .lock()
            .map_or_else(|error| error.into_inner().clone(), |state| state.clone())
    }

    /// A finished install waiting for its restart, for GUI tests.
    #[cfg(all(test, feature = "gui-tests"))]
    pub fn restarted() -> Self {
        let handle = Self::new();
        handle.set(InstallState::Restart);
        handle
    }

    fn set(&self, state: InstallState) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = state;
    }
}

impl Default for InstallHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs the install and publishes progress; the final state is always `Restart` or `Failed`.
pub fn perform_install(
    bundle: &Path,
    cache: &Path,
    plan: &InstallPlan,
    deps: &macos_install::Deps,
    handle: &InstallHandle,
) -> Result<()> {
    use macos_install::Phase;
    let result = macos_install::install(
        bundle,
        cache,
        &plan.url,
        &plan.meta,
        &plan.version.to_string(),
        deps,
        &mut |phase| {
            handle.set(match phase {
                Phase::Downloading { received, total } => {
                    InstallState::Downloading { received, total }
                }
                Phase::Verifying => InstallState::Verifying,
                Phase::Installing => InstallState::Installing,
            })
        },
    );
    match &result {
        Ok(()) => handle.set(InstallState::Restart),
        Err(error) => handle.set(InstallState::Failed(failure_reason(error))),
    }
    result
}

/// UI text for a failed install. Only claims the app is unchanged when that is true: a failed
/// swap rollback already says where the previous app is kept.
fn failure_reason(error: &anyhow::Error) -> String {
    if error
        .downcast_ref::<macos_install::RecoveryNeeded>()
        .is_some()
    {
        format!("{error:#}")
    } else {
        format!("{error:#}. The current app was not changed.")
    }
}

/// Everything after the lock is held: clean up a killed install, refuse to go backwards or
/// repeat, install. The newer-than check uses the version stamped in the installed bundle as
/// well as the running version, because another window (or the terminal installer) may have
/// installed since this process started; the stamp swaps in with the app, so it cannot be lost.
fn install_locked(
    lock: &macos_install::InstallLock,
    bundle: &Path,
    cache: &Path,
    current: Version,
    plan: &InstallPlan,
    deps: &macos_install::Deps,
    handle: &InstallHandle,
) -> Result<()> {
    macos_install::recover_interrupted(lock, bundle)?;
    let installed = macos_install::installed_version(bundle)
        .and_then(|version| version.parse::<Version>().ok())
        .unwrap_or(current);
    ensure!(
        plan.version > current.max(installed),
        "Regressify {} or newer is already installed; restart Regressify",
        current.max(installed).display_short()
    );
    perform_install(bundle, cache, plan, deps, handle)
}

/// The whole click-to-installed flow with the real downloader and `codesign`. Blocking:
/// call it off the UI thread. Holds the install lock so two windows cannot race.
pub fn install_macos_update(
    feed: &UpdateFeed,
    bundle: &Path,
    cache: &Path,
    current: &str,
    handle: &InstallHandle,
) -> Result<()> {
    let run = || -> Result<()> {
        let current: Version = current.parse()?;
        let lock = macos_install::InstallLock::acquire(bundle)?;
        let plan = feed.macos_install_plan(current)?;
        let deps = macos_install::Deps {
            open: &open_archive,
            sign: &|path| run_codesign(&["--force", "--deep", "--sign", "-"], path),
            verify: &|path| run_codesign(&["--verify", "--deep", "--strict"], path),
            ops: &macos_install::RealOps,
        };
        install_locked(&lock, bundle, cache, current, &plan, &deps, handle)
    };
    let result = run();
    if let Err(error) = &result {
        // `perform_install` already published its own failure; this covers earlier steps.
        if !matches!(handle.get(), InstallState::Failed(_)) {
            handle.set(InstallState::Failed(failure_reason(error)));
        }
    }
    result
}

/// HTTPS-only archive download. Redirects follow the same GitHub-only allowlist as the
/// Windows installer; size and hash are enforced by the caller on the received bytes.
fn open_archive(url: &str) -> Result<Box<dyn Read>> {
    let response = Client::builder()
        .https_only(true)
        .redirect(installer_redirect_policy())
        .connect_timeout(Duration::from_secs(10))
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(concat!("Regressify-Desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Cannot create the update client")?
        .get(url)
        .send()
        .context("Cannot reach the update download")?;
    ensure!(
        response.status().is_success(),
        "Update download returned HTTP {}",
        response.status()
    );
    Ok(Box::new(response))
}

/// Absolute `codesign` path and an argument vector: no shell, no PATH lookup.
fn run_codesign(args: &[&str], path: &Path) -> Result<()> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(args)
        .arg(path)
        .output()
        .context("Cannot run /usr/bin/codesign")?;
    ensure!(
        output.status.success(),
        "codesign failed: {}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(300)
            .collect::<String>()
    );
    Ok(())
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct NoticeState {
    #[serde(default)]
    last_check_at: Option<u64>,
    #[serde(default)]
    dismissed_version: Option<String>,
    /// Last release announced by any instance, so a window whose check is not due still
    /// learns about it. Revalidated through `parse_notice_manifest` on every read.
    #[serde(default)]
    available_version: Option<String>,
    #[serde(default)]
    available_tag: Option<String>,
    /// Request ordering: each network check takes the next `started_seq` before it sends, and
    /// only a response newer than `applied_seq` may overwrite the saved release, so a slower,
    /// earlier request cannot replace a newer one. Dismissals are always merged.
    #[serde(default)]
    started_seq: u64,
    #[serde(default)]
    applied_seq: u64,
}

/// Whether a dismissal of `dismissed` hides `version`: that version and every older one.
pub fn dismissal_hides(dismissed: Option<Version>, version: Version) -> bool {
    dismissed.is_some_and(|dismissed| version <= dismissed)
}

/// Prefix of the dialog message left by a failed update check.
pub const CHECK_FAILED_PREFIX: &str = "Update check failed: ";

/// Dialog message after a successful notice result. `fresh` is the status to show
/// (empty for none). Only a cached poll (`dispatched == false`) keeps an
/// earlier check failure, since it was not a check; any other old message (up to date,
/// dismissed) would be stale next to the new result and is replaced.
pub fn notice_message_after_success(dispatched: bool, previous: &str, fresh: &str) -> String {
    if !dispatched && previous.starts_with(CHECK_FAILED_PREFIX) {
        previous.to_owned()
    } else {
        fresh.to_owned()
    }
}

/// Serializes read-modify-write cycles on `notice.json` across instances. Atomic file
/// replacement alone still loses an update when two writers start from the same snapshot.
/// Held only for the short load/merge/save, never across the network request. `None` when
/// the lock file cannot be opened; the state cannot be saved then either.
fn lock_notice_state(cache: &Path) -> Option<fs::File> {
    fs::create_dir_all(cache).ok()?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache.join("notice.lock"))
        .ok()?;
    lock.lock().ok()?;
    Some(lock)
}

impl NoticeState {
    fn load(cache: &Path) -> Result<Self> {
        match fs::File::open(cache.join(NOTICE_STATE_NAME)) {
            Ok(file) => serde_json::from_reader(file.take(MAX_MANIFEST_SIZE))
                .context("Cannot read saved update notice state"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).context("Cannot open saved update notice state"),
        }
    }

    fn save(&self, cache: &Path) -> Result<()> {
        fs::create_dir_all(cache)?;
        let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
        serde_json::to_writer(&mut temporary, self)?;
        temporary
            .persist(cache.join(NOTICE_STATE_NAME))
            .map_err(|error| error.error)
            .context("Cannot save update notice state")?;
        Ok(())
    }

    fn dismisses(&self, version: Version) -> bool {
        dismissal_hides(
            self.dismissed_version
                .as_deref()
                .and_then(|saved| saved.parse::<Version>().ok()),
            version,
        )
    }

    fn store_available(&mut self, notice: Option<&UpdateNotice>) {
        self.available_version = notice.map(|notice| notice.version.to_string());
        self.available_tag = notice.map(|notice| notice.tag.clone());
    }

    /// Outcome implied by what a peer already saved, without any request.
    fn reconcile(&self, feed: &UpdateFeed, current: Version) -> NoticeOutcome {
        let stored = self
            .available_version
            .as_deref()
            .zip(self.available_tag.as_deref())
            .and_then(|(version, tag)| {
                feed.parse_notice_manifest(&format!("version={version}\ntag={tag}\n"))
                    .ok()
            });
        match stored {
            // A peer's successful check already found no release: that is `Current`, so this
            // window clears an obsolete banner. Without any applied success it is just unknown.
            None if self.applied_seq > 0 => NoticeOutcome::Current,
            None => NoticeOutcome::NotDue,
            Some(notice) if notice.version <= current => NoticeOutcome::Current,
            Some(notice) if self.dismisses(notice.version) => NoticeOutcome::Dismissed(notice),
            Some(notice) => NoticeOutcome::Available(notice),
        }
    }
}

/// Version encoded in a release tag such as `v2.2.2`; rejects paths and odd component shapes.
fn tag_version(tag: &str) -> Option<Version> {
    tag.strip_prefix('v')
        .filter(|raw| {
            !raw.contains('/')
                && matches!(raw.split('.').count(), 3 | 4)
                && raw.split('.').all(|part| (1..=4).contains(&part.len()))
        })
        .and_then(|raw| raw.parse::<Version>().ok())
}

/// Only the fields a notify-only platform needs; installer metadata is deliberately ignored.
#[derive(Deserialize)]
struct NoticeFields {
    version: String,
    tag: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    installer: Url,
    sha256: String,
    size: u64,
    payloads: Box<[PayloadArchive; 2]>,
}

#[derive(Clone, Debug, PartialEq)]
struct PayloadArchive {
    size: u64,
    sha256: String,
}

impl FromStr for PayloadArchive {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (size, hash) = value
            .split_once(':')
            .context("Missing payload size and SHA-256")?;
        ensure!(
            size.bytes().all(|byte| byte.is_ascii_digit()),
            "Invalid payload size"
        );
        let size: u64 = size.parse().context("Invalid payload size")?;
        ensure!(
            size > 0 && size <= MAX_INSTALLER_SIZE,
            "Payload size must be 1 byte to 1 GiB"
        );
        ensure!(
            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid payload SHA-256"
        );
        Ok(Self {
            size,
            sha256: hash.to_ascii_lowercase(),
        })
    }
}

#[derive(Debug, PartialEq)]
pub enum UpdateCheck {
    Current,
    Unpublished,
    Available(Release),
}

#[derive(Deserialize, Serialize)]
struct ManifestFields {
    version: String,
    installer: String,
    sha256: String,
    size: serde_json::Value,
    #[serde(
        default,
        rename = "archive.regressify-desktop-x86_64-pc-windows-msvc.zip"
    )]
    desktop_archive: String,
    #[serde(default, rename = "archive.regressify-cli-x86_64-pc-windows-msvc.zip")]
    cli_archive: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

impl From<&Release> for ManifestFields {
    fn from(release: &Release) -> Self {
        Self {
            version: release.version.to_string(),
            installer: release.installer.to_string(),
            sha256: release.sha256.clone(),
            size: release.size.into(),
            desktop_archive: format!(
                "{}:{}",
                release.payloads[0].size, release.payloads[0].sha256
            ),
            cli_archive: format!(
                "{}:{}",
                release.payloads[1].size, release.payloads[1].sha256
            ),
            tag: None,
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
struct UpdateState {
    release: Option<ManifestFields>,
    hash_failures: u8,
    discard_payload: bool,
    skipped_version: Option<String>,
    #[serde(default)]
    next_retry_at: Option<u64>,
    #[serde(default)]
    last_check_at: Option<u64>,
    #[serde(default)]
    preparation_retry_at: Option<u64>,
}

impl UpdateState {
    fn load(cache: &Path) -> Result<Self> {
        match fs::File::open(cache.join("state.json")) {
            Ok(file) => serde_json::from_reader(file.take(MAX_MANIFEST_SIZE * 2))
                .context("Cannot read saved update state"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).context("Cannot open saved update state"),
        }
    }

    fn save(&self, cache: &Path) -> Result<()> {
        let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
        serde_json::to_writer(&mut temporary, self)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(cache.join("state.json"))
            .map_err(|error| error.error)
            .context("Cannot save update state")?;
        #[cfg(unix)]
        fs::File::open(cache)?.sync_all()?;
        Ok(())
    }

    fn skips(&self, version: Version) -> Result<bool> {
        Ok(self
            .skipped_version
            .as_deref()
            .map(str::parse::<Version>)
            .transpose()?
            .is_some_and(|skipped| version <= skipped))
    }

    fn select_release(&mut self, release: &Release, cache: &Path) -> Result<()> {
        let previous = self
            .release
            .as_ref()
            .map(|saved| saved.version.parse::<Version>())
            .transpose()?;
        ensure!(
            previous.is_none_or(|version| version <= release.version),
            "A newer update has replaced this release"
        );
        if previous != Some(release.version) {
            self.hash_failures = 0;
            self.discard_payload = false;
            self.next_retry_at = None;
            self.preparation_retry_at = None;
        }
        self.release = Some(ManifestFields::from(release));
        self.save(cache)?;
        discard_superseded_installers(cache, release.version)
    }

    fn ensure_retry_due(&self, version: Version, now: u64) -> Result<()> {
        if self.skips(version)? {
            return Err(SkippedUpdate { version }.into());
        }
        if let Some(retry_at) = self.next_retry_at.filter(|retry_at| *retry_at > now) {
            return Err(RetryScheduled { version, retry_at }.into());
        }
        Ok(())
    }

    fn reset_retries(&mut self) {
        self.hash_failures = 0;
        self.skipped_version = None;
        self.next_retry_at = None;
        self.preparation_retry_at = None;
    }
}

pub enum SharedUpdateCheck {
    Busy,
    Unchanged,
    Checked(UpdateCheck, UpdateLease),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckMode {
    Scheduled,
    Startup,
    Manual,
}

pub struct UpdateLease {
    _lock: fs::File,
    cache: PathBuf,
}

impl UpdateLease {
    fn acquire(cache: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(cache).context("Cannot create the update cache")?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(cache.join("coordinator.lock"))?;
        match lock.try_lock() {
            Ok(()) => Ok(Some(Self {
                _lock: lock,
                cache: cache.to_path_buf(),
            })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(error) => Err(error).context("Cannot coordinate desktop updates"),
        }
    }

    pub fn prepare(self, release: Release, control: DownloadControl) -> Result<PreparedUpdate> {
        self.prepare_with(&control, || {
            PreparedUpdate::download_with_control(release, &self.cache, control.clone())
        })
    }

    fn prepare_with(
        &self,
        control: &DownloadControl,
        prepare: impl FnOnce() -> Result<PreparedUpdate>,
    ) -> Result<PreparedUpdate> {
        // Keep the outer lease across the check/download boundary and every state write.
        let result = prepare();
        let _lock = lock_cache(&self.cache)?;
        let mut state = UpdateState::load(&self.cache)?;
        state.preparation_retry_at = if result.as_ref().is_err_and(|error| {
            !error.is::<RetryScheduled>()
                && !error.is::<SkippedUpdate>()
                && *control.cancelled.borrow() != DownloadCancellation::Handoff
        }) {
            Some(unix_timestamp().saturating_add(CHECK_INTERVAL.as_secs()))
        } else {
            None
        };
        state.save(&self.cache)?;
        result
    }
}

fn lock_cache(cache: &Path) -> Result<fs::File> {
    fs::create_dir_all(cache).context("Cannot create the update cache")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache.join("update.lock"))?;
    lock.try_lock()
        .map_err(|error| anyhow::anyhow!("Update cache is in use or cannot be locked: {error}"))?;
    Ok(lock)
}

#[derive(Debug)]
struct ChecksumMismatch;

impl fmt::Display for ChecksumMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("Installer SHA-256 does not match the manifest; update was not installed")
    }
}

impl std::error::Error for ChecksumMismatch {}

#[derive(Debug)]
pub struct SkippedUpdate {
    pub version: Version,
}

impl fmt::Display for SkippedUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Version {} failed SHA-256 verification after the initial attempt and {MAX_HASH_RETRIES} retries; waiting for a newer version",
            self.version
        )
    }
}

impl std::error::Error for SkippedUpdate {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryScheduled {
    pub version: Version,
    pub retry_at: u64,
}

impl RetryScheduled {
    pub fn delay(&self) -> Duration {
        Duration::from_secs(self.retry_at.saturating_sub(unix_timestamp()))
    }
}

impl fmt::Display for RetryScheduled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Version {} failed SHA-256 verification; retrying in {} minutes",
            self.version,
            self.delay().as_secs().div_ceil(60)
        )
    }
}

impl std::error::Error for RetryScheduled {}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl UpdateFeed {
    #[cfg(test)]
    pub(crate) fn offline_for_test(origin: &str) -> Result<Self> {
        let mut feed = Self::new(origin)?;
        feed.manifest_url = "http://127.0.0.1/latest.txt".parse()?;
        Ok(feed)
    }

    /// Feed served by the website at `<origin>/downloads/latest.txt`, for an `https://host` origin.
    pub fn new(origin: &str) -> Result<Self> {
        Self::for_platform(origin, UpdatePlatform::WindowsX64)
    }

    fn for_platform(origin: &str, platform: UpdatePlatform) -> Result<Self> {
        let host = origin.strip_prefix("https://").unwrap_or_default();
        ensure!(
            !host.is_empty()
                && host.len() <= 253
                && !host.starts_with(['.', '-'])
                && !host.ends_with(['.', '-'])
                && !host.contains("..")
                && host.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'-'
                    || byte == b'.'),
            "Update origin must be an https://host origin without a path"
        );
        let manifest_url: Url = format!("{origin}/{DOWNLOADS_PATH}/latest.txt").parse()?;
        ensure!(
            manifest_url.host_str() == Some(host),
            "Update origin must be an https://host origin without a path"
        );
        Ok(Self {
            manifest_url,
            platform,
        })
    }

    /// Feed for `platform` and a build-time origin; `None` on unsupported platforms.
    pub fn for_origin(origin: Option<&str>, platform: UpdatePlatform) -> Option<Self> {
        match platform {
            UpdatePlatform::WindowsX64 | UpdatePlatform::MacOs => {
                Self::for_platform(origin?, platform).ok()
            }
            UpdatePlatform::Other => None,
        }
    }

    pub fn configured() -> Option<Self> {
        Self::for_origin(
            option_env!("REGRESSIFY_UPDATE_ORIGIN"),
            UpdatePlatform::current(),
        )
    }

    pub fn platform(&self) -> UpdatePlatform {
        self.platform
    }

    /// Command shown to macOS users; the host comes from the feed origin.
    pub fn install_command(&self) -> String {
        format!(
            "curl -fsSL https://{}/install.sh | sh",
            self.manifest_url.host_str().unwrap_or_default()
        )
    }

    /// Notify-only manifest parsing: validates `version` and `tag` and ignores every
    /// installer field, so a Windows-shaped manifest never fails a macOS check.
    pub fn parse_notice_manifest(&self, text: &str) -> Result<UpdateNotice> {
        ensure!(
            text.len() as u64 <= MAX_MANIFEST_SIZE,
            "Update manifest is too large"
        );
        let text = text.trim_start_matches('\u{feff}').trim();
        let fields: NoticeFields = if text.starts_with('{') {
            serde_json::from_str(text).context("Invalid JSON update manifest")?
        } else {
            let mut fields = BTreeMap::new();
            for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                let (key, value) = line
                    .split_once('=')
                    .context("Invalid update manifest line")?;
                ensure!(
                    fields.insert(key.trim(), value.trim()).is_none(),
                    "Duplicate update manifest field: {key}"
                );
            }
            serde_json::from_value(serde_json::to_value(fields)?)
                .context("Missing required update manifest fields")?
        };
        let version: Version = fields.version.parse()?;
        ensure!(
            tag_version(&fields.tag) == Some(version),
            "Mismatched release tag"
        );
        Ok(UpdateNotice {
            version,
            tag: fields.tag,
        })
    }

    pub fn check_notice(&self, current: Version) -> Result<NoticeCheck> {
        ensure!(
            self.platform == UpdatePlatform::MacOs,
            "Update notices are only used on notify-only platforms"
        );
        let response = client(Duration::from_secs(30))?
            .get(self.manifest_url.clone())
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .context("Cannot reach the update service")?;
        self.notice_response(response.status(), response, current)
    }

    fn notice_response(
        &self,
        status: StatusCode,
        response: impl Read,
        current: Version,
    ) -> Result<NoticeCheck> {
        if status == StatusCode::NOT_FOUND {
            return Ok(NoticeCheck::Current);
        }
        ensure!(status.is_success(), "Update service returned HTTP {status}");
        let mut text = String::new();
        response
            .take(MAX_MANIFEST_SIZE + 1)
            .read_to_string(&mut text)?;
        let notice = self.parse_notice_manifest(&text)?;
        Ok(if notice.version > current {
            NoticeCheck::Available(notice)
        } else {
            NoticeCheck::Older(notice)
        })
    }

    /// Scheduled notice check. Startup bypasses only the eight-hour deadline. The timestamp is
    /// refreshed after every attempt, failures included, so a broken feed is retried at the
    /// normal interval rather than on every poll. When the check is not due (or
    /// `network_allowed` is false, the caller's in-memory throttle for an unwritable cache)
    /// the saved notice from a peer is reconciled instead. No coordinator lock: nothing is
    /// downloaded, so a duplicate request from a peer instance is harmless.
    pub fn check_notice_shared(
        &self,
        current: Version,
        cache: &Path,
        startup: bool,
        network_allowed: bool,
    ) -> NoticeAttempt {
        self.check_notice_attempt(
            cache,
            current,
            startup,
            network_allowed,
            unix_timestamp(),
            || self.check_notice(current),
        )
    }

    fn check_notice_attempt(
        &self,
        cache: &Path,
        current: Version,
        startup: bool,
        network_allowed: bool,
        now: u64,
        check: impl FnOnce() -> Result<NoticeCheck>,
    ) -> NoticeAttempt {
        let dispatched = std::cell::Cell::new(false);
        let outcome = self.check_notice_with(cache, current, startup, network_allowed, now, || {
            dispatched.set(true);
            check()
        });
        NoticeAttempt {
            dispatched: dispatched.get(),
            outcome,
        }
    }

    fn check_notice_with(
        &self,
        cache: &Path,
        current: Version,
        startup: bool,
        network_allowed: bool,
        now: u64,
        check: impl FnOnce() -> Result<NoticeCheck>,
    ) -> Result<NoticeOutcome> {
        let snapshot = {
            let _lock = lock_notice_state(cache);
            NoticeState::load(cache).unwrap_or_default()
        };
        let due = network_allowed
            && (startup
                || snapshot.last_check_at.is_none_or(|checked| {
                    now < checked || now.saturating_sub(checked) >= CHECK_INTERVAL.as_secs()
                }));
        if !due {
            return Ok(snapshot.reconcile(self, current));
        }
        // Take a ticket before sending; a later request gets a higher one.
        let ticket = {
            let _lock = lock_notice_state(cache);
            let mut state = NoticeState::load(cache).unwrap_or_default();
            // Never at or below an applied ticket, even if an earlier ticket save was lost.
            state.started_seq = state.started_seq.max(state.applied_seq) + 1;
            // An unsaved ticket can be handed out again to a peer, so its response must never
            // advance `applied_seq`: it could otherwise discard the peer's newer response.
            let saved = state.save(cache).is_ok();
            (state.started_seq, saved)
        };
        let (ticket, ticket_saved) = ticket;
        let result = check();
        // Reload under the lock: a dismissal or a peer's check may have landed while the
        // request was pending, and saving the earlier snapshot would overwrite it.
        let _lock = lock_notice_state(cache);
        let mut state = NoticeState::load(cache).unwrap_or_default();
        if ticket <= state.applied_seq {
            // Superseded by a newer request whose release already landed: keep its release and
            // timestamp (the dismissal in `state` is already current) and report that.
            return match result {
                Ok(_) => Ok(match state.reconcile(self, current) {
                    NoticeOutcome::NotDue => NoticeOutcome::Current,
                    outcome => outcome,
                }),
                Err(error) => Err(error),
            };
        }
        // Keep `applied_seq` from ever passing `started_seq`, even if a peer reset the file.
        state.started_seq = state.started_seq.max(ticket);
        state.last_check_at = Some(now);
        match &result {
            Ok(NoticeCheck::Current) => {
                if ticket_saved {
                    state.applied_seq = ticket;
                }
                state.store_available(None);
            }
            Ok(NoticeCheck::Available(notice) | NoticeCheck::Older(notice)) => {
                if ticket_saved {
                    state.applied_seq = ticket;
                }
                state.store_available(Some(notice));
            }
            // A failure only throttles (`last_check_at`). It carries no release, so it must not
            // supersede an older request that is still in flight and may yet succeed.
            Err(_) => {}
        }
        // The timestamp is only a throttle: a failed save must not hide the check outcome.
        if let Err(error) = state.save(cache) {
            crate::diagnostics::startup::warning(
                crate::diagnostics::record::SafeValue::Desktop,
                &error,
            );
        }
        Ok(match result? {
            NoticeCheck::Current | NoticeCheck::Older(_) => NoticeOutcome::Current,
            NoticeCheck::Available(notice) if state.dismisses(notice.version) => {
                NoticeOutcome::Dismissed(notice)
            }
            NoticeCheck::Available(notice) => NoticeOutcome::Available(notice),
        })
    }

    /// Archive download plan for this Mac from a fresh manifest, fetched only after the user
    /// clicks "Update now". Same host rules as the notice check; no other host is contacted.
    pub fn macos_install_plan(&self, current: Version) -> Result<InstallPlan> {
        ensure!(
            self.platform == UpdatePlatform::MacOs,
            "Update install is only available on macOS"
        );
        let response = client(Duration::from_secs(30))?
            .get(self.manifest_url.clone())
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .context("Cannot reach the update service")?;
        ensure!(
            response.status().is_success(),
            "Update service returned HTTP {}",
            response.status()
        );
        let mut text = String::new();
        response
            .take(MAX_MANIFEST_SIZE + 1)
            .read_to_string(&mut text)?;
        self.parse_install_plan(&text, current, macos_install::current_archive_name())
    }

    /// Pure half of `macos_install_plan`: the manifest must announce a newer release and list
    /// the archive for this architecture. The URL is built only from the validated tag.
    fn parse_install_plan(
        &self,
        text: &str,
        current: Version,
        archive: Option<&str>,
    ) -> Result<InstallPlan> {
        let notice = self.parse_notice_manifest(text)?;
        ensure!(notice.version > current, "No newer release is available");
        let archive = archive.context("This Mac's architecture has no desktop build")?;
        let meta = macos_install::parse_archive_meta(text, archive)?;
        Ok(InstallPlan {
            version: notice.version,
            url: format!("{RELEASE_DOWNLOAD_PREFIX}{}/{archive}", notice.tag),
            meta,
        })
    }

    /// Hides this and every older notice; a newer release shows again.
    pub fn dismiss_notice(version: Version, cache: &Path) -> Result<()> {
        let _lock = lock_notice_state(cache);
        let mut state = NoticeState::load(cache).unwrap_or_default();
        if !state.dismisses(version) {
            state.dismissed_version = Some(version.to_string());
        }
        state.save(cache)
    }
    pub fn parse_manifest(&self, text: &str) -> Result<Release> {
        ensure!(
            text.len() as u64 <= MAX_MANIFEST_SIZE,
            "Update manifest is too large"
        );
        let text = text.trim_start_matches('\u{feff}').trim();
        let fields: ManifestFields = if text.starts_with('{') {
            serde_json::from_str(text).context("Invalid JSON update manifest")?
        } else {
            let mut fields = BTreeMap::new();
            for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                let (key, value) = line
                    .split_once('=')
                    .context("Invalid update manifest line")?;
                ensure!(
                    fields.insert(key.trim(), value.trim()).is_none(),
                    "Duplicate update manifest field: {key}"
                );
            }
            serde_json::from_value(serde_json::to_value(fields)?)
                .context("Missing required update manifest fields")?
        };
        let version: Version = fields.version.parse()?;
        let installer: Url = fields.installer.parse().context("Invalid installer URL")?;
        let suffix = format!("/{}", installer_name(version));
        let valid_tag = fields
            .installer
            .strip_prefix(RELEASE_DOWNLOAD_PREFIX)
            .and_then(|rest| rest.strip_suffix(&suffix))
            .and_then(tag_version)
            .is_some_and(|tag_version| tag_version == version);
        ensure!(
            valid_tag && installer.as_str() == fields.installer,
            "Installer must use this repository's versioned HTTPS GitHub Release URL"
        );
        if let Some(tag) = &fields.tag {
            ensure!(
                fields.installer == format!("{RELEASE_DOWNLOAD_PREFIX}{tag}{suffix}"),
                "Mismatched release tag"
            );
        }
        ensure!(
            fields.sha256.len() == 64 && fields.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Installer SHA-256 must contain 64 hexadecimal characters"
        );
        let size = fields.size.as_u64().or_else(|| {
            fields
                .size
                .as_str()
                .and_then(|value| value.parse::<u64>().ok())
        });
        let size = size.context("Invalid installer size")?;
        ensure!(
            size > 0 && size <= MAX_INSTALLER_SIZE,
            "Installer size must be between 1 byte and 1 GiB"
        );
        Ok(Release {
            version,
            installer,
            sha256: fields.sha256.to_ascii_lowercase(),
            size,
            payloads: Box::new([fields.desktop_archive.parse()?, fields.cli_archive.parse()?]),
        })
    }

    pub fn check(&self, current: Version) -> Result<UpdateCheck> {
        let response = client(Duration::from_secs(30))?
            .get(self.manifest_url.clone())
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .context("Cannot reach the update service")?;
        self.check_response(response.status(), response, current)
    }

    pub fn check_with_cache(&self, current: Version, cache: &Path) -> Result<UpdateCheck> {
        self.reconcile_check(current, cache, self.check(current))
    }

    pub fn check_manually(&self, current: Version, cache: &Path) -> Result<UpdateCheck> {
        {
            let _lock = lock_cache(cache)?;
            let mut state = UpdateState::load(cache)?;
            state.reset_retries();
            state.save(cache)?;
        }
        self.check_with_cache(current, cache)
    }

    pub fn check_shared(
        &self,
        current: Version,
        cache: &Path,
        manual: bool,
        prepared: Option<&Release>,
    ) -> Result<SharedUpdateCheck> {
        let mode = if manual {
            CheckMode::Manual
        } else {
            CheckMode::Scheduled
        };
        self.check_shared_with(current, cache, mode, prepared, unix_timestamp(), || {
            self.check(current)
        })
    }

    pub fn check_on_startup(
        &self,
        current: Version,
        cache: &Path,
        prepared: Option<&Release>,
    ) -> Result<SharedUpdateCheck> {
        self.check_shared_with(
            current,
            cache,
            CheckMode::Startup,
            prepared,
            unix_timestamp(),
            || self.check(current),
        )
    }

    fn check_shared_with(
        &self,
        current: Version,
        cache: &Path,
        mode: CheckMode,
        prepared: Option<&Release>,
        now: u64,
        check: impl FnOnce() -> Result<UpdateCheck>,
    ) -> Result<SharedUpdateCheck> {
        let Some(lease) = UpdateLease::acquire(cache)? else {
            return Ok(SharedUpdateCheck::Busy);
        };
        let mut state = {
            let _lock = lock_cache(cache)?;
            UpdateState::load(cache)?
        };
        let manual = mode == CheckMode::Manual;
        // Startup bypasses only the feed deadline, not preparation or checksum retries.
        let due = mode != CheckMode::Scheduled
            || state.last_check_at.is_none_or(|checked| {
                now < checked || now.saturating_sub(checked) >= CHECK_INTERVAL.as_secs()
            });
        let result = if due {
            if manual {
                let _lock = lock_cache(cache)?;
                state.reset_retries();
                state.save(cache)?;
            }
            let result = self.reconcile_check(current, cache, check());
            let _lock = lock_cache(cache)?;
            state = UpdateState::load(cache)?;
            state.last_check_at = Some(now);
            state.save(cache)?;
            result?
        } else {
            let Some(saved) = &state.release else {
                return Ok(SharedUpdateCheck::Unchanged);
            };
            if saved.version.parse::<Version>()? <= current {
                return Ok(SharedUpdateCheck::Unchanged);
            }
            let release = self.parse_manifest(&serde_json::to_string(saved)?)?;
            if release.version <= current || prepared == Some(&release) {
                return Ok(SharedUpdateCheck::Unchanged);
            }
            state.ensure_retry_due(release.version, now)?;
            UpdateCheck::Available(release)
        };
        if let UpdateCheck::Available(release) = &result
            && !manual
            && (prepared == Some(release)
                || state.preparation_retry_at.is_some_and(|retry| now < retry))
        {
            return Ok(SharedUpdateCheck::Unchanged);
        }
        Ok(SharedUpdateCheck::Checked(result, lease))
    }

    fn reconcile_check(
        &self,
        current: Version,
        cache: &Path,
        result: Result<UpdateCheck>,
    ) -> Result<UpdateCheck> {
        let _lock = lock_cache(cache)?;
        let mut state = UpdateState::load(cache)?;
        // An installed bridge can leave metadata from the retired feed behind.
        // Discard only already-installed versions before applying the new URL contract.
        if state.release.as_ref().is_some_and(|saved| {
            saved
                .version
                .parse::<Version>()
                .is_ok_and(|version| version <= current)
        }) {
            state.release = None;
            state.save(cache)?;
        }
        let cached = state
            .release
            .as_ref()
            .map(|fields| self.parse_manifest(&serde_json::to_string(fields)?))
            .transpose()?
            .filter(|release| release.version > current);
        let result = match (result, cached) {
            (Ok(UpdateCheck::Available(latest)), Some(cached)) => {
                UpdateCheck::Available(if latest.version >= cached.version {
                    latest
                } else {
                    cached
                })
            }
            (_, Some(cached)) => UpdateCheck::Available(cached),
            (result, None) => result?,
        };
        if let UpdateCheck::Available(release) = &result {
            state.select_release(release, cache)?;
            state.ensure_retry_due(release.version, unix_timestamp())?;
        }
        Ok(result)
    }

    fn check_response(
        &self,
        status: StatusCode,
        response: impl Read,
        current: Version,
    ) -> Result<UpdateCheck> {
        if status == StatusCode::NOT_FOUND {
            return Ok(UpdateCheck::Unpublished);
        }
        ensure!(status.is_success(), "Update service returned HTTP {status}");
        let mut text = String::new();
        response
            .take(MAX_MANIFEST_SIZE + 1)
            .read_to_string(&mut text)?;
        let release = self.parse_manifest(&text)?;
        Ok(if release.version > current {
            UpdateCheck::Available(release)
        } else {
            UpdateCheck::Current
        })
    }
}

fn client(timeout: Duration) -> Result<Client> {
    Client::builder()
        .https_only(true)
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .user_agent(concat!("Regressify-Desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Cannot create the update client")
}

/// GitHub Release asset downloads redirect from `github.com` to `*.githubusercontent.com`.
fn is_allowed_installer_redirect(url: &Url, hops: usize) -> std::result::Result<(), &'static str> {
    if hops >= MAX_REDIRECT_HOPS {
        return Err("Installer download was redirected too many times; update was not installed");
    }
    let valid_host = url.host_str().is_some_and(|host| {
        host == "github.com"
            || host
                .strip_suffix(".githubusercontent.com")
                .is_some_and(|sub| {
                    !sub.is_empty()
                        && !sub.starts_with(['.', '-'])
                        && !sub.ends_with(['.', '-'])
                        && !sub.contains("..")
                        && sub.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || byte == b'-'
                                || byte == b'.'
                        })
                })
    });
    if url.scheme() != "https"
        || !valid_host
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Installer download was redirected to an untrusted URL; update was not installed",
        );
    }
    Ok(())
}

fn installer_redirect_policy() -> Policy {
    Policy::custom(|attempt| {
        match is_allowed_installer_redirect(attempt.url(), attempt.previous().len()) {
            Ok(()) => attempt.follow(),
            Err(message) => attempt.error(std::io::Error::other(message)),
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DownloadPhase {
    Connecting,
    Downloading,
    Verifying,
    Saving,
    Preparing,
}

#[derive(Clone, Debug)]
pub struct DownloadProgress {
    pub phase: DownloadPhase,
    pub received: u64,
    pub total: u64,
    started: Instant,
    last_data: Instant,
}

impl DownloadProgress {
    pub fn message(&self) -> String {
        let elapsed = self.started.elapsed();
        match self.phase {
            DownloadPhase::Connecting => format!(
                "Connecting to update server... {}s elapsed (30s response limit)",
                elapsed.as_secs()
            ),
            DownloadPhase::Downloading => {
                let mib = 1024. * 1024.;
                let rate = self.received as f64 / elapsed.as_secs_f64().max(0.001) / mib;
                let mut message = format!(
                    "Downloading: {:.1} / {:.1} MiB ({}%) | {:.1} MiB/s average | {}s elapsed",
                    self.received as f64 / mib,
                    self.total as f64 / mib,
                    self.received.saturating_mul(100) / self.total.max(1),
                    rate,
                    elapsed.as_secs()
                );
                let idle = self.last_data.elapsed().as_secs();
                if idle >= 5 {
                    message.push_str(&format!(" | No data for {idle}s (30s inactivity limit)"));
                }
                message
            }
            DownloadPhase::Verifying => {
                format!(
                    "Verifying installer SHA-256... {}s elapsed",
                    elapsed.as_secs()
                )
            }
            DownloadPhase::Saving => format!(
                "Saving verified installer to disk... {}s elapsed",
                elapsed.as_secs()
            ),
            DownloadPhase::Preparing => format!(
                "Preparing application files in the background... {}s elapsed",
                elapsed.as_secs()
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum DownloadCancellation {
    Running,
    Paused,
    Handoff,
}

#[derive(Clone)]
pub struct DownloadControl {
    progress: tokio::sync::watch::Sender<DownloadProgress>,
    cancelled: tokio::sync::watch::Sender<DownloadCancellation>,
}

impl DownloadControl {
    pub fn new(release: &Release) -> Self {
        let now = Instant::now();
        let (progress, _) = tokio::sync::watch::channel(DownloadProgress {
            phase: DownloadPhase::Connecting,
            received: 0,
            total: release.size,
            started: now,
            last_data: now,
        });
        let (cancelled, _) = tokio::sync::watch::channel(DownloadCancellation::Running);
        Self {
            progress,
            cancelled,
        }
    }

    pub fn progress(&self) -> DownloadProgress {
        self.progress.borrow().clone()
    }

    pub fn cancel(&self) {
        self.cancelled.send_replace(DownloadCancellation::Paused);
    }

    pub fn cancel_for_handoff(&self) {
        self.cancelled.send_modify(|state| {
            // Closing the owner must not postpone another instance or undo an explicit pause.
            if *state == DownloadCancellation::Running {
                *state = DownloadCancellation::Handoff;
            }
        });
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow() != DownloadCancellation::Running
    }

    #[cfg(all(test, feature = "gui-tests"))]
    pub(crate) fn set_progress_for_test(&self, phase: DownloadPhase, received: u64) {
        self.update(phase, received);
    }

    fn update(&self, phase: DownloadPhase, received: u64) {
        self.progress.send_modify(|progress| {
            if progress.received != received || progress.phase != phase {
                progress.last_data = Instant::now();
            }
            progress.phase = phase;
            progress.received = received;
        });
    }

    async fn wait_cancelled(&self) {
        let mut cancelled = self.cancelled.subscribe();
        let _ = cancelled
            .wait_for(|state| *state != DownloadCancellation::Running)
            .await;
    }
}

pub struct PreparedUpdate {
    installer: PathBuf,
    release: Release,
}

fn cached_installer(release: &Release, cache: &Path) -> PathBuf {
    cache
        .join(format!("{}-{}", release.version, release.sha256))
        .join(installer_name(release.version))
}

fn discard_superseded_installers(cache: &Path, latest: Version) -> Result<()> {
    for entry in fs::read_dir(cache)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some((version, hash)) = name.to_str().and_then(|name| name.split_once('-')) else {
            continue;
        };
        let Ok(version) = version.parse::<Version>() else {
            continue;
        };
        if version < latest
            && hash.len() == 64
            && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            && name.to_str() == Some(format!("{version}-{hash}").as_str())
        {
            fs::remove_dir_all(entry.path()).context("Cannot discard a superseded update")?;
        }
    }
    Ok(())
}

fn discard_installer(release: &Release, cache: &Path) -> Result<()> {
    let installer = cached_installer(release, cache);
    let mut paths = vec![installer.clone(), installer.with_extension("exe.part")];
    if let Some(dir) = installer.parent() {
        for name in [
            PREPARED_MANIFEST_NAME,
            PREPARE_ERROR_NAME,
            DESKTOP_ARCHIVE_NAME,
            CLI_ARCHIVE_NAME,
        ] {
            paths.push(dir.join(name));
        }
    }
    for path in &paths {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Cannot discard an invalid installer"),
        }
    }
    Ok(())
}

#[cfg(test)]
fn write_prepared_payloads_for_test(cache_dir: &Path, version: Version) -> Result<()> {
    fs::create_dir_all(cache_dir)?;
    let desktop_payload = b"desktop-archive";
    let cli_payload = b"cli-archive";
    let desktop_hash = format!("{:x}", Sha256::digest(desktop_payload));
    let cli_hash = format!("{:x}", Sha256::digest(cli_payload));
    fs::write(cache_dir.join(DESKTOP_ARCHIVE_NAME), desktop_payload)?;
    fs::write(cache_dir.join(CLI_ARCHIVE_NAME), cli_payload)?;
    fs::write(
        cache_dir.join(PREPARED_MANIFEST_NAME),
        format!("version={version}\ndesktop_sha256={desktop_hash}\ncli_sha256={cli_hash}\n"),
    )?;
    Ok(())
}

fn verify_prepared_payloads(cache_dir: &Path, release: &Release) -> Result<Vec<fs::File>> {
    let manifest_path = cache_dir.join(PREPARED_MANIFEST_NAME);
    let mut text = String::new();
    fs::File::open(&manifest_path)
        .context("Prepared update manifest is missing")?
        .take(MAX_MANIFEST_SIZE + 1)
        .read_to_string(&mut text)
        .context("Cannot read prepared update manifest")?;
    ensure!(
        text.len() as u64 <= MAX_MANIFEST_SIZE,
        "Prepared update manifest is too large"
    );
    let mut fields = BTreeMap::new();
    for line in text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let (key, value) = line
            .split_once('=')
            .context("Invalid prepared update manifest line")?;
        ensure!(
            fields.insert(key.trim(), value.trim()).is_none(),
            "Duplicate prepared update manifest field: {key}"
        );
    }
    let version: Version = fields
        .get("version")
        .context("Prepared update manifest is missing version")?
        .parse()?;
    ensure!(
        version == release.version,
        "Prepared update version {version} does not match expected {}",
        release.version
    );
    let mut handles = Vec::with_capacity(2);
    for ((field, file_name), payload) in [
        ("desktop_sha256", DESKTOP_ARCHIVE_NAME),
        ("cli_sha256", CLI_ARCHIVE_NAME),
    ]
    .into_iter()
    .zip(release.payloads.iter())
    {
        let expected = fields
            .get(field)
            .with_context(|| format!("Prepared update manifest is missing {field}"))?
            .to_ascii_lowercase();
        ensure!(
            expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Prepared update {field} must contain 64 hexadecimal characters"
        );
        ensure!(
            expected == payload.sha256,
            "Prepared payload SHA-256 does not match the release feed"
        );
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let path = cache_dir.join(file_name);
        let file = options
            .open(&path)
            .with_context(|| format!("Prepared payload {file_name} is missing"))?;
        ensure!(
            file.metadata()?.len() == payload.size,
            "Prepared payload {file_name} size or SHA-256 does not match the release feed"
        );
        let mut reader = file.try_clone()?;
        let mut digest = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .with_context(|| format!("Cannot read prepared payload {file_name}"))?;
            if count == 0 {
                break;
            }
            total += count as u64;
            ensure!(
                total <= MAX_INSTALLER_SIZE,
                "Prepared payload {file_name} exceeds maximum size"
            );
            digest.update(&buffer[..count]);
        }
        ensure!(total > 0, "Prepared payload {file_name} is empty");
        ensure!(
            format!("{:x}", digest.finalize()) == expected,
            "Prepared payload {file_name} SHA-256 does not match the installer manifest"
        );
        handles.push(file);
    }
    Ok(handles)
}

async fn wait_for_preparation(
    command: &mut std::process::Command,
    control: &DownloadControl,
    timeout: Duration,
) -> Result<std::process::ExitStatus> {
    #[cfg(windows)]
    let mut wrapper = {
        // Start-Process -Wait tracks descendants even after Inno's launcher exits.
        // The outer job still owns cancellation; arguments are data, never PS source.
        let shell = PathBuf::from(
            std::env::var_os("SystemRoot").context("Windows system directory is unavailable")?,
        )
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut wrapper = std::process::Command::new(shell);
        let mut arguments = Vec::new();
        for argument in command.get_args() {
            let argument = argument.to_str().context("Invalid preparation argument")?;
            ensure!(
                !argument.contains(['"', '\r', '\n']),
                "Invalid preparation argument"
            );
            let trailing = argument
                .chars()
                .rev()
                .take_while(|character| *character == '\\')
                .count();
            arguments.push(format!("\"{argument}{}\"", "\\".repeat(trailing)));
        }
        for (name, value) in command.get_envs() {
            if let Some(value) = value {
                wrapper.env(name, value);
            } else {
                wrapper.env_remove(name);
            }
        }
        if let Some(directory) = command.get_current_dir() {
            wrapper.current_dir(directory);
        }
        wrapper.args(["-NoProfile", "-NonInteractive", "-Command",
            "$ErrorActionPreference = 'Stop'; $child = Start-Process -FilePath $env:REGRESSIFY_PREPARE_PROGRAM -ArgumentList $env:REGRESSIFY_PREPARE_ARGUMENTS -Wait -PassThru; exit $child.ExitCode"])
            .env("REGRESSIFY_PREPARE_PROGRAM", command.get_program())
            .env("REGRESSIFY_PREPARE_ARGUMENTS", arguments.join(" "))
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(test)]
        wrapper.stderr(std::process::Stdio::inherit());
        wrapper
    };
    #[cfg(windows)]
    let command = &mut wrapper;
    struct PreparationProcess(command_group::GroupChild);
    impl Drop for PreparationProcess {
        fn drop(&mut self) {
            // Inno uses a launcher and a second phase. Never leave the latter downloading.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    ensure!(
        !control.is_cancelled(),
        "Download cancelled. No update was installed"
    );
    let mut child = PreparationProcess(
        crate::helpers::spawn_background_group(command)
            .context("Cannot start the update installer to prepare application binaries")?,
    );
    let deadline = Instant::now() + timeout;
    loop {
        ensure!(
            !control.is_cancelled(),
            "Download cancelled. No update was installed"
        );
        ensure!(
            Instant::now() < deadline,
            "Update binary preparation timed out; retry later"
        );
        if let Some(status) = child
            .0
            .try_wait()
            .context("Cannot wait for update preparation")?
        {
            return Ok(status);
        }
        tokio::select! {
            _ = control.wait_cancelled() => bail!("Download cancelled. No update was installed"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {},
        }
    }
}

async fn run_installer_prepare(
    installer: &Path,
    release: &Release,
    control: &DownloadControl,
    timeout: Duration,
) -> Result<()> {
    let cache_dir = installer.parent().context("Invalid update cache path")?;
    ensure!(
        !control.is_cancelled(),
        "Download cancelled. No update was installed"
    );
    if verify_prepared_payloads(cache_dir, release).is_ok() {
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let verified_file = options.open(installer)?;
    verify_payload(verified_file.try_clone()?, std::io::sink(), release)?;
    for stale in [PREPARED_MANIFEST_NAME, PREPARE_ERROR_NAME] {
        match fs::remove_file(cache_dir.join(stale)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Cannot remove stale preparation receipt"),
        }
    }
    control.update(DownloadPhase::Preparing, release.size);
    let mut command = std::process::Command::new(installer);
    command
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/SP-",
            "/NORESTART",
            &format!("/PREPARE={}", cache_dir.display()),
            &format!("/LOG={}", cache_dir.join("prepare.log").display()),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let status = wait_for_preparation(&mut command, control, timeout).await?;
    // Returning false from Inno's InitializeSetup skips installation, cleans up, and exits 1.
    // Success additionally requires the fresh receipt and both feed-verified archives.
    ensure!(
        status.code() == Some(1),
        "Installer preparation failed ({status}); no update was installed"
    );
    ensure!(
        !control.is_cancelled(),
        "Download cancelled. No update was installed"
    );
    verify_prepared_payloads(cache_dir, release)
        .context("Installer did not finish preparing verified update files")?;
    Ok(())
}

impl PreparedUpdate {
    #[cfg(all(test, feature = "gui-tests"))]
    pub(crate) fn cache_for_test(mut release: Release, cache: &Path) -> Result<()> {
        let payload = b"installer";
        release.sha256 = format!("{:x}", Sha256::digest(payload));
        release.size = payload.len() as u64;
        let installer = cached_installer(&release, cache);
        let cache_dir = installer.parent().context("Invalid cache path")?;
        fs::create_dir_all(cache_dir)?;
        fs::write(installer.with_extension("exe.part"), payload)?;
        write_prepared_payloads_for_test(cache_dir, release.version)?;
        UpdateState {
            release: Some(ManifestFields::from(&release)),
            ..UpdateState::default()
        }
        .save(cache)
    }

    #[cfg(all(test, feature = "gui-tests"))]
    pub(crate) fn missing_installer_for_test(release: Release) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        Ok(Self {
            installer: directory.path().join(installer_name(release.version)),
            release,
        })
    }

    pub fn download(release: Release, cache: &Path) -> Result<Self> {
        let control = DownloadControl::new(&release);
        Self::download_with_control(release, cache, control)
    }

    pub fn download_with_control(
        release: Release,
        cache: &Path,
        control: DownloadControl,
    ) -> Result<Self> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Cannot start the update downloader")?
            .block_on(async {
                let client = reqwest::Client::builder()
                    .https_only(true)
                    .redirect(installer_redirect_policy())
                    .connect_timeout(Duration::from_secs(10))
                    .user_agent(concat!("Regressify-Desktop/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .context("Cannot create the update client")?;
                let prepared = Self::download_with_client(
                    release,
                    cache,
                    &control,
                    client,
                    DOWNLOAD_IDLE_TIMEOUT,
                    DOWNLOAD_TIMEOUT,
                )
                .await?;
                let _lock = lock_cache(cache)?;
                run_installer_prepare(
                    &prepared.installer,
                    &prepared.release,
                    &control,
                    DOWNLOAD_TIMEOUT,
                )
                .await?;
                Ok(prepared)
            })
    }

    async fn download_with_client(
        release: Release,
        cache: &Path,
        control: &DownloadControl,
        client: reqwest::Client,
        idle_timeout: Duration,
        total_timeout: Duration,
    ) -> Result<Self> {
        let _lock = lock_cache(cache)?;
        let mut state = UpdateState::load(cache)?;
        if state.skips(release.version)? {
            return Err(SkippedUpdate {
                version: release.version,
            }
            .into());
        }
        state.select_release(&release, cache)?;
        state.ensure_retry_due(release.version, unix_timestamp())?;
        if state.discard_payload {
            discard_installer(&release, cache)?;
            state.discard_payload = false;
            state.save(cache)?;
        }
        match Self::download_once(
            release.clone(),
            cache,
            control,
            &client,
            idle_timeout,
            total_timeout,
        )
        .await
        {
            Err(error) if error.is::<ChecksumMismatch>() => {
                state.hash_failures = state.hash_failures.saturating_add(1);
                state.discard_payload = true;
                let retry: anyhow::Error = if state.hash_failures > MAX_HASH_RETRIES {
                    state.skipped_version = Some(release.version.to_string());
                    state.next_retry_at = None;
                    SkippedUpdate {
                        version: release.version,
                    }
                    .into()
                } else {
                    let delay = HASH_RETRY_DELAY.as_secs() * (1u64 << (state.hash_failures - 1));
                    let retry_at = unix_timestamp().saturating_add(delay);
                    state.next_retry_at = Some(retry_at);
                    RetryScheduled {
                        version: release.version,
                        retry_at,
                    }
                    .into()
                };
                state.save(cache)?;
                discard_installer(&release, cache)?;
                state.discard_payload = false;
                state.save(cache)?;
                Err(retry)
            }
            Ok(prepared) => {
                state.next_retry_at = None;
                state.save(cache)?;
                Ok(prepared)
            }
            Err(error) => Err(error),
        }
    }

    async fn download_once(
        release: Release,
        cache: &Path,
        control: &DownloadControl,
        client: &reqwest::Client,
        idle_timeout: Duration,
        total_timeout: Duration,
    ) -> Result<Self> {
        let download = async {
            let installer = cached_installer(&release, cache);
            fs::create_dir_all(installer.parent().context("Invalid update cache path")?)
                .context("Cannot create the update cache")?;
            if installer.try_exists()? {
                control.update(DownloadPhase::Verifying, release.size);
                verify_payload(fs::File::open(&installer)?, std::io::sink(), &release)?;
                control.update(DownloadPhase::Saving, release.size);
                return Ok(Self { installer, release });
            }
            let partial = installer.with_extension("exe.part");
            let mut output = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&partial)
                .context(
                    "Cannot save the installer; check free disk space and update-cache permissions",
                )?;
            let mut received = output.metadata()?.len();
            if received > release.size {
                output.set_len(0)?;
                received = 0;
            }
            if received < release.size {
                control.update(DownloadPhase::Connecting, received);
                let mut request = client
                    .get(release.installer.clone())
                    .header(reqwest::header::ACCEPT_ENCODING, "identity");
                if received > 0 {
                    request = request.header(reqwest::header::RANGE, format!("bytes={received}-"));
                }
                let mut response = tokio::time::timeout(idle_timeout, request.send())
                    .await
                    .with_context(|| format!(
                        "Update server did not respond within {} seconds. Check your connection, VPN or proxy, then retry",
                        idle_timeout.as_secs_f64()
                    ))?
                    .context("Cannot connect to the update server; check your connection, VPN or proxy, then retry")?;
                ensure!(
                    !response.status().is_redirection(),
                    "Installer download was redirected (HTTP {}); update was not installed",
                    response.status()
                );
                ensure!(
                    matches!(
                        response.status(),
                        StatusCode::OK | StatusCode::PARTIAL_CONTENT
                    ),
                    "Update server returned HTTP {} for the installer; try checking for updates again",
                    response.status()
                );
                let offset = if response.status() == StatusCode::PARTIAL_CONTENT {
                    let expected =
                        format!("bytes {received}-{}/{}", release.size - 1, release.size);
                    ensure!(
                        response
                            .headers()
                            .get(reqwest::header::CONTENT_RANGE)
                            .and_then(|value| value.to_str().ok())
                            == Some(expected.as_str()),
                        "Installer Content-Range does not match the requested bytes; retry later"
                    );
                    received
                } else {
                    0
                };
                if let Some(length) = response.content_length() {
                    ensure!(
                        length == release.size - offset,
                        "Installer Content-Length is {length} bytes but {} bytes were expected; check for updates again",
                        release.size - offset
                    );
                }
                if offset == 0 {
                    output.set_len(0)?;
                }
                output.seek(SeekFrom::Start(offset))?;
                received = offset;
                control.update(DownloadPhase::Downloading, received);
                loop {
                    let chunk = tokio::time::timeout(idle_timeout, response.chunk())
                        .await
                        .with_context(|| format!(
                            "Download stalled: no data received for {} seconds ({received} of {} bytes). Check your connection, VPN or proxy, then retry",
                            idle_timeout.as_secs_f64(),
                            release.size
                        ))?
                        .with_context(|| format!(
                            "Installer transfer failed after {received} of {} bytes; check your connection and retry",
                            release.size
                        ))?;
                    let Some(chunk) = chunk else {
                        break;
                    };
                    received += chunk.len() as u64;
                    ensure!(
                        received <= release.size,
                        "Installer exceeds its declared size"
                    );
                    output.write_all(&chunk).context(
                        "Cannot save the installer; check free disk space and update-cache permissions",
                    )?;
                    control.update(DownloadPhase::Downloading, received);
                }
            }
            output.sync_all()
                .context("Cannot finish saving the installer; check free disk space and update-cache permissions")?;
            drop(output);
            control.update(DownloadPhase::Verifying, received);
            verify_payload(fs::File::open(&partial)?, std::io::sink(), &release)?;
            control.update(DownloadPhase::Saving, received);
            fs::rename(&partial, &installer).context("Cannot save the verified installer")?;
            Ok(Self { installer, release })
        };
        tokio::select! {
            biased;
            _ = control.wait_cancelled() => bail!("Download cancelled. No update was installed"),
            result = tokio::time::timeout(total_timeout, download) => result.with_context(|| {
                let progress = control.progress();
                format!(
                    "Update download exceeded its {}-second limit ({} of {} bytes). Check your connection and retry",
                    total_timeout.as_secs_f64(), progress.received, progress.total
                )
            })?,
        }
    }

    pub fn version(&self) -> Version {
        self.release.version
    }

    pub fn release(&self) -> &Release {
        &self.release
    }

    pub fn launch(&mut self, workspace: Option<&Path>) -> Result<()> {
        ensure!(
            cfg!(target_os = "windows"),
            "Self-update is supported on Windows only"
        );
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let verified_file = options.open(&self.installer)?;
        verify_payload(verified_file.try_clone()?, std::io::sink(), &self.release)?;
        let cache_dir = self
            .installer
            .parent()
            .context("Invalid update cache path")?;
        let _payload_locks = verify_prepared_payloads(cache_dir, &self.release)?;
        let mut command = std::process::Command::new(&self.installer);
        command.args(["/SP-", "/SILENT", "/NORESTART"]);
        command.arg(format!("/SOURCE={}", cache_dir.display()));
        if let Some(workspace) = workspace {
            command.arg(format!("/WORKSPACE={}", workspace.display()));
        }
        command
            .spawn()
            .context("Cannot start the update installer")?;
        Ok(())
    }
}

fn verify_payload(mut input: impl Read, mut output: impl Write, release: &Release) -> Result<()> {
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).with_context(|| {
            format!(
                "Installer read failed after {total} of {} bytes",
                release.size
            )
        })?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(total <= release.size, "Installer exceeds its declared size");
        digest.update(&buffer[..count]);
        output.write_all(&buffer[..count]).context(
            "Cannot save the installer; check free disk space and update-cache permissions",
        )?;
    }
    verify_digest(total, digest, release)
}

fn verify_digest(total: u64, digest: Sha256, release: &Release) -> Result<()> {
    ensure!(
        total == release.size,
        "Installer download is incomplete: received {total} of {} bytes",
        release.size
    );
    if format!("{:x}", digest.finalize()) != release.sha256 {
        return Err(ChecksumMismatch.into());
    }
    Ok(())
}

pub fn protect_installation(after_install: bool) -> Result<()> {
    #[cfg(windows)]
    windows::protect_installation(after_install)?;
    #[cfg(not(windows))]
    let _ = after_install;
    Ok(())
}

#[cfg(windows)]
mod windows {
    use std::{
        os::windows::io::{FromRawHandle, OwnedHandle},
        sync::OnceLock,
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result, ensure};
    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, GetLastError},
        System::Threading::{CreateMutexW, MUTEX_ALL_ACCESS, OpenMutexW},
    };

    const APP_MUTEX: &str =
        "Local\\Regressify.Desktop.Running.ED6F0908-7E11-4608-A532-E43482AA117B";
    const SETUP_MUTEX: &str =
        "Local\\Regressify.Desktop.Setup.ED6F0908-7E11-4608-A532-E43482AA117B";
    static RUNNING: OnceLock<OwnedHandle> = OnceLock::new();

    fn create_mutex(name: &str) -> Result<OwnedHandle> {
        let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        ensure!(
            !handle.is_null(),
            "Cannot register this process with the Windows installer"
        );
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn mutex_exists(name: &str) -> Result<bool> {
        let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        let handle = unsafe { OpenMutexW(MUTEX_ALL_ACCESS, 0, name.as_ptr()) };
        if !handle.is_null() {
            drop(unsafe { OwnedHandle::from_raw_handle(handle) });
            return Ok(true);
        }
        let error = unsafe { GetLastError() };
        if error == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        Err(std::io::Error::from_raw_os_error(error as i32))
            .context("Cannot check whether setup is running")
    }

    fn acquire(app: &str, setup: &str) -> Result<OwnedHandle> {
        let running = create_mutex(app)?;
        ensure!(
            !mutex_exists(setup)?,
            "Regressify setup is running. Finish setup before opening the app."
        );
        Ok(running)
    }

    pub(super) fn protect_installation(after_install: bool) -> Result<()> {
        if after_install {
            let deadline = Instant::now() + Duration::from_secs(30);
            while mutex_exists(SETUP_MUTEX)? && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let running = acquire(APP_MUTEX, SETUP_MUTEX)?;
        let _ = RUNNING.set(running);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{acquire, create_mutex, mutex_exists};

        #[test]
        fn installer_guard_tracks_all_instances_and_blocks_new_launches() {
            let app = format!("Local\\regressify-test-{}", uuid::Uuid::new_v4());
            let setup = format!("{app}-setup");
            let first = acquire(&app, &setup).unwrap();
            let second = acquire(&app, &setup).unwrap();
            drop(first);
            assert!(mutex_exists(&app).unwrap());
            let installing = create_mutex(&setup).unwrap();
            assert!(acquire(&app, &setup).is_err());
            drop(second);
            assert!(!mutex_exists(&app).unwrap());
            drop(installing);
            assert!(acquire(&app, &setup).is_ok());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHECK_FAILED_PREFIX, CheckMode, DownloadControl, DownloadPhase, ManifestFields,
        NoticeCheck, NoticeOutcome, NoticeState, PreparedUpdate, SkippedUpdate, UpdateCheck,
        UpdateFeed, UpdateNotice, UpdatePlatform, UpdateState, Version, cached_installer,
        dismissal_hides, installer_name, installer_redirect_policy, is_allowed_installer_redirect,
        lock_cache, notice_message_after_success, verify_payload,
    };
    use super::{InstallPlan, macos_install};
    use reqwest::StatusCode;
    use sha2::{Digest, Sha256};
    use std::{fs, time::Duration};

    fn manifest(payload: &[u8]) -> String {
        format!(
            "version=1.10.0.0\r\ninstaller=https://github.com/precise-alloy/regressify-rs/releases/download/v1.10.0/Regressify-Setup-1.10.0.0-x64.exe\r\nsha256={:x}\r\nsize={}\r\nnotes=https://example.com/notes\r\narchive.regressify-desktop-x86_64-pc-windows-msvc.zip=15:{:x}\narchive.regressify-cli-x86_64-pc-windows-msvc.zip=11:{:x}\n",
            Sha256::digest(payload),
            payload.len(),
            Sha256::digest(b"desktop-archive"),
            Sha256::digest(b"cli-archive"),
        )
    }

    #[test]
    fn shared_checks_wait_eight_hours_and_hold_ownership_until_preparation_finishes() {
        use super::{CHECK_INTERVAL, SharedUpdateCheck};
        use std::cell::Cell;

        let cache = tempfile::tempdir().unwrap();
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let release = feed.parse_manifest(&manifest(b"installer")).unwrap();
        let current = "1.0".parse().unwrap();
        let now = super::unix_timestamp();
        let requests = Cell::new(0);
        let check = || {
            requests.set(requests.get() + 1);
            Ok(UpdateCheck::Available(release.clone()))
        };
        let first = feed
            .check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                None,
                now,
                check,
            )
            .unwrap();
        let SharedUpdateCheck::Checked(UpdateCheck::Available(_), lease) = first else {
            panic!("Expected ownership")
        };
        for mode in [CheckMode::Scheduled, CheckMode::Startup, CheckMode::Manual] {
            assert!(matches!(
                feed.check_shared_with(current, cache.path(), mode, None, now, check)
                    .unwrap(),
                SharedUpdateCheck::Busy
            ));
        }
        assert_eq!(requests.get(), 1);
        drop(lease);
        let second = feed
            .check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                None,
                now + 1,
                check,
            )
            .unwrap();
        assert!(matches!(
            second,
            SharedUpdateCheck::Checked(UpdateCheck::Available(_), _)
        ));
        drop(second);
        assert_eq!(requests.get(), 1);
        assert!(matches!(
            feed.check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                Some(&release),
                now + CHECK_INTERVAL.as_secs() - 1,
                check
            )
            .unwrap(),
            SharedUpdateCheck::Unchanged
        ));
        assert_eq!(requests.get(), 1);
        let next = feed
            .check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                Some(&release),
                now + CHECK_INTERVAL.as_secs(),
                check,
            )
            .unwrap();
        assert!(matches!(next, SharedUpdateCheck::Unchanged));
        assert_eq!(requests.get(), 2);
    }

    #[test]
    fn startup_checks_refresh_recent_state_and_reschedule_all_outcomes() {
        use super::{CHECK_INTERVAL, SharedUpdateCheck};
        use std::cell::Cell;

        for outcome in 0..4 {
            let cache = tempfile::tempdir().unwrap();
            let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
            let release = feed.parse_manifest(&manifest(b"installer")).unwrap();
            let current = "1.0".parse().unwrap();
            UpdateState {
                last_check_at: Some(100),
                ..UpdateState::default()
            }
            .save(cache.path())
            .unwrap();
            let requests = Cell::new(0);
            let check = || {
                requests.set(requests.get() + 1);
                match outcome {
                    0 => Ok(UpdateCheck::Available(release.clone())),
                    1 => Ok(UpdateCheck::Current),
                    2 => Ok(UpdateCheck::Unpublished),
                    _ => Err(anyhow::anyhow!("offline")),
                }
            };
            for now in [101, 102] {
                let result = feed.check_shared_with(
                    current,
                    cache.path(),
                    CheckMode::Startup,
                    None,
                    now,
                    check,
                );
                match outcome {
                    0 => assert!(matches!(
                        &result,
                        Ok(SharedUpdateCheck::Checked(UpdateCheck::Available(_), _))
                    )),
                    1 => assert!(matches!(
                        &result,
                        Ok(SharedUpdateCheck::Checked(UpdateCheck::Current, _))
                    )),
                    2 => assert!(matches!(
                        &result,
                        Ok(SharedUpdateCheck::Checked(UpdateCheck::Unpublished, _))
                    )),
                    _ => assert!(result.is_err()),
                }
                drop(result);
                assert_eq!(
                    UpdateState::load(cache.path()).unwrap().last_check_at,
                    Some(now)
                );
            }
            assert_eq!(requests.get(), 2);
            let prepared = (outcome == 0).then_some(&release);
            let next_check = 102 + CHECK_INTERVAL.as_secs();
            assert!(matches!(
                feed.check_shared_with(
                    current,
                    cache.path(),
                    CheckMode::Scheduled,
                    prepared,
                    next_check - 1,
                    || panic!("Scheduled check ran before eight hours")
                )
                .unwrap(),
                SharedUpdateCheck::Unchanged
            ));
            drop(feed.check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                prepared,
                next_check,
                check,
            ));
            assert_eq!(requests.get(), 3);
            assert_eq!(
                UpdateState::load(cache.path()).unwrap().last_check_at,
                Some(next_check)
            );
        }
    }

    #[test]
    fn startup_checks_preserve_pause_checksum_backoff_and_skipped_versions() {
        use super::{CHECK_INTERVAL, RetryScheduled, SharedUpdateCheck};
        use std::cell::Cell;

        for restriction in 0..3 {
            let cache = tempfile::tempdir().unwrap();
            let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
            let release = feed.parse_manifest(&manifest(b"installer")).unwrap();
            let now = super::unix_timestamp();
            let mut state = UpdateState {
                release: Some(ManifestFields::from(&release)),
                last_check_at: Some(now),
                ..UpdateState::default()
            };
            match restriction {
                0 => state.preparation_retry_at = Some(now + CHECK_INTERVAL.as_secs()),
                1 => {
                    state.hash_failures = 1;
                    state.next_retry_at = Some(now + 7200);
                }
                _ => {
                    state.hash_failures = 4;
                    state.skipped_version = Some(release.version.to_string());
                }
            }
            state.save(cache.path()).unwrap();
            let requested = Cell::new(false);
            let result = feed.check_shared_with(
                "1.0".parse().unwrap(),
                cache.path(),
                CheckMode::Startup,
                None,
                now + 1,
                || {
                    requested.set(true);
                    Ok(UpdateCheck::Available(release.clone()))
                },
            );
            assert!(requested.get());
            match restriction {
                0 => assert!(matches!(result, Ok(SharedUpdateCheck::Unchanged))),
                1 => assert!(result.is_err_and(|error| error.is::<RetryScheduled>())),
                _ => assert!(result.is_err_and(|error| error.is::<SkippedUpdate>())),
            }
            let saved = UpdateState::load(cache.path()).unwrap();
            assert_eq!(saved.last_check_at, Some(now + 1));
            assert_eq!(saved.hash_failures, state.hash_failures);
            assert_eq!(saved.next_retry_at, state.next_retry_at);
            assert_eq!(saved.skipped_version, state.skipped_version);
            assert_eq!(saved.preparation_retry_at, state.preparation_retry_at);
        }
    }

    #[test]
    fn shared_checks_throttle_current_unpublished_and_failed_results() {
        use super::{CHECK_INTERVAL, SharedUpdateCheck};
        for outcome in [0, 1, 2] {
            let cache = tempfile::tempdir().unwrap();
            let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
            let current = "1.0".parse().unwrap();
            let check = || match outcome {
                0 => Ok(UpdateCheck::Current),
                1 => Ok(UpdateCheck::Unpublished),
                _ => Err(anyhow::anyhow!("offline")),
            };
            drop(feed.check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                None,
                100,
                check,
            ));
            let waiting = feed
                .check_shared_with(
                    current,
                    cache.path(),
                    CheckMode::Scheduled,
                    None,
                    101,
                    || panic!("Duplicate check"),
                )
                .unwrap();
            assert!(matches!(waiting, SharedUpdateCheck::Unchanged));
            drop(feed.check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                None,
                100 + CHECK_INTERVAL.as_secs(),
                check,
            ));
            drop(feed.check_shared_with(
                current,
                cache.path(),
                CheckMode::Manual,
                None,
                101 + CHECK_INTERVAL.as_secs(),
                check,
            ));
            assert_eq!(
                UpdateState::load(cache.path()).unwrap().last_check_at,
                Some(101 + CHECK_INTERVAL.as_secs())
            );
        }
    }

    #[test]
    fn shared_checks_reuse_verified_downloads_and_recover_abandoned_work() {
        use super::{SharedUpdateCheck, UpdateLease};

        let cache = tempfile::tempdir().unwrap();
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let payload = b"installer";
        let release = feed.parse_manifest(&manifest(payload)).unwrap();
        let current = "1.0".parse().unwrap();
        let now = super::unix_timestamp();
        let first = feed
            .check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                None,
                now,
                || Ok(UpdateCheck::Available(release.clone())),
            )
            .unwrap();
        let SharedUpdateCheck::Checked(_, lease) = first else {
            panic!("Expected ownership")
        };
        drop(lease);
        let second = feed
            .check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                None,
                now,
                || panic!("Duplicate feed request"),
            )
            .unwrap();
        let SharedUpdateCheck::Checked(_, lease) = second else {
            panic!("Expected takeover")
        };
        let installer = cached_installer(&release, cache.path());
        fs::create_dir_all(installer.parent().unwrap()).unwrap();
        fs::write(installer.with_extension("exe.part"), payload).unwrap();
        super::write_prepared_payloads_for_test(installer.parent().unwrap(), release.version)
            .unwrap();
        let prepared = lease
            .prepare(release.clone(), DownloadControl::new(&release))
            .unwrap();
        assert_eq!(fs::read(&prepared.installer).unwrap(), payload);
        for _ in 0..2 {
            let checked = feed
                .check_shared_with(
                    current,
                    cache.path(),
                    CheckMode::Scheduled,
                    None,
                    now,
                    || panic!("Duplicate feed request"),
                )
                .unwrap();
            let SharedUpdateCheck::Checked(_, lease) = checked else {
                panic!("Expected cached release")
            };
            let peer = lease
                .prepare(release.clone(), DownloadControl::new(&release))
                .unwrap();
            assert_eq!(peer.installer, prepared.installer);
            assert_eq!(peer.release(), prepared.release());
        }
        let lease = UpdateLease::acquire(cache.path()).unwrap().unwrap();
        let failure = lease.prepare_with(&DownloadControl::new(&release), || {
            Err(anyhow::anyhow!("offline"))
        });
        assert!(failure.is_err());
        drop(lease);
        assert!(matches!(
            feed.check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                None,
                now,
                || panic!("Duplicate feed request")
            )
            .unwrap(),
            SharedUpdateCheck::Unchanged
        ));
        let manual = feed
            .check_shared_with(current, cache.path(), CheckMode::Manual, None, now, || {
                Ok(UpdateCheck::Available(release))
            })
            .unwrap();
        assert!(matches!(
            manual,
            SharedUpdateCheck::Checked(UpdateCheck::Available(_), _)
        ));
    }

    #[test]
    fn shared_checks_observe_newer_versions_and_handle_clock_rollback() {
        use super::{CHECK_INTERVAL, SharedUpdateCheck};

        let cache = tempfile::tempdir().unwrap();
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let first = feed.parse_manifest(&manifest(b"installer")).unwrap();
        let next = feed
            .parse_manifest(
                &manifest(b"next")
                    .replace("1.10.0.0", "1.11.0.0")
                    .replace("v1.10.0", "v1.11.0"),
            )
            .unwrap();
        let current = "1.0".parse().unwrap();
        let now = super::unix_timestamp();
        drop(
            feed.check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                None,
                now,
                || Ok(UpdateCheck::Available(first.clone())),
            )
            .unwrap(),
        );
        let checked = feed
            .check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                Some(&first),
                now + CHECK_INTERVAL.as_secs(),
                || Ok(UpdateCheck::Available(next.clone())),
            )
            .unwrap();
        assert!(
            matches!(&checked, SharedUpdateCheck::Checked(UpdateCheck::Available(release), _) if release == &next)
        );
        drop(checked);
        let peer = feed
            .check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                Some(&first),
                now + CHECK_INTERVAL.as_secs(),
                || panic!("Duplicate feed request"),
            )
            .unwrap();
        assert!(
            matches!(&peer, SharedUpdateCheck::Checked(UpdateCheck::Available(release), _) if release == &next)
        );
        drop(peer);
        drop(
            feed.check_shared_with(
                current,
                cache.path(),
                CheckMode::Scheduled,
                Some(&next),
                now,
                || Ok(UpdateCheck::Available(next.clone())),
            )
            .unwrap(),
        );
        assert_eq!(
            UpdateState::load(cache.path()).unwrap().last_check_at,
            Some(now)
        );
    }

    #[test]
    fn shared_update_lease_is_exclusive_across_processes() {
        use super::UpdateLease;
        const CHILD_CACHE: &str = "REGRESSIFY_UPDATE_LOCK_TEST_CACHE";
        const CHILD_EXPECTS_BUSY: &str = "REGRESSIFY_UPDATE_LOCK_TEST_BUSY";

        if let Some(cache) = std::env::var_os(CHILD_CACHE) {
            let lease = UpdateLease::acquire(std::path::Path::new(&cache)).unwrap();
            assert_eq!(
                lease.is_none(),
                std::env::var_os(CHILD_EXPECTS_BUSY).is_some()
            );
            return;
        }
        let cache = tempfile::tempdir().unwrap();
        let lease = UpdateLease::acquire(cache.path()).unwrap().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "desktop::updates::tests::shared_update_lease_is_exclusive_across_processes",
                "--nocapture",
            ])
            .env(CHILD_CACHE, cache.path())
            .env(CHILD_EXPECTS_BUSY, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        drop(lease);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "desktop::updates::tests::shared_update_lease_is_exclusive_across_processes",
                "--nocapture",
            ])
            .env(CHILD_CACHE, cache.path())
            .env_remove(CHILD_EXPECTS_BUSY)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(UpdateLease::acquire(cache.path()).unwrap().is_some());
    }

    #[test]
    fn shared_owner_shutdown_allows_handoff_but_preserves_explicit_pause() {
        use super::{SharedUpdateCheck, UpdateLease};

        for paused in [false, true] {
            let cache = tempfile::tempdir().unwrap();
            let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
            let release = feed.parse_manifest(&manifest(b"installer")).unwrap();
            let current = "1.0".parse().unwrap();
            let now = super::unix_timestamp();
            UpdateState {
                release: Some(ManifestFields::from(&release)),
                last_check_at: Some(now),
                ..UpdateState::default()
            }
            .save(cache.path())
            .unwrap();
            let control = DownloadControl::new(&release);
            if paused {
                control.cancel();
            }
            control.cancel_for_handoff();
            assert!(control.is_cancelled());
            let lease = UpdateLease::acquire(cache.path()).unwrap().unwrap();
            assert!(
                lease
                    .prepare_with(&control, || Err(anyhow::anyhow!("cancelled")))
                    .is_err()
            );
            drop(lease);
            let checked = feed
                .check_shared_with(
                    current,
                    cache.path(),
                    CheckMode::Scheduled,
                    None,
                    now,
                    || panic!("Duplicate feed request"),
                )
                .unwrap();
            assert_eq!(matches!(checked, SharedUpdateCheck::Unchanged), paused);
            assert_eq!(
                matches!(
                    checked,
                    SharedUpdateCheck::Checked(UpdateCheck::Available(_), _)
                ),
                !paused
            );
        }
    }

    #[test]
    fn versions_are_numeric_normalized_and_bounded() {
        assert_eq!("01.03".parse::<Version>().unwrap().to_string(), "1.3.0.0");
        assert!("1.10".parse::<Version>().unwrap() > "1.9.9999".parse::<Version>().unwrap());
        assert_eq!(
            "1.3".parse::<Version>().unwrap(),
            "1.3.0.0".parse().unwrap()
        );
        for invalid in [
            "",
            "v1.2.3",
            "1.2.3-beta",
            "1..2",
            "-1",
            "1.10000",
            "1.2.3.4.5",
            " 1",
            "1.",
        ] {
            assert!(invalid.parse::<Version>().is_err(), "{invalid}");
        }
    }

    const LIVE_MANIFEST: &str = "version=2.2.2.0\ntag=v2.2.2\ninstaller=https://github.com/precise-alloy/regressify-rs/releases/download/v2.2.2/Regressify-Setup-2.2.2.0-x64.exe\nsha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsize=1234\narchive.regressify-desktop-aarch64-apple-darwin.zip=10:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\narchive.regressify-desktop-x86_64-apple-darwin.zip=11:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\n";

    fn macos_feed() -> UpdateFeed {
        UpdateFeed::for_origin(Some("https://regressify.eshn.dev"), UpdatePlatform::MacOs).unwrap()
    }

    fn current() -> Version {
        "2.0.0".parse().unwrap()
    }

    fn notice(version: &str) -> UpdateNotice {
        let tag = version.rsplit_once('.').map_or(version, |(short, _)| short);
        macos_feed()
            .parse_notice_manifest(&format!("version={version}\ntag=v{tag}\n"))
            .unwrap()
    }

    #[test]
    fn display_version_drops_a_zero_fourth_component_only() {
        let show = |text: &str| text.parse::<Version>().unwrap().display_short();
        assert_eq!(show("2.2.2.0"), "2.2.2");
        assert_eq!(show("2.2.2"), "2.2.2");
        assert_eq!(show("2.2.2.1"), "2.2.2.1");
        assert_eq!(show("2.0.0.0"), "2.0.0");
        // Comparison is unchanged.
        assert!("2.2.2.0".parse::<Version>().unwrap() < "2.2.2.1".parse().unwrap());
    }

    #[test]
    fn install_plan_comes_from_the_validated_tag_and_this_archive() {
        let feed = macos_feed();
        let arm = Some("regressify-desktop-aarch64-apple-darwin.zip");
        let plan = feed
            .parse_install_plan(LIVE_MANIFEST, current(), arm)
            .unwrap();
        assert_eq!(plan.version, "2.2.2".parse::<Version>().unwrap());
        assert_eq!(
            plan.url,
            "https://github.com/precise-alloy/regressify-rs/releases/download/v2.2.2/regressify-desktop-aarch64-apple-darwin.zip"
        );
        assert_eq!(plan.meta.size, 10);
        let intel = Some("regressify-desktop-x86_64-apple-darwin.zip");
        assert_eq!(
            feed.parse_install_plan(LIVE_MANIFEST, current(), intel)
                .unwrap()
                .meta
                .size,
            11
        );
        // Not newer, unknown architecture, missing archive, and a bad tag all refuse.
        let same = "2.2.2.0".parse().unwrap();
        assert!(feed.parse_install_plan(LIVE_MANIFEST, same, arm).is_err());
        assert!(
            feed.parse_install_plan(LIVE_MANIFEST, current(), None)
                .is_err()
        );
        let without = LIVE_MANIFEST.replace("archive.regressify-desktop-aarch64", "archive.x");
        assert!(feed.parse_install_plan(&without, current(), arm).is_err());
        let traversal = LIVE_MANIFEST.replace("tag=v2.2.2", "tag=v2.2.2/../x");
        assert!(feed.parse_install_plan(&traversal, current(), arm).is_err());
        assert!(
            UpdateFeed::new("https://regressify.eshn.dev")
                .unwrap()
                .macos_install_plan(current())
                .is_err()
        );
    }

    #[test]
    fn install_state_messages_and_final_states() {
        use super::{InstallHandle, InstallState, perform_install};
        let say = |state: InstallState| state.message();
        assert_eq!(
            say(InstallState::Downloading {
                received: 5,
                total: 10
            }),
            "Downloading 50%"
        );
        assert_eq!(
            say(InstallState::Downloading {
                received: 0,
                total: 0
            }),
            "Downloading 0%"
        );
        assert_eq!(say(InstallState::Verifying), "Verifying");
        assert_eq!(say(InstallState::Installing), "Installing");
        assert!(say(InstallState::Restart).contains("Restart"));
        assert!(say(InstallState::Failed("bad".into())).starts_with("Failed: bad"));
        assert!(InstallState::Verifying.in_progress() && !InstallState::Restart.in_progress());

        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Regressify.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/regressify-desktop"), b"old").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let plan = macos_feed()
            .parse_install_plan(
                LIVE_MANIFEST,
                current(),
                Some("regressify-desktop-aarch64-apple-darwin.zip"),
            )
            .unwrap();
        let offline =
            |_: &str| -> anyhow::Result<Box<dyn std::io::Read>> { anyhow::bail!("offline") };
        let ok = |_: &std::path::Path| Ok(());
        let deps = super::macos_install::Deps {
            open: &offline,
            sign: &ok,
            verify: &ok,
            ops: &super::macos_install::RealOps,
        };
        let handle = InstallHandle::new();
        assert!(perform_install(&app, cache.path(), &plan, &deps, &handle).is_err());
        assert!(matches!(handle.get(), InstallState::Failed(reason) if reason.contains("offline")));
        assert_eq!(
            std::fs::read(app.join("Contents/MacOS/regressify-desktop")).unwrap(),
            b"old"
        );
    }

    fn fake_installed_app(parent: &std::path::Path) -> std::path::PathBuf {
        let app = parent.join("Regressify.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/regressify-desktop"), b"old").unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleVersion</key><string>1.0.0.0</string>\
             <key>CFBundleShortVersionString</key><string>1.0.0.0</string></dict></plist>",
        )
        .unwrap();
        app
    }

    fn plan_serving(version: &str, binary: &[u8]) -> (InstallPlan, Vec<u8>) {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file(
                "regressify-desktop",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(binary).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let digest = sha2::Sha256::digest(&bytes);
        let plan = InstallPlan {
            version: version.parse().unwrap(),
            url: "https://example.test/a.zip".into(),
            meta: macos_install::ArchiveMeta {
                size: bytes.len() as u64,
                sha256: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
            },
        };
        (plan, bytes)
    }

    #[test]
    fn a_stale_process_never_downgrades_or_reinstalls_what_a_peer_installed() {
        use super::{InstallHandle, install_locked};
        let dir = tempfile::tempdir().unwrap();
        let app = fake_installed_app(dir.path());
        let cache = tempfile::tempdir().unwrap();
        let (plan_c, bytes_c) = plan_serving("2.5.0", b"c");
        let (plan_b, bytes_b) = plan_serving("2.3.0", b"b");
        let serve = |bytes: Vec<u8>| {
            move |_: &str| -> anyhow::Result<Box<dyn std::io::Read>> {
                Ok(Box::new(std::io::Cursor::new(bytes.clone())))
            }
        };
        let ok = |_: &std::path::Path| Ok(());
        let run = |plan: &InstallPlan, bytes: Vec<u8>| {
            let open = serve(bytes);
            let deps = macos_install::Deps {
                open: &open,
                sign: &ok,
                verify: &ok,
                ops: &macos_install::RealOps,
            };
            let lock = macos_install::InstallLock::acquire(&app).unwrap();
            // Both "processes" still run 2.0.0.
            install_locked(
                &lock,
                &app,
                cache.path(),
                current(),
                plan,
                &deps,
                &InstallHandle::new(),
            )
        };
        let binary = || std::fs::read(app.join("Contents/MacOS/regressify-desktop")).unwrap();

        run(&plan_c, bytes_c.clone()).unwrap();
        assert_eq!(binary(), b"c");
        assert_eq!(
            macos_install::installed_version(&app).as_deref(),
            Some("2.5.0.0")
        );
        // The stale process sees 2.0.0 < 2.3.0 < 2.5.0 in a fresh feed: refused.
        let error = run(&plan_b, bytes_b).unwrap_err();
        assert!(
            format!("{error:#}").contains("already installed"),
            "{error:#}"
        );
        assert_eq!(binary(), b"c");
        // Same version again: no reinstall, so the backup is not overwritten with a copy of C.
        let error = run(&plan_c, bytes_c).unwrap_err();
        assert!(
            format!("{error:#}").contains("already installed"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(
                dir.path()
                    .join("Regressify.app.previous/Contents/MacOS/regressify-desktop")
            )
            .unwrap(),
            b"old"
        );
        // A bundle older than the running version leaves the running version as the floor.
        let fresh = tempfile::tempdir().unwrap();
        let fresh_app = fake_installed_app(fresh.path());
        let lock = macos_install::InstallLock::acquire(&fresh_app).unwrap();
        let (same, bytes) = plan_serving("2.0.0", b"x");
        let open = serve(bytes);
        let deps = macos_install::Deps {
            open: &open,
            sign: &ok,
            verify: &ok,
            ops: &macos_install::RealOps,
        };
        assert!(
            install_locked(
                &lock,
                &fresh_app,
                cache.path(),
                current(),
                &same,
                &deps,
                &InstallHandle::new()
            )
            .is_err()
        );
    }

    #[test]
    fn failure_text_only_claims_unchanged_when_it_is() {
        use super::{InstallHandle, InstallState, perform_install};
        let dir = tempfile::tempdir().unwrap();
        let app = fake_installed_app(dir.path());
        let ok = |_: &std::path::Path| Ok(());
        let (plan, bytes) = plan_serving("2.2.2", b"n");
        let open = move |_: &str| -> anyhow::Result<Box<dyn std::io::Read>> {
            Ok(Box::new(std::io::Cursor::new(bytes.clone())))
        };
        let cache = tempfile::tempdir().unwrap();

        // Plain failure: the UI states the app is unchanged.
        let offline =
            |_: &str| -> anyhow::Result<Box<dyn std::io::Read>> { anyhow::bail!("offline") };
        let deps = macos_install::Deps {
            open: &offline,
            sign: &ok,
            verify: &ok,
            ops: &macos_install::RealOps,
        };
        let handle = InstallHandle::new();
        assert!(perform_install(&app, cache.path(), &plan, &deps, &handle).is_err());
        assert!(
            handle
                .get()
                .message()
                .ends_with("The current app was not changed.")
        );

        // Rollback failure: the message names the kept copy and must not claim "unchanged".
        struct NoRestore(std::cell::Cell<usize>);
        impl macos_install::SwapOps for NoRestore {
            fn rename(&self, from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
                std::fs::rename(from, to)
            }
            fn exchange(&self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
                self.0.set(self.0.get() + 1);
                if self.0.get() >= 2 {
                    return Err(std::io::Error::other("disk gone"));
                }
                macos_install::SwapOps::exchange(&macos_install::RealOps, a, b)
            }
        }
        let ops = NoRestore(std::cell::Cell::new(0));
        // Only the swapped-in app fails verification (stage checks pass), forcing a rollback.
        let reject_live = |path: &std::path::Path| -> anyhow::Result<()> {
            if path
                .file_name()
                .is_some_and(|name| name == "Regressify.app")
            {
                anyhow::bail!("bad signature");
            }
            Ok(())
        };
        let deps = macos_install::Deps {
            open: &open,
            sign: &ok,
            verify: &reject_live,
            ops: &ops,
        };
        let handle = InstallHandle::new();
        assert!(perform_install(&app, cache.path(), &plan, &deps, &handle).is_err());
        let InstallState::Failed(text) = handle.get() else {
            panic!("expected a failure")
        };
        assert!(
            text.contains("could not be restored")
                && text.contains(".Regressify.app.update-stage-"),
            "{text}"
        );
        assert!(!text.contains("was not changed"), "{text}");
        assert!(handle.get().message().starts_with("Failed: "));
    }

    #[test]
    fn platform_gate_selects_feeds_per_platform() {
        let origin = Some("https://regressify.eshn.dev");
        // Windows x64 and macOS (either arch maps to MacOs) get a feed; others never do.
        assert_eq!(
            UpdateFeed::for_origin(origin, UpdatePlatform::WindowsX64)
                .unwrap()
                .platform(),
            UpdatePlatform::WindowsX64
        );
        assert_eq!(
            UpdateFeed::for_origin(origin, UpdatePlatform::MacOs)
                .unwrap()
                .platform(),
            UpdatePlatform::MacOs
        );
        assert!(UpdateFeed::for_origin(origin, UpdatePlatform::Other).is_none());
        // The compile-time origin is still required, and must be a valid https origin.
        assert!(UpdateFeed::for_origin(None, UpdatePlatform::MacOs).is_none());
        assert!(UpdateFeed::for_origin(Some("http://x.dev"), UpdatePlatform::MacOs).is_none());
        // Unit tests run on Linux, which must stay unconfigured.
        if cfg!(target_os = "linux") {
            assert_eq!(UpdatePlatform::current(), UpdatePlatform::Other);
            assert!(UpdateFeed::configured().is_none());
        }
    }

    #[test]
    fn notice_manifest_accepts_the_live_shape_and_ignores_installer_fields() {
        let notice = macos_feed().parse_notice_manifest(LIVE_MANIFEST).unwrap();
        assert_eq!(notice.version, "2.2.2".parse::<Version>().unwrap());
        assert_eq!(
            notice.release_url(),
            "https://github.com/precise-alloy/regressify-rs/releases/tag/v2.2.2"
        );
        // Windows validation is unchanged: the same text still parses on the Windows path
        // only when its installer fields are valid, and a bad installer never fails a notice.
        let broken = LIVE_MANIFEST.replace("Regressify-Setup-2.2.2.0-x64.exe", "evil.exe");
        assert!(macos_feed().parse_notice_manifest(&broken).is_ok());
        assert!(
            UpdateFeed::new("https://regressify.eshn.dev")
                .unwrap()
                .parse_manifest(&broken)
                .is_err()
        );
        let json = r#"{"version":"2.2.2.0","tag":"v2.2.2","extra":1}"#;
        assert!(macos_feed().parse_notice_manifest(json).is_ok());
    }

    #[test]
    fn notice_manifest_rejects_bad_tags_versions_and_shapes() {
        let feed = macos_feed();
        for text in [
            "version=2.2.2.0\ntag=v2.2.3\n",
            "version=2.2.2.0\ntag=2.2.2\n",
            "version=2.2.2.0\ntag=v2.2.2/../x\n",
            "version=2.2.2.0\ntag=v2.2.2\ntag=v2.2.2\n",
            "version=2.2.2.0\n",
            "version=latest\ntag=vlatest\n",
            "version=2.2.2.0\ntag=v2.2\n",
            "not a manifest",
            "",
        ] {
            assert!(feed.parse_notice_manifest(text).is_err(), "{text:?}");
        }
        assert!(feed.parse_notice_manifest(&" ".repeat(20 * 1024)).is_err());
    }

    #[test]
    fn notice_responses_compare_versions_and_survive_failures() {
        let feed = macos_feed();
        let current: Version = "2.2.2".parse().unwrap();
        let check =
            |status, text: &str, current| feed.notice_response(status, text.as_bytes(), current);
        assert!(matches!(
            check(StatusCode::OK, LIVE_MANIFEST, "2.2.1".parse().unwrap()),
            Ok(NoticeCheck::Available(_))
        ));
        assert!(matches!(
            check(StatusCode::OK, LIVE_MANIFEST, current).unwrap(),
            NoticeCheck::Older(_)
        ));
        assert!(matches!(
            check(StatusCode::OK, LIVE_MANIFEST, "3.0.0".parse().unwrap()).unwrap(),
            NoticeCheck::Older(_)
        ));
        assert_eq!(
            check(StatusCode::NOT_FOUND, "", current).unwrap(),
            NoticeCheck::Current
        );
        assert!(check(StatusCode::BAD_GATEWAY, "", current).is_err());
        assert!(check(StatusCode::OK, "garbage", current).is_err());
    }

    #[test]
    fn notice_check_refuses_other_platforms_and_has_no_installer_data() {
        let windows = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        assert!(windows.check_notice("1.0.0".parse().unwrap()).is_err());
        // UpdateNotice exposes only version and a release-page URL; it has no installer,
        // hash, or payload field to download, and PreparedUpdate cannot be built from it.
        let UpdateNotice { version, tag } = notice("2.2.2.0");
        assert_eq!(
            (version.to_string(), tag.as_str()),
            ("2.2.2.0".into(), "v2.2.2")
        );
        assert_eq!(
            macos_feed().install_command(),
            "curl -fsSL https://regressify.eshn.dev/install.sh | sh"
        );
    }

    #[test]
    fn notice_schedule_dismissal_and_failures_use_a_separate_state_file() {
        let cache = tempfile::tempdir().unwrap();
        let feed = macos_feed();
        let run = |startup, now, result: Result<NoticeCheck, ()>| {
            feed.check_notice_with(cache.path(), current(), startup, true, now, || {
                result.map_err(|()| anyhow::anyhow!("offline"))
            })
        };
        let available = || Ok(NoticeCheck::Available(notice("2.2.2.0")));
        // Newer release shows; the check stamps the shared timestamp.
        assert_eq!(
            run(false, 1_000, available()).unwrap(),
            NoticeOutcome::Available(notice("2.2.2.0"))
        );
        // Not due inside eight hours: the feed is not consulted, the saved notice is shown.
        assert_eq!(
            run(false, 1_000 + 3_600, Err(())).unwrap(),
            NoticeOutcome::Available(notice("2.2.2.0"))
        );
        // Startup bypasses the deadline; failures are errors but still reschedule.
        assert!(run(true, 2_000, Err(())).is_err());
        assert_eq!(
            run(false, 2_001, Err(())).unwrap(),
            NoticeOutcome::Available(notice("2.2.2.0"))
        );
        // Due again after eight hours, and after a clock rollback.
        let after = 2_000 + super::CHECK_INTERVAL.as_secs();
        assert_eq!(
            run(false, after, Ok(NoticeCheck::Current)).unwrap(),
            NoticeOutcome::Current
        );
        assert!(run(false, 10, Err(())).is_err());
        // Dismissal hides that version and older, not newer ones.
        UpdateFeed::dismiss_notice("2.2.2".parse().unwrap(), cache.path()).unwrap();
        assert_eq!(
            run(true, 5_000, available()).unwrap(),
            NoticeOutcome::Dismissed(notice("2.2.2.0"))
        );
        assert_eq!(
            run(true, 5_001, Ok(NoticeCheck::Available(notice("2.3.0.0")))).unwrap(),
            NoticeOutcome::Available(notice("2.3.0.0"))
        );
        // Dismissing an older version later never lowers the stored one.
        UpdateFeed::dismiss_notice("2.0.0".parse().unwrap(), cache.path()).unwrap();
        assert_eq!(
            run(true, 5_002, available()).unwrap(),
            NoticeOutcome::Dismissed(notice("2.2.2.0"))
        );
        // Installer state files are untouched; a corrupt notice file is not fatal.
        assert!(!cache.path().join("state.json").exists());
        fs::write(cache.path().join("notice.json"), b"{not json").unwrap();
        assert!(NoticeState::load(cache.path()).is_err());
        assert_eq!(
            run(true, 6_000, available()).unwrap(),
            NoticeOutcome::Available(notice("2.2.2.0"))
        );
    }

    #[test]
    fn notice_found_by_one_window_reaches_a_window_that_is_not_due() {
        let cache = tempfile::tempdir().unwrap();
        let feed = macos_feed();
        let never = || -> Result<NoticeCheck, anyhow::Error> { panic!("peer must not request") };
        // Window A's due check finds a release and saves it.
        let found = feed
            .check_notice_with(cache.path(), current(), true, true, 1_000, || {
                Ok(NoticeCheck::Available(notice("2.2.2.0")))
            })
            .unwrap();
        assert_eq!(found, NoticeOutcome::Available(notice("2.2.2.0")));
        // Window B polls inside the shared interval: no request, but it learns the release.
        let polled = feed
            .check_notice_with(cache.path(), current(), false, true, 1_005, never)
            .unwrap();
        assert_eq!(polled, NoticeOutcome::Available(notice("2.2.2.0")));
        // Window A dismisses; window B's next poll hides it.
        UpdateFeed::dismiss_notice("2.2.2".parse().unwrap(), cache.path()).unwrap();
        let polled = feed
            .check_notice_with(cache.path(), current(), false, true, 1_010, never)
            .unwrap();
        assert_eq!(polled, NoticeOutcome::Dismissed(notice("2.2.2.0")));
        // A saved notice that is no longer newer (the app was upgraded) is Current.
        let upgraded: Version = "2.2.2".parse().unwrap();
        let polled = feed
            .check_notice_with(cache.path(), upgraded, false, true, 1_015, never)
            .unwrap();
        assert_eq!(polled, NoticeOutcome::Current);
        // A tampered saved notice is revalidated and ignored.
        fs::write(
            cache.path().join("notice.json"),
            br#"{"last_check_at":1000,"available_version":"2.2.2.0","available_tag":"v9.9.9"}"#,
        )
        .unwrap();
        let polled = feed
            .check_notice_with(cache.path(), current(), false, true, 1_020, never)
            .unwrap();
        assert_eq!(polled, NoticeOutcome::NotDue);
    }

    #[test]
    fn dismissal_during_a_pending_check_is_not_overwritten() {
        let cache = tempfile::tempdir().unwrap();
        let feed = macos_feed();
        let outcome = feed
            .check_notice_with(cache.path(), current(), true, true, 1_000, || {
                // The user dismisses in another window while the request is in flight.
                UpdateFeed::dismiss_notice("2.2.2".parse().unwrap(), cache.path()).unwrap();
                Ok(NoticeCheck::Available(notice("2.2.2.0")))
            })
            .unwrap();
        assert_eq!(outcome, NoticeOutcome::Dismissed(notice("2.2.2.0")));
        let state = NoticeState::load(cache.path()).unwrap();
        assert_eq!(state.dismissed_version.as_deref(), Some("2.2.2.0"));
        assert_eq!(state.last_check_at, Some(1_000));
        // The dismissal survives the next check as well.
        let again = feed
            .check_notice_with(cache.path(), current(), true, true, 2_000, || {
                Ok(NoticeCheck::Available(notice("2.2.2.0")))
            })
            .unwrap();
        assert_eq!(again, NoticeOutcome::Dismissed(notice("2.2.2.0")));
    }

    #[test]
    fn a_slower_earlier_check_cannot_overwrite_a_newer_one() {
        let cache = tempfile::tempdir().unwrap();
        let feed = macos_feed();
        let outcome = feed
            .check_notice_with(cache.path(), current(), true, true, 1_000, || {
                // A newer check starts and finishes while this older request is in flight.
                let newer = feed
                    .check_notice_with(cache.path(), current(), true, true, 2_000, || {
                        Ok(NoticeCheck::Available(notice("2.3.0.0")))
                    })
                    .unwrap();
                assert_eq!(newer, NoticeOutcome::Available(notice("2.3.0.0")));
                // The user dismisses the newer release; the late response must keep that too.
                UpdateFeed::dismiss_notice("2.1.0".parse().unwrap(), cache.path()).unwrap();
                Ok(NoticeCheck::Available(notice("2.2.2.0")))
            })
            .unwrap();
        assert_eq!(outcome, NoticeOutcome::Available(notice("2.3.0.0")));
        let state = NoticeState::load(cache.path()).unwrap();
        assert_eq!(state.available_version.as_deref(), Some("2.3.0.0"));
        assert_eq!(
            state.last_check_at,
            Some(2_000),
            "the clock does not move back"
        );
        assert_eq!(state.dismissed_version.as_deref(), Some("2.1.0.0"));
    }

    #[test]
    fn a_failed_newer_request_does_not_supersede_a_successful_older_one() {
        let cache = tempfile::tempdir().unwrap();
        let feed = macos_feed();
        let outcome = feed
            .check_notice_with(cache.path(), current(), true, true, 1_000, || {
                // Two startup checks overlap: the newer ticket fails first.
                let newer =
                    feed.check_notice_with(cache.path(), current(), true, true, 1_001, || {
                        Err(anyhow::anyhow!("offline"))
                    });
                assert!(newer.is_err());
                Ok(NoticeCheck::Available(notice("2.2.2.0")))
            })
            .unwrap();
        assert_eq!(
            outcome,
            NoticeOutcome::Available(notice("2.2.2.0")),
            "the confirmed release is not hidden until the next interval"
        );
        let state = NoticeState::load(cache.path()).unwrap();
        assert_eq!(state.available_version.as_deref(), Some("2.2.2.0"));
        assert_eq!(
            state.applied_seq, 1,
            "only the successful ticket is applied"
        );
        assert!(state.started_seq >= state.applied_seq);
    }

    #[test]
    fn a_failed_ticket_save_cannot_discard_a_newer_peers_response() {
        let cache = tempfile::tempdir().unwrap();
        let feed = macos_feed();
        let state_path = cache.path().join("notice.json");
        // A directory in the state file's place: loading yields defaults and every save fails,
        // so window A's ticket cannot be recorded.
        fs::create_dir(&state_path).unwrap();
        let (got_tx, got_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let go_rx = std::sync::Mutex::new(go_rx);
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let a = feed.check_notice_with(cache.path(), current(), true, true, 1_000, || {
                // Writes recover; window B starts (and records the ticket A could not) and is
                // still waiting for its response when A's older response arrives.
                fs::remove_dir(&state_path).unwrap();
                scope.spawn(|| {
                    let outcome =
                        feed.check_notice_with(cache.path(), current(), true, true, 1_001, || {
                            got_tx.send(()).unwrap();
                            go_rx.lock().unwrap().recv().unwrap();
                            Ok(NoticeCheck::Available(notice("2.3.0.0")))
                        });
                    result_tx.send(outcome.unwrap()).unwrap();
                });
                got_rx.recv().unwrap();
                Ok(NoticeCheck::Available(notice("2.2.2.0")))
            });
            assert!(a.is_ok());
            go_tx.send(()).unwrap();
            assert_eq!(
                result_rx.recv().unwrap(),
                NoticeOutcome::Available(notice("2.3.0.0"))
            );
        });
        let state = NoticeState::load(cache.path()).unwrap();
        assert_eq!(
            state.available_version.as_deref(),
            Some("2.3.0.0"),
            "the newer response wins: {state:?}"
        );
        assert!(state.started_seq >= state.applied_seq, "{state:?}");
    }

    #[test]
    fn a_no_release_result_clears_the_banner_of_a_peer_window() {
        let cache = tempfile::tempdir().unwrap();
        let feed = macos_feed();
        let never = || -> Result<NoticeCheck, anyhow::Error> { panic!("peer must not request") };
        // Nothing has ever succeeded: an empty state stays unknown, not "current".
        let unknown = feed
            .check_notice_with(cache.path(), current(), false, false, 1_000, never)
            .unwrap();
        assert_eq!(unknown, NoticeOutcome::NotDue);
        feed.check_notice_with(cache.path(), current(), true, true, 1_000, || {
            Ok(NoticeCheck::Available(notice("2.2.2.0")))
        })
        .unwrap();
        // Both windows show the release; one then receives "no release" (HTTP 404).
        feed.check_notice_with(cache.path(), current(), true, true, 2_000, || {
            Ok(NoticeCheck::Current)
        })
        .unwrap();
        let peer = feed
            .check_notice_with(cache.path(), current(), false, true, 2_010, never)
            .unwrap();
        assert_eq!(
            peer,
            NoticeOutcome::Current,
            "the peer drops its stale banner"
        );
    }

    #[test]
    fn an_updated_window_keeps_the_release_for_older_peers() {
        let cache = tempfile::tempdir().unwrap();
        let feed = macos_feed();
        let updated: Version = "2.2.2".parse().unwrap();
        let older: Version = "2.1.0".parse().unwrap();
        // The updated window checks first; its feed release is not newer than itself.
        let outcome = feed
            .check_notice_with(cache.path(), updated, true, true, 1_000, || {
                Ok(NoticeCheck::Older(notice("2.2.2.0")))
            })
            .unwrap();
        assert_eq!(outcome, NoticeOutcome::Current);
        // The older peer is not due, but still learns about the release, per its own version.
        let never = || -> Result<NoticeCheck, anyhow::Error> { panic!("peer must not request") };
        let peer = feed
            .check_notice_with(cache.path(), older, false, true, 1_010, never)
            .unwrap();
        assert_eq!(peer, NoticeOutcome::Available(notice("2.2.2.0")));
        let same = feed
            .check_notice_with(cache.path(), updated, false, true, 1_010, never)
            .unwrap();
        assert_eq!(same, NoticeOutcome::Current);
    }

    #[test]
    fn failed_state_save_still_reports_the_check_and_the_throttle_holds() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the cache directory should be: nothing can be saved.
        let cache = dir.path().join("updates");
        fs::write(&cache, b"not a directory").unwrap();
        let feed = macos_feed();
        let outcome = feed
            .check_notice_with(&cache, current(), true, true, 1_000, || {
                Ok(NoticeCheck::Available(notice("2.2.2.0")))
            })
            .unwrap();
        assert_eq!(outcome, NoticeOutcome::Available(notice("2.2.2.0")));
        // A failing check still surfaces its own error rather than the save error.
        let error = feed
            .check_notice_with(&cache, current(), true, true, 1_001, || {
                Err(anyhow::anyhow!("offline"))
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "offline");
        // Nothing was persisted, so the caller's in-memory throttle must stop the request.
        let held = feed
            .check_notice_with(&cache, current(), false, false, 1_002, || {
                panic!("throttled caller must not request")
            })
            .unwrap();
        assert_eq!(held, NoticeOutcome::NotDue);
    }

    #[test]
    fn only_a_dispatched_request_counts_as_an_attempt() {
        let cache = tempfile::tempdir().unwrap();
        let feed = macos_feed();
        let never = || panic!("must not request");
        let attempt = feed.check_notice_attempt(cache.path(), current(), true, true, 1_000, || {
            Ok(NoticeCheck::Available(notice("2.2.2.0")))
        });
        assert!(attempt.dispatched);
        // Cached polls (throttle closed) report the saved notice but never an attempt, so the
        // caller's throttle is not refreshed and can reopen at its deadline.
        for now in [1_005, 1_010] {
            let attempt =
                feed.check_notice_attempt(cache.path(), current(), false, false, now, never);
            assert!(!attempt.dispatched);
            assert_eq!(
                attempt.outcome.unwrap(),
                NoticeOutcome::Available(notice("2.2.2.0"))
            );
        }
        UpdateFeed::dismiss_notice("2.2.2".parse().unwrap(), cache.path()).unwrap();
        let attempt =
            feed.check_notice_attempt(cache.path(), current(), false, false, 1_015, never);
        assert!(!attempt.dispatched);
        assert_eq!(
            attempt.outcome.unwrap(),
            NoticeOutcome::Dismissed(notice("2.2.2.0"))
        );
        // Allowed by the caller but not yet due on disk: still no request.
        let attempt = feed.check_notice_attempt(cache.path(), current(), false, true, 1_020, never);
        assert!(!attempt.dispatched);
        // Past the deadline the request goes out again and finds a newer release.
        let later = 1_000 + super::CHECK_INTERVAL.as_secs();
        let attempt =
            feed.check_notice_attempt(cache.path(), current(), false, true, later, || {
                Ok(NoticeCheck::Available(notice("2.3.0.0")))
            });
        assert!(attempt.dispatched);
        assert_eq!(
            attempt.outcome.unwrap(),
            NoticeOutcome::Available(notice("2.3.0.0"))
        );
        // A failed request still counts as an attempt (the 5 s retry loop stays throttled).
        let attempt =
            feed.check_notice_attempt(cache.path(), current(), true, true, later + 1, || {
                Err(anyhow::anyhow!("offline"))
            });
        assert!(attempt.dispatched);
        assert!(attempt.outcome.is_err());
    }

    #[test]
    fn cached_results_replace_stale_status_but_keep_failures() {
        let failed = format!("{CHECK_FAILED_PREFIX}offline");
        let up_to_date = "Regressify is up to date";
        // Cached `Current` after another window recorded a rolled-back release: the old
        // dismissed message must not survive, nor a blank one.
        assert_eq!(
            notice_message_after_success(false, "", up_to_date),
            up_to_date
        );
        assert_eq!(
            notice_message_after_success(false, "A newer version is available", up_to_date),
            up_to_date
        );
        // Cached `Available` must not leave a stale "up to date".
        assert_eq!(notice_message_after_success(false, up_to_date, ""), "");
        // A cached poll keeps a failure; a dispatched success clears it.
        assert_eq!(
            notice_message_after_success(false, &failed, up_to_date),
            failed
        );
        assert_eq!(
            notice_message_after_success(true, &failed, up_to_date),
            up_to_date
        );
        assert_eq!(notice_message_after_success(true, &failed, ""), "");
    }

    #[test]
    fn dismissal_rule_hides_that_version_and_older() {
        let version = |text: &str| text.parse::<Version>().unwrap();
        assert!(!dismissal_hides(None, version("2.2.2")));
        assert!(dismissal_hides(Some(version("2.2.2")), version("2.2.2")));
        assert!(dismissal_hides(Some(version("2.2.2")), version("2.0.0")));
        assert!(!dismissal_hides(Some(version("2.2.2")), version("2.3.0")));
    }

    #[test]
    fn accepts_text_and_json_manifests() {
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let text = manifest(b"installer");
        let release = feed.parse_manifest(&text).unwrap();
        assert_eq!(release.version.to_string(), "1.10.0.0");
        let json = serde_json::json!({
            "version": "1.10.0", "installer": release.installer.as_str(),
            "sha256": release.sha256.to_ascii_uppercase(), "size": release.size,
            "pub_date": "2026-09-18T00:00:00.000Z",
            "archive.regressify-desktop-x86_64-pc-windows-msvc.zip": format!("15:{:x}", Sha256::digest(b"desktop-archive")),
            "archive.regressify-cli-x86_64-pc-windows-msvc.zip": format!("11:{:x}", Sha256::digest(b"cli-archive")),
        });
        assert_eq!(feed.parse_manifest(&json.to_string()).unwrap(), release);
        assert_eq!(
            feed.parse_manifest(&format!("\u{feff}{text}")).unwrap(),
            release
        );
        let four_part_tag = text.replace("/download/v1.10.0/", "/download/v1.10.0.0/");
        assert_eq!(
            feed.parse_manifest(&four_part_tag).unwrap().version,
            release.version
        );
    }

    #[test]
    fn unpublished_equal_and_older_releases_do_not_update() {
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let text = manifest(b"installer");
        for version in ["1.10", "2.0"] {
            assert_eq!(
                feed.check_response(StatusCode::OK, text.as_bytes(), version.parse().unwrap())
                    .unwrap(),
                UpdateCheck::Current
            );
        }
        assert!(matches!(
            feed.check_response(StatusCode::OK, text.as_bytes(), "1.9".parse().unwrap())
                .unwrap(),
            UpdateCheck::Available(_)
        ));
        assert_eq!(
            feed.check_response(StatusCode::NOT_FOUND, b"".as_slice(), "1".parse().unwrap())
                .unwrap(),
            UpdateCheck::Unpublished
        );
        for status in [
            StatusCode::FOUND,
            StatusCode::UNAUTHORIZED,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(
                feed.check_response(status, text.as_bytes(), "1".parse().unwrap())
                    .is_err()
            );
        }
    }

    #[test]
    fn rejects_untrusted_urls_and_ambiguous_manifests() {
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let text = manifest(b"installer");
        for invalid in [
            text.replace("https://github.com", "http://github.com"),
            text.replace("github.com/", "evil.example/"),
            text.replace("github.com/", "github.com.evil.example/"),
            text.replace("github.com/", "github.com:8443/"),
            text.replace("https://github.com", "https://GITHUB.COM"),
            text.replace("/precise-alloy/regressify-rs/", "/other/repo/"),
            text.replace("/releases/download/", "/releases/tag/"),
            text.replace("/download/v1.10.0/", "/download/../v1.10.0/"),
            text.replace("/download/v1.10.0/", "/download/nested/v1.10.0/"),
            text.replace("/download/v1.10.0/", "/download/1.10.0/"),
            text.replace("/download/v1.10.0/", "/download/v1.10/"),
            text.replace("/download/v1.10.0/", "/download/v1.11.0/"),
            text.replace("-x64.exe", "-x64.exe?redirect=elsewhere"),
            text.replace("-x64.exe", "-x64.exe#fragment"),
            text.replace(
                "Regressify-Setup-1.10.0.0-x64.exe",
                "Regressify-Setup-1.11.0.0-x64.exe",
            ),
            text.replace(
                "Regressify-Setup-1.10.0.0-x64.exe",
                "Regressify-Desktop-Setup-1.10.0.0-x64.exe",
            ),
            text.replace("Regressify-Setup-1.10.0.0-x64.exe", "latest/installer"),
            text.replace("size=9", "size=0"),
            text.replace("size=9", "size=1073741825"),
            format!("{text}version=2\n"),
            text.replace("sha256=", "hash="),
            "x".repeat(16 * 1024 + 1),
        ] {
            assert!(feed.parse_manifest(&invalid).is_err(), "{invalid}");
        }
        for origin in [
            "",
            "regressify.eshn.dev",
            "http://regressify.eshn.dev",
            "https://regressify.eshn.dev/",
            "https://regressify.eshn.dev/path",
            "https://regressify.eshn.dev:8443",
            "https://user@regressify.eshn.dev",
            "https://Regressify.eshn.dev",
            "https://.eshn.dev",
            "https://regressify..dev",
            "https://regressify.eshn.dev?x",
            "https://regressify.eshn.dev\n",
        ] {
            assert!(UpdateFeed::new(origin).is_err(), "{origin}");
        }
        assert_eq!(
            feed.manifest_url.as_str(),
            "https://regressify.eshn.dev/downloads/latest.txt"
        );
        for url in [
            "https://objects.githubusercontent.com/github-production-release-asset-2e65be/installer.exe",
            "https://release-assets.githubusercontent.com/github-production-release-asset/installer.exe",
            "https://github.com/precise-alloy/regressify-rs/releases/download/v1.10.0/Regressify-Setup-1.10.0.0-x64.exe",
        ] {
            assert!(
                is_allowed_installer_redirect(&url.parse().unwrap(), 0).is_ok(),
                "{url}"
            );
            assert!(
                is_allowed_installer_redirect(&url.parse().unwrap(), 5).is_err(),
                "{url}"
            );
        }
        for invalid in [
            "http://objects.githubusercontent.com/installer.exe",
            "https://evil.example/installer.exe",
            "https://githubusercontent.com/installer.exe",
            "https://objects.githubusercontent.com.evil.example/installer.exe",
            "https://.githubusercontent.com/installer.exe",
            "https://objects..githubusercontent.com/installer.exe",
            "https://objects.githubusercontent.com:8443/installer.exe",
            "https://user@objects.githubusercontent.com/installer.exe",
            "https://objects.githubusercontent.com/installer.exe#fragment",
        ] {
            assert!(
                is_allowed_installer_redirect(&invalid.parse().unwrap(), 0).is_err(),
                "{invalid}"
            );
        }
    }
    #[test]
    fn payload_must_match_both_size_and_digest() {
        let payload = b"installer";
        let release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(payload))
            .unwrap();
        let mut written = Vec::new();
        verify_payload(payload.as_slice(), &mut written, &release).unwrap();
        assert_eq!(written, payload);
        for invalid in [
            b"installed".as_slice(),
            b"short",
            b"installer with extra bytes",
        ] {
            assert!(verify_payload(invalid, std::io::sink(), &release).is_err());
        }
    }

    #[test]
    fn payload_io_errors_identify_the_failed_stage_and_received_bytes() {
        use std::io::{self, Read, Write};

        struct FailedIo;

        impl Read for FailedIo {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "connection stalled",
                ))
            }
        }

        impl Write for FailedIo {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk is full"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let payload = b"installer";
        let release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(payload))
            .unwrap();
        let error =
            verify_payload(b"ins".as_slice().chain(FailedIo), io::sink(), &release).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("after 3 of 9 bytes"), "{message}");
        assert!(message.contains("connection stalled"), "{message}");
        let error = verify_payload(payload.as_slice(), FailedIo, &release).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("Cannot save the installer"), "{message}");
        assert!(message.contains("disk is full"), "{message}");
    }

    async fn download_server(
        response: &str,
    ) -> (
        reqwest::Url,
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Receiver<String>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/installer", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let response = response.to_owned();
        let (started, request_received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
                assert!(request.len() < 16 * 1024);
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            let _ = started.send(String::from_utf8(request).unwrap());
            futures::future::pending::<()>().await;
        });
        (url, server, request_received)
    }

    #[tokio::test]
    async fn shared_download_lease_reuses_one_transfer_across_owners() {
        use super::{SharedUpdateCheck, UpdateLease};

        let (url, server, request) =
            download_server("HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\ninstaller").await;
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let mut release = feed.parse_manifest(&manifest(b"installer")).unwrap();
        release.installer = url;
        let cache = tempfile::tempdir().unwrap();
        let lease = UpdateLease::acquire(cache.path()).unwrap().unwrap();
        assert!(matches!(
            feed.check_shared_with(
                "1.0".parse().unwrap(),
                cache.path(),
                CheckMode::Scheduled,
                None,
                super::unix_timestamp(),
                || panic!("Peer issued a feed request")
            )
            .unwrap(),
            SharedUpdateCheck::Busy
        ));
        let first = PreparedUpdate::download_with_client(
            release.clone(),
            cache.path(),
            &DownloadControl::new(&release),
            reqwest::Client::new(),
            Duration::from_secs(2),
            Duration::from_secs(5),
        )
        .await;
        server.abort();
        let _ = server.await;
        assert!(request.await.unwrap().starts_with("GET /installer "));
        let first = first.unwrap();
        drop(lease);
        let _peer = UpdateLease::acquire(cache.path()).unwrap().unwrap();
        let second = PreparedUpdate::download_with_client(
            release.clone(),
            cache.path(),
            &DownloadControl::new(&release),
            reqwest::Client::new(),
            Duration::from_millis(200),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(first.installer, second.installer);
        assert_eq!(fs::read(second.installer).unwrap(), b"installer");
    }

    #[tokio::test]
    async fn stalled_headers_and_body_time_out_and_keep_partial_downloads() {
        for (response, received, expected_error) in [
            ("", 0, "Update server did not respond"),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nins",
                3,
                "Download stalled",
            ),
        ] {
            let (url, server, _) = download_server(response).await;
            let mut release = UpdateFeed::new("https://regressify.eshn.dev")
                .unwrap()
                .parse_manifest(&manifest(b"installer"))
                .unwrap();
            release.installer = url;
            let control = DownloadControl::new(&release);
            let cache = tempfile::tempdir().unwrap();
            let partial = cached_installer(&release, cache.path()).with_extension("exe.part");
            let result = PreparedUpdate::download_with_client(
                release,
                cache.path(),
                &control,
                reqwest::Client::new(),
                Duration::from_millis(200),
                Duration::from_secs(3),
            )
            .await;
            server.abort();
            let error = format!("{:#}", result.err().expect("stalled download must fail"));
            assert!(error.contains(expected_error), "{error}");
            assert!(error.contains("retry"), "{error}");
            assert_eq!(control.progress().received, received);
            assert_eq!(fs::metadata(partial).unwrap().len(), received);
            assert_eq!(UpdateState::load(cache.path()).unwrap().hash_failures, 0);
            if received > 0 {
                assert!(error.contains("3 of 9 bytes"), "{error}");
            }
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_waiting_for_the_server_and_keeps_partial_download() {
        let (url, server, started) = download_server("").await;
        let mut release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(b"installer"))
            .unwrap();
        release.installer = url;
        let control = DownloadControl::new(&release);
        let cache = tempfile::tempdir().unwrap();
        let partial = cached_installer(&release, cache.path()).with_extension("exe.part");
        let download = PreparedUpdate::download_with_client(
            release,
            cache.path(),
            &control,
            reqwest::Client::new(),
            Duration::from_secs(30),
            Duration::from_secs(60),
        );
        let cancel = async {
            started.await.unwrap();
            control.cancel();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(download, cancel)
        })
        .await
        .expect("cancellation must not wait for the network timeout");
        server.abort();
        let error = format!("{:#}", result.err().expect("cancelled download must fail"));
        assert!(error.contains("Download cancelled"), "{error}");
        assert!(control.is_cancelled());
        assert_eq!(fs::metadata(partial).unwrap().len(), 0);
    }

    #[tokio::test]
    async fn streamed_installer_still_requires_success_size_and_sha256() {
        for (response, expected) in [
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\ninstaller",
                None,
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nins",
                Some("Content-Length"),
            ),
            (
                "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n",
                Some("HTTP 503"),
            ),
            (
                "HTTP/1.1 302 Found\r\nLocation: https://example.com/\r\nContent-Length: 0\r\n\r\n",
                Some("redirected"),
            ),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nins\r\n0\r\n\r\n",
                Some("incomplete"),
            ),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nA\r\ninstallers\r\n0\r\n\r\n",
                Some("exceeds"),
            ),
        ] {
            let (url, server, _) = download_server(response).await;
            let mut release = UpdateFeed::new("https://regressify.eshn.dev")
                .unwrap()
                .parse_manifest(&manifest(b"installer"))
                .unwrap();
            release.installer = url;
            let control = DownloadControl::new(&release);
            let cache = tempfile::tempdir().unwrap();
            let installer = cached_installer(&release, cache.path());
            let client = reqwest::Client::builder()
                .redirect(installer_redirect_policy())
                .build()
                .unwrap();
            let result = PreparedUpdate::download_with_client(
                release,
                cache.path(),
                &control,
                client,
                Duration::from_secs(2),
                Duration::from_secs(3),
            )
            .await;
            server.abort();
            match expected {
                Some(expected) => {
                    let error = format!("{:#}", result.err().expect("invalid download must fail"));
                    assert!(error.contains(expected), "{error}");
                    assert!(!installer.exists());
                }
                None => {
                    let prepared = result.unwrap();
                    assert_eq!(fs::read(&prepared.installer).unwrap(), b"installer");
                    assert_eq!(control.progress().received, 9);
                    assert_eq!(control.progress().phase, DownloadPhase::Saving);
                    drop(prepared);
                    assert_eq!(fs::read(&installer).unwrap(), b"installer");
                }
            }
        }
    }

    #[tokio::test]
    async fn partial_download_resumes_and_verified_installer_is_reused_offline() {
        let (url, server, request) = download_server(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 6\r\nContent-Range: bytes 3-8/9\r\n\r\ntaller",
        ).await;
        let mut release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(b"installer"))
            .unwrap();
        release.installer = url;
        let cache = tempfile::tempdir().unwrap();
        let installer = cached_installer(&release, cache.path());
        fs::create_dir_all(installer.parent().unwrap()).unwrap();
        let partial = installer.with_extension("exe.part");
        fs::write(&partial, b"ins").unwrap();
        for _ in 0..2 {
            let control = DownloadControl::new(&release);
            let prepared = PreparedUpdate::download_with_client(
                release.clone(),
                cache.path(),
                &control,
                reqwest::Client::new(),
                Duration::from_millis(200),
                Duration::from_secs(3),
            )
            .await
            .unwrap();
            assert_eq!(fs::read(&prepared.installer).unwrap(), b"installer");
            assert!(!partial.exists());
            drop(prepared);
            server.abort();
        }
        let request = request.await.unwrap().to_ascii_lowercase();
        assert!(request.contains("range: bytes=3-"), "{request}");
    }

    #[tokio::test]
    async fn resume_restarts_when_ranges_are_ignored_and_rejects_wrong_ranges() {
        for (response, success) in [
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\ninstaller",
                true,
            ),
            (
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 6\r\nContent-Range: bytes 2-7/9\r\n\r\ntaller",
                false,
            ),
            (
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 6\r\n\r\ntaller",
                false,
            ),
        ] {
            let (url, server, _) = download_server(response).await;
            let mut release = UpdateFeed::new("https://regressify.eshn.dev")
                .unwrap()
                .parse_manifest(&manifest(b"installer"))
                .unwrap();
            release.installer = url;
            let cache = tempfile::tempdir().unwrap();
            let installer = cached_installer(&release, cache.path());
            fs::create_dir_all(installer.parent().unwrap()).unwrap();
            let partial = installer.with_extension("exe.part");
            fs::write(&partial, b"ins").unwrap();
            let result = PreparedUpdate::download_with_client(
                release.clone(),
                cache.path(),
                &DownloadControl::new(&release),
                reqwest::Client::new(),
                Duration::from_millis(200),
                Duration::from_secs(3),
            )
            .await;
            server.abort();
            if success {
                assert_eq!(fs::read(result.unwrap().installer).unwrap(), b"installer");
            } else {
                assert!(result.err().unwrap().to_string().contains("Content-Range"));
                assert_eq!(fs::read(partial).unwrap(), b"ins");
            }
        }
    }

    async fn download_sequence(
        responses: &[&str],
    ) -> (reqwest::Url, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/installer", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let responses: Vec<_> = responses
            .iter()
            .map(|response| response.to_string())
            .collect();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                    assert!(request.len() < 16 * 1024);
                }
                requests.push(String::from_utf8(request).unwrap());
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
            requests
        });
        (url, server)
    }

    #[tokio::test]
    async fn checksum_failure_schedules_a_fresh_download_and_releases_the_cache_lock() {
        let (url, server) = download_sequence(&[
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\ninstalled",
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\ninstaller",
        ])
        .await;
        let mut release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(b"installer"))
            .unwrap();
        release.installer = url;
        let cache = tempfile::tempdir().unwrap();
        let error = PreparedUpdate::download_with_client(
            release.clone(),
            cache.path(),
            &DownloadControl::new(&release),
            reqwest::Client::new(),
            Duration::from_millis(200),
            Duration::from_secs(3),
        )
        .await
        .err()
        .unwrap();
        let scheduled = error.downcast_ref::<super::RetryScheduled>().unwrap();
        assert!(scheduled.delay() <= super::HASH_RETRY_DELAY);
        assert!(scheduled.delay() >= super::HASH_RETRY_DELAY - Duration::from_secs(2));
        assert!(lock_cache(cache.path()).is_ok());
        let error = PreparedUpdate::download_with_client(
            release.clone(),
            cache.path(),
            &DownloadControl::new(&release),
            reqwest::Client::new(),
            Duration::from_millis(200),
            Duration::from_secs(3),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.downcast_ref::<super::RetryScheduled>().unwrap(),
            scheduled
        );
        let mut state = UpdateState::load(cache.path()).unwrap();
        state.next_retry_at = Some(0);
        state.save(cache.path()).unwrap();
        let prepared = PreparedUpdate::download_with_client(
            release.clone(),
            cache.path(),
            &DownloadControl::new(&release),
            reqwest::Client::new(),
            Duration::from_millis(200),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert_eq!(fs::read(prepared.installer).unwrap(), b"installer");
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| !request.to_ascii_lowercase().contains("range:"))
        );
        assert_eq!(UpdateState::load(cache.path()).unwrap().hash_failures, 1);
        assert_eq!(UpdateState::load(cache.path()).unwrap().next_retry_at, None);
    }

    #[tokio::test]
    async fn checksum_retries_persist_across_restarts_and_skip_until_a_newer_version() {
        let wrong = "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\ninstalled";
        let unavailable = "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n";
        let (url, server) = download_sequence(&[
            wrong,
            unavailable,
            wrong,
            unavailable,
            wrong,
            unavailable,
            wrong,
        ])
        .await;
        let mut release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(b"installer"))
            .unwrap();
        release.installer = url;
        let cache = tempfile::tempdir().unwrap();
        for failure in 1..=4 {
            let before = super::unix_timestamp();
            let error = PreparedUpdate::download_with_client(
                release.clone(),
                cache.path(),
                &DownloadControl::new(&release),
                reqwest::Client::new(),
                Duration::from_millis(200),
                Duration::from_secs(3),
            )
            .await
            .err()
            .expect("invalid update must not be prepared");
            if failure < 4 {
                let retry = error.downcast_ref::<super::RetryScheduled>().unwrap();
                let delay = 2 * 60 * 60 * (1u64 << (failure - 1));
                assert!(retry.retry_at >= before + delay);
                assert!(retry.retry_at <= super::unix_timestamp() + delay);
                let mut state = UpdateState::load(cache.path()).unwrap();
                assert_eq!(state.next_retry_at, Some(retry.retry_at));
                state.next_retry_at = Some(0);
                state.save(cache.path()).unwrap();
                let error = PreparedUpdate::download_with_client(
                    release.clone(),
                    cache.path(),
                    &DownloadControl::new(&release),
                    reqwest::Client::new(),
                    Duration::from_millis(200),
                    Duration::from_secs(3),
                )
                .await
                .err()
                .unwrap();
                assert!(error.to_string().contains("HTTP 503"), "{error:#}");
            } else {
                assert!(error.is::<SkippedUpdate>(), "{error:#}");
            }
            assert_eq!(
                UpdateState::load(cache.path()).unwrap().hash_failures,
                failure
            );
        }
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 7);
        assert!(
            requests
                .iter()
                .all(|request| !request.to_ascii_lowercase().contains("range:"))
        );
        assert!(
            !cached_installer(&release, cache.path())
                .with_extension("exe.part")
                .exists()
        );
        for version in ["1.9", "1.10"] {
            let mut skipped = release.clone();
            skipped.version = version.parse().unwrap();
            skipped.sha256 = "a".repeat(64);
            let error = PreparedUpdate::download_with_client(
                skipped.clone(),
                cache.path(),
                &DownloadControl::new(&skipped),
                reqwest::Client::new(),
                Duration::from_millis(200),
                Duration::from_secs(3),
            )
            .await
            .err()
            .unwrap();
            assert!(error.is::<SkippedUpdate>(), "{error:#}");
        }
        let (url, server) =
            download_sequence(&["HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\ninstaller"]).await;
        release.version = "1.11".parse().unwrap();
        release.installer = url;
        let prepared = PreparedUpdate::download_with_client(
            release.clone(),
            cache.path(),
            &DownloadControl::new(&release),
            reqwest::Client::new(),
            Duration::from_millis(200),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert_eq!(prepared.version(), release.version);
        assert_eq!(UpdateState::load(cache.path()).unwrap().hash_failures, 0);
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn interrupted_verification_resumes_from_saved_manifest_without_network() {
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let release = feed.parse_manifest(&manifest(b"installer")).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let installer = cached_installer(&release, cache.path());
        fs::create_dir_all(installer.parent().unwrap()).unwrap();
        fs::write(installer.with_extension("exe.part"), b"installer").unwrap();
        UpdateState {
            release: Some(ManifestFields::from(&release)),
            ..UpdateState::default()
        }
        .save(cache.path())
        .unwrap();
        assert_eq!(
            feed.reconcile_check(
                "1".parse().unwrap(),
                cache.path(),
                Err(anyhow::anyhow!("Update service unavailable")),
            )
            .unwrap(),
            UpdateCheck::Available(release.clone())
        );
        let prepared = PreparedUpdate::download_with_client(
            release.clone(),
            cache.path(),
            &DownloadControl::new(&release),
            reqwest::Client::builder()
                .https_only(false)
                .build()
                .unwrap(),
            Duration::from_millis(1),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert_eq!(fs::read(prepared.installer).unwrap(), b"installer");
    }

    #[tokio::test]
    async fn interruption_after_recording_a_checksum_failure_does_not_count_it_twice() {
        let (url, server) =
            download_sequence(&["HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\ninstaller"]).await;
        let mut release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(b"installer"))
            .unwrap();
        release.installer = url;
        let cache = tempfile::tempdir().unwrap();
        let installer = cached_installer(&release, cache.path());
        fs::create_dir_all(installer.parent().unwrap()).unwrap();
        fs::write(installer.with_extension("exe.part"), b"installed").unwrap();
        UpdateState {
            release: Some(ManifestFields::from(&release)),
            hash_failures: 1,
            discard_payload: true,
            skipped_version: None,
            next_retry_at: None,
            ..UpdateState::default()
        }
        .save(cache.path())
        .unwrap();
        PreparedUpdate::download_with_client(
            release.clone(),
            cache.path(),
            &DownloadControl::new(&release),
            reqwest::Client::new(),
            Duration::from_millis(200),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert_eq!(UpdateState::load(cache.path()).unwrap().hash_failures, 1);
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[test]
    fn update_cache_has_one_writer_and_revalidates_saved_manifest_urls() {
        let cache = tempfile::tempdir().unwrap();
        let lock = lock_cache(cache.path()).unwrap();
        assert!(lock_cache(cache.path()).is_err());
        drop(lock);
        assert!(lock_cache(cache.path()).is_ok());
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let mut fields =
            ManifestFields::from(&feed.parse_manifest(&manifest(b"installer")).unwrap());
        fields.installer = "https://untrusted.example/installer.exe".into();
        UpdateState {
            release: Some(fields),
            ..UpdateState::default()
        }
        .save(cache.path())
        .unwrap();
        let error = feed
            .reconcile_check("1".parse().unwrap(), cache.path(), Ok(UpdateCheck::Current))
            .unwrap_err();
        assert!(error.to_string().contains("versioned HTTPS"), "{error:#}");
    }

    #[test]
    fn newer_release_discards_partial_and_complete_older_installers() {
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let previous = feed.parse_manifest(&manifest(b"installer")).unwrap();
        let latest = feed
            .parse_manifest(&manifest(b"installer").replace("1.10.0", "1.11.0"))
            .unwrap();
        let older = feed
            .parse_manifest(&manifest(b"installer").replace("1.10.0", "1.9.0"))
            .unwrap();
        let future = feed
            .parse_manifest(&manifest(b"installer").replace("1.10.0", "1.12.0"))
            .unwrap();
        for release in [&older, &previous, &latest, &future] {
            let installer = cached_installer(release, cache.path());
            fs::create_dir_all(installer.parent().unwrap()).unwrap();
            fs::write(installer.with_extension("exe.part"), b"ins").unwrap();
            fs::write(&installer, b"installer").unwrap();
        }
        let unrelated = cache.path().join("1.0.0.0-not-an-installer-cache");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(unrelated.join("keep.txt"), b"keep").unwrap();
        UpdateState {
            release: Some(ManifestFields::from(&previous)),
            hash_failures: 2,
            next_retry_at: Some(u64::MAX),
            ..UpdateState::default()
        }
        .save(cache.path())
        .unwrap();
        assert_eq!(
            feed.reconcile_check(
                "1".parse().unwrap(),
                cache.path(),
                Ok(UpdateCheck::Available(latest.clone())),
            )
            .unwrap(),
            UpdateCheck::Available(latest.clone())
        );
        for release in [&older, &previous] {
            assert!(
                !cached_installer(release, cache.path())
                    .parent()
                    .unwrap()
                    .exists()
            );
        }
        for release in [&latest, &future] {
            assert!(cached_installer(release, cache.path()).exists());
        }
        assert!(unrelated.join("keep.txt").exists());
        let mut state = UpdateState::load(cache.path()).unwrap();
        assert_eq!(
            state.release.as_ref().unwrap().version,
            latest.version.to_string()
        );
        assert_eq!(state.hash_failures, 0);
        assert_eq!(state.next_retry_at, None);
        assert!(state.select_release(&previous, cache.path()).is_err());
    }

    #[test]
    fn manual_check_resets_backoff_and_exhausted_retry_budgets_even_offline() {
        let feed = UpdateFeed::offline_for_test("https://regressify.eshn.dev").unwrap();
        let release = feed.parse_manifest(&manifest(b"installer")).unwrap();
        let cache = tempfile::tempdir().unwrap();
        for failures in [1, 4] {
            UpdateState {
                release: Some(ManifestFields::from(&release)),
                hash_failures: failures,
                next_retry_at: Some(super::unix_timestamp() + 7200),
                skipped_version: (failures == 4).then(|| release.version.to_string()),
                ..UpdateState::default()
            }
            .save(cache.path())
            .unwrap();
            let error = feed
                .check_with_cache("1".parse().unwrap(), cache.path())
                .unwrap_err();
            if failures == 4 {
                assert!(error.is::<SkippedUpdate>(), "{error:#}");
            } else {
                assert!(error.is::<super::RetryScheduled>(), "{error:#}");
            }
            assert_eq!(
                feed.check_manually("1".parse().unwrap(), cache.path())
                    .unwrap(),
                UpdateCheck::Available(release.clone())
            );
            let state = UpdateState::load(cache.path()).unwrap();
            assert_eq!(state.hash_failures, 0);
            assert_eq!(state.next_retry_at, None);
            assert_eq!(state.skipped_version, None);
        }
    }

    #[test]
    fn existing_cache_state_without_a_retry_deadline_remains_readable() {
        let state: UpdateState = serde_json::from_value(serde_json::json!({
            "release": null, "hash_failures": 2, "discard_payload": false,
            "skipped_version": null
        }))
        .unwrap();
        assert_eq!(state.hash_failures, 2);
        assert_eq!(state.next_retry_at, None);
    }

    #[test]
    fn unavailable_or_stale_feed_does_not_discard_the_newest_cached_release() {
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let release = feed.parse_manifest(&manifest(b"installer")).unwrap();
        let older = feed
            .parse_manifest(&manifest(b"installer").replace("1.10.0", "1.9.0"))
            .unwrap();
        let cache = tempfile::tempdir().unwrap();
        let installer = cached_installer(&release, cache.path());
        fs::create_dir_all(installer.parent().unwrap()).unwrap();
        fs::write(&installer, b"installer").unwrap();
        UpdateState {
            release: Some(ManifestFields::from(&release)),
            ..UpdateState::default()
        }
        .save(cache.path())
        .unwrap();
        for result in [
            Err(anyhow::anyhow!("HTTP 503")),
            Ok(UpdateCheck::Unpublished),
            Ok(UpdateCheck::Current),
            Ok(UpdateCheck::Available(older)),
        ] {
            assert_eq!(
                feed.reconcile_check("1".parse().unwrap(), cache.path(), result)
                    .unwrap(),
                UpdateCheck::Available(release.clone())
            );
            assert_eq!(fs::read(&installer).unwrap(), b"installer");
        }
    }

    #[tokio::test]
    async fn total_deadline_is_independent_of_the_inactivity_limit() {
        let (url, server, _) = download_server("").await;
        let mut release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(b"installer"))
            .unwrap();
        release.installer = url;
        let control = DownloadControl::new(&release);
        let cache = tempfile::tempdir().unwrap();
        let partial = cached_installer(&release, cache.path()).with_extension("exe.part");
        let result = PreparedUpdate::download_with_client(
            release,
            cache.path(),
            &control,
            reqwest::Client::new(),
            Duration::from_secs(3),
            Duration::from_millis(100),
        )
        .await;
        server.abort();
        let error = format!(
            "{:#}",
            result.err().expect("total deadline must be enforced")
        );
        assert!(error.contains("exceeded its 0.1-second limit"), "{error}");
        assert!(error.contains("0 of 9 bytes"), "{error}");
        assert_eq!(fs::metadata(partial).unwrap().len(), 0);
    }

    #[test]
    fn progress_distinguishes_network_inactivity_verification_and_disk_saving() {
        let release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(b"installer"))
            .unwrap();
        let control = DownloadControl::new(&release);
        assert!(control.progress().message().contains("Connecting"));
        control.update(DownloadPhase::Downloading, 3);
        let mut progress = control.progress();
        progress.started -= Duration::from_secs(12);
        progress.last_data -= Duration::from_secs(8);
        let message = progress.message();
        assert!(message.contains("33%"), "{message}");
        assert!(message.contains("MiB/s"), "{message}");
        assert!(message.contains("12s elapsed"), "{message}");
        assert!(message.contains("No data for 8s"), "{message}");
        control.update(DownloadPhase::Verifying, 9);
        assert!(control.progress().message().contains("SHA-256"));
        control.update(DownloadPhase::Saving, 9);
        assert!(
            control
                .progress()
                .message()
                .contains("Saving verified installer to disk")
        );
    }

    #[test]
    fn prepared_installer_is_retained_and_tampering_blocks_launch() {
        let payload = b"installer";
        let release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(payload))
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let installer = path.join(installer_name(release.version));
        fs::write(&installer, b"installed").unwrap();
        let mut update = PreparedUpdate { installer, release };
        assert!(update.launch(None).is_err());
        drop(update);
        assert!(path.exists());
    }

    #[test]
    fn prepared_payload_archives_require_matching_version_and_sha256() {
        let payload = b"installer";
        let release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(payload))
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        super::write_prepared_payloads_for_test(directory.path(), release.version).unwrap();
        assert_eq!(
            super::verify_prepared_payloads(directory.path(), &release)
                .unwrap()
                .len(),
            2
        );
        fs::write(
            directory.path().join(super::DESKTOP_ARCHIVE_NAME),
            b"tampered",
        )
        .unwrap();
        assert!(
            super::verify_prepared_payloads(directory.path(), &release)
                .unwrap_err()
                .to_string()
                .contains("SHA-256")
        );
        let receipt = directory.path().join(super::PREPARED_MANIFEST_NAME);
        let forged = fs::read_to_string(&receipt).unwrap().replace(
            &format!("{:x}", Sha256::digest(b"desktop-archive")),
            &format!("{:x}", Sha256::digest(b"tampered")),
        );
        fs::write(receipt, forged).unwrap();
        assert!(super::verify_prepared_payloads(directory.path(), &release).is_err());
        super::write_prepared_payloads_for_test(directory.path(), "2.0.0.0".parse().unwrap())
            .unwrap();
        assert!(
            super::verify_prepared_payloads(directory.path(), &release)
                .unwrap_err()
                .to_string()
                .contains("does not match expected")
        );
    }

    #[test]
    fn bridge_discards_already_installed_legacy_feed_metadata() {
        let feed = UpdateFeed::new("https://regressify.eshn.dev").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let mut legacy =
            ManifestFields::from(&feed.parse_manifest(&manifest(b"installer")).unwrap());
        legacy.installer = "https://drive.tuyen.blog/downloads/regressify/Regressify-Desktop-Setup-1.10.0.0-x64.exe".into();
        UpdateState {
            release: Some(legacy),
            ..UpdateState::default()
        }
        .save(cache.path())
        .unwrap();
        let next = feed
            .parse_manifest(&manifest(b"next").replace("1.10.0", "1.11.0"))
            .unwrap();
        assert_eq!(
            feed.reconcile_check(
                "1.10".parse().unwrap(),
                cache.path(),
                Ok(UpdateCheck::Available(next.clone()))
            )
            .unwrap(),
            UpdateCheck::Available(next)
        );
    }

    #[test]
    fn preparation_group_fixture() {
        let Some(directory) = std::env::var_os("REGRESSIFY_PREPARE_TEST_DIR") else {
            return;
        };
        let directory = std::path::PathBuf::from(directory);
        if std::env::var_os("REGRESSIFY_PREPARE_TEST_CHILD").is_some() {
            use std::io::Write;
            let mut hold = fs::File::create(directory.join("hold.lock")).unwrap();
            hold.lock().unwrap();
            hold.write_all(b"ready").unwrap();
            loop {
                std::thread::park();
            }
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "desktop::updates::tests::preparation_group_fixture",
            ])
            .env("REGRESSIFY_PREPARE_TEST_CHILD", "1")
            .spawn()
            .unwrap();
        // Observe the descendant instead of a wall-clock budget: process startup
        // is slow under parallel test load, and the parent group bounds this wait.
        while !fs::metadata(directory.join("hold.lock")).is_ok_and(|metadata| metadata.len() > 0) {
            assert!(
                child.try_wait().unwrap().is_none(),
                "Preparation descendant exited before holding its lock"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::process::exit(0);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn preparation_cancellation_and_timeout_reap_launcher_descendants() {
        for mode in ["timeout", "pause", "handoff"] {
            // The timeout under test is wall-clock, but starting PowerShell plus two
            // test-binary processes can exceed it under parallel load. Retry with a
            // longer timeout only when the descendant never became ready; every
            // attempt still asserts that cleanup leaves no survivor.
            let mut timeout = Duration::from_secs(10);
            loop {
                if preparation_reaps_descendants(mode, timeout).await {
                    break;
                }
                timeout *= 2;
                assert!(
                    timeout <= Duration::from_secs(80),
                    "{mode}: preparation descendant never became ready"
                );
            }
        }
    }

    /// Returns whether the descendant held its lock before preparation ended.
    #[cfg(windows)]
    async fn preparation_reaps_descendants(mode: &str, timeout: Duration) -> bool {
        let directory = tempfile::tempdir().unwrap();
        let lock_path = directory.path().join("hold.lock");
        let release = UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(&manifest(b"installer"))
            .unwrap();
        let control = DownloadControl::new(&release);
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "desktop::updates::tests::preparation_group_fixture",
            ])
            .env("REGRESSIFY_PREPARE_TEST_DIR", directory.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let ready = async {
            while !fs::metadata(&lock_path).is_ok_and(|metadata| metadata.len() > 0) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let preparation = super::wait_for_preparation(&mut command, &control, timeout);
        tokio::pin!(preparation);
        let (result, became_ready) = tokio::select! {
            result = &mut preparation => (result, false),
            () = ready => {
                match mode {
                    "pause" => control.cancel(),
                    "handoff" => control.cancel_for_handoff(),
                    _ => {}
                }
                (preparation.await, true)
            }
        };
        assert!(
            result.is_err(),
            "{mode} completed while a child was still running"
        );
        if lock_path.exists() {
            let hold = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .unwrap();
            hold.try_lock().expect("Preparation child survived cleanup");
        }
        became_ready
    }
}
