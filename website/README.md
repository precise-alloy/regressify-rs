# Regressify Website

The public site for Regressify: a minimal landing page, downloads, docs, a blog,
and a private deployment admin view. Public pages are static [Astro](https://astro.build)
with no UI framework and work without JavaScript. The docs use
[Starlight](https://starlight.astro.build) for the Astro docs layout; its search
needs JavaScript. The admin view uses a Worker API and JavaScript. The site's look
follows the desktop app's theme.

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

| Path                        | Contents                                                                   |
| --------------------------- | -------------------------------------------------------------------------- |
| `src/config/site.ts`        | Site name, description, repository URL, and navigation.                    |
| `src/styles/tokens.css`     | Design tokens mirrored from the desktop theme (light and dark).            |
| `src/styles/global.css`     | Base element styles and Markdown `.prose` styles (not loaded on docs).     |
| `src/styles/starlight.css`  | Maps Starlight's `--sl-*` theme onto the tokens for the docs.              |
| `src/components/`           | Button, Card, CodeBlock, Header, Footer, ThemeToggle, and others.          |
| `src/components/starlight/` | Starlight overrides (head, header, theme, image zoom, mobile menu).        |
| `src/layouts/`              | `BaseLayout` and `BlogLayout`.                                             |
| `src/pages/`                | Routes: `/`, `/download/`, `/blog/`, `/about/`, `/rss.xml`, 404.           |
| `src/content/docs/docs/`    | Docs pages, routed by Starlight under `/docs/`.                            |
| `src/content/blog`          | Blog posts (Markdown).                                                     |
| `src/lib/release.ts`        | Build-time installer resolution and validation.                            |
| `public/`                   | Logo and favicons copied from [assets](../assets).                         |

Header navigation uses `HEADER_LINKS` in [src/config/site.ts](src/config/site.ts).
It includes shared `NAV_LINKS` plus a header-only Login link to `/admin/`, where
Cloudflare Access handles authentication. Footer navigation uses `FOOTER_LINKS`,
which includes `NAV_LINKS` plus `/about/`. Add future header-only links to
`HEADER_LINKS`; add links to `NAV_LINKS` when they should appear in both header and footer.
Below 640px all header links sit in a native hamburger dropdown, keeping the
header one row tall even when open. Navigation works without JavaScript; scripts
add Escape, outside-click, focus-leave, and desktop-resize dismissal.
Docs pages use the same `HEADER_LINKS` in their own header; below 50rem those
links, the GitHub link, and the theme toggle move into Starlight's sidebar menu.

## Add Docs and Blog Posts

### Docs Pages

Docs are rendered by [Starlight](https://starlight.astro.build) (configured in `starlight({ ... })` in [astro.config.mjs](astro.config.mjs)) with the Astro docs layout: a left sidebar of groups, one page per section, an **On this page** list (h2 and h3), previous/next links that follow the sidebar order, and an **Edit page** link built from `SITE.repoUrl`.

Pages are Markdown (`.md`; MDX is not installed) under `src/content/docs/docs/`. The path below `src/content/docs/` becomes the URL: `src/content/docs/docs/desktop-app/running-tests.md` is served at `/docs/desktop-app/running-tests/`, and an `index.md` serves its folder (`docs/desktop-app/index.md` is `/docs/desktop-app/`). Files whose names start with `_` are ignored. The collection uses Starlight's `docsLoader` and `docsSchema` in [src/content.config.ts](src/content.config.ts); a build fails on invalid front matter.

| Path (under `src/content/docs/`) | URL                            | Sidebar group                           |
| -------------------------------- | ------------------------------ | --------------------------------------- |
| `docs/index.md`                  | `/docs/`                       | Start here (label "Introduction")       |
| `docs/getting-started.md`        | `/docs/getting-started/`       | Start here                              |
| `docs/desktop-app/index.md`      | `/docs/desktop-app/`           | Desktop app (label "Overview", order 0) |
| `docs/desktop-app/<section>.md`  | `/docs/desktop-app/<section>/` | Desktop app, ordered by `sidebar.order` |

Docs front matter ([Starlight reference](https://starlight.astro.build/reference/frontmatter/)):

```yaml
title: Run tests # Page title (h1), browser title, and default sidebar label.
description: One sentence for the meta description and search results (required).
sidebar:
  order: 4 # Position within its sidebar group, lowest first.
  label: Run tests # Optional shorter sidebar label.
lastUpdated: 2026-10-27 # Optional; shown at the bottom of the page. Git dates are off.
draft: false # Optional; drafts appear only in `bun run dev`.
```

Each page starts at `##`; the title is the page's only h1. Keep heading text stable once published, because its id is the anchor that other pages and external sites link to.

**Sidebar.** Groups are defined in the `sidebar` option of [astro.config.mjs](astro.config.mjs). **Start here** lists pages by slug, in order. **Desktop app** autogenerates from every page in `src/content/docs/docs/desktop-app/`, ordered by `sidebar.order` (Overview is 0, then Install 1 through Troubleshooting 11). A new page in that folder appears automatically; give it a unique `sidebar.order` and add it to the **In this guide** list on the Overview page. A new top-level section needs its own folder and sidebar group.

**Asides.** Starlight asides work in plain Markdown: `:::note`, `:::tip`, `:::caution`, `:::danger`. Desktop pages that list `Ctrl` shortcuts repeat a one-line macOS `:::tip`, and pages with screenshots add "Click any screenshot to view it full screen." to that tip (or open with a tip of their own), because readers often land on a section page directly.

**Maintainer notes.** Put notes about a page in YAML comments (`# ...`) inside its front matter. HTML comments in Markdown are copied into the public HTML.

**Old single-page links.** The desktop guide used to be one page. Old deep links such as `/docs/desktop-app/#run-settings` are sent to the page that now holds that section by a small script that runs only on the Overview page. The map and its pure resolver are in [src/lib/docs-redirects.ts](src/lib/docs-redirects.ts) (tested by `docs-redirects.test.ts`), and the [Head override](src/components/starlight/Head.astro) mounts [LegacyHashRedirect](src/components/starlight/LegacyHashRedirect.astro) on that page. Unknown hashes, ids that exist on the Overview page, and visitors without JavaScript stay on the Overview, which lists every section. When you rename a desktop page or one of its headings, update the map's target; never give an Overview heading one of the map's old ids.

**Search.** Pagefind indexes the docs pages during `bun run build` into `dist/pagefind/`, and the header search box (`Ctrl+K`) queries it. Search needs JavaScript and a production build: `bun run dev` shows a notice instead, so use `bun run build` and `bun run preview` to try it. Only Starlight pages are indexed, not the landing pages or the blog.

**Theme and overrides.** Docs share the site's header identity, design tokens, and theme preference:

| File                                                                           | Role                                                                                                          |
| ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------- |
| [src/styles/tokens.css](src/styles/tokens.css)                                 | Design tokens mirrored from the desktop theme; loaded through Starlight's `customCss`.                        |
| [src/styles/starlight.css](src/styles/starlight.css)                           | Maps Starlight's `--sl-*` colors, fonts, header height, and spacing onto the tokens for both themes.          |
| [src/components/starlight/Header.astro](src/components/starlight/Header.astro) | Brand, search, `HEADER_LINKS`, GitHub, and theme toggle, like the site header.                                |
| [src/components/starlight/Head.astro](src/components/starlight/Head.astro)     | Stock head plus the legacy hash redirect on `/docs/desktop-app/` only.                                        |
| `ThemeProvider.astro`, `ThemeSelect.astro`                                     | Use the site's `regressify-theme` preference and the shared [ThemeScript](src/components/ThemeScript.astro). |
| `MobileMenuFooter.astro`                                                       | Below 50rem, holds the header links, GitHub, and theme toggle in the sidebar menu.                            |
| `MarkdownContent.astro`                                                        | Stock Markdown wrapper plus [ImageZoom](src/components/ImageZoom.astro) for full-screen screenshots.          |

Do not import `global.css` into docs pages: Starlight's styles are in cascade layers, so the site's unlayered element rules would override them. Starlight also sets `scopedStyleStrategy: 'where'` for the whole site, so scoped component styles get no extra specificity from scoping; `global.css` keeps its link hover rule at `a:where(:hover)` for that reason. Starlight's hover prefetch is turned off (`prefetch: false`) so links to `/admin/` are never prefetched.

### Blog Posts

Blog front matter:

```yaml
title: Regressify 1.3
description: One sentence for the post list, RSS, and search results.
date: 2026-11-01
author: Optional name
draft: false
```

The schemas live in [src/content.config.ts](src/content.config.ts); a build fails on invalid front matter. Blog posts are Markdown only. When migrating files from [docs](../docs), replace Zola shortcodes such as `{% spoiler() %}` with plain Markdown or `<details>`, and replace `@/...` links with site paths. Do not hard-code repository URLs in content; `SITE.repoUrl` controls every repository link.

### Copies of Repository Docs

The getting-started doc, the landing page quickstart, and the download page's build-from-source steps are copies of [docs/get-started.md](../docs/get-started.md), [docs/commands.md](../docs/commands.md), and [docs/desktop.md](../docs/desktop.md). Update them when those sources change.

The desktop app pages in `src/content/docs/docs/desktop-app/` are a user-facing adaptation of [docs/desktop.md](../docs/desktop.md). Update the matching page when desktop workflows, labels, or shortcuts change:

| docs/desktop.md section                                 | Website page (`desktop-app/`) |
| ------------------------------------------------------- | ----------------------------- |
| Windows Installation and Updates                        | `install.md`                  |
| Workspace and Files                                     | `workspaces.md`               |
| Editing (including Inheritance, Source and Save Safety) | `editing-suites.md`           |
| Running Commands                                        | `running-tests.md`            |
| Reviewing Results, Cleaning Test Runs                   | `reviewing-results.md`        |
| Git                                                     | `git.md`                      |
| Deploy Reports, Deployment Tokens, Landing Apps         | `publishing-reports.md`       |
| Appearance and Commands                                 | `appearance-and-commands.md`  |
| Shortcuts                                               | `keyboard-shortcuts.md`       |
| Problems from any of these sections                     | `troubleshooting.md`          |

The **Simplify images** page (`desktop-app/simplify-images.md`) adapts the image simplification settings in the Editing section of [docs/desktop.md](../docs/desktop.md), "Image simplification" in [docs/commands.md](../docs/commands.md), and the `imageSimplification` settings in [docs/create-test-suite.md](../docs/create-test-suite.md) and [docs/create-scenario.md](../docs/create-scenario.md); update it when those settings or their behavior change.

### Desktop Screenshots

Screenshots in the desktop pages live in [src/assets/docs/desktop](src/assets/docs/desktop) and are referenced from Markdown with relative paths, so Astro converts them to WebP with fixed dimensions. From a page in `src/content/docs/docs/desktop-app/` the path is `../../../../assets/docs/desktop/<file>.png`. Each file is used once; delete a screenshot when no page references it. Readers can click any image to open it full screen ([ImageZoom](src/components/ImageZoom.astro)). They are dark-theme window captures (1440x900 before cropping) of a release build (`cargo build --release --features gui --bin regressify-desktop`) opened on a throwaway fictional workspace ("northwind-qa", served from `127.0.0.1`).

Annotations are baked into the PNGs: a brand-violet (`#7a7efa`) ring around each control the text mentions and a numbered badge, filled with the brand gradient, matching the step's circled number (①, ②, …). Rings stay violet rather than gradient because the gradient's magenta would blend into the app's magenta diff pixels. Ring positions came from each control's Windows UI Automation bounding box, looked up by accessible name, so they line up with the real controls. When you change a step's numbering, recapture or re-annotate the image so the badges still match.

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

The home page keeps a short introduction, an existing desktop screenshot, and
terminal commands. Downloads are grouped by platform, with archive tables,
checksums, installer details, and source builds in native `details` disclosures.
Keep all files accessible without JavaScript and preserve the platform anchors.
The global scrollbar reserves a stable gutter and uses opaque theme-aware thumb
and track colors, so opening details never shifts the page horizontally.

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

Docs and site changes deploy without a release. Pushing website changes to
`master` (or running it manually) starts the
[website deploy workflow](../.github/workflows/website-deploy.yml). It fetches the
live `/downloads/latest.txt`, downloads the Linux CLI of that release tag from
GitHub, checks it against the feed's SHA-256, and runs the same deploy script.
The update feed and `/mcp` stay on the deployed release; only pages change.

That configuration deploys one Worker, `regressify`, on the custom domain
`regressify.eshn.dev` with `workers.dev` and preview URLs disabled. `dist/` serves
static assets with `404.html` for unknown paths. The website-owned
[Worker entry point](worker/index.ts) routes `/mcp` to the generated MCP handler
and `/admin` plus `/admin/*` to the private admin handler. The `ASSETS` binding
serves the admin page with no-store headers. Both the generated package and the
combined Worker are checked before deployment; `worker:build` is a dry run.
Keep compatibility settings aligned with
[the MCP Worker template](../src/mcp/worker/wrangler.jsonc.template);
`python ci/test-release.py` checks this. Deployment needs `CLOUDFLARE_ACCOUNT_ID`
and `CLOUDFLARE_API_TOKEN`, and the `eshn.dev` zone must belong to that account.

Do not deploy without a verified release manifest: a deployment replaces site
assets, including the update feed. GitHub binaries are not deleted by site deploys.

## Private Admin

The deployment table is at `https://regressify.eshn.dev/admin/`. Its same-origin
`GET /admin/api/sites` feed calls the configured PostHog endpoint server-side
with `POST {}` and a bearer token. Set Worker secrets `POSTHOG_API_ENDPOINT` and
`POSTHOG_API_TOKEN`; neither value belongs in `PUBLIC_*`, HTML, client scripts,
build-time environment definitions, or Wrangler `vars`.

Cloudflare Access must protect **both `/admin` and `/admin/*`**, including the API.
The Worker relies on that policy; it does not implement a second login or validate
Access JWTs. Do not enable unprotected Worker aliases or expose another origin.
The page is excluded from the sitemap and marked noindex. Admin responses use
`Cache-Control: private, no-store`; upstream error bodies and query metadata are
never returned. The upstream URL must use HTTPS, redirects are refused, requests
time out after 15 seconds, and results are limited to 2 MiB and 10,000 rows.

The query must return unique columns `url`, `datetime`, `file_count`,
`total_bytes`, `duration_ms` in any order. Additional columns are omitted.
Measurements must be nonnegative finite numbers or null; dates must be parseable
strings or null. The table supports full-URL search, raw-value column sorting,
manual refresh, and automatic refresh every 60 seconds while visible. Failed
refreshes retain the last successful table and show a warning. A PostHog
`hasMore` response is shown as partial, not silently treated as a complete list.
The desktop-only `#` column numbers the currently filtered and sorted rows from 1.
Size is binary MiB; relative dates have full local timestamp tooltips. Metrics
blend green/neutral/red over each complete returned column; values at or above
75% of its maximum are bold. Only HTTP(S) URLs without credentials become links.

For local end-to-end development, generate the ignored MCP package from the
repository root using the locally built CLI (this does not deploy):

```sh
cargo build --locked --bin regressify
target/debug/regressify mcp --output website/mcp-worker
cd website/mcp-worker
npm ci
cd ..
bun run check
bun run build
bun run worker:check
bun run worker:build
bun run worker:dev
```

On Windows the binary is `target/debug/regressify.exe`. Export requires a new
directory; reuse an existing generated package only if it matches your source.
`worker:dev` uses Wrangler's local runtime and reads the two keys from the
repository-root `.env.local` into Worker bindings without sending them to the
browser. Open the localhost URL Wrangler prints, then `/admin/`. Never bind this
development server to a public interface: local development has no Access gate.
`bun run dev` serves only Astro and cannot serve this Worker API.

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
