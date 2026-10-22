# Regressify Website

The public site for Regressify: a minimal landing page, a download page, and room to grow with docs and a blog. It is a static [Astro](https://astro.build) site with no UI framework. Its look follows the desktop app's theme, and every page works without JavaScript.

## Develop

Use the Bun version in [.bun-version](.bun-version). From this directory:

```sh
bun install --frozen-lockfile
bun run dev
```

| Script            | Purpose                                                   |
| ----------------- | --------------------------------------------------------- |
| `bun run dev`     | Start the dev server at `http://localhost:4321`.          |
| `bun run check`   | Type-check with `astro check` and run the Bun unit tests. |
| `bun run build`   | Build the static site into `dist/`.                       |
| `bun run preview` | Serve the built `dist/` locally.                          |

## Layout

| Path                        | Contents                                                          |
| --------------------------- | ----------------------------------------------------------------- |
| `src/config/site.ts`        | Site name, description, repository URL, and navigation.           |
| `src/styles/tokens.css`     | Design tokens mirrored from the desktop theme (light and dark).   |
| `src/styles/global.css`     | Base element styles and Markdown `.prose` styles.                 |
| `src/components/`           | Button, Card, CodeBlock, Header, Footer, ThemeToggle, and others. |
| `src/layouts/`              | `BaseLayout`, `DocsLayout`, and `BlogLayout`.                     |
| `src/pages/`                | Routes: `/`, `/download/`, `/docs/`, `/blog/`, `/rss.xml`, 404.   |
| `src/content/docs`, `/blog` | Markdown content collections.                                     |
| `src/lib/release.ts`        | Build-time installer resolution and validation.                   |
| `public/`                   | Logo and favicons copied from [assets](../assets).                |

## Add Docs and Blog Posts

Add a Markdown (`.md`) file to `src/content/docs/` or `src/content/blog/`. MDX is not enabled; `.mdx` files are not loaded. The file path becomes the URL: `src/content/docs/guides/suites.md` is served at `/docs/guides/suites/`. Files whose names start with `_` are ignored.

Docs front matter:

```yaml
title: Create a test suite
description: One sentence used for the page lead and search results.
order: 20        # Sidebar position, lowest first (default 100).
updated: 2026-10-22  # Optional.
draft: false     # Optional; drafts appear only in `bun run dev`.
```

Blog front matter:

```yaml
title: Regressify 1.3
description: One sentence for the post list, RSS, and search results.
date: 2026-11-01
author: Optional name
draft: false
```

The schemas live in [src/content.config.ts](src/content.config.ts); a build fails on invalid front matter. When migrating files from [docs](../docs), replace Zola shortcodes such as `{% spoiler() %}` with plain Markdown or `<details>`, and replace `@/...` links with site paths. Do not hard-code repository URLs in content; `SITE.repoUrl` controls every repository link.

The getting-started doc, the landing page quickstart, and the download page's build-from-source steps are copies of [docs/get-started.md](../docs/get-started.md), [docs/commands.md](../docs/commands.md), and [docs/desktop.md](../docs/desktop.md). Update them when those sources change. The desktop app guide (`src/content/docs/desktop-app.md`) is a user-facing adaptation of [docs/desktop.md](../docs/desktop.md); update it when desktop workflows, labels, or shortcuts change.

### Desktop Screenshots

Screenshots in the desktop guide live in [src/assets/docs/desktop](src/assets/docs/desktop) and are referenced from Markdown with relative paths, so Astro converts them to WebP with fixed dimensions. Readers can click any image to open it full screen ([ImageZoom](src/components/ImageZoom.astro)). They are dark-theme window captures (1440x900 before cropping) of a release build (`cargo build --release --features gui --bin regressify-desktop`) opened on a throwaway fictional workspace ("northwind-qa", served from `127.0.0.1`).

Annotations are baked into the PNGs: an amber ring around each control the text mentions and a numbered badge matching the step's circled number (①, ②, …). Amber keeps them distinct from the app's magenta diff pixels and red failure states. Ring positions came from each control's Windows UI Automation bounding box, looked up by accessible name, so they line up with the real controls. When you change a step's numbering, recapture or re-annotate the image so the badges still match.

To recapture:

- Use a disposable workspace and site, never a real project or customer URL.
- Snapshot the per-user desktop folder (`%APPDATA%\Regressify\Regressify Desktop`) first so recent-workspace paths do not appear and landing-app or target drafts cannot leak into your real settings; restore it exactly afterwards, including removing the run snapshot and instance lock the app writes for the throwaway workspace.
- Cancel every confirmation (Run Reference, Approve, Delete old runs, Save target, landing Save). Never deploy, push, or install updates while capturing.
- Replace any absolute path that reveals a user name before committing. The Git view, the Run Reference confirmation, and the cleanup dialog show one.
- Don't capture the Software updates dialog from a local build; it says updates are unavailable, which official installs don't.
- Keep each image's alt text describing what the screenshot shows and which controls are numbered.

## Download Resolution

Installers, binaries, and checksum sidecars live in versioned GitHub Releases of
`precise-alloy/regressify-rs`; older releases remain available. This site hosts
`/downloads/latest.txt`, selecting the version supported by its deployment,
plus `/install.ps1`, `/install.sh`, and `/mcp`. Release payloads are never committed;
`website/public/downloads` must not exist.

The download buttons and the archive table are resolved once at build time by [src/lib/release.ts](src/lib/release.ts):

1. Release deployments use `RELEASE_MANIFEST_FILE` and `RELEASE_REQUIRED=1`. Other builds request `<origin>/downloads/latest.txt` once, with redirects refused and a 10-second timeout. The default origin is `https://regressify.eshn.dev`; `PUBLIC_DOWNLOAD_ORIGIN` overrides the feed origin, not the trusted binary repository.
1. Validate the version, exact `https://github.com/precise-alloy/regressify-rs/releases/download/<tag>/Regressify-Setup-<version>-x64.exe` URL, matching tag, SHA-256, size (1 byte to 1 GiB), and Windows payload metadata. Reject ambiguous fields. Keep the updater contract aligned.
1. When valid, the buttons link straight to the installer and show its version and size; `/download/` also shows the file name and SHA-256.
1. `archive.<filename>=<size>:<sha256>` entries supply the archive tables from the same validated response, including normal hosted-feed builds. Group CLI and desktop downloads by Windows x64, macOS (Intel/Apple silicon), and Linux x64 glibc. Musl and Linux ARM64 releases are not available; the terminal installer rejects them before network access or writes. Every link pins the release tag, never a mutable latest redirect. OS-aware homepage buttons use the Windows setup or the appropriate platform section, with an all-platform fallback.
1. When the feed is unreachable, unpublished (HTTP 404), or invalid, the build still succeeds with a warning and the buttons link to `/download/`, which explains how to build from source. Only HTTP 404 is described as "not published yet".

Only validated values reach the page, as escaped text; manifest text is never rendered as HTML.

| Variable                       | Effect                                                                                          |
| ------------------------------ | ----------------------------------------------------------------------------------------------- |
| `PUBLIC_DOWNLOAD_ORIGIN`       | Site origin hosting `/downloads/` (default `https://regressify.eshn.dev`).                      |
| `RELEASE_REQUIRED=1`           | Fail the build unless a valid release is found.                                                 |
| `RELEASE_OFFLINE=1`            | Skip the request and render the fallback.                                                       |
| `RELEASE_MANIFEST_FILE=<path>` | Validate a local manifest instead, to preview the release state.                                |
| `SITE_URL`                     | Production origin for canonical, sitemap, and RSS URLs (default `https://regressify.eshn.dev`). |

## Deploy

Tagged releases wait for all parallel binaries, offline checks, and bootstrapper
packaging. A complete draft GitHub Release is uploaded and verified before
publication. The separate deploy job checks all 34 published assets again before
building this site (see the [release pipeline](../docs/desktop.md#release-pipeline)).
[ci/deploy-cloudflare.sh](../ci/deploy-cloudflare.sh) builds from the verified
manifest, copies only `latest.txt` into `dist/downloads/`, generates the MCP Worker
into ignored `mcp-worker/` with the released CLI, verifies it, and deploys with
[wrangler.jsonc](wrangler.jsonc).

That configuration deploys one Worker, `regressify`, on the custom domain `regressify.eshn.dev` with `workers.dev` and preview URLs disabled. `dist/` is served as static assets with `404.html` for unknown paths, and `run_worker_first: ["/mcp"]` sends only `/mcp` to the MCP handler. Keep its `compatibility_date` and `compatibility_flags` in sync with [the MCP Worker template](../src/mcp/worker/wrangler.jsonc.template); `python ci/test-release.py` checks this. Deployment needs `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN`, and the `eshn.dev` zone must belong to that account.

Do not deploy without a verified release manifest: a deployment replaces site
assets, including the update feed. GitHub binaries are not deleted by site deploys.

## Terminal Installation

The homepage and download page offer
`irm https://regressify.eshn.dev/install.ps1 | iex` and
`curl -fsSL https://regressify.eshn.dev/install.sh | sh`. Windows verifies and runs
the bootstrapper; macOS/Linux select the target, verify all selected ZIPs, and
install the CLI and desktop (where supported), licenses, and managed Chromium.
The scripts do not remove quarantine attributes, bypass execution policies, or
guarantee suppression of unsigned application warnings. In-app updates remain
Windows-only. Refer to [installation details](../docs/desktop.md#windows-installation-and-updates).

## Open Items

- Third-party notice packaging remains tracked in [outstanding issues](../OUTSTANDING_ISSUES.md).
- Until the first tagged release is deployed, local builds show the unpublished fallback.
- Pull requests do not run `bun run check` and `bun run build` yet; release deployment runs them, so run them by hand before merging site changes.
