//!   `PACKET_ALIGNMENT`.

use autors_a2l::block::Block;
use autors_a2l::model::enums::{A2lKeyword, ChecksumType};
use autors_a2l::model::unsupported::UnsupportedNode;
use autors_a2l::params::ParamCursor;
use autors_a2l::writer::{escape_str, Writer};

use crate::error::{Error, Result};

// ============================================================================
// ============================================================================

fn to_hex<T: std::fmt::UpperHex>(v: T) -> String {
    format!("0x{v:X}")
}

fn to_hex2(v: u8) -> String {
    format!("0x{v:02X}")
}

fn fmt_f32(v: f32) -> String {
    format!("{v}")
}

fn raw_text(cur: &mut ParamCursor) -> Result<String> {
    Ok(cur.next_token()?.text.clone())
}

fn kw_enum<T: A2lKeyword + Default>(s: &str) -> T {
    T::from_keyword(s).unwrap_or_default()
}

fn u8_val(cur: &mut ParamCursor) -> Result<u8> {
    Ok(cur.uint::<u64>()?.min(255) as u8)
}

fn u16_val(cur: &mut ParamCursor) -> Result<u16> {
    Ok(cur.uint::<u64>()?.min(65535) as u16)
}

fn u32_val(cur: &mut ParamCursor) -> Result<u32> {
    Ok(cur.uint::<u64>()?.min(u64::from(u32::MAX)) as u32)
}

fn f32_val(cur: &mut ParamCursor) -> Result<f32> {
    let t = cur.next_token()?;
    t.text
        .parse::<f32>()
        .map_err(|_| Error::Parse(format!("line {}: expected float, got {:?}", t.line, t.text)))
}

fn after_last_underscore(s: &str) -> &str {
    match s.rfind('_') {
        Some(i) => &s[i + 1..],
        None => s,
    }
}

fn after_second_underscore(s: &str) -> &str {
    let mut it = s.match_indices('_');
    if it.next().is_some() {
        if let Some((i, _)) = it.next() {
            return &s[i + 1..];
        }
    }
    s
}

// ============================================================================
// ============================================================================

macro_rules! xcp_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident),* $(,)? }) => {
        $(#[$meta])*
        #[allow(non_camel_case_types)]
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            #[default]
            NotSet,
            $($variant),*
        }

        impl A2lKeyword for $name {
            fn as_keyword(self) -> Option<&'static str> {
                match self {
                    $name::NotSet => None,
                    $($name::$variant => Some(stringify!($variant))),*
                }
            }

            fn from_keyword(kw: &str) -> Option<Self> {
                $(if kw.eq_ignore_ascii_case(stringify!($variant)) {
                    return Some($name::$variant);
                })*
                None
            }
        }
    };
}

xcp_enum! {
    XcpAddressExt { FREE, ODT, DAQ }
}

xcp_enum! {
    XcpAddressMode { DAQ, EVENT, ODT, NONE }
}

xcp_enum! {
    XcpAlignment { _8_BIT, _16_BIT, _32_BIT, _64_BIT }
}

xcp_enum! {
    XcpClockEpoch { ATOMIC_TIME, UNIVERSAL_COORDINATED_TIME, ARBITRARY }
}

xcp_enum! {
    XcpClockMode { XCP_SLAVE_CLOCK, ECU_CLOCK, XCP_SLAVE_GRANDMASTER_CLOCK, ECU_GRANDMASTER_CLOCK }
}

xcp_enum! {
    XcpClockReadability { RANDOMLY_READABLE, LIMITED_READABLE, NOT_READABLE }
}

xcp_enum! {
    XcpClockSyncFeature { SYN_UNSUPPORTED, SYNCHRONIZATION_ONLY, SYNTONIZATION_ONLY, SYN_ALL }
}

xcp_enum! {
    XcpDaqListCanSampleRate { SINGLE, TRIPLE }
}

xcp_enum! {
    XcpDaqListCanType { VARIABLE, FIXED }
}

xcp_enum! {
    XcpDaqListType { DAQ, STIM, DAQ_STIM }
}

xcp_enum! {
    XcpDaqMode { STATIC, DYNAMIC }
}

xcp_enum! {
    XcpEcuAccess { NOT_ALLOWED, WITHOUT_XCP_ONLY, WITH_XCP_ONLY, DONT_CARE }
}

xcp_enum! {
    XcpEndpoint { IN, OUT }
}

xcp_enum! {
    XcpGroupMode { ELEMENT_GROUPED, EVENT_GROUPED }
}

xcp_enum! {
    XcpHeaderLen { BYTE, CTR_BYTE, FILL_BYTE, WORD, CTR_WORD, FILL_WORD }
}

xcp_enum! {
    XcpIdFieldType { ABSOLUTE, BYTE, WORD, ALIGNED }
}

xcp_enum! {
    XcpMemoryAccess { NOT_ALLOWED, ALLOWED }
}

xcp_enum! {
    XcpMessagePacking { SINGLE, MULTIPLE, STREAMING }
}

xcp_enum! {
    XcpNativeTimestampSize { FOUR_BYTE, EIGHT_BYTE }
}

xcp_enum! {
    XcpOdtEntrySize { BYTE, WORD, DWORD, DLONG }
}

xcp_enum! {
    XcpOptimisationType { DEFAULT, ODT_TYPE_16, ODT_TYPE_32, ODT_TYPE_64, ODT_TYPE_ALIGNMENT, MAX_ENTRY_SIZE }
}

xcp_enum! {
    XcpOverloadInd { INDICATION, PID, EVENT }
}

xcp_enum! {
    XcpPackMode { OPTIONAL, MANDATORY }
}

xcp_enum! {
    XcpPacketAligment { _8, _16, _32 }
}

xcp_enum! {
    XcpReadWriteAccess { NOT_ALLOWED, WITHOUT_ECU_ONLY, WITH_ECU_ONLY, DONT_CARE }
}

xcp_enum! {
    XcpResourceState { NOT_ACTIVE, ACTIVE }
}

xcp_enum! {
    XcpStsMode { LAST, FIRST }
}

xcp_enum! {
    XcpSyncEdge { SINGLE, DUAL }
}

xcp_enum! {
    XcpTimestampResolution {
        _1NS, _10NS, _100NS, _1US, _10US, _100US, _1MS, _10MS, _100MS, _1S, _1PS, _10PS, _100PS,
    }
}

impl XcpTimestampResolution {
    pub fn as_u8(self) -> u8 {
        match self {
            XcpTimestampResolution::NotSet => 13,
            XcpTimestampResolution::_1NS => 0,
            XcpTimestampResolution::_10NS => 1,
            XcpTimestampResolution::_100NS => 2,
            XcpTimestampResolution::_1US => 3,
            XcpTimestampResolution::_10US => 4,
            XcpTimestampResolution::_100US => 5,
            XcpTimestampResolution::_1MS => 6,
            XcpTimestampResolution::_10MS => 7,
            XcpTimestampResolution::_100MS => 8,
            XcpTimestampResolution::_1S => 9,
            XcpTimestampResolution::_1PS => 10,
            XcpTimestampResolution::_10PS => 11,
            XcpTimestampResolution::_100PS => 12,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => XcpTimestampResolution::_1NS,
            1 => XcpTimestampResolution::_10NS,
            2 => XcpTimestampResolution::_100NS,
            3 => XcpTimestampResolution::_1US,
            4 => XcpTimestampResolution::_10US,
            5 => XcpTimestampResolution::_100US,
            6 => XcpTimestampResolution::_1MS,
            7 => XcpTimestampResolution::_10MS,
            8 => XcpTimestampResolution::_100MS,
            9 => XcpTimestampResolution::_1S,
            10 => XcpTimestampResolution::_1PS,
            11 => XcpTimestampResolution::_10PS,
            _ => XcpTimestampResolution::_100PS,
        }
    }
}

xcp_enum! {
    XcpTimestampSize { BYTE, WORD, DWORD }
}

xcp_enum! {
    XcpTransceiverDelayCompensation { ON, OFF }
}

xcp_enum! {
    XcpTransfer { BULK_TRANSFER, INTERRUPT_TRANSFER }
}

xcp_enum! {
    XcpTsRelation { XCP_SLAVE_CLOCK, ECU_CLOCK }
}

xcp_enum! {
    ChecksumSxi { NO_CHECKSUM, CHECKSUM_BYTE, CHECKSUM_WORD }
}

xcp_enum! {
    DtoCtrDaqMode { INSERT_COUNTER, INSERT_STIM_COUNTER_COPY }
}

xcp_enum! {
    DtoCtrStimMode { DO_NOT_CHECK_COUNTER, CHECK_COUNTER }
}

xcp_enum! {
    ParityType { NONE, ODD, EVEN }
}

xcp_enum! {
    StopBitsType { ONE_STOP_BIT, TWO_STOP_BITS }
}

xcp_enum! {
    SyncModeSize { BYTE, WORD, DWORD }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommModeBasic(pub u8);

impl CommModeBasic {
    pub const BIG_ENDIAN: u8 = 0x1;
    pub const ADDRESS_GRANULARITY_WORD: u8 = 0x2;
    pub const ADDRESS_GRANULARITY_DWORD: u8 = 0x4;
    pub const SLAVE_BLOCK_MODE: u8 = 0x40;
    pub const OPTIONAL: u8 = 0x80;

    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct XcpBlockMode(pub u8);

impl XcpBlockMode {
    pub const STANDARD: u8 = 0x0;
    pub const SLAVE: u8 = 0x1;
    pub const MASTER: u8 = 0x2;
    pub const INTERLEAVED: u8 = 0x4;

    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    fn from_flag_keyword(kw: &str) -> Option<u8> {
        match kw.to_ascii_uppercase().as_str() {
            "SLAVE" => Some(Self::SLAVE),
            "MASTER" => Some(Self::MASTER),
            "INTERLEAVED" => Some(Self::INTERLEAVED),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct XcpCommModes {
    pub comm_mode: XcpBlockMode,
    pub max_bs: u8,
    pub min_st: u8,
    pub queue_size: u8,
}

impl XcpCommModes {
    fn parse(cur: &mut ParamCursor) -> Result<XcpCommModes> {
        let mut modes = XcpCommModes::default();
        if !cur.take_if("BLOCK") {
            return Ok(modes);
        }
        while !cur.is_empty() {
            let flag = match cur.peek() {
                Some(t) => XcpBlockMode::from_flag_keyword(&t.text),
                None => break,
            };
            let flag = match flag {
                Some(f) => f,
                None => break,
            };
            cur.next_token()?;
            modes.comm_mode.0 |= flag;
            match flag {
                XcpBlockMode::MASTER => {
                    modes.max_bs = cur.uint::<u64>()? as u8;
                    modes.min_st = cur.uint::<u64>()? as u8;
                }
                XcpBlockMode::INTERLEAVED => {
                    modes.queue_size = cur.uint::<u64>()? as u8;
                }
                _ => {}
            }
        }
        Ok(modes)
    }

    fn write(&self, w: &mut Writer) {
        if self.comm_mode.0 == 0 {
            return;
        }
        let mut s = String::from("BLOCK ");
        if self.comm_mode.contains(XcpBlockMode::SLAVE) {
            s.push_str("SLAVE ");
        }
        if self.comm_mode.contains(XcpBlockMode::MASTER) {
            s.push_str(&format!(
                "MASTER {} {} ",
                to_hex2(self.max_bs),
                to_hex2(self.min_st)
            ));
        }
        if self.comm_mode.contains(XcpBlockMode::INTERLEAVED) {
            s.push_str(&format!("INTERLEAVED {} ", to_hex2(self.queue_size)));
        }
        w.value_line(Some("COMMUNICATION_MODE_SUPPORTED"), &s);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PagProperties(pub u8);

impl PagProperties {
    pub const FREEZE_SUPPORTED: u8 = 0x1;

    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct XcpPgmMode(pub u8);

impl XcpPgmMode {
    pub const ABSOLUTE: u8 = 0x1;
    pub const FUNCTIONAL: u8 = 0x2;

    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

// ============================================================================
//
// ============================================================================

fn kw_or_notset<T: A2lKeyword>(v: T) -> &'static str {
    v.as_keyword().unwrap_or("NotSet")
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpAddressMapping {
    pub src_address: u32,
    pub dst_address: u32,
    pub length: u32,
    pub children: Vec<XcpNode>,
}

impl XcpAddressMapping {
    pub const KEYWORD: &'static str = "ADDRESS_MAPPING";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let node = XcpAddressMapping {
            src_address: cur.uint()?,
            dst_address: cur.uint()?,
            length: cur.uint()?,
            children: Vec::new(),
        };
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(
            None,
            &format!(
                "{} {} {}",
                to_hex(self.src_address),
                to_hex(self.dst_address),
                to_hex(self.length)
            ),
        );
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum BufferReserveKind {
    /// `BUFFER_RESERVE`.
    BufferReserve,
    #[default]
    BufferReserveEvent,
}

impl BufferReserveKind {
    fn keyword(self) -> &'static str {
        match self {
            BufferReserveKind::BufferReserve => "BUFFER_RESERVE",
            BufferReserveKind::BufferReserveEvent => "BUFFER_RESERVE_EVENT",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpBufferReserve {
    pub kind: BufferReserveKind,
    pub odt_daq: u8,
    pub odt_stim: u8,
    pub children: Vec<XcpNode>,
}

impl XcpBufferReserve {
    fn parse(block: &Block, kind: BufferReserveKind) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpBufferReserve {
            kind,
            odt_daq: u8_val(&mut cur)?,
            odt_stim: u8_val(&mut cur)?,
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.odt_daq.to_string());
        w.value_line(None, &self.odt_stim.to_string());
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpCanFd {
    pub max_dlc: u16,
    pub data_transfer_baudrate: u32,
    pub sample_point: u8,
    pub secondary_sample_point: u8,
    pub btl_cycles: u8,
    pub sjw: u8,
    pub sync_edge: XcpSyncEdge,
    pub transceiver_delay_compensation: XcpTransceiverDelayCompensation,
    /// `MAX_DLC_REQUIRED`.
    pub max_dlc_required: bool,
    pub children: Vec<XcpNode>,
}

impl Default for XcpCanFd {
    fn default() -> Self {
        XcpCanFd {
            max_dlc: 8,
            data_transfer_baudrate: 0,
            sample_point: 0,
            secondary_sample_point: 0,
            btl_cycles: 0,
            sjw: 0,
            sync_edge: XcpSyncEdge::NotSet,
            transceiver_delay_compensation: XcpTransceiverDelayCompensation::NotSet,
            max_dlc_required: false,
            children: Vec::new(),
        }
    }
}

impl XcpCanFd {
    pub const KEYWORD: &'static str = "CAN_FD";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpCanFd::default();
        let mut cur = ParamCursor::new(block);
        while !cur.is_empty() {
            if cur.take_if("MAX_DLC") {
                node.max_dlc = cur.uint::<u16>()?.max(8);
            } else if cur.take_if("CAN_FD_DATA_TRANSFER_BAUDRATE") {
                node.data_transfer_baudrate = u32_val(&mut cur)?;
            } else if cur.take_if("SAMPLE_POINT") {
                node.sample_point = u8_val(&mut cur)?;
            } else if cur.take_if("SECONDARY_SAMPLE_POINT") {
                node.secondary_sample_point = u8_val(&mut cur)?;
            } else if cur.take_if("BTL_CYCLES") {
                node.btl_cycles = u8_val(&mut cur)?;
            } else if cur.take_if("SJW") {
                node.sjw = u8_val(&mut cur)?;
            } else if cur.take_if("SYNC_EDGE") {
                node.sync_edge = kw_enum(&cur.next_token()?.text);
            } else if cur.take_if("TRANSCEIVER_DELAY_COMPENSATION") {
                node.transceiver_delay_compensation = kw_enum(&cur.next_token()?.text);
            } else if cur.take_if("MAX_DLC_REQUIRED") {
                node.max_dlc_required = true;
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(Some("MAX_DLC"), &self.max_dlc.max(8).to_string());
        if self.data_transfer_baudrate != 0 {
            w.value_line(
                Some("CAN_FD_DATA_TRANSFER_BAUDRATE"),
                &self.data_transfer_baudrate.to_string(),
            );
        }
        if self.sample_point != 0 {
            w.value_line(Some("SAMPLE_POINT"), &self.sample_point.to_string());
        }
        if self.btl_cycles != 0 {
            w.value_line(Some("BTL_CYCLES"), &self.btl_cycles.to_string());
        }
        if self.sjw != 0 {
            w.value_line(Some("SJW"), &to_hex(self.sjw));
        }
        if self.max_dlc_required {
            w.value_line(None, "MAX_DLC_REQUIRED");
        }
        if self.sync_edge != XcpSyncEdge::NotSet {
            w.value_line(Some("SYNC_EDGE"), kw_or_notset(self.sync_edge));
        }
        if self.secondary_sample_point != 0 {
            w.value_line(
                Some("SECONDARY_SAMPLE_POINT"),
                &self.secondary_sample_point.to_string(),
            );
        }
        if self.transceiver_delay_compensation != XcpTransceiverDelayCompensation::NotSet {
            w.value_line(
                Some("TRANSCEIVER_DELAY_COMPENSATION"),
                kw_or_notset(self.transceiver_delay_compensation),
            );
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpChecksum {
    pub check_sum: ChecksumType,
    pub max_block_size: u32,
    pub external_function: String,
    pub mta_block_size_align: u16,
    pub children: Vec<XcpNode>,
}

impl Default for XcpChecksum {
    fn default() -> Self {
        XcpChecksum {
            check_sum: ChecksumType::NotSet,
            max_block_size: u32::MAX,
            external_function: String::new(),
            mta_block_size_align: 0,
            children: Vec::new(),
        }
    }
}

impl XcpChecksum {
    pub const KEYWORD: &'static str = "CHECKSUM";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpChecksum::default();
        let mut cur = ParamCursor::new(block);
        while !cur.is_empty() {
            let t = cur.next_token()?;
            match t.text.get(..3) {
                Some("XCP") => {
                    node.check_sum = ChecksumType::from_keyword(t.text.get(4..).unwrap_or(""))
                        .unwrap_or(ChecksumType::NotSet);
                }
                Some("MAX") => {
                    node.max_block_size = u32_val(&mut cur)?;
                }
                Some("DLL") | Some("EXT") => {
                    node.external_function = raw_text(&mut cur)?;
                }
                Some("MTA") => {
                    node.mta_block_size_align = u16_val(&mut cur)?;
                }
                _ => {}
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        if let Some(kw) = self.check_sum.as_keyword() {
            w.value_line(None, &format!("XCP_{kw}"));
        }
        if self.max_block_size < u32::MAX {
            w.value_line(Some("MAX_BLOCK_SIZE"), &to_hex(self.max_block_size));
        }
        if !self.external_function.is_empty() {
            w.tag_value(
                Some("EXTERNAL_FUNCTION"),
                Some(&escape_str(&self.external_function)),
                true,
            );
        }
        if self.mta_block_size_align > 0 {
            w.tag_value(
                Some("MTA_BLOCK_SIZE_ALIGN"),
                Some(&self.mta_block_size_align.to_string()),
                true,
            );
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpClock {
    pub uuid: [u8; 8],
    pub mode: XcpClockMode,
    pub readability: XcpClockReadability,
    pub feature: XcpClockSyncFeature,
    pub quality: u8,
    pub max_timestamp_value_before_wraparound: u64,
    pub epoch: XcpClockEpoch,
    pub children: Vec<XcpNode>,
}

impl Default for XcpClock {
    fn default() -> Self {
        XcpClock {
            uuid: [0; 8],
            mode: XcpClockMode::NotSet,
            readability: XcpClockReadability::NotSet,
            feature: XcpClockSyncFeature::NotSet,
            quality: 0,
            max_timestamp_value_before_wraparound: 0,
            epoch: XcpClockEpoch::NotSet,
            children: Vec::new(),
        }
    }
}

impl XcpClock {
    pub const KEYWORD: &'static str = "CLOCK";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut node = XcpClock::default();
        for b in &mut node.uuid {
            *b = u8_val(&mut cur)?;
        }
        node.mode = kw_enum(&cur.next_token()?.text);
        node.readability = kw_enum(&cur.next_token()?.text);
        node.feature = kw_enum(&cur.next_token()?.text);
        node.quality = u8_val(&mut cur)?;
        node.max_timestamp_value_before_wraparound = cur.uint()?;
        node.epoch = kw_enum(&cur.next_token()?.text);
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        let uuid = self
            .uuid
            .iter()
            .map(|b| to_hex2(*b))
            .collect::<Vec<_>>()
            .join(" ");
        w.value_line(None, &uuid);
        w.value_line(None, kw_or_notset(self.mode));
        w.value_line(None, kw_or_notset(self.readability));
        w.value_line(None, kw_or_notset(self.feature));
        w.value_line(None, &self.quality.to_string());
        w.value_line(
            None,
            &self.max_timestamp_value_before_wraparound.to_string(),
        );
        w.value_line(None, kw_or_notset(self.epoch));
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpCoreLoad {
    pub core_nr: u32,
    pub core_load: f32,
    pub children: Vec<XcpNode>,
}

impl XcpCoreLoad {
    pub const KEYWORD: &'static str = "CORE_LOAD_EP";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpCoreLoad {
            core_nr: cur.uint()?,
            core_load: f32_val(&mut cur)?,
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.core_nr.to_string());
        w.value_line(None, &fmt_f32(self.core_load));
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpDaq {
    pub mode: XcpDaqMode,
    pub max_daq: u16,
    pub max_evt_chn: u16,
    pub min_daq: u8,
    pub opt_type: XcpOptimisationType,
    pub adr_ext: XcpAddressExt,
    pub id_field: XcpIdFieldType,
    pub odt_entry_size: XcpOdtEntrySize,
    pub max_odt_entry_size: u8,
    pub overload_ind: XcpOverloadInd,
    pub daq_alternating_supported: u16,
    /// `PRESCALER_SUPPORTED`.
    pub prescaler_supported: bool,
    /// `RESUME_SUPPORTED`.
    pub resume_supported: bool,
    /// `STORE_DAQ_SUPPORTED`.
    pub store_daq_supported: bool,
    /// `DTO_CTR_FIELD_SUPPORTED`.
    pub dto_ctr_field_supported: bool,
    /// `PID_OFF_SUPPORTED`.
    pub pid_off_supported: bool,
    pub max_daq_total: u16,
    pub max_odt_total: u16,
    pub max_odt_daq_total: u16,
    pub max_odt_stim_total: u16,
    pub max_odt_entries_total: u16,
    pub max_odt_entries_daq_total: u16,
    pub max_odt_entries_stim_total: u16,
    pub cpu_load_max_total: f32,
    pub core_load_max_total: f32,
    pub children: Vec<XcpNode>,
}

impl Default for XcpDaq {
    fn default() -> Self {
        XcpDaq {
            mode: XcpDaqMode::NotSet,
            max_daq: 0,
            max_evt_chn: 0,
            min_daq: 0,
            opt_type: XcpOptimisationType::NotSet,
            adr_ext: XcpAddressExt::NotSet,
            id_field: XcpIdFieldType::NotSet,
            odt_entry_size: XcpOdtEntrySize::BYTE,
            max_odt_entry_size: 0,
            overload_ind: XcpOverloadInd::NotSet,
            daq_alternating_supported: u16::MAX,
            prescaler_supported: false,
            resume_supported: false,
            store_daq_supported: false,
            dto_ctr_field_supported: false,
            pid_off_supported: false,
            max_daq_total: u16::MAX,
            max_odt_total: u16::MAX,
            max_odt_daq_total: u16::MAX,
            max_odt_stim_total: u16::MAX,
            max_odt_entries_total: u16::MAX,
            max_odt_entries_daq_total: u16::MAX,
            max_odt_entries_stim_total: u16::MAX,
            cpu_load_max_total: 0.0,
            core_load_max_total: 0.0,
            children: Vec::new(),
        }
    }
}

impl XcpDaq {
    pub const KEYWORD: &'static str = "DAQ";

    pub fn id_field_size(&self) -> u8 {
        match self.id_field {
            XcpIdFieldType::ABSOLUTE => 1,
            XcpIdFieldType::BYTE => 2,
            XcpIdFieldType::WORD => 3,
            XcpIdFieldType::ALIGNED => 4,
            XcpIdFieldType::NotSet => 0,
        }
    }

    pub fn is_daq_dynamic(&self) -> bool {
        self.mode == XcpDaqMode::DYNAMIC
    }

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpDaq::default();
        let mut cur = ParamCursor::new(block);
        node.mode = kw_enum(&cur.next_token()?.text);
        node.max_daq = u16_val(&mut cur)?;
        node.max_evt_chn = u16_val(&mut cur)?;
        node.min_daq = u8_val(&mut cur)?;
        node.opt_type = kw_enum(after_second_underscore(&cur.next_token()?.text));
        node.adr_ext = kw_enum(after_last_underscore(&cur.next_token()?.text));
        let t = cur.next_token()?;
        if t.text.ends_with("BYTE") {
            node.id_field = XcpIdFieldType::BYTE;
        } else if t.text.ends_with("WORD") {
            node.id_field = XcpIdFieldType::WORD;
        } else if t.text.ends_with("ALIGNED") {
            node.id_field = XcpIdFieldType::ALIGNED;
        } else {
            node.id_field = XcpIdFieldType::ABSOLUTE;
        }
        node.odt_entry_size = kw_enum(after_last_underscore(&cur.next_token()?.text));
        node.max_odt_entry_size = u8_val(&mut cur)?;
        node.overload_ind = kw_enum(after_last_underscore(&cur.next_token()?.text));
        while !cur.is_empty() {
            if cur.take_if("DAQ_ALTERNATING_SUPPORTED") {
                node.daq_alternating_supported = cur.uint()?;
            } else if cur.take_if("PRESCALER_SUPPORTED") {
                node.prescaler_supported = true;
            } else if cur.take_if("RESUME_SUPPORTED") {
                node.resume_supported = true;
            } else if cur.take_if("STORE_DAQ_SUPPORTED") {
                node.store_daq_supported = true;
            } else if cur.take_if("DTO_CTR_FIELD_SUPPORTED") {
                node.dto_ctr_field_supported = true;
            } else if cur.take_if("PID_OFF_SUPPORTED") {
                node.pid_off_supported = true;
            } else if cur.take_if("MAX_DAQ_TOTAL") {
                node.max_daq_total = u16_val(&mut cur)?;
            } else if cur.take_if("MAX_ODT_TOTAL") {
                node.max_odt_total = u16_val(&mut cur)?;
            } else if cur.take_if("MAX_ODT_DAQ_TOTAL") {
                node.max_odt_daq_total = u16_val(&mut cur)?;
            } else if cur.take_if("MAX_ODT_STIM_TOTAL") {
                node.max_odt_stim_total = u16_val(&mut cur)?;
            } else if cur.take_if("MAX_ODT_ENTRIES_TOTAL") {
                node.max_odt_entries_total = u16_val(&mut cur)?;
            } else if cur.take_if("MAX_ODT_ENTRIES_DAQ_TOTAL") {
                node.max_odt_entries_daq_total = u16_val(&mut cur)?;
            } else if cur.take_if("MAX_ODT_ENTRIES_STIM_TOTAL") {
                node.max_odt_entries_stim_total = u16_val(&mut cur)?;
            } else if cur.take_if("CPU_LOAD_MAX_TOTAL") {
                node.cpu_load_max_total = f32_val(&mut cur)?;
            } else if cur.take_if("CORE_LOAD_MAX_TOTAL") {
                node.core_load_max_total = f32_val(&mut cur)?;
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, kw_or_notset(self.mode));
        w.value_line(None, &to_hex(self.max_daq));
        w.value_line(None, &to_hex(self.max_evt_chn));
        w.value_line(None, &to_hex(self.min_daq));
        w.value_line(
            None,
            &format!("OPTIMISATION_TYPE_{}", kw_or_notset(self.opt_type)),
        );
        w.value_line(
            None,
            &format!("ADDRESS_EXTENSION_{}", kw_or_notset(self.adr_ext)),
        );
        match self.id_field {
            XcpIdFieldType::ABSOLUTE => {
                w.value_line(None, "IDENTIFICATION_FIELD_TYPE_ABSOLUTE");
            }
            XcpIdFieldType::BYTE | XcpIdFieldType::WORD => {
                w.value_line(
                    None,
                    &format!(
                        "IDENTIFICATION_FIELD_TYPE_RELATIVE_{}",
                        kw_or_notset(self.id_field)
                    ),
                );
            }
            XcpIdFieldType::ALIGNED => {
                w.value_line(None, "IDENTIFICATION_FIELD_TYPE_RELATIVE_WORD_ALIGNED");
            }
            XcpIdFieldType::NotSet => {}
        }
        w.value_line(
            None,
            &format!(
                "GRANULARITY_ODT_ENTRY_SIZE_DAQ_{}",
                kw_or_notset(self.odt_entry_size)
            ),
        );
        w.value_line(None, &to_hex(self.max_odt_entry_size));
        if self.overload_ind == XcpOverloadInd::INDICATION {
            w.value_line(None, "NO_OVERLOAD_INDICATION");
        } else {
            w.value_line(
                None,
                &format!("OVERLOAD_INDICATION_{}", kw_or_notset(self.overload_ind)),
            );
        }
        if self.daq_alternating_supported != u16::MAX {
            w.value_line(
                Some("DAQ_ALTERNATING_SUPPORTED"),
                &self.daq_alternating_supported.to_string(),
            );
        }
        if self.prescaler_supported {
            w.value_line(None, "PRESCALER_SUPPORTED");
        }
        if self.resume_supported {
            w.value_line(None, "RESUME_SUPPORTED");
        }
        if self.store_daq_supported {
            w.value_line(None, "STORE_DAQ_SUPPORTED");
        }
        if self.dto_ctr_field_supported {
            w.value_line(None, "DTO_CTR_FIELD_SUPPORTED");
        }
        if self.pid_off_supported {
            w.value_line(None, "PID_OFF_SUPPORTED");
        }
        if self.max_daq_total < u16::MAX {
            w.value_line(Some("MAX_DAQ_TOTAL"), &self.max_daq_total.to_string());
        }
        if self.max_odt_total < u16::MAX {
            w.value_line(Some("MAX_ODT_TOTAL"), &self.max_odt_total.to_string());
        }
        if self.max_odt_daq_total < u16::MAX {
            w.value_line(
                Some("MAX_ODT_DAQ_TOTAL"),
                &self.max_odt_daq_total.to_string(),
            );
        }
        if self.max_odt_stim_total < u16::MAX {
            w.value_line(
                Some("MAX_ODT_STIM_TOTAL"),
                &self.max_odt_stim_total.to_string(),
            );
        }
        if self.max_odt_entries_total < u16::MAX {
            w.value_line(
                Some("MAX_ODT_ENTRIES_TOTAL"),
                &self.max_odt_entries_total.to_string(),
            );
        }
        if self.max_odt_entries_daq_total < u16::MAX {
            w.value_line(
                Some("MAX_ODT_ENTRIES_DAQ_TOTAL"),
                &self.max_odt_entries_daq_total.to_string(),
            );
        }
        if self.max_odt_entries_stim_total < u16::MAX {
            w.value_line(
                Some("MAX_ODT_ENTRIES_STIM_TOTAL"),
                &self.max_odt_entries_stim_total.to_string(),
            );
        }
        if self.cpu_load_max_total > 0.0 {
            w.value_line(
                Some("CPU_LOAD_MAX_TOTAL"),
                &fmt_f32(self.cpu_load_max_total),
            );
        }
        if self.core_load_max_total > 0.0 {
            w.value_line(
                Some("CORE_LOAD_MAX_TOTAL"),
                &fmt_f32(self.core_load_max_total),
            );
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpDaqEvent {
    pub name: String,
    pub events: Vec<u16>,
    pub children: Vec<XcpNode>,
}

impl XcpDaqEvent {
    pub const KEYWORD: &'static str = "DAQ_EVENT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut node = XcpDaqEvent {
            name: raw_text(&mut cur)?,
            events: Vec::new(),
            children: Vec::new(),
        };
        while cur.remaining() >= 2 {
            if cur.take_if("EVENT") {
                let n = cur.uint()?;
                if !node.events.contains(&n) {
                    node.events.push(n);
                }
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        for n in &self.events {
            w.value_line(Some("EVENT"), &n.to_string());
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpDaqList {
    pub daq_no: u16,
    pub daq_list_type: XcpDaqListType,
    pub max_odt: u8,
    pub max_odt_entries: u8,
    pub event_fixed: u16,
    pub first_pid: u8,
    /// `DAQ_PACKED_MODE_SUPPORTED`.
    pub packed_mode_supported: bool,
    pub active: bool,
    pub children: Vec<XcpNode>,
}

impl Default for XcpDaqList {
    fn default() -> Self {
        XcpDaqList {
            daq_no: 0,
            daq_list_type: XcpDaqListType::NotSet,
            max_odt: 0,
            max_odt_entries: 0,
            event_fixed: u16::MAX,
            first_pid: u8::MAX,
            packed_mode_supported: false,
            active: true,
            children: Vec::new(),
        }
    }
}

impl XcpDaqList {
    pub const KEYWORD: &'static str = "DAQ_LIST";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpDaqList::default();
        let mut cur = ParamCursor::new(block);
        node.daq_no = cur.uint()?;
        while !cur.is_empty() {
            if cur.take_if("DAQ_LIST_TYPE") {
                node.daq_list_type = kw_enum(&cur.next_token()?.text);
            } else if cur.take_if("MAX_ODT") {
                node.max_odt = u8_val(&mut cur)?;
            } else if cur.take_if("MAX_ODT_ENTRIES") {
                node.max_odt_entries = u8_val(&mut cur)?;
            } else if cur.take_if("EVENT_FIXED") {
                node.event_fixed = cur.uint()?;
            } else if cur.take_if("FIRST_PID") {
                node.first_pid = u8_val(&mut cur)?;
            } else if cur.take_if("DAQ_PACKED_MODE_SUPPORTED") {
                node.packed_mode_supported = true;
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.daq_no.to_string());
        if self.daq_list_type != XcpDaqListType::NotSet {
            w.value_line(Some("DAQ_LIST_TYPE"), kw_or_notset(self.daq_list_type));
        }
        w.value_line(Some("MAX_ODT"), &self.max_odt.to_string());
        w.value_line(Some("MAX_ODT_ENTRIES"), &self.max_odt_entries.to_string());
        if self.event_fixed != u16::MAX {
            w.value_line(Some("EVENT_FIXED"), &self.event_fixed.to_string());
        }
        if self.first_pid < u8::MAX {
            w.value_line(Some("FIRST_PID"), &to_hex(self.first_pid));
        }
        if self.packed_mode_supported {
            w.value_line(None, "DAQ_PACKED_MODE_SUPPORTED");
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpDaqListCanId {
    pub daq_no: u16,
    /// `VARIABLE` / `FIXED <CAN-ID>`.
    pub daq_list_type: XcpDaqListCanType,
    pub can_id: u32,
    pub children: Vec<XcpNode>,
}

impl XcpDaqListCanId {
    pub const KEYWORD: &'static str = "DAQ_LIST_CAN_ID";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpDaqListCanId::default();
        let mut cur = ParamCursor::new(block);
        node.daq_no = cur.uint()?;
        while !cur.is_empty() {
            if cur.take_if("VARIABLE") {
                node.daq_list_type = XcpDaqListCanType::VARIABLE;
            } else if cur.take_if("FIXED") {
                node.daq_list_type = XcpDaqListCanType::FIXED;
                node.can_id = u32_val(&mut cur)?;
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.daq_no.to_string());
        match self.daq_list_type {
            XcpDaqListCanType::VARIABLE => w.value_line(None, "VARIABLE"),
            XcpDaqListCanType::FIXED => w.value_line(Some("FIXED"), &to_hex(self.can_id)),
            XcpDaqListCanType::NotSet => {}
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpDaqListUsbEndpoint {
    pub daq_no: u16,
    pub ep_no: u8,
    /// `FIXED_IN <n>` / `FIXED_OUT <n>`.
    pub ep_type: XcpEndpoint,
    pub children: Vec<XcpNode>,
}

impl Default for XcpDaqListUsbEndpoint {
    fn default() -> Self {
        XcpDaqListUsbEndpoint {
            daq_no: 0,
            ep_no: u8::MAX,
            ep_type: XcpEndpoint::NotSet,
            children: Vec::new(),
        }
    }
}

impl XcpDaqListUsbEndpoint {
    pub const KEYWORD: &'static str = "DAQ_LIST_USB_ENDPOINT";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpDaqListUsbEndpoint::default();
        let mut cur = ParamCursor::new(block);
        node.daq_no = cur.uint()?;
        while !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.len() >= 6 {
                node.ep_type = kw_enum(&t.text[6..]);
                if matches!(node.ep_type, XcpEndpoint::IN | XcpEndpoint::OUT) {
                    node.ep_no = u8_val(&mut cur)?;
                }
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.daq_no));
        match self.ep_type {
            XcpEndpoint::IN => w.value_line(None, &format!("FIXED_IN {}", self.ep_no)),
            XcpEndpoint::OUT => w.value_line(None, &format!("FIXED_OUT {}", self.ep_no)),
            XcpEndpoint::NotSet => {}
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpDaqMemoryConsumption {
    pub daq_memory_limit: u32,
    pub daq_size: u16,
    pub odt_size: u16,
    pub odt_entry_size: u16,
    pub odt_daq_buffer_element_size: u16,
    pub odt_stim_buffer_element_size: u16,
    pub children: Vec<XcpNode>,
}

impl XcpDaqMemoryConsumption {
    pub const KEYWORD: &'static str = "DAQ_MEMORY_CONSUMPTION";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpDaqMemoryConsumption {
            daq_memory_limit: u32_val(&mut cur)?,
            daq_size: u16_val(&mut cur)?,
            odt_size: u16_val(&mut cur)?,
            odt_entry_size: u16_val(&mut cur)?,
            odt_daq_buffer_element_size: u16_val(&mut cur)?,
            odt_stim_buffer_element_size: u16_val(&mut cur)?,
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.daq_memory_limit.to_string());
        w.value_line(None, &self.daq_size.to_string());
        w.value_line(None, &self.odt_size.to_string());
        w.value_line(None, &self.odt_entry_size.to_string());
        w.value_line(None, &self.odt_daq_buffer_element_size.to_string());
        w.value_line(None, &self.odt_stim_buffer_element_size.to_string());
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEcuStatesMemoryAccess {
    pub read_access: XcpMemoryAccess,
    pub write_access: XcpMemoryAccess,
    pub children: Vec<XcpNode>,
}

impl XcpEcuStatesMemoryAccess {
    pub const KEYWORD: &'static str = "MEMORY_ACCESS";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let read = cur.next_token()?;
        let write = cur.next_token()?;
        Ok(XcpEcuStatesMemoryAccess {
            read_access: kw_enum(read.text.get(12..).unwrap_or("")),
            write_access: kw_enum(write.text.get(13..).unwrap_or("")),
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(
            None,
            &format!("READ_ACCESS_{}", kw_or_notset(self.read_access)),
        );
        w.value_line(
            None,
            &format!("WRITE_ACCESS_{}", kw_or_notset(self.write_access)),
        );
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEcuStatesState {
    pub state_number: u8,
    pub state_name: String,
    pub cal_pag: XcpResourceState,
    pub daq: XcpResourceState,
    pub stim: XcpResourceState,
    pub pgm: XcpResourceState,
    pub children: Vec<XcpNode>,
}

impl XcpEcuStatesState {
    pub const KEYWORD: &'static str = "STATE";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpEcuStatesState {
            state_number: u8_val(&mut cur)?,
            state_name: raw_text(&mut cur)?,
            cal_pag: kw_enum(&cur.next_token()?.text),
            daq: kw_enum(&cur.next_token()?.text),
            stim: kw_enum(&cur.next_token()?.text),
            pgm: kw_enum(&cur.next_token()?.text),
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.state_number.to_string());
        w.tag_value(None, Some(&escape_str(&self.state_name)), true);
        w.value_line(None, kw_or_notset(self.cal_pag));
        w.value_line(None, kw_or_notset(self.daq));
        w.value_line(None, kw_or_notset(self.stim));
        w.value_line(None, kw_or_notset(self.pgm));
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpEndpointFields {
    pub ep_no: u8,
    pub transfer_type: XcpTransfer,
    pub max_pkt_size: u16,
    pub ep_poll_interval: u8,
    pub packing: XcpMessagePacking,
    pub alignment: XcpAlignment,
    pub host_buffer_size: u16,
}

impl Default for XcpEndpointFields {
    fn default() -> Self {
        XcpEndpointFields {
            ep_no: 0,
            transfer_type: XcpTransfer::NotSet,
            max_pkt_size: 0,
            ep_poll_interval: 0,
            packing: XcpMessagePacking::NotSet,
            alignment: XcpAlignment::_8_BIT,
            host_buffer_size: u16::MAX,
        }
    }
}

impl XcpEndpointFields {
    fn parse(cur: &mut ParamCursor) -> Result<Self> {
        let mut f = XcpEndpointFields {
            ep_no: u8_val(cur)?,
            transfer_type: kw_enum(&cur.next_token()?.text),
            max_pkt_size: u16_val(cur)?,
            ep_poll_interval: u8_val(cur)?,
            packing: kw_enum(cur.next_token()?.text.get(16..).unwrap_or("")),
            alignment: kw_enum(cur.next_token()?.text.get(9..).unwrap_or("")),
            ..XcpEndpointFields::default()
        };
        while !cur.is_empty() {
            if cur.take_if("RECOMMENDED_HOST_BUFSIZE") {
                f.host_buffer_size = u16_val(cur)?;
            } else {
                cur.next_token()?;
            }
        }
        Ok(f)
    }

    fn write_body(&self, w: &mut Writer) {
        w.value_line(None, &to_hex(self.ep_no));
        w.value_line(None, kw_or_notset(self.transfer_type));
        w.value_line(None, &to_hex(self.max_pkt_size));
        w.value_line(None, &self.ep_poll_interval.to_string());
        w.value_line(
            None,
            &format!("MESSAGE_PACKING_{}", kw_or_notset(self.packing)),
        );
        w.value_line(None, &format!("ALIGNMENT{}", kw_or_notset(self.alignment)));
        if self.host_buffer_size != u16::MAX {
            w.value_line(
                Some("RECOMMENDED_HOST_BUFSIZE"),
                &to_hex(self.host_buffer_size),
            );
        }
    }
}

macro_rules! endpoint_node {
    ($(#[$meta:meta])* $name:ident, $kw:literal) => {
        $(#[$meta])*
        #[derive(Debug, Default, Clone, PartialEq)]
        pub struct $name {
            pub endpoint: XcpEndpointFields,
            pub children: Vec<XcpNode>,
        }

        impl $name {
            pub const KEYWORD: &'static str = $kw;

            fn parse(block: &Block) -> Result<Self> {
                let mut cur = ParamCursor::new(block);
                Ok($name {
                    endpoint: XcpEndpointFields::parse(&mut cur)?,
                    children: Vec::new(),
                })
            }

            fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
                self.endpoint.write_body(w);
                write_children(&self.children, w, plus)
            }
        }
    };
}

endpoint_node! {
    XcpOutEpCmdStim, "OUT_EP_CMD_STIM"
}

endpoint_node! {
    XcpOutEpOnlyStim, "OUT_EP_ONLY_STIM"
}

endpoint_node! {
    XcpInEpOnlyDaq, "IN_EP_ONLY_DAQ"
}

endpoint_node! {
    XcpInEpOnlyEvServ, "IN_EP_ONLY_EVSERV"
}

endpoint_node! {
    XcpInEpResErrDaqEvServ, "IN_EP_RESERR_DAQ_EVSERV"
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpEvent {
    pub name_long: String,
    pub name_short: String,
    pub id: u16,
    pub daq_list_type: XcpDaqListType,
    pub max_daq_list: u8,
    pub time_cycle: u8,
    pub time_unit: XcpTimestampResolution,
    pub priority: u8,
    pub complementary_bypass_event_channel_number: u16,
    pub consistency: XcpAddressMode,
    /// `EVENT_COUNTER_PRESENT`(XCPplus).
    pub event_counter_present: bool,
    pub related_event_channel_number: u16,
    /// `RELATED_EVENT_CHANNEL_NUMBER_FIXED`(XCPplus).
    pub related_event_channel_number_fixed: bool,
    /// `DTO_CTR_DAQ_MODE_FIXED`(XCPplus).
    pub dto_ctr_daq_mode_fixed: bool,
    pub dto_ctr_daq_mode: DtoCtrDaqMode,
    /// `DTO_CTR_STIM_MODE_FIXED`(XCPplus).
    pub dto_ctr_stim_mode_fixed: bool,
    pub dto_ctr_stim_mode: DtoCtrStimMode,
    /// `STIM_DTO_CTR_COPY_PRESENT`(XCPplus).
    pub stim_dto_ctr_copy_present: bool,
    pub cpu_load_max: f32,
    pub children: Vec<XcpNode>,
}

impl Default for XcpEvent {
    fn default() -> Self {
        XcpEvent {
            name_long: String::new(),
            name_short: String::new(),
            id: 0,
            daq_list_type: XcpDaqListType::NotSet,
            max_daq_list: 0,
            time_cycle: 0,
            time_unit: XcpTimestampResolution::NotSet,
            priority: 0,
            complementary_bypass_event_channel_number: u16::MAX,
            consistency: XcpAddressMode::NotSet,
            event_counter_present: false,
            related_event_channel_number: u16::MAX,
            related_event_channel_number_fixed: false,
            dto_ctr_daq_mode_fixed: false,
            dto_ctr_daq_mode: DtoCtrDaqMode::NotSet,
            dto_ctr_stim_mode_fixed: false,
            dto_ctr_stim_mode: DtoCtrStimMode::NotSet,
            stim_dto_ctr_copy_present: false,
            cpu_load_max: 0.0,
            children: Vec::new(),
        }
    }
}

impl XcpEvent {
    pub const KEYWORD: &'static str = "EVENT";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpEvent::default();
        let mut cur = ParamCursor::new(block);
        node.name_long = raw_text(&mut cur)?;
        node.name_short = raw_text(&mut cur)?;
        node.id = cur.uint()?;
        node.daq_list_type = kw_enum(&cur.next_token()?.text);
        node.max_daq_list = u8_val(&mut cur)?;
        node.time_cycle = u8_val(&mut cur)?;
        node.time_unit = XcpTimestampResolution::from_u8(u8_val(&mut cur)?);
        node.priority = u8_val(&mut cur)?;
        while !cur.is_empty() {
            if cur.take_if("COMPLEMENTARY_BYPASS_EVENT_CHANNEL_NUMBER") {
                node.complementary_bypass_event_channel_number = u16_val(&mut cur)?;
            } else if cur.take_if("CONSISTENCY") {
                node.consistency = kw_enum(&cur.next_token()?.text);
            } else if cur.take_if("EVENT_COUNTER_PRESENT") {
                node.event_counter_present = true;
            } else if cur.take_if("RELATED_EVENT_CHANNEL_NUMBER") {
                node.related_event_channel_number = u16_val(&mut cur)?;
            } else if cur.take_if("RELATED_EVENT_CHANNEL_NUMBER_FIXED") {
                node.related_event_channel_number_fixed = true;
            } else if cur.take_if("DTO_CTR_DAQ_MODE_FIXED") {
                node.dto_ctr_daq_mode_fixed = true;
            } else if cur.take_if("DTO_CTR_DAQ_MODE") {
                node.dto_ctr_daq_mode = kw_enum(&cur.next_token()?.text);
            } else if cur.take_if("DTO_CTR_STIM_MODE_FIXED") {
                node.dto_ctr_stim_mode_fixed = true;
            } else if cur.take_if("DTO_CTR_STIM_MODE") {
                node.dto_ctr_stim_mode = kw_enum(&cur.next_token()?.text);
            } else if cur.take_if("STIM_DTO_CTR_COPY_PRESENT") {
                node.stim_dto_ctr_copy_present = true;
            } else if cur.take_if("CPU_LOAD_MAX") {
                node.cpu_load_max = f32_val(&mut cur)?;
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.tag_value(None, Some(&escape_str(&self.name_long)), true);
        w.tag_value(None, Some(&escape_str(&self.name_short)), true);
        w.value_line(None, &self.id.to_string());
        w.value_line(None, kw_or_notset(self.daq_list_type));
        w.value_line(None, &self.max_daq_list.to_string());
        w.value_line(None, &self.time_cycle.to_string());
        w.value_line(None, &self.time_unit.as_u8().to_string());
        w.value_line(None, &self.priority.to_string());
        if self.complementary_bypass_event_channel_number < u16::MAX {
            w.value_line(
                Some("COMPLEMENTARY_BYPASS_EVENT_CHANNEL_NUMBER"),
                &self.complementary_bypass_event_channel_number.to_string(),
            );
        }
        if self.consistency != XcpAddressMode::NotSet {
            w.value_line(Some("CONSISTENCY"), kw_or_notset(self.consistency));
        }
        if plus {
            if self.event_counter_present {
                w.value_line(None, "EVENT_COUNTER_PRESENT");
            }
            if self.related_event_channel_number < u16::MAX {
                w.value_line(
                    Some("RELATED_EVENT_CHANNEL_NUMBER"),
                    &self.related_event_channel_number.to_string(),
                );
            }
            if self.related_event_channel_number_fixed {
                w.value_line(None, "RELATED_EVENT_CHANNEL_NUMBER_FIXED");
            }
            if self.dto_ctr_daq_mode != DtoCtrDaqMode::NotSet {
                w.value_line(
                    Some("DTO_CTR_DAQ_MODE"),
                    kw_or_notset(self.dto_ctr_daq_mode),
                );
            }
            if self.dto_ctr_daq_mode_fixed {
                w.value_line(None, "DTO_CTR_DAQ_MODE_FIXED");
            }
            if self.dto_ctr_stim_mode != DtoCtrStimMode::NotSet {
                w.value_line(
                    Some("DTO_CTR_STIM_MODE"),
                    kw_or_notset(self.dto_ctr_stim_mode),
                );
            }
            if self.dto_ctr_stim_mode_fixed {
                w.value_line(None, "DTO_CTR_STIM_MODE_FIXED");
            }
            if self.stim_dto_ctr_copy_present {
                w.value_line(None, "STIM_DTO_CTR_COPY_PRESENT");
            }
            if self.cpu_load_max > 0.0 {
                w.value_line(Some("CPU_LOAD_MAX"), &fmt_f32(self.cpu_load_max));
            }
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEventCanIdList {
    pub evt_no: u16,
    pub fixed_can_ids: Vec<u32>,
    pub children: Vec<XcpNode>,
}

impl XcpEventCanIdList {
    pub const KEYWORD: &'static str = "EVENT_CAN_ID_LIST";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpEventCanIdList::default();
        let mut cur = ParamCursor::new(block);
        node.evt_no = cur.uint()?;
        while cur.remaining() >= 2 {
            if cur.take_if("FIXED") {
                let id = u32_val(&mut cur)?;
                if !node.fixed_can_ids.contains(&id) {
                    node.fixed_can_ids.push(id);
                }
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.evt_no.to_string());
        for id in &self.fixed_can_ids {
            w.value_line(Some("FIXED"), &to_hex(*id));
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CpuLoadConsumptionKind {
    /// `CPU_LOAD_CONSUMPTION_DAQ`.
    #[default]
    Daq,
    /// `CPU_LOAD_CONSUMPTION_STIM`.
    Stim,
}

impl CpuLoadConsumptionKind {
    fn keyword(self) -> &'static str {
        match self {
            CpuLoadConsumptionKind::Daq => "CPU_LOAD_CONSUMPTION_DAQ",
            CpuLoadConsumptionKind::Stim => "CPU_LOAD_CONSUMPTION_STIM",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEventCpuLoadConsumption {
    pub kind: CpuLoadConsumptionKind,
    pub daq_factor: f32,
    pub odt_factor: f32,
    pub odt_entry_factor: f32,
    pub children: Vec<XcpNode>,
}

impl XcpEventCpuLoadConsumption {
    fn parse(block: &Block, kind: CpuLoadConsumptionKind) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpEventCpuLoadConsumption {
            kind,
            daq_factor: f32_val(&mut cur)?,
            odt_factor: f32_val(&mut cur)?,
            odt_entry_factor: f32_val(&mut cur)?,
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &fmt_f32(self.daq_factor));
        w.value_line(None, &fmt_f32(self.odt_factor));
        w.value_line(None, &fmt_f32(self.odt_entry_factor));
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CpuLoadConsumptionQueueKind {
    /// `CPU_LOAD_CONSUMPTION_QUEUE`.
    #[default]
    Queue,
    /// `CPU_LOAD_CONSUMPTION_QUEUE_STIM`.
    QueueStim,
}

impl CpuLoadConsumptionQueueKind {
    fn keyword(self) -> &'static str {
        match self {
            CpuLoadConsumptionQueueKind::Queue => "CPU_LOAD_CONSUMPTION_QUEUE",
            CpuLoadConsumptionQueueKind::QueueStim => "CPU_LOAD_CONSUMPTION_QUEUE_STIM",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEventCpuLoadConsumptionQueue {
    pub kind: CpuLoadConsumptionQueueKind,
    pub odt_factor: f32,
    pub odt_element_load: f32,
    pub children: Vec<XcpNode>,
}

impl XcpEventCpuLoadConsumptionQueue {
    fn parse(block: &Block, kind: CpuLoadConsumptionQueueKind) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpEventCpuLoadConsumptionQueue {
            kind,
            odt_factor: f32_val(&mut cur)?,
            odt_element_load: f32_val(&mut cur)?,
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &fmt_f32(self.odt_factor));
        w.value_line(None, &fmt_f32(self.odt_element_load));
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEventDaqPackedMode {
    pub group_mode: XcpGroupMode,
    pub sts_mode: XcpStsMode,
    pub pack_mode: XcpPackMode,
    pub sample_count: u16,
    pub alt_sample_counts: Vec<u16>,
    pub children: Vec<XcpNode>,
}

impl XcpEventDaqPackedMode {
    pub const KEYWORD: &'static str = "DAQ_PACKED_MODE";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpEventDaqPackedMode::default();
        let mut cur = ParamCursor::new(block);
        node.group_mode = kw_enum(&cur.next_token()?.text);
        node.sts_mode = kw_enum(cur.next_token()?.text.get(4..).unwrap_or(""));
        node.pack_mode = kw_enum(&cur.next_token()?.text);
        node.sample_count = cur.uint()?;
        while !cur.is_empty() {
            if cur.take_if("ALT_SAMPLE_COUNT") {
                node.alt_sample_counts.push(cur.uint()?);
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, kw_or_notset(self.group_mode));
        w.value_line(None, &format!("STS_{}", kw_or_notset(self.sts_mode)));
        w.value_line(None, kw_or_notset(self.pack_mode));
        w.value_line(None, &self.sample_count.to_string());
        for n in &self.alt_sample_counts {
            w.value_line(Some("ALT_SAMPLE_COUNT"), &n.to_string());
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum EventListKind {
    /// `AVAILABLE_EVENT_LIST`.
    #[default]
    Available,
    /// `DEFAULT_EVENT_LIST`.
    Default,
    /// `CONSISTENCY_EVENT_LIST`.
    Consistency,
}

impl EventListKind {
    fn keyword(self) -> &'static str {
        match self {
            EventListKind::Available => "AVAILABLE_EVENT_LIST",
            EventListKind::Default => "DEFAULT_EVENT_LIST",
            EventListKind::Consistency => "CONSISTENCY_EVENT_LIST",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEventList {
    pub kind: EventListKind,
    pub events: Vec<u16>,
    pub children: Vec<XcpNode>,
}

impl XcpEventList {
    fn parse(block: &Block, kind: EventListKind) -> Result<Self> {
        let mut node = XcpEventList {
            kind,
            events: Vec::new(),
            children: Vec::new(),
        };
        let mut cur = ParamCursor::new(block);
        while cur.remaining() >= 2 {
            if cur.take_if("EVENT") {
                let n = cur.uint()?;
                if !node.events.contains(&n) {
                    node.events.push(n);
                }
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        for n in &self.events {
            w.value_line(Some("EVENT"), &n.to_string());
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum MinCycleTimeKind {
    /// `MIN_CYCLE_TIME`.
    #[default]
    MinCycleTime,
    /// `CORE_LOAD_MAX`.
    CoreLoadMax,
}

impl MinCycleTimeKind {
    fn keyword(self) -> &'static str {
        match self {
            MinCycleTimeKind::MinCycleTime => "MIN_CYCLE_TIME",
            MinCycleTimeKind::CoreLoadMax => "CORE_LOAD_MAX",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEventMinCycleTime {
    pub kind: MinCycleTimeKind,
    pub time_cycle: u8,
    pub time_unit: XcpTimestampResolution,
    pub children: Vec<XcpNode>,
}

impl XcpEventMinCycleTime {
    fn parse(block: &Block, kind: MinCycleTimeKind) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpEventMinCycleTime {
            kind,
            time_cycle: u8_val(&mut cur)?,
            time_unit: XcpTimestampResolution::from_u8(u8_val(&mut cur)?),
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.time_cycle.to_string());
        w.value_line(None, &self.time_unit.as_u8().to_string());
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEventOdtEntrySizeFactorTable {
    pub size: u32,
    pub size_factor: f32,
    pub children: Vec<XcpNode>,
}

impl XcpEventOdtEntrySizeFactorTable {
    pub const KEYWORD: &'static str = "ODT_ENTRY_SIZE_FACTOR_TABLE";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpEventOdtEntrySizeFactorTable {
            size: cur.uint()?,
            size_factor: f32_val(&mut cur)?,
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.size.to_string());
        w.value_line(None, &fmt_f32(self.size_factor));
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpFraming {
    pub sync: u8,
    pub esc: u8,
    pub children: Vec<XcpNode>,
}

impl XcpFraming {
    pub const KEYWORD: &'static str = "FRAMING";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpFraming {
            sync: cur.uint::<u64>()? as u8,
            esc: cur.uint::<u64>()? as u8,
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.sync));
        w.value_line(None, &to_hex(self.esc));
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpMediaFields {
    pub version: u16,
    pub transport_layer_instance: String,
    pub max_bus_load: u32,
    pub optional_tl_cmds: Vec<String>,
}

fn take_prefixed(cur: &mut ParamCursor, prefix: &str) -> Result<Option<String>> {
    let matched = match cur.peek() {
        Some(t) if !t.quoted => t
            .text
            .get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix)),
        _ => false,
    };
    if matched {
        let t = cur.next_token()?;
        Ok(Some(t.text[prefix.len()..].to_string()))
    } else {
        Ok(None)
    }
}

impl XcpMediaFields {
    fn take_common(&mut self, cur: &mut ParamCursor) -> Result<bool> {
        let t = match cur.peek() {
            Some(t) => t,
            None => return Ok(false),
        };
        if !t.quoted
            && (t.text.eq_ignore_ascii_case("OPTIONAL_TL_SUBCMD")
                || t.text.eq_ignore_ascii_case("OPTIONAL_CMD"))
        {
            cur.next_token()?;
            self.optional_tl_cmds.push(raw_text(cur)?);
            Ok(true)
        } else if !t.quoted && t.text.eq_ignore_ascii_case("MAX_BUS_LOAD") {
            cur.next_token()?;
            self.max_bus_load = u32_val(cur)?;
            Ok(true)
        } else if !t.quoted && t.text.eq_ignore_ascii_case("TRANSPORT_LAYER_INSTANCE") {
            cur.next_token()?;
            self.transport_layer_instance = raw_text(cur)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn write_common(&self, w: &mut Writer) {
        if !self.transport_layer_instance.is_empty() {
            w.tag_value(
                Some("TRANSPORT_LAYER_INSTANCE"),
                Some(&escape_str(&self.transport_layer_instance)),
                true,
            );
        }
        for cmd in &self.optional_tl_cmds {
            w.value_line(Some("OPTIONAL_TL_SUBCMD"), cmd);
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct XcpOdtEntry {
    pub odt_entry_no: u8,
    pub address: u32,
    pub address_extension: u8,
    pub size: u8,
    pub bit_offset: u8,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpOdt {
    pub odt_no: u8,
    pub odt_entries: Vec<XcpOdtEntry>,
    pub children: Vec<XcpNode>,
}

impl XcpOdt {
    pub const KEYWORD: &'static str = "ODT";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpOdt::default();
        let mut cur = ParamCursor::new(block);
        node.odt_no = cur.uint::<u64>()? as u8;
        while cur.remaining() >= 6 {
            if cur.take_if("ODT_ENTRY") {
                node.odt_entries.push(XcpOdtEntry {
                    odt_entry_no: cur.uint::<u64>()? as u8,
                    address: u32_val(&mut cur)?,
                    address_extension: cur.uint::<u64>()? as u8,
                    size: cur.uint::<u64>()? as u8,
                    bit_offset: cur.uint::<u64>()? as u8,
                });
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.odt_no.to_string());
        for e in &self.odt_entries {
            w.value_line(
                Some("ODT_ENTRY"),
                &format!(
                    "{} {} {} {} {}",
                    e.odt_entry_no,
                    to_hex(e.address),
                    to_hex(e.address_extension),
                    to_hex(e.size),
                    to_hex(e.bit_offset)
                ),
            );
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpOnCan {
    pub media: XcpMediaFields,
    pub can_id_resp: u32,
    pub can_id_cmd: u32,
    /// `CAN_ID_MASTER_INCREMENTAL`.
    pub can_id_master_incremental: bool,
    pub can_id_broadcast: u32,
    pub can_id_get_daq_clock_multicast: u32,
    pub baudrate: u32,
    pub sample_point: u8,
    pub sample_rate: XcpDaqListCanSampleRate,
    pub btl_cycles: u8,
    pub sjw: u8,
    /// `MAX_DLC_REQUIRED`.
    pub max_dlc_required: bool,
    pub sync_edge: XcpSyncEdge,
    /// `MEASUREMENT_SPLIT_ALLOWED`.
    pub measurement_split_allowed: bool,
    pub children: Vec<XcpNode>,
}

impl Default for XcpOnCan {
    fn default() -> Self {
        XcpOnCan {
            media: XcpMediaFields::default(),
            can_id_resp: u32::MAX,
            can_id_cmd: u32::MAX,
            can_id_master_incremental: false,
            can_id_broadcast: u32::MAX,
            can_id_get_daq_clock_multicast: u32::MAX,
            baudrate: 0,
            sample_point: 0,
            sample_rate: XcpDaqListCanSampleRate::NotSet,
            btl_cycles: 0,
            sjw: 0,
            max_dlc_required: false,
            sync_edge: XcpSyncEdge::NotSet,
            measurement_split_allowed: false,
            children: Vec::new(),
        }
    }
}

impl XcpOnCan {
    pub const KEYWORD: &'static str = "XCP_ON_CAN";

    pub fn request_ids_by_broadcast(&self) -> bool {
        (self.can_id_cmd == u32::MAX || self.can_id_resp == u32::MAX)
            && self.can_id_broadcast != u32::MAX
    }

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpOnCan::default();
        let mut cur = ParamCursor::new(block);
        node.media.version = u16_val(&mut cur)?;
        while !cur.is_empty() {
            if cur.take_if("CAN_ID_MASTER_INCREMENTAL") {
                node.can_id_master_incremental = true;
            } else if cur.take_if("CAN_ID_BROADCAST") {
                node.can_id_broadcast = u32_val(&mut cur)?;
            } else if cur.take_if("CAN_ID_GET_DAQ_CLOCK_MULTICAST") {
                node.can_id_get_daq_clock_multicast = u32_val(&mut cur)?;
            } else if cur.take_if("CAN_ID_MASTER") {
                node.can_id_cmd = u32_val(&mut cur)?;
            } else if cur.take_if("CAN_ID_SLAVE") {
                node.can_id_resp = u32_val(&mut cur)?;
            } else if cur.take_if("BAUDRATE") {
                node.baudrate = u32_val(&mut cur)?;
            } else if cur.take_if("SAMPLE_POINT") {
                node.sample_point = u8_val(&mut cur)?;
            } else if cur.take_if("SAMPLE_RATE") {
                node.sample_rate = kw_enum(&cur.next_token()?.text);
            } else if cur.take_if("BTL_CYCLES") {
                node.btl_cycles = u8_val(&mut cur)?;
            } else if cur.take_if("SJW") {
                node.sjw = u8_val(&mut cur)?;
            } else if cur.take_if("MAX_DLC_REQUIRED") {
                node.max_dlc_required = true;
            } else if cur.take_if("SYNC_EDGE") {
                node.sync_edge = kw_enum(&cur.next_token()?.text);
            } else if cur.take_if("MEASUREMENT_SPLIT_ALLOWED") {
                node.measurement_split_allowed = true;
            } else if node.media.take_common(&mut cur)? {
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.media.version));
        if self.can_id_master_incremental {
            w.value_line(None, "CAN_ID_MASTER_INCREMENTAL");
        }
        if self.can_id_broadcast != u32::MAX {
            w.value_line(Some("CAN_ID_BROADCAST"), &to_hex(self.can_id_broadcast));
        }
        if self.can_id_get_daq_clock_multicast != u32::MAX {
            w.value_line(
                Some("CAN_ID_GET_DAQ_CLOCK_MULTICAST"),
                &to_hex(self.can_id_get_daq_clock_multicast),
            );
        }
        if self.can_id_cmd != u32::MAX {
            w.value_line(Some("CAN_ID_MASTER"), &to_hex(self.can_id_cmd));
        }
        if self.can_id_resp != u32::MAX {
            w.value_line(Some("CAN_ID_SLAVE"), &to_hex(self.can_id_resp));
        }
        if self.baudrate != 0 {
            w.value_line(Some("BAUDRATE"), &self.baudrate.to_string());
        }
        if self.sample_point != 0 {
            w.value_line(Some("SAMPLE_POINT"), &self.sample_point.to_string());
        }
        if self.sample_rate != XcpDaqListCanSampleRate::NotSet {
            w.value_line(Some("SAMPLE_RATE"), kw_or_notset(self.sample_rate));
        }
        if self.btl_cycles != 0 {
            w.value_line(Some("BTL_CYCLES"), &self.btl_cycles.to_string());
        }
        if self.sjw != 0 {
            w.value_line(Some("SJW"), &to_hex(self.sjw));
        }
        if self.max_dlc_required {
            w.value_line(None, "MAX_DLC_REQUIRED");
        }
        if self.sync_edge != XcpSyncEdge::NotSet {
            w.value_line(Some("SYNC_EDGE"), kw_or_notset(self.sync_edge));
        }
        if self.measurement_split_allowed {
            w.value_line(None, "MEASUREMENT_SPLIT_ALLOWED");
        }
        if self.media.max_bus_load != 0 {
            w.value_line(Some("MAX_BUS_LOAD"), &to_hex(self.media.max_bus_load));
        }
        self.media.write_common(w);
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpEthernetFields {
    pub port: u16,
    pub address: String,
    pub host_name: String,
    pub ipv6: String,
    pub max_bit_rate: u32,
    pub packet_alignment: XcpPacketAligment,
}

impl XcpEthernetFields {
    fn parse(cur: &mut ParamCursor, media: &mut XcpMediaFields) -> Result<Self> {
        let mut f = XcpEthernetFields {
            port: cur.uint()?,
            ..XcpEthernetFields::default()
        };
        while !cur.is_empty() {
            if cur.take_if("ADDRESS") {
                f.address = raw_text(cur)?;
            } else if cur.take_if("HOST_NAME") {
                f.host_name = raw_text(cur)?;
            } else if cur.take_if("IPV6") {
                f.ipv6 = raw_text(cur)?;
            } else if cur.take_if("MAX_BIT_RATE") {
                f.max_bit_rate = u32_val(cur)?;
            } else if let Some(v) = take_prefixed(cur, "PACKET_ALIGNMENT")? {
                f.packet_alignment = kw_enum(&v);
            } else if media.take_common(cur)? {
            } else {
                cur.next_token()?;
            }
        }
        Ok(f)
    }

    fn write_body(&self, w: &mut Writer) {
        w.value_line(None, &self.port.to_string());
        if !self.address.is_empty() {
            w.tag_value(Some("ADDRESS"), Some(&escape_str(&self.address)), true);
        }
        if !self.host_name.is_empty() {
            w.tag_value(Some("HOST_NAME"), Some(&escape_str(&self.host_name)), true);
        }
        if !self.ipv6.is_empty() {
            w.tag_value(Some("IPV6"), Some(&escape_str(&self.ipv6)), true);
        }
        if self.packet_alignment != XcpPacketAligment::NotSet {
            w.value_line(
                None,
                &format!("PACKET_ALIGNMENT{}", kw_or_notset(self.packet_alignment)),
            );
        }
    }
}

macro_rules! ethernet_node {
    ($(#[$meta:meta])* $name:ident, $kw:literal) => {
        $(#[$meta])*
        #[derive(Debug, Default, Clone, PartialEq)]
        pub struct $name {
            pub media: XcpMediaFields,
            pub ethernet: XcpEthernetFields,
            pub children: Vec<XcpNode>,
        }

        impl $name {
            pub const KEYWORD: &'static str = $kw;

            fn parse(block: &Block) -> Result<Self> {
                let mut cur = ParamCursor::new(block);
                let mut media = XcpMediaFields::default();
                media.version = u16_val(&mut cur)?;
                let ethernet = XcpEthernetFields::parse(&mut cur, &mut media)?;
                Ok($name {
                    media,
                    ethernet,
                    children: Vec::new(),
                })
            }

            fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
                w.value_line(None, &to_hex(self.media.version));
                self.ethernet.write_body(w);
                self.media.write_common(w);
                write_children(&self.children, w, plus)
            }
        }
    };
}

ethernet_node! {
    XcpOnTcpIp, "XCP_ON_TCP_IP"
}

ethernet_node! {
    XcpOnUdpIp, "XCP_ON_UDP_IP"
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct XcpAsyncFullDuplexMode {
    pub parity: ParityType,
    pub stop_bits: StopBitsType,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpOnSxi {
    pub media: XcpMediaFields,
    pub baudrate: u32,
    pub duplex_mode: Option<XcpAsyncFullDuplexMode>,
    pub sync_full_duplex_mode: SyncModeSize,
    pub sync_master_slave_mode: SyncModeSize,
    pub header_len: XcpHeaderLen,
    pub checksum: ChecksumSxi,
    pub children: Vec<XcpNode>,
}

impl XcpOnSxi {
    pub const KEYWORD: &'static str = "XCP_ON_SxI";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpOnSxi::default();
        let mut cur = ParamCursor::new(block);
        node.media.version = u16_val(&mut cur)?;
        node.baudrate = u32_val(&mut cur)?;
        while !cur.is_empty() {
            if cur.take_if("ASYNCH_FULL_DUPLEX_MODE") {
                let parity = kw_enum(cur.next_token()?.text.get(7..).unwrap_or(""));
                let stop_bits = kw_enum(&cur.next_token()?.text);
                node.duplex_mode = Some(XcpAsyncFullDuplexMode { parity, stop_bits });
            } else if let Some(v) = take_prefixed(&mut cur, "SYNCH_FULL_DUPLEX_MODE_")? {
                node.sync_full_duplex_mode = kw_enum(&v);
            } else if let Some(v) = take_prefixed(&mut cur, "SYNCH_MASTER_SLAVE_MODE_")? {
                node.sync_master_slave_mode = kw_enum(&v);
            } else if let Some(v) = take_prefixed(&mut cur, "HEADER_LEN_")? {
                node.header_len = kw_enum(&v);
            } else if node.media.take_common(&mut cur)? {
            } else {
                let t = cur.next_token()?;
                node.checksum =
                    ChecksumSxi::from_keyword(&t.text).unwrap_or(ChecksumSxi::NO_CHECKSUM);
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.media.version));
        w.value_line(None, &self.baudrate.to_string());
        if let Some(dm) = &self.duplex_mode {
            w.value_line(
                None,
                &format!(
                    "ASYNCH_FULL_DUPLEX_MODE PARITY_{} {}",
                    kw_or_notset(dm.parity),
                    kw_or_notset(dm.stop_bits)
                ),
            );
        }
        if self.sync_full_duplex_mode != SyncModeSize::NotSet {
            w.value_line(
                None,
                &format!(
                    "SYNCH_FULL_DUPLEX_MODE_{}",
                    kw_or_notset(self.sync_full_duplex_mode)
                ),
            );
        }
        if self.sync_master_slave_mode != SyncModeSize::NotSet {
            w.value_line(
                None,
                &format!(
                    "SYNCH_MASTER_SLAVE_MODE_{}",
                    kw_or_notset(self.sync_master_slave_mode)
                ),
            );
        }
        if self.header_len != XcpHeaderLen::NotSet {
            w.value_line(
                None,
                &format!("HEADER_LEN_{}", kw_or_notset(self.header_len)),
            );
        }
        if self.checksum != ChecksumSxi::NotSet {
            w.value_line(None, kw_or_notset(self.checksum));
        }
        self.media.write_common(w);
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpOnUsb {
    pub media: XcpMediaFields,
    pub vendor_id: u16,
    pub product_id: u16,
    pub number_of_if: u8,
    pub header_len: XcpHeaderLen,
    pub alternate_setting_no: u8,
    pub interface_descriptor: String,
    pub children: Vec<XcpNode>,
}

impl Default for XcpOnUsb {
    fn default() -> Self {
        XcpOnUsb {
            media: XcpMediaFields::default(),
            vendor_id: 0,
            product_id: 0,
            number_of_if: 0,
            header_len: XcpHeaderLen::NotSet,
            alternate_setting_no: u8::MAX,
            interface_descriptor: String::new(),
            children: Vec::new(),
        }
    }
}

impl XcpOnUsb {
    pub const KEYWORD: &'static str = "XCP_ON_USB";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpOnUsb::default();
        let mut cur = ParamCursor::new(block);
        node.media.version = u16_val(&mut cur)?;
        node.vendor_id = u16_val(&mut cur)?;
        node.product_id = u16_val(&mut cur)?;
        node.number_of_if = u8_val(&mut cur)?;
        node.header_len = kw_enum(cur.next_token()?.text.get(11..).unwrap_or(""));
        while !cur.is_empty() {
            if cur.take_if("ALTERNATE_SETTING_NO") {
                node.alternate_setting_no = u8_val(&mut cur)?;
            } else if cur.take_if("INTERFACE_STRING_DESCRIPTOR") {
                node.interface_descriptor = raw_text(&mut cur)?;
            } else if node.media.take_common(&mut cur)? {
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.media.version));
        w.value_line(None, &to_hex(self.vendor_id));
        w.value_line(None, &to_hex(self.product_id));
        w.value_line(None, &self.number_of_if.to_string());
        w.value_line(
            None,
            &format!("HEADER_LEN_{}", kw_or_notset(self.header_len)),
        );
        if self.alternate_setting_no != u8::MAX {
            w.value_line(
                Some("ALTERNATE_SETTING_NO"),
                &self.alternate_setting_no.to_string(),
            );
        }
        if !self.interface_descriptor.is_empty() {
            w.value_line(
                Some("INTERFACE_STRING_DESCRIPTOR"),
                &format!("\"{}\"", escape_str(&self.interface_descriptor)),
            );
        }
        self.media.write_common(w);
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpPag {
    pub max_segments: u8,
    pub properties: PagProperties,
    pub children: Vec<XcpNode>,
}

impl XcpPag {
    pub const KEYWORD: &'static str = "PAG";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpPag::default();
        let mut cur = ParamCursor::new(block);
        node.max_segments = cur.uint::<u64>()? as u8;
        while !cur.is_empty() {
            if cur.take_if("FREEZE_SUPPORTED") {
                node.properties.0 |= PagProperties::FREEZE_SUPPORTED;
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.max_segments));
        if self.properties.contains(PagProperties::FREEZE_SUPPORTED) {
            w.value_line(None, "FREEZE_SUPPORTED");
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpPage {
    pub page_no: u8,
    pub access_ecu: XcpEcuAccess,
    pub access_read: XcpReadWriteAccess,
    pub access_write: XcpReadWriteAccess,
    pub children: Vec<XcpNode>,
}

impl XcpPage {
    pub const KEYWORD: &'static str = "PAGE";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpPage {
            page_no: u8_val(&mut cur)?,
            access_ecu: kw_enum(cur.next_token()?.text.get(11..).unwrap_or("")),
            access_read: kw_enum(cur.next_token()?.text.get(16..).unwrap_or("")),
            access_write: kw_enum(cur.next_token()?.text.get(17..).unwrap_or("")),
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.page_no));
        w.value_line(
            None,
            &format!("ECU_ACCESS_{}", kw_or_notset(self.access_ecu)),
        );
        w.value_line(
            None,
            &format!("XCP_READ_ACCESS_{}", kw_or_notset(self.access_read)),
        );
        w.value_line(
            None,
            &format!("XCP_WRITE_ACCESS_{}", kw_or_notset(self.access_write)),
        );
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpPgm {
    /// `PGM_MODE_ABSOLUTE` / `PGM_MODE_FUNCTIONAL` / `PGM_MODE_ABSOLUTE_AND_FUNCTIONAL`.
    pub mode: XcpPgmMode,
    pub max_sectors: u8,
    pub max_cto: u8,
    pub comm_modes_supported: XcpCommModes,
    pub children: Vec<XcpNode>,
}

impl XcpPgm {
    pub const KEYWORD: &'static str = "PGM";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpPgm::default();
        let mut cur = ParamCursor::new(block);
        if cur.take_if("PGM_MODE_ABSOLUTE") {
            node.mode = XcpPgmMode(XcpPgmMode::ABSOLUTE);
        } else if cur.take_if("PGM_MODE_FUNCTIONAL") {
            node.mode = XcpPgmMode(XcpPgmMode::FUNCTIONAL);
        } else if cur.take_if("PGM_MODE_ABSOLUTE_AND_FUNCTIONAL") {
            node.mode = XcpPgmMode(XcpPgmMode::ABSOLUTE | XcpPgmMode::FUNCTIONAL);
        } else {
            cur.next_token()?;
        }
        node.max_sectors = u8_val(&mut cur)?;
        node.max_cto = u8_val(&mut cur)?;
        if cur.remaining() >= 2 && cur.take_if("COMMUNICATION_MODE_SUPPORTED") {
            node.comm_modes_supported = XcpCommModes::parse(&mut cur)?;
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        if self.mode.contains(XcpPgmMode::ABSOLUTE) && self.mode.contains(XcpPgmMode::FUNCTIONAL) {
            w.value_line(None, "PGM_MODE_ABSOLUTE_AND_FUNCTIONAL");
        } else if self.mode.contains(XcpPgmMode::ABSOLUTE) {
            w.value_line(None, "PGM_MODE_ABSOLUTE");
        } else if self.mode.contains(XcpPgmMode::FUNCTIONAL) {
            w.value_line(None, "PGM_MODE_FUNCTIONAL");
        }
        w.value_line(
            None,
            &format!("{} {}", to_hex(self.max_sectors), to_hex(self.max_cto)),
        );
        self.comm_modes_supported.write(w);
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpProtocolLayer {
    pub version: u16,
    pub timings: [u16; 7],
    pub max_cto: u8,
    pub max_dto: u16,
    pub max_dto_stim: u16,
    pub comm_mode_basic: CommModeBasic,
    pub optional_cmds: Vec<String>,
    pub comm_modes_supported: XcpCommModes,
    pub seed_and_key_external_function: String,
    pub children: Vec<XcpNode>,
}

impl Default for XcpProtocolLayer {
    fn default() -> Self {
        XcpProtocolLayer {
            version: 0,
            timings: [0; 7],
            max_cto: u8::MAX,
            max_dto: u16::MAX,
            max_dto_stim: u16::MAX,
            comm_mode_basic: CommModeBasic::default(),
            optional_cmds: Vec::new(),
            comm_modes_supported: XcpCommModes::default(),
            seed_and_key_external_function: String::new(),
            children: Vec::new(),
        }
    }
}

impl XcpProtocolLayer {
    pub const KEYWORD: &'static str = "PROTOCOL_LAYER";

    pub fn is_msb_first(&self) -> bool {
        self.comm_mode_basic.contains(CommModeBasic::BIG_ENDIAN)
    }

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpProtocolLayer::default();
        let mut cur = ParamCursor::new(block);
        node.version = cur.uint()?;
        for t in &mut node.timings {
            *t = cur.uint()?;
        }
        node.max_cto = u8_val(&mut cur)?;
        node.max_dto = u16_val(&mut cur)?;
        while cur.remaining() >= 2 {
            if cur.take_if("ADDRESS_GRANULARITY_BYTE") {
            } else if cur.take_if("ADDRESS_GRANULARITY_WORD") {
                node.comm_mode_basic.0 |= CommModeBasic::ADDRESS_GRANULARITY_WORD;
            } else if cur.take_if("ADDRESS_GRANULARITY_DWORD") {
                node.comm_mode_basic.0 |= CommModeBasic::ADDRESS_GRANULARITY_DWORD;
            } else if cur.take_if("OPTIONAL_CMD") {
                node.optional_cmds.push(raw_text(&mut cur)?);
            } else if cur.take_if("MAX_DTO_STIM") {
                node.max_dto_stim = u16_val(&mut cur)?;
            } else if cur.take_if("BYTE_ORDER_MSB_FIRST") {
                node.comm_mode_basic.0 |= CommModeBasic::BIG_ENDIAN;
            } else if cur.take_if("BYTE_ORDER_MSB_LAST") {
                node.comm_mode_basic.0 &= !CommModeBasic::BIG_ENDIAN;
            } else if cur.take_if("SEED_AND_KEY_EXTERNAL_FUNCTION") {
                node.seed_and_key_external_function = raw_text(&mut cur)?;
            } else if cur.take_if("COMMUNICATION_MODE_SUPPORTED") {
                node.comm_modes_supported = XcpCommModes::parse(&mut cur)?;
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.version));
        let timings = self
            .timings
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        w.value_line(None, &timings);
        w.value_line(None, &to_hex(self.max_cto));
        w.value_line(None, &to_hex(self.max_dto));
        if self.is_msb_first() {
            w.value_line(None, "BYTE_ORDER_MSB_FIRST");
        } else {
            w.value_line(None, "BYTE_ORDER_MSB_LAST");
        }
        if self
            .comm_mode_basic
            .contains(CommModeBasic::ADDRESS_GRANULARITY_WORD)
        {
            w.value_line(None, "ADDRESS_GRANULARITY_WORD");
        } else if self
            .comm_mode_basic
            .contains(CommModeBasic::ADDRESS_GRANULARITY_DWORD)
        {
            w.value_line(None, "ADDRESS_GRANULARITY_DWORD");
        } else {
            w.value_line(None, "ADDRESS_GRANULARITY_BYTE");
        }
        if !self.seed_and_key_external_function.is_empty() {
            w.tag_value(
                Some("SEED_AND_KEY_EXTERNAL_FUNCTION"),
                Some(&escape_str(&self.seed_and_key_external_function)),
                true,
            );
        }
        for cmd in &self.optional_cmds {
            w.value_line(Some("OPTIONAL_CMD"), cmd);
        }
        self.comm_modes_supported.write(w);
        if self.max_dto_stim != u16::MAX {
            w.value_line(Some("MAX_DTO_STIM"), &to_hex(self.max_dto_stim));
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpSector {
    pub name_long: String,
    pub number: u8,
    pub address: u32,
    pub length: u32,
    pub clear_seq_no: u8,
    pub pgm_seq_no: u8,
    pub pgm_method: u8,
    pub children: Vec<XcpNode>,
}

impl XcpSector {
    pub const KEYWORD: &'static str = "SECTOR";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpSector {
            name_long: raw_text(&mut cur)?,
            number: u8_val(&mut cur)?,
            address: cur.uint()?,
            length: cur.uint()?,
            clear_seq_no: u8_val(&mut cur)?,
            pgm_seq_no: u8_val(&mut cur)?,
            pgm_method: u8_val(&mut cur)?,
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.tag_value(None, Some(&escape_str(&self.name_long)), true);
        w.value_line(
            None,
            &format!(
                "{} {} {}",
                to_hex(self.number),
                to_hex(self.address),
                to_hex(self.length)
            ),
        );
        w.value_line(
            None,
            &format!(
                "{} {} {}",
                to_hex(self.clear_seq_no),
                to_hex(self.pgm_seq_no),
                to_hex(self.pgm_method)
            ),
        );
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpSegment {
    pub segment_no: u8,
    pub no_of_pages: u8,
    pub address_extension: u8,
    pub compression_method: u8,
    pub encryption_method: u8,
    pub pgm_verify: u32,
    pub default_page_number: u8,
    pub children: Vec<XcpNode>,
}

impl Default for XcpSegment {
    fn default() -> Self {
        XcpSegment {
            segment_no: 0,
            no_of_pages: 0,
            address_extension: 0,
            compression_method: 0,
            encryption_method: 0,
            pgm_verify: 0,
            default_page_number: u8::MAX,
            children: Vec::new(),
        }
    }
}

impl XcpSegment {
    pub const KEYWORD: &'static str = "SEGMENT";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpSegment::default();
        let mut cur = ParamCursor::new(block);
        node.segment_no = u8_val(&mut cur)?;
        node.no_of_pages = u8_val(&mut cur)?;
        node.address_extension = u8_val(&mut cur)?;
        node.compression_method = u8_val(&mut cur)?;
        node.encryption_method = u8_val(&mut cur)?;
        while !cur.is_empty() {
            if cur.take_if("PGM_VERIFY") {
                node.pgm_verify = u32_val(&mut cur)?;
            } else if cur.take_if("DEFAULT_PAGE_NUMBER") {
                node.default_page_number = cur.uint::<u64>()? as u8;
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.segment_no));
        w.value_line(None, &to_hex(self.no_of_pages));
        w.value_line(None, &to_hex(self.address_extension));
        w.value_line(None, &to_hex(self.compression_method));
        w.value_line(None, &to_hex(self.encryption_method));
        if self.pgm_verify != 0 {
            w.value_line(Some("PGM_VERIFY"), &to_hex(self.pgm_verify));
        }
        if self.default_page_number != u8::MAX {
            w.value_line(
                Some("DEFAULT_PAGE_NUMBER"),
                &self.default_page_number.to_string(),
            );
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XcpStim {
    pub odt_entry_size: XcpOdtEntrySize,
    pub max_odt_entry_size: u8,
    /// `BIT_STIM_SUPPORTED`.
    pub bit_stim_supported: bool,
    pub min_st_stim: u8,
    pub children: Vec<XcpNode>,
}

impl Default for XcpStim {
    fn default() -> Self {
        XcpStim {
            odt_entry_size: XcpOdtEntrySize::BYTE,
            max_odt_entry_size: u8::MAX,
            bit_stim_supported: false,
            min_st_stim: 0,
            children: Vec::new(),
        }
    }
}

impl XcpStim {
    pub const KEYWORD: &'static str = "STIM";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpStim::default();
        let mut cur = ParamCursor::new(block);
        node.odt_entry_size = kw_enum(after_last_underscore(&cur.next_token()?.text));
        node.max_odt_entry_size = u8_val(&mut cur)?;
        while !cur.is_empty() {
            if cur.take_if("BIT_STIM_SUPPORTED") {
                node.bit_stim_supported = true;
            } else if cur.take_if("MIN_ST_STIM") {
                node.min_st_stim = u8_val(&mut cur)?;
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(
            None,
            &format!(
                "GRANULARITY_ODT_ENTRY_SIZE_STIM_{}",
                kw_or_notset(self.odt_entry_size)
            ),
        );
        w.value_line(None, &to_hex(self.max_odt_entry_size));
        if self.bit_stim_supported {
            w.value_line(None, "BIT_STIM_SUPPORTED");
        }
        if self.min_st_stim > 0 {
            w.value_line(Some("MIN_ST_STIM"), &self.min_st_stim.to_string());
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpTimeCorrelation {
    pub timestamps_relate_to: XcpTsRelation,
    pub children: Vec<XcpNode>,
}

impl XcpTimeCorrelation {
    pub const KEYWORD: &'static str = "TIME_CORRELATION";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpTimeCorrelation::default();
        let mut cur = ParamCursor::new(block);
        while !cur.is_empty() {
            if cur.take_if("DAQ_TIMESTAMPS_RELATE_TO") {
                node.timestamps_relate_to = kw_enum(&cur.next_token()?.text);
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(
            Some("DAQ_TIMESTAMPS_RELATE_TO"),
            kw_or_notset(self.timestamps_relate_to),
        );
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpTimestampCharacterization {
    pub timestamp_ticks: u32,
    pub resolution: XcpTimestampResolution,
    pub size: XcpNativeTimestampSize,
    pub children: Vec<XcpNode>,
}

impl XcpTimestampCharacterization {
    pub const KEYWORD: &'static str = "TIMESTAMP_CHARACTERIZATION";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(XcpTimestampCharacterization {
            timestamp_ticks: u32_val(&mut cur)?,
            resolution: kw_enum(cur.next_token()?.text.get(4..).unwrap_or("")),
            size: kw_enum(cur.next_token()?.text.get(5..).unwrap_or("")),
            children: Vec::new(),
        })
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &self.timestamp_ticks.to_string());
        w.value_line(None, &format!("UNIT{}", kw_or_notset(self.resolution)));
        w.value_line(None, &format!("SIZE_{}", kw_or_notset(self.size)));
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpTimestampSupported {
    pub ticks: u16,
    pub size: XcpTimestampSize,
    pub resolution: XcpTimestampResolution,
    /// `TIMESTAMP_FIXED`.
    pub is_fixed: bool,
    pub children: Vec<XcpNode>,
}

impl XcpTimestampSupported {
    pub const KEYWORD: &'static str = "TIMESTAMP_SUPPORTED";

    fn parse(block: &Block) -> Result<Self> {
        let mut node = XcpTimestampSupported::default();
        let mut cur = ParamCursor::new(block);
        node.ticks = cur.uint()?;
        let t = cur.next_token()?;
        if !t.text.starts_with("NO_") {
            node.size = kw_enum(t.text.get(5..).unwrap_or(""));
        }
        node.resolution = kw_enum(cur.next_token()?.text.get(4..).unwrap_or(""));
        if !cur.is_empty() {
            node.is_fixed = cur.take_if("TIMESTAMP_FIXED");
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        w.value_line(None, &to_hex(self.ticks));
        if self.size != XcpTimestampSize::NotSet {
            w.value_line(None, &format!("SIZE_{}", kw_or_notset(self.size)));
        } else {
            w.value_line(None, "NO_TIME_STAMP");
        }
        w.value_line(None, &format!("UNIT{}", kw_or_notset(self.resolution)));
        if self.is_fixed {
            w.value_line(None, "TIMESTAMP_FIXED");
        }
        write_children(&self.children, w, plus)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct XcpGenericNode {
    pub keyword: String,
    pub children: Vec<XcpNode>,
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum XcpNode {
    /// [`XcpAddressMapping`].
    AddressMapping(XcpAddressMapping),
    /// [`XcpBufferReserve`].
    BufferReserve(XcpBufferReserve),
    /// [`XcpCanFd`].
    CanFd(XcpCanFd),
    /// [`XcpChecksum`].
    Checksum(XcpChecksum),
    /// [`XcpClock`].
    Clock(XcpClock),
    /// [`XcpCoreLoad`].
    CoreLoad(XcpCoreLoad),
    /// [`XcpDaq`].
    Daq(XcpDaq),
    /// [`XcpDaqEvent`].
    DaqEvent(XcpDaqEvent),
    /// [`XcpDaqList`].
    DaqList(XcpDaqList),
    /// [`XcpDaqListCanId`].
    DaqListCanId(XcpDaqListCanId),
    /// [`XcpDaqListUsbEndpoint`].
    DaqListUsbEndpoint(XcpDaqListUsbEndpoint),
    /// [`XcpDaqMemoryConsumption`].
    DaqMemoryConsumption(XcpDaqMemoryConsumption),
    /// [`XcpEcuStatesMemoryAccess`].
    EcuStatesMemoryAccess(XcpEcuStatesMemoryAccess),
    /// [`XcpEcuStatesState`].
    EcuStatesState(XcpEcuStatesState),
    /// [`XcpEvent`].
    Event(XcpEvent),
    /// [`XcpEventCanIdList`].
    EventCanIdList(XcpEventCanIdList),
    /// [`XcpEventCpuLoadConsumption`].
    EventCpuLoadConsumption(XcpEventCpuLoadConsumption),
    /// [`XcpEventCpuLoadConsumptionQueue`].
    EventCpuLoadConsumptionQueue(XcpEventCpuLoadConsumptionQueue),
    /// [`XcpEventDaqPackedMode`].
    EventDaqPackedMode(XcpEventDaqPackedMode),
    /// [`XcpEventList`].
    EventList(XcpEventList),
    /// [`XcpEventMinCycleTime`].
    EventMinCycleTime(XcpEventMinCycleTime),
    /// [`XcpEventOdtEntrySizeFactorTable`].
    EventOdtEntrySizeFactorTable(XcpEventOdtEntrySizeFactorTable),
    /// [`XcpFraming`].
    Framing(XcpFraming),
    /// [`XcpOdt`].
    Odt(XcpOdt),
    /// [`XcpOnCan`].
    OnCan(XcpOnCan),
    /// [`XcpOnTcpIp`].
    OnTcpIp(XcpOnTcpIp),
    /// [`XcpOnUdpIp`].
    OnUdpIp(XcpOnUdpIp),
    /// [`XcpOnSxi`].
    OnSxi(XcpOnSxi),
    /// [`XcpOnUsb`].
    OnUsb(XcpOnUsb),
    /// [`XcpOutEpCmdStim`].
    OutEpCmdStim(XcpOutEpCmdStim),
    /// [`XcpOutEpOnlyStim`].
    OutEpOnlyStim(XcpOutEpOnlyStim),
    /// [`XcpInEpOnlyDaq`].
    InEpOnlyDaq(XcpInEpOnlyDaq),
    /// [`XcpInEpOnlyEvServ`].
    InEpOnlyEvServ(XcpInEpOnlyEvServ),
    /// [`XcpInEpResErrDaqEvServ`].
    InEpResErrDaqEvServ(XcpInEpResErrDaqEvServ),
    /// [`XcpPag`].
    Pag(XcpPag),
    /// [`XcpPage`].
    Page(XcpPage),
    /// [`XcpPgm`].
    Pgm(XcpPgm),
    /// [`XcpProtocolLayer`].
    ProtocolLayer(XcpProtocolLayer),
    /// [`XcpSector`].
    Sector(XcpSector),
    /// [`XcpSegment`].
    Segment(XcpSegment),
    /// [`XcpStim`].
    Stim(XcpStim),
    /// [`XcpTimeCorrelation`].
    TimeCorrelation(XcpTimeCorrelation),
    /// [`XcpTimestampCharacterization`].
    TimestampCharacterization(XcpTimestampCharacterization),
    /// [`XcpTimestampSupported`].
    TimestampSupported(XcpTimestampSupported),
    Generic(XcpGenericNode),
    Unsupported(UnsupportedNode),
}

impl XcpNode {
    fn children_mut(&mut self) -> Option<&mut Vec<XcpNode>> {
        match self {
            XcpNode::AddressMapping(n) => Some(&mut n.children),
            XcpNode::BufferReserve(n) => Some(&mut n.children),
            XcpNode::CanFd(n) => Some(&mut n.children),
            XcpNode::Checksum(n) => Some(&mut n.children),
            XcpNode::Clock(n) => Some(&mut n.children),
            XcpNode::CoreLoad(n) => Some(&mut n.children),
            XcpNode::Daq(n) => Some(&mut n.children),
            XcpNode::DaqEvent(n) => Some(&mut n.children),
            XcpNode::DaqList(n) => Some(&mut n.children),
            XcpNode::DaqListCanId(n) => Some(&mut n.children),
            XcpNode::DaqListUsbEndpoint(n) => Some(&mut n.children),
            XcpNode::DaqMemoryConsumption(n) => Some(&mut n.children),
            XcpNode::EcuStatesMemoryAccess(n) => Some(&mut n.children),
            XcpNode::EcuStatesState(n) => Some(&mut n.children),
            XcpNode::Event(n) => Some(&mut n.children),
            XcpNode::EventCanIdList(n) => Some(&mut n.children),
            XcpNode::EventCpuLoadConsumption(n) => Some(&mut n.children),
            XcpNode::EventCpuLoadConsumptionQueue(n) => Some(&mut n.children),
            XcpNode::EventDaqPackedMode(n) => Some(&mut n.children),
            XcpNode::EventList(n) => Some(&mut n.children),
            XcpNode::EventMinCycleTime(n) => Some(&mut n.children),
            XcpNode::EventOdtEntrySizeFactorTable(n) => Some(&mut n.children),
            XcpNode::Framing(n) => Some(&mut n.children),
            XcpNode::Odt(n) => Some(&mut n.children),
            XcpNode::OnCan(n) => Some(&mut n.children),
            XcpNode::OnTcpIp(n) => Some(&mut n.children),
            XcpNode::OnUdpIp(n) => Some(&mut n.children),
            XcpNode::OnSxi(n) => Some(&mut n.children),
            XcpNode::OnUsb(n) => Some(&mut n.children),
            XcpNode::OutEpCmdStim(n) => Some(&mut n.children),
            XcpNode::OutEpOnlyStim(n) => Some(&mut n.children),
            XcpNode::InEpOnlyDaq(n) => Some(&mut n.children),
            XcpNode::InEpOnlyEvServ(n) => Some(&mut n.children),
            XcpNode::InEpResErrDaqEvServ(n) => Some(&mut n.children),
            XcpNode::Pag(n) => Some(&mut n.children),
            XcpNode::Page(n) => Some(&mut n.children),
            XcpNode::Pgm(n) => Some(&mut n.children),
            XcpNode::ProtocolLayer(n) => Some(&mut n.children),
            XcpNode::Sector(n) => Some(&mut n.children),
            XcpNode::Segment(n) => Some(&mut n.children),
            XcpNode::Stim(n) => Some(&mut n.children),
            XcpNode::TimeCorrelation(n) => Some(&mut n.children),
            XcpNode::TimestampCharacterization(n) => Some(&mut n.children),
            XcpNode::TimestampSupported(n) => Some(&mut n.children),
            XcpNode::Generic(n) => Some(&mut n.children),
            XcpNode::Unsupported(_) => None,
        }
    }

    pub fn keyword(&self) -> &str {
        match self {
            XcpNode::AddressMapping(_) => XcpAddressMapping::KEYWORD,
            XcpNode::BufferReserve(n) => n.kind.keyword(),
            XcpNode::CanFd(_) => XcpCanFd::KEYWORD,
            XcpNode::Checksum(_) => XcpChecksum::KEYWORD,
            XcpNode::Clock(_) => XcpClock::KEYWORD,
            XcpNode::CoreLoad(_) => XcpCoreLoad::KEYWORD,
            XcpNode::Daq(_) => XcpDaq::KEYWORD,
            XcpNode::DaqEvent(_) => XcpDaqEvent::KEYWORD,
            XcpNode::DaqList(_) => XcpDaqList::KEYWORD,
            XcpNode::DaqListCanId(_) => XcpDaqListCanId::KEYWORD,
            XcpNode::DaqListUsbEndpoint(_) => XcpDaqListUsbEndpoint::KEYWORD,
            XcpNode::DaqMemoryConsumption(_) => XcpDaqMemoryConsumption::KEYWORD,
            XcpNode::EcuStatesMemoryAccess(_) => XcpEcuStatesMemoryAccess::KEYWORD,
            XcpNode::EcuStatesState(_) => XcpEcuStatesState::KEYWORD,
            XcpNode::Event(_) => XcpEvent::KEYWORD,
            XcpNode::EventCanIdList(_) => XcpEventCanIdList::KEYWORD,
            XcpNode::EventCpuLoadConsumption(n) => n.kind.keyword(),
            XcpNode::EventCpuLoadConsumptionQueue(n) => n.kind.keyword(),
            XcpNode::EventDaqPackedMode(_) => XcpEventDaqPackedMode::KEYWORD,
            XcpNode::EventList(n) => n.kind.keyword(),
            XcpNode::EventMinCycleTime(n) => n.kind.keyword(),
            XcpNode::EventOdtEntrySizeFactorTable(_) => XcpEventOdtEntrySizeFactorTable::KEYWORD,
            XcpNode::Framing(_) => XcpFraming::KEYWORD,
            XcpNode::Odt(_) => XcpOdt::KEYWORD,
            XcpNode::OnCan(_) => XcpOnCan::KEYWORD,
            XcpNode::OnTcpIp(_) => XcpOnTcpIp::KEYWORD,
            XcpNode::OnUdpIp(_) => XcpOnUdpIp::KEYWORD,
            XcpNode::OnSxi(_) => XcpOnSxi::KEYWORD,
            XcpNode::OnUsb(_) => XcpOnUsb::KEYWORD,
            XcpNode::OutEpCmdStim(_) => XcpOutEpCmdStim::KEYWORD,
            XcpNode::OutEpOnlyStim(_) => XcpOutEpOnlyStim::KEYWORD,
            XcpNode::InEpOnlyDaq(_) => XcpInEpOnlyDaq::KEYWORD,
            XcpNode::InEpOnlyEvServ(_) => XcpInEpOnlyEvServ::KEYWORD,
            XcpNode::InEpResErrDaqEvServ(_) => XcpInEpResErrDaqEvServ::KEYWORD,
            XcpNode::Pag(_) => XcpPag::KEYWORD,
            XcpNode::Page(_) => XcpPage::KEYWORD,
            XcpNode::Pgm(_) => XcpPgm::KEYWORD,
            XcpNode::ProtocolLayer(_) => XcpProtocolLayer::KEYWORD,
            XcpNode::Sector(_) => XcpSector::KEYWORD,
            XcpNode::Segment(_) => XcpSegment::KEYWORD,
            XcpNode::Stim(_) => XcpStim::KEYWORD,
            XcpNode::TimeCorrelation(_) => XcpTimeCorrelation::KEYWORD,
            XcpNode::TimestampCharacterization(_) => XcpTimestampCharacterization::KEYWORD,
            XcpNode::TimestampSupported(_) => XcpTimestampSupported::KEYWORD,
            XcpNode::Generic(n) => &n.keyword,
            XcpNode::Unsupported(n) => &n.keyword,
        }
    }

    pub fn parse_block(block: &Block) -> Result<XcpNode> {
        let kw = block.keyword.as_str();
        let typed = (|| -> Result<Option<XcpNode>> {
            let node = match () {
                _ if kw.eq_ignore_ascii_case("ADDRESS_MAPPING") => {
                    XcpNode::AddressMapping(XcpAddressMapping::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("BUFFER_RESERVE") => XcpNode::BufferReserve(
                    XcpBufferReserve::parse(block, BufferReserveKind::BufferReserve)?,
                ),
                _ if kw.eq_ignore_ascii_case("BUFFER_RESERVE_EVENT") => XcpNode::BufferReserve(
                    XcpBufferReserve::parse(block, BufferReserveKind::BufferReserveEvent)?,
                ),
                _ if kw.eq_ignore_ascii_case("CAN_FD") => XcpNode::CanFd(XcpCanFd::parse(block)?),
                _ if kw.eq_ignore_ascii_case("CHECKSUM") => {
                    XcpNode::Checksum(XcpChecksum::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("CLOCK") => XcpNode::Clock(XcpClock::parse(block)?),
                _ if kw.eq_ignore_ascii_case("CORE_LOAD_EP") => {
                    XcpNode::CoreLoad(XcpCoreLoad::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("DAQ") => XcpNode::Daq(XcpDaq::parse(block)?),
                _ if kw.eq_ignore_ascii_case("DAQ_EVENT") => {
                    XcpNode::DaqEvent(XcpDaqEvent::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("DAQ_LIST") => {
                    XcpNode::DaqList(XcpDaqList::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("DAQ_LIST_CAN_ID") => {
                    XcpNode::DaqListCanId(XcpDaqListCanId::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("DAQ_LIST_USB_ENDPOINT") => {
                    XcpNode::DaqListUsbEndpoint(XcpDaqListUsbEndpoint::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("DAQ_MEMORY_CONSUMPTION") => {
                    XcpNode::DaqMemoryConsumption(XcpDaqMemoryConsumption::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("MEMORY_ACCESS") => {
                    XcpNode::EcuStatesMemoryAccess(XcpEcuStatesMemoryAccess::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("STATE") => {
                    XcpNode::EcuStatesState(XcpEcuStatesState::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("EVENT") => XcpNode::Event(XcpEvent::parse(block)?),
                _ if kw.eq_ignore_ascii_case("EVENT_CAN_ID_LIST") => {
                    XcpNode::EventCanIdList(XcpEventCanIdList::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("CPU_LOAD_CONSUMPTION_DAQ") => {
                    XcpNode::EventCpuLoadConsumption(XcpEventCpuLoadConsumption::parse(
                        block,
                        CpuLoadConsumptionKind::Daq,
                    )?)
                }
                _ if kw.eq_ignore_ascii_case("CPU_LOAD_CONSUMPTION_STIM") => {
                    XcpNode::EventCpuLoadConsumption(XcpEventCpuLoadConsumption::parse(
                        block,
                        CpuLoadConsumptionKind::Stim,
                    )?)
                }
                _ if kw.eq_ignore_ascii_case("CPU_LOAD_CONSUMPTION_QUEUE") => {
                    XcpNode::EventCpuLoadConsumptionQueue(XcpEventCpuLoadConsumptionQueue::parse(
                        block,
                        CpuLoadConsumptionQueueKind::Queue,
                    )?)
                }
                _ if kw.eq_ignore_ascii_case("CPU_LOAD_CONSUMPTION_QUEUE_STIM") => {
                    XcpNode::EventCpuLoadConsumptionQueue(XcpEventCpuLoadConsumptionQueue::parse(
                        block,
                        CpuLoadConsumptionQueueKind::QueueStim,
                    )?)
                }
                _ if kw.eq_ignore_ascii_case("DAQ_PACKED_MODE") => {
                    XcpNode::EventDaqPackedMode(XcpEventDaqPackedMode::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("AVAILABLE_EVENT_LIST") => {
                    XcpNode::EventList(XcpEventList::parse(block, EventListKind::Available)?)
                }
                _ if kw.eq_ignore_ascii_case("DEFAULT_EVENT_LIST") => {
                    XcpNode::EventList(XcpEventList::parse(block, EventListKind::Default)?)
                }
                _ if kw.eq_ignore_ascii_case("CONSISTENCY_EVENT_LIST") => {
                    XcpNode::EventList(XcpEventList::parse(block, EventListKind::Consistency)?)
                }
                _ if kw.eq_ignore_ascii_case("MIN_CYCLE_TIME") => XcpNode::EventMinCycleTime(
                    XcpEventMinCycleTime::parse(block, MinCycleTimeKind::MinCycleTime)?,
                ),
                _ if kw.eq_ignore_ascii_case("CORE_LOAD_MAX") => XcpNode::EventMinCycleTime(
                    XcpEventMinCycleTime::parse(block, MinCycleTimeKind::CoreLoadMax)?,
                ),
                _ if kw.eq_ignore_ascii_case("ODT_ENTRY_SIZE_FACTOR_TABLE") => {
                    XcpNode::EventOdtEntrySizeFactorTable(XcpEventOdtEntrySizeFactorTable::parse(
                        block,
                    )?)
                }
                _ if kw.eq_ignore_ascii_case("FRAMING") => {
                    XcpNode::Framing(XcpFraming::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("ODT") => XcpNode::Odt(XcpOdt::parse(block)?),
                _ if kw.eq_ignore_ascii_case("XCP_ON_CAN") => {
                    XcpNode::OnCan(XcpOnCan::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("XCP_ON_TCP_IP") => {
                    XcpNode::OnTcpIp(XcpOnTcpIp::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("XCP_ON_UDP_IP") => {
                    XcpNode::OnUdpIp(XcpOnUdpIp::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("XCP_ON_SXI") => {
                    XcpNode::OnSxi(XcpOnSxi::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("XCP_ON_USB") => {
                    XcpNode::OnUsb(XcpOnUsb::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("OUT_EP_CMD_STIM") => {
                    XcpNode::OutEpCmdStim(XcpOutEpCmdStim::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("OUT_EP_ONLY_STIM") => {
                    XcpNode::OutEpOnlyStim(XcpOutEpOnlyStim::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("IN_EP_ONLY_DAQ") => {
                    XcpNode::InEpOnlyDaq(XcpInEpOnlyDaq::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("IN_EP_ONLY_EVSERV") => {
                    XcpNode::InEpOnlyEvServ(XcpInEpOnlyEvServ::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("IN_EP_RESERR_DAQ_EVSERV") => {
                    XcpNode::InEpResErrDaqEvServ(XcpInEpResErrDaqEvServ::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("PAG") => XcpNode::Pag(XcpPag::parse(block)?),
                _ if kw.eq_ignore_ascii_case("PAGE") => XcpNode::Page(XcpPage::parse(block)?),
                _ if kw.eq_ignore_ascii_case("PGM") => XcpNode::Pgm(XcpPgm::parse(block)?),
                _ if kw.eq_ignore_ascii_case("PROTOCOL_LAYER") => {
                    XcpNode::ProtocolLayer(XcpProtocolLayer::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("SECTOR") => XcpNode::Sector(XcpSector::parse(block)?),
                _ if kw.eq_ignore_ascii_case("SEGMENT") => {
                    XcpNode::Segment(XcpSegment::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("STIM") => XcpNode::Stim(XcpStim::parse(block)?),
                _ if kw.eq_ignore_ascii_case("TIME_CORRELATION") => {
                    XcpNode::TimeCorrelation(XcpTimeCorrelation::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("TIMESTAMP_CHARACTERIZATION") => {
                    XcpNode::TimestampCharacterization(XcpTimestampCharacterization::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("TIMESTAMP_SUPPORTED") => {
                    XcpNode::TimestampSupported(XcpTimestampSupported::parse(block)?)
                }
                _ if kw.eq_ignore_ascii_case("PREDEFINED")
                    || kw.eq_ignore_ascii_case("ECU_STATES") =>
                {
                    XcpNode::Generic(XcpGenericNode {
                        keyword: block.keyword.clone(),
                        children: Vec::new(),
                    })
                }
                _ => return Ok(None),
            };
            Ok(Some(node))
        })();

        let mut node = match typed {
            Ok(Some(n)) => n,
            Ok(None) => return Ok(XcpNode::Unsupported(UnsupportedNode::from_block(block))),
            Err(_) => return Ok(XcpNode::Unsupported(UnsupportedNode::from_block(block))),
        };
        for child in block.children() {
            let child_node = XcpNode::parse_block(child)?;
            if let Some(children) = node.children_mut() {
                children.push(child_node);
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer, plus: bool) -> Result<()> {
        match self {
            XcpNode::AddressMapping(n) => n.write_body(w, plus),
            XcpNode::BufferReserve(n) => n.write_body(w, plus),
            XcpNode::CanFd(n) => n.write_body(w, plus),
            XcpNode::Checksum(n) => n.write_body(w, plus),
            XcpNode::Clock(n) => n.write_body(w, plus),
            XcpNode::CoreLoad(n) => n.write_body(w, plus),
            XcpNode::Daq(n) => n.write_body(w, plus),
            XcpNode::DaqEvent(n) => n.write_body(w, plus),
            XcpNode::DaqList(n) => n.write_body(w, plus),
            XcpNode::DaqListCanId(n) => n.write_body(w, plus),
            XcpNode::DaqListUsbEndpoint(n) => n.write_body(w, plus),
            XcpNode::DaqMemoryConsumption(n) => n.write_body(w, plus),
            XcpNode::EcuStatesMemoryAccess(n) => n.write_body(w, plus),
            XcpNode::EcuStatesState(n) => n.write_body(w, plus),
            XcpNode::Event(n) => n.write_body(w, plus),
            XcpNode::EventCanIdList(n) => n.write_body(w, plus),
            XcpNode::EventCpuLoadConsumption(n) => n.write_body(w, plus),
            XcpNode::EventCpuLoadConsumptionQueue(n) => n.write_body(w, plus),
            XcpNode::EventDaqPackedMode(n) => n.write_body(w, plus),
            XcpNode::EventList(n) => n.write_body(w, plus),
            XcpNode::EventMinCycleTime(n) => n.write_body(w, plus),
            XcpNode::EventOdtEntrySizeFactorTable(n) => n.write_body(w, plus),
            XcpNode::Framing(n) => n.write_body(w, plus),
            XcpNode::Odt(n) => n.write_body(w, plus),
            XcpNode::OnCan(n) => n.write_body(w, plus),
            XcpNode::OnTcpIp(n) => n.write_body(w, plus),
            XcpNode::OnUdpIp(n) => n.write_body(w, plus),
            XcpNode::OnSxi(n) => n.write_body(w, plus),
            XcpNode::OnUsb(n) => n.write_body(w, plus),
            XcpNode::OutEpCmdStim(n) => n.write_body(w, plus),
            XcpNode::OutEpOnlyStim(n) => n.write_body(w, plus),
            XcpNode::InEpOnlyDaq(n) => n.write_body(w, plus),
            XcpNode::InEpOnlyEvServ(n) => n.write_body(w, plus),
            XcpNode::InEpResErrDaqEvServ(n) => n.write_body(w, plus),
            XcpNode::Pag(n) => n.write_body(w, plus),
            XcpNode::Page(n) => n.write_body(w, plus),
            XcpNode::Pgm(n) => n.write_body(w, plus),
            XcpNode::ProtocolLayer(n) => n.write_body(w, plus),
            XcpNode::Sector(n) => n.write_body(w, plus),
            XcpNode::Segment(n) => n.write_body(w, plus),
            XcpNode::Stim(n) => n.write_body(w, plus),
            XcpNode::TimeCorrelation(n) => n.write_body(w, plus),
            XcpNode::TimestampCharacterization(n) => n.write_body(w, plus),
            XcpNode::TimestampSupported(n) => n.write_body(w, plus),
            XcpNode::Generic(n) => write_children(&n.children, w, plus),
            XcpNode::Unsupported(n) => {
                let _ = n;
                Ok(())
            }
        }
    }

    pub fn write_block(&self, w: &mut Writer, plus: bool) -> Result<()> {
        if let XcpNode::Unsupported(u) = self {
            return u.write_block(w).map_err(Error::from);
        }
        if let XcpNode::DaqEvent(n) = self {
            w.begin_block(&format!("{} {}", XcpDaqEvent::KEYWORD, n.name));
            n.write_body(w, plus)?;
            w.end_block(XcpDaqEvent::KEYWORD);
            return Ok(());
        }
        let kw = self.keyword().to_string();
        w.begin_block(&kw);
        self.write_body(w, plus)?;
        w.end_block(&kw);
        Ok(())
    }
}

fn write_children(children: &[XcpNode], w: &mut Writer, plus: bool) -> Result<()> {
    for c in children {
        c.write_block(w, plus)?;
    }
    Ok(())
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct XcpIfData {
    pub name: String,
    pub extra_params: Vec<String>,
    pub children: Vec<XcpNode>,
}

impl Default for XcpIfData {
    fn default() -> Self {
        XcpIfData {
            name: "XCP".to_string(),
            extra_params: Vec::new(),
            children: Vec::new(),
        }
    }
}

impl XcpIfData {
    pub const KEYWORD: &'static str = "IF_DATA";

    pub fn is_xcp_plus(&self) -> bool {
        self.name.eq_ignore_ascii_case("XCPplus")
    }

    pub fn is_xcp_name(name: &str) -> bool {
        name.eq_ignore_ascii_case("XCP") || name.eq_ignore_ascii_case("XCPplus")
    }

    pub fn parse(block: &Block) -> Result<Self> {
        if !block.keyword.eq_ignore_ascii_case(Self::KEYWORD) {
            return Err(Error::Parse(format!(
                "XcpIfData: expected IF_DATA block, got {:?}",
                block.keyword
            )));
        }
        let mut node = XcpIfData::default();
        let mut params = block.params();
        match params.next() {
            Some(t) => node.name = t.text.clone(),
            None => {
                return Err(Error::Parse(
                    "XcpIfData: missing IF_DATA name (XCP/XCPplus)".to_string(),
                ))
            }
        }
        if !Self::is_xcp_name(&node.name) {
            return Err(Error::Parse(format!(
                "XcpIfData: IF_DATA name {:?} is not XCP/XCPplus",
                node.name
            )));
        }
        node.extra_params = params.map(|t| t.text.clone()).collect();
        for child in block.children() {
            node.children.push(XcpNode::parse_block(child)?);
        }
        Ok(node)
    }

    pub fn child(&self, keyword: &str) -> Option<&XcpNode> {
        self.children
            .iter()
            .find(|n| n.keyword().eq_ignore_ascii_case(keyword))
    }

    /// XCP_ON_USB / XCP_ON_SxI).
    pub fn media(&self) -> Option<&XcpNode> {
        self.children.iter().find(|n| {
            matches!(
                n,
                XcpNode::OnCan(_)
                    | XcpNode::OnTcpIp(_)
                    | XcpNode::OnUdpIp(_)
                    | XcpNode::OnSxi(_)
                    | XcpNode::OnUsb(_)
            )
        })
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        let mut first = format!("{} {}", Self::KEYWORD, self.name);
        for p in &self.extra_params {
            first.push(' ');
            first.push_str(p);
        }
        let plus = self.is_xcp_plus();
        w.begin_block(&first);
        write_children(&self.children, w, plus)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }

    pub fn write_string(&self) -> Result<String> {
        let mut w = Writer::new(autors_a2l::writer::WriterOptions::default());
        self.write_block(&mut w)?;
        Ok(w.into_string())
    }
}

// ============================================================================
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use autors_a2l::block::build_block_tree;
    use autors_a2l::token::tokenize;
    use autors_a2l::writer::WriterOptions;

    fn block_of(src: &str) -> Block {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let first = root.children().next().unwrap().clone();
        first
    }

    fn ifdata_of(src: &str) -> XcpIfData {
        XcpIfData::parse(&block_of(src)).unwrap()
    }

    fn node_of(src: &str) -> XcpNode {
        XcpNode::parse_block(&block_of(src)).unwrap()
    }

    fn assert_node_roundtrip(src: &str) -> XcpNode {
        assert_node_roundtrip_plus(src, false)
    }

    fn assert_node_roundtrip_plus(src: &str, plus: bool) -> XcpNode {
        let n1 = node_of(src);
        let mut w = Writer::new(WriterOptions::default());
        n1.write_block(&mut w, plus).unwrap();
        let text = w.into_string();
        let n2 = node_of(&text);
        assert_eq!(n1, n2, "round-trip mismatch, written:\n{text}");
        n1
    }

    fn assert_ifdata_roundtrip(src: &str) -> XcpIfData {
        let n1 = ifdata_of(src);
        let text = n1.write_string().unwrap();
        let n2 = ifdata_of(&text);
        assert_eq!(n1, n2, "round-trip mismatch, written:\n{text}");
        n1
    }

    // ------------------------------------------------------------------------
    // ------------------------------------------------------------------------

    #[test]
    fn address_mapping_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin ADDRESS_MAPPING 0x1000 0x2000 0x400 /end ADDRESS_MAPPING",
        );
        let XcpNode::AddressMapping(m) = n else {
            panic!("wrong node: {n:?}")
        };
        assert_eq!(m.src_address, 0x1000);
        assert_eq!(m.dst_address, 0x2000);
        assert_eq!(m.length, 0x400);
    }

    #[test]
    fn buffer_reserve_roundtrip() {
        let n = assert_node_roundtrip("/begin BUFFER_RESERVE_EVENT 1 2 /end BUFFER_RESERVE_EVENT");
        let XcpNode::BufferReserve(b) = n else {
            panic!("wrong node")
        };
        assert_eq!(b.kind, BufferReserveKind::BufferReserveEvent);
        assert_eq!((b.odt_daq, b.odt_stim), (1, 2));

        let n = assert_node_roundtrip("/begin BUFFER_RESERVE 3 4 /end BUFFER_RESERVE");
        let XcpNode::BufferReserve(b) = n else {
            panic!("wrong node")
        };
        assert_eq!(b.kind, BufferReserveKind::BufferReserve);
    }

    #[test]
    fn can_fd_roundtrip() {
        let src = "/begin CAN_FD MAX_DLC 64 CAN_FD_DATA_TRANSFER_BAUDRATE 2000000 \
                   SAMPLE_POINT 80 BTL_CYCLES 10 SJW 0x2 MAX_DLC_REQUIRED SYNC_EDGE SINGLE \
                   SECONDARY_SAMPLE_POINT 75 TRANSCEIVER_DELAY_COMPENSATION ON /end CAN_FD";
        let n = assert_node_roundtrip(src);
        let XcpNode::CanFd(f) = n else {
            panic!("wrong node")
        };
        assert_eq!(f.max_dlc, 64);
        assert_eq!(f.data_transfer_baudrate, 2_000_000);
        assert_eq!(f.sample_point, 80);
        assert_eq!(f.secondary_sample_point, 75);
        assert_eq!(f.btl_cycles, 10);
        assert_eq!(f.sjw, 2);
        assert!(f.max_dlc_required);
        assert_eq!(f.sync_edge, XcpSyncEdge::SINGLE);
        assert_eq!(
            f.transceiver_delay_compensation,
            XcpTransceiverDelayCompensation::ON
        );
    }

    #[test]
    fn can_fd_max_dlc_clamped_to_8() {
        let n = node_of("/begin CAN_FD MAX_DLC 4 /end CAN_FD");
        let XcpNode::CanFd(f) = n else {
            panic!("wrong node")
        };
        assert_eq!(f.max_dlc, 8);
        let mut w = Writer::new(WriterOptions::default());
        XcpNode::CanFd(f).write_block(&mut w, false).unwrap();
        assert!(w.into_string().contains("MAX_DLC 8"));
    }

    #[test]
    fn checksum_roundtrip() {
        let src = "/begin CHECKSUM XCP_CRC_16 MAX_BLOCK_SIZE 0xFF EXTERNAL_FUNCTION \"chk.dll\" /end CHECKSUM";
        let n = assert_node_roundtrip(src);
        let XcpNode::Checksum(c) = n else {
            panic!("wrong node")
        };
        assert_eq!(c.check_sum, ChecksumType::CRC_16);
        assert_eq!(c.max_block_size, 0xFF);
        assert_eq!(c.external_function, "chk.dll");
    }

    #[test]
    fn checksum_mta_block_size_align_quoted() {
        let n = node_of("/begin CHECKSUM XCP_ADD_44 MTA_BLOCK_SIZE_ALIGN 4 /end CHECKSUM");
        let mut w = Writer::new(WriterOptions::default());
        n.write_block(&mut w, false).unwrap();
        let text = w.into_string();
        assert!(text.contains("MTA_BLOCK_SIZE_ALIGN \"4\""), "{text}");
        let n2 = node_of(&text);
        let XcpNode::Checksum(c) = n2 else {
            panic!("wrong node")
        };
        assert_eq!(c.mta_block_size_align, 4);
    }

    #[test]
    fn clock_roundtrip() {
        let src = "/begin CLOCK 0x01 0x02 0x03 0x04 0x05 0x06 0x07 0x08 \
                   XCP_SLAVE_CLOCK RANDOMLY_READABLE SYNCHRONIZATION_ONLY 2 4294967295 \
                   UNIVERSAL_COORDINATED_TIME /end CLOCK";
        let n = assert_node_roundtrip(src);
        let XcpNode::Clock(c) = n else {
            panic!("wrong node")
        };
        assert_eq!(c.uuid, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(c.mode, XcpClockMode::XCP_SLAVE_CLOCK);
        assert_eq!(c.max_timestamp_value_before_wraparound, 4294967295);
        assert_eq!(c.epoch, XcpClockEpoch::UNIVERSAL_COORDINATED_TIME);
    }

    #[test]
    fn core_load_roundtrip() {
        let n = assert_node_roundtrip("/begin CORE_LOAD_EP 1 37.5 /end CORE_LOAD_EP");
        let XcpNode::CoreLoad(c) = n else {
            panic!("wrong node")
        };
        assert_eq!(c.core_nr, 1);
        assert!((c.core_load - 37.5).abs() < f32::EPSILON);
    }

    // ------------------------------------------------------------------------
    // ------------------------------------------------------------------------

    #[test]
    fn daq_roundtrip() {
        let src = "/begin DAQ DYNAMIC 0x20 0x10 0x0 OPTIMISATION_TYPE_DEFAULT ADDRESS_EXTENSION_FREE \
                   IDENTIFICATION_FIELD_TYPE_ABSOLUTE GRANULARITY_ODT_ENTRY_SIZE_DAQ_BYTE 0x7 \
                   OVERLOAD_INDICATION_PID \
                   /begin TIMESTAMP_SUPPORTED 0x1 SIZE_DWORD UNIT_1MS TIMESTAMP_FIXED /end TIMESTAMP_SUPPORTED \
                   /end DAQ";
        let n = assert_node_roundtrip(src);
        let XcpNode::Daq(d) = n else {
            panic!("wrong node")
        };
        assert_eq!(d.mode, XcpDaqMode::DYNAMIC);
        assert!(d.is_daq_dynamic());
        assert_eq!(d.max_daq, 0x20);
        assert_eq!(d.max_evt_chn, 0x10);
        assert_eq!(d.min_daq, 0);
        assert_eq!(d.opt_type, XcpOptimisationType::DEFAULT);
        assert_eq!(d.adr_ext, XcpAddressExt::FREE);
        assert_eq!(d.id_field, XcpIdFieldType::ABSOLUTE);
        assert_eq!(d.id_field_size(), 1);
        assert_eq!(d.odt_entry_size, XcpOdtEntrySize::BYTE);
        assert_eq!(d.max_odt_entry_size, 7);
        assert_eq!(d.overload_ind, XcpOverloadInd::PID);
        assert_eq!(d.children.len(), 1);
    }

    #[test]
    fn daq_id_field_variants() {
        for (kw, expect, size) in [
            (
                "IDENTIFICATION_FIELD_TYPE_RELATIVE_BYTE",
                XcpIdFieldType::BYTE,
                2u8,
            ),
            (
                "IDENTIFICATION_FIELD_TYPE_RELATIVE_WORD",
                XcpIdFieldType::WORD,
                3u8,
            ),
            (
                "IDENTIFICATION_FIELD_TYPE_RELATIVE_WORD_ALIGNED",
                XcpIdFieldType::ALIGNED,
                4u8,
            ),
        ] {
            let src = format!(
                "/begin DAQ STATIC 0x4 0x2 0 OPTIMISATION_TYPE_ODT_TYPE_16 ADDRESS_EXTENSION_ODT \
                 {kw} GRANULARITY_ODT_ENTRY_SIZE_DAQ_WORD 0x4 NO_OVERLOAD_INDICATION /end DAQ"
            );
            let n = assert_node_roundtrip(&src);
            let XcpNode::Daq(d) = n else {
                panic!("wrong node")
            };
            assert_eq!(d.id_field, expect, "{kw}");
            assert_eq!(d.id_field_size(), size, "{kw}");
            assert_eq!(d.opt_type, XcpOptimisationType::ODT_TYPE_16);
            assert_eq!(d.overload_ind, XcpOverloadInd::INDICATION);
            assert!(!d.is_daq_dynamic());
        }
    }

    #[test]
    fn daq_xcpplus_totals_and_flags() {
        let src = "/begin DAQ DYNAMIC 0x20 0x10 0 OPTIMISATION_TYPE_MAX_ENTRY_SIZE ADDRESS_EXTENSION_DAQ \
                   IDENTIFICATION_FIELD_TYPE_ABSOLUTE GRANULARITY_ODT_ENTRY_SIZE_DAQ_DLONG 0x8 \
                   OVERLOAD_INDICATION_EVENT DAQ_ALTERNATING_SUPPORTED 1 \
                   PRESCALER_SUPPORTED RESUME_SUPPORTED STORE_DAQ_SUPPORTED DTO_CTR_FIELD_SUPPORTED \
                   PID_OFF_SUPPORTED MAX_DAQ_TOTAL 100 MAX_ODT_TOTAL 200 MAX_ODT_DAQ_TOTAL 150 \
                   MAX_ODT_STIM_TOTAL 50 MAX_ODT_ENTRIES_TOTAL 300 MAX_ODT_ENTRIES_DAQ_TOTAL 250 \
                   MAX_ODT_ENTRIES_STIM_TOTAL 60 CPU_LOAD_MAX_TOTAL 1.5 CORE_LOAD_MAX_TOTAL 2.5 /end DAQ";
        let n = assert_node_roundtrip(src);
        let XcpNode::Daq(d) = n else {
            panic!("wrong node")
        };
        assert_eq!(d.daq_alternating_supported, 1);
        assert!(d.prescaler_supported && d.resume_supported && d.store_daq_supported);
        assert!(d.dto_ctr_field_supported && d.pid_off_supported);
        assert_eq!(d.max_daq_total, 100);
        assert_eq!(d.max_odt_entries_stim_total, 60);
        assert!((d.cpu_load_max_total - 1.5).abs() < f32::EPSILON);
        assert!((d.core_load_max_total - 2.5).abs() < f32::EPSILON);
    }

    #[test]
    fn daq_event_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin DAQ_EVENT VARIABLE EVENT 1 EVENT 2 EVENT 2 /end DAQ_EVENT",
        );
        let XcpNode::DaqEvent(e) = n else {
            panic!("wrong node")
        };
        assert_eq!(e.name, "VARIABLE");
        assert_eq!(e.events, vec![1, 2]);
    }

    #[test]
    fn daq_list_roundtrip() {
        let src = "/begin DAQ_LIST 3 DAQ_LIST_TYPE DAQ_STIM MAX_ODT 7 MAX_ODT_ENTRIES 5 \
                   EVENT_FIXED 10 FIRST_PID 0x2 DAQ_PACKED_MODE_SUPPORTED /end DAQ_LIST";
        let n = assert_node_roundtrip(src);
        let XcpNode::DaqList(l) = n else {
            panic!("wrong node")
        };
        assert_eq!(l.daq_no, 3);
        assert_eq!(l.daq_list_type, XcpDaqListType::DAQ_STIM);
        assert_eq!(l.max_odt, 7);
        assert_eq!(l.max_odt_entries, 5);
        assert_eq!(l.event_fixed, 10);
        assert_eq!(l.first_pid, 2);
        assert!(l.packed_mode_supported);
        assert!(l.active);
    }

    #[test]
    fn daq_list_can_id_roundtrip() {
        let n = assert_node_roundtrip("/begin DAQ_LIST_CAN_ID 1 FIXED 0x123 /end DAQ_LIST_CAN_ID");
        let XcpNode::DaqListCanId(c) = n else {
            panic!("wrong node")
        };
        assert_eq!(c.daq_list_type, XcpDaqListCanType::FIXED);
        assert_eq!(c.can_id, 0x123);

        let n = assert_node_roundtrip("/begin DAQ_LIST_CAN_ID 2 VARIABLE /end DAQ_LIST_CAN_ID");
        let XcpNode::DaqListCanId(c) = n else {
            panic!("wrong node")
        };
        assert_eq!(c.daq_list_type, XcpDaqListCanType::VARIABLE);
    }

    #[test]
    fn daq_list_usb_endpoint_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin DAQ_LIST_USB_ENDPOINT 0x5 FIXED_IN 1 /end DAQ_LIST_USB_ENDPOINT",
        );
        let XcpNode::DaqListUsbEndpoint(e) = n else {
            panic!("wrong node")
        };
        assert_eq!(e.daq_no, 5);
        assert_eq!(e.ep_type, XcpEndpoint::IN);
        assert_eq!(e.ep_no, 1);

        let n = assert_node_roundtrip(
            "/begin DAQ_LIST_USB_ENDPOINT 0x6 FIXED_OUT 2 /end DAQ_LIST_USB_ENDPOINT",
        );
        let XcpNode::DaqListUsbEndpoint(e) = n else {
            panic!("wrong node")
        };
        assert_eq!(e.ep_type, XcpEndpoint::OUT);
    }

    #[test]
    fn daq_memory_consumption_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin DAQ_MEMORY_CONSUMPTION 4096 16 8 4 12 12 /end DAQ_MEMORY_CONSUMPTION",
        );
        let XcpNode::DaqMemoryConsumption(m) = n else {
            panic!("wrong node")
        };
        assert_eq!(m.daq_memory_limit, 4096);
        assert_eq!(m.odt_stim_buffer_element_size, 12);
    }

    #[test]
    fn odt_roundtrip() {
        let src =
            "/begin ODT 1 ODT_ENTRY 0 0x4000 0x0 0x2 0x0 ODT_ENTRY 1 0x4002 0x0 0x4 0x3 /end ODT";
        let n = assert_node_roundtrip(src);
        let XcpNode::Odt(o) = n else {
            panic!("wrong node")
        };
        assert_eq!(o.odt_no, 1);
        assert_eq!(o.odt_entries.len(), 2);
        assert_eq!(o.odt_entries[0].address, 0x4000);
        assert_eq!(o.odt_entries[1].size, 4);
        assert_eq!(o.odt_entries[1].bit_offset, 3);
    }

    #[test]
    fn event_roundtrip() {
        let src = "/begin EVENT \"task 10ms\" \"t10\" 5 DAQ 3 10 6 1 \
                   CONSISTENCY DAQ /end EVENT";
        let n = assert_node_roundtrip(src);
        let XcpNode::Event(e) = n else {
            panic!("wrong node")
        };
        assert_eq!(e.name_long, "task 10ms");
        assert_eq!(e.name_short, "t10");
        assert_eq!(e.id, 5);
        assert_eq!(e.daq_list_type, XcpDaqListType::DAQ);
        assert_eq!(e.max_daq_list, 3);
        assert_eq!(e.time_cycle, 10);
        assert_eq!(e.time_unit, XcpTimestampResolution::_1MS);
        assert_eq!(e.time_unit.as_u8(), 6);
        assert_eq!(e.priority, 1);
        assert_eq!(e.consistency, XcpAddressMode::DAQ);
    }

    #[test]
    fn event_xcpplus_fields_gated_by_ifdata_name() {
        let src = "/begin EVENT \"a\" \"b\" 1 STIM 1 1 3 0 \
                   COMPLEMENTARY_BYPASS_EVENT_CHANNEL_NUMBER 7 \
                   EVENT_COUNTER_PRESENT RELATED_EVENT_CHANNEL_NUMBER 9 \
                   RELATED_EVENT_CHANNEL_NUMBER_FIXED DTO_CTR_DAQ_MODE INSERT_COUNTER \
                   DTO_CTR_DAQ_MODE_FIXED DTO_CTR_STIM_MODE CHECK_COUNTER DTO_CTR_STIM_MODE_FIXED \
                   STIM_DTO_CTR_COPY_PRESENT CPU_LOAD_MAX 12.5 /end EVENT";
        let n = assert_node_roundtrip_plus(src, true);
        let XcpNode::Event(e) = n.clone() else {
            panic!("wrong node")
        };
        assert!(e.event_counter_present);
        assert_eq!(e.related_event_channel_number, 9);
        assert_eq!(e.dto_ctr_daq_mode, DtoCtrDaqMode::INSERT_COUNTER);
        assert_eq!(e.dto_ctr_stim_mode, DtoCtrStimMode::CHECK_COUNTER);
        assert!((e.cpu_load_max - 12.5).abs() < f32::EPSILON);

        let mut w = Writer::new(WriterOptions::default());
        n.write_block(&mut w, false).unwrap();
        let text = w.into_string();
        assert!(!text.contains("EVENT_COUNTER_PRESENT"), "{text}");
        assert!(!text.contains("CPU_LOAD_MAX"), "{text}");
        assert!(
            text.contains("COMPLEMENTARY_BYPASS_EVENT_CHANNEL_NUMBER 7"),
            "{text}"
        );
    }

    #[test]
    fn event_can_id_list_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin EVENT_CAN_ID_LIST 4 FIXED 0x111 FIXED 0x222 /end EVENT_CAN_ID_LIST",
        );
        let XcpNode::EventCanIdList(l) = n else {
            panic!("wrong node")
        };
        assert_eq!(l.evt_no, 4);
        assert_eq!(l.fixed_can_ids, vec![0x111, 0x222]);
    }

    #[test]
    fn event_cpu_load_consumption_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin CPU_LOAD_CONSUMPTION_DAQ 1 0.5 0.25 /end CPU_LOAD_CONSUMPTION_DAQ",
        );
        let XcpNode::EventCpuLoadConsumption(c) = n else {
            panic!("wrong node")
        };
        assert_eq!(c.kind, CpuLoadConsumptionKind::Daq);

        let n = assert_node_roundtrip(
            "/begin CPU_LOAD_CONSUMPTION_STIM 2 0.5 0.25 /end CPU_LOAD_CONSUMPTION_STIM",
        );
        let XcpNode::EventCpuLoadConsumption(c) = n else {
            panic!("wrong node")
        };
        assert_eq!(c.kind, CpuLoadConsumptionKind::Stim);
    }

    #[test]
    fn event_cpu_load_consumption_queue_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin CPU_LOAD_CONSUMPTION_QUEUE_STIM 0.5 0.75 /end CPU_LOAD_CONSUMPTION_QUEUE_STIM",
        );
        let XcpNode::EventCpuLoadConsumptionQueue(c) = n else {
            panic!("wrong node")
        };
        assert_eq!(c.kind, CpuLoadConsumptionQueueKind::QueueStim);
        assert!((c.odt_element_load - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn event_daq_packed_mode_roundtrip() {
        let src = "/begin DAQ_PACKED_MODE ELEMENT_GROUPED STS_LAST MANDATORY 3 \
                   ALT_SAMPLE_COUNT 1 ALT_SAMPLE_COUNT 2 /end DAQ_PACKED_MODE";
        let n = assert_node_roundtrip(src);
        let XcpNode::EventDaqPackedMode(p) = n else {
            panic!("wrong node")
        };
        assert_eq!(p.group_mode, XcpGroupMode::ELEMENT_GROUPED);
        assert_eq!(p.sts_mode, XcpStsMode::LAST);
        assert_eq!(p.pack_mode, XcpPackMode::MANDATORY);
        assert_eq!(p.sample_count, 3);
        assert_eq!(p.alt_sample_counts, vec![1, 2]);
    }

    #[test]
    fn event_list_kinds_roundtrip() {
        for kw in [
            "AVAILABLE_EVENT_LIST",
            "DEFAULT_EVENT_LIST",
            "CONSISTENCY_EVENT_LIST",
        ] {
            let src = format!("/begin {kw} EVENT 1 EVENT 3 /end {kw}");
            let n = assert_node_roundtrip(&src);
            assert_eq!(n.keyword(), kw);
            let XcpNode::EventList(l) = n else {
                panic!("wrong node")
            };
            assert_eq!(l.events, vec![1, 3]);
        }
    }

    #[test]
    fn event_min_cycle_time_roundtrip() {
        let n = assert_node_roundtrip("/begin MIN_CYCLE_TIME 5 3 /end MIN_CYCLE_TIME");
        let XcpNode::EventMinCycleTime(m) = n else {
            panic!("wrong node")
        };
        assert_eq!(m.kind, MinCycleTimeKind::MinCycleTime);
        assert_eq!(m.time_cycle, 5);
        assert_eq!(m.time_unit, XcpTimestampResolution::_1US);

        let n = assert_node_roundtrip("/begin CORE_LOAD_MAX 2 6 /end CORE_LOAD_MAX");
        let XcpNode::EventMinCycleTime(m) = n else {
            panic!("wrong node")
        };
        assert_eq!(m.kind, MinCycleTimeKind::CoreLoadMax);
    }

    #[test]
    fn odt_entry_size_factor_table_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin ODT_ENTRY_SIZE_FACTOR_TABLE 4 0.5 /end ODT_ENTRY_SIZE_FACTOR_TABLE",
        );
        let XcpNode::EventOdtEntrySizeFactorTable(t) = n else {
            panic!("wrong node")
        };
        assert_eq!(t.size, 4);
        assert!((t.size_factor - 0.5).abs() < f32::EPSILON);
    }

    // ------------------------------------------------------------------------
    // ------------------------------------------------------------------------

    #[test]
    fn framing_roundtrip() {
        let n = assert_node_roundtrip("/begin FRAMING 0xAA 0x55 /end FRAMING");
        let XcpNode::Framing(f) = n else {
            panic!("wrong node")
        };
        assert_eq!((f.sync, f.esc), (0xAA, 0x55));
    }

    #[test]
    fn on_can_roundtrip() {
        let src = "/begin XCP_ON_CAN 0x101 \
                   CAN_ID_MASTER_INCREMENTAL CAN_ID_BROADCAST 0x7FF \
                   CAN_ID_GET_DAQ_CLOCK_MULTICAST 0x100 CAN_ID_MASTER 0x667 CAN_ID_SLAVE 0x668 \
                   BAUDRATE 500000 SAMPLE_POINT 80 SAMPLE_RATE SINGLE BTL_CYCLES 10 SJW 0x2 \
                   MAX_DLC_REQUIRED SYNC_EDGE SINGLE MEASUREMENT_SPLIT_ALLOWED MAX_BUS_LOAD 0x5A \
                   TRANSPORT_LAYER_INSTANCE \"CAN1\" OPTIONAL_TL_SUBCMD GET_DAQ_PROCESSOR_INFO \
                   /begin CAN_FD MAX_DLC 64 /end CAN_FD \
                   /end XCP_ON_CAN";
        let n = assert_node_roundtrip(src);
        let XcpNode::OnCan(c) = n else {
            panic!("wrong node")
        };
        assert_eq!(c.media.version, 0x101);
        assert!(c.can_id_master_incremental);
        assert_eq!(c.can_id_broadcast, 0x7FF);
        assert_eq!(c.can_id_get_daq_clock_multicast, 0x100);
        assert_eq!(c.can_id_cmd, 0x667);
        assert_eq!(c.can_id_resp, 0x668);
        assert!(!c.request_ids_by_broadcast());
        assert_eq!(c.baudrate, 500000);
        assert_eq!(c.sample_point, 80);
        assert_eq!(c.sample_rate, XcpDaqListCanSampleRate::SINGLE);
        assert_eq!(c.btl_cycles, 10);
        assert_eq!(c.sjw, 2);
        assert!(c.max_dlc_required);
        assert_eq!(c.sync_edge, XcpSyncEdge::SINGLE);
        assert!(c.measurement_split_allowed);
        assert_eq!(c.media.max_bus_load, 0x5A);
        assert_eq!(c.media.transport_layer_instance, "CAN1");
        assert_eq!(c.media.optional_tl_cmds, ["GET_DAQ_PROCESSOR_INFO"]);
        assert_eq!(c.children.len(), 1);
    }

    #[test]
    fn on_can_optional_cmd_compat() {
        let n = node_of(
            "/begin XCP_ON_CAN 0x101 CAN_ID_MASTER 0x1 CAN_ID_SLAVE 0x2 \
                         OPTIONAL_CMD GET_DAQ_CLOCK /end XCP_ON_CAN",
        );
        let XcpNode::OnCan(c) = &n else {
            panic!("wrong node")
        };
        assert_eq!(c.media.optional_tl_cmds, ["GET_DAQ_CLOCK"]);
        let mut w = Writer::new(WriterOptions::default());
        n.write_block(&mut w, false).unwrap();
        let text = w.into_string();
        assert!(text.contains("OPTIONAL_TL_SUBCMD GET_DAQ_CLOCK"), "{text}");
    }

    #[test]
    fn on_can_request_ids_by_broadcast() {
        let n = node_of("/begin XCP_ON_CAN 0x101 CAN_ID_BROADCAST 0x7FF /end XCP_ON_CAN");
        let XcpNode::OnCan(c) = n else {
            panic!("wrong node")
        };
        assert!(c.request_ids_by_broadcast());
    }

    #[test]
    fn on_tcp_udp_roundtrip() {
        let src = "/begin XCP_ON_UDP_IP 0x102 5555 ADDRESS \"192.168.0.10\" HOST_NAME \"ecu1\" \
                   IPV6 \"::1\" PACKET_ALIGNMENT_32 TRANSPORT_LAYER_INSTANCE \"eth0\" \
                   OPTIONAL_TL_SUBCMD GET_DAQ_PROCESSOR_INFO /end XCP_ON_UDP_IP";
        let n = assert_node_roundtrip(src);
        let XcpNode::OnUdpIp(u) = n else {
            panic!("wrong node")
        };
        assert_eq!(u.media.version, 0x102);
        assert_eq!(u.ethernet.port, 5555);
        assert_eq!(u.ethernet.address, "192.168.0.10");
        assert_eq!(u.ethernet.host_name, "ecu1");
        assert_eq!(u.ethernet.ipv6, "::1");
        assert_eq!(u.ethernet.packet_alignment, XcpPacketAligment::_32);

        let n = assert_node_roundtrip(
            "/begin XCP_ON_TCP_IP 0x100 8080 ADDRESS \"127.0.0.1\" /end XCP_ON_TCP_IP",
        );
        assert!(matches!(n, XcpNode::OnTcpIp(_)));
    }

    #[test]
    fn on_ethernet_max_bit_rate_parsed_but_not_written() {
        let n = node_of("/begin XCP_ON_UDP_IP 0x100 5555 MAX_BIT_RATE 100 /end XCP_ON_UDP_IP");
        let XcpNode::OnUdpIp(u) = &n else {
            panic!("wrong node")
        };
        assert_eq!(u.ethernet.max_bit_rate, 100);
        let mut w = Writer::new(WriterOptions::default());
        n.write_block(&mut w, false).unwrap();
        let text = w.into_string();
        assert!(!text.contains("MAX_BIT_RATE"), "{text}");
    }

    #[test]
    fn on_sxi_roundtrip() {
        let src = "/begin XCP_ON_SxI 0x101 115200 \
                   ASYNCH_FULL_DUPLEX_MODE PARITY_EVEN ONE_STOP_BIT \
                   SYNCH_FULL_DUPLEX_MODE_BYTE SYNCH_MASTER_SLAVE_MODE_WORD HEADER_LEN_CTR_BYTE \
                   CHECKSUM_BYTE TRANSPORT_LAYER_INSTANCE \"COM1\" \
                   /begin FRAMING 0xAA 0x55 /end FRAMING /end XCP_ON_SxI";
        let n = assert_node_roundtrip(src);
        let XcpNode::OnSxi(s) = n else {
            panic!("wrong node")
        };
        assert_eq!(s.media.version, 0x101);
        assert_eq!(s.baudrate, 115200);
        let dm = s.duplex_mode.unwrap();
        assert_eq!(dm.parity, ParityType::EVEN);
        assert_eq!(dm.stop_bits, StopBitsType::ONE_STOP_BIT);
        assert_eq!(s.sync_full_duplex_mode, SyncModeSize::BYTE);
        assert_eq!(s.sync_master_slave_mode, SyncModeSize::WORD);
        assert_eq!(s.header_len, XcpHeaderLen::CTR_BYTE);
        assert_eq!(s.checksum, ChecksumSxi::CHECKSUM_BYTE);
        assert_eq!(s.children.len(), 1);
    }

    #[test]
    fn on_usb_roundtrip() {
        let src = "/begin XCP_ON_USB 0x101 0x1234 0x5678 1 HEADER_LEN_CTR_BYTE \
                   ALTERNATE_SETTING_NO 2 INTERFACE_STRING_DESCRIPTOR \"XCP-USB\" \
                   /begin OUT_EP_CMD_STIM 0x1 BULK_TRANSFER 0x40 0 MESSAGE_PACKING_SINGLE \
                   ALIGNMENT_8_BIT /end OUT_EP_CMD_STIM \
                   /begin IN_EP_ONLY_DAQ 0x81 INTERRUPT_TRANSFER 0x40 1 MESSAGE_PACKING_SINGLE \
                   ALIGNMENT_8_BIT RECOMMENDED_HOST_BUFSIZE 0x100 /end IN_EP_ONLY_DAQ \
                   /begin DAQ_LIST_USB_ENDPOINT 0x0 FIXED_IN 1 /end DAQ_LIST_USB_ENDPOINT \
                   /end XCP_ON_USB";
        let n = assert_node_roundtrip(src);
        let XcpNode::OnUsb(u) = n else {
            panic!("wrong node")
        };
        assert_eq!(u.vendor_id, 0x1234);
        assert_eq!(u.product_id, 0x5678);
        assert_eq!(u.number_of_if, 1);
        assert_eq!(u.header_len, XcpHeaderLen::CTR_BYTE);
        assert_eq!(u.alternate_setting_no, 2);
        assert_eq!(u.interface_descriptor, "XCP-USB");
        assert_eq!(u.children.len(), 3);
    }

    #[test]
    fn endpoint_roundtrip() {
        let src = "/begin IN_EP_RESERR_DAQ_EVSERV 0x81 INTERRUPT_TRANSFER 0x40 1 \
                   MESSAGE_PACKING_STREAMING ALIGNMENT_32_BIT RECOMMENDED_HOST_BUFSIZE 0x200 \
                   /end IN_EP_RESERR_DAQ_EVSERV";
        let n = assert_node_roundtrip(src);
        let XcpNode::InEpResErrDaqEvServ(e) = n else {
            panic!("wrong node")
        };
        assert_eq!(e.endpoint.ep_no, 0x81);
        assert_eq!(e.endpoint.transfer_type, XcpTransfer::INTERRUPT_TRANSFER);
        assert_eq!(e.endpoint.max_pkt_size, 0x40);
        assert_eq!(e.endpoint.packing, XcpMessagePacking::STREAMING);
        assert_eq!(e.endpoint.alignment, XcpAlignment::_32_BIT);
        assert_eq!(e.endpoint.host_buffer_size, 0x200);

        assert!(matches!(
            node_of("/begin OUT_EP_ONLY_STIM 0x1 BULK_TRANSFER 0x8 0 MESSAGE_PACKING_SINGLE ALIGNMENT_8_BIT /end OUT_EP_ONLY_STIM"),
            XcpNode::OutEpOnlyStim(_)
        ));
        assert!(matches!(
            node_of("/begin IN_EP_ONLY_EVSERV 0x2 BULK_TRANSFER 0x8 0 MESSAGE_PACKING_SINGLE ALIGNMENT_8_BIT /end IN_EP_ONLY_EVSERV"),
            XcpNode::InEpOnlyEvServ(_)
        ));
    }

    // ------------------------------------------------------------------------
    // ------------------------------------------------------------------------

    #[test]
    fn pag_roundtrip() {
        let n = assert_node_roundtrip("/begin PAG 0x2 FREEZE_SUPPORTED /end PAG");
        let XcpNode::Pag(p) = n else {
            panic!("wrong node")
        };
        assert_eq!(p.max_segments, 2);
        assert!(p.properties.contains(PagProperties::FREEZE_SUPPORTED));
    }

    #[test]
    fn page_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin PAGE 0x0 ECU_ACCESS_DONT_CARE XCP_READ_ACCESS_WITH_ECU_ONLY \
             XCP_WRITE_ACCESS_NOT_ALLOWED /end PAGE",
        );
        let XcpNode::Page(p) = n else {
            panic!("wrong node")
        };
        assert_eq!(p.page_no, 0);
        assert_eq!(p.access_ecu, XcpEcuAccess::DONT_CARE);
        assert_eq!(p.access_read, XcpReadWriteAccess::WITH_ECU_ONLY);
        assert_eq!(p.access_write, XcpReadWriteAccess::NOT_ALLOWED);
    }

    #[test]
    fn pgm_roundtrip() {
        let src = "/begin PGM PGM_MODE_ABSOLUTE_AND_FUNCTIONAL 0x2 0x8 \
                   COMMUNICATION_MODE_SUPPORTED BLOCK SLAVE MASTER 0x04 0x02 INTERLEAVED 0x01 \
                   /begin SECTOR \"flash0\" 0x0 0x8000000 0x10000 0x1 0x2 0x0 /end SECTOR \
                   /end PGM";
        let n = assert_node_roundtrip(src);
        let XcpNode::Pgm(p) = n else {
            panic!("wrong node")
        };
        assert!(p.mode.contains(XcpPgmMode::ABSOLUTE));
        assert!(p.mode.contains(XcpPgmMode::FUNCTIONAL));
        assert_eq!(p.max_sectors, 2);
        assert_eq!(p.max_cto, 8);
        assert!(p
            .comm_modes_supported
            .comm_mode
            .contains(XcpBlockMode::SLAVE));
        assert!(p
            .comm_modes_supported
            .comm_mode
            .contains(XcpBlockMode::MASTER));
        assert!(p
            .comm_modes_supported
            .comm_mode
            .contains(XcpBlockMode::INTERLEAVED));
        assert_eq!(p.comm_modes_supported.max_bs, 4);
        assert_eq!(p.comm_modes_supported.min_st, 2);
        assert_eq!(p.comm_modes_supported.queue_size, 1);
        assert_eq!(p.children.len(), 1);
    }

    #[test]
    fn protocol_layer_roundtrip() {
        let src = "/begin PROTOCOL_LAYER 0x101 1 2 3 4 5 6 7 0x8 0xFF \
                   BYTE_ORDER_MSB_FIRST ADDRESS_GRANULARITY_WORD \
                   SEED_AND_KEY_EXTERNAL_FUNCTION \"seedkey.dll\" \
                   OPTIONAL_CMD GET_DAQ_CLOCK OPTIONAL_CMD SET_CAL_PAGE \
                   COMMUNICATION_MODE_SUPPORTED BLOCK MASTER 0x08 0x01 \
                   MAX_DTO_STIM 0x20 /end PROTOCOL_LAYER";
        let n = assert_node_roundtrip(src);
        let XcpNode::ProtocolLayer(p) = n else {
            panic!("wrong node")
        };
        assert_eq!(p.version, 0x101);
        assert_eq!(p.timings, [1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(p.max_cto, 8);
        assert_eq!(p.max_dto, 0xFF);
        assert!(p.is_msb_first());
        assert!(p
            .comm_mode_basic
            .contains(CommModeBasic::ADDRESS_GRANULARITY_WORD));
        assert_eq!(p.seed_and_key_external_function, "seedkey.dll");
        assert_eq!(p.optional_cmds, ["GET_DAQ_CLOCK", "SET_CAL_PAGE"]);
        assert!(p
            .comm_modes_supported
            .comm_mode
            .contains(XcpBlockMode::MASTER));
        assert_eq!(p.max_dto_stim, 0x20);
    }

    #[test]
    fn protocol_layer_byte_order_and_granularity_write() {
        let n = node_of(
            "/begin PROTOCOL_LAYER 0x100 0 0 0 0 0 0 0 0x8 0x20 \
                         BYTE_ORDER_MSB_LAST ADDRESS_GRANULARITY_BYTE /end PROTOCOL_LAYER",
        );
        let mut w = Writer::new(WriterOptions::default());
        n.write_block(&mut w, false).unwrap();
        let text = w.into_string();
        assert!(text.contains("BYTE_ORDER_MSB_LAST"), "{text}");
        assert!(text.contains("ADDRESS_GRANULARITY_BYTE"), "{text}");
        let XcpNode::ProtocolLayer(p) = node_of(&text) else {
            panic!("wrong node")
        };
        assert!(!p.is_msb_first());
    }

    #[test]
    fn sector_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin SECTOR \"sector 1\" 0x1 0x8000 0x4000 0x2 0x3 0x1 /end SECTOR",
        );
        let XcpNode::Sector(s) = n else {
            panic!("wrong node")
        };
        assert_eq!(s.name_long, "sector 1");
        assert_eq!(s.number, 1);
        assert_eq!(s.address, 0x8000);
        assert_eq!(s.length, 0x4000);
        assert_eq!((s.clear_seq_no, s.pgm_seq_no, s.pgm_method), (2, 3, 1));
    }

    #[test]
    fn segment_roundtrip() {
        let src = "/begin SEGMENT 0x0 0x2 0x0 0x0 0x0 PGM_VERIFY 0x12345678 DEFAULT_PAGE_NUMBER 1 \
                   /begin PAGE 0x0 ECU_ACCESS_DONT_CARE XCP_READ_ACCESS_DONT_CARE \
                   XCP_WRITE_ACCESS_DONT_CARE /end PAGE \
                   /begin CHECKSUM XCP_CRC_32 MAX_BLOCK_SIZE 0x100 /end CHECKSUM \
                   /end SEGMENT";
        let n = assert_node_roundtrip(src);
        let XcpNode::Segment(s) = n else {
            panic!("wrong node")
        };
        assert_eq!(s.segment_no, 0);
        assert_eq!(s.no_of_pages, 2);
        assert_eq!(s.pgm_verify, 0x12345678);
        assert_eq!(s.default_page_number, 1);
        assert_eq!(s.children.len(), 2);
    }

    #[test]
    fn stim_roundtrip() {
        let src = "/begin STIM GRANULARITY_ODT_ENTRY_SIZE_STIM_WORD 0x4 BIT_STIM_SUPPORTED MIN_ST_STIM 10 /end STIM";
        let n = assert_node_roundtrip(src);
        let XcpNode::Stim(s) = n else {
            panic!("wrong node")
        };
        assert_eq!(s.odt_entry_size, XcpOdtEntrySize::WORD);
        assert_eq!(s.max_odt_entry_size, 4);
        assert!(s.bit_stim_supported);
        assert_eq!(s.min_st_stim, 10);
    }

    #[test]
    fn time_correlation_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin TIME_CORRELATION DAQ_TIMESTAMPS_RELATE_TO ECU_CLOCK /end TIME_CORRELATION",
        );
        let XcpNode::TimeCorrelation(t) = n else {
            panic!("wrong node")
        };
        assert_eq!(t.timestamps_relate_to, XcpTsRelation::ECU_CLOCK);
    }

    #[test]
    fn timestamp_characterization_roundtrip() {
        let n = assert_node_roundtrip(
            "/begin TIMESTAMP_CHARACTERIZATION 1000 UNIT_1US SIZE_FOUR_BYTE /end TIMESTAMP_CHARACTERIZATION",
        );
        let XcpNode::TimestampCharacterization(t) = n else {
            panic!("wrong node")
        };
        assert_eq!(t.timestamp_ticks, 1000);
        assert_eq!(t.resolution, XcpTimestampResolution::_1US);
        assert_eq!(t.size, XcpNativeTimestampSize::FOUR_BYTE);
    }

    #[test]
    fn timestamp_supported_roundtrip() {
        let n = assert_node_roundtrip("/begin TIMESTAMP_SUPPORTED 0x10 SIZE_WORD UNIT_10US TIMESTAMP_FIXED /end TIMESTAMP_SUPPORTED");
        let XcpNode::TimestampSupported(t) = n else {
            panic!("wrong node")
        };
        assert_eq!(t.ticks, 0x10);
        assert_eq!(t.size, XcpTimestampSize::WORD);
        assert_eq!(t.resolution, XcpTimestampResolution::_10US);
        assert!(t.is_fixed);

        let n = assert_node_roundtrip(
            "/begin TIMESTAMP_SUPPORTED 0x0 NO_TIME_STAMP UNIT_1MS /end TIMESTAMP_SUPPORTED",
        );
        let XcpNode::TimestampSupported(t) = n else {
            panic!("wrong node")
        };
        assert_eq!(t.size, XcpTimestampSize::NotSet);
        assert!(!t.is_fixed);
    }

    #[test]
    fn ecu_states_generic_container_roundtrip() {
        let src = "/begin ECU_STATES \
                   /begin STATE 1 \"run\" ACTIVE ACTIVE NOT_ACTIVE NOT_ACTIVE /end STATE \
                   /begin MEMORY_ACCESS READ_ACCESS_ALLOWED WRITE_ACCESS_NOT_ALLOWED /end MEMORY_ACCESS \
                   /end ECU_STATES";
        let n = assert_node_roundtrip(src);
        let XcpNode::Generic(g) = n else {
            panic!("wrong node")
        };
        assert_eq!(g.keyword, "ECU_STATES");
        assert_eq!(g.children.len(), 2);
        let XcpNode::EcuStatesState(s) = &g.children[0] else {
            panic!("wrong child")
        };
        assert_eq!(s.state_number, 1);
        assert_eq!(s.state_name, "run");
        assert_eq!(s.cal_pag, XcpResourceState::ACTIVE);
        assert_eq!(s.pgm, XcpResourceState::NOT_ACTIVE);
        let XcpNode::EcuStatesMemoryAccess(m) = &g.children[1] else {
            panic!("wrong child")
        };
        assert_eq!(m.read_access, XcpMemoryAccess::ALLOWED);
        assert_eq!(m.write_access, XcpMemoryAccess::NOT_ALLOWED);
    }

    #[test]
    fn unsupported_block_passthrough() {
        let src = "/begin IF_DATA XCP /begin FOO_BAR 1 \"x\" /end FOO_BAR /end IF_DATA";
        let d = ifdata_of(src);
        let XcpNode::Unsupported(u) = &d.children[0] else {
            panic!("wrong node")
        };
        assert_eq!(u.keyword, "FOO_BAR");
        let text = d.write_string().unwrap();
        assert!(text.contains("/begin FOO_BAR"), "{text}");
        assert!(text.contains("1 \"x\""), "{text}");
    }

    // ------------------------------------------------------------------------
    // ------------------------------------------------------------------------

    #[test]
    fn ifdata_rejects_non_xcp() {
        assert!(XcpIfData::parse(&block_of("/begin IF_DATA CCP 0x1 /end IF_DATA")).is_err());
        assert!(XcpIfData::parse(&block_of("/begin MODULE M /end MODULE")).is_err());
    }

    #[test]
    fn ifdata_full_roundtrip() {
        let src = "/begin IF_DATA XCP \
            /begin PROTOCOL_LAYER 0x101 0 0 0 0 0 0 0 0x8 0xFF \
                BYTE_ORDER_MSB_LAST ADDRESS_GRANULARITY_BYTE \
            /end PROTOCOL_LAYER \
            /begin DAQ DYNAMIC 0x20 0x10 0 OPTIMISATION_TYPE_DEFAULT ADDRESS_EXTENSION_FREE \
                IDENTIFICATION_FIELD_TYPE_ABSOLUTE GRANULARITY_ODT_ENTRY_SIZE_DAQ_BYTE 0x7 \
                OVERLOAD_INDICATION_PID \
                /begin PREDEFINED \
                    /begin DAQ_LIST 0 DAQ_LIST_TYPE DAQ MAX_ODT 2 MAX_ODT_ENTRIES 3 EVENT_FIXED 0 /end DAQ_LIST \
                /end PREDEFINED \
                /begin EVENT \"10ms\" \"t10\" 0 DAQ 1 10 6 0 /end EVENT \
                /begin TIMESTAMP_SUPPORTED 0x1 SIZE_DWORD UNIT_1MS TIMESTAMP_FIXED /end TIMESTAMP_SUPPORTED \
            /end DAQ \
            /begin XCP_ON_CAN 0x101 CAN_ID_MASTER 0x667 CAN_ID_SLAVE 0x668 BAUDRATE 500000 /end XCP_ON_CAN \
            /end IF_DATA";
        let d = assert_ifdata_roundtrip(src);
        assert_eq!(d.name, "XCP");
        assert!(!d.is_xcp_plus());
        assert_eq!(d.children.len(), 3);
        assert!(matches!(
            d.child("PROTOCOL_LAYER"),
            Some(XcpNode::ProtocolLayer(_))
        ));
        assert!(matches!(d.media(), Some(XcpNode::OnCan(_))));
        let XcpNode::Daq(daq) = d.child("DAQ").unwrap() else {
            panic!("wrong node")
        };
        let XcpNode::Generic(pre) = &daq.children[0] else {
            panic!("wrong node")
        };
        assert_eq!(pre.keyword, "PREDEFINED");
        assert!(matches!(pre.children[0], XcpNode::DaqList(_)));
    }

    #[test]
    fn ifdata_xcpplus_event_fields_roundtrip() {
        let src = "/begin IF_DATA XCPplus \
            /begin EVENT \"a\" \"b\" 1 DAQ 1 1 3 0 EVENT_COUNTER_PRESENT CPU_LOAD_MAX 5 /end EVENT \
            /end IF_DATA";
        let d = assert_ifdata_roundtrip(src);
        assert!(d.is_xcp_plus());
        let text = d.write_string().unwrap();
        assert!(text.contains("EVENT_COUNTER_PRESENT"), "{text}");
        assert!(text.contains("CPU_LOAD_MAX 5"), "{text}");
    }

    #[test]
    fn daq_alternating_and_event_fixed_defaults() {
        let n = node_of(
            "/begin DAQ_LIST 1 DAQ_LIST_TYPE DAQ MAX_ODT 1 MAX_ODT_ENTRIES 1 /end DAQ_LIST",
        );
        let mut w = Writer::new(WriterOptions::default());
        n.write_block(&mut w, false).unwrap();
        let text = w.into_string();
        assert!(!text.contains("EVENT_FIXED"), "{text}");
        assert!(!text.contains("FIRST_PID"), "{text}");
    }
}
