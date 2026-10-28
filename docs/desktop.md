# Regressify Desktop

The optional GPUI Kit desktop app shares the CLI's Rust library and keeps
YAML/JSON files as the source of truth. It provides configuration authoring,
isolated bundled-engine execution, and visual report review. See
[the implementation plan](desktop-plan.md) for scope and verification evidence.

## Build and Launch

The Windows GUI binary reserves an 8 MiB main-thread stack for GPUI rendering,
including debug builds. This linker setting does not apply to the CLI binary.

Use stable Rust and native build tools. Windows requires Visual Studio C++ build
tools and the Windows SDK. Other GPUI platform prerequisites are documented at
<https://gpui-kit.com/docs/installation>.

```sh
cargo build --features gui --bin regressify-desktop
cargo run --features gui --bin regressify-desktop
cargo run --features gui --bin regressify-desktop -- /path/to/workspace
```

Omit the workspace argument to use the native folder picker. The default
`cargo build` and `cargo run -- test` remain CLI-only. Feature `desktop` builds
headless models; `gui` adds native widgets; `gui-tests` adds UI instrumentation.

The native UI pins GPUI Kit 0.8.0 through the single optional `gpui-kit`
dependency. Production windows and native test fixtures use
`gpui_kit::open_window`; its Base root owns dialog, sheet and notification
layers. Application close, cancellation and unsaved-document guards remain
owned by the workbench.

Run and installer-download progress expose labeled percentage values to
assistive technologies. Download progress remains separate from verification
and installation readiness. Individual Git-file checkboxes use the toolkit's
binary control; folder and suite aggregates retain partial-selection semantics.

Numeric Fields controls support toolkit step buttons and Up/Down stepping.
Regressify retains schema limits, inherited values and document undo/redo;
invalid or intermediate text remains editable and is not coerced on blur.
Numeric spinners expose their field names to assistive technologies; the separate
Source preference spinner is named `Source font size`.

For distribution, use `cargo build --release --features gui --bin regressify-desktop`.
The desktop executable contains the engine; no separate `regressify` binary or
executable picker is needed. It launches itself in a private headless worker mode
to isolate run settings and browser processes. Windows installation and in-app
updates are described below; installer signing is not configured.

### Desktop Icon

The icon source is [assets/icon.svg](../assets/icon.svg). Windows GUI builds embed
[assets/icon.ico](../assets/icon.ico) as resource `1`, which GPUI uses for the
window and taskbar. The custom title bar embeds [assets/icon.png](../assets/icon.png)
without tinting its gradient. Shortcuts, Jump Lists, and Explorer context menus
use the executable's icon; the Windows installer uses the same artwork.

The installed executable keeps the `Regressify.Desktop` taskbar identity.
Local and portable copies use a stable identity derived from their executable
path, so Windows does not reuse an installed copy's shortcut or cached icon.
Their window metadata explicitly supplies the running executable's icon,
display name, and relaunch command. Moving a portable copy changes its identity.

The ICO contains transparent 32-bit images at 16, 20, 24, 32, 40, 48, 64, 128,
and 256 pixels; the title-bar PNG is 256 pixels. The SVG viewBox tightly frames
the letter to avoid oversized transparent margins. After editing the SVG,
regenerate both exports with Node.js:

```sh
npm install --prefix target/icon-tools --no-save --package-lock=false --ignore-scripts @resvg/resvg-js@2.6.2 png-to-ico@2.1.8
node ci/generate-icon.mjs
```

Commit the SVG and both exports together. Normal Rust and installer builds use
the checked-in images and do not require Node.js or the export tools. On an
unlocked Windows desktop with no other app instances running, verify the native
window icons and rendered title-bar/taskbar pixels with:

```powershell
./ci/desktop-smoke.ps1 -IconFlows
```

## Windows Installation and Updates

Windows x86_64 releases include `Regressify-Setup-N.N.N.N-x64.exe`.
Run the installer normally, without administrator elevation. It installs
`regressify-desktop.exe` and `regressify.exe` together in the per-user folder
`%LOCALAPPDATA%\Programs\Regressify` with the `LICENSE` and
`LICENSING.md` license files, creates desktop and Start-menu shortcuts, and adds
**Open with Regressify Desktop** to folder and folder-background
context menus. On Windows 11, the command is under **Show more options**.
Setup adds this shared install folder to the user's PATH, leaving the machine PATH
unchanged. Open a new terminal after setup to run `regressify`; restart any existing
terminal host to pick up the environment change. Upgrades do not duplicate the
entry. Uninstall removes both executables, the license files, shortcuts,
context-menu entries, and only the PATH entry owned by setup; pre-existing entries
and later PATH edits are
preserved. Workspaces and desktop preferences survive updates and uninstall.
Both release executables link the MSVC runtime statically.

Setup is a bootstrapper, not an embedded copy of the binaries. It downloads the
exact tagged Windows ZIP archives from GitHub Releases and verifies hashes baked
into setup at build time before extracting them. Upgrades keep the same AppId,
migrate the registered old install directory and owned PATH entry, and remove only
known installer-owned files from the old directory. Other user files remain.
GUI shortcuts and the Explorer command retain the name Regressify Desktop.

For terminal installation on Windows use
`irm https://regressify.eshn.dev/install.ps1 | iex`. The script verifies the
bootstrapper's size and SHA-256, then launches normal setup. Terminal downloads
may not receive browser-added origin metadata, but unsigned application and
organization security warnings may still apply. Scripts do not disable security
policies. Review the script before executing it.

Setup also prepares the pinned Chromium build in
`%LOCALAPPDATA%\regressify\chromium` before launching the app. It checks the same
shared cache used by CLI, Reference, and Test and downloads only when that build
is missing. Reinstallation or an app update does not download Chromium again
unless the required browser revision changes or the cache was removed. Installed
Chrome/Edge are not substitutes for this managed build. The browser cache remains
available after app uninstall.

Interactive setup and visible `/SILENT` updates show Chromium preparation progress.
During downloads, the text reports transferred size, percentage when available,
speed, and elapsed time. Connection waits, downloads, and extraction report activity
every five seconds. Fully silent `/VERYSILENT` setup stays hidden; all modes write
detailed messages to the setup log. If browser preparation fails, the app installation
still succeeds; interactive setup reports the problem, and Reference/Test can retry later.
Silent setup records the failure without opening a dialog. Pass `/SKIPCHROMIUM=1`
to defer preparation explicitly. Setup uses the HTTP client's default proxy
configuration, not a workspace's saved Proxy setting; for a workspace-specific
proxy, finish setup and choose Install Chromium from the app after configuring
Run settings. The same network time limits and diagnostics described under
[Chromium installation diagnostics](commands.md#chromium-installation-diagnostics)
apply during setup.

Right-click the app's taskbar icon to open a workspace from **Recent Workspaces**.
The Jump List uses saved workspace history, refreshes as workspaces are opened,
and skips missing folders and duplicates. Windows removals are remembered until
the workspace is explicitly reopened. Windows recent-item privacy settings and
the available Jump List space determine whether these entries are shown.

Official Windows builds check for updates in the background whenever a new app
instance opens, then every eight hours while the app remains open. Newer releases
are automatically downloaded and verified in the background. Each startup check
refreshes the shared check timestamp even if another instance checked recently;
scheduled checks in long-running instances use that timestamp and eight-hour interval.
After sleep, an overdue check runs on the next local poll. No upgrade banner,
highlighted update button, or installation prompt appears until the entire
installer and both application archives are saved locally and match the manifest's
sizes and SHA-256s. After verifying setup, the updater runs it with
`/PREPARE=<cache-directory>` in the background; it downloads and verifies the
archives without touching the installation, PATH, shortcuts, or running windows.
Network
outages and failed preparation do not interrupt editing or announce an upgrade.

Only one instance per user update cache owns checking and downloading at a time.
If another instance owns the updater, a new instance keeps its startup check pending
and checks after ownership becomes available, without starting concurrent work.
Other instances poll the shared cache every five seconds without contacting the
feed. Once the owner finishes, each instance verifies the same cached installer
and shows its own notification; it does not download another copy. Notification
normally follows the next local poll plus verification time. Closing the owner
releases the process lock and allows another instance to resume any partial
download. Abnormal process termination also releases ownership automatically.

Once preparation succeeds, a banner stays visible below the title bar in every
view with an Install update button, and the header's Software updates button is
highlighted. Select Install update, then Install and restart. Installation uses
the verified local installer with `/SOURCE=<cache-directory>`; setup rechecks
its baked-in archive hashes and does not fetch missing application files in this
mode. The updater rechecks setup and both archives immediately before launch.
Optional Chromium preparation can still need a download. Closing the dialog does
not dismiss the ready banner or remove the installer. Only newer numeric versions
and this app's immutable HTTPS installer URLs are accepted. A missing manifest
(404) means no release is published.

Release metadata, partial downloads, and checksum retry counts are retained in
the `updates` directory alongside desktop preferences. After startup, scheduled
feed checks use the persisted eight-hour interval. Startup and manual checks
contact the feed immediately when no other instance owns preparation; only manual
checks reset pauses and checksum retry limits. Cache notifications and checksum retries
within that interval reuse validated metadata without issuing another feed
request. Feed failures fall back to the newest cached release if available.
Discovering a newer release discards older partial and complete cached
installers before preparing the replacement. Unrelated cache files are left alone.
Closing the app or losing
power does not intentionally discard download progress: the next launch resumes
the bootstrapper from the saved bytes using HTTP range requests. Verified complete
application archives are reused; an interrupted archive transfer may restart.
If the server ignores ranges, the
transfer restarts from byte zero. An interrupted verification rechecks the retained
file, and an already downloaded installer can be verified and offered even while
the update service is unavailable. Cached release URLs are validated again before
use. An outer process lock covers the entire check/preparation cycle, in addition
to the existing lock around cache writes. Idle windows keep their ready banners
visible during automatic polls and continue to check for newer releases.

If SHA-256 verification fails, the invalid payload is discarded and downloaded
again, with at most three retries after the initial attempt (four attempts total).
Retries use exponential backoff: wait 2 hours after the first failure, 4 hours after
the second, and 8 hours after the third. The failure count and next retry deadline
survive restarts; reopening the app does not bypass the remaining wait. Scheduled
retries run automatically while the app stays open, without holding the cache lock
during the wait. If all attempts fail, automatic checks skip that version until a
strictly newer release is published. Network interruptions do not consume checksum
retries.

Manual Check for updates resets the checksum retry count, clears any scheduled
delay or skipped-version marker, and checks immediately when no peer owns the
update cycle. If another instance owns it, the dialog reports that preparation
is underway and observes its result instead of starting duplicate work. This is also available
when an installer is already ready, so a newer release can replace it. Automatic
startup checks and scheduled retries never reset the budget for the same version.

The Software updates dialog supports manual checks and shows preparation status:
connecting, receiving data, SHA-256 verification, saving the verified installer,
and preparing application files. During the bootstrapper transfer it shows received and
total MiB, percentage, average speed, and elapsed seconds. After five seconds
without data it also shows the inactivity time. Connecting or receiving no data
for 30 seconds pauses the attempt; each download attempt has a separate 15-minute
limit. HTTP errors, incomplete transfers, and disk-write failures retain the
partial download and defer automatic preparation for eight hours; manual Check
for updates retries immediately. Pause download also retains progress and applies
the same shared delay, so another instance does not immediately undo the pause.
Closing the owner without pausing allows another instance or the next launch to
resume instead. Closing only the dialog lets
preparation continue. Diagnostics are shown only in the dialog, where Copy details
copies the message for troubleshooting. A ready installer still requires an
explicit Install and restart; there is no automatic installation. Background payload
preparation has its own 15-minute deadline. Pause, shutdown handoff, and timeout
terminate and reap the entire preparation process group, including Inno's second
phase. The app stays usable while preparation runs.

Installation requires active runs to finish and uses the usual Save/Discard/Cancel
guard for a draft. No open workspace or document is required. After the initiating
app instance closes and the running-app checks pass, the installer requests
foreground focus for its progress window. Fully silent installs remain hidden.
The installer never
forcibly closes other windows or workers; close them normally when prompted. New
app launches are blocked during setup. After installation the initiating workspace
reopens, if one was open. Older installers cannot
downgrade an installed version. Local preparation removes the network dependency
from installation, but disk errors, security software, or running app instances
can still prevent setup from completing. Unconfigured development builds and non-Windows
builds do not contact the update service.

The public feed is `https://regressify.eshn.dev/downloads/latest.txt`. It selects
the site's supported version; binaries remain on GitHub Releases. Official builds
embed `REGRESSIFY_UPDATE_ORIGIN`, never credentials. Manifest redirects are refused.
Installer URLs must be exactly
`https://github.com/precise-alloy/regressify-rs/releases/download/<tag>/Regressify-Setup-<version>-x64.exe`,
with a tag matching the numeric version. Downloads allow bounded HTTPS GitHub
asset redirects and remain size/SHA-256 checked. Installers are unsigned.
Existing Tuyen Drive installs use the separately published bridge described
under [Bridge Rollout](#bridge-rollout); the old feed is not redirected.

macOS and Linux provide terminal installation via
`curl -fsSL https://regressify.eshn.dev/install.sh | sh`, plus ZIP/7z downloads.
The script installs into `~/.local/bin`, stores licenses under
`~/.local/share/regressify`, and attempts Chromium preparation. Set
`REGRESSIFY_INSTALL_DIR`, `REGRESSIFY_SHARE_DIR`, `REGRESSIFY_CLI_ONLY=1`, or
`REGRESSIFY_SKIP_CHROMIUM=1` before invoking it to customize those choices.
Both macOS architectures and Linux x64 glibc have CLI and desktop binaries.
Musl distributions and Linux ARM64 are not supported by prebuilt releases; the
installer rejects them before downloading or changing files. Linux desktop still requires compatible
system GUI libraries. The script requires curl, unzip, and a SHA-256 tool; it
verifies every selected archive before replacing binaries and does not edit shell
profiles. Non-Windows in-app updates remain unavailable; rerun the terminal command.

## Git

The title bar shows the number of changed files and incoming/outgoing commits
for the workspace's repository. Click it, use Ctrl+Shift+G (Cmd+Shift+G on macOS),
or choose Git from Commands to open the change trees and read-only change review.
The repository root is shown explicitly, including when the workspace is a
subdirectory of a larger repository. Counts and the tree cover that repository.

Local status refreshes every five seconds. The app checks the selected remote
every minute while idle. Before an explicit selection, it uses the tracking remote,
otherwise origin or the only configured remote.
Automatic checks fetch remote refs only: they never pull, merge, rebase, stash,
or change working files. Fetch also provides a manual remote check. Incoming
commits, divergence, merge conflicts, and failed remote checks are highlighted;
the last successful check remains visible so cached counts are not mistaken
for a successful fresh check. Network commands have deadlines and terminal
credential prompts are disabled. Configure Git credentials/SSH trust outside
the app; authentication failures can be handled in VS Code.

Changed files appear as collapsible, virtualized folder trees. **Changes** contains
ordinary project files. **Comparison Changes**, beneath it, contains files under
`.regressify` and `.regressify__<environment>` directories, including nested
workspaces. Renames into or out of these directories are comparison changes too.
Comparison Changes starts collapsed and unselected, even for files already staged
in Git. Expanding or reviewing a file does not select it. The ordinary All and
folder checkboxes never select comparison files; select those explicitly in their
own panel to include them in a commit. Collapsing the panel preserves explicit
selections, and the confirmation calls out included comparison files. Git-ignored
results stay ignored; this does not edit `.gitignore` or force-add ignored files.

Each tree retains its own cursor and folder expansion across status refreshes.
The cursor follows its repository-relative path when rows move and clears when
that path is no longer visible. During Git operations, you can still navigate,
expand folders and open other diffs. File selection and mutation commands stay
locked; browsing cannot change the paths or snapshot of the reviewed operation.

Selecting a text file opens a side-by-side comparison of **Last Commit** and
**Saved File**. Removed lines are red, added lines are green, and empty rows align
insertions and deletions without changing the original line numbers. Both panes
scroll together, including horizontally for long lines. Previous/Next change
buttons navigate changed sections; each pane has a copy button for its exact
source, including original line endings.

Comparison works on text, not parsed configuration: invalid YAML/JSON, comments,
formatting changes, JavaScript, and other UTF-8 files use the same view. Binary,
non-UTF-8, conflicted, submodule, and oversized files use a bounded raw Git diff
with a reason. Side-by-side previews allow up to 4 MiB per file, 20,000 lines per
side, and 16 KiB per line; the diff algorithm has a time budget. File loading and
comparison run off the UI thread, and only visible rows are rendered.

The comparison includes staged and unstaged saved changes relative to the last
commit, not unsaved editor drafts. It does not execute scenarios, predict visual
website changes, or modify files or the Git index. Existing save/discard/cancel
guards still apply before commits.

Choose files or folders with the checkboxes. A folder checkbox selects its group's
eligible changed files recursively, including files in collapsed subfolders, and
shows a partial state when only some are selected. Conflicts and submodules are
excluded from selection. Enter a commit message and confirm the commit.
The current contents of selected files are committed, including selected
renames/deletions and untracked files. Unselected staged files remain staged and
are not included. A failed commit may leave the selected files staged, as in
normal Git; no changes are discarded. Git hooks and signing configuration remain
in effect. Submodule commits and in-progress merge/rebase commits are delegated
to VS Code. Filenames are passed literally using NUL-delimited pathspecs.

Discard selected files is available beside Commit selected files and does not
require a commit message. Its confirmation lists the selected files and warns
that discarding cannot be undone. Tracked files lose both staged and unstaged
changes and are restored to the last commit, including selected deletions and
renames. New files, whether staged or untracked, are permanently deleted;
unselected changes and unrelated staged files remain untouched. Folder selection
does not include unchanged or ignored files. Discard refuses conflicts,
in-progress Git operations, submodules, directory replacements, and renames that
would overwrite an unselected file. Changed Git status requires another review.

Untracked workspaces display Not tracked and offer Initialize Git; initialization
does not stage files. The **Remote** row supports adding named remotes, selecting
the active remote, renaming it, editing its fetch or push URL, and removing it.
The remote selector supports search; unchanged status refreshes retain its query.
Add and URL edits accept credential-free HTTPS/SSH URLs or local repository paths.
Remote names start with a letter or number and use letters, numbers, `.`, `-`, `_`,
or `/`, subject to Git reference-name rules. URLs containing credentials or
unsupported formats are hidden; entering a replacement does not reveal the old
value. Push defaults to the fetch URL unless a separate push URL is configured.
URL edits change the first configured URL and preserve any additional URLs, whose
count is shown beside the primary URL.

Selection is saved in the repository's local Git configuration as
`regressify.remote`, not in workspace files. Fetch, pull, push, and incoming/outgoing
counts use this selection. With the tracking remote selected, network operations
use the configured upstream branch; otherwise they use the current branch name.
Renaming a selected remote retains the selection. Removing the active remote, or
losing it through an external edit, requires selecting a destination again; it
does not silently switch to another remote. Removing a remote deletes its local
remote-tracking references and upstream settings, not local branches, worktree
files, or the hosted repository. Remote edits retain their reviewed configuration
and reject an external change instead of overwriting it; cancel and reopen an edit
to review the latest configuration.

Push establishes upstream tracking and never force-pushes.
Pull requires a clean working tree and uses fast-forward-only
semantics. Diverged branches and existing conflicts show Open in VS Code for
merge/rebase resolution; refresh afterward. This avoids embedding a second merge
editor or silently resolving conflicts. These actions open the repository in its
own VS Code window, or focus the window already open on that folder. Opening a
selected file includes the repository folder so it uses that same window.

Commit, discard, pull, push, initialization, and remote changes use explicit
confirmation and the editor's unsaved-draft guard. Mutating operations block
workspace edits until completion and offer Stop; other Git work runs off the UI
thread. Requests
revalidate repository identity and HEAD before acting. Cancelling a network
operation cannot undo changes already accepted by the remote; stopping discard
cannot undo changes already discarded. Stopping a multi-step remote rename can
leave completed configuration changes in place; refresh and select the renamed
remote if necessary. Git must be installed on PATH. Git UI
tests use isolated repositories and local bare remotes.

**Activity** shows the running operation, elapsed time, current stage, live Git
progress/diagnostics, and completion, failure, or cancellation. Explicit actions
open it automatically; periodic fetches update it without expanding a collapsed
panel. It sits below the diff pane, keeping the file trees and commit controls
independent of log height, including in compact windows. Stop remains available
while a command is running or cancelling. Logs are
read-only and can be copied, collapsed, or cleared after completion. They retain
the newest output in view while **Follow latest** is checked; uncheck it to read
older output without jumping to the newest line. Logs retain
up to 400 lines for the current operation and 400 lines of recent history in memory;
switching workspaces clears them. Oversized output and omitted lines are marked.
URLs and authorization diagnostics are redacted before display or copying; after
an authorization header or oversized streamed line, the remaining diagnostic
stream is omitted conservatively. Process deadlines and cancellation remain in
effect while inherited output/input pipes are draining after Git exits.

Remote/upstream configuration is fingerprinted without retaining credentials in
status messages; network actions reject a changed destination after confirmation.
Changing branches or remotes invalidates the previous remote-check timestamp and
any in-flight result. Pull and discard reload the editor from disk in the
background and close a document removed by the operation while retaining the
Git view. Discard retains the draft commit message.

`./ci/desktop-smoke.ps1 -GitFlows` runs the native acceptance flow against local
repositories only. It verifies named remote add/select/update/rename/remove,
cancelled removal, separate fetch/push URLs, activity logs, initialization, structured and source review,
comparison-file opt-in, selected
commits that preserve unrelated staging, push, automatic one-minute incoming
checks, fast-forward pull, and conflict handoff in light/dark compact windows.
The Git workspace is isolated under the OS temporary directory; screenshots
remain under `target/desktop-smoke`. Other native fixtures initialize an isolated
repository before opening, so periodic checks cannot use the parent project's
remote. No real project commits or remote pushes are made by these tests.

## Landing Apps

Open **Publish > Landing apps** to manage customer landing pages independently
of the current workspace. Use the selector to view a saved app, **New landing
app** to create one, and **Save** to persist changes. The application chooses and
manages storage automatically; there is no file picker or Save As step. Landing
definitions are JSON files under `landing-apps/<id>.json` in the per-user desktop
settings directory, not in a visual-test workspace.

Each landing app has a heading, a Markdown subheading, and an ordered list of
sites. Each site has its own heading, Markdown subheading, and absolute HTTP(S)
URL. Add, reorder, edit, or remove sites in the editor. Heading text is escaped;
subheadings are pre-rendered with `pulldown-cmark`, excluding raw HTML, embedded
images, credentialed links, and unsafe URL schemes. Up to 100 sites are supported.

**Preview** opens the rendered draft on a loopback-only server. It exposes only
the generated HTML and bundled frontend assets, not workspace files or credentials.
Saved landing apps can be deployed to an existing Azure Static Web App, Netlify
site, or Cloudflare Worker using the same target controls as report deployment.
Save the draft before deploying. The app generates complete static HTML before
uploading; hosting does not need a Node.js server or request-time SSR runtime.
The page uses the report logo, fonts, and Light/Dark/System theme controls.
Each landing deployment target remembers its last successful URL. Reopen the
landing app's deployment settings to open or copy that URL without deploying again,
including after switching tabs or restarting the desktop app.

Linked sites do not need to exist locally and may be created by other users.
The desktop app never searches for their workspaces or fetches their reports.
Visitors' browsers independently request `/_snapshot/status.json` from each
linked URL's origin, with no cookies, authorization, or referrer. Site headings,
Markdown, and links remain usable without JavaScript or when a request fails.
Status shows suite count, capture count, capture pass rate, failures, engine
errors, and report-generation time; an empty report shows **No captures**.
Unavailable or older reports show **Status unavailable** with a retry control.
An HTTPS landing page may also be unable to read HTTP reports because of browser
mixed-content protection. You remain responsible for the correctness of links.

New reports include the small public status endpoint with CORS enabled and
cache revalidation. It contains aggregate counts and a timestamp, not suite
names, capture URLs, credentials, or browser state. Regenerate and redeploy older
reports to enable status loading. Private/authenticated reports are not fetched
with the visitor's credentials and may remain unavailable.

For a report behind Cloudflare Access, exempt only this endpoint: add a separate
self-hosted Access application for `<report-host>/_snapshot/status.json` with a
**Bypass** policy that includes **Everyone**. Its counts and timestamp then become
public. Access runs before the Worker, so Worker or Wrangler settings cannot
exempt a path.

Switching apps or closing a dirty editor requires saving or discarding the
draft. Saves and removal reject externally changed definitions. **Remove landing
app** confirms local deletion of the definition and its deployment settings;
it never deletes a hosted website. Deployment cancellation stops local work but
cannot undo a publication already accepted by a provider.

## Deployment Tokens

Open **Publish > Deployment tokens** to create, list, rename, rotate, or remove
reusable credentials shared across workspaces and landing apps on this machine.
Netlify entries contain a name and token; Cloudflare entries also contain the
Account ID. The token input is masked, and saved secrets are never displayed.
Leave it blank when editing to preserve the current secret. To change provider
or Cloudflare account, create a new entry so existing targets cannot silently
switch identity.

Saved-token and target selectors support search and retain their stable identity
when names change. Reconfirming the current provider or token source preserves
the current secret draft; choosing a different identity clears that draft.

In a deployment target, choose a saved name under **Token source**, then enter
the Netlify Site ID or Cloudflare Worker name. A named Cloudflare token supplies
the Account ID automatically. **Target-specific token** preserves direct entry
for a single target. Azure tokens remain target-specific.

Named-token metadata is stored in `deployment-tokens.json`; secrets remain in
the operating system credential manager. Deleting a deployment target does not
delete a reusable token. Removing a named token requires confirmation and makes
targets referencing it require another token, without affecting hosted sites.
Concurrent token-editor changes are detected rather than silently overwritten.

## Deploy Reports

The header's Deploy command publishes selected suites to Azure Static Web Apps
or Netlify, or to Cloudflare Workers Static Assets.
Use Deployment targets to add, edit, or remove multiple targets for the current
workspace. Choose the target in the deployment dialog each time. A target has a
name, a provider, and a securely saved token. Azure targets also have an optional
named environment (blank means production); Netlify targets have a Site ID;
Cloudflare targets have an Account ID and Worker name.
Paste the provider's token directly into its masked token field and select
Save target. No environment variable
or app restart is needed. When editing a target, leave that field blank to keep
the saved token, or paste a replacement token to rotate it. Saved tokens are
never displayed in the editor; cancelling or closing clears newly entered text.
Changing providers clears the token draft and requires a compatible named token
or a new target-specific token before saving.
Existing targets without a provider setting continue to use Azure.

Each target stores its last successfully deployed HTTPS URL. Open **Deployment
targets**, choose a target, then click its URL to open the site or use **Copy
deployed URL** directly beside the left-aligned link. The dialog omits the selected
suite list; partial selections still require explicit confirmation.
These controls do not require suite selection or a saved token,
and remain available after switching tabs or restarting the app. Failed or
cancelled deployments keep the previous URL. Renaming a target preserves it;
changing provider, Azure environment, Netlify Site ID, Cloudflare account/Worker,
Cloudflare routing mode, or replacing an Azure token clears it until the next
successful deployment. Cloudflare HTML, compatibility-date, and exclusion edits
preserve the URL. A successful private Cloudflare deployment clears any previous
public URL; it still appears as a completed deployment without an Open Site button.
Azure tokens identify the destination app as well as authorize it. If saving a
successful deployment's URL fails, the app reports the local persistence error.

Successful public report and landing deployments send `regressify_site_deployed`
to PostHog US Cloud. Event properties include `url`, `provider`, `file_count`,
`original_bytes`, `total_bytes`, `duration_ms`, and `app_version`, plus
SDK/runtime metadata.
Counts describe the final provider manifest or package after any optimization,
including cached files and duplicate paths, not just newly uploaded blobs;
`original_bytes` records the staged content size before provider-budget image
optimization (matching `total_bytes` when no optimization runs).
Bytes measure file content, excluding ZIP and transport overhead. Duration covers
provider preparation, uploads, and publication confirmation, excluding snapshot
generation, staging, and analytics delivery. Failed deployments, private sites,
and unsafe URLs do not create this event.

Tracking is anonymous, with person profiles and GeoIP enrichment disabled.
Deployment tokens, workspace paths, target names, and report contents are not
event properties. Delivery is silent and best effort, with one attempt and a
two-second deadline; tracking failures do not change the deployment result or
appear in the UI. Set `DO_NOT_TRACK=1` in the application's environment before
launch to disable tracking. The [shared analytics API](../src/analytics.rs) also
serves future CLI and desktop events, automatically naming them
`regressify_[object]_[verb]`.

In **Runs**, **Open Site** appears beside **Open Report** in the toolbar and on
each published suite's row. It opens the most recently deployed site containing
that suite at `/suites/<suite-id>`. With multiple suites selected, the toolbar
opens the newest site containing all of them. Publication membership survives
restarts and other runs; redeploying a subset replaces the previous membership
for that site. Failed or cancelled deployments preserve it. Destination changes,
target removal, or successful private publication remove the affected links.
Older saved URLs without suite membership remain available in the deployment
dialog and gain suite-specific links after the next successful report deployment.

For **Azure Static Web Apps**, choose that provider and enter the app's deployment
token. The Azure environment field remains optional.

For **Netlify**, create a dedicated Netlify project/site first, choose Netlify
as the provider, and enter its Site ID and a Netlify personal access token.
Netlify now labels the Site ID **Project ID**, under Project configuration >
General > Project details > Project information. Generate the token under your
Netlify user settings > Applications > Personal access tokens; it must have
access to the project's team, including SSO authorization when required.
The app publishes to the existing site's production deployment, replacing its
contents with the selected reports. It does not create or delete Netlify sites,
change domains, run builds, or require the Netlify CLI.

For **Cloudflare Workers**, select the provider, enter an Account ID and dedicated
Worker name, and provide an API token with **Workers Scripts Write** permission for that
account. The Account ID is the 32-character hexadecimal identifier in the account
dashboard; it is also the first hostname component of an R2 S3 endpoint. R2 Access
Key IDs and Secret Access Keys are not used. The desktop does not read `.env.local`;
enter the token in its masked field to save it securely.

Deployment creates the Worker if it does not exist, or replaces its code/assets
with an assets-only report. **Public (production)** is the default for both new
and existing targets: deployment publishes the production version and enables
its `workers.dev` route. Register an account-level `workers.dev` subdomain in
Cloudflare first. The desktop does not change DNS, custom domains, or Access
policies, and does not require Wrangler, Bun, or Node.js.

Choose **Private** to disable both the production `workers.dev` route and preview
URLs, or **Preserve existing routing** to leave an existing Worker's routing
untouched, including dashboard changes made during upload. Preserve mode creates
a missing Worker privately; unreadable routing or permission errors never imply
that a Worker is missing. A public deployment returns
`https://<worker>.<account-subdomain>.workers.dev/` only after checking the published
HTML and a companion asset without sending credentials. Access-protected public
responses cannot pass that verification. Report pages send the visitor's
same-origin Access session cookie with their data requests; reports generated by
older builds omit it and fail with a browser CORS error, so regenerate and
redeploy them. Private deployments succeed without a public URL; configure any
custom-domain access separately.

The Cloudflare asset-settings control saves HTML handling, not-found handling,
compatibility date, and excluded paths with each target. Defaults are
`auto-trailing-slash`, `none`, and `2026-09-25`, with no extra exclusions.
Excluded paths are an app-relative JSON array of globs, such as
`["debug/**", "*.map"]`; matching directories are pruned. Keep required report
JSON, images, scripts, and `index.html` included. Private environment files,
`.dev.vars` variants, `.git`, `.wrangler`, and `node_modules` are always excluded.
An unsupported `.assetsignore` file or included Pages `_worker.js` code causes
local validation to fail before API requests. These options use saved target
settings, not CLI flags or environment-variable fallbacks.

Tokens are stored in the operating system's secure credential store, outside the
repository: Windows Credential Manager (current user, this machine only), macOS
Keychain, or a Linux Secret Service such as GNOME Keyring or KWallet. Linux needs
an available, unlocked Secret Service. A locked or unavailable store produces an
error, never a plaintext fallback. Direct-entry credentials are scoped to the
canonical workspace path and target, so moving a workspace requires entering
those tokens again. Explicit named tokens use a separate per-user identity and
can be shared across workspaces on the same machine. Moving to another computer
requires entering secrets again.

Target settings contain public target metadata, the last successful URL, and
credential references in the per-user application configuration.
Token values are never saved in project files or settings JSON,
passed in worker requests or command-line arguments, or included in deployment
logs. Removing a target removes its target-specific token, but preserves named
tokens and does not delete the hosted site. Targets created with the old environment-variable setting retain
their names and Azure environments; edit each target and enter its token once
to enable deployment.

At least one suite must be selected. When only a subset is selected, the dialog
lists it and requires explicit confirmation that excluding the other suites is
intentional. Individual scenario selections do not narrow deployment: the full
latest report of each selected suite is published. Unsaved changes use the normal
Save/Cancel guard, and a changed suite catalog requires selection review again.

Deploy validates the token, runs the bundled `snapshot` command for the selected
suites, and stages the snapshot entry page, versioned JSON, referenced images,
frontend assets, and hosting policies before publishing. Staging follows the
generated manifest rather than walking capture folders. Tracked suites use their
latest completed full-suite run, independently of the last command or the shared
`config.js`. A skipped comparison is generated from those completed captures when
needed. Legacy suites without a registry retain their existing report selection.
Snapshot generation also creates fast, deterministic small PNG previews for suite
browsing. Staging includes those versioned preview dependencies alongside original
images; comparison and downloads retain full resolution. Preview generation does
not resize source captures or run expensive image optimization. Repeating it with
identical inputs produces identical preview bytes and cache versions.

Deployment staging, optimization copies, and upload packages stay inside an
operation-owned `.staging` container under the selected output folder. Removal
after deployment is best effort and does not turn successful publication into
failure. Failed removal retains ownership metadata; the next deployment retries
abandoned containers while preserving active ones. Landing deployments use the
same lifecycle in their managed storage. None of this bookkeeping is deployed.

Snapshot and Deploy use `snapshotTitle`, `snapshotHeading`, and
`snapshotSubheading` from the workspace configuration unless an explicit override
was entered in Runs > Settings > Report. The subheading supports Markdown links.
Untouched report fields inherit workspace values, then the built-in defaults;
deliberately entered empty strings remain explicit overrides.

Saved activity from v0.32.0 and earlier prefilled these overrides with built-in
text. On loading that legacy activity, exact built-in strings become inherited
values while custom text is retained. Newly saved explicit overrides, including
values equal to the built-in defaults, are preserved when reopening the workspace.

Unselected suites, unreferenced captures and diffs from any run (including the
current run), capture metadata, unused TTF fonts, source maps, persisted
authentication state, source YAML, and unrelated files are not uploaded. Skipped
files remain unchanged on disk. Referenced web fonts, icons, scripts, and styles
are retained. Symlinked or escaping artifact paths are rejected. No external
Azure, npm, Tuyen CLI, or Wrangler installation is required.
Azure uses Tuyen CLI's SWA deploy handshake: validate token, request
upload URLs, upload Azure blob blocks, update status, and poll until completion.
Transient network failures retry within bounds; the entire deployment has a
30-minute deadline. ZIP packaging is disk-backed, preserves already-compressed
images without recompressing them, and uploads at most four 4 MiB blocks at once.
Missing current/diff images fail deployment rather than publishing broken links;
missing references remain valid report failures. Non-Standard plans reject security-sensitive `auth` and
`networking` configuration rather than letting Azure silently discard it.

Azure token validation also detects the app's actual Free or Standard tier.
Deployment budgets are 250 MiB for Free and 500 MiB for Standard per environment
(Azure labels these limits in MB), measured from all uncompressed report files,
not the ZIP size. An unrecognized tier is logged and conservatively uses the Free
budget. Reports already within budget are uploaded without image changes.

Oversized Azure and Cloudflare reports use the same image optimizer, only in a
separate OS-temporary directory. Cloudflare uses a 2 GiB budget for selected public
assets; excluded files and hosting policies do not count toward that budget.
Local screenshots, references, report metadata, and files used by future
comparisons are never overwritten. Lossless PNG recompression comes first. If
more savings are needed, perceptual palette reduction tries progressively lower
quality floors for passed comparisons (99 down to 50), then failed Reference/Test
images (99 down to 80). Larger images are tried first at each quality level;
processing stops when the report fits, with a higher-quality search for the last
image when possible. Every attempt starts from the original, and replacements
are kept only if smaller. Dimensions, transparency support, and test results
remain intact; failed diff images, error placeholders, and generated previews stay lossless. Any changed
image bytes receive a new `?v=...` value from `get_asset_version`, and suite JSON
plus the entry manifest receive updated versions as well. Filenames remain stable,
and only the temporary staging copy is rewritten.
The deployment log reports the budget, savings, reduced images, and skipped
images. If the report cannot fit within these safeguards, deployment stops before
upload with sizes and recovery steps. Choose fewer suites or a larger plan;
Azure may also reject a fitting report if other environments exhaust total app
storage. Optimization shares each provider's deployment deadline and does not
affect Netlify.

Netlify verifies site access, hashes the staged files, and creates an asynchronous
file-digest deployment; it does not build or upload a ZIP. The manifest includes
every selected report path and its SHA-1, as required by Netlify's API. Only
content Netlify requests is uploaded, with at most four file requests in flight.
Identical content is uploaded once even when used at multiple paths. Existing
reference images and shared assets can therefore be reused across deployments.
Netlify still publishes the complete report atomically, after every required
file is accepted and the deployment becomes ready.

The client limits Netlify deployments to 25,000 paths, 2 GiB of staged content,
and a 16 MiB manifest; it bounds API response bodies to 2 MiB. File paths and
content are revalidated when uploading. Authentication, permanent HTTP errors,
and changed or unreadable staged files fail without automatic retries. File
uploads and read/poll requests retry transient errors up to six attempts with
bounded backoff and numeric Retry-After support. Site validation and status reads
also retry timed-out or interrupted response bodies after successful HTTP headers;
malformed JSON and oversized bodies still fail immediately. Retry logs distinguish
response-body timeouts from interruptions, report received byte counts without
response content, and identify when all six attempts are exhausted. A failure
during `Validating Netlify deployment target` occurs before any report upload;
if it persists, check the connection, proxy/firewall, and Netlify's service status.
Deployment creation is not
automatically retried because a failed response may still have created a deploy.
Check Netlify's deploy history before retrying a failed or canceled deployment.
Locked production deploys and Git-only production restrictions are reported
before upload. Azure authentication, network restrictions, and restricted role
rules are rejected instead of silently publishing unprotected reports on Netlify.
Configure access protection in Netlify itself for private reports. Netlify allows
up to one hour for hashing, uploads, retries, and publication polling. Each file
request uses that timeout but cannot outlive the overall deadline. Connections
retain a 10-second timeout; manifest creation and read/poll requests use 30 seconds.
Azure retains its ZIP/block-upload protocol and 30-minute deployment deadline.
All providers keep tokens out of URLs and logs.

Cloudflare creates a path/hash/size manifest, uploads only requested content, and
publishes the completed asset set. Upload-session hints select raw, single-asset
requests or Base64 multipart batches. Identical content with the same extension
and MIME type is uploaded once; MIME changes invalidate cached response headers.
Fully cached deployments skip uploads but still publish. Public sites are then
verified. Multipart mode permits three concurrent requests; single-asset mode
honors the server's concurrency hint up to eight. Uploads share a 50 MiB raw-content
budget, retained through reads and retries; encoded bodies and request copies
use additional memory. Individual multipart buckets also have a 50 MiB raw limit.
MIME support includes manifests, WebVTT, WebAssembly, media, and document formats;
unknown formats use Cloudflare's MIME-header omission marker.
The provider's one-hour temporary tokens are bounded by a 55-minute client
deadline. Upload request timeouts scale with content size, up to 30 minutes.
Safe reads, upload-session creation, explicit route updates, and content-addressed
uploads retry transient failures up to six attempts. Publication is never
automatically retried: a confirmed API rejection means the Worker was not updated,
while an interrupted or ambiguous response requires checking deployment history.
Stop cancels local work, not a publication already accepted by Cloudflare.

Cloudflare limits individual files to 25 MiB, including screenshots. Free accounts
allow 20,000 files per Worker version; Paid accounts allow 100,000. The client
checks the per-file limit, caps content at 2 GiB and 100,000 paths, and lets the
manifest API enforce the account's file-count allowance. Reports above the total
content budget use the shared image optimizer before any API request; asset hashes
and sizes are rebuilt from the optimized copy, which stays available through
upload and public verification. Reports already within budget are unchanged.
Manifests and API responses are bounded to 32 MiB and 8 MiB respectively. Individual
files above 25 MiB still fail before optimization; they are never uploaded to R2.
Deploy fewer suites or reduce capture sizes when needed. Azure access restrictions
are rejected before any Cloudflare API request instead of silently publishing
unprotected reports.

Provider failures include available structured error details, HTTP status,
request/deployment identifiers, and recovery guidance for access, storage,
billing, throttling, and provider outages. This includes Azure's `errorMessage`
and `errorDetails` and Netlify's `error_message`. Details are bounded and
credential/signed-URL redacted; unparseable responses fall back to status-specific
guidance. The full error is shown in deployment Activity and View Log.

On Windows, `Access is denied (os error 5)` is an operating-system access failure,
not a Cloudflare API permission code. Local target-settings errors include the
operation and settings path; report-worker errors identify snapshot generation or
staging, and Cloudflare file-preparation errors identify the staging location.
`Cannot generate report snapshots for deployment` occurs before staging or any
Cloudflare upload. Run Snapshot with the same workspace, suites, environment, and
output-directory settings to isolate that local step without deploying.
Snapshot errors identify output discovery, managed `.staging` creation,
artifact reads/writes, or cleanup and the affected path. Snapshot staging is
inside the selected report output directory (normally `.regressify`), which needs
write and cleanup access as well as read access. A Windows file-sharing conflict
uses error 32 and is distinct from access-denied error 5.
Removal of confirmed-empty generated directories is optional cleanup. If Windows
or a sync client blocks it, Snapshot logs `Warning: Could not remove empty snapshot directory`
and continues deployment, leaving the folder for a later cleanup attempt.
Empty folders are not deployed. Artifact read/write failures, stale-file deletion,
and directory inspection errors still stop deployment; permissions are never
changed automatically.
Cloudflare asset reads and staging copies include the affected paths. Check the full error
and last activity step before changing permissions or replacing a token. Confirm
that the Windows account can read the report output and write the application
settings and temporary directories. Check Windows Security Protection history for
a matching Controlled folder access block, or ask your administrator to check
endpoint protection. Do not disable protection or run as administrator just to
bypass an unidentified failure. Include the complete error when reporting it,
without sharing credentials.

All providers send `X-Robots-Tag: noindex,nofollow` for report URLs. Snapshots
include matching Azure/Netlify security and cache policies, plus clean-URL SPA
fallback rules. Cloudflare passes the header and redirect rules as asset metadata,
not public files. With HTML normalization enabled, it translates
`/suites/* /index.html 200` to `/suites/* / 200` because Workers rejects the former
as a canonical-HTML redirect loop. With HTML handling set to `none`, the original
`index.html` rewrite is retained. Default not-found handling keeps missing
`/_snapshot/*` assets as real 404s. HTML and `/_snapshot/status.json` revalidate;
versioned assets use immutable one-year caching. Versioned resource URLs include
`?v=...`; any additional CDN must include that parameter
in its cache key. Stable origin files contain only the current report version.
These directives ask search engines not to index reports, but do not restrict
access. See [snapshot site hosting](commands.md#snapshot-site) for other hosts.

The last selected target is remembered per workspace, and removing a target
requires confirmation. Deploy and target management are also available from the
command palette. Deploy immediately shows a loading state, then opens Runs >
Activity with a highlighted deployment band, animated spinner, target, current
stage, and elapsed time. Deployment activity replaces scenario counters with
deployment-specific feedback. Completion, failure, and cancellation have distinct
status icons and colors; View Log remains available, and successful deployments
offer Open Site.
The suite summary stays on one line, with an ellipsis for overflow, and deployment
activity omits the reference/capture statistics row. Its read-only log editor
fills the remaining container height and retains the full bounded run log, just
like the Log tab. Select text to copy it, or use **Copy deployment log** for all
retained text. Snapshot generation logs suite loading, capture preview resizing,
log publication, and output writes; staging and
packaging report file/byte counts, optimization reports image classification,
per-image lossless and palette reduction progress, cache-version refreshes, and
actual savings, and
Netlify reports reused paths, per-file bytes sent, and accepted file/byte counts;
Azure reports acknowledged bytes. Azure block commits,
retry reasons/delays, and each provider polling state are logged. A five-second
heartbeat continues during long asynchronous operations, without claiming
unmeasured completion percentages or provider acceptance.
Netlify samples each active file upload every 60 seconds. If its byte count has
not advanced since the previous sample, Activity and View Log show a warning
each minute identifying the file number, elapsed time without observed progress,
and sent/total bytes. The upload keeps running, so users can keep waiting or
choose Stop. Once a file's bytes have been sent, warnings describe waiting for
Netlify's acceptance instead of a stalled transfer. Retry attempts restart that
file's sent counter; accepted totals count each required content only once.
Stop cancels the local worker process tree; it cannot undo a deployment already
accepted by Azure or Netlify. Check the provider's deploy history before retrying
after cancellation. Deployment
transport is covered by local mock-server tests; a live Azure smoke deployment
requires a configured target and is not performed by the test suite.

Optional non-publishing API checks use `AZURE_STATIC_WEB_APPS_API_TOKEN`,
`NETLIFY_PAT`, and `NETLIFY_SITE_ID` from the process environment:
`cargo test --locked --features desktop --lib live_deployment_ -- --ignored --nocapture --test-threads=1`.
These tests validate the Azure tier and Netlify site access without creating a deploy.

An explicitly publishing Netlify smoke test is also available after setting
`NETLIFY_PAT` and `NETLIFY_SITE_ID` in the process environment:
`cargo test --locked --features desktop --lib live_publish_netlify_digest -- --ignored --nocapture --test-threads=1`.
Use a dedicated test site: this replaces its current content with a tiny
synthetic report, publishes twice to exercise content reuse, and checks the
public page and stylesheet. It is excluded from the `live_deployment_` preflight
filter and all ordinary test runs. Load credentials privately from a local
environment file when needed; never print or commit their values.

Native Windows deployment/layout verification uses
`./ci/desktop-smoke.ps1 -SkipBuild -DeploymentFlows` after building the GUI. It
creates isolated target settings, exercises target persistence and subset
confirmation, checks masked token entry and machine-local credential persistence
across edits and restarts, and verifies credential deletion for Azure and Netlify.
It checks provider switching and Netlify Site ID persistence, captures desktop
and compact light/dark layouts, and verifies missing-token failures without
contacting either provider.
It refuses a locked session before changing preferences and cleans up only its
own fixture credentials, settings, and processes.

## Appearance and Commands

Workspace navigation and global tools share a compact title bar with native
window controls. The brand/version, menus, view tabs, and workspace name use
matching text sizes on one aligned row, with 16px gaps between groups. The view
tabs keep their underline close to the labels. The version is visible at client
widths of 1080px or wider; the workspace name appears at 1400px or wider. Visible workspace
names use normal foreground contrast and truncate within the space left by the
controls; their full paths remain accessible. Action controls use normal toolkit
sizing, and toolbar groups wrap without hiding actions. The minimum client area
is 1000x640; native Windows checks cover both themes there, at 1080x720 and at
1600x1000. The Git badge keeps its full, current status in a tooltip. Source text
size remains independently configurable.

On Windows and Linux, File, Publish, and Help share a toolkit application menu
bar with hover and arrow-key switching. Escape returns focus to the previous
control. File commands operate on the open document; suite context menus and
F2/Delete continue to target the focused suite row. Commands retain their current
busy, validation, and unsaved-change guards. macOS keeps the existing in-window
menus; this does not add a native macOS menu bar.

The header appearance menu offers System, Light, and Dark. System follows the
operating system and responds to appearance changes while the app is open;
explicit Light or Dark overrides those changes. New installations default to
System. Existing `darkMode` preferences retain their previous light/dark choice.
The File-menu toggle remains available and switches from the currently displayed
theme. Native window decorations may still follow the OS on some platforms.

The stock palettes use minimal contrast-only color corrections and visible
keyboard-focus rings. Theme changes preserve open drafts, selection, run state,
and comparison images. Automated checks retain at least 4.5:1 contrast for the
principal text/status pairs and danger-button states; this is not full
accessibility certification.

Source wrapping, line numbers, and text size are saved across restarts. Text size
accepts integers from 10 to 24 pixels, entered directly or adjusted with Up/Down
and the step buttons. Valid sizes apply immediately and are saved automatically.
Invalid text, fractions, and out-of-range values do not change the saved size;
leaving the input restores the last valid size. Reset text size returns it to
14 pixels. These changes do not edit the document. Preference writes are
serialized and coalesced without blocking editing; normal close waits for the
final write. A settings-write error is reported without locking the editor or
discarding a draft.

Open Commands from the header or Ctrl+Shift+P (Cmd+Shift+P on macOS). Search by
command name or category, use arrow keys to select, Enter to execute, and Escape
to cancel. File, edit, view, navigation, and theme actions reuse the normal
workbench commands and their save/confirmation guards. Unavailable actions remain
disabled rather than bypassing the current workspace or document conditions.

## Release Pipeline

The [GitHub Release workflow](../.github/workflows/tuyen-release.yml) has a `build`
choice: `cli`, `desktop`, or `both` (default). The existing platform switches
select targets; selecting none falls back to Linux GNU x86_64. Selecting `desktop`
also schedules a CLI build for each supported desktop target; `both` does not
create duplicate CLI jobs. A `cli` selection still builds only the CLI.

Every application/target pair is an independent parallel job. CLI and desktop
jobs have separate caches and artifact names, and one failed job does not cancel
the others. Desktop builds enable `gui`, which includes the bundled local engine;
CLI feature flags remain unchanged. Desktop jobs have a 90-minute timeout.

Release targets for both CLI and desktop are Linux GNU x86_64 (desktop builds use
Ubuntu 24.04; CLI builds use Ubuntu 22.04), macOS x86_64 and ARM64 (native macOS 15
runners), and Windows x86_64. Musl release targets have been removed: they require
additional native TLS/toolchain work and a compatible managed Chromium runtime.
The current managed-browser implementation explicitly rejects Linux ARM64.

Every build produces `regressify-cli-<target>` or `regressify-desktop-<target>`
archives in ZIP and 7z formats with SHA-256 sidecars. Each archive contains the
executable with `LICENSE` and `LICENSING.md`. Manual runs keep them as workflow
artifacts (`release-<application>-<target>`) and never publish.
Every build checks out the triggering ref, not a hard-coded release branch.

Push a version tag in `precise-alloy/regressify-rs` to publish a release:

```sh
git tag v0.2.1
git push origin v0.2.1
```

Automatic tag pushes build the CLI and desktop app for every supported platform
in parallel. A separate Windows packaging job waits for every build job,
downloads the exact Windows executable artifacts and archives, and creates a
bootstrapper with their archive hashes baked in. Tagged Windows payloads are archived without UPX after their uncompressed
executables are retained for the installer. Tags must be `vN.N.N` or `vN.N.N.N`, with
each numeric component between 0 and 9999. The tag supplies the desktop version,
Windows executable metadata, and installer name/version; three components
normalize to four (for example `v0.2.1` becomes `0.2.1.0`). The Cargo/CLI package
version is not changed. Manual runs, including manual runs on a tag, only build.

Publication is a strict chain. Eight application/target jobs build independently,
while Linux/macOS offline checks run in parallel. Any failed check, build, or
Windows packaging job prevents publication.

1. `publish` stages the complete expected inventory: 16 ZIP/7z archives (four CLI
   targets and four desktop targets), 16 named SHA-256 sidecars, and one Windows
   bootstrapper. Unknown, duplicate, missing, symlinked, empty, oversized, or
   checksum-mismatched assets fail before release creation. The generated
   `latest.txt` adds version, tag, installer metadata, and
   `archive.<filename>=<size>:<sha256>` for every archive. The client limit is 1 GiB
   per binary and 16 KiB for the manifest; binary assets do not go to Cloudflare.
1. Only the `publish` job has `contents: write` and uses the repository's built-in
   `GITHUB_TOKEN`. It creates a draft, uploads all 34 files, then checks GitHub's
   exact asset names, uploaded state, sizes, SHA-256 digests, and URLs before
   publishing. Reruns may fill missing draft assets but never overwrite bytes.
   A published release must already match completely; failed lookups are not
   treated as permission to create a new release.
1. `deploy` depends on successful publication. It downloads the verified inputs
   and checks the published GitHub inventory again **before building the site**.
   It extracts the released Linux CLI, builds the website from the same manifest,
   exports and tests the MCP Worker, and deploys one `regressify` Cloudflare Worker.
   Only `latest.txt`, installation scripts, website assets, and `/mcp` are hosted
   there. The final credential-free feed check verifies the installer metadata.

Previous release binaries remain downloadable from GitHub. The website/feed
selects the version associated with its deployment, not GitHub's mutable latest
redirect. Configure `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` repository
secrets in `precise-alloy/regressify-rs`; only the deploy step receives them.
The `eshn.dev` zone must belong to that account. Workflows serialize releases
without cancelling an active deployment. Publishing from other repositories and
manual runs is blocked. Azure release publishing has been removed.
The installer is compiled with Inno Setup 6. Release CI requires both executable
payloads, verifies both binaries' static-runtime dependencies, and runs isolated
native PATH tests before packaging. Its installation smoke checks also exercise
CLI lookup through the registered user PATH, upgrade idempotence, PATH cleanup,
visible-update foreground requests, fully silent setup, active-process blocking,
downgrade rejection, shortcuts, folder context menus and uninstall cleanup before
upload. Windows may deny
foreground activation on a noninteractive runner; the smoke test reports that
as a warning while still requiring the activation request. Local packaging requires
both executables in `-BinaryDirectory`, with the desktop executable's version
matching `-Version`. For an isolated machine authorized for installation testing:

```powershell
$env:REGRESSIFY_DESKTOP_VERSION = '0.2.0.0'
cargo build --locked --no-default-features --features gui --bin regressify-desktop
cargo build --locked --no-default-features --bin regressify
./ci/build-installer.ps1 -Version 0.2.0.0 -ReleaseTag v0.2.0 -BinaryDirectory target/debug -Compiler 'C:/Tools/Inno Setup 6/ISCC.exe' -SmokeTest
./ci/desktop-smoke.ps1 -SkipBuild -UpdateFlows
```

The installer smoke test refuses to run over an existing per-user installation,
shortcut group, context-menu registration or PATH ownership record. It installs only under a generated
`target/installer-smoke-*` directory, suppresses relaunch, verifies both shortcuts
and their Jump List identity, and checks the folder context-menu commands.
Chromium requests in the smoke child processes use an unreachable loopback proxy:
setup must reuse an existing cached build or log a non-fatal network failure,
without downloading a browser. The fixture also checks `/SKIPCHROMIUM=1`.
Uninstall must remove both executables, the license files, the owned PATH entry,
fixture shortcuts, context menus and installation registration, preserving the
original user PATH value and type and leaving the machine PATH unchanged. Fresh
installation must also copy the repository `LICENSE` and `LICENSING.md` unchanged.
The smoke helper waits for the complete installer/uninstaller process tree with
a two-minute execution deadline and retains `uninstall.log` in the fixture.
Inno's first-phase uninstaller can exit before its second phase finishes PATH
cleanup; waiting only on the launcher can falsely report leftover metadata.
The native UI smoke requires an unconfigured development build and
temporarily backs up/restores preferences, like the other desktop smoke modes.

The opt-in native Jump List test uses a unique application ID, temporary workspace
history and automatic cleanup without modifying the installed app's Jump List:

```powershell
cargo test --features gui-tests --lib desktop::jump_list::tests -- --include-ignored --test-threads=1
```

Release tests run alongside the build matrix. To run them locally, install
`PyYAML==6.0.3` for Python 3.11+ and run `python ci/test-release.py`. Bash and
PowerShell 7.4+ (`pwsh`) are also required; set `BASH_PATH` to Git Bash on Windows.
The tests execute all 48 application/target selections, stub Cargo
to check the real workflow arguments and failure handling, and verify manifests
against offline fixtures without publishing.

On Windows, also set `ISCC_PATH` to an Inno Setup 6 compiler to enable
`python ci/test-release.py InstallerTests`. These tests compile the production
installer with synthetic payloads and execute the PATH logic against isolated
temporary registry keys. A synthetic setup/uninstall cycle exercises the actual
uninstall callback and smoke process helper inside a temporary directory, without
changing the real user PATH or app registration. Process fixtures also verify
exit codes, argument/proxy preservation and timeout cleanup, including a surviving
second phase after its launcher exits. The Windows packaging job runs these
checks with `ISCC_PATH` set before the full installation smoke test.

### Bridge Rollout

Do not publish the final Tuyen Drive bridge until the new GitHub release and site
have deployed successfully and the maintainer confirms the site is working.
The bridge will be prepared separately from commit
`d066dc81a2befb9cb622762cbf3c7a21e8c198fc`; no bridge branch, tag, or upload is
created by this change. That commit alone still reads Tuyen Drive and must receive
the new updater/feed, GitHub redirect, payload preparation, and cache-migration
support before it can bridge clients.

Keep the bridge download on the old feed with its old accepted
`Regressify-Desktop-Setup-<bridge-version>-x64.exe` filename and stable AppId.
Old clients cannot follow a redirect or accept a new-host installer URL. The
bridge's installed app then reads the website feed and prepares the new
`Regressify-Setup-<site-version>-x64.exe`. Preserve preferences and update caches;
discard already-installed legacy feed entries before validating newer URLs.
Choose versions so `old version < bridge version < site version`; publication
chronology alone is insufficient for numeric update checks. Do not publish the
bridge as the website's newest release. Retain the old bridge feed and payload
for clients that return later. A manual current install remains an alternative.

## Workspace and Files

Displayed filesystem paths use forward slashes and omit Windows extended-length
prefixes: `\\?\D:\project` appears as `D:/project`, while a network share remains
`//server/share`. This is display formatting only; filesystem access, safety
checks, and saved paths keep their original semantics.

The navigator has separate, vertically resizable Test suites and Scenarios panes.
Selected rows are highlighted without checkboxes; a left-edge marker identifies
the suite or scenario currently open. The lower pane lists only the open suite's scenarios, with
labels and URLs; actions are edited in the scenario's action sequence rather
than nested in the sidebar. Both panes scroll independently.

Click a row to select only that item in its list. Ctrl-click toggles individual
items (Cmd-click also works on macOS), and Shift-click selects a range from the
last clicked item in visible list order. Ctrl+Shift-click adds a range to the
existing selection. Hidden filtered rows are excluded from ranges.
Click empty list space or None to clear selection; All selects every valid suite
or every scenario in the open suite, including filtered-out items. Scenario
selection changes only the open suite's scope, preserving other selected suites.
Clearing selection does not close the editor or discard its draft.

Click a list row or tab into the list to use its keyboard controls. In the suite
and scenario lists:

| Shortcut                    | Behavior                                                         |
| --------------------------- | ---------------------------------------------------------------- |
| Ctrl+A                      | Select visible rows, excluding filtered-out items                |
| Ctrl+Shift+A or Escape      | Clear selection without closing the document                     |
| Up/Down, Home/End           | Move selection to the previous, next, first, or last visible row |
| Shift + navigation key      | Extend or shrink the selection range                             |
| Ctrl + navigation key       | Move focus without changing selection                            |
| Ctrl+Shift + navigation key | Add a range to the existing selection                            |
| Space or Ctrl+Space         | Toggle the focused row                                           |
| Enter                       | Open the focused suite or scenario                               |

Use Cmd instead of Ctrl on macOS. Scenario selection remains scoped to the open
suite; other selected suites are preserved. The toolbar's All button still
includes filtered-out rows. Shortcuts in search boxes, document fields, and
source/log editors continue to operate on their text, not the surrounding lists.
Selection shortcuts respect active-operation locks.

Right-click a Test suites row for Rename suite or Delete suite. With that panel
focused, F2 renames the focused suite and Delete opens its deletion confirmation.
These commands target one row, including in Results; they do not apply to every
checked suite or intercept keys in text inputs. Active runs block both operations.
Right-click changes the focused row without opening or checking it. Escape closes
the menu and returns keyboard control to the suite list; catalog refresh closes
an open menu so its actions cannot target a changed list.

New suite (`+`) and Rename suite focus the filename input and select the name
without its `.tests.yaml`, `.tests.yml`, or `.tests.json` suffix. Start typing to
replace `new-suite` or the current name. Enter submits from the filename input,
Rename's Display name input, or the focused Create/Save button. The top-right X,
Cancel, and Escape close without saving; Enter on a focused Cancel or X also
cancels. Invalid or existing names keep the dialog open for correction. During a
write, submission and dismissal controls are locked until the operation finishes.

Runs > Suites supports Ctrl+A, Ctrl+Shift+A, and Escape for its checklist. Git's
Changes and Comparison Changes trees support the same selection shortcuts within
the focused group, keeping comparison artifacts separate and excluding conflicts
and submodules. In Git, Up/Down moves the cursor and wraps at the ends; Home/End
moves to the first/last visible row. Left/Right collapses/expands a folder,
Space toggles checked selection, and Enter opens a diff or toggles folder
expansion. Cursor movement alone does not open a diff or check a file. These
keys never commit or discard files, and selection remains locked during operations.

Viewports and Captures remain single-selection lists with Up/Down and Home/End
navigation. Workspace files and action sequences use these keys to move focus,
then Enter or Space opens the focused item. Keyboard navigation scrolls off-screen
rows into view and retains the existing unsaved-draft guards.
Workspace-file and action rows are virtualized; Up/Down wraps at their ends,
while Home/End moves directly to the first/last row without opening it. Refresh
keeps workspace selection by file path rather than a formerly occupied row.

In Results, the lower pane becomes Captures for the active suite. Selecting a
suite there changes the report being reviewed without opening another editor
document or disturbing an unsaved draft.

Selecting a suite selects all of its scenarios; clicking an individual scenario
narrows that suite's execution scope and opens its editor. The sidebar's play
button runs only that row's scenario. Runs > Suites still expands each suite into
scenario checkboxes with direct Test and Reference buttons. Clearing the last
scenario disables suite commands rather than reverting to an all-scenario run.

Overflowing areas show persistent, draggable scrollbars in both themes, including
navigation, settings, action sequences, activity, source/log editors, and result
lists. Image previews have horizontal and vertical scrollbars when the selected
zoom exceeds the available space. Side-by-side previews share both scroll offsets;
scrolling or dragging a thumb in either pane moves both images together.
Test suites, Scenarios, and Landing apps keep their scrollbar tracks fixed to
the viewport while content scrolls, so the thumb remains reachable at either end.

Suite search matches display names, paths, and scenario labels, IDs, or URLs across the workspace,
but displays matching suites only. Open a suite to see its scenarios below. The
lower search filters only that suite, without changing the suite search or run
selection. Selected narrows the upper list to selected suites. Ctrl+P (Cmd+P on
macOS) focuses suite search. Navigation retains Save/Discard/Cancel protection.
Suite rows also show queued/running/pass/fail totals. The refresh icon reloads
suite discovery. Scenario rows shorten web addresses to host/path and local
fixture URLs to workspace-relative paths; tooltips retain the full URL.

Unlabelled scenarios use the same [URL-derived names](create-scenario.md) in the
suite catalog, live draft list, editor heading, and action breadcrumb. Names use
the shortest unique readable path suffix, with origins only for mixed-origin
suites. Explicit labels take priority; generated names do not modify the draft,
scenario selection IDs, or capture filenames.

The Workspace files section contains workspace configuration and every regular
file under `common`, including nested files, JavaScript helpers such as
`_on-ready.js`, CSS, images, and files without extensions. UTF-8 text files up to
4 MiB open in the built-in editor regardless of extension. YAML/JSON retain their
configuration editing; other text files use Source only. Binary, non-UTF-8,
unreadable, and oversized files remain visible but disabled, with a reason in
their tooltip. Suite-like filenames under `common` remain in this section rather
than the runnable suite list. Opening a viewport file displays its
definitions in the lower pane. Open configuration file adds other workspace-relative
data files, including custom viewport definitions.

File supports new/duplicate/rename/delete suite, Save As, reload, configuration
and viewport-file creation, file browsing, refresh, and light/dark appearance.
Open in VS Code focuses the VS Code window already open on the current workspace
folder, or opens a dedicated window when that folder is not already open. Other
VS Code workspaces and their sessions remain unchanged. Regressify launches the
installed editor directly instead of using a `vscode://` link, avoiding the
external-URI permission prompt. Normal VS Code workspace-trust prompts still apply.
VS Code must be installed in a standard location or have its executable/CLI `bin`
directory on `PATH`; Windows supports both user and system installations without
requiring its URI handler. Missing installations and launch failures appear in
the desktop status message.
Open in File Explorer (Windows), Open in Finder (macOS), or Open in File Manager
(Linux) opens the workspace folder in the operating system's default folder viewer.
These actions require an open workspace and do not save or discard editor drafts.
Save As takes a workspace-relative destination and never replaces an existing
file. Destinations under `visual_tests` use suite-name and suite-content
validation even when saving a Data template. Windows directory casing and
in-workspace path aliases cannot bypass these checks. Suite filenames must use
`.tests.yaml`, `.tests.yml`, or `.tests.json`; their names cannot start with `.`
or use the reserved names `all`, `_snapshot`, `_diagnostics`, `_landing`, or
`states` (case-insensitive). Dot-prefixed helper files under `common` remain
allowed. Reload discards a draft after confirmation. Suite-file deletion is
confirmed but cannot be undone through document history.

The open file is checked for external changes about once per second in the
background. Saving it in VS Code or another editor automatically reloads a clean
document, including YAML, JSON, JavaScript, CSS, and other editable UTF-8 text.
Reload retains the current editor tab and any selection that still exists;
syntax-invalid configuration opens in Source with diagnostics so it can be
repaired. Unchanged files do not reset the cursor or undo history.

If Source or Fields contains unsaved edits, the desktop keeps them and shows a
conflict banner. **Reload from disk** opens the existing unsaved-changes dialog;
Cancel keeps the draft, while Discard loads the external version. Save As can
preserve the draft separately. Deleted, unreadable, binary, or oversized files
leave the current content intact with a warning; checking resumes automatically
when the file becomes readable again. No external changes are written back to disk.

The Rename suite dialog edits Filename and Display name independently. Display
name is the optional suite-level `displayName` property, also available in
Fields > General. Nonblank values label the desktop app and snapshot site;
omitted or blank values fall back to the filename-derived name (or report ID).
Changing this label does not change the file on disk, suite ID, capture names,
CLI filters, or site routes.

Renaming a filename-derived suite moves its references, every saved test run,
and comparison reports to the new output ID. An explicit `id` stays unchanged.
Deleting a suite removes its corresponding references, test history, and local
report entries. Published local snapshot data and cache-versioned links are
updated; unrelated suites and browser states are preserved. Saving a display
name also refreshes existing local report labels without recapturing.

Operations cover `.regressify`, workspace `.regressify__<environment>` folders,
and custom output roots selected or recorded in desktop activity. Unknown custom
roots cannot be discovered automatically. Shared/ambiguous IDs, destination
collisions, unsafe links, malformed reports, and external document changes stop
the operation. Updates are staged with rollback on ordinary I/O failures; avoid
running CLI writers concurrently. They are not crash-atomic. Remote deployments
are never modified automatically; redeploy to publish the updated site.

The title bar displays the package version on the same line as Regressify at
client widths of 1080px or wider; narrower windows hide the title-bar version.
The workspace name uses the same text size as the menus, with a small gap after
Results. Help > About always shows Regressify Desktop, its version, contributors
(author and lead developer Tuyen Pham, developer and docs maintainer Dinh Pham,
developers Hung Le and Dung Nguyen Tien, and QA advisors Nga Nguyen, Hanh
Nguyen, and Duyen Pham), and
the clickable website `https://regressify.eshn.dev/`, which opens in the external
default browser. It also shows the copyright and no-warranty notices, a
**GNU AGPL v3 or later** link to the licensing terms, and a link to the public
source repository.
There is no bottom application or Results status bar. Application status and
Report status icons in their respective headers retain messages in tooltips and
accessibility labels, leaving the bottom edge available for content.

Opening another document/workspace or closing with a draft prompts Save,
Discard, or Cancel. A failed save retains the prompt and draft. Recent workspaces
and appearance are stored outside the project in the
operating system's per-user Regressify Desktop configuration directory.

Multiple desktop instances can run at once, each with its own workspace. Empty
windows are also independent. Opening a workspace already owned by another
instance restores and focuses that existing window instead of opening a duplicate.
This applies to startup paths, the folder chooser, and recent workspaces. A
duplicate startup exits successfully after handoff; an existing requesting window
keeps its current workspace, draft, selection, and run. Reopening the current
workspace also preserves its draft rather than reloading it.

Each window still has one active workspace. Close workspace unloads documents,
run options, selections, logs, and previews and releases its ownership without
closing the window. Switching to a different unowned workspace retains the normal
save/discard guard and requires active runs to finish or be cancelled. The old
workspace remains owned until the new workspace loads successfully.

Ownership uses the canonical workspace path, resolving relative and symlink
aliases; Windows paths are also case-normalized. Per-workspace file locks and a
token-checked, loopback-only activation endpoint live in the user's configuration
directory, not the project. Closing or crashing releases the OS lock, and the next
owner replaces stale activation data. An unresponsive owner is reported rather
than bypassing the lock. Bundled engine workers do not acquire GUI ownership.
Use the updated executable for all windows; an older globally locked version
cannot receive the new activation requests.

Preferences remain per-user. Concurrent writes are serialized and recent-workspace
entries are merged so separate windows retain each other's history. Appearance
and editor defaults use the last saved settings; existing windows keep their
current appearance until changed or reopened.

Opening a folder automatically creates missing runtime and agent files before
displaying the workspace. No Initialize workspace command or confirmation is
required:

- `regressify.yaml`: an empty mapping (`{}`), inheriting the built-in defaults.
- `common/_viewports.yaml`: phone, tablet, and desktop viewport definitions.
- `common/_cookies.yaml`: an empty cookie list.
- `common/_override.css`: a CSS override placeholder.
- `common/_on-ready.js`: a browser JavaScript placeholder.
- `common/_replacement-profiles.yaml`: an empty default URL replacement profile.
- `visual_tests/`: an empty suite folder, without adding a demo suite.
- `.vscode/mcp.json`: the hosted `regressify-mcp` HTTP server configuration.
- `AGENTS.md`: a managed `## Regressify CMP` section directing agents to use the MCP.

Existing runtime defaults are never overwritten, including unfinished or invalid drafts.
An existing `regressify.yml` or `common/_replacement-profiles.yml` is used without
creating a higher-priority `.yaml` file. Reopening restores missing defaults.

MCP setup adds a missing server or refreshes its URL while preserving other
servers, settings, and JSONC comments. Agent setup appends a missing section or
refreshes outdated instructions through the next top-level H2 or EOF, preserving
surrounding content. Current MCP and agent configurations are not rewritten.
See [workspace initialization](commands.md#workspace-initialization) for the
shared preservation rules and Markdown freshness checks.

The generated instructions tell agents to read and maintain site-specific lessons
in the consumer project's `.ai/memory/`. Each note is at most 60 lines and covers
one specific behavior or issue on one site, with a compact linked index. Agents
retain verified fixes, failed approaches, and evidence without secrets or raw
logs. Desktop setup neither creates memory files nor rewrites existing notes.

Setup runs in the background after workspace ownership and unsaved-work checks;
blocked or unsafe paths report an error and keep the current workspace open.
Malformed MCP configuration or an agent draft that cannot safely accept a section
also fails setup without replacing that file or switching the current workspace.

Automatic setup does not export schema JSON, add `.vscode` settings or extensions,
copy Node/Backstop helpers, or create reports and saved browser states. The desktop
uses its built-in schema metadata; capture and report artifacts are generated by
their commands. Custom paths explicitly configured by the project must still
point to real files.

The explicit CLI `regressify init` command, also available as Initialize workspace
in Runs, additionally creates an example suite, exported schemas, and VS Code
settings/extension integration, and copies missing legacy `data/` files. It shares
the runtime defaults and managed MCP/agent setup above.

## Editing

The navigator can be hidden for a wider editor or comparison preview; its
visibility is saved with appearance preferences. Editor, Runs, and Results keep
their state while switching views. Document and parent-scenario breadcrumbs
navigate without changing source or bypassing draft validation.

Find a setting searches names, keys, descriptions, and groups, including advanced
settings. Overrides only narrows the form to explicit values. Groups can be
collapsed independently or expanded together without changing the draft. Search
matches and field errors, including unsupported choice values, keep the full
matching group open without overwriting its collapse preference. When those
conditions clear, the preference is restored. Disclosure labels support Tab,
Enter, and Space; correcting the last error returns focus to the label if the
group closes. Failed suite activity also remains expanded while preserving a
requested collapsed state.

Numeric
steppers start from the effective inherited value; mismatch percentages remain
within 0-100 and concurrency counts remain positive. File pickers sit beside
their input. Field-issue links reveal the affected control even under filters.

Single-choice fields use searchable selectors. Inherited values are displayed
without creating overrides; explicitly confirming a value creates an override.
Reset removes the override and updates the displayed inherited selection.
Unsupported values remain visible until explicitly replaced or reset.

Copy controls for saved deployment URLs, Git logs and source diffs, update errors,
and capture summaries briefly show a check after copying. Repeated copies of
unchanged text are suppressed for two seconds; changed content can be copied
immediately. Copies preserve the full text, not truncated labels or line numbers.
Unavailable URLs and incomplete or empty log pages retain their existing copy
and export restrictions.

Pixel color threshold (`pixelMatchThreshold`) is available in the Capture group
for workspace, suite, and scenario Fields, without enabling Advanced. It ranges
from `0` to `100`, defaults to `5`, and steps by `0.1`. Lower values highlight
subtler color changes; Mismatch threshold (%) only controls pass/fail. The pixel
threshold is divided by `100` internally, so `5` means a normalized tolerance of
`0.05`. Inheritance and Reset work as for other numeric settings, including an
explicit `0` override. New captures save the resolved value; comparing a saved
run reuses that run's threshold.

Image simplification settings also live in the Capture group for workspace,
suite, and scenario Fields. At the workspace or suite root, unset values appear
with Advanced or a settings search, such as `image simplification`. Image
simplification chooses `none` or `pixelate` and shows the inherited choice until
overridden. Pixelation block size (px) steps between even values from `2` to
`16`, moving an odd draft to the adjacent even value; Minimum image size (px)
steps by `1` from `1`. Excluded image selectors takes one CSS selector per line,
and an explicit empty list clears inherited exclusions. Odd or out-of-range
sizes, zero minimum sizes, and blank selectors stay in the draft as validation
issues and block Save, matching CLI validation. Enabling simplification or
changing any of these settings requires new reference captures. Until then,
comparisons print a settings warning in All run logs without changing results;
Results does not mark these captures yet. See
[create a test suite](create-test-suite.md) for runtime behavior.

Viewport and dependency lists retain free-form input and also offer searchable
multi-select controls. Each accepted toggle is one undoable edit; the picker stays
open, and Escape closes it without rolling back edits. Viewports load asynchronously from the effective viewport file,
including custom names and dimensions. Missing scenario selections inherit their
configuration; an empty resolved selection means all configured viewports. The
picker keeps at least one selected. Unknown
existing values remain editable and appear as unavailable choices instead of
being silently removed. Viewport matching ignores case; dependency matching is
case-sensitive. Dependencies
use IDs from the current unsaved suite and exclude the current scenario and
ambiguous duplicate IDs. Missing or invalid explicit viewport files display a
recoverable warning beside the field.

Every action has an optional `viewportNames` string or list in Fields. Its choice
picker and YAML/JSON Source value completion offer only the consuming scenario's
effective viewport labels and preserve scalar or array values. The action never
adds capture jobs. Unset, `null`, and empty lists apply to all those jobs and do
not add fields to new action templates. Empty strings are invalid.
Unknown or out-of-scope values stay in the draft with a diagnostic, including
inherited `needs` actions. Source/Fields transitions and undo/redo preserve them;
Save and Save As require correction. Contextual validation runs off the UI thread
and discards results for older drafts or documents. The same CLI validator
determines accepted names, rather than a separate desktop interpretation.

Conditional `save` checkpoints appear only for their matching viewports; final
captures still run when all actions skip. See [action viewport selection](create-action.md#viewport-selection)
for the responsive-menu example, strict prerequisite rule, and the caveat for
scoped `persist` writes consumed through another scenario's `state` or `restore`.

Scenario fields include an action sequence with step icons and target summaries.
Open a step directly, move it earlier/later, duplicate it, or remove it with
confirmation. Reorder and duplicate keep the scenario selected and participate
in document undo. The focused action follows a known reorder, and duplicate
actions remain distinct. A stale action command cannot edit a changed document;
ambiguous identities after external edits clear the row cursor without rewriting
the action data. The Add action menu groups navigation, interactions, page
utilities, and capture/state actions. Search settings for `actions` to isolate
the sequence. Input values are not included in the sequence summaries.

The navigation group includes `switchTab`. Add it after a click that opens a
`target="_blank"` link or JavaScript popup, choose `new`, and optionally supply a
destination URL or glob. Following waits, actions, checkpoint captures, and the
final screenshot use that tab. Choose `previous` to step back through selection
history or `original` to return to the initial scenario tab. The Source editor
completes the same choices in YAML and JSON. Tab selection needs no Node runtime.

The workspace opens to an overview with suite/scenario totals, selection counts,
configuration issues, and per-suite status. Edit opens the selected suite; Overview
returns to the workspace without discarding an open draft. The header identifies
the single active workspace and links to live execution while a run is active.

Overview suites use uniform, virtualized table rows. Checking a suite changes
run selection without opening it; a row click only moves the table cursor.
Details, or Enter on the focused row, opens complete selectable suite metadata,
artifact summary and configuration errors. Copy preserves the full text, including
content too long for the row. Edit and Open Report remain separate actions.
Refreshing the catalog preserves the cursor by suite file path, not row number.

Fields provides typed controls derived from the Rust schemas. Advanced reveals
optional root settings. Source exposes complete YAML/JSON with highlighting,
selection, native text editing, and search, and opens other text files without
configuration highlighting or validation. Effective uses the real CLI resolver.

Related fields are grouped into General, Capture, Session and dependencies,
Execution, Page setup, and Behavior. Short values use a two-column layout; URLs
and lists use the full width. Document actions stay with the editor. Light and
dark appearances share restrained teal accents and consistent native typography.

The Add menu supports scenarios, viewports, and all 25 action types. Selected
items can be duplicated, reordered, and removed. Removal is confirmed and
undoable. Compound changes form one undo step. Scenario copies omit explicit IDs
and receive unique labels; suite copies omit the suite ID to avoid sharing output
artifacts. Renaming preserves IDs.

All serialized configuration and action fields are represented by generated
controls. Scenarios/actions have dedicated collection editors; Basic Auth,
URL replacements, and other object collections use structured YAML/JSON input. Generated runtime `testDate`
metadata remains available in Source. Frame chains, selector lists, dependencies,
restore state, and uploads retain the existing serialization shapes.

File fields offer native pickers. Configuration paths are workspace-relative;
input uploads are relative to `visual_tests`, including multi-file selection.
Out-of-base files are rejected. If the selected document/item changes while a
picker is open, its result is discarded rather than applied to another field.

### Inheritance

Absent properties inherit. Explicit `false`, zero, an empty string, or an empty
list is an override. Inherited fields show their effective value and source
(workspace, suite, or default); explicit values show Override. Reset removes the
property. Optional action fields do not inherit unrelated scenario settings.

Configuration cascades from scenario to suite to workspace to defaults. State
and restore use the CLI's merge behavior rather than simple replacement. Live
effective values update with guided edits. Unresolvable configuration produces
a diagnostic rather than a guessed value.

### Source and Save Safety

Editor tabs are ordered Source, Fields, and Effective. Selecting a test suite in
the editor opens Source, including reselecting an already-open suite. Selecting
a scenario or action opens its Fields. Suite selection while reviewing Results
stays in Results and switches the displayed suite.

JavaScript, CSS, and other non-YAML/JSON text files have only the Source tab, with
search, undo/redo, Save, and Save As. Empty text files are editable. Saving keeps
the source bytes, including comments and line endings, and detects external edits
or deletion before replacing a file. Save As does not overwrite an existing file
or convert between plain text and YAML/JSON; use a matching destination type.

Switching Source, Fields, and Effective reads pending input before changing the
view. Invalid source stays in Source when Fields cannot represent it, with its
diagnostic retained. File-menu actions use the same availability rules as command
search, including disabling suite deletion for unsaved documents.

Corrected YAML/JSON can be saved even when the file originally contained syntax
errors. Editor diagnostics describe the current draft; the suite list describes
the saved file and refreshes after a successful save. If the original source
cannot be parsed, the repair leaves existing report labels unchanged because
their previous metadata cannot be determined safely. Draft validation and
external-change protection still apply.

Blank or comment-only `regressify.yaml` and `regressify.yml` files use defaults,
just like `{}`. Opening, validating, and saving preserve their source text. Fields
can add the first setting, and undo restores the original text. Explicit `null`
or `~` values remain invalid, as do blank suite, viewport, structured data, and JSON documents.

Source has local YAML/JSON schema completion. Suggestions appear while typing
property names, supported values, and documented examples; Ctrl+Space opens the menu
explicitly (also Ctrl+Space on macOS). Arrow keys choose an item, Enter or Tab accepts,
and Escape dismisses. Keys follow the schema through nested objects and lists,
including Basic Auth, URL replacements, iframe settings, and navigation options.
Required fields appear first; existing sibling keys and unrelated action fields
are excluded. Suggestions include the expected type, descriptions, units, and
safe examples. Structured examples insert valid YAML flow notation or JSON.
Accepting a suggestion replaces only the partial token, preserving surrounding source,
including text after emoji on the same line.
With suggestions open, Shift+Tab dismisses the menu without editing. Without
suggestions, Tab indents and Shift+Tab outdents as usual. Quoted viewport names and
hyphenated labels keep their suggestions while typing. Returning from
another window restores lost Source focus for the same visible document in the same workspace, so you can
accept or dismiss suggestions without clicking the editor. Focus in another control
is preserved.

Hover over a field or value for its schema description, required/optional status,
expected type, and examples. These hints contain schema metadata, not entered
credential values. Arbitrary data files, JavaScript, and live-page CSS selectors
are not completed. JSON files and Effective use JSON highlighting; YAML uses YAML.
Other text files use plain-text editing without schema diagnostics or hover help.
Suggest is disabled for arbitrary data and plain-text files. Switching away from
a schema document closes its suggestions and clears the old providers; YAML/JSON
data files retain their syntax highlighting.

Schemas are generated automatically from the Rust types with `schemars`.
Field documentation, examples, and constraints live beside those types and feed
both the desktop editor and schemas exported by `regressify init` for VS Code.
There is no separately maintained desktop schema, network schema lookup, or
language-server installation. Validation uses an offline, cached JSON Schema
validator plus the existing semantic checks. Tests check field-help coverage and
validate documented examples against the generated schemas.

Duplicate YAML/JSON properties remain invalid Source drafts rather than being
silently collapsed by Fields. Save and Fields navigation stay blocked until the
duplicates are repaired. Scenario dependencies are checked even without action
viewport filters; dependency errors do not hide independent viewport errors.
Semantic errors such as blank URLs, invalid replacement regexes, and duplicate
viewport labels point to their source fields alongside schema errors.

Live Source checks start after a roughly 200 ms typing pause and run in the
background, with at most one check in progress per window. New edits replace
pending work, and stale results cannot overwrite current feedback. The status
strip shows **Checking source...** rather than a green check while feedback is
pending. Scenario/action selection commands wait for current feedback; text
editing and undo remain available. Save and tab changes still read the latest
buffer instead of trusting a delayed result.

For example, a Basic Auth entry without `origin` reports
`basicAuth[0].origin: Missing required field`, followed by an origin example and
an explanation. A complete entry is:

```yaml
basicAuth:
  - origin: https://example.com
    username: "${BASIC_AUTH_USERNAME}"
    password: "${BASIC_AUTH_PASSWORD}"
```

Set those environment variables before launching Regressify. The origin includes
the scheme and optional port, not a page path. Each entry needs all three fields;
the single-object form is also supported. In the editor, indices such as `[0]`
refer to the first list item. Generated-schema diagnostics never echo entered
values. Runtime checks still determine whether files, environment variables,
websites, and elements actually exist or work. `init` remains non-destructive:
it does not overwrite existing exported schemas or VS Code settings. The desktop
uses the schema built into its current version, independent of those files.

The source toolbar provides an Edit source lines menu, Find, Replace, and Suggest,
plus word wrapping and integer text sizes from 10 to 24 pixels. Wrapping, line-number,
and text-size changes preserve source and undo history.

Line commands operate on the current line or all lines touched by the selection.
A selection ending at the start of the next line leaves that line untouched.
Commands preserve indentation, Unicode, line endings, and selected text; non-ASCII
whitespace and form feeds are content, not indentation to remove. After
a line command, the selection is forward-oriented with the caret at its end.
Each command is one document undo step. They also work on invalid drafts without
bypassing save validation. Comment toggling uses YAML `#` comments and is disabled
for strict JSON. Effective, logs, and Git diffs remain read-only.

| Shortcut                      | Source Operation                                                      |
| ----------------------------- | --------------------------------------------------------------------- |
| Ctrl+/                        | Toggle YAML line comments                                             |
| Tab / Shift+Tab               | Indent / outdent selected lines; Tab accepts an open suggestion first |
| Ctrl+] / Ctrl+[               | Indent / outdent the current or selected lines                        |
| Alt+Up / Alt+Down             | Move lines up / down                                                  |
| Shift+Alt+Up / Shift+Alt+Down | Duplicate lines above / below                                         |
| Ctrl+Shift+K                  | Delete lines                                                          |
| Ctrl+Z / Ctrl+Shift+Z         | Undo / redo source edits                                              |

On macOS, use Cmd instead of Ctrl and Option instead of Alt for these shortcuts.
Ctrl+Y also redoes source edits on Windows and Linux. Clipboard, text selection,
navigation, and Find/Replace retain the editor's native behavior. Line shortcuts
do not act on the source while a search box or another input has focus.

Source shortcuts, toolbar Undo/Redo, and document commands share one history,
including accepted Fields edits. Adjacent typing or deletion is grouped; moving
the caret, pasting, line commands, structural edits, and saving end a group.
Undo restores the Source selection as well as the exact text. A focused Fields
text box retains local text undo, including invalid input not yet applied to the
document.

The status strip shows the language, cursor position, and issue count. Syntax
errors underline their source line; schema errors identify the
field and offer corrections for missing fields, misspellings, types, supported
choices, and numeric limits. Up to 50 issues are reported together in a scrollable
panel. Each line link unfolds and focuses the affected field, or its enclosing
object's first line when a required field is missing. The issue button moves to the next
issue, wrapping to the first. Fixing the draft clears resolved diagnostics.

`./ci/desktop-smoke.ps1 -SchemaFlows` verifies missing-origin guidance, credential-safe
messages, issue jumps, Enter/Tab completion and Escape after Alt+Tab away and native
reactivation, selected-line comments/indentation/duplication/movement/deletion,
Edit menu focus, undo/redo, save guards, automatic external reload, conflict
cancellation/confirmation, and compact/large light/dark layouts in a synthetic
workspace on an unlocked Windows desktop.

Use `./ci/desktop-smoke.ps1 -SourceFlows` for the narrower Source keyboard/editing,
save, external-reload, and conflict checks. It reuses those assertions and cleanup
from Schema smoke without running the Fields/diagnostic-panel checks.
`-SchemaFlows` remains the complete authoring flow and takes precedence if both
switches are supplied. Run either from a fresh STA PowerShell host, with no other
Regressify windows and no concurrent mouse/keyboard input.

- Guided YAML edits retain existing comments and formatting using a concrete syntax tree. New nested values use valid YAML flow notation.
- Generated edits are parsed and checked against the requested semantic change. Unsupported constructs leave the draft untouched; use Source instead. Multiple YAML documents are rejected.
- JSON edits are pretty-printed. Save As format conversion preserves data, not YAML comments or formatting.
- Invalid drafts cannot save. Invalid field text blocks structural commands until corrected, reset, or undone. Problem links reveal and focus invalid controls; resetting one field preserves other drafts and errors.
- Saves detect external conflicts and use same-directory atomic replacement. Save As never replaces existing files. Paths reject workspace and symlink escapes.
- Immediate saves, breadcrumbs, and structural commands read retained native input, including text whose change event has not run. Save completion acknowledges only the source written; newer edits stay dirty. This is conflict detection, not an OS-wide lock.
- Documents opened or saved are limited to 4 MiB. An oversized Source draft stays visible and unsaved; Save and Fields remain blocked until it is shortened or undone. Pending native edits participate in close, reload, and workspace-switch guards even before their change notification runs. Undo/redo together retain at most 100 exact-source snapshots with a 16 MiB budget.

Valid plain YAML strings such as `.item[aria-label='10 midpoints']` retain their
original spelling during guided edits. Adding suite fields does not require
quoting existing selectors. A YAML 1.2 span adapter protects these strings from
the lossless editor's flow-delimiter parsing, then restores them without changing
comments, Unicode text, or surrounding line endings.

Use trusted local workspaces. Configuration can reference scripts and browser
actions executed by the engine. The desktop is not a sandbox for untrusted projects
or arbitrary executables.

## Running Commands

Completed scenario/job logs load from their capture-owned archives, including after
reopening a workspace or losing the bounded live buffer. The existing View Log
navigation is preserved. Archives load in the background in 500-record pages;
compact previous/next controls and a page indicator navigate the full history,
and Copy copies the displayed page. Filtering or shrinking history reconciles
the selected page without an empty out-of-range view. Shared execution history
is not repeated for each image.

Compare, Approve, Build Report, Optimize, Install, and Initialize retain their own
command journals. View Log uses the operation ID saved with Activity, not the
latest directory or a guessed capture run. Test/Reference activity includes the
command journal alongside original capture journals; per-image Capture Log still
shows only that image's execution. A failed operation retains error severity.

Application startup and native shell/window warnings are retained separately in
`regressify/diagnostics/startup/_diagnostics` under the user's local application
data directory. Those local JSON-line journals are available for support review,
including failures before a workspace window opens. They are not public report
assets and are not automatically deleted. See [diagnostic storage](commands.md#diagnostic-storage-limits).

The view distinguishes loaded, partial/interrupted, expected-but-missing, legacy,
and loading-error states. Live runs still show omitted-retained-history notices;
those notices are not evidence that the durable archive lost records. Missing log
data never changes a capture's pass/fail status.

Runs provides direct Run Test, Run Reference, Approve Results, and Compare
Results buttons. There is no command-mode selector followed by a generic Run.
More actions provides Build Report, Optimize Images, Install Chromium, and
Initialize Workspace. Navigator rows and the checkboxes in Runs support multiple
selections and All. No selection disables suite commands, never an
implicit all-suite run. Invalid suites remain visible for
repair and are excluded from Select all. Editor Run Suite Test and Run Suite
Reference act on the whole open suite, with Approve/Compare in More suite actions.
The scenario editor separately provides Run Scenario Test and Run Scenario
Reference, also available while editing one of its actions. Command search
exposes scenario-scoped Test, Reference, Compare, and Approve actions.
Overview actions operate on checked suites. Command search also exposes direct
open-suite actions. All routes start execution after any draft and artifact confirmations.
Reference capture, approve, and optimize require action-named confirmations
describing the baseline or image changes. Pending option text is read before
command changes and execution; invalid option drafts are retained for correction.

Command search matches a case-insensitive contiguous substring of a command title
or category. For example, `dark th` matches Dark theme, but `theme dark` does not.
Escape clears a nonempty search first; press Escape again to close the palette.
Choosing a command closes search and returns to the workbench before executing
the existing guarded action. Disabled commands remain unavailable.

Opening or selecting a suite automatically discovers its reference images and
saved test runs in the active output folder, using the suite's output ID. The
editor, Overview, and Runs show the detected counts. Discovery runs in the
background and refreshes after commands, workspace refreshes, and output-folder
or environment changes. Refresh saved captures also checks changes made outside
the app. Results from an older workspace or output-folder scan cannot replace
the current inventory.

Run Test is disabled until every selected suite has at least one successful
reference image. The same requirement applies to suite/scenario buttons and
command search; Run Reference remains available for initial setup. Empty folders,
diff images, invalid PNGs, and failed-capture placeholders do not establish a
baseline. References are rechecked immediately before execution, including after
saving a draft. This is a suite-level existence check, not a guarantee that every
scenario, viewport, or saved step has a reference. Missing individual references
remain comparison results. Standalone CLI behavior is unchanged.

When references and test data exist, Open Report is available beside the open
suite, on its Runs/Overview row, and for a compatible suite selection.
An existing report opens directly without starting a worker or changing captures.
If saved screenshots have no usable report, the app compares those screenshots
against the references and opens the result automatically, without launching a
browser or creating a capture run. Tracked suites select their latest completed
full-suite run by recorded completion order; legacy inventory uses screenshot
modification time, not UUID ordering. An explicit Run name selects that exact
run. Suite output IDs and run IDs are pinned together, so another suite with the
same run ID cannot appear accidentally. Existing reports from multiple suites
can be viewed together even when their run IDs differ. If report generation is
needed for different run IDs, use each suite's Open Report control separately.

Run settings is independent of the last command. Search the Environment & output,
Capture, Comparison, Report, Image optimization, and Diagnostics & network groups
before running any action. Browser download has Auto/Always/Never choices;
Compare after test controls whether a test capture is immediately compared.
Auto (the default) and Always use the managed Chromium revision shipped with
Regressify, downloading it only when missing from the shared per-user browser
cache. On Windows this is `%LOCALAPPDATA%\regressify\chromium`; all workspaces and
CLI runs reuse it. Concurrent installations are serialized with a file lock.
Never uses that same cached build without downloading; run Install Chromium once
if the shared cache is empty. Captures never auto-detect installed Chrome or Edge,
and missing or invalid managed executables fail the run instead of switching
browsers. Use the same Regressify version across QA machines and review baselines
again when changing browser revisions.
During installation, Activity distinguishes cache-lock waits, connecting,
downloading (MiB, percentage when known, speed, elapsed time, and idle time),
extraction, and readiness. Status refreshes every five seconds during long steps;
View Log includes the download URL, cache/staging paths, limits, and error details.
The Proxy run setting also applies to Chromium downloads. Network stalls fail
with recovery guidance instead of waiting indefinitely. See
[Chromium installation diagnostics](commands.md#chromium-installation-diagnostics)
for time limits and QA troubleshooting checks.
Invalid settings block only commands that use them. Other drafts stay visible
and retained, and unrelated options are omitted from the execution request.

Each request freezes its command, options, logical suites, and exact scenario
IDs when scenarios are selected. The
run-specific draft prompt offers Save and continue or Run saved version, plus
Cancel. Saving cannot silently switch the requested command. Preflight resolves
those exact suites again, including any newly saved output IDs; it never substitutes
another same-sized selection or expands an empty selection to all suites.

Selecting a scenario in the navigator also selects it for execution. Use Ctrl or
Shift to select multiple scenarios, the direct scenario actions to run one, or
the checkboxes in Runs to adjust scope without navigating. Scenario runs
include all configured viewports and saved steps, and still execute `needs`
setup actions without separately capturing dependency scenarios. Unsaved new
scenarios use the normal save guard; choosing Run saved version cannot run a new
scenario absent from that saved suite. Removed or renamed IDs stop preflight for
review rather than selecting another row. Whole-suite selections use exact
escaped names; compare and approve resolve explicit output IDs and use recorded
scenario metadata, never screenshot filename prefixes.
The selected suite catalog is refreshed immediately before execution. A removed
or newly invalid selection stops the run for review, while updated output IDs
are resolved from disk. Workspace-only commands do not require valid suites.
For duplicate names, `.tests.yaml` takes precedence over `.tests.yml`, then
`.tests.json`. Open-suite actions reject a shadowed file and name the competing path.
Same-priority duplicates and reserved `all` selected-file runs are rejected.

Each command starts a fresh copy of the desktop executable as a headless engine
worker. Validated settings travel over stdin as structured JSON, never through
a shell or an external CLI executable. Process-wide state cannot leak between runs.
Captures generate unique run IDs unless supplied. One command runs per desktop
session; workspace switching is blocked during execution.

With no explicit Run name, Approve Results and Compare Results reuse the last
executed run when its output folder and suite scope match the current selection.
An explicit Run name takes precedence. Subsequent Run Test and Run Reference
actions still create fresh captures rather than reusing that previous run ID.

After Compare Results completes successfully, Results reloads the completed
request's output folder, run, and compared suites, replacing any older report
selection. This updates capture outcomes and suite counts without switching
away from Runs or including unrelated suites that share the same run name.
Failed or cancelled comparisons do not switch the reviewed run; scoped approval
continues to refresh the reviewed report in place.

Stop terminates the process group, including children. Closing during a run
requests cancellation; close again after it stops to handle unsaved work.
The live log is searchable, read-only, and capped at 2 MiB. Its bounded queue may
omit chunks under heavy output; the omission count is displayed. Completion is
tracked separately. Proxy input is masked and command lines are not echoed;
CLI/script logs can still contain project-provided data.

Capture execution runs in a separate worker process. Desktop progress polling
and queue draining remain nonblocking at 100 ms. Progress notifications, redraws,
and live-log refreshes are coalesced to at most once every 500 ms; unchanged
activity redraws only when elapsed seconds change. Completion and explicit user
actions refresh immediately without waiting for that interval.
While a capture is running, hidden log editors are not rebuilt:
opening Log loads the current archive on demand without dropping retained output.
Deployment Activity keeps its visible live log, and completion still refreshes
archive health.

Click a scenario/viewport row in Activity to open its timestamped run log. The
terminal button beside a scenario in the sidebar or Runs checklist opens that
scenario's combined viewport log. These routes work while running, after
completion, and after cancellation. Logs include navigation, action targets,
waits, captures, browser cleanup, and comparison outcomes; typed `input.value`
contents are omitted. Search with Ctrl+F, select/copy text, or use Copy log. Back
to Activity and All run logs retain direct access to the other views.

Scenario messages are attributed to job IDs before concurrent output interleaves.
Their shared retained history is capped at 2 MiB or 4,096 entries, with 16 KiB
per message and an explicit omission notice. It is independent of the console
queue, so heavy console output cannot mix or discard scenario telemetry. The
latest completed run's retained logs are saved with its activity and restored
when the workspace reopens. Starting another command replaces the displayed
activity; its completion replaces the saved snapshot. Historical comparison
reports do not contain execution logs. Custom scripts, URLs, selectors, and
error messages may still contain project-provided data in the local snapshot.

**Command completed means exit zero, not visual success.** Scenario failures,
missing references, and mismatches are report outcomes. Configuration,
infrastructure, and artifact-write errors are command failures.

Activity headlines distinguish failed/passed captures, captures not compared,
stopped runs, and command failures. The Running or Last run label identifies the
executed command even while another command awaits confirmation. Stopped runs
use amber status indicators and offer View Log, not a link to uncompleted results.
After Run Test, Open Report stays available in the Runs header, including while
viewing Log or Run settings. It opens the executed output folder and exact run
with Reference and Current images side by side, never an older report when the
requested run is missing. If Compare after test was off, Open Report compares
that captured run first and opens its report automatically without recapturing
or using the next run's changed settings/selection. Normal draft guards still
apply to that comparison request. Compare This Run remains available after
capture-only execution or approval. Select suites and View Log provide direct
routes from ready and completed states.

Once comparison results are available, the headline and Passed/Failed totals
count **screenshot comparisons**, matching the HTML report. Scenario/job progress
is labeled separately: four scenarios across three viewports can produce twelve
comparison results. A row passes only when all its recorded screenshots pass.
Browser-cleanup or other infrastructure errors remain explicit execution errors
and can make the command fail, but they no longer turn passing image comparisons
into visual failures. Open Report remains available when comparisons were
produced even if the command also reported an infrastructure failure.

The engine emits versioned progress events independently of console logs. Each
scenario/viewport job reports queued, running, capture completion, comparison,
and failure states. Screenshot keys connect visual comparisons to their original
jobs; a scenario is visually passed only after all of its screenshots pass.
Cancellation resolves queued/running jobs explicitly. The worker writes events
to a private temporary file so an overflowing log queue cannot drop status updates.

Activity is the default run view. It shows overall progress, active/pass/fail
counts, expandable suites, and each scenario/viewport's status, elapsed time,
and failure details. Failures only narrows the activity list. Captured is distinct
from Passed, which requires comparison. Suites, Run settings, and Log are separate
tabs; live running/finished counts remain visible in the workspace navigator.

The Test suites panel retains each suite's latest counts when another suite runs
and after restarting the app. These per-suite summaries are saved separately from
the current Activity totals and scoped to the output directory/environment.
Missing summaries are loaded from each suite's latest saved comparison report at
startup, including suites outside the most recent run. Capture-only and cancelled
statuses remain distinct from passed comparisons.

Completed, failed, and cancelled run activity persists across app launches.
Reopening the same workspace restores its last command, suite/scenario selection,
pass/fail counts, viewport rows, failure details, timings, bounded logs, and output
directory settings. It does not restart a worker. The recorded run ID remains
available for report review; the next capture gets a fresh ID. Proxy credentials
are not included in the saved settings.

Activity is saved atomically under `runs/<workspace-key>.json` alongside the
desktop preferences, separately for each workspace. Completion waits for the save;
save/load failures remain visible. This is the latest activity snapshot, not an
unbounded execution history. Screenshots and reports remain in their existing
output directories. If there is no readable snapshot, the app recovers the newest
matching comparison run from the default `.regressify` output. These older results
are labeled Saved comparison results; original execution logs and timings cannot
be reconstructed from reports and are not fabricated.

## Cleaning Test Runs

Use **Clean up old test runs** in the Overview toolbar, Results toolbar, or the
root Fields settings toolbar. The dialog shows the selected output folder,
supports one or more suite checkboxes, and defaults **Completed runs to keep** to

1. Run **Preview**, review the retained IDs and estimated space, then confirm
**Delete old runs**. Changing the selection or count invalidates the preview.
Closing the dialog never deletes data. Cleanup runs off the UI thread and
refreshes Results and saved-run availability afterward.

The keep count is a strict per-suite maximum, with a minimum of 1. The latest
completed full-suite run is protected first; other slots hold the newest completed
runs. Cancelled/incomplete runs are removable and do not consume slots. A newer
scenario-only run can be removed when keeping only the latest full-suite result.
Reference images, browser state, source files, and unselected suites are preserved.
Capture logs shared with baselines or retained runs are protected until no longer
referenced. Active artifact use blocks cleanup, and changed artifacts require a
new preview. A reported deletion failure may leave part of the requested cleanup
unfinished; repeat the preview to retry safely.

In workspace or suite **Fields > Execution**, **Automatic run cleanup** controls
`autoCleanRuns`, and **Test runs to keep** controls `keepTestRuns`. Workspace
defaults are disabled and 1. Suite values override the workspace; explicitly
disable the suite toggle to opt it out. These are ordinary configuration drafts
with normal Save/Reset behavior. Automatic cleanup runs only after full-suite
tests finish, and cleanup errors do not change their completed results.

Each suite's private output `runs.json` records completion independently. Visual
mismatches and website/action errors count as completed; stopped runs and
infrastructure failures do not become defaults. Completing a subset of scenarios
does not replace the full-suite default, but that run remains explicitly viewable.
Existing runs are adopted as legacy completed runs and become eligible for
retention normally. Results, generated reports, and deployment all use the same
completed-run selection. Opening results performs skipped comparisons on demand;
background inventory refreshes do not.

Results does not offer an **Open HTML report** action. The web snapshot requires
HTTP hosting for its root-relative assets, JavaScript modules, JSON requests,
and routes; opening its HTML through `file://` is unsupported. Use native Results
for local review, or generate the snapshot and serve or publish it over HTTP.
**Open artifact directory**, **Refresh reports**, and published **Open Site**
links remain available.

## Reviewing Results

The Capture Log icon opens the selected capture's archived timeline without
leaving Results. Reference and Test select their original executions separately;
the segmented source tabs support pointer selection and keyboard activation.
Changing source starts on its first page and discards stale load results;
the heading shows viewport dimensions and attempt when available. Multiple images
from one execution share its history. Use Back to comparison to return to the
same image and pass/fail result. Legacy and unavailable logs remain explicit.

New capture records display suite/scenario IDs, viewport names, selectors, source
URLs, artifact paths, and capture names instead of opaque hashes. Search those
names or an action index to find the corresponding suite action. Simulated
free-text payloads such as `input.value` are omitted; literal selector text is
source metadata and remains visible. URL credentials and credential-bearing
parameters stay redacted. Older hashed records remain viewable, but require a
new capture to produce readable source metadata.

Activity and Results share severity and stage menus, text search, page copy, and
Download. Filters affect the view only. Download streams all matching records,
not just the visible page, and replaces the chosen file only after verifying a
complete archive. A partial, missing, legacy, or unreadable log cannot be exported
as complete. Active Activity logs offer Follow to keep the latest bounded page
visible without a numbered-page indicator. Turning Follow off loads the last
page-aligned archive page while the worker keeps recording; that page may contain
fewer than 500 records. This is distinct from the rolling last-500-record tail.

Log-health warnings are separate from image results: a passed capture stays
passed when its log is partial or unavailable. Activity's run summary and Results
show the warning as well as the log view. Download and page-export controls remain
disabled with an explicit reason until a complete archive is available.

Display levels and storage limits are documented in [CLI diagnostics](commands.md#diagnostic-verbosity).
Maintenance procedures live in the [Activity skill](../.github/skills/desktop-progress-events/SKILL.md),
[Results skill](../.github/skills/desktop-results-review/SKILL.md), and
[diagnostics skill](../.github/skills/logging-diagnostics/SKILL.md).

Results uses one left navigator with Test suites above Captures and one detail
area on the right. Only the active suite's captures are shown. A suite without a
report shows an empty capture list, never another suite's results. The report
title is read-only; there is no separate report dropdown.
While reviewing Results, suite pass/fail badges use the loaded reports' scenario
and viewport totals, not older saved Activity totals. Saved steps and targets do
not inflate these totals. Live execution counts remain visible during a run.

Approve This Scenario promotes the current scenario's successful captures for
the displayed viewport only, including its saved steps and targets in this report.
Other viewports and scenarios keep their existing baselines; approve each viewport
separately when needed. The confirmation identifies the scenario, viewport, and
exact reviewed run, including historical runs, directly in the current comparison.
Cancel dismisses it without changing baselines; switching captures or refreshing
also dismisses the pending confirmation. Approval stays in Results rather than
switching to Runs, and failures appear in the comparison. Its output folder and suite output
identity are pinned; changes to the suite output or capture metadata stop approval
for review. Failed or missing test captures and reports without scenario metadata
cannot use this action. Valid test captures with missing references or changed
dimensions can be approved; failed sibling captures remain unapproved.

After approval, the same report refreshes in place with recalculated mismatch
percentages, dimensions, suite pass/fail badges, and diff images. The selected
capture, tab, and zoom are retained when the capture still matches the filter.
Recalculation preserves the original
report scope, order, labels, URLs, and thresholds, including all unapproved failures.
It does not capture the website again. Results has no whole-run approval button;
use Approve Results in Runs for suite-wide or all-viewport approval. Ordinary Runs
approval retains its existing behavior and does not automatically recompare.

Capture rows use scenario labels, with viewport and saved-step/target details on
the secondary line. Newly captured display names are preserved in report metadata
and used unchanged by desktop and web reports, including partial comparisons.
Older runs derive friendly names from recorded original URLs using the same
shared rules; when names and URLs are missing, an unambiguous current-workspace
ID match can supply the name. Unresolved generated IDs use an unnamed-scenario
label. Raw capture IDs remain searchable and available in
tooltips. Generated desktop run UUIDs are presented as report timestamps;
timestamp-based CLI run IDs are formatted as local dates. Custom run names remain
unchanged.

The compact comparison heading shows the scenario name, measured difference,
and exact mismatched-pixel count when available.
Hover over the information icon beside the name to see Label, Selector,
Reference URL, Test URL, Diff %, Diff X, Diff Y, and Mismatch pixels. Selector is the recorded
capture target, such as `document` or `body .carousel`. Each recorded URL can
be opened in the external default browser or copied using its adjacent button.
When both URLs match, Test URL displays Same URL but still opens and copies the
actual URL. Diff X and Diff Y are the signed width and height differences in
pixels (test minus reference). Missing metadata displays Unavailable rather
than an inferred value; unavailable URL controls are disabled.
Percentages use the raw floating-point report value, retaining two decimal places
unless a positive difference would appear as zero. Smaller values use additional
decimals or scientific notation, without modifying the saved audit percentage.
Legacy rounded strings are only a fallback when the raw percentage is absent.
Rerun Compare to populate missing pixel counts in older reports; no recapture is needed.
Capture context, allowed mismatch, page, failure details, and raw capture ID are
available in the Copy capture details tooltip and copied summary, without extra
metadata rows above the images. The summary also includes the precise display
percentage and available pixel count. Image panes have no repeated headings or
external-open buttons; image actions are in the context menu. Report totals are
on Refresh reports. Edit suite opens
the owning workspace file through the same guarded navigation used by the
sidebar; it is unavailable when that association cannot be resolved safely.

Results refreshes on opening and after commands finish using current output
options. It loads each suite's `bitmaps_test/<run>/report.json`, newest first,
and shows the latest report for the active suite. Open Report pins its requested
runs, including on refresh and while switching between their suites. Selecting a
suite outside that request returns to latest-report browsing. Refresh retains a
selected capture by its unique capture key even when report rows are reordered;
missing or ambiguous keys fall back within the current filter. Filter
All/Failed/Passed and search labels or errors. The preview follows the filtered
selection and clears when nothing matches. Captures use a continuously scrolling,
virtualized list without pagination. Select a capture directly from the list;
filtering and refresh keep the selected capture visible when it still matches.
The navigator toggle hides both left panes for a full-width preview without
resetting selection, filters, images, or zoom.

Pass, mismatch, engine/capture error, and unknown status are distinct. Unknown
statuses never count as passes. Malformed reports warn without hiding valid
reports. Reports whose suite ID disagrees with their output directory are
rejected. Ordinary browsing reads only the newest valid report per suite;
explicit run requests exclude unrelated reports before applying the discovery
limit of 1,000 report reads, limited to 16 MiB each.

Reference, Test, Diff, Side by side, Wipe, and Overlay display background-loaded PNGs.
Side by side is selected by default and whenever a different scenario is selected.
Switching between captures of the same scenario or refreshing its report preserves
the selected tab. Legacy reports without scenario IDs return to Side by side for
each different capture.
Fit and zoom controls sit to the right of these tabs on the same toolbar.
Fit area is enabled by default and uses the smaller width/height scale so the
entire image is visible, capped at 100% to keep small images at their original size.
Fit width scales to the viewport width, also capped at 100%, with vertical
scrolling for tall images. The two fit modes are mutually exclusive.
The zoom readout shows the actual displayed scale, rounded to one decimal place,
and updates when the fit mode, image tab, capture, or available preview size changes.
Preview canvases are horizontally centered when narrower than their viewport.
These defaults apply to all six image modes.
Each tab independently remembers its fit mode and manual zoom value while viewing
the same scenario, including across report refreshes and viewport captures of that
scenario. Selecting a different scenario, leaving Results, or restarting the app
resets every tab to Fit area. Legacy reports without scenario IDs reset on each
different capture.
Zoom buttons and Ctrl/Cmd+mouse wheel use Chrome-style zoom presets, extended to 800%:
25%, 33.3%, 50%, 66.7%, 75%, 80%, 90%, 100%, 110%, 125%, 150%, 175%, 200%, 250%,
300%, 400%, 500%, 600%, and 800%. Each action selects the nearest preset in the requested
direction, starting from the displayed scale. Fit area remains exact and can
shrink below 25%; zooming out at that size leaves the image fitted.
Ctrl/Cmd+mouse wheel keeps the image point under the pointer fixed. Left-button
dragging pans images that exceed the viewport; ordinary wheel scrolling,
scrollbars, and actual-size reset remain available.
With the image surface focused, `+`/`-` step zoom, `0` fits the window, `1` selects
actual size, and `W` fits width. Arrow keys pan, Shift+arrows move farther, and
Escape cancels a drag. Shortcuts do not intercept editor, filter, slider, or menu
input. Double-click a single-image, wipe, or overlay view to toggle actual size
and fit-to-window.
Wipe reveals Reference on the left and Test on the right, following the pointer
when no button is held. Overlay adjusts the Test image's opacity. Both use a
0-100% slider with arrow keys and Home/End, share the original images' scale and
origin, and are disabled for missing images or capture-error placeholders.
Side by side shows Reference and Test, followed by Diff on the right when a
diff image can be loaded. Clicking an image opens its corresponding tab and
restores that tab's own fit/zoom choice; dragging does not switch tabs.
All comparison panes use a common canvas and scale, including when their
dimensions differ. Every canvas uses the maximum image width and maximum image
height; each image keeps its own dimensions at the canvas's top-left origin.
Zoom, dragging, and both scroll axes stay synchronized from any pane. When the
shared canvas is narrower than the lanes, left-drag moves all canvases together
within the unused horizontal space, stopping when the widest image touches a
lane edge. When every image is shorter than its lane, dragging moves all canvases
vertically together until the tallest image touches the top or bottom edge.
Both axes can move together. Dragging preserves zoom and fit mode without adding
scroll overflow; fit controls restore horizontally centered, top-aligned placement.
Moving the pointer anywhere in a comparison lane, including blank space around
the shared canvas or beyond a smaller image, draws horizontal and vertical guides
at corresponding image pixels in all comparison panes. Each guide spans the full
visible width or height of its lane, stays clipped to that lane, and does not
enlarge the image's scroll range. For an aligned insertion
or deletion, guides account for the saved band position and height; points inside
the band have no counterpart in the shorter image. Unavailable images have no
guides. Guides disappear when the pointer leaves the lane, and selecting another
capture resets the shared scroll position and
alignment. Older reports without alignment metadata retain ordinary image-pixel
coordinates. Rerun Compare for their saved run to add alignment without recapturing.
Image dimensions are bounded
with the CLI's PNG helper before decoding. When a reference capture failed and
no baseline image exists, Reference displays the shared rounded report error
image. Failed Test captures use the same artwork. Older Reference and Test images
with capture-failure markers also display this rounded image in memory, without
rewriting their files. These previews never create or promote a baseline.
Successful reference images and the capture's error message remain intact.
Other missing images, invalid PNGs, and unsafe paths retain their diagnostics.
Paths resolve relative to `html_report` and must remain inside the output root.

Right-click any saved Reference, Test, or Diff image, including in Side by side, to open
it in the default browser, open it in the default image viewer, or reveal it in
the OS file manager. In-memory error placeholders have no file context menu.
Reference also offers Open Reference URL and Copy Reference
URL; Test offers Open Test URL and Copy Test URL. These actions use that capture's
recorded initial scenario URL: Reference uses `referenceUrl`, while Test uses
`url`, which includes the replacements applied when the test ran. These URLs do
not use the final URL after actions or recompute from today's replacement profile.
URL actions are disabled when the corresponding metadata is missing or unsupported;
they are not offered for Diff. HTTP, HTTPS, and local file URLs are supported.
Opening an image menu cancels an active pan without switching panes. Changing the
capture, image mode, report, or Results view dismisses the old menu.

External-open controls also launch the HTML summary and artifact folder.
The app does not embed a browser or webview; browser actions launch the installed
default browser as a separate application. Historical reports refer to current baseline
files; later approval can change their reference image.

## Shortcuts

Use Cmd instead of Ctrl on macOS. Native inputs retain their editing shortcuts;
toolbar Undo/Redo always use complete document history.

Toolbars wrap to keep commands visible in narrow windows. With a toolbar control
focused, Left/Right cycle through its enabled controls; Tab/Shift+Tab use the
normal focus order. Search fields retain their caret keys, and open menus keep
their own arrow-key navigation.

| Shortcut                             | Command                                                |
| ------------------------------------ | ------------------------------------------------------ |
| Ctrl+O                               | Open workspace                                         |
| Ctrl+P                               | Search suites and scenarios; reopen a hidden navigator |
| Ctrl+B                               | Toggle navigator                                       |
| Ctrl+1 / Ctrl+2 / Ctrl+3             | Editor / Runs / Results                                |
| Ctrl+Shift+F                         | Find a setting                                         |
| Ctrl+N                               | New suite                                              |
| Ctrl+S                               | Save                                                   |
| Ctrl+Shift+S                         | Save As                                                |
| Ctrl+R                               | Reload with draft guard                                |
| Ctrl+Z / Ctrl+Shift+Z                | Undo / redo                                            |
| Ctrl+F in Source or Log              | Find                                                   |
| Ctrl+Space in Source (all platforms) | Show schema suggestions                                |
| F2 in Test suites                    | Rename the focused suite                               |
| Delete in Test suites                | Confirm deletion of the focused suite and its data     |
| F5                                   | Refresh files                                          |

## Verification

```sh
cargo fmt --check
cargo test --all-targets
cargo test --features desktop --lib desktop::
cargo test --features gui-tests --all-targets -- --test-threads=1
cargo clippy --features gui --lib -- -D warnings
cargo build --features gui --bin regressify-desktop
```

[Desktop Checks](../.github/workflows/desktop-checks.yml) defines Windows CLI,
native interaction, build, and lint gates plus Linux headless model tests.
GPUI tests use production views and real clicks/typing; they verify events,
state, and layout bounds, not full OS IME behavior or pixels.

### Native Windows Smoke

In PowerShell 7 on an interactive Windows desktop:

```powershell
./ci/desktop-smoke.ps1
```

The [runner](../ci/desktop-smoke.ps1) keeps the public switches, safety guards,
build, and flow dispatch. Its dot-sourced modules share the runner's script scope:

| Module                                           | Responsibility                                                              |
| ------------------------------------------------ | --------------------------------------------------------------------------- |
| [native.ps1](../ci/desktop-smoke/native.ps1)     | Windows interop, window state, screenshots, and native buttons              |
| [controls.ps1](../ci/desktop-smoke/controls.ps1) | Accessible controls, input, themes, and application menu assertions         |
| [fixture.ps1](../ci/desktop-smoke/fixture.ps1)   | Synthetic workspace, process tracking, settings backup, and fixture helpers |
| [cleanup.ps1](../ci/desktop-smoke/cleanup.ps1)   | Process teardown and restoration of preferences and fixture state           |
| [flows](../ci/desktop-smoke/flows)               | Feature-specific acceptance flows and shared Results assertions             |

Keep shared cleanup state script-scoped when moving code into flow functions.
Run [the headless harness checks](../ci/test-desktop-smoke.ps1) with
`./ci/test-desktop-smoke.ps1` after changing the harness. These checks need only
PowerShell 7: they parse and load helpers, exercise switch dispatch and cleanup
with mocked side effects, compile and check native keyboard event sequences
without sending input, and verify draft cleanup state. They do not build the
app, open windows, or touch preferences; native acceptance still needs Windows.
After editing the inline Windows interop code, run native checks in a fresh
PowerShell session because `Add-Type` definitions are cached for the process.

The runner backs up clipboard data into independent storage before smoke input
and restores it in its outer cleanup boundary, even after partial startup or a
failed flow. Unsupported user formats stop tests before clipboard mutation;
missing private formats are ignored only after the fixture owns the clipboard.
Foreground activation targets the exact fixture window and never substitutes
input to another application for a failed focus check.

Add `-Capture` to run direct reference capture, stop a test, rerun the test, and
open the completed results for three local fixture scenarios across two suites.
The fixture reuses the shared managed Chromium cache, downloading the browser only
if needed. The script asserts live pass counts and capture metadata. Test pages
use local HTML only and do not contact an external test website.

Use `./ci/desktop-smoke.ps1 -ScenarioFlows` for focused scenario acceptance. It
implies capture, creates suite references, selects and runs only Checkout,
opens live logs from Activity and the sidebar, reviews results, approves and
compares that exact scenario, and checks sibling reference hashes. It captures
11 light/dark windows at 1600x1000 and 1080x720, including overflowing settings,
logs, and fit-width images. This mode does not run the source-authoring sequence.

Use `./ci/desktop-smoke.ps1 -ArtifactFlows` to verify first-open artifact detection
without Chromium or network access. It checks disabled Test actions with no
baseline, multi-suite gating, direct saved-report opening, comparison of existing
capture-only data, exact suite identity when run IDs match, and detection after
restart. Screenshot and reference hashes remain unchanged. It captures seven
light/dark windows at 1600x1000 and 1080x720 and restores user preferences.

Use `./ci/desktop-smoke.ps1 -ComparisonFlows` for native Results acceptance without
browser capture. It verifies the information popup and URL clipboard contents,
including distinct URLs and Same URL, image context menus, header-only controls,
fit/zoom, drag panning, linked scrolling, and full-lane crosshair pixels in both
themes at 1000x640, 1600x1000, and 1080x720. Image navigation uses the continuous capture list.
All smoke modes also check the title-bar version, File workspace-opening items,
and Help > About. External launch items are checked without opening other apps;
URL copy tests restore the clipboard afterward.

Use `./ci/desktop-smoke.ps1 -InstanceFlows` for multi-instance acceptance. It opens
two different workspaces and multiple empty windows, verifies duplicate startup
and recent-workspace handoff, restores a minimized owner, and checks its unsaved
draft and saved-file hash remain unchanged. It also verifies ownership release
after Close workspace and recovery after terminating only its own synthetic
fixture. Seven native window captures are retained; all fixture processes are
tracked for cleanup and the original preference file is restored.

Use `./ci/desktop-smoke.ps1 -TitlebarFlows` for native window input acceptance.
It uses physical pointer events for Commands, view tabs, navigator visibility,
and Deploy, then drags the workspace-title area and verifies the native
maximize/restore, minimize, and close controls. Closing a dirty document must
show the save guard; cancelling preserves the draft and the saved-file hash.
Seven screenshots include the maximized window and close prompt. Title-bar
controls consume their pointer press so Windows cannot mistake them for a
caption drag; only the surrounding title area remains draggable.

The script builds the standalone desktop executable, creates an isolated synthetic workspace under
`target/desktop-smoke`, navigates fields/source/results with Windows UI Automation,
and runs the bundled engine's init command from the GUI. It captures the target window
directly, checks nonblank pixels, and leaves screenshots for review. By default it runs
no browser scenarios or downloads. `-SkipBuild` uses the existing debug
binaries; `-KeepOpen` leaves the fixture open for inspection.

The native workflow also exercises navigator visibility, expanded comparison,
compact side-by-side previews, actual-size reset, action menus, checked viewport
and dependency choices, and real source-completion acceptance and saving. It
approves a viewed historical run despite a newer capture directory, checks the
promoted image's checksum, compares that exact run, and opens Run settings in
both themes. Input steps verify foreground focus and stable control bounds;
literal fixture text uses Unicode input packets without changing the user's
keyboard settings. Shortcuts use `Send-FixtureKeys` with a `ConsoleKey` and
explicit `-Control` / `-Shift` switches, not SendKeys strings. Native `SendInput`
batches press and release modifiers in order and mark navigation keys as extended;
failed batches attempt to release their keys before reporting the error.
Source-completion setup verifies the caret position between navigation keys
before entering a new property. Native menu items use enabled
state checks and physical clicks rather than assuming an Invoke automation pattern.
Close existing desktop windows before starting the smoke check because the
fixture temporarily changes shared preferences, not because the application
restricts the number of windows.

Unless `-KeepOpen` is set, cleanup automatically discards unsaved changes in the
smoke runner's own synthetic fixture. The test deliberately closes a dirty draft
and verifies normal exit and an unchanged saved file. Normal application windows
continue to show Save/Discard/Cancel for unsaved work.

The native checks also verify selected-suite-only scenario lists, readable names
for generated capture IDs, persisted scenario-label metadata, forward-slash
workspace paths, and live suite totals. Pointer input uses physical screen
coordinates to avoid DPI virtualization errors.

The native theme checks switch Light/Dark/System, exercise the command palette,
capture source and result views in both palettes, and restart the app to verify
saved appearance and editor display settings. They back up and restore the
original preferences file, including recent workspaces. With `-KeepOpen`, the
remaining fixture window retains its test appearance until closed; the original
file is restored when the script returns.

The latest automated gates pass 438 library/UI tests, five executable integration
tests, 72 headless desktop tests, and 293 default CLI tests, plus formatting and
strict library Clippy. The standalone Windows desktop builds successfully.

The multi-instance native flow passes all seven captures and foreground/restore,
draft preservation, independent-window, close/reopen, and crash-recovery checks.
Transport tests cover simultaneous launches, canonical aliases, stale endpoints,
invalid tokens, and oversized requests. The executable integration test verifies
duplicate launchers exit successfully and internal workers bypass GUI ownership.
Window switching and duplicate-open draft guards have production-view coverage;
concurrent preference tests verify both windows' recent workspaces are retained.
Native focus/restore behavior has been checked on Windows, not yet macOS/Linux.

The saved-artifact native flow passes all seven captures, no-baseline action
checks, compare-only generation, unchanged-image assertions, and restart checks.
The screenshots were inspected in both themes and window sizes. GPUI's Windows
accessibility bridge removes disabled buttons' Invoke action but does not expose
`IsEnabled=false`; the native check verifies action availability instead. An
empty Activity view's duplicate Select suites accessibility ID was also fixed.

The preceding focused scenario native flow passed all 11 captures and artifact/log checks.
The screenshots were inspected for visible overflow bars, compact controls,
scenario scope, and log readability; an image Fit width regression found during
inspection was fixed and now has a production-view bounds test.

The report-consistency update adds a real-worker fixture proving that HTML,
saved report JSON, desktop loading, and live comparison totals all agree on nine
passes and three failed captures across twelve viewport screenshots. UI tests
cover report access after execution errors, side-by-side exact-run opening, and
capture-only compare-and-open through the draft guard. The Windows smoke locator
uses Open Report; the saved-artifact native flow above now also verifies the
comparison handoff from the suite editor and after restarting the app.

The full authoring smoke now passes with native keyboard shortcuts, including
viewport and dependency controls, document-start navigation, source completion,
saving, historical-run approval, appearance restoration, and dirty-draft cleanup.
The schema flow also passes with the shared input helper, including clipboard
paste, diagnostic-text copying, completion, and corrected saves in compact
light/dark windows. Both runs restored original preferences and closed only
their synthetic fixture.

Full IME composition, screen-reader usability, and native macOS/Linux behavior still require testing
on those environments. The Linux CI model job is not a claim of native GUI
validation there. Signed installers, cloud accounts, collaborative editing,
browser recording, and non-Chromium execution are outside this implementation.
GPUI 0.6.1 has a remaining managed-tooltip issue after some programmatic view
changes; this does not affect document or result data.
