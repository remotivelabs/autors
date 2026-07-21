use std::path::{Path, PathBuf};

use autors_can::device::{CAN_EXT_FLAG, CAN_EXT_ID_MASK, CAN_STD_ID_MASK};
use autors_odx::odx::{OdxFile, ParamDataInfo, ParamValue, VariantKind};

use crate::protocol::CanTransportConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OdxTransport {
    UdsCan,
    DoIp,
}

impl OdxTransport {
    pub const fn title(self) -> &'static str {
        match self {
            Self::UdsCan => "UDS / ISO-TP / CAN",
            Self::DoIp => "UDS / DoIP",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OdxCanParameters {
    pub config: CanTransportConfig,
    pub functional_id: Option<u32>,
    pub baudrate: String,
    pub p2_ms: u32,
}

#[derive(Debug, Clone)]
pub struct OdxVariantView {
    pub name: String,
    pub kind: String,
    pub protocols: Vec<String>,
    pub dtc_count: usize,
    pub service_start: usize,
    pub service_end: usize,
    pub can: Option<OdxCanParameters>,
}

#[derive(Debug, Clone)]
pub struct OdxServiceView {
    pub name: String,
    pub semantic: String,
    pub addressing: String,
    pub sid: u8,
    pub subfunction: Option<u8>,
    pub identifier: Option<u16>,
    pub request_prefix: Vec<u8>,
    pub request_params: Vec<ParamDataInfo>,
    pub response_params: Vec<ParamDataInfo>,
    pub positive_responses: usize,
    pub negative_responses: usize,
    pub warnings: Vec<String>,
}

impl OdxServiceView {
    pub fn selector(&self) -> String {
        let mut value = format!("{:02X}", self.sid);
        if let Some(subfunction) = self.subfunction {
            value.push_str(&format!("/{subfunction:02X}"));
        }
        if let Some(identifier) = self.identifier {
            value.push_str(&format!("/{identifier:04X}"));
        }
        value
    }
}

pub struct OdxLab {
    pub path: Option<PathBuf>,
    pub variants: Vec<OdxVariantView>,
    pub services: Vec<OdxServiceView>,
    pub variant_index: usize,
    pub service_index: usize,
    pub transport: OdxTransport,
    pub last_request: Vec<u8>,
    pub last_response: Vec<u8>,
    pub response_details: Vec<String>,
}

impl Default for OdxLab {
    fn default() -> Self {
        Self::new()
    }
}

impl OdxLab {
    pub fn new() -> Self {
        Self {
            path: None,
            variants: Vec::new(),
            services: Vec::new(),
            variant_index: 0,
            service_index: 0,
            transport: OdxTransport::UdsCan,
            last_request: Vec::new(),
            last_response: Vec::new(),
            response_details: Vec::new(),
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.path.is_some()
    }

    pub fn attach_path(&mut self, path: &Path) -> Result<usize, String> {
        let file = OdxFile::open(path).map_err(|error| error.to_string())?;
        let count = self.attach_file(file)?;
        self.path = Some(path.to_owned());
        Ok(count)
    }

    fn attach_file(&mut self, file: OdxFile) -> Result<usize, String> {
        let mut variants = Vec::new();
        let mut services = Vec::new();
        for variant in file.odx.get_variants() {
            let variant_name = variant.short_name().to_owned();
            let service_start = services.len();
            let layer = file.odx.merged_layer(variant);
            for service in layer.diag_services {
                let analysis = file.odx.analyze_service(service);
                let service_name = if service.display_name().is_empty() {
                    service.short_name.as_deref().unwrap_or("-").to_owned()
                } else {
                    service.display_name().to_owned()
                };
                let identifiers = analysis
                    .identifiers
                    .clone()
                    .filter(|values| !values.is_empty());
                if let Some(identifiers) = identifiers {
                    for identifier in identifiers {
                        services.push(build_service_view(
                            &service_name,
                            service,
                            &analysis,
                            Some(identifier.identifier),
                            identifier.response_param_info,
                        ));
                    }
                } else {
                    services.push(build_service_view(
                        &service_name,
                        service,
                        &analysis,
                        None,
                        analysis.response_param_info.clone(),
                    ));
                }
            }
            let protocols = file.odx.supported_protocols(variant);
            let can = protocols.iter().find_map(|protocol| {
                let analysis = file.odx.analyze_protocol(protocol);
                let command_id = normalize_can_id(analysis.phys_req_can_id)?;
                let response_id = normalize_can_id(analysis.phys_resp_can_id)?;
                Some(OdxCanParameters {
                    config: CanTransportConfig {
                        command_id,
                        response_id,
                        use_can_fd: false,
                    },
                    functional_id: normalize_can_id(analysis.func_req_can_id),
                    baudrate: analysis.baudrate.to_string(),
                    p2_ms: analysis.p2_client,
                })
            });
            variants.push(OdxVariantView {
                name: variant_name.clone(),
                kind: match variant {
                    VariantKind::Base(_) => "Base variant",
                    VariantKind::Ecu(_) => "ECU variant",
                }
                .to_owned(),
                protocols: protocols
                    .iter()
                    .map(|protocol| protocol.short_name.as_deref().unwrap_or("-").to_owned())
                    .collect(),
                dtc_count: file.odx.get_dtcs(Some(&variant_name)).len(),
                service_start,
                service_end: services.len(),
                can,
            });
        }
        if variants.is_empty() {
            return Err("ODX contains no base or ECU diagnostic variant".to_owned());
        }
        self.variants = variants;
        self.services = services;
        self.variant_index = 0;
        self.service_index = 0;
        self.clear_exchange();
        Ok(self.services.len())
    }

    pub fn selected_variant(&self) -> Option<&OdxVariantView> {
        self.variants.get(self.variant_index)
    }

    pub fn visible_services(&self) -> &[OdxServiceView] {
        let Some(variant) = self.selected_variant() else {
            return &[];
        };
        &self.services[variant.service_start..variant.service_end]
    }

    pub fn selected_service(&self) -> Option<&OdxServiceView> {
        self.visible_services().get(self.service_index)
    }

    pub fn move_variant(&mut self, delta: isize) {
        self.variant_index = self
            .variant_index
            .saturating_add_signed(delta)
            .min(self.variants.len().saturating_sub(1));
        self.service_index = 0;
        self.clear_exchange();
    }

    pub fn move_service(&mut self, delta: isize) {
        self.service_index = self
            .service_index
            .saturating_add_signed(delta)
            .min(self.visible_services().len().saturating_sub(1));
        self.clear_exchange();
    }

    pub fn toggle_transport(&mut self) {
        self.transport = match self.transport {
            OdxTransport::UdsCan => OdxTransport::DoIp,
            OdxTransport::DoIp => OdxTransport::UdsCan,
        };
    }

    pub fn selected_can_parameters(&self) -> Option<&OdxCanParameters> {
        self.selected_variant()?.can.as_ref()
    }

    pub fn compose_selected(&mut self, suffix: &str) -> Result<Vec<u8>, String> {
        let service = self
            .selected_service()
            .ok_or_else(|| "the selected ODX variant contains no diagnostic service".to_owned())?;
        if service.sid == u8::MAX || service.request_prefix.is_empty() {
            return Err("ODX service has no resolvable request SID".to_owned());
        }
        let mut request = service.request_prefix.clone();
        request.extend(parse_hex_suffix(suffix)?);
        self.last_request = request.clone();
        self.last_response.clear();
        self.response_details.clear();
        Ok(request)
    }

    pub fn record_response(&mut self, response: Vec<u8>) {
        let response_params = self
            .selected_service()
            .map(|service| service.response_params.clone())
            .unwrap_or_default();
        self.last_response = response;
        self.response_details.clear();
        if self.last_response.is_empty() {
            self.response_details
                .push("Transport returned no response bytes.".to_owned());
            return;
        }
        if self.last_response.first() == Some(&0x7f) {
            self.response_details.push(format!(
                "Negative response · requested SID {:02X} · NRC {:02X}",
                self.last_response.get(1).copied().unwrap_or_default(),
                self.last_response.get(2).copied().unwrap_or_default()
            ));
            return;
        }
        self.response_details.push(format!(
            "Positive response · {} byte(s)",
            self.last_response.len()
        ));
        for mut parameter in response_params {
            parameter.to_physical(&self.last_response);
            if !matches!(parameter.value, ParamValue::None) {
                self.response_details.push(format!(
                    "{} = {}{}",
                    parameter.name,
                    parameter.to_string_value(),
                    if parameter.unit.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", parameter.unit)
                    }
                ));
            }
        }
        if self.response_details.len() == 1 {
            self.response_details
                .push("No ODX response parameter could be decoded from this PDU.".to_owned());
        }
    }

    pub fn clear_exchange(&mut self) {
        self.last_request.clear();
        self.last_response.clear();
        self.response_details.clear();
    }
}

fn build_service_view(
    name: &str,
    service: &autors_odx::odx::DiagService,
    analysis: &autors_odx::odx::ServiceAnalysis<'_>,
    identifier: Option<u16>,
    response_params: Vec<ParamDataInfo>,
) -> OdxServiceView {
    let subfunction = (analysis.subfunction != u8::MAX).then_some(analysis.subfunction);
    let mut request_prefix = Vec::new();
    if analysis.sid != u8::MAX {
        request_prefix.push(analysis.sid);
        if let Some(subfunction) = subfunction {
            request_prefix.push(subfunction);
        }
        if let Some(identifier) = identifier {
            request_prefix.extend_from_slice(&identifier.to_be_bytes());
        }
    }
    OdxServiceView {
        name: name.to_owned(),
        semantic: service.semantic.clone().unwrap_or_default(),
        addressing: format!("{:?}", service.addressing),
        sid: analysis.sid,
        subfunction,
        identifier,
        request_prefix,
        request_params: analysis.request_param_info.clone(),
        response_params,
        positive_responses: analysis.pos_response_refs.len(),
        negative_responses: analysis.neg_response_refs.len(),
        warnings: analysis.warnings.clone(),
    }
}

fn normalize_can_id(id: u32) -> Option<u32> {
    if id <= CAN_STD_ID_MASK {
        Some(id)
    } else if id <= CAN_EXT_ID_MASK {
        Some(id | CAN_EXT_FLAG)
    } else {
        None
    }
}

fn parse_hex_suffix(input: &str) -> Result<Vec<u8>, String> {
    input
        .split(|character: char| character.is_ascii_whitespace() || ",:-_".contains(character))
        .filter(|field| !field.is_empty())
        .map(|field| {
            let value = field
                .strip_prefix("0x")
                .or_else(|| field.strip_prefix("0X"))
                .unwrap_or(field);
            u8::from_str_radix(value, 16)
                .map_err(|_| format!("invalid hexadecimal request byte {field:?}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use autors_odx::odx::{
        BaseVariant, DiagCodedType, DiagCodedTypeStd, DiagLayerContainer, DiagService, IdRef,
        OdxRoot, ParCodedConst, Param, ParamContainer, Params,
    };

    use super::*;

    fn coded_const(position: i64, bits: i64, value: i64) -> Param {
        Param::CodedConst(ParCodedConst {
            byte_position: position,
            coded_value: value,
            diag_coded_type: Some(DiagCodedType::StandardLength(DiagCodedTypeStd {
                bit_length: bits,
                ..DiagCodedTypeStd::default()
            })),
            ..ParCodedConst::default()
        })
    }

    #[test]
    fn expands_odx_identifier_services_and_composes_request_suffix() {
        let request = ParamContainer {
            id: Some("REQ.READ.VIN".to_owned()),
            params: Params {
                items: vec![coded_const(0, 8, 0x22), coded_const(1, 16, 0xf190)],
            },
            ..ParamContainer::default()
        };

        let service = DiagService {
            id: Some("SERVICE.READ.VIN".to_owned()),
            short_name: Some("ReadVIN".to_owned()),
            request_ref: Some(IdRef {
                id_ref: request.id.clone(),
                ..IdRef::default()
            }),
            ..DiagService::default()
        };

        let mut variant = BaseVariant {
            id: Some("VARIANT.DEMO".to_owned()),
            short_name: Some("DemoEcu".to_owned()),
            ..BaseVariant::default()
        };
        variant.requests.items.push(request);
        variant.diag_comms.items.push(service);

        let mut container = DiagLayerContainer::default();
        container.base_variants.items.push(variant);
        let file = OdxFile {
            odx: OdxRoot {
                model_version: Some("2.2.0".to_owned()),
                diag_layer_container: Some(container),
                ..OdxRoot::default()
            },
            source_file: None,
            parser_events: Vec::new(),
        };

        let mut lab = OdxLab::new();
        assert_eq!(lab.attach_file(file).unwrap(), 1);
        assert_eq!(lab.selected_variant().unwrap().name, "DemoEcu");
        assert_eq!(lab.selected_service().unwrap().selector(), "22/F190");
        assert_eq!(
            lab.compose_selected("AA 55").unwrap(),
            [0x22, 0xf1, 0x90, 0xaa, 0x55]
        );
        lab.record_response(vec![0x7f, 0x22, 0x31]);
        assert!(lab.response_details[0].contains("NRC 31"));
    }
}
