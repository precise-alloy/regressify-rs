//! Separate suite selection from the open document's scenario outline.

use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::resizable::v_resizable;

use super::*;

pub(super) type WorkspaceListState = gpui_kit::component::list::ListState<WorkspaceFiles>;

pub(super) struct WorkspaceFiles {
    owner: WeakEntity<Workbench>,
    root: Option<PathBuf>,
    rows: Vec<(usize, Entry)>,
    pub(super) current: Option<PathBuf>,
    disabled: bool,
}

impl WorkspaceFiles {
    pub(super) fn new(owner: WeakEntity<Workbench>) -> Self {
        Self {
            owner,
            root: None,
            rows: Vec::new(),
            current: None,
            disabled: false,
        }
    }
}

impl gpui_kit::component::list::ListDelegate for WorkspaceFiles {
    type Item = gpui_kit::component::list::ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.rows.len()
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        _: &mut Context<WorkspaceListState>,
    ) -> Option<Self::Item> {
        let (original, entry) = self.rows.get(index.row)?;
        let label = entry.label();
        let tooltip = if entry.supports_editing() {
            label.clone()
        } else {
            format!("{label} (not editable UTF-8 text; 4 MiB limit)")
        };
        Some(
            gpui_kit::component::list::ListItem::new(SharedString::from(format!(
                "workspace-file-{}",
                label
            )))
            .disabled(self.disabled || !entry.supports_editing())
            .accessibility_label(label.clone())
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .child(
                div()
                    .id(("file", *original))
                    .role(Role::ListBoxOption)
                    .aria_label(label.clone())
                    .test_support()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(Icon::new(gpui_kit::assets::IconName::FileCog))
                    .child(div().flex_1().min_w_0().truncate().child(label)),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<WorkspaceListState>,
    ) {
        self.current = index.and_then(|index| {
            self.rows
                .get(index.row)
                .map(|(_, entry)| entry.path.clone())
        });
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<WorkspaceListState>) {
        if self.disabled {
            return;
        }
        let Some(entry) = self
            .rows
            .iter()
            .find(|(_, entry)| Some(&entry.path) == self.current.as_ref())
            .map(|(_, entry)| entry.clone())
            .filter(Entry::supports_editing)
        else {
            return;
        };
        let owner = self.owner.clone();
        let root = self.root.clone();
        window.defer(cx, move |window, cx| {
            let _ = owner.update(cx, |this, cx| {
                if this.busy
                    || this
                        .session
                        .workspace
                        .as_ref()
                        .map(|workspace| &workspace.root)
                        != root.as_ref()
                {
                    return;
                }
                if let Some(current) = this
                    .session
                    .entries
                    .iter()
                    .find(|current| current.path == entry.path && current.supports_editing())
                    .cloned()
                {
                    this.open_location(current, Selection::Root, window, cx);
                }
            });
        });
    }
}

fn empty_list(id: &'static str, text: &'static str, cx: &App) -> AnyElement {
    div()
        .id(id)
        .role(Role::Status)
        .aria_label(text)
        .test_support()
        .px_3()
        .py_5()
        .text_sm()
        .text_center()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

fn page_location(url: &str, root: Option<&std::path::Path>) -> String {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return url.to_string();
    };
    if parsed.scheme() == "file"
        && let Ok(path) = parsed.to_file_path()
    {
        let path = display_path(&path);
        return root
            .and_then(|root| {
                path.strip_prefix(&format!("{}/", display_path(root).trim_end_matches('/')))
                    .map(str::to_string)
            })
            .unwrap_or(path);
    }
    if matches!(parsed.scheme(), "http" | "https") {
        let origin = parsed.origin().ascii_serialization();
        let host = origin
            .split_once("://")
            .map_or(origin.as_str(), |(_, host)| host);
        let mut location = format!("{host}{}", parsed.path());
        if let Some(query) = parsed.query() {
            location.push('?');
            location.push_str(query);
        }
        if let Some(fragment) = parsed.fragment() {
            location.push('#');
            location.push_str(fragment);
        }
        return location;
    }
    url.to_string()
}

/// Which suites the suite list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SuiteFilter {
    #[default]
    All,
    Selected,
    /// On the deployment target's site with newer local results.
    Outdated,
}

impl Workbench {
    fn select_visible_suites(&mut self, visible: &[String], cx: &mut Context<Self>) {
        if self.busy || visible.is_empty() {
            return;
        }
        self.runs.update(cx, |panel, cx| {
            panel.select_suite(&visible[0], visible, Modifiers::default(), cx);
            panel.select_suite(
                visible.last().unwrap(),
                visible,
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                cx,
            );
        });
    }

    fn move_suite_list(
        &mut self,
        action: &lists::MoveListSelection,
        visible: &[PathBuf],
        names: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() {
            return;
        }
        let fallback = if self.main_tab == 2 {
            self.results.read(cx).active_suite().cloned()
        } else {
            self.session
                .open
                .as_ref()
                .map(|open| open.entry.path.clone())
        };
        let Some(path) = self.suite_list.move_to(action, visible, fallback.as_ref()) else {
            return;
        };
        if !action.focus_only()
            && let Some(entry) = self.session.entries.iter().find(|entry| entry.path == path)
            && let Some(name) = entry.suite_name()
        {
            self.runs.update(cx, |panel, cx| {
                panel.select_suite(&name, names, action.modifiers(), cx);
            });
        }
        window.focus(&self.suite_list.focus, cx);
        cx.notify();
    }

    fn activate_suite_list(
        &mut self,
        visible: &[PathBuf],
        names: &[String],
        toggle: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() {
            return;
        }
        let fallback = self.session.open.as_ref().map(|open| &open.entry.path);
        let Some(path) = self.suite_list.current_item(visible, fallback) else {
            return;
        };
        let Some(entry) = self
            .session
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .cloned()
        else {
            return;
        };
        self.suite_list.focus_item(path, window, cx);
        if toggle {
            if let Some(name) = entry.suite_name() {
                self.runs.update(cx, |panel, cx| {
                    panel.select_suite(
                        &name,
                        names,
                        Modifiers {
                            control: true,
                            ..Default::default()
                        },
                        cx,
                    );
                });
            }
        } else if self.main_tab == 2 {
            self.results
                .update(cx, |panel, cx| panel.select_suite(&entry, window, cx));
        } else {
            self.open_location(entry, Selection::Root, window, cx);
        }
        cx.notify();
    }

    fn select_visible_scenarios(
        &mut self,
        visible: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(first) = visible.first() {
            self.select_open_scenario(*first, visible, Modifiers::default(), window, cx);
            self.select_open_scenario(
                *visible.last().unwrap(),
                visible,
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                window,
                cx,
            );
        }
    }

    fn scenario_list_items(&self, visible: &[usize]) -> Vec<(PathBuf, usize)> {
        self.session.open.as_ref().map_or_else(Vec::new, |open| {
            visible
                .iter()
                .map(|index| (open.entry.path.clone(), *index))
                .collect()
        })
    }

    fn scenario_list_fallback(&self) -> Option<(PathBuf, usize)> {
        let open = self.session.open.as_ref()?;
        let index = match self.session.selection {
            Selection::Viewport(index) => Some(index),
            selection => selection.scenario(),
        }?;
        Some((open.entry.path.clone(), index))
    }

    fn move_scenario_list(
        &mut self,
        action: &lists::MoveListSelection,
        visible: &[usize],
        viewport: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() {
            return;
        }
        let items = self.scenario_list_items(visible);
        let fallback = self.scenario_list_fallback();
        let Some((_, index)) = self
            .scenario_list
            .move_to(action, &items, fallback.as_ref())
        else {
            return;
        };
        if !action.focus_only() {
            if !viewport {
                self.select_open_scenario(index, visible, action.modifiers(), window, cx);
            }
            self.select(
                if viewport {
                    Selection::Viewport(index)
                } else {
                    Selection::Scenario(index)
                },
                window,
                cx,
            );
        }
        window.focus(&self.scenario_list.focus, cx);
        cx.notify();
    }

    fn activate_scenario_list(
        &mut self,
        visible: &[usize],
        viewport: bool,
        toggle: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.runs.read(cx).running() {
            return;
        }
        let items = self.scenario_list_items(visible);
        let fallback = self.scenario_list_fallback();
        let Some(item) = self.scenario_list.current_item(&items, fallback.as_ref()) else {
            return;
        };
        let index = item.1;
        self.scenario_list.focus_item(item, window, cx);
        if toggle && !viewport {
            self.select_open_scenario(
                index,
                visible,
                Modifiers {
                    control: true,
                    ..Default::default()
                },
                window,
                cx,
            );
        } else {
            self.select(
                if viewport {
                    Selection::Viewport(index)
                } else {
                    Selection::Scenario(index)
                },
                window,
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn render_navigation(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut navigation = div()
            .id("navigator")
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .bg(cx.theme().muted.opacity(0.4))
            .when(self.main_tab != 2, |navigation| {
                navigation.child(
                    div().px_2().py_2().child(
                        Button::new("workspace-overview")
                            .ghost()
                            .w_full()
                            .justify_start()
                            .icon(gpui_kit::assets::IconName::LayoutDashboard)
                            .accessibility_label("Overview")
                            .child(div().flex_1().text_left().child("Overview"))
                            .selected(self.overview && self.main_tab == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.overview = true;
                                this.main_tab = 0;
                                cx.notify();
                            })),
                    ),
                )
            });
        if self.session.workspace.is_none() {
            navigation = navigation.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("RECENT WORKSPACES"),
            );
            for (index, path) in self.preferences.recent_workspaces.iter().enumerate() {
                let path = path.clone();
                let label = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                navigation = navigation.child(
                    Button::new(("recent", index))
                        .ghost()
                        .justify_start()
                        .w_full()
                        .min_w_0()
                        .accessibility_label(label.clone())
                        .tooltip(display_path(&path))
                        .child(div().flex_1().min_w_0().truncate().text_left().child(label))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.load_workspace(path.clone(), window, cx)
                        })),
                );
            }
            return navigation.into_any_element();
        }
        navigation
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable("navigation-panes")
                        .child(
                            resizable_panel()
                                .size(px(280.))
                                .size_range(
                                    px(if self.workspace_files_open {
                                        150.
                                    } else {
                                        256.
                                    })..px(800.),
                                )
                                .child(self.render_suite_list(cx)),
                        )
                        .child(resizable_panel().size_range(px(120.)..px(1200.)).child(
                            if self.main_tab == 2 {
                                self.captures.clone().into_any_element()
                            } else {
                                self.render_scenario_list(cx)
                            },
                        )),
                ),
            )
            .when(self.main_tab != 2, |navigation| {
                navigation.child(self.render_workspace_files(window, cx))
            })
            .into_any_element()
    }

    fn render_suite_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let runs = self.runs.read(cx);
        let valid = self
            .session
            .suites
            .iter()
            .filter(|suite| suite.error.is_none())
            .count();
        let entries = self
            .session
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.kind == DocumentKind::Suite)
            .filter_map(|(index, entry)| {
                let open = self
                    .session
                    .open
                    .as_ref()
                    .is_some_and(|open| open.entry.path == entry.path);
                let suite = self
                    .session
                    .suites
                    .iter()
                    .find(|suite| suite.entry.path == entry.path);
                let shown = match self.suite_filter {
                    SuiteFilter::All => true,
                    SuiteFilter::Selected => suite.is_some_and(|suite| runs.selected(&suite.name)),
                    SuiteFilter::Outdated => {
                        suite.is_some_and(|suite| runs.site_status(&suite.output_id) == Some(true))
                    }
                };
                if !shown {
                    return None;
                }
                let matches = if open {
                    self.value["scenarios"]
                        .as_array()
                        .map(|scenarios| {
                            scenarios
                                .iter()
                                .filter(|scenario| {
                                    ["label", "id", "url"].iter().any(|key| {
                                        scenario[*key].as_str().is_some_and(|text| {
                                            text.to_lowercase().contains(&query)
                                        })
                                    })
                                })
                                .count()
                        })
                        .unwrap_or(0)
                } else {
                    suite
                        .map(|suite| {
                            suite
                                .scenarios
                                .iter()
                                .filter(|scenario| {
                                    [&scenario.label, &scenario.id, &scenario.url]
                                        .iter()
                                        .any(|text| text.to_lowercase().contains(&query))
                                })
                                .count()
                        })
                        .unwrap_or(0)
                };
                if !query.is_empty()
                    && !entry.label().to_lowercase().contains(&query)
                    && !suite.is_some_and(|suite| suite.label().to_lowercase().contains(&query))
                    && !(open
                        && self.value["displayName"]
                            .as_str()
                            .is_some_and(|name| name.to_lowercase().contains(&query)))
                    && matches == 0
                {
                    return None;
                }
                Some((index, entry, suite, open, matches))
            })
            .collect::<Vec<_>>();
        let visible = std::sync::Arc::new(
            entries
                .iter()
                .filter_map(|(_, _, suite, _, _)| {
                    suite
                        .filter(|suite| suite.error.is_none())
                        .map(|suite| suite.name.clone())
                })
                .collect::<Vec<_>>(),
        );
        let paths = std::sync::Arc::new(
            entries
                .iter()
                .map(|(_, entry, _, _, _)| entry.path.clone())
                .collect::<Vec<_>>(),
        );
        let all_names = visible.clone();
        let shown_names = visible.clone();
        let outdated = self
            .session
            .suites
            .iter()
            .filter(|suite| runs.site_status(&suite.output_id) == Some(true))
            .count();
        let site_target = runs.site_target().map(|target| target.name.clone());
        let deployed = runs.site_suites().map(|_| {
            self.session
                .suites
                .iter()
                .filter(|suite| runs.site_status(&suite.output_id).is_some())
                .count()
        });
        let site_loading = *runs.site_source() == super::runs::SiteSource::Loading;
        // A removal opened on a selected suite acts on every selected suite the site serves,
        // like other list context menus act on the selection; shared to avoid a copy per row.
        let selected_on_site = std::sync::Arc::new(
            self.session
                .suites
                .iter()
                .filter(|suite| {
                    runs.selected(&suite.name) && runs.site_status(&suite.output_id).is_some()
                })
                .map(|suite| suite.output_id.clone())
                .collect::<Vec<_>>(),
        );
        let move_names = visible.clone();
        let move_paths = paths.clone();
        let toggle_names = visible.clone();
        let toggle_paths = paths.clone();
        let open_names = visible.clone();
        let command_paths = paths.clone();
        let mut rows = div()
            .id("suite-list")
            .test_support()
            .key_context("RegressifyList RegressifySuiteList")
            .track_focus(&self.suite_list.focus)
            .track_scroll(&self.suite_list.scroll)
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .on_action(cx.listener(move |this, _: &SelectAllListItems, _, cx| {
                this.select_visible_suites(&all_names, cx);
            }))
            .on_action(cx.listener(|this, _: &ClearListSelection, _, cx| {
                if !this.busy {
                    this.runs
                        .update(cx, |panel, cx| panel.select_all(false, cx));
                }
            }))
            .on_action(
                cx.listener(move |this, action: &lists::MoveListSelection, window, cx| {
                    this.move_suite_list(action, &move_paths, &move_names, window, cx);
                }),
            )
            .on_action(cx.listener(move |this, _: &ToggleListItem, window, cx| {
                this.activate_suite_list(&toggle_paths, &toggle_names, true, window, cx);
            }))
            .on_action(cx.listener(move |this, _: &OpenListItem, window, cx| {
                this.activate_suite_list(&paths, &open_names, false, window, cx);
            }))
            .on_action(
                cx.listener(move |this, command: &files::FileCommand, window, cx| {
                    cx.stop_propagation();
                    let fallback = this.session.open.as_ref().map(|open| &open.entry.path);
                    if let Some(path) = this.suite_list.current_item(&command_paths, fallback)
                        && let Some(entry) = this
                            .session
                            .entries
                            .iter()
                            .find(|entry| entry.path == path)
                            .cloned()
                    {
                        this.begin_suite_operation(entry, command.operation, window, cx);
                    }
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.suite_list.focus, cx);
                if !this.busy {
                    this.runs
                        .update(cx, |panel, cx| panel.select_all(false, cx));
                }
            }))
            .overflow_y_scroll();
        let count = entries.len();
        for (index, entry, suite, open, matches) in entries {
            let current = if self.main_tab == 2 {
                self.results.read(cx).active_suite() == Some(&entry.path)
            } else {
                open
            };
            let scenario_count = if open {
                self.value["scenarios"].as_array().map_or(0, Vec::len)
            } else {
                suite.map_or(0, |suite| suite.scenarios.len())
            };
            let error = suite.and_then(|suite| suite.error.clone());
            let mut detail = if error.is_some() {
                "Configuration error".into()
            } else if !query.is_empty() && matches > 0 {
                format!(
                    "{matches} matching {}",
                    if matches == 1 {
                        "scenario"
                    } else {
                        "scenarios"
                    }
                )
            } else {
                format!(
                    "{scenario_count} {}",
                    if scenario_count == 1 {
                        "scenario"
                    } else {
                        "scenarios"
                    }
                )
            };
            let name = suite
                .map(|suite| suite.name.clone())
                .or_else(|| entry.suite_name())
                .unwrap_or_default();
            let selected = runs.selected(&name);
            let visible = visible.clone();
            let entry = entry.clone();
            let menu_entry = entry.clone();
            let rename_entry = entry.clone();
            let delete_entry = entry.clone();
            // Site status against the deployment target the suite list describes.
            let site = suite.and_then(|suite| {
                Some((
                    suite.output_id.clone(),
                    runs.site_status(&suite.output_id)?,
                    site_target.clone()?,
                    runs.site_target()?.id.clone(),
                ))
            });
            let removal = suite.and_then(|suite| {
                let target = site_target.clone()?;
                let target_id = runs.site_target()?.id.clone();
                let ids = if selected {
                    selected_on_site.clone()
                } else {
                    site.as_ref()?;
                    std::sync::Arc::new(vec![suite.output_id.clone()])
                };
                (!ids.is_empty()).then_some((ids, target, target_id))
            });
            let deployment = suite.and_then(|suite| {
                let target = site_target.clone()?;
                runs.site_suites()?;
                Some(match runs.site_status(&suite.output_id) {
                    Some(true) => (
                        "Newer than site",
                        format!("Newer local results than on {target}"),
                        cx.theme().primary,
                    ),
                    Some(false) => (
                        "Deployed",
                        format!("Deployed to {target}"),
                        cx.theme().muted_foreground,
                    ),
                    None => (
                        "Not deployed",
                        format!("Not deployed to {target}"),
                        cx.theme().muted_foreground.opacity(0.7),
                    ),
                })
            });
            let menu_view = cx.entity().downgrade();
            let menu_focus = self.suite_list.focus.clone();
            let menu_epoch = self.suite_menu_epoch;
            let menu_id = SharedString::from(format!(
                "suite-menu-{}-{:?}-{:?}",
                self.suite_menu_epoch,
                self.session
                    .workspace
                    .as_ref()
                    .map(|workspace| &workspace.root),
                entry.path
            ));
            let label = if open {
                self.value["displayName"]
                    .as_str()
                    .filter(|name| !name.trim().is_empty())
                    .map(str::to_owned)
            } else {
                suite.map(|suite| suite.label().to_owned())
            }
            .unwrap_or_else(|| name.clone());
            let filename = entry
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let counts = suite.map(|suite| {
                // Approval can recalculate a report after its activity snapshot was saved.
                if self.main_tab == 2
                    && !runs.running()
                    && let Some(counts) = self.results.read(cx).suite_counts(&suite.entry)
                {
                    counts
                } else {
                    runs.suite_counts(&suite.output_id)
                }
            });
            if error.is_none()
                && let Some(counts) = &counts
            {
                if counts.running > 0 {
                    detail = format!(
                        "{} running / {} finished",
                        counts.running,
                        counts.complete()
                    );
                } else if counts.failed > 0 {
                    detail = format!("{} failed / {} finished", counts.failed, counts.complete());
                } else if counts.passed > 0 {
                    detail = format!("{} passed", counts.passed);
                } else if counts.queued > 0 {
                    detail = format!("{} queued", counts.queued);
                } else if counts.cancelled > 0 {
                    detail = format!(
                        "{} cancelled / {} finished",
                        counts.cancelled,
                        counts.complete()
                    );
                } else if counts.captured > 0 {
                    detail = format!("{} captured", counts.captured);
                }
            }
            let status_color =
                if error.is_some() || counts.as_ref().is_some_and(|counts| counts.failed > 0) {
                    cx.theme().danger
                } else if counts.as_ref().is_some_and(|counts| counts.running > 0) {
                    cx.theme().primary
                } else if counts.as_ref().is_some_and(|counts| counts.passed > 0) {
                    cx.theme().success
                } else {
                    cx.theme().muted_foreground
                };
            let row = gpui_kit::component::list::ListItem::new(menu_id)
                .flex()
                .items_center()
                .w_full()
                .flex_shrink_0()
                .min_w_0()
                .selected(selected)
                .secondary_selected(
                    current || self.suite_list.current.as_ref() == Some(&entry.path),
                )
                .on_click(|_, _, cx| cx.stop_propagation())
                .capture_any_mouse_down(cx.listener(
                    move |this, event: &MouseDownEvent, window, cx| {
                        if event.button == MouseButton::Right {
                            if this.busy || this.runs.read(cx).running() {
                                cx.stop_propagation();
                                return;
                            }
                            this.suite_list
                                .focus_item(menu_entry.path.clone(), window, cx);
                            window.prevent_default();
                            cx.notify();
                        }
                    },
                ))
                .child(
                    Button::new(("file", index))
                        .ghost()
                        .w_full()
                        .flex_1()
                        .min_w_0()
                        .justify_start()
                        .accessibility_label(if label == name {
                            filename
                        } else {
                            label.clone()
                        })
                        .tooltip(error.map_or_else(
                            || entry.label(),
                            |error| format!("{}\n{error}", entry.label()),
                        ))
                        .child(
                            Icon::new(gpui_kit::assets::IconName::Files)
                                .small()
                                .text_color(status_color),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .flex_1()
                                .min_w_0()
                                .text_left()
                                .child(
                                    div()
                                        .truncate()
                                        .font_weight(if current {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::NORMAL
                                        })
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .min_w_0()
                                        .text_xs()
                                        .when(!detail.is_empty(), |line| {
                                            line.child(
                                                div()
                                                    .id(("suite-activity", index))
                                                    .role(Role::Status)
                                                    .aria_label(detail.clone())
                                                    .test_support()
                                                    .truncate()
                                                    .text_color(status_color)
                                                    .child(detail.clone()),
                                            )
                                        })
                                        .when_some(deployment, |line, (text, label, color)| {
                                            line.when(!detail.is_empty(), |line| {
                                                line.child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child("·"),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .id(("suite-site-status", index))
                                                    .role(Role::Status)
                                                    .aria_label(label)
                                                    .test_support()
                                                    .flex_shrink_0()
                                                    .text_color(color)
                                                    .child(text),
                                            )
                                        }),
                                ),
                        )
                        .disabled(self.busy)
                        .on_click(cx.listener(
                            move |this, event: &gpui_kit::ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.suite_list.focus_item(entry.path.clone(), window, cx);
                                this.runs.update(cx, |panel, cx| {
                                    panel.select_suite(&name, &visible, event.modifiers(), cx);
                                });
                                if this.main_tab == 2 {
                                    this.results.update(cx, |panel, cx| {
                                        panel.select_suite(&entry, window, cx);
                                    });
                                } else {
                                    this.open_location(entry.clone(), Selection::Root, window, cx);
                                }
                            },
                        )),
                );
            let row = if self.busy || runs.running() {
                row.into_any_element()
            } else {
                row.context_menu(move |menu, window, cx| {
                    if menu_view
                        .upgrade()
                        .is_none_or(|view| view.read(cx).suite_menu_epoch != menu_epoch)
                    {
                        return menu;
                    }
                    design::restore_managed_menu_focus(
                        menu_view.clone(),
                        menu.focus_handle(cx),
                        menu_focus.clone(),
                        |this| this.suite_menu_epoch = this.suite_menu_epoch.wrapping_add(1),
                        window,
                        cx,
                    );
                    let rename_view = menu_view.clone();
                    let delete_view = menu_view.clone();
                    let removal_view = menu_view.clone();
                    let rename_entry = rename_entry.clone();
                    let delete_entry = delete_entry.clone();
                    let removal = removal.clone();
                    menu.action_context(menu_focus.clone())
                        .item(
                            PopupMenuItem::new("Rename suite").on_click(move |_, window, cx| {
                                let _ = rename_view.update(cx, |this, cx| {
                                    if this.suite_menu_epoch != menu_epoch {
                                        return;
                                    }
                                    this.begin_suite_operation(
                                        rename_entry.clone(),
                                        files::Operation::Rename,
                                        window,
                                        cx,
                                    )
                                });
                            }),
                        )
                        .item(
                            PopupMenuItem::new("Delete suite").on_click(move |_, window, cx| {
                                let _ = delete_view.update(cx, |this, cx| {
                                    if this.suite_menu_epoch != menu_epoch {
                                        return;
                                    }
                                    this.begin_suite_operation(
                                        delete_entry.clone(),
                                        files::Operation::Delete,
                                        window,
                                        cx,
                                    )
                                });
                            }),
                        )
                        // Removing from the site is a deployment, kept apart from deleting the
                        // suite's local files.
                        .when_some(removal, |menu, (ids, target, target_id)| {
                            let label = match ids.len() {
                                1 => format!("Remove from site: {target}..."),
                                count => {
                                    format!("Remove {count} selected suites from site: {target}...")
                                }
                            };
                            menu.separator().item(PopupMenuItem::new(label).on_click(
                                move |_, window, cx| {
                                    let ids = ids.to_vec();
                                    let target_id = target_id.clone();
                                    let _ = removal_view.update(cx, |this, cx| {
                                        if this.suite_menu_epoch == menu_epoch {
                                            this.open_site_removal(ids, target_id, window, cx);
                                        }
                                    });
                                },
                            ))
                        })
                })
                .into_any_element()
            };
            rows = rows.child(
                div()
                    .id(("suite-row", index))
                    .test_support()
                    .w_full()
                    .flex_shrink_0()
                    .child(row),
            );
        }
        if count == 0 {
            rows = rows.child(empty_list("navigator-empty", "No matches", cx));
        }
        div()
            .id("suite-pane")
            .test_support()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .flex_shrink_0()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_sm()
                            .child("Test suites"),
                    )
                    .child(
                        div()
                            .id("suite-count")
                            .role(Role::Status)
                            .aria_label(format!(
                                "{count} {}",
                                if count == 1 { "suite" } else { "suites" }
                            ))
                            .test_support()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(count.to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("navigator-refresh")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::RefreshCw)
                            .tooltip("Refresh suites")
                            .accessibility_label("Refresh suites")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.file_command(
                                    &files::FileCommand {
                                        operation: files::Operation::Refresh,
                                    },
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("navigator-new-suite")
                            .ghost()
                            .icon(IconName::Plus)
                            .tooltip("New suite")
                            .accessibility_label("New suite")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.guard(files::Pending::Name(files::Operation::New), window, cx)
                            })),
                    ),
            )
            // Which site the suite statuses describe, with counts and a shortcut to the
            // outdated suites.
            .when_some(site_target.clone(), |pane, target| {
                let busy = self.busy || runs.running();
                let summary = match deployed {
                    Some(deployed) => format!("{deployed} deployed"),
                    None if site_loading => "reading the site...".into(),
                    None => "status unknown".into(),
                };
                pane.child(
                    div()
                        .id("suite-site-summary")
                        .role(Role::Group)
                        .aria_label(format!("Site {target}: {summary}"))
                        .test_support()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1()
                        .px_3()
                        .pb_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            Button::new("suite-site-target")
                                .ghost()
                                .xsmall()
                                .label(format!("Site: {target}"))
                                .icon(IconName::ChevronDown)
                                .tooltip("The deployment target these statuses describe; open Deploy to choose another")
                                .accessibility_label(format!("Site {target}, choose another target"))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_deployment(window, cx)
                                })),
                        )
                        .child("·")
                        .child(
                            div()
                                .id("suite-site-deployed")
                                .role(Role::Status)
                                .aria_label(summary.clone())
                                .test_support()
                                .child(summary),
                        )
                        .when(outdated > 0, |row| {
                            row.child("·").child(
                                Button::new("suite-site-outdated")
                                    .link()
                                    .xsmall()
                                    .label(format!("{outdated} outdated"))
                                    .tooltip("Show only suites with newer local results")
                                    .accessibility_label(format!("Show {outdated} outdated suites"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.suite_filter = SuiteFilter::Outdated;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
            })
            .child(
                div().px_3().pb_2().child(
                    Input::new(&self.search)
                        .id("navigator-search")
                        .prefix(IconName::Search)
                        .cleanable(true),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .pb_2()
                    .child(
                        TabBar::new("navigator-filter")
                            .segmented()
                            .selected_index(match self.suite_filter {
                                SuiteFilter::All => 0,
                                SuiteFilter::Selected => 1,
                                SuiteFilter::Outdated => 2,
                            })
                            .on_click(cx.listener(|this, index: &usize, _, cx| {
                                this.suite_filter = match *index {
                                    1 => SuiteFilter::Selected,
                                    2 => SuiteFilter::Outdated,
                                    _ => SuiteFilter::All,
                                };
                                cx.notify();
                            }))
                            .child(Tab::new().label("All"))
                            .child(Tab::new().label("Selected"))
                            .child(Tab::new().label(format!("Outdated {outdated}"))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .ml_auto()
                            .flex_shrink_0()
                            .child(
                                Button::new("sidebar-select-none")
                                    .ghost()
                                    .label("None")
                                    .accessibility_label("Deselect all suites")
                                    .disabled(
                                        self.busy || runs.running() || runs.selected_count() == 0,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.runs
                                            .update(cx, |panel, cx| panel.select_all(false, cx));
                                    })),
                            )
                            .child(
                                Button::new("sidebar-select-all")
                                    .ghost()
                                    .label("All")
                                    // Filters and searches narrow what "All" selects.
                                    .accessibility_label("Select all shown suites")
                                    .disabled(
                                        self.busy || runs.running() || valid == 0 || shown_names.is_empty(),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_visible_suites(&shown_names, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                // The overlay must not inherit the rows' scroll offset.
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(rows)
                    .vertical_scrollbar(&self.suite_list.scroll),
            )
            .into_any_element()
    }

    fn render_scenario_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = self.session.open.as_ref();
        let runs = self.runs.read(cx);
        let viewport = open.is_some_and(|open| open.entry.kind == DocumentKind::Viewports);
        let suite = open.is_some_and(|open| open.entry.kind == DocumentKind::Suite);
        let items = if suite {
            self.value["scenarios"].as_array()
        } else if viewport {
            self.value.as_array()
        } else {
            None
        };
        let items = items.map(Vec::as_slice).unwrap_or_default();
        let scenario_titles = self.scenario_titles();
        let query = self.scenario_search.read(cx).value().trim().to_lowercase();
        let owner = open
            .filter(|_| suite || viewport)
            .map(|open| {
                self.value["displayName"]
                    .as_str()
                    .filter(|name| !name.trim().is_empty())
                    .map(str::to_owned)
                    .or_else(|| open.entry.suite_name())
                    .unwrap_or_else(|| open.entry.label())
            })
            .unwrap_or_else(|| "No suite selected".into());
        let entries = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let label = if viewport {
                    item["label"]
                        .as_str()
                        .filter(|label| !label.trim().is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("Viewport {}", index + 1))
                } else {
                    scenario_titles
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| format!("Scenario {}", index + 1))
                };
                if !query.is_empty()
                    && ![
                        label.as_str(),
                        item["url"].as_str().unwrap_or_default(),
                        item["id"].as_str().unwrap_or_default(),
                    ]
                    .iter()
                    .any(|text| text.to_lowercase().contains(&query))
                {
                    return None;
                }
                Some((index, item, label))
            })
            .collect::<Vec<_>>();
        let visible = std::sync::Arc::new(
            entries
                .iter()
                .map(|(index, _, _)| *index)
                .collect::<Vec<_>>(),
        );
        let all_visible = visible.clone();
        let move_visible = visible.clone();
        let toggle_visible = visible.clone();
        let open_visible = visible.clone();
        let mut rows = div()
            .id("scenario-list")
            .test_support()
            .key_context("RegressifyList")
            .track_focus(&self.scenario_list.focus)
            .track_scroll(&self.scenario_list.scroll)
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .on_action(
                cx.listener(move |this, action: &lists::MoveListSelection, window, cx| {
                    this.move_scenario_list(action, &move_visible, viewport, window, cx);
                }),
            )
            .on_action(cx.listener(move |this, _: &ToggleListItem, window, cx| {
                this.activate_scenario_list(&toggle_visible, viewport, true, window, cx);
            }))
            .on_action(cx.listener(move |this, _: &OpenListItem, window, cx| {
                this.activate_scenario_list(&open_visible, viewport, false, window, cx);
            }))
            .when(suite, |rows| {
                rows.on_action(
                    cx.listener(move |this, _: &SelectAllListItems, window, cx| {
                        this.select_visible_scenarios(&all_visible, window, cx);
                    }),
                )
                .on_action(cx.listener(
                    |this, _: &ClearListSelection, window, cx| {
                        this.select_open_scenarios(false, window, cx);
                    },
                ))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.scenario_list.focus, cx);
                if suite {
                    this.select_open_scenarios(false, window, cx);
                }
            }))
            .overflow_y_scroll();
        let count = entries.len();
        for (index, item, label) in entries {
            let visible = visible.clone();
            let url = item["url"].as_str().unwrap_or_default();
            let selection = if viewport {
                Selection::Viewport(index)
            } else {
                Selection::Scenario(index)
            };
            let current = self.session.selection == selection
                || (!viewport && self.session.selection.scenario() == Some(index));
            let detail = if viewport {
                format!("{} x {}", item["width"], item["height"])
            } else {
                page_location(
                    url,
                    self.session
                        .workspace
                        .as_ref()
                        .map(|workspace| workspace.root.as_path()),
                )
            };
            let scenario_id = serde_json::from_value::<crate::types::TestScenario>(item.clone())
                .ok()
                .map(|scenario| workspace::ScenarioSummary::from_scenario(&scenario).id);
            let selected = if viewport {
                current
            } else {
                scenario_id
                    .as_ref()
                    .is_some_and(|id| runs.scenario_selected(&owner, id))
            };
            let log_scenario = scenario_id.clone();
            let log_suite = owner.clone();
            let has_log = scenario_id
                .as_ref()
                .is_some_and(|id| runs.has_scenario_log(&owner, id));
            rows = rows.child(
                gpui_kit::component::list::ListItem::new(("scenario-row", index))
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .w_full()
                    .min_w_0()
                    .selected(selected)
                    .secondary_selected(
                        current
                            || self
                                .scenario_list
                                .current
                                .as_ref()
                                .is_some_and(|(path, item)| {
                                    *item == index
                                        && open.is_some_and(|open| &open.entry.path == path)
                                }),
                    )
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .w_full()
                            .min_w_0()
                            .child(
                                Button::new(("scenario", index))
                                    .ghost()
                                    .flex_1()
                                    .min_w_0()
                                    .justify_start()
                                    .accessibility_label(label.clone())
                                    .tooltip(format!(
                                        "{label}\n{}\n{}",
                                        if viewport { detail.as_str() } else { url },
                                        item["id"].as_str().unwrap_or_default()
                                    ))
                                    .child(
                                        div()
                                            .w(px(22.))
                                            .flex_shrink_0()
                                            .text_center()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child((index + 1).to_string()),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .flex_1()
                                            .min_w_0()
                                            .text_left()
                                            .child(
                                                div()
                                                    .truncate()
                                                    .font_weight(if current {
                                                        FontWeight::SEMIBOLD
                                                    } else {
                                                        FontWeight::NORMAL
                                                    })
                                                    .child(label.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .truncate()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(detail),
                                            ),
                                    )
                                    .disabled(self.busy)
                                    .on_click(cx.listener(
                                        move |this, event: &gpui_kit::ClickEvent, window, cx| {
                                            cx.stop_propagation();
                                            if suite {
                                                this.select_open_scenario(
                                                    index,
                                                    &visible,
                                                    event.modifiers(),
                                                    window,
                                                    cx,
                                                );
                                            }
                                            this.select(selection, window, cx);
                                            if let Some(open) = &this.session.open {
                                                this.scenario_list.focus_item(
                                                    (open.entry.path.clone(), index),
                                                    window,
                                                    cx,
                                                );
                                            }
                                        },
                                    )),
                            )
                            .when(suite, |row| {
                                row.child(
                                    Button::new(("sidebar-scenario-log", index))
                                        .ghost()
                                        .icon(gpui_kit::assets::IconName::Terminal)
                                        .tooltip(format!("View run log: {label}"))
                                        .accessibility_label(format!("View run log: {label}"))
                                        .disabled(self.busy || !has_log)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            if let Some(id) = &log_scenario {
                                                this.main_tab = 1;
                                                this.runs.update(cx, |panel, cx| {
                                                    panel.show_scenario_log(
                                                        &log_suite, id, window, cx,
                                                    )
                                                });
                                                cx.notify();
                                            }
                                        })),
                                )
                                .child(
                                    Button::new(("sidebar-scenario-test", index))
                                        .ghost()
                                        .icon(gpui_kit::assets::IconName::Play)
                                        .tooltip(format!("Run Test: {label}"))
                                        .accessibility_label(format!("Run Test: {label}"))
                                        .disabled(
                                            self.busy
                                                || runs.running()
                                                || scenario_id.is_none()
                                                || !runs.has_reference(&owner),
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.run_scenario(
                                                super::super::runner::RunCommand::Test,
                                                index,
                                                window,
                                                cx,
                                            )
                                        })),
                                )
                            }),
                    ),
            );
        }
        if count == 0 {
            rows = rows.child(empty_list(
                "scenario-empty",
                if suite || viewport {
                    "No matching items"
                } else {
                    "Select a test suite"
                },
                cx,
            ));
        }
        div()
            .id("scenario-pane")
            .test_support()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .flex_shrink_0()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_sm()
                            .child(if viewport { "Viewports" } else { "Scenarios" }),
                    )
                    .child(
                        div()
                            .id("scenario-count")
                            .role(Role::Status)
                            .aria_label(format!("{count} of {}", items.len()))
                            .test_support()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{count}/{}", items.len())),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("navigator-add-scenario")
                            .ghost()
                            .icon(IconName::Plus)
                            .tooltip(if viewport {
                                "Add viewport"
                            } else {
                                "Add scenario"
                            })
                            .disabled(
                                self.busy || !(suite || viewport) || !self.field_errors.is_empty(),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if viewport {
                                    this.edit(Session::add_viewport, window, cx);
                                } else {
                                    this.edit(Session::add_scenario, window, cx);
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .id("scenario-suite")
                    .role(Role::Status)
                    .aria_label(owner.clone())
                    .test_support()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(owner),
            )
            .child(
                div().px_3().pb_2().child(
                    Input::new(&self.scenario_search)
                        .id("scenario-search")
                        .prefix(IconName::Search)
                        .cleanable(true)
                        .disabled(!(suite || viewport)),
                ),
            )
            .when(!viewport, |pane| {
                pane.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .px_3()
                        .pb_2()
                        .child(
                            Button::new("sidebar-scenarios-none")
                                .ghost()
                                .label("None")
                                .accessibility_label("Deselect all scenarios")
                                .disabled(!suite || self.busy || runs.running())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.select_open_scenarios(false, window, cx);
                                })),
                        )
                        .child(
                            Button::new("sidebar-scenarios-all")
                                .ghost()
                                .label("All")
                                .accessibility_label("Select all scenarios")
                                .disabled(!suite || self.busy || runs.running() || items.is_empty())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.select_open_scenarios(true, window, cx);
                                })),
                        ),
                )
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(rows)
                    .vertical_scrollbar(&self.scenario_list.scroll),
            )
            .into_any_element()
    }

    pub(super) fn sync_workspace_list(&self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self
            .session
            .workspace
            .as_ref()
            .map(|workspace| workspace.root.clone());
        let rows = self
            .session
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.kind != DocumentKind::Suite)
            .map(|(index, entry)| (index, entry.clone()))
            .collect::<Vec<_>>();
        let fallback = self
            .session
            .open
            .as_ref()
            .map(|open| open.entry.path.clone());
        self.workspace_list.update(cx, |list, cx| {
            let current = list.delegate();
            let changed = current.root != root
                || current.rows.len() != rows.len()
                || current.rows.iter().zip(&rows).any(
                    |((before_index, before), (after_index, after))| {
                        before_index != after_index
                            || before.path != after.path
                            || before.kind != after.kind
                            || before.editable != after.editable
                    },
                );
            let disabled_changed = current.disabled != self.busy;
            if !changed && !disabled_changed {
                return;
            }
            let selected = current
                .current
                .as_ref()
                .filter(|_| current.root == root)
                .filter(|path| rows.iter().any(|(_, entry)| &entry.path == *path))
                .cloned()
                .or_else(|| {
                    fallback.filter(|path| rows.iter().any(|(_, entry)| &entry.path == path))
                });
            let index = selected
                .as_ref()
                .and_then(|path| rows.iter().position(|(_, entry)| &entry.path == path))
                .map(IndexPath::new);
            let delegate = list.delegate_mut();
            delegate.root = root;
            delegate.rows = rows;
            delegate.disabled = self.busy;
            list.set_selected_index(index, window, cx);
            if disabled_changed {
                list.set_selectable(!self.busy, cx);
            }
            cx.notify();
        });
    }

    pub(super) fn focus_workspace_file(
        &self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_list.update(cx, |list, cx| {
            let index = list
                .delegate()
                .rows
                .iter()
                .position(|(_, entry)| entry.path == path)
                .map(IndexPath::new);
            list.set_selected_index(index, window, cx);
            list.scroll_to_selected_item(window, cx);
            list.focus(window, cx);
            cx.notify();
        });
    }

    fn open_workspace_list_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let selected = self.workspace_list.read(cx).delegate().current.clone();
        if let Some(path) = selected
            && let Some(entry) = self
                .session
                .entries
                .iter()
                .find(|entry| entry.path == path && entry.supports_editing())
                .cloned()
        {
            self.focus_workspace_file(path, window, cx);
            self.open_location(entry, Selection::Root, window, cx);
        }
    }

    fn render_workspace_files(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let entries = self
            .session
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.kind != DocumentKind::Suite)
            .collect::<Vec<_>>();
        let mut section = gpui_kit::component::collapsible::Collapsible::new()
            .open(self.workspace_files_open)
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("workspace-files-toggle")
                    .ghost()
                    .open(self.workspace_files_open)
                    .w_full()
                    .justify_start()
                    .icon(if self.workspace_files_open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .accessibility_label(format!("Workspace files ({})", entries.len()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_left()
                            .truncate()
                            .child(format!("Workspace files ({})", entries.len())),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workspace_files_open = !this.workspace_files_open;
                        cx.notify();
                    })),
            );
        if self.workspace_files_open {
            let row_height = self.workspace_list.update(cx, |list, cx| {
                use gpui_kit::component::list::ListDelegate as _;
                list.delegate_mut()
                    .render_item(IndexPath::new(0), window, cx)
                    .map_or(px(0.), |row| {
                        row.into_any_element()
                            .layout_as_root(
                                gpui_kit::size(
                                    gpui_kit::AvailableSpace::MinContent,
                                    gpui_kit::AvailableSpace::MinContent,
                                ),
                                window,
                                cx,
                            )
                            .height
                    })
            });
            let focus = self.workspace_list.read(cx).focus_handle(cx);
            let files = div()
                .id("workspace-file-rows")
                .key_context("RegressifyList")
                .track_focus(&focus)
                .relative()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.workspace_list
                            .update(cx, |list, cx| list.focus(window, cx));
                    }),
                )
                .on_action(cx.listener(
                    move |this, action: &lists::MoveListSelection, window, cx| {
                        if !this.busy {
                            this.workspace_list.update(cx, |list, cx| {
                                let index = action
                                    .index(
                                        list.selected_index().map(|index| index.row),
                                        list.delegate().rows.len(),
                                    )
                                    .map(IndexPath::new);
                                list.set_selected_index(index, window, cx);
                                list.scroll_to_selected_item(window, cx);
                                cx.notify();
                            });
                        }
                    },
                ))
                .on_action(cx.listener(move |this, _: &OpenListItem, window, cx| {
                    this.open_workspace_list_item(window, cx);
                }))
                .on_action(cx.listener(move |this, _: &ToggleListItem, window, cx| {
                    this.open_workspace_list_item(window, cx);
                }))
                .flex()
                .flex_col()
                .h(row_height * entries.len().min(8) as f32)
                .child(gpui_kit::component::list::List::new(&self.workspace_list));
            section = section.content(div().id("workspace-files").test_support().child(files));
        }
        section.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{display_path, page_location};

    #[test]
    fn scenario_locations_keep_useful_page_context_without_long_root_prefixes() {
        assert_eq!(
            page_location(
                "https://example.com:8443/checkout?tab=payment#summary",
                None
            ),
            "example.com:8443/checkout?tab=payment#summary"
        );
        assert_eq!(
            page_location("{{BASE_URL}}/checkout", None),
            "{{BASE_URL}}/checkout"
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pages/invoice.html");
        let url = reqwest::Url::from_file_path(&path).unwrap();
        assert_eq!(
            page_location(url.as_str(), Some(directory.path())),
            "pages/invoice.html"
        );
        assert_eq!(page_location(url.as_str(), None), display_path(&path));
    }
}
