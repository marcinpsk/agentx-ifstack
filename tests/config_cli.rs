use std::fs;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn finish(mut command: Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("configuration entered the supervise loop: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn invalid_configuration_exits_before_connecting() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let socket = directory.path().join("master");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    for contents in [
        None,
        Some("reconcile = 0"),
        Some("refresh = 0"),
        Some("priority = 256"),
        Some("sockett = 'master'"),
        Some("log_level = 'off'"),
        Some("refresh = ["),
    ] {
        if let Some(contents) = contents {
            fs::write(&path, contents).unwrap();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentx-ifstack"));
        command
            .arg("--config")
            .arg(&path)
            .arg("--socket")
            .arg(&socket)
            .env_remove("RUST_LOG");
        let output = finish(command);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn fatal_configuration_error_is_visible_with_logging_disabled() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, "refresh = 0").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentx-ifstack"));
    command.arg("--config").arg(&path).env("RUST_LOG", "off");
    let output = finish(command);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.is_empty(), "fatal error must be visible");
    assert!(stderr.contains("refresh"), "{stderr}");
}

#[test]
fn help_and_version_exit_without_reading_configuration() {
    let directory = tempfile::tempdir().unwrap();
    for option in ["--help", "-h", "--version"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentx-ifstack"));
        command
            .arg("--config")
            .arg(directory.path().join("absent.toml"))
            .arg(option);
        let output = finish(command);
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        if option == "--version" {
            assert_eq!(
                stdout,
                format!("agentx-ifstack {}\n", env!("CARGO_PKG_VERSION"))
            );
        } else {
            for flag in ["--config", "--socket", "--version", "--help"] {
                assert!(stdout.contains(flag));
            }
        }
    }
}

#[test]
fn invalid_cli_arguments_exit_nonzero() {
    for args in [
        vec!["--config"],
        vec!["--socket"],
        vec!["--socket", ""],
        vec!["--config", "--socket"],
        vec!["--unknown"],
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentx-ifstack"));
        command.args(args);
        assert_eq!(finish(command).status.code(), Some(1));
    }
}

#[test]
fn an_empty_file_socket_is_rejected_without_a_cli_override() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, "socket = ''").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentx-ifstack"));
    command.arg("--config").arg(&path).env_remove("RUST_LOG");
    let output = finish(command);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

#[test]
fn cli_socket_overrides_an_empty_file_socket() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let socket = directory.path().join("master");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::write(&path, "socket = ''").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentx-ifstack"))
        .arg("--config")
        .arg(&path)
        .arg("--socket")
        .arg(&socket)
        .env_remove("RUST_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The override has to reach validation, so the daemon starts and connects.
    let deadline = Instant::now() + Duration::from_secs(10);
    let connected = loop {
        if listener.accept().is_ok() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        connected,
        "--socket did not override the empty file socket: {output:?}"
    );
}
