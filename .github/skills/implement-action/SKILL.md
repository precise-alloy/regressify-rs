---
name: implement-action
description: 'Use when adding or fixing click, hover, check, uncheck, focus, input, press, select, switchTab, or other browser actions, their types, events, and error propagation.'
---

# Implement an Action

## Start Here

- [Action types](../../../src/types/actions.rs): serde shapes and `TestAction` order.
- [Action execution](../../../src/test/test_action.rs): `exec_test_action`.
- [Tab selection](../../../src/test/tabs.rs): popup tracking and bounded selection.
- [Actionability](../../../src/test/actionability.rs): validated interaction helpers.
- [Browser fixtures](../../../src/test/browser_tests.rs): local pages and cleanup.

## Procedure and Invariants

1. Add or update the type and dispatcher together. Preserve `camelCase`, unknown field rejection, and untagged variant precedence; test ambiguous action shapes.
1. Every action has optional string-or-array `viewportNames`, using the same `Selectors` type as scenarios and a normalized borrowed-slice accessor. Preserve scalar/list serialization; unset/null/empty lists mean all effective scenario jobs, but blank strings are invalid. Gate at `ScenarioPage::exec` before tabs, hooks, diagnostics indices, frames, or dispatch. Reuse `action_applies_to_viewport` for execution, tab installation, and capture planning; never filter inside individual handlers. Legacy conversions remain unset.
1. `switchTab` and `closeTab` are dispatched by the scenario page owner before page-scoped actions. Keep waits, frames, saves, persistence, final captures, and injected ready scripts on the selected page. `closeTab: current` closes only popup tabs, honors armed `beforeunload` dialog expectations, and returns to the previously active tab. Update the desktop action template/menu and schema tests too.
1. Reuse pre-action helpers for selector-targeted actions: wait for existence, visibility, stable geometry across at least two observations, centered scrolling, unobscured targeting, and enabled controls. Preserve configured deadlines and poll bounds. Custom positions (`click`, `hover`, `drag`, `dropFiles`, `wheel`, `tap`, `swipe`, `longPress`, `pinch`) must validate/hit-test the actual border-box point, not the center.
1. Use CDP pointer movement/press/release for click and hover, and CDP key events with key/code/modifiers/virtual codes for press. JS event dispatch alone does not reproduce browser interaction or CSS hover.
1. [Gestures](../../../src/test/gestures.rs) own complete pointer/touch/modifier lifetimes, including cancellation cleanup. Preserve typed CDP required empty arrays for touch end/cancel and file drops. `pinch` dispatches two tracked touch points (`id: 0` and `id: 1`) with `scale` in `0.1..=10.0` (excluding `1.0`). Native HTML5 drags retain intercepted data; file drops reuse confined regular-file validation. Same-document endpoints must be visible together.
1. `key` preserves current focus; `press` focuses its selector; `blur` removes focus from a focused selector or `"active"` and fails if unfocused. Keep `press` before `key` in action catalogs so schema/completion and MCP distinguish the discriminator from the existing argument. Typing delays are fixed, not randomized. State waits and `selectText` never archive expected text/value. `paste` dispatches a cancelable `ClipboardEvent('paste')` with `clipboardData` and uses CDP `Input.insertText` when not `preventDefault()`ed without touching the host OS clipboard.
1. [Dialogs](../../../src/test/dialogs.rs) arm one expectation before the next applicable trigger; missing, extra, mismatched, or orphan expectations fail. Validate consumer/prerequisite viewport sequences before launch. Dismissed beforeunload cancels navigation or tab close intentionally; history operations remain selected-tab scoped.
1. Applicable touch actions (including `pinch` and needs) enable touch before navigation. [Tab tracking](../../../src/test/tabs.rs) also owns recursive OOPIF setup for touch-only jobs: configure paused child targets and owned popups before releasing their first scripts.
1. Check checkbox/radio type and current state before check/uncheck. Focus before input, clear unless `append`, and preserve framework input/change notification; native `range`/`color`/`date`/`time` controls validate values and reject `append`/`typingDelay`. `scroll` optionally applies container `position` (`scrollTo`) or `offset` (`scrollBy`). Select must reject missing options and multiple values on a single-select.
1. Propagate failures with selector/action context. A failing action stops its scenario, not peers; do not panic, exit, or discard meaningful errors.
1. Put field rules the JSON Schema cannot express (exclusive fields, key names, state names, tab-only fields) in `validate_action_fields`, which suite preflight runs before browser launch; file existence belongs in `validate_action_files`. Keep the matching execution checks for code-built actions. Wrap waits in `diagnostic_condition`/`diagnostic_condition_in` so failure summaries can name the unmet condition.
1. For `save`, validate the name and optional nonblank CSS `selector` before waiting, then reuse the shared screenshot preparation in [page stability](../../../src/test/page_stability.rs). Save selectors are independent of scenario final selectors and retain the named checkpoint filename. Keep delay, animation freezing, and settling aligned with final captures; see the [capture skill](../capture-stability/SKILL.md) for terminal-save suppression, explicit final targets, and pixel regressions.

Utilities (save, persist, hide/remove) and waits/navigation do not require the interactive actionability sequence. Scroll still validates its target. File input paths remain confined to `visual_tests`; preserve chooser and direct-input paths. Frame actions need the same validation within each frame and correct CDP coordinates.

## Checks

```sh
cargo test --lib types::actions::tests
cargo test --lib test::test_action::tests
cargo test --lib test::actionability::tests
cargo test --lib expanded_actions_ -- --include-ignored
```

With managed Chromium, run `cargo test --lib test::tabs::tests -- --include-ignored` and `cargo test --lib test::test_scenario::tests::popup_actions_and_captures_follow_the_selected_tab -- --ignored --exact` for `_blank`/`noopener`, JavaScript and iframe popups, selection errors, and capture pixels.

For action filters, explicitly run the ignored `skipped_viewport_actions_do_not_touch_documents_or_dispatch` and `tab_tracking_uses_only_applicable_own_and_inherited_actions` scenario tests. They use cached Chromium, both viewport labels, all action kinds, and closed documents. Debug skip messages are console/live-only, exclude action values, and must not increment executed-action indices.

Run `cargo test --lib test::browser_tests::responsive_hamburger_menu_mobile_only_click_and_save -- --ignored --exact` for the responsive-menu contract. It verifies mobile-only DOM state, ordered shared interactions, capture pixels, filtered inventory, and absence of desktop menu dispatch in the job journal.

Add a local-browser regression for changed event behavior: missing, delayed, hidden, moving, disabled, covered, nested-frame, quoted-selector, and success cases as relevant. Browser tests are ignored by default; inspect their `#[ignore]` prerequisites before explicitly running the selected test with `--ignored`.

## Maintain

Update [action docs](../../../docs/create-action.md), applicable [examples](../../../visual_tests), and this skill in the same change. Comment non-obvious event ordering, native setters, or platform workarounds, not each step.

Keep the standalone [MCP action guide and typed examples](../mcp-command/SKILL.md) aligned; `cargo test --lib mcp` detects missing action examples and schema drift.
New diagnostic action/condition kinds also require the frontend log allowlist, unit tests, and regenerated embedded assets.
