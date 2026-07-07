//! Minimal typed UDS client wiring for an already opened LIN device.

use std::time::Duration;

use autors_diag::uds::UdsClient;
use autors_diag::uds_transport::LinTpTransport;
use autors_lin::device::LinDevice;

#[allow(dead_code)]
async fn read_vin<D: LinDevice + Send>(device: D, nad: u8) -> autors_diag::Result<Vec<u8>> {
    let mut transport = LinTpTransport::new(device, nad);

    // These values can be copied directly from the target node attributes in
    // an autors-ldf model.
    transport.lintp.set_node_timing(
        Duration::from_millis(50),
        Duration::ZERO,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );

    let mut client = UdsClient::new(transport);
    let (_, response) = client.read_data_by_identifier(&[0xf190]).await?;
    Ok(response.map_or_else(Vec::new, |message| message.data))
}

fn main() {
    // Hardware discovery/opening is vendor-specific. Pass an opened
    // `KvaserLin`, `PeakLin`, `VectorLin`, or custom `LinDevice`, configured
    // with DLC 8 and classic diagnostic checksums, to `read_vin` from the
    // application's async runtime.
}
