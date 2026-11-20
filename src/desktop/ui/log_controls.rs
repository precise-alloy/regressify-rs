use super::*;
use crate::{
    desktop::logs::LogQuery,
    diagnostics::{
        journal::ArchiveState,
        record::{Severity, Stage},
    },
    test::capture::CaptureLog,
};

const LEVELS: [Option<Severity>; 6] = [
    None,
    Some(Severity::Trace),
    Some(Severity::Debug),
    Some(Severity::Info),
    Some(Severity::Warn),
    Some(Severity::Error),
];
const STAGES: [Option<Stage>; 14] = [
    None,
    Some(Stage::QueueSetup),
    Some(Stage::Navigation),
    Some(Stage::StateHooks),
    Some(Stage::ActionsFrames),
    Some(Stage::ScreenshotSettling),
    Some(Stage::CaptureArtifacts),
    Some(Stage::Finish),
    Some(Stage::Preflight),
    Some(Stage::SnapshotStaging),
    Some(Stage::PackagingOptimization),
    Some(Stage::ApiUploads),
    Some(Stage::RetryPolling),
    Some(Stage::CompletionCleanup),
];

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
struct ChooseLogLevel {
    index: usize,
}

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
struct ChooseLogStage {
    index: usize,
}

pub(super) enum LogControlEvent {
    Filter(LogQuery),
    Follow(bool),
    Copy,
    Export(PathBuf),
}

pub(super) struct LogControls {
    pub(super) search: Entity<InputState>,
    query: LogQuery,
    follow: bool,
    live: bool,
    state: Option<ArchiveState>,
    busy: bool,
    status: String,
    focus: FocusHandle,
    _subscription: Subscription,
    #[cfg(test)]
    destination: Option<PathBuf>,
}

impl EventEmitter<LogControlEvent> for LogControls {}

impl LogControls {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search log"));
        let subscription = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.query.search = this.search.read(cx).value().to_string();
                this.query.offset = 0;
                cx.emit(LogControlEvent::Filter(this.query.clone()));
                cx.notify();
            }
        });
        Self {
            search,
            query: Default::default(),
            follow: true,
            live: false,
            state: None,
            busy: false,
            status: String::new(),
            focus: cx.focus_handle(),
            _subscription: subscription,
            #[cfg(test)]
            destination: None,
        }
    }

    pub(super) fn availability(
        &mut self,
        state: Option<ArchiveState>,
        live: bool,
        cx: &mut Context<Self>,
    ) {
        self.state = state;
        self.live = live;
        cx.notify();
    }

    pub(super) fn export(
        &mut self,
        output: PathBuf,
        sources: Vec<CaptureLog>,
        query: LogQuery,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.state != Some(ArchiveState::Complete) {
            return;
        }
        self.busy = true;
        self.status = "Exporting log...".into();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::desktop::logs::export_complete(&output, &sources, &query, &path)
                })
                .await;
            let _ = view.update(cx, |this, cx| {
                this.busy = false;
                this.status = match result {
                    Ok(()) => "Log exported".into(),
                    Err(_) => "Log export failed; destination unchanged".into(),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn download(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.state != Some(ArchiveState::Complete) {
            return;
        }
        #[cfg(test)]
        if let Some(path) = self.destination.clone() {
            cx.emit(LogControlEvent::Export(path));
            return;
        }
        cx.spawn(async move |view, cx| {
            if let Some(file) = rfd::AsyncFileDialog::new()
                .set_file_name("capture-log.txt")
                .add_filter("Text log", &["txt"])
                .save_file()
                .await
            {
                let _ = view.update(cx, |_, cx| {
                    cx.emit(LogControlEvent::Export(file.path().to_owned()))
                });
            }
        })
        .detach();
    }

    #[cfg(test)]
    pub(in crate::desktop::ui) fn export_destination(&mut self, path: PathBuf) {
        self.destination = Some(path);
    }
}

impl Render for LogControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus.clone();
        let stage_focus = focus.clone();
        let level = self.query.severity;
        let stage = self.query.stage;
        let reason = self
            .state
            .map(ArchiveState::label)
            .unwrap_or("Loading archive");
        div()
            .id("log-controls")
            .track_focus(&self.focus)
            .role(Role::Group)
            .aria_label("Log controls")
            .test_support()
            .min_w_0()
            .on_action(cx.listener(|this, action: &ChooseLogLevel, _, cx| {
                if let Some(level) = LEVELS.get(action.index) {
                    this.query.severity = *level;
                    this.query.offset = 0;
                    cx.emit(LogControlEvent::Filter(this.query.clone()));
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, action: &ChooseLogStage, _, cx| {
                if let Some(stage) = STAGES.get(action.index) {
                    this.query.stage = *stage;
                    this.query.offset = 0;
                    cx.emit(LogControlEvent::Filter(this.query.clone()));
                    cx.notify();
                }
            }))
            .child(design::ToolbarFocus::new(
                ("log-toolbar-focus", cx.entity().entity_id()),
                gpui_kit::component::toolbar::Toolbar::new((
                    "log-command-toolbar",
                    cx.entity().entity_id(),
                ))
                .with_size(gpui_kit::component::Size::Medium)
                .content_height()
                .flex_wrap()
                .content(
                    Button::new("log-level-filter")
                        .ghost()
                        .icon(gpui_kit::assets::IconName::List)
                        .label(
                            level
                                .map(|level| format!("{level:?}"))
                                .unwrap_or_else(|| "All levels".into()),
                        )
                        .prepare_for_toolbar()
                        .with_size(gpui_kit::component::Size::Medium)
                        .dropdown_menu(move |menu, window, cx| {
                            super::design::restore_dropdown_menu_focus(
                                menu.focus_handle(cx),
                                focus.clone(),
                                window,
                                cx,
                            );
                            let mut menu = menu.action_context(focus.clone());
                            for (index, choice) in LEVELS.iter().enumerate() {
                                menu = menu.menu_with_check_and_disabled(
                                    choice
                                        .map(|choice| format!("{choice:?}"))
                                        .unwrap_or_else(|| "All levels".into()),
                                    *choice == level,
                                    Box::new(ChooseLogLevel { index }),
                                    false,
                                );
                            }
                            menu
                        }),
                )
                .content(
                    Button::new("log-stage-filter")
                        .ghost()
                        .icon(gpui_kit::assets::IconName::List)
                        .label(
                            stage
                                .map(|stage| format!("{stage:?}"))
                                .unwrap_or_else(|| "All stages".into()),
                        )
                        .prepare_for_toolbar()
                        .with_size(gpui_kit::component::Size::Medium)
                        .dropdown_menu(move |menu, window, cx| {
                            super::design::restore_dropdown_menu_focus(
                                menu.focus_handle(cx),
                                stage_focus.clone(),
                                window,
                                cx,
                            );
                            let mut menu =
                                menu.action_context(stage_focus.clone()).scrollable(true);
                            for (index, choice) in STAGES.iter().enumerate() {
                                menu = menu.menu_with_check_and_disabled(
                                    choice
                                        .map(|choice| format!("{choice:?}"))
                                        .unwrap_or_else(|| "All stages".into()),
                                    *choice == stage,
                                    Box::new(ChooseLogStage { index }),
                                    false,
                                );
                            }
                            menu
                        }),
                )
                .child(
                    Checkbox::new("log-follow-tail")
                        .label("Follow")
                        .checked(self.follow)
                        .disabled(!self.live)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            this.follow = *checked;
                            cx.emit(LogControlEvent::Follow(*checked));
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("log-copy-page")
                        .ghost()
                        .icon(IconName::Copy)
                        .tooltip("Copy displayed page")
                        .accessibility_label("Copy displayed page")
                        .disabled(self.state != Some(ArchiveState::Complete))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(LogControlEvent::Copy))),
                )
                .child(
                    Button::new("log-download")
                        .ghost()
                        .icon(gpui_kit::assets::IconName::Download)
                        .tooltip(if self.state == Some(ArchiveState::Complete) {
                            "Download complete filtered log".to_owned()
                        } else {
                            format!("Export unavailable: {reason}")
                        })
                        .accessibility_label("Download complete filtered log")
                        .disabled(self.busy || self.state != Some(ArchiveState::Complete))
                        .on_click(cx.listener(|this, _, _, cx| this.download(cx))),
                )
                .content(
                    div().id("log-search").w(px(200.)).min_w(px(120.)).child(
                        Input::new(&self.search).with_size(gpui_kit::component::Size::Medium),
                    ),
                )
                .when(
                    self.state
                        .is_some_and(|state| state != ArchiveState::Complete)
                        && !self.live,
                    |bar| {
                        let warning =
                            format!("Log unavailable or incomplete: {reason}. Export disabled.");
                        bar.content(
                            div()
                                .id("log-health-warning")
                                .role(Role::Status)
                                .aria_label(warning.clone())
                                .test_support()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(warning),
                        )
                    },
                )
                .content(
                    div()
                        .id("log-export-status")
                        .role(Role::Status)
                        .aria_label(self.status.clone())
                        .test_support()
                        .text_xs()
                        .child(self.status.clone()),
                ),
            ))
    }
}
