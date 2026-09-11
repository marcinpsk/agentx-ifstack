mod config;
mod link;
mod mib;
mod session;

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let config = match config::load(std::env::args_os().skip(1), Path::new(config::DEFAULT_PATH)) {
        Ok(config::Action::Help) => {
            println!(
                "Usage: agentx-ifstack [--config PATH] [--socket PATH]\n\n\
                 Serve ifStackTable through an AgentX Unix socket.\n\n\
                 Options:\n\
                   --config PATH  Read configuration (default: {})\n\
                   --socket PATH  Override the configured AgentX socket\n\
                   --version      Print the version and exit\n\
                   -h, --help     Print this help and exit",
                config::DEFAULT_PATH
            );
            return ExitCode::SUCCESS;
        }
        Ok(config::Action::Version) => {
            println!("agentx-ifstack {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(config::Action::Run(config)) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    init_logging(config.log_level.filter());
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = Instant::now();
        match session::run(&config) {
            Ok(()) => log::warn!("AgentX master closed the session"),
            Err(error) => log::warn!("AgentX session ended: {error}"),
        }
        if started.elapsed() >= Duration::from_secs(30) {
            backoff = Duration::from_secs(1);
        }
        log::info!("Reconnecting in {} seconds", backoff.as_secs());
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

fn init_logging(level: log::LevelFilter) {
    env_logger::Builder::new()
        .filter_level(level)
        .parse_default_env()
        .format_timestamp(None)
        .target(env_logger::Target::Stderr)
        .init();
}
