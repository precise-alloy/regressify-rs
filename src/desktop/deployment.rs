use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod cloudflare;
pub mod credentials;
mod diagnostics;
pub(crate) mod incremental;
pub mod netlify;
mod optimization;
mod package;
// The preview serves the native UI only.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub(crate) mod preview;
pub(crate) mod site_state;
pub mod swa;
pub mod tokens;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum DeploymentProvider {
    #[default]
    AzureSwa,
    Netlify,
    CloudflareWorkers,
}

impl DeploymentProvider {
    /// Whether a deployment can keep files already on the site by their hash. Azure Static
    /// Web Apps accepts only whole packages, so everything it keeps must be uploaded again.
    pub(crate) fn keeps_files_by_hash(self) -> bool {
        self != Self::AzureSwa
    }

    pub(crate) fn diagnostic_value(self) -> crate::diagnostics::record::SafeValue {
        use crate::diagnostics::record::SafeValue;
        match self {
            Self::AzureSwa => SafeValue::Azure,
            Self::Netlify => SafeValue::Netlify,
            Self::CloudflareWorkers => SafeValue::Cloudflare,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AzureSwa => "Azure Static Web Apps",
            Self::Netlify => "Netlify",
            Self::CloudflareWorkers => "Cloudflare Workers",
        }
    }

    pub fn validate_token(self, token: &str) -> Result<()> {
        match self {
            Self::AzureSwa => swa::validate_token(token),
            Self::CloudflareWorkers => cloudflare::validate_token(token),
            Self::Netlify => {
                ensure!(
                    !token.is_empty()
                        && token.len() <= 1024
                        && token.bytes().all(|byte| byte.is_ascii_graphic()),
                    "Enter a valid Netlify personal access token"
                );
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeployTarget {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_token_id: Option<String>,
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub provider: DeploymentProvider,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub site_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloudflare: Option<cloudflare::CloudflareTarget>,
}

impl<'de> Deserialize<'de> for DeployTarget {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Fields {
            id: String,
            name: String,
            #[serde(default)]
            published_url: Option<String>,
            #[serde(default)]
            credential_id: Option<String>,
            #[serde(default)]
            named_token_id: Option<String>,
            #[serde(default)]
            environment: String,
            #[serde(default)]
            provider: DeploymentProvider,
            #[serde(default)]
            site_id: String,
            #[serde(default)]
            cloudflare: Option<cloudflare::CloudflareTarget>,
            #[serde(default, rename = "tokenEnv")]
            _legacy_token_env: Option<String>,
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            id: fields.id,
            name: fields.name,
            published_url: fields.published_url,
            credential_id: fields.credential_id,
            named_token_id: fields.named_token_id,
            environment: fields.environment,
            provider: fields.provider,
            site_id: fields.site_id,
            cloudflare: fields.cloudflare,
        })
    }
}

/// How a report deployment changes the site.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum DeploymentMode {
    /// The site becomes exactly the selected suites. The default for requests saved before
    /// incremental deployment existed.
    #[default]
    Replace,
    /// Publish the selected suites and keep every other suite already on the site.
    Update,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentJob {
    pub target: DeployTarget,
    pub suite_ids: Vec<String>,
    pub catalog_ids: Vec<String>,
    pub partial_confirmed: bool,
    /// Publish even when the staged site matches the target's last recorded deployment.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    /// Requests saved before incremental deployment existed replace the site.
    #[serde(default)]
    pub mode: DeploymentMode,
    /// Suites to remove from the site in `Update` mode. They may no longer exist in the
    /// workspace, so they are not limited to the catalog.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_ids: Vec<String>,
}

impl DeploymentJob {
    pub fn validate(&self) -> Result<()> {
        self.target.validate()?;
        let selected = self.suite_ids.iter().collect::<BTreeSet<_>>();
        let catalog = self.catalog_ids.iter().collect::<BTreeSet<_>>();
        let remove = self.remove_ids.iter().collect::<BTreeSet<_>>();
        ensure!(
            selected.len() == self.suite_ids.len()
                && catalog.len() == self.catalog_ids.len()
                && remove.len() == self.remove_ids.len()
                && selected.is_subset(&catalog)
                && selected.is_disjoint(&remove),
            "Deployment suite selection is invalid"
        );
        for suite in self.catalog_ids.iter().chain(&self.remove_ids) {
            validate_suite_id(suite)?;
        }
        match self.mode {
            DeploymentMode::Replace => {
                ensure!(
                    !self.suite_ids.is_empty(),
                    "Select at least one test suite to deploy"
                );
                ensure!(
                    self.remove_ids.is_empty(),
                    "Replacing the site removes suites by deselecting them"
                );
                ensure!(
                    selected == catalog || self.partial_confirmed,
                    "Confirm that you intend to deploy only the selected test suites"
                );
            }
            DeploymentMode::Update => ensure!(
                !self.suite_ids.is_empty() || !self.remove_ids.is_empty(),
                "Select at least one test suite to deploy or remove"
            ),
        }
        Ok(())
    }
}

impl DeployTarget {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.is_empty()
                && self.id.len() <= 80
                && self
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "Invalid deployment target ID"
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.len() <= 100
                && !self.name.chars().any(char::is_control),
            "Target name must contain 1-100 characters"
        );
        if let Some(url) = &self.published_url {
            validate_published_url(url)?;
        }
        ensure!(
            self.credential_id
                .as_ref()
                .is_none_or(|id| uuid::Uuid::parse_str(id).is_ok()),
            "Invalid deployment credential ID"
        );
        ensure!(
            self.named_token_id
                .as_ref()
                .is_none_or(|id| uuid::Uuid::parse_str(id).is_ok()),
            "Invalid named deployment token ID"
        );
        ensure!(
            self.named_token_id.is_none()
                || self.credential_id.is_none() && self.provider != DeploymentProvider::AzureSwa,
            "Choose either a named token or a target-specific token; Azure requires a target-specific token"
        );
        ensure!(
            self.provider == DeploymentProvider::CloudflareWorkers || self.cloudflare.is_none(),
            "Only Cloudflare targets use Cloudflare settings"
        );
        match self.provider {
            DeploymentProvider::AzureSwa => {
                ensure!(
                    self.site_id.is_empty(),
                    "Azure targets do not use a Netlify Site ID"
                );
                ensure!(
                    self.environment.is_empty()
                        || self.environment.len() <= 16
                            && self
                                .environment
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
                    "Azure environment must contain at most 16 letters, digits, or hyphens; leave blank for production"
                );
            }
            DeploymentProvider::Netlify => {
                ensure!(
                    self.environment.is_empty(),
                    "Netlify targets do not use an Azure environment"
                );
                ensure!(
                    self.site_id.len() == 36 && uuid::Uuid::parse_str(&self.site_id).is_ok(),
                    "Enter the Netlify Site ID (Project ID) from project configuration"
                );
            }
            DeploymentProvider::CloudflareWorkers => {
                ensure!(
                    self.environment.is_empty() && self.site_id.is_empty(),
                    "Cloudflare targets do not use an Azure environment or Netlify Site ID"
                );
                self.cloudflare
                    .as_ref()
                    .context("Enter the Cloudflare Account ID and Worker name")?
                    .validate()?;
            }
        }
        Ok(())
    }

    pub fn token(&self, root: &Path) -> Result<zeroize::Zeroizing<String>> {
        self.validate()?;
        if self.named_token_id.is_some() {
            return self.named_token(&tokens::TokenCatalog::load(&tokens::TokenCatalog::path()?)?);
        }
        let id = self
            .credential_id
            .as_deref()
            .context("No saved deployment token. Edit the target and enter its deployment token")?;
        credentials::DeploymentCredential::new(root, id)?.read()
    }

    pub fn named_token(
        &self,
        catalog: &tokens::TokenCatalog,
    ) -> Result<zeroize::Zeroizing<String>> {
        self.validate()?;
        let entry = catalog.find(
            self.named_token_id
                .as_deref()
                .context("Choose a named deployment token")?,
        )?;
        ensure!(
            entry.provider == self.provider,
            "The named token belongs to a different deployment provider"
        );
        if let Some(cloudflare) = &self.cloudflare {
            ensure!(
                entry
                    .account_id
                    .eq_ignore_ascii_case(&cloudflare.account_id),
                "The named token belongs to a different Cloudflare account"
            );
        }
        entry.token()
    }

    pub fn has_token(&self) -> bool {
        self.credential_id.is_some() || self.named_token_id.is_some()
    }

    fn same_destination(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.environment == other.environment
            && self.site_id == other.site_id
            && match (&self.cloudflare, &other.cloudflare) {
                (Some(left), Some(right)) => {
                    left.account_id.eq_ignore_ascii_case(&right.account_id)
                        && left.worker_name == right.worker_name
                        && left.workers_dev == right.workers_dev
                }
                (None, None) => true,
                _ => false,
            }
    }

    /// Everything that decides where and how a deployment publishes, including credentials
    /// and Cloudflare asset options; display names and saved URLs are excluded.
    fn same_publication_settings(&self, other: &Self) -> bool {
        let publication = |target: &Self| Self {
            name: String::new(),
            published_url: None,
            ..target.clone()
        };
        publication(self) == publication(other)
    }
}

pub fn validate_published_url(value: &str) -> Result<()> {
    ensure!(
        value.len() <= 4096
            && value.starts_with("https://")
            && !value.chars().any(|character| {
                character.is_whitespace() || character.is_control() || character == '\\'
            })
            && reqwest::Url::parse(value).is_ok_and(|url| {
                url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            }),
        "Invalid deployed site URL: expected an HTTPS URL without credentials, query, or fragment"
    );
    Ok(())
}

fn same_published_site(left: &str, right: &str) -> bool {
    match (reqwest::Url::parse(left), reqwest::Url::parse(right)) {
        (Ok(mut left), Ok(mut right)) => {
            if let Ok(mut segments) = left.path_segments_mut() {
                segments.pop_if_empty();
            }
            if let Ok(mut segments) = right.path_segments_mut() {
                segments.pop_if_empty();
            }
            left == right
        }
        _ => left.trim_end_matches('/') == right.trim_end_matches('/'),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportPublication {
    pub target_id: String,
    pub suite_ids: Vec<String>,
    /// `deployment_fingerprint` of the published content. Absent for legacy records and after
    /// an attempt whose outcome is unknown, so the next deployment always publishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_fingerprint: Option<String>,
    /// `deployment_input_fingerprint` of inputs known to generate `site_fingerprint`'s content,
    /// so an unchanged deployment can be recognized before generating anything. Only valid
    /// together with `site_fingerprint`; cleared with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_fingerprint: Option<String>,
}

/// Saved site URL and published content fingerprint of a deployment that needs no upload.
pub type UnchangedSite = (String, String);

fn valid_fingerprint(value: &str) -> bool {
    is_lower_hex(value, 64)
}

/// Whether `value` is exactly `len` lowercase hex digits, the form Regressify writes every
/// fingerprint and provider hash in. Records read back must match it exactly, because they are
/// compared as strings.
pub(crate) fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Identity of what a report deployment publishes: the staged site content plus the target
/// settings that shape the provider site (routing, HTML handling, exclusions, environment).
/// Display names, saved URLs, and credential references do not change the published site.
///
/// `staging` must already be verified by `stage_reports`; see `snapshot::site_fingerprint`.
pub fn deployment_fingerprint(
    staging: &Path,
    suite_ids: &[String],
    target: &DeployTarget,
) -> Result<String> {
    let site = crate::snapshot::site_fingerprint(staging, suite_ids)?;
    target_bound_fingerprint(b"regressify-deployment-fingerprint-v1", &site, target)
}

/// Identity of what a report deployment would be generated from: the snapshot inputs of its
/// suites and report text plus the same target settings as `deployment_fingerprint`. Equal to
/// a publication's recorded inputs, it proves the deployment would publish that content again.
pub fn deployment_input_fingerprint(
    output: &Path,
    job: &DeploymentJob,
    args: &crate::cli::args::SnapshotArgs,
    workspace: &crate::config::WorkspaceConfig,
) -> Result<String> {
    let inputs = crate::snapshot::input_fingerprint(output, &job.suite_ids, args, workspace)?;
    // The mode and removals shape the published site as much as the generated suites do.
    let mut removals = job.remove_ids.clone();
    removals.sort();
    let content = format!("{inputs}\0{:?}\0{}", job.mode, removals.join("\0"));
    target_bound_fingerprint(b"regressify-deployment-inputs-v2", &content, &job.target)
}

fn target_bound_fingerprint(kind: &[u8], content: &str, target: &DeployTarget) -> Result<String> {
    #[derive(Serialize)]
    struct PublishedSettings<'a> {
        provider: DeploymentProvider,
        environment: &'a str,
        site_id: &'a str,
        cloudflare: &'a Option<cloudflare::CloudflareTarget>,
    }
    let mut hasher = Sha256::new();
    hasher.update(kind);
    hasher.update([0]);
    hasher.update(content.as_bytes());
    hasher.update([0]);
    hasher.update(serde_json::to_vec(&PublishedSettings {
        provider: target.provider,
        environment: &target.environment,
        site_id: &target.site_id,
        cloudflare: &target.cloudflare,
    })?);
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeployTargets {
    #[serde(default)]
    pub targets: Vec<DeployTarget>,
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub report_publications: Vec<ReportPublication>,
}

impl DeployTargets {
    pub fn path(root: &Path) -> Result<PathBuf> {
        Self::path_in(root, &super::workspace::Preferences::path()?)
    }

    pub fn path_in(root: &Path, preferences: &Path) -> Result<PathBuf> {
        let key = workspace_key(root)?;
        Ok(preferences
            .parent()
            .context("Preferences directory unavailable")?
            .join("deployments")
            .join(format!("{key}.json")))
    }

    pub fn load(path: &Path) -> Result<Self> {
        match fs::File::open(path) {
            Ok(file) => {
                let bytes = bounded_read(file, 1024 * 1024, "Deployment target settings")
                    .with_context(|| {
                        format!(
                            "Cannot read deployment target settings '{}'",
                            path.display()
                        )
                    })?;
                let value: Self =
                    serde_json::from_slice(&bytes).context("Invalid deployment target settings")?;
                value.validate()?;
                Ok(value)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "Cannot read deployment target settings '{}'",
                    path.display()
                )
            }),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let directory = path
            .parent()
            .context("Deployment settings directory unavailable")?;
        fs::create_dir_all(directory).with_context(|| {
            format!(
                "Cannot create deployment settings directory '{}'",
                directory.display()
            )
        })?;
        crate::helpers::write_file_atomically(path, &serde_json::to_vec_pretty(self)?).with_context(
            || {
                format!(
                    "Cannot save deployment target settings '{}'",
                    path.display()
                )
            },
        )
    }

    pub fn record_published_url(path: &Path, target: &DeployTarget, url: &str) -> Result<Self> {
        Self::record_publication(path, target, Some(url))
    }

    pub fn record_publication(
        path: &Path,
        target: &DeployTarget,
        url: Option<&str>,
    ) -> Result<Self> {
        Self::record_publication_for_suites(path, target, url, &[], None, None)
    }

    pub fn record_report_publication(
        path: &Path,
        target: &DeployTarget,
        url: Option<&str>,
        suite_ids: &[String],
    ) -> Result<Self> {
        Self::record_deployed_report(path, target, url, suite_ids, None, None)
    }

    /// Records a confirmed report publication with the fingerprint of its published content and
    /// of the inputs it was generated from.
    pub fn record_deployed_report(
        path: &Path,
        target: &DeployTarget,
        url: Option<&str>,
        suite_ids: &[String],
        fingerprint: Option<&str>,
        inputs: Option<&str>,
    ) -> Result<Self> {
        ensure!(
            !suite_ids.is_empty(),
            "Published reports must contain at least one suite"
        );
        ensure!(
            inputs.is_none() || fingerprint.is_some(),
            "Report inputs can only be recorded with their published content"
        );
        Self::record_publication_for_suites(path, target, url, suite_ids, fingerprint, inputs)
    }

    /// The fingerprint of `target`'s last confirmed report publication, if it still describes
    /// the saved target's destination.
    pub fn report_fingerprint(&self, target: &DeployTarget) -> Option<&str> {
        self.targets
            .iter()
            .find(|saved| saved.id == target.id)
            .filter(|saved| saved.published_url.is_some() && saved.same_destination(target))?;
        self.report_publications
            .iter()
            .find(|publication| publication.target_id == target.id)?
            .site_fingerprint
            .as_deref()
    }

    /// The saved site URL when `fingerprint` matches `target`'s last confirmed publication, so
    /// publishing again would serve the same site.
    pub fn unchanged_site_url(&self, target: &DeployTarget, fingerprint: &str) -> Option<String> {
        if self.report_fingerprint(target)? != fingerprint {
            return None;
        }
        self.saved_url(target)
    }

    /// The saved site URL and published content fingerprint when `inputs` match the inputs
    /// recorded for `target`'s last confirmed publication, so generating the deployment again
    /// would reproduce that content.
    pub fn unchanged_inputs(&self, target: &DeployTarget, inputs: &str) -> Option<UnchangedSite> {
        let fingerprint = self.report_fingerprint(target)?;
        let publication = self
            .report_publications
            .iter()
            .find(|publication| publication.target_id == target.id)?;
        if publication.input_fingerprint.as_deref() != Some(inputs) {
            return None;
        }
        Some((self.saved_url(target)?, fingerprint.to_owned()))
    }

    fn saved_url(&self, target: &DeployTarget) -> Option<String> {
        self.targets
            .iter()
            .find(|saved| saved.id == target.id)?
            .published_url
            .clone()
    }

    /// Records that `inputs` generate the content `fingerprint` already published to
    /// `target_id`, after a deployment skipped its upload because only the inputs changed
    /// (for example, a new run with identical captures). Nothing changes when the published
    /// content is no longer `fingerprint`.
    pub fn record_report_inputs(
        path: &Path,
        target_id: &str,
        fingerprint: &str,
        inputs: &str,
    ) -> Result<()> {
        let mut settings = Self::load(path)?;
        let Some(publication) = settings
            .report_publications
            .iter_mut()
            .find(|publication| publication.target_id == target_id)
            .filter(|publication| {
                publication.site_fingerprint.as_deref() == Some(fingerprint)
                    && publication.input_fingerprint.as_deref() != Some(inputs)
            })
        else {
            return Ok(());
        };
        publication.input_fingerprint = Some(inputs.to_owned());
        settings.save(path)
    }

    /// Forgets the recorded content of every target publishing to `target`'s site before or
    /// after a deployment attempt whose effect on the site is unknown, while keeping suite
    /// membership for Open Site links. Otherwise a failed upload followed by a return to the
    /// recorded content could skip a needed publish.
    ///
    /// Targets share a site when they have the same destination or the same saved site URL,
    /// matching how a confirmed publication replaces the membership of a shared site.
    pub fn forget_report_fingerprint(path: &Path, target: &DeployTarget) -> Result<()> {
        let mut settings = Self::load(path)?;
        let saved_url = settings
            .targets
            .iter()
            .find(|saved| saved.id == target.id)
            .and_then(|saved| saved.published_url.clone());
        let shared = settings
            .targets
            .iter()
            .filter(|saved| {
                saved.id == target.id
                    || saved.same_destination(target)
                    || saved.published_url.as_deref().is_some_and(|url| {
                        saved_url
                            .as_deref()
                            .is_some_and(|own| same_published_site(url, own))
                    })
            })
            .map(|saved| saved.id.clone())
            .collect::<BTreeSet<_>>();
        let mut changed = false;
        for publication in &mut settings.report_publications {
            if (shared.contains(&publication.target_id) || publication.target_id == target.id)
                && (publication.site_fingerprint.is_some()
                    || publication.input_fingerprint.is_some())
            {
                publication.site_fingerprint = None;
                publication.input_fingerprint = None;
                changed = true;
            }
        }
        if changed {
            settings.save(path)?;
        }
        Ok(())
    }

    fn record_publication_for_suites(
        path: &Path,
        target: &DeployTarget,
        url: Option<&str>,
        suite_ids: &[String],
        fingerprint: Option<&str>,
        inputs: Option<&str>,
    ) -> Result<Self> {
        if let Some(url) = url {
            validate_published_url(url)?;
        } else {
            ensure!(
                target.provider == DeploymentProvider::CloudflareWorkers,
                "This provider must return a published URL"
            );
        }
        let mut settings = Self::load(path)?;
        let saved = settings
            .targets
            .iter_mut()
            .find(|saved| saved.id == target.id)
            .context("Deployment target no longer exists; the deployed URL was not saved")?;
        ensure!(
            saved.same_destination(target)
                && (target.provider != DeploymentProvider::AzureSwa
                    || saved.credential_id == target.credential_id),
            "Deployment target changed while publishing; the deployed URL was not saved"
        );
        let previous_url = saved.published_url.clone();
        saved.published_url = url.map(str::to_owned);
        // A new publication replaces the site's membership, not just its latest URL.
        settings.report_publications.retain(|publication| {
            publication.target_id != target.id
                && !settings.targets.iter().any(|saved| {
                    saved.id == publication.target_id
                        && saved.published_url.as_deref().is_some_and(|saved_url| {
                            url.is_some_and(|url| same_published_site(saved_url, url))
                                || previous_url
                                    .as_deref()
                                    .is_some_and(|prev| same_published_site(saved_url, prev))
                        })
                })
        });
        if url.is_some() && !suite_ids.is_empty() {
            settings.report_publications.push(ReportPublication {
                target_id: target.id.clone(),
                suite_ids: suite_ids.to_vec(),
                site_fingerprint: fingerprint.map(str::to_owned),
                input_fingerprint: inputs.map(str::to_owned),
            });
        }
        settings.save(path)?;
        Ok(settings)
    }

    pub fn latest_report_url(&self, suite_ids: &[String]) -> Option<String> {
        if suite_ids.is_empty() || suite_ids.iter().any(|id| validate_suite_id(id).is_err()) {
            return None;
        }
        let publication = self.report_publications.iter().rev().find(|publication| {
            suite_ids
                .iter()
                .all(|id| publication.suite_ids.contains(id))
        })?;
        let published = self
            .targets
            .iter()
            .find(|target| target.id == publication.target_id)?
            .published_url
            .as_deref()?;
        validate_published_url(published).ok()?;
        let mut url = reqwest::Url::parse(published).ok()?;
        if let [suite] = suite_ids {
            url.path_segments_mut()
                .ok()?
                .pop_if_empty()
                .push("suites")
                .push(suite);
        }
        Some(url.into())
    }

    pub fn save_target(
        &mut self,
        root: &Path,
        path: &Path,
        mut target: DeployTarget,
        token: &str,
    ) -> Result<()> {
        ensure!(
            target.named_token_id.is_none(),
            "Save named token selections without entering a target-specific token"
        );
        let token = token.trim();
        let existing = self
            .targets
            .iter()
            .find(|previous| previous.id == target.id);
        let existing_id = existing.and_then(|previous| previous.credential_id.clone());
        let provider_changed =
            existing.is_some_and(|previous| previous.provider != target.provider);
        ensure!(
            !token.is_empty() || !provider_changed,
            "Enter a new deployment token when changing providers"
        );
        ensure!(
            !token.is_empty() || existing_id.is_some(),
            "Enter a deployment token"
        );
        if !token.is_empty() {
            target.provider.validate_token(token)?;
        }
        // An Azure token identifies the destination app as well as authorizing it.
        target.published_url = existing
            .filter(|previous| {
                previous.same_destination(&target)
                    && (target.provider != DeploymentProvider::AzureSwa || token.is_empty())
            })
            .and_then(|previous| previous.published_url.clone());
        let credential_id = existing_id
            .clone()
            .filter(|_| !provider_changed)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        target.credential_id = Some(credential_id.clone());
        let mut next = self.clone();
        next.targets.retain(|previous| previous.id != target.id);
        if target.published_url.is_none() {
            next.report_publications
                .retain(|publication| publication.target_id != target.id);
        }
        // An Azure token identifies the app, so replacing it may point at another site.
        let other_site = self.targets.iter().any(|previous| {
            previous.id == target.id
                && (!previous.same_destination(&target)
                    || (target.provider == DeploymentProvider::AzureSwa && !token.is_empty()))
        });
        let target_id = target.id.clone();
        next.selected = Some(target.id.clone());
        next.targets.push(target);
        next.validate()?;
        let credential = credentials::DeploymentCredential::new(root, &credential_id)?;
        let previous = if existing_id.is_some() && !provider_changed {
            credential.read_optional()?
        } else {
            None
        };
        let retired = existing_id
            .as_deref()
            .filter(|_| provider_changed)
            .map(|id| credentials::DeploymentCredential::new(root, id))
            .transpose()?;
        let retired_token = retired
            .as_ref()
            .map(|credential| credential.read_optional())
            .transpose()?
            .flatten();
        ensure!(
            !token.is_empty() || previous.is_some(),
            "No saved deployment token. Enter the deployment token again"
        );
        if !token.is_empty() {
            credential.write(token)?;
        }
        let saved = (|| -> Result<()> {
            if let Some(retired) = &retired {
                retired.delete()?;
            }
            next.save(path)
        })();
        if let Err(error) = saved {
            if let (Some(retired), Some(retired_token)) = (&retired, &retired_token) {
                retired
                    .write(retired_token)
                    .context("Cannot restore the previous provider's credential")?;
            }
            if !token.is_empty() {
                match previous.as_deref() {
                    Some(previous) => credential.write(previous),
                    None => credential.delete(),
                }
                .context(
                    "Cannot restore the secure credential after saving target settings failed",
                )?;
            }
            return Err(error).context("Cannot save deployment target settings");
        }
        *self = next;
        if other_site {
            forget_site_records(path, &target_id);
        }
        Ok(())
    }

    pub fn save_linked_target(
        &mut self,
        root: &Path,
        path: &Path,
        mut target: DeployTarget,
        catalog: &tokens::TokenCatalog,
    ) -> Result<()> {
        target.named_token(catalog)?;
        target.published_url = self
            .targets
            .iter()
            .find(|previous| previous.id == target.id && previous.same_destination(&target))
            .and_then(|previous| previous.published_url.clone());
        let previous_id = self
            .targets
            .iter()
            .find(|entry| entry.id == target.id)
            .and_then(|entry| entry.credential_id.as_deref());
        let credential = previous_id
            .map(|id| credentials::DeploymentCredential::new(root, id))
            .transpose()?;
        let previous = credential
            .as_ref()
            .map(|entry| entry.read_optional())
            .transpose()?
            .flatten();
        let mut next = self.clone();
        next.targets.retain(|entry| entry.id != target.id);
        let other_site = self
            .targets
            .iter()
            .any(|previous| previous.id == target.id && !previous.same_destination(&target));
        let target_id = target.id.clone();
        if target.published_url.is_none() {
            next.report_publications
                .retain(|publication| publication.target_id != target.id);
        }
        next.selected = Some(target.id.clone());
        next.targets.push(target);
        next.validate()?;
        if let Some(credential) = &credential {
            credential.delete()?;
        }
        if let Err(error) = next.save(path) {
            if let (Some(credential), Some(previous)) = (&credential, &previous) {
                credential
                    .write(previous)
                    .context("Cannot restore the target-specific token after saving failed")?;
            }
            return Err(error).context("Cannot save deployment target settings");
        }
        *self = next;
        if other_site {
            forget_site_records(path, &target_id);
        }
        Ok(())
    }

    pub fn remove_target(&mut self, root: &Path, path: &Path, id: &str) -> Result<()> {
        let target = self
            .targets
            .iter()
            .find(|target| target.id == id)
            .context("Deployment target no longer exists")?;
        let credential = target
            .credential_id
            .as_deref()
            .map(|id| credentials::DeploymentCredential::new(root, id))
            .transpose()?;
        let previous = credential
            .as_ref()
            .map(|credential| credential.read_optional())
            .transpose()?
            .flatten();
        let mut next = self.clone();
        next.targets.retain(|target| target.id != id);
        next.report_publications
            .retain(|publication| publication.target_id != id);
        if next.selected.as_deref() == Some(id) {
            next.selected = next.targets.first().map(|target| target.id.clone());
        }
        next.validate()?;
        if let Some(credential) = &credential {
            credential.delete()?;
        }
        if let Err(error) = next.save(path) {
            if let (Some(credential), Some(previous)) = (&credential, &previous) {
                credential.write(previous).context(
                    "Cannot restore the secure credential after removing target settings failed",
                )?;
            }
            return Err(error).context("Cannot remove deployment target settings");
        }
        *self = next;
        forget_site_records(path, id);
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.targets.len() <= 50,
            "A workspace supports up to 50 deployment targets"
        );
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut credentials = BTreeSet::new();
        for target in &self.targets {
            target.validate()?;
            ensure!(
                ids.insert(&target.id) && names.insert(target.name.trim().to_lowercase()),
                "Deployment target names and IDs must be unique"
            );
            ensure!(
                target
                    .credential_id
                    .as_ref()
                    .is_none_or(|id| credentials.insert(id)),
                "Each deployment target must have its own saved credential"
            );
        }
        ensure!(
            self.selected.as_ref().is_none_or(|id| ids.contains(id)),
            "Selected deployment target no longer exists"
        );
        let mut published = BTreeSet::new();
        for publication in &self.report_publications {
            ensure!(
                published.insert(&publication.target_id)
                    && self.targets.iter().any(|target| {
                        target.id == publication.target_id && target.published_url.is_some()
                    }),
                "Published reports must identify a unique target with a public URL"
            );
            let mut suites = BTreeSet::new();
            ensure!(
                !publication.suite_ids.is_empty(),
                "Published reports must contain at least one suite"
            );
            ensure!(
                publication
                    .site_fingerprint
                    .as_deref()
                    .is_none_or(valid_fingerprint)
                    && publication
                        .input_fingerprint
                        .as_deref()
                        .is_none_or(valid_fingerprint)
                    && (publication.input_fingerprint.is_none()
                        || publication.site_fingerprint.is_some()),
                "Invalid published report fingerprint"
            );
            for id in &publication.suite_ids {
                validate_suite_id(id)?;
                ensure!(
                    suites.insert(id),
                    "Published report suite IDs must be unique"
                );
            }
        }
        Ok(())
    }
}

/// Forgets the local records of a target's site after its settings were committed. The
/// settings are already saved, so a failure is a warning rather than an error that would make
/// the editor treat the saved change as failed; a stale copy is still destination-checked
/// when read back through the site, and the next deployment to the target replaces it.
fn forget_site_records(path: &Path, target_id: &str) {
    if let Err(error) = site_state::forget_target(path, target_id) {
        eprintln!("Warning: Cannot clear the previous site state of the target: {error:#}");
    }
}

/// Files a site keeps serving without local content, by site-relative path, with the
/// provider's content identity from the site's published state.
pub(crate) type KeptFiles = std::collections::BTreeMap<String, site_state::StateFile>;

/// Files a deployment keeps on the site by hash, and how the images uploaded beside them may
/// change to fit a storage budget.
#[derive(Clone, Copy)]
pub(crate) struct Keeping<'a> {
    pub files: &'a KeptFiles,
    /// The uploaded images may be reduced in quality, not only recompressed losslessly,
    /// although files stay kept (Cloudflare only).
    pub reduce_quality: bool,
}

impl<'a> Keeping<'a> {
    /// Keeps `files`, recompressing the images beside them only losslessly.
    pub(crate) fn files(files: &'a KeptFiles) -> Self {
        Self {
            files,
            reduce_quality: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DeploymentStats {
    file_count: u64,
    original_bytes: u64,
    total_bytes: u64,
    /// A site state was staged but not published: excluded by the target, or, for optimized
    /// Cloudflare uploads, impossible to rewrite for the optimized files.
    state_dropped: bool,
}

/// What a provider manifest lists: staged files with local content, and files kept on the
/// site by their provider identity (which have no local content to deduplicate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ManifestCounts {
    pub staged_files: usize,
    pub staged_bytes: u64,
    /// Distinct local contents among the staged files.
    pub unique: usize,
    pub kept_files: usize,
    pub kept_bytes: u64,
}

impl ManifestCounts {
    pub fn files(&self) -> usize {
        self.staged_files + self.kept_files
    }

    pub fn bytes(&self) -> u64 {
        self.staged_bytes.saturating_add(self.kept_bytes)
    }

    /// A log summary that keeps staged and kept counts apart, e.g. "743 files: 150 staged
    /// (89.55 MiB, 130 unique digests), 593 kept by digest (2.10 MiB)".
    pub fn describe(&self, unique: &str, kept_by: &str) -> String {
        let mib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
        if self.kept_files == 0 {
            return format!(
                "{} files ({:.2} MiB, {} unique {unique})",
                self.staged_files,
                mib(self.staged_bytes),
                self.unique
            );
        }
        format!(
            "{} files: {} staged ({:.2} MiB, {} unique {unique}), {} kept by {kept_by} ({:.2} MiB)",
            self.files(),
            self.staged_files,
            mib(self.staged_bytes),
            self.unique,
            self.kept_files,
            mib(self.kept_bytes)
        )
    }
}

/// Share of PNG bytes lossless recompression is expected to save on report screenshots. About
/// 10% was measured on the largest Chromium captures and more on smaller ones. It only ranks
/// the kept suites a Cloudflare update brings back to optimize; the staged suites' savings are
/// measured, and the optimizer stops as soon as the site fits.
const EXPECTED_LOSSLESS_SAVING_PERCENT: u64 = 10;

/// Bytes lossless recompression of `png_bytes` is expected to save.
pub(crate) fn expected_lossless_saving(png_bytes: u64) -> u64 {
    percent_of(png_bytes, EXPECTED_LOSSLESS_SAVING_PERCENT)
}

/// Share of a suite's original PNG bytes quality reduction down to the spreading floor (85)
/// is roughly expected to save beyond lossless recompression, as measured on report
/// screenshots (about a third together). It only decides how many kept suites a Cloudflare
/// retry brings back to spread the reduction over; every attempt measures what is still
/// missing.
const EXPECTED_REDUCTION_SAVING_PERCENT: u64 = 20;

/// Bytes quality reduction of `png_bytes` is roughly expected to save.
pub(crate) fn expected_reduction_saving(png_bytes: u64) -> u64 {
    percent_of(png_bytes, EXPECTED_REDUCTION_SAVING_PERCENT)
}

/// `percent` of `bytes`, rounded down, without overflowing.
fn percent_of(bytes: u64, percent: u64) -> u64 {
    (u128::from(bytes) * u128::from(percent) / 100) as u64
}

/// The site exceeds Cloudflare's storage budget by `remaining` bytes even after the staged
/// suites were recompressed losslessly, and kept suites cannot be optimized without their
/// files. `staged_png` counts the staged suites' PNG bytes before optimization. A report
/// deployment then brings back kept suites and tries again
/// (`ReportDeployment::generate_and_publish`).
#[derive(Debug)]
pub(crate) struct KeptSuitesOverBudget {
    pub remaining: u64,
    pub staged_png: u64,
}

impl std::fmt::Display for KeptSuitesOverBudget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "The site exceeds the Cloudflare content budget by {:.2} MiB, which the selected suites cannot be optimized to cover while other suites are kept; replace the whole site",
            self.remaining as f64 / (1024.0 * 1024.0)
        )
    }
}

impl std::error::Error for KeptSuitesOverBudget {}

/// A confirmed report publication.
pub(crate) struct Published {
    pub url: Option<String>,
    /// Whether the site serves the staged `deployment.json`, as the provider may have
    /// rewritten it in staging.
    pub state_published: bool,
}

pub enum DeploymentClient {
    Azure(swa::SwaClient),
    Netlify(netlify::NetlifyClient),
    Cloudflare(cloudflare::CloudflareClient),
}

impl DeploymentClient {
    pub fn new(target: &DeployTarget, token: &str) -> Result<Self> {
        target.validate()?;
        match target.provider {
            DeploymentProvider::AzureSwa => Ok(Self::Azure(swa::SwaClient::new(token)?)),
            DeploymentProvider::Netlify => Ok(Self::Netlify(netlify::NetlifyClient::new(token)?)),
            DeploymentProvider::CloudflareWorkers => {
                Ok(Self::Cloudflare(cloudflare::CloudflareClient::new(token)?))
            }
        }
    }

    pub async fn deploy(&self, root: &Path, target: &DeployTarget) -> Result<Option<String>> {
        Ok(self
            .deploy_keeping(root, target, Keeping::files(&KeptFiles::new()))
            .await?
            .url)
    }

    /// Publishes `root` as the whole site, plus the files `keeping` names that the site already
    /// serves. Only Netlify and Cloudflare address content by hash; Azure uploads complete
    /// packages, so its deployments must stage every file.
    pub(crate) async fn deploy_keeping(
        &self,
        root: &Path,
        target: &DeployTarget,
        keeping: Keeping<'_>,
    ) -> Result<Published> {
        target.validate()?;
        let started = std::time::Instant::now();
        let mut stats = DeploymentStats::default();
        let result = match self {
            Self::Azure(client) => {
                ensure!(
                    keeping.files.is_empty(),
                    "Azure Static Web Apps cannot keep files that are not uploaded"
                );
                client
                    .deploy_with_stats(root, target, &mut stats)
                    .await
                    .map(Some)
            }
            Self::Netlify(client) => client
                .deploy_with_stats(root, target, keeping.files, &mut stats)
                .await
                .map(Some),
            Self::Cloudflare(client) => {
                ensure!(
                    target.provider == DeploymentProvider::CloudflareWorkers,
                    "Cloudflare client requires a Cloudflare target"
                );
                client
                    .deploy_with_stats(
                        root,
                        target
                            .cloudflare
                            .as_ref()
                            .context("Cloudflare target settings are missing")?,
                        keeping,
                        &mut stats,
                    )
                    .await
            }
        };
        if let Some(properties) =
            site_deployed_properties(target.provider, &result, stats, started.elapsed())
        {
            crate::analytics::capture("site", "deployed", properties).await;
        }
        result.map(|url| Published {
            url,
            state_published: !stats.state_dropped,
        })
    }
}

fn site_deployed_properties(
    provider: DeploymentProvider,
    result: &Result<Option<String>>,
    stats: DeploymentStats,
    duration: std::time::Duration,
) -> Option<serde_json::Value> {
    let url = result.as_ref().ok()?.as_deref()?;
    validate_published_url(url).ok()?;
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    Some(serde_json::json!({
        "url": url,
        "current_url": url,
        "provider": provider,
        "file_count": stats.file_count,
        "original_bytes": stats.original_bytes,
        "total_bytes": stats.total_bytes,
        "duration_ms": duration_ms,
    }))
}

fn validate_static_configuration(root: &Path, provider: DeploymentProvider) -> Result<()> {
    let root = root.canonicalize()?;
    let Some(path) = artifact_source(&root, Path::new("staticwebapp.config.json"), true)? else {
        return Ok(());
    };
    let config: serde_json::Value = serde_json::from_slice(&bounded_read(
        fs::File::open(path).context("Cannot read Azure static site configuration")?,
        1024 * 1024,
        "Static site configuration",
    )?)
    .map_err(|_| anyhow::anyhow!("Invalid Azure static site configuration"))?;
    let restricted = config.get("auth").is_some()
        || config.get("networking").is_some()
        || config["routes"].as_array().is_some_and(|routes| {
            routes.iter().any(|route| {
                route.get("allowedRoles").is_some_and(|roles| {
                    !roles
                        .as_array()
                        .is_some_and(|roles| roles.iter().any(|role| role == "anonymous"))
                })
            })
        });
    let protection = if provider == DeploymentProvider::CloudflareWorkers {
        "Cloudflare Access"
    } else {
        provider.label()
    };
    ensure!(
        !restricted,
        "Azure access restrictions cannot be enforced by {}; configure {protection} access protection before deploying these reports",
        provider.label()
    );
    Ok(())
}

#[cfg(test)]
pub(super) async fn with_deployment_journal<Output>(
    output: &Path,
    work: impl std::future::Future<Output = Result<Output>>,
) -> Result<Output> {
    with_deployment_journal_id(output, None, work).await
}

pub(super) async fn with_deployment_journal_id<Output>(
    output: &Path,
    id: Option<crate::diagnostics::record::OperationId>,
    work: impl std::future::Future<Output = Result<Output>>,
) -> Result<Output> {
    use crate::diagnostics::{
        record::{OperationKind, Outcome},
        runtime,
    };
    let operation = runtime::Operation::start_identified(
        output,
        OperationKind::Deployment,
        None,
        None,
        None,
        id,
    );
    let result = runtime::scope(Some(operation.context()), work).await;
    operation
        .finish(if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Failure
        })
        .await;
    result
}

fn phase(name: &str) {
    use crate::diagnostics::{record::*, runtime};
    if runtime::current().is_none() {
        eprintln!("{name}");
    }
    runtime::event(
        runtime::current_stage().unwrap_or(Stage::ApiUploads),
        DiagnosticEvent::Decision,
        Severity::Info,
        [(
            Field::Artifact,
            FieldValue::Identity(SafeIdentity::source(name)),
        )],
    );
    crate::progress::emit(crate::progress::Event::Phase { name: name.into() });
}

pub(super) async fn with_heartbeat<Work: std::future::Future>(
    label: &str,
    work: Work,
) -> Work::Output {
    let stage = crate::diagnostics::runtime::current_stage()
        .unwrap_or(crate::diagnostics::record::Stage::RetryPolling);
    monitor(work, std::time::Duration::from_secs(5), |elapsed| {
        use crate::diagnostics::{record::*, runtime};
        let message = format!("{label}: still working ({}s elapsed)", elapsed.as_secs());
        if runtime::current().is_none() {
            eprintln!("{message}");
        }
        runtime::event(
            stage,
            DiagnosticEvent::Heartbeat,
            Severity::Info,
            [
                (
                    Field::Artifact,
                    FieldValue::Identity(SafeIdentity::source(label)),
                ),
                (
                    Field::ElapsedMs,
                    FieldValue::Number(elapsed.as_millis() as u64),
                ),
            ],
        );
        crate::progress::emit(crate::progress::Event::Phase { name: message });
    })
    .await
}

async fn monitor<Work: std::future::Future>(
    work: Work,
    interval: std::time::Duration,
    mut notify: impl FnMut(std::time::Duration),
) -> Work::Output {
    let started = tokio::time::Instant::now();
    let mut timer = tokio::time::interval_at(started + interval, interval);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::pin!(work);
    loop {
        tokio::select! {
            output = &mut work => return output,
            _ = timer.tick() => notify(started.elapsed()),
        }
    }
}

pub(super) fn workspace_key(root: &Path) -> Result<String> {
    let root = root.canonicalize()?;
    let mut identity = super::workspace::display_path(&root);
    if cfg!(windows) {
        identity.make_ascii_lowercase();
    }
    Ok(format!("{:x}", Sha256::digest(identity.as_bytes())))
}

fn bounded_read(file: fs::File, limit: usize, label: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "{label} exceeds its size limit");
    Ok(bytes)
}

fn validate_suite_id(suite: &str) -> Result<()> {
    ensure!(
        !suite.is_empty()
            && !suite.starts_with('.')
            && suite == suite.trim()
            && suite.len() <= 200
            && !crate::test::is_reserved_suite_identifier(suite)
            && Path::new(suite).components().count() == 1
            && Path::new(suite)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && !suite.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*'])
            && !suite.chars().any(char::is_control)
            && !crate::helpers::is_windows_reserved_path_component(suite),
        "Invalid deployment suite ID"
    );
    Ok(())
}

fn artifact_source(
    output: &Path,
    relative: &Path,
    missing_reference: bool,
) -> Result<Option<PathBuf>> {
    ensure!(
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
            && !relative
                .to_string_lossy()
                .contains([':', '\0', '<', '>', '"', '|', '?', '*']),
        "Report path escapes deployment output"
    );
    let mut source = output.to_path_buf();
    for component in relative.components() {
        source.push(component);
        match fs::symlink_metadata(&source) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "Deployment artifacts cannot be symlinks"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && missing_reference => {
                return Ok(None);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Deployment artifact '{}' is unavailable",
                        relative.display()
                    )
                });
            }
        }
    }
    let source = source.canonicalize()?;
    ensure!(
        source.starts_with(output) && source.is_file(),
        "Deployment artifact must be a regular file inside report output"
    );
    Ok(Some(source))
}

fn read_report_configuration(path: &Path) -> Result<serde_json::Value> {
    let text = String::from_utf8(bounded_read(
        fs::File::open(path)?,
        16 * 1024 * 1024,
        "Report configuration",
    )?)?;
    let data = text
        .trim()
        .strip_prefix("report(")
        .and_then(|value| value.strip_suffix(");"))
        .context("Invalid report configuration")?;
    serde_json::from_str(data).context("Invalid report configuration JSON")
}

fn report_image_relative(suite: &str, key: &str, image: &str) -> Result<PathBuf> {
    let image = image
        .split('?')
        .next()
        .unwrap_or_default()
        .replace('\\', "/");
    let image = image
        .strip_prefix("../")
        .context("Report image must be suite-relative")?;
    ensure!(
        image.starts_with(if key == "reference" {
            "bitmaps_reference/"
        } else {
            "bitmaps_test/"
        }) && Path::new(image)
            .extension()
            .is_some_and(|extension| extension == "png")
            && Path::new(image)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "Report image is outside capture folders"
    );
    Ok(Path::new(suite).join(image))
}

/// Fingerprints the staged reports of `job` and decides whether they must be published.
///
/// Returns the saved site URL as well when the target's last confirmed publication already
/// has this content and `job` is not forced; nothing must be uploaded then. Otherwise the
/// target's recorded content is forgotten before returning, so an upload interrupted by a
/// crash or kill, which never reaches the runner's completion handling, cannot leave a
/// record that matches content the site may no longer serve.
///
/// Settings are read from disk here rather than taken from the request: the request can be
/// older than the latest deployment (a stale record could match) or than target edits (Deploy
/// anyway replays a saved request), so a removed or edited target is refused.
///
/// When the upload is skipped and `inputs` are known, they are recorded as generating the
/// published content, so the next unchanged deployment is recognized by `unchanged_inputs`
/// before any generation. Deployments call `decide_publication` with the site's fingerprint;
/// this local-only form is the tests' entry point.
#[cfg(test)]
pub fn unchanged_deployment(
    staging: &Path,
    job: &DeploymentJob,
    settings: Option<&Path>,
    inputs: Option<&str>,
) -> Result<(String, Option<String>)> {
    phase("Comparing staged reports with the last deployment to this target");
    let fingerprint = deployment_fingerprint(staging, &job.suite_ids, &job.target)?;
    let url = decide_publication(job, settings, &fingerprint, None, inputs)?;
    Ok((fingerprint, url))
}

/// Decides whether content with `fingerprint` must be published; see `unchanged_deployment`.
///
/// `remote` is the fingerprint the site itself reports, when it could be read. It is the
/// authority: the local record only decides when the site could not be read, because another
/// machine may have changed the site since this workspace recorded it.
pub(crate) fn decide_publication(
    job: &DeploymentJob,
    settings: Option<&Path>,
    fingerprint: &str,
    remote: Option<&str>,
    inputs: Option<&str>,
) -> Result<Option<String>> {
    let Some(path) = settings else {
        return Ok(None);
    };
    let saved = current_settings(path, job)?;
    let unchanged_url = || match remote {
        Some(remote) if remote == fingerprint => saved
            .targets
            .iter()
            .find(|target| target.id == job.target.id)
            .and_then(|target| target.published_url.clone()),
        Some(_) => None,
        None => saved.unchanged_site_url(&job.target, fingerprint),
    };
    if job.force {
        phase("Deploy anyway requested; publishing without comparing to the last deployment");
    } else if let Some(url) = unchanged_url() {
        phase("No changes since the last deployment to this target; skipping the upload");
        if let Some(inputs) = inputs
            && let Err(error) =
                DeployTargets::record_report_inputs(path, &job.target.id, fingerprint, inputs)
        {
            // Only the next deployment's early check depends on it; this one is complete.
            eprintln!(
                "Warning: Cannot remember the unchanged report inputs; the next deployment will regenerate its reports before comparing: {error:#}"
            );
        }
        return Ok(Some(url));
    }
    // The local site copy stays: whether this upload lands or not, the suites it lists are on
    // the site before or after, so it remains the safest plan for a later unreadable update.
    DeployTargets::forget_report_fingerprint(path, &job.target)
        .context("Cannot clear the target's last deployment record before publishing")?;
    Ok(None)
}

/// The early check before any report generation: returns the saved site URL and published
/// content fingerprint when `inputs` match the inputs recorded for the target's last confirmed
/// publication and `job` is not forced. Applies the same stale-target checks as
/// `decide_publication`; a mismatch publishes nothing and changes nothing. When the site's own
/// fingerprint is known, it must still report the recorded content: another machine may have
/// published to it since.
pub(crate) fn unchanged_deployment_inputs_on(
    job: &DeploymentJob,
    settings: Option<&Path>,
    inputs: &str,
    remote: Option<&str>,
) -> Result<Option<UnchangedSite>> {
    let Some(path) = settings else {
        return Ok(None);
    };
    let saved = current_settings(path, job)?;
    if job.force {
        return Ok(None);
    }
    let unchanged = saved
        .unchanged_inputs(&job.target, inputs)
        .filter(|(_, fingerprint)| remote.is_none_or(|remote| remote == fingerprint));
    if unchanged.is_some() {
        phase(
            "No changes to the selected reports since the last deployment to this target; skipping report generation and upload",
        );
    }
    Ok(unchanged)
}

/// Computes the deployment's input fingerprint and runs the early check.
///
/// Returns the inputs (to record after publishing) and, when they match the last confirmed
/// publication, its URL and content fingerprint. Inputs that cannot be fingerprinted only
/// disable the early check: report generation and the content comparison still decide. Stale
/// targets and unreadable settings fail, as they would after generation. Deployments call
/// `check_unchanged_inputs_on` with the site's fingerprint; this form is the tests' entry point.
#[cfg(test)]
pub fn check_unchanged_inputs(
    output: &Path,
    root: &Path,
    job: &DeploymentJob,
    args: &crate::cli::args::SnapshotArgs,
    settings: Option<&Path>,
) -> Result<(Option<String>, Option<UnchangedSite>)> {
    check_unchanged_inputs_on(output, root, job, args, settings, None)
}

pub(crate) fn check_unchanged_inputs_on(
    output: &Path,
    root: &Path,
    job: &DeploymentJob,
    args: &crate::cli::args::SnapshotArgs,
    settings: Option<&Path>,
    remote: Option<&str>,
) -> Result<(Option<String>, Option<UnchangedSite>)> {
    phase("Checking the selected reports for changes since the last deployment");
    let inputs = match crate::config::load_workspace_config(root)
        .and_then(|workspace| deployment_input_fingerprint(output, job, args, &workspace))
    {
        Ok(inputs) => inputs,
        Err(error) => {
            eprintln!(
                "Warning: Cannot check the selected reports for changes before generating them: {error:#}"
            );
            return Ok((None, None));
        }
    };
    let unchanged = unchanged_deployment_inputs_on(job, settings, &inputs, remote)?;
    Ok((Some(inputs), unchanged))
}

/// Current settings, refusing a job prepared for a target that was since removed or edited.
fn current_settings(path: &Path, job: &DeploymentJob) -> Result<DeployTargets> {
    let saved = DeployTargets::load(path)?;
    let current = saved
        .targets
        .iter()
        .find(|saved| saved.id == job.target.id)
        .context("The deployment target no longer exists; open Deploy and choose a target")?;
    ensure!(
        current.same_publication_settings(&job.target),
        "The deployment target changed after this deployment was prepared; open Deploy again"
    );
    Ok(saved)
}

pub fn stage_reports(output: &Path, suite_ids: &[String], destination: &Path) -> Result<()> {
    ensure!(
        !suite_ids.is_empty(),
        "Select at least one test suite to deploy"
    );
    stage_site(output, suite_ids, &[], destination).map(drop)
}

/// Stages a site of the generated `suite_ids` plus `kept` suites the site already serves.
///
/// Only generated suites' files and the bundled frontend are copied; kept suites contribute
/// their summaries to a merged manifest, status, and entry page, and their files stay on the
/// site. The staged site is verified with kept suites absent, and its inventory is returned.
pub(crate) fn stage_site(
    output: &Path,
    suite_ids: &[String],
    kept: &[crate::snapshot::SuiteSummary],
    destination: &Path,
) -> Result<crate::snapshot::SiteInventory> {
    use crate::diagnostics::{record::*, runtime};
    runtime::stage_sync(
        Stage::SnapshotStaging,
        [
            (
                Field::SelectedSuites,
                FieldValue::Number(suite_ids.len() as u64),
            ),
            (
                Field::Artifact,
                FieldValue::Identity(SafeIdentity::fingerprint("index.html")),
            ),
        ],
        || stage_site_inner(output, suite_ids, kept, destination),
    )
}

fn stage_site_inner(
    output: &Path,
    suite_ids: &[String],
    kept: &[crate::snapshot::SuiteSummary],
    destination: &Path,
) -> Result<crate::snapshot::SiteInventory> {
    let output = output
        .canonicalize()
        .context("Report output directory is unavailable")?;
    ensure!(
        !destination.exists(),
        "Deployment staging directory must be new"
    );
    phase(&format!(
        "Validating snapshot manifest and asset versions for {} selected {}",
        suite_ids.len(),
        if suite_ids.len() == 1 {
            "suite"
        } else {
            "suites"
        }
    ));
    let inventory = crate::snapshot::artifact_inventory(&output, suite_ids, &[])?;
    let mut dependencies = inventory.shared;
    if !kept.is_empty() {
        // A merged entry page, manifest, and status replace the generated ones below.
        dependencies.retain(|path| {
            path != Path::new("index.html")
                && path != Path::new("_snapshot/status.json")
                && !path.starts_with("_snapshot/data")
        });
    }
    for files in inventory.suites.into_values() {
        dependencies.extend(files);
    }
    let dependency_count = dependencies.len();
    fs::create_dir_all(destination).with_context(|| {
        format!(
            "Cannot create deployment staging directory '{}'",
            destination.display()
        )
    })?;
    phase(&format!(
        "Staging {dependency_count} selected report dependencies"
    ));
    let mut copied = BTreeSet::new();
    let mut copied_bytes = 0_u64;
    let mut last_update = std::time::Instant::now();
    let staged = (|| -> Result<crate::snapshot::SiteInventory> {
        {
            let mut copy = |relative: &Path, missing_reference: bool| -> Result<()> {
                let Some(source) = artifact_source(&output, relative, missing_reference)
                    .inspect_err(|_| {
                        use crate::diagnostics::{record::*, runtime};
                        runtime::event(
                            Stage::SnapshotStaging,
                            DiagnosticEvent::StageFinished,
                            Severity::Error,
                            [(
                                Field::Artifact,
                                FieldValue::Identity(SafeIdentity::fingerprint(
                                    relative.to_string_lossy().as_bytes(),
                                )),
                            )],
                        );
                    })?
                else {
                    return Ok(());
                };
                if !copied.insert(relative.to_path_buf()) {
                    return Ok(());
                };
                let target = destination.join(relative);
                let directory = target.parent().context("Staging path has no parent")?;
                fs::create_dir_all(directory).with_context(|| {
                    format!(
                        "Cannot create deployment staging directory '{}'",
                        directory.display()
                    )
                })?;
                let expected_bytes = fs::metadata(&source)?.len();
                let bytes = fs::copy(&source, &target).with_context(|| {
                    format!(
                        "Cannot stage deployment artifact '{}' from '{}' to '{}'",
                        relative.display(),
                        source.display(),
                        target.display()
                    )
                })?;
                ensure!(
                    bytes == expected_bytes,
                    "Report changed during deployment staging"
                );
                copied_bytes += bytes;
                if copied.len() == dependency_count {
                    use crate::diagnostics::{record::*, runtime};
                    runtime::event(
                        Stage::SnapshotStaging,
                        DiagnosticEvent::Decision,
                        Severity::Info,
                        [
                            (Field::FileCount, FieldValue::Number(copied.len() as u64)),
                            (Field::Bytes, FieldValue::Number(copied_bytes)),
                        ],
                    );
                }
                if copied.len() == 1
                    || copied.len() == dependency_count
                    || copied.len().is_multiple_of(50)
                    || last_update.elapsed().as_secs() >= 1
                {
                    phase(&format!(
                        "Staged {} of {dependency_count} report files ({:.2} MiB)",
                        copied.len(),
                        copied_bytes as f64 / (1024.0 * 1024.0)
                    ));
                    last_update = std::time::Instant::now();
                }
                Ok(())
            };
            for relative in dependencies {
                copy(&relative, false)?;
            }
        }
        let kept_ids = kept
            .iter()
            .map(|summary| summary.id.clone())
            .collect::<Vec<_>>();
        if !kept.is_empty() {
            phase(&format!(
                "Merging {} kept {} into the site manifest",
                kept.len(),
                if kept.len() == 1 { "suite" } else { "suites" }
            ));
            crate::snapshot::publish_merged_index(destination, &output, kept.to_vec())?;
        }
        phase(&format!(
            "Verifying {} staged report files ({:.2} MiB)",
            copied.len(),
            copied_bytes as f64 / (1024.0 * 1024.0)
        ));
        crate::snapshot::artifact_inventory(destination, suite_ids, &kept_ids)
    })();
    let staged = match staged {
        Ok(staged) => staged,
        Err(error) => {
            let _ = fs::remove_dir_all(destination);
            return Err(error);
        }
    };
    phase(&format!(
        "Staging complete: {} files ({:.2} MiB) for {} suites",
        copied.len(),
        copied_bytes as f64 / (1024.0 * 1024.0),
        suite_ids.len()
    ));
    Ok(staged)
}

#[cfg(test)]
mod tests {
    #[test]
    fn log_budget_exceeded_fails_actionably() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"), "report").unwrap();
        let log = root
            .path()
            .join("_snapshot/data/logs/selected-capture/chunk-000000.json");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::File::create(&log)
            .unwrap()
            .set_len(260 * 1024 * 1024)
            .unwrap();
        let error = super::optimization::prepare(
            root.path(),
            250 * 1024 * 1024,
            std::time::Instant::now() + std::time::Duration::from_secs(10),
        )
        .err()
        .unwrap()
        .to_string();
        for detail in ["260 MiB", "250 MiB", "selected-capture", "never truncated"] {
            assert!(error.contains(detail), "{error}");
        }
        assert_eq!(std::fs::metadata(log).unwrap().len(), 260 * 1024 * 1024);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_mid_upload_reports_unknown_remote_state() -> Result<()> {
        use crate::diagnostics::{
            journal::{
                ArchiveState, JournalOptions, JournalReader, JournalWriter, OperationMetadata,
                record_worker_cancellation,
            },
            record::*,
        };
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };
        let root = tempfile::tempdir()?;
        let metadata = OperationMetadata::new(OperationKind::Deployment, None);
        let mut writer =
            JournalWriter::create(root.path(), metadata.clone(), JournalOptions::default())?;
        writer.write(&DiagnosticRecordV1::new(
            metadata.operation_id.clone(),
            OperationKind::Deployment,
            Stage::ApiUploads,
            DiagnosticEvent::Started,
        ))?;
        writer.flush()?;
        drop(writer);
        let (received, request_received) = tokio::sync::oneshot::channel();
        let received = Mutex::new(Some(received));
        let (release, gate) = std::sync::mpsc::channel();
        let site = crate::test::browser_tests::Site::new(move |request| {
            if let Some(received) = received.lock().unwrap().take() {
                let _ = received.send(());
            }
            let _ = gate.recv();
            let _ = request.respond(tiny_http::Response::empty(200));
        })?;
        let published = Arc::new(AtomicBool::new(false));
        let confirmation = published.clone();
        let url = site.origin.clone();
        let task = tokio::spawn(async move {
            reqwest::Client::new()
                .put(url)
                .body("synthetic upload")
                .send()
                .await?;
            confirmation.store(true, Ordering::Release);
            crate::progress::emit(crate::progress::Event::Deployed {
                target: "fixture".into(),
                url: Some("https://reports.example.test/".into()),
                fingerprint: None,
                input_fingerprint: None,
                unchanged: false,
                suites: None,
            });
            Ok::<_, anyhow::Error>(())
        });
        request_received.await?;
        task.abort();
        let cancelled = task.await;
        release.send(())?;
        drop(site);
        assert!(cancelled.is_err_and(|error| error.is_cancelled()));
        assert!(!published.load(Ordering::Acquire));
        record_worker_cancellation(root.path(), &metadata.operation_id)?;
        record_worker_cancellation(root.path(), &metadata.operation_id)?;
        let reader = JournalReader::open(root.path(), &metadata.operation_id)?;
        assert_eq!(reader.archive_state(), ArchiveState::Partial);
        let mut outcomes = Vec::new();
        reader.visit(|record| {
            if record.event_name == DiagnosticEvent::Cancelled {
                outcomes.push(record.outcome);
            }
        })?;
        assert_eq!(outcomes, vec![Some(Outcome::CancelledRemoteUnknown)]);
        Ok(())
    }

    #[tokio::test]
    async fn staging_failure_identifies_stage_and_artifact() {
        use crate::diagnostics::{journal::JournalReader, record::*, runtime};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("synthetic-token-source");
        fs::create_dir(&source).unwrap();
        let operation =
            runtime::Operation::start(root.path(), OperationKind::Deployment, None, None, None);
        let result = runtime::scope(Some(operation.context()), async {
            runtime::event(
                Stage::Preflight,
                DiagnosticEvent::Started,
                Severity::Info,
                [
                    (Field::Provider, FieldValue::Kind(SafeValue::Azure)),
                    (Field::CredentialPresent, FieldValue::Boolean(true)),
                ],
            );
            super::stage_reports(&source, &["shop".into()], &root.path().join("staged"))
        })
        .await;
        assert!(result.is_err());
        let reference = operation.finish(Outcome::Failure).await;
        let mut failed = false;
        JournalReader::open(root.path(), &reference.operation_id)
            .unwrap()
            .visit(|record| {
                assert!(
                    !serde_json::to_string(&record)
                        .unwrap()
                        .contains("synthetic-token")
                );
                if record.stage == Stage::SnapshotStaging
                    && record.outcome == Some(Outcome::Failure)
                {
                    failed = true;
                    assert_eq!(
                        record.fields.get(&Field::Artifact),
                        Some(&FieldValue::Identity(SafeIdentity::fingerprint(
                            "index.html"
                        )))
                    );
                }
            })
            .unwrap();
        assert!(failed);
    }

    use super::*;

    #[test]
    fn site_deployed_properties_describe_confirmed_publication() {
        for (provider, label) in [
            (DeploymentProvider::AzureSwa, "azureSwa"),
            (DeploymentProvider::Netlify, "netlify"),
            (DeploymentProvider::CloudflareWorkers, "cloudflareWorkers"),
        ] {
            let result = Ok(Some("https://reports.example.com/".into()));
            let properties = site_deployed_properties(
                provider,
                &result,
                DeploymentStats {
                    file_count: 3,
                    original_bytes: 8192,
                    total_bytes: 4096,
                    state_dropped: false,
                },
                std::time::Duration::from_millis(1234),
            )
            .unwrap();
            assert_eq!(
                properties,
                serde_json::json!({
                    "url": "https://reports.example.com/",
                    "current_url": "https://reports.example.com/",
                    "provider": label,
                    "file_count": 3,
                    "original_bytes": 8192,
                    "total_bytes": 4096,
                    "duration_ms": 1234,
                })
            );
            assert!(matches!(
                result,
                Ok(Some(url)) if url == "https://reports.example.com/"
            ));
        }
    }

    #[tokio::test]
    async fn preparation_phases_and_resizing_record_readable_stage_diagnostics() -> Result<()> {
        use crate::diagnostics::{
            journal::{ArchiveState, JournalReader},
            record::*,
            runtime,
        };
        let source = optimization::tests::report_fixture()?;
        let report =
            super::read_report_configuration(&source.path().join("one/html_report/config.js"))?;
        std::fs::write(
            source.path().join("one/bitmaps_test/run/report.json"),
            serde_json::to_vec(&report)?,
        )?;
        let suites = vec!["one".into()];
        let staged = tempfile::tempdir()?;
        let staged_report = staged.path().join("report");
        let operation =
            runtime::Operation::start(source.path(), OperationKind::Deployment, None, None, None);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        runtime::scope(Some(operation.context()), async {
            runtime::stage(Stage::SnapshotStaging, [], async {
                crate::snapshot::generate_site(
                    source.path(),
                    &suites,
                    &crate::cli::args::SnapshotArgs {
                        test_suite: suites.clone(),
                        title: None,
                        heading: None,
                        subheading: None,
                    },
                    &crate::config::WorkspaceConfig::default(),
                )
            })
            .await?;
            super::stage_reports(source.path(), &suites, &staged_report)?;
            let original = optimization::prepare(&staged_report, 500 * 1024 * 1024, deadline)?
                .original_bytes();
            let prepared = optimization::prepare(&staged_report, original * 85 / 100, deadline)?;
            assert_eq!(prepared.original_bytes(), original);
            super::with_heartbeat("Test deployment", async {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                Ok::<_, anyhow::Error>(())
            })
            .await
        })
        .await?;
        let reference = operation.finish(Outcome::Success).await;
        assert_eq!(reference.state, ArchiveState::Complete);
        let mut artifacts = Vec::new();
        let mut stages = Vec::new();
        JournalReader::open(source.path(), &reference.operation_id)?.visit(|record| {
            stages.push(record.stage);
            if let Some(FieldValue::Identity(identity)) = record.fields.get(&Field::Artifact) {
                artifacts.push((record.stage, identity.as_str().to_owned()));
            }
        })?;
        assert!(stages.contains(&Stage::SnapshotStaging));
        assert!(stages.contains(&Stage::PackagingOptimization));
        for expected in [
            "Preparing report snapshot for 1 suite",
            "Loading snapshot suite 1/1: one",
            "Preparing 6 capture preview images",
            "Prepared ",
            "Reused 0 cached and resized 6 capture preview images",
            "Publishing ",
            "Validating snapshot manifest and asset versions for 1 selected suite",
            "Staging ",
            "Verifying ",
            "Staging complete:",
            "Classifying report images by comparison role for optimization",
            "Temporary optimization copy:",
            "Optimizing ",
            "Refreshing snapshot cache versions for optimized images",
            "Temporary deployment report:",
        ] {
            assert!(
                artifacts.iter().any(|(_, text)| text.contains(expected)),
                "Missing expected preparation log '{expected}' in {artifacts:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn site_deployed_properties_skip_failures_private_sites_and_unsafe_urls() {
        let mut results = vec![Err(anyhow::anyhow!("synthetic failure")), Ok(None)];
        results.extend(
            [
                "",
                "not a URL",
                "http://reports.example.com/",
                "https://user:synthetic-secret@reports.example.com/",
                "https://reports.example.com/?token=synthetic-secret",
                "https://reports.example.com/#synthetic-secret",
            ]
            .into_iter()
            .map(|url| Ok(Some(url.into()))),
        );
        for result in results {
            assert!(
                site_deployed_properties(
                    DeploymentProvider::CloudflareWorkers,
                    &result,
                    DeploymentStats::default(),
                    std::time::Duration::ZERO,
                )
                .is_none()
            );
        }
    }

    #[tokio::test]
    async fn heartbeat_reports_waiting_and_preserves_results() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let mut sender = Some(sender);
        let mut updates = 0;
        let output = monitor(receiver, std::time::Duration::from_millis(1), |_| {
            updates += 1;
            if let Some(sender) = sender.take() {
                sender.send("finished").unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(output, "finished");
        assert!(updates >= 1);
        let error =
            with_heartbeat("Test deployment", async { Err::<(), _>("provider detail") }).await;
        assert_eq!(error, Err("provider detail"));
    }

    #[test]
    fn deployment_requires_nonempty_selection_and_explicit_subset_confirmation() {
        let mut job = DeploymentJob {
            target: DeployTarget {
                id: "qa".into(),
                name: "QA".into(),
                published_url: None,
                credential_id: None,
                named_token_id: None,
                environment: String::new(),
                provider: DeploymentProvider::AzureSwa,
                site_id: String::new(),
                cloudflare: None,
            },
            suite_ids: Vec::new(),
            catalog_ids: vec!["one".into(), "two".into()],
            partial_confirmed: false,
            force: false,
            mode: crate::desktop::deployment::DeploymentMode::Replace,
            remove_ids: Vec::new(),
        };
        assert!(job.validate().is_err());
        job.suite_ids = vec!["one".into()];
        assert!(job.validate().is_err());
        job.partial_confirmed = true;
        assert!(job.validate().is_ok());
        job.suite_ids.push("two".into());
        job.partial_confirmed = false;
        assert!(job.validate().is_ok());
        job.suite_ids.push("outside".into());
        assert!(job.validate().is_err());
    }

    #[test]
    fn target_settings_are_workspace_scoped_and_secret_free() {
        let directory = tempfile::tempdir().unwrap();
        let target = DeployTarget {
            id: "one".into(),
            name: "QA reports".into(),
            published_url: None,
            credential_id: None,
            named_token_id: None,
            environment: String::new(),
            provider: DeploymentProvider::AzureSwa,
            site_id: String::new(),
            cloudflare: None,
        };
        let settings = DeployTargets {
            targets: vec![target.clone()],
            selected: Some(target.id.clone()),
            ..Default::default()
        };
        let path = directory.path().join("targets.json");
        settings.save(&path).unwrap();
        assert_eq!(
            DeployTargets::load(&path).unwrap().targets,
            vec![target.clone()]
        );
        let mut invalid = settings;
        invalid.targets.push(target);
        assert!(invalid.save(&path).is_err());
        assert_ne!(
            DeployTargets::path(directory.path()).unwrap(),
            DeployTargets::path(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    fn target_settings_permission_errors_identify_the_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("targets.json");
        fs::create_dir(&path)?;
        let error = DeployTargets::load(&path)
            .err()
            .expect("A directory is not settings");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(5)
        );
        let message = format!("{error:#}");
        assert!(
            message.contains("Cannot read deployment target settings"),
            "{message}"
        );
        assert!(message.contains(&path.display().to_string()), "{message}");
        fs::remove_dir(&path)?;

        let settings = DeployTargets::default();
        settings.save(&path)?;
        let before = fs::read(&path)?;
        let permissions = fs::metadata(&path)?.permissions();
        let mut read_only = permissions.clone();
        read_only.set_readonly(true);
        fs::set_permissions(&path, read_only)?;
        let result = settings.save(&path);
        fs::set_permissions(&path, permissions)?;

        let error = result.expect_err("Read-only settings must not be replaced");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(5)
        );
        assert_eq!(fs::read(&path)?, before);
        let message = format!("{error:#}");
        assert!(
            message.contains("Cannot save deployment target settings"),
            "{message}"
        );
        assert!(message.contains(&path.display().to_string()), "{message}");
        Ok(())
    }

    #[test]
    fn target_settings_directory_errors_identify_the_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let blocked = directory.path().join("deployments");
        fs::write(&blocked, "existing file")?;
        let error = DeployTargets::default()
            .save(&blocked.join("targets.json"))
            .expect_err("A file cannot contain the settings directory");
        let message = format!("{error:#}");
        assert!(
            message.contains("Cannot create deployment settings directory"),
            "{message}"
        );
        assert!(
            message.contains(&blocked.display().to_string()),
            "{message}"
        );
        assert!(error.downcast_ref::<std::io::Error>().is_some());
        assert_eq!(fs::read(&blocked)?, b"existing file");
        Ok(())
    }

    #[test]
    fn published_urls_round_trip_with_each_target() {
        let directory = tempfile::tempdir().unwrap();
        let input = serde_json::json!({
            "targets": [
                {
                    "id": "azure", "name": "Azure",
                    "publishedUrl": "https://qa.azurestaticapps.net/"
                },
                {
                    "id": "netlify", "name": "Netlify", "provider": "netlify",
                    "siteId": "12345678-1234-1234-1234-123456789abc",
                    "publishedUrl": "https://qa.netlify.app/"
                },
                {
                    "id": "cloudflare", "name": "Cloudflare", "provider": "cloudflareWorkers",
                    "cloudflare": { "accountId": "a".repeat(32), "workerName": "qa" },
                    "publishedUrl": "https://qa.example.workers.dev/"
                }
            ],
            "selected": "netlify"
        });
        let settings: DeployTargets = serde_json::from_value(input.clone()).unwrap();
        let path = directory.path().join("targets.json");
        settings.save(&path).unwrap();
        let reloaded = serde_json::to_value(DeployTargets::load(&path).unwrap()).unwrap();
        assert_eq!(reloaded["selected"], input["selected"]);
        for (saved, expected) in reloaded["targets"]
            .as_array()
            .unwrap()
            .iter()
            .zip(input["targets"].as_array().unwrap())
        {
            assert_eq!(saved["id"], expected["id"]);
            assert_eq!(saved["publishedUrl"], expected["publishedUrl"]);
        }
    }

    #[test]
    fn published_report_sites_follow_latest_membership_and_reload() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("targets.json");
        let settings: DeployTargets = serde_json::from_value(serde_json::json!({
            "targets": [{"id": "first", "name": "First"}, {"id": "second", "name": "Second"}],
            "selected": "first"
        }))?;
        settings.save(&path)?;
        let suites = |ids: &[&str]| ids.iter().map(|id| (*id).to_string()).collect::<Vec<_>>();
        assert!(settings.latest_report_url(&suites(&["alloy"])).is_none());
        DeployTargets::record_report_publication(
            &path,
            &settings.targets[0],
            Some("https://first.example.com/"),
            &suites(&["alloy", "dark"]),
        )?;
        DeployTargets::record_report_publication(
            &path,
            &settings.targets[1],
            Some("https://second.example.com/"),
            &suites(&["alloy", "light"]),
        )?;
        let loaded = DeployTargets::load(&path)?;
        assert_eq!(loaded.selected.as_deref(), Some("first"));
        assert_eq!(
            loaded.latest_report_url(&suites(&["alloy"])).as_deref(),
            Some("https://second.example.com/suites/alloy")
        );
        assert_eq!(
            loaded.latest_report_url(&suites(&["dark"])).as_deref(),
            Some("https://first.example.com/suites/dark")
        );
        assert_eq!(
            loaded
                .latest_report_url(&suites(&["alloy", "dark"]))
                .as_deref(),
            Some("https://first.example.com/")
        );
        assert!(
            loaded
                .latest_report_url(&suites(&["dark", "light"]))
                .is_none()
        );
        assert!(loaded.latest_report_url(&[]).is_none());

        let updated = DeployTargets::record_report_publication(
            &path,
            &settings.targets[1],
            Some("https://second.example.com/"),
            &suites(&["light"]),
        )?;
        assert_eq!(
            updated.latest_report_url(&suites(&["alloy"])).as_deref(),
            Some("https://first.example.com/suites/alloy")
        );
        assert_eq!(updated.report_publications.len(), 2);
        let replaced = DeployTargets::record_report_publication(
            &path,
            &settings.targets[1],
            Some("https://FIRST.example.com"),
            &suites(&["light"]),
        )?;
        assert!(replaced.latest_report_url(&suites(&["alloy"])).is_none());
        assert_eq!(replaced.report_publications.len(), 1);
        let landing = DeployTargets::record_publication(
            &path,
            &settings.targets[1],
            Some("https://first.example.com/"),
        )?;
        assert!(landing.latest_report_url(&suites(&["light"])).is_none());
        Ok(())
    }

    #[test]
    fn published_report_sites_reject_invalid_membership_without_writes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("targets.json");
        let settings: DeployTargets = serde_json::from_value(serde_json::json!({
            "targets": [{"id": "qa", "name": "QA"}]
        }))?;
        settings.save(&path)?;
        let before = fs::read(&path)?;
        for suites in [
            vec![],
            vec!["../escape".into()],
            vec!["alloy".into(), "alloy".into()],
        ] {
            assert!(
                DeployTargets::record_report_publication(
                    &path,
                    &settings.targets[0],
                    Some("https://qa.example.com/"),
                    &suites,
                )
                .is_err()
            );
            assert_eq!(fs::read(&path)?, before);
        }
        let saved = DeployTargets::record_report_publication(
            &path,
            &settings.targets[0],
            Some("https://qa.example.com/reports/"),
            &["suite #1".into()],
        )?;
        assert_eq!(
            saved.latest_report_url(&["suite #1".into()]).as_deref(),
            Some("https://qa.example.com/reports/suites/suite%20%231")
        );
        assert!(saved.latest_report_url(&["../escape".into()]).is_none());
        Ok(())
    }

    #[test]
    fn published_urls_update_only_the_matching_target() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("targets.json");
        let mut settings: DeployTargets = serde_json::from_value(serde_json::json!({
            "targets": [
                {"id": "one", "name": "One", "publishedUrl": "https://old.example.com/"},
                {"id": "two", "name": "Two", "publishedUrl": "https://other.example.com/"}
            ],
            "selected": "one"
        }))?;
        let deployed = settings.targets[0].clone();
        settings.targets[0].name = "Renamed while publishing".into();
        settings.selected = Some("two".into());
        settings.save(&path)?;
        let updated =
            DeployTargets::record_published_url(&path, &deployed, "https://new.example.com/")?;
        assert_eq!(updated.targets[0].name, settings.targets[0].name);
        assert_eq!(
            updated.targets[0].published_url.as_deref(),
            Some("https://new.example.com/")
        );
        assert_eq!(updated.targets[1], settings.targets[1]);
        assert_eq!(updated.selected, settings.selected);
        assert_eq!(DeployTargets::load(&path)?.targets, updated.targets);
        Ok(())
    }

    #[test]
    fn published_urls_reject_unsafe_links_without_replacing_saved_settings() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("targets.json");
        let settings: DeployTargets = serde_json::from_value(serde_json::json!({
            "targets": [{"id": "qa", "name": "QA", "publishedUrl": "https://qa.example.com/"}]
        }))?;
        settings.save(&path)?;
        let before = fs::read(&path)?;
        for url in [
            "".to_string(),
            "javascript:alert(1)".into(),
            "file:///C:/Windows/notepad.exe".into(),
            "http://qa.example.com/".into(),
            "https://user:synthetic-secret@qa.example.com/".into(),
            "https://qa.example.com/?token=synthetic-secret".into(),
            "https://qa.example.com/#synthetic-secret".into(),
            "https://qa.example.com/\n".into(),
            "https://qa.example.com/\\other".into(),
            format!("https://qa.example.com/{}", "a".repeat(4096)),
        ] {
            let error = DeployTargets::record_published_url(&path, &settings.targets[0], &url)
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("synthetic-secret"));
            let mut invalid = settings.clone();
            invalid.targets[0].published_url = Some(url);
            assert!(invalid.save(&path).is_err());
            assert_eq!(fs::read(&path)?, before);
        }
        Ok(())
    }

    #[test]
    fn published_urls_do_not_restore_removed_or_changed_destinations() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("targets.json");
        for input in [
            serde_json::json!({"id": "qa", "name": "QA"}),
            serde_json::json!({"id": "qa", "name": "QA", "provider": "netlify", "siteId": uuid::Uuid::new_v4().to_string()}),
            serde_json::json!({"id": "qa", "name": "QA", "provider": "cloudflareWorkers", "cloudflare": {"accountId": "a".repeat(32), "workerName": "reports"}}),
        ] {
            let target: DeployTarget = serde_json::from_value(input)?;
            let mut changed = target.clone();
            match changed.provider {
                DeploymentProvider::AzureSwa => changed.environment = "preview".into(),
                DeploymentProvider::Netlify => changed.site_id = uuid::Uuid::new_v4().to_string(),
                DeploymentProvider::CloudflareWorkers => {
                    changed.cloudflare.as_mut().unwrap().worker_name = "other".into()
                }
            }
            for targets in [vec![changed], Vec::new()] {
                DeployTargets {
                    targets,
                    selected: None,
                    ..Default::default()
                }
                .save(&path)?;
                let before = fs::read(&path)?;
                assert!(
                    DeployTargets::record_published_url(&path, &target, "https://qa.example.com/")
                        .is_err()
                );
                assert_eq!(fs::read(&path)?, before);
            }
        }
        Ok(())
    }

    #[test]
    fn published_urls_survive_edits_only_for_the_same_destination() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("targets.json");
        let token = format!("{}-{}000", "a".repeat(64), "b".repeat(36));
        for input in [
            serde_json::json!({"id": "qa", "name": "QA"}),
            serde_json::json!({"id": "qa", "name": "QA", "provider": "netlify", "siteId": uuid::Uuid::new_v4().to_string()}),
            serde_json::json!({"id": "qa", "name": "QA", "provider": "cloudflareWorkers", "cloudflare": {"accountId": "a".repeat(32), "workerName": "reports"}}),
        ] {
            let mut target: DeployTarget = serde_json::from_value(input)?;
            let mut settings = DeployTargets::default();
            settings.save_target(root.path(), &path, target.clone(), &token)?;
            settings = DeployTargets::record_report_publication(
                &path,
                &settings.targets[0],
                Some("https://qa.example.com/"),
                &["alloy".into()],
            )?;
            target.name = "Renamed".into();
            settings.save_target(root.path(), &path, target.clone(), "")?;
            assert_eq!(
                settings.targets[0].published_url.as_deref(),
                Some("https://qa.example.com/")
            );
            assert_eq!(
                settings.latest_report_url(&["alloy".into()]).as_deref(),
                Some("https://qa.example.com/suites/alloy")
            );
            settings.save_target(root.path(), &path, target.clone(), &token)?;
            assert_eq!(
                settings.targets[0].published_url.is_some(),
                target.provider != DeploymentProvider::AzureSwa
            );
            assert_eq!(
                settings.latest_report_url(&["alloy".into()]).is_some(),
                target.provider != DeploymentProvider::AzureSwa
            );
            settings = DeployTargets::record_report_publication(
                &path,
                &settings.targets[0],
                Some("https://qa.example.com/"),
                &["alloy".into()],
            )?;
            match target.provider {
                DeploymentProvider::AzureSwa => target.environment = "preview".into(),
                DeploymentProvider::Netlify => target.site_id = uuid::Uuid::new_v4().to_string(),
                DeploymentProvider::CloudflareWorkers => {
                    target.cloudflare.as_mut().unwrap().worker_name = "other".into()
                }
            }
            settings.save_target(root.path(), &path, target, "")?;
            assert!(settings.targets[0].published_url.is_none());
            assert!(settings.latest_report_url(&["alloy".into()]).is_none());
            assert!(settings.report_publications.is_empty());
            settings = DeployTargets::record_report_publication(
                &path,
                &settings.targets[0],
                Some("https://new.example.com/"),
                &["alloy".into()],
            )?;
            settings.remove_target(root.path(), &path, "qa")?;
            assert!(settings.report_publications.is_empty());
        }
        Ok(())
    }

    #[test]
    fn legacy_targets_keep_metadata_but_require_a_secure_token() {
        let target: DeployTarget = serde_json::from_value(serde_json::json!({
            "id": "qa", "name": "QA", "tokenEnv": "QA_SWA_TOKEN", "environment": "preview"
        }))
        .unwrap();
        assert_eq!(target.environment, "preview");
        assert_eq!(target.provider, DeploymentProvider::AzureSwa);
        assert!(target.site_id.is_empty());
        assert!(target.credential_id.is_none());
        assert!(target.published_url.is_none());
        assert!(!serde_json::to_string(&target).unwrap().contains("tokenEnv"));
        assert!(
            target
                .token(Path::new("."))
                .unwrap_err()
                .to_string()
                .contains("enter its deployment token")
        );
    }

    #[test]
    fn target_tokens_are_securely_saved_preserved_replaced_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let path = config.path().join("targets.json");
        let target = DeployTarget {
            id: "qa".into(),
            name: "QA reports".into(),
            published_url: None,
            credential_id: None,
            named_token_id: None,
            environment: String::new(),
            provider: DeploymentProvider::AzureSwa,
            site_id: String::new(),
            cloudflare: None,
        };
        let token = format!("{}-{}000", "a".repeat(64), "b".repeat(36));
        let replacement = format!("{}-{}000", "c".repeat(64), "d".repeat(36));
        let mut settings = DeployTargets::default();
        assert!(
            settings
                .save_target(root.path(), &path, target.clone(), "")
                .is_err()
        );
        assert!(
            settings
                .save_target(root.path(), &path, target.clone(), "invalid")
                .is_err()
        );
        assert!(!path.exists());
        settings
            .save_target(root.path(), &path, target, &token)
            .unwrap();
        let saved = DeployTargets::load(&path).unwrap().targets.remove(0);
        let credential = credentials::DeploymentCredential::new(
            root.path(),
            saved.credential_id.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(saved.token(root.path()).unwrap().as_str(), token);
        assert!(saved.token(config.path()).is_err());
        assert!(!fs::read_to_string(&path).unwrap().contains(&token));
        assert!(!format!("{saved:?}").contains(&token));
        let mut aliased = settings.clone();
        let mut duplicate = saved.clone();
        duplicate.id = "other".into();
        duplicate.name = "Other target".into();
        aliased.targets.push(duplicate);
        assert!(aliased.save(&path).is_err());
        let mut renamed = saved.clone();
        renamed.name = "Renamed QA".into();
        settings
            .save_target(root.path(), &path, renamed.clone(), "")
            .unwrap();
        assert_eq!(settings.targets[0].name, "Renamed QA");
        assert_eq!(saved.token(root.path()).unwrap().as_str(), token);
        assert!(
            settings
                .save_target(root.path(), config.path(), renamed.clone(), &replacement)
                .is_err()
        );
        assert_eq!(saved.token(root.path()).unwrap().as_str(), token);
        settings
            .save_target(root.path(), &path, renamed, &replacement)
            .unwrap();
        assert_eq!(saved.token(root.path()).unwrap().as_str(), replacement);
        assert!(
            settings
                .remove_target(root.path(), config.path(), "qa")
                .is_err()
        );
        assert_eq!(saved.token(root.path()).unwrap().as_str(), replacement);
        settings.remove_target(root.path(), &path, "qa").unwrap();
        assert!(DeployTargets::load(&path).unwrap().targets.is_empty());
        assert!(credential.read_optional().unwrap().is_none());
    }

    #[test]
    fn netlify_targets_validate_provider_fields_and_require_new_credentials_on_switch() {
        let root = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let path = config.path().join("targets.json");
        let mut target: DeployTarget = serde_json::from_value(serde_json::json!({
            "id": "qa", "name": "QA", "provider": "netlify", "siteId": uuid::Uuid::new_v4().to_string()
        })).unwrap();
        assert!(target.validate().is_ok());
        let encoded = serde_json::to_string(&target).unwrap();
        assert_eq!(
            serde_json::from_str::<DeployTarget>(&encoded).unwrap(),
            target
        );
        for invalid in ["", "../other", "https://example.com", "invalid"] {
            let mut invalid_target = target.clone();
            invalid_target.site_id = invalid.into();
            assert!(invalid_target.validate().is_err());
        }
        target.environment = "preview".into();
        assert!(target.validate().is_err());
        target.environment.clear();
        let mut settings = DeployTargets::default();
        settings
            .save_target(
                root.path(),
                &path,
                target.clone(),
                "synthetic-netlify-token",
            )
            .unwrap();
        assert_eq!(
            settings.targets[0].token(root.path()).unwrap().as_str(),
            "synthetic-netlify-token"
        );
        target.provider = DeploymentProvider::AzureSwa;
        target.site_id.clear();
        assert!(
            settings
                .save_target(root.path(), &path, target.clone(), "")
                .is_err()
        );
        assert_eq!(settings.targets[0].provider, DeploymentProvider::Netlify);
        let previous_target = settings.targets[0].clone();
        let azure_token = format!("{}-{}000", "a".repeat(64), "b".repeat(36));
        assert!(
            settings
                .save_target(root.path(), config.path(), target.clone(), &azure_token)
                .is_err()
        );
        assert_eq!(
            previous_target.token(root.path()).unwrap().as_str(),
            "synthetic-netlify-token"
        );
        settings
            .save_target(root.path(), &path, target.clone(), &azure_token)
            .unwrap();
        assert_ne!(
            previous_target.credential_id,
            settings.targets[0].credential_id
        );
        assert!(previous_target.token(root.path()).is_err());
        target.provider = DeploymentProvider::Netlify;
        target.site_id = uuid::Uuid::new_v4().to_string();
        assert!(
            settings
                .save_target(root.path(), &path, target, "")
                .is_err()
        );
        assert_eq!(
            settings.targets[0].token(root.path()).unwrap().as_str(),
            azure_token
        );
        settings.remove_target(root.path(), &path, "qa").unwrap();
        for invalid in [
            "",
            "token\nAuthorization: extra",
            "token with spaces",
            "\u{00e9}",
        ] {
            assert!(DeploymentProvider::Netlify.validate_token(invalid).is_err());
        }
        assert!(
            serde_json::from_value::<DeployTarget>(serde_json::json!({
                "id": "qa", "name": "QA", "provider": "unknown"
            }))
            .is_err()
        );
    }

    #[test]
    fn named_tokens_reuse_across_targets_and_survive_target_removal() -> Result<()> {
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let config = tempfile::tempdir()?;
        let catalog_path = config.path().join("tokens.json");
        let mut catalog = tokens::TokenCatalog::default();
        let entry = tokens::NamedToken {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Shared Netlify".into(),
            provider: DeploymentProvider::Netlify,
            account_id: String::new(),
        };
        catalog.save_token(&catalog_path, entry.clone(), "synthetic-shared")?;
        let mut target: DeployTarget = serde_json::from_value(serde_json::json!({
            "id": "qa", "name": "QA", "provider": "netlify", "siteId": uuid::Uuid::new_v4().to_string(),
            "namedTokenId": entry.id
        }))?;
        for root in [first.path(), second.path()] {
            let mut settings = DeployTargets::default();
            let path = root.join("targets.json");
            settings.save_linked_target(root, &path, target.clone(), &catalog)?;
            settings = DeployTargets::record_published_url(
                &path,
                &settings.targets[0],
                "https://qa.netlify.app/",
            )?;
            settings.save_linked_target(root, &path, target.clone(), &catalog)?;
            assert_eq!(
                settings.targets[0].published_url.as_deref(),
                Some("https://qa.netlify.app/")
            );
            assert_eq!(
                settings.targets[0].named_token(&catalog)?.as_str(),
                "synthetic-shared"
            );
            settings.remove_target(root, &path, &target.id)?;
            assert_eq!(entry.token()?.as_str(), "synthetic-shared");
        }
        let path = first.path().join("targets.json");
        let mut settings = DeployTargets::default();
        target.named_token_id = None;
        settings.save_target(first.path(), &path, target.clone(), "synthetic-local")?;
        let local = settings.targets[0].clone();
        target.named_token_id = Some(entry.id.clone());
        assert!(
            settings
                .save_linked_target(first.path(), first.path(), target.clone(), &catalog)
                .is_err()
        );
        assert_eq!(local.token(first.path())?.as_str(), "synthetic-local");
        settings.save_linked_target(first.path(), &path, target.clone(), &catalog)?;
        assert!(local.token(first.path()).is_err());
        let mut invalid = target.clone();
        invalid.credential_id = Some(uuid::Uuid::new_v4().to_string());
        assert!(invalid.validate().is_err());
        invalid = target.clone();
        invalid.provider = DeploymentProvider::CloudflareWorkers;
        invalid.site_id.clear();
        invalid.cloudflare = Some(cloudflare::CloudflareTarget {
            account_id: "a".repeat(32),
            worker_name: "reports".into(),
            ..cloudflare::CloudflareTarget::default()
        });
        assert!(invalid.named_token(&catalog).is_err());
        catalog.remove(&catalog_path, &entry.id)?;
        assert!(target.named_token(&catalog).is_err());
        Ok(())
    }

    #[test]
    fn cloudflare_content_options_preserve_urls_and_private_publication_clears_them() -> Result<()>
    {
        let root = tempfile::tempdir()?;
        let path = root.path().join("targets.json");
        let target: DeployTarget = serde_json::from_value(serde_json::json!({
            "id":"cloudflare", "name":"Reports", "provider":"cloudflareWorkers", "publishedUrl":"https://reports.account.workers.dev/",
            "cloudflare":{"accountId":"0123456789abcdef0123456789abcdef", "workerName":"reports"}
        }))?;
        let mut alias = target.clone();
        alias.id = "alias".into();
        alias.name = "Same site".into();
        DeployTargets {
            targets: vec![target.clone(), alias.clone()],
            selected: Some(target.id.clone()),
            ..Default::default()
        }
        .save(&path)?;
        let published = DeployTargets::record_report_publication(
            &path,
            &target,
            target.published_url.as_deref(),
            &["alloy".into()],
        )?;
        assert!(published.latest_report_url(&["alloy".into()]).is_some());
        DeployTargets::record_report_publication(
            &path,
            &alias,
            target.published_url.as_deref(),
            &["alloy".into()],
        )?;
        let mut updated = target.clone();
        let settings = updated.cloudflare.as_mut().unwrap();
        settings.html_handling = "none".into();
        settings.not_found_handling = "404-page".into();
        settings.compatibility_date = "2026-09-28".into();
        settings.exclude = vec!["*.map".into()];
        assert!(target.same_destination(&updated));
        updated.cloudflare.as_mut().unwrap().workers_dev = Some(false);
        assert!(!target.same_destination(&updated));
        let saved =
            DeployTargets::record_report_publication(&path, &target, None, &["alloy".into()])?;
        assert!(saved.targets[0].published_url.is_none());
        assert!(saved.report_publications.is_empty());
        assert!(saved.latest_report_url(&["alloy".into()]).is_none());
        let mut azure = target.clone();
        azure.provider = DeploymentProvider::AzureSwa;
        azure.cloudflare = None;
        assert!(DeployTargets::record_publication(&path, &azure, None).is_err());
        Ok(())
    }

    #[test]
    fn cloudflare_targets_round_trip_and_keep_credentials_on_failed_switch() -> Result<()> {
        let root = tempfile::tempdir()?;
        let config = tempfile::tempdir()?;
        let path = config.path().join("targets.json");
        let target: DeployTarget = serde_json::from_value(serde_json::json!({
            "id": "qa", "name": "Cloudflare QA", "provider": "cloudflareWorkers",
            "cloudflare": {"accountId": "0123456789abcdef0123456789abcdef", "workerName": "reports"}
        }))?;
        target.validate()?;
        assert_eq!(
            serde_json::from_str::<DeployTarget>(&serde_json::to_string(&target)?)?,
            target
        );
        let mut invalid = target.clone();
        invalid.cloudflare = None;
        assert!(invalid.validate().is_err());
        invalid = target.clone();
        invalid.site_id = uuid::Uuid::new_v4().to_string();
        assert!(invalid.validate().is_err());
        invalid = target.clone();
        invalid.environment = "preview".into();
        assert!(invalid.validate().is_err());
        invalid = target.clone();
        invalid.provider = DeploymentProvider::AzureSwa;
        assert!(invalid.validate().is_err());
        let mut settings = DeployTargets::default();
        settings.save_target(
            root.path(),
            &path,
            target.clone(),
            "synthetic-cloudflare-token",
        )?;
        let saved = settings.targets[0].clone();
        assert_eq!(
            saved.token(root.path())?.as_str(),
            "synthetic-cloudflare-token"
        );
        assert!(!fs::read_to_string(&path)?.contains("synthetic-cloudflare-token"));
        settings.save_target(root.path(), &path, target.clone(), "")?;
        assert_eq!(settings.targets[0].credential_id, saved.credential_id);
        invalid.cloudflare = None;
        assert!(
            settings
                .save_target(root.path(), &path, invalid.clone(), "")
                .is_err()
        );
        let azure_token = format!("{}-{}000", "a".repeat(64), "b".repeat(36));
        assert!(
            settings
                .save_target(root.path(), config.path(), invalid.clone(), &azure_token)
                .is_err()
        );
        assert_eq!(
            saved.token(root.path())?.as_str(),
            "synthetic-cloudflare-token"
        );
        settings.save_target(root.path(), &path, invalid, &azure_token)?;
        assert!(saved.token(root.path()).is_err());
        assert!(
            settings
                .save_target(root.path(), &path, target, "")
                .is_err()
        );
        assert_eq!(settings.targets[0].provider, DeploymentProvider::AzureSwa);
        settings.remove_target(root.path(), &path, "qa")?;
        Ok(())
    }

    fn snapshot_fixture(output: &Path) -> BTreeSet<String> {
        for path in [
            "one/html_report",
            "one/bitmaps_reference",
            "one/bitmaps_test/older",
        ] {
            fs::create_dir_all(output.join(path)).unwrap();
        }
        let mut expected: BTreeSet<String> = crate::snapshot::FRONTEND_FILES
            .iter()
            .filter(|(name, _)| *name != "gitignore")
            .map(|(name, _)| (*name).to_owned())
            .collect();
        expected.insert("index.html".into());
        for (path, pixels) in [
            ("one/bitmaps_reference/home.png", "reference"),
            ("one/bitmaps_test/older/home.png", "test"),
            ("one/bitmaps_test/older/home_diff.png", "diff"),
        ] {
            let bytes = lodepng::encode32(&[pixels.as_bytes()[0], 40, 60, 255], 1, 1).unwrap();
            fs::write(output.join(path), &bytes).unwrap();
            expected.insert(format!(
                "_snapshot/images/one/{pixels}/{}",
                Path::new(path).file_name().unwrap().to_str().unwrap()
            ));
            expected.insert(format!(
                "_snapshot/images/one/{pixels}-preview/{}",
                Path::new(path).file_name().unwrap().to_str().unwrap()
            ));
            if pixels == "diff" {
                expected.insert("_snapshot/images/one/diff-thumbnail/home_diff.png".into());
            }
        }
        let report = serde_json::json!({ "id": "one", "tests": [{ "status": "fail", "pair": {
            "label": "Home", "test": "../bitmaps_test/older/home.png", "reference": "../bitmaps_reference/home.png", "diffImage": "../bitmaps_test/older/home_diff.png"
        }}] });
        fs::write(
            output.join("one/html_report/config.js"),
            format!("report({report});"),
        )
        .unwrap();
        fs::write(
            output.join("one/bitmaps_test/older/report.json"),
            report.to_string(),
        )
        .unwrap();
        crate::snapshot::generate_site(
            output,
            &["one".into()],
            &crate::cli::args::SnapshotArgs {
                test_suite: Vec::new(),
                title: None,
                heading: None,
                subheading: None,
            },
            &crate::config::WorkspaceConfig::default(),
        )
        .unwrap();
        expected.extend([
            "_snapshot/status.json".into(),
            "_snapshot/data/manifest.json".into(),
            "_snapshot/data/suites/one.json".into(),
        ]);
        expected
    }

    #[test]
    fn staging_rejects_missing_images_tampering_and_wrong_suite_selection() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        let expected = snapshot_fixture(&output);
        let image = expected
            .iter()
            .find(|path| path.starts_with("_snapshot/images/"))
            .unwrap();
        let original = fs::read(output.join(image)).unwrap();
        fs::remove_file(output.join(image)).unwrap();
        assert!(stage_reports(&output, &["one".into()], &root.path().join("missing")).is_err());
        fs::write(output.join(image), b"tampered").unwrap();
        assert!(stage_reports(&output, &["one".into()], &root.path().join("changed")).is_err());
        fs::write(output.join(image), original).unwrap();
        for selection in [
            vec![],
            vec!["../outside".into()],
            vec!["two".into()],
            vec!["one".into(), "one".into()],
        ] {
            assert!(stage_reports(&output, &selection, &root.path().join("wrong")).is_err());
        }
        assert!(!root.path().join("missing").exists());
    }

    fn regenerate(output: &Path, title: Option<&str>) {
        crate::snapshot::generate_site(
            output,
            &["one".into()],
            &crate::cli::args::SnapshotArgs {
                test_suite: Vec::new(),
                title: title.map(str::to_owned),
                heading: None,
                subheading: None,
            },
            &crate::config::WorkspaceConfig::default(),
        )
        .unwrap();
    }

    fn staged(output: &Path, destination: &Path) -> PathBuf {
        stage_reports(output, &["one".into()], destination).unwrap();
        destination.to_path_buf()
    }

    fn azure_target() -> DeployTarget {
        serde_json::from_value(serde_json::json!({ "id": "qa", "name": "QA" })).unwrap()
    }

    #[test]
    fn deployment_fingerprints_ignore_generation_time_and_follow_published_content() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        snapshot_fixture(&output);
        let suites = vec!["one".to_string()];
        let target = azure_target();
        let first = staged(&output, &root.path().join("first"));
        let fingerprint = deployment_fingerprint(&first, &suites, &target).unwrap();
        assert!(valid_fingerprint(&fingerprint));

        // generatedAt has second resolution; a later generation must still match.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        regenerate(&output, None);
        let second = staged(&output, &root.path().join("second"));
        let manifest = |root: &Path| fs::read(root.join("_snapshot/data/manifest.json")).unwrap();
        assert_ne!(manifest(&first), manifest(&second));
        assert_ne!(
            fs::read(first.join("index.html")).unwrap(),
            fs::read(second.join("index.html")).unwrap()
        );
        assert_eq!(
            deployment_fingerprint(&second, &suites, &target).unwrap(),
            fingerprint
        );

        // Names, saved URLs, and credential references do not change the published site.
        let mut renamed = target.clone();
        renamed.name = "Renamed".into();
        renamed.published_url = Some("https://reports.example.com/".into());
        renamed.credential_id = Some(uuid::Uuid::new_v4().to_string());
        assert_eq!(
            deployment_fingerprint(&second, &suites, &renamed).unwrap(),
            fingerprint
        );
        let mut preview = target.clone();
        preview.environment = "preview".into();
        let mut netlify = target.clone();
        netlify.provider = DeploymentProvider::Netlify;
        netlify.site_id = uuid::Uuid::new_v4().to_string();
        for changed in [preview, netlify] {
            assert_ne!(
                deployment_fingerprint(&second, &suites, &changed).unwrap(),
                fingerprint
            );
        }
        // Site-wide text and any report image change the published content.
        regenerate(&output, Some("Release candidate"));
        let titled = staged(&output, &root.path().join("titled"));
        assert_ne!(
            deployment_fingerprint(&titled, &suites, &target).unwrap(),
            fingerprint
        );
        fs::write(
            output.join("one/bitmaps_reference/home.png"),
            lodepng::encode32(&[1_u8, 2, 3, 255], 1, 1).unwrap(),
        )
        .unwrap();
        regenerate(&output, None);
        let approved = staged(&output, &root.path().join("approved"));
        assert_ne!(
            deployment_fingerprint(&approved, &suites, &target).unwrap(),
            fingerprint
        );
        // The fingerprint only describes a complete staged selection. Content verification
        // belongs to staging, which rejects tampered files before any fingerprint.
        assert!(deployment_fingerprint(&approved, &["two".into()], &target).is_err());
        fs::remove_file(approved.join("_snapshot/assets/app.js")).unwrap();
        assert!(deployment_fingerprint(&approved, &suites, &target).is_err());
    }

    #[test]
    fn lossless_estimates_save_a_tenth_of_the_png_bytes() {
        assert_eq!(expected_lossless_saving(1000), 100);
        assert_eq!(expected_lossless_saving(999), 99);
        assert_eq!(expected_lossless_saving(0), 0);
        // Large sites never overflow the arithmetic.
        assert_eq!(expected_lossless_saving(u64::MAX), u64::MAX / 10);
        assert_eq!(expected_reduction_saving(1000), 200);
        assert_eq!(expected_reduction_saving(u64::MAX), u64::MAX / 5);
    }

    #[test]
    fn manifest_summaries_keep_staged_and_kept_counts_apart() {
        let counts = ManifestCounts {
            staged_files: 150,
            staged_bytes: 93_902_315,
            unique: 130,
            kept_files: 593,
            kept_bytes: 2 * 1024 * 1024,
        };
        assert_eq!(
            (counts.files(), counts.bytes()),
            (743, 93_902_315 + 2 * 1024 * 1024)
        );
        assert_eq!(
            counts.describe("digests", "digest"),
            "743 files: 150 staged (89.55 MiB, 130 unique digests), 593 kept by digest (2.00 MiB)"
        );
        // Without kept files the summary reads as before.
        let staged = ManifestCounts {
            kept_files: 0,
            kept_bytes: 0,
            ..counts
        };
        assert_eq!(
            staged.describe("assets", "hash"),
            "150 files (89.55 MiB, 130 unique assets)"
        );
    }

    #[test]
    fn report_fingerprints_follow_confirmed_publications_and_destinations() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("targets.json");
        let target = azure_target();
        DeployTargets {
            targets: vec![target.clone()],
            selected: Some(target.id.clone()),
            ..Default::default()
        }
        .save(&path)?;
        let suites = vec!["alloy".to_string()];
        let first = "a".repeat(64);
        let inputs = "c".repeat(64);
        let url = "https://reports.example.com/";
        DeployTargets::record_deployed_report(
            &path,
            &target,
            Some(url),
            &suites,
            Some(&first),
            Some(&inputs),
        )?;
        let settings = DeployTargets::load(&path)?;
        assert_eq!(
            settings.unchanged_inputs(&target, &inputs),
            Some((url.to_owned(), first.clone()))
        );
        assert!(settings.unchanged_inputs(&target, &first).is_none());
        assert_eq!(settings.report_fingerprint(&target), Some(first.as_str()));
        assert_eq!(
            settings.unchanged_site_url(&target, &first).as_deref(),
            Some(url)
        );
        assert!(
            settings
                .unchanged_site_url(&target, &"b".repeat(64))
                .is_none()
        );
        // A destination edit that has not been saved yet no longer matches the record.
        let mut moved = target.clone();
        moved.environment = "preview".into();
        assert!(settings.report_fingerprint(&moved).is_none());
        assert!(settings.unchanged_site_url(&moved, &first).is_none());
        assert!(settings.unchanged_inputs(&moved, &inputs).is_none());

        // Unknown outcomes forget the content but keep Open Site membership.
        DeployTargets::forget_report_fingerprint(&path, &target)?;
        let forgotten = DeployTargets::load(&path)?;
        assert!(forgotten.report_fingerprint(&target).is_none());
        assert!(forgotten.unchanged_inputs(&target, &inputs).is_none());
        assert_eq!(
            forgotten.latest_report_url(&suites).as_deref(),
            Some("https://reports.example.com/suites/alloy")
        );
        let bytes = fs::read(&path)?;
        assert!(!String::from_utf8_lossy(&bytes).contains("siteFingerprint"));
        assert!(!String::from_utf8_lossy(&bytes).contains("inputFingerprint"));
        DeployTargets::forget_report_fingerprint(&path, &target)?;
        let mut elsewhere = target.clone();
        elsewhere.id = "missing".into();
        elsewhere.environment = "other".into();
        DeployTargets::forget_report_fingerprint(&path, &elsewhere)?;
        assert_eq!(fs::read(&path)?, bytes);

        // Legacy membership records have no content identity.
        DeployTargets::record_report_publication(&path, &target, Some(url), &suites)?;
        assert!(
            DeployTargets::load(&path)?
                .report_fingerprint(&target)
                .is_none()
        );
        let legacy: DeployTargets = serde_json::from_value(serde_json::json!({
            "targets": [{ "id": "qa", "name": "QA", "publishedUrl": url }],
            "reportPublications": [{ "targetId": "qa", "suiteIds": ["alloy"] }]
        }))?;
        assert!(legacy.report_fingerprint(&target).is_none());

        // Inputs are only recorded for unchanged content they are known to generate.
        DeployTargets::record_deployed_report(
            &path,
            &target,
            Some(url),
            &suites,
            Some(&first),
            None,
        )?;
        DeployTargets::record_report_inputs(&path, &target.id, &"b".repeat(64), &inputs)?;
        assert!(
            DeployTargets::load(&path)?
                .unchanged_inputs(&target, &inputs)
                .is_none()
        );
        DeployTargets::record_report_inputs(&path, &target.id, &first, &inputs)?;
        assert!(
            DeployTargets::load(&path)?
                .unchanged_inputs(&target, &inputs)
                .is_some()
        );
        // Invalid identities are rejected without replacing the saved record.
        let saved = fs::read(&path)?;
        assert!(
            DeployTargets::record_deployed_report(
                &path,
                &target,
                Some(url),
                &suites,
                None,
                Some(&inputs)
            )
            .is_err()
        );
        assert_eq!(fs::read(&path)?, saved);
        for invalid in [
            "",
            "A".repeat(64).as_str(),
            "a".repeat(63).as_str(),
            "g".repeat(64).as_str(),
        ] {
            assert!(
                DeployTargets::record_deployed_report(
                    &path,
                    &target,
                    Some(url),
                    &suites,
                    Some(invalid),
                    None
                )
                .is_err()
            );
            assert!(
                DeployTargets::record_deployed_report(
                    &path,
                    &target,
                    Some(url),
                    &suites,
                    Some(&first),
                    Some(invalid)
                )
                .is_err()
            );
            assert_eq!(fs::read(&path)?, saved);
        }
        // Publishing a landing page to the same target replaces the report site entirely.
        DeployTargets::record_published_url(&path, &target, url)?;
        assert!(
            DeployTargets::load(&path)?
                .report_fingerprint(&target)
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn unchanged_deployments_skip_only_matching_unforced_targets() -> Result<()> {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        snapshot_fixture(&output);
        let staging = staged(&output, &root.path().join("staged"));
        let path = root.path().join("targets.json");
        let target = azure_target();
        DeployTargets {
            targets: vec![target.clone()],
            selected: Some(target.id.clone()),
            ..Default::default()
        }
        .save(&path)?;
        let job = DeploymentJob {
            target: target.clone(),
            suite_ids: vec!["one".into()],
            catalog_ids: vec!["one".into()],
            partial_confirmed: false,
            force: false,
            mode: crate::desktop::deployment::DeploymentMode::Replace,
            remove_ids: Vec::new(),
        };
        // Nothing recorded yet, or no settings at all: publish.
        let (fingerprint, unchanged) = unchanged_deployment(&staging, &job, Some(&path), None)?;
        assert!(unchanged.is_none());
        assert!(
            unchanged_deployment(&staging, &job, None, None)?
                .1
                .is_none()
        );
        let url = "https://reports.example.com/";
        let record = || {
            DeployTargets::record_deployed_report(
                &path,
                &target,
                Some(url),
                &job.suite_ids,
                Some(&fingerprint),
                None,
            )
        };
        record()?;
        let recorded = fs::read(&path)?;
        // A skip leaves the record untouched.
        assert_eq!(
            unchanged_deployment(&staging, &job, Some(&path), None)?,
            (fingerprint.clone(), Some(url.to_owned()))
        );
        assert_eq!(fs::read(&path)?, recorded);

        // Deploy anyway publishes and still reports the identity to record. Publishing
        // forgets the old record first, so a crash during upload cannot leave a match.
        let mut forced = job.clone();
        forced.force = true;
        assert_eq!(
            unchanged_deployment(&staging, &forced, Some(&path), None)?,
            (fingerprint.clone(), None)
        );
        let settings = DeployTargets::load(&path)?;
        assert!(settings.report_fingerprint(&target).is_none());
        assert_eq!(
            settings.latest_report_url(&job.suite_ids).as_deref(),
            Some("https://reports.example.com/suites/one")
        );
        assert!(
            unchanged_deployment(&staging, &job, Some(&path), None)?
                .1
                .is_none()
        );

        // Requests prepared before a target edit or removal are refused before any upload,
        // including a replayed Deploy anyway, and leave the record in place.
        record()?;
        let recorded = fs::read(&path)?;
        let mut preview = forced.clone();
        preview.target.environment = "preview".into();
        let mut stale_credential = forced.clone();
        stale_credential.target.credential_id = Some(uuid::Uuid::new_v4().to_string());
        let mut removed = forced.clone();
        removed.target.id = "removed".into();
        for stale in [preview, stale_credential, removed] {
            assert!(unchanged_deployment(&staging, &stale, Some(&path), None).is_err());
            assert_eq!(fs::read(&path)?, recorded);
        }
        // Display names and saved URLs do not make a request stale.
        let mut renamed = job.clone();
        renamed.target.name = "Renamed".into();
        renamed.target.published_url = None;
        assert_eq!(
            unchanged_deployment(&staging, &renamed, Some(&path), None)?
                .1
                .as_deref(),
            Some(url)
        );

        // Unreadable settings fail before uploading instead of risking a stale record.
        let corrupt = root.path().join("corrupt.json");
        fs::write(&corrupt, b"{not json")?;
        assert!(unchanged_deployment(&staging, &job, Some(&corrupt), None).is_err());
        assert!(unchanged_deployment(&staging, &job, Some(root.path()), None).is_err());

        // New report content publishes.
        fs::write(
            output.join("one/bitmaps_test/older/home.png"),
            lodepng::encode32(&[9_u8, 8, 7, 255], 1, 1)?,
        )?;
        regenerate(&output, None);
        let changed = staged(&output, &root.path().join("changed"));
        let (next, unchanged) = unchanged_deployment(&changed, &job, Some(&path), None)?;
        assert_ne!(next, fingerprint);
        assert!(unchanged.is_none());
        assert!(
            DeployTargets::load(&path)?
                .report_fingerprint(&target)
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn unknown_outcomes_forget_every_target_sharing_the_site() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("targets.json");
        let site = uuid::Uuid::new_v4().to_string();
        let netlify = |id: &str, site_id: &str| -> DeployTarget {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "provider": "netlify", "siteId": site_id,
                "credentialId": uuid::Uuid::new_v4().to_string()
            }))
            .unwrap()
        };
        let first = netlify("first", &site);
        let second = netlify("second", &site);
        let other = netlify("other", &uuid::Uuid::new_v4().to_string());
        // Same saved URL with a different destination record, e.g. a renamed custom domain.
        let mut aliased = netlify("aliased", &uuid::Uuid::new_v4().to_string());
        aliased.provider = DeploymentProvider::AzureSwa;
        aliased.site_id.clear();
        aliased.environment = "alias".into();
        DeployTargets {
            targets: vec![
                first.clone(),
                second.clone(),
                other.clone(),
                aliased.clone(),
            ],
            selected: Some(first.id.clone()),
            ..Default::default()
        }
        .save(&path)?;
        let content = "a".repeat(64);
        let inputs = "c".repeat(64);
        let record = |target: &DeployTarget, url: &str| {
            DeployTargets::record_deployed_report(
                &path,
                target,
                Some(url),
                std::slice::from_ref(&target.id),
                Some(&content),
                Some(&inputs),
            )
        };
        record(&first, "https://shared.example.com/")?;
        record(&other, "https://other.example.com/")?;
        record(&aliased, "https://alias.example.com/")?;
        // An interrupted upload to `second` may have replaced the site `first` published to.
        DeployTargets::forget_report_fingerprint(&path, &second)?;
        let settings = DeployTargets::load(&path)?;
        assert!(settings.report_fingerprint(&first).is_none());
        assert!(settings.unchanged_inputs(&first, &inputs).is_none());
        assert_eq!(
            settings.latest_report_url(&["first".into()]).as_deref(),
            Some("https://shared.example.com/suites/first")
        );
        assert_eq!(settings.report_fingerprint(&other), Some(content.as_str()));
        assert_eq!(
            settings.report_fingerprint(&aliased),
            Some(content.as_str())
        );
        // Targets whose saved URLs name the same site are forgotten together too.
        let mut renamed = other.clone();
        renamed.id = "renamed".into();
        renamed.name = "renamed".into();
        renamed.credential_id = Some(uuid::Uuid::new_v4().to_string());
        let mut settings = DeployTargets::load(&path)?;
        let mut alias_url = renamed.clone();
        alias_url.site_id = uuid::Uuid::new_v4().to_string();
        alias_url.published_url = Some("https://alias.example.com".into());
        settings.targets.push(alias_url.clone());
        settings.save(&path)?;
        DeployTargets::forget_report_fingerprint(&path, &alias_url)?;
        let settings = DeployTargets::load(&path)?;
        assert!(settings.report_fingerprint(&aliased).is_none());
        assert_eq!(settings.report_fingerprint(&other), Some(content.as_str()));
        Ok(())
    }

    fn no_text() -> crate::cli::args::SnapshotArgs {
        crate::cli::args::SnapshotArgs::default()
    }

    #[test]
    fn input_fingerprints_follow_suite_files_text_selection_and_target() -> Result<()> {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        snapshot_fixture(&output);
        let workspace = crate::config::WorkspaceConfig::default();
        let job = DeploymentJob {
            target: azure_target(),
            suite_ids: vec!["one".into()],
            catalog_ids: vec!["one".into()],
            partial_confirmed: false,
            force: false,
            mode: crate::desktop::deployment::DeploymentMode::Replace,
            remove_ids: Vec::new(),
        };
        let inputs = |job: &DeploymentJob, args: &crate::cli::args::SnapshotArgs| {
            deployment_input_fingerprint(&output, job, args, &workspace).unwrap()
        };
        let first = inputs(&job, &no_text());
        assert!(valid_fingerprint(&first));

        // Generating, reading, and locking reports leave the inputs unchanged.
        regenerate(&output, None);
        fs::write(output.join("one/.artifacts.lock"), b"")?;
        fs::write(output.join("one/runs.json.tmp"), b"partial")?;
        fs::create_dir_all(output.join("one/.compare-staging-1"))?;
        fs::write(output.join("one/.compare-staging-1/home.png"), b"partial")?;
        assert_eq!(inputs(&job, &no_text()), first);
        // Names, URLs, and credentials of the target are not inputs.
        let mut renamed = job.clone();
        renamed.target.name = "Renamed".into();
        renamed.target.published_url = Some("https://reports.example.com/".into());
        assert_eq!(inputs(&renamed, &no_text()), first);

        // Report text, suite selection, and site settings are inputs.
        let titled = crate::cli::args::SnapshotArgs {
            title: Some("Release candidate".into()),
            ..no_text()
        };
        assert_ne!(inputs(&job, &titled), first);
        let mut preview = job.clone();
        preview.target.environment = "preview".into();
        assert_ne!(inputs(&preview, &no_text()), first);
        let mut both = job.clone();
        both.suite_ids = vec!["one".into(), "two".into()];
        fs::create_dir_all(output.join("two"))?;
        assert_ne!(inputs(&both, &no_text()), first);
        // A selected suite without output cannot be fingerprinted.
        both.suite_ids = vec!["one".into(), "missing".into()];
        assert!(deployment_input_fingerprint(&output, &both, &no_text(), &workspace).is_err());

        // Approvals rewrite baselines, metadata edits rewrite suite.json, and runs add files.
        let reference = output.join("one/bitmaps_reference/home.png");
        let bytes = fs::read(&reference)?;
        let mut changed = Vec::new();
        for change in [
            Box::new(|| fs::write(&reference, b"approved")) as Box<dyn Fn() -> std::io::Result<()>>,
            Box::new(|| fs::write(output.join("one/suite.json"), br#"{"displayName":"Shop"}"#)),
            Box::new(|| fs::write(output.join("one/bitmaps_test/older/extra.png"), b"new")),
        ] {
            change()?;
            let next = inputs(&job, &no_text());
            assert!(!changed.contains(&next));
            assert_ne!(next, first);
            changed.push(next);
        }
        // Restoring the same bytes later is still a change: modification times differ.
        fs::write(&reference, bytes)?;
        assert_ne!(inputs(&job, &no_text()), first);
        Ok(())
    }

    #[test]
    fn early_checks_skip_generation_only_for_recorded_inputs() -> Result<()> {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        snapshot_fixture(&output);
        let staging = staged(&output, &root.path().join("staged"));
        let path = root.path().join("targets.json");
        let target = azure_target();
        DeployTargets {
            targets: vec![target.clone()],
            selected: Some(target.id.clone()),
            ..Default::default()
        }
        .save(&path)?;
        let job = DeploymentJob {
            target: target.clone(),
            suite_ids: vec!["one".into()],
            catalog_ids: vec!["one".into()],
            partial_confirmed: false,
            force: false,
            mode: crate::desktop::deployment::DeploymentMode::Replace,
            remove_ids: Vec::new(),
        };
        let check = |job: &DeploymentJob| {
            check_unchanged_inputs(&output, root.path(), job, &no_text(), Some(&path))
        };
        // Nothing recorded: generate and publish, remembering the inputs.
        let (inputs, unchanged) = check(&job)?;
        let inputs = inputs.unwrap();
        assert!(unchanged.is_none());
        let (fingerprint, _) = unchanged_deployment(&staging, &job, Some(&path), Some(&inputs))?;
        let url = "https://reports.example.com/";
        DeployTargets::record_deployed_report(
            &path,
            &target,
            Some(url),
            &job.suite_ids,
            Some(&fingerprint),
            Some(&inputs),
        )?;
        // The same inputs are recognized before any generation, without writing settings.
        let recorded = fs::read(&path)?;
        assert_eq!(
            check(&job)?,
            (
                Some(inputs.clone()),
                Some((url.to_owned(), fingerprint.clone()))
            )
        );
        assert_eq!(fs::read(&path)?, recorded);
        // Deploy anyway always generates and publishes.
        let mut forced = job.clone();
        forced.force = true;
        assert!(check(&forced)?.1.is_none());
        // Stale requests are refused at the early check too.
        let mut stale = job.clone();
        stale.target.environment = "preview".into();
        assert!(check(&stale).is_err());
        assert_eq!(fs::read(&path)?, recorded);

        // New inputs that reproduce the published content (an identical rerun) skip the
        // upload after generation and are remembered for the next early check.
        let marker = output.join("one/bitmaps_test/older/notes.txt");
        fs::write(&marker, b"unrelated")?;
        let (changed_inputs, unchanged) = check(&job)?;
        let changed_inputs = changed_inputs.unwrap();
        assert_ne!(changed_inputs, inputs);
        assert!(unchanged.is_none());
        assert_eq!(
            unchanged_deployment(&staging, &job, Some(&path), Some(&changed_inputs))?,
            (fingerprint.clone(), Some(url.to_owned()))
        );
        assert_eq!(check(&job)?.1, Some((url.to_owned(), fingerprint.clone())));
        // Without inputs the early check is disabled, never failed.
        fs::remove_dir_all(output.join("one"))?;
        assert_eq!(check(&job)?, (None, None));
        Ok(())
    }

    #[test]
    fn merged_staging_keeps_other_suites_by_summary_without_their_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let output = root.path().join("output");
        snapshot_fixture(&output);
        let kept = vec![
            super::site_state::tests::summary("alpha"),
            super::site_state::tests::summary("zeta"),
        ];
        let destination = root.path().join("merged");
        let inventory = stage_site(&output, &["one".into()], &kept, &destination)?;
        // Only generated suites are staged and inventoried.
        assert_eq!(inventory.suites.keys().collect::<Vec<_>>(), ["one"]);
        assert!(inventory.runs.contains_key("one"));
        assert!(
            !destination
                .join("_snapshot/data/suites/alpha.json")
                .exists()
        );
        assert!(!destination.join("_snapshot/images/alpha").exists());
        // The merged manifest lists every suite in order, with kept summaries verbatim.
        let summaries = crate::snapshot::suite_summaries(&destination)?;
        assert_eq!(
            summaries
                .iter()
                .map(|summary| summary.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "one", "zeta"]
        );
        assert_eq!(summaries[0], kept[0]);
        let status: serde_json::Value =
            serde_json::from_slice(&fs::read(destination.join("_snapshot/status.json"))?)?;
        assert_eq!(status["suites"], 3);
        assert_eq!(status["total"], 5);
        // A merged site fingerprints as the whole site, and partial verification passes.
        let all = vec!["alpha".to_string(), "one".into(), "zeta".into()];
        assert!(crate::snapshot::site_fingerprint(&destination, &all).is_ok());
        assert!(crate::snapshot::site_fingerprint(&destination, &["one".into()]).is_err());
        crate::snapshot::artifact_inventory(
            &destination,
            &["one".into()],
            &["alpha".into(), "zeta".into()],
        )?;

        // Kept suites that collide with generated ones or carry foreign report paths fail,
        // and leave no staging behind.
        let mut foreign = super::site_state::tests::summary("beta");
        foreign.report = "_snapshot/data/suites/alpha.json?v=AAAAAAAAAAAAAAAAAAAA".into();
        for invalid in [
            vec![super::site_state::tests::summary("one")],
            vec![foreign],
        ] {
            let failed = root.path().join("failed");
            assert!(stage_site(&output, &["one".into()], &invalid, &failed).is_err());
            assert!(!failed.exists());
        }
        // Without kept suites, staging matches the original full-site staging.
        let full = root.path().join("full");
        stage_site(&output, &["one".into()], &[], &full)?;
        assert_eq!(
            crate::snapshot::suite_summaries(&full)?,
            crate::snapshot::suite_summaries(&output)?
        );
        Ok(())
    }

    #[test]
    fn staging_includes_only_selected_report_dependencies() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output");
        let expected = snapshot_fixture(&output);
        let skipped = [
            "Lato-Bold.ttf",
            "Lato-Regular.ttf",
            ".gitignore",
            "unused.js",
            "index_bundle.js.map",
            "old-report.html",
            "one/bitmaps_reference/unused.png",
            "one/bitmaps_test/run/unused.png",
            "one/bitmaps_test/run/unused.diff.png",
            "one/bitmaps_test/run/private.json",
            "one/runs.json",
            "one/.runs.lock",
            "one/.artifacts.lock",
            ".staging/owned/owner.json",
            ".staging/owned/files/private.zip",
            ".staging.lock",
            "one/bitmaps_test/older/unused.png",
            "one/bitmaps_test/older/unused.diff.png",
            "one/bitmaps_test/oldest/unused.png",
            "one/html_report/unused.js",
            "two/html_report/index.html",
            "two/bitmaps_test/run/image.png",
            "states/private.json",
        ];
        for relative in skipped {
            let path = output.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        let destination = directory.path().join("publish");
        stage_reports(&output, &["one".into()], &destination).unwrap();
        for relative in skipped {
            assert!(!destination.join(relative).exists(), "{relative}");
            assert_eq!(fs::read(output.join(relative)).unwrap(), b"fixture");
        }
        let package = package::Package::create(&destination, expected.len() as u64).unwrap();
        let archive = zip::ZipArchive::new(package.file.reopen().unwrap()).unwrap();
        assert_eq!(
            archive
                .file_names()
                .map(|name| name.expect("ZIP entry name must be readable").into_owned())
                .collect::<BTreeSet<_>>(),
            expected
        );
        for relative in &expected {
            assert_eq!(
                fs::read(destination.join(relative)).unwrap(),
                fs::read(output.join(relative)).unwrap()
            );
        }
        assert!(!destination.join("two").exists());
        assert!(!destination.join("states").exists());
        assert!(stage_reports(&output, &[], &directory.path().join("empty")).is_err());
        assert!(
            stage_reports(
                &output,
                &["../outside".into()],
                &directory.path().join("escape")
            )
            .is_err()
        );
    }
}
