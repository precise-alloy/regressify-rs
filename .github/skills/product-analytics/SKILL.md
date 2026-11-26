---
name: product-analytics
description: 'Use when changing global PostHog tracking, analytics events or properties, event namespacing, telemetry opt-out, silent failures, or deployment analytics.'
---

# Product Analytics

## Start Here

[Shared analytics](../../../src/analytics.rs), [deployment orchestration](../../../src/desktop/deployment.rs), and [deployment docs](../../../docs/desktop.md#deploy-reports).

## Procedure And Invariants

1. Use `analytics::capture(object, verb, properties).await` from either CLI or desktop code. The shared client constructs `regressify_[object]_[verb]`; object and verb contain only nonempty ASCII letters, digits, and underscores. Properties must be a JSON object. Do not introduce unnamespaced SDK calls.
1. Keep PostHog initialization lazy and process-wide. Use the configured US Cloud project ingestion token, never deployment credentials or personal API keys. Do not store tokens in guidance or shared memory. Other apps sharing the project must remain distinguishable by the event namespace.
1. Capture anonymous events, disable person profiles and GeoIP enrichment, and respect `DO_NOT_TRACK=1`. Review properties at the caller: no credentials, browser state, local paths, target names, or report contents.
1. Await only the durable local enqueue. The [outbox](../../../src/analytics/outbox.rs) is per user, outside workspaces: cap at 100 records, evict oldest first, and expire at 30 days. Keep the event UUID, anonymous identity, properties and occurrence time stable across replays. Do not add delivery branches for new event types.
1. CLI/Desktop startup resumes delivery without collecting an event. An opted-out launch must purge committed and staged payloads without waiting for HTTP. Do not start detached processes, block the UI on requests, or couple delivery to the producer's Tokio runtime.
1. Only one sender at a time may issue an immediate SDK request across processes. Never hold the queue mutation lock across HTTP. Acknowledge by UUID against current disk state; never resurrect purged or evicted records. Retry network/408/429/5xx failures with backoff, three attempts per round, 30 seconds per request. Persist retry eligibility; a delayed event must not block others.
1. Keep SDK setup, requests, timeout and cleanup silent. Do not return tracking errors or emit UI/progress/log messages. Do not enable panic capture, feature flags, or automatic exception tracking.
1. Emit `regressify_site_deployed` only at the shared provider-completion boundary for successful, validated public URLs. `file_count` and `total_bytes` come from final provider inventories, after optimization, while `original_bytes` records the pre-optimization byte count from the initial preparation inventory (equal to `total_bytes` when no optimization runs), without an extra file scan. Include cached paths and files kept on the site from an earlier deployment (counted by their recorded sizes); bytes exclude archive/transport overhead. Measure `duration_ms` before analytics delivery. Preserve original deployment results.
1. Unit tests must never initialize the production client. Test delivery using loopback servers and synthetic tokens. Set `REGRESSIFY_TEST_NO_ANALYTICS=1` on fixture CLI/Desktop processes so they cannot send **or purge** the real queue; `DO_NOT_TRACK=1` alone would delete it. Live publication or production analytics checks require explicit authorization.

## Checks

```sh
cargo test --lib analytics:: -- --include-ignored
cargo test --features desktop --lib desktop::deployment::
cargo fmt --all -- --check
```

The analytics filter includes one 33-second loopback test that measures the actual retry schedule and production request deadline. Cover interrupted sender replay, competing writers/senders, retention, opt-out, missing runtimes, permanent and transient failures, and successful/failing/private deployment gating. Keep exact inventory assertions in the existing provider fixtures. Then run the affected [Rust feature gates](../rust-feature-checks/SKILL.md).

## Maintain

Update [desktop docs](../../../docs/desktop.md), [AGENTS.md](../../../AGENTS.md), and [publishing memory](../../../.ai/memory/desktop-publishing.md) when event contracts or lifecycle behavior change.
