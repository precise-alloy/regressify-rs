use super::*;
use crate::desktop::updates::{
    CHECK_FAILED_PREFIX, CURRENT_VERSION, DownloadControl, InstallHandle, InstallState,
    NoticeAttempt, NoticeOutcome, PreparedUpdate, Release, RetryScheduled, SHARED_POLL_INTERVAL,
    SharedUpdateCheck, SkippedUpdate, UpdateCheck, UpdateFeed, UpdateLease, UpdateNotice,
    UpdatePlatform, Version, dismiss_version, dismissal_hides, install_is_stale,
    install_macos_update, install_scope, install_status, install_title, macos_install,
    notice_message_after_success, notice_network_allowed, running_version_label, unix_timestamp,
};
use futures::FutureExt;

/// Starts the replacement app (app bundle, workspace) and reports whether it launched.
/// Injected so GUI tests never open a real application.
pub(super) type Relauncher =
    std::sync::Arc<dyn Fn(&std::path::Path, Option<&std::path::Path>) -> bool + Send + Sync>;

/// `-n` opens a fresh instance. Bounded so a stalled `/usr/bin/open` ends in the failure path
/// (which reclaims the workspace) instead of leaving the window busy.
fn open_new_instance(app: &std::path::Path, workspace: Option<&std::path::Path>) -> bool {
    crate::desktop::updates::open_new_instance_bounded(
        std::path::Path::new("/usr/bin/open"),
        app,
        workspace,
        crate::desktop::updates::RELAUNCH_TIMEOUT,
    )
}

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
    /// A macOS relaunch is in flight (off the UI thread); a second Restart must not start another.
    restarting: bool,
    relaunch: Relauncher,
    /// Notify-only feed (macOS). Never shares state with the installer flow above.
    notice_feed: Option<UpdateFeed>,
    notice: Option<UpdateNotice>,
    /// The newest release the user dismissed. Only hides the banner: the dialog keeps offering
    /// Update now / Copy command for it, since its status points there.
    dismissed_notice: Option<UpdateNotice>,
    notice_checking: bool,
    notice_startup_pending: bool,
    notice_task: Option<Task<()>>,
    /// Newest version dismissed in this window, recorded synchronously so a check already in
    /// flight (which loaded its state before the dismissal) cannot restore the banner.
    notice_dismissed: Option<Version>,
    /// In-memory throttle (unix seconds, wall clock): holds the interval even when `notice.json`
    /// cannot be saved, and keeps counting while the Mac sleeps.
    notice_attempted_at: Option<u64>,
    /// Last check result for the dialog (up to date / failed); the banner stays silent.
    notice_message: String,
    /// The running app's `Regressify.app`, set only when its folder is writable. Without it
    /// macOS keeps the copy-the-command flow and shows no "Update now".
    install_bundle: Option<std::path::PathBuf>,
    /// Progress of an in-app install; shared with the worker thread.
    install: Option<InstallHandle>,
    /// The release `install` was started for (the clicked notice). The install row is scoped to
    /// the higher of this and the release the plan actually installed: only a strictly newer
    /// announced release retires a finished or failed row, an older or equal one keeps it.
    install_for: Option<Version>,
}

impl Default for UpdatePanel {
    fn default() -> Self {
        let configured = UpdateFeed::configured();
        let notice_feed = configured
            .clone()
            .filter(|feed| feed.platform() == UpdatePlatform::MacOs);
        let install_bundle = notice_feed
            .as_ref()
            .and_then(|_| macos_install::installable_bundle());
        // The download/install machinery stays Windows-only.
        let feed = configured.filter(|feed| feed.platform() == UpdatePlatform::WindowsX64);
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
            restarting: false,
            relaunch: std::sync::Arc::new(open_new_instance),
            notice_feed,
            notice: None,
            dismissed_notice: None,
            notice_checking: false,
            notice_startup_pending: true,
            notice_task: None,
            notice_dismissed: None,
            notice_attempted_at: None,
            notice_message: "Ready to check for updates".into(),
            install_bundle,
            install: None,
            install_for: None,
        }
    }
}

impl UpdatePanel {
    /// Dialog status on macOS, from the same state the banner reads.
    fn mac_status(&self) -> String {
        if let Some(install) = &self.install {
            let state = install.get();
            return match self.install_version() {
                Some(version) => install_status(version, &state),
                None => state.message(),
            };
        }
        // Check progress and errors show even while a release notice is retained.
        if self.notice_checking {
            return "Checking for updates...".into();
        }
        if let Some(notice) = &self.notice {
            let available = format!("Regressify {} is available", notice.version.display_short());
            return if self.notice_message.starts_with(CHECK_FAILED_PREFIX) {
                format!("{available}. {}", self.notice_message)
            } else {
                available
            };
        }
        self.notice_message.clone()
    }

    /// The release the install row is about: the plan's target once known, else the clicked
    /// notice. The cached notice can announce a different release than the one installed.
    fn install_version(&self) -> Option<Version> {
        self.install
            .as_ref()
            .and_then(InstallHandle::target)
            .or(self.install_for)
    }

    fn install_state(&self) -> Option<InstallState> {
        self.install.as_ref().map(InstallHandle::get)
    }

    /// New app to relaunch once an install finished.
    fn mac_restart_app(&self) -> Option<std::path::PathBuf> {
        matches!(self.install_state(), Some(InstallState::Restart))
            .then(|| self.install_bundle.clone())
            .flatten()
    }

    /// Applies one finished notice check. Split out so tests can feed a result without a request.
    fn apply_notice_attempt(&mut self, attempt: NoticeAttempt, cx: &mut Context<Self>) {
        let NoticeAttempt {
            dispatched,
            outcome: result,
        } = attempt;
        self.notice_checking = false;
        // A release newer than the install scope retires a finished or failed install row; an older or
        // equal (e.g. cached) announcement keeps its Restart/Failed state.
        if let Ok(NoticeOutcome::Available(notice)) = &result
            && let Some(state) = self.install_state()
            && install_is_stale(
                &state,
                // The higher of the installed release (fresh plan) and the clicked notice.
                install_scope(
                    self.install.as_ref().and_then(InstallHandle::target),
                    self.install_for,
                ),
                notice.version,
            )
        {
            self.install = None;
            self.install_for = None;
        }
        // An install in progress (or finished, awaiting Restart, or failed) renders from the
        // notice: keep it until the install row is gone, whatever a peer or a check reports.
        let installing = self.install_state().is_some();
        // Only a sent request (failures included) restarts the throttle; cached polls
        // must not, or the deadline never arrives.
        if dispatched {
            self.notice_attempted_at = Some(unix_timestamp());
        }
        match result {
            Ok(NoticeOutcome::NotDue) => {}
            // A stale result must not restore a banner dismissed meanwhile.
            Ok(NoticeOutcome::Available(notice))
                if dismissal_hides(self.notice_dismissed, notice.version) =>
            {
                if !installing {
                    self.notice = None;
                }
                self.dismissed_notice = Some(notice);
                // A cached poll must not erase the failure of the manual check that preceded it.
                self.notice_message = notice_message_after_success(
                    dispatched,
                    &self.notice_message,
                    DISMISSED_MESSAGE,
                );
            }
            Ok(NoticeOutcome::Available(notice)) => {
                self.dismissed_notice = None;
                self.notice = Some(notice);
                // Only a cached poll keeps an earlier failure; any other old
                // message would contradict the available release.
                self.notice_message =
                    notice_message_after_success(dispatched, &self.notice_message, "");
            }
            Ok(NoticeOutcome::Current) => {
                // Only a running or finished install keeps its notice; a failed one must not
                // mask the check outcome, so it goes with the notice.
                if self
                    .install_state()
                    .is_some_and(|s| !s.in_progress() && !matches!(s, InstallState::Restart))
                {
                    self.install = None;
                    self.install_for = None;
                }
                if self.install_state().is_none() {
                    self.notice = None;
                }
                self.dismissed_notice = None;
                self.notice_message = notice_message_after_success(
                    dispatched,
                    &self.notice_message,
                    "Regressify is up to date",
                );
            }
            Ok(NoticeOutcome::Dismissed(notice)) => {
                if !installing {
                    self.notice = None;
                }
                self.dismissed_notice = Some(notice);
                // A cached poll must not erase the failure of the manual check that preceded it.
                self.notice_message = notice_message_after_success(
                    dispatched,
                    &self.notice_message,
                    DISMISSED_MESSAGE,
                );
            }
            // Silent in the banner; the dialog shows it and the next check retries.
            Err(error) => {
                self.notice_message = format!("{CHECK_FAILED_PREFIX}{error:#}");
                crate::diagnostics::startup::warning(
                    crate::diagnostics::record::SafeValue::Desktop,
                    &error,
                )
            }
        }
        cx.notify();
    }

    /// Seeds a macOS notice (no network, throttled) for GUI tests. With `relaunch` the install
    /// is finished and the banner offers Restart through that non-launching callback.
    #[cfg(all(test, feature = "gui-tests"))]
    pub(super) fn seed_macos(&mut self, relaunch: Option<Relauncher>) {
        let feed =
            UpdateFeed::for_origin(Some("https://regressify.eshn.dev"), UpdatePlatform::MacOs)
                .unwrap();
        self.notice = Some(
            feed.parse_notice_manifest("version=9999.9999.9999.9999\ntag=v9999.9999.9999.9999\n")
                .unwrap(),
        );
        self.notice_feed = Some(feed);
        self.notice_startup_pending = false;
        self.notice_attempted_at = Some(unix_timestamp());
        if let Some(relaunch) = relaunch {
            self.install_bundle = Some(std::path::PathBuf::from("/nonexistent/Regressify.app"));
            self.install = Some(InstallHandle::restarted());
            self.install_for = self.notice.as_ref().map(|notice| notice.version);
            self.relaunch = relaunch;
        }
    }

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

/// Prefix of the dialog message after a failed notice check (also tells `mac_status` to append it).
const DISMISSED_MESSAGE: &str = "A newer version is available, but its notice was dismissed. It shows again when a newer release is published.";

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

    pub(super) fn render_update_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Same guard as `request_macos_restart` / `start_macos_install`, or the buttons look
        // enabled while a click silently does nothing.
        let install_blocked = self.busy || self.runs.read(cx).running();
        let update = self.updates.read(cx);
        let notice = update.notice.as_ref()?;
        let feed = update.notice_feed.as_ref()?;
        let install_state = update.install_state();
        let shown = update.install_version().unwrap_or(notice.version);
        let title = install_title(shown, install_state.as_ref());
        let can_install = update.install_bundle.is_some();
        let failed = matches!(install_state, Some(InstallState::Failed(_)));
        let restart = matches!(install_state, Some(InstallState::Restart));
        let installing = install_state
            .as_ref()
            .is_some_and(InstallState::in_progress);
        let detail = match &install_state {
            Some(state) => state.message(),
            None if can_install => {
                "Update now downloads, verifies and installs it; you restart when it is done."
                    .to_owned()
            }
            None => "Run the install command in a terminal to update, or review the release notes."
                .to_owned(),
        };
        // Without a replaceable app bundle the command is the only path, and after a failed
        // install it is the fallback.
        let show_command = !can_install || failed;
        let command = feed.install_command();
        let copy_command = command.clone();
        let release_url = notice.release_url();
        // Dismiss what the install row displays, or the clicked release if that is higher, so
        // the same release does not return as a Restart banner at the next check.
        let version = dismiss_version(shown, notice.version);
        // The release page belongs to the clicked notice; the fresh plan's tag is not retained,
        // so the link is hidden while the row names a different release.
        let release_matches = shown == notice.version;
        let colors = update_banner_colors(cx);
        Some(
            div()
                .id("update-notice")
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
                        .flex_wrap()
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
                                        .id("update-notice-title")
                                        .test_support()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .id("update-notice-detail")
                                        .test_support()
                                        .text_xs()
                                        .text_color(colors.detail)
                                        .whitespace_normal()
                                        .child(detail),
                                )
                                .when(show_command, |column| {
                                    column.child(
                                        // Shown so the banner is self-explanatory; wraps instead of clipping on narrow windows.
                                        div()
                                            .id("update-notice-command")
                                            .test_support()
                                            .text_xs()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_color(colors.title)
                                            .whitespace_normal()
                                            .child(command),
                                    )
                                }),
                        )
                        .when(can_install && !installing && !restart, |row| {
                            row.child(
                                Button::new("update-notice-install")
                                    .primary()
                                    .flex_shrink_0()
                                    .icon(gpui_kit::assets::IconName::Download)
                                    .label("Update now")
                                    .disabled(install_blocked)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.start_macos_install(window, cx)
                                    })),
                            )
                        })
                        .when(restart, |row| {
                            row.child(
                                Button::new("update-notice-restart")
                                    .primary()
                                    .flex_shrink_0()
                                    .icon(gpui_kit::assets::IconName::RotateCw)
                                    .label("Restart")
                                    .disabled(install_blocked)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.request_macos_restart(window, cx)
                                    })),
                            )
                        })
                        .when(show_command && !installing && !restart, |row| {
                            row.child(
                                // `CopyText` is icon-only; the banner needs a visible instruction.
                                Button::new("update-notice-copy")
                                    .flex_shrink_0()
                                    .icon(gpui_kit::assets::IconName::Copy)
                                    .label("Copy command")
                                    .tooltip("Copy install command")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_command.clone(),
                                        ));
                                    }),
                            )
                        })
                        .when(release_matches, |row| {
                            row.child(
                                Button::new("update-notice-release")
                                    .flex_shrink_0()
                                    .icon(gpui_kit::assets::IconName::ExternalLink)
                                    .label("View release")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Err(error) = open::that_detached(&release_url) {
                                            this.status =
                                                format!("Cannot open release page: {error}");
                                            cx.notify();
                                        }
                                    })),
                            )
                        })
                        .when(!installing, |row| {
                            row.child(
                                Button::new("update-notice-dismiss")
                                    .ghost()
                                    .flex_shrink_0()
                                    .icon(gpui_kit::assets::IconName::X)
                                    .accessibility_label("Dismiss update notice")
                                    .tooltip("Dismiss until a newer version")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.dismiss_update_notice(version, cx)
                                    })),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    fn dismiss_update_notice(&mut self, version: Version, cx: &mut Context<Self>) {
        self.updates.update(cx, |state, cx| {
            state.dismissed_notice = state.notice.take().or(state.dismissed_notice.take());
            state.notice_message = DISMISSED_MESSAGE.into();
            state.notice_dismissed = state.notice_dismissed.max(Some(version));
            cx.notify();
        });
        // Persist off the UI thread; a failed write only means the notice may return next check.
        if let Some(cache) = self.update_cache_dir() {
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = UpdateFeed::dismiss_notice(version, &cache) {
                        crate::diagnostics::startup::warning(
                            crate::diagnostics::record::SafeValue::Desktop,
                            &error,
                        );
                    }
                })
                .detach();
        }
        cx.notify();
    }

    /// "Update now": download, verify, stage and swap off the UI thread. Nothing runs before
    /// this click, and the running app is untouched until the final atomic swap.
    fn start_macos_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let run_active = self.runs.read(cx).running();
        let state = self.updates.read(cx);
        if self.busy
            || run_active
            || (state.notice.is_none() && state.dismissed_notice.is_none())
            || state
                .install_state()
                .is_some_and(|s| matches!(s, InstallState::Restart) || s.in_progress())
        {
            return;
        }
        let (Some(feed), Some(bundle), Some(cache)) = (
            state.notice_feed.clone(),
            state.install_bundle.clone(),
            self.update_cache_dir(),
        ) else {
            return;
        };
        let handle = InstallHandle::new();
        let worker = handle.clone();
        self.updates.update(cx, |state, cx| {
            state.install = Some(handle);
            state.install_for = state
                .notice
                .as_ref()
                .or(state.dismissed_notice.as_ref())
                .map(|notice| notice.version);
            // Started from the dialog after the banner was dismissed: bring the banner back so
            // progress, Failed and Restart stay visible once the dialog closes.
            if state.notice.is_none() {
                state.notice = state.dismissed_notice.clone();
            }
            cx.notify();
        });
        cx.background_executor()
            .spawn(async move {
                // The outcome is published through the handle.
                let _ = install_macos_update(&feed, &bundle, &cache, CURRENT_VERSION, &worker);
            })
            .detach();
        // Re-render while the worker runs; stops once it reaches Restart or Failed.
        cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let running = view.update_in(cx, |this, _, cx| {
                    let running = this
                        .updates
                        .read(cx)
                        .install_state()
                        .is_some_and(|state| state.in_progress());
                    this.updates.update(cx, |_, cx| cx.notify());
                    cx.notify();
                    running
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// "Restart": same unsaved-work guard as quitting, then relaunch from `quit_or_install`.
    fn request_macos_restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self.runs.read(cx).running()
            || self.updates.read(cx).mac_restart_app().is_none()
        {
            return;
        }
        if self.session.open.is_some()
            && let Err(error) = self.flush_drafts(window, cx)
        {
            // An invalid draft counts as unsaved work: continue into the guard like native
            // close does (Save disabled, Discard available) instead of refusing silently.
            if !self.unsaved(cx) {
                self.status = format!("Cannot restart to update: {error:#}");
                cx.notify();
                return;
            }
            self.sync_modal(cx);
        }
        // Same landing-editor guard as native close, but carrying the restart: after
        // Save/Discard it continues into `Pending::Update` (relaunch), and Keep editing
        // cancels it with nothing changed.
        if self.guard_landing_restart(window, cx) {
            return;
        }
        window.close_dialog(cx);
        self.guard(files::Pending::Update, window, cx);
    }

    /// True while a macOS install owns the app bundle; quitting then could kill it mid-swap.
    pub(super) fn install_blocks_quit(&mut self, cx: &mut Context<Self>) -> bool {
        let running = self
            .updates
            .read(cx)
            .install_state()
            .is_some_and(|state| state.in_progress());
        if running {
            self.status = "An update is installing; close again when it finishes".into();
            cx.notify();
        }
        running
    }

    fn update_cache_dir(&self) -> Option<std::path::PathBuf> {
        self.preferences_path
            .as_ref()
            .map(|path| path.with_file_name("updates"))
    }

    fn start_notice_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_notice_check(false, window, cx);
    }

    /// `force` is the dialog's "Check for updates": it skips only the eight-hour deadline,
    /// like the startup check, and shares the banner's state and request path.
    fn run_notice_check(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.updates.read(cx);
        if state.notice_checking {
            return;
        }
        let Some(feed) = state.notice_feed.clone() else {
            return;
        };
        let startup = state.notice_startup_pending || force;
        let network_allowed =
            notice_network_allowed(startup, state.notice_attempted_at, unix_timestamp());
        let Some(cache) = self.update_cache_dir() else {
            // Say so instead of leaving "Ready to check" next to a button that does nothing.
            self.updates.update(cx, |state, cx| {
                state.notice_message =
                    format!("{CHECK_FAILED_PREFIX}the update cache location is unavailable");
                cx.notify();
            });
            window.refresh();
            return;
        };
        self.updates.update(cx, |state, _| {
            // Only a check that may reach the network blocks other checks and shows
            // "Checking...": a not-due poll just reads `notice.json`, and must not drop a manual
            // check clicked meanwhile.
            if network_allowed {
                state.notice_checking = true;
            }
            state.notice_startup_pending = false;
            // A manual check after a failed install starts fresh; Failed would mask its result.
            if force && matches!(state.install_state(), Some(InstallState::Failed(_))) {
                state.install = None;
                state.install_for = None;
            }
        });
        if network_allowed {
            window.refresh();
        }
        let task = cx.background_executor().spawn(async move {
            match CURRENT_VERSION.parse() {
                Ok(current) => feed.check_notice_shared(current, &cache, startup, network_allowed),
                Err(error) => NoticeAttempt {
                    dispatched: false,
                    outcome: Err(error),
                },
            }
        });
        cx.spawn_in(window, async move |view, cx| {
            let attempt = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.updates.update(cx, |state, cx| {
                    let checking = state.notice_checking;
                    state.apply_notice_attempt(attempt, cx);
                    // A not-due poll never set the flag, so it must not clear the one a
                    // network check in flight owns.
                    if !network_allowed {
                        state.notice_checking = checking;
                    }
                });
                cx.notify();
                window.refresh();
            });
        })
        .detach();
    }

    fn watch_update_notices(
        &mut self,
        interval: std::time::Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.updates.read(cx);
        if state.notice_feed.is_none() || state.notice_task.is_some() {
            return;
        }
        let task = cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                if view
                    .update_in(cx, |this, window, cx| this.start_notice_check(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.updates
            .update(cx, |state, _| state.notice_task = Some(task));
    }

    pub(super) fn open_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let updates = self.updates.clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            // Read on every render: a run can start or finish while the dialog is open.
            let install_blocked = view.read(cx).busy || view.read(cx).runs.read(cx).running();
            let state = updates.read(cx);
            let working = state.working;
            let prepared = state.prepared.is_some();
            // macOS shows the banner's notice state; Windows keeps its installer state.
            let mac = state.notice_feed.is_some();
            let status_text = if mac {
                state.mac_status()
            } else {
                state.message.clone()
            };
            let install_state = state.install_state();
            let install_running = install_state
                .as_ref()
                .is_some_and(InstallState::in_progress);
            // A dismissed notice only hides the banner; the dialog still has its release.
            let release = state.notice.as_ref().or(state.dismissed_notice.as_ref());
            let can_install = mac && state.install_bundle.is_some() && release.is_some();
            let offer_install = can_install
                && !install_running
                && !matches!(install_state, Some(InstallState::Restart));
            let restart = mac && matches!(install_state, Some(InstallState::Restart));
            let copy_command = state
                .notice_feed
                .as_ref()
                .zip(release)
                .filter(|_| {
                    state.install_bundle.is_none()
                        || matches!(install_state, Some(InstallState::Failed(_)))
                })
                .map(|(feed, _)| feed.install_command());
            dialog
                .title("Software updates")
                .child(format!(
                    "Regressify Desktop {}",
                    running_version_label(mac, CURRENT_VERSION)
                ))
                .child(
                    div()
                        .id("update-status")
                        .role(Role::Status)
                        .aria_label(status_text.clone())
                        .test_support()
                        .text_sm()
                        .w_full()
                        .whitespace_normal()
                        .child(status_text),
                )
                .when_some(copy_command.clone(), |dialog, command| {
                    dialog.child(
                        div()
                            .id("update-dialog-command")
                            .test_support()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .w_full()
                            .whitespace_normal()
                            .child(command),
                    )
                })
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
                                .disabled(if mac {
                                    state.notice_checking || install_running
                                } else {
                                    state.feed.is_none() || working
                                })
                                .on_click(window.listener_for(
                                    &view,
                                    move |this, _, window, cx| {
                                        if mac {
                                            this.run_notice_check(true, window, cx);
                                        } else {
                                            this.start_update_check(true, window, cx);
                                        }
                                    },
                                )),
                        )
                        .when(offer_install, |footer| {
                            footer.child(
                                Button::new("update-now")
                                    .primary()
                                    .icon(gpui_kit::assets::IconName::Download)
                                    .label("Update now")
                                    .disabled(install_blocked)
                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                        this.start_macos_install(window, cx);
                                    })),
                            )
                        })
                        .when(restart, |footer| {
                            footer.child(
                                Button::new("update-restart")
                                    .primary()
                                    .icon(gpui_kit::assets::IconName::RotateCw)
                                    .label("Restart")
                                    .disabled(install_blocked)
                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                        this.request_macos_restart(window, cx);
                                    })),
                            )
                        })
                        .when_some(copy_command, |footer, command| {
                            footer.child(
                                Button::new("update-copy-command")
                                    .icon(gpui_kit::assets::IconName::Copy)
                                    .label("Copy command")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            command.clone(),
                                        ));
                                    }),
                            )
                        })
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
        self.start_notice_check(window, cx);
        self.watch_update_notices(SHARED_POLL_INTERVAL, window, cx);
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
            || (self.updates.read(cx).prepared.is_none()
                && self.updates.read(cx).mac_restart_app().is_none())
        {
            return;
        }
        self.updates
            .update(cx, |state, _| state.install_on_close = true);
        self.finish_close(cx);
    }

    /// Re-takes the activation receiver of a reclaimed instance (the dropped one closed its
    /// channel), so a second open of the workspace foregrounds this window again. No window is
    /// in scope at the restart handoff, so this activates the app rather than one window.
    fn watch_activation(&mut self, instance: &mut DesktopInstance, cx: &mut Context<Self>) {
        self.activation_task = instance.activation_events().map(|mut events| {
            cx.spawn(async move |view, cx| {
                while events.next().await.is_some() {
                    if view
                        .update(cx, |this, cx| {
                            if this
                                .workspace_instance
                                .as_ref()
                                .is_some_and(DesktopInstance::take_activation)
                            {
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
    }

    pub(super) fn quit_or_install(&mut self, cx: &mut Context<Self>) {
        if self.install_blocks_quit(cx) {
            return;
        }
        // macOS: the new app is already in place; relaunch it, then quit this instance.
        if self.updates.read(cx).install_on_close
            && let Some(app) = self.updates.read(cx).mac_restart_app()
        {
            self.updates.update(cx, |state, _| {
                state.install_on_close = false;
            });
            if self.updates.read(cx).restarting {
                return;
            }
            self.updates.update(cx, |state, _| state.restarting = true);
            // Hand the workspace over: the new instance only opens it once this window no
            // longer owns it (otherwise it would merely activate this one), so release the
            // ownership first and take it back if the launch fails.
            let workspace = self
                .session
                .workspace
                .as_ref()
                .map(|workspace| workspace.root.clone());
            let directory = self
                .preferences_path
                .clone()
                .map(Ok)
                .unwrap_or_else(Preferences::path)
                .map(|path| path.with_file_name("instances"));
            let released = workspace
                .as_ref()
                .and_then(|_| self.workspace_instance.take());
            // Unsaved-work guards already passed, so freeze editing and new operations for the
            // whole handoff: an edit made while `open` waits would be dropped by `cx.quit()`.
            // Cleared again if the launch fails.
            self.busy = true;
            self.status = "Restarting...".into();
            // `busy` alone does not reach the retained panels (Git's Fetch, Results, Runs):
            // push it to them before ownership is released, or they could start a workspace
            // operation during the handoff.
            self.sync_modal(cx);
            cx.notify();
            // `/usr/bin/open` can stall in LaunchServices: wait for it off the UI thread.
            let relaunch = self.updates.read(cx).relaunch.clone();
            let had_workspace = workspace.is_some();
            let task = cx.background_executor().spawn(async move {
                // Release the instance lock and activation listener before `open`: a replacement
                // that opens the workspace early must find it unowned, not activate this window
                // and exit (which would leave both instances gone once this one quits).
                drop(released);
                if relaunch(&app, workspace.as_deref()) {
                    return (true, None, None);
                }
                // Launch failed: this window keeps running, so reclaim the workspace.
                // Keep why it failed: a filesystem error has no other owner to blame.
                let (reclaimed, reason) = match (&workspace, directory) {
                    (Some(root), Ok(directory)) => match DesktopInstance::open(&directory, root) {
                        Ok(OpenInstance::Owned(instance)) => (Some(instance), None),
                        Ok(OpenInstance::Activated) => (
                            None,
                            Some("it is now open in another Regressify window".to_owned()),
                        ),
                        Err(error) => (None, Some(format!("{error:#}"))),
                    },
                    (Some(_), Err(error)) => (None, Some(format!("{error:#}"))),
                    (None, _) => (None, None),
                };
                (false, reclaimed, reason)
            });
            cx.spawn(async move |view, cx| {
                let (launched, reclaimed, reason) = task.await;
                let _ = view.update(cx, |this, cx| {
                    this.updates.update(cx, |state, _| state.restarting = false);
                    if launched {
                        cx.quit();
                        return;
                    }
                    if let Some(mut instance) = reclaimed {
                        // The dropped instance closed its activation channel: re-take the
                        // receiver of the reclaimed one, as the initial open does, so a second
                        // open of this workspace foregrounds this window again.
                        this.watch_activation(&mut instance, cx);
                        this.workspace_instance = Some(instance);
                        this.busy = false;
                    } else if had_workspace {
                        // Another process took the workspace while `open` was failing: keep
                        // this window frozen (still `busy`) so two windows never edit one
                        // workspace, but closable: `workspace_lost` bypasses the busy close guard.
                        this.workspace_lost = true;
                        this.status = format!(
                            "Cannot reopen this workspace after the failed restart ({}); close this window",
                            reason.as_deref().unwrap_or("it is owned elsewhere")
                        );
                        cx.notify();
                        return;
                    } else {
                        this.busy = false;
                    }
                    this.status = "Cannot restart the updated app; open Regressify manually".into();
                    // The save task synced the panels while `busy` was set; resync now that it
                    // is cleared so Activity/Results/Git are usable again.
                    this.sync_modal(cx);
                    cx.notify();
                });
            })
            .detach();
            return;
        }
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

    use gpui_kit::component::WindowExt;
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use gpui_kit::{AppContext, TestAppContext, px, size};

    use super::{PreparedUpdate, UpdateFeed};
    use crate::desktop::ui::{
        design,
        tests::{editor, editor_at_size, settle_dialog},
    };
    use crate::desktop::updates::{InstallHandle, Release};

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
    async fn macos_notice_banner_copies_the_install_command(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.updates
                    .update(cx, |updates, _| updates.seed_macos(None));
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.find("update-notice").visible());
            // No replaceable bundle: the command is the only path and is shown.
            assert!(window.try_find("update-notice-install").is_none());
            let command = view
                .read(cx)
                .updates
                .read(cx)
                .notice_feed
                .as_ref()
                .unwrap()
                .install_command();
            window.click("update-notice-copy", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(command.as_str())
            );
            // Copying neither dismisses the banner nor changes the update state.
            window.render_frame(cx);
            assert!(window.find("update-notice").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn macos_notice_banner_dismisses_and_keeps_the_dialog_offer(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.updates
                    .update(cx, |updates, _| updates.seed_macos(None));
                cx.notify();
            });
            window.render_frame(cx);
            window.click("update-notice-dismiss", cx);
            window.render_frame(cx);
            assert!(window.try_find("update-notice").is_none());
            let updates = view.read(cx).updates.read(cx);
            assert!(updates.notice.is_none() && updates.dismissed_notice.is_some());
            assert!(updates.notice_dismissed.is_some());
            assert_eq!(updates.notice_message, super::DISMISSED_MESSAGE);
        })
        .unwrap();
        // The dialog keeps the dismissed release's offer: with no writable app bundle that is
        // the copyable install command, never an in-app install button.
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| this.open_updates(window, cx));
        })
        .unwrap();
        settle_dialog(cx, handle).await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("update-dialog-command").visible());
            assert!(window.try_find("update-install").is_none());
            assert!(window.try_find("update-now").is_none());
            assert!(window.find("update-check").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn dismissing_during_a_check_keeps_the_banner_hidden_for_that_result(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::updates::{NoticeAttempt, NoticeOutcome};

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            let notice = view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.seed_macos(None);
                    // A check is in flight; its result is fed in below without any request.
                    updates.notice_checking = true;
                    updates.notice.clone().unwrap()
                })
            });
            window.render_frame(cx);
            window.click("update-notice-dismiss", cx);
            window.render_frame(cx);
            assert!(window.try_find("update-notice").is_none());
            // The in-flight check had loaded its state before the dismissal.
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, cx| {
                    updates.apply_notice_attempt(
                        NoticeAttempt {
                            dispatched: true,
                            outcome: Ok(NoticeOutcome::Available(notice)),
                        },
                        cx,
                    )
                })
            });
            window.render_frame(cx);
            let updates = view.read(cx).updates.read(cx);
            assert!(!updates.notice_checking);
            assert!(updates.notice.is_none() && updates.dismissed_notice.is_some());
            assert_eq!(updates.notice_message, super::DISMISSED_MESSAGE);
            assert!(window.try_find("update-notice").is_none());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn a_cached_dismissed_result_keeps_a_failed_manual_check_message(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::updates::{CHECK_FAILED_PREFIX, NoticeAttempt, NoticeOutcome};

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            let notice = view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.seed_macos(None);
                    updates.notice.clone().unwrap()
                })
            });
            let feed = |dispatched, outcome, cx: &mut gpui_kit::App| {
                view.update(cx, |this, cx| {
                    this.updates.update(cx, |updates, cx| {
                        updates.apply_notice_attempt(
                            NoticeAttempt {
                                dispatched,
                                outcome,
                            },
                            cx,
                        )
                    })
                })
            };
            // A manual check fails after the notice was dismissed...
            feed(true, Err(anyhow::anyhow!("offline")), cx);
            // ...then the 5-second cached poll reports the dismissed release without a request.
            feed(false, Ok(NoticeOutcome::Dismissed(notice.clone())), cx);
            let message =
                |cx: &gpui_kit::App| view.read(cx).updates.read(cx).notice_message.clone();
            assert!(
                message(cx).starts_with(CHECK_FAILED_PREFIX),
                "{}",
                message(cx)
            );
            // A real check that succeeds replaces it.
            feed(true, Ok(NoticeOutcome::Dismissed(notice)), cx);
            assert_eq!(message(cx), super::DISMISSED_MESSAGE);
            window.render_frame(cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn restart_tells_the_panels_they_are_busy_before_the_handoff_and_frees_them_on_failure(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    let relaunch: super::Relauncher =
                        std::sync::Arc::new(|_: &std::path::Path, _: Option<&std::path::Path>| {
                            false
                        });
                    updates.seed_macos(Some(relaunch));
                })
            });
            window.render_frame(cx);
            window.click("update-notice-restart", cx);
            // Same turn: the relaunch task has not run yet, so the panels must already be told.
            assert!(view.read(cx).busy);
            assert!(view.read(cx).modal_busy(cx));
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            view.read(cx).status.starts_with("Cannot restart")
        })
        .await;
        cx.update_window(handle.into(), |_, _, cx| {
            assert!(!view.read(cx).busy);
            assert!(!view.read(cx).modal_busy(cx));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn a_newer_release_retires_a_stale_restart_and_a_check_clears_a_failed_install(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::updates::{NoticeAttempt, NoticeOutcome};

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            let relaunch: super::Relauncher =
                std::sync::Arc::new(|_: &std::path::Path, _: Option<&std::path::Path>| false);
            let newer = view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.seed_macos(Some(relaunch));
                    // The seeded notice is the maximum version, so scope the finished install
                    // to a lower release and announce a genuinely higher one.
                    let installed: crate::desktop::updates::Version = "2.2.0.0".parse().unwrap();
                    updates.install = Some(InstallHandle::restarted_for(installed));
                    updates.install_for = Some(installed);
                    let feed = updates.notice_feed.clone().unwrap();
                    feed.parse_notice_manifest("version=2.3.0.0\ntag=v2.3.0.0\n")
                        .unwrap()
                })
            });
            let feed = |outcome, cx: &mut gpui_kit::App| {
                view.update(cx, |this, cx| {
                    this.updates.update(cx, |updates, cx| {
                        updates.apply_notice_attempt(
                            NoticeAttempt {
                                dispatched: true,
                                outcome,
                            },
                            cx,
                        )
                    })
                })
            };
            // Restart was for 2.2.0; a newer announcement must not inherit it.
            feed(Ok(NoticeOutcome::Available(newer)), cx);
            let updates = view.read(cx).updates.read(cx);
            assert!(updates.install_state().is_none());
            assert!(updates.notice.is_some());
            // A failed install must not mask a later "up to date".
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.install = Some(InstallHandle::failed("boom"));
                    updates.install_for = None;
                })
            });
            feed(Ok(NoticeOutcome::Current), cx);
            let updates = view.read(cx).updates.read(cx);
            assert!(updates.install_state().is_none());
            assert!(updates.notice.is_none());
            assert_eq!(updates.mac_status(), "Regressify is up to date");
            window.render_frame(cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn only_a_release_newer_than_the_installed_one_retires_the_restart(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::updates::{NoticeAttempt, NoticeOutcome, Version};

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            let relaunch: super::Relauncher =
                std::sync::Arc::new(|_: &std::path::Path, _: Option<&std::path::Path>| false);
            let feed = view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.seed_macos(Some(relaunch));
                    updates.notice_feed.clone().unwrap()
                })
            });
            let notice = |version: &str| {
                feed.parse_notice_manifest(&format!("version={version}\ntag=v{version}\n"))
                    .unwrap()
            };
            let planned: Version = "2.5.0.0".parse().unwrap();
            // The banner was clicked at 2.3.0, but the fresh plan installed 2.5.0.
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.install = Some(InstallHandle::restarted_for(planned));
                    updates.install_for = Some("2.3.0.0".parse().unwrap());
                })
            });
            let announce = |version: &str, cx: &mut gpui_kit::App| {
                let outcome = Ok(NoticeOutcome::Available(notice(version)));
                view.update(cx, |this, cx| {
                    this.updates.update(cx, |updates, cx| {
                        updates.apply_notice_attempt(
                            NoticeAttempt {
                                dispatched: false,
                                outcome,
                            },
                            cx,
                        )
                    })
                })
            };
            let restart_kept = |cx: &gpui_kit::App| {
                matches!(
                    view.read(cx).updates.read(cx).install_state(),
                    Some(crate::desktop::updates::InstallState::Restart)
                )
            };
            // A cached poll still announcing the older clicked release keeps the Restart.
            announce("2.3.0.0", cx);
            assert!(restart_kept(cx));
            // So does the installed release itself.
            announce("2.5.0.0", cx);
            assert!(restart_kept(cx));
            // Only a newer release retires it.
            announce("2.6.0.0", cx);
            assert!(!restart_kept(cx));
            assert!(view.read(cx).updates.read(cx).install_state().is_none());
            // Inverse: clicked 2.5.0, but an older fresh plan installed 2.3.0.
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.install =
                        Some(InstallHandle::restarted_for("2.3.0.0".parse().unwrap()));
                    updates.install_for = Some("2.5.0.0".parse().unwrap());
                })
            });
            // The cached poll announcing the clicked release keeps the Restart.
            announce("2.5.0.0", cx);
            assert!(restart_kept(cx));
            // Only a release newer than both retires it.
            announce("2.6.0.0", cx);
            assert!(!restart_kept(cx));
            window.render_frame(cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn a_running_install_blocks_quit_close_and_the_install_controls(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.seed_macos(None);
                    updates.install_bundle =
                        Some(std::path::PathBuf::from("/nonexistent/Regressify.app"));
                    updates.install = Some(InstallHandle::in_progress());
                    updates.install_for = updates.notice.as_ref().map(|notice| notice.version);
                });
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.find("update-notice").visible());
            for id in [
                "update-notice-install",
                "update-notice-restart",
                "update-notice-dismiss",
                "update-notice-copy",
            ] {
                assert!(
                    window.try_find(id).is_none(),
                    "{id} is hidden while installing"
                );
            }
            let blocked = "An update is installing; close again when it finishes";
            // The native close hook refuses and says why.
            let closes = view.update(cx, |this, cx| this.window_should_close(window, cx));
            assert!(!closes);
            assert_eq!(view.read(cx).status, blocked);
            // Quit paths (including a pending relaunch) do nothing while the install runs.
            view.update(cx, |this, cx| {
                this.status.clear();
                this.updates
                    .update(cx, |updates, _| updates.install_on_close = true);
                this.quit_or_install(cx);
            });
            assert_eq!(view.read(cx).status, blocked);
            assert!(view.read(cx).updates.read(cx).install_on_close);
            assert!(
                !view.read(cx).busy,
                "the install guard is not the busy flag"
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn a_busy_window_refuses_to_start_an_install(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.seed_macos(None);
                    updates.install_bundle =
                        Some(std::path::PathBuf::from("/nonexistent/Regressify.app"));
                });
                this.busy = true;
                cx.notify();
            });
            window.render_frame(cx);
            // Clicking and calling the handler directly: neither may start an install.
            window.click("update-notice-install", cx);
            view.update(cx, |this, cx| this.start_macos_install(window, cx));
            assert!(view.read(cx).updates.read(cx).install.is_none());
            // Idle again: the same call starts one (it fails fast on the missing bundle folder).
            view.update(cx, |this, cx| {
                this.busy = false;
                this.start_macos_install(window, cx);
            });
            assert!(view.read(cx).updates.read(cx).install.is_some());
        })
        .unwrap();
    }

    /// A relauncher that records its calls and, while it runs, tries to open the workspace the
    /// way the replacement app would; `keep` holds the instance it got (another process took it).
    fn probing_relauncher(
        locks: std::path::PathBuf,
        keep: bool,
    ) -> (
        super::Relauncher,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        use crate::desktop::instance::{DesktopInstance, OpenInstance};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let was_free = Arc::new(AtomicBool::new(false));
        let held: Arc<Mutex<Option<DesktopInstance>>> = Arc::default();
        let relaunch: super::Relauncher = {
            let calls = calls.clone();
            let was_free = was_free.clone();
            Arc::new(
                move |_: &std::path::Path, workspace: Option<&std::path::Path>| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Evaluated first so the guard is not held while it is locked again below.
                    let first = held.lock().unwrap().is_none();
                    if let Some(workspace) = workspace
                        && first
                        && let Ok(OpenInstance::Owned(instance)) =
                            DesktopInstance::open(&locks, workspace)
                    {
                        was_free.store(true, Ordering::SeqCst);
                        if keep {
                            *held.lock().unwrap() = Some(instance);
                        }
                    }
                    false
                },
            )
        };
        (relaunch, calls, was_free)
    }

    #[gpui_kit::test]
    async fn restart_frees_the_workspace_before_open_and_reclaims_it_when_the_launch_fails(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::instance::{DesktopInstance, OpenInstance};
        let directory = tempfile::tempdir().unwrap();
        let locks = directory.path().join("instances");
        let (handle, view) = editor(cx, directory.path());
        let (relaunch, calls, was_free) = probing_relauncher(locks.clone(), false);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.workspace_instance =
                    match DesktopInstance::open(&locks, directory.path()).unwrap() {
                        OpenInstance::Owned(instance) => Some(instance),
                        OpenInstance::Activated => panic!("Expected first fixture ownership"),
                    };
                this.updates
                    .update(cx, |updates, _| updates.seed_macos(Some(relaunch)));
            });
            window.render_frame(cx);
            window.click("update-notice-restart", cx);
            assert!(view.read(cx).busy);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            view.read(cx).status.starts_with("Cannot restart")
        })
        .await;
        cx.update_window(handle.into(), |_, _, cx| {
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            // While `open` ran the workspace was unowned, so the replacement could take it.
            assert!(was_free.load(std::sync::atomic::Ordering::SeqCst));
            // The failed launch gave it back to this window, which is usable again.
            let this = view.read(cx);
            assert!(!this.busy && !this.workspace_lost);
            assert!(
                this.workspace_instance
                    .as_ref()
                    .is_some_and(|instance| instance.owns_workspace(directory.path()).unwrap())
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn restart_keeps_a_window_frozen_but_closable_when_another_process_took_the_workspace(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::instance::{DesktopInstance, OpenInstance};
        use std::sync::atomic::Ordering;
        let directory = tempfile::tempdir().unwrap();
        let locks = directory.path().join("instances");
        let (handle, view) = editor(cx, directory.path());
        let (relaunch, calls, _) = probing_relauncher(locks.clone(), true);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.workspace_instance =
                    match DesktopInstance::open(&locks, directory.path()).unwrap() {
                        OpenInstance::Owned(instance) => Some(instance),
                        OpenInstance::Activated => panic!("Expected first fixture ownership"),
                    };
                this.updates
                    .update(cx, |updates, _| updates.seed_macos(Some(relaunch)));
            });
            window.render_frame(cx);
            window.click("update-notice-restart", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            view.read(cx).workspace_lost
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            let this = view.read(cx);
            assert!(this.busy && this.workspace_lost);
            assert!(this.workspace_instance.is_none());
            assert!(this.status.starts_with("Cannot reopen this workspace"));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            // Closing the window reaches `quit_or_install`, which relaunches again (the
            // relauncher counts the call) even though the window is `busy`.
            view.update(cx, |this, cx| {
                this.updates
                    .update(cx, |updates, _| updates.install_on_close = true);
                assert!(!this.window_should_close(window, cx));
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
            calls.load(Ordering::SeqCst) == 2
        })
        .await;
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            !view.read(cx).updates.read(cx).restarting
        })
        .await;
        // The close that waited for a preferences save takes the same path.
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.close_after_preferences = true;
                this.updates
                    .update(cx, |updates, _| updates.install_on_close = true);
                this.save_preferences(window, cx);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
            calls.load(Ordering::SeqCst) == 3
        })
        .await;
    }

    #[gpui_kit::test]
    async fn dismissing_the_displayed_release_keeps_the_same_target_hidden(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::updates::{NoticeAttempt, NoticeOutcome};
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        let relaunch: super::Relauncher =
            std::sync::Arc::new(|_: &std::path::Path, _: Option<&std::path::Path>| false);
        cx.update_window(handle.into(), |_, window, cx| {
            // Clicked 2.3, but the fresh plan installed 2.5.
            let feed = view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    updates.seed_macos(Some(relaunch));
                    let feed = updates.notice_feed.clone().unwrap();
                    updates.notice = Some(
                        feed.parse_notice_manifest("version=2.3.0.0\ntag=v2.3.0.0\n")
                            .unwrap(),
                    );
                    updates.install =
                        Some(InstallHandle::restarted_for("2.5.0.0".parse().unwrap()));
                    updates.install_for = Some("2.3.0.0".parse().unwrap());
                    feed
                })
            });
            window.render_frame(cx);
            window.click("update-notice-dismiss", cx);
            window.render_frame(cx);
            assert!(window.try_find("update-notice").is_none());
            // The next check announces the displayed release: it must stay hidden.
            let notice = feed
                .parse_notice_manifest("version=2.5.0.0\ntag=v2.5.0.0\n")
                .unwrap();
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, cx| {
                    updates.apply_notice_attempt(
                        NoticeAttempt {
                            dispatched: true,
                            outcome: Ok(NoticeOutcome::Available(notice)),
                        },
                        cx,
                    )
                })
            });
            window.render_frame(cx);
            assert!(window.try_find("update-notice").is_none());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn the_banner_and_dialog_name_the_release_that_was_installed(cx: &mut TestAppContext) {
        use crate::desktop::updates::Version;
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        let relaunch: super::Relauncher =
            std::sync::Arc::new(|_: &std::path::Path, _: Option<&std::path::Path>| false);
        // (cached announcement, clicked release, release the plan installed)
        for (announced, clicked, planned) in [
            ("2.3.0.0", "2.3.0.0", "2.4.0.0"),
            ("2.5.0.0", "2.5.0.0", "2.4.0.0"),
        ] {
            let planned: Version = planned.parse().unwrap();
            cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.updates.update(cx, |updates, _| {
                        updates.seed_macos(Some(relaunch.clone()));
                        let feed = updates.notice_feed.clone().unwrap();
                        updates.notice = Some(
                            feed.parse_notice_manifest(&format!(
                                "version={announced}\ntag=v{announced}\n"
                            ))
                            .unwrap(),
                        );
                        updates.install = Some(InstallHandle::restarted_for(planned));
                        updates.install_for = Some(clicked.parse().unwrap());
                    });
                    cx.notify();
                });
                window.render_frame(cx);
                // The release page would be the clicked release's, not the installed one's.
                assert!(window.try_find("update-notice-release").is_none());
                // The banner names what was installed, not the cached announcement.
                assert_eq!(
                    window.find("update-notice").label(),
                    Some("Regressify 2.4.0 is installed. Restart Regressify to finish updating.")
                );
                view.update(cx, |this, cx| this.open_updates(window, cx));
            })
            .unwrap();
            settle_dialog(cx, handle).await;
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(
                    window.find("update-status").label(),
                    Some("Regressify 2.4.0 is installed. Restart to finish updating.")
                );
                window.close_dialog(cx);
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    async fn restart_with_an_invalid_field_draft_opens_the_unsaved_guard(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    let relaunch: super::Relauncher = std::sync::Arc::new(
                        move |_: &std::path::Path, _: Option<&std::path::Path>| {
                            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            false
                        },
                    );
                    updates.seed_macos(Some(relaunch));
                });
                // An unparsable Fields draft makes `flush_drafts` return Err.
                let binding = this
                    .bindings
                    .iter()
                    .find(|binding| binding.field.key == "delay")
                    .unwrap();
                if let crate::desktop::ui::Control::Input(input) = &binding.control {
                    input.update(cx, |state, cx| state.set_value("invalid draft", window, cx));
                }
                this.request_macos_restart(window, cx);
                assert!(this.field_errors.contains_key("delay"));
            });
        })
        .unwrap();
        settle_dialog(cx, handle).await;
        cx.update_window(handle.into(), |_, window, cx| {
            // Same guard as native close; the restart is not refused with a bare status.
            assert!(window.find("guard-discard").visible());
            assert!(!view.read(cx).status.starts_with("Cannot restart to update"));
            // Save is disabled while a field is invalid, so clicking it restarts nothing.
            window.click("guard-save", cx);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            window.click("guard-discard", cx);
        })
        .unwrap();
        // Discard drops the invalid draft and the restart continues into the relaunch.
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, _| {
            calls.load(std::sync::atomic::Ordering::SeqCst) == 1
        })
        .await;
    }

    #[gpui_kit::test]
    async fn a_peer_dismissal_keeps_the_restart_banner_while_an_install_is_pending(
        cx: &mut TestAppContext,
    ) {
        use crate::desktop::updates::{NoticeAttempt, NoticeOutcome};

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            let notice = view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, _| {
                    // Install finished (Restart offered); the relaunch is never invoked here.
                    let relaunch: super::Relauncher =
                        std::sync::Arc::new(|_: &std::path::Path, _: Option<&std::path::Path>| {
                            false
                        });
                    updates.seed_macos(Some(relaunch));
                    updates.notice.clone().unwrap()
                })
            });
            view.update(cx, |this, cx| {
                this.updates.update(cx, |updates, cx| {
                    updates.apply_notice_attempt(
                        NoticeAttempt {
                            dispatched: false,
                            outcome: Ok(NoticeOutcome::Dismissed(notice)),
                        },
                        cx,
                    )
                })
            });
            window.render_frame(cx);
            assert!(window.try_find("update-notice-restart").is_some());
        })
        .unwrap();
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
