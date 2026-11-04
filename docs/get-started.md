---
title: Get Started With Regressify Testing
nav: Get Started
description: Get started with Regressify for Optimizely CMS by installing the tools, creating your first suite, and running visual checks.
guid: 6f40e57f-eb81-4589-9599-b2838c21c07f
date: 2023-06-29T17:00:00+00:00
updated: 2026-04-24T17:00:00+00:00
draft: false
hidden: false
weight: 10000
sort_by: weight
template: docs/page.html

extra:
   lead: This guide walks through the first Regressify setup for Optimizely CMS, from local prerequisites and one-time build steps to creating a simple suite and running your first visual checks. It is the quickest path from zero to a working baseline.
  toc: true
  top: false
---

If this is your first Regressify project, work through this page in order once. After that, you can come back only for the parts you need.

## Prerequisites

{% danger() %}

Please follow the instructions on this page exactly. Testers sometimes use other tools or IDEs such as Visual Studio or IntelliJ IDEA. Those alternatives may work, but advanced features like auto-suggest, linting, and error detection will often be missing, which makes test cases easier to break and harder to support.

{% end %}

Before you begin, make sure you have the following programs installed on your system:

- [Rust][rust] - latest stable toolchain
- Network access for the first managed Chromium download. All workspaces share a per-user browser cache; `regressify install` can populate it ahead of offline runs. Installed Chrome and Edge are not used.
- [Git][git]
- [Visual Studio Code][vs-code] - Latest version

Additionally, ensure you have the following Visual Studio Code extensions installed:

- [Path Intellisense][path-intellisense] - Christian Kohler
- [Regressify](https://marketplace.visualstudio.com/items?itemName=tuyen.regressify) - Tuyen Pham
- [YAML][yaml] - Red Hat  
  *After installation, go to Settings --> YAML Store --> Disable.*

## Initial Setup

If this is your first time working with **Regressify**, follow these steps (only required once). Please note that the process may take 10 minutes or more depending on your internet connection.

1. *(Microsoft Windows only)* Ensure that you have enabled [script execution](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.security/set-executionpolicy) in PowerShell by running the following command in Admin mode:

   ```powershell
   Set-ExecutionPolicy -ExecutionPolicy RemoteSigned
   ```

1. Build the CLI from the repository:

   ```bash wrap
   cargo build --release
   ```

   The binary is created at `target/release/regressify`. You can run it directly or install it into your Cargo bin directory:

   ```bash wrap
   cargo install --path .
   ```

## Installation

1. Create a new folder for the regression test.

1. Navigate into the newly created folder and initialize starter files:

   ```bash wrap
   regressify init
   ```

   This creates `regressify.yaml`, `common/` files, a starter
   `visual_tests/alloy.tests.yaml`, and VS Code schema integration without
   overwriting existing files. An existing `regressify.yml` is preserved instead
   of creating a competing configuration file.

   It also adds `regressify-mcp` to `.vscode/mcp.json`, or refreshes its URL to the
   bundled endpoint if the server already exists. Other servers, input definitions,
   server settings, and JSONC comments are preserved. See
   [workspace initialization](commands.md#workspace-initialization) for details.

   A managed `## Regressify CMP` section in `AGENTS.md` tells agents to use this
   MCP server. Initialization creates the file or section if missing and refreshes
   outdated instructions through the next H2 or the end of the file. Other
   sections and already-current instructions are preserved.

   Agents also read and maintain site lessons in your project's `.ai/memory/`:
   one specific behavior or issue on one site per file, at most 60 lines, with a
   compact linked index. Notes retain verified fixes, failed approaches, and
   evidence without secrets or raw logs. Initialization leaves memory files alone.

   In the [desktop app](desktop.md#workspace-and-files), opening the folder
   automatically creates missing runtime files, including `regressify.yaml`,
   and performs the same MCP and `AGENTS.md` maintenance. No separate
   initialization step is needed. Schema exports, other VS Code settings, and
   an example suite are not added automatically.

1. (Optionally) Initialize git in your folder:  
   Ensure you have [Git](https://git-scm.com/downloads) installed locally.

   ```bash wrap
   git init
   git add .
   git commit -m "Initial commit"
   ```

## Creating your first test suite

1. Create a test suite.

   Create a test suite in the [**visual_tests**](https://github.com/precise-alloy/regression-test/tree/master/visual_tests) folder. Each test suite should adhere to the naming convention: **\<name\>.tests.yaml**.  
   For instance, you can create a file named **[visual_tests/alloy.tests.yaml][alloy-test-suite]** with the following content:

   ```yaml
   scenarios:
     - url: https://alloy.eshn.dev/pages/home.html
     - url: https://alloy.eshn.dev/pages/service.html
     - url: https://alloy.eshn.dev/pages/portfolio.html
     - url: https://alloy.eshn.dev/pages/contact/default.html
   ```

   For more information on available options within a test suite, refer to the [Create a test suite][create-test-suite] and [Create a scenario][create-scenario] documentation.

1. Take a snapshot of the website.

   To capture a snapshot of the website at the current time, use the following command:

   ```bash wrap
   regressify ref --test-suite alloy
   ```

   The `ref` command captures screenshots and promotes successful captures as references. It does not open a browser report. If a page scenario fails, that failure is written into the report output and existing reference screenshots are left in place.

1. Test for visual changes.

   Suppose the development team has just deployed new code to the site, and you want to ensure that nothing is breaking visually or that all visual changes are as intended. Use the following command to perform the test:

   ```bash wrap
   regressify test --test-suite alloy
   ```

   After the command finishes, comparison data is written under `.regressify/<suite>/bitmaps_test/<run>/`. While `alloy` is the only suite, the command also regenerates the summary report; once other suites exist, a run that skips any of them leaves the report unchanged, so run `regressify snapshot` to refresh it. The command does not force-open a browser window.

   If the test results appear to be correct, run the `approve` command again as mentioned above to set the new reference. However, if any issues are identified, inform the development team so that they can fix the problems and then re-run the testing step.

1. Run regression testing after a new deployment.

   Whenever there is a new deployment, you can re-execute the `test` and `approve` it again to perform regression testing on the site visually.

[rust]: https://www.rust-lang.org/tools/install
[git]: https://git-scm.com/downloads
[vs-code]: https://code.visualstudio.com/
[yaml]: https://marketplace.visualstudio.com/items?itemName=redhat.vscode-yaml
[path-intellisense]: https://marketplace.visualstudio.com/items?itemName=christian-kohler.path-intellisense
[create-test-suite]: @/optimizely-cms/testing/create-test-suite.md
[create-scenario]: @/optimizely-cms/testing/create-scenario.md
[alloy-test-suite]: https://github.com/precise-alloy/regression-test/blob/master/visual_tests/alloy.tests.yaml
