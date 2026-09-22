use std::fs;
use std::io::{Read, Write};
use std::os::unix::{fs::PermissionsExt, net::UnixListener, net::UnixStream};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant};

use agentx::encodings::ID;
use agentx::pdu::{self, Header, Response, Type};

pub const NETWORK_ORDER: u8 = 1 << pdu::NETWORK_BYTE_ORDER;

pub struct AgentxMaster {
    listener: UnixListener,
}

impl AgentxMaster {
    pub fn bind(socket: &Path) -> Self {
        let listener = UnixListener::bind(socket).expect("bind AgentX test socket");
        listener
            .set_nonblocking(true)
            .expect("make AgentX test listener nonblocking");
        fs::set_permissions(socket, fs::Permissions::from_mode(0o777))
            .expect("make AgentX test socket writable by the subagent");
        Self { listener }
    }

    pub fn connect(&self, flags: u8, session_id: u32, priority: u8) -> UnixStream {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "subagent did not connect");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept AgentX connection: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set AgentX read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set AgentX write timeout");

        let open = pdu::Open::from_bytes(&read_frame(&mut stream)).expect("decode Open PDU");
        assert_eq!(open.header.ty, Type::Open);
        assert_eq!(open.header.session_id, 0);
        assert_eq!(open.header.flags, NETWORK_ORDER);
        assert_eq!(open.descr.0, "agentx-ifstack");
        let mut response = Response::from_header(&open.header);
        response.header.session_id = session_id;
        response.header.flags = flags;
        let mut bytes = response.to_bytes().expect("encode Open response");
        bytes[20..24].copy_from_slice(&[255; 4]);
        for chunk in bytes.chunks(3) {
            stream.write_all(chunk).expect("send Open response");
        }

        let register =
            pdu::Register::from_bytes(&read_frame(&mut stream)).expect("decode Register PDU");
        assert_eq!(register.header.ty, Type::Register);
        assert_eq!(register.header.session_id, session_id);
        assert_eq!(register.header.flags, flags);
        assert_eq!(
            register.subtree,
            ID::from_str("1.3.6.1.2.1.31.1.2").expect("valid table OID")
        );
        assert_eq!(register.priority, priority);
        assert_eq!(register.range_subid, 0);
        assert_eq!(register.context, None);
        let mut response = Response::from_header(&register.header);
        response.header.flags = flags;
        stream
            .write_all(&response.to_bytes().expect("encode Register response"))
            .expect("send Register response");
        stream
    }
}

pub fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut bytes = vec![0; 20];
    stream.read_exact(&mut bytes).expect("read AgentX header");
    let header = Header::from_bytes(&bytes).expect("decode AgentX header");
    assert!(header.payload_length < 1024 * 1024);
    bytes.resize(20 + header.payload_length as usize, 0);
    stream
        .read_exact(&mut bytes[20..])
        .expect("read AgentX payload");
    bytes
}
