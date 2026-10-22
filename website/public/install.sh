#!/bin/sh
# Install the Regressify CLI and desktop app (where supported) on macOS and Linux.
# Usage: curl -fsSL https://regressify.eshn.dev/install.sh | sh
regressify_install() (
set -eu

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

origin="${REGRESSIFY_ORIGIN:-https://regressify.eshn.dev}"
case "$origin" in
  https://*[!a-z0-9.-]*|https://|https://*/*)
    fail "Invalid REGRESSIFY_ORIGIN '$origin'; expected https://host without a path"
    ;;
  https://*) ;;
  *)
    fail "Invalid REGRESSIFY_ORIGIN '$origin'; expected https://host without a path"
    ;;
esac

os_name="${REGRESSIFY_OS:-$(uname -s)}"
arch_name="${REGRESSIFY_ARCH:-$(uname -m)}"
install_desktop=1

case "$os_name" in
  Darwin)
    case "$arch_name" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      x86_64|amd64) target="x86_64-apple-darwin" ;;
      *) fail "Unsupported macOS architecture '$arch_name'" ;;
    esac
    ;;
  Linux)
    case "$arch_name" in
      x86_64|amd64)
        libc="${REGRESSIFY_LIBC:-}"
        if [ -z "$libc" ]; then
          if command -v ldd >/dev/null 2>&1; then
            libc_version="$(ldd --version 2>&1 || true)"
            case "$libc_version" in
              *musl*) libc="musl" ;;
              *GLIBC*|*glibc*|*'GNU libc'*|*'Free Software Foundation'*) libc="gnu" ;;
            esac
          fi
          if [ -z "$libc" ] && { [ -e /lib64/ld-linux-x86-64.so.2 ] || [ -e /lib/x86_64-linux-gnu/ld-linux-x86-64.so.2 ]; }; then
            libc="gnu"
          fi
        fi
        [ "$libc" = "gnu" ] || fail "Prebuilt Linux releases require x86_64 glibc; Musl distributions are not supported"
        target="x86_64-unknown-linux-gnu"
        ;;
      aarch64|arm64)
        fail "Prebuilt Linux ARM64 releases are not supported; managed Chromium is unavailable for Linux ARM64. Use Linux x86_64 with glibc"
        ;;
      *) fail "Unsupported Linux architecture '$arch_name'" ;;
    esac
    ;;
  *)
    fail "Unsupported operating system '$os_name'. On Windows, run: irm https://regressify.eshn.dev/install.ps1 | iex"
    ;;
esac

if [ "${REGRESSIFY_CLI_ONLY:-0}" = "1" ]; then
  install_desktop=0
fi

command -v unzip >/dev/null 2>&1 || fail "'unzip' is required to extract Regressify archives"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print tolower($1)}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print tolower($1)}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print tolower($NF)}'
  else
    fail "sha256sum, shasum, or openssl is required to verify downloads"
  fi
}

fetch_file() {
  url="$1"
  dest="$2"
  limit="$3"
  curl -q -fsSL --proto '=https' --proto-redir '=https' --max-redirs 5 \
    --connect-timeout 10 --max-time 900 --max-filesize "$limit" "$url" -o "$dest"
}

command -v curl >/dev/null 2>&1 || fail "curl is required to download Regressify"
tmp_dir="$(mktemp -d 2>/dev/null || mktemp -d -t regressify-install)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup 0
trap 'exit 1' INT TERM

manifest_file="$tmp_dir/latest.txt"
if [ -n "${REGRESSIFY_MANIFEST_FILE:-}" ]; then
  cp "$REGRESSIFY_MANIFEST_FILE" "$manifest_file"
else
  printf 'Fetching release manifest from %s/downloads/latest.txt...\n' "$origin"
  curl -q -fsS --proto '=https' --max-redirs 0 --connect-timeout 10 --max-time 30 \
    --max-filesize 16384 -H 'Cache-Control: no-cache' "$origin/downloads/latest.txt" -o "$manifest_file"
fi

[ "$(wc -c < "$manifest_file")" -le 16384 ] || fail "Release manifest exceeds 16 KiB"
tr -d '\r' < "$manifest_file" > "$tmp_dir/manifest.clean"
mv "$tmp_dir/manifest.clean" "$manifest_file"
awk -F= '
  function version(value, required, count, parts, result, component) {
    count = split(value, parts, ".")
    if (count < required || count > 4) return ""
    result = ""
    for (component = 1; component <= 4; component++) {
      if (component <= count && (parts[component] !~ /^[0-9]+$/ || length(parts[component]) > 4)) return ""
      result = result (component == 1 ? "" : ".") (parts[component] + 0)
    }
    return result
  }
  NF {
    if (NF != 2 || $1 !~ /^[a-zA-Z0-9_.-]+$/ || seen[$1]++) exit 1
    fields[$1] = $2
  }
  END {
    normalized = version(fields["version"], 4)
    if (normalized == "" || normalized != fields["version"] || fields["tag"] !~ /^v/ ||
        version(substr(fields["tag"], 2), 3) != normalized ||
        fields["sha256"] !~ /^[0-9a-fA-F]+$/ || length(fields["sha256"]) != 64 ||
        fields["size"] !~ /^[0-9]+$/ || fields["size"] + 0 < 1 || fields["size"] + 0 > 1073741824 ||
        fields["installer"] != "https://github.com/precise-alloy/regressify-rs/releases/download/" fields["tag"] "/Regressify-Setup-" normalized "-x64.exe") exit 1
  }
' "$manifest_file" || fail "Invalid or ambiguous release manifest"

read_field() {
  key="$1"
  awk -F'=' -v k="$key" '$1 == k { sub(/^[^=]*=/, ""); print; exit }' "$manifest_file"
}

version="$(read_field version)"
tag="$(read_field tag)"
installer_url="$(read_field installer)"
[ -n "$version" ] || fail "Release manifest is missing version"
[ -n "$installer_url" ] || fail "Release manifest is missing installer URL"

if [ -z "$tag" ]; then
  tag="$(printf '%s' "$installer_url" | sed -n 's|^https://github\.com/precise-alloy/regressify-rs/releases/download/\(v[0-9.]*\)/.*$|\1|p')"
fi
case "$tag" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *) fail "Invalid release tag '$tag' in manifest" ;;
esac

download_and_extract() {
  archive_name="$1"
  binary_name="$2"
  meta="$(read_field "archive.$archive_name")"
  [ -n "$meta" ] || fail "Release manifest does not list '$archive_name'"
  expected_size="${meta%%:*}"
  expected_hash="$(printf '%s' "${meta#*:}" | tr 'A-F' 'a-f')"
  case "$expected_size" in
    ''|*[!0-9]*) fail "Invalid size for '$archive_name' in manifest" ;;
  esac
  [ "$expected_size" -ge 1 ] && [ "$expected_size" -le 1073741824 ] || fail "Archive size out of range"
  case "$expected_hash" in *[!0-9a-f]*) fail "Invalid SHA-256 for '$archive_name'" ;; esac
  case "${#expected_hash}" in
    64) ;;
    *) fail "Invalid SHA-256 for '$archive_name' in manifest" ;;
  esac

  archive_path="$tmp_dir/$archive_name"
  if [ -n "${REGRESSIFY_ASSETS_DIR:-}" ]; then
    cp "$REGRESSIFY_ASSETS_DIR/$archive_name" "$archive_path"
  else
    archive_url="https://github.com/precise-alloy/regressify-rs/releases/download/$tag/$archive_name"
    printf 'Downloading %s (%s)...\n' "$archive_name" "$tag"
    fetch_file "$archive_url" "$archive_path" "$expected_size"
  fi

  actual_size="$(wc -c < "$archive_path" | tr -d '[:space:]')"
  [ "$actual_size" = "$expected_size" ] || fail "Size mismatch for '$archive_name' (expected $expected_size, got $actual_size)"

  actual_hash="$(sha256_file "$archive_path")"
  [ "$actual_hash" = "$expected_hash" ] || fail "SHA-256 mismatch for '$archive_name'"

  extract_dir="$tmp_dir/extract-$binary_name"
  mkdir -p "$extract_dir"
  entries="$(unzip -Z1 "$archive_path" | LC_ALL=C sort)"
  expected_entries="$(printf 'LICENSE\nLICENSING.md\n%s\n' "$binary_name" | LC_ALL=C sort)"
  [ "$entries" = "$expected_entries" ] || fail "Unexpected archive entries in '$archive_name'"
  for entry in "$binary_name" LICENSE LICENSING.md; do
    unzip -p "$archive_path" "$entry" > "$extract_dir/$entry"
  done
  chmod +x "$extract_dir/$binary_name"
}

install_dir="${REGRESSIFY_INSTALL_DIR:-$HOME/.local/bin}"
share_dir="${REGRESSIFY_SHARE_DIR:-$HOME/.local/share/regressify}"
download_and_extract "regressify-cli-$target.zip" "regressify"
if [ "$install_desktop" = "1" ]; then
  download_and_extract "regressify-desktop-$target.zip" "regressify-desktop"
fi

mkdir -p "$install_dir" "$share_dir"
install_file() {
  source_file="$1"
  destination="$2"
  [ ! -L "$destination" ] || fail "Refusing to replace symlink '$destination'"
  [ ! -d "$destination" ] || fail "Refusing to replace directory '$destination'"
  temporary="$(mktemp "${destination}.XXXXXX")"
  if ! cp -p "$source_file" "$temporary" || ! mv -f "$temporary" "$destination"; then
    rm -f "$temporary"
    fail "Cannot install '$destination'"
  fi
}
install_file "$tmp_dir/extract-regressify/regressify" "$install_dir/regressify"
if [ "$install_desktop" = "1" ]; then
  install_file "$tmp_dir/extract-regressify-desktop/regressify-desktop" "$install_dir/regressify-desktop"
fi
for license_file in LICENSE LICENSING.md; do
  install_file "$tmp_dir/extract-regressify/$license_file" "$share_dir/$license_file"
done

if [ "${REGRESSIFY_SKIP_CHROMIUM:-0}" != "1" ]; then
  printf 'Preparing managed Chromium build...\n'
  if ! "$install_dir/regressify" install; then
    printf 'warning: Chromium could not be prepared right now; "regressify install" or the first test run will retry.\n' >&2
  fi
fi

if [ "$install_desktop" = "1" ]; then
  printf 'Installed Regressify %s (regressify and regressify-desktop) to %s\n' "$version" "$install_dir"
else
  printf 'Installed Regressify %s (regressify CLI) to %s\n' "$version" "$install_dir"
fi

case ":${PATH:-}:" in
  *":$install_dir:"*) ;;
  *)
    printf '\nAdd %s to your PATH if it is not already configured:\n  export PATH="%s:$PATH"\n' "$install_dir" "$install_dir"
    ;;
esac
)

regressify_install
