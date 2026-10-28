pub mod actionability;
mod authentication;
mod browsers;
pub mod capture;
mod cdp;
mod dialogs;
mod gestures;
mod hooks;
pub(crate) mod image_simplification;
mod keyboard;
mod page_stability;
pub mod runtime;
mod storage_state;
mod tabs;
mod test_action;
mod test_scenario;
mod test_suite;
mod test_suites;

#[cfg(test)]
pub(crate) mod browser_tests;

pub use browsers::fetcher;
pub use test_suites::exec_ref;
pub use test_suites::exec_test_suites;

#[cfg(feature = "desktop")]
pub(crate) use test_suites::discover_test_suites;

pub(crate) use test_suites::is_reserved_suite_identifier;
pub(crate) use test_suites::validate_suite_identifier;

#[cfg(feature = "desktop")]
pub(crate) use test_suites::{
    ActionViewportNameError, ScenarioDependencyError, validate_action_viewport_names,
};

#[cfg(any(feature = "desktop", test))]
pub(crate) use test_suites::validate_scenario_dependencies;

#[cfg(any(feature = "desktop", test))]
pub(crate) use test_action::validate_action_fields;

#[cfg(feature = "desktop")]
pub(crate) use test_suite::assign_scenario_ids;
