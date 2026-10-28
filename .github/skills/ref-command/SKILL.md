---
name: ref-command
description: 'Use when changing or reviewing regressify ref, baseline capture, raw reference URLs, failed capture handling, or reference promotion.'
---

# Reference Capture

## Start Here

[RefArgs](../../../src/cli/args.rs), [exec_ref](../../../src/test/test_suites.rs),
[capture](../../../src/test/capture.rs), and [promotion](../../../src/compare/report_gen.rs).

## Procedure and Invariants

1. Validate selected suites, IDs, dependencies, viewports, and configuration before
   browser fetch or reference mutation. Preserve exact scenario selection.
1. Reference mode uses raw scenario/goto/wait URLs. The accepted replacement-profile
   option must not silently apply test-environment substitutions in reference mode.
1. Capture before promotion. Only successful screenshots may replace references;
   never delete good baselines first or promote error placeholders/sidecars.
1. `promote_reference_captures` copies each PNG atomically, then rewrites its
   `.log.json` through `promote_capture_log`, carrying the capture's archive
   reference and recorded image simplification setting. The link binds to the
   image digest, so readers ignore a link left over from the replaced image.
1. A failed website/action stops its scenario but not peers; it remains a report
   result with zero exit. Config, launch, and artifact-write failures are non-zero.
1. Preserve manifests, self-approved report generation (`generate_reference_report`, without running pixel comparison against existing images), and unselected references during partial runs. Test failures at each stage, including promotion after some captures pass.

## Checks

```sh
cargo test --lib test::test_suites::tests
cargo test --lib compare::report_gen::tests
```

For recorded image simplification settings, this ignored fixture runs real
`ref`, `test`, and a changed-setting `test` with cached Chromium, then checks
both sides' links and the report and console warnings:

```sh
cargo test --lib test::browser_tests::image_simplification_ref_and_test -- --ignored
```

For live changes use local fixtures with one passing and one failing scenario;
assert the old baseline survives and the failed placeholder is not promoted.
Never run a reference-mutating probe against the user's real output directory.

## Maintain

Update [command docs](../../../docs/commands.md) and this skill with capture,
filtering, or exit changes. Comment the capture-before-promotion invariant where
it is enforced; keep regression coverage for failed writes and partial runs.
