# Scenario Recipes

These complete synthetic suites show common page patterns. Replace every URL,
selector, label, and value with ones verified against the user's application;
never run a recipe against a page the user has not authorized. Each block is a
whole `visual_tests/<name>.tests.yaml` file unless stated otherwise. Read the
scenarios and actions topics for exact semantics.

## Mask Changing Content

```yaml
hideSelectors:
  - "[data-testid=current-time]"
  - ".ad-slot"
removeSelectors:
  - "#chat-widget"
scenarios:
  - id: home
    label: Home
    url: https://example.com
```

`hideSelectors` keeps layout space (`visibility: hidden`); `removeSelectors`
collapses it (`display: none`). Both lists add inline CSS to the selected tab's
main document before actions. They do not reach iframes, a strict `style-src`
can block them, and selectors that match nothing are ignored. The `hide` and
`remove` actions instead wait for a match and fail when none appears, so use them
only for elements that always exist at that step. Never hide an element a later
action must use, and mask only nondeterministic regions such as clocks, ads, or
randomized recommendations.

## Dismiss a Consent Banner

When the banner always appears, accept it as the first action:

```yaml
scenarios:
  - id: pricing
    label: Pricing
    url: https://example.com/pricing
    actions:
      - click: "#accept-cookies"
```

When it appears only sometimes, for example by region or after earlier consent,
a click would fail. Remove it instead, or seed the site's consent cookie in
`common/_cookies.yaml` (see the `cookies` example):

```yaml
removeSelectors:
  - "#cookie-banner"
scenarios:
  - id: pricing
    label: Pricing
    url: https://example.com/pricing
```

Removing the banner does not record consent, so consent-gated content may still
differ. An uncovered target is required for clicks: a banner over a button
shows up as an `Unobscured` failure.

## Wait for Results After Navigation

```yaml
scenarios:
  - id: search-results
    label: Search results
    url: https://example.com
    actions:
      - input: "#search"
        value: running shoes
      - press: "#search"
        key: Enter
      - wait: "[data-testid=results]"
        url: "**/search*"
```

The URL must match first, then the results element must become visible. Clicks
and key presses await only navigation that starts immediately; single-page
applications need such explicit waits. Prefer element waits to fixed sleeps.

## Fill a Form

```yaml
scenarios:
  - id: contact-form
    label: Contact form
    url: https://example.com/contact
    actions:
      - input: "#name"
        value: Test User
      - input: "#email"
        value: qa@example.com
      - input: "#order-number"
        value: "12345"
      - select: "#topic"
        label: Support
      - check: "#terms"
      - save: filled
      - click: "button[type=submit]"
      - wait: "#confirmation"
```

Use synthetic data and an environment that accepts test submissions. Quote
numeric-looking text. `select` requires a native `<select>`, and `check`/`uncheck`
require native `<input type="checkbox">` or `<input type="radio">` elements.
For custom dropdowns (`role="combobox"` or `<button>` triggers), `click` the
trigger, `wait` for the option, and `click` the option; for custom switches
(`role="switch"`), ARIA checkboxes (`role="checkbox"`), or visually hidden
native inputs, `click` the visible button or `<label>` instead of using `check`.
For uploads, set `useFileChooser: true` on the visible upload button when the
file input is hidden.

## Sign In Without Committing Secrets

Action values are stored literally in the suite, and only `basicAuth` expands
environment variables. Choose the lightest option that fits:

1. HTTP Basic Auth: `basicAuth` with `${VARIABLE}` placeholders.
1. Session cookies exported from a manual sign-in into a local, ignored cookie
   file referenced by `cookiePath`.
1. A sign-in scenario that saves state with `persist`, run before suites that
  load it with `state`. Use a dedicated test account approved for this, and keep
  private sign-in suites and saved state local and ignored by version control.

Browser hooks cannot read `process.env` or local files. Use Basic Auth placeholders
or an authorized cookie/state file when authentication needs private values.

```yaml
# visual_tests/sign-in.tests.yaml
scenarios:
  - id: sign-in
    label: Sign in
    url: https://example.com/login
    actions:
      - input: "#username"
        value: visual-test-user
      - input: "#password"
        value: synthetic-test-password
      - click: "button[type=submit]"
      - wait: "#account-menu"
        url: "**/account*"
      - persist: signed-in
```

```yaml
# visual_tests/account.tests.yaml
state: signed-in
scenarios:
  - id: account
    label: Account overview
    url: https://example.com/account
```

Run `regressify test --test-suite sign-in` first, then the dependent suite.
State files are validated before capture, so one command containing both suites
fails until the state exists, and otherwise may read an older file. Each
`--site-env` output directory has its own `states/` folder. Keep state and
cookie files out of version control and out of MCP requests.

## Header Navigation and Menus

```yaml
viewportNames: [phone, desktop]
scenarios:
  - id: header
    label: Header
    url: https://example.com
    selector: viewport
  - id: products-menu
    label: Products menu
    url: https://example.com
    actions:
      - click: "#menu-toggle"
        viewportNames: phone
      - click: "#products-toggle"
      - wait: "#products-panel"
    selectors:
      - viewport
      - "#products-panel"
  - id: account-menu
    label: Account menu
    url: https://example.com
    viewportNames: desktop
    actions:
      - hover: "#account-toggle"
      - wait: "#account-dropdown"
    selector: viewport
```

The closed/default `header` and opened/expanded menu scenarios use the same larger
capture area: `viewport`. A header-only default screenshot could miss a menu
incorrectly expanded before interaction. The initial scenario therefore takes
no opening or forced-closing action and retains the area where a menu could
appear. Keep matching viewport sizes and scroll positions for QA's cross-state
inspection; each capture still compares against its own reference. Choose full
page consistently instead when the relevant area extends beyond the viewport.

Opened panels usually extend past the header, where a `header` capture would
cut them off. `products-menu` adds a panel detail without replacing its common
viewport image; the default scenario does not require that panel to be visible.
On phones
the first click opens the navigation drawer; its `viewportNames` skips it in
desktop jobs. The hover-opened account menu uses `viewport`, which never
scrolls, so the pointer stays on its trigger. Each viewport label from
`common/_viewports.yaml` is a separate capture job, sized in CSS pixels without
a mobile user agent or touch emulation. Read the scenarios topic for other
opened layers, such as dialogs, popovers, and search suggestions.

## Popups and New Tabs

```yaml
scenarios:
  - id: quote-popup
    label: Quote popup
    url: https://example.com/products/sample
    actions:
      - click: "a[target=_blank]"
      - switchTab: new
        url: "**/quote*"
      - wait: "#quote-form"
      - save: quote
      - switchTab: original
```

A click never selects the new tab by itself. After `switchTab: new`, actions and
checkpoints use the popup; `switchTab: original` returns before the final
full-page capture of the first tab.

## Content Inside an Iframe

```yaml
scenarios:
  - id: embedded-form
    label: Embedded form
    url: https://example.com/checkout
    actions:
      - wait: "#card-holder"
        frame: "iframe#payment"
      - input: "#card-holder"
        value: Test User
        frame: "iframe#payment"
    selectors:
      - "iframe#payment"
```

Frames may be cross-origin; list nested frames from outermost to innermost.
Scenario-level screenshot selectors run in the main document, so capture the
`iframe` element rather than a selector inside it, or use a `save` action with
both `frame` and `selector` to clip a specific element inside the frame. Never
enter real payment data.

## Components and Checkpoints

```yaml
scenarios:
  - id: product-page
    label: Product page
    url: https://example.com/products/sample
    selectors:
      - viewport
      - "[data-testid=price-box]"
      - "#reviews"
  - id: cart-steps
    label: Cart steps
    url: https://example.com/cart
    actions:
      - save: empty-cart
      - click: "[data-testid=add-sample]"
      - wait: "[data-testid=cart-line]"
      - save: one-item
        selector: "[data-testid=cart]"
```

`selectors` creates one image per target after all actions. A `save` captures at
its position; because `one-item` is the last action, `cart-steps` has no
automatic final image. Component captures keep unrelated page-height changes
from failing comparison.

## Reuse Opening Steps

```yaml
scenarios:
  - id: open-filters
    label: Filters open
    url: https://example.com/products
    actions:
      - click: "#filters-toggle"
  - id: in-stock-products
    label: In-stock products
    url: https://example.com/products
    needs: open-filters
    actions:
      - check: "#filter-in-stock"
      - wait: "[data-testid=product-grid]"
```

`needs` replays the `open-filters` actions inside `in-stock-products` after it
opens its own `url`; both scenarios are still captured independently.

## Lazy Content and Late Rendering

```yaml
scenarios:
  - id: gallery
    label: Gallery
    url: https://example.com/gallery
    readySelector: "[data-testid=gallery]"
    actions:
      - scroll: "#footer"
      - wait: "#footer img"
      - scroll: "[data-testid=gallery]"
```

Each page is scrolled once before actions to trigger lazy loading; set
`noScrollTop: true` when scrolling changes state. Before every capture
Regressify freezes CSS and Web Animations (`document.getAnimations`), waits up
to `settleTimeout` for fonts, images, and layout, and hides text carets. Use
`readySelector` for content that must exist before any step, explicit waits for
content created by steps, and a larger `delay` only as a last resort. JavaScript
timer/`requestAnimationFrame` loops (such as rotating hero text or JS carousels)
and `<video>` elements are not frozen automatically and can keep DOM settling
active until `settleTimeout` expires; pause or stabilize them in a browser ready
script or mask nondeterministic regions.

## Staging Versus Production

```yaml
urlReplacements:
  - ref: https://www.example.com
    test: https://staging.example.com
scenarios:
  - id: home
    label: Home
    url: https://www.example.com
```

`ref` captures URLs as written; `test` applies profile, workspace, and suite
replacements to scenario URLs, `goto`, `wait.url`, and `switchTab.url`. Use
`common/_replacement-profiles.yaml` with `--replacement-profile` to switch
targets, and `--site-env <name>` to keep each environment's baselines and state
in `.regressify__<name>/`.
