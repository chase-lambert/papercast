//! `papercast ctl ...` — the client half of the control socket. Talks to a
//! running `papercast run` over its Unix socket to switch modes, force a
//! refresh, or read status. Plain blocking code (no tokio), like `probe`.

use clap::{Args, Subcommand};

use crate::control::{self, Request, Status};

#[derive(Args)]
pub struct CtlArgs {
    #[command(subcommand)]
    pub cmd: CtlCommand,
}

#[derive(Subcommand)]
pub enum CtlCommand {
    /// Switch the active display mode (reading | browsing | writing | video
    /// or a custom mode from config).
    Mode {
        /// Mode name.
        name: String,
    },
    /// Force one full-frame redraw now (clears e-ink ghosting).
    Refresh,
    /// Print the running mirror's effective settings.
    Status,
}

pub fn run(args: CtlArgs) -> anyhow::Result<()> {
    let req = match args.cmd {
        CtlCommand::Mode { name } => Request::Mode { name },
        CtlCommand::Refresh => Request::Refresh,
        CtlCommand::Status => Request::Status,
    };
    let resp = control::send(&req)?;
    if !resp.ok {
        anyhow::bail!(resp.error.unwrap_or_else(|| "unknown error".into()));
    }
    match resp.status {
        Some(status) => print_status(&status),
        None => println!("ok"),
    }
    Ok(())
}

fn print_status(s: &Status) {
    println!("mode:             {}", s.mode.as_deref().unwrap_or("(none)"));
    println!("fps:              {}", s.fps);
    println!("levels:           {}", s.levels);
    println!("dither:           {:?}", s.dither);
    println!("tile-size:        {}", s.tile_size);
    println!("full-refresh:     {} s / {} updates", s.full_refresh_secs, s.full_refresh_updates);
    println!("framebuffer:      {}x{}", s.framebuffer.0, s.framebuffer.1);
    println!("output:           {}", s.output.as_deref().unwrap_or("(first)"));
}
