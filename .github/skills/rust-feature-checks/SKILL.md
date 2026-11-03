---
name: rust-feature-checks
description: 'Use when choosing Rust validation gates, changing Cargo features/dependencies, verifying CLI versus desktop/GUI coverage, or updating CI test commands.'
---

# Rust Feature and Test Gates

## Start Here

[Cargo features](../../../Cargo.toml), [library gates](../../../src/lib.rs),
[desktop CI](../../workflows/desktop-checks.yml), and
[worker integration target](../../../tests/desktop_worker.rs).

## Procedure

1. Run the owning skill's smallest behavior test first. Confirm a nonzero matching
   test count; skipped/ignored tests are not passing coverage.
1. Default `local` is the CLI slice. `desktop` adds headless models, `gui` adds the
   native binary, and `gui-tests` enables toolkit test support. `cloud` is separate.
1. After Rust edits run `cargo fmt --all -- --check`, then the affected gates below.
   Do not run an all-features build as a substitute for supported feature boundaries.
1. Read ignored-test prerequisites before enabling live Chromium cases. Never
   silently install browsers, contact production websites, or change credentials.
1. All gates, including `gui-tests`, run with the default parallel test threads.
   Keep tests isolated rather than serializing them: use temporary directories
   and injected paths (`Workbench::new` preferences path, `*_in` path helpers),
   never real user settings. Tests that set process-global `OnceLock`s re-run
   `current_exe()` with `--exact` in a child process. GPUI `wait_for` timeouts
   use simulated time, so wait on observable completion, not wall-clock delays.

## Current CI Gates

Windows runs:

```sh
cargo test --locked --all-targets
cargo test --locked --features gui-tests --all-targets
cargo clippy --locked --features gui --lib -- -D warnings
cargo build --locked --features gui --bin regressify-desktop
```

Linux headless models run:

```sh
cargo test --locked --features desktop --lib
```

Cargo manifest, lockfile, or [deny.toml](../../../deny.toml) changes run the
[license gate](../../workflows/license-checks.yml). Every allowed license must stay
AGPL-3.0-or-later compatible; review compatibility before adding one. Locally,
install `cargo-deny` (for example under `target/tools`) and run:

```sh
cargo deny --locked check licenses
```

Native smoke additionally requires an unlocked Windows desktop and isolated app
state. Installer/release checks require their own prerequisites; ordinary unit
tests do not validate visible setup, shell integration, or actual update installation.

## Maintain

Update this skill and [AGENTS.md](../../../AGENTS.md) when features or commands
change; update CI filters/jobs and user build docs together. Report exact commands,
outcomes, skipped tests, and missing prerequisites without claiming broader coverage.
