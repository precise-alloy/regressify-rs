//! What a published report site serves, recorded in the site itself.
//!
//! Every report deployment publishes `/_snapshot/deployment.json` together with the reports. It
//! lists each suite on the site with its summary, capture run, and files, including the hash
//! each provider knows the file by. Later deployments read it back over HTTP to keep the suites
//! they do not change without having those files locally, and a local copy serves as a
//! fallback when the site cannot be read. Because it is published atomically with the content
//! it describes, it never disagrees with the site, even across machines.
//!
//! The file is public: it holds only data the site already exposes (suite IDs, paths, hashes,
//! sizes, run IDs, counts, and versions), never workspace paths, target names, or credentials.
//! Remote copies are untrusted input and are validated before use.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use super::{DeploymentMode, DeploymentProvider};
use crate::snapshot::SuiteSummary;

/// Site path of the state file; kept outside the immutable `_snapshot/data` tree.
pub const STATE_PATH: &str = "_snapshot/deployment.json";
pub(crate) const STATE_FORMAT: u8 = 1;
/// Snapshot report data format understood by the bundled frontend.
pub const DATA_VERSION: u8 = 2;
const MAX_STATE_BYTES: usize = 32 * 1024 * 1024;
const MAX_STATE_FILES: usize = 100_000;
const MAX_MANIFEST_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SiteState {
    pub format: u8,
    /// Report data format of every suite on the site.
    pub data_version: u8,
    /// Regressify build that published the site.
    pub generator: String,
    pub provider: DeploymentProvider,
    /// RFC 3339 publication time.
    pub deployed_at: String,
    /// `deployment_fingerprint` of the whole published site.
    pub fingerprint: String,
    pub suites: BTreeMap<String, SuiteState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuiteState {
    pub summary: SuiteSummary,
    pub run_id: String,
    /// Site-relative paths of the suite's JSON, images, and log chunks.
    pub files: BTreeMap<String, StateFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StateFile {
    pub size: u64,
    /// The provider's content identity: Netlify SHA-1 (40 hex), Cloudflare asset hash
    /// (32 hex), or SHA-256 (64 hex) for Azure, which keeps no addressable content.
    pub hash: String,
}

fn hash_length(provider: DeploymentProvider) -> usize {
    match provider {
        DeploymentProvider::Netlify => 40,
        DeploymentProvider::CloudflareWorkers => 32,
        DeploymentProvider::AzureSwa => 64,
    }
}

fn suite_report_path(id: &str) -> String {
    format!("_snapshot/data/suites/{id}.json")
}

/// Decoded site-relative paths with the snapshot's segment rules: the same names snapshot
/// inventories produce (suite IDs may contain spaces or non-ASCII letters), never traversal.
pub(crate) fn valid_site_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 1024 && path.split('/').all(crate::snapshot::valid_segment)
}

/// Whether a summary's encoded, versioned report URL names `id`'s report file.
fn names_report(report: &str, id: &str) -> bool {
    crate::snapshot::versioned_path(report, "data")
        .is_ok_and(|path| path == Path::new(&suite_report_path(id)))
}

impl SiteState {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.format == STATE_FORMAT,
            "Unsupported site state format {}",
            self.format
        );
        ensure!(
            !self.generator.is_empty()
                && self.generator.len() <= 200
                && chrono::DateTime::parse_from_rfc3339(&self.deployed_at).is_ok()
                && super::is_lower_hex(&self.fingerprint, 64),
            "Invalid site state metadata"
        );
        let length = hash_length(self.provider);
        let mut total = 0usize;
        let mut ids = BTreeSet::new();
        for (id, suite) in &self.suites {
            ensure!(
                crate::snapshot::valid_suite_id(id)
                    && ids.insert(id.to_ascii_lowercase())
                    && suite.summary.id == *id
                    && suite.summary.passed.checked_add(suite.summary.failed)
                        == Some(suite.summary.total)
                    && suite.summary.errors <= suite.summary.failed
                    && names_report(&suite.summary.report, id)
                    && !suite.run_id.is_empty()
                    && suite.run_id.len() <= 200
                    && !suite.run_id.chars().any(char::is_control),
                "Invalid site state for suite '{id}'"
            );
            ensure!(
                suite.files.contains_key(&suite_report_path(id)),
                "Site state for suite '{id}' does not list its report"
            );
            let images = format!("_snapshot/images/{id}/");
            for (path, file) in &suite.files {
                ensure!(
                    valid_site_path(path)
                        && (*path == suite_report_path(id)
                            || path.starts_with(&images)
                            || path.starts_with("_snapshot/data/logs/"))
                        && super::is_lower_hex(&file.hash, length)
                        && file.size <= 512 * 1024 * 1024,
                    "Invalid site state file '{path}' for suite '{id}'"
                );
            }
            total = total.saturating_add(suite.files.len());
        }
        ensure!(total <= MAX_STATE_FILES, "Site state lists too many files");
        Ok(())
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_STATE_BYTES, "Site state is too large");
        let state: Self = serde_json::from_slice(bytes).context("Invalid site state")?;
        state.validate()?;
        Ok(state)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        ensure!(bytes.len() <= MAX_STATE_BYTES, "Site state is too large");
        Ok(bytes)
    }

    /// Ensures `provider` can keep this site's suites as they are: the site uses this build's
    /// report data format and was published by the same provider, whose hashes it records.
    pub fn compatible_with(&self, provider: DeploymentProvider) -> Result<()> {
        ensure!(
            self.data_version == DATA_VERSION,
            "The site uses report data format {}, but this Regressify uses format {DATA_VERSION}; replace the whole site",
            self.data_version
        );
        ensure!(
            self.provider == provider,
            "The site was published by another provider; replace the whole site"
        );
        Ok(())
    }
}

/// What reading a site's published state found.
#[derive(Debug)]
pub enum RemoteState {
    /// A valid state file.
    Found(SiteState),
    /// No state file, but a report manifest: a site published before site states existed.
    Legacy(Vec<SuiteSummary>),
    /// Neither file: an empty site, or one that serves something else.
    Empty,
    /// The site could not be read (network, private site, or access gateway).
    Unreachable(String),
    /// The site answered with a state file that is not valid.
    Invalid(String),
}

fn http_client() -> Result<reqwest::Client> {
    // Redirects are refused: access gateways redirect to sign-in pages, which must read as
    // unreachable rather than as an empty site.
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()?)
}

async fn fetch_bounded(
    client: &reqwest::Client,
    url: reqwest::Url,
    limit: usize,
) -> Result<Option<Vec<u8>>> {
    let response = client
        .get(url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    ensure!(
        response.status().is_success(),
        "The site answered HTTP {}",
        response.status().as_u16()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= limit as u64),
        "The site's response is too large"
    );
    let mut bytes = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        ensure!(bytes.len() <= limit, "The site's response is too large");
    }
    Ok(Some(bytes))
}

/// The site URL with `path` appended and a unique query, so that no cache serves an old copy.
fn site_url(site: &str, path: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(site)?;
    ensure!(
        url.scheme() == "https" || cfg!(test),
        "Site URLs must use HTTPS"
    );
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("Invalid site URL"))?
        .pop_if_empty()
        .extend(path.split('/'));
    url.set_query(Some(&format!(
        "regressify={}",
        uuid::Uuid::new_v4().simple()
    )));
    Ok(url)
}

/// Reads the published state of the site at `site`, falling back to its report manifest.
pub async fn fetch(site: &str) -> RemoteState {
    let result = async {
        let client = http_client()?;
        match fetch_bounded(&client, site_url(site, STATE_PATH)?, MAX_STATE_BYTES).await? {
            Some(bytes) => Ok::<_, anyhow::Error>(Some(bytes)),
            None => Ok(None),
        }
    }
    .await;
    // A single-page-application fallback serves the entry page for missing files.
    let result = result.map(|bytes| {
        bytes.filter(|bytes| bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'<'))
    });
    match result {
        Ok(Some(bytes)) => match SiteState::parse(&bytes) {
            Ok(state) => RemoteState::Found(state),
            Err(error) => RemoteState::Invalid(format!("{error:#}")),
        },
        Ok(None) => match fetch_legacy(site).await {
            Ok(Some(suites)) => RemoteState::Legacy(suites),
            Ok(None) => RemoteState::Empty,
            Err(error) => RemoteState::Unreachable(format!("{error:#}")),
        },
        Err(error) => RemoteState::Unreachable(format!("{error:#}")),
    }
}

/// Suites listed by a site's report manifest, found through its entry page.
async fn fetch_legacy(site: &str) -> Result<Option<Vec<SuiteSummary>>> {
    let client = http_client()?;
    let marker = "name=\"regressify-manifest\" content=\"";
    // Hosts serve the entry page at the root, or only at /index.html (Cloudflare without
    // HTML handling); some redirect one to the other. Network failures stay failures: they
    // must not make a site look empty.
    let mut manifest = None;
    let mut failure = None;
    for page in ["", "index.html"] {
        match fetch_bounded(&client, site_url(site, page)?, 1024 * 1024).await {
            Ok(Some(html)) => {
                if let Some(found) = String::from_utf8(html).ok().and_then(|html| {
                    html.split_once(marker)
                        .and_then(|(_, rest)| rest.split_once('"'))
                        .map(|(path, _)| path.to_owned())
                }) {
                    manifest = Some(found);
                    break;
                }
            }
            Ok(None) => {}
            Err(error) => failure = Some(error),
        }
    }
    let Some(manifest) = manifest else {
        return match failure {
            Some(error) => Err(error),
            None => Ok(None),
        };
    };
    let (path, version) = manifest
        .split_once("?v=")
        .context("The site's manifest URL has no version")?;
    ensure!(
        path == "_snapshot/data/manifest.json" && version.len() == 20,
        "The site's manifest URL is invalid"
    );
    let Some(bytes) = fetch_bounded(&client, site_url(site, path)?, MAX_MANIFEST_BYTES).await?
    else {
        return Ok(None);
    };
    #[derive(Deserialize)]
    struct Manifest {
        version: u8,
        suites: Vec<SuiteSummary>,
    }
    let manifest: Manifest = serde_json::from_slice(&bytes).context("Invalid site manifest")?;
    ensure!(
        manifest.version == DATA_VERSION,
        "The site uses report data format {}; replace the whole site",
        manifest.version
    );
    let mut ids = BTreeSet::new();
    for suite in &manifest.suites {
        ensure!(
            crate::snapshot::valid_suite_id(&suite.id) && ids.insert(suite.id.to_ascii_lowercase()),
            "The site's manifest lists an invalid suite"
        );
    }
    Ok(Some(manifest.suites))
}

/// Local copy of the last state this workspace published to a target: the fallback when the
/// site cannot be read. Stored beside the workspace's deployment settings.
pub fn local_path(settings: &Path, target_id: &str) -> Result<std::path::PathBuf> {
    ensure!(
        !target_id.is_empty()
            && target_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "Invalid deployment target ID"
    );
    let stem = settings
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("Invalid deployment settings path")?;
    Ok(settings
        .parent()
        .context("Deployment settings directory unavailable")?
        .join(format!("{stem}.sites"))
        .join(format!("{target_id}.json")))
}

pub fn load_local(settings: &Path, target_id: &str) -> Result<Option<SiteState>> {
    let path = local_path(settings, target_id)?;
    match std::fs::metadata(&path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && metadata.len() <= MAX_STATE_BYTES as u64,
                "Invalid local site state '{}'",
                path.display()
            );
            let bytes = std::fs::read(&path)
                .with_context(|| format!("Cannot read local site state '{}'", path.display()))?;
            Ok(Some(SiteState::parse(&bytes).with_context(|| {
                format!("Invalid local site state '{}'", path.display())
            })?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("Cannot read local site state '{}'", path.display()))
        }
    }
}

pub fn save_local(settings: &Path, target_id: &str, state: &SiteState) -> Result<()> {
    let path = local_path(settings, target_id)?;
    std::fs::create_dir_all(path.parent().context("Invalid local site state path")?)?;
    crate::helpers::write_file_atomically(&path, &state.to_bytes()?)
        .with_context(|| format!("Cannot save local site state '{}'", path.display()))
}

/// Marks that a report deployment reached the target, even a private one without a URL or
/// a published state: an unknown site must then not be mistaken for an empty one.
fn marker_path(settings: &Path, target_id: &str) -> Result<std::path::PathBuf> {
    Ok(local_path(settings, target_id)?.with_extension("published"))
}

pub fn mark_published(settings: &Path, target_id: &str) -> Result<()> {
    let path = marker_path(settings, target_id)?;
    std::fs::create_dir_all(path.parent().context("Invalid site marker path")?)?;
    crate::helpers::write_file_atomically(&path, b"")
        .with_context(|| format!("Cannot record the publication in '{}'", path.display()))
}

pub fn was_published(settings: &Path, target_id: &str) -> Result<bool> {
    Ok(marker_path(settings, target_id)?.try_exists()?)
}

/// Local inputs each suite on a site was generated from, so a selected suite whose inputs are
/// unchanged is kept on the site instead of being regenerated, staged, and hashed again.
///
/// Each record binds a per-suite `input_fingerprint` (file metadata, so machine-local and never
/// published) to the digest of the [`SuiteState`] that publication left on the site. A record
/// applies only while the site still serves exactly that suite state, so a suite another
/// workspace or a later deployment republished is regenerated. Records therefore need no
/// write-ahead clearing: a stale one can only fail to match.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuiteInputs {
    pub format: u8,
    pub suites: BTreeMap<String, SuiteInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuiteInput {
    /// Per-suite input fingerprint before generation.
    pub inputs: String,
    /// [`suite_digest`] of the suite state this publication left on the site.
    pub state: String,
}

const INPUTS_FORMAT: u8 = 1;
const MAX_INPUTS_BYTES: u64 = 4 * 1024 * 1024;

fn inputs_path(settings: &Path, target_id: &str) -> Result<std::path::PathBuf> {
    Ok(local_path(settings, target_id)?.with_extension("inputs.json"))
}

/// Identity of a suite as a site serves it: summary, run, and every file's provider hash.
pub fn suite_digest(suite: &SuiteState) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"regressify-suite-state-v1\0");
    hasher.update(serde_json::to_vec(suite)?);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Loads the target's records. They only save work, so unreadable or corrupt records are
/// reported and treated as absent, which regenerates the selected suites.
pub fn load_inputs(settings: &Path, target_id: &str) -> SuiteInputs {
    let read = || -> Result<SuiteInputs> {
        let path = inputs_path(settings, target_id)?;
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SuiteInputs::default());
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_INPUTS_BYTES,
            "Invalid suite input records"
        );
        let records: SuiteInputs = serde_json::from_slice(&std::fs::read(&path)?)?;
        ensure!(
            records.format == INPUTS_FORMAT,
            "Unsupported suite input records"
        );
        Ok(records)
    };
    read().unwrap_or_else(|error| {
        eprintln!(
            "Warning: Ignoring the recorded suite inputs; selected suites will be regenerated: {error:#}"
        );
        SuiteInputs::default()
    })
}

pub fn save_inputs(settings: &Path, target_id: &str, records: &SuiteInputs) -> Result<()> {
    let path = inputs_path(settings, target_id)?;
    std::fs::create_dir_all(path.parent().context("Invalid suite input records path")?)?;
    let records = SuiteInputs {
        format: INPUTS_FORMAT,
        suites: records.suites.clone(),
    };
    crate::helpers::write_file_atomically(&path, &serde_json::to_vec(&records)?)
        .with_context(|| format!("Cannot save suite input records '{}'", path.display()))
}

impl SuiteInputs {
    /// The one rule deciding whether a suite the site serves is unchanged since this
    /// workspace published it: whether its record still names the site's suite state and the
    /// current inputs (computed only when needed) match. A site changed elsewhere counts as
    /// changed, as a deployment rebuilds it. `None` without a record or current inputs.
    fn matches(
        &self,
        id: &str,
        suite: &SuiteState,
        current: impl FnOnce() -> Option<String>,
    ) -> Option<bool> {
        let record = self.suites.get(id)?;
        if !suite_digest(suite).is_ok_and(|digest| digest == record.state) {
            return Some(false);
        }
        Some(current()? == record.inputs)
    }

    /// Selected suites the site serves exactly as published from the current `inputs`.
    pub fn unchanged(
        &self,
        site: &KnownSite,
        inputs: &BTreeMap<String, String>,
    ) -> BTreeSet<String> {
        let KnownSite::State(state) = site else {
            return BTreeSet::new();
        };
        inputs
            .iter()
            .filter(|(id, current)| {
                state.suites.get(*id).is_some_and(|suite| {
                    self.matches(id, suite, || Some((*current).clone())) == Some(true)
                })
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Whether each suite the site serves is unchanged locally since this workspace published
    /// it, by the rule deployments use to keep suites (`matches`), with inputs read from
    /// `output`. Suites without a record, or whose local output is gone, are left out, so
    /// callers fall back to run IDs.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn verify(&self, state: &SiteState, output: &Path) -> BTreeMap<String, bool> {
        state
            .suites
            .iter()
            .filter_map(|(id, suite)| {
                let unchanged = self.matches(id, suite, || {
                    crate::snapshot::suite_input_fingerprint(output, id).ok()
                })?;
                Some((id.clone(), unchanged))
            })
            .collect()
    }

    /// Records after publishing `state`: generated suites get their pre-generation `inputs`,
    /// kept suites retain theirs (their suite state is unchanged), and removed suites go.
    pub fn after(&self, state: &SiteState, inputs: &BTreeMap<String, String>) -> Result<Self> {
        let mut suites = BTreeMap::new();
        for (id, suite) in &state.suites {
            let digest = suite_digest(suite)?;
            let record = match inputs.get(id) {
                Some(inputs) => Some(SuiteInput {
                    inputs: inputs.clone(),
                    state: digest,
                }),
                None => self
                    .suites
                    .get(id)
                    .filter(|record| record.state == digest)
                    .cloned(),
            };
            if let Some(record) = record {
                suites.insert(id.clone(), record);
            }
        }
        Ok(Self {
            format: INPUTS_FORMAT,
            suites,
        })
    }
}

/// Forgets everything recorded about the target's site, for destination changes and removal.
pub fn forget_target(settings: &Path, target_id: &str) -> Result<()> {
    forget_local(settings, target_id)?;
    remove_if_present(&inputs_path(settings, target_id)?, "suite input records")?;
    remove_if_present(&marker_path(settings, target_id)?, "site marker")
}

pub fn forget_local(settings: &Path, target_id: &str) -> Result<()> {
    remove_if_present(&local_path(settings, target_id)?, "local site state")
}

/// Removes a local record; one that is already absent counts as forgotten.
fn remove_if_present(path: &Path, label: &str) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("Cannot remove {label} '{}'", path.display()))
        }
    }
}

/// The suites a deployment generates locally, keeps on the site, and removes from it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Generated from local results and uploaded.
    pub generate: Vec<String>,
    /// Kept on the site without local content: their summaries and files come from the state.
    pub kept: Vec<SuiteState>,
    /// Kept on the site by uploading this workspace's copies of the files it serves, for
    /// providers that accept only whole sites (Azure). The copies are verified against the
    /// state's hashes before use (`ReportDeployment::stash_local`).
    pub local: Vec<SuiteState>,
    /// On the site before, absent after.
    pub removed: Vec<String>,
}

impl Plan {
    /// Re-uploads, rather than regenerates, the suites an update of a whole-site provider
    /// (Azure) only keeps: unselected suites the site serves and selected `unchanged` ones.
    ///
    /// Regenerating them would rebuild every report on each deployment although only their
    /// files are needed, and the site's state records each file's hash, so this workspace's
    /// last generated copies can be checked byte for byte. A suite whose copies do not verify
    /// is regenerated after all. Providers that keep files by hash need none of this.
    pub fn reuse_local(
        &mut self,
        mode: DeploymentMode,
        provider: DeploymentProvider,
        site: &KnownSite,
        selected: &[String],
        unchanged: &BTreeSet<String>,
    ) {
        let KnownSite::State(state) = site else {
            return;
        };
        if mode == DeploymentMode::Replace
            || provider.keeps_files_by_hash()
            || state.compatible_with(provider).is_err()
        {
            return;
        }
        let pending = std::mem::take(&mut self.generate);
        for id in pending {
            let keeps = !selected.contains(&id) || unchanged.contains(&id);
            match state.suites.get(&id) {
                Some(suite) if keeps => self.local.push(suite.clone()),
                _ => self.generate.push(id),
            }
        }
        self.local.sort_by(|a, b| a.summary.id.cmp(&b.summary.id));
    }

    /// Moves a local suite whose copies could not be used back to generation.
    pub fn regenerate_local(&mut self, id: &str) {
        if let Some(index) = self.local.iter().position(|suite| suite.summary.id == id) {
            self.local.remove(index);
            self.generate.push(id.to_owned());
            self.generate.sort();
        }
    }

    /// Keeps the `unchanged` selected suites on the site instead of regenerating them.
    ///
    /// Only updates skip suites. Replace rebuilds every selected suite: it is the way to
    /// start over when a kept copy cannot be used (content the provider dropped, files the
    /// target now excludes, or images that must be optimized), so it must not keep any.
    /// Only Netlify and Cloudflare can keep files by hash, and only a known, compatible state
    /// lists them; otherwise the plan is left alone. The site ends up the same either way:
    /// the kept suite state is exactly what regenerating unchanged inputs would publish.
    pub fn keep_unchanged(
        &mut self,
        mode: DeploymentMode,
        provider: DeploymentProvider,
        site: &KnownSite,
        unchanged: &BTreeSet<String>,
    ) {
        let KnownSite::State(state) = site else {
            return;
        };
        if mode == DeploymentMode::Replace
            || !provider.keeps_files_by_hash()
            || state.compatible_with(provider).is_err()
        {
            return;
        }
        let pending = std::mem::take(&mut self.generate);
        for id in pending {
            match state.suites.get(&id) {
                Some(suite) if unchanged.contains(&id) => self.kept.push(suite.clone()),
                _ => self.generate.push(id),
            }
        }
        self.kept.sort_by(|a, b| a.summary.id.cmp(&b.summary.id));
    }
}

/// What a deployment knows about the site before publishing.
#[derive(Debug)]
pub enum KnownSite {
    /// The current published state, from the site or the local fallback.
    State(SiteState),
    /// A site published before site states existed: only its suite list is known.
    Legacy(Vec<SuiteSummary>),
    /// Nothing is published there yet.
    Empty,
}

impl KnownSite {
    /// IDs of the suites the site serves.
    pub fn suite_ids(&self) -> Vec<String> {
        match self {
            Self::State(state) => state.suites.keys().cloned().collect(),
            Self::Legacy(suites) => suites.iter().map(|suite| suite.id.clone()).collect(),
            Self::Empty => Vec::new(),
        }
    }
}

/// Decides which suites to generate, keep, and remove.
///
/// `local` lists suites with local results. Kept suites need no local content on Netlify and
/// Cloudflare; Azure uploads whole packages and legacy sites list no files, so their kept
/// suites are regenerated from local results instead, and a missing one requires a replace.
pub fn plan(
    mode: DeploymentMode,
    provider: DeploymentProvider,
    selected: &[String],
    remove: &[String],
    site: &KnownSite,
    local: &BTreeSet<String>,
) -> Result<Plan> {
    let selected_set = selected.iter().collect::<BTreeSet<_>>();
    let remove_set = remove.iter().collect::<BTreeSet<_>>();
    ensure!(
        selected_set.len() == selected.len()
            && remove_set.len() == remove.len()
            && selected_set.is_disjoint(&remove_set),
        "Deployment suite selection is invalid"
    );
    let on_site = site.suite_ids();
    let mut plan = Plan {
        generate: selected.to_vec(),
        ..Plan::default()
    };
    match mode {
        DeploymentMode::Replace => {
            ensure!(
                remove.is_empty(),
                "Replacing the site removes suites by deselecting them"
            );
            ensure!(
                !selected.is_empty(),
                "Select at least one test suite to deploy"
            );
            plan.removed = on_site
                .into_iter()
                .filter(|id| !selected_set.contains(id))
                .collect();
        }
        DeploymentMode::Update => {
            ensure!(
                !selected.is_empty() || !remove.is_empty(),
                "Select at least one test suite to deploy or remove"
            );
            let kept_ids = on_site
                .iter()
                .filter(|id| !selected_set.contains(id) && !remove_set.contains(id))
                .cloned()
                .collect::<Vec<_>>();
            plan.removed = on_site
                .iter()
                .filter(|id| remove_set.contains(id))
                .cloned()
                .collect();
            let rebuild = match site {
                KnownSite::State(state) => {
                    state.compatible_with(provider)?;
                    if !provider.keeps_files_by_hash() {
                        true
                    } else {
                        plan.kept = kept_ids.iter().map(|id| state.suites[id].clone()).collect();
                        false
                    }
                }
                KnownSite::Legacy(_) => true,
                KnownSite::Empty => false,
            };
            if rebuild && !kept_ids.is_empty() {
                // A known state lets a whole-site upload reuse the report files of kept suites
                // (`Plan::reuse_local`), so their results are only needed if that fails, which
                // publishing reports. Legacy sites list no files and always need them.
                let missing = kept_ids
                    .iter()
                    .filter(|id| !local.contains(*id))
                    .cloned()
                    .collect::<Vec<_>>();
                if !missing.is_empty() && matches!(site, KnownSite::Legacy(_)) {
                    bail!(
                        "Keeping {} on this site needs their local results, which are missing; run them or replace the whole site",
                        missing.join(", ")
                    );
                }
                plan.generate.extend(kept_ids);
            }
        }
    }
    plan.generate.sort();
    ensure!(
        !plan.generate.is_empty() || !plan.kept.is_empty(),
        "This would remove every suite from the site; replace the site with a selection instead"
    );
    Ok(plan)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn summary(id: &str) -> SuiteSummary {
        serde_json::from_value(serde_json::json!({
            "id": id, "total": 2, "passed": 1, "failed": 1, "errors": 0,
            "report": format!("_snapshot/data/suites/{id}.json?v=AAAAAAAAAAAAAAAAAAAA")
        }))
        .unwrap()
    }

    fn suite(id: &str, provider: DeploymentProvider) -> SuiteState {
        let hash = "a".repeat(hash_length(provider));
        SuiteState {
            summary: summary(id),
            run_id: "run-1".into(),
            files: BTreeMap::from([
                (
                    suite_report_path(id),
                    StateFile {
                        size: 10,
                        hash: hash.clone(),
                    },
                ),
                (
                    format!("_snapshot/images/{id}/test/home.png"),
                    StateFile { size: 20, hash },
                ),
            ]),
        }
    }

    pub(crate) fn state(provider: DeploymentProvider, ids: &[&str]) -> SiteState {
        SiteState {
            format: STATE_FORMAT,
            data_version: DATA_VERSION,
            generator: "test".into(),
            provider,
            deployed_at: "2026-11-06T00:00:00Z".into(),
            fingerprint: "b".repeat(64),
            suites: ids
                .iter()
                .map(|id| ((*id).to_owned(), suite(id, provider)))
                .collect(),
        }
    }

    fn file(hash: &str) -> StateFile {
        StateFile {
            size: 1,
            hash: hash.into(),
        }
    }

    #[test]
    fn site_states_round_trip_and_reject_untrusted_content() {
        let valid = state(DeploymentProvider::Netlify, &["header", "shop"]);
        assert_eq!(SiteState::parse(&valid.to_bytes().unwrap()).unwrap(), valid);
        let netlify_hash = "a".repeat(40);
        type Mutation<'a> = Box<dyn Fn(&mut SiteState) + 'a>;
        let mutations: Vec<Mutation<'_>> = vec![
            Box::new(|state| state.format = 2),
            Box::new(|state| state.fingerprint = "short".into()),
            // Fingerprints are compared as strings, so only the lowercase form is valid.
            Box::new(|state| state.fingerprint = "B".repeat(64)),
            Box::new(|state| state.deployed_at = "yesterday".into()),
            // Hashes must match the provider's identity format.
            Box::new(|state| state.provider = DeploymentProvider::CloudflareWorkers),
            Box::new(|state| state.suites.get_mut("shop").unwrap().summary.id = "header".into()),
            Box::new(|state| {
                state.suites.get_mut("shop").unwrap().summary.report =
                    "_snapshot/data/suites/other.json?v=AAAAAAAAAAAAAAAAAAAA".into();
            }),
            Box::new(|state| {
                state
                    .suites
                    .get_mut("shop")
                    .unwrap()
                    .files
                    .remove("_snapshot/data/suites/shop.json");
            }),
            // A suite may not claim another suite's images, traverse, or name hosting files.
            Box::new(|state| {
                let files = &mut state.suites.get_mut("shop").unwrap().files;
                files.insert(
                    "_snapshot/images/header/test/x.png".into(),
                    file(&netlify_hash),
                );
            }),
            Box::new(|state| {
                let files = &mut state.suites.get_mut("shop").unwrap().files;
                files.insert(
                    "_snapshot/images/shop/../header/x.png".into(),
                    file(&netlify_hash),
                );
            }),
            Box::new(|state| {
                let files = &mut state.suites.get_mut("shop").unwrap().files;
                files.insert("_headers".into(), file(&netlify_hash));
            }),
            Box::new(|state| {
                let suite = state.suites.get_mut("shop").unwrap();
                suite.files.values_mut().next().unwrap().hash = "A".repeat(40);
            }),
            Box::new(|state| state.suites.get_mut("shop").unwrap().run_id.clear()),
        ];
        for mutate in mutations {
            let mut invalid = valid.clone();
            mutate(&mut invalid);
            assert!(invalid.validate().is_err(), "{invalid:?}");
            assert!(SiteState::parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        assert!(SiteState::parse(b"{\"format\":1}").is_err());
        // Unknown fields could smuggle private data into a public file.
        let mut unknown = serde_json::to_value(&valid).unwrap();
        unknown["owner"] = serde_json::json!("C:/Users/private");
        assert!(SiteState::parse(&serde_json::to_vec(&unknown).unwrap()).is_err());
    }

    #[test]
    fn local_site_states_are_stored_beside_settings_per_target() {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("deployments").join("workspace.json");
        assert!(load_local(&settings, "qa").unwrap().is_none());
        let saved = state(DeploymentProvider::CloudflareWorkers, &["shop"]);
        save_local(&settings, "qa", &saved).unwrap();
        assert_eq!(load_local(&settings, "qa").unwrap(), Some(saved));
        assert!(load_local(&settings, "other").unwrap().is_none());
        assert!(local_path(&settings, "../escape").is_err());
        std::fs::write(local_path(&settings, "qa").unwrap(), b"{").unwrap();
        assert!(load_local(&settings, "qa").is_err());
        forget_local(&settings, "qa").unwrap();
        forget_local(&settings, "qa").unwrap();
        assert!(load_local(&settings, "qa").unwrap().is_none());
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn update(
        provider: DeploymentProvider,
        selected: &[&str],
        remove: &[&str],
        site: &KnownSite,
        local: &BTreeSet<String>,
    ) -> Result<Plan> {
        plan(
            DeploymentMode::Update,
            provider,
            &ids(selected),
            &ids(remove),
            site,
            local,
        )
    }

    #[test]
    fn plans_keep_remove_and_regenerate_suites_per_mode_and_provider() {
        use DeploymentProvider::{AzureSwa, CloudflareWorkers, Netlify};
        let local = ids(&["a", "b", "c", "d"])
            .into_iter()
            .collect::<BTreeSet<_>>();
        let none = BTreeSet::new();
        let netlify = KnownSite::State(state(Netlify, &["a", "b", "c"]));

        // Updating keeps every other suite without local content.
        let kept = update(Netlify, &["b", "d"], &[], &netlify, &local).unwrap();
        assert_eq!(kept.generate, ids(&["b", "d"]));
        assert_eq!(
            kept.kept
                .iter()
                .map(|suite| suite.summary.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert!(kept.removed.is_empty());
        // Even suites with no local results stay on the site.
        assert_eq!(
            update(Netlify, &["b"], &[], &netlify, &none)
                .unwrap()
                .kept
                .len(),
            2
        );

        // Removing publishes nothing new and drops only the named suite.
        let removed = update(Netlify, &[], &["c"], &netlify, &none).unwrap();
        assert!(removed.generate.is_empty());
        assert_eq!(removed.removed, ids(&["c"]));
        assert_eq!(removed.kept.len(), 2);
        // Removing a suite that is not on the site changes nothing.
        assert!(
            update(Netlify, &[], &["z"], &netlify, &none)
                .unwrap()
                .removed
                .is_empty()
        );

        // Replacing makes the site exactly the selection.
        let replace = |selected: &[&str], remove: &[&str]| {
            plan(
                DeploymentMode::Replace,
                Netlify,
                &ids(selected),
                &ids(remove),
                &netlify,
                &none,
            )
        };
        let replaced = replace(&["b"], &[]).unwrap();
        assert_eq!(replaced.generate, ids(&["b"]));
        assert!(replaced.kept.is_empty());
        assert_eq!(replaced.removed, ids(&["a", "c"]));
        assert!(replace(&["b"], &["a"]).is_err());
        assert!(replace(&[], &[]).is_err());

        // Azure keeps nothing remotely: kept suites go through the upload again, rebuilt or
        // reused from local report files (`reuse_local`), so missing results are not fatal yet.
        let azure = KnownSite::State(state(AzureSwa, &["a", "b", "c"]));
        let rebuilt = update(AzureSwa, &["b"], &["c"], &azure, &local).unwrap();
        assert_eq!(rebuilt.generate, ids(&["a", "b"]));
        assert!(rebuilt.kept.is_empty());
        let only_b = ids(&["b"]).into_iter().collect();
        assert_eq!(
            update(AzureSwa, &["b"], &[], &azure, &only_b)
                .unwrap()
                .generate,
            ids(&["a", "b", "c"])
        );
        // Legacy sites list no files to reuse, so their kept suites need local results.
        let legacy_ac = KnownSite::Legacy(vec![summary("a"), summary("b"), summary("c")]);
        let error = update(Netlify, &["b"], &[], &legacy_ac, &only_b)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("a, c") && error.contains("replace"),
            "{error}"
        );

        // Sites published before site states list suites but no files.
        let legacy = KnownSite::Legacy(vec![summary("a"), summary("b")]);
        assert_eq!(
            update(Netlify, &["b"], &[], &legacy, &local)
                .unwrap()
                .generate,
            ids(&["a", "b"])
        );
        assert!(update(Netlify, &["b"], &[], &legacy, &none).is_err());
        assert_eq!(
            update(Netlify, &["b"], &[], &KnownSite::Empty, &none)
                .unwrap()
                .generate,
            ids(&["b"])
        );

        // Incompatible sites can only be replaced.
        let mut old = state(Netlify, &["a"]);
        old.data_version = 1;
        assert!(update(Netlify, &["b"], &[], &KnownSite::State(old), &local).is_err());
        assert!(update(CloudflareWorkers, &["b"], &[], &netlify, &local).is_err());

        // Invalid selections.
        assert!(update(Netlify, &[], &[], &netlify, &local).is_err());
        assert!(update(Netlify, &["a"], &["a"], &netlify, &local).is_err());
        assert!(update(Netlify, &["a", "a"], &[], &netlify, &local).is_err());
    }

    #[test]
    fn unchanged_selected_suites_are_kept_only_where_files_can_be_kept() {
        use DeploymentProvider::{AzureSwa, CloudflareWorkers, Netlify};
        let unchanged = ids(&["a"]).into_iter().collect::<BTreeSet<_>>();
        let local = ids(&["a", "b", "c"]).into_iter().collect::<BTreeSet<_>>();
        let kept = |plan: &Plan| {
            plan.kept
                .iter()
                .map(|suite| suite.summary.id.clone())
                .collect::<Vec<_>>()
        };
        for provider in [Netlify, CloudflareWorkers] {
            let site = KnownSite::State(state(provider, &["a", "b", "c"]));
            let update = DeploymentMode::Update;
            let mut planned =
                plan(update, provider, &ids(&["a", "b"]), &[], &site, &local).unwrap();
            planned.keep_unchanged(update, provider, &site, &unchanged);
            assert_eq!(planned.generate, ids(&["b"]), "{provider:?}");
            assert_eq!(kept(&planned), ids(&["a", "c"]), "{provider:?}");
        }
        // Replace starts over: every selected suite is rebuilt, unchanged or not.
        let site = KnownSite::State(state(Netlify, &["a", "b", "c"]));
        let replace = DeploymentMode::Replace;
        let mut planned = plan(replace, Netlify, &ids(&["a", "b"]), &[], &site, &local).unwrap();
        planned.keep_unchanged(replace, Netlify, &site, &unchanged);
        assert_eq!(planned.generate, ids(&["a", "b"]));
        assert!(planned.kept.is_empty());
        assert_eq!(planned.removed, ids(&["c"]));

        // Azure re-uploads whole packages, legacy and empty sites list no files, and an
        // incompatible state cannot be kept: all of them regenerate the selection.
        let mut old = state(Netlify, &["a"]);
        old.data_version = 1;
        for (provider, site) in [
            (AzureSwa, KnownSite::State(state(AzureSwa, &["a"]))),
            (Netlify, KnownSite::Legacy(vec![summary("a")])),
            (Netlify, KnownSite::Empty),
            (Netlify, KnownSite::State(old)),
        ] {
            let mut planned = Plan {
                generate: ids(&["a"]),
                ..Plan::default()
            };
            planned.keep_unchanged(DeploymentMode::Update, provider, &site, &unchanged);
            assert_eq!(planned.generate, ids(&["a"]), "{site:?}");
            assert!(planned.kept.is_empty(), "{site:?}");
        }
    }

    #[test]
    fn whole_site_updates_reuse_local_copies_of_the_suites_they_only_keep() {
        use DeploymentProvider::{AzureSwa, Netlify};
        let local = ids(&["a", "b", "c", "d"])
            .into_iter()
            .collect::<BTreeSet<_>>();
        let unchanged = ids(&["a"]).into_iter().collect::<BTreeSet<_>>();
        let azure = KnownSite::State(state(AzureSwa, &["a", "b", "c"]));
        let local_ids = |plan: &Plan| {
            plan.local
                .iter()
                .map(|suite| suite.summary.id.clone())
                .collect::<Vec<_>>()
        };
        // An Azure update rebuilds only the changed and new suites; the unchanged selected
        // suite and the unselected one it keeps are re-uploaded from local copies.
        let selected = ids(&["a", "b", "d"]);
        let mut planned = plan(
            DeploymentMode::Update,
            AzureSwa,
            &selected,
            &[],
            &azure,
            &local,
        )
        .unwrap();
        assert_eq!(planned.generate, ids(&["a", "b", "c", "d"]));
        planned.reuse_local(
            DeploymentMode::Update,
            AzureSwa,
            &azure,
            &selected,
            &unchanged,
        );
        assert_eq!(planned.generate, ids(&["b", "d"]));
        assert_eq!(local_ids(&planned), ids(&["a", "c"]));
        // Copies that fail verification are regenerated after all.
        planned.regenerate_local("c");
        assert_eq!(planned.generate, ids(&["b", "c", "d"]));
        assert_eq!(local_ids(&planned), ids(&["a"]));

        // Replace, providers that keep files by hash, and sites without a state rebuild.
        let mut replaced = plan(
            DeploymentMode::Replace,
            AzureSwa,
            &selected,
            &[],
            &azure,
            &local,
        )
        .unwrap();
        replaced.reuse_local(
            DeploymentMode::Replace,
            AzureSwa,
            &azure,
            &selected,
            &unchanged,
        );
        assert!(replaced.local.is_empty());
        let netlify = KnownSite::State(state(Netlify, &["a", "b", "c"]));
        let mut kept = plan(
            DeploymentMode::Update,
            Netlify,
            &selected,
            &[],
            &netlify,
            &local,
        )
        .unwrap();
        kept.reuse_local(
            DeploymentMode::Update,
            Netlify,
            &netlify,
            &selected,
            &unchanged,
        );
        assert!(kept.local.is_empty());
        let legacy = KnownSite::Legacy(vec![summary("a"), summary("c")]);
        let mut rebuilt = plan(
            DeploymentMode::Update,
            AzureSwa,
            &selected,
            &[],
            &legacy,
            &local,
        )
        .unwrap();
        rebuilt.reuse_local(
            DeploymentMode::Update,
            AzureSwa,
            &legacy,
            &selected,
            &unchanged,
        );
        assert!(rebuilt.local.is_empty());
    }

    #[test]
    fn suite_input_records_match_only_the_suite_state_they_published() {
        let published = state(DeploymentProvider::Netlify, &["a", "b", "c"]);
        let site = KnownSite::State(published.clone());
        let inputs = BTreeMap::from([
            ("a".to_owned(), "in-a".to_owned()),
            ("b".to_owned(), "in-b".to_owned()),
        ]);
        let records = SuiteInputs::default().after(&published, &inputs).unwrap();
        // Suites published without known inputs get no record.
        assert_eq!(records.suites.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(
            records.unchanged(&site, &inputs),
            BTreeSet::from(["a".into(), "b".into()])
        );

        // Changed inputs, a republished suite, or an unknown site state never match.
        let changed = BTreeMap::from([("a".to_owned(), "in-a2".to_owned())]);
        assert!(records.unchanged(&site, &changed).is_empty());
        let mut republished = published.clone();
        republished
            .suites
            .get_mut("a")
            .unwrap()
            .files
            .values_mut()
            .next()
            .unwrap()
            .size = 11;
        let republished_site = KnownSite::State(republished.clone());
        assert_eq!(
            records.unchanged(&republished_site, &inputs),
            BTreeSet::from(["b".into()])
        );
        assert!(
            records
                .unchanged(&KnownSite::Legacy(vec![summary("a")]), &inputs)
                .is_empty()
        );
        assert!(records.unchanged(&KnownSite::Empty, &inputs).is_empty());

        // Kept suites retain records that still describe them; removed suites drop theirs.
        let mut next = republished.clone();
        next.suites.remove("b");
        let after = records.after(&next, &BTreeMap::new()).unwrap();
        assert!(after.suites.is_empty(), "{after:?}");
        let mut next = published.clone();
        next.suites.remove("a");
        let after = records.after(&next, &BTreeMap::new()).unwrap();
        assert_eq!(after.suites.keys().collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn verification_matches_the_rule_deployments_use_to_keep_suites() {
        let output = tempfile::tempdir().unwrap();
        for id in ["a", "b", "c"] {
            std::fs::create_dir_all(output.path().join(id).join("bitmaps_reference")).unwrap();
            std::fs::write(
                output.path().join(id).join("bitmaps_reference/home.png"),
                id,
            )
            .unwrap();
        }
        let published = state(DeploymentProvider::Netlify, &["a", "b", "c", "gone"]);
        let inputs = ["a", "b", "c"]
            .into_iter()
            .map(|id| {
                let fingerprint = crate::snapshot::suite_input_fingerprint(output.path(), id);
                (id.to_owned(), fingerprint.unwrap())
            })
            .collect::<BTreeMap<_, _>>();
        let mut records = SuiteInputs::default().after(&published, &inputs).unwrap();
        // "c" was republished elsewhere, so its record no longer describes the site.
        records.suites.get_mut("c").unwrap().state = "e".repeat(64);
        // An approval rewrites a baseline without a new run.
        std::fs::write(
            output.path().join("b/bitmaps_reference/home.png"),
            "approved",
        )
        .unwrap();

        // A site changed elsewhere counts as changed, as a deployment rebuilds it.
        let verified = records.verify(&published, output.path());
        assert_eq!(
            verified,
            BTreeMap::from([
                ("a".to_owned(), true),
                ("b".to_owned(), false),
                ("c".to_owned(), false),
            ])
        );
        // The same rule decides what a deployment keeps.
        let current = ["a", "b"]
            .into_iter()
            .map(|id| {
                let fingerprint = crate::snapshot::suite_input_fingerprint(output.path(), id);
                (id.to_owned(), fingerprint.unwrap())
            })
            .collect();
        assert_eq!(
            records.unchanged(&KnownSite::State(published.clone()), &current),
            BTreeSet::from(["a".to_owned()])
        );
        // Without local output there are no newer results to report, so the suite is left
        // to the run-ID comparison, like suites without a record ("gone").
        std::fs::remove_dir_all(output.path().join("a")).unwrap();
        assert_eq!(
            records.verify(&published, output.path()),
            BTreeMap::from([("b".to_owned(), false), ("c".to_owned(), false)])
        );
    }

    #[test]
    fn suite_input_records_are_local_per_target_and_forgotten_with_it() {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("deployments").join("workspace.json");
        assert_eq!(load_inputs(&settings, "qa"), SuiteInputs::default());
        let published = state(DeploymentProvider::Netlify, &["a"]);
        let records = SuiteInputs::default()
            .after(&published, &BTreeMap::from([("a".into(), "in-a".into())]))
            .unwrap();
        save_inputs(&settings, "qa", &records).unwrap();
        assert_eq!(load_inputs(&settings, "qa"), records);
        assert_eq!(load_inputs(&settings, "other"), SuiteInputs::default());
        // The site state never carries them: they hold machine-local file metadata.
        assert!(load_local(&settings, "qa").unwrap().is_none());
        let path = inputs_path(&settings, "qa").unwrap();
        assert!(path.ends_with("workspace.sites/qa.inputs.json"), "{path:?}");
        std::fs::write(&path, b"{\"format\":9,\"suites\":{}}").unwrap();
        assert_eq!(load_inputs(&settings, "qa"), SuiteInputs::default());
        save_inputs(&settings, "qa", &records).unwrap();
        // Forgetting the local copy alone keeps them; they only match the site's own state.
        forget_local(&settings, "qa").unwrap();
        assert_eq!(load_inputs(&settings, "qa"), records);
        forget_target(&settings, "qa").unwrap();
        assert!(!path.exists());
        forget_target(&settings, "qa").unwrap();
    }

    #[test]
    fn suite_ids_that_need_url_encoding_are_valid_site_state() {
        for id in ["Header Nav", "trang-chủ", "Login (EU)"] {
            let mut state = state(DeploymentProvider::Netlify, &[]);
            let encoded = utf8_percent_encode_path(&suite_report_path(id));
            let mut suite = suite(id, DeploymentProvider::Netlify);
            suite.summary.report = format!("{encoded}?v=AAAAAAAAAAAAAAAAAAAA");
            state.suites.insert(id.to_owned(), suite);
            state.validate().unwrap();
            // The decoded file key, not the URL, is what keeping a suite sends.
            assert!(state.suites[id].files.contains_key(&suite_report_path(id)));
        }
    }

    fn utf8_percent_encode_path(path: &str) -> String {
        path.split('/')
            .map(|segment| {
                percent_encoding::utf8_percent_encode(segment, percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("/")
            .replace("%2E", ".")
            .replace("%2D", "-")
            .replace("%5F", "_")
    }

    #[test]
    fn updates_never_leave_an_empty_site() {
        let netlify = KnownSite::State(state(DeploymentProvider::Netlify, &["a", "b"]));
        let error = update(
            DeploymentProvider::Netlify,
            &[],
            &["a", "b"],
            &netlify,
            &BTreeSet::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("every suite"), "{error}");
        assert!(
            update(
                DeploymentProvider::Netlify,
                &[],
                &["a"],
                &netlify,
                &BTreeSet::new()
            )
            .is_ok()
        );
    }

    fn respond(
        request: crate::test::browser_tests::Request,
        status: u16,
        body: &[u8],
        location: Option<&str>,
    ) {
        let mut response = tiny_http::Response::from_data(body.to_vec()).with_status_code(status);
        if let Some(location) = location {
            response.add_header(tiny_http::Header::from_bytes("Location", location).unwrap());
        }
        let _ = request.respond(response);
    }

    #[tokio::test]
    async fn remote_states_classify_found_legacy_empty_unreachable_and_invalid() {
        let published = state(DeploymentProvider::Netlify, &["shop"])
            .to_bytes()
            .unwrap();
        let manifest = serde_json::to_vec(&serde_json::json!({
            "version": 2, "title": "t", "heading": "h", "subheading": "",
            "generatedAt": "2026-11-06T00:00:00Z", "suites": [summary("shop")]
        }))
        .unwrap();
        let index = concat!(
            "<meta name=\"regressify-manifest\" ",
            "content=\"_snapshot/data/manifest.json?v=AAAAAAAAAAAAAAAAAAAA\">"
        );
        for (case, expected) in [
            ("found", "Found"),
            ("legacy", "Legacy"),
            ("spa", "Legacy"),
            ("index-only", "Legacy"),
            ("empty", "Empty"),
            ("gateway", "Unreachable"),
            ("broken", "Invalid"),
        ] {
            let (published, manifest) = (published.clone(), manifest.clone());
            let site = crate::test::browser_tests::Site::new(move |request| {
                let url = request.url().to_owned();
                // Every read bypasses caches with a unique query.
                assert!(url.contains("?regressify="), "{url}");
                match (case, url.split('?').next().unwrap()) {
                    ("found", "/reports/_snapshot/deployment.json") => {
                        respond(request, 200, &published, None)
                    }
                    ("broken", "/reports/_snapshot/deployment.json") => {
                        respond(request, 200, b"{\"format\":1}", None)
                    }
                    ("gateway", _) => {
                        respond(request, 302, b"", Some("https://login.example.com/"))
                    }
                    ("legacy" | "spa", "/reports/") => {
                        respond(request, 200, index.as_bytes(), None)
                    }
                    // Without HTML handling, Cloudflare serves the entry page only here.
                    ("index-only", "/reports/index.html") => {
                        respond(request, 200, index.as_bytes(), None)
                    }
                    ("index-only", "/reports/_snapshot/data/manifest.json") => {
                        respond(request, 200, &manifest, None)
                    }
                    // A single-page fallback answers the missing state with the entry page.
                    ("spa", "/reports/_snapshot/deployment.json") => {
                        respond(request, 200, index.as_bytes(), None)
                    }
                    ("spa", "/reports/_snapshot/data/manifest.json") => {
                        respond(request, 200, &manifest, None)
                    }
                    ("legacy", "/reports/_snapshot/data/manifest.json") => {
                        respond(request, 200, &manifest, None)
                    }
                    _ => respond(request, 404, b"", None),
                }
            })
            .unwrap();
            let remote = fetch(&format!("{}/reports/", site.origin)).await;
            let name = match &remote {
                RemoteState::Found(found) => {
                    assert_eq!(found.suites.len(), 1);
                    "Found"
                }
                RemoteState::Legacy(suites) => {
                    assert_eq!(suites[0].id, "shop");
                    "Legacy"
                }
                RemoteState::Empty => "Empty",
                RemoteState::Unreachable(_) => "Unreachable",
                RemoteState::Invalid(_) => "Invalid",
            };
            assert_eq!(name, expected, "{case}: {remote:?}");
        }
        assert!(matches!(
            fetch("http://127.0.0.1:1/").await,
            RemoteState::Unreachable(_)
        ));
    }
}
