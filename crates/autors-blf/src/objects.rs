//! BLF object model: object headers, CAN/CAN FD message objects, statistics
//! objects, containers, and the file header.
//! Binary layouts are serialized little-endian with naturally aligned fields,
//! matching a sequential C-style struct layout (default packing 8; `Header`
//! and `CanMessage2` use packing 1, but their fields are naturally aligned so
//! the resulting layouts are identical). Inheritance-style designs (base
//! object header -> extended header -> concrete object, raw struct -> wrapper
//! carrying the payload) are flattened into field composition: every object
//! embeds a `header: LObj`, and the "managed" variants fold the base fields
//! and the attached payload into a single struct.
//! Notable behaviors (details at each item):
//! - `CanFdManaged::fd_flags` keeps the raw on-file value. An alternative
//!   interpretation applies a `(ushort)changeEndianess(uint)` transform on
//!   read, which turns EDL=0x1000 into 0 for standard files (python-can reads
//!   the field as a plain little-endian u32). Use
//!   [`CanFdManaged::fd_flags_cs`] when that alternative semantics is needed.
//! - Registered objects without a dedicated Rust structure are retained as
//!   [`BlfObject::Raw`]; IDs added by future BLF versions use
//!   [`BlfObject::Unknown`]. Both representations are lossless.

use crate::error::{Error, Result};
use crate::ethernet::{
    EthernetFrame, EthernetFrameEx, EthernetRxError, EthernetStatistic, EthernetStatus,
};
use crate::general::{
    AppTrigger, AttributeEvent, DataLostBegin, DataLostEnd, DiagRequestInterpretation,
    DistributedObjectMember, DriverOverrun, EnvironmentVariable, EventComment, FunctionBus,
    GlobalMarker, GpsEvent, RealtimeClock, TriggerCondition, WaterMarkEvent,
};

pub const OBJ_SIGNATURE: [u8; 4] = *b"LOBJ";
pub const FILE_SIGNATURE: [u8; 4] = *b"LOGG";
pub const OBJ_BASE_SIZE: usize = 16;
pub const LOBJ_SIZE: usize = 32;
pub const LOBJ_V2_SIZE: usize = 40;
pub const LOG_CONTAINER_SIZE: usize = 32;
/// Size of the mandatory portion of a BLF file header.
pub const HEADER_CORE_SIZE: usize = 72;
pub const HEADER_SIZE: usize = 144;

pub(crate) fn parse_err<T>(offset: u64, message: impl Into<String>) -> Result<T> {
    Err(Error::Parse {
        offset,
        message: message.into(),
    })
}

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    base: u64,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8], base: u64) -> Self {
        Reader { buf, pos: 0, base }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.buf.len() - self.pos < n {
            return parse_err(
                self.base + self.pos as u64,
                format!(
                    "unexpected end of data: need {n} bytes, have {}",
                    self.buf.len() - self.pos
                ),
            );
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        let mut a = [0u8; 2];
        a.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(a))
    }

    pub(crate) fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        let mut a = [0u8; 4];
        a.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(a))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(a))
    }

    pub(crate) fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    pub(crate) fn sig(&mut self) -> Result<[u8; 4]> {
        let mut a = [0u8; 4];
        a.copy_from_slice(self.take(4)?);
        Ok(a)
    }
}

pub(crate) fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}
pub(crate) fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
pub(crate) fn put_i16(out: &mut Vec<u8>, v: i16) {
    put_u16(out, v as u16);
}
pub(crate) fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
pub(crate) fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
pub(crate) fn put_f64(out: &mut Vec<u8>, v: f64) {
    put_u64(out, v.to_bits());
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

macro_rules! flags_newtype {
    ($(#[$meta:meta])* $name:ident($ty:ty), $($(#[$cmeta:meta])* $cname:ident = $cval:expr),* $(,)?) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct $name(pub $ty);

        impl $name {
            $($(#[$cmeta])* pub const $cname: Self = Self($cval);)*

            pub fn bits(self) -> $ty {
                self.0
            }

            pub fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            pub fn from_bits(bits: $ty) -> Self {
                Self(bits)
            }
        }

        impl std::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }

        impl std::ops::BitAnd for $name {
            type Output = Self;
            fn bitand(self, rhs: Self) -> Self {
                Self(self.0 & rhs.0)
            }
        }
    };
}

flags_newtype! {
    AppId(u8),
    UNKNOWN = 0,
    CANALYZER = 1,
    CANOE = 2,
    CANSTRESS = 3,
    CANLOG = 4,
    CANAPE = 5,
    CANCASEXL_LOG = 6,
    VLCONFIG = 7,
    PORSCHELOGGER = 200,
    CAETECLOGGER = 201,
    VECTORNETWORKSIMULATOR = 202,
    IPETRONIKLOGGER = 203,
    RT_PK = 204,
    PIKETEC = 205,
    SPARKS = 206,
}

flags_newtype! {
    ObjectFlags(u32),
    TIME_TEN_MICS = 1,
    TIME_ONE_NANS = 2,
}

flags_newtype! {
    CanFlags(u8),
    RX = 0,
    TX = 1,
    NERR = 0x20,
    WU = 0x40,
    RTR = 0x80,
}

flags_newtype! {
    CanFdFlags(u8),
    EDL = 1,
    BRS = 2,
    ESI = 4,
}

flags_newtype! {
    CanFd64Flags(u32),
    R = 2,
    NERR = 4,
    HIGH_VOLTAGE_WAKEUP = 8,
    RR_FRAME = 0x10,
    TX_ACK = 0x40,
    TX_REQ = 0x80,
    SRR = 0x200,
    R0 = 0x400,
    R1 = 0x800,
    EDL = 0x1000,
    BRS = 0x2000,
    ESI = 0x4000,
}

flags_newtype! {
    EccFlags(u8),
    NONE = 0,
    BIT_ERROR = 1,
    FORM_ERROR = 2,
    STUFF_ERROR = 4,
    OTHER_ERROR = 8,
    CRC_ERROR = 0x10,
    ACK_DEL_ERROR = 0x20,
    TX_NAK_ERROR = 0x40,
    RX_ERROR = 0x80,
    TX_ERROR = 192,
}

flags_newtype! {
    CanErrorExtFlags(u16),
    NONE = 0,
    RX = 0x10,
    BIT_ERROR = 0x20,
    FORM_ERROR = 0x40,
    STUFF_ERROR = 0x80,
    OTHER_ERROR = 0x100,
    CRC_ERROR = 0x200,
    ACK_DEL_ERROR = 0x400,
    TX_NAK_ERROR = 0x800,
    RX_ERROR = 0x1000,
    TX_ERROR = 6144,
}

flags_newtype! {
    CanErrorExtValidFlags(u32),
    NOT_SET = 0,
    SJA1000_ECC = 1,
    ECC = 2,
    POSITION = 4,
    FRAME_LENGTH = 8,
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ObjectType {
    Unknown = 0,
    CanMessage = 1,
    CanError = 2,
    CanOverload = 3,
    CanStatistic = 4,
    AppTrigger = 5,
    EnvInteger = 6,
    EnvDouble = 7,
    EnvString = 8,
    EnvData = 9,
    LogContainer = 10,
    LinMessage = 11,
    LinCrcError = 12,
    LinDlcInfo = 13,
    LinRcvError = 14,
    LinSndError = 15,
    LinSlvTimeout = 16,
    LinSchedModch = 17,
    LinSynError = 18,
    LinBaudrate = 19,
    LinSleep = 20,
    LinWakeup = 21,
    MostSpy = 22,
    MostCtrl = 23,
    MostLightlock = 24,
    MostStatistic = 25,
    Reserved26 = 26,
    Reserved27 = 27,
    Reserved28 = 28,
    FlexrayData = 29,
    FlexraySync = 30,
    CanDriverError = 31,
    MostPkt = 32,
    MostPkt2 = 33,
    MostHwmode = 34,
    MostReg = 35,
    MostGenreg = 36,
    MostNetstate = 37,
    MostDatalost = 38,
    MostTrigger = 39,
    FlexrayCycle = 40,
    FlexrayMessage = 41,
    LinChecksumInfo = 42,
    LinSpikeEvent = 43,
    CanDriverSync = 44,
    FlexrayStatus = 45,
    GpsEvent = 46,
    FrError = 47,
    FrStatus = 48,
    FrStartcycle = 49,
    FrRcvmessage = 50,
    Realtimclock = 51,
    Reserved52 = 52,
    Reserved53 = 53,
    LinStatistic = 54,
    J1708Message = 55,
    J1708VirtualMsg = 56,
    LinMessage2 = 57,
    LinSndError2 = 58,
    LinSynError2 = 59,
    LinCrcError2 = 60,
    LinRcvError2 = 61,
    LinWakeup2 = 62,
    LinSpikeEvent2 = 63,
    LinLongDomSig = 64,
    AppText = 65,
    FrRcvmessageEx = 66,
    MostStatisticex = 67,
    MostTxlight = 68,
    MostAlloctab = 69,
    MostStress = 70,
    EthernetFrame = 71,
    SysVariable = 72,
    CanErrorExt = 73,
    CanDriverErrorExt = 74,
    LinLongDomSig2 = 75,
    Most150Message = 76,
    Most150Pkt = 77,
    MostEthernetPkt = 78,
    Most150MessageFragment = 79,
    Most150PktFragment = 80,
    MostEthernetPktFragment = 81,
    MostSystemEvent = 82,
    Most150Alloctab = 83,
    Most50Message = 84,
    Most50Pkt = 85,
    CanMessage2 = 86,
    LinUnexpectedWakeup = 87,
    LinShortOrSlowResponse = 88,
    LinDisturbanceEvent = 89,
    SerialEvent = 90,
    OverrunError = 91,
    EventComment = 92,
    WlanFrame = 93,
    WlanStatistic = 94,
    MostEcl = 95,
    GlobalMarker = 96,
    AfdxFrame = 97,
    AfdxStatistic = 98,
    KlineStatusevent = 99,
    CanFdMessage = 100,
    CanFdMessage64 = 101,
    EthernetRxError = 102,
    EthernetStatus = 103,
    CanFdError64 = 104,
    LinShortOrSlowResponse2 = 105,
    AfdxStatus = 106,
    AfdxBusStatistic = 107,
    Reserved108 = 108,
    AfdxErrorEvent = 109,
    A429Error = 110,
    A429Status = 111,
    A429BusStatistic = 112,
    A429Message = 113,
    EthernetStatistic = 114,
    RestorepointContainer = 115,
    Reserved116 = 116,
    Reserved117 = 117,
    TestStructure = 118,
    DiagRequestInterpretation = 119,
    EthernetFrameEx = 120,
    EthernetFrameForwarded = 121,
    EthernetErrorEx = 122,
    EthernetErrorForwarded = 123,
    FunctionBus = 124,
    DataLostBegin = 125,
    DataLostEnd = 126,
    WaterMarkEvent = 127,
    TriggerCondition = 128,
    CanSettingChanged = 129,
    DistributedObjectMember = 130,
    AttributeEvent = 131,
}

impl ObjectType {
    /// Iterates every object ID registered by the supported BLF API revision.
    pub fn registered() -> impl Iterator<Item = Self> {
        (0..=131).filter_map(Self::from_raw)
    }

    pub fn is_reserved(self) -> bool {
        matches!(
            self,
            Self::Reserved26
                | Self::Reserved27
                | Self::Reserved28
                | Self::Reserved52
                | Self::Reserved53
                | Self::Reserved108
                | Self::Reserved116
                | Self::Reserved117
        )
    }

    pub fn to_raw(self) -> u32 {
        self as u32
    }

    pub fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            0 => ObjectType::Unknown,
            1 => ObjectType::CanMessage,
            2 => ObjectType::CanError,
            3 => ObjectType::CanOverload,
            4 => ObjectType::CanStatistic,
            5 => ObjectType::AppTrigger,
            6 => ObjectType::EnvInteger,
            7 => ObjectType::EnvDouble,
            8 => ObjectType::EnvString,
            9 => ObjectType::EnvData,
            10 => ObjectType::LogContainer,
            11 => ObjectType::LinMessage,
            12 => ObjectType::LinCrcError,
            13 => ObjectType::LinDlcInfo,
            14 => ObjectType::LinRcvError,
            15 => ObjectType::LinSndError,
            16 => ObjectType::LinSlvTimeout,
            17 => ObjectType::LinSchedModch,
            18 => ObjectType::LinSynError,
            19 => ObjectType::LinBaudrate,
            20 => ObjectType::LinSleep,
            21 => ObjectType::LinWakeup,
            22 => ObjectType::MostSpy,
            23 => ObjectType::MostCtrl,
            24 => ObjectType::MostLightlock,
            25 => ObjectType::MostStatistic,
            26 => ObjectType::Reserved26,
            27 => ObjectType::Reserved27,
            28 => ObjectType::Reserved28,
            29 => ObjectType::FlexrayData,
            30 => ObjectType::FlexraySync,
            31 => ObjectType::CanDriverError,
            32 => ObjectType::MostPkt,
            33 => ObjectType::MostPkt2,
            34 => ObjectType::MostHwmode,
            35 => ObjectType::MostReg,
            36 => ObjectType::MostGenreg,
            37 => ObjectType::MostNetstate,
            38 => ObjectType::MostDatalost,
            39 => ObjectType::MostTrigger,
            40 => ObjectType::FlexrayCycle,
            41 => ObjectType::FlexrayMessage,
            42 => ObjectType::LinChecksumInfo,
            43 => ObjectType::LinSpikeEvent,
            44 => ObjectType::CanDriverSync,
            45 => ObjectType::FlexrayStatus,
            46 => ObjectType::GpsEvent,
            47 => ObjectType::FrError,
            48 => ObjectType::FrStatus,
            49 => ObjectType::FrStartcycle,
            50 => ObjectType::FrRcvmessage,
            51 => ObjectType::Realtimclock,
            52 => ObjectType::Reserved52,
            53 => ObjectType::Reserved53,
            54 => ObjectType::LinStatistic,
            55 => ObjectType::J1708Message,
            56 => ObjectType::J1708VirtualMsg,
            57 => ObjectType::LinMessage2,
            58 => ObjectType::LinSndError2,
            59 => ObjectType::LinSynError2,
            60 => ObjectType::LinCrcError2,
            61 => ObjectType::LinRcvError2,
            62 => ObjectType::LinWakeup2,
            63 => ObjectType::LinSpikeEvent2,
            64 => ObjectType::LinLongDomSig,
            65 => ObjectType::AppText,
            66 => ObjectType::FrRcvmessageEx,
            67 => ObjectType::MostStatisticex,
            68 => ObjectType::MostTxlight,
            69 => ObjectType::MostAlloctab,
            70 => ObjectType::MostStress,
            71 => ObjectType::EthernetFrame,
            72 => ObjectType::SysVariable,
            73 => ObjectType::CanErrorExt,
            74 => ObjectType::CanDriverErrorExt,
            75 => ObjectType::LinLongDomSig2,
            76 => ObjectType::Most150Message,
            77 => ObjectType::Most150Pkt,
            78 => ObjectType::MostEthernetPkt,
            79 => ObjectType::Most150MessageFragment,
            80 => ObjectType::Most150PktFragment,
            81 => ObjectType::MostEthernetPktFragment,
            82 => ObjectType::MostSystemEvent,
            83 => ObjectType::Most150Alloctab,
            84 => ObjectType::Most50Message,
            85 => ObjectType::Most50Pkt,
            86 => ObjectType::CanMessage2,
            87 => ObjectType::LinUnexpectedWakeup,
            88 => ObjectType::LinShortOrSlowResponse,
            89 => ObjectType::LinDisturbanceEvent,
            90 => ObjectType::SerialEvent,
            91 => ObjectType::OverrunError,
            92 => ObjectType::EventComment,
            93 => ObjectType::WlanFrame,
            94 => ObjectType::WlanStatistic,
            95 => ObjectType::MostEcl,
            96 => ObjectType::GlobalMarker,
            97 => ObjectType::AfdxFrame,
            98 => ObjectType::AfdxStatistic,
            99 => ObjectType::KlineStatusevent,
            100 => ObjectType::CanFdMessage,
            101 => ObjectType::CanFdMessage64,
            102 => ObjectType::EthernetRxError,
            103 => ObjectType::EthernetStatus,
            104 => ObjectType::CanFdError64,
            105 => ObjectType::LinShortOrSlowResponse2,
            106 => ObjectType::AfdxStatus,
            107 => ObjectType::AfdxBusStatistic,
            108 => ObjectType::Reserved108,
            109 => ObjectType::AfdxErrorEvent,
            110 => ObjectType::A429Error,
            111 => ObjectType::A429Status,
            112 => ObjectType::A429BusStatistic,
            113 => ObjectType::A429Message,
            114 => ObjectType::EthernetStatistic,
            115 => ObjectType::RestorepointContainer,
            116 => ObjectType::Reserved116,
            117 => ObjectType::Reserved117,
            118 => ObjectType::TestStructure,
            119 => ObjectType::DiagRequestInterpretation,
            120 => ObjectType::EthernetFrameEx,
            121 => ObjectType::EthernetFrameForwarded,
            122 => ObjectType::EthernetErrorEx,
            123 => ObjectType::EthernetErrorForwarded,
            124 => ObjectType::FunctionBus,
            125 => ObjectType::DataLostBegin,
            126 => ObjectType::DataLostEnd,
            127 => ObjectType::WaterMarkEvent,
            128 => ObjectType::TriggerCondition,
            129 => ObjectType::CanSettingChanged,
            130 => ObjectType::DistributedObjectMember,
            131 => ObjectType::AttributeEvent,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum SysVarDataType {
    Double = 1,
    Long = 2,
    String = 3,
    DoubleArray = 4,
    LongArray = 5,
    LongLong = 6,
    ByteArray = 7,
}

impl SysVarDataType {
    pub fn to_raw(self) -> u32 {
        self as u32
    }

    pub fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            1 => SysVarDataType::Double,
            2 => SysVarDataType::Long,
            3 => SysVarDataType::String,
            4 => SysVarDataType::DoubleArray,
            5 => SysVarDataType::LongArray,
            6 => SysVarDataType::LongLong,
            7 => SysVarDataType::ByteArray,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjBase {
    pub signature: [u8; 4],
    pub header_size: u16,
    pub header_version: u16,
    pub object_size: u32,
    pub object_type: u32,
}

impl ObjBase {
    pub fn new(object_type: u32, header_size: u16, object_size: u32) -> Self {
        ObjBase {
            signature: OBJ_SIGNATURE,
            header_size,
            header_version: 0,
            object_size,
            object_type,
        }
    }

    pub fn object_type(&self) -> Option<ObjectType> {
        ObjectType::from_raw(self.object_type)
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let mut r = Reader::new(bytes, base);
        let ob = ObjBase {
            signature: r.sig()?,
            header_size: r.u16()?,
            header_version: r.u16()?,
            object_size: r.u32()?,
            object_type: r.u32()?,
        };
        Ok(ob)
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.signature);
        put_u16(out, self.header_size);
        put_u16(out, self.header_version);
        put_u32(out, self.object_size);
        put_u32(out, self.object_type);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LObj {
    pub base: ObjBase,
    pub object_flags: ObjectFlags,
    pub client_index: u16,
    pub object_version: u16,
    pub timestamp: u64,
    /// Timestamp-status flags used by version-2 object headers.
    pub timestamp_status: Option<u8>,
    /// Reserved byte used by version-2 object headers.
    pub reserved_header: Option<u8>,
    /// Original timestamp carried by version-2 object headers.
    pub original_timestamp: Option<u64>,
    /// Unrecognized bytes between the known header fields and `header_size`.
    pub header_extension: Vec<u8>,
}

impl LObj {
    pub fn new(
        object_type: ObjectType,
        object_flags: ObjectFlags,
        timestamp: u64,
        object_size: u32,
    ) -> Self {
        Self::new_raw(object_type.to_raw(), object_flags, timestamp, object_size)
    }

    /// Creates a version-1 object header for any numeric object type.
    pub fn new_raw(
        object_type: u32,
        object_flags: ObjectFlags,
        timestamp: u64,
        object_size: u32,
    ) -> Self {
        let mut base = ObjBase::new(object_type, LOBJ_SIZE as u16, object_size);
        base.header_version = 1;
        LObj {
            base,
            object_flags,
            client_index: 0,
            object_version: 0,
            timestamp,
            timestamp_status: None,
            reserved_header: None,
            original_timestamp: None,
            header_extension: Vec::new(),
        }
    }

    pub fn padding(&self) -> u32 {
        const PADDED: [ObjectType; 26] = [
            ObjectType::EnvInteger,
            ObjectType::EnvDouble,
            ObjectType::EnvString,
            ObjectType::EnvData,
            ObjectType::LogContainer,
            ObjectType::MostPkt,
            ObjectType::MostPkt2,
            ObjectType::AppText,
            ObjectType::MostAlloctab,
            ObjectType::EthernetFrame,
            ObjectType::EthernetRxError,
            ObjectType::SysVariable,
            ObjectType::Most150Message,
            ObjectType::Most150Pkt,
            ObjectType::MostEthernetPkt,
            ObjectType::Most150MessageFragment,
            ObjectType::Most150PktFragment,
            ObjectType::MostEthernetPktFragment,
            ObjectType::Most150Alloctab,
            ObjectType::Most50Message,
            ObjectType::Most50Pkt,
            ObjectType::SerialEvent,
            ObjectType::EventComment,
            ObjectType::WlanFrame,
            ObjectType::GlobalMarker,
            ObjectType::AfdxFrame,
        ];
        match self.base.object_type() {
            Some(t) if PADDED.contains(&t) => self.base.object_size & 3,
            _ => 0,
        }
    }

    pub fn timestamp_seconds(&self) -> f64 {
        let divisor = if self.object_flags.contains(ObjectFlags::TIME_ONE_NANS) {
            1_000_000_000.0
        } else {
            100_000.0
        };
        self.timestamp as f64 / divisor
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let base_obj = ObjBase::parse(bytes, base)?;
        if base_obj.signature != OBJ_SIGNATURE {
            return parse_err(
                base,
                format!(
                    "bad object signature {:?}, expected {:?}",
                    String::from_utf8_lossy(&base_obj.signature),
                    String::from_utf8_lossy(&OBJ_SIGNATURE)
                ),
            );
        }
        let header_size = usize::from(base_obj.header_size);
        if header_size < OBJ_BASE_SIZE || header_size > bytes.len() {
            return parse_err(
                base + 4,
                format!(
                    "object header size {header_size} is outside available {} bytes",
                    bytes.len()
                ),
            );
        }
        let mut r = Reader::new(
            &bytes[OBJ_BASE_SIZE..header_size],
            base + OBJ_BASE_SIZE as u64,
        );
        let mut object_flags = ObjectFlags::default();
        let mut client_index = 0;
        let mut object_version = 0;
        let mut timestamp = 0;
        let mut timestamp_status = None;
        let mut reserved_header = None;
        let mut original_timestamp = None;
        let known_size = match base_obj.header_version {
            0 if header_size == OBJ_BASE_SIZE => OBJ_BASE_SIZE,
            1 if header_size >= LOBJ_SIZE => {
                object_flags = ObjectFlags(r.u32()?);
                client_index = r.u16()?;
                object_version = r.u16()?;
                timestamp = r.u64()?;
                LOBJ_SIZE
            }
            2 if header_size >= LOBJ_V2_SIZE => {
                object_flags = ObjectFlags(r.u32()?);
                timestamp_status = Some(r.u8()?);
                reserved_header = Some(r.u8()?);
                object_version = r.u16()?;
                timestamp = r.u64()?;
                original_timestamp = Some(r.u64()?);
                LOBJ_V2_SIZE
            }
            version => {
                return parse_err(
                    base + 6,
                    format!("unsupported object header version {version} with size {header_size}"),
                );
            }
        };
        Ok(LObj {
            base: base_obj,
            object_flags,
            client_index,
            object_version,
            timestamp,
            timestamp_status,
            reserved_header,
            original_timestamp,
            header_extension: bytes[known_size..header_size].to_vec(),
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.base.write_to(out);
        match self.base.header_version {
            0 => {}
            1 => {
                put_u32(out, self.object_flags.0);
                put_u16(out, self.client_index);
                put_u16(out, self.object_version);
                put_u64(out, self.timestamp);
            }
            2 => {
                put_u32(out, self.object_flags.0);
                put_u8(out, self.timestamp_status.unwrap_or(0));
                put_u8(out, self.reserved_header.unwrap_or(0));
                put_u16(out, self.object_version);
                put_u64(out, self.timestamp);
                put_u64(out, self.original_timestamp.unwrap_or(0));
            }
            _ => {}
        }
        out.extend_from_slice(&self.header_extension);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

pub const CAN_MESSAGE_SIZE: usize = 48;
pub const CAN_MESSAGE2_SIZE: usize = 56;
pub const CAN_ERROR_SIZE: usize = 40;
pub const CAN_ERROR_EXT_SIZE: usize = 56;
pub const CAN_OVERLOAD_SIZE: usize = 40;
pub const CAN_FD_MESSAGE_SIZE: usize = 120;
pub const CAN_FD_MESSAGE64_SIZE: usize = 72;
pub const CAN_FD_ERROR64_SIZE: usize = 76;
pub const CAN_STATISTIC_SIZE: usize = 64;
pub const CAN_DRIVER_ERROR_SIZE: usize = 40;
pub const CAN_DRIVER_SYNC_SIZE: usize = 40;
pub const CAN_DRIVER_ERROR_EXT_SIZE: usize = 64;
pub const CAN_SETTING_CHANGED_SIZE: usize = 43;
pub const APP_TEXT_SIZE: usize = 48;
pub const SYS_VARIABLE_SIZE: usize = 64;
pub const RESTORE_POINT_CONTAINER_SIZE: usize = 48;
pub const LIN_MESSAGE_SIZE: usize = 52;
pub const LIN_MESSAGE_EXTENDED_SIZE: usize = 56;
pub const LIN_MESSAGE2_V1_SIZE: usize = 164;
pub const LIN_MESSAGE2_V2_SIZE: usize = 168;
pub const LIN_MESSAGE2_V3_SIZE: usize = 184;

pub fn length_to_dlc(len: usize) -> u8 {
    match len {
        0..=8 => len as u8,
        9..=12 => 9,
        13..=16 => 10,
        17..=20 => 11,
        21..=24 => 12,
        25..=32 => 13,
        33..=48 => 14,
        _ => 15,
    }
}

pub fn dlc_to_length(dlc: u8) -> usize {
    match dlc {
        0..=8 => dlc as usize,
        9 => 12,
        10 => 16,
        11 => 20,
        12 => 24,
        13 => 32,
        14 => 48,
        _ => 64,
    }
}

/// Bit-preserving wrapper for an IEEE-754 double stored in a BLF object.
///
/// Keeping the raw representation makes object equality reflexive even for
/// NaN payloads while still exposing conversion helpers for normal use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Float64(u64);

impl Float64 {
    pub fn new(value: f64) -> Self {
        Self(value.to_bits())
    }

    pub fn value(self) -> f64 {
        f64::from_bits(self.0)
    }
}

/// Legacy BLF LIN frame object (`LIN_MESSAGE`, type 11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinMessage {
    pub header: LObj,
    pub channel: u16,
    pub id: u8,
    pub dlc: u8,
    pub data: [u8; 8],
    pub fsm_id: u8,
    pub fsm_state: u8,
    pub header_time: u8,
    pub full_time: u8,
    pub crc: u16,
    /// 0 = received, 1 = transmit receipt, 2 = transmit request.
    pub dir: u8,
    pub reserved1: u8,
    /// Present in the 56-byte form of the object.
    pub reserved2: Option<u32>,
    /// Bytes added by a future producer after the known layout.
    pub trailing: Vec<u8>,
}

impl LinMessage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        object_flags: ObjectFlags,
        timestamp: u64,
        channel: u16,
        id: u8,
        data: &[u8],
        checksum: u8,
        is_tx: bool,
    ) -> Self {
        let mut payload = [0u8; 8];
        let length = data.len().min(8);
        payload[..length].copy_from_slice(&data[..length]);
        Self {
            header: LObj::new(
                ObjectType::LinMessage,
                object_flags,
                timestamp,
                LIN_MESSAGE_EXTENDED_SIZE as u32,
            ),
            channel,
            id,
            dlc: length as u8,
            data: payload,
            fsm_id: 0,
            fsm_state: 0,
            header_time: 0,
            full_time: 0,
            crc: u16::from(checksum),
            dir: u8::from(is_tx),
            reserved1: 0,
            reserved2: Some(0),
            trailing: Vec::new(),
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let object_size = header.base.object_size as usize;
        if object_size < LIN_MESSAGE_SIZE {
            return parse_err(
                base,
                format!("LIN_MESSAGE size {object_size} is smaller than {LIN_MESSAGE_SIZE}"),
            );
        }
        let mut r = Reader::new(&bytes[LOBJ_SIZE..object_size], base + LOBJ_SIZE as u64);
        let channel = r.u16()?;
        let id = r.u8()?;
        let dlc = r.u8()?;
        let mut data = [0u8; 8];
        data.copy_from_slice(r.take(8)?);
        let fsm_id = r.u8()?;
        let fsm_state = r.u8()?;
        let header_time = r.u8()?;
        let full_time = r.u8()?;
        let crc = r.u16()?;
        let dir = r.u8()?;
        let reserved1 = r.u8()?;
        let remaining = object_size - LIN_MESSAGE_SIZE;
        let reserved2 = if remaining >= 4 { Some(r.u32()?) } else { None };
        let known_extension = usize::from(reserved2.is_some()) * 4;
        let trailing = r.take(remaining - known_extension)?.to_vec();
        Ok(Self {
            header,
            channel,
            id,
            dlc,
            data,
            fsm_id,
            fsm_state,
            header_time,
            full_time,
            crc,
            dir,
            reserved1,
            reserved2,
            trailing,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u8(out, self.id);
        put_u8(out, self.dlc);
        out.extend_from_slice(&self.data);
        put_u8(out, self.fsm_id);
        put_u8(out, self.fsm_state);
        put_u8(out, self.header_time);
        put_u8(out, self.full_time);
        put_u16(out, self.crc);
        put_u8(out, self.dir);
        put_u8(out, self.reserved1);
        if let Some(value) = self.reserved2 {
            put_u32(out, value);
        }
        out.extend_from_slice(&self.trailing);
    }
}

/// Current BLF LIN frame object (`LIN_MESSAGE2`, type 57).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinMessage2 {
    pub header: LObj,
    /// Start-of-frame time in nanoseconds.
    pub sof: u64,
    pub event_baudrate: u32,
    pub channel: u16,
    pub reserved_bus_event: u16,
    pub sync_break_length: u64,
    pub sync_delimiter_length: u64,
    pub supplier_id: u16,
    pub message_id: u16,
    pub nad: u8,
    pub id: u8,
    pub dlc: u8,
    /// 0 = classic checksum, 1 = enhanced checksum.
    pub checksum_model: u8,
    /// End-of-header followed by end-of-data-byte timestamps, in nanoseconds.
    pub databyte_timestamps: [u64; 9],
    pub data: [u8; 8],
    pub crc: u16,
    /// 0 = received, 1 = transmit receipt, 2 = transmit request.
    pub dir: u8,
    pub simulated: u8,
    pub is_event_triggered: u8,
    pub event_triggered_associated_index: u8,
    pub event_triggered_associated_id: u8,
    pub fsm_id: u8,
    pub fsm_state: u8,
    pub reserved1: u8,
    pub reserved2: u16,
    /// Layout generation: 1 (base), 2 (response baudrate), or 3 (exact timing).
    pub api_major: u8,
    pub response_baudrate: u32,
    pub exact_header_baudrate: Float64,
    pub early_stop_bit_offset: u32,
    pub early_stop_bit_offset_response: u32,
    /// Bytes added by a future producer after the known layout.
    pub trailing: Vec<u8>,
}

impl LinMessage2 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        object_flags: ObjectFlags,
        timestamp: u64,
        channel: u16,
        id: u8,
        data: &[u8],
        checksum: u8,
        checksum_model: u8,
        is_tx: bool,
    ) -> Self {
        let mut payload = [0u8; 8];
        let length = data.len().min(8);
        payload[..length].copy_from_slice(&data[..length]);
        let mut header = LObj::new(
            ObjectType::LinMessage2,
            object_flags,
            timestamp,
            LIN_MESSAGE2_V1_SIZE as u32,
        );
        header.object_version = 1;
        Self {
            header,
            sof: 0,
            event_baudrate: 0,
            channel,
            reserved_bus_event: 0,
            sync_break_length: 0,
            sync_delimiter_length: 0,
            supplier_id: 0,
            message_id: 0,
            nad: 0,
            id,
            dlc: length as u8,
            checksum_model,
            databyte_timestamps: [0; 9],
            data: payload,
            crc: u16::from(checksum),
            dir: u8::from(is_tx),
            simulated: 0,
            is_event_triggered: 0,
            event_triggered_associated_index: 0,
            event_triggered_associated_id: 0,
            fsm_id: 0,
            fsm_state: 0,
            reserved1: 0,
            reserved2: 0,
            api_major: 1,
            response_baudrate: 0,
            exact_header_baudrate: Float64::default(),
            early_stop_bit_offset: 0,
            early_stop_bit_offset_response: 0,
            trailing: Vec::new(),
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let object_size = header.base.object_size as usize;
        if object_size < LIN_MESSAGE2_V1_SIZE {
            return parse_err(
                base,
                format!("LIN_MESSAGE2 size {object_size} is smaller than {LIN_MESSAGE2_V1_SIZE}"),
            );
        }
        let mut r = Reader::new(&bytes[LOBJ_SIZE..object_size], base + LOBJ_SIZE as u64);
        let sof = r.u64()?;
        let event_baudrate = r.u32()?;
        let channel = r.u16()?;
        let reserved_bus_event = r.u16()?;
        let sync_break_length = r.u64()?;
        let sync_delimiter_length = r.u64()?;
        let supplier_id = r.u16()?;
        let message_id = r.u16()?;
        let nad = r.u8()?;
        let id = r.u8()?;
        let dlc = r.u8()?;
        let checksum_model = r.u8()?;
        let mut databyte_timestamps = [0u64; 9];
        for value in &mut databyte_timestamps {
            *value = r.u64()?;
        }
        let mut data = [0u8; 8];
        data.copy_from_slice(r.take(8)?);
        let crc = r.u16()?;
        let dir = r.u8()?;
        let simulated = r.u8()?;
        let is_event_triggered = r.u8()?;
        let event_triggered_associated_index = r.u8()?;
        let event_triggered_associated_id = r.u8()?;
        let fsm_id = r.u8()?;
        let fsm_state = r.u8()?;
        let reserved1 = r.u8()?;
        let reserved2 = r.u16()?;

        let mut consumed = LIN_MESSAGE2_V1_SIZE;
        let (api_major, response_baudrate) = if object_size - consumed >= 4 {
            consumed += 4;
            (2, r.u32()?)
        } else {
            (1, 0)
        };
        let (
            api_major,
            exact_header_baudrate,
            early_stop_bit_offset,
            early_stop_bit_offset_response,
        ) = if object_size - consumed >= 16 {
            consumed += 16;
            (3, Float64::new(r.f64()?), r.u32()?, r.u32()?)
        } else {
            (api_major, Float64::default(), 0, 0)
        };
        let trailing = r.take(object_size - consumed)?.to_vec();
        Ok(Self {
            header,
            sof,
            event_baudrate,
            channel,
            reserved_bus_event,
            sync_break_length,
            sync_delimiter_length,
            supplier_id,
            message_id,
            nad,
            id,
            dlc,
            checksum_model,
            databyte_timestamps,
            data,
            crc,
            dir,
            simulated,
            is_event_triggered,
            event_triggered_associated_index,
            event_triggered_associated_id,
            fsm_id,
            fsm_state,
            reserved1,
            reserved2,
            api_major,
            response_baudrate,
            exact_header_baudrate,
            early_stop_bit_offset,
            early_stop_bit_offset_response,
            trailing,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u64(out, self.sof);
        put_u32(out, self.event_baudrate);
        put_u16(out, self.channel);
        put_u16(out, self.reserved_bus_event);
        put_u64(out, self.sync_break_length);
        put_u64(out, self.sync_delimiter_length);
        put_u16(out, self.supplier_id);
        put_u16(out, self.message_id);
        put_u8(out, self.nad);
        put_u8(out, self.id);
        put_u8(out, self.dlc);
        put_u8(out, self.checksum_model);
        for timestamp in self.databyte_timestamps {
            put_u64(out, timestamp);
        }
        out.extend_from_slice(&self.data);
        put_u16(out, self.crc);
        put_u8(out, self.dir);
        put_u8(out, self.simulated);
        put_u8(out, self.is_event_triggered);
        put_u8(out, self.event_triggered_associated_index);
        put_u8(out, self.event_triggered_associated_id);
        put_u8(out, self.fsm_id);
        put_u8(out, self.fsm_state);
        put_u8(out, self.reserved1);
        put_u16(out, self.reserved2);
        if self.api_major >= 2 {
            put_u32(out, self.response_baudrate);
        }
        if self.api_major >= 3 {
            put_f64(out, self.exact_header_baudrate.value());
            put_u32(out, self.early_stop_bit_offset);
            put_u32(out, self.early_stop_bit_offset_response);
        }
        out.extend_from_slice(&self.trailing);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanMessage {
    pub header: LObj,
    pub channel: u16,
    pub flags: CanFlags,
    pub dlc: u8,
    pub id: u32,
    pub data: [u8; 8],
}

impl CanMessage {
    pub fn new(
        object_flags: ObjectFlags,
        timestamp: u64,
        channel: u16,
        id: u32,
        data: &[u8],
        is_tx: bool,
    ) -> Self {
        let mut buf = [0u8; 8];
        let n = data.len().min(8);
        buf[..n].copy_from_slice(&data[..n]);
        CanMessage {
            header: LObj::new(
                ObjectType::CanMessage,
                object_flags,
                timestamp,
                CAN_MESSAGE_SIZE as u32,
            ),
            channel,
            flags: if is_tx { CanFlags::TX } else { CanFlags::RX },
            dlc: n as u8,
            id,
            data: buf,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let msg = CanMessage {
            header,
            channel: r.u16()?,
            flags: CanFlags(r.u8()?),
            dlc: r.u8()?,
            id: r.u32()?,
            data: {
                let mut a = [0u8; 8];
                a.copy_from_slice(r.take(8)?);
                a
            },
        };
        Ok(msg)
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u8(out, self.flags.0);
        put_u8(out, self.dlc);
        put_u32(out, self.id);
        out.extend_from_slice(&self.data);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanMessage2 {
    pub header: LObj,
    pub channel: u16,
    pub flags: CanFlags,
    pub dlc: u8,
    pub id: u32,
    pub data: [u8; 8],
    pub frame_length: u32,
    pub bit_count: u8,
    pub reserved: u8,
    pub reserved2: u16,
}

impl CanMessage2 {
    pub fn new(
        object_flags: ObjectFlags,
        timestamp: u64,
        channel: u16,
        id: u32,
        data: &[u8],
        is_tx: bool,
    ) -> Self {
        let mut buf = [0u8; 8];
        let n = data.len().min(8);
        buf[..n].copy_from_slice(&data[..n]);
        CanMessage2 {
            header: LObj::new(
                ObjectType::CanMessage2,
                object_flags,
                timestamp,
                CAN_MESSAGE2_SIZE as u32,
            ),
            channel,
            flags: if is_tx { CanFlags::TX } else { CanFlags::RX },
            dlc: n as u8,
            id,
            data: buf,
            frame_length: n as u32,
            bit_count: (n * 8) as u8,
            reserved: 0,
            reserved2: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let msg = CanMessage2 {
            header,
            channel: r.u16()?,
            flags: CanFlags(r.u8()?),
            dlc: r.u8()?,
            id: r.u32()?,
            data: {
                let mut a = [0u8; 8];
                a.copy_from_slice(r.take(8)?);
                a
            },
            frame_length: r.u32()?,
            bit_count: r.u8()?,
            reserved: r.u8()?,
            reserved2: r.u16()?,
        };
        Ok(msg)
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u8(out, self.flags.0);
        put_u8(out, self.dlc);
        put_u32(out, self.id);
        out.extend_from_slice(&self.data);
        put_u32(out, self.frame_length);
        put_u8(out, self.bit_count);
        put_u8(out, self.reserved);
        put_u16(out, self.reserved2);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanError {
    pub header: LObj,
    pub channel: u16,
    pub length: u16,
    pub reserved: u32,
}

impl CanError {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        CanError {
            header: LObj::new(
                ObjectType::CanError,
                object_flags,
                timestamp,
                CAN_ERROR_SIZE as u32,
            ),
            channel: 0,
            length: 0,
            reserved: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(CanError {
            header,
            channel: r.u16()?,
            length: r.u16()?,
            reserved: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u16(out, self.length);
        put_u32(out, self.reserved);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanErrorExt {
    pub header: LObj,
    pub channel: u16,
    pub length: u16,
    pub valid_flags: CanErrorExtValidFlags,
    pub ecc: EccFlags,
    pub position: u8,
    pub dlc: u8,
    pub reserved: u8,
    pub frame_length_ns: u32,
    pub id: u32,
    pub flags: CanErrorExtFlags,
    pub reserved2: u16,
    /// Optional payload bytes reported by newer CAN cores.
    pub data: Vec<u8>,
}

impl CanErrorExt {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        CanErrorExt {
            header: LObj::new(
                ObjectType::CanErrorExt,
                object_flags,
                timestamp,
                CAN_ERROR_EXT_SIZE as u32,
            ),
            channel: 0,
            length: 0,
            valid_flags: CanErrorExtValidFlags::NOT_SET,
            ecc: EccFlags::NONE,
            position: 0,
            dlc: 0,
            reserved: 0,
            frame_length_ns: 0,
            id: 0,
            flags: CanErrorExtFlags::NONE,
            reserved2: 0,
            data: Vec::new(),
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let mut parsed = CanErrorExt {
            header,
            channel: r.u16()?,
            length: r.u16()?,
            valid_flags: CanErrorExtValidFlags(r.u32()?),
            ecc: EccFlags(r.u8()?),
            position: r.u8()?,
            dlc: r.u8()?,
            reserved: r.u8()?,
            frame_length_ns: r.u32()?,
            id: r.u32()?,
            flags: CanErrorExtFlags(r.u16()?),
            reserved2: r.u16()?,
            data: Vec::new(),
        };
        let fixed_size = CAN_ERROR_EXT_SIZE;
        let object_size = parsed.header.base.object_size as usize;
        if object_size < fixed_size || object_size > bytes.len() {
            return parse_err(
                base,
                format!("CAN_ERROR_EXT object size {object_size} is out of bounds"),
            );
        }
        parsed
            .data
            .extend_from_slice(&bytes[fixed_size..object_size]);
        Ok(parsed)
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut header = self.header.clone();
        header.base.object_size = (CAN_ERROR_EXT_SIZE + self.data.len()) as u32;
        header.write_to(out);
        put_u16(out, self.channel);
        put_u16(out, self.length);
        put_u32(out, self.valid_flags.0);
        put_u8(out, self.ecc.0);
        put_u8(out, self.position);
        put_u8(out, self.dlc);
        put_u8(out, self.reserved);
        put_u32(out, self.frame_length_ns);
        put_u32(out, self.id);
        put_u16(out, self.flags.0);
        put_u16(out, self.reserved2);
        out.extend_from_slice(&self.data);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanFdMessage {
    pub header: LObj,
    pub channel: u16,
    pub flags: CanFlags,
    pub dlc: u8,
    pub id: u32,
    pub frame_length: u32,
    pub arb_bit_count: u8,
    pub fd_flags: CanFdFlags,
    pub valid_data_bytes: u8,
    pub reserved: u8,
    pub reserved2: u32,
    pub data: [u8; 64],
    pub reserved3: u32,
}

impl CanFdMessage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        object_flags: ObjectFlags,
        timestamp: u64,
        channel: u16,
        id: u32,
        data: &[u8],
        is_tx: bool,
        is_fd: bool,
        brs: bool,
    ) -> Self {
        let mut buf = [0u8; 64];
        let n = data.len().min(64);
        buf[..n].copy_from_slice(&data[..n]);
        let mut fd_flags = CanFdFlags(0);
        if is_fd {
            fd_flags = fd_flags | CanFdFlags::EDL;
        }
        if brs {
            fd_flags = fd_flags | CanFdFlags::BRS;
        }
        CanFdMessage {
            header: LObj::new(
                ObjectType::CanFdMessage,
                object_flags,
                timestamp,
                CAN_FD_MESSAGE_SIZE as u32,
            ),
            channel,
            flags: if is_tx { CanFlags::TX } else { CanFlags::RX },
            dlc: length_to_dlc(n),
            id,
            frame_length: 0,
            arb_bit_count: 0,
            fd_flags,
            valid_data_bytes: n as u8,
            reserved: 0,
            reserved2: 0,
            data: buf,
            reserved3: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(CanFdMessage {
            header,
            channel: r.u16()?,
            flags: CanFlags(r.u8()?),
            dlc: r.u8()?,
            id: r.u32()?,
            frame_length: r.u32()?,
            arb_bit_count: r.u8()?,
            fd_flags: CanFdFlags(r.u8()?),
            valid_data_bytes: r.u8()?,
            reserved: r.u8()?,
            reserved2: r.u32()?,
            data: {
                let mut a = [0u8; 64];
                a.copy_from_slice(r.take(64)?);
                a
            },
            reserved3: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u8(out, self.flags.0);
        put_u8(out, self.dlc);
        put_u32(out, self.id);
        put_u32(out, self.frame_length);
        put_u8(out, self.arb_bit_count);
        put_u8(out, self.fd_flags.0);
        put_u8(out, self.valid_data_bytes);
        put_u8(out, self.reserved);
        put_u32(out, self.reserved2);
        out.extend_from_slice(&self.data);
        put_u32(out, self.reserved3);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanFdMessage64 {
    pub header: LObj,
    pub channel: u8,
    pub dlc: u8,
    pub valid_data_bytes: u8,
    pub tx_count: u8,
    pub id: u32,
    pub frame_length: u32,
    pub fd_flags: CanFd64Flags,
    pub btr_cfg_arb: u32,
    pub btr_cfg_data: u32,
    pub time_offset_brs_ns: u32,
    pub time_offset_crc_del_ns: u32,
    pub bit_count: u16,
    pub dir: u8,
    pub ext_data_offset: u8,
    pub crc: u32,
}

impl CanFdMessage64 {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        CanFdMessage64 {
            header: LObj::new(
                ObjectType::CanFdMessage64,
                object_flags,
                timestamp,
                CAN_FD_MESSAGE64_SIZE as u32,
            ),
            channel: 0,
            dlc: 0,
            valid_data_bytes: 0,
            tx_count: 0,
            id: 0,
            frame_length: 0,
            fd_flags: CanFd64Flags(0),
            btr_cfg_arb: 0,
            btr_cfg_data: 0,
            time_offset_brs_ns: 0,
            time_offset_crc_del_ns: 0,
            bit_count: 0,
            dir: 0,
            ext_data_offset: 0,
            crc: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(CanFdMessage64 {
            header,
            channel: r.u8()?,
            dlc: r.u8()?,
            valid_data_bytes: r.u8()?,
            tx_count: r.u8()?,
            id: r.u32()?,
            frame_length: r.u32()?,
            fd_flags: CanFd64Flags(r.u32()?),
            btr_cfg_arb: r.u32()?,
            btr_cfg_data: r.u32()?,
            time_offset_brs_ns: r.u32()?,
            time_offset_crc_del_ns: r.u32()?,
            bit_count: r.u16()?,
            dir: r.u8()?,
            ext_data_offset: r.u8()?,
            crc: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u8(out, self.channel);
        put_u8(out, self.dlc);
        put_u8(out, self.valid_data_bytes);
        put_u8(out, self.tx_count);
        put_u32(out, self.id);
        put_u32(out, self.frame_length);
        put_u32(out, self.fd_flags.0);
        put_u32(out, self.btr_cfg_arb);
        put_u32(out, self.btr_cfg_data);
        put_u32(out, self.time_offset_brs_ns);
        put_u32(out, self.time_offset_crc_del_ns);
        put_u16(out, self.bit_count);
        put_u8(out, self.dir);
        put_u8(out, self.ext_data_offset);
        put_u32(out, self.crc);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanFdManaged {
    pub header: LObj,
    pub channel: u8,
    pub dlc: u8,
    pub valid_data_bytes: u8,
    pub tx_count: u8,
    pub id: u32,
    pub frame_length: u32,
    pub fd_flags: CanFd64Flags,
    pub btr_cfg_arb: u32,
    pub btr_cfg_data: u32,
    pub time_offset_brs_ns: u32,
    pub time_offset_crc_del_ns: u32,
    pub bit_count: u16,
    pub dir: u8,
    pub ext_data_offset: u8,
    pub crc: u32,
    pub data: Vec<u8>,
    /// All bytes following the CAN FD payload, including reserved extensions.
    pub ext_data: Vec<u8>,
    /// Whether `ext_data` begins with extended arbitration/data bit timings.
    pub has_ext_data: bool,
    pub btr_ext_arb: u32,
    pub btr_ext_data: u32,
}

impl CanFdManaged {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        CanFdManaged {
            header: LObj::new(
                ObjectType::CanFdMessage64,
                object_flags,
                timestamp,
                CAN_FD_MESSAGE64_SIZE as u32,
            ),
            channel: 0,
            dlc: 0,
            valid_data_bytes: 0,
            tx_count: 0,
            id: 0,
            frame_length: 0,
            fd_flags: CanFd64Flags(0),
            btr_cfg_arb: 0,
            btr_cfg_data: 0,
            time_offset_brs_ns: 0,
            time_offset_crc_del_ns: 0,
            bit_count: 0,
            dir: 0,
            ext_data_offset: 0,
            crc: 0,
            data: Vec::new(),
            ext_data: Vec::new(),
            has_ext_data: false,
            btr_ext_arb: 0,
            btr_ext_data: 0,
        }
    }

    pub fn fd_flags_cs(&self) -> CanFd64Flags {
        CanFd64Flags((self.fd_flags.0.swap_bytes() & 0xFFFF) as u16 as u32)
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let fixed = CanFdMessage64::parse(bytes, base)?;
        let object_size = fixed.header.base.object_size as usize;
        let data_len = fixed.valid_data_bytes as usize;
        if CAN_FD_MESSAGE64_SIZE + data_len > object_size || object_size > bytes.len() {
            return parse_err(
                base,
                format!(
                    "CAN_FD_MESSAGE_64: data length {data_len} exceeds object size {object_size}"
                ),
            );
        }
        let mut data = vec![0u8; data_len];
        data.copy_from_slice(&bytes[CAN_FD_MESSAGE64_SIZE..CAN_FD_MESSAGE64_SIZE + data_len]);
        let ext_start = CAN_FD_MESSAGE64_SIZE + data_len;
        let has_ext_data =
            fixed.ext_data_offset != 0 && object_size >= usize::from(fixed.ext_data_offset) + 8;
        let ext_len = object_size.saturating_sub(ext_start);
        if has_ext_data && ext_len < 8 {
            return parse_err(
                base,
                "CAN_FD_MESSAGE_64: truncated extended bit-timing data",
            );
        }
        let ext_data = bytes[ext_start..object_size].to_vec();
        let mut btr_ext_arb = 0u32;
        let mut btr_ext_data = 0u32;
        if ext_data.len() >= 4 {
            let mut a = [0u8; 4];
            a.copy_from_slice(&ext_data[..4]);
            btr_ext_arb = u32::from_le_bytes(a);
        }
        if ext_data.len() >= 8 {
            let mut a = [0u8; 4];
            a.copy_from_slice(&ext_data[4..8]);
            btr_ext_data = u32::from_le_bytes(a);
        }
        Ok(CanFdManaged {
            header: fixed.header,
            channel: fixed.channel,
            dlc: fixed.dlc,
            valid_data_bytes: fixed.valid_data_bytes,
            tx_count: fixed.tx_count,
            id: fixed.id,
            frame_length: fixed.frame_length,
            fd_flags: fixed.fd_flags,
            btr_cfg_arb: fixed.btr_cfg_arb,
            btr_cfg_data: fixed.btr_cfg_data,
            time_offset_brs_ns: fixed.time_offset_brs_ns,
            time_offset_crc_del_ns: fixed.time_offset_crc_del_ns,
            bit_count: fixed.bit_count,
            dir: fixed.dir,
            ext_data_offset: fixed.ext_data_offset,
            crc: fixed.crc,
            data,
            ext_data,
            has_ext_data,
            btr_ext_arb,
            btr_ext_data,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let writes_bit_timing = self.has_ext_data
            || (self.ext_data_offset == 0 && (self.btr_ext_arb != 0 || self.btr_ext_data != 0));
        let mut fixed = CanFdMessage64 {
            header: self.header.clone(),
            channel: self.channel,
            dlc: self.dlc,
            valid_data_bytes: self.data.len() as u8,
            tx_count: self.tx_count,
            id: self.id,
            frame_length: self.frame_length,
            fd_flags: self.fd_flags,
            btr_cfg_arb: self.btr_cfg_arb,
            btr_cfg_data: self.btr_cfg_data,
            time_offset_brs_ns: self.time_offset_brs_ns,
            time_offset_crc_del_ns: self.time_offset_crc_del_ns,
            bit_count: self.bit_count,
            dir: self.dir,
            ext_data_offset: if writes_bit_timing && self.ext_data_offset == 0 {
                u8::try_from(CAN_FD_MESSAGE64_SIZE + self.data.len()).unwrap_or(u8::MAX)
            } else {
                self.ext_data_offset
            },
            crc: self.crc,
        };
        fixed.header.base.object_size =
            (CAN_FD_MESSAGE64_SIZE + self.data.len() + self.ext_data.len()) as u32;
        fixed.write_to(out);
        out.extend_from_slice(&self.data);
        out.extend_from_slice(&self.ext_data);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanStatistic {
    pub header: LObj,
    pub channel: u16,
    pub bus_load: u16,
    pub data_frames: u32,
    pub ex_data_frames: u32,
    pub remote_frames: u32,
    pub ex_remote_frames: u32,
    pub error_frames: u32,
    pub overload_frames: u32,
    pub reserved: u32,
}

impl CanStatistic {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        CanStatistic {
            header: LObj::new(
                ObjectType::CanStatistic,
                object_flags,
                timestamp,
                CAN_STATISTIC_SIZE as u32,
            ),
            channel: 0,
            bus_load: 0,
            data_frames: 0,
            ex_data_frames: 0,
            remote_frames: 0,
            ex_remote_frames: 0,
            error_frames: 0,
            overload_frames: 0,
            reserved: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(CanStatistic {
            header,
            channel: r.u16()?,
            bus_load: r.u16()?,
            data_frames: r.u32()?,
            ex_data_frames: r.u32()?,
            remote_frames: r.u32()?,
            ex_remote_frames: r.u32()?,
            error_frames: r.u32()?,
            overload_frames: r.u32()?,
            reserved: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u16(out, self.bus_load);
        put_u32(out, self.data_frames);
        put_u32(out, self.ex_data_frames);
        put_u32(out, self.remote_frames);
        put_u32(out, self.ex_remote_frames);
        put_u32(out, self.error_frames);
        put_u32(out, self.overload_frames);
        put_u32(out, self.reserved);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanDriverError {
    pub header: LObj,
    pub channel: u16,
    pub tx_errors: u8,
    pub rx_errors: u8,
    pub error_code: u32,
}

impl CanDriverError {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        CanDriverError {
            header: LObj::new(
                ObjectType::CanDriverError,
                object_flags,
                timestamp,
                CAN_DRIVER_ERROR_SIZE as u32,
            ),
            channel: 0,
            tx_errors: 0,
            rx_errors: 0,
            error_code: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(CanDriverError {
            header,
            channel: r.u16()?,
            tx_errors: r.u8()?,
            rx_errors: r.u8()?,
            error_code: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u8(out, self.tx_errors);
        put_u8(out, self.rx_errors);
        put_u32(out, self.error_code);
    }
}

/// A CAN overload frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanOverload {
    pub header: LObj,
    pub channel: u16,
    pub reserved1: u16,
    pub reserved2: u32,
}

impl CanOverload {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        Self {
            header: LObj::new(
                ObjectType::CanOverload,
                object_flags,
                timestamp,
                CAN_OVERLOAD_SIZE as u32,
            ),
            channel: 0,
            reserved1: 0,
            reserved2: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            channel: r.u16()?,
            reserved1: r.u16()?,
            reserved2: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u16(out, self.reserved1);
        put_u32(out, self.reserved2);
    }
}

/// A hardware synchronization event reported by a CAN driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanDriverSync {
    pub header: LObj,
    pub channel: u16,
    pub flags: u8,
    pub reserved1: u8,
    pub reserved2: u32,
}

impl CanDriverSync {
    pub const TX: u8 = 1;
    pub const RX: u8 = 2;
    pub const RX_THIS: u8 = 4;

    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        Self {
            header: LObj::new(
                ObjectType::CanDriverSync,
                object_flags,
                timestamp,
                CAN_DRIVER_SYNC_SIZE as u32,
            ),
            channel: 0,
            flags: 0,
            reserved1: 0,
            reserved2: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            channel: r.u16()?,
            flags: r.u8()?,
            reserved1: r.u8()?,
            reserved2: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u8(out, self.flags);
        put_u8(out, self.reserved1);
        put_u32(out, self.reserved2);
    }
}

/// Extended CAN driver error state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanDriverErrorExt {
    pub header: LObj,
    pub channel: u16,
    pub tx_errors: u8,
    pub rx_errors: u8,
    pub error_code: u32,
    pub flags: u32,
    pub state: u8,
    pub reserved1: u8,
    pub reserved2: u16,
    pub reserved3: [u32; 4],
}

impl CanDriverErrorExt {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        Self {
            header: LObj::new(
                ObjectType::CanDriverErrorExt,
                object_flags,
                timestamp,
                CAN_DRIVER_ERROR_EXT_SIZE as u32,
            ),
            channel: 0,
            tx_errors: 0,
            rx_errors: 0,
            error_code: 0,
            flags: 0,
            state: 0,
            reserved1: 0,
            reserved2: 0,
            reserved3: [0; 4],
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(Self {
            header,
            channel: r.u16()?,
            tx_errors: r.u8()?,
            rx_errors: r.u8()?,
            error_code: r.u32()?,
            flags: r.u32()?,
            state: r.u8()?,
            reserved1: r.u8()?,
            reserved2: r.u16()?,
            reserved3: [r.u32()?, r.u32()?, r.u32()?, r.u32()?],
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u16(out, self.channel);
        put_u8(out, self.tx_errors);
        put_u8(out, self.rx_errors);
        put_u32(out, self.error_code);
        put_u32(out, self.flags);
        put_u8(out, self.state);
        put_u8(out, self.reserved1);
        put_u16(out, self.reserved2);
        for value in self.reserved3 {
            put_u32(out, value);
        }
    }
}

/// Extended CAN FD bit-timing information.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CanFdExtFrameData {
    pub btr_ext_arb: u32,
    pub btr_ext_data: u32,
    pub reserved: Vec<u8>,
}

impl CanFdExtFrameData {
    fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let mut r = Reader::new(bytes, base);
        let btr_ext_arb = r.u32()?;
        let btr_ext_data = r.u32()?;
        Ok(Self {
            btr_ext_arb,
            btr_ext_data,
            reserved: r.take(bytes.len() - 8)?.to_vec(),
        })
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        put_u32(out, self.btr_ext_arb);
        put_u32(out, self.btr_ext_data);
        out.extend_from_slice(&self.reserved);
    }

    fn encoded_size(&self) -> usize {
        8 + self.reserved.len()
    }
}

/// A CAN FD error frame with an optional variable-length payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanFdError64 {
    pub header: LObj,
    pub channel: u8,
    pub dlc: u8,
    pub valid_data_bytes: u8,
    pub ecc: u8,
    pub flags: u16,
    pub error_code_ext: u16,
    pub ext_flags: u16,
    pub ext_data_offset: u8,
    pub reserved1: u8,
    pub id: u32,
    pub frame_length: u32,
    pub btr_cfg_arb: u32,
    pub btr_cfg_data: u32,
    pub time_offset_brs_ns: u32,
    pub time_offset_crc_del_ns: u32,
    pub crc: u32,
    pub error_position: u16,
    pub reserved2: u16,
    pub data: Vec<u8>,
    pub ext_data: Option<CanFdExtFrameData>,
    pub reserved: Vec<u8>,
}

impl CanFdError64 {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        Self {
            header: LObj::new(
                ObjectType::CanFdError64,
                object_flags,
                timestamp,
                CAN_FD_ERROR64_SIZE as u32,
            ),
            channel: 0,
            dlc: 0,
            valid_data_bytes: 0,
            ecc: 0,
            flags: 0,
            error_code_ext: 0,
            ext_flags: 0,
            ext_data_offset: 0,
            reserved1: 0,
            id: 0,
            frame_length: 0,
            btr_cfg_arb: 0,
            btr_cfg_data: 0,
            time_offset_brs_ns: 0,
            time_offset_crc_del_ns: 0,
            crc: 0,
            error_position: 0,
            reserved2: 0,
            data: Vec::new(),
            ext_data: None,
            reserved: Vec::new(),
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let object_size = header.base.object_size as usize;
        if !(CAN_FD_ERROR64_SIZE..=bytes.len()).contains(&object_size) {
            return parse_err(base, "CAN_FD_ERROR_64 object size is out of bounds");
        }
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let channel = r.u8()?;
        let dlc = r.u8()?;
        let valid_data_bytes = r.u8()?;
        let ecc = r.u8()?;
        let flags = r.u16()?;
        let error_code_ext = r.u16()?;
        let ext_flags = r.u16()?;
        let ext_data_offset = r.u8()?;
        let reserved1 = r.u8()?;
        let id = r.u32()?;
        let frame_length = r.u32()?;
        let btr_cfg_arb = r.u32()?;
        let btr_cfg_data = r.u32()?;
        let time_offset_brs_ns = r.u32()?;
        let time_offset_crc_del_ns = r.u32()?;
        let crc = r.u32()?;
        let error_position = r.u16()?;
        let reserved2 = r.u16()?;
        let data_end = CAN_FD_ERROR64_SIZE + usize::from(valid_data_bytes);
        if data_end > object_size {
            return parse_err(base, "CAN_FD_ERROR_64 payload exceeds its object size");
        }
        let data = bytes[CAN_FD_ERROR64_SIZE..data_end].to_vec();
        let tail = &bytes[data_end..object_size];
        let has_ext_data = ext_data_offset != 0 && object_size >= usize::from(ext_data_offset) + 8;
        let (ext_data, reserved) = if has_ext_data {
            if tail.len() < 8 {
                return parse_err(base, "CAN_FD_ERROR_64 extended bit timing is truncated");
            }
            (
                Some(CanFdExtFrameData::parse(tail, base + data_end as u64)?),
                Vec::new(),
            )
        } else {
            (None, tail.to_vec())
        };
        Ok(Self {
            header,
            channel,
            dlc,
            valid_data_bytes,
            ecc,
            flags,
            error_code_ext,
            ext_flags,
            ext_data_offset,
            reserved1,
            id,
            frame_length,
            btr_cfg_arb,
            btr_cfg_data,
            time_offset_brs_ns,
            time_offset_crc_del_ns,
            crc,
            error_position,
            reserved2,
            data,
            ext_data,
            reserved,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let tail_size = self
            .ext_data
            .as_ref()
            .map_or(self.reserved.len(), CanFdExtFrameData::encoded_size);
        let mut header = self.header.clone();
        header.base.object_size = (CAN_FD_ERROR64_SIZE + self.data.len() + tail_size) as u32;
        header.write_to(out);
        put_u8(out, self.channel);
        put_u8(out, self.dlc);
        put_u8(out, self.data.len() as u8);
        put_u8(out, self.ecc);
        put_u16(out, self.flags);
        put_u16(out, self.error_code_ext);
        put_u16(out, self.ext_flags);
        let ext_data_offset = if self.ext_data.is_some() && self.ext_data_offset == 0 {
            u8::try_from(CAN_FD_ERROR64_SIZE + self.data.len()).unwrap_or(u8::MAX)
        } else {
            self.ext_data_offset
        };
        put_u8(out, ext_data_offset);
        put_u8(out, self.reserved1);
        put_u32(out, self.id);
        put_u32(out, self.frame_length);
        put_u32(out, self.btr_cfg_arb);
        put_u32(out, self.btr_cfg_data);
        put_u32(out, self.time_offset_brs_ns);
        put_u32(out, self.time_offset_crc_del_ns);
        put_u32(out, self.crc);
        put_u16(out, self.error_position);
        put_u16(out, self.reserved2);
        out.extend_from_slice(&self.data);
        if let Some(ext_data) = &self.ext_data {
            ext_data.write_to(out);
        } else {
            out.extend_from_slice(&self.reserved);
        }
    }
}

/// A reset or bit-timing change on a CAN channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanSettingChanged {
    pub header: LObj,
    pub channel: u16,
    pub changed_type: u8,
    pub bit_timings: CanFdExtFrameData,
}

impl CanSettingChanged {
    pub fn new(object_flags: ObjectFlags, timestamp: u64) -> Self {
        Self {
            header: LObj::new(
                ObjectType::CanSettingChanged,
                object_flags,
                timestamp,
                CAN_SETTING_CHANGED_SIZE as u32,
            ),
            channel: 0,
            changed_type: u8::MAX,
            bit_timings: CanFdExtFrameData::default(),
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let object_size = header.base.object_size as usize;
        if !(CAN_SETTING_CHANGED_SIZE..=bytes.len()).contains(&object_size) {
            return parse_err(base, "CAN_SETTING_CHANGED object size is out of bounds");
        }
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let channel = r.u16()?;
        let changed_type = r.u8()?;
        let bit_timings = CanFdExtFrameData::parse(
            &bytes[LOBJ_SIZE + 3..object_size],
            base + (LOBJ_SIZE + 3) as u64,
        )?;
        Ok(Self {
            header,
            channel,
            changed_type,
            bit_timings,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut header = self.header.clone();
        header.base.object_size = (LOBJ_SIZE + 3 + self.bit_timings.encoded_size()) as u32;
        header.write_to(out);
        put_u16(out, self.channel);
        put_u8(out, self.changed_type);
        self.bit_timings.write_to(out);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppText {
    pub header: LObj,
    pub source: u32,
    pub reserved: u32,
    pub length: u32,
    pub reserved2: u32,
}

impl AppText {
    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        Ok(AppText {
            header,
            source: r.u32()?,
            reserved: r.u32()?,
            length: r.u32()?,
            reserved2: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u32(out, self.source);
        put_u32(out, self.reserved);
        put_u32(out, self.length);
        put_u32(out, self.reserved2);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppTextManaged {
    pub header: LObj,
    pub source: u32,
    pub reserved: u32,
    pub reserved2: u32,
    pub text: String,
}

impl AppTextManaged {
    pub fn new(object_flags: ObjectFlags, timestamp: u64, source: u32, text: &str) -> Self {
        AppTextManaged {
            header: LObj::new(
                ObjectType::AppText,
                object_flags,
                timestamp,
                (APP_TEXT_SIZE + text.len()) as u32,
            ),
            source,
            reserved: 0,
            reserved2: 0,
            text: text.to_string(),
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let fixed = AppText::parse(bytes, base)?;
        let object_size = fixed.header.base.object_size as usize;
        let len = fixed.length as usize;
        if APP_TEXT_SIZE + len > object_size || object_size > bytes.len() {
            return parse_err(
                base,
                format!("APP_TEXT: text length {len} exceeds object size {object_size}"),
            );
        }
        let text = String::from_utf8_lossy(&bytes[APP_TEXT_SIZE..APP_TEXT_SIZE + len])
            .trim_matches('\0')
            .to_string();
        Ok(AppTextManaged {
            header: fixed.header,
            source: fixed.source,
            reserved: fixed.reserved,
            reserved2: fixed.reserved2,
            text,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let bytes = self.text.as_bytes();
        let mut fixed = AppText {
            header: self.header.clone(),
            source: self.source,
            reserved: self.reserved,
            length: bytes.len() as u32,
            reserved2: self.reserved2,
        };
        fixed.header.base.object_size = (APP_TEXT_SIZE + bytes.len()) as u32;
        fixed.write_to(out);
        out.extend_from_slice(bytes);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysVariable {
    pub header: LObj,
    pub data_type: SysVarDataType,
    pub representation: u32,
    pub reserved: u64,
    pub name_length: u32,
    pub data_length: u32,
    pub reserved2: u64,
}

impl SysVariable {
    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let raw_type = r.u32()?;
        let data_type = match SysVarDataType::from_raw(raw_type) {
            Some(t) => t,
            None => {
                return parse_err(
                    base + LOBJ_SIZE as u64,
                    format!("unknown SysVarDataType {raw_type}"),
                )
            }
        };
        Ok(SysVariable {
            header,
            data_type,
            representation: r.u32()?,
            reserved: r.u64()?,
            name_length: r.u32()?,
            data_length: r.u32()?,
            reserved2: r.u64()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        put_u32(out, self.data_type.to_raw());
        put_u32(out, self.representation);
        put_u64(out, self.reserved);
        put_u32(out, self.name_length);
        put_u32(out, self.data_length);
        put_u64(out, self.reserved2);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysVariableManaged {
    pub header: LObj,
    pub data_type: SysVarDataType,
    pub representation: u32,
    pub reserved: u64,
    pub reserved2: u64,
    pub name: String,
    pub data: Vec<u8>,
}

impl SysVariableManaged {
    pub fn new(
        object_flags: ObjectFlags,
        timestamp: u64,
        data_type: SysVarDataType,
        representation: u32,
        name: &str,
        data: Vec<u8>,
    ) -> Self {
        SysVariableManaged {
            header: LObj::new(
                ObjectType::SysVariable,
                object_flags,
                timestamp,
                (SYS_VARIABLE_SIZE + name.len() + data.len()) as u32,
            ),
            data_type,
            representation,
            reserved: 0,
            reserved2: 0,
            name: name.to_string(),
            data,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let fixed = SysVariable::parse(bytes, base)?;
        let object_size = fixed.header.base.object_size as usize;
        let name_len = fixed.name_length as usize;
        let data_len = fixed.data_length as usize;
        if SYS_VARIABLE_SIZE + name_len + data_len > object_size || object_size > bytes.len() {
            return parse_err(
                base,
                format!("SYS_VARIABLE: name+data length exceeds object size {object_size}"),
            );
        }
        let name = String::from_utf8_lossy(&bytes[SYS_VARIABLE_SIZE..SYS_VARIABLE_SIZE + name_len])
            .trim_matches('\0')
            .to_string();
        let mut data = vec![0u8; data_len];
        data.copy_from_slice(
            &bytes[SYS_VARIABLE_SIZE + name_len..SYS_VARIABLE_SIZE + name_len + data_len],
        );
        Ok(SysVariableManaged {
            header: fixed.header,
            data_type: fixed.data_type,
            representation: fixed.representation,
            reserved: fixed.reserved,
            reserved2: fixed.reserved2,
            name,
            data,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let name_bytes = self.name.as_bytes();
        let mut fixed = SysVariable {
            header: self.header.clone(),
            data_type: self.data_type,
            representation: self.representation,
            reserved: self.reserved,
            name_length: name_bytes.len() as u32,
            data_length: self.data.len() as u32,
            reserved2: self.reserved2,
        };
        fixed.header.base.object_size =
            (SYS_VARIABLE_SIZE + name_bytes.len() + self.data.len()) as u32;
        fixed.write_to(out);
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&self.data);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePointContainer {
    pub header: LObj,
    pub reserved: [u8; 14],
    pub data_length: u16,
}

impl RestorePointContainer {
    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let mut r = Reader::new(&bytes[LOBJ_SIZE..], base + LOBJ_SIZE as u64);
        let mut reserved = [0u8; 14];
        reserved.copy_from_slice(r.take(14)?);
        Ok(RestorePointContainer {
            header,
            reserved,
            data_length: r.u16()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.header.write_to(out);
        out.extend_from_slice(&self.reserved);
        put_u16(out, self.data_length);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePointContainerManaged {
    pub header: LObj,
    pub reserved: [u8; 14],
    pub data: Vec<u8>,
}

impl RestorePointContainerManaged {
    pub fn new(object_flags: ObjectFlags, timestamp: u64, data: Vec<u8>) -> Self {
        RestorePointContainerManaged {
            header: LObj::new(
                ObjectType::RestorepointContainer,
                object_flags,
                timestamp,
                (RESTORE_POINT_CONTAINER_SIZE + data.len()) as u32,
            ),
            reserved: [0u8; 14],
            data,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let fixed = RestorePointContainer::parse(bytes, base)?;
        let object_size = fixed.header.base.object_size as usize;
        let len = fixed.data_length as usize;
        if RESTORE_POINT_CONTAINER_SIZE + len > object_size || object_size > bytes.len() {
            return parse_err(
                base,
                format!(
                    "RESTOREPOINT_CONTAINER: data length {len} exceeds object size {object_size}"
                ),
            );
        }
        let mut data = vec![0u8; len];
        data.copy_from_slice(
            &bytes[RESTORE_POINT_CONTAINER_SIZE..RESTORE_POINT_CONTAINER_SIZE + len],
        );
        Ok(RestorePointContainerManaged {
            header: fixed.header,
            reserved: fixed.reserved,
            data,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let mut fixed = RestorePointContainer {
            header: self.header.clone(),
            reserved: self.reserved,
            data_length: self.data.len() as u16,
        };
        fixed.header.base.object_size = (RESTORE_POINT_CONTAINER_SIZE + self.data.len()) as u32;
        fixed.write_to(out);
        out.extend_from_slice(&self.data);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogContainer {
    pub base: ObjBase,
    pub compression_method: u16,
    pub reserved: u16,
    pub reserved2: u32,
    pub uncompressed_size: u32,
    pub reserved3: u32,
}

impl LogContainer {
    pub fn new(data_len: usize, uncompressed_size: u32) -> Self {
        let compression_method = if uncompressed_size != 0 { 2 } else { 0 };
        Self::new_with_method(data_len, compression_method, uncompressed_size)
    }

    /// Creates a log container with an explicit BLF compression method.
    pub fn new_with_method(
        data_len: usize,
        compression_method: u16,
        uncompressed_size: u32,
    ) -> Self {
        LogContainer {
            base: ObjBase::new(
                ObjectType::LogContainer.to_raw(),
                OBJ_BASE_SIZE as u16,
                (LOG_CONTAINER_SIZE + data_len) as u32,
            ),
            compression_method,
            reserved: 0,
            reserved2: 0,
            uncompressed_size,
            reserved3: 0,
        }
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let mut r = Reader::new(bytes, base);
        let base_obj = ObjBase {
            signature: r.sig()?,
            header_size: r.u16()?,
            header_version: r.u16()?,
            object_size: r.u32()?,
            object_type: r.u32()?,
        };
        if base_obj.signature != OBJ_SIGNATURE {
            return parse_err(
                base,
                format!(
                    "bad object signature {:?}, expected {:?}",
                    String::from_utf8_lossy(&base_obj.signature),
                    String::from_utf8_lossy(&OBJ_SIGNATURE)
                ),
            );
        }
        Ok(LogContainer {
            base: base_obj,
            compression_method: r.u16()?,
            reserved: r.u16()?,
            reserved2: r.u32()?,
            uncompressed_size: r.u32()?,
            reserved3: r.u32()?,
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.base.write_to(out);
        put_u16(out, self.compression_method);
        put_u16(out, self.reserved);
        put_u32(out, self.reserved2);
        put_u32(out, self.uncompressed_size);
        put_u32(out, self.reserved3);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Time {
    pub year: u16,
    pub month: u16,
    pub day_of_week: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub milliseconds: u16,
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = ((m as i64) + 9) % 12; // mar=0 .. feb=11
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

fn is_leap_year(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(y) => 29,
        2 => 28,
        _ => 0,
    }
}

impl Time {
    pub fn from_system_time(st: std::time::SystemTime) -> Option<Time> {
        let dur = st.duration_since(std::time::UNIX_EPOCH).ok()?;
        let secs = dur.as_secs() as i64;
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        let day_of_week = (days + 4).rem_euclid(7) as u16;
        Some(Time {
            year: year as u16,
            month: month as u16,
            day_of_week,
            day: day as u16,
            hour: (rem / 3600) as u16,
            minute: ((rem % 3600) / 60) as u16,
            second: (rem % 60) as u16,
            milliseconds: dur.subsec_millis() as u16,
        })
    }

    pub fn to_system_time(&self) -> Option<std::time::SystemTime> {
        let year = i32::from(self.year);
        let month = u32::from(self.month);
        let day = u32::from(self.day);
        if !(1..=12).contains(&month)
            || day < 1
            || day > days_in_month(year, month)
            || self.hour >= 24
            || self.minute >= 60
            || self.second >= 60
            || self.milliseconds >= 1000
        {
            return None;
        }
        let days = days_from_civil(year, month, day);
        let secs = days * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second);
        if secs < 0 {
            return None;
        }
        Some(
            std::time::UNIX_EPOCH
                + std::time::Duration::new(secs as u64, u32::from(self.milliseconds) * 1_000_000),
        )
    }

    pub(crate) fn parse(r: &mut Reader) -> Result<Time> {
        Ok(Time {
            year: r.u16()?,
            month: r.u16()?,
            day_of_week: r.u16()?,
            day: r.u16()?,
            hour: r.u16()?,
            minute: r.u16()?,
            second: r.u16()?,
            milliseconds: r.u16()?,
        })
    }

    pub(crate) fn write_to(&self, out: &mut Vec<u8>) {
        put_u16(out, self.year);
        put_u16(out, self.month);
        put_u16(out, self.day_of_week);
        put_u16(out, self.day);
        put_u16(out, self.hour);
        put_u16(out, self.minute);
        put_u16(out, self.second);
        put_u16(out, self.milliseconds);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub signature: [u8; 4],
    pub header_size: u32,
    pub api_number: [u8; 4],
    pub app_id: AppId,
    pub compression: u8,
    pub app_major: u8,
    pub app_minor: u8,
    pub file_size: u64,
    pub uncompressed_file_size: u64,
    pub object_count: u32,
    pub app_build: u32,
    pub start: Time,
    pub end: Time,
    /// Offset of the first restore-point container when the extended header is present.
    pub restore_point_offset: Option<u64>,
    /// Reserved or future header bytes following `restore_point_offset`.
    pub reserved3: Vec<u8>,
}

impl Header {
    pub fn new(
        compressed: bool,
        app_id: AppId,
        app_major: u8,
        app_minor: u8,
        app_build: u32,
    ) -> Self {
        Header {
            signature: FILE_SIGNATURE,
            header_size: HEADER_SIZE as u32,
            api_number: [0; 4],
            app_id,
            compression: u8::from(compressed),
            app_major,
            app_minor,
            file_size: 0,
            uncompressed_file_size: 0,
            object_count: 0,
            app_build,
            start: Time::default(),
            end: Time::default(),
            restore_point_offset: Some(0),
            reserved3: vec![0; HEADER_SIZE - HEADER_CORE_SIZE - 8],
        }
    }

    pub fn started(&self) -> Option<std::time::SystemTime> {
        self.start.to_system_time()
    }

    pub fn ended(&self) -> Option<std::time::SystemTime> {
        self.end.to_system_time()
    }

    pub fn duration(&self) -> Option<std::time::Duration> {
        self.ended()?.duration_since(self.started()?).ok()
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        if bytes.len() < 8 {
            return parse_err(
                base,
                format!(
                    "file too small for BLF header prefix: {} bytes, need 8",
                    bytes.len()
                ),
            );
        }
        let mut prefix = Reader::new(bytes, base);
        let signature = prefix.sig()?;
        let header_size = prefix.u32()?;
        if signature != FILE_SIGNATURE {
            return parse_err(
                base,
                format!(
                    "bad file signature {:?}, expected {:?}",
                    String::from_utf8_lossy(&signature),
                    String::from_utf8_lossy(&FILE_SIGNATURE)
                ),
            );
        }
        let header_size_usize = header_size as usize;
        if header_size_usize < HEADER_CORE_SIZE {
            return parse_err(
                base + 4,
                format!(
                    "header size {header_size} too small, expected at least {HEADER_CORE_SIZE}"
                ),
            );
        }
        if header_size_usize > bytes.len() {
            return parse_err(
                base + 4,
                format!(
                    "header size {header_size} exceeds available {} bytes",
                    bytes.len()
                ),
            );
        }
        let mut r = Reader::new(&bytes[..header_size_usize], base);
        let signature = r.sig()?;
        let header_size = r.u32()?;
        let mut api_number = [0u8; 4];
        api_number.copy_from_slice(r.take(4)?);
        let header = Header {
            signature,
            header_size,
            api_number,
            app_id: AppId(r.u8()?),
            compression: r.u8()?,
            app_major: r.u8()?,
            app_minor: r.u8()?,
            file_size: r.u64()?,
            uncompressed_file_size: r.u64()?,
            object_count: r.u32()?,
            app_build: r.u32()?,
            start: Time::parse(&mut r)?,
            end: Time::parse(&mut r)?,
            restore_point_offset: if header_size_usize >= HEADER_CORE_SIZE + 8 {
                Some(r.u64()?)
            } else {
                None
            },
            reserved3: r.take(header_size_usize - r.pos)?.to_vec(),
        };
        Ok(header)
    }

    /// Returns the number of bytes produced by [`Header::write_to`].
    pub fn encoded_size(&self) -> usize {
        HEADER_CORE_SIZE
            + usize::from(self.restore_point_offset.is_some()) * 8
            + self.reserved3.len()
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.signature);
        put_u32(out, self.header_size);
        out.extend_from_slice(&self.api_number);
        put_u8(out, self.app_id.0);
        put_u8(out, self.compression);
        put_u8(out, self.app_major);
        put_u8(out, self.app_minor);
        put_u64(out, self.file_size);
        put_u64(out, self.uncompressed_file_size);
        put_u32(out, self.object_count);
        put_u32(out, self.app_build);
        self.start.write_to(out);
        self.end.write_to(out);
        if let Some(offset) = self.restore_point_offset {
            put_u64(out, offset);
        }
        out.extend_from_slice(&self.reserved3);
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlfObject {
    AppTrigger(AppTrigger),
    EnvironmentVariable(EnvironmentVariable),
    RealtimeClock(RealtimeClock),
    DriverOverrun(DriverOverrun),
    EventComment(EventComment),
    GlobalMarker(GlobalMarker),
    GpsEvent(GpsEvent),
    DataLostBegin(DataLostBegin),
    DataLostEnd(DataLostEnd),
    WaterMarkEvent(WaterMarkEvent),
    TriggerCondition(TriggerCondition),
    DistributedObjectMember(DistributedObjectMember),
    AttributeEvent(AttributeEvent),
    FunctionBus(FunctionBus),
    DiagRequestInterpretation(DiagRequestInterpretation),
    EthernetFrame(EthernetFrame),
    EthernetRxError(EthernetRxError),
    EthernetStatus(EthernetStatus),
    EthernetStatistic(EthernetStatistic),
    EthernetFrameEx(EthernetFrameEx),
    LinMessage(LinMessage),
    LinMessage2(LinMessage2),
    CanMessage(CanMessage),
    CanMessage2(CanMessage2),
    CanError(CanError),
    CanErrorExt(CanErrorExt),
    CanOverload(CanOverload),
    CanFdMessage(CanFdMessage),
    CanFdMessage64(CanFdManaged),
    CanFdError64(CanFdError64),
    CanStatistic(CanStatistic),
    CanDriverError(CanDriverError),
    CanDriverSync(CanDriverSync),
    CanDriverErrorExt(CanDriverErrorExt),
    CanSettingChanged(CanSettingChanged),
    AppText(AppTextManaged),
    SysVariable(SysVariableManaged),
    RestorePointContainer(RestorePointContainerManaged),
    /// A registered BLF object whose family-specific fields are not decoded.
    Raw {
        header: LObj,
        data: Vec<u8>,
    },
    /// An object type not present in this version's BLF object registry.
    Unknown {
        header: LObj,
        data: Vec<u8>,
    },
}

/// How deeply an object was decoded by this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectRepresentation {
    Typed,
    RegisteredRaw,
    Unknown,
}

macro_rules! impl_object_from {
    ($($type:ty => $variant:ident),+ $(,)?) => {
        $(impl From<$type> for BlfObject {
            fn from(value: $type) -> Self {
                Self::$variant(value)
            }
        })+
    };
}

impl_object_from! {
    AppTrigger => AppTrigger,
    EnvironmentVariable => EnvironmentVariable,
    RealtimeClock => RealtimeClock,
    DriverOverrun => DriverOverrun,
    EventComment => EventComment,
    GlobalMarker => GlobalMarker,
    GpsEvent => GpsEvent,
    DataLostBegin => DataLostBegin,
    DataLostEnd => DataLostEnd,
    WaterMarkEvent => WaterMarkEvent,
    TriggerCondition => TriggerCondition,
    DistributedObjectMember => DistributedObjectMember,
    AttributeEvent => AttributeEvent,
    FunctionBus => FunctionBus,
    DiagRequestInterpretation => DiagRequestInterpretation,
    EthernetFrame => EthernetFrame,
    EthernetRxError => EthernetRxError,
    EthernetStatus => EthernetStatus,
    EthernetStatistic => EthernetStatistic,
    EthernetFrameEx => EthernetFrameEx,
    LinMessage => LinMessage,
    LinMessage2 => LinMessage2,
    CanMessage => CanMessage,
    CanMessage2 => CanMessage2,
    CanError => CanError,
    CanErrorExt => CanErrorExt,
    CanOverload => CanOverload,
    CanFdMessage => CanFdMessage,
    CanFdManaged => CanFdMessage64,
    CanFdError64 => CanFdError64,
    CanStatistic => CanStatistic,
    CanDriverError => CanDriverError,
    CanDriverSync => CanDriverSync,
    CanDriverErrorExt => CanDriverErrorExt,
    CanSettingChanged => CanSettingChanged,
    AppTextManaged => AppText,
    SysVariableManaged => SysVariable,
    RestorePointContainerManaged => RestorePointContainer,
}

impl BlfObject {
    pub fn representation(&self) -> ObjectRepresentation {
        match self {
            Self::Raw { .. } => ObjectRepresentation::RegisteredRaw,
            Self::Unknown { .. } => ObjectRepresentation::Unknown,
            _ => ObjectRepresentation::Typed,
        }
    }

    /// Creates an opaque object for a known or future BLF object type.
    pub fn opaque(
        object_type: u32,
        object_flags: ObjectFlags,
        timestamp: u64,
        data: Vec<u8>,
    ) -> Result<Self> {
        let object_size = LOBJ_SIZE
            .checked_add(data.len())
            .and_then(|size| u32::try_from(size).ok())
            .ok_or_else(|| Error::Write("opaque BLF object exceeds the u32 size limit".into()))?;
        let header = LObj::new_raw(object_type, object_flags, timestamp, object_size);
        Ok(if ObjectType::from_raw(object_type).is_some() {
            Self::Raw { header, data }
        } else {
            Self::Unknown { header, data }
        })
    }

    /// Returns the uninterpreted payload when this is an opaque object.
    pub fn opaque_data(&self) -> Option<&[u8]> {
        match self {
            Self::Raw { data, .. } | Self::Unknown { data, .. } => Some(data),
            _ => None,
        }
    }

    /// Replaces the payload of an opaque object and updates its declared size.
    pub fn set_opaque_data(&mut self, data: Vec<u8>) -> Result<()> {
        let (header, current) = match self {
            Self::Raw { header, data } | Self::Unknown { header, data } => (header, data),
            _ => return Err(Error::Write("object has a typed payload".into())),
        };
        let object_size = usize::from(header.base.header_size)
            .checked_add(data.len())
            .and_then(|size| u32::try_from(size).ok())
            .ok_or_else(|| Error::Write("opaque BLF object exceeds the u32 size limit".into()))?;
        header.base.object_size = object_size;
        *current = data;
        Ok(())
    }

    pub fn object_type(&self) -> u32 {
        self.header().base.object_type
    }

    pub fn header(&self) -> &LObj {
        match self {
            BlfObject::AppTrigger(o) => &o.header,
            BlfObject::EnvironmentVariable(o) => &o.header,
            BlfObject::RealtimeClock(o) => &o.header,
            BlfObject::DriverOverrun(o) => &o.header,
            BlfObject::EventComment(o) => &o.header,
            BlfObject::GlobalMarker(o) => &o.header,
            BlfObject::GpsEvent(o) => &o.header,
            BlfObject::DataLostBegin(o) => &o.header,
            BlfObject::DataLostEnd(o) => &o.header,
            BlfObject::WaterMarkEvent(o) => &o.header,
            BlfObject::TriggerCondition(o) => &o.header,
            BlfObject::DistributedObjectMember(o) => &o.header,
            BlfObject::AttributeEvent(o) => &o.header,
            BlfObject::FunctionBus(o) => &o.header,
            BlfObject::DiagRequestInterpretation(o) => &o.header,
            BlfObject::EthernetFrame(o) => &o.header,
            BlfObject::EthernetRxError(o) => &o.header,
            BlfObject::EthernetStatus(o) => &o.header,
            BlfObject::EthernetStatistic(o) => &o.header,
            BlfObject::EthernetFrameEx(o) => &o.header,
            BlfObject::LinMessage(o) => &o.header,
            BlfObject::LinMessage2(o) => &o.header,
            BlfObject::CanMessage(o) => &o.header,
            BlfObject::CanMessage2(o) => &o.header,
            BlfObject::CanError(o) => &o.header,
            BlfObject::CanErrorExt(o) => &o.header,
            BlfObject::CanOverload(o) => &o.header,
            BlfObject::CanFdMessage(o) => &o.header,
            BlfObject::CanFdMessage64(o) => &o.header,
            BlfObject::CanFdError64(o) => &o.header,
            BlfObject::CanStatistic(o) => &o.header,
            BlfObject::CanDriverError(o) => &o.header,
            BlfObject::CanDriverSync(o) => &o.header,
            BlfObject::CanDriverErrorExt(o) => &o.header,
            BlfObject::CanSettingChanged(o) => &o.header,
            BlfObject::AppText(o) => &o.header,
            BlfObject::SysVariable(o) => &o.header,
            BlfObject::RestorePointContainer(o) => &o.header,
            BlfObject::Raw { header, .. } => header,
            BlfObject::Unknown { header, .. } => header,
        }
    }

    pub fn padding(&self) -> u32 {
        self.header().padding()
    }

    pub fn timestamp(&self) -> u64 {
        self.header().timestamp
    }

    pub fn parse(bytes: &[u8], base: u64) -> Result<Self> {
        let header = LObj::parse(bytes, base)?;
        let object_size = header.base.object_size as usize;
        if object_size < LOBJ_SIZE {
            return parse_err(
                base,
                format!("object size {object_size} smaller than LObj header"),
            );
        }
        if bytes.len() < object_size {
            return parse_err(
                base,
                format!(
                    "object size {object_size} exceeds available {} bytes",
                    bytes.len()
                ),
            );
        }
        let body = &bytes[..object_size];
        let header_size = usize::from(header.base.header_size);
        let typed_v1 =
            header.base.header_version == 1 && usize::from(header.base.header_size) == LOBJ_SIZE;
        Ok(match (typed_v1, header.base.object_type()) {
            (true, Some(ObjectType::AppTrigger)) => {
                BlfObject::AppTrigger(AppTrigger::parse(body, base)?)
            }
            (
                true,
                Some(
                    ObjectType::EnvInteger
                    | ObjectType::EnvDouble
                    | ObjectType::EnvString
                    | ObjectType::EnvData,
                ),
            ) => BlfObject::EnvironmentVariable(EnvironmentVariable::parse(body, base)?),
            (true, Some(ObjectType::Realtimclock)) => {
                BlfObject::RealtimeClock(RealtimeClock::parse(body, base)?)
            }
            (true, Some(ObjectType::OverrunError)) => {
                BlfObject::DriverOverrun(DriverOverrun::parse(body, base)?)
            }
            (true, Some(ObjectType::EventComment)) => {
                BlfObject::EventComment(EventComment::parse(body, base)?)
            }
            (true, Some(ObjectType::GlobalMarker)) => match GlobalMarker::parse(body, base) {
                Ok(marker) => BlfObject::GlobalMarker(marker),
                Err(_) => BlfObject::Raw {
                    header,
                    data: body[header_size..].to_vec(),
                },
            },
            (true, Some(ObjectType::GpsEvent)) => BlfObject::GpsEvent(GpsEvent::parse(body, base)?),
            (true, Some(ObjectType::DataLostBegin)) => {
                BlfObject::DataLostBegin(DataLostBegin::parse(body, base)?)
            }
            (true, Some(ObjectType::DataLostEnd)) => {
                BlfObject::DataLostEnd(DataLostEnd::parse(body, base)?)
            }
            (true, Some(ObjectType::WaterMarkEvent)) => {
                BlfObject::WaterMarkEvent(WaterMarkEvent::parse(body, base)?)
            }
            (true, Some(ObjectType::TriggerCondition)) => {
                BlfObject::TriggerCondition(TriggerCondition::parse(body, base)?)
            }
            (true, Some(ObjectType::DistributedObjectMember)) => {
                BlfObject::DistributedObjectMember(DistributedObjectMember::parse(body, base)?)
            }
            (true, Some(ObjectType::AttributeEvent)) => {
                BlfObject::AttributeEvent(AttributeEvent::parse(body, base)?)
            }
            (true, Some(ObjectType::FunctionBus)) => {
                BlfObject::FunctionBus(FunctionBus::parse(body, base)?)
            }
            (true, Some(ObjectType::DiagRequestInterpretation)) => {
                BlfObject::DiagRequestInterpretation(DiagRequestInterpretation::parse(body, base)?)
            }
            (true, Some(ObjectType::EthernetFrame)) => {
                BlfObject::EthernetFrame(EthernetFrame::parse(body, base)?)
            }
            (true, Some(ObjectType::EthernetRxError)) => {
                BlfObject::EthernetRxError(EthernetRxError::parse(body, base)?)
            }
            (true, Some(ObjectType::EthernetStatus)) => {
                BlfObject::EthernetStatus(EthernetStatus::parse(body, base)?)
            }
            (true, Some(ObjectType::EthernetStatistic)) => {
                BlfObject::EthernetStatistic(EthernetStatistic::parse(body, base)?)
            }
            (
                true,
                Some(
                    ObjectType::EthernetFrameEx
                    | ObjectType::EthernetFrameForwarded
                    | ObjectType::EthernetErrorEx
                    | ObjectType::EthernetErrorForwarded,
                ),
            ) => BlfObject::EthernetFrameEx(EthernetFrameEx::parse(body, base)?),
            (true, Some(ObjectType::LinMessage)) => {
                BlfObject::LinMessage(LinMessage::parse(body, base)?)
            }
            (true, Some(ObjectType::LinMessage2)) => {
                BlfObject::LinMessage2(LinMessage2::parse(body, base)?)
            }
            (true, Some(ObjectType::CanMessage)) => {
                BlfObject::CanMessage(CanMessage::parse(body, base)?)
            }
            (true, Some(ObjectType::CanMessage2)) => {
                BlfObject::CanMessage2(CanMessage2::parse(body, base)?)
            }
            (true, Some(ObjectType::CanError)) => BlfObject::CanError(CanError::parse(body, base)?),
            (true, Some(ObjectType::CanErrorExt)) => {
                BlfObject::CanErrorExt(CanErrorExt::parse(body, base)?)
            }
            (true, Some(ObjectType::CanOverload)) => {
                BlfObject::CanOverload(CanOverload::parse(body, base)?)
            }
            (true, Some(ObjectType::CanFdMessage)) => {
                BlfObject::CanFdMessage(CanFdMessage::parse(body, base)?)
            }
            (true, Some(ObjectType::CanFdMessage64)) => {
                BlfObject::CanFdMessage64(CanFdManaged::parse(body, base)?)
            }
            (true, Some(ObjectType::CanFdError64)) => {
                BlfObject::CanFdError64(CanFdError64::parse(body, base)?)
            }
            (true, Some(ObjectType::CanStatistic)) => {
                BlfObject::CanStatistic(CanStatistic::parse(body, base)?)
            }
            (true, Some(ObjectType::CanDriverError)) => {
                BlfObject::CanDriverError(CanDriverError::parse(body, base)?)
            }
            (true, Some(ObjectType::CanDriverSync)) => {
                BlfObject::CanDriverSync(CanDriverSync::parse(body, base)?)
            }
            (true, Some(ObjectType::CanDriverErrorExt)) => {
                BlfObject::CanDriverErrorExt(CanDriverErrorExt::parse(body, base)?)
            }
            (true, Some(ObjectType::CanSettingChanged)) => {
                BlfObject::CanSettingChanged(CanSettingChanged::parse(body, base)?)
            }
            (true, Some(ObjectType::AppText)) => {
                BlfObject::AppText(AppTextManaged::parse(body, base)?)
            }
            (true, Some(ObjectType::SysVariable)) => {
                BlfObject::SysVariable(SysVariableManaged::parse(body, base)?)
            }
            (true, Some(ObjectType::RestorepointContainer)) => {
                BlfObject::RestorePointContainer(RestorePointContainerManaged::parse(body, base)?)
            }
            _ => {
                let data = body[header_size..].to_vec();
                if header.base.object_type().is_some() {
                    BlfObject::Raw { header, data }
                } else {
                    BlfObject::Unknown { header, data }
                }
            }
        })
    }

    pub fn write_to(&self, out: &mut Vec<u8>) -> Result<()> {
        let start = out.len();
        match self {
            BlfObject::AppTrigger(o) => o.write_to(out),
            BlfObject::EnvironmentVariable(o) => o.write_to(out),
            BlfObject::RealtimeClock(o) => o.write_to(out),
            BlfObject::DriverOverrun(o) => o.write_to(out),
            BlfObject::EventComment(o) => o.write_to(out),
            BlfObject::GlobalMarker(o) => o.write_to(out),
            BlfObject::GpsEvent(o) => o.write_to(out),
            BlfObject::DataLostBegin(o) => o.write_to(out),
            BlfObject::DataLostEnd(o) => o.write_to(out),
            BlfObject::WaterMarkEvent(o) => o.write_to(out),
            BlfObject::TriggerCondition(o) => o.write_to(out),
            BlfObject::DistributedObjectMember(o) => o.write_to(out),
            BlfObject::AttributeEvent(o) => o.write_to(out),
            BlfObject::FunctionBus(o) => o.write_to(out),
            BlfObject::DiagRequestInterpretation(o) => o.write_to(out),
            BlfObject::EthernetFrame(o) => o.write_to(out),
            BlfObject::EthernetRxError(o) => o.write_to(out),
            BlfObject::EthernetStatus(o) => o.write_to(out),
            BlfObject::EthernetStatistic(o) => o.write_to(out),
            BlfObject::EthernetFrameEx(o) => o.write_to(out),
            BlfObject::LinMessage(o) => o.write_to(out),
            BlfObject::LinMessage2(o) => o.write_to(out),
            BlfObject::CanMessage(o) => o.write_to(out),
            BlfObject::CanMessage2(o) => o.write_to(out),
            BlfObject::CanError(o) => o.write_to(out),
            BlfObject::CanErrorExt(o) => o.write_to(out),
            BlfObject::CanOverload(o) => o.write_to(out),
            BlfObject::CanFdMessage(o) => o.write_to(out),
            BlfObject::CanFdMessage64(o) => o.write_to(out),
            BlfObject::CanFdError64(o) => o.write_to(out),
            BlfObject::CanStatistic(o) => o.write_to(out),
            BlfObject::CanDriverError(o) => o.write_to(out),
            BlfObject::CanDriverSync(o) => o.write_to(out),
            BlfObject::CanDriverErrorExt(o) => o.write_to(out),
            BlfObject::CanSettingChanged(o) => o.write_to(out),
            BlfObject::AppText(o) => o.write_to(out),
            BlfObject::SysVariable(o) => o.write_to(out),
            BlfObject::RestorePointContainer(o) => o.write_to(out),
            BlfObject::Raw { header, data } | BlfObject::Unknown { header, data } => {
                if header.base.object_size as usize
                    != usize::from(header.base.header_size) + data.len()
                {
                    return Err(Error::Write(format!(
                        "unknown object: object_size {} != header {} + payload {}",
                        header.base.object_size,
                        header.base.header_size,
                        data.len()
                    )));
                }
                header.write_to(out);
                out.extend_from_slice(data);
            }
        }
        let written = out.len() - start;
        if written < 12 {
            out.truncate(start);
            return Err(Error::Write(
                "object writer produced a truncated header".into(),
            ));
        }
        let declared = u32::from_le_bytes(
            out[start + 8..start + 12]
                .try_into()
                .map_err(|_| Error::Write("invalid object-size field".into()))?,
        ) as usize;
        if written != declared {
            out.truncate(start);
            return Err(Error::Write(format!(
                "object writer produced {written} bytes but declared {declared}"
            )));
        }
        Ok(())
    }

    pub fn matches_can_filter(&self, channel: i32, id: u32) -> bool {
        match self {
            BlfObject::CanMessage(m) => {
                m.dlc > 0
                    && (m.flags == CanFlags::RX || m.flags == CanFlags::TX)
                    && (id == u32::MAX || id == m.id)
                    && (channel < 0 || m.channel == channel as u16)
            }
            BlfObject::CanMessage2(m) => {
                m.dlc > 0
                    && (m.flags == CanFlags::RX || m.flags == CanFlags::TX)
                    && (id == u32::MAX || id == m.id)
                    && (channel < 0 || m.channel == channel as u16)
            }
            BlfObject::CanFdMessage(m) => {
                m.dlc > 0
                    && (m.flags == CanFlags::RX || m.flags == CanFlags::TX)
                    && (id == u32::MAX || id == m.id)
                    && (channel < 0 || m.channel == channel as u16)
            }
            BlfObject::CanFdMessage64(m) => {
                m.valid_data_bytes > 0
                    && (id == u32::MAX || id == m.id)
                    && (channel < 0 || i32::from(m.channel) == channel)
            }
            _ => false,
        }
    }

    pub fn matches_lin_filter(&self, channel: i32, id: u8) -> bool {
        let matches = |message_channel: u16, message_id: u8, dlc: u8, dir: u8| {
            message_id <= 0x3f
                && dlc <= 8
                && dir <= 2
                && (id == u8::MAX || id == message_id)
                && (channel < 0 || message_channel == channel as u16)
        };
        match self {
            BlfObject::LinMessage(message) => {
                matches(message.channel, message.id, message.dlc, message.dir)
            }
            BlfObject::LinMessage2(message) => {
                matches(message.channel, message.id, message.dlc, message.dir)
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: ObjectFlags = ObjectFlags::TIME_ONE_NANS;

    fn roundtrip<T, W, P>(obj: &T, write: W, parse: P, size: usize) -> T
    where
        W: Fn(&T, &mut Vec<u8>),
        P: Fn(&[u8], u64) -> Result<T>,
    {
        let mut buf = Vec::new();
        write(obj, &mut buf);
        assert_eq!(buf.len(), size, "serialized size");
        parse(&buf, 0).expect("re-parse")
    }

    #[test]
    fn obj_base_layout() {
        let ob = ObjBase::new(10, 16, 132);
        let mut buf = Vec::new();
        ob.write_to(&mut buf);
        assert_eq!(buf.len(), OBJ_BASE_SIZE);
        assert_eq!(&buf[0..4], b"LOBJ");
        assert_eq!(u16::from_le_bytes([buf[4], buf[5]]), 16);
        assert_eq!(u16::from_le_bytes([buf[6], buf[7]]), 0);
        assert_eq!(u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]), 132);
        assert_eq!(u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]), 10);
        let back = ObjBase::parse(&buf, 0).unwrap();
        assert_eq!(back, ob);
        assert_eq!(back.object_type(), Some(ObjectType::LogContainer));
    }

    #[test]
    fn lobj_signature_checked() {
        let mut buf = vec![0u8; LOBJ_SIZE];
        buf[0..4].copy_from_slice(b"XXXX");
        assert!(LObj::parse(&buf, 0).is_err());
    }

    #[test]
    fn lobj_padding_rules() {
        let h = LObj::new(ObjectType::LogContainer, NS, 0, 33);
        assert_eq!(h.padding(), 1);
        let h = LObj::new(ObjectType::AppText, NS, 0, 53);
        assert_eq!(h.padding(), 1);
        let h = LObj::new(ObjectType::SysVariable, NS, 0, 66);
        assert_eq!(h.padding(), 2);
        let h = LObj::new(ObjectType::CanMessage, NS, 0, 49);
        assert_eq!(h.padding(), 0);
        let mut h = LObj::new(ObjectType::CanMessage, NS, 0, 48);
        h.base.object_type = 117;
        assert_eq!(h.padding(), 0);
    }

    #[test]
    fn lobj_timestamp_seconds() {
        let h = LObj::new(ObjectType::CanMessage, NS, 1_500_000_000, 48);
        assert!((h.timestamp_seconds() - 1.5).abs() < 1e-12);
        let h = LObj::new(
            ObjectType::CanMessage,
            ObjectFlags::TIME_TEN_MICS,
            150_000,
            48,
        );
        assert!((h.timestamp_seconds() - 1.5).abs() < 1e-12);
    }

    #[test]
    fn can_message_layout_and_roundtrip() {
        let msg = CanMessage::new(NS, 123_456, 2, 0x1AB, &[1, 2, 3, 4, 5], true);
        assert_eq!(msg.dlc, 5);
        assert_eq!(msg.flags, CanFlags::TX);
        let mut buf = Vec::new();
        msg.write_to(&mut buf);
        assert_eq!(buf.len(), CAN_MESSAGE_SIZE);
        assert_eq!(&buf[0..4], b"LOBJ");
        assert_eq!(u16::from_le_bytes([buf[4], buf[5]]), 32, "header_size");
        assert_eq!(
            u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            48,
            "object_size"
        );
        assert_eq!(
            u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            1,
            "type"
        );
        assert_eq!(
            u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
            2,
            "object_flags"
        );
        assert_eq!(u64::from_le_bytes(buf[24..32].try_into().unwrap()), 123_456);
        assert_eq!(u16::from_le_bytes([buf[32], buf[33]]), 2, "channel");
        assert_eq!(buf[34], 1, "flags TX");
        assert_eq!(buf[35], 5, "dlc");
        assert_eq!(u32::from_le_bytes(buf[36..40].try_into().unwrap()), 0x1AB);
        assert_eq!(&buf[40..45], &[1, 2, 3, 4, 5]);
        assert_eq!(&buf[45..48], &[0, 0, 0], "data padded to 8");
        let back = CanMessage::parse(&buf, 0).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn can_message2_layout_and_roundtrip() {
        let msg = CanMessage2::new(NS, 7, 1, 0x55, &[0xAA; 8], false);
        assert_eq!(msg.flags, CanFlags::RX);
        assert_eq!(msg.frame_length, 8);
        assert_eq!(msg.bit_count, 64);
        let back = roundtrip(
            &msg,
            CanMessage2::write_to,
            CanMessage2::parse,
            CAN_MESSAGE2_SIZE,
        );
        assert_eq!(back, msg);
        let mut buf = Vec::new();
        msg.write_to(&mut buf);
        assert_eq!(u32::from_le_bytes(buf[48..52].try_into().unwrap()), 8);
        assert_eq!(buf[52], 64);
        assert_eq!(buf[53], 0);
        assert_eq!(u16::from_le_bytes([buf[54], buf[55]]), 0);
    }

    #[test]
    fn can_error_roundtrip() {
        let mut e = CanError::new(NS, 42);
        e.channel = 3;
        e.length = 6;
        let back = roundtrip(&e, CanError::write_to, CanError::parse, CAN_ERROR_SIZE);
        assert_eq!(back, e);
        assert_eq!(back.header.base.object_type(), Some(ObjectType::CanError));
    }

    #[test]
    fn can_error_ext_roundtrip() {
        let mut e = CanErrorExt::new(NS, 99);
        e.channel = 1;
        e.length = 8;
        e.valid_flags = CanErrorExtValidFlags::ECC | CanErrorExtValidFlags::POSITION;
        e.ecc = EccFlags::BIT_ERROR | EccFlags::RX_ERROR;
        e.position = 17;
        e.dlc = 8;
        e.frame_length_ns = 123_000;
        e.id = 0x7FF;
        e.flags = CanErrorExtFlags::RX;
        let back = roundtrip(
            &e,
            CanErrorExt::write_to,
            CanErrorExt::parse,
            CAN_ERROR_EXT_SIZE,
        );
        assert_eq!(back, e);
        assert!(back.valid_flags.contains(CanErrorExtValidFlags::ECC));
        assert!(back.ecc.contains(EccFlags::RX_ERROR));
    }

    #[test]
    fn can_fd_message_layout_and_roundtrip() {
        let data = [0x11u8; 12];
        let msg = CanFdMessage::new(NS, 1_000, 4, 0x12345678, &data, true, true, true);
        assert_eq!(msg.dlc, 9, "12 bytes -> DLC 9");
        assert_eq!(msg.valid_data_bytes, 12);
        assert!(msg.fd_flags.contains(CanFdFlags::EDL));
        assert!(msg.fd_flags.contains(CanFdFlags::BRS));
        let back = roundtrip(
            &msg,
            CanFdMessage::write_to,
            CanFdMessage::parse,
            CAN_FD_MESSAGE_SIZE,
        );
        assert_eq!(back, msg);
        let mut buf = Vec::new();
        msg.write_to(&mut buf);
        assert_eq!(&buf[52..64], &[0x11u8; 12]);
        assert_eq!(&buf[116..120], &[0, 0, 0, 0]);
    }

    #[test]
    fn can_fd_managed_roundtrip_with_ext_data() {
        let mut m = CanFdManaged::new(NS, 2_000);
        m.channel = 1;
        m.dlc = 9;
        m.tx_count = 2;
        m.id = 0x321;
        m.frame_length = 55_000;
        m.fd_flags = CanFd64Flags::EDL | CanFd64Flags::BRS;
        m.btr_cfg_arb = 500_000;
        m.btr_cfg_data = 2_000_000;
        m.bit_count = 100;
        m.dir = 1;
        m.crc = 0xBEEF;
        m.data = vec![9u8; 12];
        m.ext_data = vec![1, 0, 0, 0, 2, 0, 0, 0];
        m.btr_ext_arb = 1;
        m.btr_ext_data = 2;
        let mut buf = Vec::new();
        m.write_to(&mut buf);
        assert_eq!(buf.len(), CAN_FD_MESSAGE64_SIZE + 12 + 8);
        assert_eq!(buf[67], 84);
        let back = CanFdManaged::parse(&buf, 0).unwrap();
        assert_eq!(back.data, vec![9u8; 12]);
        assert_eq!(back.ext_data, vec![1, 0, 0, 0, 2, 0, 0, 0]);
        assert!(back.has_ext_data);
        assert_eq!(back.btr_ext_arb, 1);
        assert_eq!(back.btr_ext_data, 2);
        assert_eq!(back.fd_flags, CanFd64Flags::EDL | CanFd64Flags::BRS);
        assert_eq!(back.id, 0x321);
        assert!(back.header.base.object_size == 92);
    }

    #[test]
    fn can_fd_managed_preserves_reserved_tail_without_ext_offset() {
        let mut message = CanFdManaged::new(NS, 3_000);
        message.data = vec![1, 2];
        message.ext_data = vec![0; 8];
        message.header.base.object_size = (CAN_FD_MESSAGE64_SIZE + 10) as u32;
        let mut bytes = Vec::new();
        message.write_to(&mut bytes);
        assert_eq!(bytes[67], 0);
        let parsed = CanFdManaged::parse(&bytes, 0).unwrap();
        assert!(!parsed.has_ext_data);
        assert_eq!(parsed.ext_data, vec![0; 8]);
    }

    #[test]
    fn can_fd_managed_fd_flags_cs_quirk() {
        let mut m = CanFdManaged::new(NS, 0);
        m.fd_flags = CanFd64Flags::EDL;
        assert_eq!(m.fd_flags_cs(), CanFd64Flags(0));
        m.fd_flags = CanFd64Flags(0x0010_0000);
        assert_eq!(m.fd_flags_cs(), CanFd64Flags::EDL);
    }

    #[test]
    fn can_statistic_roundtrip() {
        let mut s = CanStatistic::new(NS, 5);
        s.channel = 1;
        s.bus_load = 37;
        s.data_frames = 100;
        s.ex_data_frames = 200;
        s.remote_frames = 3;
        s.ex_remote_frames = 4;
        s.error_frames = 5;
        s.overload_frames = 6;
        let back = roundtrip(
            &s,
            CanStatistic::write_to,
            CanStatistic::parse,
            CAN_STATISTIC_SIZE,
        );
        assert_eq!(back, s);
    }

    #[test]
    fn can_driver_error_roundtrip() {
        let mut d = CanDriverError::new(NS, 8);
        d.channel = 2;
        d.tx_errors = 10;
        d.rx_errors = 20;
        d.error_code = 0xDEAD;
        let back = roundtrip(
            &d,
            CanDriverError::write_to,
            CanDriverError::parse,
            CAN_DRIVER_ERROR_SIZE,
        );
        assert_eq!(back, d);
    }

    #[test]
    fn app_text_roundtrip_and_trim() {
        let t = AppTextManaged::new(NS, 11, 7, "hello BLF");
        let mut buf = Vec::new();
        t.write_to(&mut buf);
        assert_eq!(buf.len(), APP_TEXT_SIZE + 9);
        assert_eq!(u32::from_le_bytes(buf[40..44].try_into().unwrap()), 9);
        let back = AppTextManaged::parse(&buf, 0).unwrap();
        assert_eq!(back, t);

        let mut t2 = AppTextManaged::new(NS, 11, 0, "abc\0\0");
        t2.header.base.object_size = (APP_TEXT_SIZE + 5) as u32;
        let mut buf2 = Vec::new();
        let fixed = AppText {
            header: t2.header.clone(),
            source: 0,
            reserved: 0,
            length: 5,
            reserved2: 0,
        };
        fixed.write_to(&mut buf2);
        buf2.extend_from_slice(b"abc\0\0");
        let back2 = AppTextManaged::parse(&buf2, 0).unwrap();
        assert_eq!(back2.text, "abc");
    }

    #[test]
    fn sys_variable_roundtrip() {
        let v = SysVariableManaged::new(NS, 3, SysVarDataType::Double, 1, "var1", vec![0xAB; 8]);
        let mut buf = Vec::new();
        v.write_to(&mut buf);
        assert_eq!(buf.len(), SYS_VARIABLE_SIZE + 4 + 8);
        // data_type @32,representation @36,name_length @48,data_length @52
        assert_eq!(u32::from_le_bytes(buf[32..36].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(buf[48..52].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(buf[52..56].try_into().unwrap()), 8);
        let back = SysVariableManaged::parse(&buf, 0).unwrap();
        assert_eq!(back, v);
        assert_eq!(
            back.header.base.object_type(),
            Some(ObjectType::SysVariable)
        );
    }

    #[test]
    fn sys_variable_unknown_data_type_rejected() {
        let mut buf = Vec::new();
        let v = SysVariableManaged::new(NS, 3, SysVarDataType::Double, 1, "v", vec![]);
        v.write_to(&mut buf);
        buf[32] = 99;
        assert!(SysVariableManaged::parse(&buf, 0).is_err());
    }

    #[test]
    fn restore_point_container_roundtrip() {
        let r = RestorePointContainerManaged::new(NS, 1, vec![7u8; 6]);
        let back = roundtrip(
            &r,
            RestorePointContainerManaged::write_to,
            RestorePointContainerManaged::parse,
            RESTORE_POINT_CONTAINER_SIZE + 6,
        );
        assert_eq!(back, r);
    }

    #[test]
    fn log_container_layout() {
        let lc = LogContainer::new(100, 200);
        assert_eq!(lc.compression_method, 2);
        assert_eq!(lc.uncompressed_size, 200);
        assert_eq!(lc.base.header_size, 16);
        assert_eq!(lc.base.object_size, 132);
        let back = roundtrip(
            &lc,
            LogContainer::write_to,
            LogContainer::parse,
            LOG_CONTAINER_SIZE,
        );
        assert_eq!(back, lc);
        let lc2 = LogContainer::new(50, 0);
        assert_eq!(lc2.compression_method, 0);
    }

    #[test]
    fn header_layout_and_roundtrip() {
        let mut h = Header::new(true, AppId::CANOE, 8, 5, 3);
        h.file_size = 1234;
        h.uncompressed_file_size = 5678;
        h.object_count = 42;
        h.start = Time {
            year: 2024,
            month: 3,
            day_of_week: 2,
            day: 5,
            hour: 12,
            minute: 30,
            second: 45,
            milliseconds: 123,
        };
        h.end = Time {
            second: 46,
            ..h.start
        };
        let mut buf = Vec::new();
        h.write_to(&mut buf);
        assert_eq!(buf.len(), HEADER_SIZE);
        assert_eq!(&buf[0..4], b"LOGG");
        assert_eq!(u32::from_le_bytes(buf[4..8].try_into().unwrap()), 144);
        assert_eq!(buf[12], 2, "app_id CANoe");
        assert_eq!(buf[13], 1, "compression");
        assert_eq!(buf[14], 8);
        assert_eq!(buf[15], 5);
        assert_eq!(u64::from_le_bytes(buf[16..24].try_into().unwrap()), 1234);
        assert_eq!(u64::from_le_bytes(buf[24..32].try_into().unwrap()), 5678);
        assert_eq!(u32::from_le_bytes(buf[32..36].try_into().unwrap()), 42);
        assert_eq!(buf[36], 3, "app_build");
        assert_eq!(u16::from_le_bytes([buf[40], buf[41]]), 2024, "start.year");
        assert_eq!(u16::from_le_bytes([buf[58], buf[59]]), 3, "end.month");
        let back = Header::parse(&buf, 0).unwrap();
        assert_eq!(back, h);
        // duration = 1s
        assert_eq!(h.duration(), Some(std::time::Duration::new(1, 0)));
    }

    #[test]
    fn header_rejects_bad_signature_and_size() {
        let mut buf = vec![0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(b"XXXX");
        buf[4..8].copy_from_slice(&144u32.to_le_bytes());
        assert!(Header::parse(&buf, 0).is_err());
        buf[0..4].copy_from_slice(b"LOGG");
        buf[4..8].copy_from_slice(&(HEADER_CORE_SIZE as u32 - 1).to_le_bytes());
        assert!(Header::parse(&buf, 0).is_err());
        buf[4..8].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        assert!(Header::parse(&buf[..100], 0).is_err());
    }

    #[test]
    fn header_variable_size_roundtrip() {
        let mut original = Header::new(false, AppId::CANOE, 1, 2, 0x1234_5678);
        original.header_size = 100;
        original.reserved3.truncate(20);
        let mut bytes = Vec::new();
        original.write_to(&mut bytes);
        assert_eq!(bytes.len(), 100);
        assert_eq!(Header::parse(&bytes, 0).unwrap(), original);

        original.header_size = HEADER_CORE_SIZE as u32;
        original.restore_point_offset = None;
        original.reserved3.clear();
        bytes.clear();
        original.write_to(&mut bytes);
        assert_eq!(bytes.len(), HEADER_CORE_SIZE);
        assert_eq!(Header::parse(&bytes, 0).unwrap(), original);
    }

    #[test]
    fn time_system_time_conversion() {
        let t0 = Time::from_system_time(std::time::UNIX_EPOCH).unwrap();
        assert_eq!(
            t0,
            Time {
                year: 1970,
                month: 1,
                day_of_week: 4,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
                milliseconds: 0
            }
        );
        let t = Time {
            year: 2024,
            month: 3,
            day_of_week: 2,
            day: 5,
            hour: 12,
            minute: 30,
            second: 45,
            milliseconds: 123,
        };
        let st = t.to_system_time().unwrap();
        let back = Time::from_system_time(st).unwrap();
        assert_eq!(back, t);
        assert!(Time { month: 13, ..t }.to_system_time().is_none());
        assert!(Time {
            month: 2,
            day: 30,
            ..t
        }
        .to_system_time()
        .is_none());
    }

    #[test]
    fn enum_raw_conversions() {
        assert_eq!(ObjectType::registered().count(), 132);
        assert!(ObjectType::Reserved117.is_reserved());
        assert!(!ObjectType::CanMessage.is_reserved());
        assert_eq!(ObjectType::from_raw(1), Some(ObjectType::CanMessage));
        assert_eq!(ObjectType::from_raw(101), Some(ObjectType::CanFdMessage64));
        assert_eq!(
            ObjectType::from_raw(115),
            Some(ObjectType::RestorepointContainer)
        );
        assert_eq!(ObjectType::from_raw(131), Some(ObjectType::AttributeEvent));
        assert_eq!(ObjectType::from_raw(0), Some(ObjectType::Unknown));
        assert_eq!(ObjectType::from_raw(26), Some(ObjectType::Reserved26));
        assert_eq!(ObjectType::from_raw(108), Some(ObjectType::Reserved108));
        assert_eq!(ObjectType::from_raw(117), Some(ObjectType::Reserved117));
        assert_eq!(ObjectType::from_raw(200), None);
        assert_eq!(ObjectType::CanFdMessage.to_raw(), 100);
        assert_eq!(SysVarDataType::from_raw(7), Some(SysVarDataType::ByteArray));
        assert_eq!(SysVarDataType::from_raw(0), None);
        assert_eq!(CanFd64Flags::EDL.bits(), 0x1000);
        assert_eq!(EccFlags::TX_ERROR.bits(), 192);
        assert_eq!(CanErrorExtFlags::TX_ERROR.bits(), 6144);
        assert!(ObjectFlags::TIME_ONE_NANS.contains(ObjectFlags::TIME_ONE_NANS));
        assert_eq!(AppId::PIKETEC, AppId(205));
    }

    #[test]
    fn dlc_tables() {
        assert_eq!(length_to_dlc(0), 0);
        assert_eq!(length_to_dlc(8), 8);
        assert_eq!(length_to_dlc(12), 9);
        assert_eq!(length_to_dlc(16), 10);
        assert_eq!(length_to_dlc(20), 11);
        assert_eq!(length_to_dlc(24), 12);
        assert_eq!(length_to_dlc(32), 13);
        assert_eq!(length_to_dlc(48), 14);
        assert_eq!(length_to_dlc(64), 15);
        assert_eq!(dlc_to_length(8), 8);
        assert_eq!(dlc_to_length(9), 12);
        assert_eq!(dlc_to_length(15), 64);
    }

    #[test]
    fn lin_message_layout_and_roundtrip() {
        let mut message = LinMessage::new(NS, 123, 2, 0x22, &[1, 2, 3], 0xF9, true);
        message.fsm_id = 4;
        message.header_time = 5;
        let back = roundtrip(
            &message,
            LinMessage::write_to,
            LinMessage::parse,
            LIN_MESSAGE_EXTENDED_SIZE,
        );
        assert_eq!(back, message);
        assert_eq!(back.header.base.object_type(), Some(ObjectType::LinMessage));
    }

    #[test]
    fn lin_message2_layout_versions_and_roundtrip() {
        let mut message = LinMessage2::new(NS, 456, 3, 0x2A, &[0x11; 8], 0x77, 1, false);
        message.sof = 100;
        message.event_baudrate = 19_200;
        message.databyte_timestamps = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let back = roundtrip(
            &message,
            LinMessage2::write_to,
            LinMessage2::parse,
            LIN_MESSAGE2_V1_SIZE,
        );
        assert_eq!(back, message);

        message.api_major = 2;
        message.response_baudrate = 19_150;
        message.header.base.object_size = LIN_MESSAGE2_V2_SIZE as u32;
        let back = roundtrip(
            &message,
            LinMessage2::write_to,
            LinMessage2::parse,
            LIN_MESSAGE2_V2_SIZE,
        );
        assert_eq!(back, message);

        message.api_major = 3;
        message.exact_header_baudrate = Float64::new(19_199.5);
        message.early_stop_bit_offset = 10;
        message.early_stop_bit_offset_response = 20;
        message.header.base.object_size = LIN_MESSAGE2_V3_SIZE as u32;
        let back = roundtrip(
            &message,
            LinMessage2::write_to,
            LinMessage2::parse,
            LIN_MESSAGE2_V3_SIZE,
        );
        assert_eq!(back, message);
        assert_eq!(back.exact_header_baudrate.value(), 19_199.5);
    }

    #[test]
    fn blf_object_dispatch_and_unknown() {
        let msg = CanMessage::new(NS, 1, 1, 0x100, &[1, 2], true);
        let mut buf = Vec::new();
        msg.write_to(&mut buf);
        let obj = BlfObject::parse(&buf, 0).unwrap();
        assert!(matches!(obj, BlfObject::CanMessage(_)));
        assert_eq!(obj.object_type(), 1);
        assert_eq!(obj.timestamp(), 1);

        let header = LObj::new(ObjectType::CanMessage, NS, 9, 40);
        let mut header = header;
        header.base.object_type = 117;
        let unknown = BlfObject::Raw {
            header,
            data: vec![0xEE; 8],
        };
        let mut buf2 = Vec::new();
        unknown.write_to(&mut buf2).unwrap();
        assert_eq!(buf2.len(), 40);
        let obj2 = BlfObject::parse(&buf2, 0).unwrap();
        assert_eq!(obj2, unknown);

        let bad = BlfObject::Raw {
            header: LObj::new(ObjectType::CanMessage, NS, 0, 99),
            data: vec![0; 4],
        };
        assert!(bad.write_to(&mut Vec::new()).is_err());
    }

    #[test]
    fn blf_object_parse_size_errors() {
        let mut buf = vec![0u8; LOBJ_SIZE];
        buf[0..4].copy_from_slice(b"LOBJ");
        buf[8..12].copy_from_slice(&20u32.to_le_bytes()); // object_size < 32
        assert!(BlfObject::parse(&buf, 0).is_err());
        buf[8..12].copy_from_slice(&100u32.to_le_bytes());
        assert!(BlfObject::parse(&buf, 0).is_err());
    }

    #[test]
    fn remaining_can_objects_dispatch_and_roundtrip() {
        let mut error = CanFdError64::new(NS, 11);
        error.channel = 2;
        error.dlc = 3;
        error.data = vec![1, 2, 3];
        error.valid_data_bytes = 3;
        error.ext_data_offset = (CAN_FD_ERROR64_SIZE + error.data.len()) as u8;
        error.ext_data = Some(CanFdExtFrameData {
            btr_ext_arb: 0x1122_3344,
            btr_ext_data: 0x5566_7788,
            reserved: vec![9, 8],
        });
        error.header.base.object_size = (CAN_FD_ERROR64_SIZE + 3 + 10) as u32;

        let mut setting = CanSettingChanged::new(NS, 12);
        setting.channel = 3;
        setting.changed_type = 1;
        setting.bit_timings.btr_ext_arb = 7;

        let objects = [
            BlfObject::CanOverload(CanOverload::new(NS, 8)),
            BlfObject::CanDriverSync(CanDriverSync::new(NS, 9)),
            BlfObject::CanDriverErrorExt(CanDriverErrorExt::new(NS, 10)),
            BlfObject::CanFdError64(error),
            BlfObject::CanSettingChanged(setting),
        ];
        for object in objects {
            let mut bytes = Vec::new();
            object.write_to(&mut bytes).unwrap();
            assert_eq!(BlfObject::parse(&bytes, 0).unwrap(), object);
        }
    }

    #[test]
    fn opaque_object_supports_future_type_ids() {
        let mut object = BlfObject::opaque(10_000, NS, 42, vec![1, 2, 3]).unwrap();
        assert_eq!(object.object_type(), 10_000);
        assert_eq!(object.opaque_data(), Some([1, 2, 3].as_slice()));
        object.set_opaque_data(vec![4, 5]).unwrap();
        let mut bytes = Vec::new();
        object.write_to(&mut bytes).unwrap();
        assert_eq!(BlfObject::parse(&bytes, 0).unwrap(), object);
    }

    #[test]
    fn matches_can_filter_semantics() {
        let obj = BlfObject::CanMessage(CanMessage::new(NS, 0, 2, 0x123, &[1; 8], true));
        assert!(obj.matches_can_filter(-1, u32::MAX));
        assert!(obj.matches_can_filter(2, 0x123));
        assert!(!obj.matches_can_filter(1, 0x123));
        assert!(!obj.matches_can_filter(2, 0x124));
        let empty = BlfObject::CanMessage(CanMessage::new(NS, 0, 2, 0x123, &[], true));
        assert!(!empty.matches_can_filter(-1, u32::MAX));
        let mut nerr = CanMessage::new(NS, 0, 2, 0x123, &[1; 8], true);
        nerr.flags = CanFlags::NERR;
        assert!(!BlfObject::CanMessage(nerr).matches_can_filter(-1, u32::MAX));
        let mut fd = CanFdManaged::new(NS, 0);
        fd.channel = 1;
        fd.id = 0x55;
        fd.data = vec![1, 2, 3];
        fd.valid_data_bytes = 3;
        assert!(BlfObject::CanFdMessage64(fd).matches_can_filter(1, 0x55));
        let txt = BlfObject::AppText(AppTextManaged::new(NS, 0, 0, "x"));
        assert!(!txt.matches_can_filter(-1, u32::MAX));
    }
}
