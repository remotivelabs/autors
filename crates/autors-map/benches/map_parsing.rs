use std::fmt::Write as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

use autors_map::map::MapFile;

const SYMBOL_COUNT: usize = 50_000;

fn source() -> String {
    let mut source = String::with_capacity(SYMBOL_COUNT * 40);
    for index in 0..SYMBOL_COUNT {
        writeln!(
            source,
            "0x{:08X} calibration_symbol_{index}_value",
            0x8000 + index
        )
        .expect("writing to a String cannot fail");
    }
    source
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
    let source = source();
    let (elapsed, count) = best_of(
        || {
            MapFile::open_str(black_box(&source), None)
                .expect("synthetic MAP should parse")
                .symbols
                .len()
        },
        5,
    );
    assert_eq!(count, SYMBOL_COUNT);

    let mib = source.len() as f64 / (1024.0 * 1024.0);
    println!("MAP parsing ({SYMBOL_COUNT} symbols, {mib:.2} MiB)");
    println!("  best time:  {elapsed:?}");
    println!("  throughput: {:.1} MiB/s", mib / elapsed.as_secs_f64());
}
