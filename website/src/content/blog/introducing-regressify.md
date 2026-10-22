---
title: Introducing Regressify
description: A visual regression testing tool for web pages, with a CLI for automation and a native desktop app for QA.
date: 2026-10-22
---

Small CSS changes can break pages far from the code that changed. Regressify catches those changes by comparing
screenshots of your pages before and after a deployment, so you can review every difference before your users see it.

## What it does

Regressify is a visual regression testing tool written in Rust. You describe the pages to check in test suites written
in YAML or JSON. Regressify drives Chromium over the Chrome DevTools Protocol, takes screenshots, compares them with
approved reference images, and builds an interactive HTML report.

The workflow has three steps:

1. `regressify ref` captures baselines and keeps only the captures that succeeded.
1. `regressify test` captures again and compares the new screenshots with the references.
1. `regressify approve` promotes intended changes to become the new references.

A failed capture never replaces a good reference, and a failing scenario never stops the others.

## Two ways to use it

The **CLI** fits scripts and CI. Configuration and infrastructure problems return a non-zero exit code, while visual
mismatches and website failures go into the report, so a pipeline can tell a broken setup from a changed page.

The **desktop app** is built for QA staff. It offers guided and source editors for suites and scenarios, runs suites with
live progress, and shows reference, current, and diff images side by side with per-scenario approval. It bundles the
engine, so no separate CLI install is needed. It can also publish reports to Azure Static Web Apps, Netlify, or
Cloudflare.

## Interactions that behave like a real user

Scenarios can click, type, hover, scroll, select, switch tabs, and more before a screenshot. Each action waits until
its target exists, is visible, has stopped moving, is scrolled into view, is not covered, and is enabled. Selectors can
reach into open Shadow DOM and frames.

## No Node toolchain required

Regressify does not need Node or Playwright. It manages one pinned Chromium build in a per-user cache that the CLI, the
desktop app, and every workspace share.

## Try it

[Download the Windows installer](/download/), which adds both the desktop app and the CLI to your PATH, or follow the
[getting started guide](/docs/getting-started/) to build from source and run your first visual check.
