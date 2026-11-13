use std::{io::SeekFrom, path::Path, time::Duration};

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{DeployTarget, DeploymentProvider, diagnostics, optimization, package::Package, phase};

const BLOCK_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwaTier {
    Free,
    Standard,
    Unknown,
}

impl SwaTier {
    fn from_response(info: &Value) -> Self {
        match info["sku"].as_str() {
            Some(sku) if sku.eq_ignore_ascii_case("Standard") => Self::Standard,
            Some(sku) if sku.eq_ignore_ascii_case("Free") => Self::Free,
            _ => Self::Unknown,
        }
    }

    fn limit(self) -> u64 {
        match self {
            Self::Standard => 500 * 1024 * 1024,
            Self::Free | Self::Unknown => 250 * 1024 * 1024,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Standard => "Standard",
            Self::Unknown => "unknown (using the Free limit conservatively)",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    response: Option<Value>,
    is_success_status_code: bool,
    status_code: u16,
    #[serde(default)]
    error_message: Value,
}

pub struct SwaClient {
    client: Client,
    endpoint: Url,
    authorization: HeaderValue,
    slice: u32,
    correlation: String,
    backoff: Duration,
    poll_interval: Duration,
    deadline: Duration,
    block_bytes: usize,
}

fn token_endpoint(token: &str) -> Result<(Url, u32)> {
    ensure!(
        token.is_ascii() && (104..=256).contains(&token.len()),
        "Invalid Azure deployment token format"
    );
    let (prefix, suffix) = token
        .split_once('-')
        .context("Invalid Azure deployment token format")?;
    ensure!(
        prefix.len() >= 64
            && suffix.len() >= 39
            && prefix[..64].bytes().all(|byte| byte.is_ascii_hexdigit())
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-'),
        "Invalid Azure deployment token format"
    );
    let slice = if prefix.len() == 64 {
        0
    } else {
        prefix[64..]
            .parse::<u32>()
            .context("Invalid Azure deployment token slice")?
    };
    let region = u16::from_str_radix(&suffix[36..39], 16)
        .context("Invalid Azure deployment token region")?;
    let (region, version) = match region {
        0 => ("hk1", ""),
        3 => ("am2", ""),
        15 => ("eus2", ""),
        16 => ("dm1", ""),
        30 => ("wus2", ""),
        21 | 26 => ("msftinthk1", "beta"),
        33 => ("euapbn1", "canary"),
        34 => ("euapdm1", "canary"),
        _ => bail!("Unsupported Azure deployment token region"),
    };
    let slice_label = if slice == 0 {
        String::new()
    } else {
        format!("{slice}.")
    };
    Ok((
        Url::parse(&format!(
            "https://content-{region}.infrastructure.{slice_label}azurestaticapps{version}.net"
        ))?,
        slice,
    ))
}

pub fn validate_token(token: &str) -> Result<()> {
    token_endpoint(token).map(|_| ())
}

impl SwaClient {
    pub fn new(token: &str) -> Result<Self> {
        let (endpoint, slice) = token_endpoint(token)?;
        let mut authorization = HeaderValue::from_str(&format!("token {token}"))
            .map_err(|_| anyhow!("Invalid Azure deployment token"))?;
        authorization.set_sensitive(true);
        Ok(Self {
            client: Client::builder()
                .user_agent("staticsites-github-client")
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(180))
                .build()?,
            endpoint,
            authorization,
            slice,
            correlation: uuid::Uuid::new_v4().to_string(),
            backoff: Duration::from_secs(1),
            poll_interval: Duration::from_secs(5),
            deadline: Duration::from_secs(30 * 60),
            block_bytes: BLOCK_BYTES,
        })
    }

    async fn send(
        &self,
        endpoint: crate::diagnostics::record::SafeValue,
        request: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        for attempt in 0..6 {
            diagnostics::request_started(DeploymentProvider::AzureSwa, endpoint, attempt + 1);
            let started = std::time::Instant::now();
            let result = request().send().await;
            diagnostics::response_received(
                DeploymentProvider::AzureSwa,
                endpoint,
                attempt + 1,
                result
                    .as_ref()
                    .ok()
                    .map(|response| response.status().as_u16()),
                started.elapsed(),
            );
            let mut retry_after = None;
            let error = match result {
                Ok(response) if response.status().is_success() => return Ok(response),
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
                        DeploymentProvider::AzureSwa,
                        &self.authorization,
                    )
                    .await;
                    if !status.is_server_error() && !matches!(status.as_u16(), 408 | 429) {
                        return Err(error);
                    }
                    error
                }
                Err(error) => anyhow!(
                    "Azure request {}; check the connection, proxy, and Azure service status",
                    if error.is_timeout() {
                        "timed out"
                    } else {
                        "could not connect"
                    }
                ),
            };
            if attempt == 5 {
                return Err(error);
            }
            let delay = retry_after.unwrap_or(self.backoff * (1 << attempt));
            diagnostics::retry(DeploymentProvider::AzureSwa, endpoint, attempt + 2, delay);
            phase(&format!(
                "Azure retry {}/6 in {:.1}s: {error}",
                attempt + 2,
                delay.as_secs_f64()
            ));
            tokio::time::sleep(delay).await;
        }
        bail!("Azure request retry budget exhausted")
    }

    async fn api(&self, operation: &str, body: Option<&Value>) -> Result<Value> {
        use crate::diagnostics::record::SafeValue;
        let endpoint = match operation {
            "validateapitoken" => SafeValue::Preflight,
            "checkstatus" => SafeValue::Poll,
            "updatestatus" => SafeValue::Publication,
            _ => SafeValue::Upload,
        };
        let mut url = self.endpoint.join(&format!("/api/upload/{operation}"))?;
        url.query_pairs_mut()
            .append_pair("apiVersion", "v1")
            .append_pair("deploymentCorrelationId", &self.correlation);
        let response = self
            .send(endpoint, || {
                let request = self
                    .client
                    .post(url.clone())
                    .header(AUTHORIZATION, self.authorization.clone())
                    .header("Content-Type", "application/json; charset=utf-8");
                match body {
                    Some(body) => request.json(body),
                    None => request.header("Content-Length", 0),
                }
            })
            .await
            .map_err(|error| anyhow!("Azure {operation}: {error}"))?;
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| anyhow!("Cannot read Azure response"))?;
            ensure!(
                bytes.len() + chunk.len() <= 1024 * 1024,
                "Azure response exceeds 1 MiB"
            );
            bytes.extend_from_slice(&chunk);
        }
        let response: Envelope = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("Invalid Azure response envelope"))?;
        if !response.is_success_status_code {
            return Err(diagnostics::failure(
                DeploymentProvider::AzureSwa,
                operation,
                Some(response.status_code),
                &json!({"errorMessage": response.error_message, "response": response.response}),
                &self.authorization,
            ));
        }
        Ok(response.response.unwrap_or(Value::Null))
    }

    fn blob_url(&self, value: &str) -> Result<Url> {
        let url = Url::parse(value).map_err(|_| anyhow!("Azure returned an invalid upload URL"))?;
        let azure = url.scheme() == "https"
            && url
                .host_str()
                .is_some_and(|host| host.ends_with(".blob.core.windows.net"));
        let test = cfg!(test)
            && url.origin() == self.endpoint.origin()
            && url.host_str() == Some("127.0.0.1");
        ensure!(
            (azure || test)
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "Azure returned an untrusted upload URL"
        );
        Ok(url)
    }

    async fn upload(&self, url: &Url, package: &Package) -> Result<()> {
        use futures::{StreamExt, TryStreamExt, stream};
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let length = package.file.as_file().metadata()?.len();
        let total = length.div_ceil(self.block_bytes as u64);
        let completed = AtomicU64::new(0);
        let blocks = (0..total)
            .map(|index| STANDARD.encode(format!("{}-{index:08}", self.correlation)))
            .collect::<Vec<_>>();
        stream::iter(blocks.iter().enumerate())
            .map(|(index, id)| {
                let completed = &completed;
                async move {
                    let offset = index as u64 * self.block_bytes as u64;
                    let mut input = tokio::fs::File::open(package.file.path()).await?;
                    input.seek(SeekFrom::Start(offset)).await?;
                    let mut chunk =
                        vec![0; (length - offset).min(self.block_bytes as u64) as usize];
                    input.read_exact(&mut chunk).await?;
                    let count = chunk.len() as u64;
                    {
                        use crate::diagnostics::{record::*, runtime};
                        runtime::event(
                            Stage::ApiUploads,
                            DiagnosticEvent::Started,
                            Severity::Debug,
                            [
                                (Field::Provider, FieldValue::Kind(SafeValue::Azure)),
                                (Field::BatchCount, FieldValue::Number(total)),
                                (Field::Bytes, FieldValue::Number(count)),
                            ],
                        );
                    }
                    let mut destination = url.clone();
                    if total > 1 {
                        destination
                            .query_pairs_mut()
                            .append_pair("comp", "block")
                            .append_pair("blockid", id);
                    }
                    self.send(crate::diagnostics::record::SafeValue::Upload, || {
                        let request = self
                            .client
                            .put(destination.clone())
                            .header("x-ms-version", "2023-11-03")
                            .header("x-ms-client-request-id", &self.correlation)
                            .body(chunk.clone());
                        if total == 1 {
                            request.header("x-ms-blob-type", "BlockBlob")
                        } else {
                            request
                        }
                    })
                    .await?;
                    let sent = completed.fetch_add(count, Ordering::Relaxed) + count;
                    phase(&format!(
                        "Azure upload acknowledged: {} of {} KiB ({:.1}%)",
                        sent / 1024,
                        length / 1024,
                        sent as f64 * 100.0 / length as f64
                    ));
                    Ok::<_, anyhow::Error>(())
                }
            })
            .buffer_unordered(4)
            .try_collect::<Vec<_>>()
            .await?;
        if total > 1 {
            phase(&format!("Committing {total} Azure upload blocks"));
            let mut destination = url.clone();
            destination
                .query_pairs_mut()
                .append_pair("comp", "blocklist");
            let body = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>{}</BlockList>",
                blocks
                    .iter()
                    .map(|id| format!("<Latest>{id}</Latest>"))
                    .collect::<String>()
            );
            self.send(crate::diagnostics::record::SafeValue::Upload, || {
                self.client
                    .put(destination.clone())
                    .header("x-ms-version", "2023-11-03")
                    .header("Content-Type", "application/xml")
                    .body(body.clone())
            })
            .await?;
        }
        Ok(())
    }

    pub async fn deploy(&self, root: &Path, target: &DeployTarget) -> Result<String> {
        self.deploy_with_stats(root, target, &mut super::DeploymentStats::default())
            .await
    }

    pub(super) async fn deploy_with_stats(
        &self,
        root: &Path,
        target: &DeployTarget,
        stats: &mut super::DeploymentStats,
    ) -> Result<String> {
        target.validate()?;
        ensure!(
            target.provider == DeploymentProvider::AzureSwa,
            "Azure client requires an Azure target"
        );
        let deadline = std::time::Instant::now() + self.deadline;
        let operation = async {
            phase("Validating Azure deployment target");
            let info = self.api("validateapitoken", None).await?;
            let tier = SwaTier::from_response(&info);
            phase(&format!(
                "Azure tier: {}; per-environment storage limit: {} MB",
                tier.label(),
                tier.limit() / (1024 * 1024)
            ));
            let settings = root.join("staticwebapp.config.json");
            if settings.is_file() {
                let data: Value = serde_json::from_slice(&std::fs::read(settings)?)?;
                ensure!(
                    tier == SwaTier::Standard
                        || data.get("auth").is_none() && data.get("networking").is_none(),
                    "This report's auth/networking configuration requires the Azure Standard SKU"
                );
            }
            phase("Packaging selected reports");
            let directory = root.to_path_buf();
            let diagnostic_context = crate::diagnostics::runtime::current();
            let (package, original_bytes) = tokio::task::spawn_blocking(move || crate::diagnostics::runtime::in_scope(diagnostic_context, || {
                let prepared = optimization::prepare(&directory, tier.limit(), deadline)?;
                let original_bytes = prepared.original_bytes();
                let package = Package::create(prepared.path(), 100_000)?;
                ensure!(package.bytes <= tier.limit(), "Report size changed during packaging and exceeds the Azure {} storage limit; retry deployment", tier.label());
                Ok::<_, anyhow::Error>((package, original_bytes))
            })).await??;
            *stats = super::DeploymentStats {
                file_count: package.files,
                original_bytes,
                total_bytes: package.bytes,
                state_dropped: false,
            };
            let mut request = json!({
                "eventInfo": {"repoUrl": null, "isPullRequest": false, "isNamedEnvironment": !target.environment.is_empty(),
                    "pullRequestId": null, "pullRequestTitle": null, "headBranch": null, "baseBranch": null,
                    "environmentName": if target.environment.is_empty() { None } else { Some(&target.environment) },
                    "tenantId": null, "defaultHostname": null, "slice": self.slice},
                "uploadInfo": {"totalAppSizeInBytes": package.bytes, "apiSizeInBytes": 0, "hasFunctions": false,
                    "hasDataApiFiles": false, "hasDataApiConfigFile": false, "databaseType": "", "hasRoutes": false,
                    "status": "RequestingUpload", "defaultFileType": "index.html", "apiContentHash": null,
                    "configuredRoles": ["administrator"], "appFileCount": package.files, "functionLanguage": null,
                    "functionLanguageVersion": null, "serverRenderFramework": "StaticWebApp", "deploymentProvider": "GitHub",
                    "maxSingleFileSizeInBytes": package.maximum_file, "backendStartupCommandType": 0,
                    "shouldDeployToWebApp": false, "tenantId": "", "defaultHostname": null, "slice": self.slice},
                "pollingInfo": null
            });
            phase(&format!(
                "Requesting Azure upload for {} files ({:.2} MiB content)",
                package.files,
                package.bytes as f64 / (1024.0 * 1024.0)
            ));
            let response = self.api("request", Some(&request)).await?;
            ensure!(
                target.environment.is_empty() || response["allowStagingEnvironments"] != false,
                "This Azure target does not allow named environments"
            );
            let upload = self.blob_url(
                response["packageUris"]["app"]
                    .as_str()
                    .context("Azure returned no app upload URL")?,
            )?;
            let mut polling = response["pollingInfo"]
                .as_object()
                .cloned()
                .context("Azure returned no deployment polling information")?;
            polling.insert("slice".into(), json!(self.slice));
            request["pollingInfo"] = json!(polling);
            phase("Uploading selected reports");
            self.upload(&upload, &package).await?;
            request["uploadInfo"]["status"] = json!("Succeeded");
            phase("Azure upload complete; submitting deployment for processing");
            self.api("updatestatus", Some(&request)).await?;
            phase("Waiting for Azure deployment");
            let polling_started = std::time::Instant::now();
            let mut polls = 0;
            loop {
                polls += 1;
                let result = self.api("checkstatus", Some(&json!(polling))).await?;
                match result["deploymentStatus"].as_str() {
                    Some("Succeeded") => {
                        let value = result["siteUrl"]
                            .as_str()
                            .context("Azure deployment succeeded without a site URL")?;
                        let url = Url::parse(value).context("Invalid deployed site URL")?;
                        ensure!(
                            url.scheme() == "https"
                                && url.username().is_empty()
                                && url.password().is_none()
                                && url.query().is_none()
                                && url.fragment().is_none(),
                            "Azure returned an unsafe site URL"
                        );
                        phase(&format!("Deployed: {url}"));
                        diagnostics::published(DeploymentProvider::AzureSwa, Some(url.as_str()));
                        return Ok(url.to_string());
                    }
                    Some("Failed" | "Canceled" | "Cancelled") => {
                        let error = diagnostics::failure(
                            DeploymentProvider::AzureSwa,
                            "deployment",
                            None,
                            &result,
                            &self.authorization,
                        );
                        bail!("{error} Correlation ID: {}", self.correlation);
                    }
                    Some(status) => {
                        let status = if matches!(
                            status,
                            "InProgress"
                                | "NotStarted"
                                | "Pending"
                                | "Running"
                                | "Queued"
                                | "Building"
                                | "Deploying"
                        ) {
                            status
                        } else {
                            "Processing"
                        };
                        phase(&format!(
                            "Azure status: {status}; poll {polls}, {}s waiting; next check in {:.1}s",
                            polling_started.elapsed().as_secs(),
                            self.poll_interval.as_secs_f64()
                        ));
                        tokio::time::sleep(self.poll_interval).await;
                    }
                    None => bail!("Azure returned no deployment status"),
                }
            }
        };
        tokio::time::timeout(
            self.deadline,
            super::with_heartbeat("Azure deployment", operation),
        )
        .await
        .context("Azure deployment exceeded its 30-minute deadline")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    fn token() -> String {
        format!("{}-{}00f", "a".repeat(64), "b".repeat(36))
    }

    #[test]
    fn storage_budget_uses_the_detected_tier_and_falls_back_conservatively() {
        for (info, expected) in [
            (json!({"sku":"Free"}), 250),
            (json!({"sku":"Standard"}), 500),
            (json!({"sku":"standard"}), 500),
            (json!({}), 250),
            (json!({"sku":"unexpected"}), 250),
        ] {
            assert_eq!(
                SwaTier::from_response(&info).limit(),
                expected * 1024 * 1024
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires AZURE_STATIC_WEB_APPS_API_TOKEN; validates the target without publishing"]
    async fn live_deployment_azure_preflight() -> Result<()> {
        let token = zeroize::Zeroizing::new(
            std::env::var("AZURE_STATIC_WEB_APPS_API_TOKEN")
                .context("Set AZURE_STATIC_WEB_APPS_API_TOKEN for live validation")?,
        );
        let client = SwaClient::new(&token)?;
        let info = client.api("validateapitoken", None).await?;
        let tier = SwaTier::from_response(&info);
        ensure!(
            tier != SwaTier::Unknown,
            "Azure did not return a recognized Free/Standard SKU"
        );
        println!(
            "Live Azure validation succeeded: tier {}, storage budget {} MiB; no deployment created",
            tier.label(),
            tier.limit() / (1024 * 1024)
        );
        Ok(())
    }

    #[test]
    fn token_with_hyphenated_suffix_routes_to_its_slice() {
        let token = format!(
            "{}12-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb00f{}003",
            "a".repeat(64),
            "c".repeat(10)
        );
        let (url, slice) = token_endpoint(&token).unwrap();
        assert_eq!(slice, 12);
        assert_eq!(
            url.host_str(),
            Some("content-eus2.infrastructure.12.azurestaticapps.net")
        );
        validate_token(&token).unwrap();
        let client = SwaClient::new(&token).unwrap();
        assert!(client.authorization.is_sensitive());
        assert_eq!(
            client.authorization.to_str().unwrap(),
            format!("token {token}")
        );
    }

    #[test]
    fn token_regions_and_untrusted_upload_urls_are_validated_without_secret_errors() {
        let (url, slice) = token_endpoint(&token()).unwrap();
        assert_eq!(slice, 0);
        assert_eq!(
            url.host_str(),
            Some("content-eus2.infrastructure.azurestaticapps.net")
        );
        for token in [
            "not-a-token".into(),
            "\u{1f600}".repeat(104),
            format!("{}-{}00f", "g".repeat(64), "b".repeat(36)),
            format!("{}xx-{}00f", "a".repeat(64), "b".repeat(36)),
            format!("{}12-{}00f", "a".repeat(64), "b".repeat(35)),
            format!("{}-{}00f\r\n", "a".repeat(64), "b".repeat(36)),
            format!("{}-{}00f!", "a".repeat(64), "b".repeat(36)),
            format!("{}-{}0-0", "a".repeat(64), "b".repeat(36)),
            format!("{}-{}fff", "a".repeat(64), "b".repeat(36)),
        ] {
            let error = token_endpoint(&token).unwrap_err().to_string();
            assert!(!error.contains(&token));
        }
        let client = SwaClient::new(&token()).unwrap();
        assert!(
            client
                .blob_url("https://example.com/blob?sig=secret")
                .is_err()
        );
        assert!(
            client
                .blob_url("https://storage.blob.core.windows.net/a?sig=secret")
                .is_ok()
        );
    }

    #[test]
    fn packaging_tracks_files_and_writes_a_readable_zip() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"), "report").unwrap();
        let package = Package::create(root.path(), 100_000).unwrap();
        assert_eq!(package.bytes, 6);
        assert_eq!(package.files, 1);
        let mut archive = zip::ZipArchive::new(package.file.reopen().unwrap()).unwrap();
        assert_eq!(archive.by_name("index.html").unwrap().size(), 6);
    }

    #[tokio::test]
    async fn deployment_protocol_retries_upload_and_never_sends_token_to_blob() -> Result<()> {
        let logs = tempfile::tempdir()?;
        super::super::with_deployment_journal(logs.path(), azure_retry_fixture()).await?;
        super::diagnostics::assert_retry_journal(
            logs.path(),
            DeploymentProvider::AzureSwa,
            &[&token(), "sensitive"],
        )
    }

    async fn azure_retry_fixture() -> Result<()> {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let upload_count = Arc::new(AtomicUsize::new(0));
        let uploads = upload_count.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let path = request
                .url()
                .split('?')
                .next()
                .unwrap_or_default()
                .to_string();
            let authorization = request
                .headers()
                .iter()
                .any(|header| header.field.equiv("Authorization"));
            let host = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Host"))
                .unwrap()
                .value
                .to_string();
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body).unwrap();
            captured
                .lock()
                .unwrap()
                .push((path.clone(), authorization, body));
            let response = match path.as_str() {
                "/api/upload/validateapitoken" => json!({"sku":"Standard"}),
                "/api/upload/request" => {
                    json!({"packageUris":{"app": format!("http://{host}/blob?sig=sensitive")}, "pollingInfo":{"version":"one","tenantId":"tenant","defaultHostname":"reports.azurestaticapps.net","stageSiteIdentifier":"site"}})
                }
                "/api/upload/updatestatus" => Value::Null,
                "/api/upload/checkstatus" => {
                    json!({"deploymentStatus":"Succeeded", "siteUrl":"https://reports.azurestaticapps.net/"})
                }
                "/blob" => {
                    let code = if uploads.fetch_add(1, Ordering::Relaxed) == 0 {
                        503
                    } else {
                        201
                    };
                    let _ = request.respond(tiny_http::Response::empty(code));
                    return;
                }
                _ => {
                    let _ = request.respond(tiny_http::Response::empty(404));
                    return;
                }
            };
            let _ = request.respond(tiny_http::Response::from_string(
                json!({"isSuccessStatusCode":true,"statusCode":200,"response":response})
                    .to_string(),
            ));
        })?;
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        let mut client = SwaClient::new(&token())?;
        client.endpoint = Url::parse(&site.origin)?;
        client.backoff = Duration::from_millis(1);
        let target = DeployTarget {
            id: "test".into(),
            name: "Test".into(),
            published_url: None,
            credential_id: None,
            named_token_id: None,
            environment: "preview".into(),
            provider: Default::default(),
            site_id: String::new(),
            cloudflare: None,
        };
        let mut stats = super::super::DeploymentStats::default();
        assert_eq!(
            client
                .deploy_with_stats(root.path(), &target, &mut stats)
                .await?,
            "https://reports.azurestaticapps.net/"
        );
        assert_eq!(stats.file_count, 1);
        assert_eq!(stats.original_bytes, 6);
        assert_eq!(stats.total_bytes, 6);
        let records = requests.lock().unwrap();
        assert_eq!(upload_count.load(Ordering::Relaxed), 2);
        assert!(
            records
                .iter()
                .filter(|(path, _, _)| path == "/blob")
                .all(|(_, auth, _)| !auth)
        );
        assert!(
            records
                .iter()
                .filter(|(path, _, _)| path.starts_with("/api/"))
                .all(|(_, auth, _)| *auth)
        );
        let request = records
            .iter()
            .find(|(path, _, _)| path == "/api/upload/request")
            .unwrap();
        let payload: Value = serde_json::from_slice(&request.2)?;
        assert_eq!(payload["eventInfo"]["environmentName"], "preview");
        assert_eq!(payload["uploadInfo"]["appFileCount"], 1);
        let request = records
            .iter()
            .find(|(path, _, _)| path == "/api/upload/updatestatus")
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&request.2)?["uploadInfo"]["status"],
            "Succeeded"
        );
        Ok(())
    }

    #[tokio::test]
    async fn mocked_deployment_produces_operation_kind_deployment_journal() -> Result<()> {
        use crate::diagnostics::{
            journal::{ArchiveState, JournalReader, tests::exercise_journal_round_trip},
            record::{OperationId, OperationKind},
        };
        let logs = tempfile::tempdir()?;
        super::super::with_deployment_journal(logs.path(), azure_retry_fixture()).await?;
        let directory = std::fs::read_dir(logs.path().join("_diagnostics"))?
            .next()
            .unwrap()?;
        let operation =
            OperationId::try_from(directory.file_name().into_string().unwrap()).unwrap();
        let reader = JournalReader::open(logs.path(), &operation)?;
        assert_eq!(reader.metadata.operation_kind, OperationKind::Deployment);
        assert_eq!(reader.archive_state(), ArchiveState::Complete);
        let mut records = Vec::new();
        reader.visit(|record| records.push(record))?;
        assert!(records.len() > 2);
        records.pop();
        exercise_journal_round_trip(reader.metadata, records);
        Ok(())
    }

    #[tokio::test]
    async fn permanent_http_errors_are_not_retried_and_do_not_expose_sas_urls() -> Result<()> {
        let attempts = Arc::new(AtomicUsize::new(0));
        let calls = attempts.clone();
        let site = crate::test::browser_tests::Site::new(move |request| {
            calls.fetch_add(1, Ordering::Relaxed);
            let _ = request.respond(
                tiny_http::Response::from_string("sensitive server body").with_status_code(403),
            );
        })?;
        let client = SwaClient::new(&token())?;
        let error = client
            .send(crate::diagnostics::record::SafeValue::Upload, || {
                client
                    .client
                    .put(format!("{}/blob?sig=private", site.origin))
            })
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert!(error.contains("403"));
        assert!(!error.contains("private") && !error.contains("sensitive"));
        Ok(())
    }

    #[tokio::test]
    async fn azure_envelope_and_http_errors_preserve_the_provider_reason() -> Result<()> {
        for status in [200, 413] {
            let site = crate::test::browser_tests::Site::new(move |request| {
                let _ = request.respond(tiny_http::Response::from_string(
                    json!({"isSuccessStatusCode":false,"statusCode":400,"response":null,
                        "errorMessage":format!("Content exceeds the 250 MB storage limit; token={} https://storage.blob.core.windows.net/app?sig=private", token())})
                        .to_string(),
                ).with_status_code(status));
            })?;
            let mut client = SwaClient::new(&token())?;
            client.endpoint = Url::parse(&site.origin)?;
            let error = client.api("request", None).await.unwrap_err().to_string();
            assert!(error.contains("Content exceeds the 250 MB storage limit"));
            assert!(error.contains(if status == 200 {
                "HTTP 400"
            } else {
                "HTTP 413"
            }));
            assert!(error.contains("upgrade Free"));
            assert!(!error.contains(&token()) && !error.contains("private"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn multi_block_upload_commits_ordered_base64_ids_after_all_blocks() -> Result<()> {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let site = crate::test::browser_tests::Site::new(move |mut request| {
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body).unwrap();
            captured
                .lock()
                .unwrap()
                .push((request.url().to_string(), body));
            let _ = request.respond(tiny_http::Response::empty(201));
        })?;
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("index.html"), "report")?;
        let package = Package::create(root.path(), 100_000)?;
        let mut client = SwaClient::new(&token())?;
        client.block_bytes = 64;
        client
            .upload(
                &Url::parse(&format!("{}/blob?sig=private", site.origin))?,
                &package,
            )
            .await?;
        let records = requests.lock().unwrap();
        let (url, body) = records.last().unwrap();
        assert!(url.contains("comp=blocklist"));
        let blocks = &records[..records.len() - 1];
        assert!(blocks.len() > 1);
        let mut ids = blocks
            .iter()
            .map(|(url, _)| {
                Url::parse(&format!("{}{}", site.origin, url))
                    .unwrap()
                    .query_pairs()
                    .find(|(key, _)| key == "blockid")
                    .unwrap()
                    .1
                    .into_owned()
            })
            .collect::<Vec<_>>();
        ids.sort_by_key(|id| STANDARD.decode(id).unwrap());
        let expected = ids
            .iter()
            .map(|id| format!("<Latest>{id}</Latest>"))
            .collect::<String>();
        assert!(String::from_utf8_lossy(body).contains(&expected));
        assert_eq!(
            blocks
                .iter()
                .map(|(_, body)| body.len() as u64)
                .sum::<u64>(),
            package.file.as_file().metadata()?.len()
        );
        Ok(())
    }

    #[tokio::test]
    async fn polling_is_bounded_and_terminal_failure_is_redacted() -> Result<()> {
        for status in ["Failed", "InProgress"] {
            let site = crate::test::browser_tests::Site::new(move |request| {
                let host = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Host"))
                    .unwrap()
                    .value
                    .to_string();
                if request.url().starts_with("/blob") {
                    let _ = request.respond(tiny_http::Response::empty(201));
                    return;
                }
                let response = if request.url().starts_with("/api/upload/validateapitoken") {
                    json!({"sku":"Standard"})
                } else if request.url().starts_with("/api/upload/request") {
                    json!({"packageUris":{"app":format!("http://{host}/blob")}, "pollingInfo":{}})
                } else if request.url().starts_with("/api/upload/checkstatus") {
                    json!({"deploymentStatus":status,
                        "errorDetails":format!("Report storage limit exceeded; token={} https://storage.blob.core.windows.net/app?sig=sensitive", token()),
                        "unhealthyRegions":["eastus2"]})
                } else {
                    Value::Null
                };
                let _ = request.respond(tiny_http::Response::from_string(
                    json!({"isSuccessStatusCode":true,"statusCode":200,"response":response})
                        .to_string(),
                ));
            })?;
            let root = tempfile::tempdir()?;
            std::fs::write(root.path().join("index.html"), "report")?;
            let mut client = SwaClient::new(&token())?;
            client.endpoint = Url::parse(&site.origin)?;
            client.poll_interval = Duration::from_millis(1);
            client.deadline = if status == "Failed" {
                Duration::from_secs(5)
            } else {
                Duration::from_millis(100)
            };
            let target = DeployTarget {
                id: "qa".into(),
                name: "QA".into(),
                published_url: None,
                credential_id: None,
                named_token_id: None,
                environment: String::new(),
                provider: Default::default(),
                site_id: String::new(),
                cloudflare: None,
            };
            let error = client
                .deploy(root.path(), &target)
                .await
                .unwrap_err()
                .to_string();
            assert!(!error.contains("sensitive"));
            assert!(!error.contains(&token()));
            if status == "Failed" {
                assert!(error.contains("Report storage limit exceeded"));
                assert!(error.contains("eastus2"));
                assert!(error.contains(&client.correlation));
            }
            assert!(error.contains(if status == "Failed" {
                "failed"
            } else {
                "deadline"
            }));
        }
        Ok(())
    }
}
