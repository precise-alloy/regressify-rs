---
title: Review results
description: Compare captures with references, inspect capture details, approve changes, and clean up old test runs.
sidebar:
  label: Review results
  order: 5
lastUpdated: 2026-10-27
---

:::tip
On macOS, use `Cmd` where this page says `Ctrl`, except `Ctrl+Space`. Click any screenshot to view it full screen.
:::

**Open Report** in Runs opens the report, comparing first if needed. **Results** (`Ctrl+3`) shows each suite's latest
completed full-suite run. Partial, stopped, or failed runs don't replace it; open those right after the run or by
**Run name**.

1. Filter with **Failed** ①, **All**, or **Passed**.
1. Pick a capture ②.
1. Choose a view ③: **Reference**, **Test**, **Diff**, **Side by side**, **Wipe**, or **Overlay**.
1. Use **Fit to window (0)** ④ or the zoom controls.

![Results with the Failed filter, the capture list, the Diff tab, and Fit to window numbered.](../../../../assets/docs/desktop/results-diff.png)

**Wipe** ① shows Reference on one side and Test on the other. Drag the **Wipe** slider ② to move the split. **Overlay** fades Test over Reference. Panes zoom and scroll
together. `Ctrl+B` widens the preview.

![Results on the Wipe tab at 4.90% mismatch, with the Wipe tab and the Wipe slider numbered.](../../../../assets/docs/desktop/results-wipe.png)

| Zoom             | Button                                  | Key      |
| ---------------- | --------------------------------------- | -------- |
| Fit to window    | **Fit to window (0)**                   | `0`      |
| Fit width        | **Fit width (W)**                       | `W`      |
| Actual size      | **Actual size (100%)**                  | `1`      |
| Zoom 25% to 800% | **Zoom in**, **Zoom out**, `Ctrl`+wheel | `+`, `-` |
| Pan              | Drag                                    | Arrows   |

Double-click an image to toggle actual size.

## Capture details

- Hover the info icon ① for selector, URLs, and diff values.
- **Copy capture details** ② copies a summary.
- **Edit suite** ③ opens the file.
- **Capture Log** ④ shows the log here.

![The comparison heading with the info icon, Copy capture details, Edit suite, and Capture Log numbered.](../../../../assets/docs/desktop/capture-details.png)

Right-click an image to open it, show it in the file manager, or copy its URL. Failed captures show an error image that
is never saved as a reference; existing references stay.

## Approve a change

1. Select the capture and click **Approve This Scenario** ①.
1. Check the viewport and run ②.
1. Click **Cancel** ③ or **Approve This Scenario** ④.

![The approval confirmation with Approve This Scenario, Viewport and Run, Cancel, and the confirm button numbered.](../../../../assets/docs/desktop/approve-scenario.png)

Only the viewport shown is approved. Failed captures can't be approved. Use [**Approve Results**](/docs/desktop-app/running-tests/) in Runs for a whole
run.

## Clean up old test runs

1. Click **Clean up old test runs** in Overview, Results, or workspace settings.
1. Set **Completed runs to keep** ① (default 1).
1. Pick suites ②.
1. Click **Preview** ③ to see what stays and the space saved.
1. Click **Delete old runs** ④, or **Close** ⑤ to delete nothing.

![The Clean up test runs dialog with Completed runs to keep, suites, Preview, Delete old runs, and Close numbered.](../../../../assets/docs/desktop/clean-up-runs.png)

References, login state, source files, and unselected suites are never touched. Each suite keeps its latest completed
full-suite run. Stopped or incomplete runs can be removed and don't count.

For automatic cleanup, set **Automatic run cleanup** and **Test runs to keep** in Fields > Execution (off and 1 by
default; suite overrides workspace). It runs after full-suite tests, and its errors never change results.
