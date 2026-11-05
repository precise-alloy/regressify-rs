use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::{
    common, config,
    helpers::{IOExtensions, slugify},
    snapshot::get_asset_version,
    test::{
        browsers::{LaunchOptions, launch_browser},
        capture::is_artifact_write_error,
        test_scenario::{ScenarioRequest, exec_test_scenario, make_log_prefix},
    },
    types::{TestScenario, TestSuite, TestSuiteInput},
};

/// Workspace and replacement-profile context used when loading a test suite.
pub struct SuiteProfileContext<'a> {
    pub root_dir: &'a Path,
    pub profiles: Option<&'a config::replacements::ReplacementProfilesFile>,
    pub cli_profile: Option<&'a str>,
    pub mode: config::replacements::ReplacementMode,
}

/// Parse a test suite file (YAML or JSON) and assign IDs to scenarios.
#[cfg(test)]
pub fn parse_test_suite(visual_tests_dir: &Path, input: &TestSuiteInput) -> Result<TestSuite> {
    parse_test_suite_with_profile(visual_tests_dir, input, None)
}

/// Parse a test suite file (YAML or JSON), apply the active replacement profile's
/// ordered steps (URL replacements and scripts), and assign IDs to scenarios.
pub fn parse_test_suite_with_profile(
    visual_tests_dir: &Path,
    input: &TestSuiteInput,
    profile_ctx: Option<&SuiteProfileContext<'_>>,
) -> Result<TestSuite> {
    let TestSuiteInput { name, path, format } = input;

    let test_suite_path = if path.is_absolute() {
        path.clone()
    } else {
        visual_tests_dir.join(path)
    };
    if !test_suite_path.exists() {
        return Err(anyhow!(
            "Test suite file '{}' does not exist.",
            test_suite_path.display()
        ));
    }

    crate::progress::scenario_log!(
        "Loading test suite from '{}'",
        test_suite_path.normalize_string(false)?
    );
    let test_suite_content = std::fs::read_to_string(&test_suite_path)?;

    crate::progress::scenario_log!("Parsing test suite...");
    let mut test_suite: TestSuite = if let Some(ctx) = profile_ctx {
        let mut suite_value: serde_json::Value = match format {
            crate::types::TestSuiteFormat::Yaml => serde_yaml::from_str(&test_suite_content)?,
            crate::types::TestSuiteFormat::Json => serde_json::from_str(&test_suite_content)?,
        };
        let suite_profile = match suite_value.get("replacementProfile") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(profile_str)) => {
                if profile_str.trim().is_empty() {
                    anyhow::bail!("Test suite '{}' replacementProfile must not be empty", name);
                }
                Some(profile_str.clone())
            }
            Some(_) => {
                // Delegate non-string `replacementProfile` values to typed deserialization
                // so serde reports the expected field type error.
                let typed: TestSuite = match format {
                    crate::types::TestSuiteFormat::Yaml => {
                        serde_yaml::from_str(&test_suite_content)?
                    }
                    crate::types::TestSuiteFormat::Json => {
                        serde_json::from_str(&test_suite_content)?
                    }
                };
                typed.replacement_profile
            }
        };

        let (profile_name, explicit_profile) = config::replacements::resolve_profile_selection(
            ctx.cli_profile,
            suite_profile.as_deref(),
        );
        if profile_name.trim().is_empty() {
            anyhow::bail!("Replacement profile name must not be empty");
        }

        let active_items = match ctx.profiles {
            Some(profiles) => match profiles.profiles.get(&profile_name) {
                Some(items) => items.as_slice(),
                None if explicit_profile => {
                    anyhow::bail!("Replacement profile '{}' is not defined", profile_name)
                }
                None => &[],
            },
            None if explicit_profile => {
                anyhow::bail!(
                    "Replacement profile file not found: {}",
                    ctx.root_dir
                        .join("common/_replacement-profiles.yaml")
                        .display()
                )
            }
            None => &[],
        };

        if !active_items.is_empty() {
            config::replacements::validate_profile_items(
                ctx.root_dir,
                active_items,
                &format!("replacement profile '{}'", profile_name),
            )?;
            config::replacements::apply_profile_items_to_suite_value(
                ctx.root_dir,
                active_items,
                &mut suite_value,
                ctx.mode,
                &profile_name,
                name,
            )?;
            serde_json::from_value(suite_value)?
        } else {
            match format {
                crate::types::TestSuiteFormat::Yaml => serde_yaml::from_str(&test_suite_content)?,
                crate::types::TestSuiteFormat::Json => serde_json::from_str(&test_suite_content)?,
            }
        }
    } else {
        match format {
            crate::types::TestSuiteFormat::Yaml => serde_yaml::from_str(&test_suite_content)?,
            crate::types::TestSuiteFormat::Json => serde_json::from_str(&test_suite_content)?,
        }
    };

    if test_suite
        .replacement_profile
        .as_deref()
        .is_some_and(|profile| profile.trim().is_empty())
    {
        anyhow::bail!("Test suite '{}' replacementProfile must not be empty", name);
    }

    if test_suite.id.is_none() {
        test_suite.id = Some(name.clone());
    }

    // An explicit CLI run ID groups every selected suite under one known run;
    // otherwise use a sortable timestamp that avoids same-second collisions.
    let now = chrono::Utc::now();
    test_suite.test_date = Some(resolve_test_run_id(
        common::RUN_ID.get().map(String::as_str),
        now,
    ));

    if test_suite.scenarios.is_empty() {
        return Err(anyhow!("Test suite contains no scenarios."));
    }

    // Loop through scenarios and assign IDs if missing
    assign_scenario_ids(&mut test_suite.scenarios);

    // Screenshot names must remain unique on case-insensitive filesystems.
    let mut scenario_ids = std::collections::HashMap::new();
    for (index, scenario) in test_suite.scenarios.iter().enumerate() {
        let scenario_id = scenario
            .id
            .as_ref()
            .ok_or_else(|| anyhow!("Scenario ID is missing."))?;
        if let Some(previous_index) = scenario_ids.insert(scenario_id.to_lowercase(), index) {
            let describe_scenario = |index: usize| {
                let scenario = &test_suite.scenarios[index];
                let label = scenario
                    .label
                    .as_ref()
                    .map(|label| format!(" (label: {label:?})"))
                    .unwrap_or_default();
                format!(
                    "  Scenario #{}{label}: URL {:?}",
                    index + 1,
                    scenario_diagnostic_url(&scenario.url)
                )
            };
            return Err(anyhow!(
                "Duplicate scenario ID found in test suite '{}' ({}): '{}'.\n{}\n{}\nScenario IDs must be unique within a test suite (case-insensitive). IDs default to a slug of 'label', or a hash of 'url' when no label is set. Give each conflicting scenario a unique explicit 'id', or a distinct 'label' when no 'id' is set.",
                name,
                test_suite_path.display(),
                scenario_id,
                describe_scenario(previous_index),
                describe_scenario(index),
            ));
        }
    }

    Ok(test_suite)
}

pub(crate) fn assign_scenario_ids(scenarios: &mut [TestScenario]) {
    for scenario in scenarios {
        match (scenario.id.as_ref(), scenario.label.as_ref()) {
            (None, Some(label)) => scenario.id = Some(slugify(label)),
            (None, None) => scenario.id = Some(get_asset_version(scenario.url.as_bytes(), 20)),
            (Some(_), _) => {}
        }
    }
}

fn scenario_diagnostic_url(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return value.to_string();
    };
    if url.password().is_none() {
        return value.to_string();
    }

    let _ = url.set_password(Some("***"));
    url.to_string()
}

fn test_run_id(time: chrono::DateTime<chrono::Utc>) -> String {
    format!(
        "{}_{:09}",
        time.format("%Y%m%d_%H%M%S"),
        time.timestamp_subsec_nanos()
    )
}

fn resolve_test_run_id(
    explicit_run_id: Option<&str>,
    time: chrono::DateTime<chrono::Utc>,
) -> String {
    explicit_run_id
        .map(str::to_owned)
        .unwrap_or_else(|| test_run_id(time))
}

/// Execute a single scenario: launch browser, run scenario, close browser.
/// Categorized result of running a single scenario.
///
/// A visual regression tool must keep running (and exit zero) when the *page
/// under test* misbehaves — timeouts, missing elements, navigation errors are
/// expected outcomes that should surface in the comparison report, not crash
/// the run. Only failures of our own infrastructure (launching the browser)
/// should fail the command.
pub enum ScenarioOutcome {
    /// Scenario captured successfully.
    Passed,
    /// The page under test caused the failure (timeout, element not found,
    /// navigation error, etc.). Expected — must NOT fail the command.
    PageFailed(anyhow::Error),
    /// The browser or an output artifact failed. Must fail the command with a
    /// non-zero exit code.
    InfraFailed(anyhow::Error),
}

async fn wait_for_browser_handler(
    handle: &mut tokio::task::JoinHandle<()>,
    timeout: std::time::Duration,
    _log_prefix: &str,
) -> bool {
    match tokio::time::timeout(timeout, &mut *handle).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) => false,
        Err(_) => {
            handle.abort();
            let _ = handle.await;
            false
        }
    }
}

pub async fn exec_single_scenario(
    exe_path: &Option<PathBuf>,
    req: ScenarioRequest<'_>,
) -> ScenarioOutcome {
    let log_prefix = make_log_prefix(req.index, req.total);
    // Browser launch is infrastructure — if this fails the environment is
    // broken (e.g. Chrome not installed/launchable), so it must fail the run.
    let resolved = config::resolve_config(req.workspace_config, req.test_suite, req.scenario);
    let launch_options = LaunchOptions {
        debug: resolved.debug,
        ignore_ssl_errors: resolved.ignore_ssl_errors,
        viewport: req
            .viewport
            .map(|viewport| (viewport.width, viewport.height)),
        has_touch: super::test_scenario::scenario_uses_touch(
            req.scenario,
            req.all_scenarios,
            req.viewport.map(|viewport| viewport.label.as_str()),
        ),
    };
    let (mut browser, handle, user_data_dir) =
        match launch_browser(exe_path, &log_prefix, launch_options).await {
            Ok(triple) => triple,
            Err(e) => {
                return ScenarioOutcome::InfraFailed(anyhow!("Failed to launch browser: {:?}", e));
            }
        };

    // Everything from here on interacts with the page under test. Any error is
    // a page-level failure that must not fail the command.
    let scenario_result = exec_test_scenario(&browser, req).await;

    use crate::diagnostics::{record::*, runtime as diagnostics};
    diagnostics::result(Stage::CaptureArtifacts, &scenario_result, 0);
    let cleanup_started = std::time::Instant::now();
    diagnostics::event(
        Stage::CompletionCleanup,
        DiagnosticEvent::StageStarted,
        Severity::Debug,
        [],
    );
    let handler_alive_before_close = !handle.is_finished();
    let browser_closed =
        match tokio::time::timeout(std::time::Duration::from_secs(10), browser.close()).await {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        };

    let mut handle = handle;
    let handler_finished =
        wait_for_browser_handler(&mut handle, std::time::Duration::from_secs(5), &log_prefix).await;
    // If the handler task had already exited before `browser.close()` was sent and
    // `browser.close()` failed, the browser crashed or disconnected mid-scenario.
    let shutdown_confirmed = browser_closed || (handler_alive_before_close && handler_finished);

    if !browser_closed {
        let _ = browser.kill().await;
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), browser.wait()).await;
    drop(browser);
    let _ = std::fs::remove_dir_all(&user_data_dir);

    let cleanup = if shutdown_confirmed {
        Ok(())
    } else {
        Err(anyhow!("Browser shutdown was not confirmed"))
    };
    diagnostics::result(
        Stage::CompletionCleanup,
        &cleanup,
        cleanup_started.elapsed().as_millis() as u64,
    );
    classify_scenario_completion(scenario_result, shutdown_confirmed)
}

fn classify_scenario_result(result: Result<()>) -> ScenarioOutcome {
    match result {
        Ok(()) => ScenarioOutcome::Passed,
        Err(error) if is_artifact_write_error(&error) => ScenarioOutcome::InfraFailed(error),
        Err(error) => ScenarioOutcome::PageFailed(error),
    }
}

fn classify_scenario_completion(
    scenario_result: Result<()>,
    shutdown_confirmed: bool,
) -> ScenarioOutcome {
    if shutdown_confirmed {
        return classify_scenario_result(scenario_result);
    }

    let error = match scenario_result {
        Ok(()) => anyhow!("Browser shutdown was not confirmed after a successful scenario"),
        Err(scenario_error) => anyhow!(
            "Browser shutdown was not confirmed; the scenario also failed: {}",
            scenario_error
        ),
    };
    ScenarioOutcome::InfraFailed(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TestSuiteFormat;

    #[test]
    fn parse_test_suite_uses_discovered_json_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("alloy.tests.json");
        std::fs::write(
            &path,
            r#"{"scenarios":[{"url":"https://json.example.com"}]}"#,
        )
        .unwrap();

        let suite = parse_test_suite(
            temp_dir.path(),
            &TestSuiteInput {
                name: "alloy".to_string(),
                path,
                format: TestSuiteFormat::Json,
            },
        )
        .unwrap();

        assert_eq!(suite.id.as_deref(), Some("alloy"));
        assert_eq!(suite.scenarios[0].url, "https://json.example.com");
    }

    #[test]
    fn parse_test_suite_uses_discovered_yml_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("alloy.tests.yml");
        std::fs::write(&path, "scenarios:\n  - url: https://yml.example.com\n").unwrap();

        let suite = parse_test_suite(
            temp_dir.path(),
            &TestSuiteInput {
                name: "alloy".to_string(),
                path,
                format: TestSuiteFormat::Yaml,
            },
        )
        .unwrap();

        assert_eq!(suite.id.as_deref(), Some("alloy"));
        assert_eq!(suite.scenarios[0].url, "https://yml.example.com");
    }

    #[test]
    fn test_run_ids_are_unique_and_sortable_within_one_second() {
        let first = chrono::DateTime::from_timestamp(1_700_000_000, 123).unwrap();
        let second = chrono::DateTime::from_timestamp(1_700_000_000, 456).unwrap();
        let first_id = test_run_id(first);
        let second_id = test_run_id(second);

        assert_eq!(first_id, "20231114_221320_000000123");
        assert_eq!(second_id, "20231114_221320_000000456");
        assert!(first_id < second_id);
    }

    #[test]
    fn explicit_run_id_overrides_generated_timestamp() {
        let time = chrono::DateTime::from_timestamp(1_700_000_000, 123).unwrap();

        assert_eq!(resolve_test_run_id(Some("ci-123"), time), "ci-123");
        assert_eq!(resolve_test_run_id(None, time), "20231114_221320_000000123");
    }

    #[test]
    fn parse_test_suite_duplicate_ids_identify_unlabeled_url_collisions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("suite.tests.yaml");
        std::fs::write(
            &path,
            "scenarios:\n  - url: https://example.com/shared\n  - url: https://example.com/other\n  - url: https://example.com/shared\n",
        )
        .unwrap();

        let error = parse_test_suite(
            temp_dir.path(),
            &TestSuiteInput {
                name: "suite".to_string(),
                path,
                format: TestSuiteFormat::Yaml,
            },
        )
        .err()
        .expect("Repeated unlabeled URLs must collide")
        .to_string();

        for expected in [
            "Duplicate scenario ID",
            "test suite 'suite'",
            "suite.tests.yaml",
            "Scenario #1",
            "Scenario #3",
            "https://example.com/shared",
            "unique explicit 'id'",
        ] {
            assert!(error.contains(expected), "Missing {expected:?}: {error}");
        }
        assert!(error.contains(&get_asset_version(b"https://example.com/shared", 20)));
        assert!(!error.contains("Scenario #2"), "{error}");
    }

    #[test]
    fn parse_test_suite_rejects_case_insensitive_scenario_id_collisions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("suite.tests.yaml");
        std::fs::write(
            &path,
            "scenarios:\n  - id: Home\n    label: First visit\n    url: https://example.com/home\n  - id: home\n    label: Second visit\n    url: https://example.com/other\n",
        )
        .unwrap();

        let error = parse_test_suite(
            temp_dir.path(),
            &TestSuiteInput {
                name: "suite".to_string(),
                path,
                format: TestSuiteFormat::Yaml,
            },
        )
        .err()
        .expect("Case-insensitive IDs must collide")
        .to_string();

        for expected in [
            "Duplicate scenario ID",
            "'home'",
            "Scenario #1 (label: \"First visit\"): URL \"https://example.com/home\"",
            "Scenario #2 (label: \"Second visit\"): URL \"https://example.com/other\"",
            "case-insensitive",
        ] {
            assert!(error.contains(expected), "Missing {expected:?}: {error}");
        }
    }

    #[test]
    fn parse_test_suite_duplicate_ids_identify_label_slug_collisions_in_json() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("suite.tests.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"scenarios": [
                {"label": "Shared page", "url": "https://example.com/first"},
                {"label": "Shared-page", "url": "https://example.com/second"}
            ]}))
            .unwrap(),
        )
        .unwrap();

        let error = parse_test_suite(
            temp_dir.path(),
            &TestSuiteInput {
                name: "suite".into(),
                path,
                format: TestSuiteFormat::Json,
            },
        )
        .err()
        .expect("Matching label slugs must collide")
        .to_string();

        for expected in [
            "suite.tests.json",
            "'shared-page'",
            "Scenario #1 (label: \"Shared page\"): URL \"https://example.com/first\"",
            "Scenario #2 (label: \"Shared-page\"): URL \"https://example.com/second\"",
            "unique explicit 'id'",
        ] {
            assert!(error.contains(expected), "Missing {expected:?}: {error}");
        }
    }

    #[test]
    fn parse_test_suite_allows_shared_urls_with_distinct_ids_or_labels() {
        let temp_dir = tempfile::tempdir().unwrap();
        let input = TestSuiteInput {
            name: "suite".into(),
            path: temp_dir.path().join("suite.tests.json"),
            format: TestSuiteFormat::Json,
        };
        for scenarios in [
            serde_json::json!([
                {"id": "first", "label": "Same label", "url": "https://example.com/shared"},
                {"id": "second", "label": "Same label", "url": "https://example.com/shared"}
            ]),
            serde_json::json!([
                {"label": "First visit", "url": "https://example.com/shared"},
                {"label": "Second visit", "url": "https://example.com/shared"}
            ]),
        ] {
            std::fs::write(
                &input.path,
                serde_json::to_vec(&serde_json::json!({"scenarios": scenarios})).unwrap(),
            )
            .unwrap();

            let suite = parse_test_suite(temp_dir.path(), &input).unwrap();
            assert_eq!(suite.scenarios.len(), 2);
            assert_ne!(suite.scenarios[0].id, suite.scenarios[1].id);
        }
    }

    #[test]
    fn parse_test_suite_duplicate_ids_redact_only_url_passwords() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("suite.tests.json");
        let url = "https://synthetic-user:synthetic-password@example.com/shared?access_token=synthetic-query-value#synthetic-fragment-value";
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"scenarios": [
                {"url": url}, {"url": url}
            ]}))
            .unwrap(),
        )
        .unwrap();

        let error = parse_test_suite(
            temp_dir.path(),
            &TestSuiteInput {
                name: "suite".into(),
                path,
                format: TestSuiteFormat::Json,
            },
        )
        .err()
        .expect("Repeated URLs must collide")
        .to_string();

        assert!(error.contains("https://synthetic-user:***@example.com/shared?access_token=synthetic-query-value#synthetic-fragment-value"));
        assert!(!error.contains("synthetic-password"));
    }

    #[test]
    fn scenario_diagnostic_urls_preserve_urls_without_passwords() {
        for url in [
            "https://example.com",
            "https://synthetic-user@example.com/shared?item=123#details",
            "file:///tmp/page.html",
            "not a valid URL",
            "data:text/plain,synthetic-content",
            "javascript:synthetic-content",
        ] {
            assert_eq!(scenario_diagnostic_url(url), url);
        }
    }

    #[test]
    fn scenario_diagnostic_urls_redact_only_passwords() {
        assert_eq!(
            scenario_diagnostic_url(
                "https://synthetic-user:synthetic%40password@example.com/shared?item=123#details"
            ),
            "https://synthetic-user:***@example.com/shared?item=123#details"
        );
        assert_eq!(
            scenario_diagnostic_url("https://:synthetic-password@example.com/shared"),
            "https://:***@example.com/shared"
        );
        assert_eq!(
            scenario_diagnostic_url("ftp://synthetic-user:synthetic-password@example.com/shared"),
            "ftp://synthetic-user:***@example.com/shared"
        );
    }

    #[test]
    fn parse_test_suite_rejects_unknown_suite_or_scenario_properties() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("suite.tests.yaml");
        std::fs::write(
            &path,
            "postinteractionwait: 5000\nscenarios:\n  - url: https://example.com\n    delayy: 10\n",
        )
        .unwrap();

        let result = parse_test_suite(
            temp_dir.path(),
            &TestSuiteInput {
                name: "suite".to_string(),
                path,
                format: TestSuiteFormat::Yaml,
            },
        );

        assert!(matches!(result, Err(error) if error.to_string().contains("unknown field")));
    }

    #[test]
    fn bundled_visual_suites_deserialize_with_the_strict_schema() {
        let visual_tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("visual_tests");
        for entry in std::fs::read_dir(&visual_tests_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let is_yaml = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension == "yaml" || extension == "yml");
            if !is_yaml {
                continue;
            }

            let result = parse_test_suite(
                &visual_tests_dir,
                &TestSuiteInput {
                    name: entry.file_name().to_string_lossy().to_string(),
                    path,
                    format: TestSuiteFormat::Yaml,
                },
            );
            if let Err(error) = result {
                panic!("Bundled visual suite failed strict parsing: {}", error);
            }
        }
    }

    #[tokio::test]
    async fn timed_out_handler_is_aborted_before_returning() {
        let mut handle = tokio::spawn(std::future::pending::<()>());

        let finished =
            wait_for_browser_handler(&mut handle, std::time::Duration::from_millis(1), "[test]")
                .await;

        assert!(!finished);
        assert!(handle.is_finished());
    }

    #[test]
    fn artifact_write_failure_is_infrastructure_failure() {
        let error = crate::test::capture::artifact_write_error("write screenshot", "disk full");

        assert!(matches!(
            classify_scenario_result(Err(error)),
            ScenarioOutcome::InfraFailed(_)
        ));
        assert!(matches!(
            classify_scenario_result(Err(anyhow!("page failed"))),
            ScenarioOutcome::PageFailed(_)
        ));
    }

    #[test]
    fn unconfirmed_browser_shutdown_is_an_infrastructure_failure() {
        assert!(matches!(
            classify_scenario_completion(Ok(()), false),
            ScenarioOutcome::InfraFailed(_)
        ));
        assert!(matches!(
            classify_scenario_completion(Err(anyhow!("page failed")), false),
            ScenarioOutcome::InfraFailed(_)
        ));
        assert!(matches!(
            classify_scenario_completion(Ok(()), true),
            ScenarioOutcome::Passed
        ));
    }
}
