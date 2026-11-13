//! The suites the selected deployment target's site serves, for the suite list and the Deploy
//! dialog. The site is read over the network only when deploying is in view (opening the
//! dialog, changing target, or after a deployment); otherwise the local copy of the last
//! published state is shown, so the suite list never waits on the network. Suites this
//! workspace published are compared with their recorded inputs off the UI thread, after each
//! site read and artifact scan, so statuses match what a deployment would upload.

use super::*;
use crate::desktop::deployment::{
    DeployTarget,
    preview::SiteSuites,
    site_state::{self, RemoteState, SiteState},
};

/// Where the shown site suites came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::desktop::ui) enum SiteSource {
    #[default]
    Unknown,
    Loading,
    /// Read from the site itself.
    Site,
    /// The last state this workspace published; the site was not read or not readable.
    Local,
    /// Neither the site nor a local copy could be read.
    Unavailable(String),
}

#[derive(Default)]
pub(super) struct SiteScan {
    generation: u64,
    /// Counts local result changes (finished artifact scans), so comparisons started before
    /// one can be told apart from those started after.
    revision: u64,
    /// Read the site again from saved settings when the next artifact scan finishes.
    pub(super) reload_pending: bool,
    target: Option<DeployTarget>,
    suites: Option<SiteSuites>,
    source: SiteSource,
    read_at: Option<String>,
    /// The published state the suites came from, kept to compare recorded inputs again.
    state: Option<SiteState>,
    /// Latest local runs that tests use instead of scanned artifacts.
    #[cfg(test)]
    local_runs: Option<BTreeMap<String, String>>,
}

/// When a site read or a comparison of local results with the site started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Started {
    generation: u64,
    revision: u64,
}

impl SiteScan {
    fn started(&self) -> Started {
        Started {
            generation: self.generation,
            revision: self.revision,
        }
    }

    /// Local results changed; returns when the comparison they call for starts.
    fn local_results_changed(&mut self) -> Started {
        self.revision += 1;
        self.started()
    }

    /// Shows a comparison started at `started`, unless a newer site read or local change
    /// replaced it: comparisons run in the background and may finish out of order, and an
    /// earlier one may describe results from before an approval or another output folder.
    /// Returns whether what is shown changed.
    fn finish_verification(&mut self, started: Started, verified: BTreeMap<String, bool>) -> bool {
        if started != self.started() {
            return false;
        }
        match &mut self.suites {
            Some(suites) if suites.verified != verified => {
                suites.verified = verified;
                true
            }
            _ => false,
        }
    }

    /// Shows a site read started at `started`, unless a newer read replaced it (`None`).
    /// Returns whether local results changed while the site was read, so the comparison it
    /// made must run again.
    fn finish_read(
        &mut self,
        started: Started,
        suites: Option<SiteSuites>,
        state: Option<SiteState>,
        source: SiteSource,
    ) -> Option<bool> {
        if self.generation != started.generation {
            return None;
        }
        if suites.is_some() || !matches!(source, SiteSource::Unknown) {
            self.suites = suites;
            self.state = state;
        }
        if source == SiteSource::Site {
            self.read_at = Some(chrono::Local::now().format("%H:%M").to_string());
        }
        self.source = source;
        Some(self.revision != started.revision)
    }
}

impl RunPanel {
    /// After a deployment, the saved target (with its new URL) is read again, then its site.
    pub(in crate::desktop::ui) fn reload_site_after_deployment(&mut self, cx: &mut Context<Self>) {
        self.site.reload_pending = true;
        self.refresh_artifacts(cx);
    }

    /// Forgets the site of a closed workspace. The new generation makes reads still in flight
    /// for it drop their results instead of restoring its suites.
    pub(super) fn reset_site(&mut self) {
        self.site = SiteScan {
            generation: self.site.generation + 1,
            ..SiteScan::default()
        };
    }

    pub(in crate::desktop::ui) fn last_command(&self) -> Option<RunCommand> {
        self.manager.request.as_ref().map(|request| request.command)
    }

    pub(in crate::desktop::ui) fn site_target(&self) -> Option<&DeployTarget> {
        self.site.target.as_ref()
    }

    pub(in crate::desktop::ui) fn site_suites(&self) -> Option<&SiteSuites> {
        self.site.suites.as_ref()
    }

    pub(in crate::desktop::ui) fn site_source(&self) -> &SiteSource {
        &self.site.source
    }

    pub(in crate::desktop::ui) fn site_read_at(&self) -> Option<&str> {
        self.site.read_at.as_deref()
    }

    /// The latest completed full-suite run of each suite, which deployments publish.
    pub(in crate::desktop::ui) fn latest_run_ids(&self) -> BTreeMap<String, String> {
        #[cfg(test)]
        if let Some(runs) = &self.site.local_runs {
            return runs.clone();
        }
        self.artifacts.latest_run_ids()
    }

    fn latest_run_id(&self, output_id: &str) -> Option<String> {
        #[cfg(test)]
        if let Some(runs) = &self.site.local_runs {
            return runs.get(output_id).cloned();
        }
        self.artifacts.latest_run_id(output_id)
    }

    #[cfg(test)]
    pub(in crate::desktop::ui) fn set_site_for_test(
        &mut self,
        target: Option<DeployTarget>,
        suites: Option<SiteSuites>,
        source: SiteSource,
        local_runs: BTreeMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        self.site.generation += 1;
        self.site.target = target;
        self.site.suites = suites;
        self.site.source = source;
        self.site.read_at = Some("14:02".into());
        self.site.local_runs = Some(local_runs);
        self.site.state = None;
        cx.notify();
    }

    /// `None` when the suite is not on the site; otherwise whether its local results are newer.
    pub(in crate::desktop::ui) fn site_status(&self, output_id: &str) -> Option<bool> {
        let suites = self.site.suites.as_ref()?;
        // One lookup per suite row; the list renders often with many suites.
        suites
            .suites
            .contains_key(output_id)
            .then(|| suites.outdated(output_id, self.latest_run_id(output_id).as_deref()))
    }

    /// Compares the shown site's suites with their recorded inputs again, after local results
    /// changed (a run, approval, or cleanup). Nothing is read from the network.
    pub(super) fn verify_site(&mut self, cx: &mut Context<Self>) {
        // Counted even without a state to compare, so a site read in flight compares again.
        let started = self.site.local_results_changed();
        let (Some(target), Some(state)) = (self.site.target.clone(), self.site.state.clone())
        else {
            return;
        };
        let settings = self.deployment_path.clone();
        let output = self.output_root().ok();
        let task = cx.background_executor().spawn(async move {
            verify(settings.as_deref(), output.as_deref(), &target.id, &state)
        });
        cx.spawn(async move |panel, cx| {
            let verified = task.await;
            let _ = panel.update(cx, |panel, cx| {
                if panel.site.finish_verification(started, verified) {
                    cx.emit(RunEvent::ArtifactsChanged);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Reloads the suites of `target`'s site. `network` reads the site itself; otherwise, or
    /// when it cannot be read, the local copy of the last published state is used.
    pub(in crate::desktop::ui) fn refresh_site(
        &mut self,
        target: Option<DeployTarget>,
        network: bool,
        cx: &mut Context<Self>,
    ) {
        self.site.generation += 1;
        let generation = self.site.generation;
        let Some(target) = target else {
            self.site = SiteScan {
                generation,
                ..SiteScan::default()
            };
            cx.emit(RunEvent::ArtifactsChanged);
            cx.notify();
            return;
        };
        // Any target edit (destination, URL, settings) may describe another site.
        let same_target = self.site.target.as_ref() == Some(&target);
        if !same_target {
            self.site.suites = None;
            self.site.read_at = None;
        }
        self.site.target = Some(target.clone());
        if network {
            self.site.source = SiteSource::Loading;
        }
        let started = self.site.started();
        let settings = self.deployment_path.clone();
        let output = self.output_root().ok();
        let task = cx.background_executor().spawn(async move {
            let verified = |state: SiteState| {
                let mut suites = SiteSuites::from_state(&state);
                suites.verified =
                    verify(settings.as_deref(), output.as_deref(), &target.id, &state);
                (Some(suites), Some(state))
            };
            let local = || -> Result<Option<SiteState>, String> {
                let Some(settings) = &settings else {
                    return Ok(None);
                };
                site_state::load_local(settings, &target.id).map_err(|error| format!("{error:#}"))
            };
            // `None` when the site is not read: no URL, or not asked to use the network.
            let remote = match (&target.published_url, network) {
                (Some(url), true) => Some(
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map(|runtime| runtime.block_on(site_state::fetch(url)))
                        .map_err(|error| error.to_string()),
                ),
                _ => None,
            };
            // The rule deployments apply: a site this workspace published to is never assumed
            // empty, since Replace would then remove its suites without confirmation. History
            // that cannot be read counts as published.
            let published_before = || {
                settings.as_deref().is_some_and(|settings| {
                    site_state::published_before(settings, &target.id).unwrap_or(true)
                })
            };
            // The local copy, when the site was not read (`reason` is `None`) or could not be.
            let fallback = |reason: Option<String>| match local() {
                Ok(Some(state)) => (verified(state), SiteSource::Local),
                Ok(None) if target.published_url.is_none() && published_before() => (
                    (None, None),
                    SiteSource::Unavailable(
                        "it was published before, but which suites it serves is unknown here"
                            .into(),
                    ),
                ),
                // Never published here: nothing is on the site yet.
                Ok(None) if target.published_url.is_none() => {
                    ((Some(SiteSuites::default()), None), SiteSource::Local)
                }
                Ok(None) => (
                    (None, None),
                    reason.map_or(SiteSource::Unknown, SiteSource::Unavailable),
                ),
                Err(error) => ((None, None), SiteSource::Unavailable(error)),
            };
            match remote {
                Some(Ok(RemoteState::Found(state))) => (verified(state), SiteSource::Site),
                Some(Ok(RemoteState::Legacy(suites))) => (
                    (
                        Some(SiteSuites::from_ids(
                            suites.into_iter().map(|suite| suite.id),
                        )),
                        None,
                    ),
                    SiteSource::Site,
                ),
                // An access gateway can hide a published report behind an empty page.
                Some(Ok(RemoteState::Empty)) if published_before() => {
                    fallback(Some("it serves no Regressify report".into()))
                }
                Some(Ok(RemoteState::Empty)) => {
                    ((Some(SiteSuites::default()), None), SiteSource::Site)
                }
                Some(Ok(RemoteState::Unreachable(reason) | RemoteState::Invalid(reason)))
                | Some(Err(reason)) => fallback(Some(reason)),
                None => fallback(None),
            }
        });
        cx.spawn(async move |panel, cx| {
            let ((suites, state), source) = task.await;
            let _ = panel.update(cx, |panel, cx| {
                let Some(compare_again) = panel.site.finish_read(started, suites, state, source)
                else {
                    return;
                };
                // Local results changed while the site was read (a run, approval, or another
                // output folder): what it read stands, but its comparison is stale.
                if compare_again {
                    panel.verify_site(cx);
                }
                cx.emit(RunEvent::ArtifactsChanged);
                cx.notify();
            });
        })
        .detach();
        cx.emit(RunEvent::ArtifactsChanged);
        cx.notify();
    }
}

/// Recorded-input comparison of `state`'s suites; empty without settings or an output folder.
fn verify(
    settings: Option<&std::path::Path>,
    output: Option<&std::path::Path>,
    target_id: &str,
    state: &SiteState,
) -> BTreeMap<String, bool> {
    match (settings, output) {
        (Some(settings), Some(output)) => {
            site_state::load_inputs(settings, target_id).verify(state, output)
        }
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    // Not a glob import: the UI prelude's `test` attribute would shadow the built-in one.
    use super::{SiteScan, SiteSource, SiteSuites};
    use std::collections::BTreeMap;

    fn verified(unchanged: bool) -> BTreeMap<String, bool> {
        BTreeMap::from([("one".to_owned(), unchanged)])
    }

    #[test]
    fn comparisons_started_before_a_local_change_never_replace_later_ones() {
        let mut scan = SiteScan {
            suites: Some(SiteSuites::from_ids(["one".to_owned()])),
            ..SiteScan::default()
        };
        // Scans before and after an approval each start a comparison.
        let before = scan.local_results_changed();
        let after = scan.local_results_changed();
        // The later one finishes first; the earlier, slower one must not replace it.
        assert!(scan.finish_verification(after, verified(false)));
        assert!(!scan.finish_verification(before, verified(true)));
        assert_eq!(scan.suites.as_ref().unwrap().verified, verified(false));
    }

    #[test]
    fn site_reads_compare_again_after_local_changes_while_reading() {
        let suites = Some(SiteSuites::from_ids(["one".to_owned()]));
        let mut scan = SiteScan::default();
        let read = scan.started();
        // Without local changes, the comparison the read made stands.
        assert_eq!(
            scan.finish_read(read, suites.clone(), None, SiteSource::Site),
            Some(false)
        );
        let read = scan.started();
        // A scan that finishes while the site is read changes local results after the read
        // compared them: what it read is still shown, and compared again.
        scan.local_results_changed();
        assert_eq!(
            scan.finish_read(read, suites.clone(), None, SiteSource::Site),
            Some(true)
        );
        assert_eq!(scan.suites, suites);
        // A newer read replaces an older one.
        let older = scan.started();
        scan.generation += 1;
        assert_eq!(
            scan.finish_read(older, None, None, SiteSource::Unknown),
            None
        );
    }
}
