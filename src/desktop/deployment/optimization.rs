use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, ensure};

use super::{
    bounded_read, phase, read_report_configuration, report_image_relative, validate_suite_id,
};
use crate::helpers::png::{
    MAX_VISUAL_IMAGE_DECOMPRESSED_BYTES, read_bounded_png_dimensions_from_reader,
};

const MIB: u64 = 1024 * 1024;
const QUALITY_STEPS: [u8; 8] = [99, 95, 90, 85, 80, 70, 60, 50];
const MIN_COMPARISON_QUALITY: u8 = 80;
/// The lowest quality a full deployment reduces passed comparisons to.
pub(super) const LOWEST_QUALITY: u8 = 50;
/// The lowest quality while a Cloudflare site still keeps suites without local files: the
/// deployment brings back more suites before going lower, as a full deployment reduces every
/// image at one floor before the next.
pub(super) const SPREAD_QUALITY: u8 = 85;
/// Images recompressed at once. Each holds a decoded image (up to
/// `MAX_VISUAL_IMAGE_DECOMPRESSED_BYTES`) plus oxipng's working copies, which bounds memory.
const PARALLEL_IMAGES: usize = 4;

/// What an optimization must fit and how it may change the report to get there.
#[derive(Clone, Copy)]
pub(super) struct Request<'a> {
    /// The storage budget, in uncompressed bytes.
    pub limit: u64,
    pub deadline: Instant,
    /// Suites the snapshot manifest lists that the site keeps without local files: their data
    /// is neither read nor rewritten, and `limit` is what they leave of the provider's budget.
    pub kept: &'a BTreeSet<String>,
    /// Without one, images are only recompressed losslessly; with one, quality is reduced no
    /// lower than it (nor than the comparison minimum).
    pub quality_floor: Option<u8>,
    /// Reuses results across attempts and deployments.
    pub cache: Option<&'a OptimizationCache>,
}

static NOTHING_KEPT: BTreeSet<String> = BTreeSet::new();

impl Request<'_> {
    /// Fits `limit` by `deadline` down to a full deployment's quality floors, with nothing
    /// kept and nothing cached.
    pub(super) fn new(limit: u64, deadline: Instant) -> Self {
        Self {
            limit,
            deadline,
            kept: &NOTHING_KEPT,
            quality_floor: Some(LOWEST_QUALITY),
            cache: None,
        }
    }
}

/// The report cannot be optimized into its storage limit, which it still exceeds by
/// `remaining` bytes. A provider whose site keeps suites from earlier deployments may still
/// fit once those suites can be optimized too.
#[derive(Debug)]
pub(super) struct DoesNotFit {
    message: String,
    pub remaining: u64,
}

impl std::fmt::Display for DoesNotFit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DoesNotFit {}

/// Bumped whenever recompression or quantization settings change (`lossless_options`,
/// `quantize`), so stored results from other settings are never reused.
const CACHE_FORMAT: u32 = 1;
/// Bytes of optimization results the desktop app keeps across deployments; the least recently
/// used go first.
const CACHE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Image optimization results by source content. A Cloudflare report deployment that retries
/// over its budget recompresses and reduces the same images in every attempt, and later
/// deployments regenerate the same suites again. oxipng's output depends only on the bytes and
/// the fixed `lossless_options`, and a quantization only on the bytes and its quality, so
/// results are reused as they are.
///
/// A persistent cache lives in the desktop app's cache directory, shared by deployments and
/// pruned to `CACHE_BYTES` when opened; since it outlives this process, a stored result is only
/// reused as a PNG of the source's dimensions. The default cache is temporary and removed with
/// it.
#[derive(Default)]
pub(super) struct OptimizationCache {
    /// Where persistent results go; without one, a temporary directory is used.
    root: Option<PathBuf>,
    directory: std::sync::Mutex<Option<CacheDirectory>>,
    recompressions: CacheCounts,
    quantizations: CacheCounts,
}

/// Lookups and reused results, which deployment logs report per attempt so a cache that does
/// not help shows.
#[derive(Default)]
struct CacheCounts {
    lookups: std::sync::atomic::AtomicUsize,
    reused: std::sync::atomic::AtomicUsize,
}

impl CacheCounts {
    fn record(&self, reused: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        self.lookups.fetch_add(1, Relaxed);
        if reused {
            self.reused.fetch_add(1, Relaxed);
        }
    }

    /// Lookups and reused results so far.
    fn get(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::Relaxed;
        (self.lookups.load(Relaxed), self.reused.load(Relaxed))
    }
}

/// Logs how many of the lookups since `before` an earlier attempt or deployment answered.
fn report_reuse(counts: &CacheCounts, before: (usize, usize), work: &str) {
    let (lookups, reused) = counts.get();
    let (lookups, reused) = (lookups - before.0, reused - before.1);
    if lookups > 0 {
        phase(&format!(
            "{work} reused {reused} of {lookups} results from the optimization cache"
        ));
    }
}

enum CacheDirectory {
    Persistent(PathBuf),
    Temporary(tempfile::TempDir),
}

impl CacheDirectory {
    fn path(&self) -> &Path {
        match self {
            Self::Persistent(path) => path,
            Self::Temporary(directory) => directory.path(),
        }
    }
}

impl OptimizationCache {
    /// Results in the desktop app's cache directory, shared with later deployments.
    pub(super) fn persistent() -> Self {
        Self {
            root: crate::desktop::project_directories().map(|directories| {
                directories
                    .cache_dir()
                    .join("image-optimization")
                    .join(format!("v{CACHE_FORMAT}"))
            }),
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn at(root: PathBuf) -> Self {
        Self {
            root: Some(root),
            ..Self::default()
        }
    }

    fn directory(&self) -> Option<PathBuf> {
        let mut directory = self.directory.lock().ok()?;
        if directory.is_none() {
            *directory = self.open();
        }
        directory
            .as_ref()
            .map(|directory| directory.path().to_path_buf())
    }

    fn open(&self) -> Option<CacheDirectory> {
        if let Some(root) = &self.root
            && fs::create_dir_all(root).is_ok()
        {
            prune(root, CACHE_BYTES);
            return Some(CacheDirectory::Persistent(root.clone()));
        }
        tempfile::Builder::new()
            .prefix("regressify-optimization-")
            .tempdir()
            .ok()
            .map(CacheDirectory::Temporary)
    }

    /// `source` recompressed losslessly, from an earlier result for the same bytes when there
    /// is one. Results that do not shrink the image are recorded too, so they are not retried.
    fn recompress(&self, source: &[u8], options: &oxipng::Options) -> Result<Vec<u8>> {
        let Some(directory) = self.directory() else {
            return Ok(oxipng::optimize_from_memory(source, options)?);
        };
        let key = content_key(source);
        let smaller = directory.join(format!("{key}.png"));
        let unchanged = directory.join(format!("{key}.same"));
        if let Some(bytes) = stored(&smaller, png_dimensions(source)) {
            self.recompressions.record(true);
            return Ok(bytes);
        }
        if marked(&unchanged) {
            self.recompressions.record(true);
            return Ok(source.to_vec());
        }
        self.recompressions.record(false);
        let optimized = oxipng::optimize_from_memory(source, options)?;
        // The cache only saves time: a result it cannot store is recomputed when needed again.
        // Atomic writes keep concurrent workers with the same image from reading a partial file.
        let _ = if optimized.len() < source.len() {
            crate::helpers::write_file_atomically(&smaller, &optimized)
        } else {
            crate::helpers::write_file_atomically(&unchanged, b"")
        };
        Ok(optimized)
    }

    /// The image with content `key` and `dimensions` quantized at `quality`, which `compute`
    /// produces, from an earlier result when there is one. Qualities an image cannot reach are
    /// recorded too.
    fn quantize(
        &self,
        key: &str,
        dimensions: Option<(u32, u32)>,
        quality: u8,
        compute: impl FnOnce() -> Result<Option<Vec<u8>>>,
    ) -> Result<Option<Vec<u8>>> {
        let Some(directory) = self.directory() else {
            return compute();
        };
        let reduced = directory.join(format!("{key}-q{quality}.png"));
        let unreachable = directory.join(format!("{key}-q{quality}.none"));
        if let Some(bytes) = stored(&reduced, dimensions) {
            self.quantizations.record(true);
            return Ok(Some(bytes));
        }
        if marked(&unreachable) {
            self.quantizations.record(true);
            return Ok(None);
        }
        self.quantizations.record(false);
        let result = compute()?;
        // As for recompression, a result the cache cannot store only costs time later.
        let _ = match &result {
            Some(bytes) => crate::helpers::write_file_atomically(&reduced, bytes),
            None => crate::helpers::write_file_atomically(&unreachable, b""),
        };
        Ok(result)
    }
}

/// A stored result when it is a PNG of the source's `dimensions`, marked as recently used.
fn stored(path: &Path, dimensions: Option<(u32, u32)>) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    if dimensions.is_none() || png_dimensions(&bytes) != dimensions {
        return None;
    }
    touch(path);
    Some(bytes)
}

/// Whether a marker result exists, marking it as recently used.
fn marked(path: &Path) -> bool {
    let exists = path.is_file();
    if exists {
        touch(path);
    }
    exists
}

/// Records a use for `prune`, which keeps the most recently used results.
fn touch(path: &Path) {
    let _ = fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(std::time::SystemTime::now()));
}

/// Removes the least recently used results until `directory` holds at most `limit` bytes.
/// Files another deployment holds open may refuse removal; they are skipped.
fn prune(directory: &Path, limit: u64) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut files = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                (
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                    metadata.len(),
                    entry.path(),
                )
            })
        })
        .collect::<Vec<_>>();
    let mut total = files
        .iter()
        .fold(0_u64, |total, (_, size, _)| total.saturating_add(*size));
    if total <= limit {
        return;
    }
    files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    for (_, size, path) in files {
        if total <= limit {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Width and height from a PNG's header, or none for anything else.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    if bytes.get(..8)? != SIGNATURE || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let number = |range: std::ops::Range<usize>| {
        Some(u32::from_be_bytes(bytes.get(range)?.try_into().ok()?))
    };
    Some((number(16..20)?, number(20..24)?))
}

fn content_key(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// The fixed lossless recompression settings: pixels, transparency, interlacing, and chunks
/// stay as they are. Bump `CACHE_FORMAT` when they change.
fn lossless_options() -> oxipng::Options {
    let mut options = oxipng::Options::from_preset(1);
    options.optimize_alpha = false;
    options.scale_16 = false;
    options.interlace = None;
    options.strip = oxipng::StripChunks::None;
    options.max_decompressed_size = Some(MAX_VISUAL_IMAGE_DECOMPRESSED_BYTES);
    options
}

pub(super) struct PreparedReport {
    source: PathBuf,
    temporary: Option<tempfile::TempDir>,
    original_bytes: u64,
}

impl PreparedReport {
    /// The report at `source` as it is, for deployments without a storage budget.
    pub fn unchanged(source: &Path, original_bytes: u64) -> Self {
        Self {
            source: source.to_path_buf(),
            temporary: None,
            original_bytes,
        }
    }

    pub fn path(&self) -> &Path {
        self.temporary
            .as_ref()
            .map_or(&self.source, |directory| directory.path())
    }

    pub fn original_bytes(&self) -> u64 {
        self.original_bytes
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    Passed,
    Comparison,
    #[default]
    Protected,
}

struct ReportFile {
    relative: PathBuf,
    bytes: u64,
    priority: Priority,
    quality: u8,
    optimizable: bool,
}

fn inventory(root: &Path) -> Result<(Vec<ReportFile>, u64)> {
    let mut files = Vec::new();
    let mut total = 0_u64;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry.context("Cannot inspect deployment content")?;
        ensure!(
            !entry.file_type().is_symlink(),
            "Deployment content cannot contain symlinks"
        );
        if entry.file_type().is_dir() {
            continue;
        }
        ensure!(
            entry.file_type().is_file(),
            "Deployment content must contain regular files"
        );
        let bytes = entry.metadata()?.len();
        total = total
            .checked_add(bytes)
            .context("Deployment content size overflow")?;
        files.push(ReportFile {
            relative: entry.path().strip_prefix(root)?.to_path_buf(),
            bytes,
            priority: Priority::Protected,
            quality: 100,
            optimizable: true,
        });
        ensure!(
            files.len() <= 100_000,
            "Deployment exceeds the 100000-file package limit; select fewer suites"
        );
    }
    Ok((files, total))
}

fn check_deadline(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "Report optimization reached the deployment deadline; deploy fewer suites or retry. Original reports and baselines are unchanged."
    );
    Ok(())
}

fn classify_images(
    root: &Path,
    files: &mut [ReportFile],
    deadline: Instant,
    kept: &BTreeSet<String>,
) -> Result<()> {
    if let Some(roles) = crate::snapshot::image_roles(root, kept)? {
        for file in files {
            check_deadline(deadline)?;
            file.priority = match roles.get(&file.relative) {
                Some(crate::snapshot::ImageRole::Passed) => Priority::Passed,
                Some(crate::snapshot::ImageRole::Comparison) => Priority::Comparison,
                _ => Priority::Protected,
            };
        }
        return Ok(());
    }
    let mut priorities = BTreeMap::<PathBuf, Priority>::new();
    for file in files
        .iter()
        .filter(|file| file.relative.ends_with("html_report/config.js"))
    {
        check_deadline(deadline)?;
        ensure!(
            file.relative.components().count() == 3,
            "Invalid deployment report layout"
        );
        let suite = file
            .relative
            .iter()
            .next()
            .and_then(|value| value.to_str())
            .context("Report suite path must be UTF-8")?;
        validate_suite_id(suite)?;
        let data = read_report_configuration(&root.join(&file.relative))?;
        ensure!(
            data["id"].as_str() == Some(suite),
            "Report suite identity changed"
        );
        for test in data["tests"].as_array().context("Report has no tests")? {
            let error = test["pair"]
                .get("engineErrorMsg")
                .is_some_and(|value| !value.is_null() && value.as_str() != Some(""));
            for key in ["reference", "test", "diffImage"] {
                let Some(image) = test["pair"][key].as_str().filter(|image| !image.is_empty())
                else {
                    continue;
                };
                let priority = if error {
                    Priority::Protected
                } else if test["status"] == "pass" {
                    Priority::Passed
                } else if key == "diffImage" {
                    Priority::Protected
                } else {
                    Priority::Comparison
                };
                priorities
                    .entry(report_image_relative(suite, key, image)?)
                    .and_modify(|previous| *previous = (*previous).max(priority))
                    .or_insert(priority);
            }
        }
    }
    for file in files {
        file.priority = priorities.get(&file.relative).copied().unwrap_or_default();
    }
    Ok(())
}

fn read_png(root: &Path, relative: &Path) -> Result<Vec<u8>> {
    let path = super::artifact_source(root, relative, false)?
        .context("Deployment image is unavailable")?;
    let bytes = bounded_read(fs::File::open(&path)?, 128 * MIB as usize, "Deployment PNG")?;
    read_bounded_png_dimensions_from_reader(bytes.as_slice(), &path)?;
    Ok(bytes)
}

fn quantize(
    image: &lodepng::Bitmap<rgb::RGBA8>,
    quality: u8,
    deadline: Instant,
) -> Result<Option<Vec<u8>>> {
    check_deadline(deadline)?;
    // Stored quantizations depend on these settings: bump `CACHE_FORMAT` when they change.
    let mut attributes = imagequant::new();
    attributes.set_speed(6)?;
    attributes.set_quality(quality, quality)?;
    let progress = move |_| {
        if Instant::now() < deadline {
            imagequant::ControlFlow::Continue
        } else {
            imagequant::ControlFlow::Break
        }
    };
    attributes.set_progress_callback(progress);
    let mut input = attributes.new_image_borrowed(&image.buffer, image.width, image.height, 0.0)?;
    let mut result = match attributes.quantize(&mut input) {
        Ok(result) => result,
        Err(imagequant::Error::QualityTooLow) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    result.set_dithering_level(1.0)?;
    result.set_progress_callback(progress);
    let (palette, indices) = result.remapped(&mut input)?;
    if result
        .remapping_quality()
        .is_some_and(|actual| actual < quality)
    {
        return Ok(None);
    }
    let mut encoder = lodepng::Encoder::new();
    encoder.set_palette(&palette)?;
    Ok(Some(encoder.encode(&indices, image.width, image.height)?))
}

fn refine_candidate(
    mut bytes: Vec<u8>,
    mut quality: u8,
    previous_quality: u8,
    target_bytes: u64,
    mut encode: impl FnMut(u8) -> Result<Option<Vec<u8>>>,
) -> Result<(Vec<u8>, u8)> {
    if bytes.len() as u64 <= target_bytes {
        for higher in (quality + 1..previous_quality).rev() {
            if let Some(candidate) = encode(higher)?
                && candidate.len() as u64 <= target_bytes
            {
                bytes = candidate;
                quality = higher;
                break;
            }
        }
    }
    Ok((bytes, quality))
}

fn reduce_images(
    root: &Path,
    destination: &Path,
    files: &mut [ReportFile],
    total: &mut u64,
    skipped: &mut usize,
    request: Request,
) -> Result<()> {
    let Request {
        limit,
        deadline,
        quality_floor,
        cache,
        ..
    } = request;
    let Some(floor) = quality_floor else {
        return Ok(());
    };
    for priority in [Priority::Passed, Priority::Comparison] {
        let mut previous_quality = 100;
        for quality in QUALITY_STEPS {
            if *total <= limit
                || quality < floor
                || priority == Priority::Comparison && quality < MIN_COMPARISON_QUALITY
            {
                break;
            }
            let label = if priority == Priority::Passed {
                "passed comparisons"
            } else {
                "failed Reference/Test images"
            };
            let candidates = files
                .iter()
                .filter(|file| file.priority == priority && file.optimizable)
                .count();
            if candidates == 0 {
                break;
            }
            phase(&format!(
                "Optimizing {label}: quality floor {quality}, {:.2} MiB still needed",
                (*total - limit) as f64 / MIB as f64
            ));
            files.sort_by(|left, right| {
                right
                    .bytes
                    .cmp(&left.bytes)
                    .then_with(|| left.relative.cmp(&right.relative))
            });
            let mut last_update = Instant::now();
            for (index, file) in files
                .iter_mut()
                .filter(|file| file.priority == priority && file.optimizable)
                .enumerate()
            {
                check_deadline(deadline)?;
                if *total <= limit {
                    break;
                }
                if index == 0 || last_update.elapsed().as_secs() >= 1 {
                    phase(&format!(
                        "Optimizing {label}: processing image {} of {candidates} ({}, {:.1} KiB) at quality floor {quality}; report {:.2} MiB",
                        index + 1,
                        file.relative.display(),
                        file.bytes as f64 / 1024.0,
                        *total as f64 / MIB as f64
                    ));
                    last_update = Instant::now();
                }
                let result = (|| -> Result<Option<(Vec<u8>, u8)>> {
                    let source = read_png(root, &file.relative)?;
                    let key = cache.map(|_| (content_key(&source), png_dimensions(&source)));
                    // Decoded only when a quality is not cached yet.
                    let mut decoded = None;
                    let mut encode = |quality: u8| -> Result<Option<Vec<u8>>> {
                        let mut compute = || {
                            let image = match decoded.take() {
                                Some(image) => image,
                                None => lodepng::decode32(&source)?,
                            };
                            let result = quantize(&image, quality, deadline);
                            decoded = Some(image);
                            result
                        };
                        match (cache, &key) {
                            (Some(cache), Some((key, dimensions))) => {
                                cache.quantize(key, *dimensions, quality, compute)
                            }
                            _ => compute(),
                        }
                    };
                    let Some(bytes) = encode(quality)? else {
                        return Ok(None);
                    };
                    if bytes.len() as u64 >= file.bytes {
                        return Ok(None);
                    }
                    let target_bytes = file.bytes.saturating_sub(*total - limit);
                    Ok(Some(refine_candidate(
                        bytes,
                        quality,
                        previous_quality,
                        target_bytes,
                        &mut encode,
                    )?))
                })();
                check_deadline(deadline)?;
                if index == 0
                    || index + 1 == candidates
                    || (index + 1).is_multiple_of(10)
                    || last_update.elapsed().as_secs() >= 1
                {
                    phase(&format!(
                        "Optimizing {label}: image {} of {candidates}, quality floor {quality}; report {:.2} MiB",
                        index + 1,
                        *total as f64 / MIB as f64
                    ));
                    last_update = Instant::now();
                }
                match result {
                    Ok(Some((bytes, applied_quality))) => {
                        crate::helpers::write_file_atomically(
                            &destination.join(&file.relative),
                            &bytes,
                        )?;
                        let previous_bytes = file.bytes;
                        *total -= file.bytes - bytes.len() as u64;
                        file.bytes = bytes.len() as u64;
                        file.quality = applied_quality;
                        phase(&format!(
                            "Temporary image optimized: {} ({:.1} -> {:.1} KiB); quality floor {applied_quality}; report {:.2} MiB",
                            file.relative.display(),
                            previous_bytes as f64 / 1024.0,
                            file.bytes as f64 / 1024.0,
                            *total as f64 / MIB as f64
                        ));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        file.optimizable = false;
                        *skipped += 1;
                        phase(&format!(
                            "Keeping image unchanged: {} ({error})",
                            file.relative.display()
                        ));
                    }
                }
            }
            previous_quality = quality;
        }
    }
    Ok(())
}

pub(super) fn prepare(root: &Path, limit: u64, deadline: Instant) -> Result<PreparedReport> {
    check_deadline(deadline)?;
    phase("Measuring uncompressed deployment content");
    let (files, original_bytes) = inventory(root)?;
    prepare_report(root, files, original_bytes, Request::new(limit, deadline))
}

/// Optimizes the selected `paths` of the snapshot site at `root` as `request` asks.
pub(super) fn prepare_files(
    root: &Path,
    paths: impl IntoIterator<Item = PathBuf>,
    request: Request,
) -> Result<PreparedReport> {
    let mut files = Vec::new();
    let mut total = 0_u64;
    check_deadline(request.deadline)?;
    phase("Measuring uncompressed deployment content");
    for relative in paths {
        check_deadline(request.deadline)?;
        let source = super::artifact_source(root, &relative, false)?
            .context("Deployment file is unavailable")?;
        let bytes = fs::metadata(source)?.len();
        total = total
            .checked_add(bytes)
            .context("Deployment content size overflow")?;
        files.push(ReportFile {
            relative,
            bytes,
            priority: Priority::Protected,
            quality: 100,
            optimizable: true,
        });
        ensure!(
            files.len() <= 100_000,
            "Deployment exceeds the 100000-file package limit; select fewer suites"
        );
    }
    prepare_report(root, files, total, request)
}

fn prepare_report(
    root: &Path,
    files: Vec<ReportFile>,
    original_bytes: u64,
    request: Request,
) -> Result<PreparedReport> {
    use crate::diagnostics::{record::*, runtime};
    runtime::stage_sync(
        Stage::PackagingOptimization,
        [
            (Field::BytesBefore, FieldValue::Number(original_bytes)),
            (Field::BudgetBytes, FieldValue::Number(request.limit)),
            (Field::FileCount, FieldValue::Number(files.len() as u64)),
            (
                Field::Compression,
                FieldValue::Boolean(original_bytes > request.limit),
            ),
        ],
        || prepare_report_inner(root, files, original_bytes, request),
    )
}

fn prepare_report_inner(
    root: &Path,
    mut files: Vec<ReportFile>,
    original_bytes: u64,
    request: Request,
) -> Result<PreparedReport> {
    let Request {
        limit,
        deadline,
        kept,
        quality_floor,
        cache,
    } = request;
    check_deadline(deadline)?;
    let log_files = files
        .iter()
        .filter(|file| file.relative.starts_with("_snapshot/data/logs"))
        .collect::<Vec<_>>();
    let log_bytes = log_files.iter().map(|file| file.bytes).sum::<u64>();
    if log_bytes > limit {
        return Err(DoesNotFit {
            remaining: original_bytes.saturating_sub(limit),
            message: format!(
            "Capture log assets require {} MiB, exceeding this provider's {} MiB storage budget (largest log: {}). Select fewer suites or use a larger provider allowance; logs are never truncated.",
            log_bytes.div_ceil(MIB),
            limit / MIB,
            log_files
                .iter()
                .max_by_key(|file| file.bytes)
                .map(|file| file.relative.display().to_string())
                .unwrap_or_default()
            ),
        }
        .into());
    }
    files.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.relative.cmp(&right.relative))
    });
    phase(&format!(
        "Deployment content: {} files, {:.2} MiB; storage budget: {:.2} MiB (uncompressed)",
        files.len(),
        original_bytes as f64 / MIB as f64,
        limit as f64 / MIB as f64
    ));
    let mut prepared = PreparedReport {
        source: root.to_path_buf(),
        temporary: None,
        original_bytes,
    };
    if original_bytes <= limit {
        use crate::diagnostics::{record::*, runtime};
        runtime::event(
            Stage::PackagingOptimization,
            DiagnosticEvent::Decision,
            Severity::Debug,
            [
                (Field::BytesAfter, FieldValue::Number(original_bytes)),
                (Field::Compression, FieldValue::Boolean(false)),
            ],
        );
        phase("Deployment content fits within the storage budget; skipping image optimization");
        return Ok(prepared);
    }

    let source_root = root.canonicalize()?;
    phase("Classifying report images by comparison role for optimization");
    classify_images(&source_root, &mut files, deadline, kept)?;
    let temporary = tempfile::Builder::new()
        .prefix(".optimization-")
        .tempdir_in(
            source_root
                .parent()
                .context("Deployment staging directory has no parent")?,
        )
        .context(
            "Cannot create temporary report optimization directory; check available disk space",
        )?;
    phase("Creating an isolated temporary copy for report optimization");
    let mut copied_bytes = 0_u64;
    let mut last_update = Instant::now();
    for (index, file) in files.iter().enumerate() {
        check_deadline(deadline)?;
        let destination = temporary.path().join(&file.relative);
        fs::create_dir_all(
            destination
                .parent()
                .context("Deployment file has no parent")?,
        )?;
        let source = super::artifact_source(&source_root, &file.relative, false)?
            .context("Deployment file is unavailable")?;
        let copied = fs::copy(source, &destination).with_context(|| {
            format!(
                "Cannot copy '{}' to temporary deployment storage; check available disk space",
                file.relative.display()
            )
        })?;
        ensure!(
            copied == file.bytes,
            "Report changed while preparing deployment; regenerate it and retry"
        );
        copied_bytes += copied;
        if index == 0
            || index + 1 == files.len()
            || (index + 1).is_multiple_of(50)
            || last_update.elapsed().as_secs() >= 1
        {
            phase(&format!(
                "Temporary optimization copy: {} of {} files ({:.2} MiB)",
                index + 1,
                files.len(),
                copied_bytes as f64 / MIB as f64
            ));
            last_update = Instant::now();
        }
    }
    prepared.temporary = Some(temporary);
    let mut total = original_bytes;
    let mut skipped = 0;
    let mut options = lossless_options();
    let recompressions = cache.map(|cache| cache.recompressions.get());
    let image_count = files
        .iter()
        .filter(|file| {
            file.relative
                .extension()
                .is_some_and(|extension| extension == "png")
        })
        .count();
    phase(&format!(
        "Optimizing {image_count} temporary report images losslessly ({:.2} MiB over budget)",
        (total - limit) as f64 / MIB as f64
    ));
    let mut processed = 0_usize;
    let mut last_update = Instant::now();
    let pngs = files
        .iter()
        .enumerate()
        .filter(|(_, file)| {
            file.relative
                .extension()
                .is_some_and(|extension| extension == "png")
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, PARALLEL_IMAGES);
    // Largest images first, a few at a time. Results apply in order and stop as soon as the
    // report fits, so exactly the files one-at-a-time optimization would change are changed;
    // a batch's later results are discarded then.
    'batches: for batch in pngs.chunks(workers) {
        check_deadline(deadline)?;
        if total <= limit {
            break;
        }
        if processed == 0 || last_update.elapsed().as_secs() >= 1 {
            let file = &files[batch[0]];
            phase(&format!(
                "Lossless optimization: processing image {} of {image_count} ({}, {:.1} KiB); report {:.2} MiB",
                processed + 1,
                file.relative.display(),
                file.bytes as f64 / 1024.0,
                total as f64 / MIB as f64
            ));
            last_update = Instant::now();
        }
        options.timeout = Some(deadline.saturating_duration_since(Instant::now()));
        let results = {
            use rayon::prelude::*;
            let (files, options, source_root) = (&files, &options, &source_root);
            batch
                .par_iter()
                .map(|&index| -> Result<Vec<u8>> {
                    let source = read_png(source_root, &files[index].relative)?;
                    match cache {
                        Some(cache) => cache.recompress(&source, options),
                        None => Ok(oxipng::optimize_from_memory(&source, options)?),
                    }
                })
                .collect::<Vec<_>>()
        };
        check_deadline(deadline)?;
        for (&index, optimized) in batch.iter().zip(results) {
            if total <= limit {
                break 'batches;
            }
            let file = &mut files[index];
            processed += 1;
            match optimized {
                Ok(bytes) if (bytes.len() as u64) < file.bytes => {
                    crate::helpers::write_file_atomically(
                        &prepared.path().join(&file.relative),
                        &bytes,
                    )?;
                    total -= file.bytes - bytes.len() as u64;
                    file.bytes = bytes.len() as u64;
                }
                Ok(_) => {}
                Err(error) => {
                    skipped += 1;
                    file.optimizable = false;
                    phase(&format!(
                        "Keeping image unchanged: {} ({error})",
                        file.relative.display()
                    ));
                }
            }
            if processed == 1
                || processed == image_count
                || processed.is_multiple_of(10)
                || last_update.elapsed().as_secs() >= 1
            {
                phase(&format!(
                    "Lossless optimization: image {processed} of {image_count}; report {:.2} MiB",
                    total as f64 / MIB as f64
                ));
                last_update = Instant::now();
            }
        }
    }
    if let (Some(cache), Some(before)) = (cache, recompressions) {
        report_reuse(&cache.recompressions, before, "Lossless recompression");
    }
    if total > limit && quality_floor.is_some() {
        let quantizations = cache.map(|cache| cache.quantizations.get());
        reduce_images(
            &source_root,
            prepared.path(),
            &mut files,
            &mut total,
            &mut skipped,
            request,
        )?;
        if let (Some(cache), Some(before)) = (cache, quantizations) {
            report_reuse(&cache.quantizations, before, "Quality reduction");
        }
    }
    if total > limit {
        return Err(DoesNotFit {
            remaining: total - limit,
            message: format!(
                "Report cannot fit the deployment storage limit: {:.2} MiB originally, {:.2} MiB after optimization, {:.2} MiB allowed ({skipped} images could not be optimized). Failed diffs, error placeholders, and image dimensions were preserved. Select fewer suites or use a larger deployment storage budget. Original reports and baselines are unchanged.",
                original_bytes as f64 / MIB as f64,
                total as f64 / MIB as f64,
                limit as f64 / MIB as f64
            ),
        }
        .into());
    }
    if crate::snapshot::image_roles(prepared.path(), kept)?.is_some() {
        check_deadline(deadline)?;
        phase("Refreshing snapshot cache versions for optimized images");
        crate::snapshot::refresh_site_versions(prepared.path(), kept)?;
        total = inventory(prepared.path())?.1;
        if total > limit {
            return Err(DoesNotFit {
                remaining: total - limit,
                message: "Updated snapshot metadata exceeds the deployment storage limit".into(),
            }
            .into());
        }
    }
    let reduced = files.iter().filter(|file| file.quality < 100).count();
    use crate::diagnostics::{record::*, runtime};
    runtime::event(
        Stage::PackagingOptimization,
        DiagnosticEvent::Decision,
        Severity::Info,
        [
            (Field::BytesAfter, FieldValue::Number(total)),
            (Field::Compression, FieldValue::Boolean(true)),
        ],
    );
    phase(&format!(
        "Temporary deployment report: {:.2} -> {:.2} MiB; {reduced} images quality-reduced; {skipped} skipped; original reports and baselines unchanged",
        original_bytes as f64 / MIB as f64,
        total as f64 / MIB as f64
    ));
    Ok(prepared)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;

    fn deadline() -> Instant {
        Instant::now() + std::time::Duration::from_secs(60)
    }

    fn sample_png() -> Result<Vec<u8>> {
        let mut state = 12345_u32;
        let pixels = (0..256 * 128)
            .map(|index| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let base = if index / 256 % 32 < 4 { 20 } else { 220 };
                rgb::RGBA8::new(
                    base + (state & 15) as u8,
                    base + ((state >> 8) & 15) as u8,
                    base + ((state >> 16) & 15) as u8,
                    if index < 256 { 0 } else { 255 },
                )
            })
            .collect::<Vec<_>>();
        Ok(lodepng::encode32(&pixels, 256, 128)?)
    }

    #[test]
    fn palette_reduction_preserves_dimensions_and_transparency() -> Result<()> {
        let original = sample_png()?;
        let image = lodepng::decode32(&original)?;
        let bytes =
            quantize(&image, 80, deadline())?.context("Fixture should meet quality floor")?;
        assert!(bytes.len() < original.len());
        let optimized = lodepng::decode32(&bytes)?;
        assert_eq!(
            (optimized.width, optimized.height),
            (image.width, image.height)
        );
        assert_eq!(optimized.buffer[0].a, 0);
        assert_eq!(optimized.buffer[256].a, 255);
        Ok(())
    }

    #[test]
    fn final_image_uses_the_highest_quality_that_actually_fits() -> Result<()> {
        let mut attempted = Vec::new();
        let (bytes, quality) = refine_candidate(vec![0; 40], 80, 90, 50, |quality| {
            attempted.push(quality);
            Ok(Some(vec![0; if quality == 88 { 49 } else { 60 }]))
        })?;
        assert_eq!(attempted, [89, 88]);
        assert_eq!(quality, 88);
        assert_eq!(bytes.len(), 49);
        Ok(())
    }

    pub(crate) fn report_fixture() -> Result<tempfile::TempDir> {
        let root = tempfile::tempdir()?;
        fs::create_dir_all(root.path().join("one/html_report"))?;
        fs::create_dir_all(root.path().join("one/bitmaps_reference"))?;
        fs::create_dir_all(root.path().join("one/bitmaps_test/run"))?;
        fs::write(root.path().join("index.html"), "report")?;
        fs::write(root.path().join("one/html_report/index.html"), "suite")?;
        let png = sample_png()?;
        let mut tests = Vec::new();
        for (label, status) in [("passed", "pass"), ("failed", "fail")] {
            fs::write(
                root.path()
                    .join(format!("one/bitmaps_reference/{label}.png")),
                &png,
            )?;
            fs::write(
                root.path()
                    .join(format!("one/bitmaps_test/run/{label}.png")),
                &png,
            )?;
            fs::write(
                root.path()
                    .join(format!("one/bitmaps_test/run/{label}-diff.png")),
                &png,
            )?;
            tests.push(json!({"status":status,"pair":{
                "reference":format!("../bitmaps_reference/{label}.png?original-hash"),
                "test":format!("../bitmaps_test/run/{label}.png?original-hash"),
                "diffImage":format!("../bitmaps_test/run/{label}-diff.png?original-hash"),
                "diff":{"misMatchPercentage":"12.34","isSameDimensions":true,
                    "dimensionDifference":{"width":0,"height":0}}}}));
        }
        fs::write(
            root.path().join("one/html_report/config.js"),
            format!("report({});", json!({"id":"one","tests":tests})),
        )?;
        Ok(root)
    }

    #[test]
    fn lossless_only_preparation_reports_what_still_does_not_fit() -> Result<()> {
        let root = report_fixture()?;
        let source = root.path().canonicalize()?;
        let (files, total) = inventory(&source)?;
        let before = files
            .iter()
            .map(|file| fs::read(source.join(&file.relative)))
            .collect::<std::io::Result<Vec<_>>>()?;
        let short = |quality_floor| -> Result<u64> {
            let error = prepare_files(
                &source,
                files.iter().map(|file| file.relative.clone()),
                Request {
                    quality_floor,
                    ..Request::new(1, deadline())
                },
            )
            .err()
            .context("a one-byte limit cannot fit")?;
            Ok(error
                .downcast_ref::<DoesNotFit>()
                .context("typed shortfall")?
                .remaining)
        };
        let lossless = short(None)?;
        // Lossless recompression saved something, and what it could not save is reported.
        assert!(lossless > 0 && lossless < total - 1);
        // Quality reduction saves more, so the lossless-only pass never ran it, and going down
        // to the lowest floor saves more than stopping at the spreading floor.
        let spread = short(Some(SPREAD_QUALITY))?;
        assert!(spread < lossless);
        assert!(short(Some(LOWEST_QUALITY))? < spread);
        let after = files
            .iter()
            .map(|file| fs::read(source.join(&file.relative)))
            .collect::<std::io::Result<Vec<_>>>()?;
        assert_eq!(before, after);
        Ok(())
    }

    #[test]
    fn lossless_results_are_reused_for_the_same_bytes() -> Result<()> {
        let cache = OptimizationCache::default();
        let options = lossless_options();
        let source = sample_png()?;
        let first = cache.recompress(&source, &options)?;
        assert!(first.len() < source.len());
        assert_eq!(first, oxipng::optimize_from_memory(&source, &options)?);
        let directory = cache.directory().context("cache directory")?;
        let entries = || -> Result<Vec<PathBuf>> {
            Ok(fs::read_dir(&directory)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::io::Result<_>>()?)
        };
        let stored = entries()?;
        assert_eq!(stored.len(), 1);
        // A later attempt takes the stored result instead of recompressing.
        let substitute =
            lodepng::encode32(&vec![rgb::RGBA8::new(1, 2, 3, 255); 256 * 128], 256, 128)?;
        fs::write(&stored[0], &substitute)?;
        assert_eq!(cache.recompress(&source, &options)?, substitute);
        // Anything but a PNG of the source's dimensions is recomputed and replaced.
        for invalid in [
            b"stored result".to_vec(),
            lodepng::encode32(&[rgb::RGBA8::default()], 1, 1)?,
        ] {
            fs::write(&stored[0], &invalid)?;
            assert_eq!(cache.recompress(&source, &options)?, first);
            assert_eq!(fs::read(&stored[0])?, first);
        }
        // Results that cannot shrink an image are remembered as such.
        let again = cache.recompress(&first, &options)?;
        assert!(again.len() >= first.len());
        assert_eq!(entries()?.len(), 2);
        assert_eq!(cache.recompress(&first, &options)?, first);
        // The directory goes with the cache.
        drop(cache);
        assert!(!directory.exists());
        Ok(())
    }

    #[test]
    fn quantizations_are_reused_for_the_same_bytes_and_quality() -> Result<()> {
        let source = sample_png()?;
        let dimensions = png_dimensions(&source);
        assert_eq!(dimensions, Some((256, 128)));
        let reduced = lodepng::encode32(&vec![rgb::RGBA8::new(9, 9, 9, 255); 256 * 128], 256, 128)?;
        let root = tempfile::tempdir()?;
        let computed = std::cell::Cell::new(0);
        let quantize = |cache: &OptimizationCache, quality, result: Option<&[u8]>| {
            cache.quantize("key", dimensions, quality, || {
                computed.set(computed.get() + 1);
                Ok(result.map(<[u8]>::to_vec))
            })
        };
        let first = OptimizationCache::at(root.path().join("cache"));
        assert_eq!(quantize(&first, 90, Some(&reduced))?, Some(reduced.clone()));
        // A later attempt takes the stored result without computing it again.
        assert_eq!(quantize(&first, 90, Some(&source))?, Some(reduced.clone()));
        // Each quality is its own result, and unreachable ones are remembered as such.
        assert_eq!(quantize(&first, 99, None)?, None);
        assert_eq!(quantize(&first, 99, Some(&source))?, None);
        assert_eq!(computed.get(), 2);
        // A persistent cache outlives its client: a later deployment reuses the results.
        drop(first);
        let later = OptimizationCache::at(root.path().join("cache"));
        assert_eq!(quantize(&later, 90, Some(&source))?, Some(reduced));
        assert_eq!(quantize(&later, 99, Some(&source))?, None);
        assert_eq!(computed.get(), 2);
        Ok(())
    }

    #[test]
    fn persistent_results_are_pruned_least_recently_used_first() -> Result<()> {
        let root = tempfile::tempdir()?;
        let now = std::time::SystemTime::now();
        for (name, age) in [("old", 300), ("middle", 200), ("recent", 100)] {
            let path = root.path().join(name);
            fs::write(&path, vec![0_u8; 100])?;
            fs::File::options()
                .write(true)
                .open(&path)?
                .set_modified(now - std::time::Duration::from_secs(age))?;
        }
        // A use makes "old" the most recent.
        touch(&root.path().join("old"));
        prune(root.path(), 250);
        let mut left = fs::read_dir(root.path())?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>>>()?;
        left.sort();
        assert_eq!(left, ["old", "recent"]);
        // Within the limit, nothing goes.
        prune(root.path(), 200);
        assert_eq!(fs::read_dir(root.path())?.count(), 2);
        Ok(())
    }

    #[test]
    fn cached_attempts_prepare_the_same_report() -> Result<()> {
        let root = report_fixture()?;
        let source = root.path().canonicalize()?;
        let (files, total) = inventory(&source)?;
        let shortfall = |cache: Option<&OptimizationCache>, quality_floor| -> Result<u64> {
            let error = prepare_files(
                &source,
                files.iter().map(|file| file.relative.clone()),
                Request {
                    quality_floor,
                    cache,
                    ..Request::new(1, deadline())
                },
            )
            .err()
            .context("a one-byte limit cannot fit")?;
            Ok(error
                .downcast_ref::<DoesNotFit>()
                .context("typed shortfall")?
                .remaining)
        };
        for quality_floor in [None, Some(LOWEST_QUALITY)] {
            let cache = OptimizationCache::default();
            let uncached = shortfall(None, quality_floor)?;
            assert!(uncached < total - 1);
            // The first attempt fills the cache and the second reuses it, with the same outcome.
            assert_eq!(shortfall(Some(&cache), quality_floor)?, uncached);
            let filled = (cache.recompressions.get(), cache.quantizations.get());
            assert!(filled.0.1 < filled.0.0);
            assert_eq!(shortfall(Some(&cache), quality_floor)?, uncached);
            // Every lookup of the second attempt was answered by the first.
            let (recompressions, quantizations) =
                (cache.recompressions.get(), cache.quantizations.get());
            assert_eq!(recompressions.0 - filled.0.0, recompressions.1 - filled.0.1);
            assert_eq!(quantizations.0 - filled.1.0, quantizations.1 - filled.1.1);
            assert_eq!(quantizations.0 > 0, quality_floor.is_some());
        }
        Ok(())
    }

    #[test]
    fn parallel_recompression_changes_only_the_images_needed_to_fit() -> Result<()> {
        let root = report_fixture()?;
        let (files, total) = inventory(root.path())?;
        // One byte over: the first image of the first batch is enough, even though its whole
        // batch was recompressed.
        let prepared = prepare(root.path(), total - 1, deadline())?;
        let changed = files
            .iter()
            .filter(|file| {
                fs::read(root.path().join(&file.relative)).ok()
                    != fs::read(prepared.path().join(&file.relative)).ok()
            })
            .map(|file| file.relative.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>();
        // Equal images go in path order.
        assert_eq!(changed, ["one/bitmaps_reference/failed.png"]);
        Ok(())
    }

    #[test]
    fn oversized_reports_reduce_passed_images_in_temp_and_preserve_failures_and_sources()
    -> Result<()> {
        let root = report_fixture()?;
        let (files, original_total) = inventory(root.path())?;
        let before = files
            .iter()
            .map(|file| {
                Ok((
                    file.relative.clone(),
                    fs::read(root.path().join(&file.relative))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let limit = original_total * 85 / 100;
        let prepared = prepare(root.path(), limit, deadline())?;
        assert_eq!(prepared.original_bytes(), original_total);
        assert_ne!(prepared.path(), root.path());
        assert!(inventory(prepared.path())?.1 <= limit);
        let package = super::super::package::Package::create(prepared.path(), 100_000)?;
        assert!(package.bytes <= limit);
        assert_eq!(package.files as usize, before.len());
        let mut archive = zip::ZipArchive::new(package.file.reopen()?)?;
        let mut reduced = 0;
        for (relative, bytes) in &before {
            assert_eq!(fs::read(root.path().join(relative))?, *bytes);
            let uploaded = fs::read(prepared.path().join(relative))?;
            let mut archived = Vec::new();
            std::io::Read::read_to_end(
                &mut archive.by_name(&relative.to_string_lossy().replace('\\', "/"))?,
                &mut archived,
            )?;
            assert_eq!(archived, uploaded);
            if relative
                .extension()
                .is_some_and(|extension| extension == "png")
            {
                let original = lodepng::decode32(bytes)?;
                let optimized = lodepng::decode32(&uploaded)?;
                assert_eq!(
                    (original.width, original.height),
                    (optimized.width, optimized.height)
                );
                assert_eq!(optimized.buffer[0].a, 0);
                if relative.to_string_lossy().contains("failed") {
                    assert_eq!(original.buffer, optimized.buffer);
                } else if original.buffer != optimized.buffer {
                    reduced += 1;
                }
            } else {
                assert_eq!(uploaded, *bytes);
            }
        }
        assert!(reduced > 0, "Fixture must require quality reduction");
        let temporary = prepared.path().to_path_buf();
        drop(prepared);
        assert!(!temporary.exists());
        Ok(())
    }

    #[test]
    fn shared_images_take_the_more_protective_priority() -> Result<()> {
        let root = report_fixture()?;
        let config = root.path().join("one/html_report/config.js");
        let mut data = read_report_configuration(&config)?;
        data["tests"][1]["pair"]["reference"] = data["tests"][0]["pair"]["reference"].clone();
        data["tests"][1]["pair"]["engineErrorMsg"] = json!("Capture failed");
        fs::write(config, format!("report({data});"))?;
        let (mut files, _) = inventory(root.path())?;
        classify_images(root.path(), &mut files, deadline(), &BTreeSet::new())?;
        let priorities = files
            .into_iter()
            .map(|file| (file.relative, file.priority))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            priorities[Path::new("one/bitmaps_reference/passed.png")],
            Priority::Protected
        );
        assert_eq!(
            priorities[Path::new("one/bitmaps_test/run/passed.png")],
            Priority::Passed
        );
        assert_eq!(
            priorities[Path::new("one/bitmaps_test/run/failed-diff.png")],
            Priority::Protected
        );
        Ok(())
    }

    #[test]
    fn fitting_reports_are_byte_identical_and_need_no_optimization_copy() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("index.html"), "report")?;
        let prepared = prepare(root.path(), 6, deadline())?;
        assert_eq!(prepared.original_bytes(), 6);
        assert!(prepared.temporary.is_none());
        assert_eq!(prepared.path(), root.path());
        assert_eq!(fs::read(root.path().join("index.html"))?, b"report");
        Ok(())
    }

    #[test]
    fn selected_files_validate_paths_without_measuring_unselected_content() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        fs::write(root.join("index.html"), "report")?;
        fs::write(root.join("unselected.txt"), "unselected content")?;
        let prepared = prepare_files(
            &root,
            [PathBuf::from("index.html")],
            Request::new(6, deadline()),
        )?;
        assert_eq!(prepared.path(), root);
        for relative in [
            PathBuf::from("../outside"),
            PathBuf::from("missing.png"),
            PathBuf::from("index.html?version"),
            root.join("index.html"),
        ] {
            assert!(prepare_files(&root, [relative], Request::new(6, deadline())).is_err());
        }
        assert_eq!(fs::read(root.join("index.html"))?, b"report");
        Ok(())
    }

    #[test]
    fn an_unfittable_report_never_changes_source_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("index.html"), "report")?;
        let path = root.path().join("capture.png");
        let pixels = vec![rgb::RGBA8::new(10, 20, 30, 255); 64 * 64];
        let original = lodepng::encode32(&pixels, 64, 64)?;
        fs::write(&path, &original)?;
        let error = prepare(root.path(), 1, deadline())
            .err()
            .context("Expected a size failure")?;
        let message = error.to_string();
        assert!(message.contains("Original reports and baselines are unchanged"));
        assert!(message.contains("deployment storage limit"));
        assert!(!message.contains("Azure"));
        assert_eq!(fs::read(path)?, original);
        assert_eq!(fs::read(root.path().join("index.html"))?, b"report");
        Ok(())
    }

    #[test]
    fn failed_reference_and_test_can_shrink_without_changing_failed_diffs() -> Result<()> {
        let root = report_fixture()?;
        let config = root.path().join("one/html_report/config.js");
        let mut data = read_report_configuration(&config)?;
        data["tests"][0]["status"] = json!("fail");
        fs::write(&config, format!("report({data});"))?;
        let (_, original_total) = inventory(root.path())?;
        let prepared = prepare(root.path(), original_total * 85 / 100, deadline())?;
        let mut reduced = 0;
        for file in inventory(root.path())?.0.into_iter().filter(|file| {
            file.relative
                .extension()
                .is_some_and(|extension| extension == "png")
        }) {
            let original = lodepng::decode32(&fs::read(root.path().join(&file.relative))?)?;
            let uploaded = lodepng::decode32(&fs::read(prepared.path().join(&file.relative))?)?;
            if file.relative.to_string_lossy().contains("-diff") {
                assert_eq!(original.buffer, uploaded.buffer);
            } else if original.buffer != uploaded.buffer {
                reduced += 1;
            }
        }
        assert!(reduced > 0);
        assert_eq!(
            read_report_configuration(&prepared.path().join("one/html_report/config.js"))?,
            data
        );
        Ok(())
    }

    #[test]
    fn expired_deadlines_abort_without_changing_the_source() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("index.html"), "report")?;
        let error = prepare(root.path(), 1, Instant::now())
            .err()
            .context("Expected deadline")?;
        assert!(error.to_string().contains("deadline"));
        assert_eq!(fs::read(root.path().join("index.html"))?, b"report");
        Ok(())
    }
}
