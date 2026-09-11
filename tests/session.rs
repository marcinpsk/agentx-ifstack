use std::fs;
use std::io::{Read, Write};
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use agentx::encodings::{
    Context, ID, OctetString, SearchRange, SearchRangeList, Value, VarBind, VarBindList,
};
use agentx::pdu::{self, Header, ResError, Response, Type};

static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);
const NETWORK_ORDER: u8 = 1 << pdu::NETWORK_BYTE_ORDER;

const IP_SERVES_LINKS: &str = "#!/bin/sh\n[ \"$*\" = '-details -json link show' ] || exit 2\nexec /bin/cat \"$IFSTACK_TEST_LINKS\"\n";
const IP_NEVER_EXITS: &str = "#!/bin/sh\nexec /bin/sleep 600\n";
// Exits at once, but a descendant keeps the inherited stdout pipe open.
const IP_LEAKS_ITS_PIPE: &str =
    "#!/bin/sh\n/bin/sleep 600 &\nexec /bin/cat \"$IFSTACK_TEST_LINKS\"\n";

fn write_ip(directory: &std::path::Path, script: &str) {
    let ip = directory.join("ip");
    fs::write(&ip, script).unwrap();
    fs::set_permissions(&ip, fs::Permissions::from_mode(0o700)).unwrap();
}

struct Master {
    directory: PathBuf,
    listener: UnixListener,
    child: Child,
}

impl Master {
    fn start() -> Self {
        Self::start_with_config(Some(""), true)
    }

    fn start_with_config(config: Option<&str>, socket_override: bool) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "ifstack-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let socket = directory.join("master");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        fs::write(
            directory.join("links.json"),
            include_str!("fixtures/bond.json"),
        )
        .unwrap();
        write_ip(&directory, IP_SERVES_LINKS);
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentx-ifstack"));
        if let Some(config) = config {
            let configured_socket = if socket_override {
                directory.join("unused")
            } else {
                socket.clone()
            };
            let path = directory.join("config.toml");
            fs::write(&path, format!("socket = {configured_socket:?}\n{config}")).unwrap();
            command.arg("--config").arg(path);
        }
        if socket_override {
            command.arg("--socket").arg(&socket);
        }
        let child = command
            .env("PATH", &directory)
            .env("IFSTACK_TEST_LINKS", directory.join("links.json"))
            .spawn()
            .unwrap();
        Self {
            directory,
            listener,
            child,
        }
    }

    fn connect(&self, flags: u8, session_id: u32) -> UnixStream {
        self.connect_with_priority(flags, session_id, 127)
    }

    fn connect_with_priority(&self, flags: u8, session_id: u32, priority: u8) -> UnixStream {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "subagent did not connect");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let open = pdu::Open::from_bytes(&read_frame(&mut stream)).unwrap();
        assert_eq!(open.header.ty, Type::Open);
        assert_eq!(open.header.session_id, 0);
        assert_eq!(open.header.flags, NETWORK_ORDER);
        assert_eq!(open.descr.0, "agentx-ifstack");
        let mut response = Response::from_header(&open.header);
        response.header.session_id = session_id;
        response.header.flags = flags;
        let mut bytes = response.to_bytes().unwrap();
        bytes[20..24].copy_from_slice(&[255; 4]);
        for chunk in bytes.chunks(3) {
            stream.write_all(chunk).unwrap();
        }
        let register = pdu::Register::from_bytes(&read_frame(&mut stream)).unwrap();
        assert_eq!(register.header.ty, Type::Register);
        assert_eq!(register.header.session_id, session_id);
        assert_eq!(register.header.flags, flags);
        assert_eq!(
            register.subtree,
            ID::from_str("1.3.6.1.2.1.31.1.2").unwrap()
        );
        assert_eq!(register.priority, priority);
        assert_eq!(register.range_subid, 0);
        assert_eq!(register.context, None);
        let mut response = Response::from_header(&register.header);
        response.header.flags = flags;
        stream.write_all(&response.to_bytes().unwrap()).unwrap();
        stream
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
}

impl Drop for Master {
    fn drop(&mut self) {
        self.child.kill().expect("stop test subagent");
        self.child.wait().expect("reap test subagent");
        fs::remove_dir_all(&self.directory).expect("remove test files");
    }
}

fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut bytes = vec![0; 20];
    stream.read_exact(&mut bytes).unwrap();
    let header = Header::from_bytes(&bytes).unwrap();
    assert!(header.payload_length < 1024 * 1024);
    bytes.resize(20 + header.payload_length as usize, 0);
    stream.read_exact(&mut bytes[20..]).unwrap();
    bytes
}

fn header(ty: Type, flags: u8, session_id: u32, packet_id: u32) -> Header {
    let mut header = Header::new(ty);
    header.flags = flags;
    header.session_id = session_id;
    header.transaction_id = 0x12345678;
    header.packet_id = packet_id;
    header
}

fn exchange(stream: &mut UnixStream, bytes: &[u8]) -> Response {
    let request = Header::from_bytes(bytes).unwrap();
    stream.write_all(bytes).unwrap();
    let bytes = read_frame(stream);
    let response = Response::from_bytes(&bytes).unwrap();
    assert_eq!(response.header.ty, Type::Response);
    assert_eq!(response.header.flags, request.flags & NETWORK_ORDER);
    assert_eq!(response.header.session_id, request.session_id);
    assert_eq!(response.header.transaction_id, request.transaction_id);
    assert_eq!(response.header.packet_id, request.packet_id);
    response
}

fn oid(suffix: &str) -> ID {
    ID::from_str(&format!("1.3.6.1.2.1.31.1.2.1.3{suffix}")).unwrap()
}

fn ranges(suffix: &str) -> SearchRangeList {
    SearchRangeList(vec![SearchRange::new(oid(suffix), ID::default())])
}

fn close(stream: &mut UnixStream, flags: u8, session_id: u32) {
    let mut request = pdu::Close::new(pdu::CloseReason::Shutdown);
    request.header = header(Type::Close, flags, session_id, 99);
    let response = exchange(stream, &request.to_bytes().unwrap());
    assert_eq!(response.res_error, ResError::NoAgentXError);
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).unwrap(), 0);
}

fn wire_oid(n_subid: u8, prefix: u8) -> Vec<u8> {
    let mut bytes = vec![n_subid, prefix, 0, 0];
    bytes.extend(vec![255; usize::from(n_subid) * 4]);
    bytes
}

fn check_request_oid_limit(ty: Type, at_end: bool, n_subid: u8, prefix: u8) {
    for flags in [0, NETWORK_ORDER] {
        let mut master = Master::start();
        let mut stream = master.connect(flags, 100);
        let order = if flags == 0 {
            agentx::ByteOrder::LittleEndian
        } else {
            agentx::ByteOrder::BigEndian
        };
        let name = wire_oid(n_subid, prefix);
        let mut payload = Vec::new();
        if ty == Type::TestSet {
            payload.extend(
                VarBind::new(oid(".10.2"), Value::Integer(2))
                    .to_bytes(&order)
                    .unwrap(),
            );
            let value_type: u16 = if at_end { 6 } else { 5 };
            payload.extend(if flags == 0 {
                value_type.to_le_bytes()
            } else {
                value_type.to_be_bytes()
            });
            payload.extend([0, 0]);
            if at_end {
                payload.extend(oid(".10.2").to_bytes(&order));
            }
            payload.extend(name);
        } else {
            if ty == Type::GetBulk {
                payload.extend(if flags == 0 {
                    [0, 0, 1, 0]
                } else {
                    [0, 0, 0, 1]
                });
            }
            payload.extend(ranges(".10.2").to_bytes(&order));
            if at_end {
                payload.extend(oid(".10.2").to_bytes(&order));
                payload.extend(name);
            } else {
                payload.extend(name);
                payload.extend([0; 4]);
            }
        }
        let mut request = header(ty.clone(), flags, 100, 3);
        request.payload_length = payload.len().try_into().unwrap();
        let mut bytes = request.to_bytes();
        bytes.extend(payload);
        stream.write_all(&bytes).unwrap();
        let bytes = read_frame(&mut stream);
        let error = if flags == 0 {
            u16::from_le_bytes(bytes[24..26].try_into().unwrap())
        } else {
            u16::from_be_bytes(bytes[24..26].try_into().unwrap())
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
            let response = Response::from_bytes(&bytes).unwrap();
            assert_eq!(response.res_error, ResError::ParseError);
            assert_eq!(response.res_index, 0);
            assert!(response.vb.unwrap().is_empty());
        }
        assert_eq!(master.child.try_wait().unwrap(), None);
        let mut get = pdu::Get::new(ranges(".10.2"));
        get.header = header(Type::Get, flags, 100, 4);
        let response = exchange(&mut stream, &get.to_bytes().unwrap());
        assert_eq!(response.res_error, ResError::NoAgentXError);
        assert_eq!(
            response.vb.unwrap().0,
            [VarBind::new(oid(".10.2"), Value::Integer(1))]
        );
        assert_eq!(master.child.try_wait().unwrap(), None);
    }
}

#[test]
fn getnext_rejects_255_subids_with_prefix_and_process_keeps_serving() {
    check_request_oid_limit(Type::GetNext, false, 255, 2);
}

#[test]
fn requests_reject_search_range_starts_over_128_subids() {
    for ty in [Type::Get, Type::GetNext, Type::GetBulk] {
        for (n_subid, prefix) in [(129, 0), (124, 2)] {
            check_request_oid_limit(ty.clone(), false, n_subid, prefix);
        }
    }
}

#[test]
fn requests_reject_search_range_ends_over_128_subids() {
    for ty in [Type::Get, Type::GetNext, Type::GetBulk] {
        for (n_subid, prefix) in [(129, 0), (124, 2), (255, 2)] {
            check_request_oid_limit(ty.clone(), true, n_subid, prefix);
        }
    }
}

#[test]
fn testset_rejects_names_over_128_subids() {
    for (n_subid, prefix) in [(129, 0), (124, 2), (255, 2)] {
        check_request_oid_limit(Type::TestSet, false, n_subid, prefix);
    }
}

#[test]
fn testset_rejects_oid_values_over_128_subids() {
    for (n_subid, prefix) in [(129, 0), (124, 2), (255, 2)] {
        check_request_oid_limit(Type::TestSet, true, n_subid, prefix);
    }
}

#[test]
fn requests_accept_128_subids_with_or_without_prefix() {
    for ty in [Type::Get, Type::GetNext, Type::GetBulk, Type::TestSet] {
        for at_end in [false, true] {
            for (n_subid, prefix) in [(128, 0), (123, 2)] {
                check_request_oid_limit(ty.clone(), at_end, n_subid, prefix);
            }
        }
    }
}

#[test]
fn unix_session_serves_wire_requests_in_both_byte_orders() {
    for flags in [0, NETWORK_ORDER] {
        let master = Master::start();
        let session_id = 0x01020304;
        let mut stream = master.connect(flags, session_id);
        let mut next = pdu::GetNext::new(ranges(".10.1"));
        next.header = header(Type::GetNext, flags, session_id, 3);
        let response = exchange(&mut stream, &next.to_bytes().unwrap());
        assert_eq!(response.res_error, ResError::NoAgentXError);
        assert_eq!(
            response.vb.unwrap().0,
            [VarBind::new(oid(".10.2"), Value::Integer(1))]
        );

        let mut get = pdu::Get::new(SearchRangeList(vec![
            SearchRange::new(oid(".10.2"), ID::default()),
            SearchRange::new(oid(".10.9"), ID::default()),
            SearchRange::new(
                ID::from_str("1.3.6.1.2.1.31.1.2.1.4").unwrap(),
                ID::default(),
            ),
        ]));
        get.header = header(Type::Get, flags ^ NETWORK_ORDER, session_id, 4);
        let response = exchange(&mut stream, &get.to_bytes().unwrap());
        assert_eq!(
            response
                .vb
                .unwrap()
                .0
                .iter()
                .map(|vb| vb.data.clone())
                .collect::<Vec<_>>(),
            [
                Value::Integer(1),
                Value::NoSuchInstance,
                Value::NoSuchObject
            ]
        );

        next.sr = ranges(".10.3");
        next.header.packet_id = 5;
        let response = exchange(&mut stream, &next.to_bytes().unwrap());
        assert_eq!(
            response.vb.unwrap().0,
            [VarBind::new(oid(".10.3"), Value::EndOfMibView)]
        );

        let mut bulk = pdu::GetBulk::new(ranges(".10.1"));
        bulk.header = header(Type::GetBulk, flags, session_id, 6);
        bulk.max_repetitions = 10;
        let response = exchange(&mut stream, &bulk.to_bytes().unwrap());
        assert_eq!(
            response.vb.unwrap().0,
            [
                VarBind::new(oid(".10.2"), Value::Integer(1)),
                VarBind::new(oid(".10.3"), Value::Integer(1)),
                VarBind::new(oid(".10.3"), Value::EndOfMibView),
            ]
        );

        let mut set = pdu::TestSet::new(VarBindList(vec![VarBind::new(
            oid(".10.2"),
            Value::Integer(2),
        )]));
        set.header = header(Type::TestSet, flags, session_id, 7);
        stream.write_all(&set.to_bytes().unwrap()).unwrap();
        let bytes = read_frame(&mut stream);
        assert_eq!(Header::from_bytes(&bytes).unwrap().packet_id, 7);
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
        stream.write_all(&cleanup.to_bytes()).unwrap();
        get.header.packet_id = 9;
        let response = exchange(&mut stream, &get.to_bytes().unwrap());
        assert_eq!(response.vb.unwrap().0[0].data, Value::Integer(1));

        let malformed = header(Type::GetBulk, flags, session_id, 10);
        assert_eq!(
            exchange(&mut stream, &malformed.to_bytes()).res_error,
            ResError::ParseError
        );
        get.context = Some(Context(OctetString("unregistered".into())));
        get.header.flags = flags | (1 << pdu::NON_DEFAULT_CONTEXT);
        get.header.packet_id = 11;
        assert_eq!(
            exchange(&mut stream, &get.to_bytes().unwrap()).res_error,
            ResError::UnsupportedContext
        );
        close(&mut stream, flags, session_id);
    }
}

#[test]
fn process_reopens_and_reregisters_after_close_and_socket_loss() {
    let master = Master::start();
    let mut first = master.connect(0, 100);
    close(&mut first, 0, 100);
    let second = master.connect(NETWORK_ORDER, 200);
    drop(second);
    let mut third = master.connect(0, 300);
    let mut next = pdu::GetNext::new(ranges(".10.1"));
    next.header = header(Type::GetNext, 0, 300, 3);
    let response = exchange(&mut third, &next.to_bytes().unwrap());
    assert_eq!(
        response.vb.unwrap().0,
        [VarBind::new(oid(".10.2"), Value::Integer(1))]
    );
    close(&mut third, 0, 300);
}

#[test]
fn cache_refresh_changes_wire_values_and_reports_invalid_output() {
    let master = Master::start();
    let mut stream = master.connect(NETWORK_ORDER, 100);
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    assert_eq!(
        exchange(&mut stream, &get.to_bytes().unwrap())
            .vb
            .unwrap()
            .0[0]
            .data,
        Value::Integer(1)
    );
    master.topology("not JSON");
    assert_eq!(
        exchange(&mut stream, &get.to_bytes().unwrap())
            .vb
            .unwrap()
            .0[0]
            .data,
        Value::Integer(1)
    );
    std::thread::sleep(Duration::from_millis(5100));
    let error = exchange(&mut stream, &get.to_bytes().unwrap());
    assert_eq!(error.res_error, ResError::ProcessingError);
    assert!(error.vb.unwrap().is_empty());
    fs::remove_file(master.directory.join("links.json")).unwrap();
    assert_eq!(
        exchange(&mut stream, &get.to_bytes().unwrap()).res_error,
        ResError::ProcessingError
    );
    master.topology(include_str!("fixtures/plain.json"));
    assert_eq!(
        exchange(&mut stream, &get.to_bytes().unwrap())
            .vb
            .unwrap()
            .0[0]
            .data,
        Value::NoSuchInstance
    );
    get.sr = ranges(".0.1");
    assert_eq!(
        exchange(&mut stream, &get.to_bytes().unwrap())
            .vb
            .unwrap()
            .0[0]
            .data,
        Value::Integer(1)
    );
    close(&mut stream, NETWORK_ORDER, 100);
}

#[test]
fn config_drives_socket_registration_and_cache_window_on_wire() {
    let master = Master::start_with_config(Some("refresh = 1\npriority = 42\n"), false);
    let mut stream = master.connect_with_priority(NETWORK_ORDER, 100, 42);
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
    let mut value = || {
        exchange(&mut stream, &get.to_bytes().unwrap())
            .vb
            .unwrap()
            .0[0]
            .data
            .clone()
    };
    assert_eq!(value(), Value::Integer(1));
    master.topology(include_str!("fixtures/plain.json"));
    assert_eq!(value(), Value::Integer(1));
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(value(), Value::NoSuchInstance);
}

#[test]
fn cli_socket_overrides_config_socket_on_wire() {
    let master = Master::start_with_config(Some(""), true);
    let mut stream = master.connect(NETWORK_ORDER, 100);
    let mut get = pdu::Get::new(ranges(".10.2"));
    get.header = header(Type::Get, NETWORK_ORDER, 100, 3);
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
fn a_stalled_ip_does_not_wedge_the_session() {
    let master = Master::start_with_config(Some("refresh = 1\n"), false);
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
    let master = Master::start_with_config(Some("refresh = 1\n"), false);
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
