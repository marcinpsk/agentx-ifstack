use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
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
const MAX_IP_OUTPUT: usize = 16 * 1024 * 1024;
const IP_REAP_TIMEOUT: Duration = Duration::from_millis(500);
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

fn read_links() -> Result<Vec<u8>> {
    IpCommand::spawn()?.into_output()
}

struct IpCommand {
    child: Option<Child>,
}

impl IpCommand {
    fn spawn() -> Result<Self> {
        Ok(Self {
            child: Some(
                Command::new("ip")
                    .args(["-details", "-json", "link", "show"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .process_group(0)
                    .spawn()?,
            ),
        })
    }

    fn exited(&self) -> Result<bool> {
        let child = self.child.as_ref().expect("unreaped ip child");
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: info is writable, and WNOWAIT keeps our child's PID allocated.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if result == -1 {
            return Err(Error::last_os_error());
        }
        // SAFETY: waitid succeeded; the zeroed record also covers no pending exit.
        Ok(unsafe { info.assume_init().si_pid() } != 0)
    }

    fn into_output(mut self) -> Result<Vec<u8>> {
        let deadline = Instant::now() + IP_TIMEOUT;
        let child = self.child.as_mut().expect("unreaped ip child");
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let mut fds = [stdout.as_raw_fd(), stderr.as_raw_fd()].map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
        for fd in &fds {
            // SAFETY: Each descriptor belongs to a live pipe owned by this call.
            let flags = unsafe { libc::fcntl(fd.fd, libc::F_GETFL) };
            if flags == -1 {
                return Err(Error::last_os_error());
            }
            // SAFETY: F_SETFL changes only this pipe's read-side file description.
            if unsafe { libc::fcntl(fd.fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
                return Err(Error::last_os_error());
            }
        }
        let mut output = [Vec::new(), Vec::new()];
        let mut pipes: [&mut dyn Read; 2] = [&mut stdout, &mut stderr];
        loop {
            ip_time_left(deadline)?;
            let exited = match self.exited() {
                Ok(exited) => exited,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if exited && fds.iter().all(|fd| fd.fd < 0) {
                break;
            }
            let remaining = ip_time_left(deadline)?;
            let timeout = if exited {
                remaining
            } else {
                remaining.min(IP_POLL)
            };
            let millis = timeout.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
            // SAFETY: fds is writable and contains exactly the supplied number of entries.
            let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
            let error = (result == -1).then(Error::last_os_error);
            ip_time_left(deadline)?;
            if let Some(error) = error {
                if error.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            for ((fd, pipe), bytes) in fds.iter_mut().zip(&mut pipes).zip(&mut output) {
                if fd.fd < 0 || fd.revents == 0 {
                    continue;
                }
                let mut buffer = [0; 8192];
                loop {
                    ip_time_left(deadline)?;
                    // Read one byte past the cap to distinguish full from oversized output.
                    let limit = buffer.len().min(MAX_IP_OUTPUT + 1 - bytes.len());
                    match pipe.read(&mut buffer[..limit]) {
                        Ok(0) => {
                            fd.fd = -1;
                            break;
                        }
                        Ok(count) => {
                            bytes.extend_from_slice(&buffer[..count]);
                            if bytes.len() > MAX_IP_OUTPUT {
                                return Err(invalid(&format!(
                                    "ip link wrote more than {MAX_IP_OUTPUT} bytes"
                                )));
                            }
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                        Err(error) => {
                            fd.fd = -1;
                            return Err(error);
                        }
                    }
                }
                if fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                    fd.fd = -1;
                    return Err(Error::other("ip output pipe failed"));
                }
            }
        }
        let status = self.finish()?;
        let [stdout, stderr] = output;
        if status.success() {
            Ok(stdout)
        } else {
            Err(Error::other(format!(
                "ip link exited with {}: {}",
                status,
                String::from_utf8_lossy(&stderr).trim()
            )))
        }
    }

    fn finish(&mut self) -> Result<ExitStatus> {
        let mut child = self.child.take().expect("unreaped ip child");
        // SAFETY: The owned, unreaped child reserves this process group ID.
        let result = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        if result == -1 {
            let error = Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                log::warn!("Cannot kill ip process group: {error}");
            }
        }
        let deadline = Instant::now() + IP_REAP_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => (),
                Err(error) if error.kind() == ErrorKind::Interrupted => (),
                Err(error) => return Err(error),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // A D-state child may leak a zombie; never block or signal its group again.
                return Err(Error::new(
                    ErrorKind::TimedOut,
                    "ip child could not be reaped",
                ));
            }
            std::thread::sleep(remaining.min(IP_POLL));
        }
    }
}

impl Drop for IpCommand {
    fn drop(&mut self) {
        if self.child.is_some()
            && let Err(error) = self.finish()
        {
            log::warn!("Cannot clean up ip child: {error}");
        }
    }
}

fn ip_time_left(deadline: Instant) -> Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(Error::new(
            ErrorKind::TimedOut,
            "ip link exceeded its deadline",
        ))
    } else {
        Ok(remaining)
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
