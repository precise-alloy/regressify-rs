use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use chromiumoxide::{
    Browser, Command, Page,
    cdp::{
        browser_protocol::dom::NodeId,
        js_protocol::runtime::{EvaluateParams, EvaluateReturns},
    },
    error::CdpError,
    js::{Evaluation, EvaluationResult},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::types::Frame;

use super::cdp::Client;

pub(super) async fn evaluate_main_document(
    page: &Page,
    expression: impl Into<String>,
) -> Result<EvaluationResult> {
    // Let Chromium select the live main context; the client's cached ID can outlive navigation.
    let params = EvaluateParams::builder()
        .expression(expression)
        .await_promise(true)
        .return_by_value(true)
        .build()
        .map_err(anyhow::Error::msg)?;
    let response = page.execute(params).await?.result;
    if let Some(exception) = response.exception_details {
        return Err(CdpError::JavascriptException(Box::new(exception)).into());
    }
    Ok(EvaluationResult::new(response.result))
}

#[derive(Clone)]
struct Realm {
    session: String,
    frame: String,
}

#[derive(Clone)]
pub(crate) struct Runtime {
    client: Client,
    main_session: String,
    cache: Arc<Mutex<ContextCache>>,
    dialogs: Arc<super::dialogs::DialogState>,
}

struct ContextCache {
    frames: BTreeMap<(String, String), (String, i64)>,
    installed: BTreeSet<(String, i64)>,
    sessions: BTreeMap<String, String>,
    events: tokio::sync::broadcast::Receiver<super::cdp::Event>,
}

impl ContextCache {
    fn new(events: tokio::sync::broadcast::Receiver<super::cdp::Event>) -> Self {
        Self {
            frames: BTreeMap::new(),
            installed: BTreeSet::new(),
            sessions: BTreeMap::new(),
            events,
        }
    }

    fn drain(&mut self) {
        use tokio::sync::broadcast::error::TryRecvError;
        loop {
            match self.events.try_recv() {
                Ok(event)
                    if matches!(
                        event.method.as_str(),
                        "Page.frameNavigated"
                            | "Page.frameDetached"
                            | "Runtime.executionContextsCleared"
                            | "Runtime.executionContextDestroyed"
                            | "Target.detachedFromTarget"
                    ) =>
                {
                    self.frames.clear();
                    self.installed.clear();
                    if event.method == "Target.detachedFromTarget" {
                        self.sessions.clear();
                    }
                }
                Ok(_) => {}
                Err(TryRecvError::Lagged(_)) => {
                    self.frames.clear();
                    self.installed.clear();
                    self.sessions.clear();
                }
                Err(TryRecvError::Closed | TryRecvError::Empty) => break,
            }
        }
    }
}

#[derive(Clone)]
pub struct Target {
    runtime: Runtime,
    realm: Realm,
    parents: Vec<(Realm, String)>,
}

pub struct Evaluated(Value);

impl Evaluated {
    pub fn into_value<T: DeserializeOwned>(self) -> Result<T> {
        Ok(serde_json::from_value(self.0)?)
    }
}

pub struct Response<T> {
    pub result: T,
}

pub struct Element {
    pub node_id: NodeId,
}

pub(crate) struct NavigationWatch {
    events: tokio::sync::broadcast::Receiver<super::cdp::Event>,
    main_frame: String,
}

impl Runtime {
    pub async fn new(browser: &Browser, page: &Page) -> Result<Self> {
        let client = Client::connect(browser).await?;
        Self::with_client(client, page).await
    }

    pub(super) async fn with_client(client: Client, page: &Page) -> Result<Self> {
        let events = client.subscribe();
        let main_session = client.attach(page.target_id().as_ref()).await?;
        client
            .call(Some(&main_session), "Page.enable", json!({}))
            .await?;
        client
            .call(Some(&main_session), "Runtime.enable", json!({}))
            .await?;
        Ok(Self {
            client,
            main_session,
            cache: Arc::new(Mutex::new(ContextCache::new(events))),
            dialogs: Arc::new(super::dialogs::DialogState::default()),
        })
    }

    pub(super) fn client(&self) -> Client {
        self.client.clone()
    }

    pub(super) fn arm_dialog(&self, action: crate::types::DialogAction) -> Result<()> {
        self.dialogs.arm(action)
    }

    pub(super) fn ensure_no_pending_dialog(&self) -> Result<()> {
        self.dialogs.ensure_clear()
    }

    pub(super) fn navigation_cancelled(&self) -> bool {
        self.dialogs.navigation_cancelled()
    }

    pub(super) async fn close_page(&self, timeout: Duration) -> Result<()> {
        self.client
            .call_with_timeout(Some(&self.main_session), "Page.close", json!({}), timeout)
            .await?;
        Ok(())
    }

    pub(super) async fn with_dialog(
        &self,
        action: &crate::types::TestAction,
        timeout: Duration,
        work: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        self.dialogs
            .run(&self.client, &self.main_session, action, timeout, work)
            .await
    }

    async fn execute_main<C: Command>(&self, command: C, timeout: Duration) -> Result<C::Response> {
        let std::borrow::Cow::Borrowed(method) = command.identifier() else {
            bail!("CDP command identifier must be static");
        };
        let response = self
            .client
            .call_with_timeout(
                Some(&self.main_session),
                method,
                serde_json::to_value(command)?,
                timeout,
            )
            .await?;
        Ok(serde_json::from_value(response)?)
    }

    pub(super) async fn navigate_history(
        &self,
        action: &crate::types::NavigateAction,
        timeout: Duration,
    ) -> Result<()> {
        use crate::types::NavigationOperation;
        use chromiumoxide::cdp::browser_protocol::page::{
            GetNavigationHistoryParams, NavigateToHistoryEntryParams, ReloadParams,
        };
        match action.navigate {
            NavigationOperation::Reload => {
                self.execute_main(
                    ReloadParams::builder()
                        .ignore_cache(action.ignore_cache.unwrap_or(false))
                        .build(),
                    timeout,
                )
                .await?;
            }
            NavigationOperation::Back | NavigationOperation::Forward => {
                let history = self
                    .execute_main(GetNavigationHistoryParams {}, timeout)
                    .await?;
                let offset = if action.navigate == NavigationOperation::Back {
                    -1
                } else {
                    1
                };
                let index = history
                    .current_index
                    .checked_add(offset)
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or_else(|| anyhow!("No requested navigation history entry"))?;
                let entry = history
                    .entries
                    .get(index)
                    .ok_or_else(|| anyhow!("No requested navigation history entry"))?;
                self.execute_main(NavigateToHistoryEntryParams::new(entry.id), timeout)
                    .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn evaluate_main_document_with_timeout(
        &self,
        expression: impl Into<String>,
        timeout: Duration,
    ) -> Result<EvaluationResult> {
        // Long lazy-load waits must not inherit Chromiumoxide's periodic request eviction.
        let params = EvaluateParams::builder()
            .expression(expression)
            .await_promise(true)
            .return_by_value(true)
            .build()
            .map_err(anyhow::Error::msg)?;
        let response: EvaluateReturns = serde_json::from_value(
            self.client
                .call_with_timeout(
                    Some(&self.main_session),
                    "Runtime.evaluate",
                    serde_json::to_value(params)?,
                    timeout,
                )
                .await?,
        )?;
        if let Some(exception) = response.exception_details {
            return Err(CdpError::JavascriptException(Box::new(exception)).into());
        }
        Ok(EvaluationResult::new(response.result))
    }

    async fn root(&self) -> Result<Realm> {
        let tree = self
            .client
            .call(Some(&self.main_session), "Page.getFrameTree", json!({}))
            .await?;
        let frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Main frame is unavailable"))?;
        Ok(Realm {
            session: self.main_session.clone(),
            frame: frame.into(),
        })
    }

    async fn context(&self, realm: &Realm) -> Result<(String, i64)> {
        let key = (realm.session.clone(), realm.frame.clone());
        {
            let mut cache = self.cache.lock().await;
            cache.drain();
            if let Some(context) = cache.frames.get(&key) {
                return Ok(context.clone());
            }
        }
        let params = json!({"frameId": realm.frame, "worldName": "__regressify_runtime"});
        let mut session = realm.session.clone();
        let result = match self
            .client
            .call(Some(&session), "Page.createIsolatedWorld", params.clone())
            .await
        {
            Ok(result) => result,
            Err(_) => {
                session = if let Some(session) =
                    self.cache.lock().await.sessions.get(&realm.frame).cloned()
                {
                    session
                } else {
                    let session = self.client.attach(&realm.frame).await?;
                    self.client
                        .call(Some(&session), "Page.enable", json!({}))
                        .await?;
                    self.client
                        .call(Some(&session), "Runtime.enable", json!({}))
                        .await?;
                    self.cache
                        .lock()
                        .await
                        .sessions
                        .insert(realm.frame.clone(), session.clone());
                    session
                };
                self.client
                    .call(Some(&session), "Page.createIsolatedWorld", params)
                    .await?
            }
        };
        let context = result["executionContextId"]
            .as_i64()
            .ok_or_else(|| anyhow!("Frame execution context is unavailable"))?;
        let result = (session, context);
        let mut cache = self.cache.lock().await;
        cache.drain();
        cache.frames.insert(key, result.clone());
        Ok(result)
    }

    async fn evaluate(&self, realm: &Realm, expression: &str, by_value: bool) -> Result<Value> {
        let mut retries = 2;
        loop {
            match self.evaluate_in_context(realm, expression, by_value).await {
                // Navigation can destroy the cached world before its event arrives; the
                // expression never ran, so re-resolving the context is safe.
                Err(error)
                    if retries > 0
                        && format!("{error:#}")
                            .contains("Cannot find context with specified id") =>
                {
                    retries -= 1;
                    self.cache
                        .lock()
                        .await
                        .frames
                        .remove(&(realm.session.clone(), realm.frame.clone()));
                }
                result => return result,
            }
        }
    }

    async fn evaluate_in_context(
        &self,
        realm: &Realm,
        expression: &str,
        by_value: bool,
    ) -> Result<Value> {
        let (session, context) = self.context(realm).await?;
        let key = (session.clone(), context);
        let installed = self.cache.lock().await.installed.contains(&key);
        if !installed {
            let bootstrap = format!(
                r#"(() => {{
                const module = {{ exports: {{}} }};
                const exports = module.exports;
                {}
                const engine = new (module.exports.InjectedScript())(globalThis, {{
                    isUnderTest: false, sdkLanguage: 'javascript', testIdAttributeName: 'data-testid',
                    stableRafCount: 2, browserName: 'chromium', customEngines: []
                }});
                const deepElement = (element) => {{
                    let previous;
                    while (element && element !== previous && element.shadowRoot) {{
                        previous = element;
                        element = element.shadowRoot.activeElement || element;
                    }}
                    return element;
                }};
                globalThis.__regressifyDocument = new Proxy(document, {{ get(target, key) {{
                    if (key === 'querySelector') return selector => engine.querySelector(engine.parseSelector(selector), target, false);
                    if (key === 'querySelectorAll') return selector => engine.querySelectorAll(engine.parseSelector(selector), target);
                    if (key === 'activeElement') return deepElement(target.activeElement);
                    if (key === 'elementFromPoint') return (horizontal, vertical) => {{
                        let element = target.elementFromPoint(horizontal, vertical);
                        let previous;
                        while (element && element !== previous && element.shadowRoot) {{
                            previous = element;
                            element = element.shadowRoot.elementFromPoint(horizontal, vertical) || element;
                        }}
                        return element;
                    }};
                    const value = Reflect.get(target, key, target);
                    return typeof value === 'function' ? value.bind(target) : value;
                }} }});
            }})()"#,
                include_str!("vendor/injected.js")
            );
            let result = self
                .client
                .call(
                    Some(&session),
                    "Runtime.evaluate",
                    json!({
                        "contextId": context, "expression": bootstrap, "returnByValue": true
                    }),
                )
                .await?;
            check_evaluation(&result)?;
            self.cache.lock().await.installed.insert(key);
        }
        let expression = format!(
            "((document) => ({}))(globalThis.__regressifyDocument)",
            expression
        );
        let result = self
            .client
            .call(
                Some(&session),
                "Runtime.evaluate",
                json!({
                    "contextId": context, "expression": expression, "returnByValue": by_value,
                    "awaitPromise": true, "userGesture": true
                }),
            )
            .await?;
        check_evaluation(&result)?;
        Ok(result["result"].clone())
    }

    pub async fn target(&self, frame: &Option<Frame>, timeout: Duration) -> Result<Target> {
        tokio::time::timeout(timeout, self.resolve_target(frame, timeout))
            .await
            .map_err(|_| anyhow!("Frame resolution exceeded {}ms", timeout.as_millis()))?
    }

    async fn resolve_target(&self, frame: &Option<Frame>, timeout: Duration) -> Result<Target> {
        let selectors = match frame {
            Some(Frame::Single(selector)) => vec![selector.clone()],
            Some(Frame::Multiple(selectors)) if selectors.is_empty() => {
                bail!("Frame selector chain cannot be empty")
            }
            Some(Frame::Multiple(selectors)) => selectors.clone(),
            None => Vec::new(),
        };
        let started = std::time::Instant::now();
        loop {
            let mut realm = self.root().await?;
            let mut parents = Vec::new();
            let mut pending = None;
            for selector in &selectors {
                let expression = format!(
                    "document.querySelector({})",
                    serde_json::to_string(selector)?
                );
                let element = self.evaluate(&realm, &expression, false).await?;
                let Some(object) = element["objectId"].as_str() else {
                    pending = Some(selector);
                    break;
                };
                let (session, _) = self.context(&realm).await?;
                let described = self
                    .client
                    .call(
                        Some(&session),
                        "DOM.describeNode",
                        json!({"objectId": object}),
                    )
                    .await?;
                self.client
                    .call(
                        Some(&session),
                        "Runtime.releaseObject",
                        json!({"objectId": object}),
                    )
                    .await?;
                let node = &described["node"];
                anyhow::ensure!(
                    node["nodeName"] == "IFRAME" || node["nodeName"] == "FRAME",
                    "Frame selector '{}' must identify an iframe",
                    selector
                );
                let child = node["frameId"]
                    .as_str()
                    .or_else(|| node["contentDocument"]["frameId"].as_str());
                let Some(child) = child else {
                    pending = Some(selector);
                    break;
                };
                parents.push((realm.clone(), selector.clone()));
                realm = Realm {
                    session,
                    frame: child.to_string(),
                };
            }
            if let Some(selector) = pending {
                anyhow::ensure!(
                    started.elapsed() < timeout,
                    "Timed out waiting for frame '{}'",
                    selector
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            self.context(&realm).await?;
            return Ok(Target {
                runtime: self.clone(),
                realm,
                parents,
            });
        }
    }
}

fn frame_url(tree: &Value, frame_id: &str) -> Option<String> {
    let frame = &tree["frame"];
    if frame["id"].as_str() == Some(frame_id) {
        let url = frame["url"].as_str()?;
        let fragment = frame["urlFragment"].as_str().unwrap_or_default();
        return Some(format!("{url}{fragment}"));
    }
    tree["childFrames"]
        .as_array()?
        .iter()
        .find_map(|child| frame_url(child, frame_id))
}

fn check_evaluation(result: &Value) -> Result<()> {
    if let Some(details) = result.get("exceptionDetails") {
        bail!(
            "Browser evaluation failed: {}",
            details["exception"]["description"]
                .as_str()
                .or_else(|| details["text"].as_str())
                .unwrap_or("unknown JavaScript error")
        );
    }
    Ok(())
}

impl Target {
    pub(crate) async fn navigation_watch(&self) -> Result<NavigationWatch> {
        Ok(NavigationWatch {
            events: self.runtime.client.subscribe(),
            main_frame: self.runtime.root().await?.frame,
        })
    }

    pub(crate) async fn finish_navigation(
        &self,
        mut watch: NavigationWatch,
        timeout: Duration,
    ) -> Result<()> {
        if self.runtime.dialogs.navigation_cancelled() {
            return Ok(());
        }
        let mut deadline = tokio::time::Instant::now() + Duration::from_millis(75);
        let mut started = false;
        loop {
            let event = match tokio::time::timeout_at(deadline, watch.events.recv()).await {
                Ok(Ok(event)) => event,
                Ok(Err(_)) => bail!("Navigation event stream closed or overflowed"),
                Err(_) if started => {
                    bail!("Timed out waiting for navigation triggered by the action")
                }
                Err(_) => return Ok(()),
            };
            let frame = event.params["frameId"]
                .as_str()
                .or_else(|| event.params["frame"]["id"].as_str());
            if frame != Some(self.realm.frame.as_str()) && frame != Some(watch.main_frame.as_str())
            {
                continue;
            }
            match event.method.as_str() {
                "Page.frameStartedLoading" | "Page.frameStartedNavigating" if !started => {
                    started = true;
                    deadline = tokio::time::Instant::now() + timeout;
                }
                "Page.navigatedWithinDocument" => return Ok(()),
                "Page.frameStoppedLoading" if started => return Ok(()),
                "Page.frameNavigated" => {
                    if frame == Some(watch.main_frame.as_str()) {
                        return self
                            .runtime
                            .target(&None, timeout)
                            .await?
                            .wait_for_load(timeout)
                            .await;
                    }
                    return self.wait_for_load(timeout).await;
                }
                _ => {}
            }
        }
    }

    pub async fn evaluate(&self, script: impl Into<Evaluation>) -> Result<Evaluated> {
        let expression = match script.into() {
            Evaluation::Expression(expression) => expression.expression,
            Evaluation::Function(function) => format!("({})()", function.function_declaration),
        };
        let result = self
            .runtime
            .evaluate(&self.realm, &expression, true)
            .await?;
        Ok(Evaluated(
            result.get("value").cloned().unwrap_or(Value::Null),
        ))
    }

    pub async fn execute<CommandType: Command>(
        &self,
        command: CommandType,
    ) -> Result<Response<CommandType::Response>> {
        let method = command.identifier();
        let params = serde_json::to_value(&command)?;
        let session = if method.starts_with("Input.") {
            self.runtime.main_session.clone()
        } else {
            self.runtime.context(&self.realm).await?.0
        };
        let method: &'static str = match method {
            std::borrow::Cow::Borrowed(method) => method,
            std::borrow::Cow::Owned(_) => bail!("CDP command identifier must be static"),
        };
        let result = self
            .runtime
            .client
            .call(Some(&session), method, params)
            .await?;
        Ok(Response {
            result: serde_json::from_value(result)?,
        })
    }

    pub(super) fn input_connection(&self) -> (Client, String) {
        (
            self.runtime.client.clone(),
            self.runtime.main_session.clone(),
        )
    }

    pub async fn find_element(&self, selector: &str) -> Result<Element> {
        let result = self
            .runtime
            .evaluate(
                &self.realm,
                &format!(
                    "document.querySelector({})",
                    serde_json::to_string(selector)?
                ),
                false,
            )
            .await?;
        let object = result["objectId"]
            .as_str()
            .ok_or_else(|| anyhow!("Element '{}' is missing", selector))?;
        let (session, _) = self.runtime.context(&self.realm).await?;
        self.runtime
            .client
            .call(Some(&session), "DOM.getDocument", json!({}))
            .await?;
        let node = self
            .runtime
            .client
            .call(
                Some(&session),
                "DOM.requestNode",
                json!({"objectId": object}),
            )
            .await?;
        self.runtime
            .client
            .call(
                Some(&session),
                "Runtime.releaseObject",
                json!({"objectId": object}),
            )
            .await?;
        Ok(Element {
            node_id: serde_json::from_value(node["nodeId"].clone())?,
        })
    }

    pub async fn choose_files(
        &self,
        files: Vec<String>,
        horizontal: f64,
        vertical: f64,
        config: &super::actionability::ActionabilityConfig,
    ) -> Result<()> {
        let (session, _) = self.runtime.context(&self.realm).await?;
        let mut events = self.runtime.client.subscribe();
        let sessions = if session == self.runtime.main_session {
            vec![session]
        } else {
            vec![self.runtime.main_session.clone(), session]
        };
        for session in &sessions {
            self.runtime
                .client
                .call(
                    Some(session),
                    "Page.setInterceptFileChooserDialog",
                    json!({"enabled": true}),
                )
                .await?;
        }
        let result = async {
            super::actionability::cdp_click(self, horizontal, vertical, config).await?;
            let event =
                tokio::time::timeout(Duration::from_millis(config.element_timeout_ms), async {
                    loop {
                        let event = events.recv().await.map_err(|_| {
                            anyhow!("File chooser event stream closed or overflowed")
                        })?;
                        if event.method == "Page.fileChooserOpened"
                            && event.params["frameId"] == self.realm.frame
                        {
                            return Ok::<_, anyhow::Error>(event);
                        }
                    }
                })
                .await
                .map_err(|_| {
                    anyhow!("Timed out waiting for the upload control to open a file chooser")
                })??;
            anyhow::ensure!(
                event.params.get("backendNodeId").is_some(),
                "File chooser did not identify its input element"
            );
            self.runtime
                .client
                .call(
                    event.session_id.as_deref(),
                    "DOM.setFileInputFiles",
                    json!({
                        "files": files, "backendNodeId": event.params["backendNodeId"]
                    }),
                )
                .await?;
            Ok(())
        }
        .await;
        for session in &sessions {
            let disabled = self
                .runtime
                .client
                .call(
                    Some(session),
                    "Page.setInterceptFileChooserDialog",
                    json!({"enabled": false}),
                )
                .await;
            if result.is_ok() {
                disabled?;
            }
        }
        result
    }

    pub async fn goto(&self, url: &str) -> Result<()> {
        let (session, _) = self.runtime.context(&self.realm).await?;
        let result = self
            .runtime
            .client
            .call(
                Some(&session),
                "Page.navigate",
                json!({"url": url, "frameId": self.realm.frame}),
            )
            .await?;
        if let Some(error) = result["errorText"].as_str() {
            if error == "net::ERR_ABORTED" && self.runtime.dialogs.navigation_cancelled() {
                return Ok(());
            }
            bail!("Navigation failed: {}", error);
        }
        Ok(())
    }

    pub(crate) async fn navigate(
        &self,
        url: &str,
        options: &crate::types::GotoParameters,
    ) -> Result<()> {
        use crate::types::NavigationWait;
        let timeout = navigation_timeout(options);
        crate::diagnostics::runtime::navigation(url, timeout.as_millis() as u64, async {
            let mut network_session = None;
            let result = tokio::time::timeout(timeout, async {
                let (session, _) = self.runtime.context(&self.realm).await?;
                let wait = options.wait_until.unwrap_or_default();
                let mut events = self.runtime.client.subscribe();
                self.runtime
                    .client
                    .call(Some(&session), "Network.enable", json!({}))
                    .await?;
                network_session = Some(session.clone());
                let mut parameters = json!({"url": url, "frameId": self.realm.frame});
                if let Some(referer) = &options.referer {
                    parameters["referrer"] = json!(referer);
                }
                let response = self
                    .runtime
                    .client
                    .call_with_timeout(Some(&session), "Page.navigate", parameters, timeout)
                    .await?;
                if let Some(error) = response["errorText"].as_str() {
                    bail!("Navigation failed: {}", error);
                }
                let mut redirects = 0;
                match wait {
                    NavigationWait::Commit => {}
                    NavigationWait::DomContentLoaded => self.wait_for_ready(timeout).await?,
                    NavigationWait::Load => self.wait_for_load(timeout).await?,
                    NavigationWait::NetworkIdle => {
                        wait_for_network_idle(
                            &mut events,
                            &session,
                            &self.realm.frame,
                            &mut redirects,
                        )
                        .await?;
                        self.wait_for_ready(timeout).await?;
                    }
                }
                if wait != NavigationWait::NetworkIdle {
                    while let Ok(event) = events.try_recv() {
                        if event.session_id.as_deref() == Some(session.as_str()) {
                            record_navigation_response(
                                &event.method,
                                &event.params,
                                &self.realm.frame,
                                &mut redirects,
                            );
                        }
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
            .await
            .map_err(|_| anyhow!("Navigation exceeded {}ms", timeout.as_millis()));
            if let Some(session) = network_session {
                let _ = self
                    .runtime
                    .client
                    .call(Some(&session), "Network.disable", json!({}))
                    .await;
            }
            result?
        })
        .await
    }

    pub async fn url(&self) -> Result<Option<String>> {
        // Redirects replace JavaScript contexts; frame metadata does not depend on them.
        let tree = self
            .runtime
            .client
            .call(Some(&self.realm.session), "Page.getFrameTree", json!({}))
            .await?;
        if let Some(url) = frame_url(&tree["frameTree"], &self.realm.frame) {
            return Ok(Some(url));
        }
        let session = {
            let mut cache = self.runtime.cache.lock().await;
            cache.drain();
            cache.sessions.get(&self.realm.frame).cloned()
        };
        let session = match session {
            Some(session) => session,
            None => match self.runtime.context(&self.realm).await {
                Ok((session, _)) => session,
                Err(_) => return Ok(None),
            },
        };
        let tree = self
            .runtime
            .client
            .call(Some(&session), "Page.getFrameTree", json!({}))
            .await?;
        Ok(frame_url(&tree["frameTree"], &self.realm.frame))
    }

    pub async fn wait_for_ready(&self, timeout: Duration) -> Result<()> {
        self.wait_for_document("document.readyState !== 'loading'", timeout)
            .await
    }

    pub(crate) async fn wait_for_load(&self, timeout: Duration) -> Result<()> {
        self.wait_for_document("document.readyState === 'complete'", timeout)
            .await
    }

    async fn wait_for_document(&self, expression: &str, timeout: Duration) -> Result<()> {
        let started = std::time::Instant::now();
        loop {
            match self.evaluate(expression).await {
                Ok(result) if result.0 == true => return Ok(()),
                _ if started.elapsed() >= timeout => {
                    bail!("Timed out waiting for frame navigation readiness")
                }
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }

    pub async fn scroll_frames(&self) -> Result<()> {
        for (realm, selector) in &self.parents {
            self.runtime.evaluate(realm, &format!("(() => {{ const element = document.querySelector({}); if (!element) throw new Error('Frame disappeared'); element.scrollIntoView({{block:'center', inline:'center', behavior:'instant'}}); }})()", serde_json::to_string(selector)?), true).await?;
        }
        Ok(())
    }

    pub async fn frame_offset(&self) -> Result<(f64, f64)> {
        self.project_point(0.0, 0.0, false).await
    }

    pub async fn check_frame_coverage(&self, horizontal: f64, vertical: f64) -> Result<()> {
        self.project_point(horizontal, vertical, true)
            .await
            .map(|_| ())
    }

    pub fn has_frames(&self) -> bool {
        !self.parents.is_empty()
    }

    async fn project_point(
        &self,
        mut horizontal: f64,
        mut vertical: f64,
        check: bool,
    ) -> Result<(f64, f64)> {
        for (realm, selector) in self.parents.iter().rev() {
            let expression = format!(
                r#"(() => {{
                const frame = document.querySelector({selector});
                if (!frame) throw new Error('Frame disappeared');
                const rectangle = frame.getBoundingClientRect();
                for (let ancestor = frame; ancestor; ancestor = ancestor.parentElement || ancestor.getRootNode?.()?.host || null) {{
                    const style = getComputedStyle(ancestor);
                    if (ancestor.inert === true || ancestor.hasAttribute?.('inert')
                        || style.visibility === 'hidden' || style.visibility === 'collapse'
                        || style.display === 'none' || style.contentVisibility === 'hidden'
                        || +style.opacity === 0)
                        throw new Error('Frame is hidden or inert: ' + {selector});
                }}
                if (!rectangle.width || !rectangle.height) throw new Error('Frame has zero size');
                const horizontal = rectangle.left + frame.clientLeft + {horizontal};
                const vertical = rectangle.top + frame.clientTop + {vertical};
                if ({check}) {{
                    const covering = document.elementFromPoint(horizontal, vertical);
                    let inside = false;
                    for (let current = covering; current; current = current.parentElement || current.getRootNode?.()?.host || null) {{
                        if (current === frame || frame.contains?.(current)) {{
                            inside = true;
                            break;
                        }}
                    }}
                    if (!inside)
                        throw new Error('Frame ' + {selector} + ' is obscured by ' + (covering ? covering.tagName + '#' + covering.id : 'viewport clipping'));
                }}
                return {{horizontal, vertical}};
            }})()"#,
                selector = serde_json::to_string(selector)?
            );
            let result = self.runtime.evaluate(realm, &expression, true).await?;
            horizontal = result["value"]["horizontal"]
                .as_f64()
                .ok_or_else(|| anyhow!("Invalid frame horizontal offset"))?;
            vertical = result["value"]["vertical"]
                .as_f64()
                .ok_or_else(|| anyhow!("Invalid frame vertical offset"))?;
        }
        Ok((horizontal, vertical))
    }
}

fn navigation_timeout(options: &crate::types::GotoParameters) -> Duration {
    Duration::from_millis(options.timeout.filter(|value| *value > 0).unwrap_or(60_000))
}

async fn wait_for_network_idle(
    events: &mut tokio::sync::broadcast::Receiver<super::cdp::Event>,
    session: &str,
    frame: &str,
    redirects: &mut u64,
) -> Result<()> {
    let mut pending = std::collections::HashSet::new();
    let mut idle_deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        let received = if pending.is_empty() {
            match tokio::time::timeout_at(idle_deadline, events.recv()).await {
                Ok(result) => result,
                Err(_) => break,
            }
        } else {
            events.recv().await
        };
        let event = received.map_err(|_| anyhow!("Network event stream closed or overflowed"))?;
        if event.session_id.as_deref() != Some(session) {
            continue;
        }
        record_navigation_response(&event.method, &event.params, frame, redirects);
        if let Some(request) = event.params["requestId"].as_str() {
            match event.method.as_str() {
                "Network.requestWillBeSent" => {
                    pending.insert(request.to_string());
                }
                "Network.loadingFinished" | "Network.loadingFailed" => {
                    pending.remove(request);
                    if pending.is_empty() {
                        idle_deadline = tokio::time::Instant::now() + Duration::from_millis(500);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn record_navigation_response(
    method: &str,
    parameters: &serde_json::Value,
    frame: &str,
    redirects: &mut u64,
) {
    use crate::diagnostics::{record::*, runtime as diagnostics};
    if parameters["type"].as_str() != Some("Document")
        || parameters["frameId"].as_str() != Some(frame)
    {
        return;
    }
    if method == "Network.requestWillBeSent" && parameters["redirectResponse"].is_object() {
        *redirects += 1;
        diagnostics::event(
            Stage::Navigation,
            DiagnosticEvent::Decision,
            Severity::Debug,
            [(Field::RedirectCount, FieldValue::Number(*redirects))],
        );
    }
    if method == "Network.responseReceived"
        && let Some(status) = parameters["response"]["status"].as_f64().filter(|status| {
            status.is_finite() && (100.0..=599.0).contains(status) && status.fract() == 0.0
        })
    {
        diagnostics::event(
            Stage::Navigation,
            DiagnosticEvent::Decision,
            Severity::Debug,
            [(Field::StatusCode, FieldValue::Number(status as u64))],
        );
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn navigation_response_diagnostics_only_include_matching_document_numbers() {
        use crate::diagnostics::{journal::JournalReader, record::*, runtime as diagnostics};
        let root = tempfile::tempdir().unwrap();
        let operation =
            diagnostics::Operation::start(root.path(), OperationKind::Capture, None, None, None);
        diagnostics::scope(Some(operation.context()), async {
            let mut redirects = 0;
            super::record_navigation_response("Network.requestWillBeSent", &serde_json::json!({"type":"Document", "frameId":"main", "redirectResponse":{"url":"https://user:private@example.test/?secret=value"}}), "main", &mut redirects);
            for (kind, frame, status) in [("Document", "main", 200.0), ("Image", "main", 201.0), ("Document", "other", 202.0), ("Document", "main", 900.0)] {
                super::record_navigation_response("Network.responseReceived", &serde_json::json!({"type":kind, "frameId":frame, "response":{"status":status, "url":"private-url", "headers":{"Authorization":"synthetic-secret"}}}), "main", &mut redirects);
            }
            assert_eq!(redirects, 1);
        }).await;
        let reference = operation.finish(Outcome::Success).await;
        let mut navigation = Vec::new();
        JournalReader::open(root.path(), &reference.operation_id)
            .unwrap()
            .visit(|record| {
                assert!(!serde_json::to_string(&record).unwrap().contains("private"));
                assert!(
                    !serde_json::to_string(&record)
                        .unwrap()
                        .contains("synthetic-secret")
                );
                if record.stage == Stage::Navigation {
                    navigation.push(record);
                }
            })
            .unwrap();
        assert_eq!(navigation.len(), 2);
        assert_eq!(
            navigation[1].fields.get(&Field::StatusCode),
            Some(&FieldValue::Number(200))
        );
    }

    #[test]
    fn navigation_timeout_defaults_to_sixty_seconds_without_changing_element_waits() {
        for timeout in [None, Some(0)] {
            let options = crate::types::GotoParameters {
                timeout,
                ..Default::default()
            };
            assert_eq!(super::navigation_timeout(&options), Duration::from_secs(60));
        }
        assert_eq!(
            crate::config::ResolvedConfig::default().element_timeout_ms,
            30_000
        );
    }

    #[test]
    fn navigation_timeout_preserves_positive_overrides() {
        for timeout in [1, 30_000, 60_000, 90_000] {
            let options = crate::types::GotoParameters {
                timeout: Some(timeout),
                ..Default::default()
            };
            assert_eq!(
                super::navigation_timeout(&options),
                Duration::from_millis(timeout)
            );
        }
    }

    #[tokio::test]
    async fn network_idle_does_not_reset_quiet_window_on_unrelated_cdp_events() {
        let (sender, mut receiver) = tokio::sync::broadcast::channel(32);
        let ticker = sender.clone();
        let task = tokio::spawn(async move {
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(120)).await;
                let _ = ticker.send(super::super::cdp::Event {
                    method: "Runtime.consoleAPICalled".into(),
                    session_id: Some("session".into()),
                    params: serde_json::json!({}),
                });
            }
        });
        let mut redirects = 0;
        tokio::time::timeout(
            Duration::from_millis(850),
            super::wait_for_network_idle(&mut receiver, "session", "main", &mut redirects),
        )
        .await
        .expect("unrelated CDP events must not stall network idle")
        .unwrap();
        task.abort();
    }

    #[test]
    fn frame_urls_follow_target_identity_and_preserve_fragments() {
        let tree = serde_json::json!({
            "frame": {"id": "main", "url": "https://example.com/?tab=one", "urlFragment": "#top"},
            "childFrames": [
                {"frame": {"id": "sibling", "url": "about:blank"}},
                {
                    "frame": {"id": "child", "url": "https://auth.example.com/login"},
                    "childFrames": [{
                        "frame": {"id": "nested", "url": "https://other.example.com/", "urlFragment": "#ready"}
                    }]
                }
            ]
        });
        for (frame, expected) in [
            ("main", "https://example.com/?tab=one#top"),
            ("sibling", "about:blank"),
            ("child", "https://auth.example.com/login"),
            ("nested", "https://other.example.com/#ready"),
        ] {
            assert_eq!(super::frame_url(&tree, frame).as_deref(), Some(expected));
        }
        assert_eq!(super::frame_url(&tree, "missing"), None);
    }

    #[test]
    fn frame_urls_do_not_substitute_an_unavailable_target() {
        for tree in [
            serde_json::Value::Null,
            serde_json::json!({"frame": {"id": "main"}}),
            serde_json::json!({"frame": {"id": "main", "url": 123}}),
            serde_json::json!({"frame": {"id": "other", "url": "https://example.com/"}}),
        ] {
            assert_eq!(super::frame_url(&tree, "main"), None);
        }
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn url_reads_do_not_require_a_live_execution_context() -> anyhow::Result<()> {
        let (mut browser, handler, directory) = super::super::browser_tests::launch_browser(
            "url-context",
            super::super::browsers::LaunchOptions::default(),
        )
        .await?;
        let result: anyhow::Result<()> = async {
            let page = browser.new_page("about:blank").await?;
            let runtime = super::Runtime::new(&browser, &page).await?;
            let target = runtime
                .target(&None, std::time::Duration::from_secs(5))
                .await?;
            {
                let mut cache = runtime.cache.lock().await;
                cache.drain();
                // Navigation can destroy a cached context before its invalidation event arrives.
                cache.frames.insert(
                    (target.realm.session.clone(), target.realm.frame.clone()),
                    (target.realm.session.clone(), 1_000_000),
                );
            }
            anyhow::ensure!(
                target.url().await?.as_deref() == Some("about:blank"),
                "URL read failed with a stale execution context"
            );
            Ok(())
        }
        .await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }

    #[test]
    fn context_cache_invalidates_after_navigation_or_event_overflow() {
        let (sender, receiver) = tokio::sync::broadcast::channel(1);
        let mut cache = ContextCache::new(receiver);
        let key = ("session".into(), "frame".into());
        cache.frames.insert(key.clone(), ("session".into(), 7));
        cache.installed.insert(("session".into(), 7));
        sender
            .send(super::super::cdp::Event {
                method: "Page.frameNavigated".into(),
                session_id: Some("session".into()),
                params: json!({}),
            })
            .unwrap();
        cache.drain();
        assert!(cache.frames.is_empty() && cache.installed.is_empty());
        cache.frames.insert(key, ("session".into(), 8));
        for _ in 0..2 {
            sender
                .send(super::super::cdp::Event {
                    method: "Runtime.consoleAPICalled".into(),
                    session_id: None,
                    params: json!({}),
                })
                .unwrap();
        }
        cache.drain();
        assert!(cache.frames.is_empty());
    }
    use super::*;

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn evaluation_recovers_when_navigation_destroys_a_cached_context() -> Result<()> {
        let site = super::super::browser_tests::Site::new(|request| {
            let _ = request.respond(
                tiny_http::Response::from_string("<body><div id='ready'>data</div></body>")
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap(),
                    ),
            );
        })?;
        let (mut browser, handler, directory) =
            super::super::browser_tests::launch_browser("runtime-context", Default::default())
                .await?;
        let result: Result<()> = async {
            let page = browser.new_page(site.origin.as_str()).await?;
            let runtime = Runtime::new(&browser, &page).await?;
            let root = runtime.target(&None, Duration::from_secs(5)).await?;
            root.evaluate("true").await?;
            // Suppress invalidation events so the cache keeps the world navigation destroys.
            for method in ["Page.disable", "Runtime.disable"] {
                runtime
                    .client
                    .call(Some(&runtime.main_session), method, json!({}))
                    .await?;
            }
            page.goto(format!("{}/next", site.origin)).await?;
            let found: bool = root
                .evaluate("!!document.querySelector('#ready')")
                .await?
                .into_value()?;
            anyhow::ensure!(found, "evaluation did not reach the navigated document");
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
    async fn selectors_resolve_text_xpath_shadow_dom_and_cross_origin_frames() -> Result<()> {
        use super::super::browser_tests::Site;
        use tiny_http::{Header, Response};
        let inner = Site::new(|request| {
            let _ = request.respond(Response::from_string("<button id='inside'>Frame button</button><input type=file id=upload><button id=chooser onclick=\"document.querySelector('#hidden-file').click()\">Upload</button><input type=file id=hidden-file hidden><script>document.querySelector('#inside').onclick=()=>document.body.dataset.clicked='yes';</script>")
                .with_header(Header::from_bytes("Content-Type", "text/html").unwrap()));
        })?;
        let inner_url = inner.origin.replace("127.0.0.1", "localhost");
        let outer = Site::new(move |request| {
            let html = format!(
                "<button id=outside>Outside</button><div id=host></div><iframe src='{inner_url}'></iframe><script>document.querySelector('#host').attachShadow({{mode:'open'}}).innerHTML='<button>Shadow button</button>';</script>"
            );
            let _ = request.respond(
                Response::from_string(html)
                    .with_header(Header::from_bytes("Content-Type", "text/html").unwrap()),
            );
        })?;
        let (mut browser, handler, directory) =
            super::super::browser_tests::launch_browser("selectors", Default::default()).await?;
        let result: Result<()> = async {
            let page = browser.new_page(outer.origin.as_str()).await?;
            let runtime = Runtime::new(&browser, &page).await?;
            let root = runtime.target(&None, Duration::from_secs(5)).await?;
            for selector in ["text=Shadow button", "button:has-text('Shadow')", "xpath=//button[@id='outside']", "css=#host >> text=Shadow button"] {
                let found: bool = root.evaluate(format!("!!document.querySelector({})", serde_json::to_string(selector)?)).await?.into_value()?;
                anyhow::ensure!(found, "selector not resolved: {selector}");
            }
            let frame = runtime.target(&Some(Frame::Single("iframe".into())), Duration::from_secs(5)).await?;
            let found: String = frame.evaluate("document.querySelector('text=Frame button').id").await?.into_value()?;
            anyhow::ensure!(found == "inside", "cross-origin frame did not resolve");
            frame.scroll_frames().await?;
            frame.check_frame_coverage(30.0, 20.0).await?;
            let config = super::super::actionability::ActionabilityConfig { element_timeout_ms: 2000, actionability_timeout_ms: 1000, poll_interval_ms: 25 };
            super::super::actionability::ensure_actionable(&root, "#host", &None, &config).await?;
            let position = super::super::actionability::ensure_actionable(&frame, "text=Frame button", &None, &config).await?;
            let offset = frame.frame_offset().await?;
            super::super::actionability::cdp_click(&frame, offset.0 + position.center_x, offset.1 + position.center_y, &config).await?;
            let clicked: String = frame.evaluate("document.body.dataset.clicked").await?.into_value()?;
            anyhow::ensure!(clicked == "yes", "cross-origin CDP click missed its element");
            let upload = tempfile::NamedTempFile::new()?;
            let files = vec![upload.path().to_string_lossy().to_string()];
            let element = frame.find_element("#upload").await?;
            let command = chromiumoxide::cdp::browser_protocol::dom::SetFileInputFilesParams::builder()
                .node_id(element.node_id).files(files.clone()).build().map_err(|error| anyhow!(error))?;
            frame.execute(command).await?;
            let count: usize = frame.evaluate("document.querySelector('#upload').files.length").await?.into_value()?;
            anyhow::ensure!(count == 1, "cross-origin direct file upload failed");
            let position = super::super::actionability::ensure_actionable(&frame, "#chooser", &None, &config).await?;
            frame.choose_files(files, offset.0 + position.center_x, offset.1 + position.center_y, &config).await?;
            let count: usize = frame.evaluate("document.querySelector('#hidden-file').files.length").await?.into_value()?;
            anyhow::ensure!(count == 1, "cross-origin button file chooser failed");
            frame.evaluate(format!("(() => {{ const child = document.createElement('iframe'); child.id = 'nested'; child.src = {}; document.body.appendChild(child); }})()", serde_json::to_string(&inner.origin)?)).await?;
            let nested = runtime.target(&Some(Frame::Multiple(vec!["iframe".into(), "#nested".into()])), Duration::from_secs(5)).await?;
            let nested_position = super::super::actionability::ensure_actionable(&nested, "#inside", &None, &config).await?;
            let nested_offset = nested.frame_offset().await?;
            super::super::actionability::cdp_click(&nested, nested_offset.0 + nested_position.center_x, nested_offset.1 + nested_position.center_y, &config).await?;
            let nested_clicked: String = nested.evaluate("document.body.dataset.clicked").await?.into_value()?;
            anyhow::ensure!(nested_clicked == "yes", "nested frame click missed");
            root.evaluate("(() => { const element = document.createElement('div'); element.id = 'later'; element.textContent = 'later'; element.style.display = 'none'; document.body.append(element); setTimeout(() => element.style.display = 'block', 200); })()").await?;
            let started = std::time::Instant::now();
            super::super::actionability::wait_for_selector(&root, "#later", &None, &config, crate::diagnostics::record::Stage::ActionsFrames).await?;
            anyhow::ensure!(started.elapsed() >= Duration::from_millis(100), "selector wait returned before visibility");
            root.evaluate("(() => { const overlay = document.createElement('div'); overlay.id = 'overlay'; overlay.style.cssText = 'position:fixed;inset:0;z-index:99999;background:white'; document.body.append(overlay); })()").await?;
            let error = super::super::actionability::ensure_actionable(&frame, "#inside", &None, &config).await.err()
                .ok_or_else(|| anyhow!("parent overlay was not detected"))?;
            anyhow::ensure!(error.to_string().contains("obscured"), "unexpected overlay failure: {error}");
            root.evaluate("document.querySelector('#overlay').remove()").await?;
            frame.goto(&format!("{}/next", inner.origin)).await?;
            frame.wait_for_ready(Duration::from_secs(5)).await?;
            anyhow::ensure!(frame.url().await?.is_some_and(|url| url.ends_with("/next")), "iframe navigation did not target the frame");
            anyhow::ensure!(page.url().await?.as_deref() == Some(format!("{}/", outer.origin).as_str()), "iframe navigation changed the main page");
            Ok(())
        }.await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }
}
