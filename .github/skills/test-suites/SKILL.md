---
name: test-suites
description: 'Use when authoring or debugging visual_tests YAML/JSON suites, scenario IDs, needs dependencies, suite discovery, viewport selection, and test data.'
---

# Test Suite Authoring

## Start Here

- [Suite and scenario types](../../../src/types/regressify.rs).
- [Discovery and dependency resolution](../../../src/test/test_suites.rs).
- [Existing suites](../../../visual_tests) and [suite docs](../../../docs/create-test-suite.md).

## Procedure and Invariants

1. Use `visual_tests/<name>.tests.yaml`, `.tests.yml`, or `.tests.json`. Preserve discovered paths; format precedence is YAML, YML, then JSON. Same-priority duplicate suite names are ambiguous, including across subdirectories.
1. Use documented `camelCase` fields and explicit readable scenario labels/IDs. Unknown fields are errors. Keep generated IDs and case-insensitive uniqueness consistent with capture names and desktop selection.
1. Resolve configuration as scenario -> suite -> workspace -> defaults. Lists replace rather than implicitly append. Consult the types before adding a key.
1. `needs` reuses dependency actions within the scenario; it is not scheduling or shared browser state. Reject missing/duplicate/cyclic dependencies and excessive depth before launching browsers. Preserve `ScenarioDependencyError` field/index metadata for desktop source locations without changing CLI messages; desktop save validation must not depend on action viewport filters. Do not make tests depend on run order.
1. Match waits to observable page state. URL waits support exact URLs, segment `*`, multi-segment `**`, and query-value wildcards. Test mode applies replacements; reference mode uses raw URLs.
1. Prefer a stable standard CSS selector verified on both environments at the intended viewports. For a wording check, use a CSS `wait` with `text` or `containsText`. Reserve Playwright extensions for fields that accept them and targets CSS cannot reliably distinguish.
1. Use target `selectors` only for intended captures: full page, `viewport`, or CSS clips. Do not assume captures always use full-page mode. Use hide/remove for nondeterministic regions without hiding the behavior under test.

## Checks

```sh
cargo test --lib test::test_suite::tests
cargo test --lib test::test_suites::tests
cargo test --lib config::
cargo test --features desktop --lib desktop::document::tests::every_shipped_suite_allows_root_field_edits
```

Add parsing/dependency tests for success, unknown keys, duplicate IDs/formats, cycles and depth limits. Run live suites only against authorized/local pages; real credentials and persisted browser state do not belong in fixtures.

## Maintain

Update [scenario docs](../../../docs/create-scenario.md), [suite docs](../../../docs/create-test-suite.md), and examples for changed schema or discovery behavior. Keep this skill aligned with actual parsing and defaults.
