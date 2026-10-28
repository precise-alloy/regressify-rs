use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use chromiumoxide::{
    Page,
    cdp::browser_protocol::page::{
        AddScriptToEvaluateOnNewDocumentParams, RemoveScriptToEvaluateOnNewDocumentParams,
        ScriptIdentifier,
    },
};

use crate::{
    config::{self, ResolvedConfig},
    diagnostics::{
        record::{Field, FieldValue, SafeValue, Stage},
        runtime as diagnostics,
    },
    types::TestScenario,
};

use super::runtime::evaluate_main_document;

pub(crate) struct DocumentHooks {
    marker: String,
    context: serde_json::Value,
    setup: String,
    simplifier: Option<super::image_simplification::ImageSimplifier>,
    script: Option<String>,
    before_script: Option<String>,
    ready_script: Option<String>,
    timeout: Duration,
}

pub(crate) fn validate_lifecycle_script(root: &Path, script: &str) -> Result<()> {
    config::resolve_workspace_file(root, script, "browser hook")?;
    Ok(())
}

impl DocumentHooks {
    pub fn load(root: &Path, scenario: &TestScenario, config: &ResolvedConfig) -> Result<Self> {
        diagnostics::stage_sync(
            Stage::StateHooks,
            [(Field::ActionType, FieldValue::Kind(SafeValue::Hook))],
            || Self::load_inner(root, scenario, config),
        )
    }

    fn load_inner(root: &Path, scenario: &TestScenario, config: &ResolvedConfig) -> Result<Self> {
        let read = |path: &str| -> Result<Option<String>> {
            config::resolve_optional_workspace_file(root, path, "document hook")?
                .map(std::fs::read_to_string)
                .transpose()
                .map_err(Into::into)
        };
        let mut css = if config.use_css_override {
            read(
                config
                    .css_override_path
                    .as_deref()
                    .unwrap_or("common/_override.css"),
            )?
            .unwrap_or_default()
        } else {
            String::new()
        };
        for (selectors, declaration) in [
            (&config.hide_selectors, "visibility: hidden !important"),
            (&config.remove_selectors, "display: none !important"),
        ] {
            for selector in selectors {
                let escaped = super::actionability::escape_css_rule(selector);
                css.push_str(&format!("\n{escaped} {{ {declaration}; }}"));
            }
        }
        let context = serde_json::json!({
            "requiredLogin": config.required_login,
            "scenario": { "id": scenario.id, "label": scenario.label,
                "description": scenario.description, "url": scenario.url,
                "requiredLogin": config.required_login }
        });
        let setup = format!(
            "(() => {{ const style = document.createElement('style'); style.textContent = {}; \
             (document.head || document.documentElement).appendChild(style); }})()",
            serde_json::to_string(&css)?
        );
        let read_lifecycle_script = |script: &str| -> Result<String> {
            let path = config::resolve_workspace_file(root, script, "browser hook")?;
            std::fs::read_to_string(path).context("Cannot read browser lifecycle hook")
        };
        Ok(Self {
            marker: format!("__regressify_hooks_{}", uuid::Uuid::new_v4().simple()),
            context,
            setup,
            simplifier: super::image_simplification::ImageSimplifier::from_config(config)?,
            script: read(
                config
                    .js_on_ready_path
                    .as_deref()
                    .unwrap_or("common/_on-ready.js"),
            )?,
            before_script: scenario
                .on_before_script
                .as_deref()
                .map(read_lifecycle_script)
                .transpose()?,
            ready_script: scenario
                .on_ready_script
                .as_deref()
                .map(read_lifecycle_script)
                .transpose()?,
            timeout: Duration::from_millis(config.element_timeout_ms),
        })
    }

    pub fn with_context(mut self, context: serde_json::Value) -> Self {
        self.context = context;
        self
    }

    fn context_setup(&self) -> String {
        format!(
            "window.__REGRESSIFY__ = {}; window.regressify = window.__REGRESSIFY__;",
            self.context
        )
    }

    pub async fn install_before(&self, page: &Page) -> Result<Option<ScriptIdentifier>> {
        let Some(script) = &self.before_script else {
            return Ok(None);
        };
        let marker = serde_json::to_string(&format!("{}_before", self.marker))?;
        // Keep document-start errors observable without unhandled promise rejections.
        let source = format!(
            "(() => {{ if (window !== window.top) return; {} \
             try {{ window[{marker}] = Promise.resolve((0, eval)({})).then(\
                 () => ({{ ok: true }}), error => ({{ ok: false, error: String(error) }})); \
             }} catch (error) {{ window[{marker}] = Promise.resolve({{ ok: false, error: String(error) }}); }} \
             }})()",
            self.context_setup(),
            serde_json::to_string(script)?
        );
        let command = AddScriptToEvaluateOnNewDocumentParams::builder()
            .source(source)
            .build()
            .map_err(anyhow::Error::msg)?;
        let response = tokio::time::timeout(self.timeout, page.execute(command))
            .await
            .context("Timed out installing onBeforeScript")??;
        Ok(Some(response.identifier.clone()))
    }

    pub async fn complete_before(
        &self,
        page: &Page,
        identifier: Option<ScriptIdentifier>,
    ) -> Result<()> {
        let Some(identifier) = identifier else {
            return Ok(());
        };
        // Initial-navigation hooks must not leak into later goto actions or sibling frames.
        tokio::time::timeout(
            self.timeout,
            page.execute(RemoveScriptToEvaluateOnNewDocumentParams::new(identifier)),
        )
        .await
        .context("Timed out removing onBeforeScript registration")??;
        let marker = serde_json::to_string(&format!("{}_before", self.marker))?;
        self.evaluate_lifecycle_script(
            page,
            &format!(
                "(async () => {{ const pending = window[{marker}]; \
                 if (!pending) throw new Error('onBeforeScript did not execute in the initial document'); \
                 const result = await pending; delete window[{marker}]; \
                 if (!result.ok) throw new Error(result.error); }})()"
            ),
            SafeValue::Before,
        )
        .await
    }

    async fn evaluate_lifecycle_script(
        &self,
        page: &Page,
        script: &str,
        phase: SafeValue,
    ) -> Result<()> {
        diagnostics::stage(
            Stage::StateHooks,
            [
                (Field::ActionType, FieldValue::Kind(SafeValue::Hook)),
                (Field::Condition, FieldValue::Kind(phase)),
                (
                    Field::TimeoutMs,
                    FieldValue::Number(self.timeout.as_millis() as u64),
                ),
            ],
            async {
                tokio::time::timeout(self.timeout, evaluate_main_document(page, script))
                    .await
                    .context("Browser lifecycle hook exceeded its timeout")??;
                Ok(())
            },
        )
        .await
    }

    pub async fn apply(&self, page: &Page) -> Result<()> {
        diagnostics::stage(
            Stage::StateHooks,
            [
                (Field::ActionType, FieldValue::Kind(SafeValue::Hook)),
                (Field::State, FieldValue::Kind(SafeValue::Ready)),
            ],
            self.apply_inner(page),
        )
        .await
    }

    async fn apply_inner(&self, page: &Page) -> Result<()> {
        tokio::time::timeout(self.timeout, async {
            super::page_stability::wait_for_dom_ready(page, self.timeout).await?;
            let marker = serde_json::to_string(&self.marker)?;
            let completed: bool =
                evaluate_main_document(page, format!("window[{marker}] === true"))
                    .await?
                    .into_value()?;
            if !completed {
                evaluate_main_document(page, self.context_setup()).await?;
                evaluate_main_document(page, self.setup.as_str()).await?;
                // Install the image filter before user hooks so they see the final
                // DOM; screenshot preparation re-installs it if a page removes it.
                if let Some(simplifier) = &self.simplifier {
                    simplifier.ensure(page).await?;
                }
                if let Some(script) = &self.script {
                    evaluate_main_document(page, script.as_str()).await?;
                }
                if let Some(script) = &self.ready_script {
                    self.evaluate_lifecycle_script(page, script, SafeValue::Ready)
                        .await?;
                }
                evaluate_main_document(page, format!("window[{marker}] = true")).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|_| anyhow::anyhow!("Timed out applying document CSS/on-ready script"))??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_hooks_validate_without_node_dependencies() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        std::fs::write(
            workspace.path().join("hook.js"),
            "document.documentElement.dataset.hook = 'ready';",
        )?;
        validate_lifecycle_script(workspace.path(), "hook.js")?;
        let scenario = TestScenario {
            on_before_script: Some("hook.js".into()),
            on_ready_script: Some("hook.js".into()),
            ..Default::default()
        };
        let hooks = DocumentHooks::load(workspace.path(), &scenario, &ResolvedConfig::default())?;
        assert!(hooks.before_script.is_some());
        assert!(hooks.ready_script.is_some());
        assert!(validate_lifecycle_script(workspace.path(), "missing.js").is_err());
        assert!(validate_lifecycle_script(workspace.path(), ".").is_err());
        assert!(validate_lifecycle_script(workspace.path(), "../hook.js").is_err());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn lifecycle_scripts_execute_in_chromium_with_errors_and_deadlines() -> Result<()> {
        let site = super::super::browser_tests::Site::new(|request| {
            let _ = request.respond(
                tiny_http::Response::from_string(
                    "<script>window.observedBefore = window.beforeCalls;</script><body><main>original</main><div id='cookie-banner'>Cookies</div></body>",
                )
                .with_header(tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap())
                .with_header(tiny_http::Header::from_bytes("Content-Security-Policy", "script-src 'self' 'unsafe-inline'").unwrap()),
            );
        })?;
        let workspace = tempfile::tempdir()?;
        let scripts = [
            (
                "before.js",
                "window.beforeCalls = (window.beforeCalls || 0) + 1; (async () => { await new Promise(resolve => setTimeout(resolve, 25)); window.beforeLabel = window.regressify.scenario.label; })()",
            ),
            (
                "shared.js",
                "function sharedHelper() { throw new Error('helper declaration was invoked'); }\n(async () => { await new Promise(resolve => setTimeout(resolve, 25)); window.hookOrder = ['shared']; window.sharedCalls = (window.sharedCalls || 0) + 1; })()",
            ),
            (
                "ready.js",
                "function readyHelper() { throw new Error('helper declaration was invoked'); }\n(async () => { await Promise.resolve(); window.hookOrder.push('ready'); document.querySelector('#cookie-banner').remove(); document.querySelector('main').textContent = window.regressify.scenario.label; window.readyCalls = (window.readyCalls || 0) + 1; })()",
            ),
        ];
        for (path, script) in scripts {
            std::fs::write(workspace.path().join(path), script)?;
        }
        let label = "Browser hook \"quotes\" \\ newline\n</script>";
        let scenario = TestScenario {
            label: Some(label.into()),
            on_before_script: Some("before.js".into()),
            on_ready_script: Some("ready.js".into()),
            ..Default::default()
        };
        let config = ResolvedConfig {
            js_on_ready_path: Some("shared.js".into()),
            ..Default::default()
        };
        let hooks = DocumentHooks::load(workspace.path(), &scenario, &config)?;
        let (mut browser, handler, directory) =
            super::super::browser_tests::launch_browser("browser-hooks", Default::default())
                .await?;
        let result: Result<()> = async {
            let page = browser.new_page("about:blank").await?;
            let identifier = hooks.install_before(&page).await?;
            page.goto(site.origin.as_str()).await?;
            hooks.complete_before(&page, identifier).await?;
            hooks.apply(&page).await?;
            hooks.apply(&page).await?;
            let state: serde_json::Value = page
                .evaluate("({before: window.observedBefore, beforeLabel: window.beforeLabel, shared: window.sharedCalls, ready: window.readyCalls, order: window.hookOrder, text: document.querySelector('main').textContent, banner: !!document.querySelector('#cookie-banner'), node: typeof process, require: typeof require})")
                .await?
                .into_value()?;
            anyhow::ensure!(state == serde_json::json!({"before": 1, "beforeLabel": label, "shared": 1, "ready": 1, "order": ["shared", "ready"], "text": label, "banner": false, "node": "undefined", "require": "undefined"}), "unexpected hook state: {state}");
            page.goto(format!("{}/next", site.origin)).await?;
            hooks.apply(&page).await?;
            let navigated: bool = page.evaluate("typeof window.beforeCalls === 'undefined' && window.sharedCalls === 1 && window.readyCalls === 1 && !document.querySelector('#cookie-banner')").await?.into_value()?;
            anyhow::ensure!(navigated, "ready scripts must follow navigation without repeating the before hook");
            let selected = browser.new_page(site.origin.as_str()).await?;
            hooks.apply(&selected).await?;
            hooks.apply(&page).await?;
            for target in [&selected, &page] {
                let once: bool = target.evaluate("window.sharedCalls === 1 && window.readyCalls === 1").await?.into_value()?;
                anyhow::ensure!(once, "ready hooks repeated or missed a selected document");
            }

            for (failing_script, expected) in [
                ("throw new Error('browser-hook-error')", "browser-hook-error"),
                ("Promise.reject(new Error('browser-hook-error'))", "browser-hook-error"),
                ("module.exports = async function ready(page) { await page.locator('#target').click(); };", "module is not defined"),
                ("page.locator('#target')", "page is not defined"),
                ("const = 'invalid syntax'", "SyntaxError"),
                ("new Promise(() => {})", "time"),
            ] {
                for (failing_path, _) in scripts {
                    for (path, script) in scripts {
                        std::fs::write(workspace.path().join(path), if path == failing_path { failing_script } else { script })?;
                    }
                    let bounded = ResolvedConfig { element_timeout_ms: 300, ..config.clone() };
                    let failing = DocumentHooks::load(workspace.path(), &scenario, &bounded)?;
                    let identifier = failing.install_before(&page).await?;
                    page.goto(site.origin.as_str()).await?;
                    let outcome = async {
                        failing.complete_before(&page, identifier).await?;
                        failing.apply(&page).await
                    }.await;
                    let error = outcome.err().with_context(|| format!("{failing_path} did not fail"))?;
                    anyhow::ensure!(format!("{error:#}").to_ascii_lowercase().contains(&expected.to_ascii_lowercase()), "{failing_path}: {error:#}");
                    let marker = serde_json::to_string(&failing.marker)?;
                    let completed: bool = page.evaluate(format!("window[{marker}] === true")).await?.into_value()?;
                    anyhow::ensure!(!completed, "failed hook marked its document complete");
                }
            }
            page.evaluate("document.body.textContent = 'browser remains available'").await?;
            Ok(())
        }.await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn hooks_do_not_reuse_destroyed_execution_contexts() -> Result<()> {
        use chromiumoxide::cdp::js_protocol::runtime::{DisableParams, EvaluateParams};

        let site = super::super::browser_tests::Site::new(|request| {
            let _ = request.respond(
                tiny_http::Response::from_string("<body><div class='dynamic'>data</div></body>")
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap(),
                    ),
            );
        })?;
        let workspace = tempfile::tempdir()?;
        std::fs::write(
            workspace.path().join("shared.js"),
            "window.sharedCalls = (window.sharedCalls || 0) + 1;",
        )?;
        std::fs::write(
            workspace.path().join("ready.js"),
            "window.readyCalls = (window.readyCalls || 0) + 1;",
        )?;
        let scenario = TestScenario {
            on_ready_script: Some("ready.js".into()),
            ..Default::default()
        };
        let config = ResolvedConfig {
            js_on_ready_path: Some("shared.js".into()),
            hide_selectors: vec![".dynamic".into()],
            delay: Some(0.0),
            settle_timeout_ms: 1_200,
            ..Default::default()
        };
        let hooks = DocumentHooks::load(workspace.path(), &scenario, &config)?;
        let (mut browser, handler, directory) =
            super::super::browser_tests::launch_browser("hook-context", Default::default()).await?;
        let result: Result<()> = async {
            let page = browser.new_page(site.origin.as_str()).await?;
            let storage =
                super::super::storage_state::StorageState::install(&browser, &page, None, &site.origin)
                    .await?;
            page.evaluate("true").await?;
            // Keep the client's cached context stale while Chromium replaces the document.
            page.execute(DisableParams::default()).await?;
            for path in ["/next", "/last"] {
                page.goto(format!("{}{path}", site.origin)).await?;
                hooks.apply(&page).await?;
                hooks.apply(&page).await?;
                let state = page
                    .execute(EvaluateParams::new(
                        "window.sharedCalls === 1 && window.readyCalls === 1 && \
                         window.regressify === window.__REGRESSIFY__ && \
                         document.querySelectorAll('style').length === 1 && \
                         getComputedStyle(document.querySelector('.dynamic')).visibility === 'hidden'",
                    ))
                    .await?;
                anyhow::ensure!(
                    state.result.result.value == Some(serde_json::json!(true)),
                    "hooks must reach the current document exactly once"
                );
                let settled = super::super::page_stability::wait_for_page_settle(
                    &page,
                    Duration::from_millis(1_200),
                )
                .await?;
                anyhow::ensure!(settled.settled, "settle wait failed with stale context");
                super::super::page_stability::prepare_screenshot(&page, &config, "hook-context")
                    .await?;
                storage.capture(&page).await?;
            }
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
    async fn hooks_follow_navigation_once_and_propagate_script_errors() -> Result<()> {
        let site = super::super::browser_tests::Site::new(|request| {
            let _ = request.respond(
                tiny_http::Response::from_string("<body><div class='dynamic'>data</div></body>")
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap(),
                    ),
            );
        })?;
        let workspace = tempfile::tempdir()?;
        std::fs::create_dir(workspace.path().join("common"))?;
        std::fs::write(
            workspace.path().join("common/_on-ready.js"),
            "(async () => { await Promise.resolve(); window.hookCalls = (window.hookCalls || 0) + 1; })()",
        )?;
        let config = ResolvedConfig {
            hide_selectors: vec![".dynamic".into()],
            ..Default::default()
        };
        let hooks = DocumentHooks::load(workspace.path(), &TestScenario::default(), &config)?;
        let (mut browser, handler, directory) =
            super::super::browser_tests::launch_browser("hooks", Default::default()).await?;
        let result: Result<()> = async {
            let page = browser.new_page(site.origin.as_str()).await?;
            for path in ["/first", "/next"] {
                page.goto(format!("{}{path}", site.origin)).await?;
                hooks.apply(&page).await?;
                hooks.apply(&page).await?;
                let valid: bool = page.evaluate("window.hookCalls === 1 && getComputedStyle(document.querySelector('.dynamic')).visibility === 'hidden'").await?.into_value()?;
                anyhow::ensure!(valid, "document hook missing or repeated after navigation");
            }
            std::fs::write(workspace.path().join("common/_on-ready.js"), "throw new Error('hook-fixture-error')")?;
            let failing = DocumentHooks::load(workspace.path(), &TestScenario::default(), &config)?;
            anyhow::ensure!(failing.apply(&page).await.is_err(), "hook errors must propagate");
            Ok(())
        }.await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }
}
