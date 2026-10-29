# Workspace Configuration

The workspace root contains `regressify.yaml` or `regressify.yml`, a YAML object
with optional fields. Prefer one file; if both exist, the CLI loads `.yaml`.
Use `{}` to inherit defaults. Do not create a `workspace:` or `config:` wrapper.
Use `get_schema(kind="workspace")` for the exact field set, descriptions,
accepted types, and constraints, and `get_example(task="workspace")` or
`basic-auth` for synthetic templates. A JSON-looking value may be valid YAML,
but a file named `regressify.json` is not automatically discovered as workspace
config.

## Precedence and Exceptions

For fields supported at each level, values resolve scenario, then suite, then
workspace, then built-in defaults. Omitted/null optional values inherit. Explicit
`false` and allowed zero values are not treated as missing. `hideSelectors`,
`removeSelectors`, `imageSimplificationExcludeSelectors`, and viewport
selections replace rather than append lists.
An empty viewport selection means all configured viewports, not no viewports.

Do not assume every field exists at every scope:

- Workspace, suite, and scenario: `logLevel`, `basicAuth`, `bypassCsp`,
  `cookiePath`, `cssOverridePath`, `jsOnReadyPath`, `useCssOverride`,
  `hideSelectors`, `removeSelectors`, `misMatchThreshold`,
  `pixelMatchThreshold`, `postInteractionWait`, `lazyLoadScrollDelay`,
  `lazyLoadTimeout`, `imageSimplification`, `imageSimplificationBlockSize`,
  `imageSimplificationMinSize`, `imageSimplificationExcludeSelectors`, and
  `viewportNames`.
- Workspace and scenario only: `delay`, `noScrollTop`, and `requiredLogin`.
  The global `--required-login` flag can force the login flag.
- Workspace and suite only: `asyncCaptureLimit`, `asyncCompareLimit`,
  `browser`, `debug`, `ignoreSslErrors`, `viewportsPath`, `state`, and
  `urlReplacements`, plus `autoCleanRuns` and `keepTestRuns`.
- Workspace only: `freezeAnimations`, `elementTimeout`, `actionabilityTimeout`,
  `settleTimeout`, `scenarioTimeout`, `snapshotTitle`, `snapshotHeading`, and
  `snapshotSubheading`.
- Suite only: required `scenarios`, `displayName`, and `id`; `testDate` is
  runtime metadata that Regressify overwrites.
- Scenario only: required `url`, `id`, `label`, `description`, `actions`,
  `needs`, `restore`, `readySelector`, `readyTimeout`, `engineOptions`,
  `onBeforeScript`, `onReadyScript`, `selector`, `selectors`, and legacy
  selector shortcuts.

Inheritance exceptions:

- `basicAuth` entries for the same origin override inherited credentials; other
  origins remain inherited. Origin matching prevents sending credentials to an
  unrelated redirect or a proxy.
- URL replacements concatenate in this order: profile, then workspace, then suite.
  They are not an ordinary replacing list and are not a scenario-level setting.
- Suite `state` replaces workspace `state`; scenario `restore` adds names to that
  selected list in order, without duplicates.
- `logLevel` is overridden by CLI verbosity flags, then `REGRESSIFY_LOG_LEVEL`,
  before the file cascade. It changes display only, not archived collection.
- Snapshot command title/heading/subheading options override the corresponding
  workspace report text. Suite/scenario overrides do not apply to report text.
- Comparison thresholds and page URLs are recorded per capture when a run
  starts. Standalone `compare` reuses them, so threshold changes need a new
  capture before they affect results.

## Units and Defaults

- `browser`: `chromium`; Firefox and WebKit are unsupported.
- `asyncCaptureLimit`: 5; positive whole number, also bounded by the global
  `--async-capture-limit` (default 5), which ramps up one slot per second.
- `asyncCompareLimit`: 2; positive whole number.
- `autoCleanRuns`: false; opt-in cleanup after completed full-suite tests.
- `keepTestRuns`: 1; positive whole number, with a strict minimum of 1.
- `misMatchThreshold`: 0.1; allowed percentage of differing pixels, from 0 to
  100. A capture passes only when its mismatch is at most this value and its
  dimensions equal the reference.
- `pixelMatchThreshold`: 5; per-pixel color tolerance on a separate 0 to 100
  scale. Lower values flag subtler color changes.
- `delay`: 1000 milliseconds before each save checkpoint and final capture.
- `postInteractionWait`: 1000 milliseconds after actions,
  before final captures only.
- `lazyLoadScrollDelay`: 250 whole milliseconds minimum at each initial
  lazy-load scroll stop. Zero removes the pause, not visible resource checks.
- `lazyLoadTimeout`: 20000 positive whole milliseconds for the complete initial
  lazy-load pass and stable return to the top. Expiry fails the capture.
- `elementTimeout`: 30000 milliseconds for finding elements and frames,
  selector and URL waits, `goto`, `select`, `switchTab`, and hooks.
- `actionabilityTimeout`: 5000 milliseconds for visibility, stability,
  coverage, and enabled checks.
- `settleTimeout`: 5000 milliseconds of best-effort font, image, and layout
  settling before each capture; expiry is logged, not fatal.
- `scenarioTimeout`: 600000 milliseconds for a complete scenario/viewport capture.
- `freezeAnimations`: true; completes finite and pauses infinite CSS/Web
  Animations before actions and each capture.
- `useCssOverride`: true; applies the configured CSS override file.
- `imageSimplification`: `none`; opt-in `pixelate` simplifies large images just
  before each screenshot. See Image Simplification below.
- `imageSimplificationBlockSize`: 4; even whole CSS pixels from 2 to 16.
- `imageSimplificationMinSize`: 64; positive whole CSS pixels.
- `imageSimplificationExcludeSelectors`: empty list of standard CSS selectors.
- `debug`, `bypassCsp`, `ignoreSslErrors`, `noScrollTop`, `requiredLogin`:
  false. Headed debug mode is disabled in CI without a display.

Thresholds and timing values must be finite; do not use negative delays or zero
concurrency. `elementTimeout`, `actionabilityTimeout`, `lazyLoadTimeout`, and `scenarioTimeout` must
be positive whole milliseconds; zero is invalid. `elementTimeout` also bounds
injected browser hooks. `settleTimeout: 0` disables settling, not all waits or
animation freezing. A scenario's initial navigation timeout lives under
`engineOptions.gotoParameters.timeout`; omitted or zero means 60000 milliseconds,
independent of `elementTimeout`. `waitUntil` is `commit`, `domcontentloaded`,
`load` (default), or `networkidle`, which needs 500 ms without network activity
and may never arrive on continuously polling pages. The overall scenario
timeout still applies. Omitted/zero scenario `readyTimeout` inherits
`elementTimeout`. Use the hooks topic for script timing and troubleshooting for
interpreting timeout failures.

## Run History and Cleanup

Each output suite records runs in a private `runs.json`. Default Results,
Compare, Snapshot, and Deploy use its latest completed full-suite run. Website
errors and visual mismatches qualify as completed; cancelled runs and
infrastructure failures do not. Completed scenario-only runs remain explicitly
selectable without replacing the full-suite default. Suites complete independently.
Skipped comparisons are generated on demand when results or reports are requested.

`autoCleanRuns` and `keepTestRuns` resolve independently from suite to workspace.
A suite can set `autoCleanRuns: false` to override an enabled workspace policy.
Cleanup keeps the latest full-suite result first, then fills any remaining slots
with newer completed runs. Keeping only 1 may discard newer scenario-only runs;
automatic cleanup does not run after those partial selections. Reference baselines,
browser state, retained capture logs, and unselected suites remain protected.

`regressify clean --test-suite home --keep-runs 1` only previews removals. Add
`--yes` only with authorization to delete the displayed run artifacts. Repeated
suite filters and `--test-suite all` are supported, but an explicit filter is
required. Cleanup is limited to the selected output folder and refuses active
artifact owners. Registries and managed deployment staging are local-only and
must not be uploaded as report content.

## Slow Lazy-Loaded Content

Initial scrolling runs before actions, moves 80% of a viewport at a time, and
waits for visible image loading/decoding and common deferred source/loading
markers, including picture sources and LazySizes backgrounds. It rechecks page
height after each stop and excludes hidden or clipped slides. It does not
replay scrolling before saves or final captures. `noScrollTop: true` skips it.

Both lazy-load settings inherit independently from workspace to suite to
scenario. For a slow page, this complete suite overrides a workspace's defaults:

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

The configured timeout covers pauses, resources, and the return to the top;
150 scroll steps also bound endlessly growing pages. `scenarioTimeout` remains
the outer limit. Custom loaders or CSS backgrounds without observable loading
state may need an awaited browser ready hook. Increasing best-effort
`settleTimeout` alone cannot activate an untouched placeholder. Recapture and
review affected references as well as test images; rerunning comparison cannot
repair existing PNGs.

## Image Simplification

Image simplification is opt-in. `imageSimplification: pixelate` replaces each
qualifying image in the selected tab's main document with uniform blocks of its
average color just before every screenshot, including `save` checkpoints.
Qualifying images are `img`, `video`, and SVG `image` elements rendered at least
`imageSimplificationMinSize` CSS pixels wide and high. These stay sharp: images
matching an `imageSimplificationExcludeSelectors` entry or inside a matching
element; images that already have a CSS `filter`, or a border, padding,
rounded corners, a box shadow, an outline, a clip path, or a mask; scaled,
zoomed, skewed, or fractionally moved images, images rotated other than in
quarter turns, and images rotated a quarter turn while their width and height
differ by an odd number of pixels; SVG `image` elements not placed at `x` and
`y` 0 or under a fractional SVG translation; and images whose simplification
could change which content paints on top, such as overlaid text or badges. Iframes, shadow DOM,
canvas, and CSS background images are unchanged. Exclusion selectors that the
browser rejects match nothing. Images are flagged only while each screenshot is
taken. Write `none`, not `off`; YAML 1.1 tools read an unquoted `off` as `false`.

Each key inherits independently from workspace to suite to scenario. This
complete suite pixelates its scenarios but keeps one scenario's images sharp:

```yaml
id: catalog
imageSimplification: pixelate
imageSimplificationBlockSize: 8
imageSimplificationExcludeSelectors:
  - '#hero img'
scenarios:
  - id: products
    url: https://example.com/products
  - id: product-photo
    url: https://example.com/products/sample
    imageSimplification: none
```

Recommend it only with the user's agreement: enabling it or changing any
image simplification key changes pixels, so affected references must be
re-captured, and reference and test captures must use identical values. Each
capture records its setting, and comparisons warn without failing when they
differ. Keep `pixelMatchThreshold` above 0 for pixelated scenarios; inside
uniform blocks anti-alias filtering cannot forgive pixels. Do not use it to
hide a regression in the images under test; exclude those images instead. Odd
or out-of-range block sizes, `imageSimplificationMinSize: 0`, and blank
exclusion selectors fail before any browser starts.

## Files, Replacements, and Credentials

`cookiePath`, `cssOverridePath`, `jsOnReadyPath`, `viewportsPath`,
`onBeforeScript`, and `onReadyScript` resolve from the workspace root, not from
the suite's folder. Explicit configured paths must
exist as regular files inside the canonical workspace, including after symlink
resolution. An absolute path is allowed only if it remains inside that workspace.
Missing optional defaults differ from explicit missing paths; consult the files
guide before assuming a default file is required.

`ref` captures raw reference URLs. `test` applies URL replacement rules in order,
including supported navigation and URL-wait targets. Each rule has `ref` and
`test` strings; literal rules replace every occurrence, and `regex: true` enables
Rust regular expressions. Optional flags are `g`, `i`, `m`, `s`, and `u`;
invalid/repeated flags or flags without regex fail. Do not assume
JavaScript-only regex features such as look-around or backreferences work. In
`test`, write capture groups as `${1}` when a letter, digit, or underscore
follows, because `$1a` names a group `1a`.
`--replacement-profile` overrides `REPLACEMENT_PROFILE`; otherwise the `default`
profile applies when present. An explicitly selected profile, or its file, must
exist; an absent implicit `default` simply adds no rules. `ref` ignores the
profile option with a warning.

For HTTP Basic Auth, supply an object or array of objects with `origin`,
`username`, and `password`. Use `${VARIABLE}` or `$VARIABLE` placeholders,
resolved locally at execution; `$NAME` directly after a letter, digit, or
underscore stays literal. Missing/empty environment values fail validation.
Basic Auth is the only configuration with environment substitution: URLs,
cookies, and action values are literal. Never commit literal passwords or send
resolved values to the MCP server. Cookies, persisted state, and Basic Auth are
separate mechanisms; none is an automatic application-specific login workflow.
