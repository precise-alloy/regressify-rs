use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};
use serde::{Deserialize, Serialize};

const MAX_DEFINITION_BYTES: usize = 1024 * 1024;
mod preview;
pub use preview::LandingPreview;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LandingSite {
    pub heading: String,
    #[serde(default)]
    pub subheading: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LandingApp {
    pub version: u8,
    pub id: String,
    pub heading: String,
    #[serde(default)]
    pub subheading: String,
    #[serde(default)]
    pub sites: Vec<LandingSite>,
}

impl Default for LandingApp {
    fn default() -> Self {
        Self {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            heading: "Visual regression sites".into(),
            subheading: String::new(),
            sites: Vec::new(),
        }
    }
}

impl LandingApp {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "Unsupported landing definition version");
        ensure!(
            uuid::Uuid::parse_str(&self.id).is_ok(),
            "Invalid landing app ID"
        );
        validate_text(&self.heading, &self.subheading)?;
        ensure!(
            self.sites.len() <= 100,
            "A landing page supports up to 100 sites"
        );
        for site in &self.sites {
            validate_text(&site.heading, &site.subheading)?;
            validate_url(&site.url)?;
        }
        Ok(())
    }

    pub fn deployment_root(&self, preferences: &Path) -> Result<PathBuf> {
        self.validate()?;
        let id = uuid::Uuid::parse_str(&self.id)?;
        let root = preferences
            .parent()
            .context("Desktop settings directory unavailable")?
            .join("landing-deployments")
            .join(id.to_string());
        fs::create_dir_all(&root)?;
        ensure!(
            !fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "Landing deployment settings cannot be a symlink"
        );
        Ok(root.canonicalize()?)
    }

    pub fn render(&self) -> Result<String> {
        use crate::snapshot::{FRONTEND_FILES, FRONTEND_HTML, escape_html, get_asset_version};

        self.validate()?;
        let icon_path = "_snapshot/assets/icon.svg";
        let icon = FRONTEND_FILES
            .iter()
            .find(|(path, _)| *path == icon_path)
            .context("Landing logo is unavailable")?;
        let icon_url = format!("/{icon_path}?v={}", get_asset_version(icon.1, 20));
        let mut sites = String::new();
        for site in &self.sites {
            let url = escape_html(&site.url);
            let heading = escape_html(&site.heading);
            sites.push_str(&format!(r#"<article class="landing-site"><div class="landing-site-copy"><h2><a class="link" href="{url}" target="_blank" rel="noopener noreferrer">{heading}</a></h2><div class="report-subheading landing-subheading">{subheading}</div><a class="landing-url mono link" href="{url}" target="_blank" rel="noopener noreferrer">{url}</a></div><div class="landing-site-status" data-regressify-status="{url}" data-site-heading="{heading}" role="region" aria-label="Status: {heading}"><p class="muted">Status not loaded</p></div></article>"#, subheading = markdown(&site.subheading)));
        }
        if self.sites.is_empty() {
            sites.push_str("<p class=\"landing-empty muted\">No sites</p>");
        }
        let content = format!(
            r##"<div id="root" data-regressify-landing><a href="#main" class="skip-link">Skip to sites</a><header class="app-header"><a href="/" class="brand" aria-label="Regressify sites"><img src="{icon_url}" width="24" height="24" alt=""><span>Regressify</span><span class="brand-divider"></span><span class="brand-context">Sites</span></a><div id="landing-controls" class="header-actions"></div></header><div class="landing-layout"><main id="main" tabindex="-1" class="page overview-page landing-page"><div class="page-heading"><div><h1>{heading}</h1><div class="report-subheading">{subheading}</div></div></div><section class="landing-sites" aria-label="Sites">{sites}</section></main><footer class="app-footer"><span>Regressify</span><span>Visual regression sites</span></footer></div></div>"##,
            heading = escape_html(&self.heading),
            subheading = markdown(&self.subheading)
        );
        ensure!(
            FRONTEND_HTML.matches("<div id=\"root\"></div>").count() == 1,
            "Unsupported landing frontend template"
        );
        Ok(FRONTEND_HTML
            .replace(
                "</head>",
                &format!("<link rel=\"icon\" type=\"image/svg+xml\" href=\"{icon_url}\"></head>"),
            )
            .replace(
                "__SNAPSHOT_TITLE__",
                &escape_html(&format!("{} | Regressify", self.heading)),
            )
            .replace(
                "<meta name=\"regressify-manifest\" content=\"__SNAPSHOT_MANIFEST__\">",
                "",
            )
            .replace("<noscript>This report requires JavaScript.</noscript>", "")
            .replace("<div id=\"root\"></div>", &content))
    }

    pub fn stage(&self, root: &Path) -> Result<()> {
        let html = self.render()?;
        ensure!(
            root.is_dir()
                && !fs::symlink_metadata(root)?.file_type().is_symlink()
                && fs::read_dir(root)?.next().is_none(),
            "Landing staging must be a new empty directory"
        );
        for (name, bytes) in crate::snapshot::FRONTEND_FILES {
            if *name == "gitignore" {
                continue;
            }
            let path = root.join(name);
            fs::create_dir_all(
                path.parent()
                    .context("Landing asset directory unavailable")?,
            )?;
            crate::helpers::write_file_atomically(&path, bytes)?;
        }
        crate::helpers::write_file_atomically(&root.join("index.html"), html.as_bytes())
    }
}

fn validate_text(heading: &str, subheading: &str) -> Result<()> {
    ensure!(
        !heading.trim().is_empty()
            && heading.len() <= 300
            && !heading.chars().any(char::is_control),
        "Heading must contain 1-300 characters"
    );
    ensure!(
        subheading.len() <= 20 * 1024 && !subheading.contains('\0'),
        "Markdown subheading exceeds its size limit or contains a null character"
    );
    Ok(())
}

fn validate_url(value: &str) -> Result<reqwest::Url> {
    ensure!(
        value.len() <= 4096 && value.trim() == value && !value.chars().any(char::is_control),
        "Enter an absolute HTTP(S) site URL without credentials"
    );
    let url = reqwest::Url::parse(value).context("Enter an absolute HTTP(S) site URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none(),
        "Enter an absolute HTTP(S) site URL without credentials"
    );
    Ok(url)
}

fn markdown(source: &str) -> String {
    let mut links = Vec::new();
    let mut image_depth = 0usize;
    let events = Parser::new_ext(
        source,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES,
    )
    .filter_map(|event| {
        match &event {
            Event::Start(Tag::Image { .. }) => {
                image_depth += 1;
                return None;
            }
            Event::End(TagEnd::Image) => {
                image_depth = image_depth.saturating_sub(1);
                return None;
            }
            _ if image_depth > 0 => return None,
            Event::Html(_) | Event::InlineHtml(_) => return None,
            Event::Start(Tag::Link { dest_url, .. }) => {
                let safe = validate_url(dest_url).is_ok();
                links.push(safe);
                if !safe {
                    return None;
                }
            }
            Event::End(TagEnd::Link) if !links.pop().unwrap_or(false) => return None,
            _ => {}
        }
        Some(event)
    });
    let mut output = String::new();
    html::push_html(&mut output, events);
    output
}

#[derive(Clone, Debug)]
pub struct LandingFile {
    pub path: PathBuf,
    pub app: LandingApp,
    saved_source: Vec<u8>,
}

impl LandingFile {
    pub fn create_managed(preferences: &Path, app: LandingApp) -> Result<Self> {
        app.validate()?;
        let directory = preferences
            .parent()
            .context("Desktop settings directory unavailable")?
            .join("landing-apps");
        fs::create_dir_all(&directory)?;
        ensure!(
            !fs::symlink_metadata(&directory)?.file_type().is_symlink(),
            "Landing app storage cannot be a symlink"
        );
        let id = uuid::Uuid::parse_str(&app.id)?;
        Self::create(&directory.join(format!("{id}.json")), app)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let saved_source = read_definition(path)?;
        let app: LandingApp =
            serde_json::from_slice(&saved_source).context("Invalid landing definition JSON")?;
        app.validate()?;
        Ok(Self {
            path: path.canonicalize()?,
            app,
            saved_source,
        })
    }

    pub fn create(path: &Path, app: LandingApp) -> Result<Self> {
        let bytes = definition_bytes(&app)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("Choose a JSON file name")?;
        ensure!(
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
                && !name.ends_with(['.', ' '])
                && !name.chars().any(char::is_control)
                && !name.contains([':', '<', '>', '"', '|', '?', '*'])
                && !crate::helpers::is_windows_reserved_path_component(name),
            "Choose a portable JSON file name"
        );
        let parent = path
            .parent()
            .context("Choose a landing file directory")?
            .canonicalize()?;
        let path = parent.join(name);
        let mut temporary = tempfile::NamedTempFile::new_in(&parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist_noclobber(&path).map_err(|error| {
            anyhow::anyhow!(
                "Cannot create landing file without overwriting an existing file: {}",
                error.error
            )
        })?;
        Ok(Self {
            path: path.canonicalize()?,
            app,
            saved_source: bytes,
        })
    }

    pub fn save(&mut self) -> Result<()> {
        let bytes = definition_bytes(&self.app)?;
        let _lock = self.lock()?;
        self.ensure_unchanged()?;
        crate::helpers::write_file_atomically(&self.path, &bytes)?;
        self.saved_source = bytes;
        Ok(())
    }

    pub fn remove(&self) -> Result<()> {
        let _lock = self.lock()?;
        self.ensure_unchanged()?;
        fs::remove_file(&self.path).context("Cannot remove the local landing definition")
    }

    pub fn remove_with_deployments(
        &self,
        preferences: &Path,
        catalog_path: &Path,
    ) -> Result<LandingCatalog> {
        let _lock = self.lock()?;
        self.ensure_unchanged()?;
        let root = self.app.deployment_root(preferences)?;
        let path = super::deployment::DeployTargets::path_in(&root, preferences)?;
        let mut targets = super::deployment::DeployTargets::load(&path)?;
        let parent = self
            .path
            .parent()
            .context("Landing file directory unavailable")?;
        let backup_dir = tempfile::Builder::new()
            .prefix(".landing-remove-")
            .tempdir_in(parent)?;
        let backup = backup_dir.path().join("definition.json");
        // Stage the definition removal under the file lock before deleting target credentials,
        // and restore the definition if credential or catalog cleanup fails.
        fs::rename(&self.path, &backup).context("Cannot remove the local landing definition")?;
        let cleanup = (|| -> Result<LandingCatalog> {
            for id in targets
                .targets
                .iter()
                .map(|target| target.id.clone())
                .collect::<Vec<_>>()
            {
                targets.remove_target(&root, &path, &id)?;
            }
            LandingCatalog::update(catalog_path, &self.path, false)
        })();
        match cleanup {
            Ok(catalog) => {
                let _ = fs::remove_file(&backup);
                let _ = fs::remove_file(&path);
                let _ = fs::remove_dir(&root);
                Ok(catalog)
            }
            Err(error) => {
                let _ = fs::rename(&backup, &self.path);
                Err(error)
            }
        }
    }

    pub fn ensure_unchanged(&self) -> Result<()> {
        ensure!(
            read_definition(&self.path)? == self.saved_source,
            "Landing app changed outside this editor. Reopen it before saving; your draft has been preserved"
        );
        Ok(())
    }

    fn lock(&self) -> Result<fs::File> {
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("json.lock"))?;
        lock.lock().context("Cannot lock the landing definition")?;
        Ok(lock)
    }
}

fn definition_bytes(app: &LandingApp) -> Result<Vec<u8>> {
    app.validate()?;
    let bytes = serde_json::to_vec_pretty(app)?;
    ensure!(
        bytes.len() <= MAX_DEFINITION_BYTES,
        "Landing definition exceeds 1 MiB"
    );
    Ok(bytes)
}

fn read_definition(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).context("Landing definition is unavailable")?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Landing definition must be a regular file, not a symlink"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_DEFINITION_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_DEFINITION_BYTES,
        "Landing definition exceeds 1 MiB"
    );
    Ok(bytes)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LandingCatalog {
    pub files: Vec<PathBuf>,
}

impl LandingCatalog {
    pub fn path_in(preferences: &Path) -> Result<PathBuf> {
        Ok(preferences
            .parent()
            .context("Desktop settings directory unavailable")?
            .join("landing-apps.json"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        match fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_DEFINITION_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= MAX_DEFINITION_BYTES,
                    "Landing app catalog exceeds its size limit"
                );
                let catalog: Self =
                    serde_json::from_slice(&bytes).context("Invalid landing app catalog")?;
                ensure!(
                    catalog.files.len() <= 100
                        && catalog.files.iter().all(|path| path.is_absolute()),
                    "Invalid landing app catalog paths"
                );
                Ok(catalog)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn update(path: &Path, file: &Path, remember: bool) -> Result<Self> {
        fs::create_dir_all(
            path.parent()
                .context("Landing catalog directory unavailable")?,
        )?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        lock.lock().context("Cannot lock the landing app catalog")?;
        let mut catalog = Self::load(path)?;
        catalog.files.retain(|entry| entry != file);
        if remember {
            let file = file.canonicalize()?;
            catalog.files.retain(|entry| entry != &file);
            catalog.files.insert(0, file);
        }
        ensure!(
            catalog.files.len() <= 100,
            "Up to 100 landing apps are supported"
        );
        crate::helpers::write_file_atomically(path, &serde_json::to_vec_pretty(&catalog)?)?;
        Ok(catalog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Publishes a synthetic landing page to an explicitly authorized provider target; replaces hosted content"]
    async fn live_publish_landing_page() -> Result<()> {
        use crate::desktop::deployment::{
            DeployTarget, DeploymentClient, DeploymentProvider, cloudflare::CloudflareTarget,
        };

        ensure!(
            std::env::var("REGRESSIFY_ALLOW_LANDING_PUBLISH").as_deref() == Ok("1"),
            "Set REGRESSIFY_ALLOW_LANDING_PUBLISH=1 to authorize replacement of the target site"
        );
        let provider = match std::env::var("REGRESSIFY_LANDING_PROVIDER").as_deref() {
            Ok("azure") => DeploymentProvider::AzureSwa,
            Ok("netlify") => DeploymentProvider::Netlify,
            Ok("cloudflare") => DeploymentProvider::CloudflareWorkers,
            _ => anyhow::bail!("Set REGRESSIFY_LANDING_PROVIDER to azure, netlify, or cloudflare"),
        };
        let token_name = match provider {
            DeploymentProvider::AzureSwa => "AZURE_STATIC_WEB_APP_API_TOKEN",
            DeploymentProvider::Netlify => "NETLIFY_AUTH_TOKEN",
            DeploymentProvider::CloudflareWorkers => "CLOUDFLARE_API_TOKEN",
        };
        let token = zeroize::Zeroizing::new(
            std::env::var(token_name).context("The selected provider token is missing")?,
        );
        let target = DeployTarget {
            id: "landing-verification".into(),
            name: "Landing verification".into(),
            published_url: None,
            provider,
            credential_id: None,
            named_token_id: None,
            environment: String::new(),
            site_id: if provider == DeploymentProvider::Netlify {
                std::env::var("NETLIFY_SITE_ID")?
            } else {
                String::new()
            },
            cloudflare: if provider == DeploymentProvider::CloudflareWorkers {
                Some(CloudflareTarget {
                    account_id: std::env::var("CLOUDFLARE_ACCOUNT_ID")?,
                    worker_name: std::env::var("CLOUDFLARE_WORKER_NAME")?,
                    ..CloudflareTarget::default()
                })
            } else {
                None
            },
        };
        target.validate()?;
        let app = LandingApp {
            heading: "Visual regression sites".into(),
            subheading: "**Landing deployment verification**\n\nIndependent reports linked by URL."
                .into(),
            sites: vec![LandingSite {
                heading: "Regressify report".into(),
                subheading: "**Published report**".into(),
                url: std::env::var("REGRESSIFY_LANDING_SITE_URL")
                    .unwrap_or_else(|_| "https://example.com/".into()),
            }],
            ..LandingApp::default()
        };
        let staging = tempfile::tempdir()?;
        let status_fixture =
            std::env::var("REGRESSIFY_LANDING_STATUS_FIXTURE").as_deref() == Ok("1");
        if status_fixture {
            crate::snapshot::generate_site(
                staging.path(),
                &[],
                &crate::cli::args::SnapshotArgs {
                    test_suite: Vec::new(),
                    title: None,
                    heading: None,
                    subheading: None,
                },
                &crate::config::WorkspaceConfig::default(),
            )?;
        } else {
            app.stage(staging.path())?;
        }
        let client = DeploymentClient::new(&target, &token)?;
        drop(token);
        let published = client
            .deploy(staging.path(), &target)
            .await?
            .context("Expected a public deployment URL")?;
        let base = reqwest::Url::parse(&published)?;
        let public = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        let html = public
            .get(base.clone())
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Cannot read published landing page"))?;
        ensure!(
            html.status().is_success(),
            "Published landing page is unavailable"
        );
        ensure!(
            html.text()
                .await
                .map_err(|_| anyhow::anyhow!("Cannot read published landing HTML"))?
                == fs::read_to_string(staging.path().join("index.html"))?,
            "Published HTML does not match the generated page"
        );
        if status_fixture {
            let response = public
                .get(base.join("_snapshot/status.json")?)
                .header("Origin", "https://landing.example")
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Cannot read published report status"))?;
            ensure!(
                response.status().is_success(),
                "Published report status is unavailable"
            );
            ensure!(
                response
                    .headers()
                    .get("access-control-allow-origin")
                    .and_then(|value| value.to_str().ok())
                    == Some("*"),
                "Published status is missing its public CORS policy"
            );
            let cache = response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            ensure!(
                cache.contains("no-cache") && !cache.contains("immutable"),
                "Report status must revalidate instead of caching immutably"
            );
            ensure!(
                response
                    .bytes()
                    .await
                    .map_err(|_| anyhow::anyhow!("Cannot read published status bytes"))?
                    .as_ref()
                    == fs::read(staging.path().join("_snapshot/status.json"))?,
                "Published status differs from the generated report"
            );
        }
        for (name, bytes) in crate::snapshot::FRONTEND_FILES
            .iter()
            .filter(|(name, _)| name.starts_with("_snapshot/assets/"))
        {
            let mut url = base.join(name)?;
            url.set_query(Some(&format!(
                "v={}",
                crate::snapshot::get_asset_version(bytes, 20)
            )));
            let response = public
                .get(url)
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Cannot read a published landing asset"))?;
            ensure!(
                response.status().is_success(),
                "Published landing asset is unavailable"
            );
            ensure!(
                response
                    .bytes()
                    .await
                    .map_err(|_| anyhow::anyhow!("Cannot read published asset bytes"))?
                    .as_ref()
                    == *bytes,
                "Published landing asset differs from the generated bytes"
            );
        }
        println!(
            "Verified {} landing deployment: {published}",
            provider.label()
        );
        Ok(())
    }

    #[test]
    fn managed_landing_creation_chooses_storage_without_using_heading_as_a_path() {
        let directory = tempfile::tempdir().unwrap();
        let preferences = directory.path().join("settings.json");
        let app = LandingApp {
            heading: "../Customer <reports>".into(),
            ..LandingApp::default()
        };
        let mut file = LandingFile::create_managed(&preferences, app.clone()).unwrap();
        assert_eq!(
            file.path,
            directory
                .path()
                .join("landing-apps")
                .join(format!("{}.json", app.id))
                .canonicalize()
                .unwrap()
        );
        assert!(LandingFile::create_managed(&preferences, app).is_err());
        file.app.heading = "Updated".into();
        file.save().unwrap();
        assert_eq!(
            LandingFile::open(&file.path).unwrap().app.heading,
            "Updated"
        );
    }

    #[test]
    fn landing_definitions_are_portable_and_do_not_require_local_sites() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = LandingApp::default();
        app.sites.push(LandingSite {
            heading: "Another user's report".into(),
            subheading: "**Published elsewhere**".into(),
            url: "https://not-a-local-workspace.example/suites/shop".into(),
        });
        let path = directory.path().join("customer.landing.json");
        let mut file = LandingFile::create(&path, app.clone()).unwrap();
        assert_eq!(LandingFile::open(&path).unwrap().app, app);
        assert!(LandingFile::create(&path, LandingApp::default()).is_err());
        file.app.heading = "Renamed".into();
        file.save().unwrap();
        assert_eq!(LandingFile::open(&path).unwrap().app.heading, "Renamed");
        let mut external = LandingFile::open(&path).unwrap();
        external.app.heading = "External edit".into();
        external.save().unwrap();
        assert!(file.save().is_err());
        assert!(file.remove().is_err());
        let preferences = directory.path().join("settings.json");
        let catalog_path = LandingCatalog::path_in(&preferences).unwrap();
        LandingCatalog::update(&catalog_path, &external.path, true).unwrap();
        let deploy_root = external.app.deployment_root(&preferences).unwrap();
        let deploy_path =
            crate::desktop::deployment::DeployTargets::path_in(&deploy_root, &preferences).unwrap();
        let mut targets = crate::desktop::deployment::DeployTargets::default();
        targets
            .save_target(
                &deploy_root,
                &deploy_path,
                crate::desktop::deployment::DeployTarget {
                    id: "landing-target".into(),
                    name: "Landing target".into(),
                    published_url: None,
                    credential_id: None,
                    named_token_id: None,
                    environment: String::new(),
                    provider: crate::desktop::deployment::DeploymentProvider::Netlify,
                    site_id: "11111111-2222-4333-8444-555555555555".into(),
                    cloudflare: None,
                },
                "netlify-synthetic-token",
            )
            .unwrap();
        assert!(
            file.remove_with_deployments(&preferences, &catalog_path)
                .is_err()
        );
        assert_eq!(
            crate::desktop::deployment::DeployTargets::load(&deploy_path)
                .unwrap()
                .targets[0]
                .token(&deploy_root)
                .unwrap()
                .as_str(),
            "netlify-synthetic-token"
        );
        let removed_catalog = external
            .remove_with_deployments(&preferences, &catalog_path)
            .unwrap();
        assert!(removed_catalog.files.is_empty());
        assert!(!path.exists());
        assert!(!deploy_path.exists());
        assert!(!deploy_root.exists());
    }

    #[test]
    fn landing_definitions_reject_unsafe_urls_and_malformed_or_oversized_files() {
        for url in [
            "javascript:alert(1)",
            "file:///private",
            "//example.com",
            "https://user:secret@example.com",
            "https://example.com\n",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.json");
        fs::write(&path, "{}").unwrap();
        assert!(LandingFile::open(&path).is_err());
        fs::write(&path, vec![b' '; MAX_DEFINITION_BYTES + 1]).unwrap();
        assert!(LandingFile::open(&path).is_err());
        assert!(
            LandingFile::create(&directory.path().join("CON.json"), LandingApp::default()).is_err()
        );
    }

    #[test]
    fn landing_markdown_renders_formatting_without_html_images_or_unsafe_links() {
        let output = markdown(
            "**Bold** and *emphasis*, `code`\n\n- Item\n\n[Safe](https://example.com/a?x=1&y=2) [Bad](javascript:alert%281%29) [Credential](https://user:secret@example.com)\n\n![image](https://example.com/image.png)\n\n<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>",
        );
        for expected in [
            "<strong>Bold</strong>",
            "<em>emphasis</em>",
            "<code>code</code>",
            "<li>Item</li>",
            "href=\"https://example.com/a?x=1&amp;y=2\"",
        ] {
            assert!(output.contains(expected), "{output}");
        }
        for unsafe_text in ["<script", "<img", "javascript:", "user:secret", "onerror="] {
            assert!(!output.contains(unsafe_text), "{output}");
        }
    }

    #[test]
    fn landing_generation_prerenders_markdown_and_stages_only_public_assets() {
        let directory = tempfile::tempdir().unwrap();
        let app = LandingApp {
            heading: "Customer <sites> & reports".into(),
            subheading: "**Customer overview**".into(),
            sites: vec![LandingSite {
                heading: "Remote <report>".into(),
                subheading: "*Site details*".into(),
                url: "https://someone-else.example/suites/shop?tag=a&other=b".into(),
            }],
            ..LandingApp::default()
        };
        let html = app.render().unwrap();
        for expected in [
            "<strong>Customer overview</strong>",
            "<em>Site details</em>",
            "Customer &lt;sites&gt; &amp; reports",
            "Remote &lt;report&gt;",
            "data-regressify-status=\"https://someone-else.example/suites/shop?tag=a&amp;other=b\"",
            "data-regressify-landing",
            "_snapshot/assets/icon.svg?v=",
            // Site heading and URL are text links styled by the shared `.link` rule.
            "<h2><a class=\"link\" href=\"https://someone-else.example/suites/shop?tag=a&amp;other=b\"",
            "<a class=\"landing-url mono link\" href=",
        ] {
            assert!(html.contains(expected), "Missing {expected}");
        }
        assert!(!html.contains("__SNAPSHOT_"));
        assert!(!html.contains("requires JavaScript"));
        app.stage(directory.path()).unwrap();
        let actual = walkdir::WalkDir::new(directory.path())
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(directory.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<std::collections::BTreeSet<_>>();
        let expected = crate::snapshot::FRONTEND_FILES
            .iter()
            .filter(|(name, _)| *name != "gitignore")
            .map(|(name, _)| name.to_string())
            .chain(["index.html".into()])
            .collect();
        assert_eq!(actual, expected);
        assert!(
            !fs::read_to_string(directory.path().join("index.html"))
                .unwrap()
                .contains(&app.id)
        );
        assert!(app.stage(directory.path()).is_err());
    }

    #[test]
    fn landing_catalog_updates_preserve_other_windows_entries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("apps.json");
        let first =
            LandingFile::create(&directory.path().join("first.json"), LandingApp::default())
                .unwrap();
        let second =
            LandingFile::create(&directory.path().join("second.json"), LandingApp::default())
                .unwrap();
        LandingCatalog::update(&path, &first.path, true).unwrap();
        LandingCatalog::update(&path, &second.path, true).unwrap();
        assert_eq!(LandingCatalog::load(&path).unwrap().files.len(), 2);
        LandingCatalog::update(&path, &first.path, false).unwrap();
        assert_eq!(
            LandingCatalog::load(&path).unwrap().files,
            vec![second.path]
        );
        assert!(first.path.exists());
    }

    #[test]
    #[ignore = "Creates an isolated synthetic production-bundle fixture for browser checks"]
    fn generate_landing_browser_fixture() {
        let path = std::env::var_os("REGRESSIFY_LANDING_FIXTURE")
            .map(PathBuf::from)
            .expect("Set REGRESSIFY_LANDING_FIXTURE to a new fixture directory");
        fs::create_dir_all(&path).unwrap();
        let app = LandingApp {
            heading: "Customer visual sites".into(),
            subheading: "**Published reports** from independent teams.\n\n[Customer portal](https://example.com/)".into(),
            sites: vec![
                LandingSite { heading: "Commerce storefront".into(), subheading: "**Production**\n\n- Desktop and mobile\n- Checkout and account".into(), url: "https://commerce.report.test/suites/shop".into() },
                LandingSite { heading: "Support portal".into(), subheading: "*Customer service*".into(), url: "https://support.report.test/".into() },
                LandingSite { heading: "Unavailable report".into(), subheading: "<script>window.landingUnsafe = true</script>\n\n[Unsafe](javascript:alert%281%29)".into(), url: "https://unavailable.report.test/".into() },
                LandingSite { heading: "A very long customer report heading with multiple environments and regional storefronts".into(), subheading: "`release/2026-09`".into(), url: "https://long.report.test/suites/an-extremely-long-site-path-that-must-wrap-on-mobile-without-overflowing".into() },
            ],
            ..LandingApp::default()
        };
        app.stage(&path).unwrap();
        println!("Landing browser fixture: {}", path.display());
    }
}
