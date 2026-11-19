---
name: selector-escaping
description: 'Use when interpolating selectors or page data into JavaScript, CSS rules, document hooks, frame traversal, or generated report markup.'
---

# Selector and Sink Escaping

## Start Here

[Escaping helpers](../../../src/test/actionability.rs),
[action sinks](../../../src/test/test_action.rs), and
[document hooks](../../../src/test/hooks.rs).

## Procedure and Invariants

1. Identify each parsing layer: Rust string -> JS literal -> selector/CSS rule,
   or HTML/URL sink. Escaping for one layer is not protection for the next.
1. Prefer JSON serialization for JS values. Reuse `escape_js_string` only for its
   single-quoted JS context; quote-only replacement is insufficient.
1. Inspect the exact contract of `escape_css_rule` before reusing it. Do not apply
   JS escaping blindly to raw CSS text or double-escape valid selectors.
1. Audit action-level hide/remove and config-level hideSelectors/removeSelectors
   separately. Serializing an entire stylesheet safely into JS does not constrain
   a selector's ability to introduce extra CSS rules. Hide/remove actions use
   native lookup, assign `CSSStyleRule.selectorText`, and insert only the
   serialized rule into the frame's CDP inspector stylesheet (independent of
   inline style CSP and page-owned adopted sheets). Enable CSS/DOM only on an
   action-owned CDP connection. An outer timeout can skip async cleanup, but
   dropping this connection detaches its sessions without enabling events on
   the scenario connection. Config lists still use `escape_css_rule`.
   In a resolved frame `document` is the action-selector
   proxy, not a native CSS lookup. Keep screenshot and stylesheet targets in
   the light DOM of `window.document`.
1. `imageSimplificationExcludeSelectors` reach the page only inside one JSON
   options literal and are used through `matches`/`closest`, never CSS text; the
   filter rule is built from generated IDs. Keep
   [the escaping regression](../../../src/test/image_simplification.rs) asserting
   the script prefix is the fixed wrapper plus that literal.
1. Prefer text content and DOM APIs for display data; explicitly validate link
   schemes and HTML/Markdown policy in generated reports.

## Checks

```sh
cargo test --lib test::actionability::tests
cargo test --lib test::test_action::tests
cargo test --lib css_only_selectors_in_ -- --ignored
cargo test --lib cancelled_inspector_session_does_not_stream_stylesheet_events -- --ignored
```

The ignored fixtures require cached Chromium. Test quotes, backslashes,
braces, angle brackets, CR/LF, tabs, NUL, Unicode, and
valid attribute selectors. For CSS/DOM changes use a local browser fixture and
assert both intended selection and absence of effects on unrelated elements.

## Maintain

Keep helper comments explicit about the sink they protect. Update
[action docs](../../../docs/create-action.md), relevant hook/config docs, this
skill, and the issue list when a previously unchecked sink is confirmed or fixed.
