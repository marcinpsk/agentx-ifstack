mod config;
mod link;
mod mib;
mod monitor;
mod netlink;
mod notify;
mod session;

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const MONITOR_FAILURE_EXIT_STATUS: i32 = 70;

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
    let tables = monitor::publication();
    let monitor_tables = tables.clone();
    let reconcile = Duration::from_secs(config.reconcile);
    if let Err(error) = std::thread::Builder::new()
        .name("topology-monitor".to_owned())
        .spawn(move || {
            run_monitor(|| {
                monitor::run(netlink::NetlinkSource::new(), &monitor_tables, reconcile);
            });
        })
    {
        log::error!("Cannot start topology monitor: {error}");
        return ExitCode::FAILURE;
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = Instant::now();
        match session::run(&config, &tables, &notify::ready) {
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

fn run_monitor(run: impl FnOnce()) -> ! {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(()) => log::error!("Topology monitor stopped unexpectedly"),
        Err(_) => log::error!("Topology monitor panicked"),
    }
    std::process::exit(MONITOR_FAILURE_EXIT_STATUS);
}

fn init_logging(level: log::LevelFilter) {
    env_logger::Builder::new()
        .filter_level(level)
        .parse_default_env()
        .format_timestamp(None)
        .target(env_logger::Target::Stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::run_monitor;

    const MONITOR_PANIC_CHILD: &str = "AGENTX_IFSTACK_MONITOR_PANIC_CHILD";

    #[test]
    fn a_monitor_panic_stops_the_process() {
        if std::env::var_os(MONITOR_PANIC_CHILD).is_some() {
            run_monitor(|| panic!("simulated topology monitor failure"));
        }

        let output = Command::new(std::env::current_exe().unwrap())
            .env(MONITOR_PANIC_CHILD, "1")
            .args(["--exact", "tests::a_monitor_panic_stops_the_process"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(70));
    }

    #[test]
    fn ignored_unit_tests_do_not_run_the_monitor_exit_driver() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--nocapture"])
            .output()
            .unwrap();

        assert!(output.status.success());
    }
}
