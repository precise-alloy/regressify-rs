---
title: Create a test suite
guid: 4e46191b-a1e4-4d2d-82ff-5ec3827a33cb
description: Learn how to create Regressify test suites, group related scenarios, and configure suite-level options for stable visual regression testing.
date: 2023-06-29T17:00:00+00:00
updated: 2026-10-26T00:00:00+00:00
draft: false
hidden: false
weight: 20000
sort_by: weight
template: docs/page.html

extra:
  toc: true
  top: false
---

A test suite is a collection of [test scenarios][create-test-scenario] that are grouped together based on your decision. You have the flexibility to determine which scenarios should be included in the same test suite. For example, a test suite can encompass test cases for the entire Home page, including different blocks, or focus solely on the Header block with various states (e.g., closed/opened menu).

In one of my projects, the objective was to assist customers in migrating multiple websites from an old platform to a newer one. To ensure that everything was functioning correctly, I created dedicated test suites for each website. This allowed the QA engineer to test any individual website as needed.

At a minimum, a test suite contains a list of URLs to be tested. For example:

```yaml
scenarios:
  - url: https://alloy.eshn.dev/pages/home.html
  - url: https://alloy.eshn.dev/pages/service.html
  - url: https://alloy.eshn.dev/pages/portfolio.html
  - url: https://alloy.eshn.dev/pages/contact/default.html
```

The purpose of these tests is to load the specified URLs and capture screenshots. For more complex test cases, you can utilize a combination of the properties provided below.

Configuration is validated before browser capture starts. Malformed suite files,
invalid viewport names, invalid replacement regexes, unsupported browser
settings, out-of-range mismatch thresholds, negative or non-finite timing
values, and invalid image simplification settings stop the command early. Suite
and scenario IDs must also be portable
single path components no longer than 80 bytes. Viewport labels must produce
unique screenshot suffixes no longer than 48 bytes. Website failures during
scenario execution are written to the reports instead of stopping sibling
scenarios.
IDs cannot have leading or trailing whitespace, trailing dots, control
characters, or Windows-reserved device names such as `CON` and `LPT1`.
Suite names and suite output IDs also cannot start with a dot or use `all`,
`_snapshot`, `_diagnostics`, `_landing`, or `states` (case-insensitive). These
restrictions do not apply to ordinary helper/data filenames under `common`.

Before capture, Regressify also resolves every scenario, viewport, saved step,
and selector target to its actual screenshot filename. Any collision, including
a collision that differs only by letter case, is a configuration error.

Unknown workspace, suite, scenario, viewport, URL replacement, and action
properties also fail parsing. Use the documented camelCase property names so a
typo cannot silently change a capture.

An explicit `cookiePath`, `cssOverridePath`, `jsOnReadyPath`, `onBeforeScript`, or
`onReadyScript` must name an
existing regular file inside the workspace; malformed cookie YAML also fails
before Chromium launches. The default optional `common/_cookies.yaml`,
`common/_override.css`, and `common/_on-ready.js` files may be absent, but are
validated when present.
CSS and all JavaScript hooks are injected directly into Chromium, without Node.js
or Playwright. After readiness, CSS and `jsOnReadyPath` run before the scenario's
`onReadyScript`; both scripts finish before actions and run once per selected
document. See [browser script timing and context](create-scenario.md).
Configured persisted `state` and scenario `restore` files are likewise required
to exist in the active Regressify output directory and must have valid state
data before Chromium launches.

## Properties

### Diagnostic Display

Set `logLevel: trace|debug|info|warn|error` to control diagnostic display for a
suite. Scenarios can override it; the workspace provides an inherited default.
CLI flags and `REGRESSIFY_LOG_LEVEL` take precedence. Display filtering never
changes the archived capture timeline. See [diagnostic verbosity](commands.md#diagnostic-verbosity).

### Run Retention

`autoCleanRuns` enables automatic cleanup after completed full-suite tests;
`keepTestRuns` is the maximum completed runs to retain and must be at least 1.
Both inherit workspace values; their defaults are `false` and `1`. A suite can
set `autoCleanRuns: false` to opt out of an enabled workspace policy. These
settings do not cascade to scenarios. The latest completed full-suite run and
all reference baselines remain protected. See [run cleanup](commands.md#run-history-and-cleanup)
for partial-run handling, diagnostic ownership, and manual previews.

### Display Name

Set the optional `displayName` string to label a suite in the desktop app and
snapshot website independently of its filename and `id`:

```yaml
displayName: Customer checkout
scenarios:
  - url: https://example.com/checkout
```

Blank or omitted names fall back to the suite name. Display names do not rename
files, output directories, capture keys, CLI suite filters, or report routes.
The desktop exposes this value in Fields > General and in the suite Rename
dialog. Desktop saves update existing local report labels; CLI-authored changes
are recorded on the next reference or test capture. Redeploy published sites to
make updated labels visible remotely.

### HTTP Basic Authentication

`basicAuth` is supported in workspace, suite, and scenario configuration.
Use an object for one origin or an array for several origins. More specific
levels replace credentials for the same origin and inherit other origins.

```yaml
basicAuth:
  - origin: https://staging.example.com
    username: ${REGRESSION_USER}
    password: ${REGRESSION_PASSWORD}
```

`${NAME}` and `$NAME` resolve from environment variables. Missing/empty variables,
credentials, or invalid HTTP(S) origins are configuration errors. Credentials are
sent only in response to a Basic challenge from a matching server origin, never
to a proxy or an unrelated redirect origin. A failed credential challenge is not
retried indefinitely. Do not commit literal passwords in suite files.

### Required properties

{% spoiler() %}

scenarios

:::::

Type: `ScenarioModel[]`.

List of test scenarios. Refer to [create a scenario](@/optimizely-cms/testing/create-scenario.md) for more information.

{% end %}

### Optional properties

{% spoiler() %}

asyncCaptureLimit

:::::

Type: `number`.  
Default value: `5`.

This parameter determines the maximum number of screens from this suite captured
in parallel during testing. The global `--async-capture-limit` remains a cap
across all suites, so the effective limit is the lower of the two. Increasing
this value can accelerate a suite, but a high value can make websites and
network connections less stable.

{% end %}

{% spoiler() %}

asyncCompareLimit

:::::

Type: `number`.  
Default value: `2`.

This parameter determines the maximum number of CPU-bound image comparisons run
in parallel. Keep it low for large full-page screenshots because each worker
decodes both images and may hold a diff image in memory.

{% end %}

{% spoiler() %}

browser

:::::

Type: `string`.  
Default value: `chromium`.  
Allowed value: `chromium`.

The browser used for the test. The Rust CLI currently uses Chromium through the Chrome DevTools Protocol. `firefox` and `webkit` are rejected as configuration errors until those backends are implemented.

{% end %}

{% spoiler() %}

debug

:::::

Type: `boolean`.  
Default value: `false`.

Should the tool display the Chromium window as tests are running. This will allow you to visually see your app state at the time of your test. The global `--debug` flag also enables headed mode.

> Debug (headed) mode is automatically disabled in CI environments (Azure Pipelines, GitHub Actions, or `CI=true`). CI agents have no display, so the browser always runs headless there even when `debug: true` is set; otherwise the launch would fail with `Missing X server or $DISPLAY`.

Example:

```yaml
debug: true
```

{% end %}

{% spoiler() %}

cssOverridePath

:::::

Type: `string`.

CSS override path.  
Default value: **common/_override.css**

{% end %}

{% spoiler() %}

cookiePath

:::::

Type: `string`.
Default value: `common/_cookies.yaml`.

Path to a YAML cookie file. Relative paths are resolved from the project root and must stay inside the project. Cookies are loaded before the initial page navigation so they can affect the first request.

{% end %}

{% spoiler() %}

hideSelectors

:::::

Type: `string[]`.  
Default value: `[]`.

Array of selectors set to `visibility: hidden`.  
This property will affects all the scenarios within the same test suite.

{% end %}

{% spoiler() %}

ignoreSslErrors

:::::

Type: `boolean`.  
Default value: `false`.

Whether the tool should ignore SSL errors.

{% end %}

{% spoiler() %}

imageSimplification

:::::

Type: `none | pixelate`.  
Default value: workspace value, then `none`.

Opt-in image simplification for image-heavy pages. With `pixelate`, each
qualifying image in the page's main document is replaced by uniform blocks of
its average color immediately before every screenshot, including `save`
checkpoints. Screenshots of photo-heavy pages become much smaller and minor
image encoding or scaling differences shrink, while text, layout, and other
content stay sharp. `none` captures images unchanged. Write `none`, not `off`:
YAML 1.1 tools read an unquoted `off` as `false`.

Qualifying images are `img`, `video` (including its poster), and SVG `image`
elements rendered at least `imageSimplificationMinSize` CSS pixels wide and
high. Blocks start at each image's top-left corner; blocks cut off by the right
or bottom edge use their own average. These images stay sharp:

- images that match `imageSimplificationExcludeSelectors` or sit inside an
  element that matches one;
- images that already have a CSS `filter`;
- images that are scaled, zoomed, skewed, rotated other than in quarter turns,
  or moved by a fraction of a pixel, because that would blur the blocks;
- images whose simplification could change which content paints on top of
  them, such as caption text, a badge, or a sticky header drawn over the image.

Iframes, shadow DOM, canvas, and CSS background images are not simplified.

Images are flagged with a `data-regressify-simplify` attribute only while a
screenshot is taken; the flags are removed before later actions run. Each page
document also gets a hidden zero-size SVG element holding the filter and one
constructed stylesheet. Neither changes layout, and both work under a strict
Content Security Policy. The capture log counts simplified images and the reason
each other image stayed sharp, without recording selectors or image URLs.

Enabling it, or changing any image simplification setting, changes captured
pixels. Re-capture references afterwards and keep reference and test captures
on identical settings. Each capture records its setting, and comparisons warn,
without failing, when a reference and its test capture differ; see
[image simplification](commands.md#image-simplification). Keep
`pixelMatchThreshold` above `0` for pixelated scenarios: inside uniform blocks,
anti-alias filtering cannot forgive a pixel, so the color tolerance is the only
allowance there.

Scenario values override this suite value. Each image simplification setting
resolves independently: scenario, suite, workspace, then the default.

```yaml
imageSimplification: pixelate
imageSimplificationBlockSize: 8
scenarios:
  - url: https://example.com/gallery
```

{% end %}

{% spoiler() %}

imageSimplificationBlockSize

:::::

Type: even whole CSS pixels from `2` to `16`.  
Default value: workspace value, then `4`.

Width and height of each uniform block used by `imageSimplification: pixelate`.
Larger blocks remove more detail and produce smaller screenshots. Odd or
out-of-range values are configuration errors; Chromium limits the averaging
kernel to 16 pixels.

{% end %}

{% spoiler() %}

imageSimplificationExcludeSelectors

:::::

Type: `string[]`.  
Default value: workspace value, then `[]`.

Standard CSS selectors for images, or for elements containing images, that stay
sharp during image simplification, such as a product photo under review or the
gallery around it. A selector the browser rejects matches nothing, like an
invalid `hideSelectors` entry, and the capture log counts it without recording
its text. Blank entries are configuration errors. A scenario list replaces this
suite list, and this list replaces the workspace list; lists are never merged.

{% end %}

{% spoiler() %}

imageSimplificationMinSize

:::::

Type: positive whole CSS pixels.  
Default value: workspace value, then `64`.

Images rendered narrower or shorter than this size stay sharp during image
simplification, so icons, logos, and avatars remain recognizable. `0` is a
configuration error.

{% end %}

{% spoiler() %}

lazyLoadScrollDelay

:::::

Type: non-negative whole milliseconds. Default: workspace value, then `250`.

Minimum pause at each stop during the initial lazy-load scroll, before actions.
Visible image loading/decoding and common lazy-loader states are also checked;
`0` removes only the minimum pause. Scenario values override the suite value.
Ignored when `noScrollTop` resolves to `true`.

{% end %}

{% spoiler() %}

lazyLoadTimeout

:::::

Type: positive whole milliseconds. Default: workspace value, then `20000`.

Total budget for initial lazy loading and returning to the top. Increase it with
`lazyLoadScrollDelay` for slow or long pages. Expiry fails the capture; unlike
`settleTimeout`, it is not best-effort. Scenario values override the suite value.
Zero is invalid; use `noScrollTop` to skip initial scrolling.

{% end %}

{% spoiler() %}

misMatchThreshold

:::::

Type: `number`.  
Default value: `0.1`.  
Min value: `0`.  
Max value: `100`.

This parameter represents the percentage of permissible differing pixels for a test to pass. A lower value ensures more precise test results. The choice of a suitable value depends entirely on your project's specific requirements.

It controls pass/fail, not which pixels are highlighted. Use `pixelMatchThreshold`
to adjust sensitivity to color differences.

{% end %}

{% spoiler() %}

pixelMatchThreshold

:::::

Type: `number`.
Default value: Workspace value, otherwise `5`.
Min value: `0`.
Max value: `100`.

Per-pixel color tolerance. Lower values highlight subtler background and other
color changes; higher values ignore more rendering differences. This is separate
from the percentage of mismatched pixels permitted by `misMatchThreshold`.
The comparer divides the value by `100`, so `5` is a normalized tolerance of `0.05`.

Scenario values override this suite value. The desktop field uses `0.1` steps.
An explicit `0` removes color tolerance, while anti-alias filtering remains active
for equal-size images. Resolved values are saved with each capture and reused by
standalone `compare`; configuration changes apply to new captures.

```yaml
pixelMatchThreshold: 3
scenarios:
  - url: https://example.com
```

{% end %}

{% spoiler() %}

postInteractionWait

:::::

Type: `number`.  
Default value: `1000`.  

Wait for X milliseconds after actions and before the final screenshot. Scenario values override test-suite values, which override workspace values, which fall back to `1000`.

{% end %}

{% spoiler() %}

removeSelectors

:::::

Type: `string[]`.  
Default value: `[]`.

Array of selectors set to `display: none`.  
This property will affects all the scenarios within the same test suite.

{% end %}

{% spoiler() %}

state

:::::

Type: `string | string[]`.  
Default value: Empty.

Loads a previously saved browser state before running tests. This should be used in conjunction with the [persist action][action-persist]. State files are stored under the active output directory, for example `.regressify/states/user-aaaa.json` or `.regressify__staging/states/user-aaaa.json` when `--site-env staging` is used.

Persisted cookies are restored before the first navigation. Persisted local storage is restored after the page origin exists, then the page is reloaded so application startup code can observe it.

Example:

```yaml wrap
state: user-aaaa
```

{% end %}

{% spoiler() %}

urlReplacements

:::::

Type: `ReplacementModel[]`.  
Default value: `[]`.

Find/replace for URLs in the `test` command. Invalid regex replacements are treated as configuration errors before scenarios start.
Set `regex: true` to use a regular expression. Supported JavaScript-style
flags are `g` (global replacement), `i` (case-insensitive), `m` (multiline),
`s` (dot-all), and `u` (Unicode); flags require `regex: true` and may not be
repeated.

Example:

```yaml
# Below settings have no effect when running under `ref` command.
# When running using `test` command,
# replace the URL from `https://legacy1.domain.com/` to `https://new1.domain.com/`,
# and from `https://legacy2.domain.com/` to `https://new2.domain.com/`
urlReplacements:
  - ref: https://legacy1.domain.com/
    test: https://new1.domain.com/
  - ref: https://legacy2.domain.com/
    test: https://new2.domain.com/
```

{% end %}

{% spoiler() %}

useCssOverride

:::::

Type: `boolean`.  
Default value: `true`.

Should the scenarios use CSS override.

{% end %}

{% spoiler() %}

viewportNames

:::::

Type: `string | string[]`.  
Default value: all available viewports.

Choose the viewports you want applied for the test suite.
Unknown viewport names are configuration errors. Viewport labels are matched case-insensitively.

Sample:

```yaml hl_lines=4-6
viewportNames:
  - tablet
  - desktop
```

{% end %}

{% spoiler() %}

viewportsPath

:::::

Type: `string`.  
Default value: `common/_viewports.yaml`.

Path to the viewport definition file. Each viewport must have a non-empty label and non-zero `width` and `height`. Duplicate labels are rejected case-insensitively.
An explicit path must name an existing regular file inside the workspace. The
default file may be absent, in which case captures use Chromium's default
viewport.

{% end %}

[create-test-scenario]: @/optimizely-cms/testing/create-scenario.md
[action-persist]: @/optimizely-cms/testing/create-action.md#spoiler--persist
