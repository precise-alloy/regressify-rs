use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use serde::Deserialize;

use crate::helpers::{ScenarioName, scenario_names};

use super::site::{
    Capture, CapturePreviews, Dimensions, ImageSize, ImageSizes, MAX_IMAGE_BYTES, MAX_REPORT_BYTES,
    SuiteReport, checked_file, read_bounded, store_asset, valid_segment, valid_suite_id,
};

#[derive(Clone)]
pub(super) struct ImageAsset {
    full: String,
    preview: String,
    grid: Option<String>,
    size: ImageSize,
}

fn encode_preview(preview: image::DynamicImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    preview.write_with_encoder(image::codecs::png::PngEncoder::new_with_quality(
        &mut bytes,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Sub,
    ))?;
    Ok(bytes)
}

fn preview_images(
    bytes: &[u8],
    include_grid: bool,
) -> Result<(Vec<u8>, Option<Vec<u8>>, ImageSize)> {
    let decoded = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png)
        .decode()
        .context("Failed to decode PNG capture")?;
    let (width, height) = (decoded.width(), decoded.height());
    let preview = encode_preview(decoded.thumbnail(width.min(640), height.min(480)))?;
    let grid = include_grid
        .then(|| {
            let crop_width = width.min(height.saturating_mul(8) / 5).max(1);
            let crop_height = height.min(width.saturating_mul(5) / 8).max(1);
            encode_preview(
                decoded
                    .crop_imm((width - crop_width) / 2, 0, crop_width, crop_height)
                    .thumbnail(crop_width.min(640), crop_height.min(400)),
            )
        })
        .transpose()?;
    Ok((preview, grid, ImageSize { width, height }))
}

#[derive(Deserialize)]
struct RawReport {
    id: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    tests: Vec<RawTest>,
}

#[derive(Deserialize)]
struct RawTest {
    status: String,
    pair: RawPair,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPair {
    #[serde(default)]
    label: String,
    #[serde(default)]
    scenario_id: String,
    #[serde(default)]
    scenario_label: String,
    scenario_display_name: Option<String>,
    #[serde(default)]
    selector: String,
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    mis_match_threshold: f64,
    reference: Option<String>,
    test: Option<String>,
    reference_log: Option<crate::test::capture::CaptureLog>,
    test_log: Option<crate::test::capture::CaptureLog>,
    diff_image: Option<String>,
    reference_url: Option<String>,
    url: Option<String>,
    engine_error_msg: Option<String>,
    diff: Option<RawDiff>,
}

impl RawPair {
    fn scenario_name(&self) -> ScenarioName<'_> {
        let filename = if self.file_name.is_empty() {
            self.test
                .as_deref()
                .or(self.reference.as_deref())
                .unwrap_or(&self.label)
                .split('?')
                .next()
                .unwrap_or_default()
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or_default()
        } else {
            &self.file_name
        };
        ScenarioName::captured(
            &self.scenario_id,
            self.scenario_display_name.as_deref(),
            Some(&self.scenario_label),
            self.reference_url.as_deref(),
            self.url.as_deref(),
        )
        .with_capture_label(
            &self.label,
            filename.strip_suffix(".png").unwrap_or(filename),
        )
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDiff {
    raw_mis_match_percentage: Option<f64>,
    mis_match_percentage: Option<String>,
    mismatch_pixels: Option<u64>,
    is_same_dimensions: bool,
    dimension_difference: RawDimensions,
    alignment: Option<crate::compare::image_compare::DiffAlignment>,
}

#[derive(Deserialize)]
struct RawDimensions {
    width: i64,
    height: i64,
}

fn normalize_image(path: &str) -> String {
    path.split_once('?')
        .map(|(path, _)| path)
        .unwrap_or(path)
        .replace('\\', "/")
}

fn capture_paths(report: &RawReport) -> Result<Vec<String>> {
    report
        .tests
        .iter()
        .map(|test| {
            let path = normalize_image(
                test.pair
                    .test
                    .as_deref()
                    .context("Snapshot capture has no test image path")?,
            );
            let rest = path
                .strip_prefix("../bitmaps_test/")
                .context("Snapshot test image must be inside bitmaps_test")?;
            let components: Vec<_> = rest.split('/').collect();
            ensure!(
                components.len() == 2 && components.iter().all(|part| valid_segment(part)),
                "Invalid snapshot capture run path"
            );
            Ok(path)
        })
        .collect()
}

struct PendingImage<'a> {
    source: PathBuf,
    suite: &'a str,
    role: &'static str,
    name: String,
    include_grid: bool,
}

fn resolve_image_source<'a>(
    root: &Path,
    suite: &'a str,
    run_id: &str,
    path: Option<&str>,
    role: &'static str,
    grid_paths: &BTreeSet<String>,
) -> Result<Option<PendingImage<'a>>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let normalized = normalize_image(path);
    let reference = role == "reference";
    let prefix = if reference {
        "../bitmaps_reference/"
    } else {
        "../bitmaps_test/"
    };
    let relative = normalized
        .strip_prefix(prefix)
        .context("Snapshot image is outside its capture folder")?;
    ensure!(
        !relative.is_empty()
            && relative.split('/').all(valid_segment)
            && relative.to_ascii_lowercase().ends_with(".png"),
        "Invalid snapshot image path"
    );
    let name = if reference {
        ensure!(!relative.contains('/'), "Invalid snapshot image path");
        relative.to_owned()
    } else {
        let (image_run, name) = relative
            .split_once('/')
            .context("Snapshot test image has no capture run")?;
        ensure!(
            image_run == run_id && valid_segment(name),
            "Invalid snapshot image path"
        );
        name.to_owned()
    };
    let folder = if reference {
        "bitmaps_reference"
    } else {
        "bitmaps_test"
    };
    let Some(source) = checked_file(
        root,
        &Path::new(suite).join(folder).join(relative),
        reference,
    )?
    else {
        return Ok(None);
    };
    let include_grid = grid_paths.contains(&normalized);
    let role = if include_grid { "test" } else { role };
    Ok(Some(PendingImage {
        source,
        suite,
        role,
        name,
        include_grid,
    }))
}

type DecodedImage = (Vec<u8>, Vec<u8>, Option<Vec<u8>>, ImageSize);

fn decode_pending_image(pending: &PendingImage<'_>) -> Result<DecodedImage> {
    let bytes = read_bounded(&pending.source, MAX_IMAGE_BYTES)?;
    ensure!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "Snapshot capture is not a PNG image: {}",
        pending.source.display()
    );
    let (preview, grid, size) =
        preview_images(&bytes, pending.include_grid).with_context(|| {
            format!(
                "Failed to create snapshot previews for '{}'",
                pending.source.display()
            )
        })?;
    Ok((bytes, preview, grid, size))
}

fn store_pending_image(
    output: &Path,
    pending: &PendingImage<'_>,
    encoded: (Vec<u8>, Vec<u8>, Option<Vec<u8>>, ImageSize),
) -> Result<ImageAsset> {
    let (bytes, preview, grid, size) = encoded;
    let PendingImage {
        suite, role, name, ..
    } = pending;
    Ok(ImageAsset {
        size,
        full: store_asset(
            output,
            &format!("_snapshot/images/{suite}/{role}/{name}"),
            &bytes,
        )?,
        preview: store_asset(
            output,
            &format!("_snapshot/images/{suite}/{role}-preview/{name}"),
            &preview,
        )?,
        grid: grid
            .map(|bytes| {
                store_asset(
                    output,
                    &format!("_snapshot/images/{suite}/{role}-thumbnail/{name}"),
                    &bytes,
                )
            })
            .transpose()?,
    })
}

pub(super) fn load_suite(
    root: &Path,
    output: &Path,
    id: &str,
    images: &mut BTreeMap<PathBuf, ImageAsset>,
    logs: &mut super::logs::LogCache,
) -> Result<Option<SuiteReport>> {
    ensure!(valid_suite_id(id), "Invalid snapshot suite ID");
    let suite_dir = root.join(id);
    if !suite_dir.try_exists()? {
        return Ok(None);
    }
    let _artifacts = crate::run_history::ArtifactLease::read(&suite_dir)?;
    let tracked = crate::run_history::is_tracked(&suite_dir)?;
    let selected_run = if tracked {
        let Some(run) =
            crate::run_history::latest_completed_capture_run(&suite_dir.join("bitmaps_test"))?
        else {
            return Ok(None);
        };
        crate::compare::report_gen::ensure_complete_capture_report(&suite_dir, id, &run)?;
        Some(run)
    } else {
        if !suite_dir.join("html_report/config.js").try_exists()?
            && suite_dir.join("bitmaps_test").try_exists()?
            && let Some(run) =
                crate::run_history::latest_completed_capture_run(&suite_dir.join("bitmaps_test"))?
        {
            crate::compare::report_gen::ensure_capture_report(&suite_dir, id, &run)?;
        }
        None
    };
    let config_report: RawReport = if let Some(run) = &selected_run {
        let path = checked_file(
            root,
            &Path::new(id)
                .join("bitmaps_test")
                .join(run)
                .join("report.json"),
            false,
        )?
        .context("Capture run report is missing")?;
        serde_json::from_slice(&read_bounded(&path, MAX_REPORT_BYTES)?)
            .context("Snapshot capture run has invalid report.json")?
    } else {
        let Some(config) = checked_file(root, &Path::new(id).join("html_report/config.js"), true)?
        else {
            return Ok(None);
        };
        let config_text = String::from_utf8(read_bounded(&config, MAX_REPORT_BYTES)?)?;
        let json = config_text
            .trim()
            .strip_prefix("report(")
            .and_then(|value| value.strip_suffix(");"))
            .context("Invalid snapshot config.js wrapper; expected report(<JSON>);")?;
        serde_json::from_str(json).context("Invalid snapshot config.js report data")?
    };
    ensure!(
        config_report.id == id,
        "Snapshot config suite ID does not match its output directory"
    );
    let paths = capture_paths(&config_report)?;
    let runs: BTreeSet<_> = paths
        .iter()
        .filter_map(|path| {
            path.strip_prefix("../bitmaps_test/")
                .and_then(|path| path.split('/').next())
        })
        .collect();
    ensure!(
        runs.len() <= 1,
        "Snapshot config references multiple capture runs"
    );
    ensure!(
        selected_run
            .as_ref()
            .is_none_or(|selected| runs.iter().all(|run| *run == selected)),
        "Snapshot report references a different capture run"
    );
    let run_id =
        selected_run.unwrap_or_else(|| runs.first().copied().unwrap_or_default().to_owned());
    let report = if run_id.is_empty() || tracked {
        config_report
    } else {
        let path = checked_file(
            root,
            &Path::new(id)
                .join("bitmaps_test")
                .join(&run_id)
                .join("report.json"),
            false,
        )?
        .context("Capture run report is missing")?;
        let report: RawReport = serde_json::from_slice(&read_bounded(&path, MAX_REPORT_BYTES)?)
            .context("Snapshot capture run has invalid report.json")?;
        ensure!(
            report.id == id && capture_paths(&report)? == paths,
            "Snapshot config and capture run report disagree"
        );
        report
    };
    let grid_paths = paths.into_iter().collect();
    let mut tests = Vec::with_capacity(report.tests.len());
    let mut identifiers = BTreeSet::new();
    let names = scenario_names(
        &report
            .tests
            .iter()
            .map(|test| test.pair.scenario_name())
            .collect::<Vec<_>>(),
    );
    let mut prepared_tests = Vec::with_capacity(report.tests.len());
    let mut pending_images = Vec::new();
    let mut queued_sources = BTreeSet::new();
    let mut role_names = BTreeMap::new();
    for (test, scenario) in report.tests.into_iter().zip(names) {
        let pair = test.pair;
        ensure!(
            matches!(test.status.as_str(), "pass" | "fail")
                && (pair.engine_error_msg.is_none() || test.status == "fail"),
            "Invalid snapshot comparison status"
        );
        ensure!(
            (0.0..=100.0).contains(&pair.mis_match_threshold),
            "Invalid snapshot comparison threshold"
        );
        if let Some(alignment) = pair.diff.as_ref().and_then(|diff| diff.alignment.as_ref()) {
            ensure!(alignment.is_valid(), "Invalid snapshot diff alignment");
        }
        let capture_path = normalize_image(
            pair.test
                .as_deref()
                .context("Snapshot capture has no test image")?,
        );
        let capture_id = if pair.file_name.is_empty() {
            capture_path
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned()
        } else {
            pair.file_name.clone()
        };
        ensure!(
            valid_segment(&capture_id) && identifiers.insert(capture_id.to_ascii_lowercase()),
            "Invalid or duplicate snapshot capture ID"
        );
        let mismatch = pair
            .diff
            .as_ref()
            .map(|diff| {
                let value = diff.raw_mis_match_percentage.map(Ok).unwrap_or_else(|| {
                    diff.mis_match_percentage
                        .as_deref()
                        .context("Comparison diff has no mismatch percentage")?
                        .parse::<f64>()
                        .context("Invalid mismatch percentage")
                })?;
                ensure!(
                    (0.0..=100.0).contains(&value),
                    "Invalid snapshot mismatch percentage"
                );
                Ok::<_, anyhow::Error>(value)
            })
            .transpose()?;
        ensure!(
            pair.diff
                .as_ref()
                .and_then(|diff| diff.mismatch_pixels)
                .is_none_or(|pixels| pixels <= crate::helpers::png::MAX_VISUAL_IMAGE_PIXELS),
            "Invalid snapshot mismatch pixel count"
        );
        let reference = resolve_image_source(
            root,
            id,
            &run_id,
            pair.reference.as_deref(),
            "reference",
            &grid_paths,
        )?;
        let current =
            resolve_image_source(root, id, &run_id, pair.test.as_deref(), "test", &grid_paths)?;
        let diff = resolve_image_source(
            root,
            id,
            &run_id,
            pair.diff_image.as_deref(),
            "diff",
            &grid_paths,
        )?;
        for pending in [&reference, &current, &diff].into_iter().flatten() {
            let previous_name = role_names
                .entry((pending.role, pending.name.to_ascii_lowercase()))
                .or_insert_with(|| pending.name.clone());
            ensure!(
                *previous_name == pending.name,
                "Conflicting case for snapshot image path"
            );
            if !images.contains_key(&pending.source)
                && queued_sources.insert(pending.source.clone())
            {
                pending_images.push(PendingImage {
                    source: pending.source.clone(),
                    suite: pending.suite,
                    role: pending.role,
                    name: pending.name.clone(),
                    include_grid: pending.include_grid,
                });
            }
        }
        prepared_tests.push((
            test.status,
            pair,
            scenario,
            capture_id,
            mismatch,
            reference.map(|item| item.source),
            current.map(|item| item.source),
            diff.map(|item| item.source),
        ));
    }
    let total_images = pending_images.len();
    if total_images > 0 {
        super::site::snapshot_phase(&format!(
            "Resizing and staging {total_images} capture preview {} for suite '{id}' ({} {})",
            if total_images == 1 { "image" } else { "images" },
            prepared_tests.len(),
            if prepared_tests.len() == 1 {
                "capture"
            } else {
                "captures"
            }
        ));
    }
    let diagnostic_context = crate::diagnostics::runtime::current();
    let completed = std::sync::atomic::AtomicUsize::new(0);
    let last_update = std::sync::Mutex::new(std::time::Instant::now());
    let process_pending = |pending: &PendingImage<'_>| -> Result<ImageAsset> {
        let asset = store_pending_image(output, pending, decode_pending_image(pending)?)?;
        let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let should_log = done == 1
            || done == total_images
            || done.is_multiple_of(10)
            || last_update.lock().ok().is_some_and(|mut last| {
                if last.elapsed() >= std::time::Duration::from_secs(1) {
                    *last = std::time::Instant::now();
                    true
                } else {
                    false
                }
            });
        if should_log {
            if let Ok(mut last) = last_update.lock() {
                *last = std::time::Instant::now();
            }
            crate::diagnostics::runtime::in_scope(diagnostic_context.clone(), || {
                super::site::snapshot_phase(&format!(
                    "Resized {done} of {total_images} capture preview images for suite '{id}' ({}/{})",
                    pending.role, pending.name
                ));
            });
        }
        Ok(asset)
    };
    let stored_images = if pending_images.len() <= 1 {
        pending_images
            .iter()
            .map(process_pending)
            .collect::<Result<Vec<_>>>()?
    } else {
        pending_images
            .par_iter()
            .map(process_pending)
            .collect::<Result<Vec<_>>>()?
    };
    for (pending, asset) in pending_images.into_iter().zip(stored_images) {
        images.insert(pending.source, asset);
    }
    if prepared_tests
        .iter()
        .any(|(_, pair, ..)| pair.reference_log.is_some() || pair.test_log.is_some())
    {
        super::site::snapshot_phase(&format!(
            "Publishing capture diagnostic logs for suite '{id}'"
        ));
    }
    for (status, pair, scenario, capture_id, mismatch, reference_src, current_src, diff_src) in
        prepared_tests
    {
        let reference = reference_src
            .as_ref()
            .and_then(|source| images.get(source))
            .cloned();
        let current = current_src
            .as_ref()
            .and_then(|source| images.get(source))
            .cloned();
        let diff = diff_src
            .as_ref()
            .and_then(|source| images.get(source))
            .cloned();
        let key = capture_id.strip_suffix(".png").unwrap_or(&capture_id);
        let label = if crate::helpers::generated_capture_label(&pair.label, key) {
            let (_, detail) =
                crate::helpers::capture_details(key, Some(&pair.scenario_id), &pair.selector);
            [scenario.clone(), detail]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" / ")
        } else {
            pair.label
        };
        // Legacy reports also put comparison diagnostics in engineErrorMsg.
        let (error, comparison_error) = match pair.engine_error_msg {
            Some(message)
                if mismatch.is_some()
                    || message == "Reference image not found"
                    || message.starts_with("Comparison error: ") =>
            {
                (None, Some(message))
            }
            error => (error, None),
        };
        tests.push(Capture {
            logs: if pair.reference_log.is_some() || pair.test_log.is_some() {
                Some(super::site::CaptureLogs {
                    reference: super::logs::publish(root, output, pair.reference_log, logs)?,
                    test: super::logs::publish(root, output, pair.test_log, logs)?,
                })
            } else {
                None
            },
            id: capture_id,
            label,
            scenario,
            selector: pair.selector,
            status,
            threshold: pair.mis_match_threshold,
            mismatch,
            mismatch_pixels: pair.diff.as_ref().and_then(|diff| diff.mismatch_pixels),
            error,
            comparison_error,
            reference: reference.as_ref().map(|asset| asset.full.clone()),
            test: current.as_ref().map(|asset| asset.full.clone()),
            diff: diff.as_ref().map(|asset| asset.full.clone()),
            alignment: pair.diff.as_ref().and_then(|diff| diff.alignment.clone()),
            image_sizes: Some(ImageSizes {
                reference: reference.as_ref().map(|asset| asset.size),
                test: current.as_ref().map(|asset| asset.size),
                diff: diff.as_ref().map(|asset| asset.size),
            }),
            previews: Some(CapturePreviews {
                reference: reference.as_ref().map(|asset| asset.preview.clone()),
                test: current.as_ref().map(|asset| asset.preview.clone()),
                diff: diff.as_ref().map(|asset| asset.preview.clone()),
                grid: current.as_ref().and_then(|asset| asset.grid.clone()),
            }),
            reference_url: pair.reference_url,
            test_url: pair.url,
            dimensions: pair.diff.map(|diff| Dimensions {
                same: diff.is_same_dimensions,
                width: diff.dimension_difference.width,
                height: diff.dimension_difference.height,
            }),
        });
    }
    Ok(Some(SuiteReport {
        id: id.into(),
        display_name: crate::compare::report_gen::read_suite_metadata(&root.join(id))?
            .map(|metadata| metadata.display_name)
            .unwrap_or(report.display_name)
            .filter(|name| !name.trim().is_empty()),
        run_id,
        tests,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn png(value: u8) -> Vec<u8> {
        lodepng::encode32(&[value, value, value, 255], 1, 1).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, serde_json::Value) {
        let temp = tempfile::tempdir().unwrap();
        let suite = temp.path().join("suite");
        for path in [
            "html_report",
            "bitmaps_test/current",
            "bitmaps_test/prior",
            "bitmaps_reference",
        ] {
            fs::create_dir_all(suite.join(path)).unwrap();
        }
        for path in [
            "bitmaps_test/current/home.png",
            "bitmaps_reference/home.png",
        ] {
            fs::write(suite.join(path), png(80)).unwrap();
        }
        let report = json!({ "id": "suite", "tests": [{ "status": "fail", "pair": {
            "label": "Home <desktop>", "scenarioLabel": "Home", "selector": "document", "fileName": "home.png",
            "reference": "../bitmaps_reference/home.png", "test": "../bitmaps_test/current/home.png", "misMatchThreshold": 0.1,
            "referenceUrl": "https://example.com/ref", "url": "https://example.com/test",
            "diff": { "isSameDimensions": false, "dimensionDifference": { "width": -20, "height": 40 }, "rawMisMatchPercentage": 1.25 }
        }}] });
        write_fixture(temp.path(), &report);
        (temp, report)
    }

    fn write_fixture(root: &Path, report: &serde_json::Value) {
        fs::write(
            root.join("suite/html_report/config.js"),
            format!("report({report});"),
        )
        .unwrap();
        fs::write(
            root.join("suite/bitmaps_test/current/report.json"),
            report.to_string(),
        )
        .unwrap();
    }

    fn load(root: &Path) -> Result<Option<SuiteReport>> {
        load_suite(
            &root.canonicalize()?,
            &root.canonicalize()?,
            "suite",
            &mut BTreeMap::new(),
            &mut super::super::logs::LogCache::new(),
        )
    }

    #[test]
    fn mismatch_pixels_preserve_exact_counts_and_legacy_absence() {
        for pixels in [None, Some(0), Some(1)] {
            let (temp, mut report) = fixture();
            let percentage = if pixels == Some(0) {
                0.0
            } else {
                0.000017922688690066528_f64
            };
            report["tests"][0]["pair"]["diff"]["rawMisMatchPercentage"] = json!(percentage);
            report["tests"][0]["pair"]["diff"]["misMatchPercentage"] = json!("0.00");
            if let Some(pixels) = pixels {
                report["tests"][0]["pair"]["diff"]["mismatchPixels"] = json!(pixels);
            }
            write_fixture(temp.path(), &report);
            let suite = load(temp.path()).unwrap().unwrap();
            assert_eq!(suite.tests[0].mismatch_pixels, pixels);
            assert_eq!(
                suite.tests[0].mismatch.unwrap().to_bits(),
                percentage.to_bits()
            );
            let decoded: SuiteReport =
                serde_json::from_str(&serde_json::to_string(&suite).unwrap()).unwrap();
            assert_eq!(
                decoded.tests[0].mismatch.unwrap().to_bits(),
                percentage.to_bits()
            );
            let serialized = serde_json::to_value(&suite).unwrap();
            assert!(serialized["tests"][0]["mismatch"].is_number());
            assert_eq!(
                serialized["tests"][0]
                    .get("mismatchPixels")
                    .and_then(|value| value.as_u64()),
                pixels
            );
        }
        for invalid in [json!(-1), json!(1.5), json!("1"), json!(64_000_001)] {
            let (temp, mut report) = fixture();
            report["tests"][0]["pair"]["diff"]["mismatchPixels"] = invalid;
            write_fixture(temp.path(), &report);
            assert!(load(temp.path()).is_err());
        }
    }

    #[test]
    fn completed_run_snapshot_compares_on_demand_and_ignores_partial_or_active_runs() {
        use crate::run_history::{RunLease, RunState};
        let temp = tempfile::tempdir().unwrap();
        let suite = temp.path().join("suite");
        let mut completed = RunLease::begin(&suite, "completed", true).unwrap();
        fs::write(suite.join("bitmaps_test/completed/home.png"), png(80)).unwrap();
        completed.finish(RunState::Completed, false).unwrap();
        drop(completed);
        assert!(!suite.join("bitmaps_test/completed/report.json").exists());
        assert_eq!(load(temp.path()).unwrap().unwrap().run_id, "completed");
        assert!(suite.join("bitmaps_test/completed/report.json").is_file());

        let mut partial = RunLease::begin(&suite, "partial", false).unwrap();
        fs::write(suite.join("bitmaps_test/partial/home.png"), png(100)).unwrap();
        crate::compare::report_gen::generate_comparison_report(&suite, "suite", "partial", 0.1, 1)
            .unwrap();
        partial.finish(RunState::Completed, true).unwrap();
        drop(partial);
        let active = RunLease::begin(&suite, "active", true).unwrap();
        assert_eq!(load(temp.path()).unwrap().unwrap().run_id, "completed");
        drop(active);
        assert_eq!(load(temp.path()).unwrap().unwrap().run_id, "completed");
    }

    #[test]
    fn image_simplification_warnings_in_reports_leave_snapshot_data_unchanged() {
        let (temp, report) = fixture();
        let baseline = serde_json::to_value(load(temp.path()).unwrap().unwrap()).unwrap();
        // Newer reports may carry this optional comparison warning; the
        // version-2 snapshot data does not publish it yet.
        let mut warned = report;
        warned["tests"][0]["pair"]["imageSimplificationWarning"] = json!({
            "message": "Reference and test captures used different image simplification settings",
            "reference": null,
            "test": { "mode": "pixelate", "blockSize": 4, "minSize": 64,
                      "excludeSelectorsDigest": "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" },
        });
        write_fixture(temp.path(), &warned);
        assert_eq!(
            serde_json::to_value(load(temp.path()).unwrap().unwrap()).unwrap(),
            baseline
        );
    }

    #[test]
    fn visual_mismatch_is_not_an_engine_error() {
        for mismatch in [0.0, 1.25] {
            let (temp, mut report) = fixture();
            let message = "Dimensions differ: reference 100x100 vs test 80x140";
            report["tests"][0]["pair"]["engineErrorMsg"] = json!(message);
            report["tests"][0]["pair"]["diff"]["rawMisMatchPercentage"] = json!(mismatch);
            write_fixture(temp.path(), &report);

            let suite = load(temp.path()).unwrap().unwrap();
            let capture = &suite.tests[0];
            assert_eq!(capture.status, "fail");
            assert_eq!(capture.mismatch, Some(mismatch));
            assert!(capture.reference.is_some());
            assert!(capture.test.is_some());
            assert!(!capture.dimensions.as_ref().unwrap().same);
            assert!(capture.error.is_none());
            assert_eq!(capture.comparison_error.as_deref(), Some(message));
        }
    }

    #[test]
    fn comparison_failures_without_a_diff_are_not_engine_errors() {
        for message in [
            "Reference image not found",
            "Comparison error: unable to compare images",
        ] {
            let (temp, mut report) = fixture();
            report["tests"][0]["pair"]["diff"] = serde_json::Value::Null;
            report["tests"][0]["pair"]["engineErrorMsg"] = json!(message);
            if message == "Reference image not found" {
                fs::remove_file(temp.path().join("suite/bitmaps_reference/home.png")).unwrap();
            }
            write_fixture(temp.path(), &report);

            let suite = load(temp.path()).unwrap().unwrap();
            let capture = &suite.tests[0];
            assert_eq!(capture.status, "fail");
            assert!(capture.mismatch.is_none());
            assert!(capture.test.is_some());
            assert!(capture.error.is_none());
            assert_eq!(capture.comparison_error.as_deref(), Some(message));
        }
    }

    #[test]
    fn failed_captures_remain_engine_errors_even_with_placeholder_images() {
        for message in [
            "Test scenario failed to capture: click timed out",
            "Reference scenario failed to capture: navigation failed",
            "Both reference and test scenarios failed to capture (reference: Timeout; test: Timeout)",
            "Navigation timed out after 30 seconds",
            "Test scenario failed to capture: Reference image not found",
            "Test scenario failed to capture: Comparison error: action failed",
        ] {
            let (temp, mut report) = fixture();
            report["tests"][0]["pair"]["diff"] = serde_json::Value::Null;
            report["tests"][0]["pair"]["engineErrorMsg"] = json!(message);
            write_fixture(temp.path(), &report);

            let suite = load(temp.path()).unwrap().unwrap();
            let capture = &suite.tests[0];
            assert_eq!(capture.status, "fail");
            assert!(capture.mismatch.is_none());
            assert!(capture.test.is_some());
            assert_eq!(capture.error.as_deref(), Some(message));
            assert!(capture.comparison_error.is_none());
        }
    }

    #[test]
    fn snapshot_counts_only_capture_failures_as_errors() {
        let (temp, mut report) = fixture();
        let template = report["tests"][0].clone();
        report["tests"] = json!([template.clone(), template.clone(), template]);
        for (index, test) in report["tests"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            let filename = format!("capture-{index}.png");
            test["pair"]["fileName"] = json!(filename);
            test["pair"]["test"] = json!(format!("../bitmaps_test/current/{filename}"));
            fs::write(
                temp.path()
                    .join("suite/bitmaps_test/current")
                    .join(filename),
                png(80),
            )
            .unwrap();
        }
        report["tests"][0]["pair"]["engineErrorMsg"] =
            json!("Dimensions differ: reference 100x100 vs test 80x140");
        report["tests"][1]["pair"]["diff"] = serde_json::Value::Null;
        report["tests"][1]["pair"]["reference"] = json!("../bitmaps_reference/missing.png");
        report["tests"][1]["pair"]["engineErrorMsg"] = json!("Reference image not found");
        report["tests"][2]["pair"]["diff"] = serde_json::Value::Null;
        report["tests"][2]["pair"]["engineErrorMsg"] =
            json!("Test scenario failed to capture: navigation timed out");
        write_fixture(temp.path(), &report);

        super::super::site::generate_site(
            temp.path(),
            &["suite".into()],
            &crate::cli::args::SnapshotArgs {
                test_suite: Vec::new(),
                title: None,
                heading: None,
                subheading: None,
            },
            &crate::config::WorkspaceConfig::default(),
        )
        .unwrap();
        for filename in ["manifest.json", "../status.json"] {
            let published: serde_json::Value = serde_json::from_slice(
                &fs::read(temp.path().join("_snapshot/data").join(filename)).unwrap(),
            )
            .unwrap();
            let summary = if filename == "manifest.json" {
                &published["suites"][0]
            } else {
                &published
            };
            assert_eq!(summary["total"], 3);
            assert_eq!(summary["failed"], 3);
            assert_eq!(summary["errors"], 1);
        }
    }

    #[test]
    fn shared_diff_and_test_source_keeps_its_grid_thumbnail() {
        let (temp, mut report) = fixture();
        let mut second = report["tests"][0].clone();
        second["pair"]["fileName"] = json!("shared.png");
        second["pair"]["test"] = json!("../bitmaps_test/current/shared.png");
        report["tests"][0]["pair"]["diffImage"] = second["pair"]["test"].clone();
        report["tests"].as_array_mut().unwrap().push(second);
        fs::write(
            temp.path().join("suite/bitmaps_test/current/shared.png"),
            png(120),
        )
        .unwrap();
        write_fixture(temp.path(), &report);
        let loaded = load(temp.path()).unwrap().unwrap();
        assert_eq!(loaded.tests[0].diff, loaded.tests[1].test);
        assert!(
            loaded.tests[1]
                .test
                .as_deref()
                .unwrap()
                .starts_with("_snapshot/images/suite/test/shared.png?v=")
        );
        assert!(
            loaded.tests[1]
                .previews
                .as_ref()
                .unwrap()
                .grid
                .as_deref()
                .unwrap()
                .starts_with("_snapshot/images/suite/test-thumbnail/shared.png?v=")
        );

        report["tests"][1]["pair"]["fileName"] = json!("HOME.png");
        write_fixture(temp.path(), &report);
        assert!(load(temp.path()).is_err());
    }

    #[test]
    fn snapshot_preserves_diff_alignment_and_legacy_absence() {
        use crate::compare::image_compare::{DiffAlignment, VerticalGap};

        let (temp, mut report) = fixture();
        assert!(
            load(temp.path()).unwrap().unwrap().tests[0]
                .alignment
                .is_none()
        );
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
            report["tests"][0]["pair"]["diff"]["alignment"] =
                serde_json::to_value(&alignment).unwrap();
            write_fixture(temp.path(), &report);
            let loaded = load(temp.path()).unwrap().unwrap();
            assert_eq!(loaded.tests[0].alignment, Some(alignment.clone()));
            assert_eq!(
                serde_json::to_value(&loaded.tests[0]).unwrap()["alignment"],
                serde_json::to_value(&alignment).unwrap()
            );
        }
        for alignment in [
            json!({"reference": {"start": 24, "height": 0}, "test": null}),
            json!({"reference": {"start": -1, "height": 5}, "test": null}),
            json!({"reference": {"start": 24.5, "height": 5}, "test": null}),
            json!({"reference": {"start": 4294967296_u64, "height": 5}, "test": null}),
            json!({"reference": {"start": 24, "height": 5}, "test": {"start": 24, "height": 5}}),
            json!({"reference": {"start": 24, "height": 5}, "test": null, "referenceGaps": [{"start": 24, "height": 5}, {"start": 24, "height": 5}]}),
            json!({"reference": {"start": 24, "height": 5}, "test": {"start": 24, "height": 5}, "referenceGaps": [{"start": 24, "height": 5}], "testGaps": [{"start": 24, "height": 5}]}),
        ] {
            report["tests"][0]["pair"]["diff"]["alignment"] = alignment;
            write_fixture(temp.path(), &report);
            assert!(load(temp.path()).is_err());
        }
    }

    #[test]
    fn source_image_sizes_preserve_unequal_images_missing_references_and_legacy_reports() {
        let (temp, mut report) = fixture();
        for (path, width, height) in [
            ("bitmaps_reference/home.png", 32, 24),
            ("bitmaps_test/current/home.png", 24, 48),
            ("bitmaps_test/current/home_diff.png", 32, 48),
        ] {
            let pixels = vec![rgb::RGBA8::new(30, 60, 90, 255); width * height];
            fs::write(
                temp.path().join("suite").join(path),
                lodepng::encode32(&pixels, width, height).unwrap(),
            )
            .unwrap();
        }
        report["tests"][0]["pair"]["diffImage"] = json!("../bitmaps_test/current/home_diff.png");
        write_fixture(temp.path(), &report);
        let loaded = load(temp.path()).unwrap().unwrap();
        let mut capture = serde_json::to_value(&loaded.tests[0]).unwrap();
        assert_eq!(
            capture["imageSizes"],
            json!({
                "reference": { "width": 32, "height": 24 },
                "test": { "width": 24, "height": 48 },
                "diff": { "width": 32, "height": 48 }
            })
        );
        capture.as_object_mut().unwrap().remove("imageSizes");
        assert!(
            serde_json::from_value::<Capture>(capture)
                .unwrap()
                .image_sizes
                .is_none()
        );
        fs::remove_file(temp.path().join("suite/bitmaps_reference/home.png")).unwrap();
        let missing = load(temp.path()).unwrap().unwrap();
        assert!(missing.tests[0].reference.is_none());
        let sizes = missing.tests[0].image_sizes.as_ref().unwrap();
        assert!(sizes.reference.is_none());
        assert_eq!(
            sizes.test,
            Some(ImageSize {
                width: 24,
                height: 48
            })
        );
    }

    #[test]
    fn preview_images_are_byte_identical_for_repeated_inputs() {
        let mut pixels = vec![0u8; 800 * 1600 * 4];
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            pixel.copy_from_slice(&[
                (index % 251) as u8,
                (index / 800 % 251) as u8,
                (index / 13 % 251) as u8,
                255,
            ]);
        }
        let source = lodepng::encode32(&pixels, 800, 1600).unwrap();
        let expected = preview_images(&source, true).unwrap();
        for _ in 0..3 {
            assert_eq!(preview_images(&source, true).unwrap(), expected);
        }
        let (list_only, grid, size) = preview_images(&source, false).unwrap();
        assert_eq!(list_only, expected.0);
        assert!(grid.is_none());
        assert_eq!(
            size,
            ImageSize {
                width: 800,
                height: 1600
            }
        );
    }

    #[test]
    fn preview_images_preserve_alpha_and_bound_extreme_aspect_ratios() {
        for (width, height) in [(1, 1), (2048, 4), (4, 2048), (800, 800)] {
            let pixels = vec![rgb::RGBA8::new(30, 60, 90, 64); width * height];
            let source = lodepng::encode32(&pixels, width, height).unwrap();
            let (preview, grid, size) = preview_images(&source, true).unwrap();
            assert_eq!((size.width as usize, size.height as usize), (width, height));
            for bytes in [preview, grid.unwrap()] {
                let image = lodepng::decode32(&bytes).unwrap();
                assert!(image.width <= width.min(640));
                assert!(image.height <= height.min(480));
                assert_eq!(image.buffer[0].a, 64);
            }
        }
        assert!(preview_images(b"\x89PNG\r\n\x1a\ncorrupt", true).is_err());
    }

    #[test]
    fn corrupt_preview_input_preserves_the_previous_published_site() {
        let (temp, _) = fixture();
        let root = temp.path();
        let args = crate::cli::args::SnapshotArgs {
            test_suite: Vec::new(),
            title: None,
            heading: None,
            subheading: None,
        };
        let config = crate::config::WorkspaceConfig::default();
        super::super::site::generate_site(root, &["suite".into()], &args, &config).unwrap();
        let inventory = super::super::site::artifact_paths(root, &["suite".into()]).unwrap();
        let previous: Vec<_> = inventory
            .iter()
            .map(|path| fs::read(root.join(path)).unwrap())
            .collect();
        fs::write(
            root.join("suite/bitmaps_test/current/home.png"),
            b"\x89PNG\r\n\x1a\ncorrupt",
        )
        .unwrap();
        let error =
            super::super::site::generate_site(root, &["suite".into()], &args, &config).unwrap_err();
        assert!(format!("{error:#}").contains("home.png"));
        assert!(format!("{error:#}").contains("PNG"));
        for (path, bytes) in inventory.iter().zip(previous) {
            assert_eq!(fs::read(root.join(path)).unwrap(), bytes);
        }
    }

    #[test]
    fn generates_bounded_previews_without_changing_original_captures() {
        let (temp, _) = fixture();
        let root = temp.path().canonicalize().unwrap();
        let mut pixels = vec![0u8; 320 * 6000 * 4];
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            pixel.copy_from_slice(if index < 320 * 200 {
                &[240, 20, 20, 255]
            } else {
                &[20, 20, 240, 255]
            });
        }
        let original = lodepng::encode32(&pixels, 320, 6000).unwrap();
        for path in [
            "bitmaps_reference/home.png",
            "bitmaps_test/current/home.png",
        ] {
            fs::write(root.join("suite").join(path), &original).unwrap();
        }
        super::super::site::generate_site(
            &root,
            &["suite".into()],
            &crate::cli::args::SnapshotArgs {
                test_suite: Vec::new(),
                title: None,
                heading: None,
                subheading: None,
            },
            &crate::config::WorkspaceConfig::default(),
        )
        .unwrap();
        let report: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join("_snapshot/data/suites/suite.json")).unwrap(),
        )
        .unwrap();
        let capture = &report["tests"][0];
        assert_eq!(
            capture["imageSizes"],
            json!({
                "reference": { "width": 320, "height": 6000 },
                "test": { "width": 320, "height": 6000 },
                "diff": null
            })
        );
        let inventory = super::super::site::artifact_paths(&root, &["suite".into()]).unwrap();
        let read_asset = |url: &str| {
            let (path, version) = url.split_once("?v=").unwrap();
            assert!(inventory.contains(&PathBuf::from(path)));
            let bytes = fs::read(root.join(path)).unwrap();
            assert_eq!(super::super::get_asset_version(&bytes, 20), version);
            bytes
        };
        for role in ["reference", "test", "grid"] {
            let url = capture["previews"][role]
                .as_str()
                .expect("preview is missing");
            let bytes = read_asset(url);
            let preview = lodepng::decode32(&bytes).unwrap();
            assert!(preview.width <= 640 && preview.height <= 480);
            if role == "grid" {
                assert_eq!((preview.width, preview.height), (320, 200));
                assert_eq!(preview.buffer[0], rgb::RGBA8::new(240, 20, 20, 255));
                assert_eq!(preview.buffer.last().unwrap(), &preview.buffer[0]);
            } else {
                assert_eq!(preview.height, 480);
                assert!(preview.width < 30);
            }
        }
        assert!(capture["previews"]["diff"].is_null());
        for role in ["reference", "test"] {
            assert_eq!(read_asset(capture[role].as_str().unwrap()), original);
        }
        assert_eq!(
            fs::read(root.join("suite/bitmaps_test/current/home.png")).unwrap(),
            original
        );
    }

    #[test]
    fn suite_display_name_uses_metadata_with_legacy_report_fallback() {
        let (temp, mut report) = fixture();
        assert!(load(temp.path()).unwrap().unwrap().display_name.is_none());
        report["displayName"] = json!("Captured name");
        write_fixture(temp.path(), &report);
        assert_eq!(
            load(temp.path()).unwrap().unwrap().display_name.as_deref(),
            Some("Captured name")
        );
        for name in [Some("Current name"), None] {
            crate::compare::report_gen::write_suite_metadata(&temp.path().join("suite"), name)
                .unwrap();
            let suite = load(temp.path()).unwrap().unwrap();
            assert_eq!(suite.display_name.as_deref(), name);
            assert_eq!(suite.id, "suite");
            assert_eq!(suite.run_id, "current");
        }
        fs::remove_file(temp.path().join("suite/suite.json")).unwrap();
        report["displayName"] = json!("   ");
        write_fixture(temp.path(), &report);
        assert!(load(temp.path()).unwrap().unwrap().display_name.is_none());
    }

    #[test]
    fn url_only_scenarios_get_friendly_names_without_changing_capture_ids() {
        let (temp, mut report) = fixture();
        let template = report["tests"][0].clone();
        let mut filenames = Vec::new();
        let tests = [
            ("https://example.test/men/running-shoes", "desktop"),
            ("https://example.test/women/running-shoes", "desktop"),
            ("https://example.test/help-center", "desktop"),
            ("https://example.test/men/running-shoes", "phone"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (url, viewport))| {
            let scenario_id = super::super::get_asset_version(url.as_bytes(), 20);
            let filename = format!("{scenario_id}_{viewport}__FINAL.png");
            let mut test = template.clone();
            let pair = &mut test["pair"];
            pair["scenarioId"] = json!(scenario_id);
            pair["scenarioLabel"] = json!(url);
            if index == 2 {
                pair.as_object_mut().unwrap().remove("scenarioLabel");
            }
            pair["label"] = json!(filename.trim_end_matches(".png").replace('_', " "));
            pair["fileName"] = json!(filename);
            pair["referenceUrl"] = json!(url);
            pair["url"] = json!(url.replace("example.test", "staging.example.test"));
            pair["test"] = json!(format!("../bitmaps_test/current/{filename}"));
            fs::write(
                temp.path()
                    .join("suite/bitmaps_test/current")
                    .join(&filename),
                png(80),
            )
            .unwrap();
            filenames.push(filename);
            test
        })
        .collect::<Vec<_>>();
        report["tests"] = json!(tests);
        write_fixture(temp.path(), &report);
        let original =
            fs::read(temp.path().join("suite/bitmaps_test/current/report.json")).unwrap();

        let loaded = load(temp.path()).unwrap().unwrap();
        assert_eq!(
            loaded
                .tests
                .iter()
                .map(|test| test.scenario.as_str())
                .collect::<Vec<_>>(),
            [
                "Men / Running shoes",
                "Women / Running shoes",
                "Help center",
                "Men / Running shoes"
            ]
        );
        assert_eq!(
            loaded
                .tests
                .iter()
                .map(|test| test.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Men / Running shoes / Desktop / Full page",
                "Women / Running shoes / Desktop / Full page",
                "Help center / Desktop / Full page",
                "Men / Running shoes / Phone / Full page"
            ]
        );
        assert_eq!(
            loaded.tests.iter().map(|test| &test.id).collect::<Vec<_>>(),
            filenames.iter().collect::<Vec<_>>()
        );
        assert_eq!(
            fs::read(temp.path().join("suite/bitmaps_test/current/report.json")).unwrap(),
            original
        );
        #[cfg(feature = "desktop")]
        {
            let mut desktop: crate::desktop::results::Report =
                serde_json::from_value(report).unwrap();
            desktop.resolve_names(None);
            assert_eq!(
                desktop
                    .tests
                    .iter()
                    .map(|test| test.pair.display.title.as_str())
                    .collect::<Vec<_>>(),
                loaded
                    .tests
                    .iter()
                    .map(|test| test.scenario.as_str())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                desktop
                    .tests
                    .iter()
                    .map(|test| format!(
                        "{} / {}",
                        test.pair.display.title, test.pair.display.detail
                    ))
                    .collect::<Vec<_>>(),
                loaded
                    .tests
                    .iter()
                    .map(|test| test.label.clone())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn recorded_scenario_names_survive_partial_reports_and_explicit_url_labels() {
        let (temp, mut report) = fixture();
        for name in ["Men / Shoes", "https://example.test/men/shoes"] {
            report["tests"][0]["pair"]["scenarioLabel"] = json!("https://example.test/men/shoes");
            report["tests"][0]["pair"]["referenceUrl"] = json!("https://example.test/men/shoes");
            report["tests"][0]["pair"]["scenarioDisplayName"] = json!(name);
            write_fixture(temp.path(), &report);
            let loaded = load(temp.path()).unwrap().unwrap();
            assert_eq!(loaded.tests[0].scenario, name);
            assert_eq!(loaded.tests[0].id, "home.png");
            #[cfg(feature = "desktop")]
            {
                let mut desktop: crate::desktop::results::Report =
                    serde_json::from_value(report.clone()).unwrap();
                desktop.resolve_names(None);
                assert_eq!(desktop.tests[0].pair.display.title, name);
            }
        }
    }

    #[test]
    fn converts_comparison_metadata_and_keeps_sources_and_prior_runs() {
        let (temp, _) = fixture();
        let config = fs::read(temp.path().join("suite/html_report/config.js")).unwrap();
        let report = load(temp.path()).unwrap().unwrap();
        assert_eq!(report.run_id, "current");
        assert_eq!(report.tests[0].label, "Home <desktop>");
        assert_eq!(report.tests[0].dimensions.as_ref().unwrap().width, -20);
        assert_eq!(report.tests[0].mismatch, Some(1.25));
        assert_ne!(report.tests[0].reference, report.tests[0].test);
        assert_eq!(
            report.tests[0]
                .reference
                .as_ref()
                .unwrap()
                .split_once("?v=")
                .unwrap()
                .1,
            report.tests[0]
                .test
                .as_ref()
                .unwrap()
                .split_once("?v=")
                .unwrap()
                .1
        );
        assert!(
            report.tests[0]
                .test
                .as_ref()
                .unwrap()
                .starts_with("_snapshot/images/")
        );
        assert!(temp.path().join("suite/bitmaps_test/prior").is_dir());
        assert_eq!(
            fs::read(temp.path().join("suite/html_report/config.js")).unwrap(),
            config
        );
        assert_eq!(
            serde_json::to_value(load(temp.path()).unwrap().unwrap()).unwrap(),
            serde_json::to_value(report).unwrap()
        );
    }

    #[test]
    fn invalid_later_suite_preserves_the_previous_versioned_site() {
        let (temp, _) = fixture();
        let root = temp.path();
        let args = crate::cli::args::SnapshotArgs {
            test_suite: Vec::new(),
            title: None,
            heading: None,
            subheading: None,
        };
        let config = crate::config::WorkspaceConfig::default();
        super::super::site::generate_site(root, &["suite".into()], &args, &config).unwrap();
        let previous = super::super::site::artifact_paths(root, &["suite".into()])
            .unwrap()
            .into_iter()
            .map(|path| {
                let bytes = fs::read(root.join(&path)).unwrap();
                (path, bytes)
            })
            .collect::<BTreeMap<_, _>>();
        fs::write(root.join("suite/bitmaps_test/current/home.png"), png(120)).unwrap();
        fs::create_dir_all(root.join("z-bad/html_report")).unwrap();
        fs::write(root.join("z-bad/html_report/config.js"), "not a report").unwrap();
        assert!(
            super::super::site::generate_site(
                root,
                &["suite".into(), "z-bad".into()],
                &args,
                &config
            )
            .is_err()
        );
        for (path, bytes) in previous {
            assert_eq!(fs::read(root.join(path)).unwrap(), bytes);
        }
        super::super::site::artifact_paths(root, &["suite".into()]).unwrap();
    }

    #[test]
    fn regeneration_keeps_file_list_stable_and_updates_all_dependent_versions() {
        let (temp, _) = fixture();
        let root = temp.path();
        let args = crate::cli::args::SnapshotArgs {
            test_suite: Vec::new(),
            title: None,
            heading: None,
            subheading: None,
        };
        let config = crate::config::WorkspaceConfig::default();
        let generate =
            || super::super::site::generate_site(root, &["suite".into()], &args, &config).unwrap();
        let read = |path: &str| {
            serde_json::from_slice::<serde_json::Value>(&fs::read(root.join(path)).unwrap())
                .unwrap()
        };
        generate();
        let first_paths = super::super::site::artifact_paths(root, &["suite".into()]).unwrap();
        let first_index = fs::read(root.join("index.html")).unwrap();
        let first_manifest = read("_snapshot/data/manifest.json");
        let first_report = read("_snapshot/data/suites/suite.json");
        fs::write(root.join("suite/bitmaps_test/current/home.png"), png(160)).unwrap();
        fs::write(
            root.join("_snapshot/images/old-content-hash.png"),
            b"obsolete",
        )
        .unwrap();
        generate();
        assert_eq!(
            super::super::site::artifact_paths(root, &["suite".into()]).unwrap(),
            first_paths
        );
        assert!(!root.join("_snapshot/images/old-content-hash.png").exists());
        assert_ne!(fs::read(root.join("index.html")).unwrap(), first_index);
        let next_manifest = read("_snapshot/data/manifest.json");
        let next_report = read("_snapshot/data/suites/suite.json");
        assert_ne!(
            first_manifest["suites"][0]["report"],
            next_manifest["suites"][0]["report"]
        );
        assert_eq!(
            first_report["tests"][0]["reference"],
            next_report["tests"][0]["reference"]
        );
        let old_test = first_report["tests"][0]["test"]
            .as_str()
            .unwrap()
            .split_once("?v=")
            .unwrap();
        let new_test = next_report["tests"][0]["test"]
            .as_str()
            .unwrap()
            .split_once("?v=")
            .unwrap();
        assert_eq!(old_test.0, new_test.0);
        assert_ne!(old_test.1, new_test.1);
        assert_eq!(new_test.1, super::super::get_asset_version(&png(160), 20));
        assert_ne!(
            first_report["tests"][0]["previews"]["test"],
            next_report["tests"][0]["previews"]["test"]
        );
        assert_ne!(
            first_report["tests"][0]["previews"]["grid"],
            next_report["tests"][0]["previews"]["grid"]
        );
        assert!(root.join("suite/bitmaps_test/prior").exists());
        assert!(fs::read_dir(root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".snapshot-")
        }));
    }

    #[test]
    fn malformed_missing_and_multi_run_reports_are_errors_without_capture_deletion() {
        let (temp, mut report) = fixture();
        fs::write(
            temp.path().join("suite/bitmaps_test/current/report.json"),
            "not json",
        )
        .unwrap();
        assert!(
            load(temp.path())
                .unwrap_err()
                .to_string()
                .contains("invalid report.json")
        );
        fs::write(
            temp.path().join("suite/html_report/config.js"),
            "report(alert(1));",
        )
        .unwrap();
        assert!(load(temp.path()).is_err());
        report["tests"][0]["pair"]["test"] = json!("../bitmaps_test/missing/home.png");
        write_fixture(temp.path(), &report);
        assert!(load(temp.path()).is_err());
        let mut other = report["tests"][0].clone();
        other["pair"]["test"] = json!("../bitmaps_test/current/home.png");
        report["tests"].as_array_mut().unwrap().push(other);
        write_fixture(temp.path(), &report);
        assert!(
            load(temp.path())
                .unwrap_err()
                .to_string()
                .contains("multiple capture runs")
        );
        assert!(temp.path().join("suite/bitmaps_test/prior").is_dir());
        assert!(temp.path().join("suite/bitmaps_test/current").is_dir());
    }

    #[test]
    fn missing_reference_is_reportable_but_missing_test_and_path_escapes_fail() {
        let (temp, mut report) = fixture();
        fs::remove_file(temp.path().join("suite/bitmaps_reference/home.png")).unwrap();
        assert!(
            load(temp.path()).unwrap().unwrap().tests[0]
                .reference
                .is_none()
        );
        for path in [
            "../../private.png",
            "../bitmaps_reference/../secret.png",
            "../bitmaps_reference/nested/home.png",
            "https://example.com/a.png",
            "../bitmaps_reference/CON.png",
        ] {
            report["tests"][0]["pair"]["reference"] = json!(path);
            write_fixture(temp.path(), &report);
            assert!(load(temp.path()).is_err(), "{path}");
        }
        report["tests"][0]["pair"]["reference"] = serde_json::Value::Null;
        fs::write(
            temp.path().join("suite/bitmaps_test/prior/home_diff.png"),
            png(80),
        )
        .unwrap();
        report["tests"][0]["pair"]["diffImage"] = json!("../bitmaps_test/prior/home_diff.png");
        write_fixture(temp.path(), &report);
        assert!(load(temp.path()).is_err());
        report["tests"][0]["pair"]["diffImage"] = serde_json::Value::Null;
        write_fixture(temp.path(), &report);
        fs::remove_file(temp.path().join("suite/bitmaps_test/current/home.png")).unwrap();
        assert!(load(temp.path()).is_err());
    }

    #[test]
    fn accepts_empty_suites_and_versioned_windows_paths_and_rejects_invalid_results() {
        let (temp, mut report) = fixture();
        report["tests"][0]["pair"]["test"] = json!("..\\bitmaps_test\\current\\home.png?v=old");
        write_fixture(temp.path(), &report);
        assert!(load(temp.path()).is_ok());
        report["tests"][0]["pair"]["misMatchThreshold"] = json!(101);
        write_fixture(temp.path(), &report);
        assert!(load(temp.path()).is_err());
        report["tests"][0]["pair"]["misMatchThreshold"] = json!(0.1);
        report["tests"][0]["status"] = json!("unknown");
        write_fixture(temp.path(), &report);
        assert!(load(temp.path()).is_err());
        report["tests"] = json!([]);
        write_fixture(temp.path(), &report);
        assert!(load(temp.path()).unwrap().unwrap().tests.is_empty());
    }
}
