# Shared Desktop Updates

Verified: 2026-10-22

- The feed is `https://regressify.eshn.dev/downloads/latest.txt`; binaries are
  versioned GitHub assets. Manifest redirects are refused; bounded GitHub HTTPS
  binary redirects are allowed. Release builds embed `REGRESSIFY_UPDATE_ORIGIN`.
  See the deferred [Tuyen Drive bridge](../../docs/desktop.md#bridge-rollout).
- [UpdateFeed::check_on_startup](../../src/desktop/updates.rs) requests fresh
  metadata for each newly opened instance, regardless of `last_check_at`, without
  resetting pauses or checksum retry limits. Subsequent `check_shared` calls use
  the shared timestamp for the eight-hour interval. Startup and manual checks
  refresh that timestamp; old state files default to due.
- `UpdateLease` holds `coordinator.lock` before any network check and across
  preparation. Inner `update.lock` operations preserve existing state/hash guards.
  A peer returns Busy without a feed request. Dropping or exiting the owner
  releases the OS lock; another process can reuse metadata and resume partials.
- Readiness requires the shell installer and both Windows ZIPs. Verified setup
  runs `/PREPARE`, then the updater checks the receipt against feed-provided
  sizes/hashes. A forged local receipt cannot authenticate changed archives.
  `/SOURCE` installation rechecks baked hashes without downloading substitutes.
- The preparation job owns a Windows PowerShell `Start-Process -Wait` wrapper:
  command-group 5.0.1 `try_wait` can finish when only the launcher exits. Native
  regressions verify descendant cleanup for timeout, Pause, and handoff.
- [UpdatePanel](../../src/desktop/ui/updates.rs) owns a window-scoped cache watcher.
  A pending startup check survives Busy responses and completes once ownership
  becomes available; later five-second polls follow the shared schedule.
  Each window verifies the completed local installer before displaying the same
  ready banner, without another transfer. Existing banners survive idle polls.
- Explicit pause and preparation errors defer automatic work eight hours without
  consuming checksum retries. Owner shutdown uses `cancel_for_handoff` to permit
  takeover, without overriding an already requested pause. Clear expired local
  retry timers when a peer owns the cycle; rescheduling a due timer can spin.
- Checks: `cargo test --features desktop --lib desktop::updates::tests` and
  `cargo test --features gui-tests --lib desktop::ui::updates::tests -- --test-threads=1`.
  Fixtures cover fake-clock scheduling, child-process contention/release,
  startup inside a recent check interval, one-transfer reuse, takeover/pause,
  and two windows showing the ready banner.
  Tests do not download or install a real update.

See the [updater skill](../../.github/skills/desktop-updates/SKILL.md) and
[desktop docs](../../docs/desktop.md) for contracts and user-visible behavior.
