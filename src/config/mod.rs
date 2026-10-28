#![allow(dead_code)]

pub mod basic_auth;
pub mod replacements;
pub mod viewports;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    common,
    types::{
        BasicAuth, BasicAuthConfig, Browser, ImageSimplification, Needs, Selectors, TestScenario,
        TestSuite, UrlReplacement,
    },
};

pub const DEFAULT_DELAY_MS: f64 = 1000.0;
pub const DEFAULT_POST_INTERACTION_WAIT_MS: f64 = 1000.0;
pub const DEFAULT_ASYNC_COMPARE_LIMIT: usize = 2;
pub const DEFAULT_ASYNC_CAPTURE_LIMIT: usize = 5;
pub const DEFAULT_PIXEL_MATCH_THRESHOLD: f64 = 5.0;
pub const DEFAULT_LAZY_LOAD_SCROLL_DELAY_MS: u64 = 250;
pub const DEFAULT_LAZY_LOAD_TIMEOUT_MS: u64 = 20_000;
pub const DEFAULT_IMAGE_SIMPLIFICATION_BLOCK_SIZE: u32 = 4;
// Block size limits: Chromium caps `feConvolveMatrix` kernels at 16x16, and the
// block filter needs an even size so its per-block sample and dilation tile exactly.
pub const MIN_IMAGE_SIMPLIFICATION_BLOCK_SIZE: u32 = 2;
pub const MAX_IMAGE_SIMPLIFICATION_BLOCK_SIZE: u32 = 16;
pub const DEFAULT_IMAGE_SIMPLIFICATION_MIN_SIZE: u32 = 64;

/// Workspace-level configuration loaded from `regressify.yaml` or `regressify.yml`.
/// All fields are optional — missing fields inherit defaults.
#[derive(Deserialize, Serialize, Clone, Default, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Diagnostic display level only: trace, debug, info, warn, or error. Default: info; archives are unaffected.
    pub log_level: Option<crate::diagnostics::record::Severity>,
    /// Default HTTP Basic Auth credentials as one object or a list. Every entry requires origin, username, and password.
    pub basic_auth: Option<BasicAuthConfig>,
    /// Maximum simultaneous browser captures. Positive whole number; default: 5.
    #[schemars(range(min = 1), example = 5)]
    pub async_capture_limit: Option<usize>,
    /// Maximum simultaneous image comparisons. Positive whole number; default: 2.
    #[schemars(range(min = 1), example = 2)]
    pub async_compare_limit: Option<usize>,
    /// Automatically clean old test runs after completed full-suite tests. Default: false; suites may override it.
    pub auto_clean_runs: Option<bool>,
    /// Maximum completed test runs to keep when automatic cleanup is enabled. Minimum and default: 1. The latest completed full-suite run is always retained.
    #[schemars(range(min = 1), example = 1)]
    pub keep_test_runs: Option<usize>,
    /// Browser engine. Only "chromium" is supported.
    pub browser: Option<Browser>,
    /// Bypass the site's Content Security Policy (default: false).
    pub bypass_csp: Option<bool>,
    /// Default cookie YAML or JSON file loaded before navigation.
    #[schemars(example = "common/_cookies.yaml")]
    pub cookie_path: Option<String>,
    /// Default CSS override file injected into every page document before actions and capture.
    #[schemars(example = "common/_override.css")]
    pub css_override_path: Option<String>,
    /// Show the browser while capturing (default: false). Always disabled in CI environments without a display.
    pub debug: Option<bool>,
    /// Default delay in milliseconds before each save checkpoint and before the final captures. Non-negative; default: 1000.
    #[schemars(range(min = 0.0), example = 1000)]
    pub delay: Option<f64>,
    /// Complete finite and pause infinite CSS/Web Animations before actions and before each capture. Default: true.
    pub freeze_animations: Option<bool>,
    /// Standard CSS selectors hidden in every document while preserving layout space; selectors matching nothing are ignored. Supply a list, even for one selector.
    #[schemars(example = vec![".timestamp"])]
    pub hide_selectors: Option<Vec<String>>,
    /// Ignore HTTPS certificate errors (default: false).
    pub ignore_ssl_errors: Option<bool>,
    /// Opt-in image simplification before each screenshot: none (default) keeps images unchanged; pixelate turns large images in the main document into uniform blocks of their average color. Enabling it or changing any image simplification setting changes pixels, so re-capture references; keep pixelMatchThreshold above 0. Suites and scenarios can override it.
    #[schemars(example = &"pixelate")]
    pub image_simplification: Option<ImageSimplification>,
    /// Image simplification block size in whole CSS pixels: an even number from 2 to 16. Default: 4. Larger blocks remove more detail.
    #[schemars(range(min = 2, max = 16), extend("multipleOf" = 2), example = 4)]
    pub image_simplification_block_size: Option<u32>,
    /// Image simplification minimum size in whole CSS pixels; images rendered narrower or shorter stay sharp. Default: 64; at least 1.
    #[schemars(range(min = 1), example = 64)]
    pub image_simplification_min_size: Option<u32>,
    /// Standard CSS selectors for images, or elements containing images, that image simplification keeps sharp; invalid selectors match nothing. Supply a list; entries must not be blank. Suite and scenario lists replace it.
    #[schemars(example = vec![".product-photo", "#hero img"])]
    #[schemars(extend("items" = serde_json::json!({"type": "string", "minLength": 1, "pattern": r"\S"})))]
    pub image_simplification_exclude_selectors: Option<Vec<String>>,
    /// Standard browser JavaScript injected once per active page document after readiness and before onReadyScript and actions. Returned promises are awaited; errors fail the capture. No Node.js or Playwright is required. Defaults to common/_on-ready.js.
    #[schemars(example = "common/_on-ready.js")]
    pub js_on_ready_path: Option<String>,
    /// Minimum whole milliseconds at each lazy-load scroll stop. Resource checks still run with 0. Default: 250; overridden by suite and scenario settings. Ignored when noScrollTop is true.
    #[schemars(example = 250)]
    pub lazy_load_scroll_delay: Option<u64>,
    /// Positive whole milliseconds for the complete lazy-load scroll and return to the top. Expiry fails the capture. Default: 20000; overridden by suite and scenario settings. Ignored when noScrollTop is true.
    #[schemars(range(min = 1), example = 20000)]
    pub lazy_load_timeout: Option<u64>,
    /// Allowed percentage of different pixels, from 0 to 100. Default: 0.1, meaning 0.1%.
    #[schemars(range(min = 0.0, max = 100.0), example = 0.1)]
    pub mis_match_threshold: Option<f64>,
    /// Per-pixel color tolerance from 0 to 100. Lower values highlight subtler color changes; this is not the allowed percentage of different pixels. Default: 5.
    #[schemars(range(min = 0.0, max = 100.0), example = 5.0)]
    pub pixel_match_threshold: Option<f64>,
    /// Skip the one-time lazy-load scroll after navigation, before actions (default: false).
    pub no_scroll_top: Option<bool>,
    /// Milliseconds to wait after actions, before the final captures only. Non-negative; default: 1000.
    #[schemars(range(min = 0.0), example = 1000)]
    pub post_interaction_wait: Option<f64>,
    /// Standard CSS selectors set to display: none in every document; selectors matching nothing are ignored. Supply a list, even for one selector.
    #[schemars(example = vec![".advertisement"])]
    pub remove_selectors: Option<Vec<String>>,
    /// Login requirement flag exposed to ready scripts. Does not log in automatically (default: false).
    pub required_login: Option<bool>,
    /// HTML page title for snapshot reports. Default: Visual Tests Summary | Regressify. Overridden by snapshot --title.
    #[schemars(example = "Shop | Regressify")]
    pub snapshot_title: Option<String>,
    /// Heading for snapshot reports. Default: Visual Tests Summary. Overridden by snapshot --heading.
    #[schemars(example = "Shop results")]
    pub snapshot_heading: Option<String>,
    /// Markdown subheading for snapshot reports. Default: Snapshot regression test results. Overridden by snapshot --subheading.
    #[schemars(example = "**Staging** regression results")]
    pub snapshot_subheading: Option<String>,
    /// Saved browser state name or list of names, without .json. Created by persist and loaded from the active output directory's states folder.
    #[schemars(example = "signed-in")]
    pub state: Option<Needs>,
    /// URL replacement rules applied in order. Each entry requires ref and test; regex and flags are optional.
    pub url_replacements: Option<Vec<UrlReplacement>>,
    /// Apply the CSS override file to each page document (default: true). hideSelectors and removeSelectors still apply when false.
    pub use_css_override: Option<bool>,
    /// Viewport label or list of labels. Matching ignores case; omit or use an empty list to capture all configured viewports.
    #[schemars(example = vec!["phone", "desktop"])]
    pub viewport_names: Option<Selectors>,
    /// Workspace-relative viewport YAML file. Default: common/_viewports.yaml. An explicit path must exist inside the workspace.
    #[schemars(example = "common/_viewports.yaml")]
    pub viewports_path: Option<String>,
    /// Positive whole milliseconds for element and frame existence, selector waits and readySelector visibility, URL waits, goto, select retries, switchTab, and injected browser hooks. Default: 30000; zero is invalid.
    #[schemars(range(min = 1), example = 30000)]
    pub element_timeout: Option<u64>,
    /// Positive whole milliseconds for visibility, stability, enabled, and unobscured-target checks. Default: 5000; zero is invalid.
    #[schemars(range(min = 1), example = 5000)]
    pub actionability_timeout: Option<u64>,
    /// Whole milliseconds to wait before each capture for fonts, images, and DOM/layout quiet.
    /// Best effort: expiry is logged and capture continues. 0 disables settling. Default: 5000.
    #[schemars(example = 5000)]
    pub settle_timeout: Option<u64>,
    /// Positive whole milliseconds for one complete scenario/viewport capture, including actions and hooks. Default: 600000 (ten minutes); zero is invalid.
    #[schemars(range(min = 1), example = 600000)]
    pub scenario_timeout: Option<u64>,
}

pub fn resolve_run_retention(workspace: &WorkspaceConfig, suite: &TestSuite) -> Option<usize> {
    suite
        .auto_clean_runs
        .or(workspace.auto_clean_runs)
        .unwrap_or(false)
        .then(|| {
            suite
                .keep_test_runs
                .or(workspace.keep_test_runs)
                .unwrap_or(1)
        })
}

/// Fully resolved configuration — all cascade levels merged with defaults applied.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedConfig {
    pub log_level: crate::diagnostics::record::Severity,
    #[serde(skip_serializing)]
    pub basic_auth: Vec<BasicAuth>,
    pub async_capture_limit: usize,
    pub async_compare_limit: usize,
    pub browser: Browser,
    pub bypass_csp: bool,
    pub cookie_path: Option<String>,
    pub css_override_path: Option<String>,
    pub debug: bool,
    pub delay: Option<f64>,
    pub freeze_animations: bool,
    pub hide_selectors: Vec<String>,
    pub ignore_ssl_errors: bool,
    pub image_simplification: ImageSimplification,
    pub image_simplification_block_size: u32,
    pub image_simplification_min_size: u32,
    pub image_simplification_exclude_selectors: Vec<String>,
    pub js_on_ready_path: Option<String>,
    pub lazy_load_scroll_delay_ms: u64,
    pub lazy_load_timeout_ms: u64,
    pub mis_match_threshold: f64,
    pub pixel_match_threshold: f64,
    pub no_scroll_top: bool,
    pub post_interaction_wait: Option<f64>,
    pub remove_selectors: Vec<String>,
    pub required_login: bool,
    pub state: Option<Needs>,
    pub use_css_override: bool,
    pub viewport_names: Option<Vec<String>>,
    pub viewports_path: String,
    pub viewports_path_is_explicit: bool,
    pub element_timeout_ms: u64,
    pub actionability_timeout_ms: u64,
    pub settle_timeout_ms: u64,
    pub scenario_timeout_ms: u64,
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        Self {
            log_level: crate::diagnostics::record::Severity::Info,
            basic_auth: Vec::new(),
            async_capture_limit: DEFAULT_ASYNC_CAPTURE_LIMIT,
            async_compare_limit: DEFAULT_ASYNC_COMPARE_LIMIT,
            browser: Browser::Chromium,
            bypass_csp: false,
            cookie_path: None,
            css_override_path: None,
            debug: false,
            delay: Some(DEFAULT_DELAY_MS),
            freeze_animations: true,
            hide_selectors: Vec::new(),
            ignore_ssl_errors: false,
            image_simplification: ImageSimplification::None,
            image_simplification_block_size: DEFAULT_IMAGE_SIMPLIFICATION_BLOCK_SIZE,
            image_simplification_min_size: DEFAULT_IMAGE_SIMPLIFICATION_MIN_SIZE,
            image_simplification_exclude_selectors: Vec::new(),
            js_on_ready_path: None,
            lazy_load_scroll_delay_ms: DEFAULT_LAZY_LOAD_SCROLL_DELAY_MS,
            lazy_load_timeout_ms: DEFAULT_LAZY_LOAD_TIMEOUT_MS,
            mis_match_threshold: 0.1,
            pixel_match_threshold: DEFAULT_PIXEL_MATCH_THRESHOLD,
            no_scroll_top: false,
            post_interaction_wait: Some(DEFAULT_POST_INTERACTION_WAIT_MS),
            remove_selectors: Vec::new(),
            required_login: false,
            state: None,
            use_css_override: true,
            viewport_names: None,
            viewports_path: "common/_viewports.yaml".to_string(),
            viewports_path_is_explicit: false,
            element_timeout_ms: 30_000,
            actionability_timeout_ms: 5_000,
            settle_timeout_ms: 5_000,
            scenario_timeout_ms: 600_000,
        }
    }
}

/// Load workspace config from `regressify.yaml` or `regressify.yml` in the given directory.
/// Returns `WorkspaceConfig::default()` if neither file exists.
pub fn load_workspace_config(root_dir: &Path) -> Result<WorkspaceConfig> {
    let yaml_path = root_dir.join("regressify.yaml");
    let yml_path = root_dir.join("regressify.yml");

    let config_path = if yaml_path.exists() {
        Some(yaml_path)
    } else if yml_path.exists() {
        Some(yml_path)
    } else {
        None
    };

    match config_path {
        Some(path) => {
            let content = std::fs::read_to_string(&path)?;
            let config: WorkspaceConfig = serde_yaml::from_str(&content)?;
            Ok(config)
        }
        None => Ok(WorkspaceConfig::default()),
    }
}

/// Resolve an explicitly configured file and ensure it remains inside the
/// workspace, including after symlink resolution.
pub fn resolve_workspace_file(
    root_dir: &Path,
    configured_path: &str,
    label: &str,
) -> Result<PathBuf> {
    let candidate = if Path::new(configured_path).is_absolute() {
        PathBuf::from(configured_path)
    } else {
        root_dir.join(configured_path)
    };
    if !candidate.exists() {
        anyhow::bail!("Configured {} '{}' does not exist", label, configured_path);
    }

    let canonical_root = root_dir
        .canonicalize()
        .with_context(|| format!("Failed to resolve workspace root '{}'", root_dir.display()))?;
    let canonical_path = candidate.canonicalize().with_context(|| {
        format!(
            "Failed to resolve configured {} '{}'",
            label, configured_path
        )
    })?;
    if !canonical_path.starts_with(&canonical_root) {
        anyhow::bail!(
            "Configured {} '{}' escapes workspace root '{}'",
            label,
            configured_path,
            canonical_root.display()
        );
    }
    if !canonical_path.is_file() {
        anyhow::bail!("Configured {} '{}' is not a file", label, configured_path);
    }
    Ok(canonical_path)
}

/// Resolve an optional default file. Missing defaults are allowed, but a file
/// that exists must satisfy the same containment and regular-file checks as an
/// explicitly configured path.
pub fn resolve_optional_workspace_file(
    root_dir: &Path,
    configured_path: &str,
    label: &str,
) -> Result<Option<PathBuf>> {
    let candidate = if Path::new(configured_path).is_absolute() {
        PathBuf::from(configured_path)
    } else {
        root_dir.join(configured_path)
    };
    if !candidate.exists() {
        return Ok(None);
    }
    resolve_workspace_file(root_dir, configured_path, label).map(Some)
}

fn selectors_to_vec(s: &Option<Selectors>) -> Option<Vec<String>> {
    match s {
        Some(Selectors::String(s)) => Some(vec![s.clone()]),
        Some(Selectors::StringArray(v)) => Some(v.clone()),
        None => None,
    }
}

fn needs_to_vec(needs: &Needs) -> Vec<String> {
    match needs {
        Needs::String(value) => vec![value.clone()],
        Needs::StringArray(values) => values.clone(),
    }
}

fn needs_from_vec(values: Vec<String>) -> Option<Needs> {
    match values.as_slice() {
        [] => None,
        [single] => Some(Needs::String(single.clone())),
        _ => Some(Needs::StringArray(values)),
    }
}

fn merge_state_and_restore(state: Option<Needs>, restore: Option<Needs>) -> Option<Needs> {
    let mut values = Vec::new();
    for needs in state.iter().chain(restore.iter()) {
        for value in needs_to_vec(needs) {
            if !values.contains(&value) {
                values.push(value);
            }
        }
    }
    needs_from_vec(values)
}

/// Resolve the suite-wide CPU worker count used for image comparisons.
pub fn resolve_async_compare_limit(workspace: &WorkspaceConfig, suite: &TestSuite) -> usize {
    suite
        .async_compare_limit
        .or(workspace.async_compare_limit)
        .unwrap_or(DEFAULT_ASYNC_COMPARE_LIMIT)
        .max(1)
}

/// Resolve the per-suite capture cap. The CLI-level global cap is applied by
/// the scheduler separately, so each suite cannot consume all capture slots.
pub fn resolve_async_capture_limit(workspace: &WorkspaceConfig, suite: &TestSuite) -> usize {
    suite
        .async_capture_limit
        .or(workspace.async_capture_limit)
        .unwrap_or(DEFAULT_ASYNC_CAPTURE_LIMIT)
        .max(1)
}

/// Validate one scope's explicit image simplification values before capture.
/// Every scope is checked even when a narrower scope overrides it or the mode
/// resolves to `none`, so a latent invalid value cannot surface later. The
/// generated schema carries the same limits for editors and desktop feedback.
pub fn validate_image_simplification(
    scope: &str,
    block_size: Option<u32>,
    min_size: Option<u32>,
    exclude_selectors: Option<&[String]>,
) -> Result<()> {
    if let Some(block_size) = block_size
        && (block_size % 2 != 0
            || !(MIN_IMAGE_SIMPLIFICATION_BLOCK_SIZE..=MAX_IMAGE_SIMPLIFICATION_BLOCK_SIZE)
                .contains(&block_size))
    {
        anyhow::bail!(
            "{} imageSimplificationBlockSize must be an even whole number of CSS pixels from {} to {}",
            scope,
            MIN_IMAGE_SIMPLIFICATION_BLOCK_SIZE,
            MAX_IMAGE_SIMPLIFICATION_BLOCK_SIZE
        );
    }
    if min_size == Some(0) {
        anyhow::bail!(
            "{} imageSimplificationMinSize must be at least 1 CSS pixel",
            scope
        );
    }
    // Blank entries are invalid CSS and would otherwise fail inside the page;
    // report the position only, without echoing configured selector text.
    if let Some(index) = exclude_selectors
        .unwrap_or_default()
        .iter()
        .position(|selector| selector.trim().is_empty())
    {
        anyhow::bail!(
            "{} imageSimplificationExcludeSelectors entry #{} must not be blank",
            scope,
            index + 1
        );
    }
    Ok(())
}

/// Resolve configuration by cascading: scenario -> suite -> workspace -> defaults.
/// For most fields: first non-None value wins (scenario overrides suite overrides workspace).
/// For selector lists (hide/remove/image simplification exclusions): the most
/// specific list replaces inherited lists (no merge).
pub fn resolve_config(
    workspace: &WorkspaceConfig,
    suite: &TestSuite,
    scenario: &TestScenario,
) -> ResolvedConfig {
    let defaults = ResolvedConfig::default();
    let configured_viewports_path = suite
        .viewports_path
        .clone()
        .or(workspace.viewports_path.clone());

    ResolvedConfig {
        basic_auth: [
            &workspace.basic_auth,
            &suite.basic_auth,
            &scenario.basic_auth,
        ]
        .into_iter()
        .flatten()
        .flat_map(|config| config.entries().iter().cloned())
        .collect(),

        async_capture_limit: resolve_async_capture_limit(workspace, suite),

        async_compare_limit: resolve_async_compare_limit(workspace, suite),

        browser: suite
            .browser
            .clone()
            .or(workspace.browser.clone())
            .unwrap_or(defaults.browser),

        bypass_csp: scenario
            .bypass_csp
            .or(suite.bypass_csp)
            .or(workspace.bypass_csp)
            .unwrap_or(defaults.bypass_csp),

        cookie_path: scenario
            .cookie_path
            .clone()
            .or(suite.cookie_path.clone())
            .or(workspace.cookie_path.clone()),

        css_override_path: scenario
            .css_override_path
            .clone()
            .or(suite.css_override_path.clone())
            .or(workspace.css_override_path.clone()),

        debug: suite.debug.or(workspace.debug).unwrap_or(defaults.debug),
        log_level: scenario
            .log_level
            .or(suite.log_level)
            .or(workspace.log_level)
            .unwrap_or(defaults.log_level),

        delay: scenario.delay.or(workspace.delay).or(defaults.delay),

        freeze_animations: workspace
            .freeze_animations
            .unwrap_or(defaults.freeze_animations),

        hide_selectors: scenario
            .hide_selectors
            .clone()
            .or(suite.hide_selectors.clone())
            .or(workspace.hide_selectors.clone())
            .unwrap_or(defaults.hide_selectors),

        ignore_ssl_errors: suite
            .ignore_ssl_errors
            .or(workspace.ignore_ssl_errors)
            .unwrap_or(defaults.ignore_ssl_errors),

        // Each image simplification key resolves independently, so a scenario
        // can switch the mode while inheriting the block size, or vice versa.
        image_simplification: scenario
            .image_simplification
            .or(suite.image_simplification)
            .or(workspace.image_simplification)
            .unwrap_or(defaults.image_simplification),

        image_simplification_block_size: scenario
            .image_simplification_block_size
            .or(suite.image_simplification_block_size)
            .or(workspace.image_simplification_block_size)
            .unwrap_or(defaults.image_simplification_block_size),

        image_simplification_min_size: scenario
            .image_simplification_min_size
            .or(suite.image_simplification_min_size)
            .or(workspace.image_simplification_min_size)
            .unwrap_or(defaults.image_simplification_min_size),

        image_simplification_exclude_selectors: scenario
            .image_simplification_exclude_selectors
            .clone()
            .or(suite.image_simplification_exclude_selectors.clone())
            .or(workspace.image_simplification_exclude_selectors.clone())
            .unwrap_or(defaults.image_simplification_exclude_selectors),

        js_on_ready_path: scenario
            .js_on_ready_path
            .clone()
            .or(suite.js_on_ready_path.clone())
            .or(workspace.js_on_ready_path.clone()),

        lazy_load_scroll_delay_ms: scenario
            .lazy_load_scroll_delay
            .or(suite.lazy_load_scroll_delay)
            .or(workspace.lazy_load_scroll_delay)
            .unwrap_or(defaults.lazy_load_scroll_delay_ms),

        lazy_load_timeout_ms: scenario
            .lazy_load_timeout
            .or(suite.lazy_load_timeout)
            .or(workspace.lazy_load_timeout)
            .unwrap_or(defaults.lazy_load_timeout_ms),

        mis_match_threshold: scenario
            .mis_match_threshold
            .or(suite.mis_match_threshold)
            .or(workspace.mis_match_threshold)
            .unwrap_or(defaults.mis_match_threshold),

        pixel_match_threshold: scenario
            .pixel_match_threshold
            .or(suite.pixel_match_threshold)
            .or(workspace.pixel_match_threshold)
            .unwrap_or(defaults.pixel_match_threshold),

        no_scroll_top: scenario
            .no_scroll_top
            .or(workspace.no_scroll_top)
            .unwrap_or(defaults.no_scroll_top),

        post_interaction_wait: scenario
            .post_interaction_wait
            .or(suite.post_interaction_wait)
            .or(workspace.post_interaction_wait)
            .or(defaults.post_interaction_wait),

        remove_selectors: scenario
            .remove_selectors
            .clone()
            .or(suite.remove_selectors.clone())
            .or(workspace.remove_selectors.clone())
            .unwrap_or(defaults.remove_selectors),

        required_login: common::REQUIRED_LOGIN.get().copied().unwrap_or(false)
            || scenario
                .required_login
                .or(workspace.required_login)
                .unwrap_or(defaults.required_login),

        state: merge_state_and_restore(
            suite.state.clone().or(workspace.state.clone()),
            scenario.restore.clone(),
        ),

        use_css_override: scenario
            .use_css_override
            .or(suite.use_css_override)
            .or(workspace.use_css_override)
            .unwrap_or(defaults.use_css_override),

        viewport_names: selectors_to_vec(&scenario.viewport_names)
            .or(selectors_to_vec(&suite.viewport_names))
            .or(selectors_to_vec(&workspace.viewport_names)),

        viewports_path: configured_viewports_path
            .clone()
            .unwrap_or(defaults.viewports_path),

        viewports_path_is_explicit: configured_viewports_path.is_some(),

        element_timeout_ms: workspace
            .element_timeout
            .unwrap_or(defaults.element_timeout_ms),

        actionability_timeout_ms: workspace
            .actionability_timeout
            .unwrap_or(defaults.actionability_timeout_ms),

        settle_timeout_ms: workspace
            .settle_timeout
            .unwrap_or(defaults.settle_timeout_ms),

        scenario_timeout_ms: workspace
            .scenario_timeout
            .unwrap_or(defaults.scenario_timeout_ms),
    }
}

pub fn resolve_log_level(
    flag: Option<crate::diagnostics::record::Severity>,
    environment: Option<&str>,
    scenario: Option<crate::diagnostics::record::Severity>,
    suite: Option<crate::diagnostics::record::Severity>,
    workspace: Option<crate::diagnostics::record::Severity>,
) -> Result<crate::diagnostics::record::Severity> {
    if let Some(flag) = flag {
        return Ok(flag);
    }
    if let Some(environment) = environment {
        return environment.parse().map_err(anyhow::Error::msg);
    }
    Ok(scenario
        .or(suite)
        .or(workspace)
        .unwrap_or(crate::diagnostics::record::Severity::Info))
}

#[cfg(test)]
mod tests {
    #[test]
    fn log_level_precedence_matrix() {
        use crate::diagnostics::{journal::*, record::*};
        let root = tempfile::tempdir().unwrap();
        let cases = [
            (
                Some(Severity::Trace),
                Some("error"),
                Some(Severity::Warn),
                Some(Severity::Info),
                Some(Severity::Debug),
                Severity::Trace,
            ),
            (
                None,
                Some("error"),
                Some(Severity::Warn),
                Some(Severity::Info),
                Some(Severity::Debug),
                Severity::Error,
            ),
            (
                None,
                None,
                Some(Severity::Warn),
                Some(Severity::Info),
                Some(Severity::Debug),
                Severity::Warn,
            ),
            (
                None,
                None,
                None,
                Some(Severity::Error),
                Some(Severity::Debug),
                Severity::Error,
            ),
            (
                None,
                None,
                None,
                None,
                Some(Severity::Debug),
                Severity::Debug,
            ),
            (None, None, None, None, None, Severity::Info),
        ];
        for (flag, environment, scenario, suite, workspace, expected) in cases {
            assert_eq!(
                super::resolve_log_level(flag, environment, scenario, suite, workspace).unwrap(),
                expected
            );
            let metadata = OperationMetadata::new(OperationKind::Capture, None);
            let mut writer =
                JournalWriter::create(root.path(), metadata.clone(), JournalOptions::default())
                    .unwrap();
            let mut record = DiagnosticRecordV1::new(
                metadata.operation_id.clone(),
                OperationKind::Capture,
                Stage::ActionsFrames,
                DiagnosticEvent::Decision,
            );
            record.severity = Severity::Debug;
            for _ in 0..20 {
                writer.write(&record).unwrap();
            }
            writer.finalize(Outcome::Success).unwrap();
            assert_eq!(
                JournalReader::open(root.path(), &metadata.operation_id)
                    .unwrap()
                    .visit(|_| {})
                    .unwrap()
                    .records,
                21
            );
        }
        assert!(super::resolve_log_level(None, Some("invalid"), None, None, None).is_err());
        assert_eq!(
            super::resolve_log_level(Some(Severity::Info), Some("invalid"), None, None, None)
                .unwrap(),
            Severity::Info
        );
    }

    #[test]
    fn archive_writer_settings_are_not_scenario_cascaded() {
        use crate::diagnostics::{journal::JournalOptions, record::Severity};
        for level in [Severity::Trace, Severity::Error] {
            let workspace = super::WorkspaceConfig {
                log_level: Some(level),
                ..Default::default()
            };
            let mut suite = crate::types::TestSuite::default();
            let mut scenario = crate::types::TestScenario::default();
            for scope in 0..3 {
                if scope == 1 {
                    suite.log_level = Some(level);
                }
                if scope == 2 {
                    scenario.log_level = Some(level);
                }
                let options = JournalOptions::resolve(Some(4096), Some(8192), |_| None).unwrap();
                assert_eq!(
                    super::resolve_config(&workspace, &suite, &scenario).log_level,
                    level
                );
                assert_eq!(
                    (options.chunk_bytes, options.disk_budget_bytes),
                    (4096, 8192)
                );
                assert_eq!(
                    JournalOptions::resolve(None, None, |_| None).unwrap(),
                    JournalOptions::default()
                );
            }
        }
        for json in [
            r#"{"diagnosticChunkSizeBytes":4096}"#,
            r#"{"diagnosticDiskBudgetBytes":8192}"#,
        ] {
            assert!(serde_json::from_str::<super::WorkspaceConfig>(json).is_err());
            assert!(serde_json::from_str::<crate::types::TestScenario>(json).is_err());
        }
    }

    use super::*;

    fn suite() -> TestSuite {
        TestSuite {
            id: Some("alloy".to_string()),
            test_date: None,
            async_capture_limit: None,
            async_compare_limit: None,
            browser: None,
            bypass_csp: None,
            cookie_path: None,
            css_override_path: None,
            debug: None,
            hide_selectors: None,
            ignore_ssl_errors: None,
            js_on_ready_path: None,
            mis_match_threshold: None,
            post_interaction_wait: None,
            remove_selectors: None,
            scenarios: Vec::new(),
            state: None,
            url_replacements: None,
            use_css_override: None,
            viewport_names: None,
            viewports_path: None,
            ..Default::default()
        }
    }

    fn scenario() -> TestScenario {
        TestScenario {
            actions: None,
            bypass_csp: None,
            click_selector: None,
            cookie_path: None,
            css_override_path: None,
            delay: None,
            description: None,
            hide_selectors: None,
            id: Some("home".to_string()),
            js_on_ready_path: None,
            label: None,
            mis_match_threshold: None,
            needs: None,
            no_scroll_top: None,
            post_interaction_wait: None,
            remove_selectors: None,
            required_login: None,
            restore: None,
            selectors: None,
            url: "https://example.com".to_string(),
            use_css_override: None,
            viewport_names: None,
            ..Default::default()
        }
    }

    #[test]
    fn basic_auth_cascades_by_origin_and_is_not_serialized() {
        let workspace: WorkspaceConfig = serde_yaml::from_str(
            "basicAuth:\n  origin: https://example.com\n  username: workspace\n  password: private\n",
        ).unwrap();
        let suite: TestSuite = serde_yaml::from_str(
            "scenarios: []\nbasicAuth:\n  - origin: https://other.example\n    username: other\n    password: private\n",
        ).unwrap();
        let scenario: TestScenario = serde_yaml::from_str(
            "url: https://example.com\nbasicAuth:\n  origin: https://example.com\n  username: scenario\n  password: private\n",
        ).unwrap();
        let resolved = resolve_config(&workspace, &suite, &scenario);
        let entries = basic_auth::resolve(&resolved.basic_auth).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].username, "scenario");
        assert_eq!(entries[1].username, "other");
        assert!(
            !serde_json::to_string(&resolved)
                .unwrap()
                .contains("private")
        );
    }

    #[test]
    fn pixel_match_threshold_cascades_and_preserves_explicit_zero() {
        for (workspace_value, suite_value, scenario_value, expected) in [
            (None, None, None, 5.0),
            (Some(20.0), None, None, 20.0),
            (Some(20.0), Some(10.0), None, 10.0),
            (Some(20.0), Some(10.0), Some(3.1), 3.1),
            (Some(20.0), Some(10.0), Some(0.0), 0.0),
            (Some(20.0), Some(0.0), None, 0.0),
            (Some(0.0), None, None, 0.0),
        ] {
            let workspace = WorkspaceConfig {
                pixel_match_threshold: workspace_value,
                ..Default::default()
            };
            let suite = TestSuite {
                pixel_match_threshold: suite_value,
                ..suite()
            };
            let scenario = TestScenario {
                pixel_match_threshold: scenario_value,
                ..scenario()
            };
            let resolved = resolve_config(&workspace, &suite, &scenario);

            assert_eq!(resolved.pixel_match_threshold, expected);
            assert_eq!(resolved.mis_match_threshold, 0.1);
            assert_eq!(
                serde_json::to_value(&resolved).unwrap()["pixelMatchThreshold"],
                expected
            );
        }
    }

    #[test]
    fn pixel_match_threshold_round_trips_at_all_configuration_levels() {
        let workspace: WorkspaceConfig = serde_yaml::from_str("pixelMatchThreshold: 10\n").unwrap();
        let suite: TestSuite = serde_yaml::from_str(
            "pixelMatchThreshold: 3.1\nscenarios:\n  - url: https://example.com\n    pixelMatchThreshold: 0\n",
        )
        .unwrap();

        assert_eq!(workspace.pixel_match_threshold, Some(10.0));
        assert_eq!(suite.pixel_match_threshold, Some(3.1));
        assert_eq!(suite.scenarios[0].pixel_match_threshold, Some(0.0));
        assert_eq!(
            serde_json::to_value(workspace).unwrap()["pixelMatchThreshold"],
            10.0
        );
        let serialized = serde_json::to_value(suite).unwrap();
        assert_eq!(serialized["pixelMatchThreshold"], 3.1);
        assert_eq!(serialized["scenarios"][0]["pixelMatchThreshold"], 0.0);
        let round_trip: TestSuite = serde_json::from_value(serialized).unwrap();
        assert_eq!(round_trip.pixel_match_threshold, Some(3.1));
        assert_eq!(round_trip.scenarios[0].pixel_match_threshold, Some(0.0));
    }

    #[test]
    fn lazy_load_settings_cascade_and_preserve_zero_scroll_delay() {
        for (workspace_values, suite_values, scenario_values, expected) in [
            (None, None, None, (250, 20_000)),
            (Some((300, 30_000)), None, None, (300, 30_000)),
            (
                Some((300, 30_000)),
                Some((500, 40_000)),
                None,
                (500, 40_000),
            ),
            (
                Some((300, 30_000)),
                Some((500, 40_000)),
                Some((700, 50_000)),
                (700, 50_000),
            ),
            (
                Some((300, 30_000)),
                Some((500, 40_000)),
                Some((0, 1_000)),
                (0, 1_000),
            ),
            (Some((300, 30_000)), Some((0, 40_000)), None, (0, 40_000)),
            (Some((0, 30_000)), None, None, (0, 30_000)),
        ] {
            let workspace = WorkspaceConfig {
                lazy_load_scroll_delay: workspace_values.map(|values| values.0),
                lazy_load_timeout: workspace_values.map(|values| values.1),
                ..Default::default()
            };
            let suite = TestSuite {
                lazy_load_scroll_delay: suite_values.map(|values| values.0),
                lazy_load_timeout: suite_values.map(|values| values.1),
                ..suite()
            };
            let scenario = TestScenario {
                lazy_load_scroll_delay: scenario_values.map(|values| values.0),
                lazy_load_timeout: scenario_values.map(|values| values.1),
                ..scenario()
            };
            let resolved = resolve_config(&workspace, &suite, &scenario);
            assert_eq!(
                (
                    resolved.lazy_load_scroll_delay_ms,
                    resolved.lazy_load_timeout_ms
                ),
                expected
            );
        }

        let workspace = WorkspaceConfig {
            lazy_load_scroll_delay: Some(300),
            lazy_load_timeout: Some(30_000),
            ..Default::default()
        };
        let suite = TestSuite {
            lazy_load_timeout: Some(40_000),
            ..suite()
        };
        let scenario = TestScenario {
            lazy_load_scroll_delay: Some(0),
            ..scenario()
        };
        let resolved = resolve_config(&workspace, &suite, &scenario);
        assert_eq!(resolved.lazy_load_scroll_delay_ms, 0);
        assert_eq!(resolved.lazy_load_timeout_ms, 40_000);
    }

    #[test]
    fn lazy_load_settings_round_trip_and_reject_non_integer_values() {
        let workspace: WorkspaceConfig =
            serde_yaml::from_str("lazyLoadScrollDelay: 300\nlazyLoadTimeout: 40000\n").unwrap();
        let suite: TestSuite = serde_yaml::from_str(
            "lazyLoadScrollDelay: 500\nlazyLoadTimeout: 35000\nscenarios:\n  - url: https://example.com\n    lazyLoadScrollDelay: 0\n    lazyLoadTimeout: 30000\n",
        ).unwrap();
        let workspace_json = serde_json::to_value(workspace).unwrap();
        assert_eq!(workspace_json["lazyLoadScrollDelay"], 300);
        assert_eq!(workspace_json["lazyLoadTimeout"], 40_000);
        let round_trip: WorkspaceConfig = serde_json::from_value(workspace_json).unwrap();
        assert_eq!(round_trip.lazy_load_timeout, Some(40_000));
        let suite_json = serde_json::to_value(suite).unwrap();
        assert_eq!(suite_json["lazyLoadScrollDelay"], 500);
        assert_eq!(suite_json["lazyLoadTimeout"], 35_000);
        assert_eq!(suite_json["scenarios"][0]["lazyLoadScrollDelay"], 0);
        assert_eq!(suite_json["scenarios"][0]["lazyLoadTimeout"], 30_000);
        let round_trip: TestSuite = serde_json::from_value(suite_json).unwrap();
        assert_eq!(round_trip.scenarios[0].lazy_load_scroll_delay, Some(0));
        for field in ["lazyLoadScrollDelay", "lazyLoadTimeout"] {
            for invalid in [
                serde_json::json!(-1),
                serde_json::json!(0.5),
                serde_json::json!("300"),
            ] {
                assert!(
                    serde_json::from_value::<WorkspaceConfig>(
                        serde_json::json!({(field): invalid})
                    )
                    .is_err()
                );
                assert!(
                    serde_json::from_value::<TestSuite>(
                        serde_json::json!({(field): invalid, "scenarios": []})
                    )
                    .is_err()
                );
                assert!(
                    serde_json::from_value::<TestScenario>(
                        serde_json::json!({(field): invalid, "url": "https://example.com"})
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn image_simplification_defaults_off_and_each_key_cascades_independently() {
        let default = resolve_config(&WorkspaceConfig::default(), &suite(), &scenario());
        assert_eq!(default.image_simplification, ImageSimplification::None);
        assert_eq!(default.image_simplification_block_size, 4);
        assert_eq!(default.image_simplification_min_size, 64);
        assert!(default.image_simplification_exclude_selectors.is_empty());
        let serialized = serde_json::to_value(&default).unwrap();
        assert_eq!(serialized["imageSimplification"], "none");
        assert_eq!(serialized["imageSimplificationBlockSize"], 4);
        assert_eq!(serialized["imageSimplificationMinSize"], 64);
        assert_eq!(
            serialized["imageSimplificationExcludeSelectors"],
            serde_json::json!([])
        );

        use ImageSimplification::{None as Off, Pixelate};
        type Level = (
            Option<ImageSimplification>,
            Option<u32>,
            Option<u32>,
            Option<Vec<&'static str>>,
        );
        let unset: Level = (None, None, None, None);
        for (workspace_values, suite_values, scenario_values, expected) in [
            (
                unset.clone(),
                unset.clone(),
                unset.clone(),
                (Off, 4, 64, vec![]),
            ),
            (
                (Some(Pixelate), Some(8), Some(100), Some(vec![".logo"])),
                unset.clone(),
                unset.clone(),
                (Pixelate, 8, 100, vec![".logo"]),
            ),
            (
                (Some(Pixelate), Some(8), Some(100), Some(vec![".logo"])),
                (None, Some(6), None, Some(vec![".hero", ".team"])),
                unset.clone(),
                (Pixelate, 6, 100, vec![".hero", ".team"]),
            ),
            // An explicit scenario `none` and an explicit empty list override
            // inherited values; lists replace rather than merge.
            (
                (Some(Pixelate), Some(8), Some(100), Some(vec![".logo"])),
                (None, Some(6), Some(32), Some(vec![".hero"])),
                (Some(Off), None, Some(1), Some(vec![])),
                (Off, 6, 1, vec![]),
            ),
            (
                unset.clone(),
                (Some(Pixelate), None, None, None),
                (None, Some(16), None, Some(vec!["#map img"])),
                (Pixelate, 16, 64, vec!["#map img"]),
            ),
            (
                (Some(Off), Some(2), None, None),
                unset.clone(),
                (Some(Pixelate), None, None, None),
                (Pixelate, 2, 64, vec![]),
            ),
        ] {
            let workspace = WorkspaceConfig {
                image_simplification: workspace_values.0,
                image_simplification_block_size: workspace_values.1,
                image_simplification_min_size: workspace_values.2,
                image_simplification_exclude_selectors: workspace_values
                    .3
                    .map(|values| values.into_iter().map(String::from).collect()),
                ..Default::default()
            };
            let suite = TestSuite {
                image_simplification: suite_values.0,
                image_simplification_block_size: suite_values.1,
                image_simplification_min_size: suite_values.2,
                image_simplification_exclude_selectors: suite_values
                    .3
                    .map(|values| values.into_iter().map(String::from).collect()),
                ..suite()
            };
            let scenario = TestScenario {
                image_simplification: scenario_values.0,
                image_simplification_block_size: scenario_values.1,
                image_simplification_min_size: scenario_values.2,
                image_simplification_exclude_selectors: scenario_values
                    .3
                    .map(|values| values.into_iter().map(String::from).collect()),
                ..scenario()
            };
            let resolved = resolve_config(&workspace, &suite, &scenario);
            assert_eq!(
                (
                    resolved.image_simplification,
                    resolved.image_simplification_block_size,
                    resolved.image_simplification_min_size,
                    resolved.image_simplification_exclude_selectors,
                ),
                (
                    expected.0,
                    expected.1,
                    expected.2,
                    expected.3.into_iter().map(String::from).collect()
                )
            );
            // Unrelated settings keep their defaults.
            assert!(resolved.hide_selectors.is_empty());
            assert_eq!(resolved.pixel_match_threshold, 5.0);
        }
    }

    #[test]
    fn image_simplification_settings_round_trip_and_reject_wrong_types() {
        let workspace: WorkspaceConfig = serde_yaml::from_str(
            "imageSimplification: pixelate\nimageSimplificationBlockSize: 8\nimageSimplificationMinSize: 96\nimageSimplificationExcludeSelectors:\n  - .logo\n",
        )
        .unwrap();
        assert_eq!(
            workspace.image_simplification,
            Some(ImageSimplification::Pixelate)
        );
        assert_eq!(workspace.image_simplification_block_size, Some(8));
        assert_eq!(workspace.image_simplification_min_size, Some(96));
        let suite: TestSuite = serde_yaml::from_str(
            "imageSimplification: none\nimageSimplificationExcludeSelectors: []\nscenarios:\n  - url: https://example.com\n    imageSimplification: pixelate\n    imageSimplificationBlockSize: 2\n    imageSimplificationMinSize: 1\n    imageSimplificationExcludeSelectors: ['#hero img']\n  - url: https://example.com/other\n    imageSimplification: ~\n",
        )
        .unwrap();
        assert_eq!(suite.image_simplification, Some(ImageSimplification::None));
        assert_eq!(
            suite.image_simplification_exclude_selectors,
            Some(Vec::new())
        );
        assert_eq!(suite.scenarios[1].image_simplification, None);
        let suite_json = serde_json::to_value(&suite).unwrap();
        assert_eq!(suite_json["imageSimplification"], "none");
        assert_eq!(
            suite_json["scenarios"][0]["imageSimplification"],
            "pixelate"
        );
        assert_eq!(
            suite_json["scenarios"][0]["imageSimplificationBlockSize"],
            2
        );
        assert_eq!(suite_json["scenarios"][0]["imageSimplificationMinSize"], 1);
        assert_eq!(
            suite_json["scenarios"][0]["imageSimplificationExcludeSelectors"],
            serde_json::json!(["#hero img"])
        );
        let round_trip: TestSuite = serde_json::from_value(suite_json).unwrap();
        assert_eq!(
            round_trip.scenarios[0].image_simplification,
            Some(ImageSimplification::Pixelate)
        );
        let workspace_json = serde_json::to_value(workspace).unwrap();
        let round_trip: WorkspaceConfig = serde_json::from_value(workspace_json).unwrap();
        assert_eq!(round_trip.image_simplification_min_size, Some(96));

        // YAML 1.1 tools read an unquoted `off` as false; only none/pixelate parse.
        for invalid in ["off", "false", "Pixelate", "NONE", "blur", "''"] {
            let yaml = format!("imageSimplification: {invalid}\n");
            assert!(
                serde_yaml::from_str::<WorkspaceConfig>(&yaml).is_err(),
                "{invalid}"
            );
            assert!(
                serde_yaml::from_str::<TestSuite>(&format!("{yaml}scenarios: []\n")).is_err(),
                "{invalid}"
            );
            assert!(
                serde_yaml::from_str::<TestScenario>(&format!("{yaml}url: https://example.com\n"))
                    .is_err(),
                "{invalid}"
            );
        }
        for (field, invalid) in [
            ("imageSimplificationBlockSize", serde_json::json!(-2)),
            ("imageSimplificationBlockSize", serde_json::json!(4.5)),
            ("imageSimplificationBlockSize", serde_json::json!("4")),
            (
                "imageSimplificationBlockSize",
                serde_json::json!(4_294_967_296_u64),
            ),
            ("imageSimplificationMinSize", serde_json::json!(-1)),
            ("imageSimplificationMinSize", serde_json::json!(64.5)),
            ("imageSimplificationMinSize", serde_json::json!(true)),
            (
                "imageSimplificationExcludeSelectors",
                serde_json::json!(".logo"),
            ),
            (
                "imageSimplificationExcludeSelectors",
                serde_json::json!([1]),
            ),
            ("imageSimplificationBlocksize", serde_json::json!(4)),
        ] {
            assert!(
                serde_json::from_value::<WorkspaceConfig>(serde_json::json!({(field): invalid}))
                    .is_err(),
                "{field}: {invalid}"
            );
            assert!(
                serde_json::from_value::<TestSuite>(
                    serde_json::json!({(field): invalid, "scenarios": []})
                )
                .is_err(),
                "{field}: {invalid}"
            );
            assert!(
                serde_json::from_value::<TestScenario>(
                    serde_json::json!({(field): invalid, "url": "https://example.com"})
                )
                .is_err(),
                "{field}: {invalid}"
            );
        }
    }

    #[test]
    fn image_simplification_validation_rejects_invalid_values_without_echoing_selectors() {
        for block_size in [2, 4, 6, 8, 10, 12, 14, 16] {
            validate_image_simplification("workspace", Some(block_size), None, None).unwrap();
        }
        for block_size in [0, 1, 3, 5, 15, 17, 18, 32, u32::MAX] {
            let error = validate_image_simplification("workspace", Some(block_size), None, None)
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                "workspace imageSimplificationBlockSize must be an even whole number of CSS pixels from 2 to 16"
            );
        }
        for min_size in [1, 64, u32::MAX] {
            validate_image_simplification("workspace", None, Some(min_size), None).unwrap();
        }
        assert_eq!(
            validate_image_simplification("Test suite 'shop'", None, Some(0), None)
                .unwrap_err()
                .to_string(),
            "Test suite 'shop' imageSimplificationMinSize must be at least 1 CSS pixel"
        );
        let selectors = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };
        for valid in [
            selectors(&[]),
            selectors(&[".logo"]),
            selectors(&[" .logo ", "#hero img"]),
        ] {
            validate_image_simplification("workspace", None, None, Some(&valid)).unwrap();
        }
        for (invalid, entry) in [
            (selectors(&[""]), 1),
            (selectors(&[" "]), 1),
            (selectors(&["\t\n"]), 1),
            (selectors(&[".private-banner", ""]), 2),
        ] {
            let error =
                validate_image_simplification("scenario 'home'", None, None, Some(&invalid))
                    .unwrap_err()
                    .to_string();
            assert_eq!(
                error,
                format!(
                    "scenario 'home' imageSimplificationExcludeSelectors entry #{entry} must not be blank"
                )
            );
            assert!(!error.contains("private-banner"), "{error}");
        }
        // Absent values and the documented defaults are valid at every scope.
        validate_image_simplification("workspace", None, None, None).unwrap();
        validate_image_simplification(
            "workspace",
            Some(DEFAULT_IMAGE_SIMPLIFICATION_BLOCK_SIZE),
            Some(DEFAULT_IMAGE_SIMPLIFICATION_MIN_SIZE),
            Some(&[]),
        )
        .unwrap();
    }

    #[test]
    fn image_simplification_schema_limits_match_preflight_validation() {
        let schemas = [
            serde_json::to_value(schemars::schema_for!(WorkspaceConfig)).unwrap(),
            serde_json::to_value(schemars::schema_for!(TestSuite)).unwrap(),
            serde_json::to_value(schemars::schema_for!(TestScenario)).unwrap(),
        ];
        for root in &schemas {
            let validator = jsonschema::validator_for(root).unwrap();
            let document = |key: &str, value: serde_json::Value| {
                let mut document = serde_json::json!({(key): value});
                if root["required"]
                    .as_array()
                    .is_some_and(|required| required.contains(&serde_json::json!("url")))
                {
                    document["url"] = serde_json::json!("https://example.com");
                } else if root["required"]
                    .as_array()
                    .is_some_and(|required| required.contains(&serde_json::json!("scenarios")))
                {
                    document["scenarios"] = serde_json::json!([]);
                }
                document
            };
            for block_size in 0..=20_u32 {
                assert_eq!(
                    validator
                        .is_valid(&document("imageSimplificationBlockSize", block_size.into())),
                    validate_image_simplification("scope", Some(block_size), None, None).is_ok(),
                    "block size {block_size}"
                );
            }
            for min_size in [0_u32, 1, 2, 64, u32::MAX] {
                assert_eq!(
                    validator.is_valid(&document("imageSimplificationMinSize", min_size.into())),
                    validate_image_simplification("scope", None, Some(min_size), None).is_ok(),
                    "min size {min_size}"
                );
            }
            for selectors in [
                vec![],
                vec![".logo".to_string()],
                vec![String::new()],
                vec![" ".to_string()],
                vec![".logo".to_string(), "\t".to_string()],
            ] {
                assert_eq!(
                    validator.is_valid(&document(
                        "imageSimplificationExcludeSelectors",
                        serde_json::json!(selectors)
                    )),
                    validate_image_simplification("scope", None, None, Some(&selectors)).is_ok(),
                    "{selectors:?}"
                );
            }
            for (mode, valid) in [
                (serde_json::json!("none"), true),
                (serde_json::json!("pixelate"), true),
                (serde_json::json!(null), true),
                (serde_json::json!("off"), false),
                (serde_json::json!(false), false),
                (serde_json::json!("Pixelate"), false),
            ] {
                assert_eq!(
                    validator.is_valid(&document("imageSimplification", mode.clone())),
                    valid,
                    "{mode}"
                );
            }
        }
    }

    #[test]
    fn delay_defaults_to_1000_ms_and_preserves_explicit_zero() {
        let workspace = WorkspaceConfig::default();
        let suite = suite();
        let mut scenario = scenario();
        assert_eq!(
            resolve_config(&workspace, &suite, &scenario).delay,
            Some(1000.0)
        );
        let workspace = WorkspaceConfig {
            delay: Some(250.0),
            ..workspace
        };
        assert_eq!(
            resolve_config(&workspace, &suite, &scenario).delay,
            Some(250.0)
        );
        scenario.delay = Some(0.0);
        assert_eq!(
            resolve_config(&workspace, &suite, &scenario).delay,
            Some(0.0)
        );
    }

    #[test]
    fn post_interaction_wait_defaults_to_1000_ms() {
        let resolved = resolve_config(&WorkspaceConfig::default(), &suite(), &scenario());

        assert_eq!(resolved.post_interaction_wait, Some(1000.0));
    }

    #[test]
    fn post_interaction_wait_uses_scenario_suite_workspace_order() {
        let workspace = WorkspaceConfig {
            post_interaction_wait: Some(3000.0),
            ..Default::default()
        };
        let mut suite = suite();
        suite.post_interaction_wait = Some(2000.0);
        let mut scenario = scenario();
        scenario.post_interaction_wait = Some(500.0);

        assert_eq!(
            resolve_config(&workspace, &suite, &scenario).post_interaction_wait,
            Some(500.0)
        );

        scenario.post_interaction_wait = None;
        assert_eq!(
            resolve_config(&workspace, &suite, &scenario).post_interaction_wait,
            Some(2000.0)
        );

        suite.post_interaction_wait = None;
        assert_eq!(
            resolve_config(&workspace, &suite, &scenario).post_interaction_wait,
            Some(3000.0)
        );
    }

    #[test]
    fn restore_extends_state_without_duplicates() {
        let mut suite = suite();
        suite.state = Some(Needs::StringArray(vec![
            "auth".to_string(),
            "prefs".to_string(),
        ]));
        let mut scenario = scenario();
        scenario.restore = Some(Needs::StringArray(vec![
            "auth".to_string(),
            "cart".to_string(),
        ]));

        let state = resolve_config(&WorkspaceConfig::default(), &suite, &scenario).state;
        match state {
            Some(Needs::StringArray(values)) => {
                assert_eq!(values, vec!["auth", "prefs", "cart"]);
            }
            _ => panic!("expected merged state array"),
        }
    }

    #[test]
    fn scenario_timeout_uses_workspace_value_or_default() {
        let default = resolve_config(&WorkspaceConfig::default(), &suite(), &scenario());
        assert_eq!(default.scenario_timeout_ms, 600_000);

        let workspace = WorkspaceConfig {
            scenario_timeout: Some(42_000),
            ..Default::default()
        };
        let resolved = resolve_config(&workspace, &suite(), &scenario());
        assert_eq!(resolved.scenario_timeout_ms, 42_000);
    }

    #[test]
    fn freeze_animations_defaults_on_and_can_be_disabled() {
        let default = resolve_config(&WorkspaceConfig::default(), &suite(), &scenario());
        assert!(default.freeze_animations);

        let workspace = WorkspaceConfig {
            freeze_animations: Some(false),
            ..Default::default()
        };
        let resolved = resolve_config(&workspace, &suite(), &scenario());
        assert!(!resolved.freeze_animations);
    }

    #[test]
    fn async_compare_limit_uses_suite_workspace_and_default_order() {
        let workspace = WorkspaceConfig {
            async_compare_limit: Some(3),
            ..Default::default()
        };
        let mut suite = suite();

        assert_eq!(resolve_async_compare_limit(&workspace, &suite), 3);
        suite.async_compare_limit = Some(4);
        assert_eq!(resolve_async_compare_limit(&workspace, &suite), 4);
        assert_eq!(
            resolve_async_compare_limit(
                &WorkspaceConfig::default(),
                &TestSuite {
                    async_compare_limit: None,
                    ..suite
                }
            ),
            DEFAULT_ASYNC_COMPARE_LIMIT
        );
    }

    #[test]
    fn async_capture_limit_uses_suite_workspace_and_default_order() {
        let workspace = WorkspaceConfig {
            async_capture_limit: Some(3),
            ..Default::default()
        };
        let mut suite = suite();

        assert_eq!(resolve_async_capture_limit(&workspace, &suite), 3);
        suite.async_capture_limit = Some(4);
        assert_eq!(resolve_async_capture_limit(&workspace, &suite), 4);
        assert_eq!(
            resolve_async_capture_limit(
                &WorkspaceConfig::default(),
                &TestSuite {
                    async_capture_limit: None,
                    ..suite
                }
            ),
            DEFAULT_ASYNC_CAPTURE_LIMIT
        );
    }

    #[test]
    fn snapshot_report_text_loads_from_workspace_yaml() {
        for file_name in ["regressify.yaml", "regressify.yml"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(
                root.path().join(file_name),
                "snapshotTitle: Shop | Regressify\nsnapshotHeading: Shop results\nsnapshotSubheading: '**Staging** regression results'\n",
            )
            .unwrap();

            let config = super::load_workspace_config(root.path()).unwrap();
            assert_eq!(config.snapshot_title.as_deref(), Some("Shop | Regressify"));
            assert_eq!(config.snapshot_heading.as_deref(), Some("Shop results"));
            assert_eq!(
                config.snapshot_subheading.as_deref(),
                Some("**Staging** regression results")
            );
        }
    }

    #[test]
    fn workspace_config_rejects_unknown_properties() {
        let result = serde_yaml::from_str::<WorkspaceConfig>("postinteractionwait: 5000\n");

        assert!(matches!(result, Err(error) if error.to_string().contains("unknown field")));
    }

    #[test]
    fn workspace_file_resolution_requires_an_in_workspace_file() {
        let workspace = tempfile::tempdir().unwrap();
        let file = workspace.path().join("common/cookies.yaml");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "[]\n").unwrap();

        assert_eq!(
            resolve_workspace_file(workspace.path(), "common/cookies.yaml", "cookiePath").unwrap(),
            file.canonicalize().unwrap()
        );
        assert!(
            resolve_workspace_file(workspace.path(), "missing.yaml", "cookiePath")
                .unwrap_err()
                .to_string()
                .contains("does not exist")
        );
        assert!(
            resolve_workspace_file(workspace.path(), "common", "cookiePath")
                .unwrap_err()
                .to_string()
                .contains("is not a file")
        );
    }

    #[test]
    fn optional_workspace_file_resolution_allows_missing_defaults() {
        let workspace = tempfile::tempdir().unwrap();

        assert_eq!(
            resolve_optional_workspace_file(
                workspace.path(),
                "common/missing.css",
                "cssOverridePath",
            )
            .unwrap(),
            None
        );
    }
}
