use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use reqwest::Client;
use tokio::{io::AsyncWriteExt, time::MissedTickBehavior};

use super::{BROWSER_LOG_INTERVAL, ManagedBrowser, browser_status};
use crate::helpers::{FileLock, HttpExtensions};

const MIB: f64 = 1024.0 * 1024.0;
const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const NETWORK_HINT: &str = "Check your connection, VPN/firewall, proxy authentication and TLS certificates. Chromium downloads require access to storage.googleapis.com; configure --proxy in CLI or Proxy in Desktop run settings if needed, then retry.";
const DISK_HINT: &str =
    "Check free disk space, cache folder permissions and antivirus/quarantine logs, then retry.";

#[derive(Clone, Copy)]
struct InstallLimits {
    response: Duration,
    idle: Duration,
    download: Duration,
    extraction: Duration,
    log_interval: Duration,
}

impl Default for InstallLimits {
    fn default() -> Self {
        Self {
            response: Duration::from_secs(30),
            idle: Duration::from_secs(60),
            download: Duration::from_secs(15 * 60),
            extraction: Duration::from_secs(5 * 60),
            log_interval: BROWSER_LOG_INTERVAL,
        }
    }
}

pub(super) async fn install_browser(
    browser: &ManagedBrowser,
    cache_lock: Arc<FileLock>,
) -> Result<()> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(concat!("Regressify/", env!("CARGO_PKG_VERSION")))
        .set_optional_proxy()
        .build()
        .context("Cannot initialize the Chromium download client")?;
    install_with_client(browser, &client, InstallLimits::default(), cache_lock).await
}

async fn install_with_client(
    browser: &ManagedBrowser,
    client: &Client,
    limits: InstallLimits,
    cache_lock: Arc<FileLock>,
) -> Result<()> {
    let cache_dir = browser
        .directory
        .parent()
        .context("Invalid Chromium cache path")?;
    let staging = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(cache_dir)
        .with_context(|| format!("Cannot create Chromium staging directory. {DISK_HINT}"))?;
    let archive_path = staging.path().join("chromium.zip");
    use crate::diagnostics::{record::*, runtime as diagnostics};
    diagnostics::event(
        Stage::QueueSetup,
        DiagnosticEvent::Started,
        Severity::Info,
        [
            (Field::State, FieldValue::Kind(SafeValue::Download)),
            (
                Field::Destination,
                FieldValue::Identity(SafeIdentity::source(&browser.url)),
            ),
            (
                Field::TimeoutMs,
                FieldValue::Number(limits.download.as_millis() as u64),
            ),
        ],
    );
    let received = download_archive(client, &browser.url, &archive_path, limits).await?;
    browser_status(format!(
        "Extracting Chromium ({:.1} MiB archive)",
        received as f64 / MIB
    ));
    let relative_executable = browser
        .executable
        .strip_prefix(&browser.directory)?
        .to_path_buf();
    let extraction_started = tokio::time::Instant::now();
    let extraction_lock = cache_lock.clone();
    let diagnostic_context = diagnostics::current();
    let mut extraction = tokio::task::spawn_blocking(move || {
        diagnostics::in_scope(diagnostic_context, || {
            let _cache_lock = extraction_lock;
            let unpacked = staging.path().join("unpacked");
            extract_archive(&archive_path, &unpacked, limits.extraction)?;
            ensure!(
                unpacked.join(relative_executable).is_file(),
                "Chromium archive did not contain the expected executable. Check antivirus/quarantine logs, then retry."
            );
            Ok::<_, anyhow::Error>(staging)
        })
    });
    let mut heartbeat = tokio::time::interval_at(
        extraction_started + limits.log_interval,
        limits.log_interval,
    );
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let staging = loop {
        tokio::select! {
            result = &mut extraction => break result.context("Chromium extraction task failed")??,
            _ = heartbeat.tick() => browser_status(format!(
                "Extracting Chromium ({:.0}s elapsed; unpacking files)",
                extraction_started.elapsed().as_secs_f64()
            )),
        }
    };
    browser_status("Verifying and saving Chromium installation");
    if tokio::fs::try_exists(&browser.directory).await? {
        tokio::fs::remove_dir_all(&browser.directory)
            .await
            .with_context(|| {
                format!(
                    "Cannot replace incomplete Chromium cache: {}. {DISK_HINT}",
                    browser.directory.display()
                )
            })?;
    }
    tokio::fs::rename(staging.path().join("unpacked"), &browser.directory)
        .await
        .with_context(|| {
            format!(
                "Cannot save Chromium installation: {}. {DISK_HINT}",
                browser.directory.display()
            )
        })?;
    Ok(())
}

fn download_status(received: u64, total: Option<u64>, elapsed: Duration, idle: Duration) -> String {
    let size = match total.filter(|total| *total > 0) {
        Some(total) => format!(
            "{:.1}/{:.1} MiB ({:.0}%)",
            received as f64 / MIB,
            total as f64 / MIB,
            received as f64 * 100.0 / total as f64
        ),
        None => format!("{:.1} MiB (total size unknown)", received as f64 / MIB),
    };
    let mut message = format!(
        "Downloading Chromium: {size}, {:.2} MiB/s, {:.0}s elapsed",
        received as f64 / MIB / elapsed.as_secs_f64().max(0.001),
        elapsed.as_secs_f64()
    );
    if idle >= BROWSER_LOG_INTERVAL {
        message.push_str(&format!("; no data for {:.0}s", idle.as_secs_f64()));
    }
    message
}

async fn download_archive(
    client: &Client,
    url: &str,
    path: &Path,
    limits: InstallLimits,
) -> Result<u64> {
    let started = tokio::time::Instant::now();
    let deadline = started + limits.download;
    let response_deadline = started + limits.response;
    let mut heartbeat =
        tokio::time::interval_at(started + limits.log_interval, limits.log_interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    browser_status("Connecting to Chromium download server");
    let request = client.get(url).send();
    tokio::pin!(request);
    let mut response = loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => bail!("Chromium download exceeded {:.0}s. {NETWORK_HINT}", limits.download.as_secs_f64()),
            _ = tokio::time::sleep_until(response_deadline) => bail!("Chromium server did not respond within {:.0}s. {NETWORK_HINT}", limits.response.as_secs_f64()),
            result = &mut request => break result.map_err(|error| anyhow!("Cannot connect to the Chromium download server: {}. {NETWORK_HINT}", error.without_url()))?,
            _ = heartbeat.tick() => browser_status(format!("Connecting to Chromium download server ({:.0}s elapsed; waiting for response)", started.elapsed().as_secs_f64())),
        }
    };
    let status = response.status();
    ensure!(
        status == reqwest::StatusCode::OK,
        "Chromium download server returned HTTP {status}. {NETWORK_HINT}"
    );
    let total = response.content_length();
    ensure!(
        total.is_none_or(|total| total <= MAX_ARCHIVE_BYTES),
        "Chromium archive exceeds the 1 GiB download limit"
    );
    let file = tokio::fs::File::create(path)
        .await
        .with_context(|| format!("Cannot create Chromium download archive. {DISK_HINT}"))?;
    let mut file = tokio::io::BufWriter::new(file);
    let mut received = 0_u64;
    let mut last_data = tokio::time::Instant::now();
    browser_status(download_status(
        received,
        total,
        started.elapsed(),
        Duration::ZERO,
    ));
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => bail!("Chromium download exceeded {:.0}s after {received} bytes. {NETWORK_HINT}", limits.download.as_secs_f64()),
            _ = tokio::time::sleep_until(last_data + limits.idle) => bail!("Chromium download stalled: no data for {:.0}s after {received} bytes. {NETWORK_HINT}", limits.idle.as_secs_f64()),
            _ = heartbeat.tick() => browser_status(download_status(received, total, started.elapsed(), last_data.elapsed())),
            chunk = response.chunk() => {
                let Some(chunk) = chunk.map_err(|error| anyhow!("Chromium download failed after {received} bytes: {}. {NETWORK_HINT}", error.without_url()))? else { break };
                if chunk.is_empty() {
                    continue;
                }
                received += chunk.len() as u64;
                ensure!(received <= MAX_ARCHIVE_BYTES, "Chromium archive exceeds the 1 GiB download limit");
                tokio::time::timeout_at(deadline, file.write_all(&chunk))
                    .await
                    .context("Timed out writing the Chromium archive to disk")?
                    .with_context(|| format!("Cannot write Chromium download archive. {DISK_HINT}"))?;
                last_data = tokio::time::Instant::now();
            }
        }
    }
    ensure!(
        received > 0,
        "Chromium download returned an empty archive. {NETWORK_HINT}"
    );
    ensure!(
        total.is_none_or(|total| total == received),
        "Chromium download was incomplete ({received} bytes; expected {total:?}). {NETWORK_HINT}"
    );
    tokio::time::timeout_at(deadline, file.flush())
        .await
        .context("Timed out flushing the Chromium archive to disk")?
        .with_context(|| format!("Cannot flush Chromium download archive. {DISK_HINT}"))?;
    browser_status(format!(
        "Chromium download complete: {:.1} MiB in {:.0}s",
        received as f64 / MIB,
        started.elapsed().as_secs_f64()
    ));
    Ok(received)
}

struct ExtractionReader {
    file: File,
    deadline: Instant,
}

impl ExtractionReader {
    fn check_deadline(&self) -> io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Chromium extraction time limit exceeded",
            ));
        }
        Ok(())
    }
}

impl Read for ExtractionReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check_deadline()?;
        self.file.read(buffer)
    }
}

impl Seek for ExtractionReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check_deadline()?;
        self.file.seek(position)
    }
}

fn extract_archive(archive_path: &Path, directory: &Path, limit: Duration) -> Result<()> {
    let reader = ExtractionReader {
        file: File::open(archive_path).context("Cannot open Chromium archive")?,
        deadline: Instant::now() + limit,
    };
    let mut archive = zip::ZipArchive::new(reader)
        .context("Cannot read Chromium ZIP archive; it may be corrupt or a proxy returned a non-ZIP response. Retry the install")?;
    crate::diagnostics::runtime::event(
        crate::diagnostics::record::Stage::QueueSetup,
        crate::diagnostics::record::DiagnosticEvent::Decision,
        crate::diagnostics::record::Severity::Debug,
        [(
            crate::diagnostics::record::Field::FileCount,
            crate::diagnostics::record::FieldValue::Number(archive.len() as u64),
        )],
    );
    archive.extract(directory).with_context(|| {
        format!(
            "Cannot extract Chromium archive (time limit {:.0}s). {DISK_HINT}",
            limit.as_secs_f64()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tokio::io::AsyncReadExt;

    struct MockServer {
        url: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn server(response: Vec<u8>, stall: bool) -> MockServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/chromium.zip", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let mut request_length = 0;
            while !request[..request_length].ends_with(b"\r\n\r\n") {
                let received = socket.read(&mut request[request_length..]).await.unwrap();
                assert!(received > 0, "incomplete or oversized mock HTTP request");
                request_length += received;
            }
            socket.write_all(&response).await.unwrap();
            if stall {
                std::future::pending::<()>().await;
            }
        });
        MockServer { url, task }
    }

    fn client() -> Client {
        Client::builder().no_proxy().build().unwrap()
    }

    fn limits() -> InstallLimits {
        InstallLimits {
            response: Duration::from_millis(100),
            idle: Duration::from_millis(100),
            download: Duration::from_secs(2),
            log_interval: Duration::from_millis(20),
            ..Default::default()
        }
    }

    fn response(body: &[u8]) -> Vec<u8> {
        [
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
            body,
        ]
        .concat()
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn download_progress_shows_bytes_speed_elapsed_and_idle_without_inventing_a_total() {
        let known = download_status(
            1024 * 1024,
            Some(2 * 1024 * 1024),
            Duration::from_secs(10),
            Duration::from_secs(6),
        );
        assert!(known.contains("1.0/2.0 MiB (50%)"));
        assert!(known.contains("0.10 MiB/s, 10s elapsed"));
        assert!(known.contains("no data for 6s"));
        let unknown = download_status(1024 * 1024, None, Duration::from_secs(10), Duration::ZERO);
        assert!(unknown.contains("total size unknown"));
        assert!(!unknown.contains('%'));
        assert!(!unknown.contains("no data"));
    }

    #[tokio::test]
    async fn download_writes_all_bytes_with_or_without_content_length() {
        for reply in [
            response(b"archive data"),
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\narchive data".to_vec(),
        ] {
            let server = server(reply, false).await;
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("download.zip");
            assert_eq!(
                download_archive(&client(), &server.url, &path, limits())
                    .await
                    .unwrap(),
                12
            );
            assert_eq!(std::fs::read(path).unwrap(), b"archive data");
        }
    }

    #[tokio::test]
    async fn download_fails_when_server_never_sends_headers() {
        let server = server(Vec::new(), true).await;
        let directory = tempfile::tempdir().unwrap();
        let error = download_archive(
            &client(),
            &server.url,
            &directory.path().join("archive.zip"),
            limits(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("did not respond"), "{error}");
        assert!(error.contains("--proxy"), "{error}");
    }

    #[tokio::test]
    async fn download_fails_when_body_stalls_after_partial_data() {
        let server = server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc".to_vec(),
            true,
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let error = download_archive(
            &client(),
            &server.url,
            &directory.path().join("archive.zip"),
            limits(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("stalled"), "{error}");
        assert!(error.contains("after 3 bytes"), "{error}");
    }

    #[tokio::test]
    async fn download_has_a_total_deadline_independent_of_idle_timeout() {
        let server = server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc".to_vec(),
            true,
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let limits = InstallLimits {
            download: Duration::from_millis(30),
            idle: Duration::from_secs(1),
            ..limits()
        };
        let error = download_archive(
            &client(),
            &server.url,
            &directory.path().join("archive.zip"),
            limits,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("download exceeded"), "{error}");
    }

    #[tokio::test]
    async fn download_rejects_http_errors_empty_and_truncated_responses() {
        for (reply, expected) in [
            (
                b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n".to_vec(),
                "HTTP 407",
            ),
            (response(b""), "empty archive"),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc".to_vec(),
                "after 3 bytes",
            ),
        ] {
            let server = server(reply, false).await;
            let directory = tempfile::tempdir().unwrap();
            let error = download_archive(
                &client(),
                &server.url,
                &directory.path().join("archive.zip"),
                limits(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn extraction_checks_deadline_and_rejects_corrupt_archives() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.zip");
        std::fs::write(&path, archive(&[("chrome", b"fixture")])).unwrap();
        let error =
            extract_archive(&path, &directory.path().join("unpacked"), Duration::ZERO).unwrap_err();
        assert!(format!("{error:#}").contains("time limit"));
        std::fs::write(&path, b"<html>proxy sign in required</html>").unwrap();
        let error = extract_archive(
            &path,
            &directory.path().join("unpacked"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("non-ZIP response"));
    }

    #[tokio::test]
    async fn installation_publishes_only_complete_archives_and_cleans_failed_staging() {
        for valid in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut browser = super::super::managed_browser(directory.path()).unwrap();
            std::fs::create_dir_all(&browser.directory).unwrap();
            std::fs::write(
                browser.directory.join("old-incomplete"),
                b"preserve on failure",
            )
            .unwrap();
            let executable = browser
                .executable
                .strip_prefix(&browser.directory)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let body = archive(&[(
                if valid {
                    &executable
                } else {
                    "unexpected-file"
                },
                b"fixture",
            )]);
            let server = server(response(&body), false).await;
            browser.url = server.url.clone();
            let lock = Arc::new(
                super::super::lock_browser_cache(directory.path())
                    .await
                    .unwrap(),
            );
            let result = install_with_client(&browser, &client(), limits(), lock).await;
            if valid {
                result.unwrap();
                assert_eq!(std::fs::read(&browser.executable).unwrap(), b"fixture");
                assert!(!browser.directory.join("old-incomplete").exists());
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("expected executable")
                );
                assert!(!browser.executable.exists());
                assert!(browser.directory.join("old-incomplete").is_file());
            }
            assert!(std::fs::read_dir(directory.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".install-")
            }));
            super::super::lock_browser_cache_with_timeout(directory.path(), Duration::ZERO)
                .await
                .unwrap();
        }
    }
}
