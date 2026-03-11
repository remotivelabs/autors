//! Object model and read/write support for ASAM CDF (Calibration Data Format)
//! XML files.
//! The model is serialized with `serde` + `quick-xml`; element and attribute
//! names follow the ASAM CDF V2.0.0 tag naming.
//! Scope: calibration-value import/export tied to an A2L model (which depends
//! on the autors-a2l A2L model and its value abstractions) is out of scope for
//! this module; it provides the CDF XML document object model and
//! `parse_str` / `write_string`-style read/write entry points.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// XML declaration prepended to written documents.
/// Files are written without a BOM (cross-platform convention).
const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>";

/// CDF 2.0 DOCTYPE included in written documents.
/// Uses the standard DOCTYPE from the ASAM CDF V2.0.0 specification.
const CDF_DOCTYPE: &str =
    "<!DOCTYPE MSRSW PUBLIC \"-//ASAM//DTD CALIBRATION DATA FORMAT VERSION 2.0.0//EN\" \"cdf_v2.0.0.sl.dtd\">";

/// Value entry inside `SW-VALUES-PHYS` / `VG`: `V` numeric value, `VT` text,
/// `VG` value group.
/// Modeled as an externally-tagged enum (the tag name is the variant name),
/// representing a heterogeneous element sequence.
/// Note: for axis containers the format declares only `V`/`VT` items; this
/// enum is shared between value and axis containers, so deserialization is
/// more permissive there (a `VG` item is not rejected).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SwValue {
    /// `<V>` numeric value (physical value, double).
    #[serde(rename = "V")]
    V(f64),
    /// `<VT>` text value (text table entry of a TAB_VERB, or ASCII text).
    #[serde(rename = "VT")]
    Vt(String),
    /// `<VG>` value group (nested grouping of MAP/CUBOID/CUBE_4/CUBE_5).
    #[serde(rename = "VG")]
    Vg(Vg),
}

/// `SW-ARRAYSIZE` array wrapper element; children are `<V>` (int).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwArraySize {
    /// Size of each dimension.
    #[serde(rename = "V", default)]
    pub items: Vec<i32>,
}

/// `SW-VALUES-PHYS` array wrapper element; children are heterogeneous value
/// entries (V/VT/VG).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwValuesPhys {
    /// Physical value entries (document order preserved).
    #[serde(rename = "$value", default)]
    pub items: Vec<SwValue>,
}

/// `SW-AXIS-CONTS` array wrapper element.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwAxisConts {
    /// Axis container list.
    #[serde(rename = "SW-AXIS-CONT", default)]
    pub items: Vec<SwAxisCont>,
}

/// `SW-CS-COLLECTIONS` array wrapper element.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwCsCollections {
    /// CS collection list.
    #[serde(rename = "SW-CS-COLLECTION", default)]
    pub items: Vec<SwCsCollection>,
}

/// `SW-SYSTEMS` array wrapper element.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwSystems {
    /// System list.
    #[serde(rename = "SW-SYSTEM", default)]
    pub items: Vec<SwSystem>,
}

/// CDF document root element (`MSRSW`).
/// No polymorphic substitution is modeled: every concrete type in this model
/// appears only in its declared position.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename = "MSRSW")]
pub struct Msrsw {
    /// `CREATOR` attribute: name of the creating tool.
    #[serde(rename = "@CREATOR", default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    /// `CREATOR-VERSION` attribute: version of the creating tool.
    #[serde(
        rename = "@CREATOR-VERSION",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub creator_version: Option<String>,
    /// `SHORT-NAME` element.
    #[serde(
        rename = "SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_name: Option<String>,
    /// `INTRODUCTION` element (omitted from the output when `None`).
    #[serde(
        rename = "INTRODUCTION",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub introduction: Option<String>,
    /// `CATEGORY` element (omitted from the output when `None`).
    #[serde(rename = "CATEGORY", default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `SW-SYSTEMS` array (omitted from the output when `None`).
    #[serde(
        rename = "SW-SYSTEMS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_systems: Option<SwSystems>,
}

impl Msrsw {
    /// Creates a document root with the given metadata.
    /// `category` is left as `None`; set it explicitly when needed.
    pub fn new(
        short_name: impl Into<String>,
        creator: impl Into<String>,
        creator_version: impl Into<String>,
        introduction: Option<String>,
    ) -> Self {
        Msrsw {
            creator: Some(creator.into()),
            creator_version: Some(creator_version.into()),
            short_name: Some(short_name.into()),
            // An empty introduction is treated as absent.
            introduction: introduction.filter(|s| !s.is_empty()),
            category: None,
            sw_systems: None,
        }
    }

    /// Collects all calibration instances under every
    /// SW-SYSTEM / SW-INSTANCE-TREE.
    pub fn all_instances(&self) -> Vec<&SwInstance> {
        let mut list = Vec::new();
        if let Some(systems) = &self.sw_systems {
            for system in &systems.items {
                for tree in &system.sw_instance_spec.sw_instance_trees {
                    list.extend(tree.sw_instances.iter());
                }
            }
        }
        list
    }
}

/// Physical axis value container.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwAxisCont {
    /// `CATEGORY` element (axis type, e.g. `STD_AXIS`/`COM_AXIS`).
    #[serde(rename = "CATEGORY", default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `UNIT-DISPLAY-NAME` element (omitted from the output when `None`).
    #[serde(
        rename = "UNIT-DISPLAY-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub unit_display_name: Option<String>,
    /// `SW-INSTANCE-REF` element: instance name referenced by a shared axis
    /// (COM_AXIS/CURVE_AXIS). Omitted from the output when `None`.
    #[serde(
        rename = "SW-INSTANCE-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_instance_ref: Option<String>,
    /// `SW-ARRAYSIZE` array.
    #[serde(
        rename = "SW-ARRAYSIZE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_array_size: Option<SwArraySize>,
    /// `SW-VALUES-PHYS` array (the format declares only `V`/`VT` items for
    /// axes).
    #[serde(
        rename = "SW-VALUES-PHYS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_values_phys: Option<SwValuesPhys>,
}

/// SW-CS-COLLECTION: the set of characteristic (function) references shared by
/// a group of calibration instances.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwCsCollection {
    /// `CATEGORY` element.
    #[serde(rename = "CATEGORY", default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `SW-FEATURE-REF` element: A2L FUNCTION name.
    #[serde(
        rename = "SW-FEATURE-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_feature_ref: Option<String>,
}

impl SwCsCollection {
    /// Creates a collection entry with the given category and feature
    /// reference.
    pub fn new(category: impl Into<String>, feature_ref: impl Into<String>) -> Self {
        SwCsCollection {
            category: Some(category.into()),
            sw_feature_ref: Some(feature_ref.into()),
        }
    }
}

/// A calibration instance (corresponds to a CDF CAL-ITEM).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwInstance {
    /// `SHORT-NAME` element: calibration object name.
    #[serde(
        rename = "SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_name: Option<String>,
    /// `LONG-NAME` element (may hold a description when saving; omitted from
    /// the output when `None`).
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    pub long_name: Option<String>,
    /// `CATEGORY` element (CHAR_TYPE name, e.g. VALUE/CURVE/MAP).
    #[serde(rename = "CATEGORY", default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `SW-FEATURE-REF` element (omitted from the output when `None`).
    #[serde(
        rename = "SW-FEATURE-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_feature_ref: Option<String>,
    /// `SW-VALUE-CONT` element (omitted from the output when `None`).
    #[serde(
        rename = "SW-VALUE-CONT",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_value_cont: Option<SwValueCont>,
    /// `SW-AXIS-CONTS` array.
    #[serde(
        rename = "SW-AXIS-CONTS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_axis_conts: Option<SwAxisConts>,
}

impl SwInstance {
    /// Creates an instance with the given name, category, and value container.
    pub fn new(
        short_name: impl Into<String>,
        category: impl Into<String>,
        value_cont: SwValueCont,
    ) -> Self {
        SwInstance {
            short_name: Some(short_name.into()),
            category: Some(category.into()),
            sw_value_cont: Some(value_cont),
            ..Default::default()
        }
    }
}

/// SW-INSTANCE-SPEC: the section of an SWSYSTEM carrying all data instance
/// information. It is always serialized, even when empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwInstanceSpec {
    /// Repeated `SW-INSTANCE-TREE` elements (no array wrapper).
    #[serde(rename = "SW-INSTANCE-TREE", default)]
    pub sw_instance_trees: Vec<SwInstanceTree>,
}

/// A tree of calibration instances.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwInstanceTree {
    /// `SHORT-NAME` element.
    #[serde(
        rename = "SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_name: Option<String>,
    /// `CATEGORY` element (omitted from the output when `None`).
    #[serde(rename = "CATEGORY", default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `SW-INSTANCE-TREE-ORIGIN` element (omitted from the output when `None`).
    #[serde(
        rename = "SW-INSTANCE-TREE-ORIGIN",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_instance_tree_origin: Option<SwInstanceTreeOrigin>,
    /// `SW-CS-COLLECTIONS` array (omitted from the output when `None`).
    #[serde(
        rename = "SW-CS-COLLECTIONS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_cs_collections: Option<SwCsCollections>,
    /// Repeated `SW-INSTANCE` elements (no array wrapper).
    #[serde(rename = "SW-INSTANCE", default)]
    pub sw_instances: Vec<SwInstance>,
}

impl SwInstanceTree {
    /// Creates a tree with the given metadata, origin, CS collections, and
    /// instances.
    pub fn new(
        short_name: impl Into<String>,
        category: impl Into<String>,
        origin: SwInstanceTreeOrigin,
        cs_collections: Option<Vec<SwCsCollection>>,
        instances: Vec<SwInstance>,
    ) -> Self {
        SwInstanceTree {
            short_name: Some(short_name.into()),
            category: Some(category.into()),
            sw_instance_tree_origin: Some(origin),
            sw_cs_collections: cs_collections.map(|items| SwCsCollections { items }),
            sw_instances: instances,
        }
    }
}

/// Instance tree origin (symbol file / data file).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwInstanceTreeOrigin {
    /// `SYMBOLIC-FILE` element: A2L source file name (omitted from the output
    /// when `None`).
    #[serde(
        rename = "SYMBOLIC-FILE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub symbolic_file: Option<String>,
    /// `DATA-FILE` element: data (HEX) source file name (omitted from the
    /// output when `None`).
    #[serde(rename = "DATA-FILE", default, skip_serializing_if = "Option::is_none")]
    pub data_file: Option<String>,
}

impl SwInstanceTreeOrigin {
    /// Creates an origin from optional symbol and data file names.
    pub fn new(symbolic_file: Option<String>, data_file: Option<String>) -> Self {
        SwInstanceTreeOrigin {
            symbolic_file,
            data_file,
        }
    }
}

/// A system described by the file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwSystem {
    /// `SHORT-NAME` element.
    #[serde(
        rename = "SHORT-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub short_name: Option<String>,
    /// `SW-INSTANCE-SPEC` element (always written out).
    #[serde(rename = "SW-INSTANCE-SPEC", default)]
    pub sw_instance_spec: SwInstanceSpec,
}

impl SwSystem {
    /// Creates a system wrapping the given instance spec.
    /// `short_name` is left as `None`; set it explicitly when needed.
    pub fn new(spec: SwInstanceSpec) -> Self {
        SwSystem {
            short_name: None,
            sw_instance_spec: spec,
        }
    }
}

/// Physical value container.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwValueCont {
    /// `UNIT-DISPLAY-NAME` element (omitted from the output when `None`).
    #[serde(
        rename = "UNIT-DISPLAY-NAME",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub unit_display_name: Option<String>,
    /// `SW-ARRAYSIZE` array (child `<V>` items are int).
    #[serde(
        rename = "SW-ARRAYSIZE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_array_size: Option<SwArraySize>,
    /// `SW-VALUES-PHYS` array (children V/VT/VG).
    #[serde(
        rename = "SW-VALUES-PHYS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sw_values_phys: Option<SwValuesPhys>,
}

/// Value group: holds numbers, text, or nested groups.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Vg {
    /// `LABEL` element (holds the axis point value or dimension index when
    /// saving; omitted from the output when `None`).
    #[serde(rename = "LABEL", default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Direct child entries (heterogeneous `VG`/`VT`/`V` sequence, no array
    /// wrapper).
    #[serde(rename = "$value", default)]
    pub vs: Vec<SwValue>,
}

impl Vg {
    /// Creates a value group with the given label and child entries.
    pub fn new(label: Option<String>, vs: Vec<SwValue>) -> Self {
        Vg { label, vs }
    }
}

/// Facade for reading and writing CDF files.
/// Calibration-value conversion in interaction with an A2L model is out of
/// scope (see the module-level scope note).
pub struct CdfFile;

impl CdfFile {
    /// Parses a CDF document from an XML string.
    /// The DOCTYPE is skipped during parsing.
    pub fn parse_str(xml: &str) -> Result<Msrsw> {
        quick_xml::de::from_str(xml).map_err(|e| Error::Xml(e.to_string()))
    }

    /// Serializes a CDF document to a complete XML string (including the XML
    /// declaration and the CDF DOCTYPE).
    /// The output is indented with tabs; line endings are `\n` and no BOM is
    /// written.
    pub fn write_string(msrsw: &Msrsw) -> Result<String> {
        let mut body = String::new();
        let mut ser = quick_xml::se::Serializer::new(&mut body);
        ser.indent('\t', 1);
        msrsw
            .serialize(ser)
            .map_err(|e| Error::Xml(e.to_string()))?;
        Ok(format!("{XML_DECL}\n{CDF_DOCTYPE}\n{body}"))
    }

    /// Convenience entry point: reads a file and parses it.
    pub fn load(path: impl AsRef<Path>) -> Result<Msrsw> {
        let xml = std::fs::read_to_string(path)?;
        Self::parse_str(&xml)
    }

    /// Convenience entry point: serializes and writes to a file.
    pub fn save(path: impl AsRef<Path>, msrsw: &Msrsw) -> Result<()> {
        std::fs::write(path, Self::write_string(msrsw)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal CDF sample covering the main types (with XML declaration and
    /// DOCTYPE).
    const SAMPLE: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
        "<!DOCTYPE MSRSW PUBLIC \"-//ASAM//DTD CALIBRATION DATA FORMAT VERSION 2.0.0//EN\" \"cdf_v2.0.0.sl.dtd\">\n",
        "<MSRSW CREATOR=\"autors\" CREATOR-VERSION=\"1.0\">\n",
        "\t<SHORT-NAME>Sample</SHORT-NAME>\n",
        "\t<INTRODUCTION>demo</INTRODUCTION>\n",
        "\t<CATEGORY>CDF20</CATEGORY>\n",
        "\t<SW-SYSTEMS>\n",
        "\t\t<SW-SYSTEM>\n",
        "\t\t\t<SHORT-NAME>System1</SHORT-NAME>\n",
        "\t\t\t<SW-INSTANCE-SPEC>\n",
        "\t\t\t\t<SW-INSTANCE-TREE>\n",
        "\t\t\t\t\t<SHORT-NAME>Tree1</SHORT-NAME>\n",
        "\t\t\t\t\t<CATEGORY>CAL</CATEGORY>\n",
        "\t\t\t\t\t<SW-INSTANCE-TREE-ORIGIN>\n",
        "\t\t\t\t\t\t<SYMBOLIC-FILE>proj.a2l</SYMBOLIC-FILE>\n",
        "\t\t\t\t\t\t<DATA-FILE>proj.hex</DATA-FILE>\n",
        "\t\t\t\t\t</SW-INSTANCE-TREE-ORIGIN>\n",
        "\t\t\t\t\t<SW-CS-COLLECTIONS>\n",
        "\t\t\t\t\t\t<SW-CS-COLLECTION>\n",
        "\t\t\t\t\t\t\t<CATEGORY>FEATURE</CATEGORY>\n",
        "\t\t\t\t\t\t\t<SW-FEATURE-REF>Fnc1</SW-FEATURE-REF>\n",
        "\t\t\t\t\t\t</SW-CS-COLLECTION>\n",
        "\t\t\t\t\t</SW-CS-COLLECTIONS>\n",
        "\t\t\t\t\t<SW-INSTANCE>\n",
        "\t\t\t\t\t\t<SHORT-NAME>KFMAP</SHORT-NAME>\n",
        "\t\t\t\t\t\t<LONG-NAME>demo map</LONG-NAME>\n",
        "\t\t\t\t\t\t<CATEGORY>MAP</CATEGORY>\n",
        "\t\t\t\t\t\t<SW-FEATURE-REF>Fnc1</SW-FEATURE-REF>\n",
        "\t\t\t\t\t\t<SW-VALUE-CONT>\n",
        "\t\t\t\t\t\t\t<UNIT-DISPLAY-NAME>rpm</UNIT-DISPLAY-NAME>\n",
        "\t\t\t\t\t\t\t<SW-ARRAYSIZE>\n",
        "\t\t\t\t\t\t\t\t<V>2</V>\n",
        "\t\t\t\t\t\t\t\t<V>2</V>\n",
        "\t\t\t\t\t\t\t</SW-ARRAYSIZE>\n",
        "\t\t\t\t\t\t\t<SW-VALUES-PHYS>\n",
        "\t\t\t\t\t\t\t\t<VG>\n",
        "\t\t\t\t\t\t\t\t\t<LABEL>1000</LABEL>\n",
        "\t\t\t\t\t\t\t\t\t<V>1.5</V>\n",
        "\t\t\t\t\t\t\t\t\t<V>2.5</V>\n",
        "\t\t\t\t\t\t\t\t</VG>\n",
        "\t\t\t\t\t\t\t\t<VG>\n",
        "\t\t\t\t\t\t\t\t\t<LABEL>2000</LABEL>\n",
        "\t\t\t\t\t\t\t\t\t<VT>a&amp;b</VT>\n",
        "\t\t\t\t\t\t\t\t\t<VT>off</VT>\n",
        "\t\t\t\t\t\t\t\t</VG>\n",
        "\t\t\t\t\t\t\t</SW-VALUES-PHYS>\n",
        "\t\t\t\t\t\t</SW-VALUE-CONT>\n",
        "\t\t\t\t\t\t<SW-AXIS-CONTS>\n",
        "\t\t\t\t\t\t\t<SW-AXIS-CONT>\n",
        "\t\t\t\t\t\t\t\t<CATEGORY>STD_AXIS</CATEGORY>\n",
        "\t\t\t\t\t\t\t\t<UNIT-DISPLAY-NAME>rpm</UNIT-DISPLAY-NAME>\n",
        "\t\t\t\t\t\t\t\t<SW-ARRAYSIZE>\n",
        "\t\t\t\t\t\t\t\t\t<V>2</V>\n",
        "\t\t\t\t\t\t\t\t</SW-ARRAYSIZE>\n",
        "\t\t\t\t\t\t\t\t<SW-VALUES-PHYS>\n",
        "\t\t\t\t\t\t\t\t\t<V>1000</V>\n",
        "\t\t\t\t\t\t\t\t\t<V>2000</V>\n",
        "\t\t\t\t\t\t\t\t</SW-VALUES-PHYS>\n",
        "\t\t\t\t\t\t\t</SW-AXIS-CONT>\n",
        "\t\t\t\t\t\t\t<SW-AXIS-CONT>\n",
        "\t\t\t\t\t\t\t\t<CATEGORY>COM_AXIS</CATEGORY>\n",
        "\t\t\t\t\t\t\t\t<SW-INSTANCE-REF>NPED</SW-INSTANCE-REF>\n",
        "\t\t\t\t\t\t\t</SW-AXIS-CONT>\n",
        "\t\t\t\t\t\t</SW-AXIS-CONTS>\n",
        "\t\t\t\t\t</SW-INSTANCE>\n",
        "\t\t\t\t</SW-INSTANCE-TREE>\n",
        "\t\t\t</SW-INSTANCE-SPEC>\n",
        "\t\t</SW-SYSTEM>\n",
        "\t</SW-SYSTEMS>\n",
        "</MSRSW>",
    );

    fn sample_tree(doc: &Msrsw) -> &SwInstanceTree {
        &doc.sw_systems.as_ref().unwrap().items[0]
            .sw_instance_spec
            .sw_instance_trees[0]
    }

    #[test]
    fn parse_minimal_cdf() {
        let doc = CdfFile::parse_str(SAMPLE).unwrap();
        assert_eq!(doc.creator.as_deref(), Some("autors"));
        assert_eq!(doc.creator_version.as_deref(), Some("1.0"));
        assert_eq!(doc.short_name.as_deref(), Some("Sample"));
        assert_eq!(doc.introduction.as_deref(), Some("demo"));
        assert_eq!(doc.category.as_deref(), Some("CDF20"));

        let tree = sample_tree(&doc);
        assert_eq!(tree.short_name.as_deref(), Some("Tree1"));
        assert_eq!(tree.category.as_deref(), Some("CAL"));
        let origin = tree.sw_instance_tree_origin.as_ref().unwrap();
        assert_eq!(origin.symbolic_file.as_deref(), Some("proj.a2l"));
        assert_eq!(origin.data_file.as_deref(), Some("proj.hex"));
        let cs = &tree.sw_cs_collections.as_ref().unwrap().items;
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].category.as_deref(), Some("FEATURE"));
        assert_eq!(cs[0].sw_feature_ref.as_deref(), Some("Fnc1"));

        assert_eq!(tree.sw_instances.len(), 1);
        let inst = &tree.sw_instances[0];
        assert_eq!(inst.short_name.as_deref(), Some("KFMAP"));
        assert_eq!(inst.long_name.as_deref(), Some("demo map"));
        assert_eq!(inst.category.as_deref(), Some("MAP"));
        assert_eq!(inst.sw_feature_ref.as_deref(), Some("Fnc1"));

        let vc = inst.sw_value_cont.as_ref().unwrap();
        assert_eq!(vc.unit_display_name.as_deref(), Some("rpm"));
        assert_eq!(vc.sw_array_size.as_ref().unwrap().items, vec![2, 2]);
        let vs = &vc.sw_values_phys.as_ref().unwrap().items;
        assert_eq!(vs.len(), 2);
        match &vs[0] {
            SwValue::Vg(vg) => {
                assert_eq!(vg.label.as_deref(), Some("1000"));
                assert_eq!(vg.vs, vec![SwValue::V(1.5), SwValue::V(2.5)]);
            }
            other => panic!("expected VG, got {other:?}"),
        }
        match &vs[1] {
            SwValue::Vg(vg) => {
                assert_eq!(vg.label.as_deref(), Some("2000"));
                assert_eq!(
                    vg.vs,
                    vec![SwValue::Vt("a&b".into()), SwValue::Vt("off".into())]
                );
            }
            other => panic!("expected VG, got {other:?}"),
        }

        let axes = &inst.sw_axis_conts.as_ref().unwrap().items;
        assert_eq!(axes.len(), 2);
        assert_eq!(axes[0].category.as_deref(), Some("STD_AXIS"));
        assert_eq!(axes[0].unit_display_name.as_deref(), Some("rpm"));
        assert_eq!(axes[0].sw_array_size.as_ref().unwrap().items, vec![2]);
        assert_eq!(
            axes[0].sw_values_phys.as_ref().unwrap().items,
            vec![SwValue::V(1000.0), SwValue::V(2000.0)]
        );
        assert_eq!(axes[1].category.as_deref(), Some("COM_AXIS"));
        assert_eq!(axes[1].sw_instance_ref.as_deref(), Some("NPED"));
        assert!(axes[1].sw_values_phys.is_none());
    }

    #[test]
    fn round_trip_semantic() {
        let doc = CdfFile::parse_str(SAMPLE).unwrap();
        let xml = CdfFile::write_string(&doc).unwrap();
        // Output carries the XML declaration and DOCTYPE, indented with tabs.
        assert!(xml.starts_with(XML_DECL));
        assert!(xml.contains(CDF_DOCTYPE));
        assert!(xml.contains("<MSRSW CREATOR=\"autors\" CREATOR-VERSION=\"1.0\">"));
        assert!(xml.contains("\n\t<SHORT-NAME>Sample</SHORT-NAME>"));
        // Re-parsing yields a semantically equal document.
        let doc2 = CdfFile::parse_str(&xml).unwrap();
        assert_eq!(doc, doc2);
    }

    #[test]
    fn round_trip_minimal_document() {
        // Minimal document with only the root element.
        let xml = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<MSRSW/>";
        let doc = CdfFile::parse_str(xml).unwrap();
        assert_eq!(doc, Msrsw::default());
        let out = CdfFile::write_string(&doc).unwrap();
        assert!(out.contains("<MSRSW/>"));
        assert_eq!(CdfFile::parse_str(&out).unwrap(), doc);
    }

    #[test]
    fn write_omits_absent_members() {
        // `None` fields are not written out.
        let mut doc = Msrsw::new("S", "tool", "2.0", None);
        doc.sw_systems = Some(SwSystems {
            items: vec![SwSystem::new(SwInstanceSpec::default())],
        });
        let xml = CdfFile::write_string(&doc).unwrap();
        assert!(!xml.contains("INTRODUCTION"));
        assert!(!xml.contains("CATEGORY"));
        // SW-INSTANCE-SPEC is always written out, even when empty.
        assert!(xml.contains("<SW-INSTANCE-SPEC/>"));
        assert_eq!(CdfFile::parse_str(&xml).unwrap(), doc);
    }

    #[test]
    fn sw_value_enum_tags() {
        let phys = SwValuesPhys {
            items: vec![
                SwValue::V(1.5),
                SwValue::Vt("txt".into()),
                SwValue::Vg(Vg::new(Some("L".into()), vec![SwValue::V(3.0)])),
            ],
        };
        let mut buf = String::new();
        let ser = quick_xml::se::Serializer::new(&mut buf);
        phys.serialize(ser).unwrap();
        assert!(buf.contains("<V>1.5</V>"));
        assert!(buf.contains("<VT>txt</VT>"));
        assert!(buf.contains("<VG><LABEL>L</LABEL><V>3</V></VG>"));
    }

    #[test]
    fn all_instances_collects_across_trees() {
        let mut doc = Msrsw::default();
        assert!(doc.all_instances().is_empty());
        let tree1 = SwInstanceTree {
            sw_instances: vec![SwInstance::new("A", "VALUE", SwValueCont::default())],
            ..Default::default()
        };
        let tree2 = SwInstanceTree {
            sw_instances: vec![
                SwInstance::new("B", "CURVE", SwValueCont::default()),
                SwInstance::new("C", "MAP", SwValueCont::default()),
            ],
            ..Default::default()
        };
        doc.sw_systems = Some(SwSystems {
            items: vec![SwSystem {
                short_name: None,
                sw_instance_spec: SwInstanceSpec {
                    sw_instance_trees: vec![tree1, tree2],
                },
            }],
        });
        let names: Vec<_> = doc
            .all_instances()
            .iter()
            .map(|i| i.short_name.as_deref().unwrap())
            .collect();
        assert_eq!(names, ["A", "B", "C"]);
    }

    #[test]
    fn constructors_set_expected_fields() {
        let cs = SwCsCollection::new("CAT", "F");
        assert_eq!(cs.category.as_deref(), Some("CAT"));
        assert_eq!(cs.sw_feature_ref.as_deref(), Some("F"));

        let inst = SwInstance::new("X", "VALUE", SwValueCont::default());
        assert_eq!(inst.short_name.as_deref(), Some("X"));
        assert_eq!(inst.category.as_deref(), Some("VALUE"));
        assert!(inst.sw_value_cont.is_some());

        let origin = SwInstanceTreeOrigin::new(Some("a.a2l".into()), None);
        assert_eq!(origin.symbolic_file.as_deref(), Some("a.a2l"));
        assert!(origin.data_file.is_none());

        // An empty introduction is treated as absent.
        let doc = Msrsw::new("S", "c", "v", Some(String::new()));
        assert!(doc.introduction.is_none());
        let doc = Msrsw::new("S", "c", "v", Some("intro".to_string()));
        assert_eq!(doc.introduction.as_deref(), Some("intro"));
    }

    #[test]
    fn file_round_trip() {
        let doc = CdfFile::parse_str(SAMPLE).unwrap();
        let mut path = std::env::temp_dir();
        path.push(format!("autors_cdf_test_{}.cdf", std::process::id()));
        CdfFile::save(&path, &doc).unwrap();
        let loaded = CdfFile::load(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(doc, loaded);
    }

    #[test]
    fn parse_rejects_invalid_xml() {
        assert!(CdfFile::parse_str("not xml").is_err());
        assert!(CdfFile::parse_str("<MSRSW><SHORT-NAME>x</MSRSW>").is_err());
    }
}
