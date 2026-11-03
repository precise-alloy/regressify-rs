---
name: desktop-workspace-files
description: 'Use when changing desktop file discovery, Save/Save As, suite creation/duplication/rename/delete, workspace paths, preferences, or external-conflict handling.'
---

# Desktop Workspace Files

## Start Here

[Workspace operations](../../../src/desktop/workspace.rs), [session transitions](../../../src/desktop/session.rs), [runtime initialization](../../../src/init.rs), and [atomic I/O](../../../src/helpers/io.rs).

## Procedure and Invariants

1. `Session::connect` creates missing runtime defaults and maintains MCP configuration and the managed `AGENTS.md` section before listing files. Keep `Workspace::open` validation-only because ownership probes use it too. Run setup off the UI thread after ownership and unsaved-work guards, without replacing the current session on failure. Follow the [init skill](../init-command/SKILL.md) for the shared runtime/agent inventory and preservation rules.
1. Resolve workspace-relative components with portable-name and canonical containment checks. Validate existing ancestors, not just the final filename. Classify resolved destinations, accounting for Windows folder casing; aliases into `visual_tests` must receive suite-name/content validation even from Data templates. Preserve dot-prefixed non-suite helpers and no-clobber writes.
1. Validate both structured values and original typed source of the current draft before save. The document JSON parser rejects duplicate keys before constructing `Value`; retain typed-source validation as the final serde contract check. Validate scenario dependencies independently of action viewport filters. Do not require the previously saved source to parse: syntax-invalid files must remain repairable. Skip optional report-label migration when old metadata cannot be parsed, retaining exact saved bytes for conflict detection.
1. Compare on-disk bytes with saved_source before replacing, renaming, or deleting. Preserve the draft on conflict. Atomic replacement alone does not detect edits.
1. The open-file check in [UI file operations](../../../src/desktop/ui/files.rs) polls off-thread once per second. Apply results only to the same workspace, path, and saved bytes, outside busy operations. Reload clean documents; preserve unsaved and pending native drafts with a guarded Reload action. All destructive/close guards use `unsaved(cx)`, including native Source text not yet acknowledged by a change event and oversized drafts. Keep content on deletion/read failure and retry automatically; unchanged bytes must not reset history or selection.
1. Create/Save As must not clobber an existing file, even between existence check and write. Preserve the existing no-clobber temporary-file mechanism.
1. Duplication removes explicit suite IDs to avoid shared output. Preserve format on rename and do not erase the source if creating the destination fails.
1. Suite rename/delete use [staged artifact updates](../../../src/desktop/workspace/artifacts.rs) across default/environment and caller-supplied custom roots. Preserve explicit IDs, reject shared IDs/collisions, update historical raw reports and published snapshots, and roll back on document failure. This is not crash-atomic or a lock against external writers. Never touch browser states or remote deployments.
1. `displayName` is presentation-only. Fields saves and Rename update local report labels without changing file/output identity. Pass active and saved-history roots from the UI; remap selections/activity after successful mutations. F2/Delete are scoped to the suite list and retain draft/confirmation guards.
1. Application File menus dispatch AppFileCommand to the open document and revalidate enabled state. Do not reuse focused suite-list FileCommand routing for that surface; context menus/F2/Delete intentionally retain focused-row identity. Cover a different focused suite while the original document stays open.
1. Keep discovery precedence aligned with the CLI; runnable_suite must reject a selected file shadowed by another format. Merge preferences without losing another window's recent workspaces or settings.
1. List every regular file under `common`, including nested and binary files. Only `visual_tests` entries become suites (`Workspace::classify_entry`). Compute `Entry::supports_editing` from bounded UTF-8 text reads, not an extension whitelist. Keep binary, unreadable, and oversized rows visible but disabled; guard mouse/keyboard activation and recheck content and containment at `Workspace::open_document`. Never treat JavaScript or CSS as YAML, and refresh both `entries` and `suite_catalog` on `Refresh files`.
1. Non-YAML/JSON text uses `Format::Text` and Source only. Preserve exact source, undo/redo, conflict detection, and no-clobber Save As; reject binary controls and conversion between text and structured formats. Keep Source selected after Save As and breadcrumb navigation.
1. New/Rename dialogs select the filename stem in the focused input. Enter from inputs or Create/Save shares the existing asynchronous commit and no-clobber guards; only successful writes close the dialog. Retain editable input on failure. X, Cancel, and Escape cancel without saving and cannot dismiss a busy write. Preserve keyboard cancellation when wiring a dialog-wide default action.
1. Workspace configuration Fields may use toolkit Form/Field for presentation. Source fidelity, external-conflict checks, save validation, path containment, and no-clobber behavior remain owned by the existing document/session/workspace code.

## Checks

```sh
cargo test --features desktop --lib desktop::workspace::tests
cargo test --features desktop --lib desktop::workspace::artifacts::tests
cargo test --features desktop --lib desktop::session::tests
cargo test --features desktop --lib init::tests
cargo test --features gui-tests --lib workspace_files_
cargo test --features gui-tests --lib external_reload
cargo test --features gui-tests --lib corrected_invalid_source
cargo test --features gui-tests --lib suite_name_dialog_
```

Cover repairs of existing syntax/schema errors, suite status refresh after saving, external edits/deletion, existing destinations, path traversal/symlinks, reserved names, duplicate/shadowed suites, read-only paths, and concurrent settings.

GUI fixtures use `Session::connect`, so initialized default files already exist. Validate only editable documents, and assert `_on-ready.js` and `_override.css` are listed and editable. `common_text_files_` covers raw saves, content/size boundaries, conflicts, and Save As; `workspace_files_text_documents_support_source_editing_and_save` covers native editing and binary activation guards. Suite-creation assertions preserve the runtime inventory; sidebar-scroll fixtures count all non-suite entries and cover the eight/nine-row boundary in `desktop::ui::tests::workspace_files_scroll_within_sidebar_at_both_window_sizes`.

Workspace UI uses retained Kit List state with stable paths and guarded deferred
activation. The sidebar-scroll test also covers 64 rows: offscreen rows may be
unmounted, but the last row must be visible after wheel/drag/End. `flat_collection_workspace_`
verifies refresh/reorder identity, wrapping, binary/busy gates and input scope.

## Maintain

Update [desktop docs](../../../docs/desktop.md) and this skill. Comment why conflict and no-clobber guards exist; retain failure-path tests whenever I/O is refactored.
