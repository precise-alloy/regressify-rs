use super::*;
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use std::sync::{Arc, LazyLock};

pub(super) type OverviewTableState = TableState<OverviewRows>;

#[derive(Clone, PartialEq)]
struct OverviewRow {
    index: usize,
    path: PathBuf,
    name: String,
    label: String,
    summary: String,
    error: Option<String>,
    scenarios: usize,
    status: &'static str,
    checked: bool,
    partial: bool,
    report: bool,
}

pub(super) struct OverviewRows {
    owner: WeakEntity<Workbench>,
    root: Option<PathBuf>,
    rows: Vec<OverviewRow>,
    disabled: bool,
    running: bool,
    suite_width: Pixels,
}

impl OverviewRows {
    pub(super) fn new(owner: WeakEntity<Workbench>) -> Self {
        Self {
            owner,
            root: None,
            rows: Vec::new(),
            disabled: false,
            running: false,
            suite_width: px(240.),
        }
    }
}

#[derive(Clone, Copy)]
enum OverviewAction {
    Check(bool),
    Edit,
    Report,
    Details,
}

const OVERVIEW_COLUMNS: [&str; 5] = ["selection", "suite", "scenarios", "status", "actions"];

impl TableDelegate for OverviewRows {
    fn columns_count(&self, _: &App) -> usize {
        OVERVIEW_COLUMNS.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, index: usize, _: &App) -> Column {
        Column::new(
            OVERVIEW_COLUMNS[index],
            ["", "SUITE", "SCENARIOS", "STATUS", ""][index],
        )
        .width([px(36.), self.suite_width, px(90.), px(100.), px(152.)][index])
        .p_0()
        .resizable(false)
        .movable(false)
    }
    fn render_th(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<OverviewTableState>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "overview-{}-header",
                OVERVIEW_COLUMNS[index]
            )))
            .test_support()
            .w(self.column(index, cx).width)
            .h_full()
            .flex_shrink_0()
            .px_2()
            .flex()
            .items_center()
            .text_xs()
            .child(["", "SUITE", "SCENARIOS", "STATUS", ""][index])
    }
    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        _: &mut Context<OverviewTableState>,
    ) -> Stateful<Div> {
        div().id(("overview-row", self.rows[row].index))
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        _: &mut Context<OverviewTableState>,
    ) -> impl IntoElement {
        div()
            .id("overview-no-matches")
            .role(Role::Status)
            .aria_label("No matching test suites")
            .test_support()
            .p_3()
            .child("No matching test suites")
    }
    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<OverviewTableState>,
    ) -> impl IntoElement {
        let row = &self.rows[row];
        let root = self.root.clone();
        let path = row.path.clone();
        let owner = self.owner.clone();
        let content = match column {
            0 => design::selection_checkbox(
                ("overview-check", row.index),
                row.checked,
                row.partial,
                self.disabled || self.running || row.error.is_some(),
                cx,
            )
            .accessibility_label(format!("Select suite {}", row.name))
            .on_change(move |checked, _, window, cx| {
                cx.stop_propagation();
                let _ = owner.update(cx, |this, cx| {
                    this.overview_action(
                        &root,
                        &path,
                        OverviewAction::Check(checked == gpui_kit::base::CheckboxState::Checked),
                        window,
                        cx,
                    )
                });
            })
            .into_any_element(),
            1 => div()
                .flex()
                .flex_col()
                .min_w_0()
                .w_full()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(row.label.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(display_path(&row.path)),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(row.summary.clone()),
                )
                .into_any_element(),
            2 => div().child(row.scenarios.to_string()).into_any_element(),
            3 => {
                let variant = match row.status {
                    "Invalid" | "Failed" => gpui_kit::component::tag::TagVariant::Danger,
                    "Running" => gpui_kit::component::tag::TagVariant::Primary,
                    "Passed" => gpui_kit::component::tag::TagVariant::Success,
                    _ => gpui_kit::component::tag::TagVariant::Secondary,
                };
                gpui_kit::component::tag::Tag::new()
                    .with_variant(variant)
                    .outline()
                    .child(row.status)
                    .into_any_element()
            }
            _ => {
                let report_owner = owner.clone();
                let report_root = root.clone();
                let report_path = path.clone();
                let details_owner = owner.clone();
                let details_root = root.clone();
                let details_path = path.clone();
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .w_full()
                    .child(
                        Button::new(("overview-details", row.index))
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Info)
                            .tooltip("Suite details")
                            .accessibility_label(format!("Details: {}", row.name))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                let _ = details_owner.update(cx, |this, cx| {
                                    this.overview_action(
                                        &details_root,
                                        &details_path,
                                        OverviewAction::Details,
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    )
                    .when(row.report, |actions| {
                        actions.child(
                            Button::new(("overview-report", row.index))
                                .ghost()
                                .icon(gpui_kit::assets::IconName::Images)
                                .accessibility_label(format!("Open Report: {}", row.name))
                                .tooltip("Open saved report")
                                .disabled(self.disabled)
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    let _ = report_owner.update(cx, |this, cx| {
                                        this.overview_action(
                                            &report_root,
                                            &report_path,
                                            OverviewAction::Report,
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        )
                    })
                    .child(
                        Button::new(("overview-edit", row.index))
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Pencil)
                            .label("Edit")
                            .disabled(self.disabled)
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                let _ = owner.update(cx, |this, cx| {
                                    this.overview_action(
                                        &root,
                                        &path,
                                        OverviewAction::Edit,
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    )
                    .into_any_element()
            }
        };
        div()
            .id((
                SharedString::from(format!("overview-{}", OVERVIEW_COLUMNS[column])),
                row.index,
            ))
            .test_support()
            .size_full()
            .px_2()
            .flex()
            .items_center()
            .min_w_0()
            .when(column == 3, |cell| {
                cell.role(Role::Status).aria_label(row.status)
            })
            .when(column == 0 || column == 4, |cell| {
                cell.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            })
            .child(content)
    }
}

static APPLICATION_LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../../../assets/icon.png").to_vec(),
    ))
});

impl Workbench {
    pub(super) fn sync_overview_table(&self, cx: &mut Context<Self>) {
        if !self.overview || self.main_tab != 0 {
            return;
        }
        let root = self
            .session
            .workspace
            .as_ref()
            .map(|workspace| workspace.root.clone());
        let query = self.search.read(cx).value().to_lowercase();
        let runs = self.runs.read(cx);
        let disabled = self.busy;
        let running = runs.running();
        let rows = self
            .session
            .suites
            .iter()
            .enumerate()
            .filter(|(_, suite)| {
                query.is_empty()
                    || suite.name.to_lowercase().contains(&query)
                    || suite.label().to_lowercase().contains(&query)
                    || suite
                        .scenarios
                        .iter()
                        .any(|scenario| scenario.label.to_lowercase().contains(&query))
            })
            .map(|(index, suite)| {
                let counts = runs.progress().counts(Some(&suite.output_id));
                let status = if suite.error.is_some() {
                    "Invalid"
                } else if counts.running > 0 {
                    "Running"
                } else if counts.failed > 0 {
                    "Failed"
                } else if counts.passed > 0 {
                    "Passed"
                } else {
                    "Ready"
                };
                let checked = runs.all_selected(&suite.name);
                OverviewRow {
                    index,
                    path: suite.entry.path.clone(),
                    name: suite.name.clone(),
                    label: suite.label().to_owned(),
                    summary: runs.artifact_summary(&suite.name),
                    error: suite.error.clone(),
                    scenarios: suite.scenarios.len(),
                    status,
                    checked,
                    partial: runs.selected(&suite.name) && !checked,
                    report: runs.can_open_saved_suite(&suite.name),
                }
            })
            .collect::<Vec<_>>();
        self.overview_table.update(cx, |table, cx| {
            let old = table.delegate();
            if old.root == root
                && old.rows == rows
                && old.disabled == disabled
                && old.running == running
            {
                return;
            }
            let current = table
                .selected_row()
                .filter(|_| old.root == root)
                .and_then(|index| old.rows.get(index))
                .map(|row| row.path.clone());
            let selected = current
                .as_ref()
                .and_then(|path| rows.iter().position(|row| &row.path == path));
            let delegate = table.delegate_mut();
            delegate.root = root;
            delegate.rows = rows;
            delegate.disabled = disabled;
            delegate.running = running;
            table.refresh(cx);
            if let Some(selected) = selected {
                table.set_selected_row(selected, cx);
            } else {
                table.clear_selection(cx);
            }
        });
    }

    fn overview_action(
        &mut self,
        root: &Option<PathBuf>,
        path: &std::path::Path,
        action: OverviewAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if (!matches!(action, OverviewAction::Details) && self.busy)
            || self
                .session
                .workspace
                .as_ref()
                .map(|workspace| &workspace.root)
                != root.as_ref()
        {
            return;
        }
        let Some(suite) = self
            .session
            .suites
            .iter()
            .find(|suite| suite.entry.path == path)
        else {
            return;
        };
        let name = suite.name.clone();
        match action {
            OverviewAction::Check(checked) => {
                if suite.error.is_none() && !self.runs.read(cx).running() {
                    self.runs
                        .update(cx, |panel, cx| panel.toggle_suite(&name, checked, cx));
                }
            }
            OverviewAction::Edit => self.load_document(suite.entry.clone(), window, cx),
            OverviewAction::Report => {
                if self.runs.read(cx).can_open_saved_suite(&name) {
                    self.runs
                        .update(cx, |panel, cx| panel.open_saved_reports(vec![name], cx));
                }
            }
            OverviewAction::Details => {
                let Some(row) = self
                    .overview_table
                    .read(cx)
                    .delegate()
                    .rows
                    .iter()
                    .find(|row| row.path == path)
                    .cloned()
                else {
                    return;
                };
                let text = format!(
                    "Suite: {}\nFile: {}\nScenarios: {}\nStatus: {}\n\n{}{}",
                    row.label,
                    display_path(&row.path),
                    row.scenarios,
                    row.status,
                    row.summary,
                    row.error
                        .as_ref()
                        .map(|error| format!("\n\nConfiguration error:\n{error}"))
                        .unwrap_or_default()
                );
                let content = cx.new(|cx| {
                    let mut editor = EditorState::new(window, cx)
                        .language("text")
                        .line_number(false);
                    editor.set_value(text.clone(), window, cx);
                    editor
                });
                window.open_dialog(cx, move |dialog, _, _| {
                    let text = text.clone();
                    dialog
                        .title("Suite details")
                        .w(px(640.))
                        .child(
                            div().id("overview-details-dialog").test_support().child(
                                div()
                                    .id("overview-details-text")
                                    .test_support()
                                    .h(px(300.))
                                    .w_full()
                                    .child(
                                        CodeEditor::new(&content)
                                            .aria_label("Suite details text")
                                            .readonly(true)
                                            .size_full(),
                                    ),
                            ),
                        )
                        .footer(
                            gpui_kit::component::dialog::DialogFooter::new()
                                .child(design::CopyText::new(
                                    "overview-details-copy",
                                    text,
                                    "Copy suite details",
                                ))
                                .child(
                                    Button::new("overview-details-close")
                                        .label("Close")
                                        .accessibility_label("Close suite details")
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                ),
                        )
                });
            }
        }
    }

    pub(super) fn show_about(
        &mut self,
        _: &ShowAbout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // AGPL "Appropriate Legal Notices": copyright, no warranty, redistribution terms and license.
        const WARRANTY: &str = "This program comes with ABSOLUTELY NO WARRANTY. It is free software; you may redistribute it under the terms of the license below.";
        const LICENSE: &str = "GNU AGPL v3 or later";
        const LICENSE_URL: &str =
            "https://github.com/precise-alloy/regressify-rs/blob/master/LICENSING.md";
        const SOURCE_URL: &str = "https://github.com/precise-alloy/regressify-rs";
        window.open_dialog(cx, |dialog, _, _| {
            dialog
                .title("About")
                .child(
                    div()
                        .id("about-application")
                        .role(Role::Heading)
                        .aria_label("Regressify Desktop")
                        .test_support()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Regressify Desktop"),
                )
                .child(
                    div()
                        .id("about-version")
                        .role(Role::Status)
                        .aria_label(format!("Version: {}", env!("CARGO_PKG_VERSION")))
                        .test_support()
                        .child(format!("Version: {}", env!("CARGO_PKG_VERSION"))),
                )
                .child(
                    div()
                        .id("about-author")
                        .role(Role::Status)
                        .aria_label("Author and lead developer: Tuyen Pham")
                        .test_support()
                        .child("Author and lead developer: Tuyen Pham"),
                )
                .child(
                    div()
                        .id("about-docs-maintainer")
                        .role(Role::Status)
                        .aria_label("Developer and docs maintainer: Dinh Pham")
                        .test_support()
                        .child("Developer and docs maintainer: Dinh Pham"),
                )
                .child(
                    div()
                        .id("about-developer")
                        .role(Role::Status)
                        .aria_label("Developers: Hung Le, Dung Nguyen Tien")
                        .test_support()
                        .child("Developers: Hung Le, Dung Nguyen Tien"),
                )
                .child(
                    div()
                        .id("about-advisors")
                        .role(Role::Status)
                        .aria_label("Advisors (QA): Nga Nguyen, Hanh Nguyen, Duyen Pham")
                        .test_support()
                        .child("Advisors (QA): Nga Nguyen, Hanh Nguyen, Duyen Pham"),
                )
                .child(
                    div().flex().items_center().gap_1().child("Website:").child(
                        design::ExternalLink::new(
                            "about-website",
                            Some("https://regressify.eshn.dev/"),
                        )
                        .label("https://regressify.eshn.dev/")
                        .accessibility_label("https://regressify.eshn.dev/"),
                    ),
                )
                .child(
                    div()
                        .id("about-copyright")
                        .role(Role::Status)
                        .aria_label(env!("REGRESSIFY_COPYRIGHT"))
                        .test_support()
                        .child(env!("REGRESSIFY_COPYRIGHT")),
                )
                .child(
                    div()
                        .id("about-warranty")
                        .role(Role::Status)
                        .aria_label(WARRANTY)
                        .test_support()
                        .child(WARRANTY),
                )
                .child(
                    div().flex().items_center().gap_1().child("License:").child(
                        design::ExternalLink::new("about-license", Some(LICENSE_URL))
                            .label(LICENSE)
                            .accessibility_label(LICENSE),
                    ),
                )
                .child(
                    div().flex().items_center().gap_1().child("Source:").child(
                        design::ExternalLink::new("about-source", Some(SOURCE_URL))
                            .label(SOURCE_URL)
                            .accessibility_label(SOURCE_URL),
                    ),
                )
                .footer(
                    gpui_kit::component::dialog::DialogFooter::new().child(
                        // DialogClose owns its trigger ID; retain the application's observation scope.
                        div()
                            .id("about-close")
                            .role(Role::Group)
                            .aria_label("About actions")
                            .test_support()
                            .child(
                                gpui_kit::component::dialog::DialogClose::new()
                                    .trigger(|button| button.label("Close")),
                            ),
                    ),
                )
        });
    }

    pub(super) fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let left_padding = px(if cfg!(target_os = "macos") { 80. } else { 12. });
        let controls = window.window_controls();
        let controls_width = if cfg!(target_os = "macos")
            || cfg!(target_family = "wasm")
            || (cfg!(target_os = "linux")
                && !matches!(window.window_decorations(), Decorations::Client { .. }))
        {
            px(0.)
        } else {
            // Kit caption slots match TITLE_BAR_HEIGHT (34px), also verified by the native smoke harness.
            gpui_kit::component::TITLE_BAR_HEIGHT
                * (1 + usize::from(controls.minimize) + usize::from(controls.maximize)) as f32
        };
        let header_width =
            (window.viewport_size().width - left_padding - controls_width).max(px(0.));
        let root = self
            .session
            .workspace
            .as_ref()
            .map(|workspace| display_path(&workspace.root))
            .unwrap_or_else(|| "No workspace open".into());
        let counts = self.runs.read(cx).progress().counts(None);
        let workspace_label = self
            .session
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.root.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "No workspace".into());
        TitleBar::new()
            .h(px(38.))
            .pl(left_padding)
            .bg(cx.theme().title_bar)
            .on_close_window(cx.listener(|this, _, window, cx| {
                if this.git.read(cx).active() {
                    this.git.update(cx, |panel, cx| panel.cancel_operation(cx));
                } else if this.runs.read(cx).running() {
                    this.runs.update(cx, |panel, cx| panel.cancel(cx));
                } else if !this.busy {
                    this.guard(files::Pending::Close, window, cx);
                }
            }))
            .child(
                div()
                    .id("workspace-titlebar")
                    .test_support()
                    .flex()
                    .flex_1()
                    .w_0()
                    .min_w_0()
                    .max_w(header_width)
                    .h_full()
                    .items_center()
                    .gap_0()
                    .whitespace_nowrap()
                    .pr_2()
                    .child(
                        div()
                            .id("titlebar-brand")
                            .test_support()
                            .flex()
                            .items_center()
                            .gap_1()
                            .flex_shrink_0()
                            .mr_4()
                            .child(
                                div()
                                    .id("application-logo")
                                    .role(Role::Image)
                                    .aria_label("Regressify logo")
                                    .test_support()
                                    .size(px(22.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        img(Arc::clone(&APPLICATION_LOGO))
                                            .size_full()
                                            .object_fit(ObjectFit::Contain),
                                    ),
                            )
                            .child(
                                div()
                                    .id("application-name")
                                    .test_support()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_sm()
                                    .line_height(relative(1.25))
                                    .child("Regressify"),
                            )
                            .when(window.viewport_size().width >= px(1080.), |brand| {
                                brand.child(
                                    div()
                                        .id("application-version")
                                        .role(Role::Status)
                                        .aria_label(format!("v{}", env!("CARGO_PKG_VERSION")))
                                        .test_support()
                                        .text_sm()
                                        .line_height(relative(1.25))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                                )
                            }),
                    )
                    .child(if cfg!(target_os = "macos") {
                        div()
                            .id("workbench-app-menu")
                            .test_support()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .mr_4()
                            .child(design::titlebar_control(self.file_menu(cx)))
                            .child(design::titlebar_control({
                                let focus = self.focus.clone();
                                Button::new("publishing-menu")
                                    .ghost()
                                    .label("Publish")
                                    .disabled(self.busy || self.runs.read(cx).running())
                                    .dropdown_menu(move |menu, window, cx| {
                                        design::restore_dropdown_menu_focus(
                                            menu.focus_handle(cx),
                                            focus.clone(),
                                            window,
                                            cx,
                                        );
                                        menu.action_context(focus.clone())
                                            .menu("Landing apps", Box::new(OpenLandingApps))
                                            .menu(
                                                "Deployment tokens",
                                                Box::new(OpenDeploymentTokens),
                                            )
                                    })
                            }))
                            .child(design::titlebar_control({
                                let focus = self.focus.clone();
                                Button::new("help-menu")
                                    .ghost()
                                    .label("Help")
                                    .dropdown_menu(move |menu, window, cx| {
                                        design::restore_dropdown_menu_focus(
                                            menu.focus_handle(cx),
                                            focus.clone(),
                                            window,
                                            cx,
                                        );
                                        menu.action_context(focus.clone())
                                            .menu("About", Box::new(ShowAbout))
                                    })
                            }))
                            .into_any_element()
                    } else {
                        div()
                            .id("workbench-app-menu")
                            .test_support()
                            .flex()
                            .items_center()
                            .h_full()
                            .flex_shrink_0()
                            .mr_4()
                            .track_focus(&self.app_menu_focus)
                            .child(self.app_menu_bar.clone())
                            .into_any_element()
                    })
                    .child(
                        div()
                            .id("titlebar-view-group")
                            .test_support()
                            .flex()
                            .items_center()
                            .flex_shrink_0()
                            .mr_4()
                            .child(design::titlebar_control(
                                Button::new("toggle-navigation")
                                    .ghost()
                                    .icon(if self.preferences.navigator_hidden {
                                        gpui_kit::assets::IconName::PanelLeftOpen
                                    } else {
                                        gpui_kit::assets::IconName::PanelLeftClose
                                    })
                                    .tooltip("Toggle navigator")
                                    .accessibility_label(if self.preferences.navigator_hidden {
                                        "Show navigator"
                                    } else {
                                        "Hide navigator"
                                    })
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_navigator(&ToggleNavigator, window, cx)
                                    })),
                            ))
                            .child(design::titlebar_control(
                                div()
                                    .id("titlebar-navigation")
                                    .test_support()
                                    // Balance the underline tab's bottom border and asymmetric inner margins.
                                    .mt(px(3.))
                                    .child(
                                        TabBar::new("workbench-tabs")
                                            .underline()
                                            .small()
                                            .selected_index(self.main_tab)
                                            .on_click(cx.listener(|this, index, window, cx| {
                                                this.select_view(*index, window, cx)
                                            }))
                                            .children(
                                                ["Editor", "Runs", "Results"]
                                                    .into_iter()
                                                    .enumerate()
                                                    .map(|(index, label)| {
                                                        Tab::new().aria_label(label).child(
                                                            div()
                                                                .id(("titlebar-tab-label", index))
                                                                .test_support()
                                                                .text_sm()
                                                                .line_height(relative(1.25))
                                                                .child(label),
                                                        )
                                                    }),
                                            ),
                                    ),
                            )),
                    )
                    .child(
                        // A long workspace name must not set the title bar's intrinsic minimum width.
                        div().flex_1().w_0().min_w_0().when(
                            window.viewport_size().width >= px(1400.),
                            |workspace| {
                                workspace.child(
                                    div()
                                        .id("workspace-path")
                                        .role(Role::Status)
                                        .aria_label(root.clone())
                                        .test_support()
                                        .truncate()
                                        .text_sm()
                                        .line_height(relative(1.25))
                                        .text_color(cx.theme().foreground)
                                        .child(workspace_label),
                                )
                            },
                        ),
                    )
                    .when(self.runs.read(cx).running(), |header| {
                        header.child(design::titlebar_control(
                            Button::new("view-running")
                                .ghost()
                                .icon(gpui_kit::assets::IconName::Activity)
                                .label(format!("{} / {}", counts.running, counts.complete()))
                                .tooltip("View running command")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.main_tab = 1;
                                    cx.notify();
                                })),
                        ))
                    })
                    .child(design::titlebar_control(
                        div()
                            .id("status")
                            .role(Role::Status)
                            .aria_label(self.status.clone())
                            .test_support()
                            .child(
                                Button::new("application-status")
                                    .ghost()
                                    .icon(gpui_kit::assets::IconName::Info)
                                    .accessibility_label("Application status")
                                    .tooltip(format!(
                                        "{}\n{}",
                                        self.status,
                                        if self.busy {
                                            "Working"
                                        } else if self.unsaved(cx) {
                                            "Modified"
                                        } else {
                                            "Ready"
                                        }
                                    )),
                            ),
                    ))
                    .child(design::titlebar_control(
                        Button::new("open-commands")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Command)
                            .accessibility_label("Commands")
                            .tooltip("Commands")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_commands(&OpenCommands, window, cx)
                            })),
                    ))
                    .child(design::titlebar_control(self.render_update_button(cx)))
                    .when(self.session.workspace.is_some(), |header| {
                        let git = self.git.read(cx);
                        let git_owner = self.git.downgrade();
                        header.child(design::titlebar_control(
                            div()
                                .id("git-status-hover")
                                .tooltip(move |window, cx| {
                                    let git_owner = git_owner.clone();
                                    gpui_kit::component::tooltip::Tooltip::element(
                                        move |window, cx| {
                                            let summary = git_owner
                                                .upgrade()
                                                .map(|git| git.read(cx).summary())
                                                .unwrap_or_default();
                                            div()
                                                .id("git-status-tooltip")
                                                .role(Role::Label)
                                                .aria_label(summary.clone())
                                                .aria_value(summary.clone())
                                                .test_support()
                                                .w((window.viewport_size().width - px(64.))
                                                    .min(px(640.)))
                                                .whitespace_normal()
                                                .child(summary.clone())
                                        },
                                    )
                                    .build(window, cx)
                                })
                                .child(
                                    Button::new("git-status")
                                        .ghost()
                                        .icon(gpui_kit::assets::IconName::GitBranch)
                                        .max_w(px(112.))
                                        .child(div().min_w_0().truncate().child(git.badge()))
                                        .accessibility_label(format!("Git: {}", git.summary()))
                                        .selected(self.main_tab == 3)
                                        .when(git.attention(), |button| {
                                            button.text_color(cx.theme().warning)
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.select_view(3, window, cx)
                                        })),
                                ),
                        ))
                    })
                    .child(design::titlebar_control(self.render_deploy_controls(cx)))
                    .child(design::titlebar_control(self.render_theme_picker()))
                    .child(design::titlebar_control(
                        Button::new("open-workspace")
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .tooltip(if self.session.workspace.is_some() {
                                "Switch workspace"
                            } else {
                                "Open workspace"
                            })
                            .accessibility_label(if self.session.workspace.is_some() {
                                "Switch workspace"
                            } else {
                                "Open workspace"
                            })
                            .disabled(self.busy || self.runs.read(cx).running())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_workspace(&OpenWorkspace, window, cx)
                            })),
                    )),
            )
            .into_any_element()
    }

    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(workspace) = &self.session.workspace else {
            return div()
                .id("overview-empty")
                .role(Role::Status)
                .aria_label("No workspace open")
                .test_support()
                .flex()
                .size_full()
                .child(
                    gpui_kit::component::empty::Empty::new()
                        .size_full()
                        .child(
                            Icon::new(gpui::assets::IconName::Folders)
                                .size(px(48.))
                                .text_color(cx.theme().primary),
                        )
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("No workspace open"),
                        )
                        .child(
                            Button::new("overview-open")
                                .primary()
                                .icon(IconName::FolderOpen)
                                .label("Open workspace")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_workspace(&OpenWorkspace, window, cx)
                                })),
                        ),
                )
                .into_any_element();
        };
        let name = workspace
            .root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let scenarios = self
            .session
            .suites
            .iter()
            .map(|suite| suite.scenarios.len())
            .sum::<usize>();
        let invalid = self
            .session
            .suites
            .iter()
            .filter(|suite| suite.error.is_some())
            .count();
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("WORKSPACE"),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(name),
                            ),
                    )
                    .child(
                        Button::new("overview-new-suite")
                            .primary()
                            .icon(IconName::Plus)
                            .label("New suite")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.guard(files::Pending::Name(files::Operation::New), window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_4()
                    .border_y_1()
                    .border_color(cx.theme().border)
                    .py_2()
                    .child(self.overview_metric(
                        "Test suites",
                        self.session.suites.len(),
                        cx.theme().foreground,
                    ))
                    .child(self.overview_metric("Scenarios", scenarios, cx.theme().foreground))
                    .child(self.overview_metric(
                        "Selected",
                        self.runs.read(cx).selected_count(),
                        cx.theme().primary,
                    ))
                    .child(self.overview_metric(
                        "Config issues",
                        invalid,
                        if invalid == 0 {
                            cx.theme().muted_foreground
                        } else {
                            cx.theme().danger
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Test suites"))
                    .child(div().flex_1())
                    .child(self.cleanup_button("overview-cleanup", cx))
                    .child(
                        Button::new("overview-run-reference")
                            .icon(gpui_kit::assets::IconName::Camera)
                            .label("Run Reference")
                            .disabled(
                                self.busy
                                    || self.runs.read(cx).selected_count() == 0
                                    || self.runs.read(cx).running(),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request_selected_command(
                                    crate::desktop::runner::RunCommand::Reference,
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("overview-run-selected")
                            .primary()
                            .icon(gpui_kit::assets::IconName::Play)
                            .label(format!(
                                "Run Test ({} scenarios)",
                                self.runs.read(cx).selected_scenario_count()
                            ))
                            .disabled(self.busy || !self.runs.read(cx).can_test_selection())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request_selected_command(
                                    crate::desktop::runner::RunCommand::Test,
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .when(self.runs.read(cx).can_open_saved_selection(), |row| {
                        row.child(
                            Button::new("overview-open-report")
                                .icon(gpui_kit::assets::IconName::Images)
                                .label("Open Report")
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let names = this.runs.read(cx).selected_names();
                                    this.runs.update(cx, |panel, cx| {
                                        panel.open_saved_reports(names, cx)
                                    });
                                })),
                        )
                    }),
            );
        let table = self.overview_table.downgrade();
        page = page.child(
            div()
                .id("overview-table")
                .test_support()
                .relative()
                .flex_1()
                .min_h(px(120.))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.modifiers == Modifiers::default()
                        && event.keystroke.key == "enter"
                        && this
                            .overview_table
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window)
                    {
                        let table = this.overview_table.read(cx);
                        let target = table
                            .selected_row()
                            .and_then(|index| table.delegate().rows.get(index))
                            .map(|row| (table.delegate().root.clone(), row.path.clone()));
                        if let Some((root, path)) = target {
                            this.overview_action(&root, &path, OverviewAction::Details, window, cx);
                            window.prevent_default();
                            cx.stop_propagation();
                        }
                    }
                }))
                .child(
                    canvas(
                        |bounds, _, _| bounds,
                        move |_, bounds, window, cx| {
                            let width = (bounds.size.width - px(390.)).max(px(140.));
                            let table = table.clone();
                            window.defer(cx, move |_, cx| {
                                let _ = table.update(cx, |table, cx| {
                                    if table.delegate().suite_width != width {
                                        table.delegate_mut().suite_width = width;
                                        table.refresh(cx);
                                    }
                                });
                            });
                        },
                    )
                    .absolute()
                    .size_full(),
                )
                .child(
                    DataTable::new(&self.overview_table)
                        .bordered(false)
                        .header_row_height(gpui_kit::component::Size::Medium.table_row_height())
                        .with_size(gpui_kit::component::Size::Size(px(64.))),
                ),
        );
        if let Some(error) = &self.session.suite_error {
            page = page.child(div().text_color(cx.theme().danger).child(error.clone()));
        }
        if self.session.suites.is_empty() {
            page = page.child(
                div()
                    .py_8()
                    .text_color(cx.theme().muted_foreground)
                    .child("No test suites"),
            );
        }
        div()
            .id("workspace-overview-scroll")
            .size_full()
            .child(page)
            .into_any_element()
    }

    fn overview_metric(&self, label: &'static str, count: usize, color: Hsla) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(100.))
            .gap_1()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(count.to_string()),
            )
            .child(div().text_xs().child(label))
            .into_any_element()
    }
}
