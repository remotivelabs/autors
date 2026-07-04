//! Synchronous facade over the async CCP master core.
//! [`BlockingCcpMaster`] wraps a [`CcpMaster`]: every command/flow method
//! (`connect`, `disconnect`,
//! `get_seed`/`unlock`, `read_sync`/`write_sync`/`program_sync`, page
//! operations, DAQ configuration and start/stop, ...) is driven to completion
//! on the calling thread via [`autors_runtime::block_on`], while the
//! inherently synchronous accessors (cached responses, error state, callback
//! registration, `configure_measurements`, ...) delegate directly.
//! The wrapped master is publicly accessible (`.0`), so the public
//! configuration fields (`base`, `ccp_if`, `device`, `config`,
//! `source_and_raster_map`) remain directly reachable.
//! Do not call the blocking methods from within async code running on the
//! shared runtime: [`autors_runtime::block_on`] panics on its executor
//! threads, the same restriction as `tokio::runtime::Runtime::block_on`.

use std::collections::BTreeMap;

use autors_a2l::model::enums::EcuPage;
use autors_a2l::model::module::ModPar;
use autors_can::device::CanDevice;
use autors_comm::base::{
    ConnectBehaviourType, DaqMeasurement, EpkCheckResult, ProgressCallback, SeedKeyProvider,
    StartStopMode, XcpPrgParams,
};

use crate::ccp::{
    CcpErrorCallback, CcpEventCallback, CcpFrame, CcpMaster, CmdResult, CommandCode,
    DisconnectMode, ResourceType, RespActionDiagService, RespBase, RespBuildChksum, RespDownload,
    RespExchangeId, RespGetActiveCalPage, RespGetCcpVersion, RespGetDaqSize, RespGetSStatus,
    RespGetSeed, RespUnlock, SessionState,
};
use crate::error::Result;
use crate::ifdata_ccp::{CcpTpBlob, SourceAndRaster};

/// Synchronous wrapper around a [`CcpMaster`].
/// The wrapped master is publicly accessible (`.0`), so inherent methods of
/// the async core remain reachable.
pub struct BlockingCcpMaster<D: CanDevice>(pub CcpMaster<D>);

impl<D: CanDevice + Send> BlockingCcpMaster<D> {
    /// Wraps `master` in the synchronous facade.
    pub fn new(master: CcpMaster<D>) -> Self {
        Self(master)
    }

    /// Unwraps the facade, returning the inner master.
    pub fn into_inner(self) -> CcpMaster<D> {
        self.0
    }

    /// Delegates to [`CcpMaster::new`] (constructor, inherently synchronous).
    pub fn new_master(
        connect_behaviour: ConnectBehaviourType,
        ccp_if: CcpTpBlob,
        device: D,
    ) -> Self {
        Self(CcpMaster::new(connect_behaviour, ccp_if, device))
    }
}

impl<D: CanDevice + Send> BlockingCcpMaster<D> {
    /// Drives [`CcpMaster::poll`] to completion on the calling thread.
    pub fn poll(&mut self) -> usize {
        autors_runtime::block_on(self.0.poll())
    }

    /// Drives [`CcpMaster::set_connection_state`] to completion on the calling thread.
    pub fn set_connection_state(&mut self, resp: Option<RespBase>) {
        autors_runtime::block_on(self.0.set_connection_state(resp))
    }

    /// Drives [`CcpMaster::connect`] to completion on the calling thread.
    pub fn connect(&mut self) -> CmdResult {
        autors_runtime::block_on(self.0.connect())
    }

    /// Drives [`CcpMaster::internal_connect`] to completion on the calling thread.
    pub fn internal_connect(&mut self) -> bool {
        autors_runtime::block_on(self.0.internal_connect())
    }

    /// Drives [`CcpMaster::internal_get_status`] to completion on the calling thread.
    pub fn internal_get_status(&mut self) -> bool {
        autors_runtime::block_on(self.0.internal_get_status())
    }

    /// Drives [`CcpMaster::disconnect`] to completion on the calling thread.
    pub fn disconnect(&mut self, use_timeout: i32) -> bool {
        autors_runtime::block_on(self.0.disconnect(use_timeout))
    }

    /// Drives [`CcpMaster::disconnect_mode`] to completion on the calling thread.
    pub fn disconnect_mode(&mut self, mode: DisconnectMode, use_timeout: i32) -> bool {
        autors_runtime::block_on(self.0.disconnect_mode(mode, use_timeout))
    }

    /// Drives [`CcpMaster::close`] to completion on the calling thread.
    pub fn close(self) {
        autors_runtime::block_on(self.0.close())
    }

    /// Drives [`CcpMaster::get_ccp_versions`] to completion on the calling thread.
    pub fn get_ccp_versions(
        &mut self,
        desired_main: u8,
        desired_release: u8,
    ) -> (CmdResult, Option<RespGetCcpVersion>) {
        autors_runtime::block_on(self.0.get_ccp_versions(desired_main, desired_release))
    }

    /// Drives [`CcpMaster::exchange_id`] to completion on the calling thread.
    pub fn exchange_id(&mut self) -> (CmdResult, Option<RespExchangeId>) {
        autors_runtime::block_on(self.0.exchange_id())
    }

    /// Drives [`CcpMaster::get_seed`] to completion on the calling thread.
    pub fn get_seed(
        &mut self,
        resource: ResourceType,
    ) -> (CmdResult, Option<RespGetSeed>, Vec<u8>) {
        autors_runtime::block_on(self.0.get_seed(resource))
    }

    /// Drives [`CcpMaster::set_mta`] to completion on the calling thread.
    pub fn set_mta(&mut self, mta_no: u8, address_extension: u8, address: u32) -> CmdResult {
        autors_runtime::block_on(self.0.set_mta(mta_no, address_extension, address))
    }

    /// Drives [`CcpMaster::move_`] to completion on the calling thread.
    pub fn move_(&mut self, size: u32) -> CmdResult {
        autors_runtime::block_on(self.0.move_(size))
    }

    /// Drives [`CcpMaster::clear_memory`] to completion on the calling thread.
    pub fn clear_memory(&mut self, size: u32) -> CmdResult {
        autors_runtime::block_on(self.0.clear_memory(size))
    }

    /// Drives [`CcpMaster::build_chksum`] to completion on the calling thread.
    pub fn build_chksum(&mut self, block_size: u32) -> (CmdResult, Option<RespBuildChksum>) {
        autors_runtime::block_on(self.0.build_chksum(block_size))
    }

    /// Drives [`CcpMaster::unlock`] to completion on the calling thread.
    pub fn unlock(&mut self, key: &[u8]) -> (CmdResult, Option<RespUnlock>) {
        autors_runtime::block_on(self.0.unlock(key))
    }

    /// Drives [`CcpMaster::upload`] to completion on the calling thread.
    pub fn upload(&mut self, size: u8) -> (CmdResult, Vec<u8>) {
        autors_runtime::block_on(self.0.upload(size))
    }

    /// Drives [`CcpMaster::short_up`] to completion on the calling thread.
    pub fn short_up(
        &mut self,
        size: u8,
        address_extension: u8,
        address: u32,
    ) -> (CmdResult, Vec<u8>) {
        autors_runtime::block_on(self.0.short_up(size, address_extension, address))
    }

    /// Drives [`CcpMaster::download`] to completion on the calling thread.
    pub fn download(&mut self, bytes: &[u8]) -> (CmdResult, Option<RespDownload>) {
        autors_runtime::block_on(self.0.download(bytes))
    }

    /// Drives [`CcpMaster::program`] to completion on the calling thread.
    pub fn program(&mut self, bytes: &[u8]) -> (CmdResult, Option<RespDownload>) {
        autors_runtime::block_on(self.0.program(bytes))
    }

    /// Drives [`CcpMaster::download6`] to completion on the calling thread.
    pub fn download6(&mut self, bytes: &[u8]) -> (CmdResult, Option<RespDownload>) {
        autors_runtime::block_on(self.0.download6(bytes))
    }

    /// Drives [`CcpMaster::program6`] to completion on the calling thread.
    pub fn program6(&mut self, bytes: &[u8]) -> (CmdResult, Option<RespDownload>) {
        autors_runtime::block_on(self.0.program6(bytes))
    }

    /// Drives [`CcpMaster::get_status`] to completion on the calling thread.
    pub fn get_status(&mut self) -> (CmdResult, Option<RespGetSStatus>) {
        autors_runtime::block_on(self.0.get_status())
    }

    /// Drives [`CcpMaster::set_status`] to completion on the calling thread.
    pub fn set_status(&mut self, state: SessionState) -> CmdResult {
        autors_runtime::block_on(self.0.set_status(state))
    }

    /// Drives [`CcpMaster::get_daq_size`] to completion on the calling thread.
    pub fn get_daq_size(&mut self, daq_no: u8, can_id: u32) -> (CmdResult, Option<RespGetDaqSize>) {
        autors_runtime::block_on(self.0.get_daq_size(daq_no, can_id))
    }

    /// Drives [`CcpMaster::action_service`] to completion on the calling thread.
    pub fn action_service(
        &mut self,
        no: u16,
        add_bytes: Option<&[u8]>,
    ) -> (CmdResult, Option<RespActionDiagService>, Vec<u8>) {
        autors_runtime::block_on(self.0.action_service(no, add_bytes))
    }

    /// Drives [`CcpMaster::diag_service`] to completion on the calling thread.
    pub fn diag_service(
        &mut self,
        no: u16,
        add_bytes: Option<&[u8]>,
    ) -> (CmdResult, Option<RespActionDiagService>, Vec<u8>) {
        autors_runtime::block_on(self.0.diag_service(no, add_bytes))
    }

    /// Drives [`CcpMaster::set_daq_ptr`] to completion on the calling thread.
    pub fn set_daq_ptr(&mut self, daq_list_no: u8, odt_no: u8, element_no: u8) -> CmdResult {
        autors_runtime::block_on(self.0.set_daq_ptr(daq_list_no, odt_no, element_no))
    }

    /// Drives [`CcpMaster::write_daq`] to completion on the calling thread.
    pub fn write_daq(&mut self, size: u8, extension: u8, address: u32) -> CmdResult {
        autors_runtime::block_on(self.0.write_daq(size, extension, address))
    }

    /// Drives [`CcpMaster::start_stop`] to completion on the calling thread.
    pub fn start_stop(
        &mut self,
        mode: StartStopMode,
        daq_no: u8,
        last_odt_no: u8,
        evt_chn_no: u8,
        prescaler: u16,
    ) -> CmdResult {
        autors_runtime::block_on(self.0.start_stop(
            mode,
            daq_no,
            last_odt_no,
            evt_chn_no,
            prescaler,
        ))
    }

    /// Drives [`CcpMaster::start_stop_all`] to completion on the calling thread.
    pub fn start_stop_all(&mut self, mode: StartStopMode) -> CmdResult {
        autors_runtime::block_on(self.0.start_stop_all(mode))
    }

    /// Drives [`CcpMaster::get_active_cal_page`] to completion on the calling thread.
    pub fn get_active_cal_page(&mut self) -> (CmdResult, Option<RespGetActiveCalPage>) {
        autors_runtime::block_on(self.0.get_active_cal_page())
    }

    /// Drives [`CcpMaster::select_cal_page`] to completion on the calling thread.
    pub fn select_cal_page(&mut self) -> CmdResult {
        autors_runtime::block_on(self.0.select_cal_page())
    }

    /// Drives [`CcpMaster::unlock_ecu`] to completion on the calling thread.
    pub fn unlock_ecu(&mut self, resource: ResourceType) -> CmdResult {
        autors_runtime::block_on(self.0.unlock_ecu(resource))
    }

    /// Drives [`CcpMaster::unlock_ecu_with`] to completion on the calling thread.
    pub fn unlock_ecu_with(
        &mut self,
        sk: &(dyn SeedKeyProvider + 'static),
        resource: ResourceType,
    ) -> CmdResult {
        autors_runtime::block_on(self.0.unlock_ecu_with(sk, resource))
    }

    /// Drives [`CcpMaster::unlock_ecu_multi`] to completion on the calling thread.
    pub fn unlock_ecu_multi(&mut self, resource: ResourceType) -> CmdResult {
        autors_runtime::block_on(self.0.unlock_ecu_multi(resource))
    }

    /// Drives [`CcpMaster::read_sync`] to completion on the calling thread.
    pub fn read_sync(
        &mut self,
        len: usize,
        address_extension: u8,
        address: u32,
        progress: ProgressCallback<'_>,
    ) -> Option<Vec<u8>> {
        autors_runtime::block_on(self.0.read_sync(len, address_extension, address, progress))
    }

    /// Drives [`CcpMaster::write_sync`] to completion on the calling thread.
    pub fn write_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: ProgressCallback<'_>,
    ) -> bool {
        autors_runtime::block_on(
            self.0
                .write_sync(address_extension, address, data, progress),
        )
    }

    /// Drives [`CcpMaster::program_sync`] to completion on the calling thread.
    pub fn program_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: ProgressCallback<'_>,
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

    /// Drives [`CcpMaster::copy_page2page`] to completion on the calling thread.
    pub fn copy_page2page(&mut self, src_page: EcuPage, dst_page: EcuPage) -> bool {
        autors_runtime::block_on(self.0.copy_page2page(src_page, dst_page))
    }

    /// Drives [`CcpMaster::set_page`] to completion on the calling thread.
    pub fn set_page(&mut self, page: EcuPage) -> bool {
        autors_runtime::block_on(self.0.set_page(page))
    }

    /// Drives [`CcpMaster::get_checksum`] to completion on the calling thread.
    pub fn get_checksum(&mut self, address_extension: u8, address: u32, size: u32) -> Option<u32> {
        autors_runtime::block_on(self.0.get_checksum(address_extension, address, size))
    }

    /// Drives [`CcpMaster::epk_check`] to completion on the calling thread.
    pub fn epk_check(
        &mut self,
        epk_address: u32,
        expected_epk: &str,
    ) -> (EpkCheckResult, Option<String>) {
        autors_runtime::block_on(self.0.epk_check(epk_address, expected_epk))
    }

    /// Drives [`CcpMaster::epk_check_mod_par`] to completion on the calling thread.
    pub fn epk_check_mod_par(
        &mut self,
        mod_par: Option<&ModPar>,
    ) -> (EpkCheckResult, Option<String>) {
        autors_runtime::block_on(self.0.epk_check_mod_par(mod_par))
    }

    /// Drives [`CcpMaster::start_measurements`] to completion on the calling thread.
    pub fn start_measurements(&mut self, do_synchronized: bool) -> bool {
        autors_runtime::block_on(self.0.start_measurements(do_synchronized))
    }

    /// Drives [`CcpMaster::stop_measurements`] to completion on the calling thread.
    pub fn stop_measurements(&mut self, do_synchronized: bool) -> bool {
        autors_runtime::block_on(self.0.stop_measurements(do_synchronized))
    }
}

impl<D: CanDevice + Send> BlockingCcpMaster<D> {
    /// Delegates to [`CcpMaster::set_source_and_raster_map`] (inherently synchronous).
    pub fn set_source_and_raster_map(&mut self, map: BTreeMap<u16, SourceAndRaster>) {
        self.0.set_source_and_raster_map(map)
    }

    /// Delegates to [`CcpMaster::add_event_callback`] (inherently synchronous).
    pub fn add_event_callback(&mut self, cb: CcpEventCallback) {
        self.0.add_event_callback(cb)
    }

    /// Delegates to [`CcpMaster::add_error_callback`] (inherently synchronous).
    pub fn add_error_callback(&mut self, cb: CcpErrorCallback) {
        self.0.add_error_callback(cb)
    }

    /// Delegates to [`CcpMaster::is_allowed_request`] (inherently synchronous).
    pub fn is_allowed_request(&self, cmd: CommandCode) -> bool {
        self.0.is_allowed_request(cmd)
    }

    /// Delegates to [`CcpMaster::connect_response`] (inherently synchronous).
    pub fn connect_response(&self) -> Option<&RespBase> {
        self.0.connect_response()
    }

    /// Delegates to [`CcpMaster::status_response`] (inherently synchronous).
    pub fn status_response(&self) -> Option<&RespGetSStatus> {
        self.0.status_response()
    }

    /// Delegates to [`CcpMaster::exchange_id_response`] (inherently synchronous).
    pub fn exchange_id_response(&self) -> Option<&RespExchangeId> {
        self.0.exchange_id_response()
    }

    /// Delegates to [`CcpMaster::exchange_id_str`] (inherently synchronous).
    pub fn exchange_id_str(&self) -> &str {
        self.0.exchange_id_str()
    }

    /// Delegates to [`CcpMaster::version`] (inherently synchronous).
    pub fn version(&self) -> String {
        self.0.version()
    }

    /// Delegates to [`CcpMaster::active_page`] (inherently synchronous).
    pub fn active_page(&self) -> EcuPage {
        self.0.active_page()
    }

    /// Delegates to [`CcpMaster::can_write`] (inherently synchronous).
    pub fn can_write(&self) -> bool {
        self.0.can_write()
    }

    /// Delegates to [`CcpMaster::is_daq_running`] (inherently synchronous).
    pub fn is_daq_running(&self) -> bool {
        self.0.is_daq_running()
    }

    /// Delegates to [`CcpMaster::last_response`] (inherently synchronous).
    pub fn last_response(&self) -> Option<&CcpFrame> {
        self.0.last_response()
    }

    /// Delegates to [`CcpMaster::last_error_response`] (inherently synchronous).
    pub fn last_error_response(&self) -> CmdResult {
        self.0.last_error_response()
    }

    /// Delegates to [`CcpMaster::last_error_text`] (inherently synchronous).
    pub fn last_error_text(&self) -> String {
        self.0.last_error_text()
    }

    /// Delegates to [`CcpMaster::reset_last_error`] (inherently synchronous).
    pub fn reset_last_error(&mut self) {
        self.0.reset_last_error()
    }

    /// Delegates to [`CcpMaster::map_address`] (inherently synchronous).
    pub fn map_address(&self, address: u32) -> u32 {
        self.0.map_address(address)
    }

    /// Delegates to [`CcpMaster::configure_measurements`] (inherently synchronous).
    pub fn configure_measurements(&mut self, measurements: Vec<DaqMeasurement>) -> Result<usize> {
        self.0.configure_measurements(measurements)
    }

    /// Delegates to [`CcpMaster::on_daq_frame_received`] (inherently synchronous).
    pub fn on_daq_frame_received(&mut self, frame: &CcpFrame) {
        self.0.on_daq_frame_received(frame)
    }
}
