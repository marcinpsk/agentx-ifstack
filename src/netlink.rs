use std::io::{Error, ErrorKind, Result};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use netlink_packet_core::{
    NLM_F_DUMP, NLM_F_DUMP_INTR, NLM_F_REQUEST, NetlinkMessage, NetlinkPayload,
};
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::link::{
    InfoData, InfoKind, InfoVxlan, LinkAttribute, LinkInfo, LinkMessage,
};
use netlink_sys::{Socket, SocketAddr, protocols::NETLINK_ROUTE};

use crate::link::{LinkKind, ObservedLink};
use crate::monitor::{Acquisition, AcquisitionError, Attempt, EventBatch, Inventory, WaitOutcome};

const LINK_MULTICAST_GROUP: u32 = 1;
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_DATAGRAM: usize = 1024 * 1024;
const MAX_INVENTORY_BYTES: usize = 16 * 1024 * 1024;
const NETLINK_HEADER_LENGTH: usize = 16;

pub struct NetlinkSource<I = SocketIo>
where
    I: DatagramIo,
{
    io: I,
    notification: Option<I::Socket>,
    origin: Instant,
    sequence: u32,
}

impl NetlinkSource<SocketIo> {
    pub fn new() -> Self {
        Self::with_io(SocketIo)
    }
}

impl<I> NetlinkSource<I>
where
    I: DatagramIo,
{
    fn with_io(io: I) -> Self {
        Self {
            io,
            notification: None,
            origin: Instant::now(),
            sequence: 0,
        }
    }

    fn receive_notification(&mut self) -> Result<Option<Duration>> {
        let notification = self
            .notification
            .as_ref()
            .ok_or_else(|| Error::other("link notifications are not subscribed"))?;
        let datagram = self.io.receive(notification)?;
        if datagram.truncated {
            return Err(invalid("link notification datagram was truncated"));
        }
        if !datagram.from_kernel {
            return Err(invalid("link notification came from a non-kernel sender"));
        }
        if datagram.bytes.is_empty() {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "link notification socket terminated",
            ));
        }
        let messages = decode_datagram(&datagram.bytes)?;
        let mut changed = false;
        for message in messages {
            if message.header.sequence_number != 0 {
                return Err(invalid("link notification carried a response sequence"));
            }
            match message.payload {
                NetlinkPayload::InnerMessage(
                    RouteNetlinkMessage::NewLink(_) | RouteNetlinkMessage::DelLink(_),
                ) => changed = true,
                NetlinkPayload::Overrun(_) => {
                    return Err(Error::other("link notification socket reported overrun"));
                }
                NetlinkPayload::Noop => {}
                _ => return Err(invalid("unexpected link notification message")),
            }
        }
        Ok(changed.then(|| self.now()))
    }

    fn next_sequence(&mut self) -> u32 {
        self.sequence = self.sequence.wrapping_add(1);
        if self.sequence == 0 {
            self.sequence = 1;
        }
        self.sequence
    }
}

impl<I> Acquisition for NetlinkSource<I>
where
    I: DatagramIo,
{
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn subscribe(&mut self) -> Result<()> {
        let socket = self.io.open(LINK_MULTICAST_GROUP)?;
        self.notification = Some(socket);
        Ok(())
    }

    fn inventory(&mut self, attempt: Attempt) -> std::result::Result<Inventory, AcquisitionError> {
        let mut events = EventBatch::default();
        let dump = self.io.open(0).map_err(|error| ordinary(error, events))?;
        let sequence = self.next_sequence();
        let request = dump_request(sequence);
        self.io
            .send(&dump, &request)
            .map_err(|error| ordinary(error, events))?;

        let deadline = Instant::now() + INVENTORY_TIMEOUT;
        let mut links = Vec::new();
        let mut total = 0usize;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ordinary(
                    Error::new(
                        ErrorKind::TimedOut,
                        "interface inventory exceeded its deadline",
                    ),
                    events,
                ));
            }
            let notification = self
                .notification
                .as_ref()
                .ok_or_else(|| lost(Error::other("notification socket absent"), events))?;
            let ready = match self.io.wait(notification, Some(&dump), remaining) {
                Ok(ready) => ready,
                Err(error) => {
                    self.notification = None;
                    return Err(lost(error, events));
                }
            };
            if ready.notification_failed {
                self.notification = None;
                return Err(lost(
                    Error::other("link notification socket failed"),
                    events,
                ));
            }
            if ready.dump_failed {
                return Err(ordinary(
                    Error::other("interface inventory socket failed"),
                    events,
                ));
            }
            if ready.notification {
                let at = match self.receive_notification() {
                    Ok(at) => at,
                    Err(error) => {
                        self.notification = None;
                        return Err(lost(error, events));
                    }
                };
                if let Some(at) = at {
                    events.first.get_or_insert(at);
                    events.last = Some(at);
                }
            }
            if ready.dump {
                let datagram = self
                    .io
                    .receive(&dump)
                    .map_err(|error| ordinary(error, events))?;
                if datagram.truncated {
                    return Err(ordinary(
                        invalid("interface inventory datagram was truncated"),
                        events,
                    ));
                }
                if !datagram.from_kernel || datagram.bytes.is_empty() {
                    return Err(ordinary(
                        invalid("invalid interface inventory sender or termination"),
                        events,
                    ));
                }
                total = total.saturating_add(datagram.bytes.len());
                if total > MAX_INVENTORY_BYTES {
                    return Err(ordinary(
                        invalid("interface inventory exceeded its byte limit"),
                        events,
                    ));
                }
                match consume_dump(&datagram.bytes, sequence, &mut links) {
                    Ok(true) => {
                        return Ok(Inventory {
                            attempt,
                            links,
                            events,
                        });
                    }
                    Ok(false) => {}
                    Err(error) => return Err(ordinary(error, events)),
                }
            }
        }
    }

    fn wait(&mut self, timeout: Duration) -> Result<WaitOutcome> {
        let Some(notification) = self.notification.as_ref() else {
            std::thread::sleep(timeout);
            return Ok(WaitOutcome::Deadline);
        };
        let ready = match self.io.wait(notification, None, timeout) {
            Ok(ready) => ready,
            Err(error) => {
                self.notification = None;
                return Err(error);
            }
        };
        #[cfg(test)]
        if ready.shutdown {
            return Ok(WaitOutcome::Shutdown);
        }
        if ready.notification_failed {
            self.notification = None;
            return Err(Error::other("link notification socket failed"));
        }
        if !ready.notification {
            return Ok(WaitOutcome::Deadline);
        }
        match self.receive_notification() {
            Ok(Some(at)) => Ok(WaitOutcome::LinkChanged(at)),
            Ok(None) => Ok(WaitOutcome::Deadline),
            Err(error) => {
                self.notification = None;
                Err(error)
            }
        }
    }
}

fn dump_request(sequence: u32) -> Vec<u8> {
    let mut message = NetlinkMessage::from(RouteNetlinkMessage::GetLink(LinkMessage::default()));
    message.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
    message.header.sequence_number = sequence;
    message.finalize();
    let mut bytes = vec![0; message.buffer_len()];
    message.serialize(&mut bytes);
    bytes
}

fn consume_dump(bytes: &[u8], sequence: u32, links: &mut Vec<ObservedLink>) -> Result<bool> {
    let messages = decode_datagram(bytes)?;
    let message_count = messages.len();
    for (position, message) in messages.into_iter().enumerate() {
        if message.header.sequence_number != sequence {
            return Err(invalid(
                "interface inventory response sequence did not match its request",
            ));
        }
        if message.header.flags & NLM_F_DUMP_INTR != 0 {
            return Err(Error::other("interface inventory dump was interrupted"));
        }
        match message.payload {
            NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewLink(link)) => {
                links.push(decode_link(link)?);
            }
            NetlinkPayload::Done(done) if done.code == 0 && position + 1 == message_count => {
                return Ok(true);
            }
            NetlinkPayload::Done(done) if done.code == 0 => {
                return Err(invalid("interface inventory continued after completion"));
            }
            NetlinkPayload::Done(_) => {
                return Err(Error::other(
                    "interface inventory completion reported an error",
                ));
            }
            NetlinkPayload::Error(error) => return Err(error.into()),
            NetlinkPayload::Overrun(_) => {
                return Err(Error::other("interface inventory socket reported overrun"));
            }
            _ => return Err(invalid("unexpected interface inventory message")),
        }
    }
    Ok(false)
}

fn decode_datagram(bytes: &[u8]) -> Result<Vec<NetlinkMessage<RouteNetlinkMessage>>> {
    let mut messages = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.len() < NETLINK_HEADER_LENGTH {
            return Err(invalid("trailing bytes after netlink message"));
        }
        let length = u32::from_ne_bytes(
            remaining[..4]
                .try_into()
                .expect("netlink length field has four bytes"),
        ) as usize;
        if length < NETLINK_HEADER_LENGTH || length > remaining.len() {
            return Err(invalid("invalid netlink message length"));
        }
        messages.push(
            NetlinkMessage::deserialize(&remaining[..length])
                .map_err(|error| invalid(&format!("cannot decode netlink message: {error}")))?,
        );
        let aligned = length
            .checked_add(3)
            .map(|value| value & !3)
            .ok_or_else(|| invalid("netlink message length overflow"))?;
        if aligned > remaining.len() {
            if length == remaining.len() {
                offset = bytes.len();
            } else {
                return Err(invalid("truncated netlink message padding"));
            }
        } else {
            offset += aligned;
        }
    }
    Ok(messages)
}

fn decode_link(message: LinkMessage) -> Result<ObservedLink> {
    let mut name = None;
    let mut controller = None;
    let mut lower = None;
    let mut lower_netnsid = None;
    let mut kind = None;
    let mut data = None;
    for attribute in message.attributes {
        match attribute {
            LinkAttribute::IfName(value) => set_once(&mut name, value, "interface name")?,
            LinkAttribute::Controller(value) => {
                set_once(&mut controller, value, "controller")?;
            }
            LinkAttribute::Link(value) => set_once(&mut lower, value, "lower interface")?,
            LinkAttribute::LinkNetNsId(value) => {
                set_once(&mut lower_netnsid, value, "lower network namespace")?;
            }
            LinkAttribute::LinkInfo(infos) => {
                for info in infos {
                    match info {
                        LinkInfo::Kind(value) => set_once(&mut kind, value, "link kind")?,
                        LinkInfo::Data(value) => set_once(&mut data, value, "link data")?,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    let kind = match kind.as_ref() {
        Some(InfoKind::Bond) => LinkKind::Bond,
        Some(InfoKind::Bridge) => LinkKind::Bridge,
        Some(InfoKind::Vlan) => LinkKind::Vlan,
        Some(InfoKind::MacVlan) => LinkKind::MacVlan,
        Some(InfoKind::IpVlan) => LinkKind::IpVlan,
        Some(InfoKind::MacVtap) => LinkKind::MacVtap,
        Some(InfoKind::Vxlan) => LinkKind::Vxlan,
        Some(InfoKind::Veth) => LinkKind::Veth,
        _ => LinkKind::Other,
    };
    let vxlan_lower = match data {
        Some(InfoData::Vxlan(attributes)) => {
            let mut result = None;
            for attribute in attributes {
                if let InfoVxlan::Link(value) = attribute {
                    set_once(&mut result, value, "VXLAN underlay")?;
                }
            }
            result
        }
        _ => None,
    };

    Ok(ObservedLink {
        index: message.header.index,
        name: name.ok_or_else(|| invalid("interface has no name"))?,
        kind,
        controller,
        lower,
        lower_netnsid,
        vxlan_lower,
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, field: &str) -> Result<()> {
    if slot.replace(value).is_some() {
        Err(invalid(&format!("duplicate {field}")))
    } else {
        Ok(())
    }
}

pub(crate) struct Datagram {
    bytes: Vec<u8>,
    from_kernel: bool,
    truncated: bool,
}

#[derive(Default)]
pub(crate) struct Ready {
    notification: bool,
    dump: bool,
    notification_failed: bool,
    dump_failed: bool,
    #[cfg(test)]
    shutdown: bool,
}

pub(crate) trait DatagramIo {
    type Socket;

    fn open(&mut self, groups: u32) -> Result<Self::Socket>;
    fn send(&mut self, socket: &Self::Socket, bytes: &[u8]) -> Result<()>;
    fn wait(
        &mut self,
        notification: &Self::Socket,
        dump: Option<&Self::Socket>,
        timeout: Duration,
    ) -> Result<Ready>;
    fn receive(&mut self, socket: &Self::Socket) -> Result<Datagram>;
}

pub(crate) struct SocketIo;

impl DatagramIo for SocketIo {
    type Socket = Socket;

    fn open(&mut self, groups: u32) -> Result<Self::Socket> {
        let mut socket = Socket::new(NETLINK_ROUTE)?;
        socket.bind(&SocketAddr::new(0, groups))?;
        socket.connect(&SocketAddr::new(0, 0))?;
        socket.set_non_blocking(true)?;
        Ok(socket)
    }

    fn send(&mut self, socket: &Self::Socket, bytes: &[u8]) -> Result<()> {
        loop {
            match socket.send(bytes, 0) {
                Ok(written) if written == bytes.len() => return Ok(()),
                Ok(_) => {
                    return Err(Error::new(
                        ErrorKind::WriteZero,
                        "partial netlink request write",
                    ));
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn wait(
        &mut self,
        notification: &Self::Socket,
        dump: Option<&Self::Socket>,
        timeout: Duration,
    ) -> Result<Ready> {
        let mut descriptors = vec![libc::pollfd {
            fd: notification.as_raw_fd(),
            events: libc::POLLIN | libc::POLLERR,
            revents: 0,
        }];
        if let Some(dump) = dump {
            descriptors.push(libc::pollfd {
                fd: dump.as_raw_fd(),
                events: libc::POLLIN | libc::POLLERR,
                revents: 0,
            });
        }
        let started = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            let millis = remaining
                .as_millis()
                .saturating_add(u128::from(
                    !remaining.subsec_nanos().is_multiple_of(1_000_000),
                ))
                .min(i32::MAX as u128) as i32;
            // SAFETY: The vector owns valid pollfd records for the duration of the call.
            let count = unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    descriptors.len() as libc::nfds_t,
                    millis,
                )
            };
            if count >= 0 {
                break;
            }
            let error = Error::last_os_error();
            if error.kind() != ErrorKind::Interrupted {
                return Err(error);
            }
        }
        let failed = libc::POLLHUP | libc::POLLNVAL;
        let notification = descriptors[0].revents;
        let dump = descriptors
            .get(1)
            .map_or(0, |descriptor| descriptor.revents);
        Ok(Ready {
            notification: notification & (libc::POLLIN | libc::POLLERR) != 0,
            dump: dump & (libc::POLLIN | libc::POLLERR) != 0,
            notification_failed: notification & failed != 0,
            dump_failed: dump & failed != 0,
            #[cfg(test)]
            shutdown: false,
        })
    }

    fn receive(&mut self, socket: &Self::Socket) -> Result<Datagram> {
        loop {
            let mut storage = vec![0; MAX_DATAGRAM];
            let received = {
                let mut target = &mut storage[..];
                socket.recv_from(&mut target, libc::MSG_TRUNC)
            };
            match received {
                Ok((size, sender)) => {
                    let truncated = size > storage.len();
                    storage.truncate(size.min(storage.len()));
                    return Ok(Datagram {
                        bytes: storage,
                        from_kernel: sender.port_number() == 0,
                        truncated,
                    });
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

fn ordinary(error: Error, events: EventBatch) -> AcquisitionError {
    AcquisitionError::Ordinary { error, events }
}

fn lost(error: Error, events: EventBatch) -> AcquisitionError {
    AcquisitionError::Lost { error, events }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, VecDeque};
    use std::rc::Rc;

    use super::*;
    use crate::link::Topology;
    use crate::monitor::{TableReader, publication, run};

    fn encoded(mut message: NetlinkMessage<RouteNetlinkMessage>) -> Vec<u8> {
        message.finalize();
        let mut bytes = vec![0; message.buffer_len()];
        message.serialize(&mut bytes);
        bytes
    }

    fn link_message(sequence: u32, index: u32, name: &str) -> Vec<u8> {
        typed_link_message(sequence, index, name, Vec::new())
    }

    fn typed_link_message(
        sequence: u32,
        index: u32,
        name: &str,
        mut attributes: Vec<LinkAttribute>,
    ) -> Vec<u8> {
        let mut link = LinkMessage::default();
        link.header.index = index;
        link.attributes.push(LinkAttribute::IfName(name.to_owned()));
        link.attributes.append(&mut attributes);
        let mut message = NetlinkMessage::from(RouteNetlinkMessage::NewLink(link));
        message.header.sequence_number = sequence;
        encoded(message)
    }

    fn done_message(sequence: u32, flags: u16) -> Vec<u8> {
        let mut message = NetlinkMessage::new(
            netlink_packet_core::NetlinkHeader::default(),
            NetlinkPayload::Done(netlink_packet_core::DoneMessage::default()),
        );
        message.header.sequence_number = sequence;
        message.header.flags = flags;
        encoded(message)
    }

    struct ScriptIo {
        opens: VecDeque<Result<u8>>,
        waits: VecDeque<Result<Ready>>,
        receives: BTreeMap<u8, VecDeque<Result<Datagram>>>,
        observer: Option<(TableReader, Rc<RefCell<Vec<bool>>>)>,
    }

    impl ScriptIo {
        fn subscribed() -> Self {
            Self {
                opens: VecDeque::from([Ok(1)]),
                waits: VecDeque::new(),
                receives: BTreeMap::new(),
                observer: None,
            }
        }

        fn receive(&mut self, socket: u8, result: Result<Datagram>) {
            self.receives.entry(socket).or_default().push_back(result);
        }
    }

    impl DatagramIo for ScriptIo {
        type Socket = u8;

        fn open(&mut self, _groups: u32) -> Result<Self::Socket> {
            self.opens
                .pop_front()
                .unwrap_or_else(|| Err(Error::other("unexpected open")))
        }

        fn send(&mut self, _socket: &Self::Socket, _bytes: &[u8]) -> Result<()> {
            Ok(())
        }

        fn wait(
            &mut self,
            _notification: &Self::Socket,
            _dump: Option<&Self::Socket>,
            _timeout: Duration,
        ) -> Result<Ready> {
            if let Some((reader, observations)) = &self.observer {
                observations.borrow_mut().push(reader.snapshot().is_some());
            }
            self.waits
                .pop_front()
                .unwrap_or_else(|| Err(Error::other("unexpected wait")))
        }

        fn receive(&mut self, socket: &Self::Socket) -> Result<Datagram> {
            self.receives
                .get_mut(socket)
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| Err(Error::other("unexpected receive")))
        }
    }

    fn datagram(bytes: Vec<u8>) -> Datagram {
        Datagram {
            bytes,
            from_kernel: true,
            truncated: false,
        }
    }

    #[test]
    fn actual_link_message_types_decode_supported_fields() {
        let mut message = LinkMessage::default();
        message.header.index = 10;
        message.attributes = vec![
            LinkAttribute::IfName("vlan0".to_owned()),
            LinkAttribute::Link(2),
            LinkAttribute::Controller(9),
            LinkAttribute::LinkInfo(vec![LinkInfo::Kind(InfoKind::Vlan)]),
        ];
        let observed = decode_link(message).unwrap();

        assert_eq!(observed.index, 10);
        assert_eq!(observed.name, "vlan0");
        assert_eq!(observed.kind, LinkKind::Vlan);
        assert_eq!(observed.lower, Some(2));
        assert_eq!(observed.controller, Some(9));
    }

    #[test]
    fn production_source_reads_a_complete_current_namespace_inventory() {
        let mut source = NetlinkSource::new();
        source.subscribe().unwrap();

        let inventory = source.inventory(Attempt { continuity: 0 }).unwrap();
        let topology = Topology::from_observed(inventory.links).unwrap();

        assert!(!topology.interfaces().is_empty());
    }

    #[test]
    fn final_done_interruption_rejects_the_dump() {
        let mut done = NetlinkMessage::new(
            netlink_packet_core::NetlinkHeader::default(),
            NetlinkPayload::Done(netlink_packet_core::DoneMessage::default()),
        );
        done.header.sequence_number = 7;
        done.header.flags = NLM_F_DUMP_INTR;
        let mut links = Vec::new();

        let error = consume_dump(&encoded(done), 7, &mut links).unwrap_err();

        assert!(error.to_string().contains("interrupted"), "{error}");
    }

    #[test]
    fn production_acquisition_path_decodes_a_complete_typed_inventory() {
        let mut io = ScriptIo::subscribed();
        io.opens.push_back(Ok(2));
        io.waits.push_back(Ok(Ready {
            dump: true,
            ..Ready::default()
        }));
        let mut bytes = link_message(1, 2, "lower");
        bytes.extend(done_message(1, 0));
        io.receive(2, Ok(datagram(bytes)));
        let mut source = NetlinkSource::with_io(io);
        source.subscribe().unwrap();

        let inventory = source.inventory(Attempt { continuity: 4 }).unwrap();

        assert_eq!(inventory.attempt.continuity, 4);
        assert_eq!(inventory.links.len(), 1);
        assert_eq!(inventory.links[0].index, 2);
        assert_eq!(inventory.links[0].name, "lower");
    }

    #[test]
    fn production_packets_build_every_supported_direct_relationship() {
        let mut io = ScriptIo::subscribed();
        io.opens.push_back(Ok(2));
        io.waits.push_back(Ok(Ready {
            dump: true,
            ..Ready::default()
        }));
        let kind = |kind| LinkAttribute::LinkInfo(vec![LinkInfo::Kind(kind)]);
        let mut bytes = Vec::new();
        for packet in [
            typed_link_message(1, 2, "lower", vec![]),
            typed_link_message(1, 9, "bond", vec![kind(InfoKind::Bond)]),
            typed_link_message(1, 3, "member", vec![LinkAttribute::Controller(9)]),
            typed_link_message(
                1,
                10,
                "vlan",
                vec![LinkAttribute::Link(9), kind(InfoKind::Vlan)],
            ),
            typed_link_message(
                1,
                11,
                "macvlan",
                vec![LinkAttribute::Link(2), kind(InfoKind::MacVlan)],
            ),
            typed_link_message(
                1,
                12,
                "ipvlan",
                vec![LinkAttribute::Link(2), kind(InfoKind::IpVlan)],
            ),
            typed_link_message(
                1,
                13,
                "macvtap",
                vec![LinkAttribute::Link(2), kind(InfoKind::MacVtap)],
            ),
            typed_link_message(
                1,
                14,
                "vxlan",
                vec![
                    LinkAttribute::Link(99),
                    LinkAttribute::LinkInfo(vec![
                        LinkInfo::Kind(InfoKind::Vxlan),
                        LinkInfo::Data(InfoData::Vxlan(vec![InfoVxlan::Link(2)])),
                    ]),
                ],
            ),
            typed_link_message(
                1,
                20,
                "veth",
                vec![LinkAttribute::Link(2), kind(InfoKind::Veth)],
            ),
        ] {
            bytes.extend(packet);
        }
        bytes.extend(done_message(1, 0));
        io.receive(2, Ok(datagram(bytes)));
        let mut source = NetlinkSource::with_io(io);
        source.subscribe().unwrap();

        let inventory = source.inventory(Attempt { continuity: 0 }).unwrap();
        let topology = Topology::from_observed(inventory.links).unwrap();
        let relationships: Vec<_> = topology
            .relationships()
            .iter()
            .map(|relationship| (relationship.higher, relationship.lower))
            .collect();

        assert_eq!(
            relationships,
            [(9, 3), (10, 9), (11, 2), (12, 2), (13, 2), (14, 2)]
        );
    }

    #[test]
    fn production_acquisition_path_checks_interruption_on_final_done() {
        let mut io = ScriptIo::subscribed();
        io.opens.push_back(Ok(2));
        io.waits.push_back(Ok(Ready {
            dump: true,
            ..Ready::default()
        }));
        io.receive(2, Ok(datagram(done_message(1, NLM_F_DUMP_INTR))));
        let mut source = NetlinkSource::with_io(io);
        source.subscribe().unwrap();

        let error = source.inventory(Attempt { continuity: 0 }).unwrap_err();

        assert!(matches!(error, AcquisitionError::Ordinary { .. }));
    }

    #[test]
    fn failed_production_acquisitions_return_events_already_consumed() {
        for continuity_lost in [false, true] {
            let mut io = ScriptIo::subscribed();
            io.opens.push_back(Ok(2));
            io.waits.push_back(Ok(Ready {
                notification: true,
                ..Ready::default()
            }));
            io.receive(1, Ok(datagram(link_message(0, 3, "changed"))));
            if continuity_lost {
                io.waits.push_back(Ok(Ready {
                    notification: true,
                    ..Ready::default()
                }));
                io.receive(1, Err(Error::from_raw_os_error(libc::ENOBUFS)));
            } else {
                io.waits.push_back(Ok(Ready {
                    dump: true,
                    ..Ready::default()
                }));
                io.receive(
                    2,
                    Ok(Datagram {
                        truncated: true,
                        ..datagram(done_message(1, 0))
                    }),
                );
            }
            let mut source = NetlinkSource::with_io(io);
            source.subscribe().unwrap();

            let error = source.inventory(Attempt { continuity: 0 }).unwrap_err();
            let events = match error {
                AcquisitionError::Ordinary { events, .. } if !continuity_lost => events,
                AcquisitionError::Lost { events, .. } if continuity_lost => events,
                error => panic!("unexpected acquisition result: {error:?}"),
            };

            assert!(events.first.is_some());
            assert_eq!(events.first, events.last);
            assert_eq!(source.notification.is_none(), continuity_lost);
        }
    }

    #[test]
    fn raw_notification_failures_all_report_lost_continuity() {
        #[derive(Clone, Copy)]
        enum Failure {
            Poll,
            Receive,
            Termination,
            Truncation,
            Malformed,
            Overrun,
        }
        for failure in [
            Failure::Poll,
            Failure::Receive,
            Failure::Termination,
            Failure::Truncation,
            Failure::Malformed,
            Failure::Overrun,
        ] {
            let mut io = ScriptIo::subscribed();
            if matches!(failure, Failure::Poll) {
                io.waits
                    .push_back(Err(Error::from_raw_os_error(libc::ENOBUFS)));
            } else {
                io.waits.push_back(Ok(Ready {
                    notification: true,
                    ..Ready::default()
                }));
                let result = match failure {
                    Failure::Receive => Err(Error::from_raw_os_error(libc::ENOBUFS)),
                    Failure::Termination => Ok(datagram(Vec::new())),
                    Failure::Truncation => Ok(Datagram {
                        truncated: true,
                        ..datagram(link_message(0, 2, "link"))
                    }),
                    Failure::Malformed => Ok(datagram(vec![0; 15])),
                    Failure::Overrun => {
                        let mut message = NetlinkMessage::new(
                            netlink_packet_core::NetlinkHeader::default(),
                            NetlinkPayload::Overrun(Vec::new()),
                        );
                        message.header.sequence_number = 0;
                        Ok(datagram(encoded(message)))
                    }
                    Failure::Poll => unreachable!(),
                };
                io.receive(1, result);
            }
            let mut source = NetlinkSource::with_io(io);
            source.subscribe().unwrap();

            assert!(source.wait(Duration::ZERO).is_err());
            assert!(source.notification.is_none());
        }
    }

    #[test]
    fn failed_resubscription_can_be_retried_on_the_production_adapter() {
        let mut io = ScriptIo::subscribed();
        io.opens
            .push_back(Err(Error::other("scripted resubscription failure")));
        io.opens.push_back(Ok(2));
        let mut source = NetlinkSource::with_io(io);

        source.subscribe().unwrap();
        assert!(source.subscribe().is_err());
        source.subscribe().unwrap();
    }

    #[test]
    fn waiting_for_a_subscription_retry_needs_no_notification_socket() {
        let mut source = NetlinkSource::with_io(ScriptIo::subscribed());

        assert!(matches!(
            source.wait(Duration::ZERO).unwrap(),
            WaitOutcome::Deadline
        ));
    }

    #[test]
    fn raw_loss_failed_resubscription_and_recovery_drive_publication() {
        let reader = publication();
        let observations = Rc::new(RefCell::new(Vec::new()));
        let mut io = ScriptIo::subscribed();
        io.opens.extend([
            Ok(2),
            Err(Error::other("scripted resubscription failure")),
            Ok(3),
            Ok(4),
        ]);
        io.waits.extend([
            Ok(Ready {
                dump: true,
                ..Ready::default()
            }),
            Ok(Ready {
                notification: true,
                ..Ready::default()
            }),
            Ok(Ready {
                dump: true,
                ..Ready::default()
            }),
            Ok(Ready {
                shutdown: true,
                ..Ready::default()
            }),
        ]);
        let mut initial = link_message(1, 1, "initial");
        initial.extend(done_message(1, 0));
        io.receive(2, Ok(datagram(initial)));
        io.receive(1, Err(Error::from_raw_os_error(libc::ENOBUFS)));
        let mut recovered = link_message(2, 2, "recovered");
        recovered.extend(done_message(2, 0));
        io.receive(4, Ok(datagram(recovered)));
        io.observer = Some((reader.clone(), Rc::clone(&observations)));

        run(
            NetlinkSource::with_io(io),
            &reader,
            Duration::from_secs(3600),
        );

        assert_eq!(&*observations.borrow(), &[false, true, false, true]);
        assert!(reader.snapshot().is_some());
    }

    #[test]
    fn malformed_and_trailing_netlink_bytes_are_rejected() {
        for bytes in [
            vec![],
            vec![0; 15],
            vec![15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ] {
            if bytes.is_empty() {
                assert!(decode_datagram(&bytes).unwrap().is_empty());
            } else {
                assert!(decode_datagram(&bytes).is_err());
            }
        }
    }

    struct CountedSocket {
        id: u32,
        active: Rc<Cell<usize>>,
    }

    impl Drop for CountedSocket {
        fn drop(&mut self) {
            self.active.set(self.active.get() - 1);
        }
    }

    struct CountingIo {
        active: Rc<Cell<usize>>,
        next_id: u32,
        sequences: BTreeMap<u32, u32>,
    }

    impl DatagramIo for CountingIo {
        type Socket = CountedSocket;

        fn open(&mut self, _groups: u32) -> Result<Self::Socket> {
            self.next_id += 1;
            self.active.set(self.active.get() + 1);
            Ok(CountedSocket {
                id: self.next_id,
                active: Rc::clone(&self.active),
            })
        }

        fn send(&mut self, socket: &Self::Socket, bytes: &[u8]) -> Result<()> {
            assert!(
                socket.id > 1,
                "the notification socket received a dump request"
            );
            let request = decode_datagram(bytes)?;
            assert_eq!(request.len(), 1);
            self.sequences
                .insert(socket.id, request[0].header.sequence_number);
            Ok(())
        }

        fn wait(
            &mut self,
            _notification: &Self::Socket,
            dump: Option<&Self::Socket>,
            _timeout: Duration,
        ) -> Result<Ready> {
            let dump = dump.expect("inventory wait has a dump socket");
            Ok(Ready {
                dump: !dump.id.is_multiple_of(3),
                dump_failed: dump.id.is_multiple_of(3),
                ..Ready::default()
            })
        }

        fn receive(&mut self, socket: &Self::Socket) -> Result<Datagram> {
            let sequence = self
                .sequences
                .remove(&socket.id)
                .expect("dump request recorded its sequence");
            Ok(datagram(done_message(sequence, 0)))
        }
    }

    #[test]
    fn repeated_failures_recoveries_and_inventories_release_sockets() {
        let active = Rc::new(Cell::new(0));
        let io = CountingIo {
            active: Rc::clone(&active),
            next_id: 0,
            sequences: BTreeMap::new(),
        };
        let mut source = NetlinkSource::with_io(io);
        source.subscribe().unwrap();

        for continuity in 0..100 {
            if continuity % 10 == 0 {
                source.subscribe().unwrap();
            }
            let dump_id = source.io.next_id + 1;
            let result = source.inventory(Attempt { continuity });
            assert_eq!(result.is_err(), dump_id.is_multiple_of(3));
            assert_eq!(active.get(), 1);
        }

        drop(source);
        assert_eq!(active.get(), 0);
    }
}
