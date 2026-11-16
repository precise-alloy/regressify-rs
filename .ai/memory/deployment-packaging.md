# Deployment Packaging

Verified: 2026-11-13

- The [worker](../../src/desktop/worker.rs) regenerates selected snapshots, then
  stages only the active SPA manifest and its versioned dependencies. The shared
  [optimizer](../../src/desktop/deployment/optimization.rs) uses Azure's staged
  files or Cloudflare's validated asset list, never the original output tree.
  Budgets are 250 MiB for Azure Free/unknown and 500 MiB for Standard; Cloudflare
  sets none outside tests ([why](deployment-cloudflare.md)). Fitting reports need no
  optimization copy; Netlify is unchanged. The optimizer's quality floors and
  persistent cache are described in the
  [packaging skill](../../.github/skills/deployment-packaging/SKILL.md).
- [Artifact reads](../../src/desktop/deployment.rs) require a canonical root for
  containment checks, including Windows extended-length paths. Cloudflare reads
  hosting policies from the original canonical staging root; its temporary
  optimization copy contains only public assets and stays alive through verification.
- The [runner](../../src/desktop/runner.rs) must keep unchosen report text as
  `None`, otherwise generated CLI flags override workspace snapshot settings.
  Saved activity version 1 prefilled built-in text; loading it migrates those
  strings to inheritance. Version 2 preserves explicit defaults and custom text.
  The [worker staging test](../../tests/desktop_worker.rs) verifies configured
  title in staged HTML and heading/Markdown in the versioned manifest without publishing.
- [Snapshot conversion](../../src/snapshot/capture_report.rs) uses completed
  full-suite registry records, comparing missing reports on demand; legacy suites
  retain `config.js` selection. It preserves captures and produces
  stable-path PNG copies with `?v=`. [Staging](../../src/desktop/deployment.rs) follows
  the active manifest, verifies cache versions, and excludes raw reports, captures,
  state, and stale assets. Query strings never become filesystem paths.
- [Frontend build](../../frontend/scripts/sync-assets.ts) generates `FRONTEND_FILES`.
  Source fonts are WOFF2 in `frontend/src/assets/fonts`; capture error artwork is
  owned by `src/test/data`, outside the generated snapshot directory.
- Compression roles come from suite data. Post-compression updates image query
  versions, suite JSON versions, and the entry manifest without changing paths.
  Optional capture `previews` are inventory dependencies and protected images;
  snapshot tests cover their version refresh and suite rename/delete cleanup.
  Versions use `get_asset_version(bytes, 20)` (SHA-384/base64url). Regression coverage:
  `cargo test --features desktop --lib snapshot::site::tests` and
  `cargo test --features desktop --lib desktop::deployment::cloudflare::tests::optimized_snapshots_refresh_image_suite_and_manifest_versions`.
- Verification: `cargo test --features desktop --lib desktop::deployment::tests::staging_includes_only_selected_report_dependencies`
  asserts the exact ZIP entry set and unchanged source files. Checking only that
  required files exist does not detect extra uploaded files.
- [Managed staging](../../src/run_history/staging.rs) tracks payload ownership
  outside staged files, cleans up best effort, and retries abandoned operations
  before deployment. Optimization copies and ZIPs stay under that payload; no
  ownership or run-registry metadata enters the exact deployment inventory.

See the [packaging skill](../../.github/skills/deployment-packaging/SKILL.md) for
the maintained procedure and checks.
