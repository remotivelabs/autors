//! Asynchronous executor for parsed PRM flash procedures.
//! The executor coordinates UDS, CCP, XCP, CAN access, checksum calculation,
//! variable substitution, progress callbacks, and cooperative cancellation.
//! Transport implementations are injected through traits for deterministic use
//! in both applications and tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use autors_can::device::CanDevice;
use autors_ccp::ccp::{CcpMaster, CmdResult as CcpCmdResult};
use autors_diag::uds::{
    DiagnosticSessionType, DtcSettingType, MsgState, ResetType, RespBase, RespControlDtcSetting,
    RespData, RespIdentifier, RespReset, RespRoutineControl, RespSFBase, RespSession,
    SeedKeyProvider, Sid, UdsClient, UdsResponse, UdsTransport,
};
use autors_formula::formula::Checksum;
use autors_util::helpers::ProgressArgs;
use autors_xcp::xcp::{CmdResult as XcpCmdResult, ConnectMode, XcpMaster};

use crate::error::Result;
use crate::prm::{CnfSegment, Mode, PrmFile};
use crate::prm_if::{prm_error, MsgType, PrmExecutor, PrmMsg, PrmValue as PrmIfValue};

// ============================================================================
// ============================================================================

const MSG_STATE_HEX: &str = "%h";
const MSG_STATE_DEC: &str = "%d";
const PROGRESS_PREFIX: &str = "$+|";
const ERR_MISS_VAR: &str = "GET_VARIABLE: Variable {0} was not set by a previous SET_VARIABLE!";
const ERR_MISS_SK: &str = "Referenced Seed&Key file {0} not found!";
const ERR_FAIL_SK: &str = "Failed to use Seed&Key file.\nTry to switch ASAP2Demo to {0} Bit mode!";

fn sk_fail_msg() -> String {
    let bits = if cfg!(target_pointer_width = "64") {
        "32"
    } else {
        "64"
    };
    ERR_FAIL_SK.replace("{0}", bits)
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdsReply {
    pub state: MsgState,
    pub error_code: u8,
    pub data: Vec<u8>,
}

/// Every method performs bus I/O and is async (object-safe via
/// `async_trait(?Send)`); the futures are not `Send` because `download`
/// holds the progress callback and `unlock` the `&dyn SeedKeyProvider`
/// across await points (the same constraint as the underlying client
/// flows). Synchronous callers use the `blocking` feature facade.
#[async_trait(?Send)]
pub trait UdsOps {
    async fn exec_sf_base(&mut self, service: u8, data: &[u8]) -> autors_diag::Result<UdsReply>;
    async fn exec_routine_control(&mut self, sf: u8, data: &[u8]) -> autors_diag::Result<UdsReply>;
    async fn exec_data(
        &mut self,
        service: u8,
        data: Option<&[u8]>,
    ) -> autors_diag::Result<UdsReply>;
    async fn diagnostic_session_control(&mut self, sf: u8) -> autors_diag::Result<UdsReply>;
    async fn control_dtc_setting(&mut self, sf: u8) -> autors_diag::Result<UdsReply>;
    async fn read_data_by_identifier(
        &mut self,
        identifiers: &[u16],
    ) -> autors_diag::Result<UdsReply>;
    async fn write_data_by_identifier(
        &mut self,
        id: u16,
        data: &[u8],
    ) -> autors_diag::Result<UdsReply>;
    async fn clear_diagnostic_information(
        &mut self,
        group_of_dtc: u32,
    ) -> autors_diag::Result<UdsReply>;
    async fn ecu_reset(&mut self, reset_type: u8) -> autors_diag::Result<UdsReply>;
    async fn download(
        &mut self,
        address: i64,
        data: &[u8],
        adr_and_len_fmt: u8,
        omit_erase_mem: bool,
        progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> autors_diag::Result<u8>;
    async fn unlock(
        &mut self,
        request_seed_sf: u8,
        sk: &dyn SeedKeyProvider,
        variant: Option<&[u8]>,
    ) -> autors_diag::Result<u8>;
}

/// Command methods perform bus I/O and are async (object-safe via
/// `async_trait(?Send)`; `program_sync` holds the progress callback across
/// await points, so the futures are not `Send`). `set_can_ids` is pure
/// configuration and stays synchronous.
#[async_trait(?Send)]
pub trait CcpOps {
    fn set_can_ids(&mut self, cmd_id: u32, rsp_id: u32, station: u16);
    async fn connect(&mut self) -> CcpCmdResult;
    async fn disconnect(&mut self) -> bool;
    async fn diag_service(&mut self, no: u16, add_bytes: Option<&[u8]>) -> CcpCmdResult;
    async fn action_service(&mut self, no: u16, add_bytes: Option<&[u8]>) -> CcpCmdResult;
    async fn set_mta(&mut self, mta_no: u8, address_extension: u8, address: u32) -> CcpCmdResult;
    async fn clear_memory(&mut self, size: u32) -> CcpCmdResult;
    async fn program_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> bool;
    async fn program(&mut self, data: &[u8]) -> CcpCmdResult;
}

/// Both methods perform bus I/O and are async (object-safe via
/// `async_trait(?Send)`).
#[async_trait(?Send)]
pub trait XcpOps {
    async fn connect(&mut self, mode: u8) -> XcpCmdResult;
    async fn program_reset(&mut self);
}

#[async_trait(?Send)]
pub trait CanSend {
    /// Bus I/O, hence async (object-safe via `async_trait(?Send)`).
    async fn send_msg(&mut self, id: u32, data: &[u8]) -> usize;
}

// ============================================================================
// ============================================================================

fn uds_reply<R: UdsResponse>(state: MsgState, resp: Option<R>) -> UdsReply {
    UdsReply {
        state,
        error_code: resp.as_ref().map_or(0, |r| r.base().error_code),
        data: Vec::new(),
    }
}

fn uds_reply_data(state: MsgState, resp: Option<RespData>) -> UdsReply {
    UdsReply {
        state,
        error_code: resp.as_ref().map_or(0, |r| r.base().error_code),
        data: resp.map_or_else(Vec::new, |r| r.data),
    }
}

fn unknown_sid(service: u8) -> autors_diag::Error {
    autors_diag::Error::Protocol(format!("unknown SID 0x{service:02X}"))
}

#[async_trait(?Send)]
impl<T: UdsTransport + Send> UdsOps for UdsClient<T> {
    async fn exec_sf_base(&mut self, service: u8, data: &[u8]) -> autors_diag::Result<UdsReply> {
        let sid = Sid::from_value(service).ok_or_else(|| unknown_sid(service))?;
        let (state, resp) = self.execute_service::<RespSFBase>(sid, Some(data)).await;
        Ok(uds_reply(state, resp))
    }

    async fn exec_routine_control(&mut self, sf: u8, data: &[u8]) -> autors_diag::Result<UdsReply> {
        let (state, resp) = self
            .execute_service_sf::<RespRoutineControl>(Sid::RoutineControl, sf, Some(data), true)
            .await;
        Ok(uds_reply(state, resp))
    }

    async fn exec_data(
        &mut self,
        service: u8,
        data: Option<&[u8]>,
    ) -> autors_diag::Result<UdsReply> {
        let sid = Sid::from_value(service).ok_or_else(|| unknown_sid(service))?;
        let (state, resp) = self.execute_service::<RespData>(sid, data).await;
        Ok(uds_reply_data(state, resp))
    }

    async fn diagnostic_session_control(&mut self, sf: u8) -> autors_diag::Result<UdsReply> {
        let t = DiagnosticSessionType::from_value(sf).ok_or_else(|| {
            autors_diag::Error::Protocol(format!("unknown session type 0x{sf:02X}"))
        })?;
        let (state, resp): (MsgState, Option<RespSession>) =
            self.diagnostic_session_control(t, true).await?;
        Ok(uds_reply(state, resp))
    }

    async fn control_dtc_setting(&mut self, sf: u8) -> autors_diag::Result<UdsReply> {
        let t = DtcSettingType::from_value(sf).ok_or_else(|| {
            autors_diag::Error::Protocol(format!("unknown DTC setting type 0x{sf:02X}"))
        })?;
        let (state, resp): (MsgState, Option<RespControlDtcSetting>) =
            self.control_dtc_setting(t, true).await?;
        Ok(uds_reply(state, resp))
    }

    async fn read_data_by_identifier(
        &mut self,
        identifiers: &[u16],
    ) -> autors_diag::Result<UdsReply> {
        let (state, resp) = self.read_data_by_identifier(identifiers).await?;
        Ok(uds_reply_data(state, resp))
    }

    async fn write_data_by_identifier(
        &mut self,
        id: u16,
        data: &[u8],
    ) -> autors_diag::Result<UdsReply> {
        let (state, resp): (MsgState, Option<RespIdentifier>) =
            self.write_data_by_identifier(id, data).await?;
        Ok(uds_reply(state, resp))
    }

    async fn clear_diagnostic_information(
        &mut self,
        group_of_dtc: u32,
    ) -> autors_diag::Result<UdsReply> {
        let (state, resp): (MsgState, Option<RespBase>) =
            self.clear_diagnostic_information(group_of_dtc).await?;
        Ok(uds_reply(state, resp))
    }

    async fn ecu_reset(&mut self, reset_type: u8) -> autors_diag::Result<UdsReply> {
        let t = ResetType::from_value(reset_type).ok_or_else(|| {
            autors_diag::Error::Protocol(format!("unknown reset type 0x{reset_type:02X}"))
        })?;
        let (state, resp): (MsgState, Option<RespReset>) = self.ecu_reset(t, true).await?;
        Ok(uds_reply(state, resp))
    }

    async fn download(
        &mut self,
        address: i64,
        data: &[u8],
        adr_and_len_fmt: u8,
        omit_erase_mem: bool,
        progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> autors_diag::Result<u8> {
        self.download(address, data, adr_and_len_fmt, omit_erase_mem, progress)
            .await
    }

    async fn unlock(
        &mut self,
        request_seed_sf: u8,
        sk: &dyn SeedKeyProvider,
        variant: Option<&[u8]>,
    ) -> autors_diag::Result<u8> {
        self.unlock(request_seed_sf, sk, variant).await
    }
}

#[async_trait(?Send)]
impl<D: CanDevice + Send> CcpOps for CcpMaster<D> {
    fn set_can_ids(&mut self, cmd_id: u32, rsp_id: u32, station: u16) {
        self.ccp_if.can_id_cmd = cmd_id;
        self.ccp_if.can_id_resp = rsp_id;
        self.ccp_if.station_address = station;
        self.base.prevent_default_requests = true;
    }

    async fn connect(&mut self) -> CcpCmdResult {
        CcpMaster::connect(self).await
    }

    async fn disconnect(&mut self) -> bool {
        CcpMaster::disconnect(self, -1).await
    }

    async fn diag_service(&mut self, no: u16, add_bytes: Option<&[u8]>) -> CcpCmdResult {
        CcpMaster::diag_service(self, no, add_bytes).await.0
    }

    async fn action_service(&mut self, no: u16, add_bytes: Option<&[u8]>) -> CcpCmdResult {
        CcpMaster::action_service(self, no, add_bytes).await.0
    }

    async fn set_mta(&mut self, mta_no: u8, address_extension: u8, address: u32) -> CcpCmdResult {
        CcpMaster::set_mta(self, mta_no, address_extension, address).await
    }

    async fn clear_memory(&mut self, size: u32) -> CcpCmdResult {
        CcpMaster::clear_memory(self, size).await
    }

    async fn program_sync(
        &mut self,
        address_extension: u8,
        address: u32,
        data: &[u8],
        progress: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> bool {
        match progress {
            Some(cb) => {
                let mut adapted = |args: &mut ProgressArgs| {
                    args.cancel = cb(args.percent as u32);
                };
                CcpMaster::program_sync(
                    self,
                    address_extension,
                    address,
                    data,
                    Some(&mut adapted),
                    None,
                )
                .await
            }
            None => {
                CcpMaster::program_sync(self, address_extension, address, data, None, None).await
            }
        }
    }

    async fn program(&mut self, data: &[u8]) -> CcpCmdResult {
        CcpMaster::program(self, data).await.0
    }
}

#[async_trait(?Send)]
impl XcpOps for XcpMaster {
    async fn connect(&mut self, mode: u8) -> XcpCmdResult {
        let mode = match mode {
            0 => ConnectMode::Normal,
            1 => ConnectMode::UserDefined,
            _ => return XcpCmdResult::ERR_INVALID_ARGUMENT,
        };
        self.connect(mode).await.0
    }

    async fn program_reset(&mut self) {
        let _ = self.base.program_reset().await;
    }
}

// ============================================================================
// ============================================================================

pub type MsgHandler = Box<dyn FnMut(PrmMsg)>;

pub type SkFactory = Box<dyn Fn(&Path) -> Result<Box<dyn SeedKeyProvider>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum VarValue {
    Str(String),
    Num(u64),
}

pub struct Executor<'a> {
    prm: &'a PrmFile,
    mode: Mode,
    uds: Option<Box<dyn UdsOps>>,
    ccp: Option<Box<dyn CcpOps>>,
    xcp: Option<Box<dyn XcpOps>>,
    can: Option<Box<dyn CanSend>>,
    sk_factory: Option<SkFactory>,
    msg_handler: Option<MsgHandler>,
    cancel: Arc<AtomicBool>,
    state: u64,
    vars: BTreeMap<u64, VarValue>,
    last_rdbi: Option<Vec<u8>>,
    last_pass_through: Option<Vec<u8>>,
    last_percent: i32,
}

impl<'a> Executor<'a> {
    pub fn new(prm: &'a PrmFile) -> Self {
        Executor {
            prm,
            mode: prm.mode,
            uds: None,
            ccp: None,
            xcp: None,
            can: None,
            sk_factory: None,
            msg_handler: None,
            cancel: Arc::new(AtomicBool::new(false)),
            state: 0,
            vars: BTreeMap::new(),
            last_rdbi: None,
            last_pass_through: None,
            last_percent: 0,
        }
    }

    pub fn with_uds(mut self, client: impl UdsOps + 'static) -> Self {
        self.uds = Some(Box::new(client));
        self
    }

    pub fn with_ccp(mut self, mut client: impl CcpOps + 'static) -> Result<Self> {
        client.set_can_ids(
            self.prm.cnf.cmd_id()?,
            self.prm.cnf.rsp_id()?,
            self.prm.cnf.ecu_address()?,
        );
        self.ccp = Some(Box::new(client));
        Ok(self)
    }

    pub fn with_xcp(mut self, client: impl XcpOps + 'static) -> Self {
        self.xcp = Some(Box::new(client));
        self
    }

    pub fn with_can_sender(mut self, sender: impl CanSend + 'static) -> Self {
        self.can = Some(Box::new(sender));
        self
    }

    pub fn with_seed_key_factory(
        mut self,
        factory: impl Fn(&Path) -> Result<Box<dyn SeedKeyProvider>> + 'static,
    ) -> Self {
        self.sk_factory = Some(Box::new(factory));
        self
    }

    pub fn on_message(mut self, handler: impl FnMut(PrmMsg) + 'static) -> Self {
        self.msg_handler = Some(Box::new(handler));
        self
    }

    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    fn has_comm(&self) -> bool {
        self.uds.is_some() || self.ccp.is_some() || self.xcp.is_some() || self.can.is_some()
    }
}

fn fire_message(
    handler: &mut Option<MsgHandler>,
    state: &mut u64,
    cancelled: bool,
    msg: &str,
    par: u64,
) {
    if cancelled {
        return;
    }
    let Some(h) = handler.as_mut() else {
        return;
    };
    let add_lf = (par & 2) == 0;
    let text = if msg == MSG_STATE_HEX {
        format!("{:X}", *state as u8)
    } else if msg == MSG_STATE_DEC {
        format!("{state}")
    } else {
        msg.to_owned()
    };
    h(PrmMsg::new(text, MsgType::Message, add_lf));
    *state = par & 1;
}

fn make_progress<'a>(
    handler: &'a mut Option<MsgHandler>,
    state: &'a mut u64,
    last_percent: &'a mut i32,
    cancel: &'a AtomicBool,
) -> impl FnMut(u32) -> bool + 'a {
    move |percent: u32| {
        let cancelled = cancel.load(Ordering::Relaxed);
        let p = percent as i32;
        if p != *last_percent {
            *last_percent = p;
            fire_message(
                handler,
                state,
                cancelled,
                &format!("{PROGRESS_PREFIX}{percent}"),
                2,
            );
        }
        cancelled
    }
}

fn parse_hex_bytes(s: &str) -> Result<Vec<u8>> {
    let cleaned = s.to_lowercase().replace("0x", "");
    cleaned
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(|t| {
            u8::from_str_radix(t, 16).map_err(|e| prm_error(format!("invalid hex byte '{t}': {e}")))
        })
        .collect()
}

fn find_segment(segs: &[CnfSegment], idx: u64) -> Result<&CnfSegment> {
    segs.iter()
        .find(|s| u64::from(s.index) == idx)
        .ok_or_else(|| prm_error(format!("CNF segment index {idx} not found")))
}

fn segment_not_loaded(seg: &CnfSegment) -> crate::error::Error {
    prm_error(format!(
        "CNF segment {} data not loaded (call load_segment_data first)",
        seg.index
    ))
}

fn service_state(r: &UdsReply) -> u64 {
    u64::from(!(r.state == MsgState::Success && r.error_code == 0))
}

fn ccp_state(r: CcpCmdResult) -> u64 {
    u64::from(r != CcpCmdResult::OK)
}

fn xcp_state(r: XcpCmdResult) -> u64 {
    u64::from(r != XcpCmdResult::OK)
}

#[async_trait(?Send)]
impl PrmExecutor for Executor<'_> {
    fn state(&self) -> u64 {
        self.state
    }

    fn set_state(&mut self, state: u64) {
        self.state = state;
    }

    fn call(&mut self, _procedure: &str) -> Result<u64> {
        Err(prm_error(
            "CALL: invalid operation (handled by the script interpreter)",
        ))
    }

    fn set_re_entry(&mut self, _step: &str) -> Result<()> {
        Err(prm_error(
            "SET_RE_ENTRY: invalid operation (handled by the script interpreter)",
        ))
    }

    async fn wait(&mut self, time_in_ms: u64) -> Result<()> {
        if !self.is_cancelled() {
            autors_runtime::sleep(Duration::from_millis(time_in_ms)).await;
        }
        Ok(())
    }

    fn display_message(&mut self, msg: &str, par: u64) -> Result<()> {
        let cancelled = self.is_cancelled();
        fire_message(&mut self.msg_handler, &mut self.state, cancelled, msg, par);
        Ok(())
    }

    fn display_error_message(&mut self, _par: u64) -> Result<()> {
        Ok(())
    }

    fn default_screen_layout(&mut self, _value: u64) -> Result<()> {
        Ok(())
    }

    fn extended_message(&mut self, _par: u64) -> Result<()> {
        Ok(())
    }

    fn set_debug_level(&mut self, _level: u64) -> Result<()> {
        Ok(())
    }

    fn set_variable_str(&mut self, variable: u64, action: &str) -> Result<()> {
        if !self.is_cancelled() {
            self.vars.insert(variable, VarValue::Str(action.to_owned()));
            self.state = variable;
        }
        Ok(())
    }

    fn set_variable_num(&mut self, variable: u64, action: u64) -> Result<()> {
        if !self.is_cancelled() {
            self.vars.insert(variable, VarValue::Num(action));
        }
        Ok(())
    }

    fn get_variable(&mut self, variable: u64) -> Result<()> {
        if !self.is_cancelled() {
            match self.vars.get(&variable) {
                Some(VarValue::Num(n)) => self.state = *n,
                Some(VarValue::Str(_)) => self.state = 1,
                None => {
                    return Err(prm_error(
                        ERR_MISS_VAR.replace("{0}", &variable.to_string()),
                    ))
                }
            }
        }
        Ok(())
    }

    fn show_programming_info(&mut self, _step: u64, _bin_file: &str, _p2: u64) -> Result<()> {
        if !self.is_cancelled() {
            self.state = 1;
        }
        Ok(())
    }

    fn run_dll(&mut self, _file: &str, _args: &[PrmIfValue]) -> Result<()> {
        self.state = 1;
        Ok(())
    }

    fn init_flash_programming(
        &mut self,
        _ecu_address: u64,
        _flash_type: i64,
        _cnf: &str,
    ) -> Result<()> {
        self.state = 1;
        Ok(())
    }

    async fn can_send_message(&mut self, id: u64, data: &str) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        let Some(can) = self.can.as_mut() else {
            self.state = 0;
            return Ok(());
        };
        let bytes = parse_hex_bytes(data)?;
        let sent = can.send_msg(id as u32, &bytes).await;
        self.state = u64::from(sent != bytes.len());
        if id == u64::from(self.prm.cnf.cmd_id()?) {
            self.wait(1000).await?;
        }
        Ok(())
    }

    fn udsb_init_communication(&mut self) -> Result<()> {
        self.state = 0;
        Ok(())
    }

    fn udsb_msg_ret_get_at(&mut self, idx: u64, default: u8) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if self.has_comm() {
            if idx == 0 {
                return Err(prm_error("UDSB_MSG_RET_GET_AT: index out of range"));
            }
            self.state = match &self.last_pass_through {
                Some(d) if d.len() as u64 > idx - 1 => u64::from(d[(idx - 1) as usize]),
                _ => u64::from(default),
            };
        }
        Ok(())
    }

    async fn uds_communication_control(
        &mut self,
        control_type: u64,
        communication_type: u64,
    ) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(uds) = self.uds.as_mut() {
            let data = [control_type as u8, communication_type as u8];
            let r = uds
                .exec_sf_base(Sid::CommunicationControl.as_value(), &data)
                .await
                .map_err(|e| prm_error(e.to_string()))?;
            self.state = service_state(&r);
        }
        Ok(())
    }

    async fn uds_diagnostic_session_control(&mut self, sf: u64) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(uds) = self.uds.as_mut() {
            let r = uds
                .diagnostic_session_control(sf as u8)
                .await
                .map_err(|e| prm_error(e.to_string()))?;
            self.state = service_state(&r);
        }
        Ok(())
    }

    async fn uds_control_dtc_setting(&mut self, dtc_off: u64, _msg: &str) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(uds) = self.uds.as_mut() {
            let r = uds
                .control_dtc_setting(dtc_off as u8)
                .await
                .map_err(|e| prm_error(e.to_string()))?;
            self.state = service_state(&r);
        }
        Ok(())
    }

    async fn uds_read_data_by_identifier(&mut self, hex_value: &str) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(uds) = self.uds.as_mut() {
            let hex = hex_value.get(2..).ok_or_else(|| {
                prm_error(format!(
                    "UDS_READ_DATA_BY_IDENTIFIER: bad identifier '{hex_value}'"
                ))
            })?;
            let id = u16::from_str_radix(hex.trim(), 16).map_err(|e| {
                prm_error(format!("UDS_READ_DATA_BY_IDENTIFIER: '{hex_value}': {e}"))
            })?;
            let r = uds
                .read_data_by_identifier(&[id])
                .await
                .map_err(|e| prm_error(e.to_string()))?;
            self.last_rdbi = Some(r.data.clone());
            self.state = service_state(&r);
        }
        Ok(())
    }

    async fn udsx_read_data_by_identifier_scaling(
        &mut self,
        msg: &str,
        _unknown: &str,
        hex_id: &str,
        _hex_idx: &str,
    ) -> Result<()> {
        if !self.is_cancelled() {
            self.display_message(msg, 0)?;
            self.uds_read_data_by_identifier(hex_id).await?;
            self.state = 0;
        }
        Ok(())
    }

    fn uds_read_data_by_identifier_get_data_rec_at(&mut self, idx: u64) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if self.has_comm() {
            if idx == 0 {
                return Err(prm_error(
                    "UDS_READ_DATA_BY_IDENTIFIER_GET_DATA_REC_AT: index out of range",
                ));
            }
            let guard_len = self.last_rdbi.as_ref().map_or(0, Vec::len) as u64;
            self.state = if guard_len > idx - 1 {
                match &self.last_pass_through {
                    Some(d) if d.len() as u64 > idx - 1 => u64::from(d[(idx - 1) as usize]),
                    _ => {
                        return Err(prm_error(
                            "UDS_READ_DATA_BY_IDENTIFIER_GET_DATA_REC_AT: pass-through response missing or short",
                        ));
                    }
                }
            } else {
                u64::from(u8::MAX)
            };
        }
        Ok(())
    }

    async fn uds_write_data_by_identifier(&mut self, id: u64, hex_data: &str) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(uds) = self.uds.as_mut() {
            let data = parse_hex_bytes(hex_data)?;
            let r = uds
                .write_data_by_identifier(id as u16, &data)
                .await
                .map_err(|e| prm_error(e.to_string()))?;
            self.state = service_state(&r);
        }
        Ok(())
    }

    async fn uds_clear_dtc_information(&mut self, _p: u64) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(uds) = self.uds.as_mut() {
            let r = uds
                .clear_diagnostic_information(0x00FF_FFFF)
                .await
                .map_err(|e| prm_error(e.to_string()))?;
            self.state = service_state(&r);
        }
        Ok(())
    }

    async fn uds_routine_control(&mut self, sf: u64, routine: u64, data_bytes: &str) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(uds) = self.uds.as_mut() {
            let mut data = vec![sf as u8];
            data.extend_from_slice(&(routine as u16).to_be_bytes());
            data.extend_from_slice(&parse_hex_bytes(data_bytes)?);
            let r = uds
                .exec_routine_control(sf as u8, &data)
                .await
                .map_err(|e| prm_error(e.to_string()))?;
            self.state = service_state(&r);
        }
        Ok(())
    }

    async fn uds_pass_through(&mut self, hex_data: &str) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(uds) = self.uds.as_mut() {
            let bytes = parse_hex_bytes(hex_data)?;
            let (&sid, rest) = bytes
                .split_first()
                .ok_or_else(|| prm_error("UDS_PASS_THROUGH: empty data"))?;
            let data = if rest.is_empty() { None } else { Some(rest) };
            let r = uds
                .exec_data(sid, data)
                .await
                .map_err(|e| prm_error(e.to_string()))?;
            self.last_pass_through = Some(r.data.clone());
            self.state = service_state(&r);
        }
        Ok(())
    }

    async fn uds_ecu_reset(&mut self, reset_type: u64) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(uds) = self.uds.as_mut() {
            let r = uds
                .ecu_reset(reset_type as u8)
                .await
                .map_err(|e| prm_error(e.to_string()))?;
            self.state = service_state(&r);
        }
        Ok(())
    }

    async fn udsx_security_access(&mut self, sf: u64, variant: u64, sk_file: &str) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if self.uds.is_none() {
            return Ok(());
        }
        let prm = self.prm;
        let Executor {
            uds,
            sk_factory,
            state,
            ..
        } = self;
        let dir = Path::new(&prm.source_file)
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let file_name = Path::new(sk_file)
            .file_name()
            .ok_or_else(|| prm_error(format!("invalid seed&key file name: {sk_file}")))?;
        let path = dir.join(file_name);
        if !path.exists() {
            return Err(prm_error(
                ERR_MISS_SK.replace("{0}", &path.display().to_string()),
            ));
        }
        let Some(factory) = sk_factory else {
            return Err(prm_error(sk_fail_msg()));
        };
        let sk = factory(&path).map_err(|_| prm_error(sk_fail_msg()))?;
        let code = uds
            .as_deref_mut()
            .expect("checked")
            .unlock(sf as u8, sk.as_ref(), Some(&[variant as u8]))
            .await
            .map_err(|_| prm_error(sk_fail_msg()))?;
        *state = u64::from(code);
        Ok(())
    }

    async fn udsx_verify_memory(
        &mut self,
        _file: &str,
        src_seg_idx: u64,
        dst_seg_idx: u64,
        _max_block_len: u64,
        _checksum: u64,
        _timeout_msec: u64,
    ) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        let Some(uds) = self.uds.as_mut() else {
            return Ok(());
        };
        let prm = self.prm;
        let src = find_segment(&prm.cnf.sections.source_mem_areas, src_seg_idx)?;
        let data = src.data.as_deref().ok_or_else(|| segment_not_loaded(src))?;
        let crc = Checksum::crc32(data, 0, src.byte_len() as usize)
            .map_err(|e| prm_error(e.to_string()))?;
        let dst = find_segment(&prm.cnf.sections.dest_mem_areas, dst_seg_idx)?;
        let mut payload = vec![0xF0, 0x01, 0x01];
        payload.extend_from_slice(&dst.start.to_be_bytes());
        payload.extend_from_slice(&dst.end.to_be_bytes());
        payload.extend_from_slice(&crc.to_be_bytes());
        let r = uds
            .exec_sf_base(Sid::RoutineControl.as_value(), &payload)
            .await
            .map_err(|e| prm_error(e.to_string()))?;
        self.state = service_state(&r);
        Ok(())
    }

    /// `download(dst.start, src.data, FmtIdentifier, progress, omitEraseMem: true)`).
    async fn udsx_program_memory(
        &mut self,
        _file: &str,
        src_seg_idx: u64,
        dst_seg_idx: u64,
        _p1: u64,
        _p2: &str,
    ) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if self.uds.is_none() {
            return Ok(());
        }
        let prm = self.prm;
        let src = find_segment(&prm.cnf.sections.source_mem_areas, src_seg_idx)?;
        let dst = find_segment(&prm.cnf.sections.dest_mem_areas, dst_seg_idx)?;
        let data = src.data.as_deref().ok_or_else(|| segment_not_loaded(src))?;
        let fmt = prm.cnf.fmt_identifier()?;
        let Executor {
            uds,
            msg_handler,
            state,
            last_percent,
            cancel,
            ..
        } = self;
        let mut progress = make_progress(msg_handler, state, last_percent, cancel);
        let code = uds
            .as_deref_mut()
            .expect("checked")
            .download(i64::from(dst.start), data, fmt, true, Some(&mut progress))
            .await
            .map_err(|e| prm_error(e.to_string()))?;
        drop(progress);
        *state = u64::from(code);
        Ok(())
    }

    fn check_inca_configuration(&mut self) -> Result<()> {
        Ok(())
    }

    async fn ccp_disconnect(&mut self, _permanent: u64) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(ccp) = self.ccp.as_mut() {
            self.state = u64::from(!ccp.disconnect().await);
        }
        Ok(())
    }

    fn ccpb_store_ccp_cmd_timeout(&mut self, _cmd: u64, _timeout: u64) -> Result<()> {
        Ok(())
    }

    fn ccpb_set_canids(&mut self, cmd_id: u64, rsp_id: u64, station_addr: u64) -> Result<()> {
        if !self.is_cancelled() {
            if let Some(ccp) = self.ccp.as_mut() {
                let station = if station_addr != 0 {
                    station_addr as u16
                } else {
                    self.prm.cnf.ecu_address()?
                };
                ccp.set_can_ids(cmd_id as u32, rsp_id as u32, station);
            }
        }
        Ok(())
    }

    async fn ccpx_start_ecu_communication(&mut self) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(ccp) = self.ccp.as_mut() {
            self.state = ccp_state(ccp.connect().await);
        }
        Ok(())
    }

    async fn ccpx_diag_service(&mut self, diag: u64, p: &[u64]) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(ccp) = self.ccp.as_mut() {
            let add: Option<Vec<u8>> = if p.is_empty() {
                None
            } else {
                Some(p.iter().take(4).map(|&v| v as u8).collect())
            };
            self.state = ccp_state(ccp.diag_service(diag as u16, add.as_deref()).await);
        }
        Ok(())
    }

    async fn ccpx_action_service(&mut self, action: u64, p: &[u64]) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(ccp) = self.ccp.as_mut() {
            let add: Option<Vec<u8>> = if p.is_empty() {
                None
            } else {
                Some(p.iter().take(4).map(|&v| v as u8).collect())
            };
            self.state = ccp_state(ccp.action_service(action as u16, add.as_deref()).await);
        }
        Ok(())
    }

    async fn ccpx_erase_memory(&mut self, seg_idx: u64, _timeout: u64) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if self.ccp.is_none() {
            return Ok(());
        }
        let seg = find_segment(&self.prm.cnf.sections.dest_mem_areas, seg_idx)?;
        let ccp = self.ccp.as_deref_mut().expect("checked");
        self.state = ccp_state(ccp.set_mta(0, 0, seg.start).await);
        if self.state == 0 {
            self.state = ccp_state(ccp.clear_memory(seg.byte_len()).await);
        }
        Ok(())
    }

    async fn ccpx_program_memory(
        &mut self,
        _data_file: &str,
        src_seg_idx: u64,
        dst_seg_idx: u64,
        ccp_program_zero: u64,
        _timeout: u64,
    ) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if self.ccp.is_none() {
            return Ok(());
        }
        let prm = self.prm;
        let src = find_segment(&prm.cnf.sections.source_mem_areas, src_seg_idx)?;
        let dst = find_segment(&prm.cnf.sections.dest_mem_areas, dst_seg_idx)?;
        let data = src.data.as_deref().ok_or_else(|| segment_not_loaded(src))?;
        let Executor {
            ccp,
            msg_handler,
            state,
            last_percent,
            cancel,
            ..
        } = self;
        let mut progress = make_progress(msg_handler, state, last_percent, cancel);
        let ok = ccp
            .as_deref_mut()
            .expect("checked")
            .program_sync(0, dst.start, data, Some(&mut progress))
            .await;
        drop(progress);
        *state = u64::from(!ok);
        if ccp_program_zero != 0 {
            *state = ccp_state(ccp.as_deref_mut().expect("checked").program(&[]).await);
        }
        Ok(())
    }

    async fn xcp_connect(&mut self, mode: u64) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        self.state = 0;
        if let Some(xcp) = self.xcp.as_mut() {
            self.state = xcp_state(xcp.connect(mode as u8).await);
        }
        Ok(())
    }

    fn xcp_program_start(&mut self) -> Result<()> {
        if !self.is_cancelled() {
            self.state = 0;
        }
        Ok(())
    }

    fn xcp_set_mta(&mut self, _adr_ext: u64, _address: u64) -> Result<()> {
        if !self.is_cancelled() {
            self.state = 0;
        }
        Ok(())
    }

    fn xcpx_program_clear(&mut self, _mode: u64, _len: u64, _timeout: u64) -> Result<()> {
        if !self.is_cancelled() {
            self.state = 0;
        }
        Ok(())
    }

    fn xcpx_program_memory_file(
        &mut self,
        _file: &str,
        _p1: u64,
        _p2: u64,
        _p3: u64,
        _p4: u64,
    ) -> Result<()> {
        if !self.is_cancelled() {
            self.state = 0;
        }
        Ok(())
    }

    fn xcpx_program_memory(&mut self) -> Result<()> {
        if !self.is_cancelled() {
            self.state = 0;
        }
        Ok(())
    }

    async fn xcp_program_reset(&mut self) -> Result<()> {
        if !self.is_cancelled() {
            self.state = 0;
            if let Some(xcp) = self.xcp.as_mut() {
                xcp.program_reset().await;
            }
        }
        Ok(())
    }
}

// ============================================================================
// ============================================================================

#[cfg(all(test, feature = "blocking"))]
mod tests {
    use super::*;
    use crate::blocking::{BlockingExecutor, BlockingPrmExecutor};
    use autors_datafile::datafile::{DataFile, DataFileType, MemorySegmentList};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    type Log = Rc<RefCell<Vec<String>>>;

    fn log() -> Log {
        Rc::new(RefCell::new(Vec::new()))
    }

    fn reply(state: MsgState, error_code: u8, data: &[u8]) -> UdsReply {
        UdsReply {
            state,
            error_code,
            data: data.to_vec(),
        }
    }

    struct MockUds {
        log: Log,
        replies: VecDeque<UdsReply>,
        download_code: u8,
        unlock_code: u8,
        progress_steps: Vec<u32>,
        fail: bool,
    }

    impl MockUds {
        fn new(log: Log) -> Self {
            MockUds {
                log,
                replies: VecDeque::new(),
                download_code: 0,
                unlock_code: 0,
                progress_steps: Vec::new(),
                fail: false,
            }
        }

        fn push(&mut self, s: String) {
            self.log.borrow_mut().push(s);
        }

        fn next_reply(&mut self) -> autors_diag::Result<UdsReply> {
            if self.fail {
                return Err(autors_diag::Error::Protocol("boom".to_owned()));
            }
            Ok(self
                .replies
                .pop_front()
                .unwrap_or_else(|| reply(MsgState::Success, 0, &[])))
        }
    }

    #[async_trait::async_trait(?Send)]
    impl UdsOps for MockUds {
        async fn exec_sf_base(
            &mut self,
            service: u8,
            data: &[u8],
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("sf_base sid={service:02X} data={data:02X?}"));
            self.next_reply()
        }
        async fn exec_routine_control(
            &mut self,
            sf: u8,
            data: &[u8],
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("routine sf={sf:02X} data={data:02X?}"));
            self.next_reply()
        }
        async fn exec_data(
            &mut self,
            service: u8,
            data: Option<&[u8]>,
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("data sid={service:02X} data={data:02X?}"));
            self.next_reply()
        }
        async fn diagnostic_session_control(&mut self, sf: u8) -> autors_diag::Result<UdsReply> {
            self.push(format!("session {sf:02X}"));
            self.next_reply()
        }
        async fn control_dtc_setting(&mut self, sf: u8) -> autors_diag::Result<UdsReply> {
            self.push(format!("dtc {sf:02X}"));
            self.next_reply()
        }
        async fn read_data_by_identifier(
            &mut self,
            identifiers: &[u16],
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("rdbi {identifiers:04X?}"));
            self.next_reply()
        }
        async fn write_data_by_identifier(
            &mut self,
            id: u16,
            data: &[u8],
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("wdbi {id:04X} {data:02X?}"));
            self.next_reply()
        }
        async fn clear_diagnostic_information(
            &mut self,
            group_of_dtc: u32,
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("clear {group_of_dtc:06X}"));
            self.next_reply()
        }
        async fn ecu_reset(&mut self, reset_type: u8) -> autors_diag::Result<UdsReply> {
            self.push(format!("reset {reset_type:02X}"));
            self.next_reply()
        }
        async fn download(
            &mut self,
            address: i64,
            data: &[u8],
            adr_and_len_fmt: u8,
            omit_erase_mem: bool,
            progress: Option<&mut dyn FnMut(u32) -> bool>,
        ) -> autors_diag::Result<u8> {
            self.push(format!(
                "download {address:X} len={} fmt={adr_and_len_fmt:02X} omit={omit_erase_mem}",
                data.len()
            ));
            if let Some(cb) = progress {
                for &p in &self.progress_steps {
                    if cb(p) {
                        break;
                    }
                }
            }
            Ok(self.download_code)
        }
        async fn unlock(
            &mut self,
            request_seed_sf: u8,
            _sk: &dyn SeedKeyProvider,
            variant: Option<&[u8]>,
        ) -> autors_diag::Result<u8> {
            self.push(format!("unlock {request_seed_sf:02X} {variant:02X?}"));
            Ok(self.unlock_code)
        }
    }

    struct MockCcp {
        log: Log,
        result: CcpCmdResult,
        disconnect_ok: bool,
        program_sync_ok: bool,
        progress_steps: Vec<u32>,
    }

    impl MockCcp {
        fn new(log: Log) -> Self {
            MockCcp {
                log,
                result: CcpCmdResult::OK,
                disconnect_ok: true,
                program_sync_ok: true,
                progress_steps: Vec::new(),
            }
        }

        fn push(&mut self, s: String) {
            self.log.borrow_mut().push(s);
        }
    }

    #[async_trait::async_trait(?Send)]
    impl CcpOps for MockCcp {
        fn set_can_ids(&mut self, cmd_id: u32, rsp_id: u32, station: u16) {
            self.push(format!("canids {cmd_id:X}/{rsp_id:X}/{station:X}"));
        }
        async fn connect(&mut self) -> CcpCmdResult {
            self.push("connect".to_owned());
            self.result
        }
        async fn disconnect(&mut self) -> bool {
            self.push("disconnect".to_owned());
            self.disconnect_ok
        }
        async fn diag_service(&mut self, no: u16, add_bytes: Option<&[u8]>) -> CcpCmdResult {
            self.push(format!("diag {no:04X} {add_bytes:02X?}"));
            self.result
        }
        async fn action_service(&mut self, no: u16, add_bytes: Option<&[u8]>) -> CcpCmdResult {
            self.push(format!("action {no:04X} {add_bytes:02X?}"));
            self.result
        }
        async fn set_mta(
            &mut self,
            mta_no: u8,
            address_extension: u8,
            address: u32,
        ) -> CcpCmdResult {
            self.push(format!("mta {mta_no}/{address_extension:02X}/{address:X}"));
            self.result
        }
        async fn clear_memory(&mut self, size: u32) -> CcpCmdResult {
            self.push(format!("clear {size}"));
            self.result
        }
        async fn program_sync(
            &mut self,
            address_extension: u8,
            address: u32,
            data: &[u8],
            progress: Option<&mut dyn FnMut(u32) -> bool>,
        ) -> bool {
            self.push(format!(
                "program_sync {address_extension}/{address:X} len={}",
                data.len()
            ));
            if let Some(cb) = progress {
                for &p in &self.progress_steps {
                    if cb(p) {
                        break;
                    }
                }
            }
            self.program_sync_ok
        }
        async fn program(&mut self, data: &[u8]) -> CcpCmdResult {
            self.push(format!("program len={}", data.len()));
            self.result
        }
    }

    struct MockXcp {
        log: Log,
    }

    #[async_trait::async_trait(?Send)]
    impl XcpOps for MockXcp {
        async fn connect(&mut self, mode: u8) -> XcpCmdResult {
            self.log.borrow_mut().push(format!("connect {mode}"));
            XcpCmdResult::OK
        }
        async fn program_reset(&mut self) {
            self.log.borrow_mut().push("reset".to_owned());
        }
    }

    struct MockCan {
        log: Log,
        short: usize,
    }

    #[async_trait::async_trait(?Send)]
    impl CanSend for MockCan {
        async fn send_msg(&mut self, id: u32, data: &[u8]) -> usize {
            self.log
                .borrow_mut()
                .push(format!("can {id:X} {data:02X?}"));
            data.len() - self.short
        }
    }

    struct StubSk;

    impl SeedKeyProvider for StubSk {
        fn compute_key_from_seed(
            &self,
            _request_seed_sf: u8,
            _variant: Option<&[u8]>,
            _seed: &[u8],
        ) -> std::result::Result<Vec<u8>, i32> {
            Ok(vec![0x42])
        }
    }

    const EXEC_CNF: &str = "\
PROJECT_NAME:test
ECU_ADDR:0x7C
KWP_CAN_BUS_TIMING:500000
INCA_TO_ECU_CAN_ID:0x700
ECU_TO_INCA_CAN_ID:0x708
ADDRESS_AND_LENGTH_FORMAT_IDENTIFIER:0x44
DATA_FORMAT_IDENTIFIER:0x00
SOURCE_MEM_AREA:1,0,0,0x10000L,0x1000FL
DEST_MEM_AREA:2,0,0,0x20000L,0x2000FL
";

    const EXEC_HEX: &str =
        ":020000040001F9\n:10000000000102030405060708090A0B0C0D0E0F78\n:00000001FF\n";

    fn setup(tag: &str, mode_cmd: &str) -> (PathBuf, PrmFile) {
        let dir =
            std::env::temp_dir().join(format!("autors_prm_exec_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("conf.cnf"), EXEC_CNF).unwrap();
        let prm_src = format!("#define CONFIG conf.cnf\n[S]\n{mode_cmd}\ndefault : S\n[S_END]\n");
        std::fs::write(dir.join("main.prm"), prm_src).unwrap();
        let prm = PrmFile::open(dir.join("main.prm")).unwrap();
        (dir, prm)
    }

    fn setup_uds(tag: &str) -> (PathBuf, PrmFile) {
        setup(tag, "UDSX_PROGRAM_MEMORY(1, 1, 2, 3, \"x\")")
    }

    fn setup_ccp(tag: &str) -> (PathBuf, PrmFile) {
        setup(tag, "CCPX_START_ECU_COMMUNICATION")
    }

    fn setup_xcp(tag: &str) -> (PathBuf, PrmFile) {
        setup(tag, "XCPX_PROGRAM_CLEAR(1, 2, 3)")
    }

    fn load_seg_data(prm: &mut PrmFile) {
        let df = DataFile::parse(
            DataFileType::IntelHex,
            EXEC_HEX.as_bytes(),
            MemorySegmentList::new(),
            0,
            None,
        )
        .unwrap();
        prm.cnf.load_segment_data(&df, 0, "t.hex").unwrap();
    }

    fn cleanup(dir: &Path) {
        std::fs::remove_dir_all(dir).ok();
    }

    fn msgs_sink() -> (
        std::sync::Arc<std::sync::Mutex<Vec<PrmMsg>>>,
        impl FnMut(PrmMsg),
    ) {
        let msgs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<PrmMsg>::new()));
        let m2 = std::sync::Arc::clone(&msgs);
        (msgs, move |m| m2.lock().unwrap().push(m))
    }

    // ---- CAN_SEND_MESSAGE ----

    #[test]
    fn can_send_message_hex_and_state() {
        let (dir, prm) = setup_uds("can");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm).with_can_sender(MockCan {
            log: Rc::clone(&lg),
            short: 0,
        });
        e.can_send_message(0x123, "0xAA bb 0C").unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(*lg.borrow(), vec!["can 123 [AA, BB, 0C]"]);
        let mut e = BlockingExecutor::new(&prm).with_can_sender(MockCan {
            log: log(),
            short: 1,
        });
        e.can_send_message(0x123, "AA BB").unwrap();
        assert_eq!(e.state(), 1);
        let mut e = BlockingExecutor::new(&prm);
        e.can_send_message(0x123, "AA").unwrap();
        assert_eq!(e.state(), 0);
        cleanup(&dir);
    }

    #[test]
    fn can_send_message_cmd_id_waits() {
        let (dir, prm) = setup_uds("canwait");
        let mut e = BlockingExecutor::new(&prm).with_can_sender(MockCan {
            log: log(),
            short: 0,
        });
        let t0 = std::time::Instant::now();
        e.can_send_message(0x700, "AA").unwrap(); // == CNF INCA_TO_ECU_CAN_ID → WAIT(1000)
        assert!(t0.elapsed() >= Duration::from_millis(1000));
        assert_eq!(e.state(), 0);
        cleanup(&dir);
    }

    #[test]
    fn uds_session_and_state_mapping() {
        let (dir, prm) = setup_uds("session");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm).with_uds(MockUds::new(Rc::clone(&lg)));
        e.uds_diagnostic_session_control(2).unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(*lg.borrow(), vec!["session 02"]);
        let mut mock = MockUds::new(log());
        mock.replies.push_back(reply(MsgState::Success, 0x31, &[]));
        let mut e = BlockingExecutor::new(&prm).with_uds(mock);
        e.uds_diagnostic_session_control(2).unwrap();
        assert_eq!(e.state(), 1);
        let mut mock = MockUds::new(log());
        mock.replies.push_back(reply(MsgState::ErrTimeout, 0, &[]));
        let mut e = BlockingExecutor::new(&prm).with_uds(mock);
        e.uds_ecu_reset(1).unwrap();
        assert_eq!(e.state(), 1);
        cleanup(&dir);
    }

    #[test]
    fn uds_communication_control_payload() {
        let (dir, prm) = setup_uds("commctrl");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm).with_uds(MockUds::new(Rc::clone(&lg)));
        e.uds_communication_control(1, 1).unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(*lg.borrow(), vec!["sf_base sid=28 data=[01, 01]"]);
        cleanup(&dir);
    }

    #[test]
    fn uds_routine_control_wire_bytes() {
        let (dir, prm) = setup_uds("routine");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm).with_uds(MockUds::new(Rc::clone(&lg)));
        e.uds_routine_control(1, 0x0203, "0xAA 0xBB").unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(
            *lg.borrow(),
            vec!["routine sf=01 data=[01, 02, 03, AA, BB]"]
        );
        cleanup(&dir);
    }

    #[test]
    fn uds_pass_through_and_msg_ret_get_at() {
        let (dir, prm) = setup_uds("passthrough");
        let lg = log();
        let mut mock = MockUds::new(Rc::clone(&lg));
        mock.replies
            .push_back(reply(MsgState::Success, 0, &[0x11, 0x22, 0x33]));
        let mut e = BlockingExecutor::new(&prm).with_uds(mock);
        e.uds_pass_through("0x22 0xF1 0x90").unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(*lg.borrow(), vec!["data sid=22 data=Some([F1, 90])"]);
        e.udsb_msg_ret_get_at(2, 0xEE).unwrap();
        assert_eq!(e.state(), 0x22);
        e.udsb_msg_ret_get_at(9, 0xEE).unwrap();
        assert_eq!(e.state(), 0xEE);
        assert!(e.udsb_msg_ret_get_at(0, 0xEE).is_err());
        cleanup(&dir);
    }

    #[test]
    fn uds_pass_through_single_byte() {
        let (dir, prm) = setup_uds("passthrough1");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm).with_uds(MockUds::new(Rc::clone(&lg)));
        e.uds_pass_through("0x3E").unwrap();
        assert_eq!(*lg.borrow(), vec!["data sid=3E data=None"]);
        assert!(e.uds_pass_through("").is_err());
        cleanup(&dir);
    }

    #[test]
    fn uds_rdbi_and_get_data_rec_at_quirk() {
        let (dir, prm) = setup_uds("rdbi");
        let lg = log();
        let mut mock = MockUds::new(Rc::clone(&lg));
        mock.replies
            .push_back(reply(MsgState::Success, 0, &[0xAA, 0xBB, 0xCC]));
        mock.replies
            .push_back(reply(MsgState::Success, 0, &[0x44, 0x55]));
        let mut e = BlockingExecutor::new(&prm).with_uds(mock);
        e.uds_read_data_by_identifier("0xF190").unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!((*lg.borrow())[0], "rdbi [F190]");
        e.uds_pass_through("0x2E 0xF1").unwrap();
        e.uds_read_data_by_identifier_get_data_rec_at(2).unwrap();
        assert_eq!(e.state(), 0x55);
        e.uds_read_data_by_identifier_get_data_rec_at(5).unwrap();
        assert_eq!(e.state(), 0xFF);
        cleanup(&dir);
    }

    #[test]
    fn uds_rdbi_get_data_rec_at_missing_pass_through() {
        let (dir, prm) = setup_uds("rdbi_err");
        let mut mock = MockUds::new(log());
        mock.replies
            .push_back(reply(MsgState::Success, 0, &[0xAA, 0xBB]));
        let mut e = BlockingExecutor::new(&prm).with_uds(mock);
        e.uds_read_data_by_identifier("0xF190").unwrap();
        assert!(e.uds_read_data_by_identifier_get_data_rec_at(1).is_err());
        cleanup(&dir);
    }

    #[test]
    fn uds_write_clear_reset_calls() {
        let (dir, prm) = setup_uds("misc");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm).with_uds(MockUds::new(Rc::clone(&lg)));
        e.uds_write_data_by_identifier(0xF190, "01 02").unwrap();
        assert_eq!(e.state(), 0);
        e.uds_clear_dtc_information(0x1234).unwrap();
        e.uds_ecu_reset(1).unwrap();
        e.uds_control_dtc_setting(2, "off").unwrap();
        assert_eq!(
            *lg.borrow(),
            vec!["wdbi F190 [01, 02]", "clear FFFFFF", "reset 01", "dtc 02"]
        );
        cleanup(&dir);
    }

    #[test]
    fn uds_error_propagation() {
        let (dir, prm) = setup_uds("udserr");
        let mut mock = MockUds::new(log());
        mock.fail = true;
        let mut e = BlockingExecutor::new(&prm).with_uds(mock);
        let err = e.uds_diagnostic_session_control(2).unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
        cleanup(&dir);
    }

    #[test]
    fn udsx_verify_memory_payload() {
        let (dir, mut prm) = setup_uds("verify");
        load_seg_data(&mut prm);
        let lg = log();
        let mut e = BlockingExecutor::new(&prm).with_uds(MockUds::new(Rc::clone(&lg)));
        e.udsx_verify_memory("f.hex", 1, 2, 0, 0, 0).unwrap();
        assert_eq!(e.state(), 0);
        let data: Vec<u8> = (0x00..=0x0F).collect();
        let crc = Checksum::crc32(&data, 0, 16).unwrap();
        let got = &(*lg.borrow())[0];
        assert!(
            got.starts_with("sf_base sid=31 data=[F0, 01, 01, 00, 02, 00, 00, 00, 02, 00, 0F, "),
            "{got}"
        );
        for b in crc.to_be_bytes() {
            assert!(got.contains(&format!("{b:02X}")), "{got}");
        }
        cleanup(&dir);
    }

    #[test]
    fn udsx_verify_memory_errors() {
        let (dir2, prm2) = setup_uds("verify_no_data");
        let mut e = BlockingExecutor::new(&prm2).with_uds(MockUds::new(log()));
        let err = e.udsx_verify_memory("f.hex", 1, 2, 0, 0, 0).unwrap_err();
        assert!(err.to_string().contains("data not loaded"), "{err}");
        let (dir, mut prm) = setup_uds("verify_bad_idx");
        load_seg_data(&mut prm);
        let mut e = BlockingExecutor::new(&prm).with_uds(MockUds::new(log()));
        let err = e.udsx_verify_memory("f.hex", 9, 2, 0, 0, 0).unwrap_err();
        assert!(err.to_string().contains("index 9 not found"), "{err}");
        cleanup(&dir);
        cleanup(&dir2);
    }

    #[test]
    fn udsx_program_memory_download_and_progress() {
        let (dir, mut prm) = setup_uds("program");
        load_seg_data(&mut prm);
        let lg = log();
        let mut mock = MockUds::new(Rc::clone(&lg));
        mock.progress_steps = vec![10, 10, 50];
        let (msgs, sink) = msgs_sink();
        let mut e = BlockingExecutor::new(&prm).with_uds(mock).on_message(sink);
        e.udsx_program_memory("f.hex", 1, 2, 0, "").unwrap();
        assert_eq!(e.state(), 0);
        // download(dst.start, src.data, FmtIdentifier, omitEraseMem: true)
        assert_eq!(*lg.borrow(), vec!["download 20000 len=16 fmt=44 omit=true"]);
        let msgs = msgs.lock().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].msg, "$+|10");
        assert_eq!(msgs[1].msg, "$+|50");
        assert_eq!(msgs[0].msg_type, MsgType::Message);
        drop(msgs);
        let mut mock = MockUds::new(log());
        mock.download_code = 0x22;
        let mut e = BlockingExecutor::new(&prm).with_uds(mock);
        e.udsx_program_memory("f.hex", 1, 2, 0, "").unwrap();
        assert_eq!(e.state(), 0x22);
        cleanup(&dir);
    }

    #[test]
    fn udsx_security_access_paths() {
        let (dir, prm) = setup_uds("sk");
        let mut e = BlockingExecutor::new(&prm).with_uds(MockUds::new(log()));
        let err = e.udsx_security_access(3, 1, "sk.dll").unwrap_err();
        assert!(
            err.to_string().contains("Referenced Seed&Key file"),
            "{err}"
        );
        assert!(err.to_string().contains("not found!"), "{err}");
        std::fs::write(dir.join("sk.dll"), b"stub").unwrap();
        let mut e = BlockingExecutor::new(&prm).with_uds(MockUds::new(log()));
        let err = e.udsx_security_access(3, 1, "sk.dll").unwrap_err();
        assert!(
            err.to_string().contains("Failed to use Seed&Key file"),
            "{err}"
        );
        let lg = log();
        let mut e = BlockingExecutor::new(&prm)
            .with_uds(MockUds::new(Rc::clone(&lg)))
            .with_seed_key_factory(|path| {
                assert!(path.ends_with("sk.dll"));
                Ok(Box::new(StubSk))
            });
        e.udsx_security_access(3, 1, "sk.dll").unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(*lg.borrow(), vec!["unlock 03 Some([01])"]);
        cleanup(&dir);
    }

    #[test]
    fn ccp_ctor_and_connect() {
        let (dir, prm) = setup_ccp("ccpconnect");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm)
            .with_ccp(MockCcp::new(Rc::clone(&lg)))
            .unwrap();
        assert_eq!(e.mode(), Mode::Ccp);
        assert_eq!(*lg.borrow(), vec!["canids 700/708/7C"]);
        e.ccpx_start_ecu_communication().unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(*lg.borrow(), vec!["canids 700/708/7C", "connect"]);
        let mut mock = MockCcp::new(log());
        mock.result = CcpCmdResult::Timeout;
        let mut e = BlockingExecutor::new(&prm).with_ccp(mock).unwrap();
        e.ccpx_start_ecu_communication().unwrap();
        assert_eq!(e.state(), 1);
        cleanup(&dir);
    }

    #[test]
    fn ccp_disconnect_and_services() {
        let (dir, prm) = setup_ccp("ccpsvc");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm)
            .with_ccp(MockCcp::new(Rc::clone(&lg)))
            .unwrap();
        e.ccp_disconnect(0).unwrap();
        assert_eq!(e.state(), 0);
        let mut mock = MockCcp::new(log());
        mock.disconnect_ok = false;
        let mut e = BlockingExecutor::new(&prm).with_ccp(mock).unwrap();
        e.ccp_disconnect(1).unwrap();
        assert_eq!(e.state(), 1);
        let lg2 = log();
        let mut e = BlockingExecutor::new(&prm)
            .with_ccp(MockCcp::new(Rc::clone(&lg2)))
            .unwrap();
        e.ccpx_diag_service(0x20, &[1, 2, 3, 4, 5]).unwrap();
        assert_eq!(e.state(), 0);
        e.ccpx_action_service(0x21, &[]).unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(
            lg2.borrow()[1..],
            vec!["diag 0020 Some([01, 02, 03, 04])", "action 0021 None"]
        );
        assert_eq!(*lg.borrow(), vec!["canids 700/708/7C", "disconnect"]);
        cleanup(&dir);
    }

    #[test]
    fn ccp_erase_uses_dest_list() {
        let (dir, prm) = setup_ccp("ccperase");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm)
            .with_ccp(MockCcp::new(Rc::clone(&lg)))
            .unwrap();
        e.ccpx_erase_memory(2, 1000).unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(lg.borrow()[1..], vec!["mta 0/00/20000", "clear 16"]);
        let mut e = BlockingExecutor::new(&prm)
            .with_ccp(MockCcp::new(log()))
            .unwrap();
        assert!(e.ccpx_erase_memory(7, 1000).is_err());
        cleanup(&dir);
    }

    #[test]
    fn ccp_program_memory_flow() {
        let (dir, mut prm) = setup_ccp("ccpprog");
        load_seg_data(&mut prm);
        let lg = log();
        let mut mock = MockCcp::new(Rc::clone(&lg));
        mock.progress_steps = vec![25, 100];
        let (msgs, sink) = msgs_sink();
        let mut e = BlockingExecutor::new(&prm)
            .with_ccp(mock)
            .unwrap()
            .on_message(sink);
        e.ccpx_program_memory("f.hex", 1, 2, 0, 0).unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(lg.borrow()[1..], vec!["program_sync 0/20000 len=16"]);
        let msgs = msgs.lock().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].msg, "$+|25");
        assert_eq!(msgs[1].msg, "$+|100");
        drop(msgs);
        let lg2 = log();
        let mut e = BlockingExecutor::new(&prm)
            .with_ccp(MockCcp::new(Rc::clone(&lg2)))
            .unwrap();
        e.ccpx_program_memory("f.hex", 1, 2, 1, 0).unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(
            lg2.borrow()[1..],
            vec!["program_sync 0/20000 len=16", "program len=0"]
        );
        let mut mock = MockCcp::new(log());
        mock.program_sync_ok = false;
        let mut e = BlockingExecutor::new(&prm).with_ccp(mock).unwrap();
        e.ccpx_program_memory("f.hex", 1, 2, 0, 0).unwrap();
        assert_eq!(e.state(), 1);
        cleanup(&dir);
    }

    #[test]
    fn xcp_connect_and_reset() {
        let (dir, prm) = setup_xcp("xcp");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm).with_xcp(MockXcp {
            log: Rc::clone(&lg),
        });
        assert_eq!(e.mode(), Mode::Xcp);
        e.xcp_connect(0).unwrap();
        assert_eq!(e.state(), 0);
        e.xcp_program_reset().unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(*lg.borrow(), vec!["connect 0", "reset"]);
        cleanup(&dir);
    }

    #[test]
    fn xcp_stub_commands_only_set_state() {
        let (dir, prm) = setup_xcp("xcpstub");
        let lg = log();
        let mut e = BlockingExecutor::new(&prm).with_xcp(MockXcp {
            log: Rc::clone(&lg),
        });
        for invoke in [
            &mut (|e: &mut BlockingExecutor| e.xcp_program_start())
                as &mut dyn FnMut(&mut BlockingExecutor) -> Result<()>,
            &mut |e: &mut BlockingExecutor| e.xcp_set_mta(0, 0x1000),
            &mut |e: &mut BlockingExecutor| e.xcpx_program_clear(0, 16, 1000),
            &mut |e: &mut BlockingExecutor| e.xcpx_program_memory_file("f", 1, 2, 3, 4),
            &mut |e: &mut BlockingExecutor| e.xcpx_program_memory(),
        ] {
            e.set_state(9);
            invoke(&mut e).unwrap();
            assert_eq!(e.state(), 0);
        }
        assert!(lg.borrow().is_empty());
        cleanup(&dir);
    }

    #[test]
    fn display_message_sentinels_and_par_bits() {
        let (dir, prm) = setup_uds("disp");
        let (msgs, sink) = msgs_sink();
        let mut e = BlockingExecutor::new(&prm).on_message(sink);
        e.set_state(0xAB);
        e.display_message("%h", 1).unwrap();
        assert_eq!(msgs.lock().unwrap()[0].msg, "AB\n");
        e.set_state(0xAB);
        e.display_message("%d", 2).unwrap();
        assert_eq!(msgs.lock().unwrap()[1].msg, "171");
        e.display_message("plain", 1).unwrap();
        assert_eq!(msgs.lock().unwrap()[2].msg, "plain\n");
        assert_eq!(e.state(), 1);
        e.display_message("x", 2).unwrap();
        assert_eq!(e.state(), 0);
        let mut e = BlockingExecutor::new(&prm);
        e.set_state(5);
        e.display_message("x", 1).unwrap();
        assert_eq!(e.state(), 5);
        cleanup(&dir);
    }

    #[test]
    fn variables_set_get() {
        let (dir, prm) = setup_uds("vars");
        let mut e = BlockingExecutor::new(&prm);
        e.set_variable_str(7, "hello").unwrap();
        assert_eq!(e.state(), 7);
        e.get_variable(7).unwrap();
        assert_eq!(e.state(), 1);
        e.set_variable_num(8, 42).unwrap();
        assert_eq!(e.state(), 1);
        e.get_variable(8).unwrap();
        assert_eq!(e.state(), 42);
        let err = e.get_variable(9).unwrap_err();
        assert!(
            err.to_string()
                .contains("GET_VARIABLE: Variable 9 was not set"),
            "{err}"
        );
        cleanup(&dir);
    }

    #[test]
    fn state_only_and_empty_commands() {
        let (dir, prm) = setup_uds("states");
        let mut e = BlockingExecutor::new(&prm);
        e.run_dll("x.dll", &[]).unwrap();
        assert_eq!(e.state(), 1);
        e.init_flash_programming(0x7C, 0, "conf.cnf").unwrap();
        assert_eq!(e.state(), 1);
        e.set_state(0);
        e.show_programming_info(1, "f.bin", 0).unwrap();
        assert_eq!(e.state(), 1);
        e.udsb_init_communication().unwrap();
        assert_eq!(e.state(), 0);
        e.set_state(3);
        e.display_error_message(1).unwrap();
        e.default_screen_layout(1).unwrap();
        e.extended_message(1).unwrap();
        e.set_debug_level(1).unwrap();
        e.check_inca_configuration().unwrap();
        e.ccpb_store_ccp_cmd_timeout(1, 100).unwrap();
        assert_eq!(e.state(), 3);
        assert!(e.call("proc").is_err());
        assert!(e.set_re_entry("step").is_err());
        cleanup(&dir);
    }

    #[test]
    fn cancel_suppresses_everything() {
        let (dir, prm) = setup_uds("cancel");
        let lg = log();
        let (msgs, sink) = msgs_sink();
        let mut e = BlockingExecutor::new(&prm)
            .with_uds(MockUds::new(Rc::clone(&lg)))
            .on_message(sink);
        let handle = e.cancel_handle();
        e.set_state(7);
        handle.store(true, Ordering::Relaxed);
        assert!(e.is_cancelled());
        e.uds_diagnostic_session_control(2).unwrap();
        assert_eq!(e.state(), 7);
        assert!(lg.borrow().is_empty());
        e.display_message("x", 1).unwrap();
        assert!(msgs.lock().unwrap().is_empty());
        e.set_variable_num(1, 2).unwrap();
        e.get_variable(1).unwrap();
        assert_eq!(e.state(), 7);
        e.udsb_init_communication().unwrap();
        assert_eq!(e.state(), 0);
        cleanup(&dir);
    }

    #[test]
    fn wait_sleeps_unless_cancelled() {
        let (dir, prm) = setup_uds("wait");
        let mut e = BlockingExecutor::new(&prm);
        let t0 = std::time::Instant::now();
        e.wait(20).unwrap();
        assert!(t0.elapsed() >= Duration::from_millis(20));
        e.cancel();
        let t0 = std::time::Instant::now();
        e.wait(500).unwrap();
        assert!(t0.elapsed() < Duration::from_millis(200));
        cleanup(&dir);
    }

    #[test]
    fn no_client_commands_are_noop() {
        let (dir, prm) = setup_uds("nocomm");
        let mut e = BlockingExecutor::new(&prm);
        e.set_state(5);
        e.uds_diagnostic_session_control(2).unwrap();
        assert_eq!(e.state(), 0);
        e.set_state(5);
        e.udsx_program_memory("f", 1, 2, 0, "").unwrap();
        assert_eq!(e.state(), 0);
        e.set_state(5);
        e.xcp_connect(0).unwrap();
        assert_eq!(e.state(), 0);
        cleanup(&dir);
    }

    #[test]
    fn udsx_scaling_flow() {
        let (dir, prm) = setup_uds("scaling");
        let lg = log();
        let (msgs, sink) = msgs_sink();
        let mut mock = MockUds::new(Rc::clone(&lg));
        mock.replies.push_back(reply(MsgState::Success, 0, &[1]));
        let mut e = BlockingExecutor::new(&prm).with_uds(mock).on_message(sink);
        e.set_state(5);
        e.udsx_read_data_by_identifier_scaling("reading", "?", "0xF190", "0x00")
            .unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(msgs.lock().unwrap()[0].msg, "reading\n");
        assert_eq!(*lg.borrow(), vec!["rdbi [F190]"]);
        cleanup(&dir);
    }
}

// ============================================================================
// ============================================================================

#[cfg(test)]
mod async_tests {
    use super::*;
    use autors_datafile::datafile::{DataFile, DataFileType, MemorySegmentList};
    use std::sync::Mutex;

    type SharedLog = Arc<Mutex<Vec<String>>>;

    fn shared_log() -> SharedLog {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn ok_reply() -> autors_diag::Result<UdsReply> {
        Ok(UdsReply {
            state: MsgState::Success,
            error_code: 0,
            data: Vec::new(),
        })
    }

    struct AsyncMockUds {
        log: SharedLog,
        progress_steps: Vec<u32>,
    }

    impl AsyncMockUds {
        fn new(log: SharedLog) -> Self {
            Self {
                log,
                progress_steps: Vec::new(),
            }
        }

        fn push(&mut self, s: String) {
            self.log.lock().unwrap().push(s);
        }
    }

    #[async_trait::async_trait(?Send)]
    impl UdsOps for AsyncMockUds {
        async fn exec_sf_base(
            &mut self,
            service: u8,
            data: &[u8],
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("sf_base sid={service:02X} data={data:02X?}"));
            ok_reply()
        }
        async fn exec_routine_control(
            &mut self,
            sf: u8,
            data: &[u8],
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("routine sf={sf:02X} data={data:02X?}"));
            ok_reply()
        }
        async fn exec_data(
            &mut self,
            service: u8,
            data: Option<&[u8]>,
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("data sid={service:02X} data={data:02X?}"));
            ok_reply()
        }
        async fn diagnostic_session_control(&mut self, sf: u8) -> autors_diag::Result<UdsReply> {
            self.push(format!("session {sf:02X}"));
            ok_reply()
        }
        async fn control_dtc_setting(&mut self, sf: u8) -> autors_diag::Result<UdsReply> {
            self.push(format!("dtc {sf:02X}"));
            ok_reply()
        }
        async fn read_data_by_identifier(
            &mut self,
            identifiers: &[u16],
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("rdbi {identifiers:04X?}"));
            ok_reply()
        }
        async fn write_data_by_identifier(
            &mut self,
            id: u16,
            data: &[u8],
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("wdbi {id:04X} {data:02X?}"));
            ok_reply()
        }
        async fn clear_diagnostic_information(
            &mut self,
            group_of_dtc: u32,
        ) -> autors_diag::Result<UdsReply> {
            self.push(format!("clear {group_of_dtc:06X}"));
            ok_reply()
        }
        async fn ecu_reset(&mut self, reset_type: u8) -> autors_diag::Result<UdsReply> {
            self.push(format!("reset {reset_type:02X}"));
            ok_reply()
        }
        async fn download(
            &mut self,
            address: i64,
            data: &[u8],
            adr_and_len_fmt: u8,
            omit_erase_mem: bool,
            progress: Option<&mut dyn FnMut(u32) -> bool>,
        ) -> autors_diag::Result<u8> {
            self.push(format!(
                "download {address:X} len={} fmt={adr_and_len_fmt:02X} omit={omit_erase_mem}",
                data.len()
            ));
            if let Some(cb) = progress {
                for &p in &self.progress_steps {
                    if cb(p) {
                        break;
                    }
                }
            }
            Ok(0)
        }
        async fn unlock(
            &mut self,
            request_seed_sf: u8,
            _sk: &dyn SeedKeyProvider,
            variant: Option<&[u8]>,
        ) -> autors_diag::Result<u8> {
            self.push(format!("unlock {request_seed_sf:02X} {variant:02X?}"));
            Ok(0)
        }
    }

    struct AsyncMockCcp {
        log: SharedLog,
        progress_steps: Vec<u32>,
    }

    impl AsyncMockCcp {
        fn new(log: SharedLog) -> Self {
            Self {
                log,
                progress_steps: Vec::new(),
            }
        }

        fn push(&mut self, s: String) {
            self.log.lock().unwrap().push(s);
        }
    }

    #[async_trait::async_trait(?Send)]
    impl CcpOps for AsyncMockCcp {
        fn set_can_ids(&mut self, cmd_id: u32, rsp_id: u32, station: u16) {
            self.push(format!("canids {cmd_id:X}/{rsp_id:X}/{station:X}"));
        }
        async fn connect(&mut self) -> CcpCmdResult {
            self.push("connect".to_owned());
            CcpCmdResult::OK
        }
        async fn disconnect(&mut self) -> bool {
            self.push("disconnect".to_owned());
            true
        }
        async fn diag_service(&mut self, no: u16, add_bytes: Option<&[u8]>) -> CcpCmdResult {
            self.push(format!("diag {no:04X} {add_bytes:02X?}"));
            CcpCmdResult::OK
        }
        async fn action_service(&mut self, no: u16, add_bytes: Option<&[u8]>) -> CcpCmdResult {
            self.push(format!("action {no:04X} {add_bytes:02X?}"));
            CcpCmdResult::OK
        }
        async fn set_mta(
            &mut self,
            mta_no: u8,
            address_extension: u8,
            address: u32,
        ) -> CcpCmdResult {
            self.push(format!("mta {mta_no}/{address_extension:02X}/{address:X}"));
            CcpCmdResult::OK
        }
        async fn clear_memory(&mut self, size: u32) -> CcpCmdResult {
            self.push(format!("clear {size}"));
            CcpCmdResult::OK
        }
        async fn program_sync(
            &mut self,
            address_extension: u8,
            address: u32,
            data: &[u8],
            progress: Option<&mut dyn FnMut(u32) -> bool>,
        ) -> bool {
            self.push(format!(
                "program_sync {address_extension}/{address:X} len={}",
                data.len()
            ));
            if let Some(cb) = progress {
                for &p in &self.progress_steps {
                    if cb(p) {
                        break;
                    }
                }
            }
            true
        }
        async fn program(&mut self, data: &[u8]) -> CcpCmdResult {
            self.push(format!("program len={}", data.len()));
            CcpCmdResult::OK
        }
    }

    struct AsyncMockXcp {
        log: SharedLog,
    }

    #[async_trait::async_trait(?Send)]
    impl XcpOps for AsyncMockXcp {
        async fn connect(&mut self, mode: u8) -> XcpCmdResult {
            self.log.lock().unwrap().push(format!("connect {mode}"));
            XcpCmdResult::OK
        }
        async fn program_reset(&mut self) {
            self.log.lock().unwrap().push("reset".to_owned());
        }
    }

    struct AsyncMockCan {
        log: SharedLog,
    }

    #[async_trait::async_trait(?Send)]
    impl CanSend for AsyncMockCan {
        async fn send_msg(&mut self, id: u32, data: &[u8]) -> usize {
            self.log
                .lock()
                .unwrap()
                .push(format!("can {id:X} {data:02X?}"));
            data.len()
        }
    }

    const EXEC_CNF: &str = "\
PROJECT_NAME:test
ECU_ADDR:0x7C
KWP_CAN_BUS_TIMING:500000
INCA_TO_ECU_CAN_ID:0x700
ECU_TO_INCA_CAN_ID:0x708
ADDRESS_AND_LENGTH_FORMAT_IDENTIFIER:0x44
DATA_FORMAT_IDENTIFIER:0x00
SOURCE_MEM_AREA:1,0,0,0x10000L,0x1000FL
DEST_MEM_AREA:2,0,0,0x20000L,0x2000FL
";

    const EXEC_HEX: &str =
        ":020000040001F9\n:10000000000102030405060708090A0B0C0D0E0F78\n:00000001FF\n";

    fn setup(tag: &str, mode_cmd: &str) -> (PathBuf, PrmFile) {
        let dir =
            std::env::temp_dir().join(format!("autors_prm_async_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("conf.cnf"), EXEC_CNF).unwrap();
        let prm_src = format!("#define CONFIG conf.cnf\n[S]\n{mode_cmd}\ndefault : S\n[S_END]\n");
        std::fs::write(dir.join("main.prm"), prm_src).unwrap();
        let prm = PrmFile::open(dir.join("main.prm")).unwrap();
        (dir, prm)
    }

    fn load_seg_data(prm: &mut PrmFile) {
        let df = DataFile::parse(
            DataFileType::IntelHex,
            EXEC_HEX.as_bytes(),
            MemorySegmentList::new(),
            0,
            None,
        )
        .unwrap();
        prm.cnf.load_segment_data(&df, 0, "t.hex").unwrap();
    }

    fn cleanup(dir: &Path) {
        std::fs::remove_dir_all(dir).ok();
    }

    fn msgs_sink() -> (Arc<Mutex<Vec<PrmMsg>>>, impl FnMut(PrmMsg)) {
        let msgs = Arc::new(Mutex::new(Vec::<PrmMsg>::new()));
        let m2 = Arc::clone(&msgs);
        (msgs, move |m| m2.lock().unwrap().push(m))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_uds_flash_flow() {
        let (dir, mut prm) = setup("uds", "UDSX_PROGRAM_MEMORY(1, 1, 2, 3, \"x\")");
        load_seg_data(&mut prm);
        let lg = shared_log();
        let mut mock = AsyncMockUds::new(Arc::clone(&lg));
        mock.progress_steps = vec![10, 10, 50, 100];
        let (msgs, sink) = msgs_sink();
        let mut e = Executor::new(&prm).with_uds(mock).on_message(sink);
        e.uds_diagnostic_session_control(2).await.unwrap();
        assert_eq!(e.state(), 0);
        e.udsx_verify_memory("f.hex", 1, 2, 0, 0, 0).await.unwrap();
        assert_eq!(e.state(), 0);
        e.udsx_program_memory("f.hex", 1, 2, 0, "").await.unwrap();
        assert_eq!(e.state(), 0);
        {
            let log = lg.lock().unwrap();
            assert_eq!(log[0], "session 02");
            assert!(
                log[1].starts_with(
                    "sf_base sid=31 data=[F0, 01, 01, 00, 02, 00, 00, 00, 02, 00, 0F, "
                ),
                "{}",
                log[1]
            );
            assert_eq!(log[2], "download 20000 len=16 fmt=44 omit=true");
        }
        let msgs = msgs.lock().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].msg, "$+|10");
        assert_eq!(msgs[1].msg, "$+|50");
        assert_eq!(msgs[2].msg, "$+|100");
        assert_eq!(msgs[0].msg_type, MsgType::Message);
        drop(msgs);
        cleanup(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_ccp_flash_flow() {
        let (dir, mut prm) = setup("ccp", "CCPX_START_ECU_COMMUNICATION");
        load_seg_data(&mut prm);
        let lg = shared_log();
        let mut mock = AsyncMockCcp::new(Arc::clone(&lg));
        mock.progress_steps = vec![25, 100];
        let (msgs, sink) = msgs_sink();
        let mut e = Executor::new(&prm).with_ccp(mock).unwrap().on_message(sink);
        assert_eq!(e.mode(), Mode::Ccp);
        e.ccpx_start_ecu_communication().await.unwrap();
        assert_eq!(e.state(), 0);
        e.ccpx_erase_memory(2, 1000).await.unwrap();
        assert_eq!(e.state(), 0);
        e.ccpx_program_memory("f.hex", 1, 2, 1, 0).await.unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(
            *lg.lock().unwrap(),
            vec![
                "canids 700/708/7C",
                "connect",
                "mta 0/00/20000",
                "clear 16",
                "program_sync 0/20000 len=16",
                "program len=0",
            ]
        );
        let msgs = msgs.lock().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].msg, "$+|25");
        assert_eq!(msgs[1].msg, "$+|100");
        drop(msgs);
        cleanup(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_wait_and_cancel() {
        let (dir, prm) = setup("wait", "UDSX_PROGRAM_MEMORY(1, 1, 2, 3, \"x\")");
        let lg = shared_log();
        let mut e = Executor::new(&prm).with_uds(AsyncMockUds::new(Arc::clone(&lg)));
        let t0 = std::time::Instant::now();
        e.wait(20).await.unwrap();
        assert!(t0.elapsed() >= Duration::from_millis(20));
        e.set_state(7);
        e.cancel();
        assert!(e.is_cancelled());
        let t0 = std::time::Instant::now();
        e.wait(500).await.unwrap();
        assert!(t0.elapsed() < Duration::from_millis(200));
        e.uds_diagnostic_session_control(2).await.unwrap();
        assert_eq!(e.state(), 7);
        assert!(lg.lock().unwrap().is_empty());
        cleanup(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_xcp_and_can_flow() {
        let (dir, prm) = setup("xcp", "XCPX_PROGRAM_CLEAR(1, 2, 3)");
        let lg = shared_log();
        let mut e = Executor::new(&prm)
            .with_xcp(AsyncMockXcp {
                log: Arc::clone(&lg),
            })
            .with_can_sender(AsyncMockCan {
                log: Arc::clone(&lg),
            });
        assert_eq!(e.mode(), Mode::Xcp);
        e.xcp_connect(0).await.unwrap();
        assert_eq!(e.state(), 0);
        e.xcp_program_reset().await.unwrap();
        assert_eq!(e.state(), 0);
        e.can_send_message(0x123, "AA BB").await.unwrap();
        assert_eq!(e.state(), 0);
        assert_eq!(
            *lg.lock().unwrap(),
            vec!["connect 0", "reset", "can 123 [AA, BB]"]
        );
        cleanup(&dir);
    }
}
