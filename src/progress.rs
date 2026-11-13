//! Structured run telemetry, independent of console output and the UI framework.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

const VERSION: u8 = 1;
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const EVENT_BATCH_BYTES: usize = 4 * MAX_EVENT_BYTES;
const EVENT_BATCH_RECORDS: usize = 256;
const MAX_SCENARIO_LOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_SCENARIO_LOG_ENTRIES: usize = 4096;
const MAX_SCENARIO_LOG_MESSAGE_BYTES: usize = 16 * 1024;
static SINK: OnceLock<Mutex<EventWriter>> = OnceLock::new();

tokio::task_local! {
    static SCENARIO_JOB: usize;
}

pub(crate) async fn scope_scenario<Work: std::future::Future>(
    id: usize,
    work: Work,
) -> Work::Output {
    SCENARIO_JOB.scope(id, work).await
}

macro_rules! scenario_log {
    ($($message:tt)*) => {
        $crate::progress::write_scenario_log(format_args!($($message)*))
    };
}
pub(crate) use scenario_log;

/// Attribute messages before concurrent console streams are interleaved.
pub(crate) fn write_scenario_log(message: std::fmt::Arguments<'_>) {
    if enabled()
        && let Ok(id) = SCENARIO_JOB.try_with(|id| *id)
    {
        let message = message.to_string();
        eprintln!("{message}");
        emit(Event::Log {
            id,
            message: bounded_log_message(message),
        });
    } else {
        eprintln!("{message}");
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: usize,
    pub suite_id: String,
    pub suite_name: String,
    pub scenario_id: String,
    pub label: String,
    pub viewport: String,
    pub captures: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "event",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    Phase {
        name: String,
    },
    SuiteQueued {
        id: String,
        name: String,
    },
    Queued {
        job: Job,
    },
    Started {
        id: usize,
    },
    Log {
        id: usize,
        message: String,
    },
    Captured {
        id: usize,
        error: Option<String>,
        infrastructure: bool,
    },
    JobComparing {
        id: usize,
        error: Option<String>,
    },
    JobApproved {
        id: usize,
        error: Option<String>,
    },
    Comparing {
        suite_id: String,
    },
    Compared {
        suite_id: String,
        capture: String,
        label: String,
        #[serde(default)]
        scenario_id: Option<String>,
        passed: bool,
        error: Option<String>,
    },
    SuiteFinished {
        id: String,
    },
    Deployed {
        target: String,
        url: Option<String>,
        /// Content fingerprint of a report deployment; landing deployments have none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
        /// Snapshot input fingerprint the published content was generated from, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_fingerprint: Option<String>,
        /// The target already served this content, so nothing was uploaded.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unchanged: bool,
        /// Every suite the site serves after a publication, including kept suites.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suites: Option<Vec<String>>,
    },
    Finished {
        error: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub version: u8,
    pub elapsed_ms: u64,
    pub event: Event,
}

struct EventWriter {
    output: BufWriter<File>,
    started: Instant,
    error: Option<String>,
}

impl EventWriter {
    fn write(&mut self, event: Event) -> Result<()> {
        let record = Record {
            version: VERSION,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            event,
        };
        let bytes = serde_json::to_vec(&record)?;
        ensure!(
            bytes.len() <= MAX_EVENT_BYTES,
            "Progress event exceeds 1 MiB"
        );
        self.output.write_all(&bytes)?;
        self.output.write_all(b"\n")?;
        self.output.flush()?;
        Ok(())
    }
}

/// The parent creates this private file; the worker only opens the existing path.
pub fn install(path: &Path) -> Result<()> {
    let output = OpenOptions::new().write(true).truncate(true).open(path)?;
    SINK.set(Mutex::new(EventWriter {
        output: BufWriter::new(output),
        started: Instant::now(),
        error: None,
    }))
    .map_err(|_| anyhow!("Progress transport is already initialized"))
}

pub fn enabled() -> bool {
    SINK.get().is_some()
}

/// Telemetry failures must not interrupt sibling scenarios; finish reports them.
pub fn emit(event: Event) {
    if let Some(sink) = SINK.get()
        && let Ok(mut sink) = sink.lock()
        && sink.error.is_none()
        && let Err(error) = sink.write(event)
    {
        sink.error = Some(error.to_string());
    }
}

pub fn finish() -> Result<()> {
    if let Some(sink) = SINK.get() {
        let mut sink = sink
            .lock()
            .map_err(|_| anyhow!("Progress writer is unavailable"))?;
        if let Some(error) = &sink.error {
            return Err(anyhow!("Progress transport failed: {error}"));
        }
        sink.output.flush()?;
    }
    Ok(())
}

pub struct EventReader {
    input: BufReader<File>,
    pending: Vec<u8>,
}

impl EventReader {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            input: BufReader::new(File::open(path)?),
            pending: Vec::new(),
        })
    }

    /// Regular-file EOF is temporary while the worker is still appending records.
    pub fn drain(&mut self, receive: impl FnMut(Record)) -> Result<()> {
        self.drain_bounded(usize::MAX, usize::MAX, receive)
    }

    pub fn drain_batch(&mut self, receive: impl FnMut(Record)) -> Result<()> {
        self.drain_bounded(EVENT_BATCH_BYTES, EVENT_BATCH_RECORDS, receive)
    }

    fn drain_bounded(
        &mut self,
        mut bytes: usize,
        mut records: usize,
        mut receive: impl FnMut(Record),
    ) -> Result<()> {
        while bytes > 0 && records > 0 {
            let available = self.input.fill_buf()?;
            if available.is_empty() {
                return Ok(());
            }
            let end = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|position| position + 1)
                .unwrap_or(available.len())
                .min(bytes);
            self.pending.extend_from_slice(&available[..end]);
            self.input.consume(end);
            bytes -= end;
            ensure!(
                self.pending.len() <= MAX_EVENT_BYTES + 1,
                "Progress record exceeds 1 MiB"
            );
            if self.pending.last() == Some(&b'\n') {
                let record: Record = serde_json::from_slice(&self.pending)?;
                ensure!(
                    record.version == VERSION,
                    "Unsupported progress protocol version"
                );
                receive(record);
                self.pending.clear();
                records -= 1;
            }
        }
        Ok(())
    }

    pub fn complete(&self) -> Result<()> {
        ensure!(
            self.pending.is_empty(),
            "Worker ended with an incomplete progress record"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Queued,
    Running,
    Captured,
    Comparing,
    Passed,
    Failed,
    Cancelled,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Captured => "Captured",
            Self::Comparing => "Comparing",
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
    pub fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Comparing)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioProgress {
    pub job: Job,
    pub status: Status,
    pub message: Option<String>,
    pub infrastructure: bool,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    compared: HashMap<String, bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteProgress {
    pub id: String,
    pub name: String,
    pub finished: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioLog {
    pub job_id: usize,
    pub elapsed_ms: u64,
    pub message: Arc<str>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveProgress {
    pub deployment: Option<DeploymentResult>,
    pub phase: String,
    pub suites: BTreeMap<String, SuiteProgress>,
    pub scenarios: BTreeMap<usize, ScenarioProgress>,
    pub elapsed_ms: u64,
    pub finished: bool,
    pub error: Option<String>,
    pub logs: VecDeque<ScenarioLog>,
    pub omitted_logs: usize,
    log_bytes: usize,
    #[serde(with = "capture_index")]
    captures: HashMap<(String, String), usize>,
    #[serde(skip)]
    revision: u64,
}

mod capture_index {
    use super::*;

    pub fn serialize<S: serde::Serializer>(
        captures: &HashMap<(String, String), usize>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        captures.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<HashMap<(String, String), usize>, D::Error> {
        Ok(Vec::<((String, String), usize)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentResult {
    pub target: String,
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_fingerprint: Option<String>,
    /// Skipped because the target already served identical content.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unchanged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suites: Option<Vec<String>>,
}

impl DeploymentResult {
    pub fn has_safe_url(&self) -> bool {
        self.url.as_ref().is_none_or(|url| {
            reqwest::Url::parse(url).is_ok_and(|parsed| {
                parsed.scheme() == "https"
                    && parsed.host_str().is_some()
                    && parsed.username().is_empty()
                    && parsed.password().is_none()
                    && parsed.query().is_none()
                    && parsed.fragment().is_none()
            })
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Counts {
    pub total: usize,
    pub queued: usize,
    pub running: usize,
    pub captured: usize,
    pub passed: usize,
    pub failed: usize,
    pub cancelled: usize,
}

impl Counts {
    pub fn complete(self) -> usize {
        self.captured + self.passed + self.failed + self.cancelled
    }
    pub fn fraction(self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.complete() as f32 / self.total as f32
        }
    }
}

impl LiveProgress {
    #[cfg(feature = "desktop")]
    pub(crate) fn remap_suite(&mut self, old_id: &str, new: Option<(&str, &str)>) {
        if let Some(mut suite) = self.suites.remove(old_id)
            && let Some((id, name)) = new
        {
            suite.id = id.into();
            suite.name = name.into();
            self.suites.insert(id.into(), suite);
        }
        self.scenarios.retain(|_, scenario| {
            if scenario.job.suite_id != old_id {
                return true;
            }
            if let Some((id, name)) = new {
                scenario.job.suite_id = id.into();
                scenario.job.suite_name = name.into();
                true
            } else {
                false
            }
        });
        self.captures = std::mem::take(&mut self.captures)
            .into_iter()
            .filter_map(|((id, capture), job)| {
                if id != old_id {
                    Some(((id, capture), job))
                } else {
                    new.map(|(id, _)| ((id.into(), capture), job))
                }
            })
            .collect();
        self.logs
            .retain(|entry| self.scenarios.contains_key(&entry.job_id));
        self.log_bytes = self.logs.iter().map(|entry| entry.message.len()).sum();
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn clear_logs(&mut self) {
        self.logs.clear();
        self.log_bytes = 0;
        self.omitted_logs = 0;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn apply(&mut self, record: Record) {
        self.revision = self.revision.wrapping_add(1);
        self.elapsed_ms = self.elapsed_ms.max(record.elapsed_ms);
        match record.event {
            Event::Phase { name } => self.phase = name,
            Event::SuiteQueued { id, name } => {
                self.suites.entry(id.clone()).or_insert(SuiteProgress {
                    id,
                    name,
                    finished: false,
                });
            }
            Event::Queued { job } => {
                let id = job.id;
                self.queue(job);
                self.push_log(id, record.elapsed_ms, "Queued".into());
            }
            Event::Started { id } => {
                self.push_log(id, record.elapsed_ms, "Started".into());
                if let Some(scenario) = self.scenarios.get_mut(&id) {
                    scenario.status = Status::Running;
                    scenario.started_ms = Some(record.elapsed_ms);
                }
            }
            Event::Log { id, message } => self.push_log(id, record.elapsed_ms, message),
            Event::Captured {
                id,
                error,
                infrastructure,
            } => {
                self.push_log(
                    id,
                    record.elapsed_ms,
                    error
                        .as_ref()
                        .map(|error| {
                            format!(
                                "{}: {error}",
                                if infrastructure {
                                    "Infrastructure failure"
                                } else {
                                    "Capture failed"
                                }
                            )
                        })
                        .unwrap_or_else(|| "Capture completed".into()),
                );
                if let Some(scenario) = self.scenarios.get_mut(&id) {
                    scenario.status = if error.is_some() {
                        Status::Failed
                    } else {
                        Status::Captured
                    };
                    scenario.message = error;
                    scenario.infrastructure = infrastructure;
                    scenario.finished_ms = Some(record.elapsed_ms);
                }
            }
            Event::JobComparing { id, error } => {
                self.push_log(
                    id,
                    record.elapsed_ms,
                    error
                        .as_ref()
                        .map(|error| format!("Capture failed: {error}"))
                        .unwrap_or_else(|| "Capture completed".into()),
                );
                self.push_log(id, record.elapsed_ms, "Comparing screenshots".into());
                if let Some(scenario) = self.scenarios.get_mut(&id) {
                    scenario.status = Status::Comparing;
                    scenario.message = error;
                    scenario.infrastructure = false;
                }
            }
            Event::JobApproved { id, error } => {
                self.push_log(
                    id,
                    record.elapsed_ms,
                    error
                        .as_ref()
                        .map(|error| format!("Capture failed: {error}"))
                        .unwrap_or_else(|| "Capture completed".into()),
                );
                if let Some(scenario) = self.scenarios.get_mut(&id) {
                    let passed = error.is_none();
                    for capture in scenario.job.captures.clone() {
                        scenario.compared.insert(capture, passed);
                    }
                    scenario.status = if passed {
                        Status::Passed
                    } else {
                        Status::Failed
                    };
                    scenario.message = error;
                    scenario.infrastructure = false;
                    scenario.finished_ms = Some(record.elapsed_ms);
                }
            }
            Event::Comparing { suite_id } => {
                self.phase = "Comparing screenshots".into();
                self.suites
                    .entry(suite_id.clone())
                    .or_insert(SuiteProgress {
                        id: suite_id.clone(),
                        name: suite_id.clone(),
                        finished: false,
                    });
                for scenario in self.scenarios.values_mut().filter(|scenario| {
                    scenario.job.suite_id == suite_id && scenario.status == Status::Captured
                }) {
                    scenario.status = Status::Comparing;
                }
                let jobs = self
                    .scenarios
                    .values()
                    .filter(|scenario| scenario.job.suite_id == suite_id)
                    .map(|scenario| scenario.job.id)
                    .collect::<Vec<_>>();
                for id in jobs {
                    self.push_log(id, record.elapsed_ms, "Comparing screenshots".into());
                }
            }
            Event::Compared {
                suite_id,
                capture,
                label,
                scenario_id,
                passed,
                error,
            } => {
                let key = (suite_id.clone(), capture.clone());
                let id = if let Some(id) = self.captures.get(&key) {
                    *id
                } else if let Some(scenario) = self.scenarios.values_mut().find(|scenario| {
                    scenario.job.suite_id == suite_id
                        && scenario.job.viewport.is_empty()
                        && scenario_id.as_deref() == Some(scenario.job.scenario_id.as_str())
                }) {
                    scenario.job.captures.push(capture.clone());
                    self.captures.insert(key, scenario.job.id);
                    scenario.job.id
                } else {
                    let id = self.scenarios.keys().next_back().copied().unwrap_or(0) + 1;
                    self.queue(Job {
                        id,
                        suite_id: suite_id.clone(),
                        suite_name: suite_id,
                        scenario_id: scenario_id.unwrap_or_else(|| capture.clone()),
                        label,
                        viewport: String::new(),
                        captures: vec![capture.clone()],
                    });
                    id
                };
                self.push_log(
                    id,
                    record.elapsed_ms,
                    format!(
                        "Comparison {}: {capture}{}",
                        if passed { "passed" } else { "failed" },
                        error
                            .as_ref()
                            .map(|error| format!("\n{error}"))
                            .unwrap_or_default()
                    ),
                );
                if let Some(scenario) = self.scenarios.get_mut(&id) {
                    scenario.compared.insert(capture, passed);
                    scenario.status = if scenario.compared.len() < scenario.job.captures.len() {
                        Status::Comparing
                    } else if scenario.compared.values().any(|passed| !passed) {
                        Status::Failed
                    } else {
                        Status::Passed
                    };
                    if !scenario.infrastructure {
                        if error.is_some() {
                            scenario.message = error;
                        } else if scenario.status == Status::Passed {
                            scenario.message = None;
                        }
                    }
                    if !scenario.status.active() {
                        scenario.finished_ms = Some(record.elapsed_ms);
                    }
                }
            }
            Event::SuiteFinished { id } => {
                if let Some(suite) = self.suites.get_mut(&id) {
                    suite.finished = true;
                }
            }
            Event::Deployed {
                target,
                url,
                fingerprint,
                input_fingerprint,
                unchanged,
                suites,
            } => {
                let deployment = DeploymentResult {
                    target,
                    url,
                    fingerprint,
                    input_fingerprint,
                    unchanged,
                    suites,
                };
                if deployment.has_safe_url() {
                    self.deployment = Some(deployment);
                }
            }
            Event::Finished { error } => self.end(false, error),
        }
    }

    fn push_log(&mut self, id: usize, elapsed_ms: u64, message: String) {
        if !self.scenarios.contains_key(&id) {
            return;
        }
        let message = bounded_log_message(message);
        self.log_bytes += message.len();
        self.logs.push_back(ScenarioLog {
            job_id: id,
            elapsed_ms,
            message: message.into(),
        });
        while self.log_bytes > MAX_SCENARIO_LOG_BYTES || self.logs.len() > MAX_SCENARIO_LOG_ENTRIES
        {
            if let Some(entry) = self.logs.pop_front() {
                self.log_bytes -= entry.message.len();
                self.omitted_logs += 1;
            }
        }
    }

    fn queue(&mut self, job: Job) {
        self.suites
            .entry(job.suite_id.clone())
            .or_insert(SuiteProgress {
                id: job.suite_id.clone(),
                name: job.suite_name.clone(),
                finished: false,
            });
        for capture in &job.captures {
            self.captures
                .insert((job.suite_id.clone(), capture.clone()), job.id);
        }
        self.scenarios.entry(job.id).or_insert(ScenarioProgress {
            job,
            status: Status::Queued,
            message: None,
            infrastructure: false,
            started_ms: None,
            finished_ms: None,
            compared: HashMap::new(),
        });
    }

    pub fn end(&mut self, cancelled: bool, error: Option<String>) {
        self.revision = self.revision.wrapping_add(1);
        self.finished = true;
        if cancelled {
            self.phase = "Cancelled".into();
        } else if error.is_some() {
            self.phase = "Run failed".into();
        } else {
            self.phase = "Finished".into();
        }
        if error.is_some() {
            self.error = error.clone();
        }
        let mut ended = Vec::new();
        for scenario in self
            .scenarios
            .values_mut()
            .filter(|scenario| scenario.status.active())
        {
            scenario.status = if cancelled {
                Status::Cancelled
            } else {
                Status::Failed
            };
            scenario.infrastructure = !cancelled;
            scenario.message = error.clone().or_else(|| {
                Some(
                    if cancelled {
                        "Run cancelled"
                    } else {
                        "Worker ended before this capture finished"
                    }
                    .into(),
                )
            });
            scenario.finished_ms = Some(self.elapsed_ms);
            if let Some(message) = &scenario.message {
                ended.push((scenario.job.id, message.clone()));
            }
        }
        for (id, message) in ended {
            self.push_log(id, self.elapsed_ms, message);
        }
    }

    /// Report totals count images, not scenario groups or browser lifecycle failures.
    pub fn comparison_counts(&self, suite_id: Option<&str>) -> Counts {
        let mut counts = Counts::default();
        for scenario in self
            .scenarios
            .values()
            .filter(|scenario| suite_id.is_none_or(|id| scenario.job.suite_id == id))
        {
            for passed in scenario.compared.values() {
                counts.total += 1;
                if *passed {
                    counts.passed += 1;
                } else {
                    counts.failed += 1;
                }
            }
        }
        counts
    }

    pub fn counts(&self, suite_id: Option<&str>) -> Counts {
        let mut counts = Counts::default();
        for scenario in self
            .scenarios
            .values()
            .filter(|scenario| suite_id.is_none_or(|id| scenario.job.suite_id == id))
        {
            counts.total += 1;
            match scenario.status {
                Status::Queued => counts.queued += 1,
                Status::Running | Status::Comparing => counts.running += 1,
                Status::Captured => counts.captured += 1,
                Status::Passed => counts.passed += 1,
                Status::Failed => counts.failed += 1,
                Status::Cancelled => counts.cancelled += 1,
            }
        }
        counts
    }
}

fn bounded_log_message(mut message: String) -> String {
    const SUFFIX: &str = "\n[log message truncated]";
    if message.len() > MAX_SCENARIO_LOG_MESSAGE_BYTES {
        let mut cutoff = MAX_SCENARIO_LOG_MESSAGE_BYTES - SUFFIX.len();
        while !message.is_char_boundary(cutoff) {
            cutoff -= 1;
        }
        message.truncate(cutoff);
        message.push_str(SUFFIX);
    }
    message
}

#[cfg(test)]
mod tests {
    #[test]
    fn deployed_site_results_are_preserved_and_unsafe_urls_are_rejected() {
        let mut live = LiveProgress::default();
        for url in [
            "javascript:alert(1)",
            "https://user:secret@example.com",
            "https://example.com/?token=private",
        ] {
            live.apply(record(Event::Deployed {
                target: "QA".into(),
                url: Some(url.into()),
                fingerprint: None,
                input_fingerprint: None,
                unchanged: false,
                suites: None,
            }));
            assert!(live.deployment.is_none());
        }
        live.apply(record(Event::Deployed {
            target: "QA".into(),
            url: Some("https://reports.example.com/".into()),
            fingerprint: Some("a".repeat(64)),
            input_fingerprint: None,
            unchanged: true,
            suites: None,
        }));
        let deployed = live.deployment.as_ref().unwrap();
        assert!(deployed.unchanged);
        assert_eq!(
            deployed.fingerprint.as_deref(),
            Some("a".repeat(64).as_str())
        );
        live.apply(record(Event::Deployed {
            target: "QA".into(),
            url: Some("https://reports.example.com/".into()),
            fingerprint: None,
            input_fingerprint: None,
            unchanged: false,
            suites: None,
        }));
        live.apply(record(Event::Finished { error: None }));
        assert_eq!(live.deployment.as_ref().unwrap().target, "QA");
        assert_eq!(
            live.deployment.as_ref().unwrap().url.as_deref(),
            Some("https://reports.example.com/")
        );
        let legacy: DeploymentResult =
            serde_json::from_str(r#"{"target":"QA","url":"https://reports.example.com/"}"#)
                .unwrap();
        assert!(legacy.has_safe_url());
        assert!(!legacy.unchanged && legacy.fingerprint.is_none());
        // Workers from before skipped deployments omit both fields, and new results without
        // them keep the legacy wire format.
        let legacy_event: Record = serde_json::from_str(
            r#"{"version":1,"elapsedMs":0,"event":{"event":"deployed","target":"QA","url":null}}"#,
        )
        .unwrap();
        assert!(matches!(
            legacy_event.event,
            Event::Deployed {
                fingerprint: None,
                input_fingerprint: None,
                unchanged: false,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_string(&legacy).unwrap(),
            r#"{"target":"QA","url":"https://reports.example.com/"}"#
        );
        live.apply(record(Event::Deployed {
            target: "Private".into(),
            url: None,
            fingerprint: None,
            input_fingerprint: None,
            unchanged: false,
            suites: None,
        }));
        assert!(live.deployment.as_ref().unwrap().url.is_none());
    }
    use super::*;

    fn record(event: Event) -> Record {
        Record {
            version: VERSION,
            elapsed_ms: 100,
            event,
        }
    }
    fn job(id: usize, suite: &str) -> Job {
        Job {
            id,
            suite_id: suite.into(),
            suite_name: suite.into(),
            scenario_id: format!("scenario-{id}"),
            label: format!("Scenario {id}"),
            viewport: "desktop".into(),
            captures: vec![format!("capture-{id}")],
        }
    }

    #[test]
    fn saved_progress_preserves_results_timings_logs_and_capture_identity() {
        let mut live = LiveProgress::default();
        for (id, suite) in [(1, "shop"), (2, "account")] {
            let mut queued = job(id, suite);
            queued.captures = vec!["home".into()];
            live.apply(record(Event::Queued { job: queued }));
            live.apply(record(Event::Started { id }));
            live.apply(record(Event::Captured {
                id,
                error: None,
                infrastructure: false,
            }));
            live.apply(Record {
                version: VERSION,
                elapsed_ms: 27_100,
                event: Event::Compared {
                    suite_id: suite.into(),
                    capture: "home".into(),
                    label: "Home".into(),
                    scenario_id: Some(format!("scenario-{id}")),
                    passed: id == 2,
                    error: (id == 1).then(|| "Dimensions differ".into()),
                },
            });
        }
        live.end(false, None);
        let bytes = serde_json::to_vec(&live).unwrap();
        let mut restored: LiveProgress = serde_json::from_slice(&bytes).unwrap();
        assert!(restored.finished);
        assert_eq!(restored.elapsed_ms, 27_100);
        assert_eq!(restored.comparison_counts(None).passed, 1);
        assert_eq!(restored.comparison_counts(None).failed, 1);
        assert_eq!(
            restored.scenarios[&1].message.as_deref(),
            Some("Dimensions differ")
        );
        assert_eq!(restored.scenarios[&1].finished_ms, Some(27_100));
        assert_eq!(restored.logs.len(), live.logs.len());
        assert_eq!(restored.log_bytes, live.log_bytes);
        assert_eq!(
            restored.logs.back().unwrap().message,
            live.logs.back().unwrap().message
        );
        restored.apply(record(Event::Compared {
            suite_id: "shop".into(),
            capture: "home".into(),
            label: "Home".into(),
            scenario_id: Some("scenario-1".into()),
            passed: true,
            error: None,
        }));
        assert_eq!(restored.scenarios.len(), 2);
        assert_eq!(restored.comparison_counts(None).passed, 2);
        assert_eq!(restored.comparison_counts(None).failed, 0);
    }

    #[test]
    fn comparison_logs_group_recorded_scenario_ids_without_prefix_matching() {
        let mut live = LiveProgress::default();
        for (capture, scenario_id) in [
            ("home__FINAL", "home"),
            ("home__step", "home"),
            ("home-extra__FINAL", "home-extra"),
        ] {
            live.apply(record(Event::Compared {
                suite_id: "shop-output".into(),
                capture: capture.into(),
                label: scenario_id.into(),
                scenario_id: Some(scenario_id.into()),
                passed: true,
                error: None,
            }));
        }
        assert_eq!(live.scenarios.len(), 2);
        let home = live
            .scenarios
            .values()
            .find(|scenario| scenario.job.scenario_id == "home")
            .unwrap();
        assert_eq!(home.job.captures.len(), 2);
        let logs = live
            .logs
            .iter()
            .filter(|entry| entry.job_id == home.job.id)
            .collect::<Vec<_>>();
        assert_eq!(logs.len(), 2);
        assert!(
            logs.iter()
                .all(|entry| !entry.message.contains("home-extra"))
        );
    }

    #[test]
    fn scenario_logs_stay_attributed_when_jobs_interleave() {
        let mut live = LiveProgress::default();
        for id in [1, 2] {
            live.apply(record(Event::Queued {
                job: job(id, "shop"),
            }));
        }
        for (id, message) in [
            (1, "Navigate checkout"),
            (2, "Navigate account"),
            (1, "Click #submit"),
        ] {
            live.apply(record(Event::Log {
                id,
                message: message.into(),
            }));
        }
        live.apply(record(Event::Captured {
            id: 1,
            error: Some("Missing #submit".into()),
            infrastructure: false,
        }));
        live.end(true, None);
        let checkout = live
            .logs
            .iter()
            .filter(|entry| entry.job_id == 1)
            .map(|entry| entry.message.as_ref())
            .collect::<Vec<_>>();
        assert!(checkout.contains(&"Navigate checkout"));
        assert!(checkout.contains(&"Click #submit"));
        assert!(checkout.contains(&"Capture failed: Missing #submit"));
        assert!(!checkout.iter().any(|message| message.contains("account")));
        assert!(
            live.logs
                .iter()
                .any(|entry| entry.job_id == 2 && entry.message.as_ref() == "Run cancelled")
        );
        let count = live.logs.len();
        live.apply(record(Event::Log {
            id: 99,
            message: "Unattributed".into(),
        }));
        assert_eq!(live.logs.len(), count);
    }

    #[tokio::test]
    async fn scenario_log_context_is_isolated_across_parallel_tasks() {
        let mut tasks = Vec::new();
        for id in [1, 2] {
            tasks.push(tokio::spawn(scope_scenario(id, async move {
                tokio::task::yield_now().await;
                assert_eq!(SCENARIO_JOB.with(|current| *current), id);
                id
            })));
        }
        for (index, task) in tasks.into_iter().enumerate() {
            assert_eq!(task.await.unwrap(), index + 1);
        }
        assert!(SCENARIO_JOB.try_with(|current| *current).is_err());
    }

    #[test]
    fn scenario_log_history_bounds_bytes_entries_and_unicode_messages() {
        let mut live = LiveProgress::default();
        live.apply(record(Event::Queued {
            job: job(1, "shop"),
        }));
        for _ in 0..150 {
            live.apply(record(Event::Log {
                id: 1,
                message: "\u{754c}".repeat(MAX_SCENARIO_LOG_MESSAGE_BYTES),
            }));
        }
        assert!(live.log_bytes <= MAX_SCENARIO_LOG_BYTES);
        assert!(live.omitted_logs > 0);
        assert!(
            live.logs
                .back()
                .unwrap()
                .message
                .ends_with("[log message truncated]")
        );
        for _ in 0..MAX_SCENARIO_LOG_ENTRIES + 10 {
            live.apply(record(Event::Log {
                id: 1,
                message: "step".into(),
            }));
        }
        assert_eq!(live.logs.len(), MAX_SCENARIO_LOG_ENTRIES);
        assert!(
            live.logs
                .iter()
                .all(|entry| entry.message.as_ref() == "step")
        );
    }

    #[test]
    fn parallel_jobs_keep_independent_suite_and_scenario_statuses() {
        let mut live = LiveProgress::default();
        live.apply(record(Event::Queued {
            job: job(1, "shop"),
        }));
        live.apply(record(Event::Queued {
            job: job(2, "account"),
        }));
        live.apply(record(Event::Started { id: 2 }));
        assert_eq!(live.counts(Some("shop")).queued, 1);
        assert_eq!(live.counts(Some("account")).running, 1);
        live.apply(record(Event::Captured {
            id: 2,
            error: Some("Missing #login".into()),
            infrastructure: false,
        }));
        live.apply(record(Event::Started { id: 1 }));
        live.apply(record(Event::Captured {
            id: 1,
            error: None,
            infrastructure: false,
        }));
        assert_eq!(live.counts(None).captured, 1);
        assert_eq!(live.counts(None).failed, 1);
        assert_eq!(
            live.scenarios[&2].message.as_deref(),
            Some("Missing #login")
        );
    }

    #[test]
    fn passed_comparisons_preserve_cleanup_errors_without_becoming_visual_failures() {
        let mut live = LiveProgress::default();
        let error = "Browser shutdown was not confirmed after a successful scenario";
        live.apply(record(Event::Queued {
            job: job(1, "shop"),
        }));
        live.apply(record(Event::Started { id: 1 }));
        live.apply(record(Event::Captured {
            id: 1,
            error: Some(error.into()),
            infrastructure: true,
        }));
        live.apply(record(Event::Comparing {
            suite_id: "shop".into(),
        }));
        live.apply(record(Event::Compared {
            suite_id: "shop".into(),
            capture: "capture-1".into(),
            label: "Home".into(),
            scenario_id: Some("scenario-1".into()),
            passed: true,
            error: None,
        }));
        live.apply(record(Event::Finished {
            error: Some(error.into()),
        }));
        assert_eq!(live.scenarios[&1].status, Status::Passed);
        assert!(live.scenarios[&1].infrastructure);
        assert_eq!(live.scenarios[&1].message.as_deref(), Some(error));
        assert_eq!(live.error.as_deref(), Some(error));
        assert_eq!(live.comparison_counts(None).passed, 1);
        assert_eq!(live.comparison_counts(None).failed, 0);
    }

    #[test]
    fn report_totals_count_images_independently_of_scenario_groups() {
        let mut live = LiveProgress::default();
        for scenario in 0..4 {
            for viewport in ["phone", "tablet", "desktop"] {
                let scenario_id = format!("scenario-{scenario}");
                live.apply(record(Event::Compared {
                    suite_id: "alloy".into(),
                    capture: format!("{scenario_id}_{viewport}__FINAL"),
                    label: scenario_id.clone(),
                    scenario_id: Some(scenario_id),
                    passed: scenario != 3,
                    error: (scenario == 3).then(|| "Missing selector".into()),
                }));
            }
        }
        live.apply(record(Event::Finished { error: None }));
        assert_eq!(live.counts(None).total, 4);
        assert_eq!(live.counts(None).passed, 3);
        assert_eq!(live.counts(None).failed, 1);
        let comparisons = live.comparison_counts(Some("alloy"));
        assert_eq!(comparisons.total, 12);
        assert_eq!(comparisons.passed, 9);
        assert_eq!(comparisons.failed, 3);
        assert_eq!(live.comparison_counts(Some("other")).total, 0);
    }

    #[test]
    fn mixed_viewport_results_preserve_nine_passes_and_three_capture_failures() {
        let mut live = LiveProgress::default();
        let mut captures = Vec::new();
        for scenario in 0..4 {
            for viewport in ["phone", "tablet", "desktop"] {
                let id = captures.len() + 1;
                let scenario_id = format!("scenario-{scenario}");
                let capture = format!("{scenario_id}_{viewport}__FINAL");
                live.apply(record(Event::Queued {
                    job: Job {
                        id,
                        suite_id: "alloy".into(),
                        suite_name: "alloy".into(),
                        scenario_id: scenario_id.clone(),
                        label: scenario_id.clone(),
                        viewport: viewport.into(),
                        captures: vec![capture.clone()],
                    },
                }));
                live.apply(record(Event::Started { id }));
                live.apply(record(Event::Captured {
                    id,
                    error: (scenario == 3).then(|| "Missing selector".into()),
                    infrastructure: false,
                }));
                captures.push((capture, scenario_id, scenario != 3));
            }
        }
        live.apply(record(Event::Comparing {
            suite_id: "alloy".into(),
        }));
        for (capture, scenario_id, passed) in captures {
            live.apply(record(Event::Compared {
                suite_id: "alloy".into(),
                capture,
                label: scenario_id.clone(),
                scenario_id: Some(scenario_id),
                passed,
                error: (!passed).then(|| "Missing selector".into()),
            }));
        }
        live.apply(record(Event::SuiteFinished { id: "alloy".into() }));
        live.apply(record(Event::Finished { error: None }));
        let counts = live.counts(None);
        assert_eq!(counts.total, 12);
        assert_eq!(counts.passed, 9);
        assert_eq!(counts.failed, 3);
        assert_eq!(counts.running, 0);
        assert!(
            live.scenarios
                .values()
                .all(|scenario| !scenario.infrastructure)
        );
    }

    #[test]
    fn visual_pass_waits_for_every_screenshot_and_failure_never_flips_to_pass() {
        let mut live = LiveProgress::default();
        let mut details = job(1, "shop");
        details.captures = vec!["step".into(), "final".into()];
        live.apply(record(Event::Queued { job: details }));
        live.apply(record(Event::Captured {
            id: 1,
            error: None,
            infrastructure: false,
        }));
        live.apply(record(Event::Comparing {
            suite_id: "shop".into(),
        }));
        for (capture, passed) in [("step", true), ("final", false), ("step", true)] {
            live.apply(record(Event::Compared {
                suite_id: "shop".into(),
                capture: capture.into(),
                label: "Home".into(),
                scenario_id: None,
                passed,
                error: (!passed).then(|| "Visual mismatch".into()),
            }));
            assert_ne!(live.scenarios[&1].status, Status::Passed);
        }
        assert_eq!(live.scenarios[&1].status, Status::Failed);
    }

    #[test]
    fn cancellation_and_infrastructure_failure_resolve_pending_jobs() {
        for cancelled in [true, false] {
            let mut live = LiveProgress::default();
            live.apply(record(Event::Queued {
                job: job(1, "shop"),
            }));
            live.apply(record(Event::Queued {
                job: job(2, "shop"),
            }));
            live.apply(record(Event::Started { id: 1 }));
            live.end(
                cancelled,
                (!cancelled).then(|| "Browser unavailable".into()),
            );
            assert_eq!(live.counts(None).complete(), 2);
            assert_eq!(live.counts(None).running, 0);
            assert_eq!(
                live.scenarios[&1].status,
                if cancelled {
                    Status::Cancelled
                } else {
                    Status::Failed
                }
            );
        }
    }

    #[test]
    fn progress_reader_batches_preserve_order_and_partial_utf8() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let mut source = Vec::new();
        for index in 0..EVENT_BATCH_RECORDS + 3 {
            let record = Record {
                version: VERSION,
                elapsed_ms: index as u64,
                event: Event::Phase {
                    name: "\u{754c}".repeat(3),
                },
            };
            serde_json::to_writer(&mut source, &record).unwrap();
            source.push(b'\n');
        }
        file.write_all(&source).unwrap();
        file.flush().unwrap();
        let split = source.iter().position(|byte| *byte == 0xe7).unwrap() + 1;
        let mut reader = EventReader::open(file.path()).unwrap();
        let mut received = Vec::new();
        reader
            .drain_bounded(split, usize::MAX, |record| received.push(record.elapsed_ms))
            .unwrap();
        assert!(received.is_empty());
        assert!(reader.complete().is_err());
        reader
            .drain_batch(|record| received.push(record.elapsed_ms))
            .unwrap();
        assert_eq!(received.len(), EVENT_BATCH_RECORDS);
        reader
            .drain(|record| received.push(record.elapsed_ms))
            .unwrap();
        reader.complete().unwrap();
        assert_eq!(
            received,
            (0..(EVENT_BATCH_RECORDS + 3) as u64).collect::<Vec<_>>()
        );
        reader
            .drain_batch(|_| panic!("Completed records were replayed"))
            .unwrap();
    }

    #[test]
    fn progress_reader_retains_partial_records_and_rejects_invalid_protocol() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut reader = EventReader::open(file.path()).unwrap();
        let mut output = OpenOptions::new().append(true).open(file.path()).unwrap();
        let bytes = serde_json::to_vec(&record(Event::Started { id: 1 })).unwrap();
        output.write_all(&bytes[..10]).unwrap();
        let mut events = Vec::new();
        reader.drain(|event| events.push(event)).unwrap();
        assert!(events.is_empty());
        assert!(reader.complete().is_err());
        output.write_all(&bytes[10..]).unwrap();
        output.write_all(b"\n").unwrap();
        reader.drain(|event| events.push(event)).unwrap();
        reader.complete().unwrap();
        assert_eq!(events.len(), 1);
        output
            .write_all(
                b"{\"version\":99,\"elapsedMs\":0,\"event\":{\"event\":\"started\",\"id\":1}}\n",
            )
            .unwrap();
        assert!(reader.drain(|_| {}).is_err());
    }

    #[test]
    fn comparison_only_runs_can_populate_live_rows() {
        let mut live = LiveProgress::default();
        live.apply(record(Event::Comparing {
            suite_id: "existing".into(),
        }));
        live.apply(record(Event::Compared {
            suite_id: "existing".into(),
            capture: "home".into(),
            label: "Home".into(),
            scenario_id: None,
            passed: true,
            error: None,
        }));
        assert_eq!(live.counts(Some("existing")).passed, 1);
        live.apply(record(Event::Finished { error: None }));
        assert_eq!(live.scenarios[&1].status, Status::Passed);
    }

    #[test]
    fn test_progress_advances_monotonically_as_each_job_captures_and_compares() {
        let mut live = LiveProgress::default();
        let mut first = job(1, "shop");
        first.captures = vec!["step".into(), "final".into()];
        let mut second = job(2, "shop");
        second.captures = vec!["home".into()];
        live.apply(record(Event::Queued { job: first }));
        live.apply(record(Event::Queued { job: second }));
        assert_eq!(live.counts(None).fraction(), 0.0);

        live.apply(record(Event::Started { id: 1 }));
        live.apply(record(Event::JobComparing { id: 1, error: None }));
        assert_eq!(live.scenarios[&1].status, Status::Comparing);
        assert_eq!(live.counts(None).captured, 0);
        assert_eq!(live.counts(None).running, 1);
        assert_eq!(live.counts(None).complete(), 0);
        assert_eq!(live.counts(None).fraction(), 0.0);

        // A multi-screenshot job stays Comparing until every capture in the job is compared.
        live.apply(record(Event::Compared {
            suite_id: "shop".into(),
            capture: "step".into(),
            label: "Scenario 1".into(),
            scenario_id: Some("scenario-1".into()),
            passed: false,
            error: Some("Visual mismatch".into()),
        }));
        assert_eq!(live.scenarios[&1].status, Status::Comparing);
        assert_eq!(live.counts(None).complete(), 0);
        assert_eq!(live.counts(None).fraction(), 0.0);

        live.apply(record(Event::Compared {
            suite_id: "shop".into(),
            capture: "final".into(),
            label: "Scenario 1".into(),
            scenario_id: Some("scenario-1".into()),
            passed: true,
            error: None,
        }));
        assert_eq!(live.scenarios[&1].status, Status::Failed);
        assert_eq!(live.counts(None).complete(), 1);
        assert_eq!(live.counts(None).fraction(), 0.5);

        live.apply(record(Event::Started { id: 2 }));
        live.apply(record(Event::JobComparing { id: 2, error: None }));
        assert_eq!(live.counts(None).fraction(), 0.5);

        live.apply(record(Event::Compared {
            suite_id: "shop".into(),
            capture: "home".into(),
            label: "Scenario 2".into(),
            scenario_id: Some("scenario-2".into()),
            passed: true,
            error: None,
        }));
        assert_eq!(live.scenarios[&2].status, Status::Passed);
        assert_eq!(live.counts(None).complete(), 2);
        assert_eq!(live.counts(None).fraction(), 1.0);
    }

    #[test]
    fn reference_progress_self_approves_jobs_without_comparing_phase() {
        let mut live = LiveProgress::default();
        live.apply(record(Event::Queued {
            job: job(1, "shop"),
        }));
        live.apply(record(Event::Queued {
            job: job(2, "shop"),
        }));
        live.apply(record(Event::Started { id: 1 }));
        live.apply(record(Event::JobApproved { id: 1, error: None }));
        assert_eq!(live.scenarios[&1].status, Status::Passed);
        assert_eq!(live.counts(None).captured, 0);
        assert_eq!(live.counts(None).passed, 1);
        assert_eq!(live.counts(None).fraction(), 0.5);

        live.apply(record(Event::Started { id: 2 }));
        live.apply(record(Event::JobApproved {
            id: 2,
            error: Some("Missing selector".into()),
        }));
        assert_eq!(live.scenarios[&2].status, Status::Failed);
        assert_eq!(live.counts(None).captured, 0);
        assert_eq!(live.counts(None).passed, 1);
        assert_eq!(live.counts(None).failed, 1);
        assert_eq!(live.counts(None).fraction(), 1.0);
        let comparisons = live.comparison_counts(None);
        assert_eq!(comparisons.total, 2);
        assert_eq!(comparisons.passed, 1);
        assert_eq!(comparisons.failed, 1);
    }
}
