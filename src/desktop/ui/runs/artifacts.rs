use super::*;
use crate::desktop::deployment::{DeployTarget, DeployTargets};
use crate::desktop::results::{SavedRun, SuiteArtifacts};

#[derive(Default)]
pub(super) struct ArtifactScan {
    root: Option<PathBuf>,
    generation: u64,
    pub(super) loading: bool,
    suites: BTreeMap<String, std::result::Result<SuiteArtifacts, String>>,
    publications: DeployTargets,
}

impl ArtifactScan {
    pub(super) fn latest_run_id(&self, id: &str) -> Option<String> {
        Some(self.suites.get(id)?.as_ref().ok()?.latest()?.run_id.clone())
    }

    pub(super) fn latest_run_ids(&self) -> BTreeMap<String, String> {
        self.suites
            .keys()
            .filter_map(|id| Some((id.clone(), self.latest_run_id(id)?)))
            .collect()
    }

    pub(super) fn clear(&mut self) {
        self.generation += 1;
        self.root = None;
        self.loading = false;
        self.suites.clear();
        self.publications = DeployTargets::default();
    }
}

impl RunPanel {
    pub(in crate::desktop::ui) fn set_deployment_storage_path(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.manager.set_deployment_storage_path(path.clone());
        self.deployment_path = Some(path);
        self.artifacts.publications = DeployTargets::default();
        self.refresh_artifacts(cx);
    }

    pub(super) fn suite_site_url(&self, name: &str) -> Option<String> {
        let suite = self.suites.iter().find(|suite| suite.name == name)?;
        self.artifacts
            .publications
            .latest_report_url(std::slice::from_ref(&suite.output_id))
    }

    pub(super) fn selected_site_url(&self) -> Option<String> {
        let ids = if self.selection.is_empty() {
            self.executed_suites
                .iter()
                .map(|(_, id)| id.clone())
                .collect::<Vec<_>>()
        } else {
            self.suites
                .iter()
                .filter(|suite| self.selection.contains(&suite.name))
                .map(|suite| suite.output_id.clone())
                .collect()
        };
        self.artifacts.publications.latest_report_url(&ids)
    }

    pub(super) fn open_site(&mut self, url: &str, cx: &mut Context<Self>) {
        if self.disabled || self.running() {
            return;
        }
        if let Err(error) = open::that_detached(url) {
            self.status = format!("Cannot open deployed site: {error}");
            cx.notify();
        }
    }

    pub(in crate::desktop::ui) fn suite_counts(&self, id: &str) -> crate::progress::Counts {
        let Ok(root) = self.output_root() else {
            return crate::progress::Counts::default();
        };
        self.manager
            .suite_counts(&root, id)
            .or_else(|| {
                if self.artifacts.root.as_ref() != Some(&root) {
                    return None;
                }
                self.artifacts
                    .suites
                    .get(id)?
                    .as_ref()
                    .ok()?
                    .latest()?
                    .counts
            })
            .unwrap_or_default()
    }

    pub(in crate::desktop::ui) fn refresh_artifacts_if_idle(&mut self, cx: &mut Context<Self>) {
        if !self.running() && !self.artifacts.loading {
            self.refresh_artifacts(cx);
        }
    }

    pub(in crate::desktop::ui) fn refresh_artifacts(&mut self, cx: &mut Context<Self>) {
        let Ok(root) = self.output_root() else {
            self.artifacts.clear();
            cx.emit(RunEvent::ArtifactsChanged);
            cx.notify();
            return;
        };
        self.artifacts.generation += 1;
        if self.artifacts.root.as_ref() != Some(&root) {
            self.artifacts.suites.clear();
        }
        self.artifacts.root = Some(root.clone());
        self.artifacts.loading = true;
        let generation = self.artifacts.generation;
        let deployment_path = self.deployment_path.clone();
        let ids = self
            .suites
            .iter()
            .filter(|suite| suite.error.is_none())
            .map(|suite| suite.output_id.clone())
            .collect::<BTreeSet<_>>();
        let task = cx.background_executor().spawn(async move {
            let artifacts = ids
                .into_iter()
                .map(|id| {
                    let artifacts =
                        SuiteArtifacts::load(&root, &id).map_err(|error| format!("{error:#}"));
                    (id, artifacts)
                })
                .collect::<BTreeMap<_, _>>();
            let publications = deployment_path
                .as_deref()
                .map(DeployTargets::load)
                .transpose()
                .map(Option::unwrap_or_default);
            (artifacts, publications)
        });
        cx.spawn(async move |panel, cx| {
            let (artifacts, publications) = task.await;
            let _ = panel.update(cx, |panel, cx| {
                if panel.artifacts.generation != generation {
                    return;
                }
                panel.artifacts.suites = artifacts;
                panel.artifacts.loading = false;
                match publications {
                    Ok(publications) => {
                        let saved = followed_target(panel.site_target(), &publications);
                        let reload = std::mem::take(&mut panel.site.reload_pending);
                        if reload || saved.as_ref() != panel.site_target() {
                            panel.refresh_site(saved, reload, cx);
                        } else {
                            // Local results may have changed without a new run (an approval).
                            panel.verify_site(cx);
                        }
                        panel.artifacts.publications = publications;
                    }
                    Err(error) => {
                        panel.artifacts.publications = DeployTargets::default();
                        panel.status = format!("Cannot load deployed sites: {error:#}");
                    }
                }
                cx.emit(RunEvent::ArtifactsChanged);
                cx.notify();
            });
        })
        .detach();
        cx.emit(RunEvent::ArtifactsChanged);
        cx.notify();
    }

    fn suite_artifacts(&self, name: &str) -> Result<&SuiteArtifacts> {
        anyhow::ensure!(
            self.artifacts.root.as_ref() == Some(&self.output_root()?),
            "Checking saved captures..."
        );
        let suite = self
            .suites
            .iter()
            .find(|suite| suite.name == name)
            .ok_or_else(|| anyhow!("Selected suite is unavailable"))?;
        anyhow::ensure!(
            suite.error.is_none(),
            "Suite '{name}' has configuration errors"
        );
        self.artifacts
            .suites
            .get(&suite.output_id)
            .ok_or_else(|| anyhow!("Checking saved captures..."))?
            .as_ref()
            .map_err(|error| anyhow!("{error}"))
    }

    pub(in crate::desktop::ui) fn has_reference(&self, name: &str) -> bool {
        self.suite_artifacts(name)
            .is_ok_and(|artifacts| artifacts.reference_count > 0)
    }

    pub(in crate::desktop::ui) fn artifact_summary(&self, name: &str) -> String {
        match self.suite_artifacts(name) {
            Ok(artifacts) => format!(
                "{} reference {} / {} saved test {}",
                artifacts.reference_count,
                if artifacts.reference_count == 1 {
                    "image"
                } else {
                    "images"
                },
                artifacts.runs.len(),
                if artifacts.runs.len() == 1 {
                    "run"
                } else {
                    "runs"
                }
            ),
            Err(error) => error.to_string(),
        }
    }

    fn saved_run(&self, name: &str) -> Result<&SavedRun> {
        let artifacts = self.suite_artifacts(name)?;
        anyhow::ensure!(
            artifacts.reference_count > 0,
            "No reference images for '{name}'"
        );
        let requested = self.options["runId"].as_str().unwrap_or_default();
        artifacts
            .runs
            .iter()
            .find(|run| {
                if requested.is_empty() {
                    run.default_candidate
                } else {
                    run.run_id == requested
                }
            })
            .ok_or_else(|| anyhow!("No matching saved test run for '{name}'"))
    }

    pub(in crate::desktop::ui) fn can_open_saved_suite(&self, name: &str) -> bool {
        !self.running() && self.saved_run(name).is_ok()
    }

    pub(in crate::desktop::ui) fn can_open_saved_selection(&self) -> bool {
        if self.running() {
            return false;
        }
        let names = self.selected_names();
        if names.is_empty() {
            return false;
        }
        let runs = names
            .iter()
            .map(|name| self.saved_run(name))
            .collect::<Result<Vec<_>>>();
        runs.is_ok_and(|runs| {
            runs.iter().all(|run| run.has_report)
                || runs.iter().all(|run| run.run_id == runs[0].run_id)
        })
    }

    pub(in crate::desktop::ui) fn open_saved_reports(
        &mut self,
        names: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.disabled || self.running() {
            return;
        }
        let result = (|| -> Result<()> {
            self.flush_options(RunCommand::Init, cx)?;
            anyhow::ensure!(!names.is_empty(), "Select a suite to open its report");
            let root = self.output_root()?;
            let mut targets = Vec::new();
            let mut has_reports = true;
            for name in &names {
                let run = self.saved_run(name)?;
                let suite = self
                    .suites
                    .iter()
                    .find(|suite| &suite.name == name)
                    .ok_or_else(|| anyhow!("Selected suite is unavailable"))?;
                has_reports &= run.has_report;
                targets.push((suite.output_id.clone(), run.run_id.clone()));
            }
            self.confirmation = false;
            self.pending_request = None;
            if has_reports {
                cx.emit(RunEvent::ReviewSavedResults { root, targets });
            } else {
                anyhow::ensure!(
                    targets.iter().all(|(_, run)| run == &targets[0].1),
                    "These suites have different capture runs. Open each suite's report to compare its saved captures."
                );
                self.flush_options(RunCommand::Compare, cx)?;
                let mut options: RunOptions = serde_json::from_value(self.options.clone())?;
                options.regressify_dir = display_path(&root);
                options.run_id = targets[0].1.clone();
                options.scenarios.clear();
                let intent = RunIntent {
                    request: RunRequest {
                        root: self
                            .root
                            .clone()
                            .ok_or_else(|| anyhow!("Open a workspace first"))?,
                        command: RunCommand::Compare,
                        options,
                    },
                    suite_names: names,
                    expected_output_ids: Some(targets.iter().map(|(id, _)| id.clone()).collect()),
                    review_after_run: true,
                };
                intent.resolve(&self.suites)?;
                cx.emit(RunEvent::Requested(Box::new(intent)));
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.status = format!("{error:#}");
        }
        cx.notify();
    }

    pub(super) fn require_references(&self) -> Result<()> {
        for suite in self.selection.selected(&self.suites)? {
            let artifacts = self.suite_artifacts(&suite.name)?;
            anyhow::ensure!(
                artifacts.reference_count > 0,
                "No reference images for '{}'. Run Reference before Run Test.",
                suite.name
            );
        }
        Ok(())
    }

    pub(in crate::desktop::ui) fn can_test_selection(&self) -> bool {
        self.can_request(RunCommand::Test)
    }

    pub(super) fn render_artifact_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let summary = if self.selection.is_empty() {
            "Select a suite".into()
        } else {
            self.selected_names()
                .iter()
                .map(|name| format!("{name}: {}", self.artifact_summary(name)))
                .collect::<Vec<_>>()
                .join("; ")
        };
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .id("artifact-status")
                    .role(Role::Status)
                    .aria_label(summary.clone())
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary),
            )
            .child(
                Button::new("refresh-run-artifacts")
                    .ghost()
                    .icon(gpui_kit::assets::IconName::RefreshCw)
                    .tooltip("Refresh references and saved test runs")
                    .accessibility_label("Refresh saved captures")
                    .disabled(self.disabled || self.running() || self.root.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_artifacts(cx))),
            )
            .into_any_element()
    }
}

/// The target the site scan should show, refreshed from its saved settings: the one it follows
/// (the Deploy dialog may have chosen it) while the loaded settings still hold it, else the saved
/// selection. A target from another workspace, or one since deleted, falls back to the selection.
pub(super) fn followed_target(
    followed: Option<&DeployTarget>,
    publications: &DeployTargets,
) -> Option<DeployTarget> {
    let find = |id: &str| publications.targets.iter().find(|target| target.id == id);
    followed
        .and_then(|target| find(&target.id))
        .or_else(|| publications.selected.as_deref().and_then(find))
        .cloned()
}
