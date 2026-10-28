//! Derive field coverage from the CLI types instead of maintaining a second schema.

use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};

use crate::{
    config::{WorkspaceConfig, resolve_config, viewports::ViewportDef},
    types::{TestAction, TestScenario, TestSuite},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Workspace,
    Suite,
    Scenario,
    Action(String),
    Viewport,
    Run,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Boolean,
    Number { integer: bool },
    List { single: bool },
    Choice(Vec<String>),
    Wait,
    Structured,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub description: String,
    pub kind: FieldKind,
    pub required: bool,
}

impl Field {
    pub fn toggle_list_choice(
        &self,
        explicit: &Value,
        inherited: &Value,
        choice: &str,
        available: &[String],
    ) -> Result<Value> {
        ensure!(
            matches!(self.kind, FieldKind::List { .. }),
            "{} is not a list",
            self.label
        );
        let current = if explicit.is_null() {
            inherited
        } else {
            explicit
        };
        let mut values = string_values(current);
        let viewports = self.key == "viewportNames";
        if viewports && values.is_empty() {
            values = available.to_vec();
        }
        let matches = |value: &str| {
            if viewports {
                value.eq_ignore_ascii_case(choice)
            } else {
                value == choice
            }
        };
        if values.iter().any(|value| matches(value)) {
            ensure!(
                !viewports || values.iter().any(|value| !matches(value)),
                "At least one viewport must remain selected"
            );
            values.retain(|value| !matches(value));
        } else {
            ensure!(
                available.iter().any(|value| matches(value)),
                "Unknown choice '{choice}'"
            );
            values.push(choice.into());
        }
        Ok(json!(values))
    }

    pub fn stepped_value(&self, value: &Value, increase: bool) -> Result<Value> {
        ensure!(
            matches!(self.kind, FieldKind::Number { .. }),
            "{} is not numeric",
            self.label
        );
        let number = value.as_f64().unwrap_or(0.0);
        if self.key == "imageSimplificationBlockSize" {
            // Block sizes must stay even; step an odd draft to the adjacent even value.
            let next = if increase {
                (number / 2.0).floor() * 2.0 + 2.0
            } else {
                (number / 2.0).ceil() * 2.0 - 2.0
            };
            return Ok(json!(next.clamp(
                crate::config::MIN_IMAGE_SIMPLIFICATION_BLOCK_SIZE as f64,
                crate::config::MAX_IMAGE_SIMPLIFICATION_BLOCK_SIZE as f64
            ) as u64));
        }
        let (step, minimum, maximum) = match self.key.as_str() {
            "misMatchThreshold" => (0.1, 0.0, 100.0),
            "pixelMatchThreshold" => (0.1, 0.0, 100.0),
            "scale" => (0.1, 0.1, 10.0),
            "asyncCaptureLimit"
            | "asyncCompareLimit"
            | "keepTestRuns"
            | "width"
            | "height"
            | "imageSimplificationMinSize" => (1.0, 1.0, u32::MAX as f64),
            "lazyLoadTimeout" => (100.0, 1.0, u32::MAX as f64),
            key if key.ends_with("Timeout")
                || matches!(key, "delay" | "postInteractionWait" | "lazyLoadScrollDelay") =>
            {
                (100.0, 0.0, u32::MAX as f64)
            }
            _ => (1.0, 0.0, u32::MAX as f64),
        };
        let next = ((number + if increase { step } else { -step }) * 1000.0).round() / 1000.0;
        let next = next.clamp(minimum, maximum);
        Ok(
            if matches!(self.kind, FieldKind::Number { integer: true }) {
                json!(next as u64)
            } else {
                json!(next)
            },
        )
    }

    pub fn parse(&self, text: &str) -> Result<Value> {
        Ok(match &self.kind {
            FieldKind::Text => json!(text),
            FieldKind::Boolean => json!(
                text.parse::<bool>()
                    .map_err(|_| anyhow!("{} must be true or false", self.label))?
            ),
            FieldKind::Number { integer } => {
                let number: Value = serde_json::from_str(text)
                    .map_err(|_| anyhow!("{} needs a number", self.label))?;
                ensure!(
                    number.is_number() && (!integer || number.as_u64().is_some()),
                    "{} needs a non-negative {}",
                    self.label,
                    if *integer { "integer" } else { "number" }
                );
                number
            }
            FieldKind::List { single } => {
                if text.trim_start().starts_with('[') {
                    let values: Vec<String> = serde_yaml::from_str(text)?;
                    json!(values)
                } else {
                    let lines = text
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .collect::<Vec<_>>();
                    if *single && lines.len() == 1 {
                        json!(lines[0])
                    } else {
                        json!(lines)
                    }
                }
            }
            FieldKind::Choice(choices) => {
                ensure!(
                    choices.iter().any(|choice| choice == text),
                    "Unsupported {}",
                    self.label
                );
                json!(text)
            }
            FieldKind::Wait => {
                if let Ok(milliseconds) = text.parse::<u64>() {
                    json!(milliseconds)
                } else {
                    ensure!(
                        !text.is_empty() && text.parse::<f64>().is_err(),
                        "Wait needs non-negative integer milliseconds or a selector"
                    );
                    json!(text)
                }
            }
            FieldKind::Structured => serde_yaml::from_str(text)?,
        })
    }

    pub fn display(&self, value: &Value) -> String {
        match (&self.kind, value) {
            (_, Value::Null) => String::new(),
            (_, Value::String(text)) => text.clone(),
            (FieldKind::List { .. }, Value::Array(items)) => items
                .iter()
                .map(|item| item.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n"),
            (FieldKind::Structured, _) => serde_json::to_string_pretty(value).unwrap_or_default(),
            _ => value.to_string(),
        }
    }
}

pub fn string_values(value: &Value) -> Vec<String> {
    match value {
        Value::String(value) => vec![value.clone()],
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

pub fn dependency_choices(
    suite: &Value,
    current: Option<usize>,
) -> Vec<super::workspace::ScenarioSummary> {
    let Ok(suite) = serde_json::from_value::<TestSuite>(suite.clone()) else {
        return Vec::new();
    };
    let mut counts = std::collections::HashMap::new();
    let scenarios = suite
        .scenarios
        .iter()
        .map(super::workspace::ScenarioSummary::from_scenario)
        .collect::<Vec<_>>();
    for scenario in &scenarios {
        *counts.entry(scenario.id.to_ascii_lowercase()).or_insert(0) += 1;
    }
    scenarios
        .into_iter()
        .enumerate()
        .filter(|(index, scenario)| {
            Some(*index) != current
                && !scenario.id.is_empty()
                && counts[&scenario.id.to_ascii_lowercase()] == 1
        })
        .map(|(_, scenario)| scenario)
        .collect()
}

pub fn fields(scope: &Scope) -> Result<Vec<Field>> {
    let root = match scope {
        Scope::Workspace => serde_json::to_value(schemars::schema_for!(WorkspaceConfig))?,
        Scope::Suite => serde_json::to_value(schemars::schema_for!(TestSuite))?,
        Scope::Scenario => serde_json::to_value(schemars::schema_for!(TestScenario))?,
        Scope::Action(_) => serde_json::to_value(schemars::schema_for!(TestAction))?,
        Scope::Viewport => serde_json::to_value(schemars::schema_for!(ViewportDef))?,
        Scope::Run => serde_json::to_value(schemars::schema_for!(super::runner::RunOptions))?,
    };
    let schema = if let Scope::Action(name) = scope {
        root["anyOf"]
            .as_array()
            .ok_or_else(|| anyhow!("Missing action schema"))?
            .iter()
            .map(|schema| resolve(schema, &root))
            .find(|schema| action_discriminator(schema) == Some(name.as_str()))
            .ok_or_else(|| anyhow!("Unknown action '{name}'"))?
    } else {
        &root
    };
    let properties = schema["properties"]
        .as_object()
        .ok_or_else(|| anyhow!("Missing editor schema"))?;
    let mut fields = Vec::new();
    for (key, definition) in properties {
        if matches!(key.as_str(), "scenarios" | "actions" | "testDate") {
            continue;
        }
        let required = schema["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|value| value == key));
        fields.push(Field {
            key: key.clone(),
            label: label(key),
            description: documentation(definition, &root),
            kind: kind(key, definition, &root),
            required,
        });
    }
    fields.sort_by_key(|field| (!field.required, priority(&field.key), field.key.clone()));
    Ok(fields)
}

pub(crate) fn resolve<'schema>(schema: &'schema Value, root: &'schema Value) -> &'schema Value {
    if let Some(reference) = schema["$ref"]
        .as_str()
        .and_then(|reference| reference.strip_prefix('#'))
    {
        root.pointer(reference).unwrap_or(schema)
    } else {
        schema
    }
}

pub(crate) fn action_discriminator(schema: &Value) -> Option<&'static str> {
    // Catalog order keeps press ahead of key, which is also press's required argument.
    super::document::ACTIONS.iter().copied().find(|name| {
        schema["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|value| value == name))
    })
}

pub(crate) fn variants<'schema>(
    schema: &'schema Value,
    root: &'schema Value,
) -> Vec<&'schema Value> {
    let schema = resolve(schema, root);
    if let Some(branches) = schema["anyOf"]
        .as_array()
        .or_else(|| schema["oneOf"].as_array())
    {
        return branches
            .iter()
            .flat_map(|branch| variants(branch, root))
            .collect();
    }
    vec![schema]
}

pub(crate) fn annotation<'schema>(
    schema: &'schema Value,
    root: &'schema Value,
    key: &str,
) -> Option<&'schema Value> {
    fn find<'schema>(
        schema: &'schema Value,
        root: &'schema Value,
        key: &str,
        depth: usize,
    ) -> Option<&'schema Value> {
        if depth > 32 {
            return None;
        }
        let resolved = resolve(schema, root);
        schema.get(key).or_else(|| resolved.get(key)).or_else(|| {
            resolved["anyOf"]
                .as_array()?
                .iter()
                .find_map(|branch| find(branch, root, key, depth + 1))
        })
    }
    find(schema, root, key, 0)
}

pub(crate) fn documentation(schema: &Value, root: &Value) -> String {
    let mut description = annotation(schema, root, "description")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let examples = annotation(schema, root, "examples").and_then(Value::as_array);
    if let Some(examples) = examples.filter(|examples| !examples.is_empty()) {
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description.push_str("Example: ");
        description.push_str(
            &examples
                .iter()
                .take(3)
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        );
    }
    description
}

pub(crate) fn examples(schema: &Value, root: &Value) -> Vec<Value> {
    fn sample(schema: &Value, root: &Value, depth: usize) -> Option<Value> {
        if depth > 16 {
            return None;
        }
        if let Some(example) = annotation(schema, root, "examples")
            .and_then(Value::as_array)
            .and_then(|examples| examples.first())
        {
            return Some(example.clone());
        }
        let schema = variants(schema, root)
            .into_iter()
            .find(|schema| schema["type"] != "null")?;
        if let Some(constant) = schema.get("const") {
            return Some(constant.clone());
        }
        if let Some(choice) = schema["enum"]
            .as_array()
            .and_then(|choices| choices.first())
        {
            return Some(choice.clone());
        }
        if let Some(items) = schema.get("items") {
            return Some(json!([sample(items, root, depth + 1)?]));
        }
        if let Some(properties) = schema["properties"].as_object() {
            let mut object = serde_json::Map::new();
            if let Some(required) = schema["required"].as_array() {
                for key in required.iter().filter_map(Value::as_str) {
                    object.insert(key.into(), sample(properties.get(key)?, root, depth + 1)?);
                }
            }
            return Some(Value::Object(object));
        }
        match types(schema, root).first().map(String::as_str) {
            Some("string") => Some(json!("text")),
            Some("integer" | "number") => Some(schema.get("minimum").cloned().unwrap_or(json!(0))),
            Some("boolean") => Some(json!(false)),
            _ => None,
        }
    }
    annotation(schema, root, "examples")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| sample(schema, root, 0).into_iter().collect())
}

fn types(schema: &Value, root: &Value) -> Vec<String> {
    let schema = resolve(schema, root);
    if let Some(variants) = schema["anyOf"].as_array() {
        return variants
            .iter()
            .flat_map(|variant| types(variant, root))
            .collect();
    }
    if let Some(variants) = schema["type"].as_array() {
        return variants
            .iter()
            .filter_map(Value::as_str)
            .filter(|kind| *kind != "null")
            .map(str::to_string)
            .collect();
    }
    schema["type"]
        .as_str()
        .filter(|kind| *kind != "null")
        .map(|kind| vec![kind.to_string()])
        .unwrap_or_default()
}

fn kind(key: &str, schema: &Value, root: &Value) -> FieldKind {
    if key == "wait" {
        return FieldKind::Wait;
    }
    let types = types(schema, root);
    if types.iter().any(|kind| kind == "array") {
        if types.iter().any(|kind| kind == "object")
            || variants(schema, root)
                .iter()
                .filter(|variant| variant.get("items").is_some())
                .any(|variant| resolve(&variant["items"], root)["type"] != "string")
        {
            return FieldKind::Structured;
        }
        return FieldKind::List {
            single: types.iter().any(|kind| kind == "string"),
        };
    }
    let choices = variants(schema, root)
        .iter()
        .flat_map(|variant| {
            variant["enum"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(variant.get("const"))
        })
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !choices.is_empty() {
        return FieldKind::Choice(choices);
    }
    if types.iter().any(|kind| kind == "boolean") {
        return FieldKind::Boolean;
    }
    if types
        .iter()
        .any(|kind| kind == "integer" || kind == "number")
    {
        return FieldKind::Number {
            integer: types.iter().any(|kind| kind == "integer"),
        };
    }
    if types.iter().any(|kind| kind == "string") {
        return FieldKind::Text;
    }
    FieldKind::Structured
}

fn priority(key: &str) -> usize {
    match key {
        "label" => 0,
        "url" => 1,
        "id" => 2,
        "description" => 3,
        "viewportNames" => 4,
        "misMatchThreshold" => 5,
        "pixelMatchThreshold" => 6,
        _ => 10,
    }
}

pub(crate) fn label(key: &str) -> String {
    match key {
        "misMatchThreshold" => return "Mismatch threshold (%)".into(),
        "pixelMatchThreshold" => return "Pixel color threshold".into(),
        "url" => return "URL".into(),
        "jsOnReadyPath" => return "Ready script".into(),
        "cssOverridePath" => return "CSS override file".into(),
        "autoCleanRuns" => return "Automatic run cleanup".into(),
        "keepTestRuns" => return "Test runs to keep".into(),
        "imageSimplificationBlockSize" => return "Pixelation block size (px)".into(),
        "imageSimplificationMinSize" => return "Minimum image size (px)".into(),
        "imageSimplificationExcludeSelectors" => return "Excluded image selectors".into(),
        _ => {}
    }
    let mut label = String::new();
    for (index, character) in key.chars().enumerate() {
        if index == 0 {
            label.extend(character.to_uppercase());
        } else if character.is_uppercase() {
            label.push(' ');
            label.extend(character.to_lowercase());
        } else {
            label.push(character);
        }
    }
    if key.ends_with("Timeout")
        || matches!(key, "delay" | "postInteractionWait" | "lazyLoadScrollDelay")
    {
        label.push_str(" (ms)");
    }
    label
}

/// Use the production resolver, including restore-state merging and timeout defaults.
pub fn effective(
    workspace: Option<&Value>,
    suite: Option<&Value>,
    scenario: Option<usize>,
) -> Result<Value> {
    let workspace: WorkspaceConfig =
        serde_json::from_value(workspace.cloned().unwrap_or_else(|| json!({})))?;
    let suite: TestSuite =
        serde_json::from_value(suite.cloned().unwrap_or_else(|| json!({"scenarios": []})))?;
    let empty: TestScenario = serde_json::from_value(json!({"url": ""}))?;
    let scenario = scenario
        .and_then(|index| suite.scenarios.get(index))
        .unwrap_or(&empty);
    let mut effective = serde_json::to_value(resolve_config(&workspace, &suite, scenario))?;
    effective["autoCleanRuns"] = json!(
        suite
            .auto_clean_runs
            .or(workspace.auto_clean_runs)
            .unwrap_or(false)
    );
    effective["keepTestRuns"] = json!(
        suite
            .keep_test_runs
            .or(workspace.keep_test_runs)
            .unwrap_or(1)
    );
    Ok(effective)
}

pub(crate) fn action_viewport_choices(
    effective: &Value,
    all: &[ViewportDef],
) -> Result<Vec<ViewportDef>> {
    if all.is_empty() {
        return Ok(Vec::new());
    }
    let names: Option<Vec<String>> = serde_json::from_value(effective["viewportNames"].clone())?;
    crate::config::viewports::resolve_viewports(names.as_deref(), all)
}

pub fn effective_key(key: &str) -> String {
    if matches!(
        key,
        "elementTimeout"
            | "actionabilityTimeout"
            | "settleTimeout"
            | "scenarioTimeout"
            | "lazyLoadScrollDelay"
            | "lazyLoadTimeout"
    ) {
        format!("{key}Ms")
    } else {
        key.to_string()
    }
}

pub fn origin(
    key: &str,
    workspace: Option<&Value>,
    suite: Option<&Value>,
    scenario: Option<&Value>,
) -> &'static str {
    if key == "state"
        && scenario
            .and_then(|scenario| scenario.get("restore"))
            .is_some_and(|value| !value.is_null())
    {
        return "State + restore";
    }
    for (source, name) in [
        (scenario, "Scenario"),
        (suite, "Suite"),
        (workspace, "Workspace"),
    ] {
        if source
            .and_then(|source| source.get(key))
            .is_some_and(|value| !value.is_null())
        {
            return name;
        }
    }
    "Default"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_collections_are_structured_and_inherit_schema_documentation() {
        let suite = fields(&Scope::Suite).unwrap();
        for key in ["basicAuth", "urlReplacements"] {
            let field = suite.iter().find(|field| field.key == key).unwrap();
            assert_eq!(field.kind, FieldKind::Structured, "{key}");
            assert!(!field.description.is_empty(), "{key}");
        }
        let auth = suite.iter().find(|field| field.key == "basicAuth").unwrap();
        let value =
            json!([{"origin": "https://example.com", "username": "user", "password": "pass"}]);
        assert_eq!(auth.parse(&auth.display(&value)).unwrap(), value);
        let scenario = fields(&Scope::Scenario).unwrap();
        assert_eq!(
            scenario
                .iter()
                .find(|field| field.key == "keyPressSelectors")
                .unwrap()
                .kind,
            FieldKind::Structured
        );
    }

    #[test]
    fn list_choices_preserve_inheritance_custom_values_and_viewport_all_semantics() {
        let fields = fields(&Scope::Scenario).unwrap();
        let viewports = fields
            .iter()
            .find(|field| field.key == "viewportNames")
            .unwrap();
        let names = vec!["phone".into(), "desktop".into()];
        assert_eq!(
            viewports
                .toggle_list_choice(&Value::Null, &Value::Null, "desktop", &names)
                .unwrap(),
            json!(["phone"])
        );
        assert!(
            viewports
                .toggle_list_choice(&json!(["phone"]), &Value::Null, "phone", &names)
                .is_err()
        );
        assert_eq!(
            viewports
                .toggle_list_choice(&Value::Null, &json!(["desktop"]), "phone", &names)
                .unwrap(),
            json!(["desktop", "phone"])
        );
        assert_eq!(
            viewports
                .toggle_list_choice(&json!(["custom", "PHONE"]), &Value::Null, "phone", &names)
                .unwrap(),
            json!(["custom"])
        );
        let needs = fields.iter().find(|field| field.key == "needs").unwrap();
        assert_eq!(
            needs
                .toggle_list_choice(&json!("login"), &Value::Null, "login", &["login".into()])
                .unwrap(),
            json!([])
        );
    }

    #[test]
    fn dependency_choices_follow_draft_ids_and_exclude_self_or_collisions() {
        let suite = json!({"scenarios": [
            {"url": "/", "label": "Current"}, {"url": "/login", "label": "Sign in"},
            {"url": "/", "id": "setup"}, {"url": "/", "id": "duplicate"}, {"url": "/", "id": "DUPLICATE"}
        ]});
        assert_eq!(
            dependency_choices(&suite, Some(0))
                .iter()
                .map(|scenario| scenario.id.as_str())
                .collect::<Vec<_>>(),
            vec!["sign-in", "setup"]
        );
    }

    #[test]
    fn pixel_match_threshold_fields_support_all_scopes_and_effective_values() {
        for scope in [Scope::Workspace, Scope::Suite, Scope::Scenario] {
            let fields = fields(&scope).unwrap();
            let field = fields
                .iter()
                .find(|field| field.key == "pixelMatchThreshold")
                .unwrap();
            assert_eq!(field.kind, FieldKind::Number { integer: false });
            assert_eq!(field.label, "Pixel color threshold");
            assert!(
                field.description.contains("default: 5")
                    || field.description.contains("Default: 5")
            );
            assert!(!field.required);
            assert_eq!(field.parse("1.1").unwrap(), json!(1.1));
            assert_eq!(field.stepped_value(&json!(5.0), true).unwrap(), json!(5.1));
            assert_eq!(field.stepped_value(&json!(5.0), false).unwrap(), json!(4.9));
            assert_eq!(field.stepped_value(&json!(0.0), false).unwrap(), json!(0.0));
            assert_eq!(
                field.stepped_value(&json!(100.0), true).unwrap(),
                json!(100.0)
            );
        }

        let workspace = json!({"pixelMatchThreshold": 10.0});
        let suite = json!({"pixelMatchThreshold": 3.0, "scenarios": [
            {"url": "/", "pixelMatchThreshold": 0.0}, {"url": "/other"}
        ]});
        for (workspace, suite, scenario, expected) in [
            (None, None, None, 5.0),
            (Some(&workspace), None, None, 10.0),
            (Some(&workspace), Some(&suite), None, 3.0),
            (Some(&workspace), Some(&suite), Some(0), 0.0),
            (Some(&workspace), Some(&suite), Some(1), 3.0),
        ] {
            assert_eq!(
                effective(workspace, suite, scenario).unwrap()["pixelMatchThreshold"],
                expected
            );
        }
        assert_eq!(origin("pixelMatchThreshold", None, None, None), "Default");
        assert_eq!(
            origin("pixelMatchThreshold", Some(&workspace), None, None),
            "Workspace"
        );
        assert_eq!(
            origin("pixelMatchThreshold", Some(&workspace), Some(&suite), None),
            "Suite"
        );
        assert_eq!(
            origin(
                "pixelMatchThreshold",
                Some(&workspace),
                Some(&suite),
                Some(&suite["scenarios"][0])
            ),
            "Scenario"
        );
    }

    #[test]
    fn lazy_load_fields_support_all_scopes_and_effective_values() {
        for scope in [Scope::Workspace, Scope::Suite, Scope::Scenario] {
            let fields = fields(&scope).unwrap();
            for (key, minimum) in [("lazyLoadScrollDelay", 0), ("lazyLoadTimeout", 1)] {
                let field = fields.iter().find(|field| field.key == key).unwrap();
                assert_eq!(field.kind, FieldKind::Number { integer: true });
                assert!(field.label.ends_with("(ms)"));
                assert!(!field.required);
                assert!(!field.description.is_empty());
                assert_eq!(field.parse("250").unwrap(), json!(250));
                assert!(field.parse("0.5").is_err());
                assert_eq!(field.stepped_value(&json!(250), true).unwrap(), json!(350));
                assert_eq!(
                    field.stepped_value(&json!(0), false).unwrap(),
                    json!(minimum)
                );
            }
        }
        let workspace = json!({"lazyLoadScrollDelay": 300, "lazyLoadTimeout": 30000});
        let suite = json!({"lazyLoadScrollDelay": 500, "lazyLoadTimeout": 40000, "scenarios": [
            {"url": "/", "lazyLoadScrollDelay": 0, "lazyLoadTimeout": 50000}, {"url": "/other"}
        ]});
        for (workspace, suite, scenario, delay, timeout) in [
            (None, None, None, 250, 20000),
            (Some(&workspace), None, None, 300, 30000),
            (Some(&workspace), Some(&suite), None, 500, 40000),
            (Some(&workspace), Some(&suite), Some(0), 0, 50000),
            (Some(&workspace), Some(&suite), Some(1), 500, 40000),
        ] {
            let effective = effective(workspace, suite, scenario).unwrap();
            assert_eq!(effective[effective_key("lazyLoadScrollDelay")], delay);
            assert_eq!(effective[effective_key("lazyLoadTimeout")], timeout);
        }
        for key in ["lazyLoadScrollDelay", "lazyLoadTimeout"] {
            assert_eq!(origin(key, None, None, None), "Default");
            assert_eq!(origin(key, Some(&workspace), None, None), "Workspace");
            assert_eq!(origin(key, Some(&workspace), Some(&suite), None), "Suite");
            assert_eq!(
                origin(
                    key,
                    Some(&workspace),
                    Some(&suite),
                    Some(&suite["scenarios"][0])
                ),
                "Scenario"
            );
        }
    }

    #[test]
    fn image_simplification_fields_support_all_scopes_steps_and_effective_values() {
        for scope in [Scope::Workspace, Scope::Suite, Scope::Scenario] {
            let fields = fields(&scope).unwrap();
            let field = |key: &str| fields.iter().find(|field| field.key == key).unwrap();

            let mode = field("imageSimplification");
            assert_eq!(
                mode.kind,
                FieldKind::Choice(vec!["none".into(), "pixelate".into()]),
                "{scope:?}"
            );
            assert_eq!(mode.label, "Image simplification");
            assert_eq!(mode.parse("pixelate").unwrap(), json!("pixelate"));
            assert!(mode.parse("off").is_err());

            let block = field("imageSimplificationBlockSize");
            assert_eq!(block.kind, FieldKind::Number { integer: true });
            assert_eq!(block.label, "Pixelation block size (px)");
            assert!(
                block.description.contains("2 to 16"),
                "{}",
                block.description
            );
            assert!(block.parse("4.5").is_err());
            for (current, increase, expected) in [
                (4, true, 6),
                (4, false, 2),
                (2, false, 2),
                (16, true, 16),
                (14, true, 16),
                (5, true, 6),
                (5, false, 4),
                (0, true, 2),
                (1, false, 2),
                (17, false, 16),
                (40, true, 16),
            ] {
                assert_eq!(
                    block.stepped_value(&json!(current), increase).unwrap(),
                    json!(expected),
                    "{current} {increase}"
                );
            }

            let minimum = field("imageSimplificationMinSize");
            assert_eq!(minimum.kind, FieldKind::Number { integer: true });
            assert_eq!(minimum.label, "Minimum image size (px)");
            assert_eq!(minimum.stepped_value(&json!(64), true).unwrap(), json!(65));
            assert_eq!(minimum.stepped_value(&json!(1), false).unwrap(), json!(1));
            assert_eq!(minimum.stepped_value(&json!(0), true).unwrap(), json!(1));

            let excluded = field("imageSimplificationExcludeSelectors");
            assert_eq!(excluded.kind, FieldKind::List { single: false });
            assert_eq!(excluded.label, "Excluded image selectors");
            let value = json!([".logo", "#hero img"]);
            assert_eq!(excluded.parse(&excluded.display(&value)).unwrap(), value);
            assert_eq!(excluded.parse("").unwrap(), json!([]));

            for field in [mode, block, minimum, excluded] {
                assert!(!field.required);
                // Searching the shared phrase finds every related field.
                assert!(
                    field
                        .description
                        .to_lowercase()
                        .contains("image simplification"),
                    "{}",
                    field.key
                );
            }
        }

        let workspace = json!({
            "imageSimplification": "pixelate",
            "imageSimplificationBlockSize": 8,
            "imageSimplificationExcludeSelectors": [".logo"]
        });
        let suite = json!({"imageSimplificationMinSize": 32, "imageSimplificationExcludeSelectors": [], "scenarios": [
            {"url": "/", "imageSimplification": "none", "imageSimplificationBlockSize": 2}, {"url": "/other"}
        ]});
        for (workspace, suite, scenario, expected) in [
            (None, None, None, json!(["none", 4, 64, []])),
            (
                Some(&workspace),
                None,
                None,
                json!(["pixelate", 8, 64, [".logo"]]),
            ),
            (
                Some(&workspace),
                Some(&suite),
                None,
                json!(["pixelate", 8, 32, []]),
            ),
            (
                Some(&workspace),
                Some(&suite),
                Some(0),
                json!(["none", 2, 32, []]),
            ),
            (
                Some(&workspace),
                Some(&suite),
                Some(1),
                json!(["pixelate", 8, 32, []]),
            ),
        ] {
            let effective = effective(workspace, suite, scenario).unwrap();
            assert_eq!(
                json!([
                    effective[effective_key("imageSimplification")],
                    effective[effective_key("imageSimplificationBlockSize")],
                    effective[effective_key("imageSimplificationMinSize")],
                    effective[effective_key("imageSimplificationExcludeSelectors")],
                ]),
                expected
            );
        }
        let scenario = &suite["scenarios"][0];
        for (key, expected) in [
            ("imageSimplification", "Scenario"),
            ("imageSimplificationBlockSize", "Scenario"),
            ("imageSimplificationMinSize", "Suite"),
            ("imageSimplificationExcludeSelectors", "Suite"),
        ] {
            assert_eq!(origin(key, None, None, None), "Default");
            assert_eq!(
                origin(key, Some(&workspace), Some(&suite), Some(scenario)),
                expected
            );
        }
        assert_eq!(
            origin(
                "imageSimplificationBlockSize",
                Some(&workspace),
                Some(&suite),
                None
            ),
            "Workspace"
        );
    }

    #[test]
    fn numeric_steps_respect_field_ranges_and_types() {
        let fields = fields(&Scope::Suite).unwrap();
        let workers = fields
            .iter()
            .find(|field| field.key == "asyncCaptureLimit")
            .unwrap();
        assert_eq!(workers.stepped_value(&json!(5), true).unwrap(), json!(6));
        assert_eq!(workers.stepped_value(&json!(1), false).unwrap(), json!(1));
        let threshold = fields
            .iter()
            .find(|field| field.key == "misMatchThreshold")
            .unwrap();
        assert_eq!(
            threshold.stepped_value(&json!(0.1), true).unwrap(),
            json!(0.2)
        );
        assert_eq!(
            threshold.stepped_value(&json!(100), true).unwrap(),
            json!(100.0)
        );
        assert_eq!(
            threshold.stepped_value(&json!(0), false).unwrap(),
            json!(0.0)
        );
        let text = fields.iter().find(|field| field.key == "id").unwrap();
        assert!(text.stepped_value(&json!("id"), true).is_err());
    }
    use crate::desktop::document::{ACTIONS, action_template};

    #[test]
    fn schema_covers_workspace_suite_scenario_and_viewports() {
        let suite: TestSuite =
            serde_json::from_value(json!({"scenarios": [], "displayName": "Suite label"})).unwrap();
        let scenario: TestScenario =
            serde_json::from_value(json!({"url": "https://example.com", "selector": "#content"}))
                .unwrap();
        for (scope, serialized) in [
            (
                Scope::Workspace,
                serde_json::to_value(WorkspaceConfig::default()).unwrap(),
            ),
            (Scope::Suite, serde_json::to_value(suite).unwrap()),
            (Scope::Scenario, serde_json::to_value(scenario).unwrap()),
        ] {
            let expected = serialized
                .as_object()
                .unwrap()
                .keys()
                .filter(|key| !matches!(key.as_str(), "scenarios" | "actions" | "testDate"))
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            let actual = fields(&scope)
                .unwrap()
                .into_iter()
                .map(|field| field.key)
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(actual, expected, "{scope:?}");
        }
        let scenarios = fields(&Scope::Scenario).unwrap();
        assert!(
            fields(&Scope::Suite)
                .unwrap()
                .iter()
                .any(|field| field.key == "displayName"
                    && !field.required
                    && field.kind == FieldKind::Text)
        );
        assert!(
            scenarios
                .iter()
                .any(|field| field.key == "url" && field.required)
        );
        assert!(scenarios.iter().any(|field| field.key == "restore"
            && matches!(field.kind, FieldKind::List { single: true })));
        assert_eq!(fields(&Scope::Viewport).unwrap().len(), 3);
    }

    #[test]
    fn screenshot_selector_fields_are_optional_text() {
        for scope in [Scope::Scenario, Scope::Action("save".into())] {
            let fields = fields(&scope).unwrap();
            let selector = fields.iter().find(|field| field.key == "selector").unwrap();
            assert_eq!(selector.kind, FieldKind::Text, "{scope:?}");
            assert!(!selector.required, "{scope:?}");
            assert!(selector.description.contains("CSS selector"), "{scope:?}");
            assert_eq!(selector.parse("#content").unwrap(), json!("#content"));
        }
    }

    #[test]
    fn all_action_properties_have_guided_fields() {
        for action in ACTIONS {
            let fields = fields(&Scope::Action(action.to_string())).unwrap();
            assert!(
                fields
                    .iter()
                    .any(|field| field.key == *action && field.required),
                "{action}"
            );
            assert_eq!(
                fields.iter().any(|field| field.key == "frame"),
                !matches!(*action, "switchTab" | "closeTab" | "navigate" | "dialog"),
                "{action}"
            );
            let value = action_template(action).unwrap();
            for (key, value) in value.as_object().unwrap() {
                let field = fields.iter().find(|field| field.key == *key).unwrap();
                assert_eq!(
                    field.parse(&field.display(value)).unwrap(),
                    *value,
                    "{action}.{key}"
                );
            }
        }
        assert!(fields(&Scope::Action("unknown".into())).is_err());
    }

    #[test]
    fn switch_tab_fields_use_typed_choices_and_url_help() {
        let fields = fields(&Scope::Action("switchTab".into())).unwrap();
        let selection = fields
            .iter()
            .find(|field| field.key == "switchTab")
            .unwrap();
        assert_eq!(
            selection.kind,
            FieldKind::Choice(vec!["new".into(), "previous".into(), "original".into()])
        );
        assert!(selection.required);
        let url = fields.iter().find(|field| field.key == "url").unwrap();
        assert_eq!(url.kind, FieldKind::Text);
        assert!(url.description.contains("switchTab: new"));
        assert!(!url.required);
    }

    #[test]
    fn focused_key_fields_do_not_select_the_press_schema() {
        let fields = fields(&Scope::Action("key".into())).unwrap();
        assert!(
            fields
                .iter()
                .any(|field| field.key == "key" && field.required)
        );
        assert!(!fields.iter().any(|field| field.key == "press"));
        let click = super::fields(&Scope::Action("click".into())).unwrap();
        for key in ["button", "clickCount", "modifiers", "position"] {
            assert!(click.iter().any(|field| field.key == key));
        }
    }

    #[test]
    fn explicit_empty_text_false_and_arrays_round_trip() {
        for (key, value) in [
            ("cookiePath", json!("")),
            ("snapshotTitle", json!("")),
            ("snapshotHeading", json!("")),
            (
                "snapshotSubheading",
                json!("**Staging** regression results"),
            ),
            ("debug", json!(false)),
            ("hideSelectors", json!([])),
            ("state", json!(["one", "two"])),
        ] {
            let field = fields(&Scope::Workspace)
                .unwrap()
                .into_iter()
                .find(|field| field.key == key)
                .unwrap();
            assert_eq!(field.parse(&field.display(&value)).unwrap(), value);
        }
        let wait = fields(&Scope::Action("wait".into()))
            .unwrap()
            .into_iter()
            .find(|field| field.key == "wait")
            .unwrap();
        assert!(wait.parse("-1").is_err());
        assert_eq!(wait.parse("#ready").unwrap(), "#ready");
    }

    #[test]
    fn effective_settings_use_the_real_cascade_and_restore_merge() {
        let workspace = json!({"bypassCsp": true, "state": "signed-in", "elementTimeout": 400});
        let suite = json!({"bypassCsp": false, "scenarios": [{"url": "https://example.com", "restore": "checkout"}]});
        let resolved = effective(Some(&workspace), Some(&suite), Some(0)).unwrap();
        assert_eq!(resolved["bypassCsp"], false);
        assert_eq!(resolved["elementTimeoutMs"], 400);
        assert_eq!(resolved["state"], json!(["signed-in", "checkout"]));
        assert_eq!(
            origin(
                "bypassCsp",
                Some(&workspace),
                Some(&suite),
                Some(&suite["scenarios"][0])
            ),
            "Suite"
        );
        assert_eq!(
            origin(
                "state",
                Some(&workspace),
                Some(&suite),
                Some(&suite["scenarios"][0])
            ),
            "State + restore"
        );
    }

    #[test]
    fn run_retention_fields_show_inherited_defaults_and_positive_steps() {
        let workspace = json!({"autoCleanRuns": true, "keepTestRuns": 3});
        let suite = json!({"autoCleanRuns": false, "scenarios": []});
        let resolved = effective(Some(&workspace), Some(&suite), None).unwrap();
        assert_eq!(resolved["autoCleanRuns"], false);
        assert_eq!(resolved["keepTestRuns"], 3);
        assert_eq!(effective(None, None, None).unwrap()["keepTestRuns"], 1);
        for scope in [Scope::Workspace, Scope::Suite] {
            let fields = fields(&scope).unwrap();
            assert!(
                fields
                    .iter()
                    .any(|field| field.key == "autoCleanRuns" && field.kind == FieldKind::Boolean)
            );
            let count = fields
                .iter()
                .find(|field| field.key == "keepTestRuns")
                .unwrap();
            assert_eq!(count.stepped_value(&json!(1), false).unwrap(), 1);
            assert_eq!(count.stepped_value(&json!(1), true).unwrap(), 2);
        }
        assert!(
            !fields(&Scope::Scenario)
                .unwrap()
                .iter()
                .any(|field| field.key == "autoCleanRuns")
        );
    }
}
