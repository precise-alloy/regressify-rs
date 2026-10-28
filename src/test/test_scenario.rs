use std::{future::Future, path::PathBuf, sync::Arc};

use anyhow::{Result, anyhow};
use chromiumoxide::Browser;
use colored::Colorize;

use crate::{
    common,
    config::{self, ResolvedConfig, WorkspaceConfig},
    helpers::OnceLockExtensions,
    test::{
        capture::{
            final_screenshot_order_key, final_screenshot_types, load_persisted_state,
            save_screenshot, saved_screenshot_types, validate_cookie_value,
            validate_screenshot_types, write_error_screenshots,
        },
        image_simplification::{ImageSimplifier, restore_after},
        page_stability::{prepare_screenshot, stabilize_capture_animations, trigger_lazy_loading},
        test_action::{ActionExecutionContext, exec_test_action},
    },
    types::{Needs, ScreenshotType, TestAction, TestScenario, TestSuite, UrlReplacement},
};

fn legacy_actions(scenario: &TestScenario) -> Vec<TestAction> {
    use crate::types::{ClickAction, HoverAction, InputAction, ScrollAction};
    if scenario.actions.is_some() {
        return Vec::new();
    }
    let mut actions = Vec::new();
    let typing = scenario
        .key_press_selectors
        .as_deref()
        .or_else(|| {
            scenario
                .key_press_selector
                .as_ref()
                .map(std::slice::from_ref)
        })
        .unwrap_or_default();
    for entry in typing {
        actions.push(TestAction::Input(InputAction {
            input: entry.selector.clone(),
            value: Some(entry.key_press.clone()),
            append: Some(true),
            file: None,
            use_file_chooser: None,
            frame: None,
            viewport_names: None,
            ..Default::default()
        }));
    }
    let selectors = |multiple: &Option<crate::types::Selectors>, single: &Option<String>| {
        multiple
            .as_ref()
            .map(|value| value.values().to_vec())
            .unwrap_or_else(|| single.iter().cloned().collect())
    };
    for hover in selectors(&scenario.hover_selectors, &scenario.hover_selector) {
        actions.push(TestAction::Hover(HoverAction {
            hover,
            frame: None,
            viewport_names: None,
            ..Default::default()
        }));
    }
    for click in selectors(&scenario.click_selectors, &scenario.click_selector) {
        actions.push(TestAction::Click(ClickAction {
            click,
            frame: None,
            viewport_names: None,
            ..Default::default()
        }));
    }
    if let Some(scroll) = &scenario.scroll_to_selector {
        actions.push(TestAction::Scroll(ScrollAction {
            scroll: scroll.clone(),
            frame: None,
            viewport_names: None,
            ..Default::default()
        }));
    }
    actions
}

/// Retain prerequisite origins while preserving the runtime's depth-first replay order.
pub(crate) fn collect_needs_scenarios<'a>(
    scenario: &'a TestScenario,
    all_scenarios: &'a [TestScenario],
    visited: &mut Vec<String>,
) -> Vec<&'a TestScenario> {
    let mut sources = Vec::new();

    let needed_ids = match &scenario.needs {
        Some(Needs::String(id)) => vec![id.as_str()],
        Some(Needs::StringArray(ids)) => ids.iter().map(|s| s.as_str()).collect(),
        None => return sources,
    };

    for needed_id in needed_ids {
        if visited.contains(&needed_id.to_string()) {
            continue; // Already processed — avoid cycles
        }

        if let Some(dep_scenario) = all_scenarios.iter().find(|s| {
            s.id.as_deref()
                .map(|id| id.eq_ignore_ascii_case(needed_id))
                .unwrap_or(false)
        }) {
            visited.push(needed_id.to_string());
            // Recursively collect dependencies of this dependency first
            sources.extend(collect_needs_scenarios(
                dep_scenario,
                all_scenarios,
                visited,
            ));
            sources.push(dep_scenario);
        } else {
            crate::progress::scenario_log!(
                "    {} Dependency scenario '{}' not found",
                "WARNING:".yellow(),
                needed_id
            );
        }
    }

    sources
}

fn collect_needs_actions<'a>(
    scenario: &'a TestScenario,
    all_scenarios: &'a [TestScenario],
    visited: &mut Vec<String>,
) -> Vec<&'a TestAction> {
    collect_needs_scenarios(scenario, all_scenarios, visited)
        .into_iter()
        .flat_map(|source| source.actions.iter().flatten())
        .collect()
}

pub(super) fn scenario_uses_touch(
    scenario: &TestScenario,
    all_scenarios: &[TestScenario],
    viewport: Option<&str>,
) -> bool {
    collect_needs_actions(scenario, all_scenarios, &mut Vec::new())
        .into_iter()
        .chain(scenario.actions.iter().flatten())
        .any(|action| action.uses_touch() && action_applies_to_viewport(action, viewport))
}

/// Build a yellow `[index of total]` log prefix, padding the index to the width
/// of `total` so prefixes line up. Mirrors the TS `actions.js` logPrefix so logs
/// from scenarios/viewports running in parallel can be filtered by their pair.
pub(crate) fn make_log_prefix(index: usize, total: usize) -> String {
    let width = total.to_string().len();
    format!("[{index:>width$} of {total}]").yellow().to_string()
}

/// Shared by execution, tab setup, and capture inventory; do not fork selection semantics.
pub(crate) fn action_applies_to_viewport(action: &TestAction, viewport: Option<&str>) -> bool {
    match action.viewport_names() {
        Some(names) if !names.is_empty() => {
            viewport.is_some_and(|label| names.iter().any(|name| name.eq_ignore_ascii_case(label)))
        }
        _ => true,
    }
}

fn log_skipped_viewport_action(
    action: &TestAction,
    viewport: Option<&str>,
    log_prefix: &str,
    fallback_level: crate::diagnostics::record::Severity,
) {
    use crate::diagnostics::{record::Severity, runtime as diagnostics};
    let level = diagnostics::current()
        .and_then(|context| context.display_level())
        .unwrap_or(fallback_level);
    if level > Severity::Debug {
        return;
    }
    let (kind, _) = super::test_action::action_diagnostic_identity(action);
    crate::progress::scenario_log!(
        "{} Skipping {:?} action for viewport {:?}: excluded by viewportNames",
        log_prefix,
        kind,
        viewport.unwrap_or("<unnamed>"),
    );
}

/// Resolve the viewports a scenario captures, as a flat list of capture targets.
/// Returns `[None]` when no viewport file is configured (capture once at the
/// browser's default size) or one `Some(viewport)` per resolved viewport (in
/// viewport-file order). Each entry becomes an independent capture job with its
/// own browser and dedicated log index — mirroring the TS/BackstopJS model where
/// every scenario/viewport pair is scaffolded upfront and runs independently.
pub(crate) fn scenario_capture_viewports(
    workspace_config: &WorkspaceConfig,
    test_suite: &TestSuite,
    scenario: &TestScenario,
) -> Result<Vec<Option<config::viewports::ViewportDef>>> {
    let resolved = config::resolve_config(workspace_config, test_suite, scenario);
    let viewports = resolve_scenario_viewports(&resolved)?;
    if viewports.is_empty() {
        Ok(vec![None])
    } else {
        Ok(viewports.into_iter().map(Some).collect())
    }
}

/// Ordered capture keys for a scenario. Saved steps from prerequisite and
/// current actions precede final targets, matching their execution order.
pub(crate) fn scenario_capture_order_keys(
    workspace_config: &WorkspaceConfig,
    test_suite: &TestSuite,
    scenario: &TestScenario,
    all_scenarios: &[TestScenario],
) -> Result<Vec<String>> {
    let Some(scenario_id) = scenario.id.as_deref() else {
        return Ok(Vec::new());
    };
    let mut keys = Vec::new();
    for viewport in scenario_capture_viewports(workspace_config, test_suite, scenario)? {
        let label = viewport.as_ref().map(|viewport| viewport.label.as_str());
        let screenshot_types = scenario_screenshot_types(scenario, all_scenarios, label)?;
        keys.extend(screenshot_types.iter().map(|screenshot_type| {
            final_screenshot_order_key(scenario_id, label, screenshot_type)
        }));
    }
    Ok(keys)
}

pub(crate) fn capture_progress_keys(
    scenario: &TestScenario,
    all_scenarios: &[TestScenario],
    viewport: Option<&str>,
) -> Result<Vec<String>> {
    let Some(scenario_id) = scenario.id.as_deref() else {
        return Ok(Vec::new());
    };
    Ok(
        scenario_screenshot_types(scenario, all_scenarios, viewport)?
            .iter()
            .map(|screenshot_type| {
                final_screenshot_order_key(scenario_id, viewport, screenshot_type)
            })
            .collect(),
    )
}

fn scenario_final_screenshot_types(
    scenario: &TestScenario,
    viewport: Option<&str>,
) -> Result<Vec<ScreenshotType>> {
    let screenshot_types = final_screenshot_types(scenario)?;
    let last_action = scenario
        .actions
        .iter()
        .flatten()
        .rfind(|action| action_applies_to_viewport(action, viewport));
    if scenario.selectors.is_none() && matches!(last_action, Some(TestAction::Save(_))) {
        Ok(Vec::new())
    } else {
        Ok(screenshot_types)
    }
}

fn scenario_screenshot_types(
    scenario: &TestScenario,
    all_scenarios: &[TestScenario],
    viewport: Option<&str>,
) -> Result<Vec<ScreenshotType>> {
    let mut visited = Vec::new();
    let prerequisite_actions = collect_needs_actions(scenario, all_scenarios, &mut visited);
    let mut screenshot_types = saved_screenshot_types(
        prerequisite_actions
            .into_iter()
            .chain(scenario.actions.iter().flatten())
            .filter(|action| action_applies_to_viewport(action, viewport)),
    );
    screenshot_types.extend(scenario_final_screenshot_types(scenario, viewport)?);
    validate_screenshot_types(&screenshot_types)?;
    Ok(screenshot_types)
}

/// Bundled inputs for executing one capture (a single scenario/viewport pair).
/// Grouping these avoids long argument lists across `exec_single_scenario` /
/// `exec_test_scenario`. Each capture is an independent unit with its own browser
/// and dedicated `index`, matching the TS/BackstopJS model.
#[derive(Clone, Copy)]
pub(crate) struct ScenarioRequest<'a> {
    pub test_suite: &'a TestSuite,
    pub scenario: &'a TestScenario,
    pub all_scenarios: &'a [TestScenario],
    pub workspace_config: &'a WorkspaceConfig,
    pub replacement_profile: &'a [UrlReplacement],
    pub apply_url_replacements: bool,
    /// The viewport to capture, or `None` to capture at the browser default size.
    pub viewport: Option<&'a config::viewports::ViewportDef>,
    /// 1-based dedicated index of this scenario/viewport pair in the global count.
    pub index: usize,
    /// Total number of scenario/viewport capture pairs across the whole run.
    pub total: usize,
}

/// Bundled inputs for a single scenario/viewport capture.
#[derive(Clone, Copy)]
struct CaptureContext<'a> {
    is_reference: bool,
    test_suite: &'a TestSuite,
    scenario: &'a TestScenario,
    all_scenarios: &'a [TestScenario],
    resolved: &'a ResolvedConfig,
    url: &'a str,
    url_replacements: &'a [UrlReplacement],
    viewport: Option<&'a config::viewports::ViewportDef>,
    log_prefix: &'a str,
}

struct ScenarioPage {
    page: chromiumoxide::Page,
    runtime: super::runtime::Runtime,
    storage: Arc<super::storage_state::StorageState>,
    authentication: Option<super::authentication::Authentication>,
    tabs: Option<super::tabs::Tabs>,
}

async fn install_tab_tracker_for_actions<'a>(
    page: &chromiumoxide::Page,
    storage: Arc<super::storage_state::StorageState>,
    resolved: &ResolvedConfig,
    viewport: Option<&config::viewports::ViewportDef>,
    actions: impl Iterator<Item = &'a TestAction>,
) -> Result<Option<super::tabs::Tabs>> {
    let label = viewport.map(|viewport| viewport.label.as_str());
    let mut needs_tabs = false;
    let mut has_touch = false;
    for action in actions.filter(|action| action_applies_to_viewport(action, label)) {
        needs_tabs |= matches!(action, TestAction::SwitchTab(_) | TestAction::CloseTab(_));
        has_touch |= action.uses_touch();
    }
    if needs_tabs || has_touch {
        Ok(Some(
            super::tabs::Tabs::install(page, storage, resolved, viewport, has_touch).await?,
        ))
    } else {
        Ok(None)
    }
}

impl ScenarioPage {
    async fn exec(
        &mut self,
        browser: &Browser,
        ctx: CaptureContext<'_>,
        hooks: &super::hooks::DocumentHooks,
        action: &TestAction,
    ) -> Result<()> {
        // A skipped action must not touch even a closed document or tab tracker.
        if !action_applies_to_viewport(action, ctx.viewport.map(|viewport| viewport.label.as_str()))
        {
            log_skipped_viewport_action(
                action,
                ctx.viewport.map(|viewport| viewport.label.as_str()),
                ctx.log_prefix,
                ctx.resolved.log_level,
            );
            return Ok(());
        }
        // A popup can close itself. Handle switching before touching the old
        // document so returning to a surviving tab does not depend on that page.
        if let TestAction::SwitchTab(selection) = action {
            self.runtime.ensure_no_pending_dialog()?;
            use crate::diagnostics::{record::Stage, runtime as diagnostics};
            let fields = super::test_action::action_diagnostic_fields(
                action,
                diagnostics::next_action_index(),
            );
            let tabs = self
                .tabs
                .as_mut()
                .ok_or_else(|| anyhow!("Tab tracking was not initialized"))?;
            let (page, runtime, first_visit) = diagnostics::action_scope(
                fields.clone(),
                diagnostics::stage(
                    Stage::ActionsFrames,
                    fields,
                    tabs.switch(browser, selection, ctx.url_replacements),
                ),
            )
            .await?;
            self.page = page;
            self.runtime = runtime;
            hooks.apply(&self.page).await?;
            if first_visit {
                prepare_page_for_actions(&self.page, &self.runtime, ctx.resolved, ctx.log_prefix)
                    .await?;
            }
            return Ok(());
        }
        if let TestAction::CloseTab(close) = action {
            use crate::diagnostics::{record::Stage, runtime as diagnostics};
            let fields = super::test_action::action_diagnostic_fields(
                action,
                diagnostics::next_action_index(),
            );
            let tabs = self
                .tabs
                .as_mut()
                .ok_or_else(|| anyhow!("Tab tracking was not initialized"))?;
            let switched = diagnostics::action_scope(
                fields.clone(),
                diagnostics::stage(
                    Stage::ActionsFrames,
                    fields,
                    tabs.close(browser, &self.runtime, action, close),
                ),
            )
            .await?;
            if let Some((page, runtime, first_visit)) = switched {
                self.page = page;
                self.runtime = runtime;
                hooks.apply(&self.page).await?;
                if first_visit {
                    prepare_page_for_actions(
                        &self.page,
                        &self.runtime,
                        ctx.resolved,
                        ctx.log_prefix,
                    )
                    .await?;
                }
            }
            return Ok(());
        }
        if let Some(tabs) = &self.tabs {
            tabs.check().await?;
        }
        if let Some(authentication) = &self.authentication {
            authentication.check()?;
        }
        hooks.apply(&self.page).await?;
        exec_test_action(
            ActionExecutionContext {
                page: &self.page,
                runtime: &self.runtime,
                storage: &self.storage,
                test_suite: ctx.test_suite,
                scenario: ctx.scenario,
                config: ctx.resolved,
                url_replacements: ctx.url_replacements,
                viewport_label: ctx.viewport.map(|viewport| viewport.label.as_str()),
                log_prefix: ctx.log_prefix,
            },
            action,
        )
        .await
    }
}

async fn prepare_page_for_actions(
    page: &chromiumoxide::Page,
    runtime: &super::runtime::Runtime,
    resolved: &ResolvedConfig,
    log_prefix: &str,
) -> Result<()> {
    if resolved.freeze_animations {
        stabilize_capture_animations(page, log_prefix).await?;
    }
    if !resolved.no_scroll_top {
        trigger_lazy_loading(runtime, resolved).await?;
    }
    Ok(())
}

pub async fn exec_test_scenario(browser: &Browser, req: ScenarioRequest<'_>) -> Result<()> {
    let ScenarioRequest {
        test_suite,
        scenario,
        all_scenarios,
        workspace_config,
        replacement_profile,
        apply_url_replacements: should_apply_url_replacements,
        viewport,
        index,
        total,
    } = req;

    let log_prefix = make_log_prefix(index, total);

    // Resolve configuration cascade: scenario -> suite -> workspace -> defaults
    let resolved = config::resolve_config(workspace_config, test_suite, scenario);

    let url_replacements = if should_apply_url_replacements {
        collect_url_replacements(test_suite, workspace_config, replacement_profile)
    } else {
        Vec::new()
    };
    let url = apply_url_replacements(&scenario.url, &url_replacements)?;

    // This request captures exactly one scenario/viewport pair. When a viewport
    // is present the full page lifecycle (navigation + actions) is replayed at
    // that size so layout and interactions reflect it; with no viewport we
    // capture once at the browser's default size. Each pair is an independent
    // task (own browser, own index), so a failure here only fails this pair.
    let capture_context = CaptureContext {
        is_reference: !should_apply_url_replacements,
        test_suite,
        scenario,
        all_scenarios,
        resolved: &resolved,
        url: &url,
        url_replacements: &url_replacements,
        viewport,
        log_prefix: &log_prefix,
    };
    run_scenario_capture(browser, capture_context).await
}

/// Load and filter the viewports configured for this scenario.
/// Viewport definitions live in a YAML file resolved relative to the workspace
/// root (default `common/_viewports.yaml`). Returns an empty vec when no viewport
/// file exists, which means "capture once at the default browser size".
fn resolve_scenario_viewports(
    resolved: &ResolvedConfig,
) -> Result<Vec<config::viewports::ViewportDef>> {
    let root_dir = common::ROOT_DIR.get_required()?;
    let Some(viewports_path) = resolve_viewports_file(
        root_dir,
        &resolved.viewports_path,
        resolved.viewports_path_is_explicit,
    )?
    else {
        return Ok(Vec::new());
    };
    let all = config::viewports::load_viewports(&viewports_path)?;
    if all.is_empty() {
        return Ok(Vec::new());
    }
    config::viewports::resolve_viewports(resolved.viewport_names.as_deref(), &all)
}

fn resolve_viewports_file(
    root_dir: &std::path::Path,
    configured_path: &str,
    is_explicit: bool,
) -> Result<Option<PathBuf>> {
    if is_explicit {
        config::resolve_workspace_file(root_dir, configured_path, "viewportsPath").map(Some)
    } else {
        config::resolve_optional_workspace_file(root_dir, configured_path, "default viewportsPath")
    }
}

/// Apply a viewport size to the page via CDP device-metrics emulation.
async fn apply_viewport(
    page: &chromiumoxide::Page,
    viewport: Option<&config::viewports::ViewportDef>,
) -> Result<()> {
    if let Some(vp) = viewport {
        use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
        let cmd = SetDeviceMetricsOverrideParams::builder()
            .width(vp.width as i64)
            .height(vp.height as i64)
            .device_scale_factor(1.0)
            .mobile(false)
            .build()
            .map_err(|e| anyhow!("Failed to build device metrics override: {:?}", e))?;
        page.execute(cmd).await?;
    }
    Ok(())
}

/// Run a single capture of the scenario at an optional viewport size.
///
/// On failure (the page under test errored before a screenshot was produced) an
/// error-placeholder image is written to the scenario's screenshot path so the
/// report and visual comparison still have an image to show. The original error
/// is then propagated so the scenario is still recorded as failed.
async fn run_scenario_capture(browser: &Browser, ctx: CaptureContext<'_>) -> Result<()> {
    let viewport_label = ctx.viewport.map(|v| v.label.as_str());
    let label = ctx
        .scenario
        .label
        .as_deref()
        .or(ctx.scenario.id.as_deref())
        .unwrap_or(&ctx.scenario.url);
    let timeout = std::time::Duration::from_millis(ctx.resolved.scenario_timeout_ms);
    let final_screenshot_types = scenario_final_screenshot_types(ctx.scenario, viewport_label)?;
    let error_screenshot_types =
        scenario_screenshot_types(ctx.scenario, ctx.all_scenarios, viewport_label)?;

    match run_with_scenario_timeout(
        timeout,
        label,
        run_scenario_capture_inner(browser, ctx, &final_screenshot_types),
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(e) => {
            use crate::diagnostics::{record::*, runtime as diagnostics};
            if let Some(context) = diagnostics::current() {
                context.record(
                    Stage::CaptureArtifacts,
                    DiagnosticEvent::StageFinished,
                    Severity::Error,
                    [],
                    diagnostics::Completion {
                        outcome: Some(Outcome::Failure),
                        error: Some(SanitizedError::capture(&e)),
                        duration_ms: None,
                    },
                );
            }
            write_error_screenshots(
                ctx.test_suite,
                ctx.scenario,
                &error_screenshot_types,
                viewport_label,
                &diagnostics::failure_summary(&e),
            )?;
            Err(e)
        }
    }
}

/// Run an entire scenario lifecycle under a hard deadline so an unresponsive
/// page or CDP operation cannot occupy a capture slot forever.
async fn run_with_scenario_timeout<T, F>(
    timeout: std::time::Duration,
    label: &str,
    operation: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    match tokio::time::timeout(timeout, operation).await {
        Ok(result) => result,
        Err(_) => {
            crate::diagnostics::runtime::scenario_timeout(timeout.as_millis() as u64);
            Err(anyhow!(
                "Scenario '{}' exceeded its {}ms timeout",
                label,
                timeout.as_millis()
            ))
        }
    }
}

/// Inner capture routine: drives the full page lifecycle and saves the
/// screenshot. Errors are handled by the `run_scenario_capture` wrapper.
async fn run_scenario_capture_inner(
    browser: &Browser,
    ctx: CaptureContext<'_>,
    screenshot_types: &[ScreenshotType],
) -> Result<()> {
    let CaptureContext {
        is_reference,
        test_suite,
        scenario,
        all_scenarios,
        resolved,
        url,
        viewport,
        log_prefix,
        ..
    } = ctx;
    let viewport_label = viewport.map(|v| v.label.as_str());

    let hooks =
        super::hooks::DocumentHooks::load(common::ROOT_DIR.get_required()?, scenario, resolved)?
            .with_context(serde_json::json!({
                "requiredLogin": resolved.required_login,
                "scenario": {
                    "id": scenario.id, "label": scenario.label,
                    "description": scenario.description, "url": url,
                    "requiredLogin": resolved.required_login, "testSuite": test_suite.id,
                },
                "viewport": viewport.map(|value| serde_json::json!({
                    "label": value.label, "width": value.width, "height": value.height
                })),
                "isReference": is_reference,
                "config": resolved,
            }));

    // Create page with about:blank first, apply the viewport + CSP bypass, then
    // navigate so layout is computed at the intended size from the first paint.
    let page = browser.new_page("about:blank").await?;
    let runtime = super::runtime::Runtime::new(browser, &page).await?;
    let authentication = super::authentication::Authentication::install_with_client(
        runtime.client(),
        &page,
        &resolved.basic_auth,
    )
    .await?;
    if resolved.bypass_csp {
        let cmd = chromiumoxide::cdp::browser_protocol::page::SetBypassCspParams::builder()
            .enabled(true)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build SetBypassCspParams: {}", e))?;
        page.execute(cmd).await?;
    }
    apply_viewport(&page, viewport).await?;
    let has_touch = scenario_uses_touch(scenario, all_scenarios, viewport_label);
    if has_touch {
        page.execute(
            chromiumoxide::cdp::browser_protocol::emulation::SetTouchEmulationEnabledParams::new(
                true,
            ),
        )
        .await?;
    }
    let storage = super::storage_state::StorageState::install_with_client(
        runtime.client(),
        &page,
        resolved.state.as_ref(),
        url,
    )
    .await?;

    // Load cookies before the first navigation so authenticated/session state
    // can affect the initial request.
    if let Some(ref state) = resolved.state {
        load_state_cookies(&page, state, log_prefix).await?;
    }
    let cookie_path = resolved
        .cookie_path
        .as_deref()
        .unwrap_or("common/_cookies.yaml");
    load_cookie_path(&page, cookie_path, log_prefix).await?;

    let mut visited = Vec::new();
    let needs_actions = collect_needs_actions(scenario, all_scenarios, &mut visited);
    let storage = Arc::new(storage);
    let tabs = install_tab_tracker_for_actions(
        &page,
        storage.clone(),
        resolved,
        viewport,
        needs_actions
            .iter()
            .copied()
            .chain(scenario.actions.iter().flatten()),
    )
    .await?;
    let mut active = ScenarioPage {
        page,
        runtime,
        storage,
        authentication,
        tabs,
    };

    let before_hook = hooks.install_before(&active.page).await?;

    let target = active
        .runtime
        .target(
            &None,
            std::time::Duration::from_millis(resolved.element_timeout_ms),
        )
        .await?;
    let options = scenario
        .engine_options
        .as_ref()
        .and_then(|value| value.goto_parameters.as_ref())
        .cloned()
        .unwrap_or_default();
    target.navigate(url, &options).await?;
    hooks.complete_before(&active.page, before_hook).await?;
    if let Some(selector) = &scenario.ready_selector {
        use crate::diagnostics::{record::*, runtime as diagnostics};
        let timeout = scenario
            .ready_timeout
            .filter(|value| *value > 0)
            .unwrap_or(resolved.element_timeout_ms);
        let readiness = crate::test::actionability::ActionabilityConfig {
            element_timeout_ms: timeout,
            actionability_timeout_ms: timeout,
            ..Default::default()
        };
        diagnostics::stage(
            Stage::Navigation,
            [
                (Field::State, FieldValue::Kind(SafeValue::Ready)),
                (
                    Field::Target,
                    FieldValue::Identity(SafeIdentity::source(selector)),
                ),
                (Field::TimeoutMs, FieldValue::Number(timeout)),
            ],
            crate::test::actionability::wait_for_selector(
                &target,
                selector,
                &None,
                &readiness,
                Stage::Navigation,
            ),
        )
        .await?;
    }

    hooks.apply(&active.page).await?;
    prepare_page_for_actions(&active.page, &active.runtime, resolved, log_prefix).await?;

    // Execute prerequisite actions from `needs` dependencies
    if !needs_actions.is_empty() {
        crate::progress::scenario_log!(
            "{} Executing {} prerequisite actions from dependencies...",
            log_prefix,
            needs_actions
                .iter()
                .filter(|action| action_applies_to_viewport(action, viewport_label))
                .count()
        );
        for action in &needs_actions {
            active.exec(browser, ctx, &hooks, action).await?;
        }
    }

    // Execute this scenario's own actions
    if let Some(actions) = &scenario.actions {
        crate::progress::scenario_log!(
            "{} Executing {} actions...",
            log_prefix,
            actions
                .iter()
                .filter(|action| action_applies_to_viewport(action, viewport_label))
                .count()
        );
        for action in actions {
            active.exec(browser, ctx, &hooks, action).await?;
        }
    }

    hooks.apply(&active.page).await?;

    for action in &legacy_actions(scenario) {
        active.exec(browser, ctx, &hooks, action).await?;
    }

    hooks.apply(&active.page).await?;

    active.runtime.ensure_no_pending_dialog()?;
    if screenshot_types.is_empty() {
        if let Some(tabs) = &active.tabs {
            tabs.check().await?;
        }
        if let Some(authentication) = &active.authentication {
            authentication.check()?;
        }
        return Ok(());
    }

    // Apply postInteractionWait from resolved config
    if let Some(wait_ms) = resolved.post_interaction_wait
        && wait_ms > 0.0
    {
        crate::progress::scenario_log!(
            "{} Waiting for {} ms after interactions...",
            log_prefix,
            wait_ms
        );
        tokio::time::sleep(std::time::Duration::from_millis(wait_ms as u64)).await;
    }

    // Images flagged during preparation stay flagged for every final target
    // of this preparation, then are restored even when a capture fails.
    let simplifier = ImageSimplifier::from_config(resolved)?;
    let captured = async {
        prepare_screenshot(&active.page, resolved, log_prefix).await?;
        for screenshot_type in screenshot_types {
            hooks.apply(&active.page).await?;
            if let Some(tabs) = &active.tabs {
                tabs.check().await?;
            }
            if let Some(authentication) = &active.authentication {
                authentication.check()?;
            }
            save_screenshot(
                test_suite,
                scenario,
                &active.page,
                screenshot_type.clone(),
                viewport_label,
                scenario.selector.as_deref(),
                simplifier.as_ref(),
            )
            .await?;
        }
        Ok(())
    }
    .await;
    restore_after(simplifier.as_ref(), &active.page, captured).await
}

/// Apply urlReplacements from the test suite to a URL.
/// Supports both literal string replacement and regex replacement.
pub(crate) fn collect_url_replacements(
    test_suite: &TestSuite,
    workspace_config: &WorkspaceConfig,
    replacement_profile: &[UrlReplacement],
) -> Vec<UrlReplacement> {
    let mut combined = replacement_profile.to_vec();
    if let Some(ref workspace_replacements) = workspace_config.url_replacements {
        combined.extend(workspace_replacements.clone());
    }
    if let Some(ref suite_replacements) = test_suite.url_replacements {
        combined.extend(suite_replacements.clone());
    }
    combined
}

/// Apply URL replacements in declaration order.
fn apply_url_replacements(url: &str, replacements: &[UrlReplacement]) -> Result<String> {
    config::replacements::apply_replacements(url, replacements)
}

fn resolve_work_dir_path(path: &str) -> Result<Option<PathBuf>> {
    let root_dir = common::ROOT_DIR.get_required()?;
    config::resolve_optional_workspace_file(root_dir, path, "workspace file")
}

/// Load persisted state cookies before the first navigation.
async fn load_state_cookies(
    page: &chromiumoxide::Page,
    state: &Needs,
    _log_prefix: &str,
) -> Result<()> {
    use crate::diagnostics::{record::*, runtime as diagnostics};
    diagnostics::stage(
        Stage::StateHooks,
        [(Field::ActionType, FieldValue::Kind(SafeValue::Restore))],
        load_state_cookies_inner(page, state),
    )
    .await
}

async fn load_state_cookies_inner(page: &chromiumoxide::Page, state: &Needs) -> Result<()> {
    let state_names = match state {
        Needs::String(s) => vec![s.clone()],
        Needs::StringArray(v) => v.clone(),
    };

    for name in &state_names {
        let (_, state) = load_persisted_state(name)?;

        // Restore cookies
        if let Some(cookies) = state.get("cookies").and_then(|c| c.as_array()) {
            for (index, cookie) in cookies.iter().enumerate() {
                let (name, cookie_param) = build_cookie_param(
                    cookie,
                    &format!("Persisted state '{}' cookie {}", name, index),
                )?;
                page.set_cookie(cookie_param).await.map_err(|error| {
                    anyhow!("Failed to restore state cookie '{}': {:?}", name, error)
                })?;
            }
        }
    }

    Ok(())
}

/// Load cookies from a YAML file.
async fn load_cookie_path(
    page: &chromiumoxide::Page,
    cookie_path: &str,
    _log_prefix: &str,
) -> Result<()> {
    use crate::diagnostics::{record::*, runtime as diagnostics};
    diagnostics::stage(
        Stage::StateHooks,
        [
            (Field::ActionType, FieldValue::Kind(SafeValue::Restore)),
            (
                Field::Artifact,
                FieldValue::Identity(SafeIdentity::source(cookie_path)),
            ),
        ],
        load_cookie_path_inner(page, cookie_path),
    )
    .await
}

async fn load_cookie_path_inner(page: &chromiumoxide::Page, cookie_path: &str) -> Result<()> {
    let Some(cookie_path) = resolve_work_dir_path(cookie_path)? else {
        return Ok(()); // No cookie file — that's fine
    };

    let content = std::fs::read_to_string(&cookie_path)?;
    let cookies: Vec<serde_json::Value> = serde_yaml::from_str(&content)?;
    let cookies = super::capture::expand_cookie_entries(
        &cookies,
        &format!("cookiePath '{}'", cookie_path.display()),
    )?;

    for (index, cookie) in cookies.iter().enumerate() {
        let (name, cookie_param) = build_cookie_param(
            cookie,
            &format!("cookiePath '{}' cookie {}", cookie_path.display(), index),
        )?;
        page.set_cookie(cookie_param)
            .await
            .map_err(|error| anyhow!("Failed to load cookie '{}': {:?}", name, error))?;
    }

    Ok(())
}

/// Derive a cookie `url` from its JSON definition.
///
/// Cookies are loaded before the first navigation, while the page is still at
/// `about:blank`. chromiumoxide falls back to the current page URL when a
/// cookie has no explicit `url`, and rejects `about:blank` with
/// "Blank page can not have cookie". Supplying a `url` derived from the
/// cookie's `domain` avoids that fallback. An explicit `url` field wins.
fn cookie_url_from(cookie: &serde_json::Value) -> Option<String> {
    if let Some(url) = cookie.get("url").and_then(|v| v.as_str()) {
        return Some(url.to_string());
    }
    let domain = cookie.get("domain").and_then(|v| v.as_str())?;
    let host = domain.trim_start_matches('.');
    if host.is_empty() {
        return None;
    }
    let secure = cookie
        .get("secure")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let scheme = if secure { "https" } else { "http" };
    let path = cookie.get("path").and_then(|v| v.as_str()).unwrap_or("/");
    Some(format!("{scheme}://{host}{path}"))
}

fn build_cookie_param(
    cookie: &serde_json::Value,
    context: &str,
) -> Result<(
    String,
    chromiumoxide::cdp::browser_protocol::network::CookieParam,
)> {
    use chromiumoxide::cdp::browser_protocol::network::{CookieParam, TimeSinceEpoch};

    validate_cookie_value(cookie, context)?;
    let name = cookie
        .get("name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("{} has no name after validation", context))?;
    let value = cookie
        .get("value")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("{} has no value after validation", context))?;
    let mut param = CookieParam::builder().name(name).value(value);

    if let Some(url) = cookie_url_from(cookie) {
        param = param.url(url);
    }
    if let Some(domain) = cookie.get("domain").and_then(|value| value.as_str()) {
        param = param.domain(domain);
    }
    if let Some(path) = cookie.get("path").and_then(|value| value.as_str()) {
        param = param.path(path);
    }
    if let Some(secure) = cookie.get("secure").and_then(|value| value.as_bool()) {
        param = param.secure(secure);
    }
    if let Some(http_only) = cookie.get("httpOnly").and_then(|value| value.as_bool()) {
        param = param.http_only(http_only);
    }
    if let Some(expires) = cookie.get("expires").and_then(|value| value.as_f64()) {
        param = param.expires(TimeSinceEpoch::new(expires));
    }
    if let Some(same_site) = cookie.get("sameSite").and_then(|value| value.as_str()) {
        param = param.same_site(parse_same_site(same_site)?);
    }

    let param = param
        .build()
        .map_err(|error| anyhow!("Failed to build {}: {:?}", context, error))?;
    Ok((name.to_string(), param))
}

/// Parse a SameSite string from cookie JSON into the CDP enum.
fn parse_same_site(
    s: &str,
) -> Result<chromiumoxide::cdp::browser_protocol::network::CookieSameSite> {
    use chromiumoxide::cdp::browser_protocol::network::CookieSameSite;
    match s {
        "Strict" | "strict" => Ok(CookieSameSite::Strict),
        "Lax" | "lax" => Ok(CookieSameSite::Lax),
        "None" | "none" => Ok(CookieSameSite::None),
        _ => anyhow::bail!(
            "Unsupported SameSite value '{}'; expected Strict, Lax, or None",
            s
        ),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn expanded_actions_touch_activation_uses_consumer_viewports_and_needs() {
        let suite: TestSuite = serde_yaml::from_str("scenarios:\n- id: base\n  url: /\n  actions:\n  - tap: '#menu'\n    viewportNames: mobile\n- id: child\n  url: /\n  needs: base\n- id: plain\n  url: /\n  actions:\n  - click: '#menu'\n").unwrap();
        for scenario in &suite.scenarios[..2] {
            assert!(super::scenario_uses_touch(
                scenario,
                &suite.scenarios,
                Some("MOBILE")
            ));
            assert!(!super::scenario_uses_touch(
                scenario,
                &suite.scenarios,
                Some("desktop")
            ));
            assert!(!super::scenario_uses_touch(
                scenario,
                &suite.scenarios,
                None
            ));
        }
        assert!(!super::scenario_uses_touch(
            &suite.scenarios[2],
            &suite.scenarios,
            Some("mobile")
        ));
    }

    use super::*;

    #[tokio::test]
    async fn skipped_action_debug_logs_are_redacted_and_do_not_count_as_execution() -> Result<()> {
        use crate::diagnostics::{
            record::{OperationKind, Outcome, Severity},
            runtime as diagnostics,
        };
        use serde_json::json;

        let Some(level) = std::env::var_os("REGRESSIFY_ACTION_SKIP_LOG_FIXTURE") else {
            for level in ["debug", "info"] {
                let output = std::process::Command::new(std::env::current_exe()?)
                    .args(["--exact", "test::test_scenario::tests::skipped_action_debug_logs_are_redacted_and_do_not_count_as_execution", "--nocapture"])
                    .env("REGRESSIFY_ACTION_SKIP_LOG_FIXTURE", level)
                    .output()?;
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                anyhow::ensure!(output.status.success(), "{text}");
                assert_eq!(
                    text.contains("Skipping Input action for viewport \"desktop\""),
                    level == "debug",
                    "{text}"
                );
                for sensitive in [
                    "super-secret-value",
                    "selector-fixture",
                    "private-file",
                    "private-frame",
                    "credential-fixture",
                    "password-fixture",
                    "state-fixture",
                    "token-fixture",
                    "capture-fixture",
                ] {
                    assert!(!text.contains(sensitive), "diagnostic leaked {sensitive}");
                }
            }
            return Ok(());
        };
        let level: Severity = level.to_string_lossy().parse().unwrap();
        let workspace = tempfile::tempdir()?;
        let operation = diagnostics::Operation::start(
            workspace.path(),
            OperationKind::Capture,
            None,
            None,
            None,
        );
        let context = operation.context();
        context.set_display_level(level);
        diagnostics::scope(Some(context), async {
            assert_eq!(diagnostics::next_action_index(), 1);
            for mut shape in [
                json!({ "input": "#selector-fixture", "value": "super-secret-value", "file": "private-file", "frame": "#private-frame" }),
                json!({ "goto": "https://credential-fixture:password-fixture@example.invalid/path" }),
                json!({ "persist": "state-fixture" }),
                json!({ "switchTab": "new", "url": "https://token-fixture.example.invalid" }),
                json!({ "save": "capture-fixture" }),
            ] {
                shape["viewportNames"] = json!(["mobile"]);
                let action: TestAction = serde_json::from_value(shape).unwrap();
                assert!(!action_applies_to_viewport(&action, Some("desktop")));
                log_skipped_viewport_action(&action, Some("desktop"), "[fixture]", Severity::Trace);
            }
            assert_eq!(diagnostics::next_action_index(), 2);
        }).await;
        operation.finish(Outcome::Success).await;
        Ok(())
    }

    #[test]
    fn action_viewport_applicability_matches_selection_semantics() {
        use serde_json::json;

        for (filter, expected) in [
            (json!(null), [true, true, true]),
            (json!([]), [true, true, true]),
            (json!("Desktop"), [false, true, false]),
            (json!("mobile"), [false, false, true]),
            (json!(""), [false, false, false]),
            (json!(["Desktop"]), [false, true, false]),
            (json!(["mobile", "MOBILE", "mobile"]), [false, false, true]),
            (json!(["desktop", "mobile"]), [false, true, true]),
            (json!([" desktop"]), [false, false, false]),
        ] {
            let action: TestAction = serde_json::from_value(json!({
                "wait": 0,
                "viewportNames": filter
            }))
            .unwrap();
            let execution: fn(&TestAction, Option<&str>) -> bool = action_applies_to_viewport;
            let capture_planning: fn(&TestAction, Option<&str>) -> bool =
                action_applies_to_viewport;
            assert!(std::ptr::fn_addr_eq(execution, capture_planning));
            for (viewport, expected) in [None, Some("desktop"), Some("mobile")]
                .into_iter()
                .zip(expected)
            {
                assert_eq!(
                    execution(&action, viewport),
                    expected,
                    "{filter} {viewport:?}"
                );
                assert_eq!(
                    capture_planning(&action, viewport),
                    expected,
                    "{filter} {viewport:?}"
                );
            }
        }
        let unset: TestAction = serde_yaml::from_str("wait: 0\n").unwrap();
        assert!(action_applies_to_viewport(&unset, None));
        assert!(action_applies_to_viewport(&unset, Some("desktop")));
    }

    #[test]
    fn action_viewport_applicability_preserves_order_and_shared_input() {
        let actions: Vec<TestAction> = serde_yaml::from_str("- wait: 1\n  viewportNames: [mobile, MOBILE]\n- wait: 2\n- wait: 3\n  viewportNames: [desktop]\n").unwrap();
        let before = serde_json::to_value(&actions).unwrap();
        std::thread::scope(|scope| {
            for (viewport, expected) in [("desktop", vec![2, 3]), ("mobile", vec![1, 2])] {
                let actions = &actions;
                scope.spawn(move || {
                    let selected = actions
                        .iter()
                        .filter(|action| action_applies_to_viewport(action, Some(viewport)))
                        .map(|action| {
                            serde_json::to_value(action).unwrap()["wait"]
                                .as_u64()
                                .unwrap()
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(selected, expected);
                });
            }
        });
        assert_eq!(serde_json::to_value(actions).unwrap(), before);
    }

    #[test]
    fn legacy_shortcuts_preserve_order_and_yield_to_actions() {
        let mut scenario: TestScenario = serde_yaml::from_str("url: https://example.com\nkeyPressSelector: {selector: '#input', keyPress: hello}\nhoverSelectors: ['#one', '#two']\nclickSelectors: ['#first', '#second']\nscrollToSelector: '#last'\n").unwrap();
        let actions = legacy_actions(&scenario);
        assert_eq!(actions.len(), 6);
        assert!(matches!(actions[0], TestAction::Input(_)));
        assert!(matches!(actions[1], TestAction::Hover(_)));
        assert!(matches!(actions[3], TestAction::Click(_)));
        assert!(matches!(actions[5], TestAction::Scroll(_)));
        assert!(
            actions
                .iter()
                .all(|action| action.viewport_names().is_none())
        );
        scenario.actions = Some(Vec::new());
        assert!(legacy_actions(&scenario).is_empty());
    }

    #[test]
    fn explicit_viewports_files_must_exist_inside_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let viewport_path = workspace.path().join("common/viewports.yaml");
        std::fs::create_dir_all(viewport_path.parent().unwrap()).unwrap();
        std::fs::write(
            &viewport_path,
            "- label: desktop\n  width: 1280\n  height: 720\n",
        )
        .unwrap();

        assert_eq!(
            resolve_viewports_file(workspace.path(), "common/viewports.yaml", true).unwrap(),
            Some(viewport_path.canonicalize().unwrap())
        );
        assert!(
            resolve_viewports_file(workspace.path(), "common/missing.yaml", false)
                .unwrap()
                .is_none()
        );
        assert!(
            resolve_viewports_file(workspace.path(), outside.path().to_str().unwrap(), true,)
                .unwrap_err()
                .to_string()
                .contains("escapes workspace root")
        );
        assert!(
            resolve_viewports_file(workspace.path(), "common/missing.yaml", true)
                .unwrap_err()
                .to_string()
                .contains("does not exist")
        );
    }

    fn scenario(id: &str, yaml: &str) -> TestScenario {
        serde_yaml::from_str(&format!("id: {id}\nurl: https://example.com/{id}\n{yaml}")).unwrap()
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn selector_saves_and_final_captures_match_pixels_and_inventory() -> Result<()> {
        use crate::test::{browser_tests, capture};
        use serde_json::json;

        let root = match std::env::var_os("REGRESSIFY_SELECTOR_CAPTURE_FIXTURE") {
            Some(path) => PathBuf::from(path),
            None => {
                let workspace = tempfile::tempdir()?;
                let output = std::process::Command::new(std::env::current_exe()?)
                    .args([
                        "--exact",
                        "test::test_scenario::tests::selector_saves_and_final_captures_match_pixels_and_inventory",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("REGRESSIFY_SELECTOR_CAPTURE_FIXTURE", workspace.path())
                    .output()?;
                anyhow::ensure!(
                    output.status.success(),
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return Ok(());
            }
        };
        common::ROOT_DIR.set(root.clone()).unwrap();
        let output_dir = root.join(".regressify");
        common::REGRESSIFY_DIR.set(output_dir.clone()).unwrap();
        let site = browser_tests::Site::new(|request| {
            let _ = request.respond(tiny_http::Response::from_string(r#"<!doctype html>
                <style>
                body { margin: 0; height: 900px; background: rgb(14, 160, 80); }
                #final { position: absolute; left: 20px; top: 30px; width: 120px; height: 60px; background: rgb(200, 30, 30); }
                #checkpoint { position: absolute; left: 40px; top: 700px; width: 80px; height: 40px; background: rgb(40, 90, 200); }
                </style><div id="final"></div><div id="checkpoint" data-name="it's a card"></div><div id="hidden" hidden></div>"#)
                .with_header(tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap()));
        })?;
        let (mut browser, handler, directory) =
            browser_tests::launch_browser("selector-capture", Default::default()).await?;
        let result: Result<()> = async {
            let suite = TestSuite {
                id: Some("selectors".into()),
                test_date: Some("run".into()),
                ..Default::default()
            };
            let viewport = config::viewports::ViewportDef {
                label: "desktop".into(),
                width: 640,
                height: 240,
            };
            let full_page = Some((640, 900, [14, 160, 80]));
            let viewport_image = Some((640, 240, [14, 160, 80]));
            let final_clip = Some((120, 60, [200, 30, 30]));
            let checkpoint = Some((80, 40, [40, 90, 200]));
            let step = |name: &str| ScreenshotType::Step(name.into());
            for (id, mut source, expected) in [
                ("default", json!({}), vec![(ScreenshotType::Final, full_page)]),
                ("final-selector", json!({ "selector": "#final" }), vec![(ScreenshotType::Final, final_clip)]),
                ("final-viewport", json!({ "selector": "viewport" }), vec![(ScreenshotType::Final, viewport_image)]),
                (
                    "save-viewport",
                    json!({ "actions": [{ "save": "top", "selector": "viewport" }, { "wait": 0 }] }),
                    vec![(step("top"), viewport_image), (ScreenshotType::Final, full_page)],
                ),
                (
                    "independent-selectors",
                    json!({ "selector": "#final", "actions": [
                        { "save": "whole" },
                        { "save": "detail", "selector": "[data-name=\"it's a card\"]" },
                        { "wait": 0 }
                    ] }),
                    vec![(step("whole"), full_page), (step("detail"), checkpoint), (ScreenshotType::Final, final_clip)],
                ),
                (
                    "default-final-after-save",
                    json!({ "actions": [{ "save": "detail", "selector": "#checkpoint" }, { "wait": 0 }] }),
                    vec![(step("detail"), checkpoint), (ScreenshotType::Final, full_page)],
                ),
                (
                    "last-save",
                    json!({ "selector": "#missing", "actions": [{ "save": "detail", "selector": "#checkpoint" }] }),
                    vec![(step("detail"), checkpoint)],
                ),
                (
                    "explicit-finals",
                    json!({ "selectors": ["viewport", "#final"], "actions": [{ "save": "detail", "selector": "#checkpoint" }] }),
                    vec![(step("detail"), checkpoint), (ScreenshotType::Viewport, viewport_image), (ScreenshotType::Selector("#final".into()), final_clip)],
                ),
                (
                    "excluded-save",
                    json!({ "actions": [{ "save": "detail", "selector": "#missing", "viewportNames": "mobile" }] }),
                    vec![(ScreenshotType::Final, full_page)],
                ),
                (
                    "last-applicable-save",
                    json!({ "actions": [{ "save": "detail", "selector": "#checkpoint" }, { "wait": 0, "viewportNames": "mobile" }] }),
                    vec![(step("detail"), checkpoint)],
                ),
                (
                    "missing-final",
                    json!({ "selector": "#missing", "actions": [{ "save": "whole" }, { "wait": 0 }] }),
                    vec![(step("whole"), full_page), (ScreenshotType::Final, None)],
                ),
                (
                    "invalid-save",
                    json!({ "actions": [{ "save": "whole" }, { "save": "detail", "selector": "div[" }] }),
                    vec![(step("whole"), full_page), (step("detail"), None)],
                ),
                ("hidden-final", json!({ "selector": "#hidden" }), vec![(ScreenshotType::Final, None)]),
                (
                    "missing-save",
                    json!({ "actions": [{ "save": "detail", "selector": "#missing" }] }),
                    vec![(step("detail"), None)],
                ),
                ("peer-after-failures", json!({}), vec![(ScreenshotType::Final, full_page)]),
            ] {
                source["id"] = json!(id);
                source["url"] = json!(site.origin);
                let current: TestScenario = serde_json::from_value(source)?;
                let final_types = scenario_final_screenshot_types(&current, Some(&viewport.label))?;
                let resolved = ResolvedConfig {
                    no_scroll_top: true,
                    delay: Some(0.0),
                    post_interaction_wait: Some(if final_types.is_empty() { 60_000.0 } else { 0.0 }),
                    settle_timeout_ms: 0,
                    freeze_animations: false,
                    scenario_timeout_ms: 15_000,
                    ..Default::default()
                };
                let all_scenarios = std::slice::from_ref(&current);
                let outcome = run_scenario_capture(&browser, CaptureContext {
                    is_reference: false,
                    test_suite: &suite,
                    scenario: &current,
                    all_scenarios,
                    resolved: &resolved,
                    url: &current.url,
                    url_replacements: &[],
                    viewport: Some(&viewport),
                    log_prefix: id,
                }).await;
                assert_eq!(outcome.is_ok(), expected.iter().all(|(_, image)| image.is_some()), "{id}: {outcome:?}");
                if let Err(error) = outcome {
                    assert!(error.to_string().contains("selector"), "{id}: {error}");
                }
                let expected_keys = expected.iter().map(|(kind, _)| {
                    final_screenshot_order_key(id, Some(&viewport.label), kind)
                }).collect::<Vec<_>>();
                assert_eq!(capture_progress_keys(&current, all_scenarios, Some(&viewport.label))?, expected_keys, "{id}");
                let run_dir = output_dir.join("selectors/bitmaps_test/run");
                for (key, (_, pixels)) in expected_keys.iter().zip(&expected) {
                    let path = run_dir.join(format!("{key}.png"));
                    assert_eq!(capture::error_marker_path(&path).exists(), pixels.is_none(), "{key}");
                    let image = lodepng::decode32(std::fs::read(&path)?)?;
                    if let Some((width, height, color)) = pixels {
                        assert_eq!((image.width, image.height), (*width, *height), "{key}");
                        let pixel = image.buffer.first().unwrap();
                        assert_eq!([pixel.r, pixel.g, pixel.b], *color, "{key}");
                    }
                }
                let files = std::fs::read_dir(&run_dir)?
                    .collect::<std::io::Result<Vec<_>>>()?
                    .into_iter()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with(&format!("{id}_desktop__")))
                    .collect::<Vec<_>>();
                assert_eq!(files.iter().filter(|name| name.ends_with(".png")).count(), expected.len(), "{id}: {files:?}");
                assert_eq!(files.iter().filter(|name| name.ends_with(".error.json")).count(), expected.iter().filter(|(_, pixels)| pixels.is_none()).count(), "{id}: {files:?}");
                for page in browser.pages().await? {
                    let offsets: serde_json::Value = page.evaluate("[scrollX, scrollY]").await?.into_value()?;
                    assert_eq!(offsets, json!([0, 0]), "{id}");
                    page.close().await?;
                }
            }
            Ok(())
        }.await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn filtered_reference_and_test_manifests_preserve_baselines() -> Result<()> {
        use crate::test::capture;
        use clap::Parser;
        use serde_json::{Value, json};

        if let Some(root) = std::env::var_os("REGRESSIFY_FILTERED_RUN_FIXTURE") {
            let mode = std::env::var("REGRESSIFY_FILTERED_RUN_MODE")?;
            let command = if mode == "test" { "test" } else { "ref" };
            let arguments: Vec<std::ffi::OsString> = vec![
                "regressify".into(),
                "--root".into(),
                root,
                "--run-id".into(),
                mode.clone().into(),
                "--async-capture-limit".into(),
                "2".into(),
                "--async-capture-ramp-ms".into(),
                "0".into(),
                "--log-level".into(),
                "debug".into(),
                command.into(),
                "--fetch-browser".into(),
                "false".into(),
            ];
            return crate::application::execute(crate::cli::Cli::try_parse_from(arguments)?).await;
        }
        let site = super::super::browser_tests::Site::new(|request| {
            let _ = request.respond(
                tiny_http::Response::from_string(
                    "<body style='margin:0;min-height:100vh;background:rgb(14,160,80)'></body>",
                )
                .with_header(tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap()),
            );
        })?;
        let workspace = tempfile::tempdir()?;
        let root = workspace.path();
        std::fs::create_dir_all(root.join("visual_tests"))?;
        std::fs::create_dir_all(root.join("common"))?;
        std::fs::write(
            root.join("common/_viewports.yaml"),
            "- label: desktop\n  width: 640\n  height: 240\n- label: mobile\n  width: 320\n  height: 240\n",
        )?;
        let suite_path = root.join("visual_tests/fixture.tests.json");
        let mut suite = json!({
            "id": "fixture",
            "scenarios": [
                { "id": "filtered", "url": site.origin, "delay": 0, "noScrollTop": true,
                  "selectors": ["viewport"], "actions": [
                    { "save": "checkpoint", "viewportNames": ["desktop"] },
                    { "save": "checkpoint", "viewportNames": ["mobile"] },
                    { "save": "mobile-only", "viewportNames": ["mobile"] },
                    { "save": "shared" }
                  ] },
                { "id": "peer", "url": site.origin, "delay": 0, "noScrollTop": true,
                                    "selectors": ["viewport"] },
                                { "id": "all-skipped", "url": site.origin, "delay": 0, "noScrollTop": true,
                                    "selectors": ["viewport"], "actions": [
                                        { "wait": 0, "viewportNames": ["mobile"] },
                                        { "save": "mobile-only", "viewportNames": ["mobile"] }
                                    ] }
            ]
        });
        std::fs::write(&suite_path, serde_json::to_vec_pretty(&suite)?)?;
        let suite_dir = root.join(".regressify/fixture");
        let references = suite_dir.join("bitmaps_reference");
        std::fs::create_dir_all(&references)?;
        let historical_key = final_screenshot_order_key(
            "filtered",
            Some("desktop"),
            &ScreenshotType::Step("mobile-only".into()),
        );
        let historical = references.join(format!("{historical_key}.png"));
        image::RgbaImage::from_pixel(2, 2, image::Rgba([7, 8, 9, 255])).save(&historical)?;
        let historical_bytes = std::fs::read(&historical)?;
        let run = |mode: &str| -> Result<()> {
            let output = std::process::Command::new(std::env::current_exe()?)
                .args(["--ignored", "--exact", "test::test_scenario::tests::filtered_reference_and_test_manifests_preserve_baselines", "--nocapture"])
                .env("REGRESSIFY_FILTERED_RUN_FIXTURE", root)
                .env("REGRESSIFY_FILTERED_RUN_MODE", mode)
                .env_remove("REPLACEMENT_PROFILE")
                .output()?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            anyhow::ensure!(output.status.success(), "{mode}: {text}");
            anyhow::ensure!(
                text.contains("Skipping Save action for viewport \"desktop\""),
                "missing real-command skip diagnostic: {text}"
            );
            Ok(())
        };
        run("reference")?;
        run("test")?;
        let read_json = |run: &str, name: &str| -> Result<Value> {
            Ok(serde_json::from_slice(&std::fs::read(
                suite_dir.join("bitmaps_test").join(run).join(name),
            )?)?)
        };
        let mut expected = Vec::new();
        for (scenario_id, label, steps) in [
            ("filtered", "desktop", vec!["checkpoint", "shared"]),
            (
                "filtered",
                "mobile",
                vec!["checkpoint", "mobile-only", "shared"],
            ),
            ("peer", "desktop", vec![]),
            ("peer", "mobile", vec![]),
            ("all-skipped", "desktop", vec![]),
            ("all-skipped", "mobile", vec!["mobile-only"]),
        ] {
            for step in steps {
                expected.push(final_screenshot_order_key(
                    scenario_id,
                    Some(label),
                    &ScreenshotType::Step(step.into()),
                ));
            }
            expected.push(final_screenshot_order_key(
                scenario_id,
                Some(label),
                &ScreenshotType::Viewport,
            ));
        }
        for name in [
            capture::REPORT_ORDER_FILE,
            capture::REPORT_THRESHOLDS_FILE,
            capture::REPORT_PIXEL_THRESHOLDS_FILE,
            capture::REPORT_URLS_FILE,
        ] {
            assert_eq!(
                read_json("reference", name)?,
                read_json("test", name)?,
                "{name}"
            );
        }
        assert_eq!(
            read_json("test", capture::REPORT_ORDER_FILE)?["captures"],
            json!(expected)
        );
        for run in ["reference", "test"] {
            let report = read_json(run, "report.json")?;
            let tests = report["tests"].as_array().unwrap();
            assert_eq!(tests.len(), expected.len());
            assert!(tests.iter().all(|test| test["status"] == "pass"));
            let order = tests
                .iter()
                .map(|test| {
                    test["pair"]["fileName"]
                        .as_str()
                        .unwrap()
                        .trim_end_matches(".png")
                })
                .collect::<Vec<_>>();
            assert_eq!(order, expected);
            let run_dir = suite_dir.join("bitmaps_test").join(run);
            assert_eq!(
                crate::compare::report_gen::validate_capture_run_inventory(&run_dir)?.len(),
                expected.len()
            );
            for key in &expected {
                let path = run_dir.join(format!("{key}.png"));
                let image = lodepng::decode32(std::fs::read(&path)?)?;
                let pixel = image.buffer.first().unwrap();
                assert_eq!((pixel.r, pixel.g, pixel.b), (14, 160, 80), "{key}");
                assert!(!capture::error_marker_path(&path).exists());
            }
        }
        assert_eq!(std::fs::read(&historical)?, historical_bytes);
        let baseline_bytes = expected
            .iter()
            .map(|key| {
                let path = references.join(format!("{key}.png"));
                Ok((path.clone(), std::fs::read(path)?))
            })
            .collect::<Result<Vec<_>>>()?;
        suite["scenarios"][0]["actions"]
            .as_array_mut()
            .unwrap()
            .insert(
                0,
                json!({
                    "wait": 100, "url": "**/never-reached", "viewportNames": ["mobile"]
                }),
            );
        // A pre-action failure must still omit saves when every desktop action is excluded.
        suite["scenarios"][2]["readySelector"] = json!("#never-ready");
        suite["scenarios"][2]["readyTimeout"] = json!(100);
        std::fs::write(&suite_path, serde_json::to_vec_pretty(&suite)?)?;
        run("failed-reference")?;
        assert_eq!(
            read_json("failed-reference", capture::REPORT_ORDER_FILE)?["captures"],
            json!(expected)
        );
        let failed_report = read_json("failed-reference", "report.json")?;
        let tests = failed_report["tests"].as_array().unwrap();
        assert_eq!(
            tests.iter().filter(|test| test["status"] == "fail").count(),
            7
        );
        assert_eq!(
            tests.iter().filter(|test| test["status"] == "pass").count(),
            5
        );
        for key in &expected {
            let path = suite_dir
                .join("bitmaps_test/failed-reference")
                .join(format!("{key}.png"));
            assert_eq!(
                capture::error_marker_path(&path).exists(),
                key.starts_with("filtered_mobile") || key.starts_with("all-skipped_")
            );
        }
        let excluded_key = final_screenshot_order_key(
            "all-skipped",
            Some("desktop"),
            &ScreenshotType::Step("mobile-only".into()),
        );
        for run in ["reference", "test", "failed-reference"] {
            let excluded = suite_dir
                .join("bitmaps_test")
                .join(run)
                .join(format!("{excluded_key}.png"));
            assert!(!excluded.exists(), "excluded save returned in {run}");
            assert!(!capture::error_marker_path(&excluded).exists());
        }
        for (path, bytes) in baseline_bytes {
            assert_eq!(std::fs::read(&path)?, bytes, "{}", path.display());
            assert!(!capture::error_marker_path(&path).exists());
        }
        assert_eq!(std::fs::read(historical)?, historical_bytes);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn tab_tracking_uses_only_applicable_own_and_inherited_actions() -> Result<()> {
        let scenarios = [
            scenario(
                "base",
                "actions:\n  - switchTab: new\n    viewportNames: [MOBILE, mobile]\n",
            ),
            scenario("nested", "needs: base\n"),
            scenario("consumer", "needs: nested\n"),
            scenario("unscoped", "actions:\n  - switchTab: new\n"),
            scenario(
                "empty",
                "actions:\n  - switchTab: new\n    viewportNames: []\n",
            ),
            scenario(
                "ordinary",
                "actions:\n  - wait: 0\n    viewportNames: [mobile]\n",
            ),
        ];
        let resolved = ResolvedConfig::default();
        let (mut browser, handler, directory) =
            super::super::browser_tests::launch_browser("viewport-tabs", Default::default())
                .await?;
        let result: Result<()> = async {
            for (source, expected) in [
                (&scenarios[0], [false, true, false]),
                (&scenarios[1], [false, true, false]),
                (&scenarios[2], [false, true, false]),
                (&scenarios[3], [true, true, true]),
                (&scenarios[4], [true, true, true]),
                (&scenarios[5], [false, false, false]),
            ] {
                for (label, expected) in [Some("desktop"), Some("mobile"), None]
                    .into_iter()
                    .zip(expected)
                {
                    let viewport = label.map(|label| config::viewports::ViewportDef {
                        label: label.into(),
                        width: 800,
                        height: 600,
                    });
                    let page = browser.new_page("about:blank").await?;
                    let storage = Arc::new(
                        super::super::storage_state::StorageState::install(
                            &browser,
                            &page,
                            None,
                            "http://127.0.0.1",
                        )
                        .await?,
                    );
                    let inherited = collect_needs_actions(source, &scenarios, &mut Vec::new());
                    let tabs = install_tab_tracker_for_actions(
                        &page,
                        storage,
                        &resolved,
                        viewport.as_ref(),
                        inherited
                            .iter()
                            .copied()
                            .chain(source.actions.iter().flatten()),
                    )
                    .await?;
                    anyhow::ensure!(
                        tabs.is_some() == expected,
                        "{} {label:?}: expected tracker={expected}",
                        source.id.as_deref().unwrap()
                    );
                    let before_navigation: bool = page
                        .evaluate("location.href === 'about:blank'")
                        .await?
                        .into_value()?;
                    anyhow::ensure!(before_navigation, "tab setup must precede navigation");
                    if let Some(tabs) = tabs {
                        tabs.check().await?;
                    }
                    page.close().await?;
                }
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
    async fn skipped_viewport_actions_do_not_touch_documents_or_dispatch() -> Result<()> {
        use serde_json::json;
        use std::{
            sync::atomic::{AtomicUsize, Ordering},
            time::Duration,
        };

        let navigation_count = Arc::new(AtomicUsize::new(0));
        let observed_navigation = navigation_count.clone();
        let site = super::super::browser_tests::Site::new(move |request| {
            if request.url() == "/excluded" {
                observed_navigation.fetch_add(1, Ordering::SeqCst);
            }
            let _ = request.respond(tiny_http::Response::from_string(
                "<body><button id='shared' onclick='window.clicks = (window.clicks || 0) + 1'>Continue</button></body>"
            ).with_header(tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap()));
        })?;
        let workspace = tempfile::tempdir()?;
        std::fs::create_dir(workspace.path().join("common"))?;
        std::fs::write(
            workspace.path().join("common/_on-ready.js"),
            "window.hookCalls = (window.hookCalls || 0) + 1;",
        )?;
        let scenario = TestScenario::default();
        let suite = TestSuite::default();
        let resolved = ResolvedConfig {
            element_timeout_ms: 250,
            ..Default::default()
        };
        let hooks =
            super::super::hooks::DocumentHooks::load(workspace.path(), &scenario, &resolved)?;
        let (mut browser, handler, directory) =
            super::super::browser_tests::launch_browser("viewport-gate", Default::default())
                .await?;
        let result: Result<()> = async {
            for (label, excluded) in [("desktop", "mobile"), ("mobile", "desktop")] {
                let page = browser.new_page(site.origin.as_str()).await?;
                let runtime = super::super::runtime::Runtime::new(&browser, &page).await?;
                let storage = Arc::new(
                    super::super::storage_state::StorageState::install(
                        &browser,
                        &page,
                        None,
                        &site.origin,
                    )
                    .await?,
                );
                let mut active = ScenarioPage {
                    page,
                    runtime,
                    storage,
                    authentication: None,
                    tabs: None,
                };
                let viewport = config::viewports::ViewportDef { label: label.into(), width: 800, height: 600 };
                let context = CaptureContext {
                    is_reference: false,
                    test_suite: &suite,
                    scenario: &scenario,
                    all_scenarios: &[],
                    resolved: &resolved,
                    url: &site.origin,
                    url_replacements: &[],
                    viewport: Some(&viewport),
                    log_prefix: "viewport-gate",
                };
                let mut skipped = Vec::new();
                for mut shape in [
                    json!({ "switchTab": "new" }),
                    json!({ "check": "#never" }),
                    json!({ "click": "#never", "frame": ["#absent-frame", "#nested"] }),
                    json!({ "focus": "#never" }),
                    json!({ "goto": format!("{}/excluded", site.origin) }),
                    json!({ "hide": "#shared" }),
                    json!({ "hover": "#never" }),
                    json!({ "input": "#never", "file": "missing-upload.txt", "useFileChooser": true }),
                    json!({ "persist": "excluded-state" }),
                    json!({ "press": "#never", "key": "Enter" }),
                    json!({ "remove": "#shared" }),
                    json!({ "save": "excluded-checkpoint" }),
                    json!({ "select": "#never", "value": "missing" }),
                    json!({ "scroll": "#never" }),
                    json!({ "uncheck": "#never" }),
                    json!({ "wait": 60000 }),
                    json!({ "drag": "#never", "to": "#missing" }),
                    json!({ "dropFiles": "#never", "file": "missing-file.txt" }),
                    json!({ "wheel": "#never", "deltaY": 100 }),
                    json!({ "navigate": "back" }),
                    json!({ "dialog": "accept" }),
                    json!({ "key": "Tab" }),
                    json!({ "tap": "#never" }),
                    json!({ "swipe": "#never", "offset": {"x": 100, "y": 0} }),
                    json!({ "longPress": "#never", "duration": 60000 }),
                    json!({ "blur": "#never" }),
                    json!({ "pinch": "#never", "scale": 2.0, "duration": 60000 }),
                    json!({ "selectText": "#never", "text": "missing" }),
                    json!({ "paste": "#never", "value": "excluded" }),
                    json!({ "closeTab": "current" }),
                ] {
                    shape["viewportNames"] = json!([excluded]);
                    skipped.push(serde_json::from_value::<TestAction>(shape)?);
                }
                for action in &skipped {
                    tokio::time::timeout(Duration::from_millis(200), active.exec(&browser, context, &hooks, action)).await??;
                }
                let untouched: bool = active.page.evaluate("window.hookCalls === undefined && window.clicks === undefined && !!document.querySelector('#shared')").await?.into_value()?;
                anyhow::ensure!(untouched, "{label}: an excluded action touched the document");
                let click: TestAction = serde_json::from_value(json!({ "click": "#shared", "viewportNames": [label] }))?;
                active.exec(&browser, context, &hooks, &click).await?;
                let clicked: bool = active.page.evaluate("window.hookCalls === 1 && window.clicks === 1").await?.into_value()?;
                anyhow::ensure!(clicked, "{label}: matching action or document hook did not run");
                let failing: TestAction = serde_json::from_value(json!({ "switchTab": "new", "viewportNames": [label] }))?;
                let error = active.exec(&browser, context, &hooks, &failing).await.expect_err("a matching tab action must retain its failure");
                anyhow::ensure!(error.to_string().contains("Tab tracking was not initialized"));
                active.page.clone().close().await?;
                for action in &skipped {
                    tokio::time::timeout(Duration::from_millis(200), active.exec(&browser, context, &hooks, action)).await??;
                }
            }
            anyhow::ensure!(navigation_count.load(Ordering::SeqCst) == 0, "excluded navigation ran");
            anyhow::ensure!(std::fs::read_dir(workspace.path())?.count() == 1, "excluded action created output");
            Ok(())
        }.await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn popup_actions_and_captures_follow_the_selected_tab() -> Result<()> {
        let root = match std::env::var_os("REGRESSIFY_POPUP_CAPTURE_FIXTURE") {
            Some(path) => PathBuf::from(path),
            None => {
                let workspace = tempfile::tempdir()?;
                let output = std::process::Command::new(std::env::current_exe()?)
                    .args([
                        "--exact",
                        "test::test_scenario::tests::popup_actions_and_captures_follow_the_selected_tab",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("REGRESSIFY_POPUP_CAPTURE_FIXTURE", workspace.path())
                    .output()?;
                anyhow::ensure!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return Ok(());
            }
        };
        common::ROOT_DIR.set(root.clone()).unwrap();
        let output_dir = root.join(".regressify");
        common::REGRESSIFY_DIR.set(output_dir.clone()).unwrap();
        std::fs::create_dir_all(root.join("common"))?;
        std::fs::write(
            root.join("common/_on-ready.js"),
            "window.hookCount = (window.hookCount || 0) + 1;",
        )?;
        let authenticated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let authenticated_request = authenticated.clone();
        let destination = crate::test::browser_tests::Site::new(move |request| {
            let authorized = request.headers().iter().any(|header| {
                header.field.equiv("Authorization") && header.value.as_str() == "Basic dXNlcjpwYXNz"
            });
            let response = if authorized {
                authenticated_request.store(true, std::sync::atomic::Ordering::Relaxed);
                tiny_http::Response::from_string(
                    r#"<!doctype html>
                    <script>window.restored = localStorage.getItem('fixture'); window.firstWidth = innerWidth;</script>
                    <style>body{margin:0;min-height:100vh;background:rgb(0,255,0)}</style>
                    <button id="change" style="margin:40px" onclick="document.body.style.background='rgb(0,0,255)'; localStorage.setItem('popup', 'changed')">Change</button>
                    <button id="close" onclick="window.close()">Close</button>"#,
                )
            } else {
                tiny_http::Response::from_string("unauthorized")
                    .with_status_code(tiny_http::StatusCode(401))
                    .with_header(
                        tiny_http::Header::from_bytes(
                            "WWW-Authenticate",
                            "Basic realm=\"popup-fixture\"",
                        )
                        .unwrap(),
                    )
            };
            let _ =
                request.respond(response.with_header(
                    tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap(),
                ));
        })?;
        let destination_origin = destination.origin.clone();
        let leaked_auth = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let unexpected_auth = leaked_auth.clone();
        let site = crate::test::browser_tests::Site::new(move |request| {
            if request
                .headers()
                .iter()
                .any(|header| header.field.equiv("Authorization"))
            {
                unexpected_auth.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let content = format!(
                r#"<!doctype html>
                <script>localStorage.setItem('original', 'retained');</script>
                <style>body{{margin:0;min-height:100vh;background:rgb(255,0,0)}}</style>
                <a id="open" style="margin:40px;display:block" href="{destination_origin}/popup" target="_blank" rel="noopener">Open popup</a>
                <button id="second" onclick="window.open('{destination_origin}/popup?second')">Second popup</button>"#
            );
            let _ =
                request.respond(tiny_http::Response::from_string(content).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap(),
                ));
        })?;
        let state_path = super::super::capture::state_path("popup-initial")?;
        std::fs::create_dir_all(state_path.parent().unwrap())?;
        std::fs::write(
            state_path,
            serde_json::to_vec(&serde_json::json!({
                "cookies": [], "origins": [{"origin": destination.origin,
                    "localStorage": [{"name": "fixture", "value": "restored-before-scripts"}]}]
            }))?,
        )?;
        let viewport = config::viewports::ViewportDef {
            label: "phone".into(),
            width: 360,
            height: 240,
        };
        let (mut browser, handler, directory) = crate::test::browser_tests::launch_browser(
            "popup-capture",
            super::super::browsers::LaunchOptions {
                viewport: Some((viewport.width, viewport.height)),
                ..Default::default()
            },
        )
        .await?;
        let result: Result<()> = async {
            let suite = TestSuite { id: Some("tabs".into()), test_date: Some("run".into()), ..Default::default() };
            let resolved = ResolvedConfig {
                no_scroll_top: true, delay: Some(0.0), post_interaction_wait: Some(0.0),
                settle_timeout_ms: 0, freeze_animations: false, element_timeout_ms: 5000,
                state: Some(Needs::String("popup-initial".into())),
                basic_auth: vec![crate::types::BasicAuth { origin: destination.origin.clone(), username: "user".into(), password: "pass".into() }],
                ..Default::default()
            };
            let setup = scenario("open-popup", "actions:\n  - click: '#open'\n  - switchTab: new\n  - wait: '#change'\n    url: '**/popup'\n  - save: popup\n");
            let mut current = scenario("popup-capture", "needs: open-popup\nselectors: [viewport]\nactions:\n  - click: '#change'\n  - save: changed\n  - persist: after-popup\n  - click: '#close'\n  - switchTab: previous\n  - wait: '#open'\n  - save: original\n  - click: '#second'\n  - switchTab: new\n    url: '**/popup?second'\n  - wait: '#change'\n");
            current.url = site.origin.clone();
            let all_scenarios = [setup, current.clone()];
            run_scenario_capture_inner(&browser, CaptureContext {
                is_reference: false, test_suite: &suite, scenario: &current,
                all_scenarios: &all_scenarios, resolved: &resolved, url: &current.url,
                url_replacements: &[], viewport: Some(&viewport), log_prefix: "popup-capture",
            }, &final_screenshot_types(&current)?).await?;
            for (kind, color) in [
                (ScreenshotType::Step("popup".into()), (0, 255, 0)),
                (ScreenshotType::Step("changed".into()), (0, 0, 255)),
                (ScreenshotType::Step("original".into()), (255, 0, 0)),
                (ScreenshotType::Viewport, (0, 255, 0)),
            ] {
                let key = final_screenshot_order_key("popup-capture", Some(&viewport.label), &kind);
                let path = output_dir.join("tabs/bitmaps_test/run").join(format!("{key}.png"));
                let image = lodepng::decode32(std::fs::read(path)?)?;
                let pixel = image.buffer.first().ok_or_else(|| anyhow!("empty tab screenshot"))?;
                anyhow::ensure!((pixel.r, pixel.g, pixel.b) == color, "{key} captured the wrong tab: {pixel:?}");
                anyhow::ensure!(image.width == viewport.width as usize, "{key} used the wrong viewport");
            }
            let mut pages_checked = 0;
            for page in browser.pages().await? {
                if page.url().await?.is_some_and(|url| url.starts_with(&site.origin) || url.starts_with(&destination.origin)) {
                    let hooks: u32 = page.evaluate("window.hookCount").await?.into_value()?;
                    anyhow::ensure!(hooks == 1, "document hooks did not run exactly once on each tab");
                    if page.url().await?.is_some_and(|url| url.starts_with(&destination.origin)) {
                        let restored: String = page.evaluate("window.restored").await?.into_value()?;
                        anyhow::ensure!(restored == "restored-before-scripts", "popup scripts ran before storage restoration");
                        let width: u32 = page.evaluate("window.firstWidth").await?.into_value()?;
                        anyhow::ensure!(width == viewport.width, "popup first-load viewport was incorrect");
                    }
                    pages_checked += 1;
                }
            }
            anyhow::ensure!(pages_checked == 2, "scenario did not retain the original and the surviving popup");
            anyhow::ensure!(authenticated.load(std::sync::atomic::Ordering::Relaxed), "popup authentication was not handled");
            anyhow::ensure!(!leaked_auth.load(std::sync::atomic::Ordering::Relaxed), "credentials reached an unconfigured origin");
            let (_, persisted) = super::super::capture::load_persisted_state("after-popup")?;
            let origins = super::super::storage_state::validate_origins(&persisted)?;
            anyhow::ensure!(origins[&site.origin]["original"] == "retained", "persist lost the original tab's origin");
            anyhow::ensure!(origins[&destination.origin]["popup"] == "changed", "persist lost popup storage");
            Ok(())
        }.await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn lazy_loading_precedes_actions_and_preserves_hover() -> Result<()> {
        let root = match std::env::var_os("REGRESSIFY_LAZY_LOADING_FIXTURE") {
            Some(path) => PathBuf::from(path),
            None => {
                let workspace = tempfile::tempdir()?;
                let output = std::process::Command::new(std::env::current_exe()?)
                    .args([
                        "--exact",
                        "test::test_scenario::tests::lazy_loading_precedes_actions_and_preserves_hover",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("REGRESSIFY_LAZY_LOADING_FIXTURE", workspace.path())
                    .output()?;
                anyhow::ensure!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return Ok(());
            }
        };
        common::ROOT_DIR.set(root).unwrap();
        let site = crate::test::browser_tests::Site::new(|request| {
            let _ = request.respond(
                tiny_http::Response::from_string(
                    r#"<!doctype html><style>
                    html { scroll-behavior: smooth; }
                    body { margin: 0; height: 12000px; }
                    #target { margin: 40px; width: 120px; height: 50px; }
                    #target:hover { background: green; }
                    </style><button id="target">Hover</button><script>
                    window.fixture = { scrolls: 0, scrollsAtHover: null, scrolledAfterHover: false };
                    window.initialAnimation = document.body.animate(
                        [{ opacity: 0.8 }, { opacity: 1 }], { duration: 60000, fill: 'forwards' }
                    );
                    window.addEventListener('scroll', () => {
                        window.fixture.scrolls++;
                        if (window.fixture.scrollsAtHover !== null)
                            window.fixture.scrolledAfterHover = true;
                    });
                    document.querySelector('#target').addEventListener('pointermove', () => {
                        if (window.fixture.scrollsAtHover === null) {
                            window.fixture.scrollsAtHover = window.fixture.scrolls;
                            window.fixture.animationStateAtHover = window.initialAnimation.playState;
                        }
                    });
                    </script>"#,
                )
                .with_header(tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap()),
            );
        })?;
        let (mut browser, handler, directory) =
            crate::test::browser_tests::launch_browser("lazy-loading", Default::default()).await?;
        let result: Result<()> = async {
            let suite = TestSuite::default();
            let mut prerequisite = scenario("prerequisite", "actions:\n  - hover: '#target'\n");
            prerequisite.url = site.origin.clone();
            for (no_scroll_top, freeze_animations) in
                [(false, false), (false, true), (true, false), (true, true)]
            {
                let resolved = ResolvedConfig {
                    no_scroll_top,
                    post_interaction_wait: Some(0.0),
                    delay: Some(0.0),
                    settle_timeout_ms: 0,
                    freeze_animations,
                    ..Default::default()
                };
                for (kind, yaml) in [
                    ("no-actions", ""),
                    ("empty-actions", "actions: []\n"),
                    ("actions", "actions:\n  - hover: '#target'\n"),
                    ("needs", "needs: prerequisite\n"),
                    ("legacy", "hoverSelector: '#target'\n"),
                ] {
                    let mut current = scenario(kind, yaml);
                    current.url = format!("{}/{kind}-{no_scroll_top}-{freeze_animations}", site.origin);
                    let all_scenarios = [prerequisite.clone(), current.clone()];
                    run_with_scenario_timeout(
                        std::time::Duration::from_secs(20),
                        kind,
                        run_scenario_capture_inner(
                            &browser,
                            CaptureContext {
                                is_reference: true,
                                test_suite: &suite,
                                scenario: &current,
                                all_scenarios: &all_scenarios,
                                resolved: &resolved,
                                url: &current.url,
                                url_replacements: &[],
                                viewport: None,
                                log_prefix: kind,
                            },
                            &[],
                        ),
                    )
                    .await?;
                    let mut checked = false;
                    for page in browser.pages().await? {
                        if page.url().await?.as_deref() != Some(current.url.as_str()) {
                            continue;
                        }
                        let state: serde_json::Value = page
                            .evaluate(
                                "({ ...window.fixture, hovered: document.querySelector('#target').matches(':hover'), atTop: window.scrollY === 0 })",
                            )
                            .await?
                            .into_value()?;
                        anyhow::ensure!(
                            state["scrolledAfterHover"] == false,
                            "{kind} hover was disturbed (noScrollTop={no_scroll_top}): {state}"
                        );
                        let scrolls = if matches!(kind, "no-actions" | "empty-actions") {
                            state["scrolls"].as_u64()
                        } else {
                            anyhow::ensure!(
                                state["hovered"] == true,
                                "{kind} did not preserve hover: {state}"
                            );
                            let expected_animation = if freeze_animations { "finished" } else { "running" };
                            anyhow::ensure!(
                                state["animationStateAtHover"] == expected_animation,
                                "{kind} initial animations were not prepared before actions: {state}"
                            );
                            state["scrollsAtHover"].as_u64()
                        }
                        .ok_or_else(|| anyhow!("{kind} scroll state was not recorded: {state}"))?;
                        anyhow::ensure!(
                            (scrolls == 0) == no_scroll_top && state["atTop"] == true,
                            "{kind} lazy-load ordering or noScrollTop was incorrect: {state}"
                        );
                        page.close().await?;
                        checked = true;
                    }
                    anyhow::ensure!(checked, "{kind} capture page was not found");
                }
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
    async fn scenario_timeout_cancels_stalled_work() {
        let result = run_with_scenario_timeout(
            std::time::Duration::from_millis(1),
            "stalled scenario",
            std::future::pending::<Result<()>>(),
        )
        .await;

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("stalled scenario' exceeded its 1ms timeout")
        );
    }

    #[test]
    fn viewport_capture_inventories_match_order_and_progress() {
        let root = match std::env::var_os("REGRESSIFY_ACTION_VIEWPORT_INVENTORY_FIXTURE") {
            Some(root) => PathBuf::from(root),
            None => {
                let workspace = tempfile::tempdir().unwrap();
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "test::test_scenario::tests::viewport_capture_inventories_match_order_and_progress", "--nocapture"])
                    .env("REGRESSIFY_ACTION_VIEWPORT_INVENTORY_FIXTURE", workspace.path())
                    .output().unwrap();
                assert!(
                    output.status.success(),
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
        };
        common::ROOT_DIR.set(root.clone()).unwrap();
        std::fs::create_dir_all(root.join("common")).unwrap();
        std::fs::write(root.join("common/_viewports.yaml"), "- label: desktop\n  width: 1280\n  height: 720\n- label: mobile\n  width: 375\n  height: 667\n").unwrap();
        let suite = TestSuite {
            scenarios: vec![
                scenario(
                    "base",
                    "actions:\n  - save: base-mobile\n    viewportNames: [mobile]\n  - save: base-all\n",
                ),
                scenario("left", "needs: base\n"),
                scenario("right", "needs: base\n"),
                scenario(
                    "consumer",
                    "needs: [left, right]\nactions:\n  - save: shared\n    viewportNames: []\n  - save: mobile-only\n    viewportNames: [MOBILE, mobile]\nselectors: [viewport]\n",
                ),
            ],
            ..Default::default()
        };
        let consumer = &suite.scenarios[3];
        let before = serde_json::to_value(&suite).unwrap();
        let actual_order = scenario_capture_order_keys(
            &WorkspaceConfig::default(),
            &suite,
            consumer,
            &suite.scenarios,
        )
        .unwrap();
        let mut expected_order = Vec::new();
        for (label, names) in [
            ("desktop", vec!["base-all", "shared"]),
            (
                "mobile",
                vec!["base-mobile", "base-all", "shared", "mobile-only"],
            ),
        ] {
            let mut types = names
                .into_iter()
                .map(|name| ScreenshotType::Step(name.into()))
                .collect::<Vec<_>>();
            types.push(ScreenshotType::Viewport);
            assert_eq!(
                scenario_screenshot_types(consumer, &suite.scenarios, Some(label)).unwrap(),
                types,
                "{label}"
            );
            let keys = types
                .iter()
                .map(|kind| final_screenshot_order_key("consumer", Some(label), kind))
                .collect::<Vec<_>>();
            assert_eq!(
                capture_progress_keys(consumer, &suite.scenarios, Some(label)).unwrap(),
                keys,
                "{label}"
            );
            expected_order.extend(keys);
        }
        assert_eq!(actual_order, expected_order);
        assert_eq!(serde_json::to_value(&suite).unwrap(), before);
    }

    #[test]
    fn final_capture_depends_on_last_applicable_own_action() {
        let setup = scenario("setup", "actions:\n  - save: prepared\n");
        for (yaml, viewport, expect_final) in [
            ("", None, true),
            ("actions: []\n", None, true),
            ("clickSelector: '#button'\n", None, true),
            ("actions:\n  - save: ready\n", None, false),
            ("actions:\n  - save: ready\n  - wait: 0\n", None, true),
            (
                "actions:\n  - save: ready\n    viewportNames: mobile\n",
                None,
                true,
            ),
            (
                "actions:\n  - save: ready\n    viewportNames: mobile\n",
                Some("desktop"),
                true,
            ),
            (
                "actions:\n  - save: ready\n    viewportNames: mobile\n",
                Some("MOBILE"),
                false,
            ),
            (
                "actions:\n  - save: ready\n  - wait: 0\n    viewportNames: mobile\n",
                Some("desktop"),
                false,
            ),
            (
                "actions:\n  - save: ready\n  - wait: 0\n    viewportNames: mobile\n",
                Some("mobile"),
                true,
            ),
            (
                "selector: '#content'\nactions:\n  - save: ready\n",
                None,
                false,
            ),
        ] {
            let current = scenario("page", &format!("needs: setup\n{yaml}"));
            let all_scenarios = [setup.clone(), current.clone()];
            assert_eq!(
                scenario_final_screenshot_types(&current, viewport).unwrap(),
                if expect_final {
                    vec![ScreenshotType::Final]
                } else {
                    Vec::new()
                },
                "{yaml} at {viewport:?}"
            );
            let types = scenario_screenshot_types(&current, &all_scenarios, viewport).unwrap();
            assert_eq!(
                types.first(),
                Some(&ScreenshotType::Step("prepared".into()))
            );
            assert_eq!(types.contains(&ScreenshotType::Final), expect_final);
        }
    }

    #[test]
    fn save_as_last_action_preserves_explicit_final_targets() {
        for (selectors, expected) in [
            ("viewport", vec![ScreenshotType::Viewport]),
            (
                "'#content'",
                vec![ScreenshotType::Selector("#content".into())],
            ),
            (
                "[viewport, '#content']",
                vec![
                    ScreenshotType::Viewport,
                    ScreenshotType::Selector("#content".into()),
                ],
            ),
        ] {
            let current = scenario(
                "page",
                &format!("selectors: {selectors}\nactions:\n  - save: ready\n"),
            );
            assert_eq!(
                scenario_final_screenshot_types(&current, None).unwrap(),
                expected
            );
            let mut inventory = vec![ScreenshotType::Step("ready".into())];
            inventory.extend(expected);
            assert_eq!(
                scenario_screenshot_types(&current, std::slice::from_ref(&current), None).unwrap(),
                inventory
            );
        }
    }

    #[test]
    fn save_as_last_action_omits_final_capture() {
        let current = scenario("page", "actions:\n  - save: ready\n");
        let all_scenarios = std::slice::from_ref(&current);

        assert_eq!(
            scenario_screenshot_types(&current, all_scenarios, None).unwrap(),
            vec![ScreenshotType::Step("ready".into())]
        );
        assert_eq!(
            capture_progress_keys(&current, all_scenarios, None).unwrap(),
            vec!["page__$ready"]
        );
    }

    #[test]
    fn capture_order_includes_prerequisite_and_current_save_steps() {
        let setup = scenario("setup", "actions:\n  - save: prepared\n");
        let page = scenario(
            "page",
            "needs: setup\nactions:\n  - save: ready\nselectors:\n  - viewport\n",
        );
        let all = vec![setup, page];

        let types = scenario_screenshot_types(&all[1], &all, None).unwrap();

        assert_eq!(
            types,
            vec![
                ScreenshotType::Step("prepared".into()),
                ScreenshotType::Step("ready".into()),
                ScreenshotType::Viewport,
            ]
        );
    }

    #[test]
    fn capture_order_rejects_duplicate_inherited_save_steps() {
        let setup = scenario("setup", "actions:\n  - save: current\n");
        let page = scenario("page", "needs: setup\nactions:\n  - save: current\n");
        let all = vec![setup, page];

        let error = scenario_screenshot_types(&all[1], &all, None).unwrap_err();

        assert!(error.to_string().contains("duplicate filename"));
    }
}
