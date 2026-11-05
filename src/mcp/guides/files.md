# Predefined and Generated Files

Paths below are relative to the user's Regressify workspace, not the MCP Worker
package or this service's source. CLI init and desktop folder opening preserve
existing runtime defaults and add missing ones. Both also create or refresh the
managed MCP configuration and agent section described below. Only explicit CLI
`regressify init` adds an example suite, exported schemas, and other VS Code
settings/extension recommendations; existing copies of those files are preserved.

## Managed Agent Files

- `.vscode/mcp.json`: adds `servers.regressify-mcp` when missing, or updates only
  its URL to the installed CLI's bundled endpoint. Other servers, fields, input
  definitions, JSONC comments, and trailing commas are preserved. A current URL
  is not rewritten. A custom URL under this managed server name is refreshed too.
- `AGENTS.md`: creates the file, appends a missing `## Regressify CMP` section,
  or updates outdated instructions through the next top-level H2 or EOF.
  Other sections remain unchanged. Markdown parsing ignores quoted/code examples
  and preserves already-current content despite equivalent heading/list syntax.

Do not place unrelated instructions inside the managed section; its entire H2
range is maintained. Duplicate managed sections are consolidated. Malformed MCP
JSONC, unsafe paths, unreadable files, or read-only files needing an update fail
setup without replacing the affected file. An unfinished Markdown block that
would swallow an appended heading also fails unchanged. Older installed versions
may lack automatic desktop setup; compare CLI and server versions when diagnosing.

## User-Authored Runtime Files

- `regressify.yaml` or `regressify.yml`: workspace YAML object; `{}` inherits
  defaults. An existing `.yml` is preserved by initialization.
- `visual_tests/<name>.tests.yaml`, `.tests.yml`, or `.tests.json`: suite object
  containing `scenarios`. Use one format per suite name.
- `common/_viewports.yaml`: top-level list of `{label, width, height}` objects.
  Default viewport file; override with `viewportsPath`.
- `common/_cookies.yaml`: top-level cookie list, initialized as `[]`. Override
  with `cookiePath`, including an explicit JSON file.
- `common/_replacement-profiles.yaml` or `.yml`: object with `profiles`,
  mapping names to ordered replacement-rule lists. Initialized as
  `profiles: {default: []}`.
- `common/_override.css`: browser CSS injected into every page document before
  actions and capture when `useCssOverride` is true; override with
  `cssOverridePath`. See `get_example(task="css-override")`.
- `common/_on-ready.js`: browser-side JavaScript run once per page document
  before actions; override with `jsOnReadyPath`. See
  `get_example(task="ready-script")`.

Configured paths must remain inside the canonical workspace and be regular
files. Explicit missing paths are errors. The default cookie/CSS/ready files are
optional when absent, but validated when present. A missing default viewport
file, or one containing an empty list, means one unnamed capture per scenario at
1920x1080 CSS pixels; an explicit missing viewport file is an error. Named
`viewportNames` filters are then ignored, and action filters must be empty.

## Viewport and Profile Formats

```yaml
- label: phone
  width: 375
  height: 667
- label: desktop
  width: 1366
  height: 768
```

Viewport dimensions are positive whole CSS-pixel values. Labels must be nonblank,
case-insensitively unique, and produce distinct screenshot suffixes of at most
48 bytes. Distinct text can normalize to the same suffix, so avoid punctuation
variants. `viewportNames` values select these labels, not device types or sizes.
A viewport sets only the CSS size at device scale factor 1: there is no mobile
user agent, touch emulation, or high-density rendering, so sites that choose
layouts by user agent show their desktop markup at phone widths.

```yaml
profiles:
  default: []
  staging:
    - find: "{HOST}"
      ref: https://example.com
      test: https://staging.example.com
    - ref: https://example.com
      test: https://staging.example.com
```

Select a profile with `--replacement-profile staging` (in `ref` or `test`),
`REPLACEMENT_PROFILE`, or suite `replacementProfile`. Each profile is processed
in order from first item to last item: URL items replace `find -> ref` in `ref`
mode and `find -> test` (or `ref -> test` when `find` is omitted) in `test`
mode. A `script` item, written as `- script: common/transform.cjs`, runs a script
from `common/` (`.ps1`, `.bat`/`.cmd`, `.sh`/`.bash`, or
`.js`/`.mjs`/`.cjs`/`.ts`/`.mts` via Bun or Node.js), passing the suite JSON on
`stdin` and reading the transformed suite JSON from `stdout`. Every item must
exist before a run starts, so create the script file together with the item; see
`get_example(task="profile-script")`. Scripts execute arbitrary code on the
user's machine, and a failing script's output is never shown in errors; debug it
by running it manually with the suite JSON on stdin. Profile steps run before
workspace rules and then suite rules. Use the matching schemas and the
`viewports` and `replacement-profiles` examples for complete values.

## Cookies

A cookie entry requires a nonempty string `name`, string `value`, and a nonblank
`url` or `domain`. `domain` may be a string or nonempty list; list entries expand
into separate cookies. Optional `path` and `sameSite` are strings; `secure` and
`httpOnly` are booleans; `expires` is a finite numeric Unix timestamp in seconds.
`sameSite` accepts Strict, Lax, or None, ignoring case; None requires secure true.
Additional browser cookie fields are allowed, unlike unknown typed config keys.

Name plus value must fit within Chromium's 4096-byte limit. Values must not contain
CR, LF, or NUL. Keep chunked authentication cookies separate; do not join, trim,
truncate, or rewrite their contents. Quote values in YAML; block `|-` avoids a
trailing newline when needed. Real cookies are secrets and must stay local.
The `cookies` example contains only a synthetic consent value. Cookie validation
uses the runtime parser, not an independently maintained JSON Schema here.

## CSS and JavaScript

CSS overrides are ordinary CSS, not YAML. Hide only approved nondeterministic
regions; do not conceal the behavior under test. `hideSelectors` and
`removeSelectors` are appended to this CSS as `visibility: hidden` and
`display: none` rules, even when `useCssOverride` is false. Ready JavaScript
executes in the page, with DOM APIs and `window.__REGRESSIFY__` /
`window.regressify` context. The context includes the scenario's identity, URL,
and `requiredLogin` flag. CSS/ready hooks apply once for each document,
including subsequent navigation, before the next action or capture. A script may
return a promise. Errors fail the capture; do not assume a failed script was
applied.

All browser hook scripts, `onBeforeScript`, `onReadyScript`, and `jsOnReadyPath`,
execute inside Chromium with no Node or Playwright dependency. Node filesystem
APIs, `require`, module exports, and callback arguments are unavailable there.
Replacement profile scripts are different: they run on the host with Bun or
Node.js.
Read `get_authoring_guide(topic="hooks")` for browser context, lifecycle, and
timeouts; both ready-script settings run before actions, after readiness.
Upload files referenced by `input.file` live under `visual_tests/` and
use paths relative to that folder. Never execute untrusted scripts as validation.

## Generated and Sensitive Files

`common/test-schema.json`, `common/regressify-schema.json`, and
`common/replacement-profiles-schema.json` are editor schemas generated by `init`.
They are not runtime configuration. Existing copies are not refreshed by `init`;
check CLI/schema versions before relying on an old copy. VS Code settings and
extension recommendations are editor integration, not suite fields.

The default output directory is `.regressify/`; `--site-env` selects
`.regressify__<environment>/`, and `--regressify-dir` overrides the output root.
These root options precede the command. Each suite writes
`<output>/<suite-id>/bitmaps_test/<run-id>/` for every capture run (including
`ref` runs) and `<output>/<suite-id>/bitmaps_reference/` for approved baselines;
the snapshot website is `<output>/index.html`. `states/<name>.json` files
contain sensitive browser state generated by `persist`. Do not hand-author,
publish, upload, or commit them. Reference/test PNGs, reports, manifests, and
diagnostic journals are generated output, not test configuration. Preserve
baselines until the user explicitly authorizes reference capture or approval.
Retrieve `get_authoring_guide(topic="troubleshooting")` for concrete report paths,
capture-error/log sidecars, operation journals, and missing-log interpretation.

Legacy `data/` files can be copied by CLI initialization; do not create new
workspaces in that layout. `_signing-in.yaml` is not a newly initialized default.
Do not invent a required login file or assume that `requiredLogin` signs in.
