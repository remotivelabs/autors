use std::env;
use std::path::Path;

use autors_datafile::{DataFile, EditSession, MemorySegmentList, ProcessingOptions, XorProcessor};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filename = env::args().nth(1).ok_or("usage: edit <data-file>")?;
    let (mut file, _) =
        DataFile::open_auto(Path::new(&filename), MemorySegmentList::new(), 0, None)?;
    let mut session = EditSession::from_base(file.base())?;

    let outcome = session.apply("invert initialized bytes", |image| {
        image.process_with(&XorProcessor::invert(), ProcessingOptions::default())
    })?;
    println!(
        "processed {} byte(s); changed={}",
        outcome.value.input_bytes, outcome.changed
    );

    if let Some(event) = session.undo() {
        println!("undid: {}", event.label);
    }
    if let Some(event) = session.redo() {
        println!("redid: {}", event.label);
    }
    println!("committed: {}", session.commit_to(file.base_mut()));
    Ok(())
}
