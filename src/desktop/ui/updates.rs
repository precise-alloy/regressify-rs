use super::*;
use crate::desktop::updates::{
    CURRENT_VERSION, DownloadControl, PreparedUpdate, Release, RetryScheduled,
    SHARED_POLL_INTERVAL, SharedUpdateCheck, SkippedUpdate, UpdateCheck, UpdateFeed, UpdateLease,
};
use futures::FutureExt;

pub(super) struct UpdatePanel {
    feed: Option<UpdateFeed>,
    working: bool,
    available: Option<Release>,
    prepared: Option<PreparedUpdate>,
    message: String,
    download: Option<DownloadControl>,
    failed: bool,
    retry: Option<RetryScheduled>,
    retry_task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
    startup_check_pending: bool,
    lease: Option<UpdateLease>,
    pub(super) install_on_close: bool,
}

impl Default for UpdatePanel {
    fn default() -> Self {
        let feed = UpdateFeed::configured();
        Self {
            message: if feed.is_some() {
                "Ready to check for updates".into()
            } else {
                "Updates are unavailable in this build".into()
            },
            feed,
            working: false,
            available: None,
            prepared: None,
            download: None,
            failed: false,
            retry: None,
            retry_task: None,
            poll_task: None,
            startup_check_pending: true,
            lease: None,
            install_on_close: false,
        }
    }
}

impl UpdatePanel {
    fn begin_download(&mut self, release: &Release) -> DownloadControl {
        let control = DownloadControl::new(release);
        self.working = true;
        self.failed = false;
        self.retry = None;
        self.retry_task = None;
        self.message = control.progress().message();
        self.download = Some(control.clone());
        control
    }

    fn refresh_download(&mut self) -> bool {
        let Some(download) = &self.download else {
            return false;
        };
        let message = if download.is_cancelled() {
            "Cancelling download...".into()
        } else {
            download.progress().message()
        };
        if self.message == message {
            return false;
        }
        self.message = message;
        true
    }

    fn complete_download(&mut self, result: Result<PreparedUpdate>) {
        self.working = false;
        self.failed = false;
        self.available = None;
        self.retry = None;
        self.retry_task = None;
        if self
            .download
            .take()
            .is_some_and(|control| control.is_cancelled())
        {
            self.message =
                "Download paused. Preparation will resume at the next scheduled check; Check for updates retries now.".into();
            return;
        }
        self.message = match result {
            Ok(prepared) => {
                let message = format!(
                    "Version {} is verified and ready to install",
                    prepared.version()
                );
                self.prepared = Some(prepared);
                message
            }
            Err(error) if error.is::<SkippedUpdate>() => error.to_string(),
            Err(error) if error.is::<RetryScheduled>() => {
                self.retry = error.downcast_ref::<RetryScheduled>().copied();
                error.to_string()
            }
            Err(error) => {
                self.failed = true;
                format!(
                    "Update preparation paused: {error:#}. No update was installed. Preparation will resume at the next scheduled check; Check for updates retries now."
                )
            }
        };
    }
}

impl Drop for UpdatePanel {
    fn drop(&mut self) {
        if let Some(download) = &self.download {
            download.cancel_for_handoff();
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct UpdateBannerColors {
    background: Hsla,
    border: Hsla,
    title: Hsla,
    detail: Hsla,
    #[cfg(all(test, feature = "gui-tests"))]
    action_background: Hsla,
    #[cfg(all(test, feature = "gui-tests"))]
    action_hover: Hsla,
    #[cfg(all(test, feature = "gui-tests"))]
    action_active: Hsla,
    #[cfg(all(test, feature = "gui-tests"))]
    action_foreground: Hsla,
}

fn update_banner_colors(cx: &App) -> UpdateBannerColors {
    let theme = cx.theme();
    UpdateBannerColors {
        background: theme.muted,
        border: theme.border,
        title: theme.foreground,
        detail: theme.muted_foreground,
        #[cfg(all(test, feature = "gui-tests"))]
        action_background: theme.button_primary,
        #[cfg(all(test, feature = "gui-tests"))]
        action_hover: theme.button_primary_hover,
        #[cfg(all(test, feature = "gui-tests"))]
        action_active: theme.button_primary_active,
        #[cfg(all(test, feature = "gui-tests"))]
        action_foreground: theme.button_primary_foreground,
    }
}

impl Workbench {
    pub(super) fn render_update_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let update = self.updates.read(cx);
        let available = update.prepared.is_some();
        let label = if available {
            "Update ready to install"
        } else {
            "Software updates"
        };
        Button::new("software-updates")
            .ghost()
            .when(available, |button| button.primary())
            .icon(if available {
                gpui_kit::assets::IconName::Download
            } else {
                gpui_kit::assets::IconName::RefreshCw
            })
            .accessibility_label(label)
            .tooltip(label)
            .disabled(self.busy)
            .on_click(cx.listener(|this, _, window, cx| this.open_updates(window, cx)))
    }

    pub(super) fn render_update_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let update = self.updates.read(cx);
        let version = update.prepared.as_ref()?.version();
        let title = format!("Regressify {version} is ready to install");
        let detail = update.message.clone();
        let colors = update_banner_colors(cx);
        Some(
            div()
                .id("update-banner")
                .role(Role::Status)
                .aria_label(format!("{title}. {detail}"))
                .test_support()
                .gap_3()
                .w_full()
                .flex_shrink_0()
                .border_b_1()
                .border_color(colors.border)
                .bg(colors.background)
                .child(
                    gpui_kit::component::marker::Marker::new()
                        .w_full()
                        .min_h(px(64.))
                        .items_center()
                        .gap_3()
                        .px_4()
                        .py_3()
                        .text_color(colors.title)
                        .child(
                            Icon::new(gpui_kit::assets::IconName::Download)
                                .size_5()
                                .flex_shrink_0(),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .id("update-banner-title")
                                        .test_support()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .id("update-banner-detail")
                                        .test_support()
                                        .text_xs()
                                        .text_color(colors.detail)
                                        .whitespace_normal()
                                        .child(detail),
                                ),
                        )
                        .child(
                            Button::new("update-banner-action")
                                .primary()
                                .flex_shrink_0()
                                .icon(gpui_kit::assets::IconName::RotateCw)
                                .label("Install update")
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.open_updates(window, cx)
                                    }),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn open_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let updates = self.updates.clone();
        let install_blocked = self.busy || self.runs.read(cx).running();
        window.open_dialog(cx, move |dialog, window, cx| {
            let state = updates.read(cx);
            let working = state.working;
            let prepared = state.prepared.is_some();
            dialog
                .title("Software updates")
                .child(format!("Regressify Desktop {CURRENT_VERSION}"))
                .child(
                    div()
                        .id("update-status")
                        .role(Role::Status)
                        .aria_label(state.message.clone())
                        .test_support()
                        .text_sm()
                        .w_full()
                        .whitespace_normal()
                        .child(state.message.clone()),
                )
                .when_some(state.download.as_ref(), |dialog, control| {
                    let progress = control.progress();
                    dialog.child(
                        progress::Progress::new("update-download-progress")
                            .accessibility_label("Installer download progress")
                            .value(
                                (progress.received as f32 / progress.total.max(1) as f32)
                                    .clamp(0., 1.)
                                    * 100.,
                            )
                            .color(cx.theme().primary),
                    )
                })
                .footer(
                    gpui_kit::component::dialog::DialogFooter::new()
                        .flex_wrap()
                        .child(
                            Button::new("update-close")
                                .label("Close")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .when(state.failed, |footer| {
                            footer.child(design::CopyText::new(
                                "update-copy-details",
                                state.message.clone(),
                                "Copy details",
                            ))
                        })
                        .when_some(state.download.as_ref(), |footer, control| {
                            footer.child(
                                Button::new("update-cancel-download")
                                    .icon(gpui_kit::assets::IconName::X)
                                    .label("Pause download")
                                    .disabled(control.is_cancelled())
                                    .on_click(window.listener_for(&view, |this, _, _, cx| {
                                        this.updates.update(cx, |state, cx| {
                                            if let Some(download) = &state.download {
                                                download.cancel();
                                                state.refresh_download();
                                                cx.notify();
                                            }
                                        });
                                        cx.notify();
                                    })),
                            )
                        })
                        .child(
                            Button::new("update-check")
                                .icon(gpui_kit::assets::IconName::RefreshCw)
                                .label("Check for updates")
                                .disabled(state.feed.is_none() || working)
                                .on_click(window.listener_for(&view, |this, _, window, cx| {
                                    this.start_update_check(true, window, cx);
                                })),
                        )
                        .when(prepared, |footer| {
                            footer.child(
                                Button::new("update-install")
                                    .primary()
                                    .icon(gpui_kit::assets::IconName::RotateCw)
                                    .label("Install and restart")
                                    .disabled(working || install_blocked)
                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                        this.request_update_install(window, cx);
                                    })),
                            )
                        }),
                )
        });
    }

    pub(super) fn check_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.watch_shared_updates(SHARED_POLL_INTERVAL, window, cx);
        self.start_update_check(false, window, cx);
    }

    fn watch_shared_updates(
        &mut self,
        interval: std::time::Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.updates.read(cx);
        if state.feed.is_none() || state.poll_task.is_some() {
            return;
        }
        let task = cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                if view
                    .update_in(cx, |this, window, cx| {
                        this.start_update_check(false, window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.updates
            .update(cx, |state, _| state.poll_task = Some(task));
    }

    fn start_update_check(&mut self, manual: bool, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.updates.read(cx);
        if state.working {
            return;
        }
        let Some(feed) = state.feed.clone() else {
            return;
        };
        let startup = state.startup_check_pending;
        let prepared = state
            .prepared
            .as_ref()
            .map(|update| update.release().clone());
        let cache = self
            .preferences_path
            .as_ref()
            .map(|path| path.with_file_name("updates"));
        self.updates.update(cx, |state, cx| {
            state.working = true;
            state.available = None;
            state.lease = None;
            if manual {
                state.failed = false;
                state.prepared = None;
                state.retry = None;
                state.retry_task = None;
                state.message = "Checking for updates...".into();
            }
            cx.notify();
        });
        let task = cx.background_executor().spawn(async move {
            let cache = cache.ok_or_else(|| anyhow!("Cannot locate the update cache"))?;
            let current = CURRENT_VERSION.parse()?;
            if startup && !manual {
                feed.check_on_startup(current, &cache, prepared.as_ref())
            } else {
                feed.check_shared(current, &cache, manual, prepared.as_ref())
            }
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                let mut prepare = false;
                this.updates.update(cx, |state, cx| {
                    state.working = false;
                    if !matches!(&result, Ok(SharedUpdateCheck::Busy)) {
                        state.startup_check_pending = false;
                    }
                    state.retry = None;
                    state.retry_task = None;
                    state.message = match result {
                        Ok(SharedUpdateCheck::Busy) => {
                            if state.prepared.is_some() {
                                state.message.clone()
                            } else {
                                "Another Regressify instance is checking or preparing the update..."
                                    .into()
                            }
                        }
                        Ok(SharedUpdateCheck::Unchanged) => {
                            if state.message == "Ready to check for updates"
                                || state.message == "Checking for updates..."
                                || state.message.starts_with("Another Regressify instance")
                            {
                                "No new update is ready to install".into()
                            } else {
                                state.message.clone()
                            }
                        }
                        Ok(SharedUpdateCheck::Checked(UpdateCheck::Available(release), lease)) => {
                            state.available = Some(release);
                            state.prepared = None;
                            state.failed = false;
                            state.lease = Some(lease);
                            prepare = true;
                            "Preparing update...".into()
                        }
                        Ok(SharedUpdateCheck::Checked(UpdateCheck::Current, _)) => {
                            state.available = None;
                            state.failed = false;
                            "You have the latest version".into()
                        }
                        Ok(SharedUpdateCheck::Checked(UpdateCheck::Unpublished, _)) => {
                            state.available = None;
                            state.failed = false;
                            "No releases have been published yet".into()
                        }
                        Err(error) if error.is::<SkippedUpdate>() => error.to_string(),
                        Err(error) if error.is::<RetryScheduled>() => {
                            state.retry = error.downcast_ref::<RetryScheduled>().copied();
                            error.to_string()
                        }
                        Err(error) => {
                            state.failed = true;
                            format!("Update check failed: {error:#}")
                        }
                    };
                    cx.notify();
                });
                if prepare {
                    this.download_update(window, cx);
                } else {
                    this.schedule_update_retry(window, cx);
                }
                window.refresh();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn schedule_update_retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(retry) = self.updates.read(cx).retry else {
            return;
        };
        let task = cx.spawn_in(window, async move |view, cx| {
            cx.background_executor().timer(retry.delay()).await;
            let _ = view.update_in(cx, |this, window, cx| {
                if this.updates.read(cx).retry == Some(retry) {
                    this.check_updates(window, cx);
                }
            });
        });
        self.updates
            .update(cx, |state, _| state.retry_task = Some(task));
    }

    fn download_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.updates.read(cx);
        if state.working || state.prepared.is_some() {
            return;
        }
        let Some(release) = state.available.clone() else {
            return;
        };
        let (control, lease) = self.updates.update(cx, |state, cx| {
            let control = state.begin_download(&release);
            let lease = state.lease.take();
            cx.notify();
            (control, lease)
        });
        let worker_control = control.clone();
        let task = cx.background_executor().spawn(async move {
            lease
                .ok_or_else(|| anyhow!("Update preparation ownership was lost"))?
                .prepare(release, worker_control)
        });
        self.observe_update_download(task, control, window, cx);
    }

    fn observe_update_download(
        &mut self,
        task: Task<Result<PreparedUpdate>>,
        control: DownloadControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |view, cx| {
            let task = task.fuse();
            futures::pin_mut!(task);
            loop {
                let timer = cx
                    .background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .fuse();
                futures::pin_mut!(timer);
                futures::select_biased! {
                    result = task => {
                        let _ = view.update_in(cx, |this, window, cx| {
                            this.updates.update(cx, |state, cx| {
                                state.complete_download(result);
                                cx.notify();
                            });
                            this.schedule_update_retry(window, cx);
                            window.refresh();
                            cx.notify();
                        });
                        break;
                    }
                    _ = timer => {
                        if view.update_in(cx, |this, window, cx| {
                            let changed = this.updates.update(cx, |state, cx| {
                                let changed = state.refresh_download();
                                if changed {
                                    cx.notify();
                                }
                                changed
                            });
                            if changed {
                                window.refresh();
                                cx.notify();
                            }
                        }).is_err() {
                            control.cancel_for_handoff();
                            break;
                        }
                    }
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn request_update_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.runs.read(cx).running() || self.updates.read(cx).prepared.is_none() {
            return;
        }
        if self.session.open.is_some()
            && let Err(error) = self.flush_drafts(window, cx)
        {
            self.status = format!("Cannot install update: {error:#}");
            self.updates.update(cx, |state, cx| {
                state.message = format!("Cannot install update: {error:#}");
                cx.notify();
            });
            cx.notify();
            return;
        }
        window.close_dialog(cx);
        self.guard(files::Pending::Update, window, cx);
    }

    pub(super) fn finish_update(&mut self, cx: &mut Context<Self>) {
        if self.busy
            || self.unsaved(cx)
            || self.runs.read(cx).running()
            || self.updates.read(cx).prepared.is_none()
        {
            return;
        }
        self.updates
            .update(cx, |state, _| state.install_on_close = true);
        self.finish_close(cx);
    }

    pub(super) fn quit_or_install(&mut self, cx: &mut Context<Self>) {
        if self.updates.read(cx).install_on_close {
            let workspace = self
                .session
                .workspace
                .as_ref()
                .map(|workspace| workspace.root.clone());
            let result = self.updates.update(cx, |state, cx| {
                state.install_on_close = false;
                let result = state
                    .prepared
                    .as_mut()
                    .ok_or_else(|| anyhow!("No verified update is ready"))?
                    .launch(workspace.as_deref());
                if let Err(error) = &result {
                    state.message = format!("Cannot start installer: {error:#}");
                }
                cx.notify();
                result
            });
            if let Err(error) = result {
                self.status = format!("Cannot install update: {error:#}");
                cx.notify();
                return;
            }
        }
        #[cfg(target_os = "windows")]
        self.taskbar.take();
        cx.quit();
    }
}

#[cfg(all(test, feature = "gui-tests"))]
mod tests {
    use std::time::Duration;

    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use gpui_kit::{AppContext, TestAppContext, px, size};

    use super::{PreparedUpdate, UpdateFeed};
    use crate::desktop::ui::{
        design,
        tests::{editor, editor_at_size, settle_dialog},
    };
    use crate::desktop::updates::Release;

    fn available_release() -> Release {
        UpdateFeed::new("https://regressify.eshn.dev")
            .unwrap()
            .parse_manifest(
                &serde_json::json!({
                    "version": "9999.9999.9999.9999",
                    "installer": "https://github.com/precise-alloy/regressify-rs/releases/download/v9999.9999.9999.9999/Regressify-Setup-9999.9999.9999.9999-x64.exe",
                    "sha256": "a".repeat(64),
                    "size": 100 * 1024 * 1024,
                    "archive.regressify-desktop-x86_64-pc-windows-msvc.zip": format!("15:{:x}", <sha2::Sha256 as sha2::Digest>::digest(b"desktop-archive")),
                    "archive.regressify-cli-x86_64-pc-windows-msvc.zip": format!("11:{:x}", <sha2::Sha256 as sha2::Digest>::digest(b"cli-archive")),
                })
                .to_string(),
            )
            .unwrap()
    }

    #[gpui_kit::test]
    async fn update_install_does_not_require_an_open_document_or_workspace(
        cx: &mut TestAppContext,
    ) {
        for workspace_open in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (handle, view) = editor(cx, directory.path());
            cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, cx| {
                    if workspace_open {
                        this.session.open = None;
                        this.sync_editor(window, cx);
                    } else {
                        this.close_workspace(window, cx);
                    }
                    this.updates.update(cx, |state, cx| {
                        state.prepared = Some(
                            PreparedUpdate::missing_installer_for_test(available_release())
                                .unwrap(),
                        );
                        cx.notify();
                    });
                    this.open_updates(window, cx);
                });
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
                !view.read(cx).preferences_saving
            })
            .await;
            settle_dialog(cx, handle).await;
            cx.update_window(handle.into(), |_, window, cx| {
                assert_eq!(view.read(cx).session.workspace.is_some(), workspace_open);
                assert!(view.read(cx).session.open.is_none());
                assert!(!view.read(cx).unsaved(cx));
                assert!(window.find("update-install").visible());
                window.click("update-install", cx);
                let state = view.read(cx).updates.read(cx);
                assert!(
                    state.message.starts_with("Cannot start installer:"),
                    "{}",
                    state.message
                );
                assert!(state.prepared.is_some());
                assert!(!state.install_on_close);
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    async fn update_install_flushes_pending_edits_and_preserves_the_cancel_guard(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.updates.update(cx, |state, cx| {
                    state.prepared = Some(
                        PreparedUpdate::missing_installer_for_test(available_release()).unwrap(),
                    );
                    cx.notify();
                });
                this.tab = 1;
                let draft = format!("{}\n# Pending update draft\n", this.source.read(cx).value());
                this.source
                    .update(cx, |state, cx| state.set_value(draft.clone(), window, cx));
                this.request_update_install(window, cx);
                assert_eq!(this.session.open.as_ref().unwrap().document.source(), draft);
            });
        })
        .unwrap();
        settle_dialog(cx, handle).await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.find("guard-save").visible());
            assert!(window.find("guard-discard").visible());
            window.click("guard-cancel", cx);
            assert!(view.read(cx).unsaved(cx));
            let state = view.read(cx).updates.read(cx);
            assert!(state.prepared.is_some());
            assert!(!state.install_on_close);
            assert!(!state.message.contains("Cannot start installer:"));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn update_banner_opens_the_dialog_and_preserves_the_draft(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.updates.update(cx, |state, cx| {
                    state.prepared = Some(
                        PreparedUpdate::missing_installer_for_test(available_release()).unwrap(),
                    );
                    cx.notify();
                });
                cx.notify();
            });
            window.render_frame(cx);
            window.click("duplicate-item", cx);
            window.click("update-banner-action", cx);
        })
        .unwrap();
        settle_dialog(cx, handle).await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.find("update-install").visible());
            assert!(window.try_find("update-download").is_none());
            assert!(view.read(cx).unsaved(cx));
            assert!(!view.read(cx).updates.read(cx).working);
            assert!(!view.read(cx).updates.read(cx).install_on_close);
            window.click("update-close", cx);
            window.render_frame(cx);
            assert!(window.find("update-banner").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn update_banner_is_hidden_until_preparation_succeeds(cx: &mut TestAppContext) {
        use crate::desktop::updates::{DownloadPhase, SkippedUpdate};

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            let control = view.update(cx, |this, cx| {
                this.updates.update(cx, |state, cx| {
                    let release = available_release();
                    state.available = Some(release.clone());
                    let control = state.begin_download(&release);
                    cx.notify();
                    control
                })
            });
            for phase in [
                DownloadPhase::Connecting,
                DownloadPhase::Downloading,
                DownloadPhase::Verifying,
                DownloadPhase::Saving,
            ] {
                control.set_progress_for_test(phase, 100 * 1024 * 1024);
                view.update(cx, |this, cx| {
                    this.updates.update(cx, |state, cx| {
                        state.refresh_download();
                        cx.notify();
                    });
                    cx.notify();
                });
                window.render_frame(cx);
                assert!(window.try_find("update-banner").is_none());
                assert!(window.try_find("update-install").is_none());
            }
            for error in [
                anyhow::anyhow!("HTTP 503"),
                SkippedUpdate {
                    version: available_release().version,
                }
                .into(),
            ] {
                view.update(cx, |this, cx| {
                    this.updates.update(cx, |state, cx| {
                        state.complete_download(Err(error));
                        assert!(state.available.is_none());
                        assert!(state.prepared.is_none());
                        cx.notify();
                    });
                    cx.notify();
                });
                window.render_frame(cx);
                assert!(window.try_find("update-banner").is_none());
            }
            view.update(cx, |this, cx| {
                this.updates.update(cx, |state, cx| {
                    state.complete_download(Ok(PreparedUpdate::missing_installer_for_test(
                        available_release(),
                    )
                    .unwrap()));
                    cx.notify();
                });
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.find("update-banner").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn checking_automatically_prepares_a_cached_release_before_announcing_it(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        PreparedUpdate::cache_for_test(available_release(), &directory.path().join("updates"))
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.preferences_path = Some(directory.path().join("preferences.json"));
                this.updates.update(cx, |state, _| {
                    state.feed =
                        Some(UpdateFeed::offline_for_test("https://regressify.eshn.dev").unwrap());
                });
                this.check_updates(window, cx);
            });
            window.render_frame(cx);
            assert!(window.try_find("update-banner").is_none());
            assert!(view.read(cx).updates.read(cx).working);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            view.read(cx).updates.read(cx).prepared.is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let state = view.read(cx).updates.read(cx);
            assert!(!state.working);
            assert!(!state.failed);
            assert!(state.message.contains("verified and ready"));
            assert!(window.find("update-banner").visible());
            window.click("update-banner-action", cx);
        })
        .unwrap();
        settle_dialog(cx, handle).await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.find("update-install").visible());
            assert!(window.try_find("update-download").is_none());
            window.click("update-check", cx);
            assert!(view.read(cx).updates.read(cx).working);
            assert!(view.read(cx).updates.read(cx).prepared.is_none());
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            view.read(cx).updates.read(cx).prepared.is_some()
        })
        .await;
    }

    #[gpui_kit::test]
    async fn startup_check_ignores_recent_shared_deadline(cx: &mut TestAppContext) {
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        let cache = directory.path().join("updates");
        PreparedUpdate::cache_for_test(available_release(), &cache).unwrap();
        let state_path = cache.join("state.json");
        let mut saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        let previous_check = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(60);
        saved["last_check_at"] = previous_check.into();
        fs::write(&state_path, serde_json::to_vec(&saved).unwrap()).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.preferences_path = Some(directory.path().join("preferences.json"));
                this.updates.update(cx, |state, _| {
                    state.feed =
                        Some(UpdateFeed::offline_for_test("https://regressify.eshn.dev").unwrap());
                });
                this.check_updates(window, cx);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            view.read(cx).updates.read(cx).prepared.is_some()
        })
        .await;
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert!(saved["last_check_at"].as_u64().unwrap() > previous_check);
        cx.update_window(handle.into(), |_, _, cx| {
            assert!(!view.read(cx).updates.read(cx).startup_check_pending);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn shared_polling_notifies_every_window_and_preserves_ready_banners(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let second_directory = tempfile::tempdir().unwrap();
        let (first_handle, first) = editor(cx, directory.path());
        let (second_handle, second) = editor(cx, second_directory.path());
        let cache = directory.path().join("updates");
        PreparedUpdate::cache_for_test(available_release(), &cache).unwrap();
        for (handle, view) in [(first_handle, &first), (second_handle, &second)] {
            cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.preferences_path = Some(directory.path().join("preferences.json"));
                    this.updates.update(cx, |state, _| {
                        state.feed = Some(
                            UpdateFeed::offline_for_test("https://regressify.eshn.dev").unwrap(),
                        );
                    });
                    this.watch_shared_updates(Duration::from_millis(50), window, cx);
                });
            })
            .unwrap();
        }
        cx.wait_for(first_handle.into(), Duration::from_secs(5), |_, cx| {
            first.read(cx).updates.read(cx).prepared.is_some()
                && second.read(cx).updates.read(cx).prepared.is_some()
        })
        .await;
        for (handle, view) in [(first_handle, &first), (second_handle, &second)] {
            cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, cx| {
                    assert!(this.updates.read(cx).poll_task.is_some());
                    this.start_update_check(false, window, cx);
                    assert!(this.updates.read(cx).prepared.is_some());
                });
                window.render_frame(cx);
                assert!(window.find("update-banner").visible());
            })
            .unwrap();
        }
        assert_eq!(
            std::fs::read_dir(&cache)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().unwrap().is_dir())
                .count(),
            1
        );
    }

    #[gpui_kit::test]
    async fn shared_owner_wait_clears_expired_retry_and_prepares_after_release(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::updates::{CURRENT_VERSION, SharedUpdateCheck};

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        let cache = directory.path().join("updates");
        PreparedUpdate::cache_for_test(available_release(), &cache).unwrap();
        let feed = UpdateFeed::offline_for_test("https://regressify.eshn.dev").unwrap();
        let owner = feed
            .check_shared(CURRENT_VERSION.parse().unwrap(), &cache, false, None)
            .unwrap();
        assert!(matches!(owner, SharedUpdateCheck::Checked(_, _)));
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.preferences_path = Some(directory.path().join("preferences.json"));
                this.updates.update(cx, |state, _| {
                    state.feed = Some(feed);
                    state.complete_download(Err(crate::desktop::updates::RetryScheduled {
                        version: available_release().version,
                        retry_at: 0,
                    }
                    .into()));
                });
                this.watch_shared_updates(Duration::from_millis(50), window, cx);
                this.schedule_update_retry(window, cx);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            view.read(cx)
                .updates
                .read(cx)
                .message
                .starts_with("Another Regressify instance")
        })
        .await;
        cx.update_window(handle.into(), |_, _, cx| {
            let state = view.read(cx).updates.read(cx);
            assert!(state.startup_check_pending);
            assert!(state.retry.is_none());
            assert!(state.retry_task.is_none());
            assert!(!state.failed);
        })
        .unwrap();
        drop(owner);
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            view.read(cx).updates.read(cx).prepared.is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, _, cx| {
            assert!(!view.read(cx).updates.read(cx).startup_check_pending);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn manual_checks_reset_waiting_and_exhausted_retries(cx: &mut TestAppContext) {
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };

        for failures in [1, 4] {
            let directory = tempfile::tempdir().unwrap();
            let (handle, view) = editor(cx, directory.path());
            let cache = directory.path().join("updates");
            PreparedUpdate::cache_for_test(available_release(), &cache).unwrap();
            let state_path = cache.join("state.json");
            let mut saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
            saved["hash_failures"] = failures.into();
            saved["next_retry_at"] = (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 7200)
                .into();
            if failures == 4 {
                saved["skipped_version"] = available_release().version.to_string().into();
            }
            fs::write(&state_path, serde_json::to_vec(&saved).unwrap()).unwrap();
            cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.preferences_path = Some(directory.path().join("preferences.json"));
                    this.updates.update(cx, |state, _| {
                        state.feed = Some(
                            UpdateFeed::offline_for_test("https://regressify.eshn.dev").unwrap(),
                        );
                    });
                    this.check_updates(window, cx);
                });
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
                !view.read(cx).updates.read(cx).working
            })
            .await;
            cx.update_window(handle.into(), |_, window, cx| {
                let state = view.read(cx).updates.read(cx);
                assert!(state.prepared.is_none());
                assert!(!state.failed);
                assert_eq!(state.retry_task.is_some(), failures == 1);
                window.render_frame(cx);
                assert!(window.try_find("update-banner").is_none());
                window.click("software-updates", cx);
            })
            .unwrap();
            settle_dialog(cx, handle).await;
            cx.update_window(handle.into(), |_, window, cx| {
                window.click("update-check", cx);
                let state = view.read(cx).updates.read(cx);
                assert!(state.working);
                assert!(state.retry.is_none());
                assert!(state.retry_task.is_none());
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
                view.read(cx).updates.read(cx).prepared.is_some()
            })
            .await;
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
            assert_eq!(saved["hash_failures"], 0);
            assert!(saved["next_retry_at"].is_null());
            assert!(saved["skipped_version"].is_null());
            cx.update_window(handle.into(), |_, window, cx| {
                assert!(view.read(cx).updates.read(cx).retry_task.is_none());
                window.click("update-close", cx);
                window.render_frame(cx);
                assert!(window.find("update-banner").visible());
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    async fn scheduled_retry_automatically_prepares_the_release_when_due(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        PreparedUpdate::cache_for_test(available_release(), &directory.path().join("updates"))
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.preferences_path = Some(directory.path().join("preferences.json"));
                this.updates.update(cx, |state, _| {
                    state.feed =
                        Some(UpdateFeed::offline_for_test("https://regressify.eshn.dev").unwrap());
                    state.complete_download(Err(crate::desktop::updates::RetryScheduled {
                        version: available_release().version,
                        retry_at: 0,
                    }
                    .into()));
                });
                this.schedule_update_retry(window, cx);
            });
            assert!(view.read(cx).updates.read(cx).prepared.is_none());
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            view.read(cx).updates.read(cx).prepared.is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            let state = view.read(cx).updates.read(cx);
            assert!(state.retry_task.is_none());
            assert!(state.retry.is_none());
            assert!(!state.failed);
            window.render_frame(cx);
            assert!(window.find("update-banner").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn update_progress_refreshes_during_transfer_and_cancel_wins_completion(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::updates::DownloadPhase;

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        let (finished, completion) = futures::channel::oneshot::channel();
        let control = cx
            .update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, cx| {
                    let release = available_release();
                    let control = this.updates.update(cx, |state, cx| {
                        state.available = Some(release.clone());
                        let control = state.begin_download(&release);
                        cx.notify();
                        control
                    });
                    let task = cx.background_executor().spawn(async move {
                        completion
                            .await
                            .unwrap_or_else(|_| Err(anyhow::anyhow!("worker stopped")))
                    });
                    this.observe_update_download(task, control.clone(), window, cx);
                    this.open_updates(window, cx);
                    control
                })
            })
            .unwrap();
        settle_dialog(cx, handle).await;
        control.set_progress_for_test(DownloadPhase::Downloading, 50 * 1024 * 1024);
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            view.read(cx).updates.read(cx).message.contains("50%")
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("update-download-progress").visible());
            assert_eq!(
                window.find("update-download-progress").label(),
                Some("Installer download progress")
            );
            assert!(
                view.read(cx)
                    .updates
                    .read(cx)
                    .message
                    .contains("50.0 / 100.0 MiB")
            );
            window.click("update-close", cx);
            assert!(view.read(cx).updates.read(cx).working);
            assert!(!control.is_cancelled());
        })
        .unwrap();
        for (phase, message) in [
            (DownloadPhase::Verifying, "Verifying installer SHA-256"),
            (DownloadPhase::Saving, "Saving verified installer to disk"),
        ] {
            control.set_progress_for_test(phase, 100 * 1024 * 1024);
            cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
                view.read(cx).updates.read(cx).message.contains(message)
            })
            .await;
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("update-banner").is_none());
            window.click("software-updates", cx);
        })
        .unwrap();
        settle_dialog(cx, handle).await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("update-cancel-download", cx);
            assert!(control.is_cancelled());
        })
        .unwrap();
        assert!(
            finished
                .send(Ok(PreparedUpdate::missing_installer_for_test(
                    available_release()
                )
                .unwrap()))
                .is_ok()
        );
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            !view.read(cx).updates.read(cx).working
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            let state = view.read(cx).updates.read(cx);
            assert!(state.message.contains("Download paused"));
            assert!(!state.failed);
            assert!(state.prepared.is_none());
            assert!(!state.install_on_close);
            window.click("update-close", cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn update_download_pause_check_and_errors_preserve_the_draft(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        let control = cx
            .update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.click("duplicate-item", cx);
                view.update(cx, |this, cx| {
                    this.preferences_path = None;
                    let release = available_release();
                    let control = this.updates.update(cx, |state, cx| {
                        state.feed = Some(UpdateFeed::new("https://regressify.eshn.dev").unwrap());
                        state.available = Some(release.clone());
                        let control = state.begin_download(&release);
                        cx.notify();
                        control
                    });
                    this.open_updates(window, cx);
                    control
                })
            })
            .unwrap();
        settle_dialog(cx, handle).await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.find("update-download-progress").visible());
            assert!(
                view.read(cx)
                    .updates
                    .read(cx)
                    .message
                    .contains("Connecting")
            );
            window.click("update-cancel-download", cx);
            assert!(control.is_cancelled());
            assert!(view.read(cx).updates.read(cx).working);
            assert!(
                view.read(cx)
                    .updates
                    .read(cx)
                    .message
                    .contains("Cancelling")
            );
            view.update(cx, |this, cx| {
                this.updates.update(cx, |state, cx| {
                    state.complete_download(Err(anyhow::anyhow!("cancelled")));
                    assert!(!state.failed);
                    assert!(state.available.is_none());
                    assert!(state.download.is_none());
                    cx.notify();
                });
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.try_find("update-cancel-download").is_none());
            assert!(window.try_find("update-download").is_none());
            window.click("update-check", cx);
            assert!(view.read(cx).updates.read(cx).working);
            assert!(!view.read(cx).updates.read(cx).failed);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            !view.read(cx).updates.read(cx).working
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let state = view.read(cx).updates.read(cx);
            assert!(state.failed);
            assert!(state.message.contains("Cannot locate the update cache"));
            assert!(state.prepared.is_none());
            assert!(window.find("update-copy-details").visible());
            window.click("update-close", cx);
            window.render_frame(cx);
            assert!(window.try_find("update-banner").is_none());
            assert!(view.read(cx).unsaved(cx));
            assert!(!view.read(cx).updates.read(cx).install_on_close);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn update_failure_details_fit_compact_and_large_windows(cx: &mut TestAppContext) {
        for (width, height) in [(1000., 640.), (1600., 1000.)] {
            for dark in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let (handle, view) =
                    editor_at_size(cx, directory.path(), size(px(width), px(height)));
                cx.update_window(handle.into(), |_, window, cx| {
                    design::apply_theme(dark, Some(window), cx);
                    view.update(cx, |this, cx| {
                        this.updates.update(cx, |state, cx| {
                            let release = available_release();
                            state.available = Some(release.clone());
                            state.begin_download(&release);
                            state.complete_download(Err(anyhow::anyhow!(
                                "Download stalled: no data received for 30 seconds (12345678 of 987654321 bytes). Check your connection, VPN or proxy, then retry"
                            )));
                            cx.notify();
                        });
                        this.open_updates(window, cx);
                        cx.notify();
                    });
                }).unwrap();
                settle_dialog(cx, handle).await;
                cx.update_window(handle.into(), |_, window, cx| {
                    assert!(window.try_find("update-banner").is_none());
                    assert!(window.try_find("update-install").is_none());
                    for id in [
                        "update-status",
                        "update-check",
                        "update-copy-details",
                        "update-close",
                    ] {
                        let bounds = window.find(id).bounds();
                        assert!(bounds.origin.x >= px(0.));
                        assert!(bounds.origin.x + bounds.size.width <= px(width));
                        assert!(bounds.origin.y + bounds.size.height <= px(height));
                    }
                    let message = view.read(cx).updates.read(cx).message.clone();
                    window.click("update-copy-details", cx);
                    assert_eq!(
                        cx.read_from_clipboard().unwrap().text().as_deref(),
                        Some(message.as_str())
                    );
                    let changed = format!("{message}\r\nAdditional failure detail");
                    view.read(cx).updates.clone().update(cx, |state, cx| {
                        state.message = changed.clone();
                        cx.notify();
                    });
                    window.click("update-copy-details", cx);
                    assert_eq!(
                        cx.read_from_clipboard().unwrap().text().as_deref(),
                        Some(changed.as_str())
                    );
                    window.click("update-close", cx);
                })
                .unwrap();
            }
        }
    }

    #[gpui_kit::test]
    fn update_banner_stays_visible_across_views_themes_and_sizes(cx: &mut TestAppContext) {
        for (width, height) in [(1000., 640.), (1600., 1000.)] {
            let directory = tempfile::tempdir().unwrap();
            let (handle, view) = editor_at_size(cx, directory.path(), size(px(width), px(height)));
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(window.try_find("update-banner").is_none());
                view.update(cx, |this, cx| {
                    this.updates.update(cx, |state, cx| {
                        state.prepared = Some(
                            PreparedUpdate::missing_installer_for_test(available_release())
                                .unwrap(),
                        );
                        cx.notify();
                    });
                    cx.notify();
                });
                for dark in [false, true] {
                    design::apply_theme(dark, Some(window), cx);
                    let colors = super::update_banner_colors(cx);
                    assert!(
                        design::contrast_ratio(colors.title, colors.background) >= 4.5,
                        "banner title contrast with dark={dark}: {colors:?}"
                    );
                    assert!(
                        design::contrast_ratio(colors.detail, colors.background) >= 4.5,
                        "banner detail contrast with dark={dark}: {colors:?}"
                    );
                    for (state, button_background) in [
                        ("normal", colors.action_background),
                        ("hover", colors.action_hover),
                        ("active", colors.action_active),
                    ] {
                        let button_background = colors.background.blend(button_background);
                        assert!(
                            design::contrast_ratio(colors.action_foreground, button_background)
                                >= 4.5,
                            "banner action {state} text contrast with dark={dark}: {colors:?}"
                        );
                        assert!(
                            design::contrast_ratio(button_background, colors.background) >= 4.5,
                            "banner action {state} surface contrast with dark={dark}: {colors:?}"
                        );
                    }
                    for main_tab in 0..4 {
                        view.update(cx, |this, cx| {
                            this.main_tab = main_tab;
                            cx.notify();
                        });
                        window.render_frame(cx);
                        assert!(window.find("update-banner").visible());
                        assert!(window.find("update-banner-action").visible());
                        let banner = window.find("update-banner").bounds();
                        let title = window.find("update-banner-title").bounds();
                        let action = window.find("update-banner-action").bounds();
                        assert_eq!(banner.size.width, px(width));
                        assert!(banner.origin.y >= px(38.));
                        assert!(banner.size.height >= px(64.));
                        assert!(banner.size.height <= px(100.));
                        assert!(title.origin.x + title.size.width <= action.origin.x);
                        assert!(action.origin.x + action.size.width <= px(width));
                        assert!(action.origin.y >= banner.origin.y);
                        assert!(
                            action.origin.y + action.size.height
                                <= banner.origin.y + banner.size.height
                        );
                    }
                }
                view.update(cx, |this, cx| {
                    this.updates.update(cx, |state, cx| {
                        state.prepared = None;
                        cx.notify();
                    });
                    cx.notify();
                });
                window.render_frame(cx);
                assert!(window.try_find("update-banner").is_none());
            })
            .unwrap();
        }
    }
}
