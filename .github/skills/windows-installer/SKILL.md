---
name: windows-installer
description: 'Use when changing Inno Setup packaging, Windows installer versions, shell shortcuts/context menus, Chromium preparation, silent progress, upgrade/uninstall, or installer smoke tests.'
---

# Windows Installer

## Start Here

[Inno script](../../../installer/regressify.iss),
[builder/smoke](../../../ci/build-installer.ps1),
[release tests](../../../ci/test-release.py), and [resources](../../../build.rs).

## Procedure and Invariants

1. Preserve canonical four-part `N.N.N.N` installer/PE versions, matching executable
   resources, x64 artifact naming, and static MSVC runtime verification.
1. Build `Regressify-Setup-N.N.N.N-x64.exe` as a bootstrapper. Require both verified
   Windows executables and their exact release ZIPs; validate ZIP entry names,
   sizes, and bytes against the binaries/licenses before baking archive hashes.
   Pass the exact version-matched tag, with no URL fallback or runtime latest lookup.
   The CLI retains its Cargo version. Extract and register files through Inno so
   uninstall tracks them; recheck hashes immediately before extraction.
1. `/PREPARE=<cache>` downloads/verifies both archives without installation or
   app/setup exclusion. Returning false from InitializeSetup cleans temporary
   files and returns 1; a fresh receipt and both hashes additionally prove success.
   `/SOURCE=<cache>` must fail closed for missing/bad files, never fetch substitutes.
1. Keep the AppId, data paths, credentials, and GUI identity stable. Setup's name,
   default install directory and Start-menu group are Regressify; GUI shortcuts
   remain Regressify Desktop. Migrate the registered legacy directory only after
   successful installation; delete only known owned files and preserve user data.
1. Register `{app}` only in the user PATH. Preserve raw values, registry types,
   pre-existing entries and later edits; keep ownership across upgrades and remove
   only the owned entry on uninstall. Notify Explorer of environment changes.
1. Keep setup/app exclusion, worker-startup guards, per-user shell integration,
   AppUserModel identity, and quoted workspace relaunch paths aligned.
1. Attempt managed Chromium preparation through the intended headless helper.
   Cache reuse and recoverable offline failure must remain distinguishable in logs.
1. Inno `WizardSilent` includes both `/SILENT` and `/VERYSILENT`. Visible progress
   must follow actual wizard visibility; failure dialogs retain their silent guard.
1. `ExecAndLogOutput` callback Error means capture failure/limit, not stderr. Normal
   Chromium progress uses stderr; do not remove error checks to display progress.
1. Preserve user data on upgrade/uninstall. Smoke tests must refuse an existing
   app registration, shell integration or PATH ownership record; never bypass
   these guards on this machine.
1. Inno's first-phase uninstaller exits before second-phase PATH cleanup completes.
   Use the smoke helper's bounded `Start-Process -Wait` process-tree wait, not only
   `Process.WaitForExit`. Keep timeout child cleanup and the fixture uninstall log.

## Checks

```sh
python ci/test-release.py
```

Requires PyYAML and PowerShell 7.4+. For authorized isolated Windows installer testing, build the
matching desktop executable and companion CLI, then run `./ci/build-installer.ps1` with `-Version`, `-ReleaseTag`,
`-Compiler`, `-RequireStaticRuntime`, and `-SmokeTest`. Use the actual four-part
release version, not the three-part Cargo package version. Inno Setup is required.

Set `ISCC_PATH` to an Inno Setup 6 compiler and run
`python ci/test-release.py InstallerTests` for native PATH fixtures, a temporary
synthetic setup/uninstall cycle through the actual smoke helper, and a
synthetic-payload compile without installing the app or changing the real user
PATH. Assert cleanup inside the invoking process so inherited output handles do
not mask premature return; cover both parent and orphaned second-phase timeouts.
Execute the uninstall license check from its PowerShell AST against temporary
directories: accept removed files, reject each leftover license, and preserve
unrelated files, including paths with spaces and literal brackets. Parsing alone
does not catch syntactically valid expressions referencing undefined variables.
The Windows packaging job requires this gate. The Inno 6 setup engine is 32-bit
even in x64 install mode; keep external Win32 declarations compatible. Inno's
`RegWriteStringValue` preserves an existing `REG_EXPAND_SZ` type, so remove only
the isolated fixture value before seeding a `REG_SZ` test case.

## Maintain

Update [desktop docs](../../../docs/desktop.md), release tests, and this skill.
Comment silent-mode/platform workarounds and verify fresh install, upgrade,
uninstall, running-app rejection, shell paths, and offline preparation failure.
