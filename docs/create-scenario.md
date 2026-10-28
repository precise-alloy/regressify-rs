---
title: Create a Test Scenario
description: Learn how to define Regressify test scenarios, set target URLs, add actions, and tune per-scenario options for reliable runs.
guid: 620b9098-627b-4db5-bb0b-cd43b9949e35
date: 2023-06-29T17:00:00+00:00
updated: 2026-10-26T00:00:00+00:00
draft: false
hidden: false
weight: 30000
sort_by: weight
template: docs/page.html

extra:
  lead: A scenario tells Regressify which page to open, what to do before capture, and which settings apply only to that run. Use this reference when a suite needs more than a plain list of URLs and starts behaving like an actual test case.
  toc: true
  top: false
---

Each scenario describes one path through a page, from the first URL load to any clicks, waits, or overrides needed before the screenshot is captured.

Scenario `viewportNames` resolves through the existing scenario, suite, workspace,
and default settings to choose capture jobs. Individual actions may also declare
`viewportNames`, restricting only that step within those jobs. Omitted, `null`,
and empty action filters run everywhere; nonempty filters must name only effective
scenario viewports, including when the action comes from `needs`. An automatic
final screenshot is skipped when the last applicable scenario action is `save`;
explicit `selectors` are still captured. See [action viewport selection](create-action.md#viewport-selection)
for the responsive-menu example, conditional saves, and shared-state caveat.

The optional `logLevel` setting accepts `trace`, `debug`, `info`, `warn`, or
`error`. It overrides suite/workspace diagnostic display settings, but CLI flags
and `REGRESSIFY_LOG_LEVEL` take precedence. It never filters archived records;
see [diagnostic verbosity](commands.md#diagnostic-verbosity).

## Properties

### Required properties

{% spoiler() %}

url

:::::

Type: `string`.

The URL you want to test.
Initial navigation waits for `engineOptions.gotoParameters.waitUntil` (default
`load`) within `engineOptions.gotoParameters.timeout` (default `60000` ms),
independent of `elementTimeout`. Saved local storage is restored before page
scripts run, without a reload.

{% end %}

### Optional properties

{% spoiler() %}

actions

:::::

Type: array of actions.  
Default value: `[]`.

Provide the list of actions that the test browser should execute in sequence. Available options are:

| Action    | Use for                                                  |
| --------- | -------------------------------------------------------- |
| check     | Check a radio or checkbox                                |
| click     | Click an element                                         |
| focus     | Focus an element                                         |
| goto      | Navigate to a URL                                        |
| hide      | Hide an element - set `visibility: hidden !important`    |
| hover     | Hover an element                                         |
| input     | Type text in an element or upload files                  |
| persist   | Save cookies and local storage for later scenarios       |
| press     | Press a key (or key combination) on an element           |
| remove    | Remove an element - set `display: none !important`       |
| save      | Capture a named full-page or selector screenshot         |
| scroll    | Scroll to an element                                     |
| select    | Select an option inside a `select` element               |
| switchTab | Select a new, previous, or original browser tab          |
| uncheck   | Uncheck a radio or checkbox                              |
| wait      | Wait for X milliseconds or wait for an element to appear |

For more information, please refer to [Create an action][create-an-action].

A scenario can have multiple actions. Sample:

```yml
actions:

  # Check the checkbox with `tos-accept` ID
  - check: checkbox#tos-accept

  # Click the button with `submit` class
  - click: button.submit

  # Focus an element with `username` ID
  - focus: '#username'

  # Hide the element with `cookie-modal` ID
  - hide: '#cookie-modal'

  # Hover mouse over an element contains `my-chart` ID
  - hover: '#my-chart'

  # Type `My Name` in an input element with `username` ID
  - input: 'input#username'
    value: My Name
    append: false

  # Type `my-name@domain.com` in an input element with `email` ID
  - input: 'input#email'
    value: my-name@domain.com
    append: false

  # Select all content inside an element with `text-area` ID
  - press: '#text-area'
    key: Ctrl+A

  # Remove a `div` element with `random-content` class
  - remove: div.random-content

  # Scroll to an element with `report` class
  - scroll: .report

  # Uncheck the checkbox named `subscribe-email`
  - uncheck: checkbox[name="subscribe-email"]

  # Delay 1 second
  - wait: 1000

  # Wait for an element with `popup` class appears
  - wait: .popup
```

Applicable actions execute in the order listed. An explicit `actions` list,
including `[]`, takes precedence over legacy selector shortcuts.
`postInteractionWait` still applies after actions.

When a click opens a new tab, use `switchTab: new` before waiting for or capturing
that destination. `_blank` links and JavaScript popups keep their normal browser
behavior. Subsequent actions and screenshots use the selected tab; `previous`
and `original` return to earlier tabs. See [tab actions](create-action.md).

When `actions` is omitted, legacy `clickSelector`/`clickSelectors`,
`hoverSelector`/`hoverSelectors`, `keyPressSelector`/`keyPressSelectors`, and
`scrollToSelector` are converted into unfiltered actions. Prefer explicit actions
for new suites, especially when interactions differ by viewport.

{% end %}

{% spoiler() %}

cssOverridePath

:::::

Type: `string`.  
Default value: Test suite's value.

{% end %}

{% spoiler() %}

cookiePath

:::::

Type: `string`.  
Default value: Test suite's value, then workspace value, then `common/_cookies.yaml`.

Import cookies in YAML format. Cookies are loaded before the first navigation
and the path must stay inside the project root. Each cookie needs a non-empty
`name`, a string `value`, and either `url` or `domain`. `domain` may be a string
or a non-empty array of strings; arrays create one cookie per domain while
preserving the other attributes. Optional `secure` and
`httpOnly` values must be booleans; `sameSite` is `Strict`, `Lax`, or `None`.
`SameSite: None` requires `secure: true`.

The combined UTF-8 byte length of each cookie's `name` and `value` must not
exceed Chromium's 4,096-byte limit. Values must not contain CR, LF, or NUL
characters. A YAML `|` or `>` block normally adds a trailing newline; use a
quoted single-line value or `|-` containing one line instead. These errors
are reported before browser launch, with the file, cookie, and reason but
without the cookie value.

For chunked authentication cookies such as `.AspNetCore.Cookies`, export
the base cookie and every numbered chunk (`.AspNetCore.CookiesC1`,
`.AspNetCore.CookiesC2`, etc.) as separate entries exactly as stored in the
browser. Re-export an oversized cookie; do not concatenate, truncate, or
manually split its value. Regressify does not modify authentication values.

```yaml
- name: session
  value: example-token
  domain: .example.com
  path: /
  secure: true
  httpOnly: true
  sameSite: Lax
```

{% end %}

{% spoiler() %}

delay

:::::

Type: `number`.  
Default value: `1000`.

Resolution order: scenario, workspace, then the default. Set `delay: 0` to disable this wait.

Wait for X milliseconds before each `save` checkpoint and before the scenario's final captures. This is separate from `postInteractionWait`, which applies only after the actions finish.  
Use this for scenarios where the page under test needs extra time to load or render.

{% end %}

{% spoiler() %}

hideSelectors

:::::

Type: `string[]`.  
Default value: Test suite's value, then workspace value, then `[]`.

Standard CSS selectors set to `visibility: hidden` in every document of the
scenario, before actions and captures. Hidden elements keep their layout space,
actions cannot interact with them, and selectors that match nothing are ignored.
A scenario list replaces the inherited list. Use the [`hide` action][action-hide]
instead when only a later step should hide an element.

{% end %}

{% spoiler() %}

id

:::::

Type: `string`.  
Default value: a slug of `label`, or a hash of `url` when no label is provided.

Scenario ID. This property is for identifying this scenario when you re-use in other one. Use any value you find meaningful, but should be as short and concise as possible.  
IDs must be a portable single path component: they cannot be empty or contain
path separators, control characters, or Windows-reserved filename characters.
Leading or trailing whitespace and trailing dots are also rejected.
Lowercase alphanumeric characters (`a-z`, `0-9`), minus (`-`), and underscore
(`_`) are recommended.

IDs must be unique within each suite, ignoring letter case. Scenarios sharing a
URL need distinct explicit IDs or labels that produce different ID slugs.
Different suites may reuse the same URLs and scenario IDs.

Duplicate-ID errors identify the suite and source file, list both conflicting
scenario numbers (starting at 1), labels when supplied, and URLs, and suggest
assigning unique IDs. Only URL passwords are redacted; usernames, query strings,
fragments, and other URL information remain visible in the diagnostic.

Use together with the `needs` property.

Example:

```yaml
scenarios:
  - url: https://www.domain.com
    id: first-visit
  - url: https://www.domain.com
    id: return-visit
```

{% end %}

{% spoiler() %}

imageSimplification

:::::

Type: `none | pixelate`.  
Default value: Test suite's value, then workspace value, then `none`.

Opt-in image simplification for this scenario. With `pixelate`, each qualifying
image in the page's main document is replaced by uniform blocks of its average
color immediately before every screenshot, including `save` checkpoints. Text,
layout, and other content stay sharp. Use `none` to turn off an inherited
`pixelate`; write `none`, not `off`, because YAML 1.1 tools read an unquoted
`off` as `false`.

Qualifying images are `img`, `video` (including its poster), and SVG `image`
elements rendered at least `imageSimplificationMinSize` CSS pixels wide and
high. Blocks start at each image's top-left corner; blocks cut off by the right
or bottom edge use their own average. Images stay sharp when they match
`imageSimplificationExcludeSelectors` or sit inside an element that matches one,
already have a CSS `filter`, are scaled, zoomed, skewed, rotated other than in
quarter turns, or moved by a fraction of a pixel, or when simplification could
change which content paints on top of them, such as caption text, a badge, or a
sticky header drawn over the image. Iframes, shadow DOM, canvas, and CSS
background images are not simplified.

Images carry a `data-regressify-simplify` attribute only while a screenshot is
taken, so later actions see the unmodified page. Each page document also gets a
hidden zero-size SVG element holding the filter and one constructed stylesheet;
neither changes layout. The capture log counts simplified images and the reason
each other image stayed sharp, without recording selectors or image URLs.

Enabling it, or changing any image simplification setting, changes captured
pixels. Re-capture this scenario's references afterwards and keep reference and
test captures on identical settings. Each capture records its setting, and
comparisons warn, without failing, when a reference and its test capture differ;
see [image simplification](commands.md#image-simplification). Keep
`pixelMatchThreshold` above `0` for pixelated scenarios: inside uniform blocks,
anti-alias filtering cannot forgive a pixel, so the color tolerance is the only
allowance there.

Each image simplification setting resolves independently: scenario, suite,
workspace, then the default.

```yaml
scenarios:
  - url: https://example.com/products
    imageSimplification: pixelate
    imageSimplificationExcludeSelectors:
      - '#hero img'
    pixelMatchThreshold: 5
```

{% end %}

{% spoiler() %}

imageSimplificationBlockSize

:::::

Type: even whole CSS pixels from `2` to `16`.  
Default value: Test suite's value, then workspace value, then `4`.

Width and height of each uniform block used by `imageSimplification: pixelate`.
Larger blocks remove more detail and produce smaller screenshots. Odd or
out-of-range values are configuration errors reported before any browser
starts; Chromium limits the averaging kernel to 16 pixels.

{% end %}

{% spoiler() %}

imageSimplificationExcludeSelectors

:::::

Type: `string[]`.  
Default value: Test suite's value, then workspace value, then `[]`.

Standard CSS selectors for images, or for elements containing images, that stay
sharp during image simplification, such as the product photo a scenario reviews.
A selector the browser rejects matches nothing, like an invalid `hideSelectors`
entry, and the capture log counts it without recording its text. Blank entries
are configuration errors. A scenario list replaces the inherited list; lists are
never merged, so use `[]` to clear inherited exclusions.

{% end %}

{% spoiler() %}

imageSimplificationMinSize

:::::

Type: positive whole CSS pixels.  
Default value: Test suite's value, then workspace value, then `64`.

Images rendered narrower or shorter than this size stay sharp during image
simplification, so icons, logos, and avatars remain recognizable. `0` is a
configuration error.

{% end %}

{% spoiler() %}

jsOnReadyPath

:::::

Type: `string`.  
Default value: Test suite's value.

Defaults to `common/_on-ready.js` when no level supplies a path. All scripts are
standard browser JavaScript injected directly into Chromium; Node.js and
Playwright are not required. CSS overrides and hide/remove rules apply first,
then `jsOnReadyPath`, then the optional scenario `onReadyScript`. Both ready
scripts run after DOM readiness and `readySelector`, before dependency, scenario,
or legacy actions. They apply once per document, including navigation and tab
selection, before the next action or screenshot. Files are loaded once per
scenario capture. Returned promises are awaited; errors fail that scenario.

Use `onReadyScript` or `jsOnReadyPath` to dismiss an approved cookie banner before
interaction. Paths are relative to the workspace and must stay inside it:

```javascript
document.querySelector("#cookie-banner")?.remove();
```

Replace the selector with a verified target. For asynchronous work, use an async
immediately invoked function that returns its promise. Scripts are evaluated as
written; merely declaring a function does not invoke it. `elementTimeout` bounds
script execution and the overall `scenarioTimeout` still applies.

Optional `onBeforeScript` is also browser JavaScript. It runs at the start of
the initial top-level document, before site scripts; the DOM may not exist yet.
Its returned promise is awaited before readiness/actions, but does not block the
site's own scripts. It does not repeat on subsequent `goto` actions or frames.

Capture context is available as `window.regressify` and `window.__REGRESSIFY__`:
scenario identity, initial capture URL, resolved `requiredLogin`, `isReference`,
`viewport` (or null without a named viewport), and `config` with settings such as
`elementTimeoutMs`. Action values and authentication credentials are omitted.
Use `window.location.href` for the current URL.

Existing Node/Playwright hooks must be rewritten as DOM code. Module exports,
`require`, `process.env`, filesystem APIs, and Playwright `page` objects are not
available. There is no Node fallback. The standalone MCP `hooks` topic and
`before-script`, `scenario-ready-script`, and `hooked-suite` examples cover the
full lifecycle.

After `switchTab`, document hooks apply to the selected page. Returning to an
existing document does not repeat its
completed document hooks.

`readySelector` waits for a visible selector before on-ready scripts and actions. A positive `readyTimeout` overrides its timeout; omitted or zero inherits `elementTimeout`. Do not wait here for content created by later scripts or actions. `engineOptions.gotoParameters` accepts `waitUntil: commit | domcontentloaded | load | networkidle`, a `timeout` in milliseconds, and `referer`. Initial navigation defaults to `load` with a `60000` ms timeout, matching the legacy Node/VS Code runner. Omitted or zero timeout uses this bounded default; a positive timeout overrides it. Navigation is independent of `elementTimeout`, which still defaults to `30000` ms for element waits. The overall `scenarioTimeout` still applies. Workspace `elementTimeout`, `actionabilityTimeout`, and `scenarioTimeout` must be positive whole milliseconds.

Legacy `keyPressSelector`/`keyPressSelectors` (`{selector, keyPress}`), `hoverSelector`/`hoverSelectors`, `clickSelector`/`clickSelectors`, and `scrollToSelector` are accepted when `actions` is omitted. They execute in that order through the same validated action engine. Prefer `actions` for new scenarios; an explicit action list takes precedence over all shortcuts.
Despite its name, legacy `keyPress` appends literal text. Use `input` with `value`
and `append: true` for that behavior, or `press` with `key` for Enter, Tab, and
other keyboard events. An explicit empty `actions: []` also disables shortcuts.

{% end %}

{% spoiler() %}

label

:::::

Type: `string`.

An explicit, nonblank label is used as the scenario's display name. When it is
omitted, the desktop app and snapshot web reports share URL-based naming rules:

- Use the shortest unique path suffix within the suite. For example,
  `/men/running-shoes` and `/women/running-shoes` become `Men / Running shoes`
  and `Women / Running shoes`, while `/help-center` becomes `Help center`.
- Omit the origin when all pages share it; otherwise include the scheme, host,
  and nondefault port, without URL credentials.
- Decode readable path text, turn hyphens/underscores into spaces, ignore trailing
  slashes and common index filenames, and call the root page `Home`.
- Use query or fragment differences only when paths still collide. Common
  tracking and credential parameter names are excluded. Remaining duplicates
  receive numbered suffixes, and unusually long generated names are shortened.

Names are resolved against the full suite before scenario filtering and test URL
replacements, then recorded with captures. Viewports and saved captures of the
same scenario share its name. Older reports use the URLs and scenarios available
in that report when no recorded display name exists.

Automatic display names never change scenario IDs, capture filenames, dependency
references, or the suite source. The existing identity rules still apply: without
an explicit `id`, a provided label supplies the ID slug, otherwise the original
URL supplies its hash. Scenarios with the exact same URL and no labels still need
distinct explicit IDs; display-name disambiguation does not change ID validation.

Example:

```yaml
scenarios:
  - url: https://www.domain.com
    label: check-login-with-valid-credentials
```

{% end %}

{% spoiler() %}

lazyLoadScrollDelay

:::::

Type: non-negative whole milliseconds. Default: suite value, then workspace
value, then `250`.

Minimum pause at each stop during initial lazy-load scrolling. The pass also
waits for visible image loading/decoding and common deferred image, picture, and
lazy-loader states. A longer pause can help debounced custom loaders. `0`
removes only the minimum pause, not resource checks. This scenario's value
overrides the suite and workspace; it is ignored with `noScrollTop: true`.

{% end %}

{% spoiler() %}

lazyLoadTimeout

:::::

Type: positive whole milliseconds. Default: suite value, then workspace value,
then `20000`.

Total budget for the initial lazy-load pass, including its pauses, visible
resource waits, and stable return to the top. Increase it for slow or long pages,
for example `60000`. Expiry fails this capture, rather than treating its remaining
placeholders as a successful load. The overall `scenarioTimeout` still applies.
Zero is invalid; `noScrollTop: true` skips the entire pass instead.

{% end %}

{% spoiler() %}

misMatchThreshold

:::::

Type: `number`.  
Default value: Test suite's value.  
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
Default value: Test suite value, then workspace value, otherwise `5`.
Min value: `0`.
Max value: `100`.

Per-pixel color tolerance for this scenario's screenshots. Lower values highlight
subtler color changes. The comparer divides the value by `100`, so `5` corresponds
to a normalized tolerance of `0.05`. This does not replace `misMatchThreshold`,
which controls the percentage of mismatches allowed for a pass.

The desktop field uses `0.1` steps. An explicit `0` overrides inherited values and
removes color tolerance; anti-alias filtering still applies to equal-size images.
The resolved value is saved for all scenario captures, including intermediate
saves and selector targets. Standalone `compare` reuses captured values.

```yaml
scenarios:
  - url: https://example.com
    pixelMatchThreshold: 0.5
```

{% end %}

{% spoiler() %}

needs

:::::

Type: `string | string[]`.  
Default value: `[]`.

List of the IDs of other scenarios needed to be executed before this scenario.

Example:

```yaml
scenarios:
  - url: https://www.domain.com
    id: open-form
    actions:
      - click: '#form-popup'

  - url: https://www.domain.com
    id: input-form
    # Execute `open-form` actions
    # before executing this scenario's actions
    needs: open-form
    actions:
      - input: input#username
        value: My Name
      - input: input#password
        value: Passw0rd!
  
  - url: https://www.domain.com
    label: Submit form
    # Execute `open-form` and `input-form` actions
    # before executing this scenario's actions
    needs: input-form
    actions:
      - click: button[type="submit"]
```

{% end %}

{% spoiler() %}

noScrollTop

:::::

Type: `boolean`.  
Default value: `false`.

Disable the initial lazy-load scroll.

By default, after navigation and readiness hooks, Regressify scrolls the page down and back to the top to trigger lazy-loaded images before any prerequisite (`needs`), scenario, or legacy actions. This lazy-load pass is not repeated before the final screenshot, so it does not disturb hover or scroll states established by actions. Set this to `true` to skip the initial scroll.

Automatic lazy-load scrolling bypasses smooth scrolling and honors
`lazyLoadScrollDelay` and `lazyLoadTimeout`. Before actions or capture, both scroll
offsets must remain at the top for 500 ms, even when there are no actions or
capture settling is disabled. Returning to a stable top position has a 3-second
deadline within the configured total budget; failure stops the scenario instead
of capturing mid-scroll. The pass also stops with an error after 150 scroll steps
to bound endlessly growing pages.

{% end %}

{% spoiler() %}

postInteractionWait

:::::

Type: `number`.  
Default value: Test suite's value, then workspace value, then `1000`.

Wait for X milliseconds after actions and before taking the screenshot. Set this to `0` when the scenario should capture immediately after interactions.

Capture preparation hides blinking text carets without blurring focused fields or changing their selection and values. This also applies with animation freezing or settling disabled; extra waiting is not needed to avoid caret-only mismatches.

{% end %}

{% spoiler() %}

removeSelectors

:::::

Type: `string[]`.  
Default value: Test suite's value, then workspace value, then `[]`.

Standard CSS selectors set to `display: none` in every document of the
scenario, before actions and captures. Matching elements stay in the DOM but
take no space, actions cannot interact with them, and selectors that match
nothing are ignored. A scenario list replaces the inherited list. Use the
[`remove` action][action-remove] instead when only a later step should remove
an element.

{% end %}

{% spoiler() %}

restore

:::::

Type: `string | string[]`.  
Default value: Test suite `state`, then workspace `state`.

Restore one or more persisted states for this scenario. Values are combined with suite/workspace `state` values and duplicates are removed while preserving order. State files are loaded from the active output directory, for example `.regressify/states/auth.json` or `.regressify__staging/states/auth.json`.

Example:

```yaml
scenarios:
  - url: https://www.domain.com/account
    restore:
      - auth
      - preferences
```

{% end %}

{% spoiler() %}

requiredLogin

:::::

Type: `boolean`.  
Default value: Workspace value, then `false`.

Marks the scenario as requiring login in the resolved scenario context. The global `--requiredLogin` flag forces this value to `true` for every scenario. The Rust CLI exposes the value to custom page scripts as `window.__REGRESSIFY__.requiredLogin` and `window.regressify.requiredLogin`; the actual login flow should still be implemented through persisted `state`, `restore`, cookie files, or explicit actions.

{% end %}

{% spoiler() %}

useCssOverride

:::::

Type: `boolean`.  
Accepted values: `true` or `false`.  
Default value: `true`.

Should the scenario use CSS override.

{% end %}

{% spoiler() %}

viewportNames

:::::

Type: `string | string[]`.  
Default value: all available viewports.

Choose the viewports you want applied for the scenario.  
If this property is set at both the scenario and test suite levels, the scenario-level setting will take precedence.
Unknown viewport names fail as configuration errors before any browser is launched.

Sample:

```yaml hl_lines=4-6
scenarios:
  - url: https://tuyen.blog/
    label: Click theme toggle on tablet and desktop
    viewportNames:
      - tablet
      - desktop
    actions:
      - click: theme-toggle
```

{% end %}

{% spoiler() %}

selector

:::::

Type: `string`.

Capture only the first matching CSS element for the scenario's `FINAL`
screenshot, or use the special value `viewport` to capture the currently visible
viewport. This does not affect any `save` actions, which have their own
optional `selector`. The `FINAL` filename suffix stays unchanged.

An element capture contains only the pixels inside that element's box. Menus,
drawers, popovers, and dialogs opened by actions usually extend beyond their
container, so capturing `header` after opening a dropdown records only the
header bar. Capture `viewport` or the opened element instead, and capture the
closed container in a separate scenario.

Omit `selector` or use `null` for a full-page final capture, unless using
`selectors` below. A selector must be nonblank and match a visible element with
positive dimensions; invalid or missing targets fail the capture. Do not set
both `selector` and `selectors`.

The final screenshot is skipped if the last applicable scenario action is
`save`. Scenarios with no actions, no applicable actions, or a later non-save
action still capture it. Saves inherited through `needs` do not suppress it.

```yaml
scenarios:
  - url: https://www.domain.com/dashboard
    selector: '#activity-feed'
```

{% end %}

{% spoiler() %}

selectors

:::::

Type: `string | string[]`.

Capture one or more CSS selectors instead of the full page. Each target writes a
separate screenshot. Use the special value `viewport` to capture the currently
visible viewport. Explicit `selectors` are always captured after actions, even
when the last applicable action is `save`. Do not combine this property with
`selector`. Omit both properties for a full-page final capture, unless a terminal
`save` replaces it.

Targets are captured in declaration order, must be non-empty and unique, and
must resolve to a visible element when captured. Like `selector`, each element
target is clipped to its own box. Regressify restores the prior page scroll
position after each selector capture so one target does not alter a later
viewport, selector, or full-page capture. Comparison reports label each
target as `document`, `viewport`, a saved step, or a stable selector capture
name. Full-page and selector targets must resolve to finite positive dimensions
no larger than 100000 CSS pixels in either direction or 64000000 total CSS
pixels.

```yaml
scenarios:
  - url: https://www.domain.com/dashboard
    selectors:
      - viewport
      - main .summary
      - '#activity-feed'
```

{% end %}

### Obsoleted properties

{% spoiler(danger=true) %}

clickSelector - *obsoleted*

:::::

> OBSOLETED - use [actions.click][action-click] instead.

Type: `string`.  
Default value: Empty.

Click the specified DOM element prior to screen shot.  
Notes: legacy shortcuts become actions only when `actions` is omitted; any
`actions` list, even an empty one, disables them. They run in this order:
`keyPressSelector(s)`, `hoverSelector(s)`, `clickSelector(s)`, then
`scrollToSelector`. Prefer the explicit `click` action for new suites.

{% end %}

{% spoiler(danger=true) %}

clickSelectors - *obsoleted*

:::::

> OBSOLETED - use [actions.click][action-click] instead.

Type: `string | string[]`.  
Default value: Empty.

Click the specified DOM elements, in order, prior to screen shot.  
Notes: converted like `clickSelector`, only when `actions` is omitted. When both
are set, `clickSelectors` is used and `clickSelector` is ignored. Prefer
explicit `click` actions for new suites.

{% end %}

[create-an-action]: @/optimizely-cms/testing/create-action.md
[action-click]: @/optimizely-cms/testing/create-action.md#spoiler--click
[action-hide]: @/optimizely-cms/testing/create-action.md#spoiler--hide
[action-remove]: @/optimizely-cms/testing/create-action.md#spoiler--remove
