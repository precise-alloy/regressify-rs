//! Workspace-scoped persistence. Editor drafts never write directly to disk.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use super::document::{Document, DocumentKind, Format, MAX_DOCUMENT_BYTES, is_text};
use crate::helpers::{is_windows_reserved_path_component, write_file_atomically};

mod artifacts;

/// Format paths for people without changing filesystem identity or resolving relative paths.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some(network) = text
        .strip_prefix("//?/UNC/")
        .or_else(|| text.strip_prefix("//?/unc/"))
    {
        format!("//{network}")
    } else {
        text.strip_prefix("//?/").unwrap_or(&text).to_string()
    }
}

#[derive(Clone, Debug)]
pub struct Workspace {
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: DocumentKind,
    pub editable: bool,
}

impl Entry {
    pub fn label(&self) -> String {
        display_path(&self.path)
    }
    pub fn supports_editing(&self) -> bool {
        self.editable
    }
    pub fn suite_name(&self) -> Option<String> {
        let filename = self.path.file_name()?.to_str()?.to_ascii_lowercase();
        [".tests.yaml", ".tests.yml", ".tests.json"]
            .iter()
            .find_map(|suffix| filename.strip_suffix(suffix).map(str::to_string))
    }
}

#[derive(Debug)]
pub struct OpenDocument {
    pub entry: Entry,
    pub document: Document,
}

#[derive(Clone, Debug)]
pub struct SuiteSummary {
    pub entry: Entry,
    pub name: String,
    pub display_name: Option<String>,
    pub output_id: String,
    pub scenarios: Vec<ScenarioSummary>,
    pub error: Option<String>,
}

impl SuiteSummary {
    pub fn label(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug)]
pub struct ScenarioSummary {
    pub id: String,
    pub label: String,
    pub url: String,
}

#[derive(Clone, Debug, Default)]
pub struct SuiteSelection {
    names: BTreeSet<String>,
    scenarios: BTreeMap<String, BTreeSet<String>>,
}

impl SuiteSelection {
    pub fn rename_suite(&mut self, old: &str, new: Option<&str>) {
        let selected = self.names.remove(old);
        let scenarios = self.scenarios.remove(old);
        if let Some(new) = new {
            if selected {
                self.names.insert(new.into());
            }
            if let Some(scenarios) = scenarios {
                self.scenarios.insert(new.into(), scenarios);
            }
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }
    pub fn len(&self) -> usize {
        self.names.len()
    }
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
    pub fn clear(&mut self) {
        self.names.clear();
        self.scenarios.clear();
    }
    pub fn has_scenario_selection(&self) -> bool {
        !self.scenarios.is_empty()
    }
    pub fn scenario_selected(&self, suite: &str, scenario: &str) -> bool {
        self.contains(suite)
            && self
                .scenarios
                .get(suite)
                .is_none_or(|ids| ids.contains(scenario))
    }
    pub fn scenario_count(&self, suite: &SuiteSummary) -> usize {
        suite
            .scenarios
            .iter()
            .filter(|scenario| self.scenario_selected(&suite.name, &scenario.id))
            .count()
    }
    pub fn all_selected(&self, suite: &SuiteSummary) -> bool {
        self.contains(&suite.name) && self.scenario_count(suite) == suite.scenarios.len()
    }
    pub fn toggle_scenario(
        &mut self,
        suite: &SuiteSummary,
        id: &str,
        selected: bool,
    ) -> Result<()> {
        ensure!(
            suite.error.is_none(),
            "Suite '{}' has configuration errors",
            suite.name
        );
        ensure!(
            suite
                .scenarios
                .iter()
                .filter(|scenario| scenario.id == id)
                .count()
                == 1,
            "Scenario '{id}' must match exactly one scenario in suite '{}'",
            suite.name
        );
        let mut ids = self.scenarios.get(&suite.name).cloned().unwrap_or_else(|| {
            if self.contains(&suite.name) {
                suite
                    .scenarios
                    .iter()
                    .map(|scenario| scenario.id.clone())
                    .collect()
            } else {
                BTreeSet::new()
            }
        });
        if selected {
            ids.insert(id.to_string());
        } else {
            ids.remove(id);
        }
        if ids.is_empty() {
            self.names.remove(&suite.name);
            self.scenarios.remove(&suite.name);
        } else {
            self.names.insert(suite.name.clone());
            self.scenarios.insert(suite.name.clone(), ids);
        }
        Ok(())
    }
    pub fn toggle(&mut self, suite: &SuiteSummary, selected: bool) -> Result<()> {
        ensure!(
            suite.error.is_none(),
            "Suite '{}' has configuration errors",
            suite.name
        );
        if selected {
            self.names.insert(suite.name.clone());
        } else {
            self.names.remove(&suite.name);
        }
        self.scenarios.remove(&suite.name);
        Ok(())
    }
    pub fn select_all(&mut self, suites: &[SuiteSummary]) {
        self.scenarios.clear();
        self.names = suites
            .iter()
            .filter(|suite| suite.error.is_none())
            .map(|suite| suite.name.clone())
            .collect();
    }
    pub fn reconcile(&mut self, suites: &[SuiteSummary]) {
        self.names.retain(|name| {
            suites
                .iter()
                .any(|suite| &suite.name == name && suite.error.is_none())
        });
        self.scenarios.retain(|name, ids| {
            let Some(suite) = suites
                .iter()
                .find(|suite| &suite.name == name && suite.error.is_none())
            else {
                return false;
            };
            ids.retain(|id| suite.scenarios.iter().any(|scenario| &scenario.id == id));
            if ids.is_empty() {
                self.names.remove(name);
                false
            } else {
                true
            }
        });
    }
    pub fn selected<'suite>(
        &self,
        suites: &'suite [SuiteSummary],
    ) -> Result<Vec<&'suite SuiteSummary>> {
        ensure!(!self.is_empty(), "Select at least one test suite");
        let selected: Vec<_> = suites
            .iter()
            .filter(|suite| self.contains(&suite.name) && suite.error.is_none())
            .collect();
        ensure!(
            selected.len() == self.len(),
            "Selected suites changed; refresh the workspace before running"
        );
        for suite in &selected {
            if let Some(ids) = self.scenarios.get(&suite.name) {
                ensure!(
                    !ids.is_empty()
                        && ids.iter().all(|id| suite
                            .scenarios
                            .iter()
                            .filter(|scenario| &scenario.id == id)
                            .count()
                            == 1),
                    "Selected scenarios changed in suite '{}'; review the selection before running",
                    suite.name
                );
            }
        }
        Ok(selected)
    }
    pub fn filters(&self, suites: &[SuiteSummary], output_ids: bool) -> Result<Vec<String>> {
        Ok(self
            .selected(suites)?
            .into_iter()
            .map(|suite| {
                glob::Pattern::escape(if output_ids {
                    &suite.output_id
                } else {
                    &suite.name
                })
            })
            .collect())
    }
    pub fn scenario_filters(
        &self,
        suites: &[SuiteSummary],
        output_ids: bool,
    ) -> Result<Vec<crate::cli::args::ScenarioFilter>> {
        let selected = self.selected(suites)?;
        if !self.has_scenario_selection() {
            return Ok(Vec::new());
        }
        Ok(selected
            .into_iter()
            .flat_map(|suite| {
                suite
                    .scenarios
                    .iter()
                    .filter(|scenario| self.scenario_selected(&suite.name, &scenario.id))
                    .map(move |scenario| crate::cli::args::ScenarioFilter {
                        suite: if output_ids {
                            suite.output_id.clone()
                        } else {
                            suite.name.clone()
                        },
                        scenario: scenario.id.clone(),
                    })
            })
            .collect())
    }
}

impl ScenarioSummary {
    pub fn from_scenarios(scenarios: &[crate::types::TestScenario]) -> Vec<Self> {
        let names = crate::helpers::scenario_names(
            &scenarios
                .iter()
                .map(|scenario| crate::helpers::ScenarioName {
                    id: scenario.id.as_deref().unwrap_or_default(),
                    label: scenario.label.as_deref(),
                    url: Some(&scenario.url),
                })
                .collect::<Vec<_>>(),
        );
        scenarios
            .iter()
            .zip(names)
            .map(|(scenario, label)| Self {
                label,
                ..Self::from_scenario(scenario)
            })
            .collect()
    }

    pub fn from_scenario(scenario: &crate::types::TestScenario) -> Self {
        let id = scenario.id.clone().unwrap_or_else(|| {
            scenario
                .label
                .as_deref()
                .map(crate::helpers::slugify)
                .unwrap_or_else(|| crate::snapshot::get_asset_version(scenario.url.as_bytes(), 20))
        });
        Self {
            label: scenario.label.clone().unwrap_or_else(|| id.clone()),
            id,
            url: scenario.url.clone(),
        }
    }
}

impl Workspace {
    pub fn open(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("Cannot open workspace '{}'", display_path(root)))?;
        ensure!(root.is_dir(), "Workspace must be a directory");
        Ok(Self { root })
    }

    /// Validate every existing component, not only the final filename.
    pub fn resolve(&self, relative: &Path) -> Result<PathBuf> {
        ensure!(
            !relative.as_os_str().is_empty(),
            "A workspace-relative filename is required"
        );
        let mut destination = self.root.clone();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(anyhow!(
                    "Path must stay inside the workspace: '{}'",
                    display_path(relative)
                ));
            };
            validate_component(
                name.to_str()
                    .ok_or_else(|| anyhow!("Filename must be UTF-8"))?,
            )?;
            destination.push(name);
            match fs::symlink_metadata(&destination) {
                Ok(_) => {
                    destination = destination
                        .canonicalize()
                        .context("Cannot resolve workspace path")?;
                    ensure!(
                        destination.starts_with(&self.root),
                        "Path escapes the workspace: '{}'",
                        display_path(relative)
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(destination)
    }

    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for name in ["regressify.yaml", "regressify.yml"] {
            let path = PathBuf::from(name);
            let resolved = self.resolve(&path)?;
            if resolved.is_file() {
                entries.push(Entry {
                    editable: read_document(&resolved).is_ok(),
                    path,
                    kind: DocumentKind::Workspace,
                });
            }
        }
        for folder in ["visual_tests", "common"] {
            let directory = self.resolve(Path::new(folder))?;
            if !directory.exists() {
                continue;
            }
            for item in WalkDir::new(directory).follow_links(false).max_depth(24) {
                let item = item.context("Cannot list workspace files")?;
                if !item.file_type().is_file() {
                    continue;
                }
                let path = item.path().strip_prefix(&self.root)?.to_path_buf();
                let kind = entry_kind(&path);
                if kind == DocumentKind::Suite || folder == "common" {
                    entries.push(Entry {
                        editable: read_document(&self.resolve(&path)?).is_ok(),
                        path,
                        kind,
                    });
                }
            }
        }
        entries.sort_by_key(Entry::label);
        Ok(entries)
    }

    pub fn classify_entry(&self, relative: &Path) -> Result<Entry> {
        let resolved = self.resolve(relative)?;
        ensure!(
            resolved.is_file(),
            "Select a regular file inside the workspace"
        );
        Ok(Entry {
            editable: read_document(&resolved).is_ok(),
            path: relative.to_path_buf(),
            kind: entry_kind(resolved.strip_prefix(&self.root)?),
        })
    }

    pub fn open_document(&self, entry: Entry) -> Result<OpenDocument> {
        ensure!(
            entry.supports_editing(),
            "This file is not editable UTF-8 text (4 MiB limit)"
        );
        // Recheck the bytes and workspace boundary; discovery is only a UI hint.
        let source = read_document(&self.resolve(&entry.path)?)?;
        let document = Document::new(entry.kind, Format::for_path(&entry.path), source)?;
        Ok(OpenDocument { entry, document })
    }

    pub fn viewport_choices(
        &self,
        path: &Path,
        explicit: bool,
    ) -> Result<Vec<crate::config::viewports::ViewportDef>> {
        let resolved = self.resolve(path)?;
        if !resolved.exists() && !explicit {
            return Ok(Vec::new());
        }
        ensure!(
            resolved.is_file(),
            "Viewport file '{}' does not exist",
            display_path(path)
        );
        let document = self
            .open_document(Entry {
                path: path.into(),
                kind: DocumentKind::Viewports,
                editable: true,
            })?
            .document;
        document.validate()?;
        Ok(serde_json::from_value(document.value()?)?)
    }

    pub(crate) fn action_viewport_diagnostics(
        &self,
        document: &Document,
        suite_name: &str,
    ) -> Result<Vec<super::validation::Diagnostic>> {
        if document.kind != DocumentKind::Suite || document.format == Format::Text {
            return Ok(Vec::new());
        }
        // Source inspection owns syntax errors; contextual checks must not duplicate them.
        let Ok(value) = document.value() else {
            return Ok(Vec::new());
        };
        if !super::validation::diagnostics(document.kind, &value)?.is_empty() {
            return Ok(Vec::new());
        }
        let suite: crate::types::TestSuite = serde_json::from_value(value)?;
        self.action_viewport_diagnostics_for_suite(document.format, suite, suite_name)
    }

    pub(crate) fn action_viewport_diagnostics_for_suite(
        &self,
        format: Format,
        mut suite: crate::types::TestSuite,
        suite_name: &str,
    ) -> Result<Vec<super::validation::Diagnostic>> {
        crate::test::assign_scenario_ids(&mut suite.scenarios);
        let input = crate::types::TestSuiteInput {
            name: suite_name.into(),
            path: PathBuf::from(suite_name),
            format: match format {
                Format::Json => crate::types::TestSuiteFormat::Json,
                _ => crate::types::TestSuiteFormat::Yaml,
            },
        };
        let mut issues = Vec::new();
        if let Err(error) = crate::test::validate_scenario_dependencies(&input, &suite) {
            let pointer = error
                .downcast_ref::<crate::test::ScenarioDependencyError>()
                .map_or_else(String::new, |error| {
                    format!("/scenarios/{}/{}", error.scenario_index, error.field)
                });
            issues.push(super::validation::Diagnostic {
                pointer,
                message: error.to_string(),
            });
        }
        if !suite
            .scenarios
            .iter()
            .flat_map(|scenario| scenario.actions.iter().flatten())
            .any(|action| {
                action
                    .viewport_names()
                    .is_some_and(|names| !names.is_empty())
            })
        {
            return Ok(issues);
        }
        let workspace = crate::config::load_workspace_config(&self.root)?;
        let mut files = BTreeMap::new();
        for (index, scenario) in suite.scenarios.iter().enumerate() {
            let resolved = crate::config::resolve_config(&workspace, &suite, scenario);
            let key = (
                resolved.viewports_path.clone(),
                resolved.viewports_path_is_explicit,
            );
            if !files.contains_key(&key) {
                files.insert(
                    key.clone(),
                    self.viewport_choices(Path::new(&key.0), key.1)?,
                );
            }
            let effective = super::schema::action_viewport_choices(
                &serde_json::to_value(&resolved)?,
                &files[&key],
            )?;
            issues.extend(super::validation::action_viewport_diagnostics(
                suite_name, &suite, index, &effective,
            ));
            if issues.len() >= super::validation::MAX_DIAGNOSTICS {
                break;
            }
        }
        Ok(issues)
    }

    pub fn suite_catalog(&self) -> Result<Vec<SuiteSummary>> {
        let directory = self.resolve(Path::new("visual_tests"))?;
        if !directory.exists() {
            return Ok(Vec::new());
        }
        crate::test::discover_test_suites(&directory)?
            .into_iter()
            .map(|input| {
                let entry = Entry {
                    path: input.path.strip_prefix(&self.root)?.into(),
                    kind: DocumentKind::Suite,
                    editable: true,
                };
                let parsed = (|| -> Result<crate::types::TestSuite> {
                    ensure!(input.name != "all", "The suite name 'all' is reserved");
                    crate::test::validate_suite_identifier("Suite name", Some(&input.name))?;
                    let document = self.open_document(entry.clone())?.document;
                    let value = document.value()?;
                    super::document::validate_value(document.kind, &value)?;
                    let suite = parse_typed_source::<crate::types::TestSuite>(&document)?;
                    if let Some(id) = suite.id.as_deref() {
                        crate::test::validate_suite_identifier("Test suite ID", Some(id))?;
                    }
                    ensure!(!suite.scenarios.is_empty(), "Suite contains no scenarios");
                    Ok(suite)
                })();
                let output_id = parsed
                    .as_ref()
                    .ok()
                    .and_then(|suite| suite.id.clone())
                    .unwrap_or_else(|| input.name.clone());
                let scenarios = parsed
                    .as_ref()
                    .map(|suite| ScenarioSummary::from_scenarios(&suite.scenarios))
                    .unwrap_or_default();
                Ok(SuiteSummary {
                    entry,
                    name: input.name,
                    display_name: parsed
                        .as_ref()
                        .ok()
                        .and_then(|suite| suite.display_name.clone()),
                    output_id,
                    scenarios,
                    error: parsed.err().map(|error| format!("{error:#}")),
                })
            })
            .collect()
    }

    /// A filename filter must resolve to the selected file, not a shadowing format.
    pub fn runnable_suite(&self, entry: &Entry) -> Result<String> {
        let name = entry
            .suite_name()
            .ok_or_else(|| anyhow!("Select a test suite first"))?;
        ensure!(
            name != "all",
            "The suite name 'all' is reserved by the CLI wildcard filter"
        );
        crate::test::validate_suite_identifier("Suite name", Some(&name))?;
        let suites = crate::test::discover_test_suites(&self.resolve(Path::new("visual_tests"))?)?;
        let actual = suites
            .into_iter()
            .find(|suite| suite.name == name)
            .ok_or_else(|| anyhow!("Suite '{name}' is not discovered by the CLI"))?;
        ensure!(
            actual.path.canonicalize()? == self.resolve(&entry.path)?,
            "The CLI runs '{}', not the selected '{}'. Rename or remove the shadowing suite before running this file.",
            display_path(&actual.path),
            display_path(&entry.path)
        );
        Ok(name)
    }

    pub fn reload(&self, open: &mut OpenDocument) -> Result<()> {
        *open = self.open_document(open.entry.clone())?;
        Ok(())
    }

    /// Upload paths use visual_tests; other configured files use the workspace root.
    pub fn relative_file(&self, path: &Path, upload: bool) -> Result<String> {
        let canonical = path.canonicalize()?;
        let base = if upload {
            self.resolve(Path::new("visual_tests"))?
        } else {
            self.root.clone()
        };
        ensure!(
            canonical.is_file() && canonical.starts_with(&base),
            "Select a file inside {}",
            display_path(&base)
        );
        let relative = canonical.strip_prefix(base)?;
        Ok(display_path(relative))
    }

    pub fn check_unchanged(&self, open: &OpenDocument) -> Result<PathBuf> {
        let path = self.resolve(&open.entry.path)?;
        let current = read_document(&path)
            .context("File is unavailable; reload or Save As before continuing")?;
        ensure!(
            current == open.document.saved_source(),
            "File changed outside Regressify. Reload it or Save As a new file; your draft has been retained."
        );
        Ok(path)
    }

    pub fn save(&self, open: &mut OpenDocument) -> Result<()> {
        self.save_in_outputs(open, &[])
    }

    pub fn save_in_outputs(&self, open: &mut OpenDocument, outputs: &[PathBuf]) -> Result<()> {
        open.document.validate()?;
        validate_typed_source(&open.document)?;
        let suite_name = open
            .entry
            .suite_name()
            .unwrap_or_else(|| open.entry.label());
        if let Some(issue) = self
            .action_viewport_diagnostics(&open.document, &suite_name)?
            .first()
        {
            anyhow::bail!("{}", issue.message);
        }
        let path = self.check_unchanged(open)?;
        let value = open.document.value()?;
        // Invalid saved source must remain repairable; it has no reliable metadata to migrate.
        let saved = Document::new(
            open.document.kind,
            open.document.format,
            open.document.saved_source().into(),
        )?
        .value()
        .ok();
        let update = if open.entry.kind == DocumentKind::Suite
            && saved.is_some_and(|saved| value["displayName"] != saved["displayName"])
        {
            let (id, _) = self.suite_mutation_ids(open, None)?;
            artifacts::ArtifactUpdate::prepare(
                &self.suite_output_roots(outputs)?,
                &id,
                Some(&id),
                value["displayName"].as_str(),
            )?
        } else {
            artifacts::ArtifactUpdate::default()
        };
        update.commit(|| {
            self.check_unchanged(open)?;
            write_file_atomically(&path, open.document.source().as_bytes())
        })?;
        open.document.mark_saved();
        Ok(())
    }

    pub fn save_as(&self, open: &OpenDocument, relative: &Path) -> Result<OpenDocument> {
        open.document.validate()?;
        validate_typed_source(&open.document)?;
        let format = Format::for_path(relative);
        ensure!(
            (format == Format::Text) == (open.document.format == Format::Text),
            "Cannot convert between plain text and YAML/JSON; choose a matching file type"
        );
        let source = if format == open.document.format {
            open.document.source().to_string()
        } else {
            format.serialize(&open.document.value()?)?
        };
        self.create_document(relative, open.entry.kind, &source)
    }

    pub fn create_document(
        &self,
        relative: &Path,
        kind: DocumentKind,
        source: &str,
    ) -> Result<OpenDocument> {
        let path = self.resolve(relative)?;
        let resolved_relative = path.strip_prefix(&self.root)?;
        // Validate the actual destination, including aliases into visual_tests.
        let kind = if kind == DocumentKind::Suite
            || entry_kind(resolved_relative) == DocumentKind::Suite
        {
            let suite_name = Entry {
                path: resolved_relative.into(),
                kind: DocumentKind::Suite,
                editable: true,
            }
            .suite_name()
            .ok_or_else(|| {
                anyhow!("Test suite files must use .tests.yaml, .tests.yml, or .tests.json")
            })?;
            crate::test::validate_suite_identifier("Suite name", Some(&suite_name))?;
            DocumentKind::Suite
        } else if entry_kind(resolved_relative) == DocumentKind::ReplacementProfiles {
            // The runtime loads this exact name, so a copy saved there must be a valid profile file.
            DocumentKind::ReplacementProfiles
        } else {
            kind
        };
        let document = Document::new(kind, Format::for_path(relative), source.to_string())?;
        document.validate()?;
        validate_typed_source(&document)?;
        if let Some(issue) = self
            .action_viewport_diagnostics(&document, &display_path(relative))?
            .first()
        {
            anyhow::bail!("{}", issue.message);
        }
        ensure!(
            !path.exists(),
            "A file already exists at '{}'",
            display_path(relative)
        );
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("Missing parent directory"))?;
        fs::create_dir_all(parent)?;
        // persist_noclobber closes the create/save race without replacing another file.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(source.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist_noclobber(&path).map_err(|error| {
            anyhow!(
                "Cannot create '{}': {}",
                display_path(relative),
                error.error
            )
        })?;
        Ok(OpenDocument {
            entry: Entry {
                path: relative.into(),
                kind,
                editable: true,
            },
            document,
        })
    }

    pub fn create_suite(&self, name: &str) -> Result<OpenDocument> {
        let path = suite_path(name)?;
        let source = Format::for_path(&path).serialize(&serde_json::json!({
            "scenarios": [{"label": "Scenario 1", "url": "https://example.com", "actions": []}]
        }))?;
        self.create_document(&path, DocumentKind::Suite, &source)
    }

    pub fn duplicate_suite(&self, open: &OpenDocument, name: &str) -> Result<OpenDocument> {
        ensure!(
            open.entry.kind == DocumentKind::Suite,
            "Only test suites can be duplicated here"
        );
        let mut copy = Document::new(
            DocumentKind::Suite,
            open.document.format,
            open.document.source().into(),
        )?;
        // An explicit suite ID would otherwise make the copy share output artifacts.
        copy.apply(super::document::Edit::Set {
            parent: "".into(),
            key: "id".into(),
            value: None,
        })?;
        let copy = OpenDocument {
            entry: open.entry.clone(),
            document: copy,
        };
        self.save_as(&copy, &suite_path(name)?)
    }

    pub fn rename_suite(&self, open: &mut OpenDocument, name: &str) -> Result<()> {
        let display_name = open.document.value()?["displayName"]
            .as_str()
            .map(str::to_owned);
        self.rename_suite_in_outputs(open, name, display_name.as_deref(), &[])
    }

    pub fn rename_suite_in_outputs(
        &self,
        open: &mut OpenDocument,
        name: &str,
        display_name: Option<&str>,
        outputs: &[PathBuf],
    ) -> Result<()> {
        ensure!(
            open.entry.kind == DocumentKind::Suite,
            "Only test suites can be renamed here"
        );
        ensure!(
            !open.document.dirty(),
            "Save or discard changes before renaming"
        );
        let relative = suite_path(name)?;
        ensure!(
            Format::for_path(&relative) == open.document.format,
            "Use Save As to change file format"
        );
        let source = self.check_unchanged(open)?;
        let destination = self.resolve(&relative)?;
        let same_file = relative == open.entry.path;
        ensure!(
            same_file || !destination.exists(),
            "Destination already exists"
        );
        let mut document = Document::new(
            DocumentKind::Suite,
            open.document.format,
            open.document.source().into(),
        )?;
        document.apply(super::document::Edit::Set {
            parent: String::new(),
            key: "displayName".into(),
            value: display_name
                .filter(|name| !name.trim().is_empty())
                .map(|name| serde_json::json!(name)),
        })?;
        document.validate()?;
        validate_typed_source(&document)?;
        let (old_id, new_id) = self.suite_mutation_ids(open, Some(&relative))?;
        let update = artifacts::ArtifactUpdate::prepare(
            &self.suite_output_roots(outputs)?,
            &old_id,
            new_id.as_deref(),
            display_name,
        )?;
        update.commit(|| {
            self.check_unchanged(open)?;
            if same_file {
                write_file_atomically(&source, document.source().as_bytes())?;
            } else {
                self.create_document(&relative, DocumentKind::Suite, document.source())?;
                if let Err(error) = fs::remove_file(&source) {
                    fs::remove_file(&destination).context("Cannot roll back the new suite file")?;
                    return Err(error).context("Cannot remove the old suite name");
                }
            }
            Ok(())
        })?;
        document.mark_saved();
        open.document = document;
        open.entry.path = relative;
        Ok(())
    }

    pub fn delete_suite(&self, open: &OpenDocument) -> Result<()> {
        self.delete_suite_in_outputs(open, &[])
    }

    pub fn delete_suite_in_outputs(&self, open: &OpenDocument, outputs: &[PathBuf]) -> Result<()> {
        ensure!(
            open.entry.kind == DocumentKind::Suite,
            "Only test suite files can be deleted here"
        );
        ensure!(
            !open.document.dirty(),
            "Save or discard changes before deleting"
        );
        self.check_unchanged(open)?;
        let (old_id, _) = self.suite_mutation_ids(open, None)?;
        let update = artifacts::ArtifactUpdate::prepare(
            &self.suite_output_roots(outputs)?,
            &old_id,
            None,
            None,
        )?;
        update
            .commit(|| fs::remove_file(self.check_unchanged(open)?).context("Cannot delete suite"))
    }

    fn suite_mutation_ids(
        &self,
        open: &OpenDocument,
        destination: Option<&Path>,
    ) -> Result<(String, Option<String>)> {
        let value = open.document.value()?;
        let id = value
            .get("id")
            .filter(|id| !id.is_null())
            .map(|id| id.as_str().context("Suite ID must be a string"))
            .transpose()?;
        let old_id = id
            .map(str::to_owned)
            .or_else(|| open.entry.suite_name())
            .context("Missing suite identity")?;
        let new_id = destination
            .map(|path| {
                id.map(str::to_owned)
                    .or_else(|| {
                        Entry {
                            path: path.into(),
                            kind: DocumentKind::Suite,
                            editable: true,
                        }
                        .suite_name()
                    })
                    .context("Missing destination suite identity")
            })
            .transpose()?;
        for id in std::iter::once(&old_id).chain(new_id.iter()) {
            validate_component(id)?;
            ensure!(
                id.len() <= 80 && id.trim() == id,
                "Suite output IDs must be portable components no longer than 80 bytes without surrounding whitespace"
            );
            ensure!(
                !id.starts_with('.') && !crate::test::is_reserved_suite_identifier(id),
                "Reserved suite output ID"
            );
        }
        for entry in self
            .entries()?
            .into_iter()
            .filter(|entry| entry.kind == DocumentKind::Suite && entry.path != open.entry.path)
        {
            let other = self.open_document(entry.clone())?;
            let value = other
                .document
                .value()
                .context("Cannot determine artifact ownership while another suite is invalid")?;
            let other_id = value["id"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| entry.suite_name())
                .context("Missing other suite identity")?;
            ensure!(
                !other_id.eq_ignore_ascii_case(&old_id),
                "Another suite shares this output ID; resolve the duplicate before renaming or deleting"
            );
            if let Some(new_id) = &new_id {
                ensure!(
                    !other_id.eq_ignore_ascii_case(new_id),
                    "Another suite already uses the destination output ID"
                );
            }
            if let Some(destination) = destination {
                ensure!(
                    entry.suite_name()
                        != (Entry {
                            path: destination.into(),
                            kind: DocumentKind::Suite,
                            editable: true,
                        })
                        .suite_name(),
                    "Another file format already uses the destination suite name"
                );
            }
        }
        Ok((old_id, new_id))
    }

    fn suite_output_roots(&self, outputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut candidates = BTreeSet::from([self.root.join(".regressify")]);
        candidates.extend(outputs.iter().map(|path| self.root.join(path)));
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".regressify__")
            {
                candidates.insert(entry.path());
            }
        }
        let mut roots = BTreeSet::new();
        for path in candidates {
            if let Some(root) = artifacts::existing_root(&path)? {
                ensure!(
                    root != self.root && !self.root.starts_with(&root),
                    "Output root cannot contain the workspace"
                );
                ensure!(
                    roots.iter().all(|existing: &PathBuf| existing == &root
                        || (!existing.starts_with(&root) && !root.starts_with(existing))),
                    "Suite output roots cannot overlap"
                );
                roots.insert(root);
            }
        }
        Ok(roots.into_iter().collect())
    }
}

fn entry_kind(relative: &Path) -> DocumentKind {
    let name = relative
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut components = relative.components();
    let first = components.next().and_then(|part| part.as_os_str().to_str());
    let in_folder = |folder: &str| {
        first.is_some_and(|first| {
            first == folder || (cfg!(windows) && first.eq_ignore_ascii_case(folder))
        })
    };
    if components.next().is_none() && matches!(name.as_str(), "regressify.yaml" | "regressify.yml")
    {
        return DocumentKind::Workspace;
    }
    if in_folder("visual_tests")
        && [".tests.yaml", ".tests.yml", ".tests.json"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
    {
        return DocumentKind::Suite;
    }
    if in_folder("common")
        && Format::for_path(relative) != Format::Text
        && name.contains("viewports")
    {
        return DocumentKind::Viewports;
    }
    // Only the exact names the runtime loads; `components` is exhausted when the file is directly in `common/`.
    if in_folder("common")
        && components.next().is_none()
        && matches!(
            name.as_str(),
            "_replacement-profiles.yaml" | "_replacement-profiles.yml"
        )
    {
        return DocumentKind::ReplacementProfiles;
    }
    DocumentKind::Data
}

fn validate_component(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && !name.ends_with(['.', ' '])
            && !name
                .chars()
                .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
            && !is_windows_reserved_path_component(name),
        "Invalid portable filename '{name}'"
    );
    Ok(())
}

pub fn suite_path(name: &str) -> Result<PathBuf> {
    validate_component(name)?;
    let normalized = name.to_ascii_lowercase();
    let stem = [".tests.yaml", ".tests.yml", ".tests.json"]
        .iter()
        .find_map(|suffix| normalized.strip_suffix(suffix))
        .unwrap_or(&normalized);
    ensure!(stem != "all", "The suite name 'all' is reserved by the CLI");
    ensure!(
        !stem.is_empty() && !stem.starts_with('.'),
        "Suite name must not start with '.'"
    );
    ensure!(
        !crate::test::is_reserved_suite_identifier(stem),
        "The suite name '{stem}' is reserved"
    );
    let filename = if [".tests.yaml", ".tests.yml", ".tests.json"]
        .iter()
        .any(|suffix| name.to_ascii_lowercase().ends_with(suffix))
    {
        name.to_string()
    } else {
        format!("{name}.tests.yaml")
    };
    Ok(Path::new("visual_tests").join(filename))
}

fn read_document(path: &Path) -> Result<String> {
    let file =
        fs::File::open(path).with_context(|| format!("Cannot read '{}'", display_path(path)))?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Document must be a regular file");
    ensure!(
        metadata.len() <= MAX_DOCUMENT_BYTES as u64,
        "Document exceeds the 4 MiB editing limit"
    );
    let mut source = String::new();
    file.take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_string(&mut source)?;
    ensure!(
        source.len() <= MAX_DOCUMENT_BYTES,
        "Document exceeds the 4 MiB editing limit"
    );
    ensure!(is_text(&source), "Binary content cannot be edited as text");
    Ok(source)
}

fn parse_typed_source<T: serde::de::DeserializeOwned>(document: &Document) -> Result<T> {
    match document.format {
        Format::Yaml => Ok(serde_yaml::from_str::<T>(document.source())?),
        Format::Json => Ok(serde_json::from_str::<T>(document.source())?),
        Format::Text => Err(anyhow!("Configuration documents must use YAML or JSON")),
    }
}

fn validate_typed_source(document: &Document) -> Result<()> {
    // Parsing structs directly also catches duplicate properties with source locations.
    match document.kind {
        DocumentKind::Workspace => {
            parse_typed_source::<crate::config::WorkspaceConfig>(document).map(|_| ())
        }
        DocumentKind::Suite => parse_typed_source::<crate::types::TestSuite>(document).map(|_| ()),
        DocumentKind::Viewports => {
            parse_typed_source::<Vec<crate::config::viewports::ViewportDef>>(document).map(|_| ())
        }
        DocumentKind::ReplacementProfiles => {
            parse_typed_source::<crate::config::replacements::ReplacementProfilesFile>(document)
                .map(|_| ())
        }
        DocumentKind::Data => Ok(()),
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub fn is_dark(self, system_dark: bool) -> bool {
        match self {
            Self::System => system_dark,
            Self::Light => false,
            Self::Dark => true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    pub recent_workspaces: Vec<PathBuf>,
    pub dark_mode: bool,
    pub navigator_hidden: bool,
    #[serde(default)]
    pub theme: Option<ThemePreference>,
    pub source_wrap: bool,
    pub source_line_numbers: bool,
    pub source_font_size: u8,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            recent_workspaces: Vec::new(),
            dark_mode: false,
            navigator_hidden: false,
            theme: Some(ThemePreference::System),
            source_wrap: true,
            source_line_numbers: true,
            source_font_size: 14,
        }
    }
}

impl Preferences {
    pub fn theme_preference(&self) -> ThemePreference {
        self.theme.unwrap_or(if self.dark_mode {
            ThemePreference::Dark
        } else {
            ThemePreference::Light
        })
    }

    pub fn set_theme_preference(&mut self, preference: ThemePreference) {
        self.theme = Some(preference);
        self.dark_mode = preference == ThemePreference::Dark;
    }

    pub fn path() -> Result<PathBuf> {
        directories::ProjectDirs::from("", "Regressify", "Regressify Desktop")
            .map(|directories| directories.config_dir().join("settings.json"))
            .ok_or_else(|| anyhow!("Cannot determine the desktop settings directory"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(source) => {
                let mut preferences: Self =
                    serde_json::from_str(&source).context("Invalid desktop preferences")?;
                preferences.source_font_size = preferences.source_font_size.clamp(10, 24);
                Ok(preferences)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn remember(&mut self, workspace: &Workspace) {
        self.recent_workspaces
            .retain(|path| path != &workspace.root);
        self.recent_workspaces.insert(0, workspace.root.clone());
        self.recent_workspaces.truncate(10);
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow!("Invalid preferences path"))?,
        )?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        lock.lock()
            .context("Cannot lock shared desktop preferences")?;
        let latest = Self::load(path)?;
        let mut merged = self.clone();
        for root in latest.recent_workspaces {
            if !merged.recent_workspaces.contains(&root) {
                merged.recent_workspaces.push(root);
            }
        }
        merged.recent_workspaces.truncate(10);
        write_file_atomically(path, &serde_json::to_vec_pretty(&merged)?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::document::Edit;
    use super::*;
    use serde_json::json;

    #[test]
    fn common_text_files_are_editable_and_save_without_structured_validation() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        for (relative, source) in [
            (
                "common/_on-ready.js",
                "module.exports = async ({ page }) => {\r\n  await page.title();\r\n};\r\n",
            ),
            (
                "common/_on-ready.css",
                "body::before { content: \"ready: [\"; }\n",
            ),
            (
                "common/_override.css",
                "* { animation: none !important; }\n",
            ),
            ("common/nested/README", "Text with [unfinished markup\n"),
            ("common/notes.custom", "caf\u{00e9}\t[\r\n"),
            ("common/.keep", ""),
        ] {
            let path = directory.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, source).unwrap();
            let entry = workspace
                .entries()
                .unwrap()
                .into_iter()
                .find(|entry| entry.path == Path::new(relative))
                .unwrap();
            assert!(entry.supports_editing(), "{relative}");
            let mut open = workspace.open_document(entry).unwrap();
            assert_eq!(open.document.format, Format::Text);
            assert_eq!(open.document.source(), source);
            open.document.validate().unwrap();
            let updated = format!("{source}\nUpdated: [\n");
            open.document.set_source(updated.clone()).unwrap();
            open.document.undo();
            assert_eq!(open.document.source(), source);
            open.document.redo();
            assert_eq!(open.document.source(), updated);
            workspace.save(&mut open).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), updated);
            assert!(!open.document.dirty());
        }
    }

    #[test]
    fn common_text_files_reject_binary_and_oversized_content_even_after_discovery() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        fs::create_dir(directory.path().join("common")).unwrap();
        for (relative, bytes) in [
            ("common/image.png", b"\x89PNG\r\n\x1a\n".as_slice()),
            ("common/invalid.js", b"invalid\xFF".as_slice()),
            ("common/binary.css", b"binary\0text".as_slice()),
            ("common/control.txt", b"binary\x01text".as_slice()),
            ("common/binary.yaml", b"binary\0text".as_slice()),
        ] {
            fs::write(directory.path().join(relative), bytes).unwrap();
        }
        for mut entry in workspace.entries().unwrap() {
            assert!(!entry.supports_editing(), "{}", entry.label());
            assert!(workspace.open_document(entry.clone()).is_err());
            entry.editable = true;
            assert!(workspace.open_document(entry).is_err());
        }

        let path = directory.path().join("common/large.txt");
        let source = "a".repeat(MAX_DOCUMENT_BYTES);
        fs::write(&path, &source).unwrap();
        let entry = workspace
            .entries()
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == Path::new("common/large.txt"))
            .unwrap();
        assert!(entry.supports_editing());
        let mut open = workspace.open_document(entry.clone()).unwrap();
        assert_eq!(open.document.source().len(), MAX_DOCUMENT_BYTES);
        assert!(open.document.set_source(format!("{source}a")).is_err());
        fs::write(&path, format!("{source}a")).unwrap();
        assert!(workspace.open_document(entry.clone()).is_err());
        assert!(
            workspace
                .entries()
                .unwrap()
                .iter()
                .all(|entry| !entry.supports_editing())
        );
        fs::write(&path, b"changed to binary\0").unwrap();
        assert!(workspace.open_document(entry).is_err());
    }

    #[test]
    fn common_text_files_preserve_drafts_conflicts_and_save_as_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let path = Path::new("common/hook.js");
        let original = "// caf\u{00e9}\r\nconst ready = () => {};\r\n";
        let mut open = workspace
            .create_document(path, DocumentKind::Data, original)
            .unwrap();
        let draft = "// Updated\r\nconst ready = () => {\r\n";
        open.document.set_source(draft.into()).unwrap();
        fs::write(directory.path().join(path), "External change\n").unwrap();
        assert!(workspace.save(&mut open).is_err());
        assert_eq!(
            fs::read_to_string(directory.path().join(path)).unwrap(),
            "External change\n"
        );
        assert_eq!(open.document.source(), draft);
        assert!(open.document.dirty());
        fs::remove_file(directory.path().join(path)).unwrap();
        assert!(workspace.save(&mut open).is_err());
        assert_eq!(open.document.source(), draft);
        let copy_path = Path::new("common/nested/copy.custom");
        let copy = workspace.save_as(&open, copy_path).unwrap();
        assert_eq!(copy.document.source(), draft);
        assert_eq!(copy.document.format, Format::Text);
        assert_eq!(
            fs::read(directory.path().join(copy_path)).unwrap(),
            draft.as_bytes()
        );
        assert!(!copy.document.dirty());
        assert!(open.document.dirty());
        assert!(workspace.save_as(&open, copy_path).is_err());
        assert!(workspace.save_as(&open, Path::new("../escape.js")).is_err());
        assert!(
            workspace
                .save_as(&open, Path::new("common/copy.yaml"))
                .is_err()
        );
        assert!(!directory.path().join("common/copy.yaml").exists());
        open.document.set_source("binary\0data".into()).unwrap();
        assert!(
            workspace
                .save_as(&open, Path::new("common/binary.js"))
                .is_err()
        );
        assert!(!directory.path().join("common/binary.js").exists());
    }

    #[test]
    fn entries_include_all_common_files_and_only_visual_test_suites() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let expected = [
            ("common/.keep", DocumentKind::Data),
            ("common/README", DocumentKind::Data),
            ("common/_on-ready.js", DocumentKind::Data),
            ("common/_override.css", DocumentKind::Data),
            (
                "common/_replacement-profiles.yaml",
                DocumentKind::ReplacementProfiles,
            ),
            (
                "common/_replacement-profiles.yml",
                DocumentKind::ReplacementProfiles,
            ),
            ("common/_viewports.yaml", DocumentKind::Viewports),
            ("common/data.json", DocumentKind::Data),
            (
                "common/nested/_replacement-profiles.yaml",
                DocumentKind::Data,
            ),
            ("common/nested/helper.tests.json", DocumentKind::Data),
            ("common/nested/hook.js", DocumentKind::Data),
            ("common/template.tests.yaml", DocumentKind::Data),
            ("common/viewports-preview.png", DocumentKind::Data),
            ("common/viewports.json", DocumentKind::Viewports),
            ("common/viewports.yml", DocumentKind::Viewports),
            ("regressify.yaml", DocumentKind::Workspace),
            ("visual_tests/home.tests.yaml", DocumentKind::Suite),
        ];
        for (path, _) in &expected {
            let destination = directory.path().join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::write(destination, b"not parsed during discovery\xFF").unwrap();
        }
        for path in [
            "visual_tests/helper.js",
            "visual_tests/data.json",
            "other.txt",
        ] {
            fs::write(directory.path().join(path), "not listed").unwrap();
        }
        fs::create_dir_all(directory.path().join("common/empty-folder")).unwrap();

        let entries = workspace.entries().unwrap();
        let actual: Vec<_> = entries
            .iter()
            .map(|entry| (entry.label(), entry.kind))
            .collect();

        assert_eq!(
            actual,
            expected.map(|(path, kind)| (path.to_string(), kind))
        );
        for (path, expected_kind) in &expected {
            let classified = workspace.classify_entry(Path::new(path)).unwrap();
            assert_eq!(classified.kind, *expected_kind, "{path}");
            assert!(!classified.supports_editing(), "{path}");
        }
        for path in [
            "visual_tests/helper.js",
            "visual_tests/data.json",
            "other.txt",
        ] {
            let classified = workspace.classify_entry(Path::new(path)).unwrap();
            assert_eq!(classified.kind, DocumentKind::Data, "{path}");
            assert!(classified.supports_editing(), "{path}");
        }
        assert!(
            workspace
                .classify_entry(Path::new("../outside.yaml"))
                .is_err()
        );
        for entry in entries
            .into_iter()
            .filter(|entry| !entry.supports_editing())
        {
            let error = workspace.open_document(entry).unwrap_err();
            assert_eq!(
                error.to_string(),
                "This file is not editable UTF-8 text (4 MiB limit)"
            );
        }
    }

    #[test]
    fn editor_display_preferences_default_safely_and_clamp_saved_font_sizes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        for (text, expected) in [
            (r#"{}"#, 14),
            (r#"{"sourceFontSize":1}"#, 10),
            (r#"{"sourceFontSize":240}"#, 24),
        ] {
            fs::write(&path, text).unwrap();
            let preferences = Preferences::load(&path).unwrap();
            assert_eq!(preferences.source_font_size, expected);
            assert!(preferences.source_wrap);
            assert!(preferences.source_line_numbers);
        }
        fs::write(
            &path,
            r#"{"sourceFontSize":18,"sourceWrap":false,"sourceLineNumbers":false}"#,
        )
        .unwrap();
        let preferences = Preferences::load(&path).unwrap();
        assert_eq!(preferences.source_font_size, 18);
        assert!(!preferences.source_wrap);
        assert!(!preferences.source_line_numbers);
    }

    #[test]
    fn theme_preferences_preserve_legacy_choices_and_follow_system_only_when_selected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        assert_eq!(
            Preferences::load(&path).unwrap().theme_preference(),
            ThemePreference::System
        );
        for (json, expected) in [
            (r#"{"darkMode":true}"#, ThemePreference::Dark),
            (r#"{"darkMode":false}"#, ThemePreference::Light),
            (
                r#"{"darkMode":true,"theme":"system"}"#,
                ThemePreference::System,
            ),
            (
                r#"{"darkMode":true,"theme":"light"}"#,
                ThemePreference::Light,
            ),
        ] {
            assert_eq!(
                serde_json::from_str::<Preferences>(json)
                    .unwrap()
                    .theme_preference(),
                expected
            );
        }
        for preference in [
            ThemePreference::System,
            ThemePreference::Light,
            ThemePreference::Dark,
        ] {
            let mut preferences = Preferences::default();
            preferences.set_theme_preference(preference);
            preferences.save(&path).unwrap();
            assert_eq!(
                Preferences::load(&path).unwrap().theme_preference(),
                preference
            );
        }
        assert!(ThemePreference::System.is_dark(true));
        assert!(!ThemePreference::System.is_dark(false));
        assert!(!ThemePreference::Light.is_dark(true));
        assert!(ThemePreference::Dark.is_dark(false));
        assert!(serde_json::from_str::<Preferences>(r#"{"theme":"unknown"}"#).is_err());
    }

    #[test]
    fn displayed_paths_remove_extended_prefixes_without_losing_network_roots() {
        for (source, expected) in [
            (r"\\?\D:\git\QA workspace", "D:/git/QA workspace"),
            (r"D:\git\QA workspace", "D:/git/QA workspace"),
            (
                r"\\?\UNC\server\share\tests.yaml",
                "//server/share/tests.yaml",
            ),
            (r"\\server\share\tests.yaml", "//server/share/tests.yaml"),
            ("//?/D:/git/tests", "D:/git/tests"),
            ("//?/UNC/server/share", "//server/share"),
            (
                r"visual_tests\checkout.tests.yaml",
                "visual_tests/checkout.tests.yaml",
            ),
            ("/home/qa/tests", "/home/qa/tests"),
        ] {
            let path = PathBuf::from(source);
            assert_eq!(display_path(&path), expected);
            assert_eq!(path, PathBuf::from(source));
        }
    }

    #[test]
    fn action_viewport_names_round_trips_fields_and_source() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        fs::create_dir_all(directory.path().join("common")).unwrap();
        fs::write(directory.path().join("common/_viewports.yaml"), "- label: desktop\n  width: 1280\n  height: 720\n- label: mobile\n  width: 375\n  height: 667\n- label: tablet\n  width: 768\n  height: 1024\n").unwrap();
        let viewports = workspace
            .viewport_choices(Path::new("common/_viewports.yaml"), false)
            .unwrap();
        for (format, suffix) in [(Format::Yaml, "yaml"), (Format::Json, "json")] {
            let value = json!({ "scenarios": [{ "id": "page", "url": "/", "viewportNames": ["desktop", "mobile"], "actions": [{ "click": "#toggle" }] }] });
            let source = format.serialize(&value).unwrap();
            let path = PathBuf::from(format!("visual_tests/viewport.{suffix}.tests.{suffix}"));
            let mut open = workspace
                .create_document(&path, DocumentKind::Suite, &source)
                .unwrap();
            let field =
                super::super::schema::fields(&super::super::schema::Scope::Action("click".into()))
                    .unwrap()
                    .into_iter()
                    .find(|field| field.key == "viewportNames")
                    .unwrap();
            assert!(
                open.document.value().unwrap()["scenarios"][0]["actions"][0]
                    .get("viewportNames")
                    .is_none()
            );
            let effective = super::super::schema::effective(None, Some(&value), Some(0)).unwrap();
            let choices = super::super::schema::action_viewport_choices(&effective, &viewports)
                .unwrap()
                .into_iter()
                .map(|viewport| viewport.label)
                .collect::<Vec<_>>();
            assert_eq!(choices, ["desktop", "mobile"]);
            let selected = field
                .toggle_list_choice(
                    &serde_json::Value::Null,
                    &effective["viewportNames"],
                    "desktop",
                    &choices,
                )
                .unwrap();
            assert_eq!(selected, json!(["mobile"]));
            open.document
                .apply(Edit::Set {
                    parent: "/scenarios/0/actions/0".into(),
                    key: field.key.clone(),
                    value: Some(selected),
                })
                .unwrap();
            assert_eq!(
                open.document
                    .value()
                    .unwrap()
                    .pointer("/scenarios/0/actions/0/viewportNames"),
                Some(&json!(["mobile"]))
            );
            assert!(
                open.document.source().contains("viewportNames")
                    && open.document.source().contains("mobile")
            );
            assert!(
                workspace
                    .action_viewport_diagnostics(&open.document, "viewport")
                    .unwrap()
                    .is_empty()
            );
            workspace.save(&mut open).unwrap();
            let scalar = field.parse("mobile").unwrap();
            assert_eq!(scalar, json!("mobile"));
            open.document
                .apply(Edit::Set {
                    parent: "/scenarios/0/actions/0".into(),
                    key: field.key.clone(),
                    value: Some(scalar),
                })
                .unwrap();
            workspace.save(&mut open).unwrap();
            let saved = fs::read(directory.path().join(&path)).unwrap();
            open.document
                .apply(Edit::Set {
                    parent: "/scenarios/0/actions/0".into(),
                    key: field.key.clone(),
                    value: Some(field.parse("tablet").unwrap()),
                })
                .unwrap();
            let issues = workspace
                .action_viewport_diagnostics(&open.document, "viewport")
                .unwrap();
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].pointer, "/scenarios/0/actions/0/viewportNames");
            assert!(issues[0].message.contains("tablet"));
            assert!(workspace.save(&mut open).is_err());
            assert!(
                workspace
                    .save_as(
                        &open,
                        &PathBuf::from(format!("visual_tests/invalid.tests.{suffix}"))
                    )
                    .is_err()
            );
            assert_eq!(fs::read(directory.path().join(&path)).unwrap(), saved);
            assert_eq!(
                open.document
                    .value()
                    .unwrap()
                    .pointer("/scenarios/0/actions/0/viewportNames"),
                Some(&json!("tablet"))
            );
            assert!(open.document.source().contains("tablet"));
            open.document.undo();
            assert_eq!(
                open.document
                    .value()
                    .unwrap()
                    .pointer("/scenarios/0/actions/0/viewportNames"),
                Some(&json!("mobile"))
            );
            open.document.redo();
            assert!(open.document.source().contains("tablet"));
        }
    }

    #[test]
    fn action_viewport_diagnostics_bound_errors_and_reuse_cli_dependencies() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let malformed =
            Document::new(DocumentKind::Suite, Format::Yaml, "scenarios: [}".into()).unwrap();
        assert!(
            workspace
                .action_viewport_diagnostics(&malformed, "fixture")
                .unwrap()
                .is_empty()
        );
        let document = |value: serde_json::Value| {
            Document::new(DocumentKind::Suite, Format::Json, value.to_string()).unwrap()
        };
        let unnamed = document(
            json!({ "scenarios": [{ "url": "/", "actions": [{ "wait": 0, "viewportNames": ["mobile"] }] }] }),
        );
        let issues = workspace
            .action_viewport_diagnostics(&unnamed, "fixture")
            .unwrap();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("<unnamed browser default>"));
        let inherited = document(json!({ "scenarios": [
            { "label": "Setup", "url": "/setup", "actions": [{ "wait": 0, "viewportNames": ["mobile"] }] },
            { "id": "consumer", "url": "/consumer", "needs": "setup" }
        ] }));
        let issues = workspace
            .action_viewport_diagnostics(&inherited, "fixture")
            .unwrap();
        assert_eq!(issues.len(), 2);
        assert!(issues[1].message.contains("prerequisite"));
        assert_eq!(issues[1].pointer, "/scenarios/0/actions/0/viewportNames");
        let cyclic = document(json!({ "scenarios": [
            { "id": "first", "url": "/", "needs": "second", "actions": [{ "wait": 0, "viewportNames": ["mobile"] }] },
            { "id": "second", "url": "/", "needs": "first" }
        ] }));
        assert!(
            workspace
                .action_viewport_diagnostics(&cyclic, "fixture")
                .unwrap()
                .iter()
                .any(|issue| issue.message.contains("Circular scenario dependency"))
        );
        let many = document(json!({ "scenarios": (0..60).map(|index| json!({
            "id": format!("page-{index}"), "url": "/", "actions": [{ "wait": 0, "viewportNames": ["missing"] }]
        })).collect::<Vec<_>>() }));
        assert_eq!(
            workspace
                .action_viewport_diagnostics(&many, "fixture")
                .unwrap()
                .len(),
            super::super::validation::MAX_DIAGNOSTICS
        );
    }

    #[test]
    fn source_validation_dependencies_without_viewport_filters_block_save() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let mut open = workspace.create_suite("dependencies.tests.json").unwrap();
        let original = fs::read_to_string(directory.path().join(&open.entry.path)).unwrap();
        for scenarios in [
            json!([{"id":"page","url":"/","needs":"missing"}]),
            json!([
                {"id":"first","url":"/","needs":"second"},
                {"id":"second","url":"/","needs":"first"}
            ]),
            json!([{"id":"same","url":"/"},{"id":"SAME","url":"/"}]),
        ] {
            let source = json!({"scenarios":scenarios}).to_string();
            open.document.set_source(source.clone()).unwrap();
            let issues = workspace
                .action_viewport_diagnostics(&open.document, "dependencies")
                .unwrap();
            assert_eq!(issues.len(), 1, "{source}");
            assert!(issues[0].pointer.starts_with("/scenarios/"));
            assert!(issues[0].pointer.ends_with("/needs") || issues[0].pointer.ends_with("/id"));
            assert!(workspace.save(&mut open).is_err());
            assert_eq!(open.document.source(), source);
            assert_eq!(
                fs::read_to_string(directory.path().join(&open.entry.path)).unwrap(),
                original
            );
        }
    }

    #[test]
    fn source_validation_dependency_errors_do_not_hide_viewport_errors() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let document = Document::new(
            DocumentKind::Suite,
            Format::Json,
            json!({"scenarios":[{"id":"page","url":"/","needs":"missing",
                "actions":[{"wait":0,"viewportNames":["mobile"]}]}]})
            .to_string(),
        )
        .unwrap();
        let issues = workspace
            .action_viewport_diagnostics(&document, "dependencies")
            .unwrap();
        assert!(
            issues
                .iter()
                .any(|issue| issue.pointer == "/scenarios/0/needs")
        );
        assert!(
            issues
                .iter()
                .any(|issue| { issue.pointer == "/scenarios/0/actions/0/viewportNames" })
        );
    }

    #[test]
    fn viewport_choices_validate_files_and_respect_workspace_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let path = Path::new("common/_viewports.yaml");
        assert!(workspace.viewport_choices(path, false).unwrap().is_empty());
        assert!(workspace.viewport_choices(path, true).is_err());
        fs::create_dir_all(directory.path().join("common")).unwrap();
        fs::write(
            directory.path().join(path),
            "- label: Phone\n  width: 390\n  height: 844\n",
        )
        .unwrap();
        let choices = workspace.viewport_choices(path, true).unwrap();
        assert_eq!(choices[0].label, "Phone");
        assert_eq!(choices[0].width, 390);
        assert!(
            workspace
                .viewport_choices(Path::new("../outside.yaml"), true)
                .is_err()
        );
        fs::write(
            directory.path().join(path),
            "- label: Phone\n  width: 0\n  height: 844\n",
        )
        .unwrap();
        assert!(workspace.viewport_choices(path, true).is_err());
    }

    #[test]
    fn create_edit_save_and_reload_suite() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let mut open = workspace.create_suite("checkout").unwrap();
        open.document
            .apply(Edit::Set {
                parent: "/scenarios/0".into(),
                key: "label".into(),
                value: Some(json!("Checkout")),
            })
            .unwrap();
        assert!(open.document.dirty());
        workspace.save(&mut open).unwrap();
        assert!(!open.document.dirty());
        assert_eq!(workspace.entries().unwrap().len(), 1);
        workspace.reload(&mut open).unwrap();
        assert_eq!(
            open.document.value().unwrap()["scenarios"][0]["label"],
            "Checkout"
        );
        assert!(workspace.create_suite("checkout").is_err());
    }

    #[test]
    fn external_changes_and_deletion_never_get_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let mut open = workspace.create_suite("home").unwrap();
        let path = workspace.resolve(&open.entry.path).unwrap();
        fs::write(&path, "# external\nscenarios: []\n").unwrap();
        assert!(
            workspace
                .save(&mut open)
                .unwrap_err()
                .to_string()
                .contains("changed outside")
        );
        assert!(workspace.delete_suite(&open).is_err());
        assert!(workspace.rename_suite(&mut open, "other").is_err());
        assert!(fs::read_to_string(&path).unwrap().contains("external"));
        fs::remove_file(path).unwrap();
        assert!(workspace.save(&mut open).is_err());
    }

    #[test]
    fn invalid_and_duplicate_fields_cannot_be_saved() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let mut open = workspace.create_suite("home").unwrap();
        for source in [
            "scenarios: [",
            "scenarios: []\ndebug: true\ndebug: false\n",
            "scenarios: []\nunknown: 1\n",
        ] {
            open.document.set_source(source.into()).unwrap();
            assert!(workspace.save(&mut open).is_err());
        }
        let disk = workspace.open_document(open.entry.clone()).unwrap();
        assert_eq!(
            disk.document.value().unwrap()["scenarios"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn corrected_invalid_source_can_be_saved() {
        let valid_yaml = "scenarios:\n- url: https://example.com\n";
        let valid_json = "{\"scenarios\": [{\"url\": \"https://example.com\"}]}\n";
        for (relative, invalid, corrected) in [
            ("visual_tests/home.tests.yaml", "scenarios: [", valid_yaml),
            (
                "visual_tests/home.tests.json",
                r#"{"scenarios": ["#,
                valid_json,
            ),
            (
                "visual_tests/home.tests.yaml",
                "scenarios: not-a-list\n",
                valid_yaml,
            ),
            (
                "visual_tests/home.tests.json",
                r#"{"scenarios": "not-a-list"}"#,
                valid_json,
            ),
            (
                "visual_tests/home.tests.yaml",
                "displayName: [\nscenarios: []\n",
                "displayName: Repaired suite\nscenarios:\n- url: https://example.com\n",
            ),
            ("regressify.yaml", "[", "# Default workspace\n"),
            (
                "common/_viewports.yaml",
                "- label: [\n",
                "- label: Desktop\n  width: 1280\n  height: 720\n",
            ),
            ("common/data.json", "{", "{\"items\": []}\n"),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let workspace = Workspace::open(directory.path()).unwrap();
            let path = directory.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, invalid).unwrap();
            let entry = workspace.entries().unwrap().into_iter().next().unwrap();
            let mut open = workspace.open_document(entry).unwrap();
            assert!(open.document.validate().is_err());
            assert!(workspace.save(&mut open).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
            if open.entry.kind == DocumentKind::Suite {
                assert!(workspace.suite_catalog().unwrap()[0].error.is_some());
            }

            open.document.set_source(corrected.into()).unwrap();
            open.document.validate().unwrap();
            validate_typed_source(&open.document).unwrap();

            let external = format!("{corrected}\n");
            fs::write(&path, &external).unwrap();
            assert!(
                workspace
                    .save(&mut open)
                    .unwrap_err()
                    .to_string()
                    .contains("changed outside")
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), external);
            assert_eq!(open.document.source(), corrected);
            assert_eq!(open.document.saved_source(), invalid);
            assert!(open.document.dirty());

            fs::write(&path, invalid).unwrap();
            workspace.save(&mut open).unwrap();

            assert_eq!(fs::read_to_string(path).unwrap(), corrected);
            assert_eq!(open.document.saved_source(), corrected);
            assert!(!open.document.dirty());
            if open.entry.kind == DocumentKind::Suite {
                let catalog = workspace.suite_catalog().unwrap();
                assert!(catalog[0].error.is_none());
                assert_eq!(catalog[0].scenarios.len(), 1);
            }
        }
    }

    #[test]
    fn suite_display_name_does_not_change_file_or_output_identity() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let mut open = workspace.create_suite("checkout").unwrap();
        let original_path = open.entry.path.clone();
        open.document
            .apply(Edit::Set {
                parent: "".into(),
                key: "displayName".into(),
                value: Some(json!("Customer checkout")),
            })
            .unwrap();
        workspace.save(&mut open).unwrap();
        workspace.reload(&mut open).unwrap();
        assert_eq!(open.entry.path, original_path);
        assert_eq!(
            open.document.value().unwrap()["displayName"],
            "Customer checkout"
        );
        let catalog = workspace.suite_catalog().unwrap();
        assert_eq!(catalog[0].name, "checkout");
        assert_eq!(catalog[0].label(), "Customer checkout");
        assert_eq!(catalog[0].output_id, "checkout");
        assert!(catalog[0].error.is_none());
        for display_name in [None, Some(json!("  "))] {
            open.document
                .apply(Edit::Set {
                    parent: "".into(),
                    key: "displayName".into(),
                    value: display_name,
                })
                .unwrap();
            workspace.save(&mut open).unwrap();
            assert_eq!(workspace.suite_catalog().unwrap()[0].label(), "checkout");
        }
    }

    #[test]
    fn suite_copy_rename_delete_preserve_data_and_avoid_output_id_collisions() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let mut original = workspace.create_suite("original").unwrap();
        original
            .document
            .apply(Edit::Set {
                parent: "".into(),
                key: "id".into(),
                value: Some(json!("original-id")),
            })
            .unwrap();
        workspace.save(&mut original).unwrap();
        let mut copy = workspace.duplicate_suite(&original, "copy").unwrap();
        assert!(copy.document.value().unwrap().get("id").is_none());
        workspace.rename_suite(&mut copy, "renamed").unwrap();
        assert!(
            !workspace
                .resolve(Path::new("visual_tests/copy.tests.yaml"))
                .unwrap()
                .exists()
        );
        assert!(workspace.rename_suite(&mut copy, "original").is_err());
        workspace.delete_suite(&copy).unwrap();
        assert_eq!(workspace.entries().unwrap().len(), 1);
    }

    #[test]
    fn suite_artifact_mutations_preserve_history_and_other_suites_across_outputs() {
        let directory = tempfile::tempdir().unwrap();
        let custom = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let mut open = workspace.create_suite("shop").unwrap();
        let roots = [
            directory.path().join(".regressify"),
            directory.path().join(".regressify__staging"),
            custom.path().to_path_buf(),
        ];
        for root in &roots {
            for id in ["shop", "other"] {
                artifact_fixture(root, id);
            }
            fs::create_dir_all(root.join("states")).unwrap();
            fs::write(root.join("states/auth.json"), b"private state").unwrap();
            crate::snapshot::generate_site(
                root,
                &["shop".into(), "other".into()],
                &crate::cli::args::SnapshotArgs {
                    test_suite: vec![],
                    title: Some("Review".into()),
                    heading: None,
                    subheading: None,
                },
                &crate::config::WorkspaceConfig::default(),
            )
            .unwrap();
        }
        let pixel = fs::read(roots[0].join("shop/bitmaps_reference/home.png")).unwrap();
        let other = fs::read(roots[0].join("_snapshot/data/suites/other.json")).unwrap();
        workspace
            .rename_suite_in_outputs(
                &mut open,
                "checkout",
                Some("Customer checkout"),
                &[custom.path().into()],
            )
            .unwrap();
        assert_eq!(
            open.document.value().unwrap()["displayName"],
            "Customer checkout"
        );
        assert!(!open.document.dirty());
        for root in &roots {
            assert!(!root.join("shop").exists());
            assert_eq!(
                fs::read(root.join("checkout/bitmaps_reference/home.png")).unwrap(),
                pixel
            );
            for run in ["old", "current"] {
                let report: serde_json::Value = serde_json::from_slice(
                    &fs::read(root.join(format!("checkout/bitmaps_test/{run}/report.json")))
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(report["id"], "checkout");
                assert_eq!(report["testSuite"], "checkout");
                assert_eq!(report["displayName"], "Customer checkout");
                assert_eq!(
                    report["tests"][0]["pair"]["url"],
                    "https://example.com/shop"
                );
            }
            crate::snapshot::artifact_paths(root, &["checkout".into(), "other".into()]).unwrap();
            assert_eq!(
                fs::read(root.join("_snapshot/data/suites/other.json")).unwrap(),
                other
            );
            assert!(!root.join("_snapshot/images/shop").exists());
        }
        open.document
            .apply(Edit::Set {
                parent: "".into(),
                key: "displayName".into(),
                value: Some(json!("Updated label")),
            })
            .unwrap();
        workspace
            .save_in_outputs(&mut open, &[custom.path().into()])
            .unwrap();
        for root in &roots {
            let report: serde_json::Value = serde_json::from_slice(
                &fs::read(root.join("_snapshot/data/suites/checkout.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(report["displayName"], "Updated label");
            assert_eq!(report["id"], "checkout");
        }
        workspace
            .delete_suite_in_outputs(&open, &[custom.path().into()])
            .unwrap();
        assert!(!workspace.resolve(&open.entry.path).unwrap().exists());
        for root in &roots {
            assert!(!root.join("checkout").exists());
            assert!(!root.join("_snapshot/images/checkout").exists());
            assert!(root.join("other/bitmaps_test/old/report.json").exists());
            assert_eq!(
                fs::read(root.join("states/auth.json")).unwrap(),
                b"private state"
            );
            crate::snapshot::artifact_paths(root, &["other".into()]).unwrap();
        }
    }

    fn artifact_fixture(root: &Path, id: &str) {
        let suite = root.join(id);
        fs::create_dir_all(suite.join("bitmaps_reference")).unwrap();
        fs::create_dir_all(suite.join("html_report")).unwrap();
        let png = lodepng::encode32(&[10_u8, 20, 30, 255], 1, 1).unwrap();
        fs::write(suite.join("bitmaps_reference/home.png"), &png).unwrap();
        for run in ["old", "current"] {
            let folder = suite.join("bitmaps_test").join(run);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("home.png"), &png).unwrap();
            let report = json!({"id": id, "testSuite": id, "tests": [{"status": "pass", "pair": {
                "fileName": "home.png", "label": "Home", "reference": "../bitmaps_reference/home.png",
                "test": format!("../bitmaps_test/{run}/home.png"), "url": "https://example.com/shop"
            }}]});
            fs::write(folder.join("report.json"), report.to_string()).unwrap();
            fs::write(
                suite.join("html_report/config.js"),
                format!("report({report});"),
            )
            .unwrap();
        }
    }

    #[test]
    fn saving_data_as_the_profile_file_validates_it_as_a_profile_file() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let path = Path::new("common/_replacement-profiles.yaml");
        let error = workspace
            .create_document(path, DocumentKind::Data, "profiles:\n  qa:\n    - ref: a\n")
            .unwrap_err();
        assert!(format!("{error:#}").contains("test"), "{error:#}");
        assert!(!directory.path().join(path).exists());

        let open = workspace
            .create_document(
                path,
                DocumentKind::Data,
                "profiles:\n  qa:\n    - ref: a\n      test: b\n",
            )
            .unwrap();
        assert_eq!(open.entry.kind, DocumentKind::ReplacementProfiles);
    }

    #[test]
    fn suite_rename_keeps_explicit_ids_and_format_and_rejects_ambiguous_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let mut open = workspace
            .create_document(
                Path::new("visual_tests/shop.tests.json"),
                DocumentKind::Suite,
                r#"{"id":"stable","scenarios":[]}"#,
            )
            .unwrap();
        let output = directory.path().join(".regressify");
        artifact_fixture(&output, "stable");
        assert!(workspace.rename_suite(&mut open, "new").is_err());
        workspace
            .rename_suite_in_outputs(&mut open, "new.tests.json", Some("Storefront"), &[])
            .unwrap();
        assert!(output.join("stable/bitmaps_test/old/report.json").exists());
        assert!(!output.join("new").exists());
        let other = workspace
            .create_document(
                Path::new("visual_tests/other.tests.yaml"),
                DocumentKind::Suite,
                "id: stable\nscenarios: []\n",
            )
            .unwrap();
        assert!(workspace.delete_suite(&open).is_err());
        assert!(
            workspace
                .rename_suite(&mut open, "next.tests.json")
                .is_err()
        );
        assert!(workspace.resolve(&other.entry.path).unwrap().exists());
        assert!(output.join("stable").exists());
    }

    #[test]
    fn suite_mutation_failures_preserve_files_reports_and_drafts() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let mut open = workspace.create_suite("shop").unwrap();
        let output = directory.path().join(".regressify");
        artifact_fixture(&output, "shop");
        fs::create_dir_all(output.join("destination")).unwrap();
        assert!(workspace.rename_suite(&mut open, "destination").is_err());
        assert!(workspace.resolve(&open.entry.path).unwrap().exists());
        let report = output.join("shop/bitmaps_test/old/report.json");
        fs::write(&report, b"invalid").unwrap();
        assert!(workspace.rename_suite(&mut open, "renamed").is_err());
        assert!(!output.join("renamed").exists());
        assert_eq!(fs::read(&report).unwrap(), b"invalid");
        open.document
            .apply(Edit::Set {
                parent: "".into(),
                key: "displayName".into(),
                value: Some(json!("Draft label")),
            })
            .unwrap();
        assert!(workspace.save(&mut open).is_err());
        assert!(open.document.dirty());
        assert!(
            workspace
                .open_document(open.entry.clone())
                .unwrap()
                .document
                .value()
                .unwrap()
                .get("displayName")
                .is_none()
        );
    }

    #[test]
    fn suite_mutations_reject_reserved_output_ids_case_insensitively() {
        for id in [
            "states",
            "STATES",
            "_snapshot",
            "_Snapshot",
            ".SUITE-UPDATE-1",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let workspace = Workspace::open(directory.path()).unwrap();
            let source = serde_json::to_string(&json!({"id": id, "scenarios": []})).unwrap();
            let mut open = workspace
                .create_document(
                    Path::new("visual_tests/shop.tests.json"),
                    DocumentKind::Suite,
                    &source,
                )
                .unwrap();
            let output = directory.path().join(".regressify").join(id);
            fs::create_dir_all(&output).unwrap();
            fs::write(output.join("sentinel"), b"keep").unwrap();
            assert!(workspace.delete_suite(&open).is_err(), "{id}");
            assert!(
                workspace.rename_suite(&mut open, "new.tests.json").is_err(),
                "{id}"
            );
            assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
            assert_eq!(
                workspace
                    .open_document(open.entry.clone())
                    .unwrap()
                    .document
                    .source(),
                source
            );
        }
    }

    #[test]
    fn paths_are_portable_and_workspace_scoped() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        for path in [
            "../escape",
            "C:\\outside",
            "CON.yaml",
            "common/file:stream",
            "file.",
            "file ",
            "",
            "visual_tests/../../outside",
        ] {
            assert!(workspace.resolve(Path::new(path)).is_err(), "{path}");
        }
        for name in [
            "../escape",
            "CON",
            "all",
            ".checkout",
            ".checkout.tests.yaml",
            "_snapshot",
            "_diagnostics",
            "_landing",
            "states",
            "a/b",
            "*.tests.yaml",
        ] {
            assert!(suite_path(name).is_err(), "{name}");
        }
        assert!(
            workspace
                .resolve(Path::new("common/my settings.yaml"))
                .is_ok()
        );
        let suite = workspace.create_suite("checkout").unwrap();
        for invalid_save_as in [
            "visual_tests/.checkout.tests.yaml",
            "visual_tests/all.tests.yaml",
            "visual_tests/_snapshot.tests.yaml",
            "common/not-a-suite.yaml",
        ] {
            assert!(
                workspace
                    .save_as(&suite, Path::new(invalid_save_as))
                    .is_err(),
                "{invalid_save_as}"
            );
            assert!(!directory.path().join(invalid_save_as).exists());
        }
        let dot_data = workspace
            .create_document(
                Path::new("common/.helper.js"),
                DocumentKind::Data,
                "// helper\n",
            )
            .unwrap();
        assert!(
            workspace
                .save_as(&dot_data, Path::new("common/.helper-copy.js"))
                .is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escapes_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        std::os::unix::fs::symlink(external.path(), directory.path().join("outside")).unwrap();
        assert!(workspace.resolve(Path::new("outside/file.yaml")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn save_as_validates_case_aliased_suite_destinations() {
        for existing_directory in [false, true] {
            for folder in ["visual_tests", "VISUAL_TESTS", "Visual_Tests"] {
                let directory = tempfile::tempdir().unwrap();
                let workspace = Workspace::open(directory.path()).unwrap();
                if existing_directory {
                    fs::create_dir(directory.path().join("visual_tests")).unwrap();
                }
                let source = "scenarios:\n  - url: https://example.com\n";
                let template = workspace
                    .create_document(
                        Path::new("common/template.yaml"),
                        DocumentKind::Data,
                        source,
                    )
                    .unwrap();
                for name in [
                    ".checkout",
                    "all",
                    "_snapshot",
                    "_diagnostics",
                    "_landing",
                    "states",
                ] {
                    let relative = Path::new(folder)
                        .join("nested")
                        .join(format!("{name}.tests.yaml"));
                    assert!(
                        workspace.save_as(&template, &relative).is_err(),
                        "{relative:?}, existing_directory={existing_directory}"
                    );
                    assert!(
                        workspace
                            .create_document(&relative, DocumentKind::Data, source)
                            .is_err()
                    );
                    assert!(!directory.path().join(&relative).exists());
                }
                assert!(!directory.path().join(folder).join("nested").exists());
                let relative = Path::new(folder).join("nested/checkout.tests.yaml");
                let saved = workspace.save_as(&template, &relative).unwrap();
                assert_eq!(saved.entry.kind, DocumentKind::Suite);
                assert_eq!(
                    workspace.classify_entry(&relative).unwrap().kind,
                    DocumentKind::Suite
                );
                assert_eq!(workspace.runnable_suite(&saved.entry).unwrap(), "checkout");
                assert_eq!(saved.document.source(), source);
                assert!(workspace.save_as(&template, &relative).is_err());
                assert_eq!(
                    fs::read_to_string(workspace.resolve(&relative).unwrap()).unwrap(),
                    source
                );
                assert_eq!(template.document.source(), source);
            }
        }
    }

    #[test]
    fn save_as_applies_suite_validation_to_data_templates() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let template = workspace
            .create_document(
                Path::new("common/template.yaml"),
                DocumentKind::Data,
                "unknownSuiteField: true\n",
            )
            .unwrap();
        let relative = Path::new("visual_tests/checkout.tests.yaml");
        assert!(workspace.save_as(&template, relative).is_err());
        assert!(!directory.path().join(relative).exists());
        assert_eq!(template.document.source(), "unknownSuiteField: true\n");
    }

    #[cfg(unix)]
    #[test]
    fn save_as_validates_symlinked_suite_destinations() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        fs::create_dir(directory.path().join("visual_tests")).unwrap();
        std::os::unix::fs::symlink(
            directory.path().join("visual_tests"),
            directory.path().join("suite-alias"),
        )
        .unwrap();
        let source = "scenarios:\n  - url: https://example.com\n";
        let template = workspace
            .create_document(
                Path::new("common/template.yaml"),
                DocumentKind::Data,
                source,
            )
            .unwrap();
        let relative = Path::new("suite-alias/.checkout.tests.yaml");
        assert!(workspace.save_as(&template, relative).is_err());
        assert!(!directory.path().join(relative).exists());
        let relative = Path::new("suite-alias/checkout.tests.yaml");
        let saved = workspace.save_as(&template, relative).unwrap();
        assert_eq!(saved.entry.kind, DocumentKind::Suite);
        assert_eq!(
            workspace.classify_entry(relative).unwrap().kind,
            DocumentKind::Suite
        );
        assert_eq!(workspace.runnable_suite(&saved.entry).unwrap(), "checkout");
    }

    #[test]
    fn concurrent_preferences_saves_preserve_recent_workspaces_from_both_windows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let roots = [
            directory.path().join("first"),
            directory.path().join("second"),
        ];
        let saves = roots
            .iter()
            .map(|root| {
                let mut preferences = Preferences::default();
                preferences.remember(&Workspace { root: root.clone() });
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    preferences.save(&path).unwrap();
                    preferences
                })
            })
            .collect::<Vec<_>>();
        let mut snapshots = saves
            .into_iter()
            .map(|save| save.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(Preferences::load(&path).unwrap().recent_workspaces.len(), 2);
        snapshots[0].source_font_size = 18;
        snapshots[0].save(&path).unwrap();
        let saved = Preferences::load(&path).unwrap();
        assert_eq!(saved.source_font_size, 18);
        assert!(
            roots
                .iter()
                .all(|root| saved.recent_workspaces.contains(root))
        );
    }

    #[test]
    fn preferences_round_trip_and_bound_recent_workspaces() {
        let directory = tempfile::tempdir().unwrap();
        let mut preferences = Preferences::default();
        for index in 0..15 {
            preferences.remember(&Workspace {
                root: PathBuf::from(format!("workspace-{index}")),
            });
        }
        let workspace = Workspace {
            root: PathBuf::from("workspace-10"),
        };
        preferences.remember(&workspace);
        assert_eq!(preferences.recent_workspaces.len(), 10);
        assert_eq!(preferences.recent_workspaces[0], workspace.root);
        let path = directory.path().join("settings.json");
        preferences.navigator_hidden = true;
        preferences.save(&path).unwrap();
        assert!(Preferences::load(&path).unwrap().navigator_hidden);
        let legacy: Preferences = serde_json::from_str(r#"{"darkMode":true}"#).unwrap();
        assert!(legacy.dark_mode);
        assert!(!legacy.navigator_hidden);
        assert_eq!(
            Preferences::load(&path).unwrap().recent_workspaces,
            preferences.recent_workspaces
        );
        fs::write(&path, "invalid").unwrap();
        assert!(Preferences::load(&path).is_err());
    }

    #[test]
    fn picked_files_use_the_correct_base_directory() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        fs::create_dir_all(directory.path().join("visual_tests/uploads")).unwrap();
        let upload = directory.path().join("visual_tests/uploads/sample.txt");
        fs::write(&upload, "sample").unwrap();
        assert_eq!(
            workspace.relative_file(&upload, true).unwrap(),
            "uploads/sample.txt"
        );
        assert_eq!(
            workspace.relative_file(&upload, false).unwrap(),
            "visual_tests/uploads/sample.txt"
        );
        let other = outside.path().join("other.txt");
        fs::write(&other, "outside").unwrap();
        assert!(workspace.relative_file(&other, false).is_err());
        let root_file = directory.path().join("root.txt");
        fs::write(&root_file, "root").unwrap();
        assert!(workspace.relative_file(&root_file, true).is_err());
    }

    #[test]
    fn selected_suite_must_match_cli_format_precedence() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let json = workspace.create_suite("Shop.TESTS.JSON").unwrap();
        assert_eq!(json.document.format, Format::Json);
        assert_eq!(workspace.runnable_suite(&json.entry).unwrap(), "shop");
        let yaml = workspace.create_suite("shop.tests.yaml").unwrap();
        assert!(
            workspace
                .runnable_suite(&json.entry)
                .unwrap_err()
                .to_string()
                .contains("not the selected")
        );
        assert_eq!(workspace.runnable_suite(&yaml.entry).unwrap(), "shop");
        assert_eq!(workspace.entries().unwrap().len(), 2);
        assert!(suite_path("ALL").is_err());
        assert!(suite_path("All.TESTS.JSON").is_err());
        assert!(suite_path("all.tests.yaml").is_err());
    }

    #[test]
    fn url_only_scenario_names_are_suite_scoped_and_keep_cli_ids() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        workspace.create_suite("shop").unwrap();
        let path = directory.path().join("visual_tests/shop.tests.yaml");
        let source = "scenarios:\n  - url: https://example.test/men/running-shoes\n  - url: https://example.test/women/running-shoes\n  - url: https://example.test/help-center\n  - label: Checkout flow\n    url: https://example.test/checkout\n";
        fs::write(&path, source).unwrap();
        let suites = workspace.suite_catalog().unwrap();
        assert!(suites[0].error.is_none());
        assert_eq!(
            suites[0]
                .scenarios
                .iter()
                .map(|scenario| scenario.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Men / Running shoes",
                "Women / Running shoes",
                "Help center",
                "Checkout flow"
            ]
        );
        for scenario in &suites[0].scenarios[..3] {
            assert_eq!(
                scenario.id,
                crate::snapshot::get_asset_version(scenario.url.as_bytes(), 20)
            );
        }
        assert_eq!(suites[0].scenarios[3].id, "checkout-flow");
        assert_eq!(fs::read_to_string(path).unwrap(), source);
    }

    #[test]
    fn multiple_suite_selection_uses_exact_names_and_output_ids() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        workspace.create_suite("shop").unwrap();
        let mut account = workspace.create_suite("account").unwrap();
        account
            .document
            .apply(Edit::Set {
                parent: String::new(),
                key: "id".into(),
                value: Some(json!("account-output")),
            })
            .unwrap();
        workspace.save(&mut account).unwrap();
        let suites = workspace.suite_catalog().unwrap();
        let mut selected = SuiteSelection::default();
        assert!(selected.filters(&suites, false).is_err());
        selected.select_all(&suites);
        assert_eq!(selected.len(), 2);
        assert_eq!(
            selected.filters(&suites, false).unwrap(),
            vec!["account", "shop"]
        );
        assert_eq!(
            selected.filters(&suites, true).unwrap(),
            vec!["account-output", "shop"]
        );
        selected.toggle(&suites[0], false).unwrap();
        assert_eq!(selected.filters(&suites, false).unwrap(), vec!["shop"]);
        selected.reconcile(&[]);
        assert!(selected.is_empty());
    }

    #[test]
    fn scenario_selection_transitions_never_expand_an_empty_subset() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        workspace.create_suite("shop").unwrap();
        let mut suites = workspace.suite_catalog().unwrap();
        suites[0].scenarios[0].id = "home".into();
        suites[0].scenarios.push(ScenarioSummary {
            id: "checkout".into(),
            label: "Checkout".into(),
            url: "about:blank".into(),
        });
        suites[0].output_id = "shop-output".into();
        let mut selected = SuiteSelection::default();
        selected
            .toggle_scenario(&suites[0], "checkout", true)
            .unwrap();
        assert!(selected.contains("shop"));
        assert!(!selected.all_selected(&suites[0]));
        assert_eq!(selected.scenario_count(&suites[0]), 1);
        assert_eq!(
            selected.scenario_filters(&suites, true).unwrap(),
            vec!["shop-output:checkout".parse().unwrap()]
        );
        selected.toggle(&suites[0], true).unwrap();
        assert!(selected.all_selected(&suites[0]));
        assert!(
            selected
                .scenario_filters(&suites, false)
                .unwrap()
                .is_empty()
        );
        selected.toggle_scenario(&suites[0], "home", false).unwrap();
        assert_eq!(selected.scenario_count(&suites[0]), 1);
        selected
            .toggle_scenario(&suites[0], "checkout", false)
            .unwrap();
        assert!(selected.is_empty());
        assert!(selected.scenario_filters(&suites, false).is_err());
        assert!(
            selected
                .toggle_scenario(&suites[0], "missing", true)
                .is_err()
        );
    }

    #[test]
    fn scenario_selection_survives_new_scenarios_without_widening() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        workspace.create_suite("shop").unwrap();
        let mut suites = workspace.suite_catalog().unwrap();
        let id = suites[0].scenarios[0].id.clone();
        let mut selected = SuiteSelection::default();
        selected.toggle_scenario(&suites[0], &id, true).unwrap();
        assert!(selected.all_selected(&suites[0]));
        suites[0].scenarios.push(ScenarioSummary {
            id: "new".into(),
            label: "New".into(),
            url: "about:blank".into(),
        });
        selected.reconcile(&suites);
        assert_eq!(selected.scenario_count(&suites[0]), 1);
        assert!(!selected.scenario_selected("shop", "new"));
        suites[0].scenarios.remove(0);
        assert!(selected.scenario_filters(&suites, false).is_err());
        selected.reconcile(&suites);
        assert!(selected.is_empty());
        assert!(!selected.has_scenario_selection());
    }

    #[test]
    fn invalid_suites_are_visible_but_not_silently_selected() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let bad = workspace.create_suite("bad").unwrap();
        workspace.create_suite("good").unwrap();
        fs::write(workspace.resolve(&bad.entry.path).unwrap(), "scenarios: [").unwrap();
        let suites = workspace.suite_catalog().unwrap();
        assert_eq!(suites.len(), 2);
        assert!(suites[0].error.is_some());
        let mut selected = SuiteSelection::default();
        selected.select_all(&suites);
        assert_eq!(selected.len(), 1);
        assert!(selected.toggle(&suites[0], true).is_err());
        assert_eq!(selected.filters(&suites, false).unwrap(), vec!["good"]);
    }
}
