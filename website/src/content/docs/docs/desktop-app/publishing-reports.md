---
title: Publish reports
description: Deploy reports to Azure Static Web Apps, Netlify, or Cloudflare Workers, manage tokens, and build landing apps.
sidebar:
  label: Publish reports
  order: 8
lastUpdated: 2026-10-27
# The DO_NOT_TRACK paragraph applies to every deployment, so it sits before the
# subsections rather than under Landing apps. (A YAML comment is not rendered.)
---

:::tip
Click any screenshot to view it full screen.
:::

Publish to Azure Static Web Apps, Netlify, or Cloudflare Workers without extra tools.

1. Select suites, then click **Deploy selected suite reports** ①.
1. Click **Add target** ②, enter a **Target name** ③, and pick a **Provider** ④.
1. Paste a **Deployment token** ⑤ (for Netlify or Cloudflare you can pick a saved token under **Token source**), then
   click **Save target** ⑥.
1. Click **Deploy**. For a partial selection, tick "Yes, deploy only these N of M suites".
1. Watch **Activity**, then click **Open Site**.

![The Deploy reports dialog with the title-bar Deploy button, Add target, Target name, Provider, Deployment token, and Save target numbered.](../../../../assets/docs/desktop/deploy-dialog.png)

- Each suite's latest completed full-suite run is published. Partial or stopped runs aren't used.
- Source files, login state, and unrelated files aren't uploaded. Screenshots and references don't change.
- **Stop** can't undo what the provider already accepted.
- **Deployment targets** (gear) has **Copy deployed URL**. Report titles are in Run settings > Report.

| Provider              | You need                                                                                                                            |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| Azure Static Web Apps | Deployment token. **Azure environment** blank means production.                                                                     |
| Netlify               | An existing site's **Netlify Site ID** (shown as Project ID in Netlify) and a personal access token. Deploying replaces production. |
| Cloudflare Workers    | **Cloudflare Account ID**, **Worker name**, and a token with Workers Scripts Write.                                                 |

For Cloudflare, **workers.dev** can be **Public (production)** (the default, which makes the report reachable at a
public workers.dev address and needs a workers.dev subdomain), **Private**, or **Preserve existing routing**.

Reports ask search engines not to index them but aren't private. Use your provider's access controls. Each successful
deployment sends one anonymous event; set `DO_NOT_TRACK=1` to disable it.

## Deployment tokens

**Publish** > **Deployment tokens** saves Netlify or Cloudflare tokens for all workspaces. Pick one under **Token
source**, or use **Target-specific token**. Azure tokens are per target.

Tokens are stored in the OS credential store, never in files, settings, or logs, and never shown again. Leave the
field blank to keep a token. Removing a target or token never deletes a website.

## Landing apps

A landing app lists several report sites with live pass rates.

1. Open **Publish** > **Landing apps** (no workspace needed).
1. Click **New landing app** ①, then fill in **Heading** ② and **Subheading (Markdown)** ③.
1. Click **Add site**, then enter a **Site heading** ④ and an absolute `http://` or `https://` **URL** (up to 100).
1. Click **Preview** ⑤.
1. Click **Save** ⑥, then **Deploy** ⑦.

![The Landing apps dialog with New landing app, Heading, Subheading (Markdown), Site heading, Preview, Save, and Deploy numbered.](../../../../assets/docs/desktop/landing-apps.png)

- Use `https://` sites on an HTTPS landing page; browsers may block status from `http://` sites. Private or
  login-protected reports show "Status unavailable".
- Behind Cloudflare Access, add a self-hosted Access app for `<report-host>/_snapshot/status.json` with a **Bypass**
  policy for **Everyone**. Only pass counts and a timestamp become public.
- Landing apps are saved in desktop settings. Older reports show "Status unavailable" until you regenerate and redeploy them.
