use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use autors_can::device::{CanDevice, ChannelInfo, DeviceCore};
use autors_can::frame::{CanConfiguration, CanFrame, FrameType};
use autors_dbc::dbc::DBCFile;
use autors_ldf::model::Ldf;
use autors_lin::device::{LinConfiguration, LinDevice, LinFrame};
use autors_scheduler::can::CanTransmission;
use autors_scheduler::hook::{SendOutcome, TransmissionCause};
use autors_scheduler::lin::{LinOperation, LinTransmission};
use autors_scheduler::{CanScheduler, Error, LinScheduler};

const DBC: &str = r#"
VERSION "scheduler"
NS_ :
BS_:
BU_: ECU1 ECU2

BO_ 100 Status: 2 ECU1
 SG_ Counter : 0|4@1+ (1,0) [0|15] "" ECU2
BO_ 200 Event: 1 ECU2
 SG_ Value : 0|8@1+ (1,0) [0|255] "" ECU1

BA_DEF_ BO_ "GenMsgCycleTime" INT 0 10000;
BA_DEF_DEF_ "GenMsgCycleTime" 0;
BA_DEF_ BO_ "GenMsgStartDelayTime" INT 0 10000;
BA_DEF_DEF_ "GenMsgStartDelayTime" 0;
BA_DEF_ SG_ "GenSigStartValue" INT 0 255;
BA_DEF_DEF_ "GenSigStartValue" 0;
BA_ "GenMsgCycleTime" BO_ 100 10;
BA_ "GenMsgStartDelayTime" BO_ 100 5;
BA_ "GenSigStartValue" SG_ 100 Counter 5;
"#;

const LDF: &str = r#"
LIN_description_file;
LIN_protocol_version = "2.2";
LIN_language_version = "2.2";
LIN_speed = 19.2 kbps;

Nodes {
    Master: Master, 5 ms, 0.1 ms;
    Slaves: Slave, Legacy;
}

Signals {
    StatusValue: 8, 2, Slave, Master;
    CommandValue: 8, 7, Master, Slave;
}

Diagnostic_signals {
    Req: 8, 0;
    Resp: 8, 0;
}

Frames {
    Status: 1, Slave, 1 { StatusValue, 0; }
    Command: 3, Master, 1 { CommandValue, 0; }
}

Sporadic_frames {
    Burst: Command;
}

Event_triggered_frames {
    StatusEvent: Collision, 2, Status;
}

Diagnostic_frames {
    MasterReq: 60 { Req, 0; }
    SlaveResp: 61 { Resp, 0; }
}

Node_attributes {
    Slave {
        LIN_protocol = "2.2";
        configured_NAD = 1;
        initial_NAD = 2;
        product_id = 1, 2, 3;
        configurable_frames { Status; }
    }
    Legacy {
        LIN_protocol = "2.0";
        configured_NAD = 4;
        product_id = 291, 1110, 0;
        configurable_frames { Status = 4660; }
    }
}

Schedule_tables {
    Main {
        Command delay 10 ms;
        Status delay 10 ms;
    }
    Events {
        StatusEvent delay 10 ms;
    }
    Collision {
        Status delay 10 ms;
    }
    Sporadic {
        Burst delay 10 ms;
    }
    Configuration {
        AssignNAD { Slave } delay 10 ms;
        AssignFrameIdRange { Slave, 0 } delay 10 ms;
        AssignFrameId { Legacy, Status } delay 10 ms;
    }
}
"#;

struct MockCan {
    core: DeviceCore,
    sent: Vec<(u32, Vec<u8>, FrameType)>,
    rx: VecDeque<CanFrame>,
    send_limit: Option<usize>,
}

impl MockCan {
    fn new() -> Self {
        Self {
            core: DeviceCore::new(),
            sent: Vec::new(),
            rx: VecDeque::new(),
            send_limit: None,
        }
    }
}

#[async_trait]
impl CanDevice for MockCan {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    async fn is_available(&mut self) -> autors_can::Result<bool> {
        Ok(true)
    }

    async fn open(&mut self, _config: CanConfiguration) -> autors_can::Result<bool> {
        Ok(true)
    }

    async fn close(&mut self) {}

    async fn send(
        &mut self,
        can_id: u32,
        data: &[u8],
        frame_type: FrameType,
    ) -> autors_can::Result<usize> {
        self.sent.push((can_id, data.to_vec(), frame_type));
        Ok(self.send_limit.unwrap_or(data.len()).min(data.len()))
    }

    async fn receive(&mut self) -> autors_can::Result<Option<CanFrame>> {
        Ok(self.rx.pop_front())
    }

    async fn available_channels(&self) -> autors_can::Result<Vec<ChannelInfo>> {
        Ok(Vec::new())
    }
}

struct MockLin {
    sent: Vec<(u8, Vec<u8>)>,
    requested: Vec<u8>,
    request_accepted: bool,
}

impl Default for MockLin {
    fn default() -> Self {
        Self {
            sent: Vec::new(),
            requested: Vec::new(),
            request_accepted: true,
        }
    }
}

#[async_trait]
impl LinDevice for MockLin {
    fn unique_bus_id(&self) -> i32 {
        1
    }

    fn is_available(&self) -> bool {
        true
    }

    async fn open(&mut self, _config: &LinConfiguration) -> autors_lin::Result<bool> {
        Ok(true)
    }

    async fn send(&mut self, id: u8, data: &[u8]) -> autors_lin::Result<usize> {
        self.sent.push((id, data.to_vec()));
        Ok(data.len())
    }

    async fn request(&mut self, id: u8) -> autors_lin::Result<bool> {
        self.requested.push(id);
        Ok(self.request_accepted)
    }

    async fn on_receive(&mut self) -> autors_lin::Result<Option<LinFrame>> {
        Ok(None)
    }

    async fn close(&mut self) {}
}

#[tokio::test]
async fn can_uses_dbc_timing_runtime_selection_and_hooks() {
    let dbc = DBCFile::parse_str(DBC).unwrap();
    let origin = Instant::now();
    let mut scheduler = CanScheduler::from_dbc_at(&dbc, origin).unwrap();
    assert_eq!(scheduler.message(100).unwrap().payload, [5, 0]);
    assert_eq!(
        scheduler.message(100).unwrap().period,
        Some(Duration::from_millis(10))
    );
    scheduler.set_node_enabled("ECU1", true).unwrap();

    let after = Arc::new(Mutex::new(Vec::new()));
    let after_capture = Arc::clone(&after);
    let mut rolling = 0_u8;
    let before_hook = scheduler
        .add_before_hook(100, move |frame, context| {
            assert_eq!(context.node_name, "ECU1");
            frame.data[1] = rolling;
            rolling = rolling.wrapping_add(1);
            Ok(())
        })
        .unwrap();
    scheduler
        .add_after_hook(100, move |frame, _, outcome| {
            after_capture
                .lock()
                .unwrap()
                .push((frame.data.clone(), outcome.clone()));
            Ok(())
        })
        .unwrap();

    let mut device = MockCan::new();
    assert!(scheduler
        .poll_at(&mut device, origin)
        .await
        .unwrap()
        .is_empty());
    let first = scheduler
        .poll_at(&mut device, origin + Duration::from_millis(5))
        .await
        .unwrap();
    assert_eq!(
        first,
        [CanTransmission {
            message_id: 100,
            message_name: "Status".to_string(),
            cause: TransmissionCause::Cyclic,
            bytes: 2,
        }]
    );
    assert_eq!(device.sent[0].1, [5, 0]);

    scheduler.set_message_enabled(100, false).unwrap();
    assert!(scheduler
        .poll_at(&mut device, origin + Duration::from_millis(15))
        .await
        .unwrap()
        .is_empty());
    scheduler.trigger(100).unwrap();
    let triggered = scheduler
        .poll_at(&mut device, origin + Duration::from_millis(16))
        .await
        .unwrap();
    assert_eq!(triggered[0].cause, TransmissionCause::Triggered);
    assert_eq!(device.sent[1].1, [5, 1]);
    assert_eq!(after.lock().unwrap().len(), 2);
    assert_eq!(after.lock().unwrap()[0].1, SendOutcome::Sent { bytes: 2 });

    scheduler.clear_message_override(100).unwrap();
    scheduler
        .set_period(100, Some(Duration::from_millis(20)))
        .unwrap();
    assert!(
        scheduler
            .poll_at(&mut device, origin + Duration::from_millis(16))
            .await
            .unwrap()
            .len()
            == 1
    );
    assert!(scheduler.remove_hook(before_hook));
    assert!(!scheduler.remove_hook(before_hook));
    device.send_limit = Some(0);
    scheduler.trigger(100).unwrap();
    let error = scheduler
        .poll_at(&mut device, origin + Duration::from_millis(17))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::IncompleteTransmission {
            bus: "CAN",
            expected: 2,
            actual: 0,
            ..
        }
    ));
    assert!(matches!(
        after.lock().unwrap().last(),
        Some((_, SendOutcome::Failed { .. }))
    ));
}

#[tokio::test]
async fn lin_switches_between_physical_requests_and_simulated_nodes() {
    let ldf = Ldf::parse_str(LDF).unwrap();
    let origin = Instant::now();
    let mut scheduler = LinScheduler::from_ldf_at(&ldf, origin).unwrap();
    scheduler.start_schedule("Main").unwrap();
    let after = Arc::new(Mutex::new(Vec::new()));
    let after_capture = Arc::clone(&after);
    scheduler
        .add_before_hook("Status", |frame, _| {
            if let Some(value) = frame.data.first_mut() {
                *value = value.wrapping_add(1);
            }
            Ok(())
        })
        .unwrap();
    scheduler
        .add_after_hook(1_u8, move |_, _, outcome| {
            after_capture.lock().unwrap().push(outcome.clone());
            Ok(())
        })
        .unwrap();

    let mut device = MockLin::default();
    let master = scheduler.poll_at(&mut device, origin).await.unwrap();
    assert_eq!(master[0].frame_name, "Command");
    assert_eq!(master[0].operation, LinOperation::Sent);
    assert_eq!(device.sent[0], (3, vec![7]));

    let physical = scheduler
        .poll_at(&mut device, origin + Duration::from_millis(10))
        .await
        .unwrap();
    assert_eq!(physical[0].operation, LinOperation::Requested);
    assert_eq!(device.requested, [1]);
    assert_eq!(after.lock().unwrap().as_slice(), [SendOutcome::Requested]);

    scheduler.set_node_enabled("Slave", true).unwrap();
    scheduler
        .poll_at(&mut device, origin + Duration::from_millis(20))
        .await
        .unwrap();
    let simulated = scheduler
        .poll_at(&mut device, origin + Duration::from_millis(30))
        .await
        .unwrap();
    assert_eq!(simulated[0].operation, LinOperation::Sent);
    assert_eq!(device.sent.last().unwrap(), &(1, vec![3]));

    scheduler.set_frame_enabled("Status", false).unwrap();
    scheduler
        .poll_at(&mut device, origin + Duration::from_millis(40))
        .await
        .unwrap();
    let skipped = scheduler
        .poll_at(&mut device, origin + Duration::from_millis(50))
        .await
        .unwrap();
    assert_eq!(skipped[0].operation, LinOperation::Skipped);

    scheduler.clear_frame_override("Status").unwrap();
    scheduler.set_node_enabled("Slave", false).unwrap();
    device.request_accepted = false;
    scheduler
        .poll_at(&mut device, origin + Duration::from_millis(60))
        .await
        .unwrap();
    let error = scheduler
        .poll_at(&mut device, origin + Duration::from_millis(70))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::IncompleteTransmission {
            bus: "LIN",
            expected: 1,
            actual: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn lin_executes_event_sporadic_and_configuration_slots() {
    let ldf = Ldf::parse_str(LDF).unwrap();
    let origin = Instant::now();
    let mut scheduler = LinScheduler::from_ldf_at(&ldf, origin).unwrap();
    let mut device = MockLin::default();

    scheduler.set_node_enabled("Slave", true).unwrap();
    scheduler.start_schedule("Events").unwrap();
    scheduler.set_frame_enabled("StatusEvent", false).unwrap();
    let suppressed_event = scheduler.poll_at(&mut device, origin).await.unwrap();
    assert_eq!(suppressed_event[0].operation, LinOperation::Skipped);
    scheduler.clear_frame_override("StatusEvent").unwrap();
    scheduler.start_schedule("Events").unwrap();
    let event = scheduler.poll_at(&mut device, origin).await.unwrap();
    assert_eq!(event[0].operation, LinOperation::Sent);
    assert_eq!(device.sent[0].0, 2);
    assert_eq!(device.sent[0].1, [0xC1]);

    scheduler.start_schedule("Sporadic").unwrap();
    let empty = scheduler.poll_at(&mut device, origin).await.unwrap();
    assert_eq!(empty[0].operation, LinOperation::Skipped);
    scheduler.set_payload("Command", vec![9]).unwrap();
    let burst = scheduler
        .poll_at(&mut device, origin + Duration::from_millis(10))
        .await
        .unwrap();
    assert_eq!(burst[0].frame_name, "Command");
    assert_eq!(device.sent.last().unwrap(), &(3, vec![9]));

    scheduler.start_schedule("Configuration").unwrap();
    let assign_nad = scheduler
        .poll_at(&mut device, origin + Duration::from_millis(10))
        .await
        .unwrap();
    assert_eq!(
        assign_nad,
        [LinTransmission {
            frame_name: "AssignNAD(Slave)".to_string(),
            frame_id: Some(0x3c),
            schedule_name: Some("Configuration".to_string()),
            schedule_index: Some(0),
            operation: LinOperation::Sent,
            bytes: 8,
        }]
    );
    assert_eq!(
        device.sent.last().unwrap(),
        &(0x3c, vec![2, 6, 0xb0, 1, 0, 2, 0, 1])
    );
    scheduler
        .poll_at(&mut device, origin + Duration::from_millis(20))
        .await
        .unwrap();
    assert_eq!(
        device.sent.last().unwrap(),
        &(0x3c, vec![1, 6, 0xb7, 0, 0xc1, 0xff, 0xff, 0xff])
    );
    scheduler
        .poll_at(&mut device, origin + Duration::from_millis(30))
        .await
        .unwrap();
    assert_eq!(
        device.sent.last().unwrap(),
        &(0x3c, vec![4, 6, 0xb1, 0x23, 0x01, 0x34, 0x12, 0xc1])
    );
}
