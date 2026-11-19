//! Source editing commands, schema assistance, and diagnostics.

use std::{ops::Range, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{
    input::{CompletionProvider, HoverProvider, Rope},
    status_bar::StatusBar,
    text::{SelectionFormat, TextView},
};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Documentation, Position, TextEdit,
};
use serde::Deserialize;

use super::*;
use crate::desktop::{
    completion::{CompletionCatalog, SourceIndex},
    document::{Document, Format, semantic_diagnostics},
    validation,
};

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
pub(super) enum LineOperation {
    ToggleComment,
    Indent,
    Outdent,
    DuplicateUp,
    DuplicateDown,
    MoveUp,
    MoveDown,
    Delete,
    InsertBelow,
    InsertAbove,
}

actions!(regressify_desktop, [SelectSourceLines]);

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
pub(super) struct SourceLineCommand {
    operation: LineOperation,
}

pub(super) fn bind_keys(modifier: &str, cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        &format!("{modifier}-shift-z"),
        gpui_kit::component::input::Redo,
        Some("RegressifySource > Input"),
    )]);
    cx.bind_keys(
        [
            (format!("{modifier}-/"), LineOperation::ToggleComment),
            (format!("{modifier}-]"), LineOperation::Indent),
            (format!("{modifier}-["), LineOperation::Outdent),
            ("alt-shift-up".into(), LineOperation::DuplicateUp),
            ("alt-shift-down".into(), LineOperation::DuplicateDown),
            ("alt-up".into(), LineOperation::MoveUp),
            ("alt-down".into(), LineOperation::MoveDown),
            (format!("{modifier}-shift-k"), LineOperation::Delete),
            (format!("{modifier}-enter"), LineOperation::InsertBelow),
            (
                format!("{modifier}-shift-enter"),
                LineOperation::InsertAbove,
            ),
        ]
        .into_iter()
        .map(|(key, operation)| {
            KeyBinding::new(
                &key,
                SourceLineCommand { operation },
                Some("RegressifySource > Input"),
            )
        }),
    );
    cx.bind_keys([KeyBinding::new(
        &format!("{modifier}-l"),
        SelectSourceLines,
        Some("RegressifySource > Input"),
    )]);
}

struct LineEdit {
    range: Range<usize>,
    text: String,
    selection: Range<usize>,
}

fn selected_lines(text: &str, selection: &Range<usize>) -> Range<usize> {
    let start = text[..selection.start]
        .rfind('\n')
        .map_or(0, |offset| offset + 1);
    // The line at a selection's exclusive column-zero end is not selected.
    let last = if !selection.is_empty() && text.as_bytes().get(selection.end - 1) == Some(&b'\n') {
        selection.end - 1
    } else {
        selection.end
    };
    let end = text[last..]
        .find('\n')
        .map_or(text.len(), |offset| last + offset + 1);
    start..end
}

fn line_content(line: &str) -> &str {
    // Non-ASCII whitespace and form feed are content, not YAML indentation.
    line.trim_end_matches(['\r', '\n'])
        .trim_start_matches([' ', '\t'])
}

fn toggle_line_comments(text: &str, selection: Range<usize>) -> Option<LineEdit> {
    let range = selected_lines(text, &selection);
    let block = &text[range.clone()];
    let lines = if block.is_empty() {
        vec![""]
    } else {
        block.split_inclusive('\n').collect::<Vec<_>>()
    };
    let uncomment = lines.iter().any(|line| !line_content(line).is_empty())
        && lines
            .iter()
            .map(|line| line_content(line))
            .filter(|line| !line.is_empty())
            .all(|line| line.starts_with('#'));
    let mut edits = Vec::new();
    let mut offset = 0;
    for line in lines {
        let content = line.trim_end_matches(['\r', '\n']);
        let trimmed = line_content(line);
        if !trimmed.is_empty() || selection.is_empty() {
            let prefix = offset + content.len() - trimmed.len();
            let removed = if uncomment {
                trimmed
                    .strip_prefix('#')
                    .map_or(0, |rest| 1 + usize::from(rest.starts_with(' ')))
            } else {
                0
            };
            edits.push((prefix..prefix + removed, if uncomment { "" } else { "# " }));
        }
        offset += line.len();
    }
    replace_line_prefixes(text, range, selection, edits)
}

fn replace_line_prefixes(
    text: &str,
    range: Range<usize>,
    selection: Range<usize>,
    edits: Vec<(Range<usize>, &str)>,
) -> Option<LineEdit> {
    let mut replacement = text[range.clone()].to_string();
    let mut selection = selection;
    for (removed, inserted) in edits.into_iter().rev() {
        replacement.replace_range(removed.clone(), inserted);
        for endpoint in [&mut selection.start, &mut selection.end] {
            let start = range.start + removed.start;
            let end = range.start + removed.end;
            if *endpoint >= start {
                *endpoint = start + inserted.len() + endpoint.saturating_sub(end);
            }
        }
    }
    (replacement != text[range.clone()]).then_some(LineEdit {
        range,
        text: replacement,
        selection,
    })
}

// gpui-kit's block indent includes the exclusive end line; use our shared bounds.
fn indent_lines(text: &str, selection: Range<usize>, outdent: bool) -> Option<LineEdit> {
    let range = selected_lines(text, &selection);
    let block = &text[range.clone()];
    let lines = if block.is_empty() {
        vec![""]
    } else {
        block.split_inclusive('\n').collect::<Vec<_>>()
    };
    let width = gpui_kit::component::input::TabSize::default().tab_size;
    let indent = " ".repeat(width);
    let mut edits = Vec::new();
    let mut offset = 0;
    for line in lines {
        if outdent {
            let removed = if line.starts_with('\t') {
                1
            } else {
                line.bytes()
                    .take(width)
                    .take_while(|byte| *byte == b' ')
                    .count()
            };
            if removed > 0 {
                edits.push((offset..offset + removed, ""));
            }
        } else if !line_content(line).is_empty() || selection.is_empty() {
            edits.push((offset..offset, indent.as_str()));
        }
        offset += line.len();
    }
    replace_line_prefixes(text, range, selection, edits)
}

fn split_line_ending(text: &str) -> (&str, &str) {
    if let Some(content) = text.strip_suffix("\r\n") {
        (content, "\r\n")
    } else if let Some(content) = text.strip_suffix('\n') {
        (content, "\n")
    } else {
        (text, "")
    }
}

/// The line ending to add after an unterminated block, matching its neighbours.
fn block_separator(text: &str, range: &Range<usize>) -> &'static str {
    if text[range.clone()].contains("\r\n") || text[..range.start].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// The ASCII indentation of the line starting at `start`; other whitespace is content.
fn line_indent(text: &str, start: usize) -> &str {
    let line = &text[start..];
    &line[..line
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()]
}

/// Whole selected lines, or one more line when they are already selected, like VS Code's Ctrl+L.
fn expanded_line_selection(text: &str, selection: Range<usize>) -> Range<usize> {
    let lines = selected_lines(text, &selection);
    if selection.is_empty() || selection != lines {
        return lines;
    }
    let end = text[lines.end..]
        .find('\n')
        .map_or(text.len(), |offset| lines.end + offset + 1);
    lines.start..end
}

/// The start of the line containing `offset`.
fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map_or(0, |offset| offset + 1)
}

/// Indentation characters to step over after native Home, or `None` to keep native Home.
///
/// Like VS Code, Home goes to the first non-blank character unless the caret is already there;
/// a blank line has none, so Home goes to its start.
fn smart_home_steps(text: &str, cursor: usize) -> Option<usize> {
    let start = line_start(text, cursor);
    let indent = line_indent(text, start).len();
    let rest = &text[start + indent..];
    let blank = rest.is_empty() || rest.starts_with(['\r', '\n']);
    (indent > 0 && !blank && cursor != start + indent).then_some(indent)
}

/// Opens an empty line below the caret's line, indented as Enter at its end would indent it.
fn line_below(text: &str, cursor: usize) -> LineEdit {
    let range = selected_lines(text, &(cursor..cursor));
    let (content, ending) = split_line_ending(&text[range.clone()]);
    let current = line_indent(text, range.start);
    let next = if ending.is_empty() {
        ""
    } else {
        line_indent(text, range.end)
    };
    // Mirror Kit's Enter: adopt a deeper next line, else open a bracket block (outside comments).
    let line = content.trim();
    let indent = if next.len() > current.len() {
        next.to_string()
    } else if !line.starts_with('#') && line.ends_with(['{', '[', '(']) {
        let width = gpui_kit::component::input::TabSize::default().tab_size;
        format!("{current}{}", " ".repeat(width))
    } else {
        current.to_string()
    };
    let (inserted, caret) = if ending.is_empty() {
        let separator = block_separator(text, &range);
        let caret = range.end + separator.len() + indent.len();
        (format!("{separator}{indent}"), caret)
    } else {
        (format!("{indent}{ending}"), range.end + indent.len())
    };
    LineEdit {
        range: range.end..range.end,
        text: inserted,
        selection: caret..caret,
    }
}

/// Opens an empty line above the caret's line: like VS Code, Enter at the previous line's end.
fn line_above(text: &str, cursor: usize) -> LineEdit {
    let start = line_start(text, cursor);
    if start > 0 {
        return line_below(text, start - 1);
    }
    let indent = line_indent(text, 0);
    let separator = block_separator(text, &selected_lines(text, &(0..0)));
    LineEdit {
        range: 0..0,
        text: format!("{indent}{separator}"),
        selection: indent.len()..indent.len(),
    }
}

/// Clipboard metadata marking text copied from an empty Source selection.
///
/// GPUI keeps metadata only while the platform text is unchanged (Windows checks a text hash),
/// so text copied in other applications never pastes line-wise.
const SOURCE_LINE_CLIPBOARD: &str = "regressify.source.line";

/// The caret's whole line with a line ending, as editors copy an empty selection.
fn copied_line(text: &str, cursor: usize) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let range = selected_lines(text, &(cursor..cursor));
    let block = &text[range.clone()];
    // The ending lets an unterminated last line paste as a whole line too.
    Some(if split_line_ending(block).1.is_empty() {
        format!("{block}{}", block_separator(text, &range))
    } else {
        block.to_string()
    })
}

/// Inserts a copied line above the caret's line, keeping the caret on its own text.
fn pasted_line(text: &str, cursor: usize, line: &str) -> LineEdit {
    let start = line_start(text, cursor);
    let caret = cursor + line.len();
    LineEdit {
        range: start..start,
        text: line.to_string(),
        selection: caret..caret,
    }
}

fn line_edit(text: &str, selection: Range<usize>, operation: LineOperation) -> Option<LineEdit> {
    let range = selected_lines(text, &selection);
    let block = &text[range.clone()];
    let (content, ending) = split_line_ending(block);
    match operation {
        LineOperation::DuplicateUp | LineOperation::DuplicateDown => {
            let separator = block_separator(text, &range);
            let down = operation == LineOperation::DuplicateDown;
            let replacement = if !ending.is_empty() {
                block.to_string()
            } else if down {
                format!("{separator}{block}")
            } else {
                format!("{block}{separator}")
            };
            let offset = if down { range.end } else { range.start };
            let shift = if down { replacement.len() } else { 0 };
            Some(LineEdit {
                range: offset..offset,
                text: replacement,
                selection: selection.start + shift..selection.end + shift,
            })
        }
        LineOperation::MoveUp => {
            if range.start == 0 {
                return None;
            }
            let previous_start = text[..range.start - 1]
                .rfind('\n')
                .map_or(0, |offset| offset + 1);
            let previous = &text[previous_start..range.start];
            let (previous_content, previous_ending) = split_line_ending(previous);
            let replacement = if ending.is_empty() {
                format!("{block}{previous_ending}{previous_content}")
            } else {
                format!("{block}{previous}")
            };
            Some(LineEdit {
                range: previous_start..range.end,
                text: replacement,
                selection: selection.start - previous.len()..selection.end - previous.len(),
            })
        }
        LineOperation::MoveDown => {
            if range.end == text.len() {
                return None;
            }
            let next_end = text[range.end..]
                .find('\n')
                .map_or(text.len(), |offset| range.end + offset + 1);
            let next = &text[range.end..next_end];
            let (_, next_ending) = split_line_ending(next);
            let (replacement, shift) = if next_ending.is_empty() {
                (
                    format!("{next}{ending}{content}"),
                    next.len() + ending.len(),
                )
            } else {
                (format!("{next}{block}"), next.len())
            };
            Some(LineEdit {
                range: range.start..next_end,
                text: replacement,
                selection: (selection.start + shift).min(next_end)
                    ..(selection.end + shift).min(next_end),
            })
        }
        LineOperation::Delete => {
            let start = if range.end == text.len() && ending.is_empty() && range.start > 0 {
                range.start - split_line_ending(&text[..range.start]).1.len()
            } else {
                range.start
            };
            (start != range.end).then_some(LineEdit {
                range: start..range.end,
                text: String::new(),
                selection: start..start,
            })
        }
        LineOperation::ToggleComment => toggle_line_comments(text, selection),
        LineOperation::Indent => indent_lines(text, selection, false),
        LineOperation::Outdent => indent_lines(text, selection, true),
        // Like VS Code, insertion follows the caret's line, which callers pass as `selection.end`.
        LineOperation::InsertBelow => Some(line_below(text, selection.end)),
        LineOperation::InsertAbove => Some(line_above(text, selection.end)),
    }
}

#[derive(Clone)]
struct SourceCompletions {
    catalog: Arc<CompletionCatalog>,
    kind: DocumentKind,
    format: Format,
    workspace: Option<crate::desktop::workspace::Workspace>,
}

impl SourceCompletions {
    fn items(&self, text: &str, cursor: usize) -> Vec<CompletionItem> {
        let items = self.catalog.complete_with_workspace(
            self.kind,
            self.format,
            text,
            cursor,
            self.workspace.as_ref(),
        );
        if items.is_empty() {
            return Vec::new();
        }
        let mut positions = SourcePositions::new(text);
        items
            .into_iter()
            .map(|item| CompletionItem {
                label: item.label,
                detail: Some(item.detail),
                documentation: (!item.description.is_empty()).then(|| {
                    Documentation::String(
                        item.description
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                }),
                kind: Some(if item.property {
                    CompletionItemKind::PROPERTY
                } else {
                    CompletionItemKind::VALUE
                }),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: positions.range(item.replacement),
                    new_text: item.text,
                })),
                ..Default::default()
            })
            .collect()
    }
}

impl CompletionProvider for SourceCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let provider = self.clone();
        let text = text.clone();
        cx.background_executor().spawn(async move {
            Ok(CompletionResponse::Array(
                provider.items(&text.to_string(), offset),
            ))
        })
    }

    fn is_completion_trigger(&self, _: usize, text: &str, _: &mut App) -> bool {
        !text.is_empty()
            && text.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '_' | '"' | '\'' | '-' | ':' | ' ')
            })
    }
}

impl HoverProvider for SourceCompletions {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<lsp_types::Hover>>> {
        let provider = self.clone();
        let text = text.clone();
        cx.background_executor().spawn(async move {
            let text = text.to_string();
            Ok(provider
                .catalog
                .hint(provider.kind, provider.format, &text, offset)
                .map(|hint| lsp_types::Hover {
                    range: Some(SourcePositions::new(&text).range(hint.range)),
                    contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                        kind: lsp_types::MarkupKind::PlainText,
                        value: hint.text,
                    }),
                }))
        })
    }
}

#[derive(Default)]
pub(super) struct SourceInspection {
    pub(super) value: Value,
    pub(super) issues: Vec<lsp_types::Diagnostic>,
    contextual: Vec<validation::Diagnostic>,
}

#[cfg(test)]
pub(super) fn inspect_document(document: &Document) -> (Value, Vec<lsp_types::Diagnostic>) {
    let inspection = inspect_source_document(document, &[], None);
    (inspection.value, inspection.issues)
}

pub(super) fn inspect_source_document(
    document: &Document,
    existing_context: &[validation::Diagnostic],
    workspace: Option<(&crate::desktop::workspace::Workspace, &str)>,
) -> SourceInspection {
    let text = document.source();
    let value = if document.format == Format::Text {
        document.validate().map(|()| Value::Null)
    } else {
        document.value()
    };
    let mut contextual = existing_context.to_vec();
    let result = value
        .as_ref()
        .map_err(|error| anyhow!("{error:#}"))
        .and_then(|value| {
            if document.format == Format::Text {
                return Ok(Vec::new());
            }
            let mut issues = validation::diagnostics(document.kind, value)?;
            if issues.is_empty()
                && document.kind == DocumentKind::Suite
                && let Some((workspace, suite_name)) = workspace
            {
                let suite = crate::types::TestSuite::deserialize(value)?;
                contextual = workspace
                    .action_viewport_diagnostics_for_suite(document.format, suite, suite_name)
                    .unwrap_or_else(|error| {
                        vec![validation::Diagnostic {
                            pointer: String::new(),
                            message: format!("{error:#}"),
                        }]
                    });
            }
            for issue in semantic_diagnostics(document.kind, value) {
                if issues.len() >= validation::MAX_DIAGNOSTICS {
                    break;
                }
                if !issues.iter().any(|existing| {
                    existing.pointer == issue.pointer
                        || existing.pointer.starts_with(&format!("{}/", issue.pointer))
                }) {
                    issues.push(issue);
                }
            }
            let remaining = validation::MAX_DIAGNOSTICS.saturating_sub(issues.len());
            issues.extend(contextual.iter().take(remaining).cloned());
            if issues.is_empty() {
                return Ok(Vec::new());
            }
            let index = SourceIndex::new(document.format, text);
            let mut positions = SourcePositions::new(text);
            let mut diagnostics = issues
                .into_iter()
                .map(|issue| {
                    let range = index.range(&issue.pointer);
                    source_diagnostic(positions.range(range), issue.message)
                })
                .collect::<Vec<_>>();
            diagnostics.sort_by_key(|issue| (issue.range.start, issue.range.end));
            Ok(diagnostics)
        });
    let issues = match result {
        Ok(issues) => issues,
        Err(error) => {
            let syntax_error = value.as_ref().err();
            let line = syntax_error.and_then(error_line).unwrap_or(0);
            let length = text
                .lines()
                .nth(line as usize)
                .unwrap_or_default()
                .chars()
                .count()
                .max(1) as u32;
            let hint = if syntax_error.is_some() {
                match document.format {
                    Format::Yaml => {
                        "\nUse spaces for YAML indentation, ': ' after field names, and quotes around selectors beginning with #."
                    }
                    Format::Json => {
                        "\nUse double quotes for JSON field names and text, commas between fields, and no trailing commas."
                    }
                    Format::Text => "",
                }
            } else {
                ""
            };
            vec![source_diagnostic(
                lsp_types::Range::new(Position::new(line, 0), Position::new(line, length)),
                format!("{error:#}{hint}"),
            )]
        }
    };
    SourceInspection {
        value: value.unwrap_or(Value::Null),
        issues,
        contextual,
    }
}

// Kit 0.7.3 tracks hover help and diagnostic popovers separately on EditorState;
// clear both so resolved or reconfigured diagnostics cannot leave a stale tooltip.
pub(super) fn clear_source_popovers(
    state: &mut gpui_kit::component::input::EditorState,
    cx: &mut Context<gpui_kit::component::input::EditorState>,
) {
    state.clear_hover_state(cx);
    state.clear_diagnostic_popover(cx);
}

fn source_diagnostic(range: lsp_types::Range, message: String) -> lsp_types::Diagnostic {
    lsp_types::Diagnostic {
        range,
        severity: Some(lsp_types::DiagnosticSeverity::ERROR),
        message,
        source: Some("Regressify".into()),
        ..Default::default()
    }
}

fn diagnostic_markdown(message: &str) -> String {
    let mut description = String::with_capacity(message.len());
    for character in message.replace("\r\n", "\n").chars() {
        if character == '\n' {
            description.push_str("  \n");
        } else {
            if character.is_ascii_punctuation() {
                description.push('\\');
            }
            description.push(character);
        }
    }
    description
}

struct SourcePositions<'a> {
    text: &'a str,
    line_starts: Vec<usize>,
    positions: BTreeMap<usize, Position>,
}

impl<'a> SourcePositions<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            line_starts: std::iter::once(0)
                .chain(text.match_indices('\n').map(|(offset, _)| offset + 1))
                .collect(),
            positions: BTreeMap::new(),
        }
    }

    fn position(&mut self, offset: usize) -> Position {
        if let Some(position) = self.positions.get(&offset) {
            return *position;
        }
        let line = self.line_starts.partition_point(|start| *start <= offset) - 1;
        // Kit's LSP-shaped API uses scalar columns, not UTF-16 code units.
        let column = self.text[self.line_starts[line]..offset].chars().count();
        let position = Position::new(line as u32, column as u32);
        self.positions.insert(offset, position);
        position
    }

    fn range(&mut self, range: Range<usize>) -> lsp_types::Range {
        lsp_types::Range::new(self.position(range.start), self.position(range.end))
    }
}

struct SourceKey {
    root: PathBuf,
    path: PathBuf,
    kind: DocumentKind,
    format: Format,
    text: String,
}

impl SourceKey {
    fn matches(&self, session: &Session) -> bool {
        session
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace.root == self.root)
            && session.open.as_ref().is_some_and(|open| {
                open.entry.path == self.path
                    && open.document.kind == self.kind
                    && open.document.format == self.format
                    && open.document.source() == self.text
            })
    }
}

#[derive(Default)]
pub(super) struct SourceFeedback {
    generation: u64,
    pending: bool,
    stale: bool,
    ready: bool,
    debounce: Option<Task<()>>,
    worker: Option<Task<()>>,
    key: Option<SourceKey>,
    contextual: Vec<validation::Diagnostic>,
    #[cfg(all(test, feature = "gui-tests"))]
    started: usize,
}

impl SourceFeedback {
    pub(super) fn stale(&self) -> bool {
        self.stale
    }

    fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = false;
        self.stale = false;
        self.ready = false;
        self.debounce = None;
    }

    pub(super) fn invalidate(&mut self) {
        self.cancel();
        self.key = None;
        self.contextual.clear();
    }

    pub(super) fn contextual_issues(&self, session: &Session) -> &[validation::Diagnostic] {
        if self.key.as_ref().is_some_and(|key| key.matches(session)) {
            &self.contextual
        } else {
            &[]
        }
    }
}

impl Workbench {
    pub(super) fn cancel_source_feedback(&mut self) {
        self.source_feedback.cancel();
    }

    pub(super) fn source_selection_ready(&mut self, cx: &mut Context<Self>) -> bool {
        if self.source_feedback.stale {
            self.status =
                "Wait for Source checking to finish before using the selected item".into();
            cx.notify();
            return false;
        }
        true
    }

    pub(super) fn set_source_issues(&mut self, issues: Vec<lsp_types::Diagnostic>) -> bool {
        if self.source_issues == issues {
            return false;
        }
        self.source_issue_markdown = issues
            .iter()
            .map(|issue| SharedString::from(diagnostic_markdown(&issue.message)))
            .collect();
        self.diagnostic = (!issues.is_empty()).then(|| {
            issues
                .iter()
                .map(|issue| issue.message.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        });
        self.source_issues = issues;
        true
    }

    pub(super) fn defer_source_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.open.as_ref().is_some_and(|open| {
            open.document.source().len() > crate::desktop::document::MAX_DOCUMENT_BYTES
        }) {
            self.refresh_value();
            self.session.reconcile_selection_in(&self.value);
            self.update_source_diagnostics(cx);
            return;
        }
        self.source_feedback.stale = true;
        if self.set_source_issues(Vec::new()) {
            self.update_source_diagnostics(cx);
        }
        self.schedule_source_feedback(window, cx);
    }

    pub(super) fn schedule_source_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.workspace.is_none() || self.session.open.is_none() {
            self.source_feedback.invalidate();
            return;
        }
        if self.session.open.as_ref().is_some_and(|open| {
            open.document.source().len() > crate::desktop::document::MAX_DOCUMENT_BYTES
        }) {
            self.source_feedback.cancel();
            return;
        }
        if !self.source_feedback.stale
            && self
                .source_feedback
                .key
                .as_ref()
                .is_some_and(|key| key.matches(&self.session))
        {
            return;
        }
        self.source_feedback.generation = self.source_feedback.generation.wrapping_add(1);
        self.source_feedback.pending = true;
        self.source_feedback.ready = false;
        let generation = self.source_feedback.generation;
        self.source_feedback.debounce = Some(cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                if generation != this.source_feedback.generation {
                    return;
                }
                this.source_feedback.debounce = None;
                this.source_feedback.ready = true;
                this.start_source_feedback(window, cx);
            });
        }));
        cx.notify();
    }

    fn start_source_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.source_feedback.pending
            || !self.source_feedback.ready
            || self.source_feedback.worker.is_some()
        {
            return;
        }
        // A native change can precede its model notification. That notification
        // will reschedule inspection; do not repeatedly parse an unacknowledged buffer.
        if !self.source_matches_document(cx) {
            self.source_feedback.stale = true;
            cx.notify();
            return;
        }
        let (Some(workspace), Some(open)) =
            (self.session.workspace.clone(), self.session.open.as_ref())
        else {
            self.source_feedback.invalidate();
            return;
        };
        let generation = self.source_feedback.generation;
        let entry = open.entry.clone();
        let kind = open.document.kind;
        let format = open.document.format;
        let text = self.source.read(cx).text().clone();
        self.source_feedback.ready = false;
        #[cfg(all(test, feature = "gui-tests"))]
        {
            self.source_feedback.started += 1;
        }
        let task = cx.background_executor().spawn(async move {
            let text = text.to_string();
            let suite_name = entry.suite_name().unwrap_or_else(|| entry.label());
            let key = SourceKey {
                root: workspace.root.clone(),
                path: entry.path,
                kind,
                format,
                text: text.clone(),
            };
            let inspection = Document::new(kind, format, text).map(|document| {
                inspect_source_document(&document, &[], Some((&workspace, &suite_name)))
            });
            (key, inspection)
        });
        // Keep one worker alive until completion; cancelling a waiter cannot stop
        // synchronous parsing or disk I/O that already started on the executor.
        self.source_feedback.worker = Some(cx.spawn_in(window, async move |view, cx| {
            let (key, inspection) = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.source_feedback.worker = None;
                if generation == this.source_feedback.generation
                    && key.matches(&this.session)
                    && this.source.read(cx).text().slice(..) == key.text.as_str()
                {
                    let inspection = inspection.unwrap_or_else(|error| SourceInspection {
                        issues: vec![source_diagnostic(
                            lsp_types::Range::default(),
                            format!("{error:#}"),
                        )],
                        ..Default::default()
                    });
                    this.source_feedback.pending = false;
                    this.source_feedback.stale = false;
                    this.source_feedback.key = Some(key);
                    this.source_feedback.contextual = inspection.contextual;
                    this.value = inspection.value;
                    this.session.reconcile_selection_in(&this.value);
                    if this.set_source_issues(inspection.issues) {
                        this.update_source_diagnostics(cx);
                    }
                    cx.notify();
                } else if generation == this.source_feedback.generation
                    && this.source_feedback.pending
                {
                    this.source_feedback.stale = true;
                    this.schedule_source_feedback(window, cx);
                }
                this.start_source_feedback(window, cx);
            });
        }));
    }

    fn source_edit_menu(&self, cx: &App) -> impl IntoElement {
        let source = self.source.clone();
        let focus = source.read(cx).focus_handle(cx);
        let yaml = self
            .session
            .open
            .as_ref()
            .is_some_and(|open| open.document.format == Format::Yaml);
        Button::new("source-edit-menu")
            .ghost()
            .icon(gpui_kit::assets::IconName::SquarePen)
            .accessibility_label("Edit source lines")
            .tooltip("Edit source lines")
            .disabled(self.busy || self.session.open.is_none())
            .prepare_for_toolbar()
            .with_size(gpui_kit::component::Size::Medium)
            .dropdown_menu(move |menu, window, cx| {
                design::restore_dropdown_menu_focus(
                    menu.focus_handle(cx),
                    focus.clone(),
                    window,
                    cx,
                );
                let state = source.read(cx);
                let text = state.value();
                let lines = selected_lines(&text, &state.selected_range());
                menu.action_context(focus.clone())
                    .menu_with_disabled(
                        "Toggle Line Comment",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::ToggleComment,
                        }),
                        !yaml,
                    )
                    .separator()
                    .menu(
                        "Indent Lines",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::Indent,
                        }),
                    )
                    .menu(
                        "Outdent Lines",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::Outdent,
                        }),
                    )
                    .separator()
                    .menu(
                        "Duplicate Lines Up",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::DuplicateUp,
                        }),
                    )
                    .menu(
                        "Duplicate Lines Down",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::DuplicateDown,
                        }),
                    )
                    .menu(
                        "Insert Line Above",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::InsertAbove,
                        }),
                    )
                    .menu(
                        "Insert Line Below",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::InsertBelow,
                        }),
                    )
                    .menu_with_disabled(
                        "Move Lines Up",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::MoveUp,
                        }),
                        lines.start == 0,
                    )
                    .menu_with_disabled(
                        "Move Lines Down",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::MoveDown,
                        }),
                        lines.end == text.len(),
                    )
                    .menu_with_disabled(
                        "Delete Lines",
                        Box::new(SourceLineCommand {
                            operation: LineOperation::Delete,
                        }),
                        text.is_empty(),
                    )
            })
    }

    fn source_line_command(
        &mut self,
        action: &SourceLineCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.source.read(cx);
        if !state.focus_handle(cx).is_focused(window) {
            cx.propagate();
            return;
        }
        if !self.source_editable(cx) {
            return;
        }
        let text = state.value().to_string();
        // Like VS Code, line insertion follows the caret's line rather than the selected block.
        let selection = match action.operation {
            LineOperation::InsertBelow | LineOperation::InsertAbove => {
                state.cursor()..state.cursor()
            }
            _ => state.selected_range(),
        };
        if action.operation == LineOperation::ToggleComment
            && self.session.open.as_ref().unwrap().document.format != Format::Yaml
        {
            return;
        }
        let edit = line_edit(&text, selection, action.operation);
        self.apply_source_line_edit(&text, edit, window, cx);
    }

    /// Whether Source may apply a command edit: idle, open, and editable.
    fn source_editable(&self, cx: &App) -> bool {
        !self.busy && self.source.read(cx).is_editable() && self.session.open.is_some()
    }

    /// The focused Source caret when no cursor selects text, so clipboard commands act line-wise.
    ///
    /// Kit exposes only the active selection, but `is_copyable` covers every cursor: any selected
    /// text keeps Kit's native copy, cut, and paste.
    fn source_line_caret(&self, window: &Window, cx: &App) -> Option<usize> {
        let state = self.source.read(cx);
        (state.focus_handle(cx).is_focused(window) && !state.is_copyable())
            .then_some(state.cursor())
    }

    /// Applies one line edit, computed from `text`, as its own document history step.
    fn apply_source_line_edit(
        &mut self,
        text: &str,
        edit: Option<LineEdit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.capture_source_draft(cx) {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        self.session
            .open
            .as_mut()
            .unwrap()
            .document
            .break_source_history();
        let Some(edit) = edit else {
            return;
        };
        let range = SourcePositions::new(text).range(edit.range);
        self.source.update(cx, |state, cx| {
            state.apply_lsp_edits(
                &vec![TextEdit {
                    range,
                    new_text: edit.text,
                }],
                window,
                cx,
            );
            // Kit 0.7.3 collapses reversed ranges; keep the edited selection forward.
            state.set_selected_range(edit.selection, cx);
        });
        if let Err(error) = self.capture_source_draft(cx) {
            self.status = format!("{error:#}");
        }
        self.session
            .open
            .as_mut()
            .unwrap()
            .document
            .break_source_history();
        self.defer_source_feedback(window, cx);
        cx.notify();
    }

    /// Copies or cuts the caret's whole line when no cursor selects text, like VS Code.
    ///
    /// Kit copies only selected text, so without a selection Ctrl+C and Ctrl+X did nothing.
    fn source_copy_line(&mut self, cut: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(caret) = self.source_line_caret(window, cx) else {
            return;
        };
        cx.stop_propagation();
        if cut && !self.source_editable(cx) {
            return;
        }
        let text = self.source.read(cx).value().to_string();
        // An empty document has no line to copy, so the clipboard stays as it was.
        let Some(line) = copied_line(&text, caret) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(
            line,
            SOURCE_LINE_CLIPBOARD.into(),
        ));
        if cut {
            let edit = line_edit(&text, caret..caret, LineOperation::Delete);
            self.apply_source_line_edit(&text, edit, window, cx);
        }
    }

    /// Pastes a line copied from an empty selection above the caret's line.
    ///
    /// Other clipboard text, and any paste over a selection, stays with Kit's inline paste.
    /// Kit does not expose additional empty cursors, so a line paste uses only the active caret.
    fn source_paste_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(caret) = self.source_line_caret(window, cx) else {
            return;
        };
        let Some(line) = cx
            .read_from_clipboard()
            .filter(|item| item.metadata().map(String::as_str) == Some(SOURCE_LINE_CLIPBOARD))
            .and_then(|item| item.text())
            .filter(|line| line.ends_with('\n'))
        else {
            return;
        };
        cx.stop_propagation();
        if !self.source_editable(cx) {
            return;
        }
        let text = self.source.read(cx).value().to_string();
        let edit = pasted_line(&text, caret, &line);
        self.apply_source_line_edit(&text, Some(edit), window, cx);
    }

    /// Selects whole lines, then extends by one line per press. Selection changes need no edit guards.
    fn select_source_lines(
        &mut self,
        _: &SelectSourceLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.source.read(cx);
        if !state.focus_handle(cx).is_focused(window) {
            cx.propagate();
            return;
        }
        let range = expanded_line_selection(&state.value(), state.selected_range());
        self.source
            .update(cx, |state, cx| state.set_selected_range(range, cx));
    }

    /// Turns native Home and Shift+Home into VS Code's smart Home.
    ///
    /// Kit's public API cannot set a reversed selection, so Kit still moves first: it keeps the
    /// selection anchor and stops at wrapped rows. A deferred step then crosses the line's ASCII
    /// indentation, one character per step.
    fn source_smart_home(&mut self, select: bool, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::{base::actions::SelectRight, component::input::MoveRight};
        let state = self.source.read(cx);
        if !state.focus_handle(cx).is_focused(window) {
            return;
        }
        let text = state.value();
        let cursor = state.cursor();
        let selection = state.selected_range();
        let Some(steps) = smart_home_steps(&text, cursor) else {
            return;
        };
        let start = line_start(&text, cursor);
        // The steps would move every cursor by this line's indentation, possibly into later lines,
        // so collapse to the active selection first. A reversed selection cannot be re-created
        // with its anchor through the public API; it keeps Kit's cursors (see OUTSTANDING_ISSUES).
        if !select || selection.end == cursor {
            let range = if select { selection } else { cursor..cursor };
            self.source
                .update(cx, |state, cx| state.set_selected_range(range, cx));
        }
        let source = self.source.clone();
        window.defer(cx, move |window, cx| {
            let focus = source.read(cx).focus_handle(cx);
            // Kit stopped at a wrapped row's start: keep native visual-row Home there.
            if !focus.is_focused(window) || source.read(cx).cursor() != start {
                return;
            }
            for _ in 0..steps {
                if select {
                    focus.dispatch_action(&SelectRight, window, cx);
                } else {
                    focus.dispatch_action(&MoveRight, window, cx);
                }
            }
        });
    }

    pub(super) fn source_window_activation_changed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !window.is_window_active() {
            self.source_focus_restore = self
                .session
                .workspace
                .as_ref()
                .zip(self.session.open.as_ref())
                .filter(|_| self.source.read(cx).focus_handle(cx).is_focused(window))
                .map(|(workspace, open)| (workspace.root.clone(), open.entry.path.clone()));
            return;
        }
        let Some((root, path)) = self.source_focus_restore.take() else {
            return;
        };
        // Restore only the same visible editor, without taking focus from another control.
        if self.busy
            || self.main_tab != 0
            || self.overview
            || self.tab != 1
            || self
                .session
                .workspace
                .as_ref()
                .is_none_or(|workspace| workspace.root != root)
            || self
                .session
                .open
                .as_ref()
                .is_none_or(|open| open.entry.path != path)
            || window.focused(cx).is_some_and(|focus| focus != self.focus)
        {
            return;
        }
        window.focus(&self.source.read(cx).focus_handle(cx), cx);
    }

    pub(super) fn apply_editor_display(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.source_wrap = self.preferences.source_wrap;
        self.source_line_numbers = self.preferences.source_line_numbers;
        self.source_font_size = f32::from(self.preferences.source_font_size.clamp(10, 24));
        self.source.update(cx, |state, cx| {
            state.set_soft_wrap(self.source_wrap, window, cx);
            state.set_line_number(self.source_line_numbers, window, cx);
        });
        if !self
            .source_font_size_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            self.restore_source_size(window, cx);
        }
        cx.notify();
    }

    pub(super) fn change_source_size(
        &mut self,
        size: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !(10..=24).contains(&size) || size == self.preferences.source_font_size {
            return;
        }
        self.preferences.source_font_size = size;
        self.apply_editor_display(window, cx);
        self.save_preferences(window, cx);
    }

    pub(super) fn restore_source_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.preferences.source_font_size.to_string();
        self.source_font_size_input.update(cx, |state, cx| {
            if state.value().as_ref() != value {
                state.set_value(value, window, cx);
            }
        });
    }

    pub(super) fn step_source_size(
        &mut self,
        action: StepAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(size) = self.source_font_size_input.read(cx).value().parse::<u8>() else {
            return;
        };
        if !(10..=24).contains(&size) {
            return;
        }
        let next = match action {
            StepAction::Increment => size.saturating_add(1),
            StepAction::Decrement => size.saturating_sub(1),
        };
        self.change_source_size(next, window, cx);
        self.restore_source_size(window, cx);
    }

    fn route_completion<Command: Action + Clone>(
        &mut self,
        action: &Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.busy && self.source.read(cx).completion_menu_state().open {
            let handled = self.source.update(cx, |state, cx| {
                state.route_overlay_action(Box::new(action.clone()), window, cx)
            });
            if handled {
                cx.stop_propagation();
            }
        }
    }

    pub(super) fn request_completion(
        &mut self,
        _: &CompleteSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy
            || self.tab != 1
            || self.session.open.is_none()
            || self.source.read(cx).lsp().completion_provider.is_none()
            || !self.source.read(cx).is_editable()
        {
            return;
        }
        self.source
            .update(cx, |state, cx| state.show_completions(window, cx));
    }

    pub(super) fn configure_source(&mut self, cx: &mut Context<Self>) {
        let configuration = self
            .session
            .open
            .as_ref()
            .map(|open| (open.entry.kind, open.document.format));
        let language = match configuration.map(|(_, format)| format) {
            Some(Format::Yaml) => "yaml",
            Some(Format::Json) => "json",
            Some(Format::Text) | None => "text",
        };
        self.source.update(cx, |state, cx| {
            if state.language_name().as_ref() != language {
                state.set_highlighter(language, cx);
            }
            state.dismiss_completion_overlay(cx);
            clear_source_popovers(state, cx);
            state.lsp_mut().completion_provider = None;
            state.lsp_mut().hover_provider = None;
        });
        let Some((kind, format)) = configuration
            .filter(|(kind, format)| *kind != DocumentKind::Data && *format != Format::Text)
        else {
            return;
        };
        let catalog = match &self.source_catalog {
            Some(catalog) => catalog.clone(),
            None => match CompletionCatalog::new() {
                Ok(catalog) => {
                    let catalog = Arc::new(catalog);
                    self.source_catalog = Some(catalog.clone());
                    catalog
                }
                Err(error) => {
                    self.status = format!("Completion unavailable: {error:#}");
                    return;
                }
            },
        };
        let provider = Rc::new(SourceCompletions {
            catalog,
            kind,
            format,
            workspace: self.session.workspace.clone(),
        });
        self.source.update(cx, |state, _| {
            state.lsp_mut().completion_provider = Some(provider.clone());
            state.lsp_mut().hover_provider = Some(provider);
            state.lsp_mut().completion_menu.max_width = px(440.);
        });
    }

    pub(super) fn update_source_diagnostics(&mut self, cx: &mut Context<Self>) {
        let issues = self.source_issues.clone();
        self.source.update(cx, |state, cx| {
            clear_source_popovers(state, cx);
            let text = state.text().clone();
            if let Some(diagnostics) = state.diagnostics_mut() {
                diagnostics.reset(&text);
                for issue in issues {
                    diagnostics.push(issue);
                }
            }
            cx.notify();
        });
    }

    fn jump_to_source_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.source.read(cx).cursor_position();
        if let Some(issue) = self
            .source_issues
            .iter()
            .find(|issue| issue.range.start > cursor)
            .or_else(|| self.source_issues.first())
        {
            self.go_to_source_position(issue.range.start, window, cx);
        }
    }

    fn go_to_source_position(
        &mut self,
        position: Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || (self.tab != 1 && !self.select_editor_tab(1, window, cx)) {
            return;
        }
        self.source.update(cx, |state, cx| {
            state.unfold_at(position, cx);
            state.set_cursor_position(position, window, cx);
            window.focus(&state.focus_handle(cx), cx);
        });
    }

    pub(super) fn render_source_diagnostics(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let summary = self.diagnostic.as_ref()?;
        let rows = self
            .source_issues
            .iter()
            .enumerate()
            .map(|(index, issue)| {
                let position = issue.range.start;
                let description = self.source_issue_markdown[index].clone();
                let (severity, color) =
                    if issue.severity == Some(lsp_types::DiagnosticSeverity::WARNING) {
                        ("Warning", cx.theme().warning)
                    } else {
                        ("Error", cx.theme().danger)
                    };
                div()
                    .id(("source-diagnostic-row", index))
                    .role(Role::Row)
                    .aria_label(format!("Issue on line {}", position.line + 1))
                    .flex()
                    .items_start()
                    .gap_3()
                    .px_4()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .w_full()
                    .min_w_0()
                    .text_sm()
                    .line_height(px(20.))
                    .child(
                        div()
                            .id(("source-diagnostic-line", index))
                            .role(Role::Cell)
                            .w(px(64.))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .child(
                                Button::new(("source-diagnostic", index))
                                    .ghost()
                                    .label((position.line + 1).to_string())
                                    .accessibility_label(format!(
                                        "Go to issue on line {}",
                                        position.line + 1
                                    ))
                                    .tooltip("Go to this issue")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.go_to_source_position(position, window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id(("source-diagnostic-type", index))
                            .role(Role::Cell)
                            .aria_label(severity)
                            .test_support()
                            .w(px(48.))
                            .h(px(20.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .id(("source-diagnostic-dot", index))
                                    .test_support()
                                    .size(px(8.))
                                    .child(
                                        gpui_kit::component::badge::Badge::new()
                                            .dot()
                                            .color(color)
                                            .child(div().size(px(8.))),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id(("source-diagnostic-message", index))
                            .role(Role::Cell)
                            .aria_label(issue.message.clone())
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .text_color(cx.theme().foreground)
                            .child(
                                TextView::markdown(("source-diagnostic-text", index), description)
                                    .selectable(true)
                                    .selection_format(SelectionFormat::Plain)
                                    .scrollable(false)
                                    .text_sm()
                                    .line_height(px(20.)),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        Some(
            div()
                .id("diagnostic")
                .role(Role::Alert)
                .aria_label(summary.clone())
                .test_support()
                .flex()
                .flex_col()
                .h(px(if self.source_issues.len() == 1 {
                    112.
                } else {
                    160.
                }))
                .flex_shrink_0()
                .border_t_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .child(
                    div()
                        .id("source-issues-table")
                        .role(Role::Table)
                        .aria_label("Source issues")
                        .test_support()
                        .flex()
                        .flex_col()
                        .size_full()
                        .child(
                            div()
                                .id("source-issues-header")
                                .role(Role::Row)
                                .flex()
                                .flex_shrink_0()
                                .gap_3()
                                .px_4()
                                .py_1()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .children([("Line", 64.), ("Type", 48.)].map(|(label, width)| {
                                    div()
                                        .id(label)
                                        .role(Role::ColumnHeader)
                                        .aria_label(label)
                                        .test_support()
                                        .w(px(width))
                                        .flex_shrink_0()
                                        .text_center()
                                        .child(label)
                                }))
                                .child(
                                    div()
                                        .id("Description")
                                        .role(Role::ColumnHeader)
                                        .aria_label("Description")
                                        .test_support()
                                        .flex_1()
                                        .child("Description"),
                                ),
                        )
                        .child(
                            div()
                                .id("source-issues-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scrollbar()
                                .child(div().flex().flex_col().children(rows)),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_source(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_kit::component::input::{
            Copy, Cut, Enter, Escape, IndentInline, MoveDown, MoveHome, MoveUp, OutdentInline,
            Paste, Redo, SelectToStartOfLine, Undo,
        };
        let cursor = self.source.read(cx).cursor_position();
        let location = format!("Ln {}, Col {}", cursor.line + 1, cursor.character + 1);
        let language = self.source.read(cx).language_name().to_uppercase();
        let menu = self.source.read(cx).completion_menu_state();
        let suggestions = menu.open.then(|| {
            format!(
                "{} {}",
                menu.items.len(),
                if menu.items.len() == 1 {
                    "suggestion"
                } else {
                    "suggestions"
                }
            )
        });
        div()
            .id("source-editor")
            .key_context("RegressifySource")
            .capture_action(cx.listener(|this, _: &Undo, window, cx| {
                let state = this.source.read(cx);
                if state.focus_handle(cx).is_focused(window) {
                    if state.is_editable() {
                        this.undo_document(&UndoDocument, window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Redo, window, cx| {
                let state = this.source.read(cx);
                if state.focus_handle(cx).is_focused(window) {
                    if state.is_editable() {
                        this.redo_document(&RedoDocument, window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .capture_action(
                cx.listener(|this, _: &Copy, window, cx| this.source_copy_line(false, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &Cut, window, cx| this.source_copy_line(true, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &Paste, window, cx| this.source_paste_line(window, cx)),
            )
            .capture_action(cx.listener(|this, _: &MoveHome, window, cx| {
                this.source_smart_home(false, window, cx)
            }))
            .capture_action(cx.listener(|this, _: &SelectToStartOfLine, window, cx| {
                this.source_smart_home(true, window, cx)
            }))
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                this.route_completion(action, window, cx)
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                let state = this.source.read(cx);
                if !state.focus_handle(cx).is_focused(window) {
                    return;
                }
                if state.completion_menu_state().open {
                    this.route_completion(
                        &Enter {
                            secondary: false,
                            shift: false,
                        },
                        window,
                        cx,
                    );
                } else if !state.selected_range().is_empty() {
                    this.source_line_command(
                        &SourceLineCommand {
                            operation: LineOperation::Indent,
                        },
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
                if this.source.read(cx).focus_handle(cx).is_focused(window) {
                    if this.source.read(cx).completion_menu_state().open {
                        this.source
                            .update(cx, |state, cx| state.dismiss_completion_overlay(cx));
                    } else {
                        this.source_line_command(
                            &SourceLineCommand {
                                operation: LineOperation::Outdent,
                            },
                            window,
                            cx,
                        );
                    }
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, action: &MoveDown, window, cx| {
                this.route_completion(action, window, cx)
            }))
            .capture_action(cx.listener(|this, action: &MoveUp, window, cx| {
                this.route_completion(action, window, cx)
            }))
            .capture_action(cx.listener(|this, action: &Escape, window, cx| {
                if this.source.read(cx).completion_menu_state().open {
                    this.route_completion(action, window, cx);
                } else {
                    this.source.update(cx, clear_source_popovers);
                }
            }))
            .on_action(cx.listener(Self::request_completion))
            .on_action(cx.listener(Self::source_line_command))
            .on_action(cx.listener(Self::select_source_lines))
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .id("source-toolbar")
                    .role(Role::Group)
                    .aria_label("Source commands")
                    .test_support()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(design::ToolbarFocus::new(
                        "source-toolbar-focus",
                        gpui_kit::component::toolbar::Toolbar::new("source-command-toolbar")
                            .with_size(gpui_kit::component::Size::Medium)
                            .h_auto()
                            .flex_wrap()
                            .content(self.source_edit_menu(cx))
                            .child(
                                Button::new("source-find")
                                    .ghost()
                                    .icon(IconName::Search)
                                    .accessibility_label("Find in source")
                                    .tooltip("Find in source")
                                    .disabled(self.busy || self.session.open.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.source
                                            .update(cx, |state, cx| state.open_search(false, cx))
                                    })),
                            )
                            .child(
                                Button::new("source-replace")
                                    .ghost()
                                    .icon(gpui_kit::assets::IconName::Replace)
                                    .accessibility_label("Replace in source")
                                    .tooltip("Find and replace")
                                    .disabled(self.busy || self.session.open.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.source
                                            .update(cx, |state, cx| state.open_search(true, cx))
                                    })),
                            )
                            .child(
                                Button::new("source-suggest")
                                    .ghost()
                                    .icon(gpui_kit::assets::IconName::ListFilter)
                                    .accessibility_label("Suggest")
                                    .tooltip("Suggest (Ctrl+Space)")
                                    .disabled(
                                        self.busy
                                            || self.session.open.is_none()
                                            || self
                                                .source
                                                .read(cx)
                                                .lsp()
                                                .completion_provider
                                                .is_none(),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.request_completion(&CompleteSource, window, cx)
                                    })),
                            )
                            .content(
                                gpui_kit::component::separator::Separator::vertical()
                                    .h_4()
                                    .mx_2(),
                            )
                            .child(
                                Checkbox::new("source-wrap")
                                    .label("Wrap")
                                    .checked(self.source_wrap)
                                    .on_change(cx.listener(|this, checked, window, cx| {
                                        this.preferences.source_wrap = *checked;
                                        this.apply_editor_display(window, cx);
                                        this.save_preferences(window, cx);
                                    })),
                            )
                            .child(
                                Checkbox::new("source-line-numbers")
                                    .label("Line numbers")
                                    .checked(self.source_line_numbers)
                                    .on_change(cx.listener(|this, checked, window, cx| {
                                        this.preferences.source_line_numbers = *checked;
                                        this.apply_editor_display(window, cx);
                                        this.save_preferences(window, cx);
                                    })),
                            )
                            .content(div().flex_1())
                            .child(
                                Button::new("source-reset-size")
                                    .ghost()
                                    .icon(gpui_kit::assets::IconName::RotateCcw)
                                    .tooltip("Reset text size")
                                    .accessibility_label("Reset text size")
                                    .disabled(self.source_font_size == 14.)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.change_source_size(14, window, cx);
                                        this.restore_source_size(window, cx);
                                    })),
                            )
                            .content(
                                div()
                                    .id("source-text-size")
                                    .role(Role::Group)
                                    .aria_label("Source font size")
                                    .test_support()
                                    .w(px(132.))
                                    .flex_shrink_0()
                                    .child(
                                        NumberInput::new(&self.source_font_size_input)
                                            .accessibility_label("Source font size")
                                            .with_size(gpui_kit::component::Size::Medium),
                                    ),
                            ),
                    )),
            )
            .child(
                div().flex_1().min_h_0().child(
                    CodeEditor::new(&self.source)
                        .aria_label("Configuration source")
                        .bordered(false)
                        .text_size(px(self.source_font_size))
                        .size_full()
                        .disabled(self.busy || self.session.open.is_none()),
                ),
            )
            .child(
                StatusBar::new()
                    .flex_shrink_0()
                    .left(
                        div()
                            .id("source-language")
                            .role(Role::Status)
                            .aria_label(language.clone())
                            .test_support()
                            .child(language),
                    )
                    .child(
                        div()
                            .id("source-position")
                            .role(Role::Status)
                            .aria_label(location.clone())
                            .test_support()
                            .child(location),
                    )
                    .child(div().flex_1())
                    .when_some(suggestions, |bar, suggestions| {
                        bar.right(
                            div()
                                .id("source-completion-status")
                                .role(Role::Status)
                                .aria_label(suggestions.clone())
                                .test_support()
                                .child(suggestions),
                        )
                    })
                    .when(self.diagnostic.is_some(), |bar| {
                        bar.right(
                            Button::new("source-issue")
                                .ghost()
                                .icon(gpui_kit::assets::IconName::CircleAlert)
                                .label(format!(
                                    "{} {}",
                                    self.source_issues.len(),
                                    if self.source_issues.len() == 1 {
                                        "issue"
                                    } else {
                                        "issues"
                                    }
                                ))
                                .text_color(cx.theme().danger)
                                .tooltip("Go to next issue")
                                .disabled(self.source_issues.is_empty() || self.busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.jump_to_source_issue(window, cx)
                                })),
                        )
                    })
                    .when(self.source_feedback.pending, |bar| {
                        bar.right(
                            div()
                                .id("source-validation-pending")
                                .role(Role::Status)
                                .aria_label("Checking source")
                                .test_support()
                                .text_color(cx.theme().muted_foreground)
                                .child("Checking source..."),
                        )
                    })
                    .when(
                        self.diagnostic.is_none() && !self.source_feedback.pending,
                        |bar| {
                            bar.right(
                                Icon::new(gpui_kit::assets::IconName::CircleCheck)
                                    .xsmall()
                                    .text_color(cx.theme().success),
                            )
                        },
                    ),
            )
            .into_any_element()
    }
}

pub(super) fn error_line(error: &anyhow::Error) -> Option<u32> {
    error
        .downcast_ref::<serde_yaml::Error>()
        .and_then(|error| error.location())
        .map(|location| location.line())
        .or_else(|| {
            error
                .downcast_ref::<serde_json::Error>()
                .map(|error| error.line())
                .filter(|line| *line > 0)
        })
        .map(|line| line.saturating_sub(1) as u32)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    #[cfg(feature = "gui-tests")]
    use gpui_kit::AppContext;

    use super::{
        LineOperation, SourceCompletions, copied_line, expanded_line_selection, line_edit,
        pasted_line, smart_home_steps,
    };
    use crate::desktop::{
        completion::CompletionCatalog,
        document::{DocumentKind, Format},
    };

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    fn source_assistance_viewport_punctuation_keeps_completion_triggered(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::component::input::CompletionProvider;

        let provider = SourceCompletions {
            catalog: Arc::new(CompletionCatalog::new().unwrap()),
            kind: DocumentKind::Suite,
            format: Format::Yaml,
            workspace: None,
        };
        cx.update(|cx| {
            for text in ["'", "-", "phone-wide"] {
                assert!(provider.is_completion_trigger(0, text, cx), "{text}");
            }
            for text in ["", "\n", "\t"] {
                assert!(!provider.is_completion_trigger(0, text, cx), "{text:?}");
            }
        });
    }

    #[test]
    fn source_feedback_indexes_lines_and_reuses_identical_positions() {
        let text = format!("{}label: '\u{1f680}'\r\n", "line\n".repeat(5000));
        let start = text.find("label").unwrap();
        let mut positions = super::SourcePositions::new(&text);
        for _ in 0..1000 {
            assert_eq!(
                positions.range(start..start + "label".len()),
                lsp_types::Range::new(
                    lsp_types::Position::new(5000, 0),
                    lsp_types::Position::new(5000, 5),
                )
            );
        }
        assert_eq!(positions.positions.len(), 2);
        assert_eq!(positions.line_starts.len(), 5002);
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn source_feedback_coalesces_bursts_and_runs_only_one_worker(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::test::TestAppContextExt;

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                assert!(this.select_editor_tab(1, window, cx));
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), super::Duration::from_secs(3), |_, cx| {
            let feedback = &view.read(cx).source_feedback;
            !feedback.pending && feedback.worker.is_none()
        })
        .await;
        let mut started = cx.update(|cx| view.read(cx).source_feedback.started);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                for index in 0..20 {
                    this.source.update(cx, |state, cx| {
                        state.replace_all(format!("scenarios: []\n# {index}\n"), window, cx);
                    });
                    assert!(this.capture_source_draft(cx).unwrap());
                    this.defer_source_feedback(window, cx);
                    assert_eq!(this.source_feedback.started, started);
                    assert!(this.source_feedback.worker.is_none());
                }
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), super::Duration::from_secs(3), |_, cx| {
            !view.read(cx).source_feedback.pending
        })
        .await;
        started += 1;
        cx.update(|cx| {
            assert_eq!(view.read(cx).source_feedback.started, started);
            assert_eq!(
                view.read(cx).source_feedback.key.as_ref().unwrap().text,
                "scenarios: []\n# 19\n"
            );
        });

        for revisit in [false, true] {
            let latest = "scenarios: []\n# latest\n";
            let old = if revisit {
                latest
            } else {
                "scenarios: []\noldUnknown: true\n"
            };
            cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, cx| {
                    this.source
                        .update(cx, |state, cx| state.replace_all(old, window, cx));
                    this.capture_source_draft(cx).unwrap();
                    this.defer_source_feedback(window, cx);
                    this.source_feedback.debounce = None;
                    this.source_feedback.ready = true;
                    this.start_source_feedback(window, cx);
                    assert!(this.source_feedback.worker.is_some());
                    assert_eq!(this.source_feedback.started, started + 1);

                    for draft in ["scenarios: []\n# intermediate\n", latest] {
                        this.source
                            .update(cx, |state, cx| state.replace_all(draft, window, cx));
                        this.capture_source_draft(cx).unwrap();
                        this.defer_source_feedback(window, cx);
                    }
                    this.source_feedback.debounce = None;
                    this.source_feedback.ready = true;
                    this.start_source_feedback(window, cx);
                    assert_eq!(this.source_feedback.started, started + 1);
                });
            })
            .unwrap();
            cx.wait_for(handle.into(), super::Duration::from_secs(3), |_, cx| {
                let feedback = &view.read(cx).source_feedback;
                !feedback.pending && feedback.worker.is_none()
            })
            .await;
            started += 2;
            cx.update(|cx| {
                let this = view.read(cx);
                assert_eq!(this.source_feedback.started, started);
                assert_eq!(this.source_feedback.key.as_ref().unwrap().text, latest);
                assert!(this.source_issues.is_empty());
                assert_eq!(
                    this.session.selection,
                    crate::desktop::session::Selection::Root
                );
            });
        }
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn source_feedback_waits_for_native_change_acknowledgement(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::test::TestAppContextExt;

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                assert!(this.select_editor_tab(1, window, cx));
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), super::Duration::from_secs(3), |_, cx| {
            !view.read(cx).source_feedback.pending && view.read(cx).source_feedback.worker.is_none()
        })
        .await;
        let started = cx.update(|cx| view.read(cx).source_feedback.started);
        let draft = "scenarios: []\n# native change awaiting acknowledgement\n";
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.source_feedback.invalidate();
                this.schedule_source_feedback(window, cx);
                this.source
                    .update(cx, |state, cx| state.replace_all(draft, window, cx));
                assert!(!this.source_matches_document(cx));
                this.source_feedback.debounce = None;
                this.source_feedback.ready = true;
                this.start_source_feedback(window, cx);
                assert_eq!(this.source_feedback.started, started);
                assert!(this.source_feedback.worker.is_none());
                assert!(this.unsaved(cx));
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), super::Duration::from_secs(3), |_, cx| {
            !view.read(cx).source_feedback.pending
        })
        .await;
        cx.update(|cx| {
            assert_eq!(view.read(cx).source_feedback.started, started + 1);
            assert_eq!(
                view.read(cx).source_feedback.key.as_ref().unwrap().text,
                draft
            );
        });
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    fn source_feedback_reuses_catalog_and_rendered_diagnostic_text(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::test::TestWindowExt;

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            let text = view.update(cx, |this, cx| {
                this.cancel_source_feedback();
                this.tab = 1;
                let catalog = this.source_catalog.as_ref().unwrap().clone();
                this.configure_source(cx);
                assert!(Arc::ptr_eq(&catalog, this.source_catalog.as_ref().unwrap()));
                let issues = vec![super::source_diagnostic(
                    lsp_types::Range::default(),
                    "Literal <field>\n\nKeep \\ and * unchanged.\n".into(),
                )];
                assert!(this.set_source_issues(issues.clone()));
                let text = this.source_issue_markdown[0].clone();
                assert!(!this.set_source_issues(issues));
                assert_eq!(text.as_ptr(), this.source_issue_markdown[0].as_ptr());
                text
            });
            for _ in 0..5 {
                window.render_frame(cx);
                assert_eq!(
                    text.as_ptr(),
                    view.read(cx).source_issue_markdown[0].as_ptr()
                );
            }
        })
        .unwrap();
    }

    #[test]
    fn source_feedback_plain_text_does_not_materialize_a_structured_value() {
        for text in ["", "hello\n", "\u{1f680}\r\n"] {
            let document =
                super::Document::new(DocumentKind::Data, Format::Text, text.into()).unwrap();
            let (value, issues) = super::inspect_document(&document);
            assert!(value.is_null());
            assert!(issues.is_empty());
            assert_eq!(document.source(), text);
        }
        let document =
            super::Document::new(DocumentKind::Data, Format::Text, "binary\0".into()).unwrap();
        let (_, issues) = super::inspect_document(&document);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("Binary content"));
    }

    #[test]
    fn source_validation_semantic_issues_keep_locations_alongside_schema_errors() {
        for (kind, source, expected_line, message) in [
            (
                DocumentKind::Suite,
                "scenarios:\n- url: '   '\nunknown: true\n",
                1,
                "needs a URL",
            ),
            (
                DocumentKind::Suite,
                "scenarios: []\nurlReplacements:\n- ref: '['\n  test: /\n  regex: true\n",
                2,
                "regex",
            ),
            (
                DocumentKind::Suite,
                "scenarios:\n- url: /\n  urlReplacements:\n  - ref: '['\n    test: /\n    regex: true\n",
                3,
                "regex",
            ),
            (
                DocumentKind::Viewports,
                "- label: phone\n  width: 375\n  height: 667\n- label: PHONE\n  width: 400\n  height: 700\n",
                3,
                "duplicate",
            ),
        ] {
            let document = super::Document::new(kind, Format::Yaml, source.into()).unwrap();
            let (_, diagnostics) = super::inspect_document(&document);
            let issue = diagnostics
                .iter()
                .find(|issue| issue.message.contains(message))
                .unwrap_or_else(|| panic!("Missing semantic issue: {diagnostics:?}"));
            assert_eq!(issue.range.start.line, expected_line, "{diagnostics:?}");
        }
    }

    #[test]
    fn source_validation_duplicate_keys_are_not_reported_as_clean() {
        let document = super::Document::new(
            DocumentKind::Suite,
            Format::Json,
            "{\n\"scenarios\":[],\n\"debug\":true,\n\"debug\":false\n}".into(),
        )
        .unwrap();
        let (_, issues) = super::inspect_document(&document);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].range.start.line, 3);
    }

    #[test]
    fn source_validation_diagnostic_markdown_never_adds_visible_backslashes() {
        for message in [
            "first\nsecond",
            "first\n\nsecond\n",
            "first\r\n\r\nsecond\r\n",
            "literal `[*] <x> & \\\n",
        ] {
            let markdown = super::diagnostic_markdown(message);
            let visible = pulldown_cmark::Parser::new(&markdown)
                .filter_map(|event| match event {
                    pulldown_cmark::Event::Text(text) | pulldown_cmark::Event::Code(text) => {
                        Some(text.into_string())
                    }
                    _ => None,
                })
                .collect::<String>();
            assert_eq!(visible, message.lines().collect::<String>(), "{markdown:?}");
        }
    }

    #[test]
    fn action_viewport_diagnostics_preserve_the_combined_issue_bound() {
        let document = crate::desktop::document::Document::new(
            DocumentKind::Suite,
            Format::Json,
            serde_json::json!({"scenarios": [{"url": ""}]}).to_string(),
        )
        .unwrap();
        let (_, diagnostics) = super::inspect_document(&document);
        assert_eq!(diagnostics.len(), 1);
        let original = diagnostics[0].message.clone();
        let limit = crate::desktop::validation::MAX_DIAGNOSTICS;
        let contextual = (0..limit)
            .map(|_| crate::desktop::validation::Diagnostic {
                pointer: "/scenarios/0/url".into(),
                message: "Invalid viewportNames".into(),
            })
            .collect::<Vec<_>>();
        let diagnostics = super::inspect_source_document(&document, &contextual, None).issues;
        assert_eq!(diagnostics.len(), limit);
        assert!(diagnostics.iter().any(|issue| issue.message == original));
        let mut contextual = contextual;
        contextual.extend_from_within(..);
        let diagnostics = super::inspect_source_document(&document, &contextual, None).issues;
        assert_eq!(diagnostics.len(), limit);
    }

    fn edited_lines(
        text: &str,
        selection: std::ops::Range<usize>,
        operation: LineOperation,
    ) -> (String, std::ops::Range<usize>) {
        let edit = line_edit(text, selection, operation).expect("Expected a line edit");
        let mut result = text.to_string();
        result.replace_range(edit.range, &edit.text);
        assert!(result.is_char_boundary(edit.selection.start));
        assert!(result.is_char_boundary(edit.selection.end));
        (result, edit.selection)
    }

    #[test]
    fn source_ranges_preserve_non_ascii_whitespace_in_line_commands() {
        for whitespace in ["\u{00a0}", "\u{000c}", "\u{2003}", "\u{3000}"] {
            for ending in ["\n", "\r\n"] {
                let original = format!("# a{ending}{whitespace}{ending}");
                let (commented, selected) =
                    edited_lines(&original, 0..original.len(), LineOperation::ToggleComment);
                assert_eq!(commented, format!("# # a{ending}# {whitespace}{ending}"));
                assert_eq!(
                    edited_lines(&commented, selected, LineOperation::ToggleComment).0,
                    original
                );
                let (indented, selected) =
                    edited_lines(&original, 0..original.len(), LineOperation::Indent);
                assert_eq!(indented, format!("  # a{ending}  {whitespace}{ending}"));
                assert_eq!(
                    edited_lines(&indented, selected, LineOperation::Outdent).0,
                    original
                );
            }
        }
    }

    #[test]
    fn source_ranges_positions_match_editor_unicode_columns() {
        use gpui_kit::component::input::{Rope, RopeExt};

        let text = "label: '\u{1f680}\u{1f30d} caf\u{e9}'\r\ndelay: 12\n";
        let rope = Rope::from(text);
        let mut positions = super::SourcePositions::new(text);
        for offset in text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(text.len()))
        {
            let position = positions.position(offset);
            assert_eq!(position, rope.offset_to_position(offset), "byte {offset}");
            assert_eq!(rope.position_to_offset(&position), offset, "byte {offset}");
        }
    }

    #[test]
    fn source_line_commands_comments_preserve_indentation_unicode_and_crlf() {
        let original = "  label: caf\u{e9}\r\n\r\n  debug: true\r\nscenarios: []\r\n";
        let selection = 2..original.find("scenarios").unwrap();
        let (commented, selected) =
            edited_lines(original, selection.clone(), LineOperation::ToggleComment);
        assert_eq!(
            commented,
            "  # label: caf\u{e9}\r\n\r\n  # debug: true\r\nscenarios: []\r\n"
        );
        assert_eq!(
            edited_lines(&commented, selected, LineOperation::ToggleComment),
            (original.to_string(), selection)
        );
        assert_eq!(
            edited_lines(
                "#debug: true\n  # headless: true",
                0..30,
                LineOperation::ToggleComment
            )
            .0,
            "debug: true\n  headless: true"
        );
    }

    #[test]
    fn source_line_commands_comments_handle_mixed_and_empty_lines() {
        let original = "# existing\nnew\n";
        let (commented, selected) =
            edited_lines(original, 0..original.len(), LineOperation::ToggleComment);
        assert_eq!(commented, "# # existing\n# new\n");
        assert_eq!(
            edited_lines(&commented, selected, LineOperation::ToggleComment).0,
            original
        );
        assert_eq!(
            edited_lines("", 0..0, LineOperation::ToggleComment),
            ("# ".into(), 2..2)
        );
        assert_eq!(
            edited_lines("\n", 0..0, LineOperation::ToggleComment),
            ("# \n".into(), 2..2)
        );
        assert!(line_edit(" \n\n", 0..3, LineOperation::ToggleComment).is_none());
    }

    #[test]
    fn source_line_commands_indent_only_selected_lines() {
        let original = "  a\r\n\r\n b\r\nc\r\n";
        let selection = 0..original.find('c').unwrap();
        let (indented, selected) = edited_lines(original, selection, LineOperation::Indent);
        assert_eq!(indented, "    a\r\n\r\n   b\r\nc\r\n");
        assert_eq!(
            edited_lines(&indented, selected, LineOperation::Outdent).0,
            original
        );
        assert_eq!(
            edited_lines("\ta\n b", 0..5, LineOperation::Outdent),
            ("a\nb".into(), 0..3)
        );
        assert_eq!(
            edited_lines("", 0..0, LineOperation::Indent),
            ("  ".into(), 2..2)
        );
        assert!(line_edit("text", 0..4, LineOperation::Outdent).is_none());
    }

    #[test]
    fn source_line_commands_duplicate_selected_blocks_and_unterminated_lines() {
        for (original, selection, operation, expected, selected) in [
            (
                "a\nb\nc",
                0..4,
                LineOperation::DuplicateDown,
                "a\nb\na\nb\nc",
                4..8,
            ),
            (
                "a\nb\nc",
                2..4,
                LineOperation::DuplicateUp,
                "a\nb\nb\nc",
                2..4,
            ),
            ("a\nb", 2..3, LineOperation::DuplicateDown, "a\nb\nb", 4..5),
            ("a\nb", 2..3, LineOperation::DuplicateUp, "a\nb\nb", 2..3),
            (
                "a\r\nb",
                3..4,
                LineOperation::DuplicateDown,
                "a\r\nb\r\nb",
                6..7,
            ),
            ("", 0..0, LineOperation::DuplicateDown, "\n", 1..1),
            ("a\n", 2..2, LineOperation::DuplicateUp, "a\n\n", 2..2),
        ] {
            assert_eq!(
                edited_lines(original, selection, operation),
                (expected.into(), selected),
                "{operation:?}: {original:?}"
            );
        }
    }

    #[test]
    fn source_line_commands_move_blocks_without_changing_line_endings() {
        for (original, selection, operation, expected, selected) in [
            (
                "a\nb\nc\nd\n",
                2..6,
                LineOperation::MoveUp,
                "b\nc\na\nd\n",
                0..4,
            ),
            (
                "a\nb\nc\nd\n",
                2..6,
                LineOperation::MoveDown,
                "a\nd\nb\nc\n",
                4..8,
            ),
            ("a\nb", 2..3, LineOperation::MoveUp, "b\na", 0..1),
            ("a\nb", 0..2, LineOperation::MoveDown, "b\na", 2..3),
            ("a\r\nb", 3..4, LineOperation::MoveUp, "b\r\na", 0..1),
            ("a\r\nb", 0..3, LineOperation::MoveDown, "b\r\na", 3..4),
            ("a\n", 2..2, LineOperation::MoveUp, "\na", 0..0),
            (
                "\u{e9}\n\u{1f680}",
                3..7,
                LineOperation::MoveUp,
                "\u{1f680}\n\u{e9}",
                0..4,
            ),
        ] {
            assert_eq!(
                edited_lines(original, selection, operation),
                (expected.into(), selected),
                "{operation:?}: {original:?}"
            );
        }
        assert!(line_edit("a\nb", 0..0, LineOperation::MoveUp).is_none());
        assert!(line_edit("a\nb\n", 2..2, LineOperation::MoveDown).is_none());
        assert!(line_edit("", 0..0, LineOperation::MoveDown).is_none());
    }

    #[test]
    fn source_line_commands_delete_whole_lines_including_the_last_line() {
        for (original, selection, expected, selected) in [
            ("a\nb\nc\n", 2..4, "a\nc\n", 2..2),
            ("a\nb", 2..2, "a", 1..1),
            ("a\r\nb", 3..4, "a", 1..1),
            ("a\nb\n", 4..4, "a\nb", 3..3),
            ("a\nb\n", 0..4, "", 0..0),
        ] {
            assert_eq!(
                edited_lines(original, selection, LineOperation::Delete),
                (expected.into(), selected),
                "{original:?}"
            );
        }
        assert!(line_edit("", 0..0, LineOperation::Delete).is_none());
    }

    #[test]
    fn source_line_clipboard_copies_terminated_lines() {
        for (original, cursor, expected) in [
            ("a\nb\nc\n", 2, "b\n"),
            ("a\nb\nc\n", 3, "b\n"),
            ("a\r\nb", 4, "b\r\n"),
            ("a\nb", 3, "b\n"),
            ("a\nb\n", 4, "\n"),
            ("\n", 0, "\n"),
            ("only", 2, "only\n"),
            ("\u{e9}\n\u{1f680}", 3, "\u{1f680}\n"),
        ] {
            assert_eq!(
                copied_line(original, cursor).as_deref(),
                Some(expected),
                "{original:?} at {cursor}"
            );
        }
        assert_eq!(copied_line("", 0), None);
    }

    #[test]
    fn source_line_clipboard_pastes_above_the_caret_line() {
        for (original, cursor, line, expected, caret) in [
            ("a\nbc\n", 3, "x\n", "a\nx\nbc\n", 5),
            ("a\nbc\n", 0, "x\n", "x\na\nbc\n", 2),
            ("a\nbc\n", 5, "x\n", "a\nbc\nx\n", 7),
            ("a\r\nb", 4, "x\r\n", "a\r\nx\r\nb", 7),
            ("", 0, "x\n", "x\n", 2),
            ("\u{e9}\n\u{1f680}", 7, "x\n", "\u{e9}\nx\n\u{1f680}", 9),
        ] {
            let edit = pasted_line(original, cursor, line);
            let mut result = original.to_string();
            result.replace_range(edit.range, &edit.text);
            assert_eq!(result, expected, "{original:?} at {cursor}");
            assert_eq!(edit.selection, caret..caret, "{original:?} at {cursor}");
            assert!(result.is_char_boundary(caret));
        }
    }

    #[test]
    fn source_line_selection_expands_to_whole_lines_then_the_next_line() {
        for (original, selection, expected) in [
            ("a\nbc\nd", 3..3, 2..5),
            ("a\nbc\nd", 2..5, 2..6),
            ("a\nbc\nd", 3..4, 2..5),
            ("a\nbc\nd", 0..3, 0..5),
            ("a\nbc\nd", 5..6, 5..6),
            ("a\r\nb\r\n", 0..0, 0..3),
            ("a\r\nb\r\n", 0..3, 0..6),
            ("a\n", 2..2, 2..2),
            ("", 0..0, 0..0),
        ] {
            assert_eq!(
                expanded_line_selection(original, selection.clone()),
                expected,
                "{original:?} from {selection:?}"
            );
        }
    }

    #[test]
    fn source_line_commands_insert_indented_lines_around_the_selection() {
        for (original, selection, operation, expected, selected) in [
            (
                "  a: 1\nb",
                3..3,
                LineOperation::InsertBelow,
                "  a: 1\n  \nb",
                9..9,
            ),
            (
                "  a: 1\nb",
                3..3,
                LineOperation::InsertAbove,
                "  \n  a: 1\nb",
                2..2,
            ),
            // Like Enter, a deeper next line or a bracket opener sets the new indentation.
            (
                "a:\n  - b\n",
                1..1,
                LineOperation::InsertBelow,
                "a:\n  \n  - b\n",
                5..5,
            ),
            ("{\n}", 0..0, LineOperation::InsertBelow, "{\n  \n}", 4..4),
            ("# [\n", 0..0, LineOperation::InsertBelow, "# [\n\n", 4..4),
            ("\tx", 1..1, LineOperation::InsertBelow, "\tx\n\t", 4..4),
            (
                "a\r\n b",
                4..4,
                LineOperation::InsertBelow,
                "a\r\n b\r\n ",
                8..8,
            ),
            (
                "a\r\n b",
                4..4,
                LineOperation::InsertAbove,
                "a\r\n \r\n b",
                4..4,
            ),
            (
                "a\nb\nc\n",
                0..3,
                LineOperation::InsertBelow,
                "a\nb\n\nc\n",
                4..4,
            ),
            // The caret's line, at the selection end, decides where the line goes.
            (
                "a\nb\nc\n",
                2..5,
                LineOperation::InsertAbove,
                "a\nb\n\nc\n",
                4..4,
            ),
            ("a\nb\n", 0..2, LineOperation::InsertBelow, "a\nb\n\n", 4..4),
            // Above is Enter at the previous line's end, so it keeps that line's indentation.
            (
                "    b: 1\n  - c",
                13..13,
                LineOperation::InsertAbove,
                "    b: 1\n    \n  - c",
                13..13,
            ),
            ("", 0..0, LineOperation::InsertBelow, "\n", 1..1),
            ("", 0..0, LineOperation::InsertAbove, "\n", 0..0),
        ] {
            assert_eq!(
                edited_lines(original, selection, operation),
                (expected.into(), selected),
                "{operation:?}: {original:?}"
            );
        }
    }

    #[test]
    fn source_smart_home_steps_over_indentation_unless_already_there() {
        for (original, cursor, expected) in [
            ("  a: 1", 5, Some(2)),
            ("  a: 1", 0, Some(2)),
            ("  a: 1", 1, Some(2)),
            ("  a: 1", 2, None),
            ("x\n\t\tb", 5, Some(2)),
            ("a: 1", 3, None),
            // Blank lines have no first non-blank character, so Home goes to their start.
            ("    ", 1, None),
            ("    ", 4, None),
            ("  \nx", 1, None),
            ("  \r\nx", 2, None),
            ("", 0, None),
        ] {
            assert_eq!(
                smart_home_steps(original, cursor),
                expected,
                "{original:?} at {cursor}"
            );
        }
    }

    #[test]
    fn compact_completion_documentation_keeps_the_example_and_full_explanation() {
        let provider = SourceCompletions {
            workspace: None,
            catalog: Arc::new(CompletionCatalog::new().unwrap()),
            kind: DocumentKind::Suite,
            format: Format::Yaml,
        };
        let source = "scenarios: []\nbasicAuth:\n  ori";
        let items = provider.items(source, source.len());
        let origin = items.iter().find(|item| item.label == "origin").unwrap();
        let Some(lsp_types::Documentation::String(documentation)) = &origin.documentation else {
            panic!("Expected completion documentation");
        };
        assert_eq!(documentation.lines().count(), 1);
        assert!(documentation.contains("https://example.com"));
        assert!(documentation.contains("Required for every entry"));
        assert!(documentation.contains("Example:"));
    }
}
