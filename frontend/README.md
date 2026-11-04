# Snapshot Frontend

The snapshot site is a static React SPA built with Vite 8, TypeScript, Tailwind CSS,
and Biome. It uses the supplied Roboto and JetBrains Mono fonts locally. No CDN,
backend, BackstopJS bundle, or runtime package manager is needed by published reports.

The header logo and favicon share the desktop app's [SVG source](../assets/icon.svg).
Vite bundles it once with a content-versioned URL; no separate frontend artwork is kept.

## Develop

Use the Bun version in [.bun-version](.bun-version). From this directory:

```sh
bun install --frozen-lockfile
bun run dev
```

Development generates synthetic screenshots using an existing Chrome/Chromium/Edge
installation, then starts Vite at `http://127.0.0.1:5173`. Set
`PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH` to another Chromium executable, or explicitly
install Playwright's Chromium with `bun --bun playwright install chromium`.
No production pages or deployment credentials are used. Generated fixture data,
screenshots, traces, and dependencies are ignored by Git and Biome.

## Build

```sh
bun run lint
bun run test
bun run build
```

`bun run build` type-checks source, scripts, and tests; builds Vite; and replaces
[../src/snapshot/data](../src/snapshot/data) with the complete generated bundle.
The sync script validates the output in a staging directory before replacing the
previous assets. It removes obsolete assets and generates the Rust include list.
Files are replaced atomically inside the existing directory so Windows editor
watchers do not block a directory rename. Finish the frontend build before Cargo.
Run builds in an isolated checkout where no other process can modify `dist`,
`hosting`, or `../src/snapshot/data` during the build. The sync script rejects
observed symlinks, but its path-based checks do not protect against a concurrent
writer replacing entries between checks and reads. This is separate from the
output directory used by a running Regressify test.
Commit the generated directory for ordinary Cargo builds, which do not run Bun.
Do not edit generated files directly. `bun build` is Bun's separate built-in bundler;
the project command is **`bun run build`**.

The generated directory is marked `-text` in [.gitattributes](../.gitattributes)
so Git preserves the exact bytes used by resource cache versions. Windows CRLF
conversion otherwise invalidates those hashes, even in a clean working tree.
For an already converted checkout, run `bun run build` before rerunning Cargo;
do not normalize bytes in the hash helpers or bypass the resource-version test.

Frontend text uses `text=auto eol=lf` in the same attributes file so Windows
autocrlf cannot undo Biome formatting. Binary assets remain binary. Run
`bun run format` once for an already-converted checkout, then `bun run lint`.

To preview the actual Rust-generated site, run from the repository root:

```sh
cargo run --bin regressify -- --root frontend/.fixtures/workspace snapshot
```

Then from this directory:

```sh
bun run preview
```

The preview serves the generated fixture at `http://127.0.0.1:4174` with SPA
fallback and production cache/security headers. Another report and port can be
supplied: `bun run preview /path/to/output 4175`. It binds to loopback and exposes
only the entry page and snapshot assets, not capture folders or workspace files.
Run `bun run fixtures` first if development has not created the synthetic workspace.

## Test

```sh
bun run typecheck
bun run lint
bun run test
bun run test:e2e
```

Unit tests cover schemas, safe links, filters, route encoding, and atomic asset
synchronization. Git must be available for the read-only generated-byte regression;
it does not stage files, change Git configuration, or contact remotes.
Playwright uses desktop and phone viewports for navigation,
deep-link refresh, filtering, theme persistence, pre-bundle theme/background
application, blocked storage, cross-tab theme changes, all image modes, zoom, downloads,
suite-scoped scenarios, independent pane scrolling, divider drag/keyboard resizing,
failure jumps, selectable/copyable Info content, errors, hostile content,
narrow/wide layouts, and axe accessibility checks. They also verify the shared
logo/favicon bytes, suite-aware titles, and list preview alignment, missing images,
conditional diff lanes, and long labels.
Side-by-side crosshair checks cover two- and three-lane comparisons, bounded
equal-width lanes, a shared maximum-width/maximum-height canvas, synchronized
image pixels, unequal dimensions,
inserted/deleted diff bands, zoom-independent strokes, missing/broken images,
pointer cleanup, and linked native lane scrolling. Band tests cover every source lane, both
alignment directions, and absent target pixels inside the gap.
Lane-wide checks hover below images and inside side padding, verify full-viewport
one-pixel strokes, and retain guides during stationary-pointer zoom, scrolling,
and fit changes. The overlay's positioned wrapper sits outside the native scroll
pane so guides never add image overflow; image-coordinate projection still uses
the actual image bounds and saved diff alignment. Images retain their individual
dimensions and share the canvas's top-left origin instead of centering separately.
Smaller shared canvases start horizontally centered and top-aligned. They can be
dragged left/right and top/bottom together until the widest/tallest image reaches
the corresponding lane edge, without changing zoom or creating scroll overflow.
Each underfilled axis moves independently, including diagonal drags.
Fit controls restore horizontally centered, top-aligned placement.
Delayed-image checks verify explicit dimensions, unchanged fit zoom, and stable
comparison geometry in every mode, including failed loads. They disable entrance
animations so measurements isolate image loading.
The `image viewer` browser checks cover fit-to-window, anchored Ctrl+wheel zoom,
exact drag offsets, keyboard scoping, per-mode zoom, double-click, and touch/blur
cleanup on desktop and mobile. Run these and the existing image-mode/crosshair
checks against both Vite and the generated report.
Wheel regressions verify the exact pixel anchor through fit boundaries and from
each lane, cancellation in empty viewer space and at zoom limits, and listener
cleanup on leaving Comparison. Use integer mouse coordinates and derive the
expected source pixel from those coordinates to avoid fractional input rounding.
Fixed-lane tests zoom oversized images, verify per-lane horizontal/vertical
scrollbars and reachable image edges, and keep names/controls stationary. Geometry
tests disable entrance motion; hover offsets use the pane's padding box, excluding
its border. Crosshair targets must first be scrolled into the source lane.
Linked-scroll regressions cover both axes from every lane, two/three lanes,
exact 3000x1000 shared bounds for differently sized images, keyboard/drag input,
pointer zoom, and fit reset. Fitted-drag checks cover both edge limits, every source
lane, resizing, retained vertical scrolling, and anchoring after horizontal movement.
Mirrored scroll events and zoom adjustments must not feed back into the source pane.
Screenshots are saved under `.fixtures/screenshots`.

Footer checks cover short, filtered, and not-found pages, plus long content in
document scrolling on desktop and mobile, with an independently scrolling sidebar.
Page scrollbar gutters remain stable on overview, suite, comparison, capture-log,
error, and landing pages, including when vertical scrollbars appear or disappear.
Empty gutters retain a theme-aware track color without changing the page background.
Native-scrollbar pixel checks cover short overview and suite pages in both themes.

Overview checks cover all five sort columns in both directions, filter preservation,
aggregate totals, conditional count highlights, and rendered pass-rate colors in
both themes. Empty suites and rates that round to 100% retain their actual status.
They also verify bold summary values, dividers between stacked summary rows, and
mobile result counts centered within their cells and aligned in one value column.
Header and Settings checks cover compact spacing, separate report-text lines,
empty/long text, keyboard navigation, dismissal/focus return, and scrolling when
more settings are added. Theme synchronization also runs with Settings closed.
Large-suite checks use 300 captures and assert bounded image sources after scrolling,
view changes, filtering, and distant failure jumps. Preview checks verify that suite
browsing requests no full-resolution captures, while comparison and downloads do.
Both generated previews and legacy reports without preview fields are covered.

Set `SNAPSHOT_URL=http://127.0.0.1:4174` before `bun run test:e2e` to test the
Rust-generated synthetic report instead of starting the Vite fixture server.
The tests expect the supplied synthetic workspace, not arbitrary user reports.

## Structure

- [src/data](src/data): versioned schemas, report loading, cache, and route helpers.
- [src/pages](src/pages): overview, suite results, and individual image review.
- [src/components](src/components): controls, states, and synchronized image viewer.
- [src/assets/fonts](src/assets/fonts): the two locally bundled source fonts.
- [scripts](scripts): fixture generation, safe preview, and build synchronization.
- [hosting](hosting): Azure/Netlify fallback, security, and cache policies.
- [tests](tests): browser workflows and a synthetic screenshot page.

## Landing Pages

The desktop [landing renderer](../src/desktop/landing.rs) pre-renders headings,
safe Markdown subheadings, and external site links using the shared HTML template
and frontend assets. [Landing enhancements](src/landing.tsx) mount only theme and
status controls, leaving the pre-rendered content usable without JavaScript.
No linked workspace is required locally.

Each linked site's origin supplies `/_snapshot/status.json`. The version-1
[status schema](src/data/landing.ts) contains `generatedAt`, `suites`, `total`,
`passed`, `failed`, and `errors`; counts refer to captures, and errors are a subset
of failures. Requests omit credentials/referrers, reject redirects, revalidate
caches, time out after 10 seconds, and limit responses to 64 KiB. Old, private,
malformed, or unreachable endpoints affect only that site's status, not its link.

Generate a production landing fixture by setting `REGRESSIFY_LANDING_FIXTURE` to
a new empty directory and running:

```sh
cargo test --features desktop --lib desktop::landing::tests::generate_landing_browser_fixture -- --ignored --nocapture
```

Serve that directory with `bun run preview <directory> 4175`. Set both
`LANDING_URL` and `SNAPSHOT_URL` to `http://127.0.0.1:4175`, then run
`bun run test:e2e -- landing.spec.ts`. These tests use synthetic remote responses
and cover pre-rendered Markdown, no-JavaScript links, anonymous fetching,
unavailable/malformed status, retry, themes, responsive layout, and accessibility.

## Data And Hosting

Capture `error` and suite `errors` count only failures that prevented a requested
reference or test screenshot, such as failed actions, navigation, or timeouts.
Pixel and dimension mismatches remain failed comparisons, not engine errors.
Optional `comparisonError` preserves comparison diagnostics, including missing
references and comparison-processing failures, without adding to engine-error
counts. Result cards and Capture information retain these messages.
Mismatch percentages keep two decimal places whenever that displays a nonzero
value. Positive differences that would round to `0.00%` use additional decimal
places, with scientific notation for values still zero after 12 decimal places;
an exact zero remains `0.00%`. Formatting never alters the numeric `mismatch`
audit value, comparison thresholds, or pass/fail decisions. Optional integer
`mismatchPixels` contains the comparer's exact count, shown in suite cards and
Capture information. Absent legacy counts are omitted, not inferred or set to zero.
Rerun Compare to add counts to older reports before regenerating the snapshot.
Older capture payloads are normalized on load; regenerate existing snapshots and
redeploy hosted copies to refresh their bundled UI and overview totals. Existing
capture runs are reused; no recapture is needed. The manifest version remains 2.

Version-2 captures may include optional `logs.reference` and `logs.test` metadata.
Each reference records `complete`, `partial`, `missing`, `legacyUnavailable`, or
`loadingError`, original operation/execution IDs, record counts, build identity,
and versioned JSON chunk URLs. Missing fields identify pre-log reports. Log state
is independent of the capture's result and existing `error` field; the manifest
version remains 2.

Capture Log uses the existing image route with `?view=log&logRole=test` (or
`reference`). `logLevel`, `logStage`, `logSearch`, and `logChunk` preserve view
selection through refresh. `suiteView=list` retains the originating suite layout
while Log is open; Comparison and Results restore `view=list`.
Opening the log lazily fetches bounded, content-version
verified JSON chunks; a four-chunk cache deduplicates requests and stays bounded.
Messages render as text. Download verifies the full record sequence and exports
all matching records, subject to an explicit 128 MiB browser export limit; it never
offers a shortened file as complete. Non-complete logs disable export controls.
Out-of-range timestamps are rejected before rendering and produce a retryable
loading error rather than crashing the report. Command and application operation
kinds remain excluded from the frontend capture-log schema.

Source identities accept bounded readable text as well as legacy `sha256:`
values. New capture logs expose suite/scenario IDs, viewport names, selectors,
source URLs, artifact paths, and capture names for searching and source lookup.
Simulated free-text payloads such as `input.value` remain absent; quoted selector
text and ordinary source URL parameters are not user-input payloads. Credentials
in URLs and credential-bearing parameters are redacted by the producer. Readers
enforce the 2048-byte/control-character boundary and render metadata as text.
Review this source metadata before publishing. Rebuilding the UI does not recover
the source of historical hashes; recapture to produce readable metadata.

Snapshot generation publishes at most 250 records and 256 KiB per JSON chunk under
`_snapshot/data/logs/<operation-id>`. Selected captures share versioned chunks for
their original execution. The deployment inventory verifies every chunk's version,
identity, sequence, and record count; raw `_diagnostics` journals, deployment,
command, and application logs,
unselected runs, and workspace state are excluded. Log bytes count against the
provider's existing package/storage allowance and are never silently truncated.

Manifest, suite, and log JSON requests are same-origin and use
`credentials: "same-origin"`, so cookie-based gateways such as Cloudflare Access
authorize them like the entry page. Omitting credentials makes Access redirect them
to its cross-origin sign-in page, which browsers report as a CORS error.

Resource filenames are stable; cache versions appear only in `?v=...`.
The public aggregate endpoint `/_snapshot/status.json` is deliberately
unversioned, uses `Cache-Control: no-cache`, and permits anonymous cross-origin
reads with `Access-Control-Allow-Origin: *`. Do not give it immutable JSON caching.
The publisher checks its counts against the active manifest before staging.
The [Rust publisher](../src/snapshot/site.rs) uses
[get_asset_version](../src/snapshot/helpers.rs) with 20 characters of the SHA-384
base64url digest. The [frontend build](scripts/version-assets.ts) mirrors that
algorithm, with a Rust regression test checking the actual generated references.

The version-2 manifest and every suite/image URL follow the same contract:

```text
_snapshot/assets/app.js?v=<version>
_snapshot/assets/style.css?v=<version>
_snapshot/assets/icon.svg?v=<version>
_snapshot/assets/roboto-combined.woff2?v=<version>
_snapshot/data/manifest.json?v=<version>
_snapshot/data/suites/shop.json?v=<version>
_snapshot/images/shop/reference/home.png?v=<version>
_snapshot/images/shop/test/home.png?v=<version>
_snapshot/images/shop/diff/home_diff.png?v=<version>
```

Suite JSON is loaded on demand. [Frontend schemas](src/data/report.ts) reject
unversioned, unsafe, or noncanonical artifact URLs. Changing an image updates its
version, its suite JSON version, and the manifest version without renaming files.

Optional capture `previews` fields contain `reference`, `test`, `diff`, and `grid`
image URLs under the same version-2 schema and path-validation rules. The
[publisher](../src/snapshot/capture_report.rs) creates full-image previews bounded
to 640 x 480 and full-width grid thumbnails bounded to 640 x 400 (cropped vertically
around the first diff area when a diff exists, at the top of the test capture
otherwise, at the top of the dimmed reference capture overlaid with the error icon
at 25% opacity when no test image was captured, or the contained error icon when
neither image was captured; short pages align to the top of the thumbnail area),
without upscaling.
It uses fast thumbnail resizing and PNG Fast/Sub encoding without copied metadata
or expensive optimization passes. Identical inputs produce byte-identical outputs;
tests compare complete encoded bytes across repeated generations. Only the grid's
source image needs a cropped thumbnail. Source captures are never resized or rewritten.

Optional capture `imageSizes` fields record the decoded original `width` and `height`
for `reference`, `test`, and `diff`, with null values for absent images. These positive
pixel sizes are separate from the signed comparison deltas in `dimensions`.
The comparison viewer reserves its layout and fit zoom from these sizes before
images load. Reports without `imageSizes` still discover sizes when originals load;
regenerate and redeploy them to get stable pre-load sizing. Existing captures are reused.

Optional capture `alignment` records the comparer's virtual vertical gap in
`reference` or `test` as `{ start, height }`, with null for an unshifted source.
The diff uses aligned coordinates. Crosshairs map each source row into that space
and back into the target image; rows within a missing band have no target guide.
Validate the gap against source/diff heights and retain ordinary pixel coordinates
when metadata is absent or inconsistent. Older runs need Compare rerun before
snapshot regeneration and redeployment; never infer a gap from diff colors or
height differences alone. This metadata does not change captures or diff pixels.

[ReportImage](src/components/ui.tsx) uses one IntersectionObserver per scroll root,
activating preview sources within 600 pixels and removing distant sources so the
browser can reclaim image resources. Card labels and links remain available for
search, keyboard navigation, and failure jumps. Missing and failed images retain
their lanes. Every image has explicit width/height attributes; suite previews use
their fixed thumbnail/list bounds with responsive CSS. Full-resolution comparison images remain eager; downloads always
use original URLs. Reports without `previews` fall back to their original images.
Regenerate and redeploy existing reports to obtain the reduced-size assets.

The shared [Rust naming resolver](../src/helpers/scenario_names.rs) supplies
`scenario` and generated capture `label` values for both desktop and web reports.
Explicit labels win; URL-only scenarios use the shortest unique readable path
suffix, origins only when mixed, and query/fragment details or numbered suffixes
when needed. Captures record `scenarioDisplayName` before suite filtering or URL
replacement; legacy reports derive names from available original URLs. Capture
`id`, image filenames, source URLs, and routes are unchanged. Do not duplicate the
naming algorithm in the SPA. The search/navigation fixture covers URL-only names
and multiple viewports in both Vite and Rust-generated production reports.

The build uses one JavaScript bundle to avoid circular chunk-version dependencies;
CSS font references are versioned before the CSS itself. Captured website links
are external source pages, not local report artifacts, and remain unchanged.

Configure any CDN to **include the `v` query parameter in its cache key** and keep
it on origin requests. Generated asset policies use immutable one-year caching.
**The entry HTML must revalidate** so visitors discover new versions; do not cache
`/`, `/index.html`, or SPA route responses indefinitely. Publish the complete output
atomically, as the desktop Azure/Netlify/Cloudflare deployers do. Old versions are not retained
at the origin; reload an old report after a deployment. The local preview rejects
stale `v` values, making accidental version/content mismatches visible.

Snapshot generation stages and validates the complete report before publishing,
replaces individual files atomically, and writes HTML last. It removes obsolete
files only inside the generated `_snapshot` directory. Capture history and raw
comparison reports are untouched. Desktop deployment includes only dependencies
of the current manifest and refreshes query versions after image compression.
Azure (250 MiB Free, 500 MiB Standard) and Cloudflare (2 GiB) share budget-aware PNG
optimization in temporary copies. Original captures and stable filenames remain
unchanged; diffs and error placeholders stay lossless.

Deploy at the site's origin root. Routes are `/`, `/suites/:suiteId`, and
`/suites/:suiteId/images/:captureId`. Serve `/index.html` for suite-route requests,
but return real 404 responses for missing `/_snapshot/*` assets. Azure and Netlify
configuration is generated automatically. Opening HTML through `file://` is not
supported. Old `/suite/html_report/index.html` entry pages are removed.

Cloudflare Workers deployment consumes the same header and redirect policies as
asset metadata and excludes the policy files from public assets. Its canonical
HTML rules require `/suites/* / 200`, not `/suites/* /index.html 200`; the provider
client translates that rewrite without changing Netlify output. Keep
`not_found_handling` as `none` so missing report assets remain 404s. When a saved
target explicitly disables HTML normalization (`htmlHandling: none`), the client
retains the `index.html` rewrite instead.

Desktop Cloudflare deployment creates missing Workers and publishes their
production `workers.dev` route by default. Saved targets also support explicit
private or preserve-routing modes and asset settings. Uploads support both raw
single-asset and Base64 multipart transport, with MIME-aware cache identities.
See [desktop deployment](../docs/desktop.md#deploy-reports) for setup and limits.

Azure and Netlify share the global security policy, including
`Referrer-Policy: no-referrer` and `X-Content-Type-Options: nosniff`. Change hosting
templates in [hosting](hosting), rebuild the embedded assets, and run the Rust
hosting-policy regression to keep the generated policies aligned. Cloudflare
reuses the Netlify header rules and validates them in its live publishing test.

Suite summaries and suite JSON accept optional `displayName` strings without
changing schema version 2. Lists, search, headings, breadcrumbs, Info, and browser
titles use nonblank display names, falling back to `id`. Routes, artifact paths,
and React keys continue to use `id`; never derive identity from presentation text.

Suite and image pages use `suite name | report title` as the browser tab title.
Returning to the overview restores the configured report title.

The shared top bar places the configured report heading and safe Markdown
subheading on separate lines beside the logo. On phones, the wordmark is hidden
to leave room for report text; long text wraps without overlapping the controls.
The overview uses the page heading `Snapshot Overview`, with the generated date
and time directly below it in the browser's locale. Its content is centered within
a 1080px maximum width and has no breadcrumb row. Suite and image pages retain
their navigation. Review panes fill the remaining viewport below the actual header
height only for image comparison. Other page content uses the document scrollbar.
The shared header stays visible while breadcrumbs and page controls scroll normally.

The overview summarizes Total tests, Passed, Failed, and Pass rate across all
suites. Test totals count captures, and filters do not change the summary. The
table shows Test suite, Total tests, Passed, Failed, and Pass rate. Every column
sorts ascending or descending; selecting a new column starts ascending. Sorting
preserves filters and survives refresh. Pass rates sort by their unrounded ratios.

Passed counts highlight green only for nonempty, fully passing suites; nonzero
failed counts highlight red, including engine errors. Pass-rate bars are green
for fully passing suites and neutral for failing suites. Bar lengths and displayed
percentages still represent passing captures: 0% remains empty, and a rate that
rounds to 100% does not change the suite's status. Engine errors appear as a numeric
badge beside the suite name; hover, keyboard focus, or tap reveals the full count
label in a tooltip. Escape or an outside press dismisses it.

At widths up to 760px, each suite becomes a stacked group with its name above
labeled Total tests, Passed, Failed, and Pass rate rows. No horizontal scrolling
is needed. A Sort by selector and direction button retain all five sort options;
larger screens use the sortable table headings.

Markdown subheadings allow text, emphasis, lists, code, and safe HTTP(S) links.
Raw HTML and embedded images are not rendered. Page links reject credentials and
non-HTTP(S) protocols. No analytics run.

The header's Settings icon opens a native dialog with an Appearance section and
the Light, Dark, and System theme control. Its bounded, scrollable content supports
additional settings rows and sections without hiding the close button. Escape,
the close button, and backdrop clicks dismiss it and return focus to the trigger.
The theme control stays mounted while the dialog is closed.

Theme preferences are local. The saved choice and page background apply in the
document head before the app bundle or report data loads.
Missing, invalid, or inaccessible preferences fall back to the system theme.
Explicit theme changes synchronize across report tabs on the same origin; only
System mode follows operating-system appearance changes. Loading a report does
not rewrite the preference. When storage is blocked, theme switching still works
for the current page.

Suite and image pages show the current suite name in a dropdown selector above
Test scenarios. The selector opens a searchable Floating UI menu over the pane
without moving the scenarios. The current suite is highlighted and checked;
arrow keys move between suites and Enter selects one. Escape or an outside click
closes the menu, and reopening it clears its search. On phones, Escape dismisses
the dropdown without closing the navigation drawer.
The selected suite's scenarios remain visible, with several captures grouped into
individual image links. Scenarios have their own search.
The left pane stays below the header and scrolls independently. The review area
uses the root page scrollbar, without a second reserved gutter. Drag the divider
to resize it, or focus it and use arrow keys, Home, and End. Its width is remembered;
phones use a drawer with the same selector and scenario list.

Overview and suite footers stay at the bottom of short pages and follow longer
content without covering it. Image review remains footer-free.

Image comparison uses bounded image panes with independent canvas scrolling.
Unmodified mouse-wheel, trackpad, and touch scrolling move the active image pane.
Left-drag pans that pane using pointer capture, without changing zoom or wipe
position; release, cancellation, lost capture, blur, or a view change clears the
gesture. Ctrl/Cmd+wheel selects the next zoom preset around the pointer; zoom
buttons use the same 25-800% presets as desktop. Fit width (the web default) and
Fit to window remain responsive and never upscale. Actual size selects 100%.
Each mode retains its zoom for the current capture; selecting another capture
resets it. Double-click toggles actual size and fit-to-window. The focused image
surface accepts `+`/`-`, `0` (window), `1` (actual), `W` (width), arrow keys to pan,
Shift+arrows for larger steps, and Escape to cancel a drag. Inputs and sliders
keep their own keyboard behavior.
Side-by-side review shows aligned Reference/Test/Diff lanes when a diff exists.
Hover any image to show synchronized crosshairs at the same image-pixel coordinates
in all three lanes. White guides with dark outlines stay one screen pixel wide at
every zoom level. Missing images and coordinates outside a smaller image have no
guides. Moving off the image, scrolling, changing views, or using touch clears them.
Wipe follows mouse movement without dragging; its slider supports keyboard and touch.

Hover or focus the Info icon for mismatch/threshold statistics, dimensions, capture
and run details, source URLs, and engine errors. The bubble allows native text
selection and copying. Click/tap to pin it; Escape, outside click, or its close
button dismisses it. Metadata stays out of the main image area.

Suite pages have sticky previous/next failed-comparison arrows. They scroll to and
highlight failed cards without changing routes, respect the current search/status
filters, and disable at the first/last visible failure or when none are present.

List view shows labeled Reference and Test previews side by side for each capture,
with a Diff lane only when available. Previews fit the complete image without
cropping; missing or unavailable images retain their labeled lanes. The layout
stays side by side on phones. Grid view keeps its single preview, and opening a
capture preserves the current filters and view selection.

## Generated-Assets Pull Request

[Snapshot assets workflow](../.github/workflows/snapshot-assets.yml) runs on pushes
to `release` that change `frontend/**`, or by manual dispatch. It installs the
locked dependencies, runs lint and unit tests, then executes `bun run build`.
A separate write-permission job starts from **`master`**, replaces only
`src/snapshot/data`, and creates or updates `automation/snapshot-frontend-assets`.
The PR contains generated assets only; no release-branch source commits are merged.
It is never automatically merged, and a no-change build creates no empty PR.
The job uses the runner's GitHub CLI: `gh pr create` for a new PR and `gh pr edit`
for an existing one. Branch updates use an explicit `--force-with-lease` against
the observed remote commit. If assets already match `master`, the job closes any
stale automation PR and deletes its branch without creating a commit. Lookup,
diff, commit, and push failures stop the job rather than creating a misleading PR.

Enable **Settings > Actions > General > Workflow permissions > Allow GitHub
Actions to create and approve pull requests**. The default `GITHUB_TOKEN` needs
the declared contents/PR permissions. PRs opened with that token do not normally
trigger other Actions workflows; an optional `SNAPSHOT_ASSETS_TOKEN` secret can
provide a suitably scoped GitHub App/PAT token when required checks must trigger.
No token is passed to frontend build steps. Merge the supporting Rust integration
before using generated-only updates with this new report format.
