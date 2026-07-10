//! Typed parser and writer for KWP2000 `IF_DATA` blocks.
//! Known transport, timing, data-access, and security sections are represented
//! by dedicated types. Unknown sections remain token-preserving
//! [`UnsupportedNode`] values so they can be written back without data loss.

use std::fmt;

use autors_a2l::block::{Block, Item};
use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::enums::A2lKeyword;
use autors_a2l::model::unsupported::UnsupportedNode;
use autors_a2l::params::ParamCursor;
use autors_a2l::token::Token;
use autors_a2l::writer::Writer;

use crate::error::{Error, Result};

pub const PROTOCOL_KWP: &str = "ASAP1B_KWP2000";

// ============================================================================
// ============================================================================

fn to_hex<T: fmt::UpperHex>(v: T) -> String {
    format!("0x{v:X}")
}

fn kw_enum<T: A2lKeyword + Default>(s: &str) -> T {
    T::from_keyword(s).unwrap_or_default()
}

fn u8_trunc(cur: &mut ParamCursor) -> Result<u8> {
    Ok(cur.uint::<u64>()? as u8)
}

fn u16_trunc(cur: &mut ParamCursor) -> Result<u16> {
    Ok(cur.uint::<u64>()? as u16)
}

fn u32_trunc(cur: &mut ParamCursor) -> Result<u32> {
    Ok(cur.uint::<u64>()? as u32)
}

fn raw_text(cur: &mut ParamCursor) -> Result<String> {
    Ok(cur.next_token()?.text.clone())
}

fn bool_str(v: bool) -> &'static str {
    if v {
        "1"
    } else {
        "0"
    }
}

fn parse_bool(t: &Token) -> bool {
    t.text == "1"
}

fn write_array_hex(w: &mut Writer, tag: &str, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let mut s = String::from(tag);
    for b in data {
        s.push_str(&format!(" {}", to_hex(*b)));
    }
    w.value_line(None, &s);
}

fn write_array_dec(w: &mut Writer, tag: &str, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let mut s = String::from(tag);
    for b in data {
        s.push_str(&format!(" {b}"));
    }
    w.value_line(None, &s);
}

fn byte_order_name(bo: ByteOrder) -> &'static str {
    match bo {
        ByteOrder::MSB_FIRST => "MSB_FIRST",
        ByteOrder::MSB_LAST => "MSB_LAST",
        ByteOrder::NotSet => "NotSet",
    }
}

fn byte_order_parse(s: &str) -> ByteOrder {
    if s.eq_ignore_ascii_case("MSB_FIRST") || s.eq_ignore_ascii_case("BIG_ENDIAN") {
        ByteOrder::MSB_FIRST
    } else if s.eq_ignore_ascii_case("MSB_LAST") || s.eq_ignore_ascii_case("LITTLE_ENDIAN") {
        ByteOrder::MSB_LAST
    } else {
        match s.parse::<u8>() {
            Ok(1) => ByteOrder::MSB_FIRST,
            Ok(2) => ByteOrder::MSB_LAST,
            _ => ByteOrder::NotSet,
        }
    }
}

fn stim_write_token(mode: KwpStimulationMode) -> &'static str {
    match mode {
        KwpStimulationMode::_WuP => "WuP",
        KwpStimulationMode::_5Baud => "5Baud",
        KwpStimulationMode::NotSet => "otSet",
    }
}

fn stim_parse_prefixed(token: &str) -> KwpStimulationMode {
    kw_enum(&format!("_{token}"))
}

fn tokens_block(toks: &[&Token]) -> Block {
    Block {
        keyword: "IF_DATA".to_string(),
        line: toks.first().map_or(0, |t| t.line),
        items: toks.iter().map(|t| Item::Param((*t).clone())).collect(),
    }
}

// ============================================================================
// ============================================================================

macro_rules! kwp_enum {
    ($(#[$meta:meta])* $name:ident { $def:ident $(, $rest:ident)* $(,)? }) => {
        $(#[$meta])*
        #[allow(non_camel_case_types)]
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            #[default]
            $def,
            $($rest),*
        }

        impl A2lKeyword for $name {
            fn as_keyword(self) -> Option<&'static str> {
                match self {
                    $name::$def => Some(stringify!($def)),
                    $($name::$rest => Some(stringify!($rest))),*
                }
            }

            fn from_keyword(kw: &str) -> Option<Self> {
                if kw.eq_ignore_ascii_case(stringify!($def)) {
                    return Some($name::$def);
                }
                $(if kw.eq_ignore_ascii_case(stringify!($rest)) {
                    return Some($name::$rest);
                })*
                None
            }
        }
    };
}

kwp_enum! {
    KwpAddressLocation { INTERN, EXTERN }
}

kwp_enum! {
    KwpCopyMode { RAM_InitByECU, RAM_InitByTool }
}

kwp_enum! {
    KwpFlashMode { NOFLASHBACK, AUTOFLASHBACK, TOOLFLASHBACK }
}

kwp_enum! {
    KwpFlashResult { RequestRoutineResults, StartRoutine, CodedResult }
}

kwp_enum! {
    KwpMeasurementMode { ADDRESSMODE, BLOCKMODE }
}

kwp_enum! {
    KwpPageSwitchMode { ESCAPE_CODE, LOCAL_ROUTINE }
}

kwp_enum! {
    KwpPhysicalLayer { NotSet, KLINE, CAN, KLINE_CAN, KLINE_AND_CAN }
}

kwp_enum! {
    KwpStimulationMode { NotSet, _WuP, _5Baud }
}

kwp_enum! {
    KwpVersion { NotSet, VDA_1996 }
}

fn physical_layer_parse(token: &str) -> KwpPhysicalLayer {
    kw_enum(&token.replace('+', "_"))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KwpDataAccessFlags(pub u8);

impl KwpDataAccessFlags {
    pub const READ_DATA: u8 = 0x1;
    pub const VERIFY_CODE: u8 = 0x2;
    pub const READ_CODE: u8 = 0x4;
    pub const RW_ONLY_ON_ACTIVE_PAGE: u8 = 0x8;

    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpAddress {
    pub can_id_ecu: u32,
    pub can_id_tester: u32,
}

impl KwpAddress {
    pub const KEYWORD: &'static str = "ADDRESS";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(KwpAddress {
            can_id_ecu: u32_trunc(&mut cur)?,
            can_id_tester: u32_trunc(&mut cur)?,
        })
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &to_hex(self.can_id_ecu));
        w.value_line(None, &to_hex(self.can_id_tester));
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpNetworkLimits {
    pub wft_max: u8,
    pub xdl_max: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpCan {
    pub baudrate: u32,
    pub sample_points: u8,
    pub sample_count: u8,
    pub btl_cycles: u8,
    pub sjw_length: u8,
    pub sync_edge: u8,
    pub network_limits: Option<KwpNetworkLimits>,
    pub start_stop_routine_no: u16,
    pub address: Option<Box<KwpAddress>>,
}

impl KwpCan {
    pub const KEYWORD: &'static str = "CAN";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut c = KwpCan {
            baudrate: u32_trunc(&mut cur)?,
            sample_points: u8_trunc(&mut cur)?,
            sample_count: u8_trunc(&mut cur)?,
            btl_cycles: u8_trunc(&mut cur)?,
            sjw_length: u8_trunc(&mut cur)?,
            sync_edge: u8_trunc(&mut cur)?,
            ..KwpCan::default()
        };
        while !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("NETWORK_LIMITS") {
                c.network_limits = Some(KwpNetworkLimits {
                    wft_max: u8_trunc(&mut cur)?,
                    xdl_max: u16_trunc(&mut cur)?,
                });
            } else if t.text.eq_ignore_ascii_case("START_STOP") {
                c.start_stop_routine_no = u16_trunc(&mut cur)?;
            }
        }
        if let Some(addr) = block.child(KwpAddress::KEYWORD) {
            c.address = Some(Box::new(KwpAddress::parse(addr)?));
        }
        Ok(c)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &self.baudrate.to_string());
        w.value_line(None, &self.sample_points.to_string());
        w.value_line(None, &self.sample_count.to_string());
        w.value_line(None, &self.btl_cycles.to_string());
        w.value_line(None, &self.sjw_length.to_string());
        w.value_line(None, &self.sync_edge.to_string());
        if let Some(addr) = &self.address {
            addr.write_block(w)?;
        }
        if let Some(nl) = &self.network_limits {
            w.value_line(None, "NETWORK_LIMITS");
            w.value_line(None, &nl.wft_max.to_string());
            w.value_line(None, &to_hex(nl.xdl_max));
        }
        w.value_line(None, &format!("START_STOP\t{}", self.start_stop_routine_no));
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpChecksum {
    pub chk_sum_type: u32,
    pub only_on_active_page: bool,
    pub local_routine_no: u16,
    pub result: KwpFlashResult,
    pub rnc_result: Vec<u8>,
}

impl KwpChecksum {
    pub const KEYWORD: &'static str = "CHECKSUM";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut c = KwpChecksum {
            chk_sum_type: u32_trunc(&mut cur)?,
            only_on_active_page: parse_bool(cur.next_token()?),
            local_routine_no: u16_trunc(&mut cur)?,
            result: kw_enum(&cur.next_token()?.text),
            ..KwpChecksum::default()
        };
        let mut active = false;
        while !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("RNC_RESULT") {
                active = true;
            } else if active {
                c.rnc_result.push(
                    t.text
                        .parse::<u8>()
                        .or_else(|_| {
                            u64::from_str_radix(t.text.trim_start_matches("0x"), 16)
                                .map(|v| v as u8)
                        })
                        .map_err(|_| {
                            Error::Parse(format!(
                                "line {}: expected byte, got {:?}",
                                t.line, t.text
                            ))
                        })?,
                );
            }
        }
        Ok(c)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &to_hex(self.chk_sum_type));
        w.value_line(None, bool_str(self.only_on_active_page));
        w.value_line(None, &self.local_routine_no.to_string());
        w.value_line(
            None,
            self.result.as_keyword().unwrap_or("RequestRoutineResults"),
        );
        write_array_hex(w, "RNC_RESULT", &self.rnc_result);
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpCopy {
    pub flash_to_ram_mode: KwpCopyMode,
    pub flash_to_ram_diag_mode: u8,
    pub copy_para: Vec<u8>,
}

impl KwpCopy {
    pub const KEYWORD: &'static str = "COPY";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut c = KwpCopy {
            flash_to_ram_mode: kw_enum(&cur.next_token()?.text),
            flash_to_ram_diag_mode: u8_trunc(&mut cur)?,
            ..KwpCopy::default()
        };
        if cur.take_if("COPY_PARA") {
            while !cur.is_empty() {
                c.copy_para.push(parse_byte(cur.next_token()?)?);
            }
        }
        Ok(c)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(
            None,
            self.flash_to_ram_mode
                .as_keyword()
                .unwrap_or("RAM_InitByECU"),
        );
        w.value_line(None, &to_hex(self.flash_to_ram_diag_mode));
        write_array_hex(w, "COPY_PARA", &self.copy_para);
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpDiagBaud {
    pub baudrate: u32,
    pub diag_mode: u16,
    pub bd_para: Vec<u8>,
}

impl KwpDiagBaud {
    pub const KEYWORD: &'static str = "DIAG_BAUD";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut d = KwpDiagBaud {
            baudrate: u32_trunc(&mut cur)?,
            diag_mode: u16_trunc(&mut cur)?,
            ..KwpDiagBaud::default()
        };
        let mut active = false;
        while !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("BD_PARA") {
                active = true;
            } else if active {
                d.bd_para.push(parse_byte(t)?);
            }
        }
        Ok(d)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &self.baudrate.to_string());
        w.value_line(None, &to_hex(self.diag_mode));
        write_array_hex(w, "BD_PARA", &self.bd_para);
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KwpFlash {
    pub ram_to_flash_mode: KwpFlashMode,
    pub ram_to_flash_routine_no: u16,
    pub result: KwpFlashResult,
    pub copy_frame: Vec<u8>,
    pub rnc_result: Vec<u8>,
    pub copy_para: Vec<u8>,
}

impl Default for KwpFlash {
    fn default() -> Self {
        KwpFlash {
            ram_to_flash_mode: KwpFlashMode::TOOLFLASHBACK,
            ram_to_flash_routine_no: 0,
            result: KwpFlashResult::default(),
            copy_frame: Vec::new(),
            rnc_result: Vec::new(),
            copy_para: Vec::new(),
        }
    }
}

impl KwpFlash {
    pub const KEYWORD: &'static str = "FLASH";

    /// `parse_flash_arrays`).
    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut f = KwpFlash {
            ram_to_flash_mode: kw_enum(&cur.next_token()?.text),
            ram_to_flash_routine_no: u16_trunc(&mut cur)?,
            result: kw_enum(&cur.next_token()?.text),
            ..KwpFlash::default()
        };
        parse_flash_arrays(&mut cur, &mut f)?;
        Ok(f)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(
            None,
            self.ram_to_flash_mode.as_keyword().unwrap_or("NOFLASHBACK"),
        );
        w.value_line(None, &self.ram_to_flash_routine_no.to_string());
        w.value_line(
            None,
            self.result.as_keyword().unwrap_or("RequestRoutineResults"),
        );
        write_array_dec(w, "COPY_FRAME", &self.copy_frame);
        write_array_hex(w, "RNC_RESULT", &self.rnc_result);
        write_array_hex(w, "COPY_PARA", &self.copy_para);
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpFlashCopy {
    pub flash: KwpFlash,
    pub flash_to_ram_mode: KwpCopyMode,
    pub flash_to_ram_diag_mode: u8,
}

impl KwpFlashCopy {
    pub const KEYWORD: &'static str = "FLASH_COPY";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut fc = KwpFlashCopy {
            flash: KwpFlash {
                ram_to_flash_mode: kw_enum(&cur.next_token()?.text),
                ram_to_flash_routine_no: u16_trunc(&mut cur)?,
                result: kw_enum(&cur.next_token()?.text),
                ..KwpFlash::default()
            },
            flash_to_ram_mode: kw_enum(&cur.next_token()?.text),
            flash_to_ram_diag_mode: u8_trunc(&mut cur)?,
        };
        parse_flash_arrays(&mut cur, &mut fc.flash)?;
        Ok(fc)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(
            None,
            self.flash
                .ram_to_flash_mode
                .as_keyword()
                .unwrap_or("NOFLASHBACK"),
        );
        w.value_line(None, &self.flash.ram_to_flash_routine_no.to_string());
        w.value_line(
            None,
            self.flash
                .result
                .as_keyword()
                .unwrap_or("RequestRoutineResults"),
        );
        w.value_line(
            None,
            self.flash_to_ram_mode
                .as_keyword()
                .unwrap_or("RAM_InitByECU"),
        );
        w.value_line(None, &to_hex(self.flash_to_ram_diag_mode));
        write_array_dec(w, "COPY_FRAME", &self.flash.copy_frame);
        write_array_hex(w, "RNC_RESULT", &self.flash.rnc_result);
        write_array_hex(w, "COPY_PARA", &self.flash.copy_para);
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

fn parse_flash_arrays(cur: &mut ParamCursor, f: &mut KwpFlash) -> Result<()> {
    // 0 = COPY_FRAME, 1 = RNC_RESULT, 2 = COPY_PARA
    let mut active: Option<usize> = None;
    while !cur.is_empty() {
        let t = cur.next_token()?;
        if t.text.eq_ignore_ascii_case("COPY_FRAME") {
            active = Some(0);
        } else if t.text.eq_ignore_ascii_case("RNC_RESULT") {
            active = Some(1);
        } else if t.text.eq_ignore_ascii_case("COPY_PARA") {
            active = Some(2);
        } else if let Some(i) = active {
            let b = parse_byte(t)?;
            match i {
                0 => f.copy_frame.push(b),
                1 => f.rnc_result.push(b),
                _ => f.copy_para.push(b),
            }
        }
    }
    Ok(())
}

fn parse_byte(t: &Token) -> Result<u8> {
    if let Some(hex) = t
        .text
        .strip_prefix("0x")
        .or_else(|| t.text.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).map(|v| v as u8)
    } else {
        t.text.parse::<u64>().map(|v| v as u8)
    }
    .map_err(|_| Error::Parse(format!("line {}: expected byte, got {:?}", t.line, t.text)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KwpKLine {
    pub stimulation_mode: KwpStimulationMode,
    pub ecu_address: u16,
    pub tester_address: u16,
}

impl Default for KwpKLine {
    fn default() -> Self {
        KwpKLine {
            stimulation_mode: KwpStimulationMode::_WuP,
            ecu_address: 0,
            tester_address: 0,
        }
    }
}

impl KwpKLine {
    pub const KEYWORD: &'static str = "K_LINE";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        Ok(KwpKLine {
            stimulation_mode: stim_parse_prefixed(&cur.next_token()?.text),
            ecu_address: u16_trunc(&mut cur)?,
            tester_address: u16_trunc(&mut cur)?,
        })
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, stim_write_token(self.stimulation_mode));
        w.value_line(None, &to_hex(self.ecu_address));
        w.value_line(None, &to_hex(self.tester_address));
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpPageSwitch {
    pub mode: KwpPageSwitchMode,
    pub escape_code_para_set: Vec<u8>,
    pub escape_code_para_get: Vec<u8>,
    pub page_code: Vec<u8>,
}

impl KwpPageSwitch {
    pub const KEYWORD: &'static str = "PAGE_SWITCH";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut p = KwpPageSwitch {
            mode: kw_enum(&cur.next_token()?.text),
            ..KwpPageSwitch::default()
        };
        let mut active: Option<usize> = None;
        while !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("ESCAPE_CODE_PARA_GET") {
                active = Some(0);
            } else if t.text.eq_ignore_ascii_case("ESCAPE_CODE_PARA_SET") {
                active = Some(1);
            } else if t.text.eq_ignore_ascii_case("PAGE_CODE") {
                active = Some(2);
            } else if let Some(i) = active {
                let b = parse_byte(t)?;
                match i {
                    0 => p.escape_code_para_get.push(b),
                    1 => p.escape_code_para_set.push(b),
                    _ => p.page_code.push(b),
                }
            }
        }
        Ok(p)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, self.mode.as_keyword().unwrap_or("ESCAPE_CODE"));
        write_array_hex(w, "PAGE_CODE", &self.page_code);
        write_array_hex(w, "ESCAPE_CODE_PARA_SET", &self.escape_code_para_set);
        write_array_hex(w, "ESCAPE_CODE_PARA_GET", &self.escape_code_para_get);
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpRoutinePara {
    pub result: KwpFlashResult,
    pub local_routine_no: u16,
    pub rnc_result: Vec<u8>,
}

impl KwpRoutinePara {
    pub const KEYWORD: &'static str = "ROUTINE_PARA";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut r = KwpRoutinePara {
            result: kw_enum(&cur.next_token()?.text),
            local_routine_no: u16_trunc(&mut cur)?,
            ..KwpRoutinePara::default()
        };
        let mut active = false;
        while !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("RNC_RESULT") {
                active = true;
            } else if active {
                r.rnc_result.push(parse_byte(t)?);
            }
        }
        Ok(r)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        let kw = self.result.as_keyword().unwrap_or("RequestRoutineResults");
        w.value_line(None, kw);
        w.value_line(None, &self.local_routine_no.to_string());
        w.value_line(None, kw);
        write_array_hex(w, "RNC_RESULT", &self.rnc_result);
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpSourceQpBlob {
    pub physical_layer: KwpPhysicalLayer,
    pub no_of_samplings: u16,
    pub measurement_mode: KwpMeasurementMode,
    pub block_mode_id: u16,
    pub max_sampling_rate: u16,
    pub max_no_of_signals: u16,
    pub max_no_of_bytes: u16,
    pub can_id: u32,
    pub discriminator: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpSource {
    pub name_long: String,
    pub cse_unit: i16,
    pub rate: i32,
    pub qp_blob: Option<KwpSourceQpBlob>,
}

impl KwpSource {
    pub const KEYWORD: &'static str = "SOURCE";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut s = KwpSource {
            name_long: raw_text(&mut cur)?,
            cse_unit: cur.int::<i16>()?,
            rate: cur.int::<i32>()?,
            ..KwpSource::default()
        };
        if !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("QP_BLOB") {
                let first = cur.next_token()?;
                let layer = physical_layer_parse(&first.text);
                let mut qp = KwpSourceQpBlob {
                    physical_layer: layer,
                    ..KwpSourceQpBlob::default()
                };
                if layer > KwpPhysicalLayer::NotSet {
                    qp.measurement_mode = kw_enum(&cur.next_token()?.text);
                    qp.block_mode_id = u16_trunc(&mut cur)?;
                    qp.max_no_of_signals = u16_trunc(&mut cur)?;
                    qp.max_no_of_bytes = u16_trunc(&mut cur)?;
                    qp.can_id = u32_trunc(&mut cur)?;
                    qp.discriminator = u16_trunc(&mut cur)?;
                } else {
                    qp.no_of_samplings = first
                        .text
                        .parse::<u64>()
                        .map(|v| v as u16)
                        .or_else(|_| {
                            u64::from_str_radix(first.text.trim_start_matches("0x"), 16)
                                .map(|v| v as u16)
                        })
                        .map_err(|_| {
                            Error::Parse(format!(
                                "line {}: expected unsigned integer, got {:?}",
                                first.line, first.text
                            ))
                        })?;
                    qp.measurement_mode = kw_enum(&cur.next_token()?.text);
                    qp.block_mode_id = u16_trunc(&mut cur)?;
                    qp.max_sampling_rate = u16_trunc(&mut cur)?;
                    qp.max_no_of_signals = u16_trunc(&mut cur)?;
                }
                s.qp_blob = Some(qp);
            }
        }
        Ok(s)
    }

    pub fn write_body(&self, w: &mut Writer, tp_version: Option<KwpVersion>) -> Result<()> {
        w.tag_value(None, Some(&self.name_long), true);
        w.value_line(None, &self.cse_unit.to_string());
        w.value_line(None, &self.rate.to_string());
        if let Some(qp) = &self.qp_blob {
            let version = tp_version.ok_or_else(|| {
                Error::Protocol("KWP SOURCE with QP_BLOB requires sibling TP_BLOB".to_string())
            })?;
            w.value_line(None, "QP_BLOB");
            match version {
                KwpVersion::VDA_1996 => {
                    w.value_line(None, qp.physical_layer.as_keyword().unwrap_or("NotSet"));
                    w.value_line(
                        None,
                        qp.measurement_mode.as_keyword().unwrap_or("ADDRESSMODE"),
                    );
                    w.value_line(None, &to_hex(qp.block_mode_id));
                    w.value_line(None, &qp.max_no_of_signals.to_string());
                    w.value_line(None, &qp.max_no_of_bytes.to_string());
                    w.value_line(None, &qp.can_id.to_string());
                    w.value_line(None, &qp.discriminator.to_string());
                }
                KwpVersion::NotSet => {
                    w.value_line(None, &qp.no_of_samplings.to_string());
                    w.value_line(
                        None,
                        qp.measurement_mode.as_keyword().unwrap_or("ADDRESSMODE"),
                    );
                    w.value_line(None, &to_hex(qp.block_mode_id));
                    w.value_line(None, &qp.max_sampling_rate.to_string());
                    w.value_line(None, &qp.max_no_of_signals.to_string());
                }
            }
        }
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer, tp_version: Option<KwpVersion>) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w, tp_version)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpTimeDef {
    pub p1_max: u16,
    pub p2_min: u16,
    pub p2_max: u16,
    pub p3_min: u16,
    pub p3_max: u16,
    pub p4_min: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpUsdtpTiming {
    pub as_: u16,
    pub bs: u16,
    pub cd: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpTimeDefNode {
    pub time_defs: Vec<KwpTimeDef>,
    pub usdtp_timings: Vec<KwpUsdtpTiming>,
}

impl KwpTimeDefNode {
    pub const KEYWORD: &'static str = "TIME_DEF";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut n = KwpTimeDefNode::default();
        while !cur.is_empty() {
            let t = cur.next_token()?;
            if t.text.eq_ignore_ascii_case("KWP_TIMING") {
                n.time_defs.push(KwpTimeDef {
                    p1_max: u16_trunc(&mut cur)?,
                    p2_min: u16_trunc(&mut cur)?,
                    p2_max: u16_trunc(&mut cur)?,
                    p3_min: u16_trunc(&mut cur)?,
                    p3_max: u16_trunc(&mut cur)?,
                    p4_min: u16_trunc(&mut cur)?,
                });
            } else if t.text.eq_ignore_ascii_case("USDTP_TIMING") {
                n.usdtp_timings.push(KwpUsdtpTiming {
                    as_: u16_trunc(&mut cur)?,
                    bs: u16_trunc(&mut cur)?,
                    cd: u16_trunc(&mut cur)?,
                });
            }
        }
        Ok(n)
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        for td in &self.time_defs {
            w.value_line(
                None,
                &format!(
                    "KWP_TIMING\t0x{:04X}\t0x{:04X}\t0x{:04X}\t0x{:04X}\t0x{:04X}\t0x{:04X}",
                    td.p1_max, td.p2_min, td.p2_max, td.p3_min, td.p3_max, td.p4_min
                ),
            );
        }
        for ut in &self.usdtp_timings {
            w.value_line(
                None,
                &format!(
                    "USDTP_TIMING\t0x{:04X}\t0x{:04X}\t0x{:04X}",
                    ut.as_, ut.bs, ut.cd
                ),
            );
        }
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpBaudDef {
    pub baud_rate: u32,
    pub diag_mode: u16,
    pub ik: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpDataAccess {
    pub adr_flash: u32,
    pub adr_ram: u32,
    pub flags: KwpDataAccessFlags,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpTpKLine {
    pub stimulation_mode: KwpStimulationMode,
    pub ecu_address: u16,
    pub tester_address: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpSeram {
    pub a: u32,
    pub o: u32,
    pub u: u32,
    pub e: u32,
    pub adr_flash: u32,
    pub adr_ram: u32,
    pub flags: KwpDataAccessFlags,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpSecurityAccess {
    pub access_mode: u16,
    pub calc_mode: u16,
    pub delaytime: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KwpTpBlob {
    pub version: u16,
    pub ecu_address: u16,
    pub tester_address: u16,
    pub byte_order: ByteOrder,
    pub stimulation_mode: KwpStimulationMode,
    pub eversion: KwpVersion,
    pub start_diag_without_bd_switch: bool,
    pub project_base_address: u32,
    pub seram: Option<KwpSeram>,
    pub data_access: Option<KwpDataAccess>,
    pub k_line: Option<KwpTpKLine>,
    pub baud_defs: Vec<KwpBaudDef>,
    pub time_defs: Vec<KwpTimeDef>,
    pub security_accesses: Vec<KwpSecurityAccess>,
    pub can: Option<Box<KwpCan>>,
}

impl Default for KwpTpBlob {
    fn default() -> Self {
        KwpTpBlob {
            version: 0,
            ecu_address: 0,
            tester_address: 0,
            byte_order: ByteOrder::default(),
            stimulation_mode: KwpStimulationMode::_WuP,
            eversion: KwpVersion::default(),
            start_diag_without_bd_switch: false,
            project_base_address: 0,
            seram: None,
            data_access: None,
            k_line: None,
            baud_defs: Vec::new(),
            time_defs: Vec::new(),
            security_accesses: Vec::new(),
            can: None,
        }
    }
}

impl KwpTpBlob {
    pub const KEYWORD: &'static str = "TP_BLOB";

    pub fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut tp = KwpTpBlob {
            version: u16_trunc(&mut cur)?,
            ..KwpTpBlob::default()
        };
        let t = cur.next_token()?;
        if matches!(t.text.as_str(), "VDA_1996" | "NotSet") {
            tp.eversion = kw_enum(&t.text);
            tp.byte_order = byte_order_parse(&cur.next_token()?.text);
            while !cur.is_empty() {
                let t = cur.next_token()?;
                if t.text.eq_ignore_ascii_case("SECURITY_ACCESS") {
                    tp.security_accesses.push(KwpSecurityAccess {
                        access_mode: u16_trunc(&mut cur)?,
                        calc_mode: u16_trunc(&mut cur)?,
                        delaytime: u16_trunc(&mut cur)?,
                    });
                } else if t.text.eq_ignore_ascii_case("DATA_ACCESS") {
                    let mut da = KwpDataAccess {
                        adr_flash: u32_trunc(&mut cur)?,
                        adr_ram: u32_trunc(&mut cur)?,
                        ..KwpDataAccess::default()
                    };
                    da.flags.0 |= Self::flag_bit(&mut cur, KwpDataAccessFlags::READ_DATA)?;
                    da.flags.0 |= Self::flag_bit(&mut cur, KwpDataAccessFlags::VERIFY_CODE)?;
                    da.flags.0 |= Self::flag_bit(&mut cur, KwpDataAccessFlags::READ_CODE)?;
                    da.flags.0 |=
                        Self::flag_bit(&mut cur, KwpDataAccessFlags::RW_ONLY_ON_ACTIVE_PAGE)?;
                    tp.data_access = Some(da);
                } else if t.text.eq_ignore_ascii_case("K_LINE") {
                    let stim = cur.next_token()?;
                    let mode = if stim.text.eq_ignore_ascii_case("WuP") {
                        KwpStimulationMode::_WuP
                    } else if stim.text.eq_ignore_ascii_case("Stimulation_5Baud") {
                        KwpStimulationMode::_5Baud
                    } else {
                        KwpStimulationMode::NotSet
                    };
                    tp.k_line = Some(KwpTpKLine {
                        stimulation_mode: mode,
                        ecu_address: u16_trunc(&mut cur)?,
                        tester_address: u16_trunc(&mut cur)?,
                    });
                }
            }
        } else {
            tp.ecu_address = t
                .text
                .parse::<u64>()
                .map(|v| v as u16)
                .or_else(|_| {
                    u64::from_str_radix(t.text.trim_start_matches("0x"), 16).map(|v| v as u16)
                })
                .map_err(|_| {
                    Error::Parse(format!(
                        "line {}: expected unsigned integer, got {:?}",
                        t.line, t.text
                    ))
                })?;
            tp.tester_address = u16_trunc(&mut cur)?;
            tp.stimulation_mode = KwpStimulationMode::_WuP;
            let _stim = cur.next_token()?;
            tp.byte_order = byte_order_parse(&cur.next_token()?.text);
            tp.start_diag_without_bd_switch = parse_bool(cur.next_token()?);
            tp.project_base_address = u32_trunc(&mut cur)?;
            while !cur.is_empty() {
                let t = cur.next_token()?;
                if t.text.eq_ignore_ascii_case("SERAM") {
                    let mut s = KwpSeram {
                        a: u32_trunc(&mut cur)?,
                        o: u32_trunc(&mut cur)?,
                        u: u32_trunc(&mut cur)?,
                        e: u32_trunc(&mut cur)?,
                        adr_flash: u32_trunc(&mut cur)?,
                        adr_ram: u32_trunc(&mut cur)?,
                        ..KwpSeram::default()
                    };
                    s.flags.0 |= Self::flag_bit(&mut cur, KwpDataAccessFlags::READ_DATA)?;
                    s.flags.0 |= Self::flag_bit(&mut cur, KwpDataAccessFlags::VERIFY_CODE)?;
                    s.flags.0 |= Self::flag_bit(&mut cur, KwpDataAccessFlags::READ_CODE)?;
                    s.flags.0 |=
                        Self::flag_bit(&mut cur, KwpDataAccessFlags::RW_ONLY_ON_ACTIVE_PAGE)?;
                    tp.seram = Some(s);
                } else if t.text.eq_ignore_ascii_case("BAUD_DEF") {
                    tp.baud_defs.push(KwpBaudDef {
                        baud_rate: u32_trunc(&mut cur)?,
                        diag_mode: u16_trunc(&mut cur)?,
                        ik: u32_trunc(&mut cur)?,
                    });
                } else if t.text.eq_ignore_ascii_case("TIME_DEF") {
                    tp.time_defs.push(KwpTimeDef {
                        p1_max: u16_trunc(&mut cur)?,
                        p2_min: u16_trunc(&mut cur)?,
                        p2_max: u16_trunc(&mut cur)?,
                        p3_min: u16_trunc(&mut cur)?,
                        p3_max: u16_trunc(&mut cur)?,
                        p4_min: u16_trunc(&mut cur)?,
                    });
                } else if t.text.eq_ignore_ascii_case("SECURITY_ACCESS") {
                    tp.security_accesses.push(KwpSecurityAccess {
                        access_mode: u16_trunc(&mut cur)?,
                        calc_mode: u16_trunc(&mut cur)?,
                        delaytime: u16_trunc(&mut cur)?,
                    });
                }
            }
        }
        if let Some(can) = block.child(KwpCan::KEYWORD) {
            tp.can = Some(Box::new(KwpCan::parse(can)?));
        }
        Ok(tp)
    }

    fn flag_bit(cur: &mut ParamCursor, bit: u8) -> Result<u8> {
        Ok(if parse_bool(cur.next_token()?) {
            bit
        } else {
            0
        })
    }

    pub fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &to_hex(self.version));
        match self.eversion {
            KwpVersion::NotSet => {
                w.value_line(None, &to_hex(self.ecu_address));
                w.value_line(None, &to_hex(self.tester_address));
                w.value_line(None, stim_write_token(self.stimulation_mode));
                w.value_line(None, byte_order_name(self.byte_order));
                w.value_line(None, bool_str(self.start_diag_without_bd_switch));
                w.value_line(None, &to_hex(self.project_base_address));
                if let Some(s) = &self.seram {
                    w.value_line(None, "SERAM");
                    w.value_line(None, &to_hex(s.a));
                    w.value_line(None, &to_hex(s.o));
                    w.value_line(None, &to_hex(s.u));
                    w.value_line(None, &to_hex(s.e));
                    w.value_line(None, &to_hex(s.adr_flash));
                    w.value_line(None, &to_hex(s.adr_ram));
                    w.value_line(
                        None,
                        bool_str(s.flags.contains(KwpDataAccessFlags::READ_DATA)),
                    );
                    w.value_line(
                        None,
                        bool_str(s.flags.contains(KwpDataAccessFlags::VERIFY_CODE)),
                    );
                    w.value_line(
                        None,
                        bool_str(s.flags.contains(KwpDataAccessFlags::READ_CODE)),
                    );
                    w.value_line(
                        None,
                        bool_str(s.flags.contains(KwpDataAccessFlags::RW_ONLY_ON_ACTIVE_PAGE)),
                    );
                }
                for bd in &self.baud_defs {
                    w.value_line(None, "BAUD_DEF");
                    w.value_line(None, &bd.baud_rate.to_string());
                    w.value_line(None, &to_hex(bd.diag_mode));
                    w.value_line(None, &to_hex(bd.ik));
                }
                for td in &self.time_defs {
                    w.value_line(None, "TIME_DEF");
                    w.value_line(None, &to_hex(td.p1_max));
                    w.value_line(None, &to_hex(td.p2_min));
                    w.value_line(None, &to_hex(td.p2_max));
                    w.value_line(None, &to_hex(td.p3_min));
                    w.value_line(None, &to_hex(td.p3_max));
                    w.value_line(None, &to_hex(td.p4_min));
                }
            }
            KwpVersion::VDA_1996 => {
                w.value_line(None, "VDA_1996");
                w.value_line(None, byte_order_name(self.byte_order));
                if let Some(kl) = &self.k_line {
                    w.value_line(None, "K_LINE");
                    match kl.stimulation_mode {
                        KwpStimulationMode::_WuP => w.value_line(None, "WuP"),
                        KwpStimulationMode::_5Baud => w.value_line(None, "Stimulation_5Baud"),
                        KwpStimulationMode::NotSet => {}
                    }
                    w.value_line(None, &to_hex(kl.ecu_address));
                    w.value_line(None, &to_hex(kl.tester_address));
                }
                if let Some(can) = &self.can {
                    can.write_block(w)?;
                }
                if let Some(da) = &self.data_access {
                    w.value_line(None, "DATA_ACCESS");
                    w.value_line(None, &to_hex(da.adr_flash));
                    w.value_line(None, &to_hex(da.adr_ram));
                    w.value_line(
                        None,
                        bool_str(da.flags.contains(KwpDataAccessFlags::READ_DATA)),
                    );
                    w.value_line(
                        None,
                        bool_str(da.flags.contains(KwpDataAccessFlags::VERIFY_CODE)),
                    );
                    w.value_line(
                        None,
                        bool_str(da.flags.contains(KwpDataAccessFlags::READ_CODE)),
                    );
                    w.value_line(
                        None,
                        bool_str(
                            da.flags
                                .contains(KwpDataAccessFlags::RW_ONLY_ON_ACTIVE_PAGE),
                        ),
                    );
                }
            }
        }
        for sa in &self.security_accesses {
            w.value_line(None, "SECURITY_ACCESS");
            w.value_line(None, &sa.access_mode.to_string());
            w.value_line(None, &sa.calc_mode.to_string());
            w.value_line(None, &sa.delaytime.to_string());
        }
        Ok(())
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

impl fmt::Display for KwpTpBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KWP, Version: {:04X}", self.version)
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpAddressMapping {
    pub base_address: u32,
    pub map_address: u32,
    pub length: u32,
}

impl KwpAddressMapping {
    pub const KEYWORD: &'static str = "ADDRESS_MAPPING";

    fn parse_tokens(toks: &[&Token]) -> Result<Self> {
        let block = tokens_block(toks);
        let mut cur = ParamCursor::new(&block);
        Ok(KwpAddressMapping {
            base_address: u32_trunc(&mut cur)?,
            map_address: u32_trunc(&mut cur)?,
            length: u32_trunc(&mut cur)?,
        })
    }

    fn inline_params(&self) -> String {
        format!(
            " {} {} {} ",
            to_hex(self.base_address),
            to_hex(self.map_address),
            to_hex(self.length)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpDpBlob {
    pub address: u32,
    pub length: u32,
}

impl KwpDpBlob {
    pub const KEYWORD: &'static str = "DP_BLOB";

    fn parse_tokens(toks: &[&Token]) -> Result<Self> {
        let block = tokens_block(toks);
        let mut cur = ParamCursor::new(&block);
        Ok(KwpDpBlob {
            address: u32_trunc(&mut cur)?,
            length: u32_trunc(&mut cur)?,
        })
    }

    fn inline_params(&self) -> String {
        format!(" {} {} ", to_hex(self.address), to_hex(self.length))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwpKpBlob {
    pub address: u32,
    pub location: KwpAddressLocation,
    pub size: u32,
}

impl KwpKpBlob {
    pub const KEYWORD: &'static str = "KP_BLOB";

    fn parse_tokens(toks: &[&Token]) -> Result<Self> {
        let block = tokens_block(toks);
        let mut cur = ParamCursor::new(&block);
        Ok(KwpKpBlob {
            address: u32_trunc(&mut cur)?,
            location: kw_enum(&cur.next_token()?.text),
            size: u32_trunc(&mut cur)?,
        })
    }

    fn inline_params(&self) -> String {
        format!(
            " {} {} {} ",
            to_hex(self.address),
            self.location.as_keyword().unwrap_or("INTERN"),
            self.size
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KwpNamedNode {
    /// `ADDRESS_MAPPING`.
    AddressMapping(KwpAddressMapping),
    /// `DP_BLOB`.
    DpBlob(KwpDpBlob),
    /// `KP_BLOB`.
    KpBlob(KwpKpBlob),
}

impl KwpNamedNode {
    pub fn sub_type_keyword(&self) -> &'static str {
        match self {
            KwpNamedNode::AddressMapping(_) => KwpAddressMapping::KEYWORD,
            KwpNamedNode::DpBlob(_) => KwpDpBlob::KEYWORD,
            KwpNamedNode::KpBlob(_) => KwpKpBlob::KEYWORD,
        }
    }

    pub fn name(&self) -> String {
        format!("{PROTOCOL_KWP} {}", self.sub_type_keyword())
    }

    pub fn write_inline(&self, w: &mut Writer) -> Result<()> {
        let params = match self {
            KwpNamedNode::AddressMapping(n) => n.inline_params(),
            KwpNamedNode::DpBlob(n) => n.inline_params(),
            KwpNamedNode::KpBlob(n) => n.inline_params(),
        };
        w.value_line(
            None,
            &format!(
                "/begin IF_DATA {} {}{}/end IF_DATA",
                PROTOCOL_KWP,
                self.sub_type_keyword(),
                params
            ),
        );
        Ok(())
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum KwpNode {
    /// `TP_BLOB`.
    TpBlob(KwpTpBlob),
    /// `SOURCE`.
    Source(KwpSource),
    /// `ROUTINE_PARA`.
    RoutinePara(KwpRoutinePara),
    /// `PAGE_SWITCH`.
    PageSwitch(KwpPageSwitch),
    /// `DIAG_BAUD`.
    DiagBaud(KwpDiagBaud),
    /// `CHECKSUM`.
    Checksum(KwpChecksum),
    /// `FLASH_COPY`.
    FlashCopy(KwpFlashCopy),
    /// `FLASH`.
    Flash(KwpFlash),
    /// `COPY`.
    Copy(KwpCopy),
    /// `TIME_DEF`.
    TimeDef(KwpTimeDefNode),
    Address(KwpAddress),
    Can(KwpCan),
    /// `K_LINE`.
    KLine(KwpKLine),
    Unsupported(UnsupportedNode),
}

impl KwpNode {
    pub fn parse(block: &Block) -> Result<Self> {
        Ok(match block.keyword.to_ascii_uppercase().as_str() {
            KwpTpBlob::KEYWORD => KwpNode::TpBlob(KwpTpBlob::parse(block)?),
            KwpSource::KEYWORD => KwpNode::Source(KwpSource::parse(block)?),
            KwpRoutinePara::KEYWORD => KwpNode::RoutinePara(KwpRoutinePara::parse(block)?),
            KwpPageSwitch::KEYWORD => KwpNode::PageSwitch(KwpPageSwitch::parse(block)?),
            KwpDiagBaud::KEYWORD => KwpNode::DiagBaud(KwpDiagBaud::parse(block)?),
            KwpChecksum::KEYWORD => KwpNode::Checksum(KwpChecksum::parse(block)?),
            KwpFlashCopy::KEYWORD => KwpNode::FlashCopy(KwpFlashCopy::parse(block)?),
            KwpFlash::KEYWORD => KwpNode::Flash(KwpFlash::parse(block)?),
            KwpCopy::KEYWORD => KwpNode::Copy(KwpCopy::parse(block)?),
            KwpTimeDefNode::KEYWORD => KwpNode::TimeDef(KwpTimeDefNode::parse(block)?),
            KwpAddress::KEYWORD => KwpNode::Address(KwpAddress::parse(block)?),
            KwpCan::KEYWORD => KwpNode::Can(KwpCan::parse(block)?),
            KwpKLine::KEYWORD => KwpNode::KLine(KwpKLine::parse(block)?),
            _ => KwpNode::Unsupported(UnsupportedNode::from_block(block)),
        })
    }

    /// [`KwpSource::write_body`]).
    pub fn write_block(&self, w: &mut Writer, tp_version: Option<KwpVersion>) -> Result<()> {
        match self {
            KwpNode::TpBlob(n) => n.write_block(w),
            KwpNode::Source(n) => n.write_block(w, tp_version),
            KwpNode::RoutinePara(n) => n.write_block(w),
            KwpNode::PageSwitch(n) => n.write_block(w),
            KwpNode::DiagBaud(n) => n.write_block(w),
            KwpNode::Checksum(n) => n.write_block(w),
            KwpNode::FlashCopy(n) => n.write_block(w),
            KwpNode::Flash(n) => n.write_block(w),
            KwpNode::Copy(n) => n.write_block(w),
            KwpNode::TimeDef(n) => n.write_block(w),
            KwpNode::Address(n) => n.write_block(w),
            KwpNode::Can(n) => n.write_block(w),
            KwpNode::KLine(n) => n.write_block(w),
            KwpNode::Unsupported(n) => Ok(n.write_block(w)?),
        }
    }

    fn filter_from_write(&self) -> bool {
        matches!(self, KwpNode::Address(_) | KwpNode::Can(_))
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum KwpIfData {
    Named(KwpNamedNode),
    Nodes {
        params: Vec<String>,
        nodes: Vec<KwpNode>,
    },
    Unsupported(UnsupportedNode),
}

impl KwpIfData {
    pub fn parse(block: &Block) -> Result<Self> {
        if !block.keyword.eq_ignore_ascii_case("IF_DATA") {
            return Err(Error::Parse(format!(
                "expected IF_DATA block, got {}",
                block.keyword
            )));
        }
        let params: Vec<&Token> = block.params().collect();
        if params.len() > 2 && params[0].text == PROTOCOL_KWP {
            let toks = &params[2..];
            return Ok(match params[1].text.as_str() {
                KwpAddressMapping::KEYWORD => KwpIfData::Named(KwpNamedNode::AddressMapping(
                    KwpAddressMapping::parse_tokens(toks)?,
                )),
                KwpDpBlob::KEYWORD => {
                    KwpIfData::Named(KwpNamedNode::DpBlob(KwpDpBlob::parse_tokens(toks)?))
                }
                KwpKpBlob::KEYWORD => {
                    KwpIfData::Named(KwpNamedNode::KpBlob(KwpKpBlob::parse_tokens(toks)?))
                }
                _ => Self::parse_nodes(block, &params)?,
            });
        }
        if params.first().map(|t| t.text.as_str()) == Some(PROTOCOL_KWP) {
            return Self::parse_nodes(block, &params);
        }
        Ok(KwpIfData::Unsupported(UnsupportedNode::from_block(block)))
    }

    fn parse_nodes(block: &Block, params: &[&Token]) -> Result<Self> {
        let mut nodes = Vec::new();
        for child in block.children() {
            nodes.push(KwpNode::parse(child)?);
        }
        Ok(KwpIfData::Nodes {
            params: params.iter().map(|t| t.text.clone()).collect(),
            nodes,
        })
    }

    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            KwpIfData::Named(n) => n.write_inline(w),
            KwpIfData::Nodes { params, nodes } => {
                let tp_version = nodes.iter().find_map(|n| match n {
                    KwpNode::TpBlob(tp) => Some(tp.eversion),
                    _ => None,
                });
                w.begin_block("IF_DATA");
                if !params.is_empty() {
                    w.value_line(None, &params.join(" "));
                }
                for node in nodes {
                    if node.filter_from_write() {
                        continue;
                    }
                    node.write_block(w, tp_version)?;
                }
                w.end_block("IF_DATA");
                Ok(())
            }
            KwpIfData::Unsupported(n) => Ok(n.write_block(w)?),
        }
    }

    pub fn nodes(&self) -> &[KwpNode] {
        match self {
            KwpIfData::Nodes { nodes, .. } => nodes,
            _ => &[],
        }
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

    fn first_block(src: &str) -> Block {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let b = root.children().next().unwrap().clone();
        b
    }

    fn node_roundtrip(kw_check: &str, src: &str) -> String {
        let block = first_block(src);
        assert_eq!(block.keyword, kw_check);
        let node = KwpNode::parse(&block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        node.write_block(&mut w, None).unwrap();
        w.into_string()
    }

    fn ifdata_roundtrip(src: &str) -> String {
        let block = first_block(src);
        let ifd = KwpIfData::parse(&block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        ifd.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn enum_keywords() {
        assert_eq!(
            KwpCopyMode::RAM_InitByTool.as_keyword(),
            Some("RAM_InitByTool")
        );
        assert_eq!(
            KwpFlashMode::from_keyword("autoflashback"),
            Some(KwpFlashMode::AUTOFLASHBACK)
        );
        assert_eq!(
            KwpFlashResult::default(),
            KwpFlashResult::RequestRoutineResults
        );
        assert_eq!(
            KwpVersion::from_keyword("VDA_1996"),
            Some(KwpVersion::VDA_1996)
        );
        assert_eq!(
            KwpAddressLocation::from_keyword("extern"),
            Some(KwpAddressLocation::EXTERN)
        );
        assert_eq!(
            physical_layer_parse("KLINE+CAN"),
            KwpPhysicalLayer::KLINE_CAN
        );
        assert_eq!(
            physical_layer_parse("KLINE_CAN"),
            KwpPhysicalLayer::KLINE_CAN
        );
        assert_eq!(physical_layer_parse("2"), KwpPhysicalLayer::NotSet);
        assert_eq!(stim_write_token(KwpStimulationMode::_WuP), "WuP");
        assert_eq!(stim_write_token(KwpStimulationMode::_5Baud), "5Baud");
        assert_eq!(stim_parse_prefixed("WuP"), KwpStimulationMode::_WuP);
        assert_eq!(stim_parse_prefixed("5Baud"), KwpStimulationMode::_5Baud);
        assert_eq!(bool_str(true), "1");
        assert_eq!(bool_str(false), "0");
    }

    #[test]
    fn data_access_flags_bits() {
        let f = KwpDataAccessFlags(KwpDataAccessFlags::READ_DATA | KwpDataAccessFlags::READ_CODE);
        assert!(f.contains(KwpDataAccessFlags::READ_DATA));
        assert!(!f.contains(KwpDataAccessFlags::VERIFY_CODE));
        assert!(f.contains(KwpDataAccessFlags::READ_CODE));
        assert!(!f.contains(KwpDataAccessFlags::RW_ONLY_ON_ACTIVE_PAGE));
    }

    #[test]
    fn address_roundtrip() {
        let out = node_roundtrip("ADDRESS", "/begin ADDRESS 0x100 0x101 /end ADDRESS");
        assert_eq!(out, "/begin ADDRESS\n  0x100\n  0x101\n/end ADDRESS\n");
    }

    #[test]
    fn can_full_roundtrip() {
        let src = "/begin CAN 500000 1 3 10 4 1 \
                   /begin ADDRESS 0x7E0 0x7E8 /end ADDRESS \
                   NETWORK_LIMITS 5 0xFF START_STOP\t7 /end CAN";
        let out = node_roundtrip("CAN", src);
        assert_eq!(
            out,
            "/begin CAN\n  500000\n  1\n  3\n  10\n  4\n  1\n  /begin ADDRESS\n    0x7E0\n    0x7E8\n  /end ADDRESS\n  NETWORK_LIMITS\n  5\n  0xFF\n  START_STOP\t7\n/end CAN\n"
        );
    }

    #[test]
    fn can_minimal_roundtrip() {
        let out = node_roundtrip("CAN", "/begin CAN 250000 0 0 0 0 0 START_STOP\t0 /end CAN");
        assert_eq!(
            out,
            "/begin CAN\n  250000\n  0\n  0\n  0\n  0\n  0\n  START_STOP\t0\n/end CAN\n"
        );
    }

    #[test]
    fn checksum_roundtrip() {
        let out = node_roundtrip(
            "CHECKSUM",
            "/begin CHECKSUM 0x1234 1 5 CodedResult RNC_RESULT 0x12 0x34 /end CHECKSUM",
        );
        assert_eq!(
            out,
            "/begin CHECKSUM\n  0x1234\n  1\n  5\n  CodedResult\n  RNC_RESULT 0x12 0x34\n/end CHECKSUM\n"
        );
        let out = node_roundtrip(
            "CHECKSUM",
            "/begin CHECKSUM 0x55 0 2 StartRoutine /end CHECKSUM",
        );
        assert_eq!(
            out,
            "/begin CHECKSUM\n  0x55\n  0\n  2\n  StartRoutine\n/end CHECKSUM\n"
        );
    }

    #[test]
    fn copy_roundtrip() {
        let out = node_roundtrip(
            "COPY",
            "/begin COPY RAM_InitByTool 0x85 COPY_PARA 0x1 0x2 0x3 /end COPY",
        );
        assert_eq!(
            out,
            "/begin COPY\n  RAM_InitByTool\n  0x85\n  COPY_PARA 0x1 0x2 0x3\n/end COPY\n"
        );
        let out = node_roundtrip("COPY", "/begin COPY RAM_InitByECU 0x80 /end COPY");
        assert_eq!(out, "/begin COPY\n  RAM_InitByECU\n  0x80\n/end COPY\n");
    }

    #[test]
    fn diag_baud_roundtrip() {
        let out = node_roundtrip(
            "DIAG_BAUD",
            "/begin DIAG_BAUD 57600 0x85 BD_PARA 0x4 0x5 /end DIAG_BAUD",
        );
        assert_eq!(
            out,
            "/begin DIAG_BAUD\n  57600\n  0x85\n  BD_PARA 0x4 0x5\n/end DIAG_BAUD\n"
        );
    }

    #[test]
    fn flash_roundtrip() {
        let out = node_roundtrip(
            "FLASH",
            "/begin FLASH TOOLFLASHBACK 9 StartRoutine COPY_FRAME 1 2 RNC_RESULT 0x3 COPY_PARA 0x4 0x5 0x6 /end FLASH",
        );
        assert_eq!(
            out,
            "/begin FLASH\n  TOOLFLASHBACK\n  9\n  StartRoutine\n  COPY_FRAME 1 2\n  RNC_RESULT 0x3\n  COPY_PARA 0x4 0x5 0x6\n/end FLASH\n"
        );
    }

    #[test]
    fn flash_copy_roundtrip() {
        let out = node_roundtrip(
            "FLASH_COPY",
            "/begin FLASH_COPY AUTOFLASHBACK 8 RequestRoutineResults RAM_InitByECU 0x80 COPY_FRAME 7 RNC_RESULT 0x8 0x9 COPY_PARA 0xA /end FLASH_COPY",
        );
        assert_eq!(
            out,
            "/begin FLASH_COPY\n  AUTOFLASHBACK\n  8\n  RequestRoutineResults\n  RAM_InitByECU\n  0x80\n  COPY_FRAME 7\n  RNC_RESULT 0x8 0x9\n  COPY_PARA 0xA\n/end FLASH_COPY\n"
        );
    }

    #[test]
    fn k_line_roundtrip() {
        let out = node_roundtrip("K_LINE", "/begin K_LINE WuP 0x11 0xF1 /end K_LINE");
        assert_eq!(out, "/begin K_LINE\n  WuP\n  0x11\n  0xF1\n/end K_LINE\n");
    }

    #[test]
    fn page_switch_roundtrip() {
        let out = node_roundtrip(
            "PAGE_SWITCH",
            "/begin PAGE_SWITCH LOCAL_ROUTINE PAGE_CODE 0xA ESCAPE_CODE_PARA_SET 0xB ESCAPE_CODE_PARA_GET 0xC /end PAGE_SWITCH",
        );
        assert_eq!(
            out,
            "/begin PAGE_SWITCH\n  LOCAL_ROUTINE\n  PAGE_CODE 0xA\n  ESCAPE_CODE_PARA_SET 0xB\n  ESCAPE_CODE_PARA_GET 0xC\n/end PAGE_SWITCH\n"
        );
        let block = first_block(
            "/begin PAGE_SWITCH ESCAPE_CODE ESCAPE_CODE_PARA_GET 0x1 PAGE_CODE 0x2 ESCAPE_CODE_PARA_SET 0x3 /end PAGE_SWITCH",
        );
        let p = KwpPageSwitch::parse(&block).unwrap();
        assert_eq!(p.escape_code_para_get, vec![1]);
        assert_eq!(p.page_code, vec![2]);
        assert_eq!(p.escape_code_para_set, vec![3]);
    }

    #[test]
    fn routine_para_roundtrip() {
        let out = node_roundtrip(
            "ROUTINE_PARA",
            "/begin ROUTINE_PARA RequestRoutineResults 3 RNC_RESULT 0x9 0x9 /end ROUTINE_PARA",
        );
        assert_eq!(
            out,
            "/begin ROUTINE_PARA\n  RequestRoutineResults\n  3\n  RequestRoutineResults\n  RNC_RESULT 0x9 0x9\n/end ROUTINE_PARA\n"
        );
        let block = first_block(&out);
        let r = KwpRoutinePara::parse(&block).unwrap();
        assert_eq!(r.result, KwpFlashResult::RequestRoutineResults);
        assert_eq!(r.local_routine_no, 3);
        assert_eq!(r.rnc_result, vec![9, 9]);
    }

    #[test]
    fn time_def_roundtrip() {
        let out = node_roundtrip(
            "TIME_DEF",
            "/begin TIME_DEF KWP_TIMING\t0x0001\t0x0002\t0x0003\t0x0004\t0x0005\t0x0006 USDTP_TIMING\t0x0007\t0x0008\t0x0009 /end TIME_DEF",
        );
        assert_eq!(
            out,
            "/begin TIME_DEF\n  KWP_TIMING\t0x0001\t0x0002\t0x0003\t0x0004\t0x0005\t0x0006\n  USDTP_TIMING\t0x0007\t0x0008\t0x0009\n/end TIME_DEF\n"
        );
        let out = node_roundtrip(
            "TIME_DEF",
            "/begin TIME_DEF KWP_TIMING\t0x0001\t0x0002\t0x0003\t0x0004\t0x0005\t0x0006 KWP_TIMING\t0x000A\t0x000B\t0x000C\t0x000D\t0x000E\t0x000F /end TIME_DEF",
        );
        assert!(out.contains("KWP_TIMING\t0x000A\t0x000B\t0x000C\t0x000D\t0x000E\t0x000F"));
    }

    #[test]
    fn tp_blob_v1_roundtrip() {
        let src = "/begin TP_BLOB 0x100 0x11 0xF1 WuP MSB_FIRST 1 0x8000 \
                   SERAM 0xA0 0xF 0x55 0xE0 0xF1 0x12 1 0 1 0 \
                   BAUD_DEF 57600 0x85 0x10 \
                   TIME_DEF 0x1 0x2 0x3 0x4 0x5 0x6 \
                   SECURITY_ACCESS 1 2 100 /end TP_BLOB";
        let out = node_roundtrip("TP_BLOB", src);
        assert_eq!(
            out,
            "/begin TP_BLOB\n  0x100\n  0x11\n  0xF1\n  WuP\n  MSB_FIRST\n  1\n  0x8000\n  SERAM\n  0xA0\n  0xF\n  0x55\n  0xE0\n  0xF1\n  0x12\n  1\n  0\n  1\n  0\n  BAUD_DEF\n  57600\n  0x85\n  0x10\n  TIME_DEF\n  0x1\n  0x2\n  0x3\n  0x4\n  0x5\n  0x6\n  SECURITY_ACCESS\n  1\n  2\n  100\n/end TP_BLOB\n"
        );
        let block = first_block(src);
        let tp = KwpTpBlob::parse(&block).unwrap();
        assert_eq!(tp.eversion, KwpVersion::NotSet);
        assert_eq!(tp.byte_order, ByteOrder::MSB_FIRST);
        assert!(tp.start_diag_without_bd_switch);
        let seram = tp.seram.as_ref().unwrap();
        assert_eq!(
            (seram.a, seram.o, seram.u, seram.e),
            (0xA0, 0xF, 0x55, 0xE0)
        );
        assert!(seram.flags.contains(KwpDataAccessFlags::READ_DATA));
        assert!(seram.flags.contains(KwpDataAccessFlags::READ_CODE));
        assert_eq!(tp.to_string(), "KWP, Version: 0100");
    }

    #[test]
    fn tp_blob_v2_vda_roundtrip() {
        let src = "/begin TP_BLOB 0x200 VDA_1996 MSB_LAST \
                   K_LINE Stimulation_5Baud 0x11 0xF1 \
                   /begin CAN 250000 0 0 0 0 0 START_STOP\t0 /end CAN \
                   DATA_ACCESS 0x1000 0x2000 0 1 0 1 \
                   SECURITY_ACCESS 1 2 100 /end TP_BLOB";
        let out = node_roundtrip("TP_BLOB", src);
        assert_eq!(
            out,
            "/begin TP_BLOB\n  0x200\n  VDA_1996\n  MSB_LAST\n  K_LINE\n  Stimulation_5Baud\n  0x11\n  0xF1\n  /begin CAN\n    250000\n    0\n    0\n    0\n    0\n    0\n    START_STOP\t0\n  /end CAN\n  DATA_ACCESS\n  0x1000\n  0x2000\n  0\n  1\n  0\n  1\n  SECURITY_ACCESS\n  1\n  2\n  100\n/end TP_BLOB\n"
        );
        let block = first_block(src);
        let tp = KwpTpBlob::parse(&block).unwrap();
        assert_eq!(tp.eversion, KwpVersion::VDA_1996);
        assert_eq!(
            tp.k_line.unwrap().stimulation_mode,
            KwpStimulationMode::_5Baud
        );
        let da = tp.data_access.unwrap();
        assert!(da.flags.contains(KwpDataAccessFlags::VERIFY_CODE));
        assert!(da
            .flags
            .contains(KwpDataAccessFlags::RW_ONLY_ON_ACTIVE_PAGE));
        assert!(tp.can.is_some());
    }

    #[test]
    fn tp_blob_v2_wup_roundtrip() {
        let out = node_roundtrip(
            "TP_BLOB",
            "/begin TP_BLOB 0x201 VDA_1996 MSB_FIRST K_LINE WuP 0x21 0xF2 /end TP_BLOB",
        );
        assert_eq!(
            out,
            "/begin TP_BLOB\n  0x201\n  VDA_1996\n  MSB_FIRST\n  K_LINE\n  WuP\n  0x21\n  0xF2\n/end TP_BLOB\n"
        );
    }

    #[test]
    fn source_old_qp_blob_roundtrip() {
        let src = "/begin SOURCE \"kwp src\" 3 25 QP_BLOB 2 BLOCKMODE 0x10 100 4 /end SOURCE";
        let block = first_block(src);
        let s = KwpSource::parse(&block).unwrap();
        let qp = s.qp_blob.as_ref().unwrap();
        assert_eq!(qp.physical_layer, KwpPhysicalLayer::NotSet);
        assert_eq!(qp.no_of_samplings, 2);
        assert_eq!(qp.max_sampling_rate, 100);
        let mut w = Writer::new(WriterOptions::default());
        s.write_block(&mut w, Some(KwpVersion::NotSet)).unwrap();
        assert_eq!(
            w.into_string(),
            "/begin SOURCE\n  \"kwp src\"\n  3\n  25\n  QP_BLOB\n  2\n  BLOCKMODE\n  0x10\n  100\n  4\n/end SOURCE\n"
        );
    }

    #[test]
    fn source_vda_qp_blob_roundtrip() {
        let src = "/begin SOURCE \"kwp src vda\" -1 50 QP_BLOB KLINE+CAN ADDRESSMODE 0x20 8 16 768 2 /end SOURCE";
        let block = first_block(src);
        let s = KwpSource::parse(&block).unwrap();
        let qp = s.qp_blob.as_ref().unwrap();
        assert_eq!(qp.physical_layer, KwpPhysicalLayer::KLINE_CAN);
        assert_eq!(s.cse_unit, -1);
        assert_eq!(qp.can_id, 768);
        let mut w = Writer::new(WriterOptions::default());
        s.write_block(&mut w, Some(KwpVersion::VDA_1996)).unwrap();
        assert_eq!(
            w.into_string(),
            "/begin SOURCE\n  \"kwp src vda\"\n  -1\n  50\n  QP_BLOB\n  KLINE_CAN\n  ADDRESSMODE\n  0x20\n  8\n  16\n  768\n  2\n/end SOURCE\n"
        );
        let mut w2 = Writer::new(WriterOptions::default());
        assert!(s.write_block(&mut w2, None).is_err());
    }

    #[test]
    fn source_plain_roundtrip() {
        let block = first_block("/begin SOURCE \"plain\" 0 0 /end SOURCE");
        let s = KwpSource::parse(&block).unwrap();
        assert!(s.qp_blob.is_none());
        let mut w = Writer::new(WriterOptions::default());
        s.write_block(&mut w, None).unwrap();
        assert_eq!(
            w.into_string(),
            "/begin SOURCE\n  \"plain\"\n  0\n  0\n/end SOURCE\n"
        );
    }

    #[test]
    fn named_node_inline_roundtrip() {
        let out = ifdata_roundtrip(
            "/begin IF_DATA ASAP1B_KWP2000 ADDRESS_MAPPING 0x3000 0x4000 0x80 /end IF_DATA",
        );
        assert_eq!(
            out,
            "/begin IF_DATA ASAP1B_KWP2000 ADDRESS_MAPPING 0x3000 0x4000 0x80 /end IF_DATA\n"
        );
        let out =
            ifdata_roundtrip("/begin IF_DATA ASAP1B_KWP2000 DP_BLOB 0x80000000 0x200 /end IF_DATA");
        assert_eq!(
            out,
            "/begin IF_DATA ASAP1B_KWP2000 DP_BLOB 0x80000000 0x200 /end IF_DATA\n"
        );
        let out =
            ifdata_roundtrip("/begin IF_DATA ASAP1B_KWP2000 KP_BLOB 0x9000 EXTERN 64 /end IF_DATA");
        assert_eq!(
            out,
            "/begin IF_DATA ASAP1B_KWP2000 KP_BLOB 0x9000 EXTERN 64 /end IF_DATA\n"
        );
    }

    #[test]
    fn named_node_name() {
        let block =
            first_block("/begin IF_DATA ASAP1B_KWP2000 KP_BLOB 0x9000 INTERN 16 /end IF_DATA");
        match KwpIfData::parse(&block).unwrap() {
            KwpIfData::Named(n) => assert_eq!(n.name(), "ASAP1B_KWP2000 KP_BLOB"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn ifdata_nodes_roundtrip_v1() {
        let src = "/begin IF_DATA ASAP1B_KWP2000 \
                   /begin TP_BLOB 0x100 0x11 0xF1 WuP MSB_FIRST 1 0x8000 SECURITY_ACCESS 1 2 100 /end TP_BLOB \
                   /begin SOURCE \"kwp src\" 3 25 QP_BLOB 2 BLOCKMODE 0x10 100 4 /end SOURCE \
                   /begin COPY RAM_InitByTool 0x85 COPY_PARA 0x1 /end COPY \
                   /end IF_DATA";
        let out = ifdata_roundtrip(src);
        assert_eq!(
            out,
            "/begin IF_DATA\n  ASAP1B_KWP2000\n  /begin TP_BLOB\n    0x100\n    0x11\n    0xF1\n    WuP\n    MSB_FIRST\n    1\n    0x8000\n    SECURITY_ACCESS\n    1\n    2\n    100\n  /end TP_BLOB\n  /begin SOURCE\n    \"kwp src\"\n    3\n    25\n    QP_BLOB\n    2\n    BLOCKMODE\n    0x10\n    100\n    4\n  /end SOURCE\n  /begin COPY\n    RAM_InitByTool\n    0x85\n    COPY_PARA 0x1\n  /end COPY\n/end IF_DATA\n"
        );
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let ifd1 = KwpIfData::parse(&first_block(src)).unwrap();
        let ifd2 = KwpIfData::parse(root.child("IF_DATA").unwrap()).unwrap();
        assert_eq!(ifd1, ifd2);
    }

    #[test]
    fn ifdata_nodes_vda_with_source() {
        let src = "/begin IF_DATA ASAP1B_KWP2000 \
                   /begin TP_BLOB 0x200 VDA_1996 MSB_LAST K_LINE WuP 0x11 0xF1 /end TP_BLOB \
                   /begin SOURCE \"s\" -1 50 QP_BLOB KLINE_CAN ADDRESSMODE 0x20 8 16 768 2 /end SOURCE \
                   /end IF_DATA";
        let out = ifdata_roundtrip(src);
        assert!(out.contains(
            "QP_BLOB\n    KLINE_CAN\n    ADDRESSMODE\n    0x20\n    8\n    16\n    768\n    2\n"
        ));
    }

    #[test]
    fn ifdata_filter_from_write() {
        let src = "/begin IF_DATA ASAP1B_KWP2000 \
                   /begin TP_BLOB 0x100 0x11 0xF1 WuP MSB_FIRST 1 0x8000 /end TP_BLOB \
                   /begin CAN 500000 1 3 10 4 1 START_STOP\t7 /end CAN \
                   /begin ADDRESS 0x100 0x101 /end ADDRESS \
                   /begin K_LINE WuP 0x11 0xF1 /end K_LINE \
                   /end IF_DATA";
        let out = ifdata_roundtrip(src);
        assert!(!out.contains("/begin CAN"));
        assert!(!out.contains("/begin ADDRESS"));
        assert!(out.contains("/begin K_LINE"));
        let ifd = KwpIfData::parse(&first_block(src)).unwrap();
        assert_eq!(ifd.nodes().len(), 4);
    }

    #[test]
    fn ifdata_unknown_protocol_passthrough() {
        let src = "/begin IF_DATA SOMETHING_ELSE 1 2 /end IF_DATA";
        match KwpIfData::parse(&first_block(src)).unwrap() {
            KwpIfData::Unsupported(u) => assert_eq!(u.keyword, "IF_DATA"),
            other => panic!("unexpected {other:?}"),
        }
        let out = ifdata_roundtrip(src);
        assert_eq!(out, "/begin IF_DATA\n  SOMETHING_ELSE 1 2\n/end IF_DATA\n");
    }

    #[test]
    fn ifdata_unknown_child_passthrough() {
        let src = "/begin IF_DATA ASAP1B_KWP2000 /begin FOO_BAR 1 \"x\" /end FOO_BAR /end IF_DATA";
        let out = ifdata_roundtrip(src);
        assert_eq!(
            out,
            "/begin IF_DATA\n  ASAP1B_KWP2000\n  /begin FOO_BAR\n    1 \"x\"\n  /end FOO_BAR\n/end IF_DATA\n"
        );
    }

    #[test]
    fn parse_errors() {
        assert!(KwpTpBlob::parse(&first_block("/begin TP_BLOB 0x100 /end TP_BLOB")).is_err());
        assert!(KwpSource::parse(&first_block("/begin SOURCE \"s\" /end SOURCE")).is_err());
        assert!(KwpIfData::parse(&first_block("/begin FOO 1 /end FOO")).is_err());
        assert!(KwpTpBlob::parse(&first_block(
            "/begin TP_BLOB 0x100 vda_1996 MSB_LAST /end TP_BLOB"
        ))
        .is_err());
    }
}
