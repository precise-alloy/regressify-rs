# Logging Diagnostics

Verified: 2026-10-15

- [Diagnostic records](../../src/diagnostics/record.rs) are versioned separately
  from live progress. Public capture records use templates, typed fields, readable
  source identities, and categorical errors; provider detail is deployment-only.
- `SafeIdentity::source` keeps suite/scenario IDs, viewport names, selectors,
  source URLs, artifact paths, and capture names readable. Simulated free-text
  payloads such as `input.value` remain omitted; selectors and ordinary source
  query values are not classified as user input. URL credentials/token parameters
  are redacted, controls neutralized, and identities bounded to 2048 UTF-8 bytes.
  Legacy SHA-256 identities remain accepted; integrity hashes are unchanged.
- The [CLI logging fixture](../../tests/desktop_worker.rs) verifies readable
  archived/published selectors and source names while scanning for simulated
  input and URL credentials. The [responsive fixture](../../src/test/browser_tests.rs)
  verifies every execution-owned record; journal-level finish records can omit
  execution identity. Native and web formatters include searchable source names.
- [Native log paging](../../src/desktop/logs.rs) keeps one aligned 500-record
  page and clamps large/filter-shrunk offsets in one streaming pass. `log_pagination_`
  covers 0/1/500/501 matches, partial archives and checked page conversion.
  [Run logs](../../src/desktop/ui/runs/log.rs) hide numbering during Follow;
  disabling it loads the last aligned page. A cleared archive key permits a new
  read during loading, while key/generation checks reject stale completion.
- [Run polling](../../src/desktop/ui/runs.rs) leaves capture execution in the
  worker process and polls progress nonblockingly every 100 ms. Hidden capture
  log editors previously rebuilt up to 2 MiB per changed tick; they now wait for
  Log activation. UI/log updates coalesce over 500 ms while queues still drain
  every 100 ms; pending updates survive unchanged ticks and terminal updates
  bypass throttling. Unchanged ticks redraw only for elapsed-second changes.
  `run_log_hidden_` and `run_poll_` cover these UI-work bounds; they do not measure
  native input latency or establish a frame-time budget for large Activity lists.
- [Capture Log](../../src/desktop/ui/reports/log.rs) uses segmented source tabs
  with two public focus handles and Enter/Space hooks because Kit 0.7.3 tabs are
  pointer-only. Real-handle GUI tests and native Right/Enter, Left/Space checks
  pass; source changes reset page one and retain generation-based rejection.
- Capture failure summaries (`FailureSite` in the record module, tracked by
  `OperationContext::track` in the [runtime](../../src/diagnostics/runtime.rs))
  use only typed record fields and the category, so markers, `engineErrorMsg`,
  and progress errors stay publishable. The first failed record wins each field;
  enclosing failures fill the action. Field-less wrapper records are skipped
  because their zero durations would replace the step's. Scenario deadlines
  read the unfinished-stage stack, since cancelled stages record no completion.
- The sanitized error chain is printed only by `log_capture_failure` in
  [test_suites](../../src/test/test_suites.rs) (console and live desktop log,
  hidden by `--no-print`); never put it in journals, markers, or reports.
  `ErrorCategory::classify` treats serde phrases as `InvalidData`, matches
  `timeout` as a word, and keeps a trailing `(Category)` from a summary.
- [Startup journals](../../src/diagnostics/startup.rs) are entry-point-owned in
  local application data, use the bounded operation queue for native warnings,
  finalize explicitly, and are excluded from public reports. The desktop installs
  its subscriber before parsing/worker dispatch; a nested worker reuses ownership.
- [Engine commands](../../src/application.rs) own separate local journals except
  version output; deployment staging retains its enclosing deployment context.
  [Desktop Activity](../../src/desktop/logs.rs) reopens saved command operation IDs,
  while image Capture Log preserves original capture provenance. Quiet-mode engine
  subprocess tests cover success/failure; desktop tests cover exact reload/export.
- Outcome severity is shared by runtime/startup and journal finalization. Rust and
  frontend schemas reject timestamps outside JavaScript's date range; the browser
  regression verifies loading-error recovery on desktop/mobile. Raw ordinary
  tracing events remain intentionally excluded from the reviewed-record adapter.
- The user confirmed a 10% mean wall-time budget for the documented five-pair
  capture workload. Final measured archive overhead, including startup lifecycle,
  was 7.48%; see the [benchmark](../../docs/logging-benchmark.md) for raw data and
  the archive-kill-switch baseline and I/O-accounting limitations. That measurement
  predates the October 2 command/application ownership follow-up; it is not a
  remeasurement of the newer code.
- [Runtime stage wrappers](../../src/diagnostics/runtime.rs) box their work before
  constructing the wrapper future. Leaving nested action/hook futures inline
  overflowed a default Windows Tokio worker stack, even on the closed-semaphore
  capture path. Boxing restored the passing regression without larger stacks.
- The owning regression is
  `cargo test --lib capture_job_journal_opens_writes_and_finalizes_before_comparing`.
  It invokes the real spawn path in an isolated subprocess, preserving global
  configuration/subscriber isolation and requiring no browser.
- [Journal ownership](../../src/diagnostics/health.rs) keeps warnings separate
  from capture/result errors. A completed capture can have a partial or missing
  archive; finalization and checksum verification determine completeness.
- Browser validation for the action/hook instrumentation used existing loopback
  fixtures with `prepare_browser(Some(false))`: no downloads or personal profiles.
- [Capture log links](../../src/test/capture.rs) bind to image SHA-256 digests.
  `ref` and `approve` preserve the original archive reference after replacing an
  image; [lossless PNG optimization](../../src/optimizer/image_optimizer.rs)
  refreshes only sidecars matching the pre-optimization bytes so tamper detection
  remains intact. Scoped scenario/job run logs exclude the outer command journal,
  and `LogPage` retains execution metadata when filtered to terminal records.
- Links also carry each capture's [image simplification](image-simplification.md)
  setting. `test` finalizes suites in `spawn_blocking` without the task-local
  command context, so its comparison records never reach a journal; `compare`
  records them (probe 2026-10-26; see the outstanding issues).
