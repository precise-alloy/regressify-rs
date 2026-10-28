use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::{
    config::{WorkspaceConfig, replacements::ReplacementProfilesFile, viewports::ViewportDef},
    types::{TestAction, TestScenario, TestSuite},
};

pub(super) fn build() -> Result<Value> {
    let mut schemas = Map::new();
    for (kind, schema) in [
        ("workspace", schemars::schema_for!(WorkspaceConfig)),
        ("suite", schemars::schema_for!(TestSuite)),
        ("scenario", schemars::schema_for!(TestScenario)),
        ("action", schemars::schema_for!(TestAction)),
        ("viewports", schemars::schema_for!(Vec<ViewportDef>)),
        (
            "replacement-profiles",
            schemars::schema_for!(ReplacementProfilesFile),
        ),
    ] {
        schemas.insert(kind.into(), serde_json::to_value(schema)?);
    }
    let guides: Map<String, Value> = [
        (
            "overview",
            "Start here: authoring and safe workflows",
            include_str!("guides/overview.md"),
        ),
        (
            "suites",
            "Suite files, discovery, and identity",
            include_str!("guides/suites.md"),
        ),
        (
            "scenarios",
            "Scenarios, reuse, state, and capture selection",
            include_str!("guides/scenarios.md"),
        ),
        (
            "actions",
            "Browser action syntax and execution",
            include_str!("guides/actions.md"),
        ),
        (
            "recipes",
            "Complete suites for common page patterns",
            include_str!("guides/recipes.md"),
        ),
        (
            "workspace",
            "Workspace configuration and precedence",
            include_str!("guides/workspace.md"),
        ),
        (
            "files",
            "Predefined files, scripts, and generated artifacts",
            include_str!("guides/files.md"),
        ),
        (
            "hooks",
            "Injected browser JavaScript and CSS contracts, examples, and lifecycle",
            include_str!("guides/hooks.md"),
        ),
        (
            "troubleshooting",
            "Diagnose configuration, capture, comparison, and MCP failures",
            include_str!("guides/troubleshooting.md"),
        ),
    ]
    .into_iter()
    .map(|(topic, title, content)| (topic.into(), json!({"title": title, "content": content})))
    .collect();

    let mut examples = Map::new();
    for (name, kind, path, description, value) in [
        (
            "minimal-suite",
            "suite",
            "visual_tests/home.tests.yaml",
            "A complete minimal suite. Replace the URL with an authorized test page.",
            json!({"scenarios": [{"id": "home", "label": "Home", "url": "https://example.com"}]}),
        ),
        (
            "responsive-checkout",
            "suite",
            "visual_tests/checkout.tests.yaml",
            "A complete suite using the phone and desktop labels from the viewports example. The final save replaces the automatic final screenshot. Replace selectors with verified page selectors.",
            json!({"displayName": "Checkout", "viewportNames": ["phone", "desktop"], "scenarios": [{"id": "checkout", "label": "Checkout form", "url": "https://example.com/checkout", "actions": [{"click": "#mobile-menu", "viewportNames": "phone"}, {"input": "#email", "value": "qa@example.com"}, {"check": "#terms"}, {"click": "#submit"}, {"wait": "#confirmation"}, {"save": "confirmation", "selector": "#confirmation"}]}]}),
        ),
        (
            "navigation-menus",
            "suite",
            "visual_tests/navigation.tests.yaml",
            "Closed/default and opened/expanded navigation states use the same larger capture area: viewport in every scenario, plus an optional panel detail. A header-only default capture could miss a menu incorrectly expanded before interaction. Use full page instead when the relevant area extends beyond the viewport. Uses the phone and desktop labels from the viewports example; replace selectors with verified page selectors.",
            json!({"viewportNames": ["phone", "desktop"], "scenarios": [
                {"id": "header", "label": "Header", "url": "https://example.com", "selector": "viewport"},
                {"id": "products-menu", "label": "Products menu", "url": "https://example.com", "actions": [{"click": "#menu-toggle", "viewportNames": "phone"}, {"click": "#products-toggle"}, {"wait": "#products-panel"}], "selectors": ["viewport", "#products-panel"]},
                {"id": "account-menu", "label": "Account menu", "url": "https://example.com", "viewportNames": "desktop", "actions": [{"hover": "#account-toggle"}, {"wait": "#account-dropdown"}], "selector": "viewport"}
            ]}),
        ),
        (
            "reuse-actions",
            "suite",
            "visual_tests/cart.tests.yaml",
            "needs replays dependency actions inside each consuming scenario; it is not shared browser state or scheduling.",
            json!({"scenarios": [{"id": "dismiss-consent", "url": "https://example.com/cart", "actions": [{"click": "#accept-cookies"}]}, {"id": "cart", "url": "https://example.com/cart", "needs": "dismiss-consent", "actions": [{"click": "#show-cart"}]}]}),
        ),
        (
            "scenario",
            "scenario",
            "scenarios[] inside a suite",
            "A scenario object to insert into a suite's scenarios list.",
            json!({"id": "product", "label": "Product details", "url": "https://example.com/products/sample", "readySelector": "main", "selector": "#product"}),
        ),
        (
            "workspace",
            "workspace",
            "regressify.yaml",
            "Workspace defaults. Viewport labels are defined separately in common/_viewports.yaml.",
            json!({"misMatchThreshold": 0.1, "pixelMatchThreshold": 5, "viewportNames": ["phone", "desktop"], "hideSelectors": [".timestamp"], "settleTimeout": 5000}),
        ),
        (
            "viewports",
            "viewports",
            "common/_viewports.yaml",
            "A top-level list, not an object with a viewports key. Sizes are CSS pixels.",
            json!([{"label": "phone", "width": 375, "height": 667}, {"label": "tablet", "width": 768, "height": 1024}, {"label": "desktop", "width": 1366, "height": 768}]),
        ),
        (
            "replacement-profiles",
            "replacement-profiles",
            "common/_replacement-profiles.yaml",
            "Reference captures use raw URLs. Test captures apply the selected profile, workspace rules, then suite rules.",
            json!({"profiles": {"default": [], "staging": [{"ref": "https://example.com", "test": "https://staging.example.com"}]}}),
        ),
        (
            "cookies",
            "cookies",
            "common/_cookies.yaml",
            "Synthetic consent cookie only. Keep real authentication cookies private; an empty list is also valid.",
            json!([{"name": "consent", "value": "accepted", "url": "https://example.com", "secure": true, "httpOnly": false, "sameSite": "Lax"}]),
        ),
        (
            "basic-auth",
            "workspace",
            "regressify.yaml",
            "Merge this field into the existing workspace configuration. Resolve these placeholders locally; never upload the environment values.",
            json!({"basicAuth": {"origin": "https://example.com", "username": "${REGRESSIFY_USER}", "password": "${REGRESSIFY_PASSWORD}"}}),
        ),
        (
            "hooked-suite",
            "suite",
            "visual_tests/hooked-home.tests.yaml",
            "A complete suite using the before-script and scenario-ready-script examples at their target paths. All scripts run directly in Chromium without Node or Playwright. Read the hooks guide and adapt the URL/selectors before an authorized capture.",
            json!({"scenarios": [{"id": "home", "url": "https://example.com", "readySelector": "main", "onBeforeScript": "common/hooks/on-before.js", "onReadyScript": "common/hooks/on-ready.js", "selector": "main"}]}),
        ),
    ] {
        examples.insert(name.into(), json!({"kind": kind, "path": path, "description": description, "format": "yaml", "content": serde_yaml::to_string(&value)?}));
    }

    let actions = [
        ("check", json!({"check": "#terms"})),
        ("click", json!({"click": "#submit"})),
        ("focus", json!({"focus": "#email"})),
        ("goto", json!({"goto": "https://example.com/account"})),
        ("hide", json!({"hide": ".timestamp"})),
        ("hover", json!({"hover": "#navigation"})),
        (
            "input",
            json!({"input": "#email", "value": "qa@example.com"}),
        ),
        ("persist", json!({"persist": "signed-in"})),
        ("press", json!({"press": "#search", "key": "Enter"})),
        ("remove", json!({"remove": ".advertisement"})),
        (
            "save",
            json!({"save": "confirmation", "selector": "#confirmation"}),
        ),
        ("select", json!({"select": "#country", "value": "SE"})),
        ("scroll", json!({"scroll": "#footer"})),
        ("switchTab", json!({"switchTab": "new", "url": "**/quote"})),
        ("uncheck", json!({"uncheck": "#newsletter"})),
        (
            "wait",
            json!({"wait": "#dashboard", "url": "**/dashboard*"}),
        ),
        ("drag", json!({"drag": "#card", "to": "#column"})),
        (
            "dropFiles",
            json!({"dropFiles": "#drop-zone", "file": "uploads/sample.pdf"}),
        ),
        ("wheel", json!({"wheel": "#list", "deltaY": 300})),
        ("navigate", json!({"navigate": "back"})),
        ("dialog", json!({"dialog": "accept", "kind": "confirm"})),
        ("key", json!({"key": "Tab"})),
        ("tap", json!({"tap": "#menu", "viewportNames": "mobile"})),
        (
            "swipe",
            json!({"swipe": "#carousel", "offset": {"x": -100, "y": 0}}),
        ),
        ("longPress", json!({"longPress": "#item", "duration": 600})),
        ("blur", json!({"blur": "#email"})),
        ("pinch", json!({"pinch": "#map-canvas", "scale": 2.0})),
        (
            "selectText",
            json!({"selectText": "#editor", "text": "highlighted phrase"}),
        ),
        ("paste", json!({"paste": "#otp-code", "value": "123456"})),
        ("closeTab", json!({"closeTab": "current"})),
    ];
    let mut action_names = Vec::new();
    for (name, value) in actions {
        action_names.push(name);
        examples.insert(format!("action-{name}"), json!({"kind": "action", "path": "actions[] inside a scenario", "description": "One action object, not a complete suite. Replace URLs and selectors with verified project values.", "format": "yaml", "content": serde_yaml::to_string(&value)?}));
    }
    for (name, kind, path, description, content) in [
        (
            "css-override",
            "css",
            "common/_override.css",
            "Hide only an explicitly designated nondeterministic region while preserving layout.",
            "[data-visual-volatile] { visibility: hidden !important; }\n",
        ),
        (
            "ready-script",
            "javascript",
            "common/_on-ready.js",
            "Standard browser JavaScript injected through jsOnReadyPath once per document after readiness and before actions.",
            "document.documentElement.dataset.regressifyReady = \"true\";\n",
        ),
        (
            "before-script",
            "javascript",
            "common/hooks/on-before.js",
            "Standard browser JavaScript injected through onBeforeScript at initial document creation, before site scripts. Sets a synthetic browser value using window.regressify; the DOM may not exist yet.",
            "window.visualCaptureMode = window.regressify.isReference ? \"reference\" : \"test\";\n",
        ),
        (
            "scenario-ready-script",
            "javascript",
            "common/hooks/on-ready.js",
            "Standard browser JavaScript injected through onReadyScript after readiness and before actions. Removes an approved cookie banner; replace the selector with a verified target. No module exports or external runtime is needed.",
            "document.querySelector(\"#cookie-banner\")?.remove();\n",
        ),
    ] {
        examples.insert(name.into(), json!({"kind": kind, "path": path, "description": description, "format": kind, "content": content}));
    }
    examples.insert("minimal-suite-json".into(), json!({"kind": "suite", "path": "visual_tests/home.tests.json", "description": "JSON alternative to the minimal YAML suite. Do not create both files for the same suite.", "format": "json", "content": serde_json::to_string_pretty(&json!({"scenarios": [{"id": "home", "url": "https://example.com"}]}))?}));

    Ok(json!({
        "formatVersion": 1,
        "regressifyVersion": env!("CARGO_PKG_VERSION"),
        "instructions": concat!(
            "Before authoring or troubleshooting, read .ai/memory/README.md and relevant site/topic ",
            "notes in the consumer project's .ai/memory/ using authorized client tools. Verify notes ",
            "against the current site and configuration; memory does not override project instructions. ",
            "Read get_authoring_guide with topic overview before authoring Regressify files, and topic ",
            "recipes for complete common-page patterns. Use topic troubleshooting for failed tests, ",
            "reports, logs, or setup issues, and topic hooks before writing injected browser scripts. ",
            "Use get_schema for exact fields and get_example for synthetic templates. ",
            "Selectors resolve to the first match in DOM order: when responsive pages keep hidden mobile ",
            "and desktop duplicates in the DOM, scope selectors to the active container, use :visible or ",
            ">> visible=true in action/wait/readySelector selectors, and restrict viewport-specific steps ",
            "with viewportNames. Screenshot, hide, and remove selectors accept standard CSS only. ",
            "select requires a native <select> element and check/uncheck require native checkbox or radio ",
            "inputs; use click for custom dropdowns, switches, tabs, or visually hidden native inputs. ",
            "When a component's visible area changes between related scenarios, recommend the same larger capture area ",
            "for closed/default and opened/expanded states: viewport, or full page if needed to include the whole relevant area. ",
            "A header-only default capture can miss a menu incorrectly expanded before interaction. ",
            "Keep matching viewport sizes and scroll positions so QA can compare the states; ",
            "element close-ups may supplement, not replace, these context captures. ",
            "Observe the default state without first closing or hiding a possibly unexpected layer. ",
            "Whenever you create or edit a test suite, you must run ",
            "regressify ref --test-suite <suite-name> for each changed suite after the final edit, ",
            "from the consumer workspace using the client's authorized terminal tools. The CLI and ",
            "desktop app are installed together and added to each user's PATH; invoke regressify ",
            "directly without a source checkout or local build. Use the suite's CLI selection name, ",
            "not its display name, and do not narrow verification with --scenario. Verify from the ",
            "fresh run's results and PNGs that all intended screenshots were captured across the ",
            "suite's configured scenarios and viewports; exit code zero alone is not proof. ",
            "Fix capture failures and rerun before declaring the suite verified. ",
            "ref updates successful references, so respect authorization for target pages and baseline writes. ",
            "If authorization, the CLI, Chromium, credentials, or the target site is unavailable, ",
            "report the blocker and state that capture verification is incomplete; ",
            "do not substitute schema validation. ",
            "Before finishing, record reusable, verified lessons from trial and error, corrections, ",
            "or time-consuming investigation in the consumer project's .ai/memory/: failed approaches, ",
            "why they failed, working fixes, and verification evidence. Create an index and topic notes ",
            "only when needed. Keep each topic note at most 60 lines, with one site and one specific behavior ",
            "or issue per file. Split long or broad notes and keep the README a compact linked index. ",
            "Update existing notes and correct stale advice; see the overview for ",
            "the note format and privacy rules. This server supplies versioned public guidance only; ",
            "it cannot read or edit your workspace, analyze uploaded logs, run browsers, or approve ",
            "baselines. Never send cookies, credentials, saved state, raw logs, project memory, or ",
            "private source. Inspect the user's local files with the client's own authorized tools ",
            "and preserve unrelated edits."
        ),
        "schemaKinds": schemas.keys().collect::<Vec<_>>(),
        "schemas": schemas,
        "guides": guides,
        "actions": action_names,
        "examples": examples
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_local_references(value: &Value, root: &Value) {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref") {
                    let pointer = reference
                        .as_str()
                        .unwrap()
                        .strip_prefix('#')
                        .expect("Schemas must not depend on external files");
                    assert!(root.pointer(pointer).is_some(), "{pointer}");
                }
                for child in object.values() {
                    assert_local_references(child, root);
                }
            }
            Value::Array(items) => {
                for child in items {
                    assert_local_references(child, root);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn schemas_are_valid_and_all_references_are_local() {
        let catalog = build().unwrap();
        for schema in catalog["schemas"].as_object().unwrap().values() {
            jsonschema::validator_for(schema).unwrap();
            assert_local_references(schema, schema);
        }
    }

    #[test]
    fn examples_match_schemas_and_real_parsers() {
        let catalog = build().unwrap();
        for (name, example) in catalog["examples"].as_object().unwrap() {
            let kind = example["kind"].as_str().unwrap();
            if matches!(kind, "css" | "javascript") {
                continue;
            }
            let content = example["content"].as_str().unwrap();
            let value: Value = if example["format"] == "json" {
                serde_json::from_str(content).unwrap()
            } else {
                serde_yaml::from_str(content).unwrap()
            };
            if let Some(schema) = catalog["schemas"].get(kind) {
                let validator = jsonschema::validator_for(schema).unwrap();
                assert!(
                    validator.is_valid(&value),
                    "{name}: {:?}",
                    validator.iter_errors(&value).collect::<Vec<_>>()
                );
            }
            match kind {
                "workspace" => {
                    serde_json::from_value::<WorkspaceConfig>(value).unwrap();
                }
                "suite" => {
                    let suite = serde_json::from_value::<TestSuite>(value).unwrap();
                    for scenario in &suite.scenarios {
                        crate::test::capture::validate_scenario_capture_names(scenario)
                            .unwrap_or_else(|error| panic!("{name}: {error}"));
                        for action in scenario.actions.iter().flatten() {
                            crate::test::validate_action_fields(action)
                                .unwrap_or_else(|error| panic!("{name}: {error}"));
                        }
                    }
                }
                "scenario" => {
                    serde_json::from_value::<TestScenario>(value).unwrap();
                }
                "action" => {
                    let action = serde_json::from_value::<TestAction>(value).unwrap();
                    crate::test::validate_action_fields(&action)
                        .unwrap_or_else(|error| panic!("{name}: {error}"));
                }
                "viewports" => {
                    let viewports: Vec<ViewportDef> = serde_json::from_value(value).unwrap();
                    crate::config::viewports::validate_viewports(
                        &viewports,
                        std::path::Path::new("example"),
                    )
                    .unwrap();
                }
                "replacement-profiles" => {
                    let profiles: ReplacementProfilesFile = serde_json::from_value(value).unwrap();
                    for rules in profiles.profiles.values() {
                        crate::config::replacements::validate_replacements(rules, name).unwrap();
                    }
                }
                "cookies" => {
                    crate::test::capture::expand_cookie_entries(value.as_array().unwrap(), name)
                        .unwrap();
                }
                _ => panic!("Missing example validator for {kind}"),
            }
        }
    }

    #[test]
    fn every_action_has_a_typed_example() {
        let catalog = build().unwrap();
        let root = &catalog["schemas"]["action"];
        let variants = root["anyOf"].as_array().unwrap();
        let actions = catalog["actions"].as_array().unwrap();
        assert_eq!(variants.len(), actions.len());
        for action in actions {
            let name = action.as_str().unwrap();
            assert!(catalog["examples"].get(format!("action-{name}")).is_some());
            assert_eq!(
                variants
                    .iter()
                    .filter(|variant| {
                        let schema = variant
                            .get("$ref")
                            .and_then(Value::as_str)
                            .and_then(|reference| reference.strip_prefix('#'))
                            .and_then(|pointer| root.pointer(pointer))
                            .unwrap_or(variant);
                        actions
                            .iter()
                            .find(|name| schema["required"].as_array().unwrap().contains(name))
                            == Some(action)
                    })
                    .count(),
                1,
                "{name}"
            );
        }
    }

    #[test]
    fn schemas_enforce_local_timeout_and_capture_constraints() {
        let catalog = build().unwrap();
        for (kind, value, expected) in [
            ("workspace", json!({"elementTimeout": 0}), false),
            ("workspace", json!({"actionabilityTimeout": 0}), false),
            ("workspace", json!({"scenarioTimeout": 0}), false),
            (
                "workspace",
                json!({"elementTimeout": 1, "actionabilityTimeout": 1, "scenarioTimeout": 1, "settleTimeout": 0}),
                true,
            ),
            ("workspace", json!({"viewportNames": []}), true),
            (
                "scenario",
                json!({"url": "about:blank", "selectors": []}),
                false,
            ),
            (
                "scenario",
                json!({"url": "about:blank", "selectors": " "}),
                false,
            ),
            (
                "scenario",
                json!({"url": "about:blank", "selectors": ["main", ""]}),
                false,
            ),
            (
                "scenario",
                json!({"url": "about:blank", "selectors": ["main", ".summary"]}),
                true,
            ),
            (
                "scenario",
                json!({"url": "about:blank", "selectors": "viewport"}),
                true,
            ),
            (
                "scenario",
                json!({"url": "about:blank", "selectors": null, "viewportNames": [], "readyTimeout": 0}),
                true,
            ),
            (
                "action",
                json!({"click": "#submit", "viewportNames": []}),
                true,
            ),
            (
                "workspace",
                json!({"imageSimplification": "pixelate", "imageSimplificationBlockSize": 4, "imageSimplificationMinSize": 64, "imageSimplificationExcludeSelectors": []}),
                true,
            ),
            ("workspace", json!({"imageSimplification": "off"}), false),
            (
                "workspace",
                json!({"imageSimplificationBlockSize": 3}),
                false,
            ),
            (
                "workspace",
                json!({"imageSimplificationBlockSize": 18}),
                false,
            ),
            ("workspace", json!({"imageSimplificationMinSize": 0}), false),
            (
                "suite",
                json!({"imageSimplificationBlockSize": 16, "scenarios": []}),
                true,
            ),
            (
                "suite",
                json!({"imageSimplificationBlockSize": 0, "scenarios": []}),
                false,
            ),
            (
                "scenario",
                json!({"url": "about:blank", "imageSimplification": "none", "imageSimplificationBlockSize": 2}),
                true,
            ),
            (
                "scenario",
                json!({"url": "about:blank", "imageSimplificationExcludeSelectors": [".logo", " "]}),
                false,
            ),
        ] {
            let validator = jsonschema::validator_for(&catalog["schemas"][kind]).unwrap();
            assert_eq!(validator.is_valid(&value), expected, "{kind}: {value}");
        }
    }

    #[test]
    fn guides_are_standalone_and_cover_runtime_exceptions() {
        let catalog = build().unwrap();
        for guide in catalog["guides"].as_object().unwrap().values() {
            let body = guide["content"].as_str().unwrap();
            assert!(body.starts_with("# "));
            for forbidden in [
                "../../../",
                "(@/",
                "{%",
                "src/test/",
                ".github/skills/",
                "cargo test",
                "../docs/",
            ] {
                assert!(!body.contains(forbidden), "{forbidden}");
            }
        }
        let workspace = catalog["guides"]["workspace"]["content"].as_str().unwrap();
        assert!(workspace.contains("profile, then workspace, then suite"));
        assert!(workspace.contains("same origin"));
        for contract in [
            "Image simplification is opt-in",
            "imageSimplification: pixelate",
            "re-captured",
            "identical values",
            "`pixelMatchThreshold` above 0",
            "not `off`",
            "`imageSimplificationExcludeSelectors`, and viewport",
        ] {
            assert!(
                workspace.contains(contract),
                "Missing image simplification contract: {contract}"
            );
        }
        let scenarios = catalog["guides"]["scenarios"]["content"].as_str().unwrap();
        assert!(scenarios.contains("not scheduling"));
        assert!(scenarios.contains("last applicable"));
        let files = catalog["guides"]["files"]["content"].as_str().unwrap();
        for path in [
            "_viewports.yaml",
            "_cookies.yaml",
            "_replacement-profiles.yaml",
            "_override.css",
            "_on-ready.js",
            ".vscode/mcp.json",
            "AGENTS.md",
        ] {
            assert!(files.contains(path), "{path}");
        }
        let hooks = catalog["guides"]["hooks"]["content"].as_str().unwrap();
        for contract in [
            "isReference",
            "elementTimeoutMs",
            "before actions",
            "No Node.js or Playwright",
            "window.__REGRESSIFY__",
        ] {
            assert!(
                hooks.contains(contract),
                "Missing hook contract: {contract}"
            );
        }
        for topic in ["overview", "hooks"] {
            let guide = catalog["guides"][topic]["content"].as_str().unwrap();
            for contract in [
                "Do not generate",
                "module.exports",
                ".cjs",
                "page.locator()",
            ] {
                assert!(
                    guide.contains(contract),
                    "Missing browser-only authoring rule in {topic}: {contract}"
                );
            }
        }
        for contract in [
            "ReferenceError: module is not defined",
            "Renaming `.cjs` to `.js` does not convert",
            "`wait` with `url`",
            "does not invoke an exported callback",
        ] {
            assert!(
                hooks.contains(contract),
                "Missing hook migration rule: {contract}"
            );
        }
        let troubleshooting = catalog["guides"]["troubleshooting"]["content"]
            .as_str()
            .unwrap();
        for contract in [
            "--log-level debug",
            "report.json",
            "_diagnostics",
            ".log.json",
            "partial",
            "AGENTS.md",
            "--fetch-browser false",
            "<output>/<suite-id>/bitmaps_test/<run-id>/",
            "Reference image not found",
            "Dimensions differ",
            "ActionIndex",
            "Unobscured",
            "sha256:",
            "readable source metadata",
            "input.value",
            "Older capture logs",
            "lexicographically greatest",
            "data did not match any variant of untagged enum TestAction",
            "Local detail",
            "no element matched",
            "scenarioTimeout of N ms expired during",
            "onBeforeScript",
            "State=Ready` it is the `readySelector`",
        ] {
            assert!(
                troubleshooting.contains(contract),
                "Missing diagnostic contract: {contract}"
            );
        }
        // Captures and reports share the run directory; there is no tests/<run-id> folder.
        assert!(!troubleshooting.contains("/tests/<run-id>"));
        let actions = catalog["guides"]["actions"]["content"].as_str().unwrap();
        for contract in [
            "text=",
            "xpath=",
            ":visible",
            ">> visible=true",
            "first match in DOM order",
            "role=\"combobox\"",
            "role=\"switch\"",
            "standard CSS only",
            "URL must match first",
            "case-sensitive",
            "never substitutes environment variables",
            "Checks Beyond the Schema",
            "Invalid action #N in test suite",
            "missing upload files",
            "without path separators or `__`",
            "inside `save.frame`",
        ] {
            assert!(
                actions.contains(contract),
                "Missing action contract: {contract}"
            );
        }
        assert!(!actions.contains("supplies its timeout"));
        for contract in [
            "Capture Lifecycle",
            "never suppress the final capture",
            "1920x1080",
            "always fails",
            "Opened Menus and Overlays",
            "even the element's own overflowing children",
            "viewport captures never scroll",
            "`viewport` in `selector`, `selectors`, or a `save` action's `selector`",
            "document.getAnimations",
            "requestAnimationFrame",
            "without `__`",
        ] {
            assert!(
                scenarios.contains(contract),
                "Missing scenario contract: {contract}"
            );
        }
        let overview = catalog["guides"]["overview"]["content"].as_str().unwrap();
        for (name, example) in catalog["examples"].as_object().unwrap() {
            if example["kind"] == "action" {
                assert!(
                    overview.contains("action-<name>"),
                    "Missing action-<name> in overview"
                );
            } else {
                assert!(
                    overview.contains(name.as_str()),
                    "Overview is missing discoverable example task name: {name}"
                );
            }
        }
        let recipes = catalog["guides"]["recipes"]["content"].as_str().unwrap();
        for contract in [
            "removeSelectors",
            "switchTab: new",
            "persist: signed-in",
            "frame:",
            "process.env",
            "selector: viewport",
        ] {
            assert!(recipes.contains(contract), "Missing recipe: {contract}");
        }
        assert!(hooks.contains("process.env"));
        assert!(hooks.contains("Local detail"));
        assert!(
            catalog["guides"]["overview"]["content"]
                .as_str()
                .unwrap()
                .contains("topic=\"recipes\"")
        );
    }

    #[test]
    fn guidance_teaches_consumer_project_memory() {
        let catalog = build().unwrap();
        let overview = catalog["guides"]["overview"]["content"].as_str().unwrap();
        for (surface, guidance) in [
            ("instructions", catalog["instructions"].as_str().unwrap()),
            ("overview", overview),
        ] {
            for contract in [
                ".ai/memory/README.md",
                "consumer project",
                "Before authoring or troubleshooting",
                "Before finishing",
                "failed approaches",
                "verified",
                "60 lines",
                "one site and one specific behavior",
                "Split long or broad notes",
                "private",
            ] {
                assert!(
                    guidance.contains(contract),
                    "Missing project memory guidance in {surface}: {contract}"
                );
            }
        }
        for contract in [
            "verification date",
            "project-relative",
            "unverified",
            "stale",
            "Never store secrets",
            "public MCP server",
        ] {
            assert!(
                overview.contains(contract),
                "Missing memory rule: {contract}"
            );
        }
    }

    #[test]
    fn related_component_states_use_a_shared_context_capture() {
        let catalog = build().unwrap();
        for (surface, guidance) in [
            ("instructions", catalog["instructions"].as_str().unwrap()),
            (
                "overview",
                catalog["guides"]["overview"]["content"].as_str().unwrap(),
            ),
            (
                "scenarios",
                catalog["guides"]["scenarios"]["content"].as_str().unwrap(),
            ),
            (
                "recipes",
                catalog["guides"]["recipes"]["content"].as_str().unwrap(),
            ),
        ] {
            let guidance = guidance.split_whitespace().collect::<Vec<_>>().join(" ");
            for contract in [
                "same larger capture area",
                "closed/default",
                "opened/expanded",
                "viewport",
                "full page",
                "before interaction",
                "header-only",
            ] {
                assert!(
                    guidance.contains(contract),
                    "Missing related-state capture guidance in {surface}: {contract}"
                );
            }
        }
        let example: Value = serde_yaml::from_str(
            catalog["examples"]["navigation-menus"]["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let recipes = catalog["guides"]["recipes"]["content"].as_str().unwrap();
        let navigation = recipes
            .split_once("## Header Navigation and Menus")
            .unwrap()
            .1;
        let recipe: Value = serde_yaml::from_str(&yaml_blocks(navigation)[0]).unwrap();
        assert_eq!(
            recipe, example,
            "Navigation recipe and typed example must agree"
        );
        let scenarios = example["scenarios"].as_array().unwrap();
        assert_eq!(scenarios[0]["id"], "header");
        assert!(
            scenarios[0].get("actions").is_none(),
            "Observe the default state without forcing menus closed"
        );
        for scenario in scenarios {
            assert!(
                scenario["selector"] == "viewport"
                    || scenario["selectors"].as_array().is_some_and(|selectors| {
                        selectors.iter().any(|selector| selector == "viewport")
                    }),
                "Every related menu state needs the common viewport capture"
            );
        }
    }

    #[test]
    fn suite_edits_require_reference_capture_verification() {
        let catalog = build().unwrap();
        for (surface, guidance) in [
            ("instructions", catalog["instructions"].as_str().unwrap()),
            (
                "overview",
                catalog["guides"]["overview"]["content"].as_str().unwrap(),
            ),
        ] {
            for contract in [
                "Whenever you create or edit a test suite",
                "must run",
                "regressify ref --test-suite <suite-name>",
                "installed together",
                "PATH",
                "all intended screenshots",
                "exit code zero",
                "updates successful references",
                "report the blocker",
            ] {
                assert!(
                    guidance.contains(contract),
                    "Missing suite capture verification guidance in {surface}: {contract}"
                );
            }
        }
    }

    fn yaml_blocks(markdown: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut current: Option<String> = None;
        for line in markdown.lines() {
            match current.as_mut() {
                None if line == "```yaml" => current = Some(String::new()),
                None => assert!(
                    !line.trim_start().starts_with("```yaml"),
                    "Indented YAML fences are not validated: {line}"
                ),
                Some(_) if line == "```" => blocks.extend(current.take()),
                Some(block) => {
                    block.push_str(line);
                    block.push('\n');
                }
            }
        }
        assert!(current.is_none(), "Unterminated YAML fence");
        blocks
    }

    #[test]
    fn guide_yaml_blocks_are_complete_valid_files() {
        let catalog = build().unwrap();
        let mut validated = 0;
        for (topic, guide) in catalog["guides"].as_object().unwrap() {
            for block in yaml_blocks(guide["content"].as_str().unwrap()) {
                let value: Value = serde_yaml::from_str(&block)
                    .unwrap_or_else(|error| panic!("{topic}: {error}\n{block}"));
                let kind = if value.get("scenarios").is_some() {
                    "suite"
                } else if value.get("profiles").is_some() {
                    "replacement-profiles"
                } else if value.is_array() {
                    "viewports"
                } else {
                    "workspace"
                };
                let validator = jsonschema::validator_for(&catalog["schemas"][kind]).unwrap();
                assert!(
                    validator.is_valid(&value),
                    "{topic} {kind}: {:?}\n{block}",
                    validator.iter_errors(&value).collect::<Vec<_>>()
                );
                match kind {
                    "suite" => {
                        let suite: TestSuite = serde_json::from_value(value).unwrap();
                        let input = crate::types::TestSuiteInput {
                            name: topic.clone(),
                            path: Default::default(),
                            format: crate::types::TestSuiteFormat::Yaml,
                        };
                        crate::test::validate_scenario_dependencies(&input, &suite)
                            .unwrap_or_else(|error| panic!("{topic}: {error}\n{block}"));
                        for scenario in &suite.scenarios {
                            // Guide snippets model the recommended explicit identities.
                            assert!(scenario.id.is_some(), "{topic}: missing id\n{block}");
                            crate::test::capture::validate_scenario_capture_names(scenario)
                                .unwrap_or_else(|error| panic!("{topic}: {error}\n{block}"));
                            for action in scenario.actions.iter().flatten() {
                                crate::test::validate_action_fields(action)
                                    .unwrap_or_else(|error| panic!("{topic}: {error}\n{block}"));
                            }
                        }
                    }
                    "replacement-profiles" => {
                        let profiles: ReplacementProfilesFile =
                            serde_json::from_value(value).unwrap();
                        for rules in profiles.profiles.values() {
                            crate::config::replacements::validate_replacements(rules, topic)
                                .unwrap();
                        }
                    }
                    "viewports" => {
                        let viewports: Vec<ViewportDef> = serde_json::from_value(value).unwrap();
                        crate::config::viewports::validate_viewports(
                            &viewports,
                            std::path::Path::new(topic),
                        )
                        .unwrap();
                    }
                    _ => {
                        serde_json::from_value::<WorkspaceConfig>(value).unwrap();
                    }
                }
                validated += 1;
            }
        }
        assert!(
            validated >= 20,
            "Only {validated} guide YAML blocks were validated"
        );
    }

    #[test]
    fn screenshot_selector_help_explains_viewport_and_element_clipping() {
        let catalog = build().unwrap();
        let scenario = &catalog["schemas"]["scenario"]["properties"]["selector"];
        let save = &catalog["schemas"]["action"]["$defs"]["SaveAction"]["properties"]["selector"];
        for field in [scenario, save] {
            let help = field["description"].as_str().unwrap();
            assert!(help.contains("\"viewport\""), "{help}");
            assert!(help.contains("clipped to its box"), "{help}");
            assert!(
                field["examples"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("viewport"))
            );
        }
    }

    #[test]
    fn wait_help_describes_url_matching_before_the_wait() {
        let catalog = build().unwrap();
        let wait = &catalog["schemas"]["action"]["$defs"]["WaitAction"]["properties"];
        let value = wait["wait"]["description"].as_str().unwrap();
        assert!(value.contains("the URL is matched first"), "{value}");
        assert!(!value.contains("URL timeout"), "{value}");
        assert!(
            wait["url"]["description"]
                .as_str()
                .unwrap()
                .contains("within elementTimeout")
        );
        let example: Value = serde_yaml::from_str(
            catalog["examples"]["action-wait"]["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert!(example["wait"].is_string(), "{example}");
    }

    #[test]
    fn schema_properties_have_direct_or_referenced_help() {
        fn described(schema: &Value, root: &Value) -> bool {
            if schema["description"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
            {
                return true;
            }
            if let Some(target) = schema["$ref"]
                .as_str()
                .and_then(|reference| reference.strip_prefix('#'))
                .and_then(|pointer| root.pointer(pointer))
            {
                return described(target, root);
            }
            ["anyOf", "oneOf", "allOf"].iter().any(|keyword| {
                schema[keyword]
                    .as_array()
                    .is_some_and(|variants| variants.iter().any(|variant| described(variant, root)))
            })
        }

        let catalog = build().unwrap();
        for (kind, root) in catalog["schemas"].as_object().unwrap() {
            for schema in std::iter::once(root).chain(
                root["$defs"]
                    .as_object()
                    .into_iter()
                    .flat_map(|definitions| definitions.values()),
            ) {
                if let Some(properties) = schema["properties"].as_object() {
                    for (name, property) in properties {
                        assert!(described(property, root), "Missing help for {kind}.{name}");
                    }
                }
            }
        }
        let description = catalog["schemas"]["action"]["$defs"]["Selectors"]["description"]
            .as_str()
            .unwrap();
        assert!(!description.contains("visible viewport"));
        assert!(description.contains("containing field"));
        let legacy_typing =
            &catalog["schemas"]["scenario"]["$defs"]["KeyPressSelector"]["properties"]["keyPress"];
        assert!(
            legacy_typing["description"]
                .as_str()
                .unwrap()
                .contains("Text to append")
        );
        assert_eq!(legacy_typing["examples"][0], "search terms");
        let element_timeout =
            catalog["schemas"]["workspace"]["properties"]["elementTimeout"]["description"]
                .as_str()
                .unwrap();
        assert!(element_timeout.contains("injected browser hooks"));
        assert!(!element_timeout.contains("Node"));
        let action_defs = &catalog["schemas"]["action"]["$defs"];
        assert!(
            action_defs["InputAction"]["properties"]["input"]["description"]
                .as_str()
                .unwrap()
                .contains("preflight before any browser starts")
        );
        assert!(
            action_defs["PressAction"]["properties"]["key"]["description"]
                .as_str()
                .unwrap()
                .contains("preflight before any browser starts")
        );
        assert!(
            action_defs["SelectAction"]["properties"]["select"]["description"]
                .as_str()
                .unwrap()
                .contains("custom dropdowns or comboboxes")
        );
        assert!(
            action_defs["CheckAction"]["properties"]["check"]["description"]
                .as_str()
                .unwrap()
                .contains("role=\"switch\"")
        );
        assert!(
            action_defs["SaveAction"]["properties"]["save"]["description"]
                .as_str()
                .unwrap()
                .contains("or __")
        );
        assert!(
            action_defs["SaveAction"]["properties"]["selector"]["description"]
                .as_str()
                .unwrap()
                .contains("target frame when frame is set")
        );
    }

    #[test]
    fn schemas_reject_unknown_fields_and_ambiguous_actions() {
        let catalog = build().unwrap();
        for (kind, invalid) in [
            ("workspace", json!({"misMatchTreshold": 1})),
            ("suite", json!({"scenarios": [], "unknown": true})),
            (
                "scenario",
                json!({"url": "https://example.com", "selector": "main", "selectors": "viewport"}),
            ),
            ("action", json!({"click": "#submit", "hover": "#menu"})),
            (
                "viewports",
                json!([{"label": "phone", "width": 0, "height": 600}]),
            ),
        ] {
            assert!(
                !jsonschema::validator_for(&catalog["schemas"][kind])
                    .unwrap()
                    .is_valid(&invalid),
                "{kind}"
            );
        }
    }
}
