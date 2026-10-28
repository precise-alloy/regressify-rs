---
name: capture-stability
description: 'Use when changing screenshot settling, lazy loading, animation freezing, caret hiding, image simplification (pixelation), selector clips, full-page or viewport captures, save names, PNG metadata, or capture manifests.'
---

# Stable Screenshot Capture

## Start Here

[Page stability](../../../src/test/page_stability.rs), [capture](../../../src/test/capture.rs), [scenario execution](../../../src/test/test_scenario.rs), and [PNG safety](../../../src/helpers/png.rs).

## Procedure and Invariants

1. Preserve bounded DOM/resource/font/image/layout readiness and configurable settling. Avoid replacing observable conditions with arbitrary sleeps.
1. Run lazy-load scrolling after initial navigation/readiness hooks and before prerequisite, scenario, or legacy actions so it cannot disturb their hover or scroll state. `noScrollTop` skips this pass; keep it distinct from selecting a viewport-only capture.
1. Bypass CSS smooth scrolling for the lazy-load pass and verify both offsets stay at the top for a quiet window before returning. Bound the completion wait and propagate failure; a fixed delay or later capture settling is not proof that scrolling finished.
1. Resolve `lazyLoadScrollDelay` (default 250 ms, zero allowed) and `lazyLoadTimeout` (default 20000 ms, positive) independently through scenario -> suite -> workspace. Hold each viewport until visible native/deferred images decode and common lazy-loader states finish, including picture sources and LazySizes backgrounds. Use intersections to exclude clipped slides; decoded placeholders are not loaded sources. Recheck growing document height, bound scroll steps, disconnect observers, and restore the top on failure. Do not rewrite application sources or require global network idle. The total deadline includes the stable return and uses the existing runtime CDP client so long waits are not evicted by Chromiumoxide.
1. Stabilize initial animations after readiness hooks and before lazy loading/actions when enabled. Share `prepare_screenshot` for `save` and final captures: resolved delay, animation freeze, bounded settling, then another freeze for late animations. Honor opt-outs and do not scroll or move the pointer during this preparation. `delay` resolves scenario -> workspace -> default; `postInteractionWait` remains final-only.
1. Hide text carets independently of animation freezing and settling. Preserve focus, selection, and values; do not blur controls or weaken comparison thresholds to hide cursor noise. Caret hiding is a main-document `<style>` element, so strict `style-src` CSP silently blocks it unless `bypassCsp`, and iframe carets stay visible ([open issue](../../../OUTSTANDING_ISSUES.md)); new Regressify-owned capture styles should use CSSOM or an adopted constructed stylesheet, as image simplification does.
1. [Image simplification](../../../src/test/image_simplification.rs) (`pixelate`) flags images once in `prepare_screenshot`, after the second freeze and before caret hiding, with no observers or timers; `restore_after` removes the flags after every final and `save` screenshot, even on failure. `none` must not touch the page. Selector captures re-run the paint-order guard after centering the target and after restoring scroll, because fixed or sticky content moves. Keep blocks exact: block-start means copied by separable convolutions fill partial edge blocks, and scaled, zoomed, skewed, or fractionally translated images stay sharp because resampling breaks blocks.
1. Support full-page, viewport, CSS target, and save captures without collisions. Optional `save.selector` clips only that save; scenario `selector` clips only `FINAL`. Keep filenames independent of these selectors, reject blanks and simultaneous scenario `selector`/`selectors`, and restore scroll after clips. `viewport` is reserved in all three selector fields and captures the visible viewport without scrolling. Element clips are the target's border box after centering it, so layers overflowing the target are excluded and overlays inside it are included. With GPU rasterization, full-page captures taller than 16384 px repeat row `y - 16384` below that line ([open issue](../../../OUTSTANDING_ISSUES.md)); keep fixtures shorter unless testing that limit.
1. Plan `save` checkpoints per viewport with the same applicability predicate as execution, including `needs`. Validate portable save names unconditionally, but check conditional collisions against actual per-viewport inventory; disjoint same-name saves are valid. Omit the automatic final capture when the last applicable own action is `save`, but always retain explicit scenario `selectors`. Prerequisite saves alone do not suppress finals. Progress, manifests, errors, and comparison must share this decision; never delete historical baselines.
1. Write complete PNGs/manifests atomically; preserve required color metadata and resource bounds. Failed capture placeholders need error markers, not promotion.
1. Keep labels, scenario IDs, target names, thresholds, and raw/replaced URLs in sync across capture manifests and report consumers.
1. Persist `scenarioDisplayName` from the [shared resolver](../../../src/helpers/scenario_names.rs) against the complete suite before selection or URL replacements. Preserve the source label separately; generated names never feed IDs, capture keys, or filenames. Comparison regeneration must retain recorded names.
1. The shared error-placeholder PNG lives in `src/test/data/error-icon.png`, not the generated snapshot asset folder replaced by frontend builds.

## Checks

```sh
cargo test --lib test::capture::tests
cargo test --lib test::page_stability::tests
cargo test --lib test::test_scenario::tests
cargo test --lib helpers::png::tests
```

For scenario and checkpoint preparation, with managed Chromium already installed:

```sh
cargo test --lib test::test_scenario::tests::lazy_loading_precedes_actions_and_preserves_hover -- --ignored --exact
cargo test --lib test::page_stability::tests::lazy_loading_ -- --ignored
cargo test --lib test::page_stability::tests::screenshot_preparation_hides_carets_without_blurring_inputs -- --ignored --exact
cargo test --lib test::test_action::tests::save_actions_wait_for_the_resolved_screenshot_delay -- --ignored --exact
cargo test --lib test::test_scenario::tests::selector_saves_and_final_captures_match_pixels_and_inventory -- --ignored --exact
```

The selector fixture verifies default/full-page pixels, independent save/final clips, quoted selectors, skipped finals and their waits, explicit plural targets, viewport filtering, missing/hidden/invalid targets, preserved completed saves, and restored scroll offsets.

Cover absent/empty actions with capture settling disabled, smooth scrolling, delayed or interrupted returns, non-scrolling pages, and blocked-scroll timeouts. Checkpoint coverage must verify saved pixels, inherited/overridden delay, repeated saves, invalid-name rejection before waiting, animation opt-outs, and animations started during settling. Caret coverage must check inputs, textareas, and editable content with animation freezing both on and off, unchanged focus/selection/values, and identical screenshot pixels across a blink interval.

The lazy-load group includes a 61-second stop to cross Chromiumoxide's periodic
request eviction interval. Loopback fixtures verify delayed native/picture/background
pixels, placeholders, growing pages, clipped/hidden slides, zero/large pauses,
deadline failures, and no continued scrolling after failure. Also run the `lazy_load`
filter with `--features desktop` for cascading values and editor/schema validation.

Inspect ignored Chromium fixtures for live behavior; unit tests alone do not prove rendering stability. Cover delayed resources, motion, clipped targets, multiple saves, invalid names, timeouts, and write failures.

Run `cargo test --lib test::test_scenario::tests::filtered_reference_and_test_manifests_preserve_baselines -- --ignored --exact` for real reference/test manifests, ordered pixels, all-actions-skipped final captures, scoped failure placeholders, peer isolation, and unchanged historical references. It uses loopback pages and cached Chromium only.

For image simplification, run `cargo test --lib image_simplification`, then with cached Chromium `cargo test --lib test::browser_tests::image_simplification_ -- --ignored --test-threads=1`. The page fixture checks flags and counts, exact blocks (including 67 px partial blocks), unchanged pixels outside pixelated images and under overlays, guarded overlay scenes, mode `none` without DOM changes, restore, re-installation after removal, strict CSP, and `<base href>`. The `ref`/`test` fixture checks block means against the source, saves, finals, the sticky-header selector refresh, `switchTab`/`closeTab`, identical runs, and count-only journals. Odd image sizes matter: strips appear only when the width modulo the block size is between 1 and half the block size minus one.

## Maintain

Update [scenario docs](../../../docs/create-scenario.md), [action docs](../../../docs/create-action.md), [capture stability](../../../docs/commands.md#capture-stability) and [image simplification](../../../docs/commands.md#image-simplification) command docs, and this skill. Explain readiness heuristics and filename/metadata invariants in concise comments.
