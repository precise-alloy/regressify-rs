# Release Pipelines

Verified: 2026-10-29

- [GitHub](../../.github/workflows/tuyen-release.yml) publishes only automatic
  tags in `precise-alloy/regressify-rs`; Azure publishing is retired. Eight build
  jobs and offline checks run independently. Publication waits for all of them
  plus Windows packaging, then deployment waits for verified publication.
- [Staging](../../ci/release_feed.py) requires all 16 archives, 16 checksums, and
  setup before writing output. The manifest is the 34th uploaded asset. Drafts
  become public only after exact names, sizes, uploaded state, URL, and GitHub
  SHA-256 digest checks. Reruns never clobber published assets. Deployment repeats
  the public inventory check before website build.
- v2.0.6 uploaded all 34 assets but rejected the draft manifest's download URL.
  Draft assets use the `untagged-...` segment from the release's `html_url`.
  Pre-publication checks bind URLs to that exact validated GitHub/repository path;
  all state/size/hash/name checks remain. Published checks require the real tag.
  Read-only verification matched the live manifest bytes/hash and asset metadata.
- GitHub's release-by-tag endpoint is published-only. Draft discovery uses an
  authenticated release listing with exact tag matching, 10 entries per page,
  and a 1000-release cap. v2.0.4 created an empty draft but failed when re-querying
  it by tag; stateful tests now model that distinction and resumed partial uploads.
  Draft page URLs may use `untagged-...`; `tag_name` identifies the intended tag.
- v2.0.5 also failed an immediate post-create lookup despite correct metadata on
  a later read. Creation now uses the JSON API response directly after checking
  the exact Git tag exists; listings are only for initial discovery/resume.
  Reads send `Cache-Control: no-cache`, and write failures never mean not found.
  Tests keep new drafts hidden from listings while returning creation metadata.
- Only `publish` receives write permission and the built-in GitHub token.
  Only Cloudflare deployment receives `CLOUDFLARE_ACCOUNT_ID` and
  `CLOUDFLARE_API_TOKEN`. Hosted publication/deployment remain unverified; see
  [outstanding issues](../../OUTSTANDING_ISSUES.md).
- GitHub retains binaries; the website serves `/downloads/latest.txt`, terminal
  installers, pages, and `/mcp`. The feed records every archive as
  `archive.<filename>=<size>:<sha256>`; Rust additionally requires Windows ZIPs.
  Binary limits are 1 GiB, manifest 16 KiB. Cloudflare's binary size limit is no
  longer part of application distribution.
- The [website Worker](../../website/worker/index.ts) wraps the generated MCP
  handler without changing its export template. Admin page/API routes run Worker
  first, rely on Cloudflare Access for `/admin` and `/admin/*`, and use server-only
  PostHog secrets. Combined typecheck and dry-run build precede deployment; see
  [local checks and setup](../../website/README.md#private-admin).
- [Website-only deploys](../../.github/workflows/website-deploy.yml) skip the app
  build. `website/mcp-worker` is git-ignored and bundled into the same Worker
  script, so each deploy regenerates it with the live release's Linux CLI. The
  zip must match the live feed's `archive.` SHA-256: only the release pipeline
  writes that feed, while `gh release download` verifies nothing.
  Docs deploys and release deploys share no concurrency group; that overlap risk
  was accepted on 2026-10-29.
- Workerd rejects `fetch` with `redirect: 'error'`; the admin feed uses `manual`
  and rejects non-2xx responses without forwarding bearer credentials. Website
  tests are scoped to `src` so an ignored MCP export cannot pollute discovery.
- [Offline tests](../../ci/test-release.py) exercise all 48 GitHub selections,
  full inventories and mocked publication, all supported Unix target scripts,
  and mocked Windows script execution. Set `BASH_PATH` to Git Bash on Windows.
  Release checks also run on `macos-15` (Bash 3.2 only, no GNU `sha256sum`),
  so tests that run ubuntu-only workflow steps skip there (`website_step_bash`).
- Build jobs use GitHub Actions' default timeout, without `timeout-minutes` or a
  matrix `timeout` field. v2.0.3 failed offline checks because a stale desktop
  assertion still indexed that field; matrix regressions now assert its absence.
- Both Musl jobs in the v2.0.0 run failed in `openssl-sys`: reqwest's default TLS
  enables native OpenSSL without a target sysroot. The old ARM64 setup also used
  a glibc compiler for Musl. [Managed Chromium](../../src/test/browsers.rs) rejects
  Linux ARM64; Musl and Linux ARM64 were removed from prebuilt releases, website
  inventory, and terminal selection rather than advertising unverified support.
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
- v2.0.2 compiled setup and passed installation smoke, then failed the license
  cleanup check because a malformed `Join-Path` expression referenced undefined
  variables. The check must use the installation directory. An AST-extracted
  [runtime regression](../../ci/test-release.py) covers removed/leftover licenses
  and unrelated files; parser-only checks cannot detect this failure.
- Prepare-only mode is verified with the real compiled script and synthetic ZIPs
  while the app mutex is held. Corrupt sources remove a stale ready receipt.
  Returning false from InitializeSetup returns 1; forced ExitProcess skips cleanup.
- The deferred [bridge rollout](../../docs/desktop.md#bridge-rollout) starts from
  `d066dc81a2befb9cb622762cbf3c7a21e8c198fc` only after live-site confirmation.
  Old clients need an old-name installer on the old feed, containing a backported
  new updater. Numeric ordering must be old < bridge < site.
