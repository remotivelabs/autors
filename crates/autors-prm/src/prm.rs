//! Parsing of INCA ProF-style PRM/CNF flash scripts.
//! The file format is a subset of the ETAS INCA ProF scripting format: a
//! `.prm` main file with `#define` variables, command sections
//! (`[NAME] .. [NAME_END]`) and `procedure` sub-flows (optionally pulled in
//! from `.pri` files via `#include`), plus a `.cnf` controller configuration
//! named by the script's `CONFIG` variable. Parsing entry points are
//! [`PrmFile::open`] / [`PrmFile::parse_str`] (which also load the CNF and
//! detect the protocol [`Mode`]) and [`CnfFile::open`] / [`CnfFile::parse_str`].
//! `SOURCE_MEM_AREA` segment payloads are loaded from a data file via
//! [`CnfFile::load_segment_data`] / [`CnfFile::load_segment_data_from_path`].
//! Out of scope:
//! - runtime script compilation / code generation (a host-runtime-specific
//!   facility); scripts are interpreted instead — see [`crate::executor`];
//! - Win32 Seed&Key DLL loading: [`crate::executor`] injects a
//!   `SeedKeyProvider` factory instead (platform-neutral).

use std::path::{Path, PathBuf};

use autors_datafile::datafile::{DataFile, MemorySegmentList};
use indexmap::IndexMap;

use crate::error::{Error, Result};
use crate::prm_if::prm_error;

// ============================================================================
// Format constants
// ============================================================================

/// The script commands recognized by the parser (45 uppercase names, matched
/// case-sensitively).
const KNOWN_METHODS: &[&str] = &[
    "CALL",
    "SET_RE_ENTRY",
    "WAIT",
    "DISPLAY_MESSAGE",
    "DISPLAY_ERROR_MESSAGE",
    "DEFAULT_SCREEN_LAYOUT",
    "EXTENDED_MESSAGE",
    "SET_DEBUG_LEVEL",
    "SET_VARIABLE",
    "GET_VARIABLE",
    "SHOW_PROGRAMMING_INFO",
    "RUN_DLL",
    "INIT_FLASH_PROGRAMMING",
    "CAN_SEND_MESSAGE",
    "UDSB_INIT_COMMUNICATION",
    "UDSB_MSG_RET_GET_AT",
    "UDS_COMMUNICATION_CONTROL",
    "UDS_DIAGNOSTIC_SESSION_CONTROL",
    "UDS_CONTROL_DTC_SETTING",
    "UDS_READ_DATA_BY_IDENTIFIER",
    "UDSX_READ_DATA_BY_IDENTIFIER_SCALING",
    "UDS_READ_DATA_BY_IDENTIFIER_GET_DATA_REC_AT",
    "UDS_WRITE_DATA_BY_IDENTIFIER",
    "UDS_CLEAR_DTC_INFORMATION",
    "UDS_ROUTINE_CONTROL",
    "UDS_PASS_THROUGH",
    "UDS_ECU_RESET",
    "UDSX_SECURITY_ACCESS",
    "UDSX_VERIFY_MEMORY",
    "UDSX_PROGRAM_MEMORY",
    "CHECK_INCA_CONFIGURATION",
    "CCP_DISCONNECT",
    "CCPB_STORE_CCP_CMD_TIMEOUT",
    "CCPB_SET_CANIDS",
    "CCPX_START_ECU_COMMUNICATION",
    "CCPX_DIAG_SERVICE",
    "CCPX_ACTION_SERVICE",
    "CCPX_ERASE_MEMORY",
    "CCPX_PROGRAM_MEMORY",
    "XCP_CONNECT",
    "XCP_PROGRAM_START",
    "XCP_SET_MTA",
    "XCPX_PROGRAM_CLEAR",
    "XCPX_PROGRAM_MEMORY",
    "XCP_PROGRAM_RESET",
];

const CMDSET_END_SUFFIX: &str = "_END";
const CNF_KEY: &str = "CONFIG";
const ERR_MISSING_CNF: &str = "CNF file not specified";
const ERR_MISSING_MODE: &str =
    "PRM script doesn't rely on UDS nor CCP protocol.\nExecution is currently not supported.";

// ============================================================================
// ============================================================================

fn try_parse_int_val(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else {
        t.parse::<i64>().ok()
    }
}

fn parse_prm_int(s: &str) -> Option<i64> {
    if s.is_empty() || s.starts_with('"') {
        return None;
    }
    let owned;
    let mut t = s;
    if let Some(rest) = t.strip_prefix('$') {
        owned = format!("0x{rest}");
        t = &owned;
    }
    let t = t.strip_suffix('L').unwrap_or(t);
    try_parse_int_val(t)
}

fn parse_cnf_int(s: &str) -> Option<i64> {
    let t = s.strip_suffix('L').unwrap_or(s);
    try_parse_int_val(t)
}

// ============================================================================
// ============================================================================

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn find_after<'a>(s: &'a str, lit: &str) -> Option<&'a str> {
    s.find(lit).map(|i| &s[i + lit.len()..])
}

fn take_word(s: &str) -> (&str, &str) {
    let end = s.find(|c| !is_word(c)).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

fn skip_whitespace(s: &str) -> &str {
    s.trim_start()
}

fn match_cnf_line(line: &str) -> Option<(String, String)> {
    let bytes: Vec<(usize, char)> = line.char_indices().collect();
    for (pos, (idx, ch)) in bytes.iter().enumerate() {
        if !is_word(*ch) {
            continue;
        }
        let (word, rest) = take_word(&line[*idx..]);
        if let Some(after) = rest.strip_prefix(':') {
            let value: String = after.chars().take_while(|&c| c != ';').collect();
            if !value.is_empty() {
                return Some((word.to_owned(), value));
            }
        }
        let _ = pos;
    }
    None
}

/// `#define\s+([\w]+)\s+([^;\s]+)`.
fn match_define(line: &str) -> Option<(String, String)> {
    let rest = find_after(line, "#define")?;
    let rest = skip_whitespace(rest);
    if rest.len() == line.len() || rest.is_empty() {
        return None;
    }
    let after_lit = &line[line.find("#define")? + "#define".len()..];
    if !after_lit.chars().next()?.is_whitespace() {
        return None;
    }
    let (key, rest) = take_word(rest);
    if key.is_empty() {
        return None;
    }
    let rest = skip_whitespace(rest);
    let value: String = rest
        .chars()
        .take_while(|&c| c != ';' && !c.is_whitespace())
        .collect();
    if value.is_empty() {
        return None;
    }
    Some((key.to_owned(), value))
}

fn match_procedure(line: &str) -> Option<String> {
    let rest = find_after(line, "procedure ")?;
    let (name, _) = take_word(rest);
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// `\[([\w]+)\]`.
fn match_cmdset(line: &str) -> Option<String> {
    let rest = find_after(line, "[")?;
    let (name, after) = take_word(rest);
    if name.is_empty() || !after.starts_with(']') {
        return None;
    }
    Some(name.to_owned())
}

fn match_command(line: &str) -> Option<(String, Option<String>, String)> {
    let start = line.char_indices().find(|&(_, c)| is_word(c))?.0;
    let (name, rest) = take_word(&line[start..]);
    let after_ws = skip_whitespace(rest);
    if let Some(args) = after_ws.strip_prefix('(') {
        if !args.is_empty() {
            return Some((
                name.to_owned(),
                Some(args.to_owned()),
                line[start..].to_owned(),
            ));
        }
    }
    Some((name.to_owned(), None, name.to_owned()))
}

/// `case\s+([^:]+)\s*:\s*(.+)`.
fn match_case(line: &str) -> Option<(String, String)> {
    let after_lit = &line[line.find("case")? + 4..];
    if !after_lit.chars().next()?.is_whitespace() {
        return None;
    }
    let rest = skip_whitespace(after_lit);
    let colon = rest.find(':')?;
    let key = rest[..colon].trim();
    let target = rest[colon + 1..].trim();
    if key.is_empty() || target.is_empty() {
        return None;
    }
    Some((key.to_owned(), target.to_owned()))
}

/// `default\s*:\s*(.+)`.
fn match_default(line: &str) -> Option<String> {
    let rest = find_after(line, "default")?;
    let rest = skip_whitespace(rest);
    let rest = rest.strip_prefix(':')?;
    let target = rest.trim();
    if target.is_empty() {
        None
    } else {
        Some(target.to_owned())
    }
}

/// `#include\s*"([\w\.\\/]+)"`.
fn match_include(line: &str) -> Option<String> {
    let after_lit = &line[line.find("#include")? + "#include".len()..];
    if !after_lit.chars().next()?.is_whitespace() {
        return None;
    }
    let rest = skip_whitespace(after_lit);
    let rest = rest.strip_prefix('"')?;
    let name: String = rest
        .chars()
        .take_while(|&c| is_word(c) || c == '.' || c == '\\' || c == '/')
        .collect();
    if name.is_empty() || !rest[name.len()..].starts_with('"') {
        return None;
    }
    Some(name)
}

fn match_arg_placeholder(s: &str) -> Option<u8> {
    let rest = find_after(s, "%")?;
    rest.chars().next()?.to_digit(10).map(|d| d as u8)
}

fn split_args(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                j += 1;
            }
            let end = if j < chars.len() { j + 1 } else { j };
            let m: String = chars[i..end].iter().collect();
            let m = m.trim();
            if !m.is_empty() {
                out.push(m.to_owned());
            }
            i = end;
        } else if is_word(c) || c == '%' || c == '$' || c == '.' || c == ' ' {
            let mut j = i;
            while j < chars.len()
                && (is_word(chars[j])
                    || chars[j] == '%'
                    || chars[j] == '$'
                    || chars[j] == '.'
                    || chars[j] == ' ')
            {
                j += 1;
            }
            let m: String = chars[i..j].iter().collect();
            let m = m.trim();
            if !m.is_empty() {
                out.push(m.to_owned());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Unknown,
    Uds,
    Ccp,
    Xcp,
}

impl Mode {
    fn detect(cmd: &str) -> Mode {
        let prefix = |p: &str| cmd.len() >= p.len() && cmd[..p.len()].eq_ignore_ascii_case(p);
        if prefix("XCPX") {
            Mode::Xcp
        } else if prefix("CCPX") {
            Mode::Ccp
        } else if prefix("UDSX") {
            Mode::Uds
        } else {
            Mode::Unknown
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Unknown => "Unknown",
            Mode::Uds => "UDS",
            Mode::Ccp => "CCP",
            Mode::Xcp => "XCP",
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CnfValue {
    Int(i64),
    Str(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CnfSegment {
    pub index: u8,
    pub reserved1: u8,
    pub reserved2: u8,
    pub start: u32,
    pub end: u32,
    pub data: Option<Vec<u8>>,
}

impl CnfSegment {
    pub fn byte_len(&self) -> u32 {
        self.end.wrapping_sub(self.start).wrapping_add(1)
    }

    fn from_values(values: &[CnfValue], key: &str) -> Result<CnfSegment> {
        let mut ints = Vec::with_capacity(5);
        for v in values {
            match v {
                CnfValue::Int(n) => ints.push(*n),
                CnfValue::Str(_) => {
                    return Err(Error::Parse(format!(
                        "CNF {key}: segment values must be numeric"
                    )))
                }
            }
        }
        if ints.len() < 5 {
            return Err(Error::Parse(format!("CNF {key}: segment needs 5 values")));
        }
        Ok(CnfSegment {
            index: ints[0] as u8,
            reserved1: ints[1] as u8,
            reserved2: ints[2] as u8,
            start: ints[3] as u32,
            end: ints[4] as u32,
            data: None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CnfSections {
    pub source_mem_areas: Vec<CnfSegment>,
    pub erase_mem_areas: Vec<CnfSegment>,
    pub dest_mem_areas: Vec<CnfSegment>,
}

/// `INCA_TO_ECU_CAN_ID`, `ECU_TO_INCA_CAN_ID`, `MAX_LENGTH`,
/// `ADDRESS_AND_LENGTH_FORMAT_IDENTIFIER`, `DATA_FORMAT_IDENTIFIER`,
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CnfFile {
    pub values: IndexMap<String, Vec<CnfValue>>,
    pub sections: CnfSections,
}

impl CnfFile {
    pub fn open(path: impl AsRef<Path>) -> Result<CnfFile> {
        let src = std::fs::read_to_string(path.as_ref())?;
        CnfFile::parse_str(&src)
    }

    pub fn parse_str(src: &str) -> Result<CnfFile> {
        let mut cnf = CnfFile::default();
        for raw in src.lines() {
            let Some((key, vals)) = match_cnf_line(raw) else {
                continue;
            };
            let values: Vec<CnfValue> = vals
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let t = s.trim();
                    parse_cnf_int(t).map_or_else(|| CnfValue::Str(t.to_owned()), CnfValue::Int)
                })
                .collect();
            let key = key.trim();
            match key {
                "SOURCE_MEM_AREA" => cnf
                    .sections
                    .source_mem_areas
                    .push(CnfSegment::from_values(&values, key)?),
                "ERASE_MEM_AREA" => cnf
                    .sections
                    .erase_mem_areas
                    .push(CnfSegment::from_values(&values, key)?),
                "DEST_MEM_AREA" => cnf
                    .sections
                    .dest_mem_areas
                    .push(CnfSegment::from_values(&values, key)?),
                _ => {
                    cnf.values.insert(key.to_owned(), values);
                }
            }
        }
        Ok(cnf)
    }

    fn get(&self, key: &str) -> Result<&[CnfValue]> {
        self.values
            .get(key)
            .map(Vec::as_slice)
            .ok_or_else(|| Error::Parse(format!("CNF key {key} not present")))
    }

    fn first_int(&self, key: &str) -> Result<i64> {
        match self.get(key)?.first() {
            Some(CnfValue::Int(n)) => Ok(*n),
            _ => Err(Error::Parse(format!(
                "CNF key {key}: numeric value expected"
            ))),
        }
    }

    pub fn project(&self) -> Result<&str> {
        match self.get("PROJECT_NAME")?.first() {
            Some(CnfValue::Str(s)) => Ok(s),
            _ => Err(Error::Parse(
                "CNF key PROJECT_NAME: string value expected".to_owned(),
            )),
        }
    }

    pub fn ecu_address(&self) -> Result<u16> {
        Ok(self.first_int("ECU_ADDR")? as u16)
    }

    pub fn baudrate(&self) -> Result<u32> {
        Ok(self.first_int("KWP_CAN_BUS_TIMING")? as u32)
    }

    pub fn cmd_id(&self) -> Result<u32> {
        Ok(self.first_int("INCA_TO_ECU_CAN_ID")? as u32)
    }

    pub fn rsp_id(&self) -> Result<u32> {
        Ok(self.first_int("ECU_TO_INCA_CAN_ID")? as u32)
    }

    pub fn max_length(&self) -> Result<u16> {
        match self.values.get("MAX_LENGTH") {
            Some(_) => Ok(self.first_int("MAX_LENGTH")? as u16),
            None => Ok(u16::MAX),
        }
    }

    pub fn fmt_identifier(&self) -> Result<u8> {
        Ok(self.first_int("ADDRESS_AND_LENGTH_FORMAT_IDENTIFIER")? as u8)
    }

    pub fn data_fmt_identifier(&self) -> Result<u8> {
        Ok(self.first_int("DATA_FORMAT_IDENTIFIER")? as u8)
    }

    pub fn erase_routine(&self) -> Result<Vec<i64>> {
        all_ints(self.get("LOC_ROUTINE_ERASE")?, "LOC_ROUTINE_ERASE")
    }

    pub fn chk_routine(&self) -> Result<Vec<i64>> {
        all_ints(self.get("LOC_ROUTINE_CHK")?, "LOC_ROUTINE_CHK")
    }

    pub fn load_segment_data_from_path(
        &mut self,
        data_file: impl AsRef<Path>,
        offset: u32,
    ) -> Result<()> {
        let path = data_file.as_ref();
        let name = path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let df = DataFile::open(path, MemorySegmentList::new(), 0, None)
            .map_err(|e| prm_error(format!("Failed to open datafile {name}! ({e})")))?;
        self.load_segment_data(&df, offset, &name)
    }

    /// - `offset + end > memSeg.Address + memSeg.Size` → `strCNFFailSeg`
    /// - `seg.data = memSeg.getDataBytes(start + offset, seg.byte_len())`.
    pub fn load_segment_data(
        &mut self,
        df: &DataFile,
        offset: u32,
        source_name: &str,
    ) -> Result<()> {
        for seg in &mut self.sections.source_mem_areas {
            let addr = seg.start.wrapping_add(offset);
            let mem = df.base().find_mem_seg(u64::from(addr), 1).ok_or_else(|| {
                prm_error(format!(
                    "Segment {} ({addr:X}) not found in {source_name}!",
                    seg.index
                ))
            })?;
            if u64::from(offset.wrapping_add(seg.end)) > mem.address + mem.size() as u64 {
                return Err(prm_error(format!(
                    "Segment {} length not supported in {source_name}!",
                    seg.index
                )));
            }
            let data = mem
                .get_data_bytes(u64::from(addr), seg.byte_len() as usize)
                .map_err(|e| prm_error(format!("Failed to open datafile {source_name}! ({e})")))?;
            seg.data = Some(data.to_vec());
        }
        Ok(())
    }
}

fn all_ints(values: &[CnfValue], key: &str) -> Result<Vec<i64>> {
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        match v {
            CnfValue::Int(n) => out.push(*n),
            CnfValue::Str(_) => {
                return Err(Error::Parse(format!(
                    "CNF key {key}: numeric values expected"
                )))
            }
        }
    }
    Ok(out)
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrmValue {
    Int(i64),
    UInt(u64),
    Str(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CaseKey {
    Int(i64),
    Str(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrmCommand {
    pub name: String,
    pub args: Vec<PrmValue>,
    pub default_target: Option<String>,
    pub cases: IndexMap<CaseKey, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmdSet {
    pub name: String,
    pub commands: Vec<PrmCommand>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Procedure {
    pub name: String,
    pub cmdsets: Vec<CmdSet>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrmFile {
    pub source_file: String,
    pub vars: IndexMap<String, PrmValue>,
    pub cmdsets: IndexMap<String, CmdSet>,
    pub procedures: IndexMap<String, Procedure>,
    pub unknown_commands: Vec<(String, usize)>,
    pub mode: Mode,
    pub cnf: CnfFile,
}

impl PrmFile {
    pub fn open(path: impl AsRef<Path>) -> Result<PrmFile> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path)?;
        let base_dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
        let mut prm = Self::parse_source(&src, &base_dir)?;
        prm.source_file = path.to_string_lossy().into_owned();
        if prm.mode == Mode::Unknown {
            return Err(Error::Parse(ERR_MISSING_MODE.to_owned()));
        }
        Ok(prm)
    }

    pub fn parse_str(src: &str, base_dir: &Path) -> Result<PrmFile> {
        let prm = Self::parse_source(src, base_dir)?;
        if prm.mode == Mode::Unknown {
            return Err(Error::Parse(ERR_MISSING_MODE.to_owned()));
        }
        Ok(prm)
    }

    fn parse_source(src: &str, base_dir: &Path) -> Result<PrmFile> {
        let mut prm = PrmFile::default();
        let mut cur_cmdset: Option<CmdSet> = None;
        let mut cur_proc: Option<Procedure> = None;
        let mut cur_cmd: Option<usize> = None;

        for (line, line_no) in logical_lines(src, base_dir)? {
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            if let Some((key, value)) = match_define(&line) {
                let v = if value.starts_with('"') {
                    PrmValue::Str(value.trim_matches('"').to_owned())
                } else {
                    parse_prm_int(&value).map_or(PrmValue::Str(value), PrmValue::Int)
                };
                prm.vars.insert(key, v);
            } else if let Some(name) = match_procedure(&line) {
                cur_proc = Some(Procedure {
                    name,
                    cmdsets: Vec::new(),
                });
            } else if cur_proc.is_some() && line.starts_with('}') {
                let p = cur_proc.take().expect("checked");
                prm.procedures.insert(p.name.clone(), p);
            } else if let Some(name) = match_cmdset(&line) {
                match &cur_cmdset {
                    Some(cs) if cs.name == name => continue,
                    Some(cs) if format!("{}{CMDSET_END_SUFFIX}", cs.name) == name => {
                        let cs = cur_cmdset.take().expect("checked");
                        cur_cmd = None;
                        if let Some(p) = &mut cur_proc {
                            p.cmdsets.push(cs);
                        } else {
                            prm.cmdsets.insert(cs.name.clone(), cs);
                        }
                    }
                    _ => {
                        cur_cmd = None;
                        cur_cmdset = Some(CmdSet {
                            name,
                            commands: Vec::new(),
                        });
                    }
                }
            } else if let Some(cs) = &mut cur_cmdset {
                if cur_cmd.is_some() {
                    if let Some(target) = match_default(&line) {
                        let i = cur_cmd.take().expect("checked");
                        cs.commands[i].default_target = Some(target);
                        continue;
                    }
                    if let Some((key, target)) = match_case(&line) {
                        let i = cur_cmd.expect("checked");
                        let key = parse_prm_int(&key).map_or(CaseKey::Str(key), CaseKey::Int);
                        cs.commands[i].cases.insert(key, target);
                        continue;
                    }
                }
                if let Some((text, args, raw)) = match_command(&line) {
                    if KNOWN_METHODS.contains(&text.as_str()) {
                        if prm.mode == Mode::Unknown {
                            prm.mode = Mode::detect(&text);
                        }
                        let args = args
                            .map(|a| {
                                split_args(&a)
                                    .iter()
                                    .map(|t| {
                                        parse_prm_int(t)
                                            .map_or_else(|| PrmValue::Str(t.clone()), PrmValue::Int)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        cs.commands.push(PrmCommand {
                            name: text,
                            args,
                            ..Default::default()
                        });
                        cur_cmd = Some(cs.commands.len() - 1);
                    } else {
                        cur_cmd = None;
                        prm.unknown_commands.push((raw, line_no));
                    }
                }
            }
        }

        let cnf_name = prm
            .vars
            .get(CNF_KEY)
            .ok_or_else(|| Error::Parse(ERR_MISSING_CNF.to_owned()))?;
        let PrmValue::Str(cnf_name) = cnf_name else {
            return Err(Error::Parse(format!("{CNF_KEY}: string value expected")));
        };
        let file_name = Path::new(cnf_name)
            .file_name()
            .ok_or_else(|| Error::Parse(format!("invalid {CNF_KEY} value: {cnf_name}")))?;
        prm.cnf = CnfFile::open(base_dir.join(file_name))?;
        Ok(prm)
    }

    pub fn get_commands(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |s: String| {
            if !out.contains(&s) {
                out.push(s);
            }
        };
        for name in self.cmdsets.keys() {
            push(name.clone());
            push(format!("{name}{CMDSET_END_SUFFIX}"));
        }
        for (name, p) in &self.procedures {
            push(name.clone());
            for cs in &p.cmdsets {
                push(cs.name.clone());
                push(format!("{}{CMDSET_END_SUFFIX}", cs.name));
            }
        }
        out
    }

    pub fn substitute_args(&mut self, args: &str) -> Result<()> {
        let argv: Vec<&str> = args.split(' ').filter(|s| !s.is_empty()).collect();
        for (key, value) in &mut self.vars {
            let PrmValue::Str(s) = &*value else {
                continue;
            };
            if let Some(idx) = match_arg_placeholder(s) {
                let Some(&arg) = argv.get(idx as usize) else {
                    return Err(Error::Parse(format!(
                        "argument %{idx} out of range for variable {key}"
                    )));
                };
                *value = PrmValue::Str(arg.to_owned());
            }
        }
        for cs in self.cmdsets.values_mut() {
            for cmd in &mut cs.commands {
                for arg in &mut cmd.args {
                    let PrmValue::Str(s) = &*arg else {
                        continue;
                    };
                    if let Some(idx) = match_arg_placeholder(s) {
                        let Some(&a) = argv.get(idx as usize) else {
                            return Err(Error::Parse(format!(
                                "argument %{idx} out of range for command {}",
                                cmd.name
                            )));
                        };
                        *arg = match a.parse::<u64>() {
                            Ok(n) => PrmValue::UInt(n),
                            Err(_) => PrmValue::Str(format!("@\"{a}\"")),
                        };
                    }
                }
            }
        }
        Ok(())
    }
}

fn logical_lines(src: &str, base_dir: &Path) -> Result<Vec<(String, usize)>> {
    let mut out = Vec::new();
    let mut line_no = 0;
    for raw in src.lines() {
        let line = raw.trim();
        line_no += 1;
        if let Some(inc) = match_include(line) {
            let inc_src = std::fs::read_to_string(base_dir.join(&inc))?;
            for raw2 in inc_src.lines() {
                out.push((raw2.trim().to_owned(), line_no));
            }
        } else {
            out.push((line.to_owned(), line_no));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- CNF ----

    const SAMPLE_CNF: &str = "\
*****************************************************************************
; comment line
PROJECT_NAME:,         Software download for demo
ECU_ADDR:,             0x7C
KWP_CAN_BUS_TIMING:,   500000
INCA_TO_ECU_CAN_ID:,   0x700
ECU_TO_INCA_CAN_ID:,   0x708
MAX_LENGTH:,           0xfc
ADDRESS_AND_LENGTH_FORMAT_IDENTIFIER:, 0x44
DATA_FORMAT_IDENTIFIER:, 0x00
LOC_ROUTINE_ERASE:,    0xFF00, 0x21, 0x01
LOC_ROUTINE_CHK:,      0xFF01, 0x23, 0x01
SOURCE_MEM_AREA:,      1, 0x00, 0x00, 0x100000L, 0x13FFFFL
ERASE_MEM_AREA:,       2, 0x00, 0x00, 0x100000L, 0x13FFFFL
DEST_MEM_AREA:,        3, 0x00, 0x00, 0x020100L, 0x0FFFFFL
";

    #[test]
    fn cnf_parse_and_accessors() {
        let cnf = CnfFile::parse_str(SAMPLE_CNF).unwrap();
        assert_eq!(cnf.project().unwrap(), "Software download for demo");
        assert_eq!(cnf.ecu_address().unwrap(), 0x7C);
        assert_eq!(cnf.baudrate().unwrap(), 500000);
        assert_eq!(cnf.cmd_id().unwrap(), 0x700);
        assert_eq!(cnf.rsp_id().unwrap(), 0x708);
        assert_eq!(cnf.max_length().unwrap(), 0xfc);
        assert_eq!(cnf.fmt_identifier().unwrap(), 0x44);
        assert_eq!(cnf.data_fmt_identifier().unwrap(), 0x00);
        assert_eq!(cnf.erase_routine().unwrap(), vec![0xFF00, 0x21, 0x01]);
        assert_eq!(cnf.chk_routine().unwrap(), vec![0xFF01, 0x23, 0x01]);
    }

    #[test]
    fn cnf_segments() {
        let cnf = CnfFile::parse_str(SAMPLE_CNF).unwrap();
        assert_eq!(cnf.sections.source_mem_areas.len(), 1);
        assert_eq!(cnf.sections.erase_mem_areas.len(), 1);
        assert_eq!(cnf.sections.dest_mem_areas.len(), 1);
        let src = &cnf.sections.source_mem_areas[0];
        assert_eq!(src.index, 1);
        assert_eq!(src.start, 0x100000);
        assert_eq!(src.end, 0x13FFFF);
        assert_eq!(src.byte_len(), 0x40000);
        assert_eq!(cnf.sections.dest_mem_areas[0].index, 3);
        assert_eq!(
            cnf.sections.dest_mem_areas[0].byte_len(),
            0x0FFFFF - 0x20100 + 1
        );
        assert!(!cnf.values.contains_key("SOURCE_MEM_AREA"));
    }

    #[test]
    fn cnf_defaults_and_errors() {
        let cnf = CnfFile::parse_str("PROJECT_NAME:X\n").unwrap();
        assert_eq!(cnf.max_length().unwrap(), 65535);
        assert!(cnf.ecu_address().is_err());
        assert!(cnf.erase_routine().is_err());
        let cnf2 = CnfFile::parse_str("ECU_ADDR:abc\n").unwrap();
        assert!(cnf2.ecu_address().is_err());
        assert!(CnfFile::parse_str("DEST_MEM_AREA:1,0,0,x,9\n").is_err());
    }

    #[test]
    fn int_parsing() {
        assert_eq!(parse_prm_int("0x1F"), Some(31));
        assert_eq!(parse_prm_int("$FF"), Some(255));
        assert_eq!(parse_prm_int("123L"), Some(123));
        assert_eq!(parse_prm_int("42"), Some(42));
        assert_eq!(parse_prm_int("\"42\""), None);
        assert_eq!(parse_prm_int("abc"), None);
        assert_eq!(parse_cnf_int("0x100000L"), Some(0x100000));
        assert_eq!(parse_cnf_int("10"), Some(10));
    }

    // ---- PRM ----

    fn setup_prm(tag: &str, prm_src: &str, extra: &[(&str, &str)]) -> (PathBuf, PrmFile) {
        let dir = std::env::temp_dir().join(format!("autors_prm_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("conf.cnf"), SAMPLE_CNF).unwrap();
        for (name, content) in extra {
            std::fs::write(dir.join(name), content).unwrap();
        }
        let prm_path = dir.join("main.prm");
        std::fs::write(&prm_path, prm_src).unwrap();
        let prm = PrmFile::open(&prm_path).unwrap();
        (dir, prm)
    }

    const SAMPLE_PRM: &str = "\
; comment
#define CONFIG conf.cnf
#define NAME \"quoted\"
#define TIMEOUT 0x2EE0
#define PLAIN hello
#include \"sub.pri\"
[BOOT]
UDSX_PROGRAM_MEMORY(%0, 2, 3, 4, \"fmt\")
case TRUE : BOOT2
case 0x1 : BOOT3
default : BOOT4
[BOOT_END]
[BOOT2]
WAIT(10)
default : BOOT
[BOOT2_END]
[BOOT3]
NOT_A_COMMAND(1)
default : BOOT
[BOOT3_END]
[BOOT4]
UDSX_VERIFY_MEMORY(1, 2, 3, 4, 5, 6)
default : BOOT
[BOOT4_END]
";

    const SAMPLE_PRI: &str = "\
procedure SUB
{
[STEP]
UDSX_SECURITY_ACCESS(1, 2, \"sk.dll\")
default : $return
[STEP_END]
}
";

    #[test]
    fn prm_full_parse() {
        let (dir, prm) = setup_prm("full", SAMPLE_PRM, &[("sub.pri", SAMPLE_PRI)]);
        assert_eq!(prm.mode, Mode::Uds);
        assert_eq!(prm.vars["NAME"], PrmValue::Str("quoted".to_owned()));
        assert_eq!(prm.vars["TIMEOUT"], PrmValue::Int(0x2EE0));
        assert_eq!(prm.vars["PLAIN"], PrmValue::Str("hello".to_owned()));
        let boot = &prm.cmdsets["BOOT"];
        assert_eq!(boot.commands.len(), 1);
        let cmd = &boot.commands[0];
        assert_eq!(cmd.name, "UDSX_PROGRAM_MEMORY");
        assert_eq!(cmd.args.len(), 5);
        assert_eq!(cmd.args[1], PrmValue::Int(2));
        assert_eq!(cmd.args[4], PrmValue::Str("\"fmt\"".to_owned()));
        assert_eq!(cmd.cases[&CaseKey::Str("TRUE".to_owned())], "BOOT2");
        assert_eq!(cmd.cases[&CaseKey::Int(1)], "BOOT3");
        assert_eq!(cmd.default_target.as_deref(), Some("BOOT4"));
        assert_eq!(prm.unknown_commands.len(), 2);
        assert_eq!(prm.unknown_commands[0].0, "NOT_A_COMMAND(1)");
        assert_eq!(prm.unknown_commands[1].0, "default");
        assert!(prm.unknown_commands[0].1 > 0);
        let sub = &prm.procedures["SUB"];
        assert_eq!(sub.cmdsets.len(), 1);
        assert_eq!(sub.cmdsets[0].name, "STEP");
        assert_eq!(prm.cnf.cmd_id().unwrap(), 0x700);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prm_mode_detection() {
        let (_, prm) = setup_prm(
            "ccp",
            "#define CONFIG conf.cnf\n[S]\nCCPX_START_ECU_COMMUNICATION\ndefault : S\n[S_END]\n",
            &[],
        );
        assert_eq!(prm.mode, Mode::Ccp);
        let (_, prm) = setup_prm(
            "xcp",
            "#define CONFIG conf.cnf\n[S]\nXCPX_PROGRAM_CLEAR(1, 2, 3)\ndefault : S\n[S_END]\n",
            &[],
        );
        assert_eq!(prm.mode, Mode::Xcp);
        std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("autors_prm_{}_ccp", std::process::id())),
        )
        .ok();
        std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("autors_prm_{}_xcp", std::process::id())),
        )
        .ok();
    }

    #[test]
    fn prm_missing_cnf_key() {
        let dir = std::env::temp_dir().join(format!("autors_prm_{}_nocnf", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("m.prm");
        std::fs::write(
            &p,
            "[S]\nUDSX_SECURITY_ACCESS(1, 2, \"x\")\ndefault : S\n[S_END]\n",
        )
        .unwrap();
        let err = PrmFile::open(&p).unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
        assert!(err.to_string().contains(ERR_MISSING_CNF), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prm_unknown_mode_rejected() {
        let dir = std::env::temp_dir().join(format!("autors_prm_{}_nomode", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("conf.cnf"), SAMPLE_CNF).unwrap();
        let p = dir.join("m.prm");
        std::fs::write(
            &p,
            "#define CONFIG conf.cnf\n[S]\nWAIT(10)\ndefault : S\n[S_END]\n",
        )
        .unwrap();
        let err = PrmFile::open(&p).unwrap_err();
        assert!(
            err.to_string().contains("doesn't rely on UDS nor CCP"),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prm_get_commands() {
        let (dir, prm) = setup_prm("cmds", SAMPLE_PRM, &[("sub.pri", SAMPLE_PRI)]);
        let cmds = prm.get_commands();
        for expect in [
            "BOOT",
            "BOOT_END",
            "BOOT2",
            "BOOT2_END",
            "SUB",
            "STEP",
            "STEP_END",
        ] {
            assert!(cmds.iter().any(|c| c == expect), "missing {expect}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prm_substitute_args() {
        let (dir, mut prm) = setup_prm("subst", SAMPLE_PRM, &[("sub.pri", SAMPLE_PRI)]);
        prm.vars
            .insert("ARGV".to_owned(), PrmValue::Str("%0".to_owned()));
        prm.substitute_args("hello 42").unwrap();
        assert_eq!(prm.vars["ARGV"], PrmValue::Str("hello".to_owned()));
        assert_eq!(
            prm.cmdsets["BOOT"].commands[0].args[0],
            PrmValue::Str("@\"hello\"".to_owned())
        );
        prm.vars
            .insert("ARGV2".to_owned(), PrmValue::Str("%1".to_owned()));
        prm.substitute_args("hello 42").unwrap();
        assert_eq!(prm.vars["ARGV2"], PrmValue::Str("42".to_owned()));
        prm.vars
            .insert("ARGV3".to_owned(), PrmValue::Str("%9".to_owned()));
        assert!(prm.substitute_args("hello 42").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prm_cmdset_close_quirks() {
        let (dir, prm) = setup_prm(
            "quirks",
            "#define CONFIG conf.cnf\n[A]\nUDSX_PROGRAM_MEMORY(1, 2, 3, 4, \"x\")\n[A]\ndefault : B\n[A_END]\n[B]\nUDSX_VERIFY_MEMORY(1, 2, 3, 4, 5, 6)\ndefault : A\n[B_END]\n",
            &[],
        );
        let a = &prm.cmdsets["A"];
        assert_eq!(a.commands.len(), 1);
        assert_eq!(a.commands[0].default_target.as_deref(), Some("B"));
        assert!(prm.cmdsets.contains_key("B"));
        std::fs::remove_dir_all(&dir).ok();
    }

    use autors_datafile::datafile::{DataFile, DataFileType};

    const LOAD_HEX: &str =
        ":020000040001F9\n:10000000000102030405060708090A0B0C0D0E0F78\n:00000001FF\n";

    fn load_df() -> DataFile {
        DataFile::parse(
            DataFileType::IntelHex,
            LOAD_HEX.as_bytes(),
            MemorySegmentList::new(),
            0,
            None,
        )
        .unwrap()
    }

    #[test]
    fn cnf_load_segment_data() {
        let df = load_df();
        let mut cnf = CnfFile::parse_str("SOURCE_MEM_AREA:1,0,0,0x10000L,0x1000FL\n").unwrap();
        cnf.load_segment_data(&df, 0, "t.hex").unwrap();
        let seg = &cnf.sections.source_mem_areas[0];
        let expect: Vec<u8> = (0x00..=0x0F).collect();
        assert_eq!(seg.data.as_deref(), Some(expect.as_slice()));
        let mut cnf2 = CnfFile::parse_str("SOURCE_MEM_AREA:1,0,0,0x00000L,0x0000FL\n").unwrap();
        cnf2.load_segment_data(&df, 0x10000, "t.hex").unwrap();
        assert_eq!(
            cnf2.sections.source_mem_areas[0].data.as_deref(),
            Some(expect.as_slice())
        );
    }

    #[test]
    fn cnf_load_segment_data_errors() {
        let df = load_df();
        let mut cnf = CnfFile::parse_str("SOURCE_MEM_AREA:1,0,0,0x00000L,0x0000FL\n").unwrap();
        let err = cnf.load_segment_data(&df, 0x20000, "t.hex").unwrap_err();
        assert!(matches!(err, Error::General(_)));
        assert!(
            err.to_string()
                .contains("Segment 1 (20000) not found in t.hex!"),
            "{err}"
        );
        // strCNFFailSeg:offset + end > memSeg.Address + memSeg.Size
        let mut cnf = CnfFile::parse_str("SOURCE_MEM_AREA:1,0,0,0x10000L,0x100FFL\n").unwrap();
        let err = cnf.load_segment_data(&df, 0, "t.hex").unwrap_err();
        assert!(
            err.to_string()
                .contains("Segment 1 length not supported in t.hex!"),
            "{err}"
        );
        let mut cnf = CnfFile::parse_str("SOURCE_MEM_AREA:1,0,0,0x10000L,0x10010L\n").unwrap();
        let err = cnf.load_segment_data(&df, 0, "t.hex").unwrap_err();
        assert!(
            err.to_string().contains("Failed to open datafile t.hex!"),
            "{err}"
        );
    }
}
