//! Interpreter for parsed PRM command sets and procedure control flow.
//!
//! [`crate::executor::Executor`] implements the individual PRM operations;
//! this module supplies the missing orchestration layer that walks command
//! sets, follows state-dependent branches, and enters procedures.

use crate::prm::{CaseKey, CmdSet, PrmCommand, PrmFile, PrmValue, Procedure};
use crate::prm_if::{prm_error, PrmExecutor, PrmValue as ExecutorValue};
use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStep {
    pub scope: String,
    pub command_set: String,
    pub command_index: usize,
    pub command: String,
    pub state: u64,
    pub next: Option<String>,
    pub simulated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub entry: String,
    pub final_state: u64,
    pub steps: Vec<ExecutionStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionOptions {
    pub max_steps: usize,
    /// When false, waits and transport commands are traversed without bus I/O.
    /// Pure state, variable, message, and branch semantics still execute.
    pub execute_io: bool,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            max_steps: 10_000,
            execute_io: true,
        }
    }
}

struct Frame {
    procedure: Option<String>,
    command_set: String,
    command_index: usize,
    return_branch: Option<PrmCommand>,
}

impl Frame {
    fn scope(&self) -> &str {
        self.procedure.as_deref().unwrap_or("Main")
    }
}

/// Executes a parsed PRM control-flow graph with the supplied operation
/// provider. When `entry` is `None`, the first main command set is used.
pub async fn execute(
    prm: &PrmFile,
    executor: &mut dyn PrmExecutor,
    entry: Option<&str>,
    options: ExecutionOptions,
) -> Result<ExecutionReport> {
    if options.max_steps == 0 {
        return Err(prm_error("execution step limit must be greater than zero"));
    }
    let entry = entry
        .map(str::to_owned)
        .or_else(|| prm.cmdsets.keys().next().cloned())
        .ok_or_else(|| prm_error("script has no main command set"))?;
    if !prm.cmdsets.contains_key(&entry) {
        return Err(prm_error(format!("entry command set {entry:?} not found")));
    }
    let mut frames = vec![Frame {
        procedure: None,
        command_set: entry.clone(),
        command_index: 0,
        return_branch: None,
    }];
    let mut steps = Vec::new();

    while !frames.is_empty() {
        if steps.len() == options.max_steps {
            return Err(prm_error(format!(
                "execution exceeded the {} step safety limit",
                options.max_steps
            )));
        }
        let (command, scope, set_name, command_index) = {
            let frame = frames.last().expect("checked");
            let Some(command_set) = command_set(prm, frame) else {
                return Err(prm_error(format!(
                    "command set {:?} not found in {}",
                    frame.command_set,
                    frame.scope()
                )));
            };
            let Some(command) = command_set.commands.get(frame.command_index).cloned() else {
                finish_frame(prm, &mut frames, executor.state())?;
                continue;
            };
            (
                command,
                frame.scope().to_owned(),
                frame.command_set.clone(),
                frame.command_index,
            )
        };

        if command.name == "CALL" {
            let procedure_name = string_arg(&command, 0)?;
            let procedure = prm.procedures.get(&procedure_name).ok_or_else(|| {
                prm_error(format!("CALL: procedure {procedure_name:?} not found"))
            })?;
            let first = first_procedure_set(procedure)?;
            let caller = frames.last_mut().expect("checked");
            caller.return_branch = Some(command.clone());
            frames.push(Frame {
                procedure: Some(procedure_name),
                command_set: first,
                command_index: 0,
                return_branch: None,
            });
            steps.push(ExecutionStep {
                scope,
                command_set: set_name,
                command_index,
                command: command.name,
                state: executor.state(),
                next: frames.last().map(|frame| frame.command_set.clone()),
                simulated: false,
            });
            continue;
        }

        let simulated = !options.execute_io && is_io_command(&command.name);
        if simulated {
            executor.set_state(0);
        } else {
            dispatch(executor, &command).await?;
        }
        let state = executor.state();
        let next = branch_target(&command, state);
        steps.push(ExecutionStep {
            scope,
            command_set: set_name,
            command_index,
            command: command.name.clone(),
            state,
            next: next.clone(),
            simulated,
        });
        apply_next(prm, frames.last_mut().expect("checked"), next)?;
    }

    Ok(ExecutionReport {
        entry,
        final_state: executor.state(),
        steps,
    })
}

fn is_io_command(name: &str) -> bool {
    name == "WAIT"
        || name == "CAN_SEND_MESSAGE"
        || name.starts_with("UDS")
        || name.starts_with("CCP")
        || name.starts_with("XCP")
}

fn command_set<'a>(prm: &'a PrmFile, frame: &Frame) -> Option<&'a CmdSet> {
    match &frame.procedure {
        Some(name) => prm
            .procedures
            .get(name)?
            .cmdsets
            .iter()
            .find(|set| set.name == frame.command_set),
        None => prm.cmdsets.get(&frame.command_set),
    }
}

fn first_procedure_set(procedure: &Procedure) -> Result<String> {
    procedure
        .cmdsets
        .first()
        .map(|set| set.name.clone())
        .ok_or_else(|| {
            prm_error(format!(
                "procedure {:?} has no command sets",
                procedure.name
            ))
        })
}

fn finish_frame(prm: &PrmFile, frames: &mut Vec<Frame>, state: u64) -> Result<()> {
    frames.pop();
    if let Some(caller) = frames.last_mut() {
        if let Some(command) = caller.return_branch.take() {
            apply_next(prm, caller, branch_target(&command, state))?;
        }
    }
    Ok(())
}

fn branch_target(command: &PrmCommand, state: u64) -> Option<String> {
    command
        .cases
        .get(&CaseKey::Int(state as i64))
        .or_else(|| command.cases.get(&CaseKey::Str(state.to_string())))
        .cloned()
        .or_else(|| command.default_target.clone())
}

fn apply_next(prm: &PrmFile, frame: &mut Frame, next: Option<String>) -> Result<()> {
    let Some(next) = next else {
        frame.command_index += 1;
        return Ok(());
    };
    if next == format!("{}_END", frame.command_set) {
        frame.command_index = usize::MAX;
        return Ok(());
    }
    let exists = match &frame.procedure {
        Some(procedure) => prm
            .procedures
            .get(procedure)
            .is_some_and(|value| value.cmdsets.iter().any(|set| set.name == next)),
        None => prm.cmdsets.contains_key(&next),
    };
    if !exists {
        return Err(prm_error(format!(
            "branch target {next:?} not found in {}",
            frame.scope()
        )));
    }
    frame.command_set = next;
    frame.command_index = 0;
    Ok(())
}

async fn dispatch(executor: &mut dyn PrmExecutor, command: &PrmCommand) -> Result<()> {
    match command.name.as_str() {
        "SET_RE_ENTRY" => Ok(()),
        "WAIT" => executor.wait(uint_arg(command, 0)?).await,
        "DISPLAY_MESSAGE" => {
            executor.display_message(&string_arg(command, 0)?, uint_arg_or(command, 1, 0)?)
        }
        "DISPLAY_ERROR_MESSAGE" => executor.display_error_message(uint_arg_or(command, 0, 0)?),
        "DEFAULT_SCREEN_LAYOUT" => executor.default_screen_layout(uint_arg(command, 0)?),
        "EXTENDED_MESSAGE" => executor.extended_message(uint_arg(command, 0)?),
        "SET_DEBUG_LEVEL" => executor.set_debug_level(uint_arg(command, 0)?),
        "SET_VARIABLE" => match command.args.get(1) {
            Some(PrmValue::Str(value)) => {
                executor.set_variable_str(uint_arg(command, 0)?, &clean_string(value))
            }
            Some(_) => executor.set_variable_num(uint_arg(command, 0)?, uint_arg(command, 1)?),
            None => Err(argument_error(command, "variable and value")),
        },
        "GET_VARIABLE" => executor.get_variable(uint_arg(command, 0)?),
        "SHOW_PROGRAMMING_INFO" => executor.show_programming_info(
            uint_arg(command, 0)?,
            &string_arg(command, 1)?,
            uint_arg(command, 2)?,
        ),
        "RUN_DLL" => {
            let values: Vec<ExecutorValue> =
                command.args.iter().skip(1).map(executor_value).collect();
            executor.run_dll(&string_arg(command, 0)?, &values)
        }
        "INIT_FLASH_PROGRAMMING" => executor.init_flash_programming(
            uint_arg(command, 0)?,
            int_arg(command, 1)?,
            &string_arg(command, 2)?,
        ),
        "CAN_SEND_MESSAGE" => {
            executor
                .can_send_message(uint_arg(command, 0)?, &string_arg(command, 1)?)
                .await
        }
        "UDSB_INIT_COMMUNICATION" => executor.udsb_init_communication(),
        "UDSB_MSG_RET_GET_AT" => {
            executor.udsb_msg_ret_get_at(uint_arg(command, 0)?, uint_arg(command, 1)? as u8)
        }
        "UDS_COMMUNICATION_CONTROL" => {
            executor
                .uds_communication_control(uint_arg(command, 0)?, uint_arg(command, 1)?)
                .await
        }
        "UDS_DIAGNOSTIC_SESSION_CONTROL" => {
            executor
                .uds_diagnostic_session_control(uint_arg(command, 0)?)
                .await
        }
        "UDS_CONTROL_DTC_SETTING" => {
            executor
                .uds_control_dtc_setting(uint_arg(command, 0)?, &string_arg(command, 1)?)
                .await
        }
        "UDS_READ_DATA_BY_IDENTIFIER" => {
            executor
                .uds_read_data_by_identifier(&string_arg(command, 0)?)
                .await
        }
        "UDSX_READ_DATA_BY_IDENTIFIER_SCALING" => {
            executor
                .udsx_read_data_by_identifier_scaling(
                    &string_arg(command, 0)?,
                    &string_arg(command, 1)?,
                    &string_arg(command, 2)?,
                    &string_arg(command, 3)?,
                )
                .await
        }
        "UDS_READ_DATA_BY_IDENTIFIER_GET_DATA_REC_AT" => {
            executor.uds_read_data_by_identifier_get_data_rec_at(uint_arg(command, 0)?)
        }
        "UDS_WRITE_DATA_BY_IDENTIFIER" => {
            executor
                .uds_write_data_by_identifier(uint_arg(command, 0)?, &string_arg(command, 1)?)
                .await
        }
        "UDS_CLEAR_DTC_INFORMATION" => {
            executor
                .uds_clear_dtc_information(uint_arg(command, 0)?)
                .await
        }
        "UDS_ROUTINE_CONTROL" => {
            executor
                .uds_routine_control(
                    uint_arg(command, 0)?,
                    uint_arg(command, 1)?,
                    &string_arg(command, 2)?,
                )
                .await
        }
        "UDS_PASS_THROUGH" => executor.uds_pass_through(&string_arg(command, 0)?).await,
        "UDS_ECU_RESET" => executor.uds_ecu_reset(uint_arg(command, 0)?).await,
        "UDSX_SECURITY_ACCESS" => {
            executor
                .udsx_security_access(
                    uint_arg(command, 0)?,
                    uint_arg(command, 1)?,
                    &string_arg(command, 2)?,
                )
                .await
        }
        "UDSX_VERIFY_MEMORY" => {
            executor
                .udsx_verify_memory(
                    &string_arg(command, 0)?,
                    uint_arg(command, 1)?,
                    uint_arg(command, 2)?,
                    uint_arg(command, 3)?,
                    uint_arg(command, 4)?,
                    uint_arg(command, 5)?,
                )
                .await
        }
        "UDSX_PROGRAM_MEMORY" => {
            executor
                .udsx_program_memory(
                    &string_arg(command, 0)?,
                    uint_arg(command, 1)?,
                    uint_arg(command, 2)?,
                    uint_arg(command, 3)?,
                    &string_arg(command, 4)?,
                )
                .await
        }
        "CHECK_INCA_CONFIGURATION" => executor.check_inca_configuration(),
        "CCP_DISCONNECT" => executor.ccp_disconnect(uint_arg(command, 0)?).await,
        "CCPB_STORE_CCP_CMD_TIMEOUT" => {
            executor.ccpb_store_ccp_cmd_timeout(uint_arg(command, 0)?, uint_arg(command, 1)?)
        }
        "CCPB_SET_CANIDS" => executor.ccpb_set_canids(
            uint_arg(command, 0)?,
            uint_arg(command, 1)?,
            uint_arg(command, 2)?,
        ),
        "CCPX_START_ECU_COMMUNICATION" => executor.ccpx_start_ecu_communication().await,
        "CCPX_DIAG_SERVICE" => {
            let values = uint_args(command, 1)?;
            executor
                .ccpx_diag_service(uint_arg(command, 0)?, &values)
                .await
        }
        "CCPX_ACTION_SERVICE" => {
            let values = uint_args(command, 1)?;
            executor
                .ccpx_action_service(uint_arg(command, 0)?, &values)
                .await
        }
        "CCPX_ERASE_MEMORY" => {
            executor
                .ccpx_erase_memory(uint_arg(command, 0)?, uint_arg(command, 1)?)
                .await
        }
        "CCPX_PROGRAM_MEMORY" => {
            executor
                .ccpx_program_memory(
                    &string_arg(command, 0)?,
                    uint_arg(command, 1)?,
                    uint_arg(command, 2)?,
                    uint_arg(command, 3)?,
                    uint_arg(command, 4)?,
                )
                .await
        }
        "XCP_CONNECT" => executor.xcp_connect(uint_arg_or(command, 0, 0)?).await,
        "XCP_PROGRAM_START" => executor.xcp_program_start(),
        "XCP_SET_MTA" => executor.xcp_set_mta(uint_arg(command, 0)?, uint_arg(command, 1)?),
        "XCPX_PROGRAM_CLEAR" => executor.xcpx_program_clear(
            uint_arg(command, 0)?,
            uint_arg(command, 1)?,
            uint_arg(command, 2)?,
        ),
        "XCPX_PROGRAM_MEMORY" if command.args.is_empty() => executor.xcpx_program_memory(),
        "XCPX_PROGRAM_MEMORY" => executor.xcpx_program_memory_file(
            &string_arg(command, 0)?,
            uint_arg(command, 1)?,
            uint_arg(command, 2)?,
            uint_arg(command, 3)?,
            uint_arg(command, 4)?,
        ),
        "XCP_PROGRAM_RESET" => executor.xcp_program_reset().await,
        name => Err(prm_error(format!("unsupported PRM command {name:?}"))),
    }
}

fn argument_error(command: &PrmCommand, expected: &str) -> crate::Error {
    prm_error(format!("{}: expected {expected}", command.name))
}

fn uint_arg(command: &PrmCommand, index: usize) -> Result<u64> {
    match command.args.get(index) {
        Some(PrmValue::Int(value)) if *value >= 0 => Ok(*value as u64),
        Some(PrmValue::UInt(value)) => Ok(*value),
        Some(PrmValue::Str(value)) => {
            let parsed = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .map_or_else(|| value.parse::<u64>(), |hex| u64::from_str_radix(hex, 16));
            parsed.map_err(|_| argument_error(command, &format!("numeric argument {}", index + 1)))
        }
        _ => Err(argument_error(
            command,
            &format!("numeric argument {}", index + 1),
        )),
    }
}

fn uint_arg_or(command: &PrmCommand, index: usize, default: u64) -> Result<u64> {
    if command.args.get(index).is_some() {
        uint_arg(command, index)
    } else {
        Ok(default)
    }
}

fn int_arg(command: &PrmCommand, index: usize) -> Result<i64> {
    match command.args.get(index) {
        Some(PrmValue::Int(value)) => Ok(*value),
        Some(PrmValue::UInt(value)) => i64::try_from(*value)
            .map_err(|_| argument_error(command, &format!("signed argument {}", index + 1))),
        _ => Err(argument_error(
            command,
            &format!("signed argument {}", index + 1),
        )),
    }
}

fn string_arg(command: &PrmCommand, index: usize) -> Result<String> {
    match command.args.get(index) {
        Some(PrmValue::Str(value)) => Ok(clean_string(value)),
        Some(PrmValue::Int(value)) => Ok(value.to_string()),
        Some(PrmValue::UInt(value)) => Ok(value.to_string()),
        None => Err(argument_error(
            command,
            &format!("text argument {}", index + 1),
        )),
    }
}

fn clean_string(value: &str) -> String {
    value
        .strip_prefix('@')
        .unwrap_or(value)
        .trim_matches('"')
        .to_owned()
}

fn uint_args(command: &PrmCommand, start: usize) -> Result<Vec<u64>> {
    (start..command.args.len())
        .map(|index| uint_arg(command, index))
        .collect()
}

fn executor_value(value: &PrmValue) -> ExecutorValue {
    match value {
        PrmValue::Int(value) => ExecutorValue::Int(*value),
        PrmValue::UInt(value) => ExecutorValue::UInt(*value),
        PrmValue::Str(value) => ExecutorValue::Text(clean_string(value)),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use indexmap::IndexMap;

    use super::*;

    #[derive(Default)]
    struct MockExecutor {
        state: u64,
        log: Vec<String>,
    }

    #[async_trait(?Send)]
    impl PrmExecutor for MockExecutor {
        fn state(&self) -> u64 {
            self.state
        }

        fn set_state(&mut self, state: u64) {
            self.state = state;
        }

        fn set_variable_num(&mut self, variable: u64, action: u64) -> Result<()> {
            self.log.push(format!("set {variable}={action}"));
            self.state = action;
            Ok(())
        }

        fn display_message(&mut self, msg: &str, _par: u64) -> Result<()> {
            self.log.push(msg.to_owned());
            Ok(())
        }
    }

    fn command(name: &str, args: Vec<PrmValue>) -> PrmCommand {
        PrmCommand {
            name: name.to_owned(),
            args,
            ..PrmCommand::default()
        }
    }

    #[test]
    fn follows_state_branches_and_procedure_calls() {
        let mut select = command("SET_VARIABLE", vec![PrmValue::UInt(1), PrmValue::UInt(7)]);
        select.cases.insert(CaseKey::Int(7), "CALLER".to_owned());
        let call = command("CALL", vec![PrmValue::Str("SUB".to_owned())]);
        let main = CmdSet {
            name: "START".to_owned(),
            commands: vec![select],
        };
        let caller = CmdSet {
            name: "CALLER".to_owned(),
            commands: vec![
                call,
                command("DISPLAY_MESSAGE", vec![PrmValue::Str("done".to_owned())]),
            ],
        };
        let procedure = Procedure {
            name: "SUB".to_owned(),
            cmdsets: vec![CmdSet {
                name: "STEP".to_owned(),
                commands: vec![command(
                    "DISPLAY_MESSAGE",
                    vec![PrmValue::Str("sub".to_owned())],
                )],
            }],
        };
        let prm = PrmFile {
            cmdsets: IndexMap::from([("START".to_owned(), main), ("CALLER".to_owned(), caller)]),
            procedures: IndexMap::from([("SUB".to_owned(), procedure)]),
            ..PrmFile::default()
        };
        let mut executor = MockExecutor::default();
        let report = autors_runtime::block_on(execute(
            &prm,
            &mut executor,
            None,
            ExecutionOptions::default(),
        ))
        .unwrap();
        assert_eq!(executor.log, ["set 1=7", "sub", "done"]);
        assert_eq!(report.steps.len(), 4);
        assert_eq!(report.final_state, 7);
    }

    #[test]
    fn rejects_cycles_at_the_configured_step_limit() {
        let mut looping = command("DISPLAY_MESSAGE", vec![PrmValue::Str("again".to_owned())]);
        looping.default_target = Some("LOOP".to_owned());
        let mut prm = PrmFile::default();
        prm.cmdsets.insert(
            "LOOP".to_owned(),
            CmdSet {
                name: "LOOP".to_owned(),
                commands: vec![looping],
            },
        );
        let error = autors_runtime::block_on(execute(
            &prm,
            &mut MockExecutor::default(),
            None,
            ExecutionOptions {
                max_steps: 3,
                ..ExecutionOptions::default()
            },
        ))
        .unwrap_err();
        assert!(error.to_string().contains("3 step safety limit"));
    }
}
