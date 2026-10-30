---
title: Troubleshooting
description: Fix common install, run, update, and deployment problems in Regressify Desktop.
sidebar:
  label: Troubleshooting
  order: 11
lastUpdated: 2026-10-27
---

## Install

### Windows or macOS says it can't verify the app

Regressify isn't signed with a publisher certificate yet (on macOS the installer only ad-hoc signs
`Regressify.app`), so the operating system can't confirm who published it. First check that the file's SHA-256 matches the one on the [download page](/download/)
(**Archives & checksums**). The install scripts check this for you. Then:

- **Windows** ("Windows protected your PC"): click **More info**, then **Run anyway**. The PowerShell install command
  usually doesn't trigger this warning.
- **macOS** ("Apple could not verify … is free of malware"): click **Done**, open **System Settings** > **Privacy &
  Security**, scroll to **Security**, click **Open Anyway** next to the blocked file, and confirm with your password.
  Do this once for `regressify` and once for `regressify-desktop` (or `Regressify.app` if you opened it from Finder). Or clear the download flag in Terminal:
  `xattr -d com.apple.quarantine regressify regressify-desktop`. The `curl` install command usually doesn't trigger
  this warning.

On a company-managed computer, **Run anyway** or **Open Anyway** may be missing. Ask your IT team to allow Regressify
instead of turning off SmartScreen or Gatekeeper.

### `regressify` isn't found

Open a new terminal, or restart VS Code or Windows Terminal.

### Chromium didn't download

Set **Proxy** in [**Run settings**](/docs/desktop-app/running-tests/#run-settings), then **More actions** > **Install Chromium**.

## Runs and editing

### Run Test is disabled

Select a suite and run [**Run Reference**](/docs/desktop-app/running-tests/#first-run) first.

### My VS Code edits didn't appear

You have unsaved edits in the app. Use [**Reload from disk**](/docs/desktop-app/editing-suites/#save-and-undo) or **Save As**.

### A run shows failures

That's a result to review, not an error. Only "Command failed" is an error.

## Updates

### No update banner

It appears only after the installer downloads and passes its checksum. Click [**Check for
updates**](/docs/desktop-app/install/#updates); it also retries a version that failed its checksum. Automatic checks skip a version that keeps failing its
checksum until a newer one is published. Only official Windows builds update.

## Deployment

### CORS error behind Cloudflare Access

Older reports don't send your sign-in. Rerun or regenerate, then redeploy.

### "Access is denied (os error 5)" on deploy

A local permission issue, such as Controlled folder access. Don't
disable protection or run as administrator. Report the full error, without credentials.
