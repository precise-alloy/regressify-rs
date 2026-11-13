# Desktop Publishing

Verified: 2026-11-13

- [Landing apps](../../src/desktop/landing.rs) are global per-user definitions,
  automatically stored by UUID. The [editor](../../src/desktop/ui/landing.rs)
  has no file picker or Save As. Linked URLs need no local workspace and are
  fetched only by the visitor's browser. Local removal stages the locked
  definition before deleting target credentials and restores it on failure.
- `pulldown-cmark` renders both subheadings into static HTML. Shared frontend
  assets provide logo, theme, and client status; raw HTML/images and unsafe or
  credentialed links are excluded. The preview exposes only generated content.
- [Named tokens](../../src/desktop/deployment/tokens.rs) keep only metadata in
  JSON. Keyring identities use `named:<uuid>`, separate from direct target tokens.
  Provider/account identity is immutable, and target deletion preserves shared
  tokens. The catalog uses a file lock and stale-editor comparison.
- Deployment target and named-token edit dialogs use Kit Form/Field only for
  presentation. Native regressions pass for masked secrets, provider-specific
  fields, saved-secret preservation, and draft cancellation; IDs and accessible
  names remain on the actual controls.
- Saved target/token Select items separate UUID identity from display names;
  provider choices use stable machine keys. Unchanged confirmation preserves
  masked drafts, while actual provider/source changes clear them. Real picker
  tests cover duplicate/renamed labels, missing IDs, locks and exact credentials;
  deployment and token GUI modules pass (8 and 3 tests).
- Native token screenshots wait for Add named token, not the Publish menu's
  identically named entry. Reviewed light/dark compact/large captures show the
  bounded saved-token selector and its Add/Edit/Remove/Close actions intact.
- Saved URL copies use value-generation Clipboard feedback. The pointer/keyboard
  regression preserves encoded URL bytes, allows changed/revisited values during
  feedback, suppresses unchanged repeats and removes unavailable copies. Native
  deployment smoke preserves clipboard backup/restore and adjacent Copy layout.
- [Target settings](../../src/desktop/deployment.rs) retain optional `publishedUrl`
  per ID for reports and landing apps. Only successful publication updates it;
  destination edits (including Azure token replacement) clear it. The runner saves
  URLs with completed activity; landing completion saves before notifying the UI.
  Target/runner regressions and `saved_deployment_urls_reopen_and_copy` cover reload,
  provider isolation, unsafe links, failure preservation, and native copy controls.
- Report publications also retain suite output IDs in completion order. Runs links
  resolve the newest site containing the requested suites, encode single-suite
  paths, and reload asynchronously through artifact refresh. Membership lists every
  suite a deployment leaves on the site (kept suites included); legacy URLs do not
  imply membership. The
  `published_report_sites_` and `runs_open_site_` regressions cover these contracts.
- Unchanged deployments (issue #8, 2026-11-06): the fingerprint covers the
  whole staged site (kept suites by summary). Optional `siteFingerprint` records
  the staged content plus site-shaping target settings. The worker rereads settings and skips the
  upload on a match; Activity offers Deploy anyway (`force`). Every upload first
  drops the fingerprint (write-ahead, crash-safe) and keeps membership. Stale
  requests whose target was edited or removed are refused. An early check on
  `inputFingerprint` (suite file metadata, no generation) skips everything in
  milliseconds; without it, a no-change deploy of a large workspace regenerates and
  stages every selected report before comparing.
- Incremental deployment (issue #8, 2026-11-06): Update keeps other suites using
  the site-published `/_snapshot/deployment.json` (provider hashes per file), with a
  local copy as fallback. Netlify `listSiteFiles` is unnecessary because the state is
  atomic with each deploy. Cloudflare has no API to read a Worker's asset manifest,
  so kept hashes must come from the state. The Deploy dialog shows counts plus a searchable review; the suite list shows per-suite
  site status, an Outdated filter, and "Remove from site: X" (acting on the
  selection when the clicked suite is selected). Preview reuse in
  [snapshot](../../src/snapshot/preview_cache.rs) removes most local cost. On a
  360-image synthetic workspace, previews took ~4.4 s versus ~0.5 s copying.
- Per-suite skip (2026-11-09): selecting already-deployed suites used to regenerate,
  validate, stage, and hash all of them, though only one was newer.
  `Plan::keep_unchanged` now keeps a selected suite by hash when its local
  `<target>.inputs.json` record (per-suite input fingerprint + digest of the site's
  `SuiteState`) still matches. Run IDs alone would miss approvals, so the suite list
  and dialog apply the same rule (`SuiteInputs::verify`) and use run IDs only
  without a record. The per-suite fingerprint omits report text so the UI can
  compute it. Updates only: Replace rebuilds the selection, which keeps
  "replace the whole site" advice in kept-content errors truthful. Optimized
  Cloudflare uploads rewrite the state for the optimized files rather than drop it.
  Azure re-uploads kept/unchanged suites from hash-verified local copies (stashed,
  restored on every exit). Cloudflare budget retries are dormant
  ([why](deployment-cloudflare.md)); their invariants live in the targets skill.
- The dialog's native link uses content-sized flex layout so Copy stays beside it;
  the toolkit's label wrapper centers text inside a stretched button. Deployment
  activity reuses the Log tab's read-only editor, fills available height, and keeps
  retained lines intact. Native tests cover full-height bounds and exact copying.
- Cloudflare publication may succeed without a public URL. Nullable progress and
  saved results retain compatibility with existing strings; confirmed private
  results clear stale URLs. Asset-option edits preserve URLs, routing edits clear
  them. The native target test covers production defaults and advanced settings.
- [Shared analytics](../../src/analytics.rs) emits `regressify_site_deployed` from
  provider-confirmed public completion. Stats reuse initial pre-optimization
  (`original_bytes`) and final (`total_bytes`, `file_count`) Azure package and
  Netlify/Cloudflare manifest inventories without rescanning files. The
  digest fixture counts three paths/15 bytes although only one blob uploads;
  the cached Cloudflare fixture excludes hosting-policy files. Analytics tests
  cover silent failures and client reuse across concurrent calls and new runtimes.
  HTTP timeout cleanup must stay inside the silent tracing scope because request
  futures log on drop; see the [analytics skill](../../.github/skills/product-analytics/SKILL.md).
- [Status summaries](../../src/snapshot/site.rs) use `/_snapshot/status.json`
  with aggregate counts and generation time. They revalidate and permit anonymous
  CORS reads; exact ZIP/Netlify inventories include this extra file.
- [Publishing smoke](../../ci/desktop-smoke/flows/publishing.ps1) passes with
  automatic save, remote links, Markdown, draft guards, token reuse, target
  removal, and light/dark compact/large windows. Catalogs are isolated/restored.
- Keep selector widths bounded: long token names otherwise push toolbar actions
  beyond the dialog. Landing forms use a retained explicit scroll handle, and
  offscreen inputs may be absent from native accessibility until scrolling.
- Landing base/site fields use Kit Form/Field; each dynamic site is a normal,
  unframed GroupBox. The native site-action/scroll test verifies pointer reorder
  still works, the final URL input remains reachable through the owned handle,
  and the thumb can drag back to the top. The scrollbar belongs to the stationary
  relative viewport: attaching Kit's overlay to the scrolling body clips it.
- Native password clicks require the existing post-click `Wait-Control` settling
  before typing. Menu selection also needs a rendered-state wait before checking
  hidden fields. GPUI scroll tests must settle dialog animations before geometry.
- GPUI `label()` returns an explicit accessibility name, not overridden visible
  button text. Assert selected routing in model state separately. Conditional
  scrollbar branches need type erasure because the wrapper changes element type.
- Six production landing browser checks pass on desktop/mobile, including
  no-JavaScript content, anonymous remote fetches, errors/retry, and accessibility.
  Live Azure/Cloudflare publication and Azure-to-Cloudflare status loading passed.
  The authorized Netlify check was blocked by provider account credits, not an
  upload-client failure; do not automatically retry billing restrictions.
- Use `bun run --cwd frontend biome ...` for the installed formatter. Putting
  `--cwd frontend` after `bun x` can interpret `frontend` as a package name. Scope
  formatting to changed files; this checkout has unrelated CRLF findings.
