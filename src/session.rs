use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use agentx::ByteOrder;
use agentx::encodings::{ID, Value, VarBindList};
use agentx::pdu::{self, Header, ResError, Response, Type};

use crate::{
    config::Config,
    link,
    mib::{Mib, TABLE},
};

const NETWORK_ORDER: u8 = 1 << pdu::NETWORK_BYTE_ORDER;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
// Must stay below the session timeout above so a slow ip still leaves time to answer.
const IP_TIMEOUT: Duration = Duration::from_secs(3);
const IP_POLL: Duration = Duration::from_millis(20);
const MAX_PAYLOAD: u32 = 1024 * 1024;
const MAX_OID_SUBIDS: usize = 128;
const NOT_WRITABLE: u16 = 17;
const COMMIT_FAILED: u16 = 14;
const UNDO_FAILED: u16 = 15;

pub fn run(config: &Config) -> Result<()> {
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

    let mut cache = Cache {
        topology: None,
        refresh: Duration::from_secs(config.refresh),
    };
    loop {
        let (header, bytes) = receive(&mut stream)?;
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
        let (mut response, snmp_error) = match dispatch(&header, &bytes, &mut cache) {
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

fn dispatch(header: &Header, bytes: &[u8], cache: &mut Cache) -> Result<(Response, Option<u16>)> {
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
            match cache.get() {
                Ok(mib) => {
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
                Err(error) => {
                    log::error!("Interface refresh failed: {error}");
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
    let (header, bytes) = receive(stream)?;
    if header.ty != Type::Response
        || header.payload_length != 8
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

fn receive(stream: &mut UnixStream) -> Result<(Header, Vec<u8>)> {
    let mut bytes = vec![0; 20];
    stream.read_exact(&mut bytes[..1])?;
    let idle_timeout = stream.read_timeout()?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.read_exact(&mut bytes[1..])?;
    let header = Header::from_bytes(&bytes)?;
    if header.version != 1 || header.payload_length > MAX_PAYLOAD || header.payload_length % 4 != 0
    {
        return Err(invalid("invalid AgentX version or payload length"));
    }
    bytes.resize(20 + header.payload_length as usize, 0);
    stream.read_exact(&mut bytes[20..])?;
    stream.set_read_timeout(idle_timeout)?;
    Ok((header, bytes))
}

/// Runs `ip link show` under a deadline. An `ip` that never exits, or whose output a
/// descendant keeps open, must not wedge the single-threaded request loop, so the wait
/// and both pipe reads share one deadline and the child is killed when it expires.
fn read_links() -> Result<Vec<u8>> {
    let mut child = Command::new("ip")
        .args(["-details", "-json", "link", "show"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + IP_TIMEOUT;
    // Drain both pipes concurrently; a full pipe would otherwise block the child forever.
    let stdout = drain(child.stdout.take().expect("piped stdout"));
    let stderr = drain(child.stderr.take().expect("piped stderr"));
    let status = wait_bounded(&mut child, deadline)?;
    // A reader still blocked here means a descendant inherited the pipe. Abandon it
    // rather than block the request loop; it ends when that descendant does.
    let stdout = collect_bounded(&stdout, deadline)?;
    let stderr = collect_bounded(&stderr, deadline)?;
    if !status.success() {
        return Err(Error::other(format!(
            "ip link exited with {}: {}",
            status,
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(stdout)
}

fn drain(mut pipe: impl Read + Send + 'static) -> Receiver<Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = sender.send(pipe.read_to_end(&mut bytes).map(|_| bytes));
    });
    receiver
}

fn collect_bounded(reader: &Receiver<Result<Vec<u8>>>, deadline: Instant) -> Result<Vec<u8>> {
    match reader.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(Error::new(
            ErrorKind::TimedOut,
            "ip link output was still open after the deadline",
        )),
        Err(RecvTimeoutError::Disconnected) => Err(Error::other("ip reader thread panicked")),
    }
}

fn wait_bounded(child: &mut Child, deadline: Instant) -> Result<std::process::ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(Error::new(
                ErrorKind::TimedOut,
                format!("ip link did not exit within {IP_TIMEOUT:?}"),
            ));
        }
        std::thread::sleep(IP_POLL);
    }
}

struct Cache {
    topology: Option<(Instant, Mib)>,
    refresh: Duration,
}

impl Cache {
    fn get(&mut self) -> Result<&Mib> {
        if self
            .topology
            .as_ref()
            .is_none_or(|(updated, _)| updated.elapsed() >= self.refresh)
        {
            let stdout = read_links()?;
            let json = std::str::from_utf8(&stdout)
                .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
            let mib = Mib::new(link::parse(json)?);
            self.topology = Some((Instant::now(), mib));
        }
        Ok(&self
            .topology
            .as_ref()
            .expect("cache populated after successful refresh")
            .1)
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}
