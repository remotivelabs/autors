//! CDF ↔ A2L linkage: import of calibration value instances (CDF → value
//! objects) and export (value objects → CDF).
//! The XML document object model and the read/write entry points live in
//! autors-cdf ([`autors_cdf::cdf`]); this module implements the A2L-side
//! semantics: reading and writing CDF instance values by A2L characteristic
//! name, converting between COMPU_VTAB text and numeric values, and checking
//! axis point counts against A2L (`MaxAxisPoints` and the "point count stored
//! in ECU" flag).
//! Layering: values → cdf (file-format layer) + values → datafile, no cycles.
//! Imported value objects are written back to the data-file image via
//! [`crate::ecu_io::set_value`]; the caller is expected to combine the two
//! steps.
//! Behavioral notes:
//! - The project-wide characteristic-by-name dictionary is replaced by a
//!   caller-injected `lookup` closure (returns [`RecordLayoutRefIo`], with the
//!   reference already resolved).
//! - Instances whose value is not found or does not match the expected format
//!   are recorded in `skipped` and processing continues; no hard error is
//!   returned.
//! - On export, the SHORT-NAME of MSRSW/SW-INSTANCE-TREE and the CATEGORY of
//!   SW-CS-COLLECTION are left as `None` (same convention as autors-cdf).
//! - LABEL axis-point value texts of VG groups are formatted with the
//!   invariant-culture shortest decimal representation (same convention as
//!   autors-a2l `to_dec`).
//! - On import, VT text without a VTAB falls back to a comparison against
//!   `"TRUE"`/`"FALSE"`.

use autors_a2l::model::compu::{CompuVtab, CompuVtabRange};
use autors_a2l::model::enums::{AxisType, CharacteristicType, ConversionType, DataType};
use autors_cdf::cdf::{
    Msrsw, SwArraySize, SwAxisCont, SwAxisConts, SwCsCollection, SwCsCollections, SwInstance,
    SwInstanceSpec, SwInstanceTree, SwInstanceTreeOrigin, SwSystem, SwSystems, SwValue,
    SwValueCont, SwValuesPhys, Vg,
};

use crate::ecu_io::{number_of_elements, AxisIo, AxisRefIo, CharacteristicIo, RecordLayoutRefIo};
use crate::error::{Error, Result};
use crate::value::{
    vtab_range_to_raw, vtab_to_raw, AxisContext, CharValue, CharacteristicRef, CompuTabRef,
    Conversion, SingleValueContext, ValueData, ValueObjectFormat,
};

// ===========================================================================
// Data conservation (import result accumulation)
// ===========================================================================

/// Reason a calibration instance was skipped during CDF import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CdfErrorType {
    /// No calibration object with the same name was found in the A2L.
    NotFound,
    /// The type is not supported.
    NotSupported,
    /// The format does not match (catch-all used when processing fails).
    Format,
    /// The types differ (reserved; not used by the CDF path).
    TypeDiffers,
    /// The element counts differ (reserved; not used by the CDF path).
    NoOfElementsDiffers,
    /// The X axis is invalid.
    XAxisInvalid,
    /// The Y axis is invalid.
    YAxisInvalid,
    /// The Z axis is invalid.
    ZAxisInvalid,
    /// The 4th axis is invalid.
    Axis4Invalid,
    /// The 5th axis is invalid.
    Axis5Invalid,
}

/// Result of a CDF import: the successfully imported values plus the skipped
/// instances.
#[derive(Debug, Default)]
pub struct CdfConservation {
    /// Successfully imported calibration values (physical values; instances
    /// are processed in reverse document order).
    pub values: Vec<CharValue>,
    /// Skipped instances: (calibration object name, reason).
    pub skipped: Vec<(String, CdfErrorType)>,
}

/// Maps an axis index to the corresponding skip reason.
fn axis_error(i: usize) -> CdfErrorType {
    match i {
        0 => CdfErrorType::XAxisInvalid,
        1 => CdfErrorType::YAxisInvalid,
        2 => CdfErrorType::ZAxisInvalid,
        3 => CdfErrorType::Axis4Invalid,
        _ => CdfErrorType::Axis5Invalid,
    }
}

// ===========================================================================
// Import (CDF document -> value objects)
// ===========================================================================

/// Converts the calibration instances of a CDF document into value objects
/// according to their A2L definitions. `lookup` resolves an instance
/// SHORT-NAME to the already-resolved characteristic/AXIS_PTS reference.
pub fn import<'a>(
    msrsw: &Msrsw,
    lookup: impl Fn(&str) -> Option<RecordLayoutRefIo<'a>>,
) -> CdfConservation {
    let instances = msrsw.all_instances();
    let mut result = CdfConservation::default();
    // Instances are processed in reverse document order.
    for inst in instances.iter().rev() {
        match import_instance(inst, &instances, &lookup) {
            Ok(v) => result.values.push(v),
            Err(e) => result
                .skipped
                .push((inst.short_name.clone().unwrap_or_default(), e)),
        }
    }
    result
}

/// A2LCOMPU_VTAB_RANGE`).
fn vtabs_of<'a>(conv: &Conversion<'a>) -> (Option<&'a CompuVtab>, Option<&'a CompuVtabRange>) {
    match conv.tab {
        Some(CompuTabRef::Vtab(t)) => (Some(t), None),
        Some(CompuTabRef::VtabRange(t)) => (None, Some(t)),
        _ => (None, None),
    }
}

fn cdf_to_f64(v: &SwValue, vtab: Option<&CompuVtab>, vtab_range: Option<&CompuVtabRange>) -> f64 {
    match v {
        SwValue::V(d) => *d,
        SwValue::Vt(s) => {
            if let Some(t) = vtab {
                vtab_to_raw(t, s)
            } else if let Some(t) = vtab_range {
                vtab_range_to_raw(t, s)
            } else {
                match s.as_str() {
                    "TRUE" => 1.0,
                    "FALSE" => 0.0,
                    _ => f64::NAN,
                }
            }
        }
        SwValue::Vg(_) => f64::NAN,
    }
}

fn flatten_values(
    vs: &[SwValue],
    vtab: Option<&CompuVtab>,
    vtab_range: Option<&CompuVtabRange>,
    out: &mut Vec<f64>,
) {
    for v in vs {
        match v {
            SwValue::Vg(g) => flatten_values(&g.vs, vtab, vtab_range, out),
            _ => out.push(cdf_to_f64(v, vtab, vtab_range)),
        }
    }
}

fn validate_count(in_ecu: bool, max_axis_points: i32, n: usize) -> usize {
    let max = max_axis_points.max(0) as usize;
    if in_ecu {
        if n <= max {
            n
        } else {
            0
        }
    } else if n == max {
        n
    } else {
        0
    }
}

fn axis_count_in_ecu(io: &CharacteristicIo, i: usize, axis: &AxisIo) -> bool {
    match axis.descr.axis_type {
        AxisType::STD_AXIS => io.record_layout.no_axis_pts[i].is_some(),
        AxisType::COM_AXIS => matches!(
            &axis.reference,
            Some(AxisRefIo::AxisPts { io: ap, .. }) if ap.record_layout.no_axis_pts[0].is_some()
        ),
        AxisType::RES_AXIS => matches!(
            &axis.reference,
            Some(AxisRefIo::AxisPts { io: ap, .. }) if ap.record_layout.no_rescale_x.is_some()
        ),
        AxisType::CURVE_AXIS => matches!(
            &axis.reference,
            Some(AxisRefIo::CurveAxis { io: c, .. }) if c.record_layout.no_axis_pts[0].is_some()
        ),
        AxisType::FIX_AXIS => false,
    }
}

fn import_instance<'a>(
    inst: &SwInstance,
    instances: &[&SwInstance],
    lookup: &dyn Fn(&str) -> Option<RecordLayoutRefIo<'a>>,
) -> std::result::Result<CharValue, CdfErrorType> {
    let name = inst.short_name.as_deref().ok_or(CdfErrorType::Format)?;
    let vc = inst.sw_value_cont.as_ref().ok_or(CdfErrorType::Format)?;
    if vc
        .sw_values_phys
        .as_ref()
        .is_some_and(|v| v.items.is_empty())
    {
        return Err(CdfErrorType::Format);
    }
    let vs: &[SwValue] = vc
        .sw_values_phys
        .as_ref()
        .map(|v| v.items.as_slice())
        .unwrap_or(&[]);
    match lookup(name).ok_or(CdfErrorType::NotFound)? {
        RecordLayoutRefIo::Characteristic(c) => import_characteristic(inst, instances, vs, &c),
        RecordLayoutRefIo::AxisPts(a) => import_axis_pts(vs, &a),
    }
}

fn import_axis_pts(
    vs: &[SwValue],
    io: &crate::ecu_io::AxisPtsIo,
) -> std::result::Result<CharValue, CdfErrorType> {
    let (vtab, vtab_range) = vtabs_of(&io.conversion);
    let mut list = Vec::new();
    flatten_values(vs, vtab, vtab_range, &mut list);
    let mut n = io.axis_pts.max_axis_points.max(0) as usize;
    if io.record_layout.no_rescale_x.is_some() {
        n *= 2;
    }
    list.truncate(n);
    let mut value = CharValue::for_char_type(
        CharacteristicType::VAL_BLK,
        CharacteristicRef::AxisPts(io.axis_pts.clone()),
    )
    .map_err(|_| CdfErrorType::NotSupported)?;
    let m = list.len();
    value.base_mut().value = ValueData::array(vec![m], list).map_err(|_| CdfErrorType::Format)?;
    Ok(value)
}

fn import_characteristic(
    inst: &SwInstance,
    instances: &[&SwInstance],
    vs: &[SwValue],
    io: &CharacteristicIo,
) -> std::result::Result<CharValue, CdfErrorType> {
    let ch = io.characteristic;
    let char_type = ch.char_type;
    let (vtab, vtab_range) = vtabs_of(&io.conversion);
    let has_axes = matches!(
        char_type,
        CharacteristicType::CURVE
            | CharacteristicType::MAP
            | CharacteristicType::CUBOID
            | CharacteristicType::CUBE_4
            | CharacteristicType::CUBE_5
    );
    let flags: Vec<bool> = if has_axes {
        io.axes
            .iter()
            .enumerate()
            .map(|(i, a)| axis_count_in_ecu(io, i, a))
            .collect()
    } else {
        Vec::new()
    };
    let max_pts = |i: usize| io.axes[i].descr.max_axis_points;
    let mut counts: Vec<usize> = Vec::new();
    let mut value = CharValue::for_char_type(char_type, CharacteristicRef::Char(ch.clone()))
        .map_err(|_| CdfErrorType::NotSupported)?;
    let data: ValueData = match char_type {
        CharacteristicType::VALUE => {
            let v = vs.first().ok_or(CdfErrorType::Format)?;
            ValueData::Scalar(cdf_to_f64(v, vtab, vtab_range))
        }
        CharacteristicType::ASCII => match vs.first() {
            Some(SwValue::Vt(s)) => ValueData::Text(s.clone()),
            _ => return Err(CdfErrorType::Format),
        },
        CharacteristicType::VAL_BLK => {
            let mut list = Vec::new();
            flatten_values(vs, vtab, vtab_range, &mut list);
            let n = number_of_elements(ch).map_err(|_| CdfErrorType::Format)?;
            list.truncate(n);
            let m = list.len();
            ValueData::array(vec![m], list).map_err(|_| CdfErrorType::Format)?
        }
        CharacteristicType::CURVE => {
            let n = validate_count(flags[0], max_pts(0), vs.len());
            if n == 0 {
                return Err(CdfErrorType::XAxisInvalid);
            }
            counts.push(n);
            let mut list = Vec::new();
            flatten_values(vs, vtab, vtab_range, &mut list);
            let m = list.len();
            ValueData::array(vec![m], list).map_err(|_| CdfErrorType::Format)?
        }
        CharacteristicType::MAP
        | CharacteristicType::CUBOID
        | CharacteristicType::CUBE_4
        | CharacteristicType::CUBE_5 => {
            let ndim = io.axes.len();
            let first = vs.first().ok_or(CdfErrorType::Format)?;
            for (i, &flag) in flags.iter().enumerate().take(ndim - 1) {
                let depth = ndim - 2 - i;
                let n = nested_len(first, depth).ok_or(CdfErrorType::Format)?;
                let n = validate_count(flag, max_pts(i), n);
                if n == 0 {
                    return Err(axis_error(i));
                }
                counts.push(n);
            }
            let n = validate_count(flags[ndim - 1], max_pts(ndim - 1), vs.len());
            if n == 0 {
                return Err(axis_error(ndim - 1));
            }
            counts.push(n);
            let mut list = Vec::new();
            flatten_values(vs, vtab, vtab_range, &mut list);
            let total: usize = counts.iter().product();
            if list.len() < total {
                return Err(CdfErrorType::Format);
            }
            list.truncate(total);
            ValueData::array(counts.clone(), list).map_err(|_| CdfErrorType::Format)?
        }
        _ => return Err(CdfErrorType::NotSupported),
    };
    value.base_mut().value = data;
    if has_axes {
        value.base_mut().axis_value = import_axes(inst, instances, io, &counts)?;
    }
    Ok(value)
}

fn nested_len(v: &SwValue, depth: usize) -> Option<usize> {
    let mut cur = v;
    for _ in 0..depth {
        match cur {
            SwValue::Vg(g) => cur = g.vs.first()?,
            _ => return None,
        }
    }
    match cur {
        SwValue::Vg(g) => Some(g.vs.len()),
        _ => None,
    }
}

fn import_axes(
    inst: &SwInstance,
    instances: &[&SwInstance],
    io: &CharacteristicIo,
    counts: &[usize],
) -> std::result::Result<Vec<Vec<f64>>, CdfErrorType> {
    let conts = inst.sw_axis_conts.as_ref().ok_or(CdfErrorType::Format)?;
    let mut out = Vec::with_capacity(io.axes.len());
    for (i, axis) in io.axes.iter().enumerate() {
        let cont = conts.items.get(i).ok_or(CdfErrorType::Format)?;
        let (vtab, vtab_range) = vtabs_of(&axis.conversion);
        match &cont.sw_values_phys {
            Some(axis_vs) => {
                if axis_vs.items.len() != counts.get(i).copied().unwrap_or(0) {
                    return Err(axis_error(i));
                }
                let mut list = Vec::new();
                flatten_values(&axis_vs.items, vtab, vtab_range, &mut list);
                out.push(list);
            }
            None => match axis.descr.axis_type {
                AxisType::COM_AXIS | AxisType::RES_AXIS => {
                    let ref_inst = find_ref_instance(cont, instances)?;
                    let rvs = ref_inst
                        .sw_value_cont
                        .as_ref()
                        .and_then(|vc| vc.sw_values_phys.as_ref())
                        .ok_or(CdfErrorType::Format)?;
                    let mut list = Vec::new();
                    flatten_values(&rvs.items, vtab, vtab_range, &mut list);
                    out.push(list);
                }
                AxisType::CURVE_AXIS => {
                    let ref_inst = find_ref_instance(cont, instances)?;
                    let rvs = ref_inst
                        .sw_axis_conts
                        .as_ref()
                        .and_then(|c| c.items.first())
                        .and_then(|c| c.sw_values_phys.as_ref())
                        .ok_or(CdfErrorType::Format)?;
                    let mut list = Vec::new();
                    flatten_values(&rvs.items, vtab, vtab_range, &mut list);
                    out.push(list);
                }
                AxisType::STD_AXIS => return Err(axis_error(i)),
                AxisType::FIX_AXIS => out.push(Vec::new()),
            },
        }
    }
    Ok(out)
}

fn find_ref_instance<'a>(
    cont: &SwAxisCont,
    instances: &[&'a SwInstance],
) -> std::result::Result<&'a SwInstance, CdfErrorType> {
    let ref_name = cont
        .sw_instance_ref
        .as_deref()
        .ok_or(CdfErrorType::Format)?;
    instances
        .iter()
        .find(|x| x.short_name.as_deref() == Some(ref_name))
        .copied()
        .ok_or(CdfErrorType::Format)
}

// ===========================================================================
// ===========================================================================

pub type DefFunction<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Default)]
pub struct CdfExportOptions<'a> {
    pub a2l_source: Option<String>,
    pub data_source: Option<String>,
    pub description: Option<String>,
    pub functions: Option<Vec<String>>,
    pub def_function: Option<DefFunction<'a>>,
    pub add_parameter_desc: bool,
    pub short_name: String,
    pub creator: String,
    pub creator_version: Option<String>,
}

pub fn export(
    values: &[(&CharValue, &CharacteristicIo)],
    opts: &CdfExportOptions,
) -> Result<Msrsw> {
    let mut msrsw = Msrsw::new(
        opts.short_name.clone(),
        opts.creator.clone(),
        opts.creator_version.clone().unwrap_or_default(),
        opts.description.clone(),
    );
    msrsw.creator_version = opts.creator_version.clone();
    let cs_collections = opts.functions.as_ref().map(|fs| {
        fs.iter()
            .map(|f| SwCsCollection {
                category: None,
                sw_feature_ref: Some(f.clone()),
            })
            .collect::<Vec<_>>()
    });
    let mut instances = Vec::new();
    for (value, io) in values {
        if let Some(inst) = export_instance(value, io, opts)? {
            instances.push(inst);
        }
    }
    let tree = SwInstanceTree {
        short_name: None,
        category: None,
        sw_instance_tree_origin: Some(SwInstanceTreeOrigin::new(
            opts.a2l_source.clone(),
            opts.data_source.clone(),
        )),
        sw_cs_collections: cs_collections.map(|items| SwCsCollections { items }),
        sw_instances: instances,
    };
    let system = SwSystem::new(SwInstanceSpec {
        sw_instance_trees: vec![tree],
    });
    msrsw.sw_systems = Some(SwSystems {
        items: vec![system],
    });
    Ok(msrsw)
}

fn decimal_count_of(format: Option<&str>) -> Result<i32> {
    match format {
        Some(f) => crate::value::get_decimal_count(f),
        None => Ok(0),
    }
}

fn fnc_data_type(io: &CharacteristicIo) -> DataType {
    io.record_layout
        .fnc_values
        .as_ref()
        .map(|f| f.data_type)
        .unwrap_or(DataType::Unsupported)
}

fn axis_data_type(io: &CharacteristicIo, i: usize) -> DataType {
    io.record_layout.axis_pts[i]
        .as_ref()
        .map(|e| e.data_type)
        .or_else(|| {
            io.record_layout
                .axis_rescale_x
                .as_ref()
                .map(|e| e.data_type)
        })
        .unwrap_or_else(|| fnc_data_type(io))
}

fn export_instance(
    value: &CharValue,
    io: &CharacteristicIo,
    opts: &CdfExportOptions,
) -> Result<Option<SwInstance>> {
    let ch = io.characteristic;
    let base = value.base();
    let char_type = ch.char_type;
    let fnc_dt = fnc_data_type(io);
    let tab_verb = io.conversion.method.conversion_type == ConversionType::TAB_VERB;
    let dc = decimal_count_of(ch.conv.format.as_deref())?;
    let text_of = |v: f64| -> Result<SwValue> {
        if tab_verb {
            let (text, _, _) = io.conversion.to_string_value(
                v,
                ValueObjectFormat::Physical,
                fnc_dt,
                dc,
                ch.conv.lower_limit,
                ch.conv.upper_limit,
            )?;
            Ok(SwValue::Vt(text))
        } else {
            Ok(SwValue::V(v))
        }
    };
    let mut cont = SwValueCont::default();
    if !base.unit.is_empty() {
        cont.unit_display_name = Some(base.unit.clone());
    }
    match char_type {
        CharacteristicType::VALUE => {
            let v = base
                .value
                .as_scalar()
                .ok_or_else(|| Error::Value("CDF export: scalar value not set".into()))?;
            if v.is_nan() {
                return Ok(None);
            }
            cont.sw_values_phys = Some(SwValuesPhys {
                items: vec![text_of(v)?],
            });
        }
        CharacteristicType::ASCII => {
            let text = base
                .value
                .as_text()
                .ok_or_else(|| Error::Value("CDF export: text value not set".into()))?;
            cont.sw_array_size = Some(SwArraySize {
                items: vec![number_of_elements(ch)? as i32],
            });
            let sanitized: String = text
                .chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .collect();
            cont.sw_values_phys = Some(SwValuesPhys {
                items: vec![SwValue::Vt(sanitized)],
            });
        }
        CharacteristicType::VAL_BLK => {
            let (_, data) = base
                .value
                .as_array()
                .ok_or_else(|| Error::Value("CDF export: array value not set".into()))?;
            if data.first().is_some_and(|v| v.is_nan()) {
                return Ok(None);
            }
            let n = number_of_elements(ch)?;
            cont.sw_array_size = Some(SwArraySize {
                items: vec![n as i32],
            });
            let mut items = Vec::with_capacity(n);
            for i in 0..n {
                let v = *data
                    .get(i)
                    .ok_or_else(|| Error::Value("CDF export: VAL_BLK value too short".into()))?;
                items.push(text_of(v)?);
            }
            cont.sw_values_phys = Some(SwValuesPhys { items });
        }
        CharacteristicType::CURVE => {
            let (_, data) = base
                .value
                .as_array()
                .ok_or_else(|| Error::Value("CDF export: array value not set".into()))?;
            if data.first().is_some_and(|v| v.is_nan()) {
                return Ok(None);
            }
            let mut items = Vec::with_capacity(data.len());
            for &v in data {
                items.push(text_of(v)?);
            }
            cont.sw_values_phys = Some(SwValuesPhys { items });
        }
        CharacteristicType::MAP
        | CharacteristicType::CUBOID
        | CharacteristicType::CUBE_4
        | CharacteristicType::CUBE_5 => {
            let (dims, data) = base
                .value
                .as_array()
                .ok_or_else(|| Error::Value("CDF export: array value not set".into()))?;
            if data.first().is_some_and(|v| v.is_nan()) {
                return Ok(None);
            }
            let axis_y = base.axis_value.get(1).map(|v| v.as_slice());
            let items = vg_tree(dims, data, axis_y, &text_of)?;
            cont.sw_values_phys = Some(SwValuesPhys { items });
        }
        other => {
            return Err(Error::Value(format!(
                "CDF export: unsupported char type {other:?}"
            )))
        }
    }
    let mut inst = SwInstance::new(ch.named.name.clone(), format!("{char_type:?}"), cont);
    if opts.add_parameter_desc {
        if let Some(d) = ch.named.description.as_deref().filter(|d| !d.is_empty()) {
            inst.long_name = Some(d.to_string());
        }
    }
    if opts.functions.is_some() {
        if let Some(f) = opts.def_function.and_then(|f| f(&ch.named.name)) {
            inst.sw_feature_ref = Some(f);
        }
    }
    if !io.axes.is_empty() {
        inst.sw_axis_conts = Some(SwAxisConts {
            items: export_axis_conts(value, io)?,
        });
    }
    Ok(Some(inst))
}

fn vg_tree(
    dims: &[usize],
    data: &[f64],
    axis_y: Option<&[f64]>,
    mk: &dyn Fn(f64) -> Result<SwValue>,
) -> Result<Vec<SwValue>> {
    if dims.len() < 2 {
        return Err(Error::Value(
            "CDF export: VG nesting requires >= 2 dims".into(),
        ));
    }
    let d0 = dims[0];
    let d1 = dims[1];
    if dims.len() == 2 {
        let axis_y =
            axis_y.ok_or_else(|| Error::Value("CDF export: MAP requires Y axis".into()))?;
        let mut out = Vec::with_capacity(d1);
        for y in 0..d1 {
            let mut items = Vec::with_capacity(d0);
            for x in 0..d0 {
                items.push(mk(data[x + y * d0])?);
            }
            let label = axis_y
                .get(y)
                .ok_or_else(|| Error::Value("CDF export: Y axis values too short".into()))?;
            out.push(SwValue::Vg(Vg::new(Some(format!("{label}")), items)));
        }
        Ok(out)
    } else {
        let last = *dims.last().unwrap();
        let sub: usize = dims[..dims.len() - 1].iter().product();
        let mut out = Vec::with_capacity(last);
        for i in 0..last {
            let inner = vg_tree(
                &dims[..dims.len() - 1],
                &data[i * sub..(i + 1) * sub],
                axis_y,
                mk,
            )?;
            out.push(SwValue::Vg(Vg::new(Some(i.to_string()), inner)));
        }
        Ok(out)
    }
}

fn export_axis_conts(value: &CharValue, io: &CharacteristicIo) -> Result<Vec<SwAxisCont>> {
    let base = value.base();
    let first_category = format!("{:?}", io.axes[0].descr.axis_type);
    let ctx = SingleValueContext {
        conversion: &io.conversion,
        data_type: fnc_data_type(io),
        axes: io
            .axes
            .iter()
            .enumerate()
            .map(|(i, a)| AxisContext {
                conversion: &a.conversion,
                data_type: axis_data_type(io, i),
            })
            .collect(),
    };
    let mut out = Vec::with_capacity(io.axes.len());
    for (i, axis) in io.axes.iter().enumerate() {
        let vals = base
            .axis_value
            .get(i)
            .ok_or_else(|| Error::Value(format!("CDF export: axis {i} values missing")))?;
        let tab_verb = axis.conversion.method.conversion_type == ConversionType::TAB_VERB;
        let mut items = Vec::with_capacity(vals.len());
        for (j, &v) in vals.iter().enumerate() {
            if tab_verb {
                items.push(SwValue::Vt(
                    base.to_single_value_at(j as i32, -1, 0, false, &ctx)?,
                ));
            } else {
                items.push(SwValue::V(v));
            }
        }
        let mut cont = SwAxisCont {
            category: Some(first_category.clone()),
            ..Default::default()
        };
        if base.unit_axis.first().is_some_and(|u| !u.is_empty()) {
            cont.unit_display_name = base.unit_axis.get(i).cloned();
        }
        cont.sw_values_phys = Some(SwValuesPhys { items });
        out.push(cont);
    }
    Ok(out)
}

// ===========================================================================
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use autors_a2l::model::base::{AddressFields, ByteOrder, NamedFields};
    use autors_a2l::model::characteristic::Characteristic;
    use autors_a2l::model::compu::CompuMethod;
    use autors_a2l::model::enums::{
        AddrType, DataType, DepositType, IndexMode, IndexOrder, MemoryPrgType, MonotonyType,
    };
    use autors_a2l::model::measurement::AxisDescr;
    use autors_a2l::model::record_layout::{
        AxisPtsLayoutDesc, FncValuesLayoutDesc, NoAxisPtsLayoutDesc, RecordLayout,
    };
    use autors_cdf::cdf::CdfFile;
    use autors_datafile::datafile::{DataFileBase, MemorySegment, MemorySegmentList};

    use crate::ecu_io::{AxisIo, DEFAULT_ALIGNMENTS};
    use crate::value::ValueObjectFormat;

    fn cm_identity() -> CompuMethod {
        CompuMethod {
            name: "CM_ID".into(),
            ..Default::default()
        }
    }

    fn descr(axis_type: AxisType, max: i32) -> AxisDescr {
        AxisDescr {
            axis_type,
            input_quantity: String::new(),
            conversion: "CM_ID".into(),
            max_axis_points: max,
            lower_limit: 0.0,
            upper_limit: 0.0,
            axis_pts_ref: None,
            curve_axis_ref: None,
            read_only: false,
            extended_limits: None,
            max_grad: None,
            step_size: None,
            format: None,
            phys_unit: None,
            byte_order: ByteOrder::NotSet,
            monotony: MonotonyType::NotSet,
            deposit: DepositType::NotSet,
            fix_axis_par: None,
            fix_axis_par_dist: None,
            children: Vec::new(),
        }
    }

    fn char_of(char_type: CharacteristicType, name: &str, address: u32) -> Characteristic {
        let mut ch = Characteristic {
            char_type,
            ..Default::default()
        };
        ch.named = NamedFields {
            name: name.into(),
            description: None,
        };
        ch.addr = AddressFields {
            address: Some(address),
            ..Default::default()
        };
        ch.conv.conversion = "CM_ID".into();
        ch.rec.record_layout = "RL".into();
        ch
    }

    fn no_axis_pts(
        name: &str,
        position: i32,
        data_type: DataType,
        axis_idx: i32,
    ) -> NoAxisPtsLayoutDesc {
        NoAxisPtsLayoutDesc {
            name: name.into(),
            position,
            data_type,
            axis_idx,
        }
    }

    fn axis_pts_desc(
        name: &str,
        position: i32,
        data_type: DataType,
        axis_idx: i32,
    ) -> AxisPtsLayoutDesc {
        AxisPtsLayoutDesc {
            name: name.into(),
            position,
            data_type,
            axis_idx,
            index_order: IndexOrder::INDEX_INCR,
            address_type: AddrType::DIRECT,
        }
    }

    fn fnc(position: i32, data_type: DataType, index_mode: IndexMode) -> FncValuesLayoutDesc {
        FncValuesLayoutDesc {
            name: "FNC_VALUES".into(),
            position,
            data_type,
            index_mode,
            address_type: AddrType::DIRECT,
        }
    }

    fn io_of<'a>(
        ch: &'a Characteristic,
        rl: &'a RecordLayout,
        conv: Conversion<'a>,
        axes: Vec<AxisIo<'a>>,
    ) -> CharacteristicIo<'a> {
        CharacteristicIo {
            characteristic: ch,
            record_layout: rl,
            conversion: conv,
            axes,
            default_alignments: DEFAULT_ALIGNMENTS,
            default_byte_order: ByteOrder::MSB_LAST,
        }
    }

    fn map_fixture() -> (Characteristic, RecordLayout, DataFileBase) {
        let rl = RecordLayout {
            name: "RL".into(),
            no_axis_pts: [
                Some(no_axis_pts("NO_AXIS_PTS_X", 1, DataType::UByte, 0)),
                Some(no_axis_pts("NO_AXIS_PTS_Y", 3, DataType::UByte, 1)),
                None,
                None,
                None,
            ],
            axis_pts: [
                Some(axis_pts_desc("AXIS_PTS_X", 2, DataType::UByte, 0)),
                Some(axis_pts_desc("AXIS_PTS_Y", 4, DataType::UWord, 1)),
                None,
                None,
                None,
            ],
            fnc_values: Some(fnc(5, DataType::UWord, IndexMode::ROW_DIR)),
            ..Default::default()
        };
        let ch = char_of(CharacteristicType::MAP, "KF", 0x4000);
        let bytes = vec![
            2, 1, 2, 3, 10, 0, 20, 0, 30, 0, 0, 0, 1, 0, 10, 0, 11, 0, 20, 0, 21, 0,
        ];
        let base = DataFileBase::new(
            None,
            MemorySegmentList::from_vec(vec![MemorySegment::from_data(
                0x4000,
                bytes,
                MemoryPrgType::DATA,
                true,
            )]),
        );
        (ch, rl, base)
    }

    fn msrsw_of(instances: Vec<SwInstance>) -> Msrsw {
        Msrsw {
            sw_systems: Some(SwSystems {
                items: vec![SwSystem {
                    short_name: None,
                    sw_instance_spec: SwInstanceSpec {
                        sw_instance_trees: vec![SwInstanceTree {
                            sw_instances: instances,
                            ..Default::default()
                        }],
                    },
                }],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn export_then_import_round_trip() {
        let cm = cm_identity();
        let (mut ch, rl, base) = map_fixture();
        ch.named.description = Some("test map".into());
        let conv = Conversion::new(&cm);
        let dx = descr(AxisType::STD_AXIS, 2);
        let dy = descr(AxisType::STD_AXIS, 3);
        let axes = vec![
            AxisIo {
                descr: &dx,
                conversion: conv,
                reference: None,
            },
            AxisIo {
                descr: &dy,
                conversion: conv,
                reference: None,
            },
        ];
        let io = io_of(&ch, &rl, conv, axes);
        let value =
            crate::ecu_io::get_value(&base.segment_list, &io, ValueObjectFormat::Physical, 0x4000)
                .unwrap()
                .unwrap();

        let def_function = |name: &str| (name == "KF").then(|| "F1".to_string());
        let opts = CdfExportOptions {
            a2l_source: Some("test.a2l".into()),
            data_source: Some("test.hex".into()),
            description: Some("desc".into()),
            functions: Some(vec!["F1".into()]),
            def_function: Some(&def_function),
            add_parameter_desc: true,
            short_name: "CDF".into(),
            creator: "autors".into(),
            creator_version: Some("1.0".into()),
        };
        let msrsw = export(&[(&value, &io)], &opts).unwrap();
        let inst = &msrsw.sw_systems.as_ref().unwrap().items[0]
            .sw_instance_spec
            .sw_instance_trees[0]
            .sw_instances[0];
        assert_eq!(inst.short_name.as_deref(), Some("KF"));
        assert_eq!(inst.category.as_deref(), Some("MAP"));
        assert_eq!(inst.long_name.as_deref(), Some("test map"));
        assert_eq!(inst.sw_feature_ref.as_deref(), Some("F1"));
        let vs = &inst
            .sw_value_cont
            .as_ref()
            .unwrap()
            .sw_values_phys
            .as_ref()
            .unwrap()
            .items;
        assert_eq!(vs.len(), 3);
        match &vs[0] {
            SwValue::Vg(g) => {
                assert_eq!(g.label.as_deref(), Some("10"));
                assert_eq!(g.vs, vec![SwValue::V(0.0), SwValue::V(1.0)]);
            }
            other => panic!("expected VG, got {other:?}"),
        }
        let conts = &inst.sw_axis_conts.as_ref().unwrap().items;
        assert_eq!(conts.len(), 2);
        assert_eq!(conts[0].category.as_deref(), Some("STD_AXIS"));
        assert_eq!(
            conts[1].sw_values_phys.as_ref().unwrap().items,
            vec![SwValue::V(10.0), SwValue::V(20.0), SwValue::V(30.0)]
        );

        // ---- XML round-trip ----
        let xml = CdfFile::write_string(&msrsw).unwrap();
        let msrsw2 = CdfFile::parse_str(&xml).unwrap();

        let lookup = |name: &str| -> Option<RecordLayoutRefIo> {
            if name == "KF" {
                Some(RecordLayoutRefIo::Characteristic(io.clone()))
            } else {
                None
            }
        };
        let result = import(&msrsw2, lookup);
        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
        assert_eq!(result.values.len(), 1);
        let imported = &result.values[0];
        assert_eq!(imported.base().value, value.base().value);
        assert_eq!(imported.base().axis_value, value.base().axis_value);
    }

    #[test]
    fn import_skips() {
        let cm = cm_identity();
        let rl = RecordLayout {
            name: "RL".into(),
            axis_pts: [
                Some(axis_pts_desc("AXIS_PTS_X", 1, DataType::UWord, 0)),
                None,
                None,
                None,
                None,
            ],
            fnc_values: Some(fnc(2, DataType::UWord, IndexMode::ROW_DIR)),
            ..Default::default()
        };
        let ch = char_of(CharacteristicType::CURVE, "C1", 0x2000);
        let conv = Conversion::new(&cm);
        let d = descr(AxisType::STD_AXIS, 4);
        let axes = vec![AxisIo {
            descr: &d,
            conversion: conv,
            reference: None,
        }];
        let io = io_of(&ch, &rl, conv, axes);

        let inst = |name: &str, cat: &str, n: usize, axis_n: usize| {
            let mut i = SwInstance::new(
                name,
                cat,
                SwValueCont {
                    sw_values_phys: Some(SwValuesPhys {
                        items: (1..=n).map(|k| SwValue::V(k as f64)).collect(),
                    }),
                    ..Default::default()
                },
            );
            i.sw_axis_conts = Some(SwAxisConts {
                items: vec![SwAxisCont {
                    sw_values_phys: Some(SwValuesPhys {
                        items: (5..5 + axis_n).map(|k| SwValue::V(k as f64)).collect(),
                    }),
                    ..Default::default()
                }],
            });
            i
        };
        let msrsw = msrsw_of(vec![
            inst("C1", "CURVE", 4, 4),
            inst("NOPE", "VALUE", 1, 0),
            inst("C1", "CURVE", 3, 3),
        ]);
        let lookup = |name: &str| -> Option<RecordLayoutRefIo> {
            if name == "C1" {
                Some(RecordLayoutRefIo::Characteristic(io.clone()))
            } else {
                None
            }
        };
        let result = import(&msrsw, lookup);
        assert_eq!(result.values.len(), 1);
        let (dims, data) = result.values[0].base().value.as_array().unwrap();
        assert_eq!(dims, &[4]);
        assert_eq!(data, &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(
            result.values[0].base().axis_value,
            vec![vec![5.0, 6.0, 7.0, 8.0]]
        );
        assert_eq!(
            result.skipped,
            vec![
                ("C1".to_string(), CdfErrorType::XAxisInvalid),
                ("NOPE".to_string(), CdfErrorType::NotFound),
            ]
        );
    }
}
