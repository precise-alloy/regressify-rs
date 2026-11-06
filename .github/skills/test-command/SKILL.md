---
name: test-command
description: 'Use when changing regressify test orchestration, scenario isolation, replacements, capture concurrency, progress events, or skip-compare behavior.'
---

# Test Orchestration

## Start Here

[TestArgs](../../../src/cli/args.rs), [exec_test](../../../src/test/test_suites.rs), [scenario execution](../../../src/test/test_scenario.rs), and [progress events](../../../src/progress.rs).

## Procedure and Invariants

1. Validate configuration and selection before launching browser jobs. Keep scenario/suite/output IDs distinct when translating exact selections.
1. Test mode resolves each suite's replacement profile (`--replacement-profile` -> `REPLACEMENT_PROFILE` -> suite `replacementProfile` -> `default`), processes profile items (`UrlReplacement` and `ScriptReplacement` with `REGRESSIFY_MODE=test`) from first to last, and applies workspace/suite `urlReplacements` (`find -> test` or `ref -> test`) to initial, goto, wait, and new-tab URL filters. Keep raw reference URLs in capture metadata for later review links.
1. Bound capture concurrency and launch ramping. Isolate each job's browser and profile; stop subsequent actions only within the failing scenario.
1. Compare captured output right after each scenario/viewport capture finishes unless `--skip-compare` is set. Stage diffs in `.compare-staging-*` and commit each suite's report when all of that suite's jobs finish without infrastructure failures, independently of peers. Missing references, mismatches, and website/action failures are report results, not non-zero exits. Follow [run retention](../run-retention/SKILL.md) for registry completion, full-suite defaults, deferred comparisons, and automatic cleanup.
1. Preserve infrastructure/artifact error classification through spawned tasks and joins. A failed error-placeholder write must not masquerade as page failure.
1. After compared runs, rebuild the combined snapshot only when the run covered every discovered suite and scenario (`post_run_snapshot_decision`, sharing the `RunLease` full-suite predicate). Partial runs print one `regressify snapshot` reminder; desktop workers pass `PostRunSnapshot::Skip`. Never let unrelated suites decide a scoped run's exit code.
1. Keep progress job identities stable across intermediate/target captures; during compared `test` runs, each job transitions `Running -> Comparing -> Passed/Failed` so overall job progress advances monotonically from 0% to 100% without resetting.

## Checks

```sh
cargo test --lib test::test_suites::tests
cargo test --lib test::test_suite::tests
cargo test --lib progress::tests
```

Use local browser fixtures for sibling failure isolation, timeouts, replacements, and cancellation. Cover zero jobs, partial selection, and infrastructure failures. Post-run report scope has `cargo test --lib post_run_snapshot` plus the ignored `desktop_worker` checks in the [snapshot skill](../snapshot-command/SKILL.md#checks).

## Maintain

Update [command docs](../../../docs/commands.md), [scenario docs](../../../docs/create-scenario.md), and this skill when orchestration changes. Explain non-obvious concurrency and error-classification decisions in code.
