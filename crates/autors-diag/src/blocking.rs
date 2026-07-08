//! Synchronous facade over the async diagnostic clients.
//! The wrappers drive the diagnostic async cores to completion on the calling
//! thread via [`autors_runtime::block_on`]:
//! - [`BlockingUdsClient`] wraps [`UdsClient`]: every service method and the
//!   high-level flows (unlock, download, upload, and read/write memory);
//! - [`BlockingDoIpClient`] wraps [`DoIpClient`]: connect, routing activation,
//!   entity status / power mode queries and diagnostic requests;
//! - [`BlockingTransport`] wraps any [`UdsTransport`] for direct message
//!   exchanges (e.g. KWP services) without a client wrapper.
//!
//! The wrapped value is publicly accessible (`.0`), so the public
//! configuration fields (`p2_client`, `negative_response_codes`, `src_adr`,
//! `timeout_ms`, ...) remain directly reachable, and inherently synchronous
//! accessors delegate directly.
//! Do not call these methods from within async code running on the shared
//! runtime: [`autors_runtime::block_on`] panics on its executor threads, the
//! same restriction as `tokio::runtime::Runtime::block_on`.

use std::net::{IpAddr, SocketAddr};

use crate::doip::{
    Activation, DiagnosticPowerModeResponse, EntityStatusResponse, ProtocolVersion, ResponseState,
    RoutingActivationResponse,
};
use crate::doip_client::{DoIpClient, EntityData, VehicleIdentificationFilter};
use crate::error::Result;
use crate::uds::{
    CommunicationControlType, DiagnosticSessionType, DtcSettingType, DtcStatusMask,
    LinkControlType, ModeOfOperationType, MsgState, ReportDtcType, ResetType, RespBase,
    RespControlDtcSetting, RespData, RespDtcCount, RespDtcRecords, RespDynamicallyDefine,
    RespIdentifier, RespLinkControl, RespRequestUpDownload, RespReset, RespResponseOnEvent,
    RespRoutineControl, RespSFBase, RespSecurityAccess, RespSession, RespTimingParameter,
    RespTransferData, RespWriteMemoryByAddress, ResponseOnEventType, RoutineControlType,
    SeedKeyProvider, ServiceResult, Sid, SrcDataIdentifier, SrcDataMemory,
    TimingParameterAccessType, TransmissionModeType, UdsClient, UdsResponse, UdsTransport,
};

// ---------------------------------------------------------------------------
// BlockingTransport — synchronous view of a UdsTransport
// ---------------------------------------------------------------------------

/// Synchronous wrapper around a [`UdsTransport`].
/// Use this for direct request/response exchanges (e.g. KWP
/// `StartCommunication`) without wrapping the transport in a [`UdsClient`].
pub struct BlockingTransport<T>(pub T);

impl<T: UdsTransport + Send> BlockingTransport<T> {
    /// Wraps `transport` in the synchronous facade.
    pub fn new(transport: T) -> Self {
        Self(transport)
    }

    /// Unwraps the facade, returning the inner transport.
    pub fn into_inner(self) -> T {
        self.0
    }

    /// Drives [`UdsTransport::send_request`] to completion on the calling thread.
    pub fn send_request(&mut self, request: &[u8], response: Option<&mut Vec<u8>>) -> MsgState {
        autors_runtime::block_on(self.0.send_request(request, response))
    }

    /// Delegates to [`UdsTransport::max_msg_len`] (inherently synchronous).
    pub fn max_msg_len(&self) -> u64 {
        self.0.max_msg_len()
    }
}

// ---------------------------------------------------------------------------
// BlockingUdsClient — synchronous view of a UdsClient
// ---------------------------------------------------------------------------

/// Synchronous wrapper around a [`UdsClient`].
pub struct BlockingUdsClient<T: UdsTransport>(pub UdsClient<T>);

impl<T: UdsTransport + Send> BlockingUdsClient<T> {
    /// Wraps `client` in the synchronous facade.
    pub fn new(client: UdsClient<T>) -> Self {
        Self(client)
    }

    /// Unwraps the facade, returning the inner client.
    pub fn into_inner(self) -> UdsClient<T> {
        self.0
    }

    /// Delegates to [`UdsClient::overlay_error_codes`] (inherently synchronous).
    pub fn overlay_error_codes(&mut self, codes: impl IntoIterator<Item = (u8, String)>) {
        self.0.overlay_error_codes(codes);
    }

    /// Delegates to [`UdsClient::current_diag_session`] (inherently synchronous).
    pub fn current_diag_session(&self) -> u8 {
        self.0.current_diag_session()
    }

    /// Delegates to [`UdsClient::get_neg_res_code`] (inherently synchronous).
    pub fn get_neg_res_code(&self, error_code: u8) -> String {
        self.0.get_neg_res_code(error_code)
    }

    /// Drives [`UdsClient::execute_service_sf`] to completion on the calling thread.
    pub fn execute_service_sf<R: UdsResponse>(
        &mut self,
        service: Sid,
        sub_function: u8,
        data: Option<&[u8]>,
        await_response: bool,
    ) -> (MsgState, Option<R>) {
        autors_runtime::block_on(self.0.execute_service_sf(
            service,
            sub_function,
            data,
            await_response,
        ))
    }

    /// Drives [`UdsClient::execute_service`] to completion on the calling thread.
    pub fn execute_service<R: UdsResponse>(
        &mut self,
        service: Sid,
        data: Option<&[u8]>,
    ) -> (MsgState, Option<R>) {
        autors_runtime::block_on(self.0.execute_service(service, data))
    }

    /// Drives [`UdsClient::diagnostic_session_control`] to completion on the calling thread.
    pub fn diagnostic_session_control(
        &mut self,
        sf: DiagnosticSessionType,
        await_response: bool,
    ) -> ServiceResult<RespSession> {
        autors_runtime::block_on(self.0.diagnostic_session_control(sf, await_response))
    }

    /// Drives [`UdsClient::ecu_reset`] to completion on the calling thread.
    pub fn ecu_reset(&mut self, sf: ResetType, await_response: bool) -> ServiceResult<RespReset> {
        autors_runtime::block_on(self.0.ecu_reset(sf, await_response))
    }

    /// Drives [`UdsClient::security_access_request_seed`] to completion on the calling thread.
    pub fn security_access_request_seed(
        &mut self,
        security_access_type: u8,
        security_access_data_record: Option<&[u8]>,
    ) -> ServiceResult<RespSecurityAccess> {
        autors_runtime::block_on(
            self.0
                .security_access_request_seed(security_access_type, security_access_data_record),
        )
    }

    /// Drives [`UdsClient::security_access_send_key`] to completion on the calling thread.
    pub fn security_access_send_key(
        &mut self,
        security_access_type: u8,
        security_key: &[u8],
        await_response: bool,
    ) -> ServiceResult<RespSecurityAccess> {
        autors_runtime::block_on(self.0.security_access_send_key(
            security_access_type,
            security_key,
            await_response,
        ))
    }

    /// Drives [`UdsClient::communication_control`] to completion on the calling thread.
    pub fn communication_control(
        &mut self,
        sf: CommunicationControlType,
        communication_type: u8,
        node_id: u16,
        await_response: bool,
    ) -> ServiceResult<RespSFBase> {
        autors_runtime::block_on(self.0.communication_control(
            sf,
            communication_type,
            node_id,
            await_response,
        ))
    }

    /// Drives [`UdsClient::tester_present`] to completion on the calling thread.
    pub fn tester_present(&mut self, await_response: bool) -> ServiceResult<RespSFBase> {
        autors_runtime::block_on(self.0.tester_present(await_response))
    }

    /// Drives [`UdsClient::access_timing_service`] to completion on the calling thread.
    pub fn access_timing_service(
        &mut self,
        sf: TimingParameterAccessType,
        data: Option<&[u8]>,
        await_response: bool,
    ) -> ServiceResult<RespTimingParameter> {
        autors_runtime::block_on(self.0.access_timing_service(sf, data, await_response))
    }

    /// Drives [`UdsClient::secured_data_transmission`] to completion on the calling thread.
    pub fn secured_data_transmission(&mut self, data: &[u8]) -> ServiceResult<RespBase> {
        autors_runtime::block_on(self.0.secured_data_transmission(data))
    }

    /// Drives [`UdsClient::control_dtc_setting`] to completion on the calling thread.
    pub fn control_dtc_setting(
        &mut self,
        sf: DtcSettingType,
        await_response: bool,
    ) -> ServiceResult<RespControlDtcSetting> {
        autors_runtime::block_on(self.0.control_dtc_setting(sf, await_response))
    }

    /// Drives [`UdsClient::response_on_event`] to completion on the calling thread.
    pub fn response_on_event(
        &mut self,
        sf: ResponseOnEventType,
        event_window_time: u8,
        data: Option<&[u8]>,
        await_response: bool,
    ) -> ServiceResult<RespResponseOnEvent> {
        autors_runtime::block_on(self.0.response_on_event(
            sf,
            event_window_time,
            data,
            await_response,
        ))
    }

    /// Drives [`UdsClient::link_control`] to completion on the calling thread.
    pub fn link_control(
        &mut self,
        sf: LinkControlType,
        data: Option<&[u8]>,
        await_response: bool,
    ) -> ServiceResult<RespLinkControl> {
        autors_runtime::block_on(self.0.link_control(sf, data, await_response))
    }

    /// Drives [`UdsClient::read_data_by_identifier`] to completion on the calling thread.
    pub fn read_data_by_identifier(&mut self, identifiers: &[u16]) -> ServiceResult<RespData> {
        autors_runtime::block_on(self.0.read_data_by_identifier(identifiers))
    }

    /// Drives [`UdsClient::read_memory_by_address`] to completion on the calling thread.
    pub fn read_memory_by_address(&mut self, address: i64, size: i64) -> ServiceResult<RespData> {
        autors_runtime::block_on(self.0.read_memory_by_address(address, size))
    }

    /// Drives [`UdsClient::read_scaling_data_by_identifier`] to completion on the calling thread.
    pub fn read_scaling_data_by_identifier(&mut self, identifier: u16) -> ServiceResult<RespData> {
        autors_runtime::block_on(self.0.read_scaling_data_by_identifier(identifier))
    }

    /// Drives [`UdsClient::read_data_by_periodic_identifier`] to completion on the calling thread.
    pub fn read_data_by_periodic_identifier(
        &mut self,
        sf: TransmissionModeType,
        identifier_ids: Option<&[u8]>,
    ) -> ServiceResult<RespData> {
        autors_runtime::block_on(self.0.read_data_by_periodic_identifier(sf, identifier_ids))
    }

    /// Drives [`UdsClient::dynamically_define_data_identifier_by_id`] to completion on the calling thread.
    pub fn dynamically_define_data_identifier_by_id(
        &mut self,
        dyn_id: u16,
        src: &[SrcDataIdentifier],
        await_response: bool,
    ) -> ServiceResult<RespDynamicallyDefine> {
        autors_runtime::block_on(self.0.dynamically_define_data_identifier_by_id(
            dyn_id,
            src,
            await_response,
        ))
    }

    /// Drives [`UdsClient::dynamically_define_data_identifier_by_adr`] to completion on the calling thread.
    pub fn dynamically_define_data_identifier_by_adr(
        &mut self,
        dyn_id: u16,
        src_adr_len: u8,
        src_size_len: u8,
        src: &[SrcDataMemory],
        await_response: bool,
    ) -> ServiceResult<RespDynamicallyDefine> {
        autors_runtime::block_on(self.0.dynamically_define_data_identifier_by_adr(
            dyn_id,
            src_adr_len,
            src_size_len,
            src,
            await_response,
        ))
    }

    /// Drives [`UdsClient::dynamically_clear_data_identifier`] to completion on the calling thread.
    pub fn dynamically_clear_data_identifier(
        &mut self,
        dyn_id: u16,
        await_response: bool,
    ) -> ServiceResult<RespDynamicallyDefine> {
        autors_runtime::block_on(
            self.0
                .dynamically_clear_data_identifier(dyn_id, await_response),
        )
    }

    /// Drives [`UdsClient::write_data_by_identifier`] to completion on the calling thread.
    pub fn write_data_by_identifier(
        &mut self,
        identifier: u16,
        data: &[u8],
    ) -> ServiceResult<RespIdentifier> {
        autors_runtime::block_on(self.0.write_data_by_identifier(identifier, data))
    }

    /// Drives [`UdsClient::write_memory_by_address`] to completion on the calling thread.
    pub fn write_memory_by_address(
        &mut self,
        address: i64,
        size: i64,
        data: &[u8],
    ) -> ServiceResult<RespWriteMemoryByAddress> {
        autors_runtime::block_on(self.0.write_memory_by_address(address, size, data))
    }

    /// Drives [`UdsClient::clear_diagnostic_information`] to completion on the calling thread.
    pub fn clear_diagnostic_information(&mut self, group_of_dtc: u32) -> ServiceResult<RespBase> {
        autors_runtime::block_on(self.0.clear_diagnostic_information(group_of_dtc))
    }

    /// Drives [`UdsClient::read_dtc_information_count`] to completion on the calling thread.
    pub fn read_dtc_information_count(
        &mut self,
        sf: ReportDtcType,
        status_mask: DtcStatusMask,
        severity_mask: u16,
    ) -> ServiceResult<RespDtcCount> {
        autors_runtime::block_on(
            self.0
                .read_dtc_information_count(sf, status_mask, severity_mask),
        )
    }

    /// Drives [`UdsClient::read_dtc_information_records`] to completion on the calling thread.
    pub fn read_dtc_information_records(
        &mut self,
        sf: ReportDtcType,
        status_mask: DtcStatusMask,
    ) -> ServiceResult<RespDtcRecords> {
        autors_runtime::block_on(self.0.read_dtc_information_records(sf, status_mask))
    }

    /// Drives [`UdsClient::read_dtc_information`] to completion on the calling thread.
    pub fn read_dtc_information(
        &mut self,
        sf: ReportDtcType,
        group_of_dtc: u32,
        snapshot_record_number: u8,
        await_response: bool,
    ) -> ServiceResult<RespSFBase> {
        autors_runtime::block_on(self.0.read_dtc_information(
            sf,
            group_of_dtc,
            snapshot_record_number,
            await_response,
        ))
    }

    /// Drives [`UdsClient::input_output_control_by_identifier`] to completion on the calling thread.
    pub fn input_output_control_by_identifier(
        &mut self,
        identifier: u16,
    ) -> ServiceResult<RespIdentifier> {
        autors_runtime::block_on(self.0.input_output_control_by_identifier(identifier))
    }

    /// Drives [`UdsClient::request_download`] to completion on the calling thread.
    pub fn request_download(
        &mut self,
        address: i64,
        size: i64,
        adr_and_len_fmt: u8,
        compression_method: u8,
        encryption_method: u8,
    ) -> ServiceResult<RespRequestUpDownload> {
        autors_runtime::block_on(self.0.request_download(
            address,
            size,
            adr_and_len_fmt,
            compression_method,
            encryption_method,
        ))
    }

    /// Drives [`UdsClient::request_upload`] to completion on the calling thread.
    pub fn request_upload(
        &mut self,
        address: i64,
        size: i64,
        adr_and_len_fmt: u8,
        compression_method: u8,
        encryption_method: u8,
    ) -> ServiceResult<RespRequestUpDownload> {
        autors_runtime::block_on(self.0.request_upload(
            address,
            size,
            adr_and_len_fmt,
            compression_method,
            encryption_method,
        ))
    }

    /// Drives [`UdsClient::transfer_data`] to completion on the calling thread.
    pub fn transfer_data(
        &mut self,
        block_sequence_counter: u8,
        data_to_download: Option<&[u8]>,
    ) -> ServiceResult<RespTransferData> {
        autors_runtime::block_on(
            self.0
                .transfer_data(block_sequence_counter, data_to_download),
        )
    }

    /// Drives [`UdsClient::request_transfer_exit`] to completion on the calling thread.
    pub fn request_transfer_exit(&mut self, data: Option<&[u8]>) -> ServiceResult<RespData> {
        autors_runtime::block_on(self.0.request_transfer_exit(data))
    }

    /// Drives [`UdsClient::request_file_transfer`] to completion on the calling thread.
    pub fn request_file_transfer(
        &mut self,
        sf: ModeOfOperationType,
        file_path_and_name: &str,
    ) -> ServiceResult<RespBase> {
        autors_runtime::block_on(self.0.request_file_transfer(sf, file_path_and_name))
    }

    /// Drives [`UdsClient::routine_control`] to completion on the calling thread.
    pub fn routine_control(
        &mut self,
        sf: RoutineControlType,
        routine_identifier: u16,
        routine_control_option_record: Option<&[u8]>,
        await_response: bool,
    ) -> ServiceResult<RespRoutineControl> {
        autors_runtime::block_on(self.0.routine_control(
            sf,
            routine_identifier,
            routine_control_option_record,
            await_response,
        ))
    }

    /// Drives [`UdsClient::unlock`] to completion on the calling thread.
    pub fn unlock(
        &mut self,
        request_seed_sf: u8,
        sk: &dyn SeedKeyProvider,
        variant: Option<&[u8]>,
    ) -> Result<u8> {
        autors_runtime::block_on(self.0.unlock(request_seed_sf, sk, variant))
    }

    /// Drives [`UdsClient::download`] to completion on the calling thread.
    pub fn download(
        &mut self,
        address: i64,
        data: &[u8],
        adr_and_len_fmt: u8,
        omit_erase_mem: bool,
        progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<u8> {
        autors_runtime::block_on(self.0.download(
            address,
            data,
            adr_and_len_fmt,
            omit_erase_mem,
            progress,
        ))
    }

    /// Drives [`UdsClient::upload`] to completion on the calling thread.
    pub fn upload(
        &mut self,
        address: i64,
        len: i64,
        adr_and_len_fmt: u8,
        progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<(u8, Option<Vec<u8>>)> {
        autors_runtime::block_on(self.0.upload(address, len, adr_and_len_fmt, progress))
    }

    /// Drives [`UdsClient::write_memory`] to completion on the calling thread.
    pub fn write_memory(
        &mut self,
        address: i64,
        data: &[u8],
        max_len: i64,
        progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<u8> {
        autors_runtime::block_on(self.0.write_memory(address, data, max_len, progress))
    }

    /// Drives [`UdsClient::read_memory`] to completion on the calling thread.
    pub fn read_memory(
        &mut self,
        address: i64,
        len: i64,
        max_len: i64,
        progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<(u8, Option<Vec<u8>>)> {
        autors_runtime::block_on(self.0.read_memory(address, len, max_len, progress))
    }
}

// ---------------------------------------------------------------------------
// BlockingDoIpClient — synchronous view of a DoIpClient
// ---------------------------------------------------------------------------

/// Synchronous wrapper around a [`DoIpClient`].
/// The wrapped client is publicly accessible (`.0`), so the public
/// configuration fields (`src_adr`, `dst_adr`, `timeout_ms`, `p2_client_ms`,
/// `last_nack_code`, ...) remain directly reachable.
pub struct BlockingDoIpClient(pub DoIpClient);

impl BlockingDoIpClient {
    /// Sends a UDP vehicle-identification request and collects all unique
    /// entities that answer before the timeout.
    pub fn discover(
        remote_ep: SocketAddr,
        local_ip: IpAddr,
        version: ProtocolVersion,
        filter: VehicleIdentificationFilter,
        timeout_ms: u32,
    ) -> Result<Vec<EntityData>> {
        autors_runtime::block_on(DoIpClient::discover(
            remote_ep, local_ip, version, filter, timeout_ms,
        ))
    }

    /// Wraps `client` in the synchronous facade.
    pub fn new(client: DoIpClient) -> Self {
        Self(client)
    }

    /// Unwraps the facade, returning the inner client.
    pub fn into_inner(self) -> DoIpClient {
        self.0
    }

    /// Drives [`DoIpClient::connect`] to completion on the calling thread.
    pub fn connect(
        remote_ip: IpAddr,
        local_ip: IpAddr,
        activation_type: Activation,
        src_adr: u16,
        dst_adr: u16,
        version: ProtocolVersion,
    ) -> Result<Self> {
        autors_runtime::block_on(DoIpClient::connect(
            remote_ip,
            local_ip,
            activation_type,
            src_adr,
            dst_adr,
            version,
        ))
        .map(Self)
    }

    /// Drives [`DoIpClient::connect_to`] to completion on the calling thread.
    pub fn connect_to(
        remote_ep: SocketAddr,
        local_ip: IpAddr,
        activation_type: Activation,
        src_adr: u16,
        dst_adr: u16,
        version: ProtocolVersion,
    ) -> Result<Self> {
        autors_runtime::block_on(DoIpClient::connect_to(
            remote_ep,
            local_ip,
            activation_type,
            src_adr,
            dst_adr,
            version,
        ))
        .map(Self)
    }

    /// Delegates to [`DoIpClient::is_connected`] (inherently synchronous).
    pub fn is_connected(&self) -> bool {
        self.0.is_connected()
    }

    /// Drives [`DoIpClient::routing_activation`] to completion on the calling thread.
    pub fn routing_activation(
        &mut self,
        activation: Activation,
        reserved_by_iso: u32,
        reserved_by_oem: u32,
        timeout_ms: u32,
    ) -> (ResponseState, Option<RoutingActivationResponse>) {
        autors_runtime::block_on(self.0.routing_activation(
            activation,
            reserved_by_iso,
            reserved_by_oem,
            timeout_ms,
        ))
    }

    /// Drives [`DoIpClient::entity_status`] to completion on the calling thread.
    pub fn entity_status(
        &mut self,
        timeout_ms: u32,
    ) -> (ResponseState, Option<EntityStatusResponse>) {
        autors_runtime::block_on(self.0.entity_status(timeout_ms))
    }

    /// Drives [`DoIpClient::diagnostic_power_mode`] to completion on the calling thread.
    pub fn diagnostic_power_mode(
        &mut self,
        timeout_ms: u32,
    ) -> (ResponseState, Option<DiagnosticPowerModeResponse>) {
        autors_runtime::block_on(self.0.diagnostic_power_mode(timeout_ms))
    }

    /// Drives [`DoIpClient::diagnose_request`] to completion on the calling thread.
    pub fn diagnose_request(
        &mut self,
        p2_client_ms: u32,
        req_data: &[u8],
        res_data: &mut Vec<u8>,
    ) -> MsgState {
        autors_runtime::block_on(self.0.diagnose_request(p2_client_ms, req_data, res_data))
    }
}
