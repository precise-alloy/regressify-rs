---
title: Change Log
description: Track changes to the Optimizely CMS testing guides, actions, scenarios, and suite options so you can spot new capabilities quickly.
guid: 1679e8d7-c710-4a1f-9640-224277efcd7e
date: 2023-09-06T17:00:00+00:00
updated: 2026-10-26T00:00:00+00:00
draft: false
hidden: false
weight: 41000
sort_by: weight
template: docs/page.html

extra:
  toc: true
  top: false
---

This page records notable updates to the testing docs, which is much faster than reading Git history with unnecessary archaeology.

## 2026-10-23 (Unreleased)

- Added: opt-in [image simplification](commands.md#image-simplification).
  `imageSimplification: pixelate` replaces large images with uniform blocks of
  their average color just before each screenshot and `save` checkpoint, so
  photo-heavy screenshots shrink while text and layout stay sharp. Images with
  text or badges drawn on top, excluded images, and scaled images stay sharp.
  The default `none` leaves captures unchanged. Turning it on or changing its
  settings changes pixels: capture new references, keep reference and test
  settings identical, and keep `pixelMatchThreshold` above `0`. Each capture
  records its setting, `ref` and `approve` keep it with the promoted image, and
  comparisons warn, without failing, when reference and test settings differ.

## 2026-10-20 (Unreleased)

- Fixed: snapshot reports behind Cloudflare Access loaded the page but failed to
  load report data with a browser CORS error. Report data requests now send the
  site's own sign-in cookie. Regenerate and redeploy existing reports with the
  updated build; no recapture is needed. Landing-page status for Access-protected
  reports still needs a Bypass for `/_snapshot/status.json`; see
  [Landing Apps](desktop.md#landing-apps).

## 2026-10-12 (Unreleased)

- Fixed: intermittent `CSS override or browser ready script failed ... (Unknown)`
  after page navigation in 0.88. DOM readiness and ready hooks now use the current
  browser document instead of a destroyed execution context. Existing suites do
  not need extra waits or changes to their ready scripts for this engine error.

## 2026-10-05 (0.75.0, Unreleased)

- Added: [Per-action viewport selection](create-action.md#viewport-selection)
  for all 16 actions, including inherited prerequisites and filtered screenshot
  checkpoints. The minimum supporting CLI and desktop version is **0.75.0**.
- Runtime support and the derived JSON Schema are part of the same release.
  Upgrade CLI, desktop, and generated authoring schemas together; older binaries
  must not be assumed to ignore action-level `viewportNames` safely.
- Downgrade compatibility: pre-feature builds with strict action parsing fail
  before browser startup when reading this property. A 0.72.0 bundled engine
  reports `data did not match any variant of untagged enum TestAction`, with
  suite and source-location context. The concrete action's `deny_unknown_fields`
  rejection is wrapped by the untagged enum, so a literal `unknown field` message
  is not guaranteed. There is no automatic downgrade migration.

## 2025-01-13

- Added: Action - [goto](@/optimizely-cms/testing/create-action.md#spoiler--goto)

## 2025-01-10

- Added: Test suite - [state](@/optimizely-cms/testing/create-test-suite.md#spoiler--state)
- Added: Action - [persist](@/optimizely-cms/testing/create-action.md#spoiler--persist)
- Added: Replacement Profile - regex
- Added: Replacement Profile - flags

## 2024-12-04

- Added: Test suite - [ignoreSslErrors](@/optimizely-cms/testing/create-test-suite.md#spoiler--ignoresslerrors)
- Added: Test suite - [viewportNames](@/optimizely-cms/testing/create-test-suite.md#spoiler--viewportnames)
- Updated: Scenario - [viewportNames](@/optimizely-cms/testing/create-scenario.md#spoiler--viewportnames) - Allow both `string` and `string[]`

## 2023-09-13

- Added: Action - [frame](@/optimizely-cms/testing/create-action.md#spoiler--frame)

## 2023-09-05

- Added: Scenario - [misMatchThreshold](@/optimizely-cms/testing/create-scenario.md#spoiler--mismatchthreshold)
- Added: Scenario - [postInteractionWait](@/optimizely-cms/testing/create-scenario.md#spoiler--postinteractionwait)
- Added: Test suite - [asyncCaptureLimit](@/optimizely-cms/testing/create-test-suite.md#spoiler--asynccapturelimit)
- Added: Test suite - [asyncCompareLimit](@/optimizely-cms/testing/create-test-suite.md#spoiler--asynccomparelimit)
- Added: Test suite - [browser](@/optimizely-cms/testing/create-test-suite.md#spoiler--browser)
- Added: Test suite - [misMatchThreshold](@/optimizely-cms/testing/create-test-suite.md#spoiler--mismatchthreshold)
- Added: Test suite - [postInteractionWait](@/optimizely-cms/testing/create-test-suite.md#spoiler--postinteractionwait)

## 2023-08-30

- Added: Scenario - [noScrollTop](@/optimizely-cms/testing/create-scenario.md#spoiler--noscrolltop)
- Added: Scenario - [id](@/optimizely-cms/testing/create-scenario.md#spoiler--id)
- Added: Scenario - [needs](@/optimizely-cms/testing/create-scenario.md#spoiler--needs)

## 2023-08-29

- Added: Test suite - [debug](@/optimizely-cms/testing/create-test-suite.md#spoiler--debug)
- Updated: Action - [wait](@/optimizely-cms/testing/create-action.md#spoiler--wait) - Allow specify the URL for the navigation
