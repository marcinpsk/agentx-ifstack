use std::io::Write;
use std::os::unix::net::UnixStream;
use std::str::FromStr;

use agentx::encodings::{ID, SearchRange, SearchRangeList};
use agentx::pdu::{Header, Response, Type};

use crate::master::{NETWORK_ORDER, read_frame};

pub const STATUS: &str = "1.3.6.1.2.1.31.1.2.1.3";

pub fn header(ty: Type, flags: u8, session_id: u32, packet_id: u32) -> Header {
    let mut header = Header::new(ty);
    header.flags = flags;
    header.session_id = session_id;
    header.transaction_id = 0x12345678;
    header.packet_id = packet_id;
    header
}

pub fn exchange(stream: &mut UnixStream, bytes: &[u8]) -> Response {
    let request = Header::from_bytes(bytes).expect("decode request header");
    stream.write_all(bytes).expect("send AgentX request");
    let response = Response::from_bytes(&read_frame(stream)).expect("decode AgentX response");
    assert_eq!(response.header.ty, Type::Response);
    assert_eq!(response.header.flags, request.flags & NETWORK_ORDER);
    assert_eq!(response.header.session_id, request.session_id);
    assert_eq!(response.header.transaction_id, request.transaction_id);
    assert_eq!(response.header.packet_id, request.packet_id);
    response
}

pub fn oid(suffix: &str) -> ID {
    ID::from_str(&format!("{STATUS}{suffix}")).expect("valid test OID")
}

pub fn range(name: ID) -> SearchRangeList {
    SearchRangeList(vec![SearchRange::new(name, ID::default())])
}
