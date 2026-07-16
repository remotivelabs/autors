//! `IPrm` → [`Prm`] trait;`IPrmExecutor` → [`PrmExecutor`] trait;

use crate::error::{Error, Result};
use async_trait::async_trait;

/// `execute` drives the whole instruction flow (bus I/O and waits), so it is
/// async (object-safe via `async_trait(?Send)`; the future is not `Send`
/// because implementations typically hold [`PrmExecutor`] callbacks across
/// await points). Synchronous callers use the `blocking` feature facade
/// (`blocking::BlockingPrm`).
#[async_trait(?Send)]
pub trait Prm {
    async fn execute(&mut self) -> Result<()>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MsgType {
    Warn,
    Error,
    #[default]
    Message,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrmMsg {
    pub msg_type: MsgType,
    pub msg: String,
}

impl PrmMsg {
    pub fn new(msg: impl Into<String>, msg_type: MsgType, add_lf: bool) -> Self {
        let mut msg = msg.into();
        if add_lf {
            msg.push('\n');
        }
        Self { msg_type, msg }
    }
}

pub fn prm_error(msg: impl Into<String>) -> Error {
    Error::General(format!("PRM: {}", msg.into()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrmValue {
    UInt(u64),
    Int(i64),
    Real(f64),
    Text(String),
}

impl fmt::Display for PrmValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UInt(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Real(v) => write!(f, "{v}"),
            Self::Text(v) => f.write_str(v),
        }
    }
}

use std::fmt;

pub(crate) fn not_implemented(name: &str) -> Error {
    prm_error(format!("instruction not implemented: {name}"))
}

/// Instructions with bus I/O or wait semantics (`wait`, `can_send_message`,
/// the `uds_*`/`udsx_*` service family, the `ccp_*`/`ccpx_*` family and the
/// XCP commands that touch the bus) are async; object safety is kept via
/// `async_trait(?Send)`. The futures are not `Send` because implementations
/// may hold progress callbacks or `&dyn` seed-key providers across await points (the
/// same constraint as the underlying client flows in autors-diag /
/// autors-ccp). Pure state or recording instructions stay synchronous.
/// Synchronous callers use the `blocking` feature facade
/// (`blocking::BlockingPrmExecutor`).
#[async_trait(?Send)]
pub trait PrmExecutor {
    fn state(&self) -> u64;

    fn set_state(&mut self, state: u64);

    async fn can_send_message(&mut self, id: u64, data: &str) -> Result<()> {
        let _ = (id, data);
        Err(not_implemented("CAN_SEND_MESSAGE"))
    }

    fn call(&mut self, procedure: &str) -> Result<u64> {
        let _ = procedure;
        Err(not_implemented("CALL"))
    }

    fn set_re_entry(&mut self, step: &str) -> Result<()> {
        let _ = step;
        Err(not_implemented("SET_RE_ENTRY"))
    }

    async fn wait(&mut self, time_in_ms: u64) -> Result<()> {
        let _ = time_in_ms;
        Err(not_implemented("WAIT"))
    }

    fn display_message(&mut self, msg: &str, par: u64) -> Result<()> {
        let _ = (msg, par);
        Err(not_implemented("DISPLAY_MESSAGE"))
    }

    fn display_error_message(&mut self, par: u64) -> Result<()> {
        let _ = par;
        Err(not_implemented("DISPLAY_ERROR_MESSAGE"))
    }

    fn default_screen_layout(&mut self, value: u64) -> Result<()> {
        let _ = value;
        Err(not_implemented("DEFAULT_SCREEN_LAYOUT"))
    }

    fn extended_message(&mut self, par: u64) -> Result<()> {
        let _ = par;
        Err(not_implemented("EXTENDED_MESSAGE"))
    }

    fn set_variable_str(&mut self, variable: u64, action: &str) -> Result<()> {
        let _ = (variable, action);
        Err(not_implemented("SET_VARIABLE(string)"))
    }

    fn set_variable_num(&mut self, variable: u64, action: u64) -> Result<()> {
        let _ = (variable, action);
        Err(not_implemented("SET_VARIABLE(ulong)"))
    }

    fn get_variable(&mut self, variable: u64) -> Result<()> {
        let _ = variable;
        Err(not_implemented("GET_VARIABLE"))
    }

    fn run_dll(&mut self, file: &str, args: &[PrmValue]) -> Result<()> {
        let _ = (file, args);
        Err(not_implemented("RUN_DLL"))
    }

    fn check_inca_configuration(&mut self) -> Result<()> {
        Err(not_implemented("CHECK_INCA_CONFIGURATION"))
    }

    fn set_debug_level(&mut self, level: u64) -> Result<()> {
        let _ = level;
        Err(not_implemented("SET_DEBUG_LEVEL"))
    }

    fn init_flash_programming(
        &mut self,
        ecu_address: u64,
        flash_type: i64,
        cnf: &str,
    ) -> Result<()> {
        let _ = (ecu_address, flash_type, cnf);
        Err(not_implemented("INIT_FLASH_PROGRAMMING"))
    }

    fn udsb_init_communication(&mut self) -> Result<()> {
        Err(not_implemented("UDSB_INIT_COMMUNICATION"))
    }

    fn udsb_msg_ret_get_at(&mut self, idx: u64, default: u8) -> Result<()> {
        let _ = (idx, default);
        Err(not_implemented("UDSB_MSG_RET_GET_AT"))
    }

    async fn uds_communication_control(
        &mut self,
        control_type: u64,
        communication_type: u64,
    ) -> Result<()> {
        let _ = (control_type, communication_type);
        Err(not_implemented("UDS_COMMUNICATION_CONTROL"))
    }

    async fn uds_diagnostic_session_control(&mut self, sf: u64) -> Result<()> {
        let _ = sf;
        Err(not_implemented("UDS_DIAGNOSTIC_SESSION_CONTROL"))
    }

    async fn uds_control_dtc_setting(&mut self, dtc_off: u64, msg: &str) -> Result<()> {
        let _ = (dtc_off, msg);
        Err(not_implemented("UDS_CONTROL_DTC_SETTING"))
    }

    async fn uds_read_data_by_identifier(&mut self, hex_value: &str) -> Result<()> {
        let _ = hex_value;
        Err(not_implemented("UDS_READ_DATA_BY_IDENTIFIER"))
    }

    async fn uds_write_data_by_identifier(&mut self, id: u64, hex_data: &str) -> Result<()> {
        let _ = (id, hex_data);
        Err(not_implemented("UDS_WRITE_DATA_BY_IDENTIFIER"))
    }

    async fn uds_routine_control(&mut self, sf: u64, routine: u64, data_bytes: &str) -> Result<()> {
        let _ = (sf, routine, data_bytes);
        Err(not_implemented("UDS_ROUTINE_CONTROL"))
    }

    async fn uds_pass_through(&mut self, hex_data: &str) -> Result<()> {
        let _ = hex_data;
        Err(not_implemented("UDS_PASS_THROUGH"))
    }

    async fn uds_ecu_reset(&mut self, reset_type: u64) -> Result<()> {
        let _ = reset_type;
        Err(not_implemented("UDS_ECU_RESET"))
    }

    fn uds_read_data_by_identifier_get_data_rec_at(&mut self, idx: u64) -> Result<()> {
        let _ = idx;
        Err(not_implemented(
            "UDS_READ_DATA_BY_IDENTIFIER_GET_DATA_REC_AT",
        ))
    }

    async fn uds_clear_dtc_information(&mut self, p: u64) -> Result<()> {
        let _ = p;
        Err(not_implemented("UDS_CLEAR_DTC_INFORMATION"))
    }

    async fn udsx_verify_memory(
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

    async fn udsx_security_access(&mut self, sf: u64, variant: u64, sk_file: &str) -> Result<()> {
        let _ = (sf, variant, sk_file);
        Err(not_implemented("UDSX_SECURITY_ACCESS"))
    }

    async fn udsx_program_memory(
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

    async fn udsx_read_data_by_identifier_scaling(
        &mut self,
        msg: &str,
        unknown: &str,
        hex_id: &str,
        hex_idx: &str,
    ) -> Result<()> {
        let _ = (msg, unknown, hex_id, hex_idx);
        Err(not_implemented("UDSX_READ_DATA_BY_IDENTIFIER_SCALING"))
    }

    async fn ccp_disconnect(&mut self, permanent: u64) -> Result<()> {
        let _ = permanent;
        Err(not_implemented("CCP_DISCONNECT"))
    }

    fn ccpb_store_ccp_cmd_timeout(&mut self, cmd: u64, timeout: u64) -> Result<()> {
        let _ = (cmd, timeout);
        Err(not_implemented("CCPB_STORE_CCP_CMD_TIMEOUT"))
    }

    fn ccpb_set_canids(&mut self, cmd_id: u64, rsp_id: u64, station_addr: u64) -> Result<()> {
        let _ = (cmd_id, rsp_id, station_addr);
        Err(not_implemented("CCPB_SET_CANIDS"))
    }

    async fn ccpx_start_ecu_communication(&mut self) -> Result<()> {
        Err(not_implemented("CCPX_START_ECU_COMMUNICATION"))
    }

    async fn ccpx_diag_service(&mut self, diag: u64, p: &[u64]) -> Result<()> {
        let _ = (diag, p);
        Err(not_implemented("CCPX_DIAG_SERVICE"))
    }

    async fn ccpx_action_service(&mut self, action: u64, p: &[u64]) -> Result<()> {
        let _ = (action, p);
        Err(not_implemented("CCPX_ACTION_SERVICE"))
    }

    fn show_programming_info(&mut self, step: u64, bin_file: &str, p2: u64) -> Result<()> {
        let _ = (step, bin_file, p2);
        Err(not_implemented("SHOW_PROGRAMMING_INFO"))
    }

    async fn ccpx_erase_memory(&mut self, seg_idx: u64, timeout: u64) -> Result<()> {
        let _ = (seg_idx, timeout);
        Err(not_implemented("CCPX_ERASE_MEMORY"))
    }

    async fn ccpx_program_memory(
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

    async fn xcp_connect(&mut self, mode: u64) -> Result<()> {
        let _ = mode;
        Err(not_implemented("XCP_CONNECT"))
    }

    fn xcp_program_start(&mut self) -> Result<()> {
        Err(not_implemented("XCP_PROGRAM_START"))
    }

    fn xcp_set_mta(&mut self, adr_ext: u64, address: u64) -> Result<()> {
        let _ = (adr_ext, address);
        Err(not_implemented("XCP_SET_MTA"))
    }

    fn xcpx_program_clear(&mut self, mode: u64, len: u64, timeout: u64) -> Result<()> {
        let _ = (mode, len, timeout);
        Err(not_implemented("XCPX_PROGRAM_CLEAR"))
    }

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

    fn xcpx_program_memory(&mut self) -> Result<()> {
        Err(not_implemented("XCPX_PROGRAM_MEMORY"))
    }

    async fn xcp_program_reset(&mut self) -> Result<()> {
        Err(not_implemented("XCP_PROGRAM_RESET"))
    }
}

#[cfg(all(test, feature = "blocking"))]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockExecutor {
        state: u64,
        calls: Vec<String>,
    }

    #[async_trait::async_trait(?Send)]
    impl PrmExecutor for MockExecutor {
        fn state(&self) -> u64 {
            self.state
        }

        fn set_state(&mut self, state: u64) {
            self.state = state;
        }

        async fn can_send_message(&mut self, id: u64, data: &str) -> Result<()> {
            self.calls.push(format!("CAN_SEND_MESSAGE {id:X} {data}"));
            Ok(())
        }

        async fn wait(&mut self, time_in_ms: u64) -> Result<()> {
            self.calls.push(format!("WAIT {time_in_ms}"));
            Ok(())
        }

        async fn uds_ecu_reset(&mut self, reset_type: u64) -> Result<()> {
            self.calls.push(format!("UDS_ECU_RESET {reset_type}"));
            Ok(())
        }

        async fn ccpx_diag_service(&mut self, diag: u64, p: &[u64]) -> Result<()> {
            self.calls.push(format!("CCPX_DIAG_SERVICE {diag} {p:?}"));
            Ok(())
        }

        fn run_dll(&mut self, file: &str, args: &[PrmValue]) -> Result<()> {
            let args: Vec<String> = args.iter().map(ToString::to_string).collect();
            self.calls
                .push(format!("RUN_DLL {file} [{}]", args.join(",")));
            Ok(())
        }
    }

    struct MockPrm;

    #[async_trait::async_trait(?Send)]
    impl Prm for MockPrm {
        async fn execute(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn msg_event_args() {
        assert_eq!(MsgType::Warn as i32, 0);
        assert_eq!(MsgType::Error as i32, 1);
        assert_eq!(MsgType::Message as i32, 2);
        let m = PrmMsg::new("hello", MsgType::Message, true);
        assert_eq!(m.msg, "hello\n");
        assert_eq!(m.msg_type, MsgType::Message);
        let m2 = PrmMsg::new("hi", MsgType::Error, false);
        assert_eq!(m2.msg, "hi");
        assert_eq!(m2.msg_type, MsgType::Error);
        assert_eq!(MsgType::default(), MsgType::Message);
    }

    #[test]
    fn executor_records_calls() {
        let mut e = MockExecutor::default();
        e.set_state(7);
        assert_eq!(e.state(), 7);
        autors_runtime::block_on(e.can_send_message(0x123, "11 22")).unwrap();
        autors_runtime::block_on(e.wait(100)).unwrap();
        autors_runtime::block_on(e.uds_ecu_reset(1)).unwrap();
        autors_runtime::block_on(e.ccpx_diag_service(3, &[1, 2, 3])).unwrap();
        e.run_dll("sk.dll", &[PrmValue::UInt(5), PrmValue::Text("x".into())])
            .unwrap();
        assert_eq!(
            e.calls,
            vec![
                "CAN_SEND_MESSAGE 123 11 22",
                "WAIT 100",
                "UDS_ECU_RESET 1",
                "CCPX_DIAG_SERVICE 3 [1, 2, 3]",
                "RUN_DLL sk.dll [5,x]",
            ]
        );
    }

    #[test]
    fn executor_default_impl_errors() {
        let mut e = MockExecutor::default();
        let err = autors_runtime::block_on(e.xcp_connect(0)).unwrap_err();
        match err {
            Error::General(m) => assert!(m.contains("XCP_CONNECT")),
            other => panic!("unexpected error: {other}"),
        }
        assert!(e.call("proc").is_err());
        assert!(autors_runtime::block_on(e.udsx_security_access(1, 2, "f.dll")).is_err());
        assert!(e.xcpx_program_memory().is_err());
    }

    #[test]
    fn prm_error_maps_to_general() {
        match prm_error("boom") {
            Error::General(m) => assert!(m.contains("boom")),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn prm_trait_execute() {
        assert!(autors_runtime::block_on(MockPrm.execute()).is_ok());
    }

    #[test]
    fn prm_value_display() {
        assert_eq!(PrmValue::UInt(5).to_string(), "5");
        assert_eq!(PrmValue::Int(-2).to_string(), "-2");
        assert_eq!(PrmValue::Text("ab".into()).to_string(), "ab");
    }
}
