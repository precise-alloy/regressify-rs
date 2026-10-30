---
title: Run tests
description: Capture references, run visual tests, choose run settings, and follow progress and logs.
sidebar:
  label: Run tests
  order: 4
lastUpdated: 2026-10-27
---

:::tip
On macOS, use `Cmd` where this page says `Ctrl`, except `Ctrl+Space`. Click any screenshot to view it full screen.
:::

Press `Ctrl+2` for **Runs**.

| Button              | Does                                 |
| ------------------- | ------------------------------------ |
| **Run Reference**   | Captures baselines                   |
| **Run Test**        | Captures and compares with baselines |
| **Approve Results** | Makes test results the new baselines |
| **Compare Results** | Compares an earlier run again        |

**More actions** has **Build Report**, **Optimize Images**, **Install Chromium**, and **Initialize Workspace**.

## First run

1. Select suites on the **Suites** tab ① or in the navigator.
1. Click **Run Reference** ②.
1. Confirm "Replace reference images with new captures?" ③, or click **Cancel** ④.
1. When it finishes, click **Run Test**.

![The Runs view with Suites, Run Reference, the confirmation message, and Cancel numbered.](../../../../assets/docs/desktop/run-reference-confirm.png)

**Run Test** needs at least one successful reference per selected suite. With nothing selected, suite commands are
disabled; the app never quietly runs everything. **Run Reference**, **Approve Results**, and **Optimize Images** always
ask first.

To narrow a run, click scenarios in the navigator (`Ctrl`/`Shift` for several), or use a scenario row's **Run Test**
play button. The editor has **Run Suite Test** and **Run Suite Reference**. With unsaved edits you choose **Save and
continue**, **Run saved version**, or **Cancel**.

## Run settings

1. Open **Run settings** ①.
1. **Compare after test** ② compares captures right away.
1. **Run name (optional)** ③ targets a run for Approve and Compare. Empty means the last run, only if the same suites,
   scenarios, and output folder are selected. Type it to be sure what you approve.
1. **Browser download** ④: **Auto**, **Always**, or **Never**. With Never, run **Install Chromium** once.

![The Run settings tab with Run settings, Compare after test, Run name (optional), and Browser download numbered.](../../../../assets/docs/desktop/run-settings.png)

Use the same Regressify version on every QA machine, and recheck baselines when the browser version changes.

## Watch progress

1. Open **Activity** ① to see progress and the result for each scenario and viewport.
1. Turn on **Failures only** ② to hide passing rows.
1. Click a row ③ to open its log.

![Runs Activity showing 4 failed, with Activity, Failures only, and a failed row numbered.](../../../../assets/docs/desktop/runs-activity.png)

Counts are per screenshot: 4 scenarios on 3 viewports give 12. "Captured" isn't passed until compared. "3 failed, 5
passed" means the run worked; "Command failed" means a configuration, browser, or file-saving problem.

One command runs per window. **Stop** ends it and its browser. Closing the window stops the run first; close it
again once the run has stopped.

## Logs

1. Open **Log** ①, or click a row in Activity.
1. **Follow** ② keeps the newest entries visible. `Ctrl+F` searches.
1. **Copy displayed page** ③ copies the current page (500 entries). **Download complete filtered log** ④ saves every
   matching entry.

![The Log tab with Log, Follow, Copy displayed page, and Download complete filtered log numbered.](../../../../assets/docs/desktop/runs-log.png)

Logs never contain text you type into pages, but they can contain URLs, selectors, and script output. Check a log
before you share it.

When you reopen a workspace, Runs shows the last run's results without running anything.
