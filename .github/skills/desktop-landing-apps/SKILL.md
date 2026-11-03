---
name: desktop-landing-apps
description: 'Use when changing desktop landing apps, automatic landing storage, Markdown pre-rendering, linked-site status, landing previews, or landing deployment.'
---

# Desktop Landing Apps

## Start Here

[Model and renderer](../../../src/desktop/landing.rs), [native editor](../../../src/desktop/ui/landing.rs), [deployment dialog](../../../src/desktop/ui/deployment.rs), and [browser status controls](../../../frontend/src/landing.tsx).

## Procedure and Invariants

1. Store definitions automatically under the per-user desktop settings directory using validated UUID filenames. Never ask for a save location or tie a landing app to a workspace. Keep external-conflict and no-clobber guards.
1. Linked sites are text and uncredentialed HTTP(S) URLs, including sites owned by other users. The desktop must not fetch them or search local workspaces for them.
1. Pre-render headings and safe `pulldown-cmark` subheadings before upload. Exclude raw HTML, images, unsafe links, and credentialed links. Reuse report assets, logo/favicon, and themes; only status/theme controls need browser JavaScript.
1. Browser status reads use `/_snapshot/status.json` on each linked origin with omitted credentials/referrer, timeout, size bound, validation, and independent failure/retry states. Old/private sites retain usable links.
1. Preview binds only loopback and serves an exact public-asset allowlist with Host validation. It must never expose definitions, workspace files, or credentials.
1. Deploy saved definitions through existing provider clients and target controls. Keep work off the UI thread, check file identity again, and surface cancellation honestly: accepted remote publication cannot be undone locally. Persist each successful URL under the landing app's managed identity, and expose open/copy controls after reopening. Successful private Cloudflare publication clears stale public URLs; failure/cancellation preserves them. Report local save failures explicitly.
1. Guard draft switches and native window close. Local removal confirms its scope and never deletes hosted sites. Removing targets preserves named credentials.
1. Landing editor fields use toolkit Form/Field, with normal unframed GroupBox for each site. Preserve the retained outer scroll handle and IDs, stable per-site input/action IDs, explicit accessible names, and the existing draft/save guards; offscreen controls may require scrolling before native lookup. Attach the scrollbar to the stationary relative viewport, not its scrolling body; verify thumb dragging after wheel scrolling to the last site.

## Checks

```sh
cargo test --features desktop --lib desktop::landing
cargo test --features gui-tests --lib desktop::ui::landing::tests
cargo test --features gui-tests --lib saved_deployment_urls_reopen_and_copy
./ci/test-desktop-smoke.ps1
./ci/desktop-smoke.ps1 -PublishingFlows
```

Use [production-fixture browser checks](../../../frontend/README.md#landing-pages) for desktop/mobile, no-JavaScript rendering, Markdown safety, remote status, retry, theme, and accessibility. Never count their default skipped run as coverage.

`live_publish_landing_page` is explicitly ignored and replaces hosted content. It requires authorization plus `REGRESSIFY_ALLOW_LANDING_PUBLISH=1` and a selected `REGRESSIFY_LANDING_PROVIDER` (`azure`, `netlify`, or `cloudflare`). Load provider credentials privately into the process; never print them or use the user's keyring. `REGRESSIFY_LANDING_STATUS_FIXTURE=1` publishes a minimal report and verifies its real CORS/cache status contract. `REGRESSIFY_LANDING_SITE_URL` selects the test link.

## Maintain

Update [desktop docs](../../../docs/desktop.md), [frontend docs](../../../frontend/README.md), [token guidance](../deployment-targets/SKILL.md), and shared publishing notes.
