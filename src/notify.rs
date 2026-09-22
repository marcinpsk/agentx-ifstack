use std::ffi::OsStr;
use std::io::{Error, ErrorKind, Result};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::Path;

const SOCKET: &str = "NOTIFY_SOCKET";
const READY: &[u8] = b"READY=1";

/// Tell the service manager the subagent serves the table, so a Type=notify
/// unit reaches "active" on registration rather than on process start. An
/// absent socket means the process runs outside systemd, which is not an error.
pub fn ready() {
    let Some(socket) = std::env::var_os(SOCKET) else {
        return;
    };
    // A lost notification costs the unit its start timeout. It must not cost
    // the subagent a session it has already registered.
    if let Err(error) = send(&socket, READY) {
        log::warn!("Cannot report readiness to the service manager: {error}");
    }
}

fn send(socket: &OsStr, message: &[u8]) -> Result<()> {
    let datagram = UnixDatagram::unbound()?;
    datagram.send_to_addr(message, &address(socket)?)?;
    Ok(())
}

// systemd passes an abstract socket with a leading @, which is not a filesystem
// path: reading it as one addresses a socket that does not exist.
fn address(socket: &OsStr) -> Result<SocketAddr> {
    match socket.as_bytes() {
        [b'@', name @ ..] if !name.is_empty() => SocketAddr::from_abstract_name(name),
        [b'/', ..] => SocketAddr::from_pathname(Path::new(socket)),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{SOCKET} is neither an absolute path nor an abstract name"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn an_absolute_path_addresses_a_socket_file() {
        let address = address(OsStr::new("/run/systemd/notify")).expect("path address");

        assert_eq!(
            address.as_pathname(),
            Some(Path::new("/run/systemd/notify"))
        );
    }

    #[test]
    fn a_leading_at_sign_addresses_the_abstract_namespace() {
        let address = address(OsStr::new("@systemd/notify")).expect("abstract address");

        assert_eq!(address.as_abstract_name(), Some(&b"systemd/notify"[..]));
    }

    #[test]
    fn an_unusable_socket_value_is_an_error() {
        for value in ["", "@", "run/systemd/notify"] {
            let error = address(&OsString::from(value)).expect_err(value);

            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
    }
}
