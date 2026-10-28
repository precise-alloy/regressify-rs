#![allow(dead_code)]
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::actions::TestAction;

pub struct TestSuiteInput {
    pub name: String,

    pub path: PathBuf,

    pub format: TestSuiteFormat,
}

pub enum TestSuiteFormat {
    Yaml,

    Json,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct TestSuite {
    /// Diagnostic display level. Inherits workspace logLevel; does not filter archived records.
    pub log_level: Option<crate::diagnostics::record::Severity>,
    /// Output identifier for report and screenshot folders, also used by compare, approve, and snapshot filters. Defaults to the lowercase suite filename stem (Shop.tests.yaml becomes shop); changing it moves baselines.
    pub id: Option<String>,

    /// Human-readable suite name shown in the desktop app and reports. Does not change the filename or output identifier.
    #[schemars(example = "Customer checkout")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// Runtime metadata: Regressify replaces it with the current run ID. Do not set it when authoring tests.
    pub test_date: Option<String>,

    /// Maximum simultaneous browser captures. Positive whole number; inherits the workspace setting (default: 5).
    #[schemars(range(min = 1), example = 5)]
    pub async_capture_limit: Option<usize>,

    /// Maximum simultaneous image comparisons. Positive whole number; inherits the workspace setting (default: 2).
    #[schemars(range(min = 1), example = 2)]
    pub async_compare_limit: Option<usize>,

    /// Automatically clean old test runs after this complete suite finishes. Inherits the workspace setting (default: false); false disables an enabled workspace policy.
    pub auto_clean_runs: Option<bool>,

    /// Maximum completed test runs to retain. Minimum: 1; inherits the workspace value (default: 1). Always preserves the latest completed full-suite run.
    #[schemars(range(min = 1), example = 1)]
    pub keep_test_runs: Option<usize>,

    /// Browser engine. Only "chromium" is supported; normally leave unset to inherit it.
    pub browser: Option<Browser>,

    /// HTTP Basic Auth for this suite. Supply one object or a list; every entry needs origin, username, and password.
    pub basic_auth: Option<BasicAuthConfig>,

    /// Bypass the site's Content Security Policy. Inherits the workspace setting (default: false).
    pub bypass_csp: Option<bool>,

    /// Cookie YAML or JSON file to load before navigation. Inherits the workspace setting.
    #[schemars(example = "common/_cookies.yaml")]
    pub cookie_path: Option<String>,

    /// CSS file injected into every page document before actions and capture. Inherits the workspace setting; useCssOverride controls whether it is applied.
    #[schemars(example = "common/_override.css")]
    pub css_override_path: Option<String>,

    /// Show the browser while capturing (default: false). Always disabled in CI environments without a display.
    pub debug: Option<bool>,

    /// Standard CSS selectors hidden with visibility: hidden (layout kept) in every document before actions and capture; selectors matching nothing are ignored. A list is required, even for one selector.
    #[schemars(example = vec![".timestamp", ".cookie-banner"])]
    pub hide_selectors: Option<Vec<String>>,

    /// Ignore HTTPS certificate errors for this suite. Inherits the workspace setting (default: false).
    pub ignore_ssl_errors: Option<bool>,

    /// Opt-in image simplification before each screenshot: none (default) keeps images unchanged; pixelate turns large images in the main document into uniform blocks of their average color. Enabling it or changing any image simplification setting changes pixels, so re-capture references; keep pixelMatchThreshold above 0. Inherits the workspace setting; scenarios can override it.
    #[schemars(example = &"pixelate")]
    pub image_simplification: Option<ImageSimplification>,

    /// Image simplification block size in whole CSS pixels: an even number from 2 to 16 (default: 4). Larger blocks remove more detail. Inherits the workspace setting; scenarios can override it.
    #[schemars(range(min = 2, max = 16), extend("multipleOf" = 2), example = 4)]
    pub image_simplification_block_size: Option<u32>,

    /// Image simplification minimum size in whole CSS pixels (default: 64; at least 1). Images rendered narrower or shorter stay sharp. Inherits the workspace setting; scenarios can override it.
    #[schemars(range(min = 1), example = 64)]
    pub image_simplification_min_size: Option<u32>,

    /// Standard CSS selectors for images, or elements containing images, that image simplification keeps sharp; invalid selectors match nothing. A list is required, entries must not be blank, and it replaces inherited selectors.
    #[schemars(example = vec![".product-photo", "#hero img"])]
    #[schemars(extend("items" = serde_json::json!({"type": "string", "minLength": 1, "pattern": r"\S"})))]
    pub image_simplification_exclude_selectors: Option<Vec<String>>,

    /// Standard browser JavaScript injected once per active page document after readiness and before onReadyScript and actions. Returned promises are awaited. No Node.js or Playwright is required; inherits the workspace setting.
    #[schemars(example = "common/_on-ready.js")]
    pub js_on_ready_path: Option<String>,

    /// Minimum whole milliseconds at each lazy-load scroll stop. Resource checks still run with 0. Inherits the workspace setting (default: 250); scenarios can override it. Ignored when noScrollTop is true.
    #[schemars(example = 250)]
    pub lazy_load_scroll_delay: Option<u64>,

    /// Positive whole milliseconds for the complete lazy-load scroll and return to the top. Expiry fails the capture. Inherits the workspace setting (default: 20000); scenarios can override it. Ignored when noScrollTop is true.
    #[schemars(range(min = 1), example = 20000)]
    pub lazy_load_timeout: Option<u64>,

    /// Allowed percentage of different pixels, from 0 to 100. Inherits the workspace setting (default: 0.1, meaning 0.1%).
    #[schemars(range(min = 0.0, max = 100.0), example = 0.1)]
    pub mis_match_threshold: Option<f64>,

    /// Per-pixel color tolerance from 0 to 100. Lower values highlight subtler color changes; this is not the allowed percentage of different pixels. Inherits the workspace setting (default: 5).
    #[schemars(range(min = 0.0, max = 100.0), example = 5.0)]
    pub pixel_match_threshold: Option<f64>,

    /// Milliseconds to wait after all actions, before the final captures (not before save checkpoints). Non-negative; inherits the workspace setting (default: 1000).
    #[schemars(range(min = 0.0), example = 1000)]
    pub post_interaction_wait: Option<f64>,

    /// Standard CSS selectors set to display: none in every document before actions and capture; selectors matching nothing are ignored. A list is required.
    #[schemars(example = vec![".advertisement"])]
    pub remove_selectors: Option<Vec<String>>,

    /// Independent test scenarios. Each list item requires a url and may contain actions and capture settings.
    #[schemars(example = serde_json::json!([{"url": "https://example.com", "label": "Home page"}]))]
    pub scenarios: Vec<TestScenario>,

    /// Saved browser state name or list of names, without .json, loaded from the active output directory's states folder. Replaces workspace state; files must exist before the run starts.
    #[schemars(example = "signed-in")]
    pub state: Option<Needs>,

    /// URL replacement rules for test captures, applied in order after the replacement profile and workspace rules. Each object requires ref (text to match) and test (replacement).
    pub url_replacements: Option<Vec<UrlReplacement>>,

    /// Apply the CSS override file to each page document. hideSelectors and removeSelectors still apply when false. Inherits the workspace setting (default: true).
    pub use_css_override: Option<bool>,

    /// Viewport label or list of labels from the viewport file. Matching ignores case; an empty list selects all viewports.
    #[schemars(example = vec!["phone", "desktop"])]
    pub viewport_names: Option<Selectors>,

    /// Workspace-relative viewport definition file. Default: common/_viewports.yaml. Explicit paths must exist inside the workspace.
    #[schemars(example = "common/_viewports.yaml")]
    pub viewports_path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(schemars::JsonSchema)]
pub enum Browser {
    Chromium,

    #[schemars(skip)]
    Firefox,

    #[schemars(skip)]
    Webkit,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
#[schemars(extend("not" = serde_json::json!({
    "required": ["selector", "selectors"],
    "properties": {
        "selector": {"type": "string"},
        "selectors": {"type": ["string", "array"]}
    }
})))]
pub struct TestScenario {
    /// Diagnostic display level. Inherits suite/workspace logLevel; does not filter archived records.
    pub log_level: Option<crate::diagnostics::record::Severity>,
    /// Browser actions, executed in order after needs actions. A present list, including an empty list, disables legacy shortcuts. Use one action per item; a failed action stops this scenario but not its peers.
    pub actions: Option<Vec<TestAction>>,

    /// Legacy hover shortcut. Prefer an explicit hover action for ordered interactions.
    pub hover_selector: Option<String>,
    /// Legacy hover shortcut accepting one CSS selector or a list. Prefer explicit hover actions.
    pub hover_selectors: Option<Selectors>,
    /// Legacy click shortcut accepting one CSS selector or a list. Prefer explicit click actions.
    pub click_selectors: Option<Selectors>,
    /// Legacy text-append shortcut with selector and keyPress, used only when actions is omitted. Prefer an input action with value and append: true.
    pub key_press_selector: Option<KeyPressSelector>,
    /// Legacy text-append shortcuts, used only when actions is omitted. Each entry needs selector and literal keyPress text; prefer explicit input actions.
    pub key_press_selectors: Option<Vec<KeyPressSelector>>,
    /// Legacy CSS selector to scroll into view. Prefer an explicit scroll action.
    pub scroll_to_selector: Option<String>,
    /// Element selector that must exist and be visible after the initial navigation, before CSS/on-ready hooks, lazy-load scrolling, and actions. Use readyTimeout to override its timeout.
    #[schemars(example = &"main")]
    pub ready_selector: Option<String>,
    /// Whole milliseconds to wait for readySelector. Omit or use zero to inherit elementTimeout (default: 30000); readiness runs before dependency and scenario actions.
    #[schemars(example = 30000)]
    pub ready_timeout: Option<u64>,
    /// Navigation settings, nested under gotoParameters (waitUntil, timeout, and referer).
    pub engine_options: Option<EngineOptions>,
    /// Workspace-relative standard browser JavaScript injected at the start of the initial top-level document, before site scripts. The DOM may not exist yet. Use window.regressify for capture context; no module exports, Node.js, or Playwright. Returned promises are awaited before readiness and actions, bounded by elementTimeout.
    #[schemars(example = "common/hooks/on-before.js")]
    pub on_before_script: Option<String>,
    /// Workspace-relative standard browser JavaScript injected after DOM readiness and readySelector, after CSS and jsOnReadyPath, before dependency, scenario, or legacy actions. Runs once per selected page document, including navigation and switchTab. Use DOM APIs and window.regressify, not module exports, Node.js, or Playwright. Returned promises are awaited; elementTimeout bounds execution.
    #[schemars(example = "common/hooks/on-ready.js")]
    pub on_ready_script: Option<String>,

    /// HTTP Basic Auth for this scenario. Each object needs origin, username, and password; matching origins override inherited credentials.
    pub basic_auth: Option<BasicAuthConfig>,

    /// Bypass the site's Content Security Policy. Inherits the suite or workspace setting (default: false).
    pub bypass_csp: Option<bool>,

    /// Legacy CSS selector to click. Prefer an explicit click action.
    pub click_selector: Option<String>,

    /// Cookie YAML or JSON file to load before navigation. Inherits the suite or workspace setting.
    #[schemars(example = "common/_cookies.yaml")]
    pub cookie_path: Option<String>,

    /// CSS file injected into every page document before actions and capture. Inherits the suite or workspace setting.
    #[schemars(example = "common/_override.css")]
    pub css_override_path: Option<String>,

    /// Milliseconds to wait before each save checkpoint and before the final captures. Non-negative; inherits the workspace setting (default: 1000); 0 skips it.
    #[schemars(range(min = 0.0), example = 1000)]
    pub delay: Option<f64>,

    /// Free-text notes about what this scenario covers.
    pub description: Option<String>,

    /// Standard CSS selectors hidden with visibility: hidden (layout kept) in every document before actions and capture. A list is required; replaces inherited hideSelectors.
    #[schemars(example = vec![".timestamp"])]
    pub hide_selectors: Option<Vec<String>>,

    /// Unique ID (case-insensitive) used by needs, --scenario filters, and screenshot filenames; a portable path component of at most 80 bytes. Omit to use a slug of label, or a URL hash when label is also omitted.
    #[schemars(example = "sign-in")]
    pub id: Option<String>,

    /// Opt-in image simplification before each screenshot: none keeps images unchanged; pixelate turns large images in the main document into uniform blocks of their average color. Enabling it or changing any image simplification setting changes pixels, so re-capture references; keep pixelMatchThreshold above 0. Inherits the suite or workspace setting (default: none).
    #[schemars(example = &"pixelate")]
    pub image_simplification: Option<ImageSimplification>,

    /// Image simplification block size in whole CSS pixels: an even number from 2 to 16. Larger blocks remove more detail. Inherits the suite or workspace setting (default: 4).
    #[schemars(range(min = 2, max = 16), extend("multipleOf" = 2), example = 4)]
    pub image_simplification_block_size: Option<u32>,

    /// Image simplification minimum size in whole CSS pixels (at least 1). Images rendered narrower or shorter stay sharp. Inherits the suite or workspace setting (default: 64).
    #[schemars(range(min = 1), example = 64)]
    pub image_simplification_min_size: Option<u32>,

    /// Standard CSS selectors for images, or elements containing images, that image simplification keeps sharp; invalid selectors match nothing. A list is required, entries must not be blank, and it replaces inherited selectors.
    #[schemars(example = vec![".product-photo", "#hero img"])]
    #[schemars(extend("items" = serde_json::json!({"type": "string", "minLength": 1, "pattern": r"\S"})))]
    pub image_simplification_exclude_selectors: Option<Vec<String>>,

    /// Standard browser JavaScript injected once per active page document after readiness and before onReadyScript and actions. Returned promises are awaited. No Node.js or Playwright is required; inherits the suite or workspace setting.
    #[schemars(example = "common/_on-ready.js")]
    pub js_on_ready_path: Option<String>,

    /// Human-readable scenario name shown in reports and navigation. Also used to generate an ID when id is omitted.
    #[schemars(example = "Home page")]
    pub label: Option<String>,

    /// Minimum whole milliseconds at each lazy-load scroll stop. Resource checks still run with 0. Inherits the suite or workspace setting (default: 250). Ignored when noScrollTop is true.
    #[schemars(example = 250)]
    pub lazy_load_scroll_delay: Option<u64>,

    /// Positive whole milliseconds for the complete lazy-load scroll and return to the top. Expiry fails the capture. Inherits the suite or workspace setting (default: 20000). Ignored when noScrollTop is true.
    #[schemars(range(min = 1), example = 20000)]
    pub lazy_load_timeout: Option<u64>,

    /// Allowed percentage of different pixels, from 0 to 100. Inherits the suite or workspace setting (default: 0.1%).
    #[schemars(range(min = 0.0, max = 100.0), example = 0.1)]
    pub mis_match_threshold: Option<f64>,

    /// Per-pixel color tolerance from 0 to 100. Lower values highlight subtler color changes; this is not the allowed percentage of different pixels. Inherits the suite or workspace setting (default: 5).
    #[schemars(range(min = 0.0, max = 100.0), example = 5.0)]
    pub pixel_match_threshold: Option<f64>,

    /// ID or list of IDs of other scenarios in this suite whose actions are replayed first inside this scenario's own browser. Not run order or a shared session; do not reference this scenario or create cycles.
    #[schemars(example = "sign-in")]
    pub needs: Option<Needs>,

    /// Skip the one-time lazy-load scroll that runs after navigation and before actions. Inherits the workspace setting (default: false).
    pub no_scroll_top: Option<bool>,

    /// Milliseconds to wait after all actions, before the final captures (not before save checkpoints). Non-negative; inherits the suite or workspace setting (default: 1000).
    #[schemars(range(min = 0.0), example = 1000)]
    pub post_interaction_wait: Option<f64>,

    /// Standard CSS selectors set to display: none in every document before actions and capture. A list is required; replaces inherited removeSelectors.
    #[schemars(example = vec![".advertisement"])]
    pub remove_selectors: Option<Vec<String>>,

    /// Login requirement flag exposed to ready scripts. This does not perform a login automatically.
    pub required_login: Option<bool>,

    /// Saved state name or list of names to restore in addition to inherited state. Created by the persist action; files must exist before the run starts.
    #[schemars(example = "signed-in")]
    pub restore: Option<Needs>,

    /// Standard CSS selector for the FINAL screenshot only, or "viewport" for the visible viewport. An element capture uses the first match in the selected tab's main document, which must be visible, clipped to its box, so open menus or popups extending outside it are cut off. Omit to capture the whole page; cannot be combined with selectors. A last applicable save skips this capture.
    #[schemars(
        length(min = 1),
        regex(pattern = r"\S"),
        example = "#content",
        example = &"viewport"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,

    /// Standard CSS selectors to capture instead of the full page. Use "viewport" for
    /// the currently visible viewport; each selector produces an independent screenshot
    /// clipped to its element's box, even when the last applicable action is save. Cannot be combined with selector.
    /// A list must contain at least one target; blank selectors are invalid. Omit or use null for the automatic full-page capture.
    #[schemars(example = &"viewport", example = vec!["main", ".summary"])]
    #[schemars(extend(
        "minItems" = 1,
        "minLength" = 1,
        "pattern" = r"\S",
        "items" = serde_json::json!({"type": "string", "minLength": 1, "pattern": r"\S"})
    ))]
    pub selectors: Option<Selectors>,

    /// Required page URL opened before actions, for example https://example.com. Test captures apply URL replacement rules first; reference captures use it as written.
    #[schemars(length(min = 1), example = "https://example.com")]
    pub url: String,

    /// Apply the CSS override file to each page document. hideSelectors and removeSelectors still apply when false. Inherits the suite or workspace setting (default: true).
    pub use_css_override: Option<bool>,

    /// Viewport label or list of labels from the viewport file. Replaces inherited selection; an empty list selects all viewports.
    #[schemars(example = vec!["phone", "desktop"])]
    pub viewport_names: Option<Selectors>,
}

/// A string or list of strings. The containing field defines their meaning
/// and whether an empty list is allowed; viewport labels are not CSS selectors.
#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
#[derive(schemars::JsonSchema)]
pub enum Selectors {
    String(String),

    StringArray(Vec<String>),
}

impl Selectors {
    pub fn values(&self) -> &[String] {
        match self {
            Self::String(value) => std::slice::from_ref(value),
            Self::StringArray(values) => values,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct KeyPressSelector {
    /// CSS selector of the element that receives appended text. Quote selectors beginning with # in YAML.
    #[schemars(example = "#search")]
    pub selector: String,
    /// Text to append literally, not a keyboard key or combination. Prefer input.value for text; use press.key to send Enter, Tab, or other keyboard events.
    #[schemars(example = "search terms")]
    pub key_press: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct EngineOptions {
    /// Navigation checkpoint, timeout in milliseconds, and optional Referer header.
    pub goto_parameters: Option<GotoParameters>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct GotoParameters {
    /// Navigation checkpoint: commit, domcontentloaded, load, or networkidle. Default: load.
    pub wait_until: Option<NavigationWait>,
    /// Initial navigation timeout in whole milliseconds. Omit or use zero for the 60000 ms default, independent of elementTimeout. The overall scenario timeout still applies.
    #[schemars(example = 60000)]
    pub timeout: Option<u64>,
    /// Optional Referer HTTP header for navigation.
    #[schemars(example = "https://example.com")]
    pub referer: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(schemars::JsonSchema)]
pub enum NavigationWait {
    Commit,
    DomContentLoaded,
    #[default]
    Load,
    NetworkIdle,
}

/// Image simplification mode: none or pixelate.
// Variants intentionally have no doc comments so the schema stays a plain string
// enum for desktop choices. "none" rather than "off": YAML 1.1 tools read an
// unquoted `off` as the boolean false.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(schemars::JsonSchema)]
pub enum ImageSimplification {
    #[default]
    None,
    Pixelate,
}

/// A name or a list of names, processed in the given order.
#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
#[derive(schemars::JsonSchema)]
pub enum Needs {
    String(String),

    StringArray(Vec<String>),
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct BasicAuth {
    /// Website origin to authenticate, including the scheme and optional port, for example
    /// https://example.com. Required for every entry; credentials are only sent to this origin.
    #[schemars(length(min = 1), example = "https://example.com")]
    pub origin: String,
    /// HTTP Basic Auth username. Use "${BASIC_AUTH_USERNAME}" to read an environment variable.
    #[schemars(length(min = 1), example = "${BASIC_AUTH_USERNAME}")]
    pub username: String,
    /// HTTP Basic Auth password. Use "${BASIC_AUTH_PASSWORD}" instead of committing a secret.
    #[schemars(length(min = 1), example = "${BASIC_AUTH_PASSWORD}")]
    pub password: String,
}

/// HTTP Basic Auth credentials as one object or a list of objects. Each entry needs
/// origin, username, and password. Credentials for a matching origin override inherited ones.
#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
#[derive(schemars::JsonSchema)]
#[schemars(example = serde_json::json!({"origin": "https://example.com", "username": "${BASIC_AUTH_USERNAME}", "password": "${BASIC_AUTH_PASSWORD}"}))]
pub enum BasicAuthConfig {
    Single(BasicAuth),
    Multiple(Vec<BasicAuth>),
}

impl BasicAuthConfig {
    pub fn entries(&self) -> &[BasicAuth] {
        match self {
            Self::Single(entry) => std::slice::from_ref(entry),
            Self::Multiple(entries) => entries,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct UrlReplacement {
    /// Text to find in the URL (every occurrence), or a Rust regular expression when regex is true; look-around and backreferences are unsupported.
    #[serde(rename = "ref")]
    #[schemars(example = "https://production.example.com")]
    pub ref_pattern: String,

    /// Replacement text. With regex, use ${1} or ${name} for capture groups: a bare $1 followed by a letter, digit, or underscore is read as a longer group name.
    #[schemars(example = "https://staging.example.com")]
    pub test: String,

    /// Treat ref as a regular expression (default: false). Otherwise perform a literal replacement.
    pub regex: Option<bool>,

    /// Regular-expression flags, such as i for case-insensitive matching. Only used when regex is true.
    #[schemars(example = &"i")]
    pub flags: Option<String>,
}
