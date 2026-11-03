---
name: desktop-progress-events
description: 'Use when changing structured run events, scenario counts, UI polling budgets, saved run history, omitted logs, or recovery from comparison reports.'
---

# Progress and Saved Activity

## Start Here

[Progress model](../../../src/progress.rs), [runner persistence/polling](../../../src/desktop/runner.rs), and [run UI](../../../src/desktop/ui/runs.rs).

## Procedure and Invariants

1. Preserve stable job/suite/scenario/capture identities. Multiple screenshots from one scenario must not inflate scenario counts or lose viewport context.
1. Bound event/log processing per UI tick; do not block on a worker-held progress mutex. Notify views only when their observable revision changes.
1. Keep the 100 ms queue/completion poll separate from 500 ms coalesced progress notifications, redraws and live-log refreshes. Retain pending changes across unchanged ticks; worker exit/persistence completion bypass throttling, as do explicit user actions. Idle activity needs only elapsed-second updates. Do not rebuild hidden capture log editors; preserve on-demand Log loading, deployment logs and completion archive health. Cover exact interval boundaries with `run_poll_` and hidden logs with `run_log_hidden_` GUI tests.
1. Distinguish execution failure, cancellation, visual mismatch, capture error, and successful completion. Keep structural errors out of website-result counts.
1. Persist only finished, supported activity for the matching workspace. Surface persistence errors and retain ownership until the write completes.
1. Saved activity version 2 preserves optional report overrides. Loading version 1 converts only exact prefilled title/heading/subheading defaults to unset values; retain custom strings, explicit blanks, and version-2 overrides equal to defaults. Do not infer provenance from text in current-format activity.
1. Recover reports by suite/output/run identity; do not infer successful execution logs or silently approve missing/ambiguous scenario metadata. Preserve history across later runs and different output directories.
1. Completed logs use the [paged archive model](../../../src/desktop/logs.rs) and capture-owned links, not saved Activity text. Keep filesystem reads off the UI thread, reject stale load generations, and distinguish all five archive states. Active retained-history omission notices remain separate from archive completeness.
1. Follow-off loads the last aligned numbered page, not the rolling tail's unaligned offset. Clear the archive key when filters/Follow change so a replacement read can start during loading; accept only matching key/generation results. Use the existing test-only polling fixture for live UI cases without launching a worker.
1. Persist `diagnosticOperationId` for non-version commands, not just deployments. Non-capture View Log loads that exact command archive; capture activity may include it alongside original execution archives. Never select a latest journal or treat an absent expected operation as legacy.
1. Successful suite renames remap selections, executed targets, progress IDs, and saved count keys; deletes remove matching progress/counts. Keep historical deletion filters rather than turning an empty list into an all-suite request, and disable executed-report actions when no targets remain. Log text stays historical; visible labels use the current catalog.
1. Activity writes started outside a worker run need their own completion polling. Keep the remapped catalog current while saving, release operation locks after persistence, and refresh artifact availability; the worker's polling task has already ended.
1. Results badges prefer the loaded report's scenario/viewport counts while idle. Approval can recalculate a report after Activity was saved; do not let that older snapshot override the report being reviewed. Retain live counts during runs.
1. Suite activity uses controlled Collapsible with stable output IDs. Failed scenarios force their suite open while the user's collapsed set remains unchanged; header activation records a close preference without hiding failures. Restore it when failures clear, keeping failed-only filtering, exact counts and log targets intact.
1. Completed report deployments persist the validated URL and published suite membership to their target ID in the background completion writer, before releasing run ownership. `Deployed.url` and saved results allow `null` for successful private Cloudflare publication, clearing stale URLs and memberships without offering Open Site. Existing string-valued activity remains readable. Keep activity if the target write fails; report the error. Failed, cancelled, restored, or remapped history must not overwrite publication metadata.

## Checks

```sh
cargo test --lib progress::tests
cargo test --features desktop --lib desktop::runner::tests
cargo test --features gui-tests --lib run_poll_
cargo test --features gui-tests --lib run_log_
```

Cover partial/invalid events, unchanged polling, contended locks, overflow/omission, late writes, reload without rerun, absent suites, and legacy metadata.

## Maintain

See [logging docs](../../../docs/commands.md#diagnostic-verbosity) and the [diagnostics skill](../logging-diagnostics/SKILL.md) for archive/display boundaries.

Update [desktop docs](../../../docs/desktop.md) and this skill when status or persistence semantics change. Comment identity and nonblocking polling invariants.
