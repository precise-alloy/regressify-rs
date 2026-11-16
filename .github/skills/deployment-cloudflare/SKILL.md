---
name: deployment-cloudflare
description: 'Use when changing Cloudflare Workers report deployment, static asset manifests, upload-session JWTs, multipart batches, workers.dev routing, or Cloudflare API diagnostics.'
---

# Cloudflare Workers Deployment

## Start Here

[Cloudflare client](../../../src/desktop/deployment/cloudflare.rs), [target model and staging](../../../src/desktop/deployment.rs), and [native deployment dialog](../../../src/desktop/ui/deployment.rs).

## Procedure and Invariants

1. Use Workers Static Assets, not R2, Pages, or a Wrangler subprocess. Upsert the dedicated Worker through publication; do not require pre-creation. Saved targets default to production publication with workers.dev enabled, including legacy targets without routing settings. Validate account and Worker identifiers before API paths. Do not change DNS, custom domains, or Access policies.
1. `workersDev: false` disables public and preview routes. Explicit `null` preserves existing routes and creates missing Workers privately. Only a structured HTTP 404/code 10007 proves absence; permissions or malformed responses must fail closed. Preserve mode re-reads routing after publication without writing stale state. Explicit modes configure and confirm routing only after publication.
1. Keep the API token in the shared credential store. Use account-scoped Workers Scripts Write permission; R2 access keys are not deployment credentials. Never persist or log API tokens, upload JWTs, completion JWTs, or credentialed URLs.
1. Stage only the selected report dependencies. Reject Azure access restrictions through the shared guard before API calls. Use the existing artifact checks, reject symlinks/non-regular files, and revalidate bytes before uploading.
1. `content_budget()` is `None` outside tests, so `Manifest::prepare` validates and lists assets without optimizing them (evidence in [shared memory](../../../.ai/memory/deployment-cloudflare.md)). Keep the budgeted path for a future limit and exercise it through the test-only `content_budget` field: `Manifest::prepare` passes the validated manifest's asset paths to the [shared optimizer](../../../src/desktop/deployment/optimization.rs) with the budget minus kept assets, deferring only the total-byte limit, at the quality floor `Keeping::reduce_quality` selects, and refuses a shortfall beside kept assets as `KeptSuitesOverBudget { remaining, staged_png }` for the report deployment's retries ([deployment targets](../deployment-targets/SKILL.md)). Excluded/private files and hosting policies must not consume that budget. Rebuild the manifest after optimization and retain the temporary copy through upload and verification. Read policies from the original canonical staging root, not the asset-only optimization copy.
1. Hash Base64 content, extension, a NUL byte, and MIME with SHA-256, taking 32 hex characters. MIME changes must invalidate cached headers. Validate every bucket hash against the manifest; remote hashes must never choose local paths.
1. Enforce 25 MiB per asset, 100,000 paths, 32 MiB manifests, and 8 MiB responses; do not cap total content (`sites_above_two_gibibytes_deploy_without_a_content_budget`). Cloudflare enforces the account allowance (20,000 Free or 100,000 Paid). Limit raw batches and shared in-flight raw content to 50 MiB; retain permits during blocking reads, retries, and request copies.
1. Honor the upload JWT's transport hint: raw bytes to per-hash endpoints or MIME typed Base64 multipart parts. Hints are not authorization. Cap single-asset concurrency at eight (respect smaller hints), multipart at three; size-based upload timeouts cap at 30 minutes. Use `application/null` for unknown MIME. An empty bucket list already contains a completion token. Otherwise require a completion token after all content is accepted; later empty results cannot erase it.
1. Publish an assets-only Worker using metadata and the completion token. Exclude `_headers`, `_redirects`, Azure configuration, and `.gitignore` from public assets. Supply policies in `assets.config`, translate index-page proxies to `/`, unless saved HTML handling is `none`. Default not-found handling is `none`, so missing JSON/images return 404. Persist validated HTML/not-found modes, date, and exclusion globs with the target. Prune private files and directories, reject `.assetsignore` and included Pages `_worker.js`, and require the report entry.
1. Bound the operation to 55 minutes, below one-hour temporary-token expiry. Retry safe GETs and identical upload bytes up to six times with bounded backoff. Session creation and explicit route updates are repeatable; publication is not. Distinguish confirmed rejection from uncertain publication outcomes. Cancellation is local and cannot undo an already accepted deployment.
1. Verify returned Worker identity and public HTTPS HTML/asset bytes and MIME types before emitting success. Never send authorization to the public URL. Private deployments have no URL. Preserve-mode informational read failures after confirmed publication warn without requesting redeployment. Do not bypass Access.
1. Redact every known request/body token before truncating diagnostics; retain bounded provider messages, numeric codes, and safe cf-ray IDs. Do not attach raw reqwest errors or response bodies to error chains.
1. Local preparation and asset I/O failures must identify the operation and path while retaining the OS cause. Preserve the initial Azure access-policy rejection text. Windows `os error 5` is not a provider permission code; require the complete error before attributing it to settings, staging, or endpoint protection. Use temporary-file/loopback tests, not permission changes to user files.

## Checks

```sh
cargo test --features desktop --lib desktop::deployment::cloudflare::tests
cargo test --features desktop --lib desktop::deployment::optimization::tests
cargo test --features desktop --lib desktop::deployment::tests
cargo test --features gui-tests --lib desktop::ui::deployment::tests
```

Use synthetic tokens and loopback servers. Cover manifest limits, mutation, symlinks, bucket validation, cached deployments, MIME/policy metadata, retry exhaustion, deadlines, redaction, rejected redirects, failed publication, routing defaults/preservation, raw uploads, and completion-token ordering. Cover sites above 2 GiB uploading unchanged, budgeted oversized reports, unchanged fitting reports, source preservation, temporary cleanup, excluded files, and cascading snapshot cache versions after compression.

The `local_preparation_errors_` filter covers unavailable staging directories and Windows file locks, with no API requests and preserved sources. Deployment target settings tests separately cover Windows access-denied errors and path diagnostics.

`live_create_update_and_delete_temporary_worker` requires explicit authorization, `REGRESSIFY_ALLOW_CLOUDFLARE_TEMP=1`, `CLOUDFLARE_API_TOKEN`, and `CLOUDFLARE_ACCOUNT_ID`. It checks initial absence, creation, changed/cached updates, production routing, public bytes/MIME, and deletion. Cleanup also runs after a verification error. It never uses the configured Worker name; keep it ignored.

`live_publish_cloudflare_reports` is ignored and replaces the selected Worker's content twice. Run only with explicit authorization and private process variables: `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_WORKER_NAME`, `CLOUDFLARE_EXPECTED_URL`, `CLOUDFLARE_REPORT_ROOT`, and comma-separated `CLOUDFLARE_SUITE_IDS`. It regenerates snapshots from existing comparisons and checks every public asset, deep links, headers, and missing-file 404s. Load local environment files privately, for example with Bun; never print their contents.

## Maintain

Update [desktop docs](../../../docs/desktop.md), [hosting docs](../../../frontend/README.md), and [shared notes](../../../.ai/memory/deployment-cloudflare.md) with verified protocol or setup changes. Live publishing tests must remain explicitly ignored.
