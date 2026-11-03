---
name: deployment-targets
description: 'Use when changing desktop deployment targets, saved target configuration, OS keyring credentials, target identity, token replacement, or provider switching.'
---

# Deployment Targets and Credentials

## Start Here

[Target model](../../../src/desktop/deployment.rs), [credential store](../../../src/desktop/deployment/credentials.rs), [named tokens](../../../src/desktop/deployment/tokens.rs), and [desktop deployment UI](../../../src/desktop/ui).

## Procedure and Invariants

1. Keep public target settings separate from secrets. Direct-entry credentials remain scoped to canonical workspace/landing identity and target ID. Explicit named tokens use a separate per-user `named:<uuid>` keyring namespace; never implicitly share a direct-entry credential.
1. Use the OS credential abstraction and zeroizing secret buffers. Never serialize tokens into target files, run snapshots, progress events, logs, or diagnostic text.
1. Preserve existing credentials when an edit does not replace them. Make target save/provider-switch/delete failure paths explicit; do not leave a saved target referencing a removed secret or silently lose a previously usable credential.
1. Validate target/provider/suite selection again at execution. Missing credentials or unavailable secure storage must produce actionable secret-free errors.
1. Settings read, directory-creation, and save failures must identify the local operation/path and retain the underlying OS error. Preserve existing bytes on failure; never clear read-only attributes or change permissions automatically. Cover Windows error 5 and blocked settings directories in the existing model tests.
1. Cloudflare uses `provider: cloudflareWorkers` with a `cloudflare` object containing `accountId`, `workerName`, and saved routing/asset options; never repurpose Azure `environment` or Netlify `siteId`. Omitted `workersDev` defaults to public production (`true`); explicit `null` preserves routing. Keep this object absent for other providers and retain advanced options on ordinary edits.
1. Keep production Windows credential persistence semantics separate from the mock backend used in tests. Do not validate against the user's actual keyring.
1. Named tokens support Netlify and Cloudflare, with immutable provider/account identity and editable name/secret. Serialize only metadata and `namedTokenId` references. Revalidate provider/account when linking and executing; deleting a target must never remove a named token.
1. Catalog writes lock and compare the loaded catalog, rejecting stale windows. Preserve secrets on failed metadata saves and clear masked drafts on cancel/close. Named-token removal must warn that referencing targets will require another token.
1. Landing targets use app-managed identity, not linked-site workspaces. See the [landing skill](../desktop-landing-apps/SKILL.md).
1. Store the optional `publishedUrl` with its target ID after successful publication. Reload current settings before updating, preserve other targets/selection, and reject removed or changed destinations. Accept only credential-free HTTPS URLs without query/fragment data. Failure/cancellation retains the previous URL.
1. Name-only edits, Cloudflare asset options, and Netlify/Cloudflare token changes preserve the URL. Destination/routing-mode changes clear it; replacing an Azure token also clears it because the token identifies the app. A confirmed private Cloudflare publication clears stale URLs; failure/cancellation does not. Surface persistence errors without hiding a successful URL. Report completion writes in the runner's background task; landing completion writes before returning to the UI.
1. `reportPublications` stores target IDs and suite output IDs in completion order, atomically with report URLs. Replace membership when redeploying that target/site, clear it with invalidated URLs or removed targets, and retain it on failure/cancellation. Resolve the latest publication containing every requested suite; single suites use URL path-segment encoding under `/suites/`. Legacy URLs without membership must not imply that a suite was published. Load Runs links off the UI thread and refresh after completion or target changes.
1. Deployment and named-token edit forms use toolkit Form/Field for labels only. Keep stable input IDs and accessible names, provider-dependent controls, masked password content types, saved-secret placeholders, and the existing save/cancel locks and draft clearing behavior.
1. Saved target/token Select items use stable IDs separate from names; provider items use machine keys. Reconcile rejected confirmations to the authoritative selection. Confirming an unchanged token source or provider preserves the secret draft; actual identity changes clear it. Keep missing selected identities visible and non-deployable.

## Checks

```sh
cargo test --features desktop --lib desktop::deployment::credentials::tests
cargo test --features desktop --lib desktop::deployment::tests
cargo test --features desktop --lib desktop::deployment::tokens::tests
cargo test --features desktop --lib completed_deployments_persist_urls
cargo test --features desktop --lib published_report_sites_
cargo test --features gui-tests --lib saved_deployment_urls_reopen_and_copy
cargo test --features gui-tests --lib runs_open_site_
cargo test --features gui-tests --lib desktop::ui::tokens::tests
cargo test --features gui-tests --lib desktop::ui::deployment::tests
```

Cover workspace isolation, create/edit/delete, blank/missing tokens, provider changes, failed saves, corrupt target files, and errors that omit synthetic secrets. Run `./ci/desktop-smoke.ps1 -PublishingFlows` for named-token and landing controls; run the smoke harness self-tests first and preserve the interactive-desktop guards.

## Maintain

Update [desktop docs](../../../docs/desktop.md) and this skill when storage or target workflow changes. Comment credential ownership and rollback decisions.
