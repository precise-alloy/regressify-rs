---
title: Install the desktop app
description: Install Regressify Desktop and the CLI on Windows, macOS, or Linux, and keep Windows builds up to date.
sidebar:
  label: Install
  order: 1
lastUpdated: 2026-10-27
---

You can install Regressify on Windows, macOS, and Linux from the [download page](/download/):

- **Windows (PowerShell)**: `irm https://regressify.eshn.dev/install.ps1 | iex` or download `Regressify-Setup-N.N.N.N-x64.exe`
- **macOS and Linux**: `curl -fsSL https://regressify.eshn.dev/install.sh | sh`

On macOS the install command also creates `Regressify.app` in `/Applications` (or `~/Applications` if
`/Applications` isn't writable by your user), so you can open it from Finder, Launchpad or Spotlight. The
`regressify` and `regressify-desktop` commands are still installed to `~/.local/bin`.

On Windows, setup installs to `%LOCALAPPDATA%\Programs\Regressify` without administrator rights and:

- adds desktop and Start-menu shortcuts;
- installs the `regressify` CLI and adds it to your user PATH (open a new terminal, and restart Windows Terminal or VS
  Code if they were open);
- adds **Open with Regressify Desktop** to the folder right-click menu (**Show more options** on Windows 11);
- downloads Chromium to `%LOCALAPPDATA%\regressify\chromium` if missing. The CLI shares it. Installed Chrome or Edge
  can't be used instead.

The test engine is built in, and each run uses its own background process.

**If Chromium fails to download**, the app still installs. Setup uses the system proxy. Set **Proxy** in Runs >
[**Run settings**](/docs/desktop-app/running-tests/#run-settings), then choose **More actions** (…) > **Install Chromium** and wait for "Chromium is ready". Running a
test or reference also retries.

**Uninstalling** removes the app, CLI, shortcuts, menu entries, and PATH entry. Workspaces, preferences, and the
Chromium cache are kept.

## Updates

Official Windows builds check at startup and every eight hours. You're only notified once the installer is downloaded
and its checksum matches. Updates never install on their own.

1. Click **Software updates** in the title bar. When an update is ready, a banner also offers **Install update**.
1. Click **Check for updates** to check now.
1. Click **Install and restart** (disabled during a run). If you have unsaved edits, choose **Save**, **Discard**, or
   **Cancel**.
1. The installer runs and your workspace reopens. It may ask you to close other windows but never force-closes them.

**Pause download** postpones retries for eight hours. **Copy details** appears after a failure. Closing the dialog
doesn't stop the download.
