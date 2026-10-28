---
name: approve-command
description: 'Use when changing regressify approve, scenario-scoped baseline promotion, explicit run selection, error marker gating, or preservation of unselected references.'
---

# Approve References

## Start Here

[Approve routing](../../../src/compare/actions.rs), [scenario and viewport promotion](../../../src/compare/report_gen.rs), and [atomic copying](../../../src/helpers/io.rs).

## Procedure and Invariants

1. Resolve the requested suite/run (validating explicit `run_id` portability) and validate capture inventory (including `.report-urls.json` when present) before mutation. Explicit run selection must not silently fall back to a different capture run.
1. Scenario-scoped approval needs trustworthy scenario capture metadata; refuse legacy/missing metadata rather than approving more than the user selected.
1. Skip failed capture placeholders, including malformed error markers. Preflight all non-error-marked candidate PNGs as regular valid images before mutating `bitmaps_reference`; promote complete files atomically and preserve good and unselected references.
1. Clear stale reference error markers only after a successful replacement. Report failed I/O as a process error, not an approved visual result.
1. After successful image replacement, preserve its original capture-log link through `promote_capture_log`, including the capture's recorded image simplification setting; a capture without one leaves the reference unrecorded, never inheriting the replaced reference's setting. Diagnostic-link failures warn without changing image results; image-digest checks must reject stale links. Approval creates no new capture execution or capture journal; its separate command journal records promotions/skips without replacing provenance.
1. Results approval pins one scenario and a capture key from the reviewed run. Match exact scenario metadata before deriving its viewport; promote only that viewport's reviewed captures. Preflight all selected PNGs before any promotion.
1. Recalculate the original report scope with saved thresholds/order/metadata after viewport approval. Keep failed siblings and unapproved viewports visible; ordinary CLI/Runs approval remains separate and does not implicitly recompare.

## Checks

```sh
cargo test --lib compare::report_gen::tests
cargo test --lib compare::actions::tests
```

Use temporary output trees. Cover partial selection, failed/malformed markers, missing inventory, explicit run IDs, and copy failures without touching real baselines.

## Maintain

Update [command docs](../../../docs/commands.md), relevant desktop approval docs, and this skill. Keep comments and tests explaining why promotion is gated.
