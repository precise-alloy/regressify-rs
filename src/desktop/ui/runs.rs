//! Run state is independent of document fields so log updates cannot disturb drafts.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use super::*;
use crate::desktop::runner::{
    RunCommand, RunManager, RunOptions, RunOutcome, RunRequest, RunSnapshot,
};
use crate::desktop::workspace::{SuiteSelection, SuiteSummary};

mod activity;
mod artifacts;
mod log;
mod settings;
mod site;
pub(super) use site::SiteSource;

const RUN_UI_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

fn selection_range<'item>(
    items: &'item [String],
    clicked: &str,
    anchor: Option<&str>,
    extend: bool,
) -> Option<&'item [String]> {
    let end = items.iter().position(|item| item == clicked)?;
    let start = anchor
        .filter(|_| extend)
        .and_then(|anchor| items.iter().position(|item| item == anchor))
        .unwrap_or(end);
    Some(&items[start.min(end)..=start.max(end)])
}

#[derive(Clone, Debug)]
pub(super) enum RunEvent {
    Requested(Box<RunIntent>),
    Finished,
    SelectionChanged,
    ArtifactsChanged,
    ProgressChanged,
    ReviewResults {
        root: PathBuf,
        run_id: String,
    },
    ReviewSavedResults {
        root: PathBuf,
        targets: Vec<(String, String)>,
    },
}

#[derive(Clone, Debug)]
pub(super) struct RunIntent {
    pub request: RunRequest,
    pub suite_names: Vec<String>,
    pub expected_output_ids: Option<Vec<String>>,
    pub review_after_run: bool,
}

impl RunIntent {
    pub(super) fn resolve_for_execution(&self, suites: &[SuiteSummary]) -> Result<RunRequest> {
        let request = self.resolve(suites)?;
        if request.command == RunCommand::Test {
            let output = request.options.output_directory(&request.root);
            for name in &self.suite_names {
                let suite = suites
                    .iter()
                    .find(|suite| &suite.name == name)
                    .ok_or_else(|| anyhow!("Selected suite '{name}' is unavailable"))?;
                let artifacts =
                    crate::desktop::results::SuiteArtifacts::load(&output, &suite.output_id)?;
                anyhow::ensure!(
                    artifacts.reference_count > 0,
                    "No reference images for '{name}'. Run Reference before Run Test."
                );
            }
        }
        Ok(request)
    }

    pub(super) fn compare_run(
        mut request: RunRequest,
        executed_suites: &[(String, String)],
        review_after_run: bool,
    ) -> Result<Self> {
        if matches!(request.command, RunCommand::Compare | RunCommand::Approve) {
            for filter in &mut request.options.scenarios {
                let (name, _) = executed_suites
                    .iter()
                    .find(|(_, id)| id == &filter.suite)
                    .ok_or_else(|| {
                        anyhow!(
                            "The recorded scenario's suite is unavailable; review the run selection"
                        )
                    })?;
                filter.suite = name.clone();
            }
        }
        request.command = RunCommand::Compare;
        request.options.report_capture = None;
        Ok(Self {
            request,
            suite_names: executed_suites
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
            expected_output_ids: Some(executed_suites.iter().map(|(_, id)| id.clone()).collect()),
            review_after_run,
        })
    }

    pub(super) fn resolve(&self, suites: &[SuiteSummary]) -> Result<RunRequest> {
        let mut request = self.request.clone();
        if request.command == RunCommand::Deploy {
            let deployment = request
                .options
                .deployment
                .as_ref()
                .ok_or_else(|| anyhow!("Choose a deployment target"))?;
            let current = suites
                .iter()
                .map(|suite| &suite.output_id)
                .collect::<BTreeSet<_>>();
            let confirmed = deployment.catalog_ids.iter().collect::<BTreeSet<_>>();
            anyhow::ensure!(
                current == confirmed,
                "Workspace suites changed; review deployment selection again"
            );
        }
        if !request.command.supports_scenarios() {
            request.options.scenarios.clear();
        }
        let removal_only = request.command == RunCommand::Deploy
            && self.suite_names.is_empty()
            && request
                .options
                .deployment
                .as_ref()
                .is_some_and(|job| !job.remove_ids.is_empty());
        if removal_only {
            // Removing suites from a site generates none, so it has no suite filters.
            request.options.suites.clear();
        } else if request.command.supports_suites() {
            let mut selection = SuiteSelection::default();
            for name in &self.suite_names {
                let suite = suites
                    .iter()
                    .find(|suite| suite.name == *name)
                    .ok_or_else(|| anyhow!("Selected suite '{name}' no longer exists"))?;
                selection.toggle(suite, true)?;
            }
            request.options.suites =
                selection.filters(suites, request.command.uses_output_suite_ids())?;
            for filter in &mut request.options.scenarios {
                anyhow::ensure!(
                    self.suite_names.contains(&filter.suite),
                    "Selected scenario '{filter}' is outside the requested suites"
                );
                let suite = suites
                    .iter()
                    .find(|suite| suite.name == filter.suite)
                    .ok_or_else(|| anyhow!("Selected suite '{}' no longer exists", filter.suite))?;
                anyhow::ensure!(
                    suite
                        .scenarios
                        .iter()
                        .filter(|scenario| scenario.id == filter.scenario)
                        .count()
                        == 1,
                    "Selected scenario '{filter}' changed or no longer exists; review the selection before running"
                );
                if matches!(request.command, RunCommand::Compare | RunCommand::Approve) {
                    filter.suite = suite.output_id.clone();
                }
            }
            if let Some(expected) = &self.expected_output_ids {
                anyhow::ensure!(
                    request.options.suites
                        == expected
                            .iter()
                            .map(|id| glob::Pattern::escape(id))
                            .collect::<Vec<_>>(),
                    "The report's suite output changed; refresh results before approving"
                );
            }
        }
        request.arguments()?;
        Ok(request)
    }
}

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
struct RequestCommand {
    index: usize,
}

pub(super) struct RunPanel {
    root: Option<PathBuf>,
    command: RunCommand,
    options: Value,
    bindings: Vec<Binding>,
    errors: BTreeMap<String, String>,
    subscriptions: Vec<Subscription>,
    settings_search: Entity<InputState>,
    _settings_subscription: Subscription,
    pub(super) manager: RunManager,
    log: Entity<EditorState>,
    focus: FocusHandle,
    selection_focus: FocusHandle,
    status: String,
    tab: usize,
    disabled: bool,
    confirmation: bool,
    pending_request: Option<RunIntent>,
    log_text: String,
    log_scope: Option<log::ScenarioLogScope>,
    archive_page: Option<crate::desktop::logs::LogPage>,
    archive_key: Option<String>,
    archive_loading: bool,
    archive_generation: u64,
    log_query: crate::desktop::logs::LogQuery,
    log_controls: Entity<super::log_controls::LogControls>,
    _log_controls_subscription: Subscription,
    log_follow: bool,
    suites: Vec<SuiteSummary>,
    selection: SuiteSelection,
    suite_selection_anchor: Option<String>,
    scenario_selection_anchor: Option<(String, String)>,
    catalog_error: Option<String>,
    collapsed: BTreeSet<String>,
    expanded_selections: BTreeSet<String>,
    failed_only: bool,
    started: Option<Instant>,
    elapsed_seconds: u64,
    last_run_refresh: Option<Instant>,
    run_refresh_pending: bool,
    executed_suites: Vec<(String, String)>,
    artifacts: artifacts::ArtifactScan,
    deployment_path: Option<PathBuf>,
    site: site::SiteScan,
}

impl EventEmitter<RunEvent> for RunPanel {}

impl RunPanel {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find a run setting"));
        let settings_subscription =
            cx.subscribe(&settings_search, |_, _, _: &InputEvent, cx| cx.notify());
        let log_controls = cx.new(|cx| super::log_controls::LogControls::new(window, cx));
        let log_subscription = cx.subscribe_in(
            &log_controls,
            window,
            |this: &mut Self, _, event, window, cx| {
                use super::log_controls::LogControlEvent;
                match event {
                    LogControlEvent::Filter(query) => {
                        this.log_query = query.clone();
                        this.archive_key = None;
                        this.refresh_log(window, cx);
                    }
                    LogControlEvent::Follow(follow) => {
                        this.set_log_follow(*follow, window, cx);
                    }
                    LogControlEvent::Copy => {
                        cx.write_to_clipboard(ClipboardItem::new_string(this.log_text.clone()))
                    }
                    LogControlEvent::Export(path) => {
                        if let (Some(request), Some(page)) =
                            (&this.manager.request, &this.archive_page)
                        {
                            let (output, sources, query, path) = (
                                request.options.output_directory(&request.root),
                                page.sources.clone(),
                                this.log_query.clone(),
                                path.clone(),
                            );
                            this.log_controls.update(cx, |controls, cx| {
                                controls.export(output, sources, query, path, cx)
                            });
                        }
                    }
                }
                cx.notify();
            },
        );
        let mut panel = Self {
            root: None,
            command: RunCommand::Test,
            options: serde_json::to_value(RunOptions::default()).unwrap_or(Value::Null),
            bindings: Vec::new(),
            errors: BTreeMap::new(),
            subscriptions: Vec::new(),
            settings_search,
            _settings_subscription: settings_subscription,
            manager: RunManager::default(),
            log: cx.new(|cx| EditorState::new(window, cx).line_number(false)),
            focus: cx.focus_handle(),
            selection_focus: cx.focus_handle().tab_index(0),
            status: "No command running".into(),
            tab: 0,
            disabled: false,
            confirmation: false,
            pending_request: None,
            log_text: String::new(),
            log_scope: None,
            archive_page: None,
            archive_key: None,
            archive_loading: false,
            archive_generation: 0,
            log_query: Default::default(),
            log_controls,
            _log_controls_subscription: log_subscription,
            log_follow: true,
            suites: Vec::new(),
            selection: SuiteSelection::default(),
            suite_selection_anchor: None,
            scenario_selection_anchor: None,
            catalog_error: None,
            collapsed: BTreeSet::new(),
            expanded_selections: BTreeSet::new(),
            failed_only: false,
            started: None,
            elapsed_seconds: 0,
            last_run_refresh: None,
            run_refresh_pending: false,
            executed_suites: Vec::new(),
            artifacts: artifacts::ArtifactScan::default(),
            site: site::SiteScan::default(),
            deployment_path: None,
        };
        panel.rebuild(window, cx);
        panel
    }

    pub(super) fn running(&self) -> bool {
        self.manager.running()
    }
    pub(super) fn suite_changed(
        &mut self,
        old: &SuiteSummary,
        new: Option<&SuiteSummary>,
        cx: &mut Context<Self>,
    ) {
        self.selection
            .rename_suite(&old.name, new.map(|suite| suite.name.as_str()));
        self.suites
            .retain(|suite| suite.entry.path != old.entry.path);
        if let Some(suite) = new {
            self.suites.push(suite.clone());
            self.suites.sort_by_key(|suite| suite.entry.label());
        }
        self.suite_selection_anchor = None;
        self.scenario_selection_anchor = None;
        self.executed_suites.retain_mut(|(name, id)| {
            if *id != old.output_id {
                return true;
            }
            if let Some(suite) = new {
                *name = suite.name.clone();
                *id = suite.output_id.clone();
                true
            } else {
                false
            }
        });
        if self.expanded_selections.remove(&old.name)
            && let Some(suite) = new
        {
            self.expanded_selections.insert(suite.name.clone());
        }
        self.confirmation = false;
        self.pending_request = None;
        if let Err(error) = self.manager.remap_suite(
            &old.name,
            &old.output_id,
            new.map(|suite| (suite.output_id.as_str(), suite.name.as_str())),
        ) {
            self.manager.persistence_error = Some(format!(
                "Suite changed, but activity could not be updated: {error:#}"
            ));
        }
        if self.manager.running() {
            cx.spawn(async move |view, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    let running = view
                        .update(cx, |this, cx| {
                            if this.manager.poll() {
                                this.refresh_artifacts(cx);
                                cx.emit(RunEvent::ProgressChanged);
                                cx.notify();
                            }
                            this.running()
                        })
                        .unwrap_or(false);
                    if !running {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }
    pub(super) fn progress(&self) -> &crate::progress::LiveProgress {
        &self.manager.progress
    }
    fn can_open_report(&self) -> bool {
        self.can_open_saved_selection() || self.can_open_executed_report()
    }
    fn can_open_executed_report(&self) -> bool {
        (self.selection.is_empty()
            || self.selected_names()
                == self
                    .executed_suites
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>())
            && self.executed_report_available()
    }
    fn executed_report_available(&self) -> bool {
        !self.running()
            && self.manager.request.as_ref().is_some_and(|request| {
                request.command.supports_suites()
                    && (request.options.suites.is_empty()
                        || !self.executed_suites.is_empty()
                        || !self.manager.progress.suites.is_empty())
            })
            && (matches!(self.manager.outcome, Some(RunOutcome::Completed))
                || matches!(self.manager.outcome, Some(RunOutcome::Failed(_)))
                    && self.manager.progress.comparison_counts(None).total > 0)
    }
    fn open_report(&mut self, cx: &mut Context<Self>) {
        if self.disabled || !self.can_open_report() {
            return;
        }
        if self.can_open_saved_selection() {
            self.open_saved_reports(self.selected_names(), cx);
            return;
        }
        self.confirmation = false;
        self.pending_request = None;
        if self.manager.request.as_ref().is_some_and(|request| {
            request.command == RunCommand::Test && request.options.skip_compare
        }) {
            self.compare_completed_run(true, cx);
        } else {
            self.review_results(cx);
        }
        cx.notify();
    }
    fn review_results(&self, cx: &mut Context<Self>) {
        if let Some(request) = &self.manager.request {
            let root = request.options.output_directory(&request.root);
            if !request.options.run_id.is_empty() && !self.executed_suites.is_empty() {
                cx.emit(RunEvent::ReviewSavedResults {
                    root,
                    targets: self
                        .executed_suites
                        .iter()
                        .map(|(_, id)| (id.clone(), request.options.run_id.clone()))
                        .collect(),
                });
            } else {
                cx.emit(RunEvent::ReviewResults {
                    root,
                    run_id: request.options.run_id.clone(),
                });
            }
        }
    }
    fn compare_completed_run(&mut self, review_after_run: bool, cx: &mut Context<Self>) {
        if self.running() || self.disabled {
            return;
        }
        let Some(request) = self.manager.request.clone() else {
            return;
        };
        let result = RunIntent::compare_run(request, &self.executed_suites, review_after_run)
            .and_then(|intent| {
                intent.resolve(&self.suites)?;
                Ok(intent)
            });
        match result {
            Ok(intent) => cx.emit(RunEvent::Requested(Box::new(intent))),
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
            }
        }
    }
    pub(super) fn cancel(&mut self, cx: &mut Context<Self>) {
        self.manager.cancel();
        self.status = "Cancelling process tree...".into();
        cx.notify();
    }
    pub(super) fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        cx.notify();
    }

    pub(super) fn set_workspace(
        &mut self,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear(window, cx);
        self.root = Some(root);
        self.confirmation = false;
        self.rebuild(window, cx);
        self.refresh_artifacts(cx);
        cx.notify();
    }

    pub(super) fn restore_activity(
        &mut self,
        path: Option<PathBuf>,
        activity: Result<Option<RunSnapshot>>,
        warning: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = path {
            self.manager.set_storage_path(path);
        }
        let restored = (|| -> Result<()> {
            let Some(snapshot) = activity? else {
                return Ok(());
            };
            self.manager.restore(snapshot)?;
            let request = self
                .manager
                .request
                .as_ref()
                .ok_or_else(|| anyhow!("Missing saved run request"))?;
            let mut options = request.options.clone();
            options.run_id.clear();
            self.options = serde_json::to_value(options)?;
            self.command = request.command;
            self.selection.clear();
            for suite in self.suites.iter().filter(|suite| {
                suite.error.is_none()
                    && request.options.suites.contains(&glob::Pattern::escape(
                        if request.command.uses_output_suite_ids() {
                            &suite.output_id
                        } else {
                            &suite.name
                        },
                    ))
            }) {
                if request.options.scenarios.is_empty() {
                    self.selection.toggle(suite, true)?;
                } else {
                    for scenario in &suite.scenarios {
                        if request.options.scenarios.iter().any(|filter| {
                            filter.scenario == scenario.id
                                && filter.suite
                                    == if request.command.uses_output_suite_ids() {
                                        &suite.output_id
                                    } else {
                                        &suite.name
                                    }
                                    .as_str()
                        }) {
                            self.selection.toggle_scenario(suite, &scenario.id, true)?;
                        }
                    }
                }
            }
            self.executed_suites = self
                .manager
                .progress
                .suites
                .values()
                .map(|suite| {
                    let name = self
                        .suites
                        .iter()
                        .find(|current| current.output_id == suite.id)
                        .map(|current| current.name.clone())
                        .unwrap_or_else(|| suite.name.clone());
                    (name, suite.id.clone())
                })
                .collect();
            if self.executed_suites.is_empty() {
                self.executed_suites = self
                    .suites
                    .iter()
                    .filter(|suite| self.selection.contains(&suite.name))
                    .map(|suite| (suite.name.clone(), suite.output_id.clone()))
                    .collect();
            }
            self.status = self.completion_status();
            self.rebuild(window, cx);
            self.refresh_log(window, cx);
            Ok(())
        })();
        if let Err(error) = restored {
            let error = format!("Could not restore saved run activity: {error:#}");
            self.status = error.clone();
            self.manager.persistence_error = Some(error);
        } else if let Some(warning) = warning {
            self.status = warning.clone();
            self.manager.persistence_error = Some(warning);
        }
        self.refresh_artifacts(cx);
        cx.emit(RunEvent::SelectionChanged);
        cx.emit(RunEvent::ProgressChanged);
        cx.notify();
    }

    fn completion_status(&self) -> String {
        let status = match &self.manager.outcome {
            Some(RunOutcome::Completed) => "Completed".into(),
            Some(RunOutcome::Cancelled) => "Cancelled".into(),
            Some(RunOutcome::Failed(error)) => error.clone(),
            None => "Command ended without a result".into(),
        };
        match &self.manager.persistence_error {
            Some(error) => format!("{status}. Local save failed: {error}"),
            None => status,
        }
    }

    pub(super) fn set_suites(
        &mut self,
        suites: Vec<String>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.running() && !self.disabled,
            "Another operation is already running"
        );
        let mut selection = SuiteSelection::default();
        for suite in self
            .suites
            .iter()
            .filter(|suite| suites.contains(&suite.name))
        {
            selection.toggle(suite, true)?;
        }
        anyhow::ensure!(
            selection.len() == suites.len(),
            "Selected suite is no longer available"
        );
        self.selection = selection;
        self.suite_selection_anchor = None;
        self.scenario_selection_anchor = None;
        self.confirmation = false;
        cx.emit(RunEvent::SelectionChanged);
        cx.notify();
        Ok(())
    }

    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running() {
            return;
        }
        self.root = None;
        self.pending_request = None;
        self.options = serde_json::to_value(RunOptions::default()).unwrap_or(Value::Null);
        self.manager = RunManager::default();
        self.command = RunCommand::Test;
        self.settings_search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.selection.clear();
        self.suite_selection_anchor = None;
        self.scenario_selection_anchor = None;
        self.suites.clear();
        self.catalog_error = None;
        self.log_text.clear();
        self.log_scope = None;
        self.archive_page = None;
        self.archive_key = None;
        self.archive_loading = false;
        self.archive_generation = self.archive_generation.wrapping_add(1);
        self.log_controls
            .update(cx, |controls, cx| controls.availability(None, false, cx));
        self.collapsed.clear();
        self.expanded_selections.clear();
        self.failed_only = false;
        self.started = None;
        self.executed_suites.clear();
        self.artifacts.clear();
        self.reset_site();
        self.deployment_path = None;
        self.log
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.status = "No command running".into();
        self.tab = 0;
        self.confirmation = false;
        self.rebuild(window, cx);
        cx.notify();
    }

    pub(super) fn update_catalog(
        &mut self,
        suites: Vec<SuiteSummary>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.running() {
            return;
        }
        self.selection.reconcile(&suites);
        self.suites = suites;
        self.catalog_error = error;
        self.confirmation = false;
        self.refresh_artifacts(cx);
        cx.emit(RunEvent::SelectionChanged);
        cx.notify();
    }

    pub(super) fn selected(&self, name: &str) -> bool {
        self.selection.contains(name)
    }
    pub(super) fn selected_count(&self) -> usize {
        self.selection.len()
    }
    pub(super) fn all_selected(&self, name: &str) -> bool {
        self.suites
            .iter()
            .find(|suite| suite.name == name)
            .is_some_and(|suite| self.selection.all_selected(suite))
    }
    pub(super) fn selected_scenario_count(&self) -> usize {
        self.suites
            .iter()
            .map(|suite| self.selection.scenario_count(suite))
            .sum()
    }
    pub(super) fn scenario_selected(&self, name: &str, id: &str) -> bool {
        self.selection.scenario_selected(name, id)
    }
    pub(super) fn set_scenario(
        &mut self,
        name: &str,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.running() && !self.disabled,
            "Another operation is already running"
        );
        let suite = self
            .suites
            .iter()
            .find(|suite| suite.name == name)
            .ok_or_else(|| anyhow!("Selected suite is no longer available"))?;
        let mut selection = SuiteSelection::default();
        selection.toggle_scenario(suite, id, true)?;
        self.selection = selection;
        self.confirmation = false;
        cx.emit(RunEvent::SelectionChanged);
        cx.notify();
        Ok(())
    }
    pub(super) fn update_draft_suite(&mut self, suite: SuiteSummary, cx: &mut Context<Self>) {
        if self.running() || self.disabled {
            return;
        }
        if let Some(current) = self
            .suites
            .iter_mut()
            .find(|current| current.entry.path == suite.entry.path)
        {
            *current = suite;
            self.selection.reconcile(&self.suites);
            self.confirmation = false;
            cx.emit(RunEvent::SelectionChanged);
            cx.notify();
        }
    }
    pub(super) fn toggle_scenario(
        &mut self,
        name: &str,
        id: &str,
        checked: bool,
        cx: &mut Context<Self>,
    ) {
        if self.running() || self.disabled {
            return;
        }
        if let Some(suite) = self.suites.iter().find(|suite| suite.name == name) {
            if let Err(error) = self.selection.toggle_scenario(suite, id, checked) {
                self.status = format!("{error:#}");
            }
            self.confirmation = false;
            cx.emit(RunEvent::SelectionChanged);
            cx.notify();
        }
    }

    pub(super) fn select_scenario(
        &mut self,
        name: &str,
        id: &str,
        visible: &[String],
        modifiers: gpui_kit::Modifiers,
        cx: &mut Context<Self>,
    ) {
        if self.running() || self.disabled {
            return;
        }
        let Some(suite) = self.suites.iter().find(|suite| suite.name == name) else {
            return;
        };
        let anchor = self
            .scenario_selection_anchor
            .as_ref()
            .filter(|(owner, _)| owner == name)
            .map(|(_, id)| id.as_str());
        let Some(ids) = selection_range(visible, id, anchor, modifiers.shift) else {
            return;
        };
        let additive = modifiers.control || cfg!(target_os = "macos") && modifiers.platform;
        let checked = modifiers.shift || !additive || !self.scenario_selected(name, id);
        let mut selection = self.selection.clone();
        let result = (|| -> Result<()> {
            if !additive {
                selection.toggle(suite, false)?;
            }
            for id in ids {
                selection.toggle_scenario(suite, id, checked)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        if !modifiers.shift || anchor.is_none_or(|anchor| !visible.iter().any(|id| id == anchor)) {
            self.scenario_selection_anchor = Some((name.to_string(), id.to_string()));
        }
        self.selection = selection;
        self.confirmation = false;
        cx.emit(RunEvent::SelectionChanged);
        cx.notify();
    }

    fn scope_label(&self) -> String {
        if !self.command.supports_suites() {
            "Workspace command".into()
        } else if self.selection.is_empty() {
            "No suites selected".into()
        } else if self.command.supports_scenarios() && self.selection.has_scenario_selection() {
            let labels = self
                .suites
                .iter()
                .flat_map(|suite| {
                    suite
                        .scenarios
                        .iter()
                        .filter(|scenario| self.scenario_selected(&suite.name, &scenario.id))
                        .map(|scenario| format!("{} / {}", suite.label(), scenario.label))
                })
                .collect::<Vec<_>>();
            format!("Scenarios: {}", labels.join(", "))
        } else {
            format!(
                "Suites: {}",
                self.suites
                    .iter()
                    .filter(|suite| self.selection.contains(&suite.name))
                    .map(|suite| suite.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
    pub(super) fn report_approval_intent(
        &self,
        report: &super::reports::ReportApproval,
    ) -> Result<RunIntent> {
        anyhow::ensure!(
            !self.running() && !self.disabled,
            "Finish the active operation before approving"
        );
        let name = report
            .entry
            .suite_name()
            .ok_or_else(|| anyhow!("The report's suite is unavailable"))?;
        let intent = RunIntent {
            request: RunRequest {
                root: self
                    .root
                    .clone()
                    .ok_or_else(|| anyhow!("Open a workspace first"))?,
                command: RunCommand::Approve,
                options: RunOptions {
                    regressify_dir: display_path(&report.output),
                    run_id: report.run_id.clone(),
                    scenarios: vec![crate::cli::args::ScenarioFilter {
                        suite: name.clone(),
                        scenario: report.scenario_id.clone(),
                    }],
                    report_capture: Some(report.capture_key.clone()),
                    ..Default::default()
                },
            },
            suite_names: vec![name],
            expected_output_ids: Some(vec![report.suite_id.clone()]),
            review_after_run: false,
        };
        intent.resolve(&self.suites)?;
        Ok(intent)
    }
    pub(super) fn selected_names(&self) -> Vec<String> {
        self.suites
            .iter()
            .filter(|suite| self.selection.contains(&suite.name))
            .map(|suite| suite.name.clone())
            .collect()
    }
    pub(super) fn toggle_suite(&mut self, name: &str, checked: bool, cx: &mut Context<Self>) {
        if self.running() || self.disabled {
            return;
        }
        if let Some(suite) = self.suites.iter().find(|suite| suite.name == name) {
            if let Err(error) = self.selection.toggle(suite, checked) {
                self.status = format!("{error:#}");
            }
            self.scenario_selection_anchor = None;
            self.confirmation = false;
            cx.emit(RunEvent::SelectionChanged);
            cx.notify();
        }
    }

    pub(super) fn select_suite(
        &mut self,
        name: &str,
        visible: &[String],
        modifiers: gpui_kit::Modifiers,
        cx: &mut Context<Self>,
    ) {
        if self.running() || self.disabled {
            return;
        }
        let Some(names) = selection_range(
            visible,
            name,
            self.suite_selection_anchor.as_deref(),
            modifiers.shift,
        ) else {
            return;
        };
        let additive = modifiers.control || cfg!(target_os = "macos") && modifiers.platform;
        let checked = modifiers.shift || !additive || !self.selected(name);
        let mut selection = if additive {
            self.selection.clone()
        } else {
            SuiteSelection::default()
        };
        for name in names {
            let Some(suite) = self.suites.iter().find(|suite| &suite.name == name) else {
                return;
            };
            if let Err(error) = selection.toggle(suite, checked) {
                self.status = format!("{error:#}");
                cx.notify();
                return;
            }
        }
        if !modifiers.shift
            || self
                .suite_selection_anchor
                .as_ref()
                .is_none_or(|anchor| !visible.contains(anchor))
        {
            self.suite_selection_anchor = Some(name.to_string());
        }
        self.selection = selection;
        self.scenario_selection_anchor = None;
        self.confirmation = false;
        cx.emit(RunEvent::SelectionChanged);
        cx.notify();
    }

    pub(super) fn select_all(&mut self, selected: bool, cx: &mut Context<Self>) {
        if self.running() || self.disabled {
            return;
        }
        if selected {
            self.selection.select_all(&self.suites);
        } else {
            self.selection.clear();
        }
        self.suite_selection_anchor = None;
        self.scenario_selection_anchor = None;
        self.confirmation = false;
        cx.emit(RunEvent::SelectionChanged);
        cx.notify();
    }

    fn can_start(&self) -> bool {
        self.can_request(self.command)
    }

    fn can_request(&self, command: RunCommand) -> bool {
        !self.running()
            && !self.disabled
            && !self
                .errors
                .keys()
                .any(|key| RunOptions::applies(key, command))
            && self.root.is_some()
            && (command != RunCommand::Test || self.require_references().is_ok())
            && (!command.supports_suites()
                || (!self.selection.is_empty() && self.catalog_error.is_none()))
    }

    fn flush_options(&mut self, command: RunCommand, cx: &mut Context<Self>) -> Result<()> {
        let previous_output = self.output_root().ok();
        let mut options = self.options.clone();
        for binding in &self.bindings {
            let text = match &binding.control {
                Control::Input(input) => input.read(cx).value().to_string(),
                Control::Textarea(input) => input.read(cx).value().to_string(),
                _ => continue,
            };
            if text == binding.field.display(&options[&binding.field.key])
                && !self.errors.contains_key(&binding.field.key)
            {
                continue;
            }
            match binding.field.parse(&text).and_then(|value| {
                let mut candidate = options.clone();
                candidate[&binding.field.key] = value.clone();
                serde_json::from_value::<RunOptions>(candidate)?;
                Ok(value)
            }) {
                Ok(value) => {
                    options[&binding.field.key] = value;
                    self.errors.remove(&binding.field.key);
                }
                Err(error) => {
                    self.errors
                        .insert(binding.field.key.clone(), format!("{error:#}"));
                }
            }
        }
        self.options = options;
        if self.output_root().ok() != previous_output {
            self.refresh_artifacts(cx);
        }
        anyhow::ensure!(
            !self
                .errors
                .keys()
                .any(|key| RunOptions::applies(key, command)),
            "Correct run options before continuing"
        );
        Ok(())
    }

    pub(super) fn output_root(&self) -> Result<PathBuf> {
        let options: RunOptions = serde_json::from_value(self.options.clone())?;
        Ok(options.output_directory(
            self.root
                .as_ref()
                .ok_or_else(|| anyhow!("Open a workspace first"))?,
        ))
    }

    pub(super) fn suite_output_roots(&self) -> Vec<PathBuf> {
        let mut roots = self.manager.output_directories();
        if let Ok(root) = self.output_root() {
            roots.push(root);
        }
        roots
    }

    pub(super) fn choose(&mut self, command: RunCommand, _: &mut Window, cx: &mut Context<Self>) {
        if self.running() || self.disabled {
            return;
        }
        if let Err(error) = self.flush_options(command, cx) {
            self.status = format!("{error:#}");
            self.tab = 2;
            cx.notify();
            return;
        }
        self.command = command;
        self.tab = if command.supports_scenarios() { 0 } else { 2 };
        self.confirmation = false;
        cx.notify();
    }

    fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.bindings.clear();
        self.subscriptions.clear();
        self.errors.clear();
        let Ok(fields) = schema::fields(&schema::Scope::Run) else {
            return;
        };
        for field in fields
            .into_iter()
            .filter(|field| !matches!(field.key.as_str(), "suites" | "scenarios" | "deployment"))
        {
            let text = field.display(&self.options[&field.key]);
            let control = match &field.kind {
                FieldKind::Boolean => Control::Boolean,
                FieldKind::List { .. } => {
                    let input =
                        cx.new(|cx| TextareaState::new(window, cx).rows(2).default_value(text));
                    let descriptor = field.clone();
                    self.subscriptions.push(cx.subscribe(
                        &input,
                        move |this, input, event: &InputEvent, cx| {
                            if matches!(event, InputEvent::Change) {
                                this.change(&descriptor, input.read(cx).value().as_ref(), cx);
                            }
                        },
                    ));
                    Control::Textarea(input)
                }
                _ => {
                    let input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .default_value(text)
                            .masked(field.key == "proxy")
                    });
                    let descriptor = field.clone();
                    self.subscriptions.push(cx.subscribe(
                        &input,
                        move |this, input, event: &InputEvent, cx| {
                            if matches!(event, InputEvent::Change) {
                                this.change(&descriptor, input.read(cx).value().as_ref(), cx);
                            }
                        },
                    ));
                    Control::Input(input)
                }
            };
            self.bindings.push(Binding { field, control });
        }
    }

    fn change(&mut self, field: &Field, text: &str, cx: &mut Context<Self>) {
        if self.running() || self.disabled {
            return;
        }
        match field.parse(text).and_then(|value| {
            let mut candidate = self.options.clone();
            candidate[&field.key] = value.clone();
            serde_json::from_value::<RunOptions>(candidate)?;
            Ok(value)
        }) {
            Ok(value) => {
                self.options[&field.key] = value;
                self.errors.remove(&field.key);
            }
            Err(error) => {
                self.errors.insert(field.key.clone(), format!("{error:#}"));
            }
        }
        if matches!(field.key.as_str(), "siteEnv" | "regressifyDir") {
            self.refresh_artifacts(cx);
        }
        self.confirmation = false;
        cx.notify();
    }

    fn request_start(&mut self, cx: &mut Context<Self>) {
        match self.prepare_intent(cx) {
            Ok(intent) if self.command.needs_confirmation() => {
                self.pending_request = Some(intent);
                self.confirmation = true;
            }
            Ok(intent) => cx.emit(RunEvent::Requested(Box::new(intent))),
            Err(error) => {
                self.status = format!("{error:#}");
                self.tab = 2;
            }
        }
        cx.notify();
    }

    pub(super) fn prepare_intent(&mut self, cx: &mut Context<Self>) -> Result<RunIntent> {
        self.flush_options(self.command, cx)?;
        if self.command == RunCommand::Test {
            self.require_references()?;
        }
        anyhow::ensure!(
            self.can_start(),
            "Select valid suites and correct run settings before continuing"
        );
        let mut values = self.options.clone();
        let defaults = serde_json::to_value(RunOptions::default())?;
        if let Some(values) = values.as_object_mut() {
            for (key, value) in values {
                if !RunOptions::applies(key, self.command) {
                    *value = defaults[key].clone();
                }
            }
        }
        let mut options: RunOptions = serde_json::from_value(values)?;
        if self.command.supports_scenarios() {
            options.scenarios = self.selection.scenario_filters(&self.suites, false)?;
        }
        let suite_names = self.selected_names();
        let mut expected_output_ids = None;
        if matches!(self.command, RunCommand::Approve | RunCommand::Compare)
            && options.run_id.is_empty()
            && let Some(previous) = &self.manager.request
            && previous.command.supports_suites()
            && !previous.options.run_id.is_empty()
            && (previous.options.scenarios.is_empty()
                || (!options.scenarios.is_empty()
                    && options.scenarios.iter().all(|selected| {
                        previous.options.scenarios.iter().any(|old| {
                            old.scenario == selected.scenario
                                && if matches!(
                                    previous.command,
                                    RunCommand::Compare | RunCommand::Approve
                                ) {
                                    self.executed_suites.iter().any(|(name, id)| {
                                        name == &selected.suite && id == &old.suite
                                    })
                                } else {
                                    old.suite == selected.suite
                                }
                        })
                    })))
            && self.root.as_ref().is_some_and(|root| {
                options.output_directory(root) == previous.options.output_directory(&previous.root)
            })
            && self
                .suites
                .iter()
                .filter(|suite| suite_names.contains(&suite.name))
                .all(|suite| {
                    previous.options.suites.contains(&glob::Pattern::escape(
                        if matches!(previous.command, RunCommand::Approve | RunCommand::Compare) {
                            &suite.output_id
                        } else {
                            &suite.name
                        },
                    ))
                })
        {
            options.run_id = previous.options.run_id.clone();
            let ids = self
                .executed_suites
                .iter()
                .filter(|(name, _)| suite_names.contains(name))
                .map(|(_, id)| id.clone())
                .collect::<Vec<_>>();
            if ids.len() == suite_names.len() {
                expected_output_ids = Some(ids);
            }
        }
        let intent = RunIntent {
            request: RunRequest {
                root: self
                    .root
                    .clone()
                    .ok_or_else(|| anyhow!("Open a workspace first"))?,
                command: self.command,
                options,
            },
            suite_names,
            expected_output_ids,
            review_after_run: false,
        };
        intent.resolve(&self.suites)?;
        Ok(intent)
    }

    pub(super) fn request_command(
        &mut self,
        command: RunCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running() || self.disabled {
            return;
        }
        self.choose(command, window, cx);
        if self.command != command
            || self
                .errors
                .keys()
                .any(|key| RunOptions::applies(key, command))
        {
            return;
        }
        self.tab = 0;
        self.confirmation = false;
        self.request_start(cx);
    }

    pub(super) fn deployment_intent(&mut self, cx: &mut Context<Self>) -> Result<RunIntent> {
        let previous = self.command;
        self.command = RunCommand::Snapshot;
        let result = self.prepare_intent(cx);
        self.command = previous;
        result
    }

    /// A deployment intent that generates no suite, for removing suites from a site. It keeps
    /// the current report settings (output directory and report text) like any deployment.
    pub(super) fn removal_intent(&mut self, cx: &mut Context<Self>) -> Result<RunIntent> {
        let previous = self.command;
        self.command = RunCommand::Snapshot;
        let result = (|| {
            self.flush_options(RunCommand::Snapshot, cx)?;
            let mut values = self.options.clone();
            let defaults = serde_json::to_value(RunOptions::default())?;
            if let Some(values) = values.as_object_mut() {
                for (key, value) in values {
                    if !RunOptions::applies(key, RunCommand::Snapshot) {
                        *value = defaults[key].clone();
                    }
                }
            }
            let options: RunOptions = serde_json::from_value(values)?;
            let root = self
                .root
                .clone()
                .ok_or_else(|| anyhow!("Open a workspace before deploying"))?;
            Ok(RunIntent {
                request: RunRequest {
                    root,
                    command: RunCommand::Snapshot,
                    options,
                },
                suite_names: Vec::new(),
                expected_output_ids: Some(Vec::new()),
                review_after_run: false,
            })
        })();
        self.command = previous;
        result
    }

    pub(super) fn start(
        &mut self,
        request: RunRequest,
        review_after_run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running() || self.disabled || self.root.as_ref() != Some(&request.root) {
            return;
        }
        self.command = request.command;
        self.executed_suites = self
            .suites
            .iter()
            .filter(|suite| {
                request.options.suites.contains(&glob::Pattern::escape(
                    if request.command.uses_output_suite_ids() {
                        &suite.output_id
                    } else {
                        &suite.name
                    },
                ))
            })
            .map(|suite| (suite.name.clone(), suite.output_id.clone()))
            .collect();
        match self.manager.start(request) {
            Ok(()) => {
                self.confirmation = false;
                self.pending_request = None;
                self.tab = 0;
                self.started = Some(Instant::now());
                self.elapsed_seconds = 0;
                self.last_run_refresh = self.started;
                self.run_refresh_pending = false;
                self.collapsed.clear();
                self.status = format!("Running {}", self.command.label());
                self.log_text.clear();
                self.log_scope = None;
                self.archive_page = None;
                self.archive_key = None;
                self.archive_loading = false;
                self.archive_generation = self.archive_generation.wrapping_add(1);
                self.log_controls
                    .update(cx, |controls, cx| controls.availability(None, true, cx));
                self.log
                    .update(cx, |state, cx| state.set_value("", window, cx));
                cx.spawn_in(window, async move |view, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        let running = view
                            .update_in(cx, |this, window, cx| {
                                this.poll_run(Instant::now(), review_after_run, window, cx)
                            })
                            .unwrap_or(false);
                        if !running {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => self.status = format!("{error:#}"),
        }
        cx.notify();
    }

    fn poll_run(
        &mut self,
        now: Instant,
        review_after_run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let revision = self.manager.revision();
        let finished = self.manager.poll();
        let changed = self.manager.revision() != revision;
        self.run_refresh_pending |= changed;
        let elapsed_seconds = self.started.map_or(0, |started| {
            now.saturating_duration_since(started).as_secs()
        });
        let refresh_due = self
            .last_run_refresh
            .is_none_or(|last| now.saturating_duration_since(last) >= RUN_UI_REFRESH_INTERVAL);
        // Drain queues every tick, but coalesce UI/log work. Terminal updates bypass
        // the throttle, including the worker exit before activity persistence finishes.
        if finished
            || (changed && self.manager.outcome.is_some())
            || (refresh_due
                && (self.run_refresh_pending || elapsed_seconds != self.elapsed_seconds))
        {
            if self.run_refresh_pending {
                cx.emit(RunEvent::ProgressChanged);
                self.refresh_log(window, cx);
                self.run_refresh_pending = false;
            }
            self.elapsed_seconds = elapsed_seconds;
            self.last_run_refresh = Some(now);
            cx.notify();
        }
        if finished {
            self.status = self.completion_status();
            cx.emit(RunEvent::Finished);
            if review_after_run && self.executed_report_available() {
                self.review_results(cx);
            }
        }
        self.running()
    }
}

impl RunPanel {
    fn render_selection(&self, cx: &mut Context<Self>) -> AnyElement {
        let disabled = self.disabled || self.running();
        let all_selected = !self.selection.is_empty()
            && self
                .suites
                .iter()
                .filter(|suite| suite.error.is_none())
                .all(|suite| self.selection.all_selected(suite));
        let panel = cx.entity();
        let mut list = div()
            .id("run-selection-list")
            .test_support()
            .key_context("RegressifyChecklist")
            .track_focus(&self.selection_focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.selection_focus.contains_focused(window, cx) {
                        window.focus(&this.selection_focus, cx);
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &SelectAllListItems, _, cx| {
                this.select_all(true, cx);
            }))
            .on_action(cx.listener(|this, _: &ClearListSelection, _, cx| {
                this.select_all(false, cx);
            }))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(format!(
                        "{} {} in {} {} selected",
                        self.selected_scenario_count(),
                        if self.selected_scenario_count() == 1 {
                            "scenario"
                        } else {
                            "scenarios"
                        },
                        self.selection.len(),
                        if self.selection.len() == 1 {
                            "suite"
                        } else {
                            "suites"
                        }
                    ))
                    .child(
                        design::selection_checkbox(
                            "select-all-suites",
                            all_selected,
                            !self.selection.is_empty() && !all_selected,
                            disabled || self.suites.is_empty(),
                            cx,
                        )
                        .accessibility_label("Select all")
                        .child("Select all")
                        .on_change(move |state, _, _, cx| {
                            panel.update(cx, |panel, cx| {
                                panel
                                    .select_all(state == gpui_kit::base::CheckboxState::Checked, cx)
                            });
                        }),
                    ),
            );
        for suite in &self.suites {
            let name = suite.name.clone();
            let report_name = name.clone();
            let panel = cx.entity();
            let expanded = self.expanded_selections.contains(&name);
            let expand_name = name.clone();
            let mut section = div()
                .flex()
                .flex_col()
                .gap_1()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            design::selection_checkbox(
                                SharedString::from(format!("suite-check-{name}")),
                                self.selection.all_selected(suite),
                                self.selection.contains(&name)
                                    && !self.selection.all_selected(suite),
                                disabled || suite.error.is_some(),
                                cx,
                            )
                            .accessibility_label(format!("Select suite {name}"))
                            .on_change(move |state, _, _, cx| {
                                panel.update(cx, |panel, cx| {
                                    panel.toggle_suite(
                                        &name,
                                        state == gpui_kit::base::CheckboxState::Checked,
                                        cx,
                                    )
                                });
                            }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("suite-expand-{}", suite.name)))
                                .ghost()
                                .flex_1()
                                .min_w_0()
                                .justify_start()
                                .icon(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .accessibility_label(format!(
                                    "{} ({}/{} scenarios)",
                                    suite.label(),
                                    self.selection.scenario_count(suite),
                                    suite.scenarios.len()
                                ))
                                .child(div().flex_1().min_w_0().truncate().text_left().child(
                                    format!(
                                        "{} ({}/{} scenarios)",
                                        suite.label(),
                                        self.selection.scenario_count(suite),
                                        suite.scenarios.len()
                                    ),
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.expanded_selections.remove(&expand_name) {
                                        this.expanded_selections.insert(expand_name.clone());
                                    }
                                    cx.notify();
                                })),
                        )
                        .when(self.can_open_saved_suite(&suite.name), |row| {
                            row.child(
                                Button::new(SharedString::from(format!(
                                    "saved-report-{}",
                                    suite.name
                                )))
                                .ghost()
                                .icon(gpui_kit::assets::IconName::Images)
                                .label("Open Report")
                                .disabled(self.disabled)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.open_saved_reports(vec![report_name.clone()], cx)
                                    },
                                )),
                            )
                        })
                        .when_some(self.suite_site_url(&suite.name), |row, url| {
                            row.child(
                                Button::new(SharedString::from(format!(
                                    "saved-site-{}",
                                    suite.name
                                )))
                                .ghost()
                                .icon(gpui_kit::assets::IconName::ExternalLink)
                                .label("Open Site")
                                .tooltip(url.clone())
                                .disabled(disabled)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.open_site(&url, cx)),
                                ),
                            )
                        }),
                )
                .child(
                    div()
                        .pl_6()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(self.artifact_summary(&suite.name)),
                )
                .children(suite.error.as_ref().map(|error| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error.clone())
                }));
            if expanded {
                for scenario in &suite.scenarios {
                    let name = suite.name.clone();
                    let id = scenario.id.clone();
                    let log_suite = name.clone();
                    let log_id = id.clone();
                    section = section.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .pl_6()
                            .py_1()
                            .child(
                                Checkbox::new(SharedString::from(format!(
                                    "scenario-check-{name}:{id}"
                                )))
                                .label(scenario.label.clone())
                                .flex_1()
                                .min_w_0()
                                .tooltip(format!("{}\n{id}", scenario.url))
                                .checked(self.scenario_selected(&name, &id))
                                .disabled(disabled || suite.error.is_some())
                                .on_change(cx.listener(
                                    move |this, checked, _, cx| {
                                        this.toggle_scenario(&name, &id, *checked, cx)
                                    },
                                )),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "scenario-log-{log_suite}:{log_id}"
                                )))
                                .ghost()
                                .icon(gpui_kit::assets::IconName::Terminal)
                                .tooltip(format!("View run log: {}", scenario.label))
                                .accessibility_label(format!("View run log: {}", scenario.label))
                                .disabled(
                                    self.disabled || !self.has_scenario_log(&log_suite, &log_id),
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.show_scenario_log(&log_suite, &log_id, window, cx)
                                    },
                                )),
                            )
                            .children([RunCommand::Test, RunCommand::Reference].into_iter().map(
                                |command| {
                                    let name = suite.name.clone();
                                    let id = scenario.id.clone();
                                    Button::new(SharedString::from(format!(
                                        "scenario-{}-{name}:{id}",
                                        command.argument()
                                    )))
                                    .ghost()
                                    .icon(if command == RunCommand::Test {
                                        gpui_kit::assets::IconName::Play
                                    } else {
                                        gpui_kit::assets::IconName::Camera
                                    })
                                    .tooltip(format!(
                                        "{}: {}",
                                        command.action_label(),
                                        scenario.label
                                    ))
                                    .accessibility_label(format!(
                                        "{}: {}",
                                        command.action_label(),
                                        scenario.label
                                    ))
                                    .disabled(
                                        disabled
                                            || suite.error.is_some()
                                            || (command == RunCommand::Test
                                                && !self.has_reference(&name)),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        match this.set_scenario(&name, &id, cx) {
                                            Ok(()) => this.request_command(command, window, cx),
                                            Err(error) => {
                                                this.status = format!("{error:#}");
                                                cx.notify();
                                            }
                                        }
                                    }))
                                },
                            )),
                    );
                }
            }
            list = list.child(section);
        }
        if let Some(error) = &self.catalog_error {
            list = list.child(div().text_color(cx.theme().danger).child(error.clone()));
        }
        list.into_any_element()
    }
}

impl Render for RunPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.running();
        let focus = self.focus.clone();
        let scope = self.scope_label();
        let site_url = self.selected_site_url().filter(|_| !running);
        let can_open_report = self.can_open_report();
        let content = match self.tab {
            0 => self.render_activity(cx),
            1 => div()
                .id("run-suite-list")
                .size_full()
                .overflow_y_scrollbar()
                .p_5()
                .child(self.render_selection(cx))
                .into_any_element(),
            2 => self.render_options(cx),
            _ => self.render_log(cx),
        };
        div()
            .id("run-panel")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .on_action(cx.listener(|this, action: &RequestCommand, window, cx| {
                if let Some(command) = RunCommand::ALL.get(action.index) {
                    this.request_command(*command, window, cx);
                }
            }))
            .child(
                div()
                    .id("run-toolbar")
                    .role(Role::Group)
                    .aria_label("Run commands")
                    .test_support()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(design::ToolbarFocus::new(
                        "run-toolbar-focus",
                        gpui_kit::component::toolbar::Toolbar::new("run-command-toolbar")
                            .with_size(gpui_kit::component::Size::Medium)
                            .h_auto()
                            .px_4()
                            .flex_wrap()
                            .children(
                                [
                                    (
                                        "run-test",
                                        RunCommand::Test,
                                        gpui_kit::assets::IconName::Play,
                                    ),
                                    (
                                        "run-reference",
                                        RunCommand::Reference,
                                        gpui_kit::assets::IconName::Camera,
                                    ),
                                    (
                                        "run-approve",
                                        RunCommand::Approve,
                                        gpui_kit::assets::IconName::Check,
                                    ),
                                    (
                                        "run-compare",
                                        RunCommand::Compare,
                                        gpui_kit::assets::IconName::Images,
                                    ),
                                ]
                                .into_iter()
                                .map(|(id, command, icon)| {
                                    Button::new(id)
                                        .icon(icon)
                                        .label(command.action_label())
                                        .when(command == RunCommand::Test, |button| {
                                            button.tooltip(
                                        "Requires reference images for every selected suite",
                                    )
                                        })
                                        .when(command == RunCommand::Test, |button| {
                                            button.primary()
                                        })
                                        .disabled(!self.can_request(command))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.request_command(command, window, cx)
                                        }))
                                }),
                            )
                            .content(
                                Button::new("more-run-actions")
                                    .ghost()
                                    .icon(gpui_kit::assets::IconName::Ellipsis)
                                    .accessibility_label("More actions")
                                    .tooltip("More actions")
                                    .disabled(running || self.disabled || self.root.is_none())
                                    .prepare_for_toolbar()
                                    .with_size(gpui_kit::component::Size::Medium)
                                    .dropdown_menu(move |menu, window, cx| {
                                        design::restore_dropdown_menu_focus(
                                            menu.focus_handle(cx),
                                            focus.clone(),
                                            window,
                                            cx,
                                        );
                                        let mut menu = menu.action_context(focus.clone());
                                        for (index, command) in RunCommand::ALL
                                            .iter()
                                            .enumerate()
                                            .filter(|(_, command)| !command.supports_scenarios())
                                        {
                                            menu = menu.menu(
                                                command.action_label(),
                                                Box::new(RequestCommand { index }),
                                            );
                                        }
                                        menu
                                    }),
                            )
                            .child(
                                Button::new("cancel-command")
                                    .icon(gpui_kit::assets::IconName::Square)
                                    .label("Stop")
                                    .tooltip("Cancel running command")
                                    .disabled(!running)
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                            )
                            .when(can_open_report || site_url.is_some(), |bar| {
                                bar.child(
                                    gpui_kit::component::toolbar::ToolbarGroup::new(
                                        "run-output-actions",
                                    )
                                    .label("Run output")
                                    .gap_2()
                                    .when(can_open_report, |row| {
                                        row.child(
                                            Button::new("activity-view-results")
                                                .primary()
                                                .icon(gpui_kit::assets::IconName::Images)
                                                .label("Open Report")
                                                .tooltip(
                                                    "Open this run's test and reference comparison",
                                                )
                                                .disabled(self.disabled)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.open_report(cx)
                                                })),
                                        )
                                    })
                                    .when_some(
                                        site_url,
                                        |row, url| {
                                            row.child(
                                                Button::new("run-open-site")
                                                    .icon(gpui_kit::assets::IconName::ExternalLink)
                                                    .label("Open Site")
                                                    .tooltip(url.clone())
                                                    .disabled(self.disabled)
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.open_site(&url, cx)
                                                        },
                                                    )),
                                            )
                                        },
                                    ),
                                )
                            })
                            .content(
                                div()
                                    .id("run-scope")
                                    .role(Role::Status)
                                    .aria_label(scope.clone())
                                    .test_support()
                                    .w_full()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(scope),
                            ),
                    )),
            )
            .when(
                self.manager
                    .request
                    .as_ref()
                    .is_none_or(|request| request.command != RunCommand::Deploy),
                |panel| panel.child(self.render_artifact_status(cx)),
            )
            .when_some(
                self.pending_request.as_ref().filter(|_| self.confirmation),
                |panel, intent| {
                    panel.child(
                        div()
                            .id("run-confirmation")
                            .role(Role::Group)
                            .aria_label("Run confirmation")
                            .test_support()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap_3()
                            .px_4()
                            .py_3()
                            .bg(cx.theme().warning.opacity(0.12))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(intent.request.command.confirmation_message()),
                                    )
                                    .child(div().text_sm().child(
                                        if intent.request.command.supports_suites() {
                                            format!("Suites: {}", intent.suite_names.join(", "))
                                        } else {
                                            "Workspace images".into()
                                        },
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .truncate()
                                            .child(display_path(
                                                &intent
                                                    .request
                                                    .options
                                                    .output_directory(&intent.request.root),
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .id("run-confirm-target")
                                    .role(Role::Status)
                                    .aria_label(format!(
                                        "Run: {}",
                                        if intent.request.options.run_id.is_empty() {
                                            "Latest results"
                                        } else {
                                            &intent.request.options.run_id
                                        }
                                    ))
                                    .test_support()
                                    .text_xs()
                                    .child(if intent.request.options.run_id.is_empty() {
                                        if intent.request.command.captures() {
                                            "New capture".to_string()
                                        } else {
                                            "Latest results".to_string()
                                        }
                                    } else {
                                        format!("Run: {}", intent.request.options.run_id)
                                    }),
                            )
                            .child(Button::new("run-confirm-cancel").label("Cancel").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.confirmation = false;
                                    cx.notify();
                                }),
                            ))
                            .child(
                                Button::new("run-confirm")
                                    .danger()
                                    .label(self.command.action_label())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.confirmation
                                            && !this.disabled
                                            && !this.running()
                                            && let Some(intent) = this.pending_request.take()
                                        {
                                            this.confirmation = false;
                                            cx.emit(RunEvent::Requested(Box::new(intent)));
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                },
            )
            .child(
                TabBar::new("run-tabs")
                    .underline()
                    .px_4()
                    .selected_index(self.tab)
                    .on_click(cx.listener(|this, index, window, cx| {
                        this.tab = *index;
                        if this.tab == 3 {
                            this.refresh_log(window, cx);
                        }
                        cx.notify();
                    }))
                    .child(Tab::new().label("Activity"))
                    .child(Tab::new().label("Suites"))
                    .child(Tab::new().label("Run settings"))
                    .child(Tab::new().label("Log")),
            )
            .child(div().flex_1().min_h_0().child(content))
            .child(
                div()
                    .id("run-status")
                    .role(Role::Status)
                    .aria_label(self.status.clone())
                    .test_support()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(self.status.clone()),
            )
            .when(self.manager.omitted_chunks > 0, |panel| {
                panel.child(div().px_4().text_xs().child(format!(
                    "{} log chunks omitted",
                    self.manager.omitted_chunks
                )))
            })
    }
}

#[cfg(all(test, feature = "gui-tests"))]
mod tests {
    use super::{RunCommand, RunEvent, RunIntent, RunOptions, RunOutcome, RunRequest};
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use gpui_kit::{AppContext, Focusable, TestAppContext};
    use serde_json::json;
    use std::{cell::RefCell, rc::Rc};

    #[gpui_kit::test]
    fn runs_normal_controls_fit_minimum_and_large_windows(cx: &mut TestAppContext) {
        use gpui_kit::{SharedString, px, size};

        for (width, height) in [(1000., 640.), (1600., 1000.)] {
            let directory = tempfile::tempdir().unwrap();
            let (handle, view) = super::super::tests::editor_at_size(
                cx,
                directory.path(),
                size(px(width), px(height)),
            );
            for dark in [false, true] {
                cx.update_window(handle.into(), |_, window, cx| {
                    super::super::design::apply_theme(dark, Some(window), cx);
                    let runs = view.read(cx).runs.clone();
                    view.update(cx, |editor, cx| {
                        editor.main_tab = 1;
                        cx.notify();
                    });
                    runs.update(cx, |panel, cx| {
                        panel.tab = 0;
                        panel.select_all(true, cx);
                    });
                    window.render_frame(cx);
                    let normal_height = window.rem_size() * 2.;
                    let toolbar = window.find("run-toolbar").bounds();
                    for id in [
                        "run-test",
                        "run-reference",
                        "run-approve",
                        "run-compare",
                        "more-run-actions",
                        "cancel-command",
                        "refresh-run-artifacts",
                        "activity-select-suites",
                    ] {
                        let control = window.find(id);
                        let bounds = control.bounds();
                        assert!(control.visible(), "{id} at {width}x{height} dark={dark}");
                        assert!(bounds.size.height >= normal_height, "{id}: {bounds:?}");
                        assert!(
                            bounds.left() >= toolbar.left() && bounds.right() <= toolbar.right(),
                            "{id}: {bounds:?}, toolbar={toolbar:?}"
                        );
                        assert!(
                            bounds.top() >= px(0.) && bounds.bottom() <= px(height),
                            "{id}: {bounds:?}"
                        );
                    }
                    window.within("run-tabs").click(1_usize, cx);
                    let scenario = runs.read(cx).suites[0].scenarios[0].id.clone();
                    let scenario_check =
                        SharedString::from(format!("scenario-check-ui-test:{scenario}"));
                    if window.try_find(scenario_check.clone()).is_none() {
                        window.click("suite-expand-ui-test", cx);
                    }
                    for id in [
                        "suite-expand-ui-test".to_string(),
                        format!("scenario-log-ui-test:{scenario}"),
                        format!("scenario-test-ui-test:{scenario}"),
                        format!("scenario-ref-ui-test:{scenario}"),
                    ] {
                        let control = window.find(SharedString::from(id.clone()));
                        let bounds = control.bounds();
                        assert!(control.visible(), "{id} at {width}x{height} dark={dark}");
                        assert!(bounds.size.height >= normal_height, "{id}: {bounds:?}");
                        assert!(
                            bounds.left() >= toolbar.left() && bounds.right() <= toolbar.right(),
                            "{id}: {bounds:?}, toolbar={toolbar:?}"
                        );
                    }
                    window.click(scenario_check.clone(), cx);
                    assert!(!runs.read(cx).scenario_selected("ui-test", &scenario));
                    window.click(scenario_check, cx);
                    assert!(runs.read(cx).scenario_selected("ui-test", &scenario));
                    window.within("run-tabs").click(2_usize, cx);
                    let fields = runs
                        .read(cx)
                        .bindings
                        .iter()
                        .filter_map(|binding| {
                            let input_id: gpui_kit::ElementId = match &binding.control {
                                super::Control::Input(_) => {
                                    SharedString::from(format!("run-field-{}", binding.field.key))
                                        .into()
                                }
                                super::Control::Textarea(input) => {
                                    ("input", input.entity_id()).into()
                                }
                                _ => return None,
                            };
                            Some((binding.field.key.clone(), input_id))
                        })
                        .collect::<Vec<_>>();
                    assert!(!fields.is_empty());
                    for (key, input_id) in fields {
                        runs.update(cx, |panel, cx| {
                            panel
                                .settings_search
                                .update(cx, |state, cx| state.set_value(key.clone(), window, cx));
                            cx.notify();
                        });
                        window.render_frame(cx);
                        let field = window
                            .find(SharedString::from(format!("run-field-{key}")))
                            .bounds();
                        let input = window.find(input_id).bounds();
                        assert!(field.size.height >= normal_height, "{key}: {field:?}");
                        assert!(
                            field.left() >= toolbar.left() && field.right() <= toolbar.right(),
                            "{key}: {field:?}, toolbar={toolbar:?}"
                        );
                        assert!(
                            field.top() >= px(0.) && field.bottom() <= px(height),
                            "{key}: {field:?}"
                        );
                        assert!(
                            input.left() >= field.left() && input.right() <= field.right(),
                            "{key}: input={input:?}, field={field:?}"
                        );
                    }
                    assert!(
                        window.find("run-settings-search").bounds().size.height >= normal_height
                    );
                    runs.update(cx, |panel, cx| {
                        panel.settings_search.update(cx, |state, cx| {
                            state.set_value("browser download", window, cx)
                        });
                        cx.notify();
                    });
                    window.render_frame(cx);
                    for index in 0_usize..3 {
                        let tab = window.within("run-field-fetchBrowser").find(index).bounds();
                        assert!(
                            tab.size.height >= normal_height,
                            "browser tab {index}: {tab:?}"
                        );
                        assert!(
                            tab.left() >= toolbar.left() && tab.right() <= toolbar.right(),
                            "browser tab {index}: {tab:?}"
                        );
                    }
                    assert!(!runs.read(cx).running());
                })
                .unwrap();
            }
        }
    }

    #[gpui_kit::test]
    fn run_log_hidden_during_capture_does_not_rebuild_editor(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            let runs = view.read(cx).runs.clone();
            runs.update(cx, |panel, cx| {
                let (mut manager, _) = crate::desktop::runner::tests::polling_fixture();
                manager.request = Some(RunRequest {
                    root: directory.path().to_path_buf(),
                    command: RunCommand::Reference,
                    options: RunOptions::default(),
                });
                let text = "retained worker output\n".repeat(80_000);
                manager.logs.push_back(crate::desktop::runner::LogLine {
                    stderr: false,
                    text: text.clone(),
                });
                panel.manager = manager;
                panel.log_text = "previously viewed log".into();
                panel.log.update(cx, |editor, cx| {
                    editor.set_value("previously viewed log", window, cx);
                });
                for command in [RunCommand::Reference, RunCommand::Test] {
                    panel.manager.request.as_mut().unwrap().command = command;
                    for tab in 0..3 {
                        panel.tab = tab;
                        panel.refresh_log(window, cx);
                        assert!(
                            panel.log_text == "previously viewed log",
                            "Hidden capture logs must not rebuild retained editor text"
                        );
                        assert!(
                            panel.log.read(cx).value() == "previously viewed log",
                            "Hidden capture logs must not reset the editor"
                        );
                        assert!(!panel.archive_loading);
                        assert!(
                            panel.manager.logs[0].text == text,
                            "Retained output changed"
                        );
                    }
                }
                // Opening Log still starts the authoritative archive read on demand.
                panel.show_all_logs(window, cx);
                assert_eq!(panel.tab, 3);
                assert!(panel.archive_loading);
            });
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn run_poll_unchanged_ticks_do_not_notify_but_clock_and_progress_do(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        let (manager, progress) = crate::desktop::runner::tests::polling_fixture();
        let now = std::time::Instant::now();
        let runs = cx.update(|cx| view.read(cx).runs.clone());
        cx.update(|cx| {
            runs.update(cx, |panel, _| {
                panel.manager = manager;
                panel.started = None;
                panel.elapsed_seconds = 0;
            });
        });
        cx.run_until_parked();
        let notifications = Rc::new(std::cell::Cell::new(0));
        let observed = notifications.clone();
        let _subscription =
            cx.update(|cx| cx.observe(&runs, move |_, _| observed.set(observed.get() + 1)));
        let events = Rc::new(std::cell::Cell::new(0));
        let observed_events = events.clone();
        let _events = cx.update(|cx| {
            cx.subscribe(&runs, move |_, event: &RunEvent, _| {
                if matches!(event, RunEvent::ProgressChanged) {
                    observed_events.set(observed_events.get() + 1);
                }
            })
        });
        for _ in 0..10 {
            cx.update_window(handle.into(), |_, window, cx| {
                runs.update(cx, |panel, cx| {
                    assert!(panel.poll_run(now, false, window, cx))
                });
            })
            .unwrap();
        }
        cx.run_until_parked();
        assert_eq!(notifications.get(), 0);
        assert_eq!(events.get(), 0);

        cx.update_window(handle.into(), |_, window, cx| {
            runs.update(cx, |panel, cx| {
                panel.started = Some(now - std::time::Duration::from_secs(2));
                assert!(panel.poll_run(now, false, window, cx));
                assert!(panel.elapsed_seconds >= 2);
                panel.started = None;
                panel.elapsed_seconds = 0;
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(notifications.get(), 1);
        assert_eq!(
            events.get(),
            0,
            "Clock changes do not invalidate the workbench"
        );

        let mut worker = progress.lock().unwrap();
        worker.apply(crate::progress::Record {
            version: 1,
            elapsed_ms: 100,
            event: crate::progress::Event::Phase {
                name: "Capturing scenarios".into(),
            },
        });
        cx.update_window(handle.into(), |_, window, cx| {
            runs.update(cx, |panel, cx| {
                assert!(panel.poll_run(now + super::RUN_UI_REFRESH_INTERVAL, false, window, cx));
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(notifications.get(), 1, "Contended polling must not redraw");
        drop(worker);

        cx.update_window(handle.into(), |_, window, cx| {
            runs.update(cx, |panel, cx| {
                assert!(panel.poll_run(now + super::RUN_UI_REFRESH_INTERVAL, false, window, cx));
                assert_eq!(panel.manager.progress.phase, "Capturing scenarios");
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(notifications.get(), 2);
        assert_eq!(events.get(), 1);
    }

    #[gpui_kit::test]
    fn run_poll_coalesces_ui_and_logs_at_500ms_and_flushes_terminal_progress(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::runner::LogLine;
        use crate::progress::{Event, Record};
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        let (manager, progress) = crate::desktop::runner::tests::polling_fixture();
        let now = Instant::now();
        let runs = cx.update(|cx| view.read(cx).runs.clone());
        cx.update_window(handle.into(), |_, window, cx| {
            runs.update(cx, |panel, cx| {
                panel.manager = manager;
                panel.tab = 3;
                panel.last_run_refresh = Some(now);
                panel.log_text.clear();
                panel
                    .log
                    .update(cx, |editor, cx| editor.set_value("", window, cx));
            });
        })
        .unwrap();
        cx.run_until_parked();
        let notifications = Rc::new(std::cell::Cell::new(0));
        let observed = notifications.clone();
        let _subscription =
            cx.update(|cx| cx.observe(&runs, move |_, _| observed.set(observed.get() + 1)));
        let events = Rc::new(std::cell::Cell::new(0));
        let observed_events = events.clone();
        let _events = cx.update(|cx| {
            cx.subscribe(&runs, move |_, event: &RunEvent, _| {
                if matches!(event, RunEvent::ProgressChanged) {
                    observed_events.set(observed_events.get() + 1);
                }
            })
        });

        for ms in [100, 200, 300, 400, 499, 500, 999, 1000, 1001] {
            let changed = !matches!(ms, 500 | 1000);
            if changed {
                let mut worker = progress.lock().unwrap();
                worker.apply(Record {
                    version: 1,
                    elapsed_ms: ms,
                    event: Event::Phase {
                        name: format!("phase {ms}"),
                    },
                });
                if ms == 1001 {
                    worker.end(false, None);
                }
            }
            cx.update_window(handle.into(), |_, window, cx| {
                runs.update(cx, |panel, cx| {
                    if changed {
                        panel.manager.logs.push_back(LogLine {
                            stderr: false,
                            text: format!("log {ms}\n"),
                        });
                    }
                    if ms == 1001 {
                        panel.manager.outcome = Some(RunOutcome::Completed);
                    }
                    assert!(panel.poll_run(now + Duration::from_millis(ms), false, window, cx));
                    if changed && ms != 1001 {
                        assert_eq!(panel.manager.progress.phase, format!("phase {ms}"));
                    }
                    match ms {
                        0..500 => assert!(panel.log_text.is_empty()),
                        500 | 999 => assert!(panel.log_text.ends_with("log 499\n")),
                        1000 => assert!(panel.log_text.ends_with("log 999\n")),
                        _ => {
                            assert!(panel.log_text.ends_with("log 1001\n"));
                            assert!(panel.manager.progress.finished);
                        }
                    }
                    assert_eq!(panel.log.read(cx).value(), panel.log_text);
                });
            })
            .unwrap();
            cx.run_until_parked();
            let expected = match ms {
                0..500 => 0,
                500..1000 => 1,
                1000 => 2,
                _ => 3,
            };
            assert_eq!(events.get(), expected, "Progress notifications at {ms} ms");
            assert_eq!(notifications.get(), expected, "UI notifications at {ms} ms");
        }
    }

    #[gpui_kit::test]
    async fn log_pagination_leaving_follow_and_filtering_reject_stale_reads(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let (request, jobs, _) = crate::desktop::logs::tests::fixture(directory.path(), 1001);
        let (mut manager, _) = crate::desktop::runner::tests::polling_fixture();
        manager.request = Some(request);
        for job in jobs {
            manager.progress.apply(crate::progress::Record {
                version: 1,
                elapsed_ms: 0,
                event: crate::progress::Event::Queued { job },
            });
        }
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.main_tab = 1;
                this.runs.update(cx, |panel, cx| {
                    panel.manager = manager;
                    panel.tab = 3;
                    panel.refresh_log(window, cx);
                });
            });
        })
        .unwrap();
        cx.wait_for(
            handle.into(),
            std::time::Duration::from_secs(10),
            |_, cx| {
                let panel = view.read(cx).runs.read(cx);
                !panel.archive_loading
                    && panel
                        .archive_page
                        .as_ref()
                        .is_some_and(|page| page.offset == 502)
            },
        )
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(view.read(cx).runs.read(cx).running());
            assert_eq!(
                view.read(cx)
                    .runs
                    .read(cx)
                    .archive_page
                    .as_ref()
                    .unwrap()
                    .records
                    .len(),
                500
            );
            assert!(window.try_find("run-log-pagination").is_none());
            view.read(cx).runs.clone().update(cx, |panel, cx| {
                panel.archive_key = None;
                panel.refresh_log(window, cx);
            });
            assert!(view.read(cx).runs.read(cx).archive_loading);
            window.click("log-follow-tail", cx);
        })
        .unwrap();
        cx.wait_for(
            handle.into(),
            std::time::Duration::from_secs(10),
            |_, cx| {
                let panel = view.read(cx).runs.read(cx);
                !panel.archive_loading
                    && panel
                        .archive_page
                        .as_ref()
                        .is_some_and(|page| page.offset == 1000)
            },
        )
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let panel = view.read(cx).runs.read(cx);
            assert!(!panel.log_follow);
            assert_eq!(panel.archive_page.as_ref().unwrap().records.len(), 2);
            assert_eq!(
                window.find("run-log-page-number").label(),
                Some("Page 3 of 3")
            );
            assert!(panel.log_text.contains("#1000]"));
            assert!(!panel.log_text.contains("#999]"));
            window.within("run-log-pagination").click("prev", cx);
            assert!(view.read(cx).runs.read(cx).archive_loading);
            let search = view
                .read(cx)
                .runs
                .read(cx)
                .log_controls
                .read(cx)
                .search
                .clone();
            window.focus(&search.read(cx).focus_handle(cx), cx);
            window.input("success", cx);
        })
        .unwrap();
        cx.wait_for(
            handle.into(),
            std::time::Duration::from_secs(10),
            |_, cx| {
                let panel = view.read(cx).runs.read(cx);
                !panel.archive_loading
                    && panel
                        .archive_page
                        .as_ref()
                        .is_some_and(|page| page.matched == 1)
            },
        )
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let panel = view.read(cx).runs.read(cx);
            assert_eq!(panel.log_query.offset, 0);
            assert_eq!(panel.archive_page.as_ref().unwrap().offset, 0);
            assert_eq!(
                window.find("run-log-page-number").label(),
                Some("Page 1 of 1")
            );
            assert!(panel.log_text.contains("#1001]"));
            assert!(!panel.log_text.contains("#500]"));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn runs_open_site_reloads_latest_suite_publication(cx: &mut TestAppContext) {
        use crate::desktop::deployment::DeployTargets;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let output = root.join(".regressify/ui-test");
        let run = output.join("bitmaps_test/existing-run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(output.join("bitmaps_reference")).unwrap();
        let png = lodepng::encode32(&[255_u8; 4], 1, 1).unwrap();
        std::fs::write(run.join("home.png"), &png).unwrap();
        std::fs::write(output.join("bitmaps_reference/home.png"), &png).unwrap();
        std::fs::write(
            run.join("report.json"),
            serde_json::to_vec(&json!({
                "id": "ui-test", "tests": [{"status": "pass", "pair": {"label": "Home"}}]
            }))
            .unwrap(),
        )
        .unwrap();
        let (handle, view) = super::super::tests::editor_at_size(
            cx,
            &root,
            gpui_kit::size(gpui_kit::px(1080.), gpui_kit::px(720.)),
        );
        let path = cx.update(|cx| {
            DeployTargets::path_in(&root, view.read(cx).preferences_path.as_ref().unwrap()).unwrap()
        });
        let settings: DeployTargets = serde_json::from_value(json!({
            "targets": [{"id": "old", "name": "Old"}, {"id": "latest", "name": "Latest"}],
            "selected": "old"
        }))
        .unwrap();
        settings.save(&path).unwrap();
        for (target, url) in settings
            .targets
            .iter()
            .zip(["https://older.example.com/", "https://latest.example.com/"])
        {
            DeployTargets::record_report_publication(&path, target, Some(url), &["ui-test".into()])
                .unwrap();
        }
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.close_workspace(window, cx);
                this.load_workspace(root.clone(), window, cx);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(5), |_, cx| {
            !view.read(cx).busy
                && view
                    .read(cx)
                    .runs
                    .read(cx)
                    .suite_site_url("ui-test")
                    .is_some()
                && view.read(cx).runs.read(cx).can_open_saved_suite("ui-test")
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.main_tab = 1;
                this.runs.update(cx, |panel, cx| {
                    panel.select_all(true, cx);
                    assert_eq!(
                        panel.selected_site_url().as_deref(),
                        Some("https://latest.example.com/suites/ui-test")
                    );
                    assert!(panel.suite_site_url("missing").is_none());
                });
                cx.notify();
            });
            for dark in [false, true] {
                super::super::design::apply_theme(dark, Some(window), cx);
                window.render_frame(cx);
                assert_eq!(window.find("run-open-site").label(), Some("Open Site"));
                let report = window.find("activity-view-results").bounds();
                let site = window.find("run-open-site").bounds();
                assert_eq!(report.origin.y, site.origin.y);
                assert!(report.origin.x + report.size.width <= site.origin.x);
                assert!(site.origin.x + site.size.width <= gpui_kit::px(1080.));
                window.within("run-tabs").click(1_usize, cx);
                window.render_frame(cx);
                assert!(window.find("saved-report-ui-test").visible());
                assert!(window.find("saved-site-ui-test").visible());
                let report = window.find("saved-report-ui-test").bounds();
                let site = window.find("saved-site-ui-test").bounds();
                assert_eq!(report.origin.y, site.origin.y);
                assert!(report.origin.x + report.size.width <= site.origin.x);
            }
            view.update(cx, |this, cx| {
                this.runs.update(cx, |panel, cx| {
                    panel.suites[0].name = "different-file-name".into();
                    panel.select_all(true, cx);
                    assert_eq!(
                        panel.selected_site_url().as_deref(),
                        Some("https://latest.example.com/suites/ui-test")
                    );
                    panel.suites[0].name = "ui-test".into();
                    panel.select_all(true, cx);
                });
            });
        })
        .unwrap();
        std::fs::write(&path, b"{invalid").unwrap();
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.runs
                    .update(cx, |panel, cx| panel.refresh_artifacts(cx))
            })
        });
        cx.wait_for(handle.into(), std::time::Duration::from_secs(5), |_, cx| {
            view.read(cx)
                .runs
                .read(cx)
                .suite_site_url("ui-test")
                .is_none()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("run-open-site").is_none());
            assert!(window.try_find("saved-site-ui-test").is_none());
            assert!(window.find("activity-view-results").visible());
            assert!(
                view.read(cx)
                    .runs
                    .read(cx)
                    .status
                    .contains("Cannot load deployed sites")
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn reopening_recovers_legacy_results_even_with_corrupt_activity(cx: &mut TestAppContext) {
        use crate::desktop::runner::RunSnapshot;
        for corrupt in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            let (handle, view) = super::super::tests::editor(cx, directory.path());
            let (path, bytes) = cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, cx| {
                    let suite = this.session.suites[0].clone();
                    let scenario = &suite.scenarios[0];
                    let output = root.join(".regressify").join(&suite.output_id);
                    let run = output.join("bitmaps_test/existing-run");
                    std::fs::create_dir_all(&run).unwrap();
                    std::fs::create_dir_all(output.join("bitmaps_reference")).unwrap();
                    let png = lodepng::encode32(&[255_u8; 4], 1, 1).unwrap();
                    let tests = [("phone", false), ("tablet", false), ("desktop", true)].map(|(viewport, passed)| {
                        let filename = format!("{}_{viewport}__FINAL.png", scenario.id);
                        std::fs::write(run.join(&filename), &png).unwrap();
                        std::fs::write(output.join("bitmaps_reference").join(&filename), &png).unwrap();
                        json!({ "status": if passed { "pass" } else { "fail" }, "pair": {
                            "fileName": filename, "scenarioId": scenario.id, "scenarioLabel": "Restored Home",
                            "engineErrorMsg": if passed { None } else { Some("Dimensions differ") }
                        } })
                    });
                    let path = run.join("report.json");
                    let bytes = serde_json::to_vec(&json!({ "id": suite.output_id, "tests": tests })).unwrap();
                    std::fs::write(&path, &bytes).unwrap();
                    if corrupt {
                        let saved = RunSnapshot::path_in(&root, this.preferences_path.as_ref().unwrap()).unwrap();
                        std::fs::create_dir_all(saved.parent().unwrap()).unwrap();
                        std::fs::write(saved, b"{broken").unwrap();
                    }
                    this.close_workspace(window, cx);
                    this.load_workspace(root.clone(), window, cx);
                    (path, bytes)
                })
            }).unwrap();
            cx.wait_for(handle.into(), std::time::Duration::from_secs(5), |_, cx| {
                !view.read(cx).busy && view.read(cx).runs.read(cx).manager.request.is_some()
            })
            .await;
            cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.main_tab = 1;
                    cx.notify();
                });
                window.render_frame(cx);
                let panel = view.read(cx).runs.read(cx);
                assert!(panel.manager.recovered_from_reports);
                assert_eq!(panel.manager.persistence_error.is_some(), corrupt);
                assert!(!panel.running());
                assert_eq!(panel.selected_count(), 1);
                assert_eq!(panel.selected_scenario_count(), 1);
                assert_eq!(panel.manager.progress.scenarios.len(), 3);
                assert!(panel.manager.progress.logs.is_empty());
                assert_eq!(
                    window.find("run-outcome-heading").label(),
                    Some("2 failed, 1 passed")
                );
                assert_eq!(
                    window.find("run-command-context").label(),
                    Some("Saved comparison results")
                );
                assert_eq!(window.find("run-elapsed").label(), Some("--:--"));
                assert_eq!(window.find("metric-failed").label(), Some("Failed: 2"));
                assert_eq!(window.find("metric-passed").label(), Some("Passed: 1"));
                assert_eq!(window.try_find("run-persistence-error").is_some(), corrupt);
                window.click("activity-view-results", cx);
            })
            .unwrap();
            cx.wait_for(
                handle.into(),
                std::time::Duration::from_secs(5),
                |window, _| {
                    window
                        .try_find("capture-heading")
                        .is_some_and(|heading| heading.label() == Some("Restored Home"))
                },
            )
            .await;
            cx.update_window(handle.into(), |_, _, cx| {
                let panel = view.read(cx).runs.read(cx);
                assert!(!panel.running());
                assert_eq!(
                    panel.manager.request.as_ref().unwrap().options.run_id,
                    "existing-run"
                );
            })
            .unwrap();
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }

    #[gpui_kit::test]
    async fn reopening_workspace_restores_activity_selection_logs_and_output_settings(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::runner::RunSnapshot;
        use crate::progress::{Event, Job, Record};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                let suite = this.session.suites[0].clone();
                let scenario = suite.scenarios[0].clone();
                let path =
                    RunSnapshot::path_in(&root, this.preferences_path.as_ref().unwrap()).unwrap();
                this.runs.update(cx, |panel, _| {
                    panel.manager.request = Some(RunRequest {
                        root: root.clone(),
                        command: RunCommand::Test,
                        options: RunOptions {
                            run_id: "previous-run".into(),
                            regressify_dir: "saved-output".into(),
                            suites: vec![suite.name.clone()],
                            scenarios: vec![crate::cli::args::ScenarioFilter {
                                suite: suite.name.clone(),
                                scenario: scenario.id.clone(),
                            }],
                            ..Default::default()
                        },
                    });
                    for (id, viewport) in [(1, "phone"), (2, "tablet"), (3, "desktop")] {
                        let capture = format!("{}_{viewport}__FINAL", scenario.id);
                        for event in [
                            Event::Queued {
                                job: Job {
                                    id,
                                    suite_id: suite.output_id.clone(),
                                    suite_name: suite.name.clone(),
                                    scenario_id: scenario.id.clone(),
                                    label: scenario.label.clone(),
                                    viewport: viewport.into(),
                                    captures: vec![capture.clone()],
                                },
                            },
                            Event::Started { id },
                            Event::Compared {
                                suite_id: suite.output_id.clone(),
                                capture,
                                label: scenario.label.clone(),
                                scenario_id: Some(scenario.id.clone()),
                                passed: id == 3,
                                error: (id != 3).then(|| "Dimensions differ".into()),
                            },
                        ] {
                            panel.manager.progress.apply(Record {
                                version: 1,
                                elapsed_ms: 27_100,
                                event,
                            });
                        }
                    }
                    panel.manager.progress.end(false, None);
                    panel.manager.outcome = Some(RunOutcome::Completed);
                    panel.manager.snapshot().unwrap().save(&path).unwrap();
                });
                this.close_workspace(window, cx);
                assert!(this.runs.read(cx).manager.request.is_none());
                this.load_workspace(root.clone(), window, cx);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(5), |_, cx| {
            !view.read(cx).busy && view.read(cx).runs.read(cx).manager.request.is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.main_tab = 1;
                cx.notify();
            });
            window.render_frame(cx);
            let panel = view.read(cx).runs.read(cx);
            assert!(!panel.running());
            assert_eq!(panel.selected_count(), 1);
            assert_eq!(panel.selected_scenario_count(), 1);
            assert!(panel.selection.has_scenario_selection());
            assert_eq!(panel.options["runId"], "");
            assert_eq!(panel.output_root().unwrap(), root.join("saved-output"));
            assert_eq!(
                panel.manager.request.as_ref().unwrap().options.run_id,
                "previous-run"
            );
            assert_eq!(panel.manager.progress.elapsed_ms, 27_100);
            assert_eq!(panel.manager.progress.logs.len(), 9);
            assert_eq!(
                window.find("run-outcome-heading").label(),
                Some("2 failed, 1 passed")
            );
            assert_eq!(
                window.find("metric-total").label(),
                Some("Screenshot comparisons: 3")
            );
            assert_eq!(window.find("metric-passed").label(), Some("Passed: 1"));
            assert_eq!(window.find("metric-failed").label(), Some("Failed: 2"));
            assert!(window.find("activity-view-results").visible());
            assert_eq!(window.find("run-status").label(), Some("Completed"));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn different_suite_selection_does_not_offer_an_unrelated_previous_report(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let (_, view) = super::super::tests::editor(cx, directory.path());
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.runs.update(cx, |panel, cx| {
                    panel.manager.request = Some(RunRequest {
                        root: directory.path().canonicalize().unwrap(),
                        command: RunCommand::Test,
                        options: RunOptions {
                            suites: vec!["previous-suite".into()],
                            run_id: "old-run".into(),
                            ..Default::default()
                        },
                    });
                    panel.executed_suites =
                        vec![("previous-suite".into(), "previous-output".into())];
                    panel.manager.outcome = Some(RunOutcome::Completed);
                    panel.select_all(true, cx);
                    assert!(!panel.can_open_report());
                    assert!(panel.executed_report_available());
                    panel.select_all(false, cx);
                    assert!(panel.can_open_report());
                })
            })
        });
    }

    #[gpui_kit::test]
    fn removal_only_deployments_resolve_without_suite_filters(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, _, cx| {
            view.update(cx, |this, cx| {
                let suites = this.session.suites.clone();
                let mut intent = this.runs.update(cx, |panel, cx| {
                    panel.select_all(false, cx);
                    panel.removal_intent(cx).unwrap()
                });
                assert!(intent.suite_names.is_empty());
                intent.request.command = RunCommand::Deploy;
                let job = crate::desktop::deployment::DeploymentJob {
                    target: serde_json::from_value(serde_json::json!({ "id": "qa", "name": "QA" }))
                        .unwrap(),
                    suite_ids: Vec::new(),
                    catalog_ids: suites.iter().map(|suite| suite.output_id.clone()).collect(),
                    partial_confirmed: false,
                    force: false,
                    mode: crate::desktop::deployment::DeploymentMode::Update,
                    remove_ids: vec!["gone".into()],
                };
                intent.request.options.deployment = Some(job.clone());
                let request = intent.resolve(&suites).unwrap();
                assert!(request.options.suites.is_empty());
                request.arguments().unwrap();
                // Without suites to remove, an empty selection is still rejected.
                let mut empty = intent.clone();
                empty.request.options.deployment =
                    Some(crate::desktop::deployment::DeploymentJob {
                        remove_ids: Vec::new(),
                        ..job
                    });
                assert!(empty.resolve(&suites).is_err());
            });
        })
        .unwrap();
    }

    #[test]
    fn site_scans_follow_targets_the_loaded_settings_still_hold() {
        use crate::desktop::deployment::DeployTargets;
        let target = |id: &str| {
            let mut target = crate::desktop::deployment::netlify::tests::target();
            target.id = id.into();
            target.name = id.into();
            target
        };
        let publications = DeployTargets {
            targets: vec![target("chosen"), target("selected")],
            selected: Some("selected".into()),
            ..Default::default()
        };
        let followed = |current: Option<&str>| {
            super::artifacts::followed_target(current.map(target).as_ref(), &publications)
                .map(|target| target.id)
        };
        // The target the Deploy dialog chose stays followed while it exists.
        assert_eq!(followed(Some("chosen")).as_deref(), Some("chosen"));
        // One from another workspace, or deleted, falls back to the saved selection.
        assert_eq!(followed(Some("elsewhere")).as_deref(), Some("selected"));
        assert_eq!(followed(None).as_deref(), Some("selected"));
        let none = DeployTargets::default();
        assert!(super::artifacts::followed_target(Some(&target("chosen")), &none).is_none());
    }

    #[gpui_kit::test]
    fn closing_a_workspace_forgets_its_site(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.runs.update(cx, |panel, cx| {
                    panel.set_site_for_test(
                        Some(crate::desktop::deployment::netlify::tests::target()),
                        Some(Default::default()),
                        super::SiteSource::Site,
                        Default::default(),
                        cx,
                    );
                    panel.clear(window, cx);
                    assert!(panel.site_target().is_none());
                    assert!(panel.site_suites().is_none());
                    assert_eq!(*panel.site_source(), super::SiteSource::Unknown);
                });
            });
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn sites_published_before_are_never_shown_as_empty(cx: &mut TestAppContext) {
        use super::SiteSource;
        use crate::desktop::deployment::{DeployTargets, DeploymentProvider, site_state};
        let directory = tempfile::tempdir().unwrap();
        let (_handle, view) = super::super::tests::editor(cx, directory.path());
        let settings = directory.path().join("deployments/workspace.json");
        // Answers every request with 404, as a site that serves no report (or a gateway
        // hiding one) does.
        let site = crate::test::browser_tests::Site::new(|request| {
            let _ = request.respond(tiny_http::Response::empty(404));
        })
        .unwrap();
        let mut private = crate::desktop::deployment::netlify::tests::target();
        private.id = "private".into();
        let mut public = private.clone();
        public.id = "public".into();
        public.name = "Public QA".into();
        // Saved settings only accept HTTPS URLs; the site read uses the target it is given,
        // which points at the local test server.
        let saved_public = public.clone();
        public.published_url = Some(format!("{}/reports/", site.origin));
        DeployTargets {
            targets: vec![private.clone(), saved_public.clone()],
            selected: Some(private.id.clone()),
            ..Default::default()
        }
        .save(&settings)
        .unwrap();
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.runs.update(cx, |panel, cx| {
                    panel.set_deployment_storage_path(settings.clone(), cx)
                })
            })
        });
        let read = |cx: &mut TestAppContext, target: &crate::desktop::deployment::DeployTarget| {
            cx.update(|cx| {
                view.update(cx, |this, cx| {
                    this.runs.update(cx, |panel, cx| {
                        panel.refresh_site(Some(target.clone()), true, cx)
                    })
                })
            });
        };
        let settle = async |cx: &mut TestAppContext| {
            for _ in 0..300 {
                cx.run_until_parked();
                let loading = cx
                    .update(|cx| *view.read(cx).runs.read(cx).site_source() == SiteSource::Loading);
                if !loading {
                    break;
                }
                cx.background_executor
                    .timer(std::time::Duration::from_millis(10))
                    .await;
            }
            cx.run_until_parked();
            cx.update(|cx| {
                let runs = view.read(cx).runs.read(cx);
                (
                    runs.site_source().clone(),
                    runs.site_suites().map(|suites| suites.suites.len()),
                )
            })
        };

        // Never published: an unread private site is empty.
        read(cx, &private);
        assert_eq!(settle(cx).await, (SiteSource::Local, Some(0)));
        // A private publication left only its marker: the suites are unknown, not absent.
        site_state::mark_published(&settings, &private.id).unwrap();
        read(cx, &private);
        let (source, suites) = settle(cx).await;
        assert!(matches!(source, SiteSource::Unavailable(_)), "{source:?}");
        assert_eq!(suites, None);

        // A public site that serves nothing and was never published here is empty.
        read(cx, &public);
        assert_eq!(settle(cx).await, (SiteSource::Site, Some(0)));
        // Once published, an empty answer falls back to the state published from here.
        DeployTargets::record_report_publication(
            &settings,
            &saved_public,
            Some("https://qa-reports.netlify.app/"),
            &["shop".into()],
        )
        .unwrap();
        site_state::save_local(
            &settings,
            &public.id,
            &site_state::tests::state(DeploymentProvider::Netlify, &["shop"]),
        )
        .unwrap();
        read(cx, &public);
        assert_eq!(settle(cx).await, (SiteSource::Local, Some(1)));
        // Without a local copy either, the suites are unknown.
        site_state::forget_local(&settings, &public.id).unwrap();
        read(cx, &public);
        let (source, suites) = settle(cx).await;
        assert!(matches!(source, SiteSource::Unavailable(_)), "{source:?}");
        assert_eq!(suites, None);
    }

    #[gpui_kit::test]
    async fn suite_statuses_follow_recorded_inputs_after_approvals(cx: &mut TestAppContext) {
        use crate::desktop::deployment::{
            DeployTargets, DeploymentProvider,
            site_state::{self, SuiteInputs},
        };
        let directory = tempfile::tempdir().unwrap();
        let (_handle, view) = super::super::tests::editor(cx, directory.path());
        // Results of a published suite, and the records its publication left.
        let baseline = directory
            .path()
            .join(".regressify/ui-test/bitmaps_reference/home.png");
        std::fs::create_dir_all(baseline.parent().unwrap()).unwrap();
        std::fs::write(&baseline, "published").unwrap();
        let settings = directory.path().join("deployments/workspace.json");
        let mut target = crate::desktop::deployment::netlify::tests::target();
        target.id = "qa".into();
        DeployTargets {
            targets: vec![target],
            selected: Some("qa".into()),
            ..Default::default()
        }
        .save(&settings)
        .unwrap();
        let state = site_state::tests::state(DeploymentProvider::Netlify, &["ui-test"]);
        site_state::save_local(&settings, "qa", &state).unwrap();
        let output = directory.path().join(".regressify");
        let inputs = std::collections::BTreeMap::from([(
            "ui-test".to_owned(),
            crate::snapshot::suite_input_fingerprint(&output, "ui-test").unwrap(),
        )]);
        let records = SuiteInputs::default().after(&state, &inputs).unwrap();
        site_state::save_inputs(&settings, "qa", &records).unwrap();
        let status = |cx: &mut TestAppContext| {
            cx.update(|cx| view.read(cx).runs.read(cx).site_status("ui-test"))
        };

        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.runs.update(cx, |panel, cx| {
                    panel.set_deployment_storage_path(settings.clone(), cx)
                })
            })
        });
        for _ in 0..200 {
            cx.run_until_parked();
            if status(cx) == Some(false) {
                break;
            }
            cx.background_executor
                .timer(std::time::Duration::from_millis(10))
                .await;
        }
        assert_eq!(status(cx), Some(false), "published inputs read as deployed");

        // An approval rewrites the baseline without a new run; the next scan reports it.
        std::fs::write(&baseline, "approved").unwrap();
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.runs
                    .update(cx, |panel, cx| panel.refresh_artifacts(cx))
            })
        });
        for _ in 0..200 {
            cx.run_until_parked();
            if status(cx) == Some(true) {
                break;
            }
            cx.background_executor
                .timer(std::time::Duration::from_millis(10))
                .await;
        }
        assert_eq!(
            status(cx),
            Some(true),
            "approved inputs read as newer than the site"
        );
    }

    #[gpui_kit::test]
    async fn saved_capture_report_request_compares_without_recapturing(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join(".regressify/ui-test");
        for path in [
            output.join("bitmaps_reference/home.png"),
            output.join("bitmaps_test/stored-capture/home.png"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            lodepng::encode32_file(path, &[255_u8; 4], 1, 1).unwrap();
        }
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        let requests = Rc::new(RefCell::new(Vec::<RunIntent>::new()));
        let received = requests.clone();
        let _subscription = cx.update(|cx| {
            view.update(cx, |this, cx| {
                cx.subscribe(&this.runs, move |_, _, event, _| {
                    if let RunEvent::Requested(intent) = event {
                        received.borrow_mut().push((**intent).clone());
                    }
                })
            })
        });
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            view.read(cx).runs.read(cx).can_open_saved_suite("ui-test")
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("duplicate-item", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            view.read(cx).runs.read(cx).can_open_saved_suite("ui-test")
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("suite-open-report", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].request.command, RunCommand::Compare);
        assert_eq!(requests[0].request.options.run_id, "stored-capture");
        assert_eq!(requests[0].suite_names, vec!["ui-test"]);
        assert!(requests[0].review_after_run);
        cx.update(|cx| assert!(!view.read(cx).runs.read(cx).running()));
    }

    #[gpui_kit::test]
    async fn artifact_detection_tracks_output_changes_and_multiple_suite_baselines(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let reference = directory
            .path()
            .join(".regressify/ui-test/bitmaps_reference/home.png");
        std::fs::create_dir_all(reference.parent().unwrap()).unwrap();
        lodepng::encode32_file(reference, &[255_u8; 4], 1, 1).unwrap();
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            view.read(cx).runs.read(cx).has_reference("ui-test")
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.runs.update(cx, |panel, cx| {
                    panel.select_all(true, cx);
                    let mut suites = panel.suites.clone();
                    let mut second = suites[0].clone();
                    second.name = "another-suite".into();
                    second.output_id = "another-output".into();
                    suites.push(second);
                    panel.update_catalog(suites, None, cx);
                    panel.select_all(true, cx);
                    panel.choose(RunCommand::Reference, window, cx);
                });
            })
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            !view.read(cx).runs.read(cx).artifacts.loading
        })
        .await;
        cx.update_window(handle.into(), |_, _, cx| {
            view.update(cx, |this, cx| {
                this.runs.update(cx, |panel, cx| {
                    assert!(!panel.can_test_selection());
                    panel.toggle_suite("another-suite", false, cx);
                    assert!(panel.can_test_selection());
                    let field = panel
                        .bindings
                        .iter()
                        .find(|binding| binding.field.key == "siteEnv")
                        .unwrap()
                        .field
                        .clone();
                    panel.change(&field, "staging", cx);
                    assert!(!panel.has_reference("ui-test"));
                    panel.change(&field, "", cx);
                    panel.change(&field, "staging", cx);
                })
            })
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            !view.read(cx).runs.read(cx).artifacts.loading
        })
        .await;
        cx.update(|cx| {
            let panel = view.read(cx).runs.read(cx);
            assert_eq!(
                panel.output_root().unwrap(),
                directory
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join(".regressify__staging")
            );
            assert!(!panel.has_reference("ui-test"));
        });
    }

    #[gpui_kit::test]
    async fn baseline_detection_controls_test_and_revalidates_before_execution(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.main_tab = 1;
                this.runs.update(cx, |panel, cx| panel.select_all(true, cx));
            });
            window.render_frame(cx);
            window.click("run-test", cx);
            assert!(!view.read(cx).runs.read(cx).running());
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            !view.read(cx).runs.read(cx).artifacts.loading
        })
        .await;
        cx.update(|cx| {
            let panel = view.read(cx).runs.read(cx);
            assert!(!panel.can_test_selection());
            assert!(panel.can_request(RunCommand::Reference));
            assert!(!view.read(cx).can_test_open_suite(cx));
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.within("workbench-tabs").click(0_usize, cx);
            window.click("suite-run-test", cx);
            window.click("scenario-run-test", cx);
            window.click(("sidebar-scenario-test", 0_usize), cx);
            assert_eq!(view.read(cx).main_tab, 0);
            assert!(!view.read(cx).runs.read(cx).running());
            window.within("workbench-tabs").click(1_usize, cx);
        })
        .unwrap();
        let reference = directory
            .path()
            .join(".regressify/ui-test/bitmaps_reference/home.png");
        std::fs::create_dir_all(reference.parent().unwrap()).unwrap();
        lodepng::encode32_file(&reference, &[255_u8; 4], 1, 1).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("refresh-run-artifacts", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            view.read(cx).runs.read(cx).can_test_selection()
        })
        .await;
        cx.update(|cx| assert!(view.read(cx).can_test_open_suite(cx)));
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                let suites = this.session.suites.clone();
                this.runs.update(cx, |panel, cx| {
                    let intent = panel.prepare_intent(cx).unwrap();
                    assert!(intent.resolve_for_execution(&suites).is_ok());
                    std::fs::remove_file(&reference).unwrap();
                    assert!(
                        intent
                            .resolve_for_execution(&suites)
                            .unwrap_err()
                            .to_string()
                            .contains("Run Reference")
                    );
                    panel.refresh_artifacts(cx);
                });
            })
        });
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            !view.read(cx).runs.read(cx).artifacts.loading
        })
        .await;
        cx.update(|cx| assert!(!view.read(cx).runs.read(cx).can_test_selection()));
    }

    #[gpui_kit::test]
    fn open_report_button_uses_recorded_capture_scope(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = super::super::tests::editor(cx, directory.path());
        let requests = Rc::new(RefCell::new(Vec::<RunIntent>::new()));
        let received = requests.clone();
        let _subscription = cx.update(|cx| {
            view.update(cx, |this, cx| {
                cx.subscribe(&this.runs, move |_, _, event, _| {
                    if let RunEvent::Requested(intent) = event {
                        received.borrow_mut().push((**intent).clone());
                    }
                })
            })
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("duplicate-item", cx);
            view.update(cx, |this, cx| {
                let suite = this.session.suites[0].clone();
                this.main_tab = 1;
                this.runs.update(cx, |panel, cx| {
                    panel.manager.request = Some(RunRequest {
                        root: directory.path().canonicalize().unwrap(),
                        command: RunCommand::Test,
                        options: RunOptions {
                            regressify_dir: ".captured-output".into(),
                            run_id: "captured-run".into(),
                            suites: vec![suite.name.clone()],
                            scenarios: vec![crate::cli::args::ScenarioFilter {
                                suite: suite.name.clone(),
                                scenario: suite.scenarios[0].id.clone(),
                            }],
                            skip_compare: true,
                            ..Default::default()
                        },
                    });
                    panel.executed_suites = vec![(suite.name, suite.output_id)];
                    panel.manager.outcome = Some(RunOutcome::Completed);
                    panel.options["regressifyDir"] = json!(".changed-output");
                    panel.options["runId"] = json!("changed-run");
                    panel.selection.clear();
                    cx.notify();
                });
            });
            window.render_frame(cx);
            window.within("run-tabs").click(3_usize, cx);
            window.click("activity-view-results", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        let intent = &requests[0];
        assert!(intent.review_after_run);
        assert_eq!(intent.request.command, RunCommand::Compare);
        assert_eq!(intent.request.options.regressify_dir, ".captured-output");
        assert_eq!(intent.request.options.run_id, "captured-run");
        assert_eq!(intent.suite_names, vec!["ui-test"]);
        assert_eq!(intent.request.options.scenarios.len(), 1);
        cx.update(|cx| {
            assert!(view.read(cx).session.dirty());
            assert!(!view.read(cx).runs.read(cx).running());
        });
    }
}
