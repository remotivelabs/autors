//! ELF file parsing: ELF header, section headers, symbol tables.
//! Core types: `ElfFile` (with the flattened `ElfHeader` / `SectionHeader` /
//! `SymbolEntry` covering both the 32- and 64-bit layouts in single structs),
//! `SectionValue`, `SymbolValue` and the various enums.
//! The binary layouts correspond to tightly packed (`Pack = 1`) structs. The
//! file is parsed directly in the endianness it declares, so the parser is
//! host-endianness independent.
//! Behavioral notes:
//! - Opening a non-ELF file or a file that fails to parse yields `Err`
//!   (callers can use `.ok()` to get null-like semantics);
//! - the whole file is always read into memory (in-memory stream semantics);
//! - a missing section-name string table (`e_shstrndx == 0`) is tolerated
//!   leniently as "empty section names" instead of failing the whole parse;
//! - section type / machine type fields keep their raw integer values and
//!   enum views are offered via `from_*` methods, so unknown values are not
//!   lost;
//! - the in-memory data is managed by ownership; no explicit disposal API.

use std::path::Path;

use indexmap::IndexMap;

use crate::dwarf::DwarfInfo;
use crate::error::{Error, Result};
use autors_symbols::update::{
    align_down, modules_with_index, parse_symbol_path, AddressNodeResolver, UpdateData, UpdateType,
    UpdaterSymbols,
};

/// ELF magic number (`\x7FELF`; as a little-endian u32 this is 0x464C457F).
pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

fn parse_err<T>(offset: u64, message: impl Into<String>) -> Result<T> {
    Err(Error::Parse {
        offset,
        message: message.into(),
    })
}

// ---------------------------------------------------------------------------
// Endianness-aware read cursor (same style as the Reader in autors-blf objects.rs)
// ---------------------------------------------------------------------------

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8], big_endian: bool) -> Self {
        Reader {
            buf,
            pos: 0,
            big_endian,
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.buf.len().saturating_sub(self.pos) < n {
            return parse_err(
                self.pos as u64,
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
        Ok(match self.big_endian {
            true => u16::from_be_bytes(a),
            false => u16::from_le_bytes(a),
        })
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        let mut a = [0u8; 4];
        a.copy_from_slice(self.take(4)?);
        Ok(match self.big_endian {
            true => u32::from_be_bytes(a),
            false => u32::from_le_bytes(a),
        })
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(match self.big_endian {
            true => u64::from_be_bytes(a),
            false => u64::from_le_bytes(a),
        })
    }

    /// Fixed-width unsigned integer (only 1/2/4/8 bytes are supported).
    pub(crate) fn uint_n(&mut self, n: usize) -> Result<u64> {
        match n {
            1 => Ok(u64::from(self.u8()?)),
            2 => Ok(u64::from(self.u16()?)),
            4 => Ok(u64::from(self.u32()?)),
            8 => self.u64(),
            _ => parse_err(self.pos as u64, format!("unsupported integer size {n}")),
        }
    }

    /// ULEB128.
    /// The value is accumulated with u64 shifts per the DWARF standard. (A
    /// 32-bit int accumulator would wrap shifts beyond 31 modulo 32 starting
    /// at the 6th byte, corrupting large values.)
    pub(crate) fn uleb128(&mut self) -> Result<u64> {
        let mut num = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.u8()?;
            num |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return Ok(num);
            }
            shift += 7;
            if shift >= 64 {
                return parse_err(self.pos as u64, "ULEB128 value too large");
            }
        }
    }

    /// SLEB128.
    /// As with ULEB128, the value is accumulated with u64 shifts per the DWARF
    /// standard (a 32-bit accumulator would corrupt large values).
    pub(crate) fn sleb128(&mut self) -> Result<i64> {
        let mut num = 0i64;
        let mut shift = 0u32;
        loop {
            let b = self.u8()?;
            num |= i64::from(b & 0x7F) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && b & 0x40 != 0 {
                    num |= -1i64 << shift;
                }
                return Ok(num);
            }
            if shift >= 64 {
                return parse_err(self.pos as u64, "SLEB128 value too large");
            }
        }
    }
}

/// Reads a NUL-terminated UTF-8 string and advances the cursor past the NUL
/// terminator (lossy UTF-8 decoding).
pub(crate) fn read_cstr(r: &mut Reader) -> Result<String> {
    let start = r.pos;
    let mut end = start;
    while end < r.buf.len() && r.buf[end] != 0 {
        end += 1;
    }
    let s = String::from_utf8_lossy(&r.buf[start..end]).into_owned();
    r.pos = (end + 1).min(r.buf.len());
    Ok(s)
}

/// Reads the NUL-terminated UTF-8 string at `offset` in a slice (bounds-safe;
/// lossy UTF-8 decoding). Does not advance any cursor.
pub(crate) fn cstr_at(buf: &[u8], offset: usize) -> String {
    if offset >= buf.len() {
        return String::new();
    }
    let rest = &buf[offset..];
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).into_owned()
}

// ---------------------------------------------------------------------------
// Enums (ELF identification and header fields)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FormatType {
    Bit32 = 1,
    Bit64 = 2,
}

impl FormatType {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(FormatType::Bit32),
            2 => Some(FormatType::Bit64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EndiannessType {
    LittleEndian = 1,
    BigEndian = 2,
}

impl EndiannessType {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(EndiannessType::LittleEndian),
            2 => Some(EndiannessType::BigEndian),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VersionType {
    Original = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OsType {
    /// System V.
    SystemV = 0,
    /// HP-UX.
    HPUX = 1,
    /// NetBSD.
    NetBSD = 2,
    /// Linux.
    Linux = 3,
    /// Solaris.
    Solaris = 6,
    /// AIX.
    AIX = 7,
    /// IRIX.
    IRIX = 8,
    /// FreeBSD.
    FreeBSD = 9,
    /// OpenBSD.
    OpenBSD = 12,
}

impl OsType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => OsType::SystemV,
            1 => OsType::HPUX,
            2 => OsType::NetBSD,
            3 => OsType::Linux,
            6 => OsType::Solaris,
            7 => OsType::AIX,
            8 => OsType::IRIX,
            9 => OsType::FreeBSD,
            12 => OsType::OpenBSD,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum TypeType {
    None = 0,
    Relocatable = 1,
    Executable = 2,
    Shared = 3,
    Core = 4,
}

impl TypeType {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0 => TypeType::None,
            1 => TypeType::Relocatable,
            2 => TypeType::Executable,
            3 => TypeType::Shared,
            4 => TypeType::Core,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
#[allow(non_camel_case_types)]
pub enum MachineType {
    /// No machine.
    NONE = 0,
    /// AT&T WE 32100.
    M32 = 1,
    /// SPARC.
    SPARC = 2,
    /// Intel 80386.
    _386 = 3,
    /// Motorola 68000.
    _68K = 4,
    /// Motorola 88000.
    _88K = 5,
    /// Intel MCU.
    IAMCU = 6,
    /// Intel 80860.
    __860 = 7,
    /// MIPS I Architecture.
    MIPS = 8,
    /// IBM System/370.
    S370 = 9,
    /// MIPS RS3000 Little-endian.
    MIPS_RS3_LE = 10,
    /// Hewlett-Packard PA-RISC.
    PARISC = 0xF,
    /// Fujitsu VPP500.
    VPP500 = 17,
    /// Enhanced instruction set SPARC.
    SPARC32PLUS = 18,
    /// Intel 80960.
    _960 = 19,
    /// PowerPC.
    PPC = 20,
    /// 64-bit PowerPC.
    PPC64 = 21,
    /// IBM System/390.
    S390 = 22,
    /// IBM SPU/SPC.
    SPU = 23,
    /// NEC V800.
    V800 = 36,
    /// Fujitsu FR20.
    FR20 = 37,
    /// TRW RH-32.
    RH32 = 38,
    /// Motorola RCE.
    RCE = 39,
    /// ARM 32-bit (AARCH32).
    ARM = 40,
    /// Digital Alpha.
    ALPHA = 41,
    /// Hitachi SH.
    SH = 42,
    /// SPARC Version 9.
    SPARCV9 = 43,
    /// Siemens Tricore.
    TRICORE = 44,
    /// Argonaut RISC Core.
    ARC = 45,
    /// Hitachi H8/300.
    H8_300 = 46,
    /// Hitachi H8/300H.
    H8_300H = 47,
    /// Hitachi H8S.
    H8S = 48,
    /// Hitachi H8/500.
    H8_500 = 49,
    /// Intel IA-64.
    IA_64 = 50,
    /// Stanford MIPS-X.
    MIPS_X = 51,
    /// Motorola ColdFire.
    COLDFIRE = 52,
    /// Motorola M68HC12.
    _68HC12 = 53,
    /// Fujitsu MMA.
    MMA = 54,
    /// Siemens PCP.
    PCP = 55,
    /// Sony nCPU.
    NCPU = 56,
    /// Denso NDR1.
    NDR1 = 57,
    /// Motorola Star*Core.
    STARCORE = 58,
    /// Toyota ME16.
    ME16 = 59,
    /// STMicroelectronics ST100.
    ST100 = 60,
    /// Advanced Logic TinyJ.
    TINYJ = 61,
    /// AMD x86-64.
    X86_64 = 62,
    /// Sony DSP.
    PDSP = 0x3F,
    /// DEC PDP-10.
    PDP10 = 0x40,
    /// DEC PDP-11.
    PDP11 = 65,
    /// Siemens FX66.
    FX66 = 66,
    /// ST9+ 8/16 bit.
    ST9PLUS = 67,
    /// ST7 8-bit.
    ST7 = 68,
    /// Motorola MC68HC16.
    _68HC16 = 69,
    /// Motorola MC68HC11.
    _68HC11 = 70,
    /// Motorola MC68HC08.
    _68HC08 = 71,
    /// Motorola MC68HC05.
    _68HC05 = 72,
    /// Silicon Graphics SVx.
    SVX = 73,
    /// ST19 8-bit.
    ST19 = 74,
    /// Digital VAX.
    VAX = 75,
    /// Axis Communications 32-bit.
    CRIS = 76,
    /// Infineon 32-bit.
    JAVELIN = 77,
    /// Element 14 64-bit DSP.
    FIREPATH = 78,
    /// LSI Logic 16-bit DSP.
    ZSP = 79,
    /// MMIX.
    MMIX = 80,
    /// Harvard machine-independent.
    HUANY = 81,
    /// SiTera Prism.
    PRISM = 82,
    /// Atmel AVR 8-bit.
    AVR = 83,
    /// Fujitsu FR30.
    FR30 = 84,
    /// Mitsubishi D10V.
    D10V = 85,
    /// Mitsubishi D30V.
    D30V = 86,
    /// NEC v850.
    V850 = 87,
    /// Mitsubishi M32R.
    M32R = 88,
    /// Matsushita MN10300.
    MN10300 = 89,
    /// Matsushita MN10200.
    MN10200 = 90,
    /// picoJava.
    PJ = 91,
    /// OpenRISC 32-bit.
    OPENRISC = 92,
    /// ARC ARCompact.
    ARC_COMPACT = 93,
    /// Tensilica Xtensa.
    XTENSA = 94,
    /// Alphamosaic VideoCore.
    VIDEOCORE = 95,
    /// Thompson Multimedia TMM_GPP.
    TMM_GPP = 96,
    /// National Semiconductor 32000.
    NS32K = 97,
    /// Tenor Network TPC.
    TPC = 98,
    /// Trebia SNP 1000.
    SNP1K = 99,
    /// STMicroelectronics ST200.
    ST200 = 100,
    /// Ubicom IP2xxx.
    IP2K = 101,
    /// MAX Processor.
    MAX = 102,
    /// National Semiconductor CompactRISC.
    CR = 103,
    /// Fujitsu F2MC16.
    F2MC16 = 104,
    /// TI msp430.
    MSP430 = 105,
    /// Analog Devices Blackfin.
    BLACKFIN = 106,
    /// Seiko Epson S1C33.
    SE_C33 = 107,
    /// Sharp embedded.
    SEP = 108,
    /// Arca RISC.
    ARCA = 109,
    /// PKU-Unity UNICORE.
    UNICORE = 110,
    /// eXcess CPU.
    EXCESS = 111,
    /// Icera DXP.
    DXP = 112,
    /// Altera Nios II.
    ALTERA_NIOS2 = 113,
    /// CompactRISC CRX.
    CRX = 114,
    /// Motorola XGATE.
    XGATE = 115,
    /// Infineon C16x/XC16x.
    C166 = 116,
    /// Renesas M16C.
    M16C = 117,
    /// Microchip dsPIC30F.
    DSPIC30F = 118,
    /// Freescale CE.
    CE = 119,
    /// Renesas M32C.
    M32C = 120,
    /// Altium TSK3000.
    TSK3000 = 131,
    /// Freescale RS08.
    RS08 = 132,
    /// Analog Devices SHARC.
    SHARC = 133,
    /// Cyan eCOG2.
    ECOG2 = 134,
    /// Sunplus S+core7.
    SCORE7 = 135,
    /// NJR 24-bit DSP.
    DSP24 = 136,
    /// Broadcom VideoCore III.
    VIDEOCORE3 = 137,
    /// Lattice FPGA RISC.
    LATTICEMICO32 = 138,
    /// Seiko Epson C17.
    SE_C17 = 139,
    /// TI TMS320C6000.
    C6000 = 140,
    /// TI TMS320C2000.
    C2000 = 141,
    /// TI TMS320C55x.
    C5500 = 142,
    /// TI ARP32.
    ARP32 = 143,
    /// TI PRU.
    PRU = 144,
    /// STMicroelectronics 64bit VLIW.
    MMDSP_PLUS = 160,
    /// Cypress M8C.
    CYPRESS_M8C = 161,
    /// Renesas R32C.
    R32C = 162,
    /// NXP TriMedia.
    TRIMEDIA = 163,
    /// QUALCOMM DSP6.
    QDSP6 = 164,
    /// Intel 8051.
    _8051 = 165,
    /// STxP7x.
    STXP7X = 166,
    /// Andes NDS32.
    NDS32 = 167,
    /// Cyan eCOG1X.
    ECOG1 = 168,
    /// Dallas MAXQ30.
    MAXQ30 = 169,
    /// NJR 16-bit DSP.
    XIMO16 = 170,
    /// M2000 MANIK.
    MANIK = 171,
    /// Cray NV2.
    CRAYNV2 = 172,
    /// Renesas RX.
    RX = 173,
    /// Imagination META.
    METAG = 174,
    /// MCST Elbrus.
    MCST_ELBRUS = 175,
    /// Cyan eCOG16.
    ECOG16 = 176,
    /// CompactRISC CR16.
    CR16 = 177,
    /// Freescale ETPU.
    ETPU = 178,
    /// Infineon SLE9X.
    SLE9X = 179,
    /// Intel L10M.
    L10M = 180,
    /// Intel K10M.
    K10M = 181,
    /// ARM 64-bit (AARCH64).
    AARCH64 = 183,
    /// Atmel AVR32.
    AVR32 = 185,
    /// STMicroelectronics STM8.
    STM8 = 186,
    /// Tilera TILE64.
    TILE64 = 187,
    /// Tilera TILEPro.
    TILEPRO = 188,
    /// Xilinx MicroBlaze.
    MICROBLAZE = 189,
    /// NVIDIA CUDA.
    CUDA = 190,
    /// Tilera TILE-Gx.
    TILEGX = 191,
    /// CloudShield.
    CLOUDSHIELD = 192,
    /// KIPO-KAIST Core-A 1st gen.
    COREA_1ST = 193,
    /// KIPO-KAIST Core-A 2nd gen.
    COREA_2ST = 194,
    /// Synopsys ARCompact V2.
    ARC_COMPACT2 = 195,
    /// Open8.
    OPEN8 = 196,
    /// Renesas RL78.
    RL78 = 197,
    /// Broadcom VideoCore V.
    VIDEOCORE5 = 198,
    /// Renesas 78KOR.
    _78KOR = 199,
    /// Freescale 56800EX.
    _56800EX = 200,
    /// Beyond BA1.
    BA1 = 201,
    /// Beyond BA2.
    BA2 = 202,
    /// XMOS xCORE.
    XCORE = 203,
    /// Microchip PIC.
    MCHP_PIC = 204,
    /// Reserved by Intel (205).
    _205 = 205,
    /// Reserved by Intel (206).
    _206 = 206,
    /// Reserved by Intel (207).
    _207 = 207,
    /// Reserved by Intel (208).
    _208 = 208,
    /// Reserved by Intel (209).
    _209 = 209,
    /// KM211 KM32.
    KM32 = 210,
    /// KM211 KMX32.
    KMX32 = 211,
    /// KM211 KMX16.
    KMX16 = 212,
    /// KM211 KMX8.
    KMX8 = 213,
    /// KM211 KVARC.
    KVARC = 214,
    /// Paneve CDP.
    CDP = 215,
    /// Cognitive Smart Memory.
    COGE = 216,
    /// Bluechip CoolEngine.
    COOL = 217,
    /// Nanoradio NORC.
    NORC = 218,
    /// CSR Kalimba.
    CSR_KALIMBA = 219,
    /// Zilog Z80.
    Z80 = 220,
    /// CDS VISIUMcore.
    VISIUM = 221,
    /// FTDI FT32.
    FT32 = 222,
    /// Moxie.
    MOXIE = 223,
    /// AMD GPU.
    AMDGPU = 224,
    /// RISC-V.
    RISCV = 243,
    /// Lanai.
    EM_LANAI = 244,
    /// Linux kernel bpf.
    EM_BPF = 247,
}

impl MachineType {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0 => MachineType::NONE,
            1 => MachineType::M32,
            2 => MachineType::SPARC,
            3 => MachineType::_386,
            4 => MachineType::_68K,
            5 => MachineType::_88K,
            6 => MachineType::IAMCU,
            7 => MachineType::__860,
            8 => MachineType::MIPS,
            9 => MachineType::S370,
            10 => MachineType::MIPS_RS3_LE,
            0xF => MachineType::PARISC,
            17 => MachineType::VPP500,
            18 => MachineType::SPARC32PLUS,
            19 => MachineType::_960,
            20 => MachineType::PPC,
            21 => MachineType::PPC64,
            22 => MachineType::S390,
            23 => MachineType::SPU,
            36 => MachineType::V800,
            37 => MachineType::FR20,
            38 => MachineType::RH32,
            39 => MachineType::RCE,
            40 => MachineType::ARM,
            41 => MachineType::ALPHA,
            42 => MachineType::SH,
            43 => MachineType::SPARCV9,
            44 => MachineType::TRICORE,
            45 => MachineType::ARC,
            46 => MachineType::H8_300,
            47 => MachineType::H8_300H,
            48 => MachineType::H8S,
            49 => MachineType::H8_500,
            50 => MachineType::IA_64,
            51 => MachineType::MIPS_X,
            52 => MachineType::COLDFIRE,
            53 => MachineType::_68HC12,
            54 => MachineType::MMA,
            55 => MachineType::PCP,
            56 => MachineType::NCPU,
            57 => MachineType::NDR1,
            58 => MachineType::STARCORE,
            59 => MachineType::ME16,
            60 => MachineType::ST100,
            61 => MachineType::TINYJ,
            62 => MachineType::X86_64,
            0x3F => MachineType::PDSP,
            0x40 => MachineType::PDP10,
            65 => MachineType::PDP11,
            66 => MachineType::FX66,
            67 => MachineType::ST9PLUS,
            68 => MachineType::ST7,
            69 => MachineType::_68HC16,
            70 => MachineType::_68HC11,
            71 => MachineType::_68HC08,
            72 => MachineType::_68HC05,
            73 => MachineType::SVX,
            74 => MachineType::ST19,
            75 => MachineType::VAX,
            76 => MachineType::CRIS,
            77 => MachineType::JAVELIN,
            78 => MachineType::FIREPATH,
            79 => MachineType::ZSP,
            80 => MachineType::MMIX,
            81 => MachineType::HUANY,
            82 => MachineType::PRISM,
            83 => MachineType::AVR,
            84 => MachineType::FR30,
            85 => MachineType::D10V,
            86 => MachineType::D30V,
            87 => MachineType::V850,
            88 => MachineType::M32R,
            89 => MachineType::MN10300,
            90 => MachineType::MN10200,
            91 => MachineType::PJ,
            92 => MachineType::OPENRISC,
            93 => MachineType::ARC_COMPACT,
            94 => MachineType::XTENSA,
            95 => MachineType::VIDEOCORE,
            96 => MachineType::TMM_GPP,
            97 => MachineType::NS32K,
            98 => MachineType::TPC,
            99 => MachineType::SNP1K,
            100 => MachineType::ST200,
            101 => MachineType::IP2K,
            102 => MachineType::MAX,
            103 => MachineType::CR,
            104 => MachineType::F2MC16,
            105 => MachineType::MSP430,
            106 => MachineType::BLACKFIN,
            107 => MachineType::SE_C33,
            108 => MachineType::SEP,
            109 => MachineType::ARCA,
            110 => MachineType::UNICORE,
            111 => MachineType::EXCESS,
            112 => MachineType::DXP,
            113 => MachineType::ALTERA_NIOS2,
            114 => MachineType::CRX,
            115 => MachineType::XGATE,
            116 => MachineType::C166,
            117 => MachineType::M16C,
            118 => MachineType::DSPIC30F,
            119 => MachineType::CE,
            120 => MachineType::M32C,
            131 => MachineType::TSK3000,
            132 => MachineType::RS08,
            133 => MachineType::SHARC,
            134 => MachineType::ECOG2,
            135 => MachineType::SCORE7,
            136 => MachineType::DSP24,
            137 => MachineType::VIDEOCORE3,
            138 => MachineType::LATTICEMICO32,
            139 => MachineType::SE_C17,
            140 => MachineType::C6000,
            141 => MachineType::C2000,
            142 => MachineType::C5500,
            143 => MachineType::ARP32,
            144 => MachineType::PRU,
            160 => MachineType::MMDSP_PLUS,
            161 => MachineType::CYPRESS_M8C,
            162 => MachineType::R32C,
            163 => MachineType::TRIMEDIA,
            164 => MachineType::QDSP6,
            165 => MachineType::_8051,
            166 => MachineType::STXP7X,
            167 => MachineType::NDS32,
            168 => MachineType::ECOG1,
            169 => MachineType::MAXQ30,
            170 => MachineType::XIMO16,
            171 => MachineType::MANIK,
            172 => MachineType::CRAYNV2,
            173 => MachineType::RX,
            174 => MachineType::METAG,
            175 => MachineType::MCST_ELBRUS,
            176 => MachineType::ECOG16,
            177 => MachineType::CR16,
            178 => MachineType::ETPU,
            179 => MachineType::SLE9X,
            180 => MachineType::L10M,
            181 => MachineType::K10M,
            183 => MachineType::AARCH64,
            185 => MachineType::AVR32,
            186 => MachineType::STM8,
            187 => MachineType::TILE64,
            188 => MachineType::TILEPRO,
            189 => MachineType::MICROBLAZE,
            190 => MachineType::CUDA,
            191 => MachineType::TILEGX,
            192 => MachineType::CLOUDSHIELD,
            193 => MachineType::COREA_1ST,
            194 => MachineType::COREA_2ST,
            195 => MachineType::ARC_COMPACT2,
            196 => MachineType::OPEN8,
            197 => MachineType::RL78,
            198 => MachineType::VIDEOCORE5,
            199 => MachineType::_78KOR,
            200 => MachineType::_56800EX,
            201 => MachineType::BA1,
            202 => MachineType::BA2,
            203 => MachineType::XCORE,
            204 => MachineType::MCHP_PIC,
            205 => MachineType::_205,
            206 => MachineType::_206,
            207 => MachineType::_207,
            208 => MachineType::_208,
            209 => MachineType::_209,
            210 => MachineType::KM32,
            211 => MachineType::KMX32,
            212 => MachineType::KMX16,
            213 => MachineType::KMX8,
            214 => MachineType::KVARC,
            215 => MachineType::CDP,
            216 => MachineType::COGE,
            217 => MachineType::COOL,
            218 => MachineType::NORC,
            219 => MachineType::CSR_KALIMBA,
            220 => MachineType::Z80,
            221 => MachineType::VISIUM,
            222 => MachineType::FT32,
            223 => MachineType::MOXIE,
            224 => MachineType::AMDGPU,
            243 => MachineType::RISCV,
            244 => MachineType::EM_LANAI,
            247 => MachineType::EM_BPF,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlagsType(pub u64);

impl FlagsType {
    pub const WRITE: Self = Self(0x1);
    pub const ALLOC: Self = Self(0x2);
    pub const EXECINSTR: Self = Self(0x4);
    pub const MERGE: Self = Self(0x10);
    pub const STRINGS: Self = Self(0x20);
    /// SHT_INFO_LINK.
    pub const INFO_LINK: Self = Self(0x40);
    pub const LINK_ORDER: Self = Self(0x80);
    pub const OS_NONCONFORMING: Self = Self(0x100);
    pub const GROUP: Self = Self(0x200);
    /// TLS.
    pub const TLS: Self = Self(0x400);
    pub const COMPRESSED: Self = Self(0x800);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
#[allow(non_camel_case_types)]
pub enum SectionType {
    NULL = 0,
    PROGBITS = 1,
    SYMTAB = 2,
    STRTAB = 3,
    RELA = 4,
    HASH = 5,
    DYNAMIC = 6,
    NOTE = 7,
    NOBITS = 8,
    REL = 9,
    SHLIB = 10,
    DYNSYM = 11,
    INIT_ARRAY = 14,
    FINI_ARRAY = 0xF,
    PREINIT_ARRAY = 0x10,
    GROUP = 17,
    SYMTAB_SHNDX = 18,
    SHT_RELR = 19,
}

impl SectionType {
    pub fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0 => SectionType::NULL,
            1 => SectionType::PROGBITS,
            2 => SectionType::SYMTAB,
            3 => SectionType::STRTAB,
            4 => SectionType::RELA,
            5 => SectionType::HASH,
            6 => SectionType::DYNAMIC,
            7 => SectionType::NOTE,
            8 => SectionType::NOBITS,
            9 => SectionType::REL,
            10 => SectionType::SHLIB,
            11 => SectionType::DYNSYM,
            14 => SectionType::INIT_ARRAY,
            0xF => SectionType::FINI_ARRAY,
            0x10 => SectionType::PREINIT_ARRAY,
            17 => SectionType::GROUP,
            18 => SectionType::SYMTAB_SHNDX,
            19 => SectionType::SHT_RELR,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SymbolBindType {
    Local = 0,
    Global = 1,
    Weak = 2,
    LoProc = 13,
    HiProc = 0xF,
}

impl SymbolBindType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => SymbolBindType::Local,
            1 => SymbolBindType::Global,
            2 => SymbolBindType::Weak,
            13 => SymbolBindType::LoProc,
            0xF => SymbolBindType::HiProc,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SymbolType {
    None = 0,
    Object = 1,
    Function = 2,
    Section = 3,
    File = 4,
    Common = 5,
    /// TLS.
    TLS = 6,
    LoProc = 13,
    HiProc = 0xF,
}

impl SymbolType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => SymbolType::None,
            1 => SymbolType::Object,
            2 => SymbolType::Function,
            3 => SymbolType::Section,
            4 => SymbolType::File,
            5 => SymbolType::Common,
            6 => SymbolType::TLS,
            13 => SymbolType::LoProc,
            0xF => SymbolType::HiProc,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfHeader {
    pub magic_number: u32,
    pub format: FormatType,
    pub endianness: EndiannessType,
    pub version: u8,
    pub os: u8,
    pub os_version: u8,
    pub typ: u16,
    pub machine: u16,
    pub version_no: u32,
    pub proc_address: u64,
    pub prog_hdr_offs: u64,
    pub sect_hdr_offs: u64,
    pub flags: u32,
    pub size: u16,
    pub prog_hdr_entry_size: u16,
    pub prog_hdr_entry_count: u16,
    pub sect_hdr_entry_size: u16,
    pub sect_hdr_entry_count: u16,
    pub sect_hdr_entry_idx_name: u16,
}

impl ElfHeader {
    pub fn os_type(&self) -> Option<OsType> {
        OsType::from_u8(self.os)
    }

    pub fn type_type(&self) -> Option<TypeType> {
        TypeType::from_u16(self.typ)
    }

    pub fn machine_type(&self) -> Option<MachineType> {
        MachineType::from_u16(self.machine)
    }

    pub(crate) fn is_big_endian(&self) -> bool {
        self.endianness == EndiannessType::BigEndian
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElfSectionHeader {
    pub name: u32,
    /// [`ElfSectionHeader::section_type`]).
    pub typ: u32,
    pub flags: FlagsType,
    pub adr: u64,
    pub ofs: u64,
    pub size: u64,
    pub link: u32,
    pub info: u32,
    pub adr_align: u64,
    pub ent_size: u64,
}

impl ElfSectionHeader {
    pub fn section_type(&self) -> Option<SectionType> {
        SectionType::from_u32(self.typ)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElfSymbol {
    pub name: u32,
    pub info: u8,
    /// st_other.
    pub other: u8,
    pub shndx: u16,
    pub value: u64,
    pub size: u64,
}

impl ElfSymbol {
    pub fn symbol_type(&self) -> Option<SymbolType> {
        SymbolType::from_u8(self.info & 0xF)
    }

    pub fn symbol_bind(&self) -> Option<SymbolBindType> {
        SymbolBindType::from_u8(self.info >> 4)
    }
}

// ---------------------------------------------------------------------------
// SectionValue / SymbolValue
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionValue {
    pub name: String,
    pub section: ElfSectionHeader,
}

impl SectionValue {
    pub fn data(&self, file_data: &[u8]) -> Result<Vec<u8>> {
        if self.section.size == 0 {
            return Ok(Vec::new());
        }
        if self.section.typ == SectionType::NOBITS as u32 {
            return Ok(vec![0u8; self.section.size as usize]);
        }
        let start = self.section.ofs as usize;
        let end = start + self.section.size as usize;
        if end > file_data.len() {
            return parse_err(
                self.section.ofs,
                format!("section {:?} data out of file bounds", self.name),
            );
        }
        Ok(file_data[start..end].to_vec())
    }

    /// `SectionValue.readUTF8String(BinaryReader, long)`.
    pub fn read_utf8_string(&self, file_data: &[u8], offset: u64) -> String {
        let start = self.section.ofs as usize + offset as usize;
        cstr_at(file_data, start)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolValue {
    pub name: String,
    pub symbol: ElfSymbol,
}

// ---------------------------------------------------------------------------
// ELFFile
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ElfFile {
    pub source_file: Option<String>,
    pub header: ElfHeader,
    pub data: Vec<u8>,
    pub sections: Vec<SectionValue>,
    pub symbols: IndexMap<String, SymbolValue>,
    pub dwarf: Option<DwarfInfo>,
}

impl ElfFile {
    pub fn open(path: impl AsRef<Path>, use_dwarf: bool) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read(path)?;
        Self::from_bytes(data, Some(path.to_string_lossy().into_owned()), use_dwarf)
    }

    pub fn from_bytes(data: Vec<u8>, source_file: Option<String>, use_dwarf: bool) -> Result<Self> {
        if data.len() < 52 {
            return parse_err(0, "file too small for an ELF header");
        }
        if data[0..4] != ELF_MAGIC {
            return parse_err(0, "not an ELF file (bad magic)");
        }
        let format = FormatType::from_u8(data[4]).ok_or_else(|| Error::Parse {
            offset: 4,
            message: format!("unknown ELF class {:#x}", data[4]),
        })?;
        let big_endian = match EndiannessType::from_u8(data[5]) {
            Some(EndiannessType::LittleEndian) => false,
            Some(EndiannessType::BigEndian) => true,
            None => {
                return parse_err(5, format!("unknown ELF endianness {:#x}", data[5]));
            }
        };
        let header = parse_header(&data, format, big_endian)?;
        if header.size != 52 && header.size != 64 {
            return parse_err(0, format!("unexpected ELF header size {}", header.size));
        }
        let mut file = ElfFile {
            source_file,
            header,
            data,
            sections: Vec::new(),
            symbols: IndexMap::new(),
            dwarf: None,
        };
        file.parse_sections()?;
        file.parse_symbols()?;
        if use_dwarf {
            file.dwarf = DwarfInfo::new(&file).ok();
        }
        Ok(file)
    }

    fn parse_sections(&mut self) -> Result<()> {
        let mut count = u64::from(self.header.sect_hdr_entry_count);
        let mut start = 1u64;
        if count == 0 {
            start = 0;
            count = 1;
        }
        let mut shstr_idx: Option<usize> = None;
        let mut i = start;
        while i < count {
            let section = self.read_section_header(i)?;
            if i == 0 {
                count = section.size;
                i += 1;
                continue;
            }
            if u64::from(self.header.sect_hdr_entry_idx_name) == i {
                shstr_idx = Some(self.sections.len());
            }
            self.sections.push(SectionValue {
                name: String::new(),
                section,
            });
            i += 1;
        }
        if let Some(idx) = shstr_idx {
            let strtab_ofs = self.sections[idx].section.ofs as usize;
            for sv in &mut self.sections {
                sv.name = cstr_at(&self.data, strtab_ofs + sv.section.name as usize);
            }
        }
        Ok(())
    }

    fn read_section_header(&self, index: u64) -> Result<ElfSectionHeader> {
        let ofs = self.header.sect_hdr_offs + index * u64::from(self.header.sect_hdr_entry_size);
        let mut r = Reader::new(&self.data, self.header.is_big_endian());
        r.seek(ofs as usize);
        let name = r.u32()?;
        let typ = r.u32()?;
        Ok(match self.header.format {
            FormatType::Bit32 => ElfSectionHeader {
                name,
                typ,
                flags: FlagsType(u64::from(r.u32()?)),
                adr: u64::from(r.u32()?),
                ofs: u64::from(r.u32()?),
                size: u64::from(r.u32()?),
                link: r.u32()?,
                info: r.u32()?,
                adr_align: u64::from(r.u32()?),
                ent_size: u64::from(r.u32()?),
            },
            FormatType::Bit64 => ElfSectionHeader {
                name,
                typ,
                flags: FlagsType(r.u64()?),
                adr: r.u64()?,
                ofs: r.u64()?,
                size: r.u64()?,
                link: r.u32()?,
                info: r.u32()?,
                adr_align: r.u64()?,
                ent_size: r.u64()?,
            },
        })
    }

    fn parse_symbols(&mut self) -> Result<()> {
        let symtabs: Vec<(usize, usize)> = self
            .get_sym_tabs()
            .iter()
            .map(|(a, b)| (self.section_index(a), self.section_index(b)))
            .collect();
        for (symtab_idx, strtab_idx) in symtabs {
            let (ofs, size, ent_size) = {
                let s = &self.sections[symtab_idx].section;
                (s.ofs, s.size, s.ent_size)
            };
            if ent_size == 0 {
                continue;
            }
            let strtab_ofs = self.sections[strtab_idx].section.ofs as usize;
            let mut i = 1u64;
            while i < size / ent_size {
                let sym = self.read_symbol(ofs + i * ent_size)?;
                if sym.size != 0 {
                    let name = cstr_at(&self.data, strtab_ofs + sym.name as usize);
                    if !name.is_empty() && sym.symbol_type() == Some(SymbolType::Object) {
                        self.symbols
                            .insert(name.clone(), SymbolValue { name, symbol: sym });
                    }
                }
                i += 1;
            }
        }
        Ok(())
    }

    fn section_index(&self, sv: &SectionValue) -> usize {
        self.sections
            .iter()
            .position(|s| std::ptr::eq(s, sv))
            .unwrap_or(0)
    }

    fn read_symbol(&self, ofs: u64) -> Result<ElfSymbol> {
        let mut r = Reader::new(&self.data, self.header.is_big_endian());
        r.seek(ofs as usize);
        Ok(match self.header.format {
            FormatType::Bit32 => ElfSymbol {
                name: r.u32()?,
                value: u64::from(r.u32()?),
                size: u64::from(r.u32()?),
                info: r.u8()?,
                other: r.u8()?,
                shndx: r.u16()?,
            },
            FormatType::Bit64 => ElfSymbol {
                name: r.u32()?,
                info: r.u8()?,
                other: r.u8()?,
                shndx: r.u16()?,
                value: r.u64()?,
                size: r.u64()?,
            },
        })
    }

    pub fn data_sections(&self) -> impl Iterator<Item = &SectionValue> {
        let mask = FlagsType(FlagsType::WRITE.0 | FlagsType::ALLOC.0);
        self.sections.iter().filter(move |s| {
            s.section.size != 0
                && s.section.typ == SectionType::PROGBITS as u32
                && s.section.flags.contains(mask)
        })
    }

    pub fn get_sym_tabs(&self) -> Vec<(&SectionValue, &SectionValue)> {
        let mut out = Vec::new();
        if let Some(symtab) = self
            .sections
            .iter()
            .find(|s| s.section.size != 0 && s.section.typ == SectionType::SYMTAB as u32)
        {
            if let Some(strtab) = self
                .sections
                .iter()
                .find(|s| s.name == ".strtab" && s.section.size != 0)
            {
                out.push((symtab, strtab));
            }
        }
        if let Some(dynsym) = self
            .sections
            .iter()
            .find(|s| s.section.size != 0 && s.section.typ == SectionType::DYNSYM as u32)
        {
            if let Some(dynstr) = self
                .sections
                .iter()
                .find(|s| s.name == ".dynstr" && s.section.size != 0)
            {
                out.push((dynsym, dynstr));
            }
        }
        out
    }

    pub fn get_data(&self, address: u64, size: u64) -> Option<Vec<u8>> {
        if address == 0 || size == 0 {
            return None;
        }
        let sv = self.sections.iter().find(|s| {
            s.section.size != 0
                && address >= s.section.adr
                && address <= s.section.adr + (s.section.size - 1)
        })?;
        if sv.name.eq_ignore_ascii_case(".bss") {
            return Some(vec![0u8; size as usize]);
        }
        let start = sv.section.ofs as usize + (address - sv.section.adr) as usize;
        let end = start + size as usize;
        if end > self.data.len() {
            return None;
        }
        Some(self.data[start..end].to_vec())
    }

    pub fn get_section(&self, symbol: &ElfSymbol) -> Option<&SectionValue> {
        let idx = usize::from(symbol.shndx).checked_sub(1)?;
        self.sections.get(idx)
    }

    pub fn get_values_to_synchronize(
        &mut self,
        project: &autors_a2l::Project,
        address_multiplier: u64,
    ) -> Result<(Vec<UpdateData>, u64, u64)> {
        let mut ds_start = u64::MAX;
        let mut ds_len = 0u64;
        let mut list = Vec::new();
        let resolver = AddressNodeResolver::new(project);
        for (mi, module) in modules_with_index(project) {
            for (ci, child) in module.children.iter().enumerate() {
                let Some(node) = resolver.address_node(child)? else {
                    continue;
                };
                if node.is_record_layout_ref && node.record_layout.is_none() {
                    continue;
                }
                let Some(updater) = self.resolve_symbol(&node)? else {
                    list.push(UpdateData::not_matched(mi, ci));
                    continue;
                };
                let (updater, offset) = updater;
                if updater.size == 0 {
                    continue;
                }
                let addr = updater.address.wrapping_mul(address_multiplier);
                if node.is_record_layout_ref {
                    ds_start = ds_start.min(addr);
                    ds_len = ds_len.max(ds_start + updater.byte_size() - ds_start);
                }
                let cur_addr = u64::from(node.address.unwrap_or(u32::MAX));
                let mem_size = node.memory_size as u64;
                let cur_mask = node.bit_mask;
                if (cur_addr as i64 + i64::from(offset)) == addr as i64
                    && mem_size == updater.byte_size()
                    && cur_mask == updater.bit_mask()
                {
                    list.push(UpdateData {
                        module_idx: mi,
                        child_idx: ci,
                        address: cur_addr,
                        size: updater.byte_size(),
                        bit_mask: updater.bit_mask(),
                        typ: UpdateType::Matched,
                    });
                    continue;
                }
                let typ = if mem_size == updater.byte_size() {
                    UpdateType::AdjustAddress
                } else {
                    UpdateType::AdjustAddressAndSize
                };
                list.push(UpdateData {
                    module_idx: mi,
                    child_idx: ci,
                    address: addr.wrapping_sub(offset as u64),
                    size: updater.byte_size(),
                    bit_mask: updater.bit_mask(),
                    typ,
                });
            }
        }
        ds_start = i64::from(align_down(ds_start as u32 as i32, 4)) as u64;
        Ok((list, ds_start, ds_len))
    }

    fn resolve_symbol<'a>(
        &mut self,
        node: &autors_symbols::update::AddressNode<'a>,
    ) -> Result<Option<(UpdaterSymbols, i32)>> {
        let (sym_name, offset) = node.symbol_name();
        let path = parse_symbol_path(&sym_name)?;
        if let Some(dwarf) = &mut self.dwarf {
            if let Some(up) = dwarf.lookup_path(&path) {
                return Ok(Some((up, 0)));
            }
        }
        if path.len() == 1 {
            if let Some(sv) = self.symbols.get(&path[0].name) {
                return Ok(Some((
                    UpdaterSymbols::from_elf(&sv.name, sv.symbol.value, sv.symbol.size),
                    offset,
                )));
            }
        }
        Ok(None)
    }
}

fn parse_header(data: &[u8], format: FormatType, big_endian: bool) -> Result<ElfHeader> {
    let mut r = Reader::new(data, big_endian);
    let magic_number = r.u32()?;
    let format_b = r.u8()?;
    let endianness_b = r.u8()?;
    let version = r.u8()?;
    let os = r.u8()?;
    let os_version = r.u8()?;
    r.take(7)?;
    let typ = r.u16()?;
    let machine = r.u16()?;
    let version_no = r.u32()?;
    let (proc_address, prog_hdr_offs, sect_hdr_offs, flags) = match format {
        FormatType::Bit32 => (
            u64::from(r.u32()?),
            u64::from(r.u32()?),
            u64::from(r.u32()?),
            r.u32()?,
        ),
        FormatType::Bit64 => (r.u64()?, r.u64()?, r.u64()?, r.u32()?),
    };
    Ok(ElfHeader {
        magic_number,
        format: FormatType::from_u8(format_b).ok_or_else(|| Error::Parse {
            offset: 4,
            message: format!("unknown ELF class {format_b:#x}"),
        })?,
        endianness: EndiannessType::from_u8(endianness_b).ok_or_else(|| Error::Parse {
            offset: 5,
            message: format!("unknown ELF endianness {endianness_b:#x}"),
        })?,
        version,
        os,
        os_version,
        typ,
        machine,
        version_no,
        proc_address,
        prog_hdr_offs,
        sect_hdr_offs,
        flags,
        size: r.u16()?,
        prog_hdr_entry_size: r.u16()?,
        prog_hdr_entry_count: r.u16()?,
        sect_hdr_entry_size: r.u16()?,
        sect_hdr_entry_count: r.u16()?,
        sect_hdr_entry_idx_name: r.u16()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ElfBuilder {
        big_endian: bool,
        is64: bool,
        data: Vec<u8>,
    }

    impl ElfBuilder {
        fn new(is64: bool, big_endian: bool) -> Self {
            ElfBuilder {
                big_endian,
                is64,
                data: Vec::new(),
            }
        }

        fn w16(&mut self, v: u16) {
            self.data.extend_from_slice(&match self.big_endian {
                true => v.to_be_bytes(),
                false => v.to_le_bytes(),
            });
        }

        fn w32(&mut self, v: u32) {
            self.data.extend_from_slice(&match self.big_endian {
                true => v.to_be_bytes(),
                false => v.to_le_bytes(),
            });
        }

        fn w64(&mut self, v: u64) {
            self.data.extend_from_slice(&match self.big_endian {
                true => v.to_be_bytes(),
                false => v.to_le_bytes(),
            });
        }

        fn pad_to(&mut self, ofs: usize) {
            while self.data.len() < ofs {
                self.data.push(0);
            }
        }

        fn build(mut self) -> Vec<u8> {
            let header_size = if self.is64 { 64usize } else { 52 };
            let data_ofs = header_size;
            let data_addr = 0x8000u64;
            let data_content = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
            let strtab = b"\0myVar\0";
            let strtab_ofs = data_ofs + data_content.len();
            let symtab_ofs = strtab_ofs + strtab.len();
            let (sym_ent_size, sym_size) = if self.is64 {
                (24usize, 48u64)
            } else {
                (16, 32)
            };
            let shstrtab = b"\0.data\0.strtab\0.symtab\0.shstrtab\0";
            let shstrtab_ofs = symtab_ofs + sym_size as usize;
            let shstr_name_data = 1u32;
            let shstr_name_strtab = 7u32;
            let shstr_name_symtab = 15u32;
            let shstr_name_shstrtab = 23u32;
            let sht_ofs = shstrtab_ofs + shstrtab.len();
            let shnum = 5u16;
            self.data.extend_from_slice(&ELF_MAGIC);
            self.data.push(if self.is64 { 2 } else { 1 });
            self.data.push(if self.big_endian { 2 } else { 1 });
            self.data.push(1); // version
            self.data.push(0); // osabi
            self.data.push(0); // abiversion
            self.data.extend_from_slice(&[0; 7]);
            self.w16(2); // e_type = Executable
            self.w16(62); // e_machine = X86_64
            self.w32(1); // e_version
            if self.is64 {
                self.w64(0);
                self.w64(0);
                self.w64(sht_ofs as u64);
                self.w32(0);
            } else {
                self.w32(0);
                self.w32(0);
                self.w32(sht_ofs as u32);
                self.w32(0);
            }
            self.w16(header_size as u16); // e_ehsize
            self.w16(0); // e_phentsize
            self.w16(0); // e_phnum
            self.w16(if self.is64 { 64 } else { 40 }); // e_shentsize
            self.w16(shnum);
            self.w16(4); // e_shstrndx
                         // .data
            self.pad_to(data_ofs);
            self.data.extend_from_slice(&data_content);
            // .strtab
            self.pad_to(strtab_ofs);
            self.data.extend_from_slice(strtab);
            self.pad_to(symtab_ofs);
            for _ in 0..sym_ent_size {
                self.data.push(0);
            }
            self.w32(1); // st_name = "myVar"
            if self.is64 {
                self.data.push(0x11); // info: GLOBAL|OBJECT
                self.data.push(0);
                self.w16(1);
                self.w64(data_addr);
                self.w64(4);
            } else {
                self.w32(data_addr as u32);
                self.w32(4);
                self.data.push(0x11);
                self.data.push(0);
                self.w16(1);
            }
            // .shstrtab
            self.pad_to(shstrtab_ofs);
            self.data.extend_from_slice(shstrtab);
            self.pad_to(sht_ofs);
            let mut sh = |name: u32,
                          typ: u32,
                          flags: u64,
                          adr: u64,
                          ofs: u64,
                          size: u64,
                          link: u32,
                          info: u32,
                          align: u64,
                          ent: u64| {
                self.w32(name);
                self.w32(typ);
                if self.is64 {
                    self.w64(flags);
                    self.w64(adr);
                    self.w64(ofs);
                    self.w64(size);
                    self.w32(link);
                    self.w32(info);
                    self.w64(align);
                    self.w64(ent);
                } else {
                    self.w32(flags as u32);
                    self.w32(adr as u32);
                    self.w32(ofs as u32);
                    self.w32(size as u32);
                    self.w32(link);
                    self.w32(info);
                    self.w32(align as u32);
                    self.w32(ent as u32);
                }
            };
            // 0: NULL
            sh(0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
            // 1: .data(PROGBITS, WRITE|ALLOC)
            sh(
                shstr_name_data,
                1,
                0x3,
                data_addr,
                data_ofs as u64,
                16,
                0,
                0,
                4,
                0,
            );
            // 2: .strtab
            sh(
                shstr_name_strtab,
                3,
                0,
                0,
                strtab_ofs as u64,
                strtab.len() as u64,
                0,
                0,
                1,
                0,
            );
            // 3: .symtab(link=2 .strtab)
            sh(
                shstr_name_symtab,
                2,
                0,
                0,
                symtab_ofs as u64,
                sym_size,
                2,
                1,
                4,
                sym_ent_size as u64,
            );
            // 4: .shstrtab
            sh(
                shstr_name_shstrtab,
                3,
                0,
                0,
                shstrtab_ofs as u64,
                shstrtab.len() as u64,
                0,
                0,
                1,
                0,
            );
            self.data
        }
    }

    fn assert_elf(file: &ElfFile, is64: bool, big_endian: bool) {
        assert_eq!(
            file.header.format,
            if is64 {
                FormatType::Bit64
            } else {
                FormatType::Bit32
            }
        );
        assert_eq!(
            file.header.endianness,
            if big_endian {
                EndiannessType::BigEndian
            } else {
                EndiannessType::LittleEndian
            }
        );
        assert_eq!(file.header.machine_type(), Some(MachineType::X86_64));
        assert_eq!(file.header.type_type(), Some(TypeType::Executable));
        assert_eq!(file.sections.len(), 4);
        assert_eq!(file.sections[0].name, ".data");
        assert_eq!(file.sections[0].section.adr, 0x8000);
        assert_eq!(file.sections[2].name, ".symtab");
        let sym = file.symbols.get("myVar").expect("myVar symbol");
        assert_eq!(sym.symbol.value, 0x8000);
        assert_eq!(sym.symbol.size, 4);
        assert_eq!(sym.symbol.symbol_type(), Some(SymbolType::Object));
        assert_eq!(sym.symbol.symbol_bind(), Some(SymbolBindType::Global));
        // get_section:shndx=1 → sections[0]
        assert_eq!(file.get_section(&sym.symbol).unwrap().name, ".data");
        let d = file.get_data(0x8000, 4).unwrap();
        assert_eq!(d, vec![1, 2, 3, 4]);
        assert!(file.get_data(0x1234, 4).is_none());
        assert_eq!(file.sections[0].data(&file.data).unwrap().len(), 16);
    }

    #[test]
    fn parse_elf32_le() {
        let data = ElfBuilder::new(false, false).build();
        let file = ElfFile::from_bytes(data, None, false).unwrap();
        assert_elf(&file, false, false);
    }

    #[test]
    fn parse_elf64_le() {
        let data = ElfBuilder::new(true, false).build();
        let file = ElfFile::from_bytes(data, None, false).unwrap();
        assert_elf(&file, true, false);
    }

    #[test]
    fn parse_elf32_be() {
        let data = ElfBuilder::new(false, true).build();
        let file = ElfFile::from_bytes(data, None, false).unwrap();
        assert_elf(&file, false, true);
    }

    #[test]
    fn parse_elf64_be() {
        let data = ElfBuilder::new(true, true).build();
        let file = ElfFile::from_bytes(data, None, false).unwrap();
        assert_elf(&file, true, true);
    }

    #[test]
    fn rejects_bad_magic_and_small_files() {
        assert!(ElfFile::from_bytes(vec![0u8; 10], None, false).is_err());
        let mut data = ElfBuilder::new(false, false).build();
        data[0] = 0;
        assert!(ElfFile::from_bytes(data, None, false).is_err());
    }
}
