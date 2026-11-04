//! Shared engine entry point. Each invocation owns one process's run settings.

use std::{env::set_current_dir, path::PathBuf};

use anyhow::{Result, anyhow};
use reqwest::Proxy;

use crate::{
    cli::{Cli, Command},
    common, compare,
    diagnostics::{
        record::{
            DiagnosticEvent, Field, FieldValue, OperationId, OperationKind, Outcome, SafeIdentity,
            SafeValue, Severity, Stage,
        },
        runtime,
    },
    github::is_github,
    helpers::{IOExtensions, OnceLockBoolExtensions, OnceLockExtensions},
    init, optimizer, snapshot, test,
    vso::is_azure_devops,
};

/// Engine settings chosen by the caller rather than exposed as CLI arguments.
#[derive(Debug, Default)]
pub(crate) struct EngineOptions {
    pub(crate) operation_id: Option<OperationId>,
    pub(crate) post_run_snapshot: test::PostRunSnapshot,
}

/// Called by the CLI or an isolated desktop worker, never by the GUI thread.
pub async fn execute(cli: Cli) -> Result<()> {
    execute_with(cli, EngineOptions::default()).await
}

pub(crate) async fn execute_with(cli: Cli, options: EngineOptions) -> Result<()> {
    let EngineOptions {
        operation_id,
        post_run_snapshot,
    } = options;
    set_root(&cli)?;
    set_global_vars(&cli)?;
    let (command, phase) = match &cli.command {
        Command::Test(_) => (SafeValue::Test, "Validating test suites"),
        Command::Ref(_) => (SafeValue::Reference, "Validating test suites"),
        Command::Compare(_) => (SafeValue::Compare, "Comparing screenshots"),
        Command::Approve(_) => (SafeValue::Approve, "Approving baselines"),
        Command::Snapshot(_) => (SafeValue::Snapshot, "Generating report"),
        Command::Optimize(_) => (SafeValue::Optimize, "Optimizing images"),
        Command::Clean(_) => (SafeValue::Clean, "Cleaning old test runs"),
        Command::Install => (SafeValue::Install, "Installing Chromium"),
        Command::Init => (SafeValue::Init, "Initializing workspace"),
        Command::Version => (SafeValue::Version, "Reading version"),
    };
    let parent = runtime::current();
    let operation = if command != SafeValue::Version
        && parent
            .as_ref()
            .is_none_or(|context| context.identity.operation_kind != OperationKind::Deployment)
    {
        Some(runtime::Operation::start_identified(
            common::REGRESSIFY_DIR.get_required()?,
            OperationKind::Command,
            None,
            None,
            None,
            operation_id,
        ))
    } else {
        None
    };
    let context = operation
        .as_ref()
        .map(runtime::Operation::context)
        .or(parent);
    let result = runtime::scope(
        context,
        runtime::stage(
            Stage::Preflight,
            [(Field::Command, FieldValue::Kind(command))],
            async {
                crate::progress::emit(crate::progress::Event::Phase { name: phase.into() });
                match cli.command {
                    Command::Init => init::init(),
                    Command::Install => {
                        let executable = test::fetcher()
                            .await
                            .map_err(|error| anyhow!("Failed to fetch the browser: {error:?}"))?;
                        runtime::event(
                            Stage::QueueSetup,
                            DiagnosticEvent::Decision,
                            Severity::Info,
                            [
                                (Field::Reason, FieldValue::Kind(SafeValue::Browser)),
                                (
                                    Field::Artifact,
                                    FieldValue::Identity(SafeIdentity::fingerprint(
                                        executable.to_string_lossy().as_bytes(),
                                    )),
                                ),
                            ],
                        );
                        Ok(())
                    }
                    Command::Version => {
                        println!(
                            "{} ({})",
                            env!("CARGO_PKG_VERSION"),
                            env!("BINARY_GIT_HASH")
                        );
                        Ok(())
                    }
                    Command::Snapshot(args) => {
                        let diagnostic_context = runtime::current();
                        tokio::task::spawn_blocking(move || {
                            runtime::in_scope(diagnostic_context, || snapshot::snapshot(args))
                        })
                        .await
                        .map_err(|error| anyhow!("Snapshot generation task failed: {error}"))?
                    }
                    Command::Optimize(args) => optimizer::optimize(args),
                    Command::Clean(args) => crate::run_history::cleanup::command(
                        common::REGRESSIFY_DIR.get_required()?,
                        args,
                    ),
                    Command::Compare(args) => compare::compare(args),
                    Command::Approve(args) => compare::approve(args),
                    Command::Test(args) => test::exec_test_suites(args, post_run_snapshot).await,
                    Command::Ref(args) => test::exec_ref(args, post_run_snapshot).await,
                }
            },
        ),
    )
    .await;
    if let Some(operation) = operation {
        operation
            .finish(if result.is_ok() {
                Outcome::Success
            } else {
                Outcome::Failure
            })
            .await;
    }
    result
}

fn set_global_vars(cli: &Cli) -> Result<()> {
    let flag = cli.log_level.or_else(|| {
        (cli.log_verbose || cli.verbose).then_some(crate::diagnostics::record::Severity::Debug)
    });
    let environment = std::env::var("REGRESSIFY_LOG_LEVEL").ok();
    let level = crate::config::resolve_log_level(flag, environment.as_deref(), None, None, None)?;
    common::LOG_LEVEL_OVERRIDE
        .set_value((flag.is_some() || environment.is_some()).then_some(level))?;
    if let Some(installation) = crate::diagnostics::install::current() {
        installation.set_console_level(level);
    }
    common::DISABLE_DIAGNOSTIC_ARCHIVE.set_value(crate::diagnostics::health::archives_disabled(
        |name| std::env::var(name).ok(),
    )?)?;
    common::DIAGNOSTIC_OPTIONS.set_value(crate::diagnostics::journal::JournalOptions::resolve(
        cli.diagnostic_chunk_size_bytes,
        cli.diagnostic_disk_budget_bytes,
        |name| std::env::var(name).ok(),
    )?)?;
    let work_dir = common::ROOT_DIR.get_required()?;
    let regressify_dir = if let Some(regressify_dir) = &cli.regressify_dir {
        work_dir.join(regressify_dir)
    } else {
        let directory_name = if let Some(site_env) = &cli.site_env {
            format!(".regressify__{site_env}")
        } else {
            ".regressify".to_string()
        };
        work_dir.join(directory_name)
    };

    common::REGRESSIFY_DIR.set_value(regressify_dir)?;
    if let Some(run_id) = &cli.run_id {
        common::RUN_ID.set_value(run_id.clone())?;
    }
    common::DEBUG.set_value(cli.debug)?;
    common::REQUIRED_LOGIN.set_value(cli.required_login)?;
    common::ASYNC_CAPTURE_LIMIT.set_value(cli.async_capture_limit as usize)?;
    common::ASYNC_CAPTURE_RAMP_MS.set_value(cli.async_capture_ramp_ms)?;

    if let Some(proxy) = &cli.proxy {
        common::PROXY.set_value(Proxy::all(proxy)?)?;
    }
    if cli.no_print {
        common::NO_PRINT.set_bool(true)?;
    }
    if cli.verbose {
        common::VERBOSE.set_bool(true)?;
    }
    if is_github() || is_azure_devops() {
        colored::control::set_override(true);
    }
    Ok(())
}

fn set_root(cli: &Cli) -> Result<()> {
    let work_dir: PathBuf = cli.root.normalize()?;
    set_current_dir(&work_dir)
        .map_err(|error| anyhow!("Failed to set current directory: {error}"))?;
    common::ROOT_DIR.set_value(work_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{install, journal::JournalReader, record::Outcome};
    use clap::Parser;

    #[test]
    fn command_execution_retains_success_and_failure_without_console_output() {
        const CHILD: &str = "REGRESSIFY_COMMAND_JOURNAL_TEST";
        const ROOT: &str = "REGRESSIFY_COMMAND_JOURNAL_ROOT";
        if let Ok(mode) = std::env::var(CHILD) {
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            install::install_subscriber();
            install::configure_console(None, false, true).unwrap();
            let mut cli = Cli::try_parse_from([
                "regressify",
                "--root",
                root.to_str().unwrap(),
                "--no-print",
                "optimize",
            ])
            .unwrap();
            let Command::Optimize(args) = &mut cli.command else {
                panic!("Expected optimize command");
            };
            args.min_size_kb = 0;
            let result = tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(execute(cli));
            let outcome = if mode == "failure" {
                assert!(result.is_err());
                Outcome::Failure
            } else {
                result.unwrap();
                Outcome::Success
            };
            let output = root.join(".regressify");
            let archives: Vec<_> = std::fs::read_dir(output.join("_diagnostics"))
                .expect("Every engine command must retain an operation journal")
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            assert_eq!(archives.len(), 1);
            let operation = archives[0].clone().try_into().unwrap();
            let reader = JournalReader::open(&output, &operation).unwrap();
            assert_eq!(reader.metadata.operation_kind, OperationKind::Command);
            assert_eq!(reader.verified_index().unwrap().outcome, outcome);
            let mut has_command = false;
            let mut has_artifact_failure = false;
            assert!(
                reader
                    .visit(|record| {
                        has_command |= record.fields.get(&Field::Command)
                            == Some(&FieldValue::Kind(SafeValue::Optimize));
                        has_artifact_failure |= record.fields.contains_key(&Field::Artifact)
                            && record.severity == Severity::Error
                            && record.error_cause.is_some();
                        assert!(
                            !serde_json::to_string(&record)
                                .unwrap()
                                .contains("invalid.png")
                        );
                    })
                    .unwrap()
                    .records
                    >= 3
            );
            assert!(has_command);
            assert_eq!(has_artifact_failure, mode == "failure");
            return;
        }
        for mode in ["success", "failure"] {
            let root = tempfile::tempdir().unwrap();
            let output = root.path().join(".regressify");
            std::fs::create_dir(&output).unwrap();
            if mode == "failure" {
                std::fs::write(output.join("invalid.png"), b"not a PNG").unwrap();
            }
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "application::tests::command_execution_retains_success_and_failure_without_console_output",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .env(ROOT, root.path())
                .env_remove("REGRESSIFY_DISABLE_DIAGNOSTIC_ARCHIVE")
                .env_remove("REGRESSIFY_LOG_LEVEL")
                .env_remove("REGRESSIFY_DIAGNOSTIC_CHUNK_SIZE_BYTES")
                .env_remove("REGRESSIFY_DIAGNOSTIC_DISK_BUDGET_BYTES")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(
                !String::from_utf8_lossy(&result.stderr).contains("Starting image optimization")
            );
            assert!(!String::from_utf8_lossy(&result.stderr).contains("invalid.png"));
        }
    }
}
