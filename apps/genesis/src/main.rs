use std::io::IsTerminal;

use clap::Parser;
use genesis_cli::{run, Cli, Command};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Parse CLI first so we know whether TUI mode is active before
    // initializing tracing.  In TUI mode, logging MUST go to a file —
    // writing to stdout would corrupt the ratatui viewport.
    let cli = Cli::parse();

    let tui_active = matches!(
        &cli.command,
        Command::Chat { no_tui: false, .. }
    ) && std::io::stdout().is_terminal();

    init_tracing(tui_active);

    match run(cli).await {
        Ok(output) => {
            if !output.is_empty() {
                println!("{output}");
            }
        }
        Err(error) => {
            eprintln!("\x1b[38;2;215;95;95m{error}\x1b[0m");
            std::process::exit(1);
        }
    }
}

fn init_tracing(tui_active: bool) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let use_json = std::env::var("GENESIS_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    if tui_active {
        // TUI mode: redirect all logging to ~/.genesis/logs/tui.log so it
        // doesn't corrupt the ratatui viewport.
        if let Some(log_path) = genesis_tui::tui_log_path() {
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_target(false)
                    .compact()
                    .with_ansi(false)
                    .with_writer(file)
                    .try_init();
                return;
            }
        }
        // If file logging setup failed, suppress all output to avoid
        // corrupting the TUI.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("off"))
            .try_init();
    } else if use_json {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .compact()
            .try_init();
    }
    tracing::debug!("tracing initialized");
}
