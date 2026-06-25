//! Synchronous facade over the async XCP master core.
//! The wrappers drive async XCP operations to completion on the calling thread
//! via [`autors_runtime::block_on`]:
//! - [`BlockingXcpTransport`] wraps any [`XcpTransport`]: byte sending and
//!   receive pumping, while inherently synchronous accessors delegate directly;
//! - [`BlockingXcpMasterBase`] wraps an [`XcpMasterBase`]: every command
//!   method (`connect`, `set_mta`, `upload`, `write_daq`, `program_*`, ...);
//! - [`BlockingXcpMaster`] wraps an [`XcpMaster`]: the connection sequence,
//!   synchronous read/write/program flows, Seed&Key unlock, page operations,
//!   DAQ measurement control and slave-id discovery.
//!
//! The wrapped value is publicly accessible (`.0`), so the public
//! configuration fields and inherently synchronous accessors remain directly
//! reachable.
//! Do not call these methods from within async code running on the shared
//! runtime: [`autors_runtime::block_on`] panics on its executor threads, the
//! same restriction as `tokio::runtime::Runtime::block_on`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::enums::EcuPage;
use autors_can::device::CanDevice;
use autors_comm::base::{
    ConnectBehaviourType, DaqMeasurement, OdtEntry, ProgressCallback, StartStopMode, XcpPrgParams,
};

use crate::error::Result;
use crate::ifdata_xcp::{XcpDaq, XcpHeaderLen, XcpOnCan, XcpOnSxi, XcpProtocolLayer};
use crate::xcp::{
    BasicAddressModeType, CachedResponses, CalPageMode, CmdResult, CommandCode, ConnectMode,
    DaqAndEvt, DaqDictXcp, DaqListMode, DtoCtrMode, DtoCtrModifier, GetIdType, GetSectorInfoMode,
    MappingInfoModeType, ResourceType, RespBuildChecksum, RespConnect, RespDtoCtrResp,
    RespGetCalPage, RespGetCommModeInfo, RespGetDaqClock, RespGetDaqEventInfo, RespGetDaqListInfo,
    RespGetDaqListMode, RespGetDaqListUsbEndpoint, RespGetDaqProcessorInfo,
    RespGetDaqResolutionInfo, RespGetId, RespGetPagProcessorInfo, RespGetPageInfo,
    RespGetPgmProcessorInfo, RespGetSectorInfoModeAddressOrLen, RespGetSectorInfoModeSectorNameLen,
    RespGetSeed, RespGetSegmentInfo, RespGetSegmentInfoAddress, RespGetSegmentMode, RespGetStatus,
    RespProgramStart, RespReadDaq, RespStartStopDaqList, RespTimeCorrelation, RespUnlock,
    SeedModeType, SegmentMode, SerialPortDevice, SetRequestMode, SxiSerialIo, TimeCorrGetPropsReq,
    TimeCorrSetProps, XcpFrame, XcpMaster, XcpMasterBase, XcpMemorySegment, XcpResponse,
    XcpSeedKeyProvider, XcpTransport, XcpType,
};

// ---------------------------------------------------------------------------
// BlockingXcpTransport — synchronous view of an XcpTransport
// ---------------------------------------------------------------------------

/// Synchronous wrapper around an [`XcpTransport`].
/// The wrapped transport is publicly accessible (`.0`), so inherent methods of
/// the concrete backend remain reachable.
pub struct BlockingXcpTransport<T>(pub T);

impl<T> BlockingXcpTransport<T> {
    /// Wraps `transport` in the synchronous facade.
    pub fn new(transport: T) -> Self {
        Self(transport)
    }

    /// Unwraps the facade, returning the inner transport.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T: XcpTransport + Send> BlockingXcpTransport<T> {
    /// Drives [`XcpTransport::send_bytes`] to completion on the calling thread.
    pub fn send_bytes(&mut self, data: &[u8]) -> usize {
        autors_runtime::block_on(self.0.send_bytes(data))
    }

    /// Drives [`XcpTransport::poll`] to completion on the calling thread.
    pub fn poll(&mut self) {
        autors_runtime::block_on(self.0.poll())
    }
}

impl<T: XcpTransport> BlockingXcpTransport<T> {
    /// Delegates to [`XcpTransport::source`] (inherently synchronous).
    pub fn source(&self) -> &str {
        self.0.source()
    }

    /// Delegates to [`XcpTransport::frame_fmt`] (inherently synchronous).
    pub fn frame_fmt(&self) -> XcpHeaderLen {
        self.0.frame_fmt()
    }

    /// Delegates to [`XcpTransport::reset`] (inherently synchronous).
    pub fn reset(&mut self) {
        self.0.reset()
    }

    /// Delegates to [`XcpTransport::next_frame`] (inherently synchronous).
    pub fn next_frame(&mut self) -> Option<XcpFrame> {
        self.0.next_frame()
    }
}

// ---------------------------------------------------------------------------
// BlockingXcpMasterBase — synchronous view of an XcpMasterBase
// ---------------------------------------------------------------------------

/// Synchronous wrapper around an [`XcpMasterBase`].
/// The wrapped master is publicly accessible (`.0`), so the public
/// configuration fields (`base`, `protocol_layer`, ...) remain directly
/// reachable.
pub struct BlockingXcpMasterBase(pub XcpMasterBase);

impl BlockingXcpMasterBase {
    /// Wraps `master` in the synchronous facade.
    pub fn new(master: XcpMasterBase) -> Self {
        Self(master)
    }

    /// Unwraps the facade, returning the inner master.
    pub fn into_inner(self) -> XcpMasterBase {
        self.0
    }
}

impl BlockingXcpMasterBase {
    /// Drives [`XcpMasterBase::exchange`] to completion on the calling thread.
    pub fn exchange<T: XcpResponse>(
        &mut self,
        data: &[u8],
        use_timeout_ms: i32,
        expected_payload: u8,
    ) -> (CmdResult, Option<T>, Vec<u8>) {
        autors_runtime::block_on(self.0.exchange(data, use_timeout_ms, expected_payload))
    }

    /// Drives [`XcpMasterBase::connect`] to completion on the calling thread.
    pub fn connect(&mut self, mode: ConnectMode) -> (CmdResult, Option<RespConnect>) {
        autors_runtime::block_on(self.0.connect(mode))
    }

    /// Drives [`XcpMasterBase::internal_connect`] to completion on the calling thread.
    pub fn internal_connect(&mut self) -> bool {
        autors_runtime::block_on(self.0.internal_connect())
    }

    /// Drives [`XcpMasterBase::internal_get_status`] to completion on the calling thread.
    pub fn internal_get_status(&mut self) -> bool {
        autors_runtime::block_on(self.0.internal_get_status())
    }

    /// Drives [`XcpMasterBase::disconnect`] to completion on the calling thread.
    pub fn disconnect(&mut self, use_timeout_ms: i32) -> bool {
        autors_runtime::block_on(self.0.disconnect(use_timeout_ms))
    }

    /// Drives [`XcpMasterBase::get_status`] to completion on the calling thread.
    pub fn get_status(&mut self) -> (CmdResult, Option<RespGetStatus>) {
        autors_runtime::block_on(self.0.get_status())
    }

    /// Drives [`XcpMasterBase::synch`] to completion on the calling thread.
    pub fn synch(&mut self) -> CmdResult {
        autors_runtime::block_on(self.0.synch())
    }

    /// Drives [`XcpMasterBase::get_comm_mode_info`] to completion on the calling thread.
    pub fn get_comm_mode_info(&mut self) -> (CmdResult, Option<RespGetCommModeInfo>) {
        autors_runtime::block_on(self.0.get_comm_mode_info())
    }

    /// Drives [`XcpMasterBase::get_id`] to completion on the calling thread.
    pub fn get_id(&mut self, id_type: GetIdType) -> (CmdResult, Option<RespGetId>, Vec<u8>) {
        autors_runtime::block_on(self.0.get_id(id_type))
    }

    /// Drives [`XcpMasterBase::set_request`] to completion on the calling thread.
    pub fn set_request(&mut self, mode: SetRequestMode, session_id: u16) -> CmdResult {
        autors_runtime::block_on(self.0.set_request(mode, session_id))
    }

    /// Drives [`XcpMasterBase::get_seed`] to completion on the calling thread.
    pub fn get_seed(
        &mut self,
        seed_mode: SeedModeType,
        resource: ResourceType,
    ) -> (CmdResult, Option<RespGetSeed>, Vec<u8>) {
        autors_runtime::block_on(self.0.get_seed(seed_mode, resource))
    }

    /// Drives [`XcpMasterBase::unlock`] to completion on the calling thread.
    pub fn unlock(&mut self, key: &[u8], offset: &mut usize) -> (CmdResult, Option<RespUnlock>) {
        autors_runtime::block_on(self.0.unlock(key, offset))
    }

    /// Drives [`XcpMasterBase::set_mta`] to completion on the calling thread.
    pub fn set_mta(&mut self, address_extension: u8, address: u32) -> CmdResult {
        autors_runtime::block_on(self.0.set_mta(address_extension, address))
    }

    /// Drives [`XcpMasterBase::upload`] to completion on the calling thread.
    pub fn upload(&mut self, number_of_elements: u8) -> (CmdResult, Vec<u8>) {
        autors_runtime::block_on(self.0.upload(number_of_elements))
    }

    /// Drives [`XcpMasterBase::short_upload`] to completion on the calling thread.
    pub fn short_upload(
        &mut self,
        number_of_elements: u8,
        address_extension: u8,
        address: u32,
    ) -> (CmdResult, Vec<u8>) {
        autors_runtime::block_on(self.0.short_upload(
            number_of_elements,
            address_extension,
            address,
        ))
    }

    /// Drives [`XcpMasterBase::modify_bits`] to completion on the calling thread.
    pub fn modify_bits(&mut self, s: u8, ma: u16, mx: u16) -> CmdResult {
        autors_runtime::block_on(self.0.modify_bits(s, ma, mx))
    }

    /// Drives [`XcpMasterBase::build_checksum`] to completion on the calling thread.
    pub fn build_checksum(&mut self, block_size: u32) -> (CmdResult, Option<RespBuildChecksum>) {
        autors_runtime::block_on(self.0.build_checksum(block_size))
    }

    /// Drives [`XcpMasterBase::download`] to completion on the calling thread.
    pub fn download(&mut self, no_of_data_elements: u8, bytes: &[u8]) -> CmdResult {
        autors_runtime::block_on(self.0.download(no_of_data_elements, bytes))
    }

    /// Drives [`XcpMasterBase::download_next`] to completion on the calling thread.
    pub fn download_next(&mut self, no_of_data_elements: u8, bytes: &[u8]) -> CmdResult {
        autors_runtime::block_on(self.0.download_next(no_of_data_elements, bytes))
    }

    /// Drives [`XcpMasterBase::download_max`] to completion on the calling thread.
    pub fn download_max(&mut self, bytes: &[u8]) -> CmdResult {
        autors_runtime::block_on(self.0.download_max(bytes))
    }

    /// Drives [`XcpMasterBase::short_download`] to completion on the calling thread.
    pub fn short_download(
        &mut self,
        bytes: &[u8],
        address_extension: u8,
        address: u32,
    ) -> CmdResult {
        autors_runtime::block_on(self.0.short_download(bytes, address_extension, address))
    }

    /// Drives [`XcpMasterBase::set_cal_page`] to completion on the calling thread.
    pub fn set_cal_page(&mut self, mode: CalPageMode, segment_no: u8, page_no: u8) -> CmdResult {
        autors_runtime::block_on(self.0.set_cal_page(mode, segment_no, page_no))
    }

    /// Drives [`XcpMasterBase::get_cal_page`] to completion on the calling thread.
    pub fn get_cal_page(
        &mut self,
        mode: CalPageMode,
        segment_no: u8,
    ) -> (CmdResult, Option<RespGetCalPage>) {
        autors_runtime::block_on(self.0.get_cal_page(mode, segment_no))
    }

    /// Drives [`XcpMasterBase::get_pag_processor_info`] to completion on the calling thread.
    pub fn get_pag_processor_info(&mut self) -> (CmdResult, Option<RespGetPagProcessorInfo>) {
        autors_runtime::block_on(self.0.get_pag_processor_info())
    }

    /// Drives [`XcpMasterBase::get_segment_info_mapping`] to completion on the calling thread.
    pub fn get_segment_info_mapping(
        &mut self,
        mapping_mode: MappingInfoModeType,
        segment_no: u8,
        mapping_index: u8,
    ) -> (CmdResult, Option<RespGetSegmentInfoAddress>) {
        autors_runtime::block_on(self.0.get_segment_info_mapping(
            mapping_mode,
            segment_no,
            mapping_index,
        ))
    }

    /// Drives [`XcpMasterBase::get_segment_info_address`] to completion on the calling thread.
    pub fn get_segment_info_address(
        &mut self,
        basic_address_mode: BasicAddressModeType,
        segment_no: u8,
    ) -> (CmdResult, Option<RespGetSegmentInfoAddress>) {
        autors_runtime::block_on(
            self.0
                .get_segment_info_address(basic_address_mode, segment_no),
        )
    }

    /// Drives [`XcpMasterBase::get_segment_info`] to completion on the calling thread.
    pub fn get_segment_info(&mut self, segment_no: u8) -> (CmdResult, Option<RespGetSegmentInfo>) {
        autors_runtime::block_on(self.0.get_segment_info(segment_no))
    }

    /// Drives [`XcpMasterBase::get_page_info`] to completion on the calling thread.
    pub fn get_page_info(
        &mut self,
        segment_no: u8,
        page_no: u8,
    ) -> (CmdResult, Option<RespGetPageInfo>) {
        autors_runtime::block_on(self.0.get_page_info(segment_no, page_no))
    }

    /// Drives [`XcpMasterBase::set_segment_mode`] to completion on the calling thread.
    pub fn set_segment_mode(&mut self, mode: SegmentMode, segment_no: u8) -> CmdResult {
        autors_runtime::block_on(self.0.set_segment_mode(mode, segment_no))
    }

    /// Drives [`XcpMasterBase::get_segment_mode`] to completion on the calling thread.
    pub fn get_segment_mode(&mut self, segment_no: u8) -> (CmdResult, Option<RespGetSegmentMode>) {
        autors_runtime::block_on(self.0.get_segment_mode(segment_no))
    }

    /// Drives [`XcpMasterBase::copy_cal_page`] to completion on the calling thread.
    pub fn copy_cal_page(
        &mut self,
        src_segment_no: u8,
        src_page_no: u8,
        dst_segment_no: u8,
        dst_page_no: u8,
    ) -> CmdResult {
        autors_runtime::block_on(self.0.copy_cal_page(
            src_segment_no,
            src_page_no,
            dst_segment_no,
            dst_page_no,
        ))
    }

    /// Drives [`XcpMasterBase::set_daq_ptr`] to completion on the calling thread.
    pub fn set_daq_ptr(&mut self, daq_list_no: u16, odt_no: u8, odt_entry_no: u8) -> CmdResult {
        autors_runtime::block_on(self.0.set_daq_ptr(daq_list_no, odt_no, odt_entry_no))
    }

    /// Drives [`XcpMasterBase::write_daq`] to completion on the calling thread.
    pub fn write_daq(
        &mut self,
        bit_offset: u8,
        element_size: u8,
        address_extension: u8,
        address: u32,
    ) -> CmdResult {
        autors_runtime::block_on(self.0.write_daq(
            bit_offset,
            element_size,
            address_extension,
            address,
        ))
    }

    /// Drives [`XcpMasterBase::write_daq_multiple`] to completion on the calling thread.
    pub fn write_daq_multiple(&mut self, entries: &[Arc<OdtEntry>]) -> CmdResult {
        autors_runtime::block_on(self.0.write_daq_multiple(entries))
    }

    /// Drives [`XcpMasterBase::set_daq_list_mode`] to completion on the calling thread.
    pub fn set_daq_list_mode(
        &mut self,
        mode: DaqListMode,
        daq_list_no: u16,
        event_channel_no: u16,
        prescaler: u8,
        priority: u8,
    ) -> CmdResult {
        autors_runtime::block_on(self.0.set_daq_list_mode(
            mode,
            daq_list_no,
            event_channel_no,
            prescaler,
            priority,
        ))
    }

    /// Drives [`XcpMasterBase::start_stop_daq_list`] to completion on the calling thread.
    pub fn start_stop_daq_list(
        &mut self,
        mode: StartStopMode,
        daq_list_no: u16,
    ) -> (CmdResult, Option<RespStartStopDaqList>) {
        autors_runtime::block_on(self.0.start_stop_daq_list(mode, daq_list_no))
    }

    /// Drives [`XcpMasterBase::start_stop_synch`] to completion on the calling thread.
    pub fn start_stop_synch(&mut self, mode: StartStopMode) -> CmdResult {
        autors_runtime::block_on(self.0.start_stop_synch(mode))
    }

    /// Drives [`XcpMasterBase::read_daq`] to completion on the calling thread.
    pub fn read_daq(&mut self) -> (CmdResult, Option<RespReadDaq>) {
        autors_runtime::block_on(self.0.read_daq())
    }

    /// Drives [`XcpMasterBase::get_daq_clock`] to completion on the calling thread.
    pub fn get_daq_clock(&mut self) -> (CmdResult, Option<RespGetDaqClock>) {
        autors_runtime::block_on(self.0.get_daq_clock())
    }

    /// Drives [`XcpMasterBase::get_daq_processor_info`] to completion on the calling thread.
    pub fn get_daq_processor_info(&mut self) -> (CmdResult, Option<RespGetDaqProcessorInfo>) {
        autors_runtime::block_on(self.0.get_daq_processor_info())
    }

    /// Drives [`XcpMasterBase::get_daq_resolution_info`] to completion on the calling thread.
    pub fn get_daq_resolution_info(&mut self) -> (CmdResult, Option<RespGetDaqResolutionInfo>) {
        autors_runtime::block_on(self.0.get_daq_resolution_info())
    }

    /// Drives [`XcpMasterBase::get_daq_list_mode`] to completion on the calling thread.
    pub fn get_daq_list_mode(
        &mut self,
        daq_list_no: u16,
    ) -> (CmdResult, Option<RespGetDaqListMode>) {
        autors_runtime::block_on(self.0.get_daq_list_mode(daq_list_no))
    }

    /// Drives [`XcpMasterBase::get_daq_event_info`] to completion on the calling thread.
    pub fn get_daq_event_info(
        &mut self,
        event_channel_no: u16,
    ) -> (CmdResult, Option<RespGetDaqEventInfo>) {
        autors_runtime::block_on(self.0.get_daq_event_info(event_channel_no))
    }

    /// Drives [`XcpMasterBase::clear_daq_list`] to completion on the calling thread.
    pub fn clear_daq_list(&mut self, daq_list_no: u16) -> CmdResult {
        autors_runtime::block_on(self.0.clear_daq_list(daq_list_no))
    }

    /// Drives [`XcpMasterBase::get_daq_list_info`] to completion on the calling thread.
    pub fn get_daq_list_info(
        &mut self,
        daq_list_no: u16,
    ) -> (CmdResult, Option<RespGetDaqListInfo>) {
        autors_runtime::block_on(self.0.get_daq_list_info(daq_list_no))
    }

    /// Drives [`XcpMasterBase::free_daq`] to completion on the calling thread.
    pub fn free_daq(&mut self) -> CmdResult {
        autors_runtime::block_on(self.0.free_daq())
    }

    /// Drives [`XcpMasterBase::alloc_daq`] to completion on the calling thread.
    pub fn alloc_daq(&mut self, daq_count: u16) -> CmdResult {
        autors_runtime::block_on(self.0.alloc_daq(daq_count))
    }

    /// Drives [`XcpMasterBase::alloc_odt`] to completion on the calling thread.
    pub fn alloc_odt(&mut self, daq_list_no: u16, odt_count: u8) -> CmdResult {
        autors_runtime::block_on(self.0.alloc_odt(daq_list_no, odt_count))
    }

    /// Drives [`XcpMasterBase::alloc_odt_entry`] to completion on the calling thread.
    pub fn alloc_odt_entry(
        &mut self,
        daq_list_no: u16,
        odt_no: u8,
        odt_entries_count: u8,
    ) -> CmdResult {
        autors_runtime::block_on(
            self.0
                .alloc_odt_entry(daq_list_no, odt_no, odt_entries_count),
        )
    }

    /// Drives [`XcpMasterBase::program_start`] to completion on the calling thread.
    pub fn program_start(&mut self) -> (CmdResult, Option<RespProgramStart>) {
        autors_runtime::block_on(self.0.program_start())
    }

    /// Drives [`XcpMasterBase::program_clear`] to completion on the calling thread.
    pub fn program_clear(
        &mut self,
        mode: autors_comm::base::ProgramClearMode,
        clear_range: u32,
    ) -> CmdResult {
        autors_runtime::block_on(self.0.program_clear(mode, clear_range))
    }

    /// Drives [`XcpMasterBase::write_bytes`] to completion on the calling thread.
    pub fn write_bytes(
        &mut self,
        cmd_code: CommandCode,
        bytes: &[u8],
        no_of_data_elements: u8,
        use_timeout_ms: i32,
    ) -> CmdResult {
        autors_runtime::block_on(self.0.write_bytes(
            cmd_code,
            bytes,
            no_of_data_elements,
            use_timeout_ms,
        ))
    }

    /// Drives [`XcpMasterBase::program`] to completion on the calling thread.
    pub fn program(&mut self, no_of_data_elements: u8, bytes: &[u8]) -> CmdResult {
        autors_runtime::block_on(self.0.program(no_of_data_elements, bytes))
    }

    /// Drives [`XcpMasterBase::program_next`] to completion on the calling thread.
    pub fn program_next(&mut self, no_of_data_elements: u8, bytes: &[u8]) -> CmdResult {
        autors_runtime::block_on(self.0.program_next(no_of_data_elements, bytes))
    }

    /// Drives [`XcpMasterBase::program_max`] to completion on the calling thread.
    pub fn program_max(&mut self, bytes: &[u8]) -> CmdResult {
        autors_runtime::block_on(self.0.program_max(bytes))
    }

    /// Drives [`XcpMasterBase::program_reset`] to completion on the calling thread.
    pub fn program_reset(&mut self) -> CmdResult {
        autors_runtime::block_on(self.0.program_reset())
    }

    /// Drives [`XcpMasterBase::get_pgm_processor_info`] to completion on the calling thread.
    pub fn get_pgm_processor_info(&mut self) -> (CmdResult, Option<RespGetPgmProcessorInfo>) {
        autors_runtime::block_on(self.0.get_pgm_processor_info())
    }

    /// Drives [`XcpMasterBase::get_sector_info`] to completion on the calling thread.
    pub fn get_sector_info(
        &mut self,
        mode: GetSectorInfoMode,
        sector_no: u8,
    ) -> Result<(CmdResult, Option<RespGetSectorInfoModeAddressOrLen>)> {
        autors_runtime::block_on(self.0.get_sector_info(mode, sector_no))
    }

    /// Drives [`XcpMasterBase::get_sector_info_name_len`] to completion on the calling thread.
    pub fn get_sector_info_name_len(
        &mut self,
        mode: GetSectorInfoMode,
        sector_no: u8,
    ) -> Result<(CmdResult, Option<RespGetSectorInfoModeSectorNameLen>)> {
        autors_runtime::block_on(self.0.get_sector_info_name_len(mode, sector_no))
    }

    /// Drives [`XcpMasterBase::program_prepare`] to completion on the calling thread.
    pub fn program_prepare(&mut self, code_size: u16) -> CmdResult {
        autors_runtime::block_on(self.0.program_prepare(code_size))
    }

    /// Drives [`XcpMasterBase::program_format`] to completion on the calling thread.
    pub fn program_format(
        &mut self,
        compression_method: u8,
        encryption_method: u8,
        programming_method: u8,
        access_method: u8,
    ) -> CmdResult {
        autors_runtime::block_on(self.0.program_format(
            compression_method,
            encryption_method,
            programming_method,
            access_method,
        ))
    }

    /// Drives [`XcpMasterBase::program_verify`] to completion on the calling thread.
    pub fn program_verify(
        &mut self,
        mode: autors_comm::base::ProgramVerifyMode,
        verification_type: u16,
        verification_value: u32,
    ) -> CmdResult {
        autors_runtime::block_on(
            self.0
                .program_verify(mode, verification_type, verification_value),
        )
    }

    /// Drives [`XcpMasterBase::user_cmd`] to completion on the calling thread.
    pub fn user_cmd(&mut self, sub_command: u8, bytes: &[u8]) -> (CmdResult, Vec<u8>) {
        autors_runtime::block_on(self.0.user_cmd(sub_command, bytes))
    }

    /// Drives [`XcpMasterBase::dto_ctr_properties`] to completion on the calling thread.
    pub fn dto_ctr_properties(
        &mut self,
        modifier: DtoCtrModifier,
        event_channel_no: u16,
        related_event_channel_no: u16,
        mode: DtoCtrMode,
    ) -> (CmdResult, Option<RespDtoCtrResp>) {
        autors_runtime::block_on(self.0.dto_ctr_properties(
            modifier,
            event_channel_no,
            related_event_channel_no,
            mode,
        ))
    }

    /// Drives [`XcpMasterBase::time_correlation_properties`] to completion on the calling thread.
    pub fn time_correlation_properties(
        &mut self,
        set_properties: TimeCorrSetProps,
        get_properties_req: TimeCorrGetPropsReq,
        cluster_id: u16,
    ) -> (CmdResult, Option<RespTimeCorrelation>) {
        autors_runtime::block_on(self.0.time_correlation_properties(
            set_properties,
            get_properties_req,
            cluster_id,
        ))
    }

    /// Drives [`XcpMasterBase::transport_layer_cmd`] to completion on the calling thread.
    pub fn transport_layer_cmd(
        &mut self,
        sub_command: u8,
        bytes: &[u8],
        use_timeout_ms: i32,
    ) -> (CmdResult, Vec<u8>) {
        autors_runtime::block_on(
            self.0
                .transport_layer_cmd(sub_command, bytes, use_timeout_ms),
        )
    }

    /// Drives [`XcpMasterBase::get_daq_id`] to completion on the calling thread.
    pub fn get_daq_id(&mut self, daq_list_no: u16) -> (CmdResult, Option<(bool, u32)>) {
        autors_runtime::block_on(self.0.get_daq_id(daq_list_no))
    }

    /// Drives [`XcpMasterBase::set_daq_id`] to completion on the calling thread.
    pub fn set_daq_id(&mut self, daq_list_no: u16, can_id: u32) -> CmdResult {
        autors_runtime::block_on(self.0.set_daq_id(daq_list_no, can_id))
    }

    /// Drives [`XcpMasterBase::get_daq_clock_multicast`] to completion on the calling thread.
    pub fn get_daq_clock_multicast(&mut self, cluster_id: u16, counter: u8) -> CmdResult {
        autors_runtime::block_on(self.0.get_daq_clock_multicast(cluster_id, counter))
    }

    /// Drives [`XcpMasterBase::get_daq_list_usb_endpoint`] to completion on the calling thread.
    pub fn get_daq_list_usb_endpoint(
        &mut self,
        daq_list_no: u16,
    ) -> (CmdResult, Option<RespGetDaqListUsbEndpoint>) {
        autors_runtime::block_on(self.0.get_daq_list_usb_endpoint(daq_list_no))
    }

    /// Drives [`XcpMasterBase::set_daq_list_usb_endpoint`] to completion on the calling thread.
    pub fn set_daq_list_usb_endpoint(&mut self, daq_list_no: u16, endpoint_no: u8) -> CmdResult {
        autors_runtime::block_on(self.0.set_daq_list_usb_endpoint(daq_list_no, endpoint_no))
    }
}

impl BlockingXcpMasterBase {
    /// Delegates to [`XcpMasterBase::timings`] (inherently synchronous).
    pub fn timings(&self) -> [u16; 7] {
        self.0.timings()
    }

    /// Delegates to [`XcpMasterBase::max_cto`] (inherently synchronous).
    pub fn max_cto(&self) -> u8 {
        self.0.max_cto()
    }

    /// Delegates to [`XcpMasterBase::max_dto`] (inherently synchronous).
    pub fn max_dto(&self) -> u16 {
        self.0.max_dto()
    }

    /// Delegates to [`XcpMasterBase::address_granularity`] (inherently synchronous).
    pub fn address_granularity(&self) -> usize {
        self.0.address_granularity()
    }

    /// Delegates to [`XcpMasterBase::last_response`] (inherently synchronous).
    pub fn last_response(&self) -> Option<&XcpFrame> {
        self.0.last_response()
    }

    /// Delegates to [`XcpMasterBase::last_error_response`] (inherently synchronous).
    pub fn last_error_response(&self) -> CmdResult {
        self.0.last_error_response()
    }

    /// Delegates to [`XcpMasterBase::last_error_text`] (inherently synchronous).
    pub fn last_error_text(&self) -> String {
        self.0.last_error_text()
    }

    /// Delegates to [`XcpMasterBase::reset_last_error`] (inherently synchronous).
    pub fn reset_last_error(&mut self) {
        self.0.reset_last_error()
    }

    /// Delegates to [`XcpMasterBase::alive_cycle_time`] (inherently synchronous).
    pub fn alive_cycle_time(&self) -> i32 {
        self.0.alive_cycle_time()
    }

    /// Delegates to [`XcpMasterBase::padding_size`] (inherently synchronous).
    pub fn padding_size(ag: usize) -> usize {
        XcpMasterBase::padding_size(ag)
    }

    /// Delegates to [`XcpMasterBase::increase_ctr`] (inherently synchronous).
    pub fn increase_ctr(&mut self, bit_length: usize) {
        self.0.increase_ctr(bit_length)
    }

    /// Delegates to [`XcpMasterBase::frame_fmt`] (inherently synchronous).
    pub fn frame_fmt(&self) -> XcpHeaderLen {
        self.0.frame_fmt()
    }

    /// Delegates to [`XcpMasterBase::ctr`] (inherently synchronous).
    pub fn ctr(&self) -> u16 {
        self.0.ctr()
    }

    /// Delegates to [`XcpMasterBase::set_connection_state`] (inherently synchronous).
    pub fn set_connection_state(&mut self, resp: Option<&RespConnect>) {
        self.0.set_connection_state(resp)
    }

    /// Delegates to [`XcpMasterBase::on_error_received`] (inherently synchronous).
    pub fn on_error_received(&mut self, result: CmdResult, code: CommandCode) {
        self.0.on_error_received(result, code)
    }
}

// ---------------------------------------------------------------------------
// BlockingXcpMaster — synchronous view of an XcpMaster
// ---------------------------------------------------------------------------

/// Synchronous wrapper around an [`XcpMaster`].
/// The wrapped master is publicly accessible (`.0`), so the public
/// configuration fields (`base`, `callbacks`, `prevent_block_mode`,
/// `respect_optional_cmds`, ...) remain directly reachable.
pub struct BlockingXcpMaster(pub XcpMaster);

impl BlockingXcpMaster {
    /// Wraps `master` in the synchronous facade.
    pub fn new(master: XcpMaster) -> Self {
        Self(master)
    }

    /// Unwraps the facade, returning the inner master.
    pub fn into_inner(self) -> XcpMaster {
        self.0
    }

    /// Delegates to [`XcpMaster::with_base`] (constructor, inherently synchronous).
    pub fn with_base(base: XcpMasterBase, daq: Option<&XcpDaq>) -> Self {
        Self(XcpMaster::with_base(base, daq))
    }

    /// Delegates to [`XcpMaster::new_can`] (constructor, inherently synchronous).
    pub fn new_can(
        connect_behaviour: ConnectBehaviourType,
        device: Arc<Mutex<dyn CanDevice + Send>>,
        xcp_can: &XcpOnCan,
        protocol_layer: XcpProtocolLayer,
        daq: Option<&XcpDaq>,
        device_name: &str,
    ) -> Result<Self> {
        XcpMaster::new_can(
            connect_behaviour,
            device,
            xcp_can,
            protocol_layer,
            daq,
            device_name,
        )
        .map(Self)
    }

    /// Delegates to [`XcpMaster::new_udp_tcp`] (constructor, inherently synchronous).
    pub fn new_udp_tcp(
        connect_behaviour: ConnectBehaviourType,
        type_: XcpType,
        remote_address: &str,
        remote_port: i32,
        protocol_layer: XcpProtocolLayer,
        daq: Option<&XcpDaq>,
    ) -> Result<Self> {
        XcpMaster::new_udp_tcp(
            connect_behaviour,
            type_,
            remote_address,
            remote_port,
            protocol_layer,
            daq,
        )
        .map(Self)
    }

    /// Delegates to [`XcpMaster::new_sxi`] (constructor, inherently synchronous).
    pub fn new_sxi<IO: SxiSerialIo + Send + 'static>(
        connect_behaviour: ConnectBehaviourType,
        device: SerialPortDevice<IO>,
        xcp_sxi: &XcpOnSxi,
        protocol_layer: XcpProtocolLayer,
        daq: Option<&XcpDaq>,
    ) -> Self {
        Self(XcpMaster::new_sxi(
            connect_behaviour,
            device,
            xcp_sxi,
            protocol_layer,
            daq,
        ))
    }
}

impl BlockingXcpMaster {
    /// Drives [`XcpMaster::set_connection_state`] to completion on the calling thread.
    pub fn set_connection_state(&mut self, resp: Option<&RespConnect>) {
        autors_runtime::block_on(self.0.set_connection_state(resp))
    }

    /// Drives [`XcpMaster::connect`] to completion on the calling thread.
    pub fn connect(&mut self, mode: ConnectMode) -> (CmdResult, Option<RespConnect>) {
        autors_runtime::block_on(self.0.connect(mode))
    }

    /// Drives [`XcpMaster::get_status`] to completion on the calling thread.
    pub fn get_status(&mut self) -> (CmdResult, Option<RespGetStatus>) {
        autors_runtime::block_on(self.0.get_status())
    }

    /// Drives [`XcpMaster::get_comm_mode_info`] to completion on the calling thread.
    pub fn get_comm_mode_info(&mut self) -> (CmdResult, Option<RespGetCommModeInfo>) {
        autors_runtime::block_on(self.0.get_comm_mode_info())
    }

    /// Drives [`XcpMaster::get_cal_page`] to completion on the calling thread.
    pub fn get_cal_page(
        &mut self,
        mode: CalPageMode,
        segment_no: u8,
    ) -> (CmdResult, Option<RespGetCalPage>) {
        autors_runtime::block_on(self.0.get_cal_page(mode, segment_no))
    }

    /// Drives [`XcpMaster::get_pag_processor_info`] to completion on the calling thread.
    pub fn get_pag_processor_info(&mut self) -> (CmdResult, Option<RespGetPagProcessorInfo>) {
        autors_runtime::block_on(self.0.get_pag_processor_info())
    }

    /// Drives [`XcpMaster::get_daq_processor_info`] to completion on the calling thread.
    pub fn get_daq_processor_info(&mut self) -> (CmdResult, Option<RespGetDaqProcessorInfo>) {
        autors_runtime::block_on(self.0.get_daq_processor_info())
    }

    /// Drives [`XcpMaster::get_daq_resolution_info`] to completion on the calling thread.
    pub fn get_daq_resolution_info(&mut self) -> (CmdResult, Option<RespGetDaqResolutionInfo>) {
        autors_runtime::block_on(self.0.get_daq_resolution_info())
    }

    /// Drives [`XcpMaster::get_pgm_processor_info`] to completion on the calling thread.
    pub fn get_pgm_processor_info(&mut self) -> (CmdResult, Option<RespGetPgmProcessorInfo>) {
        autors_runtime::block_on(self.0.get_pgm_processor_info())
    }

    /// Drives [`XcpMaster::disconnect`] to completion on the calling thread.
    pub fn disconnect(&mut self, use_timeout_ms: i32) -> bool {
        autors_runtime::block_on(self.0.disconnect(use_timeout_ms))
    }

    /// Drives [`XcpMaster::close`] to completion on the calling thread.
    pub fn close(&mut self) {
        autors_runtime::block_on(self.0.close())
    }

    /// Drives [`XcpMaster::drain_pending`] to completion on the calling thread.
    pub fn drain_pending(&mut self) {
        autors_runtime::block_on(self.0.drain_pending())
    }

    /// Drives [`XcpMaster::poll`] to completion on the calling thread.
    pub fn poll(&mut self) {
        autors_runtime::block_on(self.0.poll())
    }

    /// Drives [`XcpMaster::on_event_received`] to completion on the calling thread.
    pub fn on_event_received(&mut self, frame: &XcpFrame) {
        autors_runtime::block_on(self.0.on_event_received(frame))
    }

    /// Drives [`XcpMaster::read_sync`] to completion on the calling thread.
    pub fn read_sync(
        &mut self,
        len: usize,
        address_extension: u8,
        address: u32,
        progress: ProgressCallback,
    ) -> (bool, Vec<u8>) {
        autors_runtime::block_on(self.0.read_sync(len, address_extension, address, progress))
    }

    /// Drives [`XcpMaster::write_sync`] to completion on the calling thread.
    pub fn write_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: ProgressCallback,
    ) -> bool {
        autors_runtime::block_on(
            self.0
                .write_sync(address_extension, address, data, progress),
        )
    }

    /// Drives [`XcpMaster::program_sync`] to completion on the calling thread.
    pub fn program_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: ProgressCallback,
        modes: Option<&XcpPrgParams>,
    ) -> bool {
        autors_runtime::block_on(self.0.program_sync(
            address_extension,
            address,
            data,
            progress,
            modes,
        ))
    }

    /// Drives [`XcpMaster::program_sync_segments`] to completion on the calling thread.
    pub fn program_sync_segments(
        &mut self,
        address_extension: u8,
        segments: &[XcpMemorySegment],
        progress: ProgressCallback,
        connect_mode: ConnectMode,
        modes: Option<&XcpPrgParams>,
    ) -> bool {
        autors_runtime::block_on(self.0.program_sync_segments(
            address_extension,
            segments,
            progress,
            connect_mode,
            modes,
        ))
    }

    /// Drives [`XcpMaster::modify_bits_guarded`] to completion on the calling thread.
    pub fn modify_bits_guarded(
        &mut self,
        address_extension: u8,
        address: u32,
        s: u8,
        ma: u16,
        mx: u16,
    ) -> bool {
        autors_runtime::block_on(
            self.0
                .modify_bits_guarded(address_extension, address, s, ma, mx),
        )
    }

    /// Drives [`XcpMaster::get_id_data`] to completion on the calling thread.
    pub fn get_id_data(&mut self, id_type: GetIdType) -> Option<Vec<u8>> {
        autors_runtime::block_on(self.0.get_id_data(id_type))
    }

    /// Drives [`XcpMaster::unlock_ecu`] to completion on the calling thread.
    pub fn unlock_ecu(&mut self, status: &RespGetStatus, resource: ResourceType) -> CmdResult {
        autors_runtime::block_on(self.0.unlock_ecu(status, resource))
    }

    /// Drives [`XcpMaster::unlock_ecu_all`] to completion on the calling thread.
    pub fn unlock_ecu_all(&mut self) -> CmdResult {
        autors_runtime::block_on(self.0.unlock_ecu_all())
    }

    /// Drives [`XcpMaster::set_page`] to completion on the calling thread.
    pub fn set_page(&mut self, page: EcuPage) -> bool {
        autors_runtime::block_on(self.0.set_page(page))
    }

    /// Drives [`XcpMaster::freeze_page`] to completion on the calling thread.
    pub fn freeze_page(&mut self, segment: u8, timeout_ms: u32) -> CmdResult {
        autors_runtime::block_on(self.0.freeze_page(segment, timeout_ms))
    }

    /// Drives [`XcpMaster::copy_page2page`] to completion on the calling thread.
    pub fn copy_page2page(&mut self, src_page: EcuPage, dst_page: EcuPage) -> Result<bool> {
        autors_runtime::block_on(self.0.copy_page2page(src_page, dst_page))
    }

    /// Drives [`XcpMaster::get_checksum`] to completion on the calling thread.
    pub fn get_checksum(&mut self, address_extension: u8, address: u32, size: u32) -> Option<u32> {
        autors_runtime::block_on(self.0.get_checksum(address_extension, address, size))
    }

    /// Drives [`XcpMaster::start_measurements`] to completion on the calling thread.
    pub fn start_measurements(&mut self, do_synchronized: bool) -> bool {
        autors_runtime::block_on(self.0.start_measurements(do_synchronized))
    }

    /// Drives [`XcpMaster::stop_measurements`] to completion on the calling thread.
    pub fn stop_measurements(&mut self, do_synchronized: bool) -> bool {
        autors_runtime::block_on(self.0.stop_measurements(do_synchronized))
    }

    /// Drives [`XcpMaster::get_slave_ids`] to completion on the calling thread.
    pub fn get_slave_ids(
        &mut self,
        device: &Arc<Mutex<dyn CanDevice + Send>>,
        timeout: u16,
    ) -> Result<(CmdResult, Vec<(u32, u32)>)> {
        autors_runtime::block_on(self.0.get_slave_ids(device, timeout))
    }
}

impl BlockingXcpMaster {
    /// Delegates to [`XcpMaster::set_address_mapper`] (inherently synchronous).
    pub fn set_address_mapper(&mut self, mapper: impl Fn(u32) -> u32 + Send + 'static) {
        self.0.set_address_mapper(mapper)
    }

    /// Delegates to [`XcpMaster::map_address`] (inherently synchronous).
    pub fn map_address(&self, address: u32) -> u32 {
        self.0.map_address(address)
    }

    /// Delegates to [`XcpMaster::set_seed_key_provider`] (inherently synchronous).
    pub fn set_seed_key_provider(&mut self, provider: Box<dyn XcpSeedKeyProvider>) {
        self.0.set_seed_key_provider(provider)
    }

    /// Delegates to [`XcpMaster::is_daq_running`] (inherently synchronous).
    pub fn is_daq_running(&self) -> bool {
        self.0.is_daq_running()
    }

    /// Delegates to [`XcpMaster::daq_clock`] (inherently synchronous).
    pub fn daq_clock(&self) -> u32 {
        self.0.daq_clock()
    }

    /// Delegates to [`XcpMaster::daq_and_evt_map`] (inherently synchronous).
    pub fn daq_and_evt_map(&self) -> &BTreeMap<u16, DaqAndEvt> {
        self.0.daq_and_evt_map()
    }

    /// Delegates to [`XcpMaster::cached`] (inherently synchronous).
    pub fn cached(&self) -> &CachedResponses {
        self.0.cached()
    }

    /// Delegates to [`XcpMaster::active_page`] (inherently synchronous).
    pub fn active_page(&self) -> EcuPage {
        self.0.active_page()
    }

    /// Delegates to [`XcpMaster::byte_order`] (inherently synchronous).
    pub fn byte_order(&self) -> ByteOrder {
        self.0.byte_order()
    }

    /// Delegates to [`XcpMaster::version`] (inherently synchronous).
    pub fn version(&self) -> String {
        self.0.version()
    }

    /// Delegates to [`XcpMaster::str_address_granularity`] (inherently synchronous).
    pub fn str_address_granularity(&self) -> &'static str {
        self.0.str_address_granularity()
    }

    /// Delegates to [`XcpMaster::can_write`] (inherently synchronous).
    pub fn can_write(&self) -> bool {
        self.0.can_write()
    }

    /// Delegates to [`XcpMaster::is_allowed_request`] (inherently synchronous).
    pub fn is_allowed_request(&self, cmd: &str) -> bool {
        self.0.is_allowed_request(cmd)
    }

    /// Delegates to [`XcpMaster::is_slave_block_mode`] (inherently synchronous).
    pub fn is_slave_block_mode(&self) -> bool {
        self.0.is_slave_block_mode()
    }

    /// Delegates to [`XcpMaster::daqs`] (inherently synchronous).
    pub fn daqs(&self) -> &DaqDictXcp {
        self.0.daqs()
    }

    /// Delegates to [`XcpMaster::daqs_mut`] (inherently synchronous).
    pub fn daqs_mut(&mut self) -> &mut DaqDictXcp {
        self.0.daqs_mut()
    }

    /// Delegates to [`XcpMaster::on_daq_frame_received`] (inherently synchronous).
    pub fn on_daq_frame_received(&mut self, frame: &XcpFrame) {
        self.0.on_daq_frame_received(frame)
    }

    /// Delegates to [`XcpMaster::configure_measurements`] (inherently synchronous).
    pub fn configure_measurements(&mut self, measurements: &mut Vec<DaqMeasurement>) -> Result<()> {
        self.0.configure_measurements(measurements)
    }

    /// Delegates to [`XcpMaster::into_shared`] (inherently synchronous).
    pub fn into_shared(self) -> Result<Arc<Mutex<XcpMaster>>> {
        self.0.into_shared()
    }
}
