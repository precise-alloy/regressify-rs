//! Native workbench. Domain edits stay in the headless desktop models.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::{Result, anyhow};
use futures::StreamExt;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    form::{Field as FormField, Form},
    input::{
        Editor as CodeEditor, EditorState, Input, InputEvent, InputState, NumberInput,
        NumberInputEvent, StepAction, Textarea, TextareaState,
    },
    menu::DropdownMenu,
    resizable::{h_resizable, resizable_panel},
    scroll::ScrollableElement,
    select::{SearchableVec, SelectEvent, SelectState},
    tab::{Tab, TabBar},
    *,
};
use gpui_kit::{prelude::*, *};
use serde_json::{Value, json};

use super::{
    document::{ACTIONS, Document, DocumentKind, Format},
    instance::{DesktopInstance, OpenInstance},
    schema::{self, Field, FieldKind},
    session::{Selection, Session},
    workspace::{self, Entry, OpenDocument, Preferences, display_path},
};

mod choices;
mod cleanup;
mod commands;
mod deployment;
mod design;
mod files;
mod git;
mod landing;
mod lists;
mod log_controls;
mod navigation;
mod overview;
mod reports;
mod runs;
mod sequence;
mod source;
mod tokens;
mod updates;

#[cfg(all(test, feature = "gui-tests"))]
mod tests;

actions!(
    regressify_desktop,
    [
        SaveDocument,
        UndoDocument,
        RedoDocument,
        OpenWorkspace,
        AddScenario,
        AddViewport,
        DuplicateItem,
        MoveItemUp,
        MoveItemDown,
        FocusNavigator,
        FocusSettings,
        ToggleNavigator,
        CompleteSource,
        OpenDeployment,
        OpenDeploymentTokens,
        OpenLandingApps,
        OpenGit,
        ShowAbout,
        OpenCommands,
        SelectAllListItems,
        ClearListSelection,
        ToggleListItem,
        OpenListItem
    ]
);

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
struct AddBrowserAction {
    kind: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
struct SelectView {
    index: usize,
}

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
struct RunSuiteCommand {
    index: usize,
}

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
struct RunScenarioCommand {
    index: usize,
    scenario: usize,
}

enum Control {
    Input(Entity<InputState>),
    Textarea(Entity<TextareaState>),
    Boolean,
    Choice(Entity<SelectState<choices::ValueChoices>>),
}

struct Binding {
    field: Field,
    control: Control,
}

pub struct Workbench {
    app_menu_bar: Entity<gpui_kit::component::menu::AppMenuBar>,
    app_menu_focus: FocusHandle,
    app_menu_flags: Vec<bool>,
    session: Session,
    activation_task: Option<Task<()>>,
    _external_file_task: Task<()>,
    external_file_notice: Option<files::ExternalFileNotice>,
    workspace_instance: Option<DesktopInstance>,
    #[cfg(target_os = "windows")]
    taskbar: Option<super::jump_list::WindowTaskbar>,
    source: Entity<EditorState>,
    source_focus_restore: Option<(PathBuf, PathBuf)>,
    source_feedback: source::SourceFeedback,
    source_catalog: Option<std::sync::Arc<crate::desktop::completion::CompletionCatalog>>,
    resolved: Entity<EditorState>,
    search: Entity<InputState>,
    bindings: Vec<Binding>,
    list_choice_pickers: BTreeMap<String, choices::ListChoicePicker>,
    choice_binding_scope: Option<(PathBuf, Selection)>,
    _subscriptions: Vec<Subscription>,
    field_subscriptions: Vec<Subscription>,
    field_errors: BTreeMap<String, String>,
    focus: FocusHandle,
    value: Value,
    effective: Value,
    workspace_value: Value,
    tab: usize,
    advanced: bool,
    busy: bool,
    /// A failed update handoff lost this window's workspace to another process. `busy` stays set
    /// (no edits or new operations) but, unlike a real operation, this must not block closing:
    /// nothing can be unsaved because the unsaved-work guards passed before the handoff.
    workspace_lost: bool,
    status: String,
    diagnostic: Option<String>,
    source_issues: Vec<lsp_types::Diagnostic>,
    source_issue_markdown: Vec<SharedString>,
    source_wrap: bool,
    source_line_numbers: bool,
    source_font_size: f32,
    source_font_size_input: Entity<InputState>,
    preferences: Preferences,
    preferences_path: Option<PathBuf>,
    preferences_saving: bool,
    preferences_pending: bool,
    close_after_preferences: bool,
    modal: Entity<files::ModalState>,
    runs: Entity<runs::RunPanel>,
    results: Entity<reports::ResultsPanel>,
    captures: Entity<reports::CaptureList>,
    main_tab: usize,
    overview: bool,
    overview_table: Entity<overview::OverviewTableState>,
    scenario_search: Entity<InputState>,
    suite_list: lists::ListState<PathBuf>,
    suite_menu_epoch: u64,
    scenario_list: lists::ListState<(PathBuf, usize)>,
    workspace_list: Entity<navigation::WorkspaceListState>,
    sequence_list: Entity<sequence::SequenceListState>,
    workspace_files_open: bool,
    selected_only: bool,
    field_search: Entity<InputState>,
    overrides_only: bool,
    collapsed_fields: BTreeSet<usize>,
    field_group_focus: [FocusHandle; 6],
    viewport_choices: choices::ViewportChoices,
    updates: Entity<updates::UpdatePanel>,
    deployment: Entity<deployment::DeploymentPanel>,
    tokens: Entity<tokens::TokenPanel>,
    landing: Entity<landing::LandingPanel>,
    git: Entity<git::GitPanel>,
    cleanup: Entity<cleanup::CleanupPanel>,
}

impl Workbench {
    /// `preferences_path` is injected so tests can run in parallel against
    /// per-test settings and never read the user's real preferences file.
    pub fn new(
        preferences_path: anyhow::Result<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let source = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .replaceable(true)
                .line_number(true)
        });
        let resolved = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("json")
                .line_number(true)
        });
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search suites and scenarios"));
        let source_subscription = cx.subscribe_in(&source, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                match this.capture_source_draft(cx) {
                    Ok(true) => {
                        this.defer_source_feedback(window, cx);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        this.source.update(cx, source::clear_source_popovers);
                        this.status = format!("{error:#}");
                    }
                }
                cx.notify();
            }
        });
        let search_subscription = cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify());
        let scenario_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find a scenario"));
        let scenario_search_subscription =
            cx.subscribe(&scenario_search, |_, _, _: &InputEvent, cx| cx.notify());
        let source_observer =
            cx.observe(&source, |this, state, cx| {
                let selection = state.read(cx).selected_range();
                if this.session.open.as_ref().is_some_and(|open| {
                    open.document.source_selection().as_ref() != Some(&selection)
                }) && this.source_matches_document(cx)
                    && let Some(open) = &mut this.session.open
                {
                    open.document.set_source_selection(selection);
                }
                cx.notify();
            });
        let appearance_subscription = cx.observe_window_appearance(window, |this, window, cx| {
            this.system_appearance_changed(window.appearance(), window, cx);
        });
        let activation_subscription = cx.observe_window_activation(window, |this, window, cx| {
            this.source_window_activation_changed(window, cx);
        });
        let field_search = cx.new(|cx| InputState::new(window, cx).placeholder("Find a setting"));
        let field_search_subscription =
            cx.subscribe(&field_search, |_, _, _: &InputEvent, cx| cx.notify());
        let preferences = preferences_path
            .as_ref()
            .map_err(|error| anyhow!("{error:#}"))
            .and_then(|path| Preferences::load(path));
        let status = preferences
            .as_ref()
            .err()
            .map(|error| format!("{error:#}"))
            .unwrap_or_else(|| "Ready".into());
        let preferences = preferences.unwrap_or_default();
        let source_font_size_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .mask_pattern(gpui_kit::component::input::MaskPattern::None)
                .default_value(preferences.source_font_size.to_string());
            state.set_step(None, window, cx);
            state
        });
        let source_font_size_subscription = cx.subscribe_in(
            &source_font_size_input,
            window,
            |this, state, event, window, cx| match event {
                InputEvent::Change => {
                    if let Ok(size) = state.read(cx).value().parse::<u8>() {
                        this.change_source_size(size, window, cx);
                    }
                }
                InputEvent::Blur => this.restore_source_size(window, cx),
                _ => {}
            },
        );
        let source_font_step_subscription = cx.subscribe_in(
            &source_font_size_input,
            window,
            |this, _, event: &NumberInputEvent, window, cx| {
                let NumberInputEvent::Step(action) = event;
                this.step_source_size(*action, window, cx);
            },
        );
        source.update(cx, |state, cx| {
            state.set_soft_wrap(preferences.source_wrap, window, cx);
            state.set_line_number(preferences.source_line_numbers, window, cx);
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let runs = cx.new(|cx| runs::RunPanel::new(window, cx));
        let git = cx.new(|cx| git::GitPanel::new(window, cx));
        let git_subscription =
            cx.subscribe_in(&git, window, |this, _, event, window, cx| match event {
                git::GitEvent::Changed => cx.notify(),
                git::GitEvent::Requested(request) => this.guard_git(request.clone(), window, cx),
            });
        let run_subscription =
            cx.subscribe_in(&runs, window, |this, _, event, window, cx| match event {
                runs::RunEvent::Requested(intent) => this.guard_run(intent.clone(), window, cx),
                runs::RunEvent::Finished => {
                    this.status = "Command finished".into();
                    if let Err(error) = this.session.refresh() {
                        this.status = format!("{error:#}");
                    }
                    this.sync_suites(cx);
                    let (approval, comparison) = {
                        let manager = &this.runs.read(cx).manager;
                        let approval = manager
                            .request
                            .as_ref()
                            .filter(|request| request.options.report_capture.is_some())
                            .map(|_| manager.outcome.clone());
                        let comparison = manager
                            .request
                            .as_ref()
                            .filter(|request| {
                                request.command == crate::desktop::runner::RunCommand::Compare
                                    && matches!(
                                        manager.outcome,
                                        Some(crate::desktop::runner::RunOutcome::Completed)
                                    )
                            })
                            .map(|request| {
                                let targets = manager
                                    .progress
                                    .suites
                                    .keys()
                                    .map(|suite_id| {
                                        (suite_id.clone(), request.options.run_id.clone())
                                    })
                                    .collect::<Vec<_>>();
                                (
                                    request.options.output_directory(&request.root),
                                    request.options.run_id.clone(),
                                    targets,
                                )
                            });
                        (approval, comparison)
                    };
                    if let Some(outcome) = approval {
                        let suites = this.session.suites.clone();
                        this.results.update(cx, |panel, cx| {
                            panel.set_suites(suites);
                            panel.approval_finished(outcome.as_ref(), window, cx);
                        });
                    } else if let Some((root, run_id, targets)) = comparison {
                        let suites = this.session.suites.clone();
                        this.results.update(cx, |panel, cx| {
                            panel.set_suites(suites);
                            if !run_id.is_empty() && !targets.is_empty() {
                                panel.show_saved_reports(root, targets, window, cx);
                            } else {
                                panel.show_run(root, run_id, window, cx);
                            }
                        });
                    } else {
                        this.refresh_results(window, cx);
                    }
                    this.git.update(cx, |panel, cx| {
                        panel.set_disabled(this.busy, cx);
                        panel.refresh(window, cx);
                    });
                    cx.notify();
                }
                runs::RunEvent::SelectionChanged => {
                    this.runs
                        .update(cx, |panel, cx| panel.refresh_artifacts_if_idle(cx));
                    cx.notify();
                }
                runs::RunEvent::ArtifactsChanged | runs::RunEvent::ProgressChanged => {
                    let disabled = this.busy || this.runs.read(cx).running();
                    this.git
                        .update(cx, |panel, cx| panel.set_disabled(disabled, cx));
                    this.results
                        .update(cx, |panel, cx| panel.set_disabled(disabled, cx));
                    cx.notify();
                }
                runs::RunEvent::ReviewResults { root, run_id } => {
                    this.main_tab = 2;
                    let suites = this.session.suites.clone();
                    this.results.update(cx, |panel, cx| {
                        panel.set_suites(suites);
                        panel.show_run(root.clone(), run_id.clone(), window, cx);
                    });
                    cx.notify();
                }
                runs::RunEvent::ReviewSavedResults { root, targets } => {
                    this.main_tab = 2;
                    let suites = this.session.suites.clone();
                    this.results.update(cx, |panel, cx| {
                        panel.set_suites(suites);
                        panel.show_saved_reports(root.clone(), targets.clone(), window, cx);
                    });
                    cx.notify();
                }
            });
        let results = cx.new(|cx| reports::ResultsPanel::new(window, cx));
        let captures = cx.new(|cx| reports::CaptureList::new(results.clone(), cx));
        let results_observer = cx.observe(&results, |_, _, cx| cx.notify());
        let cleanup = cx.new(|cx| cleanup::CleanupPanel::new(window, cx));
        let cleanup_observer = cx.observe(&cleanup, |_, _, cx| cx.notify());
        let results_subscription =
            cx.subscribe_in(&results, window, |this, _, event, window, cx| match event {
                reports::ResultEvent::Cleanup { root, suite } => {
                    this.open_cleanup(Some(root.clone()), suite.clone(), window, cx);
                }
                reports::ResultEvent::OpenSuite(entry) => {
                    this.open_location(entry.clone(), Selection::Root, window, cx)
                }
                reports::ResultEvent::ApproveRun(report) => {
                    if this.busy || this.runs.read(cx).running() {
                        return;
                    }
                    match this.runs.read(cx).report_approval_intent(report) {
                        Ok(intent) => this.guard_run(Box::new(intent), window, cx),
                        Err(error) => {
                            this.status = format!("{error:#}");
                            this.results.update(cx, |panel, cx| {
                                panel.approval_failed(format!("{error:#}"), cx)
                            });
                        }
                    }
                    cx.notify();
                }
            });
        let app_menu_bar = gpui_kit::component::menu::AppMenuBar::new(cx);
        let app_menu_release =
            cx.on_release(|this, cx| design::release_app_menu(&this.app_menu_bar, cx));
        let workspace_owner = cx.entity().downgrade();
        let workspace_list = cx.new(|cx| {
            navigation::WorkspaceListState::new(
                navigation::WorkspaceFiles::new(workspace_owner),
                window,
                cx,
            )
        });
        let sequence_owner = cx.entity().downgrade();
        let sequence_list = cx.new(|cx| {
            sequence::SequenceListState::new(
                sequence::SequenceRows::new(sequence_owner),
                window,
                cx,
            )
        });
        let overview_owner = cx.entity().downgrade();
        let overview_table = cx.new(|cx| {
            overview::OverviewTableState::new(
                overview::OverviewRows::new(overview_owner),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
            .col_resizable(false)
            .sortable(false)
        });
        Self {
            app_menu_bar,
            app_menu_focus: cx.focus_handle().tab_stop(false),
            app_menu_flags: Vec::new(),
            session: Session::default(),
            workspace_instance: None,
            activation_task: None,
            _external_file_task: Self::watch_external_file(window, cx),
            external_file_notice: None,
            #[cfg(target_os = "windows")]
            taskbar: None,
            source,
            source_focus_restore: None,
            source_feedback: Default::default(),
            source_catalog: None,
            resolved,
            search,
            bindings: Vec::new(),
            list_choice_pickers: BTreeMap::new(),
            choice_binding_scope: None,
            _subscriptions: vec![
                app_menu_release,
                source_subscription,
                source_font_size_subscription,
                source_font_step_subscription,
                search_subscription,
                run_subscription,
                field_search_subscription,
                source_observer,
                scenario_search_subscription,
                results_subscription,
                results_observer,
                cleanup_observer,
                appearance_subscription,
                activation_subscription,
                git_subscription,
            ],
            field_subscriptions: Vec::new(),
            field_errors: BTreeMap::new(),
            focus,
            value: Value::Null,
            effective: Value::Null,
            workspace_value: Value::Null,
            tab: 0,
            advanced: false,
            busy: false,
            workspace_lost: false,
            status,
            diagnostic: None,
            source_issues: Vec::new(),
            source_issue_markdown: Vec::new(),
            source_wrap: preferences.source_wrap,
            source_line_numbers: preferences.source_line_numbers,
            source_font_size: f32::from(preferences.source_font_size),
            source_font_size_input,
            preferences,
            preferences_path: preferences_path.ok(),
            preferences_saving: false,
            preferences_pending: false,
            close_after_preferences: false,
            modal: cx.new(|_| files::ModalState::default()),
            runs,
            results,
            captures,
            main_tab: 0,
            overview: true,
            overview_table,
            scenario_search,
            suite_list: lists::ListState::new(cx),
            suite_menu_epoch: 0,
            scenario_list: lists::ListState::new(cx),
            workspace_list,
            sequence_list,
            workspace_files_open: false,
            selected_only: false,
            field_search,
            overrides_only: false,
            collapsed_fields: BTreeSet::new(),
            field_group_focus: std::array::from_fn(|_| cx.focus_handle().tab_index(0)),
            viewport_choices: choices::ViewportChoices::default(),
            updates: cx.new(|_| updates::UpdatePanel::default()),
            deployment: cx.new(|cx| deployment::DeploymentPanel::new(window, cx)),
            tokens: cx.new(|cx| tokens::TokenPanel::new(window, cx)),
            landing: cx.new(|cx| landing::LandingPanel::new(window, cx)),
            git,
            cleanup,
        }
    }

    /// Dispatch workspace loads and saves away from the UI event loop.
    fn task<T: Send + 'static>(
        &mut self,
        message: &str,
        job: impl FnOnce() -> Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = message.into();
        self.sync_modal(cx);
        let task = cx.background_executor().spawn(async move { job() });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.busy = false;
                this.sync_modal(cx);
                match result {
                    Ok(value) => done(this, value, window, cx),
                    Err(error) => this.status = format!("{error:#}"),
                }
                this.sync_modal(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_workspace(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self
            .preferences_path
            .clone()
            .map(Ok)
            .unwrap_or_else(Preferences::path)
            .map(|path| path.with_file_name("instances"));
        let current = self
            .session
            .workspace
            .as_ref()
            .map(|workspace| workspace.root.clone());
        self.task(
            "Checking workspace...",
            move || {
                let root = workspace::Workspace::open(&path)?.root;
                if current.as_ref() == Some(&root) {
                    #[cfg(target_os = "windows")]
                    super::jump_list::record_workspace(&root);
                    return Ok(None);
                }
                let opened = DesktopInstance::open(&directory?, &root)?;
                #[cfg(target_os = "windows")]
                if matches!(opened, OpenInstance::Activated) {
                    super::jump_list::record_workspace(&root);
                }
                Ok(Some(opened))
            },
            |this, opened, window, cx| {
                match opened {
                    None => {
                        window.activate_window();
                        cx.activate(true);
                        this.status = "Workspace is already open in this window".into();
                    }
                    Some(OpenInstance::Activated) => {
                        this.status = "Switched to the existing workspace window".into();
                    }
                    Some(OpenInstance::Owned(instance)) => {
                        if this.runs.read(cx).running() {
                            this.status =
                                "Cancel the active command before switching workspaces".into();
                        } else if this.unsaved(cx) {
                            let path = instance.root().to_path_buf();
                            drop(instance);
                            this.guard(files::Pending::Workspace(path), window, cx);
                        } else {
                            this.load_owned_workspace(instance, window, cx);
                        }
                    }
                }
                cx.notify();
            },
            window,
            cx,
        );
    }

    fn load_owned_workspace(
        &mut self,
        instance: DesktopInstance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let preferences = self.preferences_path.clone();
        self.task(
            "Opening workspace...",
            move || {
                let mut session = Session::default();
                session.connect(instance.root())?;
                let workspace_value = session
                    .workspace_value()
                    .ok()
                    .flatten()
                    .unwrap_or(Value::Null);
                let deployment_path = preferences
                    .as_ref()
                    .map(|path| super::deployment::DeployTargets::path_in(instance.root(), path))
                    .transpose()?;
                let activity_path = preferences
                    .ok_or_else(|| anyhow!("Desktop preferences path unavailable"))
                    .and_then(|path| super::runner::RunSnapshot::path_in(instance.root(), &path));
                let activity = match &activity_path {
                    Ok(path) => super::runner::RunSnapshot::load(path, instance.root()),
                    Err(error) => Err(anyhow!("{error:#}")),
                };
                let activity_warning = activity
                    .as_ref()
                    .err()
                    .map(|error| format!("Could not restore saved run activity: {error:#}"));
                let activity = match activity {
                    Ok(Some(snapshot)) => Ok(Some(snapshot)),
                    _ => super::runner::RunSnapshot::recover_reports(
                        instance.root(),
                        &session.suites,
                    ),
                };
                #[cfg(target_os = "windows")]
                super::jump_list::record_workspace(instance.root());
                Ok((
                    session,
                    workspace_value,
                    instance,
                    activity_path.ok(),
                    activity,
                    activity_warning,
                    deployment_path,
                ))
            },
            |this,
             (
                session,
                workspace_value,
                mut instance,
                activity_path,
                activity,
                activity_warning,
                deployment_path,
            ),
             window,
             cx| {
                this.activation_task = instance.activation_events().map(|mut events| {
                    cx.spawn_in(window, async move |view, cx| {
                        while events.next().await.is_some() {
                            if view
                                .update_in(cx, |this, window, cx| {
                                    if this
                                        .workspace_instance
                                        .as_ref()
                                        .is_some_and(DesktopInstance::take_activation)
                                    {
                                        window.activate_window();
                                        cx.activate(true);
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                });
                this.workspace_instance = Some(instance);
                this.session = session;
                this.workspace_value = workspace_value;
                this.main_tab = 0;
                this.overview = true;
                this.tab = 0;
                this.advanced = false;
                this.scenario_search
                    .update(cx, |state, cx| state.set_value("", window, cx));
                this.workspace_files_open = false;
                this.selected_only = false;
                this.search
                    .update(cx, |state, cx| state.set_value("", window, cx));
                this.results.update(cx, |panel, cx| panel.clear(window, cx));
                if let Some(workspace) = &this.session.workspace {
                    this.preferences.remember(workspace);
                    let root = workspace.root.clone();
                    this.runs.update(cx, |panel, cx| {
                        panel.set_workspace(root.clone(), window, cx)
                    });
                    this.git.update(cx, |panel, cx| {
                        panel.set_disabled(false, cx);
                        panel.set_workspace(Some(root), window, cx);
                    });
                    this.save_preferences(window, cx);
                }
                this.sync_editor_content(window, cx);
                this.runs.update(cx, |panel, cx| {
                    if let Some(path) = deployment_path {
                        panel.set_deployment_storage_path(path, cx);
                    }
                    panel.restore_activity(activity_path, activity, activity_warning, window, cx)
                });
                this.status = "Workspace opened".into();
                this.refresh_results(window, cx);
            },
            window,
            cx,
        );
    }

    fn choose_workspace(&mut self, _: &OpenWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        cx.spawn_in(window, async move |view, cx| {
            if let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await {
                let _ = view.update_in(cx, |this, window, cx| {
                    this.load_workspace(folder.path().to_path_buf(), window, cx)
                });
            }
        })
        .detach();
    }

    fn load_document(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        self.load_document_at(entry, Selection::Root, window, cx);
    }

    fn open_location(
        &mut self,
        entry: Entry,
        selection: Selection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .session
            .open
            .as_ref()
            .is_some_and(|open| open.entry.path == entry.path)
        {
            self.select(selection, window, cx);
        } else {
            self.load_document_at(entry, selection, window, cx);
        }
    }

    fn load_document_at(
        &mut self,
        entry: Entry,
        selection: Selection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.unsaved(cx) {
            self.guard(files::Pending::Location(entry, selection), window, cx);
            return;
        }
        let Some(workspace) = self.session.workspace.clone() else {
            return;
        };
        self.task(
            "Loading document...",
            move || workspace.open_document(entry),
            move |this, open, window, cx| {
                this.reset_field_view(window, cx);
                this.scenario_search
                    .update(cx, |state, cx| state.set_value("", window, cx));
                this.workspace_files_open = open.entry.kind != DocumentKind::Suite;
                this.session.open = Some(open);
                this.overview = false;
                this.main_tab = 0;
                this.session.selection = selection;
                this.session.reconcile_selection();
                this.tab = if this.session.open.as_ref().is_some_and(|open| {
                    open.entry.kind == DocumentKind::Data
                        || (open.entry.kind == DocumentKind::Suite
                            && this.session.selection == Selection::Root)
                }) {
                    1
                } else {
                    0
                };
                this.sync_editor_content(window, cx);
                this.status = "Document loaded".into();
            },
            window,
            cx,
        );
    }

    fn source_matches_document(&self, cx: &App) -> bool {
        let text = self.source.read(cx).text();
        match &self.session.open {
            Some(open) => text.slice(..) == open.document.source(),
            None => text.len() == 0,
        }
    }

    fn unsaved(&self, cx: &App) -> bool {
        self.session.dirty()
            || !self.field_errors.is_empty()
            || (self.session.open.is_some() && !self.source_matches_document(cx))
    }

    fn capture_source_draft(&mut self, cx: &App) -> Result<bool> {
        let state = self.source.read(cx);
        let Some(open) = &mut self.session.open else {
            return Ok(false);
        };
        let selection = state.selected_range();
        if state.text().slice(..) == open.document.source() {
            open.document.set_source_selection(selection);
            return Ok(false);
        }
        open.document
            .set_source_draft(state.text().to_string(), selection)?;
        self.field_errors.clear();
        Ok(true)
    }

    fn refresh_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let suites = self.session.suites.clone();
        let active = self
            .session
            .open
            .as_ref()
            .filter(|open| open.entry.kind == DocumentKind::Suite)
            .map(|open| open.entry.clone())
            .or_else(|| suites.first().map(|suite| suite.entry.clone()));
        match self.runs.read(cx).output_root() {
            Ok(root) => self.results.update(cx, |panel, cx| {
                panel.set_suites(suites);
                if panel.active_suite().is_none()
                    && let Some(entry) = active
                {
                    panel.select_suite(&entry, window, cx);
                }
                if self.main_tab == 2 {
                    panel.refresh(root, window, cx);
                } else {
                    panel.refresh_existing(root, window, cx);
                }
            }),
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
            }
        }
    }

    fn sync_suites(&mut self, cx: &mut Context<Self>) {
        self.suite_menu_epoch = self.suite_menu_epoch.wrapping_add(1);
        let suites = self.session.suites.clone();
        let error = self.session.suite_error.clone();
        self.runs
            .update(cx, |panel, cx| panel.update_catalog(suites, error, cx));
    }

    fn start_selected_run(
        &mut self,
        intent: runs::RunIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() {
            return;
        }
        let Some(workspace) = self.session.workspace.clone() else {
            return;
        };
        if workspace.root != intent.request.root {
            return;
        }
        if !intent.request.command.supports_suites() {
            match intent.resolve(&[]) {
                Ok(request) => self.runs.update(cx, |panel, cx| {
                    panel.start(request, intent.review_after_run, window, cx)
                }),
                Err(error) => {
                    self.status = format!("{error:#}");
                    cx.notify();
                }
            }
            return;
        }
        let review_after_run = intent.review_after_run;
        let report_approval = intent.request.options.report_capture.is_some();
        self.task("Validating selected suites...", move || {
            let suites = workspace.suite_catalog()?;
            let request = intent.resolve_for_execution(&suites).map_err(|error| format!("{error:#}"));
            Ok((suites, request))
        }, move |this, (suites, request), window, cx| {
            this.session.suites = suites;
            this.session.suite_error = None;
            this.sync_suites(cx);
            match request {
                Ok(request) => this.runs.update(cx, |panel, cx| panel.start(request, review_after_run, window, cx)),
                Err(error) => {
                    this.status = format!("Selected suites changed or contain errors; review the selection before running: {error:#}");
                    if report_approval {
                        this.results.update(cx, |panel, cx| panel.approval_failed(error.clone(), cx));
                    }
                    cx.notify();
                }
            }
        }, window, cx);
    }

    fn close_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runs.read(cx).running() || self.busy {
            self.status =
                "Finish or cancel the active operation before closing the workspace".into();
            cx.notify();
            return;
        }
        match self.session.close() {
            Ok(()) => {
                self.activation_task = None;
                self.workspace_instance = None;
                self.runs.update(cx, |panel, cx| panel.clear(window, cx));
                self.results.update(cx, |panel, cx| panel.clear(window, cx));
                self.git
                    .update(cx, |panel, cx| panel.set_workspace(None, window, cx));
                self.search
                    .update(cx, |state, cx| state.set_value("", window, cx));
                self.workspace_value = Value::Null;
                self.main_tab = 0;
                self.tab = 0;
                self.sync_editor_content(window, cx);
                self.status = "Workspace closed".into();
            }
            Err(error) => self.status = format!("{error:#}"),
        }
        cx.notify();
    }

    fn guard_run(
        &mut self,
        intent: Box<runs::RunIntent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() {
            return;
        }
        if self.session.open.is_some()
            && let Err(error) = self.flush_drafts(window, cx)
        {
            self.status = format!("{error:#}");
            self.main_tab = 0;
            self.overview = false;
            cx.notify();
            return;
        }
        if intent.request.options.report_capture.is_none() {
            self.main_tab = 1;
        }
        self.guard(files::Pending::Run(intent), window, cx);
    }

    fn request_selected_command(
        &mut self,
        command: super::runner::RunCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() {
            return;
        }
        self.main_tab = 1;
        self.runs
            .update(cx, |panel, cx| panel.request_command(command, window, cx));
        cx.notify();
    }

    fn can_test_open_suite(&self, cx: &App) -> bool {
        !self.busy
            && !self.source_feedback.stale()
            && !self.runs.read(cx).running()
            && self
                .session
                .open
                .as_ref()
                .and_then(|open| open.entry.suite_name())
                .is_some_and(|name| self.runs.read(cx).has_reference(&name))
    }

    fn run_open_suite(
        &mut self,
        command: super::runner::RunCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_open_scope(command, None, window, cx);
    }

    fn run_scenario(
        &mut self,
        command: super::runner::RunCommand,
        scenario: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.source_selection_ready(cx) {
            return;
        }
        self.run_open_scope(command, Some(scenario), window, cx);
    }

    fn draft_suite_summary(&self) -> Result<workspace::SuiteSummary> {
        let open = self
            .session
            .open
            .as_ref()
            .ok_or_else(|| anyhow!("Open a test suite first"))?;
        let mut summary = self
            .session
            .suites
            .iter()
            .find(|suite| suite.entry.path == open.entry.path)
            .cloned()
            .ok_or_else(|| anyhow!("The selected file is not an active test suite"))?;
        let suite: crate::types::TestSuite = serde_json::from_value(self.value.clone())?;
        summary.display_name = suite.display_name;
        summary.scenarios = workspace::ScenarioSummary::from_scenarios(&suite.scenarios);
        summary.error = None;
        Ok(summary)
    }

    fn select_open_scenario(
        &mut self,
        index: usize,
        visible: &[usize],
        modifiers: gpui_kit::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() {
            return;
        }
        let result = (|| -> Result<()> {
            self.flush_drafts(window, cx)?;
            let suite = self.draft_suite_summary()?;
            let name = suite.name.clone();
            let id = suite
                .scenarios
                .get(index)
                .ok_or_else(|| anyhow!("Scenario is no longer available"))?
                .id
                .clone();
            let visible = visible
                .iter()
                .filter_map(|index| {
                    suite
                        .scenarios
                        .get(*index)
                        .map(|scenario| scenario.id.clone())
                })
                .collect::<Vec<_>>();
            self.runs.update(cx, |panel, cx| {
                panel.update_draft_suite(suite, cx);
                panel.select_scenario(&name, &id, &visible, modifiers, cx);
            });
            Ok(())
        })();
        if let Err(error) = result {
            self.status = format!("{error:#}");
        }
        cx.notify();
    }

    fn select_open_scenarios(
        &mut self,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() {
            return;
        }
        let result = (|| -> Result<()> {
            let name = if selected {
                self.flush_drafts(window, cx)?;
                let suite = self.draft_suite_summary()?;
                let name = suite.name.clone();
                self.runs
                    .update(cx, |panel, cx| panel.update_draft_suite(suite, cx));
                name
            } else {
                self.session
                    .open
                    .as_ref()
                    .and_then(|open| open.entry.suite_name())
                    .ok_or_else(|| anyhow!("Open a test suite first"))?
            };
            self.runs
                .update(cx, |panel, cx| panel.toggle_suite(&name, selected, cx));
            Ok(())
        })();
        if let Err(error) = result {
            self.status = format!("{error:#}");
        }
        cx.notify();
    }

    fn run_open_scope(
        &mut self,
        command: super::runner::RunCommand,
        scenario: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() || !command.supports_suites() {
            return;
        }
        if let Err(error) = self.flush_drafts(window, cx) {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        let scenario = match scenario
            .map(|index| {
                let suite = self.draft_suite_summary()?;
                let id = suite
                    .scenarios
                    .get(index)
                    .ok_or_else(|| anyhow!("Scenario is no longer available"))?
                    .id
                    .clone();
                Ok::<_, anyhow::Error>((suite, id))
            })
            .transpose()
        {
            Ok(scenario) => scenario,
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
                return;
            }
        };
        let Some((workspace, entry)) = self
            .session
            .workspace
            .clone()
            .zip(self.session.open.as_ref().map(|open| open.entry.clone()))
        else {
            return;
        };
        self.task(
            "Checking selected suite...",
            move || workspace.runnable_suite(&entry),
            move |this, suite, window, cx| {
                let result = this.runs.update(cx, |panel, cx| {
                    if let Some((summary, id)) = scenario {
                        panel.update_draft_suite(summary, cx);
                        panel.set_scenario(&suite, &id, cx)
                    } else {
                        panel.set_suites(vec![suite], window, cx)
                    }
                });
                if let Err(error) = result {
                    this.status = format!("{error:#}");
                    cx.notify();
                    return;
                }
                this.main_tab = 1;
                this.request_selected_command(command, window, cx);
                cx.notify();
            },
            window,
            cx,
        );
    }

    fn save_document(&mut self, _: &SaveDocument, window: &mut Window, cx: &mut Context<Self>) {
        self.save_and_continue(None, window, cx);
    }

    fn save_and_continue(
        &mut self,
        pending: Option<files::Pending>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        if let Err(error) = self.flush_drafts(window, cx) {
            self.status = format!("{error:#}");
            self.sync_modal(cx);
            cx.notify();
            return;
        }
        let snapshot = (|| -> Result<_> {
            let workspace = self
                .session
                .workspace
                .clone()
                .ok_or_else(|| anyhow!("Open a workspace first"))?;
            let open = self
                .session
                .open
                .as_ref()
                .ok_or_else(|| anyhow!("Open a document first"))?;
            let mut document = Document::new(
                open.document.kind,
                open.document.format,
                open.document.saved_source().into(),
            )?;
            document.set_source(open.document.source().into())?;
            Ok((
                workspace,
                OpenDocument {
                    entry: open.entry.clone(),
                    document,
                },
            ))
        })();
        let outputs = self.runs.read(cx).suite_output_roots();
        match snapshot {
            Ok((workspace, mut snapshot)) => self.task(
                "Saving...",
                move || {
                    workspace.save_in_outputs(&mut snapshot, &outputs)?;
                    Ok((
                        snapshot.document.source().to_string(),
                        workspace.suite_catalog(),
                    ))
                },
                move |this, (written, catalog), window, cx| {
                    if let Some(open) = &mut this.session.open {
                        open.document.acknowledge_saved(written);
                    }
                    match catalog {
                        Ok(suites) => {
                            this.session.suites = suites;
                            this.session.suite_error = None;
                        }
                        Err(error) => {
                            this.session.suites.clear();
                            this.session.suite_error = Some(format!("{error:#}"));
                        }
                    }
                    this.sync_suites(cx);
                    this.status = if this.unsaved(cx) {
                        "Saved snapshot; newer edits are still unsaved"
                    } else {
                        "Saved"
                    }
                    .into();
                    if let Some(pending) = pending
                        && !this.unsaved(cx)
                    {
                        window.close_dialog(cx);
                        this.perform_pending(pending, window, cx);
                    }
                },
                window,
                cx,
            ),
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
            }
        }
    }

    fn undo_document(&mut self, _: &UndoDocument, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.tab == 1
            && let Err(error) = self.capture_source_draft(cx)
        {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        if let Some(open) = &mut self.session.open {
            open.document.undo();
        }
        self.session.reconcile_selection();
        self.sync_editor_content(window, cx);
    }

    fn redo_document(&mut self, _: &RedoDocument, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.tab == 1
            && let Err(error) = self.capture_source_draft(cx)
        {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        if let Some(open) = &mut self.session.open {
            open.document.redo();
        }
        self.session.reconcile_selection();
        self.sync_editor_content(window, cx);
    }

    fn edit(
        &mut self,
        operation: impl FnOnce(&mut Session) -> Result<()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        if !self.source_selection_ready(cx) {
            return;
        }
        if let Err(error) = self.flush_drafts(window, cx) {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        match operation(&mut self.session) {
            Ok(()) => {
                self.status = "Unsaved changes".into();
                self.sync_editor_content(window, cx);
            }
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
            }
        }
    }

    fn flush_drafts(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        if self.tab == 1 {
            self.capture_source_draft(cx)?;
        }
        let open = self
            .session
            .open
            .as_mut()
            .ok_or_else(|| anyhow!("Open a document first"))?;
        open.document.break_source_history();
        if self.tab == 0 {
            let parent = self.session.selection.path();
            let current = open.document.value()?;
            let mut edits = Vec::new();
            // Read retained controls directly: change notifications can still be queued.
            for binding in &self.bindings {
                let text = match &binding.control {
                    Control::Input(input) => input.read(cx).value().to_string(),
                    Control::Textarea(input) => input.read(cx).value().to_string(),
                    _ => continue,
                };
                let key = &binding.field.key;
                let value = current
                    .pointer(&parent)
                    .and_then(|value| value.get(key))
                    .unwrap_or(&Value::Null);
                if text == binding.field.display(value) {
                    continue;
                }
                match binding.field.parse(&text) {
                    Ok(value) => {
                        edits.push(super::document::Edit::Set {
                            parent: parent.clone(),
                            key: key.clone(),
                            value: Some(value),
                        });
                        self.field_errors.remove(key);
                    }
                    Err(error) => {
                        self.field_errors.insert(key.clone(), format!("{error:#}"));
                    }
                }
            }
            anyhow::ensure!(
                self.field_errors.is_empty(),
                "Correct invalid fields before saving"
            );
            open.document.apply_many(edits)?;
            let source = open.document.source().to_string();
            self.source
                .update(cx, |state, cx| state.set_value(source, window, cx));
        }
        self.refresh_value();
        self.session.reconcile_selection_in(&self.value);
        self.update_source_diagnostics(cx);
        self.refresh_effective(window, cx);
        Ok(())
    }

    fn refresh_value(&mut self) {
        self.cancel_source_feedback();
        let inspection = self
            .session
            .open
            .as_ref()
            .map(|open| {
                source::inspect_source_document(
                    &open.document,
                    self.source_feedback.contextual_issues(&self.session),
                    None,
                )
            })
            .unwrap_or_default();
        self.value = inspection.value;
        self.set_source_issues(inspection.issues);
    }

    #[cfg(all(test, feature = "gui-tests"))]
    fn sync_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session.refresh_catalog();
        self.workspace_value = Value::Null;
        self.sync_editor_content(window, cx);
    }

    fn sync_editor_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.source_feedback.invalidate();
        self.viewport_choices.invalidate();
        self.sync_suites(cx);
        let source = self
            .session
            .open
            .as_ref()
            .map(|open| open.document.source().to_string())
            .unwrap_or_default();
        if !self.source_matches_document(cx) {
            let selection = self
                .session
                .open
                .as_ref()
                .and_then(|open| open.document.source_selection());
            self.source.update(cx, |state, cx| {
                let scroll = state.scroll_offset();
                state.set_value(source, window, cx);
                if let Some(selection) = selection {
                    state.set_selected_range(selection, cx);
                    state.set_scroll_offset(scroll, cx);
                }
            });
        }
        self.rebuild_fields(window, cx);
        cx.notify();
    }

    fn rebuild_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.configure_source(cx);
        self.refresh_value();
        self.update_source_diagnostics(cx);
        let scope = self
            .session
            .workspace
            .as_ref()
            .zip(self.session.open.as_ref())
            .map(|(workspace, open)| {
                (
                    workspace.root.join(&open.entry.path),
                    self.session.selection,
                )
            });
        let reuse_choices = self.choice_binding_scope == scope;
        self.choice_binding_scope = scope;
        if !reuse_choices {
            self.list_choice_pickers.clear();
        }
        let mut choice_states = std::mem::take(&mut self.bindings)
            .into_iter()
            .filter_map(|binding| match (binding.field.kind, binding.control) {
                (FieldKind::Choice(choices), Control::Choice(state)) if reuse_choices => {
                    Some((binding.field.key, (choices, state)))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        self.field_subscriptions.clear();
        self.field_errors.clear();
        if self
            .session
            .open
            .as_ref()
            .is_some_and(|open| open.entry.kind == DocumentKind::Workspace)
        {
            self.workspace_value = self.value.clone();
        } else if self.workspace_value.is_null() {
            self.workspace_value = self
                .session
                .workspace_value()
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
        }
        self.refresh_effective(window, cx);
        let fields = self
            .session
            .scope()
            .and_then(|scope| scope.map(|scope| schema::fields(&scope)).transpose());
        let Ok(Some(fields)) = fields else {
            return;
        };
        for field in fields {
            let value = self
                .value
                .pointer(&self.session.selection.path())
                .and_then(|value| value.get(&field.key))
                .unwrap_or(&Value::Null);
            let text = field.display(value);
            let placeholder = if field.key == "displayName" {
                field.label.clone()
            } else {
                field.display(&self.effective[&schema::effective_key(&field.key)])
            };
            let control = match &field.kind {
                FieldKind::Boolean => Control::Boolean,
                FieldKind::Choice(choices) => {
                    let shown = if value.is_null() {
                        placeholder.clone()
                    } else {
                        text.clone()
                    };
                    let (state, items_changed) = match choice_states.remove(&field.key) {
                        Some((previous_choices, state)) => (state, previous_choices != *choices),
                        None => (
                            cx.new(|cx| {
                                SelectState::new(choices::value_choices(choices), None, window, cx)
                                    .searchable(true)
                            }),
                            false,
                        ),
                    };
                    state.update(cx, |state, cx| {
                        if items_changed {
                            state.set_items(choices::value_choices(choices), window, cx);
                        }
                        // Kit selection sync clears the query; unchanged refreshes must retain it.
                        if items_changed
                            || state.selected_value()
                                != choices.iter().find(|choice| *choice == &shown)
                        {
                            state.set_selected_value(&shown, window, cx);
                        }
                    });
                    let descriptor = field.clone();
                    let entry = self
                        .session
                        .workspace
                        .as_ref()
                        .zip(self.session.open.as_ref())
                        .map(|(workspace, open)| workspace.root.join(&open.entry.path));
                    let selection = self.session.selection;
                    self.field_subscriptions.push(cx.subscribe_in(
                        &state,
                        window,
                        move |this, state, event, window, cx| {
                            let current = this
                                .session
                                .workspace
                                .as_ref()
                                .zip(this.session.open.as_ref())
                                .map(|(workspace, open)| workspace.root.join(&open.entry.path));
                            if current != entry || this.session.selection != selection {
                                return;
                            }
                            if let SelectEvent::Confirm(Some(value)) = event {
                                this.edit(
                                    |session| {
                                        session.set_field(&descriptor.key, Some(json!(value)))
                                    },
                                    window,
                                    cx,
                                );
                                let value = this
                                    .value
                                    .pointer(&selection.path())
                                    .and_then(|value| value.get(&descriptor.key))
                                    .filter(|value| !value.is_null())
                                    .unwrap_or(
                                        &this.effective[&schema::effective_key(&descriptor.key)],
                                    );
                                let shown = descriptor.display(value);
                                state.update(cx, |state, cx| {
                                    state.set_selected_value(&shown, window, cx)
                                });
                            }
                        },
                    ));
                    Control::Choice(state)
                }
                FieldKind::List { .. } | FieldKind::Structured => {
                    let state = cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .rows(3)
                            .default_value(text)
                            .placeholder(placeholder)
                    });
                    let descriptor = field.clone();
                    self.field_subscriptions.push(cx.subscribe_in(
                        &state,
                        window,
                        move |this, state, event, window, cx| {
                            if matches!(event, InputEvent::Change) {
                                this.change_field(
                                    &descriptor,
                                    state.read(cx).value().as_ref(),
                                    window,
                                    cx,
                                );
                            }
                        },
                    ));
                    Control::Textarea(state)
                }
                _ => {
                    let state = cx.new(|cx| {
                        let state = InputState::new(window, cx);
                        let state = if matches!(field.kind, FieldKind::Number { .. }) {
                            let mut state =
                                state.mask_pattern(gpui_kit::base::input::MaskPattern::None);
                            state.set_step(None, window, cx);
                            state
                        } else {
                            state
                        };
                        state.default_value(text).placeholder(placeholder)
                    });
                    let descriptor = field.clone();
                    self.field_subscriptions.push(cx.subscribe_in(
                        &state,
                        window,
                        move |this, state, event, window, cx| {
                            if matches!(event, InputEvent::Change) {
                                this.change_field(
                                    &descriptor,
                                    state.read(cx).value().as_ref(),
                                    window,
                                    cx,
                                );
                            }
                        },
                    ));
                    if matches!(field.kind, FieldKind::Number { .. }) {
                        let descriptor = field.clone();
                        self.field_subscriptions.push(cx.subscribe_in(
                            &state,
                            window,
                            move |this, _, event: &NumberInputEvent, window, cx| {
                                let NumberInputEvent::Step(action) = event;
                                this.adjust_field(
                                    &descriptor,
                                    *action == StepAction::Increment,
                                    window,
                                    cx,
                                );
                            },
                        ));
                    }
                    Control::Input(state)
                }
            };
            self.bindings.push(Binding { field, control });
        }
    }

    fn change_field(
        &mut self,
        field: &Field,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let current = self
            .value
            .pointer(&self.session.selection.path())
            .and_then(|value| value.get(&field.key))
            .unwrap_or(&Value::Null);
        // Programmatic resets emit change events too; an empty inherited control is not an override.
        if !self.field_errors.contains_key(&field.key) && field.display(current) == text {
            return;
        }
        match field
            .parse(text)
            .and_then(|value| self.session.set_field(&field.key, Some(value)))
        {
            Ok(()) => {
                self.field_errors.remove(&field.key);
                self.restore_collapsed_field_focus(field, window, cx);
                self.refresh_value();
                self.refresh_effective(window, cx);
                if let Some(open) = &self.session.open {
                    let source = open.document.source().to_string();
                    self.source
                        .update(cx, |state, cx| state.set_value(source, window, cx));
                }
                self.update_source_diagnostics(cx);
                self.status = "Unsaved changes".into();
            }
            Err(error) => {
                self.field_errors
                    .insert(field.key.clone(), format!("{error:#}"));
            }
        }
        cx.notify();
    }

    fn reset_field(&mut self, field: &Field, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || field.required {
            return;
        }
        if let Err(error) = self.session.set_field(&field.key, None) {
            self.field_errors
                .insert(field.key.clone(), format!("{error:#}"));
            cx.notify();
            return;
        }
        self.field_errors.remove(&field.key);
        self.refresh_value();
        self.refresh_effective(window, cx);
        if let Some(binding) = self
            .bindings
            .iter()
            .find(|binding| binding.field.key == field.key)
        {
            match &binding.control {
                Control::Input(input) => {
                    input.update(cx, |state, cx| state.set_value("", window, cx))
                }
                Control::Textarea(input) => {
                    input.update(cx, |state, cx| state.set_value("", window, cx))
                }
                Control::Choice(state) => {
                    let shown = field.display(&self.effective[&schema::effective_key(&field.key)]);
                    state.update(cx, |state, cx| state.set_selected_value(&shown, window, cx));
                }
                _ => {}
            }
        }
        if let Some(open) = &self.session.open {
            let source = open.document.source().to_string();
            self.source
                .update(cx, |state, cx| state.set_value(source, window, cx));
        }
        self.update_source_diagnostics(cx);
        self.status = "Override removed".into();
        cx.notify();
    }

    fn select(&mut self, selection: Selection, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Err(error) = self.flush_drafts(window, cx) {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        match self.session.select(selection) {
            Ok(()) => {
                self.reset_field_view(window, cx);
                self.tab = if self.session.selection == Selection::Root
                    && self.session.open.as_ref().is_some_and(|open| {
                        open.entry.kind == DocumentKind::Suite
                            || open.document.format == Format::Text
                    }) {
                    1
                } else {
                    0
                };
                self.overview = false;
                self.main_tab = 0;
                self.rebuild_fields(window, cx);
            }
            Err(error) => self.status = format!("{error:#}"),
        }
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = self.session.open.as_ref();
        let focus = self.focus.clone();
        let add_scenario = open.is_some_and(|open| open.entry.kind == DocumentKind::Suite);
        let add_viewport = open.is_some_and(|open| open.entry.kind == DocumentKind::Viewports);
        let add_action = self.session.selection.scenario().is_some();
        let selection_busy = self.busy || self.source_feedback.stale();
        let history = gpui_kit::component::toolbar::ToolbarGroup::new("editor-history-group")
            .label("Document commands")
            .gap_2()
            .child(
                Button::new("save")
                    .icon(gpui_kit::assets::IconName::Save)
                    .accessibility_label("Save document")
                    .tooltip("Save document")
                    .disabled(
                        self.busy
                            || !self.unsaved(cx)
                            || self.diagnostic.is_some()
                            || !self.field_errors.is_empty(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.save_document(&SaveDocument, window, cx)
                    })),
            )
            .child(
                Button::new("undo")
                    .ghost()
                    .icon(IconName::Undo)
                    .tooltip("Undo document edit")
                    .disabled(self.busy || !open.is_some_and(|open| open.document.can_undo()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.undo_document(&UndoDocument, window, cx)
                    })),
            )
            .child(
                Button::new("redo")
                    .ghost()
                    .icon(IconName::Redo)
                    .tooltip("Redo document edit")
                    .disabled(self.busy || !open.is_some_and(|open| open.document.can_redo()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.redo_document(&RedoDocument, window, cx)
                    })),
            );
        let selection = gpui_kit::component::toolbar::ToolbarGroup::new("editor-selection-group")
            .label("Selection commands")
            .gap_2()
            .content(
                Button::new("add-item")
                    .icon(IconName::Plus)
                    .label("Add")
                    .disabled(selection_busy || open.is_none())
                    .prepare_for_toolbar()
                    .with_size(gpui_kit::component::Size::Medium)
                    .dropdown_menu(move |menu, window, cx| {
                        design::restore_dropdown_menu_focus(
                            menu.focus_handle(cx),
                            focus.clone(),
                            window,
                            cx,
                        );
                        let mut menu = menu
                            .action_context(focus.clone())
                            .menu_with_disabled("Scenario", Box::new(AddScenario), !add_scenario)
                            .menu_with_disabled("Viewport", Box::new(AddViewport), !add_viewport)
                            .separator();
                        for action in ACTIONS {
                            menu = menu.menu_with_disabled(
                                *action,
                                Box::new(AddBrowserAction {
                                    kind: action.to_string(),
                                }),
                                !add_action,
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("duplicate-item")
                    .ghost()
                    .icon(IconName::Copy)
                    .tooltip("Duplicate selected item")
                    .accessibility_label("Duplicate selected item")
                    .disabled(selection_busy || self.session.selection == Selection::Root)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.edit(Session::duplicate_selected, window, cx)
                    })),
            )
            .child(
                Button::new("remove-item")
                    .ghost()
                    .icon(gpui_kit::assets::IconName::Trash)
                    .tooltip("Remove selected item")
                    .disabled(selection_busy || self.session.selection == Selection::Root)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.confirm_remove(false, window, cx)),
                    ),
            )
            .child(
                Button::new("move-up")
                    .ghost()
                    .icon(IconName::ArrowUp)
                    .tooltip("Move selected item up")
                    .disabled(selection_busy || self.session.selection == Selection::Root)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.edit(|session| session.move_selected(-1), window, cx)
                    })),
            )
            .child(
                Button::new("move-down")
                    .ghost()
                    .icon(IconName::ArrowDown)
                    .tooltip("Move selected item down")
                    .disabled(selection_busy || self.session.selection == Selection::Root)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.edit(|session| session.move_selected(1), window, cx)
                    })),
            );
        let toolbar = gpui_kit::component::toolbar::Toolbar::new("editor-command-toolbar")
            .with_size(gpui_kit::component::Size::Medium)
            .h_auto()
            .flex_wrap()
            .child(history)
            .child(selection)
            .content(div().flex_1())
            .when(add_scenario, |bar| {
                let focus = self.focus.clone();
                let execution =
                    gpui_kit::component::toolbar::ToolbarGroup::new("editor-execution-group")
                        .label("Suite execution")
                        .gap_2()
                        .child(
                            Button::new("suite-run-reference")
                                .icon(gpui_kit::assets::IconName::Camera)
                                .label("Reference")
                                .accessibility_label("Run Suite Reference")
                                .tooltip("Capture reference images for the open suite")
                                .disabled(self.busy || self.runs.read(cx).running())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.run_open_suite(
                                        super::runner::RunCommand::Reference,
                                        window,
                                        cx,
                                    )
                                })),
                        )
                        .child(
                            Button::new("suite-run-test")
                                .primary()
                                .icon(gpui_kit::assets::IconName::Play)
                                .label("Test")
                                .accessibility_label("Run Suite Test")
                                .tooltip("Run all scenarios in the open suite")
                                .disabled(!self.can_test_open_suite(cx))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.run_open_suite(super::runner::RunCommand::Test, window, cx)
                                })),
                        )
                        .content(
                            Button::new("suite-run-more")
                                .ghost()
                                .icon(gpui_kit::assets::IconName::Ellipsis)
                                .accessibility_label("More suite actions")
                                .tooltip("More suite actions")
                                .disabled(self.busy || self.runs.read(cx).running())
                                .prepare_for_toolbar()
                                .with_size(gpui_kit::component::Size::Medium)
                                .dropdown_menu(move |menu, window, cx| {
                                    design::restore_dropdown_menu_focus(
                                        menu.focus_handle(cx),
                                        focus.clone(),
                                        window,
                                        cx,
                                    );
                                    menu.action_context(focus.clone())
                                        .menu(
                                            "Approve Results",
                                            Box::new(RunSuiteCommand { index: 3 }),
                                        )
                                        .menu(
                                            "Compare Results",
                                            Box::new(RunSuiteCommand { index: 2 }),
                                        )
                                }),
                        );
                bar.child(execution)
            });
        div()
            .id("editor-toolbar")
            .role(Role::Group)
            .aria_label("Editor commands")
            .test_support()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(design::ToolbarFocus::new("editor-toolbar-focus", toolbar))
            .into_any_element()
    }

    fn refresh_effective(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .session
            .open
            .as_ref()
            .is_some_and(|open| open.entry.kind == DocumentKind::Workspace)
        {
            self.workspace_value = self.value.clone();
        }
        let suite = self
            .session
            .open
            .as_ref()
            .filter(|open| open.entry.kind == DocumentKind::Suite)
            .map(|_| &self.value);
        let resolved = schema::effective(
            self.workspace_value
                .is_object()
                .then_some(&self.workspace_value),
            suite,
            self.session.selection.scenario(),
        );
        self.effective = resolved.as_ref().cloned().unwrap_or(Value::Null);
        let text = resolved
            .and_then(|value| Ok(serde_json::to_string_pretty(&value)?))
            .unwrap_or_else(|error| format!("{error:#}"));
        self.resolved
            .update(cx, |state, cx| state.set_value(text, window, cx));
        self.load_viewport_choices(window, cx);
        self.schedule_source_feedback(window, cx);
    }

    fn field_hint(&self, field: &Field, explicit: bool) -> String {
        if field.required {
            return "Required".into();
        }
        if explicit {
            return "Override".into();
        }
        if matches!(
            self.session.selection,
            Selection::Action { .. } | Selection::Viewport(_)
        ) {
            return "Optional".into();
        }
        let value = &self.effective[&schema::effective_key(&field.key)];
        if value.is_null() {
            return "Optional".into();
        }
        let suite = self
            .session
            .open
            .as_ref()
            .filter(|open| open.entry.kind == DocumentKind::Suite)
            .map(|_| &self.value);
        let scenario = self
            .session
            .selection
            .scenario()
            .and_then(|index| self.value["scenarios"].get(index));
        let origin = schema::origin(&field.key, Some(&self.workspace_value), suite, scenario);
        let text = field.display(value).replace('\n', ", ");
        format!("{origin}: {text}")
    }

    fn reset_field_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.field_search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.overrides_only = false;
        self.collapsed_fields.clear();
    }

    fn adjust_field(
        &mut self,
        field: &Field,
        increase: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let result = (|| -> Result<Value> {
            let binding = self
                .bindings
                .iter()
                .find(|binding| binding.field.key == field.key)
                .ok_or_else(|| anyhow!("Field is unavailable"))?;
            let current = if let Control::Input(input) = &binding.control {
                let text = input.read(cx).value();
                if text.is_empty() {
                    self.effective[&schema::effective_key(&field.key)].clone()
                } else {
                    field.parse(&text)?
                }
            } else {
                return Err(anyhow!("Field is not numeric"));
            };
            field.stepped_value(&current, increase)
        })();
        match result {
            Ok(value) => {
                let text = field.display(&value);
                self.change_field(field, &text, window, cx);
                if !self.field_errors.contains_key(&field.key)
                    && let Some(binding) = self
                        .bindings
                        .iter()
                        .find(|binding| binding.field.key == field.key)
                    && let Control::Input(input) = &binding.control
                {
                    input.update(cx, |state, cx| state.set_value(text, window, cx));
                }
            }
            Err(error) => {
                self.field_errors
                    .insert(field.key.clone(), format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn reveal_problem(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = 0;
        self.overview = false;
        self.overrides_only = false;
        self.field_search
            .update(cx, |state, cx| state.set_value(key.to_string(), window, cx));
        self.collapsed_fields.clear();
        if let Some(binding) = self
            .bindings
            .iter()
            .find(|binding| binding.field.key == key)
        {
            match &binding.control {
                Control::Input(input) => window.focus(&input.read(cx).focus_handle(cx), cx),
                Control::Textarea(input) => window.focus(&input.read(cx).focus_handle(cx), cx),
                _ => {}
            }
        }
        cx.notify();
    }

    fn field_has_error(&self, field: &Field) -> bool {
        if self.field_errors.contains_key(&field.key) {
            return true;
        }
        let value = self
            .value
            .pointer(&self.session.selection.path())
            .and_then(|selected| selected.get(&field.key))
            .unwrap_or(&Value::Null);
        !value.is_null()
            && matches!(&field.kind, FieldKind::Choice(choices) if !choices.contains(&field.display(value)))
    }

    fn restore_collapsed_field_focus(
        &self,
        field: &Field,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = matches!(self.session.selection, Selection::Action { .. });
        let group = if action {
            0
        } else {
            design::group(&field.key).0
        };
        if !self.collapsed_fields.contains(&group)
            || !self.field_search.read(cx).value().trim().is_empty()
            || self.bindings.iter().any(|binding| {
                (action || design::group(&binding.field.key).0 == group)
                    && self.field_has_error(&binding.field)
            })
        {
            return;
        }
        let owns_focus = self.bindings.iter().any(|binding| {
            if !action && design::group(&binding.field.key).0 != group {
                return false;
            }
            match &binding.control {
                Control::Input(input) => input.read(cx).focus_handle(cx).is_focused(window),
                Control::Textarea(input) => input.read(cx).focus_handle(cx).is_focused(window),
                Control::Choice(input) => input.read(cx).focus_handle(cx).is_focused(window),
                Control::Boolean => false,
            }
        });
        if owns_focus {
            window.focus(&self.field_group_focus[group], cx);
        }
    }

    fn render_field_group(
        &self,
        group: (usize, &'static str),
        content: Form,
        forced: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = forced || !self.collapsed_fields.contains(&group.0);
        div()
            .id(("field-group", group.0))
            .test_support()
            .flex_shrink_0()
            .child(
                gpui_kit::component::accordion::Accordion::new("field-disclosure")
                    .multiple(true)
                    .bordered(false)
                    .h_auto()
                    .item(|item| {
                        item.open(open)
                            .title(
                                div()
                                    .id("field-group-title")
                                    .role(Role::Label)
                                    .aria_value(group.1)
                                    .aria_expanded(open)
                                    .track_focus(&self.field_group_focus[group.0])
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().transparent)
                                    .focus({
                                        let ring = cx.theme().ring;
                                        move |style| style.border_color(ring)
                                    })
                                    .on_key_down(cx.listener(
                                        move |this, event: &KeyDownEvent, window, cx| {
                                            if event.keystroke.modifiers != Modifiers::default()
                                                || !matches!(event.keystroke.key.as_str(), "enter" | "space")
                                                || !this.field_group_focus[group.0].is_focused(window)
                                                || gpui_kit::base::GlobalState::is_in_deferred_context(cx)
                                            {
                                                return;
                                            }
                                            if open {
                                                this.collapsed_fields.insert(group.0);
                                            } else {
                                                this.collapsed_fields.remove(&group.0);
                                            }
                                            window.prevent_default();
                                            cx.stop_propagation();
                                            cx.notify();
                                        },
                                    ))
                                    .child(group.1),
                            )
                                .when(open, |item| item.child(content))
                    })
                    .on_toggle_click({
                        let view = cx.entity().downgrade();
                        move |indices, window, cx| {
                            let requested_open = indices.contains(&0);
                            // Content clicks bubble too; only a trigger transition changes preference.
                            if requested_open == open {
                                return;
                            }
                            let _ = view.update(cx, |this, cx| {
                                if requested_open {
                                    this.collapsed_fields.remove(&group.0);
                                } else {
                                    this.collapsed_fields.insert(group.0);
                                }
                                window.focus(&this.field_group_focus[group.0], cx);
                                cx.notify();
                            });
                        }
                    }),
            )
            .into_any_element()
    }

    fn render_fields(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let selected = self
            .value
            .pointer(&self.session.selection.path())
            .unwrap_or(&Value::Null);
        let mut form = div()
            .id("field-form")
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .w_full();
        let action = matches!(self.session.selection, Selection::Action { .. });
        let query = self.field_search.read(cx).value().trim().to_lowercase();
        let exact = self
            .bindings
            .iter()
            .any(|binding| binding.field.key.eq_ignore_ascii_case(&query))
            || (query == "actions" && matches!(self.session.selection, Selection::Scenario(_)));
        let mut bindings: Vec<_> = self.bindings.iter().enumerate().collect();
        bindings.sort_by_key(|(index, binding)| {
            (
                if action {
                    0
                } else {
                    design::group(&binding.field.key).0
                },
                *index,
            )
        });
        let mut current_group = None;
        let mut section = Form::new().columns(2);
        let mut group_error = false;
        let mut matching = 0;
        for (_, binding) in bindings {
            let field = &binding.field;
            let explicit = selected
                .get(&field.key)
                .is_some_and(|value| !value.is_null());
            let error = self.field_has_error(field);
            let group = if action {
                (0, "Action properties")
            } else {
                design::group(&field.key)
            };
            if !query.is_empty()
                && (if exact {
                    !field.key.eq_ignore_ascii_case(&query)
                } else {
                    ![
                        field.key.as_str(),
                        field.label.as_str(),
                        field.description.as_str(),
                        group.1,
                    ]
                    .iter()
                    .any(|text| text.to_lowercase().contains(&query))
                })
            {
                continue;
            }
            if self.overrides_only && !explicit && !error {
                continue;
            }
            if query.is_empty()
                && !self.overrides_only
                && !explicit
                && !error
                && !self.advanced
                && !field.required
                && self.session.selection == Selection::Root
                && ![
                    "id",
                    "displayName",
                    "browser",
                    "viewportNames",
                    "misMatchThreshold",
                    "pixelMatchThreshold",
                    "postInteractionWait",
                    "asyncCaptureLimit",
                    "asyncCompareLimit",
                    "autoCleanRuns",
                    "keepTestRuns",
                ]
                .contains(&field.key.as_str())
            {
                continue;
            }
            matching += 1;
            if current_group != Some(group) {
                if let Some(previous) = current_group {
                    form = form.child(self.render_field_group(
                        previous,
                        section,
                        group_error || !query.is_empty(),
                        cx,
                    ));
                    if previous.0 == 0
                        && query.is_empty()
                        && !self.overrides_only
                        && let Selection::Scenario(scenario) = self.session.selection
                    {
                        form = form.child(self.render_sequence(scenario, window, cx));
                    }
                }
                section = Form::new().columns(2);
                current_group = Some(group);
                group_error = false;
            }
            group_error |= error;
            let value = selected.get(&field.key).unwrap_or(&Value::Null);
            let explicit = !value.is_null();
            let field_id = SharedString::from(format!("field-{}", field.key));
            let wide = matches!(field.kind, FieldKind::List { .. } | FieldKind::Structured)
                || matches!(field.key.as_str(), "url" | "description")
                || action;
            let key = field.key.clone();
            let reset = field.clone();
            let field_label = field.label.clone();
            let boolean = matches!(binding.control, Control::Boolean);
            let inherited_hint = self.field_hint(field, explicit);
            let hint_id = SharedString::from(format!("hint-{}", field.key));
            let hint_width = if wide { 300. } else { 150. };
            let muted_foreground = cx.theme().muted_foreground;
            let reset_disabled =
                self.busy || field.required || (!explicit && !self.field_errors.contains_key(&key));
            let view = cx.entity();
            let mut row = FormField::new()
                .label_fn(move |window, _| {
                    let reset = reset.clone();
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .justify_between()
                        .gap_2()
                        .when(!boolean, |row| {
                            row.child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(field_label.clone()),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted_foreground)
                                        .id(hint_id.clone())
                                        .role(Role::Status)
                                        .aria_label(inherited_hint.clone())
                                        .test_support()
                                        .max_w(px(hint_width))
                                        .truncate()
                                        .child(inherited_hint.clone()),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("reset-{key}")))
                                        .ghost()
                                        .icon(IconName::Undo)
                                        .tooltip("Use inherited value")
                                        .disabled(reset_disabled)
                                        .on_click(window.listener_for(
                                            &view,
                                            move |this, _, window, cx| {
                                                this.reset_field(&reset, window, cx)
                                            },
                                        )),
                                ),
                        )
                })
                .required(field.required && !boolean);
            let control = match &binding.control {
                Control::Input(state) if matches!(field.kind, FieldKind::Number { .. }) => div()
                    .id(field_id)
                    .role(Role::Group)
                    .aria_label(field.label.clone())
                    .test_support()
                    .w_full()
                    .child(
                        NumberInput::new(state)
                            .accessibility_label(field.label.clone())
                            .disabled(self.busy),
                    )
                    .into_any_element(),
                Control::Input(state) => Input::new(state)
                    .id(field_id)
                    .disabled(self.busy)
                    .when(field.key.ends_with("Path"), |input| {
                        let key = field.key.clone();
                        input.suffix(
                            Button::new(SharedString::from(format!("browse-{key}")))
                                .ghost()
                                .icon(IconName::FolderOpen)
                                .tooltip("Choose a workspace file")
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.choose_field_file(key.clone(), window, cx)
                                })),
                        )
                    })
                    .into_any_element(),
                Control::Textarea(state) => div()
                    .id(field_id)
                    .child(Textarea::new(state).disabled(self.busy))
                    .into_any_element(),
                Control::Boolean => {
                    let descriptor = field.clone();
                    let checked = value
                        .as_bool()
                        .or_else(|| self.effective[&schema::effective_key(&field.key)].as_bool())
                        .unwrap_or(false);
                    Checkbox::new(field_id)
                        .checked(checked)
                        .label(field.label.clone())
                        .disabled(self.busy)
                        .on_change(cx.listener(move |this, checked, window, cx| {
                            this.change_field(
                                &descriptor,
                                if *checked { "true" } else { "false" },
                                window,
                                cx,
                            )
                        }))
                        .into_any_element()
                }
                Control::Choice(state) => {
                    let shown = if value.is_null() {
                        field.display(&self.effective[&schema::effective_key(&field.key)])
                    } else {
                        field.display(value)
                    };
                    let unknown = !value.is_null()
                        && matches!(&field.kind, FieldKind::Choice(choices) if !choices.contains(&shown));
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            gpui_kit::component::select::Select::new(state)
                                .id(field_id)
                                .accessibility_label(field.label.clone())
                                .placeholder(if shown.is_empty() {
                                    "Inherited".to_string()
                                } else {
                                    shown
                                })
                                .disabled(self.busy)
                                .when(unknown, |control| control.border_color(cx.theme().danger)),
                        )
                        .when(unknown, |row| {
                            row.child(
                                div()
                                    .id(SharedString::from(format!("choice-error-{}", field.key)))
                                    .role(Role::Status)
                                    .aria_label("Unsupported value")
                                    .test_support()
                                    .text_xs()
                                    .text_color(cx.theme().danger)
                                    .child("Unsupported value"),
                            )
                        })
                        .into_any_element()
                }
            };
            row = row.child(control);
            if let Some(choices) = self.render_list_choices(field, cx) {
                row = row.child(choices);
            }
            if field.key == "file" {
                let key = field.key.clone();
                row = row.child(
                    Button::new(SharedString::from(format!("browse-{key}")))
                        .ghost()
                        .icon(IconName::FolderOpen)
                        .tooltip("Choose a workspace file")
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.choose_field_file(key.clone(), window, cx)
                        })),
                );
            }
            if let Some(error) = self.field_errors.get(&field.key) {
                row = row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                );
            }
            section = section.child(row.when(wide, |field| field.col_span(2)));
        }
        if let Some(group) = current_group {
            form = form.child(self.render_field_group(
                group,
                section,
                group_error || !query.is_empty(),
                cx,
            ));
        }
        if query == "actions"
            && let Selection::Scenario(scenario) = self.session.selection
        {
            form = form.child(self.render_sequence(scenario, window, cx));
            matching += 1;
        }
        if matching == 0 {
            form = form.child(div().text_color(cx.theme().muted_foreground).child(
                if self.session.open.is_some() {
                    "No matching settings"
                } else {
                    "No document open"
                },
            ));
        }
        div()
            .id("field-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(form)
            .into_any_element()
    }

    fn select_editor_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.busy
            || self.session.open.is_none()
            || index > 2
            || (index != 1
                && self
                    .session
                    .open
                    .as_ref()
                    .is_some_and(|open| open.document.format == Format::Text))
        {
            return false;
        }
        if let Err(error) = self.flush_drafts(window, cx) {
            self.status = format!("{error:#}");
            cx.notify();
            return false;
        }
        if index == 0
            && self.tab == 1
            && self
                .session
                .open
                .as_ref()
                .is_some_and(|open| open.document.value().is_err())
        {
            self.status = "Correct the source syntax errors before switching to Fields".into();
            cx.notify();
            return false;
        }
        self.tab = index;
        if index != 1 {
            self.source.update(cx, |state, cx| {
                state.dismiss_completion_overlay(cx);
                source::clear_source_popovers(state, cx);
            });
        }
        self.session.reconcile_selection();
        self.rebuild_fields(window, cx);
        cx.notify();
        true
    }

    fn render_editor(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.overview || self.session.open.is_none() {
            return self.render_overview(cx);
        }
        let title = self
            .session
            .open
            .as_ref()
            .map(|open| {
                format!(
                    "{}{}",
                    open.entry.label(),
                    if self.unsaved(cx) { " *" } else { "" }
                )
            })
            .unwrap_or_else(|| "Workspace".into());
        let text_document = self
            .session
            .open
            .as_ref()
            .is_some_and(|open| open.document.format == Format::Text);
        let content = match self.tab {
            1 => self.render_source(cx),
            2 => div()
                .id("effective-editor")
                .size_full()
                .child(
                    CodeEditor::new(&self.resolved)
                        .aria_label("Effective configuration")
                        .size_full()
                        .readonly(true),
                )
                .into_any_element(),
            _ => self.render_fields(window, cx),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .child(self.render_toolbar(cx))
            .children(self.render_external_file_notice(cx))
            .child(
                div()
                    .px_4()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                Button::new("breadcrumb-document")
                                    .ghost()
                                    .accessibility_label(title.clone())
                                    .tooltip(title.clone())
                                    .min_w_0()
                                    .max_w(px(520.))
                                    .child(div().truncate().child(title))
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.select(Selection::Root, window, cx)
                                    })),
                            )
                            .when_some(
                                self.session.selection.scenario().filter(|_| {
                                    matches!(self.session.selection, Selection::Action { .. })
                                }),
                                |row, scenario| {
                                    row.child(Icon::new(IconName::ChevronRight).xsmall()).child(
                                        Button::new("breadcrumb-scenario")
                                            .ghost()
                                            .min_w_0()
                                            .max_w(px(360.))
                                            .accessibility_label("Parent scenario")
                                            .tooltip("Open parent scenario")
                                            .child(
                                                div()
                                                    .truncate()
                                                    .child(self.scenario_title(scenario)),
                                            )
                                            .disabled(self.busy)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.select(
                                                    Selection::Scenario(scenario),
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
                                },
                            ),
                    )
                    .child(
                        div()
                            .id("editor-heading")
                            .role(Role::Heading)
                            .aria_label(self.selection_title())
                            .test_support()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(self.selection_title()),
                    )
                    .when_some(
                        self.session
                            .open
                            .as_ref()
                            .and_then(|open| open.entry.suite_name()),
                        |heading, name| {
                            let summary = self.runs.read(cx).artifact_summary(&name);
                            let available = self.runs.read(cx).can_open_saved_suite(&name);
                            heading.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .flex_wrap()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("suite-artifact-status")
                                            .role(Role::Status)
                                            .aria_label(summary.clone())
                                            .test_support()
                                            .flex_1()
                                            .min_w_0()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(summary),
                                    )
                                    .when(available, |row| {
                                        row.child(
                                            Button::new("suite-open-report")
                                                .icon(gpui_kit::assets::IconName::Images)
                                                .label("Open Report")
                                                .disabled(self.busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.runs.update(cx, |panel, cx| {
                                                        panel.open_saved_reports(
                                                            vec![name.clone()],
                                                            cx,
                                                        )
                                                    })
                                                })),
                                        )
                                    }),
                            )
                        },
                    )
                    .when_some(self.session.selection.scenario(), |heading, scenario| {
                        heading.child(
                            div()
                                .flex()
                                .items_center()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    Button::new("scenario-run-test")
                                        .primary()
                                        .icon(gpui_kit::assets::IconName::Play)
                                        .label("Run Scenario Test")
                                        .tooltip("Requires reference images for this suite")
                                        .disabled(!self.can_test_open_suite(cx))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.run_scenario(
                                                super::runner::RunCommand::Test,
                                                scenario,
                                                window,
                                                cx,
                                            )
                                        })),
                                )
                                .child(
                                    Button::new("scenario-run-reference")
                                        .icon(gpui_kit::assets::IconName::Camera)
                                        .label("Run Scenario Reference")
                                        .disabled(
                                            self.busy
                                                || self.source_feedback.stale()
                                                || self.runs.read(cx).running(),
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.run_scenario(
                                                super::runner::RunCommand::Reference,
                                                scenario,
                                                window,
                                                cx,
                                            )
                                        })),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .px_4()
                    .bg(cx.theme().muted.opacity(0.45))
                    .child(
                        TabBar::new("editor-tabs")
                            .underline()
                            .selected_index([1, 0, 2][self.tab])
                            .on_click(cx.listener(|this, index, window, cx| {
                                this.select_editor_tab([1, 0, 2][*index], window, cx);
                            }))
                            .child(Tab::new().label("Source"))
                            .when(!text_document, |tabs| {
                                tabs.child(Tab::new().label("Fields"))
                                    .child(Tab::new().label("Effective"))
                            }),
                    )
                    .when(!text_document, |bar| {
                        bar.child(
                            Checkbox::new("advanced-fields")
                                .label("Advanced")
                                .checked(self.advanced)
                                .on_change(cx.listener(|this, checked, _, cx| {
                                    this.advanced = *checked;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(self.tab == 0, |editor| {
                editor.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_3()
                        .px_5()
                        .py_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            Input::new(&self.field_search)
                                .id("field-search")
                                .prefix(IconName::Search)
                                .cleanable(true)
                                .w(px(250.)),
                        )
                        .child(
                            Checkbox::new("overrides-only")
                                .label("Overrides only")
                                .checked(self.overrides_only)
                                .on_change(cx.listener(|this, checked, _, cx| {
                                    this.overrides_only = *checked;
                                    cx.notify();
                                })),
                        )
                        .child(div().flex_1())
                        .when(self.session.selection == Selection::Root, |bar| {
                            bar.child(self.cleanup_button("settings-cleanup", cx))
                        })
                        .child(
                            Button::new("expand-fields")
                                .ghost()
                                .icon(gpui_kit::assets::IconName::ChevronsUpDown)
                                .tooltip("Expand all settings")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.collapsed_fields.clear();
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when(!self.field_errors.is_empty(), |editor| {
                editor.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_2()
                        .px_5()
                        .py_2()
                        .bg(cx.theme().danger.opacity(0.07))
                        .child(
                            Icon::new(gpui_kit::assets::IconName::CircleAlert)
                                .small()
                                .text_color(cx.theme().danger),
                        )
                        .child(format!(
                            "{} field {}",
                            self.field_errors.len(),
                            if self.field_errors.len() == 1 {
                                "issue"
                            } else {
                                "issues"
                            }
                        ))
                        .children(self.field_errors.keys().map(|key| {
                            let key = key.clone();
                            Button::new(SharedString::from(format!("problem-{key}")))
                                .ghost()
                                .label(key.clone())
                                .text_color(cx.theme().danger)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.reveal_problem(&key, window, cx)
                                }))
                        })),
                )
            })
            .child(div().flex_1().min_h_0().child(content))
            .children(self.render_source_diagnostics(cx))
            .into_any_element()
    }

    fn scenario_titles(&self) -> Vec<String> {
        crate::helpers::scenario_names(
            &self.value["scenarios"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|scenario| crate::helpers::ScenarioName {
                    id: scenario["id"].as_str().unwrap_or_default(),
                    label: scenario["label"].as_str(),
                    url: scenario["url"].as_str(),
                })
                .collect::<Vec<_>>(),
        )
    }

    fn scenario_title(&self, index: usize) -> String {
        self.scenario_titles()
            .get(index)
            .cloned()
            .unwrap_or_else(|| "Scenario".into())
    }

    fn selection_title(&self) -> String {
        if let Some(open) = self
            .session
            .open
            .as_ref()
            .filter(|open| open.document.format == Format::Text)
        {
            return open
                .entry
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| open.entry.label());
        }
        match self.session.selection {
            Selection::Root => self.value["displayName"]
                .as_str()
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    self.session
                        .open
                        .as_ref()
                        .and_then(|open| open.entry.suite_name())
                })
                .unwrap_or_else(|| "Workspace settings".into()),
            Selection::Scenario(index) => self.scenario_title(index),
            Selection::Action { scenario, index } => {
                let action = &self.value["scenarios"][scenario]["actions"][index];
                let kind = ACTIONS
                    .iter()
                    .find(|kind| action.get(**kind).is_some())
                    .copied()
                    .unwrap_or("Action");
                format!("{}. {}", index + 1, kind)
            }
            Selection::Viewport(index) => self.value[index]["label"]
                .as_str()
                .unwrap_or("Viewport")
                .into(),
        }
    }

    fn save_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preferences_pending = true;
        if self.preferences_saving {
            return;
        }
        self.preferences_pending = false;
        self.preferences_saving = true;
        let preferences = self.preferences.clone();
        let path = self.preferences_path.clone();
        let status = self.status.clone();
        let task = cx.background_executor().spawn(async move {
            preferences.save(&path.ok_or_else(|| anyhow!("Cannot determine the preferences path"))?)
        });
        // Serialize writes and take a fresh snapshot after any changes made during a save.
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.preferences_saving = false;
                let saved = result.is_ok();
                #[cfg(target_os = "windows")]
                if saved {
                    cx.background_executor()
                        .spawn(async { super::jump_list::refresh(None) })
                        .detach();
                }
                if let Err(error) = result {
                    this.status = format!("Cannot save preferences: {error:#}");
                }
                if this.preferences_pending {
                    this.save_preferences(window, cx);
                } else {
                    if saved && this.status == status {
                        this.status = "Preferences saved".into();
                    }
                    if this.close_after_preferences {
                        this.close_after_preferences = false;
                        if saved && !this.busy && !this.unsaved(cx) && !this.runs.read(cx).running()
                        {
                            this.quit_or_install(cx);
                        } else {
                            this.updates
                                .update(cx, |state, _| state.install_on_close = false);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_close(&mut self, cx: &mut Context<Self>) {
        if self.preferences_saving {
            self.close_after_preferences = true;
            self.status = "Saving preferences...".into();
            cx.notify();
        } else {
            self.quit_or_install(cx);
        }
    }

    fn toggle_navigator(
        &mut self,
        _: &ToggleNavigator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.preferences.navigator_hidden = !self.preferences.navigator_hidden;
        if self.preferences.navigator_hidden {
            window.focus(&self.focus, cx);
        }
        self.save_preferences(window, cx);
    }

    fn select_view(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index > 3 {
            return;
        }
        let changed = self.main_tab != index;
        self.main_tab = index;
        window.focus(&self.focus, cx);
        if changed && index == 2 {
            if let Some(open) = &self.session.open
                && open.entry.kind == DocumentKind::Suite
            {
                self.results.update(cx, |panel, cx| {
                    panel.select_suite(&open.entry, window, cx);
                });
            }
            self.refresh_results(window, cx);
        }
        cx.notify();
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_overview_table(cx);
        self.sync_workspace_list(window, cx);
        self.sync_sequence_list(window, cx);
        self.sync_app_menus(window, cx);
        self.sync_list_choice_pickers(window, cx);
        self.results
            .update(cx, |panel, cx| panel.set_visible(self.main_tab == 2, cx));
        let editor = if self.main_tab == 3 {
            self.git.clone().into_any_element()
        } else if self.main_tab == 2 {
            self.results.clone().into_any_element()
        } else if self.main_tab == 1 {
            self.runs.clone().into_any_element()
        } else {
            self.render_editor(window, cx)
        };
        let layout = if self.preferences.navigator_hidden || self.main_tab == 3 {
            editor
        } else {
            h_resizable("workspace-layout")
                .child(
                    resizable_panel()
                        .size(px(288.))
                        .size_range(px(200.)..px(440.))
                        .child(
                            div()
                                .id("workspace-navigation")
                                .test_support()
                                .size_full()
                                .child(self.render_navigation(window, cx)),
                        ),
                )
                .child(resizable_panel().child(editor))
                .into_any_element()
        };
        div()
            .id("workbench")
            .key_context("Regressify")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_sm()
            .on_action(cx.listener(Self::save_document))
            .on_action(cx.listener(Self::undo_document))
            .on_action(cx.listener(Self::redo_document))
            .on_action(cx.listener(Self::choose_workspace))
            .on_action(cx.listener(|this, _: &FocusNavigator, window, cx| {
                if this.preferences.navigator_hidden {
                    this.toggle_navigator(&ToggleNavigator, window, cx);
                }
                cx.notify();
                window.focus(&this.search.read(cx).focus_handle(cx), cx)
            }))
            .on_action(cx.listener(Self::toggle_navigator))
            .on_action(cx.listener(|this, action: &SelectView, window, cx| {
                this.select_view(action.index, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusSettings, window, cx| {
                if this.session.open.is_none() {
                    return;
                }
                this.select_view(0, window, cx);
                this.overview = false;
                if this.tab != 0 && !this.select_editor_tab(0, window, cx) {
                    return;
                }
                window.focus(&this.field_search.read(cx).focus_handle(cx), cx);
                cx.notify();
            }))
            .on_action(cx.listener(Self::file_command))
            .on_action(cx.listener(Self::app_file_command))
            .on_action(cx.listener(Self::show_about))
            .on_action(cx.listener(|this, action: &RunSuiteCommand, window, cx| {
                if let Some(command) = super::runner::RunCommand::ALL.get(action.index) {
                    this.run_open_suite(*command, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, action: &RunScenarioCommand, window, cx| {
                    if let Some(command) = super::runner::RunCommand::ALL.get(action.index) {
                        this.run_scenario(*command, action.scenario, window, cx);
                    }
                }),
            )
            .on_action(cx.listener(Self::open_commands))
            .on_action(cx.listener(|this, _: &OpenGit, window, cx| this.select_view(3, window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenDeployment, window, cx| {
                    this.open_deployment(window, cx)
                }),
            )
            .on_action(cx.listener(Self::change_theme))
            .on_action(cx.listener(Self::choose_deploy_target))
            .on_action(cx.listener(Self::choose_deploy_provider))
            .on_action(cx.listener(Self::choose_cloudflare_setting))
            .on_action(cx.listener(Self::choose_deploy_token))
            .on_action(cx.listener(Self::choose_named_token))
            .on_action(cx.listener(Self::choose_named_token_provider))
            .on_action(cx.listener(|this, _: &OpenDeploymentTokens, window, cx| {
                this.open_token_manager(window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenLandingApps, window, cx| {
                this.open_landing_manager(window, cx)
            }))
            .on_action(cx.listener(Self::choose_landing_app))
            .on_action(cx.listener(Self::toggle_list_choice))
            .on_action(cx.listener(|this, _: &AddScenario, window, cx| {
                this.edit(Session::add_scenario, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AddViewport, window, cx| {
                this.edit(Session::add_viewport, window, cx)
            }))
            .on_action(cx.listener(|this, _: &DuplicateItem, window, cx| {
                this.edit(Session::duplicate_selected, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveItemUp, window, cx| {
                this.edit(|session| session.move_selected(-1), window, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveItemDown, window, cx| {
                this.edit(|session| session.move_selected(1), window, cx)
            }))
            .on_action(cx.listener(|this, action: &AddBrowserAction, window, cx| {
                this.edit(|session| session.add_action(&action.kind), window, cx)
            }))
            .font_family(cx.theme().font_family.clone())
            .child(self.render_header(window, cx))
            .children(self.render_update_banner(cx))
            .children(self.render_update_notice(cx))
            .child(div().flex_1().min_h_0().child(layout))
    }
}

pub fn launch(instance: Option<DesktopInstance>) {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            #[cfg(target_os = "windows")]
            cx.background_executor()
                .spawn(async { super::jump_list::refresh(None) })
                .detach();
            let preference = Preferences::path()
                .and_then(|path| Preferences::load(&path))
                .map(|preferences| preferences.theme_preference())
                .unwrap_or_default();
            design::apply_preference(preference, None, cx);
            bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1320.), px(840.)), cx);
            cx.spawn(async move |cx| {
                let result = cx.update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(bounds)),
                            window_min_size: Some(size(px(1000.), px(640.))),
                            titlebar: Some(TitlebarOptions {
                                title: Some("Regressify Desktop".into()),
                                ..TitleBar::title_bar_options()
                            }),
                            ..TitleBar::window_options()
                        },
                        cx,
                        |window, cx| {
                            let view = cx.new(|cx| Workbench::new(Preferences::path(), window, cx));
                            #[cfg(target_os = "windows")]
                            match super::jump_list::initialize_window(window) {
                                Ok(taskbar) => {
                                    view.update(cx, |this, _| this.taskbar = Some(taskbar))
                                }
                                Err(error) => crate::diagnostics::startup::warning(
                                    crate::diagnostics::record::SafeValue::Shell,
                                    &error,
                                ),
                            }
                            let weak = view.downgrade();
                            window.on_window_should_close(cx, move |window, cx| {
                                weak.update(cx, |this, cx| {
                                    if this.install_blocks_quit(cx) {
                                        false
                                    } else if this.workspace_lost {
                                        true
                                    } else if this.busy {
                                        if this.git.read(cx).active() {
                                            this.git
                                                .update(cx, |panel, cx| panel.cancel_operation(cx));
                                            this.status =
                                                "Cancelling Git; close again after it finishes"
                                                    .into();
                                        } else {
                                            this.status =
                                                "Finish the current operation before closing"
                                                    .into();
                                        }
                                        cx.notify();
                                        false
                                    } else if this.runs.read(cx).running() {
                                        this.runs.update(cx, |panel, cx| panel.cancel(cx));
                                        this.status =
                                        "Cancelling the active run; close again after it finishes"
                                            .into();
                                        cx.notify();
                                        false
                                    } else if this.guard_landing_close(window, cx) {
                                        false
                                    } else if this.unsaved(cx) {
                                        this.guard(files::Pending::Close, window, cx);
                                        false
                                    } else {
                                        this.finish_close(cx);
                                        false
                                    }
                                })
                                .unwrap_or(true)
                            });
                            if let Some(instance) = instance {
                                view.update(cx, |this, cx| {
                                    this.load_owned_workspace(instance, window, cx)
                                });
                            }
                            view.update(cx, |this, cx| this.check_updates(window, cx));
                            view
                        },
                    )
                });
                if let Err(error) = result {
                    crate::diagnostics::startup::warning(
                        crate::diagnostics::record::SafeValue::Window,
                        &error,
                    );
                }
            })
            .detach();
        });
}

fn bind_keys(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    lists::bind_keys(modifier, cx);
    source::bind_keys(modifier, cx);
    cx.bind_keys([
        KeyBinding::new(
            &format!("{modifier}-shift-p"),
            OpenCommands,
            Some("Regressify"),
        ),
        KeyBinding::new("ctrl-space", CompleteSource, Some("RegressifySource")),
        KeyBinding::new(&format!("{modifier}-shift-g"), OpenGit, Some("Regressify")),
        KeyBinding::new(&format!("{modifier}-p"), FocusNavigator, Some("Regressify")),
        KeyBinding::new(
            &format!("{modifier}-b"),
            ToggleNavigator,
            Some("Regressify"),
        ),
        KeyBinding::new(
            &format!("{modifier}-shift-f"),
            FocusSettings,
            Some("Regressify"),
        ),
        KeyBinding::new(
            &format!("{modifier}-1"),
            SelectView { index: 0 },
            Some("Regressify"),
        ),
        KeyBinding::new(
            &format!("{modifier}-2"),
            SelectView { index: 1 },
            Some("Regressify"),
        ),
        KeyBinding::new(
            &format!("{modifier}-3"),
            SelectView { index: 2 },
            Some("Regressify"),
        ),
        KeyBinding::new(&format!("{modifier}-s"), SaveDocument, Some("Regressify")),
        KeyBinding::new(&format!("{modifier}-o"), OpenWorkspace, Some("Regressify")),
        KeyBinding::new(&format!("{modifier}-z"), UndoDocument, Some("Regressify")),
        KeyBinding::new(
            &format!("{modifier}-shift-z"),
            RedoDocument,
            Some("Regressify"),
        ),
        KeyBinding::new(
            &format!("{modifier}-n"),
            files::FileCommand {
                operation: files::Operation::New,
            },
            Some("Regressify"),
        ),
        KeyBinding::new(
            &format!("{modifier}-shift-s"),
            files::FileCommand {
                operation: files::Operation::SaveAs,
            },
            Some("Regressify"),
        ),
        KeyBinding::new(
            &format!("{modifier}-r"),
            files::FileCommand {
                operation: files::Operation::Reload,
            },
            Some("Regressify"),
        ),
        KeyBinding::new(
            "f5",
            files::FileCommand {
                operation: files::Operation::Refresh,
            },
            Some("Regressify"),
        ),
    ]);
}
