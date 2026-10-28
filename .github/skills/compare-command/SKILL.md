---
name: compare-command
description: 'Use when changing regressify compare, capture-run selection, image diff thresholds, manifests, comparison concurrency, or report regeneration.'
---

# Compare Captures

## Start Here

[Command selection](../../../src/compare/actions.rs), [report generation](../../../src/compare/report_gen.rs), and [pixel comparison](../../../src/compare/image_compare.rs).

## Procedure and Invariants

1. Honor explicit run IDs before fallback to the latest completed full-suite run in `runs.json`; preserve legacy fallback without a registry. Hold suite write ownership during comparison/approval. Resolve suite globs and scenario IDs against output/capture metadata, not current source ordering, and ignore reserved non-suite directories (`_snapshot`, `_diagnostics`, `_landing`, `states`, and hidden staging folders). See [run retention](../run-retention/SKILL.md).
1. Validate manifests and expected capture inventory through bounded regular-file readers. Generated diffs are not source captures; derive diff filenames from `screenshot_key` so step names containing `.png` keep a single `_diff.png` suffix, and remove stale diffs when a pair becomes a pass.
1. Bound parallel comparison with `asyncCompareLimit`; stage generated diff PNGs in `.compare-staging-*` inside worker threads before committing to `bitmaps_test/<run_id>` and `html_report`, and preserve manifest ordering, per-capture thresholds, labels, target names, and reference/test page URLs.
1. Keep pixel sensitivity separate from pass/fail percentage. `pixelMatchThreshold` uses 0-100 (default 5), normalized by 100 only in the image comparer. Read per-capture values from `.report-pixel-thresholds.json`, including prefix fallback for intermediate captures; absent metadata uses 5, malformed metadata is an artifact error.
1. Read optional suite-root `suite.json` through the bounded metadata reader and emit `displayName` independently of `id`/`testSuite`. Captures write metadata even with skip-compare. Malformed metadata is an artifact error, not an absent label.
1. Missing references, capture errors, and visual mismatches remain report results. Malformed manifests, unreadable files, and report-write failures are process errors.
1. Preserve PNG safety limits and signed dimension differences. Do not conflate a size mismatch with a decoding or I/O failure.
1. Persist `rawMisMatchPercentage` as the unrounded `f64` and optional integer `mismatchPixels` from the comparer's actual count, including zero. Do not write formatted percentage strings or reconstruct counts from percentages. Preserve missing legacy counts as unavailable. CLI formatting is display-only; verify one-pixel failures and bit-exact JSON round trips with `cargo test --lib mismatch`.
1. Publish `captureInventoryComplete` with the raw report in the same atomic write. Scenario-scoped comparison marks it false; full comparison and capture finalization mark it true. Scoped approval retains the reviewed report's coverage. Default consumers must not mistake a partial report for full-run results; absent flags preserve legacy compatibility.
1. Persist the comparer's chosen `alignment` on report `diff`: optional `reference`/`test` gaps and multi-band `referenceGaps`/`testGaps` contain strictly increasing source-row `start` and positive `height` with disjoint aligned intervals. Skip byte-identical prefix/row slices, prevent unaligned top-of-page changes from pulling `start` across intact middle sections, support large, near-bottom, multi-band, and same-height viewport insertions when contested rows align, and apply anti-aliasing detection across mapped source rows. Preserve the exact decision used to render the diff, including explicit unshifted metadata. Older reports omit it. Do not reconstruct alignment from dimensions or magenta pixels in consumers; rerun Compare for legacy captures.
1. Partial-row anchors need local informative signature evidence, not mandatory near-perfect rows or votes from a different region. Track each half's horizontal and vertical variation independently; a constant half cannot inherit information from the other half. Retain clean-row penalties, minimum support, deterministic ties, bounded candidate/depth searches, and canvas pixel/dimension limits.
1. Report-viewport approval recalculates the reviewed capture list, order, and per-pair thresholds, not a narrowed selection or the current workspace settings.
1. Persist `pixelMatchThreshold` on report pairs. A reviewed legacy report without it used normalized 0.1, so deserialize that missing field as 10 for approval; do not substitute the new-capture default.
1. Read both capture links once per pair (`attach_capture_metadata`). Attach optional pair `imageSimplificationWarning` only to compared pairs, not missing references or capture failures, when recorded settings differ; a missing or stale record counts as `none`. Never change status or exit codes. Reviewed-report approval recalculates it instead of copying it. Print it per pair plus a suite count; `compare` also journals a Warn decision with fingerprinted identities only. Snapshot conversion and desktop Results ignore the field for now.

## Checks

```sh
cargo test --lib compare::
```

Cover explicit/latest run choice, missing/unexpected captures, repeat generation, threshold boundaries, parallel order, malformed images, and failed artifact writes.

For alignment quality and latency, run the opt-in synthetic 1440x10000 benchmark:

```sh
cargo test --release --lib full_page_comparison_quality_and_timing -- --ignored --nocapture --test-threads=1
```

It verifies exact alignments and reports three-run median/max times including PNG reads, decoding, alignment, and diff encoding. Use the release profile; do not enforce wall-clock assertions in ordinary CI or increase search bounds without measurement.

## Maintain

Update [command docs](../../../docs/commands.md) and downstream desktop/report consumers when changing persisted report shape. Explain ordering/error invariants with concise comments and keep this skill current.
