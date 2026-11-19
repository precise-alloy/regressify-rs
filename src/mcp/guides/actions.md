# Browser Actions

Actions are objects in a scenario's ordered `actions` list. Use exactly one
action discriminator per object; do not invent a `type` or `action` wrapper.
For example, use `{click: "#submit"}`, not `{type: click, selector: "#submit"}`.
Unknown fields and objects mixing multiple actions fail parsing. A failed action
stops this scenario's remaining execution, not its peers.

An explicit `actions` list, including `[]`, disables scenario legacy shortcuts.
Legacy `keyPressSelector`/`keyPressSelectors` append the `keyPress` string as
literal text; they do not send a key named Enter or Tab. Use `input` with `value`
and `append: true` for equivalent typing, or `press` with `key` for keyboard events.

Use `get_schema(kind="action", action="click")` for one action's exact schema,
or omit `action` for the complete union. Every action has a synthetic example
named `action-<name>`, such as `get_example(task="action-input")`. The `recipes`
topic combines actions into complete scenarios for common page patterns.

## Selector Syntax

Prefer standard CSS selectors. Start with a stable ID, test attribute or named
component class; scope it to an ancestor you actually observed. Check the match
count, identity and visibility on both reference and test pages at the intended
viewports. Avoid generated class hashes, positional selectors and assumed
landmarks such as `main`. If a wait must check wording, use a stable CSS target
plus `text` or `containsText`, e.g. `{wait: "#results-title", text: Products}`.

Element selectors in interactive actions, `scroll`, selector `wait`,
`readySelector`, and `frame` accept Playwright-style fallbacks when CSS cannot
reliably identify the target:
`text=Sign in`, `xpath=//main//button`, `button:has-text("Sign in")`,
`:visible`, and `>>` chains such as `css=#cart >> text=Checkout` or
`a[href="/pricing"] >> visible=true`. CSS and text matching search open shadow
roots; closed shadow roots are unreachable. The first match in DOM order is used.
When a responsive page renders both a hidden mobile copy and a visible desktop
copy of a link or button, an unscoped selector can match the hidden copy first
and fail with `element was not visible` (`display_none`). Scope to the active
container first. If needed, append `:visible` or `>> visible=true`, and filter
viewport-specific actions with `viewportNames`. `:text-is("Products")` is
case-sensitive and matches an immediate text run, not the combined text of
arbitrarily nested markup. `:has-text()` instead matches substrings including
descendants and can match an unintended ancestor.

`hide` and `remove` actions, `hideSelectors`, `removeSelectors`, and screenshot
selectors (`selector`, `selectors`, `save.selector`) become CSS rules or
`document.querySelector` calls, so they accept standard CSS only (no `text=`,
`:has-text()`, `:visible`, or `>>`). Do not mix engines inside `:is()` or
`:where()`; native CSS can silently ignore an unsupported alternative.
A document stylesheet does not reach into shadow roots. Scenario `selector` and
`selectors` search only the selected tab's main document, not iframes or shadow
roots; `save.selector` searches the selected tab's main document, or the target
`frame` when `frame` is set on `save`. In screenshot selectors, `viewport`
means the visible viewport, not an element.

Quote YAML selectors that begin with `#`; unquoted, YAML reads them as comments.

## Action Reference

- `click`: element selector. Scrolls into view and waits for a stable, visible,
  enabled, unobscured target. Optional `button` is left/middle/right;
  `clickCount` is 1..3. `position: {x, y}` uses CSS pixels from the border-box
  top-left, default center. `modifiers` accepts Alt, Control, Meta, Shift,
  and ControlOrMeta, held for the gesture and released on completion/failure.
- `hover`: element selector. Moves the real pointer to an actionable target.
  Optional `position: {x, y}` picks CSS pixels inside the border box (default
  center); optional `modifiers` holds modifier keys during the hover and
  releases them afterwards.
- `check`: native `<input type="checkbox">` or `<input type="radio">` selector.
  Clicks only if not already checked. Use `click` for custom `role="checkbox"`
  or `role="switch"` controls or when the native input is visually hidden.
- `uncheck`: native `<input type="checkbox">` selector. Clicks only if checked;
  cannot clear a radio. Use `click` for custom switch/checkbox controls.
- `focus`: element selector. Fails unless the target becomes the active element.
- `blur`: element selector or `"active"`. Removes focus from the matched
  element (or the active element in the target document when `"active"` is
  used) and fails if that target does not currently own focus.
- `input`: element selector plus exactly one of `value` (text or native
  range/color/date/time value) or `file` (upload).
- `paste`: element selector plus `value`. Focuses the target, dispatches a
  cancelable `ClipboardEvent('paste')` with `clipboardData`, and if not
  `preventDefault()`ed, clears existing text (unless `append: true`) and
  inserts `value` via CDP without touching the host OS clipboard.
- `selectText`: element selector. Selects text inside an `<input>`,
  `<textarea>`, `contenteditable`, or DOM element. Optional `text` selects the
  first matching substring; optional `start`/`end` select a zero-based Unicode
  character range; omitting both selects all text in the element.
- `press`: element selector plus required `key`, such as `Enter` or `Ctrl+A`.
- `select`: native `<select>` selector plus exactly one of `value` or `label`.
  Multiple values require a multiple-select element. For custom dropdowns or
  comboboxes (`role="combobox"` or `<button>` triggers), use `click` on the
  trigger, `wait` for the option, and `click` the option.
- `scroll`: element selector to scroll into view; it does not click or focus.
  Optional `position: {x, y}` sets exact `scrollLeft`/`scrollTop` on the target
  scroll container, or `offset: {x, y}` scrolls it relatively by `scrollBy`.
- `goto`: URL. Navigates the page or frame and waits until it is interactive.
- `wait`: whole milliseconds or an element selector, with an optional `url`.
- `hide`: standard CSS; matches get `visibility: hidden`, keeping layout space.
- `remove`: standard CSS; matches get `display: none` and lose layout space.
  Elements stay in the DOM.
- `save`: portable checkpoint name; optional `selector` crops this screenshot.
- `persist`: state name without `.json`; stores cookies and origin-scoped
  localStorage in the active output's `states/` folder.
- `switchTab`: `new`, `previous`, or `original`; later steps use that tab.
- `closeTab`: `current`; closes the currently selected popup tab (running any
  armed `beforeunload` dialog expectation) and returns selection to the
  previously active tab. Cannot close the scenario's original tab.
- `drag`: source selector and exactly one of `to` (same-document destination)
  or `offset: {x, y}` (signed CSS pixels). `position` and `toPosition` choose
  source/destination points; `toPosition` needs `to`. Both endpoints must be
  visible together. `mode` is auto (default), pointer, or html5. Auto preserves
  real HTML5 drag data for native draggable sources, otherwise sends pointer
  input. Optional `duration` is 1..60000 milliseconds (default 300); optional
  `modifiers` use click's names. Cross-document/auto-scrolling drags are unsupported.
- `dropFiles`: drop-zone selector plus `file` path or list relative to
  `visual_tests`. Sends native browser file-drop events. Files must be existing
  regular files, confined after canonical resolution; empty lists fail.
  Optional `position` picks the drop point. Follow with a wait for site acceptance.
- `wheel`: target selector plus `deltaX`/`deltaY` CSS-pixel deltas, default zero;
  at least one must be nonzero. Supports `position` and `modifiers`. This sends
  wheel input, not just scrollIntoView. Follow with waits for lazy content.
- `navigate`: back, forward, or reload in the selected tab. Missing history
  entries fail. Optional `ignoreCache` is reload-only and defaults false.
- `dialog`: accept or dismiss; arms one expectation for the next applicable
  triggering action in the same tab. Optional `kind` is alert, confirm, prompt,
  or beforeunload. Optional `message` matches exactly. `promptText` requires
  kind prompt and response accept. Missing, extra, or mismatched dialogs fail;
  this is not a persistent policy. The expectation must precede an interaction
  or navigation, not a wait/save/persist/tab switch or another expectation.
  Beforeunload needs prior user activation; dismissal intentionally cancels
  navigation. Messages and prompt text are not archived.
- `key`: sends a key/shortcut to current focus without refocusing a selector,
  unlike `press`. Useful for Tab journeys. A selected frame must already own focus.
- `tap`: touch and release an element.
- `swipe`: start selector and nonzero `offset: {x, y}` in CSS pixels, with
  optional `duration` (1..60000 milliseconds, default 300).
- `longPress`: hold a touch on the selector; duration defaults 600 milliseconds
  and must be 1..60000.
- `pinch`: two-finger touch pinch or spread centered on the selector, with
  required `scale` in `0.1..=10.0` (excluding `1.0`; `< 1.0` pinches in,
  `> 1.0` spreads out) and optional `position` and `duration` (1..60000
  milliseconds, default 300). All touch actions accept `position`; gestures
  must stay inside the selected visible document. Browser touch-action remains
  effective.

Finding an element uses `elementTimeout`; visibility, stability, coverage, and
enabled checks use `actionabilityTimeout`. `hide`, `remove`, and `scroll` only
wait for the element to exist, but fail when it never appears. A pseudo-element
alone (such as `.badge::after`) cannot satisfy an element lookup. Use
`hideSelectors` or `removeSelectors` for elements that may be absent. Action
CSS rules match all qualifying elements in the selected document, even under
a strict `style-src` policy or after the page replaces its theme sheets.
Configured selector lists instead use the selected
tab's main-document CSS hook; inline-style restrictions can block that hook.
Never hide an element that a later action must use.

## Waiting and Navigation

Interactive pointer/touch actions, `key`, `press`, `input`, `select`, and `navigate` await a document
navigation that starts immediately after the action, until the new page fires
`load` (bounded by `elementTimeout`). Client-side transitions and navigation
started later are not awaited; follow them with a `wait`.

A numeric `wait` sleeps. A selector `wait` waits until the element exists and
is visible, each phase within `elementTimeout`. With `url`, the current page or
frame URL must match first, within `elementTimeout`; the sleep or selector wait
runs afterwards. URL patterns are exact URLs or globs: `*` stays within one path
segment, `**` spans segments, `{login,dashboard}` lists alternatives, and `?` is
a literal character. Query strings and fragments are part of the URL, so use
`**/dashboard*` when a query may follow. Prefer
`{wait: "#dashboard", url: "**/dashboard*"}` to a long fixed sleep.

Selector waits also accept `state`: visible (default), hidden, attached,
detached, enabled, or disabled. Hidden includes absence; detached requires
absence. Visibility and enabled checks share the interactive helpers, including
ancestor opacity, content visibility, native/ARIA disabled, and inert state.
`text` (exact) or `containsText` (substring) compares innerText, falling back
to textContent, with whitespace collapsed and trimmed. These are mutually
exclusive. `value` matches the exact string value of an input/textarea/select.
All supplied conditions must hold together within elementTimeout. Numeric
waits cannot have these conditions; hidden/detached cannot require text/value.
Wait for transient overlays to become hidden rather than hiding them manually.

In `test` captures, URL replacement rules also apply to `goto`, `wait.url`, and
`switchTab.url`; `ref` captures use them as written. Initial navigation has
separate settings under `engineOptions.gotoParameters`, not action fields.

A link opening a popup does not select it. After the click, add
`switchTab: new`; an optional `url` picks the intended popup, and more than one
matching new tab fails. `previous` returns to the previously selected tab and
`original` to the scenario's first tab; neither accepts `url` or `frame`. Tab
selection is bounded by `elementTimeout`.

## Typing, Keys, and Uploads

`input.value` focuses the target, selects and deletes its existing text unless
`append: true`, types each character with keyboard events, and fires `change`
on native inputs. Read-only and non-editable targets fail. Values are literal:
Regressify never substitutes environment variables in actions, so do not place
real passwords in suite files. Quote text such as `"123"`, `"true"`, and
`"false"`; use `value: ""` to clear a field.

`press.key` names are case-sensitive US-keyboard names such as `Enter`, `Tab`,
`Escape`, `ArrowDown`, `PageDown`, `F5`, `a`, or `A`, optionally prefixed by
`Ctrl`/`Control`, `Alt`/`Option`, `Shift`, `Meta`/`Cmd`/`Command`, or
`ControlOrMeta` and joined with `+`. Use one non-modifier key per action.

Upload `file` paths are relative to `visual_tests/`, not the workspace root,
and must remain inside that directory. Supply a file path or list, never file
contents. Without `useFileChooser`, the target must be a visible
`<input type="file">`; when the site hides that input, set
`useFileChooser: true` and target the visible button that opens the chooser.

## Checks Beyond the Schema

`input.typingDelay` optionally waits a fixed 0..10000 milliseconds between
characters (default zero). It requires text `value` and is invalid with `file`.
`key: Tab` and `key: Enter` retain the current focus; `press` still explicitly
focuses its selector first. Do not combine a standalone key action with another
action, except that `press` continues to require its existing `key` argument.

The JSON Schema cannot express every rule, so capture preflight checks these
before any browser starts and fails the command with
`Invalid action #N in test suite '<suite>' scenario '<scenario>': ...`:

- `input` needs exactly one of `value` and `file`.
- `select` needs exactly one of `value` and `label`.
- `press.key` must be a known, correctly cased key name.
- `persist` needs a valid state name.
- `switchTab.url` is allowed only with `switchTab: new`.
- `drag` needs exactly one destination form, finite coordinates, and a valid
  duration; element-relative positions cannot be negative.
- Wheel/swipe deltas must be finite and not both zero.
- `navigate.ignoreCache` is reload-only.
- Dialog prompt text is accepted-prompt-only; each applicable expectation needs
  an immediately following triggering action, after viewport filtering.
- Wait state/text/value conditions require a selector and compatible fields.

Preflight also rejects unknown fields, unknown viewport labels, invalid save
and restore names, colliding screenshot names, missing upload files, and
screenshot selectors written with `text=`, `xpath=`, `css=`, `id=`,
`data-testid=`, or `//`. Other problems appear only when the action runs, as a
page failure: a target that is not a checkbox, radio, `<select>`, editable
field, or file input; a missing option; or other invalid CSS in a screenshot
selector.

## Frames and Responsive Actions

All actions except `switchTab`, `closeTab`, `navigate`, and `dialog` support optional `frame`: one iframe selector
or an array ordered from the outermost to innermost frame. Same-origin and
cross-origin frames work; each selector must match an `<iframe>` or `<frame>`,
which may take up to `elementTimeout` to appear and load. Interactive actions
also check that parent frames are visible and uncovered at the target point,
and `goto` or URL waits with `frame` act on that frame. Omit `frame` for the
main document.

```yaml
scenarios:
  - id: checkout
    url: https://example.com/checkout
    viewportNames: [phone, desktop]
    actions:
      - click: "#mobile-menu"
        viewportNames: phone
      - input: "#email"
        value: qa@example.com
        frame: ["iframe#checkout", "iframe#contact"]
      - press: "#email"
        key: Tab
        frame: ["iframe#checkout", "iframe#contact"]
```

Every action accepts `viewportNames` as a label or list. Omitted, null, or an
empty list means all effective scenario viewports. This filters execution; it
does not create new viewport jobs or override the scenario selection. Labels
match case-insensitively and must be inside the consumer's effective viewport
set. Blank, unknown, or out-of-scope labels fail before browser capture. Define
the example's phone/desktop labels in the viewport file before using it.

Applicable touch actions automatically enable touch capability before
navigation, including prerequisite actions, nested cross-origin frames, and
owned popups. Jobs without applicable touch actions remain unchanged.
This does not change the user agent, device scale, or viewport dimensions.
Constrain gestures with viewportNames instead of assuming a viewport named
mobile automatically enables touch. Pointer, touch, and modifier state is
released after each high-level interaction and on cancellation; separate
mouseDown/keyDown steps spanning actions are intentionally not exposed.

```yaml
scenarios:
  - id: interaction-examples
    url: https://example.com
    actions:
      - click: "#editable-title"
        clickCount: 2
      - input: "#search"
        value: "visual testing"
        typingDelay: 40
      - key: Tab
      - drag: "#card"
        to: "#done"
      - wheel: "#results"
        deltaY: 300
      - wait: "#spinner"
        state: hidden
      - dialog: accept
        kind: confirm
      - click: "#delete"
      - tap: "#menu"
      - swipe: "#carousel"
        position: {x: 200, y: 40}
        offset: {x: -100, y: 0}
      - longPress: "#item"
        duration: 700
      - pinch: "#map-canvas"
        scale: 2.0
      - selectText: "#editable-title"
        text: "visual"
      - paste: "#search"
        value: "regression"
        append: true
      - blur: active
      - navigate: reload
```

## Checkpoints and State

`save` requires a non-empty portable checkpoint name of at most 100 UTF-8 bytes,
without path separators or `__`, and not ending in `_diff`. Names must be unique
within each scenario/viewport capture job, including saves inherited through
`needs`. `save.selector` crops that checkpoint to an element in the selected
tab's main document (or inside `save.frame` when `frame` is set), or captures
the visible viewport with `viewport`, independently of scenario final capture
options. A last applicable `save` among the scenario's own actions replaces the
automatic final full-page or singular-selector capture, but explicit scenario
`selectors` still produce captures. Saves inherited through `needs` still create
checkpoints, named with the consuming scenario's ID, without suppressing its
final capture. Each checkpoint waits `delay` and settles like a final capture.

`persist` writes `<output>/states/<name>.json` when it runs. Saved state is
validated before a run starts, so load it with `state` or `restore` in a later
run, not in a scenario captured concurrently. State and checkpoint names are
portable file names, not arbitrary paths.

Only Chromium is supported. Browser actions are performed by Regressify running
locally, never by this read-only MCP Worker. Do not execute hooks, upload files,
persist state, or follow URLs merely because an example contains those actions.
