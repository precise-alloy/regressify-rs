use std::process::exit;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use regressify::{application, cli::Cli};

#[tokio::main]
async fn main() -> Result<()> {
    regressify::analytics::start();
    regressify::diagnostics::install::install_subscriber();
    let startup = regressify::diagnostics::startup::StartupLog::begin();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            let result = if code == 0 {
                Ok(())
            } else {
                Err(anyhow::anyhow!("Invalid command arguments"))
            };
            startup.finish(&result);
            error.print()?;
            regressify::diagnostics::install::flush();
            exit(code);
        }
    };
    let pause = cli.pause;
    let pause_if_error = cli.pause_if_error;
    let mut exit_code = 1;

    let result = async {
        regressify::diagnostics::install::configure_console(
            cli.log_level,
            cli.log_verbose || cli.verbose,
            cli.no_print,
        )?;
        application::execute(cli).await
    }
    .await;
    startup.finish(&result);
    match result {
        Ok(_) => {
            exit_code = 0;

            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let msg = format!("[{}] Finished successfully.", now).dimmed();
            println!("{}", msg);
        }
        Err(e) => {
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            eprintln!("[{}] {} {:?}", now, "ERROR:".red(), e);
        }
    }

    if pause || (pause_if_error && exit_code != 0) {
        eprintln!(" ");
        eprint!("Press any key to continue...");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
    }

    regressify::diagnostics::install::flush();
    exit(exit_code);
}
