//! What a report deployment will change on its site, for the Deploy dialog and the suite list.
//!
//! The preview compares the selection with the suites the site serves. A suite this workspace
//! published is compared as the deployment compares it, by its recorded inputs, so approvals
//! without a new run count as changes. Other suites (published elsewhere, or before records
//! existed) fall back to comparing run IDs.

use std::collections::{BTreeMap, BTreeSet};

use super::{DeploymentMode, site_state::SiteState};

/// Suites a site serves, with the run each was published from (unknown for sites published
/// before site states existed).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SiteSuites {
    pub suites: BTreeMap<String, Option<String>>,
    /// Suites compared by their recorded inputs: `true` when unchanged locally since this
    /// workspace published them. See `SuiteInputs::verify`.
    pub verified: BTreeMap<String, bool>,
}

impl SiteSuites {
    pub fn from_state(state: &SiteState) -> Self {
        Self {
            suites: state
                .suites
                .iter()
                .map(|(id, suite)| (id.clone(), Some(suite.run_id.clone())))
                .collect(),
            verified: BTreeMap::new(),
        }
    }

    pub fn from_ids(ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            suites: ids.into_iter().map(|id| (id, None)).collect(),
            verified: BTreeMap::new(),
        }
    }

    /// Whether `id` is on the site and changed locally since: by its recorded inputs when
    /// known, otherwise when its latest local run differs from the published one.
    pub fn outdated(&self, id: &str, local_run: Option<&str>) -> bool {
        if self.suites.contains_key(id)
            && let Some(unchanged) = self.verified.get(id)
        {
            return !unchanged;
        }
        match (self.suites.get(id), local_run) {
            (Some(Some(published)), Some(local)) => published != local,
            // Published without a known run: only a local run can tell, and it may be newer.
            (Some(None), Some(_)) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Preview {
    /// Selected and not on the site yet.
    pub add: Vec<String>,
    /// Selected, on the site, and published again: changed locally since it was published
    /// (see `SiteSuites::outdated`), or any such suite when replacing, which rebuilds them.
    pub update: Vec<String>,
    /// Selected, on the site, and unchanged locally, so an update keeps it as it is.
    pub skip: Vec<String>,
    /// Not selected and staying on the site.
    pub kept: Vec<String>,
    /// Leaving the site.
    pub remove: Vec<String>,
    /// On the site but no longer in the workspace.
    pub orphans: Vec<String>,
}

impl Preview {
    /// Suites whose published content is expected to change.
    pub fn changes(&self) -> usize {
        self.add.len() + self.update.len() + self.remove.len()
    }
}

/// What deploying `selected` and removing `remove` changes on `site`. Counts describe what
/// changed; providers that upload whole sites (Azure) re-upload skipped suites without
/// regenerating them, which the dialog states separately.
pub(crate) fn preview(
    mode: DeploymentMode,
    selected: &[String],
    remove: &[String],
    site: Option<&SiteSuites>,
    local_runs: &BTreeMap<String, String>,
    catalog: &[String],
) -> Preview {
    let empty = SiteSuites::default();
    let site = site.unwrap_or(&empty);
    let selected_set = selected.iter().collect::<BTreeSet<_>>();
    let remove_set = remove.iter().collect::<BTreeSet<_>>();
    let catalog = catalog.iter().collect::<BTreeSet<_>>();
    let mut preview = Preview::default();
    for id in selected {
        match site.suites.get(id) {
            None => preview.add.push(id.clone()),
            Some(_) if site.outdated(id, local_runs.get(id).map(String::as_str)) => {
                preview.update.push(id.clone());
            }
            // No local run or record to compare: the deployment will find out.
            Some(_) if !local_runs.contains_key(id) && !site.verified.contains_key(id) => {
                preview.update.push(id.clone())
            }
            // Replacing rebuilds every selected suite, unchanged or not.
            Some(_) if mode == DeploymentMode::Replace => preview.update.push(id.clone()),
            Some(_) => preview.skip.push(id.clone()),
        }
    }
    for id in site.suites.keys() {
        if selected_set.contains(id) {
            continue;
        }
        let leaving = match mode {
            DeploymentMode::Replace => true,
            DeploymentMode::Update => remove_set.contains(id),
        };
        if leaving {
            preview.remove.push(id.clone());
        } else {
            preview.kept.push(id.clone());
        }
        if !catalog.contains(id) {
            preview.orphans.push(id.clone());
        }
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn previews_count_changes_for_update_and_replace() {
        let site = SiteSuites {
            suites: BTreeMap::from([
                ("current".into(), Some("run-1".into())),
                ("outdated".into(), Some("run-1".into())),
                ("other".into(), Some("run-1".into())),
                ("deleted".into(), Some("run-1".into())),
                ("legacy".into(), None),
            ]),
            ..SiteSuites::default()
        };
        let local = BTreeMap::from([
            ("current".to_owned(), "run-1".to_owned()),
            ("outdated".to_owned(), "run-2".to_owned()),
            ("other".to_owned(), "run-3".to_owned()),
            ("legacy".to_owned(), "run-1".to_owned()),
            ("new".to_owned(), "run-1".to_owned()),
        ]);
        let catalog = ids(&["current", "outdated", "other", "legacy", "new"]);
        let selected = ids(&["current", "outdated", "new", "legacy"]);

        let update = preview(
            DeploymentMode::Update,
            &selected,
            &[],
            Some(&site),
            &local,
            &catalog,
        );
        assert_eq!(update.add, ids(&["new"]));
        assert_eq!(update.update, ids(&["outdated", "legacy"]));
        assert_eq!(update.skip, ids(&["current"]));
        assert_eq!(update.kept, ids(&["deleted", "other"]));
        assert!(update.remove.is_empty());
        assert_eq!(update.orphans, ids(&["deleted"]));
        assert_eq!(update.changes(), 3);

        let removal = preview(
            DeploymentMode::Update,
            &[],
            &ids(&["deleted"]),
            Some(&site),
            &local,
            &catalog,
        );
        assert_eq!(removal.remove, ids(&["deleted"]));
        assert_eq!(removal.kept.len(), 4);
        assert_eq!(removal.changes(), 1);

        let replace = preview(
            DeploymentMode::Replace,
            &selected,
            &[],
            Some(&site),
            &local,
            &catalog,
        );
        assert_eq!(replace.remove, ids(&["deleted", "other"]));
        assert!(replace.kept.is_empty());
        // Replacing rebuilds unchanged suites too, so none is skipped.
        assert!(replace.skip.is_empty());
        assert_eq!(replace.update, ids(&["current", "outdated", "legacy"]));

        // An unknown site is treated as empty: everything selected is added.
        let unknown = preview(
            DeploymentMode::Update,
            &selected,
            &[],
            None,
            &local,
            &catalog,
        );
        assert_eq!(unknown.add.len(), 4);
        assert_eq!(unknown.changes(), 4);
    }

    #[test]
    fn outdated_suites_need_a_newer_local_run() {
        let site = SiteSuites {
            suites: BTreeMap::from([("a".into(), Some("run-1".into())), ("b".into(), None)]),
            ..SiteSuites::default()
        };
        assert!(!site.outdated("a", Some("run-1")));
        assert!(site.outdated("a", Some("run-2")));
        assert!(!site.outdated("a", None));
        assert!(site.outdated("b", Some("run-1")));
        assert!(!site.outdated("missing", Some("run-1")));
    }

    #[test]
    fn recorded_inputs_decide_over_run_ids() {
        let mut site = SiteSuites {
            suites: BTreeMap::from([
                ("approved".into(), Some("run-1".into())),
                ("rerun".into(), Some("run-1".into())),
                ("other".into(), Some("run-1".into())),
            ]),
            verified: BTreeMap::from([("approved".into(), false), ("rerun".into(), true)]),
        };
        // An approval keeps the run but changes the inputs; an identical rerun does not count.
        assert!(site.outdated("approved", Some("run-1")));
        assert!(!site.outdated("rerun", Some("run-2")));
        // Without a record, run IDs still decide.
        assert!(site.outdated("other", Some("run-2")));
        let local = BTreeMap::from([
            ("approved".to_owned(), "run-1".to_owned()),
            ("rerun".to_owned(), "run-2".to_owned()),
        ]);
        let plan = preview(
            DeploymentMode::Update,
            &ids(&["approved", "rerun", "other"]),
            &[],
            Some(&site),
            &local,
            &ids(&["approved", "rerun", "other"]),
        );
        assert_eq!(plan.update, ids(&["approved", "other"]));
        assert_eq!(plan.skip, ids(&["rerun"]));
        // A record never claims a suite the site no longer serves.
        site.suites.remove("approved");
        assert!(!site.outdated("approved", Some("run-1")));
    }
}
