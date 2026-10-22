---
title: Regressify CLI Commands
description: Command reference for the Rust Regressify CLI, including global flags, command behavior, output folders, and exit-code semantics.
guid: 35858e8f-70fd-4c4d-a3cb-a0ba3c23210f
date: 2026-06-11T00:00:00+00:00
updated: 2026-09-13T00:00:00+00:00
draft: false
hidden: false
weight: 15000
sort_by: weight
template: docs/page.html

extra:
  toc: true
  top: false
---

This page describes the Rust CLI commands implemented by this repository.

## Global flags

Global flags are placed before the command:

```bash wrap
regressify --root ./my-tests --site-env staging test --test-suite alloy
```

| Flag                          | Purpose                                                                                                                                                                                                                  |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `--root`, `-r`                | Project root. Defaults to the current directory.                                                                                                                                                                         |
| `--site-env`                  | Writes output to `.regressify__<env>` instead of `.regressify`. Persisted state is also scoped to that directory. The environment name must use only ASCII letters, digits, `_`, or `-`.                                 |
| `--run-id`                    | Portable explicit capture directory ID shared by all selected suites. `test` and `ref` write to it; `compare` and `approve` select it. Without an explicit ID, tracked suites use their latest completed full-suite run. |
| `--async-capture-limit`       | Positive global maximum scenario/viewport captures running at once. Defaults to `5`; suite/workspace `asyncCaptureLimit` can impose a lower per-suite cap.                                                               |
| `--async-capture-ramp-ms`     | Delay between adding concurrency slots. Defaults to `1000`.                                                                                                                                                              |
| `--debug`                     | Runs Chromium with a visible window. Suite-level `debug: true` also enables this. Automatically disabled in CI (no display).                                                                                             |
| `--requiredLogin`             | Forces `requiredLogin` true in the resolved scenario context exposed to custom `jsOnReadyPath` scripts.                                                                                                                  |
| `--proxy`                     | Proxy URL used by HTTP clients where supported.                                                                                                                                                                          |
| `--pause`, `--pause-if-error` | Wait for input after execution or after errors.                                                                                                                                                                          |

### Diagnostic Verbosity

`--log-level trace|debug|info|warn|error` controls diagnostic console display.
`--log-verbose` (or the existing `--verbose`) selects debug detail unless an
explicit `--log-level` is supplied. Display precedence is CLI flag, then
`REGRESSIFY_LOG_LEVEL`, then scenario `logLevel`, suite `logLevel`, workspace
`logLevel`, and finally `info`. Parallel captures keep independent display levels.
`--no-print` suppresses diagnostic console records and capture failure lines,
not command-result stdout.

Display filters never reduce the standard archived timeline. For example,
`regressify --log-level error test` still archives detailed standard debug events
for passed captures. Storage limits and the archive kill-switch below are separate
operation-level settings, not scenario configuration.

Failures retain `error` severity, cancellation and visual mismatches retain `warn`,
and successful completion retains `info`. Version output, interactive prompts,
human-readable result summaries, and emergency sink warnings remain console output;
they are not forwarded wholesale into archives.

Each failed capture prints two console lines:

```text
[ 3 of 12] FAILED: shop:checkout [Desktop]: Action 2 (click): no element matched after 30012 ms (Timeout)
[ 3 of 12]   Local detail (not saved in reports): Timed out after 30000ms waiting for element '#buy' to exist in the DOM
```

The text after the viewport is the failure summary, which is also the
error-marker message, report `engineErrorMsg`, and desktop progress error. It is
built only from typed diagnostic fields: the failed step, unmet condition,
duration, and error category; a deadline reads
`scenarioTimeout of 600000 ms expired during Action 4 (wait) (Timeout)`. The
local detail is the error message chain with URLs reduced to scheme and host,
credential-like values redacted, control characters removed, and length limited
to 2048 characters. It can still name selectors, files, and browser script
errors, so it appears only on the console and in the live desktop scenario log,
never in archives, reports, or published logs.

### Diagnostic Storage Limits

Startup/argument/configuration outcomes are recorded separately under the user's
local application data directory at `regressify/diagnostics/startup/_diagnostics`.
These safe operational journals are never included in public capture reports and
are not automatically pruned. Entry points explicitly finalize them before exit;
interrupted processes can leave a partial archive. The archive kill-switch below
also disables startup journal creation.

The desktop starts its application journal before argument parsing, installation
checks, worker dispatch, and window creation. Native shell/window warnings are
queued into that local journal, including warnings from background threads.
These records are categorized and do not retain arbitrary exception text.

Engine commands other than `version` also retain a separate command journal under
the active output root. It records command identity, completion, comparison
selection/outcomes, approval promotions/skips, optimization sizes/decisions, and
snapshot cleanup warnings with available numeric OS error codes. Desktop View Log
reopens the saved command operation ID; capture logs retain their original IDs.
Deployment staging remains in its deployment journal. Command and application
journals are local-only and never become public capture chunks. Failures before
engine configuration completes are recorded in the application journal instead.

`--diagnostic-chunk-size-bytes` controls journal segment size (default `524288`,
512 KiB; range 4096 bytes to 8 MiB). `--diagnostic-disk-budget-bytes` limits
one operation's journal (default `134217728`, 128 MiB; maximum 16 GiB). The budget
must cover at least one segment. Invalid settings fail configuration validation.

Each flag overrides its matching environment variable:
`REGRESSIFY_DIAGNOSTIC_CHUNK_SIZE_BYTES` or
`REGRESSIFY_DIAGNOSTIC_DISK_BUDGET_BYTES`. Settings are resolved once at process
start, independently of scenario/suite/workspace display verbosity. A storage
budget limits new writes; it never deletes old captures or operational history.

Journals use operation-owned `_diagnostics/<operation-id>` directories under the
active output root, with bounded JSON-line records and sequential segment files.
The writer's tested serialization/output-buffer bound is 192 KiB, excluding the
caller's record and filesystem buffers. This is an internal buffer measurement,
not a whole-process resident-memory claim. Public reports must only include their
explicitly referenced, sanitized capture assets, never a recursive journal copy.

Set `REGRESSIFY_DISABLE_DIAGNOSTIC_ARCHIVE=true` (or `1`) before starting the
process to disable durable archive creation as an operational fallback. `false`,
`0`, or an unset variable keeps it enabled. Restart to change this setting.
Disabling archives preserves bounded live activity and capture results, but
reports must show the expected log as unavailable, not as an empty complete log.

Each live journal has a 64-record nonblocking queue; serialized records must be
smaller than 64 KiB. Queue exhaustion, write failures, and a five-second writer
finalization deadline produce independent warnings and partial/unavailable logs,
not capture failures or changed exit codes. Successfully written records remain
readable. Known omitted-record counts are reported; interruption can leave that
count unknown. Only verified complete logs are eligible for complete-log exports.
The deadline stops waiting for a stalled writer; it cannot interrupt an OS file
operation already in progress. Such writes remain owned by the worker process.

Each finalized capture execution writes `.log.json` links beside its selected
images. A `.diagnostics.json` run marker distinguishes an expected-but-missing
link from a legacy capture. Comparison pairs preserve separate reference/test
log links and a comparison outcome; a later visual mismatch does not rewrite
the successful capture's original execution outcome. Missing or corrupt log
metadata is diagnostic availability information, not an image comparison error.

`ref` and `approve` carry the selected image's original log link into the baseline;
they never create a replacement execution history. Log sidecars bind to the image's
SHA-256 digest, so an old link cannot silently describe different image bytes.
Suite rename keeps immutable operation IDs, and suite deletion prunes only
unreferenced generated log copies, preserving source journals and active writes.

Cancelling a deployment records local cancellation with remote state unknown
unless publication was already confirmed. Stopping the local worker cannot undo
an upload accepted by the provider. Check the provider's deployment history before
retrying. The desktop uses the saved operation ID to mark an interrupted journal
after reaping its worker, without attributing it to another concurrent run.

The supported archive is Regressify's own execution timeline, not browser console
or network-body collection. Capture messages use safe templates and typed values;
suite/scenario IDs, viewport names, selectors, source URLs, artifact paths, and
capture names are readable and searchable. Quoted selector text and ordinary URL
query parameters are test-source metadata, not simulated end-user input.
Free-text payloads such as `input.value`, uploaded file contents, cookies, state,
and script bodies are not recorded. URL credentials and credential-bearing
parameters are redacted; control characters are neutralized and each source
identity is limited to 2048 UTF-8 bytes. Review source metadata before publishing
a report. Provider detail stays in local deployment history.

Older captures can retain `sha256:` source identities. They remain supported,
but neither opening their logs nor regenerating a snapshot reverses those hashes;
new captures are needed for readable source metadata. Image, archive, and asset
integrity hashes remain unchanged. See [desktop Capture Log](desktop.md#reviewing-results),
[web log contracts](../frontend/README.md#data-and-hosting),
[verification](logging-verification.md), and [performance evidence](logging-benchmark.md).

## Commands

| Command    | Purpose                                                                                                                                                                                                        |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `init`     | Create missing starter files, schemas, and VS Code associations; add or refresh the Regressify MCP endpoint while preserving other settings. Copy missing legacy `data/` files without deleting their sources. |
| `install`  | Fetch managed Chromium into the per-user cache shared by all CLI and Desktop workspaces.                                                                                                                       |
| `mcp`      | Generate a standalone Cloudflare MCP Worker containing versioned public authoring guidance, schemas, and examples. Does not deploy or export workspace data.                                                   |
| `version`  | Print the CLI package version. `regressify --version` is also supported by Clap.                                                                                                                               |
| `ref`      | Capture baseline screenshots, promote successful captures to references, and record them as self-approved in the report. Failed page captures are skipped and existing references are preserved.               |
| `test`     | Capture screenshots and stage comparisons unless `--skip-compare` is set. Finalize each suite independently after its jobs finish; generate the snapshot after compared tests complete.                        |
| `compare`  | Compare the latest test run for each matching suite against reference screenshots.                                                                                                                             |
| `approve`  | Promote latest successful test screenshots to references. Failed capture placeholders are skipped.                                                                                                             |
| `snapshot` | Generate the static snapshot SPA from existing comparison data.                                                                                                                                                |
| `optimize` | Optimize PNG files in the project output tree.                                                                                                                                                                 |
| `clean`    | Preview old test runs and their artifacts for selected suites; add `--yes` to delete them.                                                                                                                     |

All suite filters accept exact names or glob patterns. Use `all`, `*`, or omit `--test-suite` to process every discovered suite, except `clean`, which requires an explicit filter. Invalid glob patterns, unreadable suite directories, and an explicit filter that matches no suite fail the command instead of silently reducing coverage.

`snapshot` also accepts repeated suite filters, matched against output IDs:

```bash
regressify snapshot --test-suite 'shop*' --test-suite account
```

The summary contains only the selected suites. Excluded suite reports and
capture directories are not rewritten or cleaned up.

## Run History and Cleanup

Each suite keeps a local `runs.json` registry in its output directory. A run becomes
completed when all requested captures and, when enabled, comparisons finish.
Visual mismatches and website/action failures still count as completed;
cancellation and infrastructure failures do not. Suites finish independently:
stopping a command during one suite does not discard another suite's completed run.

Default Compare, Approve, desktop Results, Snapshot, and Deploy use the latest
completed full-suite run. Scenario-only runs remain explicitly selectable but
do not replace that default. With `--skip-compare`, comparison and snapshot work
stay deferred until results or a report are requested. Missing comparisons are
then generated from completed captures without launching a browser. Existing
untracked runs are trusted as legacy completed runs when the registry is created.
Older snapshots without a registry retain their existing report selection.

Preview cleanup for one or more output suite IDs:

```bash
regressify clean --test-suite shop --test-suite account
regressify clean --test-suite 'shop*' --keep-runs 3
regressify clean --test-suite all --keep-runs 1 --yes
```

`--keep-runs` defaults to 1 and must be at least 1. Cleanup first keeps the latest
completed full-suite run, then fills remaining slots with the newest completed
runs, including partial selections. Interrupted and failed runs consume no slots.
The limit is strict: keeping 1 can remove a newer scenario-only run. If a suite
has no full-suite run, its newest completed partial runs can fill the slots.
Preview lists retained runs, deletion counts, and estimated bytes; without
`--yes`, no run artifacts are removed. Global environment/custom-output flags
scope cleanup to that one output folder.

Cleanup removes selected run captures, reports, matching legacy raw captures,
abandoned comparison staging, and unreferenced capture-owned diagnostic journals.
It preserves reference baselines, browser state, source files, unselected suites,
and unrelated operational logs. Logs still linked by retained captures or
baselines remain protected and are tracked for a later cleanup. Unreadable log
ownership produces a warning and preserves those logs. Active artifact owners
block cleanup; changed previews must be reviewed again. Filesystem failures are
reported, and deletion already performed is not rolled back.

Automatic retention is opt-in. Configure workspace defaults in `regressify.yaml`
and override either property in a suite:

```yaml
autoCleanRuns: true
keepTestRuns: 3
```

The defaults are `false` and `1`. A suite's `autoCleanRuns: false` disables an
enabled workspace policy. Automatic cleanup runs only after a completed
full-suite test. Cleanup failures warn without changing completed test results.

Deployment and snapshot staging use private `.staging/<operation-id>` containers
under the selected output folder. Deployment attempts best-effort cleanup at
completion and retries abandoned containers before the next deployment. Active
containers are locked; ownership survives a failed payload deletion. Staging,
registries, locks, and retry metadata never enter published report packages.
The PNG optimizer skips both `_snapshot` and `.staging`.

## Workspace Initialization

`regressify init` creates a minimal `regressify.yaml` (`{}`) alongside the common
runtime files, example suite, generated schemas, and VS Code associations. The
empty configuration inherits built-in defaults and can be edited by both the CLI
workflow and the desktop app. Existing `regressify.yaml` or `regressify.yml` files
are preserved; an existing `.yml` is not shadowed by a generated `.yaml`.
Default replacement-profile generation also respects an existing `.yml` file.

Both CLI `init` and desktop folder-open setup create or merge `.vscode/mcp.json`
with the hosted Regressify authoring server:

```json
{
  "servers": {
    "regressify-mcp": {
      "type": "http",
      "url": "https://regressify.eshn.dev/mcp"
    }
  }
}
```

If `regressify-mcp` already exists, only its `url` is added or refreshed to the
bundled endpoint. Other server settings, including headers, other servers,
top-level settings such as `inputs`, comments, and trailing commas are preserved.
A matching URL leaves the file unchanged. The endpoint is defined by
`REGRESSIFY_MCP_URL` in [initialization code](../src/init.rs), so rerunning `init`
or reopening the workspace with an updated desktop build refreshes the endpoint;
the retired `https://regressify-mcp.tuyen-at-work.workers.dev/mcp` URL is migrated
this way. The hosted endpoint is deployed with the website after each tagged
release (see [Release Pipeline](desktop.md#release-pipeline)).

Malformed JSON-with-comments, non-object configuration/server entries, and files
that cannot be read or updated fail initialization without replacing the MCP
file. A read-only file is accepted when its endpoint is already current.

Both entry points also maintain a dedicated `## Regressify CMP` section in the
workspace root's `AGENTS.md`. They create the file when missing, append the section
when absent, or replace outdated section content. The instructions direct agents to
use `regressify-mcp`, starting with `get_authoring_guide` and consulting
`get_schema` and `get_example` before authoring tests or configuration.

The generated instructions and MCP overview also require agents to read relevant
notes in the consumer project's `.ai/memory/` before authoring or troubleshooting,
then record reusable lessons from corrections, failed approaches, and work that
took time to get right. Each topic file covers one specific behavior or issue on
one site and is at most 60 lines; split larger or broader notes and keep
`.ai/memory/README.md` a compact linked index. Notes include verified fixes,
scope, verification dates, and project-relative evidence links. Update existing
notes and remove stale advice; never store secrets, browser state, personal data,
raw logs, or transcripts. These are local client edits, not MCP uploads.
Initialization does not create memory files or rewrite existing notes.

The managed section includes everything through the next top-level H2 heading,
or the end of the file. Other heading levels do not end that range. Headings in
code examples, quotes, lists, or HTML comments are not section markers.
`pulldown-cmark` compares parsed Markdown content for freshness, so equivalent
heading syntax, list markers, and line endings do not trigger a rewrite. Endpoint
or instruction changes do. Duplicate managed sections are consolidated into one.

Text outside the managed sections is preserved byte-for-byte, including existing
line endings. Current instructions are not rewritten, even in a read-only file.
Unreadable or non-UTF-8 files, read-only files needing changes, and unfinished
Markdown blocks that prevent appending a real H2 fail without replacing the file.

The [desktop app](desktop.md#workspace-and-files) creates missing runtime defaults
and performs the same MCP and agent-guidance maintenance automatically when
opening a folder. It does not automatically generate example suites, exported
schemas, VS Code settings or extension recommendations, or Node-specific helpers.
CLI `init` retains those editor assets and non-destructive legacy-file migration.

## MCP Authoring Worker

```sh
regressify mcp --output ./regressify-mcp-worker
cd regressify-mcp-worker
npm ci
npm run check
npm test
npm run build
```

`--output` is required. It names a new directory whose parent already exists;
relative paths resolve from the CLI root. Absolute destinations outside the
workspace are allowed. Existing directories, files, and symlink destinations are
never replaced, and there is no `--force` option. Individual files are published
without clobbering concurrent edits. A write failure is a command error and may
leave an incomplete export; retry into a new directory rather than merging it.

Generation works with the ordinary installed CLI, without desktop/GUI features
or a source checkout. It uses embedded templates and schemas derived from the
CLI's Rust types. It does not inspect or export the current workspace's suites,
cookies, credentials, scripts, reports, or saved state. No browser, Node runtime,
network connection, or Cloudflare credential is needed to generate the package.
Normal CLI startup/command diagnostics still follow the diagnostic settings.

The generated project contains a Worker entry point, Wrangler configuration,
package and lockfile, TypeScript configuration, tests, README, and a versioned
content catalog with standalone guides, JSON Schemas, and synthetic examples.
Guides cover suite discovery/identity, the scenario capture lifecycle and
screenshot naming, capture targets for opened menus and overlays, every action
and selector engine, configuration scopes, precedence, and defaults, predefined
file formats, injected browser script contracts, complete recipes for common page
patterns, and troubleshooting. Hook examples
include complete suite wiring and plain DOM scripts that run in Chromium before
actions, without Node or Playwright. Diagnostic guidance
covers focused reruns, preflight messages, actual report paths and fields,
capture failure summaries and local detail lines, capture-error/log sidecars,
journal records, journal health, and common corrective checks. Every YAML block
in the guides is parsed and schema-checked by the catalog tests. Cookie/CSS/JS
formats use explicit guidance; JSON Schemas do not replace runtime cross-file,
filesystem, authentication, or browser validation.

Use Node.js 22.12 or newer with npm to install and validate the result. `npm test`
starts and stops a local Worker and exercises tools, resources, prompts, current
stateless requests, and legacy-compatible clients. It also executes the exported
hook examples with stub browser globals; this does not launch Chromium. `npm run build` is a Wrangler
dry run, not a deployment; it writes the single minified module that `npm run deploy`
uploads, without source maps. `npm run dev` starts the local `/mcp` endpoint.

Review the Worker name in the generated Wrangler configuration to avoid replacing
an unrelated deployment. Authenticate Wrangler locally or through your own CI
secret setup, then deploy separately:

```sh
npm run deploy
```

Connect an HTTP MCP client to the resulting
`https://<worker>.<account-subdomain>.workers.dev/mcp` URL. The generated README
includes a VS Code connection example. Start by calling `get_authoring_guide`
with `topic: overview`; use `get_schema` for exact fields and `get_example` for
synthetic templates. Select `topic: recipes` for complete common-page suites,
`topic: troubleshooting` for failures and local evidence,
or `topic: hooks` before writing lifecycle scripts. Schema tests enforce positive
workspace timeouts and nonempty capture selectors while allowing empty viewport
selections and the documented zero-timeout fallbacks. Guides/schemas are also MCP resources, and the
`author_regressify` prompt loads the workflow and a chosen topic. Clients may
require explicit selection of resources or prompts rather than loading them
automatically.

For related component states with different visible areas, such as a header
before and after opening a menu, use the same larger capture area in both
scenarios. The MCP overview, scenarios guide, recipes, and navigation example
recommend viewport captures, or full page when needed, with matching viewport
sizes and scroll positions. A header-only default capture could hide a menu
incorrectly open before interaction. Capture the untouched default state; do
not force the menu closed or hide it first. Element details are optional
supplements. QA can inspect states in context, while automated comparison still
matches each capture to its own reference.

After every suite creation or edit, the MCP instructions require agents to run
`regressify ref --test-suite <suite-name>` for each changed suite from the consumer
workspace using authorized local terminal tools. The CLI and desktop app are
installed together on each user's `PATH`; no source checkout or build is needed.
Verification covers all intended screenshots across the suite's configured
scenarios and viewports using the fresh run's results and PNGs, not just exit code
zero or old references. Agents fix capture failures and rerun before declaring
the suite verified. `ref` updates successful references, so target-page and
baseline-write authorization remain required. Missing permissions or execution
prerequisites must be reported as incomplete verification, not silently skipped.

In the generated Worker, `npm run verify` performs `npm audit --audit-level=high`,
`npm run check`, `npm test`, and `npm run build` in order without publishing.
High/critical advisories (including development dependencies), audit lookup
failures, type errors, failed tests, or a failed dry-run build block publication.
`npm run deploy` always runs this verification chain before live Wrangler
deployment. Calling `wrangler deploy` directly bypasses these package gates.
Review dependency fixes rather than automatically forcing upgrades or ignoring
advisories.

For maintainers in the source repository, `bun run deploy:check` runs Rust
formatting checks, the MCP tests, and a locked default CLI build. Root
`bun run deploy` runs those checks before replacing `../regressify-mcp-worker`,
exports with `cargo run --locked --bin regressify -- mcp`, installs dependencies
with the frozen lockfile, and calls the generated Worker's gated deploy.
It no longer pulls Git changes or uses a potentially stale installed CLI.
This shortcut requires Bun, Node/npm, Cargo, Tuyen for its existing directory
cleanup, and Cloudflare authorization; the sibling generated directory must
not contain manual edits. Running source checks alone does not authorize
publication or verify Cloudflare account permissions.

Startup instructions, the overview guide/resource, and the authoring prompt carry
the same [small site-memory policy](#workspace-initialization) as generated agent
guidance. The agent maintains notes locally with its authorized tools; the Worker
does not read, collect, or store consumer memory.

The Worker is public and unauthenticated by default. It provides only bounded,
read-only knowledge lookups; it cannot access a user's workspace, execute tests,
or approve baselines. It needs no database, Durable Objects, or LLM API key. Do
not publish private content in it. Use MCP-compatible authorization before adding
private material, and set appropriate Cloudflare rate limits for public traffic.
Origin/CORS validation is not authentication. The AI edits the user's files only
through its own authorized client tools.

The MCP identity reports the generating CLI version. Regenerate into a new
directory after upgrading Regressify, review differences, revalidate, and redeploy.
Do not mix guides and schemas from different CLI versions.

## Snapshot Site

The generated site provides an overview at `/`, suite results at
`/suites/<suite-id>`, and individual comparisons at
`/suites/<suite-id>/images/<capture-id>`. Navigation stays in the same application;
suite and image pages have a resizable quick-switch pane showing the current suite
in a searchable dropdown, with only that suite's Test scenarios below it. Opening
the dropdown overlays the pane without moving the scenarios; the current suite is
highlighted and checked. The navigation and right review area scroll independently;
phones use a drawer with the same selector and scenario list.
Search, result filters, grid/list selection, and image mode are reflected in the URL.
Suite list view shows Reference and Test previews side by side, plus Diff when
available. Images fit without cropping, and missing images keep their labeled lanes.
Grid view keeps a single preview per capture. The site logo and favicon use the
same artwork as the desktop app.
The overview uses a compact, centered layout. Engine-error counts appear beside
suite names, with their full labels available on hover, keyboard focus, or tap.
On phones, suite metrics stack into labeled rows instead of requiring horizontal
scrolling; the Sort by selector and direction button preserve every sort option.
Suite browsing uses generated PNG previews instead of downloading full-resolution
captures: list previews fit within 640 x 480 pixels, and top-cropped grid thumbnails
within 640 x 400. Only previews near the review viewport keep active image sources.
Generation prioritizes speed, uses fixed PNG encoding settings, and produces
byte-identical previews from identical input. Full-resolution captures remain
unchanged for comparison and download. Regenerate and redeploy existing reports
to get these previews; no new capture run is required.
The image viewer supports reference, test, diff, side-by-side, wipe, overlay,
stepped zoom through 800%, fit-width, fit-to-window, actual size, downloads, and
safe links to captured pages. The name, navigation buttons, and viewer controls
stay outside image overflow. Each image has a bounded viewport with native
scrollbars when needed. Unmodified wheel/trackpad and touch scroll the hovered
lane; left-drag and arrow keys pan the active lane. In side-by-side mode, scrolling
either axis in any lane automatically scrolls the other lanes to the same offset.
Each lane uses the same canvas: the maximum image width and maximum image height,
at the current zoom. For example, images sized 2000x1000, 3000x500, and 2500x800
all use a 3000x1000 canvas at 100%. Images retain their own dimensions and share
the canvas's top-left origin. Native scrollbars, wheel/touch, drag, and arrow-key
movement all stay linked, including through blank space beyond smaller images.
Ctrl/Cmd+wheel zooms around the pointer.
The pixel under the pointer stays fixed even when the image fits inside the
viewport. Modifier-wheel gestures over empty viewer space or at the zoom limit
remain image gestures and do not zoom the browser page.
Double-click toggles actual size and fit-to-window. With the image surface focused,
`+`/`-` zoom, `0` fits the window, `1` selects actual size, `W` fits width, and arrow
keys pan (Shift moves farther). Escape cancels a drag. Mode changes retain their
own zoom; capture changes reset it. Fit modes never upscale small images.
Side-by-side divides the visible viewer width into two equal lanes, or three when
a Diff image exists. Lanes never grow with zoom: oversized images scroll inside
their lanes, while smaller shared canvases start horizontally centered and top-aligned. When every image is
narrower than its lane, left-drag moves all canvases horizontally together,
stopping when the widest image touches either lane edge. This does not change
zoom or add horizontal overflow. When every image is shorter than its lane,
dragging moves the canvases vertically together until the tallest image touches
the top or bottom edge. Both axes can move together without adding scroll overflow;
fit controls restore horizontally centered, top-aligned placement.
Reference and Test remain separate even
at small zoom levels; Diff adds a third lane when available. Hovering anywhere in
a lane with a loaded image, including empty space around it, shows synchronized
crosshairs at matching image coordinates in the other lanes, whether or not a
diff exists. Guides span the entire lane, extend beyond image edges, and stay
under the pointer during scrolling, zoom, and fit changes. High-contrast strokes
stay thin at every zoom level. Leaving the lane, touching, dragging, or switching
views clears them; failed or missing images remain excluded. When comparison
aligns an inserted or deleted vertical band, crosshairs follow the corresponding
content above and below it. Within the band, the shorter image has no matching
pixel and shows no crosshair. Alignment comes from the saved comparison, not an
estimate based on image height or colors. Rerun Compare on an existing run, then
regenerate and redeploy its snapshot to add alignment to older reports; no new
capture is needed. Wipe follows mouse
movement while no button is held; its slider and the overlay-opacity slider also
support keyboard and touch input. Hover, focus, or pin
the Info icon for selectable/copyable diff statistics, capture details, URLs, and
comparison or engine errors. Previous/next failure arrows on suite pages scroll to visible failed
cards while keeping the current filters. The header's Settings icon opens a compact
popup with an Appearance section. Its theme control supports light, dark, or system
appearance and is remembered in the browser.

The Errors filter and engine-error counts include only failures that prevented a
requested reference or test screenshot, such as failed actions, navigation, or
timeouts. Pixel and dimension mismatches remain Failed comparisons, including
dimension changes with zero pixel mismatch. Missing references and comparison
processing failures also remain Failed, with their diagnostics labeled
Comparison failed. A capture-error placeholder is not a successful screenshot.
Regenerate existing snapshots with `regressify snapshot` and redeploy hosted copies
to update the UI and overview totals; no new capture run is required.

Snapshot generation preserves capture runs, baseline images, `config.js`, and
`report.json`. It publishes copies of referenced images and JSON under `_snapshot`,
with stable filenames and `?v=...` on every script, stylesheet, font, manifest,
suite JSON, and reference/test/diff image URL. Versions use `get_asset_version`
(SHA-384, base64url, 20 characters), never a hash in the filename. A full report
is validated in staging before publication; HTML is replaced last. Obsolete
generated files are removed, and desktop deployment stages only the active
manifest's dependencies. Legacy entry pages are removed. Comparison data remains
compatible with the desktop app.

Snapshot needs write and cleanup access to the selected report output directory,
not just read access to its captures. Temporary `.snapshot-*` directories are
created inside that output directory. Filesystem errors identify the operation
and affected path, including staging, existing-file reads, publication, and
obsolete-file cleanup. Failure to remove a confirmed-empty generated directory
only produces a warning: the directory stays out of deployment assets and cleanup
is retried on the next snapshot. Directory reads and obsolete-file deletion must
still succeed. On Windows, error 5 means access denied; error 32 indicates
a file-sharing conflict. Check the named path and OS protection history without
deleting captures or disabling protection. See [deployment troubleshooting](desktop.md#deploy-reports).

Scenario names follow the desktop's [URL-based naming rules](create-scenario.md)
when labels are omitted. Report headings and navigation use readable names;
capture IDs, image filenames, and image routes remain unchanged. New captures
record names using the complete suite and original URLs, so partial comparisons
and staging replacements do not rename them. Regenerate an existing snapshot to
apply the shared fallback to its older comparison data; no recapture is needed.

`optimize` skips `_snapshot` so already-published image versions stay valid. Run it
before `snapshot`, or regenerate the snapshot after optimizing source captures.

Host at the origin root over HTTP(S), not by opening `index.html` as a local file.
Azure `staticwebapp.config.json` and Netlify `_headers`/`_redirects` are generated.
The [desktop Cloudflare deployer](desktop.md#deploy-reports) passes the header rules
and suite rewrites as Workers asset metadata, using `/` as the canonical rewrite
destination rather than `/index.html`.
Other static hosts must serve `/index.html` for `/suites/*`, return real 404s for
missing `/_snapshot/*`, revalidate HTML, and cache versioned assets immutably.
CDN cache keys must include `v`; do not cache HTML indefinitely. Publish the whole
site atomically and reload an old report after deployment because origin files
contain the latest version, not historical copies.
See the [frontend guide](../frontend/README.md) for local preview and build commands.

## Snapshot Report Text

Set report defaults in `regressify.yaml` (or `regressify.yml`) at the project root:

```yaml
snapshotTitle: "Shop | Regressify"
snapshotHeading: "Shop Visual Tests"
snapshotSubheading: "**Staging** regression results"
```

Each field resolves independently: an explicit `snapshot --title`, `--heading`,
or `--subheading` overrides its matching workspace setting. If neither is set,
the defaults remain `Visual Tests Summary | Regressify`, `Visual Tests Summary`,
and `Snapshot regression test results`, respectively.

`snapshotTitle` controls the overview's HTML page title; suite and image pages
prepend the suite name as `suite name | snapshotTitle`. `snapshotHeading` is the
report heading in the compact top bar on every page, with `snapshotSubheading`
on its own line below it. The overview's page heading is `Snapshot Overview`, with
the generated date and time beneath it and no breadcrumb row.
The title and heading are plain text. The subheading supports safe Markdown text,
emphasis, lists, code, and HTTP(S) links; raw HTML and embedded images are omitted.
An empty string is an explicit value, while `null` falls back to the built-in default.

Workspace settings also apply to reports generated automatically by `test` and
`ref`. Override just one setting for a standalone report without changing the
others:

```bash
regressify snapshot --heading "Release Candidate Results"
```

## Individual Scenarios

`test` and `ref` accept repeated `--scenario <suite>:<scenario-id>` flags:

```bash
regressify test --scenario shop:checkout
regressify ref --scenario shop:checkout --scenario account:sign-in
```

The suite name is its logical name under `visual_tests`, without the file suffix.
Scenario IDs match exactly, not as globs or label substrings. An omitted `id`
uses the same label-derived slug or URL-derived ID as a normal suite run. When
`--test-suite` is also supplied, each scenario must belong to a matching suite.
Unknown or ambiguous selections fail before browser launch or capture writes.
No scenario flags retains whole-suite behavior.

Only selected scenarios produce capture jobs and report entries, across their
configured viewports and saved steps. Their `needs` actions still execute using
the full suite definitions; dependencies do not become additional independent
captures. Reference capture preserves unselected baselines.

`compare` and `approve` also accept `--scenario`, using the suite's **output ID**
instead of its file name when they differ. Use `--run-id` to target an exact run:

```bash
regressify --run-id reviewed-run compare --scenario shop-output:checkout
regressify --run-id reviewed-run approve --scenario shop-output:checkout
```

These commands match recorded scenario IDs in the capture metadata, not screenshot
filename prefixes. All requested output selections are validated before changing
reports or references. Missing scenarios or legacy runs without scenario metadata
fail with a recovery message; whole-suite operations remain compatible with older
runs. Scoped comparison regenerates the report for the selected captures, while
scoped approval leaves other reference images untouched and skips failed captures.

Desktop Results also offers **Approve This Scenario** for the displayed viewport
only. It promotes that scenario/viewport's successful captures and recalculates the
reviewed report without changing other baselines. CLI `approve --scenario` and
Approve Results in Runs still include every viewport of the selected scenarios.

## Exit codes

Regressify separates tool failures from website results.

The command exits non-zero for configuration or infrastructure problems: malformed YAML, invalid viewport names, invalid URL replacement regexes, unsupported browser settings, browser download failures, browser launch failures, and report/artifact write failures.

The command exits zero for website and visual results: navigation timeouts, missing or obscured elements, failed actions, failed scenario captures, missing references, and visual mismatches. Those are written into the comparison and snapshot reports so all scenarios can be reviewed together.

## Browser support

The Rust CLI currently drives Chromium through the Chrome DevTools Protocol. `browser: chromium` is supported. `firefox` and `webkit` are rejected as configuration errors until browser backends are implemented.

Both `ref` and `test` always use managed Chromium from a per-user cache shared by
all workspaces and by CLI and Desktop runs. The Chromium revision is fixed by the
fetcher shipped with Regressify, so
use the same Regressify version across QA machines. A missing browser is downloaded
automatically; an existing cached build is reused. Installed Chrome and Edge are
never auto-detected, and a missing or invalid executable is an infrastructure
error, not a reason to switch browsers.

The shared cache locations are:

- Windows: `%LOCALAPPDATA%\regressify\chromium`
- macOS: `~/Library/Caches/regressify/chromium`
- Linux: `$XDG_CACHE_HOME/regressify/chromium`, or `~/.cache/regressify/chromium`

Builds are stored by platform and revision. An OS file lock serializes installation
across processes, so concurrent workspace runs do not download or extract the same
build simultaneously. Only browser binaries are shared; each scenario still uses
its own temporary browser profile. Old workspace `download/` folders are no longer
used and are not deleted automatically.

The Windows Desktop installer also prepares this shared cache before launching
the app, downloading only when the pinned build is missing. Its browser step is
best-effort: offline/download failures are logged without failing app installation,
and Reference/Test retains its automatic installation fallback. Setup's
`/SKIPCHROMIUM=1` option defers this step. CLI `install`, `ref`, and `test` still
report browser preparation failures as non-zero infrastructure errors.

### Chromium installation diagnostics

Reference, Test, and Install report cache checks, lock waits, connection setup,
download progress, extraction, and readiness. Status updates appear every five
seconds while connecting, downloading, extracting, or waiting for the cache lock.
Download messages include received/total MiB (when the server supplies a size),
percentage, average speed, elapsed time, and how long no new data has arrived.
The log also records the download URL, staging/cache paths, limits, and final
executable path. Desktop shows the current phase in Activity; View Log contains
the full installation messages before any scenarios start.

Browser downloads honor `--proxy` (Desktop's Proxy run setting), or the HTTP
client's default proxy configuration when no override is supplied. Proxy
credentials and server response bodies are not printed.

Network waits are bounded: 15 seconds to connect, 30 seconds for response headers,
60 seconds without download data, and 15 minutes for the entire download. Cache
lock waits stop after five minutes. Extraction checks a five-minute deadline
between archive reads; blocked operating-system disk operations cannot be
interrupted safely. Download/extraction failures are infrastructure errors, not
scenario failures. New installations are extracted in a temporary directory and
only published after extraction and executable validation complete.

If installation fails or appears slow:

- **Connecting or no new bytes:** Check access to `https://storage.googleapis.com`,
  VPN/firewall rules, proxy authentication, and corporate TLS certificates.
- **Waiting for cache lock:** Another CLI/Desktop process is preparing Chromium.
  Let it finish or cancel that run, then retry. Do not delete a live lock file.
- **Extracting or saving:** Check free space, cache folder permissions, and
  antivirus/quarantine logs. Do not disable security software as a workaround.
- **Support report:** Include the Regressify version and Activity's View Log,
  especially the last phase, bytes received, elapsed time, and complete error.
  A successful install on another computer does not rule out a QA-specific
  network or security policy.

For offline runs, install the managed build once for your user account, then
disable downloads explicitly in any workspace:

```powershell
regressify install
regressify ref --fetch-browser false
regressify test --fetch-browser false
```

`--fetch-browser false` requires the expected cached Chromium revision and fails
with an installation hint if it is missing. Omitting the flag or setting it to
`true` permits downloading only when needed. `browser: chromium` chooses the
supported engine; it is not an executable path override. Baselines captured with
a different browser or revision may need to be regenerated and reviewed.

## Replacement profiles

The `test` command can apply URL replacement profiles from `common/_replacement-profiles.yaml` or `common/_replacement-profiles.yml`:

```bash wrap
regressify test --test-suite alloy --replacement-profile staging
```

Profile selection order is CLI `--replacement-profile`, then the `REPLACEMENT_PROFILE` environment variable, then `default`. The `test` command applies replacements to scenario URLs and action `wait.url` values. The `ref` command ignores replacements so references are captured from raw suite URLs.

## Optimize

`optimize` compresses PNGs only under the active Regressify output directory (`.regressify` or `.regressify__<site-env>`). It is lossless: it does not quantize colors, resize images, or remove color data, so decoded screenshot pixels remain unchanged.

| Flag            | Purpose                                                        |
| --------------- | -------------------------------------------------------------- |
| `--speed`, `sp` | Compression speed from `1` (smallest files) to `10` (fastest). |
| `--min-size-kb` | Skip files smaller than this many kilobytes. Defaults to `50`. |

An unreadable PNG or failed file replacement makes `optimize` fail rather than silently leaving a partially processed output tree.

## Scenario timeouts

Initial page navigation defaults to `60000` ms (60 seconds), matching the legacy Node/VS Code runner. Override it per scenario with `engineOptions.gotoParameters.timeout`; an omitted or zero value uses the bounded 60-second default. This is independent of `elementTimeout`, whose default remains `30000` ms for element waits.

Set `scenarioTimeout` in `regressify.yaml` or `regressify.yml` to bound one complete scenario/viewport capture in milliseconds. It defaults to `600000` (ten minutes). When the deadline expires, Regressify records which step was still running, preserves completed intermediate captures, records failed-capture placeholders for remaining expected targets, and continues the remaining scenarios instead of allowing one unresponsive page or CDP operation to stall the run.

## Capture stability

Regressify waits for the configured navigation checkpoint (full `load` by default). After readiness hooks, it stabilizes active animations before lazy-load scrolling and actions. `freezeAnimations` defaults to `true`; it completes finite CSS/Web Animations and freezes infinite ones at their initial frame.

The initial lazy-load pass moves 80% of a viewport at a time. Each stop waits at
least `lazyLoadScrollDelay` milliseconds (default `250`), then for visible images
to finish loading and decoding. Deferred image/picture sources and common
`lazyload`/`lazyloading` states, including LazySizes backgrounds, are checked so a
decoded placeholder is not mistaken for the final image. Hidden and clipped
carousel slides do not block this pass, and document growth is checked after
each stop. This is not a global network-idle wait or a readiness guarantee for
arbitrary CSS backgrounds and custom loaders; use a browser ready hook when the
application needs its own activation/readiness logic.

`lazyLoadTimeout` bounds the complete pass, including returning to a stable top
position (default `20000` ms). It must be positive. Expiry or exceeding 150 scroll
steps fails the capture rather than silently accepting an incomplete pass.
`noScrollTop: true` skips the pass. `lazyLoadScrollDelay: 0` removes the minimum
pause, not resource checks. Both new settings resolve independently in this
order: scenario, suite, workspace, defaults.

For slower pages, start with these workspace settings:

```yaml
lazyLoadScrollDelay: 500
lazyLoadTimeout: 60000
```

A suite can override them, and an individual scenario can override the suite:

```yaml
id: public-pages
lazyLoadScrollDelay: 300
lazyLoadTimeout: 40000
scenarios:
  - id: utilities
    url: https://example.com/industries/utilities
    lazyLoadScrollDelay: 750
    lazyLoadTimeout: 60000
```

Every `save` checkpoint and the final capture sequence share the resolved `delay` and one `settleTimeout` budget for font/image readiness plus DOM/layout quiet. Animation freezing runs before and after settling to catch newly started animations. Capture preparation does not repeat lazy-load scrolling or move the pointer. Settling remains best-effort: a timeout or settling error is logged and capture continues.

Increasing `settleTimeout` alone cannot activate untouched placeholders. Recapture
and review affected reference and test images; comparing existing PNGs again
cannot load their missing content.

Set `delay: 0` to skip the explicit delay, `settleTimeout: 0` to disable settling, or `freezeAnimations: false` when an animated intermediate state is intentional. `postInteractionWait` remains a separate wait after actions, before the final captures.

Before capture, blinking text carets are hidden with `caret-color: transparent`, independently of animation freezing and settling. Focus, selection, and input values are preserved. Re-capture affected reference/test screenshots if they already contain a visible caret; rerunning `compare` alone cannot remove caret pixels from existing PNGs.

Capture runs use sortable UTC directory IDs with nanosecond precision, so back-to-back invocations do not overwrite each other's screenshots or reports.
Capture commands refuse to reuse an existing suite/run directory. Choose a new
`--run-id` for each invocation or remove an intentionally stale run first; this
prevents old screenshots from being mixed into a new comparison.

## Comparison Reports

Comparison reports retain diff artifacts only for current mismatches. The standalone `compare` command accepts `--async-compare-limit` (default `2`); `test` and `ref` use the suite or workspace `asyncCompareLimit` value.

For equal-width captures, comparison can align several inserted or deleted
vertical bands, including an insertion that pushes content out of a fixed-height
viewport. Partially changed rows can still support alignment through their
unchanged content; the changed pixels remain highlighted. Common blank
backgrounds alone do not justify a shift, and clean unshifted rows are protected
from unrelated changes elsewhere on the page. The alignment search is bounded
rather than exhaustive, and generated diffs stay within the image-size limits.
Saved alignment is shared by desktop and web crosshairs.

Two independent settings control comparison:

- `pixelMatchThreshold` controls how different two pixel colors must be to count
  as a mismatch and receive a diff highlight. Range: `0` to `100`; default: `5`.
  Lower values detect subtler changes. The comparer divides this value by `100`,
  so `5` corresponds to a normalized tolerance of `0.05`. `0` removes color
  tolerance; anti-alias filtering still applies to equal-width aligned images.
- `misMatchThreshold` is the percentage of mismatched pixels allowed for a pass.
  Range: `0` to `100`; default: `0.1`. It does not change diff highlighting.

Saved comparison reports retain `diff.rawMisMatchPercentage` as an unrounded
floating-point JSON number and `diff.mismatchPixels` as the exact integer count
from the comparer, after pixel sensitivity and anti-alias filtering. Pass/fail
uses the raw percentage and image dimensions, never the displayed percentage.
New reports do not write the legacy formatted `misMatchPercentage` string;
readers still accept it when a legacy report has no raw percentage.

Web reports retain the same numeric percentage as `mismatch` and the optional
integer `mismatchPixels`. The CLI, web report, and desktop app use two decimal
places when they show a nonzero difference, adding precision for smaller positive
values. Extremely small values use scientific notation after 12 decimal places;
only an actual zero is displayed as `0.00%`. Formatting never changes audit data.
Web and desktop views show exact pixel counts when available. Missing legacy
counts are not inferred from percentages or assumed to be zero. Rerun Compare
to populate them, then regenerate and redeploy the snapshot; no recapture is needed.

Set `pixelMatchThreshold` in `regressify.yaml` or `regressify.yml`, at the root of
a test suite, or on an individual scenario. Resolution is scenario, then suite,
then workspace, then the default. An explicit `0` overrides inherited values.

Workspace example:

```yaml
pixelMatchThreshold: 5
misMatchThreshold: 0.1
```

Suite and scenario overrides:

```yaml
pixelMatchThreshold: 3
scenarios:
  - url: https://example.com
    pixelMatchThreshold: 0.5
```

Capture runs save the resolved pixel threshold for each screenshot, including
intermediate saves and selector captures. Standalone `compare` reuses these
values; changing configuration applies to new captures, not saved runs. Runs
without pixel-threshold metadata use `5`. Desktop viewport approval retains the
thresholds from the report being reviewed.

New capture runs record both the raw reference page URL and the resolved test
page URL for every report entry. The report links to the reference page and,
when URL replacements are active, its corresponding test page. Run `test` or
`ref` once after upgrading to add this metadata to an existing suite; standalone
`compare` reuses it without needing another browser capture.

Capture metadata also records the scenario ID and human label (or page URL when
unlabelled). Reports expose these as optional `scenarioId` and `scenarioLabel`
fields. The desktop uses the captured name even if the scenario is later renamed
or removed. Screenshot names, matching keys, and exit-code behavior are unchanged.
Older manifests without these fields remain readable.

Older capture runs without ordering or threshold manifests remain comparable: missing ordering data uses alphabetical output and missing mismatch-percentage data uses the command threshold. A present malformed manifest is treated as an artifact error and fails the command.
