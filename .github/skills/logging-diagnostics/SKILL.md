---
name: logging-diagnostics
description: 'Use when changing structured diagnostics, tracing spans, capture/deployment journals, log health, bootstrap records, verbosity, archived log views, public log chunks, or log provenance.'
---

# Durable Diagnostics

## Start Here

[Records and runtime](../../../src/diagnostics), [capture ownership](../../../src/test/test_suites.rs), [archive reader/export](../../../src/desktop/logs.rs), [public chunks](../../../src/snapshot/logs.rs), [command contract](../../../docs/commands.md#diagnostic-verbosity), and [verification evidence](../../../docs/logging-verification.md).

## Procedure And Invariants

1. Keep diagnostic version 1 separate from progress version 1 and snapshot version 2. Lifecycle events remain authoritative for counts/outcomes; do not infer them from formatted log text or emit duplicate lifecycle transitions.
1. Construct records from reviewed field/value enums, static messages, generated operation/execution IDs, and readable source identities via `SafeIdentity::source`. Suite/scenario IDs, viewport names, selectors, source URLs, artifact paths, and capture names are metadata, not simulated end-user input. Preserve quoted selector text and ordinary URL parameters; redact URL credentials and credential-bearing parameters, neutralize controls, and bound each identity to 2048 UTF-8 bytes. Never export simulated free text such as `input.value`, file/cookie/state contents, scripts, child output, headers, response bodies, or arbitrary Debug fields. Capture errors stay categorical; provider detail is deployment-only. Keep legacy hashed identities readable and retain content-integrity hashes.
1. Capture failure summaries for markers, reports, and progress come from `runtime::failure_summary`, which uses only typed fields of the failed records plus the category. Record new failure locations as typed fields rather than adding text. The sanitized error chain is local console/live-log output only, through `log_capture_failure`, and respects `--no-print`.
1. Install subscribers only at process entry points. Propagate operation context into spawned/blocking work explicitly; `test` suite finalization does not yet, so its comparison records are dropped (see the outstanding issues). Box nested stage futures to preserve default Windows worker-stack limits; do not hold entered spans across awaits.
1. One operation owns its journal and bounded nonblocking writer queue. Finalize capture work after teardown and before comparison. Explicitly flush entry-point output before process exit; abort recovery cannot rely on destructors.
1. Writer create/write/flush/queue/deadline failures warn independently and preserve capture results. A complete index requires synced segments, verified hashes, counts and terminal outcome. Never turn unknown missing counts into zero.
1. Use all five archive states consistently: complete, partial, missing, legacy unavailable, loading error. Only verified complete logs can export as complete. Missing referenced logs differ from reports that predate logging.
1. Startup journals are local application data, not capture/public assets. Keep their source on handoff failure and never announce a successful copy before verification. No automatic age pruning of startup, deployment, or capture logs.
1. Command and application operations are local-only. Start desktop diagnostics before parsing/dispatch; queue native warnings without UI-thread file writes. Reuse an existing application owner in worker mode. Save command operation IDs with Activity, preserve capture identities, and keep deployment cancellation recovery deployment-only. Version stdout remains presentation.
1. Terminal severity follows outcome. Keep Rust/frontend typed fields synchronized and reject timestamps outside JavaScript's date range before rendering. A plain tracing macro does not pass the reviewed-record subscriber boundary. The report's strict field enum rejects unknown keys, so a new `Field` also needs `frontend/src/data/logs.ts` and a rebuilt bundle before reports can show those records; image simplification count fields are pending that rebuild (see the outstanding issues).
1. Preserve original execution links through compare, approval, and suite changes. Capture sidecars bind to image digests and also record image simplification settings; rewrite them only through the `capture` link helpers (store, promote, digest refresh) so settings survive. Generated chunk cleanup follows selected report dependencies and must not delete retained or active source journals.
1. Explicit or opted-in [run retention](../run-retention/SKILL.md) may delete unreferenced capture-owned journals. Keep baseline/retained links protected, persist deferred ownership before deleting sidecars, and never treat run cleanup as permission to prune deployment/startup journals.
1. `logLevel`/CLI/environment settings filter display only. Chunk size, disk budget, and `REGRESSIFY_DISABLE_DIAGNOSTIC_ARCHIVE` are operation settings, not part of the scenario/suite/workspace cascade.
1. Keep native reads/exports off the UI thread, pages bounded, and stale completions rejected. Web chunks are lazy, version-verified, bounded and rendered as text. Preserve reference/test and capture deep-link identity through refresh.
1. Native numbered pages use checked `LogPage` arithmetic and aligned 500-record offsets. Clamp out-of-range/filter-shrunk requests while retaining one page, not the full log. Rolling Follow is unnumbered; Follow-off requests the last aligned page. Allow invalidated in-flight reads to start replacements and reject stale generations. Page counts never prove completeness or enable partial exports.
1. Deployment cancellation after worker termination uses the exact saved operation ID, never the latest directory. Local cancellation does not prove remote failure. Publication success remains a separate provider-confirmed lifecycle event.

## Checks

```sh
cargo test --lib diagnostics::
cargo test --lib application::tests
cargo test --lib approval_journal_retains_skipped_and_promoted_captures
cargo test --lib snapshot_cleanup_keeps_blocked_empty_directories_without_failing
cargo test --lib test::test_suites::tests
cargo test --lib test::test_action::tests
cargo test --lib snapshot::logs::tests
cargo test --features desktop --lib desktop::logs::tests
cargo test --features desktop --lib desktop::deployment::
cargo test --features gui-tests --lib run_log_ -- --test-threads=1
cargo test --features gui-tests --lib log_health_warning_ -- --test-threads=1
cargo test --features gui-tests --lib log_pagination_ -- --test-threads=1
```

Run frontend `bun run test:e2e --grep 'capture log'` and the explicit cached-browser CLI regression from the [verification document](../../../docs/logging-verification.md). The [benchmark](../../../docs/logging-benchmark.md) documents the confirmed 10% mean wall-time budget, five-pair workload, and archive-off baseline limitations.
The CLI regression must find readable source selectors in archives and published
chunks while excluding simulated input and URL credentials. Journal-level finish
records may omit execution identity; check names on execution-owned records.

## Maintain

Update [command docs](../../../docs/commands.md), [desktop docs](../../../docs/desktop.md), [frontend guidance](../../../frontend/README.md), and the source-linked shared [logging note](../../../.ai/memory/logging-diagnostics.md). Keep schema, inventory, provider limits and exact-package tests synchronized; do not edit generated assets.
