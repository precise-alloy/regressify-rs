//! Source-preserving document edits shared by guided forms and the source editor.

use std::{
    collections::{HashMap, VecDeque},
    ops::Range,
    path::Path,
    str::FromStr,
};

use anyhow::{Context, Result, anyhow, ensure};
use saphyr_parser::{Event, Parser as YamlParser, ScalarStyle, Span};
use serde::Deserialize;
use serde_json::{Value, json};
use yaml_edit::{AsYaml, Document as YamlDocument, Sequence, SyntaxKind, YamlFile, YamlNode};

use crate::{
    config::{
        WorkspaceConfig,
        replacements::{
            ReplacementProfileItem, ReplacementProfilesFile, validate_profile_items_syntax,
            validate_replacements,
        },
        viewports,
    },
    helpers::UniqueJsonValue,
    types::{Browser, TestSuite, UrlReplacement},
};

pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_HISTORY_BYTES: usize = 16 * 1024 * 1024;
const MAX_HISTORY_ENTRIES: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentKind {
    Workspace,
    Suite,
    Viewports,
    ReplacementProfiles,
    Data,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Yaml,
    Json,
    Text,
}

impl Format {
    pub fn for_path(path: &Path) -> Self {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some(extension) if extension.eq_ignore_ascii_case("json") => Self::Json,
            Some(extension)
                if extension.eq_ignore_ascii_case("yaml")
                    || extension.eq_ignore_ascii_case("yml") =>
            {
                Self::Yaml
            }
            _ => Self::Text,
        }
    }

    pub fn parse(self, source: &str) -> Result<Value> {
        ensure!(
            source.len() <= MAX_DOCUMENT_BYTES,
            "Document exceeds the 4 MiB editing limit"
        );
        match self {
            Self::Json => serde_json::from_str::<UniqueJsonValue>(source)
                .map(|value| value.0)
                .context("Invalid JSON"),
            Self::Yaml => serde_yaml::from_str(source).context("Invalid YAML"),
            Self::Text => {
                ensure!(is_text(source), "Binary content cannot be edited as text");
                Ok(Value::String(source.to_string()))
            }
        }
    }

    pub fn serialize(self, value: &Value) -> Result<String> {
        match self {
            Self::Json => Ok(format!("{}\n", serde_json::to_string_pretty(value)?)),
            Self::Yaml => Ok(serde_yaml::to_string(value)?),
            Self::Text => value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("Expected plain text")),
        }
    }
}

pub(super) fn is_text(source: &str) -> bool {
    !source.chars().any(|character| {
        character.is_control() && !matches!(character, '\t' | '\r' | '\n' | '\u{000c}')
    })
}

/// JSON-pointer paths avoid ambiguity for selectors and keys containing dots.
#[derive(Clone, Debug)]
pub enum Edit {
    Set {
        parent: String,
        key: String,
        value: Option<Value>,
    },
    Insert {
        path: String,
        index: usize,
        value: Value,
    },
    Remove {
        path: String,
        index: usize,
    },
    Move {
        path: String,
        from: usize,
        to: usize,
    },
}

#[derive(Debug)]
struct HistoryEntry {
    source: String,
    selection: Option<Range<usize>>,
}

#[cfg_attr(not(any(feature = "gui", test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceEditKind {
    Insert,
    Backspace,
    Delete,
}

#[derive(Debug)]
pub struct Document {
    pub kind: DocumentKind,
    pub format: Format,
    source: String,
    saved: String,
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
    undo_bytes: usize,
    redo_bytes: usize,
    source_selection: Option<Range<usize>>,
    source_edit_kind: Option<SourceEditKind>,
}

fn trim_history(
    history: &mut VecDeque<HistoryEntry>,
    bytes: &mut usize,
    other_entries: usize,
    other_bytes: usize,
) {
    while history.len() + other_entries > MAX_HISTORY_ENTRIES
        || *bytes + other_bytes > MAX_HISTORY_BYTES
    {
        let Some(entry) = history.pop_front() else {
            break;
        };
        *bytes -= entry.source.len();
    }
}

impl Document {
    pub fn new(kind: DocumentKind, format: Format, source: String) -> Result<Self> {
        ensure!(
            source.len() <= MAX_DOCUMENT_BYTES,
            "Document exceeds the 4 MiB editing limit"
        );
        Ok(Self {
            kind,
            format,
            saved: source.clone(),
            source,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            undo_bytes: 0,
            redo_bytes: 0,
            source_selection: None,
            source_edit_kind: None,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn saved_source(&self) -> &str {
        &self.saved
    }
    pub fn dirty(&self) -> bool {
        self.source != self.saved
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn value(&self) -> Result<Value> {
        self.parse_value(&self.source)
    }

    fn parse_value(&self, source: &str) -> Result<Value> {
        let value = self.format.parse(source)?;
        // The CLI parser distinguishes omitted YAML content from an explicit null.
        if self.kind == DocumentKind::Workspace
            && self.format == Format::Yaml
            && value.is_null()
            && serde_yaml::from_str::<WorkspaceConfig>(source).is_ok()
        {
            return Ok(json!({}));
        }
        Ok(value)
    }

    pub fn validate(&self) -> Result<()> {
        if self.format == Format::Text {
            ensure!(
                self.kind == DocumentKind::Data,
                "Configuration documents must use YAML or JSON"
            );
            ensure!(
                self.source.len() <= MAX_DOCUMENT_BYTES,
                "Document exceeds the 4 MiB editing limit"
            );
            ensure!(
                is_text(&self.source),
                "Binary content cannot be edited as text"
            );
            Ok(())
        } else {
            validate_value(self.kind, &self.value()?)
        }
    }

    /// Invalid drafts are editable, but validation remains a mandatory save gate.
    pub fn set_source(&mut self, source: String) -> Result<()> {
        ensure!(
            source.len() <= MAX_DOCUMENT_BYTES,
            "Document exceeds the 4 MiB editing limit"
        );
        self.break_source_history();
        self.replace_source(source, None, None);
        Ok(())
    }

    /// Native drafts may exceed the save limit; keep them dirty and undoable.
    #[cfg_attr(not(any(feature = "gui", test)), allow(dead_code))]
    pub(crate) fn set_source_draft(
        &mut self,
        source: String,
        selection: Range<usize>,
    ) -> Result<()> {
        ensure!(
            selection.start <= selection.end
                && source.is_char_boundary(selection.start)
                && source.is_char_boundary(selection.end),
            "Invalid source selection"
        );
        if source == self.source {
            self.set_source_selection(selection);
            return Ok(());
        }
        let kind = self.source_edit_kind(&source, &selection);
        self.replace_source(source, Some(selection), kind);
        Ok(())
    }

    #[cfg_attr(not(any(feature = "gui", test)), allow(dead_code))]
    pub(crate) fn source_selection(&self) -> Option<Range<usize>> {
        self.source_selection.clone()
    }

    #[cfg_attr(not(any(feature = "gui", test)), allow(dead_code))]
    pub(crate) fn set_source_selection(&mut self, selection: Range<usize>) {
        if self.source_selection.as_ref() != Some(&selection) {
            self.break_source_history();
            self.source_selection = Some(selection);
        }
    }

    pub(crate) fn break_source_history(&mut self) {
        self.source_edit_kind = None;
    }

    #[cfg_attr(not(any(feature = "gui", test)), allow(dead_code))]
    fn source_edit_kind(&self, source: &str, selection: &Range<usize>) -> Option<SourceEditKind> {
        let previous = self.source_selection.as_ref()?;
        if !previous.is_empty() || !selection.is_empty() {
            return None;
        }
        let before = previous.end;
        let after = selection.end;
        let single_character =
            |text: &str| !text.contains(['\r', '\n']) && text.chars().count() == 1;
        if source.len() > self.source.len()
            && after > before
            && source.get(..before) == self.source.get(..before)
            && source.get(after..) == self.source.get(before..)
            && source.get(before..after).is_some_and(single_character)
        {
            Some(SourceEditKind::Insert)
        } else if source.len() < self.source.len()
            && before > after
            && source.get(..after) == self.source.get(..after)
            && source.get(after..) == self.source.get(before..)
            && self.source.get(after..before).is_some_and(single_character)
        {
            Some(SourceEditKind::Backspace)
        } else if source.len() < self.source.len() && before == after {
            let end = before + self.source.len() - source.len();
            (source.get(..before) == self.source.get(..before)
                && source.get(after..) == self.source.get(end..)
                && self.source.get(before..end).is_some_and(single_character))
            .then_some(SourceEditKind::Delete)
        } else {
            None
        }
    }

    fn replace_source(
        &mut self,
        source: String,
        selection: Option<Range<usize>>,
        kind: Option<SourceEditKind>,
    ) {
        if source == self.source {
            return;
        }
        let previous = HistoryEntry {
            source: std::mem::replace(&mut self.source, source),
            selection: std::mem::replace(&mut self.source_selection, selection),
        };
        if kind.is_none() || kind != self.source_edit_kind || self.undo.is_empty() {
            self.undo_bytes += previous.source.len();
            self.undo.push_back(previous);
        }
        self.redo.clear();
        self.redo_bytes = 0;
        self.source_edit_kind = kind;
        trim_history(&mut self.undo, &mut self.undo_bytes, 0, 0);
    }

    pub fn undo(&mut self) {
        self.break_source_history();
        if let Some(previous) = self.undo.pop_back() {
            self.undo_bytes -= previous.source.len();
            let next = HistoryEntry {
                source: std::mem::replace(&mut self.source, previous.source),
                selection: std::mem::replace(&mut self.source_selection, previous.selection),
            };
            self.redo_bytes += next.source.len();
            self.redo.push_back(next);
            trim_history(
                &mut self.redo,
                &mut self.redo_bytes,
                self.undo.len(),
                self.undo_bytes,
            );
        }
    }

    pub fn redo(&mut self) {
        self.break_source_history();
        if let Some(next) = self.redo.pop_back() {
            self.redo_bytes -= next.source.len();
            let previous = HistoryEntry {
                source: std::mem::replace(&mut self.source, next.source),
                selection: std::mem::replace(&mut self.source_selection, next.selection),
            };
            self.undo_bytes += previous.source.len();
            self.undo.push_back(previous);
            trim_history(
                &mut self.undo,
                &mut self.undo_bytes,
                self.redo.len(),
                self.redo_bytes,
            );
        }
    }

    pub fn mark_saved(&mut self) {
        self.break_source_history();
        self.saved.clone_from(&self.source);
    }

    /// An asynchronous write may finish after a newer source edit was queued.
    pub fn acknowledge_saved(&mut self, source: String) {
        self.break_source_history();
        self.saved = source;
    }

    pub fn apply(&mut self, edit: Edit) -> Result<()> {
        let mut expected = self.value()?;
        apply_value(&mut expected, &edit)?;
        let source = match self.format {
            Format::Json => self.format.serialize(&expected)?,
            Format::Yaml => apply_yaml(&self.source, &edit)?,
            Format::Text => return Err(anyhow!("Plain text has no structured fields")),
        };
        // A CST editor must never silently change the meaning of unrelated YAML.
        ensure!(
            self.parse_value(&source)? == expected,
            "This YAML construct cannot be edited losslessly. Use the Source editor instead."
        );
        self.set_source(source)
    }

    /// A user command may need several structural edits but only one undo entry.
    pub fn apply_many(&mut self, edits: Vec<Edit>) -> Result<()> {
        let mut draft = Self::new(self.kind, self.format, self.source.clone())?;
        for edit in edits {
            draft.apply(edit)?;
        }
        self.set_source(draft.source)
    }
}

fn array_mut<'value>(value: &'value mut Value, path: &str) -> Result<&'value mut Vec<Value>> {
    value
        .pointer_mut(path)
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("Expected a list at '{path}'"))
}

fn apply_value(value: &mut Value, edit: &Edit) -> Result<()> {
    match edit {
        Edit::Set {
            parent,
            key,
            value: replacement,
        } => {
            let mapping = value
                .pointer_mut(parent)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| anyhow!("Expected an object at '{parent}'"))?;
            if let Some(replacement) = replacement {
                mapping.insert(key.clone(), replacement.clone());
            } else {
                mapping.remove(key);
            }
        }
        Edit::Insert {
            path,
            index,
            value: item,
        } => {
            let items = array_mut(value, path)?;
            ensure!(*index <= items.len(), "List insertion is out of bounds");
            items.insert(*index, item.clone());
        }
        Edit::Remove { path, index } => {
            let items = array_mut(value, path)?;
            ensure!(*index < items.len(), "List removal is out of bounds");
            items.remove(*index);
        }
        Edit::Move { path, from, to } => {
            let items = array_mut(value, path)?;
            ensure!(
                *from < items.len() && *to < items.len(),
                "List move is out of bounds"
            );
            let item = items.remove(*from);
            items.insert(*to, item);
        }
    }
    Ok(())
}

fn root_node(document: &YamlDocument) -> Result<YamlNode> {
    document
        .as_mapping()
        .map(YamlNode::Mapping)
        .or_else(|| document.as_sequence().map(YamlNode::Sequence))
        .or_else(|| document.as_scalar().map(YamlNode::Scalar))
        .ok_or_else(|| anyhow!("Unsupported YAML root; use the Source editor"))
}

fn yaml_node(value: &Value) -> Result<YamlNode> {
    // JSON flow nodes are valid YAML and retain their meaning at any indentation.
    // Existing CST nodes stay untouched; only newly inserted values use this form.
    root_node(&YamlDocument::from_str(&serde_json::to_string(value)?)?)
}

fn locate(root: YamlNode, path: &str) -> Result<YamlNode> {
    let mut node = root;
    for segment in path.split('/').skip(1) {
        let key = segment.replace("~1", "/").replace("~0", "~");
        node = match &node {
            YamlNode::Mapping(mapping) => mapping.get(key.as_str()),
            YamlNode::Sequence(sequence) => key.parse().ok().and_then(|index| sequence.get(index)),
            _ => None,
        }
        .ok_or_else(|| anyhow!("Cannot resolve '{path}' in YAML; use the Source editor"))?;
    }
    Ok(node)
}

fn apply_yaml(source: &str, edit: &Edit) -> Result<String> {
    let (mut protected, originals) = protect_plain_scalars(source)?;
    // Keep the stream wrapper: leading comments can belong to it, not the document.
    let mut file = YamlFile::from_str(&protected)?;
    if file.document().is_none() {
        if matches!(edit, Edit::Set { parent, value: None, .. } if parent.is_empty()) {
            // yaml-edit 0.3.5 terminates non-empty documentless streams in
            // ensure_document(); resetting an absent root key must stay a no-op.
            return Ok(source.to_string());
        }
        if matches!(edit, Edit::Set { value: Some(_), .. })
            && !protected.is_empty()
            && !protected.ends_with('\n')
        {
            // A new setting must not become part of an unterminated leading comment.
            protected.push_str(if protected.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            });
            file = YamlFile::from_str(&protected)?;
        }
    }
    let document = file.ensure_document();
    let root = root_node(&document)?;
    match edit {
        Edit::Set { parent, key, value } => {
            let node = locate(root, parent)?;
            let mapping = node
                .as_mapping()
                .ok_or_else(|| anyhow!("Expected a YAML mapping"))?;
            match value {
                Some(value) => {
                    mapping.set(key.as_str(), yaml_node(value)?);
                }
                None => {
                    // yaml-edit leaves the last entry's indentation behind, which can
                    // pull the following parent-level key into this nested mapping.
                    let trailing_indent = mapping.as_node().and_then(|syntax| {
                        let index = mapping.find_entry_index_by_key(key.as_str())?;
                        let children = syntax.children_with_tokens().collect::<Vec<_>>();
                        if children[index + 1..].iter().any(|child| {
                            child
                                .as_node()
                                .is_some_and(|node| node.kind() == SyntaxKind::MAPPING_ENTRY)
                        }) {
                            return None;
                        }
                        children
                            .get(index.checked_sub(1)?)
                            .and_then(|child| child.as_token())
                            .filter(|token| token.kind() == SyntaxKind::INDENT)
                            .cloned()
                    });
                    mapping.remove(key.as_str());
                    if let Some(indent) = trailing_indent {
                        indent.detach();
                    }
                }
            }
        }
        Edit::Insert { path, index, value } => {
            let node = locate(root, path)?;
            let sequence = node
                .as_sequence()
                .ok_or_else(|| anyhow!("Expected a YAML sequence"))?;
            insert_yaml_item(sequence, *index, yaml_node(value)?, &protected)?;
        }
        Edit::Remove { path, index } => {
            let node = locate(root, path)?;
            node.as_sequence()
                .ok_or_else(|| anyhow!("Expected a YAML sequence"))?
                .remove(*index);
        }
        Edit::Move { path, from, to } => {
            let node = locate(root, path)?;
            let sequence = node
                .as_sequence()
                .ok_or_else(|| anyhow!("Expected a YAML sequence"))?;
            let mut items = sequence.values().collect::<Vec<_>>();
            let item = items.remove(*from);
            items.insert(*to, item);
            // Keep the existing separators, including indentationless YAML lists.
            for (index, item) in items.into_iter().enumerate() {
                ensure!(sequence.set(index, item), "Unable to reorder YAML item");
            }
        }
    }
    restore_plain_scalars(&file.to_string(), &originals)
}

fn protect_plain_scalars(source: &str) -> Result<(String, HashMap<String, String>)> {
    let prefix = format!("regressify_scalar_{}_", uuid::Uuid::new_v4().simple());
    let mut originals = HashMap::new();
    let mut replacements = Vec::new();
    let offsets = yaml_source_offsets(source);
    // yaml-edit splits some legal plain scalars at embedded flow punctuation.
    // Use YAML 1.2 spans, not selector-specific text matching, to isolate them.
    for event in YamlParser::new_from_str(source) {
        let (event, span) = event.map_err(|error| anyhow!("Invalid YAML: {error}"))?;
        if let Event::Scalar(value, ScalarStyle::Plain, _, _) = event
            && value.contains(['[', ']', '{', '}', ',', '\'', '"'])
        {
            let range = scalar_byte_range(source, span, &offsets)?;
            let original = source
                .get(range.clone())
                .ok_or_else(|| anyhow!("Invalid YAML scalar span"))?;
            let marker = format!("{prefix}{}", originals.len());
            replacements.push((range, serde_json::to_string(&marker)?));
            originals.insert(marker, original.to_string());
        }
    }
    Ok((replace_scalar_spans(source, replacements)?, originals))
}

fn restore_plain_scalars(source: &str, originals: &HashMap<String, String>) -> Result<String> {
    let mut replacements = Vec::new();
    let offsets = yaml_source_offsets(source);
    for event in YamlParser::new_from_str(source) {
        let (event, span) = event.map_err(|error| anyhow!("Invalid generated YAML: {error}"))?;
        if let Event::Scalar(value, style, _, _) = event
            && let Some(original) = originals.get(value.as_ref())
        {
            let start = *offsets
                .get(span.start.index())
                .ok_or_else(|| anyhow!("Invalid YAML marker start"))?;
            let token = match style {
                ScalarStyle::Plain => value.to_string(),
                ScalarStyle::SingleQuoted => format!("'{value}'"),
                ScalarStyle::DoubleQuoted => serde_json::to_string(&value)?,
                _ => return Err(anyhow!("Unexpected YAML marker style")),
            };
            // Quoted event spans can extend past comments to the next token.
            // Restore only the exact generated marker, never its following trivia.
            let range = start..start + token.len();
            ensure!(
                source.get(range.clone()) == Some(token.as_str()),
                "Invalid YAML marker token"
            );
            replacements.push((range, original.clone()));
        }
    }
    replace_scalar_spans(source, replacements)
}

fn yaml_source_offsets(source: &str) -> Vec<usize> {
    // Saphyr 0.0.12 markers count characters rather than UTF-8 bytes.
    source
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(source.len()))
        .collect()
}

fn scalar_byte_range(source: &str, span: Span, offsets: &[usize]) -> Result<Range<usize>> {
    let start = offsets
        .get(span.start.index())
        .ok_or_else(|| anyhow!("Invalid YAML scalar start"))?;
    let end = offsets
        .get(span.end.index())
        .ok_or_else(|| anyhow!("Invalid YAML scalar end"))?;
    let text = source
        .get(*start..*end)
        .ok_or_else(|| anyhow!("Invalid YAML scalar range"))?;
    // The parser span can include trivia separating a scalar from its comment.
    Ok(*start..*start + text.trim_end_matches([' ', '\t', '\r', '\n']).len())
}

fn replace_scalar_spans(source: &str, replacements: Vec<(Range<usize>, String)>) -> Result<String> {
    let mut result = source.to_string();
    let mut following = source.len();
    for (range, replacement) in replacements.into_iter().rev() {
        ensure!(
            range.end <= following && source.get(range.clone()).is_some(),
            "Invalid overlapping YAML scalar spans"
        );
        following = range.start;
        result.replace_range(range, &replacement);
    }
    Ok(result)
}

fn insert_yaml_item(
    sequence: &Sequence,
    index: usize,
    value: YamlNode,
    source: &str,
) -> Result<()> {
    let syntax = sequence
        .as_node()
        .ok_or_else(|| anyhow!("Missing sequence syntax"))?;
    let indentationless = syntax
        .children()
        .find(|node| node.kind() == SyntaxKind::SEQUENCE_ENTRY)
        .and_then(|entry| {
            entry
                .children_with_tokens()
                .filter_map(|child| child.into_token())
                .find(|token| token.kind() == SyntaxKind::DASH)
        })
        .is_some_and(|dash| {
            let offset: usize = dash.text_range().start().into();
            offset == 0 || source.as_bytes().get(offset - 1) == Some(&b'\n')
        });
    sequence.insert(index, value);
    if indentationless {
        // yaml-edit 0.3.1 adds two spaces to inserts into column-zero lists.
        // Repair only entry separators, leaving values and comments untouched.
        let children = syntax.children_with_tokens().collect::<Vec<_>>();
        for position in (0..children.len().saturating_sub(1)).rev() {
            if children[position]
                .as_token()
                .is_some_and(|token| token.kind() == SyntaxKind::INDENT)
                && children[position + 1]
                    .as_node()
                    .is_some_and(|node| node.kind() == SyntaxKind::SEQUENCE_ENTRY)
            {
                syntax.splice_children(position..position + 1, vec![]);
            }
        }
    }
    Ok(())
}

pub fn validate_value(kind: DocumentKind, value: &Value) -> Result<()> {
    let issues = super::validation::diagnostics(kind, value)?;
    ensure!(
        issues.is_empty(),
        "{}",
        issues
            .iter()
            .map(|issue| issue.message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    validate_semantics(kind, value)
}

pub(crate) fn validate_semantics(kind: DocumentKind, value: &Value) -> Result<()> {
    match kind {
        DocumentKind::Workspace => {
            WorkspaceConfig::deserialize(value)?;
        }
        DocumentKind::Suite => {
            TestSuite::deserialize(value)?;
        }
        DocumentKind::Viewports => {
            Vec::<viewports::ViewportDef>::deserialize(value)?;
        }
        DocumentKind::ReplacementProfiles => {
            ReplacementProfilesFile::deserialize(value)?;
        }
        DocumentKind::Data => {}
    }
    let issues = semantic_diagnostics(kind, value);
    ensure!(
        issues.is_empty(),
        "{}",
        issues
            .iter()
            .map(|issue| issue.message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    Ok(())
}

pub(crate) fn semantic_diagnostics(
    kind: DocumentKind,
    value: &Value,
) -> Vec<super::validation::Diagnostic> {
    let mut issues = Vec::new();
    match kind {
        DocumentKind::Workspace | DocumentKind::Suite => {
            scope_semantics(value, "", &mut issues);
            if kind == DocumentKind::Suite
                && let Some(scenarios) = value["scenarios"].as_array()
            {
                for (index, scenario) in scenarios.iter().enumerate() {
                    if issues.len() >= super::validation::MAX_DIAGNOSTICS {
                        break;
                    }
                    let pointer = format!("/scenarios/{index}");
                    if scenario["url"]
                        .as_str()
                        .is_some_and(|url| url.trim().is_empty())
                    {
                        semantic_issue(
                            &mut issues,
                            format!("{pointer}/url"),
                            format!("Scenario {} needs a URL", index + 1),
                        );
                    }
                    scope_semantics(scenario, &pointer, &mut issues);
                    for (action_index, action) in scenario["actions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                    {
                        if let Ok(action) = crate::types::TestAction::deserialize(action)
                            && let Err(error) = crate::test::validate_action_fields(&action)
                        {
                            semantic_issue(
                                &mut issues,
                                format!("{pointer}/actions/{action_index}"),
                                error.to_string(),
                            );
                        }
                    }
                }
            }
        }
        DocumentKind::Viewports => {
            // Schema diagnostics own ill-typed fragments; inspect independent valid ones.
            if let Ok(definitions) = Vec::<viewports::ViewportDef>::deserialize(value)
                && let Err(error) =
                    viewports::validate_viewports(&definitions, Path::new("viewport editor"))
            {
                let pointer = error
                    .downcast_ref::<viewports::ViewportValidationError>()
                    .map_or_else(String::new, |error| {
                        format!("/{}/{}", error.index, error.field)
                    });
                semantic_issue(&mut issues, pointer, error.to_string());
            }
        }
        DocumentKind::ReplacementProfiles => {
            // Schema diagnostics own ill-typed items; check regexes, flags, and script names here.
            // Whether a script file exists needs the workspace, so the run checks that.
            for (name, items) in value["profiles"].as_object().into_iter().flatten() {
                for (index, item) in items.as_array().into_iter().flatten().enumerate() {
                    if issues.len() >= super::validation::MAX_DIAGNOSTICS {
                        break;
                    }
                    let pointer = format!(
                        "/profiles/{}/{index}",
                        super::validation::escape_pointer(name)
                    );
                    if let Ok(item) = ReplacementProfileItem::deserialize(item)
                        && let Err(error) = validate_profile_items_syntax(
                            std::slice::from_ref(&item),
                            &super::validation::display_path(&pointer),
                        )
                    {
                        semantic_issue(&mut issues, pointer, format!("{error:#}"));
                    }
                }
            }
        }
        DocumentKind::Data => {
            if !value.is_object() && !value.is_array() {
                semantic_issue(
                    &mut issues,
                    String::new(),
                    "Expected a mapping or a list".into(),
                );
            }
        }
    }
    // Profile names are arbitrary keys, so a profile called `delay` is a list, not a number.
    if kind != DocumentKind::ReplacementProfiles {
        number_semantics(value, "", &mut issues, 0);
    }
    issues
}

fn semantic_issue(
    issues: &mut Vec<super::validation::Diagnostic>,
    pointer: String,
    message: String,
) {
    if issues.len() < super::validation::MAX_DIAGNOSTICS {
        issues.push(super::validation::Diagnostic { pointer, message });
    }
}

fn scope_semantics(value: &Value, pointer: &str, issues: &mut Vec<super::validation::Diagnostic>) {
    if let Ok(browser) = Browser::deserialize(&value["browser"])
        && browser != Browser::Chromium
    {
        semantic_issue(
            issues,
            format!("{pointer}/browser"),
            "Only Chromium is supported".into(),
        );
    }
    if let Some(rules) = value["urlReplacements"].as_array() {
        for (index, value) in rules.iter().enumerate() {
            if issues.len() >= super::validation::MAX_DIAGNOSTICS {
                break;
            }
            let path = format!("{pointer}/urlReplacements/{index}");
            if let Ok(rule) = UrlReplacement::deserialize(value)
                && let Err(error) = validate_replacements(
                    std::slice::from_ref(&rule),
                    &super::validation::display_path(&path),
                )
            {
                semantic_issue(issues, path, format!("{error:#}"));
            }
        }
    }
}

fn number_semantics(
    value: &Value,
    pointer: &str,
    issues: &mut Vec<super::validation::Diagnostic>,
    depth: usize,
) {
    if depth > 128 || issues.len() >= super::validation::MAX_DIAGNOSTICS {
        return;
    }
    if let Some(mapping) = value.as_object() {
        for (key, value) in mapping {
            let path = format!("{pointer}/{}", super::validation::escape_pointer(key));
            if !value.is_null() {
                let message = match key.as_str() {
                    "misMatchThreshold" | "pixelMatchThreshold"
                        if !value
                            .as_f64()
                            .is_some_and(|number| (0.0..=100.0).contains(&number)) =>
                    {
                        Some(format!("{key} must be between 0 and 100"))
                    }
                    "asyncCaptureLimit" | "asyncCompareLimit"
                        if !value.as_u64().is_some_and(|number| number > 0) =>
                    {
                        Some(format!("{key} must be a positive integer"))
                    }
                    "delay" | "postInteractionWait"
                        if !value.as_f64().is_some_and(|number| number >= 0.0) =>
                    {
                        Some(format!("{key} cannot be negative"))
                    }
                    _ => None,
                };
                if let Some(message) = message {
                    semantic_issue(issues, path.clone(), message);
                }
            }
            number_semantics(value, &path, issues, depth + 1);
        }
    } else if let Some(items) = value.as_array() {
        for (index, item) in items.iter().enumerate() {
            number_semantics(item, &format!("{pointer}/{index}"), issues, depth + 1);
        }
    }
}

pub const ACTIONS: &[&str] = &[
    "check",
    "click",
    "focus",
    "goto",
    "hide",
    "hover",
    "input",
    "persist",
    "press",
    "remove",
    "save",
    "select",
    "scroll",
    "switchTab",
    "uncheck",
    "wait",
    "drag",
    "dropFiles",
    "wheel",
    "navigate",
    "dialog",
    "key",
    "tap",
    "swipe",
    "longPress",
    "blur",
    "pinch",
    "selectText",
    "paste",
    "closeTab",
];

pub fn action_template(action: &str) -> Result<Value> {
    ensure!(ACTIONS.contains(&action), "Unknown action '{action}'");
    Ok(match action {
        "wait" => json!({"wait": 1000}),
        "goto" => json!({"goto": "https://example.com"}),
        "press" => json!({"press": "#element", "key": "Enter"}),
        "input" => json!({"input": "#element", "value": ""}),
        "select" => json!({"select": "#element", "value": "option"}),
        "save" => json!({"save": "checkpoint"}),
        "persist" => json!({"persist": "session"}),
        "switchTab" => json!({"switchTab": "new"}),
        "drag" => json!({"drag": "#source", "to": "#destination"}),
        "dropFiles" => json!({"dropFiles": "#drop-zone", "file": "uploads/sample.pdf"}),
        "wheel" => json!({"wheel": "#scroll-container", "deltaY": 300}),
        "navigate" => json!({"navigate": "back"}),
        "dialog" => json!({"dialog": "accept", "kind": "confirm"}),
        "key" => json!({"key": "Tab"}),
        "swipe" => json!({"swipe": "#carousel", "offset": {"x": -100, "y": 0}}),
        "pinch" => json!({"pinch": "#map-canvas", "scale": 2.0}),
        "paste" => json!({"paste": "#element", "value": ""}),
        "closeTab" => json!({"closeTab": "current"}),
        selector => json!({selector: "#element"}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite(source: &str) -> Document {
        Document::new(DocumentKind::Suite, Format::Yaml, source.to_string()).unwrap()
    }

    const BLANK_WORKSPACE_YAML: &[&str] = &[
        "",
        "   ",
        "  \r\n \n",
        "# Workspace defaults\n",
        "# No final newline",
        "# Workspace defaults\r\n\r\n  # Keep this note\r\n",
        "# Caf\u{e9} settings\r\n  # No final newline",
    ];

    #[test]
    fn source_validation_duplicate_json_keys_cannot_be_collapsed_by_fields() {
        for (kind, source) in [
            (DocumentKind::Workspace, r#"{"debug":true,"debug":false}"#),
            (
                DocumentKind::Suite,
                r#"{"scenarios":[{"url":"/","url":"/other"}]}"#,
            ),
            (
                DocumentKind::Viewports,
                r#"[{"label":"phone","label":"other","width":375,"height":667}]"#,
            ),
            (DocumentKind::Data, r#"{"map":{"key":1,"key":2}}"#),
            (
                DocumentKind::Workspace,
                r#"{"debug":true,"d\u0065bug":false}"#,
            ),
        ] {
            let mut document = Document::new(kind, Format::Json, source.into()).unwrap();
            assert!(document.value().is_err(), "{source}");
            assert!(document.validate().is_err());
            assert!(
                document
                    .apply(Edit::Set {
                        parent: String::new(),
                        key: "debug".into(),
                        value: Some(json!(true)),
                    })
                    .is_err()
            );
            assert_eq!(document.source(), source);
            assert!(!document.can_undo());
        }
    }

    #[test]
    fn source_validation_json_values_keep_standard_number_and_string_semantics() {
        for source in [
            r#"{"text":"\uD83D\uDE80","array":[true,false,null,{"other":1}]}"#,
            r#"{"min":-9223372036854775808,"max":18446744073709551615,"float":1.25e-3}"#,
            r#"{"left":{"key":1},"right":{"key":2}}"#,
        ] {
            assert_eq!(
                Format::Json.parse(source).unwrap(),
                serde_json::from_str::<Value>(source).unwrap()
            );
        }
    }

    #[test]
    fn blank_workspace_yaml_uses_defaults_without_changing_source() {
        for &source in BLANK_WORKSPACE_YAML {
            serde_yaml::from_str::<WorkspaceConfig>(source).unwrap();
            let document =
                Document::new(DocumentKind::Workspace, Format::Yaml, source.into()).unwrap();
            assert_eq!(document.value().unwrap(), json!({}), "{source:?}");
            document.validate().unwrap();
            assert_eq!(document.source(), source);
            assert_eq!(document.saved_source(), source);
            assert!(!document.dirty());
            assert!(!document.can_undo());
        }
    }

    #[test]
    fn blank_workspace_yaml_supports_field_edits_and_exact_undo() {
        for &source in BLANK_WORKSPACE_YAML {
            let mut document =
                Document::new(DocumentKind::Workspace, Format::Yaml, source.into()).unwrap();
            document
                .apply(Edit::Set {
                    parent: String::new(),
                    key: "debug".into(),
                    value: None,
                })
                .unwrap();
            assert_eq!(document.source(), source);
            assert!(!document.dirty());
            assert!(!document.can_undo());

            document
                .apply_many(vec![
                    Edit::Set {
                        parent: String::new(),
                        key: "debug".into(),
                        value: Some(json!(false)),
                    },
                    Edit::Set {
                        parent: String::new(),
                        key: "delay".into(),
                        value: Some(json!(0)),
                    },
                ])
                .unwrap_or_else(|error| panic!("{source:?}: {error:#}"));
            assert_eq!(
                document.value().unwrap(),
                json!({"debug": false, "delay": 0})
            );
            assert!(document.source().starts_with(source), "{source:?}");
            document.validate().unwrap();
            let edited = document.source().to_string();

            document.undo();
            assert_eq!(document.source(), source);
            assert!(!document.dirty());
            assert!(!document.can_undo());
            document.validate().unwrap();
            document.redo();
            assert_eq!(document.source(), edited);
            assert!(document.dirty());
        }
    }

    #[test]
    fn blank_workspace_yaml_can_result_from_resetting_the_last_field() {
        for source in ["debug: true\n", "# Keep this note\ndebug: true\n"] {
            let mut document =
                Document::new(DocumentKind::Workspace, Format::Yaml, source.into()).unwrap();
            document
                .apply(Edit::Set {
                    parent: String::new(),
                    key: "debug".into(),
                    value: None,
                })
                .unwrap();
            assert_eq!(document.value().unwrap(), json!({}));
            document.validate().unwrap();
            if source.starts_with('#') {
                assert!(document.source().contains("# Keep this note\n"));
            }
            document.undo();
            assert_eq!(document.source(), source);
        }
    }

    #[test]
    fn blank_workspace_yaml_does_not_relax_other_document_validation() {
        for source in [
            "null\n",
            "~\n",
            "\"\"\n",
            "[]\n",
            "false\n",
            " \t\r\n",
            "# Defaults\ndebug: [\n",
            "---\n---\n",
        ] {
            assert!(serde_yaml::from_str::<WorkspaceConfig>(source).is_err());
            let mut document =
                Document::new(DocumentKind::Workspace, Format::Yaml, source.into()).unwrap();
            assert!(document.validate().is_err(), "{source:?}");
            assert!(
                document
                    .apply(Edit::Set {
                        parent: String::new(),
                        key: "debug".into(),
                        value: Some(json!(false)),
                    })
                    .is_err(),
                "{source:?}"
            );
            assert_eq!(document.source(), source);
            assert!(!document.dirty());
        }
        for kind in [
            DocumentKind::Suite,
            DocumentKind::Viewports,
            DocumentKind::Data,
        ] {
            for &source in BLANK_WORKSPACE_YAML {
                let document = Document::new(kind, Format::Yaml, source.into()).unwrap();
                assert_eq!(document.value().unwrap(), Value::Null);
                assert!(document.validate().is_err(), "{kind:?}: {source:?}");
            }
        }
        for source in ["", " \r\n", "null"] {
            let document =
                Document::new(DocumentKind::Workspace, Format::Json, source.into()).unwrap();
            assert!(document.validate().is_err(), "{source:?}");
        }
    }

    #[test]
    fn profile_names_that_look_like_numeric_settings_are_valid() {
        let value = json!({"profiles": {
            "delay": [],
            "asyncCaptureLimit": [{"ref": "a", "test": "b"}],
            "misMatchThreshold": [{"script": "x.cjs"}]
        }});
        assert!(semantic_diagnostics(DocumentKind::ReplacementProfiles, &value).is_empty());
        assert!(validate_value(DocumentKind::ReplacementProfiles, &value).is_ok());
    }

    #[test]
    fn editing_color_blender_suite_preserves_existing_scenarios() {
        let source = include_str!("../../visual_tests/color-blender.tests.yaml");
        let original = Format::Yaml.parse(source).unwrap();
        for (key, value) in [
            ("viewportNames", json!("name")),
            ("asyncCompareLimit", json!(3)),
        ] {
            let mut document = suite(source);
            document.validate().unwrap();
            let edit = Edit::Set {
                parent: String::new(),
                key: key.into(),
                value: Some(value.clone()),
            };
            let generated = apply_yaml(source, &edit).unwrap();
            Format::Yaml
                .parse(&generated)
                .unwrap_or_else(|error| panic!("{error:#}\nGenerated YAML:\n{generated}"));
            document.apply(edit).unwrap();
            let edited = document.value().unwrap();
            assert_eq!(edited[key], value);
            assert_eq!(edited["scenarios"], original["scenarios"]);
            assert!(
                document
                    .source()
                    .contains(".t-o-color-blender__midpoint-list-item[aria-label='10 midpoints']")
            );
            document.validate().unwrap();
            document.undo();
            assert_eq!(document.source(), source);
        }
    }

    #[test]
    fn plain_selector_protection_preserves_unicode_comments_and_line_endings() {
        let source = "# Caf\u{e9} selector notes\nscenarios:\n  - url: https://example.com\n    label: Caf\u{e9}\n    actions:\n      - click: .caf\u{e9}[data-label='a,b{c}'] # preserve selector\n      - input: '#name'\n        value: plain \"quoted\" text\n";
        for ending in ["\n", "\r\n"] {
            for terminal_newline in [true, false] {
                let mut source = source.replace('\n', ending);
                if !terminal_newline {
                    source.truncate(source.len() - ending.len());
                }
                let mut document = suite(&source);
                let original = document.value().unwrap();
                for (key, value) in [
                    ("viewportNames", json!("desktop")),
                    ("asyncCompareLimit", json!(3)),
                ] {
                    document
                        .apply(Edit::Set {
                            parent: String::new(),
                            key: key.into(),
                            value: Some(value),
                        })
                        .unwrap();
                }
                assert_eq!(
                    document.value().unwrap()["scenarios"],
                    original["scenarios"]
                );
                assert!(
                    document
                        .source()
                        .contains(".caf\u{e9}[data-label='a,b{c}'] # preserve selector"),
                    "ending={ending:?}, terminal_newline={terminal_newline}\n{}",
                    document.source()
                );
                assert!(document.source().contains("value: plain \"quoted\" text"));
                assert!(
                    document
                        .source()
                        .contains(&format!("# Caf\u{e9} selector notes{ending}"))
                );
                assert!(!document.source().contains("regressify_scalar_"));
                document.undo();
                document.undo();
                assert_eq!(document.source(), source);
            }
        }
    }

    #[test]
    fn edits_reach_scenarios_after_a_plain_selector_and_can_replace_it() {
        let source = include_str!("../../visual_tests/color-blender.tests.yaml");
        let mut document = suite(source);
        document
            .apply(Edit::Set {
                parent: "/scenarios/5".into(),
                key: "label".into(),
                value: Some(json!("Updated scenario")),
            })
            .unwrap();
        assert_eq!(
            document.value().unwrap()["scenarios"][5]["label"],
            "Updated scenario"
        );
        document
            .apply(Edit::Set {
                parent: "/scenarios/4/actions/3".into(),
                key: "click".into(),
                value: Some(json!(".replacement[data-name='next']")),
            })
            .unwrap();
        assert_eq!(
            document.value().unwrap()["scenarios"][4]["actions"][3]["click"],
            ".replacement[data-name='next']"
        );
        assert_eq!(
            document.value().unwrap()["scenarios"][5]["actions"][3]["click"],
            ".t-o-color-blender__midpoint-list-item[aria-label='10 midpoints']"
        );
        document.validate().unwrap();
    }

    #[test]
    fn plain_scalar_anchors_and_actual_flow_collections_are_preserved() {
        let source = "selector: &target .button[data-role='submit']\ncopy: *target\nvalues: [one, two, {label: 'a,b'}]\n";
        let mut document = Document::new(DocumentKind::Data, Format::Yaml, source.into()).unwrap();
        let mut expected = document.value().unwrap();
        expected["enabled"] = json!(false);
        document
            .apply(Edit::Set {
                parent: String::new(),
                key: "enabled".into(),
                value: Some(json!(false)),
            })
            .unwrap();
        assert_eq!(document.value().unwrap(), expected);
        assert!(
            document
                .source()
                .contains("selector: &target .button[data-role='submit']")
        );
        assert!(document.source().contains("copy: *target"));
        assert!(
            document
                .source()
                .contains("values: [one, two, {label: 'a,b'}]")
        );
    }

    #[test]
    fn every_shipped_suite_allows_root_field_edits() {
        for source in [
            include_str!("../../visual_tests/alloy.tests.yaml"),
            include_str!("../../visual_tests/basic.tests.yaml"),
            include_str!("../../visual_tests/color-blender.tests.yaml"),
            include_str!("../../visual_tests/form-reuse-scenarios.tests.yaml"),
            include_str!("../../visual_tests/form-submission.tests.yaml"),
            include_str!("../../visual_tests/frequently-changed-data.tests.yaml"),
            include_str!("../../visual_tests/sign-in.tests.yaml"),
        ] {
            let mut document = suite(source);
            let original = document.value().unwrap();
            document
                .apply(Edit::Set {
                    parent: String::new(),
                    key: "asyncCompareLimit".into(),
                    value: Some(json!(3)),
                })
                .unwrap();
            assert_eq!(
                document.value().unwrap()["scenarios"],
                original["scenarios"]
            );
        }
    }

    #[test]
    fn nested_edit_preserves_comments_and_untouched_formatting() {
        let source = "# QA notes\nscenarios:\n  - url: 'https://example.com' # keep URL\n    label: Home\n    actions:\n      - click: '#old' # target\n";
        let mut document = suite(source);
        document
            .apply(Edit::Set {
                parent: "/scenarios/0/actions/0".into(),
                key: "click".into(),
                value: Some(json!("#new")),
            })
            .unwrap();
        assert!(document.source().contains("# QA notes"));
        assert!(
            document
                .source()
                .contains("'https://example.com' # keep URL")
        );
        assert!(document.source().contains("# target"));
        assert_eq!(
            document.value().unwrap()["scenarios"][0]["actions"][0]["click"],
            "#new"
        );
        document.validate().unwrap();
        document.undo();
        assert_eq!(document.source(), source);
        assert!(!document.dirty());
        document.redo();
        assert!(document.dirty());
    }

    #[test]
    fn list_operations_preserve_semantics_and_bounds() {
        for format in [Format::Yaml, Format::Json] {
            let value = json!({"scenarios": [{"url": "https://one"}, {"url": "https://two"}]});
            let mut document = Document::new(
                DocumentKind::Suite,
                format,
                format.serialize(&value).unwrap(),
            )
            .unwrap();
            document
                .apply(Edit::Move {
                    path: "/scenarios".into(),
                    from: 0,
                    to: 1,
                })
                .unwrap();
            assert_eq!(
                document.value().unwrap()["scenarios"][0]["url"],
                "https://two"
            );
            document
                .apply(Edit::Insert {
                    path: "/scenarios".into(),
                    index: 1,
                    value: json!({"url": "https://three"}),
                })
                .unwrap();
            document
                .apply(Edit::Remove {
                    path: "/scenarios".into(),
                    index: 0,
                })
                .unwrap();
            assert_eq!(
                document.value().unwrap()["scenarios"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            let before = document.source().to_string();
            assert!(
                document
                    .apply(Edit::Remove {
                        path: "/scenarios".into(),
                        index: 20
                    })
                    .is_err()
            );
            assert_eq!(document.source(), before);
        }
    }

    #[test]
    fn empty_sequences_can_receive_first_item() {
        let mut document = suite("scenarios: []\n");
        document
            .apply(Edit::Insert {
                path: "/scenarios".into(),
                index: 0,
                value: json!({"url": "https://example.com"}),
            })
            .unwrap();
        document.validate().unwrap();
    }

    #[test]
    fn explicit_false_and_empty_lists_are_not_inherited() {
        let mut document = suite("debug: true\nscenarios: []\n");
        document
            .apply(Edit::Set {
                parent: "".into(),
                key: "debug".into(),
                value: Some(json!(false)),
            })
            .unwrap();
        assert_eq!(document.value().unwrap()["debug"], false);
        document
            .apply(Edit::Set {
                parent: "".into(),
                key: "hideSelectors".into(),
                value: Some(json!([])),
            })
            .unwrap();
        assert_eq!(document.value().unwrap()["hideSelectors"], json!([]));
        document
            .apply(Edit::Set {
                parent: "".into(),
                key: "debug".into(),
                value: None,
            })
            .unwrap();
        assert!(document.value().unwrap().get("debug").is_none());
    }

    #[test]
    fn pixel_match_threshold_reset_preserves_suite_override() {
        let source = Format::Yaml
            .serialize(&json!({
                "scenarios": [{"label": "Scenario 1", "url": "https://example.com", "actions": []}]
            }))
            .unwrap();
        let mut document = suite(&source);
        document
            .apply(Edit::Set {
                parent: "".into(),
                key: "pixelMatchThreshold".into(),
                value: Some(json!(3.0)),
            })
            .unwrap();
        document
            .apply(Edit::Set {
                parent: "/scenarios/0".into(),
                key: "pixelMatchThreshold".into(),
                value: Some(json!(3.1)),
            })
            .unwrap();
        let reset = Edit::Set {
            parent: "/scenarios/0".into(),
            key: "pixelMatchThreshold".into(),
            value: None,
        };
        let mut expected = document.value().unwrap();
        apply_value(&mut expected, &reset).unwrap();
        let changed = apply_yaml(document.source(), &reset).unwrap();
        assert_eq!(Format::Yaml.parse(&changed).unwrap(), expected, "{changed}");
        document.apply(reset).unwrap();
    }

    #[test]
    fn pixel_match_threshold_edits_reset_and_undo_losslessly() {
        for (kind, yaml, parent) in [
            (
                DocumentKind::Workspace,
                "# Workspace settings\npixelMatchThreshold: 10\n",
                "",
            ),
            (
                DocumentKind::Suite,
                "# Suite settings\npixelMatchThreshold: 10\nscenarios:\n  - url: 'https://example.com'\n",
                "",
            ),
            (
                DocumentKind::Suite,
                "# Scenario settings\nscenarios:\n  - url: 'https://example.com'\n    pixelMatchThreshold: 10\n",
                "/scenarios/0",
            ),
        ] {
            for format in [Format::Yaml, Format::Json] {
                let source = if format == Format::Yaml {
                    yaml.to_string()
                } else {
                    format
                        .serialize(&Format::Yaml.parse(yaml).unwrap())
                        .unwrap()
                };
                let mut document = Document::new(kind, format, source.clone()).unwrap();
                document
                    .apply(Edit::Set {
                        parent: parent.into(),
                        key: "pixelMatchThreshold".into(),
                        value: Some(json!(0.0)),
                    })
                    .unwrap();
                document.validate().unwrap();
                assert_eq!(
                    document.value().unwrap().pointer(parent).unwrap()["pixelMatchThreshold"],
                    0.0
                );
                document.undo();
                assert_eq!(document.source(), source);
                document.redo();
                document
                    .apply(Edit::Set {
                        parent: parent.into(),
                        key: "pixelMatchThreshold".into(),
                        value: None,
                    })
                    .unwrap();
                document.validate().unwrap();
                assert!(
                    document
                        .value()
                        .unwrap()
                        .pointer(parent)
                        .unwrap()
                        .get("pixelMatchThreshold")
                        .is_none()
                );
                if format == Format::Yaml {
                    assert!(document.source().starts_with(yaml.lines().next().unwrap()));
                }
            }
        }
    }

    #[test]
    fn invalid_source_is_retained_but_not_validated() {
        let mut document = suite("scenarios: []\n");
        document.set_source("scenarios: [".into()).unwrap();
        assert!(document.dirty());
        assert!(document.validate().is_err());
        document.undo();
        assert!(document.validate().is_ok());
    }

    #[test]
    fn every_action_template_uses_the_existing_serde_contract() {
        for action in ACTIONS {
            let template = action_template(action).unwrap();
            serde_json::from_value::<crate::types::TestAction>(template.clone()).unwrap();
            let value =
                json!({"scenarios": [{"url": "https://example.com", "actions": [template]}]});
            validate_value(DocumentKind::Suite, &value).unwrap();
        }
        assert!(action_template("unknown").is_err());
    }

    #[test]
    fn basic_auth_validation_explains_the_missing_origin_without_exposing_credentials() {
        let document = suite(
            "scenarios: []\nbasicAuth:\n  - username: private-user\n    password: private-password\n",
        );
        let error = document.validate().unwrap_err().to_string();
        assert!(error.contains("basicAuth[0].origin"), "{error}");
        assert!(error.contains("https://example.com"), "{error}");
        assert!(!error.contains("BasicAuthConfig"), "{error}");
        assert!(!error.contains("private-user"), "{error}");
        assert!(!error.contains("private-password"), "{error}");
    }

    #[test]
    fn validation_rejects_unknown_properties_and_unsafe_values() {
        for value in [
            json!({"debugg": true}),
            json!({"asyncCaptureLimit": 0}),
            json!({"browser": "firefox"}),
            json!({"misMatchThreshold": 101}),
            json!({"delay": -1}),
        ] {
            assert!(
                validate_value(DocumentKind::Workspace, &value).is_err(),
                "{value}"
            );
        }
        assert!(
            validate_value(
                DocumentKind::Viewports,
                &json!([{"label": "phone", "width": 0, "height": 600}])
            )
            .is_err()
        );
    }

    #[test]
    fn source_history_groups_adjacent_typing_and_deletion_with_cursor_restore() {
        let mut document = Document::new(DocumentKind::Data, Format::Text, "ab".into()).unwrap();
        document.set_source_selection(2..2);
        document.set_source_draft("abc".into(), 3..3).unwrap();
        document
            .set_source_draft("abc\u{1f680}".into(), 7..7)
            .unwrap();
        document.undo();
        assert_eq!(document.source(), "ab");
        assert_eq!(document.source_selection(), Some(2..2));
        assert!(!document.can_undo());
        document.redo();
        assert_eq!(document.source(), "abc\u{1f680}");
        assert_eq!(document.source_selection(), Some(7..7));

        document.set_source_draft("abc".into(), 3..3).unwrap();
        document.set_source_draft("ab".into(), 2..2).unwrap();
        document.undo();
        assert_eq!(document.source(), "abc\u{1f680}");
        assert_eq!(document.source_selection(), Some(7..7));

        document.set_source_selection(0..0);
        document
            .set_source_draft("bc\u{1f680}".into(), 0..0)
            .unwrap();
        document
            .set_source_draft("c\u{1f680}".into(), 0..0)
            .unwrap();
        document.undo();
        assert_eq!(document.source(), "abc\u{1f680}");
        assert_eq!(document.source_selection(), Some(0..0));
    }

    #[test]
    fn source_history_keeps_navigation_paste_commands_and_saves_as_boundaries() {
        let mut document = Document::new(DocumentKind::Data, Format::Text, "".into()).unwrap();
        document.set_source_selection(0..0);
        document.set_source_draft("a".into(), 1..1).unwrap();
        document.set_source_selection(0..0);
        document.set_source_selection(1..1);
        document.set_source_draft("ab".into(), 2..2).unwrap();
        document.undo();
        assert_eq!(document.source(), "a");
        document.redo();
        document.set_source_draft("ab pasted".into(), 9..9).unwrap();
        document
            .set_source_draft("ab pasted!".into(), 10..10)
            .unwrap();
        document.undo();
        assert_eq!(document.source(), "ab pasted");
        document.undo();
        assert_eq!(document.source(), "ab");
        document.set_source("command".into()).unwrap();
        assert!(!document.can_redo());
        document.undo();
        assert_eq!(document.source(), "ab");
        document.set_source_draft("abc".into(), 3..3).unwrap();
        document.acknowledge_saved("ab".into());
        document.set_source_draft("abcd".into(), 4..4).unwrap();
        document.undo();
        assert_eq!(document.source(), "abc");
        assert!(document.dirty());
    }

    #[test]
    fn source_history_preserves_oversized_drafts_and_bounds_both_stacks() {
        let mut document = Document::new(DocumentKind::Data, Format::Text, "saved".into()).unwrap();
        let oversized = "x".repeat(MAX_DOCUMENT_BYTES + 1);
        document
            .set_source_draft(oversized.clone(), oversized.len()..oversized.len())
            .unwrap();
        assert!(document.dirty());
        assert!(document.validate().is_err());
        document.undo();
        assert_eq!(document.source(), "saved");
        document.redo();
        assert_eq!(document.source(), oversized);
        for index in 0..12 {
            let text = format!("{index}{}", "x".repeat(MAX_DOCUMENT_BYTES - 2));
            document.set_source(text).unwrap();
        }
        for undo in [true, true, true, false, false] {
            if undo {
                document.undo();
            } else {
                document.redo();
            }
            assert_eq!(
                document.undo_bytes,
                document
                    .undo
                    .iter()
                    .map(|entry| entry.source.len())
                    .sum::<usize>()
            );
            assert_eq!(
                document.redo_bytes,
                document
                    .redo
                    .iter()
                    .map(|entry| entry.source.len())
                    .sum::<usize>()
            );
            assert!(document.undo_bytes + document.redo_bytes <= MAX_HISTORY_BYTES);
            assert!(document.undo.len() + document.redo.len() <= MAX_HISTORY_ENTRIES);
        }
        let previous = document.source().to_string();
        assert!(document.set_source_draft("\u{1f680}".into(), 1..1).is_err());
        assert_eq!(document.source(), previous);
    }

    #[test]
    fn history_is_bounded_and_new_edits_discard_redo() {
        let mut document = suite("scenarios: []\n");
        for index in 0..150 {
            document
                .set_source(format!("# {index}\nscenarios: []\n"))
                .unwrap();
        }
        assert_eq!(document.undo.len(), MAX_HISTORY_ENTRIES);
        document.undo();
        assert!(document.can_redo());
        document.set_source("scenarios: []\n".into()).unwrap();
        assert!(!document.can_redo());
        assert!(
            document
                .set_source(" ".repeat(MAX_DOCUMENT_BYTES + 1))
                .is_err()
        );
    }

    #[test]
    fn multiple_documents_are_not_silently_truncated() {
        let document = suite("scenarios: []\n---\nscenarios: []\n");
        assert!(document.value().is_err());
    }

    #[test]
    fn acknowledging_an_older_save_keeps_newer_edits_dirty() {
        let mut document = suite("scenarios: []\n");
        document
            .set_source("debug: true\nscenarios: []\n".into())
            .unwrap();
        let snapshot = document.source().to_string();
        document
            .set_source("debug: false\nscenarios: []\n".into())
            .unwrap();
        document.acknowledge_saved(snapshot.clone());
        assert!(document.dirty());
        assert_eq!(document.saved_source(), snapshot);
        assert!(document.source().contains("debug: false"));
    }
}
