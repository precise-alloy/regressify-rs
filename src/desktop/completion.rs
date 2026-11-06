//! Local completions use the same schema as guided fields and parse incomplete source.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

use anyhow::Result;
use serde_json::Value;
use tree_sitter::{Node, Parser};

use super::{
    document::{ACTIONS, DocumentKind, Format, MAX_DOCUMENT_BYTES},
    schema, validation,
};

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
    pub description: String,
    pub replacement: Range<usize>,
    pub text: String,
    pub property: bool,
}

#[derive(Clone, Debug)]
pub struct Hint {
    pub range: Range<usize>,
    pub text: String,
}

struct SourceField {
    path: Vec<Segment>,
    key: String,
    key_range: Range<usize>,
    value_range: Option<Range<usize>>,
    properties: BTreeSet<String>,
}

#[derive(Default)]
pub struct SourceIndex {
    ranges: BTreeMap<String, Range<usize>>,
    fields: Vec<SourceField>,
    excluded: Vec<Range<usize>>,
    line_ends: Vec<usize>,
}

impl SourceIndex {
    pub fn new(format: Format, source: &str) -> Self {
        Self::parse(format, source).0
    }

    fn parse(format: Format, source: &str) -> (Self, Option<tree_sitter::Tree>) {
        let mut index = Self::default();
        if source.len() > MAX_DOCUMENT_BYTES {
            return (index, None);
        }
        let mut parser = Parser::new();
        let language = match format {
            Format::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Format::Json => tree_sitter_json::LANGUAGE.into(),
            Format::Text => return (index, None),
        };
        let mut offset = 0;
        index.line_ends = source
            .split_inclusive('\n')
            .map(|line| {
                let end = offset + line.trim_end_matches(['\r', '\n']).len();
                offset += line.len();
                end
            })
            .collect();
        if parser.set_language(&language).is_ok()
            && let Some(tree) = parser.parse(source, None)
        {
            index.visit(tree.root_node(), source, "", &[], 0);
            return (index, Some(tree));
        }
        (index, None)
    }

    pub fn range(&self, pointer: &str) -> Range<usize> {
        let mut parent = pointer;
        loop {
            if let Some(range) = self.ranges.get(parent) {
                if parent == pointer {
                    return range.clone();
                }
                let line = self.line_ends.partition_point(|end| *end < range.start);
                return range.start
                    ..range
                        .end
                        .min(self.line_ends.get(line).copied().unwrap_or(range.end));
            }
            match parent.rsplit_once('/') {
                Some((prefix, _)) => parent = prefix,
                None => return 0..0,
            }
        }
    }

    fn visit(
        &mut self,
        node: Node<'_>,
        source: &str,
        pointer: &str,
        path: &[Segment],
        depth: usize,
    ) {
        if depth > 128 {
            return;
        }
        if matches!(node.kind(), "comment" | "block_scalar") {
            self.excluded.push(node.byte_range());
            return;
        }
        self.ranges
            .entry(pointer.into())
            .or_insert_with(|| node.byte_range());
        let mut cursor = node.walk();
        self.excluded.extend(
            node.named_children(&mut cursor)
                .filter(|child| child.kind() == "comment")
                .map(|comment| comment.byte_range()),
        );
        if matches!(node.kind(), "object" | "block_mapping" | "flow_mapping") {
            let pairs = node
                .named_children(&mut cursor)
                .filter(|child| matches!(child.kind(), "pair" | "block_mapping_pair" | "flow_pair"))
                .collect::<Vec<_>>();
            let properties = pairs
                .iter()
                .filter_map(|pair| scalar(pair.child_by_field_name("key")?, source))
                .collect::<BTreeSet<_>>();
            for pair in pairs {
                let mut pair_cursor = pair.walk();
                self.excluded.extend(
                    pair.named_children(&mut pair_cursor)
                        .filter(|child| child.kind() == "comment")
                        .map(|comment| comment.byte_range()),
                );
                let Some(key_node) = pair.child_by_field_name("key") else {
                    continue;
                };
                let Some(key) = scalar(key_node, source) else {
                    continue;
                };
                let pointer = format!("{pointer}/{}", validation::escape_pointer(&key));
                let mut nested = path.to_vec();
                nested.push(Segment::Property(key.clone()));
                let value = pair.child_by_field_name("value");
                self.ranges.insert(pointer.clone(), key_node.byte_range());
                self.fields.push(SourceField {
                    path: path.to_vec(),
                    key,
                    key_range: key_node.byte_range(),
                    value_range: value.map(|value| value.byte_range()),
                    properties: properties.clone(),
                });
                if let Some(value) = value {
                    self.visit(value, source, &pointer, &nested, depth + 1);
                }
            }
        } else if matches!(node.kind(), "array" | "block_sequence" | "flow_sequence") {
            let mut nested = path.to_vec();
            nested.push(Segment::Item);
            for (index, item) in node
                .named_children(&mut cursor)
                .filter(|node| node.kind() != "comment")
                .enumerate()
            {
                self.visit(
                    item,
                    source,
                    &format!("{pointer}/{index}"),
                    &nested,
                    depth + 1,
                );
            }
        } else {
            for child in node.named_children(&mut cursor) {
                self.visit(child, source, pointer, path, depth + 1);
            }
        }
    }
}

fn action_viewport_values(
    workspace: &super::workspace::Workspace,
    format: Format,
    probe: &str,
    marker: &str,
) -> Result<Vec<crate::config::viewports::ViewportDef>> {
    let mut value = format.parse(probe)?;
    let mut selected = None;
    if let Some(scenarios) = value["scenarios"].as_array_mut() {
        for (index, scenario) in scenarios.iter_mut().enumerate() {
            if let Some(actions) = scenario["actions"].as_array_mut() {
                for action in actions {
                    let filter = &action["viewportNames"];
                    if filter.as_str() == Some(marker)
                        || filter.as_array().is_some_and(|names| {
                            names.iter().any(|name| name.as_str() == Some(marker))
                        })
                    {
                        selected = Some(index);
                    }
                }
            }
            scenario
                .as_object_mut()
                .map(|scenario| scenario.remove("actions"));
        }
    }
    let Some(index) = selected else {
        return Ok(Vec::new());
    };
    let suite: crate::types::TestSuite = serde_json::from_value(value)?;
    let config = crate::config::load_workspace_config(&workspace.root)?;
    let resolved = crate::config::resolve_config(&config, &suite, &suite.scenarios[index]);
    let all = workspace.viewport_choices(
        std::path::Path::new(&resolved.viewports_path),
        resolved.viewports_path_is_explicit,
    )?;
    schema::action_viewport_choices(&serde_json::to_value(resolved)?, &all)
}

pub struct CompletionCatalog {
    schemas: Vec<(DocumentKind, &'static Value)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Property(String),
    Item,
}

struct Location {
    path: Vec<Segment>,
    properties: BTreeSet<String>,
    property: bool,
}

impl CompletionCatalog {
    pub fn new() -> Result<Self> {
        let mut schemas = Vec::new();
        for kind in [
            DocumentKind::Workspace,
            DocumentKind::Suite,
            DocumentKind::Viewports,
            DocumentKind::ReplacementProfiles,
        ] {
            if let Some(schema) = validation::schema(kind)? {
                schemas.push((kind, schema));
            }
        }
        Ok(Self { schemas })
    }

    pub fn hint(
        &self,
        kind: DocumentKind,
        format: Format,
        source: &str,
        offset: usize,
    ) -> Option<Hint> {
        if !source.is_char_boundary(offset) {
            return None;
        }
        let (_, root) = self
            .schemas
            .iter()
            .find(|(candidate, _)| *candidate == kind)?;
        let index = SourceIndex::new(format, source);
        if index.excluded.iter().any(|range| range.contains(&offset)) {
            return None;
        }
        let field = index
            .fields
            .iter()
            .filter(|field| {
                field.key_range.contains(&offset)
                    || field
                        .value_range
                        .as_ref()
                        .is_some_and(|range| range.contains(&offset))
            })
            .min_by_key(|field| {
                if field.key_range.contains(&offset) {
                    field.key_range.len()
                } else {
                    field.value_range.as_ref().map_or(usize::MAX, Range::len)
                }
            })?;
        let nodes = nodes_at(root, &field.path, &field.properties, true);
        let (definition, required) = nodes.iter().find_map(|node| {
            let definition = node["properties"].get(&field.key)?;
            let required = node["required"]
                .as_array()
                .is_some_and(|keys| keys.iter().any(|key| key == &field.key));
            Some((definition, required))
        })?;
        Some(Hint {
            range: if field.key_range.contains(&offset) {
                field.key_range.clone()
            } else {
                field.value_range.clone()?
            },
            text: format!(
                "{}\n{}; {}\n\n{}",
                field.key,
                if required { "Required" } else { "Optional" },
                validation::expected(definition, root),
                schema::documentation(definition, root)
            ),
        })
    }

    pub fn complete(
        &self,
        kind: DocumentKind,
        format: Format,
        source: &str,
        cursor: usize,
    ) -> Vec<Suggestion> {
        self.complete_with_workspace(kind, format, source, cursor, None)
    }

    pub(crate) fn complete_with_workspace(
        &self,
        kind: DocumentKind,
        format: Format,
        source: &str,
        cursor: usize,
        workspace: Option<&super::workspace::Workspace>,
    ) -> Vec<Suggestion> {
        if kind == DocumentKind::Data
            || source.len() > MAX_DOCUMENT_BYTES
            || !source.is_char_boundary(cursor)
        {
            return Vec::new();
        }
        let start = source[..cursor]
            .rfind(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .map_or(0, |offset| {
                offset + source[offset..].chars().next().map_or(0, char::len_utf8)
            });
        let end = source[cursor..]
            .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .map_or(source.len(), |offset| cursor + offset);
        let mut prefix = source[start..cursor].to_ascii_lowercase();
        let quoted = start
            .checked_sub(1)
            .and_then(|offset| source.as_bytes().get(offset))
            .copied()
            .filter(|quote| matches!(quote, b'\'' | b'"'));
        let mut replacement = if let Some(quote) = quoted {
            start - 1..end + usize::from(source.as_bytes().get(end) == Some(&quote))
        } else {
            start..end
        };
        if kind == DocumentKind::Suite && workspace.is_some() {
            let (index, tree) = SourceIndex::parse(format, source);
            let range = index.ranges.iter().find(|(pointer, range)| {
                let path = pointer.split('/').collect::<Vec<_>>();
                matches!(
                    path.as_slice(),
                    ["", "scenarios", _, "actions", _, "viewportNames", _]
                ) && range.start <= cursor
                    && cursor <= range.end
            }).map(|(_, range)| range).or_else(|| {
                index.fields.iter().find_map(|field| {
                    if field.key != "viewportNames" || !matches!(field.path.as_slice(),
                        [Segment::Property(scenarios), Segment::Item, Segment::Property(actions), Segment::Item]
                        if scenarios == "scenarios" && actions == "actions"
                    ) {
                        return None;
                    }
                    field.value_range.as_ref().filter(|range| range.start <= cursor && cursor <= range.end)
                })
            });
            if let Some(range) = range
                && let Some(tree) = tree
                && let Some(mut node) = tree
                    .root_node()
                    .descendant_for_byte_range(range.start, range.end)
            {
                if node.kind() == "block_sequence_item"
                    && let Some(value) = node.named_child(0)
                {
                    node = value;
                }
                let range = node.byte_range();
                if range.start <= cursor && cursor <= range.end {
                    let fragment = &source[range.start..cursor];
                    let value =
                        format
                            .parse(fragment)
                            .or_else(|error| match fragment.as_bytes().first() {
                                Some(quote @ (b'\'' | b'"')) => {
                                    format.parse(&format!("{fragment}{}", *quote as char))
                                }
                                _ => Err(error),
                            });
                    if let Ok(Value::String(value)) = value {
                        prefix = value.to_ascii_lowercase();
                        replacement = range;
                    }
                }
            }
        }
        let colon = source[replacement.end..]
            .trim_start_matches([' ', '\t'])
            .starts_with(':');
        let mut marker = "__regressify_completion__".to_string();
        while source.contains(&marker) {
            marker.push('_');
        }
        let mut parser = Parser::new();
        let language = match format {
            Format::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Format::Json => tree_sitter_json::LANGUAGE.into(),
            Format::Text => return Vec::new(),
        };
        if parser.set_language(&language).is_err() {
            return Vec::new();
        }
        // A temporary scalar makes the cursor discoverable even before a key has its colon.
        for property in [true, false] {
            let mut token = format!(
                "\"{marker}\"{}",
                if property && !colon { ": null" } else { "" }
            );
            if format == Format::Json
                && !colon
                && source[replacement.end..].trim_start().starts_with('"')
            {
                token.push(',');
            }
            let mut probe = source.to_string();
            probe.replace_range(replacement.clone(), &token);
            let Some(mut tree) = parser.parse(&probe, None) else {
                return Vec::new();
            };
            if format == Format::Json && tree.root_node().has_error() {
                let mut closing = Vec::new();
                collect_delimiters(tree.root_node(), &mut closing, 0);
                for delimiter in closing.into_iter().rev() {
                    probe.push_str(delimiter);
                }
                if let Some(completed) = parser.parse(&probe, None) {
                    tree = completed;
                }
            }
            let Some(location) = locate(tree.root_node(), &probe, &marker, &[], 0) else {
                continue;
            };
            if location.property != property {
                continue;
            }
            let Some((_, root)) = self
                .schemas
                .iter()
                .find(|(candidate, _)| *candidate == kind)
            else {
                return Vec::new();
            };
            let action_viewport = matches!(location.path.as_slice(),
                [Segment::Property(scenarios), Segment::Item, Segment::Property(actions), Segment::Item, Segment::Property(field)]
                | [Segment::Property(scenarios), Segment::Item, Segment::Property(actions), Segment::Item, Segment::Property(field), Segment::Item]
                if scenarios == "scenarios" && actions == "actions" && field == "viewportNames"
            );
            if property && action_viewport {
                continue;
            }
            let nodes = nodes_at(root, &location.path, &location.properties, property);
            if property {
                let mut fields = BTreeMap::new();
                let action_object = nodes.iter().any(|node| {
                    ACTIONS
                        .iter()
                        .any(|action| node["properties"].get(*action).is_some())
                });
                let no_action = !ACTIONS
                    .iter()
                    .any(|action| location.properties.contains(*action));
                for node in nodes {
                    if let Some(properties) = node["properties"].as_object() {
                        for (key, definition) in properties {
                            if key != "testDate"
                                && (!action_object || !no_action || ACTIONS.contains(&key.as_str()))
                            {
                                let required =
                                    node["required"].as_array().is_some_and(|required| {
                                        required.iter().any(|value| value == key)
                                    });
                                fields.entry(key).or_insert((definition, required));
                            }
                        }
                    }
                }
                let mut suggestions = fields
                    .into_iter()
                    .filter(|(key, _)| {
                        key.to_ascii_lowercase().starts_with(&prefix)
                            && !location.properties.contains(*key)
                    })
                    .map(|(name, (definition, required))| {
                        let key = if format == Format::Json || quoted.is_some() {
                            format!("\"{name}\"")
                        } else {
                            name.clone()
                        };
                        (
                            required,
                            Suggestion {
                                label: name.clone(),
                                detail: format!(
                                    "{}; {}",
                                    if required { "Required" } else { "Optional" },
                                    validation::expected(definition, root)
                                ),
                                description: schema::documentation(definition, root),
                                replacement: replacement.clone(),
                                text: format!("{key}{}", if colon { "" } else { ": " }),
                                property: true,
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                suggestions.sort_by(|left, right| {
                    right
                        .0
                        .cmp(&left.0)
                        .then_with(|| left.1.label.cmp(&right.1.label))
                });
                return suggestions
                    .into_iter()
                    .map(|(_, suggestion)| suggestion)
                    .collect();
            }
            if action_viewport && let Some(workspace) = workspace {
                return action_viewport_values(workspace, format, &probe, &marker)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|viewport| viewport.label.to_ascii_lowercase().starts_with(&prefix))
                    .map(|viewport| Suggestion {
                        text: serde_json::json!(viewport.label).to_string(),
                        label: viewport.label,
                        detail: format!("{} x {}", viewport.width, viewport.height),
                        description: "Effective scenario viewport".into(),
                        replacement: replacement.clone(),
                        property: false,
                    })
                    .collect();
            }
            let mut suggestions = Vec::new();
            let mut seen = BTreeSet::new();
            for node in nodes {
                let values = if let Some(choices) = node["enum"].as_array() {
                    choices.clone()
                } else if node["type"] == "boolean"
                    || node["type"]
                        .as_array()
                        .is_some_and(|types| types.iter().any(|kind| kind == "boolean"))
                {
                    vec![Value::Bool(false), Value::Bool(true)]
                } else if node["type"] == "null" {
                    Vec::new()
                } else {
                    schema::examples(node, root)
                };
                for value in values {
                    let text = value.to_string();
                    let label = value.as_str().unwrap_or(&text).to_string();
                    if (prefix.is_empty() || label.to_ascii_lowercase().starts_with(&prefix))
                        && seen.insert(text.clone())
                    {
                        suggestions.push(Suggestion {
                            label: if value.is_object() {
                                "Example object".into()
                            } else if value.is_array() {
                                "Example list".into()
                            } else {
                                label
                            },
                            text,
                            detail: validation::expected(node, root),
                            description: schema::documentation(node, root),
                            replacement: replacement.clone(),
                            property: false,
                        });
                    }
                }
            }
            return suggestions;
        }
        Vec::new()
    }
}

fn nodes_at<'schema>(
    root: &'schema Value,
    path: &[Segment],
    properties: &BTreeSet<String>,
    property: bool,
) -> Vec<&'schema Value> {
    let selected = ACTIONS.iter().find(|action| properties.contains(**action));
    let matches_action = |node: &&Value| {
        selected.is_none_or(|selected| {
            schema::action_discriminator(node).is_none_or(|name| name == *selected)
        })
    };
    let mut nodes = vec![root];
    for segment in path {
        nodes = nodes
            .into_iter()
            .flat_map(|node| schema::variants(node, root))
            .filter(matches_action)
            .filter_map(|node| match segment {
                // Maps such as `profiles` describe their entries with `additionalProperties`.
                Segment::Property(key) => node["properties"].get(key).or_else(|| {
                    node.get("additionalProperties")
                        .filter(|schema| schema.is_object())
                }),
                Segment::Item => node.get("items"),
            })
            .collect();
    }
    let nodes = nodes
        .into_iter()
        .flat_map(|node| {
            if !property
                && schema::annotation(node, root, "examples").is_some()
                && node.get("properties").is_none()
                && path
                    .last()
                    .is_some_and(|segment| matches!(segment, Segment::Property(_)))
            {
                vec![node]
            } else {
                schema::variants(node, root)
            }
        })
        .collect::<Vec<_>>();
    nodes.into_iter().filter(matches_action).collect()
}

fn scalar(node: Node<'_>, source: &str) -> Option<String> {
    serde_yaml::from_str::<String>(node.utf8_text(source.as_bytes()).ok()?).ok()
}

fn collect_delimiters(node: Node<'_>, closing: &mut Vec<&'static str>, depth: usize) {
    if depth > 128 || node.is_missing() {
        return;
    }
    match node.kind() {
        "{" => closing.push("}"),
        "[" => closing.push("]"),
        "}" | "]" if closing.last().copied() == Some(node.kind()) => {
            closing.pop();
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_delimiters(child, closing, depth + 1);
    }
}

fn locate(
    node: Node<'_>,
    source: &str,
    marker: &str,
    path: &[Segment],
    depth: usize,
) -> Option<Location> {
    if depth > 128 || matches!(node.kind(), "comment" | "block_scalar") {
        return None;
    }
    let mut cursor = node.walk();
    if matches!(node.kind(), "object" | "block_mapping" | "flow_mapping") {
        let pairs = node
            .named_children(&mut cursor)
            .filter(|child| matches!(child.kind(), "pair" | "block_mapping_pair" | "flow_pair"))
            .collect::<Vec<_>>();
        let properties = pairs
            .iter()
            .filter_map(|pair| scalar(pair.child_by_field_name("key")?, source))
            .filter(|key| key != marker)
            .collect::<BTreeSet<_>>();
        for pair in pairs {
            let Some(key) = pair
                .child_by_field_name("key")
                .and_then(|key| scalar(key, source))
            else {
                continue;
            };
            if key == marker {
                return Some(Location {
                    path: path.to_vec(),
                    properties,
                    property: true,
                });
            }
            if let Some(value) = pair.child_by_field_name("value") {
                let mut nested = path.to_vec();
                nested.push(Segment::Property(key));
                if let Some(mut location) = locate(value, source, marker, &nested, depth + 1) {
                    if !location.property
                        && (location.path.len() == nested.len()
                            || location.path.len() == nested.len() + 1)
                    {
                        location.properties = properties;
                    }
                    return Some(location);
                }
            }
        }
        return None;
    }
    if matches!(node.kind(), "array" | "block_sequence" | "flow_sequence") {
        let mut nested = path.to_vec();
        nested.push(Segment::Item);
        return node
            .named_children(&mut cursor)
            .find_map(|child| locate(child, source, marker, &nested, depth + 1));
    }
    if matches!(
        node.kind(),
        "string" | "double_quote_scalar" | "single_quote_scalar" | "plain_scalar"
    ) && scalar(node, source).as_deref() == Some(marker)
    {
        return Some(Location {
            path: path.to_vec(),
            properties: BTreeSet::new(),
            property: false,
        });
    }
    node.named_children(&mut cursor)
        .find_map(|child| locate(child, source, marker, path, depth + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestions(source: &str, format: Format) -> Vec<Suggestion> {
        let cursor = source.find('|').unwrap();
        let source = source.replacen('|', "", 1);
        CompletionCatalog::new()
            .unwrap()
            .complete(DocumentKind::Suite, format, &source, cursor)
    }

    #[test]
    fn yaml_properties_follow_suite_scenario_and_action_context() {
        assert!(
            suggestions("asyncC|", Format::Yaml)
                .iter()
                .any(|item| item.label == "asyncCaptureLimit")
        );
        let scenario = suggestions("scenarios:\n  - url: /\n    del|", Format::Yaml);
        assert_eq!(scenario[0].label, "delay");
        let action = suggestions(
            "scenarios:\n  - url: /\n    actions:\n      - cli|",
            Format::Yaml,
        );
        assert_eq!(action[0].label, "click");
        let properties = suggestions(
            "scenarios:\n- url: /\n  actions:\n  - click: '#pay'\n    fr|",
            Format::Yaml,
        );
        assert_eq!(properties[0].label, "frame");
        assert!(
            !suggestions("scenarios: []\n|", Format::Yaml)
                .iter()
                .any(|item| item.label == "scenarios")
        );
    }

    #[test]
    fn switch_tab_completes_in_yaml_and_json_without_frame_options() {
        let actions = suggestions("scenarios:\n- url: /\n  actions:\n  - swi|", Format::Yaml);
        assert!(actions.iter().any(|item| item.label == "switchTab"));
        for (source, format) in [
            (
                "scenarios:\n- url: /\n  actions:\n  - switchTab: |",
                Format::Yaml,
            ),
            (
                r#"{"scenarios":[{"url":"/","actions":[{"switchTab":|}]}]}"#,
                Format::Json,
            ),
        ] {
            let items = suggestions(source, format);
            for target in ["new", "previous", "original"] {
                assert!(
                    items
                        .iter()
                        .any(|item| item.text == format!("\"{target}\"")),
                    "{items:?}"
                );
            }
        }
        let fields = suggestions(
            "scenarios:\n- url: /\n  actions:\n  - switchTab: new\n    |",
            Format::Yaml,
        );
        assert!(fields.iter().any(|item| item.label == "url"));
        assert!(
            !fields
                .iter()
                .any(|item| item.label == "frame" || item.label == "click")
        );
    }

    #[test]
    fn expanded_actions_completion_distinguishes_focused_keys_and_navigation() {
        for (source, format) in [
            (
                "scenarios:\n- url: /\n  actions:\n  - key: Tab\n    |",
                Format::Yaml,
            ),
            (
                r#"{"scenarios":[{"url":"/","actions":[{"key":"Tab",|}]}]}"#,
                Format::Json,
            ),
        ] {
            let fields = suggestions(source, format);
            assert!(
                fields.iter().any(|item| item.label == "frame"),
                "{fields:?}"
            );
            assert!(
                !fields.iter().any(|item| item.label == "press"),
                "{fields:?}"
            );
        }
        let fields = suggestions(
            "scenarios:\n- url: /\n  actions:\n  - press: '#first'\n    |",
            Format::Yaml,
        );
        assert!(fields.iter().any(|item| item.label == "key"));
        for (source, format) in [
            (
                "scenarios:\n- url: /\n  actions:\n  - navigate: |",
                Format::Yaml,
            ),
            (
                r#"{"scenarios":[{"url":"/","actions":[{"navigate":|}]}]}"#,
                Format::Json,
            ),
        ] {
            let values = suggestions(source, format);
            for value in ["back", "forward", "reload"] {
                assert!(
                    values
                        .iter()
                        .any(|item| item.text == format!("\"{value}\"")),
                    "{values:?}"
                );
            }
        }
    }

    #[test]
    fn action_viewport_names_complete_only_effective_labels_without_replacing_neighbors() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("common")).unwrap();
        std::fs::write(directory.path().join("common/_viewports.yaml"), "- label: desktop\n  width: 1280\n  height: 720\n- label: mobile\n  width: 375\n  height: 667\n- label: tablet\n  width: 768\n  height: 1024\n- label: phone-wide\n  width: 500\n  height: 700\n- label: phone narrow\n  width: 300\n  height: 700\n").unwrap();
        let workspace = super::super::workspace::Workspace::open(directory.path()).unwrap();
        let catalog = CompletionCatalog::new().unwrap();
        for (format, source, expected, token) in [
            (
                Format::Yaml,
                "scenarios:\n- url: /\n  viewportNames: [mobile]\n  actions:\n  - click: '#toggle'\n    viewportNames: 'mo|' # retained\n",
                "mobile",
                "'mo'",
            ),
            (
                Format::Json,
                "{\"scenarios\":[{\"url\":\"/\",\"viewportNames\":[\"mobile\"],\"actions\":[{\"click\":\"#toggle\",\"viewportNames\":\"mo|\"}]}]}",
                "mobile",
                "\"mo\"",
            ),
            (
                Format::Yaml,
                "scenarios:\n- url: /\n  viewportNames: [phone-wide]\n  actions:\n  - click: '#toggle'\n    viewportNames: 'phone-w|' # retained\n",
                "phone-wide",
                "'phone-w'",
            ),
            (
                Format::Json,
                "{\"scenarios\":[{\"url\":\"/\",\"viewportNames\":[\"phone narrow\"],\"actions\":[{\"click\":\"#toggle\",\"viewportNames\":\"phone n|\"}]}]}",
                "phone narrow",
                "\"phone n\"",
            ),
            (
                Format::Yaml,
                "scenarios:\n- url: /\n  viewportNames: [mobile]\n  actions:\n  - click: '#toggle'\n    viewportNames: [\"mo|\"] # retained\n",
                "mobile",
                "\"mo\"",
            ),
            (
                Format::Json,
                "{\"scenarios\":[{\"url\":\"/\",\"viewportNames\":[\"mobile\"],\"actions\":[{\"click\":\"#toggle\",\"viewportNames\":[\"mo|\"]}]}]}",
                "mobile",
                "\"mo\"",
            ),
            (
                Format::Yaml,
                "scenarios:\n- url: /\n  viewportNames: [phone-wide]\n  actions:\n  - click: '#toggle'\n    viewportNames: [\"phone-w|\"] # retained\n",
                "phone-wide",
                "\"phone-w\"",
            ),
            (
                Format::Json,
                "{\"scenarios\":[{\"url\":\"/\",\"viewportNames\":[\"phone narrow\"],\"actions\":[{\"click\":\"#toggle\",\"viewportNames\":[\"phone n|\"]}]}]}",
                "phone narrow",
                "\"phone n\"",
            ),
            (
                Format::Yaml,
                "scenarios:\n- url: /\n  viewportNames: [phone-wide]\n  actions:\n  - click: '#toggle'\n    viewportNames:\n    - phone-w| # retained\n",
                "phone-wide",
                "phone-w",
            ),
        ] {
            let cursor = source.find('|').unwrap();
            let source = source.replace('|', "");
            let items = catalog.complete_with_workspace(
                DocumentKind::Suite,
                format,
                &source,
                cursor,
                Some(&workspace),
            );
            assert_eq!(items.len(), 1, "{items:?}");
            assert_eq!(items[0].label, expected);
            assert_eq!(&source[items[0].replacement.clone()], token);
            let mut edited = source.clone();
            edited.replace_range(items[0].replacement.clone(), &items[0].text);
            let expected_value =
                if format.parse(&source).unwrap()["scenarios"][0]["actions"][0]["viewportNames"]
                    .is_string()
                {
                    serde_json::json!(expected)
                } else {
                    serde_json::json!([expected])
                };
            assert_eq!(
                format.parse(&edited).unwrap()["scenarios"][0]["actions"][0]["viewportNames"],
                expected_value
            );
            assert!(edited.contains("#toggle"));
            if format == Format::Yaml {
                assert!(edited.ends_with("# retained\n"));
            }
        }
    }

    #[test]
    fn source_ranges_unclosed_viewport_quotes_preserve_following_lines() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("common")).unwrap();
        std::fs::write(
            directory.path().join("common/_viewports.yaml"),
            "- label: mobile\n  width: 375\n  height: 667\n",
        )
        .unwrap();
        let workspace = super::super::workspace::Workspace::open(directory.path()).unwrap();
        let catalog = CompletionCatalog::new().unwrap();
        for (format, source, token) in [
            (
                Format::Yaml,
                "scenarios:\n- url: /\n  viewportNames: [mobile]\n  actions:\n  - click: '#toggle'\n    viewportNames: 'mo|\n  - click: '#next'\n",
                "'mo",
            ),
            (
                Format::Yaml,
                "scenarios:\r\n- url: /\r\n  viewportNames: [mobile]\r\n  actions:\r\n  - click: \"#toggle\"\r\n    viewportNames: \"mo|\r\n  - click: \"#next\"\r\n",
                "\"mo",
            ),
            (
                Format::Json,
                "{\n\"scenarios\":[{\"url\":\"/\",\"viewportNames\":[\"mobile\"],\"actions\":[\n{\"viewportNames\":\"mo|\n,\"click\":\"#next\"}]}]}\n",
                "\"mo",
            ),
        ] {
            let cursor = source.find('|').unwrap();
            let source = source.replacen('|', "", 1);
            let items = catalog.complete_with_workspace(
                DocumentKind::Suite,
                format,
                &source,
                cursor,
                Some(&workspace),
            );
            let item = items
                .iter()
                .find(|item| item.label == "mobile")
                .unwrap_or_else(|| panic!("Missing viewport completion for {format:?}: {items:?}"));
            assert_eq!(&source[item.replacement.clone()], token, "{format:?}");
            let mut edited = source.clone();
            edited.replace_range(item.replacement.clone(), &item.text);
            assert_eq!(
                edited,
                format!(
                    "{}\"mobile\"{}",
                    &source[..cursor - token.len()],
                    &source[cursor..]
                )
            );
            let value = format.parse(&edited).unwrap();
            assert_eq!(
                value["scenarios"][0]["actions"][0]["viewportNames"],
                "mobile"
            );
            assert!(edited.contains("#next"));
        }
    }

    #[test]
    fn source_ranges_multiline_viewport_quotes_never_consume_following_content() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("common")).unwrap();
        std::fs::write(
            directory.path().join("common/_viewports.yaml"),
            "- label: mobile\n  width: 375\n  height: 667\n",
        )
        .unwrap();
        let workspace = super::super::workspace::Workspace::open(directory.path()).unwrap();
        let catalog = CompletionCatalog::new().unwrap();
        for quote in ['\'', '"'] {
            for indent in ["    ", "      ", "        "] {
                for following in [
                    format!("label: {quote}next{quote}"),
                    format!("- click: {quote}#next{quote}"),
                    format!("click: {quote}#next{quote}"),
                ] {
                    let source = format!(
                        "scenarios:\n- url: /\n  viewportNames: [mobile]\n  actions:\n  - click: '#toggle'\n    viewportNames: {quote}mo\n{indent}{following}\n"
                    );
                    let cursor = source.find(&format!("{quote}mo\n")).unwrap() + 3;
                    let items = catalog.complete_with_workspace(
                        DocumentKind::Suite,
                        Format::Yaml,
                        &source,
                        cursor,
                        Some(&workspace),
                    );
                    assert!(
                        items.iter().all(|item| item.replacement.end <= cursor),
                        "Completion consumed a following line: {items:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn generated_schema_completes_nested_objects_lists_and_navigation_choices() {
        for source in [
            "scenarios: []\nbasicAuth:\n  ori|",
            "scenarios: []\nbasicAuth:\n  - ori|",
            "scenarios:\n  - url: /\n    basicAuth:\n      - ori|",
        ] {
            let items = suggestions(source, Format::Yaml);
            let origin = items
                .iter()
                .find(|item| item.label == "origin")
                .unwrap_or_else(|| panic!("{source}: {items:?}"));
            assert!(origin.detail.contains("Required"));
            assert!(origin.description.contains("https://example.com"));
        }
        assert!(
            suggestions(
                "scenarios: []\nurlReplacements:\n  - ref: old\n    te|",
                Format::Yaml
            )
            .iter()
            .any(|item| item.label == "test")
        );
        let navigation = suggestions(
            "scenarios:\n  - url: /\n    engineOptions:\n      gotoParameters:\n        waitUntil: dom|",
            Format::Yaml,
        );
        assert_eq!(navigation[0].text, "\"domcontentloaded\"");
        let nested = suggestions(
            "{\"scenarios\":[],\"basicAuth\":[{\"ori|\":\"\"}]}",
            Format::Json,
        );
        assert_eq!(nested[0].label, "origin");
    }

    #[test]
    fn value_examples_are_valid_and_required_action_fields_are_prioritized() {
        for format in [Format::Yaml, Format::Json] {
            let source = if format == Format::Yaml {
                "scenarios: []\nbasicAuth: |"
            } else {
                "{\"scenarios\":[],\"basicAuth\":|}"
            };
            let items = suggestions(source, format);
            let item = items
                .iter()
                .find(|item| item.label == "Example object")
                .unwrap();
            let mut edited = source.replace('|', "");
            edited.replace_range(item.replacement.clone(), &item.text);
            super::super::document::validate_value(
                DocumentKind::Suite,
                &format.parse(&edited).unwrap(),
            )
            .unwrap();
        }
        let items = suggestions(
            "scenarios:\n- url: /\n  actions:\n  - press: '#search'\n    |",
            Format::Yaml,
        );
        assert_eq!(items[0].label, "key");
        assert!(items[0].description.contains("Ctrl+A"));
        assert!(
            !items
                .iter()
                .any(|item| item.label == "click" || item.label == "value")
        );
        let items = suggestions(
            "scenarios: []\nbasicAuth:\n  username: qa\n  |",
            Format::Yaml,
        );
        assert!(!items.iter().any(|item| item.label == "username"));
        assert!(items.iter().any(|item| item.label == "password"));
    }

    #[test]
    fn source_locations_follow_nested_indices_quoted_keys_unicode_and_missing_fields() {
        let source = "# \u{1f30d}\r\nscenarios:\r\n- url: /\r\n  actions:\r\n    # one step\r\n    - press: '#search'\r\nbasicAuth:\r\n- username: qa\r\n  password: hidden\r\n'other/key~': true\r\n";
        let index = SourceIndex::new(Format::Yaml, source);
        assert_eq!(&source[index.range("/scenarios/0/url")], "url");
        assert!(source[index.range("/scenarios/0/actions/0/key")].contains("press:"));
        assert!(source[index.range("/basicAuth/0/origin")].contains("username:"));
        assert_eq!(&source[index.range("/other~1key~0")], "'other/key~'");
        let source = "{\"scenarios\":[{\"url\":\"/\",\"actions\":[{\"press\":\"#search\"}]}]}";
        let index = SourceIndex::new(Format::Json, source);
        assert_eq!(
            &source[index.range("/scenarios/0/actions/0/press")],
            "\"press\""
        );
        assert_eq!(
            &source[index.range("/scenarios/0/actions/0/key")],
            "{\"press\":\"#search\"}"
        );
    }

    #[test]
    fn source_validation_missing_fields_highlight_only_the_enclosing_header() {
        for (format, source, pointer, expected) in [
            (
                Format::Yaml,
                "debug: true\r\nheadless: true\r\n",
                "/scenarios",
                "debug: true",
            ),
            (
                Format::Yaml,
                "scenarios:\n- label: Missing URL\n  delay: 12\n  actions: []\n",
                "/scenarios/0/url",
                "- label: Missing URL",
            ),
            (
                Format::Json,
                "{\n  \"debug\": true,\n  \"headless\": true\n}\n",
                "/scenarios",
                "{",
            ),
        ] {
            let index = SourceIndex::new(format, source);
            assert_eq!(&source[index.range(pointer)], expected);
        }
    }

    #[test]
    fn hover_help_uses_schema_metadata_without_echoing_values_or_comment_contents() {
        let catalog = CompletionCatalog::new().unwrap();
        let source = "scenarios: []\nbasicAuth:\n  origin: https://example.com\n  username: private-user\n  password: private-password\n# password comment\n";
        for offset in [
            source.find("password:").unwrap(),
            source.find("private-password").unwrap(),
        ] {
            let hint = catalog
                .hint(DocumentKind::Suite, Format::Yaml, source, offset)
                .unwrap();
            assert!(hint.text.contains("Required"));
            assert!(hint.text.contains("${BASIC_AUTH_PASSWORD}"));
            assert!(!hint.text.contains("private-password"));
            assert!(!hint.text.contains("private-user"));
        }
        assert!(
            catalog
                .hint(
                    DocumentKind::Suite,
                    Format::Yaml,
                    source,
                    source.find("# password").unwrap() + 3
                )
                .is_none()
        );
        let source = "{\"scenarios\":[{\"url\":\"/\",\"engineOptions\":{\"gotoParameters\":{\"timeout\":1000}}}]}";
        let hint = catalog
            .hint(
                DocumentKind::Suite,
                Format::Json,
                source,
                source.find("timeout").unwrap(),
            )
            .unwrap();
        assert!(hint.text.contains("milliseconds"));
        assert_eq!(&source[hint.range], "\"timeout\"");
    }

    #[test]
    fn json_properties_and_values_have_safe_replacement_ranges() {
        let source = "{\"scenarios\":[{\"url\":\"/\", \"del|\": 12}]}";
        let items = suggestions(source, Format::Json);
        let item = items.iter().find(|item| item.label == "delay").unwrap();
        let mut edited = source.replace('|', "");
        edited.replace_range(item.replacement.clone(), &item.text);
        let parsed: serde_json::Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(parsed["scenarios"][0]["delay"], 12);
        assert_eq!(
            suggestions("{\"debug\": tr|}", Format::Json)[0].text,
            "true"
        );
        assert_eq!(
            suggestions("browser: chr|", Format::Yaml)[0].text,
            "\"chromium\""
        );
    }

    #[test]
    fn unfinished_json_and_yaml_flow_objects_keep_their_scope() {
        assert!(
            suggestions("{\n  \"deb|\"\n  \"scenarios\": []\n}", Format::Json)
                .iter()
                .any(|item| item.label == "debug")
        );
        assert_eq!(
            suggestions("{\n  \"debug\": fal|\n  \"scenarios\": []\n}", Format::Json)[0].text,
            "false"
        );
        for source in [
            "{\"scenarios\":[{\"url\":\"/\", \"del|",
            "{\"scenarios\":[{\"url\":\"/\", |}]}",
        ] {
            assert!(
                suggestions(source, Format::Json)
                    .iter()
                    .any(|item| item.label == "delay"),
                "{source}"
            );
        }
        assert!(
            suggestions("scenarios: [{url: '/', del|}]", Format::Yaml)
                .iter()
                .any(|item| item.label == "delay")
        );
        assert!(
            !suggestions("asyncCaptureLimit: 5\nasyncC|", Format::Yaml)
                .iter()
                .any(|item| item.label == "asyncCaptureLimit")
        );
    }

    #[test]
    fn replacement_profile_items_complete_and_explain_their_fields() {
        let catalog = CompletionCatalog::new().unwrap();
        let source = "profiles:\n  qa:\n    - ref: a\n      te";
        assert!(
            catalog
                .complete(
                    DocumentKind::ReplacementProfiles,
                    Format::Yaml,
                    source,
                    source.len()
                )
                .iter()
                .any(|item| item.label == "test")
        );
        let source = "profiles:\n  qa:\n    - script: x.cjs\n";
        let hint = catalog
            .hint(
                DocumentKind::ReplacementProfiles,
                Format::Yaml,
                source,
                source.find("script").unwrap(),
            )
            .unwrap();
        assert!(hint.text.contains("common"), "{}", hint.text);
    }

    #[test]
    fn workspace_and_viewport_fields_use_their_own_schema() {
        let catalog = CompletionCatalog::new().unwrap();
        assert!(
            catalog
                .complete(DocumentKind::Workspace, Format::Yaml, "asyncC", 6)
                .iter()
                .any(|item| item.label == "asyncCaptureLimit")
        );
        assert!(
            catalog
                .complete(DocumentKind::Viewports, Format::Yaml, "- wid", 5)
                .iter()
                .any(|item| item.label == "width")
        );
        assert!(
            catalog
                .complete(DocumentKind::Data, Format::Yaml, "", 0)
                .is_empty()
        );
        assert!(
            catalog
                .complete(DocumentKind::Suite, Format::Yaml, "\u{e9}", 1)
                .is_empty()
        );
    }

    #[test]
    fn source_assistance_does_not_insert_examples_inside_existing_scalars() {
        for (format, source) in [
            (Format::Yaml, "scenarios: []\nbasicAuth:\n  origin: https:|"),
            (Format::Yaml, "scenarios:\n- url: /\n  label: Home |"),
            (Format::Yaml, "scenarios:\n- url: /\n  label: 'Home |'"),
            (
                Format::Json,
                r#"{"scenarios":[{"url":"/","label":"Home |"}]}"#,
            ),
            (
                Format::Json,
                r#"{"scenarios":[],"basicAuth":{"origin":"https:|"} }"#,
            ),
        ] {
            let items = suggestions(source, format);
            assert!(
                items.is_empty(),
                "Unexpected mid-scalar examples: {items:?}"
            );
        }
    }

    #[test]
    fn comments_free_text_and_non_schema_objects_do_not_offer_properties() {
        for source in [
            "# del|",
            "scenarios:\n- url: del|",
            "scenarios:\n- url: '/'\n  label: \"del|\"",
            "unknown:\n  del|",
        ] {
            assert!(suggestions(source, Format::Yaml).is_empty(), "{source}");
        }
    }

    #[test]
    fn unicode_crlf_and_midword_edits_preserve_surrounding_source() {
        let source = "# \u{1f30d}\r\nscenarios:\r\n- url: '/'\r\n  de|lay: 12\r\n";
        let item = suggestions(source, Format::Yaml)
            .into_iter()
            .find(|item| item.label == "delay")
            .unwrap();
        let mut edited = source.replace('|', "");
        edited.replace_range(item.replacement, &item.text);
        assert_eq!(edited, source.replace('|', ""));
    }
}
