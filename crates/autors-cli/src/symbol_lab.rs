use std::path::{Path, PathBuf};

use autors_a2l::{Project, ProjectChild};
use autors_elf::elf::ElfFile;
use autors_map::map::MapFile;
use autors_symbols::update::{update_and_write_a2l, AddressNodeResolver, UpdateData, UpdateType};

#[derive(Debug, Clone)]
pub struct SymbolCandidate {
    pub record: UpdateData,
    pub module: String,
    pub object: String,
    pub symbol: String,
    pub current_address: Option<u32>,
    pub selected: bool,
}

impl SymbolCandidate {
    pub fn status(&self) -> &'static str {
        match self.record.typ {
            UpdateType::NotMatched => "not matched",
            UpdateType::Matched => "matched",
            UpdateType::AdjustAddress => "address update",
            UpdateType::AdjustAddressAndSize => "address + size mismatch",
        }
    }

    pub fn can_select(&self, allow_size_mismatch: bool) -> bool {
        self.record.typ == UpdateType::AdjustAddress
            || (allow_size_mismatch && self.record.typ == UpdateType::AdjustAddressAndSize)
    }
}

pub struct SymbolLab {
    pub source_path: Option<PathBuf>,
    pub source_kind: String,
    pub address_multiplier: u64,
    pub data_section_start: u64,
    pub data_section_len: u64,
    pub candidates: Vec<SymbolCandidate>,
    pub candidate_index: usize,
    pub allow_size_mismatch: bool,
    pub preserve_bit_mask: bool,
    pub last_output: Option<PathBuf>,
    pub last_error: Option<String>,
}

impl Default for SymbolLab {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolLab {
    pub fn new() -> Self {
        Self {
            source_path: None,
            source_kind: "-".to_owned(),
            address_multiplier: 1,
            data_section_start: 0,
            data_section_len: 0,
            candidates: Vec::new(),
            candidate_index: 0,
            allow_size_mismatch: false,
            preserve_bit_mask: true,
            last_output: None,
            last_error: None,
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.source_path.is_some()
    }

    pub fn attach_source(&mut self, path: &Path, project: &Project) -> Result<usize, String> {
        self.source_path = Some(path.to_owned());
        self.refresh(project)
    }

    pub fn refresh(&mut self, project: &Project) -> Result<usize, String> {
        let path = self
            .source_path
            .as_ref()
            .ok_or_else(|| "open an ELF/AXF or MAP symbol file first".to_owned())?;
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let (records, data_start, data_len, source_kind) = match extension.as_str() {
            "map" => {
                let source = MapFile::open(path).map_err(|error| error.to_string())?;
                let (records, start, len) = source
                    .get_values_to_synchronize(project, self.address_multiplier)
                    .map_err(|error| error.to_string())?;
                (records, start, len, "Linker MAP")
            }
            "elf" | "axf" => {
                let mut source = ElfFile::open(path, true).map_err(|error| error.to_string())?;
                let (records, start, len) = source
                    .get_values_to_synchronize(project, self.address_multiplier)
                    .map_err(|error| error.to_string())?;
                (records, start, len, "ELF / DWARF")
            }
            _ => return Err(format!("unsupported symbol source extension {extension:?}")),
        };
        self.source_kind = source_kind.to_owned();
        self.data_section_start = data_start;
        self.data_section_len = data_len;
        self.candidates = build_candidates(project, records)?;
        self.candidate_index = 0;
        self.last_error = None;
        Ok(self.candidates.len())
    }

    pub fn set_multiplier_text(&mut self, input: &str, project: &Project) -> Result<usize, String> {
        let input = input.trim();
        let multiplier = input
            .strip_prefix("0x")
            .or_else(|| input.strip_prefix("0X"))
            .map_or_else(|| input.parse::<u64>(), |hex| u64::from_str_radix(hex, 16))
            .map_err(|_| "enter a positive decimal or 0x address multiplier".to_owned())?;
        if multiplier == 0 {
            return Err("address multiplier must be greater than zero".to_owned());
        }
        self.address_multiplier = multiplier;
        self.refresh(project)
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.candidate_index = self
            .candidate_index
            .saturating_add_signed(delta)
            .min(self.candidates.len().saturating_sub(1));
    }

    pub fn toggle_selected(&mut self) -> Result<bool, String> {
        let candidate = self
            .candidates
            .get_mut(self.candidate_index)
            .ok_or_else(|| "no symbol candidate selected".to_owned())?;
        if !candidate.can_select(self.allow_size_mismatch) {
            return Err(match candidate.record.typ {
                UpdateType::AdjustAddressAndSize => {
                    "enable size-mismatch updates with z before selecting this row".to_owned()
                }
                _ => "matched and unmatched rows are informational only".to_owned(),
            });
        }
        candidate.selected = !candidate.selected;
        Ok(candidate.selected)
    }

    pub fn selected_count(&self) -> usize {
        self.candidates
            .iter()
            .filter(|candidate| candidate.selected)
            .count()
    }

    pub fn count(&self, update_type: UpdateType) -> usize {
        self.candidates
            .iter()
            .filter(|candidate| candidate.record.typ == update_type)
            .count()
    }

    pub fn apply(&mut self, project: &mut Project, target: &Path) -> Result<u32, String> {
        let records = self
            .candidates
            .iter()
            .filter(|candidate| candidate.selected)
            .map(|candidate| candidate.record.clone())
            .collect::<Vec<_>>();
        if records.is_empty() {
            return Err("select at least one address update first".to_owned());
        }
        let mut updated_project = project.clone();
        let updated = update_and_write_a2l(
            &records,
            target,
            &mut updated_project,
            self.allow_size_mismatch,
            self.preserve_bit_mask,
            false,
        )
        .map_err(|error| error.to_string())?;
        *project = updated_project;
        self.last_output = Some(target.to_owned());
        self.last_error = None;
        Ok(updated)
    }
}

fn build_candidates(
    project: &Project,
    records: Vec<UpdateData>,
) -> Result<Vec<SymbolCandidate>, String> {
    let resolver = AddressNodeResolver::new(project);
    records
        .into_iter()
        .map(|record| {
            let module = match project.children.get(record.module_idx) {
                Some(ProjectChild::Module(module)) => module,
                _ => return Err(format!("invalid symbol module index {}", record.module_idx)),
            };
            let child = module.children.get(record.child_idx).ok_or_else(|| {
                format!(
                    "invalid symbol child index {} for module {}",
                    record.child_idx, module.name
                )
            })?;
            let node = resolver
                .address_node(child)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "symbol record points at a non-addressable A2L node".to_owned())?;
            let (symbol, _) = node.symbol_name();
            let selected = record.typ == UpdateType::AdjustAddress;
            Ok(SymbolCandidate {
                record,
                module: module.name.clone(),
                object: node.name.to_owned(),
                symbol,
                current_address: node.address,
                selected,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_source_builds_selectable_a2l_update_candidates() {
        let mut project = Project::parse_str(
            r#"
/begin PROJECT Demo "demo"
 /begin MODULE ECU "ecu"
  /begin MEASUREMENT Speed "speed" UWORD NO_COMPU_METHOD 1 0 0 8000 ECU_ADDRESS 0x1000 /end MEASUREMENT
 /end MODULE
/end PROJECT
"#,
        )
        .unwrap();
        let directory =
            std::env::temp_dir().join(format!("autors-cli-symbol-lab-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("firmware.map");
        std::fs::write(&path, "00002000 Speed\n").unwrap();
        let mut lab = SymbolLab::new();
        assert_eq!(lab.attach_source(&path, &project).unwrap(), 1);
        assert_eq!(lab.count(UpdateType::AdjustAddress), 1);
        assert_eq!(lab.selected_count(), 1);
        assert_eq!(lab.candidates[0].record.address, 0x2000);
        let output = directory.join("updated.a2l");
        assert_eq!(lab.apply(&mut project, &output).unwrap(), 1);
        assert!(output.is_file());
        lab.refresh(&project).unwrap();
        assert_eq!(lab.count(UpdateType::Matched), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
