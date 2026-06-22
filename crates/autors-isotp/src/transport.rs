use std::time::{Duration, Instant};

use autors_can::device::CanDevice;
use autors_can::frame::CanFrame;

use crate::error::Result;
use crate::isotp::{
    FrameType, IsoTpFsm, IsoTpType, MsgState, MAX_MSG_LEN_CAN, MAX_MSG_LEN_CANFD, NO_RESPONSE_MASK,
};

/// [`IsoTp::on_received`].
pub struct IsoTp {
    pub cmd_id: u32,
    pub rsp_id: u32,
    pub ta: u16,
    pub sa: u16,
    pub p2_client: u32,
    pub p3_client: u32,
    pub use_fill_byte: bool,
    pub fill_byte: u8,
    tp_type: IsoTpType,
    max_msg_len: u32,
    fsm: Option<IsoTpFsm>,
}

impl IsoTp {
    /// uint p3Client = 50, bool useFillByte = true, byte fillByte = 0xFF)`
    pub fn new(cmd_id: u32, rsp_id: u32, use_can_fd: bool) -> Self {
        Self {
            cmd_id,
            rsp_id,
            ta: 0,
            sa: 0,
            p2_client: 50,
            p3_client: 50,
            use_fill_byte: true,
            fill_byte: 0xFF,
            tp_type: if use_can_fd {
                IsoTpType::CanFd
            } else {
                IsoTpType::Can
            },
            max_msg_len: if use_can_fd {
                MAX_MSG_LEN_CANFD
            } else {
                MAX_MSG_LEN_CAN
            },
            fsm: None,
        }
    }

    pub fn tp_type(&self) -> IsoTpType {
        self.tp_type
    }

    pub fn max_msg_len(&self) -> u32 {
        self.max_msg_len
    }

    fn fsm_state(&self) -> MsgState {
        self.fsm.as_ref().map_or(MsgState::Success, IsoTpFsm::state)
    }

    pub fn on_received(&mut self, frame: &CanFrame) -> Result<bool> {
        let Some(fsm) = &mut self.fsm else {
            return Ok(false);
        };
        if frame.id != self.rsp_id {
            return Ok(false);
        }
        let state = fsm.state();
        if matches!(state, MsgState::PendingRcvFC | MsgState::PendingRcv) {
            fsm.on_received_can(frame)?;
        }
        Ok(true)
    }

    async fn wait_state_change<D: CanDevice + Send>(
        &mut self,
        device: &mut D,
        timeout: Duration,
    ) -> Result<bool> {
        let entry_state = self.fsm_state();
        let deadline = Instant::now() + timeout;
        loop {
            match device.receive().await {
                Ok(Some(frame)) => {
                    let _ = self.on_received(&frame);
                    if self.fsm_state() != entry_state {
                        return Ok(true);
                    }
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                    autors_runtime::sleep(Duration::from_millis(1)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    pub async fn send_request<D: CanDevice + Send>(
        &mut self,
        device: &mut D,
        mut req_data: Vec<u8>,
        res_data: Option<&mut Vec<u8>>,
        await_response: bool,
    ) -> Result<MsgState> {
        if req_data.len() as u64 > u64::from(self.max_msg_len) {
            return Ok(MsgState::ErrRequestLenExceeded);
        }
        if !await_response && req_data.len() > 1 {
            req_data[1] |= NO_RESPONSE_MASK;
        }
        let is_fd = self.tp_type == IsoTpType::CanFd;
        let req_len = req_data.len();
        self.fsm = Some(IsoTpFsm::new(
            req_data,
            await_response,
            if is_fd { 64 } else { 8 },
            0,
            0,
        ));
        while self.fsm_state() < MsgState::Success {
            let Some(fsm) = &mut self.fsm else { break };
            let (_, frame) = fsm.next_frame();
            let mut wait_ms = 0u32;
            if let Some(frame) = frame {
                let max_dlc = if self.use_fill_byte { 8 } else { 0 };
                let sent = device
                    .send_msg(self.cmd_id, &frame, !is_fd, max_dlc, self.fill_byte)
                    .await?;
                if sent != frame.len() {
                    return Ok(MsgState::ErrSendRequest);
                }
            }
            match self.fsm_state() {
                MsgState::Success => {
                    if !await_response {
                        wait_ms = self.p2_client;
                    }
                }
                MsgState::PendingRcvFC | MsgState::PendingRcv => {
                    let send_offset = self.fsm.as_ref().map_or(0, IsoTpFsm::send_offset);
                    if res_data.is_none() && send_offset == req_len {
                        return Ok(MsgState::Success);
                    }
                    wait_ms = self.p2_client;
                }
                MsgState::PendingSnd => {
                    let st_min = self.fsm.as_ref().map_or(0, |fsm| {
                        if fsm.send_offset() != 0 && fsm.frame_type() == FrameType::CF {
                            fsm.request_st_min
                        } else {
                            0
                        }
                    });
                    if st_min > 0 {
                        autors_runtime::sleep(Duration::from_micros(st_min as u64)).await;
                    }
                }
                _ => {}
            }
            if wait_ms > 0 {
                let changed = self
                    .wait_state_change(device, Duration::from_millis(u64::from(wait_ms)))
                    .await?;
                if !changed && await_response {
                    return Ok(MsgState::ErrTimeout);
                }
            }
        }
        let result = self.fsm_state();
        if result == MsgState::Success {
            if let (Some(out), Some(fsm)) = (res_data, &self.fsm) {
                out.extend_from_slice(fsm.received());
            }
        }
        self.fsm = None;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use autors_can::device::DeviceCore;
    use autors_can::frame::{CanConfiguration, FrameType as CanFrameType};
    use std::collections::VecDeque;

    #[cfg(feature = "blocking")]
    use crate::blocking::BlockingIsoTp;

    const CMD_ID: u32 = 0x7E0;
    const RSP_ID: u32 = 0x7E8;

    type FrameHandler = Box<dyn FnMut(&[u8]) -> Vec<Vec<u8>> + Send>;
    type Responder = Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>;

    struct EcuStub {
        core: DeviceCore,
        rsp_id: u32,
        sent: Vec<Vec<u8>>,
        sent_types: Vec<CanFrameType>,
        enqueued: Vec<Vec<u8>>,
        rx: VecDeque<CanFrame>,
        handler: FrameHandler,
    }

    impl EcuStub {
        fn new(handler: impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send + 'static) -> Self {
            Self {
                core: DeviceCore::new(),
                rsp_id: RSP_ID,
                sent: Vec::new(),
                sent_types: Vec::new(),
                enqueued: Vec::new(),
                rx: VecDeque::new(),
                handler: Box::new(handler),
            }
        }
    }

    #[async_trait]
    impl CanDevice for EcuStub {
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
            _can_id: u32,
            data: &[u8],
            frame_type: CanFrameType,
        ) -> autors_can::Result<usize> {
            self.sent.push(data.to_vec());
            self.sent_types.push(frame_type);
            for resp in (self.handler)(data) {
                self.enqueued.push(resp.clone());
                self.rx.push_back(CanFrame::new(
                    "Stub/CAN1",
                    self.rsp_id,
                    resp,
                    false,
                    frame_type,
                ));
            }
            Ok(data.len())
        }
        async fn receive(&mut self) -> autors_can::Result<Option<CanFrame>> {
            Ok(self.rx.pop_front())
        }
    }

    struct ComplianceEcu {
        bs: u8,
        st_min: u8,
        fsm: IsoTpFsm,
        responder: Responder,
    }

    impl ComplianceEcu {
        fn new(
            bs: u8,
            st_min: u8,
            responder: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static,
        ) -> Self {
            Self {
                bs,
                st_min,
                fsm: Self::receiver(bs, st_min),
                responder: Box::new(responder),
            }
        }

        fn receiver(bs: u8, st_min: u8) -> IsoTpFsm {
            IsoTpFsm::new(Vec::new(), true, 8, bs, st_min)
        }

        fn on_frame(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
            let mut out = Vec::new();
            if self.fsm.on_received(data).is_err() {
                return out;
            }
            self.pump(&mut out);
            if self.fsm.state() == MsgState::Success {
                let req = self.fsm.take_received();
                let resp = (self.responder)(&req);
                self.fsm = if resp.is_empty() {
                    Self::receiver(self.bs, self.st_min)
                } else {
                    IsoTpFsm::new(resp, false, 8, 0, 0)
                };
                self.pump(&mut out);
            }
            out
        }

        fn pump(&mut self, out: &mut Vec<Vec<u8>>) {
            loop {
                let (state, frame) = self.fsm.next_frame();
                match frame {
                    Some(f) => out.push(f),
                    None => break,
                }
                if state >= MsgState::Success {
                    break;
                }
            }
        }
    }

    fn stub_with_ecu(ecu: ComplianceEcu) -> EcuStub {
        let mut ecu = Some(ecu);
        EcuStub::new(move |data| {
            let e = ecu.as_mut().expect("ecu");
            e.on_frame(data)
        })
    }

    fn did_responder(req: &[u8]) -> Vec<u8> {
        assert_eq!(req, &[0x22, 0xF1, 0x90]);
        vec![0x62, 0xF1, 0x90, 0xAA, 0xBB]
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn single_frame_roundtrip() {
        let stub = stub_with_ecu(ComplianceEcu::new(0, 0, did_responder));
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        let mut device = stub;
        let mut res = Vec::new();
        let state = iso
            .send_request(&mut device, vec![0x22, 0xF1, 0x90], Some(&mut res), true)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, vec![0x62, 0xF1, 0x90, 0xAA, 0xBB]);
        assert_eq!(
            device.sent[0],
            vec![0x03, 0x22, 0xF1, 0x90, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(device.sent_types[0], CanFrameType::CAN20B);
        assert_eq!(
            device.enqueued,
            vec![vec![0x05, 0x62, 0xF1, 0x90, 0xAA, 0xBB]]
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn single_frame_no_response_sets_suppress_bit() {
        let stub = EcuStub::new(|_| vec![]);
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        iso.0.use_fill_byte = false;
        iso.0.p2_client = 5;
        let mut device = stub;
        let state = iso
            .send_request(&mut device, vec![0x3E, 0x00], None, false)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(device.sent[0], vec![0x02, 0x3E, 0x80]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn request_len_exceeded() {
        let stub = EcuStub::new(|_| vec![]);
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        let mut device = stub;
        let state = iso
            .send_request(&mut device, vec![0u8; 4096], None, true)
            .unwrap();
        assert_eq!(state, MsgState::ErrRequestLenExceeded);
        assert!(device.sent.is_empty());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn multi_frame_request_with_block_fc() {
        let stub = stub_with_ecu(ComplianceEcu::new(2, 0, |req| {
            assert_eq!(req.len(), 30);
            assert_eq!(req[0], 0x22);
            vec![0x62, 0x01]
        }));
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        let mut device = stub;
        let mut req = vec![0x22];
        req.extend(1..=29u8);
        let mut res = Vec::new();
        let state = iso
            .send_request(&mut device, req, Some(&mut res), true)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, vec![0x62, 0x01]);
        assert_eq!(device.sent.len(), 5);
        assert_eq!(&device.sent[0][..2], &[0x10, 0x1E]);
        for (i, sn) in (1..=4u8).enumerate() {
            assert_eq!(device.sent[i + 1][0], 0x20 | sn);
        }
        let fcs: Vec<_> = device
            .enqueued
            .iter()
            .filter(|f| f[0] & 0xF0 == 0x30)
            .collect();
        assert_eq!(fcs.len(), 2);
        assert_eq!(fcs[0][..], [0x30, 0x02, 0x00][..]);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn multi_frame_st_min_pacing() {
        let stub = stub_with_ecu(ComplianceEcu::new(0, 20, |req| {
            assert_eq!(req.len(), 20);
            vec![0x62, 0x01]
        }));
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        let mut device = stub;
        let req: Vec<u8> = (1..=20u8).collect();
        let mut res = Vec::new();
        let start = Instant::now();
        let state = iso
            .send_request(&mut device, req, Some(&mut res), true)
            .unwrap();
        let elapsed = start.elapsed();
        assert_eq!(state, MsgState::Success);
        assert_eq!(device.sent.len(), 3);
        assert_eq!(device.enqueued[0], vec![0x30, 0x00, 20]);
        assert!(
            elapsed >= Duration::from_millis(15),
            "elapsed = {elapsed:?}"
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn multi_frame_response_reassembled() {
        let stub = stub_with_ecu(ComplianceEcu::new(0, 0, |req| {
            assert_eq!(req, &[0x22, 0xF1, 0x90]);
            (1..=20u8).collect()
        }));
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        let mut device = stub;
        let mut res = Vec::new();
        let state = iso
            .send_request(&mut device, vec![0x22, 0xF1, 0x90], Some(&mut res), true)
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, (1..=20u8).collect::<Vec<_>>());
        assert_eq!(device.sent.len(), 2);
        assert_eq!(&device.sent[1][..3], &[0x30, 0x00, 0x00]);
        assert_eq!(device.enqueued.len(), 3);
        assert_eq!(&device.enqueued[0][..2], &[0x10, 0x14]);
        assert_eq!(device.enqueued[1][0], 0x21);
        assert_eq!(device.enqueued[2][0], 0x22);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn fc_wait_timeout() {
        let stub = EcuStub::new(|_| vec![]);
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        iso.0.p2_client = 20;
        let mut device = stub;
        let req: Vec<u8> = (1..=20u8).collect();
        let start = Instant::now();
        let state = iso.send_request(&mut device, req, None, true).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(state, MsgState::ErrTimeout);
        assert_eq!(device.sent.len(), 1);
        assert!(
            elapsed >= Duration::from_millis(15),
            "elapsed = {elapsed:?}"
        );
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn unexpected_cf_sequence_aborts() {
        let stub = EcuStub::new(|data| match data[0] & 0xF0 {
            0x00 => vec![vec![0x10, 0x0A, 1, 2, 3, 4, 5, 6]],
            0x30 => vec![vec![0x23, 7, 8, 9, 10]],
            _ => vec![],
        });
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        let mut device = stub;
        let mut res = Vec::new();
        let state = iso
            .send_request(&mut device, vec![0x22, 0xF1, 0x90], Some(&mut res), true)
            .unwrap();
        assert_eq!(state, MsgState::ErrUnexpectedSequenceNo);
        assert!(res.is_empty());
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn fc_overflow_aborts() {
        let stub = EcuStub::new(|data| match data[0] & 0xF0 {
            0x10 => vec![vec![0x32, 0x00, 0x00]],
            _ => vec![],
        });
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        let mut device = stub;
        let req: Vec<u8> = (1..=20u8).collect();
        let state = iso.send_request(&mut device, req, None, true).unwrap();
        assert_eq!(state, MsgState::ErrFlowControlOverflow);
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn on_received_can_overload_and_id_filter() {
        let mut iso = BlockingIsoTp::new(IsoTp::new(CMD_ID, RSP_ID, false));
        let other = CanFrame::new(
            "B",
            0x123,
            vec![0x03, 0x22, 0xF1, 0x90],
            false,
            CanFrameType::CAN20B,
        );
        assert!(!iso.on_received(&other).unwrap());
        let mut device = EcuStub::new(|_| vec![]);
        iso.0.p2_client = 5;
        let mut res = Vec::new();
        let state = iso
            .send_request(&mut device, vec![0x22, 0xF1, 0x90], Some(&mut res), true)
            .unwrap();
        assert_eq!(state, MsgState::ErrTimeout);

        let mut fsm = IsoTpFsm::new(vec![0x22], true, 8, 0, 0);
        let frame = CanFrame::new(
            "B",
            RSP_ID,
            vec![0x03, 0x22, 0xF1, 0x90],
            false,
            CanFrameType::CAN20B,
        );
        let state = fsm.on_received_can(&frame).unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(fsm.received(), &[0x22, 0xF1, 0x90]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_single_frame_roundtrip() {
        let stub = stub_with_ecu(ComplianceEcu::new(0, 0, did_responder));
        let mut iso = IsoTp::new(CMD_ID, RSP_ID, false);
        let mut device = stub;
        let mut res = Vec::new();
        let state = iso
            .send_request(&mut device, vec![0x22, 0xF1, 0x90], Some(&mut res), true)
            .await
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, vec![0x62, 0xF1, 0x90, 0xAA, 0xBB]);
        assert_eq!(
            device.sent[0],
            vec![0x03, 0x22, 0xF1, 0x90, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(device.sent_types[0], CanFrameType::CAN20B);
        assert_eq!(
            device.enqueued,
            vec![vec![0x05, 0x62, 0xF1, 0x90, 0xAA, 0xBB]]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_multi_frame_request_with_block_fc() {
        let stub = stub_with_ecu(ComplianceEcu::new(2, 1, |req| {
            assert_eq!(req.len(), 30);
            assert_eq!(req[0], 0x22);
            vec![0x62, 0x01]
        }));
        let mut iso = IsoTp::new(CMD_ID, RSP_ID, false);
        let mut device = stub;
        let mut req = vec![0x22];
        req.extend(1..=29u8);
        let mut res = Vec::new();
        let state = iso
            .send_request(&mut device, req, Some(&mut res), true)
            .await
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, vec![0x62, 0x01]);
        assert_eq!(device.sent.len(), 5);
        assert_eq!(&device.sent[0][..2], &[0x10, 0x1E]);
        for (i, sn) in (1..=4u8).enumerate() {
            assert_eq!(device.sent[i + 1][0], 0x20 | sn);
        }
        let fcs: Vec<_> = device
            .enqueued
            .iter()
            .filter(|f| f[0] & 0xF0 == 0x30)
            .collect();
        assert_eq!(fcs.len(), 2);
        assert_eq!(fcs[0][..], [0x30, 0x02, 0x01][..]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_multi_frame_response_reassembled() {
        let stub = stub_with_ecu(ComplianceEcu::new(0, 0, |req| {
            assert_eq!(req, &[0x22, 0xF1, 0x90]);
            (1..=20u8).collect()
        }));
        let mut iso = IsoTp::new(CMD_ID, RSP_ID, false);
        let mut device = stub;
        let mut res = Vec::new();
        let state = iso
            .send_request(&mut device, vec![0x22, 0xF1, 0x90], Some(&mut res), true)
            .await
            .unwrap();
        assert_eq!(state, MsgState::Success);
        assert_eq!(res, (1..=20u8).collect::<Vec<_>>());
        assert_eq!(device.sent.len(), 2);
        assert_eq!(&device.sent[1][..3], &[0x30, 0x00, 0x00]);
        assert_eq!(device.enqueued.len(), 3);
        assert_eq!(&device.enqueued[0][..2], &[0x10, 0x14]);
        assert_eq!(device.enqueued[1][0], 0x21);
        assert_eq!(device.enqueued[2][0], 0x22);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_fc_wait_timeout() {
        let stub = EcuStub::new(|_| vec![]);
        let mut iso = IsoTp::new(CMD_ID, RSP_ID, false);
        iso.p2_client = 20;
        let mut device = stub;
        let req: Vec<u8> = (1..=20u8).collect();
        let start = Instant::now();
        let state = iso
            .send_request(&mut device, req, None, true)
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert_eq!(state, MsgState::ErrTimeout);
        assert_eq!(device.sent.len(), 1);
        assert!(
            elapsed >= Duration::from_millis(15),
            "elapsed = {elapsed:?}"
        );
    }
}
