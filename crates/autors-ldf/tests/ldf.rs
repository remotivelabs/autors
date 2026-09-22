use autors_ldf::codec::SignalValues;
use autors_ldf::model::{EncodingValue, Ldf, ScheduleCommand, SignalValue};

const SAMPLE: &str = r#"
// compact integration fixture
LIN_description_file;
LIN_protocol_version = "2.2";
LIN_language_version = "2.2";
LIN_speed = 19.2 kbps;
Channel_name = "LIN1";

Nodes {
    Master: Master, 5 ms, 0.1 ms;
    Slaves: Slave;
}

Signals {
    Speed: 8, 0, Slave, Master;
    Error: 1, 0, Slave, Master;
    CommandValue: 8, 7, Master, Slave;
}

Diagnostic_signals {
    Req: 8, 0;
    Resp: 8, 0;
}

Frames {
    Status: 1, Slave, 2 {
        Speed, 0;
        Error, 8;
    }
    Command: 3, Master, 1 {
        CommandValue, 0;
    }
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
        product_id = 1, 2, 3;
        response_error = Error;
        fault_state_signals = Error;
        configurable_frames {
            Status;
            Command;
            StatusEvent;
        }
    }
}

Schedule_tables {
    Main {
        Command delay 10 ms;
        StatusEvent delay 20 ms;
        MasterReq delay 10 ms;
        SlaveResp delay 10 ms;
    }
    Collision {
        Status delay 10 ms;
    }
    Configuration {
        AssignNAD { Slave } delay 10 ms;
        ConditionalChangeNAD { 127, 1, 3, 1, 255, 1 } delay 10 ms;
        DataDump { Slave, 1, 2, 3, 4, 5 } delay 10 ms;
        SaveConfiguration { Slave } delay 10 ms;
        AssignFrameIdRange { Slave, 0, 64, 66, 255, 255 } delay 10 ms;
        AssignFrameId { Slave, Status } delay 10 ms;
        UnassignFrameId { Slave, Status } delay 10 ms;
        FreeFormat { 60, 178, 0, 0, 255, 127, 255, 255 } delay 10 ms;
    }
}

Signal_groups {
    StatusGroup: 9 {
        Speed, 0;
        Error, 8;
    }
}

Signal_encoding_types {
    SpeedType {
        physical_value, 0, 254, 2, 0, "rpm";
        logical_value, 255, "invalid";
    }
    ErrorType {
        logical_value, 0, "ok";
        logical_value, 1, "fault";
    }
}

Signal_representation {
    SpeedType: Speed;
    ErrorType: Error;
}
"#;

#[test]
fn parses_model_and_round_trips_without_loss() {
    let first = Ldf::parse_str(SAMPLE).unwrap();
    assert_eq!(first.baud_rate, 19_200);
    assert_eq!(first.channel_name.as_deref(), Some("LIN1"));
    assert_eq!(first.master.name, "Master");
    assert_eq!(first.slaves["Slave"].configured_nad, Some(1));
    assert_eq!(first.unconditional_frames["Status"].length, 2);
    assert_eq!(first.signal_groups["StatusGroup"].size, 9);
    assert!(matches!(
        first.schedule_tables["Configuration"].entries[4].command,
        ScheduleCommand::AssignFrameIdRange {
            protected_ids: Some([64, 66, 255, 255]),
            ..
        }
    ));
    assert!(matches!(
        first.signal_encoding_types["SpeedType"].values[0],
        EncodingValue::Physical { .. }
    ));
    assert_eq!(first.comments, ["// compact integration fixture"]);

    let serialized = first.write_string().unwrap();
    let second = Ldf::parse_str(&serialized).unwrap();
    assert_eq!(first, second);
}

#[test]
fn parsed_encodings_drive_frame_codec() {
    let ldf = Ldf::parse_str(SAMPLE).unwrap();
    let mut values = SignalValues::new();
    values.insert("Speed".to_string(), SignalValue::Text("100rpm".to_string()));
    values.insert("Error".to_string(), SignalValue::Text("fault".to_string()));
    let payload = ldf.encode_frame("Status", &values).unwrap();
    assert_eq!(payload, [50, 1]);

    let decoded = ldf.decode_frame("Status", &payload, true).unwrap();
    assert_eq!(
        decoded["Speed"],
        SignalValue::Text("100.000 rpm".to_string())
    );
    assert_eq!(decoded["Error"], SignalValue::Text("fault".to_string()));
}

#[test]
fn rejects_cross_reference_and_layout_errors() {
    let missing_node = SAMPLE.replace(
        "Speed: 8, 0, Slave, Master;",
        "Speed: 8, 0, Missing, Master;",
    );
    assert!(Ldf::parse_str(&missing_node).is_err());

    let overlap = SAMPLE.replace("Error, 8;", "Error, 7;");
    assert!(Ldf::parse_str(&overlap).is_err());
}

/// A generating tool writes `<...>` where a node attribute has no value.
const PLACEHOLDER: &str = r#"
LIN_description_file;
LIN_protocol_version = "2.2";
LIN_language_version = "2.2";
LIN_speed = 19.2 kbps;

Nodes {
    Master: Master, 5 ms, 0.1 ms;
    Slaves: Slave;
}

Signals {
    Status: 8, 0, Slave, Master;
}

Frames {
    Report: 0x01, Slave, 1 { Status, 0; }
}

Node_attributes {
    Slave {
        LIN_protocol = "2.2";
        configured_NAD = 0x03;
        initial_NAD = 0x03;
        product_id = 0xB0, 0xB002, 0;
        response_error = <invalid>;
    }
}
"#;

#[test]
fn a_placeholder_means_the_attribute_has_no_value() {
    let ldf = Ldf::parse_str(PLACEHOLDER).unwrap();

    assert_eq!(ldf.slaves["Slave"].response_error, None);
}

/// A slave publishes a response-error signal in one of its frames, and a file names a bit it
/// already has, so one bit range carries two names.
const ALIAS: &str = r#"
LIN_description_file;
LIN_protocol_version = "2.2";
LIN_language_version = "2.2";
LIN_speed = 19.2 kbps;

Nodes {
    Master: Master, 5 ms, 0.1 ms;
    Slaves: Slave;
}

Signals {
    DiagErrResp: 1, 0, Slave, Master;
    ErrRespSlave: 1, 0, Slave, Master;
}

Frames {
    Report: 0x01, Slave, 1 {
        DiagErrResp, 7;
        ErrRespSlave, 7;
    }
}

Node_attributes {
    Slave {
        LIN_protocol = "2.2";
        configured_NAD = 0x03;
        initial_NAD = 0x03;
        product_id = 0xB0, 0xB002, 0;
        response_error = ErrRespSlave;
    }
}
"#;

/// The specification says signals in a frame do not overlap, and an alias is an overlap, so the
/// strict path refuses it. The lenient path reads what the file says, for a reader that has to
/// take what production tool chains write.
#[test]
fn an_alias_is_refused_strictly_and_read_leniently() {
    let error = Ldf::parse_str(ALIAS).unwrap_err();
    assert!(error.to_string().contains("overlaps"), "{error}");

    let ldf = Ldf::parse_str_unvalidated(ALIAS).unwrap();
    assert_eq!(ldf.unconditional_frames["Report"].signals.len(), 2);
    assert!(ldf.validate().is_err());
}

#[test]
fn a_partial_overlap_is_refused() {
    let collides = ALIAS
        .replace("DiagErrResp: 1, 0", "DiagErrResp: 4, 0")
        .replace("ErrRespSlave: 1, 0", "ErrRespSlave: 4, 0")
        .replace("DiagErrResp, 7;", "DiagErrResp, 0;")
        .replace("ErrRespSlave, 7;", "ErrRespSlave, 2;");

    let error = Ldf::parse_str(&collides).unwrap_err();

    assert!(error.to_string().contains("overlaps"), "{error}");
}

/// Two identical modules on one bus reuse a frame identifier and are told apart by schedule.
const SHARED_ID: &str = r#"
LIN_description_file;
LIN_protocol_version = "2.2";
LIN_language_version = "2.2";
LIN_speed = 19.2 kbps;

Nodes {
    Master: Master, 5 ms, 0.1 ms;
    Slaves: Module;
}

Signals {
    LeftStatus: 8, 0, Module, Master;
    RightStatus: 8, 0, Module, Master;
}

Frames {
    Left: 0x02, Module, 1 { LeftStatus, 0; }
    Right: 0x02, Module, 1 { RightStatus, 0; }
}

Schedule_tables {
    LeftTable { Left delay 15 ms; }
    RightTable { Right delay 15 ms; }
}

Node_attributes {
    Module {
        LIN_protocol = "2.2";
        configured_NAD = 0x04;
        initial_NAD = 0x04;
        product_id = 0xB0, 0xB003, 0;
    }
}
"#;

/// The specification gives every frame its own identifier, so the strict path refuses two frames
/// that share one, whatever the schedule tables do. The lenient path reads both.
#[test]
fn frames_sharing_an_identifier_are_refused_strictly_and_read_leniently() {
    let error = Ldf::parse_str(SHARED_ID).unwrap_err();
    assert!(error.to_string().contains("share ID"), "{error}");

    let ldf = Ldf::parse_str_unvalidated(SHARED_ID).unwrap();
    assert_eq!(ldf.unconditional_frames.len(), 2);
    assert!(ldf.validate().is_err());
}

/// A document that does not hold together is still worth having, for a program that reports why.
#[test]
fn a_document_parses_even_when_it_does_not_hold_together() {
    let broken = ALIAS.replace("DiagErrResp, 7;", "Missing, 7;");

    assert!(Ldf::parse_str(&broken).is_err());

    let ldf = Ldf::parse_str_unvalidated(&broken).unwrap();

    assert_eq!(ldf.unconditional_frames["Report"].signals.len(), 2);
    assert!(
        ldf.validate().is_err(),
        "the fault is still there to report"
    );
}

/// A placeholder node, standing for whichever ECU, has no attributes. The specification wants
/// an entry for every slave from LIN 2.0 on, so the strict path says so; the lenient path reads
/// the frames and leaves the attributes at their defaults.
const NODE_WITHOUT_ATTRIBUTES: &str = r#"
LIN_description_file;
LIN_protocol_version = "2.1";
LIN_language_version = "2.1";
LIN_speed = 19.2 kbps;

Nodes {
    Master: DEVM, 5 ms, 0.1 ms;
    Slaves: DEVS1, ANY;
}

Signals {
    CRC: 8, 0, ANY;
}

Frames {
    Protected: 0x20, ANY, 4 {
        CRC, 0;
    }
}

Node_attributes {
    DEVS1 {
        LIN_protocol = "2.1";
        configured_NAD = 0x01;
        product_id = 0x0001, 0x0001, 0;
    }
}
"#;

#[test]
fn a_slave_without_attributes_is_refused_strictly_and_read_leniently() {
    let error = Ldf::parse_str(NODE_WITHOUT_ATTRIBUTES).unwrap_err();
    assert!(error.to_string().contains("has no LIN_protocol"), "{error}");

    let ldf = Ldf::parse_str_unvalidated(NODE_WITHOUT_ATTRIBUTES).unwrap();
    assert!(ldf.slaves.contains_key("ANY"));
    assert_eq!(ldf.unconditional_frames["Protected"].signals.len(), 1);
}
