use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::characteristic::Characteristic;
use autors_a2l::model::compu::{CompuMethod, CompuTab, CompuVtab, CompuVtabRange};
use autors_a2l::model::enums::{CalibrationAccess, CharacteristicType, DataType};
use autors_a2l::model::measurement::Measurement;
use autors_a2l::model::module::Module;
use autors_a2l::{ModuleChild, Project};
use autors_comm::base::{
    data_type_size_in_byte, DaqDict, DaqList, DaqMeasurement, MeasurementInfo,
};
use autors_mdf::base::{MdfWriter, TimeQualityType};
use autors_mdf::v3::HdRecordingInfo;
use autors_values::value::{CompuTabRef, Conversion, MeasurementAccessData};

const SAMPLE_PERIOD: Duration = Duration::from_millis(100);
const RECENT_LIMIT: usize = 32;
const RECORDING_LIMIT: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A2lViewMode {
    Measurements,
    Calibrations,
    All,
}

impl A2lViewMode {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Measurements => "Measurements",
            Self::Calibrations => "Calibrations",
            Self::All => "All A2L objects",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Measurements => Self::Calibrations,
            Self::Calibrations => Self::All,
            Self::All => Self::Measurements,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A2lObjectKind {
    Measurement,
    Calibration,
}

impl A2lObjectKind {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Measurement => "Measurement",
            Self::Calibration => "Calibration",
        }
    }
}

#[derive(Debug, Clone)]
enum OwnedCompuTab {
    Tab(CompuTab),
    Vtab(CompuVtab),
    VtabRange(CompuVtabRange),
}

#[derive(Debug, Clone)]
struct OwnedConversion {
    method: CompuMethod,
    tab: Option<OwnedCompuTab>,
}

impl OwnedConversion {
    fn identity() -> Self {
        Self {
            method: CompuMethod::default(),
            tab: None,
        }
    }

    fn from_module(module: &Module, name: &str) -> Self {
        let Some(method) = module.children.iter().find_map(|child| match child {
            ModuleChild::CompuMethod(method) if method.name == name => Some(method.clone()),
            _ => None,
        }) else {
            return Self::identity();
        };
        let tab = method.compu_tab_ref.as_deref().and_then(|reference| {
            module.children.iter().find_map(|child| match child {
                ModuleChild::CompuTab(tab) if tab.base.name == reference => {
                    Some(OwnedCompuTab::Tab(tab.clone()))
                }
                ModuleChild::CompuVtab(tab) if tab.base.name == reference => {
                    Some(OwnedCompuTab::Vtab(tab.clone()))
                }
                ModuleChild::CompuVtabRange(tab) if tab.base.name == reference => {
                    Some(OwnedCompuTab::VtabRange(tab.clone()))
                }
                _ => None,
            })
        });
        Self { method, tab }
    }

    fn view(&self) -> Conversion<'_> {
        match &self.tab {
            Some(OwnedCompuTab::Tab(tab)) => {
                Conversion::with_tab(&self.method, CompuTabRef::Tab(tab))
            }
            Some(OwnedCompuTab::Vtab(tab)) => {
                Conversion::with_tab(&self.method, CompuTabRef::Vtab(tab))
            }
            Some(OwnedCompuTab::VtabRange(tab)) => {
                Conversion::with_tab(&self.method, CompuTabRef::VtabRange(tab))
            }
            None => Conversion::new(&self.method),
        }
    }
}

#[derive(Debug, Clone)]
enum A2lSource {
    Measurement(Measurement),
    Characteristic(Characteristic),
}

#[derive(Debug, Clone)]
struct A2lObject {
    kind: A2lObjectKind,
    module: String,
    name: String,
    description: String,
    address: Option<u32>,
    data_type: DataType,
    shape: String,
    unit: String,
    lower: f64,
    upper: f64,
    writable: bool,
    source: A2lSource,
    conversion: OwnedConversion,
    raw_calibration_value: f64,
    current_physical_value: Option<f64>,
    armed: bool,
    daq_info: Option<MeasurementInfo>,
    recent: VecDeque<(f64, f64)>,
    recorded: VecDeque<(f64, f64)>,
    dropped_samples: usize,
}

#[derive(Debug, Clone)]
pub struct A2lObjectView {
    pub kind: A2lObjectKind,
    pub module: String,
    pub name: String,
    pub address: String,
    pub data_type: String,
    pub shape: String,
    pub unit: String,
    pub value: String,
    pub writable: bool,
    pub armed: bool,
    pub samples: usize,
}

/// A CANoe-style measurement and calibration model backed by A2L metadata,
/// `autors-values` conversion/memory access, and `autors-comm` DAQ buffers.
pub struct A2lLab {
    pub project_name: String,
    pub module_count: usize,
    objects: Vec<A2lObject>,
    memory: Option<MeasurementAccessData>,
    daq: DaqDict,
    pub view_mode: A2lViewMode,
    pub running: bool,
    pub elapsed: Duration,
    sample_accumulator: Duration,
    pub unplaced_measurements: usize,
    pub last_error: Option<String>,
}

impl Default for A2lLab {
    fn default() -> Self {
        Self::new()
    }
}

impl A2lLab {
    pub fn new() -> Self {
        Self {
            project_name: "No A2L loaded".to_owned(),
            module_count: 0,
            objects: Vec::new(),
            memory: None,
            daq: DaqDict::default(),
            view_mode: A2lViewMode::Measurements,
            running: false,
            elapsed: Duration::ZERO,
            sample_accumulator: Duration::ZERO,
            unplaced_measurements: 0,
            last_error: None,
        }
    }

    pub fn attach_project(&mut self, project: &Project) -> Result<(), String> {
        self.project_name = project.named.name.clone();
        self.module_count = project.modules().count();
        self.objects.clear();
        for module in project.modules() {
            self.add_module_objects(module);
        }

        let measurements = self
            .objects
            .iter()
            .filter_map(|object| match &object.source {
                A2lSource::Measurement(measurement)
                    if measurement.addr.address.is_some()
                        && data_type_size_in_byte(measurement.data_type).is_some()
                        && measurement
                            .matrix_dim
                            .as_ref()
                            .is_none_or(|dims| dims.iter().all(|dimension| *dimension > 0)) =>
                {
                    Some(measurement.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        self.memory =
            Some(MeasurementAccessData::new(&measurements).map_err(|error| error.to_string())?);
        self.seed_virtual_values();
        for object in self
            .objects
            .iter_mut()
            .filter(|object| object.kind == A2lObjectKind::Measurement)
            .take(8)
        {
            object.armed = true;
        }
        self.elapsed = Duration::ZERO;
        self.sample_accumulator = Duration::ZERO;
        self.running = false;
        self.last_error = None;
        self.rebuild_daq()?;
        Ok(())
    }

    fn add_module_objects(&mut self, module: &Module) {
        let default_byte_order = module
            .children
            .iter()
            .find_map(|child| match child {
                ModuleChild::ModCommon(common) => Some(common.byte_order),
                _ => None,
            })
            .unwrap_or(ByteOrder::MSB_LAST);
        for child in &module.children {
            match child {
                ModuleChild::Measurement(measurement) => {
                    let conversion =
                        OwnedConversion::from_module(module, &measurement.conv.conversion);
                    let unit = measurement
                        .conv
                        .phys_unit
                        .clone()
                        .filter(|unit| !unit.is_empty())
                        .unwrap_or_else(|| conversion.method.unit.clone());
                    self.objects.push(A2lObject {
                        kind: A2lObjectKind::Measurement,
                        module: module.name.clone(),
                        name: measurement.named.name.clone(),
                        description: measurement.named.description.clone().unwrap_or_default(),
                        address: measurement.addr.address,
                        data_type: measurement.data_type,
                        shape: dimensions_text(measurement.matrix_dim.as_deref()),
                        unit,
                        lower: measurement.conv.lower_limit,
                        upper: measurement.conv.upper_limit,
                        writable: measurement.read_write,
                        source: A2lSource::Measurement(measurement.clone()),
                        conversion,
                        raw_calibration_value: 0.0,
                        current_physical_value: None,
                        armed: false,
                        daq_info: None,
                        recent: VecDeque::new(),
                        recorded: VecDeque::new(),
                        dropped_samples: 0,
                    });
                }
                ModuleChild::Characteristic(characteristic) => {
                    let conversion =
                        OwnedConversion::from_module(module, &characteristic.conv.conversion);
                    let data_type = module
                        .children
                        .iter()
                        .find_map(|candidate| match candidate {
                            ModuleChild::RecordLayout(layout)
                                if layout.name == characteristic.rec.record_layout =>
                            {
                                layout.fnc_values.as_ref().map(|values| values.data_type)
                            }
                            _ => None,
                        })
                        .unwrap_or(DataType::Unsupported);
                    let unit = characteristic
                        .conv
                        .phys_unit
                        .clone()
                        .filter(|unit| !unit.is_empty())
                        .unwrap_or_else(|| conversion.method.unit.clone());
                    let calibration_allowed = !matches!(
                        characteristic.addr.calib_access,
                        CalibrationAccess::NO_CALIBRATION | CalibrationAccess::NOT_IN_MCD_SYSTEM
                    );
                    self.objects.push(A2lObject {
                        kind: A2lObjectKind::Calibration,
                        module: module.name.clone(),
                        name: characteristic.named.name.clone(),
                        description: characteristic.named.description.clone().unwrap_or_default(),
                        address: characteristic.addr.address,
                        data_type,
                        shape: format!(
                            "{:?} {}",
                            characteristic.char_type,
                            dimensions_text(characteristic.matrix_dim.as_deref())
                        ),
                        unit,
                        lower: characteristic.conv.lower_limit,
                        upper: characteristic.conv.upper_limit,
                        writable: characteristic.char_type == CharacteristicType::VALUE
                            && data_type_size_in_byte(data_type).is_some()
                            && !characteristic.rec.read_only
                            && calibration_allowed,
                        source: A2lSource::Characteristic(characteristic.clone()),
                        conversion,
                        raw_calibration_value: 0.0,
                        current_physical_value: None,
                        armed: false,
                        daq_info: None,
                        recent: VecDeque::new(),
                        recorded: VecDeque::new(),
                        dropped_samples: 0,
                    });
                }
                _ => {}
            }
        }
        for object in self.objects.iter_mut().filter(|object| {
            object.module == module.name && object.kind == A2lObjectKind::Measurement
        }) {
            if let A2lSource::Measurement(measurement) = &mut object.source {
                if measurement.conv.byte_order == ByteOrder::NotSet {
                    measurement.conv.byte_order = default_byte_order;
                }
            }
        }
    }

    fn seed_virtual_values(&mut self) {
        let Some(memory) = &mut self.memory else {
            return;
        };
        for object in &mut self.objects {
            let initial = initial_value(object.lower, object.upper);
            match &object.source {
                A2lSource::Measurement(measurement) => {
                    if memory.set_phys_value(measurement, &object.conversion.view(), initial, -1, 0)
                    {
                        object.current_physical_value = Some(initial);
                    }
                }
                A2lSource::Characteristic(characteristic)
                    if characteristic.char_type == CharacteristicType::VALUE =>
                {
                    object.raw_calibration_value = object
                        .conversion
                        .view()
                        .to_raw(object.data_type, initial)
                        .unwrap_or(0.0);
                    object.current_physical_value = object
                        .conversion
                        .view()
                        .to_physical(object.raw_calibration_value)
                        .ok();
                }
                A2lSource::Characteristic(_) => {}
            }
        }
    }

    pub fn is_loaded(&self) -> bool {
        !self.objects.is_empty()
    }

    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub fn measurement_count(&self) -> usize {
        self.objects
            .iter()
            .filter(|object| object.kind == A2lObjectKind::Measurement)
            .count()
    }

    pub fn calibration_count(&self) -> usize {
        self.objects
            .iter()
            .filter(|object| object.kind == A2lObjectKind::Calibration)
            .count()
    }

    pub fn armed_count(&self) -> usize {
        self.objects.iter().filter(|object| object.armed).count()
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        self.objects
            .iter()
            .enumerate()
            .filter(|(_, object)| match self.view_mode {
                A2lViewMode::Measurements => object.kind == A2lObjectKind::Measurement,
                A2lViewMode::Calibrations => object.kind == A2lObjectKind::Calibration,
                A2lViewMode::All => true,
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn cycle_view(&mut self) -> &'static str {
        self.view_mode = self.view_mode.next();
        self.view_mode.title()
    }

    pub fn object_view(&self, index: usize) -> Option<A2lObjectView> {
        let value = self.current_value_text(index);
        let object = self.objects.get(index)?;
        Some(A2lObjectView {
            kind: object.kind,
            module: object.module.clone(),
            name: object.name.clone(),
            address: object
                .address
                .map(|address| format!("0x{address:08X}"))
                .unwrap_or_else(|| "-".to_owned()),
            data_type: format!("{:?}", object.data_type),
            shape: object.shape.clone(),
            unit: object.unit.clone(),
            value,
            writable: object.writable,
            armed: object.armed,
            samples: object.recorded.len(),
        })
    }

    pub fn object_details(&self, index: usize) -> Vec<String> {
        let value = self.current_value_text(index);
        let Some(object) = self.objects.get(index) else {
            return vec!["No A2L object selected".to_owned()];
        };
        let mut details = vec![
            format!("{} · {}", object.name, object.kind.title()),
            object.description.clone(),
            String::new(),
            format!("Module        {}", object.module),
            format!(
                "Address       {}",
                object
                    .address
                    .map(|address| format!("0x{address:08X}"))
                    .unwrap_or_else(|| "not defined".to_owned())
            ),
            format!("Data type     {:?}", object.data_type),
            format!("Shape         {}", object.shape),
            format!("Conversion    {}", object.conversion.method.name),
            format!("Unit          {}", dash_if_empty(&object.unit)),
            format!("Limits        {} .. {}", object.lower, object.upper),
            format!("Current       {value}"),
            format!("Writable      {}", object.writable),
        ];
        if object.kind == A2lObjectKind::Measurement {
            details.push(format!("DAQ armed     {}", object.armed));
            details.push(format!("Samples       {}", object.recorded.len()));
            details.push(format!("Dropped       {}", object.dropped_samples));
            if !object.recent.is_empty() {
                details.push(String::new());
                details.push("Recent physical samples".to_owned());
                details.extend(
                    object
                        .recent
                        .iter()
                        .rev()
                        .take(8)
                        .rev()
                        .map(|(time, value)| format!("  {time:>8.3} s  {value:>14.6}")),
                );
            }
        }
        details
    }

    pub fn toggle_armed(&mut self, index: usize) -> Result<bool, String> {
        let object = self
            .objects
            .get_mut(index)
            .ok_or_else(|| "no A2L object selected".to_owned())?;
        if object.kind != A2lObjectKind::Measurement {
            return Err("only MEASUREMENT objects can be placed into DAQ".to_owned());
        }
        object.armed = !object.armed;
        let armed = object.armed;
        self.running = false;
        self.rebuild_daq()?;
        Ok(armed)
    }

    pub fn set_running(&mut self, running: bool) -> Result<(), String> {
        if running && !self.is_loaded() {
            return Err("open an A2L project first".to_owned());
        }
        if running && self.armed_count() == 0 {
            return Err("arm at least one MEASUREMENT object first".to_owned());
        }
        self.running = running;
        Ok(())
    }

    pub fn set_value_text(&mut self, index: usize, text: &str) -> Result<f64, String> {
        let physical = text
            .trim()
            .parse::<f64>()
            .map_err(|_| "enter a finite physical numeric value".to_owned())?;
        if !physical.is_finite() {
            return Err("enter a finite physical numeric value".to_owned());
        }
        let object = self
            .objects
            .get_mut(index)
            .ok_or_else(|| "no A2L object selected".to_owned())?;
        if !object.writable {
            return Err(format!("{} is read-only", object.name));
        }
        if object.lower < object.upper && !(object.lower..=object.upper).contains(&physical) {
            return Err(format!(
                "value {physical} is outside {} .. {}",
                object.lower, object.upper
            ));
        }
        match &object.source {
            A2lSource::Measurement(measurement) => {
                let written = self.memory.as_mut().is_some_and(|memory| {
                    memory.set_phys_value(measurement, &object.conversion.view(), physical, -1, 0)
                });
                if !written {
                    return Err("virtual ECU memory does not cover this measurement".to_owned());
                }
                object.current_physical_value = Some(physical);
            }
            A2lSource::Characteristic(characteristic) => {
                if characteristic.char_type != CharacteristicType::VALUE {
                    return Err("only scalar VALUE calibration is editable here".to_owned());
                }
                object.raw_calibration_value = object
                    .conversion
                    .view()
                    .to_raw(object.data_type, physical)
                    .map_err(|error| error.to_string())?;
                object.current_physical_value = Some(physical);
            }
        }
        Ok(physical)
    }

    pub fn clear_samples(&mut self) {
        self.daq.clear_data();
        for object in &mut self.objects {
            object.recent.clear();
            object.recorded.clear();
            object.dropped_samples = 0;
        }
        self.elapsed = Duration::ZERO;
        self.sample_accumulator = Duration::ZERO;
    }

    pub fn save_mdf(&self, path: impl AsRef<Path>) -> Result<usize, String> {
        let objects = self
            .objects
            .iter()
            .filter(|object| !object.recorded.is_empty())
            .collect::<Vec<_>>();
        if objects.is_empty() {
            return Err("capture at least one DAQ sample before MDF export".to_owned());
        }
        let unix_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        let recording =
            HdRecordingInfo::from_unix_nanos(unix_nanos, 0, TimeQualityType::ExternalAbsolute);
        let mut writer = MdfWriter::new_v3(
            "autors-cli",
            "autors",
            &self.project_name,
            "A2L DAQ recording",
            "Physical measurement values captured by the autors A2L workbench",
            recording,
            65001,
        );
        let mut sample_count = 0;
        for object in objects {
            let handle = writer.create_channel_group(Some(&format!(
                "{}::{} A2L measurement",
                object.module, object.name
            )));
            writer
                .add_measurement(
                    handle,
                    &format!("{}::{}", object.module, object.name),
                    &object.unit,
                    object.lower,
                    object.upper,
                    DataType::Float64Ieee,
                    ByteOrder::MSB_LAST,
                    Some(&object.description),
                    None,
                    0,
                )
                .map_err(|error| error.to_string())?;
            for (timestamp, value) in &object.recorded {
                writer
                    .add_data_entry(handle, *timestamp, &value.to_le_bytes())
                    .map_err(|error| error.to_string())?;
                sample_count += 1;
            }
        }
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        writer.save(path).map_err(|error| error.to_string())?;
        Ok(sample_count)
    }

    pub fn advance(&mut self, elapsed: Duration) {
        if !self.running {
            return;
        }
        self.elapsed = self.elapsed.saturating_add(elapsed);
        self.sample_accumulator = self.sample_accumulator.saturating_add(elapsed);
        while self.sample_accumulator >= SAMPLE_PERIOD {
            self.sample_accumulator = self.sample_accumulator.saturating_sub(SAMPLE_PERIOD);
            if let Err(error) = self.capture_sample() {
                self.last_error = Some(error);
                self.running = false;
                break;
            }
        }
    }

    fn rebuild_daq(&mut self) -> Result<(), String> {
        for object in &mut self.objects {
            object.daq_info = None;
            object.recent.clear();
            object.recorded.clear();
            object.dropped_samples = 0;
        }
        let mut requested = Vec::new();
        for object in &mut self.objects {
            let A2lSource::Measurement(measurement) = &object.source else {
                continue;
            };
            if !object.armed {
                continue;
            }
            let Some(address) = measurement.addr.address else {
                continue;
            };
            if data_type_size_in_byte(measurement.data_type).is_none() {
                continue;
            }
            let owned_conversion = object.conversion.clone();
            let info = MeasurementInfo {
                name: measurement.named.name.clone(),
                address,
                address_extension: measurement.addr.address_extension.unwrap_or_default(),
                data_type: measurement.data_type,
                byte_order: measurement.conv.byte_order,
                bits: Some(MeasurementAccessData::bit_operation_of(measurement)),
                matrix_dim: measurement.matrix_dim.clone(),
                phys_conv: Some(Arc::new(move |raw| {
                    owned_conversion.view().to_physical(raw).unwrap_or(raw)
                })),
            };
            object.daq_info = Some(info.clone());
            requested.push(DaqMeasurement::new(info, 0, Some(vec![0])));
        }
        let mut daq = DaqDict {
            lists: vec![DaqList::new(
                0,
                0,
                0,
                8,
                u8::MAX,
                u8::MAX,
                1,
                1,
                "100 ms".to_owned(),
                0x600,
            )],
            ..DaqDict::default()
        };
        self.unplaced_measurements = daq
            .fill_daq_lists(&mut requested, true)
            .map_err(|error| error.to_string())?;
        self.daq = daq;
        Ok(())
    }

    fn capture_sample(&mut self) -> Result<(), String> {
        let timestamp = self.elapsed.as_secs_f64();
        let memory = self
            .memory
            .as_mut()
            .ok_or_else(|| "virtual ECU memory is not initialized".to_owned())?;
        for list in &mut self.daq.lists {
            let payload_len = list
                .odts
                .values()
                .flat_map(|odt| odt.entries.iter())
                .filter(|entry| entry.daq_position() >= 0)
                .map(|entry| {
                    entry.daq_position() as usize
                        + entry.measurement.size_in_byte().unwrap_or_default()
                })
                .max()
                .unwrap_or_default();
            let mut payload = vec![0u8; payload_len];
            for entry in list
                .odts
                .values()
                .flat_map(|odt| odt.entries.iter())
                .filter(|entry| entry.daq_position() >= 0)
            {
                let size = entry.measurement.size_in_byte().unwrap_or_default();
                let Some(bytes) = memory.get_data(entry.address(), size) else {
                    continue;
                };
                let position = entry.daq_position() as usize;
                payload[position..position + bytes.len()].copy_from_slice(&bytes);
            }
            list.values
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .add(timestamp, payload);
            list.last_timestamp = timestamp;
        }
        for object in &mut self.objects {
            let Some(info) = &object.daq_info else {
                continue;
            };
            let Some(entry) = self.daq.get_odt_entry(info, -1, 0) else {
                continue;
            };
            let value =
                entry.current_value(autors_comm::base::ValueObjectFormat::Physical, f64::NAN);
            if value.is_finite() {
                object.current_physical_value = Some(value);
                if object.recent.len() == RECENT_LIMIT {
                    object.recent.pop_front();
                }
                object.recent.push_back((timestamp, value));
                if object.recorded.len() == RECORDING_LIMIT {
                    object.recorded.pop_front();
                    object.dropped_samples += 1;
                }
                object.recorded.push_back((timestamp, value));
            }
        }
        Ok(())
    }

    fn current_value_text(&self, index: usize) -> String {
        let Some(object) = self.objects.get(index) else {
            return "-".to_owned();
        };
        object
            .current_physical_value
            .filter(|value| value.is_finite())
            .map(|value| format!("{value:.6}"))
            .unwrap_or_else(|| "-".to_owned())
    }
}

fn initial_value(lower: f64, upper: f64) -> f64 {
    if lower.is_finite() && upper.is_finite() && lower < upper {
        if (lower..=upper).contains(&0.0) {
            0.0
        } else {
            lower + (upper - lower) / 2.0
        }
    } else {
        0.0
    }
}

fn dimensions_text(dimensions: Option<&[i32]>) -> String {
    dimensions
        .filter(|dimensions| !dimensions.is_empty())
        .map(|dimensions| {
            dimensions
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join("×")
        })
        .unwrap_or_else(|| "scalar".to_owned())
}

fn dash_if_empty(value: &str) -> &str {
    if value.is_empty() {
        "-"
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A2L: &str = r#"
/begin PROJECT Demo "DAQ demo"
  /begin MODULE ECU "virtual ECU"
    /begin MOD_COMMON "" BYTE_ORDER MSB_LAST /end MOD_COMMON
    /begin RECORD_LAYOUT RL FNC_VALUES 1 UWORD ROW_DIR DIRECT /end RECORD_LAYOUT
    /begin COMPU_METHOD SCALE "scale" LINEAR "%6.1" "rpm" COEFFS_LINEAR 2 0 /end COMPU_METHOD
    /begin MEASUREMENT Speed "engine speed" UWORD SCALE 1 0 0 8000 ECU_ADDRESS 0x1000 READ_WRITE /end MEASUREMENT
    /begin CHARACTERISTIC Gain "controller gain" VALUE 0x2000 RL 0 SCALE 0 100 /end CHARACTERISTIC
  /end MODULE
/end PROJECT
"#;

    #[test]
    fn attaches_a2l_edits_values_and_captures_real_daq_buffers() {
        let project = Project::parse_str(A2L).unwrap();
        let mut lab = A2lLab::new();
        lab.attach_project(&project).unwrap();
        assert_eq!(lab.measurement_count(), 1);
        assert_eq!(lab.calibration_count(), 1);
        assert_eq!(lab.armed_count(), 1);
        assert_eq!(lab.set_value_text(0, "1200").unwrap(), 1200.0);
        lab.set_running(true).unwrap();
        lab.advance(Duration::from_millis(100));
        assert_eq!(lab.objects[0].recent.len(), 1);
        assert_eq!(lab.objects[0].recorded.len(), 1);
        assert!((lab.objects[0].recent[0].1 - 1200.0).abs() < f64::EPSILON);
        assert_eq!(lab.set_value_text(1, "42").unwrap(), 42.0);
        assert!(lab.current_value_text(1).starts_with("42."));
    }

    #[test]
    fn exports_physical_daq_samples_as_roundtrippable_mdf() {
        let project = Project::parse_str(A2L).unwrap();
        let mut lab = A2lLab::new();
        lab.attach_project(&project).unwrap();
        lab.set_value_text(0, "1200").unwrap();
        lab.set_running(true).unwrap();
        lab.advance(Duration::from_millis(200));
        let path =
            std::env::temp_dir().join(format!("autors-cli-a2l-daq-{}.mdf", std::process::id()));
        assert_eq!(lab.save_mdf(&path).unwrap(), 2);
        let reader = autors_mdf::base::MdfReader::open(&path).unwrap();
        let file = reader.as_v3().unwrap();
        assert_eq!(file.hd_block.dg_blocks.len(), 1);
        assert_eq!(file.hd_block.dg_blocks[0].cg_blocks[0].record_count, 2);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn view_modes_and_read_only_rules_are_enforced() {
        let project = Project::parse_str(A2L).unwrap();
        let mut lab = A2lLab::new();
        lab.attach_project(&project).unwrap();
        assert_eq!(lab.visible_indices(), vec![0]);
        assert_eq!(lab.cycle_view(), "Calibrations");
        assert_eq!(lab.visible_indices(), vec![1]);
        assert!(lab.toggle_armed(1).is_err());
        lab.objects[0].writable = false;
        assert!(lab.set_value_text(0, "1").is_err());
    }
}
