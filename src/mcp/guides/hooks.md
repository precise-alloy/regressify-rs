# Injected Browser Scripts and CSS

Hooks are executable project code. Use them only when ordinary actions and
configuration do not express the requested behavior. Inspect existing scripts
locally and obtain permission before running untrusted code.
This MCP server supplies instructions only; it never executes hooks.

## Script Settings

| Setting          | Scope and Execution                                                                                                                                       |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `jsOnReadyPath`  | Workspace, suite, or scenario. Browser-side JavaScript, once per active page document after readiness and before actions. Default: `common/_on-ready.js`. |
| `onBeforeScript` | Scenario only. Browser JavaScript at the start of the initial top-level document, before site scripts. The DOM may not exist yet.                         |
| `onReadyScript`  | Scenario only. Browser JavaScript after readiness, CSS, and `jsOnReadyPath`, before actions. Runs once per selected document.                             |

Paths are relative to the workspace root, not to the suite or hook directory.
Explicit paths must name regular files inside the canonical workspace. All three
settings accept standard browser JavaScript with DOM APIs. No Node.js or Playwright
installation or process is required. CSS overrides and hide/remove rules are also
injected directly into Chromium.

## Capture Lifecycle

1. Create the scenario page, apply browser/viewport settings, and install state
   restoration and cookies.
1. Register `onBeforeScript`, then navigate using `engineOptions.gotoParameters`.
   Its synchronous statements run at document creation before site scripts.
   Await its returned promise after navigation; asynchronous work does not block
   the site's own scripts. Remove the registration before subsequent navigation.
1. Wait until
   `readySelector` exists and is visible.
1. Apply the CSS override, `hideSelectors`/`removeSelectors`, and the browser
   `jsOnReadyPath` script, then `onReadyScript`; freeze animations and scroll
   once for lazy content. Both ready scripts finish before actions. With
   `imageSimplification: pixelate`, the image filter (a hidden zero-size `<svg>`
   under `<html>` and one adopted stylesheet) is installed before the ready
   scripts; capture preparation re-installs it if a script removes it.
1. Run inherited `needs` actions, then scenario `actions`, or legacy shortcuts
   when `actions` is omitted. Each applicable `save` captures at its position.
1. If final captures remain, wait `postInteractionWait`
   and `delay`, settle, and capture them. With `pixelate`, qualifying images
   carry a `data-regressify-simplify` attribute only during each screenshot.

`readySelector` is checked before browser ready scripts and actions. Do not point
it at content that those later steps create. Use `onReadyScript` or
`jsOnReadyPath` to dismiss approved overlays before clicks, input, and `save`
checkpoints. A last applicable `save` can suppress
the automatic final capture, while explicit scenario `selectors` still run.
Browser hooks are applied to new documents after navigation and tab selection;
switching back to an already processed document does not rerun them.

## Browser Script Contract

Use plain `.js` files containing classic browser JavaScript. Do not generate
`module.exports = async function (...) { ... }`, `exports`, `require(...)`,
ES module `import`/`export`, or Node/Playwright callbacks for any hook setting.
No CommonJS or ES module loader runs, even if the filename ends in `.cjs` or
`.mjs`. In a normal browser document, `module.exports` fails with
`ReferenceError: module is not defined`. Renaming `.cjs` to `.js` does not convert
the contents.

For example, an `onReadyScript` file can remove an approved cookie banner before
interaction:

```javascript
document.querySelector("#cookie-banner")?.remove();
```

Return a promise for asynchronous work, for example with an async immediately
invoked function. The runner awaits that promise, but does not automatically
call a function that the script only declares. Scripts run in the selected
page's main document; `process.env` and Node filesystem APIs are unavailable.
Regressify does not invoke an exported callback or supply `page`, `scenario`,
`viewport`, `isReference`, `context`, or `config` callback arguments. Use the
`window.regressify` browser context below for metadata.

Playwright APIs such as `page.locator()`, `page.waitForURL()`, and
`page.setDefaultTimeout()` are not browser APIs. When migrating an old hook,
move typing, clicks, navigation, and waits into suite actions: `input`, `press`,
`click`, `goto`, and `wait` with `url`. Keep only browser-side DOM setup in the
hook. Ready hooks run before actions, so they cannot inspect input that a later
action has not entered yet. Do not add Node/Playwright dependencies or fake
`module`/`page` globals as a capture workaround.

Use Regressify's `switchTab` action when subsequent actions/captures need a
different tab. Ready hooks apply to that selected document. Other scenarios have
independent pages; never depend on their execution order or mutate a sibling's
session. `onBeforeScript` is limited to initial top-level navigation, not later
`goto` actions or frames.

Awaiting scripts is bounded by workspace `elementTimeout` (default 30000 ms);
the overall `scenarioTimeout` also applies. Throw an error when the intended
operation fails. Syntax errors and rejected promises fail the scenario; peers
continue and failed captures never replace good references.
Reports and capture logs name the failed hook, as in
`onReadyScript failed after 812 ms (Unknown)`, but not its message. The local
console's `Local detail` line shows the sanitized message, including the start
of the browser error, truncated to 2048 characters. For more context while
diagnosing, use `--debug` and browser developer tools on an authorized test page.
Never log credentials, cookies, input values, or private response bodies.

Retrieve `get_example(task="before-script")`, `scenario-ready-script`, and
`hooked-suite` together for a complete setup. The first hook sets a synthetic
reference/test value; the second removes a verified banner before actions.
Adapt them to verified application behavior, not merely to make a test pass.

## Browser Context

All injected scripts can use `window.__REGRESSIFY__` or `window.regressify`.
The context contains these fields, plus other resolved capture settings in `config`:

```json
{
  "requiredLogin": false,
  "scenario": {
    "id": "home",
    "label": "Home",
    "description": null,
    "url": "https://example.com",
      "requiredLogin": false,
      "testSuite": "home"
   },
   "viewport": { "label": "desktop", "width": 1366, "height": 768 },
   "isReference": true,
   "config": { "elementTimeoutMs": 30000 }
}
```

Identity/description fields may be absent or null according to the scenario.
`viewport` is null without a named viewport. `isReference` distinguishes reference
from test captures. Scenario actions and authentication credentials are not
included. `config` uses camelCase resolved names such as `elementTimeoutMs`,
`actionabilityTimeoutMs`, `settleTimeoutMs`, and `scenarioTimeoutMs`.
Use `window.location.href` for the currently navigated URL, especially after URL
replacements, `goto`, or popup selection. `requiredLogin` is a flag, not a login
implementation. An asynchronous browser script should return its promise, for
example with an async immediately invoked function.

For browser script errors, hook timeouts, and hook-versus-action ordering
failures, retrieve `get_authoring_guide(topic="troubleshooting")`.
