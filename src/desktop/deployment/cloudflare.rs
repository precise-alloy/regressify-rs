use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use futures::{StreamExt, TryStreamExt, stream};
use reqwest::{
    Client, RequestBuilder, Response, Url,
    header::{AUTHORIZATION, HeaderValue},
    multipart,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use super::optimization::{self, PreparedReport};

const MAX_FILES: usize = 100_000;
const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_CONTENT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_BUCKET_BYTES: u64 = 50 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const UPLOAD_MEMORY_BYTES: usize = 50 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudflareTarget {
    pub account_id: String,
    pub worker_name: String,
    pub workers_dev: Option<bool>,
    pub html_handling: String,
    pub not_found_handling: String,
    pub compatibility_date: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl Default for CloudflareTarget {
    fn default() -> Self {
        Self {
            account_id: String::new(),
            worker_name: String::new(),
            workers_dev: Some(true),
            html_handling: "auto-trailing-slash".into(),
            not_found_handling: "none".into(),
            compatibility_date: "2026-09-25".into(),
            exclude: Vec::new(),
        }
    }
}

impl CloudflareTarget {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.account_id.len() == 32
                && self.account_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Enter a valid Cloudflare Account ID (32 hexadecimal characters)"
        );
        validate_worker_name(&self.worker_name)?;
        if self.workers_dev == Some(true) {
            validate_public_worker_name(&self.worker_name)?;
        }
        ensure!(
            [
                "auto-trailing-slash",
                "force-trailing-slash",
                "drop-trailing-slash",
                "none"
            ]
            .contains(&self.html_handling.as_str()),
            "Invalid Cloudflare HTML handling setting"
        );
        ensure!(
            ["none", "404-page", "single-page-application"]
                .contains(&self.not_found_handling.as_str()),
            "Invalid Cloudflare not-found handling setting"
        );
        ensure!(
            self.compatibility_date.len() == 10
                && chrono::NaiveDate::parse_from_str(&self.compatibility_date, "%Y-%m-%d").is_ok(),
            "Cloudflare compatibility date must be a valid YYYY-MM-DD date"
        );
        for pattern in &self.exclude {
            glob::Pattern::new(pattern).context("Invalid Cloudflare exclusion glob")?;
        }
        Ok(())
    }
}

fn validate_worker_name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 63
            && !value.starts_with('-')
            && value.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-')),
        "Worker name must contain 1-63 lowercase letters, digits, underscores, or hyphens and cannot start with a hyphen"
    );
    Ok(())
}

fn validate_public_worker_name(value: &str) -> Result<()> {
    validate_worker_name(value)?;
    ensure!(
        !value.ends_with('-'),
        "Worker names ending in a hyphen cannot use workers.dev; rename the Worker or disable its public route"
    );
    Ok(())
}

fn validate_dns_label(value: &str, label: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 63
            && !value.starts_with('-')
            && !value.ends_with('-')
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "Cloudflare {label} must contain 1-63 letters, digits, or hyphens and cannot start or end with a hyphen"
    );
    Ok(())
}

pub(super) fn validate_token(token: &str) -> Result<()> {
    ensure!(
        !token.is_empty()
            && token.len() <= 1024
            && token.bytes().all(|byte| byte.is_ascii_graphic()),
        "Enter a valid Cloudflare API token"
    );
    Ok(())
}

pub struct CloudflareClient {
    client: Client,
    endpoint: Url,
    authorization: HeaderValue,
    backoff: Duration,
    read_timeout: Duration,
    deadline: Duration,
    verify_interval: Duration,
    verify_attempts: usize,
    #[cfg(test)]
    verification_url: Option<Url>,
}

#[derive(Deserialize)]
struct ApiEnvelope<Value> {
    success: bool,
    result: Option<Value>,
    errors: Option<Vec<ApiError>>,
}

#[derive(Deserialize)]
struct ApiError {
    code: u64,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct AccountSubdomain {
    subdomain: String,
}

#[derive(Deserialize, Serialize)]
struct WorkerRoute {
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previews_enabled: Option<bool>,
}

#[derive(Deserialize)]
struct UploadSession {
    jwt: String,
    buckets: Vec<Vec<String>>,
}

#[derive(Deserialize)]
struct UploadResult {
    jwt: Option<String>,
}

#[derive(Deserialize)]
struct PublishedWorker {
    id: String,
    has_assets: bool,
}

#[derive(Debug)]
struct ApiFailure {
    message: String,
    retryable: bool,
    retry_after: Option<Duration>,
    missing_worker: bool,
    rejected: bool,
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApiFailure {}

impl ApiFailure {
    fn new(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            retryable,
            retry_after: None,
            missing_worker: false,
            rejected: false,
        }
    }
}

impl CloudflareClient {
    pub fn new(token: &str) -> Result<Self> {
        validate_token(token)?;
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!("Regressify-Desktop/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::none())
                .https_only(!cfg!(test))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(180))
                .build()?,
            endpoint: Url::parse("https://api.cloudflare.com/client/v4/")?,
            authorization: bearer(token)?,
            backoff: Duration::from_secs(1),
            read_timeout: Duration::from_secs(30),
            // Leave time before Cloudflare's one-hour upload/completion tokens expire.
            deadline: Duration::from_secs(55 * 60),
            verify_interval: Duration::from_secs(2),
            verify_attempts: 30,
            #[cfg(test)]
            verification_url: None,
        })
    }

    async fn request<Value: DeserializeOwned>(
        &self,
        build: impl Fn() -> Result<RequestBuilder>,
        retry: bool,
    ) -> Result<Value> {
        self.request_protected(build, retry, &[]).await
    }

    async fn request_protected<Value: DeserializeOwned>(
        &self,
        build: impl Fn() -> Result<RequestBuilder>,
        retry: bool,
        secrets: &[&str],
    ) -> Result<Value> {
        let attempts = if retry { 6 } else { 1 };
        for attempt in 0..attempts {
            let endpoint = crate::diagnostics::record::SafeValue::Upload;
            super::diagnostics::request_started(
                super::DeploymentProvider::CloudflareWorkers,
                endpoint,
                attempt + 1,
            );
            let started = Instant::now();
            let request = build()?
                .build()
                .map_err(|_| anyhow!("Cannot construct Cloudflare request"))?;
            let authorization = request
                .headers()
                .get(AUTHORIZATION)
                .cloned()
                .unwrap_or_else(|| self.authorization.clone());
            let result = match self.client.execute(request).await {
                Ok(response) => {
                    super::diagnostics::response_received(
                        super::DeploymentProvider::CloudflareWorkers,
                        endpoint,
                        attempt + 1,
                        Some(response.status().as_u16()),
                        started.elapsed(),
                    );
                    api_response(response, &[&self.authorization, &authorization], secrets).await
                }
                Err(error) => {
                    super::diagnostics::response_received(
                        super::DeploymentProvider::CloudflareWorkers,
                        endpoint,
                        attempt + 1,
                        None,
                        started.elapsed(),
                    );
                    Err(network_failure(error))
                }
            };
            match result {
                Ok(value) => return Ok(value),
                Err(error) if error.retryable && attempt + 1 < attempts => {
                    let delay = error.retry_after.unwrap_or(self.backoff * (1 << attempt));
                    super::diagnostics::retry(
                        super::DeploymentProvider::CloudflareWorkers,
                        endpoint,
                        attempt + 2,
                        delay,
                    );
                    super::phase(&format!(
                        "Cloudflare retry {}/{} in {:.1}s",
                        attempt + 2,
                        attempts,
                        delay.as_secs_f64()
                    ));
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        bail!("Cloudflare request retry budget exhausted")
    }

    async fn get<Value: DeserializeOwned>(&self, path: &str) -> Result<Value> {
        let url = self.endpoint.join(path)?;
        self.request(
            || {
                Ok(self
                    .client
                    .get(url.clone())
                    .header(AUTHORIZATION, self.authorization.clone())
                    .timeout(self.read_timeout))
            },
            true,
        )
        .await
    }

    pub async fn deploy(&self, root: &Path, target: &CloudflareTarget) -> Result<Option<String>> {
        self.deploy_with_stats(
            root,
            target,
            &super::KeptFiles::new(),
            &mut super::DeploymentStats::default(),
        )
        .await
    }

    /// Publishes `root` plus `kept` assets the Worker already serves, which are listed in the
    /// manifest by hash without being uploaded.
    pub(super) async fn deploy_with_stats(
        &self,
        root: &Path,
        target: &CloudflareTarget,
        kept: &super::KeptFiles,
        stats: &mut super::DeploymentStats,
    ) -> Result<Option<String>> {
        target.validate()?;
        let result = tokio::time::timeout(
            self.deadline,
            self.deploy_inner(root, target, kept, stats),
        )
            .await
            .map_err(|_| anyhow!("Cloudflare deployment exceeded its deadline; check the Worker's deployment history before retrying"))?;
        if let Ok(url) = &result {
            super::diagnostics::published(
                super::DeploymentProvider::CloudflareWorkers,
                url.as_deref(),
            );
        }
        result
    }

    async fn deploy_inner(
        &self,
        root: &Path,
        target: &CloudflareTarget,
        kept: &super::KeptFiles,
        stats: &mut super::DeploymentStats,
    ) -> Result<Option<String>> {
        super::validate_static_configuration(root, super::DeploymentProvider::CloudflareWorkers)
            .map_err(|error| {
                if error.downcast_ref::<std::io::Error>().is_some() {
                    error.context(format!(
                        "Cannot prepare local Cloudflare deployment files in '{}'",
                        root.display()
                    ))
                } else {
                    error
                }
            })?;
        let deadline = Instant::now() + self.deadline;
        let account = format!("accounts/{}", target.account_id);
        let worker = format!("{account}/workers/scripts/{}", target.worker_name);
        super::phase("Hashing selected report files for Cloudflare");
        let directory = root.to_path_buf();
        let exclude = target.exclude.clone();
        let html_handling = target.html_handling.clone();
        let kept = kept.clone();
        let diagnostic_context = crate::diagnostics::runtime::current();
        // Keep optimized files alive until asset uploads and public verification have finished.
        let (manifest, headers, redirects, prepared) = super::with_heartbeat(
            "Preparing Cloudflare deployment files",
            tokio::task::spawn_blocking(move || {
                crate::diagnostics::runtime::in_scope(diagnostic_context, || {
                    let (manifest, prepared) = Manifest::prepare(
                        &directory,
                        deadline,
                        MAX_CONTENT_BYTES,
                        &exclude,
                        &kept,
                    )?;
                    let directory = directory.canonicalize()?;
                    let patterns = exclude
                        .iter()
                        .map(|pattern| glob::Pattern::new(pattern))
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    let policy = |name: &str| {
                        if patterns
                            .iter()
                            .any(|pattern| pattern.matches_path(Path::new(name)))
                        {
                            Ok(String::new())
                        } else {
                            hosting_policy(&directory, name)
                        }
                    };
                    let headers = policy("_headers")?;
                    let redirects = cloudflare_redirects(&policy("_redirects")?, &html_handling);
                    Ok::<_, anyhow::Error>((manifest, headers, redirects, prepared))
                })
            }),
        )
        .await
        .context("Cloudflare file preparation task failed")?
        .with_context(|| {
            format!(
                "Cannot prepare local Cloudflare deployment files in '{}'",
                root.display()
            )
        })?;
        *stats = manifest.stats(prepared.original_bytes(), root);
        let body = serde_json::to_vec(&serde_json::json!({"manifest": &manifest.files}))?;
        ensure!(
            body.len() <= MAX_MANIFEST_BYTES,
            "Cloudflare asset manifest exceeds 32 MiB"
        );
        super::phase("Validating Cloudflare deployment target");
        let (route_config, update_route) = self
            .route_configuration(&worker, target.workers_dev)
            .await?;
        let public_url = if route_config.enabled {
            Some(
                self.public_worker_url(&account, &target.worker_name)
                    .await?,
            )
        } else {
            None
        };
        super::phase(&format!(
            "Creating Cloudflare upload session for {} files ({:.2} MiB)",
            manifest.files.len(),
            manifest.bytes as f64 / (1024.0 * 1024.0)
        ));
        let session_url = self
            .endpoint
            .join(&format!("{worker}/assets-upload-session"))?;
        let session: UploadSession = self
            .request(
                || {
                    Ok(self
                        .client
                        .post(session_url.clone())
                        .header(AUTHORIZATION, self.authorization.clone())
                        .header("Content-Type", "application/json")
                        .timeout(self.read_timeout)
                        .body(body.clone()))
                },
                true,
            )
            .await?;
        let upload_token = zeroize::Zeroizing::new(session.jwt);
        let upload_authorization = bearer(&upload_token)?;
        let manifest = Arc::new(manifest);
        let completion = if session.buckets.is_empty() {
            super::phase("Cloudflare already has all report assets; no uploads required");
            upload_token
        } else {
            self.upload_assets(
                manifest.clone(),
                session.buckets,
                &account,
                upload_authorization,
            )
            .await?
        };
        let metadata = zeroize::Zeroizing::new(serde_json::to_string(&serde_json::json!({
            "compatibility_date": target.compatibility_date,
            "assets": {
                "jwt": completion.as_str(),
                "config": {
                    "_headers": headers,
                    "_redirects": redirects,
                    "html_handling": target.html_handling,
                    "not_found_handling": target.not_found_handling
                }
            }
        }))?);
        let publish_url = self.endpoint.join(&worker)?;
        super::phase("Publishing Cloudflare report assets");
        // Publication is not retried: a lost response must not overwrite a later deployment.
        let published: PublishedWorker = self
            .request_protected(
                || {
                    Ok(self
                        .client
                        .put(publish_url.clone())
                        .header(AUTHORIZATION, self.authorization.clone())
                        .timeout(self.read_timeout)
                        .multipart(
                            multipart::Form::new().part(
                                "metadata",
                                multipart::Part::text(metadata.to_string())
                                    .mime_str("application/json")?,
                            ),
                        ))
                },
                false,
                &[completion.as_str()],
            )
            .await
            .map_err(publication_error)?;
        ensure!(
            published.id == target.worker_name && published.has_assets,
            "Cloudflare returned a different Worker or no deployed assets"
        );
        let route = if update_route {
            super::phase("Configuring the published Worker's workers.dev route");
            let route_url = self.endpoint.join(&format!("{worker}/subdomain"))?;
            let route: WorkerRoute = self
                .request(
                    || {
                        Ok(self
                            .client
                            .post(route_url.clone())
                            .header(AUTHORIZATION, self.authorization.clone())
                            .timeout(self.read_timeout)
                            .json(&route_config))
                    },
                    true,
                )
                .await
                .context(
                    "Worker was published, but its workers.dev route could not be configured",
                )?;
            ensure!(
                route.enabled == route_config.enabled
                    && route_config
                        .previews_enabled
                        .is_none_or(|expected| route.previews_enabled == Some(expected)),
                "Worker was published, but Cloudflare did not confirm its workers.dev route"
            );
            route
        } else {
            // Preserve dashboard changes made while the upload was in progress.
            match self
                .get::<WorkerRoute>(&format!("{worker}/subdomain"))
                .await
            {
                Ok(route) => route,
                Err(error) => {
                    super::phase(&format!(
                        "Worker '{}' was published, but its current routing could not be verified: {error:#}. Inspect routing in Cloudflare; no public URL is reported",
                        target.worker_name
                    ));
                    return Ok(None);
                }
            }
        };
        if !route.enabled {
            super::phase(&format!(
                "Deployed Worker '{}' without a public workers.dev route",
                target.worker_name
            ));
            return Ok(None);
        }
        let public_url = match public_url {
            Some(url) => url,
            None => match self.public_worker_url(&account, &target.worker_name).await {
                Ok(url) => url,
                Err(error) => {
                    super::phase(&format!(
                        "Worker '{}' was published, but its current public URL could not be verified: {error:#}. Inspect routing in Cloudflare",
                        target.worker_name
                    ));
                    return Ok(None);
                }
            },
        };
        super::phase("Verifying published Cloudflare report");
        self.verify_publication(&public_url, manifest, &target.html_handling)
            .await?;
        super::phase(&format!("Deployed: {public_url}"));
        Ok(Some(public_url.to_string()))
    }

    async fn public_worker_url(&self, account: &str, worker: &str) -> Result<Url> {
        validate_public_worker_name(worker)?;
        let subdomain: AccountSubdomain = self.get(&format!("{account}/workers/subdomain")).await
            .context("Cannot read workers.dev subdomain; register one in Cloudflare or disable the target's public route")?;
        let url = worker_url(worker, &subdomain.subdomain)?;
        #[cfg(test)]
        let url = self.verification_url.clone().unwrap_or(url);
        Ok(url)
    }

    async fn route_configuration(
        &self,
        worker: &str,
        requested: Option<bool>,
    ) -> Result<(WorkerRoute, bool)> {
        if let Some(enabled) = requested {
            return Ok((
                WorkerRoute {
                    enabled,
                    previews_enabled: (!enabled).then_some(false),
                },
                true,
            ));
        }
        match self.get::<WorkerRoute>(&format!("{worker}/subdomain")).await {
            Ok(route) => {
                ensure!(route.previews_enabled.is_some(), "Cloudflare returned no preview routing state");
                Ok((route, false))
            }
            Err(error) if error.downcast_ref::<ApiFailure>().is_some_and(|failure| failure.missing_worker) => {
                Ok((WorkerRoute { enabled: false, previews_enabled: Some(false) }, true))
            }
            Err(error) => Err(error.context("Cannot preserve existing workers.dev routing; choose an explicit routing setting only to intentionally override it")),
        }
    }

    async fn upload_assets(
        &self,
        manifest: Arc<Manifest>,
        buckets: Vec<Vec<String>>,
        account: &str,
        authorization: HeaderValue,
    ) -> Result<zeroize::Zeroizing<String>> {
        let token = authorization.to_str()?.trim_start_matches("Bearer ");
        let concurrency = upload_concurrency(token);
        self.upload_assets_bounded(
            manifest,
            buckets,
            account,
            authorization,
            concurrency,
            UPLOAD_MEMORY_BYTES,
        )
        .await
    }

    async fn upload_assets_bounded(
        &self,
        manifest: Arc<Manifest>,
        buckets: Vec<Vec<String>>,
        account: &str,
        authorization: HeaderValue,
        concurrency: usize,
        byte_budget: usize,
    ) -> Result<zeroize::Zeroizing<String>> {
        let single = single_asset_upload(authorization.to_str()?.trim_start_matches("Bearer "));
        ensure!(
            buckets.iter().all(|bucket| !bucket.is_empty()),
            "Cloudflare returned an empty upload bucket"
        );
        let buckets = if single {
            buckets
                .into_iter()
                .flatten()
                .map(|hash| vec![hash])
                .collect()
        } else {
            buckets
        };
        let sizes = manifest.validate_buckets(&buckets)?;
        ensure!(
            concurrency > 0
                && byte_budget > 0
                && byte_budget <= u32::MAX as usize
                && sizes.iter().all(|size| *size <= byte_budget as u64),
            "Cloudflare upload limits cannot accommodate a server bucket"
        );
        let memory = Arc::new(tokio::sync::Semaphore::new(byte_budget));
        let total = buckets.len();
        let completion = stream::iter(buckets.into_iter().zip(sizes).enumerate())
            .map(|(index, (hashes, size))| {
                let manifest = manifest.clone();
                let memory = memory.clone();
                let authorization = authorization.clone();
                async move {
                    let permit = memory.acquire_many_owned(size.max(1) as u32).await?;
                    // Retain the budget during disk reads, request copies, retries, and cancellation.
                    let (assets, _permit) = tokio::task::spawn_blocking(move || {
                        let assets = hashes
                            .into_iter()
                            .map(|hash| {
                                let (path, bytes) = manifest.read_asset(&hash)?;
                                let content = if single {
                                    bytes
                                } else {
                                    STANDARD.encode(bytes).into_bytes()
                                };
                                Ok::<_, anyhow::Error>((hash, content, mime_type(&path)))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        Ok::<_, anyhow::Error>((assets, permit))
                    })
                    .await??;
                    let path = if single {
                        format!("{account}/workers/assets/upload/{}", assets[0].0)
                    } else {
                        format!("{account}/workers/assets/upload?base64=true")
                    };
                    let url = self.endpoint.join(&path)?;
                    let bytes = assets
                        .iter()
                        .map(|(_, content, _)| content.len() as u64)
                        .sum();
                    // Retries resend these bounded, verified bytes, never a changed file on disk.
                    let result: UploadResult = self
                        .request(
                            || {
                                let request = self
                                    .client
                                    .post(url.clone())
                                    .header(AUTHORIZATION, authorization.clone())
                                    .timeout(upload_timeout(bytes));
                                if single {
                                    return Ok(request
                                        .header("Content-Type", assets[0].2)
                                        .body(assets[0].1.clone()));
                                }
                                let mut form = multipart::Form::new();
                                for (hash, content, mime) in &assets {
                                    form = form.part(
                                        hash.clone(),
                                        multipart::Part::bytes(content.clone())
                                            .file_name(hash.clone())
                                            .mime_str(mime)?,
                                    );
                                }
                                Ok(request.multipart(form))
                            },
                            true,
                        )
                        .await?;
                    super::phase(&format!(
                        "Cloudflare accepted asset batch {}/{}",
                        index + 1,
                        total
                    ));
                    Ok::<_, anyhow::Error>(result.jwt.map(zeroize::Zeroizing::new))
                }
            })
            .buffer_unordered(concurrency)
            .try_fold(
                None,
                |previous, current| async move { Ok(current.or(previous)) },
            )
            .await?;
        let token = completion.context("Cloudflare did not return an asset completion token")?;
        bearer(&token)?;
        Ok(token)
    }

    async fn verify_publication(
        &self,
        base: &Url,
        manifest: Arc<Manifest>,
        html_handling: &str,
    ) -> Result<()> {
        let samples = tokio::task::spawn_blocking(move || {
            let mut paths = vec!["/index.html".to_string()];
            // Kept assets have no local bytes to compare against.
            if let Some(path) = manifest
                .files
                .iter()
                .find(|(path, asset)| {
                    !path.ends_with(".html") && manifest.contents.contains_key(&asset.hash)
                })
                .map(|(path, _)| path)
            {
                paths.push(path.clone());
            }
            paths
                .into_iter()
                .map(|path| {
                    let (_, bytes) = manifest.read_asset(&manifest.files[&path].hash)?;
                    Ok::<_, anyhow::Error>((path, bytes))
                })
                .collect::<Result<Vec<_>>>()
        })
        .await??;
        for attempt in 0..self.verify_attempts {
            let mut verified = true;
            for (path, expected) in &samples {
                let mut url = base.clone();
                url.set_path(if path == "/index.html" && html_handling != "none" {
                    "/"
                } else {
                    path
                });
                url.query_pairs_mut()
                    .append_pair("regressify-verify", &uuid::Uuid::new_v4().to_string());
                let response = self.client.get(url).timeout(self.read_timeout).send().await;
                match response {
                    Ok(response) if response.status().is_success() => {
                        let content_type = response
                            .headers()
                            .get("Content-Type")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default();
                        let expected_type = match mime_type(path) {
                            "application/null" => "",
                            mime => mime.split(';').next().unwrap_or_default(),
                        };
                        if content_type.split(';').next() != Some(expected_type)
                            || !response_bytes(response, MAX_FILE_BYTES as usize)
                                .await
                                .is_ok_and(|bytes| bytes.as_slice() == expected.as_slice())
                        {
                            verified = false;
                        }
                    }
                    _ => verified = false,
                }
            }
            if verified {
                return Ok(());
            }
            if attempt + 1 < self.verify_attempts {
                tokio::time::sleep(self.verify_interval).await;
            }
        }
        bail!(
            "Cloudflare accepted the deployment, but the public report could not be verified; check its workers.dev route, Access policy, and deployment history before retrying"
        )
    }
}

fn single_asset_upload(token: &str) -> bool {
    token_payload(token).is_some_and(|value| value["wrangler_single_asset_uploads"] == true)
}

fn upload_concurrency(token: &str) -> usize {
    if !single_asset_upload(token) {
        return 3;
    }
    token_payload(token)
        .and_then(|value| value["edge_kv_upload_concurrency"].as_u64())
        .filter(|value| *value > 0)
        .unwrap_or(8)
        .min(8) as usize
}

fn token_payload(token: &str) -> Option<serde_json::Value> {
    // These unverified claims select upload transport and bounded concurrency, not authorization.
    token
        .split('.')
        .nth(1)
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn upload_timeout(bytes: u64) -> Duration {
    Duration::from_secs(
        180_u64
            .saturating_add(bytes.div_ceil(64 * 1024))
            .min(30 * 60),
    )
}

fn publication_error(error: anyhow::Error) -> anyhow::Error {
    let rejected = error
        .downcast_ref::<ApiFailure>()
        .is_some_and(|failure| failure.rejected);
    error.context(if rejected {
        "Cloudflare rejected publication; the Worker was not updated"
    } else {
        "Cloudflare publication outcome is uncertain; check the Worker's deployment history before retrying"
    })
}

fn network_failure(error: reqwest::Error) -> ApiFailure {
    let kind = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed (check DNS, TLS certificates, and proxy settings)"
    } else if error.is_body() || error.is_decode() {
        "response body was interrupted or invalid"
    } else {
        "request transport failed"
    };
    // Raw error sources can include URLs with proxy credentials; only report structured I/O kinds.
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    let mut detail = String::new();
    while let Some(current) = source {
        if let Some(error) = current.downcast_ref::<std::io::Error>() {
            detail = format!(": {:?}", error.kind());
        }
        source = current.source();
    }
    ApiFailure::new(format!("Cloudflare {kind}{detail}"), !error.is_builder())
}

fn bearer(token: &str) -> Result<HeaderValue> {
    ensure!(
        !token.is_empty()
            && token.len() <= 32 * 1024
            && token.bytes().all(|byte| byte.is_ascii_graphic()),
        "Cloudflare returned an invalid authorization token"
    );
    let value = zeroize::Zeroizing::new(format!("Bearer {token}"));
    let mut header = HeaderValue::from_str(&value)
        .map_err(|_| anyhow!("Invalid Cloudflare authorization token"))?;
    header.set_sensitive(true);
    Ok(header)
}

fn worker_url(worker: &str, subdomain: &str) -> Result<Url> {
    validate_public_worker_name(worker)?;
    validate_dns_label(subdomain, "account subdomain")?;
    Url::parse(&format!("https://{worker}.{subdomain}.workers.dev/"))
        .context("Invalid Cloudflare public URL")
}

fn hosting_policy(root: &Path, name: &str) -> Result<String> {
    let Some(path) = super::artifact_source(root, Path::new(name), true)? else {
        return Ok(String::new());
    };
    let bytes = super::bounded_read(
        std::fs::File::open(path)?,
        256 * 1024,
        "Cloudflare hosting policy",
    )?;
    String::from_utf8(bytes).context("Cloudflare hosting policies must be UTF-8")
}

fn cloudflare_redirects(policy: &str, html_handling: &str) -> String {
    if html_handling == "none" {
        return policy.to_owned();
    }
    policy
        .lines()
        .map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            // Workers rejects index.html proxies as redirect loops; use its canonical root instead.
            match fields.as_slice() {
                [source, "/index.html", "200"] => format!("{source} / 200"),
                _ => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn mime_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "jsonld" => "application/ld+json",
        "md" => "text/markdown; charset=utf-8",
        "vtt" => "text/vtt; charset=utf-8",
        "ics" => "text/calendar; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xhtml" => "application/xhtml+xml",
        "rss" => "application/rss+xml",
        "atom" => "application/atom+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "avif" => "image/avif",
        "apng" => "image/apng",
        "jxl" => "image/jxl",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "xml" | "xsl" => "application/xml",
        "txt" | "sh" | "ps1" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "m3u8" => "application/vnd.apple.mpegurl",
        "mpd" => "application/dash+xml",
        "glb" => "model/gltf-binary",
        "gltf" => "model/gltf+json",
        "bin" => "application/octet-stream",
        _ => "application/null",
    }
}

async fn response_bytes(
    response: Response,
    limit: usize,
) -> std::result::Result<Vec<u8>, ApiFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ApiFailure::new(
            "Cloudflare response exceeds its size limit",
            false,
        ));
    }
    let mut bytes = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(network_failure)?;
        if bytes.len() + chunk.len() > limit {
            return Err(ApiFailure::new(
                "Cloudflare response exceeds its size limit",
                false,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn api_response<Value: DeserializeOwned>(
    response: Response,
    authorizations: &[&HeaderValue],
    secrets: &[&str],
) -> std::result::Result<Value, ApiFailure> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.min(60)));
    let ray = response
        .headers()
        .get("cf-ray")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .unwrap_or_default()
        .to_string();
    let bytes = zeroize::Zeroizing::new(response_bytes(response, MAX_RESPONSE_BYTES).await?);
    let envelope: ApiEnvelope<serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|_| {
            ApiFailure::new(
                format!(
                    "Cloudflare returned an invalid API response (HTTP {})",
                    status.as_u16()
                ),
                status.is_server_error() || matches!(status.as_u16(), 408 | 429),
            )
        })?;
    if status.is_success() && envelope.success {
        let result = envelope
            .result
            .ok_or_else(|| ApiFailure::new("Cloudflare returned no API result", false))?;
        return serde_json::from_value(result)
            .map_err(|_| ApiFailure::new("Cloudflare returned a malformed API result", false));
    }
    let errors = envelope.errors.unwrap_or_default();
    let codes = errors
        .iter()
        .take(8)
        .map(|error| error.code.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let mut detail = errors
        .iter()
        .take(8)
        .map(|error| error.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    // Redact every credential used by this request, including the completion JWT in publication metadata.
    for secret in secrets.iter().copied().filter(|secret| !secret.is_empty()) {
        detail = detail.replace(secret, "[redacted]");
    }
    for authorization in authorizations {
        if let Some(token) = authorization
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("Bearer "))
        {
            detail = detail.replace(token, "[redacted]");
        }
    }
    if let Some(authorization) = authorizations.first() {
        detail = super::diagnostics::sanitize(&detail, authorization);
    }
    let hint = match status.as_u16() {
        401 | 403 => "Check the API token's Workers Scripts Write permission and selected account",
        404 => "Check the Account ID and Worker name",
        413 => "Check the 25 MiB per-file and account file-count limits",
        429 => "Cloudflare rate limit reached",
        _ => "Check the Worker's deployment history and Cloudflare account limits",
    };
    Err(ApiFailure {
        message: format!(
            "Cloudflare request failed (HTTP {}; codes: {codes}; request ID: {ray}): {detail}. {hint}",
            status.as_u16()
        ),
        retryable: status.is_server_error() || matches!(status.as_u16(), 408 | 429),
        retry_after,
        missing_worker: status.as_u16() == 404
            && !envelope.success
            && errors.len() == 1
            && errors[0].code == 10007,
        rejected: status.is_client_error()
            && status.as_u16() != 408
            && !envelope.success
            && !errors.is_empty(),
    })
}

#[derive(Clone, Debug, Serialize)]
struct Asset {
    hash: String,
    size: u64,
}

struct Manifest {
    root: PathBuf,
    files: BTreeMap<String, Asset>,
    contents: BTreeMap<String, String>,
    /// Bytes of every listed asset, kept ones included (the Worker's budget covers them).
    bytes: u64,
    /// Hashes of assets kept on the Worker without local content.
    kept: BTreeSet<String>,
    /// Paths and sizes of kept assets, which `files` and `bytes` include but `contents` does not.
    kept_files: usize,
    kept_bytes: u64,
}

impl Manifest {
    /// Rewrites the site state of this optimized manifest (rooted at the optimized copy) to
    /// describe what the Worker receives. Optimization changes image bytes and cache versions,
    /// so the staged hashes and summaries name assets that are never uploaded, and a later
    /// update keeping them would serve missing files. The rewritten state also overwrites the
    /// copy in `staged_root`, deliberately: the deployment reads it back from there to record
    /// what the site serves. A state that cannot be rewritten is left out of the upload
    /// instead, as a site without one: the next update rebuilds from local results.
    fn rewrite_optimized_state(&mut self, staged_root: &Path) -> Result<()> {
        let key = format!("/{}", super::site_state::STATE_PATH);
        let Some(old) = self.files.remove(&key) else {
            return Ok(());
        };
        if self.files.values().all(|asset| asset.hash != old.hash) {
            self.contents.remove(&old.hash);
        }
        self.bytes -= old.size;
        let bytes = match optimized_state(&self.root, &self.files) {
            Ok(bytes) => bytes,
            Err(error) => {
                super::phase(&format!(
                    "Optimized reports are published without incremental site state: {error:#}"
                ));
                return Ok(());
            }
        };
        super::phase("Updating the site state for optimized reports");
        for root in [self.root.as_path(), staged_root] {
            crate::helpers::write_file_atomically(
                &root.join(super::site_state::STATE_PATH),
                &bytes,
            )
            .context("Cannot write the optimized site state")?;
        }
        let hash = asset_hash(&bytes, super::site_state::STATE_PATH);
        self.contents
            .entry(hash.clone())
            .or_insert_with(|| super::site_state::STATE_PATH.to_owned());
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .context("Cloudflare content size overflow")?;
        self.files.insert(
            key,
            Asset {
                hash,
                size: bytes.len() as u64,
            },
        );
        Ok(())
    }

    fn prepare(
        root: &Path,
        deadline: Instant,
        byte_limit: u64,
        excludes: &[String],
        kept: &super::KeptFiles,
    ) -> Result<(Self, PreparedReport)> {
        let kept_bytes = kept
            .values()
            .try_fold(0_u64, |total, file| total.checked_add(file.size))
            .context("Kept Cloudflare asset size overflow")?;
        let file_limit = MAX_FILES
            .checked_sub(kept.len())
            .context("Cloudflare deployment exceeds its file limit")?;
        let local_limit = byte_limit
            .checked_sub(kept_bytes)
            .context("Kept suites exceed the Cloudflare content limit; replace the whole site")?;
        // Validate and select assets before optimization; only the total-byte limit is deferred.
        let manifest = Self::create_bounded(
            root,
            deadline,
            file_limit,
            u64::MAX,
            MAX_FILE_BYTES,
            excludes,
        )?;
        // Optimization rewrites every suite listed in the manifest, but kept suites have no
        // local files to rewrite.
        ensure!(
            kept.is_empty() || manifest.bytes <= local_limit,
            "The site exceeds the Cloudflare content budget, which kept suites cannot be optimized into; replace the whole site"
        );
        let prepared = optimization::prepare_files(
            &manifest.root,
            manifest.files.keys().map(|path| PathBuf::from(&path[1..])),
            local_limit,
            deadline,
        )?;
        let mut manifest = if prepared.path() == manifest.root {
            manifest
        } else {
            super::phase("Rehashing optimized report files for Cloudflare");
            let mut optimized = Self::create_bounded(
                prepared.path(),
                deadline,
                file_limit,
                local_limit,
                MAX_FILE_BYTES,
                excludes,
            )?;
            optimized.rewrite_optimized_state(root)?;
            optimized
        };
        manifest.keep(kept, excludes)?;
        ensure!(
            manifest.bytes <= byte_limit,
            "Cloudflare deployment exceeds its {} MiB client content limit",
            byte_limit as f64 / (1024.0 * 1024.0)
        );
        super::phase(&format!(
            "Cloudflare manifest ready: {}",
            manifest.counts().describe("assets", "hash")
        ));
        Ok((manifest, prepared))
    }

    #[cfg(test)]
    fn create(root: &Path, deadline: Instant) -> Result<Self> {
        Self::create_bounded(
            root,
            deadline,
            MAX_FILES,
            MAX_CONTENT_BYTES,
            MAX_FILE_BYTES,
            &[],
        )
    }

    fn create_bounded(
        root: &Path,
        deadline: Instant,
        file_limit: usize,
        byte_limit: u64,
        file_byte_limit: u64,
        excludes: &[String],
    ) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(root)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Cloudflare staging root must be a regular directory"
        );
        let mut manifest = Self {
            root: root.canonicalize()?,
            kept: BTreeSet::new(),
            kept_files: 0,
            kept_bytes: 0,
            files: BTreeMap::new(),
            contents: BTreeMap::new(),
            bytes: 0,
        };
        ensure!(
            super::artifact_source(&manifest.root, Path::new(".assetsignore"), true)?.is_none(),
            ".assetsignore is not supported; use the target's Cloudflare exclusion patterns"
        );
        let patterns = excludes
            .iter()
            .map(|pattern| glob::Pattern::new(pattern).context("Invalid Cloudflare exclusion glob"))
            .collect::<Result<Vec<_>>>()?;
        let mut last_update = Instant::now();
        for entry in walkdir::WalkDir::new(&manifest.root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| {
                if entry.depth() == 0 {
                    return true;
                }
                let name = entry
                    .file_name()
                    .to_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let private = matches!(
                    name.as_str(),
                    ".git" | ".wrangler" | "node_modules" | ".env" | ".dev.vars"
                ) || name.starts_with(".env.")
                    || name.starts_with(".dev.vars.");
                let relative = entry
                    .path()
                    .strip_prefix(&manifest.root)
                    .unwrap_or(entry.path());
                !private
                    && !patterns
                        .iter()
                        .any(|pattern| pattern.matches_path(relative))
            })
        {
            ensure!(
                Instant::now() < deadline,
                "Cloudflare hashing exceeded the deployment deadline"
            );
            let entry = entry?;
            let relative = entry.path().strip_prefix(&manifest.root)?;
            ensure!(
                relative
                    .components()
                    .next()
                    .is_none_or(|part| !part.as_os_str().eq_ignore_ascii_case("_worker.js")),
                "Pages server code cannot be deployed as public assets; exclude _worker.js in the target"
            );
            ensure!(
                !entry.file_type().is_symlink(),
                "Cloudflare deployment cannot contain symlinks"
            );
            if entry.file_type().is_dir() {
                continue;
            }
            ensure!(
                entry.file_type().is_file(),
                "Cloudflare deployment requires regular files"
            );
            let path = relative
                .components()
                .map(|component| {
                    component
                        .as_os_str()
                        .to_str()
                        .context("Deployment paths must be UTF-8")
                })
                .collect::<Result<Vec<_>>>()?
                .join("/");
            ensure!(
                !path.contains(['#', '?', '\\']) && !path.chars().any(char::is_control),
                "Cloudflare deployment path contains unsupported characters"
            );
            if matches!(
                path.as_str(),
                "_headers" | "_redirects" | "staticwebapp.config.json" | ".gitignore"
            ) {
                continue;
            }
            ensure!(
                manifest.files.len() < file_limit,
                "Cloudflare deployment exceeds the {file_limit}-file client limit"
            );
            let source = super::artifact_source(&manifest.root, relative, false)?
                .context("Cloudflare deployment file is unavailable")?;
            let size = std::fs::metadata(&source)?.len();
            ensure!(
                size <= file_byte_limit,
                "Cloudflare asset '{path}' exceeds the {} MiB per-file limit",
                file_byte_limit as f64 / (1024.0 * 1024.0)
            );
            manifest.bytes = manifest
                .bytes
                .checked_add(size)
                .context("Cloudflare content size overflow")?;
            ensure!(
                manifest.bytes <= byte_limit,
                "Cloudflare deployment exceeds its {} MiB client content limit",
                byte_limit as f64 / (1024.0 * 1024.0)
            );
            let bytes = read_file(&source, size)?;
            let hash = asset_hash(&bytes, &path);
            manifest
                .contents
                .entry(hash.clone())
                .or_insert_with(|| path.clone());
            manifest
                .files
                .insert(format!("/{path}"), Asset { hash, size });
            if manifest.files.len() == 1
                || manifest.files.len().is_multiple_of(50)
                || last_update.elapsed().as_secs() >= 1
            {
                super::phase(&format!(
                    "Hashed {} Cloudflare report files ({:.2} MiB)",
                    manifest.files.len(),
                    manifest.bytes as f64 / (1024.0 * 1024.0)
                ));
                last_update = Instant::now();
            }
        }
        ensure!(
            manifest.files.contains_key("/index.html"),
            "Deployment requires index.html"
        );
        Ok(manifest)
    }

    fn read_asset(&self, hash: &str) -> Result<(String, Vec<u8>)> {
        let path = self
            .contents
            .get(hash)
            .context("Cloudflare requested content outside the deployment manifest")?;
        let asset = &self.files[&format!("/{path}")];
        let source = super::artifact_source(&self.root, Path::new(path), false)?
            .context("Cloudflare deployment file is unavailable")?;
        let bytes = read_file(&source, asset.size)?;
        ensure!(
            asset_hash(&bytes, path) == hash,
            "Cloudflare deployment file changed after hashing"
        );
        Ok((path.clone(), bytes))
    }

    /// Lists assets the Worker already serves by hash. They are untrusted state from the site,
    /// so they follow local path rules, cannot be excluded or shadow staged files, and count
    /// against the content limit.
    fn keep(&mut self, kept: &super::KeptFiles, excludes: &[String]) -> Result<()> {
        let patterns = excludes
            .iter()
            .map(|pattern| glob::Pattern::new(pattern))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (path, file) in kept {
            ensure!(
                !excluded_path(&patterns, path),
                "The target's exclusions now cover files of a suite kept on this site ({path}); replace the whole site"
            );
            ensure!(
                super::site_state::valid_site_path(path)
                    && super::is_lower_hex(&file.hash, 32)
                    && file.size <= MAX_FILE_BYTES,
                "Invalid kept Cloudflare asset '{path}'"
            );
            ensure!(
                self.files
                    .insert(
                        format!("/{path}"),
                        Asset {
                            hash: file.hash.clone(),
                            size: file.size,
                        },
                    )
                    .is_none(),
                "A kept suite file collides with a staged file: {path}"
            );
            self.bytes = self
                .bytes
                .checked_add(file.size)
                .context("Cloudflare content size overflow")?;
            self.kept.insert(file.hash.clone());
            self.kept_files += 1;
            self.kept_bytes = self
                .kept_bytes
                .checked_add(file.size)
                .context("Kept Cloudflare content size overflow")?;
        }
        Ok(())
    }

    /// Analytics and staging facts for this final manifest; `original_local` is the local
    /// content size before optimization.
    fn stats(&self, original_local: u64, root: &Path) -> super::DeploymentStats {
        let state = format!("/{}", super::site_state::STATE_PATH);
        super::DeploymentStats {
            file_count: self.files.len() as u64,
            // Kept assets count in both, as they do in the published site; optimization only
            // ever reduces the local part.
            original_bytes: original_local.saturating_add(self.kept_bytes),
            total_bytes: self.bytes,
            state_dropped: root.join(super::site_state::STATE_PATH).is_file()
                && !self.files.contains_key(&state),
        }
    }

    fn counts(&self) -> super::ManifestCounts {
        super::ManifestCounts {
            staged_files: self.files.len() - self.kept_files,
            staged_bytes: self.bytes - self.kept_bytes,
            unique: self.contents.len(),
            kept_files: self.kept_files,
            kept_bytes: self.kept_bytes,
        }
    }

    fn validate_buckets(&self, buckets: &[Vec<String>]) -> Result<Vec<u64>> {
        let mut seen = BTreeSet::new();
        let mut sizes = Vec::with_capacity(buckets.len());
        for bucket in buckets {
            ensure!(
                !bucket.is_empty(),
                "Cloudflare returned an empty upload bucket"
            );
            let mut bytes = 0_u64;
            for hash in bucket {
                ensure!(
                    hash.len() == 32 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "Cloudflare returned an invalid asset hash"
                );
                // Kept content the Worker no longer stores cannot be uploaded from here; failing
                // before any upload or publication leaves the site untouched.
                ensure!(
                    self.contents.contains_key(hash) || !self.kept.contains(hash),
                    "Cloudflare no longer has the content of a suite kept on this site; replace the whole site"
                );
                let path = self
                    .contents
                    .get(hash)
                    .context("Cloudflare requested content outside the deployment manifest")?;
                ensure!(
                    seen.insert(hash),
                    "Cloudflare requested duplicate upload content"
                );
                bytes = bytes
                    .checked_add(self.files[&format!("/{path}")].size)
                    .context("Cloudflare bucket size overflow")?;
                ensure!(
                    bytes <= MAX_BUCKET_BYTES,
                    "Cloudflare upload bucket exceeds its memory limit"
                );
            }
            sizes.push(bytes);
        }
        Ok(sizes)
    }
}

fn read_file(path: &Path, expected: u64) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).with_context(|| {
        format!(
            "Cannot open Cloudflare deployment file '{}'",
            path.display()
        )
    })?;
    ensure!(
        file.metadata()
            .with_context(|| format!(
                "Cannot inspect Cloudflare deployment file '{}'",
                path.display()
            ))?
            .is_file(),
        "Cloudflare deployment requires regular files"
    );
    let mut bytes = Vec::new();
    file.take(expected + 1)
        .read_to_end(&mut bytes)
        .with_context(|| {
            format!(
                "Cannot read Cloudflare deployment file '{}'",
                path.display()
            )
        })?;
    ensure!(
        bytes.len() as u64 == expected,
        "Cloudflare deployment file changed after hashing"
    );
    Ok(bytes)
}

/// Whether `path` or any directory above it matches an exclusion; staging prunes excluded
/// directories, so none of their files is uploaded.
pub(super) fn excluded_path(patterns: &[glob::Pattern], path: &str) -> bool {
    let mut prefix = String::new();
    path.split('/').any(|segment| {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        patterns
            .iter()
            .any(|pattern| pattern.matches_path(Path::new(&prefix)))
    })
}

/// The site state in an optimized `root`, with each suite's summary from the refreshed
/// manifest and each file's hash and size from the optimized `files`. Kept suites block
/// optimization, so every suite in the state was generated in this upload.
fn optimized_state(root: &Path, files: &BTreeMap<String, Asset>) -> Result<Vec<u8>> {
    let mut state = super::site_state::SiteState::parse(
        &std::fs::read(root.join(super::site_state::STATE_PATH))
            .context("Cannot read the staged site state")?,
    )?;
    let summaries = crate::snapshot::suite_summaries(root)?
        .into_iter()
        .map(|summary| (summary.id.clone(), summary))
        .collect::<BTreeMap<_, _>>();
    for (id, suite) in &mut state.suites {
        suite.summary = summaries
            .get(id)
            .cloned()
            .with_context(|| format!("The optimized site has no suite '{id}'"))?;
        for (path, file) in &mut suite.files {
            let asset = files
                .get(&format!("/{path}"))
                .with_context(|| format!("The optimized site has no file '{path}'"))?;
            file.hash = asset.hash.clone();
            file.size = asset.size;
        }
    }
    state.to_bytes()
}

pub(super) fn asset_hash(bytes: &[u8], path: &str) -> String {
    hash_with_content_type(bytes, path, mime_type(path))
}

fn hash_with_content_type(bytes: &[u8], path: &str, mime: &str) -> String {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    // MIME changes must invalidate cached response headers even when file bytes are unchanged.
    let mut digest = Sha256::new();
    digest.update(STANDARD.encode(bytes));
    digest.update(extension);
    digest.update(b"\0");
    digest.update(mime);
    format!("{:x}", digest.finalize())[..32].to_string()
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn cloudflare_retry_then_success_records_attempts() -> Result<()> {
        let sessions = Arc::new(AtomicUsize::new(0));
        let attempts = sessions.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let path = request.url().to_owned();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let value = if path.ends_with("/settings") {
                json!({})
            } else if path.ends_with("/workers/subdomain") {
                json!({"subdomain":"account"})
            } else if path.ends_with("/subdomain") {
                json!({"enabled":true})
            } else if path.ends_with("/assets-upload-session") {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    let _ = request.respond(
                        tiny_http::Response::from_string(
                            json!({"success":false,"errors":[{"code":1000,"message":"temporary"}]})
                                .to_string(),
                        )
                        .with_status_code(503),
                    );
                    return;
                }
                json!({"jwt":"synthetic-completion-jwt", "buckets":[]})
            } else if request.method().as_str() == "PUT" {
                json!({"id":"report-worker","has_assets":true})
            } else {
                let _ = request.respond(
                    tiny_http::Response::from_string("report").with_header(
                        tiny_http::Header::from_bytes("Content-Type", "text/html; charset=utf-8")
                            .unwrap(),
                    ),
                );
                return;
            };
            let _ = request.respond(tiny_http::Response::from_string(
                json!({"success":true,"result":value}).to_string(),
            ));
        })?;
        let root = tempfile::tempdir()?;
        let logs = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        super::super::with_deployment_journal(
            logs.path(),
            client(&site.origin).deploy(root.path(), &target()),
        )
        .await?;
        assert_eq!(sessions.load(Ordering::SeqCst), 2);
        super::super::diagnostics::assert_retry_journal(
            logs.path(),
            super::super::DeploymentProvider::CloudflareWorkers,
            &[TOKEN, "synthetic-completion-jwt"],
        )
    }

    use super::*;
    use serde_json::json;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    const TOKEN: &str = "synthetic-cloudflare-token";

    fn client(origin: &str) -> CloudflareClient {
        let mut client = CloudflareClient::new(TOKEN).unwrap();
        client.endpoint = Url::parse(&format!("{origin}/api/")).unwrap();
        client.backoff = Duration::from_millis(1);
        client.verify_interval = Duration::from_millis(1);
        client.verify_attempts = 2;
        client.verification_url = Some(Url::parse(&format!("{origin}/")).unwrap());
        client
    }

    #[tokio::test]
    async fn local_preparation_errors_identify_the_directory_before_network_access() -> Result<()> {
        let requests = Arc::new(AtomicUsize::new(0));
        let captured = requests.clone();
        let site = crate::test::browser_tests::Site::new(move |request| {
            captured.fetch_add(1, Ordering::SeqCst);
            let _ = request.respond(tiny_http::Response::empty(500));
        })?;
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("missing-report");
        let error = client(&site.origin)
            .deploy(&root, &target())
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("Cannot prepare local Cloudflare deployment files"),
            "{message}"
        );
        assert!(message.contains(&root.display().to_string()), "{message}");
        assert!(!message.contains(TOKEN), "{message}");
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        assert!(!root.exists());
        Ok(())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn local_preparation_errors_identify_locked_files_before_network_access() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;

        let requests = Arc::new(AtomicUsize::new(0));
        let captured = requests.clone();
        let site = crate::test::browser_tests::Site::new(move |request| {
            captured.fetch_add(1, Ordering::SeqCst);
            let _ = request.respond(tiny_http::Response::empty(500));
        })?;
        let root = tempfile::tempdir()?;
        let path = root.path().join("index.html");
        std::fs::write(&path, "report")?;
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)?;
        let result = client(&site.origin).deploy(root.path(), &target()).await;
        drop(locked);

        let message = format!("{:#}", result.unwrap_err());
        assert!(
            message.contains("Cannot prepare local Cloudflare deployment files"),
            "{message}"
        );
        assert!(message.contains("index.html"), "{message}");
        assert!(!message.contains(TOKEN), "{message}");
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(&path)?, b"report");
        Ok(())
    }

    #[tokio::test]
    async fn private_and_preserved_routes_never_publish_unrequested_public_access() -> Result<()> {
        for (requested, exists, initially_enabled) in [
            (Some(false), false, false),
            (None, false, false),
            (None, true, false),
            (None, true, true),
        ] {
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "report")?;
            let reads = Arc::new(AtomicUsize::new(0));
            let writes = Arc::new(AtomicUsize::new(0));
            let publications = Arc::new(AtomicUsize::new(0));
            let captured_reads = reads.clone();
            let captured_writes = writes.clone();
            let captured_publications = publications.clone();
            let site = crate::test::browser_tests::Site::new(move |mut request| {
                let path = request.url().to_owned();
                let method = request.method().as_str().to_owned();
                let result = if path.ends_with("/workers/subdomain") {
                    json!({"subdomain":"account"})
                } else if path.ends_with("/subdomain") && method == "GET" {
                    let initial = captured_reads.fetch_add(1, Ordering::SeqCst) == 0;
                    if !exists {
                        let _ = request.respond(
                            tiny_http::Response::from_string(
                                json!({"success":false,"errors":[{"code":10007}]}).to_string(),
                            )
                            .with_status_code(404),
                        );
                        return;
                    }
                    json!({"enabled":initial && initially_enabled,"previews_enabled":true})
                } else if path.ends_with("/subdomain") && method == "POST" {
                    assert_eq!(captured_publications.load(Ordering::SeqCst), 1);
                    let body: serde_json::Value =
                        serde_json::from_reader(request.as_reader()).unwrap();
                    assert_eq!(body, json!({"enabled":false,"previews_enabled":false}));
                    captured_writes.fetch_add(1, Ordering::SeqCst);
                    body
                } else if path.ends_with("/assets-upload-session") {
                    json!({"jwt":"cached","buckets":[]})
                } else {
                    assert_eq!(method, "PUT");
                    assert!(path.ends_with("/workers/scripts/report-worker"));
                    captured_publications.fetch_add(1, Ordering::SeqCst);
                    json!({"id":"report-worker","has_assets":true})
                };
                let _ = request.respond(tiny_http::Response::from_string(
                    json!({"success":true,"result":result}).to_string(),
                ));
            })?;
            let settings = CloudflareTarget {
                workers_dev: requested,
                ..target()
            };
            assert!(
                client(&site.origin)
                    .deploy(root.path(), &settings)
                    .await?
                    .is_none()
            );
            assert_eq!(publications.load(Ordering::SeqCst), 1);
            assert_eq!(
                writes.load(Ordering::SeqCst),
                usize::from(requested.is_some() || !exists)
            );
            assert_eq!(
                reads.load(Ordering::SeqCst),
                if requested.is_some() {
                    0
                } else if exists {
                    2
                } else {
                    1
                }
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn preserve_routing_rejects_permission_and_malformed_responses_without_writes()
    -> Result<()> {
        for (status, response) in [
            (403, json!({"success":false,"errors":[{"code":10007}]})),
            (404, json!({"success":false,"errors":[{"code":10000}]})),
            (
                404,
                json!({"success":false,"errors":[{"code":10007},{"code":10000}]}),
            ),
            (200, json!({"success":true,"result":{"enabled":false}})),
            (
                200,
                json!({"success":true,"result":{"enabled":"false","previews_enabled":false}}),
            ),
        ] {
            let site = crate::test::browser_tests::Site::new(move |request| {
                assert_eq!(request.method().as_str(), "GET");
                assert!(request.url().ends_with("/scripts/report-worker/subdomain"));
                let _ = request.respond(
                    tiny_http::Response::from_string(response.to_string()).with_status_code(status),
                );
            })?;
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "report")?;
            assert!(
                client(&site.origin)
                    .deploy(
                        root.path(),
                        &CloudflareTarget {
                            workers_dev: None,
                            ..target()
                        }
                    )
                    .await
                    .is_err()
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn publication_and_route_identity_failures_are_not_reported_as_success() -> Result<()> {
        for mode in ["wrong-worker", "no-assets", "route-disabled", "route-error"] {
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "report")?;
            let publications = Arc::new(AtomicUsize::new(0));
            let captured = publications.clone();
            let site = crate::test::browser_tests::Site::new(move |request| {
                let path = request.url();
                let result = if path.ends_with("/workers/subdomain") {
                    json!({"subdomain":"account"})
                } else if path.ends_with("/assets-upload-session") {
                    json!({"jwt":"cached","buckets":[]})
                } else if request.method().as_str() == "PUT" {
                    captured.fetch_add(1, Ordering::SeqCst);
                    json!({"id":if mode == "wrong-worker" { "other" } else { "report-worker" },"has_assets":mode != "no-assets"})
                } else {
                    assert!(path.ends_with("/subdomain"));
                    assert!(mode.starts_with("route-"));
                    if mode == "route-error" {
                        let _ = request.respond(
                            tiny_http::Response::from_string(
                                json!({"success":false,"errors":[{"code":10000}]}).to_string(),
                            )
                            .with_status_code(403),
                        );
                        return;
                    }
                    json!({"enabled":false,"previews_enabled":false})
                };
                let _ = request.respond(tiny_http::Response::from_string(
                    json!({"success":true,"result":result}).to_string(),
                ));
            })?;
            assert!(
                client(&site.origin)
                    .deploy(root.path(), &target())
                    .await
                    .is_err()
            );
            assert_eq!(publications.load(Ordering::SeqCst), 1);
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "creates, updates, and deletes a unique temporary Worker; requires explicit REGRESSIFY_ALLOW_CLOUDFLARE_TEMP=1 plus Cloudflare API token and Account ID"]
    async fn live_create_update_and_delete_temporary_worker() -> Result<()> {
        ensure!(
            std::env::var("REGRESSIFY_ALLOW_CLOUDFLARE_TEMP").as_deref() == Ok("1"),
            "Explicit temporary Worker test authorization is required"
        );
        let token = zeroize::Zeroizing::new(
            std::env::var("CLOUDFLARE_API_TOKEN").context("Set CLOUDFLARE_API_TOKEN")?,
        );
        let target = CloudflareTarget {
            account_id: std::env::var("CLOUDFLARE_ACCOUNT_ID")
                .context("Set CLOUDFLARE_ACCOUNT_ID")?,
            worker_name: format!("regressify-verify-{}", uuid::Uuid::new_v4().simple()),
            ..CloudflareTarget::default()
        };
        target.validate()?;
        let client = CloudflareClient::new(&token)?;
        let worker = format!(
            "accounts/{}/workers/scripts/{}",
            target.account_id, target.worker_name
        );
        let absent = client
            .get::<WorkerRoute>(&format!("{worker}/subdomain"))
            .await
            .err()
            .is_some_and(|error| {
                error
                    .downcast_ref::<ApiFailure>()
                    .is_some_and(|failure| failure.missing_worker)
            });
        ensure!(
            absent,
            "Could not confirm that the temporary Worker is absent; no publication attempted"
        );
        let verification = async {
            let root = tempfile::tempdir()?;
            std::fs::write(
                root.path().join("index.html"),
                "<!doctype html><title>Worker check</title><h1>Initial deployment</h1>",
            )?;
            std::fs::write(
                root.path().join("captions.vtt"),
                "WEBVTT\n\n00:00.000 --> 00:01.000\nVerified\n",
            )?;
            std::fs::write(root.path().join("data.json"), "{\"verified\":true}")?;
            std::fs::write(root.path().join("opaque.unknown"), "opaque bytes")?;
            let mut published = None;
            for attempt in 0..3 {
                if attempt == 1 {
                    std::fs::write(
                        root.path().join("index.html"),
                        "<!doctype html><title>Worker check</title><h1>Updated deployment</h1>",
                    )?;
                }
                let url = client
                    .deploy(root.path(), &target)
                    .await?
                    .context("Expected production publication URL")?;
                if let Some(previous) = &published {
                    ensure!(previous == &url, "Worker URL changed between deployments");
                }
                published = Some(url.clone());
                let route: WorkerRoute = client.get(&format!("{worker}/subdomain")).await?;
                ensure!(
                    route.enabled,
                    "Production workers.dev route was not enabled"
                );
                let manifest = Manifest::create(root.path(), deadline())?;
                for path in manifest.files.keys() {
                    let mut asset_url = Url::parse(&url)?;
                    asset_url.set_path(if path == "/index.html" { "/" } else { path });
                    asset_url
                        .query_pairs_mut()
                        .append_pair("verify", &uuid::Uuid::new_v4().to_string());
                    let response = client
                        .client
                        .get(asset_url)
                        .send()
                        .await
                        .map_err(network_failure)?;
                    ensure!(
                        response.status().is_success(),
                        "Published test asset is unavailable"
                    );
                    let expected_type = mime_type(path);
                    if expected_type == "application/null" {
                        ensure!(
                            response.headers().get("Content-Type").is_none(),
                            "Unknown asset MIME omission was not preserved"
                        );
                    } else {
                        ensure!(
                            response
                                .headers()
                                .get("Content-Type")
                                .and_then(|value| value.to_str().ok())
                                .and_then(|value| value.split(';').next())
                                == expected_type.split(';').next(),
                            "Published test asset has the wrong MIME type"
                        );
                    }
                    let expected = manifest.read_asset(&manifest.files[path].hash)?.1;
                    ensure!(
                        response_bytes(response, MAX_FILE_BYTES as usize).await? == expected,
                        "Published test asset differs from its source"
                    );
                }
                println!("Temporary Worker deployment {}/3 verified", attempt + 1);
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let delete_url = client.endpoint.join(&worker)?;
        let deletion = client
            .request::<serde_json::Value>(
                || {
                    Ok(client
                        .client
                        .delete(delete_url.clone())
                        .header(AUTHORIZATION, client.authorization.clone())
                        .timeout(client.read_timeout))
                },
                true,
            )
            .await;
        let removed = client
            .get::<WorkerRoute>(&format!("{worker}/subdomain"))
            .await
            .err()
            .is_some_and(|error| {
                error
                    .downcast_ref::<ApiFailure>()
                    .is_some_and(|failure| failure.missing_worker)
            });
        if !removed {
            if let Err(error) = &verification {
                eprintln!("Temporary Worker verification failed: {error:#}");
            }
            deletion.context("Temporary Worker cleanup failed")?;
            bail!(
                "Temporary Worker removal could not be confirmed: {}",
                target.worker_name
            );
        }
        println!("Temporary Worker cleanup confirmed");
        verification
    }

    #[tokio::test]
    async fn publication_diagnostics_distinguish_rejection_from_uncertain_outcomes() -> Result<()> {
        for (status, valid, rejected) in [
            (400, true, true),
            (403, true, true),
            (429, true, true),
            (408, true, false),
            (503, true, false),
            (200, false, false),
            (400, false, false),
        ] {
            let count = Arc::new(AtomicUsize::new(0));
            let captured = count.clone();
            let site = crate::test::browser_tests::Site::new(move |request| {
                captured.fetch_add(1, Ordering::SeqCst);
                let body = if valid {
                    json!({"success":false,"errors":[{"code":10016,"message":"Invalid Worker"}]})
                        .to_string()
                } else {
                    "<html>proxy error</html>".into()
                };
                let _ = request
                    .respond(tiny_http::Response::from_string(body).with_status_code(status));
            })?;
            let client = client(&site.origin);
            let error = client
                .request::<serde_json::Value>(
                    || Ok(client.client.put(client.endpoint.clone())),
                    false,
                )
                .await
                .err()
                .context("Expected publication failure")?;
            let error = publication_error(error).to_string();
            assert_eq!(error.contains("rejected publication"), rejected, "{error}");
            assert_eq!(error.contains("uncertain"), !rejected, "{error}");
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
        Ok(())
    }

    #[test]
    fn asset_hashes_invalidate_mime_changes_and_cover_static_web_formats() {
        assert_eq!(asset_hash(b"", "empty"), "3a198499e4549a4ccc0f62676814bb26");
        assert_ne!(
            asset_hash(b"WEBVTT\n", "captions.vtt"),
            hash_with_content_type(b"WEBVTT\n", "captions.vtt", "application/octet-stream")
        );
        for (extension, expected) in [
            ("htm", "text/html; charset=utf-8"),
            ("CJS", "text/javascript; charset=utf-8"),
            ("map", "application/json"),
            ("webmanifest", "application/manifest+json"),
            ("jsonld", "application/ld+json"),
            ("vtt", "text/vtt; charset=utf-8"),
            ("avif", "image/avif"),
            ("wasm", "application/wasm"),
            ("pdf", "application/pdf"),
            ("opus", "audio/ogg"),
            ("mov", "video/quicktime"),
            ("m3u8", "application/vnd.apple.mpegurl"),
            ("gltf", "model/gltf+json"),
            ("bin", "application/octet-stream"),
            ("unknown", "application/null"),
        ] {
            assert_eq!(mime_type(&format!("asset.{extension}")), expected);
        }
    }

    #[tokio::test]
    async fn single_asset_uploads_use_raw_bytes_and_keep_earlier_completion_tokens() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        std::fs::write(root.path().join("style.css"), "body {}")?;
        let manifest = Arc::new(Manifest::create(
            root.path(),
            Instant::now() + Duration::from_secs(30),
        )?);
        let expected = manifest.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let captured = count.clone();
        let token = format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(
                br#"{"wrangler_single_asset_uploads":true,"edge_kv_upload_concurrency":1}"#
            )
        );
        let expected_token = token.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            assert_eq!(request.method().as_str(), "POST");
            let hash = request
                .url()
                .strip_prefix("/api/account/workers/assets/upload/")
                .unwrap();
            let (path, bytes) = expected.read_asset(hash).unwrap();
            assert!(request.headers().iter().any(|header| {
                header.field.equiv("Content-Type") && header.value.as_str() == mime_type(&path)
            }));
            assert!(request.headers().iter().any(|header| {
                header.field.equiv("Authorization")
                    && header.value.as_str() == format!("Bearer {expected_token}")
            }));
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body).unwrap();
            assert_eq!(body, bytes);
            let result = if captured.fetch_add(1, Ordering::SeqCst) == 0 {
                json!({"jwt":"raw-completion"})
            } else {
                json!({})
            };
            let _ = request.respond(tiny_http::Response::from_string(
                json!({"success":true,"result":result}).to_string(),
            ));
        })?;
        let buckets = vec![manifest.contents.keys().cloned().collect()];
        let completion = client(&site.origin)
            .upload_assets_bounded(manifest, buckets, "account", bearer(&token)?, 1, 7)
            .await?;
        assert_eq!(completion.as_str(), "raw-completion");
        assert_eq!(count.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[test]
    fn upload_mode_concurrency_and_timeouts_are_bounded() {
        for token in [
            "",
            "opaque-token",
            "header.invalid.signature",
            "header.e30.signature",
        ] {
            assert!(!single_asset_upload(token));
            assert_eq!(upload_concurrency(token), 3);
        }
        for claim in [json!(true), json!(false), json!("true"), json!(null)] {
            for (hint, expected) in [
                (json!(null), 8),
                (json!(0), 8),
                (json!(1), 1),
                (json!(4), 4),
                (json!(99), 8),
                (json!(-1), 8),
            ] {
                let token = format!(
                    "header.{}.signature",
                    URL_SAFE_NO_PAD.encode(
                        serde_json::to_vec(&json!({
                            "wrangler_single_asset_uploads":claim,
                            "edge_kv_upload_concurrency":hint,
                        }))
                        .unwrap()
                    )
                );
                assert_eq!(single_asset_upload(&token), claim == true);
                assert_eq!(
                    upload_concurrency(&token),
                    if claim == true { expected } else { 3 }
                );
            }
        }
        assert_eq!(upload_timeout(0), Duration::from_secs(180));
        assert_eq!(upload_timeout(64 * 1024), Duration::from_secs(181));
        assert_eq!(upload_timeout(u64::MAX), Duration::from_secs(1800));
    }

    #[tokio::test]
    async fn missing_worker_is_created_and_its_public_route_is_enabled() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        let published = Arc::new(AtomicUsize::new(0));
        let routed = Arc::new(AtomicUsize::new(0));
        let captured_published = published.clone();
        let captured_routed = routed.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let path = request.url().to_owned();
            let method = request.method().as_str().to_owned();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let value = if path.ends_with("/workers/subdomain") {
                json!({"subdomain":"account"})
            } else if path.ends_with("/settings")
                || (path.ends_with("/subdomain") && method == "GET")
            {
                let _ = request.respond(
                    tiny_http::Response::from_string(
                        json!({"success":false,"errors":[{"code":10007,"message":"Worker not found"}]}).to_string(),
                    )
                    .with_status_code(404),
                );
                return;
            } else if path.ends_with("/assets-upload-session") {
                json!({"jwt":"cached-completion-token","buckets":[]})
            } else if method == "PUT" {
                assert!(path.ends_with("/workers/scripts/report-worker"));
                assert!(body.contains("cached-completion-token"));
                captured_published.fetch_add(1, Ordering::SeqCst);
                json!({"id":"report-worker","has_assets":true})
            } else if path.ends_with("/subdomain") && method == "POST" {
                assert_eq!(captured_published.load(Ordering::SeqCst), 1);
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&body).unwrap(),
                    json!({"enabled":true})
                );
                captured_routed.fetch_add(1, Ordering::SeqCst);
                json!({"enabled":true,"previews_enabled":false})
            } else {
                assert!(path.starts_with("/?regressify-verify="));
                assert_eq!(captured_routed.load(Ordering::SeqCst), 1);
                let _ = request.respond(
                    tiny_http::Response::from_string("report").with_header(
                        tiny_http::Header::from_bytes("Content-Type", "text/html; charset=utf-8")
                            .unwrap(),
                    ),
                );
                return;
            };
            let _ = request.respond(tiny_http::Response::from_string(
                json!({"success":true,"result":value}).to_string(),
            ));
        })?;
        assert_eq!(
            client(&site.origin).deploy(root.path(), &target()).await?,
            Some(format!("{}/", site.origin))
        );
        assert_eq!(published.load(Ordering::SeqCst), 1);
        assert_eq!(routed.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn cached_deployment_publishes_metadata_without_uploading_assets() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        std::fs::write(root.path().join("_headers"), "/*\n  X-Robots-Tag: noindex")?;
        std::fs::write(root.path().join("_redirects"), "/suites/* /index.html 200")?;
        let methods = Arc::new(Mutex::new(Vec::new()));
        let captured = methods.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let path = request.url().to_owned();
            let method = request.method().as_str().to_owned();
            captured
                .lock()
                .unwrap()
                .push((method.clone(), path.clone()));
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let value = if path.ends_with("/settings") {
                json!({})
            } else if path.ends_with("/workers/subdomain") {
                json!({"subdomain":"account"})
            } else if path.ends_with("/subdomain") {
                json!({"enabled":true})
            } else if path.ends_with("/assets-upload-session") {
                let manifest: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(manifest["manifest"].as_object().unwrap().len(), 1);
                assert_eq!(manifest["manifest"]["/index.html"]["size"], 6);
                json!({"jwt":"cached-completion-token","buckets":[]})
            } else if method == "PUT" {
                assert!(body.contains("cached-completion-token"));
                assert!(body.contains("X-Robots-Tag: noindex"));
                assert!(body.contains("/suites/* / 200"));
                assert!(body.contains("\"not_found_handling\":\"none\""));
                assert!(!body.contains("main_module"));
                json!({"id":"report-worker","has_assets":true})
            } else {
                assert!(path.starts_with("/?regressify-verify="));
                assert!(
                    !request
                        .headers()
                        .iter()
                        .any(|header| header.field.equiv("Authorization"))
                );
                let _ = request.respond(
                    tiny_http::Response::from_string("report").with_header(
                        tiny_http::Header::from_bytes("Content-Type", "text/html; charset=utf-8")
                            .unwrap(),
                    ),
                );
                return;
            };
            let _ = request.respond(tiny_http::Response::from_string(
                json!({"success":true,"result":value}).to_string(),
            ));
        })?;
        let mut stats = super::super::DeploymentStats::default();
        assert_eq!(
            client(&site.origin)
                .deploy_with_stats(
                    root.path(),
                    &target(),
                    &super::super::KeptFiles::new(),
                    &mut stats,
                )
                .await?,
            Some(format!("{}/", site.origin))
        );
        assert_eq!(stats.file_count, 1);
        assert_eq!(stats.original_bytes, 6);
        assert_eq!(stats.total_bytes, 6);
        let methods = methods.lock().unwrap();
        assert_eq!(
            methods
                .iter()
                .filter(|(method, _)| method == "POST")
                .count(),
            2
        );
        assert_eq!(
            methods.iter().filter(|(method, _)| method == "PUT").count(),
            1
        );
        assert_eq!(methods.len(), 5);
        Ok(())
    }

    #[tokio::test]
    async fn azure_access_policies_are_rejected_before_api_requests() -> Result<()> {
        let requests = Arc::new(AtomicUsize::new(0));
        let captured = requests.clone();
        let site = crate::test::browser_tests::Site::new(move |request| {
            captured.fetch_add(1, Ordering::SeqCst);
            let _ = request.respond(
                tiny_http::Response::from_string(
                    json!({"success":false,"errors":[{"code":10000}]}).to_string(),
                )
                .with_status_code(403),
            );
        })?;
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        for policy in [
            json!({"auth":{}}),
            json!({"networking":{}}),
            json!({"routes":[{"route":"/*","allowedRoles":["authenticated"]}]}),
        ] {
            std::fs::write(
                root.path().join("staticwebapp.config.json"),
                serde_json::to_vec(&policy)?,
            )?;
            let error = client(&site.origin)
                .deploy(root.path(), &target())
                .await
                .err()
                .context("Expected restriction rejection")?;
            assert!(
                error.to_string().contains("Azure access restrictions"),
                "{error}"
            );
            assert_eq!(requests.load(Ordering::SeqCst), 0);
        }
        Ok(())
    }

    #[tokio::test]
    async fn invalid_routes_and_unknown_hashes_fail_before_publication() -> Result<()> {
        for mode in ["unsafe-subdomain", "unknown-hash"] {
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "report")?;
            let writes = Arc::new(AtomicUsize::new(0));
            let captured = writes.clone();
            let site = crate::test::browser_tests::Site::new(move |request| {
                let path = request.url();
                if request.method().as_str() == "PUT" || path.contains("assets/upload?") {
                    captured.fetch_add(1, Ordering::SeqCst);
                }
                let value = if path.ends_with("/settings") {
                    json!({})
                } else if path.ends_with("/workers/subdomain") {
                    json!({"subdomain":if mode == "unsafe-subdomain" { "attacker.invalid/" } else { "account" }})
                } else if path.ends_with("/subdomain") {
                    json!({"enabled":mode != "disabled"})
                } else {
                    json!({"jwt":"session-token","buckets":[["0".repeat(32)]]})
                };
                let _ = request.respond(tiny_http::Response::from_string(
                    json!({"success":true,"result":value}).to_string(),
                ));
            })?;
            assert!(
                client(&site.origin)
                    .deploy(root.path(), &target())
                    .await
                    .is_err()
            );
            assert_eq!(writes.load(Ordering::SeqCst), 0);
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "publishes CLOUDFLARE_REPORT_ROOT twice; requires API token, Account ID, Worker name, suite IDs and expected URL; replaces Worker assets"]
    async fn live_publish_cloudflare_reports() -> Result<()> {
        let token = zeroize::Zeroizing::new(
            std::env::var("CLOUDFLARE_API_TOKEN").context("Set CLOUDFLARE_API_TOKEN")?,
        );
        let target = CloudflareTarget {
            account_id: std::env::var("CLOUDFLARE_ACCOUNT_ID")
                .context("Set CLOUDFLARE_ACCOUNT_ID")?,
            worker_name: std::env::var("CLOUDFLARE_WORKER_NAME")
                .context("Set CLOUDFLARE_WORKER_NAME")?,
            ..CloudflareTarget::default()
        };
        target.validate()?;
        let expected_url = std::env::var("CLOUDFLARE_EXPECTED_URL")
            .context("Set CLOUDFLARE_EXPECTED_URL to authorize the destination")?;
        let output = PathBuf::from(
            std::env::var_os("CLOUDFLARE_REPORT_ROOT").context("Set CLOUDFLARE_REPORT_ROOT")?,
        );
        let suites = std::env::var("CLOUDFLARE_SUITE_IDS")
            .context("Set CLOUDFLARE_SUITE_IDS to comma-separated selected suite IDs")?
            .split(',')
            .map(|value| value.trim().to_string())
            .collect::<Vec<_>>();
        for suite in &suites {
            super::super::validate_suite_id(suite)?;
        }
        let client = CloudflareClient::new(&token)?;
        let subdomain: AccountSubdomain = client
            .get(&format!("accounts/{}/workers/subdomain", target.account_id))
            .await?;
        ensure!(
            worker_url(&target.worker_name, &subdomain.subdomain)?.as_str() == expected_url,
            "Cloudflare target does not match the explicitly authorized public URL"
        );
        let workspace =
            crate::config::load_workspace_config(Path::new(env!("CARGO_MANIFEST_DIR")))?;
        crate::snapshot::generate_site(
            &output,
            &suites,
            &crate::cli::args::SnapshotArgs {
                test_suite: suites.clone(),
                title: None,
                heading: None,
                subheading: None,
            },
            &workspace,
        )?;
        let directory = tempfile::tempdir()?;
        let staging = directory.path().join("report");
        super::super::stage_reports(&output, &suites, &staging)?;
        let manifest = Manifest::create(&staging, deadline())?;
        let mut published = String::new();
        for attempt in 1..=2 {
            published = client
                .deploy(&staging, &target)
                .await?
                .context("Expected a public Cloudflare deployment URL")?;
            ensure!(
                published == expected_url,
                "Published URL differs from the authorized destination"
            );
            println!("Live Cloudflare deployment {attempt}/2 verified: {published}");
        }
        let base = Url::parse(&published)?;
        stream::iter(manifest.files.iter())
            .map(|(path, asset)| {
                let client = &client.client;
                let mut url = base.clone();
                async move {
                    url.set_path(if path == "/index.html" { "/" } else { path });
                    let response = client
                        .get(url)
                        .timeout(Duration::from_secs(120))
                        .send()
                        .await
                        .map_err(|_| {
                            anyhow!("Cannot retrieve published Cloudflare report asset")
                        })?;
                    ensure!(
                        response.status().is_success(),
                        "A published Cloudflare asset is unavailable"
                    );
                    ensure!(
                        response
                            .headers()
                            .get("X-Content-Type-Options")
                            .and_then(|value| value.to_str().ok())
                            == Some("nosniff"),
                        "Published security headers are missing"
                    );
                    if path.ends_with(".png")
                        || path.ends_with(".json")
                        || path.ends_with(".js")
                        || path.ends_with(".css")
                    {
                        ensure!(
                            response
                                .headers()
                                .get("Cache-Control")
                                .and_then(|value| value.to_str().ok())
                                .is_some_and(|value| value.contains("immutable")),
                            "Published asset cache policy is missing"
                        );
                    }
                    let bytes = response_bytes(response, MAX_FILE_BYTES as usize).await?;
                    ensure!(
                        bytes.len() as u64 == asset.size && asset_hash(&bytes, path) == asset.hash,
                        "Published asset differs from the staged report"
                    );
                    Ok::<_, anyhow::Error>(())
                }
            })
            .buffer_unordered(4)
            .try_collect::<Vec<_>>()
            .await?;
        for suite in &suites {
            let mut url = base.clone();
            url.path_segments_mut()
                .map_err(|_| anyhow!("Invalid public URL"))?
                .extend(["suites", suite]);
            let response = client
                .client
                .get(url)
                .send()
                .await
                .map_err(|_| anyhow!("Cannot retrieve suite deep link"))?;
            ensure!(
                response.status().is_success(),
                "Suite deep link is unavailable"
            );
            let bytes = response_bytes(response, MAX_FILE_BYTES as usize).await?;
            ensure!(
                asset_hash(&bytes, "index.html") == manifest.files["/index.html"].hash,
                "Suite deep link did not serve the report application"
            );
        }
        for path in [
            "_snapshot/data/missing.json",
            "_snapshot/images/missing.png",
            "_headers",
            "_redirects",
        ] {
            let response = client
                .client
                .get(base.join(path)?)
                .send()
                .await
                .map_err(|_| anyhow!("Cannot verify missing-asset behavior"))?;
            ensure!(
                response.status() == reqwest::StatusCode::NOT_FOUND,
                "Missing assets or hosting policies must return 404"
            );
        }
        println!(
            "Verified {} public report assets, suite deep links, security/cache headers, and missing-file 404s",
            manifest.files.len()
        );
        Ok(())
    }

    #[tokio::test]
    async fn upload_batches_use_session_authorization_and_verified_base64() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        let manifest = Arc::new(Manifest::create(root.path(), deadline())?);
        let hash = manifest.files["/index.html"].hash.clone();
        let expected_hash = hash.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = calls.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            assert_eq!(
                request.url(),
                "/api/accounts/test/workers/assets/upload?base64=true"
            );
            assert_eq!(request.method().as_str(), "POST");
            assert!(
                request
                    .headers()
                    .iter()
                    .any(|header| header.field.equiv("Authorization")
                        && header.value.as_str() == "Bearer upload-session-token")
            );
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert!(body.contains(&expected_hash));
            assert!(body.contains("cmVwb3J0"));
            assert!(body.contains("text/html; charset=utf-8"));
            let attempt = captured.fetch_add(1, Ordering::SeqCst);
            let response = if attempt == 0 {
                tiny_http::Response::from_string(
                    json!({"success":false,"errors":[{"code":10000,"message":TOKEN}]}).to_string(),
                )
                .with_status_code(503)
            } else {
                tiny_http::Response::from_string(
                    json!({"success":true,"result":{"jwt":"completion-token"}}).to_string(),
                )
                .with_status_code(201)
            };
            let _ = request.respond(response);
        })?;
        let token = client(&site.origin)
            .upload_assets(
                manifest,
                vec![vec![hash]],
                "accounts/test",
                bearer("upload-session-token")?,
            )
            .await?;
        assert_eq!(token.as_str(), "completion-token");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn api_retries_are_bounded_and_failures_do_not_echo_secrets() -> Result<()> {
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = calls.clone();
        let site = crate::test::browser_tests::Site::new(move |request| {
            captured.fetch_add(1, Ordering::SeqCst);
            let _ = request.respond(tiny_http::Response::from_string(json!({"success":false,"errors":[{"code":10000,"message":format!("{TOKEN} upload-session-token completion-token https://private.invalid/?secret=hidden")}]}).to_string()).with_status_code(503));
        })?;
        let client = client(&site.origin);
        let url = client.endpoint.join("test")?;
        let error = client
            .request_protected::<WorkerRoute>(
                || {
                    Ok(client
                        .client
                        .get(url.clone())
                        .header(AUTHORIZATION, client.authorization.clone()))
                },
                true,
                &["upload-session-token", "completion-token"],
            )
            .await
            .err()
            .context("Expected failure")?;
        let message = format!("{error:#}");
        assert!(message.contains("10000"));
        for secret in [TOKEN, "upload-session-token", "completion-token", "hidden"] {
            assert!(!message.contains(secret));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 6);
        let url = client.endpoint.join("publish")?;
        assert!(
            client
                .request::<WorkerRoute>(|| Ok(client.client.put(url.clone())), false)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 7);
        Ok(())
    }

    #[tokio::test]
    async fn permanent_failures_and_oversized_bodies_are_not_retried() -> Result<()> {
        for mode in ["authentication", "redirect", "malformed", "oversized"] {
            let requests = Arc::new(AtomicUsize::new(0));
            let captured = requests.clone();
            let site = crate::test::browser_tests::Site::new(move |request| {
                captured.fetch_add(1, Ordering::SeqCst);
                let (status, body) = match mode {
                    "authentication" => (
                        403,
                        json!({"success":false,"errors":[{"code":10000,"message":TOKEN}]})
                            .to_string(),
                    ),
                    "redirect" => (302, String::new()),
                    "malformed" => (200, TOKEN.to_string()),
                    _ => (200, "x".repeat(MAX_RESPONSE_BYTES + 1)),
                };
                let _ = request.respond(
                    tiny_http::Response::from_string(body)
                        .with_status_code(status)
                        .with_header(
                            tiny_http::Header::from_bytes("Location", "https://untrusted.invalid/")
                                .unwrap(),
                        ),
                );
            })?;
            let error = client(&site.origin)
                .get::<WorkerRoute>("test")
                .await
                .err()
                .context("Expected failure")?;
            assert!(!format!("{error:#}").contains(TOKEN));
            assert_eq!(requests.load(Ordering::SeqCst), 1);
        }
        Ok(())
    }

    #[tokio::test]
    async fn upload_credentials_are_redacted_before_diagnostic_truncation() -> Result<()> {
        let secret = "sensitive-upload-secret".repeat(10);
        let echoed = secret.clone();
        let site = crate::test::browser_tests::Site::new(move |request| {
            let body = json!({"success":false,"errors":[{"code":10000,"message":format!("{}{echoed}", "x".repeat(2020))}]}).to_string();
            let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(403));
        })?;
        let client = client(&site.origin);
        let url = client.endpoint.join("test")?;
        let authorization = bearer(&secret)?;
        let error = client
            .request::<WorkerRoute>(
                || {
                    Ok(client
                        .client
                        .get(url.clone())
                        .header(AUTHORIZATION, authorization.clone()))
                },
                false,
            )
            .await
            .err()
            .context("Expected failure")?;
        assert!(!error.to_string().contains("sensitive-upload"));
        Ok(())
    }

    #[tokio::test]
    async fn deployment_deadline_stops_waiting_for_provider_headers() -> Result<()> {
        let requests = Arc::new(AtomicUsize::new(0));
        let captured = requests.clone();
        let site = crate::test::browser_tests::Site::new(move |request| {
            captured.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_secs(1));
            let _ = request.respond(tiny_http::Response::empty(500));
        })?;
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        let mut client = client(&site.origin);
        client.deadline = Duration::from_millis(250);
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            client.deploy(root.path(), &target()),
        )
        .await?
        .err()
        .context("Expected deadline")?;
        assert!(error.to_string().contains("exceeded its deadline"));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn publication_verifies_bytes_and_never_forwards_credentials() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        std::fs::write(root.path().join("site.css"), "body{}")?;
        let manifest = Arc::new(Manifest::create(root.path(), deadline())?);
        let paths = Arc::new(Mutex::new(Vec::new()));
        let captured = paths.clone();
        let site = crate::test::browser_tests::Site::new(move |request| {
            assert!(
                !request
                    .headers()
                    .iter()
                    .any(|header| header.field.equiv("Authorization"))
            );
            captured.lock().unwrap().push(request.url().to_owned());
            let css = request.url().starts_with("/site.css?");
            let response = tiny_http::Response::from_string(if css { "body{}" } else { "report" })
                .with_header(
                    tiny_http::Header::from_bytes(
                        "Content-Type",
                        if css {
                            "text/css; charset=utf-8"
                        } else {
                            "text/html; charset=utf-8"
                        },
                    )
                    .unwrap(),
                );
            let _ = request.respond(response);
        })?;
        client(&site.origin)
            .verify_publication(&Url::parse(&site.origin)?, manifest, "auto-trailing-slash")
            .await?;
        assert_eq!(paths.lock().unwrap().len(), 2);
        assert!(worker_url("worker", "bad.example").is_err());
        assert!(worker_url("worker", "user@attacker").is_err());
        assert_eq!(
            worker_url("report", "account")?.as_str(),
            "https://report.account.workers.dev/"
        );
        Ok(())
    }

    fn target() -> CloudflareTarget {
        CloudflareTarget {
            account_id: "0123456789abcdef0123456789abcdef".into(),
            worker_name: "report-worker".into(),
            ..CloudflareTarget::default()
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn suite_rewrites_use_cloudflare_canonical_html_without_a_catch_all() {
        assert_eq!(
            cloudflare_redirects(
                include_str!("../../snapshot/data/_redirects"),
                "auto-trailing-slash"
            ),
            "/suites/* / 200"
        );
        assert_eq!(
            cloudflare_redirects(
                "# comment\n/suites/* /index.html 200\n/old /new 301",
                "auto-trailing-slash"
            ),
            "# comment\n/suites/* / 200\n/old /new 301"
        );
        assert_eq!(cloudflare_redirects("", "auto-trailing-slash"), "");
        assert_eq!(
            cloudflare_redirects("/suites/* /index.html 200", "none"),
            "/suites/* /index.html 200"
        );
    }

    #[test]
    fn target_and_token_validation() -> Result<()> {
        target().validate()?;
        for invalid in [
            "",
            "account",
            "../0123456789abcdef0123456789ab",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert!(
                CloudflareTarget {
                    account_id: invalid.into(),
                    ..target()
                }
                .validate()
                .is_err()
            );
        }
        for invalid in [
            "",
            "../worker",
            "-worker",
            "worker-",
            "worker.name",
            "Worker",
            "worker name",
            &"a".repeat(64),
        ] {
            assert!(
                CloudflareTarget {
                    worker_name: invalid.into(),
                    ..target()
                }
                .validate()
                .is_err()
            );
        }
        validate_token("synthetic-cloudflare-token")?;
        for invalid in ["", "token with space", "token\n", &"a".repeat(1025)] {
            assert!(validate_token(invalid).is_err());
        }
        Ok(())
    }

    #[test]
    fn saved_cloudflare_options_default_to_production_and_validate() -> Result<()> {
        let saved: CloudflareTarget = serde_json::from_value(json!({
            "accountId":target().account_id,
            "workerName":"worker_name",
        }))?;
        saved.validate()?;
        assert_eq!(saved.workers_dev, Some(true));
        assert_eq!(saved.html_handling, "auto-trailing-slash");
        assert_eq!(saved.not_found_handling, "none");
        assert_eq!(saved.compatibility_date, "2026-09-25");
        for workers_dev in [None, Some(false), Some(true)] {
            let settings = CloudflareTarget {
                workers_dev,
                ..saved.clone()
            };
            assert_eq!(
                serde_json::from_value::<CloudflareTarget>(serde_json::to_value(&settings)?)?,
                settings
            );
        }
        let mut private = saved.clone();
        private.workers_dev = Some(false);
        private.worker_name = "worker-".into();
        private.validate()?;
        assert!(worker_url("worker-", "account").is_err());
        for (field, value) in [
            ("htmlHandling", json!("bad")),
            ("notFoundHandling", json!("bad")),
            ("compatibilityDate", json!("2026-02-30")),
            ("exclude", json!(["["])),
        ] {
            let mut invalid = serde_json::to_value(&saved)?;
            invalid[field] = value;
            assert!(
                serde_json::from_value::<CloudflareTarget>(invalid)?
                    .validate()
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn manifest_exclusions_prune_private_files_and_reject_pages_code() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        for name in [
            ".env",
            ".ENV.local",
            ".dev.vars",
            ".dev.vars.preview",
            "debug.map",
        ] {
            std::fs::write(root.path().join(name), "private")?;
        }
        for name in [
            ".git",
            ".wrangler",
            "node_modules",
            "private",
            ".well-known",
            "_worker.js",
        ] {
            std::fs::create_dir(root.path().join(name))?;
            std::fs::write(root.path().join(name).join("file.txt"), "content")?;
        }
        assert!(Manifest::create(root.path(), deadline()).is_err());
        let manifest = Manifest::create_bounded(
            root.path(),
            deadline(),
            MAX_FILES,
            MAX_CONTENT_BYTES,
            MAX_FILE_BYTES,
            &["private".into(), "*.map".into(), "_worker.js".into()],
        )?;
        assert_eq!(manifest.files.len(), 2);
        assert!(manifest.files.contains_key("/index.html"));
        assert!(manifest.files.contains_key("/.well-known/file.txt"));
        std::fs::write(root.path().join(".assetsignore"), "private")?;
        assert!(
            Manifest::create_bounded(
                root.path(),
                deadline(),
                MAX_FILES,
                MAX_CONTENT_BYTES,
                MAX_FILE_BYTES,
                &["_worker.js".into()]
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn manifest_deduplicates_content_and_excludes_hosting_policies() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("assets"))?;
        for path in [
            "index.html",
            "assets/copy.html",
            "assets/different.txt",
            "_headers",
            "_redirects",
            "staticwebapp.config.json",
            ".gitignore",
        ] {
            std::fs::write(root.path().join(path), "report")?;
        }
        let manifest = Manifest::create(root.path(), deadline())?;
        assert_eq!(manifest.files.len(), 3);
        assert_eq!(manifest.contents.len(), 2);
        assert_eq!(manifest.bytes, 18);
        assert_eq!(
            manifest.files["/index.html"].hash,
            manifest.files["/assets/copy.html"].hash
        );
        assert_ne!(
            manifest.files["/index.html"].hash,
            manifest.files["/assets/different.txt"].hash
        );
        assert_eq!(manifest.files["/index.html"].hash.len(), 32);
        let json = serde_json::to_value(&manifest.files)?;
        assert_eq!(json["/index.html"]["size"], 6);
        Ok(())
    }

    #[test]
    fn manifest_enforces_limits_and_rejects_changed_content() -> Result<()> {
        let root = tempfile::tempdir()?;
        assert!(Manifest::create(root.path(), deadline()).is_err());
        std::fs::write(root.path().join("index.html"), "report")?;
        assert!(Manifest::create_bounded(root.path(), deadline(), 0, 6, 6, &[]).is_err());
        assert!(Manifest::create_bounded(root.path(), deadline(), 1, 5, 6, &[]).is_err());
        assert!(Manifest::create_bounded(root.path(), deadline(), 1, 6, 5, &[]).is_err());
        assert!(Manifest::create(root.path(), Instant::now()).is_err());
        let manifest = Manifest::create_bounded(root.path(), deadline(), 1, 6, 6, &[])?;
        let hash = &manifest.files["/index.html"].hash;
        assert_eq!(manifest.read_asset(hash)?.1, b"report");
        for content in ["change", "short", "longer report"] {
            std::fs::write(root.path().join("index.html"), content)?;
            assert!(manifest.read_asset(hash).is_err());
        }
        Ok(())
    }

    #[test]
    fn exclusions_cover_files_below_excluded_directories() {
        let patterns = ["_snapshot/data/logs".to_string(), "*.map".into()]
            .iter()
            .map(|pattern| glob::Pattern::new(pattern).unwrap())
            .collect::<Vec<_>>();
        assert!(excluded_path(
            &patterns,
            "_snapshot/data/logs/op/chunk-000000.json"
        ));
        assert!(excluded_path(&patterns, "assets/app.js.map"));
        assert!(!excluded_path(&patterns, "_snapshot/data/suites/one.json"));
        assert!(!excluded_path(&patterns, "_snapshot/images/logs/a.png"));
    }

    #[test]
    fn kept_assets_are_validated_budgeted_and_block_optimization() -> Result<()> {
        let kept = |path: &str, hash: &str, size: u64| {
            super::super::KeptFiles::from([(
                path.to_owned(),
                super::super::site_state::StateFile {
                    size,
                    hash: hash.into(),
                },
            )])
        };
        let good = "0123456789abcdef0123456789abcdef";
        let root = optimization::tests::report_fixture()?;
        std::fs::create_dir_all(root.path().join("_snapshot"))?;
        std::fs::write(root.path().join("_snapshot/deployment.json"), "{}")?;
        let original = Manifest::create(root.path(), deadline())?;
        // Kept assets join the manifest and its byte count without local content.
        let limit = original.bytes * 2;
        let (manifest, _) = Manifest::prepare(
            root.path(),
            deadline(),
            limit,
            &[],
            &kept("_snapshot/images/kept/test/a.png", good, 9),
        )?;
        assert_eq!(
            manifest.files["/_snapshot/images/kept/test/a.png"].hash,
            good
        );
        assert_eq!(manifest.bytes, original.bytes + 9);
        assert!(!manifest.contents.contains_key(good));
        // Logs keep staged and kept counts apart; the kept asset has no local content.
        let counts = manifest.counts();
        assert_eq!(
            (counts.staged_files, counts.staged_bytes),
            (original.files.len(), original.bytes)
        );
        assert_eq!((counts.kept_files, counts.kept_bytes), (1, 9));
        assert_eq!(counts.unique, original.contents.len());
        // Without optimization, analytics count kept assets in both sizes, so they agree.
        let stats = manifest.stats(original.bytes, root.path());
        assert_eq!(stats.file_count, original.files.len() as u64 + 1);
        assert_eq!(stats.original_bytes, stats.total_bytes);
        assert_eq!(stats.total_bytes, original.bytes + 9);
        // Untrusted kept entries: invalid hashes, excluded or unsafe paths, collisions.
        // Exclusions added after a publication call for a replacement, said plainly.
        let error = Manifest::prepare(
            root.path(),
            deadline(),
            limit,
            &["_snapshot/images/kept".to_string()],
            &kept("_snapshot/images/kept/test/a.png", good, 9),
        )
        .err()
        .context("excluded kept assets must fail")?
        .to_string();
        assert!(
            error.contains("exclusions") && error.contains("replace"),
            "{error}"
        );
        for (path, hash, excludes) in [
            ("_snapshot/images/kept/a.png", "XYZ", vec![]),
            (
                "_snapshot/images/kept/a.map",
                good,
                vec!["*.map".to_string()],
            ),
            ("_snapshot/../a.png", good, vec![]),
            ("index.html", good, vec![]),
        ] {
            assert!(
                Manifest::prepare(
                    root.path(),
                    deadline(),
                    limit,
                    &excludes,
                    &kept(path, hash, 1)
                )
                .is_err(),
                "{path}"
            );
        }
        // Kept suites cannot be optimized, so an over-budget site must be replaced.
        let error = Manifest::prepare(
            root.path(),
            deadline(),
            original.bytes,
            &[],
            &kept("_snapshot/images/kept/test/a.png", good, 9),
        )
        .err()
        .context("over-budget kept deployment must fail")?
        .to_string();
        assert!(error.contains("replace the whole site"), "{error}");
        // A staged state that cannot describe the optimized files is left out of the upload.
        let (optimized, prepared) = Manifest::prepare(
            root.path(),
            deadline(),
            original.bytes * 85 / 100,
            &[],
            &super::super::KeptFiles::new(),
        )?;
        assert_ne!(prepared.path(), root.path());
        assert!(!optimized.files.contains_key("/_snapshot/deployment.json"));
        // Buckets asking for kept content fail before any upload.
        let mut with_kept = Manifest::create(root.path(), deadline())?;
        with_kept.keep(&kept("_snapshot/images/kept/test/a.png", good, 9), &[])?;
        let error = with_kept
            .validate_buckets(&[vec![good.to_owned()]])
            .unwrap_err()
            .to_string();
        assert!(error.contains("replace the whole site"), "{error}");
        Ok(())
    }

    #[test]
    fn oversized_reports_use_shared_optimization_and_hash_the_prepared_assets() -> Result<()> {
        let root = optimization::tests::report_fixture()?;
        let original = Manifest::create(root.path(), deadline())?;
        let limit = original.bytes * 85 / 100;
        let original_files = original
            .files
            .iter()
            .map(|(path, asset)| Ok((path.clone(), original.read_asset(&asset.hash)?.1)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        std::fs::write(root.path().join("_headers"), "x".repeat(limit as usize))?;
        std::fs::write(root.path().join(".env"), "private")?;
        std::fs::write(root.path().join("debug.map"), "x".repeat(limit as usize))?;
        std::fs::create_dir(root.path().join("_worker.js"))?;
        let excludes = ["*.map".into(), "_worker.js".into()];
        let (manifest, prepared) = Manifest::prepare(
            root.path(),
            deadline(),
            limit,
            &excludes,
            &super::super::KeptFiles::new(),
        )?;
        assert_eq!(prepared.original_bytes(), original.bytes);
        assert!(manifest.bytes < prepared.original_bytes());
        assert!(manifest.bytes <= limit);
        assert_eq!(manifest.files.len(), original.files.len());
        assert_eq!(manifest.root, prepared.path().canonicalize()?);
        assert_ne!(manifest.root, root.path().canonicalize()?);
        assert!(!manifest.root.join("_headers").exists());
        assert!(!manifest.root.join(".env").exists());
        assert!(!manifest.root.join("debug.map").exists());
        let mut reduced = 0;
        for (path, asset) in &manifest.files {
            let (relative, bytes) = manifest.read_asset(&asset.hash)?;
            assert_eq!(asset.hash, asset_hash(&bytes, &relative));
            assert_eq!(asset.size, bytes.len() as u64);
            let source = &original_files[path];
            assert_eq!(std::fs::read(root.path().join(&path[1..]))?, *source);
            if path.ends_with(".png") {
                let source = lodepng::decode32(source)?;
                let optimized = lodepng::decode32(&bytes)?;
                assert_eq!(
                    (source.width, source.height),
                    (optimized.width, optimized.height)
                );
                if path.contains("failed") {
                    assert_eq!(source.buffer, optimized.buffer);
                } else if source.buffer != optimized.buffer {
                    reduced += 1;
                }
            }
        }
        assert!(reduced > 0);
        drop(prepared);
        assert!(!manifest.root.exists());
        Ok(())
    }

    #[test]
    fn preparation_preserves_fitting_reports_and_rejects_unfittable_content() -> Result<()> {
        assert_eq!(MAX_CONTENT_BYTES, 2 * 1024 * 1024 * 1024);
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        let (manifest, prepared) = Manifest::prepare(
            root.path(),
            deadline(),
            6,
            &[],
            &super::super::KeptFiles::new(),
        )?;
        assert_eq!(manifest.bytes, 6);
        assert_eq!(prepared.path(), root.path().canonicalize()?);
        assert!(
            Manifest::prepare(
                root.path(),
                deadline(),
                5,
                &[],
                &super::super::KeptFiles::new()
            )
            .is_err()
        );
        assert!(
            Manifest::prepare(
                root.path(),
                Instant::now(),
                6,
                &[],
                &super::super::KeptFiles::new()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(root.path().join("index.html"))?, b"report");
        std::fs::write(root.path().join(".assetsignore"), "*")?;
        assert!(
            Manifest::prepare(
                root.path(),
                deadline(),
                6,
                &[".assetsignore".into()],
                &super::super::KeptFiles::new()
            )
            .is_err()
        );
        Ok(())
    }

    /// A generated snapshot site for suite `one`, staged under the returned directory's
    /// `report`, with its site state as an incremental deployment stages it.
    fn staged_snapshot_with_state() -> Result<(tempfile::TempDir, tempfile::TempDir, PathBuf)> {
        let source = optimization::tests::report_fixture()?;
        let report = super::super::read_report_configuration(
            &source.path().join("one/html_report/config.js"),
        )?;
        std::fs::write(
            source.path().join("one/bitmaps_test/run/report.json"),
            serde_json::to_vec(&report)?,
        )?;
        let suites = vec!["one".to_owned()];
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
        )?;
        let staging = tempfile::tempdir()?;
        let root = staging.path().join("report");
        let inventory = super::super::stage_site(source.path(), &suites, &[], &root)?;
        let summary = crate::snapshot::suite_summaries(&root)?.remove(0);
        let files = inventory.suites["one"]
            .iter()
            .map(|relative| -> Result<_> {
                let path = relative.to_string_lossy().replace('\\', "/");
                let bytes = std::fs::read(root.join(relative))?;
                let file = super::super::site_state::StateFile {
                    size: bytes.len() as u64,
                    hash: asset_hash(&bytes, &path),
                };
                Ok((path, file))
            })
            .collect::<Result<_>>()?;
        let state = super::super::site_state::SiteState {
            format: super::super::site_state::STATE_FORMAT,
            data_version: super::super::site_state::DATA_VERSION,
            generator: "test".into(),
            provider: super::super::DeploymentProvider::CloudflareWorkers,
            deployed_at: "2026-11-09T00:00:00Z".into(),
            fingerprint: "c".repeat(64),
            suites: BTreeMap::from([(
                "one".to_owned(),
                super::super::site_state::SuiteState {
                    summary,
                    run_id: inventory.runs["one"].clone(),
                    files,
                },
            )]),
        };
        std::fs::write(
            root.join(super::super::site_state::STATE_PATH),
            state.to_bytes()?,
        )?;
        Ok((source, staging, root))
    }

    #[test]
    fn optimized_uploads_publish_a_site_state_of_the_optimized_files() -> Result<()> {
        use super::super::site_state::{STATE_PATH, SiteState};
        let (_source, _staging, root) = staged_snapshot_with_state()?;
        let staged = SiteState::parse(&std::fs::read(root.join(STATE_PATH))?)?;
        let original = Manifest::create(&root, deadline())?;
        let image_bytes: u64 = original
            .files
            .iter()
            .filter(|(path, _)| path.ends_with(".png"))
            .map(|(_, asset)| asset.size)
            .sum();
        let limit = original.bytes - image_bytes * 15 / 100;
        let (manifest, prepared) = Manifest::prepare(
            &root,
            deadline(),
            limit,
            &[],
            &super::super::KeptFiles::new(),
        )?;
        assert_ne!(prepared.path(), root.as_path());

        // The uploaded state names exactly the optimized assets and refreshed versions, and the
        // staged copy, which the deployment records locally, matches it.
        let uploaded = std::fs::read(prepared.path().join(STATE_PATH))?;
        assert_eq!(std::fs::read(root.join(STATE_PATH))?, uploaded);
        assert_eq!(
            manifest.files[&format!("/{STATE_PATH}")].hash,
            asset_hash(&uploaded, STATE_PATH)
        );
        let state = SiteState::parse(&uploaded)?;
        let suite = &state.suites["one"];
        for (path, file) in &suite.files {
            let asset = &manifest.files[&format!("/{path}")];
            assert_eq!((&file.hash, file.size), (&asset.hash, asset.size), "{path}");
        }
        assert_eq!(
            suite.summary,
            crate::snapshot::suite_summaries(prepared.path())?.remove(0)
        );
        assert_ne!(suite.files, staged.suites["one"].files);
        assert_ne!(suite.summary.report, staged.suites["one"].summary.report);
        assert_eq!(state.fingerprint, staged.fingerprint);
        assert!(manifest.bytes <= limit);

        // A state that cannot describe the optimized site is left out, never published stale.
        let (_source, _staging, root) = staged_snapshot_with_state()?;
        let mut broken = SiteState::parse(&std::fs::read(root.join(STATE_PATH))?)?;
        broken.suites.get_mut("one").unwrap().files.insert(
            "_snapshot/images/one/missing.png".into(),
            super::super::site_state::StateFile {
                size: 1,
                hash: "d".repeat(32),
            },
        );
        std::fs::write(root.join(STATE_PATH), broken.to_bytes()?)?;
        let (manifest, _prepared) = Manifest::prepare(
            &root,
            deadline(),
            limit,
            &[],
            &super::super::KeptFiles::new(),
        )?;
        assert!(!manifest.files.contains_key(&format!("/{STATE_PATH}")));
        Ok(())
    }

    #[test]
    fn optimized_snapshots_refresh_image_suite_and_manifest_versions() -> Result<()> {
        let source = optimization::tests::report_fixture()?;
        let report = super::super::read_report_configuration(
            &source.path().join("one/html_report/config.js"),
        )?;
        std::fs::write(
            source.path().join("one/bitmaps_test/run/report.json"),
            serde_json::to_vec(&report)?,
        )?;
        let suites = vec!["one".into()];
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
        )?;
        let staging = tempfile::tempdir()?;
        let root = staging.path().join("report");
        super::super::stage_reports(source.path(), &suites, &root)?;
        let original = Manifest::create(&root, deadline())?;
        let original_index = std::fs::read(root.join("index.html"))?;
        let image_bytes: u64 = original
            .files
            .iter()
            .filter(|(path, _)| path.ends_with(".png"))
            .map(|(_, asset)| asset.size)
            .sum();
        let limit = original.bytes - image_bytes * 15 / 100;
        let (manifest, prepared) = Manifest::prepare(
            &root,
            deadline(),
            limit,
            &[],
            &super::super::KeptFiles::new(),
        )?;
        assert!(manifest.bytes <= limit);
        assert_eq!(
            manifest.files.keys().collect::<Vec<_>>(),
            original.files.keys().collect::<Vec<_>>()
        );
        let versioned_bytes = |value: &str| -> Result<Vec<u8>> {
            let url = Url::parse("https://reports.invalid/")?.join(value)?;
            let bytes = std::fs::read(prepared.path().join(url.path().trim_start_matches('/')))?;
            let versions = url.query_pairs().collect::<Vec<_>>();
            assert_eq!(versions.len(), 1);
            assert_eq!(versions[0].0, "v");
            assert_eq!(
                versions[0].1,
                crate::snapshot::get_asset_version(&bytes, 20)
            );
            Ok(bytes)
        };
        let index = std::fs::read_to_string(prepared.path().join("index.html"))?;
        let manifest_bytes = std::fs::read(prepared.path().join("_snapshot/data/manifest.json"))?;
        assert!(index.contains(&crate::snapshot::get_asset_version(&manifest_bytes, 20)));
        assert_ne!(index.as_bytes(), original_index);
        let data: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
        let suite: serde_json::Value = serde_json::from_slice(&versioned_bytes(
            data["suites"][0]["report"]
                .as_str()
                .context("Expected suite URL")?,
        )?)?;
        for capture in suite["tests"].as_array().context("Expected captures")? {
            for key in ["reference", "test", "diff"] {
                if let Some(image) = capture[key].as_str() {
                    versioned_bytes(image)?;
                }
            }
        }
        assert_eq!(std::fs::read(root.join("index.html"))?, original_index);
        for (path, asset) in &original.files {
            assert_eq!(
                asset_hash(&std::fs::read(root.join(&path[1..]))?, path),
                asset.hash
            );
        }
        Ok(())
    }

    #[test]
    fn upload_buckets_only_accept_known_distinct_hashes() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "")?;
        let manifest = Manifest::create(root.path(), deadline())?;
        let hash = manifest.files["/index.html"].hash.clone();
        manifest.validate_buckets(&[])?;
        manifest.validate_buckets(&[vec![hash.clone()]])?;
        for buckets in [
            vec![vec![]],
            vec![vec!["../index.html".into()]],
            vec![vec!["0".repeat(32)]],
            vec![vec![hash.clone(), hash.clone()]],
            vec![vec![hash.clone()], vec![hash]],
        ] {
            assert!(manifest.validate_buckets(&buckets).is_err());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn manifest_rejects_symlinks_and_changed_parents() -> Result<()> {
        let root = tempfile::tempdir()?;
        let staging = root.path().join("stage");
        std::fs::create_dir_all(staging.join("assets"))?;
        std::fs::write(staging.join("index.html"), "report")?;
        std::fs::write(staging.join("assets/file.txt"), "image")?;
        let manifest = Manifest::create(&staging, deadline())?;
        std::os::unix::fs::symlink(&staging, root.path().join("link"))?;
        assert!(Manifest::create(&root.path().join("link"), deadline()).is_err());
        std::fs::rename(staging.join("assets"), staging.join("original"))?;
        std::os::unix::fs::symlink(staging.join("original"), staging.join("assets"))?;
        assert!(Manifest::create(&staging, deadline()).is_err());
        assert!(
            manifest
                .read_asset(&manifest.files["/assets/file.txt"].hash)
                .is_err()
        );
        Ok(())
    }
}
