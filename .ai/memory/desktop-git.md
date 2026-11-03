# Desktop Git

Verified: 2026-10-15

- [External editor launching](../../src/desktop/vscode.rs) uses the installed
  executable with `--new-window -- <workspace> [file]`, not the URI handler.
  VS Code checks existing folder windows before creating one; passing the root
  with Git files preserves that routing. Windows verbatim drive/UNC paths are
  normalized without lossy text conversion. `desktop::vscode::tests` covers
  arguments, discovery and failures; `workspace_external_menus_` covers the
  menu contract without launching another application or changing drafts.
- [Change trees](../../src/desktop/ui/git/view.rs) retain two Kit TreeState
  entities, using group/repository-path IDs. Refresh restores by ID after
  `set_items` clears the cursor; TreeEvent retains collapsed-folder preference.
- Styled Tree 0.7.3 retains its own state through the stored menu-render closure
  even without a configured menu. The mounted two-tree release regression fails
  with that wrapper and passes with the approved public base Tree plus styled
  ListItem and scrollbar adapter. No toolkit or dependency edits are required.
- The `git_tree_` regressions cover operation-time read-only browsing, immutable
  emitted requests, both-group selection locks, refresh identity, stale diff
  completions and large-list body bounds. Checkbox mouse-down must not expand
  its row: the unchanged folder-selection test failed until pointer isolation.
  Scoped Kit Confirm handles folder Enter; the parent intercepts only leaves.
- The [header](../../src/desktop/ui/overview.rs) now reserves supported native
  caption slots and bounds long workspace names. Long-status and native Git
  checks pass at 1000x640 without widening; the old header exception is closed.
- Git's full tooltip uses a plain accessible Label and weak owner, reading live
  status while open. Capturing the summary once froze its elapsed-time suffix;
  exact native comparison and the update-while-open regression caught this.
- [The Git model](../../src/desktop/git.rs) stores active remote selection in
  repository-local `regressify.remote`. Its configuration hash covers that key,
  remotes, and URL rewrites; branch tracking is checked separately. Removing the
  active remote deliberately retains its missing selection to prevent fallback.
  Post-Git workspace reloads use `Session::reload_workspace` so discarded default
  files are not recreated on disk, and discard revalidates index/worktree
  content identities (`Snapshot::content_identities`) before mutating files.
- [The remote Select](../../src/desktop/ui/git/view.rs) retains its search through
  unchanged snapshots and shows a removed selected name as unavailable. It calls
  the existing guarded request path; `remote_select_` and the real typed-search
  workflow pass, as does the full 18-test Git UI module.
- Git activity and both valid source-diff sides use
  [CopyText](../../src/desktop/ui/design.rs) for toolkit Clipboard feedback.
  Diff values concatenate original lines without numbers; changed text gets a
  fresh generation. Focused diff/activity checks and native Git smoke pass.
- Remote form drafts in [the UI](../../src/desktop/ui/git.rs) keep the opening
  snapshot. A newer periodic status must not silently authorize an old URL edit.
  Named remotes support add/select/update/rename/remove; fetch and push URL edits
  preserve additional configured URLs.
- Snapshot URL details contain only validated credential-free URLs; unsafe
  existing values appear as hidden rather than becoming form defaults. Selected
  remote counts use the matching remote-tracking branch, not another upstream.
- [The process wrapper](../../src/desktop/git/process.rs) streams only diagnostic
  commands through bounded reporting; config/status/diff payloads are excluded.
  CR/LF framing happens before redaction. Authorization/oversized lines suppress
  the remaining stream, while pipes continue draining under cancellation/deadlines.
- [Native tests](../../src/desktop/ui/git/tests.rs) use rendered popup items,
  guarded confirmations, local bare remotes, and compact/wide layout assertions.
  `desktop::git::tests::remote_`, `desktop::git::process::tests`, and
  `desktop::ui::git::tests::remote_` passed with their matching feature gates.
- Normal toolkit command bars avoid duplicate padding inside padded remote and
  commit sections. The open comparison panel reserves its 38px header in flex
  basis so both trees receive usable viewport height. Long virtualized fixtures
  wheel target folders into view before clicks. All 16 Git UI tests and native
  Git smoke pass with the [toolbar layout](../../src/desktop/ui/git/view.rs).
- Remote-management native smoke passed on 2026-09-24, using local repositories
  and light/dark compact/wide screenshots. Two 2026-09-27 native Git reruns passed
  the text-diff, exact-copy, malformed-JSON, and selected-comparison-commit checks,
  but stopped at remote-form text entry. This is resolved on 2026-10-06:
  `set_cursor_position` focuses its editor, so activity refresh now positions
  a non-focusing empty range. The new `git_activity_refresh_preserves_editing_focus`
  test failed before the fix and passes for name/URL/commit inputs with Follow
  on/off. Full local-repository Git smoke passes and restores preferences.
  Asynchronous copy checks wait for exact clipboard contents, and text reviews
  assert the current split view rather than the obsolete raw-diff editor.
  Activity belongs below the diff pane: placing it below both panes collapsed
  comparison rows at 1080x720. The layout test now includes those rows and clicks
  a comparison checkbox with activity expanded. Fixture commits made immediately
  before opening a remote form go through the UI to avoid stale-HEAD races.
- Workflow and native-smoke prerequisites live in the
  [Git skill](../../.github/skills/desktop-git-review/SKILL.md) and
  [desktop docs](../../docs/desktop.md), not in this note.
- GPUI Kit 0.6.1 has no built-in diff viewer in its component/base layers.
  [Text review](../../src/desktop/git/review.rs) uses the desktop-only `similar`
  dependency and preserves the existing safe HEAD/saved-file loader without
  YAML/JSON parsing. Native virtual lists share a scroll handle and equal row
  geometry; copy controls use source lines rather than numbered/padded UI text.
- `cargo test --locked --features gui-tests --lib git_text_diff`
  covers malformed source, binary fallback, exact copying, change navigation,
  aligned two-axis scrolling, and compact/wide light/dark layouts. Native wheel
  tests send horizontal and vertical gestures separately because mixed deltas
  use the dominant axis.
- For Windows test-link PDB limits, a temporary `_LINK_` environment option
  `/DEBUG:NONE` passed and leaves project/release profiles unchanged. Restore the
  environment afterward. `LINK=/PDB:NONE` instead created a file named `NONE`;
  it is not a reliable way to disable debug output.
- `cargo test --locked --features gui-tests --lib git:: -- --test-threads=1 --quiet`
  passed all 49 affected Git model/process/native-event checks. The production
  GUI Clippy gate and standalone desktop build also passed.
