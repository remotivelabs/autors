//! DoIP (ISO 13400, Diagnostics over IP) message layer.
//! This module is the message/byte layer: encoding (`to_array`) and decoding
//! ([`Frame::decode`]) of headers and payloads, plus parsing of the various
//! response payloads. The socket transport client (connect, routing
//! activation, diagnostic message exchange, Nack handling, timeouts) lives
//! in the [`crate::doip_client`] module.
//! Live vehicle-identification discovery is built on these codecs by
//! [`crate::doip_client::DoIpClient::discover`]. Frame queues and file logging
//! remain outside this message layer.
//!
//! Byte order: header and address fields are big-endian (network byte
//! order), consistent with ISO 13400-2.

use crate::error::{Error, Result};
use std::fmt;
use std::net::SocketAddr;

/// Numeric enum macro generating `from_num`/`to_num`. Unknown numeric values
/// return `None` from `from_num`; call sites convert to `Error::Parse` as
/// needed.
macro_rules! num_enum {
    ($(#[$meta:meta])* $vis:vis $name:ident : $t:ty { $( $(#[$vmeta:meta])* $var:ident = $val:expr ),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr($t)]
        $vis enum $name {
            $( $(#[$vmeta])* $var = $val ),+
        }
        impl $name {
            /// Convert a numeric value to the enum; unknown values return `None`.
            pub fn from_num(v: $t) -> Option<Self> {
                match v {
                    $( x if x == Self::$var as $t => Some(Self::$var), )+
                    _ => None,
                }
            }
            /// Convert the enum to its numeric value.
            pub fn to_num(self) -> $t {
                self as $t
            }
        }
    };
}

num_enum! {
    /// Further action code in a vehicle identification response.
    pub ActionType : u8 {
        /// No further action required.
        NoFurtherActionRequired = 0x00,
        /// Routing activation required.
        RoutingActivationRequired = 0x10,
        /// Unknown (fallback when the data is missing).
        Unknown = 0xFF,
    }
}

num_enum! {
    /// Routing activation type (ISO 13400-2).
    pub Activation : u8 {
        /// Default activation mode.
        Default = 0x00,
        /// WWH-OBD required.
        WwhObd = 0x01,
        /// Central security.
        CentralSecurity = 0xE0,
    }
}

num_enum! {
    /// Routing activation response code.
    pub ActivationCode : u8 {
        /// Unknown source address.
        UnknowSource = 0x00,
        /// Denied: too many concurrent sockets.
        DeniedSocketsExceeded = 0x01,
        /// Denied: SA differs from the already activated socket.
        DeniedDifferentSA = 0x02,
        /// Denied: SA already registered by another socket.
        DeniedRegisteredSA = 0x03,
        /// Denied: missing authentication.
        DeniedMissingAuth = 0x04,
        /// Denied: confirmation rejected.
        DeniedRejectedConfirmation = 0x05,
        /// Denied: unsupported activation type.
        DeniedUnsupportedActivationType = 0x06,
        /// Denied: the activation type requires a secure socket (TLS).
        DeniedActivationTypeRequiresASecureSocket = 0x07,
        /// Routing activated.
        RoutingActivated = 0x10,
        /// Activated, but confirmation required (ISO 13400-2:2019).
        RoutingActivationConfirmationRequired = 0x11,
    }
}

num_enum! {
    /// Diagnostic message negative acknowledgment code.
    pub DiagMsgNackCode : u8 {
        /// Invalid source address.
        InvalidSA = 0x02,
        /// Unknown target address.
        UnknownTA = 0x03,
        /// Message too large.
        MessageTooLarge = 0x04,
        /// Out of memory.
        OutOfMemory = 0x05,
        /// Target unreachable.
        TargetUnreachable = 0x06,
        /// Unknown network.
        UnknownNetwork = 0x07,
        /// Transport layer error.
        TpError = 0x08,
        /// None (fallback when no Nack was received).
        None = 0xFF,
    }
}

num_enum! {
    /// Payload type (ISO 13400-2).
    pub DoIpType : u16 {
        /// Generic DoIP header negative acknowledgment.
        Nack = 0x0000,
        /// Vehicle identification request.
        VehicleIdRequest = 0x0001,
        /// Vehicle identification request by EID.
        VehicleIdRequestEID = 0x0002,
        /// Vehicle identification request by VIN.
        VehicleIdRequestVIN = 0x0003,
        /// Vehicle identification response / vehicle announcement.
        VehicleIdResponse = 0x0004,
        /// Routing activation request.
        RoutingActivationRequest = 0x0005,
        /// Routing activation response.
        RoutingActivationResponse = 0x0006,
        /// Alive check request.
        AliveCheckRequest = 0x0007,
        /// Alive check response.
        AliveCheckResponse = 0x0008,
        /// Entity status request.
        EntityStatusRequest = 0x4001,
        /// Entity status response.
        EntityStatusResponse = 0x4002,
        /// Diagnostic power mode request.
        DiagnosticPowerModeRequest = 0x4003,
        /// Diagnostic power mode response.
        DiagnosticPowerModeResponse = 0x4004,
        /// Diagnostic message.
        DiagnosticMessage = 0x8001,
        /// Diagnostic message positive acknowledgment.
        DiagnosticMessageAck = 0x8002,
        /// Diagnostic message negative acknowledgment.
        DiagnosticMessageNack = 0x8003,
        /// Periodic diagnostic message (reserved).
        PeriodicDiagnosticMessage = 0x8004,
    }
}

num_enum! {
    /// Generic DoIP header Nack code.
    pub NackCode : u8 {
        /// Incorrect header pattern format.
        IncorrectPatternFormat = 0x00,
        /// Unknown payload type.
        UnknownPayloadType = 0x01,
        /// Message too large.
        MessageTooLarge = 0x02,
        /// Out of memory.
        OutOfMemory = 0x03,
        /// Invalid payload length.
        InvalidPayloadLength = 0x04,
        /// None (fallback when no Nack was received).
        None = 0xFF,
    }
}

num_enum! {
    /// Node type in an entity status response.
    pub NodeType : u8 {
        /// Gateway.
        Gateway = 0x00,
        /// Regular node.
        Node = 0x01,
    }
}

num_enum! {
    /// Diagnostic power mode.
    pub PowerMode : u8 {
        /// Not ready.
        NotReady = 0x00,
        /// Ready.
        Ready = 0x01,
        /// Not supported.
        NotSupported = 0x02,
    }
}

num_enum! {
    /// DoIP protocol version.
    pub ProtocolVersion : u8 {
        /// ISO 13400-2:2010.
        Iso13400_2010 = 0x01,
        /// ISO 13400-2:2012.
        Iso13400_2012 = 0x02,
        /// ISO 13400-2:2019.
        Iso13400_2019 = 0x03,
    }
}

num_enum! {
    /// VIN/GID synchronization status.
    pub SynchStatus : u8 {
        /// Synchronized.
        Synchronized = 0x00,
        /// Incomplete.
        Incomplete = 0x10,
        /// Unknown (fallback when the data is missing).
        Unknown = 0xFF,
    }
}

/// Request-response result state.
/// `Default` is `Timeout` (the initial state of a pending response wait).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ResponseState {
    /// Waiting for the response timed out.
    #[default]
    Timeout,
    /// Sending failed.
    SendFailed,
    /// A negative acknowledgment was received.
    Nack,
    /// A positive acknowledgment was received (diagnostic message Ack).
    Ack,
    /// The response was received successfully.
    Ok,
}

/// Socket type (TCP stream / UDP datagram).
/// Carried with a frame purely as a source marker; no socket operations are
/// involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocketType {
    /// TCP.
    Stream,
    /// UDP.
    Dgram,
}

/// Default DoIP port.
pub const DEFAULT_PORT: u16 = 13400;
/// Default DoIP TLS port.
pub const DEFAULT_TLS_PORT: u16 = 3496;
/// Factory default timeout (milliseconds).
pub const DEFAULT_TIMEOUT_MS: u32 = 200;
/// Default tester logical address (0x0E80).
pub const DEFAULT_TESTER_ADR: u16 = 3712;
/// Default entity logical address (0x1000).
pub const DEFAULT_ENTITY_ADR: u16 = 4096;

/// DoIP header length (version + inverse version + payload type + payload length).
pub const HEADER_LEN: usize = 8;

/// Format data as uppercase hex bytes joined by dashes (`AA-BB-CC`).
fn hex_dash(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Common part of every DoIP frame.
/// Only the wire-relevant semantics are modeled: `data` and
/// `is_master_frame`; logging timestamps and similar transport/logging
/// facilities are intentionally not represented.
/// Note that `msg_type` is stored as a raw `u16`: arbitrary payload types
/// (including OEM extensions) are allowed and decoding an unknown type does
/// not fail; use [`BaseFrame::msg_type`] to obtain the standard type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseFrame {
    /// Protocol version.
    pub version: ProtocolVersion,
    /// Raw payload type value (see [`DoIpType`]).
    pub msg_type: u16,
    /// Socket type the frame originates from (TCP/UDP).
    pub socket_type: SocketType,
    /// Sender endpoint (only present on decoded frames; constructed request
    /// frames may have `None`).
    pub sender: Option<SocketAddr>,
    /// Payload data (without the header; for addressed frames also without
    /// the source/target addresses).
    pub data: Vec<u8>,
    /// Whether the frame was sent by the master (tester).
    pub is_master_frame: bool,
}

impl BaseFrame {
    /// Create a frame with a standard payload type.
    pub fn new(
        version: ProtocolVersion,
        msg_type: DoIpType,
        socket_type: SocketType,
        sender: Option<SocketAddr>,
        data: Vec<u8>,
        is_master_frame: bool,
    ) -> Self {
        Self::with_raw_type(
            version,
            msg_type.to_num(),
            socket_type,
            sender,
            data,
            is_master_frame,
        )
    }

    /// Create a frame with a raw payload type value (for OEM extensions and
    /// other non-standard types).
    pub fn with_raw_type(
        version: ProtocolVersion,
        msg_type: u16,
        socket_type: SocketType,
        sender: Option<SocketAddr>,
        data: Vec<u8>,
        is_master_frame: bool,
    ) -> Self {
        BaseFrame {
            version,
            msg_type,
            socket_type,
            sender,
            data,
            is_master_frame,
        }
    }

    /// Standard payload type; unknown (OEM extension) values return `None`.
    pub fn msg_type(&self) -> Option<DoIpType> {
        DoIpType::from_num(self.msg_type)
    }

    /// Whether the frame is an error frame:
    /// `Type != Nack ? Type == DiagnosticMessageNack : true`.
    pub fn is_error(&self) -> bool {
        self.msg_type == DoIpType::Nack.to_num()
            || self.msg_type == DoIpType::DiagnosticMessageNack.to_num()
    }

    /// Build the header: version, bitwise-inverted version, payload type
    /// (2 bytes big-endian), payload length (4 bytes big-endian).
    pub fn build_header(&self, payload_len: usize) -> [u8; HEADER_LEN] {
        let mut h = [0u8; HEADER_LEN];
        h[0] = self.version.to_num();
        h[1] = !h[0];
        h[2..4].copy_from_slice(&self.msg_type.to_be_bytes());
        h[4..8].copy_from_slice(&(payload_len as u32).to_be_bytes());
        h
    }

    /// Serialize: header + payload.
    pub fn to_array(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len());
        out.extend_from_slice(&self.build_header(self.data.len()));
        out.extend_from_slice(&self.data);
        out
    }
}

impl fmt::Display for BaseFrame {
    /// Formats as `Version Type AA-BB-...` (payload as dashed hex).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let type_str = self
            .msg_type()
            .map(|t| format!("{t:?}"))
            .unwrap_or_else(|| format!("0x{:04X}", self.msg_type));
        write!(
            f,
            "{:?} {} {}",
            self.version,
            type_str,
            hex_dash(&self.data)
        )
    }
}

/// Frame whose payload starts with a 2-byte source address.
/// Composed by flattening: the common part lives in `base` (no inheritance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrcFrame {
    /// Common message part.
    pub base: BaseFrame,
    /// Source logical address.
    pub src_adr: u16,
}

impl SrcFrame {
    /// Create a source-addressed frame.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: ProtocolVersion,
        msg_type: DoIpType,
        src_adr: u16,
        socket_type: SocketType,
        sender: Option<SocketAddr>,
        data: Vec<u8>,
        is_master_frame: bool,
    ) -> Self {
        SrcFrame {
            base: BaseFrame::new(
                version,
                msg_type,
                socket_type,
                sender,
                data,
                is_master_frame,
            ),
            src_adr,
        }
    }

    /// Serialize: header (length includes the source address) + source
    /// address (big-endian) + payload.
    pub fn to_array(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + 2 + self.base.data.len());
        out.extend_from_slice(&self.base.build_header(self.base.data.len() + 2));
        out.extend_from_slice(&self.src_adr.to_be_bytes());
        out.extend_from_slice(&self.base.data);
        out
    }
}

impl fmt::Display for SrcFrame {
    /// Formats as `<base> SrcAdr` (source address as 4-digit hex).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:04X}", self.base, self.src_adr)
    }
}

/// Frame whose payload starts with a source address and a target address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrcDstFrame {
    /// Message and source address part.
    pub src: SrcFrame,
    /// Target logical address.
    pub dst_adr: u16,
}

impl SrcDstFrame {
    /// Create a source/target-addressed frame.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: ProtocolVersion,
        msg_type: DoIpType,
        src_adr: u16,
        dst_adr: u16,
        socket_type: SocketType,
        sender: Option<SocketAddr>,
        data: Vec<u8>,
        is_master_frame: bool,
    ) -> Self {
        SrcDstFrame {
            src: SrcFrame::new(
                version,
                msg_type,
                src_adr,
                socket_type,
                sender,
                data,
                is_master_frame,
            ),
            dst_adr,
        }
    }

    /// Build the reply frame to `request` (swaps source/target addresses,
    /// `is_master_frame = true`).
    pub fn reply(
        request: &SrcDstFrame,
        msg_type: DoIpType,
        socket_type: SocketType,
        sender: Option<SocketAddr>,
        data: Vec<u8>,
    ) -> Self {
        SrcDstFrame {
            src: SrcFrame {
                base: BaseFrame::new(
                    request.src.base.version,
                    msg_type,
                    socket_type,
                    sender,
                    data,
                    true,
                ),
                src_adr: request.dst_adr,
            },
            dst_adr: request.src.src_adr,
        }
    }

    /// Serialize: header (length includes both addresses) + source address +
    /// target address + payload.
    pub fn to_array(&self) -> Vec<u8> {
        let data = &self.src.base.data;
        let mut out = Vec::with_capacity(HEADER_LEN + 4 + data.len());
        out.extend_from_slice(&self.src.base.build_header(data.len() + 4));
        out.extend_from_slice(&self.src.src_adr.to_be_bytes());
        out.extend_from_slice(&self.dst_adr.to_be_bytes());
        out.extend_from_slice(data);
        out
    }
}

impl fmt::Display for SrcDstFrame {
    /// Formats as `Type SrcAdr->DstAdr AA-BB-...` (addresses as 4-digit hex).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.src.base;
        let type_str = b
            .msg_type()
            .map(|t| format!("{t:?}"))
            .unwrap_or_else(|| format!("0x{:04X}", b.msg_type));
        write!(
            f,
            "{} {:04X}->{:04X} {}",
            type_str,
            self.src.src_adr,
            self.dst_adr,
            hex_dash(&b.data)
        )
    }
}

/// Decoded DoIP frame (an enum instead of a class hierarchy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Frame without address fields.
    Base(BaseFrame),
    /// Frame with a source address.
    Src(SrcFrame),
    /// Frame with source and target addresses.
    SrcDst(SrcDstFrame),
}

impl Frame {
    /// Access the common part of the frame.
    pub fn base(&self) -> &BaseFrame {
        match self {
            Frame::Base(f) => f,
            Frame::Src(f) => &f.base,
            Frame::SrcDst(f) => &f.src.base,
        }
    }

    /// Encode to bytes (dispatching on the concrete frame type).
    pub fn to_array(&self) -> Vec<u8> {
        match self {
            Frame::Base(f) => f.to_array(),
            Frame::Src(f) => f.to_array(),
            Frame::SrcDst(f) => f.to_array(),
        }
    }

    /// Decode one frame from the start of `buf` (stream-buffer unpacking):
    /// - returns `Ok(None)` when less than a complete frame is available
    ///   (header or payload incomplete);
    /// - returns `Err` when the header pattern is invalid (version not in
    ///   1..=3 or the inverse version does not match);
    /// - on success returns the frame and the number of consumed bytes.
    ///
    /// Frames whose address payload (source/target address) is too short
    /// also return `Err(Error::Parse)`.
    pub fn decode(
        buf: &[u8],
        socket_type: SocketType,
        sender: Option<SocketAddr>,
    ) -> Result<Option<(Frame, usize)>> {
        if buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let ver = buf[0];
        if !(1..=3).contains(&ver) || buf[1] != !ver {
            return Err(Error::Parse(format!(
                "DoIP: invalid header pattern {ver:02X} {:02X}",
                buf[1]
            )));
        }
        // The check above guarantees 1..=3, so from_num cannot fail.
        let version = ProtocolVersion::from_num(ver)
            .ok_or_else(|| Error::Parse(format!("DoIP: unsupported version {ver}")))?;
        let msg_type = u16::from_be_bytes([buf[2], buf[3]]);
        let payload_len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        if payload_len > buf.len() - HEADER_LEN {
            return Ok(None);
        }
        let payload = &buf[HEADER_LEN..HEADER_LEN + payload_len];
        let frame = match msg_type {
            // Frames with source/target addresses.
            t if t == DoIpType::RoutingActivationResponse.to_num()
                || t == DoIpType::DiagnosticMessage.to_num()
                || t == DoIpType::DiagnosticMessageAck.to_num()
                || t == DoIpType::DiagnosticMessageNack.to_num() =>
            {
                if payload_len < 4 {
                    return Err(Error::Parse(format!(
                        "DoIP: payload too short for addresses: {payload_len}"
                    )));
                }
                let src_adr = u16::from_be_bytes([payload[0], payload[1]]);
                let dst_adr = u16::from_be_bytes([payload[2], payload[3]]);
                Frame::SrcDst(SrcDstFrame {
                    src: SrcFrame {
                        base: BaseFrame::with_raw_type(
                            version,
                            msg_type,
                            socket_type,
                            sender,
                            payload[4..].to_vec(),
                            false,
                        ),
                        src_adr,
                    },
                    dst_adr,
                })
            }
            // Frames with a source address.
            t if t == DoIpType::RoutingActivationRequest.to_num()
                || t == DoIpType::AliveCheckResponse.to_num() =>
            {
                if payload_len < 2 {
                    return Err(Error::Parse(format!(
                        "DoIP: payload too short for source address: {payload_len}"
                    )));
                }
                let src_adr = u16::from_be_bytes([payload[0], payload[1]]);
                Frame::Src(SrcFrame {
                    base: BaseFrame::with_raw_type(
                        version,
                        msg_type,
                        socket_type,
                        sender,
                        payload[2..].to_vec(),
                        false,
                    ),
                    src_adr,
                })
            }
            // Everything else (including unknown/OEM extension types): no
            // address fields.
            _ => Frame::Base(BaseFrame::with_raw_type(
                version,
                msg_type,
                socket_type,
                sender,
                payload.to_vec(),
                false,
            )),
        };
        Ok(Some((frame, HEADER_LEN + payload_len)))
    }
}

/// Vehicle identification response (payload type 0x0004).
/// The payload is parsed once in `parse` with length-based fallbacks
/// (missing data → empty string / 0 / empty array / Unknown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleIdResponse {
    /// VIN (17 bytes ASCII; empty string when fewer than 17 bytes are
    /// present). Non-ASCII bytes are mapped to '?'.
    pub vin: String,
    /// Logical address (offset 17, big-endian; 0 when missing).
    pub logical_adr: u16,
    /// EID (offset 19, 6 bytes; empty when missing).
    pub eid: Vec<u8>,
    /// GID (offset 25, 6 bytes; empty when missing).
    pub gid: Vec<u8>,
    /// Further action (offset 31; Unknown when missing).
    pub further_action: ActionType,
    /// VIN/GID synchronization status (offset 32; Unknown when missing).
    pub synch_status: SynchStatus,
    /// Sender endpoint of the frame (used by [`VehicleIdResponse::key`]).
    pub sender: Option<SocketAddr>,
}

impl VehicleIdResponse {
    /// Parse the frame payload with length-based fallbacks.
    pub fn parse(frame: &BaseFrame) -> Self {
        let d = &frame.data;
        let vin = if d.len() < 17 {
            String::new()
        } else {
            d[..17]
                .iter()
                .map(|&b| if b.is_ascii() { b as char } else { '?' })
                .collect()
        };
        let logical_adr = if d.len() >= 19 {
            u16::from_be_bytes([d[17], d[18]])
        } else {
            0
        };
        let eid = if d.len() >= 25 {
            d[19..25].to_vec()
        } else {
            Vec::new()
        };
        let gid = if d.len() >= 31 {
            d[25..31].to_vec()
        } else {
            Vec::new()
        };
        // Deliberate leniency: a payload ending exactly at offset 31/32 falls
        // back to Unknown rather than erroring on the out-of-range index.
        let further_action = d
            .get(31)
            .and_then(|&b| ActionType::from_num(b))
            .unwrap_or(ActionType::Unknown);
        let synch_status = d
            .get(32)
            .and_then(|&b| SynchStatus::from_num(b))
            .unwrap_or(SynchStatus::Unknown);
        VehicleIdResponse {
            vin,
            logical_adr,
            eid,
            gid,
            further_action,
            synch_status,
            sender: frame.sender,
        }
    }

    /// Registry key: sender endpoint string directly concatenated with the
    /// logical address in decimal (e.g. `192.168.0.10:134004097`); empty
    /// string when no sender is present.
    pub fn key(&self) -> String {
        match &self.sender {
            Some(ep) => format!("{}{}", ep, self.logical_adr),
            None => String::new(),
        }
    }
}

/// Entity status response.
/// Parsing requires the full 7-byte payload; short payloads return
/// `Err(Error::Parse)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityStatusResponse {
    /// Node type (gateway/node).
    pub node_type: NodeType,
    /// Maximum number of concurrent sockets.
    pub max_sockets: u8,
    /// Number of currently open sockets.
    pub open_sockets: u8,
    /// Maximum data size of a single message (big-endian u32).
    pub max_data_size: u32,
}

impl EntityStatusResponse {
    /// Parse from the frame payload (requires at least 7 bytes).
    pub fn parse(frame: &BaseFrame) -> Result<Self> {
        let d = &frame.data;
        if d.len() < 7 {
            return Err(Error::Parse(format!(
                "DoIP EntityStatusResponse: payload too short: {}",
                d.len()
            )));
        }
        let node_type = NodeType::from_num(d[0])
            .ok_or_else(|| Error::Parse(format!("DoIP: unknown node type {:#04X}", d[0])))?;
        Ok(EntityStatusResponse {
            node_type,
            max_sockets: d[1],
            open_sockets: d[2],
            max_data_size: u32::from_be_bytes([d[3], d[4], d[5], d[6]]),
        })
    }
}

/// Diagnostic power mode response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticPowerModeResponse {
    /// Power mode.
    pub power_mode: PowerMode,
}

impl DiagnosticPowerModeResponse {
    /// Parse from the frame payload; an empty payload returns `Err`.
    pub fn parse(frame: &BaseFrame) -> Result<Self> {
        let &b = frame.data.first().ok_or_else(|| {
            Error::Parse("DoIP DiagnosticPowerModeResponse: empty payload".to_string())
        })?;
        let power_mode = PowerMode::from_num(b)
            .ok_or_else(|| Error::Parse(format!("DoIP: unknown power mode {b:#04X}")))?;
        Ok(DiagnosticPowerModeResponse { power_mode })
    }
}

/// Routing activation response.
/// The frame has already been split into addresses and data by
/// [`Frame::decode`], so `data[0]` is the activation result code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingActivationResponse {
    /// Activation result code.
    pub activation_result: ActivationCode,
}

impl RoutingActivationResponse {
    /// Parse from the frame payload; an empty payload returns `Err`.
    pub fn parse(frame: &BaseFrame) -> Result<Self> {
        let &b = frame.data.first().ok_or_else(|| {
            Error::Parse("DoIP RoutingActivationResponse: empty payload".to_string())
        })?;
        let activation_result = ActivationCode::from_num(b)
            .ok_or_else(|| Error::Parse(format!("DoIP: unknown activation code {b:#04X}")))?;
        Ok(RoutingActivationResponse { activation_result })
    }
}

/// Diagnostic message payload (UDS/KWP service data; the addresses have
/// already been split off by [`Frame::decode`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiagnosticMessage {
    /// Diagnostic payload (e.g. UDS service data).
    pub data: Vec<u8>,
}

impl DiagnosticMessage {
    /// Construct from the frame payload.
    pub fn parse(frame: &BaseFrame) -> Self {
        DiagnosticMessage {
            data: frame.data.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V2019: ProtocolVersion = ProtocolVersion::Iso13400_2019;

    fn base_frame(msg_type: DoIpType, data: &[u8]) -> BaseFrame {
        BaseFrame::new(
            V2019,
            msg_type,
            SocketType::Stream,
            None,
            data.to_vec(),
            false,
        )
    }

    // Wire-encoding checks for BaseFrame/SrcFrame/SrcDstFrame::to_array.

    #[test]
    fn base_frame_to_array_header_big_endian() {
        let f = base_frame(DoIpType::DiagnosticMessage, &[]);
        assert_eq!(
            f.to_array(),
            vec![0x03, 0xFC, 0x80, 0x01, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn src_dst_frame_to_array() {
        let f = SrcDstFrame::new(
            V2019,
            DoIpType::DiagnosticMessage,
            0x0E80,
            0x1000,
            SocketType::Stream,
            None,
            vec![0x22, 0xF1, 0x90],
            true,
        );
        assert_eq!(
            f.to_array(),
            vec![
                0x03, 0xFC, 0x80, 0x01, 0x00, 0x00, 0x00, 0x07, 0x0E, 0x80, 0x10, 0x00, 0x22, 0xF1,
                0x90
            ]
        );
    }

    #[test]
    fn src_frame_to_array() {
        let f = SrcFrame::new(
            V2019,
            DoIpType::RoutingActivationRequest,
            0x0E80,
            SocketType::Stream,
            None,
            vec![0x22, 0xF1, 0x90],
            true,
        );
        assert_eq!(
            f.to_array(),
            vec![0x03, 0xFC, 0x00, 0x05, 0x00, 0x00, 0x00, 0x05, 0x0E, 0x80, 0x22, 0xF1, 0x90]
        );
    }

    #[test]
    fn is_error_semantics() {
        assert!(base_frame(DoIpType::Nack, &[]).is_error());
        assert!(base_frame(DoIpType::DiagnosticMessageNack, &[]).is_error());
        assert!(!base_frame(DoIpType::DiagnosticMessage, &[]).is_error());
        assert!(!base_frame(DoIpType::DiagnosticMessageAck, &[]).is_error());
    }

    #[test]
    fn decode_src_dst_roundtrip() {
        let f = SrcDstFrame::new(
            V2019,
            DoIpType::DiagnosticMessage,
            0x0E80,
            0x1000,
            SocketType::Stream,
            None,
            vec![0x22, 0xF1, 0x90],
            true,
        );
        let bytes = f.to_array();
        let (decoded, consumed) = Frame::decode(&bytes, SocketType::Stream, None)
            .unwrap()
            .expect("complete frame");
        assert_eq!(consumed, bytes.len());
        let Frame::SrcDst(d) = decoded else {
            panic!("expected SrcDst frame");
        };
        assert_eq!(d.src.src_adr, 0x0E80);
        assert_eq!(d.dst_adr, 0x1000);
        assert_eq!(d.src.base.msg_type(), Some(DoIpType::DiagnosticMessage));
        assert_eq!(d.src.base.data, vec![0x22, 0xF1, 0x90]);
        assert_eq!(d.src.base.version, V2019);
    }

    #[test]
    fn decode_src_frame() {
        // RoutingActivationRequest payload: src + activation type + 4
        // ISO-reserved bytes + 4 OEM-reserved bytes.
        let bytes = [
            0x02, 0xFD, 0x00, 0x05, 0x00, 0x00, 0x00, 0x0B, 0x0E, 0x80, 0x00, 0, 0, 0, 0, 0, 0, 0,
            0,
        ];
        let (decoded, consumed) = Frame::decode(&bytes, SocketType::Stream, None)
            .unwrap()
            .expect("complete frame");
        assert_eq!(consumed, bytes.len());
        let Frame::Src(s) = decoded else {
            panic!("expected Src frame");
        };
        assert_eq!(s.src_adr, 0x0E80);
        assert_eq!(s.base.version, ProtocolVersion::Iso13400_2012);
        assert_eq!(s.base.data.len(), 9);
    }

    #[test]
    fn decode_base_frame_with_trailing_bytes() {
        // VehicleIdResponse (Base) followed by half a frame: only the first
        // frame is consumed.
        let mut bytes = base_frame(DoIpType::VehicleIdResponse, b"WVWZZZ3CZWE123456").to_array();
        let first_len = bytes.len();
        bytes.extend_from_slice(&[0x03, 0xFC, 0x00]);
        let (decoded, consumed) = Frame::decode(&bytes, SocketType::Dgram, None)
            .unwrap()
            .expect("complete frame");
        assert_eq!(consumed, first_len);
        assert!(matches!(decoded, Frame::Base(_)));
    }

    #[test]
    fn decode_incomplete_returns_none() {
        assert!(Frame::decode(&[0x03, 0xFC, 0x80], SocketType::Stream, None)
            .unwrap()
            .is_none());
        // Complete header but incomplete payload.
        let bytes = [0x03, 0xFC, 0x80, 0x01, 0x00, 0x00, 0x00, 0x07, 0x0E, 0x80];
        assert!(Frame::decode(&bytes, SocketType::Stream, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn decode_invalid_pattern_errors() {
        // Inverse version mismatch.
        assert!(Frame::decode(
            &[0x03, 0x03, 0x80, 0x01, 0, 0, 0, 0],
            SocketType::Stream,
            None
        )
        .is_err());
        // Version outside 1..=3.
        assert!(Frame::decode(
            &[0x04, 0xFB, 0x80, 0x01, 0, 0, 0, 0],
            SocketType::Stream,
            None
        )
        .is_err());
    }

    #[test]
    fn decode_short_address_payload_errors() {
        // DiagnosticMessage payload of only 3 bytes (too short for src+dst).
        let bytes = [
            0x03, 0xFC, 0x80, 0x01, 0x00, 0x00, 0x00, 0x03, 0x0E, 0x80, 0x22,
        ];
        assert!(Frame::decode(&bytes, SocketType::Stream, None).is_err());
    }

    #[test]
    fn decode_unknown_type_becomes_base_frame() {
        // OEM extension payload types do not error (deliberately permissive
        // decoding).
        let bytes = [0x03, 0xFC, 0xE0, 0x01, 0x00, 0x00, 0x00, 0x01, 0xAA];
        let (frame, _) = Frame::decode(&bytes, SocketType::Stream, None)
            .unwrap()
            .expect("complete frame");
        let Frame::Base(b) = frame else {
            panic!("expected Base frame");
        };
        assert_eq!(b.msg_type, 0xE001);
        assert_eq!(b.msg_type(), None);
        assert_eq!(b.data, vec![0xAA]);
    }

    #[test]
    fn reply_swaps_addresses() {
        let req = SrcDstFrame::new(
            V2019,
            DoIpType::DiagnosticMessage,
            0x0E80,
            0x1000,
            SocketType::Stream,
            None,
            vec![0x22, 0xF1, 0x90],
            true,
        );
        let ack = SrcDstFrame::reply(
            &req,
            DoIpType::DiagnosticMessageAck,
            SocketType::Stream,
            None,
            vec![0x00],
        );
        assert_eq!(ack.src.src_adr, 0x1000);
        assert_eq!(ack.dst_adr, 0x0E80);
        assert!(ack.src.base.is_master_frame);
    }

    #[test]
    fn vehicle_id_response_parse() {
        let mut payload = b"WVWZZZ3CZWE123456".to_vec();
        payload.extend_from_slice(&[0x10, 0x01]); // logical addr (big-endian)
        payload.extend_from_slice(&[1, 2, 3, 4, 5, 6]); // EID
        payload.extend_from_slice(&[7, 8, 9, 10, 11, 12]); // GID
        payload.push(0x10); // further action
        payload.push(0x00); // synch status
        let frame = base_frame(DoIpType::VehicleIdResponse, &payload);
        let r = VehicleIdResponse::parse(&frame);
        assert_eq!(r.vin, "WVWZZZ3CZWE123456");
        assert_eq!(r.logical_adr, 0x1001);
        assert_eq!(r.eid, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(r.gid, vec![7, 8, 9, 10, 11, 12]);
        assert_eq!(r.further_action, ActionType::RoutingActivationRequired);
        assert_eq!(r.synch_status, SynchStatus::Synchronized);
        assert_eq!(r.key(), ""); // empty key when there is no sender
    }

    #[test]
    fn vehicle_id_response_short_data_fallbacks() {
        let r = VehicleIdResponse::parse(&base_frame(DoIpType::VehicleIdResponse, &[]));
        assert_eq!(r.vin, "");
        assert_eq!(r.logical_adr, 0);
        assert!(r.eid.is_empty());
        assert!(r.gid.is_empty());
        assert_eq!(r.further_action, ActionType::Unknown);
        assert_eq!(r.synch_status, SynchStatus::Unknown);
    }

    #[test]
    fn vehicle_id_response_key_with_sender() {
        let mut payload = b"WVWZZZ3CZWE123456".to_vec();
        payload.extend_from_slice(&[0x10, 0x01]);
        let mut frame = base_frame(DoIpType::VehicleIdResponse, &payload);
        frame.sender = Some("192.168.0.10:13400".parse().unwrap());
        let r = VehicleIdResponse::parse(&frame);
        // Sender endpoint string + LogicalAdr (decimal, directly
        // concatenated).
        assert_eq!(r.key(), "192.168.0.10:134004097");
    }

    #[test]
    fn entity_status_response_parse() {
        let frame = base_frame(
            DoIpType::EntityStatusResponse,
            &[0x00, 0x10, 0x02, 0, 0, 0x10, 0x00],
        );
        let r = EntityStatusResponse::parse(&frame).unwrap();
        assert_eq!(r.node_type, NodeType::Gateway);
        assert_eq!(r.max_sockets, 16);
        assert_eq!(r.open_sockets, 2);
        assert_eq!(r.max_data_size, 4096);
        // Too short.
        assert!(EntityStatusResponse::parse(&base_frame(
            DoIpType::EntityStatusResponse,
            &[0x00, 0x10]
        ))
        .is_err());
    }

    #[test]
    fn power_mode_response_parse() {
        let r = DiagnosticPowerModeResponse::parse(&base_frame(
            DoIpType::DiagnosticPowerModeResponse,
            &[0x01],
        ))
        .unwrap();
        assert_eq!(r.power_mode, PowerMode::Ready);
        assert!(DiagnosticPowerModeResponse::parse(&base_frame(
            DoIpType::DiagnosticPowerModeResponse,
            &[]
        ))
        .is_err());
        // Unknown values are rejected (deliberately strict).
        assert!(DiagnosticPowerModeResponse::parse(&base_frame(
            DoIpType::DiagnosticPowerModeResponse,
            &[0x42]
        ))
        .is_err());
    }

    #[test]
    fn routing_activation_response_parse() {
        let r = RoutingActivationResponse::parse(&base_frame(
            DoIpType::RoutingActivationResponse,
            &[0x10],
        ))
        .unwrap();
        assert_eq!(r.activation_result, ActivationCode::RoutingActivated);
        assert!(RoutingActivationResponse::parse(&base_frame(
            DoIpType::RoutingActivationResponse,
            &[]
        ))
        .is_err());
    }

    #[test]
    fn diagnostic_message_holds_payload() {
        let frame = base_frame(DoIpType::DiagnosticMessage, &[0x62, 0xF1, 0x90, 0xAA]);
        let m = DiagnosticMessage::parse(&frame);
        assert_eq!(m.data, vec![0x62, 0xF1, 0x90, 0xAA]);
    }

    #[test]
    fn enum_num_roundtrips() {
        assert_eq!(
            DoIpType::from_num(0x8001),
            Some(DoIpType::DiagnosticMessage)
        );
        assert_eq!(DoIpType::from_num(0x9999), None);
        assert_eq!(DoIpType::VehicleIdResponse.to_num(), 0x0004);
        assert_eq!(ProtocolVersion::from_num(3), Some(V2019));
        assert_eq!(NackCode::None.to_num(), 0xFF);
        assert_eq!(Activation::WwhObd.to_num(), 0x01);
        assert_eq!(Activation::CentralSecurity.to_num(), 0xE0);
    }
}
