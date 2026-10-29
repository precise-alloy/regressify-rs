---
name: release-pipeline
description: 'Use when changing GitHub Actions release matrices, Rust target builds, GitHub Release assets, bootstrapper hashes, multi-OS install scripts, update manifests, publish verification, or the Cloudflare website/MCP deployment.'
---

# Release Pipeline

## Start Here

[GitHub release](../../workflows/tuyen-release.yml), [build script](../../../ci/make-release.sh), [release feed helper](../../../ci/release_feed.py), [Cloudflare deploy script](../../../ci/deploy-cloudflare.sh), [website Worker config](../../../website/wrangler.jsonc), and [offline release tests](../../../ci/test-release.py). Frontend asset publication uses [snapshot-assets.yml](../../workflows/snapshot-assets.yml). Website-only deploys use [website-deploy.yml](../../workflows/website-deploy.yml).

## Procedure and Invariants

1. Trace matrix inputs to target/toolchain/features, artifact names, upload fields, installer versions, and update manifests. Do not assume CLI/desktop targets match.
   Build jobs use GitHub Actions' default timeout: neither `timeout-minutes` nor
   a matrix `timeout` field is set. Keep matrix tests aligned with this contract.
1. Desktop selections also schedule a CLI job for each supported target without duplicates. Targets are Windows x64, Linux x64 GNU, and macOS Intel/Apple silicon. Musl releases are retired; Linux ARM64 has no managed Chromium support. Do not restore these matrix rows without native TLS/toolchain and browser-runtime verification. Tagged Windows payloads build independently with static runtimes; packaging must consume both exact artifacts from the same run and stop on either failure.
1. Preserve `--locked`, static-runtime flags where required, archive contents (the executable plus `LICENSE` and `LICENSING.md`), executable resource versions, and immutable installer naming/hash/size contracts.
1. Only automatic tag builds in `precise-alloy/regressify-rs` publish. Eight application/target builds and independent offline checks run in parallel; `publish` depends on all builds, checks, and Windows packaging. `deploy` depends on publication. Never bypass failures with `always()` or continue-on-error. Manual runs retain workflow artifacts only.
1. `stage` requires 16 archives, 16 named checksum sidecars, and one `Regressify-Setup-N.N.N.N-x64.exe`; reject missing/duplicate/unexpected assets, symlinks, bad hashes, or files above the 1 GiB client limit before staging. The generated `latest.txt` is the 34th release asset, at most 16 KiB, with `archive.<filename>=<size>:<sha256>` records. Keep Python, Rust, website, and terminal script validators synchronized.
1. Only `publish` has `contents: write`; use its built-in `GITHUB_TOKEN`, not a PAT. Create a draft only after complete staging. Verify uploaded state, exact names, sizes, SHA-256 digests, and draft-scoped URLs before making it public. Resume missing draft assets without `--clobber`; immutable published releases must already match. Fail closed on API errors and unknown digests. Recheck the public inventory and canonical tagged URLs after publication and in `deploy` before any website build.
   The tag endpoint is published-only. Find drafts via authenticated release
   listings with exact tag matching, 10 releases per page, and a 1000-release cap.
   Model that API distinction in tests; reject malformed or ambiguous listings.
   Verify the tag exists before creating a draft through the JSON API, then use
   its returned metadata directly. Do not require an immediate listing to see it.
   Request fresh API reads; treat failed writes as errors, including HTTP 404.
   Draft asset URLs may use `untagged-...`: bind them to the exact temporary path
   from the draft's validated HTTPS GitHub/repository URL, never any matching prefix.
   Tests must switch to canonical tagged asset URLs after publication and reject
   temporary URLs in published-release verification.
1. The website hosts metadata, install scripts, static pages, `/mcp`, and private `/admin/`; binaries remain on GitHub. Build from the verified manifest before exporting `website/mcp-worker`, copy only `latest.txt` into `dist/downloads`, verify the generated package, then run website `worker:check` and `worker:build` before deploying. The website-owned `worker/index.ts` delegates MCP unchanged; `run_worker_first` covers `/mcp`, `/admin`, `/admin/*`. Access must protect both admin paths and its API; PostHog bindings stay server-only and no-store. Keep compatibility settings aligned; only deployment receives Cloudflare secrets.
   Website-only deploys never change the feed: they reuse the live `latest.txt`, regenerate `/mcp` with that tag's Linux CLI only after its SHA-256 matches the feed's `archive.` record, and finish by comparing the live feed bytes. Fail on a missing tag: `gh release download` without one falls back to GitHub's latest release.
1. Keep preparation/verification separate from publication. A script or workflow edit does not authorize a tag, release upload, real remote operation, or install.
1. Validate external responses and URLs with bounded reads and clear errors. Never print publishing secrets or embed credentials in artifacts/manifests.
1. Azure release publishing is retired. Keep workflow concurrency serialized and build/publish credentials separate. Windows scripts require Git Bash. Do not use a glibc C compiler as a Musl sysroot substitute.
1. Terminal installers validate manifests, HTTPS URLs, sizes, and hashes before execution/replacement. Unix verifies all selected archives first and preserves licenses. Never remove quarantine attributes or disable OS security policies. Windows launches verified setup; setup owns payload download.
1. The Tuyen Drive bridge is deferred until maintainer confirmation of the live site. Start from `d066dc81a2befb9cb622762cbf3c7a21e8c198fc`, backport the new updater contract, retain the old feed filename, and ensure old < bridge < site versions. See [rollout](../../../docs/desktop.md#bridge-rollout); this does not authorize publication.
1. Inspect path filters so changed release files actually trigger their intended offline gates. Do not claim a release-only installer test is a pull-request gate.
1. Frontend pushes on `release` build with the frozen Bun lockfile. The asset PR job must check out `master` independently and commit only `src/snapshot/data`. Keep build permissions read-only and PR credentials out of the build job. PRs are never automatically merged. Document GitHub Actions PR permissions and the optional `SNAPSHOT_ASSETS_TOKEN` for triggering downstream checks.
1. Snapshot PR publication uses `gh pr list/create/edit/close` with `GH_TOKEN`, not a PR action. Push or delete the automation branch only with an explicit observed-SHA lease. No asset diff means no commit; close the stale PR and remove its branch. Lookup, diff, commit, and push errors must propagate. Test this Bash path with mocked `git`/`gh`, never real remotes.

## Checks

```sh
python ci/test-release.py
```

Requires PyYAML. Tests should use synthetic credentials/local fixture responses, not live publishing commands. Run affected target build checks only when the toolchain exists; explicitly report untested cross-platform packaging.

Bash and PowerShell 7.4+ are required; on Windows set `BASH_PATH` to Git Bash. Tests mock GitHub queries/uploads and install execution. Include incomplete local/remote inventories, upload failures, mismatched published assets, all supported platform scripts, and preservation on failure. Hosted permissions and real cross-platform binaries remain separate gates.

The Windows packaging job sets `ISCC_PATH` and runs `python ci/test-release.py InstallerTests` before packaging/smoke. Isolated fixtures cover PATH, process trees, prepare-only mode while the app is running, corrupt payloads, and stale receipt cleanup without installing the real app. Missing Inno skips native checks outside packaging jobs.

## Maintain

Update [README](../../../README.md), [desktop docs](../../../docs/desktop.md), applicable install docs, and this skill for release-contract changes. Keep scripts, workflow inputs, updater expectations, and test fixtures synchronized.
