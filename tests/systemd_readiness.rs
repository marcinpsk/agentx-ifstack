use std::io::ErrorKind;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;

#[path = "support/master.rs"]
mod master;

use master::{AgentxMaster, NETWORK_ORDER};

// systemd holds a Type=notify unit in "activating" until READY=1 arrives, so
// this datagram is what makes "started" mean "registered" for the deployment.
const READY: &[u8] = b"READY=1";

struct Subagent {
    child: Child,
}

impl Drop for Subagent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn(directory: &Path, socket: &Path, notify: &str) -> Subagent {
    let config = directory.join("config.toml");
    std::fs::write(&config, format!("socket = {socket:?}\n")).expect("write test configuration");
    let child = Command::new(env!("CARGO_BIN_EXE_agentx-ifstack"))
        .arg("--config")
        .arg(config)
        .env("NOTIFY_SOCKET", notify)
        .env_remove("RUST_LOG")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start agentx-ifstack test process");
    Subagent { child }
}

fn receiver(address: &SocketAddr) -> UnixDatagram {
    let datagram = UnixDatagram::bind_addr(address).expect("bind notify socket");
    datagram
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("bound the readiness wait");
    datagram
}

fn read(datagram: &UnixDatagram) -> Vec<u8> {
    let mut message = vec![0; 64];
    let read = datagram.recv(&mut message).expect("receive readiness");
    message.truncate(read);
    message
}

#[test]
fn the_subagent_reports_readiness_once_it_registers_the_table() {
    let directory = TempDir::new().expect("create test directory");
    let socket = directory.path().join("master");
    let notify = directory.path().join("notify");
    let datagram = receiver(&SocketAddr::from_pathname(&notify).expect("notify socket path"));
    let master = AgentxMaster::bind(&socket);
    let _subagent = spawn(
        directory.path(),
        &socket,
        notify.to_str().expect("notify path is UTF-8"),
    );

    let _session = master.connect(NETWORK_ORDER, 100, 127);

    assert_eq!(read(&datagram), READY);
}

// systemd passes an abstract socket as a leading @, which is not a filesystem
// path. Reading it as one silently loses every notification.
#[test]
fn an_abstract_notify_socket_receives_the_same_message() {
    let directory = TempDir::new().expect("create test directory");
    let socket = directory.path().join("master");
    let name = format!("agentx-ifstack-test-{}", std::process::id());
    let datagram =
        receiver(&SocketAddr::from_abstract_name(name.as_bytes()).expect("abstract notify socket"));
    let master = AgentxMaster::bind(&socket);
    let _subagent = spawn(directory.path(), &socket, &format!("@{name}"));

    let _session = master.connect(NETWORK_ORDER, 100, 127);

    assert_eq!(read(&datagram), READY);
}

// A subagent that is up but has never registered serves nothing. Reporting
// readiness on start would hide exactly the failure the unit must catch.
#[test]
fn a_subagent_without_a_master_reports_nothing() {
    let directory = TempDir::new().expect("create test directory");
    let socket = directory.path().join("master");
    let notify = directory.path().join("notify");
    let datagram = receiver(&SocketAddr::from_pathname(&notify).expect("notify socket path"));
    datagram
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("bound the negative wait");
    let _subagent = spawn(
        directory.path(),
        &socket,
        notify.to_str().expect("notify path is UTF-8"),
    );

    let error = datagram
        .recv(&mut [0; 64])
        .expect_err("readiness without a registered session");

    assert!(
        matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
        "unexpected error: {error}"
    );
}
