---
name: mcp-command
description: 'Use when changing regressify mcp, standalone authoring or troubleshooting guides, generated schemas/examples, Cloudflare MCP Worker templates, protocol compatibility, or safe export destinations.'
---

# MCP Authoring Export

## Start Here

[Generator](../../../src/mcp/mod.rs), [catalog](../../../src/mcp/catalog.rs), [guides](../../../src/mcp/guides), [Worker template](../../../src/mcp/worker), [command reference](../../../docs/commands.md#mcp-authoring-worker), and [CI](../../workflows/mcp-checks.yml).

## Procedure and Invariants

1. Keep `regressify mcp --output <new-directory>` in the default CLI feature set. Embed all templates/guides with the binary; generation must not read workspace content, fetch dependencies, start Chromium, or require a source checkout.
1. Separate generation from publication. No Cloudflare API calls, credentials, automatic installs, or deployment subprocesses belong in the Rust command. The official endpoint (https://regressify.eshn.dev/mcp, REGRESSIFY_MCP_URL in init) is deployed by the [release pipeline](../release-pipeline/SKILL.md) inside the website Worker; template changes to compatibility settings must also update website/wrangler.jsonc.
1. Require an existing parent and a new output directory. Reserve it with `create_dir`, validate embedded paths/directories, and use no-clobber temporary file publication. Preserve existing files and competing writes. A failed export may remain partial; do not recursively delete a directory another process could have edited. Do not claim protection from every ancestor race.
1. Derive schemas from real Rust types. Keep viewport schema metadata available without desktop features. Cookie format guidance uses the real runtime parser in tests instead of introducing a second cookie schema.
   Cover local constraints negatively as well as valid examples: positive workspace
   timeouts and nonempty capture selectors must not reject documented zero
   fallbacks or empty viewport selections. Shared string/list help must be neutral.
1. Keep guides self-contained and versioned. Explain runtime exceptions, units, inheritance, paths, safety, complete-file versus fragment examples, and valid repair strategies. No required repository links, website shortcodes, or development-only agent rules may leak into the exported authoring instructions.
   Consumer files such as `AGENTS.md` are legitimate guidance, not source leaks.
   Startup instructions, the overview, and prompts must teach agents to read and
   maintain consumer `.ai/memory/` using authorized client tools. Each topic note
   is at most 60 lines and covers one behavior or issue on one site; keep a compact
   linked index, record verified lessons/failed approaches/evidence, and correct
   stale advice. Exclude secrets and raw logs; never upload memory to the Worker.
   Keep CLI/desktop managed-file behavior current. After each consumer suite edit
   or creation, require `regressify ref --test-suite <suite-name>` for each changed
   suite using authorized client terminal tools. The CLI and desktop app are
   installed together on each user's PATH; no source checkout or build is needed.
   Require all intended screenshots in fresh results/PNGs, not just exit code zero
   or old references. Preserve target-page and baseline-write authorization;
   report blocked verification as incomplete. Cover this in startup instructions,
   the overview, and non-overview prompts.
   The troubleshooting topic must
   give local evidence paths, focused commands, failure/health interpretation,
   and safe repairs without asking clients to upload logs or private data.
   Verify guide and doc-comment claims against runtime source, not user docs:
   selector engines, wait/URL order, lifecycle timing, artifact paths, report
   fields, and diagnostic record fields have drifted before. Every ```yaml fence
   in a guide must be a complete workspace, suite, viewport, or profile file with
   explicit scenario IDs; catalog tests parse and schema-check each one.
1. Every action needs a typed synthetic example. Update examples and prose for semantic changes; schema generation alone does not validate dependencies, paths, credentials, capture identity, or live browser behavior.
   The hooks topic must match injected browser scripts, page-visible context,
   timeout source, and pre-action ready timing. The overview and hooks topic must
   warn against CommonJS `module.exports` and Playwright callbacks; `.cjs` does
   not enable a module loader. Migrate navigation and interaction callbacks to
   suite actions, not fake browser globals or Node dependencies. Test exported classic scripts
   with DOM stubs and keep companion suite paths consistent; Node-based Worker
   development tests are not capture prerequisites. Distinguish legacy
   text-appending `keyPress` shortcuts from real `press` keyboard events.
   Related component states with changing visible areas should share a larger
   viewport/full-page capture, including the untouched default state; focused
   details are supplementary. Keep startup, overview, scenarios, recipes, and
   the typed navigation example consistent. Catalog and Worker regressions
   cover delivery and guard against header-only default examples.
1. The Worker serves public read-only tools, resources, and prompts at `/mcp`. Use the pinned SDK handler for discovery/transport. Retain Origin validation, request byte limits, no arbitrary path/URL inputs, no user data storage, and generic failure responses. Authentication is a separate deployment concern.
1. Preserve both current stateless and legacy-client tests. Current HTTP requests need protocol/method metadata headers as well as body metadata. Invalid tool arguments may return `isError: true` rather than throw at the client.
1. Pin template dependencies and retain a matching npm lockfile. Use `--save-exact` for upgrades: caret/tilde ranges fail the export contract even when the manifest and lockfile agree. Keep manifest pins, lockfile root declarations, and resolved direct versions synchronized. Edit the template/catalog, regenerate a fresh package, then test that actual export; do not rely only on a hand-adjusted temporary package. The frontend log-value allowlist must match new typed command identities. Embedded code/config uses `.template` filenames; only exported files are a runnable TypeScript project with a generated catalog and installed dependencies.
1. Keep `minify: true` and `upload_source_maps: false` in the Wrangler template so a deployment uploads one minified module and no source maps; Wrangler generates maps whenever uploads are unset. The package test inspects a dry-run bundle; only the worker's imports, such as `content/catalog.json`, enter it.
1. Gate generated `deploy` through `verify`: high/critical dependency audit (including dev tools), typecheck, tests, and dry-run build, all fail-closed. Root `deploy:check` covers Rust formatting/MCP tests/locked CLI build; root deploy exports current Cargo source without Git pulls or installed-CLI lookup. Never run its sibling-directory cleanup or real publication as validation. Exercise each failed gate with mocked npm/Wrangler; audit failures are blockers, not permission to force upgrades.

## Checks

```sh
cargo test --locked --lib mcp
cargo test --locked --features desktop --lib desktop::schema::tests
cargo fmt --all -- --check
cargo build --locked --bin regressify
```

Run the built binary from an empty temporary directory and export to a new path. In that package, with Node 22.12+ and npm:

```sh
npm ci
npm run check
npm test
npm run build
npm run verify
```

Worker tests start/stop a local runtime; `build` is a Wrangler dry run. Neither requires publishing credentials. Cover complete/default-feature export, version and dependency consistency, deterministic bytes, self-contained schema refs, every action/example, invalid destinations, file conflicts, concurrent exports, and symlinks where the platform permits. Unix symlink checks run in Linux CI.
`verify` also needs the npm audit service; report dependency advisories or lookup failures explicitly. Only `deploy` publishes after all gates succeed.
Catalog checks also require direct/referenced property help and standalone
diagnostic/hook contracts. Script-stub checks do not replace live hook fixtures.
Check consumer memory guidance through startup instructions, the overview tool
and resource, and a non-overview prompt, including the file-size/topic limits.
Check required suite-scoped `ref` verification through those same surfaces,
including the installed CLI/PATH, intended captures, reference writes, and blockers.

## Maintain

Update [CLI docs](../../../docs/commands.md), [README](../../../README.md), the template's standalone README, affected domain skills, and [shared notes](../../../.ai/memory/mcp-authoring.md). Keep the generated-project CI triggered by types, runtime semantics, and templates.
