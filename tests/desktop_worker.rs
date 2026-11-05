use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    sync::{PoisonError, RwLock, RwLockReadGuard},
};

use regressify::desktop::{
    runner::{RunCommand, RunOptions, RunRequest},
    worker::{WORKER_FLAG, WorkerRequest},
};

// The installer test holds the session-wide Windows setup mutex, which makes every
// regressify-desktop process refuse to start. Desktop launches share this lock so
// they run in parallel with each other but never while that test holds the mutex.
static SETUP_MUTEX: RwLock<()> = RwLock::new(());

fn desktop_launches() -> RwLockReadGuard<'static, ()> {
    SETUP_MUTEX.read().unwrap_or_else(PoisonError::into_inner)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LogBenchmarkSample {
    archive_enabled: bool,
    repetition: usize,
    seconds: f64,
    journal_bytes: u64,
    output_bytes: u64,
    journal_files: usize,
    records: u64,
}

#[test]
#[ignore = "benchmark: cached managed Chromium, 4 suites x 2 viewports, 10 runs"]
fn logging_overhead_benchmark() {
    let site = LoggingSite::new();
    let root = tempfile::tempdir().unwrap();
    logging_workspace(root.path(), &site, 4, false);
    let warmup = logging_cli(root.path(), "warmup", "test", true, true);
    assert!(
        warmup.status.success(),
        "{}",
        String::from_utf8_lossy(&warmup.stderr)
    );
    let mut samples = Vec::new();
    for repetition in 0..5 {
        for enabled in if repetition % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let workspace = tempfile::tempdir().unwrap();
            logging_workspace(workspace.path(), &site, 4, false);
            let started = std::time::Instant::now();
            let result = logging_cli(workspace.path(), "benchmark", "test", !enabled, true);
            let seconds = started.elapsed().as_secs_f64();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let output = workspace.path().join(".regressify");
            let mut sample = LogBenchmarkSample {
                archive_enabled: enabled,
                repetition: repetition + 1,
                seconds,
                journal_bytes: 0,
                output_bytes: 0,
                journal_files: 0,
                records: 0,
            };
            let mut images = 0;
            for entry in walkdir::WalkDir::new(&output)
                .into_iter()
                .map(Result::unwrap)
                .filter(|entry| entry.file_type().is_file())
            {
                let relative = entry.path().strip_prefix(&output).unwrap();
                let bytes = entry.metadata().unwrap().len();
                sample.output_bytes += bytes;
                if relative.starts_with("_diagnostics") {
                    sample.journal_bytes += bytes;
                    sample.journal_files += 1;
                    if entry.file_name() == "index.json" {
                        let index: regressify::diagnostics::journal::ArchiveIndex =
                            serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
                        assert_eq!(
                            index.state,
                            regressify::diagnostics::journal::ArchiveState::Complete
                        );
                        sample.records += index.records;
                    }
                }
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "png")
                {
                    assert!(
                        regressify::test::capture::read_error_marker(entry.path()).is_none(),
                        "Benchmark capture failed"
                    );
                    images += 1;
                }
            }
            assert_eq!(images, 8);
            assert_eq!(sample.journal_bytes == 0, !enabled);
            println!(
                "LOG_BENCHMARK_SAMPLE {}",
                serde_json::to_string(&sample).unwrap()
            );
            samples.push(sample);
        }
    }
    if let Some(path) = std::env::var_os("REGRESSIFY_LOG_BENCHMARK_OUTPUT") {
        regressify::helpers::write_file_atomically(
            Path::new(&path),
            &serde_json::to_vec_pretty(&samples).unwrap(),
        )
        .unwrap();
    }
}

struct LoggingSite {
    server: std::sync::Arc<tiny_http::Server>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    origin: String,
}

impl LoggingSite {
    fn new() -> Self {
        let server = std::sync::Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let origin = format!("http://{}", server.server_addr());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let requests = server.clone();
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(std::sync::atomic::Ordering::Acquire) {
                if let Ok(Some(request)) =
                    requests.recv_timeout(std::time::Duration::from_millis(100))
                {
                    let _ = request.respond(tiny_http::Response::from_string("<!doctype html><title>Logging fixture</title><body><main><h1>Capture fixture</h1><input id='entry'><button id='ready'>Ready</button></main></body>")
                        .with_header(tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap()));
                }
            }
        });
        Self {
            server,
            stop,
            thread: Some(thread),
            origin,
        }
    }
}

impl Drop for LoggingSite {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.server.unblock();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn logging_workspace(root: &Path, site: &LoggingSite, suites: usize, fail_first: bool) {
    std::fs::create_dir_all(root.join("visual_tests")).unwrap();
    std::fs::create_dir_all(root.join("common")).unwrap();
    std::fs::write(root.join("regressify.yaml"), "delay: 0\npostInteractionWait: 0\nsettleTimeout: 0\nfreezeAnimations: false\nnoScrollTop: true\nelementTimeout: 5000\nactionabilityTimeout: 5000\nscenarioTimeout: 30000\nlogLevel: error\n").unwrap();
    std::fs::write(root.join("common/_viewports.yaml"), "- label: desktop\n  width: 800\n  height: 600\n- label: mobile\n  width: 375\n  height: 667\n").unwrap();
    for index in 0..suites {
        let target = if fail_first && index == 0 {
            "#absent-synthetic-selector"
        } else {
            "#ready"
        };
        let suite = serde_json::json!({"scenarios":[{"id":"page", "label":format!("Page {index}"),
            "url":format!("{}/capture?token=synthetic-navigation-secret", site.origin),
            "actions":[{"input":"#entry", "value":"synthetic-input-secret"},{"click":target}], "selectors":["viewport"]}]});
        std::fs::write(
            root.join(format!("visual_tests/suite-{index}.tests.json")),
            serde_json::to_vec(&suite).unwrap(),
        )
        .unwrap();
    }
}

fn logging_cli(
    root: &Path,
    run: &str,
    command: &str,
    disabled: bool,
    skip_compare: bool,
) -> Output {
    logging_cli_command(root, run, command, disabled, skip_compare)
        .output()
        .unwrap()
}

fn logging_cli_command(
    root: &Path,
    run: &str,
    command: &str,
    disabled: bool,
    skip_compare: bool,
) -> Command {
    let mut process = Command::new(env!("CARGO_BIN_EXE_regressify"));
    process
        .args(["--root"])
        .arg(root)
        .args([
            "--run-id",
            run,
            "--no-print",
            "--async-capture-limit",
            "5",
            "--async-capture-ramp-ms",
            "0",
            command,
            "--fetch-browser",
            "false",
        ])
        .env(
            "REGRESSIFY_DISABLE_DIAGNOSTIC_ARCHIVE",
            if disabled { "true" } else { "false" },
        );
    if skip_compare {
        process.arg("--skip-compare");
    }
    process
}

#[test]
#[ignore = "requires cached managed Chromium; uses loopback fixtures only"]
fn cli_rebuilds_the_report_only_after_whole_workspace_full_runs() {
    let site = LoggingSite::new();
    let root = tempfile::tempdir().unwrap();
    logging_workspace(root.path(), &site, 2, false);
    // A two-scenario suite separates "every suite selected" from "every scenario run".
    let scenarios = ["page", "extra"].map(|id| {
        serde_json::json!({
            "id": id,
            "label": format!("Suite 2 {id}"),
            "url": format!("{}/capture", site.origin),
            "selectors": ["viewport"],
        })
    });
    std::fs::write(
        root.path().join("visual_tests/suite-2.tests.json"),
        serde_json::to_vec(&serde_json::json!({ "scenarios": scenarios })).unwrap(),
    )
    .unwrap();
    let output = root.path().join(".regressify");
    let published = |label: &str| {
        [
            "index.html",
            "_snapshot/data/suites/suite-0.json",
            "_snapshot/data/suites/suite-1.json",
        ]
        .map(|path| {
            std::fs::read(output.join(path))
                .unwrap_or_else(|error| panic!("{label}: missing published {path}: {error}"))
        })
    };
    let run = |command: &mut Command, label: &str| {
        let result = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
        assert!(result.status.success(), "{label}: {stderr}");
        stderr
    };
    let hint = "Report not regenerated for a partial run";

    let stderr = run(
        &mut logging_cli_command(root.path(), "baseline", "ref", false, false),
        "whole-workspace ref",
    );
    assert!(!stderr.contains(hint), "{stderr}");
    let baseline = published("whole-workspace ref");

    // Partial runs must not reload, rewrite, or prune unrelated suites' report data.
    let every_suite_partial_scenarios = [
        "--scenario",
        "suite-0:page",
        "--scenario",
        "suite-1:page",
        "--scenario",
        "suite-2:page",
    ];
    let extra_references = ["desktop", "mobile"].map(|viewport| {
        output.join(format!(
            "suite-2/bitmaps_reference/extra_{viewport}__VIEWPORT.png"
        ))
    });
    for reference in &extra_references {
        // A single-scenario reference must still promote its baseline.
        std::fs::remove_file(reference).unwrap();
    }
    for (label, run_id, command, filter) in [
        (
            "scenario ref",
            "scenario-ref",
            "ref",
            &["--scenario", "suite-2:extra"][..],
        ),
        (
            "suite ref",
            "suite-ref",
            "ref",
            &["--test-suite", "suite-0"][..],
        ),
        (
            "suite test",
            "suite-test",
            "test",
            &["--test-suite", "suite-0"][..],
        ),
        (
            "scenario subset test",
            "subset-test",
            "test",
            &every_suite_partial_scenarios[..],
        ),
    ] {
        let mut command = logging_cli_command(root.path(), run_id, command, false, false);
        command.args(filter);
        let stderr = run(&mut command, label);
        assert!(stderr.contains(hint), "{label}: {stderr}");
        assert!(stderr.contains("regressify snapshot"), "{label}: {stderr}");
        assert_eq!(
            published(label),
            baseline,
            "{label} changed the published report"
        );
    }
    for report in [
        "suite-2/bitmaps_test/scenario-ref/report.json",
        "suite-0/bitmaps_test/suite-ref/report.json",
        "suite-0/bitmaps_test/suite-test/report.json",
        "suite-2/bitmaps_test/subset-test/report.json",
    ] {
        assert!(
            output.join(report).is_file(),
            "Partial runs must still capture and report: {report}"
        );
    }
    for reference in &extra_references {
        assert!(reference.is_file(), "Missing {}", reference.display());
    }

    // Selecting every scenario explicitly is a whole-workspace run, as is no filter.
    let mut every_scenario = logging_cli_command(root.path(), "explicit", "test", false, false);
    every_scenario
        .args(every_suite_partial_scenarios)
        .args(["--scenario", "suite-2:extra"]);
    for (label, run_id, command) in [
        (
            "explicit whole-workspace test",
            "explicit",
            &mut every_scenario,
        ),
        (
            "whole-workspace test",
            "whole",
            &mut logging_cli_command(root.path(), "whole", "test", false, false),
        ),
    ] {
        let stderr = run(command, label);
        assert!(!stderr.contains(hint), "{label}: {stderr}");
        for suite in ["suite-0", "suite-1", "suite-2"] {
            let summary: serde_json::Value = serde_json::from_slice(
                &std::fs::read(output.join(format!("_snapshot/data/suites/{suite}.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(summary["runId"], run_id, "{label}: {suite}");
        }
        // Later tests compare against the baseline the single-scenario ref promoted.
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(output.join(format!("suite-2/bitmaps_test/{run_id}/report.json")))
                .unwrap(),
        )
        .unwrap();
        let extra = report["tests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|test| {
                test["pair"]["fileName"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("extra_"))
            })
            .collect::<Vec<_>>();
        assert_eq!(extra.len(), 2, "{label}: {report}");
        assert!(
            extra.iter().all(|test| test["status"] == "pass"),
            "{label}: {report}"
        );
    }
}

#[test]
#[ignore = "requires cached managed Chromium; uses loopback fixtures only"]
fn cli_partial_runs_succeed_beside_an_unloadable_unrelated_suite() {
    let site = LoggingSite::new();
    let root = tempfile::tempdir().unwrap();
    logging_workspace(root.path(), &site, 2, false);
    let baseline = logging_cli(root.path(), "baseline", "ref", false, false);
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    // Before the fix, every run reloaded this suite and exited non-zero after
    // its own captures, comparisons, and baseline promotion had succeeded.
    std::fs::write(
        root.path()
            .join(".regressify/suite-1/bitmaps_test/baseline/report.json"),
        b"{",
    )
    .unwrap();
    for (run_id, command) in [("partial-ref", "ref"), ("partial-test", "test")] {
        let result = logging_cli_command(root.path(), run_id, command, false, false)
            .args(["--test-suite", "suite-0"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(result.status.success(), "{run_id}: {stderr}");
        assert!(
            !stderr.contains("suite-1"),
            "{run_id} touched the unrelated suite: {stderr}"
        );
        assert!(
            root.path()
                .join(format!(
                    ".regressify/suite-0/bitmaps_test/{run_id}/report.json"
                ))
                .is_file(),
            "{run_id}: {stderr}"
        );
    }
}

#[test]
#[ignore = "requires cached managed Chromium; uses loopback fixtures only"]
fn cli_scoped_runs_on_fresh_output_keep_states_ignored() {
    let site = LoggingSite::new();
    let root = tempfile::tempdir().unwrap();
    logging_workspace(root.path(), &site, 2, false);
    let output = root.path().join(".regressify");
    let gitignore = output.join(".gitignore");
    assert!(!output.exists());
    let scoped = |run_id: &str, command: &str| {
        let result = logging_cli_command(root.path(), run_id, command, false, false)
            .args(["--test-suite", "suite-0"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{run_id}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    // Partial runs skip the snapshot, which used to be the only writer of these rules.
    scoped("fresh-ref", "ref");
    let rules = std::fs::read_to_string(&gitignore).unwrap();
    assert!(
        rules.lines().any(|line| line.trim() == "/states/"),
        "{rules}"
    );
    assert!(!output.join("index.html").exists());

    std::fs::write(&gitignore, b"custom\n").unwrap();
    scoped("edited-test", "test");
    assert_eq!(std::fs::read(&gitignore).unwrap(), b"custom\n");
}

#[test]
#[ignore = "requires cached managed Chromium; uses loopback fixtures only"]
fn standalone_cli_persists_passed_and_failed_logs_without_desktop_initialization() {
    use regressify::diagnostics::{
        journal::{ArchiveState, JournalReader},
        record::{Field, FieldValue, Outcome, SafeIdentity},
    };
    let site = LoggingSite::new();
    let root = tempfile::tempdir().unwrap();
    logging_workspace(root.path(), &site, 2, false);
    let baseline = logging_cli(root.path(), "baseline", "ref", false, false);
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    for suite in 0..2 {
        for viewport in ["desktop", "mobile"] {
            assert!(
                root.path()
                    .join(format!(
                        ".regressify/suite-{suite}/bitmaps_reference/page_{viewport}__VIEWPORT.png"
                    ))
                    .is_file(),
                "Baseline image missing: {}",
                String::from_utf8_lossy(&baseline.stderr)
            );
        }
    }
    logging_workspace(root.path(), &site, 2, true);
    let captured = logging_cli(root.path(), "mixed", "test", false, false);
    assert!(
        captured.status.success(),
        "{}",
        String::from_utf8_lossy(&captured.stderr)
    );
    for stream in [&captured.stdout, &captured.stderr] {
        let text = String::from_utf8_lossy(stream);
        for secret in ["synthetic-input-secret", "synthetic-navigation-secret"] {
            assert!(
                !text.contains(secret),
                "Sensitive payload reached command output"
            );
        }
        assert!(
            !text.contains("#absent-synthetic-selector"),
            "Quiet mode printed source metadata"
        );
    }
    let output = root.path().join(".regressify");
    for suite in 0..2 {
        for viewport in ["desktop", "mobile"] {
            let image = output.join(format!(
                "suite-{suite}/bitmaps_test/mixed/page_{viewport}__VIEWPORT.png"
            ));
            let link = regressify::test::capture::read_capture_log(&output, &image).unwrap();
            assert_eq!(link.state, ArchiveState::Complete);
            let reader = JournalReader::open(&output, &link.archive.unwrap().operation_id).unwrap();
            assert_eq!(
                reader.verified_index().unwrap().outcome,
                if suite == 0 {
                    Outcome::Failure
                } else {
                    Outcome::Success
                }
            );
            let target = if suite == 0 {
                "#absent-synthetic-selector"
            } else {
                "#ready"
            };
            let mut source_seen = false;
            reader
                .visit(|record| {
                    let text = serde_json::to_string(&record).unwrap();
                    for secret in ["synthetic-input-secret", "synthetic-navigation-secret"] {
                        assert!(!text.contains(secret));
                    }
                    if let Some(execution) = &record.execution {
                        assert_eq!(execution.suite_id.as_str(), format!("suite-{suite}"));
                        assert_eq!(execution.scenario_id.as_str(), "page");
                        assert_eq!(execution.viewport.name.as_str(), viewport);
                    }
                    source_seen |= record.fields.get(&Field::Target)
                        == Some(&FieldValue::Identity(SafeIdentity::source(target)));
                })
                .unwrap();
            assert!(
                source_seen,
                "Capture archive lost its readable source selector"
            );
            let report: serde_json::Value = serde_json::from_slice(
                &std::fs::read(output.join(format!("_snapshot/data/suites/suite-{suite}.json")))
                    .unwrap(),
            )
            .unwrap();
            for capture in report["tests"].as_array().unwrap() {
                assert_eq!(capture["logs"]["test"]["state"], "complete");
                assert_eq!(capture["logs"]["reference"]["state"], "complete");
                for role in ["test", "reference"] {
                    let mut published_source_seen = false;
                    for chunk in capture["logs"][role]["chunks"].as_array().unwrap() {
                        let url = reqwest::Url::parse("https://fixture.invalid/")
                            .unwrap()
                            .join(chunk.as_str().unwrap())
                            .unwrap();
                        let bytes =
                            std::fs::read(output.join(url.path().trim_start_matches('/'))).unwrap();
                        let text = String::from_utf8(bytes).unwrap();
                        for secret in ["synthetic-input-secret", "synthetic-navigation-secret"] {
                            assert!(
                                !text.contains(secret),
                                "Sensitive input reached a published log"
                            );
                        }
                        let chunk: serde_json::Value = serde_json::from_str(&text).unwrap();
                        published_source_seen |=
                            chunk["records"].as_array().unwrap().iter().any(|record| {
                                record["fields"]["target"]["value"] == "#entry"
                                    && record["execution"]["suiteId"] == format!("suite-{suite}")
                                    && record["execution"]["scenarioId"] == "page"
                            });
                    }
                    assert!(published_source_seen, "Published log lost source metadata");
                }
            }
        }
    }
    let journal_count = std::fs::read_dir(output.join("_diagnostics"))
        .unwrap()
        .count();
    let disabled = logging_cli(root.path(), "disabled", "test", true, true);
    assert!(
        disabled.status.success(),
        "{}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    assert_eq!(
        std::fs::read_dir(output.join("_diagnostics"))
            .unwrap()
            .count(),
        journal_count
    );
    let disabled_log = regressify::test::capture::read_capture_log(
        &output,
        &output.join("suite-1/bitmaps_test/disabled/page_desktop__VIEWPORT.png"),
    )
    .unwrap();
    assert_eq!(disabled_log.state, ArchiveState::Missing);
    if let Some(destination) = std::env::var_os("REGRESSIFY_LOG_BROWSER_FIXTURE") {
        let destination = std::path::PathBuf::from(destination);
        assert!(
            !destination.exists(),
            "Logging browser fixture destination must be new"
        );
        std::fs::create_dir_all(&destination).unwrap();
        for entry in walkdir::WalkDir::new(&output)
            .into_iter()
            .map(Result::unwrap)
        {
            let path = destination.join(entry.path().strip_prefix(&output).unwrap());
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(path).unwrap();
            } else {
                std::fs::copy(entry.path(), path).unwrap();
            }
        }
    }
}

fn invoke(executable: &Path, request: &RunRequest) -> Output {
    invoke_with_events(executable, request, None)
}

#[cfg(windows)]
#[test]
fn snapshot_worker_reports_locked_output_path() {
    let _setup = desktop_launches();
    use std::os::windows::fs::OpenOptionsExt;

    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join(".regressify");
    std::fs::create_dir(&output).unwrap();
    let path = output.canonicalize().unwrap().join("index.html");
    std::fs::write(&path, b"private old report").unwrap();
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let request = RunRequest {
        root: root.path().into(),
        command: RunCommand::Snapshot,
        options: RunOptions::default(),
    };
    let result = invoke(executable, &request);
    drop(locked);

    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success(), "{stderr}");
    assert!(
        stderr.contains("Failed to read existing snapshot artifact"),
        "{stderr}"
    );
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(stderr.contains("os error 32"), "{stderr}");
    assert!(!stderr.contains("private old report"), "{stderr}");
    assert_eq!(std::fs::read(&path).unwrap(), b"private old report");

    let result = invoke(executable, &request);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[cfg(windows)]
#[test]
fn snapshot_worker_warns_for_blocked_empty_directory() {
    let _setup = desktop_launches();
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_SHARE_READ_WRITE: u32 = 3;
    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join(".regressify");
    std::fs::create_dir(&output).unwrap();
    let blocked = output
        .canonicalize()
        .unwrap()
        .join("_snapshot/images/suite/diff");
    std::fs::create_dir_all(&blocked).unwrap();
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(&blocked)
        .unwrap();
    let request = RunRequest {
        root: root.path().into(),
        command: RunCommand::Snapshot,
        options: RunOptions::default(),
    };
    let result = invoke(executable, &request);
    drop(locked);

    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("Warning: Could not remove empty snapshot directory"),
        "{stderr}"
    );
    assert!(stderr.contains(&blocked.display().to_string()), "{stderr}");
    assert!(stderr.contains("Snapshot generated at:"), "{stderr}");
    assert!(output.join("index.html").is_file());
    assert!(blocked.is_dir());
    assert!(std::fs::read_dir(&blocked).unwrap().next().is_none());

    let result = invoke(executable, &request);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!blocked.exists());
}

#[cfg(windows)]
#[test]
#[ignore = "requires cached managed Chromium and no running installer"]
fn installer_browser_mode_reuses_cache_without_network_during_setup() {
    let _exclusive = SETUP_MUTEX.write().unwrap_or_else(PoisonError::into_inner);
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
        System::Threading::CreateMutexW,
    };

    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let chromiumoxide::fetcher::BrowserVersion::Revision(revision) =
        chromiumoxide::fetcher::BrowserVersion::current(
            chromiumoxide::fetcher::BrowserKind::Chromium,
        )
    else {
        panic!("Expected a pinned Chromium revision");
    };
    let platform = if cfg!(target_arch = "x86") {
        "win32"
    } else {
        "win64"
    };
    let browser = directories::BaseDirs::new()
        .unwrap()
        .cache_dir()
        .join(format!(
            "regressify/chromium/{platform}-{revision}/chrome-win/chrome.exe"
        ));
    let before =
        std::fs::metadata(&browser).expect("Run regressify install before this opt-in test");
    let workspace = tempfile::tempdir().unwrap();
    let name: Vec<u16> = "Local\\Regressify.Desktop.Setup.ED6F0908-7E11-4608-A532-E43482AA117B"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    assert!(!mutex.is_null());
    let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let _setup_mutex = unsafe { OwnedHandle::from_raw_handle(mutex) };
    assert!(
        !already_running,
        "Do not run this test during an actual installation"
    );

    let mut child = Command::new(executable)
        .arg("--install-chromium")
        .current_dir(workspace.path())
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Installer browser preparation did not finish from cache");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{log}");
    assert!(
        log.contains("Reusing cached Chromium; no download needed"),
        "{log}"
    );
    assert!(log.contains("Chromium ready"), "{log}");
    assert!(!log.contains("Chromium download URL:"), "{log}");
    let after = std::fs::metadata(&browser).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);

    let blocked = invoke(
        executable,
        &RunRequest {
            root: workspace.path().to_path_buf(),
            command: RunCommand::Init,
            options: RunOptions::default(),
        },
    );
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("Regressify setup is running"));
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
}

#[test]
#[ignore = "requires managed Chromium; run regressify install"]
fn capture_workspaces_share_managed_chromium() {
    let _setup = desktop_launches();
    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let shared_cache = directories::BaseDirs::new()
        .unwrap()
        .cache_dir()
        .join("regressify/chromium");
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let mut selected_browser = None;
    let page = "data:text/html,<body style='background:rgb(24,128,100)'><div id='not-select' style='height:64px'>Managed Chromium</div></body>";
    let suite = serde_json::json!({
        "id": "managed",
        "postInteractionWait": 0,
        "scenarios": [
            {"id": "passing", "label": "passing", "url": page, "delay": 0, "noScrollTop": true},
            {"id": "failing", "label": "failing", "url": page, "delay": 0, "noScrollTop": true,
             "actions": [{"select": "#not-select", "value": "missing"}]}
        ]
    });
    for (index, workspace) in [&first, &second].into_iter().enumerate() {
        let suites = workspace.path().join("visual_tests");
        std::fs::create_dir_all(&suites).unwrap();
        std::fs::write(
            suites.join("managed.tests.json"),
            serde_json::to_vec(&suite).unwrap(),
        )
        .unwrap();
        for (command, run_id) in [
            (RunCommand::Reference, "shared-reference"),
            (RunCommand::Test, "shared-test"),
        ] {
            let request = RunRequest {
                root: workspace.path().into(),
                command,
                options: RunOptions {
                    suites: vec!["managed".into()],
                    run_id: run_id.into(),
                    fetch_browser: (index == 1).then_some(false),
                    async_capture_limit: 1,
                    async_capture_ramp_ms: 0,
                    ..Default::default()
                },
            };
            let events = tempfile::NamedTempFile::new().unwrap();
            let output = invoke_with_events(executable, &request, Some(events.path()));
            let log = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.status.success(), "{log}");
            let mut phases = Vec::new();
            let mut reader = regressify::progress::EventReader::open(events.path()).unwrap();
            reader
                .drain(|record| {
                    if let regressify::progress::Event::Phase { name } = record.event {
                        phases.push(name);
                    }
                })
                .unwrap();
            reader.complete().unwrap();
            let ready = phases
                .iter()
                .position(|phase| phase == "Chromium ready")
                .unwrap();
            let capture = phases
                .iter()
                .position(|phase| phase == "Capturing scenarios")
                .unwrap();
            assert!(ready < capture, "{phases:?}");
            assert!(log.contains("Checking Chromium cache"), "{log}");
            assert!(log.contains("Shared Chromium cache:"), "{log}");
            let browser = log
                .lines()
                .find_map(|line| line.strip_prefix("Managed Chromium executable: "))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| panic!("Missing managed browser path: {log}"));
            assert!(browser.starts_with(&shared_cache), "{}", browser.display());
            assert!(browser.is_file());
            assert_eq!(selected_browser.get_or_insert(browser.clone()), &browser);
            assert!(!workspace.path().join("download").exists());
        }
        let artifacts = workspace.path().join(".regressify/managed");
        let references: Vec<_> = std::fs::read_dir(artifacts.join("bitmaps_reference"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "png"))
            .collect();
        assert!(!references.is_empty());
        assert!(references.iter().all(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("passing")
        }));
        assert!(
            walkdir::WalkDir::new(artifacts.join("bitmaps_test/shared-test"))
                .into_iter()
                .map(|entry| entry.unwrap())
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".error.json"))
        );
    }
}

#[test]
#[ignore = "requires cached managed Chromium; uses a data URL page only"]
fn desktop_test_and_reference_runs_skip_the_post_run_report() {
    let _setup = desktop_launches();
    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let workspace = tempfile::tempdir().unwrap();
    let suites = workspace.path().join("visual_tests");
    std::fs::create_dir_all(&suites).unwrap();
    let page =
        "data:text/html,<body style='background:rgb(24,128,100)'><main>Report scope</main></body>";
    for (name, ids) in [("scoped", &["page", "extra"][..]), ("other", &["page"][..])] {
        let scenarios = ids
            .iter()
            .map(|id| serde_json::json!({"id": id, "label": id, "url": page, "delay": 0, "noScrollTop": true}))
            .collect::<Vec<_>>();
        std::fs::write(
            suites.join(format!("{name}.tests.json")),
            serde_json::to_vec(&serde_json::json!({
                "postInteractionWait": 0,
                "scenarios": scenarios,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let output = workspace.path().join(".regressify");
    assert!(!output.exists());
    let extra_reference = output.join("scoped/bitmaps_reference/extra__FINAL.png");
    // The CLI would rebuild after the whole-workspace runs; the desktop never does.
    for (command, run_id, selected, scenarios) in [
        (
            RunCommand::Reference,
            "scoped-reference",
            vec!["scoped"],
            vec![],
        ),
        (
            RunCommand::Reference,
            "scenario-reference",
            vec!["scoped"],
            vec!["scoped:extra"],
        ),
        (RunCommand::Test, "scoped-test", vec!["scoped"], vec![]),
        (
            RunCommand::Reference,
            "all-reference",
            vec!["scoped", "other"],
            vec![],
        ),
        (
            RunCommand::Test,
            "all-test",
            vec!["scoped", "other"],
            vec![],
        ),
    ] {
        if run_id == "scenario-reference" {
            // A single-scenario reference must still promote its baseline.
            std::fs::remove_file(&extra_reference).unwrap();
        }
        let request = RunRequest {
            root: workspace.path().into(),
            command,
            options: RunOptions {
                suites: selected.iter().map(|name| name.to_string()).collect(),
                scenarios: scenarios
                    .iter()
                    .map(|filter| filter.parse().unwrap())
                    .collect(),
                run_id: run_id.into(),
                fetch_browser: Some(false),
                async_capture_limit: 1,
                async_capture_ramp_ms: 0,
                ..Default::default()
            },
        };
        let events = tempfile::NamedTempFile::new().unwrap();
        let result = invoke_with_events(executable, &request, Some(events.path()));
        let log = format!(
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.status.success(), "{run_id}: {log}");
        assert!(
            !log.contains("Report not regenerated"),
            "{run_id}: desktop runs must not print the CLI reminder: {log}"
        );
        let mut phases = Vec::new();
        let mut reader = regressify::progress::EventReader::open(events.path()).unwrap();
        reader
            .drain(|record| {
                if let regressify::progress::Event::Phase { name } = record.event {
                    phases.push(name);
                }
            })
            .unwrap();
        reader.complete().unwrap();
        assert!(
            phases.iter().any(|phase| phase == "Capturing scenarios"),
            "{run_id}: {phases:?}"
        );
        assert!(
            !phases
                .iter()
                .any(|phase| phase == "Generating report" || phase.contains("snapshot")),
            "{run_id}: {phases:?}"
        );
        for path in ["index.html", "_snapshot", ".staging"] {
            assert!(
                !output.join(path).exists(),
                "{run_id}: desktop {} run generated {path}",
                command.label()
            );
        }
        if run_id == "scoped-reference" {
            // The fresh output still gets the ignore rules the skipped snapshot used to write.
            let rules = std::fs::read_to_string(output.join(".gitignore")).unwrap();
            assert!(
                rules.lines().any(|line| line.trim() == "/states/"),
                "{run_id}: {rules}"
            );
        }
        for name in &selected {
            let suite = output.join(name);
            assert!(
                suite.join("bitmaps_reference/page__FINAL.png").is_file(),
                "{run_id}: missing promoted baseline for {name}"
            );
            assert!(
                suite
                    .join(format!("bitmaps_test/{run_id}/report.json"))
                    .is_file(),
                "{run_id}: missing run report for {name}"
            );
        }
        assert!(
            extra_reference.is_file(),
            "{run_id}: missing extra baseline"
        );
        if run_id == "scoped-test" {
            // The next test compares against the baseline the single-scenario run promoted.
            let report: serde_json::Value = serde_json::from_slice(
                &std::fs::read(output.join("scoped/bitmaps_test/scoped-test/report.json")).unwrap(),
            )
            .unwrap();
            let extra = report["tests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|test| {
                    test["pair"]["fileName"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("extra"))
                })
                .collect::<Vec<_>>();
            assert!(!extra.is_empty(), "{report}");
            assert!(
                extra.iter().all(|test| test["status"] == "pass"),
                "{report}"
            );
        }
    }

    // The absence checks above cannot see a deleted or rewritten report, so seed one and repeat
    // the whole-workspace runs after which the CLI would rebuild it.
    let seeded = b"<!-- report built before the desktop runs -->";
    std::fs::write(output.join("index.html"), seeded).unwrap();
    for (command, run_id) in [
        (RunCommand::Reference, "seeded-reference"),
        (RunCommand::Test, "seeded-test"),
    ] {
        let request = RunRequest {
            root: workspace.path().into(),
            command,
            options: RunOptions {
                suites: vec!["scoped".into(), "other".into()],
                run_id: run_id.into(),
                fetch_browser: Some(false),
                async_capture_limit: 1,
                async_capture_ramp_ms: 0,
                ..Default::default()
            },
        };
        let result = invoke_with_events(executable, &request, None);
        assert!(
            result.status.success(),
            "{run_id}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            std::fs::read(output.join("index.html")).unwrap(),
            seeded,
            "{run_id}: desktop {} run changed the existing report",
            command.label()
        );
        assert!(
            !output.join("_snapshot").exists(),
            "{run_id}: desktop {} run generated report data",
            command.label()
        );
    }
}

#[test]
fn deploy_missing_token_fails_before_snapshot_modifies_output() {
    let _setup = desktop_launches();
    use regressify::desktop::deployment::{DeployTarget, DeploymentJob};
    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join(".regressify");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join("index.html"), "untouched summary").unwrap();
    let request = RunRequest {
        root: root.path().into(),
        command: RunCommand::Deploy,
        options: RunOptions {
            suites: vec!["shop".into()],
            deployment: Some(DeploymentJob {
                target: DeployTarget {
                    id: "test".into(),
                    name: "Test".into(),
                    published_url: None,
                    credential_id: None,
                    named_token_id: None,
                    environment: String::new(),
                    provider: Default::default(),
                    site_id: String::new(),
                    cloudflare: None,
                },
                suite_ids: vec!["shop".into()],
                catalog_ids: vec!["shop".into()],
                partial_confirmed: false,
            }),
            ..Default::default()
        },
    };
    let result = invoke(executable, &request);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("No saved deployment token"));
    assert_eq!(
        std::fs::read_to_string(output.join("index.html")).unwrap(),
        "untouched summary"
    );
}

#[cfg(windows)]
#[test]
fn deploy_worker_reads_machine_local_credentials_without_exposing_tokens() {
    let _setup = desktop_launches();
    use regressify::desktop::deployment::{
        DeployTarget, DeployTargets, DeploymentJob, credentials::DeploymentCredential,
    };
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };

    struct Cleanup(DeploymentCredential);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.delete().expect("Remove synthetic test credential");
        }
    }

    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let root = tempfile::tempdir().unwrap();
    let credential_id = uuid::Uuid::new_v4().to_string();
    let credential = Cleanup(DeploymentCredential::new(root.path(), &credential_id).unwrap());
    let token = format!("synthetic-invalid-token-{}", uuid::Uuid::new_v4());
    credential.0.write(&token).unwrap();
    assert_eq!(credential.0.read().unwrap().as_str(), token);

    let settings_path = DeployTargets::path(root.path()).unwrap();
    let workspace_key = settings_path.file_stem().unwrap().to_str().unwrap();
    let target_name = format!("{workspace_key}:{credential_id}.Regressify Desktop Deployment")
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut stored: *mut CREDENTIALW = std::ptr::null_mut();
    assert_ne!(
        unsafe { CredReadW(target_name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut stored) },
        0
    );
    let persistence = unsafe { (*stored).Persist };
    unsafe { CredFree(stored.cast()) };
    assert_eq!(persistence, CRED_PERSIST_LOCAL_MACHINE);

    let output = root.path().join(".regressify");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join("index.html"), "untouched summary").unwrap();
    let events = root.path().join("events.jsonl");
    std::fs::write(&events, []).unwrap();
    let request = RunRequest {
        root: root.path().into(),
        command: RunCommand::Deploy,
        options: RunOptions {
            suites: vec!["shop".into()],
            deployment: Some(DeploymentJob {
                target: DeployTarget {
                    id: "test".into(),
                    name: "Test".into(),
                    published_url: None,
                    credential_id: Some(credential_id),
                    named_token_id: None,
                    environment: String::new(),
                    provider: Default::default(),
                    site_id: String::new(),
                    cloudflare: None,
                },
                suite_ids: vec!["shop".into()],
                catalog_ids: vec!["shop".into()],
                partial_confirmed: false,
            }),
            ..Default::default()
        },
    };
    assert!(!serde_json::to_string(&request).unwrap().contains(&token));
    let result = invoke_with_events(executable, &request, Some(&events));
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("Invalid Azure deployment token format"),
        "Unexpected worker error: {}",
        String::from_utf8_lossy(&result.stderr).replace(&token, "[REDACTED]")
    );
    assert!(!String::from_utf8_lossy(&result.stderr).contains(&token));
    assert!(!String::from_utf8_lossy(&result.stdout).contains(&token));
    assert!(!std::fs::read_to_string(events).unwrap().contains(&token));
    assert_eq!(
        std::fs::read_to_string(output.join("index.html")).unwrap(),
        "untouched summary"
    );
    let netlify_token = format!("synthetic invalid Netlify token {}", uuid::Uuid::new_v4());
    credential.0.write(&netlify_token).unwrap();
    let mut netlify_request = request;
    let target = &mut netlify_request.options.deployment.as_mut().unwrap().target;
    target.provider = regressify::desktop::deployment::DeploymentProvider::Netlify;
    target.site_id = uuid::Uuid::new_v4().to_string();
    let result = invoke(executable, &netlify_request);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("valid Netlify personal access token")
    );
    assert!(!String::from_utf8_lossy(&result.stderr).contains(&netlify_token));
    assert!(!String::from_utf8_lossy(&result.stdout).contains(&netlify_token));
    assert!(
        !serde_json::to_string(&netlify_request)
            .unwrap()
            .contains(&netlify_token)
    );
    assert_eq!(
        std::fs::read_to_string(output.join("index.html")).unwrap(),
        "untouched summary"
    );
    credential.0.delete().unwrap();
    assert!(credential.0.read_optional().unwrap().is_none());
}

fn invoke_with_events(
    executable: &Path,
    request: &RunRequest,
    events_path: Option<&Path>,
) -> Output {
    let scratch = (request.command == RunCommand::Deploy).then(|| tempfile::tempdir().unwrap());
    let mut child = Command::new(executable)
        .arg(WORKER_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            &serde_json::to_vec(&WorkerRequest {
                run: request.clone(),
                events_path: events_path.map(Path::to_path_buf),
                scratch_directory: scratch
                    .as_ref()
                    .map(|directory| directory.path().to_path_buf()),
            })
            .unwrap(),
        )
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn duplicate_workspace_launches_activate_the_owner_without_starting_another_gui() {
    let _setup = desktop_launches();
    use regressify::desktop::{
        instance::{DesktopInstance, OpenInstance},
        workspace::Preferences,
    };
    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let workspace = tempfile::tempdir().unwrap();
    let locks = Preferences::path().unwrap().with_file_name("instances");
    let owner = match DesktopInstance::open(&locks, workspace.path()).unwrap() {
        OpenInstance::Owned(instance) => instance,
        OpenInstance::Activated => panic!("Expected isolated workspace ownership"),
    };
    for path in [workspace.path().to_path_buf(), workspace.path().join(".")] {
        let mut child = Command::new(executable)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Duplicate launcher did not hand off the workspace");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(owner.take_activation());
    }
    let request = RunRequest {
        root: workspace.path().into(),
        command: RunCommand::Init,
        options: RunOptions::default(),
    };
    let output = invoke(executable, &request);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!owner.take_activation());
    assert!(workspace.path().join("visual_tests").is_dir());
}

#[test]
fn standalone_desktop_runs_shared_engine_without_a_cli_binary() {
    let _setup = desktop_launches();
    use regressify::{
        desktop::logs::{LogQuery, load_run_page},
        diagnostics::{journal::ArchiveState, record::*},
    };
    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let installation = tempfile::tempdir_in(executable.parent().unwrap()).unwrap();
    let standalone = installation.path().join(executable.file_name().unwrap());
    std::fs::hard_link(executable, &standalone).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut request = RunRequest {
        root: workspace.path().into(),
        command: RunCommand::Init,
        options: RunOptions {
            diagnostic_operation_id: Some(OperationId::new()),
            ..RunOptions::default()
        },
    };
    let output = invoke(&standalone, &request);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(workspace.path().join("common").is_dir());
    let page = load_run_page(&request, &[], &LogQuery::default());
    assert_eq!(page.state, ArchiveState::Complete);
    assert!(
        page.records
            .iter()
            .all(|record| record.operation_kind == OperationKind::Command)
    );
    assert!(page.records.iter().any(
        |record| record.fields.get(&Field::Command) == Some(&FieldValue::Kind(SafeValue::Init))
    ));
    let sentinel = workspace.path().join("visual_tests/basic.tests.yaml");
    std::fs::write(&sentinel, "# keep QA content\nscenarios: []\n").unwrap();
    request.options.diagnostic_operation_id = Some(OperationId::new());
    assert!(invoke(&standalone, &request).status.success());
    assert_eq!(
        std::fs::read_to_string(sentinel).unwrap(),
        "# keep QA content\nscenarios: []\n"
    );
    assert_eq!(std::fs::read_dir(installation.path()).unwrap().count(), 1);
    let mut invalid = request;
    invalid.options.async_capture_limit = 0;
    assert!(!invoke(&standalone, &invalid).status.success());
}

#[test]
fn bundled_worker_reports_live_outcomes_for_multiple_suites() {
    let _setup = desktop_launches();
    let workspace = tempfile::tempdir().unwrap();
    for (suite, changed) in [("shop", false), ("account", true)] {
        let root = workspace.path().join(".regressify").join(suite);
        let reference = root.join("bitmaps_reference");
        let captures = root.join("bitmaps_test/run-1");
        std::fs::create_dir_all(&reference).unwrap();
        std::fs::create_dir_all(&captures).unwrap();
        lodepng::encode32_file(
            reference.join("home__FINAL.png"),
            &[255_u8, 255, 255, 255],
            1,
            1,
        )
        .unwrap();
        let rgba = if changed {
            [0_u8, 0, 0, 255]
        } else {
            [255_u8; 4]
        };
        lodepng::encode32_file(captures.join("home__FINAL.png"), &rgba, 1, 1).unwrap();
    }
    let events = tempfile::NamedTempFile::new().unwrap();
    let request = RunRequest {
        root: workspace.path().into(),
        command: RunCommand::Compare,
        options: RunOptions {
            suites: vec!["shop".into(), "account".into()],
            ..Default::default()
        },
    };
    let output = invoke_with_events(
        Path::new(env!("CARGO_BIN_EXE_regressify-desktop")),
        &request,
        Some(events.path()),
    );
    assert!(
        output.status.success(),
        "Visual mismatch must not fail the worker: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reader = regressify::progress::EventReader::open(events.path()).unwrap();
    let mut live = regressify::progress::LiveProgress::default();
    reader.drain(|record| live.apply(record)).unwrap();
    reader.complete().unwrap();
    assert!(live.finished);
    assert!(live.error.is_none());
    assert_eq!(live.suites.len(), 2);
    assert_eq!(live.counts(Some("shop")).passed, 1);
    assert_eq!(live.counts(Some("account")).failed, 1);
}

#[test]
fn mixed_report_outcomes_match_html_saved_results_and_live_totals() {
    let _setup = desktop_launches();
    let workspace = tempfile::tempdir().unwrap();
    let output = workspace.path().join(".regressify");
    let suite = output.join("alloy");
    let reference = suite.join("bitmaps_reference");
    let captures = suite.join("bitmaps_test/mixed-run");
    std::fs::create_dir_all(&reference).unwrap();
    std::fs::create_dir_all(&captures).unwrap();
    let image = lodepng::encode32(&[255_u8; 4], 1, 1).unwrap();
    let mut metadata = serde_json::Map::new();
    for scenario in 0..4 {
        for viewport in ["phone", "tablet", "desktop"] {
            let scenario_id = format!("scenario-{scenario}");
            let key = format!("{scenario_id}_{viewport}__FINAL");
            let filename = format!("{key}.png");
            std::fs::write(reference.join(&filename), &image).unwrap();
            std::fs::write(captures.join(&filename), &image).unwrap();
            if scenario == 3 {
                std::fs::write(captures.join(format!("{filename}.error.json")),
                    serde_json::to_vec(&serde_json::json!({"error": true, "message": "Timed out waiting for missing selector"})).unwrap()).unwrap();
            }
            metadata.insert(
                key,
                serde_json::json!({
                    "scenarioId": scenario_id, "scenarioLabel": format!("Scenario {scenario}"),
                    "referenceUrl": "https://example.com", "url": "https://example.com"
                }),
            );
        }
    }
    std::fs::write(
        captures.join(".report-urls.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "captures": metadata
        }))
        .unwrap(),
    )
    .unwrap();
    let events = tempfile::NamedTempFile::new().unwrap();
    let request = RunRequest {
        root: workspace.path().into(),
        command: RunCommand::Compare,
        options: RunOptions {
            suites: vec!["alloy".into()],
            run_id: "mixed-run".into(),
            ..Default::default()
        },
    };
    let result = invoke_with_events(
        Path::new(env!("CARGO_BIN_EXE_regressify-desktop")),
        &request,
        Some(events.path()),
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(captures.join("report.json")).unwrap()).unwrap();
    let html_config = std::fs::read_to_string(suite.join("html_report/config.js")).unwrap();
    let html: serde_json::Value = serde_json::from_str(
        html_config
            .strip_prefix("report(")
            .unwrap()
            .strip_suffix(");")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(html, saved);
    let catalog = regressify::desktop::results::Catalog::load(&output).unwrap();
    assert!(catalog.warnings.is_empty());
    assert_eq!(catalog.reports[0].counts(), (9, 0, 3));
    let mut reader = regressify::progress::EventReader::open(events.path()).unwrap();
    let mut live = regressify::progress::LiveProgress::default();
    reader.drain(|record| live.apply(record)).unwrap();
    reader.complete().unwrap();
    assert!(live.finished && live.error.is_none());
    assert_eq!(live.counts(None).total, 4);
    let comparisons = live.comparison_counts(None);
    assert_eq!(
        (comparisons.total, comparisons.passed, comparisons.failed),
        (12, 9, 3)
    );
}

#[test]
fn filtered_snapshot_stages_only_selected_suites_and_their_public_dependencies() {
    let _setup = desktop_launches();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("regressify.yaml"),
        "snapshotTitle: Shop Visual Test Report\nsnapshotHeading: Shop Visual Tests Summary\nsnapshotSubheading: Compare [Integration](https://integration.example.com/en) and [Preproduction](https://preproduction.example.com/en)\n",
    )
    .unwrap();
    let output = workspace.path().join(".regressify");
    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let image = lodepng::encode32(&[20_u8, 40, 60, 255], 1, 1).unwrap();
    for suite in ["shop", "account"] {
        let baseline = output.join(suite).join("bitmaps_reference");
        let capture = output.join(suite).join("bitmaps_test/run-one");
        std::fs::create_dir_all(&baseline).unwrap();
        std::fs::create_dir_all(&capture).unwrap();
        std::fs::write(baseline.join("home__FINAL.png"), &image).unwrap();
        std::fs::write(capture.join("home__FINAL.png"), &image).unwrap();
    }
    let mut request = RunRequest {
        root: workspace.path().into(),
        command: RunCommand::Compare,
        options: RunOptions {
            suites: vec!["shop".into(), "account".into()],
            ..Default::default()
        },
    };
    let compared = invoke(executable, &request);
    assert!(
        compared.status.success(),
        "{}",
        String::from_utf8_lossy(&compared.stderr)
    );
    std::fs::create_dir_all(output.join("states")).unwrap();
    std::fs::write(output.join("states/private.json"), "private").unwrap();
    std::fs::write(
        output.join("shop/bitmaps_test/run-one/unreferenced.png"),
        &image,
    )
    .unwrap();
    let sibling = std::fs::read(output.join("account/html_report/config.js")).unwrap();
    request.command = RunCommand::Snapshot;
    request.options.suites = vec!["shop".into()];
    let snapshot = invoke(executable, &request);
    assert!(
        snapshot.status.success(),
        "{}",
        String::from_utf8_lossy(&snapshot.stderr)
    );
    assert_eq!(
        std::fs::read(output.join("account/html_report/config.js")).unwrap(),
        sibling
    );
    let staging = workspace.path().join("staging");
    regressify::desktop::deployment::stage_reports(&output, &["shop".into()], &staging).unwrap();
    let summary = std::fs::read_to_string(staging.join("index.html")).unwrap();
    assert!(summary.contains("<title>Shop Visual Test Report</title>"));
    let manifest_path = summary
        .split_once("name=\"regressify-manifest\" content=\"")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0;
    let read_resource = |url: &str| {
        let (path, version) = url
            .split_once("?v=")
            .expect("Every report resource needs a version");
        let path = percent_encoding::percent_decode_str(path)
            .decode_utf8()
            .unwrap();
        let bytes = std::fs::read(staging.join(path.as_ref())).unwrap();
        assert_eq!(version, regressify::snapshot::get_asset_version(&bytes, 20));
        bytes
    };
    let manifest: serde_json::Value =
        serde_json::from_slice(&read_resource(manifest_path)).unwrap();
    assert_eq!(manifest["heading"], "Shop Visual Tests Summary");
    assert_eq!(
        manifest["subheading"],
        "Compare [Integration](https://integration.example.com/en) and [Preproduction](https://preproduction.example.com/en)"
    );
    assert_eq!(manifest["suites"].as_array().unwrap().len(), 1);
    assert_eq!(manifest["suites"][0]["id"], "shop");
    let report: serde_json::Value = serde_json::from_slice(&read_resource(
        manifest["suites"][0]["report"].as_str().unwrap(),
    ))
    .unwrap();
    let capture = &report["tests"][0];
    assert_ne!(capture["reference"], capture["test"]);
    assert_eq!(read_resource(capture["test"].as_str().unwrap()), image);
    assert_eq!(read_resource(capture["reference"].as_str().unwrap()), image);
    let mut referenced = std::collections::BTreeSet::new();
    for role in ["reference", "test", "diff"] {
        if let Some(url) = capture[role].as_str() {
            referenced.insert(url.split_once("?v=").unwrap().0.to_owned());
        }
    }
    if let Some(previews) = capture["previews"].as_object() {
        for url in previews.values().filter_map(serde_json::Value::as_str) {
            referenced.insert(url.split_once("?v=").unwrap().0.to_owned());
        }
    }
    let staged_images = walkdir::WalkDir::new(staging.join("_snapshot/images"))
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&staging)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(staged_images, referenced);
    assert!(!summary.contains("html_report"));
    for name in [
        "index.html",
        "_headers",
        "_redirects",
        "staticwebapp.config.json",
    ] {
        assert!(
            staging.join(name).is_file(),
            "missing deployment asset: {name}"
        );
    }
    for name in [
        "account",
        "shop",
        "states",
        "shop/bitmaps_test/run-one/unreferenced.png",
        "shop/bitmaps_test/run-one/report.json",
    ] {
        assert!(
            !staging.join(name).exists(),
            "private or unselected artifact was staged: {name}"
        );
    }
}

#[test]
fn bundled_worker_scopes_scenarios_and_preflights_all_approvals() {
    let _setup = desktop_launches();
    let workspace = tempfile::tempdir().unwrap();
    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let reference = lodepng::encode32(&[10_u8, 20, 30, 255], 1, 1).unwrap();
    let current = lodepng::encode32(&[200_u8, 20, 30, 255], 1, 1).unwrap();
    for suite in ["shop", "account"] {
        let output = workspace.path().join(".regressify").join(suite);
        let baseline = output.join("bitmaps_reference");
        let run = output.join("bitmaps_test/run-1");
        std::fs::create_dir_all(&baseline).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        let mut metadata = serde_json::Map::new();
        for scenario in ["home", "home-extra"] {
            let key = format!("{scenario}__FINAL");
            std::fs::write(baseline.join(format!("{key}.png")), &reference).unwrap();
            std::fs::write(run.join(format!("{key}.png")), &current).unwrap();
            metadata.insert(
                key,
                serde_json::json!({
                    "scenarioId": scenario, "scenarioLabel": scenario,
                    "referenceUrl": "https://example.com", "url": "https://example.com"
                }),
            );
        }
        std::fs::write(
            run.join(".report-urls.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "captures": metadata
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let mut request = RunRequest {
        root: workspace.path().into(),
        command: RunCommand::Approve,
        options: RunOptions {
            suites: vec!["shop".into(), "account".into()],
            scenarios: vec![
                "shop:home".parse().unwrap(),
                "account:missing".parse().unwrap(),
            ],
            run_id: "run-1".into(),
            ..Default::default()
        },
    };
    let result = invoke(executable, &request);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("missing"));
    let shop = workspace.path().join(".regressify/shop");
    assert_eq!(
        std::fs::read(shop.join("bitmaps_reference/home__FINAL.png")).unwrap(),
        reference
    );
    request.options.scenarios.pop();
    let result = invoke(executable, &request);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read(shop.join("bitmaps_reference/home__FINAL.png")).unwrap(),
        current
    );
    assert_eq!(
        std::fs::read(shop.join("bitmaps_reference/home-extra__FINAL.png")).unwrap(),
        reference
    );
    assert_eq!(
        std::fs::read(
            workspace
                .path()
                .join(".regressify/account/bitmaps_reference/home__FINAL.png")
        )
        .unwrap(),
        reference
    );
    request.command = RunCommand::Compare;
    let result = invoke(executable, &request);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(shop.join("bitmaps_test/run-1/report.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report["tests"].as_array().unwrap().len(), 1);
    assert_eq!(report["tests"][0]["pair"]["scenarioId"], "home");
}

#[test]
fn bundled_worker_approves_one_report_viewport_and_recalculates_full_scope() {
    let _setup = desktop_launches();
    let workspace = tempfile::tempdir().unwrap();
    let executable = Path::new(env!("CARGO_BIN_EXE_regressify-desktop"));
    let output = workspace.path().join("review-output/shop");
    let baseline = output.join("bitmaps_reference");
    std::fs::create_dir_all(&baseline).unwrap();
    let reference = lodepng::encode32(&[10_u8, 20, 30, 255], 1, 1).unwrap();
    let current = lodepng::encode32(&[200_u8, 20, 30, 255], 1, 1).unwrap();
    let newer = lodepng::encode32(&[70_u8, 20, 30, 255], 1, 1).unwrap();
    for (run_id, png) in [("reviewed-run", &current), ("zz-newer-run", &newer)] {
        let run = output.join("bitmaps_test").join(run_id);
        std::fs::create_dir_all(&run).unwrap();
        let mut metadata = serde_json::Map::new();
        for (key, scenario) in [
            ("home_desktop__FINAL", "home"),
            ("home_desktop__$saved", "home"),
            ("home_mobile__FINAL", "home"),
            ("other_desktop__FINAL", "other"),
            ("hidden_desktop__FINAL", "hidden"),
        ] {
            std::fs::write(baseline.join(format!("{key}.png")), &reference).unwrap();
            std::fs::write(run.join(format!("{key}.png")), png).unwrap();
            metadata.insert(key.into(), serde_json::json!({
                "scenarioId": scenario, "scenarioLabel": scenario,
                "referenceUrl": "https://production.example.test", "url": "https://staging.example.test",
            }));
        }
        std::fs::write(
            run.join(".report-urls.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "captures": metadata,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let mut request = RunRequest {
        root: workspace.path().into(),
        command: RunCommand::Compare,
        options: RunOptions {
            regressify_dir: "review-output".into(),
            run_id: "reviewed-run".into(),
            suites: vec!["shop".into()],
            scenarios: vec!["shop:home".parse().unwrap(), "shop:other".parse().unwrap()],
            ..Default::default()
        },
    };
    let result = invoke(executable, &request);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    request.command = RunCommand::Approve;
    request.options.scenarios.truncate(1);
    request.options.report_capture = Some("home_desktop__FINAL".into());
    let result = invoke(executable, &request);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for key in ["home_desktop__FINAL", "home_desktop__$saved"] {
        assert_eq!(
            std::fs::read(baseline.join(format!("{key}.png"))).unwrap(),
            current
        );
    }
    for key in [
        "home_mobile__FINAL",
        "other_desktop__FINAL",
        "hidden_desktop__FINAL",
    ] {
        assert_eq!(
            std::fs::read(baseline.join(format!("{key}.png"))).unwrap(),
            reference
        );
    }
    let catalog =
        regressify::desktop::results::Catalog::load(&workspace.path().join("review-output"))
            .unwrap();
    assert_eq!(catalog.reports.len(), 1);
    assert_eq!(catalog.reports[0].run_id, "reviewed-run");
    assert_eq!(catalog.reports[0].counts(), (2, 2, 0));
    assert_eq!(catalog.reports[0].report.tests.len(), 4);
    assert!(
        !output
            .join("bitmaps_test/zz-newer-run/report.json")
            .exists()
    );
    assert!(!workspace.path().join(".regressify").exists());
}
