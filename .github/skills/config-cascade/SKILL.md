---
name: config-cascade
description: 'Use when changing workspace, suite, or scenario configuration, defaults, URL replacements, basic auth, numeric validation, or serde/schema compatibility.'
---

# Configuration Cascade

## Start Here

[Configuration](../../../src/config/mod.rs), [types](../../../src/types/regressify.rs), [URL replacements](../../../src/config/replacements.rs), and [basic authentication](../../../src/config/basic_auth.rs).

## Procedure and Invariants

1. Trace parsing and resolution to the consumer. Preserve scenario -> suite -> workspace -> defaults, explicit false/zero, and list replacement semantics. Snapshot report text is workspace-only; follow [snapshot precedence](../snapshot-command/SKILL.md) instead of the capture cascade.
1. `logLevel` is display-only: CLI `--log-level` (or `--log-verbose`/`--verbose`), then `REGRESSIFY_LOG_LEVEL`, override the file cascade; default is `info`. Keep per-execution console filtering independent of standard archive collection and operation-level journal limits.
1. Keep serde names, unknown-field rejection, schema constraints/descriptions, desktop validation, and effective configuration aligned when adding a field.
1. `pixelMatchThreshold` cascades at all three levels: finite 0-100, default 5, explicit zero preserved. Desktop steps are 0.1. It controls pixel-color sensitivity, not `misMatchThreshold`'s pass/fail percentage; capture manifests preserve the resolved value for later comparison.
1. `lazyLoadScrollDelay` and `lazyLoadTimeout` cascade independently at all three levels. Both are whole milliseconds; defaults are 250 and 20000. Preserve zero delay, reject zero timeout at every scope even if overridden or scrolling is skipped, and map both to their resolved `Ms` keys in desktop fields. `noScrollTop` skips the pass, while `settleTimeout` stays workspace-only and best-effort.
1. Suite `displayName` is an optional presentation label, not a cascading capture option or an ID. Preserve file/output identity and use blank-name fallbacks in desktop and report consumers. Include explicitly set optional fields in schema-coverage fixtures when serialization omits unset values.
1. `imageSimplification` (`none` default or `pixelate`; `none` because YAML 1.1 reads unquoted `off` as false), `imageSimplificationBlockSize` (even 2-16, default 4; Chromium caps convolution kernels at 16), `imageSimplificationMinSize` (at least 1, default 64), and `imageSimplificationExcludeSelectors` cascade independently at all three levels; the selector list replaces inherited lists. `config::validate_image_simplification` checks every scope's explicit values in preflight, even when overridden or the mode is `none`, and names blank selectors by 1-based entry without echoing selector text. Keep its limits equal to schema `minimum`/`maximum`/`multipleOf` and nonblank item constraints; the desktop block-size stepper moves between even values.
1. `autoCleanRuns` and `keepTestRuns` are workspace/suite-only settings, resolving independently to false/1. Explicit suite false opts out. Reject keep counts below 1, retain effective-value/stepper parity, and prune only after full-suite Test completion. See [run retention](../run-retention/SKILL.md).
1. Scenario `selector` and `save.selector` are independent, non-cascading CSS capture options; omitted/null means full page. Keep nonblank constraints and scenario `selector`/`selectors` exclusivity aligned between preflight and generated schema. Explicit plural `selectors` remain compatible and run after terminal saves.
1. Validate finite thresholds, positive concurrency, non-negative delays, browser support, and replacement regexes before fetch/capture. Do not clamp bad input silently or defer configuration errors to individual scenarios.
   Schema metadata must reject zero element/actionability/scenario timeouts and
   empty/blank capture selectors while retaining zero navigation/readiness
   fallbacks, disabled settling, and empty viewport selections. Keep constraints
   on the owning property instead of narrowing shared string/list types globally.
1. Shared viewport validation retains field/index metadata for desktop diagnostics while preserving CLI error text. Desktop semantic diagnostics use pointers and validate well-typed independent fields alongside schema errors, including replacement rules at each cascade scope.
1. Preserve reference versus test URL replacement behavior and precedence of CLI options/environment settings. Compile regexes once at the owning boundary.
1. Resolve configured files through the existing canonical containment helpers. An absolute configured file is allowed only inside the canonical workspace; missing optional defaults and missing explicit files have different semantics.
1. Keep authentication origin-scoped and secrets out of serialized effective configuration, diagnostics, completion examples, and progress events.

## Checks

```sh
cargo test --lib config::
cargo test --lib types::
cargo test --lib image_simplification
cargo test --features desktop --lib desktop::validation::tests
cargo test --features desktop --lib image_simplification
```

Cover all cascade levels, omitted versus explicit values, malformed regexes, numeric boundaries, unknown fields, escaping, and external/symlink file paths.

## Maintain

Update [suite docs](../../../docs/create-test-suite.md), [scenario docs](../../../docs/create-scenario.md), examples, and this skill. Explain exceptions to normal precedence with concise code comments.

Maintain the standalone [MCP guides](../mcp-command/SKILL.md) for changed semantics; field schemas derive from the same types. Run `cargo test --lib mcp` for coverage.
