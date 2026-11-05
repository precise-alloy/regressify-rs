#![allow(unused_imports)]

mod asset;
mod command;
mod date_time;
mod http;
mod io;
mod locks;
mod numbers;
pub(crate) mod png;
mod scenario_names;
mod slugify;
mod unique_json;

#[cfg(unix)]
mod mode;

pub use asset::AssetExtensions;
pub use command::CommandExtensions;
#[cfg(feature = "desktop")]
pub(crate) use command::spawn_background_group;
pub use date_time::{NaiveTimeExtensions, elapsed_time, sleep_remaining_time};
pub use http::HttpExtensions;
pub use io::{
    IOExtensions, copy_file_atomically, format_size, is_windows_reserved_path_component,
    write_file_atomically,
};
pub use locks::{OnceLockBoolExtensions, OnceLockExtensions};
pub use numbers::format_bytes;
pub(crate) use scenario_names::{
    ScenarioName, capture_details, generated_capture_label, scenario_names,
};
pub use slugify::slugify;
pub(crate) use unique_json::UniqueJsonValue;

#[cfg(unix)]
pub use mode::cmod;
#[cfg(unix)]
pub use mode::set_unix_mode;
