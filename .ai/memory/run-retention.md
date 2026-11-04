# Run Retention

Verified: 2026-10-17

- [Registry](../../src/run_history.rs) records each suite's run state, full-suite
  scope, completion order, comparison availability, and deferred log cleanup.
  Import legacy folders before recording a new running capture; never infer
  completion from directory names or a report file written just before termination.
- [Orchestration](../../src/test/test_suites.rs) observes jobs in completion order
  and finalizes each suite independently. Full requested scenario coverage decides
  default eligibility; website failures still qualify, infrastructure failures do
  not. `--skip-compare` leaves snapshot/comparison work deferred.
- [Desktop catalogs](../../src/desktop/results.rs) separate existing-only reads
  from on-demand comparison. Background refresh/recovery must use existing-only
  loading. Partial completed runs stay in inventory for explicit IDs, not defaults.
- Post-run snapshot scope (verified 2026-11-04): scoped and desktop runs no
  longer rebuild the combined report; Results has no Open HTML action. Rules and
  checks live in the [snapshot skill](../../.github/skills/snapshot-command/SKILL.md);
  remaining gaps are in [outstanding issues](../../OUTSTANDING_ISSUES.md#post-run-snapshot-scope-2026-11-04).
- Raw reports mark `captureInventoryComplete`; scoped comparisons no longer
  masquerade as full-run results. Default catalogs/snapshots repair them, explicit
  catalogs retain their scope, and suite shortcuts mark them not yet ready.
- [Cleanup](../../src/run_history/cleanup.rs) reserves the latest full-suite run
  within the strict keep limit, rechecks previews under exclusive artifact locks,
  and persists pending log ownership before removing capture sidecars. Baseline
  links protect original logs even after their capture run is deleted.
- [Staging](../../src/run_history/staging.rs) keeps owner metadata outside `files`,
  so failed payload removal can be retried. Shared owner locks protect active
  operations; deployment startup sweeps abandoned containers. Optimization copies
  and ZIPs live below the owned payload, and the CLI optimizer skips `.staging`.
  `staging_catalog_lock_` reproduces claim/teardown bypasses; both now use the
  catalog lock through final lock-file deletion, deferring cleanup on contention.
- Focused evidence: `run_history`, `suite_run_completion_`, `completed_run_`,
  `run_retention_`, `cleanup_dialog_`, and exact deployment-inventory tests pass.
  GPUI and native Cleanup smoke cover pointer/keyboard controls at compact/large
  sizes in both themes. Preview summaries need named Status elements for native
  accessibility. Six native flows pass; the separate Schema shortcut coverage gap
  remains in [verification notes](../../OUTSTANDING_ISSUES.md#run-retention-and-verification-2026-10-17).
- Final review gates pass 605 CLI tests, 1266 GUI/library tests, and 11 worker
  cases without exclusions. Cached-Chromium loopback capture and native
  capture/cancel/rerun smoke also pass; no browser download or publication was used.
