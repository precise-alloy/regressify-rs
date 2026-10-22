#!/usr/bin/env bash
# Builds the website from the staged release manifest and archives, copies
# /downloads/latest.txt into dist/downloads/, generates and verifies the MCP
# Worker with the released CLI, then deploys both as one Cloudflare Worker
# (website/wrangler.jsonc). Binary archives and the Windows installer are hosted
# on GitHub Releases; the website serves latest.txt, install.ps1, install.sh,
# and the /mcp endpoint.
set -euo pipefail

fail() {
  echo "error: $*" >&2
  exit 1
}

[ -n "${REGRESSIFY_BIN:-}" ] || fail "REGRESSIFY_BIN must name the released Regressify CLI"
[ -x "$REGRESSIFY_BIN" ] || fail "REGRESSIFY_BIN '$REGRESSIFY_BIN' is not executable"
[ -n "${DOWNLOADS_DIR:-}" ] || fail "DOWNLOADS_DIR must name the staged release downloads"
[ -f "$DOWNLOADS_DIR/latest.txt" ] || fail "DOWNLOADS_DIR '$DOWNLOADS_DIR' has no latest.txt"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
website="$root/website"
worker="$website/mcp-worker"
downloads="$(cd "$DOWNLOADS_DIR" && pwd)"

# The export refuses existing destinations, so always start from a clean copy.
rm -rf "$worker"
# Check and build the site before generating the Worker so `bun test` and
# `astro check` never pick up the generated project's files.
(
  cd "$website"
  bun install --frozen-lockfile
  bun run check
  # The release's own manifest and archives drive the download page.
  RELEASE_REQUIRED=1 RELEASE_MANIFEST_FILE="$downloads/latest.txt" bun run build
)
[ ! -e "$website/dist/downloads" ] || fail "The site build must not produce dist/downloads"
mkdir -p "$website/dist/downloads"
cp "$downloads/latest.txt" "$website/dist/downloads/latest.txt"

"$REGRESSIFY_BIN" mcp --output "$worker"
(
  cd "$worker"
  npm ci
  npm run check
  npm test
)

[ -n "${CLOUDFLARE_ACCOUNT_ID:-}" ] || fail "Set the CLOUDFLARE_ACCOUNT_ID secret to deploy."
[ -n "${CLOUDFLARE_API_TOKEN:-}" ] || fail "Set the CLOUDFLARE_API_TOKEN secret to deploy."
export WRANGLER_SEND_METRICS=false
cd "$website"
"$worker/node_modules/.bin/wrangler" deploy --config wrangler.jsonc
