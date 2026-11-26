#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use clap::Parser;
use regressify::diagnostics::{install, record::SafeValue, startup};
use std::process::ExitCode;

#[derive(Parser)]
#[command(about = "Native Regressify QA workbench", version = regressify::desktop::updates::CURRENT_VERSION)]
struct DesktopArgs {
    /// Workspace directory to open on startup
    workspace: Option<std::path::PathBuf>,
    #[arg(long, hide = true)]
    after_install: bool,
    #[arg(long, hide = true, conflicts_with_all = ["workspace", "after_install"])]
    install_chromium: bool,
}

fn install_chromium() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(regressify::test::fetcher())
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("Cannot prepare Chromium: {error:?}"))
}

fn main() -> ExitCode {
    install::install_subscriber();
    let component = if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == regressify::desktop::worker::WORKER_FLAG)
    {
        SafeValue::Worker
    } else {
        SafeValue::Desktop
    };
    let startup = startup::StartupLog::begin_for(component);
    let code = run();
    startup.finish(&if code == ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Desktop startup or worker execution failed"
        ))
    });
    install::flush();
    code
}

fn run() -> ExitCode {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == regressify::desktop::worker::WORKER_FLAG)
    {
        if let Err(error) = regressify::desktop::updates::protect_installation(false) {
            startup::warning(SafeValue::Worker, &error);
            eprintln!("{error:#}");
            return ExitCode::FAILURE;
        }
        return match regressify::desktop::worker::run_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                startup::warning(SafeValue::Worker, &error);
                eprintln!("{error:#}");
                ExitCode::FAILURE
            }
        };
    }
    regressify::analytics::start();
    let args = match DesktopArgs::try_parse() {
        Ok(args) => args,
        Err(error) => {
            let code = error.exit_code();
            if code != 0 {
                startup::warning(
                    SafeValue::Desktop,
                    &anyhow::anyhow!("Invalid desktop arguments"),
                );
            }
            let _ = error.print();
            return ExitCode::from(code as u8);
        }
    };
    if args.install_chromium {
        return match install_chromium() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                startup::warning(SafeValue::Install, &error);
                eprintln!("{error:#}");
                ExitCode::FAILURE
            }
        };
    }
    #[cfg(target_os = "windows")]
    if let Err(error) = regressify::desktop::jump_list::initialize() {
        startup::warning(SafeValue::Shell, &error);
    }
    if let Err(error) = regressify::desktop::updates::protect_installation(args.after_install) {
        startup::warning(SafeValue::Desktop, &error);
        rfd::MessageDialog::new()
            .set_title("Regressify Desktop")
            .set_description(format!("{error:#}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        return ExitCode::FAILURE;
    }
    let instance = match args.workspace {
        Some(root) => {
            let result = regressify::desktop::workspace::Preferences::path().and_then(|path| {
                regressify::desktop::instance::DesktopInstance::open(
                    &path.with_file_name("instances"),
                    &root,
                )
            });
            match result {
                Ok(regressify::desktop::instance::OpenInstance::Owned(instance)) => Some(instance),
                Ok(regressify::desktop::instance::OpenInstance::Activated) => {
                    #[cfg(target_os = "windows")]
                    regressify::desktop::jump_list::record_workspace(&root);
                    return ExitCode::SUCCESS;
                }
                Err(error) => {
                    startup::warning(SafeValue::Window, &error);
                    rfd::MessageDialog::new()
                        .set_title("Regressify Desktop")
                        .set_description(format!("{error:#}"))
                        .set_level(rfd::MessageLevel::Error)
                        .show();
                    return ExitCode::FAILURE;
                }
            }
        }
        None => None,
    };
    regressify::desktop::ui::launch(instance);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::DesktopArgs;
    use clap::Parser;

    #[test]
    fn installer_browser_mode_does_not_require_a_workspace() {
        let args =
            DesktopArgs::try_parse_from(["regressify-desktop", "--install-chromium"]).unwrap();
        assert!(args.install_chromium);
        assert!(!args.after_install);
        assert!(args.workspace.is_none());
    }

    #[test]
    fn installer_browser_mode_cannot_also_launch_the_workbench() {
        for extra in ["--after-install", "qa-workspace", "--internal-worker"] {
            assert!(
                DesktopArgs::try_parse_from(["regressify-desktop", "--install-chromium", extra])
                    .is_err()
            );
        }
    }
}
