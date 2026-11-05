//! Bundled engine execution with bounded output and process-tree cancellation.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::cli::Cli;
use crate::cli::args::ScenarioFilter;
use crate::helpers::spawn_background_group;
use crate::progress::{EventReader, LiveProgress};

const LOG_QUEUE_CAPACITY: usize = 256;
const MAX_LOG_CHUNK: usize = 16 * 1024;
const MAX_LOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_SAVED_RUN_BYTES: u64 = 32 * 1024 * 1024;
const SAVED_RUN_VERSION: u8 = 2;
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunCommand {
    Test,
    Reference,
    Compare,
    Approve,
    Snapshot,
    Deploy,
    Optimize,
    Install,
    Init,
    Version,
}

impl RunCommand {
    pub const ALL: &[Self] = &[
        Self::Test,
        Self::Reference,
        Self::Compare,
        Self::Approve,
        Self::Snapshot,
        Self::Optimize,
        Self::Install,
        Self::Init,
    ];
    pub fn argument(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Reference => "ref",
            Self::Compare => "compare",
            Self::Approve => "approve",
            Self::Snapshot => "snapshot",
            Self::Deploy => "snapshot",
            Self::Optimize => "optimize",
            Self::Install => "install",
            Self::Init => "init",
            Self::Version => "version",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Test => "Test",
            Self::Reference => "Capture baseline",
            Self::Compare => "Compare",
            Self::Approve => "Approve",
            Self::Snapshot => "Snapshot",
            Self::Deploy => "Deploy",
            Self::Optimize => "Optimize",
            Self::Install => "Install Chromium",
            Self::Init => "Initialize workspace",
            Self::Version => "Version",
        }
    }
    pub fn needs_confirmation(self) -> bool {
        matches!(self, Self::Reference | Self::Approve | Self::Optimize)
    }
    pub fn action_label(self) -> &'static str {
        match self {
            Self::Test => "Run Test",
            Self::Reference => "Run Reference",
            Self::Compare => "Compare Results",
            Self::Approve => "Approve Results",
            Self::Snapshot => "Build Report",
            Self::Deploy => "Deploy",
            Self::Optimize => "Optimize Images",
            Self::Install => "Install Chromium",
            Self::Init => "Initialize Workspace",
            Self::Version => "Show Version",
        }
    }

    pub fn confirmation_message(self) -> &'static str {
        match self {
            Self::Reference => "Replace reference images with new captures?",
            Self::Approve => "Replace reference images with the selected test results?",
            Self::Optimize => "Optimize the saved image files in place?",
            _ => "",
        }
    }

    pub fn captures(self) -> bool {
        matches!(self, Self::Test | Self::Reference)
    }
    pub fn supports_suites(self) -> bool {
        matches!(
            self,
            Self::Test
                | Self::Reference
                | Self::Compare
                | Self::Approve
                | Self::Snapshot
                | Self::Deploy
        )
    }

    pub fn supports_scenarios(self) -> bool {
        self.supports_suites() && !matches!(self, Self::Snapshot | Self::Deploy)
    }

    pub fn uses_output_suite_ids(self) -> bool {
        matches!(
            self,
            Self::Compare | Self::Approve | Self::Snapshot | Self::Deploy
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct RunOptions {
    pub deployment: Option<super::deployment::DeploymentJob>,
    pub site_env: String,
    pub regressify_dir: String,
    pub run_id: String,
    #[schemars(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_operation_id: Option<crate::diagnostics::record::OperationId>,
    pub proxy: String,
    pub async_capture_limit: u32,
    pub async_capture_ramp_ms: u64,
    pub debug: bool,
    pub verbose: bool,
    pub required_login: bool,
    pub suites: Vec<String>,
    pub scenarios: Vec<ScenarioFilter>,
    #[schemars(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_capture: Option<String>,
    pub fetch_browser: Option<bool>,
    pub skip_compare: bool,
    pub mis_match_threshold: f64,
    pub replacement_profile: String,
    pub async_compare_limit: usize,
    pub speed: u8,
    pub min_size_kb: u64,
    pub title: Option<String>,
    pub heading: Option<String>,
    pub subheading: Option<String>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            deployment: None,
            site_env: String::new(),
            regressify_dir: String::new(),
            run_id: String::new(),
            diagnostic_operation_id: None,
            proxy: String::new(),
            async_capture_limit: 5,
            async_capture_ramp_ms: 1000,
            debug: false,
            verbose: false,
            required_login: false,
            suites: Vec::new(),
            scenarios: Vec::new(),
            report_capture: None,
            fetch_browser: None,
            skip_compare: false,
            mis_match_threshold: 0.1,
            replacement_profile: String::new(),
            async_compare_limit: 2,
            speed: 8,
            min_size_kb: 50,
            title: None,
            heading: None,
            subheading: None,
        }
    }
}

impl RunOptions {
    pub fn output_directory(&self, root: &Path) -> PathBuf {
        if !self.regressify_dir.is_empty() {
            root.join(&self.regressify_dir)
        } else if !self.site_env.is_empty() {
            root.join(format!(".regressify__{}", self.site_env))
        } else {
            root.join(".regressify")
        }
    }

    pub fn applies(key: &str, command: RunCommand) -> bool {
        match key {
            "deployment" | "reportCapture" => false,
            "suites" => command.supports_suites(),
            "scenarios" => command.supports_scenarios(),
            "fetchBrowser" | "asyncCaptureLimit" | "asyncCaptureRampMs" | "debug"
            | "requiredLogin" | "replacementProfile" => command.captures(),
            "skipCompare" => command == RunCommand::Test,
            "misMatchThreshold" => matches!(command, RunCommand::Test | RunCommand::Compare),
            "asyncCompareLimit" => command == RunCommand::Compare,
            "speed" | "minSizeKb" => command == RunCommand::Optimize,
            "title" | "heading" | "subheading" => {
                matches!(command, RunCommand::Snapshot | RunCommand::Deploy)
            }
            _ => true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRequest {
    pub root: PathBuf,
    pub command: RunCommand,
    pub options: RunOptions,
}

impl RunRequest {
    pub fn cli(&self) -> Result<Cli> {
        Ok(Cli::try_parse_from(
            std::iter::once(OsString::from("regressify")).chain(self.arguments()?),
        )?)
    }

    pub fn arguments(&self) -> Result<Vec<OsString>> {
        if self.command == RunCommand::Deploy {
            let deployment = self
                .options
                .deployment
                .as_ref()
                .context("Choose a deployment target")?;
            deployment.validate()?;
            ensure!(
                self.options.suites
                    == deployment
                        .suite_ids
                        .iter()
                        .map(|id| glob::Pattern::escape(id))
                        .collect::<Vec<_>>(),
                "Deployment suite filters changed; review the selection"
            );
        }
        let mut arguments = vec![
            OsString::from("--root"),
            self.root.as_os_str().to_os_string(),
        ];
        let options = &self.options;
        for (flag, value) in [
            ("--site-env", &options.site_env),
            ("--regressify-dir", &options.regressify_dir),
            ("--run-id", &options.run_id),
            ("--proxy", &options.proxy),
        ] {
            if !value.is_empty() {
                arguments.extend([flag.into(), value.into()]);
            }
        }
        arguments.extend([
            "--async-capture-limit".into(),
            options.async_capture_limit.to_string().into(),
            "--async-capture-ramp-ms".into(),
            options.async_capture_ramp_ms.to_string().into(),
        ]);
        for (flag, enabled) in [
            ("--debug", options.debug),
            ("--verbose", options.verbose),
            ("--required-login", options.required_login),
        ] {
            if enabled {
                arguments.push(flag.into());
            }
        }
        arguments.push(self.command.argument().into());
        if self.command.supports_suites() {
            for suite in &options.suites {
                arguments.extend(["--test-suite".into(), suite.into()]);
            }
        }
        if self.command.supports_scenarios() {
            for scenario in &options.scenarios {
                arguments.extend(["--scenario".into(), scenario.to_string().into()]);
            }
        }
        if let Some(capture) = &options.report_capture {
            ensure!(
                self.command == RunCommand::Approve
                    && !options.run_id.is_empty()
                    && !capture.is_empty()
                    && options.scenarios.len() == 1
                    && options.suites == [glob::Pattern::escape(&options.scenarios[0].suite)],
                "Report approval requires the reviewed run, one suite, one scenario, and a capture"
            );
            arguments.extend(["--report-capture".into(), capture.into()]);
        }
        if self.command.captures() {
            if let Some(fetch) = options.fetch_browser {
                arguments.extend(["--fetch-browser".into(), fetch.to_string().into()]);
            }
            if !options.replacement_profile.is_empty() {
                arguments.extend([
                    "--replacement-profile".into(),
                    options.replacement_profile.clone().into(),
                ]);
            }
        }
        if self.command == RunCommand::Test && options.skip_compare {
            arguments.push("--skip-compare".into());
        }
        if matches!(self.command, RunCommand::Test | RunCommand::Compare) {
            arguments.extend([
                "--mis-match-threshold".into(),
                options.mis_match_threshold.to_string().into(),
            ]);
        }
        if self.command == RunCommand::Compare {
            arguments.extend([
                "--async-compare-limit".into(),
                options.async_compare_limit.to_string().into(),
            ]);
        }
        if self.command == RunCommand::Optimize {
            arguments.extend([
                "--speed".into(),
                options.speed.to_string().into(),
                "--min-size-kb".into(),
                options.min_size_kb.to_string().into(),
            ]);
        }
        if matches!(self.command, RunCommand::Snapshot | RunCommand::Deploy) {
            for (flag, value) in [
                ("--title", &options.title),
                ("--heading", &options.heading),
                ("--subheading", &options.subheading),
            ] {
                if let Some(value) = value {
                    arguments.extend([flag.into(), value.into()]);
                }
            }
        }
        // Validate with the real clap parser before starting a child process.
        Cli::try_parse_from(
            std::iter::once(OsString::from("regressify")).chain(arguments.clone()),
        )?;
        Ok(arguments)
    }

    fn process(&self) -> Result<Command> {
        ensure!(self.root.is_dir(), "Workspace directory is unavailable");
        self.arguments()?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg(super::worker::WORKER_FLAG)
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("NO_COLOR", "1")
            .env("CLICOLOR", "0");
        Ok(command)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunOutcome {
    /// Exit zero describes command execution, not whether visual comparisons passed.
    Completed,
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub stderr: bool,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSnapshot {
    version: u8,
    #[serde(default)]
    recovered_from_reports: bool,
    request: RunRequest,
    outcome: RunOutcome,
    progress: LiveProgress,
    #[serde(default)]
    suite_history: BTreeMap<PathBuf, BTreeMap<String, crate::progress::Counts>>,
    logs: VecDeque<LogLine>,
    omitted_chunks: usize,
}

impl RunSnapshot {
    pub fn path_in(root: &Path, preferences: &Path) -> Result<PathBuf> {
        let key = super::deployment::workspace_key(root)?;
        Ok(preferences
            .parent()
            .context("Preferences directory unavailable")?
            .join("runs")
            .join(format!("{key}.json")))
    }

    pub fn load(path: &Path, root: &Path) -> Result<Option<Self>> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("Cannot open saved run activity"),
        };
        let mut bytes = Vec::new();
        file.take(MAX_SAVED_RUN_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_SAVED_RUN_BYTES,
            "Saved run activity exceeds the 32 MiB limit"
        );
        let mut snapshot: Self =
            serde_json::from_slice(&bytes).context("Invalid saved run activity")?;
        if snapshot.version == 1 {
            // Version 1 persisted prefilled text as overrides. Only migrate that
            // format so new explicit values equal to the defaults survive.
            for (value, default) in [
                (
                    &mut snapshot.request.options.title,
                    "Visual Tests Summary | Regressify",
                ),
                (
                    &mut snapshot.request.options.heading,
                    "Visual Tests Summary",
                ),
                (
                    &mut snapshot.request.options.subheading,
                    "Snapshot regression test results",
                ),
            ] {
                if value.as_deref() == Some(default) {
                    *value = None;
                }
            }
            snapshot.version = SAVED_RUN_VERSION;
        }
        snapshot.validate()?;
        ensure!(
            snapshot.request.root.canonicalize()? == root.canonicalize()?,
            "Saved run activity belongs to a different workspace"
        );
        Ok(Some(snapshot))
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == SAVED_RUN_VERSION,
            "Unsupported saved run activity version"
        );
        ensure!(self.progress.finished, "Saved run activity is incomplete");
        ensure!(
            self.progress
                .deployment
                .as_ref()
                .is_none_or(|deployment| deployment.has_safe_url()),
            "Saved run activity contains an unsafe deployment URL"
        );
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        ensure!(
            bytes.len() as u64 <= MAX_SAVED_RUN_BYTES,
            "Saved run activity exceeds the 32 MiB limit"
        );
        std::fs::create_dir_all(
            path.parent()
                .context("Run activity directory unavailable")?,
        )?;
        crate::helpers::write_file_atomically(path, &bytes).context("Cannot save run activity")
    }

    fn save_completed(&self, path: &Path, deployment_path: Option<&Path>) -> Result<()> {
        let published = (|| -> Result<()> {
            if self.request.command == RunCommand::Deploy
                && matches!(self.outcome, RunOutcome::Completed)
                && let (Some(deployment_path), Some(deployment)) =
                    (deployment_path, self.progress.deployment.as_ref())
            {
                let job = self
                    .request
                    .options
                    .deployment
                    .as_ref()
                    .context("Completed deployment is missing its target")?;
                super::deployment::DeployTargets::record_report_publication(
                    deployment_path,
                    &job.target,
                    deployment.url.as_deref(),
                    &job.suite_ids,
                )
                .context("Site deployed, but its URL could not be saved to the target")?;
            }
            Ok(())
        })();
        // Keep successful publication details in activity even if the target write fails.
        let activity = self.save(path);
        published?;
        activity
    }

    pub fn recover_reports(
        root: &Path,
        suites: &[super::workspace::SuiteSummary],
    ) -> Result<Option<Self>> {
        use super::results::{Catalog, Outcome, ReportSelection, TestResult};
        use crate::progress::{Event, Job, Record};

        let mut options = RunOptions::default();
        let mut catalog = Catalog::load_existing_selected(
            &options.output_directory(root),
            ReportSelection::Latest,
        )?;
        catalog.resolve_names(suites);
        let Some(latest) = catalog
            .reports
            .iter()
            .find(|report| report.source.is_some() && !report.report.tests.is_empty())
        else {
            return Ok(None);
        };
        options.run_id = latest.run_id.clone();
        let mut manager = RunManager::default();
        let mut filters = BTreeSet::new();
        let mut missing_scenario_ids = false;
        let record = |event| Record {
            version: 1,
            elapsed_ms: 0,
            event,
        };
        for report in catalog
            .reports
            .iter()
            .filter(|report| report.source.is_some() && report.run_id == options.run_id)
        {
            let suite = suites
                .iter()
                .find(|suite| suite.output_id.eq_ignore_ascii_case(&report.report.id))
                .context("Saved report suite is unavailable")?;
            options.suites.push(glob::Pattern::escape(&suite.output_id));
            let mut jobs: BTreeMap<(String, String), Vec<&TestResult>> = BTreeMap::new();
            for test in &report.report.tests {
                let scenario_id = test.pair.scenario_id.as_deref().or_else(|| {
                    test.pair
                        .display
                        .scenario_index
                        .and_then(|index| suite.scenarios.get(index))
                        .map(|scenario| scenario.id.as_str())
                });
                if let Some(id) = scenario_id {
                    filters.insert((suite.output_id.clone(), id.to_string()));
                } else {
                    missing_scenario_ids = true;
                }
                jobs.entry((
                    scenario_id
                        .unwrap_or_else(|| test.pair.capture_key())
                        .to_string(),
                    test.pair.display.viewport.clone(),
                ))
                .or_default()
                .push(test);
            }
            for ((scenario_id, viewport), tests) in jobs {
                let id = manager.progress.scenarios.len() + 1;
                manager.progress.apply(record(Event::Queued {
                    job: Job {
                        id,
                        suite_id: suite.output_id.clone(),
                        suite_name: suite.name.clone(),
                        scenario_id: scenario_id.clone(),
                        label: tests[0].pair.display.title.clone(),
                        viewport,
                        captures: tests
                            .iter()
                            .map(|test| test.pair.capture_key().to_string())
                            .collect(),
                    },
                }));
                for test in tests {
                    let error = test
                        .pair
                        .engine_error_msg
                        .clone()
                        .filter(|error| !error.is_empty())
                        .or_else(|| match test.outcome() {
                            Outcome::Mismatch => Some("Visual mismatch".into()),
                            Outcome::Unknown => {
                                Some("Saved comparison has an unknown status".into())
                            }
                            _ => None,
                        });
                    manager.progress.apply(record(Event::Compared {
                        suite_id: suite.output_id.clone(),
                        capture: test.pair.capture_key().to_string(),
                        label: test.pair.display.title.clone(),
                        scenario_id: Some(scenario_id.clone()),
                        passed: test.outcome() == Outcome::Passed,
                        error,
                    }));
                }
            }
            manager.progress.apply(record(Event::SuiteFinished {
                id: suite.output_id.clone(),
            }));
        }
        if !missing_scenario_ids {
            options.scenarios = filters
                .into_iter()
                .map(|(suite, scenario)| ScenarioFilter { suite, scenario })
                .collect();
        }
        manager.request = Some(RunRequest {
            root: root.to_path_buf(),
            command: RunCommand::Compare,
            options,
        });
        manager.outcome = Some(RunOutcome::Completed);
        manager.progress.end(false, None);
        manager.progress.phase = "Saved comparison results".into();
        manager.progress.clear_logs();
        manager.recovered_from_reports = true;
        manager.push_log(LogLine {
            stderr: false,
            text: "Original execution log unavailable for these saved comparison results.\n".into(),
        });
        Ok(manager.snapshot())
    }
}

struct RunHandle {
    logs: Receiver<LogLine>,
    cancel: SyncSender<()>,
    worker: Option<JoinHandle<RunOutcome>>,
    omitted: Arc<AtomicUsize>,
    progress: Arc<Mutex<LiveProgress>>,
    _events_file: Option<tempfile::NamedTempFile>,
}

impl RunHandle {
    fn spawn(
        mut command: Command,
        payload: Option<Vec<u8>>,
        events_file: Option<tempfile::NamedTempFile>,
        scratch: Option<tempfile::TempDir>,
    ) -> Result<Self> {
        let diagnostic_operation = payload
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<super::worker::WorkerRequest>(bytes).ok())
            .filter(|request| request.run.command == RunCommand::Deploy)
            .and_then(|request| {
                request
                    .run
                    .options
                    .diagnostic_operation_id
                    .clone()
                    .map(|id| (request.run.options.output_directory(&request.run.root), id))
            });
        let (sender, logs) = mpsc::sync_channel(LOG_QUEUE_CAPACITY);
        let (cancel, cancellation) = mpsc::sync_channel(1);
        let omitted = Arc::new(AtomicUsize::new(0));
        let lost = omitted.clone();
        let mut events = events_file
            .as_ref()
            .map(|file| EventReader::open(file.path()))
            .transpose()?;
        let progress = Arc::new(Mutex::new(LiveProgress::default()));
        let live = progress.clone();
        let worker = thread::Builder::new()
            .name("regressify-run".into())
            .spawn(move || {
                let _scratch = scratch;
                let result = (|| -> Result<RunOutcome> {
                    let mut child = spawn_background_group(&mut command)
                        .context("Cannot launch the bundled engine worker")?;
                    let stdout = child.inner().stdout.take();
                    let stderr = child.inner().stderr.take();
                    let mut readers = Vec::new();
                    if let Some(stdout) = stdout {
                        readers.push(read_stream(stdout, false, sender.clone(), lost.clone()));
                    }
                    if let Some(stderr) = stderr {
                        readers.push(read_stream(stderr, true, sender, lost));
                    }
                    let writer = payload.map(|payload| {
                        let stdin = child.inner().stdin.take();
                        thread::spawn(move || -> Result<()> {
                            let mut input =
                                stdin.ok_or_else(|| anyhow!("Worker input is unavailable"))?;
                            input
                                .write_all(&payload)
                                .context("Cannot send run settings to worker")
                        })
                    });
                    let mut exited = None;
                    let outcome = (|| -> Result<RunOutcome> {
                        loop {
                            if let Some(events) = &mut events {
                                let mut state = live
                                    .lock()
                                    .map_err(|_| anyhow!("Run progress is unavailable"))?;
                                events.drain_batch(|record| state.apply(record))?;
                            }
                            if exited.is_none()
                                && let Some(status) = child.try_wait()?
                            {
                                exited = Some((status, std::time::Instant::now()));
                            }
                            if let Some((status, exited_at)) = exited {
                                let streams_done =
                                    writer.as_ref().is_none_or(JoinHandle::is_finished)
                                        && readers.iter().all(JoinHandle::is_finished);
                                let drain_expired = exited_at.elapsed() >= PIPE_DRAIN_TIMEOUT;
                                if !streams_done && drain_expired {
                                    // A descendant outlived the worker while retaining stdout/stderr;
                                    // terminate the process group so pipe readers cannot hang.
                                    let _ = child.kill();
                                    let _ = child.wait();
                                }
                                if streams_done || drain_expired {
                                    if let Some(events) = &mut events {
                                        let mut state = live
                                            .lock()
                                            .map_err(|_| anyhow!("Run progress is unavailable"))?;
                                        events.drain(|record| state.apply(record))?;
                                        events.complete()?;
                                    }
                                    break Ok(if status.success() {
                                        RunOutcome::Completed
                                    } else {
                                        RunOutcome::Failed(format!(
                                            "Engine worker exited with {}",
                                            status
                                                .code()
                                                .map(|code| code.to_string())
                                                .unwrap_or_else(|| "a signal".into())
                                        ))
                                    });
                                }
                            }
                            match cancellation.recv_timeout(Duration::from_millis(50)) {
                                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                                    child
                                        .kill()
                                        .context("Cannot terminate the engine process group")?;
                                    child.wait()?;
                                    if let Some((output, operation)) = &diagnostic_operation
                                        && crate::diagnostics::journal::record_worker_cancellation(output, operation).is_err() {
                                        crate::diagnostics::install::emergency_warning("Cancelled deployment log could not be finalized; remote state is unknown");
                                    }
                                    break Ok(RunOutcome::Cancelled);
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                            }
                        }
                    })();
                    if outcome.is_err() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    let written = writer
                        .map(|writer| {
                            writer
                                .join()
                                .map_err(|_| anyhow!("Worker input writer stopped unexpectedly"))?
                        })
                        .transpose();
                    for reader in readers {
                        reader
                            .join()
                            .map_err(|_| anyhow!("Log reader stopped unexpectedly"))??;
                    }
                    let outcome = outcome?;
                    if matches!(outcome, RunOutcome::Completed) {
                        written?;
                    }
                    Ok(outcome)
                })();
                let outcome =
                    result.unwrap_or_else(|error| RunOutcome::Failed(format!("{error:#}")));
                if let Ok(mut state) = live.lock() {
                    match &outcome {
                        RunOutcome::Cancelled => state.end(true, None),
                        RunOutcome::Failed(error) => {
                            let message = state.error.clone().unwrap_or_else(|| error.clone());
                            state.end(false, Some(message));
                        }
                        RunOutcome::Completed if !state.finished => state.end(false, None),
                        _ => {}
                    }
                }
                outcome
            })?;
        Ok(Self {
            logs,
            cancel,
            worker: Some(worker),
            omitted,
            progress,
            _events_file: events_file,
        })
    }

    fn cancel(&self) {
        let _ = self.cancel.try_send(());
    }
}

impl Drop for RunHandle {
    fn drop(&mut self) {
        self.cancel();
        // Close waits for process-group cleanup so Chromium is not orphaned on quit.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_stream(
    stream: impl Read + Send + 'static,
    stderr: bool,
    sender: SyncSender<LogLine>,
    omitted: Arc<AtomicUsize>,
) -> JoinHandle<Result<()>> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        loop {
            let mut bytes = Vec::new();
            let count = (&mut reader)
                .take(MAX_LOG_CHUNK as u64)
                .read_until(b'\n', &mut bytes)?;
            if count == 0 {
                break;
            }
            let line = LogLine {
                stderr,
                text: String::from_utf8_lossy(&bytes).into_owned(),
            };
            if sender.try_send(line).is_err() {
                omitted.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    })
}

#[derive(Default)]
pub struct RunManager {
    active: Option<RunHandle>,
    storage_path: Option<PathBuf>,
    deployment_storage_path: Option<PathBuf>,
    persistence: Option<JoinHandle<Result<()>>>,
    pub persistence_error: Option<String>,
    pub recovered_from_reports: bool,
    pub logs: VecDeque<LogLine>,
    pub outcome: Option<RunOutcome>,
    pub request: Option<RunRequest>,
    pub omitted_chunks: usize,
    log_bytes: usize,
    pub progress: LiveProgress,
    suite_history: BTreeMap<PathBuf, BTreeMap<String, crate::progress::Counts>>,
    revision: u64,
}

impl RunManager {
    pub fn remap_suite(
        &mut self,
        old_name: &str,
        old_id: &str,
        new: Option<(&str, &str)>,
    ) -> Result<()> {
        ensure!(
            !self.running(),
            "Finish the active command before changing a suite"
        );
        self.suite_history = self.completed_suite_history();
        for history in self.suite_history.values_mut() {
            if let Some(counts) = history.remove(old_id)
                && let Some((id, _)) = new
            {
                history.insert(id.into(), counts);
            }
        }
        self.progress.remap_suite(old_id, new);
        // Deletion retains historical filters: clearing them would mean "all suites" on replay.
        if let Some(request) = &mut self.request
            && let Some((id, name)) = new
        {
            let (old, updated) = if request.command.uses_output_suite_ids() {
                (old_id, id)
            } else {
                (old_name, name)
            };
            for selected in &mut request.options.suites {
                if *selected == glob::Pattern::escape(old) {
                    *selected = glob::Pattern::escape(updated);
                }
            }
            for filter in &mut request.options.scenarios {
                if filter.suite == old {
                    filter.suite = updated.into();
                }
            }
        }
        self.revision = self.revision.wrapping_add(1);
        if let (Some(path), Some(snapshot)) = (self.storage_path.clone(), self.snapshot()) {
            self.persistence = Some(
                thread::Builder::new()
                    .name("regressify-save-run".into())
                    .spawn(move || snapshot.save(&path))?,
            );
        }
        Ok(())
    }

    pub fn output_directories(&self) -> Vec<PathBuf> {
        self.suite_history
            .keys()
            .cloned()
            .chain(
                self.request
                    .iter()
                    .map(|request| request.options.output_directory(&request.root)),
            )
            .collect()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn running(&self) -> bool {
        self.active.is_some() || self.persistence.is_some()
    }

    pub fn set_storage_path(&mut self, path: PathBuf) {
        self.storage_path = Some(path);
    }

    pub fn set_deployment_storage_path(&mut self, path: PathBuf) {
        self.deployment_storage_path = Some(path);
    }

    pub fn suite_counts(&self, output: &Path, id: &str) -> Option<crate::progress::Counts> {
        if self
            .request
            .as_ref()
            .is_none_or(|request| request.options.output_directory(&request.root) == output)
        {
            let counts = self.progress.counts(Some(id));
            if counts.total > 0 {
                return Some(counts);
            }
        }
        self.suite_history.get(output)?.get(id).copied()
    }

    fn completed_suite_history(
        &self,
    ) -> BTreeMap<PathBuf, BTreeMap<String, crate::progress::Counts>> {
        let mut history = self.suite_history.clone();
        if self.progress.finished
            && let Some(request) = &self.request
        {
            let suites = history
                .entry(request.options.output_directory(&request.root))
                .or_default();
            for id in self.progress.suites.keys() {
                let counts = self.progress.counts(Some(id));
                if counts.total > 0 {
                    suites.insert(id.clone(), counts);
                }
            }
        }
        history
    }

    pub fn snapshot(&self) -> Option<RunSnapshot> {
        if self.active.is_some() || !self.progress.finished {
            return None;
        }
        let mut request = self.request.clone()?;
        request.options.proxy.clear();
        Some(RunSnapshot {
            version: SAVED_RUN_VERSION,
            recovered_from_reports: self.recovered_from_reports,
            request,
            outcome: self.outcome.clone()?,
            progress: self.progress.clone(),
            suite_history: self.completed_suite_history(),
            logs: self.logs.clone(),
            omitted_chunks: self.omitted_chunks,
        })
    }

    pub fn restore(&mut self, snapshot: RunSnapshot) -> Result<()> {
        ensure!(!self.running(), "A command is already running");
        snapshot.validate()?;
        self.logs.clear();
        self.log_bytes = 0;
        for line in snapshot.logs {
            self.push_log(line);
        }
        self.request = Some(snapshot.request);
        self.outcome = Some(snapshot.outcome);
        self.progress = snapshot.progress;
        self.suite_history = snapshot.suite_history;
        self.suite_history = self.completed_suite_history();
        self.omitted_chunks = snapshot.omitted_chunks;
        self.persistence_error = None;
        self.recovered_from_reports = snapshot.recovered_from_reports;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    pub fn start(&mut self, mut request: RunRequest) -> Result<()> {
        ensure!(!self.running(), "A command is already running");
        if request.command.captures() && request.options.run_id.is_empty() {
            request.options.run_id = format!("desktop-{}", uuid::Uuid::new_v4().simple());
        }
        request.options.diagnostic_operation_id = (request.command != RunCommand::Version)
            .then(crate::diagnostics::record::OperationId::new);
        let command = request.process()?;
        let events = tempfile::NamedTempFile::new()?;
        let payload = serde_json::to_vec(&super::worker::WorkerRequest {
            run: request.clone(),
            events_path: Some(events.path().into()),
            scratch_directory: None,
        })?;
        ensure!(
            payload.len() <= super::worker::MAX_REQUEST_BYTES,
            "Run request exceeds 1 MiB"
        );
        self.active = Some(RunHandle::spawn(
            command,
            Some(payload),
            Some(events),
            None,
        )?);
        self.suite_history = self.completed_suite_history();
        self.progress = LiveProgress::default();
        self.logs.clear();
        self.log_bytes = 0;
        self.omitted_chunks = 0;
        self.outcome = None;
        self.persistence_error = None;
        self.recovered_from_reports = false;
        self.request = Some(request);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    pub fn cancel(&self) {
        if let Some(active) = &self.active {
            active.cancel();
        }
    }

    /// Non-blocking UI tick; completion is separate from the bounded log queue.
    pub fn poll(&mut self) -> bool {
        if let Some(persistence) = &self.persistence {
            if !persistence.is_finished() {
                return false;
            }
            if let Some(persistence) = self.persistence.take() {
                self.persistence_error = persistence
                    .join()
                    .unwrap_or_else(|_| Err(anyhow!("Run activity writer stopped unexpectedly")))
                    .err()
                    .map(|error| format!("{error:#}"));
            }
            self.revision = self.revision.wrapping_add(1);
            return true;
        }
        let Some(active) = &self.active else {
            return false;
        };
        let lines = active
            .logs
            .try_iter()
            .take(LOG_QUEUE_CAPACITY)
            .collect::<Vec<_>>();
        let omitted = active.omitted.load(Ordering::Relaxed);
        if self.omitted_chunks != omitted {
            self.omitted_chunks = omitted;
            self.revision = self.revision.wrapping_add(1);
        }
        let finished = active.worker.as_ref().is_some_and(JoinHandle::is_finished);
        if let Ok(state) = active.progress.try_lock()
            && state.revision() != self.progress.revision()
        {
            self.progress = state.clone();
            self.revision = self.revision.wrapping_add(1);
        }
        for line in lines {
            self.push_log(line);
        }
        if finished && let Some(mut active) = self.active.take() {
            for line in active.logs.try_iter() {
                self.push_log(line);
            }
            self.outcome = active.worker.take().map(|worker| {
                worker.join().unwrap_or_else(|_| {
                    RunOutcome::Failed("Execution worker stopped unexpectedly".into())
                })
            });
            self.revision = self.revision.wrapping_add(1);
            if let (Some(path), Some(snapshot)) = (self.storage_path.clone(), self.snapshot()) {
                let deployment_path = self.deployment_storage_path.clone();
                match thread::Builder::new()
                    .name("regressify-save-run".into())
                    .spawn(move || snapshot.save_completed(&path, deployment_path.as_deref()))
                {
                    Ok(persistence) => self.persistence = Some(persistence),
                    Err(error) => self.persistence_error = Some(error.to_string()),
                }
            }
        }
        finished && self.persistence.is_none()
    }

    fn push_log(&mut self, line: LogLine) {
        self.revision = self.revision.wrapping_add(1);
        self.log_bytes += line.text.len();
        self.logs.push_back(line);
        while self.log_bytes > MAX_LOG_BYTES {
            if let Some(line) = self.logs.pop_front() {
                self.log_bytes -= line.text.len();
            } else {
                break;
            }
        }
    }
}

impl Drop for RunManager {
    fn drop(&mut self) {
        if let Some(persistence) = self.persistence.take() {
            let _ = persistence.join();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn snapshot_defaults_leave_workspace_report_text_unset_in_cli() {
        let request = RunRequest {
            root: "QA workspace".into(),
            command: RunCommand::Snapshot,
            options: RunOptions::default(),
        };
        let crate::cli::Command::Snapshot(args) = request.cli().unwrap().command else {
            panic!("Expected snapshot command");
        };
        assert!(args.title.is_none());
        assert!(args.heading.is_none());
        assert!(args.subheading.is_none());
    }

    #[test]
    fn snapshot_and_deploy_report_text_options_preserve_explicit_overrides() {
        use crate::desktop::deployment::{DeployTarget, DeploymentJob, DeploymentProvider};
        use serde_json::json;

        for (command, provider) in [
            (RunCommand::Snapshot, DeploymentProvider::AzureSwa),
            (RunCommand::Deploy, DeploymentProvider::AzureSwa),
            (RunCommand::Deploy, DeploymentProvider::Netlify),
            (RunCommand::Deploy, DeploymentProvider::CloudflareWorkers),
        ] {
            for options in [
                json!({}),
                json!({"title": null, "heading": null, "subheading": null}),
                json!({"title": "Project report"}),
                json!({"heading": "Project summary", "subheading": "**Staging** results"}),
                json!({
                    "title": "Visual Tests Summary | Regressify",
                    "heading": "Visual Tests Summary",
                    "subheading": "Snapshot regression test results"
                }),
                json!({"title": "", "heading": "", "subheading": ""}),
            ] {
                let mut request = request(command);
                request.options = serde_json::from_value(options).unwrap();
                if command == RunCommand::Deploy {
                    request.options.suites = vec!["shop".into()];
                    request.options.deployment = Some(DeploymentJob {
                        target: DeployTarget {
                            id: "test".into(),
                            name: "Test".into(),
                            published_url: None,
                            credential_id: None,
                            named_token_id: None,
                            environment: String::new(),
                            provider,
                            site_id: if provider == DeploymentProvider::Netlify {
                                "11111111-2222-4333-8444-555555555555".into()
                            } else {
                                String::new()
                            },
                            cloudflare: (provider == DeploymentProvider::CloudflareWorkers).then(
                                || crate::desktop::deployment::cloudflare::CloudflareTarget {
                                    account_id: "0123456789abcdef0123456789abcdef".into(),
                                    worker_name: "reports".into(),
                                    ..crate::desktop::deployment::cloudflare::CloudflareTarget::default()
                                },
                            ),
                        },
                        suite_ids: vec!["shop".into()],
                        catalog_ids: vec!["shop".into()],
                        partial_confirmed: false,
                    });
                }
                let request: RunRequest =
                    serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
                let crate::cli::Command::Snapshot(args) = request.cli().unwrap().command else {
                    panic!("Expected snapshot command");
                };
                assert_eq!(args.title, request.options.title);
                assert_eq!(args.heading, request.options.heading);
                assert_eq!(args.subheading, request.options.subheading);
            }
        }
    }

    #[test]
    fn saved_reports_recover_the_latest_matching_run_without_modifying_artifacts() {
        use super::super::document::DocumentKind;
        use super::super::workspace::{Entry, ScenarioSummary, SuiteSummary};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let suites =
            [("shop", "shop-output"), ("other", "other-output")].map(|(name, output_id)| {
                SuiteSummary {
                    entry: Entry {
                        path: format!("visual_tests/{name}.tests.yaml").into(),
                        kind: DocumentKind::Suite,
                        editable: true,
                    },
                    name: name.into(),
                    display_name: None,
                    output_id: output_id.into(),
                    error: None,
                    scenarios: vec![ScenarioSummary {
                        id: "home".into(),
                        label: "Home".into(),
                        url: "https://example.com/".into(),
                    }],
                }
            });
        assert!(
            RunSnapshot::recover_reports(root, &suites)
                .unwrap()
                .is_none()
        );
        let mut files = Vec::new();
        for (suite, run, timestamp) in [
            ("shop-output", "old", 1),
            ("other-output", "earlier", 2),
            ("shop-output", "existing", 3),
            ("orphan", "unrelated", 4),
        ] {
            let path = root.join(format!(
                ".regressify/{suite}/bitmaps_test/{run}/report.json"
            ));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let tests = [("phone", "FINAL", false), ("phone", "$step", true), ("tablet", "FINAL", false), ("desktop", "FINAL", true)]
                .map(|(viewport, capture, passed)| serde_json::json!({
                    "status": if passed { "pass" } else { "fail" },
                    "pair": { "fileName": format!("home_{viewport}__{capture}.png"), "scenarioId": "home", "scenarioLabel": "Home", "engineErrorMsg": if passed { None } else { Some("Dimensions differ") } }
                }));
            let bytes =
                serde_json::to_vec(&serde_json::json!({ "id": suite, "tests": tests })).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(
                    std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(timestamp),
                ))
                .unwrap();
            files.push((path, bytes));
        }
        let snapshot = RunSnapshot::recover_reports(root, &suites)
            .unwrap()
            .unwrap();
        let mut manager = RunManager::default();
        manager.restore(snapshot).unwrap();
        assert!(!manager.running());
        assert!(manager.recovered_from_reports);
        let request = manager.request.as_ref().unwrap();
        assert_eq!(request.options.run_id, "existing");
        assert_eq!(request.options.suites, ["shop-output"]);
        assert_eq!(request.options.scenarios[0].to_string(), "shop-output:home");
        assert_eq!(manager.progress.scenarios.len(), 3);
        assert_eq!(manager.progress.comparison_counts(None).total, 4);
        assert_eq!(manager.progress.comparison_counts(None).failed, 2);
        assert_eq!(manager.progress.comparison_counts(None).passed, 2);
        assert!(
            manager
                .progress
                .scenarios
                .values()
                .any(|scenario| scenario.job.viewport == "phone"
                    && scenario.job.captures.len() == 2
                    && scenario.message.as_deref() == Some("Dimensions differ"))
        );
        assert!(manager.progress.logs.is_empty());
        for (path, bytes) in files {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn saved_activity_migrates_only_legacy_report_defaults() {
        use serde_json::json;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let path = root.join("run.json");
        let mut manager = RunManager::default();
        let mut request = request(RunCommand::Snapshot);
        request.root = root.clone();
        manager.request = Some(request);
        manager.outcome = Some(RunOutcome::Completed);
        manager.progress.end(false, None);
        let snapshot = serde_json::to_value(manager.snapshot().unwrap()).unwrap();
        let old_defaults = json!({
            "title": "Visual Tests Summary | Regressify",
            "heading": "Visual Tests Summary",
            "subheading": "Snapshot regression test results"
        });

        for (version, options, expected) in [
            (
                1,
                old_defaults.clone(),
                json!({"title": null, "heading": null, "subheading": null}),
            ),
            (
                1,
                json!({
                    "title": "Visual Tests Summary | Regressify",
                    "heading": "Custom summary",
                    "subheading": ""
                }),
                json!({"title": null, "heading": "Custom summary", "subheading": ""}),
            ),
            (2, old_defaults.clone(), old_defaults),
        ] {
            let mut saved = snapshot.clone();
            saved["version"] = json!(version);
            saved["request"]["options"]
                .as_object_mut()
                .unwrap()
                .extend(options.as_object().unwrap().clone());
            std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();

            let loaded = RunSnapshot::load(&path, &root).unwrap().unwrap();
            let crate::cli::Command::Snapshot(args) = loaded.request.cli().unwrap().command else {
                panic!("Expected snapshot command");
            };
            assert_eq!(args.title.as_deref(), expected["title"].as_str());
            assert_eq!(args.heading.as_deref(), expected["heading"].as_str());
            assert_eq!(args.subheading.as_deref(), expected["subheading"].as_str());
            assert_eq!(loaded.version, 2);
            manager.restore(loaded).unwrap();
            manager.snapshot().unwrap().save(&path).unwrap();
            let reloaded = RunSnapshot::load(&path, &root).unwrap().unwrap();
            let options = serde_json::to_value(reloaded.request.options).unwrap();
            for key in ["title", "heading", "subheading"] {
                assert_eq!(options[key], expected[key]);
            }
        }
    }

    #[test]
    fn suite_renames_and_deletion_update_saved_activity_without_widening_filters() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut manager = RunManager::default();
        let mut request = request(RunCommand::Test);
        request.root = root.clone();
        request.options.suites = vec!["shop".into(), "other".into()];
        request.options.scenarios = vec!["shop:home".parse().unwrap()];
        manager.request = Some(request);
        for id in ["shop", "other"] {
            manager.progress.apply(crate::progress::Record {
                version: 1,
                elapsed_ms: 10,
                event: crate::progress::Event::Compared {
                    suite_id: id.into(),
                    capture: "home__FINAL".into(),
                    label: "Home".into(),
                    scenario_id: Some("home".into()),
                    passed: true,
                    error: None,
                },
            });
        }
        manager.progress.end(false, None);
        manager.outcome = Some(RunOutcome::Completed);
        manager
            .remap_suite("shop", "shop", Some(("checkout", "checkout")))
            .unwrap();
        assert_eq!(
            manager
                .suite_counts(&root.join(".regressify"), "checkout")
                .unwrap()
                .passed,
            1
        );
        assert!(
            manager
                .suite_counts(&root.join(".regressify"), "shop")
                .is_none()
        );
        assert_eq!(
            manager.request.as_ref().unwrap().options.scenarios[0].suite,
            "checkout"
        );
        assert_eq!(
            manager
                .progress
                .scenarios
                .values()
                .filter(|scenario| scenario.job.suite_id == "checkout")
                .count(),
            1
        );
        let path = root.join("activity.json");
        manager.snapshot().unwrap().save(&path).unwrap();
        let mut reopened = RunManager::default();
        reopened
            .restore(RunSnapshot::load(&path, &root).unwrap().unwrap())
            .unwrap();
        reopened.remap_suite("checkout", "checkout", None).unwrap();
        assert!(
            reopened
                .suite_counts(&root.join(".regressify"), "checkout")
                .is_none()
        );
        assert_eq!(reopened.progress.counts(None).passed, 1);
        assert_eq!(
            reopened.request.as_ref().unwrap().options.suites,
            ["checkout", "other"]
        );
        assert_eq!(
            reopened.request.as_ref().unwrap().options.scenarios[0].suite,
            "checkout"
        );
    }

    #[test]
    fn completed_run_activity_survives_a_new_manager_without_restarting_work() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let path = RunSnapshot::path_in(&root, &directory.path().join("preferences.json")).unwrap();
        assert!(RunSnapshot::load(&path, &root).unwrap().is_none());
        let mut manager = RunManager::default();
        let mut request = request(RunCommand::Test);
        request.root = root.clone();
        request.options.run_id = "saved-test".into();
        request.options.site_env = "staging".into();
        request.options.suites = vec!["shop".into()];
        request.options.scenarios = vec!["shop:home".parse().unwrap()];
        request.options.proxy = "http://user:private-password@proxy.example".into();
        manager.request = Some(request);
        manager.progress.apply(crate::progress::Record {
            version: 1,
            elapsed_ms: 27_100,
            event: crate::progress::Event::Compared {
                suite_id: "shop".into(),
                capture: "home_phone__FINAL".into(),
                label: "Home".into(),
                scenario_id: Some("home".into()),
                passed: false,
                error: Some("Dimensions differ".into()),
            },
        });
        manager.progress.end(false, None);
        manager.push_log(LogLine {
            stderr: true,
            text: "Comparison failed\n".into(),
        });
        manager.omitted_chunks = 3;
        for outcome in [
            RunOutcome::Completed,
            RunOutcome::Failed("Report write failed".into()),
            RunOutcome::Cancelled,
        ] {
            manager.outcome = Some(outcome.clone());
            manager.snapshot().unwrap().save(&path).unwrap();
            let snapshot = RunSnapshot::load(&path, &root).unwrap().unwrap();
            let mut reopened = RunManager::default();
            reopened.restore(snapshot).unwrap();
            assert!(!reopened.running());
            assert!(!reopened.poll());
            assert_eq!(reopened.outcome, Some(outcome));
            assert_eq!(reopened.progress.comparison_counts(None).failed, 1);
            assert_eq!(reopened.progress.elapsed_ms, 27_100);
            assert_eq!(reopened.logs[0].text, "Comparison failed\n");
            assert_eq!(reopened.omitted_chunks, 3);
            let request = reopened.request.as_ref().unwrap();
            assert_eq!(request.options.run_id, "saved-test");
            assert_eq!(request.options.scenarios[0].to_string(), "shop:home");
            assert_eq!(
                request.options.output_directory(&root),
                root.join(".regressify__staging")
            );
            assert!(request.options.proxy.is_empty());
        }
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("private-password")
        );
        let other = tempfile::tempdir().unwrap();
        assert!(RunSnapshot::load(&path, other.path()).is_err());
        assert_ne!(
            path,
            RunSnapshot::path_in(other.path(), &directory.path().join("preferences.json")).unwrap()
        );
        std::fs::write(&path, b"{broken").unwrap();
        assert!(RunSnapshot::load(&path, &root).is_err());
    }

    #[test]
    fn suite_status_history_survives_other_runs_restart_and_output_changes() {
        use crate::progress::{Event, Record};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let output = root.join(".regressify");
        let path = root.join("run.json");
        let mut manager = RunManager::default();
        let mut request = request(RunCommand::Test);
        request.root = root.clone();
        request.options.suites = vec!["first".into()];
        manager.request = Some(request.clone());
        manager.progress.apply(Record {
            version: 1,
            elapsed_ms: 10,
            event: Event::Compared {
                suite_id: "first".into(),
                capture: "home_phone__FINAL".into(),
                label: "Home".into(),
                scenario_id: Some("home".into()),
                passed: false,
                error: Some("Visual mismatch".into()),
            },
        });
        manager.progress.end(false, None);
        manager.outcome = Some(RunOutcome::Completed);
        manager.restore(manager.snapshot().unwrap()).unwrap();
        request.options.suites = vec!["second".into()];
        manager.request = Some(request.clone());
        manager.progress = LiveProgress::default();
        assert_eq!(manager.suite_counts(&output, "first").unwrap().failed, 1);
        assert_eq!(manager.progress.counts(None).total, 0);
        manager.progress.apply(Record {
            version: 1,
            elapsed_ms: 20,
            event: Event::Compared {
                suite_id: "second".into(),
                capture: "home_phone__FINAL".into(),
                label: "Home".into(),
                scenario_id: Some("home".into()),
                passed: true,
                error: None,
            },
        });
        assert_eq!(manager.suite_counts(&output, "first").unwrap().failed, 1);
        assert_eq!(manager.suite_counts(&output, "second").unwrap().passed, 1);
        assert_eq!(manager.progress.counts(None).total, 1);
        manager.progress.end(false, None);
        manager.snapshot().unwrap().save(&path).unwrap();
        let mut reopened = RunManager::default();
        reopened
            .restore(RunSnapshot::load(&path, &root).unwrap().unwrap())
            .unwrap();
        assert_eq!(reopened.suite_counts(&output, "first").unwrap().failed, 1);
        assert_eq!(reopened.suite_counts(&output, "second").unwrap().passed, 1);
        assert_eq!(reopened.progress.counts(None).total, 1);
        request.options.site_env = "staging".into();
        reopened.request = Some(request);
        reopened.progress = LiveProgress::default();
        assert!(
            reopened
                .suite_counts(&root.join(".regressify__staging"), "first")
                .is_none()
        );
        assert_eq!(reopened.suite_counts(&output, "first").unwrap().failed, 1);
        let mut legacy = serde_json::to_value(manager.snapshot().unwrap()).unwrap();
        legacy.as_object_mut().unwrap().remove("suiteHistory");
        reopened
            .restore(serde_json::from_value(legacy).unwrap())
            .unwrap();
        reopened.progress = LiveProgress::default();
        assert_eq!(reopened.suite_counts(&output, "second").unwrap().passed, 1);
    }

    #[test]
    fn completion_waits_for_activity_persistence_and_reports_write_failures() {
        let directory = tempfile::tempdir().unwrap();
        for fails in [false, true] {
            let path = if fails {
                directory.path().to_path_buf()
            } else {
                directory.path().join("run.json")
            };
            let (mut manager, shared) = polling_fixture();
            shared.lock().unwrap().end(false, None);
            manager.active.as_mut().unwrap().worker = Some(thread::spawn(|| RunOutcome::Completed));
            let mut request = request(RunCommand::Test);
            request.root = directory.path().canonicalize().unwrap();
            manager.request = Some(request);
            manager.set_storage_path(path.clone());
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !manager.poll() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "Run completion did not finish"
                );
                thread::yield_now();
            }
            assert!(!manager.running());
            assert_eq!(manager.persistence_error.is_some(), fails);
            assert_eq!(manager.outcome, Some(RunOutcome::Completed));
            if !fails {
                assert!(
                    RunSnapshot::load(&path, directory.path())
                        .unwrap()
                        .is_some()
                );
            }
        }
    }

    #[test]
    fn saved_activity_rejects_unsupported_incomplete_and_unsafe_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let path = directory.path().join("run.json");
        let mut manager = RunManager::default();
        manager.request = Some(RunRequest {
            root: root.clone(),
            command: RunCommand::Test,
            options: RunOptions::default(),
        });
        manager.outcome = Some(RunOutcome::Completed);
        manager.progress.end(false, None);
        let snapshot = manager.snapshot().unwrap();
        let mut invalid = vec![snapshot.clone(); 3];
        invalid[0].version = SAVED_RUN_VERSION + 1;
        invalid[1].progress.finished = false;
        invalid[2].progress.deployment = Some(crate::progress::DeploymentResult {
            target: "Unsafe".into(),
            url: Some("file:///C:/untrusted.exe".into()),
        });
        for snapshot in invalid {
            std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
            assert!(RunSnapshot::load(&path, &root).is_err());
            assert!(RunManager::default().restore(snapshot).is_err());
        }
        snapshot.save(&path).unwrap();
        assert!(RunSnapshot::load(&path, &root).unwrap().is_some());
    }

    pub(crate) fn polling_fixture() -> (RunManager, Arc<Mutex<LiveProgress>>) {
        let (_, logs) = mpsc::sync_channel(1);
        let (cancel, _) = mpsc::sync_channel(1);
        let progress = Arc::new(Mutex::new(LiveProgress::default()));
        let mut manager = RunManager::default();
        manager.active = Some(RunHandle {
            logs,
            cancel,
            worker: None,
            omitted: Arc::new(AtomicUsize::new(0)),
            progress: progress.clone(),
            _events_file: None,
        });
        (manager, progress)
    }

    #[test]
    fn completed_deployments_persist_urls_without_replacing_them_on_failure() -> Result<()> {
        use super::super::deployment::{DeployTarget, DeployTargets, DeploymentJob};

        let root = tempfile::tempdir()?;
        let path = root.path().join("targets.json");
        let history = root.path().join("activity.json");
        let target: DeployTarget = serde_json::from_value(serde_json::json!({
            "id": "qa", "name": "QA", "publishedUrl": "https://previous.example.com/"
        }))?;
        for (outcome, has_url) in [
            (RunOutcome::Completed, true),
            (RunOutcome::Completed, false),
            (RunOutcome::Failed("Upload failed".into()), true),
            (RunOutcome::Cancelled, true),
        ] {
            DeployTargets {
                targets: vec![target.clone()],
                selected: Some(target.id.clone()),
                ..Default::default()
            }
            .save(&path)?;
            DeployTargets::record_report_publication(
                &path,
                &target,
                Some("https://previous.example.com/"),
                &["older-report".into()],
            )?;
            let mut request = request(RunCommand::Deploy);
            request.root = root.path().into();
            request.options.deployment = Some(DeploymentJob {
                target: target.clone(),
                suite_ids: vec!["reports".into()],
                catalog_ids: vec!["reports".into()],
                partial_confirmed: false,
            });
            let mut manager = RunManager::default();
            manager.request = Some(request);
            manager.outcome = Some(outcome.clone());
            if has_url {
                manager.progress.deployment = Some(crate::progress::DeploymentResult {
                    target: "Display name is not the target ID".into(),
                    url: Some("https://latest.example.com/".into()),
                });
            }
            manager.progress.end(false, None);
            let snapshot = manager.snapshot().unwrap();
            snapshot.save_completed(&history, Some(&path))?;
            let expected = if matches!(outcome, RunOutcome::Completed) && has_url {
                "https://latest.example.com/"
            } else {
                "https://previous.example.com/"
            };
            assert_eq!(
                DeployTargets::load(&path)?.targets[0]
                    .published_url
                    .as_deref(),
                Some(expected)
            );
            let settings = DeployTargets::load(&path)?;
            let completed = matches!(outcome, RunOutcome::Completed) && has_url;
            assert_eq!(
                settings.latest_report_url(&["reports".into()]).as_deref(),
                completed.then_some("https://latest.example.com/suites/reports")
            );
            assert_eq!(
                settings
                    .latest_report_url(&["older-report".into()])
                    .as_deref(),
                (!completed).then_some("https://previous.example.com/suites/older-report")
            );
            assert!(RunSnapshot::load(&history, root.path())?.is_some());
            if has_url && matches!(outcome, RunOutcome::Completed) {
                let failed_history = root.path().join("failed-save-activity.json");
                let error = snapshot
                    .save_completed(&failed_history, Some(root.path()))
                    .unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("Site deployed, but its URL could not be saved")
                );
                assert!(RunSnapshot::load(&failed_history, root.path())?.is_some());
            }
        }
        Ok(())
    }

    #[test]
    fn polling_unchanged_progress_reuses_the_snapshot() {
        let (mut manager, shared) = polling_fixture();
        shared.lock().unwrap().apply(crate::progress::Record {
            version: 1,
            elapsed_ms: 1,
            event: crate::progress::Event::Phase {
                name: "Capturing".repeat(1000),
            },
        });
        assert!(!manager.poll());
        let allocation = manager.progress.phase.as_ptr();
        let revision = manager.revision();
        for _ in 0..100 {
            assert!(!manager.poll());
            assert_eq!(manager.progress.phase.as_ptr(), allocation);
            assert_eq!(manager.revision(), revision);
        }
        shared.lock().unwrap().end(true, None);
        manager.poll();
        assert!(manager.progress.finished);
        assert_eq!(manager.progress.phase, "Cancelled");
        assert_ne!(manager.revision(), revision);
    }

    #[test]
    fn polling_does_not_wait_for_the_worker_progress_lock() {
        let (mut manager, shared) = polling_fixture();
        let held = shared.lock().unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            assert!(!manager.poll());
            sender.send(manager).unwrap();
        });
        let result = receiver.recv_timeout(Duration::from_secs(1));
        drop(held);
        worker.join().unwrap();
        assert!(
            result.is_ok(),
            "The UI poll must not block behind the event reader"
        );
    }

    #[test]
    fn polling_bounds_console_work_per_tick_without_losing_chunks() {
        let (mut manager, _) = polling_fixture();
        let (sender, receiver) = mpsc::channel();
        manager.active.as_mut().unwrap().logs = receiver;
        for index in 0..LOG_QUEUE_CAPACITY * 2 {
            sender
                .send(LogLine {
                    stderr: false,
                    text: format!("{index}\n"),
                })
                .unwrap();
        }
        manager.poll();
        assert_eq!(manager.logs.len(), LOG_QUEUE_CAPACITY);
        manager.poll();
        assert_eq!(manager.logs.len(), LOG_QUEUE_CAPACITY * 2);
        assert_eq!(
            manager.logs.back().unwrap().text,
            format!("{}\n", LOG_QUEUE_CAPACITY * 2 - 1)
        );
    }

    #[test]
    fn snapshot_arguments_include_suites_but_not_scenario_filters() {
        let mut request = request(RunCommand::Snapshot);
        request.options.suites = vec!["shop-output".into()];
        request.options.scenarios = vec!["shop:home".parse().unwrap()];
        let arguments = request.arguments().unwrap();
        assert!(arguments.contains(&OsString::from("--test-suite")));
        assert!(arguments.contains(&OsString::from("shop-output")));
        assert!(!arguments.contains(&OsString::from("--scenario")));
        assert!(RunOptions::applies("suites", RunCommand::Snapshot));
        assert!(!RunOptions::applies("scenarios", RunCommand::Snapshot));
    }

    fn request(command: RunCommand) -> RunRequest {
        RunRequest {
            root: PathBuf::from("C:/QA projects/Shop & checkout"),
            command,
            options: RunOptions::default(),
        }
    }

    #[test]
    fn all_command_arguments_match_clap() {
        for command in RunCommand::ALL {
            let request = request(*command);
            assert!(request.arguments().is_ok(), "{command:?}");
        }
        let mut request = request(RunCommand::Test);
        request.options.suites = vec!["shop & checkout".into(), "mobile".into()];
        request.options.site_env = "staging".into();
        request.options.regressify_dir = "output with spaces".into();
        request.options.fetch_browser = Some(false);
        request.options.required_login = true;
        request.options.replacement_profile = "QA ; profile".into();
        let arguments = request.arguments().unwrap();
        assert!(arguments.contains(&OsString::from("shop & checkout")));
        assert!(arguments.contains(&OsString::from("QA ; profile")));
        assert_eq!(arguments[1], request.root.as_os_str());

        let mut ref_request = super::tests::request(RunCommand::Reference);
        ref_request.options.replacement_profile = "staging".into();
        let ref_arguments = ref_request.arguments().unwrap();
        assert!(ref_arguments.contains(&OsString::from("--replacement-profile")));
        assert!(ref_arguments.contains(&OsString::from("staging")));
        assert!(RunOptions::applies(
            "replacementProfile",
            RunCommand::Reference
        ));
        assert!(RunOptions::applies("replacementProfile", RunCommand::Test));
    }

    #[test]
    fn report_approval_arguments_keep_the_capture_and_reject_broadened_scope() {
        let mut request = request(RunCommand::Approve);
        request.options.suites = vec!["shop-output".into()];
        request.options.scenarios = vec!["shop-output:checkout".parse().unwrap()];
        request.options.run_id = "reviewed".into();
        request.options.report_capture = Some("checkout_mobile__$payment".into());
        let crate::cli::Command::Approve(args) = request.cli().unwrap().command else {
            panic!("Expected approval");
        };
        assert_eq!(args.report_capture, request.options.report_capture);
        let round_trip: RunRequest =
            serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
        assert_eq!(
            round_trip.options.report_capture,
            request.options.report_capture
        );
        request.command = RunCommand::Compare;
        assert!(request.arguments().is_err());
        request.command = RunCommand::Approve;
        request
            .options
            .scenarios
            .push("shop-output:other".parse().unwrap());
        assert!(request.arguments().is_err());
        request.options.scenarios.pop();
        request.options.run_id.clear();
        assert!(request.arguments().is_err());
        assert!(!RunOptions::applies("reportCapture", RunCommand::Approve));
    }

    #[test]
    fn invalid_options_fail_before_process_launch() {
        let mut request = request(RunCommand::Test);
        request.options.async_capture_limit = 0;
        assert!(request.arguments().is_err());
        request.options.async_capture_limit = 1;
        request.options.site_env = "../escape".into();
        assert!(request.arguments().is_err());
        request.options.site_env.clear();
        request.options.mis_match_threshold = f64::NAN;
        assert!(request.arguments().is_err());
    }

    #[test]
    fn output_directory_matches_cli_precedence() {
        let mut options = RunOptions::default();
        let root = Path::new("workspace");
        assert_eq!(options.output_directory(root), root.join(".regressify"));
        options.site_env = "staging".into();
        assert_eq!(
            options.output_directory(root),
            root.join(".regressify__staging")
        );
        options.regressify_dir = "artifacts".into();
        assert_eq!(options.output_directory(root), root.join("artifacts"));
    }

    fn fixture(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "desktop::runner::tests::process_fixture",
                "--nocapture",
            ])
            .env("REGRESSIFY_DESKTOP_FIXTURE", mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    #[test]
    fn process_fixture() {
        let Ok(mode) = std::env::var("REGRESSIFY_DESKTOP_FIXTURE") else {
            return;
        };
        #[cfg(windows)]
        if mode == "no-console" {
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetConsoleWindow() -> *mut std::ffi::c_void;
            }
            assert!(
                unsafe { GetConsoleWindow() }.is_null(),
                "Worker process unexpectedly created a console"
            );
        }
        println!("fixture stdout");
        eprintln!("fixture stderr");
        if mode == "progress" {
            crate::diagnostics::install::install_subscriber();
            crate::diagnostics::install::install_subscriber();
            let path = std::env::var_os("REGRESSIFY_PROGRESS_FIXTURE").unwrap();
            crate::progress::install(Path::new(&path)).unwrap();
            for index in 0..2000 {
                println!("log record {index}");
            }
            crate::progress::emit(crate::progress::Event::Queued {
                job: crate::progress::Job {
                    id: 1,
                    suite_id: "shop".into(),
                    suite_name: "shop".into(),
                    scenario_id: "home".into(),
                    label: "Home".into(),
                    viewport: "desktop".into(),
                    captures: vec!["home__FINAL".into()],
                },
            });
            crate::progress::emit(crate::progress::Event::Started { id: 1 });
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(crate::progress::scope_scenario(1, async {
                    crate::progress::scenario_log!("Scoped fixture action");
                }));
            crate::progress::emit(crate::progress::Event::Captured {
                id: 1,
                error: None,
                infrastructure: false,
            });
            crate::progress::emit(crate::progress::Event::Finished { error: None });
            crate::progress::finish().unwrap();
        }
        if mode == "wait" {
            thread::park();
        }
        if mode == "orphan-child" {
            let lock_path = std::env::var_os("REGRESSIFY_ORPHAN_LOCK").unwrap();
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(lock_path)
                .unwrap();
            lock.lock().unwrap();
            println!("orphan ready");
            // Signal the parent only after the ready line is in the shared pipe.
            (&lock).write_all(b"ready").unwrap();
            thread::park();
        }
        if mode == "orphan-pipe" {
            let lock_path = std::env::var_os("REGRESSIFY_ORPHAN_LOCK").unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "desktop::runner::tests::process_fixture",
                    "--nocapture",
                ])
                .env("REGRESSIFY_DESKTOP_FIXTURE", "orphan-child")
                .env("REGRESSIFY_ORPHAN_LOCK", &lock_path)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            // Exit only once the descendant is ready. Otherwise slow process startup
            // under parallel test load races the worker's short pipe-drain grace.
            while !std::fs::metadata(&lock_path).is_ok_and(|metadata| metadata.len() > 0) {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "Orphan descendant exited before becoming ready"
                );
                thread::sleep(Duration::from_millis(10));
            }
            // Intentionally leave the descendant running when the parent exits so
            // pipe-drain tests verify process-group cleanup.
            std::mem::forget(child);
            return;
        }
        if mode == "fail" {
            std::process::exit(7);
        }
    }

    #[test]
    #[cfg(windows)]
    fn bundled_workers_do_not_create_a_console_window() {
        let mut handle = RunHandle::spawn(fixture("no-console"), None, None, None).unwrap();
        let outcome = handle.worker.take().unwrap().join().unwrap();
        let output = handle
            .logs
            .try_iter()
            .map(|line| line.text)
            .collect::<String>();
        assert_eq!(outcome, RunOutcome::Completed, "{output}");
    }

    #[test]
    fn repeated_diagnostic_install_keeps_fixture_output_single() {
        for _ in 0..2 {
            let events = tempfile::NamedTempFile::new().unwrap();
            let output = fixture("progress")
                .env("REGRESSIFY_PROGRESS_FIXTURE", events.path())
                .output()
                .unwrap();
            assert!(output.status.success());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(stderr.matches("fixture stderr").count(), 1);
            assert_eq!(stderr.matches("Scoped fixture action").count(), 1);
        }
    }

    #[test]
    fn subprocess_reads_both_streams_and_distinguishes_failure() {
        for (mode, success) in [("complete", true), ("fail", false)] {
            let mut handle = RunHandle::spawn(fixture(mode), None, None, None).unwrap();
            let outcome = handle.worker.take().unwrap().join().unwrap();
            assert_eq!(outcome == RunOutcome::Completed, success);
            let logs = handle.logs.try_iter().collect::<Vec<_>>();
            assert!(
                logs.iter()
                    .any(|line| !line.stderr && line.text.contains("fixture stdout"))
            );
            assert!(
                logs.iter()
                    .any(|line| line.stderr && line.text.contains("fixture stderr"))
            );
        }
    }

    #[test]
    fn cancellation_yields_before_draining_a_progress_backlog() {
        let mut events = tempfile::NamedTempFile::new().unwrap();
        for elapsed_ms in 0..10_000 {
            serde_json::to_writer(
                &mut events,
                &crate::progress::Record {
                    version: 1,
                    elapsed_ms,
                    event: crate::progress::Event::Phase {
                        name: "Capturing".into(),
                    },
                },
            )
            .unwrap();
            events.write_all(b"\n").unwrap();
        }
        events.flush().unwrap();
        let mut handle = RunHandle::spawn(fixture("wait"), None, Some(events), None).unwrap();
        handle.cancel();
        assert_eq!(
            handle.worker.take().unwrap().join().unwrap(),
            RunOutcome::Cancelled
        );
        let progress = handle.progress.lock().unwrap();
        assert!(progress.finished);
        assert!(
            progress.revision() < 10_000,
            "Cancellation must be checked before consuming the entire event backlog"
        );
    }

    #[test]
    fn cancellation_stops_a_running_process_group() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().to_path_buf();
        std::fs::write(path.join("package.zip"), b"synthetic package").unwrap();
        let mut handle = RunHandle::spawn(fixture("wait"), None, None, Some(scratch)).unwrap();
        handle.logs.recv_timeout(Duration::from_secs(10)).unwrap();
        handle.cancel();
        assert_eq!(
            handle.worker.take().unwrap().join().unwrap(),
            RunOutcome::Cancelled
        );
        assert!(
            !path.exists(),
            "cancelled worker left its staging files behind"
        );
    }

    #[test]
    fn descendant_retaining_pipes_after_parent_exit_is_reaped_on_completion_and_cancellation() {
        for cancel_after_parent_exit in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let lock_path = directory.path().join("orphan.lock");
            let mut command = fixture("orphan-pipe");
            command.env("REGRESSIFY_ORPHAN_LOCK", &lock_path);
            let mut handle = RunHandle::spawn(command, None, None, None).unwrap();
            // The log channel disconnects if every pipe closes, so this observes
            // either readiness or a failed fixture without a startup-time budget.
            loop {
                let line = handle
                    .logs
                    .recv()
                    .expect("orphan child must emit ready line");
                if line.text.contains("orphan ready") {
                    break;
                }
            }
            let started = std::time::Instant::now();
            if cancel_after_parent_exit {
                handle.cancel();
            }
            let outcome = handle.worker.take().unwrap().join().unwrap();
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "worker join hung waiting on descendant pipes"
            );
            if cancel_after_parent_exit {
                assert!(matches!(
                    outcome,
                    RunOutcome::Cancelled | RunOutcome::Completed
                ));
            } else {
                assert_eq!(outcome, RunOutcome::Completed);
            }
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .unwrap();
            assert!(
                lock.try_lock().is_ok(),
                "descendant process still holds the lock after RunHandle finished"
            );
        }
    }

    #[test]
    fn scenario_selection_reaches_each_supported_worker_command() {
        for command in [
            RunCommand::Test,
            RunCommand::Reference,
            RunCommand::Compare,
            RunCommand::Approve,
        ] {
            let request = RunRequest {
                root: PathBuf::from("."),
                command,
                options: RunOptions {
                    suites: vec!["shop".into()],
                    scenarios: vec!["shop:checkout".parse().unwrap()],
                    ..Default::default()
                },
            };
            request.cli().unwrap();
            assert!(request.arguments().unwrap().windows(2).any(|pair| {
                pair == [
                    OsString::from("--scenario"),
                    OsString::from("shop:checkout"),
                ]
            }));
            let restored: RunRequest =
                serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
            assert_eq!(restored.options.scenarios, request.options.scenarios);
        }
    }

    #[test]
    fn log_memory_is_bounded() {
        let mut manager = RunManager::default();
        for _ in 0..300 {
            manager.push_log(LogLine {
                stderr: false,
                text: "x".repeat(MAX_LOG_CHUNK),
            });
        }
        assert!(manager.log_bytes <= MAX_LOG_BYTES);
        assert!(!manager.logs.is_empty());
    }

    #[test]
    fn progress_survives_a_full_console_log_queue() {
        let events = tempfile::NamedTempFile::new().unwrap();
        let mut command = fixture("progress");
        command.env("REGRESSIFY_PROGRESS_FIXTURE", events.path());
        let mut handle = RunHandle::spawn(command, None, Some(events), None).unwrap();
        let outcome = handle.worker.take().unwrap().join().unwrap();
        assert_eq!(outcome, RunOutcome::Completed);
        assert!(handle.omitted.load(Ordering::Relaxed) > 0);
        let progress = handle.progress.lock().unwrap();
        assert!(progress.finished);
        assert_eq!(progress.counts(None).captured, 1);
        assert!(progress.logs.iter().any(|entry| entry.job_id == 1 && entry.message.as_ref() == "Scoped fixture action"));
    }
}
