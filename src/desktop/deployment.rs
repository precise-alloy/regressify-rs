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
pub mod netlify;
mod optimization;
mod package;
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

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentJob {
    pub target: DeployTarget,
    pub suite_ids: Vec<String>,
    pub catalog_ids: Vec<String>,
    pub partial_confirmed: bool,
}

impl DeploymentJob {
    pub fn validate(&self) -> Result<()> {
        self.target.validate()?;
        ensure!(
            !self.suite_ids.is_empty(),
            "Select at least one test suite to deploy"
        );
        let selected = self.suite_ids.iter().collect::<BTreeSet<_>>();
        let catalog = self.catalog_ids.iter().collect::<BTreeSet<_>>();
        ensure!(
            selected.len() == self.suite_ids.len()
                && catalog.len() == self.catalog_ids.len()
                && selected.is_subset(&catalog),
            "Deployment suite selection is invalid"
        );
        for suite in &self.catalog_ids {
            validate_suite_id(suite)?;
        }
        ensure!(
            selected == catalog || self.partial_confirmed,
            "Confirm that you intend to deploy only the selected test suites"
        );
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
        Self::record_publication_for_suites(path, target, url, &[])
    }

    pub fn record_report_publication(
        path: &Path,
        target: &DeployTarget,
        url: Option<&str>,
        suite_ids: &[String],
    ) -> Result<Self> {
        ensure!(
            !suite_ids.is_empty(),
            "Published reports must contain at least one suite"
        );
        Self::record_publication_for_suites(path, target, url, suite_ids)
    }

    fn record_publication_for_suites(
        path: &Path,
        target: &DeployTarget,
        url: Option<&str>,
        suite_ids: &[String],
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DeploymentStats {
    file_count: u64,
    original_bytes: u64,
    total_bytes: u64,
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
        target.validate()?;
        let started = std::time::Instant::now();
        let mut stats = DeploymentStats::default();
        let result = match self {
            Self::Azure(client) => client
                .deploy_with_stats(root, target, &mut stats)
                .await
                .map(Some),
            Self::Netlify(client) => client
                .deploy_with_stats(root, target, &mut stats)
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
        result
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

pub fn stage_reports(output: &Path, suite_ids: &[String], destination: &Path) -> Result<()> {
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
        || stage_reports_inner(output, suite_ids, destination),
    )
}

fn stage_reports_inner(output: &Path, suite_ids: &[String], destination: &Path) -> Result<()> {
    ensure!(
        !suite_ids.is_empty(),
        "Select at least one test suite to deploy"
    );
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
    let dependencies = crate::snapshot::artifact_paths(&output, suite_ids)?;
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
    let staged = (|| -> Result<()> {
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
        phase(&format!(
            "Verifying {} staged report files ({:.2} MiB)",
            copied.len(),
            copied_bytes as f64 / (1024.0 * 1024.0)
        ));
        crate::snapshot::artifact_paths(destination, suite_ids)?;
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(destination);
        return Err(error);
    }
    phase(&format!(
        "Staging complete: {} files ({:.2} MiB) for {} suites",
        copied.len(),
        copied_bytes as f64 / (1024.0 * 1024.0),
        suite_ids.len()
    ));
    Ok(())
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
            "Resizing and staging",
            "Resized ",
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
