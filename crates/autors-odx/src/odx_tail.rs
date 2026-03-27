// Vehicle-information data model.

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoComponentRefs {
    #[serde(rename = "INFO-COMPONENT-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VehicleConnector {
    #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    pub long_name: Option<String>,
    #[serde(
        rename = "VEHICLE-CONNECTOR-PINS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub vehicle_connector_pins: Option<VehicleConnectorPins>,
}

impl VehicleConnector {
    pub fn display_name(&self) -> &str {
        match &self.long_name {
            Some(name) if !name.is_empty() => name,
            _ => self.short_name.as_deref().unwrap_or_default(),
        }
    }
}

named_id_struct! {
    pub struct VehicleConnectorPin {
        attrs {
            #[serde(rename = "@TYPE", default)]
            pub type_: PinType,
        }
        #[serde(rename = "PIN-NUMBER", default)]
        pub pin_number: i32,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VehicleConnectorPins {
    #[serde(rename = "VEHICLE-CONNECTOR-PIN", default)]
    pub items: Vec<VehicleConnectorPin>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VehicleConnectors {
    #[serde(rename = "VEHICLE-CONNECTOR", default)]
    pub items: Vec<VehicleConnector>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VehicleConnectorPinRefs {
    #[serde(rename = "VEHICLE-CONNECTOR-PIN-REF", default)]
    pub items: Vec<IdRef>,
}

named_id_struct! {
    pub struct PkysicalVehicleLink {
        attrs {
            #[serde(rename = "@TYPE", default, skip_serializing_if = "Option::is_none")]
            pub type_: Option<String>,
        }
        #[serde(
            rename = "VEHICLE-CONNECTOR-PIN-REFS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub vehicle_connector_pin_refs: Option<VehicleConnectorPinRefs>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhysicalVehicleLinks {
    #[serde(rename = "PHYSICAL-VEHICLE-LINK", default)]
    pub items: Vec<PkysicalVehicleLink>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayLogicalLinkRefs {
    #[serde(rename = "GATEWAY-LOGICAL-LINK-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogicalLinkBase {
    #[serde(rename = "@ID", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "GATEWAY-LOGICAL-LINK-REFS", default)]
    pub gateway_logical_link_refs: GatewayLogicalLinkRefs,
    #[serde(
        rename = "PHYSICAL-VEHICLE-LINK-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub physical_vehicle_link_ref: Option<IdRef>,
    #[serde(rename = "PROTOCOL-REF", default, skip_serializing_if = "Option::is_none")]
    pub protocol_ref: Option<ProtocolRef>,
    #[serde(
        rename = "BASE-VARIANT-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub base_variant_ref: Option<ProtocolRef>,
    #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    pub long_name: Option<String>,
    #[serde(rename = "SDGS", default, skip_serializing_if = "Option::is_none")]
    pub sdgs: Option<Sdgs>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LogicalLinkKind {
    Member(LogicalLinkBase),
    Gateway {
        base: LogicalLinkBase,
        semantic: Option<String>,
    },
}

impl Default for LogicalLinkKind {
    fn default() -> Self {
        LogicalLinkKind::Member(LogicalLinkBase::default())
    }
}

impl LogicalLinkKind {
    fn base(&self) -> &LogicalLinkBase {
        match self {
            LogicalLinkKind::Member(base) | LogicalLinkKind::Gateway { base, .. } => base,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct RawLogicalLink {
    #[serde(
        rename(serialize = "@xsi:type", deserialize = "@type"),
        default,
        skip_serializing_if = "Option::is_none"
    )]
    xsi_type: Option<String>,
    #[serde(rename = "@ID", default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "@SEMANTIC", default, skip_serializing_if = "Option::is_none")]
    semantic: Option<String>,
    #[serde(rename = "GATEWAY-LOGICAL-LINK-REFS", default)]
    gateway_logical_link_refs: GatewayLogicalLinkRefs,
    #[serde(
        rename = "PHYSICAL-VEHICLE-LINK-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    physical_vehicle_link_ref: Option<IdRef>,
    #[serde(rename = "PROTOCOL-REF", default, skip_serializing_if = "Option::is_none")]
    protocol_ref: Option<ProtocolRef>,
    #[serde(
        rename = "BASE-VARIANT-REF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    base_variant_ref: Option<ProtocolRef>,
    #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
    short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    long_name: Option<String>,
    #[serde(rename = "SDGS", default, skip_serializing_if = "Option::is_none")]
    sdgs: Option<Sdgs>,
}

impl From<&LogicalLinkKind> for RawLogicalLink {
    fn from(value: &LogicalLinkKind) -> Self {
        let base = value.base();
        RawLogicalLink {
            xsi_type: Some(match value {
                LogicalLinkKind::Member(_) => "MEMBER-LOGICAL-LINK",
                LogicalLinkKind::Gateway { .. } => "GATEWAY-LOGICAL-LINK",
            }
            .to_owned()),
            id: base.id.clone(),
            semantic: match value {
                LogicalLinkKind::Gateway { semantic, .. } => semantic.clone(),
                LogicalLinkKind::Member(_) => None,
            },
            gateway_logical_link_refs: base.gateway_logical_link_refs.clone(),
            physical_vehicle_link_ref: base.physical_vehicle_link_ref.clone(),
            protocol_ref: base.protocol_ref.clone(),
            base_variant_ref: base.base_variant_ref.clone(),
            short_name: base.short_name.clone(),
            long_name: base.long_name.clone(),
            sdgs: base.sdgs.clone(),
        }
    }
}

impl Serialize for LogicalLinkKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        RawLogicalLink::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LogicalLinkKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = RawLogicalLink::deserialize(deserializer)?;
        let base = LogicalLinkBase {
            id: raw.id,
            gateway_logical_link_refs: raw.gateway_logical_link_refs,
            physical_vehicle_link_ref: raw.physical_vehicle_link_ref,
            protocol_ref: raw.protocol_ref,
            base_variant_ref: raw.base_variant_ref,
            short_name: raw.short_name,
            long_name: raw.long_name,
            sdgs: raw.sdgs,
        };
        Ok(match raw.xsi_type.as_deref() {
            Some("GATEWAY-LOGICAL-LINK") => LogicalLinkKind::Gateway {
                base,
                semantic: raw.semantic,
            },
            Some("MEMBER-LOGICAL-LINK") | None => LogicalLinkKind::Member(base),
            Some(other) => {
                return Err(serde::de::Error::custom(format!(
                    "LOGICAL-LINK: unsupported xsi:type {other}"
                )))
            }
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogicalLinks {
    #[serde(rename = "LOGICAL-LINK", default)]
    pub items: Vec<LogicalLinkKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VehicleInformation {
    #[serde(
        rename = "INFO-COMPONENT-REFS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub info_component_refs: Option<InfoComponentRefs>,
    #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    pub long_name: Option<String>,
    #[serde(
        rename = "VEHICLE-CONNECTORS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub vehicle_connectors: Option<VehicleConnectors>,
    #[serde(
        rename = "PHYSICAL-VEHICLE-LINKS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub physical_vehicle_links: Option<PhysicalVehicleLinks>,
    #[serde(rename = "LOGICAL-LINKS", default, skip_serializing_if = "Option::is_none")]
    pub logical_links: Option<LogicalLinks>,
}

impl VehicleInformation {
    pub fn display_name(&self) -> &str {
        match &self.long_name {
            Some(name) if !name.is_empty() => name,
            _ => self.short_name.as_deref().unwrap_or_default(),
        }
    }
}

// Flash and memory data model.

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeValueElement {
    #[serde(rename = "@TYPE", default)]
    pub type_: BaseDataType,
    #[serde(rename = "$text", default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataFile {
    #[serde(rename = "@LATEBOUND-DATAFILE", default)]
    pub latebound_datafile: bool,
    #[serde(rename = "$text", default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataFormat {
    #[serde(rename = "@SELECTION", default, with = "data_format_type_serde")]
    pub selection: DataFormatType,
}

mod data_format_type_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &super::DataFormatType,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<super::DataFormatType, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(super::DataFormatType::from_odx_name(&value))
    }
}

named_id_struct! {
    pub struct FlashDataIntern {
        #[serde(rename = "DATAFORMAT", default)]
        pub dataformat: DataFormat,
        #[serde(rename = "ENCRYPT-COMPRESS-METHOD", default)]
        pub encrypt_compress_method: TypeValueElement,
        #[serde(rename = "DATA", default, skip_serializing_if = "Option::is_none")]
        pub data: Option<String>,
    }
}

impl FlashDataIntern {
    pub fn data_bytes(&self) -> Vec<u8> {
        self.data.as_deref().map(hex_to_bytes).unwrap_or_default()
    }

    pub fn set_data_bytes(&mut self, bytes: &[u8]) {
        self.data = (!bytes.is_empty()).then(|| bytes_to_hex(bytes));
    }
}

named_id_struct! {
    pub struct FlashDataExtern {
        #[serde(rename = "DATAFORMAT", default)]
        pub dataformat: DataFormat,
        #[serde(rename = "DATAFILE", default)]
        pub datafile: DataFile,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FlashData {
    Plain(NamedIdData),
    Intern(FlashDataIntern),
    Extern(FlashDataExtern),
}

impl Default for FlashData {
    fn default() -> Self {
        FlashData::Plain(NamedIdData::default())
    }
}

#[derive(Default, Serialize, Deserialize)]
struct RawFlashData {
    #[serde(
        rename(serialize = "@xsi:type", deserialize = "@type"),
        default,
        skip_serializing_if = "Option::is_none"
    )]
    xsi_type: Option<String>,
    #[serde(rename = "@ID", default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
    short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    long_name: Option<String>,
    #[serde(rename = "SDGS", default, skip_serializing_if = "Option::is_none")]
    sdgs: Option<Sdgs>,
    #[serde(rename = "DATAFORMAT", default, skip_serializing_if = "Option::is_none")]
    dataformat: Option<DataFormat>,
    #[serde(rename = "DATAFILE", default, skip_serializing_if = "Option::is_none")]
    datafile: Option<DataFile>,
    #[serde(
        rename = "ENCRYPT-COMPRESS-METHOD",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    encrypt_compress_method: Option<TypeValueElement>,
    #[serde(rename = "DATA", default, skip_serializing_if = "Option::is_none")]
    data: Option<String>,
}

impl From<&FlashData> for RawFlashData {
    fn from(value: &FlashData) -> Self {
        match value {
            FlashData::Plain(data) => RawFlashData {
                id: data.id.clone(),
                short_name: data.short_name.clone(),
                long_name: data.long_name.clone(),
                sdgs: data.sdgs.clone(),
                ..Default::default()
            },
            FlashData::Intern(data) => RawFlashData {
                xsi_type: Some("INTERN-FLASHDATA".to_owned()),
                id: data.id.clone(),
                short_name: data.short_name.clone(),
                long_name: data.long_name.clone(),
                sdgs: data.sdgs.clone(),
                dataformat: Some(data.dataformat.clone()),
                encrypt_compress_method: Some(data.encrypt_compress_method.clone()),
                data: data.data.clone(),
                ..Default::default()
            },
            FlashData::Extern(data) => RawFlashData {
                xsi_type: Some("EXTERN-FLASHDATA".to_owned()),
                id: data.id.clone(),
                short_name: data.short_name.clone(),
                long_name: data.long_name.clone(),
                sdgs: data.sdgs.clone(),
                dataformat: Some(data.dataformat.clone()),
                datafile: Some(data.datafile.clone()),
                ..Default::default()
            },
        }
    }
}

impl Serialize for FlashData {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        RawFlashData::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FlashData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = RawFlashData::deserialize(deserializer)?;
        Ok(match raw.xsi_type.as_deref() {
            Some("INTERN-FLASHDATA") => FlashData::Intern(FlashDataIntern {
                id: raw.id,
                short_name: raw.short_name,
                long_name: raw.long_name,
                sdgs: raw.sdgs,
                dataformat: raw.dataformat.unwrap_or_default(),
                encrypt_compress_method: raw.encrypt_compress_method.unwrap_or_default(),
                data: raw.data,
            }),
            Some("EXTERN-FLASHDATA") => FlashData::Extern(FlashDataExtern {
                id: raw.id,
                short_name: raw.short_name,
                long_name: raw.long_name,
                sdgs: raw.sdgs,
                dataformat: raw.dataformat.unwrap_or_default(),
                datafile: raw.datafile.unwrap_or_default(),
            }),
            None => FlashData::Plain(NamedIdData {
                id: raw.id,
                short_name: raw.short_name,
                long_name: raw.long_name,
                sdgs: raw.sdgs,
            }),
            Some(other) => {
                return Err(serde::de::Error::custom(format!(
                    "FLASHDATA: unsupported xsi:type {other}"
                )))
            }
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FlashDatas {
    #[serde(rename = "FLASHDATA", default)]
    pub items: Vec<FlashData>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatablockRefs {
    #[serde(rename = "DATABLOCK-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExpectedIdents {
    #[serde(rename = "EXPECTED-IDENT", default)]
    pub items: Vec<Ident>,
}

named_id_struct! {
    pub struct Ident {
        #[serde(rename = "IDENT-VALUE", default)]
        pub ident_value: TypeValueElement,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Security {
    #[serde(rename = "SECURITY-METHOD", default)]
    pub security_method: TypeValueElement,
    #[serde(rename = "FW-SIGNATURE", default)]
    pub fw_signature: TypeValueElement,
    #[serde(rename = "FW-CHECKSUM", default)]
    pub fw_checksum: TypeValueElement,
    #[serde(rename = "VALIDITY-FOR", default)]
    pub validity_for: TypeValueElement,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Securitys {
    #[serde(rename = "SECURITY", default)]
    pub items: Vec<Security>,
}

named_desc_id_struct! {
    pub struct Session {
        #[serde(
            rename = "DATABLOCK-REFS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub datablock_refs: Option<DatablockRefs>,
        #[serde(
            rename = "EXPECTED-IDENTS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub expected_idents: Option<ExpectedIdents>,
        #[serde(rename = "SECURITYS", default, skip_serializing_if = "Option::is_none")]
        pub securitys: Option<Securitys>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sessions {
    #[serde(rename = "SESSION", default)]
    pub items: Vec<Session>,
}

named_id_struct! {
    pub struct Segment {
        #[serde(
            rename = "SOURCE-START-ADDRESS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub source_start_address: Option<String>,
        #[serde(
            rename = "SOURCE-END-ADDRESS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub source_end_address: Option<String>,
        #[serde(rename = "UNCOMPRESSED-SIZE", default, skip_serializing_if = "is_zero")]
        pub uncompressed_size: u32,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Segments {
    #[serde(rename = "SEGMENT", default)]
    pub items: Vec<Segment>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    #[serde(rename = "FILTER-START", default, skip_serializing_if = "Option::is_none")]
    pub filter_start: Option<String>,
    #[serde(rename = "FILTER-END", default, skip_serializing_if = "Option::is_none")]
    pub filter_end: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filters {
    #[serde(rename = "FILTER", default)]
    pub items: Vec<Filter>,
}

named_desc_id_struct! {
    pub struct DataBlock {
        attrs {
            #[serde(rename = "@TYPE", default, skip_serializing_if = "Option::is_none")]
            pub type_: Option<String>,
        }
        #[serde(
            rename = "FLASHDATA-REF",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub flashdata_ref: Option<IdRef>,
        #[serde(rename = "SEGMENTS", default, skip_serializing_if = "Option::is_none")]
        pub segments: Option<Segments>,
        #[serde(rename = "OWN-IDENTS", default, skip_serializing_if = "Option::is_none")]
        pub own_idents: Option<OwnIdents>,
        #[serde(rename = "SECURITYS", default, skip_serializing_if = "Option::is_none")]
        pub securitys: Option<Securitys>,
        #[serde(rename = "FILTERS", default, skip_serializing_if = "Option::is_none")]
        pub filters: Option<Filters>,
    }
}

impl DataBlock {
    pub fn block_type(&self) -> DataBlockType {
        self.type_
            .as_deref()
            .map(DataBlockType::from_odx_name)
            .unwrap_or_default()
    }

    pub fn set_block_type(&mut self, value: DataBlockType) {
        self.type_ = Some(value.as_str().to_owned());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Datablocks {
    #[serde(rename = "DATABLOCK", default)]
    pub items: Vec<DataBlock>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OwnIdents {
    #[serde(rename = "OWN-IDENT", default)]
    pub items: Vec<Ident>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Mem {
    #[serde(rename = "SESSIONS", default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Sessions>,
    #[serde(rename = "DATABLOCKS", default, skip_serializing_if = "Option::is_none")]
    pub datablocks: Option<Datablocks>,
    #[serde(rename = "FLASHDATAS", default, skip_serializing_if = "Option::is_none")]
    pub flashdatas: Option<FlashDatas>,
}

// Physical flash segments use xsi:type to select their concrete shape.

named_id_struct! {
    pub struct PhysSegmentAddr {
        #[serde(rename = "FILLBYTE", default, skip_serializing_if = "Option::is_none")]
        pub fillbyte: Option<String>,
        #[serde(rename = "BLOCK-SIZE", default, skip_serializing_if = "Option::is_none")]
        pub block_size: Option<String>,
        #[serde(
            rename = "START-ADDRESS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub start_address: Option<String>,
        #[serde(rename = "END-ADDRESS", default, skip_serializing_if = "Option::is_none")]
        pub end_address: Option<String>,
    }
}

named_id_struct! {
    pub struct PhysSegmentSize {
        #[serde(rename = "FILLBYTE", default, skip_serializing_if = "Option::is_none")]
        pub fillbyte: Option<String>,
        #[serde(rename = "BLOCK-SIZE", default, skip_serializing_if = "Option::is_none")]
        pub block_size: Option<String>,
        #[serde(
            rename = "START-ADDRESS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub start_address: Option<String>,
        #[serde(rename = "SIZE", default, skip_serializing_if = "Option::is_none")]
        pub size: Option<String>,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PhysSegment {
    Addr(PhysSegmentAddr),
    Size(PhysSegmentSize),
}

#[derive(Default, Serialize, Deserialize)]
struct RawPhysSegment {
    #[serde(
        rename(serialize = "@xsi:type", deserialize = "@type"),
        default,
        skip_serializing_if = "Option::is_none"
    )]
    xsi_type: Option<String>,
    #[serde(rename = "@ID", default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
    short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    long_name: Option<String>,
    #[serde(rename = "SDGS", default, skip_serializing_if = "Option::is_none")]
    sdgs: Option<Sdgs>,
    #[serde(rename = "FILLBYTE", default, skip_serializing_if = "Option::is_none")]
    fillbyte: Option<String>,
    #[serde(rename = "BLOCK-SIZE", default, skip_serializing_if = "Option::is_none")]
    block_size: Option<String>,
    #[serde(
        rename = "START-ADDRESS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    start_address: Option<String>,
    #[serde(rename = "END-ADDRESS", default, skip_serializing_if = "Option::is_none")]
    end_address: Option<String>,
    #[serde(rename = "SIZE", default, skip_serializing_if = "Option::is_none")]
    size: Option<String>,
}

impl Serialize for PhysSegment {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let raw = match self {
            PhysSegment::Addr(value) => RawPhysSegment {
                xsi_type: Some("ADDRDEF-PHYS-SEGMENT".to_owned()),
                id: value.id.clone(),
                short_name: value.short_name.clone(),
                long_name: value.long_name.clone(),
                sdgs: value.sdgs.clone(),
                fillbyte: value.fillbyte.clone(),
                block_size: value.block_size.clone(),
                start_address: value.start_address.clone(),
                end_address: value.end_address.clone(),
                size: None,
            },
            PhysSegment::Size(value) => RawPhysSegment {
                xsi_type: Some("SIZEDEF-PHYS-SEGMENT".to_owned()),
                id: value.id.clone(),
                short_name: value.short_name.clone(),
                long_name: value.long_name.clone(),
                sdgs: value.sdgs.clone(),
                fillbyte: value.fillbyte.clone(),
                block_size: value.block_size.clone(),
                start_address: value.start_address.clone(),
                end_address: None,
                size: value.size.clone(),
            },
        };
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PhysSegment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = RawPhysSegment::deserialize(deserializer)?;
        match raw.xsi_type.as_deref() {
            Some("ADDRDEF-PHYS-SEGMENT") => Ok(PhysSegment::Addr(PhysSegmentAddr {
                id: raw.id,
                short_name: raw.short_name,
                long_name: raw.long_name,
                sdgs: raw.sdgs,
                fillbyte: raw.fillbyte,
                block_size: raw.block_size,
                start_address: raw.start_address,
                end_address: raw.end_address,
            })),
            Some("SIZEDEF-PHYS-SEGMENT") => Ok(PhysSegment::Size(PhysSegmentSize {
                id: raw.id,
                short_name: raw.short_name,
                long_name: raw.long_name,
                sdgs: raw.sdgs,
                fillbyte: raw.fillbyte,
                block_size: raw.block_size,
                start_address: raw.start_address,
                size: raw.size,
            })),
            other => Err(serde::de::Error::custom(format!(
                "PHYS-SEGMENT: unsupported or missing xsi:type {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhysSegments {
    #[serde(rename = "PHYS-SEGMENT", default)]
    pub items: Vec<PhysSegment>,
}

named_id_struct! {
    pub struct PhysMem {
        #[serde(
            rename = "PHYS-SEGMENTS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub phys_segments: Option<PhysSegments>,
    }
}

named_id_struct! {
    pub struct EcuMem {
        #[serde(rename = "MEM", default, skip_serializing_if = "Option::is_none")]
        pub mem: Option<Mem>,
        #[serde(rename = "PHYS-MEM", default, skip_serializing_if = "Option::is_none")]
        pub phys_mem: Option<PhysMem>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EcuMems {
    #[serde(rename = "ECU-MEM", default)]
    pub items: Vec<EcuMem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerRefs {
    #[serde(rename = "LAYER-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FlashClasss {
    #[serde(rename = "FLASH-CLASS", default)]
    pub items: Vec<NamedDescIdData>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashClassRefs {
    #[serde(rename = "FLASH-CLASS-REF", default)]
    pub items: Vec<IdRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentDesc {
    #[serde(
        rename = "DIAG-COMM-SNREF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub diag_comm_snref: Option<SnRef>,
    #[serde(
        rename = "OUT-PARAM-IF-SNREF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub out_param_if_snref: Option<SnRef>,
    #[serde(
        rename = "IDENT-IF-SNREF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub ident_if_snref: Option<SnRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentDescs {
    #[serde(rename = "IDENT-DESC", default)]
    pub items: Vec<IdentDesc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionDesc {
    #[serde(rename = "@DIRECTION", default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(
        rename = "SESSION-SNREF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_snref: Option<SnRef>,
    #[serde(
        rename = "DIAG-COMM-SNREF",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub diag_comm_snref: Option<SnRef>,
    #[serde(
        rename = "FLASH-CLASS-REFS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub flash_class_refs: Option<FlashClassRefs>,
    #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(rename = "LONG-NAME", default, skip_serializing_if = "Option::is_none")]
    pub long_name: Option<String>,
    #[serde(rename = "PARTNUMBER", default, skip_serializing_if = "Option::is_none")]
    pub partnumber: Option<String>,
    #[serde(rename = "PRIORITY", default, skip_serializing_if = "is_zero")]
    pub priority: i32,
}

impl SessionDesc {
    pub fn display_name(&self) -> &str {
        match &self.long_name {
            Some(name) if !name.is_empty() => name,
            _ => self.short_name.as_deref().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionDescs {
    #[serde(rename = "SESSION-DESC", default)]
    pub items: Vec<SessionDesc>,
}

named_id_struct! {
    pub struct EcuMemConnector {
        #[serde(rename = "ECU-MEM-REF", default, skip_serializing_if = "Option::is_none")]
        pub ecu_mem_ref: Option<IdRef>,
        #[serde(rename = "LAYER-REFS", default, skip_serializing_if = "Option::is_none")]
        pub layer_refs: Option<LayerRefs>,
        #[serde(rename = "FLASH-CLASSS", default, skip_serializing_if = "Option::is_none")]
        pub flash_classs: Option<FlashClasss>,
        #[serde(rename = "SESSION-DESCS", default, skip_serializing_if = "Option::is_none")]
        pub session_descs: Option<SessionDescs>,
        #[serde(rename = "IDENT-DESCS", default, skip_serializing_if = "Option::is_none")]
        pub ident_descs: Option<IdentDescs>,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EcuMemConnectors {
    #[serde(rename = "ECU-MEM-CONNECTOR", default)]
    pub items: Vec<EcuMemConnector>,
}

base_doc_info_struct! {
    pub struct Flash {
        #[serde(rename = "ECU-MEMS", default, skip_serializing_if = "Option::is_none")]
        pub ecu_mems: Option<EcuMems>,
        #[serde(
            rename = "ECU-MEM-CONNECTORS",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        pub ecu_mem_connectors: Option<EcuMemConnectors>,
    }
}

// PDX catalog index.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdType {
    #[default]
    #[serde(rename = "NEW")]
    New,
    #[serde(rename = "CHANGED")]
    Changed,
    #[serde(rename = "UNCHANGED")]
    Unchanged,
    #[serde(rename = "UNUSED")]
    Unused,
    #[serde(rename = "REUSED")]
    Reused,
    #[serde(rename = "DELETED")]
    Deleted,
    #[serde(rename = "UNDEFINED")]
    Undefined,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdxFile {
    #[serde(rename = "@MIME-TYPE", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(
        rename = "@CREATION-DATE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub creation_date: Option<String>,
    #[serde(rename = "$text", default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdxFiles {
    #[serde(rename = "FILE", default)]
    pub items: Vec<PdxFile>,
}

fn default_pdx_category() -> String {
    "ODX-DATA".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdxABlock {
    #[serde(rename = "@UPD", default)]
    pub upd: UpdType,
    #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(rename = "CATEGORY", default = "default_pdx_category")]
    pub category: String,
    #[serde(rename = "FILES", default, skip_serializing_if = "Option::is_none")]
    pub files: Option<PdxFiles>,
}

impl Default for PdxABlock {
    fn default() -> Self {
        PdxABlock {
            upd: UpdType::New,
            short_name: None,
            category: default_pdx_category(),
            files: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdxABlocks {
    #[serde(rename = "ABLOCK", default)]
    pub items: Vec<PdxABlock>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "CATALOG")]
pub struct PdxIndex {
    #[serde(rename = "SHORT-NAME", default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(rename = "ABLOCKS", default, skip_serializing_if = "Option::is_none")]
    pub ablocks: Option<PdxABlocks>,
}

impl PdxIndex {
    pub fn get_files(&self, category: &str, prefix_path: &Path) -> Vec<std::path::PathBuf> {
        let mut result = Vec::new();
        let Some(ablocks) = &self.ablocks else {
            return result;
        };
        for block in &ablocks.items {
            if block.category != category {
                continue;
            }
            let Some(files) = &block.files else { continue };
            if files.items.is_empty() {
                continue;
            }
            for file in &files.items {
                let Some(name) = file.text.as_deref() else {
                    continue;
                };
                let path = prefix_path.join(name);
                if prefix_path.as_os_str().is_empty() || path.is_file() {
                    result.push(path);
                }
            }
        }
        result
    }
}

// Resolved views and indexes.

#[derive(Debug, Clone, Copy)]
pub enum DopObject<'a> {
    DataObjectProp(&'a DataObjectProp),
    DtcDop(&'a DtcDop),
    Structure(&'a Structure),
    Mux(&'a Mux),
    EndOfPduField(&'a EndOfPduField),
    DynamicLengthField(&'a DynamicLengthField),
}

fn dtc_dop_as_data_object_prop(value: &DtcDop) -> DataObjectProp {
    DataObjectProp {
        id: value.id.clone(),
        short_name: value.short_name.clone(),
        long_name: value.long_name.clone(),
        sdgs: value.sdgs.clone(),
        desc: value.desc.clone(),
        unit_ref: value.unit_ref.clone(),
        compu_method: value.compu_method.clone(),
        diag_coded_type: value.diag_coded_type.clone(),
        physical_type: value.physical_type.clone(),
        internal_constr: value.internal_constr.clone(),
    }
}

impl DopObject<'_> {
    pub fn as_data_object_prop(self) -> Option<DataObjectProp> {
        match self {
            DopObject::DataObjectProp(value) => Some(value.clone()),
            DopObject::DtcDop(value) => Some(dtc_dop_as_data_object_prop(value)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum IdObject<'a> {
    DiagLayerContainer(&'a DiagLayerContainer),
    Protocol(&'a Protocol),
    EcuSharedData(&'a EcuSharedData),
    BaseVariant(&'a BaseVariant),
    EcuVariant(&'a EcuVariant),
    FunctionalGroup(&'a FunctionalGroup),
    FunctClass(&'a FunctClass),
    DiagService(&'a DiagService),
    ParamContainer(&'a ParamContainer),
    Structure(&'a Structure),
    EnvDataParamContainer(&'a EnvDataParamContainer),
    Param(&'a Param),
    DataObjectProp(&'a DataObjectProp),
    DtcDop(&'a DtcDop),
    EnvDataDesc(&'a EnvDataDesc),
    StaticField(&'a StaticField),
    EndOfPduField(&'a EndOfPduField),
    DynamicLengthField(&'a DynamicLengthField),
    Mux(&'a Mux),
    Table(&'a Table),
    TableRow(&'a TableRow),
    Dtc(&'a Dtc),
    Unit(&'a Unit),
    PhysicalDimension(&'a PhysicalDimension),
    TeamMember(&'a TeamMember),
    CompanyData(&'a CompanyData),
    ComParam(&'a ComParam),
    ComplexComParam(&'a ComplexComParam),
    ProtStack(&'a ProtStack),
    ComParamSpec(&'a ComParamSpec),
    ComParamSubset(&'a ComParamSubset),
    ChartContainer(&'a ChartContainer),
    StateTransitionContainer(&'a StateTransitionContainer),
    NamedDescId(&'a NamedDescIdData),
    NamedId(&'a NamedIdData),
    Session(&'a Session),
    DataBlock(&'a DataBlock),
    EcuMem(&'a EcuMem),
    EcuMemConnector(&'a EcuMemConnector),
    PhysMem(&'a PhysMem),
    Ident(&'a Ident),
    VehicleInfoSpec(&'a VehicleInfoSpec),
    VehicleConnectorPin(&'a VehicleConnectorPin),
    PkysicalVehicleLink(&'a PkysicalVehicleLink),
    LogicalLink(&'a LogicalLinkKind),
    FlashDataIntern(&'a FlashDataIntern),
    FlashDataExtern(&'a FlashDataExtern),
    PhysSegmentAddr(&'a PhysSegmentAddr),
    PhysSegmentSize(&'a PhysSegmentSize),
    InfoComponent(&'a InfoComponent),
    Flash(&'a Flash),
}

impl<'a> IdObject<'a> {
    pub fn id(self) -> Option<&'a str> {
        match self {
            IdObject::DiagLayerContainer(value) => value.id.as_deref(),
            IdObject::Protocol(value) => value.id.as_deref(),
            IdObject::EcuSharedData(value) => value.id.as_deref(),
            IdObject::BaseVariant(value) => value.id.as_deref(),
            IdObject::EcuVariant(value) => value.id.as_deref(),
            IdObject::FunctionalGroup(value) => value.id.as_deref(),
            IdObject::FunctClass(value) => value.id.as_deref(),
            IdObject::DiagService(value) => value.id.as_deref(),
            IdObject::ParamContainer(value) => value.id.as_deref(),
            IdObject::Structure(value) => value.id.as_deref(),
            IdObject::EnvDataParamContainer(value) => value.id.as_deref(),
            IdObject::Param(value) => value.id(),
            IdObject::DataObjectProp(value) => value.id.as_deref(),
            IdObject::DtcDop(value) => value.id.as_deref(),
            IdObject::EnvDataDesc(value) => value.id.as_deref(),
            IdObject::StaticField(value) => value.id.as_deref(),
            IdObject::EndOfPduField(value) => value.id.as_deref(),
            IdObject::DynamicLengthField(value) => value.id.as_deref(),
            IdObject::Mux(value) => value.id.as_deref(),
            IdObject::Table(value) => value.id.as_deref(),
            IdObject::TableRow(value) => value.id.as_deref(),
            IdObject::Dtc(value) => value.id.as_deref(),
            IdObject::Unit(value) => value.id.as_deref(),
            IdObject::PhysicalDimension(value) => value.id.as_deref(),
            IdObject::TeamMember(value) => value.id.as_deref(),
            IdObject::CompanyData(value) => value.id.as_deref(),
            IdObject::ComParam(value) => value.id.as_deref(),
            IdObject::ComplexComParam(value) => value.id.as_deref(),
            IdObject::ProtStack(value) => value.id.as_deref(),
            IdObject::ComParamSpec(value) => value.id.as_deref(),
            IdObject::ComParamSubset(value) => value.id.as_deref(),
            IdObject::ChartContainer(value) => value.id.as_deref(),
            IdObject::StateTransitionContainer(value) => value.id.as_deref(),
            IdObject::NamedDescId(value) => value.id.as_deref(),
            IdObject::NamedId(value) => value.id.as_deref(),
            IdObject::Session(value) => value.id.as_deref(),
            IdObject::DataBlock(value) => value.id.as_deref(),
            IdObject::EcuMem(value) => value.id.as_deref(),
            IdObject::EcuMemConnector(value) => value.id.as_deref(),
            IdObject::PhysMem(value) => value.id.as_deref(),
            IdObject::Ident(value) => value.id.as_deref(),
            IdObject::VehicleInfoSpec(value) => value.id.as_deref(),
            IdObject::VehicleConnectorPin(value) => value.id.as_deref(),
            IdObject::PkysicalVehicleLink(value) => value.id.as_deref(),
            IdObject::LogicalLink(value) => value.base().id.as_deref(),
            IdObject::FlashDataIntern(value) => value.id.as_deref(),
            IdObject::FlashDataExtern(value) => value.id.as_deref(),
            IdObject::PhysSegmentAddr(value) => value.id.as_deref(),
            IdObject::PhysSegmentSize(value) => value.id.as_deref(),
            IdObject::InfoComponent(value) => value.data().id.as_deref(),
            IdObject::Flash(value) => value.id.as_deref(),
        }
    }

    pub fn as_param_container(self) -> Option<&'a ParamContainer> {
        match self {
            IdObject::ParamContainer(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_structure(self) -> Option<&'a Structure> {
        match self {
            IdObject::Structure(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_data_object_prop(self) -> Option<DataObjectProp> {
        match self {
            IdObject::DataObjectProp(value) => Some(value.clone()),
            IdObject::DtcDop(value) => Some(dtc_dop_as_data_object_prop(value)),
            _ => None,
        }
    }

    pub fn as_funct_class(self) -> Option<&'a FunctClass> {
        match self {
            IdObject::FunctClass(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_diag_service(self) -> Option<&'a DiagService> {
        match self {
            IdObject::DiagService(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_protocol(self) -> Option<&'a Protocol> {
        match self {
            IdObject::Protocol(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_base_variant(self) -> Option<&'a BaseVariant> {
        match self {
            IdObject::BaseVariant(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_ecu_variant(self) -> Option<&'a EcuVariant> {
        match self {
            IdObject::EcuVariant(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_functional_group(self) -> Option<&'a FunctionalGroup> {
        match self {
            IdObject::FunctionalGroup(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_table(self) -> Option<&'a Table> {
        match self {
            IdObject::Table(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_table_row(self) -> Option<&'a TableRow> {
        match self {
            IdObject::TableRow(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_unit(self) -> Option<&'a Unit> {
        match self {
            IdObject::Unit(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_dtc(self) -> Option<&'a Dtc> {
        match self {
            IdObject::Dtc(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_com_param_subset(self) -> Option<&'a ComParamSubset> {
        match self {
            IdObject::ComParamSubset(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_com_param_spec(self) -> Option<&'a ComParamSpec> {
        match self {
            IdObject::ComParamSpec(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct OdxIndex<'a> {
    pub ids: IndexMap<String, IdObject<'a>>,
    pub protocols_by_sn: IndexMap<String, &'a Protocol>,
    pub sessions_by_sn: IndexMap<String, &'a Session>,
    pub base_variants_by_sn: IndexMap<String, &'a BaseVariant>,
    pub services_by_sn: IndexMap<String, &'a DiagService>,
    pub params_by_sn: IndexMap<String, &'a Param>,
    pub dops_by_sn: IndexMap<String, DopObject<'a>>,
    pub prot_stacks_by_sn: IndexMap<String, &'a ProtStack>,
}

impl<'a> OdxIndex<'a> {
    fn insert(&mut self, object: IdObject<'a>) {
        if let Some(id) = object.id().filter(|id| !id.is_empty()) {
            self.ids.insert(id.to_owned(), object);
        }
    }

    pub fn resolve_id_ref(&self, reference: &IdRef) -> Option<IdObject<'a>> {
        self.ids.get(reference.id_ref.as_deref()?).copied()
    }

    pub fn resolve_id(&self, reference: &LayerRef) -> Option<IdObject<'a>> {
        self.ids.get(reference.id_ref()?).copied()
    }

    pub fn resolve_esd(&self, reference: &LayerRef) -> Option<&'a dyn EcuSharedDataAccess> {
        match self.resolve_id(reference)? {
            IdObject::Protocol(value) => Some(value),
            IdObject::EcuSharedData(value) => Some(value),
            IdObject::BaseVariant(value) => Some(value),
            IdObject::EcuVariant(value) => Some(value),
            IdObject::FunctionalGroup(value) => Some(value),
            _ => None,
        }
    }

    pub fn resolve_param_container(&self, reference: &IdRef) -> Option<&'a ParamContainer> {
        self.resolve_id_ref(reference)?.as_param_container()
    }

    pub fn resolve_dop_object(
        &self,
        dop_ref: &Option<IdRef>,
        dop_snref: &Option<SnRef>,
    ) -> Option<DopObject<'a>> {
        if let Some(reference) = dop_ref {
            return match self.resolve_id_ref(reference)? {
                IdObject::DataObjectProp(value) => Some(DopObject::DataObjectProp(value)),
                IdObject::DtcDop(value) => Some(DopObject::DtcDop(value)),
                IdObject::Structure(value) => Some(DopObject::Structure(value)),
                IdObject::Mux(value) => Some(DopObject::Mux(value)),
                IdObject::EndOfPduField(value) => Some(DopObject::EndOfPduField(value)),
                IdObject::DynamicLengthField(value) => {
                    Some(DopObject::DynamicLengthField(value))
                }
                _ => None,
            };
        }
        self.dops_by_sn
            .get(dop_snref.as_ref()?.short_name.as_deref()?)
            .copied()
    }

    pub fn resolve_dop(
        &self,
        dop_ref: &Option<IdRef>,
        dop_snref: &Option<SnRef>,
    ) -> Option<DataObjectProp> {
        self.resolve_dop_object(dop_ref, dop_snref)?
            .as_data_object_prop()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum VariantKind<'a> {
    Base(&'a BaseVariant),
    Ecu(&'a EcuVariant),
}

impl<'a> VariantKind<'a> {
    pub fn short_name(self) -> &'a str {
        match self {
            VariantKind::Base(value) => value.short_name.as_deref().unwrap_or_default(),
            VariantKind::Ecu(value) => value.short_name.as_deref().unwrap_or_default(),
        }
    }

    pub fn as_access(self) -> &'a dyn EcuSharedDataAccess {
        match self {
            VariantKind::Base(value) => value,
            VariantKind::Ecu(value) => value,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MergedLayer<'a> {
    pub funct_classes: Vec<&'a FunctClass>,
    pub comparam_refs: Vec<&'a ComParamRef>,
    pub diag_services: Vec<&'a DiagService>,
    pub requests: Vec<&'a ParamContainer>,
    pub pos_responses: Vec<&'a ParamContainer>,
    pub neg_responses: Vec<&'a ParamContainer>,
    pub global_neg_responses: Vec<&'a ParamContainer>,
    pub state_charts: Vec<&'a ChartContainer>,
    pub dds: DiagDataDictSpec<'a>,
}

impl<'a> MergedLayer<'a> {
    fn add_access(&mut self, access: &'a dyn EcuSharedDataAccess) {
        self.funct_classes.extend(access.esd_funct_classs());
        self.comparam_refs.extend(access.esd_comparam_refs());
        self.diag_services.extend(access.esd_diag_comms());
        self.requests.extend(access.esd_requests());
        self.pos_responses.extend(access.esd_pos_responses());
        self.neg_responses.extend(access.esd_neg_responses());
        self.global_neg_responses
            .extend(access.esd_global_neg_responses());
        self.state_charts.extend(access.esd_state_charts());
        self.dds.add_spec(access.esd_dds());
    }
}

#[derive(Debug, Clone)]
pub struct ServiceAnalysis<'a> {
    pub service: &'a DiagService,
    pub request_ref: Option<&'a ParamContainer>,
    pub funct_class_refs: Vec<&'a FunctClass>,
    pub pos_response_refs: Vec<&'a ParamContainer>,
    pub neg_response_refs: Vec<&'a ParamContainer>,
    pub sid: u8,
    pub subfunction: u8,
    pub request_param_info: Vec<ParamDataInfo>,
    pub response_param_info: Vec<ParamDataInfo>,
    pub identifiers: Option<Vec<IdentifierRecord>>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolAnalysis {
    pub baudrate: CanBaudrate,
    pub func_req_can_id: u32,
    pub phys_req_can_id: u32,
    pub phys_resp_can_id: u32,
    pub can_filler_byte: u8,
    pub can_filler_byte_handling: bool,
    pub p2_client: u32,
    pub p3_client: u32,
}

impl Default for ProtocolAnalysis {
    fn default() -> Self {
        ProtocolAnalysis {
            baudrate: CanBaudrate::NOT_SET,
            func_req_can_id: u32::MAX,
            phys_req_can_id: u32::MAX,
            phys_resp_can_id: u32::MAX,
            can_filler_byte: u8::MAX,
            can_filler_byte_handling: false,
            p2_client: 50,
            p3_client: 50,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename = "ODX")]
pub struct OdxRoot {
    #[serde(
        rename = "@xmlns:xsi",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub xmlns_xsi: Option<String>,
    #[serde(
        rename = "@MODEL-VERSION",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_version: Option<String>,
    #[serde(
        rename = "DIAG-LAYER-CONTAINER",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub diag_layer_container: Option<DiagLayerContainer>,
    #[serde(
        rename = "VEHICLE-INFO-SPEC",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub vehicle_info_spec: Option<VehicleInfoSpec>,
    #[serde(rename = "FLASH", default, skip_serializing_if = "Option::is_none")]
    pub flash: Option<Flash>,
    #[serde(
        rename = "COMPARAM-SPEC",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub comparam_spec: Option<ComParamSpec>,
    #[serde(
        rename = "COMPARAM-SUBSET",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub comparam_subset: Option<ComParamSubset>,
}

impl fmt::Display for OdxRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ODX")
    }
}

fn index_params<'a>(index: &mut OdxIndex<'a>, params: &'a Params) {
    for param in &params.items {
        index.insert(IdObject::Param(param));
        if let Some(short_name) = param.short_name().filter(|name| !name.is_empty()) {
            index.params_by_sn.insert(short_name.to_owned(), param);
        }
    }
}

fn index_param_container<'a>(index: &mut OdxIndex<'a>, container: &'a ParamContainer) {
    index.insert(IdObject::ParamContainer(container));
    index_params(index, &container.params);
}

fn index_structure<'a>(index: &mut OdxIndex<'a>, value: &'a Structure) {
    index.insert(IdObject::Structure(value));
    if let Some(short_name) = value.short_name.as_deref().filter(|name| !name.is_empty()) {
        index
            .dops_by_sn
            .insert(short_name.to_owned(), DopObject::Structure(value));
    }
    index_params(index, &value.params);
}

fn index_dds<'a>(index: &mut OdxIndex<'a>, dds: &'a DiagDataDictionarySpec) {
    for value in &dds.dtc_dops.items {
        index.insert(IdObject::DtcDop(value));
        if let Some(short_name) = value.short_name.as_deref().filter(|name| !name.is_empty()) {
            index
                .dops_by_sn
                .insert(short_name.to_owned(), DopObject::DtcDop(value));
        }
        for item in &value.dtcs.items {
            if let DtcItem::Dtc(dtc) = item {
                index.insert(IdObject::Dtc(dtc));
            }
        }
    }
    for value in &dds.env_data_descs.items {
        index.insert(IdObject::EnvDataDesc(value));
        for env in &value.env_datas.items {
            index.insert(IdObject::EnvDataParamContainer(env));
            index_params(index, &env.params);
        }
    }
    for value in &dds.env_datas.items {
        index.insert(IdObject::EnvDataParamContainer(value));
        index_params(index, &value.params);
    }
    for value in &dds.data_object_props.items {
        index.insert(IdObject::DataObjectProp(value));
        if let Some(short_name) = value.short_name.as_deref().filter(|name| !name.is_empty()) {
            index
                .dops_by_sn
                .insert(short_name.to_owned(), DopObject::DataObjectProp(value));
        }
    }
    for value in &dds.structures.items {
        index_structure(index, value);
    }
    for value in &dds.static_fields.items {
        index.insert(IdObject::StaticField(value));
    }
    for value in &dds.end_of_pdu_fields.items {
        index.insert(IdObject::EndOfPduField(value));
        if let Some(short_name) = value.short_name.as_deref().filter(|name| !name.is_empty()) {
            index
                .dops_by_sn
                .insert(short_name.to_owned(), DopObject::EndOfPduField(value));
        }
    }
    for value in &dds.muxs.items {
        index.insert(IdObject::Mux(value));
        if let Some(short_name) = value.short_name.as_deref().filter(|name| !name.is_empty()) {
            index
                .dops_by_sn
                .insert(short_name.to_owned(), DopObject::Mux(value));
        }
    }
    for value in &dds.dynamic_length_fields.items {
        index.insert(IdObject::DynamicLengthField(value));
        if let Some(short_name) = value.short_name.as_deref().filter(|name| !name.is_empty()) {
            index.dops_by_sn.insert(
                short_name.to_owned(),
                DopObject::DynamicLengthField(value),
            );
        }
    }
    for table in &dds.tables.items {
        index.insert(IdObject::Table(table));
        for row in &table.rows {
            index.insert(IdObject::TableRow(row));
        }
    }
    if let Some(unit_spec) = &dds.unit_spec {
        index_unit_spec(index, unit_spec);
    }
}

fn index_unit_spec<'a>(index: &mut OdxIndex<'a>, unit_spec: &'a UnitSpec) {
    if let Some(units) = &unit_spec.units {
        for value in &units.items {
            index.insert(IdObject::Unit(value));
        }
    }
    if let Some(dimensions) = &unit_spec.physical_dimensions {
        for value in &dimensions.items {
            index.insert(IdObject::PhysicalDimension(value));
        }
    }
}

fn index_layer_contents<'a>(index: &mut OdxIndex<'a>, layer: &'a dyn EcuSharedDataAccess) {
    for value in layer.esd_funct_classs() {
        index.insert(IdObject::FunctClass(value));
    }
    for service in layer.esd_diag_comms() {
        index.insert(IdObject::DiagService(service));
        if let Some(short_name) = service.short_name.as_deref().filter(|name| !name.is_empty()) {
            index.services_by_sn.insert(short_name.to_owned(), service);
        }
    }
    for container in layer.esd_requests() {
        index_param_container(index, container);
    }
    for container in layer.esd_pos_responses() {
        index_param_container(index, container);
    }
    for container in layer.esd_neg_responses() {
        index_param_container(index, container);
    }
    for container in layer.esd_global_neg_responses() {
        index_param_container(index, container);
    }
    for chart in layer.esd_state_charts() {
        index.insert(IdObject::ChartContainer(chart));
        for transition in &chart.state_transitions.items {
            index.insert(IdObject::StateTransitionContainer(transition));
        }
        for state in &chart.states.items {
            index.insert(IdObject::NamedDescId(state));
        }
    }
    if let Some(dds) = layer.esd_dds() {
        index_dds(index, dds);
    }
}

fn index_company_data<'a>(index: &mut OdxIndex<'a>, companies: &'a CompanyDatas) {
    for company in &companies.items {
        index.insert(IdObject::CompanyData(company));
        for member in &company.team_members.items {
            index.insert(IdObject::TeamMember(member));
        }
    }
}

fn index_comparam_spec<'a>(index: &mut OdxIndex<'a>, spec: &'a ComParamSpec) {
    index.insert(IdObject::ComParamSpec(spec));
    index_company_data(index, &spec.company_datas);
    if let Some(stacks) = &spec.prot_stacks {
        for stack in &stacks.items {
            index.insert(IdObject::ProtStack(stack));
            if let Some(short_name) = stack.short_name.as_deref().filter(|name| !name.is_empty()) {
                index.prot_stacks_by_sn.insert(short_name.to_owned(), stack);
            }
        }
    }
    if let Some(params) = &spec.comparams {
        for value in &params.items {
            index.insert(IdObject::ComParam(value));
        }
    }
    if let Some(dops) = &spec.data_object_props {
        for value in &dops.items {
            index.insert(IdObject::DataObjectProp(value));
            if let Some(short_name) = value.short_name.as_deref().filter(|name| !name.is_empty()) {
                index
                    .dops_by_sn
                    .insert(short_name.to_owned(), DopObject::DataObjectProp(value));
            }
        }
    }
    if let Some(unit_spec) = &spec.unit_spec {
        index_unit_spec(index, unit_spec);
    }
}

fn index_comparam_subset<'a>(index: &mut OdxIndex<'a>, subset: &'a ComParamSubset) {
    index.insert(IdObject::ComParamSubset(subset));
    index_company_data(index, &subset.company_datas);
    if let Some(params) = &subset.comparams {
        for value in &params.items {
            index.insert(IdObject::ComParam(value));
        }
    }
    if let Some(params) = &subset.complex_comparam {
        for value in &params.items {
            index.insert(IdObject::ComplexComParam(value));
            for nested in &value.comparams {
                index.insert(IdObject::ComParam(nested));
            }
        }
    }
    if let Some(dops) = &subset.data_object_props {
        for value in &dops.items {
            index.insert(IdObject::DataObjectProp(value));
            if let Some(short_name) = value.short_name.as_deref().filter(|name| !name.is_empty()) {
                index
                    .dops_by_sn
                    .insert(short_name.to_owned(), DopObject::DataObjectProp(value));
            }
        }
    }
    if let Some(unit_spec) = &subset.unit_spec {
        index_unit_spec(index, unit_spec);
    }
}

fn index_mem<'a>(index: &mut OdxIndex<'a>, mem: &'a Mem) {
    if let Some(sessions) = &mem.sessions {
        for session in &sessions.items {
            index.insert(IdObject::Session(session));
            if let Some(short_name) = session.short_name.as_deref().filter(|name| !name.is_empty()) {
                index.sessions_by_sn.insert(short_name.to_owned(), session);
            }
            if let Some(idents) = &session.expected_idents {
                for ident in &idents.items {
                    index.insert(IdObject::Ident(ident));
                }
            }
        }
    }
    if let Some(blocks) = &mem.datablocks {
        for block in &blocks.items {
            index.insert(IdObject::DataBlock(block));
            if let Some(idents) = &block.own_idents {
                for ident in &idents.items {
                    index.insert(IdObject::Ident(ident));
                }
            }
        }
    }
    if let Some(flashdatas) = &mem.flashdatas {
        for data in &flashdatas.items {
            match data {
                FlashData::Plain(value) => index.insert(IdObject::NamedId(value)),
                FlashData::Intern(value) => index.insert(IdObject::FlashDataIntern(value)),
                FlashData::Extern(value) => index.insert(IdObject::FlashDataExtern(value)),
            }
        }
    }
}

fn index_flash<'a>(index: &mut OdxIndex<'a>, flash: &'a Flash) {
    index.insert(IdObject::Flash(flash));
    index_company_data(index, &flash.company_datas);
    if let Some(memories) = &flash.ecu_mems {
        for memory in &memories.items {
            index.insert(IdObject::EcuMem(memory));
            if let Some(mem) = &memory.mem {
                index_mem(index, mem);
            }
            if let Some(physical) = &memory.phys_mem {
                index.insert(IdObject::PhysMem(physical));
                if let Some(segments) = &physical.phys_segments {
                    for segment in &segments.items {
                        match segment {
                            PhysSegment::Addr(value) => {
                                index.insert(IdObject::PhysSegmentAddr(value))
                            }
                            PhysSegment::Size(value) => {
                                index.insert(IdObject::PhysSegmentSize(value))
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(connectors) = &flash.ecu_mem_connectors {
        for connector in &connectors.items {
            index.insert(IdObject::EcuMemConnector(connector));
            if let Some(classes) = &connector.flash_classs {
                for class in &classes.items {
                    index.insert(IdObject::NamedDescId(class));
                }
            }
        }
    }
}

fn index_vehicle_info<'a>(index: &mut OdxIndex<'a>, spec: &'a VehicleInfoSpec) {
    index.insert(IdObject::VehicleInfoSpec(spec));
    index_company_data(index, &spec.company_datas);
    if let Some(components) = &spec.info_components {
        for component in &components.items {
            index.insert(IdObject::InfoComponent(component));
        }
    }
    if let Some(informations) = &spec.vehicle_informations {
        for information in &informations.items {
            if let Some(connectors) = &information.vehicle_connectors {
                for connector in &connectors.items {
                    if let Some(pins) = &connector.vehicle_connector_pins {
                        for pin in &pins.items {
                            index.insert(IdObject::VehicleConnectorPin(pin));
                        }
                    }
                }
            }
            if let Some(links) = &information.physical_vehicle_links {
                for link in &links.items {
                    index.insert(IdObject::PkysicalVehicleLink(link));
                }
            }
            if let Some(links) = &information.logical_links {
                for link in &links.items {
                    index.insert(IdObject::LogicalLink(link));
                }
            }
        }
    }
}

impl OdxRoot {
    pub fn container_id(&self) -> Option<&str> {
        self.diag_layer_container
            .as_ref()
            .and_then(|value| value.id.as_deref())
            .or_else(|| {
                self.vehicle_info_spec
                    .as_ref()
                    .and_then(|value| value.id.as_deref())
            })
            .or_else(|| {
                self.comparam_spec
                    .as_ref()
                    .and_then(|value| value.id.as_deref())
            })
            .or_else(|| {
                self.comparam_subset
                    .as_ref()
                    .and_then(|value| value.id.as_deref())
            })
    }

    pub fn build_index(&self) -> OdxIndex<'_> {
        let mut index = OdxIndex::default();
        if let Some(container) = &self.diag_layer_container {
            index.insert(IdObject::DiagLayerContainer(container));
            index_company_data(&mut index, &container.company_datas);
            for protocol in &container.protocols.items {
                index.insert(IdObject::Protocol(protocol));
                if let Some(short_name) = protocol.short_name.as_deref().filter(|name| !name.is_empty()) {
                    index.protocols_by_sn.insert(short_name.to_owned(), protocol);
                }
                index_layer_contents(&mut index, protocol);
            }
            for value in &container.ecu_shared_datas.items {
                index.insert(IdObject::EcuSharedData(value));
                index_layer_contents(&mut index, value);
            }
            for value in &container.base_variants.items {
                index.insert(IdObject::BaseVariant(value));
                if let Some(short_name) = value.short_name.as_deref().filter(|name| !name.is_empty()) {
                    index.base_variants_by_sn.insert(short_name.to_owned(), value);
                }
                index_layer_contents(&mut index, value);
            }
            for value in &container.ecu_variants.items {
                index.insert(IdObject::EcuVariant(value));
                index_layer_contents(&mut index, value);
            }
            for value in &container.functional_groups.items {
                index.insert(IdObject::FunctionalGroup(value));
                index_layer_contents(&mut index, value);
            }
        }
        if let Some(spec) = &self.vehicle_info_spec {
            index_vehicle_info(&mut index, spec);
        }
        if let Some(flash) = &self.flash {
            index_flash(&mut index, flash);
        }
        if let Some(spec) = &self.comparam_spec {
            index_comparam_spec(&mut index, spec);
        }
        if let Some(subset) = &self.comparam_subset {
            index_comparam_subset(&mut index, subset);
        }
        index
    }

    pub fn find_id(&self, id: &str) -> Option<IdObject<'_>> {
        self.build_index().ids.get(id).copied()
    }

    pub fn get_variants(&self) -> Vec<VariantKind<'_>> {
        let Some(container) = &self.diag_layer_container else {
            return Vec::new();
        };
        container
            .base_variants
            .items
            .iter()
            .map(VariantKind::Base)
            .chain(container.ecu_variants.items.iter().map(VariantKind::Ecu))
            .collect()
    }

    pub fn get_variant(&self, variant_name: &str) -> Option<VariantKind<'_>> {
        self.get_variants()
            .into_iter()
            .find(|variant| variant.short_name() == variant_name)
    }

    pub fn dtcs_of<'a>(&'a self, dtc_dop: &'a DtcDop) -> IndexMap<i32, &'a Dtc> {
        let index = self.build_index();
        let mut result = IndexMap::new();
        for item in &dtc_dop.dtcs.items {
            match item {
                DtcItem::Dtc(value) => {
                    result.insert(value.trouble_code, value);
                }
                DtcItem::DtcRef(reference) => {
                    if let Some(value) = index.resolve_id_ref(reference).and_then(IdObject::as_dtc) {
                        result.insert(value.trouble_code, value);
                    }
                }
            }
        }
        result
    }

    pub fn get_dtcs(&self, variant_name: Option<&str>) -> IndexMap<i32, Dtc> {
        let mut result = IndexMap::new();
        for variant in self.get_variants() {
            if variant_name.is_some_and(|name| name != variant.short_name()) {
                continue;
            }
            for dtc_dop in &self.merged_layer(variant).dds.dtc_dops {
                for (code, dtc) in self.dtcs_of(dtc_dop) {
                    result.insert(code, dtc.clone());
                }
            }
        }
        result
    }

    pub fn get_dtc(&self, trouble_code: i32, variant_name: Option<&str>) -> Option<Dtc> {
        self.get_dtcs(variant_name).get(&trouble_code).cloned()
    }

    pub fn parent_of<'a>(&'a self, variant: &'a EcuVariant) -> Option<&'a BaseVariant> {
        let index = self.build_index();
        variant.parent_refs.items.iter().find_map(|reference| {
            if !matches!(reference, LayerRef::BaseVariant(_)) {
                return None;
            }
            index.resolve_id(reference)?.as_base_variant()
        })
    }

    pub fn protocols_of<'a>(&'a self, variant: &'a BaseVariant) -> Vec<&'a Protocol> {
        let index = self.build_index();
        let mut protocols = Vec::new();
        for reference in &variant.parent_refs.items {
            match index.resolve_id(reference) {
                Some(IdObject::Protocol(protocol)) => protocols.push(protocol),
                Some(IdObject::FunctionalGroup(group)) => {
                    if let Some(parent_refs) = &group.parent_refs {
                        for parent_ref in &parent_refs.items {
                            if let Some(IdObject::Protocol(protocol)) = index.resolve_id(parent_ref) {
                                protocols.push(protocol);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        protocols
    }

    pub fn supported_protocols<'a>(&'a self, variant: VariantKind<'a>) -> Vec<&'a Protocol> {
        match variant {
            VariantKind::Base(value) => self.protocols_of(value),
            VariantKind::Ecu(value) => self
                .parent_of(value)
                .map(|parent| self.protocols_of(parent))
                .unwrap_or_default(),
        }
    }

    pub fn physical_protocol(&self) -> Option<&Protocol> {
        let container = self.diag_layer_container.as_ref()?;
        container
            .protocols
            .items
            .first()
            .or_else(|| {
                container
                    .base_variants
                    .items
                    .iter()
                    .find_map(|variant| self.protocols_of(variant).into_iter().next())
            })
            .or_else(|| {
                container.ecu_variants.items.iter().find_map(|variant| {
                    self.parent_of(variant)
                        .and_then(|parent| self.protocols_of(parent).into_iter().next())
                })
            })
    }

    fn add_imports<'a>(
        &'a self,
        layer: &'a dyn EcuSharedDataAccess,
        merged: &mut MergedLayer<'a>,
    ) {
        let index = self.build_index();
        for reference in layer.esd_import_refs() {
            if let Some(imported) = index.resolve_esd(reference) {
                merged.add_access(imported);
            }
        }
    }

    pub fn merged_layer_base<'a>(
        &'a self,
        variant: &'a BaseVariant,
    ) -> MergedLayer<'a> {
        let index = self.build_index();
        let mut merged = MergedLayer::default();
        merged.add_access(variant);
        self.add_imports(variant, &mut merged);
        for reference in &variant.parent_refs.items {
            match index.resolve_id(reference) {
                Some(IdObject::EcuSharedData(value)) => merged.add_access(value),
                Some(IdObject::FunctionalGroup(value)) => merged.add_access(value),
                _ => {}
            }
        }
        merged
    }

    pub fn merged_layer_ecu<'a>(&'a self, variant: &'a EcuVariant) -> MergedLayer<'a> {
        let mut merged = MergedLayer::default();
        merged.add_access(variant);
        self.add_imports(variant, &mut merged);
        if let Some(parent) = self.parent_of(variant) {
            let parent_layer = self.merged_layer_base(parent);
            merged.funct_classes.extend(parent_layer.funct_classes);
            merged.comparam_refs.extend(parent_layer.comparam_refs);
            merged.diag_services.extend(parent_layer.diag_services);
            merged.requests.extend(parent_layer.requests);
            merged.pos_responses.extend(parent_layer.pos_responses);
            merged.neg_responses.extend(parent_layer.neg_responses);
            merged
                .global_neg_responses
                .extend(parent_layer.global_neg_responses);
            merged.state_charts.extend(parent_layer.state_charts);
            merged.dds.add_view(&parent_layer.dds);

            for reference in &variant.parent_refs.items {
                let LayerRef::BaseVariant(reference) = reference else {
                    continue;
                };
                if reference.id_ref.as_deref() != parent.id.as_deref() {
                    continue;
                }
                for excluded in &reference.not_inherited_diag_comms.items {
                    let Some(short_name) = excluded
                        .diag_comm_snref
                        .as_ref()
                        .and_then(|value| value.short_name.as_deref())
                    else {
                        continue;
                    };
                    merged.diag_services.retain(|service| {
                        service.short_name.as_deref() != Some(short_name)
                    });
                }
            }
        }
        merged
    }

    pub fn merged_layer<'a>(&'a self, variant: VariantKind<'a>) -> MergedLayer<'a> {
        match variant {
            VariantKind::Base(value) => self.merged_layer_base(value),
            VariantKind::Ecu(value) => self.merged_layer_ecu(value),
        }
    }

    pub fn get_neg_res_codes(
        &self,
        variant_name: Option<&str>,
    ) -> IndexMap<u8, String> {
        let index = self.build_index();
        let mut result = IndexMap::new();
        for variant in self.get_variants() {
            if variant_name.is_some_and(|name| name != variant.short_name()) {
                continue;
            }
            let merged = self.merged_layer(variant);
            for container in merged
                .global_neg_responses
                .into_iter()
                .chain(merged.neg_responses)
            {
                for param in &container.params.items {
                    let Param::Value(value) = param else { continue };
                    let Some(dop) = index.resolve_dop(&value.dop_ref, &value.dop_snref) else {
                        continue;
                    };
                    let Some(table) = dop.compu_method.as_ref().and_then(CompuMethod::text_table)
                    else {
                        continue;
                    };
                    for (code, text) in table {
                        result.insert(code as u8, text);
                    }
                }
            }
        }
        result
    }

    pub fn get_services(
        &self,
        service_id: u8,
        variant_name: Option<&str>,
        filter_no_response: bool,
    ) -> Vec<&DiagService> {
        let mut result = Vec::new();
        for variant in self.get_variants() {
            if variant_name.is_some_and(|name| name != variant.short_name()) {
                continue;
            }
            for service in self.merged_layer(variant).diag_services {
                let analysis = self.analyze_service(service);
                if analysis.sid == service_id
                    && (!filter_no_response
                        || analysis.subfunction == u8::MAX
                        || analysis.subfunction & 0x80 == 0)
                {
                    result.push(service);
                }
            }
        }
        result
    }

    pub fn get_service(
        &self,
        service_id: u8,
        variant_name: Option<&str>,
        subfunction: u8,
        identifier: u16,
        filter_no_response: bool,
    ) -> Option<&DiagService> {
        self.get_services(service_id, variant_name, filter_no_response)
            .into_iter()
            .find(|service| {
                let analysis = self.analyze_service(service);
                if analysis.subfunction != subfunction {
                    return false;
                }
                identifier == u16::MAX
                    || analysis.identifiers.as_ref().is_some_and(|values| {
                        values.iter().any(|value| value.identifier == identifier)
                    })
            })
    }

    pub fn get_security_access_requests(
        &self,
        variant_name: Option<&str>,
    ) -> Vec<&DiagService> {
        let mut services = self.get_services(0x27, variant_name, true);
        services.sort_by_key(|service| self.analyze_service(service).subfunction);
        services.into_iter().step_by(2).collect()
    }

    pub fn build_new_id(&self) -> String {
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let index = self.build_index();
        for offset in 0..=u32::MAX as u64 {
            let candidate = format!("ID_{}", base.wrapping_add(offset));
            if !index.ids.contains_key(&candidate) {
                return candidate;
            }
        }
        unreachable!("the ODX ID space is exhausted")
    }

    pub fn add_modification(
        &mut self,
        modification: Modification,
        revision: &str,
        state_text: &str,
        mut team_member: TeamMember,
        mut company: CompanyData,
    ) {
        if company.id.as_deref().is_none_or(str::is_empty) {
            company.id = Some(self.build_new_id());
        }
        if team_member.id.as_deref().is_none_or(str::is_empty) {
            team_member.id = Some(self.build_new_id());
        }
        let Some(container) = self.diag_layer_container.as_mut() else {
            return;
        };
        let company_index = container
            .company_datas
            .items
            .iter()
            .position(|value| value == &company)
            .unwrap_or_else(|| {
                container.company_datas.items.push(company);
                container.company_datas.items.len() - 1
            });
        let stored_company = &mut container.company_datas.items[company_index];
        let team_index = stored_company
            .team_members
            .items
            .iter()
            .position(|value| value == &team_member)
            .unwrap_or_else(|| {
                stored_company.team_members.items.push(team_member);
                stored_company.team_members.items.len() - 1
            });
        let stored_member = &stored_company.team_members.items[team_index];
        let admin = container.admin_data.get_or_insert_with(AdminData::default);
        let revision_data = if let Some(existing) = admin
            .doc_revisions
            .items
            .iter_mut()
            .find(|value| value.revision_label.as_deref() == Some(revision))
        {
            existing
        } else {
            admin.doc_revisions.items.push(DocRevision::new_revision(
                revision,
                stored_member,
                state_text,
            ));
            admin.doc_revisions.items.last_mut().expect("just inserted")
        };
        revision_data
            .modifications
            .get_or_insert_with(Modifications::default)
            .items
            .push(modification);
    }
}

fn param_display_name(param: &Param) -> String {
    let name = param.display_name();
    if name.is_empty() {
        param.short_name().unwrap_or_default().to_owned()
    } else {
        name.to_owned()
    }
}

fn unit_name(index: &OdxIndex<'_>, dop: &DataObjectProp) -> String {
    dop.unit_ref
        .as_ref()
        .and_then(|reference| index.resolve_id_ref(reference))
        .and_then(IdObject::as_unit)
        .map(ToString::to_string)
        .unwrap_or_default()
}

fn collect_param_info(
    index: &OdxIndex<'_>,
    container: &ParamContainer,
    output: &mut Vec<ParamDataInfo>,
    offset: &mut i64,
    prefix: Option<&str>,
) {
    for param in &container.params.items {
        match param {
            Param::Value(value) => {
                let local_name = param_display_name(param);
                let name = prefix
                    .filter(|prefix| !prefix.is_empty())
                    .map(|prefix| format!("{prefix}.{local_name}"))
                    .unwrap_or(local_name);
                match index.resolve_dop_object(&value.dop_ref, &value.dop_snref) {
                    Some(DopObject::Structure(structure)) => {
                        *offset += value.byte_position;
                        let pseudo = ParamContainer {
                            id: structure.id.clone(),
                            short_name: structure.short_name.clone(),
                            long_name: structure.long_name.clone(),
                            sdgs: structure.sdgs.clone(),
                            desc: structure.desc.clone(),
                            params: structure.params.clone(),
                        };
                        collect_param_info(index, &pseudo, output, offset, Some(&name));
                    }
                    Some(dop_object) => {
                        if let Some(dop) = dop_object.as_data_object_prop() {
                            let unit = unit_name(index, &dop);
                            output.push(ParamDataInfo::new(
                                name,
                                value.clone(),
                                *offset,
                                dop,
                                unit,
                            ));
                        }
                    }
                    None => {}
                }
            }
            Param::TableStruct(value) => *offset += value.byte_position,
            _ => {}
        }
    }
}

fn coded_bit_length(value: &ParCodedConst) -> Option<i64> {
    match value.diag_coded_type.as_ref()? {
        DiagCodedType::StandardLength(value) => Some(value.bit_length),
        _ => None,
    }
}

fn physical_constant_number(index: &OdxIndex<'_>, value: &ParPhysConst) -> Option<i64> {
    let text = value.phys_constant_value.as_deref()?;
    let dop = index.resolve_dop(&value.dop_ref, &value.dop_snref)?;
    if let Some(table) = dop.compu_method.as_ref().and_then(CompuMethod::text_table) {
        if let Some((number, _)) = table.into_iter().find(|(_, label)| label == text) {
            return Some(number);
        }
    }
    Some(parse_odx_double(text) as i64)
}

impl OdxRoot {
    pub fn analyze_service<'a>(&'a self, service: &'a DiagService) -> ServiceAnalysis<'a> {
        let index = self.build_index();
        let request_ref = service
            .request_ref
            .as_ref()
            .and_then(|reference| index.resolve_param_container(reference));
        let funct_class_refs = service
            .funct_class_refs
            .items
            .iter()
            .filter_map(|reference| index.resolve_id_ref(reference)?.as_funct_class())
            .collect::<Vec<_>>();
        let pos_response_refs = service
            .pos_response_refs
            .items
            .iter()
            .filter_map(|reference| index.resolve_param_container(reference))
            .collect::<Vec<_>>();
        let neg_response_refs = service
            .neg_response_refs
            .items
            .iter()
            .filter_map(|reference| index.resolve_param_container(reference))
            .collect::<Vec<_>>();

        let mut request_param_info = Vec::new();
        if let Some(request) = request_ref {
            collect_param_info(
                &index,
                request,
                &mut request_param_info,
                &mut 0,
                None,
            );
        }
        let mut response_param_info = Vec::new();
        if let Some(response) = pos_response_refs.first() {
            collect_param_info(
                &index,
                response,
                &mut response_param_info,
                &mut 0,
                None,
            );
        }

        let mut sid = u8::MAX;
        let mut subfunction = u8::MAX;
        let mut identifiers: Option<Vec<IdentifierRecord>> = None;
        let mut warnings = Vec::new();
        if let Some(request) = request_ref {
            for param in &request.params.items {
                if param.byte_position() > 2 {
                    continue;
                }
                match param {
                    Param::CodedConst(value) if value.byte_position == 0 => {
                        match u8::try_from(value.coded_value) {
                            Ok(value) => {
                                sid = value;
                                if matches!(sid, 0x11 | 0x22 | 0x24 | 0x2A | 0x2E | 0x2F | 0x31)
                                {
                                    identifiers = Some(Vec::new());
                                }
                            }
                            Err(_) => warnings.push(format!(
                                "service identifier {} does not fit in one byte",
                                value.coded_value
                            )),
                        }
                    }
                    Param::CodedConst(value) => match coded_bit_length(value) {
                        Some(8) => match u8::try_from(value.coded_value) {
                            Ok(value) => subfunction = value,
                            Err(_) => warnings.push(format!(
                                "subfunction {} does not fit in one byte",
                                value.coded_value
                            )),
                        },
                        Some(16) => match u16::try_from(value.coded_value) {
                            Ok(identifier) => identifiers.get_or_insert_with(Vec::new).push(
                                IdentifierRecord {
                                    name: service.display_name().to_owned(),
                                    identifier,
                                    response_param_info: response_param_info.clone(),
                                },
                            ),
                            Err(_) => warnings.push(format!(
                                "identifier {} does not fit in two bytes",
                                value.coded_value
                            )),
                        },
                        _ => {}
                    },
                    Param::PhysConst(value) => {
                        let Some(number) = physical_constant_number(&index, value) else {
                            warnings.push(format!(
                                "cannot decode physical constant {}",
                                value.phys_constant_value.as_deref().unwrap_or_default()
                            ));
                            continue;
                        };
                        let bit_length = index
                            .resolve_dop(&value.dop_ref, &value.dop_snref)
                            .and_then(|dop| dop.diag_coded_type)
                            .and_then(|coded| match coded {
                                DiagCodedType::StandardLength(value) => Some(value.bit_length),
                                _ => None,
                            })
                            .unwrap_or(0);
                        if subfunction == u8::MAX && bit_length <= 8 {
                            if let Ok(value) = u8::try_from(number) {
                                subfunction = value;
                            }
                        } else if bit_length <= 16 {
                            if let Ok(identifier) = u16::try_from(number) {
                                identifiers.get_or_insert_with(Vec::new).push(IdentifierRecord {
                                    name: service.display_name().to_owned(),
                                    identifier,
                                    response_param_info: response_param_info.clone(),
                                });
                            }
                        }
                    }
                    Param::TableKey(value) => {
                        let Some(table) = value
                            .table_ref
                            .as_ref()
                            .and_then(|reference| index.resolve_id_ref(reference))
                            .and_then(IdObject::as_table)
                        else {
                            continue;
                        };
                        for row in &table.rows {
                            let Some(key) = row.key.as_deref().and_then(|key| key.parse::<u16>().ok())
                            else {
                                warnings.push(format!(
                                    "table row key '{}' is not a 16-bit integer",
                                    row.key.as_deref().unwrap_or_default()
                                ));
                                continue;
                            };
                            let Some(structure) = row
                                .structure_ref
                                .as_ref()
                                .and_then(|reference| index.resolve_id_ref(reference))
                                .and_then(IdObject::as_structure)
                            else {
                                continue;
                            };
                            let pseudo = ParamContainer {
                                id: structure.id.clone(),
                                short_name: structure.short_name.clone(),
                                long_name: structure.long_name.clone(),
                                sdgs: structure.sdgs.clone(),
                                desc: structure.desc.clone(),
                                params: structure.params.clone(),
                            };
                            let mut info = Vec::new();
                            collect_param_info(&index, &pseudo, &mut info, &mut 0, None);
                            identifiers.get_or_insert_with(Vec::new).push(IdentifierRecord {
                                name: row.display_name().to_owned(),
                                identifier: key,
                                response_param_info: info,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }

        ServiceAnalysis {
            service,
            request_ref,
            funct_class_refs,
            pos_response_refs,
            neg_response_refs,
            sid,
            subfunction,
            request_param_info,
            response_param_info,
            identifiers,
            warnings,
        }
    }

    pub fn analyze_protocol<'a>(&'a self, protocol: &'a Protocol) -> ProtocolAnalysis {
        let index = self.build_index();
        let mut analysis = ProtocolAnalysis::default();

        for reference in &protocol.comparam_refs.items {
            if reference.protocol_snref.as_ref().is_some_and(|value| {
                value.short_name.as_deref() != protocol.short_name.as_deref()
            }) {
                continue;
            }
            let name = reference
                .id_ref
                .as_deref()
                .and_then(|value| value.rsplit('.').next())
                .unwrap_or_default();
            let value = reference
                .simple_value
                .as_deref()
                .or(reference.value.as_deref())
                .unwrap_or_default();
            apply_protocol_parameter(&mut analysis, name, value);
        }

        if let Some(spec) = protocol
            .comparam_spec_ref
            .as_ref()
            .and_then(|reference| index.resolve_id_ref(reference))
            .and_then(IdObject::as_com_param_spec)
        {
            if let Some(params) = &spec.comparams {
                for param in &params.items {
                    apply_protocol_parameter(
                        &mut analysis,
                        param.short_name.as_deref().unwrap_or_default(),
                        param.physical_default_value.as_deref().unwrap_or_default(),
                    );
                }
            }
        }

        if let Some(stack) = protocol
            .prot_stack_snref
            .as_ref()
            .and_then(|reference| reference.short_name.as_deref())
            .and_then(|short_name| index.prot_stacks_by_sn.get(short_name).copied())
        {
            if let Some(subset_refs) = &stack.comparam_subset_refs {
                for reference in &subset_refs.items {
                    let Some(subset) = index
                        .resolve_id_ref(reference)
                        .and_then(IdObject::as_com_param_subset)
                    else {
                        continue;
                    };
                    if let Some(params) = &subset.comparams {
                        for param in &params.items {
                            apply_protocol_parameter(
                                &mut analysis,
                                param.short_name.as_deref().unwrap_or_default(),
                                param.physical_default_value.as_deref().unwrap_or_default(),
                            );
                        }
                    }
                    if let Some(params) = &subset.complex_comparam {
                        for complex in &params.items {
                            for param in &complex.comparams {
                                apply_protocol_parameter(
                                    &mut analysis,
                                    param.short_name.as_deref().unwrap_or_default(),
                                    param.physical_default_value.as_deref().unwrap_or_default(),
                                );
                            }
                        }
                    }
                }
            }
        }

        normalize_protocol_analysis(&mut analysis);
        analysis
    }
}

fn parse_protocol_u32(value: &str) -> Option<u32> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let hex = has_hex_prefix(value) || value.bytes().any(|byte| byte.is_ascii_alphabetic());
    Some(parse_uint_dec_or_hex(value, hex))
}

fn protocol_parameter_key(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn apply_protocol_parameter(analysis: &mut ProtocolAnalysis, name: &str, value: &str) {
    let key = protocol_parameter_key(name);
    if key.is_empty() || value.trim().is_empty() {
        return;
    }
    if key.contains("fillerbytehandling") || key.contains("paddingactivation") {
        let value = value.trim();
        analysis.can_filler_byte_handling = !matches!(
            value.to_ascii_lowercase().as_str(),
            "false" | "no" | "off" | "0"
        );
    } else if key.contains("fillerbyte") || key.contains("paddingbyte") {
        if let Some(value) = parse_protocol_u32(value).and_then(|value| u8::try_from(value).ok()) {
            analysis.can_filler_byte = value;
        }
    } else if key.contains("funcreq") || key.contains("functionalrequest") {
        if let Some(value) = parse_protocol_u32(value) {
            analysis.func_req_can_id = value;
        }
    } else if key.contains("physreq") || key.contains("physicalrequest") {
        if let Some(value) = parse_protocol_u32(value) {
            analysis.phys_req_can_id = value;
        }
    } else if key.contains("physresp")
        || key.contains("physicalresponse")
        || key.contains("unique response")
        || key.contains("uniqueresp")
    {
        if let Some(value) = parse_protocol_u32(value) {
            analysis.phys_resp_can_id = value;
        }
    } else if key.contains("baud") {
        if let Some(value) = CanBaudrate::parse(value.trim()) {
            analysis.baudrate = value;
        }
    } else if key.contains("p2") {
        if let Some(value) = parse_protocol_u32(value) {
            analysis.p2_client = value.max(50);
        }
    } else if key.contains("p3") {
        if let Some(value) = parse_protocol_u32(value) {
            analysis.p3_client = value;
        }
    }
}

fn normalize_protocol_analysis(analysis: &mut ProtocolAnalysis) {
    for value in [
        &mut analysis.func_req_can_id,
        &mut analysis.phys_req_can_id,
        &mut analysis.phys_resp_can_id,
    ] {
        if *value != u32::MAX && *value > 0x7FF && *value & 0x8000_0000 == 0 {
            *value |= 0x8000_0000;
        }
    }
    if analysis.p2_client > 5_000 {
        analysis.p2_client /= 1_000;
    }
    if analysis.p3_client > 5_000 {
        analysis.p3_client /= 1_000;
    }
}

// File facade.

const ODX_XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>";
const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";

#[derive(Debug, Clone, PartialEq)]
pub struct OdxFile {
    pub odx: OdxRoot,
    pub source_file: Option<String>,
    pub parser_events: Vec<String>,
}

impl OdxFile {
    pub fn parse_str(xml: &str) -> Result<OdxFile> {
        let odx: OdxRoot =
            quick_xml::de::from_str(xml).map_err(|error| Error::Xml(error.to_string()))?;
        if odx.model_version.as_deref().is_none_or(str::is_empty) {
            return Err(Error::Parse("ODX MODEL-VERSION is missing".to_owned()));
        }
        Ok(OdxFile {
            odx,
            source_file: None,
            parser_events: Vec::new(),
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<OdxFile> {
        let path = path.as_ref();
        let xml = std::fs::read_to_string(path)?;
        let mut file = Self::parse_str(&xml)?;
        file.source_file = Some(path.to_string_lossy().into_owned());
        Ok(file)
    }

    pub fn write_string(&self) -> Result<String> {
        let mut root = self.odx.clone();
        if root.xmlns_xsi.as_deref().is_none_or(str::is_empty) {
            root.xmlns_xsi = Some(XSI_NAMESPACE.to_owned());
        }
        let mut body = String::new();
        let mut serializer = quick_xml::se::Serializer::new(&mut body);
        serializer.indent('\t', 1);
        root.serialize(serializer)
            .map_err(|error| Error::Xml(error.to_string()))?;
        Ok(format!("{ODX_XML_DECL}\n{body}"))
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.write_string()?)?;
        Ok(())
    }
}
