//! DWARF debug-info parsing: compilation units, variable address resolution,
//! and the symbol tree.
//! Core types: `DwarfInfo`, `CompilationUnitHeader` (with optional
//! `type_signature` / `type_offset` fields for type units),
//! `AbbrevEntry` (a single DIE representation with an `EntryKind`
//! discriminator),
//! `Attribute`, `SymbolsTree`, `SymbolValue`, and the
//! `DW_AT`/`DW_FORM`/`DW_LANG`/`DW_TAG` enums/newtypes.
//! Design notes:
//! - The DIE object graph (parent/child references) is stored in an arena:
//!   `DwarfInfo.entries` holds all DIEs and parent/child/reference relations
//!   are expressed as indices; symbol-tree `SymbolValue` nodes are stored
//!   likewise in `DwarfInfo.sym_values`;
//! - Attribute values such as `Location` are computed on demand on each
//!   access rather than cached;
//! - The required section names follow the DWARF standard: `.debug_info` /
//!   `.debug_abbrev` / `.debug_str` / `.debug_types` (a fifth, saved but
//!   unused, section name is interpreted as `.debug_line`).
//!
//! Intentional deviations from the DWARF standard, kept for behavioral
//! compatibility (see also the per-function comments):
//! - For `ref1`/`ref2` forms the target address is computed as
//!   "CU start + data start + relative value" (one data-start add more than
//!   `ref4`/`ref8`/`ref_udata`); this quirk is reproduced deliberately;
//! - `DW_FORM.strp` reads the string offset using `AddressSize` bytes (the
//!   DWARF standard would use 4/8 bytes per DWARF32/64); reproduced
//!   deliberately;
//! - A `data2`-form `location` attribute is narrowed to a byte; values above
//!   255 fall back to the "invalid address" sentinel (`u64::MAX`) instead of
//!   raising an error;
//! - ULEB128/SLEB128 values are read with u64 shifts per the DWARF standard
//!   (a 32-bit-int shift implementation would produce wrong results from the
//!   6th byte on);
//! - Missing parents / absent lookups are reported as `Option::None` instead
//!   of raising exceptions.

use indexmap::IndexMap;

use autors_a2l::model::enums::DataType;

use crate::elf::{read_cstr, ElfFile, Reader, SectionValue};
use crate::error::{Error, Result};
use autors_symbols::update::{build_bitmask, SymbolPathElem, UpdaterSymbols, UpdaterTag};

fn parse_err<T>(offset: u64, message: impl Into<String>) -> Result<T> {
    Err(Error::Parse {
        offset,
        message: message.into(),
    })
}

/// `.debug_info` section name (per the DWARF standard).
pub const SECTION_DEBUG_INFO: &str = ".debug_info";
/// `.debug_abbrev` section name (per the DWARF standard).
pub const SECTION_DEBUG_ABBREV: &str = ".debug_abbrev";
/// `.debug_str` section name (per the DWARF standard).
pub const SECTION_DEBUG_STR: &str = ".debug_str";
/// `.debug_types` section name (per the DWARF standard).
pub const SECTION_DEBUG_TYPES: &str = ".debug_types";
/// Fifth section name; saved but unused, interpreted as `.debug_line` per the
/// DWARF standard.
pub const SECTION_DEBUG_LINE: &str = ".debug_line";

// ---------------------------------------------------------------------------
// Enums and constants
// ---------------------------------------------------------------------------

/// DWARF attribute name (`DW_AT_*`).
/// Modeled as a newtype rather than a closed enum so that arbitrary values
/// are preserved; the associated constants cover the full `DW_AT` table.
/// Same style as the flag newtypes in autors-blf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DwAt(pub u16);

#[allow(non_upper_case_globals)]
impl DwAt {
    /// none.
    pub const NONE: Self = Self(0);
    /// sibling.
    pub const SIBLING: Self = Self(1);
    /// location.
    pub const LOCATION: Self = Self(2);
    /// name.
    pub const NAME: Self = Self(3);
    /// ordering.
    pub const ORDERING: Self = Self(9);
    /// subscr_data.
    pub const SUBSCR_DATA: Self = Self(10);
    /// byte_size.
    pub const BYTE_SIZE: Self = Self(11);
    /// bit_offset.
    pub const BIT_OFFSET: Self = Self(12);
    /// bit_size.
    pub const BIT_SIZE: Self = Self(13);
    /// element_list.
    pub const ELEMENT_LIST: Self = Self(0xF);
    /// stmt_list.
    pub const STMT_LIST: Self = Self(0x10);
    /// low_pc.
    pub const LOW_PC: Self = Self(17);
    /// high_pc.
    pub const HIGH_PC: Self = Self(18);
    /// language.
    pub const LANGUAGE: Self = Self(19);
    /// member.
    pub const MEMBER: Self = Self(20);
    /// discr.
    pub const DISCR: Self = Self(21);
    /// discr_value.
    pub const DISCR_VALUE: Self = Self(22);
    /// visibility.
    pub const VISIBILITY: Self = Self(23);
    /// import.
    pub const IMPORT: Self = Self(24);
    /// string_length.
    pub const STRING_LENGTH: Self = Self(25);
    /// common_reference.
    pub const COMMON_REFERENCE: Self = Self(26);
    /// comp_dir.
    pub const COMP_DIR: Self = Self(27);
    /// const_value.
    pub const CONST_VALUE: Self = Self(28);
    /// containing_type.
    pub const CONTAINING_TYPE: Self = Self(29);
    /// default_value.
    pub const DEFAULT_VALUE: Self = Self(30);
    /// inline.
    pub const INLINE: Self = Self(0x20);
    /// is_optional.
    pub const IS_OPTIONAL: Self = Self(33);
    /// lower_bound.
    pub const LOWER_BOUND: Self = Self(34);
    /// producer.
    pub const PRODUCER: Self = Self(37);
    /// prototyped.
    pub const PROTOTYPED: Self = Self(39);
    /// return_addr.
    pub const RETURN_ADDR: Self = Self(42);
    /// start_scope.
    pub const START_SCOPE: Self = Self(44);
    /// bit_stride.
    pub const BIT_STRIDE: Self = Self(46);
    /// upper_bound.
    pub const UPPER_BOUND: Self = Self(47);
    /// abstract_origin.
    pub const ABSTRACT_ORIGIN: Self = Self(49);
    /// accessibility.
    pub const ACCESSIBILITY: Self = Self(50);
    /// address_class.
    pub const ADDRESS_CLASS: Self = Self(51);
    /// artificial.
    pub const ARTIFICIAL: Self = Self(52);
    /// base_types.
    pub const BASE_TYPES: Self = Self(53);
    /// calling_convention.
    pub const CALLING_CONVENTION: Self = Self(54);
    /// count.
    pub const COUNT: Self = Self(55);
    /// data_member_location.
    pub const DATA_MEMBER_LOCATION: Self = Self(56);
    /// decl_column.
    pub const DECL_COLUMN: Self = Self(57);
    /// decl_file.
    pub const DECL_FILE: Self = Self(58);
    /// decl_line.
    pub const DECL_LINE: Self = Self(59);
    /// declaration.
    pub const DECLARATION: Self = Self(60);
    /// discr_list.
    pub const DISCR_LIST: Self = Self(61);
    /// encoding.
    pub const ENCODING: Self = Self(62);
    /// external.
    pub const EXTERNAL: Self = Self(0x3F);
    /// frame_base.
    pub const FRAME_BASE: Self = Self(0x40);
    /// friend.
    pub const FRIEND: Self = Self(65);
    /// identifier_case.
    pub const IDENTIFIER_CASE: Self = Self(66);
    /// macro_info.
    pub const MACRO_INFO: Self = Self(67);
    /// namelist_item.
    pub const NAMELIST_ITEM: Self = Self(68);
    /// priority.
    pub const PRIORITY: Self = Self(69);
    /// segment.
    pub const SEGMENT: Self = Self(70);
    /// specification.
    pub const SPECIFICATION: Self = Self(71);
    /// static_link.
    pub const STATIC_LINK: Self = Self(72);
    /// type.
    pub const TYPE: Self = Self(73);
    /// use_location.
    pub const USE_LOCATION: Self = Self(74);
    /// variable_parameter.
    pub const VARIABLE_PARAMETER: Self = Self(75);
    /// virtuality.
    pub const VIRTUALITY: Self = Self(76);
    /// vtable_elem_location.
    pub const VTABLE_ELEM_LOCATION: Self = Self(77);
    /// allocated.
    pub const ALLOCATED: Self = Self(78);
    /// associated.
    pub const ASSOCIATED: Self = Self(79);
    /// data_location.
    pub const DATA_LOCATION: Self = Self(80);
    /// byte_stride.
    pub const BYTE_STRIDE: Self = Self(81);
    /// entry_pc.
    pub const ENTRY_PC: Self = Self(82);
    /// use_UTF8.
    pub const USE_UTF8: Self = Self(83);
    /// extension.
    pub const EXTENSION: Self = Self(84);
    /// ranges.
    pub const RANGES: Self = Self(85);
    /// trampoline.
    pub const TRAMPOLINE: Self = Self(86);
    /// call_column.
    pub const CALL_COLUMN: Self = Self(87);
    /// call_file.
    pub const CALL_FILE: Self = Self(88);
    /// call_line.
    pub const CALL_LINE: Self = Self(89);
    /// description.
    pub const DESCRIPTION: Self = Self(90);
    /// binary_scale.
    pub const BINARY_SCALE: Self = Self(91);
    /// decimal_scale.
    pub const DECIMAL_SCALE: Self = Self(92);
    /// small.
    pub const SMALL: Self = Self(93);
    /// decimal_sign.
    pub const DECIMAL_SIGN: Self = Self(94);
    /// digit_count.
    pub const DIGIT_COUNT: Self = Self(95);
    /// picture_string.
    pub const PICTURE_STRING: Self = Self(96);
    /// mutable.
    pub const MUTABLE: Self = Self(97);
    /// threads_scaled.
    pub const THREADS_SCALED: Self = Self(98);
    /// explicite.
    pub const EXPLICITE: Self = Self(99);
    /// object_pointer.
    pub const OBJECT_POINTER: Self = Self(100);
    /// endianity.
    pub const ENDIANITY: Self = Self(101);
    /// elemental.
    pub const ELEMENTAL: Self = Self(102);
    /// pure.
    pub const PURE: Self = Self(103);
    /// recursive.
    pub const RECURSIVE: Self = Self(104);
    /// signature.
    pub const SIGNATURE: Self = Self(105);
    /// main_subprogram.
    pub const MAIN_SUBPROGRAM: Self = Self(106);
    /// data_bit_offset.
    pub const DATA_BIT_OFFSET: Self = Self(107);
    /// const_expr.
    pub const CONST_EXPR: Self = Self(108);
    /// enum_class.
    pub const ENUM_CLASS: Self = Self(109);
    /// linkage_name.
    pub const LINKAGE_NAME: Self = Self(110);
    /// lo_user.
    pub const LO_USER: Self = Self(0x2000);
    /// hi_user.
    pub const HI_USER: Self = Self(0x3FFF);
}

/// DWARF tag (`DW_TAG_*`), modeled as a newtype so arbitrary values are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DwTag(pub u16);

#[allow(non_upper_case_globals)]
impl DwTag {
    /// array_type.
    pub const ARRAY_TYPE: Self = Self(1);
    /// class_type.
    pub const CLASS_TYPE: Self = Self(2);
    /// entry_point.
    pub const ENTRY_POINT: Self = Self(3);
    /// enumeration_type.
    pub const ENUMERATION_TYPE: Self = Self(4);
    /// formal_parameter.
    pub const FORMAL_PARAMETER: Self = Self(5);
    /// imported_declaration.
    pub const IMPORTED_DECLARATION: Self = Self(8);
    /// label.
    pub const LABEL: Self = Self(10);
    /// lexical_block.
    pub const LEXICAL_BLOCK: Self = Self(11);
    /// member.
    pub const MEMBER: Self = Self(13);
    /// pointer_type.
    pub const POINTER_TYPE: Self = Self(0xF);
    /// reference_type.
    pub const REFERENCE_TYPE: Self = Self(0x10);
    /// compile_unit.
    pub const COMPILE_UNIT: Self = Self(17);
    /// string_type.
    pub const STRING_TYPE: Self = Self(18);
    /// structure_type.
    pub const STRUCTURE_TYPE: Self = Self(19);
    /// subroutine_type.
    pub const SUBROUTINE_TYPE: Self = Self(21);
    /// typedef.
    pub const TYPEDEF: Self = Self(22);
    /// union_type.
    pub const UNION_TYPE: Self = Self(23);
    /// unspecified_parameters.
    pub const UNSPECIFIED_PARAMETERS: Self = Self(24);
    /// variant.
    pub const VARIANT: Self = Self(25);
    /// common_block.
    pub const COMMON_BLOCK: Self = Self(26);
    /// common_inclusion.
    pub const COMMON_INCLUSION: Self = Self(27);
    /// inheritance.
    pub const INHERITANCE: Self = Self(28);
    /// inlined_subroutine.
    pub const INLINED_SUBROUTINE: Self = Self(29);
    /// module.
    pub const MODULE: Self = Self(30);
    /// ptr_to_member_type.
    pub const PTR_TO_MEMBER_TYPE: Self = Self(0x1F);
    /// set_type.
    pub const SET_TYPE: Self = Self(0x20);
    /// subrange_type.
    pub const SUBRANGE_TYPE: Self = Self(33);
    /// with_stmt.
    pub const WITH_STMT: Self = Self(34);
    /// access_declaration.
    pub const ACCESS_DECLARATION: Self = Self(35);
    /// base_type.
    pub const BASE_TYPE: Self = Self(36);
    /// catch_block.
    pub const CATCH_BLOCK: Self = Self(37);
    /// const_type.
    pub const CONST_TYPE: Self = Self(38);
    /// constant.
    pub const CONSTANT: Self = Self(39);
    /// enumerator.
    pub const ENUMERATOR: Self = Self(40);
    /// friend.
    pub const FRIEND: Self = Self(42);
    /// namelist.
    pub const NAMELIST: Self = Self(43);
    /// namelist_item.
    pub const NAMELIST_ITEM: Self = Self(44);
    /// packed_type.
    pub const PACKED_TYPE: Self = Self(45);
    /// subprogram.
    pub const SUBPROGRAM: Self = Self(46);
    /// template_type_parameter.
    pub const TEMPLATE_TYPE_PARAMETER: Self = Self(47);
    /// template_value_parameter.
    pub const TEMPLATE_VALUE_PARAMETER: Self = Self(48);
    /// thrown_type.
    pub const THROWN_TYPE: Self = Self(49);
    /// try_block.
    pub const TRY_BLOCK: Self = Self(50);
    /// variant_part.
    pub const VARIANT_PART: Self = Self(51);
    /// variable.
    pub const VARIABLE: Self = Self(52);
    /// volatile_type.
    pub const VOLATILE_TYPE: Self = Self(53);
    /// dwarf_procedure.
    pub const DWARF_PROCEDURE: Self = Self(54);
    /// restrict_type.
    pub const RESTRICT_TYPE: Self = Self(55);
    /// interface_type.
    pub const INTERFACE_TYPE: Self = Self(56);
    /// namespace.
    pub const NAMESPACE: Self = Self(57);
    /// imported_module.
    pub const IMPORTED_MODULE: Self = Self(58);
    /// unspecified_type.
    pub const UNSPECIFIED_TYPE: Self = Self(59);
    /// partial_unit.
    pub const PARTIAL_UNIT: Self = Self(60);
    /// imported_unit.
    pub const IMPORTED_UNIT: Self = Self(61);
    /// condition.
    pub const CONDITION: Self = Self(0x3F);
    /// shared_type.
    pub const SHARED_TYPE: Self = Self(0x40);
    /// type_unit.
    pub const TYPE_UNIT: Self = Self(65);
    /// rvalue_reference_type.
    pub const RVALUE_REFERENCE_TYPE: Self = Self(66);
    /// template_alias.
    pub const TEMPLATE_ALIAS: Self = Self(67);
    /// lo_user.
    pub const LO_USER: Self = Self(0x4080);
    /// hi_user.
    pub const HI_USER: Self = Self(0xFFFF);
}

/// DWARF attribute form (`DW_FORM_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DwForm {
    /// addr.
    Addr = 1,
    /// block2.
    Block2 = 3,
    /// block4.
    Block4 = 4,
    /// data2.
    Data2 = 5,
    /// data4.
    Data4 = 6,
    /// data8.
    Data8 = 7,
    /// strng (non-standard spelling of the standard `string` form).
    Strng = 8,
    /// block.
    Block = 9,
    /// block1.
    Block1 = 10,
    /// data1.
    Data1 = 11,
    /// flag.
    Flag = 12,
    /// sdata.
    Sdata = 13,
    /// strp.
    Strp = 14,
    /// udata.
    Udata = 0xF,
    /// ref_addr.
    RefAddr = 0x10,
    /// ref1.
    Ref1 = 17,
    /// ref2.
    Ref2 = 18,
    /// ref4.
    Ref4 = 19,
    /// ref8.
    Ref8 = 20,
    /// ref_udata.
    RefUdata = 21,
    /// indirect (unsupported: reading this form reports an error).
    Indirect = 22,
    /// sec_offset.
    SecOffset = 23,
    /// exprloc.
    Exprloc = 24,
    /// flag_present.
    FlagPresent = 25,
    /// ref_sig8.
    RefSig8 = 0x20,
}

impl DwForm {
    fn from_u64(v: u64) -> Option<Self> {
        Some(match v {
            1 => DwForm::Addr,
            3 => DwForm::Block2,
            4 => DwForm::Block4,
            5 => DwForm::Data2,
            6 => DwForm::Data4,
            7 => DwForm::Data8,
            8 => DwForm::Strng,
            9 => DwForm::Block,
            10 => DwForm::Block1,
            11 => DwForm::Data1,
            12 => DwForm::Flag,
            13 => DwForm::Sdata,
            14 => DwForm::Strp,
            0xF => DwForm::Udata,
            0x10 => DwForm::RefAddr,
            17 => DwForm::Ref1,
            18 => DwForm::Ref2,
            19 => DwForm::Ref4,
            20 => DwForm::Ref8,
            21 => DwForm::RefUdata,
            22 => DwForm::Indirect,
            23 => DwForm::SecOffset,
            24 => DwForm::Exprloc,
            25 => DwForm::FlagPresent,
            0x20 => DwForm::RefSig8,
            _ => return None,
        })
    }
}

/// DWARF language (`DW_LANG_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
#[allow(non_camel_case_types)]
pub enum DwLang {
    /// Unknown.
    Unknown = 0,
    /// ISO C:1989.
    C89 = 1,
    /// Non-standard C (K&R).
    C = 2,
    /// ISO Ada:1983.
    Ada83 = 3,
    /// ISO C++:1998.
    C_plus_plus = 4,
    /// Cobol 74.
    Cobol74 = 5,
    /// Cobol 85.
    Cobol85 = 6,
    /// Fortran 77.
    Fortran77 = 7,
    /// Fortran 90.
    Fortran90 = 8,
    /// Pascal 83.
    Pascal83 = 9,
    /// Modula 2.
    Modula2 = 10,
    /// Java.
    Java = 11,
    /// ISO C:1999.
    C99 = 12,
    /// ISO Ada:1995.
    Ada95 = 13,
    /// Fortran 95.
    Fortran95 = 14,
    /// PL/I.
    PLI = 0xF,
    /// Objective C.
    ObjC = 0x10,
    /// Objective C++.
    ObjC_plus_plus = 17,
    /// UPC.
    UPC = 18,
    /// D.
    D = 19,
    /// Python.
    Python = 20,
    /// OpenCL.
    OpenCL = 21,
    /// Go.
    Go = 22,
    /// Modula 3.
    Modula3 = 23,
    /// Haskell.
    Haskell = 24,
    /// C++ 03.
    C_plus_plus_3 = 25,
    /// C++ 11.
    C_plus_plus_11 = 26,
    /// OCaml.
    OCaml = 27,
    /// Rust.
    Rust = 28,
    /// C11.
    C11 = 29,
    /// Swift.
    Swift = 30,
    /// Julia.
    Julia = 0x1F,
    /// Dylan.
    Dylan = 0x20,
    /// C++ 14.
    C_plus_plus_14 = 33,
    /// Fortran 03.
    Fortran03 = 34,
    /// Fortran 08.
    Fortran08 = 35,
    /// RenderScript.
    RenderScript = 36,
    /// BLISS.
    BLISS = 37,
    /// lo_user.
    lo_user = 0x8000,
    /// hi_user.
    hi_user = 0xFFFF,
    /// MIPS assembler.
    Mips_Assembler = 32769,
    /// Upc.
    Upc = 34661,
    /// HP Bliss.
    HP_Bliss = 32771,
    /// HP Basic91.
    HP_Basic91 = 32772,
    /// HP Pascal91.
    HP_Pascal91 = 32773,
    /// HP IMacro.
    HP_IMacro = 32774,
    /// HP assembler.
    HP_Assembler = 32775,
}

impl DwLang {
    /// Raw value to enum conversion; unknown values return `None`.
    pub fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0 => DwLang::Unknown,
            1 => DwLang::C89,
            2 => DwLang::C,
            3 => DwLang::Ada83,
            4 => DwLang::C_plus_plus,
            5 => DwLang::Cobol74,
            6 => DwLang::Cobol85,
            7 => DwLang::Fortran77,
            8 => DwLang::Fortran90,
            9 => DwLang::Pascal83,
            10 => DwLang::Modula2,
            11 => DwLang::Java,
            12 => DwLang::C99,
            13 => DwLang::Ada95,
            14 => DwLang::Fortran95,
            0xF => DwLang::PLI,
            0x10 => DwLang::ObjC,
            17 => DwLang::ObjC_plus_plus,
            18 => DwLang::UPC,
            19 => DwLang::D,
            20 => DwLang::Python,
            21 => DwLang::OpenCL,
            22 => DwLang::Go,
            23 => DwLang::Modula3,
            24 => DwLang::Haskell,
            25 => DwLang::C_plus_plus_3,
            26 => DwLang::C_plus_plus_11,
            27 => DwLang::OCaml,
            28 => DwLang::Rust,
            29 => DwLang::C11,
            30 => DwLang::Swift,
            0x1F => DwLang::Julia,
            0x20 => DwLang::Dylan,
            33 => DwLang::C_plus_plus_14,
            34 => DwLang::Fortran03,
            35 => DwLang::Fortran08,
            36 => DwLang::RenderScript,
            37 => DwLang::BLISS,
            0x8000 => DwLang::lo_user,
            0xFFFF => DwLang::hi_user,
            32769 => DwLang::Mips_Assembler,
            34661 => DwLang::Upc,
            32771 => DwLang::HP_Bliss,
            32772 => DwLang::HP_Basic91,
            32773 => DwLang::HP_Pascal91,
            32774 => DwLang::HP_IMacro,
            32775 => DwLang::HP_Assembler,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    /// null.
    None,
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I64(i64),
    Str(String),
    Bytes(Vec<u8>),
}

impl AttrValue {
    pub fn as_u64(&self) -> u64 {
        match self {
            AttrValue::U8(v) => u64::from(*v),
            AttrValue::U16(v) => u64::from(*v),
            AttrValue::U32(v) => u64::from(*v),
            AttrValue::U64(v) => *v,
            AttrValue::I64(v) => *v as u64,
            _ => 0,
        }
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            AttrValue::U8(v) => i64::from(*v),
            AttrValue::U16(v) => i64::from(*v),
            AttrValue::U32(v) => i64::from(*v),
            AttrValue::U64(v) => *v as i64,
            AttrValue::I64(v) => *v,
            _ => 0,
        }
    }

    pub fn as_i32(&self) -> i32 {
        self.as_i64() as i32
    }

    pub fn as_u8_checked(&self) -> Option<u8> {
        u8::try_from(self.as_i64()).ok()
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            AttrValue::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            AttrValue::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub at: DwAt,
    pub form: DwForm,
    pub value: AttrValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    CompileUnit { cu: usize },
    Variable,
    Array,
    Member,
    VarBase,
    Plain,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbbrevEntry {
    pub tag: DwTag,
    pub ref_id: i64,
    pub has_childs: bool,
    pub attributes: Vec<(DwAt, Attribute)>,
    pub parent: Option<usize>,
    pub childs: Vec<usize>,
    pub specification: Option<usize>,
    pub kind: EntryKind,
    pub elf_address: Option<u64>,
}

impl AbbrevEntry {
    pub fn attribute<'a>(&'a self, entries: &'a [AbbrevEntry], at: DwAt) -> Option<&'a Attribute> {
        self.attributes
            .iter()
            .find(|(a, _)| *a == at)
            .map(|(_, attr)| attr)
            .or_else(|| {
                self.specification.and_then(|si| {
                    entries
                        .get(si)
                        .and_then(|e| e.attributes.iter().find(|(a, _)| *a == at))
                        .map(|(_, attr)| attr)
                })
            })
    }

    pub fn name(&self, entries: &[AbbrevEntry]) -> String {
        self.attribute(entries, DwAt::NAME)
            .and_then(|a| a.value.as_str())
            .unwrap_or("")
            .to_string()
    }

    pub fn is_declaration(&self, entries: &[AbbrevEntry]) -> bool {
        self.attribute(entries, DwAt::DECLARATION).is_some()
    }

    pub fn is_prototyped(&self, entries: &[AbbrevEntry]) -> bool {
        self.attribute(entries, DwAt::PROTOTYPED).is_some()
    }

    pub fn has_type_info(&self, entries: &[AbbrevEntry]) -> bool {
        self.attribute(entries, DwAt::TYPE).is_some()
    }

    pub fn size_bits(&self, entries: &[AbbrevEntry]) -> u64 {
        if let Some(a) = self.attribute(entries, DwAt::BIT_SIZE) {
            return a.value.as_u64();
        }
        self.byte_size(entries) * 8
    }

    pub fn byte_size(&self, entries: &[AbbrevEntry]) -> u64 {
        self.attribute(entries, DwAt::BYTE_SIZE)
            .map(|a| a.value.as_u64())
            .unwrap_or(0)
    }

    pub fn bit_offset(&self, entries: &[AbbrevEntry]) -> u32 {
        self.attribute(entries, DwAt::BIT_OFFSET)
            .map(|a| a.value.as_u64() as u32)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompilationUnitHeader {
    pub big_endian: bool,
    pub is_64_bit: bool,
    pub length: u64,
    pub version: u16,
    pub offset: u64,
    pub address_size: u8,
    pub root_entries: Vec<usize>,
    pub type_signature: Option<u64>,
    pub type_offset: Option<u64>,
}

impl CompilationUnitHeader {
    pub fn language(&self, info: &DwarfInfo) -> DwLang {
        self.root_entries
            .first()
            .and_then(|&i| info.entries[i].attribute(&info.entries, DwAt::LANGUAGE))
            .and_then(|a| DwLang::from_u32(a.value.as_u64() as u32))
            .unwrap_or(DwLang::Unknown)
    }

    pub fn producer(&self, info: &DwarfInfo) -> String {
        self.root_entries
            .first()
            .and_then(|&i| info.entries[i].attribute(&info.entries, DwAt::PRODUCER))
            .and_then(|a| a.value.as_str())
            .unwrap_or("")
            .to_string()
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolValue {
    pub parent: Option<usize>,
    pub childs: Option<Vec<usize>>,
    pub address: u64,
    pub size: u64,
    pub entry_size: u64,
    pub bit_offset: u32,
    pub array_dim: Option<Vec<i64>>,
    pub var_base: usize,
    pub typ: usize,
    pub is_expanded: bool,
}

impl SymbolValue {
    pub fn try_get_bit_mask(&self) -> u64 {
        if self.size.is_multiple_of(8) {
            return u64::MAX;
        }
        build_bitmask(self.size as i64, self.bit_offset as i32)
    }

    pub fn try_get_data_type(&self) -> DataType {
        let mut result = DataType::UByte;
        if self.try_get_bit_mask() != u64::MAX {
            match self.size + u64::from(self.bit_offset) {
                0..=8 => {}
                9..=16 => result = DataType::UWord,
                17..=32 => result = DataType::ULong,
                _ => result = DataType::AUInt64,
            }
        }
        result
    }

    pub fn array_element_count(&self) -> i64 {
        self.array_dim.as_ref().map_or(0, |d| d.iter().product())
    }

    pub fn flat_index(&self, indexes: &[i64]) -> i64 {
        let Some(dims) = &self.array_dim else {
            return -1;
        };
        if indexes.len() > dims.len() {
            return -1;
        }
        let mut num: i64 = 1;
        for i in 0..indexes.len().saturating_sub(1) {
            num *= indexes[i] * dims[i];
        }
        let num2 = indexes[indexes.len() - 1] + if indexes.len() > 1 { num } else { 0 };
        if num2 >= self.array_element_count() {
            return -1;
        }
        num2
    }

    pub fn build_array_indexes(&self) -> Vec<String> {
        let Some(dims) = &self.array_dim else {
            return Vec::new();
        };
        let n = dims.len();
        let mut strides = vec![0i64; n];
        strides[n - 1] = dims[n - 1];
        for i in (0..n - 1).rev() {
            strides[i] = strides[i + 1] * dims[i];
        }
        let count = self.array_element_count();
        let mut out = Vec::with_capacity(count.max(0) as usize);
        for i in 0..count {
            let mut s = String::new();
            for d in (0..n).rev() {
                let idx = if d == n - 1 {
                    if i > 0 {
                        i % strides[d]
                    } else {
                        i
                    }
                } else {
                    i / strides[d + 1]
                };
                s.insert_str(0, &format!("[{idx}]"));
            }
            out.push(s);
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolsTree {
    pub roots: IndexMap<String, usize>,
}

// ---------------------------------------------------------------------------
// DWARFInfo
// ---------------------------------------------------------------------------

struct ParseCtx<'a> {
    data: &'a [u8],
    abbrev: Option<&'a SectionValue>,
    debug_str: Option<&'a SectionValue>,
    big_endian: bool,
}

impl ParseCtx<'_> {
    fn section_data(&self, section: &SectionValue) -> Result<&[u8]> {
        let start = section.section.ofs as usize;
        let end = start.saturating_add(section.section.size as usize);
        if end > self.data.len() {
            return parse_err(
                section.section.ofs,
                format!("section {:?} out of file bounds", section.name),
            );
        }
        Ok(&self.data[start..end])
    }
}

#[derive(Debug)]
struct AbbrevDef {
    tag: DwTag,
    has_childs: bool,
    attrs: Vec<(DwAt, DwForm)>,
}

#[derive(Debug)]
pub struct DwarfInfo {
    pub compilation_units: Vec<CompilationUnitHeader>,
    pub entries: Vec<AbbrevEntry>,
    pub symbols: SymbolsTree,
    sym_values: Vec<SymbolValue>,
    cu_index: IndexMap<i64, usize>,
    tu_index: IndexMap<i64, usize>,
    type_units: IndexMap<u64, usize>,
    machine: u16,
    big_endian: bool,
}

impl DwarfInfo {
    pub(crate) fn new(elf: &ElfFile) -> Result<DwarfInfo> {
        let find = |name: &str| elf.sections.iter().find(|s| s.name == name);
        let ctx = ParseCtx {
            data: &elf.data,
            abbrev: find(SECTION_DEBUG_ABBREV),
            debug_str: find(SECTION_DEBUG_STR),
            big_endian: elf.header.is_big_endian(),
        };
        let mut info = DwarfInfo {
            compilation_units: Vec::new(),
            entries: Vec::new(),
            symbols: SymbolsTree::default(),
            sym_values: Vec::new(),
            cu_index: IndexMap::new(),
            tu_index: IndexMap::new(),
            type_units: IndexMap::new(),
            machine: elf.header.machine,
            big_endian: ctx.big_endian,
        };
        if let Some(types) = find(SECTION_DEBUG_TYPES) {
            info.parse_unit_section(&ctx, types, true, &elf.symbols)?;
        }
        if let Some(debug_info) = find(SECTION_DEBUG_INFO) {
            info.parse_unit_section(&ctx, debug_info, false, &elf.symbols)?;
        }
        let candidates: Vec<usize> = info.cu_index.values().copied().collect();
        for idx in candidates {
            if info.entries[idx].kind != EntryKind::Variable {
                continue;
            }
            if info.location_of(idx) != u64::MAX {
                info.add_symbol(idx, None, 0);
            }
        }
        Ok(info)
    }

    /// Parses all compilation or type units from one DWARF section.
    fn parse_unit_section(
        &mut self,
        ctx: &ParseCtx,
        section: &SectionValue,
        is_type_unit: bool,
        elf_symbols: &IndexMap<String, crate::elf::SymbolValue>,
    ) -> Result<()> {
        let data = ctx.section_data(section)?;
        let mut r = Reader::new(data, ctx.big_endian);
        while r.position() < data.len() {
            let cu_start = r.position() as i64;
            let (header, header_bytes) = parse_cu_header(&mut r, is_type_unit, ctx.big_endian)?;
            let cu_idx = self.compilation_units.len();
            if let Some(sig) = header.type_signature {
                self.type_units.insert(sig, cu_idx);
            }
            self.compilation_units.push(header);
            let mut data_start = r.position() as i64;
            if let Some(toff) = self.compilation_units[cu_idx].type_offset {
                data_start += toff as i64;
            }
            let abs_start = r.position();
            let len = self.compilation_units[cu_idx].length as usize - header_bytes;
            let dict_is_tu = is_type_unit;
            self.parse_dies(
                ctx,
                data,
                cu_idx,
                cu_start,
                data_start,
                abs_start,
                len,
                dict_is_tu,
                elf_symbols,
            )?;
            r.seek(abs_start + len);
        }
        Ok(())
    }

    /// CompilationUnitHeader, long, long, long, long, IDictionary)`.
    #[allow(clippy::too_many_arguments)]
    fn parse_dies(
        &mut self,
        ctx: &ParseCtx,
        unit_data: &[u8],
        cu_idx: usize,
        cu_start: i64,
        data_start: i64,
        abs_start: usize,
        len: usize,
        is_type_unit: bool,
        elf_symbols: &IndexMap<String, crate::elf::SymbolValue>,
    ) -> Result<()> {
        let abbrev_offset = self.compilation_units[cu_idx].offset;
        let abbrevs = parse_abbrevs(ctx, abbrev_offset)?;
        let mut r = Reader::new(unit_data, ctx.big_endian);
        let end = abs_start + len;
        r.seek(abs_start);
        let mut stack: Vec<usize> = Vec::new();
        while r.position() < end {
            let ref_id = r.position() as i64 + data_start - abs_start as i64;
            let code = r.uleb128()?;
            if code == 0 {
                if stack.is_empty() {
                    break;
                }
                stack.pop();
                continue;
            }
            let Some(def) = abbrevs.get(&code) else {
                continue;
            };
            let parent = stack.last().copied();
            let kind = match def.tag {
                t if t == DwTag::COMPILE_UNIT
                    || t == DwTag::PARTIAL_UNIT
                    || t == DwTag::TYPE_UNIT =>
                {
                    EntryKind::CompileUnit { cu: cu_idx }
                }
                t if t == DwTag::VARIABLE => EntryKind::Variable,
                t if t == DwTag::ARRAY_TYPE => EntryKind::Array,
                t if t == DwTag::MEMBER => EntryKind::Member,
                t if t == DwTag::STRUCTURE_TYPE || t == DwTag::UNION_TYPE => EntryKind::VarBase,
                _ => EntryKind::Plain,
            };
            let attributes = read_attribute_values(
                &mut r,
                def,
                &self.compilation_units[cu_idx],
                cu_start,
                data_start,
                ctx,
            )?;
            let idx = self.entries.len();
            let dict = if is_type_unit {
                &self.tu_index
            } else {
                &self.cu_index
            };
            let specification = attributes
                .iter()
                .find(|(at, _)| *at == DwAt::SPECIFICATION)
                .and_then(|(_, a)| dict.get(&a.value.as_i64()).copied());
            self.entries.push(AbbrevEntry {
                tag: def.tag,
                ref_id,
                has_childs: def.has_childs,
                attributes,
                parent,
                childs: Vec::new(),
                specification,
                kind,
                elf_address: None,
            });
            if kind == EntryKind::Variable
                && self.entries[idx]
                    .attribute(&self.entries, DwAt::LOCATION)
                    .is_none()
            {
                let name = self.entries[idx].name(&self.entries);
                if let Some(sv) = elf_symbols.get(&name) {
                    self.entries[idx].elf_address = Some(sv.symbol.value);
                }
            }
            if let Some(p) = parent {
                self.entries[p].childs.push(idx);
            } else {
                self.compilation_units[cu_idx].root_entries.push(idx);
            }
            if is_type_unit {
                self.tu_index.insert(ref_id, idx);
            } else {
                self.cu_index.insert(ref_id, idx);
            }
            if def.has_childs {
                stack.push(idx);
            }
        }
        Ok(())
    }

    fn location_of(&self, var_idx: usize) -> u64 {
        let e = &self.entries[var_idx];
        if e.kind != EntryKind::Variable {
            return 0;
        }
        if let Some(addr) = e.elf_address {
            return addr;
        }
        let Some(attr) = e.attribute(&self.entries, DwAt::LOCATION) else {
            return u64::MAX;
        };
        match attr.form {
            DwForm::Block2 | DwForm::Block4 | DwForm::Block | DwForm::Block1 | DwForm::Exprloc => {
                match attr.value.as_bytes() {
                    Some(b) => eval_location(b, self.addr_size_of(var_idx), self.big_endian),
                    None => u64::MAX,
                }
            }
            DwForm::Data2 => attr
                .value
                .as_u8_checked()
                .map(u64::from)
                .unwrap_or(u64::MAX),
            DwForm::Data4 => u64::from(attr.value.as_u64() as u32),
            DwForm::Data8 => attr.value.as_u64(),
            _ => u64::MAX,
        }
    }

    fn member_bit_offset(&self, member_idx: usize) -> i64 {
        let e = &self.entries[member_idx];
        if let Some(attr) = e.attribute(&self.entries, DwAt::DATA_MEMBER_LOCATION) {
            return match &attr.value {
                AttrValue::Bytes(b) => {
                    8 * eval_location(b, self.addr_size_of(member_idx), self.big_endian) as i64
                }
                v => i64::from(v.as_i32()) * 8,
            };
        }
        if let Some(attr) = e.attribute(&self.entries, DwAt::DATA_BIT_OFFSET) {
            return i64::from(attr.value.as_i32());
        }
        0
    }

    fn addr_size_of(&self, entry_idx: usize) -> u8 {
        let mut idx = entry_idx;
        loop {
            match self.entries.get(idx).map(|e| e.kind) {
                Some(EntryKind::CompileUnit { cu }) => {
                    return self.compilation_units[cu].address_size;
                }
                Some(_) => match self.entries[idx].parent {
                    Some(p) => idx = p,
                    None => return 4,
                },
                None => return 4,
            }
        }
    }

    pub fn get_type_ref(
        &self,
        entry_idx: usize,
        stack: &mut Vec<usize>,
        byte_size: &mut u64,
    ) -> Option<usize> {
        let cu_index = &self.cu_index;
        self.get_type_ref_in(entry_idx, cu_index, stack, byte_size)
    }

    fn get_type_ref_in(
        &self,
        entry_idx: usize,
        dict: &IndexMap<i64, usize>,
        stack: &mut Vec<usize>,
        byte_size: &mut u64,
    ) -> Option<usize> {
        *byte_size = 0;
        let entry = &self.entries[entry_idx];
        let t = entry.tag;
        if t == DwTag::ENUMERATION_TYPE
            || t == DwTag::STRUCTURE_TYPE
            || t == DwTag::SUBROUTINE_TYPE
            || t == DwTag::UNION_TYPE
            || t == DwTag::INLINED_SUBROUTINE
            || t == DwTag::BASE_TYPE
        {
            let bs = entry.byte_size(&self.entries);
            if bs != 0 {
                *byte_size = bs;
                return Some(entry_idx);
            }
        }
        let (next_idx, dict) = self.type_ref_target(entry_idx, dict)?;
        let next = &self.entries[next_idx];
        let t = next.tag;
        if t == DwTag::ARRAY_TYPE
            || t == DwTag::CLASS_TYPE
            || t == DwTag::POINTER_TYPE
            || t == DwTag::STRING_TYPE
            || t == DwTag::TYPEDEF
            || t == DwTag::CONST_TYPE
            || t == DwTag::VOLATILE_TYPE
            || t == DwTag::UNSPECIFIED_TYPE
            || t == DwTag::LO_USER
        {
            if next.has_type_info(&self.entries) {
                stack.push(next_idx);
                return self.get_type_ref_in(next_idx, dict, stack, byte_size);
            }
            return None;
        }
        if t == DwTag::TYPE_UNIT {
            for k in 0..next.childs.len() {
                let c = next.childs[k];
                let r = self.get_type_ref_in(c, dict, stack, byte_size);
                if *byte_size != 0 {
                    return r;
                }
            }
            return None;
        }
        self.get_type_ref_in(next_idx, dict, stack, byte_size)
    }

    fn type_ref_target<'a>(
        &'a self,
        entry_idx: usize,
        dict: &'a IndexMap<i64, usize>,
    ) -> Option<(usize, &'a IndexMap<i64, usize>)> {
        let attr = self.entries[entry_idx].attribute(&self.entries, DwAt::TYPE)?;
        let mut key: i64 = -1;
        let mut dict = dict;
        if let AttrValue::U64(v) = attr.value {
            if let Some(&tu) = self.type_units.get(&v) {
                key = self.entries[*self.compilation_units[tu].root_entries.first()?].ref_id;
                dict = &self.tu_index;
            }
        } else {
            key = attr.value.as_i64();
        }
        let &target = dict.get(&key)?;
        Some((target, dict))
    }

    pub fn get_type_str(&self, entry_idx: usize) -> String {
        let mut stack = Vec::new();
        let mut byte_size = 0u64;
        let type_ref = self.get_type_ref(entry_idx, &mut stack, &mut byte_size);
        let mut sb = String::new();
        let name = type_ref
            .map(|i| self.entries[i].name(&self.entries))
            .unwrap_or_default();
        if !name.is_empty() {
            sb.push_str(&name);
            sb.push(' ');
        }
        let mut has_name = !sb.is_empty();
        for &item in stack.iter().rev() {
            let e = &self.entries[item];
            if e.tag == DwTag::TYPEDEF {
                if !has_name {
                    has_name = true;
                    sb.push_str(&e.name(&self.entries));
                    sb.push(' ');
                }
            } else if e.tag == DwTag::POINTER_TYPE || e.tag == DwTag::STRING_TYPE {
                sb.push('*');
            } else if e.tag == DwTag::ARRAY_TYPE {
                sb.push_str("[]");
            } else if e.tag == DwTag::VOLATILE_TYPE {
                sb.insert_str(0, "volatile ");
            }
        }
        sb
    }

    pub fn get_variable(&self, entry_idx: usize) -> Option<usize> {
        let mut idx = Some(entry_idx);
        while let Some(i) = idx {
            if self.entries[i].tag == DwTag::VARIABLE {
                return Some(i);
            }
            idx = self.entries[i].parent;
        }
        None
    }

    pub fn expand_symbol(&mut self, sym_id: usize) {
        let type_idx = self.sym_values[sym_id].typ;
        let t = self.entries[type_idx].tag;
        if t != DwTag::CLASS_TYPE && t != DwTag::STRUCTURE_TYPE && t != DwTag::UNION_TYPE {
            return;
        }
        for k in 0..self.entries[type_idx].childs.len() {
            let ce = self.entries[type_idx].childs[k];
            if self.entries[ce].kind != EntryKind::Member {
                continue;
            }
            let name = self.entries[ce].name(&self.entries);
            if self.chain_contains(sym_id, &name) {
                continue;
            }
            let bit_off = self.member_bit_offset(ce);
            let base = self.sym_values[sym_id]
                .address
                .wrapping_add((bit_off as u64) / 8);
            self.add_symbol(ce, Some(sym_id), base);
        }
    }

    fn chain_contains(&self, sym_id: usize, name: &str) -> bool {
        let mut id = Some(sym_id);
        while let Some(i) = id {
            if self.entries[self.sym_values[i].var_base].name(&self.entries) == name {
                return true;
            }
            id = self.sym_values[i].parent;
        }
        false
    }

    /// SymbolValue, ulong)`.
    fn add_symbol(
        &mut self,
        var_idx: usize,
        parent: Option<usize>,
        base_addr: u64,
    ) -> Option<usize> {
        let mut size = self.entries[var_idx].size_bits(&self.entries);
        let has_own_size = size != 0;
        let mut stack = Vec::new();
        let mut byte_size = 0u64;
        let type_ref = self.get_type_ref(var_idx, &mut stack, &mut byte_size);
        if !has_own_size {
            if let Some(tr) = type_ref {
                size = self.entries[tr].size_bits(&self.entries);
            }
        }
        let type_ref = type_ref?;
        {
            let tr = &self.entries[type_ref];
            if tr.is_declaration(&self.entries) || tr.is_prototyped(&self.entries) || size == 0 {
                return None;
            }
        }
        let bit_offset = self.entries[var_idx].bit_offset(&self.entries);
        let addr = base_addr.wrapping_add(self.location_of(var_idx));
        let mut dims: Option<Vec<i64>> = None;
        for &item in stack.iter().rev() {
            if self.entries[item].tag == DwTag::ARRAY_TYPE {
                dims = self.array_dims(item);
                if dims.is_some() {
                    break;
                }
            }
        }
        let mut array_dim: Option<Vec<i64>> = None;
        if let Some(list) = dims {
            if !list.is_empty() {
                for &ub in &list {
                    if !has_own_size {
                        size = size.wrapping_mul((ub + 1) as u64);
                    }
                }
                array_dim = Some(list.iter().map(|&ub| ub + 1).collect());
            }
        }
        let entry_size = if array_dim.is_some() { byte_size } else { 0 };
        let id = self.sym_values.len();
        let mut bit_offset = bit_offset;
        if let Some(p) = parent {
            if (140..=142).contains(&self.machine) {
                let psize = self.sym_values[p].size;
                bit_offset = (2u64.wrapping_mul(psize) as u32).wrapping_sub(bit_offset);
                bit_offset = bit_offset.wrapping_sub(size as u32);
            }
            let childs = self.sym_values[p].childs.get_or_insert_with(Vec::new);
            childs.push(id);
            self.sym_values[p].is_expanded = true;
        }
        self.sym_values.push(SymbolValue {
            parent,
            childs: None,
            address: addr,
            size,
            entry_size,
            bit_offset,
            array_dim,
            var_base: var_idx,
            typ: type_ref,
            is_expanded: false,
        });
        if parent.is_none() {
            let name = self.entries[var_idx].name(&self.entries);
            self.symbols.roots.insert(name, id);
        }
        Some(id)
    }

    fn array_dims(&self, array_idx: usize) -> Option<Vec<i64>> {
        let mut dims = Vec::new();
        for &c in &self.entries[array_idx].childs {
            if self.entries[c].tag != DwTag::SUBRANGE_TYPE {
                continue;
            }
            if let Some(attr) = self.entries[c].attribute(&self.entries, DwAt::UPPER_BOUND) {
                dims.push(attr.value.as_i64());
            }
        }
        if dims.is_empty() {
            None
        } else {
            Some(dims)
        }
    }

    pub(crate) fn lookup_path(&mut self, path: &[SymbolPathElem]) -> Option<UpdaterSymbols> {
        if path.is_empty() {
            return None;
        }
        self.lookup_at(path, 0, None, 0)
    }

    fn lookup_at(
        &mut self,
        path: &[SymbolPathElem],
        i: usize,
        level: Option<Vec<usize>>,
        base: u64,
    ) -> Option<UpdaterSymbols> {
        let seg = path.get(i)?;
        let sym_id = if i > 0 {
            level?.into_iter().find(|&id| {
                self.entries[self.sym_values[id].var_base].name(&self.entries) == seg.name
            })?
        } else {
            let id = *self.symbols.roots.get(&seg.name)?;
            if path.len() > 1 && !self.sym_values[id].is_expanded {
                self.expand_symbol(id);
            }
            id
        };
        let idx_off = match &seg.indexes {
            Some(ix) => self.sym_values[sym_id].flat_index(ix),
            None => 0,
        };
        if idx_off < 0 {
            return None;
        }
        if i + 1 == path.len() {
            let sv = &self.sym_values[sym_id];
            let size = if seg.indexes.is_some() {
                sv.entry_size * 8
            } else {
                sv.size
            };
            return Some(UpdaterSymbols {
                address: (sv.address as i64 + idx_off * sv.entry_size as i64) as u64 + base,
                size,
                bit_offset: sv.bit_offset,
                array_idx: idx_off,
                array_dim: None,
                tag: UpdaterTag::Dwarf(sym_id),
            });
        }
        if !self.sym_values[sym_id].is_expanded {
            self.expand_symbol(sym_id);
        }
        let mut base = base;
        if idx_off > 0 {
            base += (idx_off * self.sym_values[sym_id].entry_size as i64) as u64;
        }
        let childs = self.sym_values[sym_id].childs.clone();
        self.lookup_at(path, i + 1, childs, base)
    }

    pub fn enum_symbols(&mut self) -> Vec<(String, UpdaterSymbols)> {
        let mut out = Vec::new();
        let roots: Vec<usize> = self.symbols.roots.values().copied().collect();
        for id in roots {
            self.enum_one(id, "", &mut out);
        }
        out
    }

    fn enum_one(&mut self, id: usize, prefix: &str, out: &mut Vec<(String, UpdaterSymbols)>) {
        if self.sym_values[id].parent.is_none() && !self.sym_values[id].is_expanded {
            self.expand_symbol(id);
        }
        let name = self.qualified_name(id, prefix, None);
        let (address, size, bit_offset) = {
            let sv = &self.sym_values[id];
            (sv.address, sv.size, sv.bit_offset)
        };
        out.push((
            name.clone(),
            UpdaterSymbols {
                address,
                size,
                bit_offset,
                array_idx: -1,
                array_dim: None,
                tag: UpdaterTag::Dwarf(id),
            },
        ));
        if let Some(dims) = self.sym_values[id].array_dim.clone() {
            let indexes = build_array_indexes(&dims);
            let entry_size = self.sym_values[id].entry_size;
            for (i, ix) in indexes.iter().enumerate() {
                let n = self.qualified_name(id, prefix, Some(ix));
                let sz = if entry_size != 0 {
                    entry_size * 8
                } else {
                    size
                };
                out.push((
                    n.clone(),
                    UpdaterSymbols {
                        address: address + i as u64 * entry_size,
                        size: sz,
                        bit_offset,
                        array_idx: i as i64,
                        array_dim: None,
                        tag: UpdaterTag::Dwarf(id),
                    },
                ));
                if let Some(childs) = self.sym_values[id].childs.clone() {
                    for c in childs {
                        self.enum_one(c, &n, out);
                    }
                }
            }
        } else if let Some(childs) = self.sym_values[id].childs.clone() {
            for c in childs {
                self.enum_one(c, &name, out);
            }
        }
    }

    fn qualified_name(&self, id: usize, prefix: &str, index_str: Option<&str>) -> String {
        let mut sb = String::new();
        if !prefix.is_empty() {
            sb.push_str(prefix);
            sb.push('.');
        }
        sb.push_str(&self.entries[self.sym_values[id].var_base].name(&self.entries));
        if let Some(ix) = index_str {
            sb.push_str(ix);
        }
        sb
    }
}

fn eval_location(bytes: &[u8], address_size: u8, big_endian: bool) -> u64 {
    match bytes.first() {
        // DW_OP_addr
        Some(3) => {
            let mut r = Reader::new(&bytes[1..], big_endian);
            r.uint_n(usize::from(address_size)).unwrap_or(u64::MAX)
        }
        // DW_OP_plus_uconst
        Some(35) => {
            let mut r = Reader::new(&bytes[1..], big_endian);
            r.uleb128().unwrap_or(u64::MAX)
        }
        _ => u64::MAX,
    }
}

fn build_array_indexes(dims: &[i64]) -> Vec<String> {
    let sv = SymbolValue {
        parent: None,
        childs: None,
        address: 0,
        size: 0,
        entry_size: 0,
        bit_offset: 0,
        array_dim: Some(dims.to_vec()),
        var_base: 0,
        typ: 0,
        is_expanded: false,
    };
    sv.build_array_indexes()
}

fn parse_cu_header(
    r: &mut Reader,
    is_type_unit: bool,
    big_endian: bool,
) -> Result<(CompilationUnitHeader, usize)> {
    let mut length = u64::from(r.u32()?);
    let mut is64 = false;
    if length == u64::from(u32::MAX) {
        length = r.u64()?;
        is64 = true;
    }
    let version = r.u16()?;
    if version < 1 {
        return parse_err(
            r.position() as u64,
            format!("unsupported DWARF version {version}"),
        );
    }
    let offset = r.uint_n(if is64 { 8 } else { 4 })?;
    let address_size = r.u8()?;
    let mut bytes = 2 + if is64 { 8 } else { 4 } + 1;
    let (type_signature, type_offset) = if is_type_unit {
        let sig = r.u64()?;
        let toff = r.uint_n(if is64 { 8 } else { 4 })?;
        bytes += 8 + if is64 { 8 } else { 4 };
        (Some(sig), Some(toff))
    } else {
        (None, None)
    };
    Ok((
        CompilationUnitHeader {
            big_endian,
            is_64_bit: is64,
            length,
            version,
            offset,
            address_size,
            root_entries: Vec::new(),
            type_signature,
            type_offset,
        },
        bytes,
    ))
}

fn parse_abbrevs(ctx: &ParseCtx, offset: u64) -> Result<IndexMap<u64, AbbrevDef>> {
    let Some(section) = ctx.abbrev else {
        return Ok(IndexMap::new());
    };
    let data = ctx.section_data(section)?;
    let start = offset as usize;
    if start >= data.len() {
        return Ok(IndexMap::new());
    }
    let mut r = Reader::new(&data[start..], ctx.big_endian);
    let mut out = IndexMap::new();
    while r.position() < data.len() - start {
        let code = r.uleb128()?;
        if code == 0 {
            break;
        }
        let tag = DwTag(r.uleb128()? as u16);
        let has_childs = r.u8()? > 0;
        let mut attrs = Vec::new();
        loop {
            let at = DwAt(r.uleb128()? as u16);
            if at == DwAt::NONE {
                break;
            }
            let form_v = r.uleb128()?;
            let form = DwForm::from_u64(form_v).ok_or_else(|| Error::Parse {
                offset: (start + r.position()) as u64,
                message: format!("unsupported DW_FORM {form_v:#x}"),
            })?;
            attrs.push((at, form));
        }
        let _ = r.u8()?;
        out.insert(
            code,
            AbbrevDef {
                tag,
                has_childs,
                attrs,
            },
        );
    }
    Ok(out)
}

/// CompilationUnitHeader, long, long)`.
fn read_attribute_values(
    r: &mut Reader,
    def: &AbbrevDef,
    cu: &CompilationUnitHeader,
    cu_start: i64,
    data_start: i64,
    ctx: &ParseCtx,
) -> Result<Vec<(DwAt, Attribute)>> {
    let mut out = Vec::with_capacity(def.attrs.len());
    for &(at, form) in &def.attrs {
        let value = match form {
            DwForm::Addr | DwForm::RefAddr | DwForm::SecOffset => {
                AttrValue::I64(r.uint_n(usize::from(cu.address_size))? as i64)
            }
            DwForm::Block => {
                let len = r.uleb128()?;
                AttrValue::Bytes(r.take(len as usize)?.to_vec())
            }
            DwForm::Block1 => {
                let len = r.u8()?;
                AttrValue::Bytes(r.take(usize::from(len))?.to_vec())
            }
            DwForm::Block2 => {
                let len = r.u16()?;
                AttrValue::Bytes(r.take(usize::from(len))?.to_vec())
            }
            DwForm::Block4 => {
                let len = r.u32()?;
                AttrValue::Bytes(r.take(len as usize)?.to_vec())
            }
            DwForm::Strng => AttrValue::Str(read_cstr(r)?),
            DwForm::Strp => {
                let offset = r.uint_n(usize::from(cu.address_size))?;
                match ctx.debug_str {
                    Some(s) => AttrValue::Str(s.read_utf8_string(ctx.data, offset)),
                    None => AttrValue::None,
                }
            }
            DwForm::FlagPresent => AttrValue::I64(1),
            DwForm::Ref1 => AttrValue::I64(cu_start + i64::from(r.u8()?) + data_start),
            DwForm::Data1 | DwForm::Flag => AttrValue::U8(r.u8()?),
            DwForm::Ref2 => AttrValue::I64(cu_start + i64::from(r.u16()?) + data_start),
            DwForm::Data2 => AttrValue::U16(r.u16()?),
            DwForm::Ref4 => AttrValue::I64(cu_start + i64::from(r.u32()?)),
            DwForm::Data4 => AttrValue::U32(r.u32()?),
            DwForm::Ref8 => AttrValue::I64(cu_start + r.u64()? as i64),
            DwForm::Data8 => AttrValue::I64(r.u64()? as i64),
            DwForm::RefSig8 => AttrValue::U64(r.u64()?),
            DwForm::Sdata => AttrValue::I64(r.sleb128()?),
            DwForm::RefUdata => AttrValue::I64(cu_start + r.uleb128()? as i64),
            DwForm::Udata => AttrValue::U64(r.uleb128()?),
            DwForm::Exprloc => {
                let len = r.uleb128()?;
                AttrValue::Bytes(r.take(len as usize)?.to_vec())
            }
            DwForm::Indirect => {
                return parse_err(r.position() as u64, "DW_FORM.indirect is not supported");
            }
        };
        out.push((at, Attribute { at, form, value }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::ElfFile;

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    fn uleb(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut b = (v & 0x7F) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                return out;
            }
        }
    }

    fn abbrev_def(code: u64, tag: u64, children: bool, attrs: &[(u64, u64)]) -> Vec<u8> {
        let mut out = uleb(code);
        out.extend(uleb(tag));
        out.push(children as u8);
        for &(at, form) in attrs {
            out.extend(uleb(at));
            out.extend(uleb(form));
        }
        out.extend([0, 0]);
        out
    }

    fn abbrev_section() -> Vec<u8> {
        let mut d = Vec::new();
        // 1: compile_unit, children: name(strp) producer(strp) language(data1)
        d.extend(abbrev_def(1, 17, true, &[(3, 14), (37, 14), (19, 11)]));
        // 2: base_type: name(strp) byte_size(data1)
        d.extend(abbrev_def(2, 36, false, &[(3, 14), (11, 11)]));
        // 3: variable: name(strp) type(ref4) location(exprloc)
        d.extend(abbrev_def(3, 52, false, &[(3, 14), (73, 19), (2, 24)]));
        d.extend(abbrev_def(4, 52, false, &[(3, 14), (73, 19)]));
        // 5: structure_type, children: name(strp) byte_size(data1)
        d.extend(abbrev_def(5, 19, true, &[(3, 14), (11, 11)]));
        // 6: member: name(strp) type(ref4) data_member_location(data1)
        d.extend(abbrev_def(6, 13, false, &[(3, 14), (73, 19), (56, 11)]));
        d.extend(abbrev_def(7, 52, false, &[(3, 14), (73, 19), (2, 24)]));
        d.push(0);
        d
    }

    fn debug_str_section() -> Vec<u8> {
        // 1:CU.c 6:GCC 10:uint32 17:gCounter 26:gGlobal 34:MyStruct 43:m1 46:m2 49:gStruct
        b"\0CU.c\0GCC\0uint32\0gCounter\0gGlobal\0MyStruct\0m1\0m2\0gStruct\0".to_vec()
    }

    fn strp(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    fn debug_info_section() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(90u32.to_le_bytes()); // unit_length = 90
        d.extend(4u16.to_le_bytes()); // version
        d.extend(0u32.to_le_bytes()); // abbrev offset
        d.push(4); // address_size
                   // @11 CU
        d.extend(uleb(1));
        d.extend(strp(1));
        d.extend(strp(6));
        d.push(12); // C99
                    // @21 base_type "uint32" size 4
        d.extend(uleb(2));
        d.extend(strp(10));
        d.push(4);
        // @27 structure_type "MyStruct" size 8
        d.extend(uleb(5));
        d.extend(strp(34));
        d.push(8);
        // @33 member m1 @0 : uint32
        d.extend(uleb(6));
        d.extend(strp(43));
        d.extend(strp(21));
        d.push(0);
        // @43 member m2 @4 : uint32
        d.extend(uleb(6));
        d.extend(strp(46));
        d.extend(strp(21));
        d.push(4);
        d.push(0);
        // @54 variable gCounter @0x9000 : uint32
        d.extend(uleb(3));
        d.extend(strp(17));
        d.extend(strp(21));
        d.push(5);
        d.extend([0x03, 0x00, 0x90, 0x00, 0x00]); // DW_OP_addr 0x9000
        d.extend(uleb(4));
        d.extend(strp(26));
        d.extend(strp(21));
        // @78 variable gStruct @0xB000 : MyStruct
        d.extend(uleb(7));
        d.extend(strp(49));
        d.extend(strp(27));
        d.push(5);
        d.extend([0x03, 0x00, 0xB0, 0x00, 0x00]); // DW_OP_addr 0xB000
        d.push(0);
        assert_eq!(d.len(), 94);
        d
    }

    fn strtab_section() -> Vec<u8> {
        b"\0gGlobal\0".to_vec()
    }

    fn symtab_section() -> Vec<u8> {
        let mut d = vec![0u8; 16];
        d.extend(1u32.to_le_bytes()); // st_name
        d.extend(0xA000u32.to_le_bytes()); // st_value
        d.extend(4u32.to_le_bytes()); // st_size
        d.push(0x11); // st_info: GLOBAL|OBJECT
        d.push(0);
        d.extend(1u16.to_le_bytes()); // st_shndx
        d
    }

    fn build_elf() -> Vec<u8> {
        let debug_info = debug_info_section();
        let abbrev = abbrev_section();
        let debug_str = debug_str_section();
        let symtab = symtab_section();
        let strtab = strtab_section();
        let shstrtab = b"\0.debug_info\0.debug_abbrev\0.debug_str\0.symtab\0.strtab\0.shstrtab\0";
        let name_of = |n: &str| {
            shstrtab
                .windows(n.len())
                .position(|w| w == n.as_bytes())
                .unwrap() as u32
        };

        let mut data = Vec::new();
        data.extend_from_slice(&[0x7F, b'E', b'L', b'F', 1, 1, 1, 0, 0]);
        data.extend_from_slice(&[0; 7]);
        data.extend(2u16.to_le_bytes()); // ET_EXEC
        data.extend(3u16.to_le_bytes()); // EM_386
        data.extend(1u32.to_le_bytes());
        data.extend(0u32.to_le_bytes()); // entry
        data.extend(0u32.to_le_bytes()); // phoff
        let shoff_pos = data.len();
        data.extend(0u32.to_le_bytes());
        data.extend(0u32.to_le_bytes()); // flags
        data.extend(52u16.to_le_bytes());
        data.extend(0u16.to_le_bytes());
        data.extend(0u16.to_le_bytes());
        data.extend(40u16.to_le_bytes());
        data.extend(7u16.to_le_bytes()); // shnum = 7
        data.extend(6u16.to_le_bytes()); // shstrndx = 6
        let mut section_headers: Vec<(u32, u32, u32, u32, u32, u32)> = vec![(0, 0, 0, 0, 0, 0)]; // NULL
        fn push_section(
            data: &mut Vec<u8>,
            headers: &mut Vec<(u32, u32, u32, u32, u32, u32)>,
            name: u32,
            typ: u32,
            body: &[u8],
        ) {
            let ofs = data.len() as u32;
            data.extend_from_slice(body);
            headers.push((name, typ, ofs, body.len() as u32, 0, 0));
        }
        push_section(
            &mut data,
            &mut section_headers,
            name_of(".debug_info"),
            1,
            &debug_info,
        );
        push_section(
            &mut data,
            &mut section_headers,
            name_of(".debug_abbrev"),
            1,
            &abbrev,
        );
        push_section(
            &mut data,
            &mut section_headers,
            name_of(".debug_str"),
            1,
            &debug_str,
        );
        let symtab_ofs = data.len() as u32;
        data.extend_from_slice(&symtab);
        section_headers.push((
            name_of(".symtab"),
            2,
            symtab_ofs,
            symtab.len() as u32,
            5,
            16,
        ));
        push_section(
            &mut data,
            &mut section_headers,
            name_of(".strtab"),
            3,
            &strtab,
        );
        push_section(
            &mut data,
            &mut section_headers,
            name_of(".shstrtab"),
            3,
            shstrtab,
        );
        let shoff = data.len() as u32;
        for &(name, typ, ofs, size, link, entsize) in &section_headers {
            data.extend(name.to_le_bytes());
            data.extend(typ.to_le_bytes());
            data.extend(0u32.to_le_bytes()); // flags
            data.extend(0u32.to_le_bytes()); // addr
            data.extend(ofs.to_le_bytes());
            data.extend(size.to_le_bytes());
            data.extend(link.to_le_bytes());
            data.extend(0u32.to_le_bytes()); // info
            data.extend(1u32.to_le_bytes()); // addralign
            data.extend(entsize.to_le_bytes());
        }
        data[shoff_pos..shoff_pos + 4].copy_from_slice(&shoff.to_le_bytes());
        data
    }

    fn elf_with_dwarf() -> ElfFile {
        let file = ElfFile::from_bytes(build_elf(), None, true).unwrap();
        assert!(file.dwarf.is_some(), "DWARF parsing failed");
        file
    }

    #[test]
    fn parses_compilation_units() {
        let file = elf_with_dwarf();
        let dwarf = file.dwarf.as_ref().unwrap();
        assert_eq!(dwarf.compilation_units.len(), 1);
        let cu = &dwarf.compilation_units[0];
        assert_eq!(cu.version, 4);
        assert_eq!(cu.address_size, 4);
        assert!(!cu.is_64_bit);
        assert_eq!(cu.language(dwarf), DwLang::C99);
        assert_eq!(cu.producer(dwarf), "GCC");
        assert_eq!(cu.root_entries.len(), 1);
    }

    #[test]
    fn builds_root_symbols() {
        let file = elf_with_dwarf();
        let dwarf = file.dwarf.as_ref().unwrap();
        assert_eq!(dwarf.symbols.roots.len(), 3);
        // gCounter:DWARF location
        let id = dwarf.symbols.roots["gCounter"];
        let sv = &dwarf.sym_values[id];
        assert_eq!(sv.address, 0x9000);
        assert_eq!(sv.size, 32);
        assert_eq!(sv.entry_size, 0);
        let id = dwarf.symbols.roots["gGlobal"];
        assert_eq!(dwarf.sym_values[id].address, 0xA000);
        let id = dwarf.symbols.roots["gStruct"];
        let sv = &dwarf.sym_values[id];
        assert_eq!(sv.address, 0xB000);
        assert_eq!(sv.size, 64);
    }

    #[test]
    fn expands_struct_members_and_looks_up_path() {
        let mut file = elf_with_dwarf();
        {
            let dwarf = file.dwarf.as_mut().unwrap();
            let id = dwarf.symbols.roots["gStruct"];
            dwarf.expand_symbol(id);
            let sv = &dwarf.sym_values[id];
            let childs = sv.childs.as_ref().unwrap();
            assert_eq!(childs.len(), 2);
            let m1 = &dwarf.sym_values[childs[0]];
            let m2 = &dwarf.sym_values[childs[1]];
            assert_eq!(m1.address, 0xB000);
            assert_eq!(m1.size, 32);
            assert_eq!(m2.address, 0xB004);
            let path = autors_symbols::update::parse_symbol_path("gStruct.m2").unwrap();
            assert_eq!(path.len(), 2);
            let up = dwarf.lookup_path(&path).unwrap();
            assert_eq!(up.address, 0xB004);
            assert_eq!(up.size, 32);
            let path = autors_symbols::update::parse_symbol_path("gStruct.nope").unwrap();
            assert!(dwarf.lookup_path(&path).is_none());
        }
        let dwarf = file.dwarf.as_mut().unwrap();
        let all = dwarf.enum_symbols();
        let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"gCounter"));
        assert!(names.contains(&"gStruct"));
        assert!(names.contains(&"gStruct.m1"));
        assert!(names.contains(&"gStruct.m2"));
        let (_, m2) = all.iter().find(|(n, _)| n == "gStruct.m2").unwrap();
        assert_eq!(m2.address, 0xB004);
    }

    #[test]
    fn type_str_of_base_and_struct() {
        let file = elf_with_dwarf();
        let dwarf = file.dwarf.as_ref().unwrap();
        let id = dwarf.symbols.roots["gCounter"];
        let var = dwarf.sym_values[id].var_base;
        assert_eq!(dwarf.get_type_str(var), "uint32 ");
        let id = dwarf.symbols.roots["gStruct"];
        let var = dwarf.sym_values[id].var_base;
        assert_eq!(dwarf.get_type_str(var), "MyStruct ");
    }

    #[test]
    fn elf_get_values_to_synchronize_uses_dwarf() {
        let mut file = elf_with_dwarf();
        let a2l = r#"/begin PROJECT P "d"
/begin MODULE M "m"
  /begin MEASUREMENT Meas1 "d" ULONG Conv 1 0 0 100 ECU_ADDRESS 0x1000
    SYMBOL_LINK "gStruct.m2" 0
  /end MEASUREMENT
  /begin MEASUREMENT Meas2 "d" ULONG Conv 1 0 0 100 ECU_ADDRESS 0x9000
    SYMBOL_LINK "gCounter" 0
  /end MEASUREMENT
  /begin MEASUREMENT Meas3 "d" ULONG Conv 1 0 0 100 ECU_ADDRESS 0x1000
    SYMBOL_LINK "gUnknown" 0
  /end MEASUREMENT
/end MODULE
/end PROJECT
"#;
        let project = autors_a2l::Project::parse_str(a2l).unwrap();
        let (records, _ds_start, _ds_len) = file.get_values_to_synchronize(&project, 1).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records[0].typ,
            autors_symbols::update::UpdateType::AdjustAddress
        );
        assert_eq!(records[0].address, 0xB004);
        assert_eq!(records[1].typ, autors_symbols::update::UpdateType::Matched);
        assert_eq!(
            records[2].typ,
            autors_symbols::update::UpdateType::NotMatched
        );
    }
}
