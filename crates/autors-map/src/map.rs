//! Linker MAP file parsing.
//! The core types are `MapFile` (a parsed MAP file: `source_file` path plus a
//! symbol table) and `MapSymbolValue` (one symbol: name, address, and a
//! trailing numeric index).
//! MAP file layouts vary widely between toolchains, so token recognition is
//! heuristic, tuned for the common GNU/Keil/Tasking-style symbol forms:
//! - An address token is a whole token consisting of a hexadecimal number with
//!   an optional `0x` prefix. Position matters: the address is only accepted
//!   at token position 0 (where a `0x` prefix is allowed) or token position 1
//!   (where it must be bare hex digits with no `0x` prefix — this keeps e.g. a
//!   leading symbol name followed by a size column from being misread as
//!   address + symbol).
//! - A symbol name token starts with a letter, `_`, `.`, or `$`, followed by
//!   letters, digits, and `_ . $ [ ]`.
//! - When a symbol is not found by name, lookup falls back to the name with a
//!   `"_"` prefix (a common toolchain decoration).
//! - Numeric conversion failures (e.g. a trailing index that overflows `i32`)
//!   are reported as `Err` rather than panicking.

use std::path::Path;

use indexmap::IndexMap;

use autors_a2l::model::module::{Module, ModuleChild};
use autors_a2l::Project;

use crate::error::{Error, Result};
use autors_symbols::update::{
    align_down, data_type_size_in_bytes, modules_with_index, parse_symbol_path,
    AddressNodeResolver, UpdateData, UpdateType, UpdaterSymbols,
};

/// Matches an address token: the whole token must be a hexadecimal number with
/// an optional `0x` prefix; returns the hex-digit part (see module docs).
fn match_address(tok: &str) -> Option<&str> {
    let hex = tok
        .strip_prefix("0x")
        .or_else(|| tok.strip_prefix("0X"))
        .unwrap_or(tok);
    if !hex.is_empty() && hex.len() <= 16 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hex)
    } else {
        None
    }
}

/// Matches a symbol name token (see module docs for the accepted shape).
fn match_symbol(tok: &str) -> bool {
    let mut chars = tok.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '.' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '[' | ']'))
}

/// One symbol from a MAP file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapSymbolValue {
    /// Symbol name.
    pub name: String,
    /// Symbol address.
    pub address: u64,
    /// Trailing numeric index of the name (`i32::MAX` when the name has no
    /// trailing digits).
    pub index: i32,
}

impl MapSymbolValue {
    /// Parses the trailing numeric index of `name`. When the index is 0, also
    /// returns the base name with the trailing digits stripped (used to
    /// register the base name of an array's element 0).
    fn new(name: &str, address: u64) -> Result<(Self, Option<String>)> {
        let digits = name
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .count();
        let mut index = i32::MAX;
        let mut base = None;
        if digits > 0 {
            // An index that overflows i32 is a parse error.
            let v: i32 = name[name.len() - digits..]
                .parse()
                .map_err(|_| Error::Parse {
                    offset: 0,
                    message: format!("invalid trailing index in MAP symbol {name:?}"),
                })?;
            index = v;
            if v == 0 {
                base = Some(name[..name.len() - digits].to_string());
            }
        }
        Ok((
            MapSymbolValue {
                name: name.to_string(),
                address,
                index,
            },
            base,
        ))
    }
}

/// A parsed MAP file.
#[derive(Debug)]
pub struct MapFile {
    /// Path of the file the symbols were loaded from, if any.
    pub source_file: Option<String>,
    /// Symbol table, keyed by symbol name.
    pub symbols: IndexMap<String, MapSymbolValue>,
}

impl MapFile {
    /// Opens and parses a MAP file from disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        Self::open_str(&text, Some(path.to_string_lossy().into_owned()))
    }

    /// Parses a MAP file from its text content.
    /// Each line is split into whitespace-separated tokens; once the first
    /// address token of a line is found, every other symbol-shaped token on
    /// that line is registered as a symbol at that address.
    pub fn open_str(text: &str, source_file: Option<String>) -> Result<Self> {
        let mut file = MapFile {
            source_file,
            symbols: IndexMap::new(),
        };
        for line in text.lines() {
            // Only positions 0 and 1 can contain an address. Re-create this
            // allocation-free iterator for the registration pass below.
            let tokens = || line.split([' ', '\t']).filter(|token| !token.is_empty());
            let mut prefix = tokens();
            let (Some(first), Some(second)) = (prefix.next(), prefix.next()) else {
                continue;
            };
            let (address_token, address_hex, ai) = if let Some(hex) = match_address(first) {
                (first, hex, 0)
            } else if let Some(hex) = match_address(second).filter(|hex| second.len() <= hex.len())
            {
                (second, hex, 1)
            } else {
                continue;
            };
            let address = u64::from_str_radix(address_hex, 16).map_err(|_| Error::Parse {
                offset: 0,
                message: format!("invalid MAP address {address_token:?}"),
            })?;
            for (j, tok) in tokens().enumerate() {
                if j == ai || !match_symbol(tok) {
                    continue;
                }
                let (sv, base) = MapSymbolValue::new(tok, address)?;
                file.symbols.insert(sv.name.clone(), sv);
                // Symbols with index 0 also register their base name (without
                // overwriting an existing entry).
                if let Some(base) = base {
                    if !base.is_empty() && !file.symbols.contains_key(&base) {
                        let (sv2, _) = MapSymbolValue::new(&base, address)?;
                        file.symbols.insert(base, sv2);
                    }
                }
            }
        }
        Ok(file)
    }

    /// Computes the list of nodes whose addresses need to be synchronized.
    /// Returns `(records, ds_start, ds_len)`. MAP files carry no data-section
    /// length, so `ds_len` is always 0.
    pub fn get_values_to_synchronize(
        &self,
        project: &Project,
        address_multiplier: u64,
    ) -> Result<(Vec<UpdateData>, u64, u64)> {
        let mut ds_start = u64::MAX;
        let ds_len = 0u64;
        let mut list = Vec::new();
        let resolver = AddressNodeResolver::new(project);
        for (mi, module) in modules_with_index(project) {
            let dims = build_dims_dict(module);
            for (ci, child) in module.children.iter().enumerate() {
                let Some(node) = resolver.address_node(child)? else {
                    continue;
                };
                // RECORD_LAYOUT_REF nodes whose layout cannot be resolved are skipped.
                if node.is_record_layout_ref && node.record_layout.is_none() {
                    continue;
                }
                let (text, off0) = node.symbol_name();
                let elem = map_element_size(&node)?;
                let off = off0 + map_array_offset(&dims, &text, elem)?;
                let path = parse_symbol_path(&text)?;
                // An empty symbol path cannot be looked up; treat as NotMatched.
                let Some(first) = path.first() else {
                    list.push(UpdateData::not_matched(mi, ci));
                    continue;
                };
                let base = &first.name;
                let sv = self.symbols.get(base).or_else(|| {
                    if !base.starts_with('_') {
                        // Fallback: toolchain-decorated name with "_" prefix.
                        self.symbols.get(&format!("_{base}"))
                    } else {
                        None
                    }
                });
                let Some(sv) = sv else {
                    list.push(UpdateData::not_matched(mi, ci));
                    continue;
                };
                let updater = UpdaterSymbols::from_map(&sv.name, sv.address);
                let addr = updater.address.wrapping_mul(address_multiplier);
                if node.is_record_layout_ref {
                    ds_start = ds_start.min(addr);
                }
                // Address comparison is done with signed 64-bit arithmetic.
                let cur = i64::from(node.address.unwrap_or(u32::MAX));
                let typ = if cur - i64::from(off) == addr as i64 {
                    UpdateType::Matched
                } else {
                    UpdateType::AdjustAddress
                };
                list.push(UpdateData {
                    module_idx: mi,
                    child_idx: ci,
                    // Negative offsets wrap around as unsigned.
                    address: addr.wrapping_add(off as u64),
                    size: 0,
                    bit_mask: u64::MAX,
                    typ,
                });
            }
        }
        // ds_start is truncated to 32 bits and aligned down to 4 bytes.
        ds_start = i64::from(align_down(ds_start as u32 as i32, 4)) as u64;
        Ok((list, ds_start, ds_len))
    }
}

/// Element size in bytes. MEASUREMENT nodes use their `DataType`;
/// RECORD_LAYOUT_REF nodes use `FncValues?.DataType ?? AxisPts[0].DataType`
/// (an error when neither is present); all other nodes are treated as SBYTE
/// (1 byte).
fn map_element_size(node: &autors_symbols::update::AddressNode) -> Result<i32> {
    if let Some(dt) = node.data_type {
        return data_type_size_in_bytes(dt).ok_or_else(|| {
            Error::Update(format!("unsupported DATA_TYPE for node {:?}", node.name))
        });
    }
    if node.is_record_layout_ref {
        let rl = node.record_layout.ok_or_else(|| {
            Error::Update(format!(
                "record layout of node {:?} is not resolvable",
                node.name
            ))
        })?;
        let dt = rl
            .fnc_values
            .as_ref()
            .map(|f| f.data_type)
            .or_else(|| rl.axis_pts[0].as_ref().map(|a| a.data_type));
        let dt = dt.ok_or_else(|| {
            Error::Update(format!(
                "record layout {:?} has neither FNC_VALUES nor AXIS_PTS_X",
                rl.name
            ))
        })?;
        return data_type_size_in_bytes(dt).ok_or_else(|| {
            Error::Update(format!(
                "unsupported DATA_TYPE in record layout {:?}",
                rl.name
            ))
        });
    }
    Ok(1)
}

fn map_array_offset(dims: &IndexMap<String, Vec<i32>>, text: &str, elem_size: i32) -> Result<i32> {
    let Some((base, idxs)) = parse_map_path(text) else {
        return Ok(0);
    };
    let dims = dims
        .get(&base)
        .ok_or_else(|| Error::Update(format!("no dimension info for symbol base {base:?}")))?;
    let mut num = 0i32;
    for (i, &ix) in idxs.iter().enumerate() {
        let mut n = 1i32;
        for &d in dims.iter().skip(i + 1) {
            n *= d;
        }
        num += ix * n;
    }
    Ok(num * elem_size)
}

fn parse_map_path(text: &str) -> Option<(String, Vec<i32>)> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let mut idxs = Vec::with_capacity(parts.len() - 1);
    for p in &parts[1..] {
        idxs.push(p.trim_matches('_').parse::<i32>().ok()?);
    }
    Some((parts[0].to_string(), idxs))
}

fn build_dims_dict(module: &Module) -> IndexMap<String, Vec<i32>> {
    let mut dict: IndexMap<String, Vec<i32>> = IndexMap::new();
    for child in &module.children {
        let Some(sl) = symbol_link_of(child) else {
            continue;
        };
        let Some((base, idxs)) = parse_map_path(sl) else {
            continue;
        };
        let arr: Vec<i32> = idxs.iter().map(|v| v + 1).collect();
        match dict.get_mut(&base) {
            Some(v) if v.len() == arr.len() => {
                for (i, d) in v.iter_mut().enumerate() {
                    *d = (*d).max(arr[i]);
                }
            }
            Some(_) => {}
            None => {
                dict.insert(base, arr);
            }
        }
    }
    dict
}

fn symbol_link_of(child: &ModuleChild) -> Option<&str> {
    match child {
        ModuleChild::Measurement(n) => n.addr.symbol_link.as_deref(),
        ModuleChild::Characteristic(n) => n.addr.symbol_link.as_deref(),
        ModuleChild::AxisPts(n) => n.addr.symbol_link.as_deref(),
        ModuleChild::Blob(n) => n.addr.symbol_link.as_deref(),
        ModuleChild::Instance(n) => n.addr.symbol_link.as_deref(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = "\
 Linker map
  0x00008000  gCounter   0x00000004  Data
  0x00009000  gStruct    0x00000008  Data
  0x0000A000  arr0       0x00000010  Data
  junk line without address
  0x0000B000 _privSym   0x00000002  Data
";

    #[test]
    fn parses_map_symbols() {
        let file = MapFile::open_str(MAP, None).unwrap();
        let sym = &file.symbols["gCounter"];
        assert_eq!(sym.address, 0x8000);
        assert_eq!(sym.index, i32::MAX);
        assert_eq!(file.symbols["gStruct"].address, 0x9000);
        assert_eq!(file.symbols["arr0"].index, 0);
        assert_eq!(file.symbols["arr"].address, 0xA000);
        assert!(!file.symbols.contains_key("junk"));
        assert!(file.symbols.contains_key("_privSym"));
    }

    #[test]
    fn trailing_digits_index() {
        let (sv, base) = MapSymbolValue::new("arr12", 0x1000).unwrap();
        assert_eq!(sv.index, 12);
        assert_eq!(base, None);
        let (sv, base) = MapSymbolValue::new("arr0", 0x1000).unwrap();
        assert_eq!(sv.index, 0);
        assert_eq!(base.as_deref(), Some("arr"));
        let (sv, base) = MapSymbolValue::new("plain", 0x1000).unwrap();
        assert_eq!(sv.index, i32::MAX);
        assert_eq!(base, None);
    }

    #[test]
    fn map_get_values_to_synchronize() {
        let file = MapFile::open_str(MAP, None).unwrap();
        let a2l = r#"/begin PROJECT P "d"
/begin MODULE M "m"
  /begin MEASUREMENT Meas1 "d" ULONG Conv 1 0 0 100 ECU_ADDRESS 0x1000
    SYMBOL_LINK "gCounter" 0
  /end MEASUREMENT
  /begin MEASUREMENT Meas2 "d" ULONG Conv 1 0 0 100 ECU_ADDRESS 0x9000
    SYMBOL_LINK "gStruct" 0
  /end MEASUREMENT
  /begin MEASUREMENT Meas3 "d" UWORD Conv 1 0 0 100 ECU_ADDRESS 0x2000
    SYMBOL_LINK "gUnknown" 0
  /end MEASUREMENT
/end MODULE
/end PROJECT
"#;
        let project = Project::parse_str(a2l).unwrap();
        let (records, _ds_start, _ds_len) = file.get_values_to_synchronize(&project, 1).unwrap();
        assert_eq!(records.len(), 3);
        // Meas1:0x1000 → 0x8000 AdjustAddress
        assert_eq!(records[0].typ, UpdateType::AdjustAddress);
        assert_eq!(records[0].address, 0x8000);
        assert_eq!(records[1].typ, UpdateType::Matched);
        assert_eq!(records[2].typ, UpdateType::NotMatched);
    }

    #[test]
    fn map_array_offset_via_symbol_link_dims() {
        let map =
            "  0x0000A000  arr.0.0  0x00000002  Data\n  0x0000A00A  arr.1.2  0x00000002  Data\n";
        let file = MapFile::open_str(map, None).unwrap();
        let a2l = r#"/begin PROJECT P "d"
/begin MODULE M "m"
  /begin MEASUREMENT MeasA "d" UWORD Conv 1 0 0 100 ECU_ADDRESS 0xA000
    SYMBOL_LINK "arr.0.0" 0
  /end MEASUREMENT
  /begin MEASUREMENT MeasB "d" UWORD Conv 1 0 0 100 ECU_ADDRESS 0x9000
    SYMBOL_LINK "arr.1.2" 0
  /end MEASUREMENT
/end MODULE
/end PROJECT
"#;
        let project = Project::parse_str(a2l).unwrap();
        let (records, _, _) = file.get_values_to_synchronize(&project, 1).unwrap();
        assert_eq!(records.len(), 2);
        // dims = max([0+1,0+1],[1+1,2+1]) = [2,3]
        assert_eq!(records[0].typ, UpdateType::Matched);
        assert_eq!(records[1].typ, UpdateType::AdjustAddress);
        assert_eq!(records[1].address, 0xA00A + 10);
    }
}
