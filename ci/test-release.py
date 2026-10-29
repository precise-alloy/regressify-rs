"""Exercise the release workflow and Cargo arguments without building or publishing."""

import itertools
import hashlib
import io
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import urllib.error
import urllib.request
import zipfile
from unittest import mock

import release_feed
import yaml


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = yaml.load(
    (ROOT / ".github/workflows/tuyen-release.yml").read_text(encoding="utf-8"),
    Loader=yaml.BaseLoader,
)
WEBSITE_WORKFLOW = yaml.load(
    (ROOT / ".github/workflows/website-deploy.yml").read_text(encoding="utf-8"),
    Loader=yaml.BaseLoader,
)
# Steps that fetch and check the deployed release's CLI, in workflow order.
WEBSITE_CLI_STEPS = [
    "Download deployed release manifest",
    "Read deployed release tag and CLI checksum",
    "Download the deployed release's Linux CLI",
    "Verify the Linux CLI against the deployed feed",
    "Extract the Linux CLI",
]
# One `command -v` per tool: with several names it succeeds when any one resolves.
WEBSITE_BASH_PROBE = ('echo "${BASH_VERSINFO[0]}"; '
                      'for tool in sha256sum unzip; do command -v "$tool" >/dev/null || echo missing; done')


def bash_runs_website_steps(probe_output):
    """True when WEBSITE_BASH_PROBE output shows Bash 4+ with sha256sum and unzip."""
    lines = probe_output.split()
    return bool(lines) and lines[0].isdigit() and int(lines[0]) >= 4 and "missing" not in lines


def website_step_bash():
    """Bash that can run website-deploy steps, which target ubuntu-latest; None elsewhere.
    The macOS release checks have only Bash 3.2 and no GNU sha256sum."""
    bash = os.environ.get("BASH_PATH") or shutil.which("bash")
    if not bash:
        return None
    probe = subprocess.run([bash, "-c", WEBSITE_BASH_PROBE], capture_output=True, text=True, check=False)
    return bash if bash_runs_website_steps(probe.stdout) else None


MATRIX_STEP = next(
    step for step in WORKFLOW["jobs"]["set-matrix"]["steps"] if step.get("id") == "matrix"
)
TARGETS = {
    "MACOS_X86_64": "x86_64-apple-darwin",
    "MACOS_AARCH64": "aarch64-apple-darwin",
    "WINDOWS_X86_64": "x86_64-pc-windows-msvc",
    "LINUX_GNU_X86_64": "x86_64-unknown-linux-gnu",
}


class ReleaseTests(unittest.TestCase):
    def test_tag_release_targets_match_supported_browser_platforms(self):
        result, rows = self.matrix("both", [], event="push", ref_type="tag")
        self.assertEqual(result.returncode, 0, result.stderr)
        supported = {
            "x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc",
            "x86_64-apple-darwin", "aarch64-apple-darwin",
        }
        self.assertEqual({row["target"] for row in rows}, supported)
        self.assertEqual(len(rows), 8)
        self.assertEqual(set(release_feed.TARGETS), supported)
        self.assertFalse(any("musl" in name.lower() for name in WORKFLOW["on"]["workflow_dispatch"]["inputs"]))

    def test_snapshot_frontend_workflow_builds_release_with_locked_dependencies(self):
        workflow = yaml.load(
            (ROOT / ".github/workflows/snapshot-assets.yml").read_text(encoding="utf-8"),
            Loader=yaml.BaseLoader,
        )
        self.assertEqual(workflow["on"]["push"]["branches"], ["release"])
        self.assertEqual(workflow["on"]["push"]["paths"], ["frontend/**"])
        self.assertEqual(workflow["permissions"], {"contents": "read"})
        self.assertEqual(workflow["concurrency"]["group"], "snapshot-frontend-assets")
        steps = workflow["jobs"]["build"]["steps"]
        checkout = next(step for step in steps if step.get("uses", "").startswith("actions/checkout@"))
        self.assertIn("github.sha", checkout["with"]["ref"])
        self.assertIn("'release'", checkout["with"]["ref"])
        self.assertEqual(checkout["with"]["persist-credentials"], "false")
        commands = [step["run"] for step in steps if step.get("working-directory") == "frontend"]
        self.assertEqual(commands, ["bun install --frozen-lockfile", "bun run lint", "bun run test", "bun run build"])
        upload = next(step for step in steps if step.get("uses", "").startswith("actions/upload-artifact@"))
        self.assertEqual(upload["with"]["path"], "src/snapshot/data/")

    def test_snapshot_frontend_pr_contains_generated_assets_only(self):
        workflow = yaml.load(
            (ROOT / ".github/workflows/snapshot-assets.yml").read_text(encoding="utf-8"),
            Loader=yaml.BaseLoader,
        )
        job = workflow["jobs"]["pull-request"]
        self.assertEqual(job["needs"], "build")
        self.assertEqual(job["permissions"], {"contents": "write", "pull-requests": "write"})
        checkout = next(step for step in job["steps"] if step.get("uses", "").startswith("actions/checkout@"))
        self.assertEqual(checkout["with"]["ref"], "master")
        copy = next(step for step in job["steps"] if step.get("name") == "Replace generated assets only")
        self.assertIn('rsync -a --delete "$RUNNER_TEMP/snapshot-assets/" src/snapshot/data/', copy["run"])
        self.assertIn("git diff --cached --name-only", copy["run"])
        pr = next(step for step in job["steps"] if step.get("name") == "Open or update generated-assets PR")
        self.assertNotIn("uses", pr)
        self.assertEqual(pr["shell"], "bash")
        self.assertEqual(pr["env"]["GH_TOKEN"], "${{ secrets.SNAPSHOT_ASSETS_TOKEN || github.token }}")
        self.assertEqual(pr["env"]["GH_REPO"], "${{ github.repository }}")
        self.assertEqual(pr["env"]["SOURCE_SHA"], "${{ needs.build.outputs.source }}")
        self.assertIn("branch=automation/snapshot-frontend-assets", pr["run"])
        self.assertIn('gh pr create --repo "$GH_REPO" --base master --head "$branch"', pr["run"])
        self.assertIn('gh pr edit "$pr_number" --repo "$GH_REPO" --base master', pr["run"])
        self.assertIn('git push --force-with-lease="$ref:$remote_sha" origin "HEAD:$ref"', pr["run"])
        self.assertIn("git diff --cached --quiet", pr["run"])
        self.assertNotIn("gh pr merge", pr["run"])
        self.assertNotIn("${{", pr["run"])

    def snapshot_pr(self, *, changed=True, pr_number="", remote_sha="", failure=""):
        bash = shutil.which("bash")
        if not bash:
            self.skipTest("Bash is required for the offline snapshot workflow checks")
        workflow = yaml.load(
            (ROOT / ".github/workflows/snapshot-assets.yml").read_text(encoding="utf-8"),
            Loader=yaml.BaseLoader,
        )
        script = next(step["run"] for step in workflow["jobs"]["pull-request"]["steps"]
                      if step.get("name") == "Open or update generated-assets PR")
        stub = r'''
set -euo pipefail
export GH_TOKEN=synthetic-offline-token GH_REPO=example/report SOURCE_SHA=abc123
record() { printf '%s ' "$1" >&2; shift; printf '<%s>' "$@" >&2; printf '\n' >&2; }
git() {
  record git "$@"
  if [ "$1" = "$failure" ]; then return 23; fi
  case "$1" in
    ls-remote) if [ -n "$remote_sha_fixture" ]; then printf '%s\trefs/heads/automation/snapshot-frontend-assets\n' "$remote_sha_fixture"; fi ;;
    diff) return "$diff_status" ;;
    config|commit|push) ;;
    *) return 97 ;;
  esac
}
gh() {
  record gh "$@"
  if [ "$failure" = "gh" ]; then return 24; fi
  case "$1 $2" in
    'pr list') printf '%s' "$pr_number_fixture" ;;
    'pr create'|'pr edit'|'pr close') ;;
    *) return 98 ;;
  esac
}
'''
        # Feed the script through stdin so Windows checkout line endings and WSL
        # path conversion cannot change which workflow commands are exercised.
        values = f"diff_status={1 if changed else 0}\npr_number_fixture={pr_number}\nremote_sha_fixture={remote_sha}\nfailure={failure}\n"
        result = subprocess.run(
            [bash, "--noprofile", "--norc", "-s"],
            input=(values + stub + script).encode("utf-8"), capture_output=True,
            check=False, timeout=15,
        )
        return subprocess.CompletedProcess(
            result.args, result.returncode,
            result.stdout.decode("utf-8"), result.stderr.decode("utf-8"),
        )

    def test_snapshot_frontend_gh_creates_and_updates_prs_after_safe_push(self):
        for pr_number, remote_sha in [("", ""), ("42", "old-head")]:
            with self.subTest(pr_number=pr_number):
                result = self.snapshot_pr(pr_number=pr_number, remote_sha=remote_sha)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("git <commit><-m><build(snapshot): refresh generated frontend assets>", result.stderr)
                push = f"git <push><--force-with-lease=refs/heads/automation/snapshot-frontend-assets:{remote_sha}><origin><HEAD:refs/heads/automation/snapshot-frontend-assets>"
                command = "gh <pr><edit><42>" if pr_number else "gh <pr><create>"
                self.assertIn(push, result.stderr)
                self.assertIn(command, result.stderr)
                self.assertLess(result.stderr.index(push), result.stderr.index(command))
                self.assertIn("<--base><master>", result.stderr)
                self.assertIn("release commit `abc123`", result.stderr)
                self.assertNotIn("<merge>", result.stderr)
                self.assertNotIn("synthetic-offline-token", result.stderr)

    def test_snapshot_frontend_gh_skips_empty_updates_and_cleans_stale_prs(self):
        for pr_number, remote_sha in [("", ""), ("42", "old-head"), ("", "merged-head")]:
            with self.subTest(pr_number=pr_number, remote_sha=remote_sha):
                result = self.snapshot_pr(changed=False, pr_number=pr_number, remote_sha=remote_sha)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertNotIn("git <commit>", result.stderr)
                self.assertNotIn("gh <pr><create>", result.stderr)
                self.assertNotIn("gh <pr><edit>", result.stderr)
                self.assertEqual("gh <pr><close><42>" in result.stderr, bool(pr_number))
                self.assertEqual("git <push>" in result.stderr, bool(remote_sha))
                if remote_sha:
                    self.assertIn(f"<--force-with-lease=refs/heads/automation/snapshot-frontend-assets:{remote_sha}><origin><:refs/heads/automation/snapshot-frontend-assets>", result.stderr)

    def test_snapshot_frontend_gh_stops_on_lookup_commit_or_push_failure(self):
        for failure in ["gh", "ls-remote", "diff", "commit", "push"]:
            with self.subTest(failure=failure):
                result = self.snapshot_pr(pr_number="42", remote_sha="old-head", failure=failure)
                self.assertEqual(result.returncode, 24 if failure == "gh" else 23, result.stderr)
                for action in ["create", "edit", "close"]:
                    self.assertNotIn(f"gh <pr><{action}>", result.stderr)
                if failure != "push":
                    self.assertNotIn("git <push>", result.stderr)

    def matrix(self, build, selected, event="workflow_dispatch", ref_type="branch"):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            env = dict(
                os.environ, BUILD=build, GITHUB_OUTPUT=str(output),
                GITHUB_EVENT_NAME=event, GITHUB_REF_TYPE=ref_type,
            )
            env.update({key: str(key in selected).lower() for key in TARGETS})
            result = subprocess.run(
                [sys.executable, "-c", MATRIX_STEP["run"]],
                env=env, capture_output=True, text=True, check=False,
            )
            rows = None
            self.outputs = {}
            if output.exists():
                records = dict(line.split("=", 1) for line in output.read_text(encoding="utf-8").splitlines())
                self.outputs = records
                rows = json.loads(records["matrix"])["include"]
                self.assertEqual(records["installer"], str(any(row["installer"] for row in rows)).lower())
            return result, rows

    def test_tag_push_builds_and_publishes_every_platform(self):
        result, rows = self.matrix("cli", [], event="push", ref_type="tag")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            {(row["application"], row["target"]) for row in rows},
            {(application, target) for target in TARGETS.values() for application in ("cli", "desktop")},
        )
        self.assertEqual({row["target"] for row in rows if row["installer"]}, {"x86_64-pc-windows-msvc"})
        self.assertEqual(self.outputs["publish"], "true")
        self.assertNotIn("regressify", json.dumps(rows))

    def test_manual_runs_never_publish(self):
        for ref_type in ("branch", "tag"):
            with self.subTest(ref_type=ref_type):
                result, _ = self.matrix("both", ["WINDOWS_X86_64"], ref_type=ref_type)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.outputs["publish"], "false")

    def test_manual_desktop_also_selects_companion_cli(self):
        result, rows = self.matrix("desktop", ["WINDOWS_X86_64"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            {(row["application"], row["target"]) for row in rows},
            {("cli", "x86_64-pc-windows-msvc"), ("desktop", "x86_64-pc-windows-msvc")},
        )

    def test_matrix_job_does_not_load_secrets(self):
        self.assertNotIn(
            "secrets.", json.dumps(WORKFLOW["jobs"]["set-matrix"]),
            "Release planning must not load secrets that can cause GitHub to suppress its job outputs",
        )

    def test_tagged_source_and_serial_publish(self):
        self.assertIn("v*.*.*", WORKFLOW["on"]["push"]["tags"])
        self.assertEqual(WORKFLOW["concurrency"]["cancel-in-progress"], "false")
        self.assertNotIn("github.ref", WORKFLOW["concurrency"]["group"])
        for job in WORKFLOW["jobs"].values():
            for step in job["steps"]:
                if step.get("uses", "").startswith("actions/checkout@"):
                    self.assertNotEqual(step.get("with", {}).get("ref"), "release")

    def test_windows_installer_is_tag_gated_and_publishing_keeps_tokens_out_of_builds(self):
        job = WORKFLOW["jobs"]["installer"]
        self.assertEqual(job["needs"], ["set-matrix", "release"])
        self.assertEqual(job["if"], "${{ needs.set-matrix.outputs.installer == 'true' }}")
        self.assertEqual(job["runs-on"], "windows-latest")
        self.assertEqual(WORKFLOW["jobs"]["set-matrix"]["outputs"]["installer"], "${{ steps.matrix.outputs.installer }}")
        self.assertEqual(WORKFLOW["jobs"]["set-matrix"]["outputs"]["publish"], "${{ steps.matrix.outputs.publish }}")
        self.assertEqual(WORKFLOW["env"]["RELEASE_ORIGIN"], "https://regressify.eshn.dev")
        self.assertEqual(WORKFLOW["permissions"], {"contents": "read"})
        steps = job["steps"]
        builds = WORKFLOW["jobs"]["release"]["steps"]
        build = next(step for step in builds if step.get("name") == "Build Windows desktop release")
        self.assertEqual(build["if"], "${{ matrix.installer && matrix.application == 'desktop' }}")
        self.assertEqual(build["env"]["REGRESSIFY_DESKTOP_VERSION"], "${{ needs.set-matrix.outputs.version }}")
        self.assertEqual(build["env"]["REGRESSIFY_UPDATE_ORIGIN"], "${{ env.RELEASE_ORIGIN }}")
        self.assertIn("+crt-static", build["env"]["RUSTFLAGS"])
        cli = next(step for step in builds if step.get("name") == "Build Windows CLI release")
        self.assertEqual(cli["if"], "${{ matrix.installer && matrix.application == 'cli' }}")
        self.assertIn("cargo build --locked --no-default-features --bin regressify --release --target x86_64-pc-windows-msvc", cli["run"])
        self.assertIn("if ($LASTEXITCODE -ne 0)", cli["run"])
        self.assertEqual(cli["env"]["BINARY_GIT_HASH"], "${{ github.sha }}")
        self.assertIn("+crt-static", cli["env"]["RUSTFLAGS"])
        self.assertNotIn("secrets.", json.dumps(WORKFLOW["jobs"]["release"]))
        self.assertNotIn("secrets.", json.dumps(job))
        payload = next(step for step in builds if step.get("name") == "Retain installer payload")
        self.assertEqual(payload["if"], "${{ matrix.installer }}")
        self.assertEqual(payload["with"]["name"], "regressify-installer-${{ matrix.application }}")
        self.assertEqual(payload["with"]["path"], "target/x86_64-pc-windows-msvc/release/${{ matrix.application == 'desktop' && 'regressify-desktop' || 'regressify' }}.exe")
        self.assertEqual(payload["with"]["if-no-files-found"], "error")
        archive = next(step for step in builds if step.get("name") == "Archive installer payload")
        self.assertEqual(archive["if"], "${{ matrix.installer }}")
        self.assertEqual(archive["env"]["SKIP_BUILD"], "1")
        self.assertLess(builds.index(payload), builds.index(archive), "Archive only after retaining the uncompressed payload")
        archives = next(step for step in builds if step.get("name") == "Retain release archives")
        self.assertNotIn("if", archives)
        self.assertEqual(archives["with"]["name"], "release-${{ matrix.application }}-${{ matrix.target }}")
        self.assertEqual(archives["with"]["if-no-files-found"], "error")
        self.assertLess(builds.index(archive), builds.index(archives))
        downloads = [step for step in steps if step.get("uses", "").startswith("actions/download-artifact@")]
        self.assertEqual(
            {step["with"]["name"] for step in downloads},
            {
                "regressify-installer-cli",
                "regressify-installer-desktop",
                "release-cli-x86_64-pc-windows-msvc",
                "release-desktop-x86_64-pc-windows-msvc",
            },
        )
        package = next(step for step in steps if step.get("name") == "Build Windows installer")
        self.assertIn('$env:ISCC_PATH = $compiler', package["run"])
        self.assertIn('python ci/test-release.py InstallerTests', package["run"])
        self.assertIn('./ci/build-installer.ps1', package["run"])
        for download in downloads:
            self.assertEqual(download["with"]["path"], "target/x86_64-pc-windows-msvc/release")
            self.assertLess(steps.index(download), steps.index(package))
        retain = next(step for step in steps if step.get("name") == "Retain Windows installer")
        self.assertEqual(retain["with"]["name"], "regressify-windows-${{ needs.set-matrix.outputs.version }}")
        self.assertLess(steps.index(package), steps.index(retain))
        self.assertNotIn("tuyen", json.dumps(WORKFLOW).lower().replace("tuyen-release", ""))

        publish = WORKFLOW["jobs"]["publish"]
        self.assertEqual(publish["needs"], ["set-matrix", "checks", "release", "installer"])
        self.assertIn("needs.set-matrix.outputs.publish == 'true'", publish["if"])
        self.assertIn("github.repository == 'precise-alloy/regressify-rs'", publish["if"])
        self.assertEqual(publish["permissions"], {"contents": "write"})
        steps = publish["steps"]
        self.assertEqual(steps[0]["with"], {"persist-credentials": "false"})
        names = [
            "Download release archives",
            "Download Windows installer",
            "Stage downloads and update manifest",
            "Publish GitHub Release",
            "Retain verified release inputs",
        ]
        by_name = {step.get("name"): step for step in steps}
        indexes = [steps.index(by_name[name]) for name in names]
        self.assertEqual(indexes, sorted(indexes))
        self.assertEqual(by_name["Download release archives"]["with"]["pattern"], "release-*")
        self.assertEqual(by_name["Download release archives"]["with"]["merge-multiple"], "false")
        self.assertEqual(by_name["Download Windows installer"]["with"]["name"],
                         "regressify-windows-${{ needs.set-matrix.outputs.version }}")
        self.assertIn("ci/release_feed.py stage", by_name["Stage downloads and update manifest"]["run"])
        publish_gh = by_name["Publish GitHub Release"]
        self.assertEqual(publish_gh["env"]["GH_TOKEN"], "${{ github.token }}")
        self.assertIn("ci/release_feed.py publish", publish_gh["run"])
        deploy = WORKFLOW["jobs"]["deploy"]
        self.assertEqual(deploy["needs"], ["set-matrix", "publish"])
        self.assertNotIn("permissions", deploy)
        steps = deploy["steps"]
        by_name = {step.get("name"): step for step in steps}
        verify = by_name["Verify GitHub release before building website"]
        self.assertIn("ci/release_feed.py verify-github", verify["run"])
        self.assertLess(steps.index(verify), steps.index(by_name["Deploy Cloudflare Worker"]))
        self.assertIn("$RUNNER_TEMP/downloads/regressify-cli-x86_64-unknown-linux-gnu.zip", by_name["Extract released Linux CLI"]["run"])
        run = by_name["Deploy Cloudflare Worker"]
        self.assertEqual(run["run"], "bash ci/deploy-cloudflare.sh")
        self.assertEqual(run["env"]["CLOUDFLARE_ACCOUNT_ID"], "${{ secrets.CLOUDFLARE_ACCOUNT_ID }}")
        self.assertEqual(run["env"]["CLOUDFLARE_API_TOKEN"], "${{ secrets.CLOUDFLARE_API_TOKEN }}")
        self.assertEqual(run["env"]["DOWNLOADS_DIR"], "${{ runner.temp }}/downloads")
        self.assertIn("ci/release_feed.py verify", by_name["Verify published update feed"]["run"])
        self.assertIn("Regressify-Setup-$INSTALLER_VERSION-x64.exe", by_name["Verify published update feed"]["run"])
        for step in steps:
            if step is not run:
                self.assertNotIn("secrets.", json.dumps(step))
        self.assertNotIn("PAT_TOKEN", json.dumps(WORKFLOW))
        self.assertEqual(WORKFLOW["jobs"]["release"]["needs"], "set-matrix")
        self.assertNotIn("needs", WORKFLOW["jobs"]["checks"])
        for job_name in ("release", "installer", "publish", "deploy"):
            for step in WORKFLOW["jobs"][job_name]["steps"]:
                self.assertNotIn("continue-on-error", step)
                self.assertNotIn("always()", step.get("if", ""))
        installer = (ROOT / "installer/regressify.iss").read_text(encoding="utf-8")
        updater = (ROOT / "src/desktop/updates.rs").read_text(encoding="utf-8")
        for setting in ("AppRunningMutex = '", "SetupRunningMutex = '"):
            mutex = next(
                line.split(setting, 1)[1].split("'", 1)[0]
                for line in installer.splitlines()
                if setting in line
            )
            self.assertIn(mutex.replace("\\", "\\\\"), updater)
        self.assertIn("PrivilegesRequired=lowest", installer)
        self.assertIn("CloseApplications=no", installer)
        self.assertIn("if CheckForMutexes(", installer)
        self.assertNotIn("[UninstallDelete]", installer)

    def test_windows_installer_foreground_handoff_is_smoke_tested(self):
        step = next(step for step in WORKFLOW["jobs"]["installer"]["steps"]
                    if step.get("name") == "Build Windows installer")
        self.assertIn("-SmokeTest", step["run"])
        installer = (ROOT / "installer/regressify.iss").read_text(encoding="utf-8")
        self.assertIn("procedure CurStepChanged(CurStep: TSetupStep);", installer)
        self.assertIn("(CurStep = ssInstall) and WizardForm.Visible", installer)
        self.assertIn("WizardForm.BringToFront;", installer)
        self.assertIn("SetForegroundWindow(WizardForm.Handle)", installer)
        smoke = (ROOT / "ci/build-installer.ps1").read_text(encoding="utf-8")
        self.assertIn("Fully silent installation attempted foreground activation.", smoke)
        self.assertIn("Visible update did not request foreground activation after the app closed.", smoke)

    def test_windows_installer_prepares_chromium_with_run_time_fallback(self):
        installer = (ROOT / "installer/regressify.iss").read_text(encoding="utf-8")
        desktop = (ROOT / "src/bin/desktop.rs").read_text(encoding="utf-8")
        smoke = (ROOT / "ci/build-installer.ps1").read_text(encoding="utf-8")
        self.assertIn("BeforeInstall: PrepareChromium", installer)
        self.assertIn("RegisterCliPath(ExpandConstant('{app}'));\n    CleanupPreviousInstall(ExpandConstant('{app}'), ExpandConstant('{groupname}'));\n    PrepareChromium;", installer)
        self.assertIn("if ChromiumPrepared then", installer)
        self.assertIn("{param:SKIPCHROMIUM|0}", installer)
        self.assertIn("ExecAndLogOutput(", installer)
        self.assertIn("'--install-chromium'", installer)
        self.assertIn("ewWaitUntilTerminated, ResultCode, @ChromiumInstallLog", installer)
        self.assertIn("app installation remains successful and Reference/Test can retry", installer)
        self.assertIn("if not WizardSilent then", installer)
        self.assertLess(desktop.index("if args.install_chromium"), desktop.index("protect_installation(args.after_install)"))
        self.assertIn("block_on(regressify::test::fetcher())", desktop)
        self.assertIn("Assert-ChromiumPreparation $installLog", smoke)
        self.assertIn("Assert-ChromiumPreparation $foregroundLog", smoke)
        self.assertIn("http://127.0.0.1:9", smoke)
        self.assertIn("'/SKIPCHROMIUM=1'", smoke)

    def test_windows_installer_shows_chromium_progress_for_visible_updates(self):
        installer = (ROOT / "installer/regressify.iss").read_text(encoding="utf-8")
        updater = (ROOT / "src/desktop/updates.rs").read_text(encoding="utf-8")
        smoke = (ROOT / "ci/build-installer.ps1").read_text(encoding="utf-8")
        preparation = installer.split("procedure PrepareChromium;", 1)[1].split("Started := False;", 1)[0]
        self.assertIn('command.args(["/SP-", "/SILENT", "/NORESTART"]);', updater)
        self.assertIn("if WizardForm.Visible then", preparation)
        self.assertNotIn("WizardSilent", preparation)
        self.assertIn("ChromiumProgress.Show;", preparation)
        self.assertIn("ChromiumProgress.Animate;", preparation)
        self.assertIn("ChromiumProgress.SetText(S, '');", installer)
        self.assertIn("Log('Chromium progress: ' + S);", installer)
        self.assertIn("Assert-ChromiumPreparation $installLog $false", smoke)
        self.assertIn("Assert-ChromiumPreparation $foregroundLog $true", smoke)
        self.assertIn("Chromium progress: Checking Chromium cache", smoke)

    def test_deploy_stages_downloads_into_the_built_site_before_deploying(self):
        website = json.loads((ROOT / "website/package.json").read_text(encoding="utf-8"))
        node = next(step for step in WORKFLOW["jobs"]["deploy"]["steps"]
                    if step.get("uses", "").startswith("actions/setup-node@"))
        minimum_node = website["engines"]["node"]
        node_version = node["with"]["node-version"]
        self.assertRegex(minimum_node, r"^>=[0-9]+\.[0-9]+\.[0-9]+$")
        self.assertRegex(node_version, r"^[0-9]+\.[0-9]+\.[0-9]+$")
        self.assertEqual(release_feed.version_parts(node_version)[0], 24)
        self.assertGreaterEqual(release_feed.version_parts(node_version),
                                release_feed.version_parts(minimum_node.removeprefix(">=")))
        script = (ROOT / "ci/deploy-cloudflare.sh").read_text(encoding="utf-8")
        order = ['[ -f "$DOWNLOADS_DIR/latest.txt" ]', "bun install --frozen-lockfile", "bun run check",
                 'RELEASE_REQUIRED=1 RELEASE_MANIFEST_FILE="$downloads/latest.txt" bun run build',
                 '[ ! -e "$website/dist/downloads" ]', 'cp "$downloads/latest.txt" "$website/dist/downloads/latest.txt"',
                 '"$REGRESSIFY_BIN" mcp --output "$worker"', "npm ci", "npm run check", "npm test",
                 "bun run worker:check", "bun run worker:build",
                 "CLOUDFLARE_ACCOUNT_ID", "CLOUDFLARE_API_TOKEN", 'wrangler" deploy --config wrangler.jsonc']
        indexes = [script.index(item) for item in order]
        self.assertEqual(indexes, sorted(indexes))
        self.assertNotIn("set -x", script)
        config = json.loads((ROOT / "website/wrangler.jsonc").read_text(encoding="utf-8"))
        template = json.loads((ROOT / "src/mcp/worker/wrangler.jsonc.template").read_text(encoding="utf-8"))
        for key in ("compatibility_date", "compatibility_flags", "minify", "upload_source_maps"):
            self.assertEqual(config[key], template[key], key)
        self.assertEqual(config["name"], "regressify")
        self.assertEqual(config["main"], "worker/index.ts")
        self.assertEqual(config["tsconfig"], "tsconfig.worker.json")
        self.assertFalse(config["workers_dev"])
        self.assertEqual(config["assets"], {"binding": "ASSETS", "directory": "dist", "not_found_handling": "404-page", "run_worker_first": ["/mcp", "/admin", "/admin/*"]})
        self.assertFalse((ROOT / "website/public/downloads").exists(), "Release downloads must only come from CI staging")
        self.assertTrue((ROOT / "website/public/install.ps1").is_file())
        self.assertTrue((ROOT / "website/public/install.sh").is_file())
        self.assertIn("mcp-worker/", (ROOT / "website/.gitignore").read_text(encoding="utf-8"))
        self.assertIn("https://regressify.eshn.dev/mcp", (ROOT / "src/init.rs").read_text(encoding="utf-8"))
        self.assertIn("'https://regressify.eshn.dev'", (ROOT / "website/src/lib/release.ts").read_text(encoding="utf-8"))

    def test_selector_and_parallel_job_contract(self):
        selector = WORKFLOW["on"]["workflow_dispatch"]["inputs"]["build"]
        self.assertEqual(selector["type"], "choice")
        self.assertEqual(selector["default"], "both")
        self.assertEqual(set(selector["options"]), {"cli", "desktop", "both"})
        self.assertEqual(MATRIX_STEP["shell"], "python")
        self.assertEqual(MATRIX_STEP["env"]["BUILD"], "${{ inputs.build }}")
        for variable, input_name in [
            ("MACOS_X86_64", "macosX86_64"), ("MACOS_AARCH64", "macosAarch64"),
            ("WINDOWS_X86_64", "windowsX86_64"), ("LINUX_GNU_X86_64", "linuxGnuX86_64"),
        ]:
            self.assertEqual(MATRIX_STEP["env"][variable], "${{ inputs." + input_name + " }}")
        job = WORKFLOW["jobs"]["release"]
        self.assertEqual(job["needs"], "set-matrix")
        self.assertNotIn("timeout-minutes", job)
        self.assertEqual(job["strategy"]["fail-fast"], "false")
        self.assertNotIn("max-parallel", job["strategy"])
        self.assertEqual(job["strategy"]["matrix"], "${{ fromJson(needs.set-matrix.outputs.matrix) }}")
        build = next(step for step in job["steps"] if step.get("name") == "Build artifacts")
        self.assertEqual(build["if"], "${{ !matrix.installer }}")
        self.assertEqual(build["env"]["FILE_NAME"], "${{ matrix.application == 'desktop' && 'regressify-desktop' || 'regressify' }}")
        self.assertEqual(build["env"]["PACKAGE_NAME"], "${{ matrix.application == 'desktop' && 'regressify-desktop' || 'regressify-cli' }}")
        self.assertEqual(build["env"]["CARGO_FEATURES"], "${{ matrix.features }}")
        self.assertEqual(build["env"]["NO_DEFAULT_FEATURES"], "1")
        cache = next(step for step in job["steps"] if step.get("uses", "").startswith("actions/cache@"))
        self.assertIn("matrix.application", cache["with"]["key"])

    def test_every_application_and_platform_selection(self):
        for build in ("cli", "desktop", "both"):
            applications = ("cli",) if build == "cli" else ("cli", "desktop")
            for flags in itertools.product((False, True), repeat=len(TARGETS)):
                selected = [key for key, enabled in zip(TARGETS, flags) if enabled]
                with self.subTest(build=build, selected=selected):
                    targets = [TARGETS[key] for key in selected] or [TARGETS["LINUX_GNU_X86_64"]]
                    expected = {
                        (application, target)
                        for application in applications for target in targets
                    }
                    result, rows = self.matrix(build, selected)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual({(row["application"], row["target"]) for row in rows}, expected)
                    self.assertEqual(len(rows), len(expected))
                    self.assertNotIn("regressify", json.dumps(rows))
                    for row in rows:
                        desktop = row["application"] == "desktop"
                        self.assertNotIn("binary", row)
                        self.assertNotIn("package", row)
                        self.assertEqual(row["features"], "gui" if desktop else "")
                        self.assertNotIn("timeout", row)
                        if row["target"] == "x86_64-unknown-linux-gnu":
                            self.assertEqual(row["os"], "ubuntu-24.04" if desktop else "ubuntu-22.04")
                        if row["target"] == "x86_64-apple-darwin":
                            self.assertEqual(row["os"], "macos-15-intel")
                        if row["target"] == "aarch64-apple-darwin":
                            self.assertEqual(row["os"], "macos-15")

    def test_missing_selector_defaults_to_both_and_unknown_values_fail(self):
        result, rows = self.matrix("", [])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual({row["application"] for row in rows}, {"cli", "desktop"})
        result, rows = self.matrix("invalid", [])
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(rows)

    def test_installer_payloads_require_a_tagged_windows_desktop_selection(self):
        for build, ref_type, selected in itertools.product(
            ("cli", "desktop", "both"), ("branch", "tag"), ([], ["WINDOWS_X86_64"], ["WINDOWS_X86_64", "MACOS_AARCH64"]),
        ):
            with self.subTest(build=build, ref_type=ref_type, selected=selected):
                result, rows = self.matrix(build, selected, ref_type=ref_type)
                self.assertEqual(result.returncode, 0, result.stderr)
                payloads = [row for row in rows if row["installer"]]
                expected = {"cli", "desktop"} if ref_type == "tag" and build != "cli" and selected else set()
                self.assertEqual({row["application"] for row in payloads}, expected)
                self.assertTrue(all(row["target"] == "x86_64-pc-windows-msvc" for row in payloads))

    def test_website_workflow_redeploys_the_deployed_release(self):
        self.assertEqual(WEBSITE_WORKFLOW["on"]["push"]["branches"], ["master"])
        self.assertIn("website/**", WEBSITE_WORKFLOW["on"]["push"]["paths"])
        self.assertIn("workflow_dispatch", WEBSITE_WORKFLOW["on"])
        self.assertEqual(WEBSITE_WORKFLOW["permissions"], {"contents": "read"})
        self.assertEqual(WEBSITE_WORKFLOW["defaults"], {"run": {"shell": "bash"}})
        self.assertEqual(set(WEBSITE_WORKFLOW["jobs"]), {"deploy"})
        deploy = WEBSITE_WORKFLOW["jobs"]["deploy"]
        self.assertIn("github.repository == 'precise-alloy/regressify-rs'", deploy["if"])
        self.assertEqual(deploy["env"], {"SITE_ORIGIN": "https://regressify.eshn.dev",
                                         "CLI_ARCHIVE": "regressify-cli-x86_64-unknown-linux-gnu.zip"})
        steps = deploy["steps"]
        self.assertEqual(steps[0]["with"], {"persist-credentials": "false"})
        by_name = {step.get("name"): step for step in steps}
        names = WEBSITE_CLI_STEPS + ["Deploy Cloudflare Worker", "Verify update feed is unchanged"]
        indexes = [steps.index(by_name[name]) for name in names]
        self.assertEqual(indexes, sorted(indexes))
        self.assertEqual(by_name[WEBSITE_CLI_STEPS[1]]["id"], "release")
        self.assertEqual(by_name[WEBSITE_CLI_STEPS[2]]["env"], {
            "GH_TOKEN": "${{ github.token }}", "TAG": "${{ steps.release.outputs.tag }}"})
        self.assertEqual(by_name[WEBSITE_CLI_STEPS[3]]["env"], {"SHA256": "${{ steps.release.outputs.cli-sha256 }}"})
        self.assertIn("sha256sum --check --strict", by_name[WEBSITE_CLI_STEPS[3]]["run"])
        run = by_name["Deploy Cloudflare Worker"]
        self.assertEqual(run["run"], "bash ci/deploy-cloudflare.sh")
        self.assertEqual(run["env"]["CLOUDFLARE_ACCOUNT_ID"], "${{ secrets.CLOUDFLARE_ACCOUNT_ID }}")
        self.assertEqual(run["env"]["CLOUDFLARE_API_TOKEN"], "${{ secrets.CLOUDFLARE_API_TOKEN }}")
        self.assertEqual(run["env"]["DOWNLOADS_DIR"], "${{ runner.temp }}/downloads")
        self.assertEqual(run["env"]["REGRESSIFY_BIN"], "${{ runner.temp }}/cli/regressify")
        self.assertIn('cmp "$RUNNER_TEMP/downloads/latest.txt"', by_name["Verify update feed is unchanged"]["run"])
        for step in steps:
            self.assertNotIn("continue-on-error", step)
            self.assertNotIn("always()", step.get("if", ""))
            # Values from the feed reach scripts through env, never as expanded script text.
            self.assertNotIn("${{", step.get("run", ""))
            if step is not run:
                self.assertNotIn("secrets.", json.dumps(step))

    def run_website_steps(self, bash, env):
        """Runs WEBSITE_CLI_STEPS like GitHub: one `bash -eo pipefail` process per step with
        job and step env, step outputs via GITHUB_OUTPUT, stopping at the first failure."""
        job = WEBSITE_WORKFLOW["jobs"]["deploy"]
        steps = {step.get("name"): step for step in job["steps"]}
        outputs = {}
        ran = []
        prefix = 'if command -v cygpath >/dev/null; then STUB_DIR="$(cygpath -u "$STUB_DIR")"; fi\n'
        prefix += 'export PATH="$STUB_DIR:$PATH"\n'

        def expression(match):
            value = match.group(1)
            if value == "github.token":
                return "synthetic-token"
            step_id, _, name = value.removeprefix("steps.").partition(".outputs.")
            self.assertTrue(value.startswith("steps.") and name, value)
            return outputs.get(step_id, {}).get(name, "")

        for index, name in enumerate(WEBSITE_CLI_STEPS):
            step = steps[name]
            output = Path(env["RUNNER_TEMP"]) / f"github-output-{index}"
            step_env = {key: re.sub(r"\$\{\{\s*(.+?)\s*\}\}", expression, value)
                        for key, value in step.get("env", {}).items()}
            result = subprocess.run(
                [bash, "--noprofile", "--norc", "-eo", "pipefail", "-c", prefix + step["run"]], cwd=ROOT,
                env={**env, **job["env"], **step_env, "GITHUB_OUTPUT": output.as_posix()},
                capture_output=True, text=True, check=False)
            ran.append(name)
            if result.returncode != 0:
                return ran, outputs, result
            if "id" in step and output.exists():
                lines = output.read_text(encoding="utf-8").splitlines()
                outputs[step["id"]] = dict(line.split("=", 1) for line in lines)
        return ran, outputs, result

    def test_website_workflow_extracts_the_deployed_release_cli(self):
        bash = website_step_bash()
        if not bash:
            self.skipTest("Website deploy steps need Bash 4+ with sha256sum and unzip (ubuntu-latest)")
        archive_name = "regressify-cli-x86_64-unknown-linux-gnu.zip"
        stub_sources = {
            "curl": (
                "#!/usr/bin/env bash\n"
                'printf "curl %s\\n" "$*" >> "$CALL_LOG"\n'
                'while [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done\n'
                'mkdir -p "$(dirname "$out")"\n'
                'cp "$FIXTURES/latest.txt" "$out"\n'
            ),
            # Only the deployed tag exists, like `gh release download` on a real repository.
            "gh": (
                "#!/usr/bin/env bash\n"
                'printf "gh %s\\n" "$*" >> "$CALL_LOG"\n'
                '[ "$3" = v1.2.3 ] || { echo "release not found" >&2; exit 1; }\n'
                'cp "$FIXTURES/archive.zip" "$9/$7"\n'
            ),
        }
        # The step each broken feed must stop at; None means every step succeeds.
        cases = {
            "valid": None,
            "crlf": None,
            "missing-tag": WEBSITE_CLI_STEPS[1],
            "unknown-tag": WEBSITE_CLI_STEPS[2],
            "bad-digest": WEBSITE_CLI_STEPS[3],
            "missing-archive": WEBSITE_CLI_STEPS[3],
        }
        for case, failing_step in cases.items():
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                stubs = root / "stubs"
                fixtures = root / "fixtures"
                runner = root / "runner"
                for path in (stubs, fixtures, runner):
                    path.mkdir()
                for name, source in stub_sources.items():
                    (stubs / name).write_text(source, encoding="utf-8", newline="\n")
                    (stubs / name).chmod(0o755)
                with zipfile.ZipFile(fixtures / "archive.zip", "w") as zf:
                    zf.writestr("regressify", "synthetic cli")
                    zf.writestr("LICENSE", "AGPL")
                data = (fixtures / "archive.zip").read_bytes()
                digest = "0" * 64 if case == "bad-digest" else hashlib.sha256(data).hexdigest().upper()
                archives = [] if case == "missing-archive" else [(archive_name, len(data), digest)]
                manifest = release_feed.manifest_text("v1.2.3", "1.2.3.0", 22, "a" * 64, archives)
                if case == "crlf":
                    manifest = manifest.replace("\n", "\r\n")
                elif case == "missing-tag":
                    manifest = manifest.replace("tag=v1.2.3\n", "")
                elif case == "unknown-tag":
                    manifest = manifest.replace("tag=v1.2.3", "tag=v9.9.9")
                (fixtures / "latest.txt").write_bytes(manifest.encode("utf-8"))
                log = root / "calls.log"
                env = dict(os.environ, STUB_DIR=stubs.as_posix(), FIXTURES=fixtures.as_posix(),
                           RUNNER_TEMP=runner.as_posix(), CALL_LOG=log.as_posix())
                ran, outputs, result = self.run_website_steps(bash, env)
                calls = log.read_text(encoding="utf-8").splitlines()
                self.assertTrue(calls[0].startswith("curl "))
                self.assertTrue(calls[0].endswith(" https://regressify.eshn.dev/downloads/latest.txt"))
                self.assertEqual((runner / "downloads/latest.txt").read_bytes(), manifest.encode("utf-8"))
                if failing_step is None:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(ran, WEBSITE_CLI_STEPS)
                    self.assertEqual(outputs["release"], {"tag": "v1.2.3", "cli-sha256": digest})
                    self.assertEqual(calls[1:], [
                        f"gh release download v1.2.3 --repo precise-alloy/regressify-rs "
                        f"--pattern {archive_name} --dir {runner.as_posix()}"])
                    self.assertEqual((runner / "cli/regressify").read_text(encoding="utf-8"), "synthetic cli")
                    continue
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(ran[-1], failing_step, result.stderr)
                # A rejected CLI is never extracted, so it can never run.
                self.assertFalse((runner / "cli").exists())
                if case == "missing-tag":
                    # Never fall back to gh's implicit "latest release" download.
                    self.assertEqual(len(calls), 1)
                    self.assertIn("has no tag", result.stderr)

    def test_website_step_bash_requires_bash_4_and_gnu_tools(self):
        # "3" is macOS Bash 3.2; "missing" means sha256sum or unzip is absent.
        for output in ["3\n", "5\nmissing\n", "", "x\n"]:
            with self.subTest(output=output):
                self.assertFalse(bash_runs_website_steps(output))
        self.assertTrue(bash_runs_website_steps("5\n"))
        bash = website_step_bash()
        if bash:
            probe = subprocess.run([bash, "-c", WEBSITE_BASH_PROBE], capture_output=True, text=True, check=False)
            self.assertTrue(bash_runs_website_steps(probe.stdout), probe.stdout)

    def test_website_step_bash_rejects_a_path_missing_one_tool(self):
        bash = website_step_bash()
        if not bash:
            self.skipTest("Website deploy steps need Bash 4+ with sha256sum and unzip (ubuntu-latest)")
        for tools in [(), ("sha256sum",), ("unzip",), ("sha256sum", "unzip")]:
            with self.subTest(tools=tools), tempfile.TemporaryDirectory() as directory:
                # Stubs only need to resolve through `command -v`; the probe never runs them.
                for name in tools:
                    stub = Path(directory) / name
                    stub.write_text("#!/bin/sh\n", encoding="utf-8", newline="\n")
                    stub.chmod(0o755)
                with mock.patch.dict(os.environ, {"BASH_PATH": bash, "PATH": directory}):
                    expected = bash if len(tools) == 2 else None
                    self.assertEqual(website_step_bash(), expected)

    def test_release_build_arguments(self):
        bash = os.environ.get("BASH_PATH") or shutil.which("bash")
        self.assertIsNotNone(bash, "Bash is required; on Windows set BASH_PATH to Git Bash")
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            stubs = temporary / "stubs"
            stubs.mkdir()
            for name, source in {
                "rustc": "#!/usr/bin/env bash\nprintf 'host: x86_64-unknown-linux-gnu\\n'\n",
                "cargo": '#!/usr/bin/env bash\nprintf "%s\\n" "$@" > "$BUILD_ARGS_FILE"\nexit 73\n',
            }.items():
                path = stubs / name
                path.write_text(source, encoding="utf-8", newline="\n")
                path.chmod(0o755)
            arguments = temporary / "arguments"
            env = dict(
                os.environ, TEST_MODE="1", GITHUB_REF="refs/tags/test", OS="ubuntu-22.04",
                TARGET="x86_64-unknown-linux-gnu", BINARY_GIT_HASH="test", SKIP_BUILD="",
                REPO="", TOKEN="", STUB_DIR=stubs.as_posix(), BUILD_ARGS_FILE=arguments.as_posix(),
            )
            script = 'if command -v cygpath >/dev/null; then STUB_DIR="$(cygpath -u "$STUB_DIR")"; fi\n'
            script += 'export PATH="$STUB_DIR:$PATH"\nbash ci/make-release.sh'
            for binary, features, no_defaults in [
                ("regressify", "", "1"), ("regressify-desktop", "gui", "1"),
                ("regressify-desktop", "gui", ""), ("regressify", "local,cloud", ""),
            ]:
                with self.subTest(binary=binary, features=features, no_defaults=no_defaults):
                    env.update(FILE_NAME=binary, PACKAGE_NAME=binary, CARGO_FEATURES=features, NO_DEFAULT_FEATURES=no_defaults)
                    result = subprocess.run([bash, "-c", script], cwd=ROOT, env=env, capture_output=True, text=True, check=False)
                    self.assertEqual(result.returncode, 73, result.stderr)
                    expected = ["build", "--locked", "--bin", binary, "--release", "--target", env["TARGET"]]
                    if no_defaults:
                        expected.append("--no-default-features")
                    if features:
                        expected.extend(["--features", features])
                    self.assertEqual(arguments.read_text(encoding="utf-8").splitlines(), expected)

    def test_release_archives_include_license_files(self):
        bash = os.environ.get("BASH_PATH") or shutil.which("bash")
        self.assertIsNotNone(bash, "Bash is required; on Windows set BASH_PATH to Git Bash")
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            stubs = temporary / "stubs"
            stubs.mkdir()
            for name, source in {
                "rustc": "#!/usr/bin/env bash\nprintf 'host: x86_64-unknown-linux-gnu\\n'\n",
                "upx": "#!/usr/bin/env bash\n",
                "7z": '#!/usr/bin/env bash\nprintf "%s\\n" "$*" >> "$ARCHIVE_ARGS_FILE"\n: > "$2"\n',
            }.items():
                path = stubs / name
                path.write_text(source, encoding="utf-8", newline="\n")
                path.chmod(0o755)
            release = temporary / "target/x86_64-unknown-linux-gnu/release"
            release.mkdir(parents=True)
            (release / "regressify").write_bytes(b"synthetic binary")
            arguments = temporary / "archive-arguments"
            env = dict(
                os.environ, TEST_MODE="1", GITHUB_REF="refs/tags/test", OS="ubuntu-22.04",
                TARGET="x86_64-unknown-linux-gnu", BINARY_GIT_HASH="test", SKIP_BUILD="1",
                REPO="", TOKEN="", FILE_NAME="regressify", PACKAGE_NAME="regressify-cli",
                CARGO_TARGET_DIR=(temporary / "target").as_posix(), STUB_DIR=stubs.as_posix(),
                ARCHIVE_ARGS_FILE=arguments.as_posix(),
            )
            script = 'if command -v cygpath >/dev/null; then\n'
            script += '  STUB_DIR="$(cygpath -u "$STUB_DIR")"; CARGO_TARGET_DIR="$(cygpath -u "$CARGO_TARGET_DIR")"\n'
            script += '  ARCHIVE_ARGS_FILE="$(cygpath -u "$ARCHIVE_ARGS_FILE")"\nfi\n'
            script += 'export PATH="$STUB_DIR:$PATH"\nbash ci/make-release.sh'
            for password in ("", "synthetic-archive-password"):
                with self.subTest(password=bool(password)):
                    arguments.unlink(missing_ok=True)
                    result = subprocess.run([bash, "-c", script], cwd=ROOT, env=dict(env, PASSWORD=password),
                                            capture_output=True, text=True, check=False)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    archives = [line.split()[:5] for line in arguments.read_text(encoding="utf-8").splitlines()]
                    self.assertEqual(archives, [
                        ["a", f"regressify-cli-{env['TARGET']}.{extension}", "regressify", "LICENSE", "LICENSING.md"]
                        for extension in ("7z", "zip")
                    ])
                    for name in ("LICENSE", "LICENSING.md"):
                        self.assertEqual((release / name).read_bytes(), (ROOT / name).read_bytes())


class InstallerTests(unittest.TestCase):
    def test_payload_preflight_requires_both_files_and_matching_desktop_version(self):
        shell = shutil.which("pwsh")
        self.assertIsNotNone(shell, "PowerShell 7 is required for installer checks")
        script = r'''
$ErrorActionPreference = 'Stop'
function Get-Item([string]$LiteralPath) {
    $file = Microsoft.PowerShell.Management\Get-Item -LiteralPath $LiteralPath
    [pscustomobject]@{
        FullName = $file.FullName
        PSIsContainer = $file.PSIsContainer
        VersionInfo = @{ FileVersion = $env:PAYLOAD_TEST_VERSION }
    }
}
$source = [System.IO.File]::ReadAllText('ci/build-installer.ps1')
$parsed = [ScriptBlock]::Create($source)
$boundary = $parsed.Ast.EndBlock.Statements | Where-Object { $_.Extent.Text.StartsWith('if (-not $Compiler)') } | Select-Object -First 1
if (-not $boundary) { throw 'Installer preflight boundary was not found.' }
$preflight = Join-Path $env:PAYLOAD_TEST_DIRECTORY 'preflight.ps1'
[System.IO.File]::WriteAllText($preflight, $source.Substring(0, $boundary.Extent.StartOffset))
& $preflight -Version '1.2.3.0' -BinaryDirectory $env:PAYLOAD_TEST_DIRECTORY
'''
        for desktop, cli, version, succeeds in [
            ("file", "file", "1.2.3.0", True), ("file", "missing", "1.2.3.0", False),
            ("missing", "file", "1.2.3.0", False), ("file", "directory", "1.2.3.0", False),
            ("directory", "file", "1.2.3.0", False), ("file", "file", "1.2.4.0", False),
        ]:
            with self.subTest(desktop=desktop, cli=cli, version=version), tempfile.TemporaryDirectory() as directory:
                for name, kind in [("regressify-desktop.exe", desktop), ("regressify.exe", cli)]:
                    path = Path(directory) / name
                    if kind == "file":
                        path.write_bytes(b"synthetic payload")
                    elif kind == "directory":
                        path.mkdir()
                result = subprocess.run(
                    [shell, "-NoProfile", "-NonInteractive", "-Command", script], cwd=ROOT,
                    env=dict(os.environ, PAYLOAD_TEST_DIRECTORY=directory, PAYLOAD_TEST_VERSION=version),
                    capture_output=True, text=True, check=False, timeout=15,
                )
                self.assertEqual(result.returncode == 0, succeeds, result.stderr)

    def test_static_runtime_is_required_for_each_payload(self):
        shell = shutil.which("pwsh")
        self.assertIsNotNone(shell, "PowerShell 7 is required for installer checks")
        script = r'''
$ErrorActionPreference = 'Stop'
$binary = [pscustomobject]@{ Name = 'regressify-desktop.exe'; FullName = 'regressify-desktop.exe' }
$cliBinary = [pscustomobject]@{ Name = 'regressify.exe'; FullName = 'regressify.exe' }
function Invoke-DumpbinFixture {
    if ($args[0] -ne '/DEPENDENTS') { throw 'Incorrect dumpbin arguments.' }
    [Console]::WriteLine('Checked ' + $args[1])
    $global:LASTEXITCODE = 0
    if ($args[1] -eq $env:PAYLOAD_TEST_BAD_BINARY) {
        if ($env:PAYLOAD_TEST_FAILURE -eq 'exit') { $global:LASTEXITCODE = 7 }
        else { Write-Output 'VCRUNTIME140.dll' }
    } else { Write-Output 'KERNEL32.dll' }
}
$dumpbin = 'Invoke-DumpbinFixture'
$parsed = [ScriptBlock]::Create([System.IO.File]::ReadAllText('ci/build-installer.ps1'))
$loop = $parsed.Ast.Find({
    param($node)
    $node -is [System.Management.Automation.Language.ForEachStatementAst] -and $node.Variable.Extent.Text -eq '$payload'
}, $true)
if (-not $loop) { throw 'Installer payload dependency loop was not found.' }
& ([ScriptBlock]::Create($loop.Extent.Text))
'''
        for binary, failure in itertools.product(("", "regressify-desktop.exe", "regressify.exe"), ("exit", "runtime")):
            with self.subTest(binary=binary, failure=failure):
                result = subprocess.run(
                    [shell, "-NoProfile", "-NonInteractive", "-Command", script], cwd=ROOT,
                    env=dict(os.environ, PAYLOAD_TEST_BAD_BINARY=binary, PAYLOAD_TEST_FAILURE=failure),
                    capture_output=True, text=True, check=False, timeout=15,
                )
                self.assertEqual(result.returncode == 0, not binary, result.stderr)
                if not binary:
                    self.assertIn("Checked regressify-desktop.exe", result.stdout)
                    self.assertIn("Checked regressify.exe", result.stdout)
                else:
                    self.assertIn(binary, result.stderr)

    def test_installer_copies_and_smoke_tests_both_executables(self):
        installer = (ROOT / "installer/regressify.iss").read_text(encoding="utf-8")
        for application in ("Desktop", "Cli"):
            self.assertIn(f'Source: "{{tmp}}\\{{#{application}ArchiveName}}"; DestDir: "{{app}}"; Hash: "{{#{application}ArchiveHash}}"; Flags: external extractarchive ignoreversion', installer)
        self.assertIn("DefaultDirName={localappdata}\\Programs\\Regressify", installer)
        self.assertIn("OutputBaseFilename=Regressify-Setup-{#AppVersion}-x64", installer)
        for name in ("LICENSE", "LICENSING.md"):
            self.assertIn(f'Source: "..\\{name}"; DestDir: "{{app}}"; Flags: ignoreversion', installer)
        smoke = (ROOT / "ci/build-installer.ps1").read_text(encoding="utf-8")
        self.assertIn("$licenseFiles = @('LICENSE', 'LICENSING.md')", smoke)
        self.assertIn('throw "Installer did not install the repository $name."', smoke)
        self.assertIn('throw "Uninstall left $name behind."', smoke)
        self.assertIn('Get-FileHash -LiteralPath $installedCli -Algorithm SHA256', smoke)
        self.assertIn('& $installedCli --version', smoke)
        self.assertIn("$installedCli = Join-Path $destination 'regressify.exe'", smoke)
        self.assertEqual(smoke.count('        Assert-InstallerCliPath\n'), 3)
        self.assertIn('where.exe regressify && regressify --version', smoke)
        self.assertIn('DoNotExpandEnvironmentNames', smoke)
        self.assertIn('Uninstall did not preserve the original user PATH data and type.', smoke)
        self.assertIn('Uninstall left the installed CLI behind.', smoke)

    def test_uninstall_license_check_uses_the_installation_directory(self):
        shell = shutil.which("pwsh")
        self.assertIsNotNone(shell, "PowerShell 7 is required for installer checks")
        script = r'''
$ErrorActionPreference = 'Stop'
$destination = $env:INSTALLER_TEST_DIRECTORY
$licenseFiles = @('LICENSE', 'LICENSING.md')
$parsed = [ScriptBlock]::Create([System.IO.File]::ReadAllText('ci/build-installer.ps1'))
$loop = $parsed.Ast.Find({
    param($node)
    $node -is [System.Management.Automation.Language.ForEachStatementAst] -and
        $node.Condition.Extent.Text -eq '$licenseFiles' -and
        $node.Body.Extent.Text.Contains('Uninstall left $name behind.')
}, $true)
if (-not $loop) { throw 'Installer license cleanup check was not found.' }
& ([ScriptBlock]::Create($loop.Extent.Text))
'''
        for remaining in (None, "LICENSE", "LICENSING.md"):
            with self.subTest(remaining=remaining), tempfile.TemporaryDirectory(prefix="installer cleanup ") as directory:
                destination = Path(directory) / "install [fixture]"
                destination.mkdir()
                user_file = destination / "user-workspace.txt"
                user_file.write_bytes(b"preserve user data")
                if remaining:
                    (destination / remaining).write_bytes(b"leftover license")
                result = subprocess.run(
                    [shell, "-NoProfile", "-NonInteractive", "-Command", script], cwd=ROOT,
                    env=dict(os.environ, INSTALLER_TEST_DIRECTORY=str(destination)),
                    capture_output=True, text=True, check=False, timeout=15,
                )
                self.assertEqual(result.returncode == 0, remaining is None, result.stdout + result.stderr)
                if remaining:
                    self.assertIn(f"Uninstall left {remaining} behind.", result.stderr)
                    self.assertEqual((destination / remaining).read_bytes(), b"leftover license")
                self.assertEqual(user_file.read_bytes(), b"preserve user data")

    def test_installer_builder_powershell_parses(self):
        script = r'''
$tokens = $null
$parseErrors = $null
foreach ($file in @('ci/build-installer.ps1', 'website/public/install.ps1')) {
    [System.Management.Automation.Language.Parser]::ParseFile((Join-Path (Get-Location) $file), [ref]$tokens, [ref]$parseErrors) | Out-Null
    if ($parseErrors) { throw ($parseErrors | Out-String) }
}
'''
        result = subprocess.run(
            [shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-Command", script],
            cwd=ROOT, capture_output=True, text=True, check=False, timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def installer_process_helper(self):
        return r'''
$ErrorActionPreference = 'Stop'
$parsed = [ScriptBlock]::Create([System.IO.File]::ReadAllText('ci/build-installer.ps1'))
$helper = $parsed.Ast.Find({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Invoke-InstallerFixture'
}, $true)
if (-not $helper) { throw 'Installer process helper was not found.' }
. ([ScriptBlock]::Create($helper.Extent.Text))
'''

    @unittest.skipUnless(os.name == "nt", "Installer process-tree waits require Windows")
    def test_installer_process_wait_preserves_exits_and_cleans_up_timeouts(self):
        child_script = '''
import json
import os
from pathlib import Path
import subprocess
import sys
import threading

mode = os.environ["INSTALLER_TEST_MODE"]
if mode == "second-phase":
    subprocess.Popen([sys.executable, __file__, *sys.argv[1:]],
                     env=dict(os.environ, INSTALLER_TEST_MODE="hang"),
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    raise SystemExit(0)
Path(os.environ["INSTALLER_TEST_REPORT"]).write_text(json.dumps({
    "pid": os.getpid(), "arguments": sys.argv[1:],
    "proxy": {name: os.environ.get(name, "") for name in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY")},
}), encoding="utf-8")
if mode == "hang":
    threading.Event().wait()
raise SystemExit(7 if mode == "failure" else 0)
'''
        invoke = self.installer_process_helper() + r'''
$parameters = @(('"' + $env:INSTALLER_TEST_SCRIPT + '"'), '"two words"')
try {
    $code = Invoke-InstallerFixture $env:INSTALLER_TEST_PYTHON $parameters -TimeoutMilliseconds 3000
    if ($env:INSTALLER_TEST_MODE -in @('hang', 'second-phase')) { throw 'Expected a fixture timeout.' }
    Write-Output "Exit code: $code"
} catch {
    if ($env:INSTALLER_TEST_MODE -notin @('hang', 'second-phase') -or $_ -notmatch 'Installer fixture timed out') { throw }
    $report = Get-Content -LiteralPath $env:INSTALLER_TEST_REPORT -Raw | ConvertFrom-Json
    if (Get-Process -Id $report.pid -ErrorAction SilentlyContinue) { throw 'Timed-out installer process survived.' }
    Write-Output 'Timed out and cleaned up'
}
'''
        for mode in ("success", "failure", "hang", "second-phase"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(prefix="installer process ") as directory:
                fixture = Path(directory)
                script = fixture / "child script.py"
                script.write_text(child_script, encoding="utf-8")
                report = fixture / "report.json"
                result = subprocess.run(
                    [shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-Command", invoke], cwd=ROOT,
                    env=dict(os.environ, INSTALLER_TEST_MODE=mode, INSTALLER_TEST_SCRIPT=str(script),
                             INSTALLER_TEST_REPORT=str(report), INSTALLER_TEST_PYTHON=sys.executable),
                    capture_output=True, text=True, check=False, timeout=30,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                expected = "Timed out and cleaned up" if mode in ("hang", "second-phase") else f"Exit code: {7 if mode == 'failure' else 0}"
                self.assertIn(expected, result.stdout)
                recorded = json.loads(report.read_text(encoding="utf-8"))
                self.assertEqual(recorded["arguments"], ["two words"])
                self.assertEqual(recorded["proxy"], {"HTTP_PROXY": "http://127.0.0.1:9", "HTTPS_PROXY": "http://127.0.0.1:9",
                                                     "ALL_PROXY": "http://127.0.0.1:9", "NO_PROXY": ""})

    @unittest.skipUnless(os.name == "nt" and os.environ.get("ISCC_PATH"), "Set ISCC_PATH on Windows for isolated Inno PATH tests")
    def test_inno_path_registration_preserves_user_data(self):
        source = (ROOT / "installer/regressify.iss").read_text(encoding="utf-8")
        self.assertIn("ChangesEnvironment=yes", source)
        self.assertIn("RegisterCliPath(ExpandConstant('{app}'));", source)
        self.assertIn("if CurUninstallStep = usPostUninstall then", source)
        self.assertIn("NotifyEnvironmentChange(BroadcastWindow, EnvironmentChangeMessage", source)
        path_code = source.split("[Code]\n", 1)[1].split("procedure ChromiumInstallLog", 1)[0]
        with tempfile.TemporaryDirectory(prefix="regressify-inno-") as directory:
            fixture = Path(directory)
            registry_root = "Software\\Regressify\\InstallerPathTests\\" + fixture.name
            for name, key in [("UserEnvironmentKey", "Environment"), ("CliPathOwnerKey", "Software\\Regressify\\DesktopInstaller")]:
                declaration = f"{name} = '{key}';"
                self.assertEqual(path_code.count(declaration), 1)
                path_code = path_code.replace(declaration, f"{name} = '{registry_root}\\{name}';")
            results = fixture / "results.txt"
            tests = r'''
procedure Check(Condition: Boolean; const Message: String);
begin
  if not Condition then
    RaiseException(Message);
end;

procedure CheckPath(const Expected: String; ExpectedType: Cardinal);
var
  Actual: String;
  ValueType: Cardinal;
begin
  Check(ReadUserPath(Actual, ValueType), 'Cannot read fixture PATH');
  Check(Actual = Expected, 'PATH entries were changed or duplicated');
  Check(ValueType = ExpectedType, 'PATH registry type ' + IntToStr(ValueType) + ' instead of ' + IntToStr(ExpectedType) + ', value length ' + IntToStr(Length(Expected)));
end;

procedure RunPathCases;
var
  Original, Directory, Alias, Current: String;
  Failed: Boolean;
  ValueType: Cardinal;
begin
  Directory := 'C:\Regressify Path Test';
  RegisterCliPath(Directory);
  CheckPath(Directory, 2);
  RemoveCliPath;
  CheckPath('', 0);
  Check(not RegKeyExists(HKCU, CliPathOwnerKey), 'Ownership was not removed');

  Check(RegWriteStringValue(HKCU, UserEnvironmentKey, 'Path', ''), 'Cannot seed empty PATH');
  RegisterCliPath(Directory);
  RemoveCliPath;
  CheckPath('', 1);

  Original := 'C:\tools;;%SystemRoot%\System32;C:\tools-extra;';
  Check(RegWriteExpandStringValue(HKCU, UserEnvironmentKey, 'Path', Original), 'Cannot seed PATH');
  RegisterCliPath(Directory);
  RegisterCliPath(Directory);
  CheckPath(Original + ';' + Directory, 2);
  Check(RegWriteExpandStringValue(HKCU, UserEnvironmentKey, 'Path', Original + ';' + Directory + ';C:\later'), 'Cannot append unrelated entry');
  RemoveCliPath;
  RemoveCliPath;
  CheckPath(Original + ';C:\later', 2);

  Check(RegDeleteValue(HKCU, UserEnvironmentKey, 'Path'), 'Cannot reset PATH type');
  Check(RegWriteStringValue(HKCU, UserEnvironmentKey, 'Path', Original), 'Cannot seed literal PATH');
  RegisterCliPath(Directory);
  CheckPath(Original + ';' + Directory, 1);
  RemoveCliPath;
  CheckPath(Original, 1);

  Original := '"' + Uppercase(Directory) + '\";C:\other';
  Check(RegWriteExpandStringValue(HKCU, UserEnvironmentKey, 'Path', Original), 'Cannot seed existing entry');
  RegisterCliPath(Directory);
  Check(not RegKeyExists(HKCU, CliPathOwnerKey), 'Pre-existing entry was claimed');
  RemoveCliPath;
  CheckPath(Original, 2);

  Alias := '%TEMP%\Regressify Path Test';
  Directory := AddBackslash(GetEnv('TEMP')) + 'Regressify Path Test';
  Check(RegWriteExpandStringValue(HKCU, UserEnvironmentKey, 'Path', Alias), 'Cannot seed environment alias');
  RegisterCliPath(Directory);
  RemoveCliPath;
  CheckPath(Alias, 2);

  Original := Directory + '-other';
  Check(RegWriteExpandStringValue(HKCU, UserEnvironmentKey, 'Path', Original), 'Cannot seed prefix entry');
  RegisterCliPath(Directory);
  CheckPath(Original + ';' + Directory, 2);
  RegisterCliPath('C:\new directory');
  CheckPath(Original + ';C:\new directory', 2);
  RemoveCliPath;
  CheckPath(Original, 2);

  Directory := 'C:\semicolon;directory';
  RegisterCliPath(Directory);
  CheckPath(Original + ';"' + Directory + '"', 2);
  RemoveCliPath;
  CheckPath(Original, 2);

  Check(RegWriteDWordValue(HKCU, UserEnvironmentKey, 'Path', 42), 'Cannot seed invalid PATH type');
  Failed := False;
  try
    RegisterCliPath(Directory);
  except
    Failed := True;
  end;
  Check(Failed, 'Invalid PATH type was overwritten');
  Check(RegQueryDWordValue(HKCU, UserEnvironmentKey, 'Path', ValueType) and (ValueType = 42), 'Invalid PATH data was lost');
  Check(not RegKeyExists(HKCU, CliPathOwnerKey), 'Failed registration claimed PATH');

  Original := StringOfChar('x', 32766);
  Check(RegWriteStringValue(HKCU, UserEnvironmentKey, 'Path', Original), 'Cannot seed long PATH');
  Failed := False;
  try
    RegisterCliPath(Directory);
  except
    Failed := True;
  end;
  Check(Failed, 'Oversized PATH was accepted');
  CheckPath(Original, 1);

  Current := 'C:\first;C:\owned;C:\owned;C:\last';
  Check(RemovePathEntry(Current, 'C:\owned', False), 'Owned entry was not found');
  Check(Current = 'C:\first;C:\owned;C:\last', 'Removal deleted unrelated duplicate entries');
  Current := 'C:\owned;C:\last';
  Check(RemovePathEntry(Current, 'C:\owned', False) and (Current = 'C:\last'), 'Leading entry removal failed');
end;
'''
            result_path = str(results).replace("'", "''")
            harness = fixture / "path-tests.iss"
            harness.write_text(
                "[Setup]\nAppName=Regressify PATH Tests\nAppVersion=1\nPrivilegesRequired=lowest\n"
                "CreateAppDir=no\nCreateUninstallRegKey=no\nUninstallable=no\nOutputBaseFilename=path-tests\n"
                f"OutputDir={fixture}\n[Code]\n" + path_code + tests + f'''
function InitializeSetup(): Boolean;
begin
  Result := False;
  try
    try
      RunPathCases;
      SaveStringToFile('{result_path}', 'ok', False);
    except
      SaveStringToFile('{result_path}', GetExceptionMessage, False);
    end;
  finally
    RegDeleteKeyIncludingSubkeys(HKCU, '{registry_root}');
  end;
end;
''', encoding="utf-8",
            )
            compiler = os.environ["ISCC_PATH"]
            for command in [
                [compiler, "/Qp", str(harness)],
                [str(fixture / "path-tests.exe"), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/SP-", "/NORESTART"],
            ]:
                result = subprocess.run(command, capture_output=True, text=True, check=False, timeout=90)
                if command[0] == compiler:
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(results.is_file(), "Inno PATH tests did not execute")
            self.assertEqual(results.read_text(encoding="utf-8-sig"), "ok")
            import winreg

            uninstall_code = "procedure CurUninstallStepChanged" + source.split("procedure CurUninstallStepChanged", 1)[1].split("function ShouldLaunch", 1)[0]
            lifecycle = fixture / "path-lifecycle.iss"
            destination = fixture / "installed"
            lifecycle.write_text(
                "[Setup]\nAppName=Regressify PATH Lifecycle Tests\nAppVersion=1\nPrivilegesRequired=lowest\n"
                "CreateUninstallRegKey=no\nArchitecturesAllowed=x64compatible\nArchitecturesInstallIn64BitMode=x64compatible\n"
                f"DefaultDirName={destination}\nOutputDir={fixture}\nOutputBaseFilename=path-lifecycle\n"
                "[Code]\n" + path_code + uninstall_code + """
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    RegisterCliPath(ExpandConstant('{app}'));
end;
""", encoding="utf-8",
            )
            uninstall_log = fixture / "uninstall.log"
            invoke = self.installer_process_helper() + r'''
$parameters = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/SP-', '/NORESTART')
if ($env:INSTALLER_TEST_LOG) { $parameters += '/LOG="' + $env:INSTALLER_TEST_LOG + '"' }
$result = Invoke-InstallerFixture $env:INSTALLER_TEST_EXECUTABLE $parameters
if ($result -eq 0 -and $env:INSTALLER_TEST_LOG) {
    if (Test-Path -LiteralPath ($env:INSTALLER_TEST_REGISTRY + '\CliPathOwnerKey')) {
        throw 'Uninstall helper returned before removing PATH ownership metadata.'
    }
}
exit $result
'''
            try:
                result = subprocess.run([compiler, "/Qp", str(lifecycle)], capture_output=True, text=True, check=False, timeout=90)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                for executable, log in [
                    (fixture / "path-lifecycle.exe", ""),
                    (destination / "unins000.exe", str(uninstall_log)),
                ]:
                    result = subprocess.run(
                        [shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-Command", invoke],
                        cwd=ROOT, capture_output=True, text=True, check=False, timeout=135,
                        env=dict(os.environ, INSTALLER_TEST_EXECUTABLE=str(executable), INSTALLER_TEST_LOG=log,
                                 INSTALLER_TEST_REGISTRY="HKCU:\\" + registry_root),
                    )
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                details = uninstall_log.read_text(encoding="utf-8-sig", errors="replace")
                with self.assertRaises(FileNotFoundError, msg="Uninstall left PATH ownership metadata behind.\n" + details):
                    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, registry_root + "\\CliPathOwnerKey"):
                        pass
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, registry_root + "\\UserEnvironmentKey") as environment:
                    with self.assertRaises(FileNotFoundError, msg="Uninstall left its PATH entry behind.\n" + details):
                        winreg.QueryValueEx(environment, "Path")
            finally:
                for key in ("\\CliPathOwnerKey", "\\UserEnvironmentKey", ""):
                    try:
                        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, registry_root + key)
                    except FileNotFoundError:
                        pass
            for archive_name, binary_name in [
                ("regressify-desktop-x86_64-pc-windows-msvc.zip", "regressify-desktop.exe"),
                ("regressify-cli-x86_64-pc-windows-msvc.zip", "regressify.exe"),
            ]:
                with zipfile.ZipFile(fixture / archive_name, "w") as archive:
                    archive.writestr(binary_name, b"MZ synthetic payload")
            result = subprocess.run(
                [compiler, "/Qp", str(ROOT / "installer/regressify.iss"), "/DAppVersion=1.2.3.0",
                 f"/DSourceDir={fixture}", f"/DInstallerOutput={fixture}"],
                capture_output=True, text=True, check=False, timeout=90,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            prepared_dir = fixture / "prepared"
            prepare = self.installer_process_helper() + r'''
$running = [System.Threading.Mutex]::new($false, 'Local\Regressify.Desktop.Running.ED6F0908-7E11-4608-A532-E43482AA117B')
try {
    $parameters = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/SP-', '/NORESTART',
        ('/PREPARE="' + $env:PREPARE_DESTINATION + '"'), ('/SOURCE="' + $env:PREPARE_SOURCE + '"'))
    $code = Invoke-InstallerFixture $env:PREPARE_EXECUTABLE $parameters
    if ($code -ne 1) { throw "Preparation must skip installation and return 1, got $code" }
} finally { $running.Dispose() }
'''
            environment = dict(os.environ, PREPARE_DESTINATION=str(prepared_dir), PREPARE_SOURCE=str(fixture),
                               PREPARE_EXECUTABLE=str(fixture / "Regressify-Setup-1.2.3.0-x64.exe"))
            result = subprocess.run(
                [shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-Command", prepare],
                cwd=ROOT, env=environment,
                capture_output=True,
                text=True,
                check=False,
                timeout=90,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue((prepared_dir / "prepared.txt").is_file(), "Shell installer /PREPARE did not produce prepared.txt")
            self.assertIn("version=1.2.3.0\n", (prepared_dir / "prepared.txt").read_text(encoding="utf-8"))
            (prepared_dir / "regressify-cli-x86_64-pc-windows-msvc.zip").write_bytes(b"corrupt")
            (fixture / "regressify-cli-x86_64-pc-windows-msvc.zip").write_bytes(b"corrupt")
            result = subprocess.run([shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-Command", prepare],
                                    cwd=ROOT, env=environment, capture_output=True, text=True, check=False, timeout=90)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((prepared_dir / "prepared.txt").exists(), "Failure must remove the stale ready receipt")
            self.assertTrue((prepared_dir / "prepare-error.txt").exists())


class ReleaseFeedTests(unittest.TestCase):
    ORIGIN = "https://regressify.eshn.dev"

    def test_partial_platform_inventory_is_rejected_before_staging(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = self.write_artifacts(root)
            (artifacts / "regressify-cli-x86_64-unknown-linux-gnu.zip").unlink()
            result, output = self.stage(root, artifacts)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Missing required release assets", result.stderr)
            self.assertFalse(output.exists())

    def test_prepare_needs_only_the_ref_and_output_file(self):
        for ref_type, ref_name, version in [
            ("tag", "v0.2.0", "0.2.0.0"),
            ("branch", "master", ""),
            ("tag", "v0.2.0-beta", None),
        ]:
            with self.subTest(ref_type=ref_type, ref_name=ref_name), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "output"
                env = dict(os.environ, GITHUB_REF_TYPE=ref_type, GITHUB_REF_NAME=ref_name, GITHUB_OUTPUT=str(output))
                env.pop("RELEASE_ORIGIN", None)
                result = subprocess.run(
                    [sys.executable, str(ROOT / "ci/release_feed.py"), "prepare"],
                    env=env, capture_output=True, text=True, check=False,
                )
                if version is None:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(output.exists())
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(output.read_text(encoding="utf-8"), f"version={version}\n")

    def test_version_requires_a_tag_and_normalizes_numeric_components(self):
        self.assertEqual(release_feed.tag_version("branch", "v1.2.3"), "")
        self.assertEqual(release_feed.tag_version("tag", "v1.2.3"), "1.2.3.0")
        self.assertEqual(release_feed.tag_version("tag", "v01.02.03.4"), "1.2.3.4")
        for tag in ("1.2.3", "v1.2", "v1.2.3-beta", "v1.10000.0", "v1.2.3.4.5", "v1.2.3\n"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release_feed.tag_version("tag", tag)

    def test_origin_validation(self):
        self.assertEqual(release_feed.validate_origin(self.ORIGIN), self.ORIGIN)
        for origin in ("", "regressify.eshn.dev", "http://regressify.eshn.dev", "https://regressify.eshn.dev/",
                       "https://regressify.eshn.dev/x", "https://regressify.eshn.dev:8443", "https://u@regressify.eshn.dev",
                       "https://Regressify.eshn.dev", "https://regressify.eshn.dev?x"):
            with self.subTest(origin=origin), self.assertRaises(ValueError):
                release_feed.validate_origin(origin)

    def write_artifacts(self, root, version="1.2.3.0", extra=(), installers=None, checksum=True):
        artifacts = root / "artifacts"
        (artifacts / "nested").mkdir(parents=True)
        archives = sorted(release_feed.required_archives())
        for name in archives:
            path = artifacts / name
            path.write_bytes(f"archive {name}".encode())
            if checksum:
                (artifacts / f"{name}.sha256").write_text(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {name}\n", encoding="utf-8")
        for name in installers if installers is not None else [release_feed.installer_name(version)]:
            (artifacts / "nested" / name).write_bytes(b"MZ synthetic installer")
        for name, content in extra:
            (artifacts / name).write_bytes(content)
        return artifacts

    def stage(self, root, artifacts, ref_name="v1.2.3", origin=None):
        output = root / "downloads"
        env = dict(os.environ, GITHUB_REF_TYPE="tag", GITHUB_REF_NAME=ref_name, RELEASE_ORIGIN=origin or self.ORIGIN)
        result = subprocess.run(
            [sys.executable, str(ROOT / "ci/release_feed.py"), "stage", "--source", str(artifacts), "--output", str(output)],
            env=env, capture_output=True, text=True, check=False,
        )
        return result, output

    def test_stage_copies_release_files_and_writes_the_site_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = self.write_artifacts(root)
            result, output = self.stage(root, artifacts)
            self.assertEqual(result.returncode, 0, result.stderr)
            installer = output / "Regressify-Setup-1.2.3.0-x64.exe"
            self.assertEqual({path.name for path in output.iterdir()},
                             release_feed.required_assets("1.2.3.0") | {"latest.txt"})
            self.assertEqual(len(list(output.iterdir())), 34)
            digest = hashlib.sha256(installer.read_bytes()).hexdigest()
            manifest_lines = (output / "latest.txt").read_text(encoding="utf-8").splitlines()
            self.assertEqual(manifest_lines[:5], [
                "version=1.2.3.0",
                "tag=v1.2.3",
                f"installer=https://github.com/precise-alloy/regressify-rs/releases/download/v1.2.3/{installer.name}",
                f"sha256={digest}",
                f"size={installer.stat().st_size}",
            ])
            self.assertEqual(len(manifest_lines), 21)
            self.assertTrue(all(line.startswith("archive.") for line in manifest_lines[5:]))

    def test_stage_rejects_unexpected_incomplete_or_oversized_releases(self):
        cases = [
            ({"extra": [("notes.html", b"<script>")]}, {}, "Unexpected release file"),
            ({"extra": [("latest.txt", b"version=9")]}, {}, "Unexpected release file"),
            ({"extra": [("regressify-cli-x86_64-unknown-linux-gnu.tar.gz", b"x")]}, {}, "Unexpected release file"),
            ({"installers": []}, {}, "Expected exactly one installer"),
            ({"installers": ["Regressify-Setup-1.2.4.0-x64.exe"]}, {}, "Expected exactly one installer"),
            ({"installers": ["Regressify-Setup-1.2.3.0-x64.exe", "Regressify-Setup-1.2.2.0-x64.exe"]}, {},
             "Expected exactly one installer"),
            ({"checksum": False}, {}, "Missing required release assets"),
            ({}, {"ref_name": "v1.2.4"}, "Expected exactly one installer"),
            ({}, {"origin": "http://regressify.eshn.dev"}, "RELEASE_ORIGIN"),
        ]
        for artifact_options, stage_options, message in cases:
            with self.subTest(artifacts=artifact_options, stage=stage_options), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result, output = self.stage(root, self.write_artifacts(root, **artifact_options), **stage_options)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertFalse((output / "latest.txt").exists())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = self.write_artifacts(root)
            (artifacts / "regressify-cli-x86_64-unknown-linux-gnu.zip.sha256").write_text("0" * 64 + "  x\n", encoding="utf-8")
            result, _ = self.stage(root, artifacts)
            self.assertIn("does not match the archive", result.stderr)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = self.write_artifacts(root)
            with (artifacts / "regressify-desktop-x86_64-pc-windows-msvc.zip").open("r+b") as stream:
                stream.truncate(0)
            result, _ = self.stage(root, artifacts)
            self.assertIn("1 byte to 1 GiB", result.stderr)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = self.write_artifacts(root)
            (root / "downloads").mkdir()
            (root / "downloads" / "index.html").write_text("site", encoding="utf-8")
            result, _ = self.stage(root, artifacts)
            self.assertIn("must be empty", result.stderr)

    def manifest(self, file, tag="v1.2.3", version="1.2.3.0", digest=None):
        digest = digest or hashlib.sha256(file.read_bytes()).hexdigest()
        return release_feed.manifest_text(tag, version, file.stat().st_size, digest).encode()

    def test_github_inventory_requires_exact_assets_sizes_hashes_and_urls(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, output = self.stage(root, self.write_artifacts(root))
            self.assertEqual(result.returncode, 0, result.stderr)
            files = release_feed.checked_release_files(output, "v1.2.3", "1.2.3.0")
            assets = [{"name": name, "state": "uploaded", "size": path.stat().st_size,
                       "digest": f"sha256:{release_feed.file_digest(path)}",
                       "browser_download_url": release_feed.archive_url("v1.2.3", name)}
                      for name, path in files.items()]
            for asset_tag in ("v1.2.3", "untagged-0123456789abcdefabcd"):
                current_assets = [{**asset, "browser_download_url": release_feed.archive_url(asset_tag, asset["name"])}
                                  for asset in assets]
                self.assertEqual(release_feed.verify_github_assets(current_assets, files, asset_tag), [])
                first, *rest = current_assets
                for invalid in [current_assets[:-1], current_assets + [first],
                                [{**first, "size": 0}, *rest],
                                [{**first, "digest": "sha256:" + "0" * 64}, *rest],
                                [{**first, "state": "starter"}, *rest],
                                [{**first, "browser_download_url": "https://other.example/a"}, *rest],
                                [{**first, "browser_download_url": release_feed.archive_url("untagged-ffffffff", first["name"])}, *rest]]:
                    with self.subTest(asset_tag=asset_tag, invalid=invalid[0]), self.assertRaises(ValueError):
                        release_feed.verify_github_assets(invalid, files, asset_tag)
                if asset_tag != "v1.2.3":
                    with self.assertRaises(ValueError):
                        release_feed.verify_github_assets(current_assets, files, "v1.2.3")

    def test_draft_asset_tag_is_bound_to_its_exact_release_url(self):
        prefix = f"https://github.com/{release_feed.RELEASE_REPO}/releases/tag/"
        temporary = "untagged-0123456789abcdefabcd"
        for asset_tag in ("v1.2.3", temporary):
            draft = {"draft": True, "html_url": prefix + asset_tag}
            self.assertEqual(release_feed.github_asset_tag(draft, "v1.2.3"), asset_tag)
            self.assertEqual(release_feed.github_asset_tag({**draft, "draft": False}, "v1.2.3"), "v1.2.3")
        for location in (None, 123, "", prefix + "v1.2.4", prefix + "untagged-", prefix + "untagged-nothex",
                         prefix + temporary + "/extra", prefix + temporary + "?query=1",
                         prefix + temporary + "#fragment", prefix + "untagged-%61",
                         prefix.replace("https:", "http:") + temporary,
                         prefix.replace("github.com/", "github.com:443/") + temporary,
                         prefix.replace("github.com/", "github.com.example/") + temporary,
                         prefix.replace("github.com/", "user@github.com/") + temporary,
                         prefix.replace(release_feed.RELEASE_REPO, "another/repository") + temporary):
            with self.subTest(location=location), self.assertRaises(ValueError):
                release_feed.github_asset_tag({"draft": True, "html_url": location}, "v1.2.3")

    def test_github_requests_use_fresh_authenticated_bounded_json(self):
        for data in (None, {"tag_name": "v1.2.3", "draft": True}):
            response = mock.MagicMock()
            response.__enter__.return_value = response
            response.read.return_value = b'{"id": 1}'
            opener = mock.Mock()
            opener.open.return_value = response
            with self.subTest(data=data), \
                 mock.patch.dict(os.environ, GH_TOKEN="synthetic-release-token"), \
                 mock.patch.object(release_feed.urllib.request, "build_opener", return_value=opener) as build:
                self.assertEqual(release_feed.github_request("releases", data=data), {"id": 1})
            self.assertIsInstance(build.call_args.args[0], release_feed.NoRedirect)
            request = opener.open.call_args.args[0]
            self.assertEqual(request.full_url, f"https://api.github.com/repos/{release_feed.RELEASE_REPO}/releases")
            self.assertEqual(request.get_method(), "GET" if data is None else "POST")
            self.assertEqual(request.get_header("Authorization"), "Bearer synthetic-release-token")
            self.assertEqual(request.get_header("Cache-control"), "no-cache")
            self.assertEqual(opener.open.call_args.kwargs["timeout"], 30)
            response.read.assert_called_once_with(2 * 1024 * 1024 + 1)
            if data is None:
                self.assertIsNone(request.data)
            else:
                self.assertEqual(request.get_header("Content-type"), "application/json")
                self.assertEqual(json.loads(request.data), data)

    def test_github_write_errors_are_not_missing_release_results(self):
        for status, data in [(404, None), (404, {"draft": True}), (403, {"draft": True}),
                             (422, {"draft": True}), (302, {"draft": True})]:
            opener = mock.Mock()
            opener.open.side_effect = urllib.error.HTTPError(
                "https://api.github.com/", status, "Failure", {}, io.BytesIO(b"private response body"),
            )
            with self.subTest(status=status, data=data), \
                 mock.patch.object(release_feed.urllib.request, "build_opener", return_value=opener):
                if data is None:
                    self.assertIsNone(release_feed.github_request("releases", data=data))
                else:
                    with self.assertRaisesRegex(RuntimeError, f"HTTP {status}") as failure:
                        release_feed.github_request("releases", data=data)
                    self.assertNotIn("private response body", str(failure.exception))
                opener.open.assert_called_once()

    def test_release_creation_requires_the_existing_exact_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, output = self.stage(root, self.write_artifacts(root))
            self.assertEqual(result.returncode, 0, result.stderr)
            for reference in (None, {}, {"ref": "refs/heads/v1.2.3"}, {"ref": "refs/tags/v1.2.30"}):
                with self.subTest(reference=reference), \
                     mock.patch.dict(os.environ, GITHUB_REPOSITORY=release_feed.RELEASE_REPO), \
                     mock.patch.object(release_feed, "find_github_release", return_value=None), \
                     mock.patch.object(release_feed, "github_request", return_value=reference) as request, \
                     mock.patch.object(release_feed.subprocess, "run") as run:
                    with self.assertRaisesRegex(ValueError, "release tag must exist"):
                        release_feed.publish_release(output, "v1.2.3", "1.2.3.0")
                    request.assert_called_once_with("git/ref/tags/v1.2.3")
                    run.assert_not_called()

    def test_release_lookup_finds_published_missing_and_paginated_drafts(self):
        draft = {"id": 1, "tag_name": "v1.2.3", "draft": True, "prerelease": False}
        public = {**draft, "draft": False}
        full_page = [{"tag_name": f"v0.0.{index}"} for index in range(10)]
        for responses, expected in [
            ([public], public), ([None, []], None), ([None, [draft]], draft),
            ([None, [{**draft, "tag_name": "v1.2.30"}]], None),
            ([None, full_page, [draft]], draft), ([None, full_page, []], None),
        ]:
            with self.subTest(pages=len(responses), expected=expected), \
                 mock.patch.object(release_feed, "github_request", side_effect=responses) as request:
                self.assertEqual(release_feed.find_github_release("v1.2.3"), expected)
                self.assertEqual([call.args[0] for call in request.call_args_list],
                                 ["releases/tags/v1.2.3"] +
                                 [f"releases?per_page=10&page={page}" for page in range(1, len(responses))])

    def test_release_lookup_rejects_invalid_ambiguous_and_unbounded_lists(self):
        draft = {"id": 1, "tag_name": "v1.2.3", "draft": True, "prerelease": False}
        for listing in (None, {}, [None], [{}], [{"tag_name": 123}], [draft, draft], [draft] * 11):
            with self.subTest(listing=listing), \
                 mock.patch.object(release_feed, "github_request", side_effect=[None, listing]) as request:
                with self.assertRaises(ValueError):
                    release_feed.find_github_release("v1.2.3")
                self.assertEqual(request.call_count, 2)
        full_page = [{"tag_name": f"v0.0.{index}"} for index in range(10)]
        with mock.patch.object(release_feed, "github_request", side_effect=[None] + [full_page] * 100) as request:
            with self.assertRaisesRegex(ValueError, "exceeded 1000 releases"):
                release_feed.find_github_release("v1.2.3")
            self.assertEqual(request.call_count, 101)
        with mock.patch.object(release_feed, "github_request", side_effect=[None, RuntimeError("HTTP 403")]):
            with self.assertRaisesRegex(RuntimeError, "HTTP 403"):
                release_feed.find_github_release("v1.2.3")

    def test_publication_verifies_complete_draft_before_making_it_public(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, output = self.stage(root, self.write_artifacts(root))
            self.assertEqual(result.returncode, 0, result.stderr)
            files = release_feed.checked_release_files(output, "v1.2.3", "1.2.3.0")
            assets = [{"name": name, "state": "uploaded", "size": path.stat().st_size,
                       "digest": f"sha256:{release_feed.file_digest(path)}",
                       "browser_download_url": release_feed.archive_url("v1.2.3", name)}
                      for name, path in files.items()]
            draft_tag = "untagged-7d881bc99966d6520253"
            draft_assets = [{**asset, "browser_download_url": release_feed.archive_url(draft_tag, asset["name"])}
                            for asset in assets]
            draft = {"id": 1, "tag_name": "v1.2.3", "draft": True, "prerelease": False,
                     "html_url": f"https://github.com/{release_feed.RELEASE_REPO}/releases/tag/{draft_tag}"}
            public = {**draft, "draft": False,
                      "html_url": f"https://github.com/{release_feed.RELEASE_REPO}/releases/tag/v1.2.3"}
            for complete, resume, final_urls in itertools.product((False, True), repeat=3):
                state = {"created": resume, "uploaded": False, "published": False}

                def request(endpoint, *, data=None):
                    if endpoint == "releases/tags/v1.2.3":
                        return public if state["published"] else None
                    if endpoint == "releases?per_page=10&page=1":
                        return [draft] if resume else []
                    if endpoint == "git/ref/tags/v1.2.3":
                        return {"ref": "refs/tags/v1.2.3"}
                    if endpoint == "releases":
                        self.assertFalse(state["created"], "An existing draft must be reused")
                        self.assertEqual(data, {"tag_name": "v1.2.3", "name": "Regressify v1.2.3",
                                                "draft": True, "prerelease": False, "generate_release_notes": True})
                        state["created"] = True
                        return draft
                    if endpoint == "releases/1/assets?per_page=100":
                        self.assertTrue(state["created"])
                        current_assets = assets if state["published"] and final_urls else draft_assets
                        if state["uploaded"]:
                            return current_assets if complete else current_assets[:-1]
                        return current_assets[:1] if resume else []
                    self.fail(f"Unexpected GitHub request: {endpoint}")

                def command(arguments, **kwargs):
                    operation = arguments[2]
                    if operation == "upload":
                        state["uploaded"] = True
                    elif operation == "edit":
                        self.assertTrue(complete)
                        state["published"] = True
                    else:
                        self.fail(f"Unexpected GitHub command: {arguments}")

                with self.subTest(complete=complete, resume=resume, final_urls=final_urls), \
                     mock.patch.dict(os.environ, GITHUB_REPOSITORY=release_feed.RELEASE_REPO), \
                     mock.patch.object(release_feed, "github_request", side_effect=request), \
                     mock.patch.object(release_feed.subprocess, "run", side_effect=command) as run:
                    if complete and final_urls:
                        release_feed.publish_release(output, "v1.2.3", "1.2.3.0")
                    else:
                        message = "does not match the verified build" if complete else "Missing required GitHub"
                        with self.assertRaisesRegex(ValueError, message):
                            release_feed.publish_release(output, "v1.2.3", "1.2.3.0")
                    commands = [call.args[0] for call in run.call_args_list]
                    self.assertEqual([command[2] for command in commands],
                                     ["upload"] + (["edit"] if complete else []))
                    if resume:
                        self.assertNotIn(str(files[assets[0]["name"]]), commands[0])
                    self.assertFalse(any("--clobber" in command for command in commands))

    def test_manifest_request_has_client_identity_no_credentials_and_no_redirects(self):
        body = b"Forbidden"
        opener = mock.Mock()
        opener.open.side_effect = urllib.error.HTTPError(
            release_feed.manifest_url(self.ORIGIN), 403, "Forbidden", {}, io.BytesIO(body),
        )
        with mock.patch.object(release_feed.urllib.request, "build_opener", return_value=opener) as build:
            self.assertEqual(release_feed.fetch_manifest(self.ORIGIN), (403, body))
        self.assertIsInstance(build.call_args.args[0], release_feed.NoRedirect)
        message = opener.open.call_args.args[0]
        self.assertEqual(message.get_method(), "GET")
        self.assertEqual(message.full_url, f"{self.ORIGIN}/downloads/latest.txt")
        self.assertEqual(message.get_header("User-agent"), "Regressify-Release/1.0")
        self.assertEqual(message.get_header("Cache-control"), "no-cache")
        self.assertFalse(message.has_header("Authorization"))

    def test_verification_failure_preserves_server_reason(self):
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / release_feed.installer_name("1.2.3.0")
            file.write_bytes(b"MZ synthetic installer")
            with (
                mock.patch.object(release_feed, "fetch_manifest", return_value=(403, b"Access denied")) as fetch,
                mock.patch.object(release_feed.time, "sleep") as sleep,
                self.assertRaises(RuntimeError) as failure,
            ):
                release_feed.verify_publish(self.ORIGIN, "v1.2.3", "1.2.3.0", file, "0" * 64)
            self.assertIn("HTTP 403", str(failure.exception))
            self.assertIn("Access denied", str(failure.exception))
            fetch.assert_called_once()
            sleep.assert_not_called()

    def test_verification_requires_matching_version_url_size_and_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / release_feed.installer_name("1.2.3.0")
            file.write_bytes(b"MZ synthetic installer")
            digest = hashlib.sha256(file.read_bytes()).hexdigest()
            for content in [self.manifest(file), json.dumps(release_feed.manifest_fields(self.manifest(file))).encode()]:
                with mock.patch.object(release_feed, "fetch_manifest", return_value=(200, content)):
                    release_feed.verify_publish(self.ORIGIN, "v1.2.3", "1.2.3.0", file, digest)
            for invalid in (self.manifest(file, "v1.2.4", "1.2.4.0"), self.manifest(file, digest="0" * 64),
                            self.manifest(file).replace(b"size=22", b"size=23"),
                            self.manifest(file).replace(b"/releases/download/", b"/other/"), b"garbage"):
                with mock.patch.object(release_feed, "fetch_manifest", return_value=(200, invalid)), \
                     mock.patch.object(release_feed.time, "sleep"), self.assertRaises(RuntimeError):
                    release_feed.verify_publish(self.ORIGIN, "v1.2.3", "1.2.3.0", file, digest)

    def test_verification_retries_transient_errors_with_a_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / release_feed.installer_name("1.2.3.0")
            file.write_bytes(b"MZ synthetic installer")
            digest = hashlib.sha256(file.read_bytes()).hexdigest()
            with mock.patch.object(release_feed, "fetch_manifest", side_effect=[
                urllib.error.URLError("offline"), (503, b"Unavailable"), (404, b"Not found"), (200, self.manifest(file)),
            ]) as fetch, mock.patch.object(release_feed.time, "sleep"):
                release_feed.verify_publish(self.ORIGIN, "v1.2.3", "1.2.3.0", file, digest)
                self.assertEqual(fetch.call_count, 4)
            with mock.patch.object(release_feed, "fetch_manifest", return_value=(404, b"Not found")) as fetch, \
                 mock.patch.object(release_feed.time, "sleep"), self.assertRaises(RuntimeError):
                release_feed.verify_publish(self.ORIGIN, "v1.2.3", "1.2.3.0", file, digest)
            self.assertEqual(fetch.call_count, 5)

    def test_branch_publication_is_rejected_before_network_access(self):
        env = dict(os.environ, GITHUB_REF_TYPE="branch", GITHUB_REF_NAME="master", RELEASE_ORIGIN=self.ORIGIN)
        result = subprocess.run([sys.executable, str(ROOT / "ci/release_feed.py"), "verify", "--installer", "missing.exe"],
                                env=env, capture_output=True, text=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Release publication requires a version tag", result.stderr)

    def test_unix_install_script_verifies_and_installs_platform_archives(self):
        bash = os.environ.get("BASH_PATH") or shutil.which("bash")
        if not bash:
            self.skipTest("Bash is required for install.sh checks")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            assets = root / "assets"
            assets.mkdir()
            archives = []
            for app, target, binary in [
                (application, target, "regressify" if application == "cli" else "regressify-desktop")
                for target in release_feed.TARGETS if not target.endswith("-msvc")
                for application in ("cli", "desktop")
            ]:
                archive_name = f"regressify-{app}-{target}.zip"
                archive_path = assets / archive_name
                with zipfile.ZipFile(archive_path, "w") as zf:
                    zf.writestr(binary, "#!/bin/sh\nexit 0\n")
                    zf.writestr("LICENSE", "AGPL")
                    zf.writestr("LICENSING.md", "Exception")
                data = archive_path.read_bytes()
                archives.append((archive_name, len(data), hashlib.sha256(data).hexdigest()))
            manifest_path = root / "latest.txt"
            manifest_path.write_text(
                release_feed.manifest_text("v1.2.3", "1.2.3.0", 22, "a" * 64, archives),
                encoding="utf-8",
            )
            for os_name, arch, libc, expects_desktop in [
                ("Darwin", "arm64", "", True),
                ("Darwin", "x86_64", "", True),
                ("Linux", "x86_64", "gnu", True),
            ]:
                install_dir = root / f"bin-{os_name}-{arch}-{libc}"
                share_dir = root / f"share-{os_name}-{arch}-{libc}"
                app_dir = root / f"apps-{os_name}-{arch}-{libc}"
                env = dict(
                    os.environ,
                    REGRESSIFY_OS=os_name,
                    REGRESSIFY_ARCH=arch,
                    REGRESSIFY_LIBC=libc,
                    REGRESSIFY_MANIFEST_FILE=manifest_path.as_posix(),
                    REGRESSIFY_ASSETS_DIR=assets.as_posix(),
                    REGRESSIFY_INSTALL_DIR=install_dir.as_posix(),
                    REGRESSIFY_SHARE_DIR=share_dir.as_posix(),
                    REGRESSIFY_APP_DIR=app_dir.as_posix(),
                    REGRESSIFY_SKIP_CHROMIUM="1",
                )
                script = (
                    'if command -v cygpath >/dev/null; then\n'
                    '  REGRESSIFY_MANIFEST_FILE="$(cygpath -u "$REGRESSIFY_MANIFEST_FILE")"\n'
                    '  REGRESSIFY_ASSETS_DIR="$(cygpath -u "$REGRESSIFY_ASSETS_DIR")"\n'
                    '  REGRESSIFY_INSTALL_DIR="$(cygpath -u "$REGRESSIFY_INSTALL_DIR")"\n'
                    '  REGRESSIFY_SHARE_DIR="$(cygpath -u "$REGRESSIFY_SHARE_DIR")"\n'
                    '  REGRESSIFY_APP_DIR="$(cygpath -u "$REGRESSIFY_APP_DIR")"\n'
                    'fi\n'
                    'sh website/public/install.sh'
                )
                result = subprocess.run([bash, "-c", script], cwd=ROOT, env=env, capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue((install_dir / "regressify").is_file())
                self.assertEqual((install_dir / "regressify-desktop").is_file(), expects_desktop)
                app_binary = app_dir / "Regressify.app/Contents/MacOS/regressify-desktop"
                self.assertEqual(app_binary.is_file(), os_name == "Darwin")
                if os_name == "Darwin":
                    self.assertTrue(os.access(app_binary, os.X_OK))
                    plist = (app_dir / "Regressify.app/Contents/Info.plist").read_text(encoding="utf-8")
                    self.assertIn("<string>regressify-desktop</string>", plist)
                    self.assertIn("<string>1.2.3.0</string>", plist)
                    self.assertEqual([path.name for path in app_dir.iterdir()], ["Regressify.app"])
                    # A second install replaces the bundle and leaves no staging debris.
                    stale = app_dir / "Regressify.app/Contents/stale"
                    stale.write_text("old", encoding="utf-8")
                    result = subprocess.run([bash, "-c", script], cwd=ROOT, env=env, capture_output=True, text=True, check=False)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertFalse(stale.exists())
                    self.assertEqual([path.name for path in app_dir.iterdir()], ["Regressify.app"])
                else:
                    self.assertFalse(app_dir.exists())
                self.assertTrue((share_dir / "LICENSE").is_file())
                self.assertTrue((share_dir / "LICENSING.md").is_file())
            original = manifest_path.read_text(encoding="utf-8")
            env.update(REGRESSIFY_OS="Darwin", REGRESSIFY_ARCH="arm64", REGRESSIFY_LIBC="")
            existing = install_dir / "regressify"
            existing.write_text("keep installed binary", encoding="utf-8")
            for invalid in [original + "tag=v9.9.9\n", original.replace("tag=v1.2.3", "tag=v1.2.4"),
                            original.replace("tag=v1.2.3", "tag=v1.2.3/../../other"),
                            original.replace("https://github.com/", "http://github.com/"), "x" * 16385]:
                manifest_path.write_text(invalid, encoding="utf-8")
                result = subprocess.run([bash, "-c", script], cwd=ROOT, env=env, capture_output=True, text=True, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(existing.read_text(encoding="utf-8"), "keep installed binary")
            manifest_path.write_text(original, encoding="utf-8")
            (assets / "regressify-desktop-aarch64-apple-darwin.zip").write_bytes(b"corrupt second archive")
            result = subprocess.run([bash, "-c", script], cwd=ROOT, env=env, capture_output=True, text=True, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(existing.read_text(encoding="utf-8"), "keep installed binary")

    def test_unix_install_script_keeps_previous_app_when_replacement_fails(self):
        bash = os.environ.get("BASH_PATH") or shutil.which("bash")
        if not bash or not shutil.which("mv"):
            self.skipTest("Bash and mv are required for install.sh checks")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            assets = root / "assets"
            assets.mkdir()
            archives = []
            for app, binary in [("cli", "regressify"), ("desktop", "regressify-desktop")]:
                archive_name = f"regressify-{app}-aarch64-apple-darwin.zip"
                with zipfile.ZipFile(assets / archive_name, "w") as zf:
                    zf.writestr(binary, "#!/bin/sh\nexit 0\n")
                    zf.writestr("LICENSE", "AGPL")
                    zf.writestr("LICENSING.md", "Exception")
                data = (assets / archive_name).read_bytes()
                archives.append((archive_name, len(data), hashlib.sha256(data).hexdigest()))
            manifest_path = root / "latest.txt"
            manifest_path.write_text(
                release_feed.manifest_text("v1.2.3", "1.2.3.0", 22, "a" * 64, archives), encoding="utf-8")
            # Fake mv that fails when promoting the staged bundle, and optionally the rollback too.
            shim_dir = root / "shim"
            shim_dir.mkdir()
            shim = shim_dir / "mv"
            # The shim restores the caller's PATH before delegating, so no executable path is
            # embedded; LF endings keep the shebang valid under Windows Git Bash.
            with open(shim, "w", encoding="utf-8", newline="\n") as shim_file:
                shim_file.write(
                    "#!/bin/sh\n"
                    'case "$1" in\n'
                    '  */.Regressify.*/Regressify.app) exit 1 ;;\n'
                    '  */previous.app) [ -n "$FAIL_ROLLBACK" ] && exit 1 ;;\n'
                    "esac\n"
                    'PATH="$REAL_PATH"\n'
                    'exec mv "$@"\n')
            shim.chmod(0o755)
            for fail_rollback in ("", "1"):
                with self.subTest(fail_rollback=bool(fail_rollback)):
                    app_dir = root / f"apps-{bool(fail_rollback)}"
                    marker = app_dir / "Regressify.app/Contents/marker"
                    marker.parent.mkdir(parents=True)
                    marker.write_text("previous", encoding="utf-8")
                    env = dict(
                        os.environ, PATH=f"{shim_dir}{os.pathsep}{os.environ['PATH']}",
                        FAIL_ROLLBACK=fail_rollback, REAL_PATH=os.environ["PATH"], REGRESSIFY_OS="Darwin", REGRESSIFY_ARCH="arm64",
                        REGRESSIFY_MANIFEST_FILE=manifest_path.as_posix(), REGRESSIFY_ASSETS_DIR=assets.as_posix(),
                        REGRESSIFY_INSTALL_DIR=(root / "bin").as_posix(), REGRESSIFY_SHARE_DIR=(root / "share").as_posix(),
                        REGRESSIFY_APP_DIR=app_dir.as_posix(), REGRESSIFY_SKIP_CHROMIUM="1")
                    result = subprocess.run([bash, "website/public/install.sh"], cwd=ROOT, env=env,
                                            capture_output=True, text=True, check=False)
                    self.assertNotEqual(result.returncode, 0)
                    if not fail_rollback:
                        self.assertEqual(marker.read_text(encoding="utf-8"), "previous")
                        self.assertEqual([path.name for path in app_dir.iterdir()], ["Regressify.app"])
                    else:
                        self.assertIn("kept at", result.stderr)
                        self.assertEqual(len(list(app_dir.glob(".Regressify.*/previous.app/Contents/marker"))), 1)

    def test_unix_install_script_serializes_app_replacement(self):
        bash = os.environ.get("BASH_PATH") or shutil.which("bash")
        if not bash:
            self.skipTest("Bash is required for install.sh checks")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            assets = root / "assets"
            assets.mkdir()
            archives = []
            for app, binary in [("cli", "regressify"), ("desktop", "regressify-desktop")]:
                archive_name = f"regressify-{app}-aarch64-apple-darwin.zip"
                with zipfile.ZipFile(assets / archive_name, "w") as zf:
                    zf.writestr(binary, "#!/bin/sh\nexit 0\n")
                    zf.writestr("LICENSE", "AGPL")
                    zf.writestr("LICENSING.md", "Exception")
                data = (assets / archive_name).read_bytes()
                archives.append((archive_name, len(data), hashlib.sha256(data).hexdigest()))
            manifest_path = root / "latest.txt"
            manifest_path.write_text(
                release_feed.manifest_text("v1.2.3", "1.2.3.0", 22, "a" * 64, archives), encoding="utf-8")
            app_dir = root / "apps"
            marker = app_dir / "Regressify.app/Contents/marker"
            marker.parent.mkdir(parents=True)
            marker.write_text("previous", encoding="utf-8")
            env = dict(
                os.environ, REGRESSIFY_OS="Darwin", REGRESSIFY_ARCH="arm64", REGRESSIFY_LOCK_TIMEOUT="1",
                REGRESSIFY_MANIFEST_FILE=manifest_path.as_posix(), REGRESSIFY_ASSETS_DIR=assets.as_posix(),
                REGRESSIFY_INSTALL_DIR=(root / "bin").as_posix(), REGRESSIFY_SHARE_DIR=(root / "share").as_posix(),
                REGRESSIFY_APP_DIR=app_dir.as_posix(), REGRESSIFY_SKIP_CHROMIUM="1")
            # Another installer holds the destination lock: this one must not touch the app.
            lock = app_dir / ".Regressify.lock"
            lock.mkdir()
            result = subprocess.run([bash, "website/public/install.sh"], cwd=ROOT, env=env,
                                    capture_output=True, text=True, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Another Regressify install", result.stderr)
            self.assertEqual(marker.read_text(encoding="utf-8"), "previous")
            self.assertTrue(lock.is_dir())
            # Once the lock is released the install succeeds and releases the lock itself.
            lock.rmdir()
            result = subprocess.run([bash, "website/public/install.sh"], cwd=ROOT, env=env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(marker.exists())
            self.assertEqual([path.name for path in app_dir.iterdir()], ["Regressify.app"])

    def test_unix_install_script_releases_lock_when_staging_cleanup_fails(self):
        bash = os.environ.get("BASH_PATH") or shutil.which("bash")
        if not bash or not shutil.which("rm"):
            self.skipTest("Bash and rm are required for install.sh checks")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            assets = root / "assets"
            assets.mkdir()
            archives = []
            for app, binary in [("cli", "regressify"), ("desktop", "regressify-desktop")]:
                archive_name = f"regressify-{app}-aarch64-apple-darwin.zip"
                with zipfile.ZipFile(assets / archive_name, "w") as zf:
                    zf.writestr(binary, "#!/bin/sh\nexit 0\n")
                    zf.writestr("LICENSE", "AGPL")
                    zf.writestr("LICENSING.md", "Exception")
                data = (assets / archive_name).read_bytes()
                archives.append((archive_name, len(data), hashlib.sha256(data).hexdigest()))
            manifest_path = root / "latest.txt"
            manifest_path.write_text(
                release_feed.manifest_text("v1.2.3", "1.2.3.0", 22, "a" * 64, archives), encoding="utf-8")
            shim_dir = root / "shim"
            shim_dir.mkdir()
            with open(shim_dir / "rm", "w", encoding="utf-8", newline="\n") as shim_file:
                shim_file.write(
                    "#!/bin/sh\n"
                    'for arg in "$@"; do\n'
                    '  case "$arg" in */.Regressify.*) exit 1 ;; esac\n'
                    "done\n"
                    'PATH="$REAL_PATH"\n'
                    'exec rm "$@"\n')
            (shim_dir / "rm").chmod(0o755)
            app_dir = root / "apps"
            env = dict(
                os.environ, PATH=f"{shim_dir}{os.pathsep}{os.environ['PATH']}", REAL_PATH=os.environ["PATH"],
                REGRESSIFY_OS="Darwin", REGRESSIFY_ARCH="arm64", REGRESSIFY_LOCK_TIMEOUT="1",
                REGRESSIFY_MANIFEST_FILE=manifest_path.as_posix(), REGRESSIFY_ASSETS_DIR=assets.as_posix(),
                REGRESSIFY_INSTALL_DIR=(root / "bin").as_posix(), REGRESSIFY_SHARE_DIR=(root / "share").as_posix(),
                REGRESSIFY_APP_DIR=app_dir.as_posix(), REGRESSIFY_SKIP_CHROMIUM="1")
            result = subprocess.run([bash, "website/public/install.sh"], cwd=ROOT, env=env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("could not remove staging directory", result.stderr)
            self.assertTrue((app_dir / "Regressify.app/Contents/MacOS/regressify-desktop").is_file())
            self.assertFalse((app_dir / ".Regressify.lock").exists())

    def test_unix_install_script_releases_lock_before_chromium_step(self):
        bash = os.environ.get("BASH_PATH") or shutil.which("bash")
        if not bash:
            self.skipTest("Bash is required for install.sh checks")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            assets = root / "assets"
            assets.mkdir()
            archives = []
            for app, binary in [("cli", "regressify"), ("desktop", "regressify-desktop")]:
                archive_name = f"regressify-{app}-aarch64-apple-darwin.zip"
                with zipfile.ZipFile(assets / archive_name, "w") as zf:
                    # The CLI stub stands in for `regressify install`: it fails if the app lock is still held.
                    zf.writestr(binary, '#!/bin/sh\n[ ! -e "$APP_LOCK" ] || exit 7\nexit 0\n')
                    zf.writestr("LICENSE", "AGPL")
                    zf.writestr("LICENSING.md", "Exception")
                data = (assets / archive_name).read_bytes()
                archives.append((archive_name, len(data), hashlib.sha256(data).hexdigest()))
            manifest_path = root / "latest.txt"
            manifest_path.write_text(
                release_feed.manifest_text("v1.2.3", "1.2.3.0", 22, "a" * 64, archives), encoding="utf-8")
            app_dir = root / "apps"
            env = dict(
                os.environ, APP_LOCK=(app_dir / ".Regressify.lock").as_posix(),
                REGRESSIFY_OS="Darwin", REGRESSIFY_ARCH="arm64",
                REGRESSIFY_MANIFEST_FILE=manifest_path.as_posix(), REGRESSIFY_ASSETS_DIR=assets.as_posix(),
                REGRESSIFY_INSTALL_DIR=(root / "bin").as_posix(), REGRESSIFY_SHARE_DIR=(root / "share").as_posix(),
                REGRESSIFY_APP_DIR=app_dir.as_posix())
            env.pop("REGRESSIFY_SKIP_CHROMIUM", None)
            result = subprocess.run([bash, "website/public/install.sh"], cwd=ROOT, env=env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("Chromium could not be prepared", result.stderr)
            self.assertEqual([path.name for path in app_dir.iterdir()], ["Regressify.app"])

    def test_unix_installer_rejects_unsupported_linux_before_reading_manifest(self):
        bash = os.environ.get("BASH_PATH") or shutil.which("bash")
        self.assertIsNotNone(bash, "Bash is required for install.sh checks")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = root / "regressify"
            installed.write_bytes(b"keep installed CLI")
            for arch, libc, message in [
                ("x86_64", "musl", "Musl distributions are not supported"),
                ("amd64", "unknown", "require x86_64 glibc"),
                ("aarch64", "gnu", "managed Chromium is unavailable for Linux ARM64"),
                ("arm64", "musl", "managed Chromium is unavailable for Linux ARM64"),
            ]:
                with self.subTest(arch=arch, libc=libc):
                    env = dict(os.environ, REGRESSIFY_OS="Linux", REGRESSIFY_ARCH=arch, REGRESSIFY_LIBC=libc,
                               REGRESSIFY_CLI_ONLY="1", REGRESSIFY_MANIFEST_FILE="missing-must-not-be-read",
                               REGRESSIFY_INSTALL_DIR=root.as_posix())
                    result = subprocess.run([bash, "website/public/install.sh"], cwd=ROOT, env=env,
                                            capture_output=True, text=True, check=False, timeout=15)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                    self.assertEqual(installed.read_bytes(), b"keep installed CLI")
                    self.assertEqual([path.name for path in root.iterdir()], ["regressify"])

    def test_powershell_installs_only_the_verified_bootstrapper(self):
        harness = r'''
$ErrorActionPreference = 'Stop'
$env:OS = 'Windows_NT'
function Start-Process([string]$FilePath, [switch]$Wait, [switch]$PassThru) {
    if (-not $Wait -or -not $PassThru) { throw 'Installer lifetime must be awaited.' }
    [System.IO.File]::WriteAllText($env:INSTALL_SCRIPT_REPORT, $FilePath)
    return [pscustomobject]@{ ExitCode = 0 }
}
. ./website/public/install.ps1
'''
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            name = release_feed.installer_name("1.2.3.0")
            payload = b"synthetic setup; must never execute"
            (root / name).write_bytes(payload)
            manifest = root / "latest.txt"
            text = release_feed.manifest_text("v1.2.3", "1.2.3.0", len(payload), hashlib.sha256(payload).hexdigest())
            report = root / "launched.txt"
            env = dict(os.environ, REGRESSIFY_MANIFEST_FILE=str(manifest), REGRESSIFY_ASSETS_DIR=str(root),
                       INSTALL_SCRIPT_REPORT=str(report))
            for content, succeeds in [(text, True), (text + "version=9.0.0.0\n", False),
                                      (text.replace("/v1.2.3/", "/v1.2.4/"), False),
                                      (text.replace("sha256=", "sha256=0"), False),
                                      (text.replace("size=", "size=9"), False), ("x" * 16385, False)]:
                manifest.write_text(content, encoding="utf-8")
                report.unlink(missing_ok=True)
                result = subprocess.run([shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-Command", harness],
                                        cwd=ROOT, env=env, capture_output=True, text=True, check=False, timeout=20)
                self.assertEqual(result.returncode == 0, succeeds, result.stderr)
                self.assertEqual(report.exists(), succeeds, result.stderr)
                if succeeds:
                    self.assertFalse(Path(report.read_text(encoding="utf-8")).exists(), "Temporary installer was not cleaned up")


if __name__ == "__main__":
    unittest.main()