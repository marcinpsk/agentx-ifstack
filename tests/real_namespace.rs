use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt};
use std::path::Path;
use std::process::{Child, Command, Output};
use std::str::FromStr;
use std::time::{Duration, Instant};

use agentx::encodings::{
    Context, ID, OctetString, SearchRange, SearchRangeList, Value, VarBind, VarBindList,
};
use agentx::pdu::{self, Header, ResError, Response, Type};
use serde::Deserialize;
use tempfile::TempDir;

mod support;

use support::agentx::{
    AgentxMaster, NETWORK_ORDER, STATUS, exchange, header, oid, range, read_frame,
};

const TEST_UID: u32 = 65_534;
const TEST_GID: u32 = 65_534;

struct Master {
    _directory: TempDir,
    agentx: AgentxMaster,
    child: Child,
}

enum ProcessIdentity {
    Unprivileged,
    TestProcess,
}

impl Master {
    fn start(config: &str, socket_override: bool) -> Self {
        Self::start_with_identity(config, socket_override, ProcessIdentity::Unprivileged)
    }

    fn start_as_test_process(config: &str) -> Self {
        Self::start_with_identity(config, false, ProcessIdentity::TestProcess)
    }

    fn start_with_identity(config: &str, socket_override: bool, identity: ProcessIdentity) -> Self {
        let directory = tempfile::Builder::new()
            .prefix("ifstack-real-")
            .tempdir()
            .expect("create test directory");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
            .expect("make test directory traversable by the subagent");
        let socket = directory.path().join("master");
        let agentx = AgentxMaster::bind(&socket);

        let configured_socket = if socket_override {
            directory.path().join("unused")
        } else {
            socket.clone()
        };
        let config_path = directory.path().join("config.toml");
        write_subagent_config(
            &config_path,
            format!("socket = {configured_socket:?}\n{config}"),
        );

        let executable = directory.path().join("agentx-ifstack");
        fs::copy(env!("CARGO_BIN_EXE_agentx-ifstack"), &executable)
            .expect("copy the production binary into the test directory");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("make the test copy executable by the subagent user");
        let mut command = Command::new(executable);
        command.arg("--config").arg(config_path);
        if socket_override {
            command.arg("--socket").arg(&socket);
        }
        if matches!(identity, ProcessIdentity::Unprivileged) {
            command.uid(TEST_UID).gid(TEST_GID);
        }
        let child = command.spawn().expect("start agentx-ifstack test process");
        Self {
            _directory: directory,
            agentx,
            child,
        }
    }

    fn connect(&self, flags: u8, session_id: u32) -> UnixStream {
        self.agentx.connect(flags, session_id, 127)
    }

    fn assert_unprivileged(&self) {
        let status = fs::read_to_string(format!("/proc/{}/status", self.child.id()))
            .expect("read subagent process status");
        assert!(
            status
                .lines()
                .any(|line| line == "CapEff:\t0000000000000000"),
            "subagent retained effective capabilities:\n{status}"
        );
        assert!(
            status
                .lines()
                .find(|line| line.starts_with("Uid:"))
                .is_some_and(|line| line.split_whitespace().nth(2) == Some("65534")),
            "subagent did not run with the test UID:\n{status}"
        );
    }

    fn resource_counts(&self) -> (usize, usize) {
        let count = |name| {
            fs::read_dir(format!("/proc/{}/{name}", self.child.id()))
                .expect("read subagent process resources")
                .inspect(|entry| assert!(entry.is_ok(), "read process resource: {entry:?}"))
                .count()
        };
        (count("task"), count("fd"))
    }
}

fn write_subagent_config(path: &Path, config: String) {
    fs::write(path, config).expect("write AgentX test config");
    fs::set_permissions(path, fs::Permissions::from_mode(0o444))
        .expect("make AgentX test config readable by the subagent user");
}

impl Drop for Master {
    fn drop(&mut self) {
        // Drop runs during assertion unwinding. Do not hide the first failure.
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

#[derive(Deserialize)]
struct LinkIndex {
    ifindex: u32,
    ifname: String,
}

#[test]
fn subagent_config_is_readable_after_restrictive_creation() {
    let directory = TempDir::new().expect("create test directory");
    let config = directory.path().join("config.toml");
    fs::write(&config, "").expect("create restrictive config");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))
        .expect("restrict initial config permissions");

    write_subagent_config(&config, "socket = \"/tmp/master\"\n".to_owned());

    let mode = fs::metadata(config)
        .expect("read config metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_ne!(
        mode & 0o004,
        0,
        "test config mode {mode:o} is not readable by the subagent user"
    );
}

fn enter_network_namespace() {
    let version = Command::new("ip")
        .arg("-Version")
        .output()
        .expect("iproute2 is required for real namespace tests");
    assert!(version.status.success(), "ip -Version failed");
    // SAFETY: unshare changes only the calling test thread's network namespace.
    let result = unsafe { libc::unshare(libc::CLONE_NEWNET) };
    assert_eq!(
        result,
        0,
        "real namespace tests require root and permission to create a network namespace: {}",
        std::io::Error::last_os_error()
    );
    ip(&["link", "set", "lo", "up"]);
}

fn ip(arguments: &[&str]) {
    let output = Command::new("ip")
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("run ip {arguments:?}: {error}"));
    assert_command_succeeded(arguments, &output);
}

fn assert_command_succeeded(arguments: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "ip {arguments:?} failed with {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn interface_indices() -> BTreeMap<String, u32> {
    let arguments = ["-json", "link", "show"];
    let output = Command::new("ip")
        .args(arguments)
        .output()
        .expect("read interface indices with ip");
    assert_command_succeeded(&arguments, &output);
    serde_json::from_slice::<Vec<LinkIndex>>(&output.stdout)
        .expect("decode ip link JSON")
        .into_iter()
        .map(|link| (link.ifname, link.ifindex))
        .collect()
}

fn create_chain() -> BTreeMap<String, u32> {
    ip(&["link", "add", "bond0", "type", "bond"]);
    ip(&["link", "add", "bondmember", "type", "dummy"]);
    ip(&["link", "set", "bondmember", "master", "bond0"]);
    ip(&[
        "link", "add", "link", "bond0", "name", "vlan0", "type", "vlan", "id", "100",
    ]);
    interface_indices()
}

fn create_complete_topology() -> BTreeMap<String, u32> {
    ip(&["link", "add", "standalone", "type", "dummy"]);
    create_chain();
    ip(&["link", "add", "bridge0", "type", "bridge"]);
    ip(&["link", "add", "bridgemember", "type", "dummy"]);
    ip(&["link", "set", "bridgemember", "master", "bridge0"]);
    ip(&["link", "add", "lower0", "type", "dummy"]);
    ip(&[
        "link", "add", "link", "lower0", "name", "macvlan0", "type", "macvlan", "mode", "bridge",
    ]);
    ip(&["link", "add", "ipvlanlower", "type", "dummy"]);
    ip(&[
        "link",
        "add",
        "link",
        "ipvlanlower",
        "name",
        "ipvlan0",
        "type",
        "ipvlan",
        "mode",
        "l2",
    ]);
    ip(&[
        "link", "add", "link", "lower0", "name", "macvtap0", "type", "macvtap", "mode", "bridge",
    ]);
    ip(&[
        "link", "add", "vxlan0", "type", "vxlan", "id", "42", "dev", "lower0", "dstport", "4789",
    ]);
    ip(&[
        "link", "add", "veth0", "type", "veth", "peer", "name", "veth1",
    ]);
    interface_indices()
}

fn row_oid((higher, lower): (u32, u32)) -> ID {
    oid(&format!(".{higher}.{lower}"))
}

fn get_value(
    stream: &mut UnixStream,
    flags: u8,
    session_id: u32,
    packet_id: u32,
    row: (u32, u32),
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut get = pdu::Get::new(range(row_oid(row)));
        get.header = header(Type::Get, flags, session_id, packet_id);
        let response = exchange(stream, &get.to_bytes().expect("encode Get PDU"));
        if response.res_error == ResError::NoAgentXError {
            return response
                .vb
                .expect("Get response has bindings")
                .0
                .into_iter()
                .next()
                .expect("Get response has one binding")
                .data;
        }
        assert_eq!(response.res_error, ResError::ProcessingError);
        assert!(
            Instant::now() < deadline,
            "topology monitor did not publish an initial inventory"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn walk(
    stream: &mut UnixStream,
    flags: u8,
    session_id: u32,
    first_packet_id: u32,
) -> Vec<(u32, u32)> {
    let mut start = oid("");
    let mut rows = Vec::new();
    for packet_id in first_packet_id..first_packet_id + 1024 {
        let deadline = Instant::now() + Duration::from_secs(5);
        let response = loop {
            let mut next = pdu::GetNext::new(range(start.clone()));
            next.header = header(Type::GetNext, flags, session_id, packet_id);
            let response = exchange(stream, &next.to_bytes().expect("encode GetNext PDU"));
            if response.res_error != ResError::ProcessingError {
                break response;
            }
            assert!(
                Instant::now() < deadline,
                "topology monitor did not publish an initial inventory"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(response.res_error, ResError::NoAgentXError);
        let binding = response
            .vb
            .expect("GetNext response has bindings")
            .0
            .into_iter()
            .next()
            .expect("GetNext response has one binding");
        if binding.data == Value::EndOfMibView {
            return rows;
        }
        assert_eq!(binding.data, Value::Integer(1));
        start = binding.name.clone();
        rows.push(parse_row(&binding.name));
    }
    panic!("AgentX walk did not terminate");
}

fn parse_row(name: &ID) -> (u32, u32) {
    let components: Vec<u32> = name
        .to_string()
        .split('.')
        .map(|component| component.parse().expect("numeric OID component"))
        .collect();
    let status: Vec<u32> = STATUS
        .split('.')
        .map(|component| component.parse().expect("numeric status OID component"))
        .collect();
    assert_eq!(&components[..status.len()], status);
    assert_eq!(components.len(), status.len() + 2);
    (components[status.len()], components[status.len() + 1])
}

fn close(stream: &mut UnixStream, flags: u8, session_id: u32) {
    let mut request = pdu::Close::new(pdu::CloseReason::Shutdown);
    request.header = header(Type::Close, flags, session_id, 99);
    let response = exchange(stream, &request.to_bytes().expect("encode Close request"));
    assert_eq!(response.res_error, ResError::NoAgentXError);
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).expect("read closed socket"), 0);
}

fn wait_for_value(
    stream: &mut UnixStream,
    flags: u8,
    session_id: u32,
    row: (u32, u32),
    expected: Value,
) {
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        let actual = get_value(stream, flags, session_id, 800, row);
        if actual == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "row {row:?} remained {actual:?}, expected {expected:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn expected_boundaries(
    indices: impl IntoIterator<Item = u32>,
    relationships: &BTreeSet<(u32, u32)>,
) -> BTreeSet<(u32, u32)> {
    let higher: BTreeSet<_> = relationships.iter().map(|&(index, _)| index).collect();
    let lower: BTreeSet<_> = relationships.iter().map(|&(_, index)| index).collect();
    indices
        .into_iter()
        .flat_map(|index| {
            [
                (!lower.contains(&index)).then_some((0, index)),
                (!higher.contains(&index)).then_some((index, 0)),
            ]
        })
        .flatten()
        .collect()
}

fn wire_oid(n_subid: u8, prefix: u8) -> Vec<u8> {
    let mut bytes = vec![n_subid, prefix, 0, 0];
    bytes.extend(vec![255; usize::from(n_subid) * 4]);
    bytes
}

struct OidLimitSession<'a> {
    master: &'a mut Master,
    stream: &'a mut UnixStream,
    flags: u8,
    valid_row: (u32, u32),
}

impl OidLimitSession<'_> {
    fn check(&mut self, ty: Type, at_end: bool, n_subid: u8, prefix: u8) {
        let flags = self.flags;
        let valid_row = self.valid_row;
        let order = if flags == 0 {
            agentx::ByteOrder::LittleEndian
        } else {
            agentx::ByteOrder::BigEndian
        };
        let oversized = wire_oid(n_subid, prefix);
        let name = row_oid(valid_row);
        let mut payload = Vec::new();
        if ty == Type::TestSet {
            payload.extend(
                VarBind::new(name.clone(), Value::Integer(2))
                    .to_bytes(&order)
                    .expect("encode valid TestSet binding"),
            );
            let value_type: u16 = if at_end { 6 } else { 5 };
            payload.extend(if flags == 0 {
                value_type.to_le_bytes()
            } else {
                value_type.to_be_bytes()
            });
            payload.extend([0, 0]);
            if at_end {
                payload.extend(name.to_bytes(&order));
            }
            payload.extend(oversized);
        } else {
            if ty == Type::GetBulk {
                payload.extend(if flags == 0 {
                    [0, 0, 1, 0]
                } else {
                    [0, 0, 0, 1]
                });
            }
            payload.extend(range(name.clone()).to_bytes(&order));
            if at_end {
                payload.extend(name.to_bytes(&order));
                payload.extend(oversized);
            } else {
                payload.extend(oversized);
                payload.extend([0; 4]);
            }
        }
        let mut request = header(ty.clone(), flags, 100, 3);
        request.payload_length = payload.len().try_into().expect("test payload fits u32");
        let mut bytes = request.to_bytes();
        bytes.extend(payload);
        self.stream
            .write_all(&bytes)
            .expect("send raw AgentX request");
        let bytes = read_frame(self.stream);
        let error = if flags == 0 {
            u16::from_le_bytes(bytes[24..26].try_into().expect("response error field"))
        } else {
            u16::from_be_bytes(bytes[24..26].try_into().expect("response error field"))
        };
        let expanded_length = usize::from(n_subid) + if prefix == 0 { 0 } else { 5 };
        let expected_error = if expanded_length > 128 {
            266
        } else if ty == Type::TestSet {
            17
        } else {
            0
        };
        assert_eq!(
            error, expected_error,
            "{ty:?}, at_end={at_end}, length={expanded_length}"
        );
        if expected_error == 266 {
            let response = Response::from_bytes(&bytes).expect("decode parse-error response");
            assert_eq!(response.res_error, ResError::ParseError);
            assert_eq!(response.res_index, 0);
            assert!(response.vb.expect("parse-error binding list").is_empty());
        }
        assert_eq!(self.master.child.try_wait().expect("poll subagent"), None);
        assert_eq!(
            get_value(self.stream, flags, 100, 4, valid_row),
            Value::Integer(1)
        );
        assert_eq!(self.master.child.try_wait().expect("poll subagent"), None);
    }
}

#[test]
#[ignore = "requires root, iproute2, and network namespace permission"]
fn real_interfaces_produce_direct_and_boundary_rows() {
    enter_network_namespace();
    let indices = create_complete_topology();
    let relationships = BTreeSet::from([
        (indices["bond0"], indices["bondmember"]),
        (indices["vlan0"], indices["bond0"]),
        (indices["bridge0"], indices["bridgemember"]),
        (indices["macvlan0"], indices["lower0"]),
        (indices["ipvlan0"], indices["ipvlanlower"]),
        (indices["macvtap0"], indices["lower0"]),
        (indices["vxlan0"], indices["lower0"]),
    ]);
    let boundaries = expected_boundaries(indices.values().copied(), &relationships);

    let master = Master::start("", false);
    let mut stream = master.connect(NETWORK_ORDER, 100);
    master.assert_unprivileged();
    let (actual_relationships, actual_boundaries): (BTreeSet<_>, BTreeSet<_>) =
        walk(&mut stream, NETWORK_ORDER, 100, 10)
            .into_iter()
            .partition(|&(higher, lower)| higher != 0 && lower != 0);
    assert_eq!(actual_relationships, relationships);
    assert_eq!(actual_boundaries, boundaries);
    assert!(!actual_relationships.contains(&(indices["vlan0"], indices["bondmember"])));
    assert!(actual_boundaries.contains(&(0, indices["standalone"])));
    assert!(actual_boundaries.contains(&(indices["standalone"], 0)));
    assert!(actual_boundaries.contains(&(0, indices["veth0"])));
    assert!(actual_boundaries.contains(&(indices["veth1"], 0)));
}

#[test]
#[ignore = "requires root, iproute2, and network namespace permission"]
fn wire_protocol_uses_real_interface_rows_in_both_byte_orders() {
    enter_network_namespace();
    let indices = create_chain();
    let target = (indices["bond0"], indices["bondmember"]);

    for flags in [0, NETWORK_ORDER] {
        let master = Master::start("", false);
        let session_id = 0x01020304;
        let mut stream = master.connect(flags, session_id);
        let all_rows = walk(&mut stream, flags, session_id, 20);
        let first = all_rows[0];
        let last = *all_rows.last().expect("real topology has rows");

        let mut next = pdu::GetNext::new(range(oid("")));
        next.header = header(Type::GetNext, flags, session_id, 3);
        assert_eq!(
            exchange(
                &mut stream,
                &next.to_bytes().expect("encode GetNext request")
            )
            .vb
            .expect("GetNext response bindings")
            .0,
            [VarBind::new(row_oid(first), Value::Integer(1))]
        );

        let mut get = pdu::Get::new(SearchRangeList(vec![
            SearchRange::new(row_oid(target), ID::default()),
            SearchRange::new(row_oid((u32::MAX, u32::MAX)), ID::default()),
            SearchRange::new(
                ID::from_str("1.3.6.1.2.1.31.1.2.1.4").expect("valid following OID"),
                ID::default(),
            ),
        ]));
        get.header = header(Type::Get, flags ^ NETWORK_ORDER, session_id, 4);
        assert_eq!(
            exchange(&mut stream, &get.to_bytes().expect("encode Get request"))
                .vb
                .expect("Get response bindings")
                .0
                .iter()
                .map(|binding| binding.data.clone())
                .collect::<Vec<_>>(),
            [
                Value::Integer(1),
                Value::NoSuchInstance,
                Value::NoSuchObject
            ]
        );

        next.sr = range(row_oid(last));
        next.header.packet_id = 5;
        assert_eq!(
            exchange(
                &mut stream,
                &next.to_bytes().expect("encode terminating GetNext request")
            )
            .vb
            .expect("terminating GetNext bindings")
            .0,
            [VarBind::new(row_oid(last), Value::EndOfMibView)]
        );

        let mut bulk = pdu::GetBulk::new(range(oid("")));
        bulk.header = header(Type::GetBulk, flags, session_id, 6);
        bulk.max_repetitions = 32;
        let bulk_bindings = exchange(
            &mut stream,
            &bulk.to_bytes().expect("encode GetBulk request"),
        )
        .vb
        .expect("GetBulk response bindings")
        .0;
        let expected_bulk: Vec<_> = all_rows
            .iter()
            .copied()
            .map(|row| VarBind::new(row_oid(row), Value::Integer(1)))
            .chain([VarBind::new(row_oid(last), Value::EndOfMibView)])
            .collect();
        assert_eq!(bulk_bindings, expected_bulk);

        let mut set = pdu::TestSet::new(VarBindList(vec![VarBind::new(
            row_oid(target),
            Value::Integer(2),
        )]));
        set.header = header(Type::TestSet, flags, session_id, 7);
        stream
            .write_all(&set.to_bytes().expect("encode TestSet request"))
            .expect("send TestSet request");
        let bytes = read_frame(&mut stream);
        assert_eq!(
            Header::from_bytes(&bytes)
                .expect("decode response")
                .packet_id,
            7
        );
        assert_eq!(bytes.len(), 28);
        assert_eq!(
            &bytes[24..28],
            if flags == 0 {
                &[17, 0, 1, 0]
            } else {
                &[0, 17, 0, 1]
            }
        );

        let cleanup = header(Type::CleanupSet, flags, session_id, 8);
        stream
            .write_all(&cleanup.to_bytes())
            .expect("send CleanupSet request");
        get.header.packet_id = 9;
        assert_eq!(
            exchange(&mut stream, &get.to_bytes().expect("encode Get request"))
                .vb
                .expect("Get response bindings")
                .0[0]
                .data,
            Value::Integer(1)
        );

        let malformed = header(Type::GetBulk, flags, session_id, 10);
        assert_eq!(
            exchange(&mut stream, &malformed.to_bytes()).res_error,
            ResError::ParseError
        );
        get.context = Some(Context(OctetString("unregistered".into())));
        get.header.flags = flags | (1 << pdu::NON_DEFAULT_CONTEXT);
        get.header.packet_id = 11;
        assert_eq!(
            exchange(&mut stream, &get.to_bytes().expect("encode contextual Get")).res_error,
            ResError::UnsupportedContext
        );
        close(&mut stream, flags, session_id);
    }
}

#[test]
#[ignore = "requires root, iproute2, and network namespace permission"]
fn topology_changes_eventually_reach_get_and_walk() {
    enter_network_namespace();
    ip(&["link", "add", "member0", "type", "dummy"]);
    let member = interface_indices()["member0"];
    let master = Master::start("reconcile = 3600\n", false);
    let mut stream = master.connect(NETWORK_ORDER, 100);
    assert_eq!(
        get_value(&mut stream, NETWORK_ORDER, 100, 3, (0, member)),
        Value::Integer(1)
    );

    ip(&["link", "add", "created0", "type", "dummy"]);
    let created = interface_indices()["created0"];
    std::thread::sleep(Duration::from_millis(1250));
    assert_eq!(
        get_value(&mut stream, NETWORK_ORDER, 100, 4, (0, created)),
        Value::Integer(1)
    );
    let rows = walk(&mut stream, NETWORK_ORDER, 100, 20);
    assert!(rows.contains(&(0, created)));
    assert!(rows.contains(&(created, 0)));

    ip(&["link", "add", "bond0", "type", "bond"]);
    ip(&["link", "set", "member0", "master", "bond0"]);
    let bond = interface_indices()["bond0"];
    wait_for_value(
        &mut stream,
        NETWORK_ORDER,
        100,
        (bond, member),
        Value::Integer(1),
    );
    assert!(walk(&mut stream, NETWORK_ORDER, 100, 40).contains(&(bond, member)));

    ip(&["link", "set", "member0", "nomaster"]);
    wait_for_value(
        &mut stream,
        NETWORK_ORDER,
        100,
        (bond, member),
        Value::NoSuchInstance,
    );
    let rows = walk(&mut stream, NETWORK_ORDER, 100, 60);
    assert!(!rows.contains(&(bond, member)));
    assert!(rows.contains(&(0, member)));
    assert!(rows.contains(&(member, 0)));

    ip(&["link", "delete", "member0"]);
    wait_for_value(
        &mut stream,
        NETWORK_ORDER,
        100,
        (0, member),
        Value::NoSuchInstance,
    );
    assert!(
        !walk(&mut stream, NETWORK_ORDER, 100, 80)
            .iter()
            .any(|&(higher, lower)| higher == member || lower == member)
    );
}

#[test]
#[ignore = "requires root, iproute2, and network namespace permission"]
fn process_reregisters_after_close_and_socket_loss() {
    enter_network_namespace();
    let indices = create_chain();
    let relationship = (indices["bond0"], indices["bondmember"]);
    let master = Master::start("reconcile = 3600\n", false);

    let mut first = master.connect(0, 100);
    assert_eq!(
        get_value(&mut first, 0, 100, 3, relationship),
        Value::Integer(1)
    );
    close(&mut first, 0, 100);

    let second = master.connect(NETWORK_ORDER, 200);
    drop(second);
    ip(&["link", "set", "bondmember", "nomaster"]);

    let mut third = master.connect(0, 300);
    wait_for_value(&mut third, 0, 300, relationship, Value::NoSuchInstance);
    assert!(!walk(&mut third, 0, 300, 20).contains(&relationship));
    close(&mut third, 0, 300);
}

#[test]
#[ignore = "requires root, iproute2, and network namespace permission"]
fn repeated_events_and_agentx_reconnects_keep_resources_bounded() {
    enter_network_namespace();
    // This test must share an identity with the daemon to inspect /proc without
    // adding CAP_SYS_PTRACE to the hardened CI container. Other tests prove the
    // production binary runs under an unprivileged identity.
    let master = Master::start_as_test_process("reconcile = 3600\n");
    let mut stream = master.connect(NETWORK_ORDER, 100);
    let loopback = interface_indices()["lo"];
    assert_eq!(
        get_value(&mut stream, NETWORK_ORDER, 100, 3, (0, loopback)),
        Value::Integer(1)
    );
    let baseline = master.resource_counts();
    let mut observed = Vec::new();

    for cycle in 0..3 {
        let name = format!("change{cycle}");
        ip(&["link", "add", &name, "type", "dummy"]);
        let index = interface_indices()[&name];
        wait_for_value(
            &mut stream,
            NETWORK_ORDER,
            100 + cycle,
            (0, index),
            Value::Integer(1),
        );
        close(&mut stream, NETWORK_ORDER, 100 + cycle);
        stream = master.connect(NETWORK_ORDER, 101 + cycle);
        observed.push(master.resource_counts());
    }

    assert!(
        observed
            .iter()
            .all(|&(threads, fds)| threads <= baseline.0 + 1 && fds <= baseline.1 + 2),
        "resource growth exceeded bounds: baseline={baseline:?}, observed={observed:?}"
    );
    close(&mut stream, NETWORK_ORDER, 103);
}

#[test]
#[ignore = "requires root, iproute2, and network namespace permission"]
fn configuration_controls_registration_and_socket_selection() {
    enter_network_namespace();
    let loopback = interface_indices()["lo"];

    let configured = Master::start("priority = 42\n", false);
    let mut stream = configured.agentx.connect(NETWORK_ORDER, 100, 42);
    assert_eq!(
        get_value(&mut stream, NETWORK_ORDER, 100, 3, (0, loopback)),
        Value::Integer(1)
    );
    close(&mut stream, NETWORK_ORDER, 100);
    drop(configured);

    let overridden = Master::start("", true);
    let mut stream = overridden.connect(NETWORK_ORDER, 200);
    assert_eq!(
        get_value(&mut stream, NETWORK_ORDER, 200, 3, (0, loopback)),
        Value::Integer(1)
    );
    close(&mut stream, NETWORK_ORDER, 200);
}

#[test]
#[ignore = "requires root, iproute2, and network namespace permission"]
fn request_oid_limits_preserve_the_session() {
    enter_network_namespace();
    let loopback = interface_indices()["lo"];
    let valid_row = (0, loopback);

    for flags in [0, NETWORK_ORDER] {
        let mut master = Master::start("", false);
        let mut stream = master.connect(flags, 100);
        {
            let mut session = OidLimitSession {
                master: &mut master,
                stream: &mut stream,
                flags,
                valid_row,
            };

            session.check(Type::GetNext, false, 255, 2);
            for ty in [Type::Get, Type::GetNext, Type::GetBulk] {
                for (n_subid, prefix) in [(129, 0), (124, 2)] {
                    session.check(ty.clone(), false, n_subid, prefix);
                }
                for (n_subid, prefix) in [(129, 0), (124, 2), (255, 2)] {
                    session.check(ty.clone(), true, n_subid, prefix);
                }
            }
            for (n_subid, prefix) in [(129, 0), (124, 2), (255, 2)] {
                session.check(Type::TestSet, false, n_subid, prefix);
                session.check(Type::TestSet, true, n_subid, prefix);
            }
            for ty in [Type::Get, Type::GetNext, Type::GetBulk, Type::TestSet] {
                for at_end in [false, true] {
                    for (n_subid, prefix) in [(128, 0), (123, 2)] {
                        session.check(ty.clone(), at_end, n_subid, prefix);
                    }
                }
            }
        }
        close(&mut stream, flags, 100);
    }
}
