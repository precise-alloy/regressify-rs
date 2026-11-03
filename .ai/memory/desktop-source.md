# Desktop Source Editing

Verified: 2026-10-21

- [Source font size](../../src/desktop/ui/source.rs) uses a separate unmasked integer 10..24 NumberInput named `Source font size` through Kit's `accessibility_label`: invalid text never saves; Blur restores the last valid size.
  `source_font_size_` covers input/saved bytes, steps, Reset, reload and unchanged drafts. Activate GPUI fixtures for Blur; inactive windows suppress focus events.
- [Command search](../../src/desktop/ui/commands.rs): actionless items, one-shot close/focus/dispatch, stock title/category substring matching and two-step Escape.
  `command_palette_` covers duplicate confirmation, focus/drafts and disabled/empty results; refocus query input after a row click before further typing.
- [Fields groups](../../src/desktop/ui/mod.rs) use controlled Accordion with
  stable IDs and separate collapse preference. Search/errors/unsupported choices
  force open; closed inputs unmount and return focus to the disclosure.
- Kit 0.8.0 AccordionTrigger remains pointer-only; the approved label-focus adapter
  scopes Enter/Space away from inputs/popups. Ignore no-op content clicks.
  `controlled_group_` verifies these contracts. Workspace Button keyboard tests
  use Tab and the observed `workspace-files` wrapper, not pointer-focus assumptions.
- [Application menus](../../src/desktop/ui/files.rs) route File actions with a
  distinct AppFileCommand, preserving the open document despite suite-row focus.
  The shared registry is copied only on changed flags. Public scoped Cancel
  restores Source focus before reload; an empty reload breaks Kit's menu/bar
  cycle on release. `app_menu_` covers exact targets, focus and two windows.
- [Value pickers](../../src/desktop/ui/choices.rs) use retained Select and multiple
  Combobox state, with `SearchableVec` for actual filtering. Plain Vec enabled
  an input but did not filter. `value_selector_` reproduces unchanged-refresh
  query loss and stale selection after Reset; both are fixed.
- Windows option names need `Role::Label` plus `aria_value(title)` on the
  shared item renderer. Label-only/string rows were visible but unnamed in UIA;
  the public adapter passed the unchanged native pointer/name assertions.
- `choice_combobox_` covers one undo per toggle, Escape without rollback,
  unknown-only/scalar drafts, case matching, invalid raw input and stale binding
  rejection. Viewport controls wait for discovery before marking names unavailable.
  Toolkit value setters clear search; multi-choice sync resolves the full list
  first, then restores the query. Raw textareas remain authoritative.
- [Source footer](../../src/desktop/ui/source.rs) uses Kit StatusBar slots for
  language, caret, completion and issue navigation. Keep child status IDs,
  roles/names and action guards unchanged; `source_line_commands` (12) checks
  caret position and language after the layout migration.
- Source diagnostic severity dots use Kit Badge inside the existing fixed-size
  test host; the parent severity cell retains its Warning/Error status name.
  `schema_issue_panel_scrolls_and_keeps_text_inside_compact_and_large_windows`
  (1) verifies centering, text bounds, scroll and both themes/sizes.
- Numeric Fields use styled Kit NumberInput with the field's `accessibility_label`, `MaskPattern::None`
  and `set_step(None)`; 0.8.0 defaults to fixed step one. Step events call the
  existing schema-aware `adjust_field`, while Change retains raw invalid drafts.
  `numeric_stepper_` (2) and `pixel_match_threshold_` (12) pass. Native tests
  observe `("input", state.entity_id())` inside the stable field wrapper.
- Run settings and workspace Fields use Kit Form/Field for presentation only.
  The Fields editor still owns bindings, raw drafts, `field_errors`, inheritance
  hints, reset, and semantic edits; `setting_search_`, numeric/threshold filters,
  and the invalid-draft/reset native regression pass after the migration.
- [The toolkit pin](../../Cargo.toml) moved 0.7.3 -> 0.8.0 with matching base/component/assets;
  Windows still uses `gpui-pre` 0.3.9. `gpui-fast` 0.1.10 and new renderer packages are inactive lockfile entries.
  [Windows/fixtures](../../src/desktop/ui/mod.rs) retain `open_window`; adopted spinner labels
  have red/green scoped SpinButton regressions and native UIA verification.
- Kit 0.8.0 adds custom completion highlighters, rich-text editor hosts and Editor sizing;
  retain default prefix matching, lossless Source ownership and independent font size.
  Select now remaps replaced items by value, but its setters still clear search; keep the sync guards.
  [Overview](../../src/desktop/ui/overview.rs) keeps fixed widths: unsortable header insets remain.
  [Git review](../../src/desktop/ui/git/view.rs) retains full-file aligned copies, not patch-only `diff::Diff`.
- The schema jump/hover/save-guard test failed on offscreen diagnostic 3 until
  scrolled into view before clicking; preserve cursor, hover, draft and save checks.
  Kit 0.7.3 tracks `diagnostic_popover` separately from `hover_popover`: clear both
  (`clear_source_popovers`) on diagnostic/source updates, tab switches, and Escape,
  and skip no-op background viewport diagnostic resets so active hovers do not flicker.
- [Action viewport choices](../../src/desktop/ui/choices.rs) reuse the native
  list picker and effective consumer labels. Workspace diagnostics call the
  shared CLI validator, point to own/inherited action fields, and retain invalid
  drafts. Background results are keyed to workspace, file, and exact source.
- [Source feedback](../../src/desktop/ui/source.rs) debounces 200 ms with one worker, waits for native/model acknowledgement, and rejects stale generations/scopes/bytes.
  `source_feedback_` checks 20-edit coalescing, single-flight/ABA freshness, immediate Save guards, plain-text inspection and catalog/text/position reuse. Base/contextual checks share parsing/indexing; syntax errors stay Source-owned to preserve the smoke's `1 issue` assertion.
- [Viewport completion](../../src/desktop/completion.rs) uses parsed full value
  spans for spaces/hyphens and forces YAML block-list probes into value mode.
  `source_assistance_` covers native quoted/hyphenated acceptance, dismiss-only Shift+Tab, provider/overlay guards, and workspace-scoped focus restoration; acceptance callbacks are deferred.
- [Source commands](../../src/desktop/ui/source.rs) bind inside `RegressifySource > Input`, check focus/busy/editable state, and apply atomic line edits via `apply_lsp_edits`; the Edit menu uses `action_context` and JSON disables comment toggling. Document owns grouped Source/Fields history; native Source Undo/Redo is captured before Kit, while Fields inputs keep local undo. `source_history_` covers mixed controls, selection, bounds, oversized paste and recovery.
- Kit 0.8.0 LSP-shaped positions still use scalar columns, not UTF-16. `source_ranges_` covers
  Rope round-trips, completion/navigation after emoji, whitespace-safe commands, and quoted viewport preservation.
- Kit 0.8.0 still normalizes `set_selected_range(end..start)` to an empty range; Source restores an ascending range after line edits, covered by the reverse CRLF/Unicode regression.
- Block indentation uses shared line bounds (excluding column-zero selection ends) for indentation, comments, moves, duplication, and deletion; plain Tab and completion acceptance remain native, while Ctrl+Shift+Z and Ctrl+Y share document redo.
- Native input-change notifications are deferred. [GUI tests](../../src/desktop/ui/tests.rs) settle the executor before asserting that Source edits reached the session.
- [YAML field removal](../../src/desktop/document.rs) removes an orphaned preceding
  `INDENT` token when deleting the last mapping entry. yaml-edit 0.3.1 otherwise
  indents the following parent key into the scenario in the starter suite.
  `pixel_match_threshold_reset_preserves_suite_override` reproduced the semantic
  guard failure; document tests and native threshold-reset interactions now pass.
- `cargo test --locked --features gui-tests --lib source_line_commands`
  covers text boundaries, CRLF/Unicode, reverse selections, keyboard undo/redo,
  disabled/read-only/focus guards, strict JSON, and actual menu clicks.
- [Schema smoke](../../ci/desktop-smoke/flows/schema.ps1) includes line shortcuts
  and menu screenshots. Interactive execution requires the isolation guards in
  the [native UI skill](../../.github/skills/desktop-native-ui/SKILL.md).
- [Workspace saves](../../src/desktop/workspace.rs) validate the corrected draft, not the previous saved source; unparseable old metadata only skips optional report-label migration.
  Raw JSON parsing rejects duplicate/escaped keys before Fields; dependency checks run without viewport filters and retain independent viewport errors. `source_validation_` covers pointer-bearing semantics, bounded fallback headers, and literal Markdown diagnostics.
- Create/Save As validates resolved suite destinations, including Windows case
  aliases, and promotes Data templates to Suite validation before writing.
  `save_as_` covers name/content rejection, no-clobber writes, and kept source bytes;
  ordinary dot-prefixed helpers remain allowed.
- Editor diagnostics describe the draft; suite catalog errors describe the saved
  file and refresh after Save succeeds. The `corrected_invalid_source` filter in
  `cargo test --locked --features gui-tests --lib corrected_invalid_source`
  covers YAML/JSON repairs, conflicts, and the native Save interaction.
- Non-YAML/JSON UTF-8 files use `Format::Text`, not YAML parsing. Workspace
  discovery checks content within the 4 MiB limit, and open rechecks the bytes;
  binary controls, invalid UTF-8, and oversized files remain non-editable. Oversized
  Source drafts stay dirty/undoable; `unsaved(cx)` also guards pending native edits.
- Plain text uses Source only, including breadcrumb and Save As transitions.
  The toolkit's `text` highlighter disables configuration highlighting; schema
  completion and hover providers are cleared. The `common_text_files_` model and
  `workspace_files_text_documents_support_source_editing_and_save` native tests
  cover saves, conflicts, binary guards, and keyboard undo/redo.
- [External reload](../../src/desktop/ui/files.rs) reads the open file off-thread
  once per second and validates workspace/path/saved-byte identity on completion.
  Clean content reloads; pending/invalid drafts and read failures retain the editor.
  `external_reload` model/native tests cover metadata-preserving edits, atomic
  replacement, raw text, recovery, stale results, and real conflict button clicks.
- Hidden Fields controls retain old values after Source edits/saves. Only inspect
  their raw text when Fields is active; `external_reload_after_source_save` failed
  without this guard. Native `set_value` is not typing: use keyboard input to
  exercise change notifications and session dirty/error state.
