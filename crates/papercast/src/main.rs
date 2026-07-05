mod commands;
mod config;
mod control;
mod mode;
mod pipeline_thread;
mod transport;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "papercast", version, about = "Mirror a Linux desktop to an e-ink tablet")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
// `Run` carries all the tunable flags so it dwarfs `Probe`; the CLI is parsed
// exactly once, so boxing to shrink the enum would only add indirection.
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Inspect the Wayland compositor: globals, outputs, shm formats, and
    /// which screen-capture protocols are available.
    Probe,
    /// Serve a frame source as a VNC session.
    Run(commands::run::RunArgs),
    /// Control a running `papercast run`: switch mode, force refresh, status.
    Ctl(commands::ctl::CtlArgs),
}

fn main() -> anyhow::Result<()> {
    // RUST_LOG=debug etc. controls verbosity; default to info.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Probe => commands::probe::run(),
        Command::Run(args) => commands::run::run(args),
        Command::Ctl(args) => commands::ctl::run(args),
    }
}
