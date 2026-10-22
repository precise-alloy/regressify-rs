"""Resolve release tags, stage GitHub Release assets, write the update manifest, and verify the deployed feed.

Binary archives and the Windows shell installer are published to GitHub Releases
of precise-alloy/regressify-rs. The website Worker hosts /downloads/latest.txt,
which records the release version, tag, installer URL/SHA-256/size, and per-OS
archive metadata.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request


MAX_MANIFEST_SIZE = 16 * 1024
MAX_ASSET_SIZE = 1024 * 1024 * 1024
MANIFEST_NAME = "latest.txt"
DOWNLOADS_PATH = "downloads"
RELEASE_REPO = "precise-alloy/regressify-rs"
RELEASE_DOWNLOAD_PREFIX = f"https://github.com/{RELEASE_REPO}/releases/download"
TARGETS = (
    "x86_64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
)
ARCHIVE_PATTERN = re.compile(
    r"(regressify-cli|regressify-desktop)-(" + "|".join(map(re.escape, TARGETS)) + r")\.(zip|7z)(\.sha256)?"
)
INSTALLER_PATTERN = re.compile(r"Regressify-Setup-[0-9]{1,4}(?:\.[0-9]{1,4}){3}-x64\.exe")


def version_parts(value):
    if not re.fullmatch(r"[0-9]{1,4}(?:\.[0-9]{1,4}){0,3}", value):
        raise ValueError("Version must have one to four numeric components, each 0-9999")
    parts = tuple(int(part) for part in value.split("."))
    return parts + (0,) * (4 - len(parts))


def tag_version(ref_type, ref_name):
    if ref_type != "tag":
        return ""
    if not re.fullmatch(r"v[0-9]{1,4}(?:\.[0-9]{1,4}){2,3}", ref_name):
        raise ValueError("Release tag must be vN.N.N or vN.N.N.N, each component 0-9999")
    return ".".join(map(str, version_parts(ref_name[1:])))


def validate_origin(origin):
    parts = urllib.parse.urlsplit(origin)
    if (parts.scheme != "https" or not parts.hostname or parts.port is not None or parts.username is not None
            or parts.password is not None or parts.path or parts.query or parts.fragment
            or origin != f"https://{parts.hostname}" or not re.fullmatch(r"[a-z0-9.-]{1,253}", parts.hostname)):
        raise ValueError("RELEASE_ORIGIN must be an https://host origin without a path")
    return origin


def installer_name(version):
    return f"Regressify-Setup-{version}-x64.exe"


def installer_url(tag, version):
    return f"{RELEASE_DOWNLOAD_PREFIX}/{tag}/{installer_name(version)}"


def archive_url(tag, name):
    return f"{RELEASE_DOWNLOAD_PREFIX}/{tag}/{name}"


def manifest_url(origin):
    return f"{origin}/{DOWNLOADS_PATH}/{MANIFEST_NAME}"


def manifest_text(tag, version, size, digest, archives=()):
    lines = [
        f"version={version}",
        f"tag={tag}",
        f"installer={installer_url(tag, version)}",
        f"sha256={digest}",
        f"size={size}",
    ]
    for name, archive_size, archive_digest in archives:
        lines.append(f"archive.{name}={archive_size}:{archive_digest}")
    return "\n".join(lines) + "\n"


def file_digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def required_archives():
    return {
        f"regressify-{application}-{target}.{extension}"
        for target in TARGETS
        for application in ("cli", "desktop")
        for extension in ("zip", "7z")
    }


def required_assets(version):
    archives = required_archives()
    return archives | {f"{name}.sha256" for name in archives} | {installer_name(version)}


def release_files(source, version, include_manifest=False):
    files = {}
    for path in sorted(source.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"Symlinks are not release assets: '{path.name}'")
        if not path.is_file():
            continue
        if path.name in files:
            raise ValueError(f"Duplicate release file '{path.name}'")
        files[path.name] = path
    installers = {name for name in files if INSTALLER_PATTERN.fullmatch(name)}
    if installers != {installer_name(version)}:
        raise ValueError(f"Expected exactly one installer named {installer_name(version)}")
    expected = required_assets(version) | ({MANIFEST_NAME} if include_manifest else set())
    unexpected = files.keys() - expected
    if unexpected:
        raise ValueError(f"Unexpected release file '{sorted(unexpected)[0]}'")
    missing = expected - files.keys()
    if missing:
        raise ValueError(f"Missing required release assets: {', '.join(sorted(missing))}")
    for name, path in files.items():
        limit = MAX_MANIFEST_SIZE if name.endswith((".txt", ".sha256")) else MAX_ASSET_SIZE
        if not 0 < path.stat().st_size <= limit:
            raise ValueError(f"'{name}' must be 1 byte to {limit} bytes (release binaries: 1 byte to 1 GiB)")
    for name in required_archives():
        recorded = files[f"{name}.sha256"].read_text(encoding="utf-8").strip()
        match = re.fullmatch(r"([0-9a-fA-F]{64})[ \t]+\*?" + re.escape(name), recorded)
        if not match or match[1].lower() != file_digest(files[name]):
            raise ValueError(f"'{name}.sha256' does not match the archive")
    return files


def stage_downloads(source, destination, version):
    """Copy validated release files into an empty directory.

    Only recognized archive, checksum, and installer names are accepted, so the
    staging step can never publish stray artifacts or overwrite site pages.
    """
    if destination.exists() and any(destination.iterdir()):
        raise ValueError(f"Download staging directory '{destination}' must be empty")
    files = release_files(source, version)
    destination.mkdir(parents=True, exist_ok=True)
    for path in files.values():
        shutil.copyfile(path, destination / path.name)
    archive_entries = []
    for archive in sorted(required_archives()):
        digest = file_digest(destination / archive)
        archive_entries.append((archive, (destination / archive).stat().st_size, digest))
    return destination / installer_name(version), archive_entries


def response_error(operation, status, content):
    detail = content.decode("utf-8", errors="replace")
    detail = " ".join("".join(character if character.isprintable() or character.isspace() else " "
                              for character in detail).split())
    if len(detail) > 1000:
        detail = detail[:1000] + "..."
    return RuntimeError(f"{operation} (HTTP {status}): {detail or 'Empty response body'}")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


def fetch_manifest(origin):
    opener = urllib.request.build_opener(NoRedirect())
    message = urllib.request.Request(
        manifest_url(origin),
        headers={"Cache-Control": "no-cache", "User-Agent": "Regressify-Release/1.0"},
    )
    try:
        response = opener.open(message, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return response.status, response.read(MAX_MANIFEST_SIZE + 1)


def manifest_fields(content):
    if len(content) > MAX_MANIFEST_SIZE:
        raise ValueError("Manifest exceeds 16 KiB")
    text = content.decode("utf-8-sig").strip()
    if text.startswith("{"):
        return json.loads(text)
    fields = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        key, value = line.split("=", 1)
        key, value = key.strip(), value.strip()
        if key in fields:
            raise ValueError("Duplicate manifest field")
        fields[key] = value
    return fields


def github_request(endpoint, *, data=None):
    request = urllib.request.Request(
        f"https://api.github.com/repos/{RELEASE_REPO}/{endpoint}",
        data=json.dumps(data).encode("utf-8") if data is not None else None,
        headers={"Accept": "application/vnd.github+json", "User-Agent": "Regressify-Release/1.0",
                 "X-GitHub-Api-Version": "2022-11-28", "Cache-Control": "no-cache"},
    )
    if data is not None:
        request.add_header("Content-Type", "application/json")
    token = os.environ.get("GH_TOKEN")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.build_opener(NoRedirect()).open(request, timeout=30) as response:
            content = response.read(2 * 1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        if error.code == 404 and data is None:
            return None
        raise RuntimeError(f"GitHub release request failed (HTTP {error.code})") from None
    if len(content) > 2 * 1024 * 1024:
        raise ValueError("GitHub release response is too large")
    return json.loads(content)


def find_github_release(tag):
    release = github_request(f"releases/tags/{tag}")
    if release is not None:
        return release
    # The tag endpoint is published-only; authenticated listings also include drafts.
    for page in range(1, 101):
        releases = github_request(f"releases?per_page=10&page={page}")
        if (not isinstance(releases, list) or len(releases) > 10
                or any(not isinstance(entry, dict) or not isinstance(entry.get("tag_name"), str)
                       for entry in releases)):
            raise ValueError("Unexpected GitHub release listing")
        matches = [entry for entry in releases if entry["tag_name"] == tag]
        if len(matches) > 1:
            raise ValueError("Duplicate GitHub releases for this tag")
        if matches:
            return matches[0]
        if len(releases) < 10:
            return None
    raise ValueError("GitHub release lookup exceeded 1000 releases")


def github_asset_tag(release, tag):
    if release.get("draft") is not True:
        return tag
    location = release.get("html_url")
    if not isinstance(location, str):
        raise ValueError("Unexpected GitHub draft release URL")
    parsed = urllib.parse.urlsplit(location)
    prefix = f"/{RELEASE_REPO}/releases/tag/"
    asset_tag = parsed.path.removeprefix(prefix)
    if (parsed.scheme != "https" or parsed.netloc != "github.com" or parsed.query or parsed.fragment
            or not parsed.path.startswith(prefix)
            or (asset_tag != tag and not re.fullmatch(r"untagged-[0-9a-f]{1,64}", asset_tag))):
        raise ValueError("Unexpected GitHub draft release URL")
    # Draft asset URLs use this temporary segment until the release is published.
    return asset_tag


def github_assets(release):
    identity = release.get("id")
    if type(identity) is not int or identity <= 0:
        raise ValueError("GitHub returned an invalid release ID")
    assets = github_request(f"releases/{identity}/assets?per_page=100")
    if not isinstance(assets, list) or len(assets) >= 100:
        raise ValueError("Unexpected GitHub release asset inventory")
    return assets


def verify_github_assets(assets, files, tag, allow_missing=False):
    seen = set()
    for asset in assets:
        name = asset.get("name")
        if name not in files or name in seen:
            raise ValueError("Unexpected or duplicate GitHub release asset")
        seen.add(name)
        if (asset.get("state") != "uploaded"
                or asset.get("size") != files[name].stat().st_size
                or asset.get("digest") != f"sha256:{file_digest(files[name])}"
                or asset.get("browser_download_url") != archive_url(tag, name)):
            raise ValueError(f"GitHub release asset '{name}' does not match the verified build")
    missing = files.keys() - seen
    if missing and not allow_missing:
        raise ValueError(f"Missing required GitHub release assets: {', '.join(sorted(missing))}")
    return sorted(missing)


def checked_release_files(source, tag, version):
    files = release_files(source, version, include_manifest=True)
    installer = files[installer_name(version)]
    archives = [(name, files[name].stat().st_size, file_digest(files[name]))
                for name in sorted(required_archives())]
    expected = manifest_text(tag, version, installer.stat().st_size, file_digest(installer), archives)
    if files[MANIFEST_NAME].read_bytes() != expected.encode("utf-8"):
        raise ValueError("Staged manifest does not match the complete release assets")
    return files


def verify_github_release(source, tag, version):
    files = checked_release_files(source, tag, version)
    release = github_request(f"releases/tags/{tag}")
    if not isinstance(release, dict) or release.get("tag_name") != tag or release.get("draft") is not False or release.get("prerelease") is not False:
        raise ValueError("Expected a published, non-prerelease GitHub release for this tag")
    verify_github_assets(github_assets(release), files, tag)


def publish_release(source, tag, version):
    if os.environ.get("GITHUB_REPOSITORY") != RELEASE_REPO:
        raise ValueError(f"Release publication must run in {RELEASE_REPO}")
    files = checked_release_files(source, tag, version)
    release = find_github_release(tag)
    if release is None:
        reference = github_request(f"git/ref/tags/{tag}")
        if not isinstance(reference, dict) or reference.get("ref") != f"refs/tags/{tag}":
            raise ValueError("The release tag must exist before creating a draft")
        # A successful create response is authoritative even before listings expose the draft.
        release = github_request("releases", data={
            "tag_name": tag, "name": f"Regressify {tag}", "draft": True,
            "prerelease": False, "generate_release_notes": True,
        })
    if not isinstance(release, dict) or release.get("tag_name") != tag or release.get("prerelease") is not False:
        raise ValueError("GitHub returned an unexpected release")
    draft = release.get("draft") is True
    asset_tag = github_asset_tag(release, tag)
    missing = verify_github_assets(github_assets(release), files, asset_tag, allow_missing=draft)
    if missing:
        subprocess.run(["gh", "release", "upload", tag, "--repo", RELEASE_REPO,
                        *[str(files[name]) for name in missing]], check=True, timeout=900)
    verify_github_assets(github_assets(release), files, asset_tag)
    if draft:
        subprocess.run(["gh", "release", "edit", tag, "--repo", RELEASE_REPO, "--draft=false"],
                       check=True, timeout=120)
    verify_github_release(source, tag, version)


def verify_publish(origin, tag, version, file, digest):
    expected_url = installer_url(tag, version)
    failure = "Published manifest does not match this installer version, URL, size and SHA-256"
    for attempt in range(5):
        try:
            status, content = fetch_manifest(origin)
        except (urllib.error.URLError, TimeoutError, OSError):
            failure = "Cannot reach the website to verify the published manifest"
        else:
            if status == 200:
                try:
                    fields = manifest_fields(content)
                    tag_field = fields.get("tag")
                    matches = (version_parts(str(fields.get("version", ""))) == version_parts(version)
                               and (tag_field is None or str(tag_field) == tag)
                               and fields.get("installer") == expected_url
                               and str(fields.get("sha256", "")).lower() == digest
                               and str(fields.get("size")) == str(file.stat().st_size))
                except ValueError:
                    matches = False
                if matches:
                    return
                # Edge caches can briefly serve the previous deployment, so mismatches are retried.
                failure = "Published manifest does not match this installer version, URL, size and SHA-256"
            else:
                error = response_error("Cannot verify published manifest", status, content)
                if status not in (404, 429, 500, 502, 503, 504):
                    raise error
                failure = str(error)
        if attempt < 4:
            time.sleep(2 ** attempt)
    raise RuntimeError(failure)


def checked_installer(path, version):
    if path.name != installer_name(version):
        raise ValueError("Installer filename must contain the canonical release version")
    if not path.is_file() or not 0 < path.stat().st_size <= MAX_ASSET_SIZE:
        raise ValueError("Installer size must be between 1 byte and 1 GiB")
    return file_digest(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "stage", "publish", "verify-github", "verify"))
    parser.add_argument("--source", type=Path, help="stage: directory with downloaded release artifacts")
    parser.add_argument("--output", type=Path, help="stage: new or empty downloads directory")
    parser.add_argument("--installer", type=Path, help="verify: the published installer")
    args = parser.parse_args()
    ref_type = os.environ.get("GITHUB_REF_TYPE", "")
    ref_name = os.environ.get("GITHUB_REF_NAME", "")
    version = tag_version(ref_type, ref_name)
    if args.command == "prepare":
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write(f"version={version}\n")
        return
    if not version:
        raise ValueError("Release publication requires a version tag")
    if args.command in ("publish", "verify-github"):
        if args.source is None:
            raise ValueError("--source is required")
        operation = publish_release if args.command == "publish" else verify_github_release
        operation(args.source, ref_name, version)
        print("Verified the complete GitHub release asset inventory, sizes and SHA-256s")
        return
    origin = validate_origin(os.environ.get("RELEASE_ORIGIN", ""))
    if args.command == "stage":
        if args.source is None or args.output is None:
            raise ValueError("--source and --output are required")
        installer, archives = stage_downloads(args.source, args.output, version)
        text = manifest_text(ref_name, version, installer.stat().st_size, checked_installer(installer, version), archives)
        if len(text.encode("utf-8")) > MAX_MANIFEST_SIZE:
            raise ValueError("Manifest exceeds 16 KiB")
        (args.output / MANIFEST_NAME).write_text(text, encoding="utf-8", newline="\n")
        print(f"Staged {len(list(args.output.iterdir()))} release files in {args.output}")
        return
    if args.installer is None:
        raise ValueError("--installer is required")
    verify_publish(origin, ref_name, version, args.installer, checked_installer(args.installer, version))
    print("Verified published installer version, URL, size and SHA-256")


if __name__ == "__main__":
    main()
