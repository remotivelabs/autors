use std::hint::black_box;
use std::time::{Duration, Instant};

use autors_a2l::model::base::ByteOrder;
use autors_a2l::model::enums::DataType;
use autors_comm::base::{DaqDict, DaqList, DaqMeasurement, MeasurementInfo};

const MEASUREMENT_COUNT: usize = 10_000;

fn measurements() -> Vec<DaqMeasurement> {
    (0..MEASUREMENT_COUNT)
        .map(|index| {
            DaqMeasurement::new(
                MeasurementInfo {
                    name: format!("measurement_{index}"),
                    address: 0x1000 + index as u32,
                    data_type: DataType::UByte,
                    byte_order: ByteOrder::MSB_LAST,
                    ..MeasurementInfo::default()
                },
                0,
                None,
            )
        })
        .collect()
}

fn dict() -> DaqDict {
    let mut dict = DaqDict::default();
    dict.lists.push(DaqList::new(
        0,
        0,
        1,
        u16::MAX,
        u8::MAX,
        u8::MAX,
        0,
        0,
        String::new(),
        0,
    ));
    dict
}

fn best_of(mut operation: impl FnMut() -> usize, samples: usize) -> (Duration, usize) {
    let mut best = Duration::MAX;
    let mut count = 0;
    for _ in 0..samples {
        let start = Instant::now();
        count = black_box(operation());
        best = best.min(start.elapsed());
    }
    (best, count)
}

fn main() {
    let template = measurements();
    let (elapsed, remaining) = best_of(
        || {
            let mut measurements = template.clone();
            dict()
                .fill_daq_lists(&mut measurements, true)
                .expect("DAQ configuration should succeed")
        },
        3,
    );
    assert_eq!(remaining, 0);

    println!("DAQ configuration ({MEASUREMENT_COUNT} measurements)");
    println!("  best time: {elapsed:?}");
}
