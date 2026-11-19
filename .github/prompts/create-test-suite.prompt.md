---
agent: agent
description: 'Create or modify visual test suites using the maintained schema and suite-authoring skill.'
---

# Create/Modify Test Suite

When creating or modifying test suites in `visual_tests/`:

1. Read [AGENTS.md](../../AGENTS.md) and the [suite skill](../skills/test-suites/SKILL.md).
1. Always include a `label` for each scenario (auto-generated IDs are cryptic)
1. Add `wait` actions after clicks that trigger navigation
1. Use `hide` for non-deterministic content (timestamps, ads, random data)
1. Use `needs` for scenario dependencies instead of relying on execution order
1. Set appropriate `misMatchThreshold` (default 0.1 is very strict)

## File Naming

```text
visual_tests/{descriptive-name}.tests.yaml
```

## Validation

After creating/modifying, ensure:

- All scenarios have unique IDs (auto-generated from label if not set)
- URLs are syntactically valid; contact pages only when authorized for this task
- Prefer stable standard CSS; verify the intended match on both reference and
  test pages at the configured viewports. Use Playwright-only locators as a
  fallback in supported action, wait, readiness, or frame fields, never for
  screenshot or hide/remove targets.
- `frame` selectors target actual `<iframe>` elements
- File paths in `input.file` are relative to `visual_tests/`
- Run the skill's relevant checks and update docs/examples when behavior changes.
