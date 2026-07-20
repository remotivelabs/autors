use std::fs;
use std::path::{Path, PathBuf};

use autors_a2l::{ModuleChild, Project};
use autors_asc::{AscFile, AscRecord};
use autors_blf::{BlfFile, BlfObject, ObjectRepresentation, ObjectType};
use autors_cdf::cdf::{CdfFile, SwValue};
use autors_datafile::{ChecksumAlgorithm, ChecksumOptions, DataFile, MemorySegmentList};
use autors_dbc::dbc::{DBCFile, MsgType};
use autors_dcm::conservation::{
    DataConservation, DcmFile, MatlabFile, ModuleRefs, ParFile, ValueData,
};
use autors_elf::elf::ElfFile;
use autors_ldf::model::Ldf;
use autors_ltrc::{LtrcFile, Record as LtrcRecord};
use autors_map::map::MapFile;
use autors_mdf::base::MdfReader;
use autors_odx::odx::{OdxFile, VariantKind};
use autors_prm::prm::{CaseKey, CnfFile, CnfSegment, CnfValue, PrmFile, PrmValue};

#[derive(Debug, Clone)]
pub struct Document {
    pub path: PathBuf,
    pub database: Option<DBCFile>,
    pub kind: String,
    pub summary: Vec<(String, String)>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub details: Vec<Vec<String>>,
}

impl Document {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(format!("file does not exist: {}", path.display()));
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "a2l" => Self::open_a2l(path),
            "dbc" => Self::open_dbc(path),
            "asc" => Self::open_asc(path),
            "blf" => Self::open_blf(path),
            "cdf" | "cdfx" => Self::open_cdf(path),
            "cnf" => Self::open_cnf(path),
            "ldf" => Self::open_ldf(path),
            "ltrc" => Self::open_ltrc(path),
            "map" => Self::open_map(path),
            "elf" | "axf" => Self::open_elf(path),
            "dat" | "mdf" => Self::open_mdf(path),
            "hex" | "h86" | "ihex" | "ihx" | "s19" | "s28" | "s37" | "s" | "s1" | "s2" | "s3"
            | "sx" | "srec" | "mot" | "hascii" | "hexascii" | "vbf" | "titxt" | "ti-txt"
            | "ti_txt" | "uf2" | "bin" | "rom" | "img" => Self::open_datafile(path),
            extension if extension.starts_with("odx") => Self::open_odx(path),
            "prm" => Self::open_prm(path),
            _ => Self::open_generic(path),
        }
    }

    pub fn title(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document")
            .to_owned()
    }

    /// Opens an A2L-aware calibration conservation file. Each A2L module is
    /// tried and the module resolving the most imported values is selected.
    pub fn open_conservation(path: &Path, project: &Project) -> Result<Self, String> {
        if !path.is_file() {
            return Err(format!("file does not exist: {}", path.display()));
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut best: Option<(usize, Self)> = None;
        for module in project.modules() {
            let refs = ModuleRefs::new(module);
            let conservation = match extension.as_str() {
                "dcm" => DcmFile::open(path, &refs),
                "par" => ParFile::open(path, &refs),
                "m" => MatlabFile::open(path, &refs),
                _ => return Err(format!("unsupported conservation extension {extension:?}")),
            }
            .map_err(|error| error.to_string())?;
            let score = conservation.values.len();
            let candidate = Self::from_conservation(
                path,
                &extension,
                module.name.as_str(),
                &refs,
                conservation,
            );
            if best.as_ref().is_none_or(|(current, _)| score > *current) {
                best = Some((score, candidate));
            }
        }
        best.map(|(_, document)| document)
            .ok_or_else(|| "the retained A2L project has no modules".to_owned())
    }

    fn from_conservation(
        path: &Path,
        extension: &str,
        module_name: &str,
        refs: &ModuleRefs<'_>,
        conservation: DataConservation<'_>,
    ) -> Self {
        let imported = conservation.values.len();
        let skipped = conservation.skipped_values.len();
        let mut rows = Vec::with_capacity(imported + skipped);
        let mut details = Vec::with_capacity(imported + skipped);
        for value in conservation.values {
            let shape = match &value.value {
                ValueData::Scalar(_) => "scalar".to_owned(),
                ValueData::Text(text) => format!("ASCII[{}]", text.len()),
                ValueData::Array { dims, .. } => dims
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join("×"),
            };
            let physical = match &value.value {
                ValueData::Scalar(_) | ValueData::Text(_) => value.to_single_value(false),
                ValueData::Array { data, .. } => preview_numbers(data),
            };
            let function = refs
                .def_characteristic_function(value.name())
                .map(|function| function.named.name.clone())
                .unwrap_or_else(|| "-".to_owned());
            rows.push(vec![
                module_name.to_owned(),
                value.name().to_owned(),
                format!("{:?}", value.char_type()),
                shape.clone(),
                physical.clone(),
                value.unit.clone(),
                function.clone(),
                "Imported".to_owned(),
            ]);
            let mut lines = vec![
                format!("{}::{}", module_name, value.name()),
                format!("Description   {}", value.description()),
                format!("Type          {:?}", value.char_type()),
                format!("Shape         {shape}"),
                format!("Physical      {physical}"),
                format!("Unit          {}", value.unit),
                format!("Function      {function}"),
                format!("Value format  {:?}", value.value_format),
                format!("Elements      {}", value.value.len()),
            ];
            for (axis, values) in value.axis_values.iter().enumerate() {
                lines.push(format!(
                    "Axis {}        {} {}",
                    axis + 1,
                    preview_numbers(values),
                    value.unit_axis.get(axis).map_or("", String::as_str)
                ));
            }
            details.push(lines);
        }
        for (name, error) in conservation.skipped_values {
            rows.push(vec![
                module_name.to_owned(),
                name.clone(),
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
                error.to_string(),
            ]);
            details.push(vec![
                format!("{}::{name}", module_name),
                "Value was not imported".to_owned(),
                format!("Reason: {error}"),
            ]);
        }
        let format_name = match extension {
            "dcm" => "DAMOS DCM",
            "par" => "CANape PAR",
            "m" => "MATLAB calibration",
            _ => "Calibration conservation",
        };
        Self {
            path: path.to_owned(),
            database: None,
            kind: format!("{format_name} set"),
            summary: vec![
                ("Format".to_owned(), format_name.to_owned()),
                ("A2L module".to_owned(), module_name.to_owned()),
                ("Imported".to_owned(), imported.to_string()),
                ("Skipped".to_owned(), skipped.to_string()),
            ],
            columns: [
                "Module", "Object", "Type", "Shape", "Physical", "Unit", "Function", "Status",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            rows,
            details,
        }
    }

    pub fn is_trace(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "CANoe ASC trace" | "Vector BLF trace" | "PLIN LTRC trace"
        )
    }

    pub fn row_timestamp(&self, index: usize) -> Option<f64> {
        self.rows.get(index)?.first()?.parse().ok()
    }

    pub fn matching_rows(&self, query: &str) -> Vec<usize> {
        let query = query.to_ascii_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                query.is_empty()
                    || row
                        .iter()
                        .any(|cell| cell.to_ascii_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Adds symbolic message names and decoded physical signal values to a CAN
    /// trace using a previously loaded DBC network database.
    pub fn apply_dbc(&mut self, dbc: &DBCFile) -> usize {
        let (id_column, data_column, symbol_column) = match self.kind.as_str() {
            "CANoe ASC trace" => (2, 6, 3),
            "Vector BLF trace" => (3, 6, 4),
            _ => return 0,
        };
        if self.columns.iter().any(|column| column == "Message") {
            return 0;
        }
        self.columns.insert(symbol_column, "Message".to_owned());
        let mut decoded = 0usize;
        for (index, row) in self.rows.iter_mut().enumerate() {
            let Some(raw_id) = row
                .get(id_column)
                .and_then(|value| parse_displayed_can_id(value))
            else {
                row.insert(symbol_column, "-".to_owned());
                continue;
            };
            let data = row
                .get(data_column)
                .map(|value| parse_hex_bytes(value))
                .unwrap_or_default();
            let Some(message) = dbc
                .messages()
                .find(|message| message.id & 0x1fff_ffff == raw_id)
            else {
                row.insert(symbol_column, "-".to_owned());
                continue;
            };
            decoded += 1;
            row.insert(symbol_column, message.name.clone());
            if let Some(detail) = self.details.get_mut(index) {
                detail.push(String::new());
                detail.push(format!("DBC message  {}", message.name));
                detail.extend(decode_dbc_signals(message, &data));
            }
        }
        self.summary
            .push(("DBC matches".to_owned(), decoded.to_string()));
        decoded
    }

    fn open_dbc(path: &Path) -> Result<Self, String> {
        let dbc = DBCFile::parse_file(path).map_err(|error| error.to_string())?;
        let messages = dbc.messages().collect::<Vec<_>>();
        let signal_count = messages
            .iter()
            .map(|message| message.signals.len())
            .sum::<usize>();
        let issues = dbc.plausibility_check();
        let rows = messages
            .iter()
            .map(|message| {
                vec![
                    format!("0x{:X}", message.id),
                    message.name.clone(),
                    message.dlc.to_string(),
                    message.source.clone(),
                    message.signals.len().to_string(),
                ]
            })
            .collect();
        let details = messages
            .iter()
            .map(|message| {
                let mut lines = vec![
                    format!("{} (0x{:X})", message.name, message.id),
                    format!("DLC {} bytes · transmitter {}", message.dlc, message.source),
                ];
                if !message.comment.is_empty() {
                    lines.push(message.comment.clone());
                }
                lines.push(String::new());
                lines.push("Signals".to_owned());
                lines.extend(message.signals.iter().map(|signal| {
                    format!(
                        "  {:<24} bit {:>3}:{:<2}  factor {:>8}  {}",
                        signal.name, signal.start, signal.len, signal.factor, signal.unit
                    )
                }));
                if message.signals.is_empty() {
                    lines.push("  none".to_owned());
                }
                lines
            })
            .collect();
        Ok(Self {
            path: path.to_owned(),
            database: Some(dbc.clone()),
            kind: "CAN database".to_owned(),
            summary: vec![
                ("Version".to_owned(), value_or_dash(&dbc.version)),
                ("Nodes".to_owned(), dbc.sources.len().to_string()),
                ("Messages".to_owned(), messages.len().to_string()),
                ("Signals".to_owned(), signal_count.to_string()),
                ("Environment".to_owned(), dbc.environments.len().to_string()),
                (
                    "Plausibility".to_owned(),
                    format!("{} issue(s)", issues.len()),
                ),
            ],
            columns: ["ID", "Message", "DLC", "Transmitter", "Signals"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows,
            details,
        })
    }

    fn open_asc(path: &Path) -> Result<Self, String> {
        let asc = AscFile::open(path).map_err(|error| error.to_string())?;
        let mut message_count = 0usize;
        let mut error_count = 0usize;
        let mut event_count = 0usize;
        let mut rows = Vec::with_capacity(asc.records.len());
        let mut details = Vec::with_capacity(asc.records.len());
        for record in &asc.records {
            match record {
                AscRecord::Message(message) => {
                    message_count += 1;
                    let identifier = format_can_id(message.id);
                    let data = hex_bytes(&message.data);
                    rows.push(vec![
                        format!("{:.6}", message.timestamp.as_secs_f64()),
                        message.channel.to_string(),
                        identifier.clone(),
                        format!("{:?}", message.direction),
                        format!("{:?}", message.frame_type),
                        message.data.len().to_string(),
                        data.clone(),
                    ]);
                    details.push(vec![
                        format!("CAN frame {identifier}"),
                        format!("Timestamp   {:.6} s", message.timestamp.as_secs_f64()),
                        format!("Channel     {}", message.channel),
                        format!("Direction   {:?}", message.direction),
                        format!("Type        {:?}", message.frame_type),
                        format!("DLC         {}", message.dlc),
                        format!("Payload     {data}"),
                        format!("Remote      {}", message.is_remote),
                        format!("ESI         {}", message.error_state_indicator),
                    ]);
                }
                AscRecord::ErrorFrame(error) => {
                    error_count += 1;
                    rows.push(vec![
                        format!("{:.6}", error.timestamp.as_secs_f64()),
                        error.channel.to_string(),
                        "-".to_owned(),
                        error
                            .direction
                            .map(|value| format!("{value:?}"))
                            .unwrap_or_else(|| "-".to_owned()),
                        "Error".to_owned(),
                        "0".to_owned(),
                        error.metadata.join(" "),
                    ]);
                    details.push(vec![
                        "CAN error frame".to_owned(),
                        format!("Timestamp   {:.6} s", error.timestamp.as_secs_f64()),
                        format!("Channel     {}", error.channel),
                        format!("CAN FD      {}", error.is_fd),
                        format!("Metadata    {}", error.metadata.join(" ")),
                    ]);
                }
                AscRecord::Event(event) => {
                    event_count += 1;
                    rows.push(vec![
                        format!("{:.6}", event.timestamp.as_secs_f64()),
                        "-".to_owned(),
                        "-".to_owned(),
                        "-".to_owned(),
                        "Event".to_owned(),
                        "-".to_owned(),
                        event.text.clone(),
                    ]);
                    details.push(vec![
                        "Trace event".to_owned(),
                        format!("Timestamp   {:.6} s", event.timestamp.as_secs_f64()),
                        event.text.clone(),
                    ]);
                }
            }
        }
        let duration = asc
            .records
            .last()
            .map(|record| format!("{:.6} s", record.timestamp().as_secs_f64()))
            .unwrap_or_else(|| "0 s".to_owned());
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "CANoe ASC trace".to_owned(),
            summary: vec![
                (
                    "Date".to_owned(),
                    asc.header.date.unwrap_or_else(|| "-".to_owned()),
                ),
                (
                    "Timestamp".to_owned(),
                    format!("{:?}", asc.header.timestamp_mode),
                ),
                ("Duration".to_owned(), duration),
                ("Messages".to_owned(), message_count.to_string()),
                ("Errors".to_owned(), error_count.to_string()),
                ("Events".to_owned(), event_count.to_string()),
            ],
            columns: ["Time", "Ch", "ID", "Dir", "Type", "Len", "Data / Event"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows,
            details,
        })
    }

    fn open_a2l(path: &Path) -> Result<Self, String> {
        let project = Project::parse_file(path).map_err(|error| error.to_string())?;
        let modules = project.modules().collect::<Vec<_>>();
        let rows = modules
            .iter()
            .map(|module| {
                let counts = module_counts(&module.children);
                vec![
                    module.name.clone(),
                    counts.measurements.to_string(),
                    counts.characteristics.to_string(),
                    counts.functions.to_string(),
                    counts.conversions.to_string(),
                    counts.layouts.to_string(),
                ]
            })
            .collect();
        let details = modules
            .iter()
            .map(|module| {
                let counts = module_counts(&module.children);
                vec![
                    format!("Module {}", module.name),
                    module.description.clone().unwrap_or_default(),
                    String::new(),
                    format!("Measurements       {}", counts.measurements),
                    format!("Characteristics    {}", counts.characteristics),
                    format!("Axis points        {}", counts.axis_points),
                    format!("Functions/groups   {}", counts.functions),
                    format!("Conversions        {}", counts.conversions),
                    format!("Record layouts     {}", counts.layouts),
                    format!("Type definitions   {}", counts.typedefs),
                    format!("Other blocks       {}", counts.other),
                ]
            })
            .collect();
        let totals = modules
            .iter()
            .fold(ModuleCounts::default(), |mut total, module| {
                total.add(module_counts(&module.children));
                total
            });
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "ASAM A2L project".to_owned(),
            summary: vec![
                ("Project".to_owned(), project.named.name.clone()),
                ("Modules".to_owned(), modules.len().to_string()),
                ("Measurements".to_owned(), totals.measurements.to_string()),
                (
                    "Characteristics".to_owned(),
                    totals.characteristics.to_string(),
                ),
                ("Functions".to_owned(), totals.functions.to_string()),
                ("Conversions".to_owned(), totals.conversions.to_string()),
            ],
            columns: [
                "Module",
                "Measurements",
                "Characteristics",
                "Functions",
                "Conversions",
                "Layouts",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            rows,
            details,
        })
    }

    fn open_ldf(path: &Path) -> Result<Self, String> {
        let ldf = Ldf::read(path).map_err(|error| error.to_string())?;
        let rows = ldf
            .unconditional_frames
            .values()
            .map(|frame| {
                vec![
                    format!("0x{:02X}", frame.id),
                    frame.name.clone(),
                    frame.publisher.clone(),
                    frame.length.to_string(),
                    frame.signals.len().to_string(),
                ]
            })
            .collect();
        let details = ldf
            .unconditional_frames
            .values()
            .map(|frame| {
                let mut lines = vec![
                    format!("{} (0x{:02X})", frame.name, frame.id),
                    format!("Publisher {} · {} byte(s)", frame.publisher, frame.length),
                    String::new(),
                    "Signals".to_owned(),
                ];
                lines.extend(frame.signals.iter().map(|placement| {
                    let signal = ldf.signals.get(&placement.signal);
                    format!(
                        "  {:<24} bit {:>3}  width {:>2}  subscribers {}",
                        placement.signal,
                        placement.bit_offset,
                        signal.map_or(0, |value| value.width),
                        signal
                            .map(|value| list_or_dash(&value.subscribers))
                            .unwrap_or_else(|| "-".to_owned())
                    )
                }));
                if frame.signals.is_empty() {
                    lines.push("  none".to_owned());
                }
                lines
            })
            .collect();
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "LIN Description File".to_owned(),
            summary: vec![
                ("Protocol".to_owned(), format!("{:?}", ldf.protocol_version)),
                ("Language".to_owned(), format!("{:?}", ldf.language_version)),
                ("Baud rate".to_owned(), format!("{} bit/s", ldf.baud_rate)),
                ("Master".to_owned(), ldf.master.name.clone()),
                ("Slaves".to_owned(), ldf.slaves.len().to_string()),
                ("Signals".to_owned(), ldf.signals.len().to_string()),
                (
                    "Frames".to_owned(),
                    ldf.unconditional_frames.len().to_string(),
                ),
                (
                    "Schedules".to_owned(),
                    ldf.schedule_tables.len().to_string(),
                ),
            ],
            columns: ["ID", "Frame", "Publisher", "Len", "Signals"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows,
            details,
        })
    }

    fn open_ltrc(path: &Path) -> Result<Self, String> {
        let trace = LtrcFile::open(path).map_err(|error| error.to_string())?;
        let mut frame_count = 0usize;
        let mut event_count = 0usize;
        let mut error_count = 0usize;
        let mut rows = Vec::with_capacity(trace.records.len());
        let mut details = Vec::with_capacity(trace.records.len());
        for record in &trace.records {
            match record {
                LtrcRecord::Frame(record) => {
                    frame_count += 1;
                    if !record.errors.is_empty() {
                        error_count += 1;
                    }
                    let data = record
                        .data
                        .iter()
                        .map(|byte| {
                            byte.map_or_else(|| "--".to_owned(), |byte| format!("{byte:02X}"))
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    rows.push(vec![
                        format!("{:.6}", record.timestamp.as_secs_f64()),
                        record.index.to_string(),
                        format!("0x{:02X}", record.id),
                        format!("{:?}", record.direction),
                        record.dlc.to_string(),
                        data.clone(),
                        if record.errors.is_empty() {
                            "-".to_owned()
                        } else {
                            format!("{:?}", record.errors)
                        },
                    ]);
                    details.push(vec![
                        format!("LIN frame 0x{:02X}", record.id),
                        format!("Timestamp   {:.6} s", record.timestamp.as_secs_f64()),
                        format!("Direction   {:?}", record.direction),
                        format!(
                            "Checksum    0x{:02X} ({:?})",
                            record.checksum, record.checksum_type
                        ),
                        format!("Payload     {data}"),
                        format!("Errors      {:?}", record.errors),
                    ]);
                }
                LtrcRecord::BusEvent(event) => {
                    event_count += 1;
                    rows.push(vec![
                        format!("{:.6}", event.timestamp.as_secs_f64()),
                        event.index.to_string(),
                        "-".to_owned(),
                        "-".to_owned(),
                        "-".to_owned(),
                        format!("{:?}", event.kind),
                        "-".to_owned(),
                    ]);
                    details.push(vec![
                        "LIN bus event".to_owned(),
                        format!("Timestamp   {:.6} s", event.timestamp.as_secs_f64()),
                        format!("Kind        {:?}", event.kind),
                    ]);
                }
            }
        }
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "PLIN LTRC trace".to_owned(),
            summary: vec![
                ("Version".to_owned(), trace.version.as_str().to_owned()),
                ("Records".to_owned(), trace.records.len().to_string()),
                ("Frames".to_owned(), frame_count.to_string()),
                ("Events".to_owned(), event_count.to_string()),
                ("Frames with error".to_owned(), error_count.to_string()),
            ],
            columns: ["Time", "Index", "ID", "Dir", "Len", "Data / Event", "Error"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows,
            details,
        })
    }

    fn open_blf(path: &Path) -> Result<Self, String> {
        let blf = BlfFile::open(path).map_err(|error| error.to_string())?;
        let registered_raw = blf
            .objects
            .iter()
            .filter(|object| object.representation() == ObjectRepresentation::RegisteredRaw)
            .count();
        let unknown = blf
            .objects
            .iter()
            .filter(|object| object.representation() == ObjectRepresentation::Unknown)
            .count();
        let rows = blf.objects.iter().map(blf_row).collect();
        let details = blf.objects.iter().map(blf_details).collect();
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "Vector BLF trace".to_owned(),
            summary: vec![
                (
                    "Application".to_owned(),
                    format!(
                        "{:?} {}.{}.{}",
                        blf.header.app_id,
                        blf.header.app_major,
                        blf.header.app_minor,
                        blf.header.app_build
                    ),
                ),
                (
                    "Compression".to_owned(),
                    if blf.header.compression == 0 {
                        "none"
                    } else {
                        "zlib"
                    }
                    .to_owned(),
                ),
                ("Objects".to_owned(), blf.objects.len().to_string()),
                (
                    "Object types".to_owned(),
                    blf.statistics().len().to_string(),
                ),
                (
                    "Duration".to_owned(),
                    blf.header
                        .duration()
                        .map(|value| format!("{:.6} s", value.as_secs_f64()))
                        .unwrap_or_else(|| "-".to_owned()),
                ),
                ("Registered raw".to_owned(), registered_raw.to_string()),
                ("Unknown".to_owned(), unknown.to_string()),
            ],
            columns: ["Time", "Object", "Ch", "ID", "Dir", "Len", "Data"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows,
            details,
        })
    }

    fn open_mdf(path: &Path) -> Result<Self, String> {
        let reader = MdfReader::open(path).map_err(|error| error.to_string())?;
        let Some(file) = reader.as_v3() else {
            return Err("unsupported MDF representation".to_owned());
        };
        let mut rows = Vec::new();
        let mut details = Vec::new();
        let mut group_count = 0usize;
        let mut sample_count = 0u64;
        for (data_group_index, data_group) in file.hd_block.dg_blocks.iter().enumerate() {
            for (channel_group_index, channel_group) in data_group.cg_blocks.iter().enumerate() {
                group_count += 1;
                sample_count = sample_count.saturating_add(channel_group.record_count);
                for channel in &channel_group.cn_blocks {
                    let range = if channel.min().is_finite() && channel.max().is_finite() {
                        format!("{}..{}", channel.min(), channel.max())
                    } else {
                        "-".to_owned()
                    };
                    rows.push(vec![
                        channel.name.clone(),
                        format!("DG{} / CG{}", data_group_index + 1, channel_group_index + 1),
                        format!("{:?}", channel.channel_type),
                        format!("{:?}", channel.signal_type),
                        channel.no_of_bits.to_string(),
                        channel.unit().to_owned(),
                        channel_group.record_count.to_string(),
                    ]);
                    details.push(vec![
                        channel.name.clone(),
                        channel.description.clone(),
                        String::new(),
                        format!("Channel type  {:?}", channel.channel_type),
                        format!("Signal type   {:?}", channel.signal_type),
                        format!("Bit width     {}", channel.no_of_bits),
                        format!("Byte offset   {}", channel.add_offset),
                        format!("Unit          {}", value_or_dash(channel.unit())),
                        format!("Range         {range}"),
                        format!("Rate          {}", channel.rate),
                        format!("Records       {}", channel_group.record_count),
                    ]);
                }
            }
        }
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "ASAM MDF measurement".to_owned(),
            summary: vec![
                ("Format".to_owned(), file.id_block.format_id.clone()),
                ("Producer".to_owned(), file.id_block.program_id.clone()),
                ("Project".to_owned(), value_or_dash(&file.hd_block.project)),
                ("Subject".to_owned(), value_or_dash(&file.hd_block.subject)),
                (
                    "Recording".to_owned(),
                    format!("{} {}", file.hd_block.date, file.hd_block.time),
                ),
                (
                    "Data groups".to_owned(),
                    file.hd_block.dg_blocks.len().to_string(),
                ),
                ("Channel groups".to_owned(), group_count.to_string()),
                ("Channels".to_owned(), rows.len().to_string()),
                ("Samples".to_owned(), sample_count.to_string()),
            ],
            columns: [
                "Channel", "Group", "Role", "Type", "Bits", "Unit", "Records",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            rows,
            details,
        })
    }

    fn open_cdf(path: &Path) -> Result<Self, String> {
        let cdf = CdfFile::load(path).map_err(|error| error.to_string())?;
        let instances = cdf.all_instances();
        let rows = instances
            .iter()
            .map(|instance| {
                let container = instance.sw_value_cont.as_ref();
                let dimensions = container
                    .and_then(|value| value.sw_array_size.as_ref())
                    .map(|value| {
                        value
                            .items
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" × ")
                    })
                    .unwrap_or_else(|| "1".to_owned());
                let count = container
                    .and_then(|value| value.sw_values_phys.as_ref())
                    .map(|values| values.items.iter().map(sw_value_count).sum::<usize>())
                    .unwrap_or_default();
                vec![
                    instance
                        .short_name
                        .clone()
                        .unwrap_or_else(|| "-".to_owned()),
                    instance.category.clone().unwrap_or_else(|| "-".to_owned()),
                    container
                        .and_then(|value| value.unit_display_name.clone())
                        .unwrap_or_else(|| "-".to_owned()),
                    dimensions,
                    count.to_string(),
                    instance
                        .sw_feature_ref
                        .clone()
                        .unwrap_or_else(|| "-".to_owned()),
                ]
            })
            .collect();
        let details = instances
            .iter()
            .map(|instance| {
                let container = instance.sw_value_cont.as_ref();
                let values = container
                    .and_then(|value| value.sw_values_phys.as_ref())
                    .map(|values| {
                        values
                            .items
                            .iter()
                            .take(32)
                            .map(sw_value_text)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_else(|| "-".to_owned());
                vec![
                    instance
                        .short_name
                        .clone()
                        .unwrap_or_else(|| "-".to_owned()),
                    instance.long_name.clone().unwrap_or_default(),
                    String::new(),
                    format!("Category   {}", instance.category.as_deref().unwrap_or("-")),
                    format!(
                        "Feature    {}",
                        instance.sw_feature_ref.as_deref().unwrap_or("-")
                    ),
                    format!(
                        "Unit       {}",
                        container
                            .and_then(|value| value.unit_display_name.as_deref())
                            .unwrap_or("-")
                    ),
                    format!("Values     {values}"),
                ]
            })
            .collect();
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "ASAM CDF calibration data".to_owned(),
            summary: vec![
                (
                    "Document".to_owned(),
                    cdf.short_name.clone().unwrap_or_else(|| "-".to_owned()),
                ),
                (
                    "Category".to_owned(),
                    cdf.category.clone().unwrap_or_else(|| "-".to_owned()),
                ),
                (
                    "Creator".to_owned(),
                    format!(
                        "{} {}",
                        cdf.creator.as_deref().unwrap_or("-"),
                        cdf.creator_version.as_deref().unwrap_or("")
                    ),
                ),
                (
                    "Systems".to_owned(),
                    cdf.sw_systems
                        .as_ref()
                        .map_or(0, |systems| systems.items.len())
                        .to_string(),
                ),
                ("Instances".to_owned(), instances.len().to_string()),
            ],
            columns: [
                "Instance",
                "Category",
                "Unit",
                "Dimensions",
                "Values",
                "Feature",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            rows,
            details,
        })
    }

    fn open_odx(path: &Path) -> Result<Self, String> {
        let file = OdxFile::open(path).map_err(|error| error.to_string())?;
        let variants = file.odx.get_variants();
        let rows = variants
            .iter()
            .map(|variant| {
                let name = variant.short_name();
                let layer = file.odx.merged_layer(*variant);
                vec![
                    name.to_owned(),
                    match variant {
                        VariantKind::Base(_) => "Base",
                        VariantKind::Ecu(_) => "ECU",
                    }
                    .to_owned(),
                    file.odx.supported_protocols(*variant).len().to_string(),
                    layer.diag_services.len().to_string(),
                    file.odx.get_dtcs(Some(name)).len().to_string(),
                    layer.comparam_refs.len().to_string(),
                ]
            })
            .collect();
        let details = variants
            .iter()
            .map(|variant| {
                let name = variant.short_name();
                let layer = file.odx.merged_layer(*variant);
                let mut lines = vec![
                    format!("Variant {name}"),
                    format!(
                        "Type        {}",
                        match variant {
                            VariantKind::Base(_) => "Base variant",
                            VariantKind::Ecu(_) => "ECU variant",
                        }
                    ),
                    format!(
                        "Protocols   {}",
                        file.odx
                            .supported_protocols(*variant)
                            .iter()
                            .map(|protocol| protocol.short_name.as_deref().unwrap_or("-"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    format!("DTCs        {}", file.odx.get_dtcs(Some(name)).len()),
                    String::new(),
                    "Diagnostic services".to_owned(),
                ];
                lines.extend(layer.diag_services.iter().take(100).map(|service| {
                    let analysis = file.odx.analyze_service(service);
                    format!(
                        "  {:02X} {:02X}  {}",
                        analysis.sid,
                        analysis.subfunction,
                        service.short_name.as_deref().unwrap_or("-")
                    )
                }));
                if layer.diag_services.is_empty() {
                    lines.push("  none".to_owned());
                }
                lines
            })
            .collect();
        let container = file.odx.diag_layer_container.as_ref();
        let service_count = variants
            .iter()
            .map(|variant| file.odx.merged_layer(*variant).diag_services.len())
            .sum::<usize>();
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "ASAM ODX diagnostics".to_owned(),
            summary: vec![
                (
                    "Model version".to_owned(),
                    file.odx
                        .model_version
                        .clone()
                        .unwrap_or_else(|| "-".to_owned()),
                ),
                (
                    "Container".to_owned(),
                    file.odx.container_id().unwrap_or("-").to_owned(),
                ),
                (
                    "Protocols".to_owned(),
                    container
                        .map_or(0, |value| value.protocols.items.len())
                        .to_string(),
                ),
                ("Variants".to_owned(), variants.len().to_string()),
                ("Services".to_owned(), service_count.to_string()),
                ("DTCs".to_owned(), file.odx.get_dtcs(None).len().to_string()),
                (
                    "Parser events".to_owned(),
                    file.parser_events.len().to_string(),
                ),
                (
                    "Flash data".to_owned(),
                    file.odx.flash.is_some().to_string(),
                ),
            ],
            columns: [
                "Variant",
                "Type",
                "Protocols",
                "Services",
                "DTCs",
                "ComParams",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            rows,
            details,
        })
    }

    fn open_prm(path: &Path) -> Result<Self, String> {
        let prm = PrmFile::open(path).map_err(|error| error.to_string())?;
        let mut rows = Vec::new();
        let mut details = Vec::new();
        let mut append_command_set =
            |scope: &str, set_name: &str, commands: &[autors_prm::prm::PrmCommand]| {
                for (index, command) in commands.iter().enumerate() {
                    let arguments = command
                        .args
                        .iter()
                        .map(prm_value_text)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let next = command
                        .default_target
                        .clone()
                        .unwrap_or_else(|| "-".to_owned());
                    rows.push(vec![
                        scope.to_owned(),
                        set_name.to_owned(),
                        (index + 1).to_string(),
                        command.name.clone(),
                        arguments.clone(),
                        next.clone(),
                    ]);
                    let mut lines = vec![
                        format!("{} / {} / step {}", scope, set_name, index + 1),
                        format!("Command: {}", command.name),
                        format!(
                            "Arguments: {}",
                            if arguments.is_empty() {
                                "-"
                            } else {
                                &arguments
                            }
                        ),
                        format!("Default target: {next}"),
                    ];
                    if !command.cases.is_empty() {
                        lines.push(String::new());
                        lines.push("Branches".to_owned());
                        lines.extend(
                            command.cases.iter().map(|(key, target)| {
                                format!("  {} -> {target}", case_key_text(key))
                            }),
                        );
                    }
                    details.push(lines);
                }
            };
        for (name, command_set) in &prm.cmdsets {
            append_command_set("Main", name, &command_set.commands);
        }
        for (procedure_name, procedure) in &prm.procedures {
            for command_set in &procedure.cmdsets {
                append_command_set(procedure_name, &command_set.name, &command_set.commands);
            }
        }
        let command_sets = prm.cmdsets.len()
            + prm
                .procedures
                .values()
                .map(|procedure| procedure.cmdsets.len())
                .sum::<usize>();
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "INCA ProF flash procedure".to_owned(),
            summary: vec![
                ("Protocol".to_owned(), prm.mode.to_string()),
                (
                    "Project".to_owned(),
                    prm.cnf.project().unwrap_or("-").to_owned(),
                ),
                ("Variables".to_owned(), prm.vars.len().to_string()),
                ("Command sets".to_owned(), command_sets.to_string()),
                ("Procedures".to_owned(), prm.procedures.len().to_string()),
                ("Commands".to_owned(), rows.len().to_string()),
                (
                    "Source segments".to_owned(),
                    prm.cnf.sections.source_mem_areas.len().to_string(),
                ),
                (
                    "Unknown lines".to_owned(),
                    prm.unknown_commands.len().to_string(),
                ),
            ],
            columns: [
                "Scope",
                "Command set",
                "Step",
                "Command",
                "Arguments",
                "Next",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            rows,
            details,
        })
    }

    fn open_map(path: &Path) -> Result<Self, String> {
        let map = MapFile::open(path).map_err(|error| error.to_string())?;
        let mut symbols = map.symbols.values().collect::<Vec<_>>();
        symbols.sort_by_key(|symbol| (symbol.address, symbol.name.as_str()));
        let rows = symbols
            .iter()
            .map(|symbol| {
                vec![
                    symbol.name.clone(),
                    format!("0x{:016X}", symbol.address),
                    if symbol.index == i32::MAX {
                        "-".to_owned()
                    } else {
                        symbol.index.to_string()
                    },
                ]
            })
            .collect::<Vec<_>>();
        let details = symbols
            .iter()
            .map(|symbol| {
                vec![
                    symbol.name.clone(),
                    format!("Address: 0x{:X}", symbol.address),
                    format!(
                        "Trailing array index: {}",
                        if symbol.index == i32::MAX {
                            "none".to_owned()
                        } else {
                            symbol.index.to_string()
                        }
                    ),
                    "This symbol can participate in autors-symbols A2L address synchronization."
                        .to_owned(),
                ]
            })
            .collect();
        let bounds = symbols
            .first()
            .zip(symbols.last())
            .map(|(first, last)| format!("0x{:X}..0x{:X}", first.address, last.address))
            .unwrap_or_else(|| "empty".to_owned());
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "Linker MAP symbols".to_owned(),
            summary: vec![
                ("Symbols".to_owned(), rows.len().to_string()),
                ("Address bounds".to_owned(), bounds),
                (
                    "A2L workflow".to_owned(),
                    "address synchronization".to_owned(),
                ),
            ],
            columns: ["Symbol", "Address", "Index"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows,
            details,
        })
    }

    fn open_elf(path: &Path) -> Result<Self, String> {
        let elf = ElfFile::open(path, false).map_err(|error| error.to_string())?;
        let mut rows = Vec::with_capacity(elf.sections.len() + elf.symbols.len());
        let mut details = Vec::with_capacity(rows.capacity());
        for section in &elf.sections {
            rows.push(vec![
                "Section".to_owned(),
                section.name.clone(),
                format!("0x{:016X}", section.section.adr),
                section.section.size.to_string(),
                format!("{:?}", section.section.section_type()),
                format!("{:?}", section.section.flags),
            ]);
            details.push(vec![
                format!("Section {}", section.name),
                format!("Address: 0x{:X}", section.section.adr),
                format!("File offset: 0x{:X}", section.section.ofs),
                format!("Size: {} byte(s)", section.section.size),
                format!("Type: {:?}", section.section.section_type()),
                format!("Flags: {:?}", section.section.flags),
                format!("Alignment: {}", section.section.adr_align),
            ]);
        }
        let mut symbols = elf.symbols.values().collect::<Vec<_>>();
        symbols.sort_by_key(|symbol| (symbol.symbol.value, symbol.name.as_str()));
        for symbol in symbols {
            rows.push(vec![
                "Symbol".to_owned(),
                symbol.name.clone(),
                format!("0x{:016X}", symbol.symbol.value),
                symbol.symbol.size.to_string(),
                format!("{:?}", symbol.symbol.symbol_type()),
                format!("{:?}", symbol.symbol.symbol_bind()),
            ]);
            details.push(vec![
                format!("Symbol {}", symbol.name),
                format!("Address: 0x{:X}", symbol.symbol.value),
                format!("Size: {} byte(s)", symbol.symbol.size),
                format!("Type: {:?}", symbol.symbol.symbol_type()),
                format!("Binding: {:?}", symbol.symbol.symbol_bind()),
                format!("Section index: {}", symbol.symbol.shndx),
                "This symbol can participate in autors-symbols A2L address synchronization."
                    .to_owned(),
            ]);
        }
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "ELF program and symbols".to_owned(),
            summary: vec![
                ("Class".to_owned(), format!("{:?}", elf.header.format)),
                (
                    "Endianness".to_owned(),
                    format!("{:?}", elf.header.endianness),
                ),
                (
                    "Machine".to_owned(),
                    format!("{:?}", elf.header.machine_type()),
                ),
                (
                    "File type".to_owned(),
                    format!("{:?}", elf.header.type_type()),
                ),
                (
                    "Entry point".to_owned(),
                    format!("0x{:X}", elf.header.proc_address),
                ),
                ("Sections".to_owned(), elf.sections.len().to_string()),
                ("Symbols".to_owned(), elf.symbols.len().to_string()),
                (
                    "DWARF".to_owned(),
                    "available through autors-elf".to_owned(),
                ),
            ],
            columns: ["Kind", "Name", "Address", "Bytes", "Type", "Flags / Bind"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows,
            details,
        })
    }

    fn open_datafile(path: &Path) -> Result<Self, String> {
        let (file, detected) = DataFile::open_auto(path, MemorySegmentList::new(), 0, None)
            .map_err(|error| error.to_string())?;
        let image = &file.base().segment_list;
        let summary = image.image_summary().map_err(|error| error.to_string())?;
        let checksum = file
            .base()
            .checksum(ChecksumAlgorithm::Crc32IsoHdlc, &ChecksumOptions::default())
            .map_err(|error| error.to_string())?;
        let mut rows = Vec::new();
        let mut details = Vec::new();
        for (index, segment) in image.segments.iter().enumerate() {
            let end = segment.address.saturating_add(segment.size() as u64);
            let preview = hex_bytes(&segment.data()[..segment.size().min(16)]);
            rows.push(vec![
                "Segment".to_owned(),
                format!("0x{:016X}", segment.address),
                format!("0x{end:016X}"),
                segment.size().to_string(),
                format!("{:?}", segment.prg_type),
                preview.clone(),
            ]);
            let tail_start = segment.size().saturating_sub(16);
            details.push(vec![
                format!("Segment {}", index + 1),
                format!("Address: 0x{:X}..0x{end:X}", segment.address),
                format!("Size: {} byte(s)", segment.size()),
                format!("Usage: {:?}", segment.prg_type),
                format!("Initialized: {}", segment.is_initialized()),
                format!("First bytes: {preview}"),
                format!("Last bytes: {}", hex_bytes(&segment.data()[tail_start..])),
            ]);
        }
        for hole in &summary.holes {
            rows.push(vec![
                "Hole".to_owned(),
                format!("0x{:016X}", hole.start),
                format!("0x{:016X}", hole.end),
                hole.len().to_string(),
                "uninitialized".to_owned(),
                "-".to_owned(),
            ]);
            details.push(vec![
                "Address-space hole".to_owned(),
                format!("Address: 0x{:X}..0x{:X}", hole.start, hole.end),
                format!("Size: {} byte(s)", hole.len()),
                "No image bytes are initialized in this range.".to_owned(),
            ]);
        }
        let address_range = summary
            .address_range
            .map(|range| format!("0x{:X}..0x{:X}", range.start, range.end))
            .unwrap_or_else(|| "empty".to_owned());
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "ECU program image".to_owned(),
            summary: vec![
                ("Format".to_owned(), detected.format.name().to_owned()),
                (
                    "Confidence".to_owned(),
                    format!("{:?}", detected.confidence),
                ),
                ("Detection".to_owned(), detected.reason.to_owned()),
                ("Segments".to_owned(), summary.segment_count.to_string()),
                (
                    "Initialized".to_owned(),
                    format!("{} bytes", summary.byte_count),
                ),
                ("Address range".to_owned(), address_range),
                ("Holes".to_owned(), summary.holes.len().to_string()),
                ("CRC-32".to_owned(), format!("0x{:08X}", checksum.value)),
            ],
            columns: ["Kind", "Start", "End", "Bytes", "Usage", "Preview"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows,
            details,
        })
    }

    fn open_cnf(path: &Path) -> Result<Self, String> {
        let cnf = CnfFile::open(path).map_err(|error| error.to_string())?;
        let mut rows = cnf
            .values
            .iter()
            .map(|(key, values)| {
                vec![
                    "Parameter".to_owned(),
                    key.clone(),
                    cnf_values_text(values),
                    "-".to_owned(),
                    "-".to_owned(),
                    "-".to_owned(),
                ]
            })
            .collect::<Vec<_>>();
        let mut details = cnf
            .values
            .iter()
            .map(|(key, values)| vec![key.clone(), cnf_values_text(values)])
            .collect::<Vec<_>>();
        let mut append_segments = |kind: &str, segments: &[CnfSegment]| {
            for segment in segments {
                rows.push(vec![
                    kind.to_owned(),
                    segment.index.to_string(),
                    "-".to_owned(),
                    format!("0x{:08X}", segment.start),
                    format!("0x{:08X}", segment.end),
                    segment.byte_len().to_string(),
                ]);
                details.push(vec![
                    format!("{kind} segment {}", segment.index),
                    format!("Start: 0x{:08X}", segment.start),
                    format!("End: 0x{:08X}", segment.end),
                    format!("Length: {} byte(s)", segment.byte_len()),
                    format!(
                        "Program data: {}",
                        segment
                            .data
                            .as_ref()
                            .map(|data| format!("{} byte(s) loaded", data.len()))
                            .unwrap_or_else(|| "not loaded".to_owned())
                    ),
                ]);
            }
        };
        append_segments("Source", &cnf.sections.source_mem_areas);
        append_segments("Erase", &cnf.sections.erase_mem_areas);
        append_segments("Destination", &cnf.sections.dest_mem_areas);
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: "INCA ProF controller configuration".to_owned(),
            summary: vec![
                (
                    "Project".to_owned(),
                    cnf.project().unwrap_or("-").to_owned(),
                ),
                (
                    "ECU address".to_owned(),
                    cnf.ecu_address()
                        .map(|value| format!("0x{value:04X}"))
                        .unwrap_or_else(|_| "-".to_owned()),
                ),
                (
                    "CAN command ID".to_owned(),
                    cnf.cmd_id()
                        .map(|value| format!("0x{value:X}"))
                        .unwrap_or_else(|_| "-".to_owned()),
                ),
                (
                    "CAN response ID".to_owned(),
                    cnf.rsp_id()
                        .map(|value| format!("0x{value:X}"))
                        .unwrap_or_else(|_| "-".to_owned()),
                ),
                (
                    "Bus rate".to_owned(),
                    cnf.baudrate()
                        .map(|value| format!("{value} bit/s"))
                        .unwrap_or_else(|_| "-".to_owned()),
                ),
                (
                    "Memory segments".to_owned(),
                    (cnf.sections.source_mem_areas.len()
                        + cnf.sections.erase_mem_areas.len()
                        + cnf.sections.dest_mem_areas.len())
                    .to_string(),
                ),
            ],
            columns: ["Kind", "Key / Index", "Value", "Start", "End", "Bytes"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows,
            details,
        })
    }

    fn open_generic(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|error| error.to_string())?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("file")
            .to_ascii_uppercase();
        let (columns, rows, details, encoding) = match String::from_utf8(bytes.clone()) {
            Ok(text) => {
                let rows = text
                    .lines()
                    .enumerate()
                    .map(|(index, line)| vec![(index + 1).to_string(), line.to_owned()])
                    .collect::<Vec<_>>();
                let details = rows
                    .iter()
                    .map(|row| vec![format!("Line {}", row[0]), row[1].clone()])
                    .collect();
                (
                    vec!["Line".to_owned(), "Text".to_owned()],
                    rows,
                    details,
                    "UTF-8",
                )
            }
            Err(_) => {
                let rows = bytes
                    .chunks(16)
                    .enumerate()
                    .map(|(index, chunk)| {
                        vec![
                            format!("{:08X}", index * 16),
                            hex_bytes(chunk),
                            chunk
                                .iter()
                                .map(|byte| {
                                    if byte.is_ascii_graphic() {
                                        char::from(*byte)
                                    } else {
                                        '.'
                                    }
                                })
                                .collect(),
                        ]
                    })
                    .collect::<Vec<_>>();
                let details = rows
                    .iter()
                    .map(|row| {
                        vec![
                            format!("Offset 0x{}", row[0]),
                            row[1].clone(),
                            row[2].clone(),
                        ]
                    })
                    .collect();
                (
                    vec!["Offset".to_owned(), "Hex".to_owned(), "ASCII".to_owned()],
                    rows,
                    details,
                    "binary",
                )
            }
        };
        Ok(Self {
            path: path.to_owned(),
            database: None,
            kind: format!("{extension} file"),
            summary: vec![
                ("Format".to_owned(), extension),
                ("Size".to_owned(), format!("{} bytes", bytes.len())),
                ("Encoding".to_owned(), encoding.to_owned()),
                ("Rows".to_owned(), rows.len().to_string()),
            ],
            columns,
            rows,
            details,
        })
    }
}

fn value_or_dash(value: &str) -> String {
    if value.is_empty() {
        "-".to_owned()
    } else {
        value.to_owned()
    }
}

fn prm_value_text(value: &PrmValue) -> String {
    match value {
        PrmValue::Int(value) => value.to_string(),
        PrmValue::UInt(value) => value.to_string(),
        PrmValue::Str(value) => format!("{value:?}"),
    }
}

fn case_key_text(value: &CaseKey) -> String {
    match value {
        CaseKey::Int(value) => value.to_string(),
        CaseKey::Str(value) => value.clone(),
    }
}

fn cnf_values_text(values: &[CnfValue]) -> String {
    values
        .iter()
        .map(|value| match value {
            CnfValue::Int(value) => value.to_string(),
            CnfValue::Str(value) => value.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn list_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_owned()
    } else {
        values.join(", ")
    }
}

fn sw_value_count(value: &SwValue) -> usize {
    match value {
        SwValue::V(_) | SwValue::Vt(_) => 1,
        SwValue::Vg(group) => group.vs.iter().map(sw_value_count).sum(),
    }
}

fn sw_value_text(value: &SwValue) -> String {
    match value {
        SwValue::V(value) => value.to_string(),
        SwValue::Vt(value) => value.clone(),
        SwValue::Vg(group) => {
            let values = group
                .vs
                .iter()
                .map(sw_value_text)
                .collect::<Vec<_>>()
                .join(", ");
            match &group.label {
                Some(label) => format!("{label}: [{values}]"),
                None => format!("[{values}]"),
            }
        }
    }
}

fn blf_row(object: &BlfObject) -> Vec<String> {
    let type_name = ObjectType::from_raw(object.object_type())
        .map(|value| format!("{value:?}"))
        .unwrap_or_else(|| format!("Unknown({})", object.object_type()));
    let (channel, identifier, direction, length, data) = blf_bus_fields(object);
    vec![
        format!("{:.6}", object.header().timestamp_seconds()),
        type_name,
        channel,
        identifier,
        direction,
        length,
        data,
    ]
}

fn blf_details(object: &BlfObject) -> Vec<String> {
    let row = blf_row(object);
    vec![
        row[1].clone(),
        format!("Timestamp       {} s", row[0]),
        format!("Channel         {}", row[2]),
        format!("Identifier      {}", row[3]),
        format!("Direction       {}", row[4]),
        format!("Payload length  {}", row[5]),
        format!("Payload         {}", row[6]),
        format!("Representation  {:?}", object.representation()),
        format!("Object size     {} bytes", object.header().base.object_size),
    ]
}

fn blf_bus_fields(object: &BlfObject) -> (String, String, String, String, String) {
    match object {
        BlfObject::CanMessage(message) => (
            message.channel.to_string(),
            format_can_id(message.id),
            format!("{:?}", message.flags),
            message.dlc.to_string(),
            hex_bytes(&message.data[..usize::from(message.dlc).min(message.data.len())]),
        ),
        BlfObject::CanMessage2(message) => (
            message.channel.to_string(),
            format_can_id(message.id),
            format!("{:?}", message.flags),
            message.dlc.to_string(),
            hex_bytes(&message.data[..usize::from(message.dlc).min(message.data.len())]),
        ),
        BlfObject::CanFdMessage(message) => (
            message.channel.to_string(),
            format_can_id(message.id),
            format!("{:?}", message.flags),
            message.valid_data_bytes.to_string(),
            hex_bytes(
                &message.data[..usize::from(message.valid_data_bytes).min(message.data.len())],
            ),
        ),
        BlfObject::CanFdMessage64(message) => (
            message.channel.to_string(),
            format_can_id(message.id),
            message.dir.to_string(),
            message.valid_data_bytes.to_string(),
            hex_bytes(&message.data),
        ),
        BlfObject::LinMessage(message) => (
            message.channel.to_string(),
            format!("0x{:02X}", message.id),
            message.dir.to_string(),
            message.dlc.to_string(),
            hex_bytes(&message.data[..usize::from(message.dlc).min(message.data.len())]),
        ),
        BlfObject::LinMessage2(message) => (
            message.channel.to_string(),
            format!("0x{:02X}", message.id),
            message.dir.to_string(),
            message.dlc.to_string(),
            hex_bytes(&message.data[..usize::from(message.dlc).min(message.data.len())]),
        ),
        _ => (
            "-".to_owned(),
            "-".to_owned(),
            "-".to_owned(),
            object
                .opaque_data()
                .map(|data| data.len().to_string())
                .unwrap_or_else(|| "-".to_owned()),
            object
                .opaque_data()
                .map(hex_bytes)
                .unwrap_or_else(|| "-".to_owned()),
        ),
    }
}

fn format_can_id(id: u32) -> String {
    const EXTENDED_FLAG: u32 = 0x8000_0000;
    const ID_MASK: u32 = 0x1fff_ffff;
    if id & EXTENDED_FLAG != 0 {
        format!("{:08X}x", id & ID_MASK)
    } else {
        format!("{:03X}", id & ID_MASK)
    }
}

fn parse_displayed_can_id(value: &str) -> Option<u32> {
    let value = value
        .trim()
        .strip_prefix("0x")
        .unwrap_or(value.trim())
        .trim_end_matches(['x', 'X']);
    u32::from_str_radix(value, 16)
        .ok()
        .map(|id| id & 0x1fff_ffff)
}

fn parse_hex_bytes(value: &str) -> Vec<u8> {
    value
        .split_whitespace()
        .filter_map(|byte| u8::from_str_radix(byte, 16).ok())
        .collect()
}

fn decode_dbc_signals(message: &MsgType, data: &[u8]) -> Vec<String> {
    let selector = message
        .get_multiplex_signal()
        .and_then(|signal| signal.extract_raw(data));
    let mut lines = message
        .signals
        .iter()
        .filter(|signal| {
            !signal.is_multiplexed()
                || selector.is_some_and(|value| value == i64::from(signal.multiplex_value))
        })
        .filter_map(|signal| {
            let raw = signal.extract_raw(data)?;
            let physical = signal.to_physical(raw);
            let value = signal
                .enums
                .as_ref()
                .and_then(|values| values.get(&raw))
                .cloned()
                .unwrap_or_else(|| format!("{physical:.6}"));
            Some(format!(
                "  {:<24} {:>14} {}  (raw {raw})",
                signal.name, value, signal.unit
            ))
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push("  no decodable signals".to_owned());
    }
    lines
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn preview_numbers(values: &[f64]) -> String {
    let mut preview = values
        .iter()
        .take(8)
        .map(|value| format!("{value:.6}"))
        .collect::<Vec<_>>()
        .join(", ");
    if values.len() > 8 {
        preview.push_str(&format!(" … (+{})", values.len() - 8));
    }
    preview
}

#[derive(Debug, Clone, Copy, Default)]
struct ModuleCounts {
    measurements: usize,
    characteristics: usize,
    axis_points: usize,
    functions: usize,
    conversions: usize,
    layouts: usize,
    typedefs: usize,
    other: usize,
}

impl ModuleCounts {
    fn add(&mut self, other: Self) {
        self.measurements += other.measurements;
        self.characteristics += other.characteristics;
        self.axis_points += other.axis_points;
        self.functions += other.functions;
        self.conversions += other.conversions;
        self.layouts += other.layouts;
        self.typedefs += other.typedefs;
        self.other += other.other;
    }
}

fn module_counts(children: &[ModuleChild]) -> ModuleCounts {
    let mut counts = ModuleCounts::default();
    for child in children {
        match child {
            ModuleChild::Measurement(_) | ModuleChild::Blob(_) | ModuleChild::Instance(_) => {
                counts.measurements += 1
            }
            ModuleChild::Characteristic(_) => counts.characteristics += 1,
            ModuleChild::AxisPts(_) => counts.axis_points += 1,
            ModuleChild::Function(_) | ModuleChild::Group(_) | ModuleChild::Frame(_) => {
                counts.functions += 1
            }
            ModuleChild::CompuMethod(_)
            | ModuleChild::CompuTab(_)
            | ModuleChild::CompuVtab(_)
            | ModuleChild::CompuVtabRange(_)
            | ModuleChild::Formula(_)
            | ModuleChild::Unit(_) => counts.conversions += 1,
            ModuleChild::RecordLayout(_) => counts.layouts += 1,
            ModuleChild::TypedefAxis(_)
            | ModuleChild::TypedefBlob(_)
            | ModuleChild::TypedefCharacteristic(_)
            | ModuleChild::TypedefMeasurement(_)
            | ModuleChild::TypedefStructure(_) => counts.typedefs += 1,
            _ => counts.other += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_dbc_into_message_and_signal_views() {
        let path = std::env::temp_dir().join(format!("autors-cli-{}.dbc", std::process::id()));
        fs::write(
            &path,
            "VERSION \"1\"\nNS_ :\nBS_:\nBU_: ECU\nBO_ 291 Status: 8 ECU\n SG_ Speed : 0|16@1+ (0.1,0) [0|250] \"km/h\" ECU\n",
        )
        .unwrap();
        let document = Document::open(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(document.kind, "CAN database");
        assert_eq!(document.rows.len(), 1);
        assert!(document.details[0]
            .iter()
            .any(|line| line.contains("Speed")));
    }

    #[test]
    fn filters_rows_across_all_cells_case_insensitively() {
        let document = Document {
            path: PathBuf::new(),
            database: None,
            kind: String::new(),
            summary: Vec::new(),
            columns: Vec::new(),
            rows: vec![vec!["123".to_owned(), "VehicleSpeed".to_owned()]],
            details: Vec::new(),
        };
        assert_eq!(document.matching_rows("speed"), vec![0]);
        assert!(document.matching_rows("missing").is_empty());
    }

    #[test]
    fn applies_dbc_names_and_physical_values_to_asc_trace() {
        let dbc = DBCFile::parse_str(
            "VERSION \"1\"\nNS_ :\nBS_:\nBU_: ECU\nBO_ 291 Status: 8 ECU\n SG_ Speed : 0|16@1+ (0.1,0) [0|250] \"km/h\" ECU\n",
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!("autors-cli-{}.asc", std::process::id()));
        fs::write(
            &path,
            "base hex timestamps absolute\n0.001000 1 123 Rx d 2 64 00\n",
        )
        .unwrap();
        let mut document = Document::open(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(document.apply_dbc(&dbc), 1);
        assert_eq!(document.rows[0][3], "Status");
        assert!(document.details[0]
            .iter()
            .any(|line| line.contains("10.000000 km/h")));
    }

    #[test]
    fn opens_ltrc_into_lin_frames_and_bus_events() {
        let path = std::env::temp_dir().join(format!("autors-cli-{}.ltrc", std::process::id()));
        fs::write(
            &path,
            ";$FILEVERSION=1.2\n;$STARTTIME=44824.0\n1) 100 Pub 01 2 10 20 3E EH\n2) 200 --- -- - Bus Sleep -- --\n",
        )
        .unwrap();
        let document = Document::open(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(document.kind, "PLIN LTRC trace");
        assert_eq!(document.rows.len(), 2);
        assert_eq!(document.rows[0][2], "0x01");
        assert!(document.rows[1][5].contains("BusSleep"));
    }

    #[test]
    fn opens_cdf_into_calibration_instances() {
        let path = std::env::temp_dir().join(format!("autors-cli-{}.cdfx", std::process::id()));
        fs::write(
            &path,
            "<MSRSW CREATOR=\"test\"><SHORT-NAME>Demo</SHORT-NAME><SW-SYSTEMS><SW-SYSTEM><SW-INSTANCE-SPEC><SW-INSTANCE-TREE><SW-INSTANCE><SHORT-NAME>SpeedLimit</SHORT-NAME><CATEGORY>VALUE</CATEGORY><SW-VALUE-CONT><UNIT-DISPLAY-NAME>km/h</UNIT-DISPLAY-NAME><SW-VALUES-PHYS><V>120</V></SW-VALUES-PHYS></SW-VALUE-CONT></SW-INSTANCE></SW-INSTANCE-TREE></SW-INSTANCE-SPEC></SW-SYSTEM></SW-SYSTEMS></MSRSW>",
        )
        .unwrap();
        let document = Document::open(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(document.kind, "ASAM CDF calibration data");
        assert_eq!(document.rows[0][0], "SpeedLimit");
        assert_eq!(document.rows[0][2], "km/h");
    }

    #[test]
    fn opens_prm_and_cnf_into_flash_project_views() {
        let directory =
            std::env::temp_dir().join(format!("autors-cli-prm-document-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let cnf_path = directory.join("config.cnf");
        fs::write(
            &cnf_path,
            "PROJECT_NAME: Demo flash\nECU_ADDR: 0x7C\nKWP_CAN_BUS_TIMING: 500000\nINCA_TO_ECU_CAN_ID: 0x700\nECU_TO_INCA_CAN_ID: 0x708\nSOURCE_MEM_AREA: 1,0,0,0x10000L,0x1000FL\n",
        )
        .unwrap();
        let prm_path = directory.join("flash.prm");
        fs::write(
            &prm_path,
            "#define CONFIG config.cnf\n[BOOT]\nUDSX_PROGRAM_MEMORY(0, 1, 2, 3, \"fmt\")\ndefault : BOOT\n[BOOT_END]\n",
        )
        .unwrap();

        let cnf = Document::open(&cnf_path).unwrap();
        let prm = Document::open(&prm_path).unwrap();
        fs::remove_dir_all(directory).unwrap();

        assert_eq!(cnf.kind, "INCA ProF controller configuration");
        assert!(cnf.rows.iter().any(|row| row[0] == "Source"));
        assert_eq!(prm.kind, "INCA ProF flash procedure");
        assert_eq!(prm.rows[0][3], "UDSX_PROGRAM_MEMORY");
        assert!(prm
            .summary
            .iter()
            .any(|(key, value)| key == "Protocol" && value == "UDS"));
    }

    #[test]
    fn opens_a2l_aware_dcm_values_and_selects_the_matching_module() {
        let project = Project::parse_str(
            r#"
/begin PROJECT Demo "demo"
 /begin MODULE Empty "empty"
 /end MODULE
 /begin MODULE ECU "ecu"
  /begin RECORD_LAYOUT RL FNC_VALUES 1 UWORD ROW_DIR DIRECT /end RECORD_LAYOUT
  /begin CHARACTERISTIC Gain "gain" VALUE 0x2000 RL 0 NO_COMPU_METHOD 0 100 /end CHARACTERISTIC
 /end MODULE
/end PROJECT
"#,
        )
        .unwrap();
        let directory =
            std::env::temp_dir().join(format!("autors-cli-dcm-document-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("values.dcm");
        std::fs::write(
            &path,
            "KONSERVIERUNG_FORMAT 2.0\nFESTWERT Gain\n WERT 12.5\nEND\n",
        )
        .unwrap();
        let document = Document::open_conservation(&path, &project).unwrap();
        assert_eq!(document.kind, "DAMOS DCM set");
        assert_eq!(
            document.summary[1],
            ("A2L module".to_owned(), "ECU".to_owned())
        );
        assert_eq!(document.rows[0][1], "Gain");
        assert_eq!(document.rows[0][4], "12.5");
        assert_eq!(document.rows[0][7], "Imported");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn opens_sparse_program_images_with_detection_and_checksum() {
        let path =
            std::env::temp_dir().join(format!("autors-cli-datafile-{}.hex", std::process::id()));
        fs::write(&path, ":0400100001020304E2\n:00000001FF\n").unwrap();
        let document = Document::open(&path).unwrap();
        fs::remove_file(path).unwrap();

        assert_eq!(document.kind, "ECU program image");
        assert_eq!(document.rows[0][0], "Segment");
        assert_eq!(document.rows[0][1], "0x0000000000000010");
        assert!(document
            .summary
            .iter()
            .any(|(key, value)| key == "Format" && value == "Intel HEX"));
        assert!(document
            .summary
            .iter()
            .any(|(key, value)| { key == "CRC-32" && value.starts_with("0x") }));
    }

    #[test]
    fn opens_linker_map_symbols_for_a2l_synchronization() {
        let path =
            std::env::temp_dir().join(format!("autors-cli-symbols-{}.map", std::process::id()));
        fs::write(
            &path,
            "0x00001000 VehicleSpeed\nCalibrationMap 00002000\n0x00002004 CalibrationMap0\n",
        )
        .unwrap();
        let document = Document::open(&path).unwrap();
        fs::remove_file(path).unwrap();

        assert_eq!(document.kind, "Linker MAP symbols");
        assert!(document
            .rows
            .iter()
            .any(|row| { row[0] == "VehicleSpeed" && row[1] == "0x0000000000001000" }));
        assert!(document
            .summary
            .iter()
            .any(|(key, value)| key == "A2L workflow" && value == "address synchronization"));
    }
}
