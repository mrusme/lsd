mod cli;
mod command;
mod net;
mod output;
mod settings;
mod util;

use anyhow::Context;
use clap::Parser;
use cli::{Args, Command, Overrides, UsageError};
use output::{Report, Reporter};
use settings::Settings;
use std::process::ExitCode;
use std::time::Duration;

const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);
const USAGE_EXIT: u8 = 2;

fn main() -> ExitCode {
    let args = Args::parse();
    init_tracing();
    let reporter = Reporter::new(args.json);
    match execute(&args, &reporter).and_then(|report| reporter.finish(&report)) {
        Ok(code) => code,
        Err(err) => {
            reporter.fail(&err);
            if err.downcast_ref::<UsageError>().is_some() {
                ExitCode::from(USAGE_EXIT)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn init_tracing() {
    if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
}

fn execute(args: &Args, reporter: &Reporter) -> anyhow::Result<Report> {
    let command = Command::from_args(args)?;
    let timeout = command.timeout(args.timeout);
    let settings = Settings::load(&Overrides::from(args))?;
    if matches!(command, Command::Receive) {
        settings.prepare_destination()?;
    }
    let runtime = tokio::runtime::Runtime::new().context("Could not start the async runtime")?;
    let report = runtime.block_on(command::dispatch(&command, settings, reporter, timeout));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    report
}
