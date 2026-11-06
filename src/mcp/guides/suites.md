# Test Suites

A suite is a YAML or JSON object containing a required `scenarios` array. Each
scenario requires a `url`. Place suites under `visual_tests/` in the workspace.
Use `<name>.tests.yaml`, `<name>.tests.yml`, or `<name>.tests.json`; nested folders
are allowed. Do not author duplicate files for the same suite name. Discovery
prefers YAML over YML over JSON, and same-priority duplicate names are ambiguous,
including duplicates in different subdirectories.

The suite name is the file name before `.tests.`, lowercased. Folders are not
part of it: `visual_tests/checkout/Flow.tests.yaml` is suite `flow`. Capture
commands select suites by this name; `--test-suite` accepts exact names or glob
patterns such as `'shop*'`, may be repeated, and `all`, `*`, or omission select
every suite. A selection that matches nothing fails with
`No matching test suites found`.

```yaml
displayName: Customer checkout
scenarios:
  - id: cart
    label: Shopping cart
    url: https://example.com/cart
  - id: checkout
    label: Checkout form
    url: https://example.com/checkout
```

For a file named `visual_tests/shop.tests.yaml`, use `--test-suite shop`.
`displayName` changes only presentation in the desktop app and reports. It does
not change CLI filters, filenames, output directories, or report identity.
Suite `id` defaults to the lowercase suite name and names the output folder
`<output>/<id>/`; `compare`, `approve`, and `snapshot` select suites by this
output ID. Avoid setting `id` without a specific identity requirement, because
changing it moves baselines. Leave `testDate` unset; it is runtime metadata.

## Configuration Scope

Use `get_schema(kind="suite")` for every supported field, type, description,
and constraint. Do not copy fields from the workspace or scenario schema unless
they also exist at suite scope. Shared capture settings generally inherit from
the workspace and may be overridden by scenarios where supported. Lists such
as `hideSelectors`, `removeSelectors`, and `viewportNames` replace inherited
lists. An explicit empty `viewportNames` selects all configured viewports.

Important exceptions are described in the workspace guide: URL replacement
lists concatenate, Basic Auth credentials override by origin, and suite
`replacementProfile` selects a profile from `common/_replacement-profiles.yaml`
(ignoring `default` unless overridden by `--replacement-profile` or
`REPLACEMENT_PROFILE`). `delay`, timeouts, and other workspace/scenario-only
settings are not suite fields.

```yaml
viewportNames: [phone, desktop]
misMatchThreshold: 0.1
pixelMatchThreshold: 5
scenarios:
  - id: homepage
    url: https://example.com
```

The viewport labels must be defined in the effective viewport file. By default,
that is `common/_viewports.yaml`. `misMatchThreshold` is the allowed percentage of
different pixels; `pixelMatchThreshold` is a separate color sensitivity setting.
Both use the scale 0 through 100, not fractions from 0 through 1.

## Identity and Execution

- Prefer explicit, readable scenario IDs and labels. Generated scenario IDs use
  labels or URLs. IDs are case-insensitively unique within a suite.
- Suite/scenario IDs must be portable single path components, at most 80 UTF-8
  bytes. Do not use slashes, traversal, leading/trailing whitespace, trailing
  dots, control characters, or Windows device names such as `CON` or `LPT1`.
- Changing IDs or filenames can change screenshot and reference identity. Do not
  rename them as a presentation-only edit; use `displayName` or `label` instead.
- Scenarios run independently and may run concurrently, up to
  `asyncCaptureLimit` per suite and `--async-capture-limit` overall (default 5
  each). Do not rely on list order or a previous scenario's cookies, DOM, or
  browser session. Lower the limits when the site rate-limits parallel visits.
- `needs` names scenarios in this same suite and replays their actions inside
  the consuming scenario. It is not scheduling. Consult the scenarios guide.
- All scenario/viewport/checkpoint/selector screenshot names must be unique,
  including case-insensitive collisions. Regressify checks this before capture.

Use `get_example(task="minimal-suite")`, `minimal-suite-json`,
`responsive-checkout`, `navigation-menus`, or `reuse-actions` for complete
synthetic suite files, and `get_authoring_guide(topic="recipes")` for common
page patterns.
Keep only one format for a given suite. A file with a different unrecognized
extension is not a discovered test suite.
