# Outstanding Issues

Current verification gaps and dated resolution history. Entries marked resolved
are historical evidence, not open implementation tasks.

## Website-Only Deploys (2026-10-29)

- Open: the [website deploy workflow](.github/workflows/website-deploy.yml) has
  not run on GitHub yet. It reuses the live `latest.txt` and regenerates `/mcp`
  with that release's Linux CLI, so docs changes ship without an app release.
- Offline evidence: [release tests](ci/test-release.py) check the workflow's
  triggers, read-only permissions, and secret scope. They also run its download
  steps with stubbed `curl` and `gh`: valid and CRLF manifests, a missing tag, a
  tag with no GitHub release, and a CLI zip whose SHA-256 is wrong or missing.
  The step test needs Bash 4+ with `sha256sum` and `unzip`, so it runs in the
  Ubuntu release checks and skips on `macos-15`, which has only Bash 3.2.
- Accepted risk: it shares no concurrency group with the release deploy. If a
  docs deploy reads the old feed and finishes after a release deploy, it restores
  the previous `latest.txt` until the next deploy.

## Test Comparisons Write No Command Journal Records (2026-10-27)

- Open: [`exec_test_suites`](src/test/test_suites.rs) finalizes each suite in
  `spawn_blocking` without the command's diagnostic context, which is
  task-local. [Comparison records](src/compare/report_gen.rs), both the
  per-pair `Compared` event and the image simplification settings warning, are
  therefore dropped during `test`; `compare` records them. Reports, console
  output, and desktop All run logs still show results and warnings.
- Verified with a temporary probe that ran `StagedSuiteComparison::finalize`
  as `exec_test_suites` does: 0 records without the context, 1 `Compared` and
  1 warning record with it. Passing the context with `runtime::in_scope` would
  fix it, but would add one record per pair to every `test` command journal,
  whose 64-record queue drops records under bursts; measure that first.

## Tall GPU Full-Page Captures Repeat Rows (2026-10-27)

- Open, confirmed: with GPU rasterization, a full-page capture taller than
  16384 px repeats row `y - 16384` for every row `y >= 16384`, so the lower part
  of the PNG shows the top of the page again. Seen in all 32 Allbirds captures
  during the image simplification experiments on Chromium 147 with Intel Arc
  through ANGLE Direct3D 11; software rendering was unaffected.
  [Launch flags](src/test/browsers.rs) keep GPU rasterization on purpose, and
  [full-page captures](src/test/capture.rs) take one `captureBeyondViewport`
  screenshot of the whole content size.
- Reproduced on 2026-10-26 with a temporary loopback probe using the same
  screenshot parameters: an 800x20000 page of 200 uniquely colored bands on
  Chromium 147.0.7693.0 (ANGLE, Intel Arc Pro 140T, Direct3D 11) returned all
  3616 rows from 16384 identical to row `y - 16384`. Not investigated: the cause,
  other GPUs and drivers, and fixes such as tiled captures or software rendering.

## Document CSS Under Strict CSP And In Frames (2026-10-27)

- Open, confirmed: [document hooks](src/test/hooks.rs) inject the CSS override
  (`common/_override.css` or `cssOverridePath`) and the `hideSelectors` and
  `removeSelectors` rules as one `<style>` element, and
  [caret hiding](src/test/page_stability.rs) adds another. A page whose Content
  Security Policy limits `style-src` without `'unsafe-inline'` blocks both while
  hook setup still succeeds, so the override, masks, and caret hiding are silently
  missing from captures unless `bypassCsp: true`. Image simplification is
  unaffected: its filter uses CSSOM and an adopted constructed stylesheet.
- Both elements reach only the main document of the captured page (after each
  navigation and `switchTab`), never iframes, although the
  [scenario docs](docs/create-scenario.md), schema descriptions, and the MCP
  [recipes](src/mcp/guides/recipes.md) and [actions](src/mcp/guides/actions.md)
  guides say the selector lists apply to every document.
- Verified on 2026-10-26 with a temporary loopback probe: under
  `default-src 'self'; style-src 'self'` both `<style>` elements were present but
  no rule applied (computed visibility, display, and caret color unchanged); with
  `Page.setBypassCSP`, which `bypassCsp: true` enables, every rule applied; a
  same-origin iframe received none in any case. The strict-CSP page of the
  [image simplification page fixture](src/test/browser_tests.rs) also asserts that
  its `hideSelectors` rule stays unapplied.

## Image Simplification Scope (2026-10-27)

- By design in the first release: `imageSimplification: pixelate` considers only
  `img`, `video`, and SVG `image` elements in the light DOM of the captured page's
  main document ([page library](src/test/image_simplification.js)). Images in
  iframes or shadow DOM, canvas drawings, and CSS background images stay sharp
  and are not counted in the capture log.
- Deferred: a "simplified" badge in web snapshots and desktop Results. Snapshot
  conversion and Results ignore recorded settings and
  `imageSimplificationWarning`; the warning appears only in `report.json`,
  console output, desktop All run logs, and `compare` command journals.

## Image Simplification Guard Blind Spots (2026-10-27)

- Open, by analysis only: the paint-order guard in
  [the page library](src/test/image_simplification.js) cannot see content inside
  closed shadow roots, has no box for generated `::before`/`::after` content of
  `display: contents` elements, and approximates in-flow generated content by its
  element's border box. An overlay drawn that way over an image could end up
  beneath a pixelated image. Positioned generated content uses resolved offsets;
  relative offsets and transforms move generated boxes (the transformed bounds,
  swept across the originating box for in-flow content), and sticky offsets or
  unresolvable transforms count as overlapping anything inside their clip. Text,
  generated text, and boxes include `text-shadow` and `drop-shadow()` reach, but
  descendants are not extended by a `drop-shadow()` on an ancestor other than a
  text node's parent.
  No affected page has been observed. The browser fixtures cover grid overlays,
  inline, positioned, translated, relatively offset, and generated badges,
  caption and drop shadows, clipped images, and sticky headers; none uses shadow
  DOM.

## Nested Scenario Field Reset (2026-10-27)

- Open: desktop Fields Reset cannot remove a scenario property unless it is the
  scenario mapping's last entry. [Lossless YAML edits](src/desktop/document.rs)
  reject the result with `Invalid generated YAML` and keep the draft unchanged,
  so the override remains and the field shows the error. No source is corrupted.
- Reproduced headlessly with `Document::apply_many` and
  `Edit::Set { parent: "/scenarios/0", value: None, .. }`: removing `delay`
  before `postInteractionWait` fails in both indentationless and indented
  scenario lists, with or without flow values such as `actions: []`. Root-level
  removals and removing the final scenario key (the earlier trailing-indent
  fix) succeed. Found while adding image simplification Fields; unrelated to
  those keys, whose native regression resets only trailing entries.

## GitHub Draft Asset URLs (2026-10-23)

- Resolved locally: [v2.0.6 publication](https://github.com/precise-alloy/regressify-rs/actions/runs/39873111082/job/120161484370)
  created the draft and uploaded all 34 assets, then rejected `latest.txt`.
  GitHub reports draft download URLs under `releases/download/untagged-.../`,
  but the validator required the final versioned path before publication.
- Read-only verification confirmed the uploaded manifest's bytes, size, and
  SHA-256 agree with its metadata and the other assets' recorded sizes/hashes.
  All 34 assets used the exact temporary path named by the draft's `html_url`.
- The [publisher](ci/release_feed.py) now validates that draft URL's HTTPS host,
  repository, and path before accepting its exact asset path. Names, uploaded
  state, sizes, hashes, and completeness remain mandatory. Published-release
  verification still rejects temporary URLs and requires the final tagged paths.
- [Regressions](ci/test-release.py) reproduce the failure and cover new/resumed
  drafts, malformed URLs, wrong draft identities, corrupt/incomplete assets, and
  rejection of temporary URLs after publication. All 51 offline tests pass.
  The live draft was left untouched; hosted publication/deployment remains pending.

## GitHub Draft Release Lookup (2026-10-23)

- Resolved locally: [v2.0.4 publication](https://github.com/precise-alloy/regressify-rs/actions/runs/39861369859/job/120126638235)
  staged all 34 files and created a draft, then failed with
  `GitHub returned an unexpected release`. The tag endpoint returns published
  releases only, so querying it again could not find the new draft. A read-only
  authenticated listing confirmed the draft existed with zero uploaded assets.
- [v2.0.5 publication](https://github.com/precise-alloy/regressify-rs/actions/runs/39868674393/job/120145137557)
  still failed the immediate follow-up lookup after creation. A later read found
  the correct tag and draft/prerelease flags, with no uploaded assets. The log does
  not establish whether caching or delayed visibility caused that lookup failure.
- The [publisher](ci/release_feed.py) now finds drafts through authenticated release
  listings before creation, matching the exact tag. Pages contain at most
  10 releases, with a 1000-release lookup limit; malformed, ambiguous, or incomplete
  listings fail closed. New drafts use the JSON create API's returned metadata
  directly after verifying the exact tag already exists. API reads request fresh
  responses, and failed writes never become missing-release results. Existing
  matching assets are preserved during resume.
  Complete asset verification before publication and strict published-release
  verification afterward remain mandatory.
- The corrected [fixture](ci/test-release.py) reproduces the original failure and
  covers new drafts hidden from listings, resumed drafts, missing uploads,
  published releases, pagination, tag guards, and JSON API failures. All 50
  offline release tests pass locally, including
  isolated native installer fixtures. The real draft was left untouched; a release
  containing this fix still needs hosted publication/deployment verification.

## Release Matrix Timeout Assertions (2026-10-23)

- Resolved locally: the [v2.0.3 Ubuntu offline checks](https://github.com/precise-alloy/regressify-rs/actions/runs/39856579256/job/120096235355)
  reported 32 `KeyError: 'timeout'` errors. The workflow intentionally removed
  custom job and matrix timeouts, but the desktop matrix test still required one.
- [Matrix tests](ci/test-release.py) now assert that the build job and generated
  rows omit custom timeouts, preserving GitHub Actions' default timeout behavior.
  All 48 application/platform selections pass, and the full offline suite passes
  all 45 tests locally with native installer fixtures enabled. No workflow changes
  or live publication were performed for this fix.
- A release containing the corrected tests is required; rerunning the unchanged
  v2.0.3 tag will still execute the stale assertion.

## Windows Release Uninstall Verification (2026-10-23)

- Resolved locally: the [v2.0.2 packaging job](https://github.com/precise-alloy/regressify-rs/actions/runs/39827230980/job/120003049237)
  passed its installer tests, compiled setup, and completed the installation smoke
  checks, then failed with `Cannot bind argument to parameter 'Path' because it is null.`
  A malformed `Join-Path` expression in the
  [uninstall license check](ci/build-installer.ps1) referenced nonexistent variables
  instead of the installation directory. PowerShell parsing alone did not catch it.
- The [regression](ci/test-release.py) executes the actual cleanup loop in temporary
  directories with spaces and literal brackets. It failed before the fix and now
  accepts removed licenses, rejects either leftover license, and preserves unrelated
  user files. All seven `InstallerTests` pass, including isolated native fixtures.
- Hosted end-to-end packaging/publication still needs a release containing this fix;
  rerunning the unchanged v2.0.2 tag will use the broken script. No real application
  installation or publication was performed locally.

## Musl Release Targets (2026-10-23)

- Confirmed in [v2.0.0 ARM64 Musl job](https://github.com/precise-alloy/regressify-rs/actions/runs/39821561437/job/119973968352)
  and its x64 Musl sibling: `openssl-sys` cannot find target OpenSSL and pkg-config
  is not configured for cross-compilation. `cargo tree --locked --no-default-features
  --target aarch64-unknown-linux-musl -i openssl-sys -e features` confirms the CLI's
  default reqwest TLS backend brings in native OpenSSL. The ARM64 build also used
  a glibc C compiler instead of a proper Musl toolchain.
- Musl compilation is technically possible with additional TLS/sysroot work, but
  that alone is not end-to-end support: [managed Chromium](src/test/browsers.rs)
  explicitly rejects Linux ARM64, and Musl needs a compatible browser runtime.
  Both Musl prebuilt targets are removed from CI, required assets, and website
  downloads. Terminal installation rejects unsupported Linux targets before
  downloading or modifying existing binaries.
- Retained: Windows x64, macOS Intel/Apple silicon, and Linux x64 glibc, for CLI
  and desktop. Eight parallel builds produce 16 archives and 16 sidecars; setup
  and `latest.txt` complete the 34-asset release. Full publication/deployment
  verification gates remain mandatory. A new tag containing the fix is required;
  rerunning v2.0.0 uses its unchanged workflow.

## GitHub Releases And Cloudflare Deployment (2026-10-22)

- Open: no hosted tag release or live Cloudflare deployment has verified the
  [new pipeline](docs/desktop.md#release-pipeline). Configure the repository's
  built-in release-token permissions and Cloudflare secrets/domain in
  `precise-alloy/regressify-rs`. Azure release publishing is removed.
- Resolved offline: partial inventories previously passed staging, and publication
  could overwrite assets. All required archives, sidecars, setup, and the manifest
  must match before a draft is published; a second public inventory gate blocks
  website builds/deploys until GitHub metadata matches every local file.
- The website hosts only metadata/install scripts/pages/MCP. GitHub retains
  versioned binaries, so Cloudflare's 25 MiB asset limit no longer constrains them.
  Windows uses a bootstrapper with baked payload hashes and background prepare-only
  mode. Local receipts cannot override feed hashes. Native regressions cover
  preparation while the app runs, stale receipts, and descendant cancellation.
- Offline evidence: all release/installer/script fixtures, updater model tests,
  website type checks/unit tests, and a production site build using a synthetic
  complete feed. Browser checks cover all archive links, OS-specific homepage
  buttons, and desktop/narrow layouts. Real Linux/macOS packaging and native
  application execution remain hosted-run gates, not claims from simulated targets.
  Real Windows application installation, directory migration, and uninstall smoke
  also remain release-CI checks; local tests used isolated synthetic fixtures only.
- Open: the final Tuyen Drive bridge is intentionally deferred until the maintainer
  confirms the new site. Start from `d066dc81a2befb9cb622762cbf3c7a21e8c198fc`,
  backport new updater support, retain old-feed-compatible naming, and require
  old < bridge < site versions. Old clients first install that bridge, then the
  website-selected version. See [Bridge Rollout](docs/desktop.md#bridge-rollout).
- The previously hosted `regressify-mcp` workers.dev Worker is no longer deployed
  by CI. `regressify init` migrates workspaces to `https://regressify.eshn.dev/mcp`;
  retire the old Worker once clients have moved. The root `bun run deploy` still
  targets that standalone Worker.

## Third-Party Notices In Distributions (2026-10-22)

- Open: release archives, the Windows installer, and published reports ship
  third-party components without their license notices. Rust crates and frontend
  npm packages (MIT, Apache-2.0, BSD, ISC, MPL-2.0, and others) are compiled into
  the executables and report bundle, but no third-party notice file accompanies them.
- The subset report fonts in [frontend/src/assets/fonts](frontend/src/assets/fonts)
  are Roboto Flex and JetBrains Mono 2.304 (SIL OFL 1.1), identified from their
  `name` tables. Subsetting removed their license records, and no OFL text is
  distributed with reports or binaries.
- Regressify itself is AGPL-3.0-or-later ([LICENSING.md](LICENSING.md)); archives
  and the installer include `LICENSE` and `LICENSING.md`. Vendored Playwright assets
  keep their Apache-2.0 notices in [src/test/vendor](src/test/vendor/README.md).
  `cargo deny check licenses` verifies Rust license compatibility, not notice delivery.

## Local HTML Report Action (2026-10-21)

- Removed the unsupported Results **Open HTML report** button and its file-based
  launcher. The web snapshot requires HTTP hosting; root-relative assets and
  browser module/fetch restrictions prevent direct `file://` use. Native Results,
  artifact-folder access, snapshot generation, and published-site links remain.
- The [GUI regression](src/desktop/ui/reports.rs) failed before removal and now
  verifies that the action is absent with or without an output folder and while
  disabled, while Refresh reports and Open artifact directory remain visible.
  This supersedes the historical local HTML launch behavior recorded below.

## Mismatch Precision And Pixel Counts (2026-10-21)

- Resolved: web and desktop reports could display tiny positive mismatches as
  zero. [Web](frontend/src/data/report.ts) and
  [Rust](src/compare/image_compare.rs) formatting now adds precision only when
  needed, with scientific notation for extremely small values. Exact zero and
  ordinary two-decimal formatting remain unchanged.
- [Comparison reports](src/compare/report_gen.rs) store the unrounded floating-point
  percentage and exact integer `mismatchPixels`, without a formatted percentage
  string. Snapshot conversion preserves the numeric audit value. Desktop prefers
  the raw value over legacy rounded strings; web and desktop show exact counts
  without inferring absent legacy metadata. Comparison thresholds are unchanged.
- The web precision regression failed before the fix. One-pixel PNG comparisons,
  bit-exact JSON round trips, extreme positive values, legacy metadata, and native
  and browser display regressions cover the new behavior.

## GPUI Kit 0.8.0 Upgrade (2026-10-21)

- Resolved: numeric Fields and Source font-size spinners had unnamed SpinButton
  nodes despite named outer groups. [Fields](src/desktop/ui/mod.rs) and
  [Source](src/desktop/ui/source.rs) now use Kit's `accessibility_label`.
  Both scoped role/name regressions failed before the fix; Windows default smoke
  also verifies the Source spinner's actual name, child editor and focus.
- Resolved: [default smoke](ci/desktop-smoke/flows/default.ps1) searched for Actions
  after Source reconciliation had selected the suite. It now explicitly opens
  Checkout before Fields. The full flow passes without a scrolling workaround.
- Resolved: the [zoom/pan test](src/desktop/ui/tests.rs) left Control held before
  plain scrolling. Kit 0.8.0 preserves modifiers, exposing that test defect;
  stock `change_modifiers` releases Control and the unchanged geometry assertions pass.

## Cloudflare Access Report Data (2026-10-20)

- Resolved: [report fetches](frontend/src/data/report.ts) omitted credentials, so
  Cloudflare Access redirected same-origin manifest, suite, and log requests to its
  cross-origin sign-in page and browsers reported CORS errors. They now send
  same-origin cookies; the unit and `access cookies` browser tests failed before the fix.
- By design: landing status stays anonymous and needs a path Bypass behind Access,
  and desktop public verification cannot pass Access.

## Source Editor Review (2026-10-19)

- Resolved: [Source line commands](src/desktop/ui/source.rs) classified Unicode
  whitespace inconsistently, so uncommenting could slice a UTF-8 character or
  delete a form feed. Commenting and indentation now share ASCII-only indentation
  rules, and uncommenting removes only an actual comment prefix.
- Resolved: Source used UTF-16 columns with Kit 0.7.3's scalar-column API, corrupting
  completions and preventing issue navigation after emoji. Position conversion now
  matches `RopeExt`; new pure and native regressions failed before the fixes and
  pass afterward, including exact adjacent text and issue-navigation wraparound.
- Resolved: Source and document controls used independent undo histories. Accepted
  edits now share bounded document history with grouped typing/deletion and active
  selection restoration; native Source shortcuts, toolbar, and Fields edits agree.
  Focused Fields inputs retain local undo for unapplied drafts.
- Resolved: Source drafts above 4 MiB could evade unsaved guards, and deleting a
  scenario in Source left stale selection. Oversized drafts now remain dirty and
  undoable with explicit save/Fields rejection. Guards inspect pending native text;
  Source inspection reconciles selection. Native paste/cancel/undo/redo/recovery and
  mixed-history regressions pass.
- Resolved: JSON duplicate keys could look clean and be collapsed by Fields; raw
  parsing now rejects them with source locations. Desktop dependency checks no
  longer depend on action viewport filters or suppress independent viewport errors.
  Semantic diagnostics retain field pointers alongside schema errors, including
  scenario-level replacement rules. Missing-field underlines stop at the enclosing
  header, and multiline diagnostic Markdown no longer adds visible backslashes.
- Resolved: Source completion triggers omitted apostrophes and hyphens; native
  quoted viewport entry/acceptance is now covered. Shift+Tab dismisses an open menu
  without changing text, Find respects busy state, and non-schema/no-document
  transitions clear providers and overlays. Focus restoration includes workspace
  identity, preventing a same-relative-path switch from restoring stale focus.
- Resolved: per-keystroke Source inspection and redundant contextual re-inspection
  ran on the UI thread. Feedback now debounces 200 ms, permits one worker, waits
  for native/model acknowledgement, and rejects stale generation/scope/byte results.
  Tests verify 20-edit coalescing, single-flight and ABA freshness, immediate Save
  guards, indexed/memoized positions, retained catalog/diagnostic text, and plain
  text without configuration-value copies. Base/contextual checks share parsing
  and indexing; viewport completion reuses its original CST.
- Final automated verification passed all 1302 non-ignored GUI library tests
  (39 opt-in tests ignored), the GUI build, strict GUI/default CLI Clippy,
  formatting, and maintained-guidance checks.
- Native verification exposed two smoke-driver defects: its manually pumped
  temporary focus window could leave Alt latched, and diagnostic text dragging
  omitted compositor settlement before press/after release. An owned STA message
  loop and settled drag/clipboard-sequence checks fix those paths without changing
  application key handling or weakening assertions. Focused `-SourceFlows` passed
  twice and full `-SchemaFlows` passed, including exact diagnostic copying,
  Enter/Tab/Escape after Alt+Tab, line commands, saves, reload/conflict guards, and
  compact/large light/dark layouts. Preferences and clipboard were restored.

## Run Retention and Verification (2026-10-17)

- Resolved in source: [run tracking](src/run_history.rs) excludes interrupted and
  scenario-only runs from default Results, comparison, snapshot, and deployment.
  Suites finalize independently. [Cleanup](src/run_history/cleanup.rs) supports
  selective strict retention, preview confirmation, automatic policies, protected
  baselines/logs, and stale-preview checks. [Managed staging](src/run_history/staging.rs)
  preserves retry ownership after failed removal and is swept before deployment.
- Second review resolved a staging ownership race: claim and teardown now share
  the catalog lock through the final lock-file removal. Two regressions failed
  before the fix; all six staging ownership/recovery tests now pass.
- Second review resolved stale local HTML opening: Results regenerates the viewed
  output's latest completed full-suite snapshot in the background before opening.
  `latest_html_report_` verifies skipped-comparison freshness, custom output/title
  handling, interrupted-run exclusion, and the actual button's disabled guard.
- Second review resolved scoped-comparison reuse: raw reports atomically record
  capture-inventory coverage. Default Results, suite shortcuts, and snapshots
  no longer reuse a one-scenario comparison as the full run's report. Explicit
  partial review remains available; comparison and snapshot regressions pass.
- Resolved: the deployment diagnostic fixture
  `preparation_phases_and_resizing_record_readable_stage_diagnostics` in
  [deployment tests](src/desktop/deployment.rs) now uses a realistic 500 MiB
  budget and requires a complete diagnostic archive. The full desktop gate passes
  all 965 non-ignored tests; the fixture no longer needs an exclusion.
- Resolved: [image comparison](src/compare/image_compare.rs) reuses image views,
  pairs signature inputs, clamps sample counts, and iterates bounded signatures;
  [report generation](src/compare/report_gen.rs) boxes its internal comparison
  outcome. Comparison regressions and strict GUI Clippy pass without suppressions.
- Resolved: [.gitattributes](.gitattributes) pins frontend text to LF while
  preserving binary detection and exact generated bytes. A regression failed
  before the policy change and now passes with Git autocrlf enabled. Full frontend
  lint, all 29 unit tests, typechecking, and the production build pass.
- Resolved: smoke activation now targets the exact fixture window and avoids
  reactivating an already-focused editor. Cleanup preview summaries have explicit
  accessible names. Native Cleanup, Default, Comparison, Git, Deployment, and
  Artifact flows pass; compact/large light/dark screenshots were inspected.
- The final reviewed GUI/worker gate passes with no exclusions: 1266 library tests and
  11 worker tests, with 39 and 4 opt-in tests ignored respectively.
- Final CLI validation passes 605 tests with 30 opt-in cases ignored. The cached
  Chromium loopback test was also enabled explicitly and passed without downloads.
  Native capture/cancel/rerun and scenario-only smoke passed after the review fixes,
  preserving sibling references, saved source, clipboard data, and preferences.
- Resolved in the 2026-10-19 Source review: native Basic Auth Ctrl+Space and the
  existing Enter/Tab/Escape assertions now pass in both focused Source and full
  Schema smoke. The current editor and repaired smoke-driver lifecycle were
  verified without changing OS keyboard settings or toolkit source.

## Snapshot Frontend Contract, Performance, and Viewer Hardening (2026-10-15)

- Resolved in source: [report schemas](frontend/src/data/report.ts),
  [log chunk loading](frontend/src/data/logs.ts), and
  [suite store validation](frontend/src/data/store.tsx) now enforce Windows
  reserved-name/character/control rules on suite and capture IDs, reject
  `status: "pass"` captures with engine errors, validate log chunk ownership,
  uniqueness, non-empty chunks, and sequence boundaries, enforce exact
  `_snapshot/data/manifest.json` bootstrap paths and suite pass/fail/error
  consistency, and bound report/log requests with timeouts and immediate
  `AbortSignal` cancellation.
- Resolved in source: [ComparisonViewer](frontend/src/components/ComparisonViewer.tsx)
  deduplicates and aborts detached `Image` size probes on unmount and verifies
  `isLoaded` on the source lane before projecting crosshairs;
  [ImagePage](frontend/src/pages/ImagePage.tsx) scopes viewer and log component
  keys by `suiteId` as well as `capture.id`;
  [CaptureLog](frontend/src/components/CaptureLog.tsx),
  [ReviewNavigation](frontend/src/components/ReviewNavigation.tsx), and
  [OverviewPage](frontend/src/pages/OverviewPage.tsx) avoid redundant
  `TextEncoder`, `TextDecoder`, `Date`, and string formatting/lowercasing work;
  and [styles.css](frontend/src/styles.css) removes obsolete layout selectors
  and updates `@media print` toolbar rules.

## Compare Module Alignment, Performance, and Approval Hardening (2026-10-15)

- Resolved in source: [vertical gap alignment](src/compare/image_compare.rs)
  previously supported at most one shift capped at 512 rows, allowed unrelated
  top-of-page changes to pull `best_start` across intact sparse middle sections,
  rejected short or whitespace-padded footers below an insertion, skipped
  same-height viewport shifts, and disabled anti-aliasing detection whenever
  `ref_height != test_height`. It now supports single-band and multi-band
  vertical alignment (`referenceGaps`/`testGaps`), aligns middle insertions
  above 512 rows and same-height viewport shifts, penalizes breaking clean
  unshifted rows while capping unaligned row gain, evaluates contested tail
  support across whitespace-padded footers, and maps source rows for
  anti-aliasing checks. Both [desktop Results](src/desktop/ui/reports.rs) and
  the [web viewer](frontend/src/components/ComparisonViewer.tsx) map crosshairs
  across multi-band alignments.
- Resolved in source: the near-perfect-row requirement rejected supported
  multi-band shifts when part of every middle row changed, and half-row
  signatures ignored useful variation between otherwise uniform rows.
  [Anchor selection](src/compare/image_compare.rs) now accepts local informative
  signature matches as evidence for partially changed rows while retaining
  clean-row penalties, constant-background rejection, deterministic ranking,
  and bounded search. Regressions cover insertions/deletions, mixed and viewport
  shifts, 2-50% changed columns, and the minimum supported-row boundary.
- Resolved in source: [Save As](src/desktop/workspace.rs) classified the requested
  path before resolution, allowing Windows case aliases to bypass suite-name
  checks when the source was Data. Creation and file classification now use the
  resolved destination with Windows folder-case handling. Saving a Data template
  into a suite validates suite content and returns a Suite document; invalid
  destinations do not create files, and ordinary dot-prefixed helpers remain
  supported.
- Resolved in source: [PNG loading and diff rendering](src/compare/image_compare.rs)
  decoded each file after a separate header open, cloned the decoded `RGBA8`
  buffer, decoded both images even when raw bytes were identical, and evaluated
  per-pixel YIQ deltas across byte-identical rows. It now reads bounded bytes
  once, skips the second decode on byte-identical captures, short-circuits
  byte-identical prefix/row slices, and stages diff PNGs inside worker threads
  in [report generation](src/compare/report_gen.rs).
- Resolved in source: [report generation and approval](src/compare/report_gen.rs)
  used `replace(".png", "_diff.png")` (corrupting step names containing `.png`),
  did not bound manifest reads or preflight candidate PNGs in CLI/suite approval,
  did not share cached diagnostic archive states across `StagedSuiteComparison`
  jobs, and matched reserved directories in [suite selection](src/compare/actions.rs).
  All are resolved and covered by unit, snapshot, and desktop GUI tests.

## MCP Authoring Guidance and Frame Selector Capture (2026-10-15)

- Resolved in source: [selector capture](src/test/capture.rs) had `left: rect.left, top: rect.top`
  pasted into the `"invalid_selector"` match arm of `selector_capture_problem`
  instead of `selector_capture_script`, causing `save` actions with both `frame`
  and `selector` to fail with `returned no valid left coordinate` and invalid CSS
  screenshot selectors to lose their `InvalidData` diagnostic message. Both the
  script coordinates and the match arm are restored and covered by unit tests.
- Resolved in source: [managed agent instructions](src/init.rs) told consumer
  agents to read `topic: "hooks"` before writing "browser or Node lifecycle
  scripts" and omitted the browser-only hook rule, related-state `viewport`/full-page
  capture rule, and mandatory `regressify ref --test-suite <suite-name>` verification
  workflow. [Workspace schema help](src/config/mod.rs) also referred to
  `document/Node hooks`, and [action schema help](src/types/actions.rs) described
  preflight-checked `input` and `press` errors as runtime failures and omitted
  the `__` and `frame` rules on `save`.
- Resolved in source: [MCP startup instructions](src/mcp/catalog.rs) and
  [guides](src/mcp/guides/overview.md) now list all 32 `get_example` tasks by
  category and warn agents about first-match hidden responsive DOM duplicates
  (`:visible` / `>> visible=true` and `viewportNames`), native `<select>` /
  checkbox requirements versus custom `role="combobox"` / `role="switch"`
  `click` interactions, `save` `__` and `frame` rules, and non-CSS JS/video
  animations. Existing consumer workspaces need `init`/desktop folder open to
  refresh `AGENTS.md`, and hosted MCP Workers need regeneration and redeployment.

## Desktop and Log Hardening (2026-10-15)

- Resolved in source: [run polling](src/desktop/ui/runs.rs) redrew Activity every
  100 ms even without changes, and [hidden run logs](src/desktop/ui/runs/log.rs)
  rebuilt up to 2 MiB of editor text on changed ticks during capture. Polling
  remains at 100 ms, but UI/log updates coalesce over 500 ms, unchanged ticks
  redraw only for elapsed-second changes, and hidden capture logs load only when
  opened. Terminal updates bypass throttling; visible deployment logs and
  completion archive health remain intact. Native scroll latency has not been
  measured; expanded Activity scenario rows are still eagerly rendered.
- Resolved in source: lossless [PNG optimization](src/optimizer/image_optimizer.rs)
  rewrote capture files without updating their `{image}.log.json` sidecar digests,
  turning optimized capture logs into `LoadingError` and erasing baseline log
  provenance on approval. Valid sidecars now refresh their bound SHA-256 digest
  while pre-existing mismatches remain flagged; [report](src/compare/report_gen.rs)
  and [log](src/desktop/logs.rs) readers cache archive verification by operation ID.
- Resolved in source: [file browsing](src/desktop/ui/files.rs) classified any
  `.tests.` path as `DocumentKind::Suite`, and `Refresh files` updated `entries`
  without refreshing `suites`. Both now share [workspace classification](src/desktop/workspace.rs)
  and update suite/effective state off the UI thread; in-memory edits, undo/redo,
  and selection changes reuse cached catalog and workspace values.
- Resolved in source: [scoped run logs](src/desktop/ui/runs/log.rs) exclude
  outer command journals and clear stale archive state on reset/start,
  [Capture Log](src/desktop/ui/reports/log.rs) retains viewport/attempt headings
  when filtered to terminal records, [published URL replacement](src/desktop/deployment.rs)
  normalizes host case and trailing slashes, [landing removal](src/desktop/landing.rs)
  stages locked definitions before credential deletion, and [post-Git reloads](src/desktop/ui/git.rs)
  use `Session::reload_workspace` without recreating discarded default files.

## MCP Deployment Verification (2026-10-14)

- Resolved missing gates: generated [Worker deployment](src/mcp/worker/package.json)
  now requires a high/critical dependency audit, typecheck, tests, and dry-run
  build. [Root deployment](package.json) additionally requires Rust
  formatting/MCP tests/locked CLI build and exports the checked local source,
  without implicit Git pulls or installed-CLI lookup.
- [Package tests](src/mcp/worker/tests/worker.test.mjs.template) mock npm and
  Wrangler to prove every failed command stops publication. The six prior
  high-severity audit findings are resolved by the upgraded dependencies:
  fresh export verification passes with zero reported vulnerabilities,
  TypeScript checks, all 12 Worker tests, and the dry-run build.
- Dependency upgrades initially introduced caret ranges, failing the export
  contract despite a matching lockfile. Exact pins now preserve those upgraded
  versions, and the export test names invalid pins and checks resolved direct
  versions. No advisory exceptions were applied; audit-service failures still
  block deployment.

## MCP Related-State Capture Coverage (2026-10-14)

- Resolved in source: navigation guidance recommended a header-only default
  capture, which could miss a menu incorrectly expanded outside the header
  before interaction. [MCP guides](src/mcp/guides/scenarios.md) and the
  [navigation example](src/mcp/catalog.rs) now recommend the same larger
  viewport/full-page context across related states, without forcing the initial
  state closed. Existing hosted MCP Workers need regeneration and redeployment.

## Desktop Scrollbar Clipping (2026-10-14)

- Resolved in source: Test suites, Scenarios, and Landing apps attached scrollbar
  overlays to their scrolling content, moving the tracks out of the viewport.
  [Navigation](src/desktop/ui/navigation.rs) and the
  [landing editor](src/desktop/ui/landing.rs) now attach them to stationary
  parents while preserving their existing scroll handles and row identities.
- The [navigator regression](src/desktop/ui/tests.rs) fails before the fix and
  passes afterward, covering wheel scrolling and thumb dragging in both themes
  at minimum/large window sizes. The landing test also checks dragging back from
  the last site. Other application scrollbar attachments use stationary parents
  or the toolkit's separate overflow wrapper.
- Focused interactive Windows checks passed for all three panes in light/dark
  minimum/large windows: wheel to the last item, drag back to the first, and
  inspect rendered screenshots. Preferences and publishing catalogs were
  restored afterward. The broader default smoke stopped separately on Windows
  clipboard restoration; it is not recorded as a complete pass.

## VS Code Workspace Launch (2026-10-13)

- Resolved in source: opening VS Code through its URI handler prompted for
  external access and could replace an unrelated workspace, interrupting its
  session. The [shared launcher](src/desktop/vscode.rs) now starts the installed
  editor directly with an explicit folder and `--new-window`.
- VS Code focuses a window already open on that folder; otherwise it opens a
  dedicated window. Git file actions include their repository root, and Windows
  verbatim paths normalize to the same folder identity. Regressify drafts and
  normal VS Code workspace-trust protection remain unchanged.
- Launcher unit tests cover discovery, literal arguments, path identity, missing
  installations and spawn errors. The existing native external-menu regression
  checks the launch contract and draft preservation without opening VS Code.

## Consumer Site Memory (2026-10-13)

- Resolved in source: generated agent guidance and MCP instructions did not tell
  agents to retain hard-won site knowledge, allowing repeated investigation and
  mistakes. They now require reading relevant consumer `.ai/memory/` notes before
  work and recording reusable lessons, failed approaches, verified fixes, and evidence.
- Each note covers one specific behavior or issue on one site and is at most 60
  lines, with a compact linked index. Agents split/update notes and exclude private
  data; initialization and the public MCP server do not manage the memory files.
- [Initialization](src/init.rs) and [catalog regressions](src/mcp/catalog.rs) check
  the policy; [Worker tests](src/mcp/worker/tests/worker.test.mjs.template) exercise
  startup instructions, tools, resources, and prompts. Existing projects need
  initialization with an updated CLI or desktop build; hosted MCP content needs
  regeneration and redeployment.

## Image Viewer Parity (2026-10-13)

- Resolved in source: desktop lacked wipe/overlay; the web viewer lacked drag-pan,
  fit-to-window, pointer-anchored zoom, and scoped image keyboard controls. Both
  now offer six comparison modes, no-upscale fits, actual size, 25-800% zoom steps,
  pan, modifier-wheel zoom, and double-click. Ordinary scrolling remains native.
- [Native tests](src/desktop/ui/reports.rs) cover blend controls and unavailable
  sources; [six-mode checks](src/desktop/ui/tests.rs) cover fit, pan, and anchoring.
  Native Comparison smoke verifies rendered wipe/overlay source pixels, both
  themes/window sizes, menus, crosshairs, and preference restoration.
- [Web tests](frontend/tests/report.spec.ts) cover fits, exact drag offsets,
  anchored wheel zoom, keyboard isolation, per-mode state, and gesture cleanup.
  Existing published snapshots need regeneration and redeployment, not recapture.
- Follow-up: pointer zoom now retains its pixel anchor across fit boundaries,
  captures modifier-wheel events in empty viewport space, and never hands zoom
  limits to the browser. Side-by-side uses full-span equal-width lanes and retains
  crosshairs without a Diff image. Browser regressions cover each lane, no-diff
  comparisons, source-size differences, and listener cleanup.
- Resolved oversized-image overflow: the name and controls no longer share the
  images' scroll surface. Side-by-side keeps two/three fixed-width lanes with
  separate native scrollbars, bounded height, and reachable image edges.
  Geometry and gesture regressions retain pixel anchoring through scrollbar
  transitions and verify stationary controls on desktop/mobile.
- Follow-up resolved in source (2026-10-14): side-by-side scrollbars now synchronize
  both axes from any lane, including drag and keyboard movement. Lanes share
  maximum-width/maximum-height canvas bounds, and zoom anchoring remains intact.
  Desktop/mobile browser regressions cover two/three lanes and unequal sizes.
- Follow-up resolved in source (2026-10-14): desktop and frontend crosshairs span
  the full corresponding visible lanes, including empty space around the shared
  canvas and small-image padding. Stationary overlays retain image-coordinate
  mapping without enlarging scroll ranges. Regression checks cover blank-lane
  hover, full-lane bounds, scrolling, diff bands, and unavailable images. The
  embedded frontend bundle is rebuilt; existing published reports need snapshot
  regeneration and redeployment, not recapture.
- Follow-up resolved in source (2026-10-14): both viewers allow synchronized
  horizontal and vertical dragging of fitted canvases, bounded by the widest or
  tallest image reaching a lane edge, without changing scale or creating overflow. GUI and
  browser regressions cover both edges, resize, fit reset, and pointer anchoring;
  native Comparison includes fitted dragging in both themes and window sizes.

## Snapshot Overview Mobile Layout (2026-10-13)

- Resolved in source: the overview table required horizontal scrolling on phones,
  and engine-error labels took a separate line below suite names. Suites now use
  stacked, labeled metrics at narrow widths, with all sorting options retained.
  Inline numeric error badges expose their full labels on hover, focus, and tap.
- Follow-up resolved in source (2026-10-14): mobile total/passed/failed counts now
  share a centered value column, stacked summary rows have a separator, and all
  summary values use bold text.
- Report content now uses the root scrollbar without a duplicate review gutter.
  The sidebar scrolls independently below the sticky header; breadcrumbs and page
  controls scroll normally. Empty root gutters retain a theme-aware track color.
- [Browser regressions](frontend/tests/report.spec.ts) failed before the fix and
  pass afterward, covering 320px through the table breakpoint, accessibility,
  themes, filters, the dated heading, and preserved review breadcrumbs/footers.
- Existing hosted snapshots need regeneration and redeployment; no recapture is
  required.

## Deferred Capture Resources (2026-10-13)

- Resolved in source: fast lazy-load scrolling could leave a decoded placeholder
  in place and later report successful settling. The slow-image
  [pixel regression](src/test/page_stability.rs) failed before resource-aware
  scrolling and passes afterward. Visible native/picture images and common
  lazy-loader states now finish before scrolling away; clipped slides are ignored.
- `lazyLoadScrollDelay` and `lazyLoadTimeout` support workspace, suite, and
  scenario overrides. Native Chromium tests cover delayed resources, background
  pixels, growing pages, deadline cleanup, long waits, and preserved action state;
  config and desktop tests cover inheritance and validation.
- Existing reference/test PNGs need recapture and review. Custom loaders without
  observable resource state and arbitrary CSS backgrounds can still require an
  awaited browser ready hook; this is not a global network-idle guarantee.

## Diff Band Crosshair Alignment (2026-10-13)

- Resolved in source: the comparer could align an inserted/deleted vertical band,
  but desktop and web crosshairs still used unaligned rows. The exact chosen gap
  now travels through saved comparison and snapshot metadata; both viewers map
  through diff coordinates and omit impossible target pixels within the band.
- [Browser regressions](frontend/tests/report.spec.ts) reproduced the row offset
  and extra guide before the fix. Native geometry and real generated-band pointer
  checks in [Results](src/desktop/ui/reports.rs) cover all source panes, both gap
  directions, zoom, padding, and reset. Legacy reports remain readable; rerun
  Compare for their saved run, then regenerate/redeploy snapshots. No recapture
  is required.

## Snapshot Image Layout Stability (2026-10-13)

- Resolved in source: report images omitted width/height attributes and comparison
  layout used placeholder dimensions until originals loaded. Images now have
  explicit dimensions, and [snapshot conversion](src/snapshot/capture_report.rs)
  publishes decoded per-source sizes before loading without another image decode.
- The delayed-load [browser regression](frontend/tests/report.spec.ts) failed before
  the fix and covers all comparison modes, unequal sizes, failed loads, and legacy
  reports on desktop and mobile. Preview bounds and lazy source loading are preserved.
- Existing hosted reports require regeneration and redeployment; no recapture is
  needed. Older payloads without size metadata retain runtime size discovery.

## Capture Log Source Traceability (2026-10-12)

- Resolved in source: capture logs obscured suite/scenario/viewport identities,
  selectors, URLs, artifact paths, and capture names with SHA-256 fingerprints.
  New captures retain readable, bounded source metadata in native and web logs;
  simulated free text such as `input.value` remains omitted and URL credentials
  remain redacted. Historical hashes cannot be reversed and remain unchanged.
- The [real CLI logging fixture](tests/desktop_worker.rs) verifies archived and
  published metadata, free-text/credential exclusion, quiet mode, and complete
  log linkage. Native reader tests and desktop/mobile web checks cover searching,
  legacy logs, downloads, safe text rendering, and layout.

## Browser Ready Hooks After Navigation (2026-10-12)

- Resolved in source: 0.88 could report `CSS override or browser ready script
  failed ... (Unknown)` after navigation, with local detail `Cannot find context
  with specified id`. DOM readiness and hook evaluation reused Chromiumoxide's
  cached execution-context ID after Chromium had destroyed that context.
- [Current-document evaluation](src/test/runtime.rs) now lets Chromium select
  the live main context without changing hook order, promises, errors, or bounds.
  The deterministic [hook regression](src/test/hooks.rs) failed before the fix
  and passes afterward, including once-only CSS and both ready scripts. The same
  18-capture live navigation probe went from 10 context failures to zero.

## Snapshot Failure Classification (2026-10-12)

- Resolved: snapshot conversion treated the legacy comparison report's
  `engineErrorMsg` as capture-only, labeling dimension mismatches, missing
  references, and comparison failures as engine errors. Comparison diagnostics
  now remain separate, while action/navigation failures and timeouts retain
  engine-error status even when an error placeholder image exists.
- [Converter regressions](src/snapshot/capture_report.rs) verify classification
  and published counts; [frontend regressions](frontend/tests/report.spec.ts)
  cover legacy payloads, filters, badges, and Info labels on desktop and mobile.

## Browser Script Runtime and Ready Timing (2026-10-09)

- Resolved: `onBeforeScript`/`onReadyScript` previously launched Node and required
  workspace Playwright; `onReadyScript` ran after actions, too late to dismiss a
  blocking banner. All scripts now execute directly in Chromium. Both ready
  settings run after readiness and before actions, once per selected document.
- [Hook regressions](src/test/hooks.rs) cover standard declarations, promises,
  errors, timeouts, CSP, navigation, and selected documents. The
  [reference/test fixture](src/test/browser_tests.rs) passes with an empty worker
  PATH, verifying banner removal, CSS, peer isolation, and preserved references.

## Stock Desktop Palette Contrast (2026-10-08)

- Task 019's uncommitted stock-theme pilot removes the custom palette, UI
  typography and radius overrides in [design.rs](src/desktop/ui/design.rs).
  Its unchanged `both_palettes_keep_text_readable_and_keyboard_focus_visible`
  test initially failed at light-theme secondary text: 4.35:1 versus the required 4.5:1.
  The pre-change palette passes the same test. Other pairs are not verified by
  this failed run; it stops at the first failure.
- Mitigated in the uncommitted [task 019](plan/gpui-component-reuse-tasks/019-stock-theme-sizing.md)
  candidate under the owner's contrast-only exception: failing foregrounds
  retain hue/saturation and adjust lightness; danger tints retain their stock
  visible colors. All original contrast pairs plus danger hover/active pass.
  No threshold, toolkit source or dependency change. Native minimum/large
  light/dark flows and the consolidated final Windows Rust gates pass.

## Comparison Fit-Width Pixel Rounding (2026-10-08)

- Task 019 normal sizing exposed aspect-ratio layout before pane-edge rounding:
  a side-by-side 300x1600 canvas painted 1379.5px high in a 259px-wide pane,
  versus 1381.33px required by the existing one-pixel assertion.
- The owner approved a bounded public measurement repair. FitWidth comparison
  panes now have device-pixel-aligned widths; fit-area/manual modes and all zoom
  policies remain unchanged. The failing regression and full `image_previews_`
  (3), side-by-side scrolling (1) and image menu (1) filters pass. Native
  Comparison also passes fit/pixel/gesture checks at minimum, compact and large
  sizes in both themes; no toolkit/private API change.

## Native Smoke Clipboard Restoration (2026-10-08)

- Resolved on 2026-10-17: the runner now materializes an owned clipboard backup
  before any test mutation and restores it in the outer `finally`, including
  initialization and flow failures. Text, mutable arrays, and streams no longer
  depend on a live clipboard provider. Unreadable user formats fail before writes;
  missing private formats may be discarded only inside the fixture-owned scope.
  Headless regressions and native Default/Comparison/Git/Deployment flows pass.
- Task 018's authorized adjacent repair moves Overview Details to the existing
  clipboard sequence-before-read helper. A new headless regression fails before
  the change and passes afterward; final full native Default and cleanup pass.
  The shared helper still retries only clipboard-open contention for two seconds
  and rethrows failures, now with data-free type/HRESULT/elapsed diagnostics.
  This passing run does not recover earlier failed backups or prove every
  historical restoration failure solved; the later ownership fix is recorded above.
- Task 018's rebuilt Default attempt failed at `default.ps1:20` reading the
  Overview Details clipboard. One unchanged retry reached all Overview/Command
  size/theme checks, then failed at `default.ps1:40` restoring its clipboard
  backup with `SetDataObject`. Both reported that the requested Clipboard
  operation did not succeed. Preferences were restored, but the retry's
  clipboard backup was not. Neither attempt reached the font-size checks;
  those attempts were blocked, with no relaxed clipboard assertions.
- Task 017 Default smoke passed its palette checks, then failed at
  [Results cleanup](ci/desktop-smoke/flows/results.ps1#L40): WinForms
  `Clipboard.SetDataObject` reported that the requested operation did not succeed.
  Preferences were restored; the clipboard backup from that attempt was not.
- A complete unchanged retry passed. Those earlier failed backups cannot be
  recovered from a later successful run. The owned-backup fix above addresses
  the current lifetime failure; no clipboard contents were logged.

## Git Tree Lifetime Compatibility (2026-10-08)

- Mitigated in the task 015 candidate: styled Kit Tree 0.7.1 retains its own
  TreeState through its stored context-menu renderer even with no menu configured.
  The mounted two-tree window-close regression reproduced the leak while GitPanel
  itself released. The owner approved public base Tree with the same styled
  ListItem/scrollbar; the unchanged lifecycle regression passes with that adapter.
- Keep the release test and adapter until a replacement passes ownership checks.
  All seven `git_tree_` cases and rebuilt native Git smoke pass. No dependency,
  toolkit source, private API or fork was introduced.

## Git Tree Checkbox Interaction (2026-10-08)

- Resolved in the task 015 candidate: Kit Tree expands a row on mouse-down,
  which can move an embedded folder checkbox before click completion. The
  existing conflict/submodule folder-selection test failed after migration and
  passes with local pointer isolation that focuses/selects by stable ID without
  changing expansion. All original selection and disabled-state assertions remain.
- Application guards, not disabled appearance, lock selection during operations.
  Focused regressions cover both trees, reviewed request identity, read-only
  browsing and obsolete diff completions. Native Git smoke passes on the final
  rebuilt candidate, retaining the separately approved header exception.

## Capture List Refresh Identity (2026-10-08)

- Resolved in the task 014 candidate: report refresh retained a numeric index,
  selecting another capture after rows were reordered. The new native-event
  regression failed with the prior logic and passes with unique-key restoration.
- The List pilot also reproduced duplicate-key row retargeting. Ambiguous rows
  now have catalog-scoped identities and stale click callbacks are rejected;
  the two `flat_collection_capture_` cases pass. Native migration acceptance
  remains tracked in the task bundle rather than implied by these regressions.

## Controlled Disclosure Keyboard Compatibility (2026-10-08)

- Mitigated locally: published Kit 0.7.1 AccordionTrigger lacks keyboard focus
  and activation. A new pointer-then-Space regression failed before the approved
  public label-focus adapter in [Fields](src/desktop/ui/mod.rs).
- Five `controlled_group_` cases cover raw/unsupported-choice errors, independent
  collapse preference, actual input, keyboard activation, filter identity and
  focus return. Native Schema verifies positive Fields/search state and exact
  source at both sizes/themes. Rich workspace/activity headers keep toolkit
  Buttons within Collapsible; no toolkit edits or new keyboard deferral remain.

## Compact Desktop Header (2026-10-08)

- Follow-up resolved 2026-10-15: the [native header regression](src/desktop/ui/tests.rs)
  now verifies that `workspace-path` is hidden at 1000px while controls fit the
  window-control reserve, and mounted with the expected path from 1400px, matching
  the [current title bar](src/desktop/ui/overview.rs).
- Resolved in the task 019 candidate: the application header is bounded by
  viewport width minus the pinned toolkit's native caption slots. A long
  workspace-name regression failed before that bound and now passes, preserving
  full accessible paths and normal controls. Window minimum remains 1000x640.
- Native Titlebar and Git pass theme switching without widening at that minimum
  and at 1600x1000, including long conflict status and all window-control bounds.
  Git's full tooltip has explicit plain-text accessibility and reads current
  status through a weak owner, avoiding a stale elapsed-time snapshot. Exact
  tooltip/update tests and native full-status checks pass.
- The earlier exception history follows; it no longer licenses wide-only theme
  workarounds. The approved consolidated final Windows Rust gates pass, including
  the original header assertion; the Linux baseline remains separately tracked.
- Task 015 native Git smoke reached the final compact conflict theme switch,
  then could not find Light because the appearance control was clipped. The
  owner extended wide-only theme selection to that Git step; both compact
  conflict captures and all assertions remain. Task 019 must remove it too.
- Task 014's integrated gate and an isolated `ad26ca0` baseline also fail
  `git_large_tree_and_status_fit_compact_and_wide_windows`: a long Git status
  pushes `open-workspace` beyond the window-control reserve. The owner retained
  the task 019 header exception; the assertion remains active and non-green.
- Confirmed during task 013 native Schema smoke: at requested 1000x720
  (1016x720 captured outer bounds), the title bar clips the appearance control
  and the next Dark menu choice is unavailable. Disclosure/focus/source checks
  passed in large and compact light mode before this failure.
- The owner approved wide-size theme selection for intermediate Schema/default
  body checks. [Task 019](plan/gpui-component-reuse-tasks/019-stock-theme-sizing.md)
  owns actual header fit, native theme switching at the approved minimum without
  widening, and removal of the workaround before final acceptance. This is not
  green compact-header evidence or permission to exceed the 1200x720 ceiling.

## GPUI Upgrade Baseline (2026-10-06)

- Resolved in the native test: the schema jump/hover/save-guard test clicked its
  third diagnostic without scrolling it into view. The retained GPUI Kit 0.6.1
  executable and the 0.7.1 candidate fail identically on invisible
  `source-diagnostic` 2. The authorized fixture repair scrolls before clicking;
  all existing cursor, hover, draft and save-guard assertions remain and pass.
- GPUI Kit 0.7.1 production compilation and the two new root-overlay lifecycle
  tests pass. Native Windows default, Artifact, Schema, Comparison, Git,
  Publishing and Deployment flows pass. Final GUI/library gate: 1,081 passed,
  32 ignored; worker gate: 11 passed, 4 ignored. Production lint and build pass.
  The owner approved forward-oriented selections after line commands because
  the release collapses reverse setter ranges. Exact selected text and native
  keyboard-created CRLF/Unicode repeat-toggle behavior are retained and tested.
  Git activity refresh no longer focuses its read-only log; a regression failed
  before the fix and passes for all three editable Git inputs. The
  [migration ledger](plan/gpui-component-reuse-tasks/README.md) tracks completion.

## GPUI Toolbar Focus Compatibility (2026-10-07)

- Mitigated locally: on Windows, published GPUI Kit 0.7.1 Toolbar backward
  wrapping failed in native input and a two-button public fixture, while Right
  and Shift+Tab worked. The owner-approved
  [ToolbarFocus adapter](src/desktop/ui/design.rs) uses bounded forward focus
  order and scoped destinations without changing toolkit sources.
- The adapter yields to active toolkit popups and child input handlers. Five
  `toolbar_` regressions cover wrapping, disabled/busy controls, menu/input
  isolation, dynamic children and owner release; native default, Schema, Git
  and Deployment flows pass. Keep the adapter until a replacement passes those
  checks. This is not a new unresolved task or a passing Linux gate.

## Managed Menu Focus Compatibility (2026-10-07)

- Mitigated locally: on Windows, GPUI Kit 0.7.1 ContextMenuExt could retain
  focus on its disappearing popup after Escape. The owner-approved shared
  [dismissal helper](src/desktop/ui/design.rs) uses a weak deferred callback to
  invalidate that generation and restore focus only while the old popup still
  owns it. A newly opened dialog or menu is not overridden.
- Suite/image lifecycle regressions cover identity, refresh/mode/visibility
  invalidation, focus and owner release. Native Artifact right-click/Escape/F2
  and full Comparison flows pass. No toolkit edit or new deferral is required;
  retain the helper until replacement behavior passes the same checks.

## Linux Baseline Verification (2026-10-06)

- Existing Ubuntu Git 2.34.1 reports usage errors in three remote rename/remove
  model tests. The browser installation complete-archive test times out on a
  zero-second cache-lock wait, and update scheduling/cache clock assertions vary.
- Current headless-model run: 859 passed, 6 failed, 31 ignored. An isolated
  pre-upgrade export at `278be67` reproduced the same categories: 858 passed,
  7 failed, 31 ignored. These are not established as GPUI upgrade regressions.
- The owner approved separately tracking this non-green local Linux baseline
  while Windows migration continues. Current Linux CI verification remains
  required before release; no system Git/toolchain upgrade or model repair was
  performed as part of the UI migration.

## MCP QA Findings (2026-10-06)

- Resolved in source: singular scenario `selector: viewport` and `save` with
  `selector: viewport` searched for a `<viewport>` element and failed, although
  `selectors: viewport` captured the viewport. All three now capture the visible
  viewport without scrolling; the first two keep their `FINAL` and `$<name>`
  file names. The cached-Chromium selector pixel fixture covers both.
- Resolved in source: agents captured `header` after opening navigation menus.
  Element captures are clipped to the element's border box, so dropdowns,
  drawers, and body-level panels outside it were cut off. A probe confirmed the
  clip and that `viewport` or the panel's own selector includes the opened
  layer. The scenarios guide, schema help, overview rules, a navigation recipe,
  and the `navigation-menus` example now explain targets for common opened
  layers.
- Remaining: schemas cannot tell whether a container holds its opened layers;
  agents must inspect markup or styles. The hosted Worker needs regeneration
  and redeployment to serve this guidance.

## MCP Knowledge Accuracy Review (2026-10-06)

- Resolved in source: MCP guides, examples, and schema help contradicted the
  runtime. `wait` with `url` was described as using the number as the URL
  timeout (the example slept 10 seconds after matching); troubleshooting cited a
  nonexistent `tests/<run-id>` capture folder; inherited `needs` saves were said
  to suppress final captures; `remove` was said to delete DOM nodes; CSS hooks,
  `delay`, `postInteractionWait`, and lazy-load scrolling had wrong timing.
- Added verified selector-engine limits, a parser-checked `recipes` topic, the
  setting scope matrix, report fields and messages, capture-record reading
  (`ActionIndex`, `Condition`, local SHA-256 matching), run-ID reuse/selection,
  literal action values, and viewport emulation limits. Catalog tests now parse
  and schema-check every guide YAML fence; the exported package checks pass.
- Resolved in source: page-level failures were reduced to a keyword category in
  reports, error markers, and progress (`Unknown failure` for a missing
  screenshot selector; Node hook output never surfaced; an unknown-field error
  printed `cause=Timeout` because its field list contains `readyTimeout`).
  Failure summaries now name the step, unmet condition, duration, and category
  from typed journal fields, such as
  `Action 1 (click): no element matched after 1539 ms (Timeout)`; a
  `scenarioTimeout` names the step still running. The console adds a sanitized
  local detail line that never enters archives or reports. Classification
  treats parse errors as `InvalidData` and matches `timeout` as a word.
  readySelector, Node hook phases, selector waits, URL waits, and screenshot
  selectors now record their conditions.
- Resolved in source: frame, legacy-shortcut, hide/remove selector, and initial
  navigation timeout docs in [create-action](docs/create-action.md) and
  [create-scenario](docs/create-scenario.md) matched the runtime.
- Resolved in source: schema-valid but unusable actions (`input` value/file and
  `select` value/label exclusivity, case-sensitive `press` keys, `persist`
  names, `switchTab.url` without `new`), missing upload files, and Playwright
  prefixes in screenshot selectors now fail preflight before browser launch.
  JSON Schemas still do not encode these rules; the actions guide lists them.
- Verified with unit tests and a cached-Chromium probe of missing, hidden,
  covered, URL, readySelector, screenshot-selector, key, and deadline failures.
  Node hook summaries were covered by unit tests only, without a Node hook
  browser run. The hosted Worker needs regeneration and redeployment to serve
  these guide changes.

## Windows Installer Smoke Completion (2026-10-06)

- Resolved in source: the smoke helper waited only for Inno's first-phase
  uninstaller, checking PATH ownership while second-phase cleanup was still running.
  It now waits for the complete process tree with a bounded deadline and retains
  the uninstall log. Installer payloads and PATH cleanup logic were unchanged.
- Native synthetic setup/uninstall and process fixtures reproduce the boundary
  and verify completion, exit-code propagation, proxy/argument preservation and
  timeout child cleanup. The 42-test release suite passes; a hosted release rerun
  remains separate from local verification.

## Standalone MCP Knowledge (2026-10-06)

- Resolved in source: added source-independent troubleshooting and hook guides,
  focused diagnostic commands and artifact paths, complete hook exports/arguments,
  lifecycle ordering, and tested script examples with companion suite wiring.
- Exported schemas now reject invalid zero workspace timeouts and empty/blank
  capture selectors without narrowing valid viewport selections. Shared selector
  help is neutral; every emitted property requires direct or referenced help.
- Corrected managed CLI/desktop initialization descriptions and the obsolete
  prohibition on documenting consumer `AGENTS.md`. Legacy `keyPress` help now
  describes literal text append, not keyboard events; explicit actions suppress
  shortcuts. Tool, prompt, and managed-agent guidance route diagnosis to the MCP.
- Standalone export/typecheck and local MCP/script tests pass without source or
  browser access. Existing deployed services need regeneration and redeployment;
  no publication is implied by the source changes or local dry-run checks.

## Desktop Agent Setup (2026-10-06)

- Resolved in source: desktop folder opening now creates or refreshes
  `.vscode/mcp.json` and the managed `## Regressify CMP` section in `AGENTS.md`
  using the same preservation and freshness helpers as CLI init.
- The local `v0.79.0` tag only invoked MCP setup from explicit CLI init. Installed
  v0.79 desktop binaries need a new build/release for automatic setup.
- Initialization and session regressions cover missing/deleted files, MCP URL
  updates, agent-section append/refresh/no-op behavior, retained project content,
  concurrent creation, and preserving the current workspace on setup failure.

## Desktop External Reload (2026-10-06)

- Resolved: the open desktop document now detects external saves in a background
  check and automatically reloads clean content. Unsaved Source/Fields drafts and
  pending native input are preserved behind an explicit reload confirmation.
- Model and native-event regressions cover stale reads, Source saves with hidden
  Fields controls, exact text, same-size edits, atomic replacements, invalid
  syntax, deletion, binary files, recovery, cursor/focus, and conflict choices.
  The Windows schema smoke passes external reload and conflict cancellation/
  confirmation, with compact/large light/dark screenshots inspected and preferences
  restored.

## Action Viewport Selection (2026-10-05)

- Implemented: all 16 actions accept `viewportNames`, with strict validation
  against each consumer's effective viewports, shared execution/inventory gates,
  filtered checkpoints, and desktop authoring that preserves invalid drafts.
  Cached-Chromium regressions verify skipped side effects, real reference/test
  manifests, scoped placeholders, and preserved historical baselines.
- Resolved: action `viewportNames` now accepts a single string as well as an
  array, matching scenario syntax. YAML/JSON round trips, generated schema,
  scalar/list Source completion, Fields saves, strict validation, and the local
  responsive browser fixture cover both forms; blank strings remain invalid.
- Accepted limitation: named `persist` output is shared across concurrent jobs;
  narrowing its writers does not order another scenario's `state`/`restore` load
  or guarantee freshness. No cross-scenario writer/reader analysis is performed.
  See [action guidance](docs/create-action.md#viewport-selection). Revisit this
  boundary if real incidents require static validation or scheduling changes.
- Resolved during native verification: contextual action validation initially
  duplicated Source syntax errors. A model regression and the unchanged Windows
  schema smoke now pass; invalid draft preservation remains enforced. A final
  regression also preserves the combined 50-issue limit when an existing semantic
  issue precedes contextual viewport errors.

## Diagnostic Retention Follow-up (2026-10-02)

- Resolved: command failures were retained at informational severity; non-capture
  commands lacked operation journals and desktop saved-log selection. Startup,
  command, and capture/deployment completion records now preserve outcome severity.
- Resolved: optimizer results, approval decisions, and snapshot cleanup warnings
  were console-only. Typed records now preserve safe details, including worker-thread
  optimization results and Windows cleanup OS codes without changing command outcomes.
- Resolved: the desktop GUI did not install a subscriber or retain startup/native
  warnings. An entry-point-owned, nonblocking application journal now covers them.
- Resolved: out-of-range log timestamps could crash the frontend timeline. The
  schema rejects them and desktop/mobile tests verify a recoverable loading error.
- The [follow-up audit](docs/logging-audit.md#follow-up-review-2026-10-02) records the
  remaining print classifications and review boundaries. This change does not
  resolve the separately tracked worker-pipe timeout or Git discard findings.

## Frontend Formatting Baseline (2026-10-01)

- Resolved: `bun run --cwd frontend lint` now passes with zero warnings or
  errors across all 37 frontend source, script, and test files, alongside
  `bun run --cwd frontend typecheck`, `bun run --cwd frontend test`, and
  `bun run --cwd frontend build`.

## Worker Snapshot Inventory Assertion (2026-10-01)

- The worker integration test `filtered_snapshot_stages_only_selected_suites_and_their_public_dependencies`
  expects two staged images but observes five. Its count assertion does not account
  for the generated preview dependencies described below.
- The exact test fails identically with the new logging subscriber hooks removed
  from the worker. The other ten non-ignored worker integration cases passed.
  This is a pre-existing test gap, not a subscriber-induced asset change.
- Resolved by [logging packaging Task 027](plan/log-revamp-tasks/027-snapshot-log-packaging-inventory.md):
  the assertion now compares the exact referenced original/preview image set
  rather than assuming two files. A separate exact ZIP inventory test covers
  selected-only log chunks and excludes raw journals and unselected captures.

## Large-Suite Preview Performance (2026-10-01)

- Confirmed: a read-only browser probe of 44 full-page captures downloaded about
  104 MiB of PNGs during scrolling. Their combined uncompressed pixel size was
  about 1.9 GiB, not a measurement of resident GPU memory; frame stalls reached
  517 ms in that run. The JavaScript transfer was only about 180 KiB.
- Resolved in generation/frontend: bounded small previews replace original-image
  requests while browsing, and distant preview sources are released. Original
  comparison/download images remain unchanged. Existing sites need regeneration
  and redeployment; the live report was not modified.
- Preview generation uses fast fixed PNG encoding, no expensive optimization,
  and byte-for-byte repeatability tests. A 300-capture browser regression covers
  bounded resources, filtering, view changes, distant failure jumps, and review.
  Desktop tests cover exact staging inventories, version refresh, and cleanup.

## Deployment UI And Suite Links (2026-10-01)

- Resolved: deployment dialogs now use a left-aligned native link with adjacent
  Copy and omit suite listings while retaining subset confirmation. Deployment
  activity uses a single-line suite summary, omits capture statistics, and fills
  the remaining height with a selectable, read-only log editor.
- Runs adds Open Site beside Open Report, using persisted publication membership
  and completion order to find the latest site containing the selected suites.
  Subset redeployments replace membership; failures preserve previous links.
  Legacy target URLs gain suite membership on the next successful deployment.
- Focused model and GPUI checks cover reloads, target changes, private publication,
  safe deep links, full-height layouts, and exact copying. The deployment slice
  passes 103 local tests with six live-provider checks ignored; no site was published.

## Popup Captures (2026-09-30)

- Resolved: actions and captures previously stayed on the original scenario page.
  The CLI and desktop now support explicit `switchTab: new | previous | original`
  with an optional new-tab URL filter. A URL wait alone still does not switch tabs.
- Local Chromium regressions cover `_blank`/`noopener`, JavaScript and iframe
  popups, redirects, URL replacements, ambiguous/missing/closed tabs, first-load
  viewport/auth/storage, and checkpoint/final screenshot pixels. Native desktop
  coverage verifies adding the action from the navigation menu and undoing it.
- Default CLI, desktop-model, and GUI/worker gates pass, as do production GUI
  Clippy and the desktop build. Interactive schema smoke now passes after fixing
  its selected-line endpoint setup; see the source-line smoke note below.
- See [action syntax](docs/create-action.md) and [tab ownership](src/test/tabs.rs).

## Source-Line Smoke Selection (2026-09-30)

- The `Ln 3, Col 1` timeout was caused by assuming two `Shift+Down` presses after
  `Ctrl+Home` ended at column one. A keyboard-driven GUI regression reproduced
  selection `0..28` instead of `0..27`: the first character of the third line was
  selected. The earlier regression set the selection directly and missed this.
- Fixed the smoke setup by finishing with `Shift+Home` before checking the caret.
  Exact selected-range, status-label, and resulting-text assertions remain. This
  makes the line-command fixture deterministic; production caret code is unchanged.
- `./ci/test-desktop-smoke.ps1`, the focused keyboard GUI regression, and
  `./ci/desktop-smoke.ps1 -SchemaFlows` pass. Native coverage includes all line
  commands, undo/redo, menu editing, corrected saves, completion after Alt+Tab,
  and compact/large light/dark layouts. Cleanup restored desktop preferences.

## Windows Cloudflare Access Denied (2026-09-30)

- QA's final log identifies error 5 when removing a confirmed-empty generated
  `_snapshot/images/<suite>/diff` directory in a OneDrive workspace. Snapshot
  publication had completed; optional directory cleanup incorrectly aborted
  deployment before upload. No additional log is needed to identify this blocker.
- Fixed: failure to remove a confirmed-empty generated directory now warns and
  leaves it in place; an already-removed directory needs no warning. Directory
  reads, artifact writes, stale-file deletion, and symlink checks remain strict.
  Empty directories are excluded from the deployment inventory; captures and
  permissions are unchanged.
- Windows model and native-worker regressions block directory deletion with a
  sharing handle, verify successful snapshots and warnings, preserve captures and
  manifest paths, and confirm cleanup on a later unblocked run. Existing locked
  artifact tests still fail as required. The simulated OS error is 32, not the
  exact OneDrive/ACL error 5; both use the fixed removal-error branch.
- QA should retry deployment with this fix. The particular Windows component
  denying deletion is not established. No live deployment or change to user
  credentials/security settings was performed.

## Open Review Findings (2026-09-23)

The findings below were resolved on the dates shown. The original review followed
CLI, browser, desktop editor/worker, Git/deployment, and update/release boundaries;
it was risk-focused, not an exhaustive security certification.

### P1: Discard Does Not Detect Content-Only Changes After Review (Resolved 2026-10-15)

- Resolved in source: [Git status snapshots](src/desktop/git.rs) now record
  index mode/object identities from porcelain v2 records alongside bounded
  worktree SHA-256 content digests for changed and renamed paths.
- [Discard validation](src/desktop/git.rs) compares both status records and
  content/index identities for every selected path before restoring or deleting
  any file.
- Regression coverage in `selected_discard_rejects_same_status_worktree_untracked_and_staged_content_changes`
  verifies rejection and file preservation across tracked worktree edits,
  untracked edits, and same-status staged-content updates.

### P2: Desktop Worker Pipe Joins Escape Timeout and Cancellation (Resolved 2026-10-15)

- Resolved in source: [desktop worker execution](src/desktop/runner.rs) now
  streams stdin on a separate writer thread, keeps checking cancellation after
  the parent process exits until all pipe streams finish, and terminates the
  process group if a descendant retains stdout/stderr past `PIPE_DRAIN_TIMEOUT`.
- Regression coverage in `descendant_retaining_pipes_after_parent_exit_is_reaped_on_completion_and_cancellation`
  verifies bounded completion, immediate cancellation, and descendant termination
  when a parent exits while a child holds inherited pipes and a file lock.
- Git counterpart resolved on 2026-09-24: its process wrapper keeps checking the
  deadline/cancellation until readers and the stdin writer complete. A local
  parent-exits-first helper regression verifies bounded completion.

## Review Checks (2026-09-23)

- `cargo test --features desktop --lib`: 516 passed, 9 ignored, no failures.
- Ignored Chromium/Node fixtures, GUI interaction, installer execution, and live
  provider/update services were not run. Test success does not resolve the gaps above.

## Asset Version Resolution (2026-09-28)

- The recurring generated JavaScript version failure was Git CRLF conversion,
  not a Rust release-mode hash difference. Normalizing checkout bytes to LF
  reproduced the version embedded in the HTML.
- [.gitattributes](.gitattributes) now preserves all generated snapshot bytes.
  Regenerate existing converted checkouts with `bun run --cwd frontend build`.
  The existing Rust resource-version test passes in release mode; the
  [asset-sync regression](frontend/scripts/sync-assets.test.ts) covers LF and
  CRLF inputs with Git autocrlf enabled.

## Pixel Threshold Validation (2026-09-29)

- The default CLI all-targets gate passes with 376 tests and 9 optional checks
  ignored. The GUI all-targets gate passes with 863 library tests and 18 ignored,
  plus 9 worker integration tests and 2 ignored. Production GUI Clippy with
  warnings denied, the desktop build, Rust formatting, and guidance checks pass.
- Native `./ci/desktop-smoke.ps1 -SchemaFlows -SkipBuild` passed through
  Source/Fields diagnostics, issue selection/copy, compact light/dark layouts,
  and Enter/Tab/Escape completion after reactivation. It then stopped in the
  existing source-line command check because `Ln 3, Col 1` was not found.
  Root cause is not established; this is not a full schema-smoke pass. Cleanup
  discarded the fixture draft and restored original desktop preferences.
  The isolated native `pixel_match_threshold_controls` test passes.
- The source-line smoke blocker was subsequently reproduced and resolved on
  2026-09-30; see Source-Line Smoke Selection above for the passing native rerun.

## Validation Notes (2026-09-27)

- The side-by-side Git text comparison passes all 49 checks in
  `cargo test --locked --features gui-tests --lib git:: -- --test-threads=1 --quiet`,
  plus production GUI Clippy, the standalone desktop build, and the headless smoke
  harness. Native `./ci/desktop-smoke.ps1 -GitFlows` and a `-SkipBuild` retry
  completed the text-diff, exact-copy, malformed-JSON, and selected-comparison-commit
  checks. Both captured light/dark screenshots at 1600x1000 and 1080x720.
- Historical native Git smoke failed later in remote setup: the first rerun's
  `Remote URL` control did not contain the supplied local repository path; the
  retry's `Remote name` control did not contain `review`. The failure is reported
  by `Wait-SourceText` in [native controls](ci/desktop-smoke/controls.ps1).
  Root cause is not established; neither attempt is a full Git-flow pass.
  Cleanup restored the original desktop preferences after both attempts.
  The full native Git flow passed on 2026-10-17 after the exact-window focus and
  clipboard-ownership fixes, including all remote-field and conflict checks.
- `cargo test --locked --features gui-tests --all-targets -- --test-threads=1 --quiet`
  reaches 829 passing library tests and 15 ignored checks, including the corrected
  source-save regressions. The existing embedded frontend resource-version test
  still fails for `_snapshot/assets/app.js`; Cargo stops before later test targets.
  No frontend assets were changed during the save repair.

## Shared Deployment Optimization (2026-09-29)

- Azure and Cloudflare now share budget-aware PNG optimization in temporary
  copies: 250 MiB for Azure Free/unknown, 500 MiB for Standard, and 2 GiB for
  Cloudflare's selected assets. Sources stay unchanged, Cloudflare exclusions and
  per-file limits remain enforced, and snapshot cache versions follow optimized bytes.
- `cargo test --locked --features desktop --lib -- --quiet` passes with 661 tests
  and 17 optional checks ignored. Coverage includes optimized Cloudflare manifests,
  source preservation, limits, exclusions, cleanup, and snapshot cache versions.
  No live deployment was attempted.

## Cloudflare Validation (2026-09-28)

- The guarded temporary-Worker check passes automatic creation, production routing,
  changed-content and cached deployments, public bytes/MIME, and confirmed cleanup.
  The configured Worker was not changed. Native Cloudflare target editing passes
  production defaults, advanced settings, save/reopen, and credential preservation.
- The full headless desktop library run has 656 passing tests, 17 ignored checks,
  and the already tracked embedded frontend JavaScript version mismatch. No
  frontend assets were changed in the Cloudflare port.

## Validation Notes (2026-09-26)

- Deployment URL checks pass, but the full desktop model gate has 641 passing
  tests, 14 ignored checks, and one failure:
  `snapshot::site::tests::frontend_resource_versions_match_the_rust_asset_helper`
  reports a version mismatch for the embedded frontend JavaScript. The same
  test fails with desktop features disabled. No frontend assets were changed
  during the deployment URL fix.
- Non-publishing Azure, Netlify, and Cloudflare access checks pass. Native Windows
  smoke was not run because another desktop instance was open; isolated GPUI
  deployment interaction tests pass.

## Validation Notes (2026-09-25)

- The embedded frontend resource-version gap is resolved by rebuilding the assets.
  The full desktop model gate passes with 627 tests and 13 optional checks ignored.
- Full frontend lint rules and strict checks for changed files pass. The combined
  `bun run lint` command still flags existing CRLF formatting in untouched files;
  those files were not reformatted as part of desktop publishing changes.
- Live landing publication passes on Azure and Cloudflare. The authorized Netlify
  account rejects new deployments with a credit-usage limit (HTTP 403), so live
  Netlify landing publication remains unverified until its credits are restored.

## Resolved In September 2026 Hardening Pass

- Initial page navigation now defaults to 60000 ms, matching the legacy
  Node/VS Code runner. Omitted or zero navigation timeouts use this bounded
  default; positive overrides and the 30000 ms element-wait default are preserved.
  The regression failed with the previous 30-second fallback and passes with 60.
  CDP `Page.navigate` now receives the same budget. A loopback browser regression
  failed at its old internal 30-second cutoff, then passed with a 31-second
  document response while preserving explicit short navigation timeouts.
- Compare Results now reloads the completed request's run and output folder in
  Results instead of retaining an older pinned all-pass report. Native-event
  regressions cover run and saved-report selections, custom output folders,
  shared run names across suites, intermediate capture failures, command errors,
  and cancellation.
- Pixel-color tolerance is now configurable through `pixelMatchThreshold` at
  workspace, suite, and scenario levels, including desktop Fields. The range is
  0-100, the default is 5, and desktop steps are 0.1. Comparisons normalize by 100
  and retain captured/reviewed thresholds. Background-color, inheritance, bounds,
  regeneration, legacy approval, and native-editing regressions cover the change.
- Resetting the last scenario field no longer leaves orphaned YAML indentation
  that would move a following suite property into the scenario. The lossless
  semantic guard remains intact; document and native-event regressions cover
  the starter suite's indentationless list and inherited threshold reset.
- URL waits now read CDP frame metadata instead of evaluating `location.href` in
  a JavaScript context that a redirect can destroy. A deterministic Chromium
  regression reproduces the original missing-context error and passes after the
  fix. Local fixtures cover delayed cross-origin redirects, selector readiness,
  SPA query/fragment changes, bounded timeouts, and frame-scoped URL checks.
- Git review now uses a side-by-side text comparison instead of schema-dependent
  settings/scenario grouping. Invalid YAML/JSON and ordinary UTF-8 files retain
  line-numbered, aligned red/green changes with shared scrolling, change navigation,
  and exact-source copy controls. Model and native-event tests cover malformed
  drafts, comments, line endings, limits, binary fallback, and compact/wide layouts.
- Corrected YAML/JSON files now save even if their previously saved source has
  syntax errors. Optional report-label comparison no longer rejects the repair;
  strict draft validation and external-conflict checks remain. Model regressions
  cover suites, workspace configuration, viewports, and data, and a native-event
  test verifies that saving clears the suite list's invalid status.
- Report and landing deployment targets now retain their last successful URL
  across tab switches and app restarts, with open/copy controls per target.
  Model and native-event regressions cover provider identity, failed publication,
  safe URLs, destination edits, disk reload, and compact light/dark layouts.
- Desktop Workspace files now lists every regular file under `common`, including
  `_on-ready.js`, `_override.css`, and nested, extensionless, or binary assets.
  Suite-like helper names stay separate from runnable suites. UTF-8 text files up
  to 4 MiB are editable regardless of extension; JavaScript, CSS, and other text
  use Source without configuration parsing. Binary, non-UTF-8, unreadable, and
  oversized files remain visible but disabled. Model and native-event regressions
  cover discovery, raw Save/Save As, undo/redo, external conflicts, size limits,
  mouse/keyboard guards, and sidebar scrolling in both themes/window sizes.
- Duplicate scenario-ID errors now identify the suite, source file, and both
  conflicting scenario entries, including numbers, labels, and URLs with
  only passwords redacted. Parser and multi-suite regressions cover
  URL-derived IDs, label slugs, case-insensitive collisions, and valid reused URLs.
- Desktop deployment now supports Cloudflare Workers Static Assets with secure
  target credentials, incremental uploads, and verified publication. A live check
  confirmed report bytes, suite deep links, headers, missing-file 404s, and reuse
  on an unchanged redeploy. The client translates the Netlify index-page rewrite
  to Cloudflare's canonical root and rejects Azure access policies before API calls.
- Cloudflare deployment creates missing Workers and publishes to production by
  default. Saved targets also expose private/preserve routing, HTML behavior,
  compatibility date, and exclusions. Uploads honor single-asset/concurrency hints,
  invalidate MIME changes, bound raw memory, and distinguish rejected publications
  from uncertain outcomes without retrying publication.
- Historical: Azure release preparation waited for `vswhere` before selecting
  MSBuild to avoid a stale native exit code. The Azure publishing pipeline and
  its provider-specific checks were retired in the 2026-10-22 GitHub migration.
- Source now supports selected-line comments, indentation, duplication, movement,
  and deletion through focused shortcuts and an Edit menu. GUI regressions cover
  undo/redo, JSON comment rejection, read-only and busy guards, Unicode/CRLF, and
  reversed selections. Exclusive end-line indentation and Ctrl+Shift+Z routing
  no longer affect an unselected line or invoke the wrong document history.
- Test suite rows now expose Rename/Delete context actions and focus-scoped F2/Delete
  keys. Guarded operations update references, historical test runs, local report
  manifests, and desktop selections/activity across known output roots. Optional
  `displayName` is editable in Fields and Rename and labels desktop/web UIs without
  changing filenames or IDs. Model, native-event, and desktop/mobile browser
  regressions cover preservation, rollback, focus, escaping, and stable routes.
- Source suggestions now accept Tab as well as Enter, preserving normal indentation
  outside the menu. Window reactivation restores lost focus only to the same visible
  source document without displacing another control. GUI regressions cover acceptance,
  Escape, indentation, and restoration guards; native schema smoke verifies keyboard
  operation after Alt+Tab away and native reactivation without an editor click.
- Snapshot [Netlify headers](frontend/hosting/_headers) now include
  `Referrer-Policy: no-referrer` and `X-Content-Type-Options: nosniff`, matching
  [Azure](frontend/hosting/staticwebapp.config.json). Generated assets were rebuilt,
  and the [hosting regression](src/snapshot/site.rs) passes. Cache assertions check
  required directives independently of their order or additional shared-cache directives.
- Desktop suite-creation and sidebar-scroll test fixtures now account for the
  workspace files created automatically on connection. Suite creation verifies
  preservation of existing entries; scrolling covers seven, eight, nine, and
  twelve total files in light/dark compact/wide windows. Both regressions pass
  without changing runtime discovery or layout.
- Git diagnostics now redact full authorization diagnostics case-insensitively,
  including multiline values, before display or copying. Live output is bounded,
  buffers complete lines across read boundaries, and omits remaining output after
  sensitive/oversized diagnostics. Synthetic regression tests cover URL secrets,
  header casing, whitespace, chunk splitting, and output limits.
- Desktop now accepts blank and comment-only workspace YAML as defaults, matching
  the CLI without rewriting the source. Regressions cover first-field edits,
  resetting the last field, exact undo, and continued rejection of explicit nulls
  and invalid non-workspace documents.
- Desktop folder opening now creates missing runtime defaults automatically,
  including `regressify.yaml`, without generating example suites or external-editor
  assets. CLI `init` shares workspace-config creation. Focused regressions cover
  repeat/concurrent setup, alternate YAML formats, preservation, and failed opens.
- Results Fit area and Fit width now report the rendered zoom to one decimal
  place. GUI regressions cover image shapes, tabs, both window sizes, preview
  resizing, and manual zoom. Comparison fixtures now use the shared capture-owned
  error artwork accessor, resolving the stale includes that blocked GUI tests.
- URL-only scenarios now share friendly names across desktop catalogs, draft
  headings, Results, and snapshot navigation/capture headings. Complete-suite
  display names are recorded independently of IDs and filenames; older reports
  use the same URL fallback. Model and desktop/mobile browser regressions cover
  naming collisions, repeated viewports, and unchanged capture identity.
- Desktop lists now have focus-scoped keyboard selection and navigation. Suite
  and scenario Ctrl+A/ranges respect filters; run checklists and Git trees retain
  their selection boundaries. Viewport, capture, workspace-file, and action lists
  support keyboard navigation without intercepting text-editing shortcuts.
- New and Rename suite dialogs now focus and select the filename stem, accept
  Enter from inputs and focused Create/Save buttons, and include a close X.
  Keyboard Cancel/X remain non-destructive; GUI regressions cover dismissal,
  busy-state locks, invalid names, collision preservation, and retry.
- Snapshot overview, suite results, and image review now share a responsive React
  SPA with clean URLs, light/dark themes, and quick suite navigation. Generated
  JS, CSS, fonts, JSON, and every image use stable filenames with `?v=` versions.
  Snapshot generation preserves capture history; deployment packages only the
  active manifest and refreshes versions after compression without renaming files.
  Frontend, Rust, worker, and local browser tests cover it.
- Snapshot review now has independently scrolling panes, a resizable suite/scenario
  navigator scoped to the current suite, native page-only image scrolling, and a
  selectable Info popover. A searchable suite dropdown keeps the current suite
  visible and overlays scenarios instead of expanding the pane. Suite failure
  arrows jump between filtered failed cards; desktop/mobile browser checks cover
  navigation, keyboard selection, dismissal, resize, scrolling, and copying.
- Snapshot branding now shares the desktop icon for the logo/favicon. Suite and
  image browser titles include the suite name. Suite list rows show uncropped,
  side-by-side Reference/Test/optional Diff previews, with mobile, missing-image,
  and long-label browser coverage.
- Snapshot side-by-side review now synchronizes high-contrast pixel crosshairs
  across Reference/Test/Diff. Desktop/mobile browser checks cover unequal image
  sizes, zoom, missing images, pointer cleanup, and unchanged native scrolling.
- Snapshot themes and initial page backgrounds now apply before the app bundle
  loads, removing light/dark startup flashes. Preferences synchronize across tabs
  without startup writes; desktop/mobile regressions cover system changes, invalid
  or blocked storage, and preference resets.
- Snapshot footers now stay at the viewport bottom on short, empty, and missing
  pages and follow long content in normal flow. Desktop/mobile footer regressions
  cover document scrolling without overlap, including suite pages.
- Desktop Snapshot and Deploy no longer pass built-in report text as explicit
  CLI overrides. Workspace snapshot title, heading, and Markdown subheading now
  reach staged Netlify/Azure reports. Legacy activity's prefilled defaults migrate
  to inherited values; custom and newly saved explicit overrides are preserved.
- Desktop update checks now run in the background for every newly opened instance,
  even inside a recent check's eight-hour window, then follow the shared eight-hour
  schedule while windows stay open. One check/download owner per user cache
  prevents concurrent work; startup preserves pauses and checksum retry limits.
  Every instance verifies the shared installer before showing its banner.
  Model and GUI tests cover startup, scheduling, ownership, cached reuse, pause,
  and shutdown takeover.
- Deployment staging no longer uploads unused TTF fonts. Exact-ZIP regression
  coverage confirms unreferenced old/current-run captures, diffs, source maps,
  metadata, and unselected suites are excluded without modifying source files.
- Native smoke shortcuts use typed `SendInput` events with balanced modifiers and
  extended navigation flags. The complete default authoring and `-SchemaFlows`
  checks pass, including completion, clipboard operations, saves, and preference
  restoration; [headless checks](ci/test-desktop-smoke.ps1) cover key-event generation.
- Netlify uses asynchronous digest manifests and uploads only missing content, with four concurrent file requests and bounded per-file retries. The one-hour deadline and periodic stall warnings remain; deployment creation is never automatically retried. Live publication and unchanged-content reuse were verified with a synthetic report.
- Netlify site validation and status reads now retry interrupted or timed-out response bodies after successful headers instead of exiting with `Cannot read Netlify response`. Loopback tests verify recovery, six-attempt exhaustion, and redacted timeout/interruption diagnostics; malformed or oversized responses still fail immediately.
- Results approval confirms inline for the current scenario/viewport, refreshes suite failure counts from the recalculated report, and stays in Results; whole-run approval remains in Runs.
- Scenario `selectors` supports full-page defaults, `viewport`, and visible CSS selector clips with stable target-specific filenames and report ordering.
- Optional `save.selector` and scenario `selector` clip their own screenshots while preserving save and `FINAL` filenames. A last applicable scenario save skips the automatic final capture, but explicit `selectors` still run. Unit, desktop schema, and local Chromium tests cover independent selectors, viewport filtering, pixels, error placeholders, and scroll restoration.
- `asyncCompareLimit` is now a validated bounded worker limit for parallel pixel comparison.
- Report regeneration ignores generated diffs and clears stale diff artifacts when a pair becomes a pass.
- Screenshot output writes atomically, preserves color-management PNG chunks, and uses collision-resistant run IDs.
- Capture waits for an interactive DOM, lazy-loaded resources, decoded fonts/images, and stable layout. Configurable animation stabilization now runs before actions and at each checkpoint/final capture. `save` shares the final capture's resolved delay and bounded settling without replaying lazy-load scrolling. Local Chromium regressions cover saved pixels, delay inheritance/overrides, repeated checkpoints, invalid names, animation opt-outs, late-started animations, and preserved hover state.
- Text caret suppression no longer depends on `freezeAnimations`. Checkpoints and final captures hide blinking carets without changing focus, selection, or input values, including when settling is disabled. A local Chromium regression verifies identical screenshot pixels across a blink interval for inputs, textareas, and editable content with animation freezing on and off.
- Lazy-load scrolling now finishes before prerequisite, scenario, and legacy actions instead of disturbing their final hover state. It bypasses smooth scrolling and waits for stable offsets at the top before continuing, including scenarios without actions. Local Chromium regressions cover all action paths, `noScrollTop`, disabled capture settling, delayed/interrupted scrolling, and bounded failure when returning to the top is blocked.
- Interactive actions use full actionability validation, URL replacements apply to `goto`, and iframe waits/coordinates are hardened.
- URL waits support exact URLs, single-segment `*`, multi-segment `**`, and query-value wildcards.

## Resolved In June 2026 Fix Pass

- `press` now uses CDP `Input.dispatchKeyEvent` and supports modifier combinations like `Ctrl+A`, `Shift+Tab`, and `Meta+K`.
- Replacement profiles are loaded, validated, and applied during `test`; invalid regexes fail early as configuration errors.
- Resolved `hideSelectors`/`removeSelectors` are applied through the config cascade.
- `cookiePath` and persisted state paths are confined to the project/output directories; state respects `--site-env`.
- Unknown viewport names, duplicate viewport labels, empty labels, and zero dimensions fail before browser launch.
- Visual mismatches and website/action failures are report results, not process failures.
- `.tests.yml` and `.tests.json` files are parsed from their discovered paths instead of being incorrectly remapped to `.tests.yaml`.
- `needs` dependencies now fail early as configuration errors when missing, duplicated, cyclic, or deeper than 100 levels.
- Duplicate suite formats are deduplicated by suite name with `.tests.yaml` > `.tests.yml` > `.tests.json` precedence.
- `postInteractionWait` defaults to `1000` milliseconds through the workspace/suite/scenario cascade.
- `wait.url` uses active URL replacements in `test` mode; `ref` captures and waits against raw URLs.
- `restore`, `requiredLogin` / `--requiredLogin`, `init`, `install`, and `version` are implemented in the Rust CLI where they fit the Chromium/CDP design.
