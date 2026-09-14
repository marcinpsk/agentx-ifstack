use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_PATH: &str = "/etc/agentx-ifstack.toml";

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub socket: PathBuf,
    pub reconcile: u64,
    pub priority: u8,
    pub log_level: LogLevel,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn filter(self) -> log::LevelFilter {
        match self {
            Self::Error => log::LevelFilter::Error,
            Self::Warn => log::LevelFilter::Warn,
            Self::Info => log::LevelFilter::Info,
            Self::Debug => log::LevelFilter::Debug,
            Self::Trace => log::LevelFilter::Trace,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket: PathBuf::from("/var/agentx/master"),
            reconcile: 3600,
            priority: 127,
            log_level: LogLevel::Info,
        }
    }
}

#[derive(Debug)]
pub enum Action {
    Run(Config),
    Help,
    Version,
}

pub fn load(
    args: impl IntoIterator<Item = OsString>,
    default_path: &Path,
) -> Result<Action, String> {
    let mut args = args.into_iter();
    let mut path = None;
    let mut socket = None;
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(Action::Help);
        } else if arg == "--version" {
            return Ok(Action::Version);
        } else if arg == "--config" || arg == "--socket" {
            let value = args
                .next()
                .filter(|value| !value.is_empty() && !value.to_string_lossy().starts_with("--"))
                .ok_or_else(|| format!("{} requires a path", arg.to_string_lossy()))?;
            if arg == "--config" {
                path = Some(PathBuf::from(value));
            } else {
                socket = Some(PathBuf::from(value));
            }
        } else {
            return Err(format!("Unknown argument: {}", arg.to_string_lossy()));
        }
    }

    let selected_path = path.as_deref().unwrap_or(default_path);
    let mut config: Config = match fs::read_to_string(selected_path) {
        Ok(contents) => toml::from_str(&contents)
            .map_err(|error| format!("{}: {error}", selected_path.display()))?,
        Err(error) if error.kind() == ErrorKind::NotFound && path.is_none() => Config::default(),
        Err(error) => return Err(format!("config {}: {error}", selected_path.display())),
    };
    // Apply the documented override first, so validation sees the effective socket.
    if let Some(socket) = socket {
        config.socket = socket;
    }
    if config.socket.as_os_str().is_empty() {
        return Err("socket must not be empty".into());
    }
    if config.reconcile == 0 {
        return Err("reconcile must be at least 1 second".into());
    }
    if config.priority == 0 {
        return Err("priority must be between 1 and 255".into());
    }
    Ok(Action::Run(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: Vec<OsString>, default_path: &Path) -> Config {
        match load(args, default_path).unwrap() {
            Action::Run(config) => config,
            action => panic!("expected configuration, got {action:?}"),
        }
    }

    #[test]
    fn real_file_sets_all_four_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "socket = '/run/agentx/master'\nreconcile = 1\npriority = 255\nlog_level = 'trace'\n",
        )
        .unwrap();
        let config = run(vec!["--config".into(), path.clone().into()], &path);
        assert_eq!(config.socket, Path::new("/run/agentx/master"));
        assert_eq!(config.reconcile, 1);
        assert_eq!(config.priority, 255);
        assert_eq!(config.log_level.filter(), log::LevelFilter::Trace);
    }

    #[test]
    fn absent_default_uses_built_in_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let config = run(vec![], &directory.path().join("absent.toml"));
        assert_eq!(config.socket, Path::new("/var/agentx/master"));
        assert_eq!(config.reconcile, 3600);
        assert_eq!(config.priority, 127);
        assert_eq!(config.log_level.filter(), log::LevelFilter::Info);
    }

    #[test]
    fn absent_explicit_file_is_an_error_even_at_default_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("absent.toml");
        let error = load(vec!["--config".into(), path.clone().into()], &path).unwrap_err();
        assert!(error.contains("absent.toml"), "{error}");
    }

    #[test]
    fn reconcile_replaces_refresh_without_a_compatibility_alias() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");

        fs::write(&path, "reconcile = 7").unwrap();
        assert!(load(vec![], &path).is_ok());

        fs::write(&path, "refresh = 7").unwrap();
        let error = load(vec![], &path).unwrap_err();
        assert!(error.contains("refresh"), "{error}");
    }

    #[test]
    fn invalid_files_name_the_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        for (contents, key) in [
            ("sockett = 'master'", "sockett"),
            ("socket = ''", "socket"),
            ("socket = 42", "socket"),
            ("reconcile = 'five'", "reconcile"),
            ("reconcile = 0", "reconcile"),
            ("reconcile = -1", "reconcile"),
            ("reconcile = 1.5", "reconcile"),
            ("priority = 0", "priority"),
            ("priority = 256", "priority"),
            ("priority = -1", "priority"),
            ("log_level = 'verbose'", "log_level"),
            ("log_level = 'off'", "log_level"),
            ("log_level = 'INFO'", "log_level"),
            ("reconcile = [", "reconcile"),
        ] {
            fs::write(&path, contents).unwrap();
            let error = load(vec![], &path).unwrap_err();
            assert!(error.contains(key), "{contents}: {error}");
        }
    }

    #[test]
    fn shipped_config_matches_defaults() {
        let config: Config =
            toml::from_str(include_str!("../packaging/agentx-ifstack.toml")).unwrap();
        let defaults = Config::default();
        assert_eq!(config.socket, defaults.socket);
        assert_eq!(config.reconcile, defaults.reconcile);
        assert_eq!(config.priority, defaults.priority);
        assert_eq!(config.log_level.filter(), defaults.log_level.filter());
    }

    #[test]
    fn cli_socket_overrides_file_in_either_argument_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "socket = 'file-socket'\npriority = 1").unwrap();
        for args in [
            vec![
                "--config".into(),
                path.clone().into(),
                "--socket".into(),
                "cli-socket".into(),
            ],
            vec![
                "--socket".into(),
                "cli-socket".into(),
                "--config".into(),
                path.clone().into(),
            ],
        ] {
            let config = run(args, &path);
            assert_eq!(config.socket, Path::new("cli-socket"));
            assert_eq!(config.priority, 1);
            assert_eq!(config.reconcile, 3600);
        }
    }
}
