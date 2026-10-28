# Diagnose Regressify Failures

Use this guide with the user's authorized local file, terminal, and browser tools.
The MCP server cannot inspect a workspace, receive logs for analysis, execute a
capture, or change baselines. Keep credentials, cookies, saved state, private
source, screenshots, and raw logs local. Do not send them in MCP tool arguments.

## Establish the Failing Operation

1. Compare `regressify version` with the version in MCP responses. Inspect the
   installed command's `--help` before assuming a flag or field is available.
1. Record the exact command, workspace root, output root/environment, suite,
   scenario ID, viewport, and run ID. Preserve the original CLI error and report.
1. Identify the stage: preflight/configuration, browser startup/navigation,
   state/hooks, actions, capture, comparison, or report generation. A categorical
   cause alone is insufficient: a missing element and a slow page both report
   `Timeout`.
1. Read the applicable topic/schema and inspect the actual local inputs. Validate
   names, paths, inheritance, and conditional action fields before changing code.

Exit zero is not proof that every capture passed. Website/action failures and
visual mismatches are report results; configuration, infrastructure, and artifact
write failures fail the process. A failing scenario stops without stopping its
peers. Missing diagnostics do not establish either a successful or failed capture.

## Read Process Errors

A nonzero exit prints a final line beginning `ERROR:` with the full message
chain; that line is authoritative. Earlier console records carry only a coarse
keyword category of that message, so read the final line before acting.
Typical preflight messages and corrections:

- `data did not match any variant of untagged enum TestAction`, with a line and
  column: the action item starting there has a misspelled or unsupported key, a
  `type` or `action` wrapper, two action keys, a wrong value type, or a null
  value from an unquoted `#` selector. Compare it with
  `get_schema(kind="action", action=...)`.
- `unknown field ..., expected one of ...`: a misspelled or wrong-scope key.
- `Unknown viewport name(s)` or `Invalid viewportNames in test suite`: use
  labels from the effective viewport file and the consuming scenario's set.
- `must contain exactly one scenario with id`: fix the `needs` reference.
- `Duplicate scenario ID` or `colliding screenshot filename`: make scenario
  IDs, save names, and selector targets distinct.
- `Invalid selectors for scenario`: an invalid `selector`, `selectors`, or save
  name, including `text=` or other Playwright syntax in a screenshot selector.
- `Invalid action #N in test suite`: a field combination from the actions
  topic's checks beyond the schema, such as `input` with both `value` and
  `file` or a wrongly cased `press` key. `N` counts that scenario's own
  `actions` list from 1.
- `Invalid input file for`: an upload file is missing or outside
  `visual_tests/`.
- `Selected scenario ... must match exactly one scenario` or
  `No matching test suites found`: check suite names and file discovery.
- `Capture run directory already exists`: choose a new `--run-id`.
- `Invalid browser hook for`: the configured `onBeforeScript` or `onReadyScript`
  file is missing or outside the workspace. No Node/npm dependency is needed.
- Missing, escaping, or malformed `cookiePath`, `cssOverridePath`,
  `jsOnReadyPath`, state, or Basic Auth values: repair the referenced local
  file or environment variable without printing secrets.

## Reproduce Narrowly

Only run captures against authorized pages. Replace the example suite/scenario
IDs with real ones, keep the same `--root`, `--site-env`, or `--regressify-dir` as
the failed operation, and choose a fresh run ID to avoid confusing old artifacts.
Global flags precede the command:

```sh
regressify --log-level debug --run-id diagnose-checkout-001 test --scenario shop:checkout --fetch-browser false
```

`--fetch-browser false` requires cached managed Chromium; it prevents downloads,
not browser execution. `regressify install` may download Chromium and needs user
authorization. `--debug` can show Chromium outside CI. There is no dedicated
validation-only command: a valid `test` command proceeds to browser capture.
`--skip-compare` captures without comparison when isolating capture behavior.

Every capture command creates a new run directory and refuses an existing one,
so use a new `--run-id` for each attempt. Without `--run-id`, capture runs are
named by UTC time (`YYYYMMDD_HHMMSS_nnnnnnnnn`), and `compare` or `approve`
picks the lexicographically greatest run directory; custom IDs that start with
a letter sort after every timestamp. Pass `--run-id` to target a reviewed run.

Capture commands select a suite by logical filename and an exact scenario ID.
Compare/approve scenario selections instead use the recorded suite output ID,
which may differ from its filename. Recompute a reviewed run's comparisons with:

```sh
regressify --run-id diagnose-checkout-001 compare --scenario shop-output:checkout
regressify snapshot
```

`compare` changes reports/diff images but does not recapture pages; `snapshot`
builds report assets from existing comparisons, which `test` and `ref` already
do. Neither repairs stale screenshot pixels, and `compare` reuses thresholds
recorded at capture time. `ref` and `approve` change baselines and require
explicit authorization; never use them as an automatic repair for a mismatch.

## Find Reports and Evidence

`<output>` defaults to `.regressify/`; `--site-env staging` selects
`.regressify__staging/`, and `--regressify-dir` overrides that root. Inspect the
actual suite output ID and run ID instead of guessing from a display name.

- `<output>/<suite-id>/bitmaps_test/<run-id>/`: every capture of that run,
  including `ref` runs, plus `report.json`, `_diff.png` images,
  `.png.error.json` failure markers, `.log.json` links, a `.diagnostics.json`
  marker, and `.report-*.json` manifests of expected captures, thresholds, and
  URLs. Do not treat an error placeholder as a good image.
- `<output>/<suite-id>/bitmaps_reference/`: approved baselines and their log
  links. Preserve these during diagnosis.
- `<output>/<suite-id>/html_report/config.js`: the latest report data.
- `<output>/_diagnostics/<operation-id>/`: `index.json` and
  `segment-NNNNNN.jsonl` journals, one operation per capture job or command.
- `<output>/index.html` and `<output>/_snapshot/`: the report website. Serve the
  output folder over HTTP to view it; opening the file directly does not work.

Capture files are named `<scenario-id>[_<viewport>]__<target>.png`, where
`<target>` is `FINAL` (full page or singular `selector`), `VIEWPORT`,
`$<save-name>`, or `SELECTOR-<slug>-<hash>` for each `selectors` entry.
Scenarios without `id` use a slug of `label`, or a 20-character hash of `url`.

Each `report.json` `tests[]` entry has `status` (`pass` or `fail`) and a `pair`
with `fileName`, `scenarioId`, `selector` (`document`, `viewport`,
`save:<name>`, or a selector capture name), `url`, `referenceUrl`,
`misMatchThreshold`, `pixelMatchThreshold`, `comparisonOutcome` (`success`,
`mismatch`, or `failure`), optional `diff` (`rawMisMatchPercentage`, `mismatchPixels`,
`isSameDimensions`, `dimensionDifference`), `engineErrorMsg`,
`referenceLog`/`testLog` links, and an optional `imageSimplificationWarning`.
Common `engineErrorMsg` values:

- `Reference image not found`: no baseline has this capture name, because the
  scenario is new, an ID, save, or viewport was renamed, or its reference
  capture failed.
- `Dimensions differ: reference WxH vs test WxH`: always a failure, whatever
  the thresholds; usually the page height changed. Inspect the diff first.
- `Test scenario failed to capture: <summary>`, or the `Reference` and `Both`
  variants: a page-level failure left a placeholder. In a `ref` run the message
  says `Test` because that run's capture failed; the placeholder is never
  promoted. Reports from releases before failure summaries show
  `<Category> failure (causes: [...])` instead.
- `Comparison error: ...`: an image could not be read or compared.

A failure summary names the failed step, the unmet condition when known, the
step duration, and the error category, for example
`Action 2 (click): no element matched after 30012 ms (Timeout)` or
`onReadyScript failed after 812 ms (Unknown)`. Steps are `Action N (<type>)`,
`readySelector`, `Navigation`, `onBeforeScript`, `onReadyScript`,
`CSS override or browser ready script`, `Hook file loading`, `Cookie file`,
`Saved state`, `Lazy-load scrolling`, `Capture preparation` (delay, settling,
and caret hiding), `Screenshot capture`, and `Browser launch`. Conditions:

- `no element matched`: nothing matched within `elementTimeout`: a wrong
  selector, frame, tab, or page, or content that has not loaded.
- `element was not visible`: zero size, hidden, `display: none`, or opacity 0,
  including elements hidden by your own `hideSelectors` or `removeSelectors`.
- `element kept moving` or `could not be scrolled into view`: animation,
  layout shifts, or a scroll container that resists scrolling.
- `element was covered or outside the viewport`: another element, such as a
  consent banner, sticky header, or modal, covers its center.
- `element was disabled`: disabled, `aria-disabled`, or inert.
- `URL did not match`: a `wait` with `url` timed out.
- `scenarioTimeout of N ms expired during <step>`: the whole capture exceeded
  `scenarioTimeout`; the step was still running.

`Action N` counts actions executed in that capture job from 1: inherited
`needs` actions first, then the scenario's `actions`, then legacy shortcuts;
viewport-skipped actions are not counted. A summary never contains selectors,
URLs, page text, or error messages.

A visual mismatch has `comparisonOutcome` `mismatch`, with
`rawMisMatchPercentage` above `misMatchThreshold`. In diff images, magenta pixels
mark differences over a dimmed copy of the reference.

The raw percentage is an unrounded floating-point JSON number for audits, and
`mismatchPixels` is the exact integer count after pixel sensitivity and anti-alias
filtering. Display formatting does not change either value. Small positive
percentages use extra decimals or scientific notation instead of `0.00%`.
Legacy reports may contain a formatted `misMatchPercentage` string; use it only
when the raw percentage is absent. Missing pixel counts are unavailable, not
zero. Rerun `regressify compare` against the selected saved run to add exact
counts, then regenerate and redeploy its snapshot. No recapture is required.

## Read Capture Records

The console prints each failed capture as
`[i of n] FAILED: <suite-id>:<scenario-id> [<viewport>]: <summary>`, followed by
`Local detail (not saved in reports): <message>`. The detail is the error
message chain, such as the selector that never appeared, the unknown key name,
or a hook's exception and standard error output, after sanitizing: URLs are
reduced to scheme and host, credential-like values are redacted, and long text
is truncated. It never enters reports, markers, or journals. `--no-print` hides
both lines, and output captured by CI may be visible to others, so review it
before sharing.

For timing and identities, read the console records or the capture journal. A
console record looks like:

```text
[5345ms 64e6b72f-...] [Error] ActionsFrames: Stage finished Suite=shop Scenario=checkout Viewport=desktop ActionIndex=1 ActionType=Click Target=#submit Condition=Exists FrameDepth=0 outcome=Failure duration=2080ms cause=Timeout failure (causes: [Timeout])
```

Journal JSON lines carry the same `stage`, `eventName`, `fields`, `outcome`, and
`errorCause` in camelCase, plus an `execution` block with suite, scenario, and
viewport identities, viewport size, and `captureRole`. Useful fields:

- `ActionIndex`: the `Action N` number of the summary. `ActionType` names the
  action; `Navigate` is `goto`.
- `Condition` on a failed record: `Exists`, `Visible`, `Stable`, `Scroll`,
  `Unobscured`, and `Enabled` are the element checks described above;
  `Navigate` is the URL of a `wait`. A selector `wait` checks `Exists` and
  then `Visible`.
- Stage `Navigation` with `Destination` and `TimeoutMs` covers initial
  navigation and `goto`; with `State=Ready` it is the `readySelector` wait.
- Stage `StateHooks` with `ActionType=Hook`: `State=Ready` applies CSS and the
  ready script; `Condition=Before` or `Condition=Ready` with `TimeoutMs` is the
  `onBeforeScript` or `onReadyScript` browser hook. `ActionType=Restore` loads
  saved state, or the cookie file when an `Artifact` field is present. Stage
  `ScreenshotSettling` with `Condition=LazyLoading` is the initial lazy-load
  scroll; `TimeoutMs` records its resolved `lazyLoadTimeout` (default 20000 ms).
  Stage `CaptureArtifacts` with `Capture` is a screenshot target; a
  failed screenshot selector also records `Waiting for readiness` with its
  `Target` and `Condition`.
- A `scenarioTimeout` expiry records `Execution decision` with
  `ScenarioTimeoutMs` and `Reason=Timeout` in the stage that was still running.

New logs use readable source metadata: `Target` contains the selector or action
target, `Destination` the source URL after replacements, `Artifact` a file path,
and `Capture` the capture name without `.png`. Execution records name the suite,
scenario, and viewport (`Default` without one); these names also appear in the
searchable formatted log. Use them with `ActionIndex` to find the suite action.

Simulated free-text payloads, such as an `input.value`, are not recorded. A
selector, including its quoted text, is test-source metadata, not end-user input.
Ordinary source URL parameters remain readable. URL credentials and credential-
bearing parameters are redacted; file/cookie/state contents and script bodies
remain excluded. Source identities are bounded to 2048 UTF-8 bytes and control
characters are neutralized. Review source metadata before publishing reports.

Older capture logs can retain `sha256:` source identities. New readers accept
them, but hashes cannot be reversed by refreshing or regenerating a report.
Capture again with an updated runner to obtain readable source metadata. For a
known old source candidate, its SHA-256 can still be compared locally; never try
to recover unknown values. Image, archive, and asset integrity hashes remain.

Default console output shows `info` and above; `--log-level debug` adds skipped
actions and settling details. `--log-level trace|debug|info|warn|error`,
`--log-verbose`, and `REGRESSIFY_LOG_LEVEL` change display, not archived
collection. `--no-print` suppresses diagnostic console records. A capture's
`.log.json` names its journal in `archive.operationId`; open
`_diagnostics/<operation-id>/index.json` for `state`, `records`, and
`missingRecords`. Archives can be complete, partial, missing, legacy
unavailable, or unreadable. Missing logs after a crash, disabled archive,
disk/queue limit, or corrupt sidecar are not an empty successful timeline.
Inspect warnings and `REGRESSIFY_DISABLE_DIAGNOSTIC_ARCHIVE`; do not delete
evidence to hide a warning. Disk/chunk limits are operation settings, not
fields to add to suite YAML.

In the desktop app, use Results/Capture Log for image execution and View Log for
the saved command operation. The snapshot report exposes referenced sanitized
capture logs. Startup failures may instead have journals under the user's local
application-data directory at `regressify/diagnostics/startup/_diagnostics`.
Capture journals are not browser console logs or network-body recordings. When
the summary and local detail are not enough, reproduce with `--debug` and
browser developer tools, with authorization.

## Common Failures

- Element not found or not actionable: confirm the URL, tab, frame, and
  selector against the live page, then act on the condition. Selectors use the
  first DOM match: when a responsive page keeps both a hidden mobile copy and a
  visible desktop copy of a link or button in the DOM, an unscoped selector can
  match the hidden copy first and fail with `element was not visible`
  (`display_none`). Scope the selector to the active container, append
  `:visible` or `>> visible=true` on action/wait/`readySelector` selectors, and
  restrict viewport-specific steps with `viewportNames`. Wait for the URL and a
  destination element after navigation, add `switchTab: new` after links that
  open tabs, set `frame` for iframes, dismiss or remove covering overlays, use
  `click` instead of `select`/`check`/`uncheck` for custom `role="combobox"`,
  `role="switch"`, or `role="checkbox"` controls and visually hidden native
  inputs, and use `useFileChooser` for hidden file inputs. Do not blindly raise
  timeouts.
- Screenshot selector failure: `Screenshot capture` or a `save` action is
  missing, hidden, zero-sized, inside an iframe (unless `frame` is set on
  `save`) or shadow root, or written with `text=`, `:visible`, or another
  non-CSS engine (`InvalidData`).
- Navigation or URL wait timeout: check the authorized URL, replacements,
  redirects, TLS, and `waitUntil`. A URL wait does not select a popup; end the
  pattern with `*` when a query string may follow; `networkidle` may never
  occur on continuously active pages.
- Missing state, 401, or unexpected login page: verify the active output
  environment, state names, cookie attributes, and Basic Auth origins and
  placeholders locally. State must exist before capture preflight; `needs` and
  `requiredLogin` do not create a login session.
- Browser ready script or CSS failure: the summary names
  `CSS override or browser ready script`. Read the local detail for the
  browser's error, check the `elementTimeout` bound, and try the script on the
  page in browser developer tools.
- Browser script syntax/runtime error or hook timeout: read the hooks topic.
  Use plain browser JavaScript with DOM APIs, not module exports, `require`,
  `process.env`, or Playwright callbacks. Return a promise for asynchronous
  work and check `elementTimeout`. `jsOnReadyPath` and `onReadyScript` both run
  after readiness and before actions, so they can dismiss a blocking banner.
- Invalid timeout: `elementTimeout`, `actionabilityTimeout`, `lazyLoadTimeout`, and
  `scenarioTimeout` must be positive whole milliseconds. `settleTimeout: 0`
  disables settling; omitted/zero `readyTimeout` inherits element timeout;
  omitted/zero navigation timeout uses 60000 ms.
- Invalid image simplification: `imageSimplification` accepts only `none` or
  `pixelate` (not `off`). `imageSimplificationBlockSize` must be an even whole
  number from 2 to 16, `imageSimplificationMinSize` at least 1, and
  `imageSimplificationExcludeSelectors` a list without blank entries. The error
  names the scope and, for selectors, the 1-based entry number. These checks
  apply at every scope even when simplification resolves to `none`.
- Image unexpectedly sharp with `pixelate`: read the capture log's decision
  counts. `smallImages` are below `imageSimplificationMinSize`; `excludedImages`
  match an exclusion or sit inside a matching element; `filteredImages` already
  have a CSS `filter`; `transformedImages` are scaled, zoomed, skewed, or moved
  by fractional pixels; `overlappedImages` have text, badges, or sticky content
  painted on top. A nonzero `invalidSelectors` means an exclusion is not valid
  CSS and matches nothing. These are expected outcomes, not capture failures.
- Image simplification settings warning: a pair's
  `imageSimplificationWarning`, and the matching console `WARNING:` line, mean
  its reference and test captures recorded different settings. Its `reference`
  and `test` values show what each `.log.json` recorded; `null` is a capture
  without a recorded setting, such as a reference from an earlier release, and
  counts as `none`. Exclusions appear only as `excludeSelectorsDigest`. The
  warning never changes `status`; mismatches it explains are still real pixel
  differences. Align the settings, then re-capture references with `ref` or
  approve correct test captures, both only with the user's authorization.
- Visual mismatch with successful capture: compare reference, test, and diff at
  the same viewport, and verify baseline age, page data, fonts, rendering, and
  intended changes. Distinguish the allowed mismatch percentage from per-pixel
  color tolerance; do not mask important content or raise thresholds to hide a
  real regression.
- Mismatches that vary between runs: stabilize time-based, random,
  personalized, animated, or lazily loaded content with `hideSelectors`,
  `removeSelectors`, explicit waits, `settleTimeout`, or `delay`. Captures
  already freeze animations and hide text carets.
- Missing lazy images: inspect both captures for placeholders. Initial scrolling
  waits for visible resources and common lazy-loader markers. Tune
  `lazyLoadScrollDelay` and `lazyLoadTimeout` at workspace, suite, or scenario
  level, in that override order. A timeout fails the capture and does not mean
  the page settled. Use an awaited browser ready hook for custom loaders;
  increasing `settleTimeout` alone cannot activate an untouched placeholder.
  Recapture affected references too instead of raising mismatch thresholds.
- No captures or unexpected capture count: check scenario/viewport filters,
  inherited action filters, named saves, and singular versus plural selectors.
  `selectors: []` is invalid; omitted/null means automatic full-page capture. A
  last applicable save suppresses only the automatic final capture.
- Missing dependency or capture-name collision: check `needs` IDs and cycles
  and every scenario/viewport/save/selector name. Names must remain portable and
  distinct after normalization. Do not repair collisions by deleting baselines.
- Missing file or path escapes workspace: resolve configured paths from the
  workspace root; upload paths start at `visual_tests/`. Check regular-file
  status, symlinks, and `.yaml`/`.yml` precedence. Do not disable containment.
- Chromium preparation failure: `--fetch-browser false` needs the cached build,
  which an authorized `regressify install` prepares. Proxies and firewalls must
  allow the download; `--proxy` applies to it.
- Artifact permission, disk, or unsafe-path error: preserve good outputs and
  check the failing path, free space, locks, and permissions. Initialization
  can fail on malformed/read-only MCP or agent files; repair the draft or access
  issue, not unrelated project data.

## MCP Connection Problems

The endpoint is an HTTP MCP handler ending in `/mcp`, not a browser-facing HTML
page. Confirm the configured URL/type and reconnect the client after a service
upgrade to refresh discovery. Compare server/CLI versions. For self-hosted
services, check proxy/TLS, credentials, allowed origins, and the provider's local
diagnostics; do not disable authentication or Origin checks as a workaround.

CLI init and desktop folder opening manage `regressify-mcp` in `.vscode/mcp.json`
and the `## Regressify CMP` section in `AGENTS.md`. They refresh the managed
endpoint/instructions while preserving unrelated content. Older installed
versions may lack automatic desktop setup. Explain a missing or outdated MCP
service to the user instead of inventing undocumented configuration.
