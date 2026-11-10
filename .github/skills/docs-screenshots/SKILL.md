---
name: docs-screenshots
description: 'Use when capturing, recapturing, or annotating desktop app screenshots for the website docs (website/src/assets/docs/desktop), adding numbered callouts, or updating the screenshot manifest, fixture workspace, or capture scripts.'
---

# Documentation Screenshots

## Start Here

[Capture helper](../../../website/scripts/screenshots/shot.ps1), [annotator](../../../website/scripts/screenshots/annotate.ps1), [manifest](../../../website/scripts/screenshots/manifest.json), [fixture](../../../website/scripts/screenshots/fixture.py), [demo site state](../../../website/scripts/screenshots/site-state.py), output in [desktop assets](../../../website/src/assets/docs/desktop), used by [desktop docs pages](../../../website/src/content/docs/docs/desktop-app).

Windows only. Captures, the demo workspace, and the app-data snapshot live in `$env:REGRESSIFY_SHOTS` (default `%TEMP%\regressify-shots`), never in the repository. Run every helper as `powershell -NoProfile -ExecutionPolicy Bypass -File <script> ...`.

## Procedure

1. Build the binaries the helper launches: `cargo build --release --features gui --bin regressify-desktop --bin regressify` (or set `REGRESSIFY_DESKTOP_EXE`). Close every Regressify Desktop instance; `start` refuses to run beside one. The primary display must fit the 1440×900 window (Windows caps windows at the screen size), so recheck the resolution if captures come out smaller.
1. Build the demo workspace: create `<shots>/northwind-qa`, run `git init` and `regressify init` there, then `python fixture.py <shots>`. Serve the pages with `python -m http.server 4380 --bind 127.0.0.1 --directory <shots>/site`, run `regressify ref --test-suite <id>` and `regressify test --test-suite <id>` for each suite, and commit so Git shows a clean tree.
1. `shot.ps1 start` snapshots the per-user desktop folder, clears settings, opens the workspace, and sizes the window. `shot.ps1 dump` lists accessible names with window-relative rectangles; `find <name>` checks one.
1. Drive the app through accessibility first: `uinvoke <name>` (invoke, toggle, select, or expand), `uvalue <text> -Within <field>`, and `utype <text> -Within <field>` for masked inputs. These need no foreground window and send no real input, so they are safe while the user works elsewhere.
1. Pointer and keyboard actions (`click`, `click right`, `drag`, `keys`, `invoke`) need the app in the foreground. They make the window topmost and try to bring it forward, then refuse to send input if Windows blocks it (focus-stealing protection usually does while the user works in another window). Then ask the user to click the app's title bar, and chain `waitfocus` with the real-input steps in one background command so focus is used before it is lost. Context menus need a real right-click; their items have no invoke pattern, so click the item's rectangle.
1. `shot.ps1 shot <raw>` writes `raw/<raw>.png` and `raw/<raw>.json`. Without `-Names`, it locates every key the manifest annotates on that raw. A `MISSING:` line means a label changed or a control is hidden; fix the manifest or the UI state first.
1. Deployment screenshots: add a target through the dialog with a fake token. An Azure token must look real: 64 hex digits, `-`, 36 characters, the region `00f`, then at least 8 hex digits. Run `python site-state.py <settings json> <workspace>/.regressify` for the newest `config/deployments/*.json`, then `uinvoke "Read the site again"`. The state is synthetic; never deploy with it. Before `stop`, remove the target in the dialog (`Remove target`, `Confirm remove target`) so its token leaves the OS credential store.
1. Unchanged deployments ("Site already up to date", Deploy anyway) need a matching publication record. Compute `deployment_input_fingerprint` for the exact job (suites, Update mode, no removals) with a temporary ignored test built with the release binary's `BINARY_GIT_HASH`, and delete the test afterwards. Then edit the demo settings file: give the target an `https://<name>.example.com/` `publishedUrl` and add a `reportPublications` entry whose `siteFingerprint` is the demo state's fingerprint (`b` × 64) and whose `inputFingerprint` is the computed value. Reopen Deploy so it reloads settings, then `uinvoke "Deploy selected reports"`. The unresolvable URL makes the deployment use the local state, and nothing is uploaded.
1. `shot.ps1 stop` closes only the instance it started and restores the desktop folder; confirm it prints `app data restored`. Remove leftover empty directories for the demo workspace key.
1. Describe each image in `manifest.json`: `file` (asset name), optional `raw` (capture name), `callouts` (`n` plus an accessible-name `key`, or `rect:x,y,w,h` for unnamed areas), `crop` (`x,y,w,h`, `auto`, or `auto:<pad>`), and `masks` that replace machine paths with neutral text such as `C:/Projects/northwind-qa`. Run `annotate.ps1 -Only <file> -OutDir <temp>` to preview, then without `-OutDir` to write the asset.
1. Reference callouts as ①, ② in the page's steps in the same order, describe the numbered items in the image alt text, and update the page's `lastUpdated`.

## Invariants

- Images show only the Northwind fixture: no personal paths, account names, real targets or URLs, client suite names, or tokens. Mask any path the app prints.
- Never send input unless the app is the foreground window. Always finish with `stop`, which clears topmost, closes the app, and restores the desktop folder.
- Keep numbering, crops, and callouts in the manifest so a recapture repositions every callout from fresh rectangles.
- Callout rings use the brand violet and badges the brand gradient; match the existing assets instead of adding new styles.

## Pitfalls

- GPUI split panes ignore synthetic drags, so fixtures must fit the default pane sizes; long suite lists clip their last rows.
- Select pickers do not open through UI Automation; keep the default choice (Azure for targets) or open them with a real click.
- Read the manifest as UTF-8: labels such as `Update 1 · Add 1` contain non-ASCII characters.
- Without a public URL, the Deploy dialog says the site was not read. That is expected with synthetic state.

## Checks

```sh
powershell -NoProfile -Command '$e = $null; foreach ($f in "shot.ps1", "annotate.ps1") { [void][System.Management.Automation.Language.Parser]::ParseFile((Resolve-Path "website/scripts/screenshots/$f"), [ref]$null, [ref]$e); $e }'
bun run --cwd website test
bun run --cwd website build
rumdl check website/src/content/docs/docs/desktop-app
```

Review every changed image at full size and confirm each manifest entry has an asset (and the reverse).

## Maintain

Update the manifest and recapture when labels, accessible names, or layouts change. Keep this skill, the helper comments, and [AGENTS.md](../../../AGENTS.md) entry points current when scripts move.
