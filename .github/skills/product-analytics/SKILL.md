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
1. Analytics must not return operational errors or emit UI/progress/log messages. Skip missing runtimes and invalid configuration/events. Keep initialization, requests, and timeout cleanup inside the silent tracing scope; HTTP futures can log when dropped. Delivery has one attempt and a two-second budget.
1. Use immediate SDK delivery rather than leaving events queued when short-lived workers exit. Do not enable SDK panic capture, feature flags, or automatic exception tracking. Preserve singleton reuse across concurrent callers and successive Tokio runtimes used by landing deployments.
1. Emit `regressify_site_deployed` only at the shared provider-completion boundary for successful, validated public URLs. `file_count` and `total_bytes` come from final provider inventories, after optimization, while `original_bytes` records the pre-optimization byte count from the initial preparation inventory (equal to `total_bytes` when no optimization runs), without an extra file scan. Include cached paths and files kept on the site from an earlier deployment (counted by their recorded sizes); bytes exclude archive/transport overhead. Measure `duration_ms` before analytics delivery. Preserve original deployment results.
1. Unit tests must never initialize the production client. Test the same capture implementation using loopback servers and synthetic tokens. Live publication or production analytics checks require explicit authorization.

## Checks

```sh
cargo test --lib analytics::tests
cargo test --features desktop --lib desktop::deployment::
cargo fmt --all -- --check
```

Cover payload names and properties, singleton concurrency/runtime reuse, opt-out, missing runtimes, invalid options, HTTP rejection, connection failure, timeouts, silent cleanup, and successful/failing/private deployment gating. Keep exact inventory assertions in the existing Azure, Netlify, and Cloudflare fixtures. Then run the affected [Rust feature gates](../rust-feature-checks/SKILL.md).

## Maintain

Update [desktop docs](../../../docs/desktop.md), [AGENTS.md](../../../AGENTS.md), and [publishing memory](../../../.ai/memory/desktop-publishing.md) when event contracts or lifecycle behavior change.
