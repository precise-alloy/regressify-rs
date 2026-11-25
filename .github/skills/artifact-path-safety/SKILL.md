---
name: artifact-path-safety
description: 'Use when changing workspace/output paths, run IDs, state names, symlink checks, atomic file replacement, or report artifact access.'
---

# Artifact Path Safety

## Start Here

[Path/config resolution](../../../src/config/mod.rs),
[portable CLI IDs](../../../src/cli/args.rs), [atomic I/O](../../../src/helpers/io.rs),
and [desktop workspace paths](../../../src/desktop/workspace.rs).

## Procedure and Invariants

1. Separate user-selected output roots from logical IDs/relative artifact paths.
   A custom output root may be outside the workspace; a capture/run/state name
   must not become an arbitrary path or silently select a different root.
1. Reuse the owning boundary's checks: components, canonical containment, regular
   file/directory type, symlink policy, and Windows reserved/trailing characters.
   String prefixes and lexical normalization alone are insufficient. Use the
   checked canonical path for source identity and companion files. Preserve casing,
   not DOS aliases, in published names. Check existing output files and directories
   before treating a differing spelling as a case-only rename.
1. Handle missing optional files separately from I/O errors. Revalidate at write,
   copy, delete, package, and open boundaries rather than trusting earlier UI input.
1. Use same-directory temporary writes/copies. Preserve a valid destination on
   failure and clean temporary artifacts; atomic replacement is not a transaction
   or conflict detector for multiple concurrent writers.

## Checks

```sh
cargo test --lib helpers::io::tests
cargo test --lib cli::args::tests
cargo test --features desktop --lib desktop::workspace::tests
```

Cover parent/absolute paths, symlink escape, missing parents, regular-file checks,
Windows device names, permission failures, repeat writes, and external conflicts.
State platform-specific coverage explicitly when symlink tests cannot run.

## Maintain

Update the owning command/desktop docs and this skill when path contracts change.
Comment why a boundary permits or rejects links; do not claim race-free writes
unless handle/locking behavior actually establishes that guarantee.
