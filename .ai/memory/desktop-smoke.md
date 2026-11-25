# Desktop Smoke Harness

Verified: 2026-10-21

- [Kit 0.8.0 tests](../../src/desktop/ui/tests.rs) adopt stock modifier clicks, role queries and
  `TestEventExt` pointer steps, retiring both local modifier-click helpers. Clicks restore prior
  modifiers; scroll/drag preserve held state. The zoom/pan regression failed until Ctrl-wheel
  explicitly released Control through `change_modifiers`; production zoom behavior was unchanged.
- Kit Progress retains percentage values/names and direct observation, as do TitleBar and
  CompletionMenu. Scope dialog X with `.within("dialog-close")`, excluding titlebar Close.
- All seven upgrade native flows passed in light/dark compact/large windows with restored preferences.
  The default flow now verifies the actual `Source font size` Spinner name and opens Checkout
  before compact Actions checks: checked sidebar state is not the reconciled editor selection.
- The [entry point](../../ci/desktop-smoke.ps1) owns parameters, Windows safety
  guards, build, flow precedence, and the run/cleanup boundary. It dot-sources
  [support and flow scripts](../../ci/desktop-smoke) into the same script scope.
- [Fixture setup](../../ci/desktop-smoke/fixture.ps1) initializes shared process,
  workspace, and preference state. [Cleanup](../../ci/desktop-smoke/cleanup.ps1)
  runs in the entry point's `finally`, including early-return and failure paths.
- [The default flow](../../ci/desktop-smoke/flows/default.ps1) explicitly assigns
  `$script:cleanupPath` and `$script:cleanupSnapshot` so its local function scope
  does not hide the dirty-draft verification state from cleanup.
- [Headless checks](../../ci/test-desktop-smoke.ps1) pass with PowerShell 7 and
  cover loading, all switch dispatch paths, precedence, failure cleanup, and
  shared draft state. Parser-created dispatch blocks retain the entry filename
  because `[scriptblock]::Create()` alone leaves `$PSScriptRoot` empty.
- A filename on a parser-created block does not create a new script scope. Full
  native baseline comparisons must run from a real script file beside the entry
  point so local initialization and `$script:` assignments share ownership.
- [Keyboard input](../../ci/desktop-smoke/native.ps1) builds balanced `SendInput`
  events from `ConsoleKey` plus Control/Shift/Alt, including extended navigation flags.
  Zero and undefined keys are rejected before input is sent. Headless checks
  compile this code and inspect its event sequences without invoking Windows.
- Inline `Add-Type` definitions are cached for the PowerShell process. Native
  tests of changed C# need a fresh host; rerunning in an old terminal can otherwise
  retain the previous implementation.
- [About's footer](../../src/desktop/ui/overview.rs) uses public DialogClose's
  trigger inside the `about-close` observation wrapper. The native menu smoke
  clicks Close within the `About actions` group so the titlebar X cannot satisfy
  the check. GUI tests cover actual pointer dismissal, Tab/Enter/Space/Escape,
  focus return, unchanged draft/saved bytes, and window-close owner release.
  Native Update, Artifact, Publishing, Deployment and Git flows pass with this
  footer check and the shared DialogFooter layout across all nine dialog sites.
- Kit 0.10 (verified 2026-11-20) fixed Toolbar backward wrap but moves focus on Ctrl/Alt arrows, so
  [ToolbarFocus](../../src/desktop/ui/design.rs) stays, with menu cycle/focus helpers, CopyText keys,
  the base mixed checkbox (styled `indeterminate` not adopted) and contrast fixes. Stock Tabs own log-tab
  keys; pointer clicks do not focus them, so [default](../../ci/desktop-smoke/flows/default.ps1) uses UIA focus.
  Native default verifies Add-to-More Left and exact log-search caret behavior; `toolbar_` covers release.
- Run interactive flows without concurrent user mouse/keyboard input. Pointer
  movement dismissed a metadata hover card and redirected image-tab clicks;
  the undisturbed fresh-STA `-ComparisonFlows` run passed all four size/theme
  combinations without a popup lifecycle change.
- [Clipboard checks](../../ci/desktop-smoke/controls.ps1) wait for the native
  sequence number to change after a synthetic sentinel and copy action, then
  await exact text: GPUI empties the clipboard before supplying text. Headless
  tests reproduce early empty/stale reads and retain wrong/missing-write failures.
  Avoid reads while the sequence is unchanged to reduce contention with the writer.
  Overview Details uses the same helper; its headless and native checks pass.
  Retry only `CLIPBRD_E_CANT_OPEN`. An owned user snapshot is restored in outer
  `finally`; copy mutable data/streams, reject unreadable user formats, and drop
  missing private formats only in fixture-owned scopes. Lifetime regressions pass.
  `-OverwriteClipboard` is an explicit disposable-clipboard option: it skips the
  original snapshot and clears fixture data on exit. The default remains preserving.
- Exact-HWND activation preserves focused child input. Native Cleanup, Default, Comparison, Git, Deployment and Artifact pass.
  Current Source smoke passed twice and full Schema passed, including native Basic Auth Ctrl+Space.
- [Schema smoke](../../ci/desktop-smoke/flows/schema.ps1) verifies Enter/Tab/Escape after Alt+Tab and native return activation without clicking the editor.
  Compare the actual focused accessibility element, not the source wrapper's `HasKeyboardFocus`.
- The focus fixture owns an STA `Application.Run` loop and bounded close/join. Manual pumping let plain Tab arrive with Alt set; no application/OS workaround was needed.
  `-SourceFlows` reuses source assertions/setup; full Schema retains Fields/panel coverage. Headless tests cover both dispatch paths.
- Diagnostic drags settle initial movement/release and gate copies on clipboard sequence changes. The event-order regression failed before the fix; native partial/exact copying now passes.
- Schema smoke also verifies external-save reload, draft preservation, and native
  Cancel/Discard choices in compact/large light/dark windows. The painted unsaved
  dialog title did not expose the expected standalone UIA name; wait for its
  Discard/Cancel controls instead. The native flow passes with preferences and
  clipboard restored; confirmation and final reloaded-source screenshots were inspected.
- Exact line-command selections finish with `Shift+Home` after `Shift+Down`.
  The keyboard-driven [GUI regression](../../src/desktop/ui/tests.rs) reproduced
  a one-character drift into the third line without it. Keep the column-zero
  status/range checks and exact output assertions; do not increase the wait or
  change the expected label. The full native `-SchemaFlows` run passes with this
  boundary normalization, including line edits and restored preferences.
- The [default flow](../../ci/desktop-smoke/flows/default.ps1) includes pointer-focused
  Ctrl+A, Ctrl+Shift+A, and Escape checks for suite/scenario selection and the Runs
  checklist. It verifies accessible selection counts without launching a worker.
- [Pointer clicks](../../ci/desktop-smoke/controls.ps1) flush the compositor after
  movement and press, with release in `finally`. Back-to-back mouse events missed
  native scenario clicks despite passing frame-stepped GPUI tests. The complete
  default native flow passed after synchronization; preferences were restored.
- [List keyboard state](../../src/desktop/ui/lists.rs) is scoped to each list's
  focus handle. [Navigator tests](../../src/desktop/ui/tests.rs) cover filters,
  other-suite preservation, text-input isolation, disabled state, and scrolling.
- [Suite filename dialogs](../../src/desktop/ui/files.rs) focus/select the stem
  after opening. `suite_name_dialog_` GUI tests cover Enter in inputs/buttons,
  keyboard/pointer X and Cancel, failed writes, retry, and busy locks. gpui-kit
  styled buttons replace supplied focus handles: track a parent focus region to
  distinguish the primary action from Cancel/X in the dialog-wide Enter handler.
  Button wrappers may omit disabled metadata; verify ignored interactions instead.
  X dismissal is deferred, so wait for the dialog to disappear before asserting it.
  The native artifact flow passes creation/rename focus, Enter, and X checks. Scope
  the New suite lookup by button role because status text repeats that name after
  cancellation. Set and verify native Create focus before Enter; GUI tests cover Tab.
- Keyboard-scrolled rows share an explicit `track_scroll`/`scrollbar` handle;
  put the overlay on a stationary relative parent, not the scrolling rows.
  Keep direct-child row indices aligned; the convenience overflow wrapper owns
  a separate handle. [Navigator tests](../../src/desktop/ui/tests.rs) and focused
  native wheel/drag checks cover suite/scenario thumbs in both themes and
  minimum/large windows; Landing apps has the same stationary-parent contract.
- Windows interaction prerequisites and native commands remain in the
  [native UI skill](../../.github/skills/desktop-native-ui/SKILL.md).
- [Titlebar smoke](../../ci/desktop-smoke/flows/titlebar.ps1) checks 1000x640 and
  1600x1000 without widening for theme changes, scoping the workspace-path UIA
  lookup to the 38px header band so `GitPanel` body paths never collide below
  1400px. Appearance uses Tab focus and Enter/Escape (`restore_dropdown_menu_focus`
  returns workbench focus after menu confirmation), and Default's Add uses Tab too.
  Header menus are File/Publish/Help. The [header regression](../../src/desktop/ui/tests.rs)
  checks equal group gaps, aligned text, compact tab underlines, version visibility
  at 1080px, and workspace visibility at 1400px across live resize.
- Minimum-size flows reveal Actions via settings search, the Comparison suite via wheel, and landing removal via editor scroll.
  Artifact's added Rename size matrix uses the scoped context menu; original
  F2/escape-focus checks remain. Never bypass native focus/isolation guards.
