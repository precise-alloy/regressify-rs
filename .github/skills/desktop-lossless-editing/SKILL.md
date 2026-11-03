---
name: desktop-lossless-editing
description: 'Use when changing desktop YAML/JSON or plain-text document edits, Source/Fields transitions, undo/redo, invalid drafts, batch edits, or asynchronous save acknowledgement.'
---

# Lossless Desktop Editing

## Start Here

[Document model](../../../src/desktop/document.rs), [session state](../../../src/desktop/session.rs), and [editor orchestration](../../../src/desktop/ui/mod.rs).

## Procedure and Invariants

1. Preserve editable invalid source. Syntax parsing, schema diagnostics, and the save gate are distinct; schema-invalid but parseable source can still use Fields. Duplicate JSON keys are rejected by the raw parser before `Value` can collapse them; Fields must not rewrite such drafts, including nested/escaped-key duplicates.
1. Treat blank/comment-only workspace YAML as defaults through kind-aware document parsing, without rewriting its source. First-field edits and resetting the last field must round-trip and undo exactly. Explicit nulls and blank non-workspace or JSON documents remain invalid.
1. For YAML structural edits preserve comments, quoting, anchors, and untouched formatting. Compare the resulting parsed value with the expected semantic edit; reject a lossy library rewrite without changing the user's draft.
1. Removing the last nested mapping entry with yaml-edit 0.3.1 can leave an orphaned `INDENT` token before the next parent-level key. Preserve the targeted cleanup in `apply_yaml` and the starter-suite reset regression; never bypass the semantic guard to accept it.
1. Document owns accepted-edit history across Source, Fields, toolbar, and palette. Capture Source Undo/Redo before the native input handler; keep local undo inside ordinary Fields inputs for unapplied invalid drafts. Group adjacent single-character typing/deletion, but split on selection changes, paste, line/structural commands, and saves. Preserve active Source selection and bound both history stacks by count and tracked bytes; no-op edits retain redo.
1. Source line commands use one native text replacement and explicitly delimit the document history group. Cover UTF-8 selections, CRLF, native reverse selections, exclusive column-zero ends, empty/final lines, and JSON comment rejection. Treat only ASCII spaces/tabs as indentation in both comment classification and removal; Unicode whitespace and form feed must round-trip. Kit 0.7.3 collapses reversed ranges passed to `set_selected_range`; the approved adapter preserves selected text while making the resulting selection forward-oriented. Native change events are deferred; settle them before checking session text.
1. An asynchronous save acknowledges the exact bytes written, not the latest source that may have changed while I/O ran. Newer edits must remain dirty.
1. External reload checks actual native Source and active Fields inputs before replacing a clean document; queued change events must not lose text. Ignore hidden Fields controls after Source edits/saves. Same-byte observations leave both histories intact; stale reads cannot replace a newer saved version.
1. Guard document/workspace switches and destructive actions with existing session transitions; `unsaved(cx)` must inspect pending native Source text, not only model dirty state. Preserve oversized Source drafts through `set_source_draft`, but retain the 4 MiB open/save gate and block Fields until shortened. Reconcile selection from current inspection after Source edits, and block stale scenario/action commands while debounced feedback is pending. Source edits/history remain immediate; delayed feedback must never replace bytes or trust an older save.
1. `Format::Text` is raw UTF-8 source, not a YAML scalar or a structured data mapping. Preserve bytes and history; accept empty text, reject binary controls, and do not expose Fields, Effective, schema diagnostics, or YAML/JSON conversion through Save As.
1. Toolkit Form/Field in the Fields view is presentation only. Keep `Binding`, raw `InputState` drafts, `field_errors`, inherited hints, reset handlers, and semantic document/session edits in the existing workbench owner. Do not let displayed descriptions or required markers coerce or discard draft text.
1. Retained Choice state must resynchronize from inheritance on Reset without rebuilding away unrelated invalid drafts. Multi-choice fields retain scalar/list source until an explicit toggle, preserve unknown entries and use one document history entry per accepted Change; dismissal is not a second write.

## Checks

```sh
cargo test --features desktop --lib desktop::document::tests
cargo test --features desktop --lib desktop::session::tests
cargo test --features gui-tests --lib source_line_commands
cargo test --features gui-tests --lib source_history_
cargo test --features gui-tests --lib external_reload
```

Cover invalid drafts, Unicode/comments/line endings, plain selectors, anchors, empty collections, list bounds, undo/redo, all shipped suites, and late save results. Use native UI tests when changing tab/navigation behavior.

## Maintain

Update [desktop docs](../../../docs/desktop.md) and this skill. Explain semantic round-trip guards and YAML library workarounds with concise comments. Keep a known unsupported edit separate from a failure to protect the original draft.
