mod actions;
mod capture_report;
mod helpers;
mod logs;
mod preview_cache;
mod site;

pub use actions::snapshot;
pub use helpers::get_asset_version;
#[cfg(all(test, feature = "desktop"))]
pub(crate) use site::artifact_paths;
pub(crate) use site::ensure_output_gitignore;
#[cfg(feature = "desktop")]
pub(crate) use site::generate_site;
#[cfg(feature = "desktop")]
pub(crate) use site::{
    FRONTEND_FILES, FRONTEND_HTML, GENERATOR, ImageRole, SiteInventory, SuiteSummary,
    artifact_inventory, artifact_inventory_with_suite_root, escape_html, image_roles,
    input_fingerprint, publish_merged_index, publish_ordered_files, refresh_site_versions,
    should_update_suite, site_fingerprint, suite_input_fingerprint, suite_summaries, update_suite,
    valid_segment, valid_suite_id, versioned_path,
};
#[cfg(all(windows, feature = "desktop"))]
pub(crate) use site::{materialization_order, materialization_order_from};
