use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use agentx::encodings::{SearchRangeList, Value};
use agentx::pdu::{self, ResError, Type};
use tempfile::TempDir;

mod support;

use support::agentx::{AgentxMaster, NETWORK_ORDER, exchange, header, oid, range};

const IP_SERVES_LINKS: &str = "#!/bin/sh\n[ \"$*\" = '-details -json link show' ] || exit 2\nexec /bin/cat \"$IFSTACK_TEST_LINKS\"\n";
const IP_NEVER_EXITS: &str = "#!/bin/sh\nexec /bin/sleep 600\n";
const IP_CLOSES_PIPES: &str = "#!/bin/sh\nexec 1>&- 2>&-\nexec /bin/sleep 600\n";
const IP_LEAVES_DESCENDANT: &str = concat!(
    "#!/bin/sh\n",
    "/bin/sleep 600 >/dev/null 2>&1 &\n",
    "echo $! > \"$IFSTACK_TEST_PIDFILE\"\n",
    "exec /bin/cat \"$IFSTACK_TEST_LINKS\"\n",
);
// Exits at once, but a descendant keeps the inherited stdout pipe open. The
// descendant records its pid so a test can prove it was cleaned up.
const IP_LEAKS_ITS_PIPE: &str = concat!(
    "#!/bin/sh\n",
    "/bin/sleep 600 &\n",
    "echo $! > \"$IFSTACK_TEST_PIDFILE\"\n",
    "exec /bin/cat \"$IFSTACK_TEST_LINKS\"\n",
);
// Escapes the process group with setsid, so killing the group cannot reach it.
const IP_ESCAPES_ITS_GROUP: &str = concat!(
    "#!/bin/sh\n",
    "/usr/bin/setsid /bin/sleep 600 &\n",
    "echo $! > \"$IFSTACK_TEST_PIDFILE\"\n",
    "exec /bin/cat \"$IFSTACK_TEST_LINKS\"\n",
);
// Never stops writing, so the captured output must be bounded.
const IP_FLOODS_STDOUT: &str = "#!/bin/sh\nexec /usr/bin/yes 0123456789abcdef\n";

fn write_ip(directory: &std::path::Path, script: &str) {
    let ip = directory.join("ip");
    fs::write(&ip, script).unwrap();
    fs::set_permissions(&ip, fs::Permissions::from_mode(0o700)).unwrap();
}

struct Master {
    directory: PathBuf,
    _temporary: TempDir,
    agentx: AgentxMaster,
    child: Child,
}

struct Descendant(OwnedFd);

impl Descendant {
    fn track(pid: i32) -> Option<Self> {
        // SAFETY: pidfd_open takes a process ID and zero flags.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd == -1 {
            let error = std::io::Error::last_os_error();
            assert_eq!(
                error.raw_os_error(),
                Some(libc::ESRCH),
                "pidfd_open: {error}"
            );
            return None;
        }
        // SAFETY: pidfd_open returned a new descriptor owned by this guard.
        Some(Self(unsafe { OwnedFd::from_raw_fd(fd as i32) }))
    }
}

impl Drop for Descendant {
    fn drop(&mut self) {
        // SAFETY: The owned pidfd identifies this descendant even after its PID is reused.
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
        }
    }
}

impl Master {
    fn start() -> Self {
        Self::start_with_config("")
    }

    fn start_with_config(config: &str) -> Self {
        let temporary = tempfile::Builder::new()
            .prefix("ifstack-ip-process-")
            .tempdir()
            .unwrap();
        let directory = temporary.path().to_path_buf();
        let socket = directory.join("master");
        let agentx = AgentxMaster::bind(&socket);
        fs::write(
            directory.join("links.json"),
            include_str!("fixtures/bond.json"),
        )
        .unwrap();
        write_ip(&directory, IP_SERVES_LINKS);
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentx-ifstack"));
        let path = directory.join("config.toml");
        fs::write(&path, format!("socket = {socket:?}\n{config}")).unwrap();
        command.arg("--config").arg(path);
        let child = command
            .env("PATH", &directory)
            .env("IFSTACK_TEST_LINKS", directory.join("links.json"))
            .env("IFSTACK_TEST_PIDFILE", directory.join("descendant.pid"))
            .spawn()
            .unwrap();
        Self {
            directory,
            _temporary: temporary,
            agentx,
            child,
        }
    }

    fn connect(&self, flags: u8, session_id: u32) -> std::os::unix::net::UnixStream {
        self.agentx.connect(flags, session_id, 127)
    }

    fn topology(&self, json: &str) {
        fs::write(self.directory.join("links.json"), json).unwrap();
    }

    fn stall_ip(&self) {
        write_ip(&self.directory, IP_NEVER_EXITS);
    }

    fn restore_ip(&self) {
        write_ip(&self.directory, IP_SERVES_LINKS);
    }

    fn leak_ip_pipe(&self) {
        write_ip(&self.directory, IP_LEAKS_ITS_PIPE);
    }

    fn escape_ip_group(&self) {
        write_ip(&self.directory, IP_ESCAPES_ITS_GROUP);
    }

    fn flood_ip(&self) {
        write_ip(&self.directory, IP_FLOODS_STDOUT);
    }

    fn resource_counts(&self) -> (usize, usize) {
        let count = |name| {
            fs::read_dir(format!("/proc/{}/{name}", self.child.id()))
                .unwrap()
                .inspect(|entry| assert!(entry.is_ok(), "proc entry: {entry:?}"))
                .count()
        };
        (count("task"), count("fd"))
    }

    /// Peak resident memory of the daemon, in KiB, from the kernel's own accounting.
    fn peak_rss_kib(&self) -> u64 {
        let status = fs::read_to_string(format!("/proc/{}/status", self.child.id())).unwrap();
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmHWM:"))
            .and_then(|value| value.split_whitespace().next())
            .unwrap()
            .parse()
            .unwrap()
    }

    /// The pid the leaked-pipe stub recorded for its background descendant.
    fn descendant_pid(&self) -> i32 {
        fs::read_to_string(self.directory.join("descendant.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
}

impl Drop for Master {
    fn drop(&mut self) {
        // Drop runs while a failing assertion unwinds, so a panic here would abort the
        // process and hide that assertion.
        for (label, result) in [
            ("stop test subagent", self.child.kill()),
            ("reap test subagent", self.child.wait().map(|_| ())),
        ] {
            if let Err(error) = result {
                eprintln!("{label}: {error}");
            }
        }
    }
}

fn ranges(suffix: &str) -> SearchRangeList {
    range(oid(suffix))
}

#[test]
fn a_stalled_ip_does_not_wedge_the_session() {
    let master = Master::start_with_config("refresh = 1\n");
    let mut stream = master.connect(NETWORK_ORDER, 100);
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    let mut request = || exchange(&mut stream, &get.to_bytes().unwrap());
    assert_eq!(request().vb.unwrap().0[0].data, Value::Integer(1));

    // `ip` hangs instead of exiting, so the refresh must give up rather than block.
    master.stall_ip();
    std::thread::sleep(Duration::from_millis(1100));
    let started = Instant::now();
    let response = request();
    assert_eq!(response.res_error, ResError::ProcessingError);
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "a stalled ip blocked the request loop for {:?}",
        started.elapsed()
    );

    master.restore_ip();
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(request().vb.unwrap().0[0].data, Value::Integer(1));
}

#[test]
fn an_ip_descendant_holding_the_pipe_does_not_wedge_the_session() {
    let master = Master::start_with_config("refresh = 1\n");
    let mut stream = master.connect(NETWORK_ORDER, 100);
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    let mut request = || exchange(&mut stream, &get.to_bytes().unwrap());
    assert_eq!(request().vb.unwrap().0[0].data, Value::Integer(1));

    master.leak_ip_pipe();
    std::thread::sleep(Duration::from_millis(1100));
    let started = Instant::now();
    assert_eq!(request().res_error, ResError::ProcessingError);
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "a leaked ip pipe blocked the request loop for {:?}",
        started.elapsed()
    );

    master.restore_ip();
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(request().vb.unwrap().0[0].data, Value::Integer(1));
}

/// True while the process is still running. A killed descendant stays visible as a
/// zombie until something reaps it, and `kill -0` succeeds for a zombie, so read the
/// state field instead: `Z` means it has already died.
fn process_alive(pid: i32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // The comm field is parenthesised and may contain spaces, so start after it.
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next());
    !matches!(state, None | Some("Z"))
}

#[test]
fn a_leaked_ip_descendant_is_killed_rather_than_left_running() {
    let master = Master::start_with_config("refresh = 1\n");
    let mut stream = master.connect(NETWORK_ORDER, 100);
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    let mut request = || exchange(&mut stream, &get.to_bytes().unwrap());
    assert_eq!(request().vb.unwrap().0[0].data, Value::Integer(1));

    master.leak_ip_pipe();
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(request().res_error, ResError::ProcessingError);

    let descendant = master.descendant_pid();
    // Group cleanup must kill descendants before it reaps the direct child.
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_alive(descendant) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !process_alive(descendant),
        "leaked ip descendant {descendant} is still running"
    );
}

#[test]
fn an_ip_that_never_stops_writing_is_bounded() {
    let master = Master::start_with_config("refresh = 1\n");
    let mut stream = master.connect(NETWORK_ORDER, 100);
    stream
        .set_read_timeout(Some(Duration::from_secs(90)))
        .unwrap();
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    let mut request = || exchange(&mut stream, &get.to_bytes().unwrap());
    assert_eq!(request().vb.unwrap().0[0].data, Value::Integer(1));

    master.flood_ip();
    std::thread::sleep(Duration::from_millis(1100));
    let started = Instant::now();
    assert_eq!(request().res_error, ResError::ProcessingError);
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "an endless ip blocked the request loop for {:?}",
        started.elapsed()
    );

    // The deadline alone does not bound allocation: an unbounded read_to_end can take
    // hundreds of megabytes before three seconds elapse.
    let peak = master.peak_rss_kib();
    assert!(
        peak < 128 * 1024,
        "peak resident memory reached {peak} KiB while ip flooded stdout"
    );

    master.restore_ip();
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(request().vb.unwrap().0[0].data, Value::Integer(1));
}

#[test]
fn an_ip_descendant_outside_the_group_still_does_not_wedge_the_session() {
    let master = Master::start_with_config("refresh = 1\n");
    let mut stream = master.connect(NETWORK_ORDER, 100);
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    let mut request = || exchange(&mut stream, &get.to_bytes().unwrap());
    assert_eq!(request().vb.unwrap().0[0].data, Value::Integer(1));

    // setsid puts the descendant in its own session, so group cleanup cannot reach it.
    // The request must still come back; waiting for the readers would hang forever.
    master.escape_ip_group();
    std::thread::sleep(Duration::from_millis(1100));
    let started = Instant::now();
    let response = request();
    let _descendant = Descendant::track(master.descendant_pid());
    assert_eq!(response.res_error, ResError::ProcessingError);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "an escaped ip descendant blocked the request loop for {:?}",
        started.elapsed()
    );

    master.restore_ip();
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(request().vb.unwrap().0[0].data, Value::Integer(1));
}

#[test]
fn escaped_ip_descendants_do_not_leak_threads_or_fds() {
    let master = Master::start();
    let mut stream = master.connect(NETWORK_ORDER, 100);
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    let baseline = master.resource_counts();
    let mut counts = Vec::new();
    let mut descendants = Vec::new();
    master.escape_ip_group();
    for cycle in 0..8 {
        assert_eq!(
            exchange(&mut stream, &get.to_bytes().unwrap()).res_error,
            ResError::ProcessingError
        );
        counts.push(master.resource_counts());
        descendants.push(Descendant::track(master.descendant_pid()));
        eprintln!(
            "refresh {cycle}: threads={} fds={}",
            counts[cycle].0, counts[cycle].1
        );
    }
    assert!(
        counts
            .iter()
            .all(|&(threads, fds)| threads <= baseline.0 && fds <= baseline.1),
        "resource leak: baseline={baseline:?}, refresh counts={counts:?}"
    );
    master.restore_ip();
    assert_eq!(
        exchange(&mut stream, &get.to_bytes().unwrap())
            .vb
            .unwrap()
            .0[0]
            .data,
        Value::Integer(1)
    );
    assert_eq!(master.resource_counts(), baseline);
}

#[test]
fn an_ip_that_closes_both_pipes_without_exiting_does_not_wedge_the_session() {
    let master = Master::start();
    let mut stream = master.connect(NETWORK_ORDER, 100);
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    write_ip(&master.directory, IP_CLOSES_PIPES);
    let started = Instant::now();
    assert_eq!(
        exchange(&mut stream, &get.to_bytes().unwrap()).res_error,
        ResError::ProcessingError
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    master.restore_ip();
    assert_eq!(
        exchange(&mut stream, &get.to_bytes().unwrap())
            .vb
            .unwrap()
            .0[0]
            .data,
        Value::Integer(1)
    );
}

#[test]
fn a_successful_ip_kills_descendants_that_closed_their_pipes() {
    let master = Master::start();
    let mut stream = master.connect(NETWORK_ORDER, 100);
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    write_ip(&master.directory, IP_LEAVES_DESCENDANT);
    assert_eq!(
        exchange(&mut stream, &get.to_bytes().unwrap())
            .vb
            .unwrap()
            .0[0]
            .data,
        Value::Integer(1)
    );
    let pid = master.descendant_pid();
    let _descendant = Descendant::track(pid);
    // SIGKILL still needs the descendant scheduled, so allow what the leaked-descendant
    // test already allows.
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_alive(pid),
        "ip left descendant {pid} running after success"
    );
}

#[test]
fn ip_output_limits_apply_to_each_stream() {
    for stderr in [false, true] {
        for oversized in [false, true] {
            let master = Master::start();
            let mut stream = master.connect(NETWORK_ORDER, 100);
            stream
                .set_read_timeout(Some(Duration::from_secs(90)))
                .unwrap();
            let fixture = include_str!("fixtures/bond.json");
            let limit = 16 * 1024 * 1024;
            let size = limit + usize::from(oversized);
            if stderr {
                fs::write(master.directory.join("links.json.stderr"), vec![b' '; size]).unwrap();
                write_ip(
                    &master.directory,
                    concat!(
                        "#!/bin/sh\n",
                        "/bin/cat \"${IFSTACK_TEST_LINKS}.stderr\" >&2\n",
                        "exec /bin/cat \"$IFSTACK_TEST_LINKS\"\n",
                    ),
                );
            } else {
                master.topology(&format!("{}{fixture}", " ".repeat(size - fixture.len())));
            }
            let mut get = pdu::Get::new(ranges(".10.2"));
            get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
            let response = exchange(&mut stream, &get.to_bytes().unwrap());
            assert_eq!(
                response.res_error,
                if oversized {
                    ResError::ProcessingError
                } else {
                    ResError::NoAgentXError
                },
                "stderr={stderr}, size={size}"
            );
            if !oversized {
                assert_eq!(response.vb.unwrap().0[0].data, Value::Integer(1));
            }
        }
    }
}
