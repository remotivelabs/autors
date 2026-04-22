//! A2L address updates: symbol path parsing, update records, and write-back.
//! Core types: [`parse_symbol_path`] splits a symbol path (e.g.
//! `struct.member`, `arr[0][1]`) into segments; [`UpdaterSymbols`] carries
//! address/size/bit information for one symbol; [`UpdateData`] describes one
//! pending update; [`update_and_write_a2l`] applies update records to a
//! `Project` and writes the A2L file back to disk.
//! Design notes:
//! - Nodes targeted by [`UpdateData`] are located by `(module_idx, child_idx)`
//!   indices into `Project.children` / `Module.children`, because the model
//!   owns its nodes; records are produced by `get_values_to_synchronize` (in
//!   the parser crates) and consumed by [`update_and_write_a2l`];
//! - Instance nodes are always treated as regular (non-instanced) addressable
//!   nodes: the file parsing path never constructs instanced nodes;
//! - Record layouts referenced by name are resolved by scanning the whole
//!   `Project` from back to front, so a later definition with the same name
//!   overrides an earlier one;
//! - Node deletion (`delete_not_matched_from_model`) removes `Module.children`
//!   elements by index; to avoid index shifts, deletions are applied after all
//!   address updates, in reverse index order (records point at distinct node
//!   objects, so the deletions do not interfere with each other);
//! - `build_bitmask` / `align_up` / `align_down` implement the bitmask and
//!   alignment semantics documented on each function;
//! - Digit tests in symbol path parsing use ASCII digits (symbol paths only
//!   contain ASCII in practice); a path starting with `.` that triggers a
//!   segment flush is treated leniently as a non-digit instead of failing.

use std::collections::HashMap;
use std::path::Path;

use autors_a2l::block::Item;
use autors_a2l::model::characteristic::{AxisPts, Characteristic};
use autors_a2l::model::enums::{AddrType, CharacteristicType, DataType, IndexMode};
use autors_a2l::model::measurement::{AxisDescr, Measurement};
use autors_a2l::model::module::{Module, ModuleChild};
use autors_a2l::model::project::ProjectChild;
use autors_a2l::model::record_layout::RecordLayout;
use autors_a2l::model::unsupported::UnsupportedNode;
use autors_a2l::Project;

use crate::error::{Error, Result};

/// Symbol path separator set: `_`, `.`, `[`, `]`.
const PATH_SEPS: [char; 4] = ['_', '.', '[', ']'];

/// Default alignment table (same as in autors-a2l record_layout.rs):
/// BYTE=1, WORD=2, LONG=4, FLOAT32_IEEE=4, FLOAT64_IEEE=4, INT64=8, FLOAT16_IEEE=2.
const DEFAULT_ALIGNMENTS: [i32; 7] = [1, 2, 4, 4, 4, 8, 2];

// ---------------------------------------------------------------------------
// Symbol path parsing
// ---------------------------------------------------------------------------

/// One segment of a symbol path: a name plus an optional array index sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolPathElem {
    /// Segment name.
    pub name: String,
    /// Array index sequence (`None` when the segment has no subscripts).
    pub indexes: Option<Vec<i64>>,
}

/// Parses a symbol path (e.g. `struct.member`, `arr[0][1]`) into segments.
/// The string is scanned from back to front and split by a separator/letter
/// state machine; a trailing `[0][1]`-style run within a segment is parsed as
/// the array index list. A non-numeric index yields a parse error.
pub fn parse_symbol_path(s: &str) -> Result<Vec<SymbolPathElem>> {
    #[derive(Default)]
    struct State {
        name: String, // accumulated in reverse
        idx: String,  // accumulated in reverse
    }

    /// Appends a character to the current target buffer (`true` → index
    /// buffer, `false` → name buffer).
    fn push_char(state: &mut State, use_idx: bool, c: char) {
        if use_idx {
            state.idx.push(c);
        } else {
            state.name.push(c);
        }
    }

    /// Flushes the current accumulation into one path segment; returns `false`
    /// when the name buffer is empty.
    fn flush(state: &mut State, list: &mut Vec<SymbolPathElem>) -> Result<bool> {
        if state.name.is_empty() {
            return Ok(false);
        }
        let mut indexes: Option<Vec<i64>> = None;
        let idx_len = state.idx.chars().count();
        if idx_len > 2 && state.idx.starts_with(PATH_SEPS) && state.idx.ends_with(PATH_SEPS) {
            let reversed: String = state.idx.chars().rev().collect();
            let mut parts = reversed
                .split(|c| PATH_SEPS.contains(&c))
                .filter(|p| !p.is_empty())
                .peekable();
            if parts.peek().is_some() {
                let mut v = Vec::new();
                for p in parts {
                    v.push(p.parse::<i64>().map_err(|_| Error::Parse {
                        offset: 0,
                        message: format!("invalid array index {p:?} in symbol path"),
                    })?);
                }
                indexes = Some(v);
                state.idx.clear();
            }
        }
        // Prepend the index buffer (possibly cleared) to the name, then
        // reverse the whole thing.
        state.idx.push_str(&state.name);
        let name: String = state
            .idx
            .chars()
            .rev()
            .collect::<String>()
            .trim_matches('.')
            .to_string();
        list.push(SymbolPathElem { name, indexes });
        state.idx.clear();
        state.name.clear();
        Ok(true)
    }

    let chars: Vec<char> = s.chars().collect();
    let mut state = State::default();
    let mut list = Vec::new();
    let mut in_word = false;
    let mut use_idx = true; // current target: true → index buffer, false → name buffer
    let mut i = chars.len();
    while i > 0 {
        i -= 1;
        let c = chars[i];
        match c {
            '.' => {
                if flush(&mut state, &mut list)? {
                    in_word = false;
                    // A '.' is skipped when the character before it is not a digit.
                    let prev_is_digit = i > 0 && chars[i - 1].is_ascii_digit();
                    if !prev_is_digit {
                        continue;
                    }
                }
                // fallthrough: outside a word, switch to the index buffer and append
                if !in_word {
                    use_idx = true;
                }
                push_char(&mut state, use_idx, c);
            }
            '[' => {
                if !in_word {
                    state.idx.push(c);
                    use_idx = false;
                    continue;
                }
                // fallthrough: inside a word, the target buffer stays unchanged
                push_char(&mut state, use_idx, c);
            }
            ']' | '_' => {
                if !in_word {
                    use_idx = true;
                }
                push_char(&mut state, use_idx, c);
            }
            _ => {
                if c.is_alphabetic() {
                    in_word = true;
                    use_idx = false;
                }
                push_char(&mut state, use_idx, c);
            }
        }
    }
    flush(&mut state, &mut list)?;
    list.reverse();
    Ok(list)
}

// ---------------------------------------------------------------------------
// Update records and symbol information
// ---------------------------------------------------------------------------

/// Update classification of one A2L node against its symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateType {
    /// Not matched (display text: "not matched").
    NotMatched,
    /// Matched (address, size, and mask all agree).
    Matched,
    /// Address updated (display text: "Updated").
    AdjustAddress,
    /// Address updated but size differs (display text: "Updated; size differs!").
    AdjustAddressAndSize,
}

/// One update record.
/// The target node is located by module/child index (see the module-level
/// design notes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateData {
    /// Index into `Project.children` (must point at a `ProjectChild::Module`).
    pub module_idx: usize,
    /// Index into `Module.children`.
    pub child_idx: usize,
    /// New address (`Matched` records keep the original address).
    pub address: u64,
    /// Size in bytes.
    pub size: u64,
    /// Bit mask (`u64::MAX` means unset).
    pub bit_mask: u64,
    /// Update type.
    pub typ: UpdateType,
}

impl UpdateData {
    /// Builds a `NotMatched` record (address/mask `u64::MAX`, size 0).
    pub fn not_matched(module_idx: usize, child_idx: usize) -> Self {
        UpdateData {
            module_idx,
            child_idx,
            address: u64::MAX,
            size: 0,
            bit_mask: u64::MAX,
            typ: UpdateType::NotMatched,
        }
    }
}

/// Marks the source of a symbol (the value identifies the symbol within its
/// source).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterTag {
    /// No source.
    None,
    /// DWARF symbol (index into the debug info's symbol values).
    Dwarf(usize),
    /// ELF symbol table symbol (symbol name).
    Elf(String),
    /// MAP file symbol (symbol name).
    Map(String),
}

/// Address/size information for one symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterSymbols {
    /// Address.
    pub address: u64,
    /// Bit offset.
    pub bit_offset: u32,
    /// Size in bits.
    pub size: u64,
    /// Flat array index (-1 for non-array members).
    pub array_idx: i64,
    /// Array dimensions.
    pub array_dim: Option<Vec<i32>>,
    /// Source tag.
    pub tag: UpdaterTag,
}

impl UpdaterSymbols {
    /// Builds from an ELF symbol table entry: the address is the symbol value
    /// and the size is the entry size times 8 bits.
    pub fn from_elf(name: &str, value: u64, size: u64) -> Self {
        UpdaterSymbols {
            address: value,
            bit_offset: 0,
            size: size * 8,
            array_idx: 0,
            array_dim: None,
            tag: UpdaterTag::Elf(name.to_string()),
        }
    }

    /// Builds from a MAP file symbol: the address is the symbol address and
    /// the size is 0.
    pub fn from_map(name: &str, address: u64) -> Self {
        UpdaterSymbols {
            address,
            bit_offset: 0,
            size: 0,
            array_idx: 0,
            array_dim: None,
            tag: UpdaterTag::Map(name.to_string()),
        }
    }

    /// Bit mask; `u64::MAX` when the size is a whole number of bytes.
    pub fn bit_mask(&self) -> u64 {
        if self.size.is_multiple_of(8) {
            return u64::MAX;
        }
        build_bitmask(self.size as i64, self.bit_offset as i32)
    }

    /// Size converted to bytes:
    /// `(bit_offset + size) / 8 - bit_offset / 8 + ((bit_offset + size) % 8 != 0 ? 1 : 0)`.
    pub fn byte_size(&self) -> u64 {
        let total = u64::from(self.bit_offset) + self.size;
        total / 8 - u64::from(self.bit_offset / 8) + u64::from(!total.is_multiple_of(8))
    }
}

/// Builds a bitmask of `size` ones shifted left by `bit_offset` bits;
/// `size >= 64` yields all ones.
pub fn build_bitmask(size: i64, bit_offset: i32) -> u64 {
    let mask = if size >= 64 {
        u64::MAX
    } else if size <= 0 {
        0
    } else {
        (1u64 << size) - 1
    };
    // The shift count is taken modulo 64, matching `wrapping_shl`.
    mask.wrapping_shl(bit_offset as u32)
}

/// Rounds `v` up to a multiple of `align`; returns `v` unchanged when
/// `align <= 1`.
pub(crate) fn align_up(v: i32, align: i32) -> i32 {
    if align <= 1 {
        v
    } else {
        // Division semantics for non-powers of two (agrees with the bitmask
        // form on powers of two).
        (v + align - 1) / align * align
    }
}

/// Rounds `v` down to a multiple of `align`; powers of two use the bitmask
/// `v & !(align - 1)` (negative values participate in two's complement).
pub fn align_down(v: i32, align: i32) -> i32 {
    if align <= 1 {
        v
    } else if align > 0 && (align & (align - 1)) == 0 {
        v & !(align - 1)
    } else {
        v / align * align
    }
}

/// Size of a `DataType` in bytes per ASAM MCD-2 MC. `Unsupported` yields
/// `None`.
pub fn data_type_size_in_bytes(dt: DataType) -> Option<i32> {
    Some(match dt {
        DataType::UByte | DataType::SByte => 1,
        DataType::UWord | DataType::SWord | DataType::Float16Ieee => 2,
        DataType::ULong | DataType::SLong | DataType::Float32Ieee => 4,
        DataType::AUInt64 | DataType::AInt64 | DataType::Float64Ieee => 8,
        DataType::Unsupported => return None,
    })
}

// ---------------------------------------------------------------------------
// A2L node access helpers (flattened views of addressable nodes)
// ---------------------------------------------------------------------------

/// Flattened read-only view of an addressable A2L node (MEASUREMENT /
/// CHARACTERISTIC / AXIS_PTS / BLOB / INSTANCE): the set of attributes used
/// by the update flow.
pub struct AddressNode<'a> {
    /// Node name.
    pub name: &'a str,
    /// Address (`None` when unset).
    pub address: Option<u32>,
    /// `SYMBOL_LINK` name.
    pub symbol_link: Option<&'a str>,
    /// `SYMBOL_LINK` offset.
    pub symbol_offset: i32,
    /// Current bit mask (`u64::MAX` when unset).
    pub bit_mask: u64,
    /// Memory size in bytes.
    pub memory_size: i64,
    /// Whether the node derives from a RECORD_LAYOUT reference
    /// (CHARACTERISTIC / AXIS_PTS).
    pub is_record_layout_ref: bool,
    /// Data type (MEASUREMENT only).
    pub data_type: Option<DataType>,
    /// Resolved record layout.
    pub record_layout: Option<&'a RecordLayout>,
    /// LINK_MAP from CANAPE_EXT (name + bit offset).
    pub link_map: Option<(String, u16)>,
}

/// Precomputed lookup data for resolving addressable nodes in one project.
///
/// Construct this once before traversing a project. In particular, this turns
/// repeated `RECORD_LAYOUT` resolution from a full-project scan per node into
/// an average O(1) hash lookup. Later definitions with the same name retain
/// the existing override behavior.
pub struct AddressNodeResolver<'a> {
    record_layouts: HashMap<&'a str, &'a RecordLayout>,
}

impl<'a> AddressNodeResolver<'a> {
    /// Builds the resolver's `RECORD_LAYOUT` index in one project traversal.
    pub fn new(project: &'a Project) -> Self {
        let record_layouts = project
            .modules()
            .flat_map(|module| &module.children)
            .filter_map(|child| match child {
                ModuleChild::RecordLayout(layout) => Some((layout.name.as_str(), layout)),
                _ => None,
            })
            .collect();
        Self { record_layouts }
    }

    /// Extracts an addressable-node view using the precomputed layout index.
    pub fn address_node(&self, child: &'a ModuleChild) -> Result<Option<AddressNode<'a>>> {
        let node = match child {
            ModuleChild::Measurement(n) => measurement_node(n)?,
            ModuleChild::Characteristic(n) => characteristic_node(
                n,
                self.record_layouts
                    .get(n.rec.record_layout.as_str())
                    .copied(),
            )?,
            ModuleChild::AxisPts(n) => axis_pts_node(
                n,
                self.record_layouts
                    .get(n.rec.record_layout.as_str())
                    .copied(),
            )?,
            ModuleChild::Blob(n) => AddressNode {
                name: &n.named.name,
                address: n.addr.address,
                symbol_link: n.addr.symbol_link.as_deref(),
                symbol_offset: n.addr.symbol_offset,
                bit_mask: u64::MAX,
                memory_size: i64::from(n.size),
                is_record_layout_ref: false,
                data_type: None,
                record_layout: None,
                link_map: link_map_in(n.children.iter().filter_map(|child| match child {
                    autors_a2l::model::measurement::BlobChild::Unsupported(node) => Some(node),
                    _ => None,
                })),
            },
            ModuleChild::Instance(n) => AddressNode {
                name: &n.named.name,
                address: n.addr.address,
                symbol_link: n.addr.symbol_link.as_deref(),
                symbol_offset: n.addr.symbol_offset,
                bit_mask: u64::MAX,
                memory_size: 0,
                is_record_layout_ref: false,
                data_type: None,
                record_layout: None,
                link_map: link_map_in(n.children.iter().filter_map(|child| match child {
                    autors_a2l::model::measurement::InstanceChild::Unsupported(node) => Some(node),
                    _ => None,
                })),
            },
            _ => return Ok(None),
        };
        Ok(Some(node))
    }
}

impl AddressNode<'_> {
    /// Symbol name and offset: prefers `SYMBOL_LINK` (with its offset), then
    /// the CANAPE_EXT LINK_MAP (offset `BitOffset / 8`), otherwise the node
    /// name (offset 0).
    pub fn symbol_name(&self) -> (String, i32) {
        if let Some(sl) = self.symbol_link.filter(|s| !s.is_empty()) {
            (sl.to_string(), self.symbol_offset)
        } else if let Some((name, bit_offset)) = &self.link_map {
            if !name.is_empty() {
                return (name.clone(), i32::from(*bit_offset) / 8);
            }
            (self.name.to_string(), 0)
        } else {
            (self.name.to_string(), 0)
        }
    }
}

/// Resolves a RECORD_LAYOUT by name across the whole project (first match
/// scanning from the back, so a later same-name definition overrides an
/// earlier one).
pub(crate) fn find_record_layout<'a>(project: &'a Project, name: &str) -> Option<&'a RecordLayout> {
    project.children.iter().rev().find_map(|child| match child {
        ProjectChild::Module(module) => {
            module.children.iter().rev().find_map(|child| match child {
                ModuleChild::RecordLayout(layout) if layout.name == name => Some(layout),
                _ => None,
            })
        }
        _ => None,
    })
}

/// Extracts the addressable-node view from a module child; returns `None` for
/// non-addressable nodes and for nodes whose RECORD_LAYOUT reference cannot be
/// resolved (those are skipped by the update flow).
pub fn address_node<'a>(
    project: &'a Project,
    child: &'a ModuleChild,
) -> Result<Option<AddressNode<'a>>> {
    let node = match child {
        ModuleChild::Measurement(n) => measurement_node(n)?,
        ModuleChild::Characteristic(n) => {
            characteristic_node(n, find_record_layout(project, &n.rec.record_layout))?
        }
        ModuleChild::AxisPts(n) => {
            axis_pts_node(n, find_record_layout(project, &n.rec.record_layout))?
        }
        ModuleChild::Blob(n) => AddressNode {
            name: &n.named.name,
            address: n.addr.address,
            symbol_link: n.addr.symbol_link.as_deref(),
            symbol_offset: n.addr.symbol_offset,
            bit_mask: u64::MAX,
            // BLOB memory size is its size field.
            memory_size: i64::from(n.size),
            is_record_layout_ref: false,
            data_type: None,
            record_layout: None,
            link_map: link_map_in(n.children.iter().filter_map(|child| match child {
                autors_a2l::model::measurement::BlobChild::Unsupported(node) => Some(node),
                _ => None,
            })),
        },
        ModuleChild::Instance(n) => AddressNode {
            name: &n.named.name,
            address: n.addr.address,
            symbol_link: n.addr.symbol_link.as_deref(),
            symbol_offset: n.addr.symbol_offset,
            bit_mask: u64::MAX,
            // INSTANCE memory size is 0.
            memory_size: 0,
            is_record_layout_ref: false,
            data_type: None,
            record_layout: None,
            link_map: link_map_in(n.children.iter().filter_map(|child| match child {
                autors_a2l::model::measurement::InstanceChild::Unsupported(node) => Some(node),
                _ => None,
            })),
        },
        _ => return Ok(None),
    };
    Ok(Some(node))
}

fn measurement_node<'a>(n: &'a Measurement) -> Result<AddressNode<'a>> {
    // Memory size = data type size in bytes * array size.
    let elem = data_type_size_in_bytes(n.data_type).unwrap_or(0);
    let array_size: i64 = n
        .matrix_dim
        .as_ref()
        .map(|d| d.iter().map(|&v| i64::from(v)).product())
        .unwrap_or(1);
    Ok(AddressNode {
        name: &n.named.name,
        address: n.addr.address,
        symbol_link: n.addr.symbol_link.as_deref(),
        symbol_offset: n.addr.symbol_offset,
        bit_mask: n.bit_mask.unwrap_or(u64::MAX),
        memory_size: i64::from(elem) * array_size,
        is_record_layout_ref: false,
        data_type: Some(n.data_type),
        record_layout: None,
        link_map: link_map_in(n.children.iter().filter_map(|child| match child {
            autors_a2l::model::measurement::MeasurementChild::Unsupported(node) => Some(node),
            _ => None,
        })),
    })
}

fn characteristic_node<'a>(
    n: &'a Characteristic,
    rl: Option<&'a RecordLayout>,
) -> Result<AddressNode<'a>> {
    // Memory size is the record layout size.
    let memory_size = match rl {
        Some(rl) => {
            let axis: Vec<&AxisDescr> = n
                .children
                .iter()
                .filter_map(|c| match c {
                    autors_a2l::model::characteristic::CharacteristicChild::AxisDescr(a) => Some(a),
                    _ => None,
                })
                .collect();
            let mut size = record_layout_size(i32::MAX, rl, Some(&axis), 0)?;
            // ASCII / VAL_BLK additionally multiply by the number of elements.
            if n.char_type == CharacteristicType::ASCII
                || n.char_type == CharacteristicType::VAL_BLK
            {
                size *= number_of_elements(n);
            }
            size
        }
        None => 0,
    };
    Ok(AddressNode {
        name: &n.named.name,
        address: n.addr.address,
        symbol_link: n.addr.symbol_link.as_deref(),
        symbol_offset: n.addr.symbol_offset,
        bit_mask: n.bitmask.unwrap_or(u64::MAX),
        memory_size,
        is_record_layout_ref: true,
        data_type: None,
        record_layout: rl,
        link_map: link_map_in(n.children.iter().filter_map(|child| match child {
            autors_a2l::model::characteristic::CharacteristicChild::Unsupported(node) => Some(node),
            _ => None,
        })),
    })
}

fn axis_pts_node<'a>(n: &'a AxisPts, rl: Option<&'a RecordLayout>) -> Result<AddressNode<'a>> {
    // Memory size = layout size, with `max_axis_points` as the default axis
    // point count.
    let memory_size = match rl {
        Some(rl) => record_layout_size(i32::MAX, rl, None, n.max_axis_points)?,
        None => 0,
    };
    Ok(AddressNode {
        name: &n.named.name,
        address: n.addr.address,
        symbol_link: n.addr.symbol_link.as_deref(),
        symbol_offset: n.addr.symbol_offset,
        bit_mask: u64::MAX,
        memory_size,
        is_record_layout_ref: true,
        data_type: None,
        record_layout: rl,
        link_map: link_map_in(n.children.iter().filter_map(|child| match child {
            autors_a2l::model::characteristic::AxisPtsChild::Unsupported(node) => Some(node),
            _ => None,
        })),
    })
}

/// Number of elements of a CHARACTERISTIC (only used on the ASCII / VAL_BLK
/// path).
fn number_of_elements(n: &Characteristic) -> i64 {
    match n.char_type {
        CharacteristicType::VALUE => 1,
        CharacteristicType::ASCII | CharacteristicType::VAL_BLK => match &n.matrix_dim {
            Some(dims) => dims.iter().map(|&v| i64::from(v)).product(),
            None => i64::from(n.number),
        },
        // Other types are unsupported, but this path is only reached for
        // ASCII/VAL_BLK.
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// Record layout size computation
// ---------------------------------------------------------------------------

/// Layout entry view (same as the private `LayoutEntry` in autors-a2l
/// record_layout.rs; this file keeps its own local copy).
enum LayoutEntry<'a> {
    NoAxisPts(&'a autors_a2l::model::record_layout::NoAxisPtsLayoutDesc),
    AxisPts(&'a autors_a2l::model::record_layout::AxisPtsLayoutDesc),
    AxisRescale(&'a autors_a2l::model::record_layout::AxisRescaleLayoutDesc),
    FncValues(&'a autors_a2l::model::record_layout::FncValuesLayoutDesc),
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
}

/// Collects layout entries and stably sorts them by position (same as in
/// record_layout.rs).
fn layout_entries(rl: &RecordLayout) -> Vec<LayoutEntry<'_>> {
    let mut entries = Vec::new();
    for e in rl.no_axis_pts.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in rl.axis_pts.iter().flatten() {
        entries.push(LayoutEntry::AxisPts(e));
    }
    for e in rl.src_address.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in rl.offset.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in rl.dist_op.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in rl.rip_addr.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in rl.shift_op.iter().flatten() {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    for e in &rl.reserved {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    if let Some(e) = &rl.axis_rescale_x {
        entries.push(LayoutEntry::AxisRescale(e));
    }
    if let Some(e) = &rl.no_rescale_x {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    if let Some(e) = &rl.identification {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    if let Some(e) = &rl.rip_addr_w {
        entries.push(LayoutEntry::NoAxisPts(e));
    }
    if let Some(e) = &rl.fnc_values {
        entries.push(LayoutEntry::FncValues(e));
    }
    entries.sort_by_key(|e| e.position());
    entries
}

/// Alignment of a data type within a record layout.
fn alignment_of(rl: &RecordLayout, dt: DataType) -> Result<i32> {
    let a = rl.alignments.unwrap_or(DEFAULT_ALIGNMENTS);
    Ok(match dt {
        DataType::UByte | DataType::SByte => a[0],
        DataType::UWord | DataType::SWord => a[1],
        DataType::ULong | DataType::SLong => a[2],
        DataType::Float32Ieee => a[3],
        DataType::Float64Ieee => a[4],
        DataType::AUInt64 | DataType::AInt64 => a[5],
        DataType::Float16Ieee => a[6],
        // Unsupported data types have no alignment.
        DataType::Unsupported => {
            return Err(Error::Update(format!(
                "RECORD_LAYOUT {:?}: no alignment for unsupported data type",
                rl.name
            )))
        }
    })
}

/// Placeholder size for pointer-style addressing (PBYTE/PWORD/PLONG/PLONGLONG).
fn ptr_size(num: i32, rl: &RecordLayout, addr_type: AddrType) -> Result<i32> {
    Ok(match addr_type {
        AddrType::DIRECT => num, // DIRECT is handled by the caller
        AddrType::PBYTE => num + 1,
        AddrType::PWORD => align_up(num, alignment_of(rl, DataType::UWord)?) + 2,
        AddrType::PLONG => align_up(num, alignment_of(rl, DataType::ULong)?) + 4,
        AddrType::PLONGLONG => align_up(num, alignment_of(rl, DataType::AUInt64)?) + 8,
    })
}

/// Memory size of a record layout, starting from `position` and using
/// `axis_descrs` (or `default_max_axis_points` when no axis descriptors are
/// given) for axis point counts.
pub(crate) fn record_layout_size(
    position: i32,
    rl: &RecordLayout,
    axis_descrs: Option<&[&AxisDescr]>,
    default_max_axis_points: i32,
) -> Result<i64> {
    let mut num: i32 = 0;
    for entry in layout_entries(rl) {
        if position <= entry.position() {
            continue;
        }
        match entry {
            LayoutEntry::AxisRescale(e) => {
                let size = data_type_size_in_bytes(e.data_type).unwrap_or(0);
                num = if e.address_type == AddrType::DIRECT {
                    align_up(num, alignment_of(rl, e.data_type)?)
                        + size * 2 * e.max_no_rescale_pairs
                } else {
                    ptr_size(num, rl, e.address_type)?
                };
            }
            LayoutEntry::AxisPts(e) => {
                let size = data_type_size_in_bytes(e.data_type).unwrap_or(0);
                num = if e.address_type == AddrType::DIRECT {
                    let pts = axis_descrs
                        .and_then(|d| d.get(e.axis_idx.max(0) as usize))
                        .map(|d| d.max_axis_points)
                        .unwrap_or(default_max_axis_points);
                    align_up(num, alignment_of(rl, e.data_type)?) + size * pts
                } else {
                    ptr_size(num, rl, e.address_type)?
                };
            }
            LayoutEntry::NoAxisPts(e) => {
                let size = data_type_size_in_bytes(e.data_type).unwrap_or(0);
                num = align_up(num, alignment_of(rl, e.data_type)?) + size;
            }
            LayoutEntry::FncValues(e) => {
                num = if e.address_type == AddrType::DIRECT {
                    let mut n = data_type_size_in_bytes(e.data_type).unwrap_or(0);
                    let mut aligned = align_up(num, alignment_of(rl, e.data_type)?);
                    if let Some(descrs) = axis_descrs {
                        // Walk axis descriptors in reverse; the index is the
                        // axis number. The matching axis count doubles for
                        // ALTERNATE_WITH_X/Y.
                        for (idx, d) in descrs.iter().enumerate().rev() {
                            let mut pts = d.max_axis_points;
                            if (e.index_mode == IndexMode::ALTERNATE_WITH_X && idx == 0)
                                || (e.index_mode == IndexMode::ALTERNATE_WITH_Y && idx == 1)
                            {
                                pts *= 2;
                            }
                            n *= pts;
                        }
                    }
                    aligned += n;
                    aligned
                } else {
                    ptr_size(num, rl, e.address_type)?
                };
            }
        }
    }
    Ok(i64::from(num))
}

// ---------------------------------------------------------------------------
// CANAPE_EXT LINK_MAP helpers (access via pass-through child blocks)
// ---------------------------------------------------------------------------

/// Parses a `0x` hexadecimal or decimal integer token (same convention as the
/// A2L integer parsing in autors-a2l).
fn parse_int_token(text: &str) -> Option<u64> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        t.parse::<u64>().ok()
    }
}

/// Extracts the CANAPE_EXT LINK_MAP (name + bit offset) from a pass-through
/// IF_DATA child block.
/// IF_DATA blocks are passed through as `UnsupportedNode`, so this does a
/// lightweight token scan.
fn link_map_of(u: &UnsupportedNode) -> Option<(String, u16)> {
    if !u.keyword.eq_ignore_ascii_case("IF_DATA") {
        return None;
    }
    let mut toks = u.items.iter().filter_map(|i| match i {
        Item::Param(t) => Some(t),
        _ => None,
    });
    if !toks.next()?.text.eq_ignore_ascii_case("CANAPE_EXT") {
        return None;
    }
    toks.find(|t| t.text.eq_ignore_ascii_case("LINK_MAP"))?;
    // 8 values follow LINK_MAP: name base_address base_ext is_rel offset_seg is_dt_valid dt_enum bit_offset
    let name = toks.next()?.text.clone();
    let bit_offset = parse_int_token(&toks.nth(6)?.text)? as u16;
    Some((name, bit_offset))
}

/// Finds the first CANAPE_EXT LINK_MAP among the child blocks (non-recursive
/// lookup).
fn link_map_in<'a>(children: impl Iterator<Item = &'a UnsupportedNode>) -> Option<(String, u16)> {
    children.filter_map(link_map_of).next()
}

/// Rewrites the base address token of the LINK_MAP inside a pass-through
/// IF_DATA/CANAPE_EXT child block to the new address (written out as uppercase
/// `0x` hex).
fn update_link_map_base(u: &mut UnsupportedNode, address: u32) -> bool {
    if !u.keyword.eq_ignore_ascii_case("IF_DATA") {
        return false;
    }
    let mut toks = u.items.iter_mut().filter_map(|i| match i {
        Item::Param(t) => Some(t),
        _ => None,
    });
    let Some(first) = toks.next() else {
        return false;
    };
    if !first.text.eq_ignore_ascii_case("CANAPE_EXT") {
        return false;
    }
    if toks
        .find(|t| t.text.eq_ignore_ascii_case("LINK_MAP"))
        .is_none()
    {
        return false;
    }
    // Skip the name and rewrite the following base-address token.
    if toks.next().is_none() {
        return false;
    }
    if let Some(base_address) = toks.next() {
        base_address.text = format!("0x{address:X}");
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// update_and_write_a2l
// ---------------------------------------------------------------------------

/// Applies update records and writes the A2L back to a file.
/// Returns the number of updated nodes. The target file's parent directory is
/// created if it does not exist.
pub fn update_and_write_a2l(
    update_records: &[UpdateData],
    target_file: impl AsRef<Path>,
    project: &mut Project,
    ignore_size_changes: bool,
    preserve_bm: bool,
    delete_not_matched_from_model: bool,
) -> Result<u32> {
    let target_file = target_file.as_ref();
    if let Some(dir) = target_file.parent() {
        if !dir.as_os_str().is_empty() && !dir.exists() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mut updated = 0u32;
    let mut deletions: Vec<(usize, usize)> = Vec::new();
    for rec in update_records {
        if delete_not_matched_from_model && rec.typ == UpdateType::NotMatched {
            deletions.push((rec.module_idx, rec.child_idx));
            continue;
        }
        // Only AdjustAddress is applied, plus AdjustAddressAndSize when
        // ignore_size_changes is set.
        if rec.typ != UpdateType::AdjustAddress
            && !(rec.typ == UpdateType::AdjustAddressAndSize && ignore_size_changes)
        {
            continue;
        }
        apply_update(project, rec, preserve_bm)?;
        updated += 1;
    }
    // Deletions run after the updates, in reverse index order, to avoid index
    // shifts (see module docs).
    deletions.sort_unstable_by(|a, b| b.cmp(a));
    deletions.dedup();
    for (mi, ci) in deletions {
        if let Some(ProjectChild::Module(m)) = project.children.get_mut(mi) {
            if ci < m.children.len() {
                m.children.remove(ci);
            }
        }
    }
    project.save(target_file)?;
    Ok(updated)
}

/// Applies a single update record: sets the address, optionally updates
/// BIT_MASK, and syncs the CANAPE_EXT LINK_MAP.
fn apply_update(project: &mut Project, rec: &UpdateData, preserve_bm: bool) -> Result<()> {
    let module = match project.children.get_mut(rec.module_idx) {
        Some(ProjectChild::Module(m)) => m,
        _ => {
            return Err(Error::Update(format!(
                "invalid module index {} in update record",
                rec.module_idx
            )))
        }
    };
    let child = module.children.get_mut(rec.child_idx).ok_or_else(|| {
        Error::Update(format!(
            "invalid child index {} in module {} in update record",
            rec.child_idx, module.name
        ))
    })?;
    // The address is truncated to 32 bits.
    let addr32 = rec.address as u32;
    let new_mask = (rec.bit_mask != u64::MAX).then_some(rec.bit_mask);
    match child {
        ModuleChild::Measurement(n) => {
            n.addr.address = Some(addr32);
            // The mask is written only when !preserve_bm and it differs
            // (`u64::MAX` clears it).
            if !preserve_bm && n.bit_mask.unwrap_or(u64::MAX) != rec.bit_mask {
                n.bit_mask = new_mask;
            }
            for c in &mut n.children {
                if let autors_a2l::model::measurement::MeasurementChild::Unsupported(u) = c {
                    update_link_map_base(u, addr32);
                }
            }
        }
        ModuleChild::Characteristic(n) => {
            n.addr.address = Some(addr32);
            if !preserve_bm && n.bitmask.unwrap_or(u64::MAX) != rec.bit_mask {
                n.bitmask = new_mask;
            }
            for c in &mut n.children {
                if let autors_a2l::model::characteristic::CharacteristicChild::Unsupported(u) = c {
                    update_link_map_base(u, addr32);
                }
            }
        }
        ModuleChild::AxisPts(n) => {
            n.addr.address = Some(addr32);
            for c in &mut n.children {
                if let autors_a2l::model::characteristic::AxisPtsChild::Unsupported(u) = c {
                    update_link_map_base(u, addr32);
                }
            }
        }
        ModuleChild::Blob(n) => {
            n.addr.address = Some(addr32);
            for c in &mut n.children {
                if let autors_a2l::model::measurement::BlobChild::Unsupported(u) = c {
                    update_link_map_base(u, addr32);
                }
            }
        }
        ModuleChild::Instance(n) => {
            n.addr.address = Some(addr32);
            for c in &mut n.children {
                if let autors_a2l::model::measurement::InstanceChild::Unsupported(u) = c {
                    update_link_map_base(u, addr32);
                }
            }
        }
        // Records always point at addressable nodes; other child types are
        // unreachable.
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Traversal helper shared by the ELF and MAP parser crates
// ---------------------------------------------------------------------------

/// Iterates over all MODULEs in the project, yielding their `Project.children`
/// index.
pub fn modules_with_index(project: &Project) -> impl Iterator<Item = (usize, &Module)> {
    project
        .children
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match c {
            ProjectChild::Module(m) => Some((i, m)),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_path_simple_member() {
        let path = parse_symbol_path("myStruct.member").unwrap();
        assert_eq!(
            path,
            vec![
                SymbolPathElem {
                    name: "myStruct".into(),
                    indexes: None
                },
                SymbolPathElem {
                    name: "member".into(),
                    indexes: None
                },
            ]
        );
    }

    #[test]
    fn parse_path_array_brackets() {
        let path = parse_symbol_path("arr[2][10]").unwrap();
        assert_eq!(
            path,
            vec![SymbolPathElem {
                name: "arr".into(),
                indexes: Some(vec![2, 10]),
            }]
        );
    }

    #[test]
    fn parse_path_member_with_array() {
        let path = parse_symbol_path("s.arr[1]").unwrap();
        assert_eq!(
            path,
            vec![
                SymbolPathElem {
                    name: "s".into(),
                    indexes: None
                },
                SymbolPathElem {
                    name: "arr".into(),
                    indexes: Some(vec![1])
                },
            ]
        );
    }

    #[test]
    fn parse_path_dotted_index_not_split() {
        // ".2" leaves a one-character index buffer (length not greater than
        // 2), which stays in the segment name.
        let path = parse_symbol_path("var.member.2").unwrap();
        assert_eq!(
            path,
            vec![
                SymbolPathElem {
                    name: "var".into(),
                    indexes: None
                },
                SymbolPathElem {
                    name: "member.2".into(),
                    indexes: None
                },
            ]
        );
    }

    #[test]
    fn parse_path_index_edge_cases() {
        // Letters leave the index buffer: "arr[ab]" parses to a single
        // segment name (no error).
        let path = parse_symbol_path("arr[ab]").unwrap();
        assert_eq!(
            path,
            vec![SymbolPathElem {
                name: "arr[ab]".into(),
                indexes: None
            }]
        );
        // An index buffer wrapped in separators but containing a non-numeric
        // part is a parse error.
        assert!(parse_symbol_path("arr[1$]").is_err());
    }

    #[test]
    fn updater_symbols_bit_mask_and_byte_size() {
        // Whole bytes: mask is MAX, byte count is a plain division.
        let u = UpdaterSymbols {
            address: 0,
            bit_offset: 0,
            size: 32,
            array_idx: -1,
            array_dim: None,
            tag: UpdaterTag::None,
        };
        assert_eq!(u.bit_mask(), u64::MAX);
        assert_eq!(u.byte_size(), 4);
        // Bitfield: 3 bits at offset 2 -> mask 0b11100, occupying 1 byte.
        let u = UpdaterSymbols {
            size: 3,
            bit_offset: 2,
            ..u
        };
        assert_eq!(u.bit_mask(), 0b11100);
        assert_eq!(u.byte_size(), 1);
        // Cross-byte bitfield: 9 bits at offset 7 -> 2 bytes.
        let u = UpdaterSymbols {
            size: 9,
            bit_offset: 7,
            ..u
        };
        assert_eq!(u.byte_size(), 2);
    }

    #[test]
    fn indexed_address_resolution_matches_reverse_scan_override() {
        let src = r#"/begin PROJECT P "d"
/begin MODULE FIRST "first"
  /begin RECORD_LAYOUT DUP FNC_VALUES 1 UBYTE ROW_DIR DIRECT /end RECORD_LAYOUT
/end MODULE
/begin MODULE LAST "last"
  /begin RECORD_LAYOUT DUP FNC_VALUES 1 ULONG ROW_DIR DIRECT /end RECORD_LAYOUT
  /begin CHARACTERISTIC C "d" VALUE 0x1000 DUP 0 NO_COMPU_METHOD 0 1
  /end CHARACTERISTIC
/end MODULE
/end PROJECT
"#;
        let project = Project::parse_str(src).unwrap();
        let child = &project.modules().nth(1).unwrap().children[1];
        let scanned = address_node(&project, child).unwrap().unwrap();
        let resolver = AddressNodeResolver::new(&project);
        let indexed = resolver.address_node(child).unwrap().unwrap();

        assert_eq!(scanned.record_layout.unwrap().name, "DUP");
        assert!(std::ptr::eq(
            scanned.record_layout.unwrap(),
            indexed.record_layout.unwrap()
        ));
        assert_eq!(scanned.memory_size, 4);
        assert_eq!(indexed.memory_size, scanned.memory_size);
    }

    #[test]
    fn align_helpers() {
        assert_eq!(align_up(5, 4), 8);
        assert_eq!(align_up(4, 4), 4);
        assert_eq!(align_up(7, 0), 7);
        assert_eq!(align_down(5, 4), 4);
        assert_eq!(align_down(-1, 4), -4); // bitmask semantics
    }

    const A2L: &str = r#"/begin PROJECT P "d"
/begin MODULE M "m"
  /begin RECORD_LAYOUT RL1 FNC_VALUES 1 UWORD ROW_DIR DIRECT /end RECORD_LAYOUT
  /begin MEASUREMENT Meas1 "d" UWORD Conv 1 0 0 100 ECU_ADDRESS 0x1000
    SYMBOL_LINK "sym1" 0
  /end MEASUREMENT
  /begin MEASUREMENT MeasGone "d" UWORD Conv 1 0 0 100 ECU_ADDRESS 0x2000
  /end MEASUREMENT
  /begin CHARACTERISTIC Char1 "d" VALUE 0x3000 RL1 0 Conv 0 100
  /end CHARACTERISTIC
/end MODULE
/end PROJECT
"#;

    fn module_child_index(p: &Project, name: &str) -> (usize, usize) {
        for (mi, m) in modules_with_index(p) {
            for (ci, c) in m.children.iter().enumerate() {
                let n = match c {
                    ModuleChild::Measurement(n) => Some(&n.named.name),
                    ModuleChild::Characteristic(n) => Some(&n.named.name),
                    _ => None,
                };
                if n == Some(&name.to_string()) {
                    return (mi, ci);
                }
            }
        }
        panic!("node {name} not found");
    }

    #[test]
    fn update_and_write_applies_and_deletes() {
        let mut project = Project::parse_str(A2L).unwrap();
        let (mi1, ci1) = module_child_index(&project, "Meas1");
        let (mi2, ci2) = module_child_index(&project, "MeasGone");
        let records = vec![
            UpdateData {
                module_idx: mi1,
                child_idx: ci1,
                address: 0x4000,
                size: 2,
                bit_mask: 0xFF,
                typ: UpdateType::AdjustAddress,
            },
            UpdateData {
                module_idx: mi2,
                child_idx: ci2,
                ..UpdateData::not_matched(mi2, ci2)
            },
        ];
        let dir = std::env::temp_dir().join("autors_symbols_test_update");
        let target = dir.join("out.a2l");
        let n = update_and_write_a2l(&records, &target, &mut project, false, false, true).unwrap();
        assert_eq!(n, 1);
        // Meas1 address and mask updated.
        let m = match &project.children[mi1] {
            ProjectChild::Module(m) => m,
            _ => panic!(),
        };
        let meas = match &m.children[ci1] {
            ModuleChild::Measurement(n) => n,
            _ => panic!(),
        };
        assert_eq!(meas.addr.address, Some(0x4000));
        assert_eq!(meas.bit_mask, Some(0xFF));
        // MeasGone was deleted.
        assert!(!m.children.iter().any(|c| matches!(
            c,
            ModuleChild::Measurement(n) if n.named.name == "MeasGone"
        )));
        // The written file exists and can be re-parsed.
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("ECU_ADDRESS 0x4000"));
        assert!(text.contains("BIT_MASK 0xFF"));
        assert!(!text.contains("MeasGone"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_ignores_size_changes_flag() {
        let mut project = Project::parse_str(A2L).unwrap();
        let (mi1, ci1) = module_child_index(&project, "Meas1");
        let rec = UpdateData {
            module_idx: mi1,
            child_idx: ci1,
            address: 0x5000,
            size: 4,
            bit_mask: u64::MAX,
            typ: UpdateType::AdjustAddressAndSize,
        };
        let dir = std::env::temp_dir().join("autors_symbols_test_update2");
        let target = dir.join("out.a2l");
        // ignore_size_changes = false -> AdjustAddressAndSize is not applied.
        let n = update_and_write_a2l(
            std::slice::from_ref(&rec),
            &target,
            &mut project,
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(n, 0);
        // ignore_size_changes = true -> the address is applied.
        let n = update_and_write_a2l(&[rec], &target, &mut project, true, true, false).unwrap();
        assert_eq!(n, 1);
        let m = match &project.children[mi1] {
            ProjectChild::Module(m) => m,
            _ => panic!(),
        };
        let meas = match &m.children[ci1] {
            ModuleChild::Measurement(n) => n,
            _ => panic!(),
        };
        assert_eq!(meas.addr.address, Some(0x5000));
        assert_eq!(meas.bit_mask, None); // preserve_bm = true
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn link_map_base_address_updated() {
        let src = r#"/begin PROJECT P "d"
/begin MODULE M "m"
  /begin MEASUREMENT Meas1 "d" UWORD Conv 1 0 0 100 ECU_ADDRESS 0x1000
    /begin IF_DATA CANAPE_EXT 100 LINK_MAP "mapSym" 0x1000 0 0 0 1 2 3 /end IF_DATA
  /end MEASUREMENT
/end MODULE
/end PROJECT
"#;
        let mut project = Project::parse_str(src).unwrap();
        let (mi, ci) = module_child_index(&project, "Meas1");
        // symbol_name should pick up the LINK_MAP name.
        let m = match &project.children[mi] {
            ProjectChild::Module(m) => m,
            _ => panic!(),
        };
        let node = address_node(&project, &m.children[ci]).unwrap().unwrap();
        let (name, off) = node.symbol_name();
        assert_eq!(name, "mapSym");
        assert_eq!(off, 0); // bit_offset = 3 -> 3/8 = 0
        let rec = UpdateData {
            module_idx: mi,
            child_idx: ci,
            address: 0x9000,
            size: 2,
            bit_mask: u64::MAX,
            typ: UpdateType::AdjustAddress,
        };
        let dir = std::env::temp_dir().join("autors_symbols_test_update3");
        let target = dir.join("out.a2l");
        update_and_write_a2l(&[rec], &target, &mut project, false, false, false).unwrap();
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("LINK_MAP \"mapSym\" 0x9000"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
