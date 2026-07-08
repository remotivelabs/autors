//! Offline DoIP extraction from pcapng capture files.
//!
//! The pcapng container is parsed by `pcap-parser`; Ethernet/IP/TCP/UDP
//! headers are decoded by `etherparse`. This module adds DoIP port filtering,
//! TCP stream reassembly, and conversion to the crate's [`Frame`] model.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use etherparse::{EtherType, NetSlice, SlicedPacket, TransportSlice};
use pcap_parser::pcapng::Block;
use pcap_parser::traits::{PcapNGPacketBlock, PcapReaderIterator};
use pcap_parser::{Linktype, PcapBlockOwned, PcapError, PcapNGReader};

use crate::doip::{Frame, SocketType, DEFAULT_PORT, HEADER_LEN};
use crate::{Error, Result};

const DEFAULT_READER_BUFFER_SIZE: usize = 256 * 1024;
const DEFAULT_MAX_READER_BUFFER_SIZE: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_DOIP_PAYLOAD_SIZE: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_TCP_BUFFER_SIZE: usize = 128 * 1024 * 1024;

/// Options controlling offline DoIP extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoIpCaptureOptions {
    /// Clear-text DoIP ports to inspect. The default is ISO 13400 port 13400.
    pub ports: Vec<u16>,
    /// Initial streaming buffer used by the pcapng parser.
    pub reader_buffer_size: usize,
    /// Largest pcapng parser buffer allowed for an unusually large block.
    pub max_reader_buffer_size: usize,
    /// Largest accepted DoIP payload length from a wire header.
    pub max_doip_payload_size: usize,
    /// Largest buffered directional TCP byte stream.
    pub max_tcp_buffer_size: usize,
}

impl Default for DoIpCaptureOptions {
    fn default() -> Self {
        Self {
            ports: vec![DEFAULT_PORT],
            reader_buffer_size: DEFAULT_READER_BUFFER_SIZE,
            max_reader_buffer_size: DEFAULT_MAX_READER_BUFFER_SIZE,
            max_doip_payload_size: DEFAULT_MAX_DOIP_PAYLOAD_SIZE,
            max_tcp_buffer_size: DEFAULT_MAX_TCP_BUFFER_SIZE,
        }
    }
}

impl DoIpCaptureOptions {
    fn validate(&self) -> Result<()> {
        if self.ports.is_empty() {
            return Err(Error::Parse(
                "pcapng: at least one clear-text DoIP port is required".into(),
            ));
        }
        if self.reader_buffer_size == 0 {
            return Err(Error::Parse(
                "pcapng: reader buffer size must be greater than zero".into(),
            ));
        }
        if self.max_reader_buffer_size < self.reader_buffer_size {
            return Err(Error::Parse(
                "pcapng: maximum reader buffer is smaller than the initial buffer".into(),
            ));
        }
        if self.max_doip_payload_size == 0 || self.max_tcp_buffer_size < HEADER_LEN {
            return Err(Error::Parse(
                "pcapng: DoIP and TCP buffer limits must be greater than zero".into(),
            ));
        }
        Ok(())
    }

    fn is_doip_port(&self, port: u16) -> bool {
        self.ports.contains(&port)
    }
}

/// Timestamp decoded from a pcapng interface's resolution and offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureTimestamp {
    /// Whole seconds relative to the Unix epoch.
    pub unix_seconds: i64,
    /// Fractional nanoseconds in `0..1_000_000_000`.
    pub nanoseconds: u32,
}

/// A decoded DoIP frame with capture-file metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedDoIpFrame {
    /// One-based packet-block index in the capture.
    pub packet_index: u64,
    /// Interface ID local to the current pcapng section.
    pub interface_id: u32,
    /// Capture time. Simple Packet Blocks do not carry a timestamp.
    pub timestamp: Option<CaptureTimestamp>,
    /// Network source endpoint.
    pub source: SocketAddr,
    /// Network destination endpoint.
    pub destination: SocketAddr,
    /// Decoded DoIP frame.
    pub frame: Frame,
}

/// Recoverable problem found while inspecting packet contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureIssueKind {
    /// Packet bytes were cut off by the capture snap length.
    TruncatedPacket,
    /// The packet refers to a missing interface description.
    MissingInterface,
    /// The interface uses an unsupported data-link type.
    UnsupportedLinkType,
    /// Ethernet, IP, TCP, or UDP headers are malformed or incomplete.
    MalformedNetworkPacket,
    /// An IP fragment cannot be decoded without datagram reassembly.
    FragmentedIpPacket,
    /// Clear-text DoIP bytes contain an invalid header or frame body.
    InvalidDoIpData,
    /// A TCP sequence gap left an incomplete directional stream.
    TcpSequenceGap,
    /// A stream or datagram ended with only part of a DoIP frame.
    IncompleteDoIpFrame,
    /// A configured safety limit was exceeded.
    LimitExceeded,
}

/// Description of a recoverable capture issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureIssue {
    /// One-based packet-block index, or zero for section/end-of-file issues.
    pub packet_index: u64,
    /// Stable issue category.
    pub kind: CaptureIssueKind,
    /// Human-readable details.
    pub message: String,
}

/// Aggregate counters collected while reading a pcapng file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoIpCaptureStatistics {
    pub sections: u64,
    pub interfaces: u64,
    pub packet_blocks: u64,
    pub tcp_segments: u64,
    pub udp_datagrams: u64,
    pub doip_frames: u64,
}

/// In-memory result of reading and decoding one pcapng capture.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoIpCapture {
    /// DoIP frames in capture order.
    pub frames: Vec<CapturedDoIpFrame>,
    /// Recoverable packet-level problems.
    pub issues: Vec<CaptureIssue>,
    /// Parsing and protocol counters.
    pub statistics: DoIpCaptureStatistics,
}

impl DoIpCapture {
    /// Opens a pcapng file with the default clear-text DoIP settings.
    pub fn open_pcapng(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_pcapng_with_options(path, DoIpCaptureOptions::default())
    }

    /// Opens a pcapng file with explicit limits and DoIP ports.
    pub fn open_pcapng_with_options(
        path: impl AsRef<Path>,
        options: DoIpCaptureOptions,
    ) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::read_pcapng_with_options(std::io::BufReader::new(file), options)
    }

    /// Reads a pcapng stream with the default clear-text DoIP settings.
    pub fn read_pcapng(reader: impl Read) -> Result<Self> {
        Self::read_pcapng_with_options(reader, DoIpCaptureOptions::default())
    }

    /// Reads a pcapng stream and reassembles clear-text DoIP TCP flows.
    pub fn read_pcapng_with_options(
        reader: impl Read,
        options: DoIpCaptureOptions,
    ) -> Result<Self> {
        options.validate()?;
        let mut parser = PcapNGReader::new(options.reader_buffer_size, reader)
            .map_err(|error| pcapng_error("cannot read section header", error))?;
        let mut state = CaptureState::new(options);
        let mut parser_capacity = state.options.reader_buffer_size;

        loop {
            match parser.next() {
                Ok((offset, block)) => {
                    state.process_block(block)?;
                    parser.consume(offset);
                }
                Err(PcapError::Eof) => break,
                Err(PcapError::Incomplete(_)) => parser
                    .refill()
                    .map_err(|error| pcapng_error("cannot refill parser buffer", error))?,
                Err(PcapError::BufferTooSmall) => {
                    let next_capacity = parser_capacity
                        .saturating_mul(2)
                        .min(state.options.max_reader_buffer_size);
                    if next_capacity <= parser_capacity || !parser.grow(next_capacity) {
                        return Err(Error::Parse(format!(
                            "pcapng: a block exceeds the configured {} byte parser limit",
                            state.options.max_reader_buffer_size
                        )));
                    }
                    parser_capacity = next_capacity;
                }
                Err(error) => return Err(pcapng_error("invalid capture block", error)),
            }
        }

        state.finish_streams();
        state.capture.statistics.doip_frames = state.capture.frames.len() as u64;
        Ok(state.capture)
    }
}

fn pcapng_error<I: std::fmt::Debug>(context: &str, error: PcapError<I>) -> Error {
    Error::Parse(format!("pcapng: {context}: {error}"))
}

#[derive(Debug, Clone, Copy)]
struct InterfaceInfo {
    linktype: Linktype,
    snaplen: u32,
    timestamp_resolution: u64,
    timestamp_offset: i64,
}

#[derive(Debug, Clone, Copy)]
struct FrameMetadata {
    packet_index: u64,
    interface_id: u32,
    timestamp: Option<CaptureTimestamp>,
    source: SocketAddr,
    destination: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FlowKey {
    source: SocketAddr,
    destination: SocketAddr,
}

#[derive(Debug)]
struct PendingSegment {
    sequence: u32,
    bytes: Vec<u8>,
}

#[derive(Debug, Default)]
struct TcpStream {
    next_sequence: Option<u32>,
    bytes: Vec<u8>,
    pending: Vec<PendingSegment>,
    last_packet_index: u64,
}

struct CaptureState {
    options: DoIpCaptureOptions,
    capture: DoIpCapture,
    interfaces: Vec<InterfaceInfo>,
    tcp_streams: HashMap<FlowKey, TcpStream>,
    packet_index: u64,
}

impl CaptureState {
    fn new(options: DoIpCaptureOptions) -> Self {
        Self {
            options,
            capture: DoIpCapture::default(),
            interfaces: Vec::new(),
            tcp_streams: HashMap::new(),
            packet_index: 0,
        }
    }

    fn process_block(&mut self, block: PcapBlockOwned<'_>) -> Result<()> {
        match block {
            PcapBlockOwned::NG(Block::SectionHeader(_)) => {
                self.finish_streams();
                self.interfaces.clear();
                self.capture.statistics.sections += 1;
            }
            PcapBlockOwned::NG(Block::InterfaceDescription(idb)) => {
                let timestamp_resolution = idb.ts_resolution().ok_or_else(|| {
                    Error::Parse("pcapng: interface timestamp resolution is invalid".into())
                })?;
                self.interfaces.push(InterfaceInfo {
                    linktype: idb.linktype,
                    snaplen: idb.snaplen,
                    timestamp_resolution,
                    timestamp_offset: idb.ts_offset(),
                });
                self.capture.statistics.interfaces += 1;
            }
            PcapBlockOwned::NG(Block::EnhancedPacket(epb)) => {
                self.packet_index += 1;
                self.capture.statistics.packet_blocks += 1;
                let Some(interface) = self.interfaces.get(epb.if_id as usize).copied() else {
                    self.issue(
                        CaptureIssueKind::MissingInterface,
                        format!("interface {} has no description in this section", epb.if_id),
                    );
                    return Ok(());
                };
                if epb.truncated() {
                    self.issue(
                        CaptureIssueKind::TruncatedPacket,
                        format!("captured {} of {} packet bytes", epb.caplen, epb.origlen),
                    );
                    return Ok(());
                }
                let timestamp = decode_timestamp(
                    epb.ts_high,
                    epb.ts_low,
                    interface.timestamp_resolution,
                    interface.timestamp_offset,
                );
                self.process_packet(epb.if_id, interface.linktype, timestamp, epb.packet_data());
            }
            PcapBlockOwned::NG(Block::SimplePacket(spb)) => {
                self.packet_index += 1;
                self.capture.statistics.packet_blocks += 1;
                let Some(interface) = self.interfaces.first().copied() else {
                    self.issue(
                        CaptureIssueKind::MissingInterface,
                        "simple packet has no interface 0 description".into(),
                    );
                    return Ok(());
                };
                let captured_len = usize::try_from(spb.origlen.min(interface.snaplen))
                    .unwrap_or(usize::MAX)
                    .min(spb.data.len());
                if captured_len < spb.origlen as usize {
                    self.issue(
                        CaptureIssueKind::TruncatedPacket,
                        format!(
                            "captured {captured_len} of {} simple-packet bytes",
                            spb.origlen
                        ),
                    );
                    return Ok(());
                }
                self.process_packet(0, interface.linktype, None, &spb.data[..captured_len]);
            }
            PcapBlockOwned::NG(_) => {}
            PcapBlockOwned::Legacy(_) | PcapBlockOwned::LegacyHeader(_) => {
                return Err(Error::Parse(
                    "pcapng: parser returned a legacy pcap block".into(),
                ));
            }
        }
        Ok(())
    }

    fn process_packet(
        &mut self,
        interface_id: u32,
        linktype: Linktype,
        timestamp: Option<CaptureTimestamp>,
        packet: &[u8],
    ) {
        let sliced = match slice_packet(linktype, packet) {
            Ok(value) => value,
            Err(PacketDecodeError::Unsupported(message)) => {
                self.issue(CaptureIssueKind::UnsupportedLinkType, message);
                return;
            }
            Err(PacketDecodeError::Malformed(message)) => {
                self.issue(CaptureIssueKind::MalformedNetworkPacket, message);
                return;
            }
        };
        if sliced.is_ip_payload_fragmented() {
            self.issue(
                CaptureIssueKind::FragmentedIpPacket,
                "fragmented IP packet requires datagram reassembly".into(),
            );
            return;
        }
        let Some((source_ip, destination_ip)) = ip_endpoints(sliced.net.as_ref()) else {
            return;
        };

        match sliced.transport {
            Some(TransportSlice::Udp(udp))
                if self.options.is_doip_port(udp.source_port())
                    || self.options.is_doip_port(udp.destination_port()) =>
            {
                self.capture.statistics.udp_datagrams += 1;
                let metadata = FrameMetadata {
                    packet_index: self.packet_index,
                    interface_id,
                    timestamp,
                    source: SocketAddr::new(source_ip, udp.source_port()),
                    destination: SocketAddr::new(destination_ip, udp.destination_port()),
                };
                self.process_udp(metadata, udp.payload());
            }
            Some(TransportSlice::Tcp(tcp))
                if self.options.is_doip_port(tcp.source_port())
                    || self.options.is_doip_port(tcp.destination_port()) =>
            {
                self.capture.statistics.tcp_segments += 1;
                let metadata = FrameMetadata {
                    packet_index: self.packet_index,
                    interface_id,
                    timestamp,
                    source: SocketAddr::new(source_ip, tcp.source_port()),
                    destination: SocketAddr::new(destination_ip, tcp.destination_port()),
                };
                self.process_tcp(
                    metadata,
                    tcp.sequence_number(),
                    tcp.syn(),
                    tcp.fin(),
                    tcp.rst(),
                    tcp.payload(),
                );
            }
            _ => {}
        }
    }

    fn process_udp(&mut self, metadata: FrameMetadata, payload: &[u8]) {
        let mut bytes = payload.to_vec();
        self.decode_frames(&mut bytes, metadata, SocketType::Dgram, true);
        if !bytes.is_empty() {
            self.issue_at(
                metadata.packet_index,
                CaptureIssueKind::IncompleteDoIpFrame,
                format!(
                    "UDP datagram ends with {} undecoded DoIP bytes",
                    bytes.len()
                ),
            );
        }
    }

    fn process_tcp(
        &mut self,
        metadata: FrameMetadata,
        sequence: u32,
        syn: bool,
        fin: bool,
        rst: bool,
        payload: &[u8],
    ) {
        let key = FlowKey {
            source: metadata.source,
            destination: metadata.destination,
        };
        let mut stream = self.tcp_streams.remove(&key).unwrap_or_default();
        if syn {
            if !stream.bytes.is_empty() || !stream.pending.is_empty() {
                self.finish_stream(key, stream);
                stream = TcpStream::default();
            }
            stream.next_sequence = Some(sequence.wrapping_add(1));
        }
        stream.last_packet_index = metadata.packet_index;

        if !payload.is_empty() {
            let payload_sequence = sequence.wrapping_add(u32::from(syn));
            self.insert_tcp_segment(&mut stream, payload_sequence, payload, metadata);
        }

        if fin || rst {
            self.finish_stream(key, stream);
        } else {
            self.tcp_streams.insert(key, stream);
        }
    }

    fn insert_tcp_segment(
        &mut self,
        stream: &mut TcpStream,
        sequence: u32,
        payload: &[u8],
        metadata: FrameMetadata,
    ) {
        if stream.next_sequence.is_none() {
            stream.next_sequence = Some(sequence);
        }
        let Some(next) = stream.next_sequence else {
            return;
        };
        match sequence_cmp(sequence, next) {
            Ordering::Greater => {
                if !stream
                    .pending
                    .iter()
                    .any(|pending| pending.sequence == sequence && pending.bytes == payload)
                {
                    stream.pending.push(PendingSegment {
                        sequence,
                        bytes: payload.to_vec(),
                    });
                }
                return;
            }
            Ordering::Equal => self.append_tcp_bytes(stream, payload, metadata),
            Ordering::Less => {
                let overlap = next.wrapping_sub(sequence) as usize;
                if overlap < payload.len() {
                    self.append_tcp_bytes(stream, &payload[overlap..], metadata);
                }
            }
        }

        loop {
            let Some(next) = stream.next_sequence else {
                break;
            };
            let Some(index) = stream.pending.iter().position(|pending| {
                sequence_cmp(pending.sequence, next) != Ordering::Greater
                    && next.wrapping_sub(pending.sequence) as usize <= pending.bytes.len()
            }) else {
                break;
            };
            let pending = stream.pending.swap_remove(index);
            let overlap = next.wrapping_sub(pending.sequence) as usize;
            if overlap < pending.bytes.len() {
                self.append_tcp_bytes(stream, &pending.bytes[overlap..], metadata);
            }
        }
    }

    fn append_tcp_bytes(&mut self, stream: &mut TcpStream, bytes: &[u8], metadata: FrameMetadata) {
        let Some(next) = stream.next_sequence else {
            return;
        };
        stream.next_sequence = Some(next.wrapping_add(bytes.len() as u32));
        if stream.bytes.len().saturating_add(bytes.len()) > self.options.max_tcp_buffer_size {
            self.issue_at(
                metadata.packet_index,
                CaptureIssueKind::LimitExceeded,
                format!(
                    "TCP stream exceeds the configured {} byte buffer limit",
                    self.options.max_tcp_buffer_size
                ),
            );
            stream.bytes.clear();
            stream.pending.clear();
            return;
        }
        stream.bytes.extend_from_slice(bytes);
        self.decode_frames(&mut stream.bytes, metadata, SocketType::Stream, false);
    }

    fn decode_frames(
        &mut self,
        bytes: &mut Vec<u8>,
        metadata: FrameMetadata,
        socket_type: SocketType,
        datagram: bool,
    ) {
        loop {
            if bytes.len() < HEADER_LEN {
                break;
            }
            if !valid_header_prefix(bytes) {
                let skip = next_header_offset(bytes).unwrap_or_else(|| {
                    if datagram {
                        bytes.len()
                    } else if matches!(bytes.last(), Some(1..=3)) {
                        bytes.len().saturating_sub(1)
                    } else {
                        bytes.len()
                    }
                });
                self.issue_at(
                    metadata.packet_index,
                    CaptureIssueKind::InvalidDoIpData,
                    format!("discarding {skip} bytes before the next DoIP header"),
                );
                bytes.drain(..skip);
                if skip == 0 {
                    break;
                }
                continue;
            }
            let payload_len = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
            if payload_len > self.options.max_doip_payload_size {
                self.issue_at(
                    metadata.packet_index,
                    CaptureIssueKind::LimitExceeded,
                    format!(
                        "DoIP payload length {payload_len} exceeds the configured {} byte limit",
                        self.options.max_doip_payload_size
                    ),
                );
                bytes.drain(..1);
                continue;
            }
            let Some(total_len) = HEADER_LEN.checked_add(payload_len) else {
                self.issue_at(
                    metadata.packet_index,
                    CaptureIssueKind::LimitExceeded,
                    "DoIP frame length exceeds the platform address space".into(),
                );
                bytes.drain(..1);
                continue;
            };
            if bytes.len() < total_len {
                break;
            }
            match Frame::decode(&bytes[..total_len], socket_type, Some(metadata.source)) {
                Ok(Some((mut frame, consumed))) => {
                    set_master_direction(&mut frame, self.is_master(metadata));
                    self.capture.frames.push(CapturedDoIpFrame {
                        packet_index: metadata.packet_index,
                        interface_id: metadata.interface_id,
                        timestamp: metadata.timestamp,
                        source: metadata.source,
                        destination: metadata.destination,
                        frame,
                    });
                    bytes.drain(..consumed);
                }
                Ok(None) => break,
                Err(error) => {
                    self.issue_at(
                        metadata.packet_index,
                        CaptureIssueKind::InvalidDoIpData,
                        error.to_string(),
                    );
                    bytes.drain(..total_len);
                }
            }
        }
    }

    fn is_master(&self, metadata: FrameMetadata) -> bool {
        self.options.is_doip_port(metadata.destination.port())
            && !self.options.is_doip_port(metadata.source.port())
    }

    fn finish_stream(&mut self, key: FlowKey, stream: TcpStream) {
        if !stream.pending.is_empty() {
            self.issue_at(
                stream.last_packet_index,
                CaptureIssueKind::TcpSequenceGap,
                format!(
                    "TCP flow {} -> {} ended with {} out-of-order segment(s)",
                    key.source,
                    key.destination,
                    stream.pending.len()
                ),
            );
        }
        if !stream.bytes.is_empty() {
            self.issue_at(
                stream.last_packet_index,
                CaptureIssueKind::IncompleteDoIpFrame,
                format!(
                    "TCP flow {} -> {} ended with {} undecoded DoIP bytes",
                    key.source,
                    key.destination,
                    stream.bytes.len()
                ),
            );
        }
    }

    fn finish_streams(&mut self) {
        let streams = std::mem::take(&mut self.tcp_streams);
        for (key, stream) in streams {
            self.finish_stream(key, stream);
        }
    }

    fn issue(&mut self, kind: CaptureIssueKind, message: String) {
        self.issue_at(self.packet_index, kind, message);
    }

    fn issue_at(&mut self, packet_index: u64, kind: CaptureIssueKind, message: String) {
        self.capture.issues.push(CaptureIssue {
            packet_index,
            kind,
            message,
        });
    }
}

fn sequence_cmp(left: u32, right: u32) -> Ordering {
    (left.wrapping_sub(right) as i32).cmp(&0)
}

fn valid_header_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && (1..=3).contains(&bytes[0]) && bytes[1] == !bytes[0]
}

fn next_header_offset(bytes: &[u8]) -> Option<usize> {
    (1..bytes.len().saturating_sub(1)).find(|&index| valid_header_prefix(&bytes[index..]))
}

fn set_master_direction(frame: &mut Frame, is_master: bool) {
    match frame {
        Frame::Base(value) => value.is_master_frame = is_master,
        Frame::Src(value) => value.base.is_master_frame = is_master,
        Frame::SrcDst(value) => value.src.base.is_master_frame = is_master,
    }
}

fn decode_timestamp(high: u32, low: u32, resolution: u64, offset: i64) -> Option<CaptureTimestamp> {
    if resolution == 0 {
        return None;
    }
    let raw = (u64::from(high) << 32) | u64::from(low);
    let seconds = raw / resolution;
    let unix_seconds = i64::try_from(seconds).ok()?.checked_add(offset)?;
    let fraction = raw % resolution;
    let nanoseconds = ((u128::from(fraction) * 1_000_000_000u128) / u128::from(resolution)) as u32;
    Some(CaptureTimestamp {
        unix_seconds,
        nanoseconds,
    })
}

enum PacketDecodeError {
    Unsupported(String),
    Malformed(String),
}

fn slice_packet(
    linktype: Linktype,
    packet: &[u8],
) -> std::result::Result<SlicedPacket<'_>, PacketDecodeError> {
    let result = match linktype {
        Linktype::ETHERNET => SlicedPacket::from_ethernet(packet),
        Linktype::RAW | Linktype::IPV4 | Linktype::IPV6 => SlicedPacket::from_ip(packet),
        Linktype::LINUX_SLL => SlicedPacket::from_linux_sll(packet),
        Linktype::NULL | Linktype::LOOP => {
            let Some(ip_packet) = packet.get(4..) else {
                return Err(PacketDecodeError::Malformed(format!(
                    "link type {} packet is shorter than its four-byte family field",
                    linktype.0
                )));
            };
            SlicedPacket::from_ip(ip_packet)
        }
        Linktype::LINUX_SLL2 => {
            let Some(protocol) = packet.get(..2) else {
                return Err(PacketDecodeError::Malformed(
                    "Linux SLL2 packet is shorter than its protocol field".into(),
                ));
            };
            let Some(payload) = packet.get(20..) else {
                return Err(PacketDecodeError::Malformed(
                    "Linux SLL2 packet is shorter than its 20-byte header".into(),
                ));
            };
            let ether_type = EtherType(u16::from_be_bytes([protocol[0], protocol[1]]));
            SlicedPacket::from_ether_type(ether_type, payload)
        }
        _ => {
            return Err(PacketDecodeError::Unsupported(format!(
                "data-link type {} is not supported for DoIP extraction",
                linktype.0
            )));
        }
    };
    result.map_err(|error| PacketDecodeError::Malformed(error.to_string()))
}

fn ip_endpoints(net: Option<&NetSlice<'_>>) -> Option<(IpAddr, IpAddr)> {
    match net? {
        NetSlice::Ipv4(ip) => Some((
            IpAddr::V4(ip.header().source_addr()),
            IpAddr::V4(ip.header().destination_addr()),
        )),
        NetSlice::Ipv6(ip) => Some((
            IpAddr::V6(ip.header().source_addr()),
            IpAddr::V6(ip.header().destination_addr()),
        )),
        NetSlice::Arp(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doip::DoIpType;
    use etherparse::PacketBuilder;

    fn pcapng(packets: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(&0x0A0D_0D0Au32.to_le_bytes());
        output.extend_from_slice(&28u32.to_le_bytes());
        output.extend_from_slice(&0x1A2B_3C4Du32.to_le_bytes());
        output.extend_from_slice(&1u16.to_le_bytes());
        output.extend_from_slice(&0u16.to_le_bytes());
        output.extend_from_slice(&u64::MAX.to_le_bytes());
        output.extend_from_slice(&28u32.to_le_bytes());

        output.extend_from_slice(&1u32.to_le_bytes());
        output.extend_from_slice(&20u32.to_le_bytes());
        output.extend_from_slice(&1u16.to_le_bytes());
        output.extend_from_slice(&0u16.to_le_bytes());
        output.extend_from_slice(&65_535u32.to_le_bytes());
        output.extend_from_slice(&20u32.to_le_bytes());

        for (timestamp_micros, packet) in packets {
            let padded_len = (packet.len() + 3) & !3;
            let block_len = 32 + padded_len;
            output.extend_from_slice(&6u32.to_le_bytes());
            output.extend_from_slice(&(block_len as u32).to_le_bytes());
            output.extend_from_slice(&0u32.to_le_bytes());
            output.extend_from_slice(&((*timestamp_micros >> 32) as u32).to_le_bytes());
            output.extend_from_slice(&(*timestamp_micros as u32).to_le_bytes());
            output.extend_from_slice(&(packet.len() as u32).to_le_bytes());
            output.extend_from_slice(&(packet.len() as u32).to_le_bytes());
            output.extend_from_slice(packet);
            output.resize(output.len() + padded_len - packet.len(), 0);
            output.extend_from_slice(&(block_len as u32).to_le_bytes());
        }
        output
    }

    fn udp_packet(payload: &[u8]) -> Vec<u8> {
        let builder = PacketBuilder::ethernet2([1; 6], [2; 6])
            .ipv4([192, 0, 2, 10], [192, 0, 2, 20], 64)
            .udp(50_000, DEFAULT_PORT);
        let mut packet = Vec::with_capacity(builder.size(payload.len()));
        builder.write(&mut packet, payload).unwrap();
        packet
    }

    fn tcp_packet(sequence: u32, payload: &[u8]) -> Vec<u8> {
        let builder = PacketBuilder::ethernet2([1; 6], [2; 6])
            .ipv4([192, 0, 2, 10], [192, 0, 2, 20], 64)
            .tcp(50_000, DEFAULT_PORT, sequence, 8192);
        let mut packet = Vec::with_capacity(builder.size(payload.len()));
        builder.write(&mut packet, payload).unwrap();
        packet
    }

    fn tcp_syn(sequence: u32) -> Vec<u8> {
        let builder = PacketBuilder::ethernet2([1; 6], [2; 6])
            .ipv4([192, 0, 2, 10], [192, 0, 2, 20], 64)
            .tcp(50_000, DEFAULT_PORT, sequence, 8192)
            .syn();
        let mut packet = Vec::with_capacity(builder.size(0));
        builder.write(&mut packet, &[]).unwrap();
        packet
    }

    fn diagnostic_message() -> Vec<u8> {
        vec![
            0x02, 0xFD, 0x80, 0x01, 0, 0, 0, 7, 0x0E, 0x80, 0x10, 0x00, 0x22, 0xF1, 0x90,
        ]
    }

    #[test]
    fn reads_udp_frame_and_timestamp() {
        let wire = diagnostic_message();
        let capture = DoIpCapture::read_pcapng(std::io::Cursor::new(pcapng(&[(
            1_500_001,
            udp_packet(&wire),
        )])))
        .unwrap();
        assert!(capture.issues.is_empty());
        assert_eq!(capture.frames.len(), 1);
        assert_eq!(capture.statistics.udp_datagrams, 1);
        assert_eq!(
            capture.frames[0].timestamp,
            Some(CaptureTimestamp {
                unix_seconds: 1,
                nanoseconds: 500_001_000
            })
        );
        assert_eq!(
            capture.frames[0].frame.base().msg_type(),
            Some(DoIpType::DiagnosticMessage)
        );
        assert!(capture.frames[0].frame.base().is_master_frame);
    }

    #[test]
    fn reassembles_tcp_frame_across_packets_and_ignores_retransmission() {
        let wire = diagnostic_message();
        let packets = vec![
            (10, tcp_packet(100, &wire[..5])),
            (20, tcp_packet(100, &wire[..5])),
            (30, tcp_packet(105, &wire[5..])),
        ];
        let capture = DoIpCapture::read_pcapng(std::io::Cursor::new(pcapng(&packets))).unwrap();
        assert!(capture.issues.is_empty());
        assert_eq!(capture.frames.len(), 1);
        assert_eq!(capture.frames[0].packet_index, 3);
        assert_eq!(capture.statistics.tcp_segments, 3);
    }

    #[test]
    fn reorders_tcp_segments_after_syn() {
        let wire = diagnostic_message();
        let packets = vec![
            (10, tcp_syn(99)),
            (20, tcp_packet(105, &wire[5..])),
            (30, tcp_packet(100, &wire[..5])),
        ];
        let capture = DoIpCapture::read_pcapng(std::io::Cursor::new(pcapng(&packets))).unwrap();
        assert!(capture.issues.is_empty());
        assert_eq!(capture.frames.len(), 1);
        assert_eq!(capture.frames[0].packet_index, 3);
    }

    #[test]
    fn reports_invalid_udp_payload_without_failing_the_file() {
        let capture = DoIpCapture::read_pcapng(std::io::Cursor::new(pcapng(&[(
            1,
            udp_packet(&[0xAA; 12]),
        )])))
        .unwrap();
        assert!(capture.frames.is_empty());
        assert!(capture
            .issues
            .iter()
            .any(|issue| issue.kind == CaptureIssueKind::InvalidDoIpData));
    }

    #[test]
    fn rejects_truncated_pcapng_block() {
        let mut bytes = pcapng(&[(1, udp_packet(&diagnostic_message()))]);
        bytes.truncate(bytes.len() - 2);
        let error = DoIpCapture::read_pcapng(std::io::Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("pcapng"));
    }
}
