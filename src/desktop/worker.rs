//! The desktop executable doubles as its isolated, headless engine worker.

use std::{io::Read, path::PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::runner::RunRequest;

pub const WORKER_FLAG: &str = "--internal-worker";
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerRequest {
    pub run: RunRequest,
    pub events_path: Option<PathBuf>,
    #[serde(default)]
    pub scratch_directory: Option<PathBuf>,
    /// Deployment target settings holding the last recorded publication of each target.
    /// Without it, every deployment publishes.
    #[serde(default)]
    pub deployment_settings: Option<PathBuf>,
}

pub fn read_request(input: impl Read) -> Result<WorkerRequest> {
    let mut bytes = Vec::new();
    input
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_REQUEST_BYTES,
        "Run request exceeds 1 MiB"
    );
    serde_json::from_slice(&bytes).context("Invalid desktop run request")
}

/// No desktop view reads the combined web report: Results reads run reports
/// directly, and Build Report and Deploy regenerate it for their selected
/// suites. A post-run rebuild would load, lock, and possibly compare every
/// output directory for no visible gain, so desktop Test/Reference skip it.
fn engine_options(run: &RunRequest) -> crate::application::EngineOptions {
    crate::application::EngineOptions {
        operation_id: run.options.diagnostic_operation_id.clone(),
        post_run_snapshot: crate::test::PostRunSnapshot::Skip,
    }
}

pub fn run_stdio() -> Result<()> {
    crate::diagnostics::install::install_subscriber();
    let startup = crate::diagnostics::startup::StartupLog::begin_for(
        crate::diagnostics::record::SafeValue::Worker,
    );
    let result = run_request();
    startup.finish(&result);
    crate::diagnostics::install::flush();
    result
}

fn run_request() -> Result<()> {
    let request = read_request(std::io::stdin().lock())?;
    if let Some(path) = &request.events_path {
        crate::progress::install(path)?;
    }
    let result = (|| -> Result<()> {
        let cli = request.run.cli()?;
        crate::diagnostics::install::configure_console(
            cli.log_level,
            cli.log_verbose || cli.verbose,
            cli.no_print,
        )?;
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let work = async {
                    let mut managed_staging = None;
                    let deployment = if request.run.command == super::runner::RunCommand::Deploy {
                        let job = request
                            .run
                            .options
                            .deployment
                            .as_ref()
                            .context("Deployment settings are missing")?;
                        use crate::diagnostics::{record::*, runtime};
                        runtime::stage_sync(
                            Stage::Preflight,
                            [
                                (
                                    Field::Provider,
                                    FieldValue::Kind(job.target.provider.diagnostic_value()),
                                ),
                                (
                                    Field::Target,
                                    FieldValue::Identity(SafeIdentity::fingerprint(&job.target.id)),
                                ),
                                (
                                    Field::SelectedSuites,
                                    FieldValue::Number(job.suite_ids.len() as u64),
                                ),
                            ],
                            || job.validate(),
                        )?;
                        let output = request.run.options.output_directory(&request.run.root);
                        for warning in crate::run_history::staging::cleanup_abandoned(&output) {
                            eprintln!("{warning}");
                        }
                        if request.scratch_directory.is_none() {
                            managed_staging =
                                Some(crate::run_history::staging::StagingDirectory::create(
                                    &output,
                                    crate::run_history::staging::StagingKind::Deployment,
                                )?);
                        }
                        let directory = request
                            .scratch_directory
                            .as_deref()
                            .or_else(|| managed_staging.as_ref().map(|staging| staging.path()))
                            .context("Deployment staging directory is missing")?;
                        ensure!(
                            directory.is_absolute()
                                && directory.is_dir()
                                && !std::fs::symlink_metadata(directory)?
                                    .file_type()
                                    .is_symlink()
                                && std::fs::read_dir(directory)?.next().is_none(),
                            "Deployment staging directory must be a new empty directory"
                        );
                        let token = job
                            .target
                            .token(&request.run.root)
                            .context("Cannot load the deployment token")?;
                        runtime::event(
                            Stage::Preflight,
                            DiagnosticEvent::Decision,
                            Severity::Info,
                            [(
                                Field::CredentialPresent,
                                FieldValue::Boolean(!token.is_empty()),
                            )],
                        );
                        Some((
                            job,
                            super::deployment::DeploymentClient::new(&job.target, &token)?,
                        ))
                    } else {
                        None
                    };
                    if let Some((job, client)) = deployment {
                        let directory = request
                            .scratch_directory
                            .as_deref()
                            .or_else(|| managed_staging.as_ref().map(|staging| staging.path()))
                            .context("Deployment staging directory is missing")?
                            .to_path_buf();
                        let options = &request.run.options;
                        let context = super::deployment::incremental::ReportDeployment {
                            job: job.clone(),
                            root: request.run.root.clone(),
                            output: options.output_directory(&request.run.root),
                            settings: request.deployment_settings.clone(),
                            title: options.title.clone(),
                            heading: options.heading.clone(),
                            subheading: options.subheading.clone(),
                        };
                        use super::deployment::incremental::Preparation;
                        let outcome = match context.prepare().await? {
                            Preparation::Done(outcome) => outcome,
                            Preparation::Generate(prepared) => {
                                context
                                    .generate_and_publish(*prepared, &directory, &client, || {
                                        request.run.cli()
                                    })
                                    .await?
                            }
                        };
                        crate::progress::emit(crate::progress::Event::Deployed {
                            target: job.target.name.clone(),
                            url: outcome.url,
                            fingerprint: outcome.fingerprint,
                            input_fingerprint: outcome.inputs,
                            unchanged: outcome.unchanged,
                            suites: outcome.suites,
                        });
                    } else {
                        crate::application::execute_with(cli, engine_options(&request.run)).await?;
                    }
                    Ok(())
                };
                if request.run.command == super::runner::RunCommand::Deploy {
                    super::deployment::with_deployment_journal_id(
                        &request.run.options.output_directory(&request.run.root),
                        request.run.options.diagnostic_operation_id.clone(),
                        work,
                    )
                    .await
                } else {
                    work.await
                }
            })
    })();
    crate::progress::emit(crate::progress::Event::Finished {
        error: result.as_ref().err().map(|error| format!("{error:#}")),
    });
    crate::diagnostics::install::flush();
    crate::progress::finish()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::runner::{RunCommand, RunOptions};

    #[test]
    fn worker_request_round_trip_and_limits() {
        let request = RunRequest {
            root: "QA workspace".into(),
            command: RunCommand::Init,
            options: RunOptions::default(),
        };
        let encoded = serde_json::to_vec(&WorkerRequest {
            run: request.clone(),
            events_path: None,
            scratch_directory: None,
            deployment_settings: None,
        })
        .unwrap();
        let decoded = read_request(encoded.as_slice()).unwrap();
        assert_eq!(decoded.run.root, request.root);
        assert_eq!(decoded.run.command, request.command);
        assert!(read_request(b"not JSON".as_slice()).is_err());
        assert!(read_request(vec![b' '; MAX_REQUEST_BYTES + 1].as_slice()).is_err());
    }

    #[test]
    fn desktop_capture_runs_skip_the_post_run_report_and_keep_their_operation() {
        let operation = crate::diagnostics::record::OperationId::new();
        for (command, suites, scenarios) in [
            (RunCommand::Test, vec!["shop"], vec![]),
            (RunCommand::Reference, vec!["shop"], vec![]),
            (RunCommand::Reference, vec!["shop"], vec!["shop:home"]),
            (RunCommand::Test, vec!["shop", "blog"], vec![]),
        ] {
            let request = RunRequest {
                root: "QA workspace".into(),
                command,
                options: RunOptions {
                    suites: suites.into_iter().map(String::from).collect(),
                    scenarios: scenarios
                        .into_iter()
                        .map(|filter| filter.parse().unwrap())
                        .collect(),
                    diagnostic_operation_id: Some(operation.clone()),
                    ..RunOptions::default()
                },
            };
            let options = engine_options(&request);
            assert_eq!(
                options.post_run_snapshot,
                crate::test::PostRunSnapshot::Skip,
                "{command:?}"
            );
            assert_eq!(options.operation_id, Some(operation.clone()));
        }
    }
}
