---
name: update-gpui
description: 'Use when upgrading gpui-kit (and its resolved gpui-component, gpui-base, gpui-kit-assets, and gpui-pre crates) to a new release, diffing public toolkit APIs between versions, retiring desktop UI workarounds or custom widgets in favor of stock kit components, and landing the upgrade with tests, docs, and memory. Do not use for routine widget or layout edits without a toolkit version bump (use desktop-native-ui) or unrelated dependency updates.'
argument-hint: 'Optionally name a target version; otherwise the latest stable release is used'
---

# Upgrade `gpui-kit`

`gpui-kit` is Regressify's single native UI facade for [`src/desktop/ui`](../../../src/desktop/ui). [`Cargo.toml`](../../../Cargo.toml) depends on `gpui-kit` alone (never standalone `gpui`, `gpui-component`, `gpui-base`, or `gpui-pre-*` crates), resolving matching `gpui_kit::*`, `gpui_kit::component`, `gpui_kit::base`, `gpui_kit::assets`, and `gpui_kit::test` layers together.

**Always upgrade to the latest published stable release** (never a `-preview`, `-alpha`, `-beta`, `-rc`, or yanked version). **Breaking changes and workaround deletions are welcome** - when a new release fixes an upstream defect or exposes a stock component that replaces a local adapter, adopt it and update every caller, test, doc, skill, and memory note in the same change. Do not keep the old path "just in case".

## Steps

### 1. Find the latest version and what is currently pinned

```sh
grep -n 'gpui-kit' Cargo.toml
grep -A3 -E 'name = "gpui-(kit|component|base|pre)"' Cargo.lock
```

Fetch `https://crates.io/api/v1/crates/gpui-kit` and select the newest non-yanked version with no prerelease suffix. [`Cargo.toml`](../../../Cargo.toml) pins an exact version (`gpui-kit = { version = "=0.8.0", optional = true, features = ["tree-sitter-yaml"] }`), so you must update the `=X.Y.Z` pin in `Cargo.toml` before updating the lockfile. Preserve `optional = true`, `features = ["tree-sitter-yaml"]`, `gui = ["desktop", "dep:gpui-kit", ...]`, and `gui-tests = ["gui", "gpui-kit/test-support"]`. Never add local path dependencies or vendored toolkit sources.

### 2. Update and check compilation

```sh
cargo update -p gpui-kit
cargo check --locked --features gui --bin regressify-desktop
cargo check --locked --features gui-tests --lib
```

Default `cargo build` / `cargo test` compiles only the CLI (`local`) slice and never touches `gpui-kit`. A clean `gui` / `gui-tests` check proves source compatibility only - the value of the upgrade is step 3.

### 3. Diff the public API across the kit crates

Cargo vendors both versions under `~/.cargo/registry/src/*/` across `gpui-kit`, `gpui-component`, `gpui-base`, `gpui-kit-assets`, and `gpui-pre`. Diff them directly rather than relying on release notes:

```sh
SRC=$(ls -d ~/.cargo/registry/src/*/ | head -1)
for c in gpui-kit gpui-component gpui-base gpui-kit-assets; do
  echo "=== $c ==="
  diff -rq "$SRC/$c-<old>/src" "$SRC/$c-<new>/src"
done
```

For each changed module, diff `pub (async )?(fn|struct|enum|trait|const|type)` signatures and read doc comments and implementation changes on added or modified items. Use `cargo tree --locked --features gui` to distinguish active packages from optional lockfile families such as `gpui-fast`:

| Kit crate / path                     | What lands there                                                                                                                                                                                                                                                                                                |
| ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `gpui-kit/src/lib.rs`, `src/test.rs` | `init`, `open_window`, `actions!`, `#[gpui_kit::test]`, `TestAppContextExt`, `TestWindowExt`.                                                                                                                                                                                                                   |
| `gpui-component/src/`                | Styled widgets (`accordion`, `button`, `checkbox`, `clipboard`, `combobox`, `command`, `dialog`, `form`, `input`, `list`, `menu`, `pagination`, `progress`, `scroll`, `select`, `status_bar`, `tab`, `table`, `toolbar`, `tree`), `Theme`, `Size`. **Most adoptable additions and workaround fixes land here.** |
| `gpui-base/src/`                     | Headless primitives (`Root`, `GlobalState`, `Checkbox`, `ScrollbarHandle`, `actions`), focus/popup lifecycle, accessibility roles, and virtual-list measurement.                                                                                                                                                |
| `gpui-kit-assets/src/`               | `IconName` variants and bundled SVG assets.                                                                                                                                                                                                                                                                     |
| `gpui-pre*/src/`                     | Core `Window`, `App`, `FocusHandle`, `Entity`, layout, clipboard, and platform/UIA backends.                                                                                                                                                                                                                    |

### 4. Decide what to adopt or retire

Evaluate every new or changed capability in this order:

1. **Does it fix an upstream bug behind a Regressify workaround?** Delete the local adapter and use the stock behavior.
1. **Does it replace custom UI plumbing with a stock component?** Prefer toolkit components when they preserve application state and guards.
1. **Does it improve native accessibility, focus, or keyboard/pointer behavior?** Verify Windows UIA roles/names, focus rings, and 4.5:1 contrast.
1. **Does it preserve headless domain authority?** Reject widget state ownership that weakens lossless YAML/JSON edits, invalid-draft retention, or operation locks.

Re-check these Regressify surfaces and active toolkit workarounds on every bump:

| Surface                                                                                                                                                                                                | What to re-check                                                                                                                                                                                                                                                                        |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`src/desktop/ui/design.rs`](../../../src/desktop/ui/design.rs)                                                                                                                                        | `ToolbarFocus` (Left-arrow wrap workaround still needed in 0.8.0), `release_app_menu` (empty-reload cycle break), `restore_dropdown_menu_focus` / `restore_managed_menu_focus`, `CopyText` generation keys, `selection_checkbox` tri-state adapter, `apply_theme` contrast corrections. |
| [`src/desktop/ui/mod.rs`](../../../src/desktop/ui/mod.rs)                                                                                                                                              | `gpui_kit::open_window` Base `Root` overlay mounting, `Accordion` Enter/Space label-focus adapter, `NumberInput` (`MaskPattern::None`, `set_step(None)`), `Form`/`FormField` presentation.                                                                                              |
| [`src/desktop/ui/source.rs`](../../../src/desktop/ui/source.rs)                                                                                                                                        | `CodeEditor`/`EditorState`, `show_completions`, `apply_lsp_edits`, `set_selected_range` reverse-range normalization, font-size `NumberInput` (10..24), `StatusBar`, `text`/`yaml`/`json` highlighters.                                                                                  |
| [`src/desktop/ui/choices.rs`](../../../src/desktop/ui/choices.rs)                                                                                                                                      | `SelectState`/`ComboboxState` with `SearchableVec`, `ValueChoice` (`Role::Label` + `aria_value` for Windows UIA), search restoration after `set_selected_value`.                                                                                                                        |
| [`src/desktop/ui/files.rs`](../../../src/desktop/ui/files.rs)                                                                                                                                          | `AppMenuBar` reload/cancel, `DialogFooter`/`DialogClose`, suite-name dialog parent focus region (styled buttons own internal focus handles), `ContextMenuExt`.                                                                                                                          |
| [`lists.rs`](../../../src/desktop/ui/lists.rs), [`overview.rs`](../../../src/desktop/ui/overview.rs), [`git.rs`](../../../src/desktop/ui/git.rs), [`runs/log.rs`](../../../src/desktop/ui/runs/log.rs) | Virtualized `List`/`ListItem`, stationary-parent `track_scroll`/`scrollbar` pairing, `DataTable` (`header_row_height` + unsortable header widths), `TreeState`, segmented `TabBar` keyboard adapter, `Pagination`.                                                                      |

### 5. Watch the desktop UI invariants

- **One Base `Root` per window**: `gpui_kit::open_window` wraps content in `base::Root`; builders return application content, never a nested `Root` or manual dialog/sheet/notification layer.
- **Headless domain authority stays in [`src/desktop`](../../../src/desktop)**: never let widget blur, step, or confirm coerce invalid YAML/JSON drafts, strip comments, or replace `apply_lsp_edits` with history-resetting `set_value`.
- **Observation and accessibility wrappers**: `.test_support()` is inert without `gui-tests`. Keep outer observed wrappers (`DialogClose`, scrollbars) and explicit UIA names/roles unless verified in both `gui-tests` and Windows smoke (`Progress`, `TitleBar`, and `CompletionMenu` register `.test_support()` directly). In 0.8.0, name each `NumberInput` through `.accessibility_label(...)`; a named Group does not name its SpinButton.
- **Deferred settlement and window activation**: input notifications, menu dismissals, and `MotionReveal` bounds settle across frames; activate fixture windows before testing `Blur`.
- **Source contracts**: 0.8.0's LSP-shaped positions still use scalar columns, not UTF-16. Document owns accepted Source/Fields history; Source captures native Undo/Redo while ordinary Fields inputs retain local undo. Preserve single-flight feedback and native/model acknowledgement. Re-run `source_ranges_`, `source_history_`, `source_assistance_`, and `source_feedback_` before replacing these adapters.
- **Test input**: use stock modifier clicks, scoped role queries and `TestEventExt` pointer steps. Clicks restore prior modifiers; pointer/scroll helpers preserve held modifiers. Release explicit Ctrl-wheel state with `change_modifiers` before unmodified gestures. `TestInput` uses UTF-16 composition ranges and does not establish native OS IME coverage.

### 6. Verify

Run the [feature gates](../rust-feature-checks/SKILL.md) and [native UI checks](../desktop-native-ui/SKILL.md) with nonzero test counts:

```sh
./ci/test-desktop-smoke.ps1
cargo fmt --all -- --check
cargo clippy --locked --features gui --lib -- -D warnings
cargo test --locked --features gui-tests --all-targets
cargo test --locked --all-targets
cargo build --locked --features gui --bin regressify-desktop
```

Headless `gui-tests` do not replace Windows UIA or compositor verification. On an unlocked Windows desktop with no other app instances running, run the affected [`./ci/desktop-smoke.ps1`](../../../ci/desktop-smoke.ps1) flows (default, `-ArtifactFlows`, `-SchemaFlows`, `-ComparisonFlows`, `-GitFlows`, `-PublishingFlows`, `-DeploymentFlows`).

### 7. Land it completely

- **Tests**: add or update `#[gpui_kit::test]` regressions in [`src/desktop/ui/tests.rs`](../../../src/desktop/ui/tests.rs) or module test blocks covering any changed component contract.
- **Docs & Skills**: update [`docs/desktop.md`](../../../docs/desktop.md), [`desktop-native-ui`](../desktop-native-ui/SKILL.md), and this skill when pinned versions, workarounds, or component contracts change.
- **Memory**: record the version transition, adopted/retired adapters, rejected candidates, and new toolkit traps in [`.ai/memory/desktop-source.md`](../../../.ai/memory/desktop-source.md) or [`.ai/memory/desktop-smoke.md`](../../../.ai/memory/desktop-smoke.md), then run the [guidance validator](../maintain-agent-guidance/SKILL.md).

## Known traps

- [`Cargo.toml`](../../../Cargo.toml) uses an exact `=X.Y.Z` pin; `cargo update -p gpui-kit` is a no-op until the manifest pin changes.
- Default Cargo commands omit `gui` and `gui-tests`; a green `cargo test` never compiles `gpui-kit`.
- `gpui-kit` is a thin facade; diffing only `gpui-kit-<ver>` misses widget and platform changes in `gpui-component`, `gpui-base`, `gpui-kit-assets`, and `gpui-pre-*`.
- With `gui-tests`, `use gpui_kit::*;` exports `#[gpui_kit::test]`; import Kit types explicitly in modules that also use standard `#[test]`.
- Styled wrappers (`Scrollbar`, `Button`, `DialogClose`, `Progress`) can replace element IDs, focus handles, or UIA roles; always re-verify focus restoration and Windows UIA names after a bump. In 0.7.3+, `TitleBar` (`window-controls`) also registers `.test_support()` on its `"close"` button, so dialog header close-button assertions must scope with `.within("dialog-close")` (or `try_find("dialog-close")`).
