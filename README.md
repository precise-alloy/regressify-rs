# Regressify

Regressify is a visual regression testing tool built in Rust, available as both a standalone CLI (`regressify`) and a native desktop workbench (`regressify-desktop`).

Both interfaces share the same Rust engine: they launch a managed Chromium build directly through the Chrome DevTools Protocol (`chromiumoxide`—no Node.js or Playwright runtime required), execute declarative YAML/JSON test suites, compare captured screenshots against reference baselines, and generate self-contained interactive HTML reports.

## Install

Download prebuilt releases from <https://regressify.eshn.dev/download/> or install from a terminal:

- **Windows (Desktop + CLI):**

  ```powershell
  irm https://regressify.eshn.dev/install.ps1 | iex
  ```

- **macOS / Linux (CLI):**

  ```sh
  curl -fsSL https://regressify.eshn.dev/install.sh | sh
  ```

These commands execute an install script from the site after SHA-256 verification; unsigned application warnings may still apply. On Windows, setup installs `regressify-desktop.exe` and `regressify.exe` into `%LOCALAPPDATA%\Programs\Regressify`, adds that directory to the user `PATH`, registers **Open with Regressify Desktop** in Explorer context menus, and prepares the pinned Chromium build in `%LOCALAPPDATA%\regressify\chromium` (shared by CLI and Desktop). Official Windows builds also check for updates in the background and support in-app installation.

## Desktop App (`regressify-desktop`)

The Rust-native [GPUI Kit](https://gpui-kit.com) workbench lets QA and developers author, run, review, version-control, and publish visual regression suites without leaving the app:

- **Guided & Source Authoring:** Edit workspace configuration (`regressify.yaml`), suites, scenarios, all action types, and viewports through structured **Fields** forms or syntax-highlighted **Source** editors (YAML, JSON, JavaScript) with JSON Schema validation, hover documentation, autocompletion, inheritance previews (`scenario -> suite -> workspace -> defaults`), comment-preserving saves, external-conflict detection, and undo/redo.
- **Bundled Worker Execution:** Runs `ref`, `test`, `compare`, `approve`, `snapshot`, `optimize`, `clean`, `install`, and `init` in isolated background worker processes with no separate CLI binary required. Select multiple suites with checkboxes and track scenarios live through `queued`, `running`, `captured`, `comparing`, `passed`, `failed`, and `cancelled` states.
- **Visual Results & Log Review:** Inspect reference, current, and diff images in side-by-side, wipe, overlay, or diff modes; approve updated baselines in place per viewport; and inspect structured capture and command diagnostic logs.
- **Integrated Git Review:** Track repository status and remote ahead/behind counts from the title bar; review side-by-side text diffs; stage, commit, or discard selected files; and manage remotes (`fetch`, fast-forward `pull`, `push`). Project changes and `.regressify*` **Comparison Changes** are separated into distinct trees so baseline/report artifacts are never committed accidentally.
- **Report & Landing App Publishing:** Deploy generated snapshot reports and multi-site customer **Landing Apps** (with live `/_snapshot/status.json` pass-rate badges) directly to **Azure Static Web Apps**, **Netlify**, or **Cloudflare Workers Static Assets**, using credentials stored in the OS keyring via reusable **Deployment Tokens**.
- **Workspace Management:** Enforces single-instance window ownership per workspace with activation handoff, supports Windows taskbar **Recent Workspaces** Jump Lists, and initializes missing workspace files automatically on open.

Build or launch from source:

```bash
cargo build --features gui --bin regressify-desktop
cargo run --features gui --bin regressify-desktop -- /path/to/test-workspace
```

Omit the workspace argument to use the native folder picker. For distribution, build with `cargo build --release --features gui --bin regressify-desktop`. See [the desktop guide](docs/desktop.md) for workflows, shortcuts, safety limits, and test commands, and [the delivery plan](docs/desktop-plan.md) for feature scope and verification evidence.

## CLI (`regressify`)

The default Cargo build produces the standalone CLI binary (`regressify`).

| Command    | Purpose                                                                                                                                                                                          |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `init`     | Create missing starter files, schemas, editor associations, and `.ai/memory` guidance; configure the Regressify MCP endpoint without overwriting user data.                                      |
| `install`  | Fetch the pinned Chromium build into the shared per-user cache.                                                                                                                                  |
| `ref`      | Capture baseline screenshots and promote successful captures to references while preserving existing baselines on capture errors. Whole-workspace runs also regenerate the HTML snapshot report. |
| `test`     | Capture screenshots for selected suites/scenarios and compare against references (unless `--skip-compare` is set). Compared whole-workspace runs also regenerate the HTML snapshot report.       |
| `compare`  | Re-run image comparisons against reference screenshots for the latest completed run or an explicit `--run-id`.                                                                                   |
| `approve`  | Promote successful test screenshots from the latest completed run (or `--run-id`) to references, optionally scoped by `--scenario`.                                                              |
| `snapshot` | Generate the static HTML snapshot SPA from existing comparison reports, with optional `--test-suite` glob filters.                                                                               |
| `optimize` | Losslessly or lossily compress PNG files across the project output tree.                                                                                                                         |
| `clean`    | Preview and delete old completed test runs and unreferenced logs (`--keep-runs`, `--yes`) while protecting active baselines.                                                                     |
| `version`  | Print the CLI version and Git commit hash.                                                                                                                                                       |

Key engine capabilities shared by CLI and Desktop:

- **Browser Automation:** Actionability-waited `click`, `hover`, `input`, `press`, `key`, `select`, `check`, `uncheck`, `focus`, `scroll`, `dragAndDrop`, `upload`, `wait`, `navigate`, and `switchTab` actions across the main document, nested/cross-origin iframes, and popup tabs.
- **Session & Lifecycle Control:** `persist` and `restore` browser storage state (cookies and origin-scoped `localStorage`), HTTP Basic Auth, URL replacement profiles, CSS overrides, and lifecycle hooks (`onBeforeScript`, `onReadyScript`, `jsOnReadyPath`).
- **Capture Stability & Run Retention:** Multi-viewport expansion, lazy-load settling, animation/caret freezing, selector clipping, per-suite `runs.json` completion tracking, opt-in automatic run retention (`autoCleanRuns`, `keepTestRuns`), and bounded `_diagnostics` journals.

See [docs/commands.md](docs/commands.md) for global flags (`--root`, `--site-env`, `--run-id`, `--async-capture-limit`, `--debug`, `--log-level`), output directories, and exit-code semantics, and [docs/get-started.md](docs/get-started.md) for authoring your first suite.

## Build and Validate

Build and install the CLI from source:

```bash
cargo build --release
cargo run -- --help
cargo install --path .
```

Cargo features:

- `local` (default): CLI binary and core engine.
- `cloud`: Cloud integration support.
- `desktop`: Headless desktop workspace, editor, Git, and deployment models.
- `gui`: Native GPUI Kit desktop application (`regressify-desktop`).
- `gui-tests`: Headless and native GPUI test support.

Run CLI and model validation gates:

```bash
cargo fmt --all -- --check
cargo check
cargo test --lib
cargo test --features desktop --lib
```

Run full GUI and lint gates:

```bash
cargo test --features gui-tests --all-targets
cargo clippy --features gui --lib -- -D warnings
```

On an interactive Windows desktop, run [the native smoke test](ci/desktop-smoke.ps1)
to verify real windows, screenshot rendering, and GUI-to-CLI execution. See [docs/commands.md](docs/commands.md) for all CLI options.

## MCP Authoring Guidance

The hosted, read-only MCP service at <https://regressify.eshn.dev/mcp> provides versioned authoring guides, schemas, and synthetic examples. `regressify init` configures the endpoint for VS Code. See [docs/commands.md](docs/commands.md#mcp-authoring-worker) for details.

## Snapshot Frontend and Website

- **Snapshot SPA ([frontend](frontend/README.md)):** React + Vite + TypeScript app for interactive comparison reports and static landing pages. Run `bun run build` in `frontend/` to regenerate the embedded assets in `src/snapshot/data` (ordinary Cargo builds do not require JavaScript tooling).
- **Public Website ([website](website/README.md)):** Astro site serving <https://regressify.eshn.dev> (downloads, documentation, blog, and private deployment admin view).
- **Release Pipeline:** Version tags build all supported targets, publish verified GitHub Release assets and the Windows bootstrapper, and deploy the website and `/mcp` Worker to Cloudflare. See the [release guide](docs/desktop.md#release-pipeline).

## Contributors

- Author and lead developer: [Tuyen Pham](https://github.com/tuyen-at-work)
- Developer and docs maintainer: [Dinh Pham](https://github.com/dinh-pham-optimizely)
- Developers: [Hung Le](https://github.com/lehung1109), [Dung Nguyen Tien](https://github.com/dungnthe130669)
- Advisors (QA): Nga Nguyen, Hanh Nguyen, Duyen Pham — use the tool, provide feedback, and report bugs to help improve Regressify

## License

Copyright (C) 2023-2026 Tuyen Pham. Regressify is free software under the
[GNU Affero General Public License v3.0 or later](LICENSE) and comes with
ABSOLUTELY NO WARRANTY.

An additional permission lets you use Regressify output, such as generated
reports, historical `regressify mcp` exports, `regressify init` files, and documentation
samples, under terms of your choice. See [LICENSING.md](LICENSING.md) for its
exact scope and for third-party components that keep their own licenses.

Rust dependencies must use licenses allowed by [deny.toml](deny.toml). Check them
with `cargo deny check licenses` (requires `cargo-deny`).
