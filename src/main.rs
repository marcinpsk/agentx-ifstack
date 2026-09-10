mod link;
mod mib;
mod session;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let mut socket = PathBuf::from("/var/agentx/master");
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!(
                "Usage: agentx-ifstack [--socket PATH]\n\nServe ifStackTable through an AgentX Unix socket."
            );
            return ExitCode::SUCCESS;
        }
        if arg == "--socket" {
            match args.next() {
                Some(path) if !path.is_empty() => socket = path.into(),
                _ => {
                    eprintln!("--socket requires a path");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            eprintln!("Unknown argument: {}", arg.to_string_lossy());
            return ExitCode::FAILURE;
        }
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = Instant::now();
        match session::run(&socket) {
            Ok(()) => eprintln!("AgentX master closed the session"),
            Err(error) => eprintln!("AgentX session ended: {error}"),
        }
        if started.elapsed() >= Duration::from_secs(30) {
            backoff = Duration::from_secs(1);
        }
        eprintln!("Reconnecting in {} seconds", backoff.as_secs());
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}
