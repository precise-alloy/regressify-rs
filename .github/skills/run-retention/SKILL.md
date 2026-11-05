---
name: run-retention
description: 'Use when changing completed-run tracking, runs.json, clean commands, automatic run retention, cleanup previews, protected logs, or managed deployment staging cleanup.'
---

# Run Retention

## Start Here

[Registry and leases](../../../src/run_history.rs),
[cleanup](../../../src/run_history/cleanup.rs),
[managed staging](../../../src/run_history/staging.rs),
[capture completion](../../../src/test/test_suites.rs), and
[native dialog](../../../src/desktop/ui/cleanup.rs).

## Procedure and Invariants

1. Register each run before creating capture artifacts. Write bounded, validated,
   versioned suite-local `runs.json` atomically; malformed metadata is not absence.
   Import existing folders as completed legacy runs before the first tracked run.
1. Finalize each suite independently in job-completion order. Website/action and
   visual failures qualify as completed; interruption, infrastructure failure,
   and failed artifact finalization do not. Only complete full-suite selections
   become defaults; explicit IDs may select completed partial runs.
1. Use the shared selection for Compare, Results, Snapshot, and Deploy. Generate
   skipped comparisons on demand, not during background inventory refresh or
   activity recovery. Legacy snapshots retain their existing report selection.
   Results has no Open HTML action. `test`/`ref` rebuild the combined snapshot
   only after whole-workspace full runs; partial runs and desktop workers skip it,
   so an existing `index.html` may predate completed captures.
   Raw reports persist `captureInventoryComplete`; scenario-scoped comparison is
   not full coverage. Default consumers repair partial reports on demand, while
   explicit report selection can retain them. Legacy reports omit the flag.
1. Hold suite write ownership during capture/comparison and shared artifact
   ownership while reading. Cleanup needs exclusive artifact ownership. OS locks
   release after termination; leftover running records are not default candidates.
   These are cooperative locks, not protection against arbitrary external writers.
1. `clean` requires suite filters and previews by default; only `--yes` deletes.
   Desktop Preview precedes confirmation. Revalidate selection, retention, paths,
   sizes, and modification metadata immediately before deletion. Reject links,
   unsafe IDs, active owners, and changed previews. Do not roll back unrelated work.
1. Enforce a strict minimum-one keep count. Reserve the newest full-suite result
   first, then fill slots with newer completed runs. Partial runs may be removed;
   interrupted/failed runs consume no slots. Scope one operation to one output root.
1. `autoCleanRuns` and `keepTestRuns` inherit suite -> workspace -> false/1,
   independently. Validate zero even when disabled. Only full-suite Test completion
   triggers automatic retention; cleanup failure warns without changing completion.
1. Preserve references, browser state, source files, unselected suites, and unrelated
   journals. Validate capture-log ownership and protect all retained/reference
   links. Save `pendingLogs` before deleting sidecars; retry protected/deferred
   journals later. Unreadable ownership must preserve logs and warn.
1. Managed `.staging/<uuid>` containers keep owner metadata outside `files`.
   Keep optimization copies and ZIPs under that payload. Best-effort teardown
   removes payload first; failed removal must retain owner metadata. Sweep abandoned
   containers before the next deployment, skip active locks, and report unknown
   content instead of deleting it. Never publish registries, locks, or staging.
   Hold the catalog lock during create, claim, sweep, and teardown, including
   the final owner-lock close/unlink interval; contention defers owned cleanup.
1. Keep cleanup file work off the UI thread, invalidate previews when inputs change,
   block dismissal while busy, preserve drafts, and refresh Results/inventory after
   deletion. The same dialog is reachable from Overview, Results, and root Fields.

## Checks

```sh
cargo test --lib run_history
cargo test --lib suite_run_completion_
cargo test --features desktop --lib completed_run_
cargo test --features desktop --lib run_retention_
cargo test --features desktop --lib staging_includes_only_selected_report_dependencies
cargo test --features gui-tests --lib cleanup_dialog_
./ci/test-desktop-smoke.ps1
./ci/desktop-smoke.ps1 -CleanupFlows
```

Native smoke requires an unlocked, foreground-capable Windows desktop and no
other app instances; ask the user to leave input untouched. Windows lock-retry
tests cover best-effort staging cleanup. Unix-only symlink tests need Linux CI.
Use synthetic files only; no production captures, credentials, or publication.

## Maintain

Update [command docs](../../../docs/commands.md#run-history-and-cleanup),
[desktop docs](../../../docs/desktop.md#cleaning-test-runs), suite configuration,
MCP workspace guidance, affected skills, and the
[shared note](../../../.ai/memory/run-retention.md).
