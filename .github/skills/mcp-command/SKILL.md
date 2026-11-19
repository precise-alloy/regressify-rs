---
name: mcp-command
description: 'Use when changing hosted MCP authoring or troubleshooting guides, generated schemas/examples, the website MCP Worker, catalog versioning, or protocol compatibility.'
---

# MCP Authoring Export

## Start Here

[Catalog](../../../src/mcp/catalog.rs), [guides](../../../src/mcp/guides), [website Worker](../../../website/mcp), [command reference](../../../docs/commands.md#mcp-authoring-worker), and [CI](../../workflows/mcp-checks.yml).

## Procedure and Invariants

1. The CLI has no MCP generation command. The feature-gated repository-only catalog binary derives content from Rust types; it must not read workspace content, fetch dependencies, start Chromium, or deploy. The official endpoint (https://regressify.eshn.dev/mcp, REGRESSIFY_MCP_URL in init) runs inside the website Worker.
1. Preserve release-matched catalog generation and the legacy fallback for releases predating the catalog binary. Match catalog versions to the release manifest before deployment; see the [release pipeline](../release-pipeline/SKILL.md). Assert required wording in Rust catalog tests; Worker delivery tests compare responses to the supplied catalog, which can predate `master`. Keep website and MCP test compatibility settings aligned.
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
   fields, and diagnostic record fields have drifted before. Keep startup,
   overview, actions and managed workspace instructions CSS-first; distinguish
   action-only fallbacks from CSS-only screenshot and stylesheet targets. Every ```yaml fence
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
1. Preserve both current stateless and legacy-client tests. Current HTTP requests need protocol/method metadata headers as well as body metadata. Stateless `tools/call` also requires `Mcp-Name` matching `params.name`; missing or mismatched metadata is a client error. Invalid tool arguments may return `isError: true` rather than throw at the client. A plain GET returning 405 is not an MCP health failure; verify with an MCP HTTP client.
1. Pin website MCP dependencies and keep the npm lockfile synchronized. Typecheck and exercise the real Worker using a freshly generated catalog; test the combined website Worker too. Dependency advisories and audit-service failures must be reported, not suppressed or force-fixed.
1. Keep `minify: true` and `upload_source_maps: false` in both Wrangler configurations. The MCP test inspects the dry-run bundle; the website build must also pass before deployment.

## Checks

```sh
cargo test --locked --lib mcp
cargo test --locked --features desktop --lib desktop::schema::tests
cargo fmt --all -- --check
cargo run --locked --features mcp-catalog --bin regressify-mcp-catalog > website/mcp/content/catalog.json
```

Create `website/mcp/content` first. In `website/mcp`, with Node 22.12+ and npm:

```sh
npm ci
npm run check
npm test
npm run build
```

Worker tests start/stop a local runtime; `build` is a Wrangler dry run. Neither requires publishing credentials. Cover version and dependency consistency, self-contained schema refs, and every action/example. Run `npm audit --audit-level=high` and report advisories or lookup failures explicitly.
Catalog checks also require direct/referenced property help and standalone
diagnostic/hook contracts. Script-stub checks do not replace live hook fixtures.
Check consumer memory guidance through startup instructions, the overview tool
and resource, and a non-overview prompt, including the file-size/topic limits.
Check required suite-scoped `ref` verification through those same surfaces,
including the installed CLI/PATH, intended captures, reference writes, and blockers.

## Maintain

Update [CLI docs](../../../docs/commands.md), [README](../../../README.md), [website README](../../../website/README.md), affected domain skills, and [shared notes](../../../.ai/memory/mcp-authoring.md). Keep MCP CI triggered by types, runtime semantics, and Worker changes.
