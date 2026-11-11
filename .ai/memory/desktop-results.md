# Results Review

Verified: 2026-10-21

- [Overview](../../src/desktop/ui/overview.rs) uses uniform DataTable rows with
  full summaries/errors retained in explicit selectable Details. `overview_table_`
  covers checkbox/Edit isolation, exact full-text copy, focus return, refresh and
  release, plus 80 mixed valid/error rows in both themes/sizes. Exact header/body
  bounds require fixed-width header content because Kit reserves sort-icon padding.
- [Capture rows](../../src/desktop/ui/reports.rs) use Kit List/ListItem while
  `select_capture` retains preview/approval policy. `flat_collection_capture_`
  reproduces old index-based refresh drift and duplicate-key click retargeting;
  both pass with unique-key refresh and catalog-scoped ambiguous row identity.
  The 152-row native-event regression preserves wheel/filter/keyboard behavior.
- Preview fixtures sharing one PNG need distinct `fileName` capture keys when
  asserting refresh preservation. The independent-mode zoom test passes with
  explicit keys; duplicate-key refresh deliberately falls back and has separate
  coverage in `flat_collection_capture_duplicate_keys_remain_distinct`.
- [Managed image menus](../../src/desktop/ui/reports.rs) use ContextMenuExt with
  generation-scoped scroll triggers and captured validated path/URL targets.
  Mode/capture/refresh/Results exit remove old menus. The shared weak
  [dismissal helper](../../src/desktop/ui/design.rs) restores focus only while
  the disappearing popup still owns it. Mixed-button mouse clicks cannot open
  a pane; the held-pan regression verifies cancellation and unchanged offsets.
  Image action/lifecycle tests and native Comparison pass without toolkit edits.
- [Capture information](../../src/desktop/ui/reports.rs) uses an unbordered
  DescriptionList with paired URLs, full-width metadata, exact optional `mismatchPixels`,
  normal controls and stable URL children. Header/Info/copy prefer raw percentages
  with adaptive precision; absent counts stay unavailable. GUI tests verify 8px
  outer gaps, stock 10px text insets, raw URLs, exact copy values, signed dimensions,
  unavailable fields, and reject javascript/data schemes before open or copy.
- Capture/approval error bands retain their named Role::Alert wrapper around
  presentational Kit Marker content. Styled Alert plus a neutral wrapper lost
  the error name in native UIA; the full `-ComparisonFlows` smoke passes with the
  named wrapper, exact URL copies, and error images at both sizes/themes.
- Empty preview IDs include the pane index. A shared `capture-preview-empty`
  ID caused a native duplicate-a11y-node panic on an empty side-by-side suite.
  `results_suite_navigation_is_isolated_and_preserves_drafts_during_refresh`
  covers distinct pane IDs; native comparison smoke now completes that transition.
- Native fixtures calling `ResultsPanel::show_run` directly must first supply
  discovered suites through `set_suites`. A report can load into the catalog but
  remain unselectable without its source-suite match. The `results_capture_log_view`
  native regression verifies independent reference/test archive selection.
- Suite `displayName` is independent of file/output identity. The
  [workspace model](../../src/desktop/workspace.rs) stages reference/history/site
  changes across known output roots for rename/delete and label saves. Explicit
  IDs remain stable; unknown custom roots require the caller to supply them.
  [Artifact tests](../../src/desktop/workspace/artifacts.rs) cover rollback and
  concurrent changes; this is not a crash-atomic or cross-process transaction.
- [Suite menu tests](../../src/desktop/ui/tests.rs) use `suite_panel_` to cover
  right-click Rename/Delete, F2/Delete focus isolation, Fields labels, artifact
  migration, and cancelled unsaved work. Global F2 bindings defeat input isolation.
- Post-mutation activity persistence needs its own UI polling task because the
  worker poller has stopped. `suite_panel_activity_remap_finishes_persistence_and_unlocks_catalog`
  verifies completion; native `-ArtifactFlows` additionally checks label accessibility,
  F2 rename, preserved image hashes, and cancelled/confirmed deletion.
- [Scenario naming](../../src/helpers/scenario_names.rs) is shared by desktop
  catalogs/drafts/results and Rust snapshot conversion. URL-only scenarios use
  shortest unique readable suffixes, optional mixed origins, then query/fragment
  details or ordinals. Multiple captures of one scenario share a name.
- [Capture manifests](../../src/test/test_suites.rs) record `scenarioDisplayName`
  against the whole suite before filtering/replacements. It is presentation-only:
  source labels, IDs, filenames, and routes stay independent. Legacy raw-URL labels
  are resolved during report loading. Evidence: `cargo test --features desktop
  --lib names` covers cross-client parity, stable IDs, and captured-name preference.
- [Results navigation](../../src/desktop/ui/navigation.rs) uses loaded report
  scenario/viewport counts while idle; saved Activity counts can predate approval.
  Live run counts still take precedence during execution.
- [Run completion](../../src/desktop/ui/mod.rs) replaces stale Results pins without
  changing tabs. Compare uses the request's output root, run and compared suite IDs;
  successful Test/Reference calls `follow_latest` (latest full-suite default), and
  only a visible Results tab generates missing comparisons. `comparison_completion_`
  and `capture_completion_` in [Results tests](../../src/desktop/ui/reports.rs) cover
  pins, custom outputs, shared run names, outcomes, and hidden/visible refreshes.
- [Results approval](../../src/desktop/ui/reports.rs) confirms inline, clears pending
  confirmation on capture changes/refresh, and refreshes its existing output root
  without resetting the current capture or view. It still uses the
  [run guard](../../src/desktop/ui/mod.rs) for drafts and suite preflight.
- [Pixel tolerance](../../src/config/mod.rs) cascades as `pixelMatchThreshold`,
  0-100 with default 5 and desktop steps of 0.1. [Comparison](../../src/compare/report_gen.rs)
  reads `.report-pixel-thresholds.json` and stores the value on report pairs.
  Approval retains reviewed values; missing fields on legacy reports mean 10
  (their original normalized 0.1), while a fresh comparison without metadata uses 5.
  `cargo test --features gui-tests --lib pixel_match_threshold`
  covers the cascade, reports, validation, and native editing/reset.
- Regression evidence: `suite_panel_restores_all_saved_statuses_and_keeps_them_during_other_runs`
  and `approval_completion_preserves_comparison_and_updates_suite_counts`, run with
  `cargo test --features gui-tests --lib <filter> -- --test-threads=1`.
- [Zoom readouts](../../src/desktop/ui/reports.rs) defer layout until preview bounds
  are current; fit percentages use rendered pixel bounds, including layout rounding,
  while manual percentages use the selected preset. The `image_previews_` GUI tests
  cover one-decimal labels, independent modes, pointer zoom, and resized previews.
- Wipe/Overlay reuse existing textures, common source bounds, and independent
  scroll/zoom state; placeholder sources cannot enter these modes. Blend Slider
  needs capture-phase focus for native keyboard adjustment. `image_previews_`
  covers all six modes and keys; native Comparison verifies actual blend endpoint
  pixels in both themes at 1000x640 and 1600x1000. Web/native share 25-800% presets.
- Side-by-side FitWidth pixel-aligns pane widths through public canvas layout;
  fractional widths previously caused tall-image height drift. GUI one-pixel
  checks and native minimum/compact/large Comparison pass without changing zoom,
  scrolling or no-upscaling policy.
- [Comparer](../../src/compare/image_compare.rs) exports single-band and multi-band
  vertical-gap alignment (`reference`/`test` and `referenceGaps`/`testGaps`) through
  saved reports and snapshots. Partial anchors use local informative full/half-row
  signatures, including vertical variation, without relaxing clean-row penalties.
  Native/web crosshairs map source pixels through
  aligned diff rows; missing band rows have no target pixel. Native
  [pointer geometry](../../src/desktop/ui/reports.rs) uses maximum-width/height
  canvas coordinates, including blank space beyond smaller images, without
  clamping guides to individual image sizes. `crosshairs_` includes a multi-band
  generated-PNG GPUI pointer test; web `aligned diff bands` covers multi-band
  directions and zoom. The ignored `full_page_comparison_quality_and_timing`
  release test checks exact alignments and reports synthetic 1440x10000 timings.
- Native guides measure image coordinates separately from a stationary lane-sized
  overlay. `crosshairs_` covers hover outside the shared canvas, viewport bounds
  after scrolling, and unchanged overflow; native Comparison probes full-lane strokes.
- Underfilled side-by-side canvases drag together within unused lane width/height.
  Normalized placement preserves bounds after resize; weak pan notifications
  avoid retaining Results through pointer layers. `image_previews_` covers fit,
  manual zoom, all edges, diagonal pan, orthogonal scroll, fit reset, and zoom anchors.
