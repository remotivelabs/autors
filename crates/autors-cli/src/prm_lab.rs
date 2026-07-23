use std::cell::RefCell;
use std::rc::Rc;

use autors_prm::executor::Executor;
use autors_prm::prm::PrmFile;
use autors_prm::script::{execute, ExecutionOptions, ExecutionReport};

pub struct PrmLab {
    project: Option<PrmFile>,
    pub entry_index: usize,
    pub step_index: usize,
    pub report: Option<ExecutionReport>,
    pub messages: Vec<String>,
    pub last_error: Option<String>,
}

impl Default for PrmLab {
    fn default() -> Self {
        Self::new()
    }
}

impl PrmLab {
    pub fn new() -> Self {
        Self {
            project: None,
            entry_index: 0,
            step_index: 0,
            report: None,
            messages: Vec::new(),
            last_error: None,
        }
    }

    pub fn attach_project(&mut self, project: &PrmFile) {
        self.project = Some(project.clone());
        self.entry_index = 0;
        self.clear_report();
    }

    pub fn is_loaded(&self) -> bool {
        self.project.is_some()
    }

    pub fn mode(&self) -> String {
        self.project
            .as_ref()
            .map(|project| project.mode.to_string())
            .unwrap_or_else(|| "-".to_owned())
    }

    pub fn entries(&self) -> Vec<&str> {
        self.project
            .as_ref()
            .map(|project| project.cmdsets.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }

    pub fn selected_entry(&self) -> Option<&str> {
        self.entries().get(self.entry_index).copied()
    }

    pub fn move_entry(&mut self, delta: isize) {
        self.entry_index = self
            .entry_index
            .saturating_add_signed(delta)
            .min(self.entries().len().saturating_sub(1));
    }

    pub fn move_step(&mut self, delta: isize) {
        self.step_index = self.step_index.saturating_add_signed(delta).min(
            self.report
                .as_ref()
                .map_or(0, |report| report.steps.len())
                .saturating_sub(1),
        );
    }

    pub fn clear_report(&mut self) {
        self.report = None;
        self.messages.clear();
        self.last_error = None;
        self.step_index = 0;
    }

    /// Traverses the same script graph and instruction dispatcher used by a
    /// live executor, but deliberately suppresses waits and transport I/O.
    pub fn run_dry_run(&mut self) -> Result<&ExecutionReport, String> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| "open a PRM project first".to_owned())?;
        let entry = project
            .cmdsets
            .keys()
            .nth(self.entry_index)
            .cloned()
            .ok_or_else(|| "PRM project has no entry command set".to_owned())?;
        let messages = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&messages);
        let mut executor = Executor::new(project).on_message(move |message| {
            captured.borrow_mut().push(format!(
                "{:?}: {}",
                message.msg_type,
                message.msg.trim_end()
            ));
        });
        let result = autors_runtime::block_on(execute(
            project,
            &mut executor,
            Some(&entry),
            ExecutionOptions {
                max_steps: 10_000,
                execute_io: false,
            },
        ));
        self.messages = messages.borrow().clone();
        match result {
            Ok(report) => {
                self.step_index = 0;
                self.last_error = None;
                self.report = Some(report);
                Ok(self.report.as_ref().expect("report inserted above"))
            }
            Err(error) => {
                let error = error.to_string();
                self.last_error = Some(error.clone());
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use autors_prm::prm::{CmdSet, Mode, PrmCommand, PrmValue};

    use super::*;

    #[test]
    fn dry_run_traverses_protocol_steps_without_bus_io() {
        let mut project = PrmFile {
            mode: Mode::Uds,
            ..PrmFile::default()
        };
        project.cmdsets.insert(
            "FLASH".to_owned(),
            CmdSet {
                name: "FLASH".to_owned(),
                commands: vec![
                    PrmCommand {
                        name: "DISPLAY_MESSAGE".to_owned(),
                        args: vec![PrmValue::Str("starting".to_owned())],
                        ..PrmCommand::default()
                    },
                    PrmCommand {
                        name: "UDS_DIAGNOSTIC_SESSION_CONTROL".to_owned(),
                        args: vec![PrmValue::UInt(2)],
                        ..PrmCommand::default()
                    },
                ],
            },
        );
        let mut lab = PrmLab::new();
        lab.attach_project(&project);
        let report = lab.run_dry_run().unwrap();
        assert_eq!(report.steps.len(), 2);
        assert!(!report.steps[0].simulated);
        assert!(report.steps[1].simulated);
        assert!(lab.messages[0].contains("starting"));
    }
}
