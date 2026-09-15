use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use agentx::ByteOrder;
use agentx::encodings::{ID, Value, VarBindList};
use agentx::pdu::{self, Header, ResError, Response, Type};

use crate::{config::Config, mib::TABLE, monitor::TableReader};

const NETWORK_ORDER: u8 = 1 << pdu::NETWORK_BYTE_ORDER;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PAYLOAD: u32 = 1024 * 1024;
const MAX_OID_SUBIDS: usize = 128;
const NOT_WRITABLE: u16 = 17;
const COMMIT_FAILED: u16 = 14;
const UNDO_FAILED: u16 = 15;

pub fn run(config: &Config, tables: &TableReader) -> Result<()> {
    log::info!("Connecting to AgentX master at {}", config.socket.display());
    let mut stream = UnixStream::connect(&config.socket)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut open = pdu::Open::new(ID::default(), "agentx-ifstack");
    open.timeout = IO_TIMEOUT;
    open.header.flags = NETWORK_ORDER;
    open.header.packet_id = 1;
    stream.write_all(&open.to_bytes()?)?;
    let opened = acknowledge(&mut stream, &open.header)?;

    let mut register = pdu::Register::new(ID::try_from(TABLE.to_vec())?);
    register.header.session_id = opened.session_id;
    register.header.flags = opened.flags & NETWORK_ORDER;
    register.header.packet_id = 2;
    register.priority = config.priority;
    stream.write_all(&register.to_bytes()?)?;
    acknowledge(&mut stream, &register.header)?;
    stream.set_read_timeout(None)?;
    log::info!(
        "AgentX session {} registered ifStackTable",
        opened.session_id
    );

    loop {
        let (header, bytes) = receive(&mut stream, IO_TIMEOUT)?;
        log::debug!(
            "AgentX request {:?}, packet {}",
            header.ty,
            header.packet_id
        );
        if header.session_id != opened.session_id {
            let mut response = reply(&header);
            response.res_error = ResError::NotOpen;
            stream.write_all(&response.to_bytes()?)?;
            continue;
        }
        if header.ty == Type::Close {
            pdu::Close::from_bytes(&bytes)?;
            stream.write_all(&reply(&header).to_bytes()?)?;
            return Ok(());
        }
        if header.ty == Type::CleanupSet {
            if header.payload_length != 0 {
                return Err(invalid("CleanupSet has a payload"));
            }
            continue;
        }
        let (mut response, snmp_error) = match dispatch(&header, &bytes, tables) {
            Ok(result) => result,
            Err(error) => {
                log::warn!("AgentX request parse failed: {error}");
                let mut response = reply(&header);
                response.res_error = ResError::ParseError;
                (response, None)
            }
        };
        let mut bytes = response.to_bytes()?;
        if let Some(error) = snmp_error {
            // agentx 0.1.1 omits SNMP error codes from ResError.
            let encoded = if response.header.flags & NETWORK_ORDER != 0 {
                error.to_be_bytes()
            } else {
                error.to_le_bytes()
            };
            bytes[24..26].copy_from_slice(&encoded);
        }
        stream.write_all(&bytes)?;
    }
}

fn dispatch(
    header: &Header,
    bytes: &[u8],
    tables: &TableReader,
) -> Result<(Response, Option<u16>)> {
    let mut response = reply(header);
    if header.flags & (1 << pdu::NON_DEFAULT_CONTEXT) != 0 {
        response.res_error = ResError::UnsupportedContext;
        return Ok((response, None));
    }
    match header.ty {
        Type::Get | Type::GetNext | Type::GetBulk => {
            let (ranges, bulk) = if header.ty == Type::GetBulk {
                let request = pdu::GetBulk::from_bytes(bytes)?;
                (
                    request.sr,
                    Some((request.non_repeaters, request.max_repetitions)),
                )
            } else {
                (pdu::Get::from_bytes(bytes)?.sr, None)
            };
            for range in &ranges.0 {
                validate_oid(&range.start)?;
                validate_oid(&range.end)?;
            }
            if ranges.0.iter().any(|range| range.start.include > 1) {
                return Err(invalid("invalid SearchRange include flag"));
            }
            match tables.snapshot() {
                Some(mib) => {
                    response.vb = Some(if let Some((non_repeaters, max_repetitions)) = bulk {
                        mib.get_bulk(ranges, non_repeaters, max_repetitions)
                    } else {
                        VarBindList(
                            ranges
                                .0
                                .iter()
                                .map(|range| {
                                    if header.ty == Type::Get {
                                        mib.get(&range.start)
                                    } else {
                                        mib.get_next(range)
                                    }
                                })
                                .collect(),
                        )
                    });
                }
                None => {
                    response.res_error = ResError::ProcessingError;
                }
            }
        }
        Type::TestSet => {
            let request = pdu::TestSet::from_bytes(bytes)?;
            for binding in &request.vb {
                validate_oid(&binding.name)?;
                if let Value::ObjectIdentifier(value) = &binding.data {
                    validate_oid(value)?;
                }
            }
            if !request.vb.is_empty() {
                response.res_index = 1;
                return Ok((response, Some(NOT_WRITABLE)));
            }
        }
        Type::CommitSet | Type::UndoSet => {
            if header.payload_length != 0 {
                return Err(invalid("Set phase has a payload"));
            }
            return Ok((
                response,
                Some(if header.ty == Type::CommitSet {
                    COMMIT_FAILED
                } else {
                    UNDO_FAILED
                }),
            ));
        }
        Type::Ping => {
            pdu::Ping::from_bytes(bytes)?;
        }
        _ => response.res_error = ResError::RequestDenied,
    }
    Ok((response, None))
}

fn validate_oid(oid: &ID) -> Result<()> {
    // agentx 0.1.1 exposes normalized components only through Display.
    if oid.to_string().split('.').count() > MAX_OID_SUBIDS {
        return Err(invalid("OID exceeds 128 sub-identifiers"));
    }
    Ok(())
}

fn reply(header: &Header) -> Response {
    let mut response = Response::from_header(header);
    response.header.flags = header.flags & NETWORK_ORDER;
    response
}

fn acknowledge(stream: &mut UnixStream, request: &Header) -> Result<Header> {
    let (header, bytes) = receive(stream, IO_TIMEOUT)?;
    // A Response PDU carries sysUpTime, error and index, then a VarBindList the
    // master is free to populate. net-snmp does: 40 payload bytes for Open and 32
    // for Register. Only the fixed 8 byte block is read below, so a longer payload
    // is an ordinary acknowledgement rather than a malformed one.
    if header.ty != Type::Response
        || header.payload_length < 8
        || header.packet_id != request.packet_id
        || header.transaction_id != request.transaction_id
        || (request.ty != Type::Open && header.session_id != request.session_id)
    {
        return Err(invalid("unexpected AgentX acknowledgement"));
    }
    let order = if header.flags & NETWORK_ORDER != 0 {
        ByteOrder::BigEndian
    } else {
        ByteOrder::LittleEndian
    };
    let error = ResError::from_bytes(&bytes[24..26], &order)?;
    if error != ResError::NoAgentXError {
        return Err(Error::other(format!(
            "AgentX {:?} rejected: {error:?}",
            request.ty
        )));
    }
    if bytes[26..28] != [0, 0] {
        return Err(invalid("nonzero administrative response error index"));
    }
    Ok(header)
}

fn receive(stream: &mut UnixStream, frame_budget: Duration) -> Result<(Header, Vec<u8>)> {
    let mut bytes = vec![0; 20];
    stream.read_exact(&mut bytes[..1])?;
    let idle_timeout = stream.read_timeout()?;
    let deadline = Instant::now() + frame_budget;
    read_before(stream, &mut bytes[1..], deadline)?;
    let header = Header::from_bytes(&bytes)?;
    if header.version != 1 || header.payload_length > MAX_PAYLOAD || header.payload_length % 4 != 0
    {
        return Err(invalid("invalid AgentX version or payload length"));
    }
    bytes.resize(20 + header.payload_length as usize, 0);
    read_before(stream, &mut bytes[20..], deadline)?;
    stream.set_read_timeout(idle_timeout)?;
    Ok((header, bytes))
}

fn read_before(stream: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(frame_deadline());
        }
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(bytes) {
            Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                return Err(frame_deadline());
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn frame_deadline() -> Error {
    Error::new(ErrorKind::TimedOut, "AgentX PDU frame deadline exceeded")
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Instant;

    use agentx::encodings::SearchRangeList;

    use super::*;
    use crate::monitor::publication;

    #[test]
    fn every_read_pdu_reports_processing_error_before_the_first_inventory() {
        let tables = publication();
        let mut requests = vec![
            pdu::Get::new(SearchRangeList::default()),
            pdu::Get::new(SearchRangeList::default()),
        ];
        requests[1].header.ty = Type::GetNext;

        for mut request in requests {
            let bytes = request.to_bytes().unwrap();
            let header = Header::from_bytes(&bytes).unwrap();
            let (response, _) = dispatch(&header, &bytes, &tables).unwrap();
            assert_eq!(response.res_error, ResError::ProcessingError);
        }

        let mut request = pdu::GetBulk::new(SearchRangeList::default());
        let bytes = request.to_bytes().unwrap();
        let header = Header::from_bytes(&bytes).unwrap();
        let (response, _) = dispatch(&header, &bytes, &tables).unwrap();
        assert_eq!(response.res_error, ResError::ProcessingError);
    }

    #[test]
    fn acknowledgement_accepts_a_response_carrying_a_var_bind_list() {
        // A Response PDU is sysUpTime, error and index followed by a VarBindList,
        // and net-snmp sends one: probing a live master gives 40 payload bytes for
        // Open and 32 for Register, never 8. Requiring exactly 8 refuses every real
        // master while the handshake itself succeeded.
        for (open, session_id, payload) in [(true, 21_u32, 40_usize), (false, 21_u32, 32_usize)] {
            let (mut subagent, mut master) =
                UnixStream::pair().expect("create a socket pair for the fake master");

            let request = if open {
                let mut request = pdu::Open::new(ID::default(), "agentx-ifstack");
                request.header.flags = NETWORK_ORDER;
                request.header.packet_id = 1;
                Header::from_bytes(&request.to_bytes().unwrap()).unwrap()
            } else {
                let mut request = pdu::Register::new(ID::try_from(TABLE.to_vec()).unwrap());
                request.header.flags = NETWORK_ORDER;
                request.header.session_id = session_id;
                request.header.packet_id = 2;
                Header::from_bytes(&request.to_bytes().unwrap()).unwrap()
            };

            let mut response = vec![1, Type::Response.to_byte(), NETWORK_ORDER, 0];
            response.extend_from_slice(&session_id.to_be_bytes());
            response.extend_from_slice(&request.transaction_id.to_be_bytes());
            response.extend_from_slice(&request.packet_id.to_be_bytes());
            response.extend_from_slice(&(payload as u32).to_be_bytes());
            response.extend_from_slice(&31_223_731_u32.to_be_bytes()); // sysUpTime
            response.extend_from_slice(&0_u16.to_be_bytes()); // res.error
            response.extend_from_slice(&0_u16.to_be_bytes()); // res.index
            response.resize(20 + payload, 0); // the VarBindList the master appends
            master
                .write_all(&response)
                .expect("send the master response");

            let acknowledged = acknowledge(&mut subagent, &request)
                .expect("a master response carrying a VarBindList is a valid acknowledgement");
            assert_eq!(acknowledged.session_id, session_id);
        }
    }

    #[test]
    fn a_started_frame_must_complete_before_its_budget() {
        let budget = Duration::from_millis(100);
        let frame = Header::new(Type::Ping).to_bytes();
        let (mut receiver, sender) = UnixStream::pair().expect("create slow socket pair");
        let drip = drip_frame(
            sender,
            frame.clone(),
            Duration::ZERO,
            Duration::from_millis(25),
        );
        let started = Instant::now();
        let result = receive(&mut receiver, budget);
        let elapsed = started.elapsed();
        drop(receiver);
        drip.join().expect("join slow frame sender");

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("frame completed after {elapsed:?}, past its {budget:?} budget"),
        };
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(error.to_string().contains("frame deadline"), "{error}");
        assert!(
            elapsed >= budget,
            "frame failed before its budget: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "frame deadline was not bounded: {elapsed:?}"
        );

        let (mut receiver, sender) = UnixStream::pair().expect("create timely socket pair");
        let idle = budget + Duration::from_millis(50);
        let drip = drip_frame(sender, frame, idle, Duration::from_millis(3));
        let started = Instant::now();
        let (header, _) = receive(&mut receiver, budget).expect("receive frame within its budget");
        let elapsed = started.elapsed();
        drip.join().expect("join timely frame sender");
        assert_eq!(header.ty, Type::Ping);
        assert!(elapsed >= idle, "idle wait ended after {elapsed:?}");
    }

    fn drip_frame(
        mut stream: UnixStream,
        frame: Vec<u8>,
        initial_delay: Duration,
        gap: Duration,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            thread::sleep(initial_delay);
            for (index, byte) in frame.into_iter().enumerate() {
                if index != 0 {
                    thread::sleep(gap);
                }
                if stream.write_all(&[byte]).is_err() {
                    return;
                }
            }
        })
    }
}
