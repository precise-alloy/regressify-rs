---
name: desktop-results-review
description: 'Use when changing desktop comparison reports, image previews, naming/filtering, diff dimensions, per-image URLs, capture inventory, or approval selection.'
---

# Desktop Results Review

## Start Here

[Report models](../../../src/desktop/results.rs), [results UI](../../../src/desktop/ui/reports.rs), and [approval intent](../../../src/desktop/ui/runs.rs).

## Procedure and Invariants

1. Treat report JSON and relative image paths as untrusted. Enforce artifact-root containment, file/byte/image-size bounds, and safe external URL schemes.
1. Suite labels prefer the source catalog's nonblank `displayName`, then captured report metadata for unmatched suites, then legacy names/IDs. Labels never control artifact paths or approval identity. Refresh catalogs/results after suite rename/delete and preserve unrelated reports.
1. Keep captured human labels separate from immutable capture keys/scenario IDs. Prefer recorded `scenarioDisplayName`; use the [shared URL naming resolver](../../../src/helpers/scenario_names.rs) for legacy reports. Resolve across logical scenarios, not individual viewports/captures, and keep desktop/editor/web names aligned. Legacy missing metrics are unavailable, not evidence of a pass or zero difference.
1. Do not offer a file-based Open HTML report action: the web snapshot requires HTTP hosting for assets, modules, JSON, and routes. Keep native Results, artifact-folder access, and published Open Site links. `local_html_report_action_is_not_offered` covers the unsupported action's absence and remaining toolbar controls.
1. Capture information uses stock DescriptionList with paired Reference/Test URL columns and full-width metadata rows. Retain raw URLs, signed differences, unavailable values, stable IDs and scheme-validated open/copy callbacks. Normal link buttons have a 10px text inset; geometry tests retain 8px outer control gaps separately. Never pass report URLs directly to link or clipboard controls without validation.
1. Percentage displays and copied summaries prefer `rawMisMatchPercentage`, falling back to legacy strings only when absent. Use the shared Rust formatter so positive values never display as zero; formatting must not mutate audit data. Show optional exact `mismatchPixels` in the header, information panel, and copied summary, preserving unavailable legacy counts. Check `mismatch` model tests and `scenario_information_hover` GUI tests.
1. Capture-summary copy uses value-aware Clipboard feedback with the complete `copy_summary()` snapshot, preserving its tooltip and weak status notification. Results URL buttons and context-menu copies remain gated by validated availability; do not replace those guards with an always-enabled Clipboard control.
1. Keep message-only capture/approval errors in one named Role::Alert wrapper with presentational Marker content; Kit 0.7.3 Alert plus a neutral wrapper did not retain the native accessible name. Key empty-preview wrapper IDs by pane index: side-by-side absence renders more than one pane and repeated IDs can panic in native accessibility.
1. Diff dimension offsets are signed test-minus-reference pixels. Preserve raw referenceUrl versus replaced test url; do not share a display URL fallback.
1. Inventory successful baselines and actual captures, not directories or generated diffs. Use completed full-suite registry records for defaults, retain completed partial runs for explicit selection, and preserve legacy time fallback. Generate missing comparisons only for explicit viewing, not hidden refresh or recovery. See [run retention](../run-retention/SKILL.md).
1. Results exposes Approve This Scenario for the current viewport, never whole-run approval. Pin the output root, suite, run, scenario, and capture key. Confirm inline in the comparison and retain draft/preflight guards without switching to Runs. Dismiss pending approval on capture changes or refresh; keep whole-run approval in Runs.
1. Refresh the currently viewed report in place after scoped approval, preserving capture/filter/view state and its output root. Recompute its complete original scope, including unapproved failures, without recapturing or changing thresholds; show approval errors in Results.
1. Successful Compare completion replaces older run/saved-report pins with the completed request's output root and run ID, scoped to suites recorded in comparison progress. Unrelated suites sharing a run name must not appear. Do not use mutable Run settings or switch tabs automatically. Failed/cancelled commands retain the reviewed run; scoped approval keeps its in-place refresh path.
1. Successful Test/Reference completion clears old run/saved-report pins and refreshes the latest default in the completed request's output root. Full-suite runs become the default; partial runs leave the latest completed full-suite report selected. Follow the hidden-refresh rule above: only a visible Results tab generates missing comparisons. Failed/cancelled captures retain the reviewed report. Cover hidden, visible, full, partial, failed and cancelled cases in the completion GUI tests.
1. Idle Results suite badges use the loaded report's scenario/viewport totals, not stale Activity history or raw screenshot counts. Keep live execution counts while a worker is running.
1. Keep expensive image/file loading off the UI thread and reject stale completion after selection changes. Own popup menus/subscriptions without retained cycles.
1. Image context menus use ContextMenuExt on generation-scoped scroll surfaces, freezing validated paths/URLs when built. Invalidate on capture/mode/refresh/Results exit, cancel pan on right-click, and retain the guarded weak dismissal helper. Pane activation requires both mouse down/up to be Left so mixed Left-down/Right-up cannot open a pane; preserve keyboard/touch activation and pathless-error exclusions.
1. Capture Log independently selects original reference/test histories through optional report links. Share the bounded reader/filter/export model, keep log-health warnings separate from image outcomes, and disable incomplete exports. See [logging docs](../../../docs/commands.md#diagnostic-verbosity).
1. Capture rows use Kit List/ListItem with application-owned preview navigation and the toolkit scroll handle. Keep selection single-item through `select_capture`, clear pending approval on refresh/change, and leave search keys scoped to its input. Restore only unique keys in the same report; duplicate-key row IDs are catalog-scoped and old click callbacks reject a changed catalog. `flat_collection_capture_` covers reordered and duplicate keys.
1. Fit-mode zoom readouts use current laid-out image bounds, not the stored manual zoom. Show one decimal place and cover both fit modes, image tabs, capture changes, and preview resizing with the `image_previews_` GUI tests.
1. Side-by-side FitWidth uses the approved public canvas measurement adapter to align pane widths to device pixels before aspect-ratio layout; fractional widths can amplify height error in tall images. Preserve one-pixel GUI tolerances, no-upscaling, fit-area/manual policies and native pan/zoom/scroll checks.
1. Side-by-side lanes share maximum image width/height, scale, and top-left origin; never stretch or independently center smaller images. Underfilled canvases drag together within unused lane width/height, stopping at the widest/tallest image's lane boundary without changing fit/zoom or adding scroll overflow. Track each axis independently for diagonal drags and oversized-axis scrolling. Keep normalized placement bounded after resize, reset to horizontal centering/top alignment with fit/capture resets, retain pointer anchors, and use weak ownership for pan notifications. `image_previews_` and native Comparison cover all edge limits.
1. Keep six independent image views: Reference/Test/Diff/Side by side/Wipe/Overlay. Wipe and opacity reuse loaded textures and common source dimensions, exclude error placeholders, and fall back to Side by side if a refreshed source disappears. Retain desktop/web zoom presets through 800%, modifier-wheel anchoring, scoped image keys and double-click. Blend controls use stock Slider with capture-phase focus and a guarded keyboard adapter; slider thumbs consume mouse down. `image_previews_` covers all modes and controls; native Comparison checks exact source pixels at both blend endpoints in both themes/sizes.
1. Crosshairs track hover across each complete visible lane, including padding around the shared canvas. Measure image coordinates separately from the stationary, lane-sized overlay; clip both strokes to the lane without adding scroll overflow. Use saved comparison `alignment` to map through virtual diff rows. Reject inconsistent gap heights, placeholders, missing images, and target rows within a missing band; do not clamp coordinates to image or canvas dimensions. Clear geometry/alignment on reset and apply asynchronous metadata only with the matching capture generation. Preserve raw-pixel fallback for legacy reports without alignment. `crosshairs_` covers blank-lane hover and overlay bounds; native Comparison samples full-lane strokes.

## Checks

```sh
cargo test --features desktop --lib desktop::results::tests
cargo test --features desktop --lib snapshot::capture_report::tests
cargo test --features gui-tests --lib comparison_completion_
cargo test --features gui-tests --lib capture_completion_
cargo test --features gui-tests --lib crosshairs_
cargo test --features gui-tests --lib -- image_previews_ side_by_side_scrolls
cargo test --features gui-tests --lib desktop::ui::tests
```

Use native comparison smoke for changed menus/layouts. Cover corrupt/oversized images, escaped paths, missing metadata, renamed suites, signed offsets, selection changes, context-menu dismissal, and approval of only selected successful captures.

## Maintain

Update [desktop docs](../../../docs/desktop.md) and this skill. Explain identity, legacy compatibility, and resource-ownership decisions in code comments.
