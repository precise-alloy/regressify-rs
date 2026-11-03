---
name: desktop-git-review
description: 'Use when changing desktop Git status/diffs, selected commit/discard, branch/upstream checks, remote configuration, or destructive-action confirmation.'
---

# Desktop Git Review

## Start Here

[Git model](../../../src/desktop/git.rs), [text diffs](../../../src/desktop/git/review.rs), and [Git UI](../../../src/desktop/ui/git.rs).

## Procedure and Invariants

1. Parse NUL-delimited machine output. Preserve rename pairs, conflicts, submodule state, literal filenames, and the repository root distinct from workspace root. External VS Code actions pass that repository root plus the optional selected file to the [shared launcher](../../../src/desktop/vscode.rs), so an already-open repository window is focused without replacing another workspace.
1. Revalidate repository/HEAD/branch and relevant remote/upstream configuration after review. For destructive selection, revalidate both status records and `Snapshot::content_identities` (index modes/OIDs and worktree digests) before restoring or deleting files.
1. Commit only selected paths while preserving unrelated staged work. Keep ignored comparison artifacts excluded unless explicitly included by the user.
1. Discard only selected files; protect unselected rename destinations and children, reject unsafe parents, and handle unborn branches without broad resets/cleans.
1. Pull is fast-forward-only and requires a clean appropriate state. Never force push, auto-stash, resolve conflicts, or contact real remotes without authorization.
1. Preserve dirty-editor guards and confirmation cancellation when context changes. Use no-ext-diff/no-textconv for previews and safe remote URL validation.
1. Keep keyboard selection scoped to the focused Changes or Comparison Changes tree. Select All and folder toggles exclude conflicts/submodules and must not include the other group implicitly. Focus/navigation never executes Git writes; commit-message inputs retain their text shortcuts.
1. The bounded titlebar Git badge retains full accessible status. Its Kit tooltip uses a plain Label and weak Git owner so elapsed-time/status text stays current while open. Keep exact tooltip text, update-while-open and native long-conflict bounds checks; a captured old status string can fail even though the tooltip is visibly present.
1. Both groups use retained Kit TreeState with group/repository-path IDs. `set_items` clears selection: restore by ID, preserve expansion through TreeEvent, and clear missing/hidden cursors. Reset scope on workspace changes; generation-check row callbacks and use weak render owners. The approved public base Tree adapter keeps ListItem/scrollbar styling: styled Tree 0.7.3 captures itself in its stored menu renderer even without a menu. Preserve `git_tree_entities_release_after_window_close`.
1. During operations allow read-only cursor movement, expansion and diff review while independently guarding every checkbox/Select All/request callback. Keep reviewed Request paths/snapshot immutable. Scope Enter to `RegressifyList > Tree`: Kit Confirm toggles folders; capture only leaf activation. Isolate checkbox mouse-down from row expansion so rows cannot move before click completion.
1. Remote selection is repository-local `regressify.remote`; include it in the reviewed configuration fingerprint. Fetch, pull, push, and comparison counts must use that destination. Missing or removed active remotes must not fall back silently. Preserve local-upstream pulls and tracking branch names.
1. The retained remote Select uses searchable names as values and the existing guarded SelectRemote request. Unchanged snapshots must preserve an active search; removed selections remain visible with unavailable feedback. Disabled, stale or rejected choices reconcile without issuing another request.
1. Keep remote form drafts bound to their opening snapshot; revalidate before add/select/update/rename/remove. URL edits preserve additional configured URLs and separate push URLs. Never retain credential-bearing URLs in snapshots or diagnostic logs.
1. Activity reporting shares cancellation with the process wrapper. Bound current/history logs, keep blocking work off the UI thread, show completion/cancellation/failure, and clear workspace-scoped logs on workspace changes.
1. Review committed versus saved text without parsing YAML/JSON or inferring semantic changes. Keep exact source for copying, original line numbers, aligned rows, shared two-axis scrolling, and change navigation. Use the bounded `similar` diff off-thread with native virtual lists; binary/unsupported/oversized input retains the raw Git diff fallback. Preserve stale-result generation checks.

## Checks

```sh
cargo test --features desktop --lib desktop::git::tests
cargo test --features desktop --lib desktop::git::review::tests
cargo test --features gui-tests --lib desktop::ui::git::tests
cargo test --features gui-tests --lib git_tree_
```

Reuse temporary repositories and local bare remotes. Set fixture-local `core.autocrlf=false` for byte-exact Windows assertions. Cover malformed source on either side, comments, line endings, empty files, scrolling/copy/navigation, staged/untracked, renamed/deleted, conflicts, stale review, unsafe paths, and cancelled confirmation. `git_text_diff` covers the native split view in compact/wide layouts and light/dark themes.

`git_tree_` covers path refresh, both-group operation locks, emitted request identity,
obsolete diff successes/failures, and virtualized 256-row groups in both sizes/themes.
Keep the independent body test alongside the now-passing long-status header
assertion; native minimum-size theme switching must not widen the window.

Remote-focused filters are `desktop::git::tests::remote_` and `desktop::ui::git::tests::remote_`. Native `./ci/desktop-smoke.ps1 -GitFlows` exercises remote forms, menus, removal cancellation, and progress/logs in compact and wide light/dark windows; observe the native UI skill's safety prerequisites.

## Maintain

Update [desktop docs](../../../docs/desktop.md) and this skill. Comment narrow pathspec and reviewed-state guarantees; record unresolved gaps in the issue list.
