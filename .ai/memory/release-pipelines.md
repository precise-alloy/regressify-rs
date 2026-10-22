# Release Pipelines

Verified: 2026-10-22

- [GitHub](../../.github/workflows/tuyen-release.yml) publishes only automatic
  tags in `precise-alloy/regressify-rs`; Azure publishing is retired. Ten build
  jobs and offline checks run independently. Publication waits for all of them
  plus Windows packaging, then deployment waits for verified publication.
- [Staging](../../ci/release_feed.py) requires all 20 archives, 20 checksums, and
  setup before writing output. The manifest is the 42nd uploaded asset. Drafts
  become public only after exact names, sizes, uploaded state, URL, and GitHub
  SHA-256 digest checks. Reruns never clobber published assets. Deployment repeats
  the public inventory check before website build.
- Only `publish` receives write permission and the built-in GitHub token.
  Only Cloudflare deployment receives `CLOUDFLARE_ACCOUNT_ID` and
  `CLOUDFLARE_API_TOKEN`. Hosted execution remains unverified; see
  [outstanding issues](../../OUTSTANDING_ISSUES.md).
- GitHub retains binaries; the website serves `/downloads/latest.txt`, terminal
  installers, pages, and `/mcp`. The feed records every archive as
  `archive.<filename>=<size>:<sha256>`; Rust additionally requires Windows ZIPs.
  Binary limits are 1 GiB, manifest 16 KiB. Cloudflare's binary size limit is no
  longer part of application distribution.
- [Offline tests](../../ci/test-release.py) exercise all 192 GitHub selections,
  full inventories and mocked publication, all supported Unix target scripts,
  and mocked Windows script execution. Set `BASH_PATH` to Git Bash on Windows.
- The [bootstrapper](../../installer/regressify.iss) installs both executables
  directly in `{app}` and registers that shared directory only in the user PATH.
  Ownership metadata survives upgrades; uninstall preserves unowned entries,
  later PATH edits and the registry value type. The packaging job sets
  `ISCC_PATH` and run `python ci/test-release.py InstallerTests`: native fixtures
  use isolated registry keys and a temporary synthetic setup/uninstall cycle,
  without changing the real user PATH or app registration. The separate release
  smoke test verifies actual CLI lookup from a fresh process and uninstall cleanup.
- Inno's uninstaller launcher exits before its second-phase PATH cleanup. The
  [smoke helper](../../ci/build-installer.ps1) uses bounded
  `Start-Process -Wait` to await descendants and retains `uninstall.log`.
  Native regression assertions run inside the invoking PowerShell process so
  inherited output handles cannot hide an early return; separate fixtures cover
  nonzero exits and timeout cleanup of a surviving second phase.
- Prepare-only mode is verified with the real compiled script and synthetic ZIPs
  while the app mutex is held. Corrupt sources remove a stale ready receipt.
  Returning false from InitializeSetup returns 1; forced ExitProcess skips cleanup.
- The deferred [bridge rollout](../../docs/desktop.md#bridge-rollout) starts from
  `d066dc81a2befb9cb622762cbf3c7a21e8c198fc` only after live-site confirmation.
  Old clients need an old-name installer on the old feed, containing a backported
  new updater. Numeric ordering must be old < bridge < site.
