use std::{cmp::Reverse, sync::LazyLock};

use anyhow::{Result, anyhow};
use jsonschema::{ValidationError, Validator, error::ValidationErrorKind};
use serde_json::Value;

use super::{
    document::{ACTIONS, DocumentKind},
    schema::{self, resolve},
};
use crate::{config::WorkspaceConfig, config::viewports::ViewportDef, types::TestSuite};

pub(crate) const MAX_DIAGNOSTICS: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub pointer: String,
    pub message: String,
}

struct DocumentSchema {
    root: Value,
    validator: Validator,
}

impl DocumentSchema {
    fn new(schema: schemars::Schema) -> Result<Self, String> {
        let root = Value::from(schema);
        let validator = jsonschema::options()
            .offline()
            .build(&root)
            .map_err(|error| error.to_string())?;
        Ok(Self { root, validator })
    }
}

fn document_schema(kind: DocumentKind) -> Result<Option<&'static DocumentSchema>> {
    static SUITE: LazyLock<Result<DocumentSchema, String>> =
        LazyLock::new(|| DocumentSchema::new(schemars::schema_for!(TestSuite)));
    static WORKSPACE: LazyLock<Result<DocumentSchema, String>> =
        LazyLock::new(|| DocumentSchema::new(schemars::schema_for!(WorkspaceConfig)));
    static VIEWPORTS: LazyLock<Result<DocumentSchema, String>> =
        LazyLock::new(|| DocumentSchema::new(schemars::schema_for!(Vec<ViewportDef>)));
    let schema = match kind {
        DocumentKind::Suite => &*SUITE,
        DocumentKind::Workspace => &*WORKSPACE,
        DocumentKind::Viewports => &*VIEWPORTS,
        DocumentKind::Data => return Ok(None),
    };
    schema
        .as_ref()
        .map(Some)
        .map_err(|error| anyhow!("Unable to load editor schema: {error}"))
}

pub(crate) fn schema(kind: DocumentKind) -> Result<Option<&'static Value>> {
    Ok(document_schema(kind)?.map(|schema| &schema.root))
}

pub fn diagnostics(kind: DocumentKind, value: &Value) -> Result<Vec<Diagnostic>> {
    let Some(schema) = document_schema(kind)? else {
        return Ok(Vec::new());
    };
    let mut diagnostics = Vec::new();
    for error in schema.validator.iter_errors(value).take(MAX_DIAGNOSTICS) {
        explain(&error, &schema.root, value, &mut diagnostics);
        if diagnostics.len() >= MAX_DIAGNOSTICS {
            break;
        }
    }
    check_integer_representation(value, &schema.root, &schema.root, "", &mut diagnostics, 0);
    diagnostics.truncate(MAX_DIAGNOSTICS);
    Ok(diagnostics)
}

pub(crate) fn action_viewport_diagnostics(
    suite_name: &str,
    suite: &TestSuite,
    scenario_index: usize,
    viewports: &[ViewportDef],
) -> Vec<Diagnostic> {
    let Some(scenario) = suite.scenarios.get(scenario_index) else {
        return Vec::new();
    };
    match crate::test::validate_action_viewport_names(
        suite_name,
        scenario,
        &suite.scenarios,
        viewports,
    ) {
        Ok(()) => Vec::new(),
        Err(error) => {
            let pointer = match error.downcast_ref::<crate::test::ActionViewportNameError>() {
                Some(source) => format!(
                    "/scenarios/{}/actions/{}/viewportNames",
                    source.source_scenario_index.unwrap_or(scenario_index),
                    source.action_index,
                ),
                None => format!("/scenarios/{scenario_index}/actions"),
            };
            vec![Diagnostic {
                pointer,
                message: error.to_string(),
            }]
        }
    }
}

fn check_integer_representation(
    value: &Value,
    node: &Value,
    root: &Value,
    pointer: &str,
    diagnostics: &mut Vec<Diagnostic>,
    depth: usize,
) {
    if depth > 128 || diagnostics.len() >= MAX_DIAGNOSTICS {
        return;
    }
    let Some(node) = schema::variants(node, root)
        .into_iter()
        .min_by_key(|node| branch_score(node, root, value))
    else {
        return;
    };
    let integer = node["type"] == "integer"
        || node["type"]
            .as_array()
            .is_some_and(|types| types.iter().any(|kind| kind == "integer"));
    if integer
        && value.as_f64().is_some_and(|number| number.fract() == 0.0)
        && value.as_i64().is_none()
        && value.as_u64().is_none()
        && !diagnostics.iter().any(|issue| issue.pointer == pointer)
    {
        diagnostics.push(Diagnostic {
            pointer: pointer.into(),
            message: format!("{}: Use a whole number written without a decimal point or exponent, for example 1000 instead of 1000.0. This field does not accept fractional milliseconds or dimensions.", display_path(pointer)),
        });
    }
    if let Some(properties) = node["properties"].as_object()
        && let Some(object) = value.as_object()
    {
        for (key, child) in object {
            if let Some(definition) = properties.get(key) {
                check_integer_representation(
                    child,
                    definition,
                    root,
                    &format!("{pointer}/{}", escape_pointer(key)),
                    diagnostics,
                    depth + 1,
                );
            }
        }
    } else if let Some(items) = node.get("items")
        && let Some(array) = value.as_array()
    {
        for (index, child) in array.iter().enumerate() {
            check_integer_representation(
                child,
                items,
                root,
                &format!("{pointer}/{index}"),
                diagnostics,
                depth + 1,
            );
        }
    }
}

fn branch_score(schema: &Value, root: &Value, value: &Value) -> (bool, Reverse<usize>) {
    let schema = resolve(schema, root);
    if let Some(branches) = schema["anyOf"].as_array() {
        return branches
            .iter()
            .map(|branch| branch_score(branch, root, value))
            .min()
            .unwrap_or((true, Reverse(0)));
    }
    let types = match &schema["type"] {
        Value::String(kind) => vec![kind.as_str()],
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let matches = types.is_empty()
        || types.iter().any(|kind| match *kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "integer" => value.as_u64().is_some() || value.as_i64().is_some(),
            "number" => value.is_number(),
            "null" => value.is_null(),
            _ => false,
        });
    let overlap = schema["properties"].as_object().map_or(0, |properties| {
        properties
            .keys()
            .filter(|key| value.get(*key).is_some())
            .count()
    });
    (!matches, Reverse(overlap))
}

fn explain(
    error: &ValidationError<'_>,
    root: &Value,
    value: &Value,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if diagnostics.len() >= MAX_DIAGNOSTICS {
        return;
    }
    let mut pointer = error.instance_path().to_string();
    let schema_path = error.schema_path().to_string();
    let parent = schema_path
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    let schema = root.pointer(parent).unwrap_or(root);
    if let ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context } =
        error.kind()
    {
        let branches = root.pointer(&schema_path).and_then(Value::as_array);
        let instance = value.pointer(&pointer).unwrap_or(&Value::Null);
        let action_union = branches.is_some_and(|branches| {
            branches.len() > 1
                && branches.iter().all(|branch| {
                    let branch = resolve(branch, root);
                    ACTIONS
                        .iter()
                        .any(|action| branch["properties"].get(*action).is_some())
                })
        });
        if action_union && instance.is_object() {
            let actions = ACTIONS
                .iter()
                .filter(|action| instance.get(**action).is_some())
                .filter(|action| **action != "key" || instance.get("press").is_none())
                .collect::<Vec<_>>();
            if actions.len() != 1 {
                let message = if actions.is_empty() {
                    let typo = instance
                        .as_object()
                        .into_iter()
                        .flat_map(|object| object.keys())
                        .find_map(|key| {
                            closest_key(key, ACTIONS.iter().copied())
                                .map(|suggestion| format!(" Did you mean '{suggestion}'?"))
                        })
                        .unwrap_or_default();
                    format!(
                        "Add one action, for example click: \"#submit\".{typo} Available actions: {}.",
                        ACTIONS.join(", ")
                    )
                } else {
                    "Use one action per list item. Put each additional action on a new '- action: value' line in YAML, or in a separate object in the JSON actions array.".into()
                };
                diagnostics.push(Diagnostic {
                    message: format!("{}: {message}", display_path(&pointer)),
                    pointer,
                });
                return;
            }
        }
        if let Some((index, errors)) = context.iter().enumerate().min_by_key(|(index, errors)| {
            let score = branches
                .and_then(|branches| branches.get(*index))
                .map_or((true, Reverse(0)), |branch| {
                    branch_score(branch, root, instance)
                });
            (score, errors.len())
        }) && branches
            .and_then(|branches| branches.get(index))
            .is_some_and(|branch| !branch_score(branch, root, instance).0)
        {
            for child in errors {
                explain(child, root, value, diagnostics);
            }
            return;
        }
    }
    if let ValidationErrorKind::AdditionalProperties { unexpected } = error.kind() {
        for key in unexpected
            .iter()
            .take(MAX_DIAGNOSTICS.saturating_sub(diagnostics.len()))
        {
            let keys = schema["properties"].as_object();
            let correction = closest_key(
                key,
                keys.into_iter()
                    .flat_map(|keys| keys.keys().map(String::as_str)),
            );
            let help = correction
                .map(|correction| {
                    format!("Did you mean '{correction}'? Field names are case-sensitive.")
                })
                .unwrap_or_else(|| {
                    format!(
                        "Remove this field. Available fields here: {}.",
                        keys.map(|keys| keys
                            .keys()
                            .take(12)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "))
                            .unwrap_or_default()
                    )
                });
            let pointer = format!("{pointer}/{}", escape_pointer(key));
            diagnostics.push(Diagnostic {
                message: format!("{}: Unknown field. {help}", display_path(&pointer)),
                pointer,
            });
        }
        return;
    }
    let mut help_schema = schema;
    let explanation = match error.kind() {
        ValidationErrorKind::Required { property } => {
            let property = property.as_str().unwrap_or_default();
            pointer.push('/');
            pointer.push_str(&escape_pointer(property));
            help_schema = &schema["properties"][property];
            format!(
                "Missing required field. Add {property}: {}.",
                example(help_schema, root)
            )
        }
        ValidationErrorKind::Enum { options } => format!(
            "Choose one of: {}.",
            options
                .as_array()
                .map(|options| options
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        ),
        ValidationErrorKind::Minimum { limit } => {
            format!("Use a number greater than or equal to {limit}.")
        }
        ValidationErrorKind::Maximum { limit } => {
            format!("Use a number less than or equal to {limit}.")
        }
        ValidationErrorKind::MinLength { limit } => {
            format!("Enter text with at least {limit} character(s).")
        }
        ValidationErrorKind::MultipleOf { multiple_of } => {
            format!("Use a whole multiple of {multiple_of}.")
        }
        // Nonblank constraints otherwise fall through to a misleading type mismatch.
        ValidationErrorKind::Pattern { pattern } if pattern == r"\S" => {
            "Enter at least one non-whitespace character.".into()
        }
        _ => format!(
            "Expected {}; found {}. Example: {}.",
            expected(schema, root),
            value_type(value.pointer(&pointer).unwrap_or(&Value::Null)),
            example(schema, root)
        ),
    };
    let description = schema::annotation(help_schema, root, "description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    diagnostics.push(Diagnostic {
        message: format!(
            "{}: {explanation}{}",
            display_path(&pointer),
            if description.is_empty() {
                String::new()
            } else {
                format!("\n{description}")
            }
        ),
        pointer,
    });
}

fn closest_key<'key>(key: &str, candidates: impl Iterator<Item = &'key str>) -> Option<&'key str> {
    if key.len() > 80 {
        return None;
    }
    candidates
        .map(|candidate| {
            (
                candidate,
                strsim::damerau_levenshtein(
                    &key.to_ascii_lowercase(),
                    &candidate.to_ascii_lowercase(),
                ),
            )
        })
        .filter(|(_, distance)| *distance <= if key.len() < 4 { 1 } else { 2 })
        .min_by_key(|(_, distance)| *distance)
        .map(|(candidate, _)| candidate)
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "an empty value (null)",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

pub(crate) fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

pub(crate) fn display_path(pointer: &str) -> String {
    let mut path = String::new();
    for segment in pointer.split('/').skip(1) {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        if segment.parse::<usize>().is_ok() {
            path.push_str(&format!("[{segment}]"));
        } else {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(&segment);
        }
    }
    if path.is_empty() {
        "Document".into()
    } else {
        path
    }
}

pub(crate) fn expected(schema: &Value, root: &Value) -> String {
    let schema = resolve(schema, root);
    if let Some(constant) = schema.get("const") {
        return format!("one of {constant}");
    }
    if let Some(choices) = schema["enum"].as_array() {
        return format!(
            "one of {}",
            choices
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(branches) = schema["anyOf"].as_array() {
        return branches
            .iter()
            .filter(|branch| branch["type"] != "null")
            .map(|branch| expected(branch, root))
            .collect::<Vec<_>>()
            .join(" or ");
    }
    let kind = schema["type"].as_str().or_else(|| {
        schema["type"].as_array().and_then(|types| {
            types
                .iter()
                .filter_map(Value::as_str)
                .find(|kind| *kind != "null")
        })
    });
    match kind {
        Some("string") => "text in quotes".into(),
        Some("integer") => "a whole number".into(),
        Some("number") => "a number".into(),
        Some("boolean") => "true or false (without quotes)".into(),
        Some("array") => format!("a list of {}", expected(&schema["items"], root)),
        Some("object") => {
            let required = schema["required"]
                .as_array()
                .map(|keys| {
                    keys.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if required.is_empty() {
                "an object with named fields".into()
            } else {
                format!("an object with {required}")
            }
        }
        _ => "a value matching this field's documented format".into(),
    }
}

fn example(schema: &Value, root: &Value) -> String {
    schema::examples(schema, root)
        .first()
        .map(Value::to_string)
        .unwrap_or_else(|| expected(schema, root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expanded_actions_report_semantic_and_viewport_sequence_errors() {
        use crate::desktop::document::semantic_diagnostics;
        for action in [
            json!({"drag":"#x","to":"#y","offset":{"x":1,"y":0}}),
            json!({"wait":10,"state":"hidden"}),
            json!({"dialog":"dismiss","kind":"prompt","promptText":"private"}),
            json!({"wheel":"#x"}),
        ] {
            let value = json!({"scenarios":[{"url":"/","actions":[action]}]});
            let issues = semantic_diagnostics(DocumentKind::Suite, &value);
            assert_eq!(issues.len(), 1, "{issues:?}");
            assert_eq!(issues[0].pointer, "/scenarios/0/actions/0");
            assert!(!issues[0].message.contains("private"));
        }
        for action in [json!({"key":"Tab"}), json!({"press":"#x","key":"Enter"})] {
            assert!(
                diagnostics(
                    DocumentKind::Suite,
                    &json!({"scenarios":[{"url":"/","actions":[action]}]})
                )
                .unwrap()
                .is_empty()
            );
        }
        let mut suite: TestSuite =
            serde_json::from_value(json!({"scenarios":[{"url":"/","actions":[
                {"dialog":"accept","viewportNames":"mobile"},
                {"click":"#confirm","viewportNames":"mobile"},
                {"wait":0,"viewportNames":"desktop"}
            ]}]}))
            .unwrap();
        let viewports = [
            ViewportDef {
                label: "mobile".into(),
                width: 400,
                height: 600,
            },
            ViewportDef {
                label: "desktop".into(),
                width: 1000,
                height: 800,
            },
        ];
        assert!(action_viewport_diagnostics("fixture", &suite, 0, &viewports).is_empty());
        suite.scenarios[0].actions.as_mut().unwrap().remove(1);
        let issues = action_viewport_diagnostics("fixture", &suite, 0, &viewports);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].pointer, "/scenarios/0/actions");
        assert!(issues[0].message.contains("dialog"));
    }

    #[test]
    fn action_viewport_names_diagnostics_share_preflight_errors() {
        let suite: TestSuite = serde_json::from_value(json!({ "scenarios": [
            { "id": "setup", "url": "/", "actions": [{ "click": "#target", "viewportNames": ["mobile"] }] },
            { "id": "consumer", "url": "/", "needs": "setup" },
        ] })).unwrap();
        let desktop = ViewportDef {
            label: "desktop".into(),
            width: 1280,
            height: 720,
        };
        let mobile = ViewportDef {
            label: "mobile".into(),
            width: 375,
            height: 667,
        };
        let viewports = vec![desktop.clone()];
        let error = crate::test::validate_action_viewport_names(
            "fixture",
            &suite.scenarios[1],
            &suite.scenarios,
            &viewports,
        )
        .unwrap_err();
        let issues = action_viewport_diagnostics("fixture", &suite, 1, &viewports);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].message, error.to_string());
        assert_eq!(issues[0].pointer, "/scenarios/0/actions/0/viewportNames");
        assert!(issues[0].message.contains("prerequisite 'setup' action #1"));
        assert!(action_viewport_diagnostics("fixture", &suite, 1, &[desktop, mobile]).is_empty());
    }

    #[test]
    fn generated_action_viewport_names_are_consistent_and_optional() {
        let root = schema(DocumentKind::Suite).unwrap().unwrap();
        let branches = root["$defs"]["TestAction"]["anyOf"].as_array().unwrap();
        assert_eq!(branches.len(), ACTIONS.len());
        let expected = &root["$defs"]["ClickAction"]["properties"]["viewportNames"];
        let mut field_schema = expected.clone();
        field_schema["$defs"] = root["$defs"].clone();
        let validator = jsonschema::options()
            .offline()
            .build(&field_schema)
            .unwrap();
        for value in [
            json!("mobile"),
            json!(["desktop", "mobile"]),
            json!([]),
            json!(null),
        ] {
            assert!(validator.is_valid(&value), "{value}");
        }
        for value in [json!(1), json!(false), json!([1]), json!([null]), json!({})] {
            assert!(!validator.is_valid(&value), "{value}");
        }
        assert_eq!(
            expected["description"],
            "Restrict this action to one viewport label or a list of labels (case-insensitive).\nOmitted, null, or an empty list means all scenario viewports."
        );
        assert!(expected.get("default").is_none() || expected["default"].is_null());
        for action in ACTIONS {
            let definition = branches
                .iter()
                .map(|branch| resolve(branch, root))
                .find(|branch| super::super::schema::action_discriminator(branch) == Some(*action))
                .unwrap();
            assert_eq!(
                &definition["properties"]["viewportNames"], expected,
                "{action}"
            );
            assert_eq!(definition["additionalProperties"], false, "{action}");
            assert!(
                !definition["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("viewportNames")),
                "{action}"
            );
        }
    }

    #[test]
    fn lazy_load_schema_feedback_handles_all_scopes_and_formats() {
        for key in ["lazyLoadScrollDelay", "lazyLoadTimeout"] {
            for (value, valid) in [
                (json!(null), true),
                (json!(0), key == "lazyLoadScrollDelay"),
                (json!(1), true),
                (json!(60000), true),
                (json!(-1), false),
                (json!(0.5), false),
                (json!(1.0), false),
                (json!("100"), false),
                (json!(false), false),
            ] {
                for (kind, document, pointer) in [
                    (
                        DocumentKind::Workspace,
                        json!({(key): value}),
                        format!("/{key}"),
                    ),
                    (
                        DocumentKind::Suite,
                        json!({(key): value, "scenarios": []}),
                        format!("/{key}"),
                    ),
                    (
                        DocumentKind::Suite,
                        json!({"scenarios": [{"url": "/", (key): value}]}),
                        format!("/scenarios/0/{key}"),
                    ),
                ] {
                    for document in [
                        serde_json::from_str::<Value>(&serde_json::to_string(&document).unwrap())
                            .unwrap(),
                        serde_yaml::from_str::<Value>(&serde_yaml::to_string(&document).unwrap())
                            .unwrap(),
                    ] {
                        let issues = diagnostics(kind, &document).unwrap();
                        assert_eq!(issues.is_empty(), valid, "{document}: {issues:?}");
                        assert!(
                            issues.iter().all(|issue| issue.pointer == pointer),
                            "{issues:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn screenshot_selector_schema_feedback_handles_yaml_and_json() {
        for (selector, valid) in [
            (json!(null), true),
            (json!("#content"), true),
            (json!(""), false),
            (json!(" \t\n"), false),
            (json!(["#content"]), false),
            (json!(1), false),
            (json!(false), false),
            (json!({}), false),
        ] {
            for (scenario, pointer) in [
                (
                    json!({ "url": "/", "selector": selector }),
                    "/scenarios/0/selector",
                ),
                (
                    json!({ "url": "/", "actions": [{ "save": "detail", "selector": selector }] }),
                    "/scenarios/0/actions/0/selector",
                ),
            ] {
                let value = json!({ "scenarios": [scenario] });
                for value in [
                    serde_json::from_str::<Value>(&serde_json::to_string(&value).unwrap()).unwrap(),
                    serde_yaml::from_str::<Value>(&serde_yaml::to_string(&value).unwrap()).unwrap(),
                ] {
                    let issues = diagnostics(DocumentKind::Suite, &value).unwrap();
                    assert_eq!(issues.is_empty(), valid, "{value}: {issues:?}");
                    if !valid {
                        assert!(
                            issues.iter().all(|issue| issue.pointer == pointer),
                            "{issues:?}"
                        );
                    } else {
                        let suite: TestSuite = serde_json::from_value(value).unwrap();
                        crate::test::capture::validate_scenario_capture_names(&suite.scenarios[0])
                            .unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn screenshot_selector_schema_rejects_mixed_final_target_settings() {
        for (selector, selectors, valid) in [
            (json!(null), json!(["viewport"]), true),
            (json!("#content"), json!(null), true),
            (json!("#content"), json!(["viewport"]), false),
            (json!("#content"), json!("viewport"), false),
        ] {
            let value = json!({ "scenarios": [{ "url": "/", "selector": selector, "selectors": selectors }] });
            let issues = diagnostics(DocumentKind::Suite, &value).unwrap();
            assert_eq!(issues.is_empty(), valid, "{value}: {issues:?}");
            let suite: TestSuite = serde_json::from_value(value).unwrap();
            assert_eq!(
                crate::test::capture::validate_scenario_capture_names(&suite.scenarios[0]).is_ok(),
                valid
            );
        }
    }

    #[test]
    fn action_viewport_names_schema_feedback_handles_yaml_and_json() {
        for (action, valid, mixed) in [
            (json!({"click": "#target"}), true, false),
            (
                json!({"click": "#target", "viewportNames": null}),
                true,
                false,
            ),
            (
                json!({"click": "#target", "viewportNames": []}),
                true,
                false,
            ),
            (
                json!({"click": "#target", "viewportNames": ["desktop", "mobile"]}),
                true,
                false,
            ),
            (
                json!({"switchTab": "new", "viewportNames": ["mobile"]}),
                true,
                false,
            ),
            (
                json!({"click": "#target", "viewportNames": "mobile"}),
                true,
                false,
            ),
            (
                json!({"switchTab": "new", "viewportNames": "mobile"}),
                true,
                false,
            ),
            (
                json!({"click": "#target", "viewportNames": 1}),
                false,
                false,
            ),
            (
                json!({"click": "#target", "viewportNames": [1]}),
                false,
                false,
            ),
            (
                json!({"click": "#target", "wait": 0, "viewportNames": ["mobile"]}),
                false,
                true,
            ),
            (
                json!({"click": "#target", "wait": 0, "viewportNames": "mobile"}),
                false,
                true,
            ),
        ] {
            let value = json!({"scenarios": [{"url": "/", "actions": [action]}]});
            let json = serde_json::to_string(&value).unwrap();
            let yaml = serde_yaml::to_string(&value).unwrap();
            for value in [
                serde_json::from_str::<Value>(&json).unwrap(),
                serde_yaml::from_str::<Value>(&yaml).unwrap(),
            ] {
                let issues = diagnostics(DocumentKind::Suite, &value).unwrap();
                assert_eq!(issues.is_empty(), valid, "{value}: {issues:?}");
                assert_eq!(serde_json::from_value::<TestSuite>(value).is_ok(), valid);
                if !valid {
                    assert_eq!(issues.len(), 1, "{issues:?}");
                    if mixed {
                        assert!(issues[0].message.contains("one action per list item"));
                    } else {
                        assert!(
                            issues[0]
                                .pointer
                                .starts_with("/scenarios/0/actions/0/viewportNames")
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn pixel_match_threshold_diagnostics_cover_every_configuration_level() {
        for (value, valid) in [
            (json!(null), true),
            (json!(0), true),
            (json!(0.1), true),
            (json!(5), true),
            (json!(100), true),
            (json!(-0.01), false),
            (json!(100.01), false),
            (json!("1"), false),
            (json!(true), false),
        ] {
            for (kind, config, pointer) in [
                (
                    DocumentKind::Workspace,
                    json!({"pixelMatchThreshold": value}),
                    "/pixelMatchThreshold",
                ),
                (
                    DocumentKind::Suite,
                    json!({"pixelMatchThreshold": value, "scenarios": []}),
                    "/pixelMatchThreshold",
                ),
                (
                    DocumentKind::Suite,
                    json!({"scenarios": [{"url": "/", "pixelMatchThreshold": value}]}),
                    "/scenarios/0/pixelMatchThreshold",
                ),
            ] {
                let issues = diagnostics(kind, &config).unwrap();
                assert_eq!(issues.is_empty(), valid, "{issues:?}");
                assert_eq!(
                    crate::desktop::document::validate_semantics(kind, &config).is_ok(),
                    valid
                );
                if !valid {
                    assert_eq!(issues.len(), 1);
                    assert_eq!(issues[0].pointer, pointer);
                    assert!(issues[0].message.contains("pixelMatchThreshold"));
                }
            }
        }
    }

    #[test]
    fn image_simplification_diagnostics_cover_every_level_format_and_preflight_rule() {
        for (key, value, valid, guidance) in [
            ("imageSimplification", json!("none"), true, ""),
            ("imageSimplification", json!("pixelate"), true, ""),
            ("imageSimplification", json!(null), true, ""),
            ("imageSimplification", json!("off"), false, "Choose one of"),
            ("imageSimplification", json!(false), false, "pixelate"),
            ("imageSimplificationBlockSize", json!(2), true, ""),
            ("imageSimplificationBlockSize", json!(16), true, ""),
            (
                "imageSimplificationBlockSize",
                json!(5),
                false,
                "multiple of 2",
            ),
            (
                "imageSimplificationBlockSize",
                json!(0),
                false,
                "greater than or equal to 2",
            ),
            (
                "imageSimplificationBlockSize",
                json!(18),
                false,
                "less than or equal to 16",
            ),
            (
                "imageSimplificationBlockSize",
                json!(4.0),
                false,
                "without a decimal point",
            ),
            (
                "imageSimplificationBlockSize",
                json!("4"),
                false,
                "whole number",
            ),
            ("imageSimplificationMinSize", json!(1), true, ""),
            (
                "imageSimplificationMinSize",
                json!(0),
                false,
                "greater than or equal to 1",
            ),
            ("imageSimplificationMinSize", json!(-1), false, ""),
            ("imageSimplificationExcludeSelectors", json!([]), true, ""),
            (
                "imageSimplificationExcludeSelectors",
                json!([".logo"]),
                true,
                "",
            ),
            (
                "imageSimplificationExcludeSelectors",
                json!([".logo", " "]),
                false,
                "non-whitespace",
            ),
            (
                "imageSimplificationExcludeSelectors",
                json!([""]),
                false,
                "at least 1",
            ),
            (
                "imageSimplificationExcludeSelectors",
                json!(".logo"),
                false,
                "list",
            ),
        ] {
            for (kind, document, pointer) in [
                (
                    DocumentKind::Workspace,
                    json!({(key): value}),
                    format!("/{key}"),
                ),
                (
                    DocumentKind::Suite,
                    json!({(key): value, "scenarios": []}),
                    format!("/{key}"),
                ),
                (
                    DocumentKind::Suite,
                    json!({"scenarios": [{"url": "/", (key): value}]}),
                    format!("/scenarios/0/{key}"),
                ),
            ] {
                for document in [
                    serde_json::from_str::<Value>(&serde_json::to_string(&document).unwrap())
                        .unwrap(),
                    serde_yaml::from_str::<Value>(&serde_yaml::to_string(&document).unwrap())
                        .unwrap(),
                ] {
                    let issues = diagnostics(kind, &document).unwrap();
                    assert_eq!(issues.is_empty(), valid, "{document}: {issues:?}");
                    assert_eq!(
                        crate::desktop::document::validate_value(kind, &document).is_ok(),
                        valid,
                        "{document}"
                    );
                    assert!(
                        issues
                            .iter()
                            .all(|issue| issue.pointer.starts_with(&pointer)),
                        "{issues:?}"
                    );
                    assert!(
                        issues.iter().any(|issue| issue.message.contains(guidance)) || valid,
                        "{guidance}: {issues:?}"
                    );
                    assert!(issues.iter().all(|issue| !issue.message.contains(".logo")));
                }
            }
        }
        // A blank exclusion is reported at the offending list item.
        let issues = diagnostics(
            DocumentKind::Workspace,
            &json!({"imageSimplificationExcludeSelectors": [".logo", "\t"]}),
        )
        .unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].pointer, "/imageSimplificationExcludeSelectors/1");
    }

    #[test]
    fn snapshot_report_settings_accept_optional_text_and_reject_other_types() {
        for key in ["snapshotTitle", "snapshotHeading", "snapshotSubheading"] {
            for value in [json!(null), json!(""), json!("**Staging** <results>")] {
                let config = json!({key: value});
                assert!(
                    diagnostics(DocumentKind::Workspace, &config)
                        .unwrap()
                        .is_empty()
                );
                assert!(serde_json::from_value::<WorkspaceConfig>(config).is_ok());
            }
            for value in [json!(5), json!(true), json!([]), json!({})] {
                let config = json!({key: value});
                let issues = diagnostics(DocumentKind::Workspace, &config).unwrap();
                assert_eq!(issues.len(), 1, "{issues:?}");
                assert_eq!(issues[0].pointer, format!("/{key}"));
                assert!(issues[0].message.contains("text in quotes"), "{issues:?}");
                assert!(serde_json::from_value::<WorkspaceConfig>(config).is_err());
            }
        }
    }

    #[test]
    fn nested_errors_name_the_field_and_repair_without_enum_noise_or_values() {
        for (value, pointer, guidance) in [
            (
                json!({"basicAuth":[{"username":"private-user","password":"private-password"}],"scenarios":[]}),
                "/basicAuth/0/origin",
                "https://example.com",
            ),
            (
                json!({"basicAuth":{"origin":"https://example.com","username":123,"password":"private-password"},"scenarios":[]}),
                "/basicAuth/username",
                "text in quotes",
            ),
            (
                json!({"scenarios":[{"url":"/","actions":[{"press":"#search"}]}]}),
                "/scenarios/0/actions/0/key",
                "Enter",
            ),
            (
                json!({"scenarios":[{"url":"/","actions":[{"input":"#search","value":true}]}]}),
                "/scenarios/0/actions/0/value",
                "text in quotes",
            ),
            (
                json!({"scenarios":[{"url":"/","actions":[{"click":"#search","fram":"iframe"}]}]}),
                "/scenarios/0/actions/0/fram",
                "Did you mean 'frame'",
            ),
            (
                json!({"scenarios":[],"urlReplacements":[{"ref":"old"}]}),
                "/urlReplacements/0/test",
                "Missing required field",
            ),
            (
                json!({"scenarios":[],"misMatchThreshold":101}),
                "/misMatchThreshold",
                "100",
            ),
            (
                json!({"scenarios":[],"hideSelectors":"#advert"}),
                "/hideSelectors",
                "list",
            ),
            (
                json!({"scenarios":[],"browser":"firefox"}),
                "/browser",
                "chromium",
            ),
            (
                json!({"scenarios":[],"viewportNames":[1]}),
                "/viewportNames/0",
                "text in quotes",
            ),
        ] {
            let issues = diagnostics(DocumentKind::Suite, &value).unwrap();
            assert_eq!(issues.len(), 1, "{issues:?}");
            assert_eq!(issues[0].pointer, pointer, "{issues:?}");
            assert!(issues[0].message.contains(guidance), "{issues:?}");
            for private in [
                "private-user",
                "private-password",
                "untagged",
                "BasicAuthConfig",
                "TestAction",
            ] {
                assert!(!issues[0].message.contains(private), "{issues:?}");
            }
        }
    }

    #[test]
    fn action_mistakes_have_a_single_actionable_message() {
        for (action, guidance) in [
            (json!({"clik":"#submit"}), "Did you mean 'click'"),
            (
                json!({"click":"#submit", "hover":"#menu"}),
                "one action per list item",
            ),
            (json!({}), "Add one action"),
        ] {
            let issues = diagnostics(
                DocumentKind::Suite,
                &json!({"scenarios":[{"url":"/","actions":[action]}]}),
            )
            .unwrap();
            assert_eq!(issues.len(), 1, "{issues:?}");
            assert!(issues[0].message.contains(guidance), "{issues:?}");
        }
    }

    #[test]
    fn switch_tab_diagnostics_follow_the_typed_action_schema() {
        for action in [
            json!({"switchTab": "new", "url": "**/quote"}),
            json!({"switchTab": "previous"}),
            json!({"switchTab": "original"}),
        ] {
            let issues = diagnostics(
                DocumentKind::Suite,
                &json!({"scenarios": [{"url": "/", "actions": [action]}]}),
            )
            .unwrap();
            assert!(issues.is_empty(), "{issues:?}");
        }
        for (action, key) in [
            (json!({"switchTab": "unknown"}), "switchTab"),
            (json!({"switchTab": "new", "frame": "iframe"}), "frame"),
        ] {
            let issues = diagnostics(
                DocumentKind::Suite,
                &json!({"scenarios": [{"url": "/", "actions": [action]}]}),
            )
            .unwrap();
            assert_eq!(issues.len(), 1, "{issues:?}");
            assert_eq!(issues[0].pointer, format!("/scenarios/0/actions/0/{key}"));
        }
    }

    #[test]
    fn integer_spelling_errors_do_not_fall_back_to_untagged_enum_messages() {
        for (value, pointer) in [
            (
                json!({"scenarios":[{"url":"/","actions":[{"wait":1000.0}]}]}),
                "/scenarios/0/actions/0/wait",
            ),
            (
                json!({"scenarios":[],"asyncCaptureLimit":5.0}),
                "/asyncCaptureLimit",
            ),
            (
                json!({"scenarios":[{"url":"/","engineOptions":{"gotoParameters":{"timeout":1000.0}}}]}),
                "/scenarios/0/engineOptions/gotoParameters/timeout",
            ),
        ] {
            let issues = diagnostics(DocumentKind::Suite, &value).unwrap();
            assert_eq!(issues.len(), 1, "{issues:?}");
            assert_eq!(issues[0].pointer, pointer);
            assert!(issues[0].message.contains("without a decimal point"));
        }
        assert!(
            diagnostics(
                DocumentKind::Suite,
                &json!({"scenarios":[{"url":"/","delay":1000.0}],"misMatchThreshold":0.1})
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn diagnostics_are_multiple_bounded_and_data_documents_have_no_suite_schema() {
        let issues = diagnostics(
            DocumentKind::Suite,
            &json!({"scenarios":[{},{}],"asyncCaptureLimit":0}),
        )
        .unwrap();
        assert_eq!(issues.len(), 3, "{issues:?}");
        assert!(
            issues
                .iter()
                .any(|issue| issue.pointer == "/scenarios/1/url")
        );
        let issues = diagnostics(
            DocumentKind::Suite,
            &json!({"scenarios":vec![json!({}); 100]}),
        )
        .unwrap();
        assert_eq!(issues.len(), MAX_DIAGNOSTICS);
        assert!(
            diagnostics(DocumentKind::Data, &json!({"arbitrary":true}))
                .unwrap()
                .is_empty()
        );
        assert!(
            diagnostics(
                DocumentKind::Viewports,
                &json!([{"label":"phone","width":0,"height":600}])
            )
            .unwrap()[0]
                .message
                .contains("[0].width")
        );
        assert!(
            diagnostics(DocumentKind::Workspace, &json!({"asyncCaptureLimit":0})).unwrap()[0]
                .message
                .contains("1")
        );
    }

    #[test]
    fn generated_fields_have_help_and_every_documented_example_matches_its_schema() {
        for kind in [
            DocumentKind::Workspace,
            DocumentKind::Suite,
            DocumentKind::Viewports,
        ] {
            let root = schema(kind).unwrap().unwrap();
            let definitions = std::iter::once(root).chain(
                root["$defs"]
                    .as_object()
                    .into_iter()
                    .flat_map(|definitions| definitions.values()),
            );
            for definition in definitions {
                for (key, field) in definition["properties"].as_object().into_iter().flatten() {
                    assert!(
                        !schema::documentation(field, root).is_empty(),
                        "{kind:?}.{key}"
                    );
                    if let Some(examples) =
                        schema::annotation(field, root, "examples").and_then(Value::as_array)
                    {
                        let mut field_schema = field.clone();
                        field_schema["$defs"] = root["$defs"].clone();
                        let validator = jsonschema::options()
                            .offline()
                            .build(&field_schema)
                            .unwrap();
                        for example in examples {
                            assert!(validator.is_valid(example), "{kind:?}.{key}: {example}");
                        }
                    }
                }
            }
        }
    }
}
