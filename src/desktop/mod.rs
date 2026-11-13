//! Headless desktop models. GPUI is only needed by the optional desktop binary.

pub mod completion;
pub mod deployment;
pub mod document;
pub mod git;
pub mod instance;
pub mod landing;
pub mod logs;
pub mod results;
pub mod runner;
pub mod schema;
pub mod session;
pub mod updates;
pub mod validation;

#[cfg(any(feature = "gui", test))]
pub(crate) mod vscode;

pub mod worker;
pub mod workspace;

#[cfg(all(feature = "gui", target_os = "windows"))]
pub mod jump_list;

#[cfg(feature = "gui")]
pub mod ui;

/// The desktop app's per-user directories (settings, cache), when the platform has them.
pub(crate) fn project_directories() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "Regressify", "Regressify Desktop")
}
