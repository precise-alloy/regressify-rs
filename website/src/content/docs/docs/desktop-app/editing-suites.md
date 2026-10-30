---
title: Edit suites
description: Create, rename, and edit suites with the Source and Fields editors, add actions, and save safely.
sidebar:
  label: Edit suites
  order: 3
lastUpdated: 2026-10-27
---

:::tip
On macOS, use `Cmd` where this page says `Ctrl`, except `Ctrl+Space`. Click any screenshot to view it full screen.
:::

## Create a suite

1. Click **New suite** or press `Ctrl+N`.
1. Type a **Filename** ①. `.tests.yaml` is added for you.
1. Click **Create** ②.

![The New suite dialog with Filename and Create numbered.](../../../../assets/docs/desktop/new-suite.png)

Names must end in `.tests.yaml`, `.tests.yml`, or `.tests.json` and can't start with a dot. `all`, `_snapshot`,
`_diagnostics`, `_landing`, and `states` are reserved. An invalid name keeps the dialog open.

Set a friendlier **Display name** in Fields > General. It doesn't rename the file, change the suite ID, or break CLI
filters and report links.

## Rename or delete a suite

Right-click a suite and choose **Rename suite** or **Delete suite** (`F2` or `Delete`). Neither works during a run.

- The **Rename suite** dialog sets the **Filename** and **Display name** separately. Changing only the display name
  moves nothing.
- Changing the filename of a suite without an explicit `id` also moves its references, saved runs, and reports.
- Deleting removes its references, test history, and local report entries. You confirm first, and **Undo** can't
  restore it. Deployed sites change only when you redeploy.
- Don't run CLI commands while a rename or delete is in progress.
- If a suite writes output to a custom folder the app has never used, that folder is left alone.

## Source, Fields, and Effective

**Source** is the raw file, **Fields** is a guided form, and **Effective** shows read-only final settings as the CLI
resolves them. Script and CSS files only have Source.

1. Open **Source** ①.
1. Press `Ctrl+Space` or click **Suggest** ② for suggestions (`Ctrl+Space` on macOS too).
1. Use **Find in source** ③ (`Ctrl+F`).
1. Toggle **Wrap** ④ and **Line numbers** ⑤.
1. Click **Save document** ⑥ or press `Ctrl+S`.

![The Checkout suite on the Source tab with Source, Suggest, Find in source, Wrap, Line numbers, and Save document numbered.](../../../../assets/docs/desktop/source-editor.png)

Pick a suggestion ① with the arrow keys, accept with `Enter` or `Tab`, or close with `Escape`. Hover a key for its
description and examples. Click an issue in the status strip to jump to it.

![A suggestion list open in the Source editor.](../../../../assets/docs/desktop/source-suggestions.png)

Keep credentials out of files. For `basicAuth`, use environment variables such as `${BASIC_AUTH_USERNAME}` and set them
before launching Regressify.

## Fields form

1. Select a scenario. **Fields** ① opens.
1. Search with **Find a setting** ② (`Ctrl+Shift+F`).
1. Turn on **Advanced** ③ for more options, or **Overrides only** ④ for values you set.
1. Open **Effective** ⑤ to see the final values.
1. Click **Run Scenario Test** ⑥ or **Run Scenario Reference** ⑦ to run this scenario on all its viewports.

![The Checkout page scenario on the Fields tab with Fields, Find a setting, Advanced, Overrides only, Effective, Run Scenario Test, and Run Scenario Reference numbered.](../../../../assets/docs/desktop/scenario-fields.png)

At least one viewport must stay selected. Number fields accept `Up` and `Down`.

A scenario setting you leave empty comes from the suite, then the workspace, then defaults, and the field says which.
Action and viewport fields don't inherit.

- A value you set shows **Override** ①, even `false`, `0`, or empty.
- **Use inherited value** ② removes it.

![Pixel color threshold showing the Override badge and the Use inherited value button numbered.](../../../../assets/docs/desktop/inherited-value.png)

**Pixel color threshold** (0 to 100, default 5) sets how much a pixel's color must change before it counts as
different. Higher values ignore smaller changes. **Mismatch threshold (%)** sets how many pixels can differ before the
capture fails.

**Image simplification**, also in the **Capture** group, pixelates large images before each screenshot so image-heavy
captures get much smaller. See [Simplify images](/docs/desktop-app/simplify-images/).

## Add actions

1. Click **Add action**.
1. Pick an action from a group ①.
1. Use **Move action earlier**, **Move action later**, **Duplicate action**, or **Remove action** on each step.

![The Add action menu open, with the grouped action list numbered 1.](../../../../assets/docs/desktop/add-action.png)

- The toolbar's **Add** menu adds a **Scenario**, a **Viewport**, or any of the 30 actions.
- Removing an action asks first, and you can undo any change.
- If a click opens a new tab, add a `switchTab` action with `new` after it. Use `previous` or `original` to go back.

## Save and undo

- `Ctrl+S` saves. YAML keeps comments. Fields edits re-format JSON.
- `Ctrl+Z` / `Ctrl+Shift+Z` undo and redo up to 100 steps.
- Drafts with errors can't be saved.
- **Save As** (`Ctrl+Shift+S`) never overwrites an existing file. Changing format drops YAML comments.

Leaving with unsaved edits offers **Save**, **Discard**, or **Cancel**. A failed save keeps the dialog and your edits.

If the file changes on disk and you have no unsaved edits, the app reloads it. If you have unsaved edits, the app
keeps them and shows **Reload from disk** (`Ctrl+R`):

- **Cancel** keeps your edits.
- **Discard** loads the version on disk.
- **Save As** saves your edits to a new file, so you keep both.
