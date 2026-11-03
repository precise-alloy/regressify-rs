use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{Result, anyhow, bail, ensure};
use chromiumoxide::{Browser, Page};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    config::{self, ResolvedConfig, viewports::ViewportDef},
    types::{
        CloseTabAction, CloseTabTarget, SwitchTabAction, TabTarget, TestAction, UrlReplacement,
    },
};

use super::{
    authentication::Authentication, cdp::Client, runtime::Runtime, storage_state::StorageState,
    test_action::UrlMatcher,
};

#[derive(Default)]
struct TabState {
    owned: BTreeSet<String>,
    prepared: BTreeMap<String, Option<Authentication>>,
    errors: BTreeMap<String, String>,
    failure: Option<String>,
}

pub(super) struct Tabs {
    client: Client,
    original: String,
    active: String,
    visited: BTreeSet<String>,
    history: Vec<String>,
    timeout: Duration,
    state: Arc<Mutex<TabState>>,
    task: tokio::task::JoinHandle<()>,
}

impl Tabs {
    pub async fn install(
        page: &Page,
        storage: Arc<StorageState>,
        config: &ResolvedConfig,
        viewport: Option<&ViewportDef>,
        has_touch: bool,
    ) -> Result<Self> {
        let client = storage.client();
        let mut events = client.subscribe();
        let original = page.target_id().as_ref().to_string();
        let state = Arc::new(Mutex::new(TabState {
            owned: BTreeSet::from([original.clone()]),
            prepared: BTreeMap::from([(original.clone(), None)]),
            ..Default::default()
        }));
        let timeout = Duration::from_millis(config.element_timeout_ms);
        let viewport = viewport.cloned();
        let bypass_csp = config.bypass_csp;
        let entries = config.basic_auth.clone();
        let connection = client.clone();
        let shared = state.clone();
        let task = tokio::spawn(async move {
            let result: Result<()> = async {
                loop {
                    let event = events
                        .recv()
                        .await
                        .map_err(|_| anyhow!("Tab event stream closed or overflowed"))?;
                    if event.method == "Target.targetDestroyed" {
                        if let Some(target) = event.params["targetId"].as_str() {
                            shared.lock().await.prepared.remove(target);
                        }
                        continue;
                    }
                    if event.method != "Target.attachedToTarget"
                        || event.params["waitingForDebugger"] != true
                    {
                        continue;
                    }
                    let session = event.params["sessionId"]
                        .as_str()
                        .ok_or_else(|| anyhow!("New tab has no CDP session"))?;
                    let info = &event.params["targetInfo"];
                    let target = info["targetId"]
                        .as_str()
                        .ok_or_else(|| anyhow!("New tab has no target ID"))?;
                    let owned = info["type"] == "page"
                        && match info["openerId"].as_str() {
                            Some(opener) => shared.lock().await.owned.contains(opener),
                            None => false,
                        };
                    let touch_frame = has_touch && info["type"] == "iframe";
                    if owned {
                        shared.lock().await.owned.insert(target.to_string());
                    }
                    let prepared = tokio::time::timeout(timeout, async {
                        if touch_frame {
                            connection
                                .call_all(
                                    session,
                                    vec![
                                        (
                                            "Emulation.setTouchEmulationEnabled",
                                            touch_emulation_params(true)?,
                                        ),
                                        ("Target.setAutoAttach", touch_frame_auto_attach()),
                                        ("Runtime.runIfWaitingForDebugger", json!({})),
                                    ],
                                )
                                .await?;
                            return Ok(None);
                        }
                        if !owned {
                            connection
                                .call(Some(session), "Runtime.runIfWaitingForDebugger", json!({}))
                                .await?;
                            return Ok(None);
                        }
                        // Auto-attach pauses both window.open and target="_blank"
                        // before page scripts or authenticated requests can run.
                        let source = storage.prepare_session(session).await?;
                        let authentication =
                            Authentication::prepare_session(connection.clone(), session, &entries)?;
                        let commands = popup_commands(
                            viewport.as_ref(),
                            bypass_csp,
                            source,
                            authentication.is_some(),
                            has_touch,
                        )?;
                        connection.call_all(session, commands).await?;
                        Ok(authentication)
                    })
                    .await
                    .map_err(|_| anyhow!("New tab setup exceeded {}ms", timeout.as_millis()))
                    .and_then(|result| result);
                    let failed = prepared.is_err();
                    if touch_frame && let Err(error) = &prepared {
                        shared.lock().await.failure =
                            Some(format!("Cannot initialize touch-enabled frame: {error:#}"));
                    }
                    if owned {
                        let mut state = shared.lock().await;
                        match prepared {
                            Ok(authentication) => {
                                state.prepared.insert(target.to_string(), authentication);
                            }
                            Err(error) => {
                                state
                                    .errors
                                    .insert(target.to_string(), format!("{error:#}"));
                            }
                        }
                    }
                    if failed {
                        // Release the tab even when setup failed before its batch was sent.
                        let _ = connection
                            .call(Some(session), "Runtime.runIfWaitingForDebugger", json!({}))
                            .await;
                    }
                }
            }
            .await;
            if let Err(error) = result {
                shared.lock().await.failure = Some(error.to_string());
            }
        });
        let tabs = Self {
            client,
            original: original.clone(),
            active: original.clone(),
            visited: BTreeSet::from([original]),
            history: Vec::new(),
            timeout,
            state,
            task,
        };
        // Subscribe and enable discovery before any scenario hook or action can
        // open a tab. Never depend on subscribing after the triggering click.
        tabs.client
            .call(None, "Target.setDiscoverTargets", json!({"discover": true}))
            .await?;
        tabs.client
            .call(
                None,
                "Target.setAutoAttach",
                json!({
                    "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true,
                    "filter": [{"type": "page"}, {"exclude": true}]
                }),
            )
            .await?;
        if has_touch {
            // OOPIF emulation is target-scoped, not inherited from the top-level page.
            // Pause child documents so their first scripts see the same touch capability.
            let session = tabs.client.attach(&tabs.original).await?;
            tabs.client
                .call(
                    Some(&session),
                    "Emulation.setTouchEmulationEnabled",
                    touch_emulation_params(true)?,
                )
                .await?;
            tabs.client
                .call(
                    Some(&session),
                    "Target.setAutoAttach",
                    touch_frame_auto_attach(),
                )
                .await?;
        }
        Ok(tabs)
    }

    pub async fn check(&self) -> Result<()> {
        let state = self.state.lock().await;
        if let Some(error) = &state.failure {
            bail!("{error}");
        }
        if let Some(error) = state.errors.get(&self.active) {
            bail!("Cannot initialize selected tab: {error}");
        }
        if let Some(Some(authentication)) = state.prepared.get(&self.active) {
            authentication.check()?;
        }
        Ok(())
    }

    pub async fn switch(
        &mut self,
        browser: &Browser,
        action: &SwitchTabAction,
        replacements: &[UrlReplacement],
    ) -> Result<(Page, Runtime, bool)> {
        let timeout = self.timeout;
        tokio::time::timeout(timeout, self.switch_inner(browser, action, replacements))
            .await
            .map_err(|_| anyhow!(
                "switchTab timed out after {}ms: the requested tab may have closed, or no matching new tab opened from the current page",
                timeout.as_millis()
            ))?
    }

    async fn switch_inner(
        &mut self,
        browser: &Browser,
        action: &SwitchTabAction,
        replacements: &[UrlReplacement],
    ) -> Result<(Page, Runtime, bool)> {
        ensure!(
            action.switch_tab == TabTarget::New || action.url.is_none(),
            "switchTab.url is only supported with switchTab: new"
        );
        let pattern = action
            .url
            .as_deref()
            .map(|url| config::replacements::apply_replacements(url, replacements))
            .transpose()?;
        let matcher = pattern.as_deref().map(UrlMatcher::compile).transpose()?;
        let target = match action.switch_tab {
            TabTarget::New => self.wait_for_new(matcher.as_ref()).await?,
            TabTarget::Original => self.original.clone(),
            TabTarget::Previous => self
                .history
                .last()
                .cloned()
                .ok_or_else(|| anyhow!("switchTab: previous has no previously selected tab"))?,
        };
        let page = self
            .acquire_page(
                browser,
                &target,
                "The requested tab is closed or unavailable",
            )
            .await?;
        let runtime = Runtime::with_client(self.client.clone(), &page)
            .await
            .map_err(|error| {
                anyhow!("Cannot attach to requested tab; it may have closed: {error}")
            })?;
        runtime
            .target(&None, self.timeout)
            .await?
            .wait_for_ready(self.timeout)
            .await?;
        page.bring_to_front().await?;
        let first_visit = self.visited.insert(target.clone());
        if target != self.active {
            if action.switch_tab == TabTarget::Previous {
                self.history.pop();
            } else {
                self.history.push(self.active.clone());
            }
            self.active = target;
        }
        self.check().await?;
        Ok((page, runtime, first_visit))
    }

    pub async fn close(
        &mut self,
        browser: &Browser,
        runtime: &Runtime,
        action: &TestAction,
        close: &CloseTabAction,
    ) -> Result<Option<(Page, Runtime, bool)>> {
        let CloseTabTarget::Current = close.close_tab;
        ensure!(
            self.active != self.original,
            "closeTab: current cannot close the scenario's original tab"
        );
        let previous = self
            .history
            .last()
            .cloned()
            .ok_or_else(|| anyhow!("closeTab: current has no previously selected tab"))?;
        let closing = self.active.clone();
        let timeout = self.timeout;
        tokio::time::timeout(timeout, async {
            let current_targets = self
                .client
                .call(None, "Target.getTargets", json!({}))
                .await?;
            ensure!(
                current_targets["targetInfos"]
                    .as_array()
                    .is_some_and(|targets| targets
                        .iter()
                        .any(|info| info["targetId"].as_str() == Some(closing.as_str()))),
                "The current tab is already closed or unavailable"
            );
            runtime
                .with_dialog(action, timeout, runtime.close_page(timeout))
                .await?;
            if runtime.navigation_cancelled() {
                self.check().await?;
                return Ok(None);
            }
            loop {
                let response = self
                    .client
                    .call(None, "Target.getTargets", json!({}))
                    .await?;
                let still_open = response["targetInfos"].as_array().is_some_and(|targets| {
                    targets
                        .iter()
                        .any(|info| info["targetId"].as_str() == Some(closing.as_str()))
                });
                if !still_open {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let page = self
                .acquire_page(
                    browser,
                    &previous,
                    "The previous tab is closed or unavailable",
                )
                .await?;
            let next_runtime = Runtime::with_client(self.client.clone(), &page)
                .await
                .map_err(|error| {
                    anyhow!("Cannot attach to previous tab; it may have closed: {error}")
                })?;
            next_runtime
                .target(&None, self.timeout)
                .await?
                .wait_for_ready(self.timeout)
                .await?;
            page.bring_to_front().await?;
            let first_visit = self.visited.insert(previous.clone());
            self.history.pop();
            self.history.retain(|entry| entry != &closing);
            self.active = previous;
            self.check().await?;
            Ok(Some((page, next_runtime, first_visit)))
        })
        .await
        .map_err(|_| {
            anyhow!(
                "closeTab timed out after {}ms while closing the current tab",
                timeout.as_millis()
            )
        })?
    }

    async fn acquire_page(
        &self,
        browser: &Browser,
        target: &str,
        missing_error: &'static str,
    ) -> Result<Page> {
        loop {
            let current_targets = self
                .client
                .call(None, "Target.getTargets", json!({}))
                .await?;
            ensure!(
                current_targets["targetInfos"]
                    .as_array()
                    .is_some_and(|targets| targets
                        .iter()
                        .any(|info| info["targetId"].as_str() == Some(target))),
                "{missing_error}"
            );
            if let Ok(page) = browser.get_page(target.to_string().into()).await {
                return Ok(page);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_for_new(&self, matcher: Option<&UrlMatcher>) -> Result<String> {
        loop {
            self.check().await?;
            let response = self
                .client
                .call(None, "Target.getTargets", json!({}))
                .await?;
            let targets = response["targetInfos"]
                .as_array()
                .ok_or_else(|| anyhow!("Browser did not return its tab list"))?;
            let candidates = new_tab_candidates(targets, &self.active, &self.visited, matcher);
            ensure!(
                candidates.len() <= 1,
                "More than one new tab matches; add a unique url to switchTab: new"
            );
            if let Some(target) = candidates.first() {
                let state = self.state.lock().await;
                if let Some(error) = state.errors.get(target) {
                    bail!("Cannot initialize new tab: {error}");
                }
                if state.prepared.contains_key(target) {
                    return Ok(target.clone());
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

impl Drop for Tabs {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn new_tab_candidates(
    targets: &[Value],
    opener: &str,
    visited: &BTreeSet<String>,
    matcher: Option<&UrlMatcher>,
) -> Vec<String> {
    targets
        .iter()
        .filter_map(|target| {
            let id = target["targetId"].as_str()?;
            let url = target["url"].as_str()?;
            (target["type"] == "page"
                && target["openerId"].as_str() == Some(opener)
                && !visited.contains(id)
                && !matches!(url, "" | ":")
                && matcher.is_none_or(|matcher| matcher.matches(url)))
            .then(|| id.to_string())
        })
        .collect()
}

fn touch_frame_auto_attach() -> Value {
    json!({
        "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true,
        "filter": [{"type": "iframe"}, {"exclude": true}]
    })
}

fn touch_emulation_params(enabled: bool) -> Result<Value> {
    let mut builder =
        chromiumoxide::cdp::browser_protocol::emulation::SetTouchEmulationEnabledParams::builder()
            .enabled(enabled);
    if enabled {
        builder = builder.max_touch_points(5);
    }
    Ok(serde_json::to_value(builder.build().map_err(|error| {
        anyhow!("Cannot configure touch emulation: {error}")
    })?)?)
}

fn popup_commands(
    viewport: Option<&ViewportDef>,
    bypass_csp: bool,
    storage_script: String,
    authenticate: bool,
    has_touch: bool,
) -> Result<Vec<(&'static str, Value)>> {
    let mut commands = vec![("Page.enable", json!({}))];
    commands.push((
        "Emulation.setTouchEmulationEnabled",
        touch_emulation_params(has_touch)?,
    ));
    if has_touch {
        commands.push(("Target.setAutoAttach", touch_frame_auto_attach()));
    }
    if let Some(viewport) = viewport {
        let params = chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams::builder()
            .width(viewport.width as i64)
            .height(viewport.height as i64)
            .device_scale_factor(1.0)
            .mobile(false)
            .build()
            .map_err(|error| anyhow!("Cannot configure popup viewport: {error:?}"))?;
        commands.push((
            "Emulation.setDeviceMetricsOverride",
            serde_json::to_value(params)?,
        ));
    }
    if bypass_csp {
        commands.push(("Page.setBypassCSP", json!({"enabled": true})));
    }
    commands.push((
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": storage_script}),
    ));
    commands.push(("DOMStorage.enable", json!({})));
    if authenticate {
        commands.push(("Fetch.enable", json!({"handleAuthRequests": true})));
    }
    commands.push(("Runtime.runIfWaitingForDebugger", json!({})));
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_require_an_unvisited_page_from_the_current_opener() {
        let targets = json!([
            {"targetId":"noopener", "type":"page", "openerId":"root", "canAccessOpener":false, "url":"https://other.test/quote?ready=1#top"},
            {"targetId":"seen", "type":"page", "openerId":"root", "url":"https://other.test/quote"},
            {"targetId":"unrelated", "type":"page", "openerId":"other", "url":"https://other.test/quote"},
            {"targetId":"frame", "type":"iframe", "openerId":"root", "url":"https://other.test/quote"},
            {"targetId":"loading", "type":"page", "openerId":"root", "url":""},
            {"targetId":"initial", "type":"page", "openerId":"root", "url":":"}
        ]);
        let matcher = UrlMatcher::compile("**/quote?ready=1#top").unwrap();
        let found = new_tab_candidates(
            targets.as_array().unwrap(),
            "root",
            &BTreeSet::from(["seen".into()]),
            Some(&matcher),
        );
        assert_eq!(found, ["noopener"]);
        assert_eq!(
            new_tab_candidates(
                targets.as_array().unwrap(),
                "root",
                &BTreeSet::from(["seen".into()]),
                None
            ),
            ["noopener"]
        );
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn blank_links_noopener_and_window_open_select_real_tabs() -> Result<()> {
        use super::super::browser_tests::{Site, launch_browser};
        use tiny_http::{Header, Response};

        let destination = Site::new(|request| {
            let _ = request.respond(Response::from_string(
                "<script>window.firstWidth = innerWidth;</script><body><button id='ready'>Ready</button></body>"
            ).with_header(Header::from_bytes("Content-Type", "text/html").unwrap()));
        })?;
        let destination_url = destination.origin.clone();
        let site = Site::new(move |request| {
            let _ = request.respond(Response::from_string(format!(r#"<body>
                <a id="blank" href="{destination_url}/quote?via=blank" target="_blank">Blank link</a>
                <a id="noopener" href="{destination_url}/quote?via=noopener" target="_blank" rel="noopener">No opener</a>
                <button id="script" onclick="const popup = window.open('about:blank'); setTimeout(() => popup.location.href = '{destination_url}/quote?via=script', 150)">Open</button>
                </body>"#)).with_header(Header::from_bytes("Content-Type", "text/html").unwrap()));
        })?;
        let (mut browser, handler, directory) = launch_browser(
            "tabs",
            super::super::browsers::LaunchOptions {
                viewport: Some((420, 240)),
                ..Default::default()
            },
        )
        .await?;
        let result: Result<()> = async {
            let page = browser.new_page("about:blank").await?;
            // Success paths keep the default budget; popup loads are slow under parallel load.
            let config = ResolvedConfig::default();
            let viewport = ViewportDef {
                label: "popup".into(),
                width: 420,
                height: 240,
            };
            let storage =
                Arc::new(StorageState::install(&browser, &page, None, &site.origin).await?);
            let mut tabs = Tabs::install(&page, storage, &config, Some(&viewport), false).await?;
            page.goto(site.origin.as_str()).await?;
            for kind in ["blank", "noopener", "script"] {
                page.find_element(format!("#{kind}")).await?.click().await?;
                let action = SwitchTabAction {
                    switch_tab: TabTarget::New,
                    url: Some(format!("**/quote?via={kind}")),
                    viewport_names: None,
                };
                let (popup, _, first_visit) = tabs.switch(&browser, &action, &[]).await?;
                ensure!(
                    first_visit && popup.target_id() != page.target_id(),
                    "did not select the popup"
                );
                let width: u32 = popup.evaluate("window.firstWidth").await?.into_value()?;
                ensure!(
                    width == viewport.width,
                    "viewport was applied after popup scripts ran: {width}"
                );
                if kind != "script" {
                    let no_opener: bool = popup
                        .evaluate("window.opener === null")
                        .await?
                        .into_value()?;
                    ensure!(
                        no_opener,
                        "target=_blank or noopener semantics were changed"
                    );
                }
                let (original, _, first_visit) = tabs
                    .switch(
                        &browser,
                        &SwitchTabAction {
                            switch_tab: TabTarget::Previous,
                            url: None,
                            viewport_names: None,
                        },
                        &[],
                    )
                    .await?;
                ensure!(
                    !first_visit && original.target_id() == page.target_id(),
                    "previous did not restore the original tab"
                );
                popup.close().await?;
            }
            tabs.check().await?;
            Ok(())
        }
        .await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn tab_selection_filters_redirects_and_preserves_nested_history() -> Result<()> {
        use super::super::browser_tests::{Site, launch_browser};
        use tiny_http::{Header, Response, StatusCode};
        let frame_site = Site::new(|request| {
            let _ = request.respond(
                Response::from_string(
                    "<a id='nested' href='/nested' target='_blank'>Nested popup</a>",
                )
                .with_header(Header::from_bytes("Content-Type", "text/html").unwrap()),
            );
        })?;
        let frame_origin = frame_site.origin.replace("127.0.0.1", "localhost");
        let iframe_origin = frame_origin.clone();
        let site = Site::new(move |request| {
            let response = if let Some(name) = request.url().strip_prefix("/redirect?") {
                Response::from_string("")
                    .with_status_code(StatusCode(302))
                    .with_header(
                        Header::from_bytes("Location", format!("/quote?tab={name}#ready")).unwrap(),
                    )
            } else {
                Response::from_string(format!(
                    r#"<button id="both" onclick="window.open('/redirect?first'); window.open('/redirect?second')">Open both</button>
                    <iframe id="child-frame" src="{iframe_origin}/frame"></iframe>"#
                ))
            };
            let _ = request.respond(
                response.with_header(Header::from_bytes("Content-Type", "text/html").unwrap()),
            );
        })?;
        let (mut browser, handler, directory) =
            launch_browser("tab-history", Default::default()).await?;
        let result: Result<()> = async {
            let page = browser.new_page("about:blank").await?;
            // Success paths keep the default budget; popup loads are slow under parallel load.
            let config = ResolvedConfig::default();
            let storage =
                Arc::new(StorageState::install(&browser, &page, None, &site.origin).await?);
            let mut tabs = Tabs::install(&page, storage.clone(), &config, None, false).await?;
            page.goto(site.origin.as_str()).await?;
            page.find_element("#both").await?.click().await?;
            tokio::time::timeout(Duration::from_millis(config.element_timeout_ms), async {
                loop {
                    let response = tabs
                        .client
                        .call(None, "Target.getTargets", json!({}))
                        .await?;
                    if new_tab_candidates(
                        response["targetInfos"].as_array().unwrap(),
                        &tabs.active,
                        &tabs.visited,
                        None,
                    )
                    .len()
                        == 2
                    {
                        break Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await??;
            let ambiguous = tabs
                .switch(
                    &browser,
                    &SwitchTabAction {
                        switch_tab: TabTarget::New,
                        url: None,
                        viewport_names: None,
                    },
                    &[],
                )
                .await
                .err()
                .expect("multiple matching tabs must not be chosen arbitrarily");
            ensure!(
                ambiguous.to_string().contains("More than one"),
                "wrong ambiguity error: {ambiguous}"
            );
            ensure!(
                tabs.active == page.target_id().as_ref(),
                "ambiguous selection changed the active tab"
            );
            let replacements = [serde_json::from_value::<UrlReplacement>(json!({
                "ref": "https://reference.invalid", "test": site.origin
            }))?];
            let (popup, runtime, _) = tabs
                .switch(
                    &browser,
                    &SwitchTabAction {
                        switch_tab: TabTarget::New,
                        url: Some("https://reference.invalid/quote?tab=first#ready".into()),
                        viewport_names: None,
                    },
                    &replacements,
                )
                .await?;
            let suite = crate::types::TestSuite::default();
            let scenario = crate::types::TestScenario::default();
            super::super::test_action::exec_test_action(
                super::super::test_action::ActionExecutionContext {
                    page: &popup,
                    runtime: &runtime,
                    storage: &storage,
                    test_suite: &suite,
                    scenario: &scenario,
                    config: &config,
                    url_replacements: &[],
                    viewport_label: None,
                    log_prefix: "nested-tab",
                },
                &crate::types::TestAction::Click(crate::types::ClickAction {
                    click: "#nested".into(),
                    frame: Some(crate::types::Frame::Single("#child-frame".into())),
                    viewport_names: None,
                    ..Default::default()
                }),
            )
            .await?;
            let (nested, nested_runtime, _) = tabs
                .switch(
                    &browser,
                    &SwitchTabAction {
                        switch_tab: TabTarget::New,
                        url: Some(format!("{frame_origin}/nested")),
                        viewport_names: None,
                    },
                    &[],
                )
                .await?;
            ensure!(
                nested.target_id() != popup.target_id(),
                "iframe link did not select a nested popup"
            );
            let close_action = CloseTabAction {
                close_tab: CloseTabTarget::Current,
                viewport_names: None,
            };
            let (previous, _, _) = tabs
                .close(
                    &browser,
                    &nested_runtime,
                    &TestAction::CloseTab(close_action.clone()),
                    &close_action,
                )
                .await?
                .expect("closing nested popup should return to opener");
            ensure!(
                previous.target_id() == popup.target_id(),
                "nested closeTab did not return to the opener"
            );
            let (original, _, _) = tabs
                .switch(
                    &browser,
                    &SwitchTabAction {
                        switch_tab: TabTarget::Original,
                        url: None,
                        viewport_names: None,
                    },
                    &[],
                )
                .await?;
            ensure!(
                original.target_id() == page.target_id(),
                "original did not select the initial scenario page"
            );
            let (previous, _, _) = tabs
                .switch(
                    &browser,
                    &SwitchTabAction {
                        switch_tab: TabTarget::Previous,
                        url: None,
                        viewport_names: None,
                    },
                    &[],
                )
                .await?;
            ensure!(
                previous.target_id() == popup.target_id(),
                "switching to original lost the prior selection"
            );
            Ok(())
        }
        .await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn missing_unrelated_and_closed_tabs_fail_without_switching() -> Result<()> {
        use super::super::browser_tests::launch_browser;
        let (mut browser, handler, directory) =
            launch_browser("tab-errors", Default::default()).await?;
        let result: Result<()> = async {
            let page = browser.new_page("about:blank").await?;
            let config = ResolvedConfig {
                element_timeout_ms: 300,
                ..Default::default()
            };
            let storage = Arc::new(
                StorageState::install(&browser, &page, None, "https://example.test").await?,
            );
            let mut tabs = Tabs::install(&page, storage, &config, None, false).await?;
            let original_id = tabs.active.clone();
            let unrelated = browser.new_page("about:blank").await?;
            for action in [
                SwitchTabAction {
                    switch_tab: TabTarget::New,
                    url: None,
                    viewport_names: None,
                },
                SwitchTabAction {
                    switch_tab: TabTarget::Previous,
                    url: None,
                    viewport_names: None,
                },
                SwitchTabAction {
                    switch_tab: TabTarget::Original,
                    url: Some("**/quote".into()),
                    viewport_names: None,
                },
                SwitchTabAction {
                    switch_tab: TabTarget::New,
                    url: Some("{".into()),
                    viewport_names: None,
                },
            ] {
                ensure!(
                    tabs.switch(&browser, &action, &[]).await.is_err(),
                    "invalid tab switch succeeded"
                );
                ensure!(
                    tabs.active == original_id,
                    "failed tab switch changed the active tab"
                );
            }
            let original_runtime = Runtime::new(&browser, &page).await?;
            let close_original = CloseTabAction {
                close_tab: CloseTabTarget::Current,
                viewport_names: None,
            };
            ensure!(
                tabs.close(
                    &browser,
                    &original_runtime,
                    &TestAction::CloseTab(close_original.clone()),
                    &close_original
                )
                .await
                .is_err(),
                "closing original tab must fail"
            );
            page.close().await?;
            let error = tabs
                .switch(
                    &browser,
                    &SwitchTabAction {
                        switch_tab: TabTarget::Original,
                        url: None,
                        viewport_names: None,
                    },
                    &[],
                )
                .await
                .err()
                .expect("closed original tab must fail");
            ensure!(
                error.to_string().contains("closed"),
                "wrong closed-tab error: {error}"
            );
            unrelated.close().await?;
            Ok(())
        }
        .await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }
}
