---
title: Open a workspace
description: Open a workspace folder, find suites and scenarios in the navigator, and check suite status on Overview.
sidebar:
  label: Workspaces
  order: 2
lastUpdated: 2026-10-27
---

:::tip
On macOS, use `Cmd` where this page says `Ctrl`, except `Ctrl+Space`. Click any screenshot to view it full screen.
:::

A workspace is the folder with your `regressify.yaml` and suites.

1. Start the app and click **Open workspace** ①.
1. Pick the folder.

![The No workspace open screen with Open workspace numbered.](../../../../assets/docs/desktop/open-workspace.png)

You can also open a workspace with:

- `Ctrl+O` or **Switch workspace** in the title bar
- **Open with Regressify Desktop** in the Explorer right-click menu
- **Recent Workspaces** on the taskbar icon
- `regressify-desktop <folder>` in a terminal

You can't switch workspaces during a run. **File** > **Close workspace** unloads the workspace but keeps the window
open.

Only open workspaces you trust. Configuration can run scripts in the browser, and the app is not a sandbox.

The app creates missing starter files (`regressify.yaml`, `common/` helpers and viewports, `visual_tests/`,
`.vscode/mcp.json`, and an `AGENTS.md` section) and never overwrites existing ones. For an example suite and schemas,
run **More actions** > **Initialize Workspace** in Runs (same as `regressify init`).

## Navigator

- Search suites and scenarios ① (`Ctrl+P`) by name, path, label, ID, or URL.
- **New suite** ② (`Ctrl+N`).
- **Selected** ③ shows only selected suites. Rows show queued, running, passed, and failed counts.
- **Scenarios** ④ lists the open suite. Hover for the full URL. Unlabelled scenarios get a display name from the URL
  that isn't saved.
- **Workspace files** ⑤ lists configuration and `common`. Text files up to 4 MiB open in the editor.

![The navigator with the search box, New suite, Selected tab, Scenarios pane, and Workspace files numbered.](../../../../assets/docs/desktop/navigator.png)

To edit other data files, such as a custom viewport file, use **File** > **Open configuration file**. The **File**
menu also has **Duplicate suite**, **Create viewport file**, and **Open in VS Code**.

`Ctrl+B` hides the navigator. Drag the divider to resize the panes.

| Action                                      | Mouse                   | Keyboard                    |
| ------------------------------------------- | ----------------------- | --------------------------- |
| Select one row                              | Click                   | `Up`, `Down`, `Home`, `End` |
| Add or remove a row                         | `Ctrl`+click            | `Space`                     |
| Select a range                              | `Shift`+click           | `Shift`+arrows              |
| Select all, including rows hidden by search | **All**                 |                             |
| Select all visible rows                     |                         | `Ctrl+A`                    |
| Clear selection                             | Empty space or **None** | `Ctrl+Shift+A`, `Escape`    |
| Open the focused row                        | Click                   | `Enter`                     |

With a search filter on, **All** also selects hidden rows, so check before you run. Clearing the selection keeps
unsaved edits.

## Overview

A workspace opens on **Overview** with totals, configuration issues, and each suite's status. Tick a suite to include
it in runs.

- **Suite details** ①: full suite details.
- **Edit** ②: open the suite in the editor.
- **Open Report** ③: open saved reports for the selected suites (shown once they have a saved run).
- **Clean up old test runs** ④: see [Clean up old test runs](/docs/desktop-app/reviewing-results/#clean-up-old-test-runs).
- **Run Reference** ⑤ and **Run Test** ⑥: run the selected suites.

![Overview of the northwind-qa workspace with Suite details, Edit, Open Report, Clean up old test runs, Run Reference, and Run Test numbered.](../../../../assets/docs/desktop/overview.png)
