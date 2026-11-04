mod actions;
mod capture_report;
mod helpers;
mod logs;
mod site;

pub use actions::snapshot;
pub use helpers::get_asset_version;
pub(crate) use site::ensure_output_gitignore;
#[cfg(all(test, feature = "desktop"))]
pub(crate) use site::generate_site;
#[cfg(feature = "desktop")]
pub(crate) use site::{
    FRONTEND_FILES, FRONTEND_HTML, ImageRole, artifact_paths, escape_html, image_roles,
    refresh_site_versions, should_update_suite, update_suite,
};
