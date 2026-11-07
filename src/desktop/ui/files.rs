use super::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
pub(super) enum Operation {
    New,
    Duplicate,
    Rename,
    SaveAs,
    Reload,
    Delete,
    Config,
    Viewports,
    Browse,
    Refresh,
    Theme,
    OpenVSCode,
    OpenFolder,
    CloseWorkspace,
}

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
pub(super) struct FileCommand {
    pub(super) operation: Operation,
}

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
pub(super) struct AppFileCommand {
    pub(super) operation: Operation,
}

#[derive(Clone)]
pub(super) enum Pending {
    Workspace(PathBuf),
    Location(Entry, Selection),
    Name(Operation),
    SuiteOperation(Entry, Operation),
    Reload,
    Close,
    Update,
    Run(Box<runs::RunIntent>),
    Git(Box<crate::desktop::git::Request>),
    CloseWorkspace,
}

#[derive(Default)]
pub(super) struct ModalState {
    busy: bool,
    blocked: bool,
    status: String,
}

struct FileVersion {
    root: PathBuf,
    entry: Entry,
    saved_source: String,
}

impl FileVersion {
    fn matches(&self, session: &Session) -> bool {
        session.workspace.as_ref().map(|workspace| &workspace.root) == Some(&self.root)
            && session.open.as_ref().is_some_and(|open| {
                open.entry.path == self.entry.path
                    && open.document.saved_source() == self.saved_source
            })
    }
}

struct ExternalFileChange {
    version: FileVersion,
    document: Result<OpenDocument>,
    catalog: Option<Result<Vec<workspace::SuiteSummary>>>,
}

pub(super) struct ExternalFileNotice {
    version: FileVersion,
    message: String,
}

impl Workbench {
    pub(super) fn watch_external_file(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let Ok(version) = view.update(cx, |this, _| {
                    if this.busy {
                        return None;
                    }
                    let workspace = this.session.workspace.as_ref()?;
                    let open = this.session.open.as_ref()?;
                    Some(FileVersion {
                        root: workspace.root.clone(),
                        entry: open.entry.clone(),
                        saved_source: open.document.saved_source().to_string(),
                    })
                }) else {
                    break;
                };
                let Some(version) = version else {
                    continue;
                };
                let change = cx
                    .background_executor()
                    .spawn(async move {
                        let workspace = workspace::Workspace {
                            root: version.root.clone(),
                        };
                        let document = workspace.open_document(version.entry.clone());
                        let catalog = document
                            .as_ref()
                            .ok()
                            .filter(|open| open.document.source() != version.saved_source)
                            .map(|_| workspace.suite_catalog());
                        ExternalFileChange {
                            version,
                            document,
                            catalog,
                        }
                    })
                    .await;
                if view
                    .update_in(cx, |this, window, cx| {
                        this.apply_external_file_change(change, window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    pub(super) fn external_reload_has_draft(&self, cx: &App) -> bool {
        if self.unsaved(cx) {
            return true;
        }
        if self.tab != 0 {
            return false;
        }
        let parent = self.session.selection.path();
        self.bindings.iter().any(|binding| {
            let text = match &binding.control {
                Control::Input(input) => input.read(cx).value().to_string(),
                Control::Textarea(input) => input.read(cx).value().to_string(),
                _ => return false,
            };
            let value = self
                .value
                .pointer(&parent)
                .and_then(|value| value.get(&binding.field.key))
                .unwrap_or(&Value::Null);
            text != binding.field.display(value)
        })
    }

    fn apply_external_file_change(
        &mut self,
        change: ExternalFileChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || !change.version.matches(&self.session) {
            return;
        }
        let incoming = match change.document {
            Ok(incoming) => incoming,
            Err(error) => {
                self.external_file_notice = Some(ExternalFileNotice {
                    version: change.version,
                    message: format!(
                        "Cannot reload file from disk; current content kept: {error:#}"
                    ),
                });
                cx.notify();
                return;
            }
        };
        if incoming.document.source() == change.version.saved_source {
            if self.external_file_notice.take().is_some() {
                cx.notify();
            }
            return;
        }
        // Native change events can still be queued when the background read finishes.
        if self.external_reload_has_draft(cx) {
            self.external_file_notice = Some(ExternalFileNotice {
                version: change.version,
                message: "File changed outside Regressify. Your unsaved edits were kept.".into(),
            });
            cx.notify();
            return;
        }
        let selection = self.source.read(cx).selected_range();
        if self.session.apply_external_document(
            &change.version.root,
            &change.version.saved_source,
            incoming,
        ) != super::super::session::ExternalReload::Reloaded
        {
            return;
        }
        if let Some(catalog) = change.catalog {
            match catalog {
                Ok(suites) => {
                    self.session.suites = suites;
                    self.session.suite_error = None;
                }
                Err(error) => {
                    self.session.suites.clear();
                    self.session.suite_error = Some(format!("{error:#}"));
                }
            }
        }
        if self.session.value().is_err() {
            self.tab = 1;
        }
        if change.version.entry.kind == DocumentKind::Workspace {
            self.workspace_value = Value::Null;
        }
        self.external_file_notice = None;
        self.sync_editor_content(window, cx);
        self.source.update(cx, |state, cx| {
            let text = state.value();
            let boundary = |offset: usize| {
                let mut offset = offset.min(text.len());
                while !text.is_char_boundary(offset) {
                    offset -= 1;
                }
                offset
            };
            state.set_selected_range(boundary(selection.start)..boundary(selection.end), cx);
        });
        self.status = "Reloaded external changes".into();
        self.sync_modal(cx);
        cx.notify();
    }

    pub(super) fn render_external_file_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self
            .external_file_notice
            .as_ref()
            .filter(|notice| notice.version.matches(&self.session))?;
        Some(
            div()
                .id("external-file-notice")
                .role(Role::Status)
                .aria_label(notice.message.clone())
                .test_support()
                .gap_2()
                .w_full()
                .flex_shrink_0()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted)
                .child(
                    gpui_kit::component::marker::Marker::new()
                        .w_full()
                        .flex_wrap()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .whitespace_normal()
                                .child(notice.message.clone()),
                        )
                        .child(
                            Button::new("external-file-reload")
                                .flex_shrink_0()
                                .icon(gpui_kit::assets::IconName::RotateCw)
                                .label("Reload from disk")
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if this.session.open.is_some()
                                        && let Err(error) = this.flush_drafts(window, cx)
                                    {
                                        this.status = format!("{error:#}");
                                    }
                                    this.guard(Pending::Reload, window, cx);
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn choose_field_file(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.session.workspace.clone() else {
            return;
        };
        let selection = self.session.selection;
        let document_path = self
            .session
            .open
            .as_ref()
            .map(|open| open.entry.path.clone());
        cx.spawn_in(window, async move |view, cx| {
            let picker = rfd::AsyncFileDialog::new().set_directory(&workspace.root);
            let files = if key == "file" {
                picker.pick_files().await
            } else {
                picker.pick_file().await.map(|file| vec![file])
            };
            let Some(files) = files else {
                return;
            };
            let paths = files
                .iter()
                .map(|file| workspace.relative_file(file.path(), key == "file"))
                .collect::<Result<Vec<_>>>();
            let _ = view.update_in(cx, |this, window, cx| {
                if this.session.workspace.as_ref().map(|active| &active.root)
                    != Some(&workspace.root)
                    || this.session.selection != selection
                    || this.session.open.as_ref().map(|open| &open.entry.path)
                        != document_path.as_ref()
                {
                    this.status =
                        "Selection changed while choosing a file; no field was modified".into();
                    cx.notify();
                    return;
                }
                match paths {
                    Ok(paths) => {
                        let value = if paths.len() == 1 {
                            json!(paths[0])
                        } else {
                            json!(paths)
                        };
                        this.edit(|session| session.set_field(&key, Some(value)), window, cx);
                    }
                    Err(error) => {
                        this.status = format!("{error:#}");
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// What the panels were last told: lets tests prove `sync_modal` ran after `busy` changed.
    #[cfg(all(test, feature = "gui-tests"))]
    pub(super) fn modal_busy(&self, cx: &App) -> bool {
        self.modal.read(cx).busy
    }

    pub(super) fn sync_modal(&self, cx: &mut Context<Self>) {
        let disabled = self.busy || self.runs.read(cx).running();
        self.git
            .update(cx, |panel, cx| panel.set_disabled(disabled, cx));
        self.results
            .update(cx, |panel, cx| panel.set_disabled(disabled, cx));
        self.runs
            .update(cx, |panel, cx| panel.set_disabled(self.busy, cx));
        self.modal.update(cx, |state, cx| {
            state.busy = self.busy;
            state.blocked = self.diagnostic.is_some() || !self.field_errors.is_empty();
            state.status.clone_from(&self.status);
            cx.notify();
        });
    }

    pub(super) fn file_operation_enabled(&self, operation: Operation, cx: &App) -> bool {
        let open = self.session.open.as_ref();
        let workspace = self.session.workspace.is_some();
        let suite = open.is_some_and(|open| open.entry.kind == DocumentKind::Suite);
        !self.busy
            && match operation {
                Operation::Duplicate => suite,
                Operation::Rename => suite && !self.runs.read(cx).running(),
                Operation::Delete => suite && !self.unsaved(cx) && !self.runs.read(cx).running(),
                Operation::SaveAs => {
                    open.is_some() && self.diagnostic.is_none() && self.field_errors.is_empty()
                }
                Operation::Reload => open.is_some(),
                Operation::CloseWorkspace => workspace && !self.runs.read(cx).running(),
                Operation::Theme => true,
                _ => workspace,
            }
    }

    pub(super) fn file_menu(&self, cx: &App) -> impl IntoElement {
        let focus = self.focus.clone();
        let entries = menu_entries()
            .into_iter()
            .map(|(label, action)| {
                let enabled = self.file_operation_enabled(action.operation, cx);
                (label, action, enabled)
            })
            .collect::<Vec<_>>();
        Button::new("file-menu")
            .ghost()
            .label("File")
            .disabled(self.busy)
            .dropdown_menu(move |menu, window, cx| {
                design::restore_dropdown_menu_focus(
                    menu.focus_handle(cx),
                    focus.clone(),
                    window,
                    cx,
                );
                let mut menu = menu.action_context(focus.clone());
                for (label, action, enabled) in &entries {
                    menu = menu.menu_with_disabled(*label, Box::new(action.clone()), !enabled);
                }
                menu
            })
    }

    pub(super) fn sync_app_menus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(target_os = "macos") {
            return;
        }
        let entries = menu_entries();
        let publishing = !self.busy && !self.runs.read(cx).running();
        let mut flags = entries
            .iter()
            .map(|(_, command)| self.file_operation_enabled(command.operation, cx))
            .collect::<Vec<_>>();
        flags.push(publishing);
        if flags == self.app_menu_flags {
            return;
        }
        // Cancel before replacing a focused popup so its captured action context can be restored.
        if self.app_menu_focus.contains_focused(window, cx)
            && let Some(focus) = window.focused(cx)
        {
            focus.dispatch_action(&gpui_kit::base::actions::Cancel, window, cx);
        }
        let menus = vec![
            gpui_kit::Menu::new("File")
                .items(
                    entries
                        .into_iter()
                        .zip(&flags)
                        .map(|((label, command), enabled)| {
                            gpui_kit::MenuItem::action(
                                label,
                                AppFileCommand {
                                    operation: command.operation,
                                },
                            )
                            .disabled(!enabled)
                        }),
                )
                .owned(),
            gpui_kit::Menu::new("Publish")
                .items([
                    gpui_kit::MenuItem::action("Landing apps", OpenLandingApps)
                        .disabled(!publishing),
                    gpui_kit::MenuItem::action("Deployment tokens", OpenDeploymentTokens)
                        .disabled(!publishing),
                ])
                .owned(),
            gpui_kit::Menu::new("Help")
                .items([gpui_kit::MenuItem::action("About", ShowAbout)])
                .owned(),
        ];
        gpui_kit::base::GlobalState::global_mut(cx).set_app_menus(menus);
        self.app_menu_bar.update(cx, |bar, cx| bar.reload(cx));
        self.app_menu_flags = flags;
    }

    pub(super) fn app_file_command(
        &mut self,
        command: &AppFileCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_operation_enabled(command.operation, cx) {
            self.file_command(
                &FileCommand {
                    operation: command.operation,
                },
                window,
                cx,
            );
        }
    }

    pub(super) fn file_command(
        &mut self,
        command: &FileCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        match command.operation {
            Operation::New | Operation::Duplicate => {
                self.guard(Pending::Name(command.operation), window, cx)
            }
            Operation::Rename | Operation::Delete => {
                if let Some(entry) = self.session.open.as_ref().map(|open| open.entry.clone()) {
                    self.begin_suite_operation(entry, command.operation, window, cx);
                }
            }
            Operation::SaveAs => self.name_dialog(Operation::SaveAs, window, cx),
            Operation::Reload => self.guard(Pending::Reload, window, cx),
            Operation::Config => self.create_config(false, window, cx),
            Operation::Viewports => self.create_config(true, window, cx),
            Operation::Browse => self.browse_document(window, cx),
            Operation::Refresh => self.refresh_files(window, cx),
            Operation::OpenVSCode => {
                let Some(workspace) = &self.session.workspace else {
                    return;
                };
                let root = workspace.root.clone();
                self.task(
                    "Opening VS Code...",
                    move || crate::desktop::vscode::open(&root, None),
                    |this, (), _, _| this.status = "Opened workspace in VS Code".into(),
                    window,
                    cx,
                );
            }
            Operation::OpenFolder => {
                let Some(workspace) = &self.session.workspace else {
                    return;
                };
                if let Err(error) = open::that_detached(&workspace.root) {
                    self.status = format!("Cannot open workspace externally: {error:#}");
                    cx.notify();
                }
            }
            Operation::CloseWorkspace => self.guard(Pending::CloseWorkspace, window, cx),
            Operation::Theme => {
                let preference = if cx.theme().mode.is_dark() {
                    crate::desktop::workspace::ThemePreference::Light
                } else {
                    crate::desktop::workspace::ThemePreference::Dark
                };
                self.change_theme(&design::ChooseTheme { preference }, window, cx);
            }
        }
    }

    pub(super) fn guard(&mut self, pending: Pending, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if !self.unsaved(cx) {
            self.perform_pending(pending, window, cx);
            return;
        }
        let view = cx.entity();
        self.sync_modal(cx);
        let modal = self.modal.clone();
        let target = self
            .session
            .open
            .as_ref()
            .map(|open| open.entry.label())
            .unwrap_or_default();
        window.open_dialog(cx, move |dialog, window, cx| {
            let state = modal.read(cx);
            let busy = state.busy;
            let blocked = state.blocked;
            let save = pending.clone();
            let discard = pending.clone();
            let run = matches!(pending, Pending::Run(_));
            dialog
                .title(if run {
                    "Save changes before running?"
                } else {
                    "Unsaved changes"
                })
                .close_button(false)
                .keyboard(!busy)
                .child(target.clone())
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(state.status.clone()),
                )
                .footer(
                    gpui_kit::component::dialog::DialogFooter::new()
                        .child(
                            Button::new("guard-cancel")
                                .label("Cancel")
                                .disabled(busy)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("guard-discard")
                                .label(if run { "Run saved version" } else { "Discard" })
                                .disabled(busy)
                                .on_click(window.listener_for(
                                    &view,
                                    move |this, _, window, cx| match this.session.discard() {
                                        Ok(()) => {
                                            this.field_errors.clear();
                                            this.sync_editor_content(window, cx);
                                            window.close_dialog(cx);
                                            this.perform_pending(discard.clone(), window, cx);
                                        }
                                        Err(error) => {
                                            this.status = format!("{error:#}");
                                            cx.notify();
                                        }
                                    },
                                )),
                        )
                        .child(
                            Button::new("guard-save")
                                .primary()
                                .label(if run { "Save and continue" } else { "Save" })
                                .disabled(busy || blocked)
                                .on_click(window.listener_for(
                                    &view,
                                    move |this, _, window, cx| {
                                        this.save_and_continue(Some(save.clone()), window, cx);
                                    },
                                )),
                        ),
                )
        });
    }

    pub(super) fn perform_pending(
        &mut self,
        pending: Pending,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match pending {
            Pending::Workspace(path) => self.load_workspace(path, window, cx),
            Pending::Location(entry, selection) => self.open_location(entry, selection, window, cx),
            Pending::Name(operation) => self.name_dialog(operation, window, cx),
            Pending::SuiteOperation(entry, operation) => {
                if self.busy || self.runs.read(cx).running() {
                    return;
                }
                if self
                    .session
                    .open
                    .as_ref()
                    .is_some_and(|open| open.entry.path == entry.path)
                {
                    self.finish_suite_operation(operation, window, cx);
                } else if let Some(workspace) = self.session.workspace.clone() {
                    self.task(
                        "Loading suite...",
                        move || workspace.open_document(entry),
                        move |this, open, window, cx| {
                            this.session.open = Some(open);
                            this.session.selection = Selection::Root;
                            this.tab = 0;
                            this.sync_editor_content(window, cx);
                            this.finish_suite_operation(operation, window, cx);
                        },
                        window,
                        cx,
                    );
                }
            }
            Pending::Reload => {
                if let Some(entry) = self.session.open.as_ref().map(|open| open.entry.clone()) {
                    self.load_document(entry, window, cx);
                }
            }
            Pending::Close => self.finish_close(cx),
            Pending::Update => self.finish_update(cx),
            Pending::Run(intent) => self.start_selected_run(*intent, window, cx),
            Pending::Git(request) => self.confirm_git(*request, window, cx),
            Pending::CloseWorkspace => self.close_workspace(window, cx),
        }
    }

    pub(super) fn begin_suite_operation(
        &mut self,
        entry: Entry,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy
            || self.runs.read(cx).running()
            || entry.kind != DocumentKind::Suite
            || !matches!(operation, Operation::Rename | Operation::Delete)
        {
            return;
        }
        self.suite_menu_epoch = self.suite_menu_epoch.wrapping_add(1);
        if self.session.open.is_some()
            && let Err(error) = self.flush_drafts(window, cx)
        {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        self.guard(Pending::SuiteOperation(entry, operation), window, cx);
    }

    fn finish_suite_operation(
        &mut self,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match operation {
            Operation::Rename => self.name_dialog(operation, window, cx),
            Operation::Delete => self.confirm_remove(true, window, cx),
            _ => {}
        }
    }

    fn name_dialog(&mut self, operation: Operation, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.workspace.is_none() {
            self.status = "Open a workspace first".into();
            cx.notify();
            return;
        }
        let (title, initial) = match operation {
            Operation::New => ("New suite", "new-suite.tests.yaml".to_string()),
            Operation::Duplicate => ("Duplicate suite", "suite-copy.tests.yaml".to_string()),
            Operation::Rename => (
                "Rename suite",
                self.session
                    .open
                    .as_ref()
                    .and_then(|open| open.entry.path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            Operation::SaveAs => (
                "Save As",
                self.session
                    .open
                    .as_ref()
                    .map(|open| open.entry.label())
                    .unwrap_or_default(),
            ),
            _ => return,
        };
        let selection_end = match operation {
            Operation::New => "new-suite".len(),
            Operation::Rename => self
                .session
                .open
                .as_ref()
                .and_then(|open| open.entry.suite_name())
                .map_or(initial.len(), |name| name.len()),
            _ => initial.len(),
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Filename")
                .default_value(initial)
        });
        let initial_focus = name.clone();
        let display_name = (operation == Operation::Rename).then(|| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Display name")
                    .default_value(self.value["displayName"].as_str().unwrap_or_default())
            })
        });
        let view = cx.entity();
        self.status = title.into();
        self.sync_modal(cx);
        let modal = self.modal.clone();
        // Styled buttons own their focus handles; track the parent for dialog Enter routing.
        let confirm_focus = cx.focus_handle();
        window.open_dialog(cx, move |dialog, window, cx| {
            let name = name.clone();
            let display_name = display_name.clone();
            let submit_name = name.clone();
            let submit_display_name = display_name.clone();
            let submit_focus = confirm_focus.clone();
            let submit = window.listener_for(&view, move |this, _, window, cx| {
                if this.busy {
                    return;
                }
                let input_focused = submit_name.read(cx).focus_handle(cx).is_focused(window)
                    || submit_display_name
                        .as_ref()
                        .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window));
                if !input_focused && !submit_focus.contains_focused(window, cx) {
                    window.close_dialog(cx);
                    return;
                }
                this.commit_named_file(
                    operation,
                    submit_name.read(cx).value().to_string(),
                    submit_display_name
                        .as_ref()
                        .map(|input| input.read(cx).value().to_string()),
                    window,
                    cx,
                );
            });
            let state = modal.read(cx);
            let busy = state.busy;
            dialog
                .title(title)
                .close_button(!busy)
                .keyboard(!busy)
                .overlay_closable(!busy)
                .on_ok(move |event, window, cx| {
                    submit(event, window, cx);
                    false
                })
                .when(operation == Operation::Rename, |dialog| {
                    dialog.child(div().text_sm().child("Filename"))
                })
                .child(Input::new(&name).id("file-name").disabled(busy))
                .when_some(display_name.as_ref(), |dialog, display_name| {
                    dialog.child(div().text_sm().child("Display name")).child(
                        Input::new(display_name)
                            .id("suite-display-name")
                            .disabled(busy),
                    )
                })
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(state.status.clone()),
                )
                .footer(
                    gpui_kit::component::dialog::DialogFooter::new()
                        .child(
                            Button::new("file-cancel")
                                .label("Cancel")
                                .disabled(busy)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            div()
                                .id("file-confirm-focus")
                                .track_focus(&confirm_focus)
                                .child(
                                    Button::new("file-confirm")
                                        .primary()
                                        .label(if operation == Operation::New {
                                            "Create"
                                        } else {
                                            "Save"
                                        })
                                        .disabled(busy)
                                        .on_click(window.listener_for(
                                            &view,
                                            move |this, _, window, cx| {
                                                this.commit_named_file(
                                                    operation,
                                                    name.read(cx).value().to_string(),
                                                    display_name.as_ref().map(|input| {
                                                        input.read(cx).value().to_string()
                                                    }),
                                                    window,
                                                    cx,
                                                );
                                            },
                                        )),
                                ),
                        ),
                )
        });
        initial_focus.update(cx, |input, cx| {
            input.focus(window, cx);
            input.set_selected_range(0..selection_end, cx);
        });
    }

    fn snapshot(&self) -> Result<OpenDocument> {
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
        Ok(OpenDocument {
            entry: open.entry.clone(),
            document,
        })
    }

    fn commit_named_file(
        &mut self,
        operation: Operation,
        name: String,
        display_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(workspace) = self.session.workspace.clone() else {
            return;
        };
        if !self.field_errors.is_empty() {
            self.status = "Correct invalid fields before saving".into();
            cx.notify();
            return;
        }
        let snapshot = if operation == Operation::New {
            None
        } else {
            match self.snapshot() {
                Ok(open) => Some(open),
                Err(error) => {
                    self.status = format!("{error:#}");
                    cx.notify();
                    return;
                }
            }
        };
        if operation == Operation::Rename && self.runs.read(cx).running() {
            return;
        }
        let previous = self
            .session
            .open
            .as_ref()
            .and_then(|open| {
                self.session
                    .suites
                    .iter()
                    .find(|suite| suite.entry.path == open.entry.path)
            })
            .cloned();
        let outputs = self.runs.read(cx).suite_output_roots();
        self.task(
            "Writing file...",
            move || {
                let open = match operation {
                    Operation::New => workspace.create_suite(&name)?,
                    Operation::Duplicate => workspace.duplicate_suite(
                        &snapshot.ok_or_else(|| anyhow!("Open a suite first"))?,
                        &name,
                    )?,
                    Operation::SaveAs => workspace.save_as(
                        &snapshot.ok_or_else(|| anyhow!("Open a document first"))?,
                        std::path::Path::new(&name),
                    )?,
                    Operation::Rename => {
                        let mut open = snapshot.ok_or_else(|| anyhow!("Open a suite first"))?;
                        workspace.rename_suite_in_outputs(
                            &mut open,
                            &name,
                            display_name.as_deref(),
                            &outputs,
                        )?;
                        open
                    }
                    _ => return Err(anyhow!("Unsupported file operation")),
                };
                Ok((open, workspace.entries()?, workspace.suite_catalog()))
            },
            move |this, (open, entries, catalog), window, cx| {
                if operation == Operation::Rename
                    && let Some(previous) = previous
                {
                    let mut updated = previous.clone();
                    updated.entry = open.entry.clone();
                    updated.name = open.entry.suite_name().unwrap_or_default();
                    let value = open.document.value().unwrap_or_default();
                    updated.output_id = value["id"].as_str().unwrap_or(&updated.name).into();
                    updated.display_name = value["displayName"].as_str().map(str::to_owned);
                    this.runs.update(cx, |panel, cx| {
                        panel.suite_changed(&previous, Some(&updated), cx)
                    });
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
                this.session.open = Some(open);
                this.overview = false;
                this.main_tab = 0;
                this.session.entries = entries;
                this.session.selection = Selection::Root;
                this.suite_list.current = this
                    .session
                    .open
                    .as_ref()
                    .map(|open| open.entry.path.clone());
                this.tab = if this.session.open.as_ref().is_some_and(|open| {
                    open.document.format == Format::Text
                        || open.entry.kind == DocumentKind::ReplacementProfiles
                }) {
                    1
                } else {
                    0
                };
                this.sync_editor_content(window, cx);
                this.refresh_results(window, cx);
                this.status = "File saved".into();
                window.close_dialog(cx);
            },
            window,
            cx,
        );
    }

    pub(super) fn confirm_remove(
        &mut self,
        file: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.session.open.is_none() || (file && self.runs.read(cx).running()) {
            return;
        }
        if !file && !self.source_selection_ready(cx) {
            return;
        }
        if file && self.unsaved(cx) {
            self.status = "Save or reload changes before deleting a suite".into();
            cx.notify();
            return;
        }
        let title = if file {
            "Delete suite and its data?"
        } else {
            "Remove selected item?"
        };
        let target = if file {
            self.session
                .open
                .as_ref()
                .map(|open| open.entry.label())
                .unwrap_or_default()
        } else {
            self.value
                .pointer(&self.session.selection.path())
                .and_then(|value| value.get("label"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| self.session.selection.path())
        };
        let view = cx.entity();
        self.sync_modal(cx);
        let modal = self.modal.clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            let busy = modal.read(cx).busy;
            dialog
                .title(title)
                .close_button(false)
                .keyboard(!busy)
                .child(div().id("remove-target").role(Role::Status).aria_label(title).test_support().child(target.clone()))
                .when(file, |dialog| dialog.child("Reference images, test runs, and local reports for this suite will also be deleted."))
                .footer(
                    gpui_kit::component::dialog::DialogFooter::new()
                        .child(
                            Button::new("remove-cancel")
                                .label("Cancel")
                                .disabled(busy)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("remove-confirm")
                                .danger()
                                .label(if file { "Delete" } else { "Remove" })
                                .disabled(busy)
                                .on_click(window.listener_for(
                                    &view,
                                    move |this, _, window, cx| {
                                        if !file {
                                            this.edit(Session::remove_selected, window, cx);
                                            window.close_dialog(cx);
                                            return;
                                        }
                                        let Some(workspace) = this.session.workspace.clone() else {
                                            return;
                                        };
                                        if this.runs.read(cx).running() { return; }
                                        let outputs = this.runs.read(cx).suite_output_roots();
                                        let snapshot = match this.snapshot() {
                                            Ok(snapshot) => snapshot,
                                            Err(error) => {
                                                this.status = format!("{error:#}");
                                                cx.notify();
                                                return;
                                            }
                                        };
                                        let previous = this.session.suites.iter().find(|suite| suite.entry.path == snapshot.entry.path).cloned();
                                        this.task(
                                            "Deleting suite...",
                                            move || {
                                                workspace.delete_suite_in_outputs(&snapshot, &outputs)?;
                                                Ok((workspace.entries()?, workspace.suite_catalog()))
                                            },
                                            move |this, (entries, catalog), window, cx| {
                                                if let Some(previous) = previous {
                                                    this.runs.update(cx, |panel, cx| panel.suite_changed(&previous, None, cx));
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
                                                this.session.open = None;
                                                this.session.entries = entries;
                                                this.session.selection = Selection::Root;
                                                this.suite_list.current = None;
                                                this.sync_editor_content(window, cx);
                                                this.refresh_results(window, cx);
                                                this.status = "Suite deleted".into();
                                                window.close_dialog(cx);
                                            },
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                        ),
                )
        });
    }

    fn create_config(&mut self, viewports: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.unsaved(cx) {
            self.status = "Save the current document first".into();
            cx.notify();
            return;
        }
        let Some(workspace) = self.session.workspace.clone() else {
            return;
        };
        self.task(
            "Creating configuration...",
            move || {
                let (path, kind, source) = if viewports {
                    (
                        "common/_viewports.yaml",
                        DocumentKind::Viewports,
                        "- label: desktop\n  width: 1280\n  height: 720\n",
                    )
                } else {
                    ("regressify.yaml", DocumentKind::Workspace, "{}\n")
                };
                let open = workspace.create_document(std::path::Path::new(path), kind, source)?;
                Ok((open, workspace.entries()?))
            },
            |this, (open, entries), window, cx| {
                this.session.open = Some(open);
                this.session.entries = entries;
                this.session.selection = Selection::Root;
                this.workspace_value = Value::Null;
                this.tab = 0;
                this.sync_editor_content(window, cx);
                this.status = "Configuration created".into();
            },
            window,
            cx,
        );
    }

    fn browse_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.workspace.clone() else {
            return;
        };
        let root = workspace.root.clone();
        cx.spawn_in(window, async move |view, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_directory(&root)
                .add_filter("Configuration", &["yaml", "yml", "json"])
                .pick_file()
                .await
            else {
                return;
            };
            let picked = file.path().to_path_buf();
            let classified = cx
                .background_executor()
                .spawn(async move {
                    let canonical = picked.canonicalize()?;
                    let relative = canonical
                        .strip_prefix(&workspace.root)
                        .map_err(|_| anyhow!("Select a file inside the workspace"))?;
                    workspace.classify_entry(relative)
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                if this.busy
                    || this.session.workspace.as_ref().map(|active| &active.root) != Some(&root)
                {
                    return;
                }
                match classified {
                    Ok(entry) => {
                        if let Some(existing) = this
                            .session
                            .entries
                            .iter_mut()
                            .find(|existing| existing.path == entry.path)
                        {
                            *existing = entry.clone();
                        } else {
                            this.session.entries.push(entry.clone());
                            this.session.entries.sort_by_key(Entry::label);
                        }
                        this.load_document(entry, window, cx);
                    }
                    Err(error) => {
                        this.status = format!("Select a file inside the workspace: {error:#}");
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn refresh_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.session.workspace.clone() else {
            return;
        };
        self.task(
            "Refreshing files...",
            move || Ok((workspace.entries()?, workspace.suite_catalog())),
            |this, (entries, catalog), window, cx| {
                this.session.entries = entries;
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
                this.viewport_choices.invalidate();
                this.source_feedback.invalidate();
                this.sync_suites(cx);
                this.workspace_value = this
                    .session
                    .workspace_value()
                    .ok()
                    .flatten()
                    .unwrap_or(Value::Null);
                this.refresh_effective(window, cx);
                this.status = "Files refreshed".into();
            },
            window,
            cx,
        );
    }
}

pub(super) fn folder_viewer_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "Open in File Explorer"
    } else if cfg!(target_os = "macos") {
        "Open in Finder"
    } else {
        "Open in File Manager"
    }
}

pub(super) fn menu_entries() -> Vec<(&'static str, FileCommand)> {
    [
        ("New suite", Operation::New),
        ("Duplicate suite", Operation::Duplicate),
        ("Rename suite", Operation::Rename),
        ("Save As", Operation::SaveAs),
        ("Reload from disk", Operation::Reload),
        ("Delete suite", Operation::Delete),
        ("Create workspace config", Operation::Config),
        ("Create viewport file", Operation::Viewports),
        ("Open configuration file", Operation::Browse),
        ("Open in VS Code", Operation::OpenVSCode),
        (folder_viewer_label(), Operation::OpenFolder),
        ("Refresh files", Operation::Refresh),
        ("Toggle light/dark theme", Operation::Theme),
        ("Close workspace", Operation::CloseWorkspace),
    ]
    .into_iter()
    .map(|(label, operation)| (label, FileCommand { operation }))
    .collect()
}
