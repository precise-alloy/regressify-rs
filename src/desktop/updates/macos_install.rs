//! macOS opt-in self-update: verified download, safe extraction, staged bundle, atomic swap.
//!
//! The logic is plain filesystem/stream code compiled on every platform so Linux CI tests it;
//! only the real downloader, `codesign`, and relaunch are macOS-specific and are injected by
//! the caller. Nothing here runs a shell, a downloaded script, `sudo`, or `osascript`, and
//! nothing runs before the user clicks "Update now".

// Only the macOS desktop build calls these outside tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

/// Hard ceiling for the archive regardless of what the manifest claims.
pub const MAX_ARCHIVE_SIZE: u64 = 256 * 1024 * 1024;
/// Hard ceiling for the one extracted binary.
const MAX_BINARY_SIZE: u64 = 512 * 1024 * 1024;
pub const APP_NAME: &str = "Regressify.app";
const BINARY_NAME: &str = "regressify-desktop";
/// Entries a desktop archive may contain; anything else is rejected, as `install.sh` does.
const ALLOWED_ENTRIES: [&str; 3] = [BINARY_NAME, "LICENSE", "LICENSING.md"];
const BACKUP_NAME: &str = "Regressify.app.previous";
/// Each install stages in its own uniquely named directory (this prefix plus a random suffix),
/// so one process never touches another's stage.
const STAGE_PREFIX: &str = ".Regressify.app.update-stage-";
/// Install-scoped lock beside the app: the same `mkdir` lock directory `install.sh` takes, so
/// the terminal installer and the in-app updater exclude each other (and every window/user).
const LOCK_NAME: &str = ".Regressify.lock";
/// Written beside the app when a failed swap could not put the previous app back; it names
/// where that only good copy is and blocks every later install until a human clears it.
const RECOVERY_MARK_NAME: &str = ".Regressify.app.recovery-needed";
const WORK_DIR: &str = "macos-install";

/// Archive name for a CPU architecture, as published next to the installer.
pub fn archive_name(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" => Some("regressify-desktop-aarch64-apple-darwin.zip"),
        "x86_64" => Some("regressify-desktop-x86_64-apple-darwin.zip"),
        _ => None,
    }
}

pub fn current_archive_name() -> Option<&'static str> {
    archive_name(std::env::consts::ARCH)
}

/// Size and digest the manifest promises for one archive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveMeta {
    pub size: u64,
    pub sha256: String,
}

/// Reads `archive.<name>=<size>:<sha256>` from the key/value manifest text.
pub fn parse_archive_meta(manifest: &str, name: &str) -> Result<ArchiveMeta> {
    let mut fields = BTreeMap::new();
    for line in manifest
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let (key, value) = line
            .split_once('=')
            .context("Update install needs the key=value manifest format")?;
        ensure!(
            fields.insert(key.trim(), value.trim()).is_none(),
            "Duplicate update manifest field: {key}"
        );
    }
    let value = fields
        .get(format!("archive.{name}").as_str())
        .with_context(|| format!("The release manifest does not list {name}"))?;
    let (size, sha256) = value
        .split_once(':')
        .context("Malformed archive entry in the release manifest")?;
    ensure!(
        !size.is_empty() && size.bytes().all(|byte| byte.is_ascii_digit()),
        "Malformed archive size in the release manifest"
    );
    let size: u64 = size.parse().context("Malformed archive size")?;
    ensure!(
        (1..=MAX_ARCHIVE_SIZE).contains(&size),
        "Archive size is outside the allowed range"
    );
    ensure!(
        sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Malformed archive SHA-256 in the release manifest"
    );
    Ok(ArchiveMeta {
        size,
        sha256: sha256.to_ascii_lowercase(),
    })
}

/// `…/Regressify.app` when `exe` is `…/Regressify.app/Contents/MacOS/<binary>`; `None` for a
/// bare binary or `cargo run`.
pub fn bundle_from_exe(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.file_name()? == APP_NAME)
        .then(|| bundle.to_path_buf())
}

/// Whether the user can replace `bundle`: it is a real directory (not a symlink) owned by the
/// current user, and its parent accepts new entries. Probed by creating a file, because mode bits miss ACLs and read-only
/// volumes.
pub fn bundle_replaceable(bundle: &Path) -> bool {
    let Some(parent) = bundle.parent() else {
        return false;
    };
    let Ok(meta) = fs::symlink_metadata(bundle) else {
        return false;
    };
    if !meta.file_type().is_dir() {
        return false;
    }
    let Ok(probe) = tempfile::Builder::new()
        .prefix(".Regressify.probe")
        .tempfile_in(parent)
    else {
        return false;
    };
    // The probe is owned by the current user, so this requires the bundle to be too: a
    // user must not swap out an app another account owns and may be running.
    probe
        .as_file()
        .metadata()
        .is_ok_and(|probe| same_owner(&meta, &probe))
}

#[cfg(unix)]
fn same_owner(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.uid() == b.uid()
}

#[cfg(not(unix))]
fn same_owner(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    true
}

/// Bundle to offer an in-app update for, from the running executable.
pub fn installable_bundle() -> Option<PathBuf> {
    let bundle = bundle_from_exe(&std::env::current_exe().ok()?)?;
    bundle_replaceable(&bundle).then_some(bundle)
}

/// Exclusive install lock beside the app, held as a `mkdir` lock directory exactly like
/// `install.sh` (`.Regressify.lock`); removed when dropped. Per bundle, not per user cache, so
/// it covers every writer of the shared stage/backup paths, terminal installer included.
/// A process killed while holding it leaves the directory behind; like `install.sh`, the error
/// names the path to remove.
#[derive(Debug)]
pub struct InstallLock {
    path: PathBuf,
}

impl InstallLock {
    pub fn acquire(bundle: &Path) -> Result<Self> {
        let parent = bundle
            .parent()
            .context("Application has no parent folder")?;
        let path = parent.join(LOCK_NAME);
        match fs::create_dir(&path) {
            Ok(()) => Ok(Self { path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => bail!(
                "Another Regressify install is replacing the app; if none is running, remove '{}' and retry",
                path.display()
            ),
            Err(error) => Err(error).context("Cannot take the update install lock"),
        }
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

/// Version of the app currently installed in `bundle`, read from its `Info.plist`. The staged
/// bundle is stamped before the swap, so this changes atomically with the swap and survives any
/// crash or failed bookkeeping afterwards. `None` when absent or not recognisable.
pub fn installed_version(bundle: &Path) -> Option<String> {
    let text = fs::read_to_string(bundle.join("Contents").join("Info.plist")).ok()?;
    let (start, end) = plist_string_span(&text, "CFBundleVersion")?;
    Some(text[start..end].trim().to_owned())
}

/// Marker context on an error: the swap could not finish or undo itself, so the app needs a
/// manual restore. Callers must keep every leftover path in place.
#[derive(Debug)]
pub struct RecoveryNeeded;

impl std::fmt::Display for RecoveryNeeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("The app needs manual recovery")
    }
}

/// Mark contents: the stage path, then the version live in the app BEFORE the swap. The version
/// lets a later run tell "swap never happened or was rolled back" (the live app still reports it)
/// from "the live app is the unverified update and the stage holds the previous app".
fn mark_contents(staged: &Path, previous_version: &str) -> String {
    format!("{}\n{previous_version}", staged.display())
}

/// Fails while a swap may have left the only good app outside the app path. Fails closed: a mark
/// that exists but cannot be read still counts as pending; only `NotFound` means none. A mark
/// whose recorded previous version is what the live bundle reports is stale (the swap never
/// happened, or its rollback finished before the mark could be removed): the good app is at the
/// app path, so the mark is cleared instead of sending the user to restore a rejected update.
fn ensure_no_recovery_pending(bundle: &Path) -> Result<()> {
    let parent = bundle
        .parent()
        .context("Application has no parent folder")?;
    let mark = parent.join(RECOVERY_MARK_NAME);
    let text = match fs::read_to_string(&mark) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => "(unknown: the recovery mark cannot be read)".to_owned(),
    };
    let (kept, previous) = text.split_once('\n').unwrap_or((text.as_str(), ""));
    let previous = previous.trim();
    if !previous.is_empty()
        && installed_version(bundle).as_deref() == Some(previous)
        && fs::remove_file(&mark).is_ok()
    {
        return Ok(());
    }
    let kept = kept.trim();
    let detail = if Path::new(kept).exists() {
        format!("the previous copy is kept at {kept}")
    } else {
        format!(
            "its recorded previous copy {kept} is gone; if the app runs correctly, just delete the mark"
        )
    };
    Err(anyhow::anyhow!(
        "A previous update may not have finished; {detail}. Restore it manually if the app is broken, then delete {}",
        mark.display()
    )
    .context(RecoveryNeeded))
}

/// Removes what a killed install left behind. Needs the lock, so no live install can own the
/// paths removed here. Refuses to touch anything while a recovery is pending, because a
/// stage-prefixed directory may then be the only copy of the previous app. (The swap is a
/// single atomic exchange, so a kill never leaves the app path empty.)
pub fn recover_interrupted(_lock: &InstallLock, bundle: &Path) -> Result<()> {
    let parent = bundle
        .parent()
        .context("Application has no parent folder")?;
    ensure_no_recovery_pending(bundle)?;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(STAGE_PREFIX))
        {
            remove_own(&entry.path())?;
        }
    }
    Ok(())
}

/// Private per-user directory for the download; never a world-writable location.
fn work_dir(cache: &Path) -> Result<PathBuf> {
    let dir = cache.join(WORK_DIR);
    fs::create_dir_all(&dir).context("Cannot create the update download directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Streams `reader` into `file`, failing unless it yields exactly `meta.size` bytes whose
/// SHA-256 equals `meta.sha256`. Never reads more than `size + 1` bytes.
pub fn copy_verified(
    reader: impl Read,
    file: &mut impl Write,
    meta: &ArchiveMeta,
    mut progress: impl FnMut(u64),
) -> Result<()> {
    ensure!(
        meta.size <= MAX_ARCHIVE_SIZE,
        "Archive size is outside the allowed range"
    );
    let mut reader = reader.take(meta.size + 1);
    let mut digest = Sha256::new();
    let mut received = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).context("Download interrupted")?;
        if read == 0 {
            break;
        }
        received += read as u64;
        ensure!(received <= meta.size, "Download is larger than announced");
        digest.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
        progress(received);
    }
    ensure!(received == meta.size, "Download is shorter than announced");
    let actual = hex(&digest.finalize());
    ensure!(
        actual == meta.sha256,
        "Download failed SHA-256 verification"
    );
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Copies the desktop binary out of the archive into `destination`, executable.
///
/// Every entry is checked, not just the one extracted: only the three names `install.sh`
/// accepts, each exactly once, as plain top-level regular files. Names with separators, `..`,
/// absolute paths, and symlinks are rejected, and sizes are bounded on the bytes actually read.
pub fn extract_binary(archive: &Path, destination: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?).context("Archive is not a ZIP")?;
    ensure!(
        zip.len() <= ALLOWED_ENTRIES.len(),
        "Unexpected archive entries"
    );
    let mut seen = Vec::new();
    let mut binary_index = None;
    for index in 0..zip.len() {
        let entry = zip.by_index(index).context("Cannot read archive entry")?;
        let name = entry
            .name()
            .context("Archive entry has an invalid name")?
            .into_owned();
        let name = name.as_str();
        ensure!(
            ALLOWED_ENTRIES.contains(&name) && !seen.contains(&name.to_owned()),
            "Unexpected archive entry"
        );
        ensure!(
            entry.is_file() && !entry.is_symlink() && !entry.is_dir(),
            "Archive entry is not a regular file"
        );
        if let Some(mode) = entry.unix_mode() {
            ensure!(
                mode & 0o170000 == 0o100000,
                "Archive entry is not a regular file"
            );
        }
        if name == BINARY_NAME {
            ensure!(
                (1..=MAX_BINARY_SIZE).contains(&entry.size()),
                "Archive binary has an unexpected size"
            );
            binary_index = Some(index);
        }
        seen.push(name.to_owned());
    }
    let index = binary_index.context("Archive does not contain the desktop binary")?;
    let entry = zip.by_index(index)?;
    let mut limited = entry.take(MAX_BINARY_SIZE + 1);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut output = options
        .open(destination)
        .context("Cannot create the staged binary")?;
    let written = io::copy(&mut limited, &mut output)?;
    ensure!(written <= MAX_BINARY_SIZE, "Archive binary is too large");
    output.flush()?;
    Ok(())
}

/// Recursive copy of the installed bundle into the existing directory `to` so Info.plist, icon, and layout carry over. A
/// symlink or special file means this is not a bundle `install.sh` produced; refuse.
fn copy_bundle(from: &Path, to: &Path) -> Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            fs::create_dir(&target)?;
            copy_bundle(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        } else {
            bail!("The installed app contains a link or special file; not updating it");
        }
    }
    Ok(())
}

/// Removes a leftover of ours by exact path; files and symlinks are unlinked, never followed.
fn remove_own(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Rewrites the `<string>` after `CFBundleVersion` and `CFBundleShortVersionString` to `version`, as
/// `install.sh` writes them, so the updated app reports what it is. Text-level: no other byte
/// changes. Strict: an invalid version, a missing key, an unrecognisable plist, or a failed write
/// is an error, because a new binary that still reports an old version would let a stale window's
/// downgrade check accept an intermediate release. The result is read back before returning.
fn stamp_version(plist: &Path, version: &str) -> Result<()> {
    ensure!(
        !version.is_empty()
            && version
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.'),
        "The update version is not a plain dotted number"
    );
    let text = fs::read_to_string(plist).context("Cannot read the app's Info.plist")?;
    let text = replace_plist_string(&text, "CFBundleVersion", version)
        .and_then(|text| replace_plist_string(&text, "CFBundleShortVersionString", version))
        .context("The app's Info.plist has no recognisable version entries")?;
    // Rewrite in place: same inode, so permissions are kept.
    fs::write(plist, text).context("Cannot write the app's Info.plist")?;
    ensure!(
        fs::read_to_string(plist)
            .ok()
            .and_then(|text| {
                let (start, end) = plist_string_span(&text, "CFBundleVersion")?;
                Some(text[start..end].trim() == version)
            })
            .unwrap_or(false),
        "The staged app does not report the update version"
    );
    Ok(())
}

/// Byte range of the text of the `<string>` that directly follows `<key>{key}</key>`.
fn plist_string_span(text: &str, key: &str) -> Option<(usize, usize)> {
    let marker = format!("<key>{key}</key>");
    let after_key = text.find(&marker)? + marker.len();
    let rest = &text[after_key..];
    let open = rest.find("<string>")?;
    if !rest[..open].trim().is_empty() {
        return None;
    }
    let start = after_key + open + "<string>".len();
    let end = start + text[start..].find("</string>")?;
    Some((start, end))
}

/// Replaces that text with `value`; `None` if the key is absent.
fn replace_plist_string(text: &str, key: &str, value: &str) -> Option<String> {
    let (start, end) = plist_string_span(text, key)?;
    Some(format!("{}{value}{}", &text[..start], &text[end..]))
}

/// Builds the replacement bundle in a staging directory beside `bundle` (same volume, so the
/// later rename is atomic): a copy of the installed bundle with the binary replaced and the
/// Info.plist versions stamped to `version`.
pub fn stage_bundle(
    bundle: &Path,
    new_binary: &Path,
    version: &str,
    sign: &dyn Fn(&Path) -> Result<()>,
    verify: &dyn Fn(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let parent = bundle
        .parent()
        .context("Application has no parent folder")?;
    // Uniquely named and created exclusively: never reuses or deletes anyone else's stage.
    let stage = tempfile::Builder::new()
        .prefix(STAGE_PREFIX)
        .tempdir_in(parent)
        .context("Cannot create the staging folder")?
        .keep();
    let result = (|| {
        fs::set_permissions(&stage, fs::metadata(bundle)?.permissions())?;
        copy_bundle(bundle, &stage)?;
        let target = stage.join("Contents").join("MacOS").join(BINARY_NAME);
        ensure!(
            fs::symlink_metadata(&target).is_ok_and(|meta| meta.file_type().is_file()),
            "The installed app does not have the expected layout"
        );
        // A rename inside the stage keeps the swap of the executable atomic; copy first
        // because the download cache may be on another volume.
        let incoming = stage.join("Contents").join("MacOS").join(".incoming");
        fs::copy(new_binary, &incoming)?;
        fs::rename(&incoming, &target)?;
        // Before signing, so the signature covers the new version.
        stamp_version(&stage.join("Contents").join("Info.plist"), version)?;
        sign(&stage).context("Cannot sign the updated app")?;
        verify(&stage).context("The updated app failed signature verification")?;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(stage),
        Err(error) => {
            let _ = remove_own(&stage);
            Err(error)
        }
    }
}

/// Filesystem operations the swap uses, so tests can fail each step.
pub trait SwapOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Atomically exchanges two directories: both paths exist before and after, so a kill at
    /// any moment leaves a complete app at the destination. Unsupported by default.
    fn exchange(&self, _a: &Path, _b: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Flushes a file or directory to stable storage. Directories are opened read-only, which
    /// is enough for `fsync` on Unix. Windows cannot open a directory with `File::open` (and a
    /// read-only handle cannot `FlushFileBuffers`), and the in-app install only runs on macOS,
    /// so other targets are a documented no-op. A method so tests can record the order or make
    /// it fail.
    fn sync_path(&self, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            File::open(path)?.sync_all()
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(())
        }
    }

    /// Writes the recovery mark durably (contents, then its directory entry); a method so tests
    /// can make it fail.
    fn write_mark(&self, path: &Path, contents: &str) -> io::Result<()> {
        let mut file = File::create(path)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        match path.parent() {
            Some(parent) => self.sync_path(parent),
            None => Ok(()),
        }
    }
}

/// Flushes every file and directory of a staged bundle so the exchange cannot persist a name
/// pointing at contents that were still only in the page cache.
fn sync_tree(ops: &dyn SwapOps, root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            sync_tree(ops, &path)?;
        } else if entry.file_type()?.is_file() {
            ops.sync_path(&path)?;
        }
    }
    ops.sync_path(root)
}

pub struct RealOps;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn renamex_np(
        from: *const std::ffi::c_char,
        to: *const std::ffi::c_char,
        flags: std::ffi::c_uint,
    ) -> std::ffi::c_int;
}

impl SwapOps for RealOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    #[cfg(target_os = "macos")]
    fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        const RENAME_SWAP: std::ffi::c_uint = 0x2;
        let a = CString::new(a.as_os_str().as_bytes())?;
        let b = CString::new(b.as_os_str().as_bytes())?;
        // SAFETY: both pointers are valid NUL-terminated strings for the whole call.
        if unsafe { renamex_np(a.as_ptr(), b.as_ptr(), RENAME_SWAP) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Not atomic: only so Linux unit tests can drive the real swap. The app that ships this
    /// feature (macOS) uses `renamex_np` above.
    #[cfg(not(target_os = "macos"))]
    fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
        let scratch = a.with_extension("scratch");
        fs::rename(a, &scratch)?;
        fs::rename(b, a)?;
        fs::rename(&scratch, b)
    }
}

/// Replaces `bundle` with `staged`, keeping exactly one backup (`Regressify.app.previous`).
///
/// Uses one atomic directory exchange, so the app path always holds a whole app even if the
/// process is killed. Where the volume cannot exchange, the install is refused with nothing
/// changed: a two-rename fallback could leave no app at all, and recovery would need that
/// missing app to run. A bad update is rolled back by exchanging again; if that fails the
/// previous app stays at `staged`, a recovery mark names it, and `RecoveryNeeded` is returned.
pub fn swap_bundle(
    bundle: &Path,
    staged: &Path,
    ops: &dyn SwapOps,
    verify: &dyn Fn(&Path) -> Result<()>,
) -> Result<()> {
    ensure!(
        !fs::symlink_metadata(bundle)?.file_type().is_symlink(),
        "Refusing to replace a symlinked app"
    );
    let parent = bundle
        .parent()
        .context("Application has no parent folder")?;
    ensure_no_recovery_pending(bundle)?;
    let backup = parent.join(BACKUP_NAME);
    // Persist the swap intent BEFORE the exchange: a kill at any later point (even between the
    // exchange and the rollback bookkeeping) leaves a mark that names the stage, so no later run
    // deletes a stage that may hold the only previous app. If it cannot be written, nothing has
    // changed yet and the install is refused.
    let mark_path = parent.join(RECOVERY_MARK_NAME);
    let previous_version = installed_version(bundle).unwrap_or_default();
    ops.write_mark(&mark_path, &mark_contents(staged, &previous_version))
        .context("Cannot record the swap before replacing the app; nothing was changed")?;
    // Power-loss ordering: mark durable -> staged contents durable -> exchange. Otherwise a
    // crash could persist the exchange but not the mark or the replacement's contents, and the
    // missing-mark recovery would delete the stage holding the only complete app.
    if let Err(error) = sync_tree(ops, staged) {
        let _ = fs::remove_file(&mark_path);
        return Err(anyhow::Error::new(error)
            .context("Cannot flush the staged update to disk; nothing was changed"));
    }
    if let Err(error) = ops.exchange(staged, bundle) {
        // Nothing moved; a mark we cannot remove is stale and cleared by the next check.
        let _ = fs::remove_file(&mark_path);
        bail!(
            "This volume cannot swap the app in place ({error}); the in-app install is unavailable here, use the copy command instead"
        );
    }
    // The old app now sits at `staged`; the new one is live. Persist the exchange; a failure
    // here is not fatal (the mark is already durable and the retirement sync below must pass).
    let _ = ops.sync_path(parent);
    if let Err(error) = verify(bundle).context("The installed update failed verification") {
        return match ops.exchange(staged, bundle) {
            Ok(()) => {
                // The rollback must be durable before the mark (the only protection that names
                // the stage) goes: otherwise a power loss could resurrect the rejected update.
                if let Err(sync) = ops.sync_path(parent) {
                    return Err(error
                        .context(format!(
                            "Update rolled back, but the folder could not be flushed ({sync}); {} was kept, delete it before retrying",
                            mark_path.display()
                        ))
                        .context(RecoveryNeeded));
                }
                if fs::remove_file(&mark_path).is_err() {
                    // The mark still names the rejected stage, but the live app reports the
                    // recorded previous version, so the next check clears it.
                    return Err(error.context(format!(
                        "Update rolled back, but {} could not be removed; delete it before retrying",
                        mark_path.display()
                    )));
                }
                Err(error.context("Update rolled back; the previous app is unchanged"))
            }
            Err(restore) => Err(error
                .context(format!(
                    "Update failed and the previous app could not be restored ({restore}); it is kept at {}",
                    staged.display()
                ))
                .context(RecoveryNeeded)),
        };
    }
    // Only now supersede the older backup, with the old app moving into its place. If that
    // fails, `staged` holds the only previous app: keep it and the mark, and report where.
    if remove_own(&backup).is_err() || ops.rename(staged, &backup).is_err() {
        return Err(anyhow::anyhow!(
            "The update is installed, but the previous app could not be kept as {}; it is left at {}",
            backup.display(),
            staged.display()
        )
        .context(RecoveryNeeded));
    }
    // The retirement must be on disk before the mark goes, or a crash could keep the mark gone
    // while the stage rename is lost; on failure the mark stays, naming the stage.
    if let Err(error) = ops.sync_path(parent) {
        return Err(anyhow::Error::new(error)
            .context(format!(
                "The update is installed, but the folder could not be flushed; {} was kept, delete it before the next update",
                mark_path.display()
            ))
            .context(RecoveryNeeded));
    }
    // The swap is complete, so the "current app was not changed" wording would be false:
    // flag this as recovery-needed to keep the UI from appending it.
    if let Err(error) = fs::remove_file(&mark_path) {
        return Err(anyhow::Error::new(error)
            .context(format!(
                "The update is installed, but {} could not be removed; delete it before the next update",
                mark_path.display()
            ))
            .context(RecoveryNeeded));
    }
    // Best effort: persist the mark's removal so a power loss cannot resurrect a mark that
    // names a stage which no longer exists. The install is finished either way.
    let _ = ops.sync_path(parent);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Downloading { received: u64, total: u64 },
    Verifying,
    Installing,
}

/// Everything environment-specific, injected so tests need no network or `codesign`.
pub struct Deps<'a> {
    /// Opens an HTTPS URL as a byte stream.
    pub open: &'a dyn Fn(&str) -> Result<Box<dyn Read>>,
    pub sign: &'a dyn Fn(&Path) -> Result<()>,
    pub verify: &'a dyn Fn(&Path) -> Result<()>,
    pub ops: &'a dyn SwapOps,
}

/// Downloads, verifies, stages, and swaps. On any error nothing about the installed app
/// changes (or it is rolled back) and the temporary download is deleted.
pub fn install(
    bundle: &Path,
    cache: &Path,
    url: &str,
    meta: &ArchiveMeta,
    version: &str,
    deps: &Deps,
    progress: &mut dyn FnMut(Phase),
) -> Result<()> {
    ensure!(
        url.starts_with("https://"),
        "Update downloads must use HTTPS"
    );
    ensure_no_recovery_pending(bundle)?;
    let dir = work_dir(cache)?;
    let archive = tempfile::Builder::new()
        .prefix("download-")
        .tempfile_in(&dir)
        .context("Cannot create the download file")?;
    let total = meta.size;
    let reader = (deps.open)(url)?;
    let mut writer = io::BufWriter::new(archive.as_file());
    copy_verified(reader, &mut writer, meta, |received| {
        progress(Phase::Downloading { received, total })
    })?;
    writer.flush()?;
    drop(writer);
    progress(Phase::Verifying);
    let binary = tempfile::Builder::new()
        .prefix("binary-")
        .tempfile_in(&dir)?
        .into_temp_path();
    // `create_new` needs a free name: keep the unique path, drop the placeholder file.
    let binary_path = binary.to_path_buf();
    fs::remove_file(&binary_path)?;
    extract_binary(archive.path(), &binary_path)?;
    progress(Phase::Installing);
    let staged = stage_bundle(bundle, &binary_path, version, deps.sign, deps.verify);
    let _ = fs::remove_file(&binary_path);
    let staged = staged?;
    if let Err(error) = swap_bundle(bundle, &staged, deps.ops, deps.verify) {
        // A stage that holds the only good app (failed exchange rollback) must survive.
        if error.downcast_ref::<RecoveryNeeded>().is_none() {
            let _ = remove_own(&staged);
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    const GOOD_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn manifest() -> String {
        format!(
            "version=2.2.2.0\ntag=v2.2.2\narchive.regressify-desktop-aarch64-apple-darwin.zip=10:{GOOD_HASH}\narchive.regressify-desktop-x86_64-apple-darwin.zip=11:{}\n",
            GOOD_HASH.to_uppercase().replace('B', "C")
        )
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn meta_for(bytes: &[u8]) -> ArchiveMeta {
        ArchiveMeta {
            size: bytes.len() as u64,
            sha256: hex(&Sha256::digest(bytes)),
        }
    }

    fn fake_app(parent: &Path) -> PathBuf {
        let app = parent.join(APP_NAME);
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        fs::write(app.join("Contents/MacOS").join(BINARY_NAME), b"old").unwrap();
        fs::write(app.join("Contents/Info.plist"), OLD_PLIST).unwrap();
        fs::write(app.join("Contents/Resources/Regressify.icns"), b"icon").unwrap();
        app
    }

    fn binary_of(app: &Path) -> Vec<u8> {
        fs::read(app.join("Contents/MacOS").join(BINARY_NAME)).unwrap()
    }

    fn ok_step(_: &Path) -> Result<()> {
        Ok(())
    }

    #[test]
    fn archive_is_selected_by_arch() {
        assert_eq!(
            archive_name("aarch64"),
            Some("regressify-desktop-aarch64-apple-darwin.zip")
        );
        assert_eq!(
            archive_name("x86_64"),
            Some("regressify-desktop-x86_64-apple-darwin.zip")
        );
        assert_eq!(archive_name("riscv64"), None);
        let live = manifest();
        let arm = parse_archive_meta(&live, archive_name("aarch64").unwrap()).unwrap();
        assert_eq!((arm.size, arm.sha256.as_str()), (10, GOOD_HASH));
        let intel = parse_archive_meta(&live, archive_name("x86_64").unwrap()).unwrap();
        assert_eq!(intel.size, 11);
        assert!(intel.sha256.bytes().all(|b| b == b'c'));
    }

    #[test]
    fn archive_meta_rejects_bad_manifests() {
        let name = "regressify-desktop-aarch64-apple-darwin.zip";
        let line = |value: &str| format!("archive.{name}={value}\n");
        for bad in [
            line(&format!("0:{GOOD_HASH}")),
            line(&format!("{}:{GOOD_HASH}", MAX_ARCHIVE_SIZE + 1)),
            line(&format!("-5:{GOOD_HASH}")),
            line(&format!("abc:{GOOD_HASH}")),
            line("10:abcd"),
            line(&format!("10:{}z", &GOOD_HASH[1..])),
            line("10"),
            format!(
                "{}{}",
                line(&format!("10:{GOOD_HASH}")),
                line(&format!("11:{GOOD_HASH}"))
            ),
            "no equals sign".into(),
            "version=1\n".into(),
        ] {
            assert!(parse_archive_meta(&bad, name).is_err(), "{bad:?}");
        }
        assert!(parse_archive_meta(&line(&format!("10:{GOOD_HASH}")), name).is_ok());
    }

    #[test]
    fn bundle_detection_requires_the_app_layout() {
        let inside = Path::new("/Applications/Regressify.app/Contents/MacOS/regressify-desktop");
        assert_eq!(
            bundle_from_exe(inside),
            Some(PathBuf::from("/Applications/Regressify.app"))
        );
        for outside in [
            "/usr/local/bin/regressify-desktop",
            "/home/u/target/debug/regressify-desktop",
            "/Applications/Other.app/Contents/MacOS/regressify-desktop",
            "/Applications/Regressify.app/Contents/Resources/regressify-desktop",
            "/Regressify.app/MacOS/regressify-desktop",
            "regressify-desktop",
        ] {
            assert_eq!(bundle_from_exe(Path::new(outside)), None, "{outside}");
        }
    }

    #[test]
    fn replaceable_needs_a_writable_real_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        assert!(bundle_replaceable(&app));
        assert!(!bundle_replaceable(&dir.path().join("Missing.app")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let link = dir.path().join("Link.app");
            symlink(&app, &link).unwrap();
            assert!(!bundle_replaceable(&link));
            let locked = tempfile::tempdir().unwrap();
            let locked_app = fake_app(locked.path());
            fs::set_permissions(locked.path(), fs::Permissions::from_mode(0o555)).unwrap();
            // Root ignores directory modes; only assert where the OS enforces them.
            let enforced = tempfile::Builder::new().tempfile_in(locked.path()).is_err();
            let replaceable = bundle_replaceable(&locked_app);
            fs::set_permissions(locked.path(), fs::Permissions::from_mode(0o755)).unwrap();
            if enforced {
                assert!(!replaceable);
            }
        }
    }

    #[test]
    fn lock_is_scoped_to_the_bundle_not_to_a_cache() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        let first = InstallLock::acquire(&app).unwrap();
        // A second user/window has its own cache but the same bundle: still excluded.
        let error = InstallLock::acquire(&app).unwrap_err();
        assert!(error.to_string().contains("remove '"), "{error}");
        drop(first);
        assert!(InstallLock::acquire(&app).is_ok());
        let other = tempfile::tempdir().unwrap();
        let other_app = fake_app(other.path());
        let held = InstallLock::acquire(&app).unwrap();
        assert!(
            InstallLock::acquire(&other_app).is_ok(),
            "another bundle is independent"
        );
        drop(held);
    }

    #[test]
    fn lock_is_the_directory_install_sh_takes() {
        // install.sh does `mkdir <parent>/.Regressify.lock` and `rmdir`s it when done.
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        let script = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/website/public/install.sh"
        ))
        .unwrap();
        assert!(script.contains(&format!("\"$app_parent/{LOCK_NAME}\"")));
        // The terminal installer holds it: the in-app updater is excluded, and leaves it alone.
        let terminal = dir.path().join(".Regressify.lock");
        fs::create_dir(&terminal).unwrap();
        assert!(InstallLock::acquire(&app).is_err());
        assert!(terminal.exists());
        // The updater holds it: a terminal `mkdir` fails; it is released on drop.
        fs::remove_dir(&terminal).unwrap();
        let lock = InstallLock::acquire(&app).unwrap();
        assert!(fs::create_dir(&terminal).is_err());
        drop(lock);
        assert!(!terminal.exists());
    }

    #[cfg(unix)]
    #[test]
    fn work_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let cache = tempfile::tempdir().unwrap();
        let dir = work_dir(cache.path()).unwrap();
        assert_eq!(
            fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_bundle_owned_by_someone_else_is_not_replaceable() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let mine = fs::metadata(dir.path()).unwrap();
        let root = fs::metadata("/").unwrap();
        assert!(same_owner(&mine, &mine));
        if mine.uid() != root.uid() {
            assert!(!same_owner(&root, &mine));
        }
    }

    #[test]
    fn verified_copy_checks_size_and_hash() {
        let data = b"payload bytes".to_vec();
        let meta = meta_for(&data);
        let run = |bytes: &[u8], meta: &ArchiveMeta| {
            let mut out = Vec::new();
            let mut last = 0;
            let result = copy_verified(bytes, &mut out, meta, |received| last = received);
            (result, last)
        };
        let (good, last) = run(&data, &meta);
        assert!(good.is_ok());
        assert_eq!(last, data.len() as u64);
        assert!(
            run(&data[..5], &meta)
                .0
                .unwrap_err()
                .to_string()
                .contains("shorter")
        );
        let long = [data.as_slice(), b"x"].concat();
        assert!(
            run(&long, &meta)
                .0
                .unwrap_err()
                .to_string()
                .contains("larger")
        );
        let mut tampered = data.clone();
        tampered[0] ^= 1;
        assert!(
            run(&tampered, &meta)
                .0
                .unwrap_err()
                .to_string()
                .contains("SHA-256")
        );
        let wrong = ArchiveMeta {
            sha256: GOOD_HASH.into(),
            ..meta.clone()
        };
        assert!(run(&data, &wrong).0.is_err());
        let huge = ArchiveMeta {
            size: MAX_ARCHIVE_SIZE + 1,
            ..meta
        };
        assert!(run(&data, &huge).0.is_err());
    }

    #[test]
    fn a_never_ending_stream_is_cut_off() {
        let meta = ArchiveMeta {
            size: 1000,
            sha256: GOOD_HASH.into(),
        };
        let mut out = Vec::new();
        let result = copy_verified(io::repeat(7), &mut out, &meta, |_| {});
        assert!(result.is_err());
        assert!(out.len() <= 1000);
    }

    fn extract(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, Result<()>) {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        fs::write(&archive, zip_bytes(entries)).unwrap();
        let result = extract_binary(&archive, &dir.path().join("out"));
        (dir, result)
    }

    #[test]
    fn extraction_takes_only_the_binary() {
        let (dir, result) = extract(&[
            ("LICENSE", b"l"),
            (BINARY_NAME, b"new-binary"),
            ("LICENSING.md", b"m"),
        ]);
        result.unwrap();
        assert_eq!(fs::read(dir.path().join("out")).unwrap(), b"new-binary");
        let entries = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(entries, 2, "no other entry is written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join("out"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        assert!(extract(&[(BINARY_NAME, b"x")]).1.is_ok());
    }

    #[test]
    fn extraction_rejects_unsafe_or_unexpected_entries() {
        for entries in [
            vec![("../regressify-desktop", b"x".as_slice())],
            vec![("/etc/passwd", b"x")],
            vec![("sub/regressify-desktop", b"x")],
            vec![(BINARY_NAME, b"x"), ("extra.txt", b"y")],
            vec![(BINARY_NAME, b"x"), ("LICENSE/", b"")],
            vec![("LICENSE", b"only")],
            vec![
                (BINARY_NAME, b"x"),
                ("LICENSE", b"a"),
                ("LICENSING.md", b"b"),
                ("NOTICE", b"c"),
            ],
            vec![(BINARY_NAME, b"")],
        ] {
            let (dir, result) = extract(&entries);
            assert!(result.is_err(), "{entries:?}");
            assert!(!dir.path().join("out").exists(), "{entries:?}");
        }
        assert!(extract_binary(Path::new("/nonexistent.zip"), Path::new("/tmp/x")).is_err());
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.zip"), b"not a zip").unwrap();
        assert!(extract_binary(&dir.path().join("a.zip"), &dir.path().join("o")).is_err());
    }

    #[test]
    fn extraction_rejects_symlink_entries() {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        writer
            .add_symlink(
                BINARY_NAME,
                "/etc/passwd",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        fs::write(&archive, writer.finish().unwrap().into_inner()).unwrap();
        assert!(extract_binary(&archive, &dir.path().join("out")).is_err());
        assert!(!dir.path().join("out").exists());
    }

    #[test]
    fn staging_replaces_only_the_binary_and_signs_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        let new_binary = dir.path().join("new");
        fs::write(&new_binary, b"new").unwrap();
        let signed = RefCell::new(Vec::new());
        let sign = |path: &Path| {
            signed.borrow_mut().push(path.to_path_buf());
            // The signed tree must already hold the new binary.
            assert_eq!(binary_of(path), b"new");
            Ok(())
        };
        let staged = stage_bundle(&app, &new_binary, "2.2.2.0", &sign, &ok_step).unwrap();
        assert_eq!(staged.parent(), app.parent(), "same volume as the app");
        assert_eq!(*signed.borrow(), vec![staged.clone()]);
        assert_eq!(
            fs::read_to_string(staged.join("Contents/Info.plist")).unwrap(),
            OLD_PLIST.replace("2.1.0.0", "2.2.2.0")
        );
        assert_eq!(
            fs::read(staged.join("Contents/Resources/Regressify.icns")).unwrap(),
            b"icon"
        );
        assert!(!staged.join("Contents/MacOS/.incoming").exists());
        assert_eq!(binary_of(&app), b"old", "the installed app is untouched");
    }

    const OLD_PLIST: &str = "<?xml version=\"1.0\"?>\n<plist>\n<dict>\n  <key>CFBundleName</key>\n  <string>Regressify</string>\n  <key>CFBundleVersion</key>\n  <string>2.1.0.0</string>\n  <key>CFBundleShortVersionString</key>\n  <string>2.1.0.0</string>\n  <key>NSHighResolutionCapable</key>\n  <true/>\n</dict>\n</plist>\n";

    fn stage_with_plist(plist: &[u8], version: &str) -> (tempfile::TempDir, Result<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        fs::write(app.join("Contents/Info.plist"), plist).unwrap();
        let new_binary = dir.path().join("new");
        fs::write(&new_binary, b"new").unwrap();
        let staged = stage_bundle(&app, &new_binary, version, &ok_step, &ok_step);
        (dir, staged)
    }

    #[test]
    fn staging_stamps_both_versions_and_touches_nothing_else() {
        let (_dir, staged) = stage_with_plist(OLD_PLIST.as_bytes(), "2.2.2.0");
        let out = fs::read_to_string(staged.unwrap().join("Contents/Info.plist")).unwrap();
        assert_eq!(out, OLD_PLIST.replace("2.1.0.0", "2.2.2.0"));
    }

    #[cfg(unix)]
    #[test]
    fn the_plist_is_stamped_before_signing_and_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        let plist = app.join("Contents/Info.plist");
        fs::write(&plist, OLD_PLIST).unwrap();
        fs::set_permissions(&plist, fs::Permissions::from_mode(0o640)).unwrap();
        let new_binary = dir.path().join("new");
        fs::write(&new_binary, b"new").unwrap();
        let sign = |path: &Path| {
            let text = fs::read_to_string(path.join("Contents/Info.plist")).unwrap();
            assert!(text.contains("<string>2.2.2.0</string>") && !text.contains("2.1.0.0"));
            Ok(())
        };
        let staged = stage_bundle(&app, &new_binary, "2.2.2.0", &sign, &ok_step).unwrap();
        let mode = fs::metadata(staged.join("Contents/Info.plist"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[test]
    fn staging_aborts_when_the_version_cannot_be_stamped() {
        let only_one = OLD_PLIST.replace(
            "  <key>CFBundleShortVersionString</key>\n  <string>2.1.0.0</string>\n",
            "",
        );
        let unusable: [&[u8]; 3] = [only_one.as_bytes(), b"plist", &[0xff, 0xfe, 0x00]];
        for plist in unusable {
            let (dir, staged) = stage_with_plist(plist, "2.2.2.0");
            assert!(staged.is_err(), "{plist:?}");
            assert_eq!(stages_in(dir.path()).len(), 0, "the stage is removed");
        }
    }

    #[test]
    fn a_hostile_version_is_never_written_into_the_plist() {
        for bad in [
            "",
            "2.2</string><key>x</key>",
            "2.2.2.0 ",
            "v2.2",
            "<a>",
            "2,2",
        ] {
            let (dir, staged) = stage_with_plist(OLD_PLIST.as_bytes(), bad);
            assert!(staged.is_err(), "{bad:?}");
            assert_eq!(stages_in(dir.path()).len(), 0, "{bad:?}");
            assert_eq!(
                fs::read_to_string(dir.path().join(APP_NAME).join("Contents/Info.plist")).unwrap(),
                OLD_PLIST
            );
        }
    }

    #[test]
    fn install_stamps_the_version_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        fs::write(app.join("Contents/Info.plist"), OLD_PLIST).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let bytes = zip_bytes(&[(BINARY_NAME, b"new-binary")]);
        let meta = meta_for(&bytes);
        let open =
            |_: &str| -> Result<Box<dyn Read>> { Ok(Box::new(io::Cursor::new(bytes.clone()))) };
        install(
            &app,
            cache.path(),
            "https://example.test/a.zip",
            &meta,
            "2.3.4.0",
            &deps_for(&open, &RealOps),
            &mut |_| {},
        )
        .unwrap();
        let text = fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        assert_eq!(text, OLD_PLIST.replace("2.1.0.0", "2.3.4.0"));
    }

    #[test]
    fn staging_failures_clean_up_and_leave_the_app_alone() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        let new_binary = dir.path().join("new");
        fs::write(&new_binary, b"new").unwrap();
        let fail = |_: &Path| -> Result<()> { bail!("boom") };
        assert!(stage_bundle(&app, &new_binary, "2.2.2.0", &fail, &ok_step).is_err());
        assert!(stage_bundle(&app, &new_binary, "2.2.2.0", &ok_step, &fail).is_err());
        assert!(
            stage_bundle(
                &app,
                &dir.path().join("absent"),
                "2.2.2.0",
                &ok_step,
                &ok_step
            )
            .is_err()
        );
        assert!(stages_in(dir.path()).is_empty());
        assert_eq!(binary_of(&app), b"old");
        // A stage left by another process is never touched by a new install.
        let foreign = dir.path().join(format!("{STAGE_PREFIX}foreign"));
        fs::create_dir_all(foreign.join("junk")).unwrap();
        let staged = stage_bundle(&app, &new_binary, "2.2.2.0", &ok_step, &ok_step).unwrap();
        assert!(foreign.join("junk").exists());
        assert_ne!(staged, foreign);
        // A bundle with an unexpected layout or a link inside is refused.
        let odd = dir.path().join("odd");
        fs::create_dir_all(&odd).unwrap();
        let odd = odd.join(APP_NAME);
        fs::create_dir_all(&odd).unwrap();
        assert!(stage_bundle(&odd, &new_binary, "2.2.2.0", &ok_step, &ok_step).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hosts", app.join("Contents/link")).unwrap();
            assert!(stage_bundle(&app, &new_binary, "2.2.2.0", &ok_step, &ok_step).is_err());
        }
    }

    struct FailNth {
        calls: Cell<usize>,
        fail_at: usize,
    }

    impl SwapOps for FailNth {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.calls.set(self.calls.get() + 1);
            if self.calls.get() == self.fail_at {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        }
    }

    fn stages_in(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(STAGE_PREFIX))
            })
            .collect()
    }

    fn staged_pair(dir: &Path) -> (PathBuf, PathBuf) {
        let app = fake_app(dir);
        let staged = dir.join(format!("{STAGE_PREFIX}test"));
        fs::create_dir_all(staged.join("Contents/MacOS")).unwrap();
        fs::write(staged.join("Contents/MacOS").join(BINARY_NAME), b"new").unwrap();
        fs::write(
            staged.join("Contents/Info.plist"),
            OLD_PLIST.replace("2.1.0.0", "2.2.2.0"),
        )
        .unwrap();
        (app, staged)
    }

    /// Records the durability-relevant order: syncs, the exchange, and the mark's existence.
    struct Journal {
        events: std::cell::RefCell<Vec<String>>,
        fail_sync_of: Option<&'static str>,
    }

    impl SwapOps for Journal {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.events.borrow_mut().push("rename".into());
            fs::rename(from, to)
        }

        fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
            let mark = a.parent().unwrap().join(RECOVERY_MARK_NAME);
            assert!(mark.exists(), "mark exists before the exchange");
            self.events.borrow_mut().push("exchange".into());
            RealOps.exchange(a, b)
        }

        fn sync_path(&self, path: &Path) -> io::Result<()> {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            self.events.borrow_mut().push(format!("sync:{name}"));
            if path.is_dir() {
                let mark = path.join(RECOVERY_MARK_NAME).exists();
                self.events
                    .borrow_mut()
                    .push(format!("dir-sync mark={mark}"));
            }
            if self.fail_sync_of == Some(name.as_str()) {
                return Err(io::Error::other("injected"));
            }
            Ok(())
        }
    }

    #[test]
    fn the_swap_flushes_the_mark_the_stage_and_the_folder_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let journal = Journal {
            events: Default::default(),
            fail_sync_of: None,
        };
        swap_bundle(&app, &staged, &journal, &ok_step).unwrap();
        let events = journal.events.borrow();
        let at = |name: &str| events.iter().position(|event| event == name).unwrap();
        let folder = format!("sync:{}", dir.path().file_name().unwrap().to_string_lossy());
        assert!(
            at(&folder) < at("sync:Info.plist"),
            "mark folder entry first: {events:?}"
        );
        assert!(at("sync:Info.plist") < at("exchange"), "{events:?}");
        assert!(
            at(&format!("sync:{BINARY_NAME}")) < at("exchange"),
            "{events:?}"
        );
        let exchange = at("exchange");
        let after = events.iter().skip(exchange).collect::<Vec<_>>();
        let retire = after.iter().position(|event| **event == "rename").unwrap();
        assert!(
            after[..retire].contains(&&folder),
            "folder flushed after the exchange: {events:?}"
        );
        assert!(
            after[retire..].contains(&&folder),
            "folder flushed after retiring the backup: {events:?}"
        );
        assert!(!dir.path().join(RECOVERY_MARK_NAME).exists());
    }

    #[test]
    fn a_rollback_is_flushed_before_the_mark_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let journal = Journal {
            events: Default::default(),
            fail_sync_of: None,
        };
        let reject = |_: &Path| -> Result<()> { bail!("rejected") };
        let error = swap_bundle(&app, &staged, &journal, &reject).unwrap_err();
        assert!(format!("{error:#}").contains("rolled back"), "{error:#}");
        let events = journal.events.borrow();
        let second = events
            .iter()
            .rposition(|event| event == "exchange")
            .unwrap();
        assert!(
            events[second..].contains(&"dir-sync mark=true".to_string()),
            "rollback flushed while the mark still protects it: {events:?}"
        );
        assert_eq!(binary_of(&app), b"old");
        assert!(!dir.path().join(RECOVERY_MARK_NAME).exists());
    }

    #[test]
    fn a_rollback_that_cannot_be_flushed_keeps_the_mark() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        struct FailAfterRollback(Journal);
        impl SwapOps for FailAfterRollback {
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                self.0.rename(from, to)
            }
            fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
                self.0.exchange(a, b)
            }
            fn sync_path(&self, path: &Path) -> io::Result<()> {
                let exchanges = self
                    .0
                    .events
                    .borrow()
                    .iter()
                    .filter(|event| *event == "exchange")
                    .count();
                self.0.sync_path(path)?;
                if exchanges >= 2 && path.is_dir() {
                    return Err(io::Error::other("injected"));
                }
                Ok(())
            }
        }
        let ops = FailAfterRollback(Journal {
            events: Default::default(),
            fail_sync_of: None,
        });
        let reject = |_: &Path| -> Result<()> { bail!("rejected") };
        let error = swap_bundle(&app, &staged, &ops, &reject).unwrap_err();
        assert!(dir.path().join(RECOVERY_MARK_NAME).exists());
        // `RecoveryNeeded` keeps the caller from deleting the stage that holds the only copy.
        assert!(
            error.downcast_ref::<RecoveryNeeded>().is_some(),
            "{error:#}"
        );
    }

    #[test]
    fn completion_flushes_the_folder_after_the_mark_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let journal = Journal {
            events: Default::default(),
            fail_sync_of: None,
        };
        swap_bundle(&app, &staged, &journal, &ok_step).unwrap();
        assert_eq!(
            journal.events.borrow().last().map(String::as_str),
            Some("dir-sync mark=false"),
            "{:?}",
            journal.events.borrow()
        );
    }

    #[test]
    fn a_failed_final_flush_does_not_fail_a_finished_install() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        struct FailLast(Journal, std::cell::Cell<bool>);
        impl SwapOps for FailLast {
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                self.0.rename(from, to)
            }
            fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
                self.0.exchange(a, b)
            }
            fn sync_path(&self, path: &Path) -> io::Result<()> {
                let mark_gone = path.is_dir() && !path.join(RECOVERY_MARK_NAME).exists();
                let done = self.0.events.borrow().iter().any(|e| e == "rename");
                self.0.sync_path(path)?;
                if done && mark_gone {
                    self.1.set(true);
                    return Err(io::Error::other("injected"));
                }
                Ok(())
            }
        }
        let ops = FailLast(
            Journal {
                events: Default::default(),
                fail_sync_of: None,
            },
            std::cell::Cell::new(false),
        );
        swap_bundle(&app, &staged, &ops, &ok_step).unwrap();
        assert!(ops.1.get(), "the final flush failure was injected");
        assert_eq!(binary_of(&app), b"new");
    }

    #[test]
    fn a_stage_that_cannot_be_flushed_refuses_the_swap_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let journal = Journal {
            events: Default::default(),
            fail_sync_of: Some("Info.plist"),
        };
        let error = swap_bundle(&app, &staged, &journal, &ok_step).unwrap_err();
        assert!(
            format!("{error:#}").contains("nothing was changed"),
            "{error:#}"
        );
        assert!(
            !journal
                .events
                .borrow()
                .iter()
                .any(|event| event == "exchange")
        );
        assert_eq!(binary_of(&app), b"old");
        assert!(staged.exists());
        assert!(!dir.path().join(RECOVERY_MARK_NAME).exists());
    }

    #[test]
    fn a_folder_that_cannot_be_flushed_after_retiring_keeps_the_mark() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let folder: &'static str = Box::leak(
            dir.path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
                .into_boxed_str(),
        );
        let journal = Journal {
            events: Default::default(),
            fail_sync_of: Some(folder),
        };
        // The folder sync also guards the mark write, so fail only from the exchange on.
        struct LateFail<'a>(&'a Journal);
        impl SwapOps for LateFail<'_> {
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                self.0.rename(from, to)
            }
            fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
                self.0.exchange(a, b)
            }
            fn sync_path(&self, path: &Path) -> io::Result<()> {
                if self.0.events.borrow().iter().any(|event| event == "rename") {
                    return self.0.sync_path(path);
                }
                RealOps.sync_path(path)
            }
        }
        let error = swap_bundle(&app, &staged, &LateFail(&journal), &ok_step).unwrap_err();
        assert!(
            error.downcast_ref::<RecoveryNeeded>().is_some(),
            "{error:#}"
        );
        assert!(
            dir.path().join(RECOVERY_MARK_NAME).exists(),
            "the mark stays until the retirement is durable"
        );
    }

    #[test]
    fn swap_keeps_exactly_one_backup() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        // An older backup is superseded, not accumulated.
        fs::create_dir_all(dir.path().join(BACKUP_NAME).join("old-backup")).unwrap();
        swap_bundle(&app, &staged, &RealOps, &ok_step).unwrap();
        assert_eq!(binary_of(&app), b"new");
        assert_eq!(binary_of(&dir.path().join(BACKUP_NAME)), b"old");
        assert!(!dir.path().join(BACKUP_NAME).join("old-backup").exists());
        assert!(!staged.exists());
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn swap_is_refused_untouched_where_the_volume_cannot_exchange() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        fs::create_dir_all(dir.path().join(BACKUP_NAME).join("older")).unwrap();
        // `FailNth` only renames; exchange is unsupported, as on a volume without it.
        let ops = FailNth {
            calls: Cell::new(0),
            fail_at: usize::MAX,
        };
        let error = swap_bundle(&app, &staged, &ops, &ok_step).unwrap_err();
        assert!(format!("{error:#}").contains("copy command"), "{error:#}");
        assert!(error.downcast_ref::<RecoveryNeeded>().is_none());
        assert_eq!(ops.calls.get(), 0, "no rename was attempted");
        assert_eq!(binary_of(&app), b"old");
        assert!(staged.exists(), "the caller removes its own stage");
        assert!(
            dir.path().join(BACKUP_NAME).join("older").exists(),
            "the earlier backup survives a refused swap"
        );
    }

    #[test]
    fn swap_rolls_back_when_the_installed_update_fails_verification() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let reject = |_: &Path| -> Result<()> { bail!("bad signature") };
        let error = swap_bundle(&app, &staged, &RealOps, &reject).unwrap_err();
        assert!(format!("{error:#}").contains("rolled back"), "{error:#}");
        assert_eq!(binary_of(&app), b"old");
    }

    #[cfg(unix)]
    #[test]
    fn swap_refuses_a_symlinked_app_and_never_follows_a_backup_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = tempfile::tempdir().unwrap();
        let target = fake_app(real.path());
        let link = dir.path().join(APP_NAME);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let staged = dir.path().join(format!("{STAGE_PREFIX}test"));
        fs::create_dir_all(&staged).unwrap();
        assert!(swap_bundle(&link, &staged, &RealOps, &ok_step).is_err());
        assert_eq!(binary_of(&target), b"old");

        // A symlink planted as the backup name is unlinked, not followed.
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(BACKUP_NAME)).unwrap();
        swap_bundle(&app, &staged, &RealOps, &ok_step).unwrap();
        assert!(outside.path().join("keep").exists());
    }

    /// Atomic exchange stand-in for Linux (the real one is `renamex_np`): swaps via a
    /// scratch name. Counts every operation and panics at `die_at` *before* performing it,
    /// which models the process being killed with nothing after that point running.
    struct Counting {
        calls: Cell<usize>,
        die_at: usize,
        exchange: bool,
    }

    impl Counting {
        fn tick(&self) {
            self.calls.set(self.calls.get() + 1);
            if self.calls.get() == self.die_at {
                panic!("process killed");
            }
        }
    }

    impl SwapOps for Counting {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.tick();
            fs::rename(from, to)
        }

        fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
            if !self.exchange {
                return Err(io::ErrorKind::Unsupported.into());
            }
            self.tick();
            let scratch = a.with_extension("scratch");
            fs::rename(a, &scratch)?;
            fs::rename(b, a)?;
            fs::rename(&scratch, b)
        }
    }

    fn killed_swap(exchange: bool, die_at: usize) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let ops = Counting {
            calls: Cell::new(0),
            die_at,
            exchange,
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = swap_bundle(&app, &staged, &ops, &ok_step);
        }));
        assert_eq!(
            outcome.is_err(),
            ops.calls.get() >= die_at,
            "kill point hit"
        );
        (dir, app, staged)
    }

    #[test]
    fn exchange_swap_installs_and_keeps_one_backup() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let ops = Counting {
            calls: Cell::new(0),
            die_at: usize::MAX,
            exchange: true,
        };
        swap_bundle(&app, &staged, &ops, &ok_step).unwrap();
        assert_eq!(binary_of(&app), b"new");
        assert_eq!(binary_of(&dir.path().join(BACKUP_NAME)), b"old");
        assert!(!staged.exists());
    }

    #[test]
    fn exchange_swap_rolls_back_a_bad_update_and_flags_a_failed_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let ops = Counting {
            calls: Cell::new(0),
            die_at: usize::MAX,
            exchange: true,
        };
        let reject = |_: &Path| -> Result<()> { bail!("bad signature") };
        let error = swap_bundle(&app, &staged, &ops, &reject).unwrap_err();
        assert!(error.downcast_ref::<RecoveryNeeded>().is_none());
        assert_eq!(binary_of(&app), b"old");

        // Rollback exchange fails: the old app is at the stage and must be reported there.
        struct OneExchange(Cell<usize>);
        impl SwapOps for OneExchange {
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                fs::rename(from, to)
            }
            fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
                self.0.set(self.0.get() + 1);
                if self.0.get() > 1 {
                    return Err(io::Error::other("disk gone"));
                }
                let scratch = a.with_extension("scratch");
                fs::rename(a, &scratch)?;
                fs::rename(b, a)?;
                fs::rename(&scratch, b)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let error = swap_bundle(&app, &staged, &OneExchange(Cell::new(0)), &reject).unwrap_err();
        assert!(error.downcast_ref::<RecoveryNeeded>().is_some());
        assert!(format!("{error:#}").contains(&staged.display().to_string()));
        assert_eq!(binary_of(&staged), b"old");

        // Retrying (what "Update now" does) must neither delete nor overwrite that copy.
        let lock = InstallLock::acquire(&app).unwrap();
        let error = recover_interrupted(&lock, &app).unwrap_err();
        assert!(error.downcast_ref::<RecoveryNeeded>().is_some());
        let cache = tempfile::tempdir().unwrap();
        let bytes = zip_bytes(&[(BINARY_NAME, b"newer")]);
        let meta = meta_for(&bytes);
        let open =
            |_: &str| -> Result<Box<dyn Read>> { Ok(Box::new(io::Cursor::new(bytes.clone()))) };
        let error = install(
            &app,
            cache.path(),
            "https://example.test/a.zip",
            &meta,
            "2.9.0.0",
            &deps_for(&open, &RealOps),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(
            error.downcast_ref::<RecoveryNeeded>().is_some(),
            "{error:#}"
        );
        assert!(format!("{error:#}").contains(&staged.display().to_string()));
        assert_eq!(binary_of(&staged), b"old", "the only good copy is kept");
        assert_eq!(
            binary_of(&app),
            b"new",
            "the app path is not touched either"
        );
        assert!(!dir.path().join(BACKUP_NAME).exists());
        // Once a human restores the app and clears the mark, installs work again.
        fs::remove_file(dir.path().join(RECOVERY_MARK_NAME)).unwrap();
        recover_interrupted(&lock, &app).unwrap();
        assert!(!staged.exists());
    }

    /// Exchanges once, then fails (the rollback); optionally cannot write the recovery mark.
    struct FailingRollback {
        exchanges: Cell<usize>,
        mark_fails: bool,
    }

    impl SwapOps for FailingRollback {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            fs::rename(from, to)
        }
        fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
            self.exchanges.set(self.exchanges.get() + 1);
            if self.exchanges.get() > 1 {
                return Err(io::Error::other("disk gone"));
            }
            let scratch = a.with_extension("scratch");
            fs::rename(a, &scratch)?;
            fs::rename(b, a)?;
            fs::rename(&scratch, b)
        }
        fn write_mark(&self, path: &Path, contents: &str) -> io::Result<()> {
            if self.mark_fails {
                return Err(io::Error::other("volume error"));
            }
            fs::write(path, contents)
        }
    }

    #[test]
    fn an_unwritable_mark_refuses_the_swap_before_anything_moves() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let ops = FailingRollback {
            exchanges: Cell::new(0),
            mark_fails: true,
        };
        let error = swap_bundle(&app, &staged, &ops, &ok_step).unwrap_err();
        assert!(
            format!("{error:#}").contains("nothing was changed"),
            "{error:#}"
        );
        assert_eq!(ops.exchanges.get(), 0, "no exchange without a mark");
        assert_eq!(binary_of(&app), b"old");
        assert_eq!(binary_of(&staged), b"new");
        assert!(!dir.path().join(RECOVERY_MARK_NAME).exists());
    }

    #[test]
    fn a_restart_after_a_failed_rollback_keeps_the_only_previous_app() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let ops = FailingRollback {
            exchanges: Cell::new(0),
            mark_fails: false,
        };
        let reject = |_: &Path| -> Result<()> { bail!("bad signature") };
        let error = swap_bundle(&app, &staged, &ops, &reject).unwrap_err();
        assert!(error.downcast_ref::<RecoveryNeeded>().is_some());
        // A restart keeps no in-process state: only the on-disk mark guards the stage.
        let lock = InstallLock::acquire(&app).unwrap();
        let error = recover_interrupted(&lock, &app).unwrap_err();
        assert!(error.downcast_ref::<RecoveryNeeded>().is_some());
        assert_eq!(binary_of(&staged), b"old", "the only good copy is kept");
        let cache = tempfile::tempdir().unwrap();
        let bytes = zip_bytes(&[(BINARY_NAME, b"newer")]);
        let meta = meta_for(&bytes);
        let open =
            |_: &str| -> Result<Box<dyn Read>> { Ok(Box::new(io::Cursor::new(bytes.clone()))) };
        let error = install(
            &app,
            cache.path(),
            "https://example.test/a.zip",
            &meta,
            "2.9.0.0",
            &deps_for(&open, &RealOps),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(
            error.downcast_ref::<RecoveryNeeded>().is_some(),
            "{error:#}"
        );
        assert_eq!(binary_of(&staged), b"old");
    }

    #[test]
    fn a_stale_mark_after_a_finished_rollback_is_cleared_not_followed() {
        // Killed after the rollback exchange but before the mark was removed: the good app is
        // live and the mark names the rejected update's stage.
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        fs::write(
            dir.path().join(RECOVERY_MARK_NAME),
            mark_contents(&staged, "2.1.0.0"),
        )
        .unwrap();
        let lock = InstallLock::acquire(&app).unwrap();
        recover_interrupted(&lock, &app).unwrap();
        assert!(!dir.path().join(RECOVERY_MARK_NAME).exists());
        assert!(stages_in(dir.path()).is_empty());
        assert_eq!(binary_of(&app), b"old");
    }

    #[test]
    fn a_kill_between_the_exchange_and_the_bookkeeping_keeps_the_previous_app() {
        // The exchange completed (the update is live, unverified) and the process died: the
        // mark written before the exchange must keep the stage, which holds the old app.
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let ops = Counting {
            calls: Cell::new(0),
            die_at: 2,
            exchange: true,
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = swap_bundle(&app, &staged, &ops, &ok_step);
        }));
        assert!(outcome.is_err());
        assert_eq!(binary_of(&app), b"new");
        let lock = InstallLock::acquire(&app).unwrap();
        let error = recover_interrupted(&lock, &app).unwrap_err();
        assert!(error.downcast_ref::<RecoveryNeeded>().is_some());
        assert!(format!("{error:#}").contains(&staged.display().to_string()));
        assert_eq!(binary_of(&staged), b"old", "the previous app survives");
    }

    #[test]
    fn a_failed_backup_retirement_keeps_the_previous_app_and_reports_it() {
        struct NoRename;
        impl SwapOps for NoRename {
            fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
                Err(io::Error::other("rename refused"))
            }
            fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
                let scratch = a.with_extension("scratch");
                fs::rename(a, &scratch)?;
                fs::rename(b, a)?;
                fs::rename(&scratch, b)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        fs::create_dir_all(dir.path().join(BACKUP_NAME).join("older")).unwrap();
        let error = swap_bundle(&app, &staged, &NoRename, &ok_step).unwrap_err();
        assert!(error.downcast_ref::<RecoveryNeeded>().is_some());
        assert!(format!("{error:#}").contains(&staged.display().to_string()));
        assert_eq!(binary_of(&app), b"new");
        assert_eq!(
            binary_of(&staged),
            b"old",
            "the previous app is not deleted"
        );
        // install() must not clean it up either, and later runs stay blocked.
        let lock = InstallLock::acquire(&app).unwrap();
        assert!(recover_interrupted(&lock, &app).is_err());
        assert_eq!(binary_of(&staged), b"old");
    }

    #[test]
    fn a_mark_that_cannot_be_removed_after_a_finished_swap_is_not_reported_as_unchanged() {
        struct DirMark;
        impl SwapOps for DirMark {
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                fs::rename(from, to)
            }
            fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
                let scratch = a.with_extension("scratch");
                fs::rename(a, &scratch)?;
                fs::rename(b, a)?;
                fs::rename(&scratch, b)
            }
            // A directory in the mark's place makes `remove_file` fail after the swap.
            fn write_mark(&self, path: &Path, _: &str) -> io::Result<()> {
                fs::create_dir_all(path.join("keep"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let error = swap_bundle(&app, &staged, &DirMark, &ok_step).unwrap_err();
        assert_eq!(binary_of(&app), b"new");
        assert!(error.downcast_ref::<RecoveryNeeded>().is_some());
        let text = super::super::failure_reason(&error);
        assert!(text.contains("The update is installed"), "{text}");
        assert!(!text.contains("was not changed"), "{text}");
    }

    #[test]
    fn an_unreadable_recovery_mark_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        // A directory in the mark's place: it exists but cannot be read as a file.
        fs::create_dir(dir.path().join(RECOVERY_MARK_NAME)).unwrap();
        let lock = InstallLock::acquire(&app).unwrap();
        let error = recover_interrupted(&lock, &app).unwrap_err();
        assert!(error.downcast_ref::<RecoveryNeeded>().is_some());
        assert!(
            staged.exists(),
            "nothing is deleted while the mark is unknown"
        );
    }

    #[test]
    fn the_mark_is_written_before_the_exchange_and_cleared_when_the_rollback_works() {
        struct Spy(Cell<usize>);
        impl SwapOps for Spy {
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                fs::rename(from, to)
            }
            fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
                let mark = a.parent().unwrap().join(RECOVERY_MARK_NAME);
                if mark.exists() {
                    self.0.set(self.0.get() + 1);
                }
                let scratch = a.with_extension("scratch");
                fs::rename(a, &scratch)?;
                fs::rename(b, a)?;
                fs::rename(&scratch, b)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = staged_pair(dir.path());
        let spy = Spy(Cell::new(0));
        let reject = |_: &Path| -> Result<()> { bail!("bad signature") };
        // The mark exists before the swap-in exchange and the rollback exchange alike.
        swap_bundle(&app, &staged, &spy, &reject).unwrap_err();
        assert_eq!(spy.0.get(), 2, "mark existed before both exchanges");
        assert!(!dir.path().join(RECOVERY_MARK_NAME).exists());
        assert_eq!(binary_of(&app), b"old");
    }

    #[test]
    fn an_exchange_swap_never_leaves_the_app_missing_whenever_it_is_killed() {
        // Operation 1 is the exchange, 2 retires the old app to the backup.
        for die_at in [1, 2] {
            let (dir, app, _staged) = killed_swap(true, die_at);
            let installed = binary_of(&app);
            assert!(
                installed == b"old" || installed == b"new",
                "die_at {die_at}: a whole app is always in place"
            );
            assert_eq!(installed == b"new", die_at == 2, "die_at {die_at}");
            let lock = InstallLock::acquire(&app).unwrap();
            if die_at == 1 {
                // Nothing moved: the mark is stale and the unused stage is cleaned.
                recover_interrupted(&lock, &app).unwrap();
                assert!(stages_in(dir.path()).is_empty());
            } else {
                // The unverified update is live: the stage (old app) is kept, not cleaned.
                assert!(recover_interrupted(&lock, &app).is_err());
                assert_eq!(stages_in(dir.path()).len(), 1);
            }
            assert_eq!(binary_of(&app), installed);
        }
    }

    #[test]
    fn two_installers_on_one_bundle_never_touch_each_others_stage() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        let new_binary = dir.path().join("new");
        fs::write(&new_binary, b"new").unwrap();
        // A stages and is "signing" when B (another cache, same bundle) is stopped by the lock.
        let a = InstallLock::acquire(&app).unwrap();
        let staged_a = stage_bundle(&app, &new_binary, "2.2.2.0", &ok_step, &ok_step).unwrap();
        assert!(InstallLock::acquire(&app).is_err());
        assert!(staged_a.exists());
        // Even a stage made without the lock gets its own name, and B failing removes only B's.
        let fail = |_: &Path| -> Result<()> { bail!("boom") };
        assert!(stage_bundle(&app, &new_binary, "2.2.2.0", &ok_step, &fail).is_err());
        assert_eq!(
            binary_of(&staged_a),
            b"new",
            "A's stage survives B's failure"
        );
        swap_bundle(&app, &staged_a, &RealOps, &ok_step).unwrap();
        assert_eq!(binary_of(&app), b"new");
        drop(a);
        // B can now proceed from the fresh state.
        let b = InstallLock::acquire(&app).unwrap();
        let staged_b = stage_bundle(&app, &new_binary, "2.2.2.0", &ok_step, &ok_step).unwrap();
        assert_ne!(staged_a, staged_b);
        drop(b);
    }

    #[test]
    fn installed_version_is_read_from_the_bundle_itself() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        fs::write(app.join("Contents/Info.plist"), b"plist").unwrap();
        assert_eq!(installed_version(&app), None, "unrecognisable plist");
        fs::write(app.join("Contents/Info.plist"), OLD_PLIST).unwrap();
        assert_eq!(installed_version(&app).as_deref(), Some("2.1.0.0"));
        assert_eq!(installed_version(&dir.path().join("absent.app")), None);
        // The floor moves with the swap itself, so no bookkeeping step can be lost after it.
        let cache = tempfile::tempdir().unwrap();
        let bytes = zip_bytes(&[(BINARY_NAME, b"new-binary")]);
        let meta = meta_for(&bytes);
        let open =
            |_: &str| -> Result<Box<dyn Read>> { Ok(Box::new(io::Cursor::new(bytes.clone()))) };
        install(
            &app,
            cache.path(),
            "https://example.test/a.zip",
            &meta,
            "2.4.0.0",
            &deps_for(&open, &RealOps),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(installed_version(&app).as_deref(), Some("2.4.0.0"));
    }

    fn deps_for<'a>(
        open: &'a dyn Fn(&str) -> Result<Box<dyn Read>>,
        ops: &'a dyn SwapOps,
    ) -> Deps<'a> {
        Deps {
            open,
            sign: &ok_step,
            verify: &ok_step,
            ops,
        }
    }

    #[test]
    fn install_runs_every_phase_and_cleans_the_download() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        let cache = tempfile::tempdir().unwrap();
        let bytes = zip_bytes(&[(BINARY_NAME, b"new-binary"), ("LICENSE", b"l")]);
        let meta = meta_for(&bytes);
        let urls = RefCell::new(Vec::new());
        let open = |url: &str| -> Result<Box<dyn Read>> {
            urls.borrow_mut().push(url.to_owned());
            Ok(Box::new(io::Cursor::new(bytes.clone())))
        };
        let mut phases = Vec::new();
        install(
            &app,
            cache.path(),
            "https://example.test/a.zip",
            &meta,
            "2.2.2.0",
            &deps_for(&open, &RealOps),
            &mut |phase| phases.push(phase),
        )
        .unwrap();
        assert_eq!(*urls.borrow(), ["https://example.test/a.zip"]);
        assert_eq!(binary_of(&app), b"new-binary");
        assert_eq!(binary_of(&dir.path().join(BACKUP_NAME)), b"old");
        assert!(matches!(phases.first(), Some(Phase::Downloading { .. })));
        assert_eq!(
            &phases[phases.len() - 2..],
            [Phase::Verifying, Phase::Installing]
        );
        let leftovers = fs::read_dir(cache.path().join(WORK_DIR)).unwrap().count();
        assert_eq!(leftovers, 0, "temporary download deleted");
        assert!(stages_in(dir.path()).is_empty());
    }

    #[test]
    fn install_aborts_without_changes_on_any_bad_input() {
        let bytes = zip_bytes(&[(BINARY_NAME, b"new-binary")]);
        let good = meta_for(&bytes);
        let https = "https://example.test/a.zip";
        let run = |url: &str, meta: &ArchiveMeta, served: Vec<u8>| {
            let dir = tempfile::tempdir().unwrap();
            let app = fake_app(dir.path());
            let cache = tempfile::tempdir().unwrap();
            let open = |_: &str| -> Result<Box<dyn Read>> {
                Ok(Box::new(io::Cursor::new(served.clone())))
            };
            let result = install(
                &app,
                cache.path(),
                url,
                meta,
                "2.2.2.0",
                &deps_for(&open, &RealOps),
                &mut |_| {},
            );
            if result.is_err() {
                assert_eq!(binary_of(&app), b"old");
                assert!(!dir.path().join(BACKUP_NAME).exists());
                assert!(stages_in(dir.path()).is_empty());
                let leftovers =
                    fs::read_dir(cache.path().join(WORK_DIR)).map_or(0, Iterator::count);
                assert_eq!(leftovers, 0, "temporary download deleted");
            }
            result
        };
        assert!(run(https, &good, bytes.clone()).is_ok());
        assert!(run("http://example.test/a.zip", &good, bytes.clone()).is_err());
        let bad_hash = ArchiveMeta {
            sha256: GOOD_HASH.into(),
            ..good.clone()
        };
        assert!(run(https, &bad_hash, bytes.clone()).is_err());
        assert!(run(https, &good, bytes[..bytes.len() - 1].to_vec()).is_err());
        assert!(run(https, &good, [bytes.clone(), vec![0]].concat()).is_err());
        // Correct hash, but the archive holds an unexpected entry.
        let evil = zip_bytes(&[(BINARY_NAME, b"x"), ("../evil", b"y")]);
        assert!(run(https, &meta_for(&evil), evil).is_err());
        // Correct hash, signing fails: nothing is swapped.
        let dir = tempfile::tempdir().unwrap();
        let app = fake_app(dir.path());
        let cache = tempfile::tempdir().unwrap();
        let open =
            |_: &str| -> Result<Box<dyn Read>> { Ok(Box::new(io::Cursor::new(bytes.clone()))) };
        let fail = |_: &Path| -> Result<()> { bail!("no codesign") };
        let deps = Deps {
            open: &open,
            sign: &fail,
            verify: &ok_step,
            ops: &RealOps,
        };
        assert!(
            install(
                &app,
                cache.path(),
                https,
                &good,
                "2.2.2.0",
                &deps,
                &mut |_| {}
            )
            .is_err()
        );
        assert_eq!(binary_of(&app), b"old");
        assert!(stages_in(dir.path()).is_empty());
        // Open failure (offline) changes nothing either.
        let offline = |_: &str| -> Result<Box<dyn Read>> { bail!("offline") };
        let deps = deps_for(&offline, &RealOps);
        assert!(
            install(
                &app,
                cache.path(),
                https,
                &good,
                "2.2.2.0",
                &deps,
                &mut |_| {}
            )
            .is_err()
        );
    }
}
