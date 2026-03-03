use crate::block::Block;
use crate::error::{Error, Result};
use crate::node::Node;
use crate::params::ParamCursor;
use crate::writer::Writer;

use super::enums::{A2lKeyword, AddrType, DataSize, DataType, IndexMode, IndexOrder};

const AXIS_LETTERS: [char; 5] = ['X', 'Y', 'Z', '4', '5'];

/// FLOAT64_IEEE=4, INT64=8, FLOAT16_IEEE=2).
const DEFAULT_ALIGNMENTS: [i32; 7] = [1, 2, 4, 4, 4, 8, 2];

const ALIGNMENT_TAGS: [&str; 7] = [
    "ALIGNMENT_BYTE",
    "ALIGNMENT_WORD",
    "ALIGNMENT_LONG",
    "ALIGNMENT_FLOAT32_IEEE",
    "ALIGNMENT_FLOAT64_IEEE",
    "ALIGNMENT_INT64",
    "ALIGNMENT_FLOAT16_IEEE",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisPtsLayoutDesc {
    pub name: String,
    pub position: i32,
    pub data_type: DataType,
    pub axis_idx: i32,
    pub index_order: IndexOrder,
    pub address_type: AddrType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisRescaleLayoutDesc {
    pub name: String,
    pub position: i32,
    pub data_type: DataType,
    pub axis_idx: i32,
    pub index_order: IndexOrder,
    pub address_type: AddrType,
    pub max_no_rescale_pairs: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FncValuesLayoutDesc {
    pub name: String,
    pub position: i32,
    pub data_type: DataType,
    pub index_mode: IndexMode,
    pub address_type: AddrType,
}

/// `RIP_ADDR_W_X`, `SHIFT_OP_X`, `NO_RESCALE_X`, `RIP_ADDR_W`, `IDENTIFICATION`,
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoAxisPtsLayoutDesc {
    pub name: String,
    pub position: i32,
    pub data_type: DataType,
    pub axis_idx: i32,
}

impl NoAxisPtsLayoutDesc {
    pub fn data_size(&self) -> Option<DataSize> {
        match self.data_type {
            DataType::UByte | DataType::SByte => Some(DataSize::BYTE),
            DataType::UWord | DataType::SWord => Some(DataSize::WORD),
            DataType::ULong | DataType::SLong | DataType::Float32Ieee => Some(DataSize::LONG),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordLayout {
    pub name: String,
    pub alignments: Option<[i32; 7]>,
    pub fnc_values: Option<FncValuesLayoutDesc>,
    pub axis_pts: [Option<AxisPtsLayoutDesc>; 5],
    /// `NO_AXIS_PTS_X..5`.
    pub no_axis_pts: [Option<NoAxisPtsLayoutDesc>; 5],
    pub fix_no_axis_pts: [i32; 5],
    /// `SRC_ADDR_X..5`.
    pub src_address: [Option<NoAxisPtsLayoutDesc>; 5],
    pub axis_rescale_x: Option<AxisRescaleLayoutDesc>,
    pub no_rescale_x: Option<NoAxisPtsLayoutDesc>,
    /// `OFFSET_X..5`.
    pub offset: [Option<NoAxisPtsLayoutDesc>; 5],
    /// `DIST_OP_X..5`.
    pub dist_op: [Option<NoAxisPtsLayoutDesc>; 5],
    /// `RIP_ADDR_W_X..5`.
    pub rip_addr: [Option<NoAxisPtsLayoutDesc>; 5],
    /// `SHIFT_OP_X..5`.
    pub shift_op: [Option<NoAxisPtsLayoutDesc>; 5],
    pub reserved: Vec<NoAxisPtsLayoutDesc>,
    pub identification: Option<NoAxisPtsLayoutDesc>,
    pub rip_addr_w: Option<NoAxisPtsLayoutDesc>,
    pub static_record_layout: bool,
    pub static_address_offsets: bool,
}

impl Default for RecordLayout {
    fn default() -> Self {
        RecordLayout {
            name: String::new(),
            alignments: None,
            fnc_values: None,
            axis_pts: Default::default(),
            no_axis_pts: Default::default(),
            fix_no_axis_pts: [-1; 5],
            src_address: Default::default(),
            axis_rescale_x: None,
            no_rescale_x: None,
            offset: Default::default(),
            dist_op: Default::default(),
            rip_addr: Default::default(),
            shift_op: Default::default(),
            reserved: Vec::new(),
            identification: None,
            rip_addr_w: None,
            static_record_layout: false,
            static_address_offsets: false,
        }
    }
}

enum LayoutEntry<'a> {
    NoAxisPts(&'a NoAxisPtsLayoutDesc),
    AxisPts(&'a AxisPtsLayoutDesc),
    AxisRescale(&'a AxisRescaleLayoutDesc),
    FncValues(&'a FncValuesLayoutDesc),
}

impl LayoutEntry<'_> {
    fn position(&self) -> i32 {
        match self {
            LayoutEntry::NoAxisPts(e) => e.position,
            LayoutEntry::AxisPts(e) => e.position,
            LayoutEntry::AxisRescale(e) => e.position,
            LayoutEntry::FncValues(e) => e.position,
        }
    }

    fn to_line(&self) -> Result<String> {
        match self {
            LayoutEntry::NoAxisPts(e) => {
                if e.name == "RESERVED" {
                    let ds = e.data_size().ok_or_else(|| {
                        Error::write(
                            "RECORD_LAYOUT",
                            format!("cannot derive DataSize from {:?}", e.data_type),
                        )
                    })?;
                    Ok(format!("{} {} {}", e.name, e.position, enum_kw(ds)))
                } else {
                    Ok(format!(
                        "{} {} {}",
                        e.name,
                        e.position,
                        enum_kw(e.data_type)
                    ))
                }
            }
            LayoutEntry::AxisPts(e) => Ok(format!(
                "{} {} {} {} {}",
                e.name,
                e.position,
                enum_kw(e.data_type),
                enum_kw(e.index_order),
                enum_kw(e.address_type)
            )),
            LayoutEntry::AxisRescale(e) => Ok(format!(
                "{} {} {} {} {} {}",
                e.name,
                e.position,
                enum_kw(e.data_type),
                e.max_no_rescale_pairs,
                enum_kw(e.index_order),
                enum_kw(e.address_type)
            )),
            LayoutEntry::FncValues(e) => Ok(format!(
                "{} {} {} {} {}",
                e.name,
                e.position,
                enum_kw(e.data_type),
                enum_kw(e.index_mode),
                enum_kw(e.address_type)
            )),
        }
    }
}

fn enum_kw<T: A2lKeyword + std::fmt::Debug + Copy>(v: T) -> String {
    match v.as_keyword() {
        Some(kw) => kw.to_string(),
        None => format!("{v:?}"),
    }
}

fn axis_index(text: &str, line: u32) -> Result<usize> {
    match text.chars().last() {
        Some('X') => Ok(0),
        Some('Y') => Ok(1),
        Some('Z') => Ok(2),
        Some('4') => Ok(3),
        Some('5') => Ok(4),
        _ => Err(Error::parse(
            line,
            format!("RECORD_LAYOUT: invalid axis suffix in {text:?}"),
        )),
    }
}

fn take_enum<T: A2lKeyword>(cur: &mut ParamCursor, default: T) -> Result<T> {
    let t = cur.next_token()?;
    Ok(T::from_keyword(&t.text).unwrap_or(default))
}

fn take_data_type(cur: &mut ParamCursor) -> Result<DataType> {
    let t = cur.next_token()?;
    if let Some(d) = DataType::from_keyword(&t.text) {
        return Ok(d);
    }
    if let Some(ds) = DataSize::from_keyword(&t.text) {
        return Ok(data_type_for_size(ds));
    }
    Ok(DataType::default())
}

/// LONG→ULONG.
fn data_type_for_size(size: DataSize) -> DataType {
    match size {
        DataSize::BYTE => DataType::UByte,
        DataSize::WORD => DataType::UWord,
        DataSize::LONG => DataType::ULong,
    }
}

impl Node for RecordLayout {
    const KEYWORD: &'static str = "RECORD_LAYOUT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut rl = RecordLayout {
            name: cur.ident()?,
            ..Default::default()
        };
        while !cur.is_empty() {
            let t = cur.next_token()?;
            let text = t.text.as_str();
            let prefix = match (t.quoted, text.get(..6)) {
                (false, Some(p)) => p,
                _ => continue,
            };
            match prefix {
                "ALIGNM" => {
                    if let Some(idx) = ALIGNMENT_TAGS.iter().position(|&tag| tag == text) {
                        let aligns = rl.alignments.get_or_insert(DEFAULT_ALIGNMENTS);
                        aligns[idx] = cur.int::<i32>()?;
                    }
                }
                "FNC_VA" => {
                    let position = cur.int::<i32>()?;
                    let data_type = take_data_type(&mut cur)?;
                    let index_mode = take_enum(&mut cur, IndexMode::NotSet)?;
                    let address_type = take_enum(&mut cur, AddrType::DIRECT)?;
                    rl.fnc_values = Some(FncValuesLayoutDesc {
                        name: "FNC_VALUES".to_string(),
                        position,
                        data_type,
                        index_mode,
                        address_type,
                    });
                }
                "FIX_NO" => {
                    let idx = axis_index(text, t.line)?;
                    rl.fix_no_axis_pts[idx] = cur.int::<i32>()?;
                }
                "DIST_O" => {
                    let idx = axis_index(text, t.line)?;
                    rl.dist_op[idx] = Some(take_no_axis_pts(text, idx as i32, &mut cur)?);
                }
                "SRC_AD" => {
                    let idx = axis_index(text, t.line)?;
                    rl.src_address[idx] = Some(take_no_axis_pts(text, idx as i32, &mut cur)?);
                }
                "RIP_AD" => {
                    if text.ends_with('W') {
                        rl.rip_addr_w = Some(take_no_axis_pts(text, -1, &mut cur)?);
                    } else {
                        let idx = axis_index(text, t.line)?;
                        rl.rip_addr[idx] = Some(take_no_axis_pts(text, idx as i32, &mut cur)?);
                    }
                }
                "AXIS_P" => {
                    let idx = axis_index(text, t.line)?;
                    let position = cur.int::<i32>()?;
                    let data_type = take_data_type(&mut cur)?;
                    let index_order = take_enum(&mut cur, IndexOrder::INDEX_INCR)?;
                    let address_type = take_enum(&mut cur, AddrType::DIRECT)?;
                    rl.axis_pts[idx] = Some(AxisPtsLayoutDesc {
                        name: text.to_string(),
                        position,
                        data_type,
                        axis_idx: idx as i32,
                        index_order,
                        address_type,
                    });
                }
                "NO_AXI" => {
                    let idx = axis_index(text, t.line)?;
                    rl.no_axis_pts[idx] = Some(take_no_axis_pts(text, idx as i32, &mut cur)?);
                }
                "IDENTI" => {
                    rl.identification = Some(take_no_axis_pts(text, -1, &mut cur)?);
                }
                "AXIS_R" => {
                    let idx = axis_index(text, t.line)?;
                    let position = cur.int::<i32>()?;
                    let data_type = take_data_type(&mut cur)?;
                    let max_no_rescale_pairs = cur.int::<i32>()?;
                    let index_order = take_enum(&mut cur, IndexOrder::INDEX_INCR)?;
                    let address_type = take_enum(&mut cur, AddrType::DIRECT)?;
                    rl.axis_rescale_x = Some(AxisRescaleLayoutDesc {
                        name: "AXIS_RESCALE_X".to_string(),
                        position,
                        data_type,
                        axis_idx: idx as i32,
                        index_order,
                        address_type,
                        max_no_rescale_pairs,
                    });
                }
                "NO_RES" => {
                    let idx = axis_index(text, t.line)?;
                    rl.no_rescale_x = Some(take_no_axis_pts(text, idx as i32, &mut cur)?);
                }
                "OFFSET" => {
                    let idx = axis_index(text, t.line)?;
                    rl.offset[idx] = Some(take_no_axis_pts(text, idx as i32, &mut cur)?);
                }
                "SHIFT_" => {
                    let idx = axis_index(text, t.line)?;
                    rl.shift_op[idx] = Some(take_no_axis_pts(text, idx as i32, &mut cur)?);
                }
                "RESERV" => {
                    let position = cur.int::<i32>()?;
                    let t2 = cur.next_token()?;
                    let ds = DataSize::from_keyword(&t2.text).unwrap_or(DataSize::BYTE);
                    rl.reserved.push(NoAxisPtsLayoutDesc {
                        name: "RESERVED".to_string(),
                        position,
                        data_type: data_type_for_size(ds),
                        axis_idx: -1,
                    });
                }
                "STATIC" => match text {
                    "STATIC_RECORD_LAYOUT" => rl.static_record_layout = true,
                    "STATIC_ADDRESS_OFFSETS" => rl.static_address_offsets = true,
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(rl)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.value_line(None, &self.name);
        if let Some(aligns) = &self.alignments {
            for (i, v) in aligns.iter().enumerate() {
                w.value_line(Some(ALIGNMENT_TAGS[i]), &v.to_string());
            }
        }
        for (i, &v) in self.fix_no_axis_pts.iter().enumerate() {
            if v != -1 {
                let tag = format!("FIX_NO_AXIS_PTS_{}", AXIS_LETTERS[i]);
                w.value_line(Some(&tag), &v.to_string());
            }
        }
        for entry in self.layout_entries() {
            w.value_line(None, &entry.to_line()?);
        }
        if let Some(id) = &self.identification {
            w.value_line(
                Some("IDENTIFICATION"),
                &format!("{} {}", id.position, enum_kw(id.data_type)),
            );
        }
        if self.static_record_layout {
            w.value_line(None, "STATIC_RECORD_LAYOUT");
        }
        if self.static_address_offsets {
            w.value_line(None, "STATIC_ADDRESS_OFFSETS");
        }
        Ok(())
    }
}

impl RecordLayout {
    fn layout_entries(&self) -> Vec<LayoutEntry<'_>> {
        let mut entries = Vec::new();
        for e in self.no_axis_pts.iter().flatten() {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        for e in self.axis_pts.iter().flatten() {
            entries.push(LayoutEntry::AxisPts(e));
        }
        for e in self.src_address.iter().flatten() {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        for e in self.offset.iter().flatten() {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        for e in self.dist_op.iter().flatten() {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        for e in self.rip_addr.iter().flatten() {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        for e in self.shift_op.iter().flatten() {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        for e in &self.reserved {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        if let Some(e) = &self.axis_rescale_x {
            entries.push(LayoutEntry::AxisRescale(e));
        }
        if let Some(e) = &self.no_rescale_x {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        if let Some(e) = &self.identification {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        if let Some(e) = &self.rip_addr_w {
            entries.push(LayoutEntry::NoAxisPts(e));
        }
        if let Some(e) = &self.fnc_values {
            entries.push(LayoutEntry::FncValues(e));
        }
        entries.sort_by_key(|e| e.position());
        entries
    }
}

fn take_no_axis_pts(
    text: &str,
    axis_idx: i32,
    cur: &mut ParamCursor,
) -> Result<NoAxisPtsLayoutDesc> {
    let position = cur.int::<i32>()?;
    let data_type = take_data_type(cur)?;
    Ok(NoAxisPtsLayoutDesc {
        name: text.to_string(),
        position,
        data_type,
        axis_idx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::build_block_tree;
    use crate::token::tokenize;
    use crate::writer::WriterOptions;

    fn roundtrip(src: &str) -> String {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let block = root.child("RECORD_LAYOUT").unwrap();
        let rl = RecordLayout::parse(block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        rl.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn fnc_values_only() {
        let out = roundtrip(
            "/begin RECORD_LAYOUT Scalar FNC_VALUES 1 UBYTE COLUMN_DIR DIRECT /end RECORD_LAYOUT",
        );
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  Scalar\n  FNC_VALUES 1 UBYTE COLUMN_DIR DIRECT\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn full_layout() {
        let out = roundtrip(
            "/begin RECORD_LAYOUT Map2D \
             ALIGNMENT_BYTE 2 FIX_NO_AXIS_PTS_X 4 \
             AXIS_PTS_X 1 UBYTE INDEX_INCR DIRECT NO_AXIS_PTS_X 2 BYTE \
             FNC_VALUES 3 UWORD ROW_DIR DIRECT DIST_OP_X 4 SBYTE OFFSET_X 5 SWORD \
             SRC_ADDR_X 6 ULONG RIP_ADDR_W_X 7 SLONG SHIFT_OP_X 8 A_UINT64 \
             AXIS_RESCALE_X 9 UWORD 6 INDEX_DECR PBYTE NO_RESCALE_X 10 WORD \
             RESERVED 11 LONG RIP_ADDR_W 12 ULONG IDENTIFICATION 13 UBYTE \
             STATIC_RECORD_LAYOUT STATIC_ADDRESS_OFFSETS /end RECORD_LAYOUT",
        );
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  Map2D\n\
             \x20 ALIGNMENT_BYTE 2\n  ALIGNMENT_WORD 2\n  ALIGNMENT_LONG 4\n\
             \x20 ALIGNMENT_FLOAT32_IEEE 4\n  ALIGNMENT_FLOAT64_IEEE 4\n  ALIGNMENT_INT64 8\n\
             \x20 ALIGNMENT_FLOAT16_IEEE 2\n  FIX_NO_AXIS_PTS_X 4\n\
             \x20 AXIS_PTS_X 1 UBYTE INDEX_INCR DIRECT\n  NO_AXIS_PTS_X 2 UBYTE\n\
             \x20 FNC_VALUES 3 UWORD ROW_DIR DIRECT\n  DIST_OP_X 4 SBYTE\n  OFFSET_X 5 SWORD\n\
             \x20 SRC_ADDR_X 6 ULONG\n  RIP_ADDR_W_X 7 SLONG\n  SHIFT_OP_X 8 A_UINT64\n\
             \x20 AXIS_RESCALE_X 9 UWORD 6 INDEX_DECR PBYTE\n  NO_RESCALE_X 10 UWORD\n\
             \x20 RESERVED 11 LONG\n  RIP_ADDR_W 12 ULONG\n\
             \x20 IDENTIFICATION 13 UBYTE\n  IDENTIFICATION 13 UBYTE\n\
             \x20 STATIC_RECORD_LAYOUT\n  STATIC_ADDRESS_OFFSETS\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn entries_sorted_by_position() {
        let out = roundtrip(
            "/begin RECORD_LAYOUT RL FNC_VALUES 10 UBYTE ROW_DIR DIRECT \
             AXIS_PTS_X 2 UBYTE INDEX_INCR DIRECT /end RECORD_LAYOUT",
        );
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  RL\n  AXIS_PTS_X 2 UBYTE INDEX_INCR DIRECT\n\
             \x20 FNC_VALUES 10 UBYTE ROW_DIR DIRECT\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn datasize_fallback_for_no_axis_pts() {
        let out = roundtrip("/begin RECORD_LAYOUT RL NO_AXIS_PTS_X 3 BYTE /end RECORD_LAYOUT");
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  RL\n  NO_AXIS_PTS_X 3 UBYTE\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn reserved_writes_datasize() {
        let out = roundtrip("/begin RECORD_LAYOUT RL RESERVED 7 LONG /end RECORD_LAYOUT");
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  RL\n  RESERVED 7 LONG\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn alignments_start_from_defaults() {
        let out = roundtrip(
            "/begin RECORD_LAYOUT RL ALIGNMENT_BYTE 2 ALIGNMENT_LONG 0x8 /end RECORD_LAYOUT",
        );
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  RL\n  ALIGNMENT_BYTE 2\n  ALIGNMENT_WORD 2\n\
             \x20 ALIGNMENT_LONG 8\n  ALIGNMENT_FLOAT32_IEEE 4\n  ALIGNMENT_FLOAT64_IEEE 4\n\
             \x20 ALIGNMENT_INT64 8\n  ALIGNMENT_FLOAT16_IEEE 2\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn fix_no_axis_pts_written_only_when_set() {
        let out = roundtrip("/begin RECORD_LAYOUT RL FIX_NO_AXIS_PTS_4 16 /end RECORD_LAYOUT");
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  RL\n  FIX_NO_AXIS_PTS_4 16\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn identification_written_twice_under_compatibility_rules() {
        let out = roundtrip("/begin RECORD_LAYOUT RL IDENTIFICATION 1 UWORD /end RECORD_LAYOUT");
        assert_eq!(out.matches("IDENTIFICATION 1 UWORD").count(), 2);
    }

    #[test]
    fn rip_addr_w_and_axis_variants() {
        let out = roundtrip(
            "/begin RECORD_LAYOUT RL RIP_ADDR_W 4 ULONG RIP_ADDR_W_Y 2 UWORD /end RECORD_LAYOUT",
        );
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  RL\n  RIP_ADDR_W_Y 2 UWORD\n  RIP_ADDR_W 4 ULONG\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn static_flags() {
        let out = roundtrip(
            "/begin RECORD_LAYOUT RL STATIC_RECORD_LAYOUT STATIC_ADDRESS_OFFSETS /end RECORD_LAYOUT",
        );
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  RL\n  STATIC_RECORD_LAYOUT\n  STATIC_ADDRESS_OFFSETS\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn unknown_and_lowercase_keywords_skipped() {
        let out = roundtrip(
            "/begin RECORD_LAYOUT RL FOO_BAR_X 1 UBYTE fnc_values 2 UBYTE ROW_DIR DIRECT /end RECORD_LAYOUT",
        );
        assert_eq!(out, "/begin RECORD_LAYOUT\n  RL\n/end RECORD_LAYOUT\n");
    }

    #[test]
    fn unknown_enum_values_fall_back_to_default() {
        let out = roundtrip(
            "/begin RECORD_LAYOUT RL FNC_VALUES 1 UBYTE GARBAGE DIRECT /end RECORD_LAYOUT",
        );
        assert_eq!(
            out,
            "/begin RECORD_LAYOUT\n  RL\n  FNC_VALUES 1 UBYTE NotSet DIRECT\n/end RECORD_LAYOUT\n"
        );
    }

    #[test]
    fn invalid_axis_suffix_is_error() {
        let toks = tokenize(
            "/begin RECORD_LAYOUT RL AXIS_PTS_Q 1 UBYTE INDEX_INCR DIRECT /end RECORD_LAYOUT",
        )
        .unwrap();
        let root = build_block_tree(&toks).unwrap();
        assert!(RecordLayout::parse(root.child("RECORD_LAYOUT").unwrap()).is_err());
    }
}
