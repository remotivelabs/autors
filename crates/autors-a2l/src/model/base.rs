//! `A2LCONVERSION_REF`, `A2LRECORD_LAYOUT_REF`, `A2LREFBASE`, `A2LREFERENCE`.
//! ```text
//! // parse:
//! let mut named = NamedFields::read_header(&mut cur)?;      // Name + LONG_IDENT
//! while !cur.is_empty() {
//!     if addr.take(&mut cur)? || conv.take(&mut cur)? || rec.take(&mut cur)? {
//!         continue;
//!     }
//! }
//! named.write(w)?;            // "<LONG_IDENT>"
//! conv.write_limits(w)?;      // "<lower> <upper>"
//! addr.write(w)?;             // ECU_ADDRESS_EXTENSION … MODEL_LINK
//! conv.write(w)?;             // FORMAT / PHYS_UNIT / BYTE_ORDER / REF_MEMORY_SEGMENT
//! rec.write(w)?;              // READ_ONLY / EXTENDED_LIMITS / STEP_SIZE / GUARD_RAILS
//! ```

use crate::error::{Error, Result};
use crate::model::enums::{A2lKeyword, CalibrationAccess, ScalingUnits};
use crate::params::ParamCursor;
use crate::writer::Writer;

fn to_dec(v: f64) -> String {
    format!("{v}")
}

fn scaling_unit_from_u32(v: u32) -> Option<ScalingUnits> {
    Some(match v {
        0 => ScalingUnits::Time_1uSec,
        1 => ScalingUnits::Time_10uSec,
        2 => ScalingUnits::Time_100uSec,
        3 => ScalingUnits::Time_1mSec,
        4 => ScalingUnits::Time_10mSec,
        5 => ScalingUnits::Time_100mSec,
        6 => ScalingUnits::Time_1Sec,
        7 => ScalingUnits::Time_10Sec,
        8 => ScalingUnits::Time_1Min,
        9 => ScalingUnits::Time_1Hour,
        10 => ScalingUnits::Time_1Day,
        100 => ScalingUnits::AngularDegrees,
        101 => ScalingUnits::Revolutions,
        102 => ScalingUnits::Cycle,
        103 => ScalingUnits::CylinderSegmentCombustion,
        998 => ScalingUnits::FrameAvailableEvent,
        999 => ScalingUnits::AlwaysOnNewValue,
        1000 => ScalingUnits::NonDeternministic,
        _ => return None,
    })
}

#[allow(non_camel_case_types)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ByteOrder {
    #[default]
    NotSet,
    MSB_FIRST,
    MSB_LAST,
}

impl A2lKeyword for ByteOrder {
    fn as_keyword(self) -> Option<&'static str> {
        match self {
            ByteOrder::MSB_FIRST => Some("MSB_FIRST"),
            ByteOrder::MSB_LAST => Some("MSB_LAST"),
            ByteOrder::NotSet => None,
        }
    }

    fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "MSB_FIRST" => Some(ByteOrder::MSB_FIRST),
            "MSB_LAST" => Some(ByteOrder::MSB_LAST),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NamedFields {
    pub name: String,
    pub description: Option<String>,
}

impl NamedFields {
    pub fn read_header(cur: &mut ParamCursor) -> Result<Self> {
        let name = cur.ident()?;
        let description = Some(cur.string()?);
        Ok(NamedFields { name, description })
    }

    pub fn take(&mut self, _cur: &mut ParamCursor) -> Result<bool> {
        Ok(false)
    }

    /// `write(wr, indent, quotes: true, null, escapeStr(Description))`:
    pub fn write(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, self.description.as_deref(), true);
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AddressFields {
    pub address: Option<u32>,
    pub address_extension: Option<i32>,
    pub display_name: Option<String>,
    pub calib_access: CalibrationAccess,
    pub max_refresh_unit: Option<ScalingUnits>,
    pub max_refresh_rate: i32,
    pub symbol_link: Option<String>,
    pub symbol_offset: i32,
    pub model_link: Option<String>,
}

impl AddressFields {
    /// `CALIBRATION_ACCESS` / `MAX_REFRESH` / `SYMBOL_LINK` / `MODEL_LINK`).
    pub fn take(&mut self, cur: &mut ParamCursor) -> Result<bool> {
        if cur.take_if("ECU_ADDRESS_EXTENSION") {
            self.address_extension = Some(cur.int()?);
        } else if cur.take_if("DISPLAY_IDENTIFIER") {
            self.display_name = Some(cur.ident()?);
        } else if cur.take_if("CALIBRATION_ACCESS") {
            let t = cur.next_token()?;
            self.calib_access = CalibrationAccess::from_keyword(&t.text).ok_or_else(|| {
                Error::parse(
                    t.line,
                    format!("CALIBRATION_ACCESS: unknown value {:?}", t.text),
                )
            })?;
        } else if cur.take_if("MAX_REFRESH") {
            let t = cur.next_token()?;
            let v = crate::params::A2lUint::parse_a2l(&t.text).ok_or_else(|| {
                Error::parse(
                    t.line,
                    format!("MAX_REFRESH: expected integer, got {:?}", t.text),
                )
            })?;
            self.max_refresh_unit = match v {
                2147483647 => None,
                _ => Some(scaling_unit_from_u32(v).ok_or_else(|| {
                    Error::parse(t.line, format!("MAX_REFRESH: unknown scaling unit {v}"))
                })?),
            };
            self.max_refresh_rate = cur.int()?;
        } else if cur.take_if("SYMBOL_LINK") {
            self.symbol_link = Some(cur.ident()?);
            self.symbol_offset = cur.int()?;
        } else if cur.take_if("MODEL_LINK") {
            self.model_link = Some(cur.string()?);
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    pub fn write(&self, w: &mut Writer) -> Result<()> {
        if let Some(ext) = self.address_extension {
            w.tag_value(Some("ECU_ADDRESS_EXTENSION"), Some(&ext.to_string()), false);
        }
        if let Some(dn) = self.display_name.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("DISPLAY_IDENTIFIER"), Some(dn), false);
        }
        if let Some(kw) = self.calib_access.as_keyword() {
            w.tag_value(Some("CALIBRATION_ACCESS"), Some(kw), false);
        }
        if let Some(unit) = self.max_refresh_unit {
            w.tag_value(
                Some("MAX_REFRESH"),
                Some(&format!("{} {}", unit as u32, self.max_refresh_rate)),
                false,
            );
        }
        if let Some(link) = self.symbol_link.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(
                Some("SYMBOL_LINK"),
                Some(&format!("\"{link}\" {}", self.symbol_offset)),
                false,
            );
        }
        if let Some(ml) = self.model_link.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("MODEL_LINK"), Some(ml), true);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConversionRefFields {
    pub conversion: String,
    pub lower_limit: f64,
    pub upper_limit: f64,
    pub format: Option<String>,
    pub phys_unit: Option<String>,
    pub byte_order: ByteOrder,
    pub memory_segment_ref: Option<String>,
}

impl ConversionRefFields {
    /// `REF_MEMORY_SEGMENT`).
    pub fn take(&mut self, cur: &mut ParamCursor) -> Result<bool> {
        if cur.take_if("FORMAT") {
            self.format = Some(cur.string()?);
        } else if cur.take_if("PHYS_UNIT") {
            self.phys_unit = Some(cur.string()?);
        } else if cur.take_if("BYTE_ORDER") {
            let t = cur.next_token()?;
            self.byte_order = ByteOrder::from_keyword(&t.text).ok_or_else(|| {
                Error::parse(t.line, format!("BYTE_ORDER: unknown value {:?}", t.text))
            })?;
        } else if cur.take_if("REF_MEMORY_SEGMENT") {
            self.memory_segment_ref = Some(cur.ident()?);
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    pub fn write_limits(&self, w: &mut Writer) -> Result<()> {
        w.value_line(
            None,
            &format!("{} {}", to_dec(self.lower_limit), to_dec(self.upper_limit)),
        );
        Ok(())
    }

    pub fn write(&self, w: &mut Writer) -> Result<()> {
        if let Some(f) = self.format.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("FORMAT"), Some(f), true);
        }
        if let Some(u) = self.phys_unit.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("PHYS_UNIT"), Some(u), true);
        }
        if let Some(kw) = self.byte_order.as_keyword() {
            w.tag_value(Some("BYTE_ORDER"), Some(kw), false);
        }
        if let Some(ms) = self.memory_segment_ref.as_deref().filter(|s| !s.is_empty()) {
            w.tag_value(Some("REF_MEMORY_SEGMENT"), Some(ms), false);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecordLayoutRefFields {
    pub record_layout: String,
    pub max_diff: f64,
    pub read_only: bool,
    pub guard_rails: bool,
    pub extended_limits: Option<(f64, f64)>,
    pub step_size: Option<f64>,
}

impl RecordLayoutRefFields {
    /// `GUARD_RAILS`).
    pub fn take(&mut self, cur: &mut ParamCursor) -> Result<bool> {
        if cur.take_if("READ_ONLY") {
            self.read_only = true;
        } else if cur.take_if("EXTENDED_LIMITS") {
            self.extended_limits = Some((cur.float()?, cur.float()?));
        } else if cur.take_if("STEP_SIZE") {
            self.step_size = Some(cur.float()?);
        } else if cur.take_if("GUARD_RAILS") {
            self.guard_rails = true;
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    pub fn write(&self, w: &mut Writer) -> Result<()> {
        if self.read_only {
            w.value_line(None, "READ_ONLY");
        }
        if let Some((lo, hi)) = self.extended_limits {
            w.value_line(
                None,
                &format!("EXTENDED_LIMITS {} {}", to_dec(lo), to_dec(hi)),
            );
        }
        if let Some(ss) = self.step_size {
            w.tag_value(Some("STEP_SIZE"), Some(&to_dec(ss)), false);
        }
        if self.guard_rails {
            w.value_line(None, "GUARD_RAILS");
        }
        Ok(())
    }
}

/// SUB_FUNCTION / SUB_GROUP / FUNCTION_LIST / DEF_CHARACTERISTIC / IN_MEASUREMENT /
/// OUT_MEASUREMENT / LOC_MEASUREMENT / VIRTUAL / DEPENDENT_CHARACTERISTIC /
/// TRANSFORMER_IN_OBJECTS / TRANSFORMER_OUT_OBJECTS / VIRTUAL_CHARACTERISTIC
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferenceFields {
    pub references: Vec<String>,
}

impl ReferenceFields {
    pub fn take(&mut self, cur: &mut ParamCursor) -> Result<bool> {
        if cur.is_empty() {
            return Ok(false);
        }
        self.references.push(cur.ident()?);
        Ok(true)
    }

    pub fn write(&self, w: &mut Writer) -> Result<()> {
        w.references(&self.references);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{build_block_tree, Block};
    use crate::token::tokenize;
    use crate::writer::WriterOptions;

    fn block_of(src: &str) -> Block {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        root.child("M").unwrap().clone()
    }

    fn writer() -> Writer {
        Writer::new(WriterOptions::default())
    }

    #[test]
    fn named_reads_header_and_writes_description() {
        let b = block_of("/begin M MyName \"long desc\" EXTRA /end M");
        let mut cur = ParamCursor::new(&b);
        let mut named = NamedFields::read_header(&mut cur).unwrap();
        assert_eq!(named.name, "MyName");
        assert_eq!(named.description.as_deref(), Some("long desc"));
        assert!(!named.take(&mut cur).unwrap());
        assert_eq!(cur.peek().unwrap().text, "EXTRA");

        let mut w = writer();
        named.write(&mut w).unwrap();
        assert_eq!(w.into_string(), "\"long desc\"\n");
    }

    #[test]
    fn named_writes_empty_description_when_unset() {
        let named = NamedFields::default();
        let mut w = writer();
        named.write(&mut w).unwrap();
        assert_eq!(w.into_string(), "\"\"\n");
    }

    #[test]
    fn address_fields_roundtrip() {
        let b = block_of(
            "/begin M ECU_ADDRESS_EXTENSION 2 DISPLAY_IDENTIFIER disp \
             CALIBRATION_ACCESS OFFLINE_CALIBRATION MAX_REFRESH 3 10 \
             SYMBOL_LINK \"sym\" -4 MODEL_LINK \"mdl\" /end M",
        );
        let mut cur = ParamCursor::new(&b);
        let mut f = AddressFields::default();
        while !cur.is_empty() {
            assert!(f.take(&mut cur).unwrap());
        }
        assert_eq!(f.address_extension, Some(2));
        assert_eq!(f.display_name.as_deref(), Some("disp"));
        assert_eq!(f.calib_access, CalibrationAccess::OFFLINE_CALIBRATION);
        assert_eq!(f.max_refresh_unit, Some(ScalingUnits::Time_1mSec));
        assert_eq!(f.max_refresh_rate, 10);
        assert_eq!(f.symbol_link.as_deref(), Some("sym"));
        assert_eq!(f.symbol_offset, -4);
        assert_eq!(f.model_link.as_deref(), Some("mdl"));

        let mut w = writer();
        f.write(&mut w).unwrap();
        assert_eq!(
            w.into_string(),
            "ECU_ADDRESS_EXTENSION 2\n\
             DISPLAY_IDENTIFIER disp\n\
             CALIBRATION_ACCESS OFFLINE_CALIBRATION\n\
             MAX_REFRESH 3 10\n\
             SYMBOL_LINK \"sym\" -4\n\
             MODEL_LINK \"mdl\"\n"
        );
    }

    #[test]
    fn address_fields_write_skips_defaults() {
        let f = AddressFields::default();
        let mut w = writer();
        f.write(&mut w).unwrap();
        assert_eq!(w.into_string(), "");
    }

    #[test]
    fn address_fields_take_ignores_ecu_address() {
        let b = block_of("/begin M ECU_ADDRESS 0x1000 /end M");
        let mut cur = ParamCursor::new(&b);
        let mut f = AddressFields::default();
        assert!(!f.take(&mut cur).unwrap());
        assert_eq!(cur.peek().unwrap().text, "ECU_ADDRESS");
    }

    #[test]
    fn address_fields_max_refresh_not_set_and_unknown() {
        let b = block_of("/begin M MAX_REFRESH 2147483647 5 /end M");
        let mut cur = ParamCursor::new(&b);
        let mut f = AddressFields::default();
        assert!(f.take(&mut cur).unwrap());
        assert_eq!(f.max_refresh_unit, None);
        let mut w = writer();
        f.write(&mut w).unwrap();
        assert_eq!(w.into_string(), "");

        let b = block_of("/begin M MAX_REFRESH 77 1 /end M");
        let mut cur = ParamCursor::new(&b);
        let mut f = AddressFields::default();
        assert!(f.take(&mut cur).is_err());
    }

    #[test]
    fn conversion_ref_fields_roundtrip() {
        let b = block_of(
            "/begin M FORMAT \"%4.2\" PHYS_UNIT \"bar\" BYTE_ORDER MSB_FIRST \
             REF_MEMORY_SEGMENT Seg1 /end M",
        );
        let mut cur = ParamCursor::new(&b);
        let mut f = ConversionRefFields::default();
        while !cur.is_empty() {
            assert!(f.take(&mut cur).unwrap());
        }
        assert_eq!(f.format.as_deref(), Some("%4.2"));
        assert_eq!(f.phys_unit.as_deref(), Some("bar"));
        assert_eq!(f.byte_order, ByteOrder::MSB_FIRST);
        assert_eq!(f.memory_segment_ref.as_deref(), Some("Seg1"));

        let mut w = writer();
        f.write(&mut w).unwrap();
        assert_eq!(
            w.into_string(),
            "FORMAT \"%4.2\"\nPHYS_UNIT \"bar\"\nBYTE_ORDER MSB_FIRST\nREF_MEMORY_SEGMENT Seg1\n"
        );
    }

    #[test]
    fn conversion_ref_write_limits() {
        let f = ConversionRefFields {
            lower_limit: -1.5,
            upper_limit: 250.0,
            ..Default::default()
        };
        let mut w = writer();
        f.write_limits(&mut w).unwrap();
        assert_eq!(w.into_string(), "-1.5 250\n");
    }

    #[test]
    fn conversion_ref_rejects_unknown_byte_order() {
        let b = block_of("/begin M BYTE_ORDER LSB_FIRST /end M");
        let mut cur = ParamCursor::new(&b);
        let mut f = ConversionRefFields::default();
        assert!(f.take(&mut cur).is_err());
    }

    #[test]
    fn record_layout_ref_fields_roundtrip() {
        let b =
            block_of("/begin M READ_ONLY EXTENDED_LIMITS -10 200 STEP_SIZE 0.5 GUARD_RAILS /end M");
        let mut cur = ParamCursor::new(&b);
        let mut f = RecordLayoutRefFields::default();
        while !cur.is_empty() {
            assert!(f.take(&mut cur).unwrap());
        }
        assert!(f.read_only);
        assert_eq!(f.extended_limits, Some((-10.0, 200.0)));
        assert_eq!(f.step_size, Some(0.5));
        assert!(f.guard_rails);

        let mut w = writer();
        f.write(&mut w).unwrap();
        assert_eq!(
            w.into_string(),
            "READ_ONLY\nEXTENDED_LIMITS -10 200\nSTEP_SIZE 0.5\nGUARD_RAILS\n"
        );
    }

    #[test]
    fn record_layout_ref_write_skips_defaults() {
        let f = RecordLayoutRefFields::default();
        let mut w = writer();
        f.write(&mut w).unwrap();
        assert_eq!(w.into_string(), "");
    }

    #[test]
    fn reference_fields_wrap_by_columns() {
        let b = block_of("/begin M A B C D E /end M");
        let mut cur = ParamCursor::new(&b);
        let mut f = ReferenceFields::default();
        while !cur.is_empty() {
            assert!(f.take(&mut cur).unwrap());
        }
        assert_eq!(f.references, ["A", "B", "C", "D", "E"]);

        let mut w = writer();
        f.write(&mut w).unwrap();
        assert_eq!(w.into_string(), "A B C\nD E\n");
    }

    #[test]
    fn reference_fields_write_skips_empty() {
        let f = ReferenceFields::default();
        let mut w = writer();
        f.write(&mut w).unwrap();
        assert_eq!(w.into_string(), "");
    }

    #[test]
    fn byte_order_keywords() {
        assert_eq!(
            ByteOrder::from_keyword("msb_first"),
            Some(ByteOrder::MSB_FIRST)
        );
        assert_eq!(
            ByteOrder::from_keyword("MSB_LAST"),
            Some(ByteOrder::MSB_LAST)
        );
        assert_eq!(ByteOrder::from_keyword("MSB"), None);
        assert_eq!(ByteOrder::MSB_FIRST.as_keyword(), Some("MSB_FIRST"));
        assert_eq!(ByteOrder::NotSet.as_keyword(), None);
    }

    #[test]
    fn composed_write_order_matches_expected_contract() {
        let named = NamedFields {
            name: "C".to_string(),
            description: Some("d".to_string()),
        };
        let addr = AddressFields {
            calib_access: CalibrationAccess::CALIBRATION,
            ..Default::default()
        };
        let conv = ConversionRefFields {
            conversion: "Conv".to_string(),
            lower_limit: 0.0,
            upper_limit: 100.0,
            byte_order: ByteOrder::MSB_LAST,
            ..Default::default()
        };
        let rec = RecordLayoutRefFields {
            record_layout: "RL".to_string(),
            read_only: true,
            ..Default::default()
        };

        let mut w = writer();
        named.write(&mut w).unwrap();
        conv.write_limits(&mut w).unwrap();
        addr.write(&mut w).unwrap();
        conv.write(&mut w).unwrap();
        rec.write(&mut w).unwrap();
        assert_eq!(
            w.into_string(),
            "\"d\"\n0 100\nCALIBRATION_ACCESS CALIBRATION\nBYTE_ORDER MSB_LAST\nREAD_ONLY\n"
        );
    }
}
