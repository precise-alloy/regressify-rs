---
title: Getting started
description: Install Regressify, create your first test suite, and run your first visual check.
sidebar:
  order: 1
# The next page is the Desktop app group's Overview; its sidebar label alone
# would not say which overview the card leads to.
next:
  label: Desktop app overview
lastUpdated: 2026-10-22
---

This guide takes you from an empty folder to a reviewed visual check. It is adapted from the repository's
`docs/get-started.md` and `docs/commands.md` documents.

## Install

On Windows x64, run the installer from the [download page](/download/). It installs the desktop app and the
`regressify` CLI for your user, adds them to your PATH, and prepares the managed Chromium build. Open a new terminal
after setup to run `regressify`.

On other platforms, or to build it yourself, install the stable [Rust toolchain](https://www.rust-lang.org/tools/install)
and run this from a checkout of the repository:

```sh
cargo install --path .
```

Regressify supports Chromium only. A missing pinned Chromium build is downloaded automatically into a per-user cache shared by
every workspace, the CLI, and the desktop app. Installed Chrome and Edge are not used. To prepare the cache ahead of
offline runs:

```sh
regressify install
```

## Create a workspace

Create a folder for your tests and initialize it:

```sh
regressify init
```

This creates `regressify.yaml`, `common/` files, a starter `visual_tests/alloy.tests.yaml`, and VS Code schema
integration. Missing starter files are created; an existing `regressify.yaml` or `regressify.yml` and existing suites
are kept.

`init` also changes two files in an existing project: it adds or refreshes the `regressify-mcp` server entry in
`.vscode/mcp.json`, keeping your other settings, and it creates or updates a managed `## Regressify CMP` section in
the workspace's `AGENTS.md`. See "Workspace Initialization" in `docs/commands.md` for details.

If you use the desktop app instead, open the folder there. Missing runtime files, including `regressify.yaml`, are
created automatically.

## Write your first suite

Each suite lives in the `visual_tests` folder and follows the `<name>.tests.yaml` naming convention. A suite lists the
pages to capture:

```yaml
scenarios:
  - url: https://alloy.eshn.dev/pages/home.html
  - url: https://alloy.eshn.dev/pages/service.html
  - url: https://alloy.eshn.dev/pages/contact/default.html
```

Scenarios can also run actions such as `click`, `input`, `hover`, and `scroll` before the screenshot, and can target
specific viewports. Settings resolve from the scenario, then the suite, then the workspace, then built-in defaults.

## Capture references

Take the baseline screenshots:

```sh
regressify ref --test-suite alloy
```

`ref` promotes only successful captures to references. If a page fails, the failure is written into the report and any
existing reference is left in place.

## Test for visual changes

After a deployment, capture again and compare against the references:

```sh
regressify test --test-suite alloy
```

Results are written under `.regressify/<suite>/` and summarized in the snapshot report, which offers side-by-side,
wipe, overlay, and diff views. In the desktop app, use the Results view instead.

Website and visual problems, such as timeouts, failed actions, and mismatches, are recorded in the report and the
command still exits with zero. Configuration and infrastructure problems return a non-zero exit code, so CI can tell
them apart.

## Approve intended changes

When the differences are expected, promote the latest successful screenshots to references:

```sh
regressify approve --test-suite alloy
```

In the desktop app, use **Approve This Scenario** on the result. If the differences are bugs, report them, wait for a
fix, and run `regressify test` again.

Repeat the test and approve steps after every deployment to keep the site visually in check.
