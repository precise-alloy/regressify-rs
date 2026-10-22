# Workspace Initialization

Verified: 2026-10-15

- [Initialization](../../src/init.rs) shares runtime defaults between CLI `init`
  and desktop setup: `regressify.yaml`, five `common/` files, and `visual_tests/`.
  The workspace config starts as `{}` so it inherits runtime defaults. Existing
  workspace and replacement-profile `.yml` files suppress generated `.yaml`
  defaults. Existing file contents, including invalid drafts, are preserved.
- [Desktop documents](../../src/desktop/document.rs) also accept blank/comment-only
  workspace YAML as defaults while preserving source for save and undo. They use
  the CLI parser to distinguish omitted content from explicit nulls; do not use
  global null normalization or assume tab indentation is valid YAML whitespace.
  `desktop::document::tests::blank_workspace_yaml` covers normalization, first-field
  edits, resetting the last field, and rejection boundaries.
- [Session connection](../../src/desktop/session.rs) initializes before listing
  entries and only replaces the current session after setup succeeds. The
  [UI loader](../../src/desktop/ui/mod.rs) invokes it in a background task after
  acquiring workspace ownership and checking unsaved work. `Workspace::open`
  remains read-only because activation/ownership probes also call it.
- Automatic desktop setup creates no example suites, exported schemas, VS Code
  settings/extensions, Node helpers, or run/state artifacts. It does maintain
  MCP configuration and agent guidance using the same helpers as CLI `init`.
  Explicit CLI `init` retains its sample, other editor assets, and legacy migration.
  See the [desktop workflow](../../docs/desktop.md#workspace-and-files).
- CLI and desktop setup merge `.vscode/mcp.json`: add a missing HTTP `regressify-mcp`
  server or refresh only its URL from `REGRESSIFY_MCP_URL`, preserving headers,
  other servers/settings, comments, and trailing commas. Matching URLs do not
  rewrite the file. Malformed or unwritable configurations fail unchanged.
  [Initialization](../../src/init.rs) uses `jsonc-parser` features `cst` and
  `serde_json` for CST conversion; `serde` alone does not expose `to_serde_value`.
  Parse options explicitly exclude syntax beyond VS Code JSONC.
- CLI and desktop setup manage `AGENTS.md` under the exact H2 `Regressify CMP`.
  They create, append, or refresh this section, including the current MCP endpoint and public
  guide/schema/example tool names. `pulldown-cmark` offsets delimit top-level H2
  sections through the next H2 or EOF; parsed-event equality skips current content
  despite ATX/Setext, list-marker, or line-ending differences. Outside bytes stay
  unchanged. Duplicate managed sections collapse to one; unfinished blocks that
  would swallow a new heading fail without rewriting the file. `init::tests`
  covers these boundaries alongside read-only, non-UTF-8, and concurrent creation.
- The [generated agent section](../../src/init.rs) aligns with the MCP overview:
  it teaches agents that hooks are classic browser JavaScript (never Node.js or
  CommonJS), requires shared larger `viewport`/full-page context captures for
  related component states, requires running `regressify ref --test-suite <suite-name>`
  from the installed CLI on `PATH` and verifying fresh results/PNGs after suite
  edits, and teaches the 60-line single-site/topic `.ai/memory/` workflow.
  Initialization does not create or rewrite memory files. The starter-file
  regression checks this policy.
- Local tag `v0.79.0` wired MCP setup only into explicit CLI init, not desktop
  folder opening. The desktop wiring fix also includes managed agent guidance;
  installed older binaries require a new build. Session tests cover create,
  restore, refresh, preserved project data, and failed-setup session retention.
- [Workspace discovery](../../src/desktop/workspace.rs) lists all regular files
  under `common`, including `_on-ready.js`, `_override.css`, and nested/unknown
  formats. Only `visual_tests` entries become suites. `Entry::supports_editing`
  reflects bounded UTF-8 text detection, so JavaScript/CSS helpers are editable
  in Source while binary/unreadable/oversized files remain visible but disabled.
  Session tests validate only editable entries; `workspace_files_` GUI tests
  verify raw editing, mouse/keyboard guards, and both sizes and themes.
- Starter writes use same-directory temporary files with `persist_noclobber`;
  existing regular files are skipped and symlink/non-file destinations rejected.
  This prevents partial starter publication and clobbering competing files, not
  a transaction across the whole workspace or a guarantee against ancestor races.
  MCP and agent-guidance updates recheck the original bytes before atomic
  replacement; this is not compare-and-swap protection against external edits.
- Evidence: `cargo test --locked --features desktop --lib init::tests` and
  `cargo test --locked --features desktop --lib desktop::session::tests` cover
  the inventory, repeated/concurrent setup, missing defaults, `.yml` preservation,
  read-only files, failed opens, and draft guards. Symlink regressions are Unix-only.
- Windows path assertions must compare canonical paths consistently: joining a
  slash-containing relative string can retain mixed separators, while diagnostics
  use the canonical path. Avoid raw display-string comparisons between the two.
- [GUI fixtures](../../src/desktop/ui/tests.rs) connect through the same initializer,
  so their entry lists already contain default non-suite files. Suite creation
  checks the previous inventory plus the new file; sidebar-scroll tests count all
  non-suite entries and cover seven/eight/nine/twelve total rows. These focused
  `gui-tests` regressions pass without changing runtime discovery or layout.
- The managed MCP endpoint is `https://regressify.eshn.dev/mcp` (served by the website
  Worker). Init's URL refresh migrates the retired
  `regressify-mcp.tuyen-at-work.workers.dev` URL; see the [init regression](../../src/init.rs).
