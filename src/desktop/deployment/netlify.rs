use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::{Deserialize, de::DeserializeOwned};
use sha1::{Digest, Sha1};

use super::{DeployTarget, DeploymentProvider, diagnostics, phase};

const UPLOAD_PROGRESS_INTERVAL: Duration = Duration::from_secs(60);
const MAX_FILES: usize = 25_000;
const MAX_CONTENT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const UPLOAD_CONCURRENCY: usize = 4;

#[derive(Deserialize)]
struct Site {
    id: String,
    prevent_non_git_prod_deploys: Option<bool>,
    published_deploy: Option<PublishedDeploy>,
}

#[derive(Deserialize)]
struct PublishedDeploy {
    locked: Option<bool>,
}

#[derive(Deserialize)]
struct Deploy {
    id: String,
    site_id: String,
    state: String,
    required: Option<Vec<String>>,
    ssl_url: Option<String>,
    deploy_ssl_url: Option<String>,
    draft: Option<bool>,
    #[serde(default)]
    error_message: serde_json::Value,
    #[serde(default)]
    error: serde_json::Value,
}

pub struct NetlifyClient {
    client: Client,
    endpoint: Url,
    authorization: HeaderValue,
    backoff: Duration,
    poll_interval: Duration,
    read_timeout: Duration,
    deadline: Duration,
}

impl NetlifyClient {
    pub fn new(token: &str) -> Result<Self> {
        DeploymentProvider::Netlify.validate_token(token)?;
        let value = zeroize::Zeroizing::new(format!("Bearer {token}"));
        let mut authorization = HeaderValue::from_str(&value)
            .map_err(|_| anyhow!("Invalid Netlify personal access token"))?;
        authorization.set_sensitive(true);
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!("Regressify-Desktop/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::none())
                .https_only(!cfg!(test))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(180))
                .build()?,
            endpoint: Url::parse("https://api.netlify.com/api/v1/")?,
            authorization,
            backoff: Duration::from_secs(1),
            poll_interval: Duration::from_secs(5),
            read_timeout: Duration::from_secs(30),
            deadline: Duration::from_secs(60 * 60),
        })
    }

    async fn get<Response: DeserializeOwned>(&self, path: &str) -> Result<Response> {
        use crate::diagnostics::record::SafeValue;
        let endpoint = if path.starts_with("sites/") {
            SafeValue::Preflight
        } else {
            SafeValue::Poll
        };
        let url = self.endpoint.join(path)?;
        for attempt in 0..6 {
            diagnostics::request_started(DeploymentProvider::Netlify, endpoint, attempt + 1);
            let started = Instant::now();
            let response = self
                .client
                .get(url.clone())
                .header(AUTHORIZATION, self.authorization.clone())
                .timeout(self.read_timeout)
                .send()
                .await;
            diagnostics::response_received(
                DeploymentProvider::Netlify,
                endpoint,
                attempt + 1,
                response
                    .as_ref()
                    .ok()
                    .map(|response| response.status().as_u16()),
                started.elapsed(),
            );
            let mut retry_after = None;
            let error = match response {
                Ok(response) if response.status().is_success() => match read_json(response).await {
                    Ok(value) => return Ok(value),
                    Err(error) if error.is::<ResponseBodyError>() => error,
                    Err(error) => return Err(error),
                },
                Ok(response) => {
                    let status = response.status();
                    retry_after = response
                        .headers()
                        .get("Retry-After")
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(|seconds| Duration::from_secs(seconds.min(60)));
                    let error = diagnostics::http_error(
                        response,
                        DeploymentProvider::Netlify,
                        &self.authorization,
                    )
                    .await;
                    if !status.is_server_error() && !matches!(status.as_u16(), 408 | 429) {
                        return Err(error);
                    }
                    error
                }
                Err(error) if error.is_timeout() => {
                    anyhow!("Netlify read request timed out before receiving response headers")
                }
                Err(_) => anyhow!("Cannot reach Netlify; check your connection and try again"),
            };
            if attempt == 5 {
                bail!(
                    "Netlify read request failed after 6 attempts: {error}; check your connection, proxy, or firewall and try again"
                );
            }
            let delay = retry_after.unwrap_or(self.backoff * (1 << attempt));
            diagnostics::retry(DeploymentProvider::Netlify, endpoint, attempt + 2, delay);
            phase(&format!(
                "Netlify retry {}/6 in {:.1}s: {error}",
                attempt + 2,
                delay.as_secs_f64()
            ));
            tokio::time::sleep(delay).await;
        }
        bail!("Netlify request retry budget exhausted")
    }

    pub async fn deploy(&self, root: &Path, target: &DeployTarget) -> Result<String> {
        self.deploy_with_stats(
            root,
            target,
            &super::KeptFiles::new(),
            &mut super::DeploymentStats::default(),
        )
        .await
    }

    /// Publishes `root` plus `kept` files the site already serves, which are listed in the
    /// deploy by digest without being uploaded.
    pub(super) async fn deploy_with_stats(
        &self,
        root: &Path,
        target: &DeployTarget,
        kept: &super::KeptFiles,
        stats: &mut super::DeploymentStats,
    ) -> Result<String> {
        target.validate()?;
        ensure!(
            target.provider == DeploymentProvider::Netlify,
            "Netlify client requires a Netlify target"
        );
        let deadline = Instant::now() + self.deadline;
        let operation = async {
            validate_static_configuration(root)?;
            phase("Validating Netlify deployment target");
            let site_path = format!("sites/{}", target.site_id);
            let site: Site = self.get(&site_path).await?;
            ensure!(
                site.id.eq_ignore_ascii_case(&target.site_id),
                "Netlify returned a different site"
            );
            ensure!(
                site.prevent_non_git_prod_deploys != Some(true),
                "This Netlify site only allows production deploys from Git"
            );
            ensure!(
                !site
                    .published_deploy
                    .is_some_and(|deploy| deploy.locked == Some(true)),
                "Unlock the current Netlify deployment before publishing reports"
            );
            phase("Hashing selected report files for Netlify");
            let directory = root.to_path_buf();
            let kept = kept.clone();
            let diagnostic_context = crate::diagnostics::runtime::current();
            let manifest = tokio::task::spawn_blocking(move || {
                crate::diagnostics::runtime::in_scope(diagnostic_context, || {
                    use crate::diagnostics::{record::*, runtime};
                    runtime::stage_sync(
                        Stage::PackagingOptimization,
                        [
                            (Field::Provider, FieldValue::Kind(SafeValue::Netlify)),
                            (Field::Compression, FieldValue::Boolean(false)),
                        ],
                        || {
                            let mut manifest = Manifest::create(&directory, deadline)?;
                            manifest.keep(&kept, MAX_FILES)?;
                            Ok(manifest)
                        },
                    )
                })
            })
            .await??;
            // The published site includes kept files, so stats count them like Cloudflare's.
            let counts = manifest.counts();
            *stats = super::DeploymentStats {
                file_count: counts.files() as u64,
                original_bytes: counts.bytes(),
                total_bytes: counts.bytes(),
                state_dropped: false,
            };
            phase(&format!(
                "Netlify manifest ready: {}",
                counts.describe("digests", "digest")
            ));
            {
                use crate::diagnostics::{record::*, runtime};
                runtime::event(
                    Stage::PackagingOptimization,
                    DiagnosticEvent::Decision,
                    Severity::Info,
                    [
                        (Field::Bytes, FieldValue::Number(counts.bytes())),
                        (Field::FileCount, FieldValue::Number(counts.files() as u64)),
                        (Field::Compression, FieldValue::Boolean(false)),
                    ],
                );
            }
            let body =
                serde_json::to_vec(&serde_json::json!({"async":true, "files":&manifest.files}))?;
            ensure!(
                body.len() <= MAX_MANIFEST_BYTES,
                "Netlify file manifest exceeds 16 MiB"
            );
            phase(&format!(
                "Creating Netlify deployment for {} files ({:.2} MiB content)",
                counts.files(),
                counts.bytes() as f64 / (1024.0 * 1024.0)
            ));
            let create_started = Instant::now();
            diagnostics::request_started(
                DeploymentProvider::Netlify,
                crate::diagnostics::record::SafeValue::Publication,
                1,
            );
            let response = self
                .client
                .post(self.endpoint.join(&format!("{site_path}/deploys"))?)
                .header(AUTHORIZATION, self.authorization.clone())
                .header("Content-Type", "application/json")
                .timeout(Duration::from_secs(30))
                .body(body)
                .send().await
                .map_err(|_| {
                    diagnostics::response_received(DeploymentProvider::Netlify, crate::diagnostics::record::SafeValue::Publication, 1, None, create_started.elapsed());
                    anyhow!("Netlify deployment creation did not complete; check the site's deploy history before retrying")
                })?;
            diagnostics::response_received(
                DeploymentProvider::Netlify,
                crate::diagnostics::record::SafeValue::Publication,
                1,
                Some(response.status().as_u16()),
                create_started.elapsed(),
            );
            if !response.status().is_success() {
                let error = diagnostics::http_error(
                    response,
                    DeploymentProvider::Netlify,
                    &self.authorization,
                )
                .await;
                bail!("{error}; check the site's deploy history before retrying");
            }
            let mut deploy: Deploy = read_json(response).await?;
            ensure!(
                !deploy.id.is_empty()
                    && deploy.id.len() <= 128
                    && deploy
                        .id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
                "Netlify returned an invalid deployment ID"
            );
            let deploy_id = deploy.id.clone();
            phase(&format!(
                "Netlify created deployment {deploy_id}; checking required files"
            ));
            let mut uploaded = false;
            let polling_started = std::time::Instant::now();
            let mut polls = 0;
            loop {
                ensure!(
                    deploy.id == deploy_id && deploy.site_id.eq_ignore_ascii_case(&target.site_id),
                    "Netlify returned a different deployment"
                );
                ensure!(
                    deploy.draft != Some(true),
                    "Netlify returned a draft instead of a production deployment"
                );
                match deploy.state.as_str() {
                    "prepared" | "uploading" if !uploaded => {
                        let required = deploy
                            .required
                            .as_deref()
                            .context("Netlify returned no required-file list")?;
                        self.upload_files(&manifest, required, &deploy_id).await?;
                        uploaded = true;
                    }
                    "ready" => {
                        let value = deploy
                            .ssl_url
                            .as_deref()
                            .filter(|value| !value.is_empty())
                            .or(deploy.deploy_ssl_url.as_deref())
                            .context("Netlify deployment is ready without an HTTPS site URL")?;
                        let url = site_url(value)?;
                        phase(&format!("Deployed: {url}"));
                        diagnostics::published(DeploymentProvider::Netlify, Some(&url));
                        return Ok(url);
                    }
                    "error" | "failed" | "canceled" | "cancelled" | "rejected" => {
                        let error = diagnostics::failure(
                            DeploymentProvider::Netlify,
                            "deployment",
                            None,
                            &serde_json::json!({"error_message": deploy.error_message, "error": deploy.error}),
                            &self.authorization,
                        );
                        bail!("{error} Deployment ID: {deploy_id}");
                    }
                    "new" | "pending_review" | "accepted" | "enqueued" | "building"
                    | "uploading" | "uploaded" | "preparing" | "prepared" | "processing"
                    | "processed" | "retrying" => {}
                    _ => bail!(
                        "Netlify returned an unexpected deployment state; check the site's deploy history"
                    ),
                }
                polls += 1;
                phase(&format!(
                    "Netlify status: {}; poll {polls}, {}s waiting; next check in {:.1}s",
                    deploy.state,
                    polling_started.elapsed().as_secs(),
                    self.poll_interval.as_secs_f64()
                ));
                tokio::time::sleep(self.poll_interval).await;
                deploy = self
                    .get(&format!("{site_path}/deploys/{deploy_id}"))
                    .await?;
            }
        };
        tokio::time::timeout(
            self.deadline,
            super::with_heartbeat("Netlify deployment", operation),
        )
        .await
        .context(
            "Netlify deployment exceeded its 1-hour deadline; check the site's deploy history",
        )?
    }

    async fn upload_files(
        &self,
        manifest: &Manifest,
        required: &[String],
        deploy_id: &str,
    ) -> Result<()> {
        let files = manifest.required_files(required)?;
        let count = files.len();
        let bytes = files.iter().map(|(_, file)| file.bytes).sum::<u64>();
        phase(&format!(
            "Netlify needs {count} distinct files ({:.2} MiB); reusing content for {} of {} paths",
            bytes as f64 / (1024.0 * 1024.0),
            manifest.files.len() - count,
            manifest.files.len()
        ));
        let accepted = AtomicUsize::new(0);
        let acknowledged = AtomicU64::new(0);
        stream::iter(files.into_iter().enumerate())
            .map(|(index, (digest, file))| {
                let accepted = &accepted;
                let acknowledged = &acknowledged;
                async move {
                    self.upload_file(manifest, file, digest, deploy_id, index + 1, count)
                        .await?;
                    let completed_bytes =
                        acknowledged.fetch_add(file.bytes, Ordering::Relaxed) + file.bytes;
                    let completed = accepted.fetch_add(1, Ordering::Relaxed) + 1;
                    phase(&format!(
                        "Netlify upload acknowledged: {completed} of {count} files ({} of {} KiB)",
                        completed_bytes / 1024,
                        bytes / 1024
                    ));
                    Ok::<_, anyhow::Error>(())
                }
            })
            .buffer_unordered(UPLOAD_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;
        phase("Netlify required files uploaded; waiting for publication");
        Ok(())
    }

    async fn upload_file(
        &self,
        manifest: &Manifest,
        file: &ManifestFile,
        digest: &str,
        deploy_id: &str,
        position: usize,
        count: usize,
    ) -> Result<()> {
        let mut url = self.endpoint.join(&format!("deploys/{deploy_id}/files/"))?;
        url.path_segments_mut()
            .map_err(|_| anyhow!("Invalid Netlify endpoint"))?
            .pop_if_empty()
            .extend(file.path.split('/'));
        for attempt in 0..6 {
            let endpoint = crate::diagnostics::record::SafeValue::Upload;
            diagnostics::request_started(DeploymentProvider::Netlify, endpoint, attempt + 1);
            let started = Instant::now();
            let mut input = manifest.open_file(&file.path)?;
            ensure!(
                input.metadata()?.len() == file.bytes,
                "Deployment file changed after hashing"
            );
            let transfer = Arc::new(FileTransfer::default());
            let body = if file.bytes == 0 {
                let mut extra = [0];
                ensure!(
                    input.read(&mut extra)? == 0 && format!("{:x}", Sha1::digest([])) == digest,
                    "Deployment file changed after hashing"
                );
                transfer.verified.store(true, Ordering::Release);
                reqwest::Body::from(Vec::new())
            } else {
                upload_body(
                    tokio::fs::File::from_std(input),
                    digest.to_owned(),
                    file.bytes,
                    transfer.clone(),
                    position,
                    count,
                )
            };
            let upload = self
                .client
                .put(url.clone())
                .header(AUTHORIZATION, self.authorization.clone())
                .header("Content-Type", "application/octet-stream")
                .header("Content-Length", file.bytes)
                .timeout(self.deadline)
                .body(body)
                .send();
            let mut progress = UploadStallMonitor::default();
            let response = super::monitor(upload, UPLOAD_PROGRESS_INTERVAL, |elapsed| {
                if let Some(warning) =
                    progress.warning(transfer.sent.load(Ordering::Relaxed), file.bytes, elapsed)
                {
                    phase(&format!("Netlify file {position}/{count}: {warning}"));
                }
            })
            .await;
            diagnostics::response_received(
                DeploymentProvider::Netlify,
                endpoint,
                attempt + 1,
                response
                    .as_ref()
                    .ok()
                    .map(|response| response.status().as_u16()),
                started.elapsed(),
            );
            ensure!(
                !transfer.read_failed.load(Ordering::Acquire),
                "Cannot upload changed or unreadable staged content to Netlify"
            );
            let mut retry_after = None;
            let error = match response {
                Ok(response) if response.status().is_success() => {
                    response_bytes(response).await?;
                    ensure!(
                        transfer.verified.load(Ordering::Acquire),
                        "Netlify accepted a file before its content was verified"
                    );
                    return Ok(());
                }
                Ok(response) => {
                    let status = response.status();
                    retry_after = response
                        .headers()
                        .get("Retry-After")
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(|seconds| Duration::from_secs(seconds.min(60)));
                    let error = diagnostics::http_error(
                        response,
                        DeploymentProvider::Netlify,
                        &self.authorization,
                    )
                    .await;
                    if !status.is_server_error() && !matches!(status.as_u16(), 408 | 429) {
                        return Err(error);
                    }
                    error
                }
                Err(error) => {
                    ensure!(
                        !error.is_body(),
                        "Cannot upload changed or unreadable staged content to Netlify"
                    );
                    anyhow!(
                        "Netlify file upload {} after sending {} of {} KiB",
                        if error.is_timeout() {
                            "timed out"
                        } else {
                            "did not complete"
                        },
                        transfer.sent.load(Ordering::Relaxed) / 1024,
                        file.bytes / 1024
                    )
                }
            };
            if attempt == 5 {
                return Err(error);
            }
            let delay = retry_after.unwrap_or(self.backoff * (1 << attempt));
            diagnostics::retry(DeploymentProvider::Netlify, endpoint, attempt + 2, delay);
            // File PUTs are idempotent within this deploy; creating another deploy is not.
            phase(&format!(
                "Netlify file {position}/{count} retry {}/6 in {:.1}s: {error}",
                attempt + 2,
                delay.as_secs_f64()
            ));
            tokio::time::sleep(delay).await;
        }
        bail!("Netlify file upload retry budget exhausted")
    }
}

struct ManifestFile {
    path: String,
    bytes: u64,
}

struct Manifest {
    root: PathBuf,
    files: BTreeMap<String, String>,
    contents: BTreeMap<String, ManifestFile>,
    /// Bytes of the staged files only; kept files are counted in `kept_bytes`.
    bytes: u64,
    /// Digests of files kept on the site without local content.
    kept: BTreeSet<String>,
    /// Paths and sizes of kept files, which `files` lists but `bytes` and `contents` do not.
    kept_files: usize,
    kept_bytes: u64,
}

impl Manifest {
    fn create(root: &Path, deadline: Instant) -> Result<Self> {
        Self::create_bounded(root, deadline, MAX_FILES, MAX_CONTENT_BYTES)
    }

    fn create_bounded(
        root: &Path,
        deadline: Instant,
        file_limit: usize,
        byte_limit: u64,
    ) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(root)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Deployment staging root must be a regular directory"
        );
        let mut manifest = Self {
            root: root.canonicalize()?,
            files: BTreeMap::new(),
            contents: BTreeMap::new(),
            bytes: 0,
            kept: BTreeSet::new(),
            kept_files: 0,
            kept_bytes: 0,
        };
        let mut last_update = Instant::now();
        for entry in walkdir::WalkDir::new(&manifest.root)
            .follow_links(false)
            .sort_by_file_name()
        {
            ensure!(
                Instant::now() < deadline,
                "Netlify hashing exceeded the deployment deadline"
            );
            let entry = entry?;
            ensure!(
                !entry.file_type().is_symlink(),
                "Netlify deployment cannot contain symlinks"
            );
            if entry.file_type().is_dir() {
                continue;
            }
            ensure!(
                entry.file_type().is_file(),
                "Netlify deployment requires regular files"
            );
            ensure!(
                manifest.files.len() < file_limit,
                "Netlify deployment exceeds the {file_limit}-file limit"
            );
            let relative = entry.path().strip_prefix(&manifest.root)?;
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
                "Netlify deployment path contains unsupported characters"
            );
            let mut input = manifest.open_file(&path)?;
            let bytes = input.metadata()?.len();
            manifest.bytes = manifest
                .bytes
                .checked_add(bytes)
                .context("Netlify content size overflow")?;
            ensure!(
                manifest.bytes <= byte_limit,
                "Netlify deployment content exceeds its {} MiB limit",
                byte_limit as f64 / (1024.0 * 1024.0)
            );
            let mut hasher = Sha1::new();
            let mut read = 0;
            let mut buffer = [0; 64 * 1024];
            loop {
                ensure!(
                    Instant::now() < deadline,
                    "Netlify hashing exceeded the deployment deadline"
                );
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                read += count as u64;
                ensure!(read <= bytes, "Deployment file changed while hashing");
                hasher.update(&buffer[..count]);
            }
            ensure!(read == bytes, "Deployment file changed while hashing");
            // Netlify requires SHA-1 for file identity, not authentication or signatures.
            let digest = format!("{:x}", hasher.finalize());
            manifest.files.insert(format!("/{path}"), digest.clone());
            manifest
                .contents
                .entry(digest)
                .or_insert(ManifestFile { path, bytes });
            if manifest.files.len() == 1
                || manifest.files.len().is_multiple_of(50)
                || last_update.elapsed().as_secs() >= 1
            {
                phase(&format!(
                    "Hashed {} report files ({:.2} MiB)",
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

    fn open_file(&self, path: &str) -> Result<std::fs::File> {
        let mut source = self.root.clone();
        for component in path.split('/') {
            ensure!(
                !component.is_empty()
                    && !matches!(component, "." | "..")
                    && !component.contains(['\\', ':']),
                "Unsafe deployment path"
            );
            source.push(component);
            ensure!(
                !std::fs::symlink_metadata(&source)?.file_type().is_symlink(),
                "Netlify deployment cannot contain symlinks"
            );
        }
        ensure!(
            source.canonicalize()?.starts_with(&self.root),
            "Deployment file escapes staging"
        );
        ensure!(
            std::fs::symlink_metadata(&source)?.is_file(),
            "Netlify deployment requires regular files"
        );
        let file = std::fs::File::open(source)?;
        ensure!(
            file.metadata()?.is_file(),
            "Netlify deployment requires regular files"
        );
        Ok(file)
    }

    /// Lists files the site already serves by their digests. They are untrusted state from the
    /// site, so they are validated like local paths and may not shadow staged files.
    fn keep(&mut self, kept: &super::KeptFiles, file_limit: usize) -> Result<()> {
        for (path, file) in kept {
            ensure!(
                super::site_state::valid_site_path(path) && super::is_lower_hex(&file.hash, 40),
                "Invalid kept Netlify file '{path}'"
            );
            let key = format!("/{path}");
            // Suites captured by one run share its capture-log chunks, so a regenerated suite
            // can stage a chunk a kept suite also lists. Identical content is listed once;
            // different content at one path cannot be published and fails.
            if let Some(staged) = self.files.get(&key) {
                ensure!(
                    *staged == file.hash,
                    "A kept suite file differs from the staged file at {path}"
                );
                continue;
            }
            self.files.insert(key, file.hash.clone());
            ensure!(
                self.files.len() <= file_limit,
                "Netlify deployment exceeds the {file_limit}-file limit"
            );
            self.kept.insert(file.hash.clone());
            self.kept_files += 1;
            self.kept_bytes = self
                .kept_bytes
                .checked_add(file.size)
                .context("Kept Netlify content size overflow")?;
        }
        Ok(())
    }

    fn counts(&self) -> super::ManifestCounts {
        super::ManifestCounts {
            staged_files: self.files.len() - self.kept_files,
            staged_bytes: self.bytes,
            unique: self.contents.len(),
            kept_files: self.kept_files,
            kept_bytes: self.kept_bytes,
        }
    }

    fn required_files(&self, required: &[String]) -> Result<Vec<(&str, &ManifestFile)>> {
        let mut files = Vec::new();
        let mut seen = BTreeSet::new();
        for digest in required {
            ensure!(
                digest.len() == 40 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "Netlify returned an invalid required-file digest"
            );
            let digest = digest.to_ascii_lowercase();
            // Content of a kept suite that Netlify no longer stores cannot be uploaded from
            // here; failing before any upload leaves the published site untouched.
            ensure!(
                self.contents.contains_key(&digest) || !self.kept.contains(&digest),
                "Netlify no longer has the content of a suite kept on this site; replace the whole site"
            );
            let (digest, file) = self
                .contents
                .get_key_value(&digest)
                .context("Netlify requested content outside the deployment manifest")?;
            if seen.insert(digest) {
                files.push((digest.as_str(), file));
            }
        }
        Ok(files)
    }
}

#[derive(Default)]
struct FileTransfer {
    sent: AtomicU64,
    verified: AtomicBool,
    read_failed: AtomicBool,
}

fn upload_body(
    input: tokio::fs::File,
    digest: String,
    length: u64,
    transfer: Arc<FileTransfer>,
    position: usize,
    files: usize,
) -> reqwest::Body {
    let body = stream::try_unfold(
        (input, Sha1::new(), 0, Instant::now()),
        move |(mut input, mut hasher, mut completed, mut last_update)| {
            let digest = digest.clone();
            let transfer = transfer.clone();
            async move {
                use tokio::io::AsyncReadExt;
                let mut buffer = vec![0; 64 * 1024];
                let count = input.read(&mut buffer).await.inspect_err(|_| {
                    transfer.read_failed.store(true, Ordering::Release);
                })?;
                completed += count as u64;
                hasher.update(&buffer[..count]);
                if completed > length || (count == 0 && completed != length) {
                    transfer.read_failed.store(true, Ordering::Release);
                    return Err(std::io::Error::other(
                        "Deployment file changed after hashing",
                    ));
                }
                if completed == length {
                    let mut extra = [0];
                    let extra_count = input.read(&mut extra).await.inspect_err(|_| {
                        transfer.read_failed.store(true, Ordering::Release);
                    })?;
                    if extra_count != 0 || format!("{:x}", hasher.clone().finalize()) != digest {
                        transfer.read_failed.store(true, Ordering::Release);
                        return Err(std::io::Error::other(
                            "Deployment file changed after hashing",
                        ));
                    }
                    transfer.verified.store(true, Ordering::Release);
                }
                if count == 0 {
                    return Ok(None);
                }
                buffer.truncate(count);
                transfer.sent.store(completed, Ordering::Relaxed);
                if completed == length || last_update.elapsed().as_secs() >= 1 {
                    phase(&format!(
                        "Netlify file {position}/{files} upload sent: {} of {} KiB ({:.1}%); awaiting provider acceptance",
                        completed / 1024,
                        length / 1024,
                        completed as f64 * 100.0 / length as f64
                    ));
                    last_update = Instant::now();
                }
                Ok(Some((buffer, (input, hasher, completed, last_update))))
            }
        },
    );
    reqwest::Body::wrap_stream(body)
}

#[derive(Default)]
struct UploadStallMonitor {
    last_sent: u64,
    last_progress: Duration,
}

impl UploadStallMonitor {
    fn warning(&mut self, sent: u64, length: u64, elapsed: Duration) -> Option<String> {
        if sent != self.last_sent {
            self.last_sent = sent;
            self.last_progress = elapsed;
            return None;
        }
        let idle = elapsed.saturating_sub(self.last_progress);
        if idle < UPLOAD_PROGRESS_INTERVAL {
            return None;
        }
        // Body consumption measures local progress, not Netlify's acceptance.
        let status = if sent == length {
            format!(
                "Netlify upload sent all {} KiB; still awaiting provider acceptance after {}s",
                length / 1024,
                idle.as_secs()
            )
        } else {
            format!(
                "Netlify upload has made no progress for {}s ({} of {} KiB sent)",
                idle.as_secs(),
                sent / 1024,
                length / 1024
            )
        };
        Some(format!(
            "{status}. Keep waiting or choose Stop to cancel locally."
        ))
    }
}

async fn read_json<Response: DeserializeOwned>(response: reqwest::Response) -> Result<Response> {
    serde_json::from_slice(&response_bytes(response).await?)
        .map_err(|_| anyhow!("Invalid Netlify response"))
}

#[derive(Debug)]
struct ResponseBodyError {
    timed_out: bool,
    received: usize,
}

impl std::fmt::Display for ResponseBodyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = if self.timed_out {
            "timed out"
        } else {
            "was interrupted"
        };
        write!(
            formatter,
            "Netlify response body {reason} after receiving {} bytes",
            self.received
        )
    }
}

impl std::error::Error for ResponseBodyError {}

async fn response_bytes(response: reqwest::Response) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        // Keep retry classification without exposing reqwest's URL or provider content.
        let chunk = chunk.map_err(|error| ResponseBodyError {
            timed_out: error.is_timeout(),
            received: bytes.len(),
        })?;
        ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE_BYTES,
            "Netlify response exceeds 2 MiB"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn site_url(value: &str) -> Result<String> {
    let url = Url::parse(value).map_err(|_| anyhow!("Netlify returned an invalid site URL"))?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Netlify returned an unsafe site URL"
    );
    Ok(url.to_string())
}

fn validate_static_configuration(root: &Path) -> Result<()> {
    super::validate_static_configuration(root, DeploymentProvider::Netlify)
}

#[cfg(test)]
pub(crate) mod tests {
    #[tokio::test]
    async fn netlify_retry_then_success_records_attempts() -> Result<()> {
        let uploads = Arc::new(AtomicUsize::new(0));
        let attempts = uploads.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body).unwrap();
            let value = if request.url().ends_with(SITE_ID) {
                json!({"id":SITE_ID})
            } else if request.method().as_str() == "POST" {
                json!({"id":"deploy-retry", "site_id":SITE_ID, "state":"prepared", "required":["7c211433f02071597741e6ff5a8ea34789abbf43"]})
            } else if request.method().as_str() == "PUT" {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    let _ = request.respond(tiny_http::Response::empty(503));
                    return;
                }
                json!({})
            } else {
                json!({"id":"deploy-retry", "site_id":SITE_ID, "state":"ready", "ssl_url":"https://qa-reports.netlify.app/"})
            };
            let _ = request.respond(tiny_http::Response::from_string(value.to_string()));
        })?;
        let root = tempfile::tempdir()?;
        let logs = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "world")?;
        super::super::with_deployment_journal(
            logs.path(),
            client(&site.origin).deploy(root.path(), &target()),
        )
        .await?;
        assert_eq!(uploads.load(Ordering::SeqCst), 2);
        super::diagnostics::assert_retry_journal(logs.path(), DeploymentProvider::Netlify, &[TOKEN])
    }

    use super::*;
    use serde_json::json;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    pub(crate) const SITE_ID: &str = "eacbea58-4222-4e16-9611-f3b91082a8aa";
    const TOKEN: &str = "synthetic-netlify-access-token";

    pub(crate) fn target() -> DeployTarget {
        DeployTarget {
            id: "netlify".into(),
            name: "Netlify QA".into(),
            published_url: None,
            credential_id: None,
            named_token_id: None,
            environment: String::new(),
            provider: DeploymentProvider::Netlify,
            site_id: SITE_ID.into(),
            cloudflare: None,
        }
    }

    pub(crate) fn client(origin: &str) -> NetlifyClient {
        let mut client = NetlifyClient::new(TOKEN).unwrap();
        client.endpoint = Url::parse(&format!("{origin}/api/v1/")).unwrap();
        client.backoff = Duration::from_millis(1);
        client.poll_interval = Duration::from_millis(1);
        client
    }

    #[tokio::test]
    async fn read_requests_retry_interrupted_response_bodies() -> Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

        for (stalled, recovers) in [(false, true), (false, false), (true, true), (true, false)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let origin = format!("http://{}", listener.local_addr()?);
            let requests = Arc::new(AtomicUsize::new(0));
            let attempts = requests.clone();
            let server = tokio::spawn(async move {
                for attempt in 0..6 {
                    let (socket, _) = listener.accept().await?;
                    let mut socket = BufReader::new(socket);
                    let mut request_line = String::new();
                    socket.read_line(&mut request_line).await?;
                    ensure!(
                        request_line.starts_with("GET "),
                        "Response fixture only accepts reads"
                    );
                    loop {
                        let mut line = String::new();
                        ensure!(
                            socket.read_line(&mut line).await? != 0,
                            "Missing test request headers"
                        );
                        if line == "\r\n" {
                            break;
                        }
                    }
                    attempts.fetch_add(1, Ordering::SeqCst);
                    let complete = recovers && attempt > 0;
                    let body = if complete {
                        json!({"id":SITE_ID}).to_string()
                    } else {
                        format!("{{\"token\":\"{TOKEN}\"")
                    };
                    let length = body.len() + if complete { 0 } else { 100 };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}"
                    );
                    socket.get_mut().write_all(response.as_bytes()).await?;
                    if stalled && !complete {
                        // Keep the body incomplete until the client times out and closes it.
                        let mut buffer = [0];
                        let _ = socket.read(&mut buffer).await;
                    }
                    socket.get_mut().shutdown().await?;
                    if complete {
                        break;
                    }
                }
                Ok::<_, anyhow::Error>(())
            });
            let mut client = client(&origin);
            assert_eq!(client.read_timeout, Duration::from_secs(30));
            client.read_timeout = Duration::from_millis(250);
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                client.get::<Site>(&format!("sites/{SITE_ID}?token={TOKEN}")),
            )
            .await;
            server.abort();
            let _ = server.await;
            let result = result?;
            if recovers {
                assert_eq!(result?.id, SITE_ID);
                assert_eq!(requests.load(Ordering::SeqCst), 2);
            } else {
                let error = result.err().context("Expected response body failure")?;
                let message = format!("{error:#}");
                assert!(message.contains("after 6 attempts"), "{message}");
                assert!(
                    message.contains(if stalled {
                        "body timed out"
                    } else {
                        "body was interrupted"
                    }),
                    "{message}"
                );
                assert!(message.contains("after receiving"), "{message}");
                assert!(
                    !message.contains(TOKEN) && !message.contains(&origin),
                    "{message}"
                );
                assert_eq!(requests.load(Ordering::SeqCst), 6);
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn digest_deployment_uploads_only_missing_content_and_waits_for_ready() -> Result<()> {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let method = request.method().as_str().to_owned();
            let path = request.url().to_owned();
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body).unwrap();
            captured
                .lock()
                .unwrap()
                .push((method.clone(), path.clone(), body.clone()));
            let response = if path == format!("/api/v1/sites/{SITE_ID}") {
                json!({"id":SITE_ID})
            } else if method == "POST" {
                let manifest: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                if manifest["async"] != true || !manifest["files"].is_object() {
                    let _ = request.respond(tiny_http::Response::empty(400));
                    return;
                }
                json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared",
                    "required":["7c211433f02071597741e6ff5a8ea34789abbf43"]})
            } else if method == "PUT" {
                json!({})
            } else {
                json!({"id":"deploy-123", "site_id":SITE_ID, "state":"ready",
                    "ssl_url":"https://qa-reports.netlify.app/"})
            };
            let _ = request.respond(tiny_http::Response::from_string(response.to_string()));
        })?;
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("assets"))?;
        std::fs::write(root.path().join("index.html"), "hello")?;
        std::fs::write(root.path().join("assets/first.png"), "world")?;
        std::fs::write(root.path().join("assets/second.png"), "world")?;
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
            "https://qa-reports.netlify.app/"
        );
        assert_eq!(stats.file_count, 3);
        assert_eq!(stats.original_bytes, 15);
        assert_eq!(stats.total_bytes, 15);
        let records = requests.lock().unwrap();
        let posts = records
            .iter()
            .filter(|record| record.0 == "POST")
            .collect::<Vec<_>>();
        assert_eq!(posts.len(), 1);
        let manifest: serde_json::Value = serde_json::from_slice(&posts[0].2)?;
        assert_eq!(
            manifest["files"],
            json!({
                "/index.html":"aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                "/assets/first.png":"7c211433f02071597741e6ff5a8ea34789abbf43",
                "/assets/second.png":"7c211433f02071597741e6ff5a8ea34789abbf43"
            })
        );
        let uploads = records
            .iter()
            .filter(|record| record.0 == "PUT")
            .collect::<Vec<_>>();
        assert_eq!(uploads.len(), 1);
        assert_eq!(
            uploads[0].1,
            "/api/v1/deploys/deploy-123/files/assets/first.png"
        );
        assert_eq!(uploads[0].2, b"world");
        assert!(
            records
                .iter()
                .any(|record| record.0 == "GET" && record.1.ends_with("/deploys/deploy-123"))
        );
        Ok(())
    }

    #[test]
    fn kept_files_shared_with_staged_files_are_listed_once() -> Result<()> {
        // A capture-log chunk a regenerated suite stages and a kept suite also lists.
        const HELLO: &str = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d";
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "hello")?;
        std::fs::create_dir_all(root.path().join("_snapshot/data/logs"))?;
        std::fs::write(root.path().join("_snapshot/data/logs/run.json"), "hello")?;
        let shared = |hash: &str| {
            super::super::KeptFiles::from([(
                "_snapshot/data/logs/run.json".to_owned(),
                super::super::site_state::StateFile {
                    size: 5,
                    hash: hash.into(),
                },
            )])
        };
        let mut manifest = Manifest::create(root.path(), Instant::now() + Duration::from_secs(60))?;
        manifest.keep(&shared(HELLO), MAX_FILES)?;
        let counts = manifest.counts();
        assert_eq!(
            (counts.staged_files, counts.kept_files, counts.kept_bytes),
            (2, 0, 0)
        );
        assert!(manifest.kept.is_empty());
        // Different content at one path cannot be published.
        let mut conflicting =
            Manifest::create(root.path(), Instant::now() + Duration::from_secs(60))?;
        let error = conflicting
            .keep(&shared(&"1".repeat(40)), MAX_FILES)
            .unwrap_err()
            .to_string();
        assert!(error.contains("differs from the staged file"), "{error}");
        Ok(())
    }

    #[tokio::test]
    async fn kept_files_are_listed_by_digest_and_never_uploaded() -> Result<()> {
        const KEPT: &str = "1111111111111111111111111111111111111111";
        for required_kept in [false, true] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = requests.clone();
            let site = crate::test::browser_tests::Site::new(move |mut request| {
                let method = request.method().as_str().to_owned();
                let path = request.url().to_owned();
                let mut body = Vec::new();
                request.as_reader().read_to_end(&mut body).unwrap();
                captured
                    .lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), body.clone()));
                let response = if path == format!("/api/v1/sites/{SITE_ID}") {
                    json!({"id":SITE_ID})
                } else if method == "POST" {
                    // Netlify lost the kept content in the second case.
                    let required = if required_kept {
                        json!([KEPT])
                    } else {
                        json!(["aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"])
                    };
                    json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared",
                        "required":required})
                } else if method == "PUT" {
                    json!({})
                } else {
                    json!({"id":"deploy-123", "site_id":SITE_ID, "state":"ready",
                        "ssl_url":"https://qa-reports.netlify.app/"})
                };
                let _ = request.respond(tiny_http::Response::from_string(response.to_string()));
            })?;
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "hello")?;
            let kept = super::super::KeptFiles::from([(
                "_snapshot/images/kept/test/home.png".to_owned(),
                super::super::site_state::StateFile {
                    size: 7,
                    hash: KEPT.into(),
                },
            )]);
            let mut stats = super::super::DeploymentStats::default();
            let result = client(&site.origin)
                .deploy_with_stats(root.path(), &target(), &kept, &mut stats)
                .await;
            // The published site includes the kept file: 5 staged bytes plus 7 kept.
            assert_eq!((stats.file_count, stats.total_bytes), (2, 12));
            let mut manifest =
                Manifest::create(root.path(), Instant::now() + Duration::from_secs(60))?;
            manifest.keep(&kept, MAX_FILES)?;
            assert_eq!(
                manifest.counts().describe("digests", "digest"),
                "2 files: 1 staged (0.00 MiB, 1 unique digests), 1 kept by digest (0.00 MiB)"
            );
            let records = requests.lock().unwrap();
            let manifest: serde_json::Value = serde_json::from_slice(
                &records
                    .iter()
                    .find(|record| record.0 == "POST")
                    .context("missing deploy")?
                    .2,
            )?;
            assert_eq!(
                manifest["files"]["/_snapshot/images/kept/test/home.png"],
                KEPT
            );
            assert!(
                records
                    .iter()
                    .filter(|record| record.0 == "PUT")
                    .all(|record| !record.1.contains("kept")),
                "kept files must never be uploaded"
            );
            if required_kept {
                let error = result.unwrap_err().to_string();
                assert!(error.contains("replace the whole site"), "{error}");
                assert!(records.iter().all(|record| record.0 != "PUT"));
            } else {
                assert_eq!(result?, "https://qa-reports.netlify.app/");
            }
        }

        // Kept entries are untrusted: invalid digests, unsafe paths, and staged collisions fail
        // before Netlify is contacted.
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "hello")?;
        for (path, hash) in [
            ("_snapshot/images/kept/test/home.png", "XYZ"),
            ("_snapshot/../escape.png", KEPT),
            ("index.html", KEPT),
        ] {
            let mut manifest =
                Manifest::create(root.path(), Instant::now() + Duration::from_secs(60))?;
            let kept = super::super::KeptFiles::from([(
                path.to_owned(),
                super::super::site_state::StateFile {
                    size: 1,
                    hash: hash.into(),
                },
            )]);
            assert!(manifest.keep(&kept, MAX_FILES).is_err(), "{path}");
        }
        let mut manifest = Manifest::create(root.path(), Instant::now() + Duration::from_secs(60))?;
        let kept = super::super::KeptFiles::from([(
            "_snapshot/images/kept/a.png".to_owned(),
            super::super::site_state::StateFile {
                size: 1,
                hash: KEPT.into(),
            },
        )]);
        assert!(manifest.keep(&kept, 1).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn file_uploads_retry_transient_failures_without_recreating_deployments() -> Result<()> {
        for (status, persistent, expected_attempts, succeeds) in [
            (503, false, 2, true),
            (429, false, 2, true),
            (408, false, 2, true),
            (504, true, 6, false),
            (401, true, 1, false),
            (403, true, 1, false),
            (413, true, 1, false),
            (302, true, 1, false),
        ] {
            let uploads = Arc::new(AtomicUsize::new(0));
            let attempts = uploads.clone();
            let creates = Arc::new(AtomicUsize::new(0));
            let creations = creates.clone();
            let site = crate::test::browser_tests::Site::new(move |mut request| {
                let mut body = Vec::new();
                request.as_reader().read_to_end(&mut body).unwrap();
                let response = match request.method().as_str() {
                    "POST" => {
                        creations.fetch_add(1, Ordering::SeqCst);
                        json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared",
                            "required":[format!("{:x}", Sha1::digest(b"hello"))]})
                    }
                    "PUT" => {
                        assert_eq!(body, b"hello");
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 || persistent {
                            let _ = request.respond(
                                tiny_http::Response::from_string(
                                    json!({"message":format!("Upload rejected; token={TOKEN}")})
                                        .to_string(),
                                )
                                .with_status_code(status)
                                .with_header(
                                    tiny_http::Header::from_bytes("Retry-After", "0").unwrap(),
                                )
                                .with_header(
                                    tiny_http::Header::from_bytes(
                                        "Location",
                                        "https://example.com/leak",
                                    )
                                    .unwrap(),
                                ),
                            );
                            return;
                        }
                        json!({})
                    }
                    _ if request.url().ends_with("/deploys/deploy-123") => {
                        json!({"id":"deploy-123", "site_id":SITE_ID, "state":"ready", "ssl_url":"https://qa-reports.netlify.app/"})
                    }
                    _ => json!({"id":SITE_ID}),
                };
                let _ = request.respond(tiny_http::Response::from_string(response.to_string()));
            })?;
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "hello")?;
            let result = client(&site.origin).deploy(root.path(), &target()).await;
            assert_eq!(result.is_ok(), succeeds, "HTTP {status}: {result:?}");
            if let Err(error) = result {
                assert!(!format!("{error:#}").contains(TOKEN));
                assert!(error.to_string().contains(&format!("HTTP {status}")));
            }
            assert_eq!(
                uploads.load(Ordering::SeqCst),
                expected_attempts,
                "HTTP {status}"
            );
            assert_eq!(creates.load(Ordering::SeqCst), 1);
        }
        Ok(())
    }

    #[tokio::test]
    async fn preparing_deployments_support_reuse_empty_files_and_encoded_paths() -> Result<()> {
        for cached in [false, true] {
            let files = Arc::new(Mutex::new(serde_json::Value::Null));
            let manifest = files.clone();
            let uploads = Arc::new(Mutex::new(Vec::new()));
            let uploaded = uploads.clone();
            let polls = Arc::new(AtomicUsize::new(0));
            let checks = polls.clone();
            let site = crate::test::browser_tests::Site::new(move |mut request| {
                let method = request.method().as_str().to_owned();
                let mut body = Vec::new();
                request.as_reader().read_to_end(&mut body).unwrap();
                let response = if method == "POST" {
                    *manifest.lock().unwrap() = serde_json::from_slice(&body).unwrap();
                    json!({"id":"deploy-123", "site_id":SITE_ID, "state":"preparing"})
                } else if method == "PUT" {
                    uploaded
                        .lock()
                        .unwrap()
                        .push((request.url().to_owned(), body));
                    json!({})
                } else if request.url().ends_with("/deploys/deploy-123") {
                    match checks.fetch_add(1, Ordering::SeqCst) {
                        0 => json!({"id":"deploy-123", "site_id":SITE_ID, "state":"preparing"}),
                        1 => {
                            let required = if cached {
                                Vec::new()
                            } else {
                                manifest.lock().unwrap()["files"]
                                    .as_object()
                                    .unwrap()
                                    .values()
                                    .cloned()
                                    .collect::<Vec<_>>()
                            };
                            json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared", "required":required})
                        }
                        _ => {
                            json!({"id":"deploy-123", "site_id":SITE_ID, "state":"ready", "ssl_url":"https://qa-reports.netlify.app/"})
                        }
                    }
                } else {
                    json!({"id":SITE_ID})
                };
                let _ = request.respond(tiny_http::Response::from_string(response.to_string()));
            })?;
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "")?;
            std::fs::write(root.path().join("space % \u{00e9}.png"), "image")?;
            client(&site.origin).deploy(root.path(), &target()).await?;
            let records = uploads.lock().unwrap();
            assert_eq!(records.len(), if cached { 0 } else { 2 });
            if !cached {
                assert!(
                    records
                        .iter()
                        .any(|(path, body)| path.ends_with("/index.html") && body.is_empty())
                );
                assert!(
                    records
                        .iter()
                        .any(|(path, body)| path.ends_with("/space%20%25%20%C3%A9.png")
                            && body == b"image")
                );
            }
            assert_eq!(polls.load(Ordering::SeqCst), 3);
        }
        Ok(())
    }

    #[tokio::test]
    async fn unsafe_required_lists_and_wrong_deployments_fail_before_file_uploads() -> Result<()> {
        for response in [
            json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared"}),
            json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared", "required":["0".repeat(40)]}),
            json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared", "required":[TOKEN]}),
            json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared", "required":TOKEN}),
            json!({"id":"../escape", "site_id":SITE_ID, "state":"prepared", "required":[]}),
            json!({"id":"deploy-123", "site_id":"wrong-site", "state":"prepared", "required":[]}),
            json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared", "draft":true, "required":[]}),
        ] {
            let uploads = Arc::new(AtomicUsize::new(0));
            let attempts = uploads.clone();
            let site = crate::test::browser_tests::Site::new(move |mut request| {
                std::io::copy(request.as_reader(), &mut std::io::sink()).unwrap();
                if request.method().as_str() == "PUT" {
                    attempts.fetch_add(1, Ordering::SeqCst);
                }
                let body = if request.method().as_str() == "GET" {
                    json!({"id":SITE_ID})
                } else {
                    response.clone()
                };
                let _ = request.respond(tiny_http::Response::from_string(body.to_string()));
            })?;
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "hello")?;
            let error = client(&site.origin)
                .deploy(root.path(), &target())
                .await
                .unwrap_err();
            assert!(!format!("{error:#}").contains(TOKEN));
            assert_eq!(uploads.load(Ordering::SeqCst), 0);
        }
        Ok(())
    }

    #[tokio::test]
    async fn changed_staged_files_are_rejected_without_upload_retries() -> Result<()> {
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = calls.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            requests.fetch_add(1, Ordering::SeqCst);
            let _ = std::io::copy(request.as_reader(), &mut std::io::sink());
            let _ = request.respond(tiny_http::Response::empty(200));
        })?;
        let root = tempfile::tempdir()?;
        let path = root.path().join("index.html");
        let digest = format!("{:x}", Sha1::digest(b"hello"));
        for changed in ["world", "shorter?", "x"] {
            std::fs::write(&path, "hello")?;
            let manifest = Manifest::create(root.path(), Instant::now() + Duration::from_secs(5))?;
            std::fs::write(&path, changed)?;
            let error = client(&site.origin)
                .upload_files(&manifest, std::slice::from_ref(&digest), "deploy-123")
                .await
                .unwrap_err();
            assert!(error.to_string().contains("changed"), "{error:#}");
        }
        assert!(calls.load(Ordering::SeqCst) <= 1);
        Ok(())
    }

    #[test]
    fn manifest_validates_paths_deadlines_and_required_digests() -> Result<()> {
        let root = tempfile::tempdir()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(Manifest::create(root.path(), deadline).is_err());
        std::fs::write(root.path().join("index.html"), "hello")?;
        assert!(Manifest::create(root.path(), Instant::now()).is_err());
        let manifest = Manifest::create(root.path(), deadline)?;
        for path in [
            "",
            "../outside",
            "/outside",
            "C:/outside",
            "sub\\outside",
            "./index.html",
        ] {
            assert!(manifest.open_file(path).is_err(), "{path}");
        }
        let digest = format!("{:x}", Sha1::digest(b"hello"));
        assert_eq!(
            manifest
                .required_files(&[digest.clone(), digest.to_uppercase()])?
                .len(),
            1
        );
        assert_eq!(manifest.bytes, 5);
        assert!(Manifest::create_bounded(root.path(), deadline, 1, 5).is_ok());
        assert!(Manifest::create_bounded(root.path(), deadline, 0, 5).is_err());
        assert!(Manifest::create_bounded(root.path(), deadline, 1, 4).is_err());
        std::fs::create_dir(root.path().join("nested"))?;
        std::fs::write(root.path().join("nested/second.html"), "hello")?;
        assert!(Manifest::create_bounded(root.path(), deadline, 1, 10).is_err());
        let manifest = Manifest::create_bounded(root.path(), deadline, 2, 10)?;
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.contents.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn required_file_uploads_run_in_parallel_with_a_four_request_bound() -> Result<()> {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let workers = Arc::new(Mutex::new(Vec::new()));
        let running = active.clone();
        let peak = maximum.clone();
        let threads = workers.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let running = running.clone();
            let peak = peak.clone();
            let worker = std::thread::spawn(move || {
                assert_eq!(request.method().as_str(), "PUT");
                let count = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                std::io::copy(request.as_reader(), &mut std::io::sink()).unwrap();
                std::thread::sleep(Duration::from_millis(50));
                running.fetch_sub(1, Ordering::SeqCst);
                let _ = request.respond(tiny_http::Response::empty(200));
            });
            threads.lock().unwrap().push(worker);
        })?;
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "hello")?;
        for index in 0..8 {
            std::fs::write(
                root.path().join(format!("file-{index}.txt")),
                format!("content {index}"),
            )?;
        }
        let manifest = Manifest::create(root.path(), Instant::now() + Duration::from_secs(5))?;
        let required = manifest.contents.keys().cloned().collect::<Vec<_>>();
        let result = client(&site.origin)
            .upload_files(&manifest, &required, "deploy-123")
            .await;
        for worker in std::mem::take(&mut *workers.lock().unwrap()) {
            worker.join().unwrap();
        }
        result?;
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!((2..=UPLOAD_CONCURRENCY).contains(&maximum.load(Ordering::SeqCst)));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn manifest_rejects_symlink_roots_files_and_changed_parents() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("stage");
        std::fs::create_dir_all(root.join("assets"))?;
        std::fs::write(root.join("index.html"), "hello")?;
        std::fs::write(root.join("assets/image.png"), "image")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let manifest = Manifest::create(&root, deadline)?;
        let linked = directory.path().join("linked");
        std::os::unix::fs::symlink(&root, &linked)?;
        assert!(Manifest::create(&linked, deadline).is_err());
        std::os::unix::fs::symlink(root.join("index.html"), root.join("link.html"))?;
        assert!(Manifest::create(&root, deadline).is_err());
        std::fs::remove_file(root.join("link.html"))?;
        std::fs::rename(root.join("assets"), root.join("original"))?;
        std::os::unix::fs::symlink(root.join("original"), root.join("assets"))?;
        assert!(manifest.open_file("assets/image.png").is_err());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "publishes twice to NETLIFY_SITE_ID using NETLIFY_PAT; replaces the site's current content"]
    async fn live_publish_netlify_digest() -> Result<()> {
        let token = zeroize::Zeroizing::new(
            std::env::var("NETLIFY_PAT").context("Set NETLIFY_PAT for live deployment")?,
        );
        let mut target = target();
        target.site_id =
            std::env::var("NETLIFY_SITE_ID").context("Set NETLIFY_SITE_ID for live deployment")?;
        target.validate()?;
        let client = NetlifyClient::new(&token)?;
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("assets"))?;
        let marker = format!("Regressify digest deployment {}", uuid::Uuid::new_v4());
        let html = format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Regressify Deployment Check</title><link rel=\"stylesheet\" href=\"assets/site.css\"></head><body><h1>Regressify Deployment Check</h1><p>{marker}</p></body></html>"
        );
        std::fs::write(root.path().join("index.html"), html)?;
        let css = "body{font-family:sans-serif;margin:2rem;color:#252525;background:#f7faf9}h1{font-size:1.5rem}";
        std::fs::write(root.path().join("assets/site.css"), css)?;
        std::fs::write(root.path().join("assets/duplicate.css"), css)?;
        std::fs::write(
            root.path().join("_headers"),
            include_str!("../../snapshot/data/_headers"),
        )?;
        let public = Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        for attempt in 1..=2 {
            let url = client.deploy(root.path(), &target).await?;
            let response = public
                .get(&url)
                .send()
                .await
                .context("Cannot retrieve deployed smoke report")?;
            ensure!(
                response.status().is_success(),
                "Deployed smoke report is not reachable"
            );
            let body = response_bytes(response).await?;
            ensure!(
                String::from_utf8_lossy(&body).contains(&marker),
                "Published report does not contain the smoke marker"
            );
            let response = public
                .get(Url::parse(&url)?.join("assets/site.css")?)
                .send()
                .await?;
            ensure!(
                response.status().is_success(),
                "Deployed smoke asset is not reachable"
            );
            ensure!(
                response_bytes(response).await? == css.as_bytes(),
                "Published asset content differs from the staged file"
            );
            println!("Live Netlify digest deployment {attempt}/2 verified: {url}");
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "publishes NETLIFY_REPORT_ROOT twice using NETLIFY_PAT and NETLIFY_SITE_ID; replaces site content"]
    async fn live_publish_netlify_reports() -> Result<()> {
        let token = zeroize::Zeroizing::new(
            std::env::var("NETLIFY_PAT").context("Set NETLIFY_PAT for live deployment")?,
        );
        let mut target = target();
        target.site_id =
            std::env::var("NETLIFY_SITE_ID").context("Set NETLIFY_SITE_ID for live deployment")?;
        target.validate()?;
        let output = PathBuf::from(
            std::env::var_os("NETLIFY_REPORT_ROOT")
                .context("Set NETLIFY_REPORT_ROOT to existing report output")?,
        );
        let mut suites = Vec::new();
        for entry in std::fs::read_dir(&output)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && entry.path().join("html_report/config.js").is_file() {
                suites.push(
                    entry
                        .file_name()
                        .into_string()
                        .map_err(|_| anyhow!("Report suite names must be UTF-8"))?,
                );
            }
        }
        suites.sort();
        ensure!(!suites.is_empty(), "No existing report suites found");
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("report");
        super::super::stage_reports(&output, &suites, &root)?;
        let manifest = Manifest::create(&root, Instant::now() + Duration::from_secs(300))?;
        println!(
            "Live report verification: {} suites, {} files, {:.2} MiB staged; source reports are read-only",
            suites.len(),
            manifest.files.len(),
            manifest.bytes as f64 / (1024.0 * 1024.0)
        );
        let client = NetlifyClient::new(&token)?;
        let published = if let Ok(url) = std::env::var("NETLIFY_VERIFY_URL") {
            site_url(&url)?
        } else {
            let mut published = String::new();
            for attempt in 1..=2 {
                let started = Instant::now();
                published = client.deploy(&root, &target).await?;
                println!(
                    "Live report deployment {attempt}/2 ready in {:.1}s: {published}",
                    started.elapsed().as_secs_f64()
                );
            }
            published
        };
        let public = Client::builder()
            .https_only(true)
            // This host decorates HTTP/2 HTML with its public toolbar; verify original bytes.
            .http1_only()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()?;
        let base = Url::parse(&published)?;
        let verified = AtomicUsize::new(0);
        // Netlify consumes _headers as hosting policy, not a public report asset.
        let files = manifest
            .files
            .iter()
            .filter(|(path, _)| path.as_str() != "/_headers");
        stream::iter(files)
            .map(|(path, digest)| {
                let public = &public;
                let verified = &verified;
                let expected = manifest.contents[digest].bytes;
                let mut url = base.clone();
                async move {
                    {
                        let mut segments = url.path_segments_mut()
                            .map_err(|_| anyhow!("Invalid published report URL"))?;
                        segments.pop_if_empty().extend(path.trim_start_matches('/').split('/'));
                        if path.ends_with("/index.html") {
                            segments.pop().push("");
                        }
                    }
                    let response = public.get(url.clone()).header("Cache-Control", "no-cache")
                        .send().await
                        .map_err(|_| anyhow!("Cannot fetch published report asset {path}"))?;
                    ensure!(response.status().is_success(), "Published report asset {path} returned HTTP {}", response.status().as_u16());
                    let mut body = response.bytes_stream();
                    let mut hasher = Sha1::new();
                    let mut received = 0;
                    while let Some(chunk) = body.next().await {
                        let chunk = chunk.map_err(|_| anyhow!("Cannot read published report asset {path}"))?;
                        received += chunk.len() as u64;
                        ensure!(received <= expected, "Published report asset {path} is larger than staged content: {received} bytes received, {expected} expected; URL {url}");
                        hasher.update(&chunk);
                    }
                    ensure!(received == expected && format!("{:x}", hasher.finalize()) == *digest,
                        "Published report asset {path} differs from staged content");
                    let completed = verified.fetch_add(1, Ordering::Relaxed) + 1;
                    if completed == 1 || completed.is_multiple_of(50) {
                        println!("Verified {completed} published report assets against staged hashes");
                    }
                    Ok::<_, anyhow::Error>(())
                }
            })
            .buffer_unordered(UPLOAD_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;
        println!(
            "Verified all {} public report assets: {published}",
            verified.load(Ordering::Relaxed)
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires NETLIFY_PAT and NETLIFY_SITE_ID; validates site access without publishing"]
    async fn live_deployment_netlify_preflight() -> Result<()> {
        let token = zeroize::Zeroizing::new(
            std::env::var("NETLIFY_PAT").context("Set NETLIFY_PAT for live validation")?,
        );
        let mut target = target();
        target.site_id =
            std::env::var("NETLIFY_SITE_ID").context("Set NETLIFY_SITE_ID for live validation")?;
        target.validate()?;
        let client = NetlifyClient::new(&token)?;
        let site: Site = client.get(&format!("sites/{}", target.site_id)).await?;
        ensure!(
            site.id.eq_ignore_ascii_case(&target.site_id),
            "Netlify returned a different site"
        );
        println!(
            "Live Netlify validation succeeded: Git-only restriction {}, locked deployment {}; no deployment created",
            site.prevent_non_git_prod_deploys == Some(true),
            site.published_deploy
                .is_some_and(|deploy| deploy.locked == Some(true))
        );
        Ok(())
    }

    #[tokio::test]
    async fn digest_deployment_authenticates_retries_reads_and_preserves_staged_files() -> Result<()>
    {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let reads = Arc::new(AtomicUsize::new(0));
        let attempts = reads.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let path = request.url().to_owned();
            let method = request.method().as_str().to_owned();
            let auth = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Authorization"))
                .map(|header| header.value.to_string());
            assert_eq!(auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
            let content_type = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Content-Type"))
                .map(|header| header.value.to_string());
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body).unwrap();
            let required = if method == "POST" {
                let manifest: serde_json::Value = serde_json::from_slice(&body).unwrap();
                manifest["files"]
                    .as_object()
                    .unwrap()
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            captured
                .lock()
                .unwrap()
                .push((method.clone(), path.clone(), content_type, body));
            let response = if path == format!("/api/v1/sites/{SITE_ID}") {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    let _ = request.respond(tiny_http::Response::empty(503));
                    return;
                }
                json!({"id": SITE_ID, "prevent_non_git_prod_deploys": null,
                    "published_deploy": {"locked": null}})
            } else if method == "PUT" {
                json!({})
            } else {
                json!({"id":"deploy-123", "site_id": SITE_ID,
                    "state": if method == "POST" { "prepared" } else { "ready" },
                    "required":required,
                    "draft": null,
                    "ssl_url": "https://qa-reports.netlify.app/"})
            };
            let _ = request.respond(tiny_http::Response::from_string(response.to_string()));
        })?;
        let directory = tempfile::tempdir()?;
        let output = directory.path().join("output");
        std::fs::create_dir_all(output.join("one/html_report"))?;
        std::fs::write(output.join("index.html"), "report")?;
        std::fs::write(output.join("one/html_report/index.html"), "suite report")?;
        let report = json!({"id": "one", "tests": []});
        std::fs::write(
            output.join("one/html_report/config.js"),
            format!("report({report});"),
        )?;
        crate::snapshot::generate_site(
            &output,
            &["one".into()],
            &crate::cli::args::SnapshotArgs {
                test_suite: Vec::new(),
                title: None,
                heading: None,
                subheading: None,
            },
            &crate::config::WorkspaceConfig::default(),
        )?;
        let expected_files = crate::snapshot::FRONTEND_FILES
            .iter()
            .filter(|(name, _)| *name != "gitignore")
            .count()
            + 4;
        let root = directory.path().join("publish");
        super::super::stage_reports(&output, &["one".into()], &root)?;
        assert_eq!(
            client(&site.origin).deploy(&root, &target()).await?,
            "https://qa-reports.netlify.app/"
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        let records = requests.lock().unwrap();
        let uploads = records
            .iter()
            .filter(|record| record.0 == "POST")
            .collect::<Vec<_>>();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].1, format!("/api/v1/sites/{SITE_ID}/deploys"));
        assert_eq!(uploads[0].2.as_deref(), Some("application/json"));
        let manifest: serde_json::Value = serde_json::from_slice(&uploads[0].3)?;
        assert_eq!(manifest["async"], true);
        assert!(manifest["files"].get("/_snapshot/status.json").is_some());
        assert_eq!(manifest["files"].as_object().unwrap().len(), expected_files);
        let files = records
            .iter()
            .filter(|record| record.0 == "PUT")
            .collect::<Vec<_>>();
        assert_eq!(files.len(), expected_files);
        for file in files {
            assert_eq!(file.2.as_deref(), Some("application/octet-stream"));
            let path = file
                .1
                .strip_prefix("/api/v1/deploys/deploy-123/files/")
                .unwrap();
            assert_eq!(file.3, std::fs::read(root.join(path))?);
            assert_eq!(
                manifest["files"][format!("/{path}")],
                format!("{:x}", Sha1::digest(&file.3))
            );
        }
        assert!(
            records
                .iter()
                .any(|record| record.1 == format!("/api/v1/sites/{SITE_ID}/deploys/deploy-123"))
        );
        assert!(records.iter().all(|record| !record.1.contains(TOKEN)
            && !String::from_utf8_lossy(&record.3).contains(TOKEN)));
        Ok(())
    }

    #[tokio::test]
    async fn upload_can_outlast_the_default_request_timeout() -> Result<()> {
        let uploads = Arc::new(AtomicUsize::new(0));
        let attempts = uploads.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            std::io::copy(request.as_reader(), &mut std::io::sink()).unwrap();
            let response = if request.method().as_str() == "PUT" {
                attempts.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(300));
                json!({})
            } else if request.method().as_str() == "POST" {
                json!({"id":"deploy-123", "site_id":SITE_ID, "state":"prepared",
                    "required":[format!("{:x}", Sha1::digest(b"report"))]})
            } else if request.url().ends_with("/deploys/deploy-123") {
                json!({"id":"deploy-123", "site_id":SITE_ID, "state":"ready",
                    "ssl_url":"https://qa-reports.netlify.app/"})
            } else {
                json!({"id":SITE_ID})
            };
            let _ = request.respond(tiny_http::Response::from_string(response.to_string()));
        })?;
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        let mut client = client(&site.origin);
        let default_deadline = client.deadline;
        client.client = Client::builder()
            .timeout(Duration::from_millis(25))
            .build()?;
        client.deadline = Duration::from_secs(5);
        assert_eq!(
            client.deploy(root.path(), &target()).await?,
            "https://qa-reports.netlify.app/"
        );
        assert_eq!(uploads.load(Ordering::SeqCst), 1);
        assert_eq!(default_deadline, Duration::from_secs(60 * 60));
        client.deadline = Duration::from_millis(150);
        let error = client
            .deploy(root.path(), &target())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("deadline") || error.contains("timed out"),
            "{error}"
        );
        assert_eq!(uploads.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[test]
    fn upload_stall_warnings_repeat_and_reset_when_progress_resumes() {
        let mut progress = UploadStallMonitor::default();
        let length = 4 * 1024;
        for seconds in [0, 59] {
            assert!(
                progress
                    .warning(0, length, Duration::from_secs(seconds))
                    .is_none()
            );
        }
        for seconds in [60, 120] {
            let warning = progress
                .warning(0, length, Duration::from_secs(seconds))
                .unwrap();
            assert!(
                warning.contains(&format!("no progress for {seconds}s")),
                "{warning}"
            );
            assert!(warning.contains("0 of 4 KiB sent"), "{warning}");
            assert!(warning.contains("Keep waiting or choose Stop"), "{warning}");
        }
        assert!(
            progress
                .warning(2 * 1024, length, Duration::from_secs(130))
                .is_none()
        );
        assert!(
            progress
                .warning(2 * 1024, length, Duration::from_secs(189))
                .is_none()
        );
        let warning = progress
            .warning(2 * 1024, length, Duration::from_secs(190))
            .unwrap();
        assert!(warning.contains("no progress for 60s"), "{warning}");
        assert!(warning.contains("2 of 4 KiB sent"), "{warning}");
        assert!(
            progress
                .warning(3 * 1024, length, Duration::from_secs(200))
                .is_none()
        );
    }

    #[test]
    fn completed_upload_waits_for_acceptance_without_claiming_a_stalled_transfer() {
        let mut progress = UploadStallMonitor::default();
        let length = 4 * 1024;
        assert!(
            progress
                .warning(length, length, Duration::from_secs(1))
                .is_none()
        );
        assert!(
            progress
                .warning(length, length, Duration::from_secs(60))
                .is_none()
        );
        for seconds in [61, 121] {
            let warning = progress
                .warning(length, length, Duration::from_secs(seconds))
                .unwrap();
            assert!(warning.contains("sent all 4 KiB"), "{warning}");
            assert!(
                warning.contains(&format!(
                    "awaiting provider acceptance after {}s",
                    seconds - 1
                )),
                "{warning}"
            );
            assert!(!warning.contains("no progress"), "{warning}");
            assert!(warning.contains("Keep waiting or choose Stop"), "{warning}");
        }
    }

    #[tokio::test]
    async fn failed_deploy_creation_is_not_retried_and_errors_are_redacted() -> Result<()> {
        let uploads = Arc::new(AtomicUsize::new(0));
        let attempts = uploads.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            if request.method().as_str() == "POST" {
                attempts.fetch_add(1, Ordering::SeqCst);
                std::io::copy(request.as_reader(), &mut std::io::sink()).unwrap();
                let _ = request.respond(tiny_http::Response::from_string(
                    json!({"message":format!("Upload exceeds the project size limit; token={TOKEN}")}).to_string()
                ).with_status_code(503).with_header(
                    tiny_http::Header::from_bytes("x-nf-request-id", "request-123").unwrap()
                ));
            } else {
                let _ = request.respond(tiny_http::Response::from_string(
                    json!({"id":SITE_ID}).to_string(),
                ));
            }
        })?;
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        let error = client(&site.origin)
            .deploy(root.path(), &target())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("HTTP 503"));
        assert!(
            error
                .to_string()
                .contains("Upload exceeds the project size limit")
        );
        assert!(error.to_string().contains("request-123"));
        assert!(!format!("{error:#}").contains(TOKEN));
        assert_eq!(uploads.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn authentication_redirects_and_oversized_responses_fail_before_upload() -> Result<()> {
        for (status, body) in [
            (401, TOKEN.to_string()),
            (403, TOKEN.to_string()),
            (404, TOKEN.to_string()),
            (302, TOKEN.to_string()),
            (200, TOKEN.to_string()),
            (200, "x".repeat(MAX_RESPONSE_BYTES + 1)),
            (413, "x".repeat(1024 * 1024 + 1)),
            (200, json!({"id":"wrong-site"}).to_string()),
            (
                200,
                json!({"id":SITE_ID, "prevent_non_git_prod_deploys":true,
                    "published_deploy":{"locked":null}})
                .to_string(),
            ),
            (
                200,
                json!({"id":SITE_ID, "prevent_non_git_prod_deploys":null,
                    "published_deploy":{"locked":true}})
                .to_string(),
            ),
            (
                200,
                json!({"id":SITE_ID, "prevent_non_git_prod_deploys":TOKEN}).to_string(),
            ),
            (
                200,
                json!({"id":SITE_ID, "published_deploy":{"locked":TOKEN}}).to_string(),
            ),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let attempts = calls.clone();
            let site = crate::test::browser_tests::Site::new(move |request| {
                attempts.fetch_add(1, Ordering::SeqCst);
                assert_eq!(request.method().as_str(), "GET");
                let response = tiny_http::Response::from_string(body.clone())
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes("Location", "https://example.com/leak")
                            .unwrap(),
                    );
                let _ = request.respond(response);
            })?;
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "report")?;
            let error = client(&site.origin)
                .deploy(root.path(), &target())
                .await
                .unwrap_err();
            assert!(!format!("{error:#}").contains(TOKEN));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        Ok(())
    }

    #[tokio::test]
    async fn polling_fails_for_terminal_states_changed_identity_and_deadlines() -> Result<()> {
        for (state, site_id, draft, expected) in [
            ("error", SITE_ID, json!(false), "deployment failed"),
            ("rejected", SITE_ID, json!(null), "deployment failed"),
            ("processing", SITE_ID, json!(false), "deadline"),
            ("ready", "wrong-site", json!(false), "different deployment"),
            ("ready", SITE_ID, json!(true), "draft instead"),
            ("ready", SITE_ID, json!(TOKEN), "Invalid Netlify response"),
        ] {
            let site = crate::test::browser_tests::Site::new(move |mut request| {
                let response = if request.url() == format!("/api/v1/sites/{SITE_ID}") {
                    json!({"id":SITE_ID})
                } else {
                    std::io::copy(request.as_reader(), &mut std::io::sink()).unwrap();
                    json!({"id":"deploy-123", "site_id": site_id,
                        "state":state, "draft":draft, "error_message":format!("Image processing failed; token={TOKEN}"),
                        "ssl_url":"https://qa-reports.netlify.app/"})
                };
                let _ = request.respond(tiny_http::Response::from_string(response.to_string()));
            })?;
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "report")?;
            let mut client = client(&site.origin);
            if state == "processing" {
                client.deadline = Duration::from_millis(150);
            }
            let error = client.deploy(root.path(), &target()).await.unwrap_err();
            assert!(!format!("{error:#}").contains(TOKEN));
            assert!(error.to_string().contains(expected));
            if matches!(state, "error" | "rejected") {
                assert!(error.to_string().contains("Image processing failed"));
                assert!(error.to_string().contains("deploy-123"));
            }
        }
        Ok(())
    }

    #[test]
    fn unsafe_urls_and_azure_access_rules_are_rejected() -> Result<()> {
        for url in [
            "http://example.com",
            "https://user:secret@example.com",
            "https://example.com/?token=secret",
            "javascript:alert(1)",
        ] {
            assert!(site_url(url).is_err());
        }
        assert_eq!(
            site_url("https://reports.example.com")?,
            "https://reports.example.com/"
        );
        let root = tempfile::tempdir()?;
        let path = root.path().join("staticwebapp.config.json");
        std::fs::write(
            &path,
            include_bytes!("../../snapshot/data/staticwebapp.config.json"),
        )?;
        assert!(validate_static_configuration(root.path()).is_ok());
        for config in [
            json!({"auth":{}}),
            json!({"networking":{}}),
            json!({"routes":[{"allowedRoles":["authenticated"]}]}),
        ] {
            std::fs::write(&path, config.to_string())?;
            assert!(validate_static_configuration(root.path()).is_err());
        }
        Ok(())
    }
}
