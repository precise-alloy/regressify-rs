# Test Parallelism

Verified: 2026-11-03

## Facts

- Nothing in the code forced serial tests. The only reason was the documented
  `-- --test-threads=1` flag, which was removed from CI, README, docs and skills.
  [desktop-checks.yml](../../.github/workflows/desktop-checks.yml) now uses
  the default thread count.
- GPUI tests (`#[gpui_kit::test]`) each get their own test dispatcher.
  `TestAppContextExt::wait_for` timeouts use simulated time, so they do not slow
  down under load. Real threads and child processes still run in wall-clock time.
- Isolation patterns:
  - Tempdirs and injected paths: `Workbench::new(preferences_path, ..)` in
    [ui/mod.rs](../../src/desktop/ui/mod.rs), plus the `*_in` helpers such as
    `DeployTargets::path_in`, `TokenCatalog::path_in`, `RunSnapshot::path_in` and
    `DesktopInstance::open(dir, root)`.
  - The keyring is mocked.
  - Tests that need process globals (`OnceLock` roots, diagnostics routes) re-run
    `current_exe()` with `--exact` and an env marker.
- The one leak of real user state was `Workbench::new` reading the real desktop
  preferences file. It is now injected; see the test
  `workbench_loads_only_the_injected_preferences_path` in
  [ui/tests.rs](../../src/desktop/ui/tests.rs).
- Windows named setup mutex: the opt-in installer test holds the session-wide
  desktop setup mutex, so other `regressify-desktop` launches fail. In
  [desktop_worker.rs](../../tests/desktop_worker.rs) a `SETUP_MUTEX` `RwLock`
  serializes that test (write lock) against desktop-launching tests (read lock).
- Product bug surfaced by parallel load: a navigation could destroy a cached
  isolated-world context before its event arrived. `Runtime::evaluate` in
  [runtime.rs](../../src/test/runtime.rs) now evicts the cached frame and retries on
  "Cannot find context with specified id". The regression test is
  `evaluation_recovers_when_navigation_destroys_a_cached_context`.

## Pitfalls

- Fixed wall-clock budgets that include child-process startup can flake under
  parallel load. Wait for readiness the descendant signals itself, and keep the
  process alive while waiting.
  - [updates.rs](../../src/desktop/updates.rs):
    `preparation_cancellation_and_timeout_reap_launcher_descendants` failed once
    in parallel. Its fixture now checks child liveness instead of using a 5 s
    deadline. The test retries with a doubled preparation timeout only when the
    descendant never became ready.
  - [runner.rs](../../src/desktop/runner.rs) `orphan-pipe` fixture: the parent
    exits only after the orphan writes a ready marker. Before this, the orphan
    raced the 250 ms `PIPE_DRAIN_TIMEOUT`.
- Browser fixture setup waits (iframe load, initial page markers) are hang guards.
  Bound them generously (30 s). Keep the short bounds only on behavior that is
  being tested, such as the hooks deadline case in
  [hooks.rs](../../src/test/hooks.rs). The same applies to a `ResolvedConfig`
  `element_timeout_ms` override that bounds actions expected to succeed: the
  5 s `switchTab` budget in [tabs.rs](../../src/test/tabs.rs) and the 250 ms
  viewport-gate click in [test_scenario.rs](../../src/test/test_scenario.rs)
  failed under load, so they now use the 30 s default.
- Screenshot fixtures on a fresh `browser.new_page` tab must set device metrics
  first, as scenario captures do (`apply_viewport` in
  [test_scenario.rs](../../src/test/test_scenario.rs)). Without them, under
  parallel load the first `fromSurface` capture can return a stale, tiled surface
  sized before the headless window settled. This caused the
  `screenshot_preparation_hides_carets_without_blurring_inputs` flake, and the
  pixel diff showed it was not the caret. Separately, 1 in 32 runs at 8 concurrent
  browsers hit a transient CDP `-32000 Unable to capture screenshot`.
- Real Chromium in unit tests is capped by a `cfg(test)` semaphore in
  [browsers.rs](../../src/test/browsers.rs) (`test_browser_slots`: a quarter
  of the logical CPUs, minimum 2), acquired in `launch_browser` and held until the handler
  ends. Unbounded launches overloaded the opt-in `--ignored` suite (CDP request
  timeouts, `switchTab` deadlines). Half the CPUs (11 browsers, about 250 Chromium
  processes on a 22-thread host) still pushed CPU to ~90% with heavy paging and
  failed 6–10 tests per run; check `Get-Counter` before blaming waits.
  Wrap the handler in its abort guard before
  `tokio::spawn`: an abort that lands before the first poll otherwise detaches
  the handler, so the browser keeps running after its slot is released. The cap
  is per process: tests that re-run `current_exe()` as a child get their own
  semaphore.
- tiny_http 0.12 (current upstream) can starve connections. Its pool only
  notifies when a worker counts as waiting, and a notified worker keeps counting
  until it wakes; tasks spawned in that window queue with no wakeup while the
  four workers serve Chromium's idle keep-alive sockets. Under parallel load
  image requests hung forever (one extra `Connection: close` request freed a
  worker and the stuck test passed in 2 s). The browser fixture `Site` in
  [browser_tests.rs](../../src/test/browser_tests.rs) now reads each connection
  on its own thread and keeps tiny_http only for response serialization;
  `site_serves_every_concurrent_keep_alive_connection` reproduces the stall
  against tiny_http under CPU load. The desktop landing preview still uses
  tiny_http (see [OUTSTANDING_ISSUES.md](../../OUTSTANDING_ISSUES.md)).
- Long tail: `workspace_files_scroll_within_sidebar_at_both_window_sizes` opens
  20 editors one after another. Many `desktop::git::tests` take longer than 60 s
  under full parallel load, which is expected and not a hang.
- Parallel linking of all `gui-tests` targets can exhaust the Windows page file
  (OS error 1455) on memory-constrained machines. Prebuild with
  `cargo test --locked --features gui-tests --all-targets --no-run -j 2`, then run.

## Evidence

- Serial `gui-tests` lib: about 26 min. Parallel lib: about 490–575 s.
- After the fixes, `cargo test --locked --features gui-tests --all-targets
  --no-fail-fast` passed three consecutive times on Windows with default threads
  (1345 lib passed each; 8–10.5 min wall time including builds).
- A recurring `CompletionCleanup ... OsErrorCode=32` warning (Windows sharing
  violation) appears in passing runs; it does not fail tests and is not yet
  investigated.
- Live `live_deployment_` Azure/Netlify preflights passed in parallel.
  Mutating Cloudflare/Netlify publish tests were not run.
- `capture_workspaces_share_managed_chromium` is stale independent of threading;
  see [OUTSTANDING_ISSUES.md](../../OUTSTANDING_ISSUES.md).
