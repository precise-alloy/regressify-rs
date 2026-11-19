# MCP Authoring

Verified: 2026-11-19

- Live verification on 2026-10-23: `https://regressify.eshn.dev/mcp` served
  `regressify-authoring` v2.0.8. The pinned SDK read all 3 tools, 9 guides,
  6 schemas, 46 examples, 15 resources, and the `author_regressify` prompt.
  Protocol `2026-07-28` discovery/list/tool calls also passed. Invalid tool input,
  untrusted origins (403), oversized bodies (413), and malformed JSON (400) were
  rejected. A plain GET returns 405 by design; use an MCP HTTP client.
- The CLI no longer exports Workers. The feature-gated
  [catalog builder](../../src/bin/mcp_catalog.rs) uses Rust types and guides
  without reading a consumer workspace; [the website](../../website/mcp) owns
  the Worker. Deployments match the catalog to the published release manifest.
  [Worker delivery tests](../../website/mcp/tests/worker.test.mjs) therefore
  compare with the supplied catalog; test new wording in the catalog builder.
- [The catalog](../../src/mcp/catalog.rs) derives six schemas from real Rust
  types, embeds nine guides (including `recipes`), and includes synthetic
  examples. The overview lists every example task name by category, and the
  hooks and troubleshooting topics cover script contracts, local evidence, and
  safe repair. Tests check real parsers, local schema references, property help,
  action/example coverage, negative/boundary constraints, preflight action
  rules, and every guide YAML fence as a complete file.
- Runtime facts verified for the guides: `wait` with `url` matches the URL within
  `elementTimeout` first, then sleeps or waits for the selector; action selectors
  use the bundled Playwright engines (including `:visible` and `>> visible=true`
  to avoid hidden responsive duplicates matched first in DOM order), while
  hide/remove and screenshot selectors are plain CSS (`save.selector` also runs
  inside `save.frame` when `frame` is set); `select`/`check`/`uncheck` require
  native HTML controls (use `click` for custom combobox/switch widgets); captures
  and `report.json` share `<output>/<suite>/bitmaps_test/<run>/`.
- Startup, overview, actions and [workspace guidance](../../src/init.rs) prefer
  stable standard CSS on both environments. Keep action selector lookup in
  [the runtime](../../src/test/runtime.rs) distinct from native document lookup:
  the `css_only_selectors_in_` Chromium fixtures cover frame clips and
  `hide`/`remove` under strict CSP in the main document and a cross-origin frame,
  even after the page replaces its adopted theme sheets. Configured masks use
  the [main-document inline CSS hook](../../src/test/hooks.rs), not action rules.
- Suite preflight (`validate_action_fields`, `validate_action_files`, and
  screenshot selector prefixes) rejects schema-valid but unusable actions before
  browser launch; the actions guide lists them under "Checks Beyond the Schema".
- Element captures are pixel clips of the target's border box after centering
  it: overflowing menus, drawers, and body-level panels are cut off, while
  overlays drawn inside the box are included. `viewport` is reserved in
  `selector`, `selectors`, and `save.selector`. Verified 2026-10-06 with a
  cached-Chromium probe and the selector pixel fixture; QA reported agents
  capturing `header` after opening menus.
- Related closed/default and opened/expanded states now share viewport context
  captures in the navigation example and recipe. A header-only initial image
  can miss premature expansion outside its box; do not force-close the initial
  state. Startup/overview guidance recommends the same larger viewport/full-page
  area, matching viewport/scroll position, and supplementary element details.
  Catalog/Worker regressions cover delivery and matching recipe/example targets.
- Page-level failure markers, `engineErrorMsg`, and progress errors carry a typed
  summary (step, condition, duration, category); the console adds a sanitized
  local detail line that is never archived. Journals keep `ActionIndex`,
  `Condition`, and stage records. Source selectors are readable metadata in
  current logs; do not embed secrets in them. Older logs can use `sha256:`
  identities; see the [logging note](logging-diagnostics.md).
- New guide topics are discovered through the overview served by the same Worker
  version. Keep CLI-managed `AGENTS.md` tool/topic references compatible with
  older hosted Workers; the consumer memory policy adds no tools or guide topics.
- [Startup instructions](../../src/mcp/catalog.rs) and the
  [overview](../../src/mcp/guides/overview.md) teach agents to read and maintain the
  consumer project's `.ai/memory/` with client tools. Notes are at most 60 lines,
  one behavior or issue on one site per file, with a compact linked index. Record
  verified lessons, failed approaches, fixes, and evidence; split/update notes and
  exclude private data. The server never reads or stores memory. Catalog and
  Worker regressions cover startup, tool/resource, and non-overview prompt delivery.
- Startup instructions and the overview require agents to run the installed
  `regressify ref --test-suite <suite-name>` after every consumer suite edit or
  creation. CLI and desktop are installed together on the user's PATH. Catalog
  and Worker regressions cover fresh intended screenshots, not exit code zero or
  old references, plus reference-write authorization and incomplete verification
  when blocked. [Reference execution](../../src/test/test_suites.rs) preserves
  old references on capture failure and can exit zero after page failures.
- Hook examples and a companion suite use classic browser scripts at `.js` target
  paths. [Worker tests](../../website/mcp/tests/worker.test.mjs) evaluate
  the catalog scripts with DOM stubs; this is not browser coverage or a capture
  dependency. Browser-only lifecycle order and timeout sources are explained in
  [the hooks guide](../../src/mcp/guides/hooks.md).
- The overview and hooks guide explicitly prohibit CommonJS `module.exports`
  and Playwright `page` callbacks; `.cjs` does not select a module loader, and
  renaming the file does not convert its contents. Navigation belongs in suite
  actions; metadata comes from `window.regressify`. The existing
  [Chromium hook fixture](../../src/test/hooks.rs) verifies missing `module` and
  `page` globals in before, shared-ready, and scenario-ready scripts. Catalog
  regressions require the warnings and migration guidance in the exported text.
- Standalone checks permit consumer `AGENTS.md` references but reject source-only
  dependencies. Managed MCP/agent initialization and legacy text-append shortcuts
  are documented; explicit action lists suppress legacy shortcuts.
- [Viewport schema metadata](../../src/config/viewports.rs) is available in the
  default CLI feature set, not just desktop. Cookies retain runtime-validated
  prose/examples instead of a separately maintained typed schema.
- [The Worker](../../website/mcp/src/index.ts) uses the pinned Cloudflare
  stateless MCP handler, with read-only tools/resources/prompts, Origin checks,
  bounded bodies, and no bindings. Public guidance is not authorization to read
  local files, execute captures, or approve baselines.
- [Protocol tests](../../website/mcp/tests/worker.test.mjs) cover SDK legacy
  compatibility and current HTTP metadata. Invalid tool arguments are tool-error
  results; stateless tests provide `MCP-Protocol-Version` and `Mcp-Method` headers.
  Stateless `tools/call` also needs `Mcp-Name` matching `params.name`; a live probe
  omitting it received HTTP 400/code -32020, then passed with the header present.
- [CI](../../.github/workflows/mcp-checks.yml) builds a catalog from source and
  runs npm installation, typechecking, local MCP tests, and a Wrangler dry run.
  No publishing is implied.
- Wrangler 4.147 passes `upload_source_maps` to esbuild as `sourcemap`, and an
  unset value means `true`: maps were written to `dist` (not uploaded) and the
  bundle was unminified. The Worker config sets `minify: true` and
  `upload_source_maps: false`; a package test checks the dry-run output.
- MCP deployment is part of the website deployment; there is no standalone
  publishing shortcut. The MCP package retains exact dependency pins and an npm
  lockfile. Run and report the npm audit separately.
- See [the task skill](../../.github/skills/mcp-command/SKILL.md) for maintenance
  gates. Keep schema/runtime changes synchronized with the self-contained prose.
