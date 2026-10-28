# Image Simplification

Verified: 2026-10-26

Opt-in in-page pixelation of large images just before screenshots. Scope limits,
the deferred report/desktop badge, and related capture defects are in the
[outstanding issues](../../OUTSTANDING_ISSUES.md).

## Configuration

- [Suite/scenario types](../../src/types/regressify.rs) and the
  [workspace config](../../src/config/mod.rs) define `imageSimplification`
  (`none` default, `pixelate`), `imageSimplificationBlockSize` (even 2-16,
  default 4), `imageSimplificationMinSize` (at least 1, default 64), and
  `imageSimplificationExcludeSelectors` at all three scopes; `resolve_config`
  resolves each independently scenario -> suite -> workspace -> default, and the
  selector list replaces inherited lists.
- `none`, not `off`: YAML 1.1 tooling reads an unquoted `off` as `false`.
- `config::validate_image_simplification` runs in workspace, suite, and scenario
  preflight in [suite validation](../../src/test/test_suites.rs) for every
  scope's explicit values, even when overridden or the mode is `none`. Blank
  selector errors cite a 1-based entry and never echo selector text. Generated
  schemas carry the same limits; a config test compares them with preflight.
- `ResolvedConfig` serializes these names (desktop Effective view, page-visible
  `window.regressify.config`); desktop Fields add an even-only block-size stepper.

## Browser Behavior

- [Page library](../../src/test/image_simplification.js) and
  [wrapper](../../src/test/image_simplification.rs): `ensure`, `mark`, `refresh`,
  and `clear` run as separate main-document evaluations with one JSON options
  literal; no observers, timers, or state between calls. `none` builds no
  simplifier, so no page work happens.
- `ensure` runs in [document hook setup](../../src/test/hooks.rs) after CSS and
  before ready scripts, and again before marking; it re-creates a removed host
  or adopted sheet. The host is a zero-size `<svg>` under `<html>` styled through
  CSSOM and the rule lives in a constructed stylesheet, because strict
  `style-src` CSP silently drops injected `<style>` elements, as it does for hook
  CSS and caret hiding. IDs carry a per-process token.
- Filter: block-start means (`feConvolveMatrix`, target 0, edge duplicate), one
  sample per block (tiled flood plus composite `in`), then two separable copy
  convolutions (divisor 1, edge none). The approved centre-sample/dilation chain
  had equal means but left transparent right/bottom strips when width mod block
  was 1 to block/2-1 and erased images under half a block; copying is no slower.
- Scale, zoom, skew, non-quarter-turn rotation, or fractional translation on the
  image or an ancestor, and SVG images whose screen CTM scales, gave seams or one
  flat colour with both chains, so they stay sharp (`transformedImages`). Integer
  translation, flips, quarter turns, `will-change`, fixed/sticky positions, and
  fractional layout positions keep exact blocks.
- `mark`: clear flags, ensure, count and ignore invalid exclusions (like invalid
  `hideSelectors` rules), then classify visible light-DOM `img`, `video`, and
  `svg image` elements: small, excluded (`closest`, so containers work), already
  filtered, transformed, then the paint-order guard. Flags are `pixelate` or
  `sharp`; a promoted image whose computed filter does not apply (page cascade
  wins) is unflagged and counted as filtered.
- Paint-order risk: a filter makes a non-positioned image a z-index auto stacking
  context, so it can paint over CSS-grid overlay text, a negative-margin badge,
  or an earlier positioned z-index auto badge; a later positioned overlay stays
  above. The guard (CSS 2.1 Appendix E) keeps it sharp when it overlaps (a)
  later-painting in-flow content of its layer (nearest positioned or stacking
  ancestor; float/atomic phases, order-modified tree order) or (b) earlier
  positioned/stacking z-index auto/0 content in that layer. Positive and negative
  z-index keep their order; later block backgrounds already paint beneath.
  Overflow clips honour containing blocks; SVG images never change order.
- `restore_after` removes flags after the final-capture loop and after each `save`,
  even on failure. [Selector captures](../../src/test/capture.rs) `refresh` the
  guard after centring the target and after restoring scroll; full-page
  `captureBeyondViewport` keeps fixed elements at their current viewport position,
  so geometry measured before full-page and viewport captures stays valid.
- Diagnostics: one Decision per preparation on `condition: images` with seven
  count fields (Warn for invalid selectors); refresh records add debug counts.
- Cost on a synthetic 11k-element page with 400 candidates: about 0.7-0.9 s per
  marking pass and 0.6-0.7 s per refresh; the bound is 30 s.

## Recording and Warnings

- [Capture links](../../src/test/capture.rs) (`.log.json`) record
  `imageSimplification`: `{"mode":"none"}`, or `pixelate` with `blockSize`,
  `minSize`, and `excludeSelectorsDigest` (SHA-256 of the sorted, de-duplicated
  selector JSON array, pinned in tests). `none` omits its inert keys.
- Why links: they sit beside every run and reference PNG, `ref`/`approve` rewrite
  them after the atomic PNG copy, `optimize` and suite rename keep them, and the
  image digest voids stale ones; run manifests never reach `bitmaps_reference`.
- `write_execution_logs` records the job's resolved setting before and after
  capture; `needs` actions share the job config. A promoted capture without a
  setting leaves the reference unrecorded instead of inheriting the old one.
- [Comparisons](../../src/compare/report_gen.rs) attach `imageSimplificationWarning`
  to compared pairs whose settings differ (missing counts as `none`), print it
  per pair plus a suite count, and never change status or exit codes. Only
  `compare` journals it; `test` finalization lacks the command context.

## Design Basis

Reported by the approved 2026-10-23 experiments on real pages, not re-measured:

- Images were about 86% of screenshot bytes on six pages. In-page 4 px block
  averaging left about 25% of the bytes (8 px about 18%); transcoding image
  responses in the network layer left 38-82% and was rejected.
- Filters changed no page dimensions in 96 captures and did not change capture
  time; 144 interleaved comparisons showed no pixelation-induced mismatches.
- Averaging shrinks per-pixel differences; point sampling can amplify them.
- Paint-order changes affected 0 of 726 images on real pages; a z-index-unaware
  prototype guard falsely kept 1 of 291 images sharp on one page.
- Inside uniform blocks anti-alias detection cannot forgive pixels, so
  `pixelMatchThreshold` above 0 is the only tolerance there; docs say to keep it.

## Open Issues

- Guard blind spots (closed shadow roots, generated content of
  `display: contents` elements, in-flow generated content offsets) are in the
  outstanding issues; none has been observed on a real page.
- GPU full-page captures taller than 16384 px repeat rows (seen on the measured
  pages, reproduced with a loopback probe); judge tall captures with that in mind.

## Evidence

Unit tests: `cargo test --lib image_simplification`, also with `--features desktop`.
Live fixtures with cached Chromium: `cargo test --lib test::browser_tests::image_simplification_ -- --ignored --test-threads=1`.
Temporary mutations (guard allows all, no selector refresh, no restore after
`save`, approved dilation chain with a 67 px image) each failed those fixtures.
