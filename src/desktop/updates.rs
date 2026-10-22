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

pub const CURRENT_VERSION: &str = match option_env!("REGRESSIFY_DESKTOP_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
const DOWNLOADS_PATH: &str = "downloads";
const RELEASE_DOWNLOAD_PREFIX: &str =
    "https://github.com/precise-alloy/regressify-rs/releases/download/";
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

pub fn installer_name(version: Version) -> String {
    format!("Regressify-Setup-{version}-x64.exe")
}

#[derive(Clone, Debug)]
pub struct UpdateFeed {
    manifest_url: Url,
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
        Ok(Self { manifest_url })
    }

    pub fn configured() -> Option<Self> {
        if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            return None;
        }
        option_env!("REGRESSIFY_UPDATE_ORIGIN").and_then(|origin| Self::new(origin).ok())
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
            .and_then(|tag| tag.strip_prefix('v'))
            .filter(|raw| {
                !raw.contains('/')
                    && matches!(raw.split('.').count(), 3 | 4)
                    && raw.split('.').all(|part| (1..=4).contains(&part.len()))
            })
            .and_then(|raw| raw.parse::<Version>().ok())
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
        CheckMode, DownloadControl, DownloadPhase, ManifestFields, PreparedUpdate, SkippedUpdate,
        UpdateCheck, UpdateFeed, UpdateState, Version, cached_installer, installer_name,
        installer_redirect_policy, is_allowed_installer_redirect, lock_cache, verify_payload,
    };
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
        let _child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "desktop::updates::tests::preparation_group_fixture",
            ])
            .env("REGRESSIFY_PREPARE_TEST_CHILD", "1")
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !fs::metadata(directory.join("hold.lock")).is_ok_and(|metadata| metadata.len() > 0) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        std::process::exit(0);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn preparation_cancellation_and_timeout_reap_launcher_descendants() {
        for mode in ["timeout", "pause", "handoff"] {
            let directory = tempfile::tempdir().unwrap();
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
            let stop = async {
                tokio::time::timeout(Duration::from_secs(15), async {
                    while !fs::metadata(directory.path().join("hold.lock"))
                        .is_ok_and(|metadata| metadata.len() > 0)
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                match mode {
                    "pause" => control.cancel(),
                    "handoff" => control.cancel_for_handoff(),
                    _ => {}
                }
            };
            let (result, ()) = tokio::join!(
                super::wait_for_preparation(&mut command, &control, Duration::from_secs(10)),
                stop
            );
            assert!(
                result.is_err(),
                "{mode} completed while a child was still running"
            );
            let hold = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(directory.path().join("hold.lock"))
                .unwrap();
            hold.try_lock().expect("Preparation child survived cleanup");
        }
    }
}
