use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use futures::{StreamExt, stream::FuturesUnordered};
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio::time::{Duration, sleep};
use walkdir::WalkDir;

use crate::{
    cli::args::{RefArgs, ScenarioFilter, SnapshotArgs, TestArgs},
    common,
    compare::report_gen::{
        StagedSuiteComparison, generate_reference_report, validate_capture_run_inventory,
    },
    config::{self, replacements},
    helpers::{OnceLockExtensions, copy_file_atomically, is_windows_reserved_path_component},
    progress::{self, Event as ProgressEvent},
    run_history::{RunLease, RunState},
    snapshot,
    test::{
        browsers::prepare_browser,
        capture,
        image_simplification::ImageSimplificationSetting,
        test_scenario::{
            ScenarioRequest, capture_progress_keys, collect_needs_scenarios,
            collect_url_replacements, make_log_prefix, scenario_capture_order_keys,
            scenario_capture_viewports,
        },
        test_suite::{ScenarioOutcome, exec_single_scenario, parse_test_suite},
    },
    types::{
        Browser as TestBrowser, Needs, TestScenario, TestSuite, TestSuiteFormat, TestSuiteInput,
        UrlReplacement,
    },
};

/// Outcome of a spawned scenario task, used to decide the process exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskStatus {
    /// Scenario captured successfully.
    Passed,
    /// Page-level failure (timeout, not found, ...) — does NOT fail the command.
    PageFailed,
    /// Infrastructure failure (browser launch) — fails the command.
    InfraFailed,
}

const MAX_OUTPUT_IDENTIFIER_BYTES: usize = 80;

/// One independent capture unit: a single scenario at a single viewport, with a
/// dedicated global index. Built upfront (TS/BackstopJS-style) so every
/// scenario/viewport pair runs as its own task with its own browser and index.
struct CaptureJob {
    suite: Arc<TestSuite>,
    suite_name: String,
    suite_semaphore: Arc<Semaphore>,
    scenario_idx: usize,
    viewport: Option<config::viewports::ViewportDef>,
    index: usize,
    captures: Vec<String>,
    comparison: Option<Arc<StagedSuiteComparison>>,
}

fn suite_output_dir(regressify_dir: &Path, suite: &TestSuite) -> Result<PathBuf> {
    let suite_id = suite
        .id
        .as_deref()
        .ok_or_else(|| anyhow!("Test suite ID is required for output paths"))?;
    Ok(regressify_dir.join(suite_id))
}

fn current_capture_run_dir(tests_dir: &Path, suite: &TestSuite) -> Result<PathBuf> {
    let run_id = suite
        .test_date
        .as_deref()
        .ok_or_else(|| anyhow!("Test suite is missing a capture run ID"))?;
    let run_dir = tests_dir.join(run_id);
    if !run_dir.is_dir() {
        anyhow::bail!(
            "Current capture run directory does not exist: {}",
            run_dir.display()
        );
    }
    Ok(run_dir)
}

fn ensure_capture_run_directories_are_new(
    output_dir: &Path,
    suites: &[(TestSuiteInput, Arc<TestSuite>)],
) -> Result<()> {
    for (input, suite) in suites {
        let suite_dir = suite_output_dir(output_dir, suite)?;
        let run_id = suite
            .test_date
            .as_deref()
            .ok_or_else(|| anyhow!("Test suite '{}' is missing a capture run ID", input.name))?;
        let run_dir = suite_dir.join("bitmaps_test").join(run_id);
        if run_dir.exists() {
            anyhow::bail!(
                "Capture run directory already exists for test suite '{}': {}. Use a new --run-id or remove the stale run before capturing.",
                input.name,
                run_dir.display()
            );
        }
    }
    Ok(())
}

fn filter_scenario_suites(
    inputs: Vec<TestSuiteInput>,
    filters: &[ScenarioFilter],
) -> Result<Vec<TestSuiteInput>> {
    for filter in filters {
        anyhow::ensure!(
            inputs.iter().any(|input| input.name == filter.suite),
            "Selected scenario '{filter}' does not belong to an available selected suite"
        );
    }
    Ok(inputs
        .into_iter()
        .filter(|input| {
            filters.is_empty() || filters.iter().any(|filter| filter.suite == input.name)
        })
        .collect())
}

fn validate_scenario_filters(
    suites: &[(TestSuiteInput, Arc<TestSuite>)],
    filters: &[ScenarioFilter],
) -> Result<()> {
    for filter in filters {
        let matches = suites
            .iter()
            .filter(|(input, _)| input.name == filter.suite)
            .flat_map(|(_, suite)| &suite.scenarios)
            .filter(|scenario| scenario.id.as_deref() == Some(filter.scenario.as_str()))
            .count();
        anyhow::ensure!(
            matches == 1,
            "Selected scenario '{filter}' must match exactly one scenario, found {matches}"
        );
    }
    Ok(())
}

fn selected_scenarios<'suite>(
    name: &'suite str,
    suite: &'suite TestSuite,
    filters: &'suite [ScenarioFilter],
) -> impl Iterator<Item = (usize, &'suite TestScenario)> {
    suite
        .scenarios
        .iter()
        .enumerate()
        .filter(move |(_, scenario)| {
            filters.is_empty()
                || filters.iter().any(|filter| {
                    filter.suite == name && scenario.id.as_deref() == Some(filter.scenario.as_str())
                })
        })
}

/// Keep full suite definitions for prerequisite actions, but queue only selected
/// scenario/viewport pairs with independent 1-based job indices.
fn build_capture_jobs(
    parsed_suites: &[(TestSuiteInput, Arc<TestSuite>)],
    workspace_config: &config::WorkspaceConfig,
    filters: &[ScenarioFilter],
) -> Result<Vec<CaptureJob>> {
    let mut jobs = Vec::new();
    for (input, suite) in parsed_suites {
        if progress::enabled() {
            progress::emit(ProgressEvent::SuiteQueued {
                id: suite.id.clone().unwrap_or_else(|| input.name.clone()),
                name: input.name.clone(),
            });
        }
        let suite_semaphore = Arc::new(Semaphore::new(config::resolve_async_capture_limit(
            workspace_config,
            suite,
        )));
        for (scenario_idx, scenario) in selected_scenarios(&input.name, suite, filters) {
            let viewports = scenario_capture_viewports(workspace_config, suite, scenario)?;
            for viewport in viewports {
                let captures = capture_progress_keys(
                    scenario,
                    &suite.scenarios,
                    viewport.as_ref().map(|viewport| viewport.label.as_str()),
                )?;
                jobs.push(CaptureJob {
                    suite: suite.clone(),
                    suite_name: input.name.clone(),
                    suite_semaphore: suite_semaphore.clone(),
                    scenario_idx,
                    viewport,
                    index: 0,
                    captures,
                    comparison: None,
                });
            }
        }
    }
    for (index, job) in jobs.iter_mut().enumerate() {
        job.index = index + 1;
        if progress::enabled() {
            let scenario = &job.suite.scenarios[job.scenario_idx];
            progress::emit(ProgressEvent::Queued {
                job: progress::Job {
                    id: job.index,
                    suite_id: job
                        .suite
                        .id
                        .clone()
                        .unwrap_or_else(|| job.suite_name.clone()),
                    suite_name: job.suite_name.clone(),
                    scenario_id: scenario.id.clone().unwrap_or_default(),
                    label: scenario
                        .label
                        .as_deref()
                        .filter(|label| !label.trim().is_empty())
                        .unwrap_or(&scenario.url)
                        .to_string(),
                    viewport: job
                        .viewport
                        .as_ref()
                        .map(|viewport| viewport.label.clone())
                        .unwrap_or_else(|| "Default".into()),
                    captures: job.captures.clone(),
                },
            });
        }
    }
    Ok(jobs)
}

/// Spawn one task per capture job. Each task acquires a concurrency permit,
/// launches its own browser, and runs exactly one scenario/viewport capture with
/// its dedicated index. A failure in one capture only fails that task; siblings
/// (including other viewports of the same scenario) run independently.
fn spawn_capture_jobs(
    jobs: Vec<CaptureJob>,
    semaphore: &Arc<Semaphore>,
    exe_path: &Arc<Option<PathBuf>>,
    workspace_config: &Arc<config::WorkspaceConfig>,
    active_replacements: &Arc<Vec<UrlReplacement>>,
    apply_url_replacements: bool,
) -> Vec<JoinHandle<TaskStatus>> {
    progress::emit(ProgressEvent::Phase {
        name: "Capturing scenarios".into(),
    });
    let total = jobs.len();
    let mut handles = Vec::with_capacity(total);

    for job in jobs {
        let CaptureJob {
            suite,
            suite_name: _,
            suite_semaphore,
            scenario_idx,
            viewport,
            index,
            captures,
            comparison,
        } = job;
        let sem = semaphore.clone();
        let suite_sem = suite_semaphore.clone();
        let exe_path = exe_path.clone();
        let ws_config = workspace_config.clone();
        let replacements = active_replacements.clone();

        let handle = tokio::spawn(progress::scope_scenario(index, async move {
            use crate::diagnostics::{record::*, runtime};
            let resolved =
                config::resolve_config(&ws_config, &suite, &suite.scenarios[scenario_idx]);
            // Every capture of this job, including saves from `needs` actions,
            // uses the same resolved setting; its links record it for comparison.
            let image_simplification = ImageSimplificationSetting::from_config(&resolved);
            let operation = common::REGRESSIFY_DIR.get().map(|output| {
                let scenario = &suite.scenarios[scenario_idx];
                let identity = ExecutionIdentity {
                    execution_id: ExecutionId::new(),
                    suite_id: SafeIdentity::source(suite.id.as_deref().unwrap_or_default()),
                    scenario_id: SafeIdentity::source(scenario.id.as_deref().unwrap_or_default()),
                    viewport: ViewportIdentity {
                        name: SafeIdentity::source(
                            viewport
                                .as_ref()
                                .map(|viewport| viewport.label.as_str())
                                .unwrap_or("Default"),
                        ),
                        width: viewport
                            .as_ref()
                            .map(|viewport| viewport.width)
                            .unwrap_or(1920),
                        height: viewport
                            .as_ref()
                            .map(|viewport| viewport.height)
                            .unwrap_or(1080),
                    },
                    capture_role: if apply_url_replacements {
                        CaptureRole::Test
                    } else {
                        CaptureRole::Reference
                    },
                    attempt: 1,
                };
                runtime::Operation::start(
                    output,
                    OperationKind::Capture,
                    suite
                        .test_date
                        .clone()
                        .and_then(|run| CaptureRunId::try_from(run).ok()),
                    Some(identity),
                    Some(index),
                )
            });
            if let Some(operation) = &operation
                && capture::write_execution_logs(
                    &suite,
                    &suite.scenarios[scenario_idx],
                    viewport.as_ref().map(|viewport| viewport.label.as_str()),
                    &operation.reference(),
                    &image_simplification,
                )
                .is_err()
            {
                crate::diagnostics::install::emergency_warning(
                    "Initial capture log linkage could not be written",
                );
            }
            let (status, error, infrastructure) =
                runtime::scope(operation.as_ref().map(runtime::Operation::context), async {
                    if let Some(context) = runtime::current() {
                        context.set_display_level(
                            common::LOG_LEVEL_OVERRIDE
                                .get()
                                .copied()
                                .flatten()
                                .unwrap_or(resolved.log_level),
                        );
                    }
                    let fields = queue_setup_fields(&resolved);
                    runtime::event(
                        Stage::QueueSetup,
                        DiagnosticEvent::Queued,
                        Severity::Info,
                        fields.clone(),
                    );
                    let queued = std::time::Instant::now();
                    let permits = runtime::stage(
                        Stage::QueueSetup,
                        fields,
                        acquire_capture_permits(suite_sem, sem),
                    )
                    .await;
                    runtime::event(
                        Stage::QueueSetup,
                        DiagnosticEvent::Decision,
                        Severity::Debug,
                        [(
                            Field::QueueMs,
                            FieldValue::Number(queued.elapsed().as_millis() as u64),
                        )],
                    );
                    let Ok((_suite_permit, _global_permit)) = permits else {
                        return (
                            TaskStatus::InfraFailed,
                            Some("Capture semaphore closed".into()),
                            true,
                        );
                    };
                    progress::emit(ProgressEvent::Started { id: index });
                    let outcome = exec_single_scenario(
                        &exe_path,
                        ScenarioRequest {
                            test_suite: &suite,
                            scenario: &suite.scenarios[scenario_idx],
                            all_scenarios: &suite.scenarios,
                            workspace_config: &ws_config,
                            replacement_profile: &replacements,
                            apply_url_replacements,
                            viewport: viewport.as_ref(),
                            index,
                            total,
                        },
                    )
                    .await;
                    let (error, infrastructure) = match &outcome {
                        ScenarioOutcome::Passed => (None, false),
                        ScenarioOutcome::PageFailed(error) => {
                            (Some(runtime::failure_summary(error)), false)
                        }
                        ScenarioOutcome::InfraFailed(error) => {
                            (Some(runtime::failure_summary(error)), true)
                        }
                    };
                    if let (
                        Some(summary),
                        ScenarioOutcome::PageFailed(detail) | ScenarioOutcome::InfraFailed(detail),
                    ) = (&error, &outcome)
                    {
                        log_capture_failure(
                            &make_log_prefix(index, total),
                            &format!(
                                "{}:{} [{}]",
                                suite.id.as_deref().unwrap_or_default(),
                                suite.scenarios[scenario_idx]
                                    .id
                                    .as_deref()
                                    .unwrap_or_default(),
                                viewport
                                    .as_ref()
                                    .map(|viewport| viewport.label.as_str())
                                    .unwrap_or("Default")
                            ),
                            summary,
                            detail,
                        );
                    }
                    let status = match outcome {
                        ScenarioOutcome::Passed => TaskStatus::Passed,
                        ScenarioOutcome::PageFailed(_) => TaskStatus::PageFailed,
                        ScenarioOutcome::InfraFailed(_) => TaskStatus::InfraFailed,
                    };
                    (status, error, infrastructure)
                })
                .await;
            if let Some(operation) = operation {
                let archive = operation
                    .finish(if status == TaskStatus::Passed {
                        Outcome::Success
                    } else {
                        Outcome::Failure
                    })
                    .await;
                if capture::write_execution_logs(
                    &suite,
                    &suite.scenarios[scenario_idx],
                    viewport.as_ref().map(|viewport| viewport.label.as_str()),
                    &archive,
                    &image_simplification,
                )
                .is_err()
                {
                    crate::diagnostics::install::emergency_warning(
                        "Capture log linkage could not be written; capture results are unchanged",
                    );
                    progress::emit(ProgressEvent::Log {
                        id: index,
                        message: "Capture log linkage unavailable".into(),
                    });
                }
            }
            if infrastructure {
                progress::emit(ProgressEvent::Captured {
                    id: index,
                    error,
                    infrastructure: true,
                });
                return status;
            }
            if let Some(comparison) = comparison {
                progress::emit(ProgressEvent::JobComparing { id: index, error });
                let Ok(_compare_permit) = comparison.acquire_permit().await else {
                    return TaskStatus::InfraFailed;
                };
                let compared =
                    tokio::task::spawn_blocking(move || comparison.compare_job_captures(&captures))
                        .await;
                match compared {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        eprintln!("Visual comparison failed for capture job #{index}: {error:?}");
                        return TaskStatus::InfraFailed;
                    }
                    Err(error) => {
                        eprintln!("Comparison task panicked for capture job #{index}: {error:?}");
                        return TaskStatus::InfraFailed;
                    }
                }
            } else if !apply_url_replacements {
                progress::emit(ProgressEvent::JobApproved { id: index, error });
            } else {
                progress::emit(ProgressEvent::Captured {
                    id: index,
                    error,
                    infrastructure: false,
                });
            }
            status
        }));

        handles.push(handle);
    }

    handles
}

/// Print a failed capture's step summary and its sanitized error detail. The
/// detail can name selectors, files, or hook output, so it goes only to the
/// local console and live desktop log; markers, reports, and journals keep the
/// typed summary.
fn log_capture_failure(prefix: &str, capture: &str, summary: &str, error: &anyhow::Error) {
    if common::NO_PRINT.get().copied().unwrap_or(false) {
        return;
    }
    progress::scenario_log!("{} {} {}: {}", prefix, "FAILED:".red(), capture, summary);
    progress::scenario_log!(
        "{}   Local detail (not saved in reports): {}",
        prefix,
        capture_failure_detail(error)
    );
}

/// Sanitized error chain for local display: URL paths, queries, credentials,
/// credential-like assignments, and control characters are removed and the text
/// is bounded.
fn capture_failure_detail(error: &anyhow::Error) -> String {
    crate::diagnostics::redaction::sanitize_known(&format!("{error:#}"), &[])
}

fn queue_setup_fields(
    config: &config::ResolvedConfig,
) -> Vec<(
    crate::diagnostics::record::Field,
    crate::diagnostics::record::FieldValue,
)> {
    use crate::diagnostics::record::{Field, FieldValue};
    vec![
        (
            Field::ElementTimeoutMs,
            FieldValue::Number(config.element_timeout_ms),
        ),
        (
            Field::ActionabilityTimeoutMs,
            FieldValue::Number(config.actionability_timeout_ms),
        ),
        (
            Field::PollIntervalMs,
            FieldValue::Number(
                crate::test::actionability::ActionabilityConfig::default().poll_interval_ms,
            ),
        ),
        (
            Field::ScenarioTimeoutMs,
            FieldValue::Number(config.scenario_timeout_ms),
        ),
        (
            Field::RetryLimit,
            FieldValue::Number(crate::test::browsers::MAX_LAUNCH_RETRIES as u64),
        ),
        (Field::QueueMs, FieldValue::Number(0)),
    ]
}

/// Acquire the suite limit before the global limit. This prevents a job that
/// already exceeds its suite cap from occupying a global capture slot.
async fn acquire_capture_permits(
    suite_semaphore: Arc<Semaphore>,
    global_semaphore: Arc<Semaphore>,
) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit)> {
    let suite_permit = suite_semaphore
        .acquire_owned()
        .await
        .map_err(|_| anyhow!("Suite capture semaphore closed"))?;
    let global_permit = global_semaphore
        .acquire_owned()
        .await
        .map_err(|_| anyhow!("Global capture semaphore closed"))?;
    Ok((suite_permit, global_permit))
}

/// Discover available test suite files in the visual_tests/ directory.
pub(crate) fn discover_test_suites(
    visual_tests_dir: &std::path::Path,
) -> Result<Vec<TestSuiteInput>> {
    let mut suites: HashMap<String, (u8, TestSuiteInput)> = HashMap::new();
    for entry in WalkDir::new(visual_tests_dir) {
        let entry = entry.map_err(|error| {
            anyhow!(
                "Failed to discover test suites under '{}': {}",
                visual_tests_dir.display(),
                error
            )
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_lowercase();
        if file_name.ends_with(".tests.yaml")
            || file_name.ends_with(".tests.yml")
            || file_name.ends_with(".tests.json")
        {
            let Some((suite_name, ext)) = file_name.rsplit_once(".tests.") else {
                eprintln!(
                    "  {} Skipping unexpected test suite file name: {}",
                    "WARNING:".yellow(),
                    file_name
                );
                continue;
            };

            let (format, priority) = match ext {
                "yaml" => (TestSuiteFormat::Yaml, 0),
                "yml" => (TestSuiteFormat::Yaml, 1),
                "json" => (TestSuiteFormat::Json, 2),
                _ => {
                    eprintln!(
                        "  {} Skipping test suite with unsupported format '{}': {}",
                        "WARNING:".yellow(),
                        ext,
                        file_name
                    );
                    continue;
                }
            };

            let input = TestSuiteInput {
                name: suite_name.to_string(),
                path: entry.path().to_path_buf(),
                format,
            };

            match suites.get_mut(suite_name) {
                Some((current_priority, current_input)) if priority < *current_priority => {
                    *current_priority = priority;
                    *current_input = input;
                }
                Some((current_priority, current_input)) if priority == *current_priority => {
                    anyhow::bail!(
                        "Ambiguous test suite '{}': both '{}' and '{}' use the same .tests.{} priority",
                        suite_name,
                        current_input.path.display(),
                        input.path.display(),
                        ext
                    );
                }
                Some(_) => {}
                None => {
                    suites.insert(suite_name.to_string(), (priority, input));
                }
            }
        }
    }
    let mut suites: Vec<_> = suites.into_values().map(|(_, input)| input).collect();
    suites.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(suites)
}

/// Filter test suites by glob patterns (or return all if patterns is empty / contains "all" / "*").
fn filter_test_suites(
    available: Vec<TestSuiteInput>,
    patterns: &[String],
) -> Result<Vec<TestSuiteInput>> {
    if patterns.is_empty()
        || patterns.contains(&"all".to_string())
        || patterns.contains(&"*".to_string())
    {
        Ok(available)
    } else {
        let patterns: Vec<_> = patterns
            .iter()
            .map(|pattern| {
                glob::Pattern::new(pattern)
                    .map_err(|error| anyhow!("Invalid test suite pattern '{}': {}", pattern, error))
            })
            .collect::<Result<_>>()?;
        Ok(available
            .into_iter()
            .filter(|ts| patterns.iter().any(|pattern| pattern.matches(&ts.name)))
            .collect())
    }
}

/// Build a semaphore that ramps concurrency from 1 to `limit`,
/// adding one permit every `step_ms` milliseconds until the target is reached.
fn create_ramped_semaphore(limit: usize) -> Arc<Semaphore> {
    let target = limit.max(1);
    let step_ms = common::ASYNC_CAPTURE_RAMP_MS.get().copied().unwrap_or(1000);
    let semaphore = Arc::new(Semaphore::new(1));

    if target > 1 {
        let sem = Arc::clone(&semaphore);
        tokio::spawn(async move {
            for _ in 1..target {
                sleep(Duration::from_millis(step_ms)).await;
                sem.add_permits(1);
            }
        });
    }

    semaphore
}

fn load_active_replacement_profile(
    root_dir: &std::path::Path,
    cli_profile: Option<&str>,
) -> Result<Vec<UrlReplacement>> {
    let profile_name = replacements::get_profile_name(cli_profile);
    let explicit_profile = cli_profile.is_some()
        || std::env::var("REPLACEMENT_PROFILE")
            .map(|value| !value.is_empty())
            .unwrap_or(false);

    let Some(profiles) = replacements::load_replacement_profiles(root_dir)? else {
        if explicit_profile {
            anyhow::bail!(
                "Replacement profile file not found: {}",
                root_dir.join("common/_replacement-profiles.yaml").display()
            );
        }
        return Ok(Vec::new());
    };

    let active = match profiles.profiles.get(&profile_name) {
        Some(profile) => profile.clone(),
        None if explicit_profile => {
            anyhow::bail!("Replacement profile '{}' is not defined", profile_name)
        }
        None => Vec::new(),
    };

    replacements::validate_replacements(
        &active,
        &format!("replacement profile '{}'", profile_name),
    )?;
    Ok(active)
}

fn validate_workspace_config(workspace_config: &config::WorkspaceConfig) -> Result<()> {
    validate_percentage(
        "workspace misMatchThreshold",
        workspace_config.mis_match_threshold,
    )?;
    validate_pixel_match_threshold(
        "workspace pixelMatchThreshold",
        workspace_config.pixel_match_threshold,
    )?;
    validate_duration("workspace delay", workspace_config.delay)?;
    validate_duration(
        "workspace postInteractionWait",
        workspace_config.post_interaction_wait,
    )?;
    for (name, value) in [
        ("elementTimeout", workspace_config.element_timeout),
        ("lazyLoadTimeout", workspace_config.lazy_load_timeout),
        (
            "actionabilityTimeout",
            workspace_config.actionability_timeout,
        ),
        ("scenarioTimeout", workspace_config.scenario_timeout),
    ] {
        if value == Some(0) {
            anyhow::bail!("workspace {} must be greater than zero", name);
        }
    }
    if workspace_config.async_capture_limit == Some(0) {
        anyhow::bail!("workspace asyncCaptureLimit must be greater than zero");
    }
    if workspace_config.async_compare_limit == Some(0) {
        anyhow::bail!("workspace asyncCompareLimit must be greater than zero");
    }
    if workspace_config.keep_test_runs == Some(0) {
        anyhow::bail!("workspace keepTestRuns must be at least 1");
    }
    config::validate_image_simplification(
        "workspace",
        workspace_config.image_simplification_block_size,
        workspace_config.image_simplification_min_size,
        workspace_config
            .image_simplification_exclude_selectors
            .as_deref(),
    )?;
    if let Some(browser) = &workspace_config.browser
        && !matches!(browser, TestBrowser::Chromium)
    {
        anyhow::bail!(
            "Unsupported browser in workspace config: only 'chromium' is currently supported"
        );
    }
    if let Some(replacements) = &workspace_config.url_replacements {
        replacements::validate_replacements(replacements, "workspace urlReplacements")?;
    }
    Ok(())
}

fn validate_percentage(name: &str, value: Option<f64>) -> Result<()> {
    if let Some(value) = value
        && (!value.is_finite() || !(0.0..=100.0).contains(&value))
    {
        anyhow::bail!("{} must be a finite value between 0 and 100", name);
    }
    Ok(())
}

fn validate_duration(name: &str, value: Option<f64>) -> Result<()> {
    if let Some(value) = value
        && (!value.is_finite() || value < 0.0)
    {
        anyhow::bail!(
            "{} must be a finite value greater than or equal to zero",
            name
        );
    }
    Ok(())
}

fn validate_pixel_match_threshold(name: &str, value: Option<f64>) -> Result<()> {
    if let Some(value) = value
        && (!value.is_finite() || !(0.0..=100.0).contains(&value))
    {
        anyhow::bail!("{} must be a finite value between 0 and 100", name);
    }
    Ok(())
}

pub(crate) fn is_reserved_suite_identifier(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "all" | "_snapshot" | "_diagnostics" | "_landing" | "states"
    )
}

fn validate_output_identifier(name: &str, value: Option<&str>) -> Result<()> {
    let value = value.ok_or_else(|| anyhow!("{} is required", name))?;
    if value.trim().is_empty()
        || value == "."
        || value == ".."
        || value != value.trim()
        || value.len() > MAX_OUTPUT_IDENTIFIER_BYTES
        || value.ends_with(['.', ' '])
        || value.contains(['/', '\\', '\0', '<', '>', ':', '"', '|', '?', '*'])
        || value.chars().any(char::is_control)
        || is_windows_reserved_path_component(value)
    {
        anyhow::bail!(
            "{} '{}' is not a portable output path component no longer than {} bytes",
            name,
            value,
            MAX_OUTPUT_IDENTIFIER_BYTES
        );
    }
    Ok(())
}

pub(crate) fn validate_suite_identifier(name: &str, value: Option<&str>) -> Result<()> {
    validate_output_identifier(name, value)?;
    let value = value.unwrap_or_default();
    if value.starts_with('.') {
        anyhow::bail!("{} '{}' must not start with '.'", name, value);
    }
    if is_reserved_suite_identifier(value) {
        anyhow::bail!("{} '{}' is reserved", name, value);
    }
    Ok(())
}

fn validate_test_suite_config(input: &TestSuiteInput, suite: &TestSuite) -> Result<()> {
    validate_suite_identifier("Test suite name", Some(input.name.as_str()))?;
    validate_suite_identifier("Test suite ID", suite.id.as_deref())?;
    validate_percentage(
        &format!("test suite '{}' misMatchThreshold", input.name),
        suite.mis_match_threshold,
    )?;
    validate_pixel_match_threshold(
        &format!("test suite '{}' pixelMatchThreshold", input.name),
        suite.pixel_match_threshold,
    )?;
    validate_duration(
        &format!("test suite '{}' postInteractionWait", input.name),
        suite.post_interaction_wait,
    )?;
    if suite.lazy_load_timeout == Some(0) {
        anyhow::bail!(
            "Test suite '{}' lazyLoadTimeout must be greater than zero",
            input.name
        );
    }
    if suite.async_capture_limit == Some(0) {
        anyhow::bail!(
            "Test suite '{}' asyncCaptureLimit must be greater than zero",
            input.name
        );
    }
    if suite.async_compare_limit == Some(0) {
        anyhow::bail!(
            "Test suite '{}' asyncCompareLimit must be greater than zero",
            input.name
        );
    }
    if suite.keep_test_runs == Some(0) {
        anyhow::bail!(
            "Test suite '{}' keepTestRuns must be at least 1",
            input.name
        );
    }
    config::validate_image_simplification(
        &format!("Test suite '{}'", input.name),
        suite.image_simplification_block_size,
        suite.image_simplification_min_size,
        suite.image_simplification_exclude_selectors.as_deref(),
    )?;
    if let Some(browser) = &suite.browser
        && !matches!(browser, TestBrowser::Chromium)
    {
        anyhow::bail!(
            "Unsupported browser in test suite '{}': only 'chromium' is currently supported",
            input.name
        );
    }
    if let Some(replacements) = &suite.url_replacements {
        replacements::validate_replacements(
            replacements,
            &format!("test suite '{}' urlReplacements", input.name),
        )?;
    }
    for (index, scenario) in suite.scenarios.iter().enumerate() {
        let scenario_name = scenario_identifier(scenario, &format!("<scenario:{}>", index + 1));
        validate_output_identifier("Scenario ID", scenario.id.as_deref())?;
        validate_percentage(
            &format!("scenario '{}' misMatchThreshold", scenario_name),
            scenario.mis_match_threshold,
        )?;
        validate_pixel_match_threshold(
            &format!("scenario '{}' pixelMatchThreshold", scenario_name),
            scenario.pixel_match_threshold,
        )?;
        validate_duration(
            &format!("scenario '{}' delay", scenario_name),
            scenario.delay,
        )?;
        validate_duration(
            &format!("scenario '{}' postInteractionWait", scenario_name),
            scenario.post_interaction_wait,
        )?;
        if scenario.lazy_load_timeout == Some(0) {
            anyhow::bail!(
                "scenario '{}' lazyLoadTimeout must be greater than zero",
                scenario_name
            );
        }
        config::validate_image_simplification(
            &format!("Test suite '{}' scenario '{}'", input.name, scenario_name),
            scenario.image_simplification_block_size,
            scenario.image_simplification_min_size,
            scenario.image_simplification_exclude_selectors.as_deref(),
        )?;
        capture::validate_scenario_capture_names(scenario).map_err(|error| {
            anyhow!(
                "Invalid selectors for scenario '{}': {}",
                scenario_name,
                error
            )
        })?;
        for (action_index, action) in scenario.actions.iter().flatten().enumerate() {
            super::test_action::validate_action_fields(action).map_err(|error| {
                anyhow!(
                    "Invalid action #{} in test suite '{}' scenario '{}': {}",
                    action_index + 1,
                    input.name,
                    scenario_name,
                    error
                )
            })?;
        }
    }
    validate_scenario_dependencies(input, suite)?;
    Ok(())
}

fn normalize_scenario_id(id: &str) -> String {
    id.trim().to_ascii_lowercase()
}

fn scenario_identifier(scenario: &TestScenario, fallback: &str) -> String {
    scenario
        .id
        .as_deref()
        .or(scenario.label.as_deref())
        .unwrap_or(fallback)
        .to_string()
}

fn needs_ids(needs: &Needs) -> Vec<&str> {
    match needs {
        Needs::String(id) => vec![id.as_str()],
        Needs::StringArray(ids) => ids.iter().map(String::as_str).collect(),
    }
}

#[derive(Debug)]
pub(crate) struct ScenarioDependencyError {
    #[cfg(feature = "desktop")]
    pub scenario_index: usize,
    #[cfg(feature = "desktop")]
    pub field: &'static str,
    message: String,
}

impl ScenarioDependencyError {
    fn new(_scenario_index: usize, _field: &'static str, message: String) -> Self {
        Self {
            #[cfg(feature = "desktop")]
            scenario_index: _scenario_index,
            #[cfg(feature = "desktop")]
            field: _field,
            message,
        }
    }
}

impl std::fmt::Display for ScenarioDependencyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ScenarioDependencyError {}

fn validate_dependency_chain(
    input: &TestSuiteInput,
    scenarios: &[TestScenario],
    id_map: &HashMap<String, Vec<usize>>,
    scenario_idx: usize,
    level: usize,
    trail: &mut Vec<String>,
    active_ids: &mut Vec<String>,
) -> Result<()> {
    if level >= 100 {
        return Err(ScenarioDependencyError::new(
            scenario_idx,
            "needs",
            format!(
                "Scenario dependency depth exceeded 100 in test suite '{}': {}",
                input.name,
                trail.join(" -> ")
            ),
        )
        .into());
    }

    let Some(needs) = scenarios[scenario_idx].needs.as_ref() else {
        return Ok(());
    };

    for needed_id in needs_ids(needs) {
        let normalized_need = normalize_scenario_id(needed_id);
        if active_ids.contains(&normalized_need) {
            let mut cycle = trail.clone();
            cycle.push(needed_id.to_string());
            return Err(ScenarioDependencyError::new(
                scenario_idx,
                "needs",
                format!(
                    "Circular scenario dependency detected in test suite '{}': {}",
                    input.name,
                    cycle.join(" -> ")
                ),
            )
            .into());
        }

        let matching_indices = id_map.get(&normalized_need).cloned().unwrap_or_default();
        if matching_indices.len() != 1 {
            return Err(ScenarioDependencyError::new(
                scenario_idx,
                "needs",
                format!(
                    "The test suite '{}' must contain exactly one scenario with id: {}",
                    input.name, needed_id
                ),
            )
            .into());
        }

        let target_idx = matching_indices[0];
        active_ids.push(normalized_need);
        trail.push(scenario_identifier(&scenarios[target_idx], needed_id));
        validate_dependency_chain(
            input,
            scenarios,
            id_map,
            target_idx,
            level + 1,
            trail,
            active_ids,
        )?;
        trail.pop();
        active_ids.pop();
    }

    Ok(())
}

pub(crate) fn validate_scenario_dependencies(
    input: &TestSuiteInput,
    suite: &TestSuite,
) -> Result<()> {
    let mut id_map: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, scenario) in suite.scenarios.iter().enumerate() {
        if let Some(id) = scenario.id.as_deref() {
            id_map
                .entry(normalize_scenario_id(id))
                .or_default()
                .push(index);
        }
    }

    if let Some((id, indices)) = id_map
        .iter()
        .filter(|(_, indices)| indices.len() > 1)
        .min_by_key(|(_, indices)| indices[1])
    {
        return Err(ScenarioDependencyError::new(
            indices[1],
            "id",
            format!(
                "Duplicate scenario ID found in test suite '{}': '{}'. Scenario IDs must be unique.",
                input.name,
                id
            ),
        ).into());
    }

    for (index, scenario) in suite.scenarios.iter().enumerate() {
        let start = scenario_identifier(scenario, &format!("<scenario:{}>", index + 1));
        let mut trail = vec![start];
        let mut active_ids = scenario
            .id
            .as_deref()
            .map(|id| vec![normalize_scenario_id(id)])
            .unwrap_or_default();
        validate_dependency_chain(
            input,
            &suite.scenarios,
            &id_map,
            index,
            0,
            &mut trail,
            &mut active_ids,
        )?;
    }

    Ok(())
}

fn parse_test_suites(
    visual_tests_dir: &std::path::Path,
    inputs: Vec<TestSuiteInput>,
) -> Result<Vec<(TestSuiteInput, Arc<TestSuite>)>> {
    let mut parsed = Vec::new();
    for input in inputs {
        let suite = parse_test_suite(visual_tests_dir, &input)
            .map_err(|e| anyhow!("Error parsing test suite '{}': {:?}", input.name, e))?;
        validate_test_suite_config(&input, &suite)?;
        parsed.push((input, Arc::new(suite)));
    }
    if parsed.is_empty() {
        anyhow::bail!("No matching test suites found");
    }
    validate_unique_suite_output_ids(&parsed)?;
    Ok(parsed)
}

fn validate_external_capture_files(
    root_dir: &Path,
    workspace_config: &config::WorkspaceConfig,
    parsed_suites: &[(TestSuiteInput, Arc<TestSuite>)],
) -> Result<()> {
    let output_dir = common::REGRESSIFY_DIR.get_required()?;
    validate_external_capture_files_in(root_dir, output_dir, workspace_config, parsed_suites)
}

fn validate_external_capture_files_in(
    root_dir: &Path,
    output_dir: &Path,
    workspace_config: &config::WorkspaceConfig,
    parsed_suites: &[(TestSuiteInput, Arc<TestSuite>)],
) -> Result<()> {
    let mut checked_cookie_files = HashSet::new();
    let mut checked_state_files = HashSet::new();

    for (input, suite) in parsed_suites {
        for (index, scenario) in suite.scenarios.iter().enumerate() {
            let resolved = config::resolve_config(workspace_config, suite, scenario);
            let scenario_name = scenario_identifier(scenario, &format!("<scenario:{}>", index + 1));
            let context = format!("test suite '{}' scenario '{}'", input.name, scenario_name);

            config::basic_auth::resolve(&resolved.basic_auth)
                .with_context(|| format!("Invalid basicAuth for {}", context))?;

            for script in [&scenario.on_before_script, &scenario.on_ready_script]
                .into_iter()
                .flatten()
            {
                super::hooks::validate_lifecycle_script(root_dir, script)
                    .with_context(|| format!("Invalid browser hook for {}", context))?;
            }

            for action in scenario.actions.iter().flatten() {
                super::test_action::validate_action_files(action, &root_dir.join("visual_tests"))
                    .with_context(|| format!("Invalid input file for {}", context))?;
            }

            let cookie_path = match resolved.cookie_path.as_deref() {
                Some(cookie_path) => Some(
                    config::resolve_workspace_file(root_dir, cookie_path, "cookiePath")
                        .with_context(|| format!("Invalid cookiePath for {}", context))?,
                ),
                None => config::resolve_optional_workspace_file(
                    root_dir,
                    "common/_cookies.yaml",
                    "default cookiePath",
                )
                .with_context(|| format!("Invalid default cookiePath for {}", context))?,
            };
            if let Some(path) = cookie_path
                && checked_cookie_files.insert(path.clone())
            {
                let content = std::fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read cookiePath '{}'", path.display()))?;
                let cookies: Vec<serde_json::Value> = serde_yaml::from_str(&content)
                    .with_context(|| format!("Invalid cookie YAML '{}'", path.display()))?;
                capture::expand_cookie_entries(
                    &cookies,
                    &format!("cookiePath '{}'", path.display()),
                )
                .with_context(|| format!("Invalid cookie YAML '{}'", path.display()))?;
            }

            if let Some(state) = resolved.state.as_ref() {
                let state_names = match state {
                    Needs::String(name) => vec![name.as_str()],
                    Needs::StringArray(names) => names.iter().map(String::as_str).collect(),
                };
                for state_name in state_names {
                    crate::test::capture::validate_state_name(state_name)?;
                    let state_path =
                        capture::persisted_state_source(root_dir, output_dir, state_name)?;
                    if checked_state_files.insert(state_path.clone()) {
                        crate::test::capture::read_persisted_state_file(&state_path, state_name)
                            .with_context(|| format!("Invalid persisted state for {}", context))?;
                    }
                }
            }

            if resolved.use_css_override {
                match resolved.css_override_path.as_deref() {
                    Some(css_path) => {
                        config::resolve_workspace_file(root_dir, css_path, "cssOverridePath")
                            .with_context(|| format!("Invalid cssOverridePath for {}", context))?;
                    }
                    None => {
                        config::resolve_optional_workspace_file(
                            root_dir,
                            "common/_override.css",
                            "default cssOverridePath",
                        )
                        .with_context(|| {
                            format!("Invalid default cssOverridePath for {}", context)
                        })?;
                    }
                }
            }

            match resolved.js_on_ready_path.as_deref() {
                Some(script_path) => {
                    config::resolve_workspace_file(root_dir, script_path, "jsOnReadyPath")
                        .with_context(|| format!("Invalid jsOnReadyPath for {}", context))?;
                }
                None => {
                    config::resolve_optional_workspace_file(
                        root_dir,
                        "common/_on-ready.js",
                        "default jsOnReadyPath",
                    )
                    .with_context(|| format!("Invalid default jsOnReadyPath for {}", context))?;
                }
            }
        }
    }
    Ok(())
}

fn validate_unique_suite_output_ids(suites: &[(TestSuiteInput, Arc<TestSuite>)]) -> Result<()> {
    let mut inputs_by_output_id = HashMap::new();
    for (input, suite) in suites {
        let suite_id = suite
            .id
            .as_deref()
            .ok_or_else(|| anyhow!("Test suite '{}' is missing an ID", input.name))?;
        let key = suite_id.to_lowercase();
        if let Some(previous_input) = inputs_by_output_id.insert(key, input.name.as_str()) {
            anyhow::bail!(
                "Test suites '{}' and '{}' share output ID '{}' on case-insensitive filesystems",
                previous_input,
                input.name,
                suite_id
            );
        }
    }
    Ok(())
}

fn validate_unique_capture_keys(
    suite_name: &str,
    capture_keys: impl IntoIterator<Item = (String, String)>,
) -> Result<()> {
    let mut owners_by_key = HashMap::new();
    for (capture_key, owner) in capture_keys {
        let normalized_key = capture_key.to_lowercase();
        if let Some(previous_owner) = owners_by_key.insert(normalized_key, owner.clone()) {
            anyhow::bail!(
                "Test suite '{}' has colliding screenshot filename '{}' for scenarios '{}' and '{}'",
                suite_name,
                capture_key,
                previous_owner,
                owner
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct ActionViewportNameError {
    #[cfg(feature = "desktop")]
    pub source_scenario_index: Option<usize>,
    #[cfg(feature = "desktop")]
    pub action_index: usize,
    message: String,
}

impl std::fmt::Display for ActionViewportNameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ActionViewportNameError {}

pub(crate) fn validate_action_viewport_names(
    suite_name: &str,
    scenario: &TestScenario,
    all_scenarios: &[TestScenario],
    effective_viewports: &[config::viewports::ViewportDef],
) -> Result<()> {
    let available = effective_viewports
        .iter()
        .map(|viewport| viewport.label.as_str())
        .collect::<Vec<_>>();
    let sources = collect_needs_scenarios(scenario, all_scenarios, &mut Vec::new());
    for source in sources.iter().copied().chain(std::iter::once(scenario)) {
        for (index, action) in source.actions.iter().flatten().enumerate() {
            let Some(names) = action.viewport_names() else {
                continue;
            };
            let invalid = names
                .iter()
                .filter(|name| {
                    name.trim().is_empty()
                        || !available
                            .iter()
                            .any(|label| name.eq_ignore_ascii_case(label))
                })
                .collect::<Vec<_>>();
            if invalid.is_empty() {
                continue;
            }
            let origin = if std::ptr::eq(source, scenario) {
                format!("action #{}", index + 1)
            } else {
                format!(
                    "prerequisite '{}' action #{}",
                    scenario_identifier(source, "<unnamed>"),
                    index + 1
                )
            };
            let message = format!(
                "Invalid viewportNames in test suite '{}' scenario '{}', {}: invalid name(s) {:?}. Available scenario viewport(s): {}",
                suite_name,
                scenario_identifier(scenario, "<unnamed>"),
                origin,
                invalid,
                if available.is_empty() {
                    "<unnamed browser default>".to_string()
                } else {
                    format!("{available:?}")
                }
            );
            return Err(ActionViewportNameError {
                #[cfg(feature = "desktop")]
                source_scenario_index: all_scenarios
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, source)),
                #[cfg(feature = "desktop")]
                action_index: index,
                message,
            }
            .into());
        }
    }
    let labels: Vec<_> = if effective_viewports.is_empty() {
        vec![None]
    } else {
        effective_viewports
            .iter()
            .map(|viewport| Some(viewport.label.as_str()))
            .collect()
    };
    for label in labels {
        super::test_action::validate_dialog_sequence(
            sources
                .iter()
                .copied()
                .chain(std::iter::once(scenario))
                .flat_map(|source| source.actions.iter().flatten())
                .filter(|action| super::test_scenario::action_applies_to_viewport(action, label)),
        )
        .map_err(|error| {
            anyhow!(
                "Invalid dialog sequence in suite '{}' scenario '{}': {}",
                suite_name,
                scenario_identifier(scenario, "<unnamed>"),
                error
            )
        })?;
    }
    Ok(())
}

fn validate_suite_capture_keys(
    workspace_config: &config::WorkspaceConfig,
    suites: &[(TestSuiteInput, Arc<TestSuite>)],
) -> Result<()> {
    for (input, suite) in suites {
        let mut capture_keys = Vec::new();
        for (index, scenario) in suite.scenarios.iter().enumerate() {
            let scenario_name = scenario_identifier(scenario, &format!("<scenario:{}>", index + 1));
            let viewports = scenario_capture_viewports(workspace_config, suite, scenario)?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            validate_action_viewport_names(&input.name, scenario, &suite.scenarios, &viewports)?;
            for capture_key in
                scenario_capture_order_keys(workspace_config, suite, scenario, &suite.scenarios)?
            {
                capture_keys.push((capture_key, scenario_name.clone()));
            }
        }
        validate_unique_capture_keys(&input.name, capture_keys)?;
    }
    Ok(())
}

fn report_page_urls(
    scenario: &TestScenario,
    url_replacements: &[UrlReplacement],
    apply_url_replacements: bool,
) -> Result<capture::ReportPageUrls> {
    let url = if apply_url_replacements {
        replacements::apply_replacements(&scenario.url, url_replacements)?
    } else {
        scenario.url.clone()
    };
    Ok(capture::ReportPageUrls {
        reference_url: scenario.url.clone(),
        url,
        scenario_id: scenario.id.clone(),
        scenario_label: Some(
            scenario
                .label
                .as_deref()
                .filter(|label| !label.trim().is_empty())
                .unwrap_or(&scenario.url)
                .to_string(),
        ),
        scenario_display_name: None,
    })
}

/// Persist report metadata before capture so standalone `compare` can recover
/// page URLs as well as ordering and threshold information.
fn write_capture_run_manifests(
    workspace_config: &config::WorkspaceConfig,
    parsed_suites: &[(TestSuiteInput, Arc<TestSuite>)],
    replacement_profile: &[UrlReplacement],
    apply_url_replacements: bool,
    filters: &[ScenarioFilter],
) -> Result<()> {
    for (input, suite) in parsed_suites {
        let url_replacements = if apply_url_replacements {
            collect_url_replacements(suite, workspace_config, replacement_profile)
        } else {
            Vec::new()
        };
        let mut order = Vec::new();
        let mut thresholds = Vec::new();
        let mut pixel_thresholds = Vec::new();
        let mut urls = Vec::new();

        // Resolve against the complete suite before filtering or replacing URLs.
        let names = crate::helpers::scenario_names(
            &suite
                .scenarios
                .iter()
                .map(|scenario| crate::helpers::ScenarioName {
                    id: scenario.id.as_deref().unwrap_or_default(),
                    label: scenario.label.as_deref(),
                    url: Some(&scenario.url),
                })
                .collect::<Vec<_>>(),
        );
        for (index, scenario) in selected_scenarios(&input.name, suite, filters) {
            let order_keys =
                scenario_capture_order_keys(workspace_config, suite, scenario, &suite.scenarios)?;
            let resolved = config::resolve_config(workspace_config, suite, scenario);
            let mut page_urls =
                report_page_urls(scenario, &url_replacements, apply_url_replacements)?;
            page_urls.scenario_display_name = Some(names[index].clone());
            for order_key in &order_keys {
                thresholds.push((order_key.clone(), resolved.mis_match_threshold));
                pixel_thresholds.push((order_key.clone(), resolved.pixel_match_threshold));
                if let Some((prefix, _)) = order_key.rsplit_once("__") {
                    thresholds.push((prefix.to_string(), resolved.mis_match_threshold));
                    pixel_thresholds.push((prefix.to_string(), resolved.pixel_match_threshold));
                }
                urls.push((order_key.clone(), page_urls.clone()));
            }
            order.extend(order_keys);
        }

        capture::write_report_order(suite, &order).map_err(|error| {
            anyhow!(
                "Failed to write report ordering manifest for '{}': {:?}",
                input.name,
                error
            )
        })?;
        capture::write_report_thresholds(suite, &thresholds).map_err(|error| {
            anyhow!(
                "Failed to write report threshold manifest for '{}': {:?}",
                input.name,
                error
            )
        })?;
        capture::write_report_pixel_thresholds(suite, &pixel_thresholds).map_err(|error| {
            anyhow!(
                "Failed to write report pixel threshold manifest for '{}': {:?}",
                input.name,
                error
            )
        })?;
        capture::write_report_urls(suite, &urls).map_err(|error| {
            anyhow!(
                "Failed to write report URL manifest for '{}': {:?}",
                input.name,
                error
            )
        })?;
        if capture::mark_diagnostics_expected(suite).is_err() {
            crate::diagnostics::install::emergency_warning(
                "Diagnostic availability marker could not be written",
            );
        }
        crate::compare::report_gen::write_suite_metadata(
            &suite_output_dir(common::REGRESSIFY_DIR.get_required()?, suite)?,
            suite.display_name.as_deref(),
        )?;
    }
    Ok(())
}

fn begin_suite_runs(
    output_dir: &Path,
    suites: &[(TestSuiteInput, Arc<TestSuite>)],
    filters: &[ScenarioFilter],
) -> Result<HashMap<String, RunLease>> {
    suites
        .iter()
        .map(|(input, suite)| {
            let full_suite = !suite.scenarios.is_empty()
                && selected_scenarios(&input.name, suite, filters).count() == suite.scenarios.len();
            let run = RunLease::begin(
                &suite_output_dir(output_dir, suite)?,
                suite
                    .test_date
                    .as_deref()
                    .context("Capture run ID is missing")?,
                full_suite,
            )?;
            Ok((input.name.clone(), run))
        })
        .collect()
}

fn finalize_test_suite(
    mut run: RunLease,
    comparison: Option<Arc<StagedSuiteComparison>>,
    infrastructure_failed: bool,
    keep_runs: Option<usize>,
) -> Result<()> {
    if infrastructure_failed {
        return run.finish(RunState::Failed, false);
    }
    let result = if let Some(comparison) = &comparison {
        comparison.finalize()
    } else {
        validate_capture_run_inventory(&run.suite_dir().join("bitmaps_test").join(run.run_id()))
            .map(|_| ())
    };
    if let Err(error) = result {
        run.finish(RunState::Failed, false)?;
        return Err(error);
    }
    run.finish(RunState::Completed, comparison.is_some())?;
    if comparison.is_none() {
        progress::emit(ProgressEvent::SuiteFinished {
            id: run
                .suite_dir()
                .file_name()
                .context("Suite output ID is missing")?
                .to_string_lossy()
                .into_owned(),
        });
    }
    let suite_dir = run.suite_dir().to_path_buf();
    let retention = keep_runs.filter(|_| run.is_full_suite());
    drop(run);
    drop(comparison);
    if let Some(keep) = retention {
        let output = suite_dir.parent().context("Suite output root is missing")?;
        let suite_id = suite_dir
            .file_name()
            .context("Suite output ID is missing")?
            .to_string_lossy()
            .into_owned();
        if let Err(error) = crate::run_history::cleanup::preview(output, &[suite_id], keep)
            .and_then(|plan| {
                for warning in &plan.warnings {
                    eprintln!("Automatic cleanup: {warning}");
                }
                crate::run_history::cleanup::execute(&plan)
            })
        {
            eprintln!(
                "Test results are complete, but automatic cleanup could not finish: {error:#}"
            );
        }
    }
    Ok(())
}

pub async fn exec_test_suites(args: TestArgs) -> Result<()> {
    let TestArgs {
        fetch_browser,
        test_suite: test_suites,
        scenarios,
        skip_compare,
        mis_match_threshold,
        replacement_profile,
    } = args;
    let root_dir = common::ROOT_DIR.get_required()?;
    let visual_tests_dir = root_dir.join("visual_tests");

    // Load workspace config
    let workspace_config = config::load_workspace_config(root_dir)?;
    validate_workspace_config(&workspace_config)?;
    let active_replacements =
        load_active_replacement_profile(root_dir, replacement_profile.as_deref())?;

    let available = discover_test_suites(&visual_tests_dir)?;
    let filtered = filter_test_suites(available, &test_suites)?;
    let filtered = filter_scenario_suites(filtered, &scenarios)?;

    // Parse all test suites upfront. YAML/config errors are infrastructure
    // errors and must fail before scenarios start.
    let parsed_suites = parse_test_suites(&visual_tests_dir, filtered)?;
    validate_scenario_filters(&parsed_suites, &scenarios)?;
    validate_suite_capture_keys(&workspace_config, &parsed_suites)?;
    validate_external_capture_files(root_dir, &workspace_config, &parsed_suites)?;
    ensure_capture_run_directories_are_new(common::REGRESSIFY_DIR.get_required()?, &parsed_suites)?;

    let workspace_config = Arc::new(workspace_config);
    let mut suite_runs = begin_suite_runs(
        common::REGRESSIFY_DIR.get_required()?,
        &parsed_suites,
        &scenarios,
    )?;

    write_capture_run_manifests(
        workspace_config.as_ref(),
        &parsed_suites,
        &active_replacements,
        true,
        &scenarios,
    )?;

    // Flatten every scenario/viewport pair into an independent capture job (own
    // browser, own dedicated index) and spawn them — matching the TS/BackstopJS
    // model where pairs are scaffolded upfront and run independently.
    let mut jobs = build_capture_jobs(&parsed_suites, workspace_config.as_ref(), &scenarios)?;
    let retention: HashMap<_, _> = parsed_suites
        .iter()
        .map(|(input, suite)| {
            (
                input.name.clone(),
                config::resolve_run_retention(&workspace_config, suite),
            )
        })
        .collect();
    let mut staged_comparisons = HashMap::new();
    if !skip_compare {
        let regressify_dir = common::REGRESSIFY_DIR.get_required()?;
        for (input, suite) in &parsed_suites {
            if let (Some(suite_id), Some(test_date)) =
                (suite.id.as_deref(), suite.test_date.as_deref())
            {
                let suite_dir = suite_output_dir(regressify_dir, suite)?;
                let threshold = suite.mis_match_threshold.unwrap_or(mis_match_threshold);
                let async_compare_limit =
                    config::resolve_async_compare_limit(workspace_config.as_ref(), suite);
                let staged = Arc::new(StagedSuiteComparison::new(
                    &suite_dir,
                    suite_id,
                    test_date,
                    threshold,
                    async_compare_limit,
                )?);
                staged_comparisons.insert(input.name.clone(), staged);
            }
        }
        for job in &mut jobs {
            job.comparison = staged_comparisons.get(&job.suite_name).cloned();
        }
    }

    let exe_path = Some(prepare_browser(fetch_browser).await?);

    // Run all scenarios across all suites with a global concurrency limit.
    let limit = common::ASYNC_CAPTURE_LIMIT.get().copied().unwrap_or(5);
    let semaphore = create_ramped_semaphore(limit);
    let exe_path = Arc::new(exe_path);
    let active_replacements = Arc::new(active_replacements);
    let job_suites: Vec<_> = jobs.iter().map(|job| job.suite_name.clone()).collect();
    let mut remaining = HashMap::<String, (usize, bool)>::new();
    for name in &job_suites {
        remaining.entry(name.clone()).or_default().0 += 1;
    }
    let handles = spawn_capture_jobs(
        jobs,
        &semaphore,
        &exe_path,
        &workspace_config,
        &active_replacements,
        true,
    );

    // Observe completion order so one unfinished suite cannot delay another suite's durable result.
    let mut page_failures = 0usize;
    let mut infra_failures = 0usize;
    let mut comparison_failures = 0usize;
    let mut completions: FuturesUnordered<_> = handles
        .into_iter()
        .zip(job_suites)
        .map(|(handle, name)| async move { (name, handle.await) })
        .collect();
    while let Some((name, outcome)) = completions.next().await {
        let pending = remaining
            .get_mut(&name)
            .context("Capture suite was not queued")?;
        match outcome {
            Ok(TaskStatus::Passed) => {}
            Ok(TaskStatus::PageFailed) => page_failures += 1,
            Ok(TaskStatus::InfraFailed) => {
                infra_failures += 1;
                pending.1 = true;
            }
            Err(e) => {
                eprintln!("Task panicked: {:?}", e);
                infra_failures += 1;
                pending.1 = true;
            }
        }
        pending.0 -= 1;
        if pending.0 == 0 {
            let failed = pending.1;
            let run = suite_runs
                .remove(&name)
                .context("Capture run ownership is missing")?;
            let comparison = staged_comparisons.remove(&name);
            let keep_runs = retention.get(&name).copied().flatten();
            let finalized = tokio::task::spawn_blocking(move || {
                finalize_test_suite(run, comparison, failed, keep_runs)
            })
            .await;
            match finalized {
                Ok(Ok(())) => eprintln!("Test suite '{name}' finished."),
                result => {
                    eprintln!("Could not finalize test suite '{name}': {result:?}");
                    comparison_failures += 1;
                }
            }
        }
    }
    for (name, run) in suite_runs {
        let comparison = staged_comparisons.remove(&name);
        if let Err(error) = finalize_test_suite(
            run,
            comparison,
            false,
            retention.get(&name).copied().flatten(),
        ) {
            eprintln!("Could not finalize empty test suite '{name}': {error:#}");
            comparison_failures += 1;
        }
    }

    // Only infrastructure failures (browser could not be launched or artifact
    // write failed) fail the capture phase immediately before committing reports.
    if infra_failures > 0 {
        anyhow::bail!(
            "Test run failed: {} scenario(s) had browser or artifact failures",
            infra_failures
        );
    }

    if page_failures > 0 {
        eprintln!(
            "{} scenario(s) failed due to page errors (e.g. timeout, not found). \
             These are reported in the comparison and do not fail the run.",
            page_failures.to_string().yellow()
        );
    }
    if comparison_failures > 0 {
        eprintln!(
            "{} suite comparison(s) could not be generated.",
            comparison_failures.to_string().yellow()
        );
        anyhow::bail!(
            "Test run failed: {} suite comparison report(s) could not be generated",
            comparison_failures
        );
    }

    if !skip_compare {
        generate_snapshot_reports()?;
    }

    Ok(())
}

/// `ref` command: capture baseline reference screenshots.
/// Promotes only successful selected captures, preserving unrelated references.
pub async fn exec_ref(args: RefArgs) -> Result<()> {
    let RefArgs {
        fetch_browser,
        test_suite: test_suites,
        scenarios,
        replacement_profile,
    } = args;
    if replacement_profile.is_some() {
        eprintln!(
            "{}",
            "WARNING: --replacement-profile is ignored in ref mode (references use raw URLs)."
                .yellow()
        );
    }
    let root_dir = common::ROOT_DIR.get_required()?;
    let visual_tests_dir = root_dir.join("visual_tests");

    // Load workspace config
    let workspace_config = config::load_workspace_config(root_dir)?;
    validate_workspace_config(&workspace_config)?;

    let available = discover_test_suites(&visual_tests_dir)?;
    let filtered = filter_test_suites(available, &test_suites)?;
    let filtered = filter_scenario_suites(filtered, &scenarios)?;

    let regressify_dir = common::REGRESSIFY_DIR.get_required()?;

    // Parse all test suites upfront. YAML/config errors are infrastructure
    // errors and must fail before scenarios start.
    let parsed_suites = parse_test_suites(&visual_tests_dir, filtered)?;
    validate_scenario_filters(&parsed_suites, &scenarios)?;
    validate_suite_capture_keys(&workspace_config, &parsed_suites)?;
    validate_external_capture_files(root_dir, &workspace_config, &parsed_suites)?;
    ensure_capture_run_directories_are_new(common::REGRESSIFY_DIR.get_required()?, &parsed_suites)?;

    let workspace_config = Arc::new(workspace_config);
    let mut suite_runs = begin_suite_runs(regressify_dir, &parsed_suites, &scenarios)?;

    write_capture_run_manifests(
        workspace_config.as_ref(),
        &parsed_suites,
        &[],
        false,
        &scenarios,
    )?;

    let jobs = build_capture_jobs(&parsed_suites, workspace_config.as_ref(), &scenarios)?;

    let exe_path = Some(prepare_browser(fetch_browser).await?);

    // Run all scenarios across all suites with a global concurrency limit
    let limit = common::ASYNC_CAPTURE_LIMIT.get().copied().unwrap_or(5);
    let semaphore = create_ramped_semaphore(limit);
    let exe_path = Arc::new(exe_path);
    let active_replacements = Arc::new(Vec::<UrlReplacement>::new());

    // Flatten every scenario/viewport pair into an independent capture job (own
    // browser, own dedicated index) and spawn them — matching the TS/BackstopJS
    // model where pairs are scaffolded upfront and run independently.
    let handles = spawn_capture_jobs(
        jobs,
        &semaphore,
        &exe_path,
        &workspace_config,
        &active_replacements,
        false,
    );

    // Wait for all scenarios to complete. Only infrastructure failures (browser
    // launch) fail the command; page-level failures are reported but tolerated.
    let mut page_failures = 0usize;
    let mut infra_failures = 0usize;
    for handle in handles {
        match handle.await {
            Ok(TaskStatus::Passed) => {}
            Ok(TaskStatus::PageFailed) => page_failures += 1,
            Ok(TaskStatus::InfraFailed) => infra_failures += 1,
            Err(e) => {
                // A panicked task is a bug in our code, not the page under test.
                eprintln!("Task panicked: {:?}", e);
                infra_failures += 1;
            }
        }
    }

    if page_failures > 0 {
        eprintln!(
            "{} scenario(s) failed due to page errors (e.g. timeout, not found). \
             No reference screenshot was captured for them; the run continues.",
            page_failures.to_string().yellow()
        );
    }

    // Only infrastructure failures (browser could not be launched) fail the
    // command. Page errors must not produce a non-zero exit.
    if infra_failures > 0 {
        anyhow::bail!(
            "Reference capture failed: {} scenario(s) had browser or artifact failures",
            infra_failures
        );
    }

    // Promote screenshots and run comparison per suite
    for (input, suite) in &parsed_suites {
        let suite_id = suite
            .id
            .as_deref()
            .ok_or_else(|| anyhow!("Test suite '{}' is missing an ID", input.name))?;
        let suite_dir = suite_output_dir(regressify_dir, suite)?;
        let tests_dir = suite_dir.join("bitmaps_test");

        if !tests_dir.exists() {
            eprintln!("No test screenshots found for '{}', skipping.", input.name);
            continue;
        }

        let current_run = suite
            .test_date
            .as_deref()
            .ok_or_else(|| anyhow!("Test suite '{}' is missing a capture run ID", input.name))?;
        let latest_test_dir = current_capture_run_dir(&tests_dir, suite)?;
        let screenshots = validate_capture_run_inventory(&latest_test_dir).map_err(|error| {
            anyhow!(
                "Failed to validate reference capture inventory for '{}': {}",
                input.name,
                error
            )
        })?;
        let (count, skipped_failed_count) = promote_reference_captures(
            regressify_dir,
            &latest_test_dir,
            &suite_dir.join("bitmaps_reference"),
            &screenshots,
        )?;

        eprintln!(
            "Captured {} reference screenshot(s) for '{}' (run: {}).",
            count.to_string().green(),
            input.name.cyan(),
            current_run
        );
        if skipped_failed_count > 0 {
            eprintln!(
                "Skipped {} failed capture placeholder(s) for '{}'. They remain report-only and were not promoted to references.",
                skipped_failed_count.to_string().yellow(),
                input.name.cyan()
            );
        }

        // Write the self-approved baseline report directly without comparing
        // against another image. Successful captures were just promoted to
        // references; failed captures retain their error markers.
        generate_reference_report(&suite_dir, suite_id, current_run).map_err(|error| {
            anyhow!(
                "Failed to generate reference report for '{}': {}",
                input.name,
                error
            )
        })?;
        if let Some(mut run) = suite_runs.remove(&input.name) {
            run.finish(RunState::Completed, true)?;
        }
    }

    drop(suite_runs);
    generate_snapshot_reports()?;

    Ok(())
}

/// Promote a reference run's successful captures and return the promoted and
/// skipped counts. Each PNG is copied atomically before its sidecar link is
/// rewritten; the link records the image digest, so its archive reference and
/// image simplification setting only ever describe the promoted bytes. Failed
/// placeholders are never promoted, and their old references stay untouched.
fn promote_reference_captures(
    output_dir: &Path,
    run_dir: &Path,
    reference_dir: &Path,
    screenshots: &[String],
) -> Result<(usize, usize)> {
    fs::create_dir_all(reference_dir)?;
    let mut promoted = 0;
    let mut skipped = 0;
    for file_name in screenshots {
        let source = run_dir.join(file_name);
        if capture::error_marker_path(&source).exists() {
            skipped += 1;
            eprintln!(
                "    {} Skipping failed capture: {}",
                "SKIP:".yellow(),
                file_name
            );
            continue;
        }
        let destination = reference_dir.join(file_name);
        copy_file_atomically(&source, &destination)?;
        capture::promote_capture_log(output_dir, &source, &destination);
        let marker = capture::error_marker_path(&destination);
        if marker.exists() {
            fs::remove_file(marker)?;
        }
        promoted += 1;
    }
    Ok((promoted, skipped))
}

fn generate_snapshot_reports() -> Result<()> {
    progress::emit(ProgressEvent::Phase {
        name: "Generating report".into(),
    });
    snapshot::snapshot(SnapshotArgs::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_run_completion_survives_an_interrupted_peer() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let completed = RunLease::begin(&first, "run", true).unwrap();
        let interrupted = RunLease::begin(&second, "run", true).unwrap();
        finalize_test_suite(completed, None, false, None).unwrap();
        assert_eq!(
            crate::run_history::latest_completed_capture_run(&first.join("bitmaps_test"))
                .unwrap()
                .as_deref(),
            Some("run")
        );
        assert_eq!(
            crate::run_history::latest_completed_capture_run(&second.join("bitmaps_test")).unwrap(),
            None
        );
        drop(interrupted);
        assert_eq!(
            crate::run_history::capture_runs(&second.join("bitmaps_test")).unwrap()[0].state,
            RunState::Interrupted
        );
    }

    #[test]
    fn suite_run_completion_rejects_infrastructure_and_missing_captures() {
        let temp = tempfile::tempdir().unwrap();
        let suite = temp.path().join("suite");
        let run = RunLease::begin(&suite, "infrastructure", true).unwrap();
        finalize_test_suite(run, None, true, None).unwrap();
        let run = RunLease::begin(&suite, "missing", true).unwrap();
        fs::write(
            suite
                .join("bitmaps_test/missing")
                .join(capture::REPORT_ORDER_FILE),
            serde_json::to_vec(&capture::ReportOrderManifest {
                version: capture::REPORT_ORDER_VERSION,
                captures: vec!["home__FINAL".into()],
            })
            .unwrap(),
        )
        .unwrap();
        assert!(finalize_test_suite(run, None, false, None).is_err());
        assert_eq!(
            crate::run_history::latest_completed_capture_run(&suite.join("bitmaps_test")).unwrap(),
            None
        );
        assert!(
            crate::run_history::capture_runs(&suite.join("bitmaps_test"))
                .unwrap()
                .iter()
                .all(|run| run.state == RunState::Failed)
        );
    }

    #[test]
    fn run_retention_inherits_workspace_and_honors_suite_overrides() {
        let mut workspace = config::WorkspaceConfig::default();
        let mut suite = TestSuite::default();
        assert_eq!(config::resolve_run_retention(&workspace, &suite), None);
        workspace.auto_clean_runs = Some(true);
        assert_eq!(config::resolve_run_retention(&workspace, &suite), Some(1));
        workspace.keep_test_runs = Some(3);
        assert_eq!(config::resolve_run_retention(&workspace, &suite), Some(3));
        suite.keep_test_runs = Some(2);
        assert_eq!(config::resolve_run_retention(&workspace, &suite), Some(2));
        suite.auto_clean_runs = Some(false);
        assert_eq!(config::resolve_run_retention(&workspace, &suite), None);
        workspace.keep_test_runs = Some(0);
        assert!(
            validate_workspace_config(&workspace)
                .unwrap_err()
                .to_string()
                .contains("keepTestRuns")
        );
        suite.id = Some("shop".into());
        suite.keep_test_runs = Some(0);
        let input = TestSuiteInput {
            name: "shop".into(),
            path: "shop.tests.yaml".into(),
            format: TestSuiteFormat::Yaml,
        };
        assert!(
            validate_test_suite_config(&input, &suite)
                .unwrap_err()
                .to_string()
                .contains("keepTestRuns")
        );
    }

    #[test]
    fn run_retention_runs_only_after_full_suite_completion() {
        let temp = tempfile::tempdir().unwrap();
        let suite = temp.path().join("shop");
        let old = RunLease::begin(&suite, "old", true).unwrap();
        finalize_test_suite(old, None, false, None).unwrap();
        let partial = RunLease::begin(&suite, "partial", false).unwrap();
        finalize_test_suite(partial, None, false, Some(1)).unwrap();
        assert!(suite.join("bitmaps_test/old").is_dir());
        assert!(suite.join("bitmaps_test/partial").is_dir());
        let complete = RunLease::begin(&suite, "complete", true).unwrap();
        finalize_test_suite(complete, None, false, Some(1)).unwrap();
        assert!(!suite.join("bitmaps_test/old").exists());
        assert!(!suite.join("bitmaps_test/partial").exists());
        assert!(suite.join("bitmaps_test/complete").is_dir());
    }

    #[test]
    fn run_retention_failure_does_not_change_completed_results() {
        let temp = tempfile::tempdir().unwrap();
        let suite = temp.path().join("shop");
        let old = RunLease::begin(&suite, "old", true).unwrap();
        finalize_test_suite(old, None, false, None).unwrap();
        fs::write(suite.join(".compare-staging-invalid"), "not a directory").unwrap();
        let complete = RunLease::begin(&suite, "complete", true).unwrap();
        finalize_test_suite(complete, None, false, Some(1)).unwrap();
        assert_eq!(
            crate::run_history::latest_completed_capture_run(&suite.join("bitmaps_test"))
                .unwrap()
                .as_deref(),
            Some("complete")
        );
        assert!(suite.join("bitmaps_test/old").is_dir());
    }

    #[test]
    fn capture_failure_detail_keeps_local_context_without_url_paths_or_credentials() {
        let error = anyhow!(
            "Timeout waiting for URL 'https://synthetic-user:synthetic-pass@shop.example/checkout?sig=abc', current URL is 'https://shop.example/private-login'"
        )
        .context("Wait failed\nwith token=synthetic-secret");
        let detail = capture_failure_detail(&error);
        assert!(
            detail.starts_with("Wait failed with [redacted]"),
            "{detail}"
        );
        assert!(detail.contains("Timeout waiting for URL"), "{detail}");
        assert!(
            detail.contains("'https://shop.example/[redacted]'"),
            "{detail}"
        );
        for secret in [
            "synthetic-user",
            "synthetic-pass",
            "checkout",
            "sig=abc",
            "synthetic-secret",
            "private-login",
            "\n",
        ] {
            assert!(!detail.contains(secret), "{detail}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queue_setup_stage_records_for_passed_and_timed_out_captures() {
        use crate::diagnostics::{
            journal::{ArchiveState, JournalReader},
            record::*,
            runtime,
        };
        let root = tempfile::tempdir().unwrap();
        for fails in [false, true] {
            let execution = ExecutionIdentity {
                execution_id: ExecutionId::new(),
                suite_id: SafeIdentity::fingerprint("suite"),
                scenario_id: SafeIdentity::fingerprint("scenario"),
                viewport: ViewportIdentity {
                    name: SafeIdentity::fingerprint("desktop"),
                    width: 1920,
                    height: 1080,
                },
                capture_role: CaptureRole::Test,
                attempt: 1,
            };
            let operation = runtime::Operation::start(
                root.path(),
                OperationKind::Capture,
                None,
                Some(execution),
                None,
            );
            let result = runtime::scope(
                Some(operation.context()),
                runtime::stage(
                    Stage::QueueSetup,
                    queue_setup_fields(&config::ResolvedConfig::default()),
                    async {
                        if fails {
                            anyhow::bail!("Synthetic setup timeout containing private-input");
                        }
                        Ok(())
                    },
                ),
            )
            .await;
            let reference = operation
                .finish(if result.is_ok() {
                    Outcome::Success
                } else {
                    Outcome::Failure
                })
                .await;
            assert_eq!(reference.state, ArchiveState::Complete);
            let reader = JournalReader::open(root.path(), &reference.operation_id).unwrap();
            assert_eq!(reader.metadata.build.version, env!("CARGO_PKG_VERSION"));
            assert!(!reader.metadata.build.source_commit.is_empty());
            let mut found = false;
            reader
                .visit(|record| {
                    assert!(
                        !serde_json::to_string(&record)
                            .unwrap()
                            .contains("private-input")
                    );
                    if record.stage == Stage::QueueSetup
                        && record.event_name == DiagnosticEvent::StageFinished
                    {
                        found = true;
                        assert_eq!(
                            record.outcome,
                            Some(if fails {
                                Outcome::Failure
                            } else {
                                Outcome::Success
                            })
                        );
                        assert!(record.execution.is_some());
                        for field in [
                            Field::ElementTimeoutMs,
                            Field::ActionabilityTimeoutMs,
                            Field::PollIntervalMs,
                            Field::ScenarioTimeoutMs,
                            Field::RetryLimit,
                            Field::QueueMs,
                        ] {
                            assert!(record.fields.contains_key(&field));
                        }
                        if fails {
                            assert_eq!(
                                record.error_cause.unwrap().category,
                                ErrorCategory::Timeout
                            );
                        }
                    }
                })
                .unwrap();
            assert!(found);
        }
    }

    #[test]
    fn capture_job_journal_opens_writes_and_finalizes_before_comparing() {
        use crate::diagnostics::{
            journal::{ArchiveState, JournalReader},
            record::{DiagnosticEvent, OperationId},
        };
        let root = match std::env::var_os("REGRESSIFY_CAPTURE_JOURNAL_FIXTURE") {
            Some(root) => PathBuf::from(root),
            None => {
                let root = tempfile::tempdir().unwrap();
                let result = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "test::test_suites::tests::capture_job_journal_opens_writes_and_finalizes_before_comparing", "--nocapture"])
                    .env("REGRESSIFY_CAPTURE_JOURNAL_FIXTURE", root.path()).output().unwrap();
                assert!(
                    result.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                return;
            }
        };
        common::ROOT_DIR.set(root.clone()).unwrap();
        common::REGRESSIFY_DIR.set(root.clone()).unwrap();
        crate::diagnostics::install::install_subscriber();
        let mut suite = suite(vec![scenario("home", None)]);
        suite.test_date = Some("run-journal".into());
        let jobs = build_capture_jobs(
            &[(input(), Arc::new(suite))],
            &config::WorkspaceConfig::default(),
            &[],
        )
        .unwrap();
        let semaphore = Arc::new(Semaphore::new(0));
        semaphore.close();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let handles = spawn_capture_jobs(
                jobs,
                &semaphore,
                &Arc::new(None),
                &Arc::new(config::WorkspaceConfig::default()),
                &Arc::new(Vec::new()),
                true,
            );
            for handle in handles {
                assert_eq!(handle.await.unwrap(), TaskStatus::InfraFailed);
            }
        });
        let entries = fs::read_dir(root.join("_diagnostics"))
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let operation =
            OperationId::try_from(entries[0].file_name().into_string().unwrap()).unwrap();
        let reader = JournalReader::open(&root, &operation).unwrap();
        assert_eq!(reader.archive_state(), ArchiveState::Complete);
        let mut records = Vec::new();
        reader.visit(|record| records.push(record)).unwrap();
        assert!(records.iter().any(
            |record| record.event_name == DiagnosticEvent::Queued && record.execution.is_some()
        ));
        assert!(
            records
                .iter()
                .any(|record| record.event_name == DiagnosticEvent::Finished
                    && record.execution.is_some())
        );
        let mut live = progress::LiveProgress::default();
        live.apply(progress::Record {
            version: 1,
            elapsed_ms: 0,
            event: ProgressEvent::Comparing {
                suite_id: "alloy".into(),
            },
        });
        assert_eq!(live.phase, "Comparing screenshots");
        assert_eq!(reader.archive_state(), ArchiveState::Complete);
    }

    #[test]
    fn capture_job_links_record_the_effective_image_simplification() {
        use serde_json::json;
        let root = match std::env::var_os("REGRESSIFY_SIMPLIFICATION_LINK_FIXTURE") {
            Some(root) => PathBuf::from(root),
            None => {
                let root = tempfile::tempdir().unwrap();
                let result = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "test::test_suites::tests::capture_job_links_record_the_effective_image_simplification",
                        "--nocapture",
                    ])
                    .env("REGRESSIFY_SIMPLIFICATION_LINK_FIXTURE", root.path())
                    .output()
                    .unwrap();
                // A filter matching no test would also exit successfully.
                let stdout = String::from_utf8_lossy(&result.stdout);
                assert!(
                    result.status.success() && stdout.contains("1 passed"),
                    "{}\n{}",
                    stdout,
                    String::from_utf8_lossy(&result.stderr)
                );
                return;
            }
        };
        // Real spawn path in an isolated process: global output configuration
        // is set once. The closed semaphore stops jobs before any browser starts,
        // but both link writes still run.
        common::ROOT_DIR.set(root.clone()).unwrap();
        common::REGRESSIFY_DIR.set(root.clone()).unwrap();
        let workspace = config::WorkspaceConfig {
            image_simplification: Some(crate::types::ImageSimplification::Pixelate),
            image_simplification_block_size: Some(6),
            image_simplification_exclude_selectors: Some(vec![
                "img[data-secret=\"marker\"]".into(),
            ]),
            ..Default::default()
        };
        let mut checkout = scenario("checkout", None);
        checkout.actions =
            Some(serde_json::from_value(json!([{ "save": "step" }, { "wait": 10 }])).unwrap());
        let mut sharp = scenario("sharp", None);
        sharp.image_simplification = Some(crate::types::ImageSimplification::None);
        let mut suite = suite(vec![checkout, sharp]);
        suite.test_date = Some("run-links".into());
        let run = root.join("alloy/bitmaps_test/run-links");
        fs::create_dir_all(&run).unwrap();
        let jobs = build_capture_jobs(&[(input(), Arc::new(suite))], &workspace, &[]).unwrap();
        let semaphore = Arc::new(Semaphore::new(0));
        semaphore.close();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let handles = spawn_capture_jobs(
                jobs,
                &semaphore,
                &Arc::new(None),
                &Arc::new(workspace.clone()),
                &Arc::new(Vec::new()),
                true,
            );
            for handle in handles {
                assert_eq!(handle.await.unwrap(), TaskStatus::InfraFailed);
            }
        });
        let pixelated = json!({
            "mode": "pixelate",
            "blockSize": 6,
            "minSize": 64,
            "excludeSelectorsDigest": crate::test::image_simplification::exclude_selectors_digest(
                &["img[data-secret=\"marker\"]".into()]
            ),
        });
        for (key, expected) in [
            ("checkout__$step", pixelated.clone()),
            ("checkout__FINAL", pixelated),
            ("sharp__FINAL", json!({ "mode": "none" })),
        ] {
            let text = fs::read_to_string(run.join(format!("{key}.log.json"))).unwrap();
            let link: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(link["imageSimplification"], expected, "{key}");
            assert!(link["archive"]["operationId"].is_string(), "{key}");
            assert!(!text.contains("marker"), "{key}: {text}");
        }
    }

    #[test]
    fn reference_promotion_carries_recorded_image_simplification_with_each_image() {
        use serde_json::json;
        use sha2::{Digest, Sha256};
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path();
        let run = output.join("shop/bitmaps_test/ref-run");
        let references = output.join("shop/bitmaps_reference");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(&references).unwrap();
        let png = |red: u8| lodepng::encode32(&[red, 20, 30, 255], 1, 1).unwrap();
        let link = |image: &Path, setting: Option<serde_json::Value>| {
            let mut value = json!({
                "state": "missing",
                "imageDigest": format!("{:x}", Sha256::digest(fs::read(image).unwrap())),
            });
            if let Some(setting) = setting {
                value["imageSimplification"] = setting;
            }
            fs::write(
                image.with_extension("log.json"),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
        };
        let pixelated = json!({
            "mode": "pixelate", "blockSize": 4, "minSize": 64,
            "excludeSelectorsDigest": crate::test::image_simplification::exclude_selectors_digest(&[]),
        });
        let none = json!({ "mode": "none" });
        // Fresh pixelated capture over an older unpixelated reference.
        fs::write(run.join("home__FINAL.png"), png(10)).unwrap();
        link(&run.join("home__FINAL.png"), Some(pixelated.clone()));
        fs::write(references.join("home__FINAL.png"), png(200)).unwrap();
        link(&references.join("home__FINAL.png"), Some(none.clone()));
        // New capture without a reference yet.
        fs::write(run.join("new__FINAL.png"), png(11)).unwrap();
        link(&run.join("new__FINAL.png"), Some(none.clone()));
        // Failed placeholder: the previous reference and its setting survive.
        fs::write(run.join("failed__FINAL.png"), png(12)).unwrap();
        link(&run.join("failed__FINAL.png"), Some(pixelated.clone()));
        fs::write(
            capture::error_marker_path(&run.join("failed__FINAL.png")),
            br#"{"error":true}"#,
        )
        .unwrap();
        fs::write(references.join("failed__FINAL.png"), png(200)).unwrap();
        link(&references.join("failed__FINAL.png"), Some(none.clone()));
        let screenshots: Vec<String> = ["home__FINAL.png", "new__FINAL.png", "failed__FINAL.png"]
            .map(String::from)
            .into();

        assert_eq!(
            promote_reference_captures(output, &run, &references, &screenshots).unwrap(),
            (2, 1)
        );

        let setting = |name: &str| {
            capture::read_capture_metadata(output, &references.join(name))
                .and_then(|metadata| metadata.image_simplification)
                .map(|setting| serde_json::to_value(setting).unwrap())
        };
        assert_eq!(
            fs::read(references.join("home__FINAL.png")).unwrap(),
            png(10)
        );
        assert_eq!(setting("home__FINAL.png"), Some(pixelated));
        assert_eq!(
            fs::read(references.join("new__FINAL.png")).unwrap(),
            png(11)
        );
        assert_eq!(setting("new__FINAL.png"), Some(none.clone()));
        assert_eq!(
            fs::read(references.join("failed__FINAL.png")).unwrap(),
            png(200)
        );
        assert_eq!(setting("failed__FINAL.png"), Some(none));
        // The promoted link is bound to the promoted bytes.
        let promoted: serde_json::Value =
            serde_json::from_slice(&fs::read(references.join("home__FINAL.log.json")).unwrap())
                .unwrap();
        assert_eq!(
            promoted["imageDigest"],
            format!("{:x}", Sha256::digest(png(10)))
        );
    }

    fn input() -> TestSuiteInput {
        TestSuiteInput {
            name: "alloy".to_string(),
            path: std::path::PathBuf::from("alloy.tests.yaml"),
            format: TestSuiteFormat::Yaml,
        }
    }

    fn suite(scenarios: Vec<TestScenario>) -> TestSuite {
        TestSuite {
            id: Some("alloy".to_string()),
            test_date: None,
            async_capture_limit: None,
            async_compare_limit: None,
            browser: None,
            bypass_csp: None,
            cookie_path: None,
            css_override_path: None,
            debug: None,
            hide_selectors: None,
            ignore_ssl_errors: None,
            js_on_ready_path: None,
            mis_match_threshold: None,
            post_interaction_wait: None,
            remove_selectors: None,
            scenarios,
            state: None,
            url_replacements: None,
            use_css_override: None,
            viewport_names: None,
            viewports_path: None,
            ..Default::default()
        }
    }

    fn scenario(id: &str, needs: Option<Needs>) -> TestScenario {
        TestScenario {
            actions: None,
            bypass_csp: None,
            click_selector: None,
            cookie_path: None,
            css_override_path: None,
            delay: None,
            description: None,
            hide_selectors: None,
            id: Some(id.to_string()),
            js_on_ready_path: None,
            label: None,
            mis_match_threshold: None,
            needs,
            no_scroll_top: None,
            post_interaction_wait: None,
            remove_selectors: None,
            required_login: None,
            restore: None,
            selectors: None,
            url: format!("https://example.com/{id}"),
            use_css_override: None,
            viewport_names: None,
            ..Default::default()
        }
    }

    fn named_viewports(labels: &[&str]) -> Vec<config::viewports::ViewportDef> {
        labels
            .iter()
            .map(|label| config::viewports::ViewportDef {
                label: (*label).to_string(),
                width: 800,
                height: 600,
            })
            .collect()
    }

    #[test]
    fn viewport_save_names_allow_disjoint_branches_but_reject_overlaps() {
        use serde_json::json;

        let root = match std::env::var_os("REGRESSIFY_VIEWPORT_COLLISION_FIXTURE") {
            Some(root) => PathBuf::from(root),
            None => {
                let workspace = tempfile::tempdir().unwrap();
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "test::test_suites::tests::viewport_save_names_allow_disjoint_branches_but_reject_overlaps", "--nocapture"])
                    .env("REGRESSIFY_VIEWPORT_COLLISION_FIXTURE", workspace.path())
                    .output().unwrap();
                assert!(
                    output.status.success(),
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
        };
        common::ROOT_DIR.set(root.clone()).unwrap();
        std::fs::create_dir_all(root.join("common")).unwrap();
        std::fs::write(root.join("common/_viewports.yaml"), "- label: desktop\n  width: 1280\n  height: 720\n- label: mobile\n  width: 375\n  height: 667\n").unwrap();
        let workspace = config::WorkspaceConfig::default();
        for (first, second, valid) in [
            (json!(["desktop"]), json!(["mobile"]), true),
            (json!(["DESKTOP", "desktop"]), json!(["Mobile"]), true),
            (json!(["desktop"]), json!(["desktop"]), false),
            (json!(["desktop", "mobile"]), json!(["mobile"]), false),
            (json!(null), json!(null), false),
            (json!([]), json!(["mobile"]), false),
            (json!(["desktop"]), json!(null), false),
        ] {
            let mut page = scenario("page", None);
            page.actions = Some(
                serde_json::from_value(json!([
                    { "save": "after-login", "viewportNames": first },
                    { "save": "After-Login", "viewportNames": second },
                ]))
                .unwrap(),
            );
            let parsed = [(input(), Arc::new(suite(vec![page])))];
            let candidate = &parsed[0].1;
            let result = validate_test_suite_config(&parsed[0].0, candidate)
                .and_then(|()| validate_suite_capture_keys(&workspace, &parsed));
            assert_eq!(result.is_ok(), valid, "{first} {second}: {result:?}");
            if valid {
                let keys = scenario_capture_order_keys(
                    &workspace,
                    candidate,
                    &candidate.scenarios[0],
                    &candidate.scenarios,
                )
                .unwrap();
                assert_eq!(
                    keys,
                    vec!["page_desktop__$after-login", "page_mobile__$After-Login"]
                );
                assert_eq!(
                    keys.iter()
                        .map(|key| key.to_ascii_lowercase())
                        .collect::<HashSet<_>>()
                        .len(),
                    2
                );
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("duplicate filename")
                );
            }
        }
        let mut page = scenario("page", None);
        page.actions = Some(
            serde_json::from_value(json!([{ "save": "../outside", "viewportNames": ["mobile"] }]))
                .unwrap(),
        );
        let invalid = suite(vec![page]);
        assert!(
            capture_progress_keys(&invalid.scenarios[0], &invalid.scenarios, Some("desktop"))
                .is_ok()
        );
        assert!(
            validate_test_suite_config(&input(), &invalid)
                .unwrap_err()
                .to_string()
                .contains("Invalid save name")
        );

        let mut base = scenario("base", None);
        base.actions = Some(
            serde_json::from_value(json!([{ "save": "after-login", "viewportNames": ["mobile"] }]))
                .unwrap(),
        );
        let mut consumer = scenario("consumer", Some(Needs::String("base".into())));
        consumer.actions = Some(
            serde_json::from_value(
                json!([{ "save": "after-login", "viewportNames": ["desktop"] }]),
            )
            .unwrap(),
        );
        let parsed = [(input(), Arc::new(suite(vec![base, consumer])))];
        validate_test_suite_config(&parsed[0].0, &parsed[0].1).unwrap();
        validate_suite_capture_keys(&workspace, &parsed).unwrap();
    }

    #[test]
    fn action_viewport_names_validate_the_consuming_set_before_browser_startup() {
        use serde_json::json;

        for (filter, labels, valid) in [
            (json!(null), vec!["desktop", "mobile"], true),
            (json!([]), vec!["desktop", "mobile"], true),
            (json!("DESKTOP"), vec!["desktop"], true),
            (json!("missing"), vec!["desktop", "mobile"], false),
            (json!(""), vec!["desktop", "mobile"], false),
            (json!(" \t"), vec!["desktop", "mobile"], false),
            (json!(" desktop"), vec!["desktop", "mobile"], false),
            (json!("mobile"), vec!["desktop"], false),
            (json!("desktop"), vec![], false),
            (json!(["DESKTOP", "desktop"]), vec!["desktop"], true),
            (
                json!(["mobile", "desktop"]),
                vec!["desktop", "mobile"],
                true,
            ),
            (json!(["missing"]), vec!["desktop", "mobile"], false),
            (json!([""]), vec!["desktop", "mobile"], false),
            (json!([" \t"]), vec!["desktop", "mobile"], false),
            (json!([" desktop"]), vec!["desktop", "mobile"], false),
            (json!(["mobile"]), vec!["desktop"], false),
            (json!(null), vec![], true),
            (json!([]), vec![], true),
            (json!(["desktop"]), vec![], false),
        ] {
            let mut consumer = scenario("consumer", None);
            consumer.actions = Some(serde_json::from_value(json!([
                { "wait": 0 },
                { "input": "#private-selector", "value": "private-value", "viewportNames": filter }
            ])).unwrap());
            let viewports = named_viewports(&labels);
            let result =
                validate_action_viewport_names("viewport-fixture", &consumer, &[], &viewports);
            if valid {
                result.unwrap();
            } else {
                let error = result.unwrap_err().to_string();
                for expected in [
                    "viewport-fixture",
                    "consumer",
                    "action #2",
                    "invalid name(s)",
                    "Available scenario viewport(s)",
                ] {
                    assert!(error.contains(expected), "{error}");
                }
                assert!(!error.contains("private-value") && !error.contains("private-selector"));
                if labels.is_empty() {
                    assert!(error.contains("<unnamed browser default>"));
                }
            }
        }
    }

    #[test]
    fn action_viewport_names_keep_nested_and_shared_prerequisite_origins() {
        let mut base = scenario("base", None);
        base.actions = Some(
            serde_yaml::from_str("- wait: 0\n- click: '#mobile'\n  viewportNames: mobile\n")
                .unwrap(),
        );
        let left = scenario("left", Some(Needs::String("base".into())));
        let right = scenario("right", Some(Needs::String("base".into())));
        let consumer = scenario(
            "consumer",
            Some(Needs::StringArray(vec!["left".into(), "right".into()])),
        );
        let suite = suite(vec![base, left, right, consumer]);
        validate_scenario_dependencies(&input(), &suite).unwrap();
        let consumer = &suite.scenarios[3];
        let sources = collect_needs_scenarios(consumer, &suite.scenarios, &mut Vec::new());
        assert_eq!(
            sources
                .iter()
                .map(|source| source.id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["base", "left", "right"]
        );
        let before = serde_json::to_value(&suite).unwrap();
        let error = validate_action_viewport_names(
            "viewport-fixture",
            consumer,
            &suite.scenarios,
            &named_viewports(&["desktop"]),
        )
        .unwrap_err()
        .to_string();
        for expected in [
            "viewport-fixture",
            "consumer",
            "prerequisite 'base' action #2",
            "mobile",
            "desktop",
        ] {
            assert!(error.contains(expected), "{error}");
        }
        validate_action_viewport_names(
            "viewport-fixture",
            consumer,
            &suite.scenarios,
            &named_viewports(&["desktop", "mobile"]),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&suite).unwrap(), before);
    }

    #[test]
    fn action_viewport_names_follow_the_existing_configuration_cascade() {
        use serde_json::json;

        let all = named_viewports(&["desktop", "mobile"]);
        for (workspace_names, suite_names, scenario_names, valid) in [
            (None, None, None, true),
            (Some(vec!["desktop"]), None, None, false),
            (Some(vec!["desktop"]), Some(vec!["mobile"]), None, true),
            (
                Some(vec!["mobile"]),
                Some(vec!["desktop"]),
                Some(vec!["mobile"]),
                true,
            ),
            (Some(vec!["mobile"]), None, Some(vec!["desktop"]), false),
            (Some(vec!["desktop"]), None, Some(vec![]), true),
        ] {
            let workspace: config::WorkspaceConfig =
                serde_json::from_value(json!({ "viewportNames": workspace_names })).unwrap();
            let mut suite = suite(vec![]);
            suite.viewport_names = serde_json::from_value(json!(suite_names)).unwrap();
            let mut consumer = scenario("consumer", None);
            consumer.viewport_names = serde_json::from_value(json!(scenario_names)).unwrap();
            consumer.actions = Some(
                serde_json::from_value(json!([{ "wait": 0, "viewportNames": ["mobile"] }]))
                    .unwrap(),
            );
            let resolved = config::resolve_config(&workspace, &suite, &consumer);
            let effective =
                config::viewports::resolve_viewports(resolved.viewport_names.as_deref(), &all)
                    .unwrap();
            assert_eq!(
                validate_action_viewport_names("viewport-fixture", &consumer, &[], &effective)
                    .is_ok(),
                valid
            );
        }
    }

    #[test]
    fn pixel_match_threshold_validates_every_configuration_level() {
        for (threshold, valid) in [
            (0.0, true),
            (0.1, true),
            (5.0, true),
            (100.0, true),
            (-1.0, false),
            (-0.01, false),
            (100.01, false),
            (f64::NAN, false),
            (f64::INFINITY, false),
            (f64::NEG_INFINITY, false),
        ] {
            let workspace = config::WorkspaceConfig {
                pixel_match_threshold: Some(threshold),
                ..Default::default()
            };
            let mut suite = suite(vec![scenario("home", None)]);
            suite.pixel_match_threshold = Some(threshold);
            let suite_result = validate_test_suite_config(&input(), &suite);
            suite.pixel_match_threshold = None;
            suite.scenarios[0].pixel_match_threshold = Some(threshold);

            for result in [
                validate_workspace_config(&workspace),
                suite_result,
                validate_test_suite_config(&input(), &suite),
            ] {
                if valid {
                    result.unwrap();
                } else {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("pixelMatchThreshold")
                    );
                }
            }
        }
    }

    #[test]
    fn parse_test_suites_allows_shared_urls_across_suites() {
        let temp_dir = tempfile::tempdir().unwrap();
        let inputs = ["first", "second"]
            .into_iter()
            .map(|name| {
                let path = PathBuf::from(format!("{name}.tests.yaml"));
                fs::write(
                    temp_dir.path().join(&path),
                    "scenarios:\n  - url: https://example.com/shared\n",
                )
                .unwrap();
                TestSuiteInput {
                    name: name.into(),
                    path,
                    format: TestSuiteFormat::Yaml,
                }
            })
            .collect();

        let suites = parse_test_suites(temp_dir.path(), inputs).unwrap();
        assert_eq!(suites.len(), 2);
        assert_eq!(suites[0].1.scenarios[0].id, suites[1].1.scenarios[0].id);
    }

    #[test]
    fn parse_test_suites_duplicate_ids_preserve_source_and_scenario_context() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = PathBuf::from("nested/suite.tests.yml");
        fs::create_dir_all(temp_dir.path().join("nested")).unwrap();
        fs::write(
            temp_dir.path().join(&path),
            "scenarios:\n  - url: https://example.com/shared\n  - url: https://example.com/shared\n",
        )
        .unwrap();

        let error = parse_test_suites(
            temp_dir.path(),
            vec![TestSuiteInput {
                name: "suite".into(),
                path,
                format: TestSuiteFormat::Yaml,
            }],
        )
        .err()
        .expect("Repeated unlabeled URLs must collide")
        .to_string();

        for expected in [
            "Error parsing test suite 'suite'",
            "Duplicate scenario ID",
            "nested",
            "suite.tests.yml",
            "Scenario #1",
            "Scenario #2",
            "https://example.com/shared",
            "unique explicit 'id'",
        ] {
            assert!(error.contains(expected), "Missing {expected:?}: {error}");
        }
    }

    #[test]
    fn scenario_selection_is_exact_and_preserves_dependency_definitions() {
        let root = match std::env::var_os("REGRESSIFY_SCENARIO_SELECTION_FIXTURE") {
            Some(path) => PathBuf::from(path),
            None => {
                let workspace = tempfile::tempdir().unwrap();
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "test::test_suites::tests::scenario_selection_is_exact_and_preserves_dependency_definitions", "--nocapture"])
                    .env("REGRESSIFY_SCENARIO_SELECTION_FIXTURE", workspace.path())
                    .output().unwrap();
                assert!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
        };
        common::ROOT_DIR.set(root.clone()).unwrap();
        common::REGRESSIFY_DIR
            .set(root.join(".regressify"))
            .unwrap();
        let mut selected_suite = suite(vec![
            scenario("base", None),
            scenario("page", Some(Needs::String("base".into()))),
            scenario("page-extra", None),
        ]);
        selected_suite.scenarios[0].url = "https://example.com/blog/page".into();
        selected_suite.scenarios[1].url = "https://example.com/docs/page".into();
        selected_suite.test_date = Some("selection".into());
        selected_suite.display_name = Some("Selected suite".into());
        let suite = Arc::new(selected_suite);
        let filters = vec!["alloy:page".parse().unwrap(), "alloy:page".parse().unwrap()];
        let suites = vec![(input(), suite.clone())];
        validate_scenario_filters(&suites, &filters).unwrap();
        let selected = selected_scenarios("alloy", &suite, &filters).collect::<Vec<_>>();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, 1);
        assert_eq!(selected[0].1.id.as_deref(), Some("page"));
        assert_eq!(suite.scenarios.len(), 3);
        validate_scenario_dependencies(&input(), &suite).unwrap();
        assert_eq!(selected_scenarios("alloy", &suite, &[]).count(), 3);
        assert_eq!(selected_scenarios("other", &suite, &filters).count(), 0);
        let config = config::WorkspaceConfig::default();
        let jobs = build_capture_jobs(&suites, &config, &filters).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].scenario_idx, 1);
        assert_eq!(jobs[0].index, 1);
        assert_eq!(jobs[0].suite.scenarios.len(), 3);
        let keys = selected_scenarios("alloy", &suite, &filters)
            .flat_map(|(_, scenario)| {
                scenario_capture_order_keys(&config, &suite, scenario, &suite.scenarios).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["page__FINAL"]);
        write_capture_run_manifests(&config, &suites, &[], false, &filters).unwrap();
        assert_eq!(
            crate::compare::report_gen::read_suite_metadata(&root.join(".regressify/alloy"))
                .unwrap()
                .unwrap()
                .display_name
                .as_deref(),
            Some("Selected suite")
        );
        let run = root.join(".regressify/alloy/bitmaps_test/selection");
        let order: capture::ReportOrderManifest =
            serde_json::from_slice(&fs::read(run.join(capture::REPORT_ORDER_FILE)).unwrap())
                .unwrap();
        assert_eq!(order.captures, keys);
        let urls: capture::ReportUrlsManifest =
            serde_json::from_slice(&fs::read(run.join(capture::REPORT_URLS_FILE)).unwrap())
                .unwrap();
        assert_eq!(urls.captures.len(), 1);
        assert_eq!(
            urls.captures["page__FINAL"].scenario_id.as_deref(),
            Some("page")
        );
        assert_eq!(
            urls.captures["page__FINAL"]
                .scenario_display_name
                .as_deref(),
            Some("Docs / Page")
        );
    }

    #[test]
    fn scenario_selection_rejects_unknown_and_ambiguous_targets() {
        let suites = vec![(input(), Arc::new(suite(vec![scenario("home", None)])))];
        for filter in ["alloy:missing", "other:home"] {
            assert!(validate_scenario_filters(&suites, &[filter.parse().unwrap()]).is_err());
        }
        let duplicate = vec![(
            input(),
            Arc::new(suite(vec![scenario("home", None), scenario("home", None)])),
        )];
        assert!(validate_scenario_filters(&duplicate, &["alloy:home".parse().unwrap()]).is_err());
        assert!(filter_scenario_suites(vec![input()], &["other:home".parse().unwrap()]).is_err());
        assert_eq!(
            filter_scenario_suites(vec![input()], &["alloy:home".parse().unwrap()])
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn validates_existing_dependencies() {
        let suite = suite(vec![
            scenario("base", None),
            scenario("page", Some(Needs::String("base".to_string()))),
        ]);

        validate_scenario_dependencies(&input(), &suite).unwrap();
    }

    #[test]
    fn report_page_urls_keep_reference_urls_and_apply_test_replacements() {
        let mut scenario = scenario("home", None);
        scenario.label = Some("Home page".into());
        let replacements = vec![UrlReplacement {
            ref_pattern: "https://example.com".to_string(),
            test: "https://staging.example.com".to_string(),
            regex: None,
            flags: None,
        }];

        let test_urls = report_page_urls(&scenario, &replacements, true).unwrap();
        let reference_urls = report_page_urls(&scenario, &replacements, false).unwrap();

        assert_eq!(test_urls.reference_url, "https://example.com/home");
        assert_eq!(test_urls.url, "https://staging.example.com/home");
        assert_eq!(reference_urls.reference_url, "https://example.com/home");
        assert_eq!(reference_urls.url, "https://example.com/home");
        assert_eq!(test_urls.scenario_id.as_deref(), Some("home"));
        assert_eq!(test_urls.scenario_label.as_deref(), Some("Home page"));
        scenario.label = None;
        assert_eq!(
            report_page_urls(&scenario, &replacements, true)
                .unwrap()
                .scenario_label
                .as_deref(),
            Some("https://example.com/home")
        );
    }

    #[test]
    fn rejects_missing_dependencies() {
        let suite = suite(vec![scenario(
            "page",
            Some(Needs::String("missing".to_string())),
        )]);

        let error = validate_scenario_dependencies(&input(), &suite).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("exactly one scenario with id: missing")
        );
    }

    #[test]
    fn rejects_circular_dependencies_with_path() {
        let suite = suite(vec![
            scenario("alpha", Some(Needs::String("beta".to_string()))),
            scenario("beta", Some(Needs::String("alpha".to_string()))),
        ]);

        let error = validate_scenario_dependencies(&input(), &suite).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Circular scenario dependency"));
        assert!(message.contains("alpha -> beta -> alpha"));
    }

    #[test]
    fn discover_test_suites_prefers_yaml_then_yml_then_json() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(temp_dir.path().join("alloy.tests.json"), "{}").unwrap();
        std::fs::write(temp_dir.path().join("alloy.tests.yml"), "scenarios: []\n").unwrap();
        std::fs::write(temp_dir.path().join("alloy.tests.yaml"), "scenarios: []\n").unwrap();

        let suites = discover_test_suites(temp_dir.path()).unwrap();

        assert_eq!(suites.len(), 1);
        assert_eq!(suites[0].name, "alloy");
        assert_eq!(
            suites[0].path.file_name().and_then(|name| name.to_str()),
            Some("alloy.tests.yaml")
        );
    }

    #[test]
    fn discover_test_suites_falls_back_to_yml_before_json() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(temp_dir.path().join("alloy.tests.json"), "{}").unwrap();
        std::fs::write(temp_dir.path().join("alloy.tests.yml"), "scenarios: []\n").unwrap();

        let suites = discover_test_suites(temp_dir.path()).unwrap();

        assert_eq!(suites.len(), 1);
        assert_eq!(
            suites[0].path.file_name().and_then(|name| name.to_str()),
            Some("alloy.tests.yml")
        );
    }

    #[test]
    fn discovery_rejects_same_priority_suites_with_the_same_name() {
        let temp_dir = tempfile::tempdir().unwrap();
        let first = temp_dir.path().join("first");
        let second = temp_dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("alloy.tests.yaml"), "scenarios: []\n").unwrap();
        std::fs::write(second.join("alloy.tests.yaml"), "scenarios: []\n").unwrap();

        let error = match discover_test_suites(temp_dir.path()) {
            Ok(_) => panic!("duplicate same-priority suites unexpectedly passed discovery"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("Ambiguous test suite 'alloy'"));
    }

    #[test]
    fn discovery_and_filtering_report_invalid_inputs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing_directory = temp_dir.path().join("missing");
        let discovery = discover_test_suites(&missing_directory);
        assert!(
            matches!(discovery, Err(error) if error.to_string().contains("Failed to discover test suites"))
        );

        let filtering = filter_test_suites(Vec::new(), &["[".to_string()]);
        assert!(
            matches!(filtering, Err(error) if error.to_string().contains("Invalid test suite pattern"))
        );
    }

    #[test]
    fn rejects_zero_async_compare_limit_before_capture() {
        let workspace = config::WorkspaceConfig {
            async_compare_limit: Some(0),
            ..Default::default()
        };
        assert!(
            validate_workspace_config(&workspace)
                .unwrap_err()
                .to_string()
                .contains("asyncCompareLimit")
        );

        let mut suite = suite(vec![scenario("home", None)]);
        suite.async_compare_limit = Some(0);
        assert!(
            validate_test_suite_config(&input(), &suite)
                .unwrap_err()
                .to_string()
                .contains("asyncCompareLimit")
        );
    }

    #[test]
    fn rejects_zero_async_capture_limit_before_browser_launch() {
        let workspace = config::WorkspaceConfig {
            async_capture_limit: Some(0),
            ..Default::default()
        };
        assert!(
            validate_workspace_config(&workspace)
                .unwrap_err()
                .to_string()
                .contains("asyncCaptureLimit")
        );

        let mut suite = suite(vec![scenario("home", None)]);
        suite.async_capture_limit = Some(0);
        assert!(
            validate_test_suite_config(&input(), &suite)
                .unwrap_err()
                .to_string()
                .contains("asyncCaptureLimit")
        );
    }

    #[test]
    fn lazy_load_timeouts_are_validated_at_every_scope() {
        let workspace = config::WorkspaceConfig {
            lazy_load_timeout: Some(0),
            no_scroll_top: Some(true),
            ..Default::default()
        };
        assert!(
            validate_workspace_config(&workspace)
                .unwrap_err()
                .to_string()
                .contains("lazyLoadTimeout")
        );

        let mut suite = suite(vec![scenario("home", None)]);
        suite.lazy_load_timeout = Some(0);
        suite.scenarios[0].lazy_load_timeout = Some(1000);
        assert!(
            validate_test_suite_config(&input(), &suite)
                .unwrap_err()
                .to_string()
                .contains("lazyLoadTimeout")
        );
        suite.lazy_load_timeout = Some(1000);
        suite.scenarios[0].lazy_load_timeout = Some(0);
        assert!(
            validate_test_suite_config(&input(), &suite)
                .unwrap_err()
                .to_string()
                .contains("lazyLoadTimeout")
        );

        suite.scenarios[0].lazy_load_timeout = Some(1);
        suite.scenarios[0].lazy_load_scroll_delay = Some(0);
        suite.lazy_load_scroll_delay = Some(0);
        validate_test_suite_config(&input(), &suite).unwrap();
        validate_workspace_config(&config::WorkspaceConfig {
            lazy_load_timeout: Some(1),
            lazy_load_scroll_delay: Some(0),
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn image_simplification_settings_are_validated_at_every_scope_before_capture() {
        let blank = || Some(vec![".logo".to_string(), " ".to_string()]);
        // Invalid workspace values fail even while the mode stays `none`.
        for (workspace, key) in [
            (
                config::WorkspaceConfig {
                    image_simplification_block_size: Some(5),
                    ..Default::default()
                },
                "workspace imageSimplificationBlockSize",
            ),
            (
                config::WorkspaceConfig {
                    image_simplification_block_size: Some(18),
                    image_simplification: Some(crate::types::ImageSimplification::Pixelate),
                    ..Default::default()
                },
                "workspace imageSimplificationBlockSize",
            ),
            (
                config::WorkspaceConfig {
                    image_simplification_min_size: Some(0),
                    ..Default::default()
                },
                "workspace imageSimplificationMinSize",
            ),
            (
                config::WorkspaceConfig {
                    image_simplification_exclude_selectors: blank(),
                    ..Default::default()
                },
                "workspace imageSimplificationExcludeSelectors entry #2",
            ),
        ] {
            let error = validate_workspace_config(&workspace)
                .unwrap_err()
                .to_string();
            assert!(error.contains(key), "{error}");
        }

        // Suite values are checked even when every scenario overrides them.
        let mut suite = suite(vec![scenario("home", None)]);
        suite.scenarios[0].image_simplification_block_size = Some(4);
        suite.scenarios[0].image_simplification_min_size = Some(64);
        suite.scenarios[0].image_simplification_exclude_selectors = Some(Vec::new());
        for (block_size, min_size, selectors, key) in [
            (Some(3), None, None, "imageSimplificationBlockSize"),
            (Some(0), None, None, "imageSimplificationBlockSize"),
            (None, Some(0), None, "imageSimplificationMinSize"),
            (None, None, blank(), "imageSimplificationExcludeSelectors"),
        ] {
            suite.image_simplification_block_size = block_size;
            suite.image_simplification_min_size = min_size;
            suite.image_simplification_exclude_selectors = selectors;
            let error = validate_test_suite_config(&input(), &suite)
                .unwrap_err()
                .to_string();
            assert!(
                error.starts_with(&format!("Test suite 'alloy' {key}")),
                "{error}"
            );
        }
        suite.image_simplification_block_size = Some(16);
        suite.image_simplification_min_size = Some(1);
        suite.image_simplification_exclude_selectors = Some(vec!["#hero img".into()]);
        validate_test_suite_config(&input(), &suite).unwrap();

        for (block_size, min_size, selectors, key) in [
            (Some(1), None, None, "imageSimplificationBlockSize"),
            (Some(17), None, None, "imageSimplificationBlockSize"),
            (None, Some(0), None, "imageSimplificationMinSize"),
            (
                None,
                None,
                Some(vec![String::new()]),
                "imageSimplificationExcludeSelectors",
            ),
        ] {
            let scenario = &mut suite.scenarios[0];
            scenario.image_simplification_block_size = block_size;
            scenario.image_simplification_min_size = min_size;
            scenario.image_simplification_exclude_selectors = selectors;
            let error = validate_test_suite_config(&input(), &suite)
                .unwrap_err()
                .to_string();
            assert!(
                error.starts_with(&format!("Test suite 'alloy' scenario 'home' {key}")),
                "{error}"
            );
        }
        let scenario = &mut suite.scenarios[0];
        scenario.image_simplification = Some(crate::types::ImageSimplification::None);
        scenario.image_simplification_block_size = Some(2);
        scenario.image_simplification_min_size = Some(u32::MAX);
        scenario.image_simplification_exclude_selectors = Some(Vec::new());
        validate_test_suite_config(&input(), &suite).unwrap();
        validate_workspace_config(&config::WorkspaceConfig {
            image_simplification: Some(crate::types::ImageSimplification::Pixelate),
            image_simplification_block_size: Some(4),
            image_simplification_min_size: Some(64),
            image_simplification_exclude_selectors: Some(vec![".logo".into()]),
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn invalid_image_simplification_in_a_suite_file_fails_parsing_before_capture() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = PathBuf::from("gallery.tests.yaml");
        fs::write(
            temp_dir.path().join(&path),
            "imageSimplification: pixelate\nscenarios:\n  - id: home\n    url: https://example.com\n    imageSimplificationBlockSize: 7\n",
        )
        .unwrap();
        let error = parse_test_suites(
            temp_dir.path(),
            vec![TestSuiteInput {
                name: "gallery".into(),
                path: path.clone(),
                format: TestSuiteFormat::Yaml,
            }],
        )
        .err()
        .expect("Odd block sizes must fail preflight")
        .to_string();
        assert!(
            error.contains(
                "scenario 'home' imageSimplificationBlockSize must be an even whole number"
            ),
            "{error}"
        );

        fs::write(
            temp_dir.path().join(&path),
            "imageSimplification: pixelate\nimageSimplificationExcludeSelectors: ['.logo']\nscenarios:\n  - id: home\n    url: https://example.com\n    imageSimplificationBlockSize: 8\n",
        )
        .unwrap();
        let suites = parse_test_suites(
            temp_dir.path(),
            vec![TestSuiteInput {
                name: "gallery".into(),
                path,
                format: TestSuiteFormat::Yaml,
            }],
        )
        .unwrap();
        let resolved = config::resolve_config(
            &config::WorkspaceConfig::default(),
            &suites[0].1,
            &suites[0].1.scenarios[0],
        );
        assert_eq!(
            resolved.image_simplification,
            crate::types::ImageSimplification::Pixelate
        );
        assert_eq!(resolved.image_simplification_block_size, 8);
        assert_eq!(resolved.image_simplification_min_size, 64);
        assert_eq!(
            resolved.image_simplification_exclude_selectors,
            vec![".logo"]
        );
    }

    #[test]
    fn rejects_zero_operational_timeouts_before_browser_launch() {
        for workspace in [
            config::WorkspaceConfig {
                element_timeout: Some(0),
                ..Default::default()
            },
            config::WorkspaceConfig {
                actionability_timeout: Some(0),
                ..Default::default()
            },
            config::WorkspaceConfig {
                scenario_timeout: Some(0),
                ..Default::default()
            },
        ] {
            assert!(
                validate_workspace_config(&workspace)
                    .unwrap_err()
                    .to_string()
                    .contains("must be greater than zero")
            );
        }
    }

    #[test]
    fn rejects_invalid_visual_thresholds_and_timing_values() {
        let invalid_workspace = config::WorkspaceConfig {
            mis_match_threshold: Some(100.1),
            ..Default::default()
        };
        assert!(
            validate_workspace_config(&invalid_workspace)
                .unwrap_err()
                .to_string()
                .contains("between 0 and 100")
        );

        let mut suite_with_negative_wait = suite(vec![scenario("home", None)]);
        suite_with_negative_wait.post_interaction_wait = Some(-1.0);
        assert!(
            validate_test_suite_config(&input(), &suite_with_negative_wait)
                .unwrap_err()
                .to_string()
                .contains("greater than or equal to zero")
        );

        let mut suite_with_non_finite_delay = suite(vec![scenario("home", None)]);
        suite_with_non_finite_delay.scenarios[0].delay = Some(f64::NAN);
        assert!(
            validate_test_suite_config(&input(), &suite_with_non_finite_delay)
                .unwrap_err()
                .to_string()
                .contains("finite value")
        );
    }

    #[tokio::test]
    async fn suite_capture_limit_blocks_only_its_own_jobs() {
        let global = Arc::new(Semaphore::new(2));
        let suite_a = Arc::new(Semaphore::new(1));
        let suite_b = Arc::new(Semaphore::new(1));
        let first_a = acquire_capture_permits(suite_a.clone(), global.clone())
            .await
            .unwrap();

        assert!(
            tokio::time::timeout(
                Duration::from_millis(1),
                acquire_capture_permits(suite_a, global.clone()),
            )
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(1),
                acquire_capture_permits(suite_b, global),
            )
            .await
            .is_ok()
        );

        drop(first_a);
    }

    #[test]
    fn rejects_empty_selector_capture_lists_before_browser_launch() {
        let mut suite = suite(vec![scenario("home", None)]);
        suite.scenarios[0].selectors = Some(crate::types::Selectors::StringArray(Vec::new()));

        let error = validate_test_suite_config(&input(), &suite).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Invalid selectors for scenario 'home'")
        );
        assert!(error.to_string().contains("at least one capture target"));
    }

    #[test]
    fn rejects_action_field_combinations_before_browser_launch() {
        use serde_json::json;
        let action = |value: serde_json::Value| -> crate::types::TestAction {
            serde_json::from_value(value).unwrap()
        };
        for (invalid, expected) in [
            (
                json!({"input": "#name", "value": "Ada", "file": "a.txt"}),
                "must not contain both `value` and `file`",
            ),
            (json!({"input": "#name"}), "either `value` or `file`"),
            (
                json!({"select": "#country", "value": "se", "label": "Sweden"}),
                "not both",
            ),
            (json!({"select": "#country"}), "either `value` or `label`"),
            (
                json!({"press": "#search", "key": "enter"}),
                "Unknown keyboard key 'enter'. Key names are case-sensitive",
            ),
            (json!({"persist": "../escape"}), "Invalid state name"),
            (
                json!({"switchTab": "previous", "url": "**/quote"}),
                "only supported with switchTab: new",
            ),
        ] {
            let mut suite = suite(vec![scenario("home", None)]);
            suite.scenarios[0].actions =
                Some(vec![action(json!({"click": "#ok"})), action(invalid)]);
            let error = validate_test_suite_config(&input(), &suite)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("Invalid action #2 in test suite 'alloy' scenario 'home'"),
                "{error}"
            );
            assert!(error.contains(expected), "{error}");
        }

        let mut valid = suite(vec![scenario("home", None)]);
        valid.scenarios[0].actions = Some(vec![
            action(json!({"input": "#name", "value": "Ada"})),
            action(json!({"select": "#country", "label": "Sweden"})),
            action(json!({"press": "#search", "key": "Enter"})),
            action(json!({"persist": "signed-in"})),
            action(json!({"switchTab": "new", "url": "**/quote"})),
        ]);
        validate_test_suite_config(&input(), &valid).unwrap();
    }

    #[test]
    fn upload_files_are_validated_before_browser_launch() {
        let workspace_root = tempfile::tempdir().unwrap();
        let output_dir = workspace_root.path().join(".regressify");
        std::fs::create_dir_all(workspace_root.path().join("visual_tests/files")).unwrap();
        std::fs::write(workspace_root.path().join("visual_tests/files/a.txt"), "a").unwrap();
        let mut configured = suite(vec![scenario("home", None)]);
        configured.scenarios[0].actions = Some(
            serde_json::from_value(serde_json::json!([
                {"input": "#upload", "file": "files/a.txt"}
            ]))
            .unwrap(),
        );
        let workspace = config::WorkspaceConfig::default();
        validate_external_capture_files_in(
            workspace_root.path(),
            &output_dir,
            &workspace,
            &[(input(), Arc::new(configured.clone()))],
        )
        .unwrap();

        configured.scenarios[0].actions = Some(
            serde_json::from_value(serde_json::json!([
                {"input": "#upload", "file": ["files/a.txt", "files/missing.txt"]}
            ]))
            .unwrap(),
        );
        let error = validate_external_capture_files_in(
            workspace_root.path(),
            &output_dir,
            &workspace,
            &[(input(), Arc::new(configured))],
        )
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("Invalid input file for test suite 'alloy' scenario 'home'"));
        assert!(
            error.contains("file does not exist: files/missing.txt"),
            "{error}"
        );
    }

    #[test]
    fn suite_output_directory_uses_configured_suite_id() {
        let mut suite = suite(vec![scenario("home", None)]);
        suite.id = Some("production-dashboard".to_string());
        let temp = tempfile::tempdir().unwrap();

        let output = suite_output_dir(temp.path(), &suite).unwrap();

        assert_eq!(output, temp.path().join("production-dashboard"));
    }

    #[test]
    fn current_capture_run_directory_uses_suite_run_id_not_latest_directory() {
        let temp = tempfile::tempdir().unwrap();
        let tests_dir = temp.path().join("bitmaps_test");
        let mut suite = suite(vec![scenario("home", None)]);
        suite.test_date = Some("20260913_120000_000000001".to_string());
        std::fs::create_dir_all(tests_dir.join(suite.test_date.as_deref().unwrap())).unwrap();
        std::fs::create_dir_all(tests_dir.join("99991231_235959_999999999")).unwrap();

        assert_eq!(
            current_capture_run_dir(&tests_dir, &suite).unwrap(),
            tests_dir.join("20260913_120000_000000001")
        );
    }

    #[test]
    fn capture_commands_reject_reusing_an_existing_run_directory() {
        let temp = tempfile::tempdir().unwrap();
        let mut suite = suite(vec![scenario("home", None)]);
        suite.test_date = Some("ci-123".to_string());
        let parsed = vec![(input(), Arc::new(suite))];
        std::fs::create_dir_all(temp.path().join("alloy/bitmaps_test/ci-123")).unwrap();

        let error = ensure_capture_run_directories_are_new(temp.path(), &parsed).unwrap_err();
        assert!(error.to_string().contains("already exists"));

        std::fs::remove_dir_all(temp.path().join("alloy/bitmaps_test/ci-123")).unwrap();
        assert!(ensure_capture_run_directories_are_new(temp.path(), &parsed).is_ok());
    }

    #[test]
    fn suite_capture_keys_reject_viewport_and_case_insensitive_collisions() {
        let error = validate_unique_capture_keys(
            "storefront",
            vec![
                (
                    "home_mobile__FINAL".to_string(),
                    "home at mobile".to_string(),
                ),
                (
                    "home_mobile__FINAL".to_string(),
                    "home_mobile default viewport".to_string(),
                ),
            ],
        )
        .unwrap_err();
        assert!(error.to_string().contains("colliding screenshot filename"));

        let error = validate_unique_capture_keys(
            "storefront",
            vec![
                ("Home__FINAL".to_string(), "Home".to_string()),
                ("home__final".to_string(), "home".to_string()),
            ],
        )
        .unwrap_err();
        assert!(error.to_string().contains("scenarios 'Home' and 'home'"));
    }

    #[test]
    fn rejects_unsafe_suite_and_scenario_output_identifiers() {
        let mut unsafe_suite = suite(vec![scenario("home", None)]);
        unsafe_suite.id = Some("../outside".to_string());
        assert!(
            validate_test_suite_config(&input(), &unsafe_suite)
                .unwrap_err()
                .to_string()
                .contains("Test suite ID")
        );

        let mut unsafe_scenario_suite = suite(vec![scenario("home", None)]);
        unsafe_scenario_suite.scenarios[0].id = Some("nested/home".to_string());
        assert!(
            validate_test_suite_config(&input(), &unsafe_scenario_suite)
                .unwrap_err()
                .to_string()
                .contains("Scenario ID")
        );

        let mut long_identifier_suite = suite(vec![scenario("home", None)]);
        long_identifier_suite.id = Some("a".repeat(MAX_OUTPUT_IDENTIFIER_BYTES + 1));
        assert!(
            validate_test_suite_config(&input(), &long_identifier_suite)
                .unwrap_err()
                .to_string()
                .contains("no longer than")
        );

        for invalid_id in [
            "suite.",
            "suite ",
            "suite\nname",
            "CON",
            "LPT1.txt",
            ".checkout",
            "all",
            "_snapshot",
            "_diagnostics",
            "_landing",
            "states",
        ] {
            let mut invalid_id_suite = suite(vec![scenario("home", None)]);
            invalid_id_suite.id = Some(invalid_id.to_string());
            assert!(
                validate_test_suite_config(&input(), &invalid_id_suite).is_err(),
                "{invalid_id}"
            );
            let invalid_name_input = TestSuiteInput {
                name: invalid_id.to_string(),
                path: PathBuf::from(format!("{invalid_id}.tests.yaml")),
                format: TestSuiteFormat::Yaml,
            };
            let valid_suite = suite(vec![scenario("home", None)]);
            assert!(
                validate_test_suite_config(&invalid_name_input, &valid_suite).is_err(),
                "{invalid_id}"
            );
        }

        let mut reserved_scenario_suite = suite(vec![scenario("home", None)]);
        reserved_scenario_suite.scenarios[0].id = Some("AUX".to_string());
        assert!(
            validate_test_suite_config(&input(), &reserved_scenario_suite)
                .unwrap_err()
                .to_string()
                .contains("Scenario ID")
        );
    }

    #[test]
    fn rejects_suites_with_case_insensitive_output_id_collisions() {
        let mut first = suite(vec![scenario("home", None)]);
        first.id = Some("Production".to_string());
        let mut second = suite(vec![scenario("home", None)]);
        second.id = Some("production".to_string());
        let inputs = vec![
            (
                TestSuiteInput {
                    name: "first".to_string(),
                    path: PathBuf::from("first.tests.yaml"),
                    format: TestSuiteFormat::Yaml,
                },
                Arc::new(first),
            ),
            (
                TestSuiteInput {
                    name: "second".to_string(),
                    path: PathBuf::from("second.tests.yaml"),
                    format: TestSuiteFormat::Yaml,
                },
                Arc::new(second),
            ),
        ];

        let error = validate_unique_suite_output_ids(&inputs).unwrap_err();

        assert!(error.to_string().contains("share output ID"));
    }

    #[test]
    fn explicit_external_capture_files_are_validated_before_browser_launch() {
        let workspace_root = tempfile::tempdir().unwrap();
        let output_dir = workspace_root.path().join(".regressify");
        let cookies_path = workspace_root.path().join("common/cookies.yaml");
        std::fs::create_dir_all(cookies_path.parent().unwrap()).unwrap();
        std::fs::write(&cookies_path, "[]\n").unwrap();
        let parsed = vec![(input(), Arc::new(suite(vec![scenario("home", None)])))];

        let workspace = config::WorkspaceConfig {
            cookie_path: Some("common/cookies.yaml".to_string()),
            ..Default::default()
        };
        assert!(
            validate_external_capture_files_in(
                workspace_root.path(),
                &output_dir,
                &workspace,
                &parsed,
            )
            .is_ok()
        );

        std::fs::write(
            &cookies_path,
            "- name: session\n  value: abc\n  domain: example.com\n  sameSite: Invalid\n",
        )
        .unwrap();
        let error = validate_external_capture_files_in(
            workspace_root.path(),
            &output_dir,
            &workspace,
            &parsed,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Strict, Lax, or None"));

        std::fs::write(&cookies_path, "not: a cookie list\n").unwrap();
        let error = validate_external_capture_files_in(
            workspace_root.path(),
            &output_dir,
            &workspace,
            &parsed,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Invalid cookie YAML"));

        let missing_css = config::WorkspaceConfig {
            css_override_path: Some("common/missing.css".to_string()),
            ..Default::default()
        };
        assert!(
            validate_external_capture_files_in(
                workspace_root.path(),
                &output_dir,
                &missing_css,
                &parsed,
            )
            .unwrap_err()
            .to_string()
            .contains("Invalid cssOverridePath")
        );

        let missing_script = config::WorkspaceConfig {
            js_on_ready_path: Some("common/missing.js".to_string()),
            ..Default::default()
        };
        assert!(
            validate_external_capture_files_in(
                workspace_root.path(),
                &output_dir,
                &missing_script,
                &parsed,
            )
            .unwrap_err()
            .to_string()
            .contains("Invalid jsOnReadyPath")
        );
    }

    #[test]
    fn present_default_cookie_file_is_validated_before_browser_launch() {
        let workspace_root = tempfile::tempdir().unwrap();
        let output_dir = workspace_root.path().join(".regressify");
        let cookie_path = workspace_root.path().join("common/_cookies.yaml");
        std::fs::create_dir_all(cookie_path.parent().unwrap()).unwrap();
        std::fs::write(&cookie_path, "- name: session\n  value: token\n").unwrap();
        let parsed = vec![(input(), Arc::new(suite(vec![scenario("home", None)])))];

        let error = validate_external_capture_files_in(
            workspace_root.path(),
            &output_dir,
            &config::WorkspaceConfig::default(),
            &parsed,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("requires a url or domain"));
    }

    #[test]
    fn cookie_file_size_and_value_errors_are_reported_during_preflight() {
        let workspace_root = tempfile::tempdir().unwrap();
        let output_dir = workspace_root.path().join(".regressify");
        let cookie_path = workspace_root.path().join("common/_cookies.yaml");
        std::fs::create_dir_all(cookie_path.parent().unwrap()).unwrap();
        let parsed = vec![(input(), Arc::new(suite(vec![scenario("home", None)])))];
        let name = ".AspNetCore.CookiesC1";

        for (value, expected_error) in [
            ("x".repeat(4096 - name.len()), None),
            ("x".repeat(7746), Some("4096-byte cookie limit")),
            ("private-test-token\n".to_string(), Some("single-line")),
        ] {
            let cookies = serde_json::json!([
                {"name": "session", "value": "valid", "domain": "example.com"},
                {"name": name, "value": value, "domain": "example.com"}
            ]);
            std::fs::write(&cookie_path, serde_yaml::to_string(&cookies).unwrap()).unwrap();

            for configured_path in [None, Some("common/_cookies.yaml".to_string())] {
                let workspace = config::WorkspaceConfig {
                    cookie_path: configured_path,
                    ..Default::default()
                };
                let result = validate_external_capture_files_in(
                    workspace_root.path(),
                    &output_dir,
                    &workspace,
                    &parsed,
                );

                if let Some(expected_error) = expected_error {
                    let error = format!("{:#}", result.unwrap_err());
                    assert!(error.contains("_cookies.yaml"));
                    assert!(error.contains("cookie 1"));
                    assert!(error.contains(name));
                    assert!(error.contains(expected_error));
                    assert!(!error.contains(&value));
                } else {
                    assert!(result.is_ok());
                }
                assert!(!output_dir.exists());
            }
        }
    }

    #[test]
    fn configured_persisted_states_are_validated_before_browser_launch() {
        let workspace_root = tempfile::tempdir().unwrap();
        let output_dir = workspace_root.path().join(".regressify");
        let mut configured_suite = suite(vec![scenario("home", None)]);
        configured_suite.state = Some(Needs::String("auth".to_string()));
        let parsed = vec![(input(), Arc::new(configured_suite))];
        let workspace = config::WorkspaceConfig::default();

        let error = validate_external_capture_files_in(
            workspace_root.path(),
            &output_dir,
            &workspace,
            &parsed,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Invalid persisted state"));

        let state_path = output_dir.join("states/auth.json");
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(&state_path, "not json").unwrap();
        let error = validate_external_capture_files_in(
            workspace_root.path(),
            &output_dir,
            &workspace,
            &parsed,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Invalid persisted state"));

        std::fs::write(
            &state_path,
            r#"{"cookies":[],"localStorage":{"theme":"dark"}}"#,
        )
        .unwrap();
        assert!(
            validate_external_capture_files_in(
                workspace_root.path(),
                &output_dir,
                &workspace,
                &parsed,
            )
            .is_ok()
        );

        std::fs::write(&state_path, r#"{"cookies":{}}"#).unwrap();
        let error = validate_external_capture_files_in(
            workspace_root.path(),
            &output_dir,
            &workspace,
            &parsed,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("cookies' must be an array"));
    }
}
