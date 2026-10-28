use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, create_dir_all};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::compare::image_compare::{CompareResult, DiffAlignment, compare_images_with_threshold};
use crate::config::DEFAULT_PIXEL_MATCH_THRESHOLD;
use crate::helpers::{copy_file_atomically, write_file_atomically};
use crate::progress::{self, Event as ProgressEvent};
use crate::test::capture::{
    REPORT_ORDER_FILE, REPORT_ORDER_VERSION, REPORT_PIXEL_THRESHOLDS_FILE, REPORT_THRESHOLDS_FILE,
    REPORT_URLS_FILE, REPORT_URLS_VERSION, ReportOrderManifest, ReportPageUrls, ReportUrlsManifest,
    read_capture_metadata_cached, read_error_marker,
};
use crate::test::image_simplification::{ImageSimplificationWarning, setting_warning};

type ArchiveStates =
    BTreeMap<crate::diagnostics::record::OperationId, crate::diagnostics::journal::ArchiveState>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportConfig {
    test_suite: String,
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default = "default_complete_inventory")]
    capture_inventory_complete: bool,
    tests: Vec<ReportTest>,
}

fn default_complete_inventory() -> bool {
    true
}

pub(crate) const SUITE_METADATA_FILE: &str = "suite.json";
const MAX_SUITE_METADATA_BYTES: usize = 65_536;
const MAX_REPORT_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SuiteMetadata {
    pub display_name: Option<String>,
}

pub(crate) fn read_bounded_regular_file(
    path: &Path,
    max_bytes: usize,
    label: &str,
) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to read {label} '{}'", path.display()));
        }
    };
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "{label} '{}' must be a regular file",
        path.display()
    );
    let mut bytes = Vec::new();
    fs::File::open(path)
        .with_context(|| format!("Failed to read {label} '{}'", path.display()))?
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("Failed to read {label} '{}'", path.display()))?;
    anyhow::ensure!(
        bytes.len() <= max_bytes,
        "{label} '{}' exceeds the {} byte limit",
        path.display(),
        max_bytes
    );
    Ok(Some(bytes))
}

pub(crate) fn read_suite_metadata(suite_dir: &Path) -> Result<Option<SuiteMetadata>> {
    let path = suite_dir.join(SUITE_METADATA_FILE);
    let Some(bytes) = read_bounded_regular_file(&path, MAX_SUITE_METADATA_BYTES, "suite metadata")?
    else {
        return Ok(None);
    };
    Ok(Some(
        serde_json::from_slice(&bytes).context("Invalid suite metadata")?,
    ))
}

pub(crate) fn write_suite_metadata(suite_dir: &Path, display_name: Option<&str>) -> Result<()> {
    let metadata = SuiteMetadata {
        display_name: display_name
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned),
    };
    let bytes = serde_json::to_vec(&metadata)?;
    anyhow::ensure!(
        bytes.len() <= MAX_SUITE_METADATA_BYTES,
        "Suite metadata exceeds the 64 KiB limit"
    );
    create_dir_all(suite_dir)?;
    write_file_atomically(&suite_dir.join(SUITE_METADATA_FILE), &bytes)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReportTest {
    pair: ReportPair,
    status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportPair {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scenario_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scenario_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scenario_display_name: Option<String>,
    reference: String,
    test: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reference_log: Option<crate::test::capture::CaptureLog>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    test_log: Option<crate::test::capture::CaptureLog>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comparison_outcome: Option<crate::diagnostics::record::Outcome>,
    /// Set only for compared pairs whose captures recorded different image
    /// simplification settings. Informational: never changes `status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_simplification_warning: Option<ImageSimplificationWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff_image: Option<String>,
    selector: String,
    file_name: String,
    mis_match_threshold: f64,
    #[serde(default = "legacy_pixel_match_threshold")]
    pixel_match_threshold: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<ReportDiff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_error_msg: Option<String>,
}

fn legacy_pixel_match_threshold() -> f64 {
    // Approval of reports predating this setting must retain their original tolerance.
    10.0
}

struct ComparisonJob {
    pair: ReportPair,
    reference_path: PathBuf,
    test_path: PathBuf,
}

enum ReportWork {
    Immediate(ReportTest),
    Compare(ComparisonJob),
}

enum ComparisonOutcome {
    Compared {
        result: Box<CompareResult>,
        staged_diff_name: Option<String>,
    },
    Failed(anyhow::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportDiff {
    is_same_dimensions: bool,
    dimension_difference: DimensionDiff,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alignment: Option<DiffAlignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mismatch_pixels: Option<u64>,
    raw_mis_match_percentage: f64,
    analysis_time: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DimensionDiff {
    width: i64,
    height: i64,
}

struct ReportOrder {
    captures: Vec<String>,
    is_complete_inventory: bool,
}

/// Read the report ordering manifest. Older runs may not have one, but a
/// present malformed manifest is an artifact error rather than an empty order.
fn read_report_order(tests_dir: &Path) -> Result<Option<ReportOrder>> {
    let path = tests_dir.join(REPORT_ORDER_FILE);
    let Some(bytes) =
        read_bounded_regular_file(&path, MAX_REPORT_MANIFEST_BYTES, "report order manifest")?
    else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("Invalid report order manifest '{}'", path.display()))?;
    let (captures, is_complete_inventory) = if value.is_array() {
        let captures: Vec<String> = serde_json::from_value(value)
            .with_context(|| format!("Invalid report order manifest '{}'", path.display()))?;
        (captures, false)
    } else {
        let manifest: ReportOrderManifest = serde_json::from_value(value)
            .with_context(|| format!("Invalid report order manifest '{}'", path.display()))?;
        if manifest.version != REPORT_ORDER_VERSION {
            anyhow::bail!(
                "Unsupported report order manifest version {} in '{}'",
                manifest.version,
                path.display()
            );
        }
        (manifest.captures, true)
    };
    let mut seen = HashSet::with_capacity(captures.len());
    for key in &captures {
        if key.trim().is_empty() || !seen.insert(key) {
            anyhow::bail!(
                "Invalid report order manifest '{}': duplicate or empty key",
                path.display()
            );
        }
    }
    Ok(Some(ReportOrder {
        captures,
        is_complete_inventory,
    }))
}

fn read_report_thresholds(tests_dir: &Path) -> Result<Option<HashMap<String, f64>>> {
    read_threshold_manifest(tests_dir, REPORT_THRESHOLDS_FILE)
}

fn read_report_pixel_thresholds(tests_dir: &Path) -> Result<Option<HashMap<String, f64>>> {
    read_threshold_manifest(tests_dir, REPORT_PIXEL_THRESHOLDS_FILE)
}

fn read_threshold_manifest(
    tests_dir: &Path,
    file_name: &str,
) -> Result<Option<HashMap<String, f64>>> {
    let path = tests_dir.join(file_name);
    let Some(bytes) = read_bounded_regular_file(
        &path,
        MAX_REPORT_MANIFEST_BYTES,
        "report threshold manifest",
    )?
    else {
        return Ok(None);
    };
    let thresholds: HashMap<String, f64> = serde_json::from_slice(&bytes)
        .with_context(|| format!("Invalid report threshold manifest '{}'", path.display()))?;
    for (key, value) in &thresholds {
        if key.trim().is_empty() || !value.is_finite() || !(0.0..=100.0).contains(value) {
            anyhow::bail!(
                "Invalid report threshold manifest '{}': '{}' must be a finite value between 0 and 100",
                path.display(),
                key
            );
        }
    }
    Ok(Some(thresholds))
}

/// Read page URLs recorded for a capture run. Older runs predate this manifest
/// and remain reportable without page-link metadata.
fn read_report_urls(tests_dir: &Path) -> Result<Option<BTreeMap<String, ReportPageUrls>>> {
    let path = tests_dir.join(REPORT_URLS_FILE);
    let Some(bytes) =
        read_bounded_regular_file(&path, MAX_REPORT_MANIFEST_BYTES, "report URL manifest")?
    else {
        return Ok(None);
    };
    let manifest: ReportUrlsManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("Invalid report URL manifest '{}'", path.display()))?;
    if manifest.version != REPORT_URLS_VERSION {
        anyhow::bail!(
            "Unsupported report URL manifest version {} in '{}'",
            manifest.version,
            path.display()
        );
    }
    for (capture, urls) in &manifest.captures {
        if capture.trim().is_empty()
            || urls.reference_url.trim().is_empty()
            || urls.url.trim().is_empty()
        {
            anyhow::bail!(
                "Invalid report URL manifest '{}': capture keys and page URLs must not be empty",
                path.display()
            );
        }
    }
    Ok(Some(manifest.captures))
}

fn screenshot_prefix(file_name: &str) -> &str {
    let key = screenshot_key(file_name);
    key.rsplit_once("__")
        .map(|(prefix, _)| prefix)
        .unwrap_or(key)
}

fn screenshot_key(file_name: &str) -> &str {
    file_name.strip_suffix(".png").unwrap_or(file_name)
}

fn diff_file_name(file_name: &str) -> String {
    format!("{}_diff.png", screenshot_key(file_name))
}

fn screenshot_label(file_name: &str) -> String {
    screenshot_key(file_name)
        .replace("__", " ")
        .replace('$', "")
}

fn resolve_pair_threshold(thresholds: &HashMap<String, f64>, file_name: &str, default: f64) -> f64 {
    thresholds
        .get(screenshot_key(file_name))
        .or_else(|| thresholds.get(screenshot_prefix(file_name)))
        .copied()
        .unwrap_or(default)
}

fn sort_screenshots_by_manifest(screenshots: &mut [String], order: &[String]) {
    let mut order_index = HashMap::with_capacity(order.len() * 2);
    for (index, key) in order.iter().enumerate() {
        order_index.insert(key.as_str(), index);
        order_index.entry(screenshot_prefix(key)).or_insert(index);
    }
    screenshots.sort_by(|a, b| {
        screenshot_order_key(a, &order_index)
            .cmp(&screenshot_order_key(b, &order_index))
            .then_with(|| a.cmp(b))
    });
}

fn capture_failure_message(ref_err: Option<&str>, test_err: Option<&str>) -> Option<String> {
    match (ref_err, test_err) {
        (Some(r), Some(t)) => Some(format!(
            "Both reference and test scenarios failed to capture (reference: {r}; test: {t})"
        )),
        (Some(r), None) => Some(format!("Reference scenario failed to capture: {r}")),
        (None, Some(t)) => Some(format!("Test scenario failed to capture: {t}")),
        (None, None) => None,
    }
}

/// Position of a screenshot in the ordering manifest, derived from its filename
/// prefix (`<scenario-id>[_<viewport>]`, the part before `__<step|FINAL>`).
/// Screenshots not listed in the manifest sort after all listed ones.
fn screenshot_order_key(file_name: &str, order_index: &HashMap<&str, usize>) -> usize {
    order_index
        .get(screenshot_key(file_name))
        .copied()
        // Older captures stored only the scenario/viewport prefix.
        .or_else(|| order_index.get(screenshot_prefix(file_name)).copied())
        .unwrap_or(usize::MAX)
}

/// Generated diff images live beside captures, but are report artifacts rather
/// than inputs to a later comparison.
fn is_capture_screenshot(file_name: &str) -> bool {
    file_name.ends_with(".png") && !file_name.ends_with("_diff.png")
}

fn collect_capture_screenshots(tests_dir: &Path) -> Result<Vec<String>> {
    let mut screenshots = Vec::new();
    for entry in fs::read_dir(tests_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let file_name = entry.file_name().into_string().map_err(|name| {
            anyhow!(
                "Capture file name in '{}' is not valid UTF-8: {:?}",
                tests_dir.display(),
                name
            )
        })?;
        if is_capture_screenshot(&file_name) {
            screenshots.push(file_name);
        }
    }
    screenshots.sort();
    Ok(screenshots)
}

fn display_capture_keys(captures: &[&str]) -> String {
    const DISPLAY_LIMIT: usize = 5;
    let listed = captures
        .iter()
        .take(DISPLAY_LIMIT)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if captures.len() > DISPLAY_LIMIT {
        format!("{listed}, ...")
    } else {
        listed
    }
}

fn validate_capture_inventory(
    tests_dir: &Path,
    expected_captures: &[String],
    screenshots: &[String],
) -> Result<()> {
    let expected_keys: HashSet<&str> = expected_captures.iter().map(String::as_str).collect();
    let actual_keys: HashSet<&str> = screenshots
        .iter()
        .map(|screenshot| screenshot_key(screenshot))
        .collect();
    let missing: Vec<&str> = expected_captures
        .iter()
        .map(String::as_str)
        .filter(|capture| !actual_keys.contains(capture))
        .collect();
    let unexpected: Vec<&str> = screenshots
        .iter()
        .map(|screenshot| screenshot_key(screenshot))
        .filter(|capture| !expected_keys.contains(capture))
        .collect();

    if missing.is_empty() && unexpected.is_empty() {
        return Ok(());
    }

    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!(
            "missing {} screenshot(s) listed in its manifest: {}",
            missing.len(),
            display_capture_keys(&missing)
        ));
    }
    if !unexpected.is_empty() {
        problems.push(format!(
            "contains {} unexpected screenshot(s) absent from its manifest: {}",
            unexpected.len(),
            display_capture_keys(&unexpected)
        ));
    }
    anyhow::bail!(
        "Capture run '{}' {}",
        tests_dir.display(),
        problems.join("; ")
    );
}

/// Validate the exact screenshot inventory for a modern capture run. Legacy
/// array manifests retain their historical order-only behavior.
pub(crate) fn validate_capture_run_inventory(tests_dir: &Path) -> Result<Vec<String>> {
    let screenshots = collect_capture_screenshots(tests_dir)?;
    if let Some(report_order) = read_report_order(tests_dir)?
        && report_order.is_complete_inventory
    {
        validate_capture_inventory(tests_dir, &report_order.captures, &screenshots)?;
    }
    Ok(screenshots)
}

pub fn select_capture_run(
    tests_dir: &Path,
    requested_run_id: Option<&str>,
) -> Result<Option<String>> {
    if let Some(run_id) = requested_run_id {
        crate::cli::args::run_id_parser(run_id).map_err(anyhow::Error::msg)?;
        let requested_dir = tests_dir.join(run_id);
        let metadata = match fs::symlink_metadata(&requested_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!(
                    "Requested capture run '{}' does not exist: {}",
                    run_id,
                    requested_dir.display()
                );
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Failed to inspect requested capture run '{}': {}",
                        run_id,
                        requested_dir.display()
                    )
                });
            }
        };
        if !metadata.file_type().is_dir() {
            anyhow::bail!(
                "Requested capture run '{}' is not a real directory: {}",
                run_id,
                requested_dir.display()
            );
        }
        return Ok(Some(run_id.to_string()));
    }

    crate::run_history::latest_completed_capture_run(tests_dir)
}

fn report_pair(
    label: String,
    screenshot_name: &str,
    relative_test: String,
    threshold: f64,
    pixel_threshold: f64,
    page_urls: Option<&ReportPageUrls>,
) -> ReportPair {
    ReportPair {
        label,
        scenario_id: page_urls.and_then(|urls| urls.scenario_id.clone()),
        scenario_label: page_urls.and_then(|urls| urls.scenario_label.clone()),
        scenario_display_name: page_urls.and_then(|urls| urls.scenario_display_name.clone()),
        reference: format!("../bitmaps_reference/{}", screenshot_name),
        test: relative_test,
        reference_log: None,
        test_log: None,
        comparison_outcome: None,
        image_simplification_warning: None,
        reference_url: page_urls.map(|urls| urls.reference_url.clone()),
        url: page_urls.map(|urls| urls.url.clone()),
        diff_image: None,
        selector: report_selector(screenshot_name),
        file_name: screenshot_name.to_string(),
        mis_match_threshold: threshold,
        pixel_match_threshold: pixel_threshold,
        diff: None,
        engine_error_msg: None,
    }
}

/// Read both capture links once: their log references go into the pair, and
/// the returned warning (when the recorded image simplification settings
/// differ) is attached by callers only to pairs that are actually compared.
fn attach_capture_metadata(
    pair: &mut ReportPair,
    output_root: &Path,
    reference_path: &Path,
    test_path: &Path,
    archive_states: &mut ArchiveStates,
) -> Option<ImageSimplificationWarning> {
    let reference = read_capture_metadata_cached(output_root, reference_path, archive_states);
    let test = read_capture_metadata_cached(output_root, test_path, archive_states);
    pair.reference_log = reference.as_ref().map(|metadata| metadata.log.clone());
    pair.test_log = test.as_ref().map(|metadata| metadata.log.clone());
    setting_warning(
        reference
            .as_ref()
            .and_then(|metadata| metadata.image_simplification.as_ref()),
        test.as_ref()
            .and_then(|metadata| metadata.image_simplification.as_ref()),
    )
}

fn print_image_simplification_warning(pair: &ReportPair) {
    if let Some(warning) = &pair.image_simplification_warning {
        eprintln!(
            "    {} {} → {}",
            "WARNING:".yellow(),
            pair.file_name,
            warning.message
        );
    }
}

fn report_selector(screenshot_name: &str) -> String {
    let target = screenshot_key(screenshot_name)
        .rsplit_once("__")
        .map(|(_, target)| target)
        .unwrap_or("FINAL");
    match target {
        "FINAL" => "document".to_string(),
        "VIEWPORT" => "viewport".to_string(),
        target if target.starts_with('$') => format!("save:{}", &target[1..]),
        target if target.starts_with("SELECTOR-") => target.to_string(),
        target => target.to_string(),
    }
}

fn stage_diff_image(
    staging_dir: &Path,
    file_name: &str,
    diff_png: Option<Vec<u8>>,
) -> Result<Option<String>> {
    let Some(diff_data) = diff_png else {
        return Ok(None);
    };
    let diff_image_name = diff_file_name(file_name);
    write_file_atomically(&staging_dir.join(&diff_image_name), &diff_data)?;
    Ok(Some(diff_image_name))
}

fn compare_report_work(
    work: &[ReportWork],
    staging_dir: &Path,
    max_workers: usize,
) -> Result<Vec<Option<Result<ComparisonOutcome>>>> {
    let evaluate = |entry: &ReportWork| match entry {
        ReportWork::Immediate(_) => None,
        ReportWork::Compare(job) => Some(
            match compare_images_with_threshold(
                &job.reference_path,
                &job.test_path,
                job.pair.pixel_match_threshold,
            ) {
                Ok((result, diff_png)) => {
                    stage_diff_image(staging_dir, &job.pair.file_name, diff_png).map(
                        |staged_diff_name| ComparisonOutcome::Compared {
                            result: Box::new(result),
                            staged_diff_name,
                        },
                    )
                }
                Err(error) => Ok(ComparisonOutcome::Failed(error)),
            },
        ),
    };
    let compare_jobs = work
        .iter()
        .filter(|entry| matches!(entry, ReportWork::Compare(_)))
        .count();
    if max_workers <= 1 || compare_jobs <= 1 {
        return Ok(work.iter().map(evaluate).collect());
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_workers.min(compare_jobs))
        .build()
        .map_err(|error| anyhow!("Failed to create comparison worker pool: {}", error))?;

    Ok(pool.install(|| work.par_iter().map(evaluate).collect()))
}

/// Generate a comparison report for a test suite.
///
/// Directory structure:
/// ```text
/// .regressify/<suite_id>/
///   bitmaps_reference/   ← reference screenshots
///   bitmaps_test/<date>/            ← test screenshots + diff images + report.json
///   html_report/config.js           ← report config for React UI
///   tests/<date>/                   ← raw captured screenshots (existing)
/// ```
pub fn ensure_capture_report(suite_dir: &Path, suite_id: &str, run_id: &str) -> Result<()> {
    ensure_report_scope(suite_dir, suite_id, run_id, false)
}

pub fn ensure_complete_capture_report(
    suite_dir: &Path,
    suite_id: &str,
    run_id: &str,
) -> Result<()> {
    ensure_report_scope(suite_dir, suite_id, run_id, true)
}

pub(crate) fn report_has_scope(path: &Path, complete: bool) -> Result<bool> {
    let Some(bytes) = read_bounded_regular_file(path, MAX_REPORT_MANIFEST_BYTES, "capture report")?
    else {
        return Ok(false);
    };
    if !complete {
        return Ok(true);
    }
    let report: serde_json::Value =
        serde_json::from_slice(&bytes).context("Invalid capture report")?;
    match report.get("captureInventoryComplete") {
        None => Ok(true),
        Some(serde_json::Value::Bool(complete)) => Ok(*complete),
        Some(_) => anyhow::bail!("Invalid capture report inventory scope"),
    }
}

fn ensure_report_scope(
    suite_dir: &Path,
    suite_id: &str,
    run_id: &str,
    complete: bool,
) -> Result<()> {
    select_capture_run(&suite_dir.join("bitmaps_test"), Some(run_id))?;
    for path in [
        suite_dir.to_path_buf(),
        suite_dir.join("bitmaps_test"),
        suite_dir.join("bitmaps_test").join(run_id),
    ] {
        crate::run_history::require_real_directory(&path)?;
    }
    let report = suite_dir
        .join("bitmaps_test")
        .join(run_id)
        .join("report.json");
    if report_has_scope(&report, complete)? {
        return Ok(());
    }
    let lease = crate::run_history::SuiteLease::acquire(suite_dir)?;
    if !report_has_scope(&report, complete)? {
        for directory in ["html_report", "bitmaps_reference"] {
            let path = suite_dir.join(directory);
            if path.try_exists()? {
                crate::run_history::require_real_directory(&path)?;
            }
        }
        generate_comparison_report(
            suite_dir,
            suite_id,
            run_id,
            0.1,
            crate::config::DEFAULT_ASYNC_COMPARE_LIMIT,
        )?;
        lease.mark_compared(run_id)?;
    }
    Ok(())
}

pub fn generate_comparison_report(
    suite_dir: &Path,
    suite_id: &str,
    test_date: &str,
    mis_match_threshold: f64,
    max_workers: usize,
) -> Result<()> {
    generate_comparison_report_for_scenarios(
        suite_dir,
        suite_id,
        test_date,
        mis_match_threshold,
        max_workers,
        &[],
    )
}

pub(crate) fn generate_comparison_report_for_scenarios(
    suite_dir: &Path,
    suite_id: &str,
    test_date: &str,
    mis_match_threshold: f64,
    max_workers: usize,
    scenario_ids: &[String],
) -> Result<()> {
    generate_selected_report(
        suite_dir,
        suite_id,
        test_date,
        mis_match_threshold,
        max_workers,
        scenario_ids,
        None,
    )
}

fn generate_selected_report(
    suite_dir: &Path,
    suite_id: &str,
    test_date: &str,
    mis_match_threshold: f64,
    max_workers: usize,
    scenario_ids: &[String],
    reviewed_report: Option<&ReportConfig>,
) -> Result<()> {
    let tests_dir = suite_dir.join("bitmaps_test").join(test_date);
    if !tests_dir.exists() {
        anyhow::bail!("Test screenshots directory does not exist: {:?}", tests_dir);
    }
    let display_name = read_suite_metadata(suite_dir)?.and_then(|metadata| metadata.display_name);
    progress::emit(ProgressEvent::Comparing {
        suite_id: suite_id.into(),
    });

    let reference_dir = suite_dir.join("bitmaps_reference");
    let bitmaps_test_dir = suite_dir.join("bitmaps_test").join(test_date);
    let html_report_dir = suite_dir.join("html_report");

    create_dir_all(&reference_dir)?;
    create_dir_all(&bitmaps_test_dir)?;
    create_dir_all(&html_report_dir)?;

    // Collect captured screenshots only. Diff images from an earlier report
    // regeneration share this directory and must not become test inputs.
    let mut test_screenshots = validate_capture_run_inventory(&tests_dir)?;
    // Order screenshots by the capture manifest (scenario order, then viewport
    // order) so the report mirrors the test suite + viewport file definitions
    // rather than alphabetical filename order. Any screenshot not present in the
    // manifest (e.g. stale files or runs predating the manifest) sorts last, then
    // alphabetically among themselves.
    let report_order = match read_report_order(&tests_dir)? {
        Some(order) => order,
        None => {
            if reviewed_report.is_none() {
                eprintln!(
                    "  {} Report order manifest missing; using alphabetical fallback.",
                    "WARNING:".yellow()
                );
            }
            ReportOrder {
                captures: Vec::new(),
                is_complete_inventory: false,
            }
        }
    };
    let order = report_order.captures;
    let thresholds = read_report_thresholds(&tests_dir)?.unwrap_or_default();
    let pixel_thresholds = read_report_pixel_thresholds(&tests_dir)?.unwrap_or_default();
    let report_urls = read_report_urls(&tests_dir)?;
    if let Some(page_urls) = &report_urls {
        let expected_captures: Vec<String> = page_urls.keys().cloned().collect();
        validate_capture_inventory(&tests_dir, &expected_captures, &test_screenshots)?;
    }
    test_screenshots =
        select_scenario_screenshots(test_screenshots, report_urls.as_ref(), scenario_ids)?;
    sort_screenshots_by_manifest(&mut test_screenshots, &order);
    if let Some(report) = reviewed_report {
        // Approval must retain the reviewed scope, including unapproved failures.
        test_screenshots = report
            .tests
            .iter()
            .map(|test| test.pair.file_name.clone())
            .collect();
    }

    let output_root = suite_dir.parent().unwrap_or(suite_dir);
    let mut archive_states = BTreeMap::new();
    let mut report_work = Vec::with_capacity(test_screenshots.len());
    for screenshot_name in test_screenshots {
        let test_path = tests_dir.join(&screenshot_name);
        let threshold = resolve_pair_threshold(&thresholds, &screenshot_name, mis_match_threshold);
        let pixel_threshold = resolve_pair_threshold(
            &pixel_thresholds,
            &screenshot_name,
            DEFAULT_PIXEL_MATCH_THRESHOLD,
        );

        let reference_path = reference_dir.join(&screenshot_name);

        // Relative paths for the report (from html_report/ perspective)
        let relative_test = format!("../bitmaps_test/{}/{}", test_date, screenshot_name);

        // Derive label from filename: "scenario-id__FINAL.png" → "scenario-id FINAL"
        let label = screenshot_label(&screenshot_name);
        let page_urls = report_urls
            .as_ref()
            .and_then(|urls| urls.get(screenshot_key(&screenshot_name)));
        let mut pair = report_pair(
            label,
            &screenshot_name,
            relative_test,
            threshold,
            pixel_threshold,
            page_urls,
        );
        if let Some(previous) = reviewed_report.and_then(|report| {
            report
                .tests
                .iter()
                .find(|test| test.pair.file_name == screenshot_name)
        }) {
            let reference = pair.reference;
            let test = pair.test;
            pair = previous.pair.clone();
            pair.reference = reference;
            pair.test = test;
            pair.diff = None;
            pair.diff_image = None;
            pair.engine_error_msg = None;
            // Approval may have replaced the reference and its recorded settings.
            pair.image_simplification_warning = None;
        }

        let simplification_warning = attach_capture_metadata(
            &mut pair,
            output_root,
            &reference_path,
            &test_path,
            &mut archive_states,
        );

        // If either side is flagged as a capture failure (via its JSON error
        // marker), the corresponding scenario never produced a real screenshot.
        // Skip the visual comparison and report the failure directly — comparing
        // against a placeholder is meaningless and would only produce noise.
        let test_err = read_error_marker(&test_path);
        let ref_err = if reference_path.exists() {
            read_error_marker(&reference_path)
        } else {
            None
        };
        if let Some(msg) = capture_failure_message(ref_err.as_deref(), test_err.as_deref()) {
            eprintln!("    {} {} → {}", "FAIL:".red(), screenshot_name, msg);

            pair.engine_error_msg = Some(msg);
            report_work.push(ReportWork::Immediate(ReportTest {
                pair,
                status: "fail".to_string(),
            }));
            continue;
        }

        if !reference_path.exists() {
            // No reference image — first run or new scenario
            eprintln!(
                "    {} No reference for {}. Marking as new.",
                "NEW:".yellow(),
                screenshot_name
            );

            pair.engine_error_msg = Some("Reference image not found".to_string());
            report_work.push(ReportWork::Immediate(ReportTest {
                pair,
                status: "fail".to_string(),
            }));
            continue;
        }

        pair.image_simplification_warning = simplification_warning;
        report_work.push(ReportWork::Compare(ComparisonJob {
            pair,
            reference_path,
            test_path,
        }));
    }

    let comparison_count = report_work
        .iter()
        .filter(|entry| matches!(entry, ReportWork::Compare(_)))
        .count();
    if comparison_count > 0 {
        eprintln!(
            "  Comparing {} screenshot pair(s) with up to {} worker(s)...",
            comparison_count,
            max_workers.max(1)
        );
    }
    let staging_dir = tempfile::Builder::new()
        .prefix(".compare-staging-")
        .tempdir_in(suite_dir)
        .with_context(|| {
            format!(
                "Failed to create comparison staging directory in '{}'",
                suite_dir.display()
            )
        })?;
    let outcomes = compare_report_work(&report_work, staging_dir.path(), max_workers)?;
    let mut staged_entries = Vec::with_capacity(report_work.len());

    for (entry, outcome) in report_work.into_iter().zip(outcomes) {
        match entry {
            ReportWork::Immediate(test) => staged_entries.push(StagedReportEntry {
                test,
                staged_diff_name: None,
            }),
            ReportWork::Compare(job) => match outcome.ok_or_else(|| {
                anyhow!(
                    "Comparison worker did not return an outcome for '{}'",
                    job.pair.file_name
                )
            })?? {
                ComparisonOutcome::Compared {
                    result,
                    staged_diff_name,
                } => {
                    staged_entries.push(finish_staged_compared_pair(
                        test_date,
                        job.pair,
                        *result,
                        staged_diff_name,
                    ));
                }
                ComparisonOutcome::Failed(error) => {
                    let mut pair = job.pair;
                    eprintln!(
                        "    {} Failed to compare {}: {}",
                        "ERROR:".red(),
                        pair.file_name,
                        error
                    );
                    print_image_simplification_warning(&pair);
                    pair.engine_error_msg = Some(format!("Comparison error: {}", error));
                    staged_entries.push(StagedReportEntry {
                        test: ReportTest {
                            pair,
                            status: "fail".to_string(),
                        },
                        staged_diff_name: None,
                    });
                }
            },
        }
    }

    for entry in &mut staged_entries {
        record_comparison_diagnostic(suite_id, test_date, &mut entry.test);
    }

    if progress::enabled() {
        for entry in &staged_entries {
            emit_compared_progress(suite_id, &entry.test);
        }
    }
    let report = commit_staged_report(
        staging_dir.path(),
        &bitmaps_test_dir,
        &html_report_dir,
        suite_id,
        display_name,
        staged_entries,
        scenario_ids.is_empty()
            && reviewed_report.is_none_or(|report| report.capture_inventory_complete),
    )?;
    progress::emit(ProgressEvent::SuiteFinished {
        id: suite_id.into(),
    });

    print_comparison_summary(&report);

    Ok(())
}

struct StagedReportEntry {
    test: ReportTest,
    staged_diff_name: Option<String>,
}

fn finish_compared_pair(
    staging_dir: &Path,
    test_date: &str,
    pair: ReportPair,
    result: CompareResult,
    diff_png: Option<Vec<u8>>,
) -> Result<StagedReportEntry> {
    let staged_diff_name = stage_diff_image(staging_dir, &pair.file_name, diff_png)?;
    Ok(finish_staged_compared_pair(
        test_date,
        pair,
        result,
        staged_diff_name,
    ))
}

fn finish_staged_compared_pair(
    test_date: &str,
    mut pair: ReportPair,
    result: CompareResult,
    staged_diff_name: Option<String>,
) -> StagedReportEntry {
    if let Some(diff_image_name) = &staged_diff_name {
        pair.diff_image = Some(format!("../bitmaps_test/{}/{}", test_date, diff_image_name));
    } else {
        pair.diff_image = None;
    }
    let passed =
        result.raw_mis_match_percentage <= pair.mis_match_threshold && result.is_same_dimensions;
    let status = if passed { "pass" } else { "fail" };
    let percentage = format!(
        "{}%",
        crate::compare::image_compare::format_mismatch_percentage(result.raw_mis_match_percentage)
    );
    let status_text = if passed {
        percentage.green()
    } else {
        percentage.red()
    };

    eprintln!(
        "    {} {} → mismatch: {}",
        if passed {
            "PASS:".green()
        } else {
            "FAIL:".red()
        },
        pair.file_name,
        status_text,
    );
    print_image_simplification_warning(&pair);
    pair.diff = Some(build_diff(&result));
    if !result.is_same_dimensions {
        pair.engine_error_msg = Some(format!(
            "Dimensions differ: reference {}x{} vs test {}x{}",
            result.ref_width, result.ref_height, result.test_width, result.test_height
        ));
    }
    StagedReportEntry {
        test: ReportTest {
            pair,
            status: status.to_string(),
        },
        staged_diff_name,
    }
}

fn record_comparison_diagnostic(suite_id: &str, test_date: &str, test: &mut ReportTest) {
    use crate::diagnostics::{record::*, runtime};
    let outcome = if test.status == "pass" {
        Outcome::Success
    } else if test.pair.engine_error_msg.is_some() {
        Outcome::Failure
    } else {
        Outcome::Mismatch
    };
    test.pair.comparison_outcome = Some(outcome);
    if let Some(context) = runtime::current() {
        context.record(
            Stage::CaptureArtifacts,
            DiagnosticEvent::Compared,
            outcome.severity(),
            [
                (Field::Command, FieldValue::Kind(SafeValue::Compare)),
                (
                    Field::Target,
                    FieldValue::Identity(SafeIdentity::fingerprint(suite_id)),
                ),
                (
                    Field::Destination,
                    FieldValue::Identity(SafeIdentity::fingerprint(test_date)),
                ),
                (
                    Field::Artifact,
                    FieldValue::Identity(SafeIdentity::fingerprint(&test.pair.file_name)),
                ),
            ],
            runtime::Completion {
                outcome: Some(outcome),
                error: test
                    .pair
                    .engine_error_msg
                    .as_ref()
                    .map(|message| SanitizedError::capture(&anyhow!("{message}"))),
                duration_ms: None,
            },
        );
        if test.pair.image_simplification_warning.is_some() {
            // Fingerprints and kinds only: settings, selectors, and paths stay in
            // report.json and the console.
            context.record(
                Stage::CaptureArtifacts,
                DiagnosticEvent::Decision,
                Severity::Warn,
                [
                    (Field::Command, FieldValue::Kind(SafeValue::Compare)),
                    (Field::Condition, FieldValue::Kind(SafeValue::Images)),
                    (
                        Field::Target,
                        FieldValue::Identity(SafeIdentity::fingerprint(suite_id)),
                    ),
                    (
                        Field::Destination,
                        FieldValue::Identity(SafeIdentity::fingerprint(test_date)),
                    ),
                    (
                        Field::Artifact,
                        FieldValue::Identity(SafeIdentity::fingerprint(&test.pair.file_name)),
                    ),
                ],
                runtime::Completion::default(),
            );
        }
    }
}

fn emit_compared_progress(suite_id: &str, test: &ReportTest) {
    progress::emit(ProgressEvent::Compared {
        suite_id: suite_id.into(),
        capture: screenshot_key(&test.pair.file_name).into(),
        label: test
            .pair
            .scenario_label
            .clone()
            .unwrap_or_else(|| test.pair.label.clone()),
        scenario_id: test.pair.scenario_id.clone(),
        passed: test.status == "pass",
        error: test
            .pair
            .engine_error_msg
            .clone()
            .or_else(|| (test.status != "pass").then(|| "Visual mismatch".into())),
    });
}

fn commit_staged_report(
    staging_dir: &Path,
    bitmaps_test_dir: &Path,
    html_report_dir: &Path,
    suite_id: &str,
    display_name: Option<String>,
    entries: Vec<StagedReportEntry>,
    capture_inventory_complete: bool,
) -> Result<ReportConfig> {
    let report = ReportConfig {
        test_suite: suite_id.to_string(),
        id: suite_id.to_string(),
        display_name,
        capture_inventory_complete,
        tests: entries.iter().map(|entry| entry.test.clone()).collect(),
    };

    let report_json = serde_json::to_string_pretty(&report)?;
    let config_js = format!("report({});", report_json);

    for entry in &entries {
        let diff_image_name = diff_file_name(&entry.test.pair.file_name);
        let diff_image_path = bitmaps_test_dir.join(&diff_image_name);
        if let Some(staged_diff_name) = &entry.staged_diff_name {
            let staged_path = staging_dir.join(staged_diff_name);
            if fs::rename(&staged_path, &diff_image_path).is_err() {
                copy_file_atomically(&staged_path, &diff_image_path)?;
            }
        } else if diff_image_path.exists() {
            // A previous comparison in this run may have produced a diff. Do
            // not show that stale artifact for a current pass.
            fs::remove_file(&diff_image_path)?;
        }
    }

    write_file_atomically(
        &bitmaps_test_dir.join("report.json"),
        report_json.as_bytes(),
    )?;
    write_file_atomically(&html_report_dir.join("config.js"), config_js.as_bytes())?;
    Ok(report)
}

fn print_comparison_summary(report: &ReportConfig) {
    let pass_count = report.tests.iter().filter(|t| t.status == "pass").count();
    let fail_count = report.tests.iter().filter(|t| t.status == "fail").count();
    eprintln!(
        "  Comparison complete: {} passed, {} failed out of {} tests",
        pass_count.to_string().green(),
        if fail_count > 0 {
            fail_count.to_string().red()
        } else {
            fail_count.to_string().normal()
        },
        report.tests.len()
    );
    let warnings = report
        .tests
        .iter()
        .filter(|test| test.pair.image_simplification_warning.is_some())
        .count();
    if warnings > 0 {
        eprintln!(
            "  {} {} comparison(s) used different image simplification settings for reference and test. Results are unchanged; run ref again or approve these captures so both sides match.",
            "WARNING:".yellow(),
            warnings
        );
    }
}

/// Generate a self-approved baseline report for `regressify ref` without
/// running pixel comparison against existing images. Successful captures were
/// just promoted as the baseline and are recorded as passed with zero diff;
/// failed captures retain their capture error markers.
pub(crate) fn generate_reference_report(
    suite_dir: &Path,
    suite_id: &str,
    test_date: &str,
) -> Result<()> {
    let tests_dir = suite_dir.join("bitmaps_test").join(test_date);
    if !tests_dir.exists() {
        anyhow::bail!("Test screenshots directory does not exist: {:?}", tests_dir);
    }
    let display_name = read_suite_metadata(suite_dir)?.and_then(|metadata| metadata.display_name);
    let reference_dir = suite_dir.join("bitmaps_reference");
    let html_report_dir = suite_dir.join("html_report");
    create_dir_all(&reference_dir)?;
    create_dir_all(&tests_dir)?;
    create_dir_all(&html_report_dir)?;

    let mut test_screenshots = validate_capture_run_inventory(&tests_dir)?;
    let order = read_report_order(&tests_dir)?
        .map(|order| order.captures)
        .unwrap_or_default();
    let thresholds = read_report_thresholds(&tests_dir)?.unwrap_or_default();
    let pixel_thresholds = read_report_pixel_thresholds(&tests_dir)?.unwrap_or_default();
    let report_urls = read_report_urls(&tests_dir)?;
    if let Some(page_urls) = &report_urls {
        let expected_captures: Vec<String> = page_urls.keys().cloned().collect();
        validate_capture_inventory(&tests_dir, &expected_captures, &test_screenshots)?;
    }
    sort_screenshots_by_manifest(&mut test_screenshots, &order);

    let staging_dir = tempfile::Builder::new()
        .prefix(".compare-staging-")
        .tempdir_in(suite_dir)
        .with_context(|| {
            format!(
                "Failed to create reference report staging directory in '{}'",
                suite_dir.display()
            )
        })?;
    let output_root = suite_dir.parent().unwrap_or(suite_dir);
    let mut archive_states = BTreeMap::new();
    let mut staged_entries = Vec::with_capacity(test_screenshots.len());

    for screenshot_name in test_screenshots {
        let test_path = tests_dir.join(&screenshot_name);
        let reference_path = reference_dir.join(&screenshot_name);
        let threshold = resolve_pair_threshold(&thresholds, &screenshot_name, 0.0);
        let pixel_threshold = resolve_pair_threshold(
            &pixel_thresholds,
            &screenshot_name,
            DEFAULT_PIXEL_MATCH_THRESHOLD,
        );
        let relative_test = format!("../bitmaps_test/{}/{}", test_date, screenshot_name);
        let label = screenshot_label(&screenshot_name);
        let page_urls = report_urls
            .as_ref()
            .and_then(|urls| urls.get(screenshot_key(&screenshot_name)));
        let mut pair = report_pair(
            label,
            &screenshot_name,
            relative_test,
            threshold,
            pixel_threshold,
            page_urls,
        );
        pair.reference_log = crate::test::capture::read_capture_log_cached(
            output_root,
            &reference_path,
            &mut archive_states,
        );
        pair.test_log = crate::test::capture::read_capture_log_cached(
            output_root,
            &test_path,
            &mut archive_states,
        );

        let status = if let Some(test_err) = read_error_marker(&test_path) {
            pair.engine_error_msg = Some(format!("Test scenario failed to capture: {test_err}"));
            pair.comparison_outcome = Some(crate::diagnostics::record::Outcome::Failure);
            "fail"
        } else {
            pair.diff = Some(ReportDiff {
                is_same_dimensions: true,
                dimension_difference: DimensionDiff {
                    width: 0,
                    height: 0,
                },
                alignment: Some(DiffAlignment::default()),
                mismatch_pixels: Some(0),
                raw_mis_match_percentage: 0.0,
                analysis_time: 0,
            });
            pair.comparison_outcome = Some(crate::diagnostics::record::Outcome::Success);
            "pass"
        };

        staged_entries.push(StagedReportEntry {
            test: ReportTest {
                pair,
                status: status.to_string(),
            },
            staged_diff_name: None,
        });
    }

    commit_staged_report(
        staging_dir.path(),
        &tests_dir,
        &html_report_dir,
        suite_id,
        display_name,
        staged_entries,
        true,
    )?;
    progress::emit(ProgressEvent::SuiteFinished {
        id: suite_id.into(),
    });
    Ok(())
}

/// Stages per-job visual comparison outputs in an intermediate temporary
/// directory while scenario captures are still running. Diff images and report
/// JSON/JS are only moved to `bitmaps_test/<run_id>` and `html_report` when all
/// capture and comparison jobs complete.
pub(crate) struct StagedSuiteComparison {
    suite_dir: PathBuf,
    suite_id: String,
    test_date: String,
    display_name: Option<String>,
    tests_dir: PathBuf,
    reference_dir: PathBuf,
    html_report_dir: PathBuf,
    staging_dir: tempfile::TempDir,
    order: Vec<String>,
    thresholds: HashMap<String, f64>,
    pixel_thresholds: HashMap<String, f64>,
    report_urls: Option<BTreeMap<String, ReportPageUrls>>,
    default_mis_match_threshold: f64,
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
    entries: std::sync::Mutex<HashMap<String, StagedReportEntry>>,
    archive_states: std::sync::Mutex<ArchiveStates>,
}

impl StagedSuiteComparison {
    pub(crate) fn new(
        suite_dir: &Path,
        suite_id: &str,
        test_date: &str,
        mis_match_threshold: f64,
        max_workers: usize,
    ) -> Result<Self> {
        let tests_dir = suite_dir.join("bitmaps_test").join(test_date);
        let reference_dir = suite_dir.join("bitmaps_reference");
        let html_report_dir = suite_dir.join("html_report");
        create_dir_all(&reference_dir)?;
        create_dir_all(&tests_dir)?;
        create_dir_all(&html_report_dir)?;
        let display_name =
            read_suite_metadata(suite_dir)?.and_then(|metadata| metadata.display_name);
        let order = read_report_order(&tests_dir)?
            .map(|order| order.captures)
            .unwrap_or_default();
        let thresholds = read_report_thresholds(&tests_dir)?.unwrap_or_default();
        let pixel_thresholds = read_report_pixel_thresholds(&tests_dir)?.unwrap_or_default();
        let report_urls = read_report_urls(&tests_dir)?;
        let staging_dir = tempfile::Builder::new()
            .prefix(".compare-staging-")
            .tempdir_in(suite_dir)
            .with_context(|| {
                format!(
                    "Failed to create comparison staging directory in '{}'",
                    suite_dir.display()
                )
            })?;
        Ok(Self {
            suite_dir: suite_dir.to_path_buf(),
            suite_id: suite_id.to_string(),
            test_date: test_date.to_string(),
            display_name,
            tests_dir,
            reference_dir,
            html_report_dir,
            staging_dir,
            order,
            thresholds,
            pixel_thresholds,
            report_urls,
            default_mis_match_threshold: mis_match_threshold,
            semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(max_workers.max(1))),
            entries: std::sync::Mutex::new(HashMap::new()),
            archive_states: std::sync::Mutex::new(BTreeMap::new()),
        })
    }

    pub(crate) async fn acquire_permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("Suite comparison semaphore closed"))
    }

    pub(crate) fn compare_job_captures(&self, capture_keys: &[String]) -> Result<()> {
        let output_root = self.suite_dir.parent().unwrap_or(&self.suite_dir);
        for key in capture_keys {
            let screenshot_name = format!("{key}.png");
            let entry = self.compare_single_screenshot(output_root, &screenshot_name)?;
            if progress::enabled() {
                emit_compared_progress(&self.suite_id, &entry.test);
            }
            self.entries
                .lock()
                .map_err(|_| anyhow!("Staged comparison lock poisoned"))?
                .insert(screenshot_name, entry);
        }
        Ok(())
    }

    fn compare_single_screenshot(
        &self,
        output_root: &Path,
        screenshot_name: &str,
    ) -> Result<StagedReportEntry> {
        let test_path = self.tests_dir.join(screenshot_name);
        let reference_path = self.reference_dir.join(screenshot_name);
        let threshold = resolve_pair_threshold(
            &self.thresholds,
            screenshot_name,
            self.default_mis_match_threshold,
        );
        let pixel_threshold = resolve_pair_threshold(
            &self.pixel_thresholds,
            screenshot_name,
            DEFAULT_PIXEL_MATCH_THRESHOLD,
        );
        let relative_test = format!("../bitmaps_test/{}/{}", self.test_date, screenshot_name);
        let label = screenshot_label(screenshot_name);
        let page_urls = self
            .report_urls
            .as_ref()
            .and_then(|urls| urls.get(screenshot_key(screenshot_name)));
        let mut pair = report_pair(
            label,
            screenshot_name,
            relative_test,
            threshold,
            pixel_threshold,
            page_urls,
        );
        let simplification_warning = {
            let mut archive_states = self
                .archive_states
                .lock()
                .map_err(|_| anyhow!("Staged comparison archive cache lock poisoned"))?;
            attach_capture_metadata(
                &mut pair,
                output_root,
                &reference_path,
                &test_path,
                &mut archive_states,
            )
        };

        let test_err = read_error_marker(&test_path);
        let ref_err = if reference_path.exists() {
            read_error_marker(&reference_path)
        } else {
            None
        };
        if let Some(msg) = capture_failure_message(ref_err.as_deref(), test_err.as_deref()) {
            eprintln!("    {} {} → {}", "FAIL:".red(), screenshot_name, msg);
            pair.engine_error_msg = Some(msg);
            return Ok(StagedReportEntry {
                test: ReportTest {
                    pair,
                    status: "fail".to_string(),
                },
                staged_diff_name: None,
            });
        }

        if !reference_path.exists() {
            eprintln!(
                "    {} No reference for {}. Marking as new.",
                "NEW:".yellow(),
                screenshot_name
            );
            pair.engine_error_msg = Some("Reference image not found".to_string());
            return Ok(StagedReportEntry {
                test: ReportTest {
                    pair,
                    status: "fail".to_string(),
                },
                staged_diff_name: None,
            });
        }

        pair.image_simplification_warning = simplification_warning;
        match compare_images_with_threshold(&reference_path, &test_path, pair.pixel_match_threshold)
        {
            Ok((result, diff_png)) => finish_compared_pair(
                self.staging_dir.path(),
                &self.test_date,
                pair,
                result,
                diff_png,
            ),
            Err(error) => {
                eprintln!(
                    "    {} Failed to compare {}: {}",
                    "ERROR:".red(),
                    pair.file_name,
                    error
                );
                print_image_simplification_warning(&pair);
                pair.engine_error_msg = Some(format!("Comparison error: {}", error));
                Ok(StagedReportEntry {
                    test: ReportTest {
                        pair,
                        status: "fail".to_string(),
                    },
                    staged_diff_name: None,
                })
            }
        }
    }

    pub(crate) fn finalize(&self) -> Result<()> {
        let mut test_screenshots = validate_capture_run_inventory(&self.tests_dir)?;
        if let Some(page_urls) = &self.report_urls {
            let expected_captures: Vec<String> = page_urls.keys().cloned().collect();
            validate_capture_inventory(&self.tests_dir, &expected_captures, &test_screenshots)?;
        }
        sort_screenshots_by_manifest(&mut test_screenshots, &self.order);

        let mut staged_map = self
            .entries
            .lock()
            .map_err(|_| anyhow!("Staged comparison lock poisoned"))?;
        let mut ordered_entries = Vec::with_capacity(test_screenshots.len());
        let output_root = self.suite_dir.parent().unwrap_or(&self.suite_dir);
        for screenshot_name in test_screenshots {
            let mut entry = match staged_map.remove(&screenshot_name) {
                Some(entry) => entry,
                None => {
                    let entry = self.compare_single_screenshot(output_root, &screenshot_name)?;
                    if progress::enabled() {
                        emit_compared_progress(&self.suite_id, &entry.test);
                    }
                    entry
                }
            };
            record_comparison_diagnostic(&self.suite_id, &self.test_date, &mut entry.test);
            ordered_entries.push(entry);
        }
        drop(staged_map);

        let report = commit_staged_report(
            self.staging_dir.path(),
            &self.tests_dir,
            &self.html_report_dir,
            &self.suite_id,
            self.display_name.clone(),
            ordered_entries,
            true,
        )?;
        progress::emit(ProgressEvent::SuiteFinished {
            id: self.suite_id.clone(),
        });
        print_comparison_summary(&report);
        Ok(())
    }
}

/// Promote test screenshots to reference (approve).
pub fn approve_screenshots(suite_dir: &Path, requested_run_id: Option<&str>) -> Result<()> {
    approve_screenshots_for_scenarios(suite_dir, requested_run_id, &[])
}

pub(crate) fn validate_scenario_captures(tests_dir: &Path, scenario_ids: &[String]) -> Result<()> {
    let screenshots = validate_capture_run_inventory(tests_dir)?;
    let urls = read_report_urls(tests_dir)?;
    select_scenario_screenshots(screenshots, urls.as_ref(), scenario_ids)?;
    Ok(())
}

fn select_scenario_screenshots(
    screenshots: Vec<String>,
    urls: Option<&BTreeMap<String, ReportPageUrls>>,
    scenario_ids: &[String],
) -> Result<Vec<String>> {
    if scenario_ids.is_empty() {
        return Ok(screenshots);
    }
    let urls = urls.ok_or_else(|| anyhow!(
        "This run has no scenario metadata; capture the selected scenarios again or select the whole suite"
    ))?;
    for id in scenario_ids {
        anyhow::ensure!(
            screenshots.iter().any(|name| urls
                .get(screenshot_key(name))
                .is_some_and(|urls| urls.scenario_id.as_deref() == Some(id.as_str()))),
            "Selected scenario '{id}' has no captures in this run; capture it before comparing or approving"
        );
    }
    Ok(screenshots
        .into_iter()
        .filter(|name| {
            urls.get(screenshot_key(name))
                .and_then(|urls| urls.scenario_id.as_ref())
                .is_some_and(|id| scenario_ids.contains(id))
        })
        .collect())
}

fn select_scenario_viewport_screenshots(
    screenshots: Vec<String>,
    urls: Option<&BTreeMap<String, ReportPageUrls>>,
    scenario_id: &str,
    capture_key: &str,
) -> Result<Vec<String>> {
    anyhow::ensure!(
        !scenario_id.is_empty(),
        "Select a recorded scenario to approve"
    );
    let screenshots = select_scenario_screenshots(screenshots, urls, &[scenario_id.to_string()])?;
    anyhow::ensure!(
        screenshots
            .iter()
            .any(|name| screenshot_key(name) == capture_key),
        "The selected capture is not part of this scenario's run"
    );
    let viewport = |key: &str| {
        // Scenario IDs may contain the same delimiter as capture targets.
        key.strip_prefix(scenario_id)
            .and_then(|suffix| suffix.split_once("__"))
            .filter(|(viewport, target)| {
                (viewport.is_empty() || viewport.starts_with('_')) && !target.is_empty()
            })
            .map(|(viewport, _)| viewport.to_string())
    };
    let selected = viewport(capture_key)
        .ok_or_else(|| anyhow!("The selected capture has no recognizable viewport metadata"))?;
    Ok(screenshots
        .into_iter()
        .filter(|name| viewport(screenshot_key(name)).as_ref() == Some(&selected))
        .collect())
}

fn preflight_approved_screenshots(tests_dir: &Path, screenshots: &[String]) -> Result<()> {
    for filename in screenshots {
        let path = tests_dir.join(filename);
        if read_error_marker(&path).is_none() {
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("Cannot inspect capture '{}'", path.display()))?;
            anyhow::ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Cannot approve non-regular capture '{}'",
                path.display()
            );
            super::image_compare::load_png(&path)
                .with_context(|| format!("Cannot approve invalid capture '{}'", path.display()))?;
        }
    }
    Ok(())
}

pub(crate) fn approve_screenshots_for_scenarios(
    suite_dir: &Path,
    requested_run_id: Option<&str>,
    scenario_ids: &[String],
) -> Result<()> {
    let bitmaps_test_dir = suite_dir.join("bitmaps_test");
    let reference_dir = suite_dir.join("bitmaps_reference");

    if !bitmaps_test_dir.exists() {
        eprintln!("  {}", "No test screenshots found, skipping.".yellow());
        return Ok(());
    }

    let selected_run = match select_capture_run(&bitmaps_test_dir, requested_run_id)? {
        Some(run) => run,
        None => {
            eprintln!("  {}", "No test run directories found, skipping.".yellow());
            return Ok(());
        }
    };

    let latest_test_dir = bitmaps_test_dir.join(&selected_run);

    eprintln!("  Approving screenshots from run: {}", selected_run.cyan());

    let screenshots = validate_capture_run_inventory(&latest_test_dir)?;
    let urls = read_report_urls(&latest_test_dir)?;
    if let Some(page_urls) = &urls {
        let expected_captures: Vec<String> = page_urls.keys().cloned().collect();
        validate_capture_inventory(&latest_test_dir, &expected_captures, &screenshots)?;
    }
    let screenshots = select_scenario_screenshots(screenshots, urls.as_ref(), scenario_ids)?;
    preflight_approved_screenshots(&latest_test_dir, &screenshots)?;

    promote_screenshots(&latest_test_dir, &reference_dir, &screenshots)?;
    Ok(())
}

fn promote_screenshots(
    tests_dir: &Path,
    reference_dir: &Path,
    screenshots: &[String],
) -> Result<usize> {
    use crate::diagnostics::{record::*, runtime};
    create_dir_all(reference_dir)?;

    let mut approved_count = 0;
    for file_name in screenshots {
        let src = tests_dir.join(file_name);
        if read_error_marker(&src).is_some() {
            runtime::event(
                Stage::CaptureArtifacts,
                DiagnosticEvent::Decision,
                Severity::Warn,
                [
                    (Field::Command, FieldValue::Kind(SafeValue::Approve)),
                    (Field::State, FieldValue::Kind(SafeValue::Failed)),
                    (
                        Field::Artifact,
                        FieldValue::Identity(SafeIdentity::fingerprint(
                            src.as_os_str().as_encoded_bytes(),
                        )),
                    ),
                ],
            );
            continue;
        }
        let dst = reference_dir.join(file_name);
        copy_file_atomically(&src, &dst)?;
        if let Some(output) = reference_dir.parent().and_then(Path::parent) {
            crate::test::capture::promote_capture_log(output, &src, &dst);
        }

        // Clear any stale error-marker sidecar when the newly promoted capture is good.
        let marker_dst = crate::test::capture::error_marker_path(&dst);
        if marker_dst.exists() {
            fs::remove_file(&marker_dst)?;
        }

        approved_count += 1;
        runtime::event(
            Stage::CaptureArtifacts,
            DiagnosticEvent::Decision,
            Severity::Debug,
            [
                (Field::Command, FieldValue::Kind(SafeValue::Approve)),
                (Field::State, FieldValue::Kind(SafeValue::Complete)),
                (
                    Field::Artifact,
                    FieldValue::Identity(SafeIdentity::fingerprint(
                        src.as_os_str().as_encoded_bytes(),
                    )),
                ),
            ],
        );
    }

    runtime::event(
        Stage::CaptureArtifacts,
        DiagnosticEvent::StageFinished,
        Severity::Info,
        [
            (Field::Command, FieldValue::Kind(SafeValue::Approve)),
            (Field::FileCount, FieldValue::Number(approved_count as u64)),
            (
                Field::SkippedFiles,
                FieldValue::Number((screenshots.len() - approved_count) as u64),
            ),
        ],
    );
    eprintln!(
        "  {} screenshot(s) approved as new reference.",
        approved_count.to_string().green()
    );

    Ok(approved_count)
}

pub(crate) fn approve_scenario_viewport(
    suite_dir: &Path,
    run_id: &str,
    scenario_id: &str,
    capture_key: &str,
) -> Result<()> {
    crate::cli::args::run_id_parser(run_id).map_err(anyhow::Error::msg)?;
    let suite_id = suite_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("The report suite has no valid output ID"))?;
    let tests_dir = suite_dir.join("bitmaps_test");
    select_capture_run(&tests_dir, Some(run_id))?;
    let run = tests_dir.join(run_id);
    let reference = suite_dir.join("bitmaps_reference");
    let html = suite_dir.join("html_report");
    for directory in [suite_dir, &tests_dir, &run, &reference, &html] {
        match fs::symlink_metadata(directory) {
            Ok(metadata) => anyhow::ensure!(
                metadata.file_type().is_dir(),
                "Report approval requires a real directory: {}",
                directory.display()
            ),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && (directory == reference || directory == html) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let report_path = run.join("report.json");
    for path in [
        run.join(REPORT_URLS_FILE),
        run.join(REPORT_ORDER_FILE),
        run.join(REPORT_THRESHOLDS_FILE),
        run.join(REPORT_PIXEL_THRESHOLDS_FILE),
        html.join("config.js"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) => anyhow::ensure!(
                metadata.file_type().is_file(),
                "Report approval requires regular metadata files: {}",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let bytes =
        read_bounded_regular_file(&report_path, MAX_REPORT_MANIFEST_BYTES, "reviewed report")?
            .ok_or_else(|| {
                anyhow!(
                    "The reviewed report does not exist: {}",
                    report_path.display()
                )
            })?;
    let report: ReportConfig = serde_json::from_slice(&bytes)
        .context("Cannot approve this report; regenerate its comparison first")?;
    anyhow::ensure!(
        report.id == suite_id,
        "The reviewed report's suite identity changed"
    );
    let screenshots = validate_capture_run_inventory(&run)?;
    let urls = read_report_urls(&run)?.ok_or_else(|| {
        anyhow!("This run has no scenario metadata; capture it again before approving a viewport")
    })?;
    validate_capture_inventory(
        &run,
        &urls.keys().cloned().collect::<Vec<_>>(),
        &screenshots,
    )?;
    read_report_thresholds(&run)?;
    read_report_pixel_thresholds(&run)?;
    let mut seen = HashSet::new();
    for test in &report.tests {
        let pair = &test.pair;
        anyhow::ensure!(
            screenshots.contains(&pair.file_name) && seen.insert(&pair.file_name),
            "The reviewed report contains a missing or duplicate capture"
        );
        anyhow::ensure!(
            pair.test == format!("../bitmaps_test/{run_id}/{}", pair.file_name)
                && pair.mis_match_threshold.is_finite()
                && (0.0..=100.0).contains(&pair.mis_match_threshold)
                && pair.pixel_match_threshold.is_finite()
                && (0.0..=100.0).contains(&pair.pixel_match_threshold)
                && urls
                    .get(screenshot_key(&pair.file_name))
                    .is_some_and(|metadata| metadata.scenario_id == pair.scenario_id),
            "The reviewed report's capture metadata changed; refresh it before approving"
        );
        for path in [
            run.join(&pair.file_name),
            reference.join(&pair.file_name),
            crate::test::capture::error_marker_path(&run.join(&pair.file_name)),
            crate::test::capture::error_marker_path(&reference.join(&pair.file_name)),
        ] {
            match fs::symlink_metadata(&path) {
                Ok(metadata) => anyhow::ensure!(
                    metadata.file_type().is_file(),
                    "Report approval requires regular artifact files: {}",
                    path.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    let selected = select_scenario_viewport_screenshots(
        report
            .tests
            .iter()
            .map(|test| test.pair.file_name.clone())
            .collect(),
        Some(&urls),
        scenario_id,
        capture_key,
    )?;
    anyhow::ensure!(
        read_error_marker(&run.join(format!("{capture_key}.png"))).is_none(),
        "The selected capture failed and cannot be approved"
    );
    preflight_approved_screenshots(&run, &selected)?;
    promote_screenshots(&run, &reference, &selected)?;
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2);
    generate_selected_report(
        suite_dir,
        suite_id,
        run_id,
        0.0,
        workers,
        &[],
        Some(&report),
    )
    .context("Selected baselines were promoted, but report recalculation failed")
}

fn build_diff(result: &CompareResult) -> ReportDiff {
    ReportDiff {
        is_same_dimensions: result.is_same_dimensions,
        dimension_difference: DimensionDiff {
            width: result.dimension_difference.0,
            height: result.dimension_difference.1,
        },
        alignment: Some(result.alignment.clone()),
        mismatch_pixels: Some(result.mismatch_pixels),
        raw_mis_match_percentage: result.raw_mis_match_percentage,
        analysis_time: result.analysis_time_ms,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use super::*;

    fn png_bytes(red: u8) -> Vec<u8> {
        lodepng::encode32(&[red, 20, 30, 255], 1, 1).unwrap()
    }

    #[test]
    fn mismatch_one_pixel_report_retains_raw_audit_value_and_fails_zero_threshold() {
        let directory = tempfile::tempdir().unwrap();
        let suite = directory.path().join("suite");
        let reference = suite.join("bitmaps_reference");
        let run = suite.join("bitmaps_test/current");
        fs::create_dir_all(&reference).unwrap();
        fs::create_dir_all(&run).unwrap();
        let mut pixels = vec![rgb::RGBA8::new(255, 255, 255, 255); 256 * 256];
        fs::write(
            reference.join("home.png"),
            lodepng::encode32(&pixels, 256, 256).unwrap(),
        )
        .unwrap();
        pixels[128 * 256 + 128] = rgb::RGBA8::new(0, 0, 0, 255);
        fs::write(
            run.join("home.png"),
            lodepng::encode32(&pixels, 256, 256).unwrap(),
        )
        .unwrap();

        generate_comparison_report(&suite, "suite", "current", 0.0, 1).unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
        let test = &report["tests"][0];
        let diff = &test["pair"]["diff"];
        assert_eq!(test["status"], "fail");
        assert_eq!(diff["mismatchPixels"], 1);
        assert_eq!(
            diff["rawMisMatchPercentage"].as_f64(),
            Some(100.0 / 65536.0)
        );
        assert!(diff.get("misMatchPercentage").is_none());
        #[cfg(feature = "desktop")]
        {
            let desktop: crate::desktop::results::TestResult =
                serde_json::from_value(test.clone()).unwrap();
            assert_eq!(desktop.pair.diff.as_ref().unwrap().mismatch_pixels, Some(1));
            assert!(desktop.copy_summary().contains("Mismatch: 0.002%"));
            assert!(desktop.copy_summary().contains("Mismatch pixels: 1"));
        }
    }

    #[test]
    fn mismatch_audit_metadata_preserves_raw_float_without_display_formatting() {
        for percentage in [
            0.0,
            0.000017922688690066528,
            1.2345678901234567,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            100.0,
        ] {
            let result = CompareResult {
                is_same_dimensions: true,
                dimension_difference: (0, 0),
                ref_width: 1920,
                ref_height: 2906,
                test_width: 1920,
                test_height: 2906,
                alignment: DiffAlignment::default(),
                mismatch_pixels: 1,
                raw_mis_match_percentage: percentage,
                analysis_time_ms: 0,
            };
            let encoded = serde_json::to_string(&build_diff(&result)).unwrap();
            let decoded: ReportDiff = serde_json::from_str(&encoded).unwrap();
            assert_eq!(
                decoded.raw_mis_match_percentage.to_bits(),
                percentage.to_bits()
            );
            assert_eq!(decoded.mismatch_pixels, Some(1));
            assert!(!encoded.contains("\"misMatchPercentage\""));
            let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
            assert!(value["rawMisMatchPercentage"].is_number());
            assert_eq!(value["mismatchPixels"], 1);
        }
    }

    #[test]
    fn diff_alignment_metadata_round_trips_without_changing_legacy_reports() {
        use crate::compare::image_compare::VerticalGap;

        for alignment in [
            DiffAlignment::default(),
            DiffAlignment::from_gaps(
                vec![VerticalGap {
                    start: 24,
                    height: 5,
                }],
                Vec::new(),
            ),
            DiffAlignment::from_gaps(
                Vec::new(),
                vec![VerticalGap {
                    start: 24,
                    height: 5,
                }],
            ),
            DiffAlignment::from_gaps(
                vec![
                    VerticalGap {
                        start: 24,
                        height: 5,
                    },
                    VerticalGap {
                        start: 80,
                        height: 10,
                    },
                ],
                Vec::new(),
            ),
            DiffAlignment::from_gaps(
                vec![VerticalGap {
                    start: 24,
                    height: 5,
                }],
                vec![VerticalGap {
                    start: 90,
                    height: 5,
                }],
            ),
        ] {
            let result = CompareResult {
                is_same_dimensions: alignment == DiffAlignment::default(),
                dimension_difference: (0, 5),
                ref_width: 32,
                ref_height: 128,
                test_width: 32,
                test_height: 133,
                alignment: alignment.clone(),
                mismatch_pixels: 17,
                raw_mis_match_percentage: 1.0,
                analysis_time_ms: 0,
            };
            let mut value = serde_json::to_value(build_diff(&result)).unwrap();
            assert_eq!(
                value["alignment"],
                serde_json::to_value(&alignment).unwrap()
            );
            let parsed: ReportDiff = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(parsed.alignment, Some(alignment.clone()));
            assert_eq!(parsed.mismatch_pixels, Some(17));
            assert_eq!(value["mismatchPixels"], 17);
            #[cfg(feature = "desktop")]
            {
                let parsed: crate::desktop::results::Difference =
                    serde_json::from_value(value.clone()).unwrap();
                assert_eq!(parsed.alignment, Some(alignment));
            }
            value.as_object_mut().unwrap().remove("alignment");
            value.as_object_mut().unwrap().remove("mismatchPixels");
            let legacy: ReportDiff = serde_json::from_value(value).unwrap();
            assert!(legacy.alignment.is_none());
            assert!(legacy.mismatch_pixels.is_none());
            assert!(
                serde_json::to_value(&legacy)
                    .unwrap()
                    .get("mismatchPixels")
                    .is_none()
            );
            assert!(
                serde_json::to_value(legacy)
                    .unwrap()
                    .get("alignment")
                    .is_none()
            );
        }
    }

    #[test]
    fn pixel_match_threshold_manifest_controls_each_capture() {
        use crate::diagnostics::{journal::JournalReader, record::*, runtime};
        let directory = tempfile::tempdir().unwrap();
        let suite = directory.path().join("shop");
        let run = suite.join("bitmaps_test/run");
        let reference = suite.join("bitmaps_reference");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(&reference).unwrap();
        let reference_png =
            lodepng::encode32(&[rgb::RGBA8::new(255, 255, 255, 255); 9], 3, 3).unwrap();
        let test_png = lodepng::encode32(&[rgb::RGBA8::new(230, 240, 237, 255); 9], 3, 3).unwrap();
        for key in [
            "default__FINAL",
            "loose__FINAL",
            "precise__FINAL",
            "precise__$step",
        ] {
            fs::write(reference.join(format!("{key}.png")), &reference_png).unwrap();
            fs::write(run.join(format!("{key}.png")), &test_png).unwrap();
        }
        let mut thresholds =
            serde_json::json!({"loose": 10.0, "precise": 10.0, "precise__FINAL": 0.0});
        fs::write(
            run.join(REPORT_PIXEL_THRESHOLDS_FILE),
            serde_json::to_vec(&thresholds).unwrap(),
        )
        .unwrap();

        for workers in [1, 3] {
            let operation = runtime::Operation::start(
                directory.path(),
                OperationKind::Command,
                None,
                None,
                None,
            );
            runtime::in_scope(Some(operation.context()), || {
                generate_comparison_report(&suite, "shop", "run", 0.0, workers)
            })
            .unwrap();
            let reference = operation.finish_blocking(Outcome::Success);
            let reader = JournalReader::open(directory.path(), &reference.operation_id).unwrap();
            let mut compared = 0;
            reader
                .visit(|record| {
                    if record.event_name == DiagnosticEvent::Compared {
                        compared += 1;
                        assert_eq!(record.severity, record.outcome.unwrap().severity());
                        assert_eq!(
                            record.fields.get(&Field::Command),
                            Some(&FieldValue::Kind(SafeValue::Compare))
                        );
                    }
                })
                .unwrap();
            assert_eq!(compared, 4);
            let report: ReportConfig =
                serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
            assert_eq!(report.tests.len(), 4);
            for test in &report.tests {
                let expected = match test.pair.file_name.as_str() {
                    "default__FINAL.png" => 5.0,
                    "precise__FINAL.png" => 0.0,
                    _ => 10.0,
                };
                assert_eq!(test.pair.pixel_match_threshold, expected);
                assert_eq!(test.pair.mis_match_threshold, 0.0);
                assert_eq!(test.status, if expected < 10.0 { "fail" } else { "pass" });
                assert_eq!(test.pair.diff_image.is_some(), expected < 10.0);
                assert_eq!(
                    test.pair.diff.as_ref().unwrap().raw_mis_match_percentage,
                    if expected < 10.0 { 100.0 } else { 0.0 }
                );
            }
        }

        thresholds["default"] = serde_json::json!(100.0);
        fs::write(
            run.join(REPORT_PIXEL_THRESHOLDS_FILE),
            serde_json::to_vec(&thresholds).unwrap(),
        )
        .unwrap();
        generate_comparison_report(&suite, "shop", "run", 0.0, 1).unwrap();
        assert!(!run.join("default__FINAL_diff.png").exists());
    }

    #[test]
    fn pixel_match_threshold_manifest_rejects_invalid_values() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            read_report_pixel_thresholds(directory.path())
                .unwrap()
                .is_none()
        );
        for contents in [
            "not json",
            r#"{"home":-0.01}"#,
            r#"{"home":100.01}"#,
            r#"{"home":null}"#,
            r#"{"home":"1"}"#,
            r#"{"":1}"#,
        ] {
            fs::write(
                directory.path().join(REPORT_PIXEL_THRESHOLDS_FILE),
                contents,
            )
            .unwrap();
            assert!(
                read_report_pixel_thresholds(directory.path())
                    .unwrap_err()
                    .to_string()
                    .contains("Invalid report threshold manifest")
            );
        }
    }

    #[test]
    fn legacy_reports_retain_the_original_pixel_match_threshold_on_approval() {
        let directory = viewport_approval_fixture();
        let suite = directory.path().join("shop");
        let report_path = suite.join("bitmaps_test/reviewed/report.json");
        let mut report: serde_json::Value =
            serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
        for test in report["tests"].as_array_mut().unwrap() {
            test["pair"]
                .as_object_mut()
                .unwrap()
                .remove("pixelMatchThreshold");
        }
        fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();

        approve_scenario_viewport(&suite, "reviewed", "checkout", "checkout_desktop__FINAL")
            .unwrap();

        let report: ReportConfig = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
        assert!(
            report
                .tests
                .iter()
                .all(|test| test.pair.pixel_match_threshold == 10.0)
        );
    }

    #[test]
    fn suite_display_name_metadata_round_trips_without_changing_report_identity() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("shop");
        assert!(read_suite_metadata(&suite_dir).unwrap().is_none());
        fs::create_dir_all(suite_dir.join("bitmaps_test/run")).unwrap();
        fs::write(
            suite_dir.join("bitmaps_test/run/home__FINAL.png"),
            png_bytes(10),
        )
        .unwrap();
        for name in [Some("Customer checkout"), None, Some("  ")] {
            write_suite_metadata(&suite_dir, name).unwrap();
            generate_comparison_report(&suite_dir, "shop", "run", 0.1, 1).unwrap();
            let report: ReportConfig = serde_json::from_slice(
                &fs::read(suite_dir.join("bitmaps_test/run/report.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(report.id, "shop");
            assert_eq!(report.test_suite, "shop");
            assert_eq!(
                report.display_name.as_deref(),
                name.filter(|name| !name.trim().is_empty())
            );
        }
        let metadata = fs::read(suite_dir.join(SUITE_METADATA_FILE)).unwrap();
        assert!(
            write_suite_metadata(&suite_dir, Some(&"x".repeat(MAX_SUITE_METADATA_BYTES))).is_err()
        );
        assert_eq!(
            fs::read(suite_dir.join(SUITE_METADATA_FILE)).unwrap(),
            metadata
        );
        let diff = suite_dir.join("bitmaps_test/run/home__FINAL_diff.png");
        fs::write(&diff, b"previous diff").unwrap();
        fs::write(suite_dir.join(SUITE_METADATA_FILE), b"invalid").unwrap();
        assert!(read_suite_metadata(&suite_dir).is_err());
        assert!(generate_comparison_report(&suite_dir, "shop", "run", 0.1, 1).is_err());
        assert_eq!(fs::read(diff).unwrap(), b"previous diff");
        fs::write(suite_dir.join(SUITE_METADATA_FILE), vec![b' '; 65_537]).unwrap();
        assert!(read_suite_metadata(&suite_dir).is_err());
    }

    #[test]
    fn scenario_scoped_compare_and_approve_preserve_unselected_references() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test/run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();
        let mut captures = BTreeMap::new();
        for id in ["home", "home-extra"] {
            let key = format!("{id}__FINAL");
            fs::write(reference_dir.join(format!("{key}.png")), png_bytes(10)).unwrap();
            fs::write(test_dir.join(format!("{key}.png")), png_bytes(200)).unwrap();
            captures.insert(
                key,
                ReportPageUrls {
                    scenario_id: Some(id.into()),
                    scenario_label: Some(id.into()),
                    scenario_display_name: None,
                    reference_url: "https://example.com".into(),
                    url: "https://example.com".into(),
                },
            );
        }
        fs::write(
            test_dir.join(REPORT_URLS_FILE),
            serde_json::to_vec(&ReportUrlsManifest {
                version: REPORT_URLS_VERSION,
                captures,
            })
            .unwrap(),
        )
        .unwrap();
        let selected = vec!["home".into()];
        generate_comparison_report_for_scenarios(&suite_dir, "basic", "run", 0.0, 1, &selected)
            .unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report["tests"].as_array().unwrap().len(), 1);
        assert_eq!(report["tests"][0]["pair"]["scenarioId"], "home");
        assert_eq!(report["captureInventoryComplete"], false);
        approve_screenshots_for_scenarios(&suite_dir, Some("run"), &selected).unwrap();
        assert_eq!(
            fs::read(reference_dir.join("home__FINAL.png")).unwrap(),
            png_bytes(200)
        );
        assert_eq!(
            fs::read(reference_dir.join("home-extra__FINAL.png")).unwrap(),
            png_bytes(10)
        );
        assert!(validate_scenario_captures(&test_dir, &["missing".into()]).is_err());
        ensure_capture_report(&suite_dir, "basic", "run").unwrap();
        let partial: serde_json::Value =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(partial["tests"].as_array().unwrap().len(), 1);
        ensure_complete_capture_report(&suite_dir, "basic", "run").unwrap();
        let complete: serde_json::Value =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(complete["tests"].as_array().unwrap().len(), 2);
        assert_eq!(complete["captureInventoryComplete"], true);
    }

    #[test]
    fn scenario_scoped_approval_refuses_legacy_or_missing_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path();
        let test_dir = suite_dir.join("bitmaps_test/run");
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(test_dir.join("home__FINAL.png"), png_bytes(200)).unwrap();
        let error = approve_screenshots_for_scenarios(suite_dir, Some("run"), &["home".into()])
            .unwrap_err();
        assert!(error.to_string().contains("no scenario metadata"));
        assert!(!suite_dir.join("bitmaps_reference").exists());
        approve_screenshots(suite_dir, Some("run")).unwrap();
    }

    #[test]
    fn scenario_viewport_approval_selects_only_matching_steps_and_targets() {
        let scenario = "shop__checkout";
        let mut urls = BTreeMap::new();
        let captures = [
            ("shop__checkout_desktop__FINAL", scenario),
            ("shop__checkout_desktop__$payment", scenario),
            ("shop__checkout_desktop__SELECTOR-form", scenario),
            ("shop__checkout_mobile__FINAL", scenario),
            ("shop__checkout__FINAL", scenario),
            (
                "shop__checkout_desktop_extra__FINAL",
                "shop__checkout_desktop_extra",
            ),
        ];
        let mut screenshots = Vec::new();
        for (key, scenario_id) in captures {
            screenshots.push(format!("{key}.png"));
            urls.insert(
                key.to_string(),
                ReportPageUrls {
                    scenario_id: Some(scenario_id.into()),
                    scenario_label: Some("Checkout".into()),
                    scenario_display_name: None,
                    reference_url: "https://example.test".into(),
                    url: "https://example.test".into(),
                },
            );
        }
        assert_eq!(
            select_scenario_viewport_screenshots(
                screenshots.clone(),
                Some(&urls),
                scenario,
                "shop__checkout_desktop__$payment",
            )
            .unwrap(),
            screenshots[..3]
        );
        assert_eq!(
            select_scenario_viewport_screenshots(
                screenshots.clone(),
                Some(&urls),
                scenario,
                "shop__checkout__FINAL",
            )
            .unwrap(),
            ["shop__checkout__FINAL.png"]
        );
        for key in [
            "missing",
            "../shop__checkout_desktop__FINAL",
            "shop__checkout_desktop_extra__FINAL",
        ] {
            assert!(
                select_scenario_viewport_screenshots(
                    screenshots.clone(),
                    Some(&urls),
                    scenario,
                    key,
                )
                .is_err()
            );
        }
        assert!(
            select_scenario_viewport_screenshots(
                screenshots.clone(),
                None,
                scenario,
                "shop__checkout_desktop__FINAL",
            )
            .is_err()
        );
        assert!(
            select_scenario_viewport_screenshots(
                screenshots,
                Some(&urls),
                "",
                "shop__checkout_desktop__FINAL",
            )
            .is_err()
        );
    }

    fn viewport_approval_fixture() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let suite = directory.path().join("shop");
        let run = suite.join("bitmaps_test/reviewed");
        let reference = suite.join("bitmaps_reference");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(&reference).unwrap();
        let mut captures = BTreeMap::new();
        for (key, scenario) in [
            ("checkout_desktop__FINAL", "checkout"),
            ("checkout_desktop__$payment", "checkout"),
            ("checkout_desktop__VIEWPORT", "checkout"),
            ("checkout_mobile__FINAL", "checkout"),
            ("other_desktop__FINAL", "other"),
            ("hidden_desktop__FINAL", "hidden"),
        ] {
            fs::write(run.join(format!("{key}.png")), png_bytes(200)).unwrap();
            fs::write(reference.join(format!("{key}.png")), png_bytes(10)).unwrap();
            captures.insert(
                key.to_string(),
                ReportPageUrls {
                    scenario_id: Some(scenario.into()),
                    scenario_label: Some(format!("Recorded {scenario}")),
                    scenario_display_name: Some(format!("Friendly {scenario}")),
                    reference_url: "https://production.example.test".into(),
                    url: "https://staging.example.test".into(),
                },
            );
        }
        fs::write(
            run.join(REPORT_URLS_FILE),
            serde_json::to_vec(&ReportUrlsManifest {
                version: REPORT_URLS_VERSION,
                captures,
            })
            .unwrap(),
        )
        .unwrap();
        fs::write(
            crate::test::capture::error_marker_path(&run.join("checkout_desktop__VIEWPORT.png")),
            r#"{"error":true,"message":"Capture failed"}"#,
        )
        .unwrap();
        fs::write(
            run.join(REPORT_THRESHOLDS_FILE),
            r#"{"checkout_mobile__FINAL":25.0}"#,
        )
        .unwrap();
        fs::write(
            run.join(REPORT_PIXEL_THRESHOLDS_FILE),
            r#"{"checkout_mobile__FINAL":2.0}"#,
        )
        .unwrap();
        generate_comparison_report_for_scenarios(
            &suite,
            "shop",
            "reviewed",
            20.0,
            1,
            &["checkout".into(), "other".into()],
        )
        .unwrap();
        directory
    }

    #[test]
    fn scenario_viewport_approval_promotes_only_one_viewport_and_recalculates_the_report() {
        let directory = viewport_approval_fixture();
        let suite = directory.path().join("shop");
        let run = suite.join("bitmaps_test/reviewed");
        let reference = suite.join("bitmaps_reference");
        let before: ReportConfig =
            serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
        let stale_marker =
            crate::test::capture::error_marker_path(&reference.join("checkout_desktop__FINAL.png"));
        fs::write(
            &stale_marker,
            r#"{"error":true,"message":"Old reference failed"}"#,
        )
        .unwrap();
        fs::write(
            run.join(REPORT_THRESHOLDS_FILE),
            r#"{"checkout_mobile__FINAL":100.0}"#,
        )
        .unwrap();
        fs::write(
            run.join(REPORT_PIXEL_THRESHOLDS_FILE),
            r#"{"checkout_mobile__FINAL":100.0}"#,
        )
        .unwrap();
        approve_scenario_viewport(&suite, "reviewed", "checkout", "checkout_desktop__FINAL")
            .unwrap();
        let report_bytes = fs::read(run.join("report.json")).unwrap();
        let report: ReportConfig = serde_json::from_slice(&report_bytes).unwrap();
        assert_eq!(report.tests.len(), 5);
        assert_eq!(
            report
                .tests
                .iter()
                .filter(|test| test.status == "pass")
                .count(),
            2
        );
        for (previous, current) in before.tests.iter().zip(&report.tests) {
            assert_eq!(previous.pair.file_name, current.pair.file_name);
            assert_eq!(
                previous.pair.mis_match_threshold,
                current.pair.mis_match_threshold
            );
            assert_eq!(
                previous.pair.pixel_match_threshold,
                current.pair.pixel_match_threshold
            );
            assert_eq!(previous.pair.scenario_label, current.pair.scenario_label);
            assert_eq!(
                previous.pair.scenario_display_name,
                current.pair.scenario_display_name
            );
            assert_eq!(previous.pair.reference_url, current.pair.reference_url);
            assert_eq!(previous.pair.url, current.pair.url);
            if current.status == "pass" {
                assert_eq!(
                    current.pair.diff.as_ref().unwrap().raw_mis_match_percentage,
                    0.0
                );
                assert!(current.pair.engine_error_msg.is_none());
                assert!(current.pair.diff_image.is_none());
                assert!(
                    !run.join(current.pair.file_name.replace(".png", "_diff.png"))
                        .exists()
                );
            }
        }
        for key in ["checkout_desktop__FINAL", "checkout_desktop__$payment"] {
            assert_eq!(
                fs::read(reference.join(format!("{key}.png"))).unwrap(),
                png_bytes(200)
            );
        }
        for key in [
            "checkout_mobile__FINAL",
            "checkout_desktop__VIEWPORT",
            "other_desktop__FINAL",
            "hidden_desktop__FINAL",
        ] {
            assert_eq!(
                fs::read(reference.join(format!("{key}.png"))).unwrap(),
                png_bytes(10)
            );
        }
        assert!(!stale_marker.exists());
        assert_eq!(
            fs::read_to_string(suite.join("html_report/config.js")).unwrap(),
            format!("report({});", String::from_utf8(report_bytes).unwrap())
        );
        approve_scenario_viewport(&suite, "reviewed", "checkout", "checkout_mobile__FINAL")
            .unwrap();
        let report: ReportConfig =
            serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
        assert_eq!(
            report
                .tests
                .iter()
                .filter(|test| test.status == "pass")
                .count(),
            3
        );
        assert_eq!(
            fs::read(reference.join("other_desktop__FINAL.png")).unwrap(),
            png_bytes(10)
        );
    }

    #[test]
    fn scenario_viewport_approval_rejects_stale_invalid_and_failed_selections_before_writing() {
        let directory = viewport_approval_fixture();
        let suite = directory.path().join("shop");
        let run = suite.join("bitmaps_test/reviewed");
        let original_report = fs::read(run.join("report.json")).unwrap();
        for (run_id, scenario, key) in [
            ("missing", "checkout", "checkout_desktop__FINAL"),
            ("../reviewed", "checkout", "checkout_desktop__FINAL"),
            ("reviewed", "other", "checkout_desktop__FINAL"),
            ("reviewed", "checkout", "../checkout_desktop__FINAL"),
            ("reviewed", "checkout", "checkout_desktop__VIEWPORT"),
            ("reviewed", "hidden", "hidden_desktop__FINAL"),
        ] {
            assert!(approve_scenario_viewport(&suite, run_id, scenario, key).is_err());
        }
        let mut invalid: serde_json::Value = serde_json::from_slice(&original_report).unwrap();
        invalid["tests"][0]["pair"]["scenarioId"] = "changed".into();
        fs::write(
            run.join("report.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        assert!(
            approve_scenario_viewport(&suite, "reviewed", "checkout", "checkout_desktop__FINAL")
                .is_err()
        );
        fs::write(run.join("report.json"), &original_report).unwrap();
        fs::remove_file(run.join(REPORT_URLS_FILE)).unwrap();
        assert!(
            approve_scenario_viewport(&suite, "reviewed", "checkout", "checkout_desktop__FINAL")
                .is_err()
        );
        assert_eq!(fs::read(run.join("report.json")).unwrap(), original_report);
        for entry in fs::read_dir(suite.join("bitmaps_reference")).unwrap() {
            assert_eq!(fs::read(entry.unwrap().path()).unwrap(), png_bytes(10));
        }
    }

    #[test]
    fn scenario_viewport_approval_validates_all_selected_pngs_before_promotion() {
        let directory = viewport_approval_fixture();
        let suite = directory.path().join("shop");
        let run = suite.join("bitmaps_test/reviewed");
        let original_report = fs::read(run.join("report.json")).unwrap();
        fs::write(
            run.join("checkout_desktop__FINAL.png"),
            &png_bytes(200)[..33],
        )
        .unwrap();
        let error =
            approve_scenario_viewport(&suite, "reviewed", "checkout", "checkout_desktop__$payment")
                .unwrap_err();
        assert!(error.to_string().contains("invalid capture"));
        assert_eq!(fs::read(run.join("report.json")).unwrap(), original_report);
        for entry in fs::read_dir(suite.join("bitmaps_reference")).unwrap() {
            assert_eq!(fs::read(entry.unwrap().path()).unwrap(), png_bytes(10));
        }
    }

    #[test]
    fn regenerating_report_ignores_prior_diff_images() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        let screenshot_name = "home__FINAL.png";
        fs::write(reference_dir.join(screenshot_name), png_bytes(10)).unwrap();
        fs::write(test_dir.join(screenshot_name), png_bytes(200)).unwrap();

        generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap();
        generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap();

        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        let tests = report["tests"].as_array().unwrap();

        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0]["status"], "fail");
        assert!(test_dir.join("home__FINAL_diff.png").exists());
    }

    #[test]
    fn report_removes_stale_diff_when_capture_becomes_identical() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        let screenshot_name = "home__FINAL.png";
        fs::write(reference_dir.join(screenshot_name), png_bytes(10)).unwrap();
        fs::write(test_dir.join(screenshot_name), png_bytes(200)).unwrap();
        generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap();
        let diff_path = test_dir.join("home__FINAL_diff.png");
        assert!(diff_path.exists());

        fs::write(test_dir.join(screenshot_name), png_bytes(10)).unwrap();
        generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap();

        assert!(!diff_path.exists());
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report["tests"][0]["status"], "pass");
        assert!(report["tests"][0]["pair"].get("diffImage").is_none());
    }

    #[test]
    fn report_includes_reference_and_test_page_urls_from_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        let screenshot_name = "home__FINAL.png";
        fs::write(reference_dir.join(screenshot_name), png_bytes(10)).unwrap();
        fs::write(test_dir.join(screenshot_name), png_bytes(10)).unwrap();
        fs::write(
            test_dir.join(REPORT_URLS_FILE),
            serde_json::to_vec(&ReportUrlsManifest {
                version: REPORT_URLS_VERSION,
                captures: BTreeMap::from([(
                    "home__FINAL".to_string(),
                    ReportPageUrls {
                        scenario_id: Some("home".into()),
                        scenario_label: Some("Home page".into()),
                        scenario_display_name: Some("Recorded home page".into()),
                        reference_url: "https://www.example.com/home".to_string(),
                        url: "https://test.example.com/home".to_string(),
                    },
                )]),
            })
            .unwrap(),
        )
        .unwrap();

        generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap();

        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(
            report["tests"][0]["pair"]["referenceUrl"],
            "https://www.example.com/home"
        );
        assert_eq!(
            report["tests"][0]["pair"]["url"],
            "https://test.example.com/home"
        );
        let config = fs::read_to_string(suite_dir.join("html_report/config.js")).unwrap();
        assert!(config.contains("\"referenceUrl\": \"https://www.example.com/home\""));
        assert!(config.contains("\"url\": \"https://test.example.com/home\""));
        assert_eq!(report["tests"][0]["pair"]["scenarioId"], "home");
        assert_eq!(report["tests"][0]["pair"]["scenarioLabel"], "Home page");
        assert_eq!(
            report["tests"][0]["pair"]["scenarioDisplayName"],
            "Recorded home page"
        );
        assert_eq!(report["tests"][0]["pair"]["label"], "home FINAL");
        let legacy: ReportPageUrls = serde_json::from_value(serde_json::json!({"referenceUrl": "https://example.com", "url": "https://example.com"})).unwrap();
        assert!(legacy.scenario_id.is_none());
        assert!(legacy.scenario_label.is_none());
        assert!(legacy.scenario_display_name.is_none());
    }

    #[test]
    fn parallel_comparison_preserves_manifest_order() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        for (index, name) in ["alpha__FINAL.png", "beta__FINAL.png", "gamma__FINAL.png"]
            .iter()
            .enumerate()
        {
            let png = png_bytes((index as u8) + 10);
            fs::write(reference_dir.join(name), &png).unwrap();
            fs::write(test_dir.join(name), png).unwrap();
        }
        fs::write(
            test_dir.join(REPORT_ORDER_FILE),
            serde_json::to_vec(&["gamma", "alpha", "beta"]).unwrap(),
        )
        .unwrap();

        generate_comparison_report(&suite_dir, "basic", "run", 0.0, 2).unwrap();

        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        let file_names: Vec<_> = report["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|test| test["pair"]["fileName"].as_str().unwrap())
            .collect();

        assert_eq!(
            file_names,
            vec!["gamma__FINAL.png", "alpha__FINAL.png", "beta__FINAL.png"]
        );
    }

    #[test]
    fn approve_replaces_reference_and_clears_stale_error_marker() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        let screenshot_name = "home__FINAL.png";
        let reference = reference_dir.join(screenshot_name);
        fs::write(&reference, png_bytes(10)).unwrap();
        fs::write(
            crate::test::capture::error_marker_path(&reference),
            br#"{"error":true,"message":"old capture failed"}"#,
        )
        .unwrap();
        fs::write(test_dir.join(screenshot_name), png_bytes(200)).unwrap();

        approve_screenshots(&suite_dir, None).unwrap();

        assert_eq!(fs::read(reference).unwrap(), png_bytes(200));
        assert!(
            !crate::test::capture::error_marker_path(&reference_dir.join(screenshot_name)).exists()
        );
    }

    #[test]
    fn approve_skips_capture_with_malformed_error_marker() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        let screenshot_name = "home__FINAL.png";
        let capture = test_dir.join(screenshot_name);
        fs::write(&capture, png_bytes(200)).unwrap();
        fs::write(
            crate::test::capture::error_marker_path(&capture),
            b"invalid marker",
        )
        .unwrap();

        approve_screenshots(&suite_dir, None).unwrap();

        assert!(!reference_dir.join(screenshot_name).exists());
    }

    #[test]
    fn approval_journal_retains_skipped_and_promoted_captures() {
        use crate::diagnostics::{journal::JournalReader, record::*, runtime};
        let root = tempfile::tempdir().unwrap();
        let test_dir = root.path().join("suite/bitmaps_test/run");
        let reference_dir = root.path().join("suite/bitmaps_reference");
        fs::create_dir_all(&test_dir).unwrap();
        fs::create_dir_all(&reference_dir).unwrap();
        for name in ["passed.png", "failed.png"] {
            fs::write(test_dir.join(name), png_bytes(200)).unwrap();
            fs::write(reference_dir.join(name), png_bytes(10)).unwrap();
        }
        fs::write(
            crate::test::capture::error_marker_path(&test_dir.join("failed.png")),
            b"synthetic-private-marker",
        )
        .unwrap();
        let operation =
            runtime::Operation::start(root.path(), OperationKind::Command, None, None, None);
        let count = runtime::in_scope(Some(operation.context()), || {
            promote_screenshots(
                &test_dir,
                &reference_dir,
                &["passed.png".into(), "failed.png".into()],
            )
        })
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            fs::read(reference_dir.join("passed.png")).unwrap(),
            png_bytes(200)
        );
        assert_eq!(
            fs::read(reference_dir.join("failed.png")).unwrap(),
            png_bytes(10)
        );
        let reference = operation.finish_blocking(Outcome::Success);
        let reader = JournalReader::open(root.path(), &reference.operation_id).unwrap();
        let mut decisions = Vec::new();
        reader
            .visit(|record| {
                let encoded = serde_json::to_string(&record).unwrap();
                for private in ["passed.png", "failed.png", "synthetic-private-marker"] {
                    assert!(!encoded.contains(private));
                }
                if record.fields.contains_key(&Field::Artifact) {
                    assert_eq!(
                        record.fields.get(&Field::Command),
                        Some(&FieldValue::Kind(SafeValue::Approve))
                    );
                    decisions.push((record.severity, record.fields.get(&Field::State).cloned()));
                }
            })
            .unwrap();
        assert_eq!(
            decisions,
            vec![
                (Severity::Debug, Some(FieldValue::Kind(SafeValue::Complete))),
                (Severity::Warn, Some(FieldValue::Kind(SafeValue::Failed))),
            ]
        );
    }

    #[test]
    fn malformed_report_manifests_fail_generation() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(reference_dir.join("home__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("home__FINAL.png"), png_bytes(10)).unwrap();

        fs::write(test_dir.join(REPORT_ORDER_FILE), b"not json").unwrap();
        let error = generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap_err();
        assert!(error.to_string().contains("Invalid report order manifest"));

        fs::remove_file(test_dir.join(REPORT_ORDER_FILE)).unwrap();
        fs::write(test_dir.join(REPORT_THRESHOLDS_FILE), b"not json").unwrap();
        let error = generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Invalid report threshold manifest")
        );

        fs::remove_file(test_dir.join(REPORT_THRESHOLDS_FILE)).unwrap();
        fs::write(test_dir.join(REPORT_ORDER_FILE), br#"["home","home"]"#).unwrap();
        let error = generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap_err();
        assert!(error.to_string().contains("duplicate or empty key"));

        fs::remove_file(test_dir.join(REPORT_ORDER_FILE)).unwrap();
        fs::write(test_dir.join(REPORT_THRESHOLDS_FILE), br#"{"home":101}"#).unwrap();
        let error = generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap_err();
        assert!(error.to_string().contains("finite value between 0 and 100"));
    }

    #[test]
    fn complete_manifest_rejects_missing_expected_captures() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        fs::write(reference_dir.join("available__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("available__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(
            test_dir.join(REPORT_ORDER_FILE),
            serde_json::to_vec(&ReportOrderManifest {
                version: REPORT_ORDER_VERSION,
                captures: vec!["available__FINAL".to_string(), "missing__FINAL".to_string()],
            })
            .unwrap(),
        )
        .unwrap();

        let error = generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap_err();

        assert!(error.to_string().contains("missing 1 screenshot"));
        assert!(error.to_string().contains("missing__FINAL"));
    }

    #[test]
    fn complete_manifest_rejects_unexpected_captures_before_approval() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        fs::write(reference_dir.join("home__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("home__FINAL.png"), png_bytes(20)).unwrap();
        fs::write(test_dir.join("unexpected__FINAL.png"), png_bytes(30)).unwrap();
        fs::write(
            test_dir.join(REPORT_ORDER_FILE),
            serde_json::to_vec(&ReportOrderManifest {
                version: REPORT_ORDER_VERSION,
                captures: vec!["home__FINAL".to_string()],
            })
            .unwrap(),
        )
        .unwrap();

        let error = approve_screenshots(&suite_dir, None).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("contains 1 unexpected screenshot")
        );
        assert_eq!(
            fs::read(reference_dir.join("home__FINAL.png")).unwrap(),
            png_bytes(10)
        );
    }

    #[test]
    fn capture_run_selection_honors_explicit_id_before_latest_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let runs = temp.path().join("bitmaps_test");
        fs::create_dir_all(runs.join("20260913_120000_000000001")).unwrap();
        fs::create_dir_all(runs.join("z-newer-name")).unwrap();

        assert_eq!(
            select_capture_run(&runs, Some("20260913_120000_000000001")).unwrap(),
            Some("20260913_120000_000000001".to_string())
        );
        assert_eq!(
            select_capture_run(&runs, None).unwrap(),
            Some("z-newer-name".to_string())
        );
        assert!(
            select_capture_run(&runs, Some("missing"))
                .unwrap_err()
                .to_string()
                .contains("Requested capture run 'missing' does not exist")
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_capture_run_selection_rejects_symlinked_directories() {
        let temp = tempfile::tempdir().unwrap();
        let runs = temp.path().join("bitmaps_test");
        let outside = temp.path().join("outside-run");
        fs::create_dir_all(&runs).unwrap();
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, runs.join("linked-run")).unwrap();

        let error = select_capture_run(&runs, Some("linked-run")).unwrap_err();

        assert!(error.to_string().contains("not a real directory"));
    }

    #[test]
    fn capture_run_selection_uses_completed_full_suite_runs() {
        let temp = tempfile::tempdir().unwrap();
        let runs = temp.path().join("bitmaps_test");
        let records = [
            ("a-completed", "completed", true),
            ("b-cancelled", "interrupted", true),
            ("c-partial", "completed", false),
            ("z-running", "running", true),
        ];
        for (id, _, _) in records {
            fs::create_dir_all(runs.join(id)).unwrap();
        }
        fs::write(
            temp.path().join("runs.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "runs": records.iter().enumerate().map(|(index, (id, state, full_suite))| {
                    serde_json::json!({
                        "id": id,
                        "state": state,
                        "fullSuite": full_suite,
                        "startedAt": index as u64,
                        "completedAt": if *state == "completed" { Some(index as u64) } else { None },
                    })
                }).collect::<Vec<_>>(),
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            select_capture_run(&runs, None).unwrap().as_deref(),
            Some("a-completed")
        );
        assert_eq!(
            select_capture_run(&runs, Some("c-partial"))
                .unwrap()
                .as_deref(),
            Some("c-partial")
        );
        fs::remove_dir_all(runs.join("a-completed")).unwrap();
        assert_eq!(select_capture_run(&runs, None).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn capture_discovery_rejects_non_utf8_file_and_run_names() {
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().unwrap();
        let capture_dir = temp.path().join("captures");
        fs::create_dir_all(&capture_dir).unwrap();
        let invalid_file = std::ffi::OsString::from_vec(b"capture-\xff.png".to_vec());
        fs::write(capture_dir.join(invalid_file), []).unwrap();

        let file_error = collect_capture_screenshots(&capture_dir).unwrap_err();
        assert!(file_error.to_string().contains("not valid UTF-8"));

        let run_dir = temp.path().join("runs");
        fs::create_dir_all(&run_dir).unwrap();
        let invalid_run = std::ffi::OsString::from_vec(b"run-\xff".to_vec());
        fs::create_dir(run_dir.join(invalid_run)).unwrap();

        let run_error = select_capture_run(&run_dir, None).unwrap_err();
        assert!(run_error.to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn intermediate_screenshots_use_scenario_order_and_threshold_fallbacks() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test").join("run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        fs::write(reference_dir.join("home__$after-login.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("home__$after-login.png"), png_bytes(200)).unwrap();
        fs::write(reference_dir.join("home__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("home__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(
            test_dir.join(REPORT_ORDER_FILE),
            serde_json::to_vec(&["home__FINAL"]).unwrap(),
        )
        .unwrap();
        fs::write(
            test_dir.join(REPORT_THRESHOLDS_FILE),
            serde_json::to_vec(&HashMap::from([("home".to_string(), 100.0)])).unwrap(),
        )
        .unwrap();

        generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap();

        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        let tests = report["tests"].as_array().unwrap();
        assert_eq!(tests[0]["pair"]["fileName"], "home__$after-login.png");
        assert_eq!(tests[0]["status"], "pass");
        assert_eq!(tests[0]["pair"]["misMatchThreshold"], 100.0);
    }

    #[test]
    fn report_selector_describes_capture_target_from_filename() {
        assert_eq!(report_selector("home__FINAL.png"), "document");
        assert_eq!(report_selector("home__VIEWPORT.png"), "viewport");
        assert_eq!(
            report_selector("home__$after-login.png"),
            "save:after-login"
        );
        assert_eq!(
            report_selector("home__SELECTOR-main-hero-abc123.png"),
            "SELECTOR-main-hero-abc123"
        );
        assert_eq!(
            screenshot_prefix("shop__checkout_desktop__$after-login.png"),
            "shop__checkout_desktop"
        );
    }

    #[test]
    fn staged_suite_comparison_keeps_diffs_and_reports_in_staging_until_finalized() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test/run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        fs::write(reference_dir.join("first__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("first__FINAL.png"), png_bytes(200)).unwrap();
        fs::write(reference_dir.join("second__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("second__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("second__FINAL_diff.png"), b"stale diff").unwrap();
        fs::write(
            test_dir.join(REPORT_ORDER_FILE),
            serde_json::to_vec(&ReportOrderManifest {
                version: REPORT_ORDER_VERSION,
                captures: vec!["second__FINAL".into(), "first__FINAL".into()],
            })
            .unwrap(),
        )
        .unwrap();

        let aborted = StagedSuiteComparison::new(&suite_dir, "basic", "run", 0.0, 2).unwrap();
        aborted
            .compare_job_captures(&["first__FINAL".into()])
            .unwrap();
        let aborted_staging = aborted.staging_dir.path().to_path_buf();
        assert!(aborted_staging.join("first__FINAL_diff.png").is_file());
        assert!(!test_dir.join("first__FINAL_diff.png").exists());
        assert!(!test_dir.join("report.json").exists());
        assert!(!suite_dir.join("html_report/config.js").exists());
        drop(aborted);
        assert!(!aborted_staging.exists());
        assert!(!test_dir.join("first__FINAL_diff.png").exists());
        assert!(!test_dir.join("report.json").exists());

        let staged = StagedSuiteComparison::new(&suite_dir, "basic", "run", 0.0, 2).unwrap();
        staged
            .compare_job_captures(&["first__FINAL".into()])
            .unwrap();
        staged
            .compare_job_captures(&["second__FINAL".into()])
            .unwrap();
        assert!(!test_dir.join("first__FINAL_diff.png").exists());
        assert!(!test_dir.join("report.json").exists());
        assert!(!suite_dir.join("html_report/config.js").exists());

        staged.finalize().unwrap();
        assert!(test_dir.join("first__FINAL_diff.png").is_file());
        assert!(!test_dir.join("second__FINAL_diff.png").exists());
        assert!(suite_dir.join("html_report/config.js").is_file());
        let report: ReportConfig =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report.tests.len(), 2);
        assert_eq!(report.tests[0].pair.file_name, "second__FINAL.png");
        assert_eq!(report.tests[0].status, "pass");
        assert_eq!(report.tests[1].pair.file_name, "first__FINAL.png");
        assert_eq!(report.tests[1].status, "fail");
    }

    #[test]
    fn reference_report_self_approves_passed_captures_and_retains_capture_failures() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test/ref-run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        // Even if reference and test bytes differ on disk, `generate_reference_report`
        // self-approves successful baseline captures without running pixel comparison.
        fs::write(reference_dir.join("passed__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("passed__FINAL.png"), png_bytes(200)).unwrap();
        fs::write(test_dir.join("failed__FINAL.png"), png_bytes(200)).unwrap();
        fs::write(
            crate::test::capture::error_marker_path(&test_dir.join("failed__FINAL.png")),
            r#"{"error":true,"message":"Action 1 (click): element missing"}"#,
        )
        .unwrap();
        fs::write(
            test_dir.join(REPORT_ORDER_FILE),
            serde_json::to_vec(&ReportOrderManifest {
                version: REPORT_ORDER_VERSION,
                captures: vec!["passed__FINAL".into(), "failed__FINAL".into()],
            })
            .unwrap(),
        )
        .unwrap();

        generate_reference_report(&suite_dir, "basic", "ref-run").unwrap();

        let report: ReportConfig =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report.tests.len(), 2);
        assert_eq!(report.tests[0].pair.file_name, "passed__FINAL.png");
        assert_eq!(report.tests[0].status, "pass");
        assert_eq!(
            report.tests[0]
                .pair
                .diff
                .as_ref()
                .unwrap()
                .raw_mis_match_percentage,
            0.0
        );
        assert!(report.tests[0].pair.diff_image.is_none());
        assert!(report.tests[0].pair.engine_error_msg.is_none());
        assert_eq!(report.tests[1].pair.file_name, "failed__FINAL.png");
        assert_eq!(report.tests[1].status, "fail");
        assert_eq!(
            report.tests[1].pair.engine_error_msg.as_deref(),
            Some("Test scenario failed to capture: Action 1 (click): element missing")
        );
    }

    #[test]
    fn capture_names_containing_png_preserve_single_diff_suffix_and_label() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test/run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        let file_name = "home__$logo.png.png";
        fs::write(reference_dir.join(file_name), png_bytes(10)).unwrap();
        fs::write(test_dir.join(file_name), png_bytes(200)).unwrap();

        generate_comparison_report(&suite_dir, "basic", "run", 0.0, 1).unwrap();

        let report: ReportConfig =
            serde_json::from_slice(&fs::read(test_dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report.tests.len(), 1);
        assert_eq!(report.tests[0].pair.label, "home logo.png");
        assert_eq!(
            report.tests[0].pair.diff_image.as_deref(),
            Some("../bitmaps_test/run/home__$logo.png_diff.png")
        );
        assert!(test_dir.join("home__$logo.png_diff.png").is_file());
        assert!(!test_dir.join("home__$logo_diff.png_diff.png").exists());
    }

    /// Write a capture and, when given, a digest-bound sidecar link recording
    /// its image simplification setting, as capture jobs and promotion do.
    fn write_capture(image: &Path, bytes: &[u8], setting: Option<serde_json::Value>) {
        use sha2::{Digest, Sha256};
        fs::write(image, bytes).unwrap();
        if let Some(setting) = setting {
            fs::write(
                image.with_extension("log.json"),
                serde_json::to_vec(&serde_json::json!({
                    "state": "legacyUnavailable",
                    "imageDigest": format!("{:x}", Sha256::digest(bytes)),
                    "imageSimplification": setting,
                }))
                .unwrap(),
            )
            .unwrap();
        }
    }

    fn pixelate_setting(block_size: u32) -> serde_json::Value {
        serde_json::json!({
            "mode": "pixelate",
            "blockSize": block_size,
            "minSize": 64,
            "excludeSelectorsDigest": crate::test::image_simplification::exclude_selectors_digest(&[]),
        })
    }

    #[test]
    fn image_simplification_setting_mismatches_warn_without_changing_comparison_results() {
        use crate::diagnostics::{journal::JournalReader, record::*, runtime};
        use serde_json::{Value, json};
        let directory = tempfile::tempdir().unwrap();
        let suite = directory.path().join("shop");
        let run = suite.join("bitmaps_test/run");
        let reference = suite.join("bitmaps_reference");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(&reference).unwrap();
        let none = json!({ "mode": "none" });
        let image = png_bytes(10);
        // (capture, reference link setting, test link setting, warns); pixels match.
        let cases = [
            (
                "same__FINAL",
                Some(pixelate_setting(4)),
                Some(pixelate_setting(4)),
                false,
            ),
            (
                "block__FINAL",
                Some(pixelate_setting(8)),
                Some(pixelate_setting(4)),
                true,
            ),
            // Old references without a recorded setting.
            ("legacy__FINAL", None, Some(pixelate_setting(4)), true),
            ("legacy-none__FINAL", None, Some(none.clone()), false),
            ("off__FINAL", Some(none.clone()), Some(none.clone()), false),
            // A test run captured before settings were recorded.
            ("old-run__FINAL", Some(pixelate_setting(4)), None, true),
            (
                "stale__FINAL",
                Some(pixelate_setting(4)),
                Some(pixelate_setting(4)),
                true,
            ),
        ];
        for (key, reference_setting, test_setting, _) in &cases {
            write_capture(
                &reference.join(format!("{key}.png")),
                &image,
                reference_setting.clone(),
            );
            write_capture(
                &run.join(format!("{key}.png")),
                &image,
                test_setting.clone(),
            );
        }
        // The stale reference changed after its link was written, so the link
        // no longer vouches for it and its setting counts as unrecorded.
        write_capture(
            &reference.join("stale__FINAL.png"),
            &png_bytes(11),
            Some(pixelate_setting(4)),
        );
        fs::write(reference.join("stale__FINAL.png"), &image).unwrap();
        // Pairs that are not compared get no settings warning.
        write_capture(
            &run.join("missing__FINAL.png"),
            &image,
            Some(pixelate_setting(4)),
        );
        write_capture(
            &run.join("failed__FINAL.png"),
            &image,
            Some(pixelate_setting(4)),
        );
        write_capture(
            &reference.join("failed__FINAL.png"),
            &image,
            Some(pixelate_setting(8)),
        );
        fs::write(
            crate::test::capture::error_marker_path(&run.join("failed__FINAL.png")),
            r#"{"error":true,"message":"Capture failed"}"#,
        )
        .unwrap();
        let warned: BTreeSet<String> = cases
            .iter()
            .filter(|case| case.3)
            .map(|case| format!("{}.png", case.0))
            .collect();
        let warnings_of = |report: &Value| -> BTreeMap<String, Value> {
            report["tests"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|test| {
                    test["pair"]
                        .get("imageSimplificationWarning")
                        .map(|warning| {
                            (
                                test["pair"]["fileName"].as_str().unwrap().to_string(),
                                warning.clone(),
                            )
                        })
                })
                .collect()
        };

        let mut generated = Value::Null;
        for workers in [1, 3] {
            let operation = runtime::Operation::start(
                directory.path(),
                OperationKind::Command,
                None,
                None,
                None,
            );
            runtime::in_scope(Some(operation.context()), || {
                generate_comparison_report(&suite, "shop", "run", 0.0, workers)
            })
            .unwrap();
            let archive = operation.finish_blocking(Outcome::Success);
            generated =
                serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
            let tests = generated["tests"].as_array().unwrap();
            assert_eq!(tests.len(), 9);
            for test in tests {
                let name = test["pair"]["fileName"].as_str().unwrap();
                if matches!(name, "missing__FINAL.png" | "failed__FINAL.png") {
                    assert_eq!(test["status"], "fail", "{name}");
                } else {
                    // A settings warning never changes the comparison result.
                    assert_eq!(test["status"], "pass", "{name}");
                    assert_eq!(test["pair"]["comparisonOutcome"], "success", "{name}");
                }
            }
            let warnings = warnings_of(&generated);
            assert_eq!(warnings.keys().cloned().collect::<BTreeSet<_>>(), warned);
            let block = &warnings["block__FINAL.png"];
            assert_eq!(block["reference"], pixelate_setting(8));
            assert_eq!(block["test"], pixelate_setting(4));
            assert!(
                block["message"]
                    .as_str()
                    .unwrap()
                    .contains("reference: pixelate, 8 px blocks, 64 px minimum, no exclusions; test: pixelate, 4 px blocks")
            );
            let legacy = &warnings["legacy__FINAL.png"];
            assert_eq!(legacy["reference"], Value::Null);
            assert_eq!(legacy["test"], pixelate_setting(4));
            assert!(
                legacy["message"]
                    .as_str()
                    .unwrap()
                    .contains("reference: no recorded setting; test: pixelate")
            );
            assert_eq!(warnings["old-run__FINAL.png"]["test"], Value::Null);
            assert_eq!(warnings["stale__FINAL.png"]["reference"], Value::Null);

            // One warning record per affected pair, carrying fingerprints only.
            let reader = JournalReader::open(directory.path(), &archive.operation_id).unwrap();
            let mut recorded = BTreeSet::new();
            reader
                .visit(|record| {
                    if record.event_name != DiagnosticEvent::Decision
                        || record.fields.get(&Field::Condition)
                            != Some(&FieldValue::Kind(SafeValue::Images))
                    {
                        return;
                    }
                    assert_eq!(record.severity, Severity::Warn);
                    assert_eq!(
                        record.fields.get(&Field::Command),
                        Some(&FieldValue::Kind(SafeValue::Compare))
                    );
                    for value in record.fields.values() {
                        if let FieldValue::Identity(identity) = value {
                            assert!(identity.as_str().starts_with("sha256:"), "{identity:?}");
                        }
                    }
                    if let Some(FieldValue::Identity(artifact)) =
                        record.fields.get(&Field::Artifact)
                    {
                        recorded.insert(artifact.as_str().to_string());
                    }
                })
                .unwrap();
            assert_eq!(
                recorded,
                warned
                    .iter()
                    .map(|name| SafeIdentity::fingerprint(name).as_str().to_string())
                    .collect()
            );
        }

        // The staged comparison used by `test` produces the same warnings.
        let staged = StagedSuiteComparison::new(&suite, "shop", "run", 0.0, 2).unwrap();
        staged
            .compare_job_captures(&cases.map(|case| case.0.to_string()))
            .unwrap();
        staged.finalize().unwrap();
        let staged_report: Value =
            serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
        assert_eq!(warnings_of(&staged_report), warnings_of(&generated));

        // Readers of report.json accept the optional field, and reports without
        // it (older releases, or no mismatch) keep their previous shape.
        let parsed: ReportConfig = serde_json::from_value(generated.clone()).unwrap();
        assert_eq!(
            parsed
                .tests
                .iter()
                .filter(|test| test.pair.image_simplification_warning.is_some())
                .count(),
            warned.len()
        );
        let mut legacy = generated.clone();
        for test in legacy["tests"].as_array_mut().unwrap() {
            test["pair"]
                .as_object_mut()
                .unwrap()
                .remove("imageSimplificationWarning");
        }
        let parsed: ReportConfig = serde_json::from_value(legacy.clone()).unwrap();
        assert!(
            parsed
                .tests
                .iter()
                .all(|test| test.pair.image_simplification_warning.is_none())
        );
        assert!(
            !serde_json::to_string(&parsed)
                .unwrap()
                .contains("imageSimplificationWarning")
        );
        #[cfg(feature = "desktop")]
        for test in generated["tests"].as_array().unwrap() {
            let result: crate::desktop::results::TestResult =
                serde_json::from_value(test.clone()).unwrap();
            assert!(!result.copy_summary().is_empty());
        }

        // Approval promotes each test capture's setting with its image.
        approve_screenshots(&suite, Some("run")).unwrap();
        generate_comparison_report(&suite, "shop", "run", 0.0, 1).unwrap();
        let approved: Value =
            serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
        assert!(warnings_of(&approved).is_empty(), "{approved}");
        assert!(
            approved["tests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|test| test["pair"]["fileName"] != "failed__FINAL.png")
                .all(|test| test["status"] == "pass")
        );
    }

    #[test]
    fn image_simplification_warnings_are_recalculated_after_viewport_approval() {
        let directory = viewport_approval_fixture();
        let suite = directory.path().join("shop");
        let run = suite.join("bitmaps_test/reviewed");
        // Pixelated test captures against references that predate recording.
        for entry in fs::read_dir(&run).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap();
            if is_capture_screenshot(name) {
                let bytes = fs::read(&path).unwrap();
                write_capture(&path, &bytes, Some(pixelate_setting(4)));
            }
        }
        let scope = ["checkout".to_string(), "other".to_string()];
        generate_comparison_report_for_scenarios(&suite, "shop", "reviewed", 20.0, 1, &scope)
            .unwrap();
        let warned = || -> BTreeSet<String> {
            let report: ReportConfig =
                serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
            report
                .tests
                .iter()
                .filter(|test| test.pair.image_simplification_warning.is_some())
                .map(|test| test.pair.file_name.clone())
                .collect()
        };
        // The failed viewport capture is not compared, so it has no warning.
        assert_eq!(
            warned(),
            [
                "checkout_desktop__$payment.png",
                "checkout_desktop__FINAL.png",
                "checkout_mobile__FINAL.png",
                "other_desktop__FINAL.png",
            ]
            .map(String::from)
            .into()
        );
        approve_scenario_viewport(&suite, "reviewed", "checkout", "checkout_desktop__FINAL")
            .unwrap();
        // The reviewed report's old warnings are not carried over for the
        // approved viewport, whose references now record the test setting.
        assert_eq!(
            warned(),
            ["checkout_mobile__FINAL.png", "other_desktop__FINAL.png"]
                .map(String::from)
                .into()
        );
    }

    #[test]
    fn approve_screenshots_validates_all_selected_pngs_before_promoting_references() {
        let temp = tempfile::tempdir().unwrap();
        let suite_dir = temp.path().join("basic");
        let reference_dir = suite_dir.join("bitmaps_reference");
        let test_dir = suite_dir.join("bitmaps_test/run");
        fs::create_dir_all(&reference_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();

        fs::write(reference_dir.join("alpha__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(reference_dir.join("beta__FINAL.png"), png_bytes(10)).unwrap();
        fs::write(test_dir.join("alpha__FINAL.png"), png_bytes(200)).unwrap();
        fs::write(test_dir.join("beta__FINAL.png"), &png_bytes(200)[..33]).unwrap();

        let error = approve_screenshots(&suite_dir, Some("run")).unwrap_err();
        assert!(error.to_string().contains("invalid capture"));
        assert_eq!(
            fs::read(reference_dir.join("alpha__FINAL.png")).unwrap(),
            png_bytes(10)
        );
        assert_eq!(
            fs::read(reference_dir.join("beta__FINAL.png")).unwrap(),
            png_bytes(10)
        );
        assert!(select_capture_run(&suite_dir.join("bitmaps_test"), Some("../run")).is_err());
    }
}
