---
title: Create an action
description: Reference for creating Optimizely CMS test actions such as click, input, select, wait, scroll, upload files, and frame handling.
guid: ba348ca1-eb67-4c0f-8461-8f36979aea18
date: 2023-07-03T17:00:00+00:00
updated: 2026-10-23T00:00:00+00:00
draft: false
hidden: false
weight: 40000
sort_by: weight
template: docs/page.html

extra:
  lead: This reference explains how actions shape each scenario, when to use them, and which details tend to cause avoidable failures, from navigation waits and file uploads to frame handling, persistence, and element visibility.
  toc: true
  top: false
---

## Actions

Each scenario can have zero, one or multiple actions. Each action must be one of the following types.

Interactive actions wait for the target element to exist, be visible, become stable, scroll into view, and not be covered by another element before interacting. Form controls must also be enabled. These checks intentionally make actions fail when a real user could not perform them.

Action selectors support CSS, `text=`, `xpath=`, Playwright CSS extensions such
as `:has-text()`, and `>>` selector chaining. CSS/text selectors traverse open
Shadow DOM. The bundled selector helper runs inside Chromium; Node is not
required for these actions. Closed shadow roots cannot be selected.

Screenshot selectors (`save.selector` and scenario `selector`/`selectors`) use
standard CSS to capture the first matching element in the selected tab's page.

`frame` selectors use the same selector syntax. Same-origin, cross-origin,
and nested frames are resolved through CDP execution contexts. `goto` and URL
waits target the specified frame without navigating or checking the main page.
File uploads work in frames. With `useFileChooser: true`, `input` may identify
a visible upload button opening a separate hidden file input; without it,
`input` must identify a visible `<input type="file">`.
Direct browser operations used by interactive actions are bounded by the configured actionability timeout, so an unresponsive page cannot hold a scenario indefinitely.
Unknown action properties fail suite parsing instead of being ignored.

Desktop scenario logs record action targets and outcomes separately for each
concurrent scenario/viewport job. Text supplied through `input.value` is omitted
from action logs. Selectors, URLs, custom script messages, and errors may still
include project-provided data, so review logs before sharing them.

### Viewport Selection

Every action type, including `save`, `persist`, and `switchTab`, accepts an
optional `viewportNames` string or array, matching scenario syntax.
`viewportNames: mobile` and `viewportNames: [mobile]` are equivalent.
Scenario selection determines capture jobs;
action selection only determines which steps run inside each existing job.
It does not resize the browser or add viewports.

- Omitted, `null`, or `[]` means every effective scenario viewport.
- A string or nonempty list matches labels case-insensitively. Duplicate names never repeat
  an action within one job. Remaining actions keep their original order.
- Unknown names, empty or whitespace-only strings, non-string items, and names excluded by the
  consuming scenario are configuration errors before browser startup. A filter
  may equal the whole scenario selection; it need not be strictly smaller.
- With no named viewport file, capture uses the unnamed browser default. Only
  omitted, `null`, or empty-list action filters are valid in that case.

Assume the viewport file defines `desktop` and `mobile`:

```yaml
scenarios:
  - id: products-submenu
    url: https://example.com
    viewportNames: [desktop, mobile]
    selectors: [viewport]
    actions:
      - click: "#menu-toggle"
        viewportNames: mobile
      - click: "#products-menu"
      - click: "#products-submenu a[href='/products/widget']"
```

Desktop skips the hamburger action; mobile opens it before both jobs follow the
shared menu path. When the top-level selectors also differ, use adjacent
desktop-only and mobile-only actions before the shared steps.

A valid but unintended label can **silently exclude** an action from the
intended nonmatching jobs, without an error. Matching jobs still execute it.
Validation catches names outside the scenario set, not the author's intent;
double-check the labels against the intended responsive path.

Skipped actions do not resolve selectors or frames, wait, navigate, switch tabs,
apply per-action document hooks, upload files, persist state, or write screenshots.
Scenario setup and final captures still run, even when every action skips.
When the last applicable scenario action is `save`, its checkpoint replaces the
automatic final screenshot for that viewport. Explicit scenario `selectors`
are always captured after actions, including after a terminal `save`.
`--log-level debug` shows the skipped action kind and viewport in console/live
logs without its selector, input, URL, file, or state values. These skip messages
do not count as executed actions or skipped scenarios.

`needs` replays prerequisite actions with their filters against the **consumer's**
effective viewport set, including nested prerequisites. Every named filter must
belong to that set. A mobile-only consumer cannot inherit a desktop-only action;
split the prerequisite into a common base and viewport-specific branches instead.
Legacy selector shortcuts remain unfiltered when no explicit `actions` list is set.

A filtered `save` contributes a checkpoint only on its selected viewports.
Manifests, progress, comparison ordering, and failure placeholders use that same
inventory. Identical save names on disjoint viewport branches are allowed;
overlapping branches still fail collision validation. Save names must remain
valid even when a particular job skips them. Historical baselines are not deleted.

**Persist/state limitation:** a scoped `persist` writes only from its included
viewports. Other scenarios' `state` or `restore` loads can observe older or
viewport-dependent content because jobs run concurrently. Leave `persist`
unscoped when another scenario consumes its name, and finish the producing run
before a consuming run when freshness matters. Unscoped writes alone do not
order jobs. There is no cross-scenario writer/reader analysis or new scheduling
guarantee; `needs` replays actions within a job, not between jobs.

{% spoiler() %}

check

:::::

Check a checkbox or radio element.

Example:

```yml
actions:
  # Check the checkbox with `tos-accept` ID
  - check: checkbox#tos-accept
```

{% end %}

{% spoiler() %}

click

:::::

Click an element.

Optional `button` is `left` (default), `middle`, or `right`; `clickCount` is
1 (default), 2, or 3. `modifiers` accepts `Alt`, `Control`, `Meta`, `Shift`,
and `ControlOrMeta`. Modifiers are held for the complete click sequence and
released afterwards, including failures. `position: {x: 10, y: 10}` selects
CSS pixels from the element's top-left border box instead of its center.
The requested point, not just the center, must be inside the element and uncovered.

> Important:
>
> Click-triggered document navigation is awaited automatically. Use an explicit
> URL or selector `wait` for delayed application transitions and SPA readiness.

Links with `target="_blank"` (including `rel="noopener"`) and JavaScript
`window.open()` open separate pages. Follow the click with `switchTab: new` to
use that page for subsequent actions and captures. A URL wait alone never switches tabs.

Example:

```yml
actions:
  # Click the button with .submit class
  - click: button.submit
```

{% end %}

{% spoiler() %}

focus

:::::

Focus an element.
The action fails if the requested element cannot become the active element,
preventing later keyboard input from being sent to a different target.

Example:

```yml
actions:
  # Focus an input element named `email`
  - focus: input[name="email"]
```

{% end %}

{% spoiler() %}

goto

:::::

Type: `string`.

Go to specific URL.
Regressify waits until the destination document is interactive before running the
next action. In `test` mode, active URL replacements are applied before
navigating; `ref` mode uses the URL as written.

Example:

```yml
actions:
  - goto: https://www.domain.com
```

{% end %}

{% spoiler() %}

hide

:::::

Hide an element - set `visibility: hidden !important`.

Example:

```yml
actions:
  # Hide the element with `cookie-modal` ID
  - hide: '#cookie-modal'
```

{% end %}

{% spoiler() %}

hover

:::::

Hover an element.

Lazy-load scrolling finishes with stable scroll offsets at the top before actions begin. It is not repeated afterward, so it does not reset the hover state before the final screenshot.

Example:

```yml
actions:
  # Hover mouse over an element contains `my-chart` ID
  - hover: '#my-chart'
```

{% end %}

{% spoiler() %}

input

:::::

Type text in an editable `<input>`, `<textarea>`, or contenteditable element.
Read-only and non-editable targets fail instead of silently succeeding.
Text is typed one character at a time with native keyboard events for mapped
keys, including inside frames. Unicode characters without a keyboard mapping
use Chromium text insertion. Browser edit constraints and `beforeinput`
handlers are honored. A native input receives a final `change` event.

Set exactly one of `value` and `file`. Upload `file` paths are relative to
`visual_tests/`, must stay inside it, and must exist. These rules are checked
before the browser starts.

Example:

```yml
actions:
  # Type `My Name` in an input element with `username` ID
  - input: 'input#username'
    value: My Name
    # `append` is an optional property, with `false` as the default value
    append: false

  # Type `my-name@domain.com` in an input element with `email` ID
  - input: 'input#email'
    value: my-name@domain.com
    # `append` is an optional property, with `false` as the default value
    append: true
  
  # Load `visual_tests/file1.txt` into the input with `file` ID
  - input: 'input#file'
    file: file1.txt
    # `useFileChooser` is an optional property, with `false` as the default value
    useFileChooser: true
  
  # Load `visual_tests/file1.txt` and `visual_tests/other/file2.pdf`
  # into the input with `files` ID
  # Note that the input must allow upload multiple files
  - input: 'input#files'
    file:
      - file1.txt
      - other/file2.pdf
    # `useFileChooser` is an optional property, with `false` as the default value
    useFileChooser: false
```

{% end %}

{% spoiler() %}

persist

:::::

Type: `string`.

Persists (saves) the current browser state, including cookies and local storage, to disk.

New state files use Playwright's `{ cookies, origins: [{ origin, localStorage }] }`
format. Local storage stays scoped to its origin and is restored before page
scripts execute. Changes made during navigation are retained when persisting;
later navigations do not reapply the original values. Legacy Rust
`{ cookies, localStorage }` files remain readable, with their unscoped local
storage assigned to the scenario's initial origin.

Example:

```yml
actions:
  - input: input#username
    value: aaaa
  - input: input#password
    value: pa$$w0rd
  - click: button#login-btn
  - persist: user-aaaa
```

You can later reuse this saved state to test the site as if you are already logged in by [specifying the state name][test-suite-state] in your test suite:

```yaml wrap
state: user-aaaa
```

State names must be non-empty portable filename components no longer than 80
bytes. State files are written under the active Regressify output directory, so
`--site-env staging` stores them under `.regressify__staging/states/`.
When a named state is absent there, the TS workspace's `.states/<name>.json`
is read as a migration fallback. The legacy file is never modified.
Configured `state` and `restore` files are validated before Chromium launches;
missing or malformed state files fail the command instead of silently using an
empty session. State updates are written atomically so interrupted persistence
cannot publish a partial JSON file.

{% end %}

{% spoiler() %}

press

:::::

Press a key (or key combination) on an element.

Supported modifier names are `Ctrl`/`Control`, `Alt`/`Option`, `Shift`, and `Meta`/`Cmd`/`Command`. A key combination may contain one non-modifier key, such as `Ctrl+A`, `Shift+Tab`, or `Meta+K`.
`ControlOrMeta` selects Control on Windows/Linux and Meta on macOS. Standalone
modifiers, literal `+` (or `Control++`), physical codes such as `KeyA`, and
function keys are supported. Modifiers are pressed and released around the key.
Key names are case-sensitive: use `Enter`, not `enter`. Unknown names fail
before the browser starts.

> Important:
>
> Key-triggered document navigation is awaited automatically. Use an explicit
> wait for delayed application transitions or SPA readiness.

Example:

```yml
actions:
  # Select all text inside a textarea with `message` ID
  - press: 'textarea#message'
    key: Ctrl+A
```

{% end %}

{% spoiler() %}

remove

:::::

Remove an element - set `display: none !important`.

Example:

```yml
actions:
  # Remove a `div` element with class `.random-content`
  - remove: div.random-content
```

{% end %}

{% spoiler() %}

save

:::::

Capture a named screenshot. Optional `selector` clips this screenshot to the
first matching CSS element in the selected tab's main document (or inside
`frame` when `frame` is set on `save`), or captures the currently visible
viewport when set to `viewport`; omit it or use `null` to capture the whole
page. An element capture excludes anything outside the element's box, such as
an opened menu that overflows it. The selector must be nonblank and match a
visible element with positive dimensions. Invalid, missing, or hidden targets
fail the action rather than falling back to a full-page screenshot. The
scenario's own `selector` does not affect save actions. Selector captures
restore the prior page and frame scroll positions.

Names must be non-empty portable filename components no longer than
100 bytes; path separators, `__`, reserved Windows filename characters, and the
`_diff` suffix are rejected. Reports list saved screenshots in action order
before any final capture targets. Adding a selector does not change the
`$<save-name>` filename suffix.

If the last applicable action in the scenario is `save`, Regressify skips its
automatic `FINAL` screenshot, including a singular scenario `selector` capture.
This is evaluated per viewport after action filtering. Explicit scenario
`selectors` still run. Saves inherited through `needs` do not suppress the
consuming scenario's final capture.

Each checkpoint uses the same resolved `delay`, animation freezing, and bounded
font/image/layout settling as the final capture. This does not repeat lazy-load
scrolling or move the pointer. `freezeAnimations: false` and `settleTimeout: 0`
disable their respective checks; `delay: 0` skips the explicit delay.
`postInteractionWait` runs only before final captures and is skipped when there
are none.
Text carets are hidden before each checkpoint even when animation freezing or
settling is disabled; input focus, selection, and values are preserved.
With `imageSimplification: pixelate`, qualifying images are pixelated for the
checkpoint only and restored before the next action; see
[create a test suite](create-test-suite.md) for which images qualify.

Example:

```yml
actions:
  - save: after-login
  - save: order-summary
    selector: '#order-summary'
```

This captures the full page, then the order summary, without an automatic final
screenshot unless the scenario explicitly sets `selectors`.

{% end %}

{% spoiler() %}

scroll

:::::

Scroll to an element.

Example:

```yml
actions:
  # Scroll to an element with `.report` class
  - scroll: .report
```

{% end %}

{% spoiler() %}

select

:::::

Select an option inside a `select` element.

Example:

Let say you have following HTML:

```html
<select name="method" multiple>
  <option value="email">Send via email</option>
  <option value="sms">Send via SMS</option>
  <option value="skype">Send via Skype</option>
</select>
```

Then you can create select actions like following sample:

```yml
actions:
  # Select the option with `email` value
  - select: select[name="method"]
    value: email

  # Select the option with `Send via email` label
  - select: select[name="method"]
    label: Send via email

  # Select options with `sms` or `skype` value
  - select: select[name="method"]
    value:
      - sms
      - skype
```

> Notes:
>
> The `select` action requires either `value` or `label` to be set, but not both. Omitting both or setting both fails before the browser starts.
> The selected value or label must match an existing option. Selecting multiple values requires a `<select multiple>` element.

{% end %}

{% spoiler() %}

switchTab

:::::

Select the browser tab used by subsequent actions, waits, `save`, `persist`, and
the final screenshot. This works in both the CLI and desktop app and does not
require Node.js or change the link's `target` behavior.

- `new`: wait for an open, not-yet-selected tab opened by the currently selected
  page or one of its frames. Supports `_blank`, `noopener`, and `window.open()`.
- `previous`: step back through the tab-selection history.
- `original`: select the tab where the scenario started.

An optional `url` on `switchTab: new` filters the destination using the same
exact URL/glob rules as `wait`. It follows redirects and includes query strings
and fragments. Test-mode URL replacements also apply; reference-mode URLs stay
as written. Omit `url` for `previous` and `original`; preflight rejects it there.

Tab selection is bounded by `elementTimeout`. Missing or closed tabs fail the
scenario. If several new tabs match, use a more specific `url`; Regressify does
not choose one arbitrarily. Previously selected or unrelated tabs are not new
tab candidates. `switchTab` has no `frame` property; later frame actions are
relative to the selected tab.

```yaml
actions:
  - click: 'a.quote-link'
  - switchTab: new
    url: '**/quote'
  - wait: '#my-form'
```

The final screenshot now captures the quote page. To capture it and then return
to the original page, append:

```yaml
  - save: quote-page
  - switchTab: original
```

Switching does not close other tabs. Returning to a tab preserves its state and
does not repeat initial lazy-load scrolling. New tabs inherit the capture
viewport, origin-scoped authentication and storage, and document hooks.

{% end %}

{% spoiler() %}

uncheck

:::::

Uncheck a checkbox element. A checked radio button cannot be cleared through a
real user click; select another radio in its group instead.

Example:

```yml
actions:
  # Uncheck the checkbox named `subscribe-email`
  - uncheck: checkbox[name="subscribe-email"]
```

{% end %}

{% spoiler() %}

wait

:::::

Wait for X milliseconds or wait for an element to exist and become visible.

URL patterns also support brace alternatives, for example
`**/{login,dashboard}?tab=*`. A `?` is a literal query delimiter, not a wildcard.
Select actions retry missing or disabled options until `elementTimeout` expires.
If there is navigation, add the URL pattern of the target page also.

In `test` mode, URL replacements from the active replacement profile and suite/workspace `urlReplacements` are applied to `url` before matching. In `ref` mode, wait URLs are matched as written.
URL patterns support `*` within a path segment, `**` across path depth, and
wildcards in query values.

The URL is matched before the delay or selector wait, within `elementTimeout`.
URL checks follow redirects and same-document navigation without depending on
JavaScript execution contexts. Query strings and fragments are included in the
match; a `frame` option scopes the URL check to that frame.

URL waits only observe the selected tab or frame. Use `switchTab: new` first
when a preceding click opens another browser tab.

> Important:
>
> After an action that triggers navigation, wait for the destination URL and a
> visible element on that page before continuing. A matching URL alone does not
> guarantee that the page has finished loading.

Example:

```yml
actions:
  # Delay 2 seconds
  - wait: 2000

  # Wait for an element with `popup` class to appear
  - wait: .popup

  # Wait for the navigation to `/quote`, then delay 2 seconds
  # `**` matches any path depth
  - wait: 2000
    url: '**/quote'

  # Wait for the navigation to `/quote`, then wait for `#my-form` element to appear
  # `*` matches within one path segment; `**` matches any path depth
  - wait: '#my-form'
    url: '**/quote'
```

{% end %}

## Extended user interactions

### Dragging and file drops

`drag` moves a held left pointer from its source to either `to` (another
element in the same document) or `offset: {x, y}` (signed CSS pixels).
Supply exactly one destination form. Optional `position` and `toPosition`
pick points inside the source and destination; `toPosition` requires `to`.
Both endpoints must be visible together after preparation. Cross-document
drags and automatic scrolling during a drag are not supported.

`mode` defaults to `auto`: native draggable elements use HTML5 drag/drop,
including their real `DataTransfer`; other widgets use pointer events.
Use `pointer` or `html5` to select explicitly. `duration` is a fixed 1..60000
milliseconds, default 300. Optional `modifiers` use the same names as `click`.
The native HTML5 mode fails if the source never starts a drag.

`dropFiles` targets a drop zone and requires `file`, a single path or a list.
Files must exist, be regular files, and remain inside `visual_tests` after
canonical resolution. Empty lists and directory paths fail. Optional `position`
selects the drop point. This sends browser drag events with actual files;
it does not set a hidden file input or fabricate JavaScript drop events.
The website must accept the drop; use a following `wait` to verify its result.

```yaml
actions:
  - drag: "#card"
    to: "#done-column"
    duration: 400
  - drag: "#slider-handle"
    offset: {x: 80, y: 0}
    mode: pointer
  - dropFiles: "#upload-zone"
    file: ["uploads/first.pdf", "uploads/second.pdf"]
  - wait: "#upload-complete"
```

### Wheel, typing timing, and focused keys

`wheel` sends actual wheel input over its target, with finite `deltaX` and
`deltaY` CSS-pixel deltas (both default zero; at least one must be nonzero).
Positive `deltaY` scrolls down. It supports `position` and `modifiers`.
Unlike `scroll`, it exercises wheel handlers and nested scroll containers.
Scrolling may be clamped or prevented by the site; wheel completion does not
mean lazy-loaded content is ready.

`input.typingDelay` adds a fixed 0..10000 milliseconds between characters,
default zero. It is only valid with `value`, never `file`.

`key` sends a key or shortcut to the current focus without changing it.
Its key names are the same as `press.key`. This allows real Tab navigation
without refocusing each destination and masking a site's focus-management
defect. With `frame`, that frame must already own keyboard focus.

```yaml
actions:
  - wheel: "#results"
    deltaY: 400
  - wait: "#next-page-loaded"
  - input: "#search"
    value: "visual testing"
    typingDelay: 50
  - key: Tab
  - key: Enter
```

### Browser history and dialogs

`navigate: back`, `navigate: forward`, and `navigate: reload` operate on
the selected tab. Missing history entries fail instead of silently doing
nothing. Optional `ignoreCache` is valid only with `reload`, default false.
Navigation uses the existing bounded navigation waiting and document hooks.
These operations cannot target an individual frame.

`dialog: accept` or `dialog: dismiss` arms one expectation for the next
**applicable triggering action in the same tab**, before that action begins.
Optional `kind` is `alert`, `confirm`, `prompt`, or `beforeunload`; optional
`message` matches the exact message. `promptText` requires `kind: prompt`
and `dialog: accept`. Dialog messages and prompt responses are not archived.

The expectation fails if no dialog appears within `elementTimeout`, its
kind/message differs, or the triggering action opens an extra dialog.
It cannot precede another expectation, a wait, screenshot, persistence action,
or tab switch, or remain at the end of the applicable action sequence.
Viewport filters apply before pairing. DOM modals still use normal actions.
A `beforeunload` warning requires prior user activation; browsers can suppress
it otherwise. Dismissing it intentionally cancels navigation and keeps the
current page. There is no persistent auto-accept policy.

```yaml
actions:
  - dialog: accept
    kind: confirm
    message: "Delete this item?"
  - click: "#delete"
  - navigate: back
  - navigate: reload
    ignoreCache: true
```

### Touch gestures

`tap` touches and releases an actionable element. `longPress` holds it for
`duration` milliseconds (default 600). `swipe` moves one finger by a required
nonzero `offset: {x, y}` over `duration` milliseconds (default 300).
Hold/swipe durations must be 1..60000. All three accept optional `position`.
The complete swipe must stay within the visible selected document/frame.
Browser `touch-action`, scrolling, and gesture handling remain in effect;
`longPress` does not invent a DOM long-press or context-menu event.

A job containing any applicable touch action, including inherited `needs`
actions, enables touch capability **before navigation**, also for cross-origin
frames and owned popups. Jobs without applicable touch actions stay unchanged.
Use `viewportNames` to confine touch to appropriate jobs. This does not change
the user agent, viewport size, or device scale, and is not full phone emulation.
Actions own and release their pointer, touch, and modifier state; low-level
down/move/up steps spanning separate actions are intentionally not exposed.

```yaml
actions:
  - tap: "#menu"
    viewportNames: mobile
  - swipe: "#carousel"
    position: {x: 200, y: 50}
    offset: {x: -150, y: 0}
    duration: 300
    viewportNames: mobile
  - longPress: "#item"
    duration: 700
    viewportNames: mobile
  - pinch: "#map-canvas"
    scale: 2.0
    duration: 300
    viewportNames: mobile
```

### Focus release, text selection, clipboard paste, and popup closing

`blur` removes focus from a matched element selector (or from the current
active element when `blur: active` is used). It fails if the target does not
currently own focus, ensuring scenarios catch unexpected focus shifts.

`selectText` highlights text inside an `<input>`, `<textarea>`,
`contenteditable`, or DOM element, dispatching `select` (for form controls)
and `selectionchange`. Optional `text` selects the first matching substring;
optional `start` and `end` select a zero-based Unicode character range;
omitting both selects all text in the target.

`paste` focuses the target, dispatches a cancelable `ClipboardEvent('paste')`
populated with `clipboardData` (`text/plain`), and when not canceled by the
page (`preventDefault()`), clears existing text (unless `append: true`) and
inserts `value` via CDP without modifying the host OS clipboard.

`closeTab: current` closes the currently selected popup tab (running any armed
`beforeunload` dialog expectation) and returns selection to the previously
active tab. Closing the scenario's original tab fails.

`hover` also accepts optional `position: {x, y}` and `modifiers`. `scroll`
accepts mutually exclusive `position: {x, y}` (`scrollLeft`/`scrollTop`) or
`offset: {x, y}` (`scrollBy`), and `input.value` supports native `range`,
`color`, `date`, `time`, `datetime-local`, `month`, and `week` inputs.

```yaml
actions:
  - selectText: "#editor"
    text: "important phrase"
  - paste: "#otp-input"
    value: "123456"
  - blur: active
  - closeTab: current
```

### Element-state waits

Selector waits now accept `state`: `visible` (default), `hidden`, `attached`,
`detached`, `enabled`, or `disabled`. Visibility uses the same ancestor,
opacity, and content-visibility checks as interactive actions.
`hidden` also matches an absent element; `detached` requires absence.
Enabled/disabled use the existing native/ARIA-disabled and inert checks.

Optional `text` matches exact element text, while `containsText` matches a
substring; both normalize whitespace by collapsing runs and trimming ends.
They use `innerText`, falling back to `textContent`. Supply at most one.
Optional `value` matches the exact string value of an input, textarea, or select.
All supplied conditions must hold together within `elementTimeout`.
Hidden/detached waits cannot require text or value. These fields cannot be
used with a numeric wait. An optional `url` is still matched first.

```yaml
actions:
  - wait: "#spinner"
    state: hidden
  - wait: "#submit"
    state: enabled
  - wait: "#status"
    text: "Order complete"
  - wait: "#quantity"
    value: "2"
```

## Shared properties

The following properties are applicable to page actions, except `switchTab`,
`closeTab`, `navigate`, and `dialog`, which are tab-scoped.

{% spoiler() %}

frame

:::::

Type: `string | string[]`.  
Default: empty.

This property specifies the selector for the target iframe within which the current action should be executed.
Frames wait for the configured element timeout while loading and must be
visible; same-origin and cross-origin frames both work. An array must contain at
least one selector. Regressify checks every parent frame boundary for overlays
before interacting with a nested target.

Example:

```yaml
actions:
  # Click `#submit` element inside the `.child` iframe
  - click: '#submit'
    frame: .child
  
  # Click `#submit` element inside the `#grandchild` iframe
  # `grandchild` is an element within the `.child` iframe
  - click: '#submit'
    frame:
      - .child
      - '#grandchild'
```

{% end %}

[test-suite-state]: @/optimizely-cms/testing/create-test-suite.md#spoiler--state
