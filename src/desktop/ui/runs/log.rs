use super::*;

pub(super) struct ScenarioLogScope {
    jobs: BTreeSet<usize>,
    title: String,
}

impl RunPanel {
    pub(super) fn set_log_follow(
        &mut self,
        follow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.log_follow && !follow {
            // The bounded reader clamps this request to the last page after counting matches.
            self.log_query.offset = usize::MAX;
        }
        self.log_follow = follow;
        self.archive_key = None;
        self.refresh_log(window, cx);
        cx.notify();
    }

    fn scenario_jobs(&self, suite: &str, scenario: &str) -> BTreeSet<usize> {
        let output = self
            .executed_suites
            .iter()
            .find(|(name, _)| name == suite)
            .map(|(_, id)| id.as_str())
            .or_else(|| {
                self.suites
                    .iter()
                    .find(|entry| entry.name == suite)
                    .map(|entry| entry.output_id.as_str())
            });
        self.manager
            .progress
            .scenarios
            .values()
            .filter(|entry| {
                entry.job.scenario_id == scenario
                    && output.map_or(entry.job.suite_name == suite, |output| {
                        output == entry.job.suite_id
                    })
            })
            .map(|entry| entry.job.id)
            .collect()
    }

    pub(in crate::desktop::ui) fn has_scenario_log(&self, suite: &str, scenario: &str) -> bool {
        !self.scenario_jobs(suite, scenario).is_empty()
    }

    pub(in crate::desktop::ui) fn show_scenario_log(
        &mut self,
        suite: &str,
        scenario: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let jobs = self.scenario_jobs(suite, scenario);
        let Some(entry) = jobs
            .first()
            .and_then(|id| self.manager.progress.scenarios.get(id))
        else {
            return;
        };
        self.log_scope = Some(ScenarioLogScope {
            title: format!(
                "{} / {}",
                self.suites
                    .iter()
                    .find(|suite| suite.output_id == entry.job.suite_id)
                    .map(|suite| suite.label())
                    .unwrap_or(&entry.job.suite_name),
                entry.job.label
            ),
            jobs,
        });
        self.log_query.offset = 0;
        self.archive_key = None;
        self.tab = 3;
        self.refresh_log(window, cx);
        cx.notify();
    }

    pub(super) fn show_job_log(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.manager.progress.scenarios.get(&id) else {
            return;
        };
        self.log_scope = Some(ScenarioLogScope {
            jobs: BTreeSet::from([id]),
            title: format!(
                "{} / {}{}",
                self.suites
                    .iter()
                    .find(|suite| suite.output_id == entry.job.suite_id)
                    .map(|suite| suite.label())
                    .unwrap_or(&entry.job.suite_name),
                entry.job.label,
                if entry.job.viewport.is_empty() {
                    String::new()
                } else {
                    format!(" / {}", entry.job.viewport)
                }
            ),
        });
        self.log_query.offset = 0;
        self.archive_key = None;
        self.tab = 3;
        self.refresh_log(window, cx);
        cx.notify();
    }

    pub(super) fn show_all_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.log_scope = None;
        self.log_query.offset = 0;
        self.archive_key = None;
        self.tab = 3;
        self.refresh_log(window, cx);
        cx.notify();
    }

    pub(super) fn refresh_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Captures retain output in the manager; rebuilding an unmounted editor can
        // otherwise process megabytes on the UI thread every progress tick.
        if self.running()
            && self.tab != 3
            && self
                .manager
                .request
                .as_ref()
                .is_some_and(|request| request.command != RunCommand::Deploy)
        {
            return;
        }
        let legacy_deployment_activity = self.tab != 3
            && self.manager.request.as_ref().is_some_and(|request| {
                request.command == RunCommand::Deploy
                    && request.options.diagnostic_operation_id.is_none()
            });
        if !legacy_deployment_activity
            && self.manager.request.is_some()
            && (!self.running()
                || self.tab == 3
                || self
                    .manager
                    .request
                    .as_ref()
                    .is_some_and(|request| request.command == RunCommand::Deploy))
        {
            self.refresh_archive(window, cx);
            return;
        }
        self.archive_key = None;
        self.archive_page = None;
        self.archive_loading = false;
        let running = self.running();
        self.log_controls
            .update(cx, |controls, cx| controls.availability(None, running, cx));
        let text = if let Some(scope) = &self.log_scope {
            let progress = &self.manager.progress;
            let mut text = String::new();
            if progress.omitted_logs > 0 {
                text.push_str(&format!(
                    "[{} older scenario log entries omitted from the run's bounded history]\n",
                    progress.omitted_logs
                ));
            }
            for entry in progress
                .logs
                .iter()
                .filter(|entry| scope.jobs.contains(&entry.job_id))
            {
                let viewport = if scope.jobs.len() > 1 {
                    progress
                        .scenarios
                        .get(&entry.job_id)
                        .map(|job| format!(" [{}]", job.job.viewport))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                text.push_str(&format!(
                    "[{:02}:{:02}.{:03}]{viewport} {}\n",
                    entry.elapsed_ms / 60_000,
                    entry.elapsed_ms / 1000 % 60,
                    entry.elapsed_ms % 1000,
                    entry.message
                ));
            }
            if text.is_empty() {
                text.push_str("No retained log entries for this scenario.\n");
            }
            text
        } else {
            let capacity = self
                .manager
                .logs
                .iter()
                .map(|line| line.text.len() + if line.stderr { "[stderr] ".len() } else { 0 })
                .sum();
            let mut text = String::with_capacity(capacity);
            for line in &self.manager.logs {
                if line.stderr {
                    text.push_str("[stderr] ");
                }
                text.push_str(&line.text);
            }
            text
        };
        if text != self.log_text {
            self.log.update(cx, |state, cx| {
                state.set_value(text.clone(), window, cx);
                if self.log_follow && running {
                    state.set_selected_range(text.len()..text.len(), cx);
                }
            });
            self.log_text = text;
        }
    }

    #[cfg(test)]
    pub(in crate::desktop::ui) fn rendered_log(&self) -> &str {
        &self.log_text
    }

    #[cfg(test)]
    pub(in crate::desktop::ui) fn log_controls_handle(
        &self,
    ) -> Entity<super::super::log_controls::LogControls> {
        self.log_controls.clone()
    }

    fn refresh_archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let running = self.running();
        if running && self.archive_loading && self.archive_key.is_some() {
            return;
        }
        let Some(mut request) = self.manager.request.clone() else {
            return;
        };
        if self.log_scope.is_some() {
            // Scenario and job views show only their capture journals, not the outer command log.
            request.options.diagnostic_operation_id = None;
        }
        let jobs = self
            .manager
            .progress
            .scenarios
            .values()
            .filter(|entry| {
                self.log_scope
                    .as_ref()
                    .is_none_or(|scope| scope.jobs.contains(&entry.job.id))
            })
            .map(|entry| entry.job.clone())
            .collect::<Vec<_>>();
        let query = self.log_query.clone();
        let tail = running && self.log_follow;
        let revision = if tail { self.manager.revision() } else { 0 };
        let key = serde_json::to_string(&(&request, &jobs, &query, revision, running))
            .unwrap_or_default();
        if self.archive_key.as_ref() == Some(&key) {
            return;
        }
        self.archive_key = Some(key.clone());
        self.archive_generation = self.archive_generation.wrapping_add(1);
        let generation = self.archive_generation;
        self.archive_page = None;
        self.archive_loading = true;
        self.log_controls
            .update(cx, |controls, cx| controls.availability(None, running, cx));
        if !running || self.log_text.is_empty() {
            self.log_text = "Loading archived log...".into();
            self.log.update(cx, |state, cx| {
                state.set_value(self.log_text.clone(), window, cx)
            });
        }
        cx.spawn_in(window, async move |view, cx| {
            let page = cx
                .background_executor()
                .spawn(async move {
                    if tail {
                        crate::desktop::logs::load_run_tail(&request, &jobs, &query)
                    } else {
                        crate::desktop::logs::load_run_page(&request, &jobs, &query)
                    }
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                if this.archive_key.as_ref() != Some(&key) || this.archive_generation != generation
                {
                    return;
                }
                let running = this.running();
                this.archive_loading = false;
                this.log_text = page.text();
                if running && page.state == crate::diagnostics::journal::ArchiveState::Partial {
                    this.log_text = this.log_text.replace(
                        "Partial / interrupted archive; some records may be missing",
                        "Live archive; not yet finalized",
                    );
                }
                this.log.update(cx, |state, cx| {
                    state.set_value(this.log_text.clone(), window, cx);
                    if tail && running {
                        state.set_selected_range(this.log_text.len()..this.log_text.len(), cx);
                    }
                });
                this.log_query.offset = page.offset;
                this.log_controls.update(cx, |controls, cx| {
                    controls.availability(Some(page.state), running, cx)
                });
                this.archive_page = Some(page);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_log(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = self
            .log_scope
            .as_ref()
            .map(|scope| scope.title.clone())
            .unwrap_or_else(|| "All run logs".into());
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .child(
                div()
                    .id("run-log-toolbar")
                    .role(Role::Group)
                    .aria_label("Run log commands")
                    .test_support()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(design::ToolbarFocus::new(
                        "run-log-toolbar-focus",
                        gpui_kit::component::toolbar::Toolbar::new("run-log-command-toolbar")
                            .with_size(gpui_kit::component::Size::Medium)
                            .content_height()
                            .flex_wrap()
                            .child(
                                Button::new("log-activity")
                                    .ghost()
                                    .icon(IconName::ArrowLeft)
                                    .tooltip("Back to Activity")
                                    .accessibility_label("Back to Activity")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.tab = 0;
                                        cx.notify();
                                    })),
                            )
                            .content(
                                div()
                                    .id("run-log-heading")
                                    .role(Role::Heading)
                                    .aria_label(title.clone())
                                    .test_support()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(title),
                            )
                            .when(self.log_scope.is_some(), |bar| {
                                bar.child(
                                    Button::new("log-all")
                                        .ghost()
                                        .icon(gpui_kit::assets::IconName::List)
                                        .label("All run logs")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.show_all_logs(window, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("copy-run-log")
                                    .ghost()
                                    .icon(IconName::Copy)
                                    .tooltip(if self.archive_page.is_some() {
                                        "Copy displayed log page"
                                    } else {
                                        "Copy log"
                                    })
                                    .accessibility_label(if self.archive_page.is_some() {
                                        "Copy displayed log page"
                                    } else {
                                        "Copy log"
                                    })
                                    .disabled(
                                        self.log_text.is_empty()
                                            || self
                                                .archive_page
                                                .as_ref()
                                                .is_some_and(|page| !page.state.can_export()),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            this.log_text.clone(),
                                        ))
                                    })),
                            )
                            .when_some(
                                self.archive_page
                                    .as_ref()
                                    .filter(|_| !(self.running() && self.log_follow)),
                                |bar, page| {
                                    bar.child(
                                        gpui_kit::component::pagination::Pagination::new(
                                            "run-log-pagination",
                                        )
                                        .compact()
                                        .current_page(page.page_number())
                                        .total_pages(page.page_count())
                                        .disabled(self.archive_loading)
                                        .on_click(
                                            cx.listener(|this, page: &usize, window, cx| {
                                                if this.archive_loading
                                                    || (this.running() && this.log_follow)
                                                {
                                                    return;
                                                }
                                                let Some(offset) =
                                                    this.archive_page.as_ref().and_then(
                                                        |current| current.offset_for_page(*page),
                                                    )
                                                else {
                                                    return;
                                                };
                                                this.log_query.offset = offset;
                                                this.archive_key = None;
                                                this.refresh_log(window, cx);
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .content(
                                        div()
                                            .id("run-log-page-number")
                                            .role(Role::Status)
                                            .aria_label(format!(
                                                "Page {} of {}",
                                                page.page_number(),
                                                page.page_count()
                                            ))
                                            .test_support()
                                            .text_xs()
                                            .child(format!(
                                                "Page {} of {} | {} records",
                                                page.page_number(),
                                                page.page_count(),
                                                page.matched
                                            )),
                                    )
                                },
                            ),
                    )),
            )
            .child(self.log_controls.clone())
            .child(
                div()
                    .id("run-log-editor")
                    .size_full()
                    .flex_1()
                    .min_h_0()
                    .child(
                        CodeEditor::new(&self.log)
                            .aria_label(if self.log_scope.is_some() {
                                "Scenario run log"
                            } else {
                                "Run log"
                            })
                            .size_full()
                            .readonly(true),
                    ),
            )
            .into_any_element()
    }
}
