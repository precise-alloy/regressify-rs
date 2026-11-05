#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::UrlReplacement;

/// Execution mode determining how URL replacement rules and profile scripts run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplacementMode {
    /// Baseline capture (`regressify ref`): rules with `find` replace `find` -> `ref`;
    /// rules without `find` leave URLs unchanged.
    Ref,
    /// Test capture (`regressify test`): rules with `find` replace `find` -> `test`;
    /// rules without `find` replace `ref` -> `test`.
    #[default]
    Test,
}

impl ReplacementMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ref => "ref",
            Self::Test => "test",
        }
    }
}

/// A script transformation rule in a replacement profile.
///
/// The script must live inside the workspace's `common` directory. Regressify
/// passes the test suite as a JSON object on `stdin` and reads the transformed
/// test suite JSON object from `stdout`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScriptReplacement {
    /// Path to a script in the `common` folder (for example `common/transform.js` or `transform.ps1`).
    /// Supported extensions: `.ps1` (PowerShell), `.bat`/`.cmd` (Batch), `.sh`/`.bash` (Bash),
    /// and `.js`/`.mjs`/`.cjs`/`.ts`/`.mts` (Bun or Node.js).
    #[schemars(
        length(min = 1),
        regex(pattern = r"\S"),
        example = "common/transform.js"
    )]
    pub script: String,
}

/// One step in a replacement profile: either a URL replacement rule or a script transformation.
/// Each item in a profile must be exactly one of these two variants.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ReplacementProfileItem {
    Url(UrlReplacement),
    Script(ScriptReplacement),
}

/// Supported external script runtimes detected from the script's file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptKind {
    PowerShell,
    Batch,
    Bash,
    JavaScript,
}

impl ScriptKind {
    /// Detect the script runtime from a file path's extension (case-insensitive).
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "ps1" => Some(Self::PowerShell),
            "bat" | "cmd" => Some(Self::Batch),
            "sh" | "bash" => Some(Self::Bash),
            "js" | "mjs" | "cjs" | "ts" | "mts" => Some(Self::JavaScript),
            _ => None,
        }
    }
}

/// A file containing named replacement profiles.
/// YAML structure:
/// ```yaml
/// profiles:
///   default:
///     - find: "{HOST}"
///       ref: "https://prod.example.com"
///       test: "https://staging.example.com"
///   staging:
///     - ref: "https://prod.example.com"
///       test: "https://staging.example.com"
///       regex: true
///       flags: "i"
///     - script: "common/transform.js"
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplacementProfilesFile {
    /// Named ordered replacement profile steps. The default profile is used when no profile is selected.
    pub profiles: HashMap<String, Vec<ReplacementProfileItem>>,
}

/// Load replacement profiles from a YAML file.
/// Looks for `common/_replacement-profiles.yaml` or `common/_replacement-profiles.yml`
/// relative to the workspace root.
pub fn load_replacement_profiles(root_dir: &Path) -> Result<Option<ReplacementProfilesFile>> {
    let config_path = super::resolve_optional_workspace_file(
        root_dir,
        "common/_replacement-profiles.yaml",
        "replacement profile file",
    )?
    .or(super::resolve_optional_workspace_file(
        root_dir,
        "common/_replacement-profiles.yml",
        "replacement profile file",
    )?);

    match config_path {
        Some(path) => {
            let content = std::fs::read_to_string(&path)?;
            let profiles: ReplacementProfilesFile = serde_yaml::from_str(&content)?;
            Ok(Some(profiles))
        }
        None => Ok(None),
    }
}

/// Determine the active replacement profile name and whether it was explicitly configured.
/// Priority: CLI `--replacement-profile` -> `REPLACEMENT_PROFILE` env var -> suite `replacementProfile` -> `"default"`.
pub fn resolve_profile_selection(
    cli_arg: Option<&str>,
    suite_profile: Option<&str>,
) -> (String, bool) {
    if let Some(name) = cli_arg {
        return (name.to_string(), true);
    }
    if let Ok(env_val) = std::env::var("REPLACEMENT_PROFILE")
        && !env_val.is_empty()
    {
        return (env_val, true);
    }
    if let Some(suite_name) = suite_profile {
        return (suite_name.to_string(), true);
    }
    ("default".to_string(), false)
}

/// Determine the active replacement profile name without suite context.
/// Priority: CLI arg -> REPLACEMENT_PROFILE env var -> "default"
pub fn get_profile_name(cli_arg: Option<&str>) -> String {
    resolve_profile_selection(cli_arg, None).0
}

/// Bind URL replacement rules for a specific execution mode (`Ref` or `Test`).
///
/// In `Ref` mode, only rules with `find` are active, replacing `find` -> `ref`.
/// In `Test` mode, rules with `find` replace `find` -> `test`, and rules without
/// `find` replace `ref` -> `test`.
pub fn bind_replacements_for_mode(
    replacements: &[UrlReplacement],
    mode: ReplacementMode,
) -> Vec<UrlReplacement> {
    replacements
        .iter()
        .filter_map(|rule| match mode {
            ReplacementMode::Ref => rule.find.as_ref().map(|find| UrlReplacement {
                find: None,
                ref_pattern: find.clone(),
                test: rule.ref_pattern.clone(),
                regex: rule.regex,
                flags: rule.flags.clone(),
            }),
            ReplacementMode::Test => Some(UrlReplacement {
                find: None,
                ref_pattern: rule.pattern().to_string(),
                test: rule.test.clone(),
                regex: rule.regex,
                flags: rule.flags.clone(),
            }),
        })
        .collect()
}

/// Apply a list of URL replacements to a URL string in `Test` mode.
/// Supports both literal and regex replacements.
pub fn apply_replacements(url: &str, replacements: &[UrlReplacement]) -> Result<String> {
    apply_replacements_for_mode(url, replacements, ReplacementMode::Test)
}

/// Apply a list of URL replacements to a URL string for the specified execution mode.
///
/// When `find` is set on a rule, `Ref` mode replaces `find` with `ref` and `Test`
/// mode replaces `find` with `test`. When `find` is omitted, `Ref` mode leaves the
/// URL unchanged and `Test` mode replaces `ref` with `test`.
pub fn apply_replacements_for_mode(
    url: &str,
    replacements: &[UrlReplacement],
    mode: ReplacementMode,
) -> Result<String> {
    let mut result = url.to_string();
    for replacement in replacements {
        let pattern = replacement.pattern();
        if !replacement.regex.unwrap_or(false)
            && replacement
                .flags
                .as_deref()
                .is_some_and(|flags| !flags.is_empty())
        {
            anyhow::bail!(
                "URL replacement '{}' specifies flags but does not enable regex",
                pattern
            );
        }
        let target = match (&replacement.find, mode) {
            (Some(_), ReplacementMode::Ref) => replacement.ref_pattern.as_str(),
            (Some(_), ReplacementMode::Test) | (None, ReplacementMode::Test) => {
                replacement.test.as_str()
            }
            (None, ReplacementMode::Ref) => {
                if replacement.regex.unwrap_or(false) {
                    compile_replacement_regex(replacement)?;
                }
                continue;
            }
        };
        if replacement.regex.unwrap_or(false) {
            let regex = compile_replacement_regex(replacement)?;
            result = regex.replace_all(&result, target).to_string();
        } else {
            result = result.replace(pattern, target);
        }
    }
    Ok(result)
}

fn compile_replacement_regex(replacement: &UrlReplacement) -> Result<regex::Regex> {
    let raw_pattern = replacement.pattern();
    let mut modifiers = String::new();
    let mut seen = HashSet::new();
    for flag in replacement.flags.as_deref().unwrap_or("").chars() {
        if !seen.insert(flag) {
            anyhow::bail!(
                "Regex URL replacement '{}' repeats flag '{}'",
                raw_pattern,
                flag
            );
        }
        match flag {
            // replace_all is global regardless of an explicit JS `g` flag;
            // Rust regex is Unicode-aware by default, matching JS `u` URLs.
            'g' | 'u' => {}
            'i' | 'm' | 's' => modifiers.push(flag),
            unsupported => anyhow::bail!(
                "Regex URL replacement '{}' has unsupported flag '{}' (supported: g, i, m, s, u)",
                raw_pattern,
                unsupported
            ),
        }
    }
    let pattern = if modifiers.is_empty() {
        raw_pattern.to_string()
    } else {
        format!("(?{}){}", modifiers, raw_pattern)
    };
    regex::Regex::new(&pattern)
        .map_err(|error| anyhow!("Invalid regex URL replacement '{}': {}", raw_pattern, error))
}

/// Validate URL replacement rules as configuration before browser scenarios start.
pub fn validate_replacements(replacements: &[UrlReplacement], source: &str) -> Result<()> {
    for replacement in replacements {
        if let Some(find) = &replacement.find {
            if find.is_empty() {
                return Err(anyhow!(
                    "URL replacement in {} has an empty `find` pattern",
                    source
                ));
            }
        } else if replacement.ref_pattern.is_empty() {
            return Err(anyhow!(
                "URL replacement in {} has an empty `ref` pattern",
                source
            ));
        }
        let pattern = replacement.pattern();
        if replacement.regex.unwrap_or(false) {
            compile_replacement_regex(replacement).map_err(|error| {
                anyhow!(
                    "Invalid regex URL replacement '{}' in {}: {}",
                    pattern,
                    source,
                    error
                )
            })?;
        } else if replacement
            .flags
            .as_deref()
            .is_some_and(|flags| !flags.is_empty())
        {
            anyhow::bail!(
                "URL replacement '{}' in {} specifies flags but does not enable regex",
                pattern,
                source
            );
        }
    }
    Ok(())
}

/// Validate profile item syntax (URL replacement rules and script path/extension format)
/// without requiring the workspace filesystem to exist on disk.
pub fn validate_profile_items_syntax(items: &[ReplacementProfileItem], source: &str) -> Result<()> {
    for item in items {
        match item {
            ReplacementProfileItem::Url(replacement) => {
                validate_replacements(std::slice::from_ref(replacement), source)?;
            }
            ReplacementProfileItem::Script(script) => {
                validate_script_syntax(script, source)?;
            }
        }
    }
    Ok(())
}

/// Validate profile items including resolving script files inside `<root_dir>/common`.
pub fn validate_profile_items(
    root_dir: &Path,
    items: &[ReplacementProfileItem],
    source: &str,
) -> Result<()> {
    for item in items {
        match item {
            ReplacementProfileItem::Url(replacement) => {
                validate_replacements(std::slice::from_ref(replacement), source)?;
            }
            ReplacementProfileItem::Script(script) => {
                resolve_profile_script_path(root_dir, &script.script, source)?;
            }
        }
    }
    Ok(())
}

fn validate_script_syntax(script: &ScriptReplacement, source: &str) -> Result<ScriptKind> {
    let trimmed = script.script.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "Replacement script in {} has an empty `script` path",
            source
        );
    }
    let path = Path::new(trimmed);
    ScriptKind::from_path(path).ok_or_else(|| {
        anyhow!(
            "Replacement script '{}' in {} has an unsupported extension (supported: .ps1, .bat, .cmd, .sh, .bash, .js, .mjs, .cjs, .ts, .mts)",
            script.script,
            source
        )
    })
}

/// Resolve a replacement profile script path and ensure it is an existing regular
/// file inside `<root_dir>/common`, including after symlink resolution.
pub fn resolve_profile_script_path(
    root_dir: &Path,
    configured_path: &str,
    source: &str,
) -> Result<(PathBuf, ScriptKind)> {
    let kind = validate_script_syntax(
        &ScriptReplacement {
            script: configured_path.to_string(),
        },
        source,
    )?;
    let common_dir = root_dir.join("common");
    let path = Path::new(configured_path);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else if path.components().next() == Some(Component::Normal(std::ffi::OsStr::new("common"))) {
        let from_root = root_dir.join(path);
        let from_common = common_dir.join(path);
        if from_root.exists() || !from_common.exists() {
            from_root
        } else {
            from_common
        }
    } else {
        common_dir.join(path)
    };

    if !common_dir.exists() || !candidate.exists() {
        anyhow::bail!(
            "Configured replacement script '{}' in {} does not exist",
            configured_path,
            source
        );
    }

    let canonical_root = root_dir
        .canonicalize()
        .with_context(|| format!("Failed to resolve workspace root '{}'", root_dir.display()))?;
    let canonical_common = common_dir.canonicalize().with_context(|| {
        format!(
            "Failed to resolve common directory '{}'",
            common_dir.display()
        )
    })?;
    if !canonical_common.starts_with(&canonical_root) {
        anyhow::bail!(
            "Configured replacement script '{}' in {} escapes workspace root '{}'",
            configured_path,
            source,
            root_dir.display()
        );
    }
    let canonical_path = candidate.canonicalize().with_context(|| {
        format!(
            "Failed to resolve configured replacement script '{}' in {}",
            configured_path, source
        )
    })?;
    if !canonical_path.starts_with(&canonical_common) {
        anyhow::bail!(
            "Configured replacement script '{}' in {} escapes common directory '{}'",
            configured_path,
            source,
            common_dir.display()
        );
    }
    if !canonical_path.is_file() {
        anyhow::bail!(
            "Configured replacement script '{}' in {} is not a regular file",
            configured_path,
            source
        );
    }

    // Strip the Windows verbatim `\\?\` prefix added by `canonicalize()` so
    // `cmd.exe` and other external shells can execute the resolved script path.
    #[cfg(windows)]
    let canonical_path = canonical_path
        .to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .map(PathBuf::from)
        .unwrap_or(canonical_path);

    Ok((canonical_path, kind))
}

/// Apply a single URL replacement rule to all URL-bearing fields in a raw test suite JSON value
/// (`scenarios[*].url`, `scenarios[*].actions[*].goto`, and `url` on `wait`/`switchTab` actions).
pub fn apply_url_replacement_to_suite_value(
    suite_value: &mut Value,
    replacement: &UrlReplacement,
    mode: ReplacementMode,
) -> Result<()> {
    let rules = std::slice::from_ref(replacement);
    let Some(scenarios) = suite_value
        .get_mut("scenarios")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for scenario in scenarios {
        if let Some(url_val) = scenario.get_mut("url")
            && let Some(url) = url_val.as_str()
        {
            *url_val = Value::String(apply_replacements_for_mode(url, rules, mode)?);
        }
        if let Some(actions) = scenario.get_mut("actions").and_then(Value::as_array_mut) {
            for action in actions {
                if let Some(goto_val) = action.get_mut("goto")
                    && let Some(goto) = goto_val.as_str()
                {
                    *goto_val = Value::String(apply_replacements_for_mode(goto, rules, mode)?);
                }
                if (action.get("wait").is_some() || action.get("switchTab").is_some())
                    && let Some(url_val) = action.get_mut("url")
                    && let Some(url) = url_val.as_str()
                {
                    *url_val = Value::String(apply_replacements_for_mode(url, rules, mode)?);
                }
            }
        }
    }
    Ok(())
}

/// Execute a replacement profile script, passing the test suite JSON object via `stdin`
/// and reading the transformed test suite JSON object from `stdout`.
pub fn run_profile_script(
    root_dir: &Path,
    script: &ScriptReplacement,
    suite_value: &Value,
    mode: ReplacementMode,
    profile_name: &str,
    suite_name: &str,
) -> Result<Value> {
    let source = format!("replacement profile '{}'", profile_name);
    let (script_path, kind) = resolve_profile_script_path(root_dir, &script.script, &source)?;
    let input_bytes = serde_json::to_vec(suite_value).with_context(|| {
        format!(
            "Failed to serialize test suite '{}' to JSON for script '{}'",
            suite_name, script.script
        )
    })?;

    let mut child = spawn_script_process(
        kind,
        &script_path,
        &script.script,
        root_dir,
        mode,
        profile_name,
        suite_name,
    )?;

    // Write stdin on a helper thread so large suite payloads cannot deadlock
    // if the child writes extensively to stdout/stderr before finishing stdin.
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input_bytes);
            let _ = stdin.flush();
        });
    }

    let output = child.wait_with_output().with_context(|| {
        format!(
            "Failed waiting for replacement script '{}' in profile '{}'",
            script.script, profile_name
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "no error output".to_string()
        };
        anyhow::bail!(
            "Replacement script '{}' in profile '{}' failed ({}): {}",
            script.script,
            profile_name,
            output.status,
            detail
        );
    }

    let transformed: Value = serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "Replacement script '{}' in profile '{}' did not output valid JSON on stdout",
            script.script, profile_name
        )
    })?;
    if !transformed.is_object() {
        anyhow::bail!(
            "Replacement script '{}' in profile '{}' must return a JSON object on stdout",
            script.script,
            profile_name
        );
    }
    Ok(transformed)
}

fn spawn_script_process(
    kind: ScriptKind,
    script_path: &Path,
    display_script: &str,
    root_dir: &Path,
    mode: ReplacementMode,
    profile_name: &str,
    suite_name: &str,
) -> Result<std::process::Child> {
    let script_arg = script_path.as_os_str();
    let candidates: Vec<(&str, Vec<&std::ffi::OsStr>)> = match kind {
        ScriptKind::PowerShell => vec![
            (
                "pwsh",
                vec![
                    std::ffi::OsStr::new("-NoProfile"),
                    std::ffi::OsStr::new("-NonInteractive"),
                    std::ffi::OsStr::new("-ExecutionPolicy"),
                    std::ffi::OsStr::new("Bypass"),
                    std::ffi::OsStr::new("-File"),
                    script_arg,
                ],
            ),
            (
                "powershell",
                vec![
                    std::ffi::OsStr::new("-NoProfile"),
                    std::ffi::OsStr::new("-NonInteractive"),
                    std::ffi::OsStr::new("-ExecutionPolicy"),
                    std::ffi::OsStr::new("Bypass"),
                    std::ffi::OsStr::new("-File"),
                    script_arg,
                ],
            ),
        ],
        ScriptKind::Batch => vec![(
            "cmd",
            vec![
                std::ffi::OsStr::new("/d"),
                std::ffi::OsStr::new("/c"),
                script_arg,
            ],
        )],
        ScriptKind::Bash => vec![("bash", vec![script_arg])],
        // Prefer Bun for JS/TS files when installed, falling back to Node.js.
        ScriptKind::JavaScript => vec![("bun", vec![script_arg]), ("node", vec![script_arg])],
    };

    let mut last_error = None;
    for (program, args) in &candidates {
        match Command::new(program)
            .args(args)
            .current_dir(root_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("REGRESSIFY_MODE", mode.as_str())
            .env("REGRESSIFY_PROFILE", profile_name)
            .env("REGRESSIFY_SUITE", suite_name)
            .env("REGRESSIFY_ROOT", root_dir)
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_error = Some(error);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Failed to launch '{}' for replacement script '{}'",
                        program, display_script
                    )
                });
            }
        }
    }

    let tried = candidates
        .iter()
        .map(|(program, _)| *program)
        .collect::<Vec<_>>()
        .join(", ");
    Err(last_error.unwrap_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound)))
        .with_context(|| {
            format!(
                "Could not find runtime executable ({}) on PATH to run replacement script '{}'",
                tried, display_script
            )
        })
}

/// Process all items in a replacement profile sequentially from first item to last item.
///
/// Any URL replacement items that precede a script item are applied directly to the suite
/// JSON object before the script runs so the script receives the updated URLs. Any URL
/// replacement items that follow the last script item (or all items when the profile has
/// no scripts) are also returned as trailing `UrlReplacement` rules or applied directly
/// depending on the caller.
pub fn apply_profile_items_to_suite_value(
    root_dir: &Path,
    items: &[ReplacementProfileItem],
    suite_value: &mut Value,
    mode: ReplacementMode,
    profile_name: &str,
    suite_name: &str,
) -> Result<()> {
    for item in items {
        match item {
            ReplacementProfileItem::Url(replacement) => {
                apply_url_replacement_to_suite_value(suite_value, replacement, mode)?;
            }
            ReplacementProfileItem::Script(script) => {
                *suite_value = run_profile_script(
                    root_dir,
                    script,
                    suite_value,
                    mode,
                    profile_name,
                    suite_name,
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regex_replacement(pattern: &str, test: &str, flags: &str) -> UrlReplacement {
        UrlReplacement {
            find: None,
            ref_pattern: pattern.to_string(),
            test: test.to_string(),
            regex: Some(true),
            flags: Some(flags.to_string()),
        }
    }

    #[test]
    fn regex_replacements_honor_supported_flags() {
        let insensitive = regex_replacement("^https://example\\.com$", "ok", "i");
        assert_eq!(
            apply_replacements("HTTPS://EXAMPLE.COM", &[insensitive]).unwrap(),
            "ok"
        );

        let dot_all = regex_replacement("a.b", "ok", "s");
        assert_eq!(apply_replacements("a\nb", &[dot_all]).unwrap(), "ok");

        let multiline = regex_replacement("^b$", "ok", "m");
        assert_eq!(
            apply_replacements("a\nb\nc", &[multiline]).unwrap(),
            "a\nok\nc"
        );

        let global = regex_replacement("a", "b", "g");
        assert_eq!(apply_replacements("aaa", &[global]).unwrap(), "bbb");
    }

    #[test]
    fn replacement_validation_rejects_invalid_flag_configurations() {
        for replacement in [
            regex_replacement("a", "b", "ii"),
            regex_replacement("a", "b", "y"),
            UrlReplacement {
                find: None,
                ref_pattern: "a".to_string(),
                test: "b".to_string(),
                regex: Some(false),
                flags: Some("i".to_string()),
            },
        ] {
            assert!(
                validate_replacements(&[replacement], "test")
                    .unwrap_err()
                    .to_string()
                    .contains("flag")
            );
        }
    }

    #[test]
    fn replacement_profile_file_rejects_unknown_properties() {
        let result =
            serde_yaml::from_str::<ReplacementProfilesFile>("profiles: {}\nprofilez: {}\n");

        assert!(matches!(result, Err(error) if error.to_string().contains("unknown field")));
    }

    #[test]
    fn find_replacements_apply_ref_in_ref_mode_and_test_in_test_mode() {
        let rules = vec![
            UrlReplacement {
                find: Some("{HOST}".to_string()),
                ref_pattern: "https://prod.example.com".to_string(),
                test: "https://staging.example.com".to_string(),
                regex: None,
                flags: None,
            },
            UrlReplacement {
                find: None,
                ref_pattern: "/v1/".to_string(),
                test: "/v2/".to_string(),
                regex: None,
                flags: None,
            },
            UrlReplacement {
                find: Some("^http://([a-z0-9.-]+)".to_string()),
                ref_pattern: "https://${1}".to_string(),
                test: "https://qa-${1}".to_string(),
                regex: Some(true),
                flags: Some("i".to_string()),
            },
        ];

        validate_replacements(&rules, "test").unwrap();

        // In Ref mode: `find` -> `ref` applies, while rules without `find` leave the URL untouched.
        let ref_result =
            apply_replacements_for_mode("{HOST}/v1/items", &rules, ReplacementMode::Ref).unwrap();
        assert_eq!(ref_result, "https://prod.example.com/v1/items");

        let ref_regex =
            apply_replacements_for_mode("HTTP:// example.com/v1", &rules, ReplacementMode::Ref)
                .unwrap();
        assert_eq!(ref_regex, "HTTP:// example.com/v1");
        let ref_regex_match =
            apply_replacements_for_mode("HTTP://app.example.com/v1", &rules, ReplacementMode::Ref)
                .unwrap();
        assert_eq!(ref_regex_match, "https://app.example.com/v1");

        // In Test mode: both `find` -> `test` and `ref` -> `test` apply in order.
        let test_result =
            apply_replacements_for_mode("{HOST}/v1/items", &rules, ReplacementMode::Test).unwrap();
        assert_eq!(test_result, "https://staging.example.com/v2/items");

        let test_regex_match = apply_replacements_for_mode(
            "HTTP://app.example.com/v1/page",
            &rules,
            ReplacementMode::Test,
        )
        .unwrap();
        assert_eq!(test_regex_match, "https://qa-app.example.com/v2/page");

        // `bind_replacements_for_mode` produces equivalent results when downstream callers use `apply_replacements`.
        let bound_ref = bind_replacements_for_mode(&rules, ReplacementMode::Ref);
        assert_eq!(
            apply_replacements("{HOST}/v1/items", &bound_ref).unwrap(),
            "https://prod.example.com/v1/items"
        );
        let bound_test = bind_replacements_for_mode(&rules, ReplacementMode::Test);
        assert_eq!(
            apply_replacements("{HOST}/v1/items", &bound_test).unwrap(),
            "https://staging.example.com/v2/items"
        );
    }

    #[test]
    fn validate_replacements_checks_find_and_ref_patterns() {
        let empty_find = UrlReplacement {
            find: Some(String::new()),
            ref_pattern: "https://prod.example.com".to_string(),
            test: "https://staging.example.com".to_string(),
            regex: None,
            flags: None,
        };
        assert!(
            validate_replacements(&[empty_find], "profile")
                .unwrap_err()
                .to_string()
                .contains("empty `find` pattern")
        );

        // When `find` is non-empty, `ref` is a replacement target and may be empty.
        let empty_ref_target = UrlReplacement {
            find: Some("/legacy-prefix".to_string()),
            ref_pattern: String::new(),
            test: "/staging-prefix".to_string(),
            regex: None,
            flags: None,
        };
        validate_replacements(&[empty_ref_target], "profile").unwrap();

        let invalid_find_regex = UrlReplacement {
            find: Some("[unclosed".to_string()),
            ref_pattern: "a".to_string(),
            test: "b".to_string(),
            regex: Some(true),
            flags: None,
        };
        assert!(
            validate_replacements(&[invalid_find_regex], "profile")
                .unwrap_err()
                .to_string()
                .contains("Invalid regex URL replacement '[unclosed'")
        );
    }

    #[test]
    fn profile_items_enforce_mutually_exclusive_url_or_script_types() {
        let valid_yaml = r#"
profiles:
  staging:
    - find: "{HOST}"
      ref: "https://prod.example.com"
      test: "https://staging.example.com"
    - ref: "/old"
      test: "/new"
    - script: "common/transform.js"
"#;
        let parsed: ReplacementProfilesFile = serde_yaml::from_str(valid_yaml).unwrap();
        assert_eq!(parsed.profiles["staging"].len(), 3);
        validate_profile_items_syntax(&parsed.profiles["staging"], "staging").unwrap();

        // Mixing `script` and `ref`/`test` in the same item must fail deserialization.
        let mixed_yaml = r#"
profiles:
  staging:
    - script: "common/transform.js"
      ref: "https://prod.example.com"
      test: "https://staging.example.com"
"#;
        assert!(serde_yaml::from_str::<ReplacementProfilesFile>(mixed_yaml).is_err());

        // Empty item or unknown fields must fail deserialization.
        assert!(
            serde_yaml::from_str::<ReplacementProfilesFile>("profiles:\n  staging:\n    - {}\n")
                .is_err()
        );
    }

    #[test]
    fn resolve_profile_script_path_enforces_common_containment_and_extensions() {
        let workspace = tempfile::tempdir().unwrap();
        let common = workspace.path().join("common");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::write(common.join("transform.js"), "// js\n").unwrap();
        std::fs::write(common.join("setup.ps1"), "# ps1\n").unwrap();
        std::fs::write(common.join("run.bat"), "@echo off\n").unwrap();
        std::fs::write(common.join("run.sh"), "#!/bin/sh\n").unwrap();
        std::fs::write(common.join("notes.txt"), "text\n").unwrap();
        std::fs::write(workspace.path().join("outside.js"), "// outside\n").unwrap();

        for (path, expected_kind) in [
            ("common/transform.js", ScriptKind::JavaScript),
            ("transform.js", ScriptKind::JavaScript),
            ("common/setup.ps1", ScriptKind::PowerShell),
            ("run.bat", ScriptKind::Batch),
            ("run.sh", ScriptKind::Bash),
        ] {
            let (_, kind) = resolve_profile_script_path(workspace.path(), path, "test").unwrap();
            assert_eq!(kind, expected_kind, "{path}");
        }

        // Unsupported extension is rejected.
        assert!(
            resolve_profile_script_path(workspace.path(), "common/notes.txt", "test")
                .unwrap_err()
                .to_string()
                .contains("unsupported extension")
        );

        // Escaping `common` into workspace root is rejected.
        assert!(
            resolve_profile_script_path(workspace.path(), "../outside.js", "test")
                .unwrap_err()
                .to_string()
                .contains("escapes common directory")
        );
        assert!(
            resolve_profile_script_path(workspace.path(), "common/../outside.js", "test")
                .unwrap_err()
                .to_string()
                .contains("escapes common directory")
        );

        // Missing file is rejected.
        assert!(
            resolve_profile_script_path(workspace.path(), "common/missing.js", "test")
                .unwrap_err()
                .to_string()
                .contains("does not exist")
        );
    }

    #[test]
    fn profile_items_process_url_replacements_and_scripts_in_order() {
        let workspace = tempfile::tempdir().unwrap();
        let common = workspace.path().join("common");
        std::fs::create_dir_all(&common).unwrap();

        // Create a Node/Bun JS script that reads the suite JSON from stdin,
        // records the URL it saw after step 1, replaces a custom placeholder,
        // and appends a path suffix that step 3 will further transform.
        let js_script = r##"
const fs = require("fs");
const input = JSON.parse(fs.readFileSync(0, "utf8"));
const mode = process.env.REGRESSIFY_MODE;
const profile = process.env.REGRESSIFY_PROFILE;
const suite = process.env.REGRESSIFY_SUITE;
for (const scenario of input.scenarios || []) {
  scenario.description = `seen=${scenario.url};mode=${mode};profile=${profile};suite=${suite}`;
  scenario.url = scenario.url + "/from-script";
  if (scenario.actions) {
    for (const action of scenario.actions) {
      if (action.input === "#user" && action.value === "{USER}") {
        action.value = mode === "ref" ? "ref-user" : "test-user";
      }
    }
  }
}
process.stdout.write(JSON.stringify(input));
"##;
        std::fs::write(common.join("transform.js"), js_script).unwrap();

        let items = vec![
            ReplacementProfileItem::Url(UrlReplacement {
                find: Some("{HOST}".to_string()),
                ref_pattern: "https://prod.example.com".to_string(),
                test: "https://staging.example.com".to_string(),
                regex: None,
                flags: None,
            }),
            ReplacementProfileItem::Script(ScriptReplacement {
                script: "common/transform.js".to_string(),
            }),
            ReplacementProfileItem::Url(UrlReplacement {
                find: Some("/from-script".to_string()),
                ref_pattern: "/final-ref".to_string(),
                test: "/final-test".to_string(),
                regex: None,
                flags: None,
            }),
        ];

        let initial_suite = serde_json::json!({
            "scenarios": [{
                "id": "login",
                "url": "{HOST}/login",
                "actions": [
                    {"goto": "{HOST}/step"},
                    {"wait": "#ready", "url": "{HOST}/ready"},
                    {"switchTab": "new", "url": "{HOST}/popup"},
                    {"input": "#user", "value": "{USER}"}
                ]
            }]
        });

        let mut ref_suite = initial_suite.clone();
        apply_profile_items_to_suite_value(
            workspace.path(),
            &items,
            &mut ref_suite,
            ReplacementMode::Ref,
            "custom",
            "auth",
        )
        .unwrap();
        assert_eq!(
            ref_suite["scenarios"][0]["description"],
            "seen=https://prod.example.com/login;mode=ref;profile=custom;suite=auth"
        );
        assert_eq!(
            ref_suite["scenarios"][0]["url"],
            "https://prod.example.com/login/final-ref"
        );
        assert_eq!(
            ref_suite["scenarios"][0]["actions"][0]["goto"],
            "https://prod.example.com/step"
        );
        assert_eq!(
            ref_suite["scenarios"][0]["actions"][1]["url"],
            "https://prod.example.com/ready"
        );
        assert_eq!(
            ref_suite["scenarios"][0]["actions"][2]["url"],
            "https://prod.example.com/popup"
        );
        assert_eq!(ref_suite["scenarios"][0]["actions"][3]["value"], "ref-user");

        let mut test_suite = initial_suite;
        apply_profile_items_to_suite_value(
            workspace.path(),
            &items,
            &mut test_suite,
            ReplacementMode::Test,
            "custom",
            "auth",
        )
        .unwrap();
        assert_eq!(
            test_suite["scenarios"][0]["description"],
            "seen=https://staging.example.com/login;mode=test;profile=custom;suite=auth"
        );
        assert_eq!(
            test_suite["scenarios"][0]["url"],
            "https://staging.example.com/login/final-test"
        );
        assert_eq!(
            test_suite["scenarios"][0]["actions"][3]["value"],
            "test-user"
        );
    }

    #[cfg(windows)]
    #[test]
    fn powershell_and_batch_profile_scripts_transform_suite_json() {
        let workspace = tempfile::tempdir().unwrap();
        let common = workspace.path().join("common");
        std::fs::create_dir_all(&common).unwrap();

        let ps1_script = r#"
$raw = [Console]::In.ReadToEnd()
$obj = $raw | ConvertFrom-Json
$obj.scenarios[0].url = $obj.scenarios[0].url.Replace("{PS1}", "powershell-ok")
$obj | ConvertTo-Json -Depth 10 -Compress
"#;
        std::fs::write(common.join("step.ps1"), ps1_script).unwrap();

        let bat_script = "@echo off\r\nfindstr \"^\" \r\n";
        std::fs::write(common.join("passthrough.bat"), bat_script).unwrap();

        let items = vec![
            ReplacementProfileItem::Script(ScriptReplacement {
                script: "step.ps1".to_string(),
            }),
            ReplacementProfileItem::Script(ScriptReplacement {
                script: "common/passthrough.bat".to_string(),
            }),
        ];

        let mut suite = serde_json::json!({
            "scenarios": [{"id": "home", "url": "https://example.com/{PS1}"}]
        });
        apply_profile_items_to_suite_value(
            workspace.path(),
            &items,
            &mut suite,
            ReplacementMode::Test,
            "win",
            "basic",
        )
        .unwrap();
        assert_eq!(
            suite["scenarios"][0]["url"],
            "https://example.com/powershell-ok"
        );
    }

    #[test]
    fn script_failures_and_non_object_outputs_report_clear_errors() {
        let workspace = tempfile::tempdir().unwrap();
        let common = workspace.path().join("common");
        std::fs::create_dir_all(&common).unwrap();

        std::fs::write(
            common.join("fail.js"),
            "console.error('custom script failure'); process.exit(2);\n",
        )
        .unwrap();
        std::fs::write(
            common.join("bad-json.js"),
            "process.stdout.write('not-json');\n",
        )
        .unwrap();
        std::fs::write(
            common.join("array-json.js"),
            "process.stdout.write('[1, 2, 3]');\n",
        )
        .unwrap();

        let mut suite =
            serde_json::json!({"scenarios": [{"id": "home", "url": "https://example.com"}]});

        let err = apply_profile_items_to_suite_value(
            workspace.path(),
            &[ReplacementProfileItem::Script(ScriptReplacement {
                script: "common/fail.js".to_string(),
            })],
            &mut suite,
            ReplacementMode::Test,
            "broken",
            "basic",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("custom script failure"),
            "{err:#}"
        );

        let err = apply_profile_items_to_suite_value(
            workspace.path(),
            &[ReplacementProfileItem::Script(ScriptReplacement {
                script: "common/bad-json.js".to_string(),
            })],
            &mut suite,
            ReplacementMode::Test,
            "broken",
            "basic",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("did not output valid JSON"),
            "{err:#}"
        );

        let err = apply_profile_items_to_suite_value(
            workspace.path(),
            &[ReplacementProfileItem::Script(ScriptReplacement {
                script: "common/array-json.js".to_string(),
            })],
            &mut suite,
            ReplacementMode::Test,
            "broken",
            "basic",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("must return a JSON object"),
            "{err:#}"
        );
    }

    #[test]
    fn resolve_profile_selection_prefers_cli_over_suite_over_default() {
        assert_eq!(
            resolve_profile_selection(Some("cli-prof"), Some("suite-prof")),
            ("cli-prof".to_string(), true)
        );
        assert_eq!(
            resolve_profile_selection(None, Some("suite-prof")),
            ("suite-prof".to_string(), true)
        );
        assert_eq!(
            resolve_profile_selection(None, None),
            ("default".to_string(), false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacement_profile_file_cannot_escape_workspace_through_symlink() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let common = workspace.path().join("common");
        let outside_profile = outside.path().join("profiles.yaml");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::write(&outside_profile, "profiles: {}\n").unwrap();
        std::os::unix::fs::symlink(&outside_profile, common.join("_replacement-profiles.yaml"))
            .unwrap();

        let error = match load_replacement_profiles(workspace.path()) {
            Ok(_) => panic!("replacement profile symlink unexpectedly escaped the workspace"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("escapes workspace root"));
    }
}
