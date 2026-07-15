//! Synchronous facade over the async PRM executor core.
//! [`BlockingPrmExecutor`] mirrors the operations and default "not implemented"
//! behavior of [`PrmExecutor`], and [`BlockingPrm`] mirrors [`crate::prm_if::Prm`].
//! [`BlockingExecutor`] wraps an [`Executor`] and implements
//! [`BlockingPrmExecutor`]: every instruction with bus I/O or wait semantics
//! (`wait`, `can_send_message`, the `uds_*`/`udsx_*` service family, the
//! `ccp_*`/`ccpx_*` family, `xcp_connect`/`xcp_program_reset`) is driven to
//! completion on the calling thread via [`autors_runtime::block_on`], while
//! the inherently synchronous instructions (variable access, message
//! display, state-only stubs) delegate directly.
//! The wrapped executor is publicly accessible (`.0`), so the builder-style
//! configuration methods (`with_uds`/`with_ccp`/`with_xcp`/...) and the
//! cancel handle remain reachable.
//! Do not call the blocking methods from within async code running on the
//! shared runtime: [`autors_runtime::block_on`] panics on its executor
//! threads, the same restriction as `tokio::runtime::Runtime::block_on`.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use autors_diag::uds::SeedKeyProvider;

use crate::error::Result;
use crate::executor::{CanSend, CcpOps, Executor, UdsOps, XcpOps};
use crate::prm::{Mode, PrmFile};
use crate::prm_if::{not_implemented, PrmExecutor, PrmMsg, PrmValue};

/// Synchronous counterpart of [`crate::prm_if::Prm`].
pub trait BlockingPrm {
    /// Drives the whole instruction flow to completion on the calling thread.
    fn execute(&mut self) -> Result<()>;
}

/// Synchronous counterpart of [`PrmExecutor`]. Every instruction defaults to
/// a "not implemented" error unless an implementation overrides it.
pub trait BlockingPrmExecutor {
    /// See [`PrmExecutor::state`].
    fn state(&self) -> u64;

    /// See [`PrmExecutor::set_state`].
    fn set_state(&mut self, state: u64);

    /// Drives [`PrmExecutor::can_send_message`] to completion on the calling thread.
    fn can_send_message(&mut self, id: u64, data: &str) -> Result<()> {
        let _ = (id, data);
        Err(not_implemented("CAN_SEND_MESSAGE"))
    }

    /// See [`PrmExecutor::call`].
    fn call(&mut self, procedure: &str) -> Result<u64> {
        let _ = procedure;
        Err(not_implemented("CALL"))
    }

    /// See [`PrmExecutor::set_re_entry`].
    fn set_re_entry(&mut self, step: &str) -> Result<()> {
        let _ = step;
        Err(not_implemented("SET_RE_ENTRY"))
    }

    /// Drives [`PrmExecutor::wait`] to completion on the calling thread.
    fn wait(&mut self, time_in_ms: u64) -> Result<()> {
        let _ = time_in_ms;
        Err(not_implemented("WAIT"))
    }

    /// See [`PrmExecutor::display_message`].
    fn display_message(&mut self, msg: &str, par: u64) -> Result<()> {
        let _ = (msg, par);
        Err(not_implemented("DISPLAY_MESSAGE"))
    }

    /// See [`PrmExecutor::display_error_message`].
    fn display_error_message(&mut self, par: u64) -> Result<()> {
        let _ = par;
        Err(not_implemented("DISPLAY_ERROR_MESSAGE"))
    }

    /// See [`PrmExecutor::default_screen_layout`].
    fn default_screen_layout(&mut self, value: u64) -> Result<()> {
        let _ = value;
        Err(not_implemented("DEFAULT_SCREEN_LAYOUT"))
    }

    /// See [`PrmExecutor::extended_message`].
    fn extended_message(&mut self, par: u64) -> Result<()> {
        let _ = par;
        Err(not_implemented("EXTENDED_MESSAGE"))
    }

    /// See [`PrmExecutor::set_variable_str`].
    fn set_variable_str(&mut self, variable: u64, action: &str) -> Result<()> {
        let _ = (variable, action);
        Err(not_implemented("SET_VARIABLE(string)"))
    }

    /// See [`PrmExecutor::set_variable_num`].
    fn set_variable_num(&mut self, variable: u64, action: u64) -> Result<()> {
        let _ = (variable, action);
        Err(not_implemented("SET_VARIABLE(ulong)"))
    }

    /// See [`PrmExecutor::get_variable`].
    fn get_variable(&mut self, variable: u64) -> Result<()> {
        let _ = variable;
        Err(not_implemented("GET_VARIABLE"))
    }

    /// See [`PrmExecutor::run_dll`].
    fn run_dll(&mut self, file: &str, args: &[PrmValue]) -> Result<()> {
        let _ = (file, args);
        Err(not_implemented("RUN_DLL"))
    }

    /// See [`PrmExecutor::check_inca_configuration`].
    fn check_inca_configuration(&mut self) -> Result<()> {
        Err(not_implemented("CHECK_INCA_CONFIGURATION"))
    }

    /// See [`PrmExecutor::set_debug_level`].
    fn set_debug_level(&mut self, level: u64) -> Result<()> {
        let _ = level;
        Err(not_implemented("SET_DEBUG_LEVEL"))
    }

    /// See [`PrmExecutor::init_flash_programming`].
    fn init_flash_programming(
        &mut self,
        ecu_address: u64,
        flash_type: i64,
        cnf: &str,
    ) -> Result<()> {
        let _ = (ecu_address, flash_type, cnf);
        Err(not_implemented("INIT_FLASH_PROGRAMMING"))
    }

    /// See [`PrmExecutor::udsb_init_communication`].
    fn udsb_init_communication(&mut self) -> Result<()> {
        Err(not_implemented("UDSB_INIT_COMMUNICATION"))
    }

    /// See [`PrmExecutor::udsb_msg_ret_get_at`].
    fn udsb_msg_ret_get_at(&mut self, idx: u64, default: u8) -> Result<()> {
        let _ = (idx, default);
        Err(not_implemented("UDSB_MSG_RET_GET_AT"))
    }

    /// Drives [`PrmExecutor::uds_communication_control`] to completion on the calling thread.
    fn uds_communication_control(
        &mut self,
        control_type: u64,
        communication_type: u64,
    ) -> Result<()> {
        let _ = (control_type, communication_type);
        Err(not_implemented("UDS_COMMUNICATION_CONTROL"))
    }

    /// Drives [`PrmExecutor::uds_diagnostic_session_control`] to completion on the calling thread.
    fn uds_diagnostic_session_control(&mut self, sf: u64) -> Result<()> {
        let _ = sf;
        Err(not_implemented("UDS_DIAGNOSTIC_SESSION_CONTROL"))
    }

    /// Drives [`PrmExecutor::uds_control_dtc_setting`] to completion on the calling thread.
    fn uds_control_dtc_setting(&mut self, dtc_off: u64, msg: &str) -> Result<()> {
        let _ = (dtc_off, msg);
        Err(not_implemented("UDS_CONTROL_DTC_SETTING"))
    }

    /// Drives [`PrmExecutor::uds_read_data_by_identifier`] to completion on the calling thread.
    fn uds_read_data_by_identifier(&mut self, hex_value: &str) -> Result<()> {
        let _ = hex_value;
        Err(not_implemented("UDS_READ_DATA_BY_IDENTIFIER"))
    }

    /// Drives [`PrmExecutor::uds_write_data_by_identifier`] to completion on the calling thread.
    fn uds_write_data_by_identifier(&mut self, id: u64, hex_data: &str) -> Result<()> {
        let _ = (id, hex_data);
        Err(not_implemented("UDS_WRITE_DATA_BY_IDENTIFIER"))
    }

    /// Drives [`PrmExecutor::uds_routine_control`] to completion on the calling thread.
    fn uds_routine_control(&mut self, sf: u64, routine: u64, data_bytes: &str) -> Result<()> {
        let _ = (sf, routine, data_bytes);
        Err(not_implemented("UDS_ROUTINE_CONTROL"))
    }

    /// Drives [`PrmExecutor::uds_pass_through`] to completion on the calling thread.
    fn uds_pass_through(&mut self, hex_data: &str) -> Result<()> {
        let _ = hex_data;
        Err(not_implemented("UDS_PASS_THROUGH"))
    }

    /// Drives [`PrmExecutor::uds_ecu_reset`] to completion on the calling thread.
    fn uds_ecu_reset(&mut self, reset_type: u64) -> Result<()> {
        let _ = reset_type;
        Err(not_implemented("UDS_ECU_RESET"))
    }

    /// See [`PrmExecutor::uds_read_data_by_identifier_get_data_rec_at`].
    fn uds_read_data_by_identifier_get_data_rec_at(&mut self, idx: u64) -> Result<()> {
        let _ = idx;
        Err(not_implemented(
            "UDS_READ_DATA_BY_IDENTIFIER_GET_DATA_REC_AT",
        ))
    }

    /// Drives [`PrmExecutor::uds_clear_dtc_information`] to completion on the calling thread.
    fn uds_clear_dtc_information(&mut self, p: u64) -> Result<()> {
        let _ = p;
        Err(not_implemented("UDS_CLEAR_DTC_INFORMATION"))
    }

    /// Drives [`PrmExecutor::udsx_verify_memory`] to completion on the calling thread.
    fn udsx_verify_memory(
        &mut self,
        file: &str,
        src_seg_idx: u64,
        dst_seg_idx: u64,
        max_block_len: u64,
        checksum: u64,
        timeout_msec: u64,
    ) -> Result<()> {
        let _ = (
            file,
            src_seg_idx,
            dst_seg_idx,
            max_block_len,
            checksum,
            timeout_msec,
        );
        Err(not_implemented("UDSX_VERIFY_MEMORY"))
    }

    /// Drives [`PrmExecutor::udsx_security_access`] to completion on the calling thread.
    fn udsx_security_access(&mut self, sf: u64, variant: u64, sk_file: &str) -> Result<()> {
        let _ = (sf, variant, sk_file);
        Err(not_implemented("UDSX_SECURITY_ACCESS"))
    }

    /// Drives [`PrmExecutor::udsx_program_memory`] to completion on the calling thread.
    fn udsx_program_memory(
        &mut self,
        file: &str,
        src_seg_idx: u64,
        dst_seg_idx: u64,
        p1: u64,
        p2: &str,
    ) -> Result<()> {
        let _ = (file, src_seg_idx, dst_seg_idx, p1, p2);
        Err(not_implemented("UDSX_PROGRAM_MEMORY"))
    }

    /// Drives [`PrmExecutor::udsx_read_data_by_identifier_scaling`] to completion on the calling thread.
    fn udsx_read_data_by_identifier_scaling(
        &mut self,
        msg: &str,
        unknown: &str,
        hex_id: &str,
        hex_idx: &str,
    ) -> Result<()> {
        let _ = (msg, unknown, hex_id, hex_idx);
        Err(not_implemented("UDSX_READ_DATA_BY_IDENTIFIER_SCALING"))
    }

    /// Drives [`PrmExecutor::ccp_disconnect`] to completion on the calling thread.
    fn ccp_disconnect(&mut self, permanent: u64) -> Result<()> {
        let _ = permanent;
        Err(not_implemented("CCP_DISCONNECT"))
    }

    /// See [`PrmExecutor::ccpb_store_ccp_cmd_timeout`].
    fn ccpb_store_ccp_cmd_timeout(&mut self, cmd: u64, timeout: u64) -> Result<()> {
        let _ = (cmd, timeout);
        Err(not_implemented("CCPB_STORE_CCP_CMD_TIMEOUT"))
    }

    /// See [`PrmExecutor::ccpb_set_canids`].
    fn ccpb_set_canids(&mut self, cmd_id: u64, rsp_id: u64, station_addr: u64) -> Result<()> {
        let _ = (cmd_id, rsp_id, station_addr);
        Err(not_implemented("CCPB_SET_CANIDS"))
    }

    /// Drives [`PrmExecutor::ccpx_start_ecu_communication`] to completion on the calling thread.
    fn ccpx_start_ecu_communication(&mut self) -> Result<()> {
        Err(not_implemented("CCPX_START_ECU_COMMUNICATION"))
    }

    /// Drives [`PrmExecutor::ccpx_diag_service`] to completion on the calling thread.
    fn ccpx_diag_service(&mut self, diag: u64, p: &[u64]) -> Result<()> {
        let _ = (diag, p);
        Err(not_implemented("CCPX_DIAG_SERVICE"))
    }

    /// Drives [`PrmExecutor::ccpx_action_service`] to completion on the calling thread.
    fn ccpx_action_service(&mut self, action: u64, p: &[u64]) -> Result<()> {
        let _ = (action, p);
        Err(not_implemented("CCPX_ACTION_SERVICE"))
    }

    /// See [`PrmExecutor::show_programming_info`].
    fn show_programming_info(&mut self, step: u64, bin_file: &str, p2: u64) -> Result<()> {
        let _ = (step, bin_file, p2);
        Err(not_implemented("SHOW_PROGRAMMING_INFO"))
    }

    /// Drives [`PrmExecutor::ccpx_erase_memory`] to completion on the calling thread.
    fn ccpx_erase_memory(&mut self, seg_idx: u64, timeout: u64) -> Result<()> {
        let _ = (seg_idx, timeout);
        Err(not_implemented("CCPX_ERASE_MEMORY"))
    }

    /// Drives [`PrmExecutor::ccpx_program_memory`] to completion on the calling thread.
    fn ccpx_program_memory(
        &mut self,
        data_file: &str,
        src_seg_idx: u64,
        dst_seg_idx: u64,
        ccp_program_zero: u64,
        timeout: u64,
    ) -> Result<()> {
        let _ = (
            data_file,
            src_seg_idx,
            dst_seg_idx,
            ccp_program_zero,
            timeout,
        );
        Err(not_implemented("CCPX_PROGRAM_MEMORY"))
    }

    /// Drives [`PrmExecutor::xcp_connect`] to completion on the calling thread.
    fn xcp_connect(&mut self, mode: u64) -> Result<()> {
        let _ = mode;
        Err(not_implemented("XCP_CONNECT"))
    }

    /// See [`PrmExecutor::xcp_program_start`].
    fn xcp_program_start(&mut self) -> Result<()> {
        Err(not_implemented("XCP_PROGRAM_START"))
    }

    /// See [`PrmExecutor::xcp_set_mta`].
    fn xcp_set_mta(&mut self, adr_ext: u64, address: u64) -> Result<()> {
        let _ = (adr_ext, address);
        Err(not_implemented("XCP_SET_MTA"))
    }

    /// See [`PrmExecutor::xcpx_program_clear`].
    fn xcpx_program_clear(&mut self, mode: u64, len: u64, timeout: u64) -> Result<()> {
        let _ = (mode, len, timeout);
        Err(not_implemented("XCPX_PROGRAM_CLEAR"))
    }

    /// See [`PrmExecutor::xcpx_program_memory_file`].
    fn xcpx_program_memory_file(
        &mut self,
        file: &str,
        p1: u64,
        p2: u64,
        p3: u64,
        p4: u64,
    ) -> Result<()> {
        let _ = (file, p1, p2, p3, p4);
        Err(not_implemented("XCPX_PROGRAM_MEMORY(file,...)"))
    }

    /// See [`PrmExecutor::xcpx_program_memory`].
    fn xcpx_program_memory(&mut self) -> Result<()> {
        Err(not_implemented("XCPX_PROGRAM_MEMORY"))
    }

    /// Drives [`PrmExecutor::xcp_program_reset`] to completion on the calling thread.
    fn xcp_program_reset(&mut self) -> Result<()> {
        Err(not_implemented("XCP_PROGRAM_RESET"))
    }
}

/// Synchronous wrapper around an [`Executor`].
/// The wrapped executor is publicly accessible (`.0`), so inherent methods
/// of the async core remain reachable.
pub struct BlockingExecutor<'a>(pub Executor<'a>);

impl<'a> BlockingExecutor<'a> {
    /// Wraps `executor` in the synchronous facade.
    pub fn wrap(executor: Executor<'a>) -> Self {
        Self(executor)
    }

    /// Unwraps the facade, returning the inner executor.
    pub fn into_inner(self) -> Executor<'a> {
        self.0
    }

    /// Delegates to [`Executor::new`] (constructor, inherently synchronous).
    pub fn new(prm: &'a PrmFile) -> Self {
        Self(Executor::new(prm))
    }

    /// Delegates to [`Executor::with_uds`] (inherently synchronous).
    pub fn with_uds(self, client: impl UdsOps + 'static) -> Self {
        Self(self.0.with_uds(client))
    }

    /// Delegates to [`Executor::with_ccp`] (inherently synchronous).
    pub fn with_ccp(self, client: impl CcpOps + 'static) -> Result<Self> {
        Ok(Self(self.0.with_ccp(client)?))
    }

    /// Delegates to [`Executor::with_xcp`] (inherently synchronous).
    pub fn with_xcp(self, client: impl XcpOps + 'static) -> Self {
        Self(self.0.with_xcp(client))
    }

    /// Delegates to [`Executor::with_can_sender`] (inherently synchronous).
    pub fn with_can_sender(self, sender: impl CanSend + 'static) -> Self {
        Self(self.0.with_can_sender(sender))
    }

    /// Delegates to [`Executor::with_seed_key_factory`] (inherently synchronous).
    pub fn with_seed_key_factory(
        self,
        factory: impl Fn(&Path) -> Result<Box<dyn SeedKeyProvider>> + 'static,
    ) -> Self {
        Self(self.0.with_seed_key_factory(factory))
    }

    /// Delegates to [`Executor::on_message`] (inherently synchronous).
    pub fn on_message(self, handler: impl FnMut(PrmMsg) + 'static) -> Self {
        Self(self.0.on_message(handler))
    }

    /// Delegates to [`Executor::cancel_handle`] (inherently synchronous).
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.0.cancel_handle()
    }

    /// Delegates to [`Executor::cancel`] (inherently synchronous).
    pub fn cancel(&self) {
        self.0.cancel()
    }

    /// Delegates to [`Executor::is_cancelled`] (inherently synchronous).
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    /// Delegates to [`Executor::mode`] (inherently synchronous).
    pub fn mode(&self) -> Mode {
        self.0.mode()
    }
}

impl BlockingPrmExecutor for BlockingExecutor<'_> {
    fn state(&self) -> u64 {
        self.0.state()
    }

    fn set_state(&mut self, state: u64) {
        self.0.set_state(state)
    }

    fn can_send_message(&mut self, id: u64, data: &str) -> Result<()> {
        autors_runtime::block_on(self.0.can_send_message(id, data))
    }

    fn call(&mut self, procedure: &str) -> Result<u64> {
        self.0.call(procedure)
    }

    fn set_re_entry(&mut self, step: &str) -> Result<()> {
        self.0.set_re_entry(step)
    }

    fn wait(&mut self, time_in_ms: u64) -> Result<()> {
        autors_runtime::block_on(self.0.wait(time_in_ms))
    }

    fn display_message(&mut self, msg: &str, par: u64) -> Result<()> {
        self.0.display_message(msg, par)
    }

    fn display_error_message(&mut self, par: u64) -> Result<()> {
        self.0.display_error_message(par)
    }

    fn default_screen_layout(&mut self, value: u64) -> Result<()> {
        self.0.default_screen_layout(value)
    }

    fn extended_message(&mut self, par: u64) -> Result<()> {
        self.0.extended_message(par)
    }

    fn set_variable_str(&mut self, variable: u64, action: &str) -> Result<()> {
        self.0.set_variable_str(variable, action)
    }

    fn set_variable_num(&mut self, variable: u64, action: u64) -> Result<()> {
        self.0.set_variable_num(variable, action)
    }

    fn get_variable(&mut self, variable: u64) -> Result<()> {
        self.0.get_variable(variable)
    }

    fn run_dll(&mut self, file: &str, args: &[PrmValue]) -> Result<()> {
        self.0.run_dll(file, args)
    }

    fn check_inca_configuration(&mut self) -> Result<()> {
        self.0.check_inca_configuration()
    }

    fn set_debug_level(&mut self, level: u64) -> Result<()> {
        self.0.set_debug_level(level)
    }

    fn init_flash_programming(
        &mut self,
        ecu_address: u64,
        flash_type: i64,
        cnf: &str,
    ) -> Result<()> {
        self.0.init_flash_programming(ecu_address, flash_type, cnf)
    }

    fn udsb_init_communication(&mut self) -> Result<()> {
        self.0.udsb_init_communication()
    }

    fn udsb_msg_ret_get_at(&mut self, idx: u64, default: u8) -> Result<()> {
        self.0.udsb_msg_ret_get_at(idx, default)
    }

    fn uds_communication_control(
        &mut self,
        control_type: u64,
        communication_type: u64,
    ) -> Result<()> {
        autors_runtime::block_on(
            self.0
                .uds_communication_control(control_type, communication_type),
        )
    }

    fn uds_diagnostic_session_control(&mut self, sf: u64) -> Result<()> {
        autors_runtime::block_on(self.0.uds_diagnostic_session_control(sf))
    }

    fn uds_control_dtc_setting(&mut self, dtc_off: u64, msg: &str) -> Result<()> {
        autors_runtime::block_on(self.0.uds_control_dtc_setting(dtc_off, msg))
    }

    fn uds_read_data_by_identifier(&mut self, hex_value: &str) -> Result<()> {
        autors_runtime::block_on(self.0.uds_read_data_by_identifier(hex_value))
    }

    fn uds_write_data_by_identifier(&mut self, id: u64, hex_data: &str) -> Result<()> {
        autors_runtime::block_on(self.0.uds_write_data_by_identifier(id, hex_data))
    }

    fn uds_routine_control(&mut self, sf: u64, routine: u64, data_bytes: &str) -> Result<()> {
        autors_runtime::block_on(self.0.uds_routine_control(sf, routine, data_bytes))
    }

    fn uds_pass_through(&mut self, hex_data: &str) -> Result<()> {
        autors_runtime::block_on(self.0.uds_pass_through(hex_data))
    }

    fn uds_ecu_reset(&mut self, reset_type: u64) -> Result<()> {
        autors_runtime::block_on(self.0.uds_ecu_reset(reset_type))
    }

    fn uds_read_data_by_identifier_get_data_rec_at(&mut self, idx: u64) -> Result<()> {
        self.0.uds_read_data_by_identifier_get_data_rec_at(idx)
    }

    fn uds_clear_dtc_information(&mut self, p: u64) -> Result<()> {
        autors_runtime::block_on(self.0.uds_clear_dtc_information(p))
    }

    fn udsx_verify_memory(
        &mut self,
        file: &str,
        src_seg_idx: u64,
        dst_seg_idx: u64,
        max_block_len: u64,
        checksum: u64,
        timeout_msec: u64,
    ) -> Result<()> {
        autors_runtime::block_on(self.0.udsx_verify_memory(
            file,
            src_seg_idx,
            dst_seg_idx,
            max_block_len,
            checksum,
            timeout_msec,
        ))
    }

    fn udsx_security_access(&mut self, sf: u64, variant: u64, sk_file: &str) -> Result<()> {
        autors_runtime::block_on(self.0.udsx_security_access(sf, variant, sk_file))
    }

    fn udsx_program_memory(
        &mut self,
        file: &str,
        src_seg_idx: u64,
        dst_seg_idx: u64,
        p1: u64,
        p2: &str,
    ) -> Result<()> {
        autors_runtime::block_on(
            self.0
                .udsx_program_memory(file, src_seg_idx, dst_seg_idx, p1, p2),
        )
    }

    fn udsx_read_data_by_identifier_scaling(
        &mut self,
        msg: &str,
        unknown: &str,
        hex_id: &str,
        hex_idx: &str,
    ) -> Result<()> {
        autors_runtime::block_on(
            self.0
                .udsx_read_data_by_identifier_scaling(msg, unknown, hex_id, hex_idx),
        )
    }

    fn ccp_disconnect(&mut self, permanent: u64) -> Result<()> {
        autors_runtime::block_on(self.0.ccp_disconnect(permanent))
    }

    fn ccpb_store_ccp_cmd_timeout(&mut self, cmd: u64, timeout: u64) -> Result<()> {
        self.0.ccpb_store_ccp_cmd_timeout(cmd, timeout)
    }

    fn ccpb_set_canids(&mut self, cmd_id: u64, rsp_id: u64, station_addr: u64) -> Result<()> {
        self.0.ccpb_set_canids(cmd_id, rsp_id, station_addr)
    }

    fn ccpx_start_ecu_communication(&mut self) -> Result<()> {
        autors_runtime::block_on(self.0.ccpx_start_ecu_communication())
    }

    fn ccpx_diag_service(&mut self, diag: u64, p: &[u64]) -> Result<()> {
        autors_runtime::block_on(self.0.ccpx_diag_service(diag, p))
    }

    fn ccpx_action_service(&mut self, action: u64, p: &[u64]) -> Result<()> {
        autors_runtime::block_on(self.0.ccpx_action_service(action, p))
    }

    fn show_programming_info(&mut self, step: u64, bin_file: &str, p2: u64) -> Result<()> {
        self.0.show_programming_info(step, bin_file, p2)
    }

    fn ccpx_erase_memory(&mut self, seg_idx: u64, timeout: u64) -> Result<()> {
        autors_runtime::block_on(self.0.ccpx_erase_memory(seg_idx, timeout))
    }

    fn ccpx_program_memory(
        &mut self,
        data_file: &str,
        src_seg_idx: u64,
        dst_seg_idx: u64,
        ccp_program_zero: u64,
        timeout: u64,
    ) -> Result<()> {
        autors_runtime::block_on(self.0.ccpx_program_memory(
            data_file,
            src_seg_idx,
            dst_seg_idx,
            ccp_program_zero,
            timeout,
        ))
    }

    fn xcp_connect(&mut self, mode: u64) -> Result<()> {
        autors_runtime::block_on(self.0.xcp_connect(mode))
    }

    fn xcp_program_start(&mut self) -> Result<()> {
        self.0.xcp_program_start()
    }

    fn xcp_set_mta(&mut self, adr_ext: u64, address: u64) -> Result<()> {
        self.0.xcp_set_mta(adr_ext, address)
    }

    fn xcpx_program_clear(&mut self, mode: u64, len: u64, timeout: u64) -> Result<()> {
        self.0.xcpx_program_clear(mode, len, timeout)
    }

    fn xcpx_program_memory_file(
        &mut self,
        file: &str,
        p1: u64,
        p2: u64,
        p3: u64,
        p4: u64,
    ) -> Result<()> {
        self.0.xcpx_program_memory_file(file, p1, p2, p3, p4)
    }

    fn xcpx_program_memory(&mut self) -> Result<()> {
        self.0.xcpx_program_memory()
    }

    fn xcp_program_reset(&mut self) -> Result<()> {
        autors_runtime::block_on(self.0.xcp_program_reset())
    }
}
