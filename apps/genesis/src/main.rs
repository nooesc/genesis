use clap::Parser;
use genesis_cli::{run, Cli};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    init_tracing();

    let cli = Cli::parse();

    match run(cli).await {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("\x1b[38;2;215;95;95m{error}\x1b[0m");
            std::process::exit(1);
        }
    }
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let use_json = std::env::var("GENESIS_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    if use_json {
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
