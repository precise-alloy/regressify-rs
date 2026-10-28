---
name: lifecycle-hooks
description: 'Use when changing CSS overrides, hideSelectors/removeSelectors document CSS, jsOnReadyPath, injected onBeforeScript/onReadyScript browser JavaScript, document navigation hooks, CSP behavior, or script timeouts.'
---

# Injected Browser Hooks

## Start Here

[Document and lifecycle hooks](../../../src/test/hooks.rs), [scenario sequencing](../../../src/test/test_scenario.rs), and [browser fixtures](../../../src/test/browser_tests.rs).

## Procedure and Invariants

1. Resolve hook files through workspace containment checks. Distinguish absent optional default scripts from invalid explicit paths.
1. Apply document CSS/on-ready code once per document, including after navigation. Preserve ready-context data and propagate script errors instead of marking an incomplete hook successful. Review selector CSS and JS serialization separately.
1. Document CSS (the override plus `hideSelectors`/`removeSelectors` rules) is one `<style>` element in each main document. Iframes never receive it, and strict `style-src` CSP silently blocks it unless `bypassCsp` is set ([open issue](../../../OUTSTANDING_ISSUES.md)); do not describe it as reaching every frame. Regressify-owned styles that must survive CSP use CSSOM or adopted constructed stylesheets, like the image simplification filter.
1. All CSS and JavaScript hooks execute in Chromium through CDP, with no Node subprocess or Playwright attachment. Evaluate user files as classic expressions with `runtime::evaluate_main_document`; automatic function detection can invoke declarations, and `Page::evaluate_expression` can reuse a context ID destroyed by navigation. The helper sends typed `Runtime.evaluate` without a cached context ID. Await returned promises, propagate syntax/runtime errors and rejections, and keep the `elementTimeout`/`scenarioTimeout` bounds.
1. Register `onBeforeScript` before initial navigation so synchronous code runs before site scripts. The DOM may not exist yet. Capture errors/rejections for later awaiting and remove the registration after initial navigation; never apply it to later `goto` actions or sibling frames. Asynchronous work does not block site scripts.
1. CSS, `jsOnReadyPath`, then `onReadyScript` run after DOM readiness/`readySelector`, before dependency, scenario, and legacy actions. Both ready scripts follow navigation and `switchTab`, once per document; revisiting a completed document does not rerun them. Explicit `actions`, even empty, suppress legacy shortcuts. With `imageSimplification: pixelate`, setup installs the image filter host and adopted stylesheet after CSS and before the ready scripts; screenshot preparation re-installs them, so hooks need not preserve them.
1. Expose browser metadata through `window.regressify`/`window.__REGRESSIFY__`, including nullable viewport, reference mode, and safe resolved configuration. Do not inject action values or authentication credentials. Serialize metadata as JSON; module exports, callback arguments, Node APIs, and `process.env` are not supported.
1. Scripts are executable project code: use trusted fixtures and synthetic data. Never run user scripts in a separate runtime as preflight validation or add Node/npm installation probes.

## Checks

```sh
cargo test --locked --lib test::hooks::tests -- --include-ignored --test-threads=1
cargo test --locked --lib test::browser_tests::reference_and_test_inject_ready_scripts_without_node -- --ignored --exact
```

Live fixtures require cached managed Chromium and never download it. The full
capture fixture runs ref/test with an empty worker PATH and no workspace npm
packages; it checks CSS, both ready settings, click ordering, peer isolation,
and unchanged references after hook failure. Keep coverage for declarations,
promises, CSP, errors, deadlines, metadata escaping, navigation, and tab selection.
The stale-context regression disables Runtime events before local navigation;
CSS, metadata, and both ready scripts must still reach each new document once.

## Maintain

Update [scenario docs](../../../docs/create-scenario.md), [suite docs](../../../docs/create-test-suite.md), and this skill. Comment lifecycle ownership and script-order decisions, not routine payload assignments.
Maintain the standalone [hooks guide](../../../src/mcp/guides/hooks.md), examples,
and exported Worker script-contract tests when hook contracts change. The Worker's
development tests may use Node to check DOM stubs; that is not a capture dependency.
