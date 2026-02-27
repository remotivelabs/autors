//! `FlashSectionType` / `AdrMapType` / `FirstLastNode` / `MAP_SYMBOL` / `MEMORY_SEGMENT` /
//! `CALIBRATION_METHOD` / `LinkMapType` / `DisplayType` / `VirtualConvType`).

use crate::block::Block;
use crate::error::{Error, Result};
use crate::model::unsupported::UnsupportedNode;
use crate::node::Node;
use crate::params::ParamCursor;
use crate::writer::{escape_str, Writer};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub offset: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlashSection {
    pub address: u32,
    pub length: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExSymbol {
    pub name: String,
    pub offset: u32,
    pub original_ptr_table: Option<Symbol>,
    pub flash_section: Option<FlashSection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdrMap {
    pub src: String,
    pub dst: String,
}

fn parse_symbol(cur: &mut ParamCursor) -> Result<Symbol> {
    Ok(Symbol {
        name: cur.string()?,
        offset: cur.uint::<u32>()?,
    })
}

fn push_symbol_hex(out: &mut String, name: &str, offset: u32) {
    out.push_str(&format!(" \"{}\" 0x{:X}", escape_str(name), offset));
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FirstLast {
    first: Option<Symbol>,
    last: Option<Symbol>,
    address_mapping_xcp: Vec<AdrMap>,
}

impl FirstLast {
    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut fl = FirstLast::default();
        while !cur.is_empty() {
            if cur.take_if("FIRST") {
                fl.first = Some(parse_symbol(&mut cur)?);
            } else if cur.take_if("LAST") {
                fl.last = Some(parse_symbol(&mut cur)?);
            } else if cur.take_if("ADDRESS_MAPPING_XCP") {
                fl.address_mapping_xcp.push(AdrMap {
                    src: cur.string()?,
                    dst: cur.string()?,
                });
            } else {
                cur.next_token()?;
            }
        }
        Ok(fl)
    }
}

fn write_first_last_block(
    first: &Option<Symbol>,
    last: &Option<Symbol>,
    address_mapping_xcp: &[AdrMap],
    w: &mut Writer,
    keyword: &str,
) {
    let mut line = format!("/begin {}", keyword);
    if let Some(first) = first {
        line.push_str(" FIRST");
        push_symbol_hex(&mut line, &first.name, first.offset);
    }
    if let Some(last) = last {
        line.push_str(" LAST");
        push_symbol_hex(&mut line, &last.name, last.offset);
    }
    if !address_mapping_xcp.is_empty() {
        if first.is_some() || last.is_some() {
            line.push('\n');
        }
        for m in address_mapping_xcp {
            line.push_str(&format!(
                "ADDRESS_MAPPING_XCP \"{}\" \"{}\"\n",
                escape_str(&m.src),
                escape_str(&m.dst)
            ));
        }
    }
    w.value_line(None, &line);
    w.value_line(None, &format!("/end {}", keyword));
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapSymbol {
    pub first: Option<Symbol>,
    pub last: Option<Symbol>,
    pub address_mapping_xcp: Vec<AdrMap>,
}

impl Node for MapSymbol {
    const KEYWORD: &'static str = "MAP_SYMBOL";

    fn parse(block: &Block) -> Result<Self> {
        let fl = FirstLast::parse(block)?;
        Ok(MapSymbol {
            first: fl.first,
            last: fl.last,
            address_mapping_xcp: fl.address_mapping_xcp,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        let _ = w;
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_first_last_block(
            &self.first,
            &self.last,
            &self.address_mapping_xcp,
            w,
            Self::KEYWORD,
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemorySegment {
    pub first: Option<Symbol>,
    pub last: Option<Symbol>,
    pub address_mapping_xcp: Vec<AdrMap>,
}

impl Node for MemorySegment {
    const KEYWORD: &'static str = "MEMORY_SEGMENT";

    fn parse(block: &Block) -> Result<Self> {
        let fl = FirstLast::parse(block)?;
        Ok(MemorySegment {
            first: fl.first,
            last: fl.last,
            address_mapping_xcp: fl.address_mapping_xcp,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        let _ = w;
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_first_last_block(
            &self.first,
            &self.last,
            &self.address_mapping_xcp,
            w,
            Self::KEYWORD,
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalibrationMethod {
    pub auto_sar_single_pointered: Option<ExSymbol>,
    pub in_circuit2: Option<ExSymbol>,
}

impl Node for CalibrationMethod {
    const KEYWORD: &'static str = "CALIBRATION_METHOD";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut cm = CalibrationMethod::default();
        while !cur.is_empty() {
            if cur.take_if("AUTOSAR_SINGLE_POINTERED") {
                let mut sym = ExSymbol {
                    name: cur.string()?,
                    offset: cur.uint::<u32>()?,
                    ..ExSymbol::default()
                };
                if cur.remaining() > 2 && cur.take_if("ORIGINAL_POINTER_TABLE") {
                    sym.original_ptr_table = Some(parse_symbol(&mut cur)?);
                }
                cm.auto_sar_single_pointered = Some(sym);
            } else if cur.take_if("InCircuit2") {
                let mut sym = ExSymbol {
                    name: cur.string()?,
                    offset: cur.uint::<u32>()?,
                    ..ExSymbol::default()
                };
                while cur.remaining() > 2 {
                    if cur.take_if("ORIGINAL_POINTER_TABLE") {
                        sym.original_ptr_table = Some(parse_symbol(&mut cur)?);
                    } else if cur.take_if("FLASH_SECTION") {
                        sym.flash_section = Some(FlashSection {
                            address: cur.uint::<u32>()?,
                            length: cur.uint::<u32>()?,
                        });
                    } else {
                        cur.next_token()?;
                    }
                }
                cm.in_circuit2 = Some(sym);
            } else {
                cur.next_token()?;
            }
        }
        Ok(cm)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        let _ = w;
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        let mut line = format!("/begin {}", Self::KEYWORD);
        if let Some(asp) = &self.auto_sar_single_pointered {
            line.push_str(" AUTOSAR_SINGLE_POINTERED");
            push_symbol_hex(&mut line, &asp.name, asp.offset);
            if let Some(ptr) = &asp.original_ptr_table {
                line.push_str(" ORIGINAL_POINTER_TABLE");
                push_symbol_hex(&mut line, &ptr.name, ptr.offset);
            }
        }
        if let Some(ic) = &self.in_circuit2 {
            if self.auto_sar_single_pointered.is_some() {
                line.push('\n');
            }
            line.push_str(" InCircuit2");
            push_symbol_hex(&mut line, &ic.name, ic.offset);
            if let Some(ptr) = &ic.original_ptr_table {
                line.push_str(" AUTOSAR_SINGLE_POINTERED");
                push_symbol_hex(&mut line, &ptr.name, ptr.offset);
            }
            if let Some(fs) = &ic.flash_section {
                line.push_str(&format!(
                    " FLASH_SECTION 0x{:X} 0x{:X}",
                    fs.address, fs.length
                ));
            }
        }
        w.value_line(None, &line);
        w.value_line(None, &format!("/end {}", Self::KEYWORD));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanapeAddressUpdateChild {
    /// `/begin MAP_SYMBOL ...`.
    MapSymbol(MapSymbol),
    /// `/begin MEMORY_SEGMENT ...`.
    MemorySegment(MemorySegment),
    /// `/begin CALIBRATION_METHOD ...`.
    CalibrationMethod(CalibrationMethod),
    Unsupported(UnsupportedNode),
}

/// `/begin IF_DATA CANAPE_ADDRESS_UPDATE ... /end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanapeAddressUpdate {
    pub name: String,
    pub epk_address: Vec<Symbol>,
    pub ecu_calibration_offset: Option<Symbol>,
    pub children: Vec<CanapeAddressUpdateChild>,
}

impl Default for CanapeAddressUpdate {
    fn default() -> Self {
        CanapeAddressUpdate {
            name: "CANAPE_ADDRESS_UPDATE".to_string(),
            epk_address: Vec::new(),
            ecu_calibration_offset: None,
            children: Vec::new(),
        }
    }
}

impl Node for CanapeAddressUpdate {
    const KEYWORD: &'static str = "IF_DATA";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let mut node = CanapeAddressUpdate::default();
        if !cur.is_empty() {
            node.name = cur.ident()?;
        }
        while !cur.is_empty() {
            if cur.take_if("EPK_ADDRESS") {
                node.epk_address.push(parse_symbol(&mut cur)?);
            } else if cur.take_if("ECU_CALIBRATION_OFFSET") {
                node.ecu_calibration_offset = Some(parse_symbol(&mut cur)?);
            } else {
                cur.next_token()?;
            }
        }
        for child in block.children() {
            if child.keyword.eq_ignore_ascii_case("MAP_SYMBOL") {
                node.children
                    .push(CanapeAddressUpdateChild::MapSymbol(MapSymbol::parse(
                        child,
                    )?));
            } else if child.keyword.eq_ignore_ascii_case("MEMORY_SEGMENT") {
                node.children.push(CanapeAddressUpdateChild::MemorySegment(
                    MemorySegment::parse(child)?,
                ));
            } else if child.keyword.eq_ignore_ascii_case("CALIBRATION_METHOD") {
                node.children
                    .push(CanapeAddressUpdateChild::CalibrationMethod(
                        CalibrationMethod::parse(child)?,
                    ));
            } else {
                node.children.push(CanapeAddressUpdateChild::Unsupported(
                    UnsupportedNode::from_block(child),
                ));
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        let _ = w;
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        let mut first = format!("IF_DATA {}", self.name);
        for epk in &self.epk_address {
            first.push_str(" EPK_ADDRESS");
            push_symbol_hex(&mut first, &epk.name, epk.offset);
        }
        if let Some(eco) = &self.ecu_calibration_offset {
            first.push_str(&format!(
                " ECU_CALIBRATION_OFFSET \"{}\" {}",
                escape_str(&eco.name),
                eco.offset
            ));
        }
        w.begin_block(&first);
        for child in &self.children {
            match child {
                CanapeAddressUpdateChild::MapSymbol(m) => m.write_block(w)?,
                CanapeAddressUpdateChild::MemorySegment(m) => m.write_block(w)?,
                CanapeAddressUpdateChild::CalibrationMethod(m) => m.write_block(w)?,
                CanapeAddressUpdateChild::Unsupported(u) => u.write_block(w)?,
            }
        }
        w.end_block("IF_DATA");
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkMap {
    pub name: String,
    pub base_address: u32,
    pub base_address_ext: u16,
    pub is_address_relative_to_ds: u16,
    pub offset_segment: i32,
    pub is_data_type_valid: u16,
    pub data_typ_enum: u16,
    pub bit_offset: u16,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Display {
    pub color: i32,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CanapeExt {
    pub version: u16,
    /// `LINK_MAP`.
    pub link_map: Option<LinkMap>,
    /// `DISPLAY`.
    pub display: Option<Display>,
    pub virtual_conversion: Option<String>,
}

impl Node for CanapeExt {
    const KEYWORD: &'static str = "IF_DATA";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        if !cur.take_if("CANAPE_EXT") {
            let t = cur.next_token()?;
            return Err(Error::parse(
                t.line,
                format!("IF_DATA: expected CANAPE_EXT, got {:?}", t.text),
            ));
        }
        let mut node = CanapeExt {
            version: cur.uint::<u16>()?,
            ..CanapeExt::default()
        };
        while !cur.is_empty() {
            if cur.take_if("LINK_MAP") {
                node.link_map = Some(LinkMap {
                    name: cur.string()?,
                    base_address: cur.uint::<u32>()?,
                    base_address_ext: cur.uint::<u16>()?,
                    is_address_relative_to_ds: cur.uint::<u16>()?,
                    offset_segment: cur.int::<i32>()?,
                    is_data_type_valid: cur.uint::<u16>()?,
                    data_typ_enum: cur.uint::<u16>()?,
                    bit_offset: cur.uint::<u16>()?,
                });
            } else if cur.take_if("DISPLAY") {
                node.display = Some(Display {
                    color: cur.int::<i32>()?,
                    min: cur.float()?,
                    max: cur.float()?,
                });
            } else if cur.take_if("VIRTUAL_CONVERSION") {
                node.virtual_conversion = Some(cur.string()?);
            } else {
                cur.next_token()?;
            }
        }
        Ok(node)
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        let _ = w;
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        let mut line = format!("/begin IF_DATA CANAPE_EXT {}", self.version);
        if let Some(lm) = &self.link_map {
            line.push_str(&format!(
                " LINK_MAP \"{}\" 0x{:X} 0x{:X} {} 0x{:X} {} 0x{:X} 0x{:X}",
                escape_str(&lm.name),
                lm.base_address,
                lm.base_address_ext,
                lm.is_address_relative_to_ds,
                lm.offset_segment,
                lm.is_data_type_valid,
                lm.data_typ_enum,
                lm.bit_offset
            ));
        }
        if let Some(d) = &self.display {
            line.push_str(&format!(" DISPLAY {} {} {}", d.color, d.min, d.max));
        }
        if let Some(vc) = &self.virtual_conversion {
            line.push_str(&format!(" VIRTUAL_CONVERSION \"{}\"", escape_str(vc)));
        }
        line.push_str(" /end IF_DATA");
        w.value_line(None, &line);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::build_block_tree;
    use crate::token::tokenize;
    use crate::writer::WriterOptions;

    fn parse_child<T: Node>(root: &Block) -> T {
        T::parse(root.child(T::KEYWORD).unwrap()).unwrap()
    }

    fn write<T: Node>(node: &T) -> String {
        let mut w = Writer::new(WriterOptions::default());
        node.write_block(&mut w).unwrap();
        w.into_string()
    }

    fn root_of(src: &str) -> Block {
        let toks = tokenize(src).unwrap();
        build_block_tree(&toks).unwrap()
    }

    #[test]
    fn canape_ext_single_line() {
        let root = root_of(
            "/begin IF_DATA CANAPE_EXT 100 LINK_MAP \"mapname\" 0x4000 0 0 0 1 2 3 DISPLAY 0x123456 0.0 100.0 VIRTUAL_CONVERSION \"convname\" /end IF_DATA",
        );
        let node = parse_child::<CanapeExt>(&root);
        assert_eq!(node.version, 100);
        let lm = node.link_map.as_ref().unwrap();
        assert_eq!(lm.base_address, 0x4000);
        assert_eq!(lm.offset_segment, 0);
        let d = node.display.as_ref().unwrap();
        assert_eq!(d.color, 0x123456);
        assert_eq!(
            write(&node),
            "/begin IF_DATA CANAPE_EXT 100 LINK_MAP \"mapname\" 0x4000 0x0 0 0x0 1 0x2 0x3 DISPLAY 1193046 0 100 VIRTUAL_CONVERSION \"convname\" /end IF_DATA\n"
        );
    }

    #[test]
    fn canape_ext_version_only() {
        let root = root_of("/begin IF_DATA CANAPE_EXT 100 /end IF_DATA");
        let node = parse_child::<CanapeExt>(&root);
        assert_eq!(write(&node), "/begin IF_DATA CANAPE_EXT 100 /end IF_DATA\n");
    }

    #[test]
    fn map_symbol_with_mapping() {
        let root = root_of(
            "/begin MAP_SYMBOL FIRST \"sym1\" 0x1000 LAST \"sym2\" 0x2000 ADDRESS_MAPPING_XCP \"src.map\" \"dst.map\" /end MAP_SYMBOL",
        );
        let node = parse_child::<MapSymbol>(&root);
        assert_eq!(
            write(&node),
            "/begin MAP_SYMBOL FIRST \"sym1\" 0x1000 LAST \"sym2\" 0x2000\nADDRESS_MAPPING_XCP \"src.map\" \"dst.map\"\n\n/end MAP_SYMBOL\n"
        );
    }

    #[test]
    fn memory_segment_first_last_only() {
        let root = root_of(
            "/begin MEMORY_SEGMENT FIRST \"seg1\" 0x3000 LAST \"seg2\" 0x4000 /end MEMORY_SEGMENT",
        );
        let node = parse_child::<MemorySegment>(&root);
        assert_eq!(
            write(&node),
            "/begin MEMORY_SEGMENT FIRST \"seg1\" 0x3000 LAST \"seg2\" 0x4000\n/end MEMORY_SEGMENT\n"
        );
    }

    #[test]
    fn calibration_method_full_with_incircuit_quirk() {
        let root = root_of(
            "/begin CALIBRATION_METHOD AUTOSAR_SINGLE_POINTERED \"asp\" 0x10 ORIGINAL_POINTER_TABLE \"opt\" 0x20 InCircuit2 \"ic\" 0x30 ORIGINAL_POINTER_TABLE \"opt2\" 0x40 FLASH_SECTION 0x8000 0x1000 /end CALIBRATION_METHOD",
        );
        let node = parse_child::<CalibrationMethod>(&root);
        assert!(node.auto_sar_single_pointered.is_some());
        assert!(node.in_circuit2.as_ref().unwrap().flash_section.is_some());
        assert_eq!(
            write(&node),
            "/begin CALIBRATION_METHOD AUTOSAR_SINGLE_POINTERED \"asp\" 0x10 ORIGINAL_POINTER_TABLE \"opt\" 0x20\n InCircuit2 \"ic\" 0x30 AUTOSAR_SINGLE_POINTERED \"opt2\" 0x40 FLASH_SECTION 0x8000 0x1000\n/end CALIBRATION_METHOD\n"
        );
    }

    #[test]
    fn canape_address_update_full() {
        let root = root_of(
            "/begin IF_DATA CANAPE_ADDRESS_UPDATE EPK_ADDRESS \"epk\" 0x5000 ECU_CALIBRATION_OFFSET \"eco\" 0x100 /begin MAP_SYMBOL FIRST \"sym1\" 0x1000 /end MAP_SYMBOL /begin CALIBRATION_METHOD AUTOSAR_SINGLE_POINTERED \"asp\" 0x10 /end CALIBRATION_METHOD /end IF_DATA",
        );
        let node = parse_child::<CanapeAddressUpdate>(&root);
        assert_eq!(node.epk_address.len(), 1);
        assert_eq!(node.ecu_calibration_offset.as_ref().unwrap().offset, 0x100);
        assert_eq!(node.children.len(), 2);
        assert_eq!(
            write(&node),
            "/begin IF_DATA CANAPE_ADDRESS_UPDATE EPK_ADDRESS \"epk\" 0x5000 ECU_CALIBRATION_OFFSET \"eco\" 256\n  /begin MAP_SYMBOL FIRST \"sym1\" 0x1000\n  /end MAP_SYMBOL\n  /begin CALIBRATION_METHOD AUTOSAR_SINGLE_POINTERED \"asp\" 0x10\n  /end CALIBRATION_METHOD\n/end IF_DATA\n"
        );
    }
}
