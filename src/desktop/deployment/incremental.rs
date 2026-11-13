//! Report deployments that update, keep, and remove suites on a published site.
//!
//! A deployment first reads what the site serves (its published state, a legacy manifest, or
//! the local fallback), then plans which suites to generate, keep, and remove. Only generated
//! suites are staged; kept suites reach the merged manifest by summary and the provider
//! manifest by hash. The staged site's state file records every suite for the next update.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use rayon::prelude::*;

use super::{
    DeployTarget, DeploymentClient, DeploymentJob, DeploymentMode, DeploymentProvider, KeptFiles,
    phase,
    site_state::{
        self, KnownSite, Plan, RemoteState, SiteState, StateFile, SuiteInputs, SuiteState,
    },
};

/// Everything a report deployment needs besides its provider client.
#[derive(Clone)]
pub(crate) struct ReportDeployment {
    pub job: DeploymentJob,
    /// Workspace root, for its report text settings.
    pub root: PathBuf,
    /// Report output directory with local results.
    pub output: PathBuf,
    /// Deployment settings holding the target's publication record.
    pub settings: Option<PathBuf>,
    pub title: Option<String>,
    pub heading: Option<String>,
    pub subheading: Option<String>,
}

/// The result of a report deployment, published or skipped.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub url: Option<String>,
    pub fingerprint: Option<String>,
    pub inputs: Option<String>,
    pub unchanged: bool,
    /// Suites the site serves afterwards; unknown when nothing was published.
    pub suites: Option<Vec<String>>,
}

pub(crate) enum Preparation {
    /// Nothing changed; no generation is needed.
    Done(Outcome),
    /// Generate `plan.generate` locally, then call `publish`.
    Generate(Box<Prepared>),
}

#[derive(Clone)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct Prepared {
    pub plan: Plan,
    /// What earlier attempts' storage budget refusals decided (`retry_with_kept_suites`).
    pub budget: BudgetRetry,
    /// The site's own fingerprint, when it could be read.
    pub remote: Option<String>,
    pub inputs: Option<String>,
    /// Pre-generation input fingerprint of each generated suite, recorded after publishing.
    pub suite_inputs: BTreeMap<String, String>,
    /// Suite input records loaded before planning.
    pub records: SuiteInputs,
    /// The state the site served before this deployment (from the site or the local copy).
    pub served: Option<SiteState>,
    /// Verified copies of `plan.local` suites' files, set by `stash_local`.
    pub stash: Option<PathBuf>,
}

/// How a deployment retries after its provider refused it for the storage budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BudgetRetry {
    /// The provider may reduce image quality in the suites it uploads although other suites
    /// stay kept: set once no kept suite is expected to save anything if brought back.
    pub reduce_quality: bool,
    /// The last retry that spread quality reduction over more kept suites, so the next one
    /// sizes its batch by what they actually saved.
    pub last_spread: Option<Spread>,
}

/// A retry that brought back `suites` kept suites to reduce in quality when the site was
/// `remaining` bytes over its budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Spread {
    pub remaining: u64,
    pub suites: usize,
}

impl ReportDeployment {
    pub fn args(&self) -> crate::cli::args::SnapshotArgs {
        crate::cli::args::SnapshotArgs {
            test_suite: Vec::new(),
            title: self.title.clone(),
            heading: self.heading.clone(),
            subheading: self.subheading.clone(),
        }
    }

    fn provider(&self) -> DeploymentProvider {
        self.job.target.provider
    }

    /// Reads the site, runs the early unchanged check, and plans the deployment.
    pub async fn prepare(&self) -> Result<Preparation> {
        let url = match &self.settings {
            Some(path) => super::current_settings(path, &self.job)?
                .targets
                .into_iter()
                .find(|target| target.id == self.job.target.id)
                .and_then(|target| target.published_url),
            None => self.job.target.published_url.clone(),
        };
        let remote = match url {
            Some(url) => {
                phase("Reading the suites this site serves");
                Some(site_state::fetch(&url).await)
            }
            None => None,
        };
        let (site, remote_fingerprint) = self.known_site(remote)?;
        let context = self.clone();
        let fingerprint = remote_fingerprint.clone();
        // Every provider records suite inputs so the suite list can tell what changed; only
        // providers that keep files by hash also skip unchanged suites (`Plan::keep_unchanged`).
        let per_suite = self.settings.is_some();
        let diagnostic_context = crate::diagnostics::runtime::current();
        let (inputs, unchanged, suite_inputs) = super::with_heartbeat(
            "Checking the selected reports for changes",
            tokio::task::spawn_blocking(move || {
                crate::diagnostics::runtime::in_scope(diagnostic_context, || {
                    let (inputs, unchanged) = super::check_unchanged_inputs_on(
                        &context.output,
                        &context.root,
                        &context.job,
                        &context.args(),
                        context.settings.as_deref(),
                        fingerprint.as_deref(),
                    )?;
                    let suite_inputs = if unchanged.is_none() && per_suite {
                        context.suite_inputs()
                    } else {
                        BTreeMap::new()
                    };
                    anyhow::Ok((inputs, unchanged, suite_inputs))
                })
            }),
        )
        .await
        .context("Deployment change check failed")??;
        if let Some((url, fingerprint)) = unchanged {
            return Ok(Preparation::Done(Outcome {
                url: Some(url),
                fingerprint: Some(fingerprint),
                inputs,
                unchanged: true,
                suites: None,
            }));
        }
        let local = self.local_suites(&site);
        let mut plan = site_state::plan(
            self.job.mode,
            self.provider(),
            &self.job.suite_ids,
            &self.job.remove_ids,
            &site,
            &local,
        )?;
        let records = match &self.settings {
            Some(path) if per_suite => site_state::load_inputs(path, &self.job.target.id),
            _ => SuiteInputs::default(),
        };
        // Deploy anyway regenerates every selected suite, in case the site's copy is suspect;
        // Replace does too (see `Plan::keep_unchanged`).
        let unchanged = if self.job.force {
            BTreeSet::new()
        } else {
            records.unchanged(&site, &suite_inputs)
        };
        plan.reuse_local(
            self.job.mode,
            self.provider(),
            &site,
            &self.job.suite_ids,
            &unchanged,
        );
        if !self.job.force {
            plan.keep_unchanged(self.job.mode, self.provider(), &site, &unchanged);
            let skipped = unchanged
                .iter()
                .filter(|id| plan.kept.iter().any(|suite| suite.summary.id == **id))
                .map(String::as_str)
                .collect::<Vec<_>>();
            if !skipped.is_empty() {
                phase(&format!(
                    "Keeping unchanged selected suites on the site: {}",
                    skipped.join(", ")
                ));
            }
        }
        // Every selected suite left on the site matches its current inputs: generated now, or
        // kept or re-uploaded because they are unchanged.
        phase(&format!(
            "Deployment plan: generate {}, keep {}, reuse {}, remove {}",
            plan.generate.len(),
            plan.kept.len(),
            plan.local.len(),
            plan.removed.len()
        ));
        Ok(Preparation::Generate(Box::new(Prepared {
            plan,
            budget: BudgetRetry::default(),
            remote: remote_fingerprint,
            inputs,
            suite_inputs,
            records,
            served: match site {
                KnownSite::State(state) => Some(state),
                KnownSite::Legacy(_) | KnownSite::Empty => None,
            },
            stash: None,
        })))
    }

    /// Copies the files of `prepared.plan.local` suites from the report output into `stash`,
    /// each checked against the size and provider hash the site's state records. This must run
    /// before generation, which prunes the suites it does not generate from the output. A
    /// suite with a missing or different file is regenerated instead, so a stale local copy
    /// can only cost time, never publish content the site did not serve. For Azure, the hashes
    /// describe the staged files before any package optimization, which is what the local
    /// report holds.
    pub fn stash_local(&self, prepared: &mut Prepared, stash: &Path) -> Result<()> {
        if prepared.plan.local.is_empty() {
            return Ok(());
        }
        let output = self.output.canonicalize()?;
        phase(&format!(
            "Checking local copies of {} kept suites against the site",
            prepared.plan.local.len()
        ));
        let mut regenerate = Vec::new();
        for suite in &prepared.plan.local {
            let id = &suite.summary.id;
            let copied = suite
                .files
                .par_iter()
                .try_for_each(|(path, file)| -> Result<()> {
                    // Same checks as staging: no symlinks, nothing outside the report output.
                    let source = super::artifact_source(&output, Path::new(path), true)?
                        .with_context(|| format!("Missing local copy of '{path}'"))?;
                    // The size is compared before reading, so a wrong file is never loaded.
                    ensure!(
                        std::fs::metadata(&source)?.len() == file.size,
                        "The local copy of '{path}' differs from the site"
                    );
                    let bytes = std::fs::read(&source)
                        .with_context(|| format!("Cannot read the local copy of '{path}'"))?;
                    ensure!(
                        provider_hash(self.provider(), &bytes, path) == file.hash,
                        "The local copy of '{path}' differs from the site"
                    );
                    let target = stash.join(path);
                    std::fs::create_dir_all(target.parent().context("Invalid stash path")?)?;
                    std::fs::write(&target, &bytes)
                        .with_context(|| format!("Cannot stash the local copy of '{path}'"))?;
                    Ok(())
                });
            if let Err(error) = copied {
                phase(&format!("Regenerating '{id}': {error:#}"));
                regenerate.push(id.clone());
            }
        }
        for id in &regenerate {
            prepared.plan.regenerate_local(id);
        }
        if !prepared.plan.local.is_empty() {
            phase(&format!(
                "Reusing local copies of kept suites: {}",
                prepared
                    .plan
                    .local
                    .iter()
                    .map(|suite| suite.summary.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        prepared.stash = Some(stash.to_path_buf());
        Ok(())
    }

    /// Puts reused suites back into the report output once generation pruned them from it,
    /// whatever the deployment's outcome: the next deployment can only reuse files the output
    /// still has. When staging completed (`staged`), the staged site is written back whole,
    /// so the local report lists exactly the suites it holds; otherwise only the stashed files
    /// return. Callers only warn on failure: the next deployment regenerates what it cannot
    /// reuse.
    fn restore_reused(
        &self,
        staged: Option<&Path>,
        stash: &Path,
        local: &[SuiteState],
    ) -> Result<()> {
        let source = staged.unwrap_or(stash);
        for suite in local {
            for path in suite.files.keys() {
                restore_file(&self.output, &source.join(path), path, false)?;
            }
        }
        let Some(staged) = staged else {
            return Ok(());
        };
        let last = [
            "_snapshot/status.json",
            "_snapshot/data/manifest.json",
            "index.html",
        ];
        // The staged site's bundled frontend and hosting files come first: the restored entry
        // page names their current versions, which an older output may lack, for example when
        // only shared files were generated (in scratch space) because every suite was reused.
        for entry in walkdir::WalkDir::new(staged).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = site_path(entry.path().strip_prefix(staged)?);
            let suite_file = [
                "_snapshot/images/",
                "_snapshot/data/suites/",
                "_snapshot/data/logs/",
            ]
            .iter()
            .any(|prefix| path.starts_with(prefix));
            if suite_file || path == site_state::STATE_PATH || last.contains(&path.as_str()) {
                continue;
            }
            restore_file(&self.output, entry.path(), &path, false)?;
        }
        // The entry page and manifest go last, so they never name files that are not there.
        for path in last {
            if staged.join(path).is_file() {
                restore_file(&self.output, &staged.join(path), path, true)?;
            }
        }
        let suites = SiteState::parse(&std::fs::read(staged.join(site_state::STATE_PATH))?)?
            .suites
            .into_keys()
            .collect::<Vec<_>>();
        crate::snapshot::artifact_inventory(&self.output, &suites, &[])
            .context("The restored local report is incomplete")?;
        Ok(())
    }

    /// Generates what `prepared` plans, then stages and publishes it.
    ///
    /// Beside kept suites, Cloudflare only recompresses the generated suites losslessly, into
    /// the storage budget the kept ones leave. Each time that falls short
    /// ([`super::KeptSuitesOverBudget`]), the deployment runs again with more kept suites,
    /// those expected to save what is still missing, uploaded from local files. Once no kept
    /// suite is expected to help losslessly, the uploaded suites may be reduced in quality, no
    /// lower than `SPREAD_QUALITY` while suites stay kept, and each further refusal spreads the
    /// reduction over more kept suites; the full floors apply only once every suite is local.
    /// `snapshot` builds the snapshot command for each generation.
    pub async fn generate_and_publish(
        &self,
        prepared: Prepared,
        directory: &Path,
        client: &DeploymentClient,
        mut snapshot: impl FnMut() -> Result<crate::cli::Cli>,
    ) -> Result<Outcome> {
        let mut next = prepared;
        // Every retry brings back at least one more kept suite or, once, allows quality
        // reduction, so the attempts end with the whole site local at the latest, which the
        // budget never refuses for kept suites.
        let mut attempt = 0_usize;
        loop {
            let retry = next.clone();
            let suffix = match attempt {
                0 => String::new(),
                attempt => format!("-retry{attempt}"),
            };
            let error = match self
                .generate_and_publish_once(next, directory, &suffix, client, snapshot()?)
                .await
            {
                Err(error) => error,
                outcome => return outcome,
            };
            let before = retry
                .plan
                .local
                .iter()
                .map(|suite| suite.summary.id.clone())
                .collect::<BTreeSet<_>>();
            let reduced = retry.budget.reduce_quality;
            let Some(retry) = retry_with_kept_suites(retry, &error) else {
                return Err(error);
            };
            let added = retry
                .plan
                .local
                .iter()
                .map(|suite| suite.summary.id.as_str())
                .filter(|id| !before.contains(*id))
                .collect::<Vec<_>>()
                .join(", ");
            phase(
                &if retry.budget.reduce_quality && !reduced && added.is_empty() {
                    format!(
                        "No kept suite is expected to save more losslessly; allowing quality reduction in the uploaded suites ({} kept unchanged)",
                        retry.plan.kept.len()
                    )
                } else if retry.budget.reduce_quality && !reduced {
                    format!(
                        "No kept suite is expected to save more losslessly; allowing quality reduction in the uploaded suites and the kept suites expected to save the most ({} kept unchanged): {added}",
                        retry.plan.kept.len()
                    )
                } else if reduced {
                    format!(
                        "Spreading quality reduction over more kept suites ({} kept unchanged): {added}",
                        retry.plan.kept.len()
                    )
                } else {
                    format!(
                        "Optimizing more kept suites, expected to save what is still missing ({} kept unchanged): {added}",
                        retry.plan.kept.len()
                    )
                },
            );
            next = retry;
            attempt += 1;
        }
    }

    /// One attempt: stash reused suites, generate, then publish, in folders named with `suffix`.
    async fn generate_and_publish_once(
        &self,
        mut prepared: Prepared,
        directory: &Path,
        suffix: &str,
        client: &DeploymentClient,
        mut cli: crate::cli::Cli,
    ) -> Result<Outcome> {
        // Reused suites' files are copied out before generation prunes them from the output.
        if !prepared.plan.local.is_empty() {
            let stash = directory.join(format!("local{suffix}"));
            let checker = self.clone();
            let scope = crate::diagnostics::runtime::current();
            prepared = super::with_heartbeat(
                "Checking local copies of kept suites",
                tokio::task::spawn_blocking(move || {
                    crate::diagnostics::runtime::in_scope(scope, || {
                        checker
                            .stash_local(&mut prepared, &stash)
                            .map(|()| prepared)
                    })
                }),
            )
            .await
            .context("Local copy check failed")??;
        }
        let generated = async {
            if prepared.plan.generate.is_empty() {
                // Removals and unchanged selections generate no suite. The shared site files
                // are built in scratch space so the local report keeps its suites.
                let scratch = directory.join(format!("generated{suffix}"));
                std::fs::create_dir_all(&scratch)?;
                let workspace = crate::config::load_workspace_config(&self.root)?;
                let args = self.args();
                let target = scratch.clone();
                tokio::task::spawn_blocking(move || {
                    crate::snapshot::generate_site(&target, &[], &args, &workspace)
                })
                .await
                .context("Snapshot generation task failed")??;
                anyhow::Ok(scratch)
            } else {
                // Kept suites may need regeneration too (Azure, legacy sites, a budget retry).
                if let crate::cli::Command::Snapshot(args) = &mut cli.command {
                    args.test_suite = prepared
                        .plan
                        .generate
                        .iter()
                        .map(|id| glob::Pattern::escape(id))
                        .collect();
                }
                use crate::diagnostics::{record::Stage, runtime};
                super::with_heartbeat(
                    "Generating selected report snapshots",
                    runtime::stage(Stage::SnapshotStaging, [], crate::application::execute(cli)),
                )
                .await
                .context("Cannot generate report snapshots for deployment")?;
                Ok(self.output.clone())
            }
        }
        .await;
        // Generation may have pruned reused suites from the output.
        let generated = match generated {
            Ok(generated) => generated,
            Err(error) => {
                self.restore_after_failure(&prepared);
                return Err(error);
            }
        };
        self.publish(
            prepared,
            &generated,
            &directory.join(format!("report{suffix}")),
            client,
        )
        .await
    }

    /// Restores reused suites after a failure before staging (generation, for example).
    pub fn restore_after_failure(&self, prepared: &Prepared) {
        if let Some(stash) = &prepared.stash
            && !prepared.plan.local.is_empty()
            && let Err(error) = self.restore_reused(None, stash, &prepared.plan.local)
        {
            eprintln!(
                "Warning: Cannot put the reused suites back into the local report; the next deployment regenerates them: {error:#}"
            );
        }
    }

    /// Per-suite input fingerprints of the selected suites, as the desktop app's suite list
    /// computes them. A suite that cannot be fingerprinted is left out, so it is regenerated
    /// and not recorded.
    fn suite_inputs(&self) -> BTreeMap<String, String> {
        self.job
            .suite_ids
            .iter()
            .filter_map(|id| {
                match crate::snapshot::suite_input_fingerprint(&self.output, id) {
                    Ok(inputs) => Some((id.clone(), inputs)),
                    Err(error) => {
                        eprintln!(
                            "Warning: Cannot check suite '{id}' for changes; regenerating it: {error:#}"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    /// Records the inputs of the suites this site now serves as they were published.
    fn record_suite_inputs(
        &self,
        records: &SuiteInputs,
        state: &SiteState,
        inputs: &BTreeMap<String, String>,
    ) {
        let Some(path) = &self.settings else {
            return;
        };
        let saved = records
            .after(state, inputs)
            .and_then(|records| site_state::save_inputs(path, &self.job.target.id, &records));
        if let Err(error) = saved {
            eprintln!("Warning: Cannot record the deployed suite inputs: {error:#}");
        }
    }

    /// What the site serves: its own state first, the local copy when it cannot be read.
    fn known_site(&self, remote: Option<RemoteState>) -> Result<(KnownSite, Option<String>)> {
        let local = || -> Result<Option<SiteState>> {
            match &self.settings {
                Some(path) => site_state::load_local(path, &self.job.target.id),
                None => Ok(None),
            }
        };
        let unreadable = |reason: String| -> Result<(KnownSite, Option<String>)> {
            if let Some(state) = local()? {
                eprintln!(
                    "Warning: Using the last state this workspace published, because the site could not be read: {reason}"
                );
                return Ok((KnownSite::State(state), None));
            }
            if self.job.mode == DeploymentMode::Replace {
                eprintln!("Warning: The site could not be read; replacing it anyway: {reason}");
                return Ok((KnownSite::Empty, None));
            }
            bail!(
                "Cannot read which suites this site serves ({reason}); retry when it is reachable, or replace the whole site"
            )
        };
        match remote {
            Some(RemoteState::Found(state)) => {
                let fingerprint = state.fingerprint.clone();
                Ok((KnownSite::State(state), Some(fingerprint)))
            }
            Some(RemoteState::Legacy(suites)) => Ok((KnownSite::Legacy(suites), None)),
            // A site this workspace published to that now serves no report was replaced by
            // something else, or hides it; keeping suites blindly could drop them.
            Some(RemoteState::Empty)
                if self.job.mode == DeploymentMode::Update && self.published_before()? =>
            {
                unreadable("it serves no Regressify report".into())
            }
            Some(RemoteState::Empty) => Ok((KnownSite::Empty, None)),
            Some(RemoteState::Unreachable(reason)) => unreadable(reason),
            Some(RemoteState::Invalid(reason)) => {
                unreadable(format!("its published state is invalid: {reason}"))
            }
            // Never published, or published privately without a URL to read.
            None => match local()? {
                Some(state) => Ok((KnownSite::State(state), None)),
                None if self.job.mode == DeploymentMode::Update && self.published_before()? => {
                    bail!(
                        "This site was published before, but which suites it serves is unknown here; replace the whole site"
                    )
                }
                None => Ok((KnownSite::Empty, None)),
            },
        }
    }

    /// Whether this workspace recorded a report publication to the target.
    fn published_before(&self) -> Result<bool> {
        let Some(path) = &self.settings else {
            return Ok(false);
        };
        site_state::published_before(path, &self.job.target.id)
    }

    /// Suites with local results that could be generated.
    fn local_suites(&self, site: &KnownSite) -> BTreeSet<String> {
        let on_site = site.suite_ids();
        self.job
            .catalog_ids
            .iter()
            .chain(&on_site)
            .filter(|id| crate::snapshot::valid_suite_id(id) && self.output.join(id).is_dir())
            .cloned()
            .collect()
    }

    /// Stages the generated suites with the kept ones, records the site state, and publishes
    /// unless the site already serves the same content.
    pub async fn publish(
        &self,
        prepared: Prepared,
        generated: &Path,
        staging: &Path,
        client: &DeploymentClient,
    ) -> Result<Outcome> {
        let reused = prepared
            .stash
            .clone()
            .filter(|_| !prepared.plan.local.is_empty())
            .map(|stash| (stash, prepared.plan.local.clone()));
        let result = self
            .publish_staged(prepared, generated, staging, client)
            .await;
        if let Some((stash, local)) = reused {
            // A complete staged site (its state is written last) is the best local report;
            // without one, the stashed files at least remain available for the next update.
            let staged = staging
                .join(site_state::STATE_PATH)
                .is_file()
                .then(|| staging.to_path_buf());
            let context = self.clone();
            let scope = crate::diagnostics::runtime::current();
            let restored = tokio::task::spawn_blocking(move || {
                crate::diagnostics::runtime::in_scope(scope, || {
                    context.restore_reused(staged.as_deref(), &stash, &local)
                })
            })
            .await
            .context("Local report restore task failed")
            .and_then(|result| result);
            if let Err(error) = restored {
                eprintln!(
                    "Warning: Cannot put the reused suites back into the local report; the next deployment regenerates them: {error:#}"
                );
            }
        }
        result
    }

    async fn publish_staged(
        &self,
        prepared: Prepared,
        generated: &Path,
        staging: &Path,
        client: &DeploymentClient,
    ) -> Result<Outcome> {
        let Prepared {
            plan,
            budget,
            remote,
            inputs,
            suite_inputs,
            records,
            served,
            stash,
        } = prepared;
        ensure!(
            plan.local.is_empty() || stash.is_some(),
            "Local copies of kept suites were not checked before generation"
        );
        let context = self.clone();
        let generated = generated.to_path_buf();
        let destination = staging.to_path_buf();
        let kept = plan.kept.clone();
        let mut local = plan.local.clone();
        let generate = plan.generate.clone();
        let provider = self.provider();
        let diagnostic_context = crate::diagnostics::runtime::current();
        let (fingerprint, state) = super::with_heartbeat(
            "Staging selected reports",
            tokio::task::spawn_blocking(move || {
                crate::diagnostics::runtime::in_scope(diagnostic_context, || {
                    let produced = crate::snapshot::suite_summaries(&generated)?
                        .into_iter()
                        .map(|summary| summary.id)
                        .collect::<BTreeSet<_>>();
                    let missing = generate
                        .iter()
                        .filter(|id| !produced.contains(*id))
                        .cloned()
                        .collect::<Vec<_>>();
                    if !missing.is_empty() {
                        bail!(
                            "No completed results to publish for {}; run them first, or remove them from the site",
                            missing.join(", ")
                        );
                    }
                    let summaries = kept
                        .iter()
                        .chain(&local)
                        .map(|suite| suite.summary.clone())
                        .collect::<Vec<_>>();
                    let inventory =
                        super::stage_site(&generated, &generate, &summaries, &destination)?;
                    if let Some(stash) = &stash {
                        add_local_copies(stash, &mut local, &destination, provider)?;
                    }
                    let mut all = generate.clone();
                    all.extend(summaries.iter().map(|summary| summary.id.clone()));
                    all.sort();
                    phase("Comparing staged reports with the last deployment to this target");
                    let fingerprint =
                        super::deployment_fingerprint(&destination, &all, &context.job.target)?;
                    let unchanged_suites = kept.iter().chain(&local).cloned().collect::<Vec<_>>();
                    let state = build_state(
                        &destination,
                        &inventory,
                        &unchanged_suites,
                        &context.job.target,
                        &fingerprint,
                    )?;
                    let bytes = state.to_bytes()?;
                    crate::helpers::write_file_atomically(
                        &destination.join(site_state::STATE_PATH),
                        &bytes,
                    )
                    .context("Cannot stage the site state")?;
                    Ok((fingerprint, state))
                })
            }),
        )
        .await
        .context("Deployment staging task failed")?
        .context("Cannot stage selected reports for deployment")?;
        let suites = state.suites.keys().cloned().collect::<Vec<_>>();
        let unchanged_url = super::decide_publication(
            &self.job,
            self.settings.as_deref(),
            &fingerprint,
            remote.as_deref(),
            inputs.as_deref(),
        )?;
        if let Some(url) = unchanged_url {
            // The site already serves this content, so its suites match these inputs. Records
            // name the state the site serves, which a provider may have rewritten (Cloudflare
            // does for optimized images), never the state staged here.
            if let Some(served) = &served {
                self.record_suite_inputs(&records, served, &suite_inputs);
            }
            return Ok(Outcome {
                url: Some(url),
                fingerprint: Some(fingerprint),
                inputs,
                unchanged: true,
                suites: None,
            });
        }
        let kept_files = plan
            .kept
            .iter()
            .flat_map(|suite| suite.files.clone())
            .collect::<KeptFiles>();
        if let Some(path) = &self.settings
            && !plan.removed.is_empty()
        {
            // If this upload lands but the run fails, a kept local copy would list removed
            // suites, and a later unreadable update could bring them back.
            site_state::forget_local(path, &self.job.target.id)
                .context("Cannot clear the local site state before removing suites")?;
        }
        let published = client
            .deploy_keeping(
                staging,
                &self.job.target,
                super::Keeping {
                    files: &kept_files,
                    reduce_quality: budget.reduce_quality,
                },
            )
            .await?;
        if let Some(path) = &self.settings {
            if let Err(error) = site_state::mark_published(path, &self.job.target.id) {
                eprintln!("Warning: Cannot record the publication locally: {error:#}");
            }
            // The site carries its own state; the local copy only helps when it is unreadable.
            // It is read back from staging because a provider may rewrite it (Cloudflare does
            // for optimized images). A site published without a state must not leave a copy
            // whose hashes describe files the provider never received.
            let served = published
                .state_published
                .then(|| {
                    std::fs::read(staging.join(site_state::STATE_PATH))
                        .map_err(anyhow::Error::from)
                        .and_then(|bytes| SiteState::parse(&bytes))
                })
                .transpose();
            let saved = match served {
                Ok(Some(served)) => {
                    self.record_suite_inputs(&records, &served, &suite_inputs);
                    site_state::save_local(path, &self.job.target.id, &served)
                }
                Ok(None) => site_state::forget_local(path, &self.job.target.id),
                Err(error) => {
                    eprintln!("Warning: Cannot read the published site state back: {error:#}");
                    site_state::forget_local(path, &self.job.target.id)
                }
            };
            if let Err(error) = saved {
                eprintln!(
                    "Warning: Cannot update the local copy of the published site state: {error:#}"
                );
            }
        }
        Ok(Outcome {
            url: published.url,
            fingerprint: Some(fingerprint),
            inputs,
            unchanged: false,
            suites: Some(suites),
        })
    }
}

/// The next plan after `error`, when it is Cloudflare's budget refusing a site with kept
/// suites. Each step goes further, so repeated refusals end with every suite local, where only
/// what a full deployment would refuse is reported:
/// 1. the kept suites `suites_to_optimize` picks for what is still missing also come from local
///    files, whose copies are checked afresh;
/// 2. once none is expected to help losslessly, the uploaded suites may be reduced in quality
///    (`BudgetRetry::reduce_quality`), no lower than `SPREAD_QUALITY` while suites stay kept,
///    together with kept suites (`suites_to_reduce`) for what reducing the uploaded ones alone
///    is not expected to save;
/// 3. then each refusal brings back more kept suites, those expected to save the most first and
///    as many as the previous spread's measured savings call for (`spread_count`), spreading
///    the reduction so every image stays closer to its original quality, as the optimizer does
///    over a whole site.
fn retry_with_kept_suites(mut prepared: Prepared, error: &anyhow::Error) -> Option<Prepared> {
    let refusal = error.downcast_ref::<super::KeptSuitesOverBudget>()?;
    if prepared.plan.kept.is_empty() {
        return None;
    }
    let chosen = if prepared.budget.reduce_quality {
        let count = spread_count(prepared.budget.last_spread, refusal.remaining);
        let chosen = suites_to_reduce(&prepared.plan.kept, refusal.remaining, count);
        prepared.budget.last_spread = Some(Spread {
            remaining: refusal.remaining,
            suites: chosen.len(),
        });
        chosen
    } else {
        let chosen = suites_to_optimize(&prepared.plan.kept, refusal.remaining);
        if chosen.is_empty() {
            prepared.budget.reduce_quality = true;
            // An attempt that could only squeeze the uploaded suites would be discarded anyway
            // when they are not expected to cover the shortfall alone. `remaining` already
            // follows their lossless recompression.
            let uploaded = super::expected_reduction_saving(refusal.staged_png);
            if uploaded >= refusal.remaining {
                return Some(prepared);
            }
            suites_to_reduce(&prepared.plan.kept, refusal.remaining - uploaded, 1)
        } else {
            chosen
        }
    };
    prepared.plan.upload_kept_suites(&chosen);
    prepared.stash = None;
    Some(prepared)
}

/// What regenerating `suite` adds back (its `source_bytes` beyond what the site serves), and
/// its original PNG bytes, assuming the savings were in its images.
fn regrowth_and_original_png(suite: &SuiteState) -> (u64, u64) {
    let sum = |png: bool| {
        suite
            .files
            .iter()
            .filter(|(path, _)| !png || path.ends_with(".png"))
            .fold(0_u64, |total, (_, file)| total.saturating_add(file.size))
    };
    let regrowth = suite
        .source_bytes
        .map_or(0, |source| source.saturating_sub(sum(false)));
    (regrowth, sum(true).saturating_add(regrowth))
}

/// What bringing back `suite` is expected to save once regenerated and recompressed
/// losslessly, and with `reduce` also reduced in quality, net of what regeneration adds back
/// to a suite a deployment optimized before; and its original PNG bytes.
fn expected_saving(suite: &SuiteState, reduce: bool) -> (u64, u64) {
    let (regrowth, original) = regrowth_and_original_png(suite);
    let mut saving = super::expected_lossless_saving(original);
    if reduce {
        saving = saving.saturating_add(super::expected_reduction_saving(original));
    }
    (saving.saturating_sub(regrowth), original)
}

/// The suites of `ranked`, best first with their expected savings, needed to save
/// `remaining`; all of those expected to save anything when together they are not enough.
fn enough_to_save<'a>(
    ranked: impl IntoIterator<Item = (u64, &'a String)>,
    remaining: u64,
) -> BTreeSet<String> {
    let mut saved = 0_u64;
    let mut chosen = BTreeSet::new();
    for (saving, id) in ranked {
        if saving == 0 || saved >= remaining {
            break;
        }
        saved = saved.saturating_add(saving);
        chosen.insert(id.clone());
    }
    chosen
}

/// The kept suites expected to save the most once regenerated and reduced in quality, net of
/// what regeneration adds back, enough for `remaining`; all of those when together they are not
/// enough. A suite reduced by an earlier deployment regains about what reducing it again saves,
/// so it comes last, and the reduction spreads to suites not reduced yet. When no suite is
/// expected to save anything, the largest one comes back alone, so retries always progress.
fn suites_to_reduce(kept: &[SuiteState], remaining: u64, at_least: usize) -> BTreeSet<String> {
    let mut suites = kept
        .iter()
        .map(|suite| {
            // Regenerated, the suite is recompressed and then reduced in quality.
            let (saving, original) = expected_saving(suite, true);
            (saving, original, &suite.summary.id)
        })
        .collect::<Vec<_>>();
    suites.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(b.2))
    });
    let mut chosen = enough_to_save(
        suites.iter().map(|(saving, _, id)| (*saving, *id)),
        remaining,
    );
    // Estimates miss what earlier deployments left unreduced, so measured savings may ask for
    // more suites; with none expected to save anything, the largest come first.
    for (_, _, id) in &suites {
        if chosen.len() >= at_least.max(1) {
            break;
        }
        chosen.insert((*id).clone());
    }
    chosen
}

/// How many kept suites the next spreading retry brings back: enough, at the rate the last
/// ones saved per suite, for `remaining` plus a quarter for suites saving less; twice as many
/// as last time when they saved nothing; one without a previous spread to measure.
fn spread_count(last_spread: Option<Spread>, remaining: u64) -> usize {
    let Some(Spread {
        remaining: before,
        suites: added,
    }) = last_spread
    else {
        return 1;
    };
    let gain = before.saturating_sub(remaining);
    if gain == 0 {
        return added.saturating_mul(2).max(1);
    }
    let per_suite = (gain / added.max(1) as u64).max(1);
    let needed = remaining.saturating_add(remaining / 4);
    usize::try_from(needed.div_ceil(per_suite))
        .unwrap_or(usize::MAX)
        .max(1)
}

/// The kept suites expected to save at least `remaining` bytes once regenerated and
/// recompressed losslessly, those saving the most first; all that are expected to save
/// anything when together they are not enough. A suite saves a share of its original PNG
/// bytes, less what regeneration adds back to one a deployment optimized before
/// (`source_bytes`), so those rarely qualify: bringing them back only adds bytes and work.
fn suites_to_optimize(kept: &[SuiteState], remaining: u64) -> BTreeSet<String> {
    let mut savings = kept
        .iter()
        .map(|suite| (expected_saving(suite, false).0, &suite.summary.id))
        .collect::<Vec<_>>();
    savings.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    enough_to_save(savings, remaining)
}

/// Adds the stashed copies of reused suites to a staged site. Generated suites win where both
/// list a file, such as a shared capture log chunk, because their copy is the current one; the
/// reused suite's state then records the staged bytes, so it describes what is uploaded.
fn add_local_copies(
    stash: &Path,
    local: &mut [SuiteState],
    destination: &Path,
    provider: DeploymentProvider,
) -> Result<()> {
    for suite in local {
        for (path, file) in &mut suite.files {
            let target = destination.join(path);
            if target.is_file() {
                let bytes = std::fs::read(&target)?;
                *file = StateFile {
                    size: bytes.len() as u64,
                    hash: provider_hash(provider, &bytes, path),
                };
                continue;
            }
            std::fs::create_dir_all(target.parent().context("Invalid staged path")?)?;
            std::fs::copy(stash.join(path), &target)
                .with_context(|| format!("Cannot stage the local copy of '{path}'"))?;
        }
    }
    Ok(())
}

/// Writes `source` to `relative` under the report `output` without following symlinks
/// (the same rule staging applies), replacing the file atomically. An existing identical file
/// is left alone unless `replace` asks for the newer copy anyway.
fn restore_file(output: &Path, source: &Path, relative: &str, replace: bool) -> Result<()> {
    let relative = Path::new(relative);
    ensure!(
        relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_))),
        "Report path escapes the report output"
    );
    let mut target = output.to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        target.push(part);
        let last = parts.peek().is_none();
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink() && (last || metadata.is_dir()),
                "Report output path '{}' is not a regular location",
                relative.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !last {
                    std::fs::create_dir(&target)?;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    let bytes = std::fs::read(source)
        .with_context(|| format!("Cannot read '{}' to restore", relative.display()))?;
    if !replace && std::fs::read(&target).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    crate::helpers::write_file_atomically(&target, &bytes).with_context(|| {
        format!(
            "Cannot restore '{}' in the local report",
            relative.display()
        )
    })
}

fn site_path(relative: &Path) -> String {
    relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The provider's identity for a staged file: what a later deployment sends to keep it.
fn provider_hash(provider: DeploymentProvider, bytes: &[u8], path: &str) -> String {
    use sha2::Digest;
    match provider {
        DeploymentProvider::Netlify => format!("{:x}", <sha1::Sha1 as sha1::Digest>::digest(bytes)),
        DeploymentProvider::CloudflareWorkers => super::cloudflare::asset_hash(bytes, path),
        DeploymentProvider::AzureSwa => format!("{:x}", sha2::Sha256::digest(bytes)),
    }
}

/// The state of the site being published: generated suites hashed from their staged files
/// (only those, so the work follows the changed suites), kept suites copied unchanged.
fn build_state(
    staging: &Path,
    inventory: &crate::snapshot::SiteInventory,
    kept: &[SuiteState],
    target: &DeployTarget,
    fingerprint: &str,
) -> Result<SiteState> {
    let summaries = crate::snapshot::suite_summaries(staging)?
        .into_iter()
        .map(|summary| (summary.id.clone(), summary))
        .collect::<BTreeMap<_, _>>();
    // Excluded files are never uploaded to Cloudflare, so no later deployment may keep them.
    let excludes = target
        .cloudflare
        .as_ref()
        .map(|cloudflare| {
            cloudflare
                .exclude
                .iter()
                .map(|pattern| glob::Pattern::new(pattern))
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let mut suites = BTreeMap::new();
    for (id, files) in &inventory.suites {
        let entries = files
            .par_iter()
            .map(|relative| -> Result<Option<(String, StateFile)>> {
                let path = site_path(relative);
                if super::cloudflare::excluded_path(&excludes, &path) {
                    return Ok(None);
                }
                let bytes = std::fs::read(staging.join(relative))
                    .with_context(|| format!("Cannot read staged report file '{path}'"))?;
                let hash = provider_hash(target.provider, &bytes, &path);
                Ok(Some((
                    path,
                    StateFile {
                        size: bytes.len() as u64,
                        hash,
                    },
                )))
            })
            .collect::<Result<Vec<_>>>()?;
        suites.insert(
            id.clone(),
            SuiteState {
                summary: summaries
                    .get(id)
                    .cloned()
                    .with_context(|| format!("Staged manifest has no suite '{id}'"))?,
                run_id: inventory
                    .runs
                    .get(id)
                    .cloned()
                    .with_context(|| format!("Staged suite '{id}' has no run"))?,
                files: entries.into_iter().flatten().collect(),
                source_bytes: None,
            },
        );
    }
    for suite in kept {
        suites.insert(suite.summary.id.clone(), suite.clone());
    }
    let state = SiteState {
        format: site_state::STATE_FORMAT,
        data_version: site_state::DATA_VERSION,
        generator: crate::snapshot::GENERATOR.into(),
        provider: target.provider,
        deployed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        fingerprint: fingerprint.into(),
        suites,
    };
    state.validate()?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap},
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::desktop::deployment::netlify::tests::{SITE_ID, client, target};

    /// Results for suites with one failed capture each, ready to generate.
    fn workspace(output: &Path, suites: &[(&str, u8)]) {
        for (id, shade) in suites {
            let suite = output.join(id);
            for directory in ["html_report", "bitmaps_reference", "bitmaps_test/run"] {
                std::fs::create_dir_all(suite.join(directory)).unwrap();
            }
            for (path, value) in [
                ("bitmaps_reference/home.png", *shade),
                ("bitmaps_test/run/home.png", shade.wrapping_add(1)),
            ] {
                std::fs::write(
                    suite.join(path),
                    lodepng::encode32(&[value, 40, 60, 255], 1, 1).unwrap(),
                )
                .unwrap();
            }
            let report = serde_json::json!({ "id": id, "tests": [{ "status": "fail", "pair": {
                "label": "Home", "fileName": "home.png",
                "reference": "../bitmaps_reference/home.png", "test": "../bitmaps_test/run/home.png"
            }}] });
            std::fs::write(
                suite.join("html_report/config.js"),
                format!("report({report});"),
            )
            .unwrap();
            std::fs::write(
                suite.join("bitmaps_test/run/report.json"),
                report.to_string(),
            )
            .unwrap();
        }
    }

    fn generate(output: &Path, suites: &[String]) {
        crate::snapshot::generate_site(
            output,
            suites,
            &crate::cli::args::SnapshotArgs::default(),
            &crate::config::WorkspaceConfig::default(),
        )
        .unwrap();
    }

    #[derive(Default)]
    struct Netlify {
        /// Content Netlify stores, by SHA-1.
        stored: HashMap<String, Vec<u8>>,
        /// The `files` map of each deploy.
        deploys: Vec<BTreeMap<String, String>>,
        uploads: Vec<String>,
    }

    impl Netlify {
        fn published(&self, path: &str) -> Vec<u8> {
            let digest = &self.deploys.last().unwrap()[path];
            self.stored[digest].clone()
        }
    }

    fn netlify() -> (crate::test::browser_tests::Site, Arc<Mutex<Netlify>>) {
        let state = Arc::new(Mutex::new(Netlify::default()));
        let shared = state.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let method = request.method().as_str().to_owned();
            let url = request.url().to_owned();
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body).unwrap();
            let mut netlify = shared.lock().unwrap();
            let response = if url == format!("/api/v1/sites/{SITE_ID}") {
                serde_json::json!({ "id": SITE_ID })
            } else if method == "POST" {
                let manifest: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let files: BTreeMap<String, String> =
                    serde_json::from_value(manifest["files"].clone()).unwrap();
                let required = files
                    .values()
                    .filter(|digest| !netlify.stored.contains_key(*digest))
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>();
                netlify.deploys.push(files);
                serde_json::json!({ "id": "deploy-1", "site_id": SITE_ID, "state": "prepared",
                    "required": required })
            } else if method == "PUT" {
                let digest = format!("{:x}", <sha1::Sha1 as sha1::Digest>::digest(&body));
                netlify.stored.insert(digest, body);
                netlify.uploads.push(url);
                serde_json::json!({})
            } else {
                serde_json::json!({ "id": "deploy-1", "site_id": SITE_ID, "state": "ready",
                    "ssl_url": "https://qa-reports.netlify.app/" })
            };
            let _ = request.respond(tiny_http::Response::from_string(response.to_string()));
        })
        .unwrap();
        (site, state)
    }

    fn deployment(
        output: &Path,
        mode: DeploymentMode,
        suites: &[&str],
        remove: &[&str],
    ) -> ReportDeployment {
        let ids = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        };
        ReportDeployment {
            job: DeploymentJob {
                target: target(),
                suite_ids: ids(suites),
                catalog_ids: ids(&["one", "two"]),
                partial_confirmed: true,
                force: false,
                mode,
                remove_ids: ids(remove),
            },
            root: output.to_path_buf(),
            output: output.to_path_buf(),
            settings: None,
            title: None,
            heading: None,
            subheading: None,
        }
    }

    #[test]
    fn known_sites_prefer_the_site_then_the_local_copy_and_refuse_blind_updates() -> Result<()> {
        let root = tempfile::tempdir()?;
        let settings = root.path().join("deployments").join("workspace.json");
        let mut context = deployment(root.path(), DeploymentMode::Update, &["one"], &[]);
        context.settings = Some(settings.clone());
        let published = site_state::tests::state(DeploymentProvider::Netlify, &["two"]);
        let label = |site: &KnownSite| match site {
            KnownSite::State(state) => format!("state:{}", state.suites.len()),
            KnownSite::Legacy(_) => "legacy".into(),
            KnownSite::Empty => "empty".into(),
        };

        // The site's own state wins and supplies the authoritative fingerprint.
        let (site, remote) = context.known_site(Some(RemoteState::Found(published.clone())))?;
        assert_eq!(
            (label(&site), remote),
            ("state:1".into(), Some(published.fingerprint.clone()))
        );
        let (site, remote) = context.known_site(Some(RemoteState::Legacy(Vec::new())))?;
        assert_eq!((label(&site), remote), ("legacy".into(), None));
        let (site, _) = context.known_site(Some(RemoteState::Empty))?;
        assert_eq!(label(&site), "empty");

        // An unreadable site cannot be updated blindly: keeping suites needs to know them.
        for remote in [
            RemoteState::Unreachable("offline".into()),
            RemoteState::Invalid("bad".into()),
        ] {
            assert!(context.known_site(Some(remote)).is_err());
        }
        let mut replace = context.clone();
        replace.job.mode = DeploymentMode::Replace;
        let (site, _) = replace.known_site(Some(RemoteState::Unreachable("offline".into())))?;
        assert_eq!(label(&site), "empty");

        // The local copy of the last published state is the fallback, without a fingerprint
        // that could vouch for the live site.
        site_state::save_local(&settings, "netlify", &published)?;
        let (site, remote) =
            context.known_site(Some(RemoteState::Unreachable("offline".into())))?;
        assert_eq!((label(&site), remote), ("state:1".into(), None));
        // Without a URL to read (never published, or private), the local copy decides too.
        let (site, _) = context.known_site(None)?;
        assert_eq!(label(&site), "state:1");
        site_state::forget_local(&settings, "netlify")?;
        let (site, _) = context.known_site(None)?;
        assert_eq!(label(&site), "empty");

        // Once this workspace has published there, an unknown site is not an empty one:
        // updating it blindly would drop every suite it serves.
        let target = target();
        super::super::DeployTargets {
            targets: vec![target.clone()],
            selected: Some(target.id.clone()),
            ..Default::default()
        }
        .save(&settings)?;
        super::super::DeployTargets::record_report_publication(
            &settings,
            &target,
            Some("https://qa-reports.netlify.app/"),
            &["two".into()],
        )?;
        let error = context.known_site(None).unwrap_err().to_string();
        assert!(error.contains("replace the whole site"), "{error}");
        let (site, _) = replace.known_site(None)?;
        assert_eq!(label(&site), "empty");

        // A site this workspace published to that now serves no report is not an empty site.
        let error = context
            .known_site(Some(RemoteState::Empty))
            .unwrap_err()
            .to_string();
        assert!(error.contains("serves no Regressify report"), "{error}");
        // Private Cloudflare publications leave only the marker, which still counts.
        let mut private = context.clone();
        private.job.target.id = "private".into();
        assert!(!private.published_before()?);
        site_state::mark_published(&settings, "private")?;
        assert!(private.published_before()?);
        assert!(private.known_site(None).is_err());
        site_state::forget_target(&settings, "private")?;
        assert!(!private.published_before()?);

        // An upload keeps the local copy: the suites it lists are on the site either way.
        site_state::save_local(&settings, "netlify", &published)?;
        assert!(
            super::super::decide_publication(
                &context.job,
                Some(&settings),
                &"c".repeat(64),
                None,
                None
            )?
            .is_none()
        );
        assert!(site_state::load_local(&settings, "netlify")?.is_some());
        // Removing the target forgets the copy with the rest of its records.
        let mut saved = super::super::DeployTargets::load(&settings)?;
        saved.remove_target(root.path(), &settings, "netlify")?;
        assert!(site_state::load_local(&settings, "netlify")?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn updates_keep_other_suites_by_digest_and_removals_drop_them() -> Result<()> {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        workspace(&output, &[("one", 10), ("two", 90)]);
        let (site, netlify) = netlify();
        let client = DeploymentClient::Netlify(client(&site.origin));
        let local = ["one".to_owned(), "two".to_owned()].into_iter().collect();
        let publish =
            |deployment: ReportDeployment, plan: Plan, generated: PathBuf, staging: &str| {
                let staging = root.path().join(staging);
                let client = &client;
                async move {
                    deployment
                        .publish(
                            Prepared {
                                plan,
                                ..Prepared::default()
                            },
                            &generated,
                            &staging,
                            client,
                        )
                        .await
                }
            };

        // A first deployment publishes both suites and their state.
        let first = deployment(&output, DeploymentMode::Replace, &["one", "two"], &[]);
        let plan = site_state::plan(
            DeploymentMode::Replace,
            DeploymentProvider::Netlify,
            &first.job.suite_ids,
            &[],
            &KnownSite::Empty,
            &local,
        )?;
        generate(&output, &plan.generate);
        let outcome = publish(first, plan, output.clone(), "first").await?;
        assert_eq!(
            outcome.suites.as_deref(),
            Some(&["one".to_owned(), "two".into()][..])
        );
        let published = SiteState::parse(
            &netlify
                .lock()
                .unwrap()
                .published("/_snapshot/deployment.json"),
        )?;
        assert_eq!(published.suites.keys().collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(published.fingerprint, outcome.fingerprint.clone().unwrap());
        let two_files = published.suites["two"].files.clone();
        // Recorded digests are exactly what Netlify received.
        for (path, file) in &two_files {
            assert_eq!(
                netlify.lock().unwrap().deploys[0][&format!("/{path}")],
                file.hash
            );
        }

        // Updating one suite keeps the other by digest without uploading or staging it.
        std::fs::write(
            output.join("one/bitmaps_test/run/home.png"),
            lodepng::encode32(&[200_u8, 40, 60, 255], 1, 1)?,
        )?;
        let update = deployment(&output, DeploymentMode::Update, &["one"], &[]);
        let plan = site_state::plan(
            DeploymentMode::Update,
            DeploymentProvider::Netlify,
            &update.job.suite_ids,
            &[],
            &KnownSite::State(published.clone()),
            &local,
        )?;
        assert_eq!(plan.generate, ["one".to_owned()]);
        generate(&output, &plan.generate);
        let uploads_before = netlify.lock().unwrap().uploads.len();
        let outcome = publish(update, plan, output.clone(), "update").await?;
        assert_eq!(
            outcome.suites.as_deref(),
            Some(&["one".to_owned(), "two".into()][..])
        );
        assert!(!root.path().join("update/_snapshot/images/two").exists());
        {
            let netlify = netlify.lock().unwrap();
            let files = netlify.deploys.last().unwrap();
            for (path, file) in &two_files {
                assert_eq!(files[&format!("/{path}")], file.hash);
            }
            assert!(
                netlify.uploads[uploads_before..]
                    .iter()
                    .all(|url| !url.contains("/two"))
            );
            let manifest: serde_json::Value = serde_json::from_slice(
                &netlify.published(
                    &files
                        .keys()
                        .find(|path| path.starts_with("/_snapshot/data/manifest.json"))
                        .unwrap()
                        .clone(),
                ),
            )?;
            assert_eq!(manifest["suites"].as_array().unwrap().len(), 2);
        }
        let updated = SiteState::parse(
            &netlify
                .lock()
                .unwrap()
                .published("/_snapshot/deployment.json"),
        )?;
        assert_eq!(updated.suites["two"], published.suites["two"]);
        assert_ne!(updated.suites["one"], published.suites["one"]);

        // Removing a suite generates nothing and drops its files from the site.
        let removal = deployment(&output, DeploymentMode::Update, &[], &["two"]);
        let plan = site_state::plan(
            DeploymentMode::Update,
            DeploymentProvider::Netlify,
            &[],
            &removal.job.remove_ids,
            &KnownSite::State(updated.clone()),
            &local,
        )?;
        assert!(plan.generate.is_empty());
        let scratch = root.path().join("scratch");
        std::fs::create_dir_all(&scratch)?;
        generate(&scratch, &[]);
        let outcome = publish(removal, plan, scratch, "removal").await?;
        assert_eq!(outcome.suites.as_deref(), Some(&["one".to_owned()][..]));
        let netlify = netlify.lock().unwrap();
        let files = netlify.deploys.last().unwrap();
        assert!(files.keys().all(|path| !path.contains("/two")), "{files:?}");
        for (path, file) in &updated.suites["one"].files {
            assert_eq!(files[&format!("/{path}")], file.hash);
        }
        let last = SiteState::parse(&netlify.published("/_snapshot/deployment.json"))?;
        assert_eq!(last.suites.keys().collect::<Vec<_>>(), ["one"]);
        // The local report kept its suites; removals only build shared files elsewhere.
        assert!(output.join("_snapshot/data/suites/one.json").exists());
        Ok(())
    }

    #[tokio::test]
    async fn selected_suites_with_unchanged_inputs_are_kept_instead_of_regenerated() -> Result<()> {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        workspace(&output, &[("one", 10), ("two", 90)]);
        let settings = root.path().join("deployments").join("workspace.json");
        let target = target();
        super::super::DeployTargets {
            targets: vec![target.clone()],
            selected: Some(target.id.clone()),
            ..Default::default()
        }
        .save(&settings)?;
        let (site, netlify) = netlify();
        let client = DeploymentClient::Netlify(client(&site.origin));
        let context = |mode, suites: &[&str], force| {
            let mut context = deployment(&output, mode, suites, &[]);
            context.settings = Some(settings.clone());
            context.job.force = force;
            context
        };
        let prepare = |context: ReportDeployment| async move {
            match context.prepare().await? {
                Preparation::Generate(prepared) => anyhow::Ok(*prepared),
                Preparation::Done(_) => bail!("unexpected early skip"),
            }
        };
        let kept_ids = |plan: &Plan| {
            plan.kept
                .iter()
                .map(|suite| suite.summary.id.clone())
                .collect::<Vec<_>>()
        };
        let mut staging = 0;
        let mut deploy = |context: ReportDeployment, prepared: Prepared| {
            staging += 1;
            let staging = root.path().join(format!("staging-{staging}"));
            generate(&output, &prepared.plan.generate);
            let (output, client) = (output.clone(), &client);
            async move { context.publish(prepared, &output, &staging, client).await }
        };
        let both = ["one", "two"];

        // The first deployment generates both suites and records their inputs.
        let first = context(DeploymentMode::Replace, &both, false);
        let prepared = prepare(first.clone()).await?;
        assert_eq!(prepared.plan.generate, ["one", "two"]);
        assert_eq!(
            prepared.suite_inputs.keys().collect::<Vec<_>>(),
            ["one", "two"]
        );
        deploy(first, prepared).await?;
        let records = site_state::load_inputs(&settings, &target.id);
        assert_eq!(records.suites.keys().collect::<Vec<_>>(), ["one", "two"]);
        let published = site_state::load_local(&settings, &target.id)?.unwrap();

        // A new run of one suite regenerates only that suite; the other stays by digest.
        std::fs::write(
            output.join("one/bitmaps_test/run/home.png"),
            lodepng::encode32(&[200_u8, 40, 60, 255], 1, 1)?,
        )?;
        let update = context(DeploymentMode::Update, &both, false);
        let prepared = prepare(update.clone()).await?;
        assert_eq!(prepared.plan.generate, ["one"]);
        assert_eq!(kept_ids(&prepared.plan), ["two"]);
        // Every selected suite left on the site matches its current inputs, kept ones included.
        assert_eq!(
            prepared.suite_inputs.keys().collect::<Vec<_>>(),
            ["one", "two"]
        );
        let uploads = netlify.lock().unwrap().uploads.len();
        let outcome = deploy(update, prepared).await?;
        assert_eq!(
            outcome.suites.as_deref(),
            Some(&["one".to_owned(), "two".into()][..])
        );
        assert!(
            netlify.lock().unwrap().uploads[uploads..]
                .iter()
                .all(|url| !url.contains("/two"))
        );
        let updated = site_state::load_local(&settings, &target.id)?.unwrap();
        assert_eq!(updated.suites["two"], published.suites["two"]);
        let after = site_state::load_inputs(&settings, &target.id);
        assert_ne!(after.suites["one"], records.suites["one"]);
        assert_eq!(after.suites["two"], records.suites["two"]);

        // An approval rewrites the baseline without a new run; its inputs still differ.
        std::fs::write(
            output.join("two/bitmaps_reference/home.png"),
            lodepng::encode32(&[91_u8, 40, 60, 255], 1, 1)?,
        )?;
        let approval = context(DeploymentMode::Update, &both, false);
        let prepared = prepare(approval.clone()).await?;
        assert_eq!(prepared.plan.generate, ["two"]);
        assert_eq!(kept_ids(&prepared.plan), ["one"]);
        deploy(approval, prepared).await?;

        // Deploy anyway regenerates every selected suite.
        let prepared = prepare(context(DeploymentMode::Update, &both, true)).await?;
        assert_eq!(prepared.plan.generate, ["one", "two"]);
        assert!(prepared.plan.kept.is_empty());

        // Replacing starts over: even an unchanged selection is rebuilt, and the rest removed.
        let prepared = prepare(context(DeploymentMode::Replace, &["one"], false)).await?;
        assert_eq!(prepared.plan.generate, ["one"]);
        assert!(prepared.plan.kept.is_empty());
        assert_eq!(prepared.plan.removed, ["two"]);

        // A suite the site no longer serves as recorded is regenerated, even with the same
        // inputs: another deployment may have republished it.
        let mut changed = site_state::load_local(&settings, &target.id)?.unwrap();
        changed.suites.get_mut("one").unwrap().run_id = "elsewhere".into();
        site_state::save_local(&settings, &target.id, &changed)?;
        let prepared = prepare(context(DeploymentMode::Update, &both, false)).await?;
        assert_eq!(prepared.plan.generate, ["one"]);
        assert_eq!(kept_ids(&prepared.plan), ["two"]);

        // Corrupt records only cost regeneration.
        std::fs::write(
            site_state::local_path(&settings, &target.id)?.with_extension("inputs.json"),
            b"{",
        )?;
        let prepared = prepare(context(DeploymentMode::Update, &both, false)).await?;
        assert_eq!(prepared.plan.generate, ["one", "two"]);
        Ok(())
    }

    #[tokio::test]
    async fn skipped_uploads_record_inputs_against_the_state_the_site_serves() -> Result<()> {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        workspace(&output, &[("one", 10), ("two", 90)]);
        let settings = root.path().join("deployments").join("workspace.json");
        let mut target = target();
        super::super::DeployTargets {
            targets: vec![target.clone()],
            selected: Some(target.id.clone()),
            ..Default::default()
        }
        .save(&settings)?;
        let (site, _netlify) = netlify();
        let client = DeploymentClient::Netlify(client(&site.origin));
        let mut context = deployment(&output, DeploymentMode::Replace, &["one", "two"], &[]);
        context.settings = Some(settings.clone());
        let plan = |context: &ReportDeployment| {
            site_state::plan(
                context.job.mode,
                DeploymentProvider::Netlify,
                &context.job.suite_ids,
                &[],
                &KnownSite::Empty,
                &BTreeSet::new(),
            )
        };
        let prepared = |plan, remote, served| Prepared {
            plan,
            remote,
            suite_inputs: BTreeMap::from([
                ("one".to_owned(), "in-one".to_owned()),
                ("two".to_owned(), "in-two".to_owned()),
            ]),
            served,
            ..Prepared::default()
        };
        let first_plan = plan(&context)?;
        generate(&output, &first_plan.generate);
        let first = context
            .publish(
                prepared(first_plan, None, None),
                &output,
                &root.path().join("first"),
                &client,
            )
            .await?;
        let published = site_state::load_local(&settings, &target.id)?.unwrap();

        // The site serves the same content under a state the provider rewrote (as Cloudflare
        // does for optimized images), so the staged state's digests are not what it serves.
        let mut served = published.clone();
        let one = served.suites.get_mut("one").unwrap();
        one.files.values_mut().next().unwrap().size += 1;
        target.published_url = Some("https://qa-reports.netlify.app/".into());
        let mut saved = super::super::DeployTargets::load(&settings)?;
        saved.targets = vec![target.clone()];
        saved.save(&settings)?;
        context.job.target = target.clone();
        let again = plan(&context)?;
        generate(&output, &again.generate);
        let outcome = context
            .publish(
                prepared(again, first.fingerprint.clone(), Some(served.clone())),
                &output,
                &root.path().join("again"),
                &client,
            )
            .await?;
        assert!(outcome.unchanged);
        let records = site_state::load_inputs(&settings, &target.id);
        for id in ["one", "two"] {
            assert_eq!(
                records.suites[id].state,
                site_state::suite_digest(&served.suites[id])?,
                "{id}"
            );
        }
        assert_ne!(
            records.suites["one"].state,
            site_state::suite_digest(&published.suites["one"])?
        );
        Ok(())
    }

    #[tokio::test]
    async fn whole_site_uploads_reuse_verified_local_copies_instead_of_regenerating() -> Result<()>
    {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        workspace(&output, &[("one", 10), ("two", 90)]);
        let (site, _netlify) = netlify();
        let client = DeploymentClient::Netlify(client(&site.origin));
        let context = deployment(&output, DeploymentMode::Update, &["one", "two"], &[]);
        let prepared = |plan| Prepared {
            plan,
            ..Prepared::default()
        };
        let both = ["one".to_owned(), "two".to_owned()];
        generate(&output, &both);
        let first = Plan {
            generate: both.to_vec(),
            ..Plan::default()
        };
        let staging = root.path().join("first");
        context
            .publish(prepared(first), &output, &staging, &client)
            .await?;
        let published = SiteState::parse(&std::fs::read(staging.join(site_state::STATE_PATH))?)?;
        let two = published.suites["two"].clone();

        // "one" changes; "two" is re-uploaded from the files the report output still has.
        std::fs::write(
            output.join("one/bitmaps_test/run/home.png"),
            lodepng::encode32(&[200_u8, 40, 60, 255], 1, 1)?,
        )?;
        let mut next = prepared(Plan {
            generate: vec!["one".to_owned()],
            local: vec![two.clone()],
            ..Plan::default()
        });
        context.stash_local(&mut next, &root.path().join("stash"))?;
        assert_eq!(next.plan.generate, ["one"]);
        assert_eq!(next.plan.local, std::slice::from_ref(&two));
        // Generating only "one" prunes "two" from the output; the stash still has it.
        generate(&output, &next.plan.generate);
        assert!(!output.join("_snapshot/data/suites/two.json").exists());
        let staging = root.path().join("next");
        let outcome = context.publish(next, &output, &staging, &client).await?;
        assert_eq!(outcome.suites.as_deref(), Some(&both[..]));
        let state = SiteState::parse(&std::fs::read(staging.join(site_state::STATE_PATH))?)?;
        assert_eq!(state.suites["two"], two);
        assert_ne!(state.suites["one"], published.suites["one"]);
        for (path, file) in &two.files {
            let bytes = std::fs::read(staging.join(path))?;
            assert_eq!(
                provider_hash(DeploymentProvider::Netlify, &bytes, path),
                file.hash
            );
        }
        // The output is a complete report of the published site again, ready for reuse.
        crate::snapshot::artifact_inventory(&output, &both, &[])?;
        assert_eq!(
            crate::snapshot::suite_summaries(&output)?
                .into_iter()
                .map(|summary| summary.id)
                .collect::<Vec<_>>(),
            both
        );

        // A local copy that differs from the site is never uploaded: the suite is rebuilt.
        let report = output.join("_snapshot/data/suites/two.json");
        std::fs::write(&report, b"{}")?;
        let mut stale = prepared(Plan {
            generate: vec!["one".to_owned()],
            local: vec![two],
            ..Plan::default()
        });
        context.stash_local(&mut stale, &root.path().join("stale"))?;
        assert_eq!(stale.plan.generate, both);
        assert!(stale.plan.local.is_empty());
        // Publishing reused suites requires the check to have run.
        let unchecked = prepared(Plan {
            generate: vec!["one".to_owned()],
            local: vec![published.suites["two"].clone()],
            ..Plan::default()
        });
        let error = context
            .publish(unchecked, &output, &root.path().join("unchecked"), &client)
            .await
            .err()
            .context("unchecked local copies must fail")?;
        assert!(error.to_string().contains("not checked"), "{error:#}");
        Ok(())
    }

    /// `id`'s published suite with `png` bytes in each PNG file, `other` in every other file,
    /// and `source_bytes`.
    fn sized_suite(
        published: &SiteState,
        id: &str,
        png: u64,
        other: u64,
        source_bytes: Option<u64>,
    ) -> SuiteState {
        let mut suite = published.suites[id].clone();
        for (path, file) in &mut suite.files {
            file.size = if path.ends_with(".png") { png } else { other };
        }
        suite.source_bytes = source_bytes;
        suite
    }

    fn budget_refusal(remaining: u64, staged_png: u64) -> anyhow::Error {
        super::super::KeptSuitesOverBudget {
            remaining,
            staged_png,
        }
        .into()
    }

    #[test]
    fn budget_retries_bring_back_the_kept_suites_expected_to_save_the_most() {
        let published = site_state::tests::state(
            DeploymentProvider::CloudflareWorkers,
            &["big", "mid", "small"],
        );
        // PNG bytes each suite serves, and its size before an earlier optimization.
        let kept = [
            ("big", 1000, Some(1250)),
            ("mid", 600, None),
            ("small", 100, None),
        ]
        .into_iter()
        .map(|(id, png, source_bytes)| sized_suite(&published, id, png, 50, source_bytes))
        .collect::<Vec<_>>();
        let chosen = |remaining| {
            suites_to_optimize(&kept, remaining)
                .into_iter()
                .collect::<Vec<_>>()
        };
        // "big" regains 200 bytes once regenerated, more than a tenth of its 1200 PNG bytes
        // saves, so only the unoptimized suites qualify: "mid" saves 60, "small" 10.
        assert_eq!(chosen(50), ["mid"]);
        assert_eq!(chosen(65), ["mid", "small"]);
        // Beyond what they are expected to save, all of them come back, never "big".
        assert_eq!(chosen(71), ["mid", "small"]);
        // Only optimized suites left: none is expected to help.
        assert!(suites_to_optimize(&kept[..1], 10).is_empty());
    }

    #[test]
    fn only_cloudflare_budget_refusals_with_kept_suites_retry_with_local_files() {
        let published =
            site_state::tests::state(DeploymentProvider::CloudflareWorkers, &["a", "b", "c"]);
        let prepared = |kept: Vec<SuiteState>| Prepared {
            plan: Plan {
                generate: vec!["d".to_owned()],
                kept,
                ..Plan::default()
            },
            stash: Some(PathBuf::from("stale-stash")),
            ..Prepared::default()
        };
        let refusal = |remaining| {
            budget_refusal(remaining, 0).context("Cannot prepare local Cloudflare deployment files")
        };
        let kept = published.suites.values().cloned().collect::<Vec<_>>();
        // The typed refusal survives the context deployments add on the way up. Each suite
        // serves 20 PNG bytes, expected to save 2: one covers 2 bytes.
        let retry = retry_with_kept_suites(prepared(kept.clone()), &refusal(2)).unwrap();
        assert_eq!(retry.plan.local, kept[..1]);
        assert_eq!(retry.plan.kept, kept[1..]);
        assert_eq!(retry.plan.generate, ["d"]);
        // The retry checks local copies afresh.
        assert!(retry.stash.is_none());
        // A refusal of that retry brings back more suites for what is still missing.
        let more = retry_with_kept_suites(retry, &refusal(2)).unwrap();
        assert_eq!(more.plan.local, kept[..2]);
        assert_eq!(more.plan.kept, kept[2..]);
        // Other failures are reported as they are.
        assert!(retry_with_kept_suites(more.clone(), &anyhow::anyhow!("offline")).is_none());
        // Beyond what the rest is expected to save, all of it comes back; then nothing is left
        // to bring back and the refusal is reported.
        let full = retry_with_kept_suites(more, &refusal(100)).unwrap();
        assert!(full.plan.kept.is_empty());
        assert_eq!(full.plan.local, kept);
        assert!(!full.budget.reduce_quality);
        assert!(retry_with_kept_suites(full, &refusal(1)).is_none());
    }

    #[test]
    fn budget_retries_reduce_quality_before_bringing_back_optimized_suites() {
        let published = site_state::tests::state(
            DeploymentProvider::CloudflareWorkers,
            &["optimized", "plain"],
        );
        let mut optimized = published.suites["optimized"].clone();
        // Regenerating it would add far more than recompressing it saves.
        optimized.source_bytes = Some(1000);
        let plain = published.suites["plain"].clone();
        let prepared = Prepared {
            plan: Plan {
                generate: vec!["new".to_owned()],
                kept: vec![optimized.clone(), plain.clone()],
                ..Plan::default()
            },
            ..Prepared::default()
        };
        // The uploaded suites alone are expected to cover the shortfall by quality reduction.
        let refusal = budget_refusal(50, 1000);
        // The suite expected to save something comes back first, although not enough.
        let first = retry_with_kept_suites(prepared, &refusal).unwrap();
        assert_eq!(first.plan.local, [plain]);
        assert_eq!(first.plan.kept, [optimized.clone()]);
        assert!(!first.budget.reduce_quality);
        // Then the uploaded suites may be reduced in quality, keeping the optimized suite.
        let reduced = retry_with_kept_suites(first, &refusal).unwrap();
        assert!(reduced.budget.reduce_quality);
        assert_eq!(reduced.plan.kept, [optimized]);
        // Then the reduction spreads to the optimized suite as well.
        let spread = retry_with_kept_suites(reduced, &refusal).unwrap();
        assert!(spread.plan.kept.is_empty());
        assert!(spread.budget.reduce_quality);
        // With every suite local, only a full deployment's own refusal remains.
        assert!(retry_with_kept_suites(spread, &refusal).is_none());
    }

    #[test]
    fn spreading_retries_size_their_batch_by_measured_savings() {
        let spread = |remaining, suites| Some(Spread { remaining, suites });
        // Without a previous spread, one suite; the measurement then sizes the next batch.
        assert_eq!(spread_count(None, 1000), 1);
        // One suite saved 20 of 264 bytes: 244 still missing, plus a quarter, at 20 each.
        assert_eq!(spread_count(spread(264, 1), 244), 16);
        // Three suites saved 30: 10 each.
        assert_eq!(spread_count(spread(100, 3), 70), 9);
        // Nothing saved: twice as many as last time.
        assert_eq!(spread_count(spread(100, 3), 100), 6);
        assert_eq!(spread_count(spread(100, 3), 120), 6);
        // A tiny shortfall still brings back one.
        assert_eq!(spread_count(spread(100, 1), 1), 1);

        let published =
            site_state::tests::state(DeploymentProvider::CloudflareWorkers, &["a", "b", "c", "d"]);
        // All reduced before: none is expected to save anything, so the largest come first.
        let kept = [("a", 400), ("b", 300), ("c", 200), ("d", 100)]
            .into_iter()
            .map(|(id, png)| sized_suite(&published, id, png, 0, Some(png * 2)))
            .collect::<Vec<_>>();
        let prepared = Prepared {
            plan: Plan {
                generate: vec!["new".to_owned()],
                kept: kept.clone(),
                ..Plan::default()
            },
            budget: BudgetRetry {
                reduce_quality: true,
                last_spread: None,
            },
            ..Prepared::default()
        };
        let first = retry_with_kept_suites(prepared, &budget_refusal(100, 0)).unwrap();
        assert_eq!(first.plan.local, kept[..1]);
        assert_eq!(first.budget.last_spread, spread(100, 1));
        // That suite saved 20, so the 80 still missing (plus a quarter) take the other three.
        let second = retry_with_kept_suites(first, &budget_refusal(80, 0)).unwrap();
        assert_eq!(second.plan.local, kept);
        assert_eq!(second.budget.last_spread, spread(80, 3));
    }

    #[test]
    fn suites_reduced_before_are_brought_back_last() {
        let published = site_state::tests::state(
            DeploymentProvider::CloudflareWorkers,
            &["reduced", "plain", "small"],
        );
        // PNG bytes each suite serves and its size before an earlier optimization: "reduced"
        // lost 40% to quality reduction, the others 10% losslessly.
        let suite =
            |id: &str, png: u64, source: u64| sized_suite(&published, id, png, 0, Some(source));
        let kept = vec![
            suite("reduced", 600, 1000),
            suite("plain", 450, 500),
            suite("small", 90, 100),
        ];
        let chosen = |kept: &[SuiteState], remaining| {
            suites_to_reduce(kept, remaining, 1)
                .into_iter()
                .collect::<Vec<_>>()
        };
        // "reduced" regains 400 bytes, more than reducing its 1000 again saves, so the largest
        // suite comes last: "plain" saves 100 net, "small" 20.
        assert_eq!(chosen(&kept, 50), ["plain"]);
        assert_eq!(chosen(&kept, 110), ["plain", "small"]);
        assert_eq!(chosen(&kept, 1000), ["plain", "small"]);
        // With only such suites left, the largest comes back alone.
        let reduced = vec![suite("reduced", 600, 1000), suite("small", 60, 100)];
        assert_eq!(chosen(&reduced, 1000), ["reduced"]);
    }

    #[test]
    fn quality_reductions_spread_over_the_largest_kept_suites_first() {
        let published = site_state::tests::state(
            DeploymentProvider::CloudflareWorkers,
            &["large", "medium", "small"],
        );
        // PNG bytes each suite serves, all optimized before (20% smaller than generated).
        let kept = [("large", 800), ("medium", 400), ("small", 80)]
            .into_iter()
            .map(|(id, png)| sized_suite(&published, id, png, 0, Some(png * 5 / 4)))
            .collect::<Vec<_>>();
        let chosen = |remaining| {
            suites_to_reduce(&kept, remaining, 1)
                .into_iter()
                .collect::<Vec<_>>()
        };
        // "large" regains 200 bytes and is expected to save 100 + 200 of its 1000 PNG bytes
        // (lossless, then quality): 100 net.
        assert_eq!(chosen(1), ["large"]);
        assert_eq!(chosen(100), ["large"]);
        // "medium" adds 50 net; beyond both, "small" joins too.
        assert_eq!(chosen(101), ["large", "medium"]);
        assert_eq!(chosen(1000), ["large", "medium", "small"]);
        // A retry in the reduction step brings back only what it needs.
        let prepared = Prepared {
            plan: Plan {
                generate: vec!["new".to_owned()],
                kept: kept.clone(),
                ..Plan::default()
            },
            budget: BudgetRetry {
                reduce_quality: true,
                last_spread: None,
            },
            ..Prepared::default()
        };
        let retry = retry_with_kept_suites(prepared.clone(), &budget_refusal(50, 0)).unwrap();
        assert_eq!(retry.plan.local, kept[..1]);
        assert_eq!(retry.plan.kept, kept[1..]);
        // Entering the reduction step, the largest suites come back at once for what the
        // uploaded suites are not expected to save themselves: 200 of 1000 PNG bytes here.
        let lossless = Prepared {
            budget: BudgetRetry::default(),
            ..prepared
        };
        let alone = retry_with_kept_suites(lossless.clone(), &budget_refusal(200, 1000)).unwrap();
        assert!(alone.budget.reduce_quality);
        assert!(alone.plan.local.is_empty());
        let spread = retry_with_kept_suites(lossless, &budget_refusal(250, 1000)).unwrap();
        assert!(spread.budget.reduce_quality);
        assert_eq!(spread.plan.local, kept[..1]);
    }

    #[test]
    fn repeated_budget_refusals_always_end_with_every_suite_local() {
        let ids = (0..12)
            .map(|index| format!("s{index:02}"))
            .collect::<Vec<_>>();
        let published = site_state::tests::state(
            DeploymentProvider::CloudflareWorkers,
            &ids.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let mut next = Prepared {
            plan: Plan {
                generate: vec!["new".to_owned()],
                kept: published.suites.values().cloned().collect(),
                ..Plan::default()
            },
            ..Prepared::default()
        };
        let refusal = budget_refusal(1, 0);
        let mut attempts = 0;
        while let Some(retry) = retry_with_kept_suites(next.clone(), &refusal) {
            // Every retry brings back at least one kept suite or allows quality reduction.
            assert!(
                retry.plan.kept.len() < next.plan.kept.len()
                    || retry.budget.reduce_quality && !next.budget.reduce_quality
            );
            next = retry;
            attempts += 1;
        }
        assert!(next.plan.kept.is_empty());
        assert_eq!(next.plan.local.len(), ids.len());
        assert!(attempts <= ids.len());
    }

    #[tokio::test]
    async fn reused_suites_return_to_the_local_report_whatever_the_outcome() -> Result<()> {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        workspace(&output, &[("one", 10), ("two", 90)]);
        let (site, _netlify) = netlify();
        let client = DeploymentClient::Netlify(client(&site.origin));
        let context = deployment(&output, DeploymentMode::Update, &["one", "two"], &[]);
        let prepared = |plan| Prepared {
            plan,
            ..Prepared::default()
        };
        let both = ["one".to_owned(), "two".to_owned()];
        generate(&output, &both);
        let first = root.path().join("first");
        context
            .publish(
                prepared(Plan {
                    generate: both.to_vec(),
                    ..Plan::default()
                }),
                &output,
                &first,
                &client,
            )
            .await?;
        let two =
            SiteState::parse(&std::fs::read(first.join(site_state::STATE_PATH))?)?.suites["two"]
                .clone();
        let reuse = || {
            prepared(Plan {
                generate: vec!["one".to_owned()],
                local: vec![two.clone()],
                ..Plan::default()
            })
        };
        let prune = || -> Result<()> {
            for path in two.files.keys() {
                std::fs::remove_file(output.join(path))?;
            }
            Ok(())
        };

        // Staging fails (nothing was generated for "one"): the stashed files still return.
        let mut failing = reuse();
        context.stash_local(&mut failing, &root.path().join("stash-1"))?;
        prune()?;
        let empty = root.path().join("empty");
        std::fs::create_dir_all(&empty)?;
        assert!(
            context
                .publish(failing, &empty, &root.path().join("staging-1"), &client)
                .await
                .is_err()
        );
        for path in two.files.keys() {
            assert!(output.join(path).is_file(), "{path}");
        }

        // The upload fails after staging: the staged site still becomes the local report.
        let mut unreachable = reuse();
        context.stash_local(&mut unreachable, &root.path().join("stash-2"))?;
        generate(&output, &unreachable.plan.generate);
        assert!(!output.join("_snapshot/data/suites/two.json").exists());
        let offline = DeploymentClient::Netlify(
            crate::desktop::deployment::netlify::tests::client("http://127.0.0.1:1"),
        );
        assert!(
            context
                .publish(
                    unreachable,
                    &output,
                    &root.path().join("staging-2"),
                    &offline
                )
                .await
                .is_err()
        );
        crate::snapshot::artifact_inventory(&output, &both, &[])?;

        // A failure before staging (generation, in the worker) returns the stashed files too.
        let mut generation = reuse();
        context.stash_local(&mut generation, &root.path().join("stash-3"))?;
        prune()?;
        context.restore_after_failure(&generation);
        for path in two.files.keys() {
            assert!(output.join(path).is_file(), "{path}");
        }

        // Every suite reused: only shared files are generated, in scratch space. The output's
        // stale or missing bundled frontend files are replaced along with its entry page.
        let first_state = SiteState::parse(&std::fs::read(first.join(site_state::STATE_PATH))?)?;
        let assets = std::fs::read_dir(output.join("_snapshot/assets"))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        let stale = assets.first().context("frontend assets")?.clone();
        let missing = assets.get(1).context("a second frontend asset")?.clone();
        std::fs::write(&stale, b"stale")?;
        std::fs::remove_file(&missing)?;
        let mut all_reused = prepared(Plan {
            local: first_state.suites.values().cloned().collect(),
            ..Plan::default()
        });
        context.stash_local(&mut all_reused, &root.path().join("stash-4"))?;
        assert_eq!(all_reused.plan.local.len(), 2);
        let scratch = root.path().join("scratch");
        std::fs::create_dir_all(&scratch)?;
        generate(&scratch, &[]);
        let offline = DeploymentClient::Netlify(
            crate::desktop::deployment::netlify::tests::client("http://127.0.0.1:1"),
        );
        assert!(
            context
                .publish(
                    all_reused,
                    &scratch,
                    &root.path().join("staging-4"),
                    &offline
                )
                .await
                .is_err()
        );
        crate::snapshot::artifact_inventory(&output, &both, &[])?;
        for asset in [&stale, &missing] {
            let relative = asset.strip_prefix(&output)?;
            assert_eq!(
                std::fs::read(asset)?,
                std::fs::read(scratch.join(relative))?,
                "{}",
                relative.display()
            );
        }
        Ok(())
    }

    #[test]
    fn restored_report_files_never_follow_symlinks_out_of_the_output() -> Result<()> {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(output.join("_snapshot/images"))?;
        std::fs::create_dir_all(&outside)?;
        let source = root.path().join("source.png");
        std::fs::write(&source, b"image")?;
        restore_file(&output, &source, "_snapshot/images/one/home.png", false)?;
        assert_eq!(
            std::fs::read(output.join("_snapshot/images/one/home.png"))?,
            b"image"
        );
        assert!(restore_file(&output, &source, "_snapshot/../escape.png", false).is_err());
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, output.join("_snapshot/images/two"));
        // Creating symlinks on Windows needs Developer Mode or elevation.
        #[cfg(windows)]
        let linked =
            std::os::windows::fs::symlink_dir(&outside, output.join("_snapshot/images/two"));
        if linked.is_ok() {
            assert!(
                restore_file(&output, &source, "_snapshot/images/two/home.png", false).is_err()
            );
            assert!(!outside.join("home.png").exists());
        }
        Ok(())
    }
}
