use std::path::{Path, PathBuf};

use cargo_metadata::MetadataCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityGroup {
    Overview,
    Network,
    Trace,
    Calibration,
    Diagnostics,
    Simulation,
    Platform,
    Integration,
}

impl CapabilityGroup {
    pub const ALL: [Self; 8] = [
        Self::Overview,
        Self::Network,
        Self::Trace,
        Self::Calibration,
        Self::Diagnostics,
        Self::Simulation,
        Self::Platform,
        Self::Integration,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Network => "Network",
            Self::Trace => "Trace",
            Self::Calibration => "Calibration",
            Self::Diagnostics => "Diagnostics",
            Self::Simulation => "Simulation",
            Self::Platform => "Platform",
            Self::Integration => "Integration",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Overview => "Complete workspace inventory",
            Self::Network => "CAN/LIN databases, buses, frames and transport",
            Self::Trace => "CAN/LIN traces and measurement recordings",
            Self::Calibration => "ECU descriptions, values, formulas and symbols",
            Self::Diagnostics => "UDS, KWP, DoIP, ODX and flashing procedures",
            Self::Simulation => "Remaining-bus scheduling and runtime orchestration",
            Self::Platform => "Shared utilities, runtimes and native boundaries",
            Self::Integration => "C ABI and end-user workbenches",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Capability {
    pub name: String,
    pub description: String,
    pub version: String,
    pub group: CapabilityGroup,
    pub features: Vec<String>,
    pub dependencies: Vec<String>,
    pub manifest_path: PathBuf,
}

impl Capability {
    pub fn detail_lines(&self) -> Vec<String> {
        let mut lines = vec![
            self.description.clone(),
            String::new(),
            format!("Version       {}", self.version),
            format!("Area          {}", self.group.title()),
            format!("Features      {}", list_or_none(&self.features)),
            format!("autors deps   {}", list_or_none(&self.dependencies)),
            String::new(),
            format!("Manifest      {}", self.manifest_path.display()),
        ];
        lines.push(String::new());
        lines.extend(
            workflows(self.name.as_str())
                .iter()
                .map(|line| (*line).to_owned()),
        );
        lines
    }
}

fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

#[derive(Debug, Clone)]
pub struct CapabilityCatalog {
    pub workspace_root: PathBuf,
    pub capabilities: Vec<Capability>,
}

impl CapabilityCatalog {
    pub fn load(manifest_path: &Path) -> Result<Self, String> {
        let metadata = MetadataCommand::new()
            .manifest_path(manifest_path)
            .no_deps()
            .exec()
            .map_err(|error| format!("could not read workspace metadata: {error}"))?;
        let mut capabilities = metadata
            .packages
            .into_iter()
            .filter(|package| package.name.as_str().starts_with("autors-"))
            .map(|package| {
                let name = package.name.to_string();
                let mut features = package.features.keys().cloned().collect::<Vec<_>>();
                features.sort();
                let mut dependencies = package
                    .dependencies
                    .iter()
                    .map(|dependency| dependency.name.to_string())
                    .filter(|dependency| dependency.starts_with("autors-"))
                    .collect::<Vec<_>>();
                dependencies.sort();
                dependencies.dedup();
                Capability {
                    group: classify(&name),
                    name,
                    description: package
                        .description
                        .unwrap_or_else(|| "No description".to_owned()),
                    version: package.version.to_string(),
                    features,
                    dependencies,
                    manifest_path: package.manifest_path.into_std_path_buf(),
                }
            })
            .collect::<Vec<_>>();
        capabilities.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Self {
            workspace_root: metadata.workspace_root.into_std_path_buf(),
            capabilities,
        })
    }

    pub fn in_group(&self, group: CapabilityGroup, query: &str) -> Vec<usize> {
        let query = query.to_ascii_lowercase();
        self.capabilities
            .iter()
            .enumerate()
            .filter(|(_, capability)| {
                (group == CapabilityGroup::Overview || capability.group == group)
                    && (query.is_empty()
                        || capability.name.to_ascii_lowercase().contains(&query)
                        || capability.description.to_ascii_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn group_count(&self, group: CapabilityGroup) -> usize {
        if group == CapabilityGroup::Overview {
            self.capabilities.len()
        } else {
            self.capabilities
                .iter()
                .filter(|capability| capability.group == group)
                .count()
        }
    }
}

fn classify(name: &str) -> CapabilityGroup {
    match name {
        "autors-can" | "autors-lin" | "autors-dbc" | "autors-ldf" | "autors-isotp" => {
            CapabilityGroup::Network
        }
        "autors-asc" | "autors-blf" | "autors-ltrc" | "autors-mdf" => CapabilityGroup::Trace,
        "autors-a2l" | "autors-cdf" | "autors-dcm" | "autors-datafile" | "autors-formula"
        | "autors-values" | "autors-symbols" | "autors-elf" | "autors-map" => {
            CapabilityGroup::Calibration
        }
        "autors-diag" | "autors-odx" | "autors-prm" => CapabilityGroup::Diagnostics,
        "autors-scheduler" | "autors-comm" | "autors-ccp" | "autors-xcp" => {
            CapabilityGroup::Simulation
        }
        "autors-util" | "autors-native" | "autors-runtime" => CapabilityGroup::Platform,
        "autors-ffi" | "autors-cli" => CapabilityGroup::Integration,
        _ => CapabilityGroup::Integration,
    }
}

fn workflows(name: &str) -> &'static [&'static str] {
    match name {
        "autors-a2l" => &[
            "Workflows",
            "  Parse/edit/write A2L",
            "  Browse measurements and characteristics",
        ],
        "autors-asc" => &[
            "Workflows",
            "  Read/write CANoe ASC",
            "  Inspect classic CAN and CAN FD traces",
        ],
        "autors-blf" => &[
            "Workflows",
            "  Stream compressed/uncompressed BLF",
            "  Inspect CAN, LIN and Ethernet objects",
        ],
        "autors-can" => &[
            "Workflows",
            "  Discover/configure CAN adapters",
            "  Send, receive and measure bus load",
        ],
        "autors-ccp" => &[
            "Workflows",
            "  Calibration and DAQ over CCP",
            "  ECU memory/programming operations",
        ],
        "autors-cdf" => &["Workflows", "  Read/write calibration exchange data"],
        "autors-comm" => &[
            "Workflows",
            "  Cooperative protocol polling",
            "  Shared DAQ lists and buffers",
        ],
        "autors-datafile" => &[
            "Workflows",
            "  Inspect/edit sparse images",
            "  Convert HEX, S-record, VBF, BIN, UF2 and TI-TXT",
        ],
        "autors-dbc" => &[
            "Workflows",
            "  Browse/edit CAN messages and signals",
            "  Decode raw payloads and validate databases",
        ],
        "autors-dcm" => &[
            "Workflows",
            "  Exchange DCM, PAR and MATLAB calibration sets",
        ],
        "autors-diag" => &[
            "Workflows",
            "  UDS/KWP sessions over CAN or LIN",
            "  DoIP discovery, routing and capture",
        ],
        "autors-elf" => &["Workflows", "  Inspect ELF/DWARF symbols and data types"],
        "autors-ffi" => &["Workflows", "  Consume A2L/value APIs from C and C#"],
        "autors-formula" => &["Workflows", "  Raw/physical conversions and checksums"],
        "autors-isotp" => &["Workflows", "  Segmented diagnostics over CAN and LIN"],
        "autors-ldf" => &[
            "Workflows",
            "  Browse LIN nodes, frames and schedules",
            "  Encode/decode LIN signals",
        ],
        "autors-lin" => &[
            "Workflows",
            "  Discover/configure LIN adapters",
            "  Send headers/responses and inspect traffic",
        ],
        "autors-ltrc" => &["Workflows", "  Inspect PLIN-View Pro LIN traces"],
        "autors-map" => &[
            "Workflows",
            "  Parse linker symbols and synchronize A2L addresses",
        ],
        "autors-mdf" => &["Workflows", "  Read/write MDF 3.x and 4.x measurements"],
        "autors-native" => &[
            "Workflows",
            "  Load native drivers and Seed & Key providers",
        ],
        "autors-odx" => &["Workflows", "  Resolve ODX/PDX diagnostics and flash data"],
        "autors-prm" => &["Workflows", "  Parse and execute PRM/CNF flash procedures"],
        "autors-runtime" => &[
            "Workflows",
            "  Tokio or std-backed async execution",
            "  Shared blocking bridge",
        ],
        "autors-scheduler" => &[
            "Workflows",
            "  DBC/LDF remaining-bus simulation",
            "  Runtime frame overrides and hooks",
        ],
        "autors-symbols" => &["Workflows", "  Match ELF/MAP symbols and write A2L updates"],
        "autors-util" => &[
            "Workflows",
            "  Binary, ranges, timing and logging primitives",
        ],
        "autors-values" => &[
            "Workflows",
            "  Edit scalar/curve/map calibration values",
            "  Read/write ECU memory and CDF",
        ],
        "autors-xcp" => &[
            "Workflows",
            "  Calibration, DAQ and programming over XCP",
            "  CAN, TCP, UDP and serial transports",
        ],
        "autors-cli" => &[
            "Workflows",
            "  Unified terminal capability explorer",
            "  A2L, DBC and ASC engineering workbench",
        ],
        "autors-security-flasher" => &[
            "Workflows",
            "  Discover vehicle flash configurations and plugins",
            "  Execute virtual or hardware-backed UDS programming flows",
        ],
        "autors-security-flasher-sdk" => &[
            "Workflows",
            "  Build versioned file-loader, flow and Seed & Key plugins",
            "  Cross the panic-safe C ABI boundary",
        ],
        _ => &["Workflows", "  See crate documentation for API details"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_workspace_crate_has_a_non_overview_group() {
        for name in [
            "autors-a2l",
            "autors-asc",
            "autors-blf",
            "autors-can",
            "autors-ccp",
            "autors-cdf",
            "autors-cli",
            "autors-comm",
            "autors-datafile",
            "autors-dbc",
            "autors-dcm",
            "autors-diag",
            "autors-elf",
            "autors-ffi",
            "autors-formula",
            "autors-isotp",
            "autors-ldf",
            "autors-lin",
            "autors-ltrc",
            "autors-map",
            "autors-mdf",
            "autors-native",
            "autors-odx",
            "autors-prm",
            "autors-runtime",
            "autors-scheduler",
            "autors-security-flasher",
            "autors-security-flasher-sdk",
            "autors-symbols",
            "autors-util",
            "autors-values",
            "autors-xcp",
        ] {
            assert_ne!(classify(name), CapabilityGroup::Overview, "{name}");
            assert!(workflows(name).len() > 1, "{name}");
        }
    }

    #[test]
    fn loads_the_real_workspace_catalog() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("Cargo.toml");
        let catalog = CapabilityCatalog::load(&manifest).unwrap();
        assert!(catalog.capabilities.len() >= 30);
        assert!(catalog
            .capabilities
            .iter()
            .any(|capability| capability.name == "autors-cli"));
        assert!(catalog
            .capabilities
            .iter()
            .all(|capability| !capability.description.is_empty()));
    }
}
