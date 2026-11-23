use super::*;
use gpui_kit::component::{list::ListItem, tree::TreeEntry};

fn visible_name(value: &str) -> String {
    value
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

impl GitPanel {
    fn render_remotes(&self, cx: &mut Context<Self>) -> AnyElement {
        let disabled = self.disabled || self.active() || self.error.is_some();
        let snapshot = self.snapshot.as_ref();
        let selected = snapshot
            .and_then(Snapshot::default_remote)
            .map(str::to_string);
        let rename = selected.clone();
        let remove = selected.clone();
        let mut section = div()
            .id("git-remotes")
            .role(Role::Group)
            .aria_label("Git remotes")
            .test_support()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(design::ToolbarFocus::new(
                "git-remotes-toolbar-focus",
                gpui_kit::component::toolbar::Toolbar::new("git-remotes-toolbar")
                    .with_size(gpui_kit::component::Size::Medium)
                    .p_0()
                    .content_height()
                    .flex_wrap()
                    .content(div().text_sm().child("Remote"))
                    .content(
                        div()
                            .id("git-remote-menu-frame")
                            .test_support()
                            .min_w_0()
                            .max_w(px(260.))
                            .flex_1()
                            .child(
                                gpui_kit::component::select::Select::new(&self.remote_select)
                                    .id("git-remote-menu")
                                    .accessibility_label("Select Git remote")
                                    .placeholder(
                                        self.remote_selected
                                            .clone()
                                            .unwrap_or_else(|| "Select remote".into()),
                                    )
                                    .search_placeholder("Search remotes")
                                    .menu_max_h(px(260.))
                                    .w_full()
                                    .with_size(gpui_kit::component::Size::Medium)
                                    .when(
                                        self.remote_selected.as_ref().is_some_and(|selected| {
                                            !self.remote_items.contains(selected)
                                        }),
                                        |control| control.border_color(cx.theme().danger),
                                    )
                                    .disabled(
                                        disabled
                                            || self.remote_edit.is_some()
                                            || self.remote_items.is_empty(),
                                    ),
                            ),
                    )
                    .child(
                        Button::new("git-remote-add")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Plus)
                            .tooltip("Add remote")
                            .accessibility_label("Add remote")
                            .disabled(disabled || self.remote_edit.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.edit_remote(RemoteEdit::Add, window, cx)
                            })),
                    )
                    .child(
                        Button::new("git-remote-rename")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Pencil)
                            .tooltip("Rename remote")
                            .accessibility_label("Rename remote")
                            .disabled(disabled || selected.is_none() || self.remote_edit.is_some())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(name) = &rename {
                                    this.edit_remote(RemoteEdit::Rename(name.clone()), window, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("git-remote-remove")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Trash)
                            .tooltip("Remove remote")
                            .accessibility_label("Remove remote")
                            .disabled(disabled || selected.is_none() || self.remote_edit.is_some())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(name) = &remove {
                                    this.request(
                                        model::Action::RemoveRemote { name: name.clone() },
                                        cx,
                                    );
                                }
                            })),
                    ),
            ));
        if self
            .remote_selected
            .as_ref()
            .is_some_and(|selected| !self.remote_items.contains(selected))
        {
            section = section.child(
                div()
                    .id("git-remote-unavailable")
                    .role(Role::Status)
                    .aria_label("Selected remote is unavailable")
                    .test_support()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child("Selected remote is unavailable"),
            );
        }
        if let Some((mode, _)) = &self.remote_edit {
            let needs_name = !matches!(mode, RemoteEdit::Url { .. });
            let needs_url = !matches!(mode, RemoteEdit::Rename(_));
            let label = match mode {
                RemoteEdit::Add => "Add remote",
                RemoteEdit::Rename(_) => "Rename remote",
                RemoteEdit::Url { push: true, .. } => "Update push URL",
                RemoteEdit::Url { .. } => "Update fetch URL",
            };
            section = section.child(
                div()
                    .id("git-remote-form")
                    .test_support()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_1()
                    .when(needs_name, |form| {
                        form.child(
                            Input::new(&self.remote_name)
                                .id("git-remote-name")
                                .aria_label("Remote name")
                                .disabled(disabled),
                        )
                    })
                    .when(needs_url, |form| {
                        form.child(
                            Input::new(&self.remote_url)
                                .id("git-remote-url")
                                .aria_label("Remote URL")
                                .disabled(disabled),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("git-remote-edit-cancel")
                                    .label("Cancel edit")
                                    .disabled(disabled)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.remote_edit = None;
                                        this.remote_url.update(cx, |state, cx| {
                                            state.set_value("", window, cx)
                                        });
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("git-remote-save")
                                    .primary()
                                    .icon(gpui_kit::assets::IconName::Save)
                                    .label(label)
                                    .accessibility_label("Save remote")
                                    .disabled(
                                        disabled
                                            || (needs_name
                                                && self
                                                    .remote_name
                                                    .read(cx)
                                                    .value()
                                                    .trim()
                                                    .is_empty())
                                            || (needs_url
                                                && self
                                                    .remote_url
                                                    .read(cx)
                                                    .value()
                                                    .trim()
                                                    .is_empty()),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.save_remote(cx))),
                            ),
                    ),
            );
        } else if let Some(name) = &selected {
            let urls = snapshot.and_then(|snapshot| snapshot.remote_urls.get(name));
            let mut url_rows = div().flex().w_full().gap_3();
            for push in [false, true] {
                let values = urls.map(|urls| {
                    if push && !urls.push.is_empty() {
                        &urls.push
                    } else {
                        &urls.fetch
                    }
                });
                let value = values
                    .and_then(|values| values.first())
                    .map(|url| {
                        url.as_deref()
                            .unwrap_or("URL hidden (credentials or unsupported format)")
                    })
                    .unwrap_or("No URL configured");
                let text = format!(
                    "{}: {}{}{}",
                    if push { "Push" } else { "Fetch" },
                    value,
                    if push && urls.is_some_and(|urls| urls.push.is_empty()) {
                        " (same as fetch)"
                    } else {
                        ""
                    },
                    values
                        .filter(|values| values.len() > 1)
                        .map(|values| format!(" (+{} more)", values.len() - 1))
                        .unwrap_or_default()
                );
                let name = name.clone();
                let label = if push {
                    "Edit push URL"
                } else {
                    "Edit fetch URL"
                };
                url_rows = url_rows.child(
                    div()
                        .flex()
                        .flex_1()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .id(if push {
                                    "git-push-url"
                                } else {
                                    "git-fetch-url"
                                })
                                .role(Role::Status)
                                .aria_label(text.clone())
                                .test_support()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .truncate()
                                .child(text),
                        )
                        .child(
                            Button::new(if push {
                                "git-edit-push-url"
                            } else {
                                "git-edit-fetch-url"
                            })
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Pencil)
                            .tooltip(label)
                            .accessibility_label(label)
                            .disabled(disabled)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.edit_remote(
                                        RemoteEdit::Url {
                                            name: name.clone(),
                                            push,
                                        },
                                        window,
                                        cx,
                                    )
                                },
                            )),
                        ),
                );
            }
            section = section.child(url_rows);
        }
        section.into_any_element()
    }

    fn render_activity(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(activity) = &self.activity else {
            return div().into_any_element();
        };
        let elapsed = activity
            .finished
            .unwrap_or_else(|| activity.started.elapsed());
        let state = if activity.finished.is_none() {
            if activity.reporter.is_cancelled() {
                "Cancelling"
            } else {
                "Running"
            }
        } else if activity.success == Some(true) {
            "Completed"
        } else if activity.reporter.is_cancelled() {
            "Cancelled"
        } else {
            "Failed"
        };
        let progress = activity.reporter.progress();
        let summary = format!(
            "{} | {state} | {:.1}s",
            activity.label,
            elapsed.as_secs_f32()
        );
        div()
            .id("git-activity")
            .role(Role::Group)
            .aria_label("Git activity")
            .test_support()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .min_w_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(design::ToolbarFocus::new(
                "git-activity-toolbar-focus",
                gpui_kit::component::toolbar::Toolbar::new("git-activity-toolbar")
                    .with_size(gpui_kit::component::Size::Medium)
                    .content_height()
                    .flex_wrap()
                    .child(
                        Button::new("git-activity-toggle")
                            .ghost()
                            .icon(if self.activity_open {
                                gpui_kit::assets::IconName::ChevronDown
                            } else {
                                gpui_kit::assets::IconName::ChevronRight
                            })
                            .label("Activity")
                            .accessibility_label("Toggle Git activity logs")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.activity_open = !this.activity_open;
                                cx.notify();
                            })),
                    )
                    .content(
                        div()
                            .id("git-progress")
                            .role(Role::Status)
                            .aria_label(summary.clone())
                            .test_support()
                            .text_xs()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(summary),
                    )
                    .child(
                        Checkbox::new("git-log-follow")
                            .label("Follow latest")
                            .checked(self.activity_follow)
                            .on_click(cx.listener(|this, checked, window, cx| {
                                this.activity_follow = *checked;
                                if *checked {
                                    this.activity_text.clear();
                                    this.sync_activity_log(window, cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(design::CopyText::new(
                        "git-log-copy",
                        self.activity_text.clone(),
                        "Copy Git logs",
                    ))
                    .child(
                        Button::new("git-log-clear")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Trash)
                            .tooltip("Clear Git logs")
                            .accessibility_label("Clear Git logs")
                            .disabled(activity.finished.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.activity = None;
                                this.activity_history.clear();
                                this.sync_activity_log(window, cx);
                                cx.notify();
                            })),
                    ),
            ))
            .when(self.activity_open, |section| {
                section
                    .child(
                        div()
                            .id("git-progress-stage")
                            .role(Role::Status)
                            .aria_label(progress.stage.clone())
                            .test_support()
                            .px_3()
                            .text_xs()
                            .min_w_0()
                            .truncate()
                            .child(progress.stage),
                    )
                    .child(
                        div()
                            .id("git-logs")
                            .test_support()
                            .h(px(130.))
                            .min_w_0()
                            .child(
                                CodeEditor::new(&self.activity_log)
                                    .aria_label("Git logs")
                                    .readonly(true)
                                    .size_full(),
                            ),
                    )
            })
            .into_any_element()
    }

    fn select_tree(&mut self, comparison: bool, selected: bool, cx: &mut Context<Self>) {
        if self.disabled || self.active() || self.error.is_some() {
            return;
        }
        if let Some(snapshot) = &self.snapshot {
            for change in &snapshot.changes {
                if change.is_comparison() == comparison && !change.conflict && !change.submodule {
                    if selected {
                        self.selection.insert(change.path.clone());
                    } else {
                        self.selection.remove(&change.path);
                    }
                }
            }
        }
        self.trees[usize::from(comparison)].update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn set_tree_checked(
        &mut self,
        comparison: bool,
        path: &str,
        folder: bool,
        checked: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        if self.disabled || self.active() || self.error.is_some() {
            return;
        }
        let prefix = format!("{path}/");
        let allowed = self
            .snapshot
            .as_ref()
            .into_iter()
            .flat_map(|snapshot| &snapshot.changes)
            .filter(|change| {
                change.is_comparison() == comparison
                    && !change.conflict
                    && !change.submodule
                    && if folder {
                        change.path.starts_with(&prefix)
                    } else {
                        change.path == path
                    }
            })
            .map(|change| change.path.clone())
            .collect::<Vec<_>>();
        let checked =
            checked.unwrap_or_else(|| !allowed.iter().all(|path| self.selection.contains(path)));
        for path in allowed {
            if checked {
                self.selection.insert(path);
            } else {
                self.selection.remove(&path);
            }
        }
        self.trees[usize::from(comparison)].update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn activate_tree(
        &mut self,
        comparison: bool,
        toggle: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.error.is_some() || (toggle && (self.disabled || self.active())) {
            return;
        }
        let tree = &self.trees[usize::from(comparison)];
        let target = tree.update(cx, |tree, cx| {
            let index = tree.selected_index().unwrap_or(0);
            let entry = tree.entry(index)?;
            let target = (
                entry.item().id.split_once(':')?.1.to_string(),
                entry.is_folder(),
            );
            tree.set_selected_index(Some(index), cx);
            Some(target)
        });
        let Some((path, folder)) = target else {
            return;
        };
        if toggle {
            self.set_tree_checked(comparison, &path, folder, None, cx);
        } else if !folder {
            self.review(path, window, cx);
        }
        cx.notify();
    }

    fn tree_weight(&self, comparison: bool, cx: &Context<Self>) -> f32 {
        let tree = self.trees[usize::from(comparison)].read(cx);
        (0..8)
            .take_while(|index| tree.entry(*index).is_some())
            .count()
            .max(1) as f32
    }

    fn tree_container(&self, comparison: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let group = usize::from(comparison);
        div()
            .id(if comparison {
                "git-comparison-files"
            } else {
                "git-files"
            })
            .key_context("RegressifyList")
            .flex_1()
            .flex_grow(self.tree_weight(comparison, cx))
            .min_h_0()
            .capture_action(cx.listener(
                move |this, _: &gpui_kit::base::actions::Confirm, window, cx| {
                    let folder = this.trees[group].update(cx, |tree, cx| {
                        if tree.selected_index().is_none() && tree.entry(0).is_some() {
                            tree.set_selected_index(Some(0), cx);
                        }
                        tree.selected_entry().is_some_and(TreeEntry::is_folder)
                    });
                    if folder {
                        cx.propagate();
                    } else {
                        this.activate_tree(comparison, false, window, cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.trees[group].update(cx, |tree, cx| tree.focus(window, cx));
                }),
            )
            .on_action(cx.listener(move |this, _: &SelectAllListItems, _, cx| {
                this.select_tree(comparison, true, cx);
            }))
            .on_action(cx.listener(move |this, _: &ClearListSelection, _, cx| {
                this.select_tree(comparison, false, cx);
            }))
            .on_action(
                cx.listener(move |this, action: &lists::MoveListSelection, _, cx| {
                    if this.error.is_some() {
                        return;
                    }
                    this.trees[group].update(cx, |tree, cx| {
                        let count = (0..)
                            .take_while(|index| tree.entry(*index).is_some())
                            .count();
                        let index = action.index(tree.selected_index(), count);
                        tree.set_selected_index(index, cx);
                        if let Some(index) = index {
                            tree.scroll_to_item(index, ScrollStrategy::Nearest);
                        }
                    });
                }),
            )
            .on_action(cx.listener(move |this, _: &ToggleListItem, window, cx| {
                this.activate_tree(comparison, true, window, cx);
            }))
            .on_action(cx.listener(move |this, _: &OpenListItem, window, cx| {
                this.activate_tree(comparison, false, window, cx);
            }))
    }

    pub(super) fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        self.tree_generation = self.tree_generation.wrapping_add(1);
        fn items(
            branch: &model::ChangeTree,
            prefix: &str,
            snapshot: &Snapshot,
            collapsed: &BTreeSet<String>,
            comparison: bool,
        ) -> Vec<TreeItem> {
            let mut result = Vec::new();
            for (label, child) in &branch.folders {
                let path = format!("{prefix}{label}");
                let children = items(child, &format!("{path}/"), snapshot, collapsed, comparison);
                if !children.is_empty() {
                    let id = format!("{comparison}:{path}");
                    result.push(
                        TreeItem::new(id.clone(), visible_name(label))
                            .expanded(!collapsed.contains(&id))
                            .children(children),
                    );
                }
            }
            for index in &branch.files {
                let change = &snapshot.changes[*index];
                if change.is_comparison() == comparison {
                    result.push(TreeItem::new(
                        format!("{comparison}:{}", change.path),
                        visible_name(change.path.rsplit('/').next().unwrap_or(&change.path)),
                    ));
                }
            }
            result
        }
        let hierarchy = self
            .snapshot
            .as_ref()
            .map(|snapshot| model::ChangeTree::new(&snapshot.changes));
        for comparison in [false, true] {
            let group = usize::from(comparison);
            let next = self
                .snapshot
                .as_ref()
                .zip(hierarchy.as_ref())
                .map_or_else(Vec::new, |(snapshot, hierarchy)| {
                    items(hierarchy, "", snapshot, &self.collapsed, comparison)
                });
            self.trees[group].update(cx, |tree, cx| {
                let selected = tree.selected_item().map(|item| item.id.clone());
                tree.set_items(next, cx);
                tree.set_selected_index(selected.as_ref().and_then(|id| tree.index_of(id)), cx);
                if let Some(index) = tree.selected_index() {
                    tree.scroll_to_item(index, ScrollStrategy::Nearest);
                }
            });
        }
    }

    fn render_tree(&self, comparison: bool, cx: &Context<Self>) -> AnyElement {
        let owner = cx.entity().downgrade();
        let tree = &self.trees[usize::from(comparison)];
        let scroll = tree.read(cx).scroll_handle().clone();
        // The styled Kit 0.10 Tree retains itself in its menu renderer, even without a menu.
        div()
            .size_full()
            .child(
                gpui_kit::base::Tree::new(tree)
                    .item(move |index, entry, state, _, cx| {
                        owner
                            .upgrade()
                            .map_or_else(
                                || ListItem::new(index).disabled(true),
                                |owner| {
                                    owner.update(cx, |panel, cx| {
                                        panel.tree_row(index, entry, comparison, cx)
                                    })
                                },
                            )
                            .disabled(entry.is_disabled())
                            .selected(state.is_selected())
                            .secondary_selected(state.is_right_clicked())
                            .into_any_element()
                    })
                    .list_style(
                        gpui_kit::StyleRefinement::default()
                            .flex_grow_1()
                            .size_full(),
                    )
                    .relative()
                    .size_full(),
            )
            .vertical_scrollbar(&scroll)
            .into_any_element()
    }

    fn tree_checkbox(&self, comparison: bool, id: SharedString, cx: &mut Context<Self>) -> Div {
        let generation = self.tree_generation;
        div().flex_shrink_0().on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                if this.tree_generation == generation {
                    this.trees[usize::from(comparison)].update(cx, |tree, cx| {
                        tree.set_selected_index(tree.index_of(&id), cx);
                        tree.focus(window, cx);
                    });
                }
                // Expanding the row during mouse-down can move the checkbox before mouse-up.
                cx.stop_propagation();
            }),
        )
    }

    fn tree_row(
        &self,
        index: usize,
        entry: &TreeEntry,
        comparison: bool,
        cx: &mut Context<Self>,
    ) -> ListItem {
        let id = entry.item().id.clone();
        let path = id
            .split_once(':')
            .expect("Tree IDs include the group")
            .1
            .to_string();
        let label = entry.item().label.clone();
        let generation = self.tree_generation;
        let mut element = div()
            .id((
                if comparison {
                    "git-comparison-tree-row"
                } else {
                    "git-tree-row"
                },
                index,
            ))
            .test_support()
            .flex()
            .items_center()
            .w_full()
            .min_w_0()
            .pl(px(entry.depth().min(12) as f32 * 14.))
            .gap_1();
        if !entry.is_folder() {
            let Some(file) = self.snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .changes
                    .iter()
                    .position(|change| change.path == path && change.is_comparison() == comparison)
            }) else {
                return ListItem::new(id).disabled(true);
            };
            let change = &self
                .snapshot
                .as_ref()
                .expect("tree requires snapshot")
                .changes[file];
            let selected = self.selection.contains(&path);
            let checkbox_path = path.clone();
            let conflict = change.conflict;
            let status = change.label();
            let staged = change.index != '.' && !change.untracked;
            let tooltip = format!(
                "{}\n{}{}{}",
                visible_name(&path),
                status,
                if staged { " | staged" } else { "" },
                change
                    .original_path
                    .as_ref()
                    .map(|original| format!("\nFrom {}", visible_name(original)))
                    .unwrap_or_default()
            );
            element = element
                .when(self.active_file.as_ref() == Some(&path), |row| {
                    row.bg(cx.theme().accent)
                })
                .child(
                    self.tree_checkbox(comparison, id.clone(), cx).child(
                        Checkbox::new(("git-select", file))
                            .checked(selected)
                            .disabled(
                                self.disabled || self.active() || conflict || change.submodule,
                            )
                            .accessibility_label(format!("Select {}", visible_name(&path)))
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                if this.tree_generation == generation {
                                    this.set_tree_checked(
                                        comparison,
                                        &checkbox_path,
                                        false,
                                        Some(*checked),
                                        cx,
                                    );
                                }
                            })),
                    ),
                )
                .child(
                    Button::new(("git-file", file))
                        .ghost()
                        .flex_1()
                        .min_w_0()
                        .justify_start()
                        .accessibility_label(format!("Review {}", visible_name(&path)))
                        .tooltip(tooltip)
                        .child(
                            Icon::new(if conflict {
                                gpui_kit::assets::IconName::TriangleAlert
                            } else {
                                gpui_kit::assets::IconName::FileDiff
                            })
                            .size_3(),
                        )
                        .child(div().flex_1().min_w_0().text_left().truncate().child(label))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.tree_generation == generation {
                                this.review(path.clone(), window, cx);
                            }
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .flex_shrink_0()
                        .text_color(if conflict {
                            cx.theme().danger
                        } else if change.untracked {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(if conflict {
                            "!".into()
                        } else if change.untracked {
                            "U".into()
                        } else {
                            format!("{}{}", change.index, change.worktree)
                        }),
                );
        } else {
            let collapsed = !entry.is_expanded();
            let prefix = format!("{path}/");
            let allowed = self
                .snapshot
                .as_ref()
                .expect("tree requires snapshot")
                .changes
                .iter()
                .filter(|change| {
                    change.is_comparison() == comparison
                        && change.path.starts_with(&prefix)
                        && !change.conflict
                        && !change.submodule
                })
                .map(|change| change.path.clone())
                .collect::<Vec<_>>();
            let selected = allowed
                .iter()
                .filter(|path| self.selection.contains(*path))
                .count();
            let owner = cx.entity().downgrade();
            let checkbox_path = path.clone();
            element = element.child(
                self.tree_checkbox(comparison, id.clone(), cx).child(
                    design::selection_checkbox(
                        SharedString::from(format!(
                            "{}:{path}",
                            if comparison {
                                "git-select-comparison-folder"
                            } else {
                                "git-select-folder"
                            }
                        )),
                        !allowed.is_empty() && selected == allowed.len(),
                        selected > 0 && selected < allowed.len(),
                        self.disabled
                            || self.active()
                            || self.error.is_some()
                            || allowed.is_empty(),
                        cx,
                    )
                    .accessibility_label(format!("Select folder {}", visible_name(&path)))
                    .on_change(move |state, _, _, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            if this.tree_generation == generation {
                                this.set_tree_checked(
                                    comparison,
                                    &checkbox_path,
                                    true,
                                    Some(state == gpui_kit::base::CheckboxState::Checked),
                                    cx,
                                );
                            }
                        });
                    }),
                ),
            );
            element = element.child(
                Button::new(SharedString::from(format!(
                    "{}:{path}",
                    if comparison {
                        "git-comparison-folder"
                    } else {
                        "git-folder"
                    }
                )))
                .ghost()
                .flex_1()
                .min_w_0()
                .justify_start()
                .icon(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .accessibility_label(format!("Folder {}", visible_name(&path)))
                .tooltip(visible_name(&path))
                .child(Icon::new(gpui_kit::assets::IconName::Folder).size_3())
                .child(div().flex_1().min_w_0().text_left().truncate().child(label)),
            );
        }
        ListItem::new(id).w_full().child(element)
    }

    fn render_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let generation = self.tree_generation;
        let count = self.snapshot.as_ref().map_or(0, |snapshot| {
            snapshot
                .changes
                .iter()
                .filter(|change| !change.is_comparison())
                .count()
        });
        let allowed = self
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .changes
                    .iter()
                    .filter(|change| {
                        !change.is_comparison() && !change.conflict && !change.submodule
                    })
                    .map(|change| change.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let all = !allowed.is_empty() && allowed.iter().all(|path| self.selection.contains(path));
        let disabled = self.disabled || self.active() || self.error.is_some();
        let message_empty = self.message.read(cx).value().trim().is_empty();
        let blocked = self.snapshot.as_ref().is_none_or(|snapshot| {
            snapshot.conflicts() > 0 || snapshot.operation.is_some() || snapshot.detached
        });
        let discard_blocked = self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.conflicts() > 0 || snapshot.operation.is_some());
        div()
            .id("git-changes-panel")
            .role(Role::Group)
            .aria_label("Git changes")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .h(px(38.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .id("git-change-count")
                            .role(Role::Status)
                            .aria_label(format!("{count} changed files"))
                            .test_support()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Changes ({count})")),
                    )
                    .child(
                        Checkbox::new("git-select-all")
                            .label("All")
                            .checked(all)
                            .disabled(disabled || allowed.is_empty())
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                if this.tree_generation == generation {
                                    this.select_tree(false, *checked, cx);
                                }
                            })),
                    ),
            )
            .child(
                self.tree_container(false, cx)
                    .when(count == 0, |tree| {
                        tree.child(
                            div()
                                .p_3()
                                .text_color(cx.theme().muted_foreground)
                                .child("No file changes"),
                        )
                    })
                    .when(count > 0, |tree| tree.child(self.render_tree(false, cx))),
            )
            .child(self.render_comparison_changes(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} file(s) selected", self.selection.len())),
                    )
                    .child(
                        div()
                            .id("git-commit-message")
                            .test_support()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    if !this.disabled && !this.active() {
                                        window.focus(&this.message.read(cx).focus_handle(cx), cx);
                                        window.prevent_default();
                                        cx.stop_propagation();
                                    }
                                }),
                            )
                            .child(
                                Textarea::new(&self.message)
                                    .aria_label("Git commit message")
                                    .disabled(disabled),
                            ),
                    )
                    .child(design::ToolbarFocus::new(
                        "git-selection-toolbar-focus",
                        gpui_kit::component::toolbar::Toolbar::new("git-selection-toolbar")
                            .with_size(gpui_kit::component::Size::Medium)
                            .p_0()
                            .content_height()
                            .flex_wrap()
                            .child(
                                Button::new("git-commit")
                                    .primary()
                                    .icon(gpui_kit::assets::IconName::GitCommitHorizontal)
                                    .label("Commit selected files")
                                    .disabled(
                                        disabled
                                            || blocked
                                            || message_empty
                                            || self.selection.is_empty(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.request(
                                            model::Action::Commit {
                                                paths: this.selection.iter().cloned().collect(),
                                                message: this.message.read(cx).value().to_string(),
                                            },
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("git-discard")
                                    .danger()
                                    .icon(gpui_kit::assets::IconName::Trash)
                                    .label("Discard selected files")
                                    .disabled(
                                        disabled || discard_blocked || self.selection.is_empty(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.request(
                                            model::Action::Discard {
                                                paths: this.selection.iter().cloned().collect(),
                                            },
                                            cx,
                                        );
                                    })),
                            ),
                    )),
            )
            .into_any_element()
    }

    fn render_comparison_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let generation = self.tree_generation;
        let changes = self
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .changes
                    .iter()
                    .filter(|change| change.is_comparison())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let selected = changes
            .iter()
            .filter(|change| self.selection.contains(&change.path))
            .count();
        let allowed = changes
            .iter()
            .filter(|change| !change.conflict && !change.submodule)
            .map(|change| change.path.clone())
            .collect::<Vec<_>>();
        let all = !allowed.is_empty() && allowed.iter().all(|path| self.selection.contains(path));
        div()
            .id("git-comparison-changes")
            .test_support()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            // Share space by short-tree depth; cap weights so large trees keep bounded viewports.
            .when(self.comparison_open, |panel| {
                panel
                    .flex_1()
                    .flex_grow(self.tree_weight(true, cx))
                    .flex_basis(px(38.))
            })
            .when(!self.comparison_open, |panel| panel.flex_shrink_0())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(38.))
                    .flex_shrink_0()
                    .border_t_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("git-toggle-comparison")
                            .ghost()
                            .flex_1()
                            .min_w_0()
                            .justify_start()
                            .icon(if self.comparison_open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_left()
                                    .truncate()
                                    .child(format!("Comparison Changes ({})", changes.len())),
                            )
                            .tooltip(format!(
                                "Comparison Changes: {} files, {selected} selected",
                                changes.len()
                            ))
                            .accessibility_label("Comparison Changes")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.comparison_open = !this.comparison_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("git-comparison-selected")
                            .role(Role::Status)
                            .aria_label(format!("{selected} comparison files selected"))
                            .test_support()
                            .text_xs()
                            .flex_shrink_0()
                            .child(if selected > 0 {
                                selected.to_string()
                            } else {
                                String::new()
                            }),
                    )
                    .when(self.comparison_open, |header| {
                        header.child(
                            Checkbox::new("git-select-all-comparison")
                                .label("All")
                                .checked(all)
                                .disabled(
                                    self.disabled
                                        || self.active()
                                        || self.error.is_some()
                                        || allowed.is_empty(),
                                )
                                .on_click(cx.listener(move |this, checked, _, cx| {
                                    if this.tree_generation == generation {
                                        this.select_tree(true, *checked, cx);
                                    }
                                })),
                        )
                    }),
            )
            .when(self.comparison_open, |panel| {
                panel.child(
                    self.tree_container(true, cx)
                        .when(changes.is_empty(), |tree| {
                            tree.child(div().p_3().child("No comparison changes"))
                        })
                        .when(!changes.is_empty(), |tree| {
                            tree.child(self.render_tree(true, cx))
                        }),
                )
            })
            .into_any_element()
    }

    fn render_review(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(38.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().flex_1().min_w_0().truncate().child(
                            self.active_file
                                .as_deref()
                                .map(visible_name)
                                .unwrap_or_else(|| "Review changes".into()),
                        ),
                    )
                    .child(
                        Button::new("git-open-file")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::ExternalLink)
                            .tooltip("Open file in VS Code")
                            .accessibility_label("Open file in VS Code")
                            .disabled(self.active_file.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.open_vscode(true, cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(self.active_file.is_none(), |content| {
                        content.child(
                            div()
                                .p_4()
                                .text_color(cx.theme().muted_foreground)
                                .child("No file selected"),
                        )
                    })
                    .when(self.active_file.is_some(), |content| {
                        if self.text_review.as_ref().is_some_and(Result::is_ok) {
                            return content.child(self.render_text_review(cx));
                        }
                        content
                            .flex()
                            .flex_col()
                            .when_some(self.text_review.as_ref(), |content, review| {
                                content.child(
                                    div()
                                        .id("git-review-unavailable")
                                        .role(Role::Status)
                                        .test_support()
                                        .px_3()
                                        .py_2()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            review
                                                .as_ref()
                                                .err()
                                                .cloned()
                                                .map(|error| format!("Source Diff: {error}"))
                                                .unwrap_or_else(|| "Loading changes...".into()),
                                        ),
                                )
                            })
                            .child(
                                div().flex_1().min_h_0().child(
                                    CodeEditor::new(&self.diff)
                                        .aria_label("Git diff")
                                        .readonly(true)
                                        .size_full(),
                                ),
                            )
                    }),
            )
            .into_any_element()
    }

    fn render_text_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(Ok(review)) = &self.text_review else {
            return div().into_any_element();
        };
        // Identical row geometry lets both virtual lists share scrolling without drift.
        div()
            .id("git-text-diff")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .child(
                div()
                    .id("git-diff-toolbar")
                    .role(Role::Group)
                    .aria_label("Git diff commands")
                    .test_support()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(design::ToolbarFocus::new(
                        "git-diff-toolbar-focus",
                        gpui_kit::component::toolbar::Toolbar::new("git-diff-command-toolbar")
                            .with_size(gpui_kit::component::Size::Medium)
                            .content_height()
                            .flex_wrap()
                            .content(
                                div()
                                    .id("git-diff-summary")
                                    .role(Role::Status)
                                    .test_support()
                                    .text_sm()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(format!("{} changes", review.changes.len())),
                            )
                            .when_some(review.renamed_from.as_ref(), |header, path| {
                                header.content(
                                    div()
                                        .text_sm()
                                        .min_w_0()
                                        .truncate()
                                        .child(format!("Renamed from {}", visible_name(path))),
                                )
                            })
                            .child(
                                Button::new("git-diff-previous")
                                    .ghost()
                                    .icon(IconName::ArrowUp)
                                    .tooltip("Previous change")
                                    .accessibility_label("Previous change")
                                    .disabled(review.changes.is_empty())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.move_diff_change(false, cx)
                                    })),
                            )
                            .child(
                                Button::new("git-diff-next")
                                    .ghost()
                                    .icon(IconName::ArrowDown)
                                    .tooltip("Next change")
                                    .accessibility_label("Next change")
                                    .disabled(review.changes.is_empty())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.move_diff_change(true, cx)
                                    })),
                            ),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .children([false, true].map(|after| {
                        let side = if after { "after" } else { "before" };
                        div()
                        .id(SharedString::from(format!("git-diff-{side}")))
                        .test_support()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .when(!after, |pane| {
                            pane.border_r_1().border_color(cx.theme().border)
                        })
                        .child(
                            div()
                                .flex_shrink_0()
                                .bg(cx.theme().muted.opacity(0.35))
                                .child(design::ToolbarFocus::new(
                                    SharedString::from(format!("git-{side}-copy-focus")),
                                    gpui_kit::component::toolbar::Toolbar::new(SharedString::from(
                                        format!("git-{side}-copy-toolbar"),
                                    ))
                                    .with_size(gpui_kit::component::Size::Medium)
                                    .content_height()
                                    .flex_wrap()
                                    .content(div().flex_1().text_xs().child(if after {
                                        "Saved File"
                                    } else {
                                        "Last Commit"
                                    }))
                                    .child(
                                        design::CopyText::new(
                                            SharedString::from(format!("git-copy-{side}")),
                                            if after {
                                                review.after.concat()
                                            } else {
                                                review.before.concat()
                                            },
                                            if after {
                                                "Copy saved file"
                                            } else {
                                                "Copy last committed file"
                                            },
                                        ),
                                    ),
                                )),
                        )
                        .child(
                            div()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .overflow_hidden()
                                .child(
                                    uniform_list(
                                        SharedString::from(format!("git-diff-{side}-lines")),
                                        review.rows.len(),
                                        cx.processor(
                                            move |this, range: std::ops::Range<usize>, _, cx| {
                                                range
                                                    .map(|index| {
                                                        this.render_diff_line(index, after, cx)
                                                    })
                                                    .collect::<Vec<_>>()
                                            },
                                        ),
                                    )
                                    .with_horizontal_sizing_behavior(
                                        gpui_kit::ListHorizontalSizingBehavior::Unconstrained,
                                    )
                                    .track_scroll(&self.diff_scroll)
                                    .size_full(),
                                )
                                .child(gpui_kit::component::scroll::Scrollbar::horizontal(
                                    &self.diff_scroll.0.borrow().base_handle,
                                ))
                                .when(after, |pane| {
                                    pane.child(gpui_kit::component::scroll::Scrollbar::vertical(
                                        &self.diff_scroll.0.borrow().base_handle,
                                    ))
                                }),
                        )
                    })),
            )
            .into_any_element()
    }

    fn render_diff_line(&self, index: usize, after: bool, cx: &Context<Self>) -> AnyElement {
        let Some(Ok(review)) = &self.text_review else {
            return div().into_any_element();
        };
        let row = &review.rows[index];
        let (side, number, lines, color) = if after {
            ("after", row.after, &review.after, cx.theme().success)
        } else {
            ("before", row.before, &review.before, cx.theme().danger)
        };
        let text = number
            .map(|number| lines[number].trim_end_matches(['\r', '\n']))
            .unwrap_or_default();
        div()
            .id(SharedString::from(format!("git-{side}-line-{index}")))
            .role(Role::ListItem)
            .aria_label(text.to_string())
            .test_support()
            .flex()
            .items_center()
            .w_full()
            .min_w(px(review.columns as f32 * 8. + 96.))
            .h(px(24.))
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(px(13.))
            .line_height(px(24.))
            .whitespace_nowrap()
            .when(row.changed, |line| {
                line.bg(if number.is_some() {
                    color.opacity(0.13)
                } else {
                    cx.theme().muted.opacity(0.55)
                })
            })
            .child(
                div()
                    .w(px(56.))
                    .flex_shrink_0()
                    .pr_2()
                    .text_right()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        number
                            .map(|number| (number + 1).to_string())
                            .unwrap_or_default(),
                    ),
            )
            .child(div().w(px(20.)).flex_shrink_0().text_color(color).child(
                if row.changed && number.is_some() {
                    if after { "+" } else { "-" }
                } else {
                    ""
                },
            ))
            .child(text.replace('\t', "    "))
            .into_any_element()
    }
}

impl Render for GitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.disabled || self.active() || self.error.is_some();
        let tracked = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.repository.is_some());
        let snapshot = self.snapshot.as_ref();
        let conflicts = snapshot.map_or(0, Snapshot::conflicts);
        let diverged = snapshot.is_some_and(|snapshot| snapshot.ahead > 0 && snapshot.behind > 0);
        let operation = snapshot.and_then(|snapshot| snapshot.operation.as_deref());
        let blocked = conflicts > 0 || operation.is_some();
        let branch = snapshot
            .map(|snapshot| {
                if snapshot.detached {
                    "Detached HEAD".into()
                } else {
                    snapshot.branch.clone()
                }
            })
            .unwrap_or_else(|| "Git".into());
        let root = snapshot
            .and_then(|snapshot| snapshot.repository.as_ref())
            .map(|repository| display_path(&repository.root))
            .unwrap_or_default();
        let pull_disabled = disabled
            || blocked
            || diverged
            || snapshot.is_none_or(|snapshot| {
                snapshot.detached
                    || snapshot.pull_remote().is_none()
                    || snapshot.head.is_none()
                    || !snapshot.changes.is_empty()
            });
        let push_disabled = disabled
            || blocked
            || snapshot.is_none_or(|snapshot| {
                snapshot.detached
                    || snapshot.head.is_none()
                    || snapshot.default_remote().is_none()
                    || snapshot.behind > 0
            });
        div()
            .id("git-panel")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .child(
                div()
                    .id("git-toolbar")
                    .role(Role::Group)
                    .aria_label("Repository commands")
                    .test_support()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(design::ToolbarFocus::new(
                        "git-toolbar-focus",
                        gpui_kit::component::toolbar::Toolbar::new("git-command-toolbar")
                            .with_size(gpui_kit::component::Size::Medium)
                            .content_height()
                            .flex_wrap()
                            .content(Icon::new(gpui_kit::assets::IconName::GitBranch).size_4())
                            .content(
                                div()
                                    .id("git-branch")
                                    .role(Role::Status)
                                    .aria_label(branch.clone())
                                    .test_support()
                                    .max_w(px(220.))
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(branch),
                            )
                            .content(div().flex_1())
                            .child(
                                Button::new("git-refresh")
                                    .ghost()
                                    .loading(self.scan.is_some())
                                    .icon(gpui_kit::assets::IconName::RefreshCw)
                                    .tooltip("Refresh local changes")
                                    .accessibility_label("Refresh Git status")
                                    .disabled(self.scan.is_some() || self.active())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.refresh(window, cx)),
                                    ),
                            )
                            .child(
                                Button::new("git-fetch")
                                    .loading(self.fetch.is_some())
                                    .icon(gpui_kit::assets::IconName::CloudDownload)
                                    .label("Fetch")
                                    .tooltip("Check remote commits without changing files")
                                    .disabled(
                                        disabled
                                            || self.fetch.is_some()
                                            || snapshot
                                                .and_then(Snapshot::default_remote)
                                                .is_none(),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.check_remote(true, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("git-pull")
                                    .icon(gpui_kit::assets::IconName::ArrowDown)
                                    .label("Pull")
                                    .tooltip("Fast-forward only; commit local changes first")
                                    .disabled(pull_disabled)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.request(model::Action::Pull, cx)
                                    })),
                            )
                            .child(
                                Button::new("git-push")
                                    .icon(gpui_kit::assets::IconName::ArrowUp)
                                    .label("Push")
                                    .tooltip("Push commits without overwriting remote history")
                                    .disabled(push_disabled)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.request(model::Action::Push, cx)
                                    })),
                            )
                            .child(
                                Button::new("git-open-vscode")
                                    .icon(gpui_kit::assets::IconName::ExternalLink)
                                    .label("Open in VS Code")
                                    .disabled(self.root.is_none())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_vscode(false, cx)),
                                    ),
                            )
                            .when(self.active() || self.fetch.is_some(), |bar| {
                                bar.child(
                                    Button::new("git-stop")
                                        .icon(gpui_kit::assets::IconName::Square)
                                        .label("Stop")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cancel_operation(cx);
                                        })),
                                )
                            }),
                    )),
            )
            .when(tracked, |panel| {
                panel.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .id("git-repository-path")
                                .role(Role::Status)
                                .aria_label(root.clone())
                                .test_support()
                                .text_xs()
                                .truncate()
                                .child(root),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .flex_wrap()
                                .gap_3()
                                .child(
                                    div()
                                        .id("git-incoming")
                                        .role(Role::Status)
                                        .aria_label(format!(
                                            "{} incoming commits",
                                            snapshot.map_or(0, |snapshot| snapshot.behind)
                                        ))
                                        .test_support()
                                        .text_color(
                                            if snapshot.is_some_and(|snapshot| snapshot.behind > 0)
                                            {
                                                cx.theme().warning
                                            } else {
                                                cx.theme().muted_foreground
                                            },
                                        )
                                        .child(format!(
                                            "{} incoming",
                                            snapshot.map_or(0, |snapshot| snapshot.behind)
                                        )),
                                )
                                .child(div().text_color(cx.theme().muted_foreground).child(
                                    format!(
                                        "{} outgoing",
                                        snapshot.map_or(0, |snapshot| snapshot.ahead)
                                    ),
                                ))
                                .child(
                                    div().text_xs().child(
                                        snapshot
                                            .and_then(|snapshot| {
                                                snapshot.comparison_upstream.as_deref()
                                            })
                                            .unwrap_or("No remote branch")
                                            .to_string(),
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .id("git-remote-status")
                                .role(Role::Status)
                                .aria_label(self.remote_status())
                                .test_support()
                                .text_xs()
                                .text_color(if self.remote_error.is_some() {
                                    cx.theme().warning
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(self.remote_status()),
                        ),
                )
            })
            .when(blocked || diverged, |panel| {
                let warning = format!(
                    "{}{}. Resolve in VS Code, then refresh Git status.",
                    operation.unwrap_or(if diverged {
                        "Branches diverged"
                    } else {
                        "Merge conflicts"
                    }),
                    if conflicts > 0 {
                        format!(" | {conflicts} conflicted file(s)")
                    } else {
                        String::new()
                    }
                );
                panel.child(
                    div()
                        .id("git-conflict-banner")
                        .role(Role::Alert)
                        .aria_label(warning.clone())
                        .test_support()
                        .px_3()
                        .py_2()
                        .bg(cx.theme().warning.opacity(0.12))
                        .child(warning),
                )
            })
            .when_some(self.error.as_ref(), |panel, error| {
                panel.child(
                    div()
                        .p_3()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
            })
            .when(
                !tracked && self.snapshot.is_some() && self.error.is_none(),
                |panel| {
                    panel.child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .child(Icon::new(gpui_kit::assets::IconName::GitBranch).size(px(32.)))
                            .child(
                                div()
                                    .id("git-untracked-workspace")
                                    .role(Role::Status)
                                    .aria_label("Workspace is not tracked by Git")
                                    .test_support()
                                    .child("Workspace is not tracked by Git"),
                            )
                            .child(
                                Button::new("git-init")
                                    .primary()
                                    .icon(gpui_kit::assets::IconName::Plus)
                                    .label("Initialize Git")
                                    .disabled(disabled)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.request(model::Action::Initialize, cx)
                                    })),
                            ),
                    )
                },
            )
            .when(tracked, |panel| panel.child(self.render_remotes(cx)))
            .when(tracked, |panel| {
                panel.child(
                    div().flex_1().min_h_0().child(
                        h_resizable("git-review-layout")
                            .child(
                                resizable_panel()
                                    .size(px(480.))
                                    .size_range(px(230.)..px(540.))
                                    .child(self.render_changes(cx)),
                            )
                            .child(
                                resizable_panel().child(
                                    div()
                                        .size_full()
                                        .flex()
                                        .flex_col()
                                        .min_h_0()
                                        .min_w_0()
                                        .child(
                                            div().flex_1().min_h_0().child(self.render_review(cx)),
                                        )
                                        .child(self.render_activity(cx)),
                                ),
                            ),
                    ),
                )
            })
            .when(!tracked, |panel| panel.child(self.render_activity(cx)))
            .when(!self.status.is_empty(), |panel| {
                panel.child(
                    div()
                        .id("git-operation-status")
                        .role(Role::Status)
                        .aria_label(self.status.clone())
                        .test_support()
                        .h(px(40.))
                        .flex_shrink_0()
                        .overflow_y_scrollbar()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .px_3()
                        .py_2()
                        .text_sm()
                        .child(self.status.clone()),
                )
            })
    }
}
