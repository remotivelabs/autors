use std::collections::BTreeMap;

use autors_diag::doip_capture::DoIpCapture;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: doip_pcapng <capture.pcapng>")?;
    let capture = DoIpCapture::open_pcapng(&path)?;

    println!("file: {}", std::path::Path::new(&path).display());
    println!("sections: {}", capture.statistics.sections);
    println!("interfaces: {}", capture.statistics.interfaces);
    println!("packet blocks: {}", capture.statistics.packet_blocks);
    println!("DoIP TCP segments: {}", capture.statistics.tcp_segments);
    println!("DoIP UDP datagrams: {}", capture.statistics.udp_datagrams);
    println!("decoded DoIP frames: {}", capture.statistics.doip_frames);

    let mut frame_types = BTreeMap::<String, usize>::new();
    for captured in &capture.frames {
        let base = captured.frame.base();
        let name = base
            .msg_type()
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| format!("0x{:04X}", base.msg_type));
        *frame_types.entry(name).or_default() += 1;
    }
    for (frame_type, count) in frame_types {
        println!("frame type {frame_type}: {count}");
    }

    let mut issue_types = BTreeMap::<String, usize>::new();
    for issue in &capture.issues {
        *issue_types.entry(format!("{:?}", issue.kind)).or_default() += 1;
    }
    println!("recoverable issues: {}", capture.issues.len());
    for (kind, count) in issue_types {
        println!("issue {kind}: {count}");
    }
    for issue in capture.issues.iter().take(10) {
        println!(
            "packet {} {:?}: {}",
            issue.packet_index, issue.kind, issue.message
        );
    }
    Ok(())
}
